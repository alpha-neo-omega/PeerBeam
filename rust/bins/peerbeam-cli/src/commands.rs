//! Command implementations + dispatch.

use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use clap::CommandFactory;
use serde_json::json;
use tokio::sync::mpsc;

use peerbeam_config::EngineConfig;
use peerbeam_crypto::AeadCrypto;
use peerbeam_domain::error::Result as DResult;
use peerbeam_domain::port::{EncryptionProvider, Frame, Link, Nonce};
use peerbeam_engine::{ManagedDevice, RouteManager};
use peerbeam_storage_fs::FsStorage;
use peerbeam_transfer::{
    receive_file, receive_on_channel, send_file, send_file_on_session, send_folder_on_session,
    ChannelReceived, FolderSendRequest, Identity, SendRequest, TransferControl,
};
use peerbeam_transfer_quic::QuicTransport;
use peerbeam_trust_fs::FsTrust;

use crate::cli::*;
use crate::engine::{build_engine, config_path, me};
use crate::exit::{CliError, CliResult};
use crate::output::Ctx;
use crate::{history, prompt, resolve};

pub async fn dispatch(cmd: Command, ctx: &Ctx, cfg_override: Option<String>) -> CliResult {
    match cmd {
        Command::Config(a) => config(ctx, a, cfg_override.as_deref()),
        Command::Doctor => doctor(ctx, cfg_override.as_deref()),
        Command::Benchmark(a) => benchmark(ctx, a).await,
        Command::Discover(a) => discover(ctx, a, cfg_override.as_deref()).await,
        Command::List(a) => list(ctx, a, cfg_override.as_deref()).await,
        Command::Status => status(ctx, cfg_override.as_deref()),
        Command::Completions { shell } => completions(shell),
        Command::Send(a) => send(ctx, a, cfg_override.as_deref()).await,
        Command::Receive(a) => receive(ctx, a, cfg_override.as_deref()).await,
        Command::Clipboard(a) => clipboard(ctx, a, cfg_override.as_deref()).await,
        Command::History(a) => history_cmd(ctx, a, cfg_override.as_deref()),
        Command::Daemon(a) => daemon(ctx, a, cfg_override.as_deref()).await,
        Command::Session(a) => session_cmd(ctx, a).await,
        Command::Channels(a) => channels_cmd(ctx, a).await,
        Command::Transfers => transfers_cmd(ctx),
        Command::Migration => migration_cmd(ctx),
        Command::Recovery => recovery_cmd(ctx),
        Command::Diagnostics => diagnostics_cmd(ctx),
    }
}

// ── PeerSession diagnostics (M8, presentation only) ─────────────────────────
//
// The CLI is a stateless presentation layer over the engine's
// `SessionDiagnostics` (the single source of truth). A one-shot invocation holds
// no live sessions of its own — so a standalone `peerbeam session list` reports
// an empty set — while a long-running `daemon` (or an in-process transfer) shows
// its live sessions. No engine state is duplicated here.

/// Print a diagnostics value as JSON (always machine-readable, pretty in a TTY).
fn present(ctx: &Ctx, value: &serde_json::Value) -> CliResult {
    if ctx.json {
        ctx.json_line(value);
    } else {
        let pretty = serde_json::to_string_pretty(value)
            .map_err(|e| CliError::Other(format!("format: {e}")))?;
        ctx.line(&pretty);
    }
    Ok(())
}

async fn session_cmd(ctx: &Ctx, args: SessionArgs) -> CliResult {
    let diag = peerbeam_engine::SessionDiagnostics::new();
    let value = match args.action {
        SessionAction::List | SessionAction::Watch => diag.sessions_json(),
        SessionAction::Show { id } => diag.session_json(&id),
        SessionAction::Stats => json!({
            "sessions": diag.sessions_json()["count"].clone(),
            "transport": diag.transport_json(),
        }),
    };
    present(ctx, &value)
}

async fn channels_cmd(ctx: &Ctx, args: ChannelsArgs) -> CliResult {
    let diag = peerbeam_engine::SessionDiagnostics::new();
    let value = match args.session {
        Some(id) => diag.channels_json(&id).await,
        None => diag.all_channels_json().await,
    };
    present(ctx, &value)
}

fn transfers_cmd(ctx: &Ctx) -> CliResult {
    let diag = peerbeam_engine::SessionDiagnostics::new();
    // Transfers ride PeerSession channels; present the live sessions and the
    // transport summary.
    let value = json!({
        "sessions": diag.sessions_json(),
        "transport": diag.transport_json(),
    });
    present(ctx, &value)
}

fn migration_cmd(ctx: &Ctx) -> CliResult {
    let diag = peerbeam_engine::SessionDiagnostics::new();
    present(ctx, &diag.migration_json())
}

fn recovery_cmd(ctx: &Ctx) -> CliResult {
    let diag = peerbeam_engine::SessionDiagnostics::new();
    present(ctx, &diag.recovery_json())
}

fn diagnostics_cmd(ctx: &Ctx) -> CliResult {
    let diag = peerbeam_engine::SessionDiagnostics::new();
    present(ctx, &diag.diagnostics_json())
}

fn load_config(override_path: Option<&str>) -> Result<EngineConfig, CliError> {
    EngineConfig::load_or_default(&config_path(override_path))
        .map_err(|e| CliError::Other(format!("config: {e}")))
}

// ── config ──────────────────────────────────────────────────────

fn config(ctx: &Ctx, args: ConfigArgs, path_override: Option<&str>) -> CliResult {
    let path = config_path(path_override);
    match args.action {
        ConfigAction::Path => ctx.line(&path.to_string_lossy()),
        ConfigAction::Show => {
            let cfg = load_config(path_override)?;
            let value = serde_json::to_value(&cfg)
                .map_err(|e| CliError::Other(format!("serialize config: {e}")))?;
            if ctx.json {
                ctx.json_line(&value);
            } else {
                let pretty = serde_json::to_string_pretty(&value)
                    .map_err(|e| CliError::Other(format!("format config: {e}")))?;
                ctx.line(&pretty);
            }
        }
        ConfigAction::Get { key } => {
            let cfg = load_config(path_override)?;
            let value = serde_json::to_value(&cfg)
                .map_err(|e| CliError::Other(format!("serialize config: {e}")))?;
            let found = navigate(&value, &key)
                .ok_or_else(|| CliError::NotFound(format!("config key {key}")))?;
            if ctx.json {
                ctx.json_line(found);
            } else {
                ctx.line(&render_scalar(found));
            }
        }
        ConfigAction::Set { key, value } => {
            let cfg = load_config(path_override)?;
            let mut root = serde_json::to_value(&cfg)
                .map_err(|e| CliError::Other(format!("serialize config: {e}")))?;
            set_path(&mut root, &key, parse_value(&value)).map_err(CliError::Usage)?;
            let updated: EngineConfig = serde_json::from_value(root)
                .map_err(|e| CliError::Usage(format!("invalid value for {key}: {e}")))?;
            updated
                .save(&path)
                .map_err(|e| CliError::Other(format!("save config: {e}")))?;
            ctx.line(&ctx.green(&format!("set {key} = {value}")));
        }
    }
    Ok(())
}

fn navigate<'a>(value: &'a serde_json::Value, key: &str) -> Option<&'a serde_json::Value> {
    let mut cur = value;
    for part in key.split('.') {
        cur = cur.get(part)?;
    }
    Some(cur)
}

fn set_path(root: &mut serde_json::Value, key: &str, new: serde_json::Value) -> Result<(), String> {
    let parts: Vec<&str> = key.split('.').collect();
    let mut cur = root;
    for part in &parts[..parts.len() - 1] {
        cur = cur
            .get_mut(*part)
            .ok_or_else(|| format!("unknown config key {key}"))?;
    }
    let leaf = parts.last().unwrap();
    let obj = cur
        .as_object_mut()
        .ok_or_else(|| format!("unknown config key {key}"))?;
    if !obj.contains_key(*leaf) {
        return Err(format!("unknown config key {key}"));
    }
    obj.insert((*leaf).to_string(), new);
    Ok(())
}

