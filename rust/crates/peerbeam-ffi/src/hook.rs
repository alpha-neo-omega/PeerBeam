//! The receive hook: run a program of the user's choosing after a file lands.
//!
//! # What makes this safe to have at all
//!
//! Running a program because someone sent a file is a large amount of trust.
//! Three rules keep it trust in *this machine's configuration* rather than in
//! the peer:
//!
//! * **Configured locally, never negotiated.** Nothing a sender controls
//!   decides whether a hook runs or which program it is.
//! * **Executed directly, never through a shell.** The received path is one
//!   argument, so a file named `; rm -rf ~` is an argument and not a command.
//!   That is also why the setting is a single program rather than a command
//!   line: supporting `a && b` would mean invoking a shell, and a shell is
//!   precisely what must not see a peer-supplied name.
//! * **Fire and forget, and never fatal.** A hook that fails, hangs or does not
//!   exist must not affect the transfer that already completed — the file is on
//!   disk either way, and failing the receive would be a worse outcome than a
//!   hook that did nothing.

/// Run the configured hook for a received file, if there is one.
///
/// `path` is where the file landed; `peer` is the authenticated sender's device
/// id. Both are passed as separate arguments, never interpolated into a string.
/// Returns the spawned child, which production callers **ignore** — the hook is
/// fire-and-forget, and nothing waits on it. It is returned only so a test can
/// wait for the process it started instead of polling the filesystem and hoping:
/// a test that sleeps is a test that fails under load for reasons unrelated to
/// what it checks, which this one did.
pub fn run(hook: &str, path: &str, peer: &str) -> Option<std::process::Child> {
    if hook.trim().is_empty() {
        return None;
    }
    // `spawn`, not `output`: a hook that blocks forever must not hold the
    // receive path open behind it.
    match std::process::Command::new(hook).arg(path).arg(peer).spawn() {
        Ok(child) => {
            tracing::debug!(hook, path, "receive hook started");
            Some(child)
        }
        Err(e) => {
            tracing::warn!(error = %e, hook, "receive hook could not be started");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_hook_is_a_no_op() {
        // The default. Nothing configured means nothing runs, and calling it is
        // not an error.
        assert!(run("", "/tmp/x", "pb-bob").is_none());
        assert!(run("   ", "/tmp/x", "pb-bob").is_none());
    }

    #[test]
    fn a_missing_program_does_not_panic_or_fail_the_receive() {
        // The file is already on disk. A hook that cannot start is a warning,
        // never a failure — failing the receive would be the worse outcome.
        assert!(run("/definitely/not/a/program", "/tmp/x", "pb-bob").is_none());
    }

    /// **The argument is an argument.** A file named like a shell command must
    /// reach the hook as one string, because nothing here goes through a shell.
    #[cfg(unix)]
    #[test]
    fn a_hostile_file_name_is_passed_as_one_argument() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("args.txt");
        let script = dir.path().join("hook.sh");
        std::fs::write(
            &script,
            format!("#!/bin/sh\nprintf '%s' \"$1\" > {}\n", out.display()),
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

        let hostile = "; rm -rf ~ #.txt";
        let mut child = run(script.to_str().unwrap(), hostile, "pb-bob").expect("spawned");
        // **Waited on, not polled.** An earlier version slept in a loop for the
        // file to appear; under a full-workspace run a freshly spawned shell can
        // take far longer than it does alone, so the test failed intermittently
        // for reasons that had nothing to do with argument passing. Waiting is
        // exact at any load.
        let status = child.wait().expect("the hook could not be waited on");
        assert!(status.success(), "the hook script itself failed: {status}");

        let seen = std::fs::read_to_string(&out).unwrap_or_default();
        assert_eq!(
            seen, hostile,
            "the file name was split or interpreted instead of passed whole"
        );
    }
}
