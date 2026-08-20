package com.peerbeam.peerbeam

/// The name a file should be written under in a destination directory: [name]
/// itself when nothing there holds it, else the first free ` (n)` variant
/// inserted before the extension — `taxes (1).pdf`, `taxes (2).pdf`, …
///
/// **This must stay behaviourally identical to `unique_path` in
/// `rust/crates/peerbeam-storage-fs/src/lib.rs`.** That function is the
/// engine's promise that receiving a file never destroys one the user already
/// has, and it is enforced against **app storage** — where the engine writes.
/// On Android a received file then gets published a second time, into the
/// destination the user actually chose, and the engine never sees that
/// directory's collisions. Without the same rule here, a peer that sends
/// `taxes.pdf` deletes the user's own `taxes.pdf`: no collision in app storage
/// means no suffix, and the publish overwrites. Two directories, one promise —
/// if `unique_path`'s naming ever changes, this changes with it, or the same
/// received file gets a different name depending on which platform got it.
///
/// The split mirrors Rust's `Path::file_stem`/`Path::extension`, including the
/// case that is easy to get wrong: a name whose only dot is a leading one has
/// **no** extension, so `.gitignore` becomes `.gitignore (1)` and never
/// `. (1).gitignore`. `.` and `..` need no handling — the engine reduces every
/// sender-supplied name to one path component through `sanitize_file_name`,
/// which maps exactly those to `received.bin`.
///
/// [exists] is injected rather than read from a directory in here because the
/// two destinations answer it in completely different ways — a SAF tree by
/// `DocumentFile.findFile`, public Downloads by a MediaStore query — and
/// neither is needed to test the naming rule itself.
internal fun uniqueName(name: String, exists: (String) -> Boolean): String {
    if (!exists(name)) return name
    // `> 0`, not `>= 0`: index 0 is the leading-dot case above.
    val dot = name.lastIndexOf('.')
    val stem = if (dot > 0) name.substring(0, dot) else name
    val ext = if (dot > 0) name.substring(dot + 1) else null
    var n = 1
    while (true) {
        val candidate = if (ext == null) "$stem ($n)" else "$stem ($n).$ext"
        if (!exists(candidate)) return candidate
        n++
    }
}
