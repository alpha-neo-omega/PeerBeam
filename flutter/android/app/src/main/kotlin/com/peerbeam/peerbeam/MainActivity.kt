package com.peerbeam.peerbeam

import android.Manifest
import android.content.ContentUris
import android.content.ContentValues
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.net.Uri
import android.net.wifi.WifiManager
import android.os.Build
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

class MainActivity : FlutterActivity() {
    private val methodName = "peerbeam/android"
    private val eventName = "peerbeam/android/events"

    private var events: EventChannel.EventSink? = null
    private var pendingInitial: Map<String, Any?>? = null
    private var multicastLock: WifiManager.MulticastLock? = null

    // Storage Access Framework: the user picks a destination folder once; we
    // persist the grant and copy received files into it (the Rust engine writes
    // via std::fs to app storage, which the OS hides — SAF makes files visible).
    private val reqPickTree = 4210
    private var pendingPick: MethodChannel.Result? = null

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
                }

                override fun onCancel(arguments: Any?) {
                    events = null
                }
            },
        )

        // The intent that launched us (cold-start share/view), delivered to
        // Dart on demand via `initialIntent`.
        pendingInitial = parseIntent(intent)
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        setIntent(intent)
        parseIntent(intent)?.let { events?.success(it) }
    }

    private fun onMethod(
        method: String,
        call: MethodCall,
        result: MethodChannel.Result,
    ) {
        when (method) {
            "initialIntent" -> {
                result.success(pendingInitial)
                pendingInitial = null
            }
            "startForegroundService" -> {
                val svc = Intent(this, PeerBeamService::class.java)
                    .putExtra("title", call.argument<String>("title"))
                    .putExtra("body", call.argument<String>("body"))
                    .putExtra("active", call.argument<Boolean>("active") ?: false)
                    .putExtra("incoming", call.argument<Boolean>("incoming") ?: false)
                ContextCompat.startForegroundService(this, svc)
                result.success(null)
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
            "isIgnoringBatteryOptimizations" -> {
                val pm = getSystemService(Context.POWER_SERVICE) as PowerManager
                result.success(pm.isIgnoringBatteryOptimizations(packageName))
            }
            "requestIgnoreBatteryOptimizations" -> {
                val request = Intent(
                    Settings.ACTION_REQUEST_IGNORE_BATTERY_OPTIMIZATIONS,
                    Uri.parse("package:$packageName"),
                )
                startActivity(request)
                result.success(null)
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
                    result.success(saveToTree(path, name) ?: saveToDownloads(path, name))
                }
            }
            "safSaveTree" -> {
                val path = call.argument<String>("path")
                if (path == null) {
                    result.error("args", "path required", null)
                } else {
                    result.success(saveTree(path))
                }
            }
            "safOpen" -> {
                val name = call.argument<String>("name") ?: ""
                result.success(openInTree(name) || openInDownloads(name))
            }
            else -> result.notImplemented()
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

    private fun parseIntent(intent: Intent?): Map<String, Any?>? {
        intent ?: return null
        return when (intent.action) {
            Intent.ACTION_SEND -> {
                val uri = parcelableExtra(intent, Intent.EXTRA_STREAM)
                val text = intent.getStringExtra(Intent.EXTRA_TEXT)
                when {
                    uri != null -> fileEvent("share", listOf(uri))
                    text != null -> mapOf("event" to "share", "text" to text)
                    else -> null
                }
            }
            Intent.ACTION_SEND_MULTIPLE -> {
                val uris = parcelableArrayList(intent, Intent.EXTRA_STREAM)
                if (!uris.isNullOrEmpty()) fileEvent("share", uris) else null
            }
            Intent.ACTION_VIEW -> intent.data?.let { fileEvent("view", listOf(it)) }
            else -> null
        }
    }

    /// Builds the Dart-facing share/view event, resolving every incoming URI to
    /// a real filesystem path first — the Rust engine opens paths via
    /// `tokio::fs`, which can't read a `content://` URI directly.
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
    /// synchronous copy on the UI thread, same as LocalSend does for share-in;
    /// large shared files will visibly block until the copy completes. Returns
    /// null if the URI can't be opened at all, so the caller can skip it.
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
    /// [keep] list is needed here: unlike a pick, a share-in copy happens
    /// synchronously on the UI thread in [resolveToRealPath], so by the time
    /// its path is handed to Dart the copy is already complete — an hour is
    /// well past any plausible time for that path to still be in use.
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
