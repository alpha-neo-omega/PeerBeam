package com.peerbeam.peerbeam

import android.Manifest
import android.content.ContentUris
import android.content.ContentValues
import android.content.Context
import android.content.ActivityNotFoundException
import android.content.Intent
import android.content.pm.PackageManager
import android.net.Uri
import android.net.wifi.WifiManager
import android.os.BatteryManager
import android.os.Build
import android.os.Handler
import android.os.Looper
import android.os.PowerManager
import android.provider.MediaStore
import android.provider.OpenableColumns
import android.provider.Settings
import android.webkit.MimeTypeMap
import androidx.core.app.ActivityCompat
import androidx.core.content.ContextCompat
import androidx.documentfile.provider.DocumentFile
import io.flutter.embedding.android.FlutterActivity
import io.flutter.embedding.engine.FlutterEngine
import io.flutter.plugin.common.EventChannel
import io.flutter.plugin.common.MethodCall
import io.flutter.plugin.common.MethodChannel
import java.io.File
import java.util.concurrent.ExecutorService
import java.util.concurrent.Executors

/// The `peerbeam/android/events` sink, reachable from outside the activity.
///
/// A `Service` holds no reference to the `Activity`, and the sink is an activity
/// field — so `PeerBeamService` had no way to tell Dart anything, which is why
/// the six-hour foreground-service cap was invisible to the app that had to
/// react to it.
///
/// Posted to the main looper because an `EventSink` must be used from the main
/// thread, and `Service.onTimeout` makes no such promise. `@Volatile` because the
/// writer is the activity and the reader can be a service callback on another
/// thread.
internal object PlatformEvents {
    @Volatile
    var sink: EventChannel.EventSink? = null

    fun emit(event: Map<String, Any?>) {
        Handler(Looper.getMainLooper()).post { sink?.success(event) }
    }
}

class MainActivity : FlutterActivity() {
    private val methodName = "peerbeam/android"
    private val eventName = "peerbeam/android/events"

    private var events: EventChannel.EventSink? = null
    private var multicastLock: WifiManager.MulticastLock? = null

    /// The share/view intent that launched us, on its way to Dart via
    /// `initialIntent`. Main-thread-only state, like everything else this
    /// class hands to the channels.
    private val launch = PendingLaunch()

    // Storage Access Framework: the user picks a destination folder once; we
    // persist the grant and copy received files into it (the Rust engine writes
    // via std::fs to app storage, which the OS hides — SAF makes files visible).
    private val reqPickTree = 4210
    private var pendingPick: MethodChannel.Result? = null

    // Publishing a received file copies the whole thing into SAF/MediaStore,
    // which is far too slow for the platform thread: a large file — or a
    // received folder, which is a copy per file in it — blocked it long enough
    // for Android to raise an ANR. Publishes run here instead.
    //
    // One serial worker rather than a thread per call, because the platform
    // thread used to serialize them for us: [uniqueName]'s check-then-create
    // only keeps two same-named files apart when the second one runs after the
    // first has created its document.
    private val publisher: ExecutorService = Executors.newSingleThreadExecutor()

    // Native multi-file picker (ACTION_OPEN_DOCUMENT): streams each picked
    // file into app cache instead of going through file_selector_android,
    // whose openFile() reads the whole file into a Java byte[] (readFully)
    // before returning — that OOMs on large files under this app's 256MB
    // heap cap and kills the app mid-send. The actual streamed copy happens
    // on a background thread in onActivityResult.
    private val reqPickFiles = 4212
    private var pendingFiles: MethodChannel.Result? = null

    // The `keep` argument validated alongside `pendingFiles` in [pickFiles],
    // carried across to the [onActivityResult] callback the same way
    // `pendingFiles` itself is — there is no other way to get a value from
    // the call that launched the picker to the callback that resolves it.
    private var pendingKeep: List<String> = emptyList()

    // Runtime POST_NOTIFICATIONS request (Android 13+). Fire-and-forget: we
    // don't need the grant result, the OS just silently drops notifications
    // (see Notifications.show's SecurityException catch) if denied.
    private val reqPostNotifications = 4211
    private val safPrefs
        get() = getSharedPreferences("peerbeam_saf", Context.MODE_PRIVATE)

    override fun configureFlutterEngine(flutterEngine: FlutterEngine) {
        super.configureFlutterEngine(flutterEngine)
        val messenger = flutterEngine.dartExecutor.binaryMessenger

        MethodChannel(messenger, methodName).setMethodCallHandler { call, result ->
            onMethod(call.method, call, result)
        }

        EventChannel(messenger, eventName).setStreamHandler(
            object : EventChannel.StreamHandler {
                override fun onListen(arguments: Any?, sink: EventChannel.EventSink?) {
                    events = sink
                    PlatformEvents.sink = sink
                }

                override fun onCancel(arguments: Any?) {
                    events = null
                    PlatformEvents.sink = null
                }
            },
        )

        // The intent that launched us (cold-start share/view), delivered to
        // Dart on demand via `initialIntent`. A shared `content://` payload has
        // to be copied out of its provider first, and that copy runs on a
        // worker thread: this method runs before the first Flutter frame, so
        // copying a large share here held up the launch until Android offered
        // to kill the app — and taking that offer lost the share entirely.
        //
        // Marked as resolving *before* the resolve starts, because a request
        // that arrives mid-copy has to park rather than be told there is no
        // share; [resolveIntent] may also deliver inline, which clears the mark
        // again in the same breath.
        launch.resolving()
        resolveIntent(parseIntent(intent)) { launch.deliver(it) }
    }

