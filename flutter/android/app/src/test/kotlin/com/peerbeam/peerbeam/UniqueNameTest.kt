package com.peerbeam.peerbeam

import androidx.documentfile.provider.DocumentFile
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.rules.TemporaryFolder
import java.io.File

/// [uniqueName] is the Android side of the engine's no-clobber promise, so
/// these assertions are about one thing: a peer-supplied file name must never
/// resolve to a name that is already taken.
class UniqueNameTest {
    @get:Rule
    val tmp = TemporaryFolder()

    /// Names a set of files claims, as [uniqueName]'s `exists` predicate.
    private fun taken(vararg names: String): (String) -> Boolean = names.toSet()::contains

    @Test
    fun `keeps the requested name when nothing holds it`() {
        assertEquals("taxes.pdf", uniqueName("taxes.pdf", taken()))
        assertEquals("taxes.pdf", uniqueName("taxes.pdf", taken("notes.pdf", "taxes.txt")))
    }

    @Test
    fun `suffixes before the extension when the name is taken`() {
        assertEquals("taxes (1).pdf", uniqueName("taxes.pdf", taken("taxes.pdf")))
    }

    @Test
    fun `increments past every variant already taken`() {
        assertEquals(
            "taxes (3).pdf",
            uniqueName("taxes.pdf", taken("taxes.pdf", "taxes (1).pdf", "taxes (2).pdf")),
        )
    }

    /// A gap is filled rather than skipped past — same as `unique_path`, which
    /// reserves the first candidate it can and does not track a high-water mark.
    @Test
    fun `takes the first free variant even when a later one is gone`() {
        assertEquals(
            "taxes (2).pdf",
            uniqueName("taxes.pdf", taken("taxes.pdf", "taxes (1).pdf", "taxes (3).pdf")),
        )
    }

    /// The table this and `unique_path` must agree on. Kept as one test so a
    /// change to either side's splitting rule fails as the parity break it is,
    /// rather than as an unrelated-looking edge case.
    @Test
    fun `splits names exactly as rust file_stem and extension do`() {
        // Last dot wins: `archive.tar.gz` is stem `archive.tar`, ext `gz`.
        assertEquals("archive.tar (1).gz", uniqueName("archive.tar.gz", taken("archive.tar.gz")))
        // No dot at all: no extension to insert before.
        assertEquals("README (1)", uniqueName("README", taken("README")))
        // A leading dot is part of the name, NOT an extension separator —
        // `Path::new(".gitignore").extension()` is None.
        assertEquals(".gitignore (1)", uniqueName(".gitignore", taken(".gitignore")))
        // ...but a dotfile with a real extension splits normally.
        assertEquals(".hidden (1).txt", uniqueName(".hidden.txt", taken(".hidden.txt")))
        // A trailing dot is an empty extension, and stays one.
        assertEquals("file (1).", uniqueName("file.", taken("file.")))
    }

    /// The failure that started this: the SAF publish path used to delete the
    /// colliding document before creating its own. Driven through a real
    /// [DocumentFile] directory rather than a stub set, so the predicate the
    /// production call sites pass — `dir.findFile(it) != null` — is the one
    /// under test, and the user's file is there to be destroyed if it isn't.
    @Test
    fun `never names a document the user already has in the directory`() {
        val dir = tmp.newFolder("Download")
        File(dir, "taxes.pdf").writeText("the user's own tax return")
        val doc = DocumentFile.fromFile(dir)

        val free = uniqueName("taxes.pdf") { doc.findFile(it) != null }

        assertEquals("taxes (1).pdf", free)
        assertTrue("the user's file must still be there", File(dir, "taxes.pdf").exists())
        assertEquals("the user's own tax return", File(dir, "taxes.pdf").readText())
    }
}
