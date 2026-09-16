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

    /**
     * Arriving messages, kept apart from transfer status on purpose.
     *
     * Two reasons, and the second is the one that made this a defect rather
     * than a nicety. Transfers are IMPORTANCE_LOW with `setOnlyAlertOnce` —
     * correct for a progress bar that updates twenty times a minute, and
     * exactly wrong for a message, which arrives silently into the shade and
     * is noticed whenever the phone is next unlocked. And a channel is what
     * Android gives the *user* to decide with: on one channel, someone who
     * silences transfer noise silences their conversations with it, and there
     * is nothing they can do about it from either side.
     */
    const val CHAT_CHANNEL_ID = "peerbeam_chat"
    const val SERVICE_ID = 1

    fun ensureChannel(context: Context) {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            val manager = context.getSystemService(NotificationManager::class.java)
                ?: return
            manager.createNotificationChannel(
                NotificationChannel(
                    CHANNEL_ID,
                    "Transfers",
                    NotificationManager.IMPORTANCE_LOW,
                ).apply { description = "File transfer status" },
            )
            manager.createNotificationChannel(
                NotificationChannel(
                    CHAT_CHANNEL_ID,
                    "Messages",
                    NotificationManager.IMPORTANCE_HIGH,
                ).apply {
                    description = "Messages from your devices"
                    enableVibration(true)
                },
            )
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
        channelId: String = CHANNEL_ID,
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
        val chat = channelId == CHAT_CHANNEL_ID
        val builder = NotificationCompat.Builder(context, channelId)
            .setSmallIcon(icon)
            .setContentTitle(title)
            .setContentText(body)
            .setOngoing(ongoing)
            // A message may alert every time; a transfer that updates twenty
            // times a minute may not.
            .setOnlyAlertOnce(!chat)
            .setPriority(
                if (chat) NotificationCompat.PRIORITY_HIGH
                else NotificationCompat.PRIORITY_LOW,
            )
            .setCategory(
                if (chat) NotificationCompat.CATEGORY_MESSAGE
                else NotificationCompat.CATEGORY_PROGRESS,
            )
            .setContentIntent(pi)
            .setAutoCancel(!ongoing)
        if (chat) {
            // The body is the message, and a message is not one line. Without
            // this it is ellipsised at whatever the shade's width happens to
            // be, and the preview is already capped at 120 characters on the
            // Dart side (`kChatPreviewChars`).
            builder.setStyle(NotificationCompat.BigTextStyle().bigText(body))
            builder.setDefaults(NotificationCompat.DEFAULT_ALL)
        }
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
