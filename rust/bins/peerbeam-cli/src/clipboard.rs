//! The CLI's side of clipboard sync: acknowledging a clip that arrives.
//!
//! **The CLI gains no auto-sync.** Watching a clipboard means reading one, and
//! there is no system-clipboard adapter in this workspace — that is why the
//! watcher lives in the Flutter surface (`docs/CLI.md`). `peerbeam send
//! --clipboard` is unchanged and remains the CLI's manual path.
//!
//! What the CLI *does* do is comprehend an inbound `Clip`, because it
//! advertises `CLIPBOARD_FEAT_CLIP` exactly as the FFI does. Advertising a bit
//! this build then dropped on the floor would make the advertisement a lie, and
//! a peer's behaviour would depend on which of our two frontends it reached —
//! the bug 2a shipped with `CHAT_FEAT_FILEREF`.

use std::sync::Arc;

use peerbeam_clipboard::{Clip, ClipboardSink};
use peerbeam_domain::id::DeviceId;

/// Report an arriving clip: **who sent it and how big it was, never what it
/// said.**
///
/// Not printing the text is the whole point. A clip is not a chat message: it
/// is whatever the user last copied, captured automatically, and this build
/// cannot tell a shopping list from a password (see `peerbeam_clipboard`'s
/// crate docs — nothing can). Printing it would paste that into terminal
/// scrollback, `script` captures, CI logs and anyone's `tee`, which is a worse
/// exposure than the network hop it just took. "Never log sensitive data"
/// applies most sharply to the one buffer guaranteed to hold some.
///
/// It goes to **stderr**, so a clip arriving mid-command can never interleave
/// into `--json` output on stdout and break a script parsing it.
#[must_use]
pub(crate) fn notice(peer: &DeviceId, clip: &Clip) -> String {
    format!(
        "clipboard received from {} ({} bytes) — the CLI does not apply it to a \
         system clipboard",
        peer.0,
        clip.text.len()
    )
}

/// The sink every CLI session's `ClipboardHandler` is built with.
#[must_use]
pub(crate) fn sink() -> ClipboardSink {
    Arc::new(|peer: DeviceId, clip: Clip| {
        eprintln!("{}", notice(&peer, &clip));
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clip(text: &str) -> Clip {
        Clip {
            text: text.to_string(),
            sent_at: "2026-08-17T10:00:00Z".into(),
        }
    }

    /// The load-bearing test of this module: the notice names the sender and
    /// the size, and **never reproduces the clip**. A "helpful" change that
    /// added a preview would write secrets into every terminal scrollback and
    /// log capture on the machine.
    #[test]
    fn the_notice_never_reproduces_the_clip_text() {
        const SECRET: &str = "hunter2-correct-horse-battery-staple";
        let line = notice(&DeviceId::from("pb-bob"), &clip(SECRET));
        assert!(
            !line.contains(SECRET),
            "the clip's text reached the terminal: {line}"
        );
        assert!(line.contains("pb-bob"), "the sender must be named: {line}");
        assert!(
            line.contains(&SECRET.len().to_string()),
            "the size is reported: {line}"
        );
    }

    /// Not even a fragment: a substring preview is the obvious "improvement"
    /// and is just as leaky for a short secret, which is what a PIN or a
    /// one-time code looks like.
    #[test]
    fn no_fragment_of_a_short_secret_leaks_either() {
        let line = notice(&DeviceId::from("pb-bob"), &clip("824913"));
        assert!(!line.contains("824913"), "a short secret leaked: {line}");
    }
}
