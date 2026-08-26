package com.peerbeam.peerbeam

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertTrue
import org.junit.Test

/// Android 15+ does not merely *suggest* that a timed-out `dataSync` service
/// stop itself — it raises `ForegroundServiceDidNotStopInTimeException` against
/// the app that fails to. These assertions are about that: the service always
/// stops, and it never disappears without telling the user why.
class ForegroundTimeoutTest {
    /// Records what [handleForegroundTimeout] did, in order.
    private class RecordingOps : TimeoutOps {
        val calls = mutableListOf<String>()
        var posted: TimeoutNotice? = null

        override fun notice(notice: TimeoutNotice) {
            calls += "notice"
            posted = notice
        }

        override fun announceStopped() {
            calls += "announce"
        }

        override fun stop() {
            calls += "stop"
        }
    }

    /// **The app has to learn the service died, not just the user.** Android
    /// stops a `dataSync` foreground service at its six-hour cap; the
    /// notification told the user, and Dart went on believing the service was
    /// running — so nothing ever restarted it and the device was quietly
    /// unreachable until somebody reopened the app.
    ///
    /// Announced *before* the stop for the reason the notice already is: past
    /// `stopSelf` the process is on its way out, and an event emitted then can
    /// lose the race with teardown.
    @Test
    fun `tells the app it stopped, before stopping`() {
        for (activeTransfer in listOf(true, false)) {
            val ops = RecordingOps()
            handleForegroundTimeout(activeTransfer, ops)
            assertTrue(
                "the app was never told the platform stopped the service",
                ops.calls.contains("announce"),
            )
            assertTrue(
                "announced after the stop, where it can be lost to teardown",
                ops.calls.indexOf("announce") < ops.calls.indexOf("stop"),
            )
        }
    }

    @Test
    fun `always stops the service, whatever it was doing`() {
        for (activeTransfer in listOf(true, false)) {
            val ops = RecordingOps()
            handleForegroundTimeout(activeTransfer, ops)
            assertTrue(
                "the framework kills the process if we don't stop",
                ops.calls.contains("stop"),
            )
        }
    }

    /// The notice has to be posted while this is still a live service: after
    /// `stopSelf` the Context is on its way out.
    @Test
    fun `explains itself before it stops`() {
        val ops = RecordingOps()
        handleForegroundTimeout(false, ops)
        // Both the telling and the announcing happen while this is still a live
        // service: past `stopSelf` the process is on its way to `onDestroy`, so
        // anything posted then can be lost to teardown.
        assertEquals(listOf("notice", "announce", "stop"), ops.calls)
    }

    @Test
    fun `tells the user what they lost and how to get it back`() {
        val interrupted = timeoutNotice(activeTransfer = true)
        val idle = timeoutNotice(activeTransfer = false)

        // A dropped transfer and a device that quietly stopped being reachable
        // are different losses; one line covering both would explain neither.
        assertNotEquals(idle.title, interrupted.title)

        // Opening the app is what actually restores the allowance, so both
        // notices have to say so — a notification that only reports a failure
        // leaves the user with no move to make.
        assertTrue(interrupted.body, interrupted.body.contains("Open PeerBeam"))
        assertTrue(idle.body, idle.body.contains("Open PeerBeam"))
    }

    /// A one-shot notice posted under the service's own id would be removed
    /// along with the service moments later, i.e. never seen.
    @Test
    fun `notice does not reuse the ongoing service notification id`() {
        assertNotEquals(Notifications.SERVICE_ID, TIMEOUT_NOTICE_ID)
    }

    /// The policy above is only reached if the service actually overrides the
    /// callback Android invokes. Nothing else in this suite can catch its
    /// removal, and on-device the symptom is a crash six hours in.
    @Test
    fun `PeerBeamService overrides the Android 15 timeout callback`() {
        assertNotNull(
            PeerBeamService::class.java.getDeclaredMethod(
                "onTimeout",
                Int::class.java,
                Int::class.java,
            ),
        )
    }
}

