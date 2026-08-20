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
/// Returns the reaper's handle, or `None` when no hook is configured.
///
/// Production **drops** it, which detaches the thread and lets it finish
/// reaping. A test can `join()` it instead and know the hook has exited — which
/// is exact at any load, unlike polling for a side effect.
pub fn run(hook: &str, path: &str, peer: &str) -> Option<std::thread::JoinHandle<()>> {
    if hook.trim().is_empty() {
        return None;
    }
    // `spawn`, not `output`: a hook that blocks forever must not hold the
    // receive path open behind it.
    match std::process::Command::new(hook).arg(path).arg(peer).spawn() {
        Ok(mut child) => {
            tracing::debug!(hook, path, "receive hook started");
            // **Reaped on a detached thread, not dropped.**
            //
            // Dropping a `Child` on Unix does not wait for it, so the process
            // stays a zombie in the table until this one exits. A long-running
            // daemon receiving files all day accumulates one per transfer and
            // can eventually exhaust the process table — a slow failure that
            // looks like nothing to do with PeerBeam.
            //
            // A thread rather than an async task because `wait` is blocking and
            // this function is called from a sync path; the thread costs one
            // stack for the hook's lifetime and needs no runtime.
            Some(std::thread::spawn(move || match child.wait() {
                Ok(status) if status.success() => {}
                Ok(status) => {
                    tracing::warn!(?status, "receive hook exited unsuccessfully");
                }
                Err(e) => tracing::warn!(error = %e, "receive hook could not be waited on"),
            }))
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
        let reaper = run(script.to_str().unwrap(), hostile, "pb-bob").expect("spawned");
        // **Waited on, not polled.** An earlier version slept in a loop for the
        // file to appear; under a full-workspace run a freshly spawned shell can
        // take far longer than it does alone, so the test failed intermittently
        // for reasons that had nothing to do with argument passing. Waiting is
        // exact at any load.
        // Joining the reaper is the wait: it returns once the child has been
        // collected, so this is exact at any load rather than a poll for the
        // file to appear.
        reaper.join().expect("the reaper thread panicked");

        let seen = std::fs::read_to_string(&out).unwrap_or_default();
        assert_eq!(
            seen, hostile,
            "the file name was split or interpreted instead of passed whole"
        );
    }
}

#[cfg(test)]
mod reap_tests {
    /// **A spawned hook must not be left a zombie.**
    ///
    /// Dropping a `Child` on Unix does not wait for it, so the process stays in
    /// the table until this one exits. A daemon receiving files all day
    /// accumulated one per transfer.
    ///
    /// Asserted by counting this process's own children: `/proc/self/task/*/children`
    /// is Linux-specific, so the test is gated — but the leak was too.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_finished_hook_leaves_no_zombie() {
        fn child_count() -> usize {
            glob_children().split_whitespace().count()
        }
        fn glob_children() -> String {
            let mut all = String::new();
            if let Ok(tasks) = std::fs::read_dir("/proc/self/task") {
                for t in tasks.flatten() {
                    if let Ok(s) = std::fs::read_to_string(t.path().join("children")) {
                        all.push_str(&s);
                        all.push(' ');
                    }
                }
            }
            all
        }

        let before = child_count();
        // `true` exits immediately, so a dropped handle would be a zombie for
        // the rest of this process's life.
        let reaper = super::run("/bin/true", "/tmp/x", "pb-bob").expect("spawned");
        reaper.join().expect("reaper panicked");

        // The reaper is a thread; give it a bounded chance to run rather than a
        // fixed sleep that would either flake or waste time.
        for _ in 0..200 {
            if child_count() <= before {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        panic!(
            "the hook process was never reaped: {} → {}",
            before,
            child_count()
        );
    }

    #[test]
    fn no_hook_configured_starts_nothing() {
        assert!(super::run("", "/tmp/x", "pb-bob").is_none());
        assert!(super::run("   ", "/tmp/x", "pb-bob").is_none());
    }
}
