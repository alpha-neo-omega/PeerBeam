package com.peerbeam.peerbeam

import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertSame
import org.junit.Assert.assertTrue
import org.junit.Test

/// [PendingLaunch] exists because resolving a launching share moved off the
/// main thread, which put the answer and the question in a race. These
/// assertions are about the two ways that race can go, and about the one thing
/// neither is allowed to do: leave Dart's startup waiting, or tell it there was
/// no share when there was one.
class PendingLaunchTest {
    /// Records what an `initialIntent` call was answered with, and whether it
    /// was answered at all.
    private class Asker {
        var answered = false
        var event: Map<String, Any?>? = null

        val reply: (Map<String, Any?>?) -> Unit = {
            answered = true
            event = it
        }
    }

    private val share = mapOf<String, Any?>(
        "event" to "share",
        "paths" to listOf("/data/cache/shared/1/0_photo.jpg"),
        "names" to listOf("photo.jpg"),
    )

    /// The copy won the race: the event is already here when Dart asks.
    @Test
    fun `answers a request that arrives after the resolve`() {
        val launch = PendingLaunch()
        launch.resolving()
        launch.deliver(share)

        val asker = Asker()
        launch.request(asker.reply)

        assertTrue(asker.answered)
        assertSame(share, asker.event)
    }

    /// The launch intent is consumed by the request that gets it — a second
    /// ask (a re-attached engine, say) must not stage the same files twice.
    @Test
    fun `hands the launch intent over exactly once`() {
        val launch = PendingLaunch()
        launch.resolving()
        launch.deliver(share)
        launch.request(Asker().reply)

        val second = Asker()
        launch.request(second.reply)

        assertTrue(second.answered)
        assertNull(second.event)
    }

    /// The failure this class was extracted for: Dart asks once and reads null
    /// as "nothing was shared", so answering while the copy is still running
    /// throws the share away.
    @Test
    fun `parks a request that arrives mid-resolve`() {
        val launch = PendingLaunch()
        launch.resolving()

        val asker = Asker()
        launch.request(asker.reply)
        assertFalse("answering now would lose the share", asker.answered)

        launch.deliver(share)

        assertTrue(asker.answered)
        assertSame(share, asker.event)
    }

    /// A resolve that produced nothing — an intent carrying no share, or a
    /// copy that blew up — still has to answer, or `initialIntent`'s future
    /// never completes and startup stops there.
    @Test
    fun `answers a parked request even when the resolve came back empty`() {
        val launch = PendingLaunch()
        launch.resolving()

        val asker = Asker()
        launch.request(asker.reply)
        launch.deliver(null)

        assertTrue(asker.answered)
        assertNull(asker.event)
    }

    /// Same rule the pickers follow for their own held replies: a request that
    /// is displaced is answered as it goes, never dropped.
    @Test
    fun `answers a displaced request rather than dropping it`() {
        val launch = PendingLaunch()
        launch.resolving()

        val first = Asker()
        val second = Asker()
        launch.request(first.reply)
        launch.request(second.reply)

        assertTrue("a reply that is never sent hangs its Dart future", first.answered)
        assertNull(first.event)

        launch.deliver(share)
        assertSame(share, second.event)
    }

    /// Nothing to copy: [resolveIntent] delivers inline, before any request can
    /// arrive, and the mark set just before it has to be cleared by that
    /// delivery — otherwise every later request would park forever.
    @Test
    fun `an inline delivery clears the resolving mark`() {
        val launch = PendingLaunch()
        launch.resolving()
        launch.deliver(null) // a launch that carried no share at all

        val asker = Asker()
        launch.request(asker.reply)

        assertTrue(asker.answered)
        assertNull(asker.event)
    }
}
