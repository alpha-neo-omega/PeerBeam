//! End-to-end `peerbeam pipe`: two real processes, a real QUIC connection, and
//! a payload that is deliberately not text.
//!
//! Four things are checked here that no in-process test can check:
//!
//! * **stdout really is only the payload.** The listener's stdout is captured
//!   into a buffer and compared byte-for-byte with what was piped in — so a
//!   single stray status line, progress bar or `--json` event would fail the
//!   comparison rather than merely look untidy. This is `peerbeam pipe --listen
//!   > project.tgz` in test form.
//! * **The listen gate holds across a process boundary.** A running `peerbeam
//!   receive` is offered a pipe and refuses it, with nothing of the payload
//!   reaching its output.
//! * **A pin is not approval**, likewise across processes: a first-contact
//!   sender is pinned by the handshake and *still* refused, and only a real
//!   `peerbeam trust approve` at the listener lets the pipe through.
//! * **The exit code says what happened**, which is all a script has.
//!
//! # The two-stage shape of the happy path
//!
//! The successful pipe below needs two connections, and that is not test
//! scaffolding — it is the feature. Two fresh processes have never met, so the
//! first connection can only *pin* the sender (`approved: false`, recorded so a
//! later key change is detectable) and the pipe gate, which asks
//! `TrustStore::is_approved`, correctly refuses it. An operator then approves
//! the device, and the second connection succeeds.
//!
//! The approval is driven through the real `peerbeam trust approve` command
//! rather than by writing `trust.json`, and that is load-bearing: a hand-written
//! record would carry a fingerprint the sender cannot present, so the next
//! handshake would reject it as a key change — correctly — and a test built that
//! way would prove the opposite of what it claims.

use std::io::{BufRead, BufReader, Read};
use std::path::Path;
use std::process::{Child, Command, Output, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use peerbeam_config::EngineConfig;
use serde_json::{json, Value};

const BIN: &str = env!("CARGO_BIN_EXE_peerbeam");

/// A payload no line-oriented or UTF-8-assuming path could carry intact:
/// embedded NULs, bare CR/LF, lone continuation bytes, `0xFE`/`0xFF`.
fn hostile_payload(size: usize) -> Vec<u8> {
    let seed: [u8; 11] = [
        0x00, 0xFF, b'\n', 0x80, b'\r', 0xC3, 0x00, 0xFE, b'\n', 0x1B, 0x7F,
    ];
    (0..size)
        .map(|i| seed[i % seed.len()] ^ (i as u8))
        .collect()
}

/// Two isolated config files: each end needs its own `data_directory`, or the
/// two processes share one persistent identity and the handshake's directional
/// keys collapse onto each other (see `transfer_e2e.rs`).
fn configs(dir: &std::path::Path) -> (std::path::PathBuf, std::path::PathBuf) {
    let mut cfg = EngineConfig::default();
    cfg.storage.data_directory = dir.join("data-a").to_string_lossy().into_owned();
    cfg.storage.save_directory = dir.join("recv").to_string_lossy().into_owned();
    let a = dir.join("a.json");
    cfg.save(&a).unwrap();

    let mut cfg_b = cfg.clone();
    cfg_b.storage.data_directory = dir.join("data-b").to_string_lossy().into_owned();
    let b = dir.join("b.json");
    cfg_b.save(&b).unwrap();
    (a, b)
}

/// Read `child`'s stderr on a thread, handing back the first `pipe_listening`
/// port it announces. The port is on **stderr** by design — stdout is reserved
/// for the piped bytes — so a build that announced it on stdout would both fail
/// this lookup and corrupt the stream.
fn listening_port(child: &mut Child) -> u16 {
    let stderr = child.stderr.take().expect("stderr piped");
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            if let Ok(v) = serde_json::from_str::<Value>(&line) {
                if v["event"] == "pipe_listening" {
                    if let Some(p) = v["port"].as_u64() {
                        let _ = tx.send(p as u16);
                    }
                }
            }
        }
    });
    rx.recv_timeout(Duration::from_secs(15))
        .expect("the listener must announce its port on stderr")
}

fn wait_with_timeout(child: &mut Child, timeout: Duration) -> Option<std::process::ExitStatus> {
    let start = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Some(status),
            Ok(None) => {
                if start.elapsed() > timeout {
                    let _ = child.kill();
                    return None;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(_) => return None,
        }
    }
}

/// What a first-contact attempt left behind: the sender is now pinned at the
/// listener, and was refused.
struct FirstContact {
    /// The sender's authenticated device id, as the listener recorded it.
    device: String,
    /// The refused sender's exit status and streams.
    sender: Output,
    /// Everything the listener put on stdout, which must be nothing.
    listener_stdout: Vec<u8>,
    /// Whether the listener was still running when it was stopped.
    listener_survived: bool,
}