    override fun onDestroy() {
        super.onDestroy()
        // Queued publishes still run to completion; the worker thread just
        // stops taking new ones and dies once they are done, rather than
        // outliving the activity that created it.
        publisher.shutdown()
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        setIntent(intent)
        resolveIntent(parseIntent(intent)) { event -> event?.let { events?.success(it) } }
    }

    private fun onMethod(
        method: String,
        call: MethodCall,
        result: MethodChannel.Result,
    ) {
        when (method) {
            "initialIntent" -> launch.request { result.success(it) }
            "startForegroundService" -> {
                val svc = Intent(this, PeerBeamService::class.java)
                    .putExtra("title", call.argument<String>("title"))
                    .putExtra("body", call.argument<String>("body"))
                    .putExtra("active", call.argument<Boolean>("active") ?: false)
                    .putExtra("incoming", call.argument<Boolean>("incoming") ?: false)
                try {
                    ContextCompat.startForegroundService(this, svc)
                    result.success(null)
                } catch (e: IllegalStateException) {
                    // Android 12+ refuses a foreground-service start issued from
                    // the background and throws
                    // `ForegroundServiceStartNotAllowedException` (an
                    // `IllegalStateException`); from 15 the same exception also
                    // reports a spent `dataSync` allowance. Both are the platform
                    // answering "no", not a defect, and both used to escape this
                    // handler uncaught — killing the whole method-call handler and
                    // leaving Dart's controller convinced it had started a service
                    // that does not exist. Reported as a channel error so the
                    // controller can record the refusal and retry later.
                    result.error(
                        "fgs_denied",
                        e.message ?: "foreground service start refused",
                        null,
                    )
                }
            }
            "stopForegroundService" -> {
                stopService(Intent(this, PeerBeamService::class.java))
                result.success(null)
            }
            "showNotification" -> {
                Notifications.ensureChannel(this)
                val n = Notifications.build(
                    this,
                    call.argument<String>("title") ?: "",
                    call.argument<String>("body") ?: "",
                    call.argument<Boolean>("ongoing") ?: false,
                    call.argument<Int>("progress"),
                    call.argument<Boolean>("incoming") ?: false,
                )
                Notifications.show(this, call.argument<Int>("id") ?: 0, n)
                result.success(null)
            }
            "cancelNotification" -> {
                Notifications.cancel(this, call.argument<Int>("id") ?: 0)
                result.success(null)
            }
            // The one field a phone is actually asked for, and the one the Rust
            // layer cannot read: `peerbeam_platform::battery` reads sysfs on
            // Linux and says nothing on Android, its comment naming this side as
            // the half that fills the gap. That half did not exist, so "share
            // device status" quietly omitted the only reading a phone has.
            "batteryStatus" -> {
                val bm = getSystemService(Context.BATTERY_SERVICE) as BatteryManager
                val percent = bm.getIntProperty(BatteryManager.BATTERY_PROPERTY_CAPACITY)
                // A device with no battery answers Integer.MIN_VALUE or a figure
                // outside 0..100; null is the schema's way of saying "no
                // reading", which is what every desktop already answers.
                if (percent in 0..100) {
                    result.success(
                        mapOf(
                            "percent" to percent,
                            "charging" to bm.isCharging,
                        ),
                    )
                } else {
                    result.success(null)
                }
            }
            "isIgnoringBatteryOptimizations" -> {
                val pm = getSystemService(Context.POWER_SERVICE) as PowerManager
                result.success(pm.isIgnoringBatteryOptimizations(packageName))
            }
            "requestIgnoreBatteryOptimizations" -> {
                val request = Intent(
                    Settings.ACTION_REQUEST_IGNORE_BATTERY_OPTIMIZATIONS,
                    Uri.parse("package:$packageName"),
                )
                // Guarded: this settings screen is not present on every build.
                // Several vendor ROMs and most Android-TV/Go images ship without
                // it, and `startActivity` then throws
                // `ActivityNotFoundException` — which crossed the platform
                // channel as an unhandled Kotlin exception and took the app down
                // from a Settings tap. A device that cannot show the dialog is
                // not a broken device; it is one where the user has nothing to
                // grant, and the caller is told so.
                try {
                    startActivity(request)
                    result.success(true)
                } catch (e: ActivityNotFoundException) {
                    result.success(false)
                }
            }
            "requestNotificationPermission" -> {
                requestNotificationPermission()
                result.success(null)
            }
            "setMulticastLock" -> {
                setMulticast(call.argument<Boolean>("enabled") ?: false)
                result.success(null)
            }
            "pickFiles" -> pickFiles(call, result)
            "safCurrentFolder" -> result.success(currentFolder())
            "safPickFolder" -> pickTree(result)
            "safSave" -> {
                val path = call.argument<String>("path")
                val name = call.argument<String>("name")
                if (path == null || name == null) {
                    result.error("args", "path and name required", null)
                } else {
                    // Chosen SAF folder if set, else the public Downloads default.
                    replyFromPublisher(result) {
                        saveToTree(path, name) ?: saveToDownloads(path, name)
                    }
                }
            }
            "safSaveTree" -> {
                val path = call.argument<String>("path")
                if (path == null) {
                    result.error("args", "path required", null)
                } else {
                    replyFromPublisher(result) { saveTree(path) }
                }
            }
            "safOpen" -> {
                val name = call.argument<String>("name") ?: ""
                result.success(openInTree(name) || openInDownloads(name))
            }
            else -> result.notImplemented()
        }
    }

