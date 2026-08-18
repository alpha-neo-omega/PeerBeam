//! End-to-end auto-save rules: two real `peerbeam` processes move a file over
//! QUIC and the receiver's rules decide **where** it lands.
//!
//! Unit tests pin the matcher (`peerbeam-config`'s `rules` module) and the CLI
//! tests pin the command. What only a real receive can show is the half that
//! matters most: the bytes end up in the directory the rule named, and when
//! that directory cannot be written to, the bytes end up in the save directory
//! *and the receiver says so* — rather than the transfer failing and the file
//! being lost after the user already accepted it.

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use peerbeam_config::{EngineConfig, SaveRule};

const BIN: &str = env!("CARGO_BIN_EXE_peerbeam");

/// The receiver's config: isolated data + save directory, plus `rules`.
fn receiver_config(dir: &Path, save_dir: &Path, rules: Vec<SaveRule>) -> PathBuf {
    let mut cfg = EngineConfig::default();
    cfg.storage.data_directory = dir.join("data-recv").to_string_lossy().into_owned();
    cfg.storage.save_directory = save_dir.to_string_lossy().into_owned();
    cfg.storage.rules = rules;
    let path = dir.join("recv-config.json");
    cfg.save(&path).unwrap();
    std::fs::create_dir_all(save_dir).unwrap();
    path
}

/// The sender's config. Its own `data_directory`, so it has its own identity —
/// sharing one would collapse the handshake's directional keys (see
/// `transfer_e2e.rs`).
fn sender_config(dir: &Path) -> PathBuf {
    sender_config_named(dir, "sender")
}

/// [`sender_config`] with a chosen device **name** — the peer-supplied half of
/// its identity, and the one a rule must never match on.
fn sender_config_named(dir: &Path, name: &str) -> PathBuf {
    let mut cfg = EngineConfig::default();
    cfg.storage.data_directory = dir.join("data-send").to_string_lossy().into_owned();
    cfg.storage.save_directory = dir.join("send-recv").to_string_lossy().into_owned();
    cfg.device.name = name.to_string();
    let path = dir.join("send-config.json");
    cfg.save(&path).unwrap();
    path
}

/// The sender's **authenticated** device id, as its own `status` reports it.
///
/// Running `status` also generates the identity keypair, so the id is fixed
/// before the receiver's rules are written against it — which is what makes
/// the assertion below meaningful rather than circular.
fn device_id_of(cfg: &Path) -> String {
    let out = Command::new(BIN)
        .args(["--config", cfg.to_str().unwrap(), "--json", "status"])
        .output()
        .expect("run status");
    let text = String::from_utf8_lossy(&out.stdout);
    let line = text
        .lines()
        .find(|l| l.contains("device_id"))
        .unwrap_or_else(|| panic!("no device_id in status output: {text}"));
    let v: serde_json::Value = serde_json::from_str(line).expect("status is one JSON object");
    v["device_id"].as_str().expect("device_id").to_string()
}

/// A rule matching everything, pointed at `dest`.
fn catch_all(dest: &Path) -> SaveRule {
    SaveRule {
        directory: dest.to_string_lossy().into_owned(),
        ..SaveRule::default()
    }
}

/// Start `peerbeam receive --once` on an OS-assigned port, and return the child
/// plus a channel of its stdout lines. **No `--dir`**: that flag is an explicit
/// per-run destination and deliberately turns rules off, so a test that used it
/// would be testing nothing.
fn start_receiver(cfg: &Path) -> (Child, mpsc::Receiver<String>) {
    let mut child = Command::new(BIN)
        .args([
            "--config",
            cfg.to_str().unwrap(),
            "--no-color",
            "receive",
            "--once",
            "--port",
            "0",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("spawn receiver");

    let stdout = child.stdout.take().unwrap();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            let _ = tx.send(line);
        }
    });
    (child, rx)
}

/// Collect the receiver's stdout lines until it exits, returning them joined.
/// The port is read out of the first `listening on …` line.
fn port_from(lines: &mpsc::Receiver<String>, collected: &mut Vec<String>) -> u16 {
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while std::time::Instant::now() < deadline {
        match lines.recv_timeout(Duration::from_secs(10)) {
            Ok(line) => {
                let port = parse_listen_port(&line);
                collected.push(line);
                if let Some(p) = port {
                    return p;
                }
            }
            Err(_) => break,
        }
    }
    panic!("receiver never announced a listening port: {collected:?}");
}