/// Let `sender_cfg` connect to a listener once, so the handshake pins it, and
/// hand back what happened.
///
/// This is the only way to obtain a *real* pin. The pinned fingerprint has to be
/// one the sender can present again on its next connection, and only a genuine
/// handshake produces one — see this module's header for why a hand-written
/// `trust.json` would make the test prove the opposite of its claim.
///
/// The attempt is refused (pinned is not approved), and a refusal does not end a
/// listener, so the listener is stopped here.
fn pin_sender(listener_cfg: &Path, sender_cfg: &Path, src: &Path) -> FirstContact {
    let mut listener = Command::new(BIN)
        .args([
            "--config",
            listener_cfg.to_str().unwrap(),
            "--json",
            "pipe",
            "--listen",
            "--port",
            "0",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn listener");
    let port = listening_port(&mut listener);

    let mut out_pipe = listener.stdout.take().expect("stdout piped");
    let (otx, orx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = out_pipe.read_to_end(&mut buf);
        let _ = otx.send(buf);
    });

    let sender = Command::new(BIN)
        .args([
            "--config",
            sender_cfg.to_str().unwrap(),
            "--no-color",
            "pipe",
            "--addr",
            &format!("127.0.0.1:{port}"),
        ])
        .stdin(Stdio::from(std::fs::File::open(src).unwrap()))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("run sender");

    // Read liveness and stop the listener BEFORE any assertion, so a failing
    // caller cannot orphan the process.
    let listener_survived = listener.try_wait().expect("try_wait").is_none();
    let _ = listener.kill();
    let _ = listener.wait();
    let listener_stdout = orx
        .recv_timeout(Duration::from_secs(10))
        .expect("listener stdout");

    let pinned = trust_rows(listener_cfg);
    assert_eq!(
        pinned.len(),
        1,
        "the handshake must have pinned exactly the sender: {pinned:?}"
    );
    let device = pinned[0]["id"]
        .as_str()
        .expect("the pinned row carries an id")
        .to_string();

    FirstContact {
        device,
        sender,
        listener_stdout,
        listener_survived,
    }
}

/// `peerbeam trust list --json` against a config — one object per line.
fn trust_rows(cfg: &Path) -> Vec<Value> {
    let o = Command::new(BIN)
        .args(["--config", cfg.to_str().unwrap(), "--json", "trust", "list"])
        .output()
        .expect("run trust list");
    assert!(
        o.status.success(),
        "trust list failed: {}",
        String::from_utf8_lossy(&o.stderr)
    );
    String::from_utf8_lossy(&o.stdout)
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).expect("one JSON object per line"))
        .collect()
}

/// What an operator types at the listener's shell. `--yes` because there is no
/// terminal here to answer the fingerprint prompt — the same reason a headless
/// box needs the flag.
fn trust_approve(cfg: &Path, device: &str) -> Output {
    Command::new(BIN)
        .args([
            "--config",
            cfg.to_str().unwrap(),
            "--no-color",
            "--yes",
            "trust",
            "approve",
            device,
        ])
        .output()
        .expect("run trust approve")
}

/// **A pin is not approval, across a process boundary.**
///
/// Two fresh processes: the sender completes a real handshake, so the listener
/// pins its key — and `TrustStore::is_trusted` is true for it from that instant.
/// The pipe gate asks `is_approved` instead, so the stream is refused, nothing
/// reaches the listener's stdout, and the listener keeps listening.
///
/// This is the regression the whole approval change exists to prevent, and it
/// had no coverage at the process level. A gate that went back to `is_trusted`
/// would pass every other test in this file and fail this one.
#[test]
fn a_merely_pinned_sender_is_refused_and_nothing_reaches_stdout() {
    let dir = tempfile::tempdir().unwrap();
    let (listener_cfg, sender_cfg) = configs(dir.path());
    let marker = b"PAYLOAD-MARKER-THAT-MUST-NOT-BE-PRINTED";
    let src = dir.path().join("payload.bin");
    let mut payload = marker.to_vec();
    payload.extend_from_slice(&hostile_payload(50_000));
    std::fs::write(&src, &payload).unwrap();

    let contact = pin_sender(&listener_cfg, &sender_cfg, &src);

    assert!(
        !contact.sender.status.success(),
        "a pinned-but-unapproved sender must be refused, not piped: {}",
        String::from_utf8_lossy(&contact.sender.stderr)
    );
    assert!(
        contact.listener_stdout.is_empty(),
        "the refused payload reached the listener's stdout ({} bytes)",
        contact.listener_stdout.len()
    );
    assert!(
        contact.sender.stdout.is_empty(),
        "the sender's stdout stays clean even on refusal"
    );
    assert!(
        contact.listener_survived,
        "a refused peer must not be able to end the listener with one dial"
    );

    // And the store says exactly why: the key was recorded, nobody chose it.
    let rows = trust_rows(&listener_cfg);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["id"], json!(contact.device));
    assert_eq!(
        rows[0]["approved"],
        json!(false),
        "the handshake must pin without approving — a store that approved on \
         first contact would make the gate vacuous again"
    );
}

