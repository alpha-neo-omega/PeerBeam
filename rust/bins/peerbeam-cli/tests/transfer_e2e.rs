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

    // The sender gets its own `data_directory` (own identity + trust store),
    // exactly as a second, distinct real device would. Sharing `cfg_path`
    // between the two processes would give them the *same* persistent
    // identity, collapsing the handshake's directional keys onto each other
    // (both sides otherwise-`<`-compare their own public key against an
    // identical peer key) and failing key confirmation.
    let sender_cfg_path = dir.path().join("sender-config.json");
    let mut sender_cfg = cfg.clone();
    sender_cfg.storage.data_directory = dir
        .path()
        .join("data-sender")
        .to_string_lossy()
        .into_owned();
    sender_cfg.save(&sender_cfg_path).unwrap();

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
            sender_cfg_path.to_str().unwrap(),
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

    // A distinct `data_directory` for the sender — see the comment in
    // `sends_a_file_between_two_processes_over_quic` for why sharing one
    // identity between the two ends breaks the handshake.
    let sender_cfg_path = dir.path().join("sender-config.json");
    let mut sender_cfg = cfg.clone();
    sender_cfg.storage.data_directory = dir
        .path()
        .join("data-sender")
        .to_string_lossy()
        .into_owned();
    sender_cfg.save(&sender_cfg_path).unwrap();

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
            sender_cfg_path.to_str().unwrap(),
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
fn sends_a_folder_between_two_processes_over_quic() {
    let dir = tempfile::tempdir().unwrap();
    let cfg_path = dir.path().join("config.json");
    let recv_dir = dir.path().join("recv");
    let folder = dir.path().join("payload");

    let mut cfg = EngineConfig::default();
    cfg.storage.data_directory = dir.path().join("data").to_string_lossy().into_owned();
    cfg.storage.save_directory = recv_dir.to_string_lossy().into_owned();
    cfg.save(&cfg_path).unwrap();

    // A distinct `data_directory` for the sender — see the comment in
    // `sends_a_file_between_two_processes_over_quic` for why sharing one
    // identity between the two ends breaks the handshake.
    let sender_cfg_path = dir.path().join("sender-config.json");
    let mut sender_cfg = cfg.clone();
    sender_cfg.storage.data_directory = dir
        .path()
        .join("data-sender")
        .to_string_lossy()
        .into_owned();
    sender_cfg.save(&sender_cfg_path).unwrap();

    std::fs::create_dir_all(&folder).unwrap();
    std::fs::write(folder.join("a.txt"), b"alpha").unwrap();
    let big: Vec<u8> = (0..(200 * 1024)).map(|i| (i % 251) as u8).collect();
    std::fs::write(folder.join("b.bin"), &big).unwrap();
    std::fs::create_dir_all(&recv_dir).unwrap();

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

    let send = Command::new(BIN)
        .args([
            "--config",
            sender_cfg_path.to_str().unwrap(),
            "--no-color",
            "-y",
            "send",
            folder.to_str().unwrap(),
            "--addr",
            &format!("127.0.0.1:{port}"),
        ])
        .output()
        .expect("run sender");
    assert!(
        send.status.success(),
        "folder send failed: {}\n{}",
        String::from_utf8_lossy(&send.stdout),
        String::from_utf8_lossy(&send.stderr),
    );

    let status = wait_with_timeout(&mut receiver, Duration::from_secs(15));
    assert!(
        status.map(|s| s.success()).unwrap_or(false),
        "receiver exit"
    );

    // The whole folder arrived under recv_dir/payload/ byte-for-byte.
    assert_eq!(
        std::fs::read(recv_dir.join("payload").join("a.txt")).expect("a.txt"),
        b"alpha"
    );
    assert_eq!(
        std::fs::read(recv_dir.join("payload").join("b.bin")).expect("b.bin"),
        big
    );
}

