package com.peerbeam.peerbeam

import android.app.Service
import android.content.Context
import android.content.Intent
import android.content.pm.ServiceInfo
import android.net.wifi.WifiManager
import android.os.Build
import android.os.Handler
import android.os.IBinder
import android.os.Looper
import android.os.PowerManager
import androidx.core.app.NotificationManagerCompat

/**
 * Foreground service keeping transfers / receive alive across backgrounding.
 * Holds Wi-Fi + CPU wake locks while running so Doze doesn't suspend sockets.
 */
class PeerBeamService : Service() {
    private var wifiLock: WifiManager.WifiLock? = null
    private var wakeLock: PowerManager.WakeLock? = null

    // Status-bar icon animation. Android doesn't frame-animate a notification
    // small icon on its own, so while a transfer is active we cycle the icon
    // through frames by re-posting the notification on a timer (down-arrow
    // descends for receives, up-arrow rises for sends). Idle = a static icon.
    private val animHandler = Handler(Looper.getMainLooper())
    private var animRunnable: Runnable? = null
    private val dlFrames = intArrayOf(
        R.drawable.ic_stat_dl0, R.drawable.ic_stat_dl1, R.drawable.ic_stat_dl2,
    )
    private val ulFrames = intArrayOf(
        R.drawable.ic_stat_ul0, R.drawable.ic_stat_ul1, R.drawable.ic_stat_ul2,
    )

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        // A null intent means the OS restarted the service after killing the
        // process. The receive engine lives in the (now-gone) Flutter process,
        // so a resurrected service can't actually receive anything — it would
        // just hold a Wi-Fi lock + a stale "Active" notification with no way to
        // stop it. Bail out instead of running as a zombie; the app re-creates
        // the service properly on next launch.
        if (intent == null) {
            releaseLocks()
            stopForeground(STOP_FOREGROUND_REMOVE)
            stopSelf()
            return START_NOT_STICKY
        }
        val title = intent.getStringExtra("title") ?: "PeerBeam"
        val body = intent.getStringExtra("body") ?: "Active"
        // Active = a transfer is moving bytes: animate the notification (an
        // indeterminate bar) and hold the CPU awake. Idle receive-ready shows a
        // static notification and holds no CPU wake lock (battery-friendly).
        val active = intent.getBooleanExtra("active", false)
        val incoming = intent.getBooleanExtra("incoming", false)

        Notifications.ensureChannel(this)
        val frames = if (incoming) dlFrames else ulFrames
        val notification = Notifications.build(
            this,
            title,
            body,
            true,
            if (active) -1 else null,
            incoming,
            if (active) frames[0] else null,
        )

        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
            startForeground(
                Notifications.SERVICE_ID,
                notification,
                ServiceInfo.FOREGROUND_SERVICE_TYPE_DATA_SYNC,
            )
        } else {
            startForeground(Notifications.SERVICE_ID, notification)
        }

        updateLocks(active)
        if (active) startIconAnimation(title, body, incoming) else stopIconAnimation()
        // Not sticky: don't let the OS resurrect the service without its engine
        // (see the null-intent guard above). The app re-starts it on relaunch.
        return START_NOT_STICKY
    }

    /// While a transfer is active, cycle the small icon through frames (~2/s) so
    /// the status-bar icon appears to animate. `startForeground` already posted
    /// frame 0; this loop advances from frame 1 onward, re-posting the same
    /// notification id. Stopped when idle / on destroy.
    private fun startIconAnimation(title: String, body: String, incoming: Boolean) {
        stopIconAnimation()
        val frames = if (incoming) dlFrames else ulFrames
        var i = 1
        val runnable = object : Runnable {
            override fun run() {
                val n = Notifications.build(
                    this@PeerBeamService, title, body, true, -1, incoming,
                    frames[i % frames.size],
                )
                try {
                    NotificationManagerCompat.from(this@PeerBeamService)
                        .notify(Notifications.SERVICE_ID, n)
                } catch (_: SecurityException) {
                    // POST_NOTIFICATIONS not granted — skip silently.
                }
                i++
                animHandler.postDelayed(this, 450)
            }
        }
        animRunnable = runnable
        animHandler.postDelayed(runnable, 450)
    }

    private fun stopIconAnimation() {
        animRunnable?.let { animHandler.removeCallbacks(it) }
        animRunnable = null
    }

    /// Wi-Fi lock is held for the whole service lifetime so incoming transfers
    /// stay reachable. The CPU partial wake lock is held ONLY while a transfer
    /// is active — idle receive-ready lets the CPU doze (the battery
    /// optimization) — and is released on service stop, so no fixed cap can
    /// stall a long transfer. onStartCommand can fire repeatedly, so this is
    /// idempotent: it acquires each lock only when not already held.
    private fun updateLocks(active: Boolean) {
        if (wifiLock == null) {
            val wifi = applicationContext.getSystemService(Context.WIFI_SERVICE) as WifiManager
            wifiLock = wifi
                .createWifiLock(WifiManager.WIFI_MODE_FULL_HIGH_PERF, "peerbeam:wifi")
                .apply {
                    setReferenceCounted(false)
                    acquire()
                }
        }

        val power = getSystemService(Context.POWER_SERVICE) as PowerManager
        if (active) {
            if (wakeLock == null) {
                wakeLock = power
                    .newWakeLock(PowerManager.PARTIAL_WAKE_LOCK, "peerbeam:transfer")
                    .apply { setReferenceCounted(false) }
            }
            wakeLock?.let { if (!it.isHeld) it.acquire() }
        } else {
            wakeLock?.let { if (it.isHeld) it.release() }
        }
    }

    private fun releaseLocks() {
        wifiLock?.let { if (it.isHeld) it.release() }
        wakeLock?.let { if (it.isHeld) it.release() }
        wifiLock = null
        wakeLock = null
    }

    override fun onDestroy() {
        stopIconAnimation()
        releaseLocks()
        super.onDestroy()
    }
}
