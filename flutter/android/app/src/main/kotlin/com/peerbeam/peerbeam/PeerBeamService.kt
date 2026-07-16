package com.peerbeam.peerbeam

import android.app.Service
import android.content.Context
import android.content.Intent
import android.content.pm.ServiceInfo
import android.net.wifi.WifiManager
import android.os.Build
import android.os.IBinder
import android.os.PowerManager

/**
 * Foreground service keeping transfers / receive alive across backgrounding.
 * Holds Wi-Fi + CPU wake locks while running so Doze doesn't suspend sockets.
 */
class PeerBeamService : Service() {
    private var wifiLock: WifiManager.WifiLock? = null
    private var wakeLock: PowerManager.WakeLock? = null

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        val title = intent?.getStringExtra("title") ?: "PeerBeam"
        val body = intent?.getStringExtra("body") ?: "Active"
        // Active = a transfer is moving bytes: animate the notification (an
        // indeterminate bar) and hold the CPU awake. Idle receive-ready shows a
        // static notification and holds no CPU wake lock (battery-friendly).
        val active = intent?.getBooleanExtra("active", false) ?: false

        Notifications.ensureChannel(this)
        val notification = Notifications.build(
            this,
            title,
            body,
            true,
            if (active) -1 else null,
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
        return START_STICKY
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
        releaseLocks()
        super.onDestroy()
    }
}