/// The whole feature, end to end, along the path a person actually walks: first
/// contact pins and is refused, `peerbeam trust approve` grants the device
/// standing, and only then does `pipe --listen > out` receive `in` exactly.
///
/// The byte-for-byte comparison is simultaneously the binary-safety test and
/// the stdout-cleanliness test: any human-facing text on the listener's stdout
/// would show up as extra bytes in the file.
#[test]
fn pipes_a_binary_stream_between_two_processes_and_stdout_carries_only_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let (listener_cfg, sender_cfg) = configs(dir.path());
    let payload = hostile_payload(300_000);
    let src = dir.path().join("payload.bin");
    std::fs::write(&src, &payload).unwrap();

    // 1. First contact: pinned, and refused for it. A tiny payload — this
    //    connection exists to produce a real fingerprint, not to move bytes.
    let hello = dir.path().join("first-contact.bin");
    std::fs::write(&hello, b"first contact").unwrap();
    let contact = pin_sender(&listener_cfg, &sender_cfg, &hello);
    assert!(
        !contact.sender.status.success(),
        "a merely pinned sender must not be piped through"
    );

    // 2. The operator approves the device, at the listener's own shell.
    let approved = trust_approve(&listener_cfg, &contact.device);
    assert!(
        approved.status.success(),
        "trust approve failed: {}",
        String::from_utf8_lossy(&approved.stderr)
    );
    assert!(
        String::from_utf8_lossy(&approved.stdout).contains(&contact.device),
        "approve must name the device it approved"
    );
    assert_eq!(
        trust_rows(&listener_cfg)[0]["approved"],
        json!(true),
        "approval must be on disk before the second listener reads it"
    );

    // 3. Now — and only now — the pipe works.
    let mut listener = Command::new(BIN)
        .args([
            "--config",
            listener_cfg.to_str().unwrap(),
            "--json",
            "pipe",
            "--listen",
            "--port",
            "0",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn listener");
    let port = listening_port(&mut listener);

    // Collect the listener's stdout on a thread; it is binary, so it is read as
    // bytes and never through a line reader.
    let mut out_pipe = listener.stdout.take().expect("stdout piped");
    let (otx, orx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = out_pipe.read_to_end(&mut buf);
        let _ = otx.send(buf);
    });

    let send = Command::new(BIN)
        .args([
            "--config",
            sender_cfg.to_str().unwrap(),
            "--json",
            "pipe",
            "--addr",
            &format!("127.0.0.1:{port}"),
        ])
        .stdin(Stdio::from(std::fs::File::open(&src).unwrap()))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("run sender");
    assert!(
        send.status.success(),
        "send failed: {}",
        String::from_utf8_lossy(&send.stderr)
    );
    // The sending half obeys the same rule: nothing on stdout, `--json` events
    // on stderr.
    assert!(
        send.stdout.is_empty(),
        "`pipe --to` wrote {} bytes to stdout; every line belongs on stderr",
        send.stdout.len()
    );
    let events = String::from_utf8_lossy(&send.stderr);
    assert!(
        events.lines().any(|l| l.contains("\"event\":\"piped\"")),
        "the sender must report on stderr: {events}"
    );

    let status = wait_with_timeout(&mut listener, Duration::from_secs(20));
    assert!(
        status.map(|s| s.success()).unwrap_or(false),
        "the listener must exit 0 after one stream"
    );

    let got = orx
        .recv_timeout(Duration::from_secs(10))
        .expect("listener stdout");
    assert_eq!(
        got, payload,
        "stdout must be the piped bytes and nothing else"
    );
}

