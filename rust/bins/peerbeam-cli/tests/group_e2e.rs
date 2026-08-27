//! An invitation crossing a real wire, between two real processes.
//!
//! # Why this test exists
//!
//! Every layer of groups was tested in isolation and by construction, and the
//! receiving half **did not exist**: `send_foreign` was written, nothing ever
//! called `peerbeam_groups::apply`, and every invitation that arrived was
//! discarded without a trace. Nothing failed. The unit tests passed, the CLI
//! built, `group invite` reported success, and the invitation went nowhere.
//!
//! Only a round trip could catch that, which is the whole argument for having
//! one: an integration test is not a more thorough unit test, it is the only
//! thing that checks the wiring *between* the parts each unit test assumes.

use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use peerbeam_config::EngineConfig;
use serde_json::Value;

const BIN: &str = env!("CARGO_BIN_EXE_peerbeam");

/// A spawned peer that is killed and reaped when the test ends — including when
/// it ends by panicking.
///
/// `chat watch` runs until it is stopped, unlike `receive --once`, so a test
/// that only kills it on the success path leaves one behind on every failure.
/// Those accumulate, hold ports, and make the *next* run fail for a reason that
/// has nothing to do with the code under test — which is exactly what happened
/// while this test was being written.
struct Peer(std::process::Child);

impl Drop for Peer {
    fn drop(&mut self) {
        let _ = self.0.kill();
        // Reaped, not just signalled: an unwaited child stays a zombie holding
        // its pipe open, and `cargo test` will not exit while it does.
        let _ = self.0.wait();
    }
}

/// Two isolated configs, each with its own data directory.
///
/// Distinct `data_directory` is not tidiness: it gives each process its own
/// identity and trust store, as two real devices have. Sharing one would give
/// both the same keypair, and the handshake's directional keys — assigned by
/// comparing the two public keys — would collapse onto each other and fail key
/// confirmation. `transfer_e2e` records the same trap.
fn two_configs(dir: &std::path::Path) -> (std::path::PathBuf, std::path::PathBuf) {
    let mut a = EngineConfig::default();
    a.storage.data_directory = dir.join("data-a").to_string_lossy().into_owned();
    a.storage.save_directory = dir.join("recv-a").to_string_lossy().into_owned();
    let a_path = dir.join("a.json");
    a.save(&a_path).unwrap();

    let mut b = a.clone();
    b.storage.data_directory = dir.join("data-b").to_string_lossy().into_owned();
    b.storage.save_directory = dir.join("recv-b").to_string_lossy().into_owned();
    let b_path = dir.join("b.json");
    b.save(&b_path).unwrap();
    (a_path, b_path)
}

fn run(cfg: &std::path::Path, args: &[&str]) -> std::process::Output {
    let mut full = vec!["--config", cfg.to_str().unwrap(), "--no-color"];
    full.extend_from_slice(args);
    Command::new(BIN)
        .args(&full)
        .output()
        .expect("run peerbeam")
}

/// **The invitation must cross the wire and be held as an offer.**
///
/// Held, not joined: nothing may write a roster on B until B's own user accepts
/// (A2, condition 4). So the assertion is deliberately two-sided — the
/// invitation is there, and the group is *not*.
#[test]
fn an_invitation_reaches_the_other_device_and_waits_there() {
    let dir = tempfile::tempdir().unwrap();
    let (a_cfg, b_cfg) = two_configs(dir.path());

    // B listens. `chat watch` keeps a session host up, which is what a group
    // frame needs to arrive through — it rides the Chat channel.
    let mut b = Peer(
        Command::new(BIN)
        .args([
            "--config",
            b_cfg.to_str().unwrap(),
            "--no-color",
            // `--json` so the bound port arrives as a field rather than inside
            // a sentence: the human line reads "…on 0.0.0.0:51000 (Ctrl-C to
            // stop)", and parsing a port out of prose is the kind of test-only
            // cleverness that breaks the next time the copy improves.
            "--json",
            "chat",
            "watch",
            "--port",
            "0",
        ])
        .stdout(Stdio::piped())
        // Discarded rather than inherited: B is a long-running host that logs
        // while it waits, and piping that into the test runner's own stderr
        // buys nothing a failure message does not already say.
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn B"),
    );

    // Read B's stdout until it announces the port it bound. An OS-assigned
    // port, so two of these tests can run at once without colliding — the
    // fixed-port habit is what made `chat_ffi` flaky for weeks.
    let stdout = b.0.stdout.take().unwrap();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            let Ok(v) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            if v["event"] == "listening" {
                if let Some(port) = v["port"].as_u64() {
                    let _ = tx.send(port as u16);
                }
            }
        }
    });
    let port = rx
        .recv_timeout(Duration::from_secs(30))
        .expect("B should announce a listening port");

    // A makes a group and invites B directly. `--addr` rather than discovery:
    // this test must not depend on broadcasts reaching between two processes on
    // a build machine, and the flag exists for exactly this shape of peer.
    let created = run(&a_cfg, &["group", "create", "Trip"]);
    assert!(
        created.status.success(),
        "create failed: {}",
        String::from_utf8_lossy(&created.stderr)
    );
    let invited = run(
        &a_cfg,
        &[
            "group",
            "invite",
            "Trip",
            "--addr",
            &format!("127.0.0.1:{port}"),
        ],
    );
    assert!(
        invited.status.success(),
        "invite failed: {}\n{}",
        String::from_utf8_lossy(&invited.stdout),
        String::from_utf8_lossy(&invited.stderr)
    );

    // B should now be holding an offer. Polled rather than slept on: delivery
    // is a round trip through a handshake, and a fixed sleep would either be
    // too short on a loaded machine or waste time on an idle one.
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut invites: Vec<Value> = Vec::new();
    while Instant::now() < deadline {
        let listed = run(&b_cfg, &["--json", "group", "list"]);
        invites = String::from_utf8_lossy(&listed.stdout)
            .lines()
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
            .filter(|v| v["event"] == "group_invite")
            .collect();
        if !invites.is_empty() {
            break;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    assert_eq!(
        invites.len(),
        1,
        "the invitation did not arrive — the receiving half of groups is not wired"
    );
    assert_eq!(invites[0]["name"], "Trip");

    // And it is an offer, not a membership: B is in no group until B accepts.
    let groups = run(&b_cfg, &["--json", "group", "list"]);
    let joined: Vec<Value> = String::from_utf8_lossy(&groups.stdout)
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter(|v| v["event"] == "group")
        .collect();
    assert!(
        joined.is_empty(),
        "receiving an invitation joined the group without the user accepting"
    );
}