fn parse_value(s: &str) -> serde_json::Value {
    serde_json::from_str(s).unwrap_or_else(|_| serde_json::Value::String(s.to_string()))
}

fn render_scalar(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

// ── doctor ──────────────────────────────────────────────────────

fn doctor(ctx: &Ctx, path_override: Option<&str>) -> CliResult {
    let cfg = load_config(path_override).unwrap_or_default();
    let mut checks: Vec<(String, &'static str, String)> = Vec::new();

    // Config dir writable.
    let cfg_dir = config_path(path_override)
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_default();
    checks.push(writable_check(
        "Config directory",
        &cfg_dir.to_string_lossy(),
    ));

    // Save dir writable.
    checks.push(writable_check(
        "Save directory",
        &cfg.storage.save_directory,
    ));

    // UDP socket bindable.
    match std::net::UdpSocket::bind("0.0.0.0:0") {
        Ok(_) => checks.push(("UDP sockets".into(), "pass", "bindable".into())),
        Err(e) => checks.push(("UDP sockets".into(), "fail", e.to_string())),
    }

    // Persistent identity (the same file `SecureCtx::build` authenticates
    // with, and the id discovery announces us as).
    let device_id = crate::engine::device_id(&cfg);
    match &device_id {
        Ok(id) => checks.push(("Identity".into(), "pass", id.to_string())),
        Err(e) => checks.push(("Identity".into(), "fail", e.to_string())),
    }

    // mDNS daemon.
    match device_id.and_then(peerbeam_discovery_mdns::MdnsDiscovery::new) {
        Ok(_) => checks.push(("mDNS".into(), "pass", "daemon available".into())),
        Err(e) => checks.push(("mDNS".into(), "warn", format!("unavailable: {e}"))),
    }

    // Tailscale CLI.
    match std::process::Command::new("tailscale")
        .arg("version")
        .output()
    {
        Ok(o) if o.status.success() => {
            checks.push(("Tailscale".into(), "pass", "CLI present".into()))
        }
        _ => checks.push(("Tailscale".into(), "warn", "CLI not found".into())),
    }

    // Crypto/identity.
    let enc = AeadCrypto::new();
    let kp = enc.generate_keypair();
    let fp = enc.fingerprint(&kp.public).0;
    checks.push(("Encryption".into(), "pass", format!("ok ({}…)", &fp[..12])));

    let failed = checks.iter().filter(|(_, s, _)| *s == "fail").count();

    if ctx.json {
        let arr: Vec<serde_json::Value> = checks
            .iter()
            .map(|(n, s, d)| json!({"check": n, "status": s, "detail": d}))
            .collect();
        ctx.json_line(&json!(arr));
    } else {
        for (name, st, detail) in &checks {
            let icon = match *st {
                "pass" => ctx.green("✓"),
                "warn" => ctx.yellow("!"),
                _ => ctx.red("✗"),
            };
            ctx.line(&format!("{icon} {:<20} {}", name, ctx.dim(detail)));
        }
    }

    if failed > 0 {
        Err(CliError::Other(format!("{failed} check(s) failed")))
    } else {
        Ok(())
    }
}

fn writable_check(name: &str, dir: &str) -> (String, &'static str, String) {
    let path = std::path::Path::new(dir);
    let probe = path.join(".peerbeam-write-test");
    match std::fs::create_dir_all(path).and_then(|_| std::fs::write(&probe, b"x")) {
        Ok(_) => {
            let _ = std::fs::remove_file(&probe);
            (name.into(), "pass", format!("writable ({dir})"))
        }
        Err(e) => (name.into(), "fail", format!("{dir}: {e}")),
    }
}

// ── benchmark ───────────────────────────────────────────────────

async fn benchmark(ctx: &Ctx, args: BenchmarkArgs) -> CliResult {
    match args.target {
        BenchTarget::Crypto => bench_crypto(ctx),
        BenchTarget::Hash => bench_hash(ctx),
        BenchTarget::Loopback { size, chunk } => bench_loopback(ctx, size, chunk).await,
    }
}

fn bench_crypto(ctx: &Ctx) -> CliResult {
    let enc = AeadCrypto::new();
    use std::hint::black_box;
    let key = [7u8; 32];
    let chunk = vec![0xABu8; 64 * 1024];
    let iterations = 1024u64; // 64 MiB

    // Vary the nonce per iteration and black_box the inputs/outputs so the
    // optimizer can't hoist or elide the loop (which produced bogus numbers).
    let start = Instant::now();
    let mut sealed = Vec::new();
    for i in 0..iterations {
        let mut nb = [0u8; 12];
        nb[0] = i as u8;
        nb[1] = (i >> 8) as u8;
        sealed = enc
            .seal(black_box(&key), &Nonce(nb), black_box(&chunk))
            .map_err(CliError::from)?;
        black_box(&sealed);
    }
    let seal_secs = start.elapsed().as_secs_f64();

    let start = Instant::now();
    for _ in 0..iterations {
        let plain = enc
            .open(black_box(&key), black_box(&sealed))
            .map_err(CliError::from)?;
        black_box(&plain);
    }
    let open_secs = start.elapsed().as_secs_f64();

    // 64 KiB per iteration → MiB total.
    let mib = (iterations * 64) as f64 / 1024.0;
    let seal_mbs = mib / seal_secs;
    let open_mbs = mib / open_secs;
    if ctx.json {
        ctx.json_line(&json!({"seal_mib_s": seal_mbs, "open_mib_s": open_mbs}));
    } else {
        ctx.line(&format!(
            "AES-256-GCM seal: {}",
            ctx.bold(&format!("{seal_mbs:.0} MiB/s"))
        ));
        ctx.line(&format!(
            "AES-256-GCM open: {}",
            ctx.bold(&format!("{open_mbs:.0} MiB/s"))
        ));
    }
    Ok(())
}

fn bench_hash(ctx: &Ctx) -> CliResult {
    use sha2::{Digest, Sha256};
    use std::hint::black_box;
    let chunk = vec![0xABu8; 64 * 1024];
    let iterations = 4096u64; // 256 MiB
    let start = Instant::now();
    let mut hasher = Sha256::new();
    for _ in 0..iterations {
        hasher.update(black_box(&chunk));
    }
    black_box(hasher.finalize());
    let secs = start.elapsed().as_secs_f64();
    let mib = (iterations * 64) as f64 / 1024.0;
    let mbs = mib / secs;
    if ctx.json {
        ctx.json_line(&json!({"sha256_mib_s": mbs}));
    } else {
        ctx.line(&format!(
            "SHA-256: {}",
            ctx.bold(&format!("{mbs:.0} MiB/s"))
        ));
    }
    Ok(())
}

async fn bench_loopback(ctx: &Ctx, size_mib: u64, chunk_kib: u32) -> CliResult {
    let dir = std::env::temp_dir().join(format!("pb-bench-{}", std::process::id()));
    std::fs::create_dir_all(&dir)?;
    let src = dir.join("bench.bin");
    let out = dir.join("out");
    let bytes = (size_mib * 1024 * 1024) as usize;
    // Stream the sample file in 1 MiB blocks so the harness itself stays
    // memory-bounded (the transfer under test is already streamed).
    {
        use std::io::Write;
        let mut f = std::fs::File::create(&src)?;
        let block = vec![0xABu8; 1024 * 1024];
        let mut remaining = bytes;
        while remaining > 0 {
            let n = remaining.min(block.len());
            f.write_all(&block[..n])?;
            remaining -= n;
        }
        f.flush()?;
    }

    let storage = FsStorage::new();
    let (mut la, mut lb) = MemLink::pair(8);
    let cs = TransferControl::new();
    let cr = TransferControl::new();
    let (ptx, mut prx) = mpsc::unbounded_channel();

    let req = SendRequest {
        transfer_id: "bench".into(),
        name: "bench.bin".into(),
        path: src.to_string_lossy().into(),
        size: bytes as u64,
        chunk_size: chunk_kib * 1024,
    };
    let out_str = out.to_string_lossy().to_string();
    let bar = ctx.bar(bytes as u64, "loopback");

    let start = Instant::now();
    let send = async move {
        let r = send_file(&mut la, &storage, req, &cs, &ptx, 0).await;
        drop(ptx);
        r
    };
    let recv_storage = FsStorage::new();
    let recv = async move {
        let (rtx, _rrx) = mpsc::unbounded_channel();
        receive_file(&mut lb, &recv_storage, &out_str, &cr, &rtx).await
    };
    let pump = async {
        while let Some(p) = prx.recv().await {
            bar.update(p.transferred_bytes);
        }
        bar.finish();
    };

    let (rs, rr, _) = tokio::join!(send, recv, pump);
    // Clean up the multi-hundred-MiB sample dir unconditionally, before the
    // `?`s below can return early on error — otherwise a failed benchmark
    // leaks it permanently.
    let _ = std::fs::remove_dir_all(&dir);
    rs.map_err(CliError::from)?;
    rr.map_err(CliError::from)?;
    let secs = start.elapsed().as_secs_f64();
    let mbs = size_mib as f64 / secs;

    if ctx.json {
        ctx.json_line(&json!({"mib": size_mib, "seconds": secs, "mib_s": mbs}));
    } else {
        ctx.line(&format!(
            "transferred {} MiB in {:.2}s = {}",
            size_mib,
            secs,
            ctx.bold(&format!("{mbs:.0} MiB/s")),
        ));
    }
    Ok(())
}

// ── discovery-backed ────────────────────────────────────────────

async fn snapshot(config: EngineConfig, secs: u64) -> Vec<ManagedDevice> {
    let Ok(engine) = build_engine(config.clone()) else {
        return Vec::new();
    };
    let Ok(self_device) = me(&config) else {
        return Vec::new();
    };
    if engine.start_discovery(self_device).await.is_err() {
        return Vec::new();
    }
    tokio::time::sleep(Duration::from_secs(secs)).await;
    let devices = engine.devices();
    let _ = engine.stop_discovery().await;
    devices
}

fn device_rows(devices: &[ManagedDevice]) -> Vec<Vec<String>> {
    devices
        .iter()
        .map(|m| {
            let reach = {
                let mut r = Vec::new();
                if m.capabilities.reachable_lan {
                    r.push("LAN");
                }
                if m.capabilities.reachable_remote {
                    r.push("remote");
                }
                r.join("+")
            };
            vec![
                m.device.name.clone(),
                if m.online {
                    "online".into()
                } else {
                    "offline".into()
                },
                reach,
                m.latency_ms.map(|l| format!("{l} ms")).unwrap_or_default(),
                m.device.id.to_string(),
            ]
        })
        .collect()
}

async fn discover(ctx: &Ctx, args: DiscoverArgs, path_override: Option<&str>) -> CliResult {
    let config = load_config(path_override)?;
    if args.watch {
        let engine = build_engine(config.clone())?;
        let mut changes = engine.device_changes();
        engine.start_discovery(me(&config)?).await?;
        if !ctx.json {
            ctx.line(&ctx.dim("watching for devices (Ctrl-C to stop)…"));
        }
        loop {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => break,
                change = changes.recv() => match change {
                    Ok(c) => emit_change(ctx, &c),
                    Err(_) => break,
                },
            }
        }
        let _ = engine.stop_discovery().await;
        return Ok(());
    }

    let devices = snapshot(config, args.timeout).await;
    print_devices(ctx, &devices);
    Ok(())
}

fn emit_change(ctx: &Ctx, change: &peerbeam_engine::DeviceChange) {
    use peerbeam_engine::DeviceChange as C;
    if ctx.json {
        let v = match change {
            C::Added(m) => {
                json!({"change":"added","name":m.device.name,"id":m.device.id.to_string()})
            }
            C::Updated(m) => {
                json!({"change":"updated","name":m.device.name,"id":m.device.id.to_string()})
            }
            C::StatusChanged { id, online } => {
                json!({"change":"status","id":id.to_string(),"online":online})
            }
            C::LatencyChanged { id, latency_ms } => {
                json!({"change":"latency","id":id.to_string(),"latency_ms":latency_ms})
            }
            C::Removed(id) => json!({"change":"removed","id":id.to_string()}),
        };
        ctx.json_line(&v);
    } else {
        match change {
            C::Added(m) => ctx.line(&format!("{} {}", ctx.green("+"), m.device.name)),
            C::Updated(m) => ctx.line(&ctx.dim(&format!("* {}", m.device.name))),
            C::StatusChanged { id, online } => {
                let marker = if *online {
                    ctx.green("online")
                } else {
                    ctx.dim("offline")
                };
                ctx.line(&format!("{marker} {id}"));
            }
            C::LatencyChanged { id, latency_ms } => {
                let ms = latency_ms
                    .map(|l| l.to_string())
                    .unwrap_or_else(|| "?".into());
                ctx.line(&ctx.dim(&format!("~ {id} {ms}ms")));
            }
            C::Removed(id) => ctx.line(&format!("{} {}", ctx.red("-"), id)),
        }
    }
}

async fn list(ctx: &Ctx, args: ListArgs, path_override: Option<&str>) -> CliResult {
    let config = load_config(path_override)?;
    let mut devices = snapshot(config, 2).await;
    if args.online {
        devices.retain(|m| m.online);
    }
    print_devices(ctx, &devices);
    Ok(())
}

fn print_devices(ctx: &Ctx, devices: &[ManagedDevice]) {
    if ctx.json {
        let arr: Vec<serde_json::Value> = devices
            .iter()
            .map(|m| {
                json!({
                    "id": m.device.id.to_string(),
                    "name": m.device.name,
                    "online": m.online,
                    "reachable_lan": m.capabilities.reachable_lan,
                    "reachable_remote": m.capabilities.reachable_remote,
                    "latency_ms": m.latency_ms,
                })
            })
            .collect();
        ctx.json_line(&json!(arr));
        return;
    }
    if devices.is_empty() {
        ctx.line(&ctx.dim("No devices found."));
        return;
    }
    ctx.table(
        &["NAME", "STATUS", "REACH", "LATENCY", "ID"],
        &device_rows(devices),
    );
}

fn status(ctx: &Ctx, path_override: Option<&str>) -> CliResult {
    let config = load_config(path_override)?;
    let port = config.transfer.port;

    // This device's persistent id — the same one discovery announces us as
    // and the transfer handshake authenticates with (see `SecureCtx::build`).
    let device_id = crate::engine::device_id(&config)?;

    // Real provider availability (mirrors `doctor`, concise).
    let udp_ok = std::net::UdpSocket::bind("0.0.0.0:0").is_ok();
    let mdns_ok = peerbeam_discovery_mdns::MdnsDiscovery::new(device_id.clone()).is_ok();
    let tailscale_ok = std::process::Command::new("tailscale")
        .arg("version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    let mut providers = vec![];
    if udp_ok {
        providers.push("udp");
    }
    if mdns_ok {
        providers.push("mdns");
    }
    if tailscale_ok {
        providers.push("tailscale");
    }

    // Is a receiver already listening on the transfer port? Binding the UDP
    // port fails with AddrInUse when the QUIC server holds it.
    let listening = std::net::UdpSocket::bind(("0.0.0.0", port)).is_err();

    if ctx.json {
        ctx.json_line(&json!({
            "device_id": device_id.to_string(),
            "device_name": config.device.name,
            "platform": peerbeam_platform::current().as_str(),
            "transfer_port": port,
            "save_directory": config.storage.save_directory,
            "data_directory": config.storage.data_directory,
            "providers": providers,
            "listening": listening,
        }));
    } else {
        ctx.line(&format!("{}     {}", ctx.bold("Device ID:"), device_id));
        ctx.line(&format!("{}   {}", ctx.bold("Device:"), config.device.name));
        ctx.line(&format!(
            "{} {}",
            ctx.bold("Platform:"),
            peerbeam_platform::current().as_str()
        ));
        ctx.line(&format!("{}     {}", ctx.bold("Port:"), port));
        ctx.line(&format!(
            "{}  {}",
            ctx.bold("Save to:"),
            config.storage.save_directory
        ));
        ctx.line(&format!(
            "{} {}",
            ctx.bold("Providers:"),
            if providers.is_empty() {
                "none".to_string()
            } else {
                providers.join(", ")
            }
        ));
        ctx.line(&format!(
            "{} {}",
            ctx.bold("Listening:"),
            if listening {
                ctx.green("yes")
            } else {
                ctx.dim("no")
            }
        ));
    }
    Ok(())
}

fn completions(shell: clap_complete::Shell) -> CliResult {
    let mut cmd = Cli::command();
    clap_complete::generate(shell, &mut cmd, "peerbeam", &mut std::io::stdout());
    Ok(())
}

/// Clamp a configured chunk size (stored as `u64`) into the `u32` range the
/// transfer engine expects. A plain `.max(1) as u32` cast is unsound: `.max(1)`
/// runs BEFORE the truncating cast, so any value that is an exact multiple of
/// 2^32 (e.g. 4 GiB) survives the guard unchanged and then truncates to 0,
/// handing the transfer engine a zero chunk size. Clamping into range first
/// and applying the minimum after the value is already a valid `u32` closes
/// that gap.
fn clamp_chunk_size(chunk_size: u64) -> u32 {
    chunk_size.clamp(1, u32::MAX as u64) as u32
}

async fn send(ctx: &Ctx, args: SendArgs, path_override: Option<&str>) -> CliResult {
    // Validate every path up front so a bad entry fails the whole call
    // before anything is sent.
    for p in &args.paths {
        if !std::path::Path::new(p).exists() {
            return Err(CliError::NotFound(format!("path {p}")));
        }
    }

    let config = load_config(path_override)?;

    // Resolve the target peer — directly (--addr) or via discovery. The result
    // is a `Device`; the RouteManager decides which of its routes to use.
    let target = if let Some(addr) = &args.addr {
        let sa = resolve_addr(addr)?;
        target_device(addr.clone(), sa.ip().to_string(), sa.port())
    } else {
        let devices = snapshot(config.clone(), 2).await;
        let candidates: Vec<(String, String)> = devices
            .iter()
            .map(|m| (m.device.id.to_string(), m.device.name.clone()))
            .collect();
        let index = resolve_peer(ctx, &candidates, &args.to)?;
        let dev = devices[index].device.clone();
        if dev.addresses.is_empty() {
            return Err(CliError::NotFound(format!(
                "no reachable address for {}",
                dev.name
            )));
        }
        if !prompt::confirm(
            ctx,
            &format!("Send {} file(s) to {}?", args.paths.len(), dev.name),
            true,
        ) {
            return Err(CliError::Cancelled);
        }
        dev
    };

    if target.port == 0 {
        return Err(CliError::NotFound(format!(
            "{} did not advertise a transfer port",
            target.name
        )));
    }

    let sc = SecureCtx::build(&config)?;
    // Transfers run over PeerSession: the QUIC transport dials a multiplexed
    // channel connection; the RouteManager still ranks/selects the route.
    let quic = Arc::new(QuicTransport::new().map_err(CliError::from)?);
    let routes = RouteManager::new(quic.clone());
    let storage = FsStorage::new();
    let chunk = clamp_chunk_size(config.transfer.chunk_size);

    let hist = history::path_for(&config.storage.data_directory);
    for p in &args.paths {
        let path = std::path::Path::new(p);
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "file.bin".into());
        if path.is_dir() {
            let r =
                secure_send_folder(ctx, &quic, &routes, &target, &sc, &storage, p, &name, chunk)
                    .await;
            history::record(
                &hist,
                history::entry(
                    "sending",
                    &target.name,
                    &name,
                    p,
                    r.as_ref().copied().unwrap_or(0),
                    r.is_ok(),
                ),
            );
            r?;
        } else {
            let size = std::fs::metadata(path)?.len();
            let r = secure_send_file(
                ctx, &quic, &routes, &target, &sc, &storage, p, &name, size, chunk,
            )
            .await;
            history::record(
                &hist,
                history::entry("sending", &target.name, &name, p, size, r.is_ok()),
            );
            r?;
        }
    }
    Ok(())
}

/// Establish a PeerSession and stream a whole folder over a transfer channel;
/// returns total bytes sent.
#[allow(clippy::too_many_arguments)]
async fn secure_send_folder(
    ctx: &Ctx,
    quic: &Arc<QuicTransport>,
    routes: &RouteManager,
    device: &peerbeam_domain::entity::Device,
    sc: &SecureCtx,
    storage: &FsStorage,
    path: &str,
    name: &str,
    chunk: u32,
) -> Result<u64, CliError> {
    let session =
        crate::session_transfer::dial(quic, routes, device, name, &sc.ident, &sc.enc, &sc.trust)
            .await?;
    let newly_trusted = session.newly_trusted;
    let peer_id = session.peer_id.clone();
    // Captured now (rather than read off `session` down by the JSON output)
    // because `session.close()` below takes `self` by value — after that
    // point `session` is moved and no longer accessible.
    let pairing_code = session.pairing_code.clone();
    if newly_trusted && !ctx.json {
        ctx.line(&ctx.dim(&format!("pinned new peer {peer_id}")));
        ctx.line(&format!("  pairing code: {}", ctx.bold(&pairing_code)));
    }

    let (ptx, mut prx) = mpsc::unbounded_channel();
    let ctrl = TransferControl::new();
    let req = FolderSendRequest {
        transfer_id: name.to_string(),
        root_path: path.to_string(),
        chunk_size: chunk,
    };

    let handle = &session.handle;
    let send = async move {
        let r = send_folder_on_session(handle, storage, req, &ctrl, &ptx, 3).await;
        drop(ptx);
        r
    };
    // Per-file progress: a fresh bar whenever the current file changes.
    let pump = async move {
        let mut bar: Option<crate::output::Bar> = None;
        let mut current = String::new();
        let mut last_bytes = 0u64;
        while let Some(p) = prx.recv().await {
            if !ctx.json {
                if let Some(f) = &p.current_file {
                    if *f != current {
                        if let Some(b) = bar.take() {
                            b.finish();
                        }
                        current = f.clone();
                        bar = Some(ctx.bar(p.total_bytes, &current));
                    }
                }
                if let Some(b) = &bar {
                    b.update(p.transferred_bytes);
                }
            }
            last_bytes = last_bytes.max(p.transferred_bytes);
        }
        if let Some(b) = bar.take() {
            b.finish();
        }
        last_bytes
    };
    let (r, bytes) = tokio::join!(send, pump);
    let outcome = r.map_err(CliError::from)?;
    session.close().await;

    if ctx.json {
        ctx.json_line(&json!({
            "event": "sent_folder",
            "folder": name,
            "outcome": format!("{outcome:?}"),
            "peer": peer_id,
            "newly_trusted": newly_trusted,
            "pairing_code": pairing_code,
            "transport": "peersession",
        }));
    } else {
        ctx.line(&ctx.green(&format!("sent folder {name}")));
    }
    Ok(bytes)
}

/// Outcome of the optional first-contact pairing check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PairingGate {
    /// Not first contact, or the toggle is off — proceed without blocking.
    Proceed,
    /// First contact + toggle on + the user confirmed the codes match.
    Confirmed,
    /// First contact + toggle on + declined or no answer — revoke and abort.
    Revoke,
}