#[test]
fn status_json_reports_real_fields() {
    let dir = tempfile::tempdir().unwrap();
    let cfg_path = dir.path().join("config.json");
    // `data_directory` must be isolated under the tempdir: `status` now loads
    // (and, on first run, generates + persists) the device's identity file at
    // `<data_directory>/identity.json`. Leaving `EngineConfig::default()`'s
    // real platform data dir in place would make this test read/write the
    // host's actual `identity.json` as a side effect of `cargo test`.
    let mut cfg = EngineConfig::default();
    cfg.storage.data_directory = dir.path().join("data").to_string_lossy().into_owned();
    cfg.save(&cfg_path).unwrap();

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
    assert!(
        v["device_id"]
            .as_str()
            .is_some_and(|s| s.starts_with("pb-")),
        "device_id must be the persistent pb-<fingerprint> id: {:?}",
        v["device_id"]
    );
}

/// The id `status` reports (the same id discovery announces us as, and the
/// transfer handshake authenticates with — see `crate::engine::device_id`)
/// must be the persistent identity, not a fresh id each run: same value
/// across two invocations, and identical to what's on disk.
#[test]
fn device_id_is_stable_across_invocations_and_matches_identity_file() {
    let dir = tempfile::tempdir().unwrap();
    let cfg_path = dir.path().join("config.json");
    let data_dir = dir.path().join("data");

    let mut cfg = EngineConfig::default();
    cfg.storage.data_directory = data_dir.to_string_lossy().into_owned();
    cfg.save(&cfg_path).unwrap();

    let run_status = || -> String {
        let out = Command::new(BIN)
            .args(["--config", cfg_path.to_str().unwrap(), "--json", "status"])
            .output()
            .expect("run status");
        assert!(out.status.success());
        let v: Value = serde_json::from_slice(&out.stdout).expect("valid json");
        v["device_id"]
            .as_str()
            .expect("device_id present")
            .to_string()
    };

    let id1 = run_status();
    let id2 = run_status();
    assert_eq!(id1, id2, "advertised device id must be stable across runs");
    assert!(id1.starts_with("pb-"));

    // Same source of truth as the file `SecureCtx::build` authenticates with.
    let stored: Value =
        serde_json::from_slice(&std::fs::read(data_dir.join("identity.json")).unwrap())
            .expect("identity.json is valid json");
    assert_eq!(
        stored["device_id"].as_str(),
        Some(id1.as_str()),
        "status-advertised id must match the persisted identity file"
    );
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

/// **The `files` permission, enforced by the CLI.**
///
/// It was not, for the whole of this project's life: `peerbeam receive` and
/// `peerbeam daemon` reached the receive path with no admission gate, so
/// revoking a device's `files` permission changed nothing on a headless box —
/// while `docs/SECURITY.md` said the permission was "enforced in both
/// directions" and `peerbeam trust` printed sentences about a gate that never
/// ran.
///
/// The gate refuses only what the user already decided about: a device they
/// approved and then narrowed. A merely-pinned stranger is untouched, which is
/// what keeps a headless receiver from turning into one that accepts nothing.
#[test]
fn a_device_whose_files_permission_was_revoked_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let cfg_path = dir.path().join("config.json");
    let recv_dir = dir.path().join("recv");
    let src = dir.path().join("secret.bin");

    let mut cfg = EngineConfig::default();
    cfg.storage.data_directory = dir.path().join("data").to_string_lossy().into_owned();
    cfg.storage.save_directory = recv_dir.to_string_lossy().into_owned();
    cfg.save(&cfg_path).unwrap();

    let sender_cfg_path = dir.path().join("sender-config.json");
    let mut sender_cfg = cfg.clone();
    sender_cfg.storage.data_directory = dir
        .path()
        .join("data-sender")
        .to_string_lossy()
        .into_owned();
    sender_cfg.save(&sender_cfg_path).unwrap();

    std::fs::write(&src, b"not for you").unwrap();
    std::fs::create_dir_all(&recv_dir).unwrap();

    // The sender's own device id, which is what the receiver will pin.
    let status = Command::new(BIN)
        .args([
            "--config",
            sender_cfg_path.to_str().unwrap(),
            "--no-color",
            "--json",
            "status",
        ])
        .output()
        .expect("status");
    let sender_id = serde_json::from_slice::<serde_json::Value>(&status.stdout)
        .ok()
        .and_then(|v| v["device_id"].as_str().map(str::to_string))
        .expect("sender device id");

    // Round one: an ordinary transfer, so the receiver pins the sender.
    let port = spawn_receiver_and_send(&cfg_path, &recv_dir, &sender_cfg_path, &src);
    assert!(
        recv_dir.join("secret.bin").exists(),
        "the first transfer is unaffected — the peer is only pinned, not narrowed"
    );
    std::fs::remove_file(recv_dir.join("secret.bin")).unwrap();
    let _ = port;

    // The user approves the device, then takes its Files permission away.
    for args in [
        vec!["trust", "approve", sender_id.as_str()],
        vec!["trust", "revoke-permission", sender_id.as_str(), "files"],
    ] {
        let mut full = vec!["--config", cfg_path.to_str().unwrap(), "--no-color", "-y"];
        full.extend_from_slice(&args);
        let o = Command::new(BIN).args(&full).output().expect("trust");
        assert!(
            o.status.success(),
            "{args:?} failed: {}{}",
            String::from_utf8_lossy(&o.stdout),
            String::from_utf8_lossy(&o.stderr)
        );
    }

    // Round two: the same send must now be refused, and nothing may land.
    let _ = spawn_receiver_and_send(&cfg_path, &recv_dir, &sender_cfg_path, &src);
    assert!(
        !recv_dir.join("secret.bin").exists(),
        "a device whose Files permission was revoked must not be able to send"
    );
}

