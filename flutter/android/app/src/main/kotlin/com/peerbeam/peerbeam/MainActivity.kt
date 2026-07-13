package com.peerbeam.peerbeam

import android.content.Context
import android.content.Intent
import android.net.Uri
import android.net.wifi.WifiManager
import android.os.Build
import android.os.PowerManager
import android.provider.OpenableColumns
import android.provider.Settings
import androidx.core.content.ContextCompat
import io.flutter.embedding.android.FlutterActivity
import io.flutter.embedding.engine.FlutterEngine
import io.flutter.plugin.common.EventChannel
import io.flutter.plugin.common.MethodCall
import io.flutter.plugin.common.MethodChannel

class MainActivity : FlutterActivity() {
    private val methodName = "peerbeam/android"
    private val eventName = "peerbeam/android/events"

    private var events: EventChannel.EventSink? = null
    private var pendingInitial: Map<String, Any?>? = null
    private var multicastLock: WifiManager.MulticastLock? = null

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
            "setMulticastLock" -> {
                setMulticast(call.argument<Boolean>("enabled") ?: false)
                result.success(null)
            }
            else -> result.notImplemented()
        }
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

    private fun fileEvent(event: String, uris: List<Uri>): Map<String, Any?> {
        val paths = ArrayList<String>()
        val names = ArrayList<String>()
        for (uri in uris) {
            paths.add(uri.toString())
            names.add(displayName(uri))
        }
        return mapOf("event" to event, "paths" to paths, "names" to names)
    }

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