/// Decide what to do at first contact. `answer` is `Some(true/false)` for an
/// explicit yes/no, or `None` when no confirmation could be obtained (JSON /
/// non-interactive / EOF), which is treated as a decline (safe default).
pub(crate) fn pairing_gate(
    newly_trusted: bool,
    require: bool,
    answer: Option<bool>,
) -> PairingGate {
    if !newly_trusted || !require {
        return PairingGate::Proceed;
    }
    match answer {
        Some(true) => PairingGate::Confirmed,
        _ => PairingGate::Revoke,
    }
}

/// Read a yes/no answer from `reader`. `None` on EOF/error (no answer); an
/// empty line counts as "no" (the prompt's default is No).
pub(crate) fn read_confirm(reader: &mut impl std::io::BufRead) -> Option<bool> {
    let mut line = String::new();
    match reader.read_line(&mut line) {
        Ok(0) => None,
        Ok(_) => Some(matches!(
            line.trim().to_ascii_lowercase().as_str(),
            "y" | "yes"
        )),
        Err(_) => None,
    }
}

/// A minimal `Device` for a `--addr` target (a single explicit route).
fn target_device(name: String, address: String, port: u16) -> peerbeam_domain::entity::Device {
    use peerbeam_domain::entity::{Device, DeviceType};
    use peerbeam_domain::id::DeviceId;
    Device {
        id: DeviceId::from("addr"),
        name,
        device_type: DeviceType::Desktop,
        platform: peerbeam_platform::current(),
        addresses: vec![address],
        port,
        last_seen: chrono::Utc::now(),
    }
}