    /// Run [work] on [publisher] and answer [reply] with whatever it returns,
    /// back on the main thread — the thread the call arrived on, and the only
    /// one this class's channel state is touched from.
    ///
    /// [work] runs where nothing catches for it. MethodChannel wraps a handler
    /// call in a `catch (RuntimeException)` and turns the throw into a channel
    /// error; a worker thread has no such wrapper, and an exception escaping
    /// one takes the process down. So it is caught here and reported as the
    /// error Dart would have been given anyway.
    private fun <T> replyFromPublisher(reply: MethodChannel.Result, work: () -> T) {
        publisher.execute {
            var value: T? = null
            var failure: Exception? = null
            try {
                value = work()
            } catch (e: Exception) {
                failure = e
            }
            runOnUiThread {
                val e = failure
                if (e == null) {
                    reply.success(value)
                } else {
                    reply.error("publish", e.message ?: "publish failed", null)
                }
            }
        }
    }

    /// Ask for POST_NOTIFICATIONS (Android 13+ only; older versions grant it
    /// implicitly). It's declared in the manifest but the OS still requires an
    /// explicit runtime request on API 33+, otherwise it defaults to denied and
    /// every notification silently no-ops.
    private fun requestNotificationPermission() {
        if (Build.VERSION.SDK_INT < 33) return
        val granted = ContextCompat.checkSelfPermission(
            this,
            Manifest.permission.POST_NOTIFICATIONS,
        ) == PackageManager.PERMISSION_GRANTED
        if (!granted) {
            ActivityCompat.requestPermissions(
                this,
                arrayOf(Manifest.permission.POST_NOTIFICATIONS),
                reqPostNotifications,
            )
        }
    }

    // ── Native multi-file picker (streamed to cache; never loaded into RAM) ──