fn send_to(sender_cfg: &Path, src: &Path, port: u16) {
    let send = Command::new(BIN)
        .args([
            "--config",
            sender_cfg.to_str().unwrap(),
            "--no-color",
            "-y",
            "send",
            src.to_str().unwrap(),
            "--addr",
            &format!("127.0.0.1:{port}"),
        ])
        .output()
        .expect("run sender");
    assert!(
        send.status.success(),
        "send failed: {}\n{}",
        String::from_utf8_lossy(&send.stdout),
        String::from_utf8_lossy(&send.stderr),
    );
}

/// Drain the rest of the receiver's output and wait for it to exit.
fn finish(
    child: &mut Child,
    lines: &mpsc::Receiver<String>,
    collected: &mut Vec<String>,
) -> String {
    let status = wait_with_timeout(child, Duration::from_secs(20));
    while let Ok(line) = lines.recv_timeout(Duration::from_millis(200)) {
        collected.push(line);
    }
    assert!(
        status.map(|s| s.success()).unwrap_or(false),
        "receiver exit; output was:\n{}",
        collected.join("\n")
    );
    collected.join("\n")
}

/// **A rule chooses the destination.** The file lands where the rule says, and
/// *not* in the save directory.
#[test]
fn a_matching_rule_puts_the_file_in_its_directory() {
    let dir = tempfile::tempdir().unwrap();
    let save_dir = dir.path().join("downloads");
    let sorted = dir.path().join("sorted");
    let cfg = receiver_config(dir.path(), &save_dir, vec![catch_all(&sorted)]);
    let sender_cfg = sender_config(dir.path());

    let payload: Vec<u8> = (0..(64 * 1024)).map(|i| (i % 251) as u8).collect();
    let src = dir.path().join("hello.bin");
    std::fs::write(&src, &payload).unwrap();

    let (mut child, lines) = start_receiver(&cfg);
    let mut collected = Vec::new();
    let port = port_from(&lines, &mut collected);
    send_to(&sender_cfg, &src, port);
    let output = finish(&mut child, &lines, &mut collected);

    assert_eq!(
        std::fs::read(sorted.join("hello.bin")).expect("the rule's directory holds the file"),
        payload,
        "byte-for-byte, in the directory the rule named"
    );
    assert!(
        !save_dir.join("hello.bin").exists(),
        "and nowhere else; output was:\n{output}"
    );
}

/// A rule that does not match leaves everything exactly as it was: the file
/// goes to the save directory. This is the "nothing changed for existing users"
/// guarantee, proved through a real receive rather than only in the matcher.
#[test]
fn a_rule_that_does_not_match_leaves_the_save_directory_in_charge() {
    let dir = tempfile::tempdir().unwrap();
    let save_dir = dir.path().join("downloads");
    let papers = dir.path().join("papers");
    let rule = SaveRule {
        extension: Some("pdf".into()),
        ..catch_all(&papers)
    };
    let cfg = receiver_config(dir.path(), &save_dir, vec![rule]);
    let sender_cfg = sender_config(dir.path());

    let payload = b"not a pdf".to_vec();
    let src = dir.path().join("hello.bin");
    std::fs::write(&src, &payload).unwrap();

    let (mut child, lines) = start_receiver(&cfg);
    let mut collected = Vec::new();
    let port = port_from(&lines, &mut collected);
    send_to(&sender_cfg, &src, port);
    let output = finish(&mut child, &lines, &mut collected);

    assert_eq!(
        std::fs::read(save_dir.join("hello.bin")).expect("the save directory holds the file"),
        payload,
        "output was:\n{output}"
    );
    assert!(
        !papers.exists(),
        "a rule that did not match created nothing"
    );
}

/// **The file must not be lost, and the surprise must not be silent.**
///
/// The rule's destination is a *file*, so it can never be a directory. The
/// receive must still succeed, the bytes must be in the save directory, and the
/// receiver must say on its output that it fell back and why.
///
/// Making `destination` drop the fallback and let the write fail — or fall back
/// without reporting — must break this.
#[test]
fn an_unusable_destination_falls_back_to_the_save_directory_and_says_so() {
    let dir = tempfile::tempdir().unwrap();
    let save_dir = dir.path().join("downloads");

    // Something that is emphatically not a directory, and cannot become one.
    let blocked = dir.path().join("not-a-directory");
    std::fs::write(&blocked, b"in the way").unwrap();

    let cfg = receiver_config(dir.path(), &save_dir, vec![catch_all(&blocked)]);
    let sender_cfg = sender_config(dir.path());

    let payload: Vec<u8> = (0..4096).map(|i| (i % 251) as u8).collect();
    let src = dir.path().join("hello.bin");
    std::fs::write(&src, &payload).unwrap();

    let (mut child, lines) = start_receiver(&cfg);
    let mut collected = Vec::new();
    let port = port_from(&lines, &mut collected);
    send_to(&sender_cfg, &src, port);
    let output = finish(&mut child, &lines, &mut collected);

    // 1. The file exists, at the fallback.
    assert_eq!(
        std::fs::read(save_dir.join("hello.bin")).expect("the file must survive a bad rule"),
        payload,
        "output was:\n{output}"
    );
    // 2. And the receiver said so, naming the directory it could not use.
    assert!(
        output.contains("rule destination"),
        "the fallback must be reported, not swallowed:\n{output}"
    );
    assert!(
        output.contains(&blocked.to_string_lossy().into_owned()),
        "the report must name the directory that failed:\n{output}"
    );
}