/// Receive incoming files: serve QUIC, accept, authenticate, stream to disk.
async fn receive(ctx: &Ctx, args: ReceiveArgs, path_override: Option<&str>) -> CliResult {
    let config = load_config(path_override)?;
    let port = args.port.unwrap_or(config.transfer.port);
    let dir = args
        .dir
        .clone()
        .unwrap_or_else(|| config.storage.save_directory.clone());
    std::fs::create_dir_all(&dir)?;
    serve_loop(ctx, &config, port, &dir, args.once).await
}

/// Background daemon: serve transfers until interrupted.
async fn daemon(ctx: &Ctx, args: DaemonArgs, path_override: Option<&str>) -> CliResult {
    match args.action {
        DaemonAction::Start { foreground } => {
            let config = load_config(path_override)?;
            let port = config.transfer.port;
            let dir = config.storage.save_directory.clone();
            std::fs::create_dir_all(&dir)?;
            // Backgrounding is not implemented yet: the daemon always runs in
            // the foreground. Say so honestly rather than silently ignoring
            // `--foreground` (previously a no-op) or claiming a detach mode
            // that doesn't exist.
            if !foreground {
                ctx.line(&ctx.dim(
                    "daemon: background mode is not implemented yet; running in the foreground (Ctrl-C to stop)",
                ));
            } else {
                ctx.line(&ctx.dim("daemon: serving transfers (Ctrl-C to stop)"));
            }
            serve_loop(ctx, &config, port, &dir, false).await
        }
        DaemonAction::Stop | DaemonAction::Status => Err(CliError::Unavailable(
            "daemon IPC (stop/status) is not implemented; run `daemon start` (it always runs in the foreground)".into(),
        )),
    }
}

