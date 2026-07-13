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

        Notifications.ensureChannel(this)
        val notification = Notifications.build(this, title, body, true, null)

        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
            startForeground(
                Notifications.SERVICE_ID,
                notification,
                ServiceInfo.FOREGROUND_SERVICE_TYPE_DATA_SYNC,
            )
        } else {
            startForeground(Notifications.SERVICE_ID, notification)
        }

        acquireLocks()
        return START_STICKY
    }

    private fun acquireLocks() {
        val wifi = applicationContext.getSystemService(Context.WIFI_SERVICE) as WifiManager
        wifiLock = wifi.createWifiLock(WifiManager.WIFI_MODE_FULL_HIGH_PERF, "peerbeam:wifi")
            .apply {
                setReferenceCounted(false)
                acquire()
            }

        val power = getSystemService(Context.POWER_SERVICE) as PowerManager
        wakeLock = power.newWakeLock(PowerManager.PARTIAL_WAKE_LOCK, "peerbeam:transfer")
            .apply { acquire(6 * 60 * 60 * 1000L) } // safety cap: 6h
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
