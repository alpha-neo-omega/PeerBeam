//! `peerbeam download-update`'s exit codes, driven through the real binary.
//!
//! For a script, the exit code is the whole contract. `download-update &&
//! install` must not reach the install unless a verified file was written, so
//! the codes are pinned here end to end rather than inferred from the mapping.
//!
//! **Nothing here reaches the internet.** Every request the binary makes is
//! sent to a local black hole named in `HTTPS_PROXY`, which the HTTP client
//! honours on every platform. The black hole accepts each connection, counts
//! it, and hangs up, which is what being offline looks like to the command. The
//! count is what makes the tests trustworthy: it proves the request really went
//! into the hole instead of out to the network.
//!
//! What cannot be reached offline, such as a signature that fails to verify or
//! a download that succeeds, is pinned by the unit tests in `src/update.rs`.
//! Those cover every way the command can end.

use std::net::TcpListener;
use std::path::Path;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

const BIN: &str = env!("CARGO_BIN_EXE_peerbeam");

/// A proxy that is never any use: it accepts, counts, and hangs up.
struct BlackHole {
    url: String,
    hits: Arc<AtomicUsize>,
}

impl BlackHole {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind the black hole");
        let url = format!("http://{}", listener.local_addr().expect("its address"));
        let hits = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&hits);
        // Never joined. It blocks in `accept` for the life of the test binary,
        // which exits when the tests do.
        std::thread::spawn(move || {
            for conn in listener.incoming() {
                counter.fetch_add(1, Ordering::SeqCst);
                drop(conn);
            }
        });
        BlackHole { url, hits }
    }

    /// Connections made so far.
    fn hits(&self) -> usize {
        self.hits.load(Ordering::SeqCst)
    }
}

/// `peerbeam download-update --to <to>`, with every request sent into `hole`.
fn download_update(hole: &BlackHole, to: &Path) -> Command {
    let mut cmd = Command::new(BIN);
    cmd.arg("download-update").arg("--to").arg(to);
    // Both spellings, and the catch-all, so that a proxy already set in the
    // environment running the tests cannot route around the black hole. The
    // same goes for an exemption list.
    for var in ["HTTPS_PROXY", "https_proxy", "ALL_PROXY", "all_proxy"] {
        cmd.env(var, &hole.url);
    }
    for var in ["NO_PROXY", "no_proxy"] {
        cmd.env_remove(var);
    }
    cmd
}

fn describe(out: &Output) -> String {
    format!(
        "exit {:?}\nstdout: {}\nstderr: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

/// The one `update_download` event a `--json` run promises, and nothing else on
/// stdout.
fn the_event(out: &Output) -> serde_json::Value {
    let stdout = String::from_utf8_lossy(&out.stdout);
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(
        lines.len(),
        1,
        "expected exactly one event\n{}",
        describe(out)
    );
    let event: serde_json::Value = serde_json::from_str(lines[0])
        .unwrap_or_else(|e| panic!("not JSON ({e})\n{}", describe(out)));
    assert_eq!(event["event"], "update_download", "{}", describe(out));
    event
}

/// **Offline is exit 4, and it is not exit 0.** A script that wanted a file did
/// not get one, and `download-update && install` must stop there.
///
/// It is also not exit 5. A machine with no route out is not under attack, and
/// a script has to be able to tell the two apart (A4 condition 4).
///
/// `APPIMAGE` is set so that a Linux run gets as far as the network. Without
/// it, a binary in a cargo target directory has no package format and is
/// refused before any request is made, which is the next test.
#[test]
fn an_unreachable_release_feed_exits_4_and_writes_nothing() {
    for json in [true, false] {
        let hole = BlackHole::start();
        let dir = tempfile::tempdir().expect("tempdir");
        let to = dir.path().join("updates");

        let mut cmd = download_update(&hole, &to);
        cmd.env("APPIMAGE", dir.path().join("PeerBeam.AppImage"));
        if json {
            cmd.arg("--json");
        }
        let out = cmd.output().expect("run peerbeam");

        assert!(
            hole.hits() >= 1,
            "no request reached the black hole, so this did not test being offline\n{}",
            describe(&out)
        );
        assert_eq!(
            out.status.code(),
            Some(4),
            "json={json}\n{}",
            describe(&out)
        );
        assert!(
            !to.exists(),
            "a download that never started must not even create --to"
        );
        if json {
            let event = the_event(&out);
            assert_eq!(event["downloaded"], false, "{}", describe(&out));
            assert_eq!(event["ok"], false, "{}", describe(&out));
        } else {
            assert!(
                !out.stderr.is_empty(),
                "a failure must say so, not only exit non-zero\n{}",
                describe(&out)
            );
        }
    }
}

/// **A machine this command can never fetch for exits 8, and asks nothing
/// first.**
///
/// The test binary sits in a cargo target directory that neither dpkg nor rpm
/// owns, and `APPIMAGE` is removed. To the resolver that is a build from
/// source, which it refuses rather than guess a package format for. The refusal
/// has to come before the release check. There is no answer the check could
/// give that would let this machine download anything, so asking would disclose
/// a request for no purpose. It would also make the exit code depend on the
/// day: 0 while the build is current, a refusal once it is not.
///
/// A zero count only means something if requests would have been counted. The
/// test above proves that they are.
#[cfg(target_os = "linux")]
#[test]
fn a_machine_with_no_artifact_exits_8_without_asking_anything() {
    let hole = BlackHole::start();
    let dir = tempfile::tempdir().expect("tempdir");
    let to = dir.path().join("updates");

    let out = download_update(&hole, &to)
        .arg("--json")
        .env_remove("APPIMAGE")
        .output()
        .expect("run peerbeam");

    assert_eq!(out.status.code(), Some(8), "{}", describe(&out));
    assert_eq!(
        hole.hits(),
        0,
        "refused, but only after a request it could never have used\n{}",
        describe(&out)
    );
    assert!(!to.exists(), "a refused download must not create --to");
    let event = the_event(&out);
    assert_eq!(event["downloaded"], false, "{}", describe(&out));
    assert_eq!(event["ok"], false, "{}", describe(&out));
}