/// This device's authentication material (crypto + trust + identity). Held as
/// `Arc`s so they can be shared into `PeerSession::open` (which takes owned
/// trait-object handles).
pub struct SecureCtx {
    pub enc: Arc<AeadCrypto>,
    pub trust: Arc<FsTrust>,
    pub ident: Identity,
}

impl SecureCtx {
    fn build(config: &EngineConfig) -> Result<Self, CliError> {
        let enc = AeadCrypto::new();
        let identity_path =
            std::path::Path::new(&config.storage.data_directory).join("identity.json");
        let ident = peerbeam_transfer::load_or_generate(
            &peerbeam_identity_fs::FsIdentity::open(identity_path),
            &enc,
            config.device.name.clone(),
        )?;
        let trust_path = std::path::Path::new(&config.storage.data_directory).join("trust.json");
        let trust = FsTrust::open(trust_path).map_err(CliError::from)?;
        Ok(Self {
            enc: Arc::new(enc),
            trust: Arc::new(trust),
            ident,
        })
    }
}

/// Select a route (via the RouteManager), authenticate, wrap in `SecureLink`,
/// and stream one file with progress. The route chosen is the manager's
/// concern — this function never sees it.
#[allow(clippy::too_many_arguments)]
async fn secure_send_file(
    ctx: &Ctx,
    quic: &Arc<QuicTransport>,
    routes: &RouteManager,
    device: &peerbeam_domain::entity::Device,
    sc: &SecureCtx,
    storage: &FsStorage,
    path: &str,
    name: &str,
    size: u64,
    chunk: u32,
) -> CliResult {
    let session =
        crate::session_transfer::dial(quic, routes, device, name, &sc.ident, &sc.enc, &sc.trust)
            .await?;
    let newly_trusted = session.newly_trusted;
    let peer_id = session.peer_id.clone();
    // Captured now (rather than read off `session` down by the JSON output)
    // because `session.close()` below takes `self` by value — after that
    // point `session` is moved and no longer accessible.
    let pairing_code = session.pairing_code.clone();
    if newly_trusted && !ctx.json {
        ctx.line(&ctx.dim(&format!("pinned new peer {peer_id}")));
        ctx.line(&format!("  pairing code: {}", ctx.bold(&pairing_code)));
    }

    let (ptx, mut prx) = mpsc::unbounded_channel();
    let bar = ctx.bar(size, name);
    let ctrl = TransferControl::new();
    let req = SendRequest {
        transfer_id: name.to_string(),
        name: name.to_string(),
        path: path.to_string(),
        size,
        chunk_size: chunk,
    };

    let handle = &session.handle;
    let send = async move {
        let r = send_file_on_session(handle, storage, req, &ctrl, &ptx, 3).await;
        drop(ptx);
        r
    };
    let pump = async move {
        while let Some(p) = prx.recv().await {
            bar.update(p.transferred_bytes);
        }
        bar.finish();
    };
    let (r, _) = tokio::join!(send, pump);
    r.map_err(CliError::from)?;
    session.close().await;

    if ctx.json {
        ctx.json_line(&json!({
            "event": "sent",
            "file": name,
            "bytes": size,
            "peer": peer_id,
            "newly_trusted": newly_trusted,
            "pairing_code": pairing_code,
            "transport": "peersession",
        }));
    } else {
        ctx.line(&ctx.green(&format!("sent {name}")));
    }
    Ok(())
}