/// Start a `receive --once`, send one file to it, and return the port used.
///
/// The send's exit status is deliberately not asserted: this helper is used for
/// both the accepted and the refused round, and a refusal is a *successful*
/// enforcement, not a test failure.
fn spawn_receiver_and_send(
    cfg_path: &std::path::Path,
    recv_dir: &std::path::Path,
    sender_cfg_path: &std::path::Path,
    src: &std::path::Path,
) -> u16 {
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
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn receiver");

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
        .recv_timeout(Duration::from_secs(20))
        .expect("receiver should announce a listening port");

    let _ = Command::new(BIN)
        .args([
            "--config",
            sender_cfg_path.to_str().unwrap(),
            "--no-color",
            "-y",
            "send",
            src.to_str().unwrap(),
            "--addr",
            &format!("127.0.0.1:{port}"),
        ])
        .output()
        .expect("run sender");

    let _ = wait_with_timeout(&mut receiver, Duration::from_secs(20));
    let _ = receiver.kill();
    let _ = receiver.wait();
    port
}

/// **A peer that completes the handshake and then does nothing must not stop
/// everyone else.**
///
/// The accept loop used to run each connection to completion inline, so this
/// exact peer — connected, authenticated, silent — held the loop for as long as
/// it liked. The deadline added earlier bounds that at two minutes; running the
/// connection on its own task removes it.
///
/// A bare datagram will not do: it is rejected before the loop reaches the body
/// under test, so the test would pass against the serial version and prove
/// nothing. This dials a real session and holds it.
#[test]
fn a_silent_authenticated_peer_does_not_block_a_real_transfer() {
    use std::sync::Arc;

    let dir = tempfile::tempdir().unwrap();
    let cfg_path = dir.path().join("config.json");
    let recv_dir = dir.path().join("recv");
    let src = dir.path().join("payload.bin");

    let mut cfg = EngineConfig::default();
    cfg.storage.data_directory = dir.path().join("data").to_string_lossy().into_owned();
    cfg.storage.save_directory = recv_dir.to_string_lossy().into_owned();
    cfg.save(&cfg_path).unwrap();

    let sender_cfg_path = dir.path().join("sender-config.json");
    let mut sender_cfg = cfg.clone();
    sender_cfg.storage.data_directory = dir
        .path()
        .join("data-sender")
        .to_string_lossy()
        .into_owned();
    sender_cfg.save(&sender_cfg_path).unwrap();

    std::fs::write(&src, vec![9u8; 64 * 1024]).unwrap();
    std::fs::create_dir_all(&recv_dir).unwrap();

    // Daemon-shaped: no `--once`, so it keeps serving.
    let mut receiver = Command::new(BIN)
        .args([
            "--config",
            cfg_path.to_str().unwrap(),
            "--no-color",
            "receive",
            "--port",
            "0",
            "--dir",
            recv_dir.to_str().unwrap(),
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn receiver");

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
        .recv_timeout(Duration::from_secs(20))
        .expect("receiver should announce a listening port");

    // The squatter: a real handshake, then silence, held for the whole test.
    let holding = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let held = holding.clone();
    let squatter = std::thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new().expect("rt");
        rt.block_on(async move {
            let quic = peerbeam_transfer_quic::QuicTransport::new().expect("quic");
            let route = peerbeam_domain::entity::Route {
                kind: peerbeam_domain::entity::RouteKind::Lan,
                address: "127.0.0.1".into(),
                port,
            };
            let meta = peerbeam_domain::entity::TransferSession {
                id: peerbeam_domain::id::TransferId::from("squat"),
                peer: peerbeam_domain::id::DeviceId::from("pb-squatter"),
                direction: peerbeam_domain::entity::Direction::Sending,
                status: peerbeam_domain::entity::TransferStatus::Transferring,
                files: Vec::new(),
                total_bytes: 0,
                transferred_bytes: 0,
                started_at: chrono::Utc::now(),
                completed_at: None,
                is_resume: false,
                accepted: true,
            };
            let Ok(qc) = quic.dial_channels(&route, &meta).await else {
                return;
            };
            use peerbeam_domain::port::EncryptionProvider as _;
            let enc = peerbeam_crypto::AeadCrypto::new();
            let keypair = enc.generate_keypair();
            let identity = peerbeam_transfer::Identity {
                device_id: peerbeam_domain::id::DeviceId::from("pb-squatter"),
                name: "squatter".into(),
                keypair,
            };
            let enc: Arc<dyn peerbeam_domain::port::EncryptionProvider> = Arc::new(enc);
            let trust: Arc<dyn peerbeam_domain::port::TrustStore> = Arc::new(
                peerbeam_trust_fs::FsTrust::open(
                    std::env::temp_dir().join(format!("pb-squat-{}.json", std::process::id())),
                )
                .expect("trust"),
            );
            let (ev, _e) = tokio::sync::mpsc::unbounded_channel();
            let (ch, _c) = tokio::sync::mpsc::unbounded_channel();
            let (inc, _i) = tokio::sync::mpsc::unbounded_channel();
            let transport: Arc<dyn peerbeam_domain::port::ChannelTransport> = Arc::new(qc);
            let cfg = peerbeam_transfer::SessionConfig::new(
                peerbeam_domain::session::CapabilitySet::new().with(
                    peerbeam_domain::session::Capability::new(
                        peerbeam_domain::session::ChannelType::CONTROL,
                    ),
                ),
            );
            let Ok(mut ps) = peerbeam_transfer::PeerSession::open(
                transport,
                peerbeam_transfer::SessionRole::Initiator,
                cfg,
                ev,
                ch,
                inc,
                None,
                identity,
                enc,
                trust,
            )
            .await
            else {
                return;
            };
            // Authenticated, and now deliberately silent: no channel is ever
            // opened. Held until the assertion below has run.
            tokio::spawn(async move {
                let _ = ps.run().await;
            });
            while held.load(std::sync::atomic::Ordering::Relaxed) {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        });
    });

    // Give the squatter time to finish its handshake and take the loop.
    std::thread::sleep(Duration::from_secs(2));

    let started = std::time::Instant::now();
    let send = Command::new(BIN)
        .args([
            "--config",
            sender_cfg_path.to_str().unwrap(),
            "--no-color",
            "-y",
            "send",
            src.to_str().unwrap(),
            "--addr",
            &format!("127.0.0.1:{port}"),
        ])
        .output()
        .expect("run sender");
    let elapsed = started.elapsed();

    holding.store(false, std::sync::atomic::Ordering::Relaxed);
    let _ = squatter.join();
    let _ = receiver.kill();
    let _ = receiver.wait();

    assert!(
        send.status.success(),
        "send failed: {}{}",
        String::from_utf8_lossy(&send.stdout),
        String::from_utf8_lossy(&send.stderr),
    );
    assert!(
        recv_dir.join("payload.bin").exists(),
        "the transfer must complete while a silent peer holds a session open"
    );
    assert!(
        elapsed < Duration::from_secs(60),
        "it must not wait the silent peer out: took {elapsed:?}"
    );
}
