//! Command implementations + dispatch.

use std::time::{Duration, Instant};

use async_trait::async_trait;
use clap::CommandFactory;
use serde_json::json;
use tokio::sync::mpsc;

use peerbeam_config::EngineConfig;
use peerbeam_crypto::AeadCrypto;
use peerbeam_domain::error::Result as DResult;
use peerbeam_domain::port::{EncryptionProvider, Frame, Link, Nonce};
use peerbeam_engine::ManagedDevice;
use peerbeam_storage_fs::FsStorage;
use peerbeam_transfer::{receive_file, send_file, SendRequest, TransferControl};

use crate::cli::*;
use crate::engine::{build_engine, config_path, me};
use crate::exit::{CliError, CliResult};
use crate::output::Ctx;
use crate::{prompt, resolve};

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
        Command::Receive(_) => gated("receive"),
        Command::Clipboard(_) => gated("clipboard"),
        Command::History(_) => gated("history"),
        Command::Daemon(_) => gated("daemon"),
    }
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
            let value = serde_json::to_value(&cfg).unwrap();
            if ctx.json {
                ctx.json_line(&value);
            } else {
                ctx.line(&serde_json::to_string_pretty(&value).unwrap());
            }
        }
        ConfigAction::Get { key } => {
            let cfg = load_config(path_override)?;
            let value = serde_json::to_value(&cfg).unwrap();
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
            let mut root = serde_json::to_value(&cfg).unwrap();
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

    // mDNS daemon.
    match peerbeam_discovery_mdns::MdnsDiscovery::new(crate::engine::device_id()) {
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
    rs.map_err(CliError::from)?;
    rr.map_err(CliError::from)?;
    let secs = start.elapsed().as_secs_f64();
    let mbs = size_mib as f64 / secs;

    let _ = std::fs::remove_dir_all(&dir);
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
    let engine = build_engine(config.clone());
    if engine.start_discovery(me(&config)).await.is_err() {
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
        let engine = build_engine(config.clone());
        let mut changes = engine.device_changes();
        engine.start_discovery(me(&config)).await?;
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
    } else if let C::Added(m) = change {
        ctx.line(&format!("{} {}", ctx.green("+"), m.device.name));
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
    let enc = AeadCrypto::new();
    let fp = enc.fingerprint(&enc.generate_keypair().public).0;
    if ctx.json {
        ctx.json_line(&json!({
            "device_name": config.device.name,
            "save_directory": config.storage.save_directory,
            "fingerprint": fp,
            "daemon_running": false,
            "providers": ["udp", "mdns", "tailscale"],
        }));
    } else {
        ctx.line(&format!("{}  {}", ctx.bold("Device:"), config.device.name));
        ctx.line(&format!(
            "{}  {}",
            ctx.bold("Save to:"),
            config.storage.save_directory
        ));
        ctx.line(&format!("{} {}…", ctx.bold("Fingerprint:"), &fp[..16]));
        ctx.line(&format!(
            "{}  {}",
            ctx.bold("Providers:"),
            "udp, mdns, tailscale"
        ));
        ctx.line(&format!(
            "{}  {}",
            ctx.bold("Daemon:"),
            ctx.dim("not running")
        ));
    }
    Ok(())
}

fn completions(shell: clap_complete::Shell) -> CliResult {
    let mut cmd = Cli::command();
    clap_complete::generate(shell, &mut cmd, "peerbeam", &mut std::io::stdout());
    Ok(())
}

async fn send(ctx: &Ctx, args: SendArgs, path_override: Option<&str>) -> CliResult {
    // Validate paths first.
    for p in &args.paths {
        if !std::path::Path::new(p).exists() {
            return Err(CliError::NotFound(format!("path {p}")));
        }
    }

    let config = load_config(path_override)?;
    let devices = snapshot(config, 2).await;
    let candidates: Vec<(String, String)> = devices
        .iter()
        .map(|m| (m.device.id.to_string(), m.device.name.clone()))
        .collect();

    let index = match &args.to {
        Some(q) => match resolve::resolve(&candidates, q) {
            resolve::Resolution::Exact(i) => i,
            resolve::Resolution::NotFound => return Err(CliError::NotFound(format!("device {q}"))),
            resolve::Resolution::Ambiguous(list) => {
                let names: Vec<String> = list.iter().map(|i| candidates[*i].1.clone()).collect();
                return Err(CliError::Usage(format!(
                    "'{q}' matches multiple devices: {}",
                    names.join(", ")
                )));
            }
        },
        None => {
            if candidates.is_empty() {
                return Err(CliError::NotFound("no devices discovered".into()));
            }
            let names: Vec<String> = candidates.iter().map(|(_, n)| n.clone()).collect();
            match prompt::select(ctx, "Select a device:", &names) {
                Some(i) => i,
                None => return Err(CliError::Usage("specify --to <device>".into())),
            }
        }
    };

    let peer = &candidates[index].1;
    if !prompt::confirm(
        ctx,
        &format!("Send {} path(s) to {peer}?", args.paths.len()),
        true,
    ) {
        return Err(CliError::Cancelled);
    }
    Err(CliError::Unavailable(format!(
        "resolved peer '{peer}', but the network transport isn't built yet"
    )))
}

fn gated(what: &str) -> CliResult {
    Err(CliError::Unavailable(format!(
        "`{what}` needs the transport bridge (QUIC provider), not built yet"
    )))
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