/// Serve inbound QUIC connections as PeerSessions, accept each peer's transfer
/// channel, and receive one file or folder per connection into `dir`. Advertises
/// presence via discovery so senders find us.
async fn serve_loop(
    ctx: &Ctx,
    config: &EngineConfig,
    port: u16,
    dir: &str,
    once: bool,
) -> CliResult {
    use futures::StreamExt;

    let sc = SecureCtx::build(config)?;
    let quic = Arc::new(QuicTransport::new().map_err(CliError::from)?);
    let storage = FsStorage::new();
    let addr = std::net::SocketAddr::from((std::net::Ipv4Addr::UNSPECIFIED, port));
    let (local, mut incoming) = quic.serve_channels_on(addr).await.map_err(CliError::from)?;
    if ctx.json {
        ctx.json_line(&json!({
            "event": "listening",
            "addr": local.to_string(),
            "port": local.port(),
            "dir": dir,
        }));
    } else {
        ctx.line(&format!(
            "listening on {} — saving to {}",
            ctx.bold(&local.to_string()),
            dir
        ));
    }

    // Best-effort discoverability (so `send --to <name>` can find us). Identity
    // load failure here shouldn't abort an otherwise-working receive/daemon —
    // `SecureCtx::build` above already proved the identity file is usable, but
    // this stays defensive rather than assuming that. `engine` is carried past
    // the loop below so discovery can be stopped on the way out.
    let engine = build_engine(config.clone()).ok();
    if let (Some(engine), Ok(self_device)) = (&engine, me(config)) {
        let _ = engine.start_discovery(self_device).await;
    }

    loop {
        let qc = match incoming.next().await {
            Some(Ok(c)) => c,
            Some(Err(e)) => {
                if !ctx.json {
                    ctx.line(&ctx.dim(&format!("inbound rejected: {e}")));
                }
                continue;
            }
            None => break,
        };
        // Establish the PeerSession (runs the handshake internally).
        let mut session =
            match crate::session_transfer::accept(qc, &sc.ident, &sc.enc, &sc.trust).await {
                Ok(s) => s,
                Err(e) => {
                    if ctx.json {
                        ctx.json_line(
                            &json!({"event": "error", "message": format!("session failed: {e}")}),
                        );
                    } else {
                        ctx.line(&ctx.dim(&format!("session failed: {e}")));
                    }
                    continue;
                }
            };
        let newly_trusted = session.newly_trusted;
        let peer_id = session.peer_id.clone();
        if newly_trusted && !ctx.json {
            ctx.line(&ctx.dim(&format!("pinned new peer {peer_id}")));
            ctx.line(&format!(
                "  pairing code: {}",
                ctx.bold(&session.pairing_code)
            ));
        }

        // Optional first-contact pairing check: when the toggle is on and this
        // peer was just pinned, require the operator to confirm the pairing
        // code matches before accepting the transfer. Declining (or having no
        // way to prompt — JSON/non-interactive mode) un-pins the peer and
        // aborts this connection; discovery/serving continues.
        let answer = if config.device.require_pairing_confirmation && newly_trusted {
            if ctx.json {
                None // cannot prompt in JSON/non-interactive mode
            } else {
                ctx.line("Does the pairing code match the other device? [y/N]");
                let mut stdin = std::io::stdin().lock();
                read_confirm(&mut stdin)
            }
        } else {
            None
        };
        match pairing_gate(
            newly_trusted,
            config.device.require_pairing_confirmation,
            answer,
        ) {
            PairingGate::Proceed | PairingGate::Confirmed => {}
            PairingGate::Revoke => {
                // `peer_id` here is the plain-string form the CLI's `Session`
                // wrapper surfaces (see `session_transfer::Session::peer_id`);
                // `FsTrust::remove` takes the newtyped `DeviceId`. This is a
                // security-relevant un-pin: swallowing an `Err` here would
                // leave the peer trusted on disk while claiming otherwise —
                // the next connection would then find it already trusted,
                // skip the gate entirely (no `newly_trusted`), and silently
                // defeat the whole check. So the result is checked, and a
                // failed un-pin still fails the connection closed (it must
                // never fall through to receiving data either way).
                match sc
                    .trust
                    .remove(&peerbeam_domain::id::DeviceId::from(peer_id.clone()))
                {
                    Ok(_) => {
                        if ctx.json {
                            ctx.json_line(&json!({
                                "event": "error",
                                "message": "pairing code not confirmed; peer un-pinned",
                                "peer": peer_id,
                            }));
                        } else {
                            ctx.line(&ctx.red(
                                "pairing code not confirmed — un-pinned peer (possible MITM); transfer aborted",
                            ));
                        }
                    }
                    Err(e) => {
                        if ctx.json {
                            ctx.json_line(&json!({
                                "event": "error",
                                "message": format!(
                                    "pairing code not confirmed; FAILED to un-pin peer: {e}"
                                ),
                                "peer": peer_id,
                            }));
                        } else {
                            ctx.line(&ctx.red(&format!(
                                "pairing code not confirmed — FAILED to un-pin peer ({e}); transfer aborted regardless",
                            )));
                        }
                    }
                }
                session.close().await;
                if once {
                    break;
                }
                continue; // move on to the next inbound connection
            }
        }

        // Await the peer's transfer channel.
        let incoming_ch = match session.next_incoming().await {
            Some(c) => c,
            None => {
                if ctx.json {
                    ctx.json_line(&json!({"event": "error", "message": "closed before data"}));
                } else {
                    ctx.line(&ctx.red("transfer failed: closed before data"));
                }
                session.close().await;
                if once {
                    break;
                }
                continue;
            }
        };

        let storage_ref = &storage;
        let handle = &session.handle;
        let (ptx, mut prx) = mpsc::unbounded_channel();
        let ctrl = TransferControl::new();
        let recv = async move {
            let r = receive_on_channel(incoming_ch, handle, storage_ref, dir, &ctrl, &ptx).await;
            drop(ptx);
            r
        };
        // Human progress bar (created lazily once the total size is known).
        let pump = async move {
            let mut bar: Option<crate::output::Bar> = None;
            while let Some(p) = prx.recv().await {
                if !ctx.json {
                    let b = bar.get_or_insert_with(|| ctx.bar(p.total_bytes, "recv"));
                    b.update(p.transferred_bytes);
                }
            }
            if let Some(b) = bar {
                b.finish();
            }
        };
        let (r, _) = tokio::join!(recv, pump);
        let hist = history::path_for(&config.storage.data_directory);
        match r {
            Ok(ChannelReceived::File(rcv)) => {
                let saved = std::path::Path::new(dir)
                    .join(&rcv.name)
                    .to_string_lossy()
                    .into_owned();
                history::record(
                    &hist,
                    history::entry("receiving", &peer_id, &rcv.name, &saved, rcv.bytes, true),
                );
                if ctx.json {
                    ctx.json_line(&json!({
                        "event": "received",
                        "file": rcv.name,
                        "bytes": rcv.bytes,
                        "peer": peer_id,
                        "newly_trusted": newly_trusted,
                        "pairing_code": session.pairing_code.clone(),
                        "transport": "peersession",
                    }));
                } else {
                    ctx.line(&ctx.green(&format!("received {} ({} bytes)", rcv.name, rcv.bytes)));
                }
            }
            Ok(ChannelReceived::Folder(fr)) => {
                let saved = std::path::Path::new(dir)
                    .join(&fr.root)
                    .to_string_lossy()
                    .into_owned();
                history::record(
                    &hist,
                    history::entry("receiving", &peer_id, &fr.root, &saved, 0, true),
                );
                if ctx.json {
                    ctx.json_line(&json!({
                        "event": "received_folder",
                        "folder": fr.root,
                        "files": fr.files,
                        "peer": peer_id,
                        "newly_trusted": newly_trusted,
                        "pairing_code": session.pairing_code.clone(),
                        "transport": "peersession",
                    }));
                } else {
                    ctx.line(
                        &ctx.green(&format!("received folder {} ({} files)", fr.root, fr.files)),
                    );
                }
            }
            Err(e) => {
                history::record(
                    &hist,
                    history::entry("receiving", &peer_id, "(incomplete)", "", 0, false),
                );
                if ctx.json {
                    ctx.json_line(&json!({"event": "error", "message": e.to_string()}));
                } else {
                    ctx.line(&ctx.red(&format!("transfer failed: {e}")));
                }
            }
        }
        session.close().await;
        if once {
            break;
        }
    }

    if let Some(engine) = &engine {
        let _ = engine.stop_discovery().await;
    }
    Ok(())
}

/// Resolve `host:port` (or `ip:port`) to a socket address. Parsing is attempted
/// as-is first (so every address the resolver accepts still works); only on
/// failure do we craft a clearer hint for the two common footguns — a missing
/// port and an unbracketed IPv6 host.
fn resolve_addr(s: &str) -> Result<std::net::SocketAddr, CliError> {
    use std::net::ToSocketAddrs;
    if let Ok(mut addrs) = s.to_socket_addrs() {
        return addrs
            .next()
            .ok_or_else(|| CliError::Usage(format!("no address resolved for {s}")));
    }
    // Parsing failed — give a targeted hint rather than the opaque std error.
    if !s.starts_with('[') && !s.contains(':') {
        return Err(CliError::Usage(format!(
            "address '{s}' is missing a port — use <host>:<port>, e.g. {s}:49600"
        )));
    }
    if !s.starts_with('[') && s.matches(':').count() > 1 {
        // Looks like a bare IPv6 literal (multiple colons, no brackets).
        return Err(CliError::Usage(format!(
            "IPv6 address '{s}' must be bracketed — use [<addr>]:<port>"
        )));
    }
    if s.starts_with('[') {
        // Bracketed IPv6 that still failed: missing/invalid trailing port.
        return Err(CliError::Usage(format!(
            "IPv6 address '{s}' needs a valid trailing port — use [<addr>]:<port>"
        )));
    }
    Err(CliError::Usage(format!("bad address {s}: not resolvable")))
}

