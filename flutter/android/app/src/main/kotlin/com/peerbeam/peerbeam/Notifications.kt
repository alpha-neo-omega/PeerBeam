package com.peerbeam.peerbeam

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.Context
import android.content.Intent
import android.os.Build
import androidx.core.app.NotificationCompat
import androidx.core.app.NotificationManagerCompat

/** Notification channel + builders shared by the service and transfer events. */
object Notifications {
    const val CHANNEL_ID = "peerbeam_transfers"
    const val SERVICE_ID = 1

    fun ensureChannel(context: Context) {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            val channel = NotificationChannel(
                CHANNEL_ID,
                "Transfers",
                NotificationManager.IMPORTANCE_LOW,
            ).apply { description = "File transfer status" }
            context.getSystemService(NotificationManager::class.java)
                ?.createNotificationChannel(channel)
        }
    }

    fun build(
        context: Context,
        title: String,
        body: String,
        ongoing: Boolean,
        progress: Int?,
        incoming: Boolean = false,
        iconRes: Int? = null,
    ): Notification {
        // Tapping opens the app. One-shots (complete/failed/received) dismiss
        // themselves on tap; the ongoing service note stays put.
        val launch = Intent(context, MainActivity::class.java).apply {
            flags = Intent.FLAG_ACTIVITY_SINGLE_TOP or Intent.FLAG_ACTIVITY_NEW_TASK
        }
        val pi = PendingIntent.getActivity(
            context, 0, launch,
            PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT,
        )
        // iconRes overrides the default (used by the service to cycle animation
        // frames while a transfer is active); otherwise a static direction icon.
        val icon = iconRes ?: if (incoming) {
            android.R.drawable.stat_sys_download
        } else {
            android.R.drawable.stat_sys_upload
        }
        val builder = NotificationCompat.Builder(context, CHANNEL_ID)
            .setSmallIcon(icon)
            .setContentTitle(title)
            .setContentText(body)
            .setOngoing(ongoing)
            .setOnlyAlertOnce(true)
            .setPriority(NotificationCompat.PRIORITY_LOW)
            .setContentIntent(pi)
            .setAutoCancel(!ongoing)
        if (progress != null) {
            if (progress < 0) {
                // Indeterminate: an animated, moving progress bar (used while a
                // transfer is active).
                builder.setProgress(0, 0, true)
            } else {
                builder.setProgress(100, progress.coerceIn(0, 100), false)
            }
        }
        return builder.build()
    }

    fun show(context: Context, id: Int, notification: Notification) {
        try {
            NotificationManagerCompat.from(context).notify(id, notification)
        } catch (_: SecurityException) {
            // POST_NOTIFICATIONS not granted (Android 13+) — silently skip.
        }
    }

    fun cancel(context: Context, id: Int) {
        NotificationManagerCompat.from(context).cancel(id)
    }
}