    /// Launch ACTION_OPEN_DOCUMENT to pick one or more files, optionally
    /// narrowed to [call]'s `mimeTypes` list (e.g. `["image/*", "video/*"]`
    /// for the composer's "Photos & videos" choice). `type` stays the
    /// wildcard either way — EXTRA_MIME_TYPES is what Android actually
    /// filters on when it is present, and the argument is optional precisely
    /// so an older Dart caller that sends none still gets today's unfiltered
    /// picker. The result is handled in onActivityResult, which streams each
    /// picked URI into app cache off the main thread and replies with
    /// `{path, name, size}` per file — never the file's bytes.
    ///
    /// [call] may also carry `keep`: the paths the caller currently holds
    /// staged elsewhere. [preparePickedDir] never prunes a batch containing
    /// one of them, however old it is. Optional with an empty default, same
    /// as `mimeTypes`, so an older Dart caller that sends none is unaffected.
    private fun pickFiles(call: MethodCall, result: MethodChannel.Result) {
        // Extracted and validated BEFORE `result` is taken over, and that order
        // is the whole point. `call.argument` is an unchecked cast, so a
        // malformed `mimeTypes` throws below this line; MethodChannel then
        // error-replies the very Result `pendingFiles` would be pointing at, and
        // the next pick calls `success(null)` on an already-answered Result —
        // "Reply already submitted" — after which every pick fails for the life
        // of the process. Today's Dart cannot send a malformed argument, but the
        // failure it would cause is unrecoverable, and the order costs nothing.
        val raw = call.argument<Any?>("mimeTypes")
        val mimeTypes: List<String>
        if (raw == null) {
            mimeTypes = emptyList()
        } else if (raw is List<*> && raw.all { it is String }) {
            mimeTypes = raw.filterIsInstance<String>()
        } else {
            result.error("args", "mimeTypes must be a list of strings", null)
            return
        }
        // Same reasoning, same ordering, as `mimeTypes` above: validated
        // before `pendingFiles`/`pendingKeep` take over `result`.
        val rawKeep = call.argument<Any?>("keep")
        val keep: List<String>
        if (rawKeep == null) {
            keep = emptyList()
        } else if (rawKeep is List<*> && rawKeep.all { it is String }) {
            keep = rawKeep.filterIsInstance<String>()
        } else {
            result.error("args", "keep must be a list of strings", null)
            return
        }
        pendingFiles?.success(null) // abandon any prior
        pendingFiles = result
        pendingKeep = keep
        val intent = Intent(Intent.ACTION_OPEN_DOCUMENT).apply {
            addCategory(Intent.CATEGORY_OPENABLE)
            type = "*/*"
            if (mimeTypes.isNotEmpty()) {
                putExtra(Intent.EXTRA_MIME_TYPES, mimeTypes.toTypedArray())
            }
            putExtra(Intent.EXTRA_ALLOW_MULTIPLE, true)
            addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION)
        }
        try {
            startActivityForResult(intent, reqPickFiles)
        } catch (e: Exception) {
            pendingFiles = null
            pendingKeep = emptyList()
            result.error("no_picker", e.message, null)
        }
    }

    // ── Storage Access Framework ─────────────────────────────────────

    private fun pickTree(result: MethodChannel.Result) {
        // A picker was already in flight — abandon the old reply.
        pendingPick?.success(null)
        pendingPick = result
        val intent = Intent(Intent.ACTION_OPEN_DOCUMENT_TREE).apply {
            addFlags(
                Intent.FLAG_GRANT_READ_URI_PERMISSION or
                    Intent.FLAG_GRANT_WRITE_URI_PERMISSION or
                    Intent.FLAG_GRANT_PERSISTABLE_URI_PERMISSION,
            )
        }
        try {
            startActivityForResult(intent, reqPickTree)
        } catch (e: Exception) {
            pendingPick = null
            result.error("no_picker", e.message, null)
        }
    }

    @Deprecated("startActivityForResult flow for the folder/file pickers")
    override fun onActivityResult(requestCode: Int, resultCode: Int, data: Intent?) {
        @Suppress("DEPRECATION")
        super.onActivityResult(requestCode, resultCode, data)
        if (requestCode == reqPickTree) {
            val reply = pendingPick
            pendingPick = null
            val uri = if (resultCode == RESULT_OK) data?.data else null
            if (uri == null) {
                reply?.success(null)
                return
            }
            try {
                contentResolver.takePersistableUriPermission(
                    uri,
                    Intent.FLAG_GRANT_READ_URI_PERMISSION or Intent.FLAG_GRANT_WRITE_URI_PERMISSION,
                )
                safPrefs.edit().putString("tree_uri", uri.toString()).apply()
                val doc = DocumentFile.fromTreeUri(this, uri)
                reply?.success(mapOf("uri" to uri.toString(), "name" to folderName(doc, uri)))
            } catch (e: Exception) {
                reply?.error("persist", e.message, null)
            }
            return
        }
        if (requestCode == reqPickFiles) {
            val reply = pendingFiles
            val keep = pendingKeep
            pendingFiles = null
            pendingKeep = emptyList()
            if (resultCode != RESULT_OK || data == null) {
                reply?.success(emptyList<Map<String, Any?>>())
                return
            }
            val uris = ArrayList<Uri>()
            val clip = data.clipData
            if (clip != null) {
                for (i in 0 until clip.itemCount) uris.add(clip.getItemAt(i).uri)
            } else {
                data.data?.let { uris.add(it) }
            }
            // Stream each URI to cache off the main thread — a large-file
            // copy here would ANR, and this is exactly the byte[]-in-RAM
            // pattern we're replacing, just moved to the wrong thread.
            Thread {
                val dir = preparePickedDir(keep)
                val out = ArrayList<Map<String, Any?>>()
                for (uri in uris) {
                    try {
                        val name = displayName(uri)
                        val safe = name.replace('/', '_').replace(File.separatorChar, '_')
                        val dest = File(dir, "${System.currentTimeMillis()}_${out.size}_$safe")
                        contentResolver.openInputStream(uri)?.use { input ->
                            dest.outputStream().use { output -> input.copyTo(output, 64 * 1024) }
                        } ?: continue
                        out.add(
                            mapOf(
                                "path" to dest.absolutePath,
                                "name" to name,
                                "size" to dest.length(),
                            ),
                        )
                    } catch (e: Exception) {
                        // skip a file that fails to copy
                    }
                }
                runOnUiThread { reply?.success(out) }
            }.start()
            return
        }
    }

    /// The persisted destination tree, or null if none set / permission lost.
    private fun persistedTree(): Uri? {
        val stored = safPrefs.getString("tree_uri", null) ?: return null
        val uri = Uri.parse(stored)
        val held = contentResolver.persistedUriPermissions.any {
            it.uri == uri && it.isWritePermission
        }
        return if (held) uri else null
    }

    /// Remember that the file Dart asked us to publish as [requested] actually
    /// landed under [actual], so [publishedName] can find it again.
    ///
    /// Needed because [uniqueName] can only be honest at the cost of the two
    /// names diverging, and `safOpen` is given the *requested* one: History and
    /// Chat record the name the engine reported and fall back to opening by it
    /// once the engine's own copy is gone. Without this, tapping a received
    /// `taxes.pdf` that was published as `taxes (1).pdf` would open the user's
    /// unrelated `taxes.pdf` sitting next to it — silently showing them the
    /// wrong document.
    ///
    /// Keyed by the requested name, so re-receiving the same name replaces its
    /// entry instead of adding one: this grows with the number of *distinct*
    /// colliding names, not with the number of files received. An equal pair
    /// clears the entry rather than storing it — once the collision is gone
    /// (the user deleted their copy) a stale alias would send `safOpen` to the
    /// older suffixed file forever.
    private fun rememberPublishedName(requested: String, actual: String) {
        val key = "published:$requested"
        val edit = safPrefs.edit()
        if (requested == actual) edit.remove(key) else edit.putString(key, actual)
        edit.apply()
    }

    /// The name the file published as [requested] actually landed under — the
    /// requested name itself unless a collision forced [uniqueName] to pick
    /// another. Deliberately does **not** fall back to [requested] when the
    /// alias names something that is no longer there: that fallback is exactly
    /// the wrong-document open this alias exists to prevent.
    private fun publishedName(requested: String): String =
        safPrefs.getString("published:$requested", null) ?: requested

    /// The current destination shown in Settings: a chosen SAF folder if set,
    /// otherwise the zero-config public Downloads/PeerBeam default (API 29+),
    /// otherwise null (old devices fall back to app storage).
    private fun currentFolder(): Map<String, Any?>? {
        val uri = persistedTree()
        if (uri != null) {
            val doc = DocumentFile.fromTreeUri(this, uri)
            if (doc != null) {
                return mapOf(
                    "uri" to uri.toString(),
                    "name" to folderName(doc, uri),
                    "isDefault" to false,
                )
            }
        }
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
            return mapOf("uri" to "", "name" to "Downloads/PeerBeam", "isDefault" to true)
        }
        return null
    }

    /// Copy [path] into the chosen tree under [name], or the first free
    /// ` (n)` variant of it when the user already has a file by that name.
    /// Returns the new document URI, or null if no tree / the copy failed.
    private fun saveToTree(path: String, name: String): String? {
        val uri = persistedTree() ?: return null
        val tree = DocumentFile.fromTreeUri(this, uri) ?: return null
        val src = File(path)
        if (!src.exists()) return null
        val doc = copyFileIntoDir(tree, name, src) ?: return null
        // `doc.name`, not the name we asked for: a DocumentsProvider is free to
        // rename on its own (most append their own " (1)" on a collision that
        // slipped in between our check and the create), and the alias is only
        // worth keeping if it names the document that actually exists.
        rememberPublishedName(name, doc.name ?: name)
        return doc.uri.toString()
    }

    /// Copy local file [src] into SAF directory [dir] as [name], or as the
    /// first free ` (n)` variant when [dir] already holds that name. Returns
    /// the new document, or null if the source is missing or the copy failed.
    ///
    /// This used to `findFile(name)?.delete()` for overwrite semantics, on a
    /// peer-supplied name — so a paired device could destroy any document in
    /// the user's chosen folder just by sending a file called the same thing.
    /// See [uniqueName], which the engine's own writes have always had.
    private fun copyFileIntoDir(dir: DocumentFile, name: String, src: File): DocumentFile? {
        if (!src.exists()) return null
        val free = uniqueName(name) { dir.findFile(it) != null }
        val doc = dir.createFile(mimeOf(free), free) ?: return null
        return try {
            contentResolver.openOutputStream(doc.uri)?.use { out ->
                src.inputStream().use { it.copyTo(out) }
            } ?: run {
                doc.delete()
                return null
            }
            doc
        } catch (e: Exception) {
            doc.delete()
            null
        }
    }

    // ── Recursive folder publish (received folders) ──────────────────

    /// Publish every regular file under local folder [path] into the user's
    /// destination — the chosen SAF tree if one is set, else public
    /// Downloads/PeerBeam (API 29+) — preserving the folder's own name and its
    /// subdirectory structure. Returns true only if every file was published;
    /// false if nothing is set up (no SAF tree and API < 29) or any file failed
    /// (best-effort: as many files as possible are still published).
    private fun saveTree(path: String): Boolean {
        val root = File(path)
        if (!root.isDirectory) return false
        val files = root.walkTopDown().filter { it.isFile }.toList()
        val treeUri = persistedTree()
        return when {
            treeUri != null -> saveTreeToTree(treeUri, root, files)
            Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q -> saveTreeToDownloads(root, files)
            else -> false
        }
    }

    /// Publish [files] (all under [root]) into the SAF tree at [treeUri],
    /// recreating `<root.name>/<subdirs>` beneath it.
    private fun saveTreeToTree(treeUri: Uri, root: File, files: List<File>): Boolean {
        val tree = DocumentFile.fromTreeUri(this, treeUri) ?: return false
        var ok = true
        for (f in files) {
            val segments = relativeSegments(root, f)
            val dir = findOrCreateDirs(tree, listOf(root.name) + segments.dropLast(1))
            if (dir == null || copyFileIntoDir(dir, segments.last(), f) == null) ok = false
        }
        return ok
    }

    /// [f]'s path components relative to [root] (e.g. `sub/a.txt` under `root`
    /// yields `["sub", "a.txt"]`; a direct child yields `["a.txt"]`).
    private fun relativeSegments(root: File, f: File): List<String> =
        f.relativeTo(root).path.split(File.separatorChar).filter { it.isNotEmpty() }

    /// Find-or-create each directory in [segments] under [start] in order,
    /// returning the innermost directory, or null if a segment collides with a
    /// same-name file or can't be created.
    private fun findOrCreateDirs(start: DocumentFile, segments: List<String>): DocumentFile? {
        var dir = start
        for (seg in segments) {
            val existing = dir.findFile(seg)
            dir = when {
                existing != null && existing.isDirectory -> existing
                existing != null -> return null // a same-name file blocks the dir
                else -> dir.createDirectory(seg) ?: return null
            }
        }
        return dir
    }

    /// Open a previously-saved file from the tree by [name] with a view intent.
    private fun openInTree(name: String): Boolean {
        val uri = persistedTree() ?: return false
        val tree = DocumentFile.fromTreeUri(this, uri) ?: return false
        val doc = tree.findFile(publishedName(name)) ?: return false
        return try {
            startActivity(
                Intent(Intent.ACTION_VIEW).apply {
                    setDataAndType(doc.uri, doc.type ?: mimeOf(name))
                    addFlags(
                        Intent.FLAG_GRANT_READ_URI_PERMISSION or Intent.FLAG_ACTIVITY_NEW_TASK,
                    )
                },
            )
            true
        } catch (e: Exception) {
            false
        }
    }

    // ── MediaStore Downloads/PeerBeam (zero-config default, API 29+) ──

    /// Copy [path] into public Downloads/PeerBeam via MediaStore (no runtime
    /// permission). Returns the URI, or null when unsupported (API < 29) / the
    /// copy failed.
    private fun saveToDownloads(path: String, name: String): String? {
        val (uri, saved) = saveToDownloadsAt(path, name, "Download/PeerBeam") ?: return null
        rememberPublishedName(name, saved)
        return uri
    }

    /// Publish [files] (all under [root]) into public Downloads via MediaStore,
    /// under `Download/PeerBeam/<root.name>/<subdirs>` per file (API 29+ only —
    /// callers must gate on that).
    private fun saveTreeToDownloads(root: File, files: List<File>): Boolean {
        var ok = true
        for (f in files) {
            val segments = relativeSegments(root, f)
            val subdir = (listOf(root.name) + segments.dropLast(1)).joinToString("/")
            val relativePath = "Download/PeerBeam/$subdir"
            // No [rememberPublishedName] here: `safOpen` only ever looks in the
            // destination's top level, so a file published inside a received
            // folder was never findable by bare name and an alias would only
            // point at a name that isn't there.
            if (saveToDownloadsAt(f.path, segments.last(), relativePath) == null) ok = false
        }
        return ok
    }

    /// Copy [path] into Downloads at an explicit [relativePath] under
    /// `Download/…` (e.g. `Download/PeerBeam/myfolder/sub` for a file inside a
    /// received folder), as [name] or the first free ` (n)` variant of it.
    /// Returns the URI **and the name it was actually saved under**, or null
    /// when unsupported (API < 29) / the copy failed.
    ///
    /// This used to `deleteFromDownloadsAt(name, relativePath)` first, for
    /// overwrite semantics, on a peer-supplied name — the same clobber
    /// [copyFileIntoDir] carried, against whatever the user keeps in
    /// `Download/PeerBeam`.
    private fun saveToDownloadsAt(
        path: String,
        name: String,
        relativePath: String,
    ): Pair<String, String>? {
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.Q) return null
        val src = File(path)
        if (!src.exists()) return null
        val free = uniqueName(name) { downloadsUriByNameAt(it, relativePath) != null }
        val collection = MediaStore.Downloads.getContentUri(MediaStore.VOLUME_EXTERNAL_PRIMARY)
        val values = ContentValues().apply {
            put(MediaStore.Downloads.DISPLAY_NAME, free)
            put(MediaStore.Downloads.RELATIVE_PATH, relativePath)
            put(MediaStore.Downloads.MIME_TYPE, mimeOf(free))
            put(MediaStore.Downloads.IS_PENDING, 1)
        }
        val uri = contentResolver.insert(collection, values) ?: return null
        return try {
            contentResolver.openOutputStream(uri)?.use { out ->
                src.inputStream().use { it.copyTo(out) }
            } ?: run {
                contentResolver.delete(uri, null, null)
                return null
            }
            contentResolver.update(
                uri,
                ContentValues().apply { put(MediaStore.Downloads.IS_PENDING, 0) },
                null,
                null,
            )
            uri.toString() to free
        } catch (e: Exception) {
            contentResolver.delete(uri, null, null)
            null
        }
    }

    private fun downloadsUriByName(name: String): Uri? =
        downloadsUriByNameAt(name, "Download/PeerBeam")

    private fun downloadsUriByNameAt(name: String, relativePath: String): Uri? {
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.Q) return null
        val collection = MediaStore.Downloads.getContentUri(MediaStore.VOLUME_EXTERNAL_PRIMARY)
        // Exact match on the trailing-slash-normalized RELATIVE_PATH (rather
        // than a `LIKE '%path%'` substring) so nested subdirectories can't
        // collide with this one, and so `%`/`_` in a folder name are treated
        // literally instead of as SQL LIKE wildcards.
        val dir = if (relativePath.endsWith("/")) relativePath else "$relativePath/"
        val sel = "${MediaStore.Downloads.RELATIVE_PATH} = ? AND " +
            "${MediaStore.Downloads.DISPLAY_NAME} = ?"
        val args = arrayOf(dir, name)
        contentResolver.query(collection, arrayOf(MediaStore.Downloads._ID), sel, args, null)
            ?.use { c ->
                if (c.moveToFirst()) {
                    val id = c.getLong(c.getColumnIndexOrThrow(MediaStore.Downloads._ID))
                    return ContentUris.withAppendedId(collection, id)
                }
            }
        return null
    }

    private fun openInDownloads(name: String): Boolean {
        val uri = downloadsUriByName(publishedName(name)) ?: return false
        return try {
            startActivity(
                Intent(Intent.ACTION_VIEW).apply {
                    setDataAndType(uri, mimeOf(name))
                    addFlags(
                        Intent.FLAG_GRANT_READ_URI_PERMISSION or Intent.FLAG_ACTIVITY_NEW_TASK,
                    )
                },
            )
            true
        } catch (e: Exception) {
            false
        }
    }

    private fun folderName(doc: DocumentFile?, uri: Uri): String =
        doc?.name ?: uri.lastPathSegment ?: "Selected folder"

    private fun mimeOf(name: String): String {
        val ext = name.substringAfterLast('.', "").lowercase()
        return MimeTypeMap.getSingleton().getMimeTypeFromExtension(ext)
            ?: "application/octet-stream"
    }

    private fun setMulticast(enabled: Boolean) {
        val wifi = applicationContext.getSystemService(Context.WIFI_SERVICE) as WifiManager
        if (enabled) {
            if (multicastLock == null) {
                multicastLock = wifi.createMulticastLock("peerbeam:mdns")
                    .apply { setReferenceCounted(false) }
            }
            multicastLock?.let { if (!it.isHeld) it.acquire() }
        } else {
            multicastLock?.let { if (it.isHeld) it.release() }
        }
    }

    /// What a share/view intent is asking for, worked out without reading a
    /// byte. [parseIntent] runs on the main thread, so anything that needs a
    /// provider's bytes copied is left as URIs for [resolveIntent] to
    /// materialize on a worker thread.
    private sealed interface Payload {
        /// Nothing to copy — the event is already what Dart gets.
        data class Ready(val event: Map<String, Any?>) : Payload

        /// URIs the Rust engine cannot open as they are (`content://` from
        /// Photos, Files, WhatsApp, Downloads under scoped storage…).
        data class Files(val event: String, val uris: List<Uri>) : Payload
    }

    private fun parseIntent(intent: Intent?): Payload? {
        intent ?: return null
        return when (intent.action) {
            Intent.ACTION_SEND -> {
                val uri = parcelableExtra(intent, Intent.EXTRA_STREAM)
                val text = intent.getStringExtra(Intent.EXTRA_TEXT)
                when {
                    uri != null -> Payload.Files("share", listOf(uri))
                    text != null -> Payload.Ready(mapOf("event" to "share", "text" to text))
                    else -> null
                }
            }
            Intent.ACTION_SEND_MULTIPLE -> {
                val uris = parcelableArrayList(intent, Intent.EXTRA_STREAM)
                if (!uris.isNullOrEmpty()) Payload.Files("share", uris) else null
            }
            Intent.ACTION_VIEW -> intent.data?.let { Payload.Files("view", listOf(it)) }
            else -> null
        }
    }

    /// Turn [payload] into the event map Dart consumes and hand it to
    /// [deliver] on the main thread.
    ///
    /// A payload with files goes to a worker thread first: materializing them
    /// is a whole-file copy per URI (see [resolveToRealPath]). A text share, or
    /// no share at all, is delivered inline — there is nothing to wait for, and
    /// it should still reach Dart in the same breath as before.
    ///
    /// Its own thread rather than [publisher]'s: a share the user just sent to
    /// the app should not queue behind however many received files are being
    /// published. And [deliver] is called whatever the copy did, including
    /// blowing up — a request parked in [PendingLaunch] is waiting on it, and
    /// Dart's startup is waiting on that.
    private fun resolveIntent(payload: Payload?, deliver: (Map<String, Any?>?) -> Unit) {
        when (payload) {
            null -> deliver(null)
            is Payload.Ready -> deliver(payload.event)
            is Payload.Files ->
                Thread {
                    val event = try {
                        fileEvent(payload.event, payload.uris)
                    } catch (e: Exception) {
                        null
                    }
                    runOnUiThread { deliver(event) }
                }.start()
        }
    }

    /// Builds the Dart-facing share/view event, resolving every incoming URI to
    /// a real filesystem path first — the Rust engine opens paths via
    /// `tokio::fs`, which can't read a `content://` URI directly.
    ///
    /// Copies bytes, so it only ever runs on [resolveIntent]'s worker thread.
    private fun fileEvent(event: String, uris: List<Uri>): Map<String, Any?> {
        val paths = ArrayList<String>()
        val names = ArrayList<String>()
        val sharedDir = prepareSharedDir()
        uris.forEachIndexed { index, uri ->
            val name = displayName(uri)
            val path = resolveToRealPath(uri, sharedDir, name, index)
            if (path != null) {
                paths.add(path)
                names.add(name)
            }
            // else: unreadable URI (stream open failed) — skip it rather than
            // hand Dart a path that doesn't exist.
        }
        return mapOf("event" to event, "paths" to paths, "names" to names)
    }

    /// Resolves [uri] to a path the Rust engine can open with `std`/`tokio::fs`.
    /// `file://` URIs already are one and are returned as-is (no copy). Every
    /// other scheme (`content://` from Photos/Files/WhatsApp/Downloads under
    /// scoped storage, etc.) is materialized into [sharedDir] first — a
    /// whole-file copy, which is why [resolveIntent] only ever calls this from
    /// a worker thread. Returns null if the URI can't be opened at all, so the
    /// caller can skip it.
    private fun resolveToRealPath(uri: Uri, sharedDir: File, name: String, index: Int): String? {
        if (uri.scheme == "file") return uri.path
        val safeName = sanitizeFileName(name).ifEmpty { "shared_$index" }
        // Prefix with the batch index so two shares with the same display
        // name (e.g. two "photo.jpg" from different folders) don't collide
        // on disk and silently overwrite one another.
        val dest = File(sharedDir, "${index}_$safeName")
        return try {
            contentResolver.openInputStream(uri)?.use { input ->
                dest.outputStream().use { output -> input.copyTo(output) }
            } ?: return null
            dest.absolutePath
        } catch (e: Exception) {
            dest.delete()
            null
        }
    }

    /// Cache subdirectory `name` holding one fresh, uniquely-named batch per
    /// call, rather than a single directory reused (and wiped) every time —
    /// so a new batch can never delete a previous one while it is still in
    /// use. A batch is pruned once it is older than `maxAgeMs`, UNLESS one of
    /// its files is named in `keep` (paths the caller says it still needs),
    /// in which case it survives regardless of age: age alone would still
    /// discard a batch the app is deliberately holding onto longer than the
    /// cutoff.
    private fun prepareBatchDir(name: String, maxAgeMs: Long, keep: List<String> = emptyList()): File {
        val root = File(cacheDir, name)
        root.mkdirs()
        val cutoff = System.currentTimeMillis() - maxAgeMs
        val keptDirs = keep.mapNotNull { File(it).parentFile?.absolutePath }.toHashSet()
        root.listFiles()?.forEach { child ->
            if (child.absolutePath !in keptDirs && child.lastModified() < cutoff) {
                child.deleteRecursively()
            }
        }
        val batch = File(root, System.nanoTime().toString())
        batch.mkdirs()
        return batch
    }

    /// Cache subdirectory that share-in copies are materialized into. No
    /// [keep] list is needed here: unlike a pick, a share-in copy is finished
    /// before its path is handed to Dart — [resolveIntent] delivers the event
    /// only once [fileEvent] has returned — so an hour is well past any
    /// plausible time for that path to still be in use.
    private fun prepareSharedDir(): File = prepareBatchDir("shared", 60 * 60 * 1000L)

    /// Cache subdirectory that the native file picker streams picked files
    /// into. A picked batch's bytes may still be needed long after the pick
    /// itself returns — the false assumption a blanket wipe-on-every-pick
    /// used to make here, which deleted files still in use. The Send flow
    /// only reads a staged file's path back when the user finally taps Send,
    /// which they may put off as long as they like; a chat attachment's
    /// staging copy keeps reading its source for as long as the copy takes,
    /// unawaited, in the background, which for a large file is minutes. The
    /// cutoff is a day rather than [prepareSharedDir]'s hour because a picked
    /// batch waits on the user where a share-in goes straight into a
    /// transfer, and [keep] — the paths [pickFiles]'s caller says it still
    /// holds staged — covers what even that cutoff cannot: a batch left
    /// staged past it still survives.
    private fun preparePickedDir(keep: List<String>): File =
        prepareBatchDir("picked", 24 * 60 * 60 * 1000L, keep)

    /// Strips path separators from a display name so it can't escape
    /// [prepareSharedDir]'s directory or collide with it structurally.
    private fun sanitizeFileName(name: String): String =
        name.replace('/', '_').replace('\\', '_').trim()

    private fun displayName(uri: Uri): String {
        var name = uri.lastPathSegment ?: "file"
        try {
            contentResolver.query(uri, arrayOf(OpenableColumns.DISPLAY_NAME), null, null, null)
                ?.use { cursor ->
                    if (cursor.moveToFirst()) {
                        val idx = cursor.getColumnIndex(OpenableColumns.DISPLAY_NAME)
                        if (idx >= 0) name = cursor.getString(idx) ?: name
                    }
                }
        } catch (_: Exception) {
        }
        return name
    }

    @Suppress("DEPRECATION")
    private fun parcelableExtra(intent: Intent, key: String): Uri? =
        if (Build.VERSION.SDK_INT >= 33) {
            intent.getParcelableExtra(key, Uri::class.java)
        } else {
            intent.getParcelableExtra(key)
        }

    @Suppress("DEPRECATION")
    private fun parcelableArrayList(intent: Intent, key: String): ArrayList<Uri>? =
        if (Build.VERSION.SDK_INT >= 33) {
            intent.getParcelableArrayListExtra(key, Uri::class.java)
        } else {
            intent.getParcelableArrayListExtra(key)
        }
}