/// Resolve a `--to` query (or interactive pick) to a device index.
fn resolve_peer(
    ctx: &Ctx,
    candidates: &[(String, String)],
    to: &Option<String>,
) -> Result<usize, CliError> {
    match to {
        Some(q) => match resolve::resolve(candidates, q) {
            resolve::Resolution::Exact(i) => Ok(i),
            resolve::Resolution::NotFound => Err(CliError::NotFound(format!("device {q}"))),
            resolve::Resolution::Ambiguous(list) => {
                let names: Vec<String> = list.iter().map(|i| candidates[*i].1.clone()).collect();
                Err(CliError::Usage(format!(
                    "'{q}' matches multiple devices: {}",
                    names.join(", ")
                )))
            }
        },
        None => {
            if candidates.is_empty() {
                return Err(CliError::NotFound("no devices discovered".into()));
            }
            let names: Vec<String> = candidates.iter().map(|(_, n)| n.clone()).collect();
            prompt::select(ctx, "Select a device:", &names)
                .ok_or_else(|| CliError::Usage("specify --to <device>".into()))
        }
    }
}

/// `peerbeam history` — list (or clear) the persisted transfer history.
fn history_cmd(ctx: &Ctx, args: HistoryArgs, path_override: Option<&str>) -> CliResult {
    let config = load_config(path_override)?;
    let path = history::path_for(&config.storage.data_directory);

    if args.clear {
        history::clear(&path);
        if ctx.json {
            ctx.json_line(&json!({"event": "history_cleared"}));
        } else {
            ctx.line("history cleared");
        }
        return Ok(());
    }

    let entries = history::load(&path);
    let shown = entries.iter().rev().take(args.limit).collect::<Vec<_>>();
    if ctx.json {
        for e in shown.iter().rev() {
            ctx.json_line(&json!({
                "event": "history",
                "id": e.id, "direction": e.direction, "peer": e.peer,
                "file": e.file, "path": e.path, "bytes": e.bytes,
                "success": e.success, "at": e.at,
            }));
        }
        return Ok(());
    }
    if shown.is_empty() {
        ctx.line("no transfers yet");
        return Ok(());
    }
    for e in shown.iter().rev() {
        let arrow = if e.direction == "sending" { "->" } else { "<-" };
        let status = if e.success {
            ctx.green("ok")
        } else {
            ctx.red("failed")
        };
        ctx.line(&format!(
            "{}  {} {} {}  {} bytes  {}",
            e.at, arrow, e.peer, e.file, e.bytes, status
        ));
    }
    Ok(())
}

/// `peerbeam clipboard` — send text to a peer / print the last received text.
async fn clipboard(ctx: &Ctx, args: ClipboardArgs, path_override: Option<&str>) -> CliResult {
    match args.action {
        ClipboardAction::Get => clipboard_get(ctx, path_override),
        ClipboardAction::Send { to, addr, text } => {
            clipboard_send(ctx, to, addr, text, path_override).await
        }
    }
}

/// Print the newest received clipboard payload (the `peerbeam-clipboard-*.txt`
/// wire convention) from the save directory.
fn clipboard_get(ctx: &Ctx, path_override: Option<&str>) -> CliResult {
    let config = load_config(path_override)?;
    let dir = std::path::Path::new(&config.storage.save_directory);
    let mut newest: Option<(std::time::SystemTime, std::path::PathBuf)> = None;
    if let Ok(entries) = std::fs::read_dir(dir) {
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if name.starts_with("peerbeam-clipboard-") && name.ends_with(".txt") {
                if let Ok(meta) = e.metadata() {
                    let t = meta.modified().unwrap_or(std::time::UNIX_EPOCH);
                    if newest.as_ref().map_or(true, |(bt, _)| t > *bt) {
                        newest = Some((t, e.path()));
                    }
                }
            }
        }
    }
    let Some((_, path)) = newest else {
        return Err(CliError::NotFound("no received clipboard content".into()));
    };
    let text = std::fs::read_to_string(&path)?;
    if ctx.json {
        ctx.json_line(&json!({
            "event": "clipboard",
            "file": path.to_string_lossy(),
            "text": text,
        }));
    } else {
        // Raw to stdout so it pipes cleanly (e.g. `peerbeam clipboard get | wl-copy`).
        print!("{text}");
    }
    Ok(())
}

/// Send text to a peer using the clipboard wire convention. Text source
/// priority: argument > piped stdin > system clipboard.
async fn clipboard_send(
    ctx: &Ctx,
    to: Option<String>,
    addr: Option<String>,
    text: Option<String>,
    path_override: Option<&str>,
) -> CliResult {
    use std::io::{IsTerminal, Read};

    let text = if let Some(t) = text {
        t
    } else if !std::io::stdin().is_terminal() {
        let mut buf = String::new();
        std::io::stdin().read_to_string(&mut buf)?;
        buf
    } else {
        match arboard::Clipboard::new().and_then(|mut c| c.get_text()) {
            Ok(t) => t,
            Err(e) => {
                return Err(CliError::Usage(format!(
                    "no text given and the system clipboard is unavailable ({e}) — \
                     pass TEXT or pipe stdin: echo hi | peerbeam clipboard send --to <peer>"
                )))
            }
        }
    };
    if text.trim().is_empty() {
        return Err(CliError::Usage(
            "nothing to send: clipboard/stdin is empty".into(),
        ));
    }

    // Stage as a wire-convention temp file so receivers offer one-tap Copy.
    let tmp = std::env::temp_dir().join(format!(
        "peerbeam-clipboard-{}.txt",
        chrono::Utc::now().timestamp_millis()
    ));
    std::fs::write(&tmp, &text)?;

    let send_args = SendArgs {
        paths: vec![tmp.to_string_lossy().into_owned()],
        to,
        addr,
    };
    let result = send(ctx, send_args, path_override).await;
    let _ = std::fs::remove_file(&tmp);
    result
}

// ── in-process link for benchmark ───────────────────────────────

struct MemLink {
    tx: mpsc::Sender<Frame>,
    rx: mpsc::Receiver<Frame>,
}

impl MemLink {
    fn pair(cap: usize) -> (MemLink, MemLink) {
        let (a_tx, b_rx) = mpsc::channel(cap);
        let (b_tx, a_rx) = mpsc::channel(cap);
        (
            MemLink { tx: a_tx, rx: a_rx },
            MemLink { tx: b_tx, rx: b_rx },
        )
    }
}

#[async_trait]
impl Link for MemLink {
    async fn send_frame(&mut self, frame: Frame) -> DResult<()> {
        self.tx
            .send(frame)
            .await
            .map_err(|_| peerbeam_domain::DomainError::Connection("peer closed".into()))
    }
    async fn recv_frame(&mut self) -> DResult<Option<Frame>> {
        Ok(self.rx.recv().await)
    }
    async fn close(&mut self) -> DResult<()> {
        Ok(())
    }
}

// ── unit tests: config dotted-key navigation ────────────────────
#[cfg(test)]
mod config_key_tests {
    use super::{navigate, parse_value, render_scalar, set_path};
    use serde_json::json;

    fn sample() -> serde_json::Value {
        json!({
            "device": { "name": "box", "auto_accept_trusted": false },
            "transfer": { "chunk_size": 1048576 },
        })
    }

    #[test]
    fn navigate_reaches_nested_scalar() {
        let v = sample();
        assert_eq!(navigate(&v, "device.name").unwrap(), &json!("box"));
        assert_eq!(
            navigate(&v, "transfer.chunk_size").unwrap(),
            &json!(1048576)
        );
    }

    #[test]
    fn navigate_returns_none_for_unknown_key() {
        let v = sample();
        assert!(navigate(&v, "device.nope").is_none());
        assert!(navigate(&v, "missing.section").is_none());
        // Descending into a scalar is a miss, not a panic.
        assert!(navigate(&v, "device.name.deeper").is_none());
    }

    #[test]
    fn set_path_updates_existing_leaf() {
        let mut v = sample();
        set_path(&mut v, "device.name", json!("renamed")).unwrap();
        assert_eq!(navigate(&v, "device.name").unwrap(), &json!("renamed"));
    }

