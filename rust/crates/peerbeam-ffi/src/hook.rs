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
pub fn run(hook: &str, path: &str, peer: &str) {
    if hook.trim().is_empty() {
        return;
    }
    // `spawn`, not `output`: a hook that blocks forever must not hold the
    // receive path open behind it.
    match std::process::Command::new(hook).arg(path).arg(peer).spawn() {
        Ok(_) => tracing::debug!(hook, path, "receive hook started"),
        Err(e) => tracing::warn!(error = %e, hook, "receive hook could not be started"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_hook_is_a_no_op() {
        // The default. Nothing configured means nothing runs, and calling it is
        // not an error.
        run("", "/tmp/x", "pb-bob");
        run("   ", "/tmp/x", "pb-bob");
    }

    #[test]
    fn a_missing_program_does_not_panic_or_fail_the_receive() {
        // The file is already on disk. A hook that cannot start is a warning,
        // never a failure — failing the receive would be the worse outcome.
        run("/definitely/not/a/program", "/tmp/x", "pb-bob");
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
        run(script.to_str().unwrap(), hostile, "pb-bob");

        // The hook runs detached, so this waits for it. Generously: under a
        // full parallel test run a freshly spawned shell can take far longer
        // than it does alone, and a tight window here made the test fail for
        // reasons that had nothing to do with what it checks.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while std::time::Instant::now() < deadline {
            if out.exists() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(
            out.exists(),
            "the hook never ran — this test proves nothing if it times out"
        );
        let seen = std::fs::read_to_string(&out).unwrap_or_default();
        assert_eq!(
            seen, hostile,
            "the file name was split or interpreted instead of passed whole"
        );
    }
}
