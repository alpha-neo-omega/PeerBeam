//! End-to-end CLI transfer: two real `peerbeam` processes move a file over
//! QUIC with mutual authentication. The receiver binds an OS-assigned port and
//! prints it; the sender dials it with `--addr`.

use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use peerbeam_config::EngineConfig;
use serde_json::Value;

const BIN: &str = env!("CARGO_BIN_EXE_peerbeam");

#[test]
fn sends_a_file_between_two_processes_over_quic() {
    let dir = tempfile::tempdir().unwrap();
    let cfg_path = dir.path().join("config.json");
    let recv_dir = dir.path().join("recv");
    let src = dir.path().join("hello.bin");

    // Isolated config: trust store + save dir under the tempdir.
    let mut cfg = EngineConfig::default();
    cfg.storage.data_directory = dir.path().join("data").to_string_lossy().into_owned();
    cfg.storage.save_directory = recv_dir.to_string_lossy().into_owned();
    cfg.save(&cfg_path).unwrap();

    // A payload with content worth verifying byte-for-byte.
    let payload: Vec<u8> = (0..(512 * 1024)).map(|i| (i % 251) as u8).collect();
    std::fs::write(&src, &payload).unwrap();
    std::fs::create_dir_all(&recv_dir).unwrap();

    // Start the receiver (OS-assigned port; exits after one transfer).
    let mut receiver = Command::new(BIN)
        .args([
            "--config",
            cfg_path.to_str().unwrap(),
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

    // Read its stdout until it announces the bound port.
    let stdout = receiver.stdout.take().unwrap();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if let Some(port) = parse_listen_port(&line) {
                let _ = tx.send(port);
            }
        }
    });
    let port = rx
        .recv_timeout(Duration::from_secs(10))
        .expect("receiver should announce a listening port");

    // Send to it directly.
    let send = Command::new(BIN)
        .args([
            "--config",
            cfg_path.to_str().unwrap(),
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

    // Receiver should finish and exit on its own (`--once`).
    let status = wait_with_timeout(&mut receiver, Duration::from_secs(15));
    assert!(
        status.map(|s| s.success()).unwrap_or(false),
        "receiver exit"
    );

    let got = std::fs::read(recv_dir.join("hello.bin")).expect("received file");
    assert_eq!(got, payload, "file must arrive byte-for-byte over QUIC");
}

#[test]
fn json_output_is_machine_readable() {
    let dir = tempfile::tempdir().unwrap();
    let cfg_path = dir.path().join("config.json");
    let recv_dir = dir.path().join("recv");
    let src = dir.path().join("data.bin");

    let mut cfg = EngineConfig::default();
    cfg.storage.data_directory = dir.path().join("data").to_string_lossy().into_owned();
    cfg.storage.save_directory = recv_dir.to_string_lossy().into_owned();
    cfg.save(&cfg_path).unwrap();

    let payload: Vec<u8> = (0..(300 * 1024)).map(|i| (i % 251) as u8).collect();
    std::fs::write(&src, &payload).unwrap();
    std::fs::create_dir_all(&recv_dir).unwrap();

    // Receiver in JSON mode: every line is a JSON event.
    let mut receiver = Command::new(BIN)
        .args([
            "--config",
            cfg_path.to_str().unwrap(),
            "--json",
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

    let stdout = receiver.stdout.take().unwrap();
    let (tx, rx) = mpsc::channel::<Value>();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if let Ok(v) = serde_json::from_str::<Value>(&line) {
                let _ = tx.send(v);
            }
        }
    });

    // First event must be `listening` with a numeric port.
    let listening = rx
        .recv_timeout(Duration::from_secs(10))
        .expect("listening event");
    assert_eq!(listening["event"], "listening");
    let port = listening["port"].as_u64().expect("numeric port") as u16;

    // Send in JSON mode; the last stdout line is the `sent` result.
    let send = Command::new(BIN)
        .args([
            "--config",
            cfg_path.to_str().unwrap(),
            "--json",
            "send",
            src.to_str().unwrap(),
            "--addr",
            &format!("127.0.0.1:{port}"),
        ])
        .output()
        .expect("run sender");
    assert!(send.status.success(), "send failed: {:?}", send);
    let sent: Value = String::from_utf8_lossy(&send.stdout)
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .next_back()
        .expect("a JSON send result");
    assert_eq!(sent["event"], "sent");
    assert_eq!(sent["bytes"].as_u64(), Some(payload.len() as u64));

    // Receiver must emit a matching `received` event.
    let mut received = None;
    while let Ok(v) = rx.recv_timeout(Duration::from_secs(10)) {
        if v["event"] == "received" {
            received = Some(v);
            break;
        }
    }
    let received = received.expect("received event");
    assert_eq!(received["bytes"].as_u64(), Some(payload.len() as u64));

    let _ = wait_with_timeout(&mut receiver, Duration::from_secs(15));
    assert_eq!(std::fs::read(recv_dir.join("data.bin")).unwrap(), payload);
}

#[test]
fn status_json_reports_real_fields() {
    let dir = tempfile::tempdir().unwrap();
    let cfg_path = dir.path().join("config.json");
    EngineConfig::default().save(&cfg_path).unwrap();

    let out = Command::new(BIN)
        .args(["--config", cfg_path.to_str().unwrap(), "--json", "status"])
        .output()
        .expect("run status");
    assert!(out.status.success());
    let v: Value = serde_json::from_slice(&out.stdout).expect("status --json is valid json");
    assert!(v["device_name"].is_string());
    assert_eq!(v["transfer_port"].as_u64(), Some(49600));
    assert!(v["providers"].is_array());
    assert!(v["listening"].is_boolean());
}

/// Parse the port from a line like `listening on 0.0.0.0:49731 — saving to ...`.
fn parse_listen_port(line: &str) -> Option<u16> {
    let after = line.split("listening on").nth(1)?;
    let addr = after.split_whitespace().next()?;
    addr.rsplit(':').next()?.parse().ok()
}

fn wait_with_timeout(
    child: &mut std::process::Child,
    timeout: Duration,
) -> Option<std::process::ExitStatus> {
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