    #[test]
    fn set_path_rejects_unknown_leaf() {
        let mut v = sample();
        assert!(set_path(&mut v, "device.unknown", json!(1)).is_err());
    }

    #[test]
    fn set_path_rejects_unknown_parent() {
        let mut v = sample();
        assert!(set_path(&mut v, "ghost.name", json!(1)).is_err());
    }

    #[test]
    fn set_path_rejects_descending_into_scalar() {
        let mut v = sample();
        // `device.name` is a string, not an object — cannot set a child.
        assert!(set_path(&mut v, "device.name.x", json!(1)).is_err());
    }

    #[test]
    fn parse_value_infers_json_types_and_falls_back_to_string() {
        assert_eq!(parse_value("42"), json!(42));
        assert_eq!(parse_value("true"), json!(true));
        assert_eq!(parse_value("1.5"), json!(1.5));
        // Bare, unquoted text is not valid JSON → treated as a string.
        assert_eq!(parse_value("peerbeam=debug"), json!("peerbeam=debug"));
        assert_eq!(parse_value("MyLaptop"), json!("MyLaptop"));
    }

    #[test]
    fn render_scalar_unquotes_strings_but_stringifies_others() {
        assert_eq!(render_scalar(&json!("hi")), "hi");
        assert_eq!(render_scalar(&json!(7)), "7");
        assert_eq!(render_scalar(&json!(true)), "true");
    }
}

// ── M8: additive PeerSession CLI commands ───────────────────────────────────
#[cfg(test)]
mod session_cli_tests {
    use super::dispatch;
    use crate::cli::{ChannelsArgs, Cli, Command, SessionAction, SessionArgs};
    use crate::output::Ctx;

    fn quiet_ctx() -> Ctx {
        // json=true so output is machine-readable; no colour, non-interactive.
        Ctx::new(true, true, 0, true, true)
    }

    #[tokio::test]
    async fn diagnostic_commands_dispatch_ok() {
        let ctx = quiet_ctx();
        // Each new command presents an (empty, standalone) diagnostics snapshot
        // without error — proving the presentation wiring over the engine's
        // SessionDiagnostics.
        for cmd in [
            Command::Session(SessionArgs {
                action: SessionAction::List,
            }),
            Command::Session(SessionArgs {
                action: SessionAction::Show {
                    id: "deadbeefdeadbeefdeadbeefdeadbeef".into(),
                },
            }),
            Command::Session(SessionArgs {
                action: SessionAction::Stats,
            }),
            Command::Session(SessionArgs {
                action: SessionAction::Watch,
            }),
            Command::Channels(ChannelsArgs {
                session: None,
                watch: false,
            }),
            Command::Channels(ChannelsArgs {
                session: Some("deadbeefdeadbeefdeadbeefdeadbeef".into()),
                watch: false,
            }),
            Command::Transfers,
            Command::Migration,
            Command::Recovery,
            Command::Diagnostics,
        ] {
            assert!(dispatch(cmd, &ctx, None).await.is_ok());
        }
    }

    #[test]
    fn cli_parses_new_and_existing_subcommands() {
        use clap::Parser;
        // New additive subcommands parse.
        for args in [
            vec!["peerbeam", "session", "list"],
            vec!["peerbeam", "session", "show", "abc"],
            vec!["peerbeam", "session", "watch"],
            vec!["peerbeam", "session", "stats"],
            vec!["peerbeam", "channels", "--session", "abc", "--watch"],
            vec!["peerbeam", "transfers"],
            vec!["peerbeam", "migration"],
            vec!["peerbeam", "recovery"],
            vec!["peerbeam", "diagnostics"],
        ] {
            assert!(Cli::try_parse_from(&args).is_ok(), "should parse: {args:?}");
        }
        // Existing commands still parse unchanged (backward compatibility).
        for args in [
            vec!["peerbeam", "status"],
            vec!["peerbeam", "send", "file.txt", "--to", "box"],
            vec!["peerbeam", "receive", "--once"],
            vec!["peerbeam", "history", "--limit", "5"],
        ] {
            assert!(Cli::try_parse_from(&args).is_ok(), "regression: {args:?}");
        }
    }
}

#[cfg(test)]
mod resolve_addr_tests {
    use super::resolve_addr;

    #[test]
    fn parses_ipv4_and_bracketed_ipv6() {
        assert!(resolve_addr("127.0.0.1:49600").is_ok());
        assert!(resolve_addr("[::1]:49600").is_ok());
    }

    #[test]
    fn accepts_unbracketed_ipv6_with_port_std_allows() {
        // std splits on the last colon, so these resolve — must NOT be
        // rejected by the friendly-error path (regression guard).
        assert!(resolve_addr("2001:db8::1:8080").is_ok());
        assert!(resolve_addr("::1:8080").is_ok());
        assert!(resolve_addr("fe80::1:80").is_ok());
    }

    #[test]
    fn missing_port_is_a_clear_error() {
        let err = resolve_addr("192.168.1.5").unwrap_err().to_string();
        assert!(err.contains("missing a port"), "got: {err}");
    }

    #[test]
    fn bare_ipv6_asks_for_brackets() {
        let err = resolve_addr("fe80::1").unwrap_err().to_string();
        assert!(err.contains("bracketed"), "got: {err}");
    }
}

#[cfg(test)]
mod pairing_tests {
    use super::{pairing_gate, read_confirm, PairingGate};
    use std::io::Cursor;

    #[test]
    fn gate_proceeds_when_not_first_contact_or_toggle_off() {
        assert!(matches!(
            pairing_gate(false, true, None),
            PairingGate::Proceed
        ));
        assert!(matches!(
            pairing_gate(true, false, None),
            PairingGate::Proceed
        ));
    }

    #[test]
    fn gate_confirms_on_yes_and_revokes_on_no_or_no_answer() {
        assert!(matches!(
            pairing_gate(true, true, Some(true)),
            PairingGate::Confirmed
        ));
        assert!(matches!(
            pairing_gate(true, true, Some(false)),
            PairingGate::Revoke
        ));
        // No answer available (non-interactive / EOF) -> safe default: revoke.
        assert!(matches!(
            pairing_gate(true, true, None),
            PairingGate::Revoke
        ));
    }

    #[test]
    fn read_confirm_parses_yes_no_and_eof() {
        assert_eq!(read_confirm(&mut Cursor::new(b"y\n")), Some(true));
        assert_eq!(read_confirm(&mut Cursor::new(b"Yes\n")), Some(true));
        assert_eq!(read_confirm(&mut Cursor::new(b"n\n")), Some(false));
        assert_eq!(read_confirm(&mut Cursor::new(b"\n")), Some(false));
        assert_eq!(read_confirm(&mut Cursor::new(b"")), None);
    }
}

#[cfg(test)]
mod clamp_chunk_size_tests {
    use super::clamp_chunk_size;

    #[test]
    fn passes_through_ordinary_values() {
        assert_eq!(clamp_chunk_size(1_048_576), 1_048_576);
    }

    #[test]
    fn zero_clamps_up_to_one_not_zero() {
        // `.max(1)` alone is correct here — regression guard for the trivial case.
        assert_eq!(clamp_chunk_size(0), 1);
    }

    #[test]
    fn exact_multiple_of_2_pow_32_does_not_truncate_to_zero() {
        // This is the bug: `(4_294_967_296u64.max(1)) as u32 == 0` because
        // `.max(1)` ran BEFORE the truncating cast, so the guard never saw
        // the post-cast value. Clamping into u32 range first must yield the
        // maximum representable chunk size, never 0.
        let two_pow_32: u64 = 1u64 << 32;
        assert_eq!(clamp_chunk_size(two_pow_32), u32::MAX);
        assert_ne!(clamp_chunk_size(two_pow_32), 0);
    }

    #[test]
    fn values_above_u32_max_clamp_to_u32_max() {
        assert_eq!(clamp_chunk_size(u64::MAX), u32::MAX);
    }

    #[test]
    fn u32_max_itself_is_unchanged() {
        assert_eq!(clamp_chunk_size(u32::MAX as u64), u32::MAX);
    }
}
