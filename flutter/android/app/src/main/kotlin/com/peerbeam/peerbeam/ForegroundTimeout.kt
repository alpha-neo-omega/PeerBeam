package com.peerbeam.peerbeam

/// Notification id for the "why did this stop?" notice below. Deliberately not
/// [Notifications.SERVICE_ID]: the service's own notification is torn down with
/// the service, so reusing its id would post the explanation and then remove it
/// again a moment later.
internal const val TIMEOUT_NOTICE_ID = 2

/// What the user is told when the foreground-service allowance runs out.
internal data class TimeoutNotice(val title: String, val body: String)

/// The platform half of [handleForegroundTimeout], injected rather than reached
/// for so the policy is assertable in a plain JVM unit test — `Service` is an
/// `android.jar` stub off-device and throws on every call. Same reasoning as
/// `uniqueName`'s `exists` predicate: the decision is worth testing, the
/// framework plumbing around it is not.
internal interface TimeoutOps {
    /// Post [notice] under [TIMEOUT_NOTICE_ID] — dismissible, one-shot.
    fun notice(notice: TimeoutNotice)

    /// Drop the ongoing notification and the Wi-Fi/CPU locks, then `stopSelf`.
    fun stop()

    /// Tell the app the platform stopped the service, so Dart stops believing
    /// it is running.
    ///
    /// Without this the notification says background receiving stopped and the
    /// app's own state still says it is on — so nothing ever restarts it, and
    /// the device is quietly unreachable until someone happens to reopen the
    /// app and look. A notice the user must read and act on is not the same as
    /// the app knowing.
    fun announceStopped()
}

/// Android's answer to a `dataSync` foreground service that has spent its
/// allowance: 6 cumulative hours in any 24 for an app targeting SDK 35+ (this
/// one targets 36), after which the framework calls `Service.onTimeout` and
/// gives us seconds to get out of the way. Missing that window is not a
/// degraded experience, it is a crash — the framework raises
/// `ForegroundServiceDidNotStopInTimeException` against our own process — so
/// [TimeoutOps.stop] is the one thing here that must always happen.
///
/// The notice is not decoration. `backgroundReceive` defaults on, which makes
/// this the ordinary end of a long idle day rather than an edge case, and
/// without it the whole event is invisible: the ongoing notification vanishes
/// overnight, the device silently stops being reachable, and nothing on screen
/// connects the two. Android restores the allowance when PeerBeam is next
/// brought to the foreground, so "open the app" is a real fix and not an
/// apology.
internal fun handleForegroundTimeout(activeTransfer: Boolean, ops: TimeoutOps) {
    // Posted before stopping: past `stopSelf` this service is on its way to
    // `onDestroy` and is no longer a Context worth posting from.
    ops.notice(timeoutNotice(activeTransfer))
    // Announced before stopping too, and for the same reason — the process may
    // be on its way out, and an event emitted after `stopSelf` can lose the race
    // with teardown. Dart hearing this late is the difference between the app
    // restarting the service and never knowing it was gone.
    ops.announceStopped()
    ops.stop()
}

/// The wording for [handleForegroundTimeout]. An interrupted transfer and an
/// idle receive-ready service fail the user in different ways — one lost work
/// mid-flight (resumable, because the engine checkpoints it), the other lost
/// reachability — so they say different things instead of sharing one line
/// vague enough to cover both.
internal fun timeoutNotice(activeTransfer: Boolean): TimeoutNotice = if (activeTransfer) {
    TimeoutNotice(
        "Transfer paused",
        "Android limits background transfers to 6 hours a day. Open PeerBeam to resume.",
    )
} else {
    TimeoutNotice(
        "Background receiving stopped",
        "Android limits background activity to 6 hours a day. Open PeerBeam to receive again.",
    )
}