/// The launching share/view intent on its way to Dart, holding whichever of
/// the two sides turned up first.
///
/// Dart asks for the launch intent exactly once, during startup, and reads a
/// null answer as "nothing was shared". That answer is not always ready when
/// the question arrives: the files a share names have to be copied out of
/// their provider first, and that copy runs off the main thread precisely so
/// it cannot stall the launch. Answering "nothing" in the meantime would throw
/// the share away — the failure the copy moved off-thread to avoid, arriving
/// by another route — so a request that lands mid-copy is parked and answered
/// when [deliver] lands.
///
/// Parking does cost the rest of Dart's boot sequence the length of that copy,
/// since `initialIntent` is awaited in the middle of it — but the app is
/// drawing and responsive throughout, which is what the copy moved off the
/// main thread to get.
///
/// Not thread-safe by design: every call arrives on the main thread, the copy
/// worker included, which posts [deliver] back there.
internal class PendingLaunch {
    private var event: Map<String, Any?>? = null
    private var waiter: ((Map<String, Any?>?) -> Unit)? = null
    private var resolving = false

    /// Marks a resolve as under way. From here until [deliver], a [request] is
    /// parked instead of answered.
    fun resolving() {
        resolving = true
    }

    /// The resolved event — null when the intent carried nothing to share, or
    /// when resolving it failed. Answers a parked request, or waits for one.
    fun deliver(resolved: Map<String, Any?>?) {
        resolving = false
        val parked = waiter
        waiter = null
        if (parked != null) parked(resolved) else event = resolved
    }

    /// Dart asking for the launch intent. Answered straight away unless a
    /// resolve is still running, in which case [reply] is held until [deliver].
    fun request(reply: (Map<String, Any?>?) -> Unit) {
        if (resolving) {
            // Abandon any prior request, as the pickers do: a Result that is
            // never replied to leaks its Dart-side future forever.
            waiter?.invoke(null)
            waiter = reply
            return
        }
        val ready = event
        event = null // consumed; a launch intent is delivered once
        reply(ready)
    }
}