/// `receive --dir` is an explicit destination for that run and turns rules off
/// outright. A rule quietly overriding a directory the operator just typed
/// would be the surprising direction — and this is how you say "ignore my rules
/// once" without editing them.
#[test]
fn an_explicit_dir_overrides_the_rules_for_that_run() {
    let dir = tempfile::tempdir().unwrap();
    let save_dir = dir.path().join("downloads");
    let sorted = dir.path().join("sorted");
    let explicit = dir.path().join("explicit");
    std::fs::create_dir_all(&explicit).unwrap();
    let cfg = receiver_config(dir.path(), &save_dir, vec![catch_all(&sorted)]);
    let sender_cfg = sender_config(dir.path());

    let payload = b"straight to the flag's directory".to_vec();
    let src = dir.path().join("hello.bin");
    std::fs::write(&src, &payload).unwrap();

    let mut child = Command::new(BIN)
        .args([
            "--config",
            cfg.to_str().unwrap(),
            "--no-color",
            "receive",
            "--once",
            "--port",
            "0",
            "--dir",
            explicit.to_str().unwrap(),
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("spawn receiver");
    let stdout = child.stdout.take().unwrap();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            let _ = tx.send(line);
        }
    });
    let mut collected = Vec::new();
    let port = port_from(&rx, &mut collected);
    send_to(&sender_cfg, &src, port);
    let output = finish(&mut child, &rx, &mut collected);

    assert_eq!(
        std::fs::read(explicit.join("hello.bin")).expect("--dir wins"),
        payload,
        "output was:\n{output}"
    );
    assert!(!sorted.exists(), "the rule must not have been consulted");
}

/// **The sender criterion is the authenticated device id, not the name the
/// peer presents.**
///
/// The receiver holds two rules, and the one keyed to the sender's *name* is
/// **first** — so if the matcher were ever fed a name, first-match-wins would
/// send the file to `by-name`. It lands in `by-id` instead, which is the only
/// outcome consistent with the authenticated id reaching the matcher.
///
/// Feeding the peer-supplied name to `destination` at the call site must break
/// this, and so must making `SaveRule::matches` accept one.
#[test]
fn the_sender_criterion_matches_the_authenticated_id_not_the_presented_name() {
    let dir = tempfile::tempdir().unwrap();
    let save_dir = dir.path().join("downloads");
    let by_name = dir.path().join("by-name");
    let by_id = dir.path().join("by-id");

    let sender_cfg = sender_config_named(dir.path(), "laptop");
    let sender_id = device_id_of(&sender_cfg);
    assert!(sender_id.starts_with("pb-"), "unexpected id: {sender_id}");

    let cfg = receiver_config(
        dir.path(),
        &save_dir,
        vec![
            // First — and it names the device the way a *person* would.
            SaveRule {
                device: Some("laptop".into()),
                ..catch_all(&by_name)
            },
            // Second, and the one that must actually apply.
            SaveRule {
                device: Some(sender_id.clone()),
                ..catch_all(&by_id)
            },
        ],
    );

    let payload = b"whose rule is this?".to_vec();
    let src = dir.path().join("hello.bin");
    std::fs::write(&src, &payload).unwrap();

    let (mut child, lines) = start_receiver(&cfg);
    let mut collected = Vec::new();
    let port = port_from(&lines, &mut collected);
    send_to(&sender_cfg, &src, port);
    let output = finish(&mut child, &lines, &mut collected);

    assert_eq!(
        std::fs::read(by_id.join("hello.bin")).expect("the id rule must be the one that applies"),
        payload,
        "output was:\n{output}"
    );
    assert!(
        !by_name.exists(),
        "a rule keyed to the peer's self-reported name must never match"
    );
    assert!(!save_dir.join("hello.bin").exists());
}

fn parse_listen_port(line: &str) -> Option<u16> {
    let after = line.split("listening on").nth(1)?;
    let addr = after.split_whitespace().next()?;
    addr.rsplit(':').next()?.parse().ok()
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