/// **The listen gate, across processes.** A running `peerbeam receive` — which
/// advertises the pipe capability and accepts the channel — refuses the pipe,
/// the sender fails with a non-zero exit, and none of the payload appears in the
/// receiver's output.
///
/// This is the one that matters: a daemon that accepted a pipe would be writing
/// an unrelated peer's bytes to whatever terminal it was started from.
#[test]
fn a_running_receive_refuses_a_pipe_and_the_sender_fails() {
    let dir = tempfile::tempdir().unwrap();
    let (receiver_cfg, sender_cfg) = configs(dir.path());
    let recv_dir = dir.path().join("recv");
    std::fs::create_dir_all(&recv_dir).unwrap();
    let marker = b"PAYLOAD-MARKER-THAT-MUST-NOT-BE-PRINTED";
    let src = dir.path().join("payload.bin");
    let mut payload = marker.to_vec();
    payload.extend_from_slice(&hostile_payload(50_000));
    std::fs::write(&src, &payload).unwrap();

    let mut receiver = Command::new(BIN)
        .args([
            "--config",
            receiver_cfg.to_str().unwrap(),
            "--no-color",
            "receive",
            "--once",
            "--port",
            "0",
            "--dir",
            recv_dir.to_str().unwrap(),
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("spawn receiver");

    let stdout = receiver.stdout.take().expect("stdout piped");
    let (tx, rx) = mpsc::channel::<String>();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            let _ = tx.send(line);
        }
    });
    let mut lines = Vec::new();
    let port = loop {
        let line = rx
            .recv_timeout(Duration::from_secs(15))
            .expect("receiver announces a port");
        lines.push(line.clone());
        if let Some(p) = parse_listen_port(&line) {
            break p;
        }
    };

    let send = Command::new(BIN)
        .args([
            "--config",
            sender_cfg.to_str().unwrap(),
            "--no-color",
            "pipe",
            "--addr",
            &format!("127.0.0.1:{port}"),
        ])
        .stdin(Stdio::from(std::fs::File::open(&src).unwrap()))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("run sender");

    // Collect whatever the receiver said, then stop it BEFORE asserting
    // (`--once` counts transfers, and a refused pipe is not one, so it is still
    // listening — and a panic here would otherwise orphan it).
    while let Ok(line) = rx.recv_timeout(Duration::from_secs(2)) {
        lines.push(line);
    }
    let _ = receiver.kill();
    let _ = receiver.wait();

    assert!(
        !send.status.success(),
        "piping into a `receive` must fail, not silently succeed"
    );
    let err = String::from_utf8_lossy(&send.stderr);
    assert!(
        err.contains("pipe --listen"),
        "the failure must point at the only thing that accepts a pipe: {err}"
    );
    assert!(
        send.stdout.is_empty(),
        "the sender's stdout stays clean even on failure"
    );

    let said = lines.join("\n");
    assert!(
        !said.contains(std::str::from_utf8(marker).unwrap()),
        "the payload reached the receiver's output: {said}"
    );
    assert!(
        said.contains("pipe --listen"),
        "the receiver must say why it refused: {said}"
    );
}

/// `--from` names the device a listener will take a pipe from. A peer that is
/// not that device is refused even though it is trusted, the sender fails, and
/// the listener keeps listening rather than exiting — a stranger must not be
/// able to end it with one dial.
#[test]
fn from_refuses_another_device_and_the_listener_survives_it() {
    let dir = tempfile::tempdir().unwrap();
    let (listener_cfg, sender_cfg) = configs(dir.path());
    let src = dir.path().join("payload.bin");
    std::fs::write(&src, hostile_payload(20_000)).unwrap();

    let mut listener = Command::new(BIN)
        .args([
            "--config",
            listener_cfg.to_str().unwrap(),
            "--json",
            "pipe",
            "--listen",
            "--port",
            "0",
            "--from",
            "pb-some-other-device",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn listener");
    let port = listening_port(&mut listener);

    let send = Command::new(BIN)
        .args([
            "--config",
            sender_cfg.to_str().unwrap(),
            "--no-color",
            "pipe",
            "--addr",
            &format!("127.0.0.1:{port}"),
        ])
        .stdin(Stdio::from(std::fs::File::open(&src).unwrap()))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("run sender");

    // Read the listener's liveness before killing it, and kill before
    // asserting so a failure cannot orphan the process.
    let still_up = listener.try_wait().expect("try_wait").is_none();
    let _ = listener.kill();
    let _ = listener.wait();

    assert!(!send.status.success(), "--from must refuse another device");
    assert!(
        still_up,
        "a refused peer must not be able to end the listener"
    );
}

/// Usage errors are usage errors: no direction at all, and two at once.
#[test]
fn a_direction_is_required_and_only_one_is_allowed() {
    let none = Command::new(BIN).args(["pipe"]).output().expect("run");
    assert_eq!(none.status.code(), Some(2), "clap usage exit code");

    let both = Command::new(BIN)
        .args(["pipe", "--listen", "--to", "laptop"])
        .output()
        .expect("run");
    assert_eq!(both.status.code(), Some(2));
    let msg = String::from_utf8_lossy(&both.stderr);
    assert!(msg.contains("cannot be used with"), "{msg}");
}

/// `peerbeam receive`'s own port line, unchanged from `transfer_e2e.rs`.
fn parse_listen_port(line: &str) -> Option<u16> {
    let after = line.split("listening on").nth(1)?;
    let addr = after.split_whitespace().next()?;
    addr.rsplit(':').next()?.parse().ok()
}
