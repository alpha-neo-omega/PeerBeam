//! Command implementations + dispatch.

use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use clap::CommandFactory;
use serde_json::{json, Value};
use tokio::sync::mpsc;

use peerbeam_config::EngineConfig;
use peerbeam_crypto::AeadCrypto;
use peerbeam_domain::error::Result as DResult;
use peerbeam_domain::id::DeviceId;
use peerbeam_domain::port::{EncryptionProvider, Frame, Link, Nonce};
use peerbeam_engine::{ManagedDevice, RouteManager};
use peerbeam_storage_fs::FsStorage;
use peerbeam_transfer::{
    accept_pipe, peek_incoming_meta, receive_file, receive_on_channel, send_file,
    send_file_on_session, send_folder_on_session, ChannelReceived, FolderSendRequest, Identity,
    SendRequest, TransferControl, TransferOutcome,
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
        Command::CheckUpdates => check_updates(ctx).await,
        Command::Benchmark(a) => benchmark(ctx, a).await,
        Command::Discover(a) => discover(ctx, a, cfg_override.as_deref()).await,
        Command::List(a) => list(ctx, a, cfg_override.as_deref()).await,
        Command::Status => status(ctx, cfg_override.as_deref()),
        Command::Completions { shell } => completions(shell),
        Command::Send(a) => send_files(ctx, a, cfg_override.as_deref()).await,
        Command::Receive(a) => receive(ctx, a, cfg_override.as_deref()).await,
        Command::Clipboard(a) => clipboard(ctx, a, cfg_override.as_deref()).await,
        Command::Chat(a) => crate::chat::chat(ctx, a.action, cfg_override.as_deref()).await,
        Command::Pipe(a) => crate::pipe::pipe(ctx, a, cfg_override.as_deref()).await,
        Command::History(a) => history_cmd(ctx, a, cfg_override.as_deref()),
        Command::Trust(a) => crate::trust::trust(ctx, a.action, cfg_override.as_deref()),
        Command::Rules(a) => crate::rules::rules(ctx, a.action, cfg_override.as_deref()),
        Command::Notes(a) => crate::notes::notes(ctx, a.action, cfg_override.as_deref()).await,
        Command::Space(a) => crate::spaces::space(ctx, a.action, cfg_override.as_deref()).await,
        Command::Wake(a) => crate::wake::wake(ctx, a.action, cfg_override.as_deref()),
        Command::Ring(a) => crate::presence::ring(ctx, a, cfg_override.as_deref()).await,
        Command::Timeline(a) => timeline_cmd(ctx, a, cfg_override.as_deref()),
        Command::Watch(a) => crate::watch::watch(ctx, a, cfg_override.as_deref()).await,
        Command::Browse(a) => crate::browse::browse(ctx, a, cfg_override.as_deref()).await,
        Command::Sync(a) => crate::browse::sync(ctx, a, cfg_override.as_deref()).await,
        Command::Snippet(a) => crate::chat::snippet(ctx, a, cfg_override.as_deref()).await,
        Command::Pair(a) => crate::pair::pair(ctx, a, cfg_override.as_deref()).await,
        Command::Logs(a) => crate::logs::logs(ctx, a, cfg_override.as_deref()),
        Command::Daemon(a) => daemon(ctx, a, cfg_override.as_deref()).await,
        Command::Session(a) => session_cmd(ctx, a).await,
        Command::Channels(a) => channels_cmd(ctx, a).await,
        Command::Transfers(a) => {
            crate::transfers::transfers(ctx, a.action, cfg_override.as_deref()).await
        }
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

/// Say that something went wrong **without** ending the command, on stderr.
///
/// For a failure a command survives but must not hide: a chat row that could
/// not be updated after the file it describes has already landed, a discovery
/// window that never opened while the work it was helping with is already done.
/// The CLI installs no `tracing` subscriber, so there is no log for these to
/// fall into — printing is the only trace available.
///
/// Deliberately **not** the `{"event": "error"}` lines `receive`/`pipe` put on
/// *stdout*: those are part of a documented event stream a script consumes.
/// This is a diagnostic, and a diagnostic that lands in `chat history --json`'s
/// output would corrupt the one document that command promises. `--quiet`
/// silences it like every other human line.
pub(crate) fn report_problem(ctx: &Ctx, msg: &str) {
    if ctx.json {
        ctx.json_note(&json!({"event": "error", "message": msg}));
    } else {
        ctx.note(&ctx.err_dim(msg));
    }
}

/// Print a diagnostics value as JSON (always machine-readable, pretty in a TTY).
pub(crate) fn present(ctx: &Ctx, value: &serde_json::Value) -> CliResult {
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

/// Load the effective config — and, as a side effect, publish the presence
/// opt-in it carries.
///
/// Configuring presence here rather than at each command means no command that
/// can dial or accept is able to forget it: every one of them loads config
/// first. The alternative — a `configure` call per command — is exactly the
/// shape of the bug that has already shipped twice in this codebase, where one
/// of several call sites was missed and behaviour silently depended on which
/// path a peer reached.
///
/// Publishing the value is not consent: `share_presence` defaults to false, and
/// the trusted-only gate is not configurable at all.
pub(crate) fn load_config(override_path: Option<&str>) -> Result<EngineConfig, CliError> {
    let config = EngineConfig::load_or_default(&config_path(override_path))
        .map_err(|e| CliError::Other(format!("config: {e}")))?;
    crate::presence::configure(&config);
    // Recording is decided here, once: the sink is handed a store only when the
    // user opted in, so no call site has to remember to ask.
    crate::clipboard::configure_history(if config.device.clipboard_history {
        SecureCtx::build(&config)
            .ok()
            .map(|sc| clip_history_store(&config, &sc.enc, &sc.ident))
    } else {
        None
    });
    Ok(config)
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

/// Ask the release feed what the newest published version is.
///
/// Deliberately thin: it prints what it was told and stops. Amendment A1 permits
/// a check, not an updater — nothing here downloads, installs, or changes
/// behaviour on the strength of the answer.
///
/// Being unable to reach the feed is **not an error exit**. Offline is an
/// ordinary state for this app, and a script that runs `peerbeam check-updates`
/// on a machine with no route out should not fail because of it; the JSON says
/// `reachable: false` and the human text says so plainly.
async fn check_updates(ctx: &Ctx) -> CliResult {
    let current = env!("CARGO_PKG_VERSION");
    // Awaited on the dispatcher's runtime rather than building one. An earlier
    // version called `block_on` here and panicked outright — "Cannot start a
    // runtime from within a runtime" — because `dispatch` is already async.
    let outcome = peerbeam_update::check().await;

    match outcome {
        Ok(Some(release)) => {
            let newer = peerbeam_update::is_newer(&release.version, current);
            if ctx.json {
                ctx.json_line(&serde_json::json!({
                    "event": "update_check",
                    "reachable": true,
                    "current": current,
                    "latest": release.version,
                    "update_available": newer,
                    "url": release.url,
                }));
            } else if newer {
                ctx.line(&format!(
                    "{} is available — you have {}\n{}",
                    release.version, current, release.url
                ));
            } else {
                ctx.line(&format!("{current} is the newest release"));
            }
            Ok(())
        }
        Ok(None) => {
            if ctx.json {
                ctx.json_line(&serde_json::json!({
                    "event": "update_check",
                    "reachable": true,
                    "current": current,
                    "latest": serde_json::Value::Null,
                    "update_available": false,
                }));
            } else {
                ctx.line("no releases published yet");
            }
            Ok(())
        }
        Err(e) => {
            if ctx.json {
                ctx.json_line(&serde_json::json!({
                    "event": "update_check",
                    "reachable": false,
                    "current": current,
                    "reason": e.to_string(),
                }));
            } else {
                ctx.line(&format!(
                    "could not check for updates — {e}\nyou have {current}; see {}",
                    peerbeam_update::RELEASES_PAGE
                ));
            }
            Ok(())
        }
    }
}

/// One row of `doctor`'s report: name, `pass`/`warn`/`fail`, and the detail.
type Check = (String, &'static str, String);

/// The config row `doctor` reports, and the config every check after it runs
/// against.
///
/// `doctor` used to open with `load_config(..).unwrap_or_default()`. A
/// `config.json` with a stray comma silently *became* the defaults, so the one
/// command whose entire job is naming a broken setup had nothing at all to say
/// about the most likely fault — and every check below it then answered for a
/// config the user does not have: a save directory they never chose, a port
/// they never set, an identity in a data directory they never named.
///
/// Substituting the defaults is still right — a broken config must not cost the
/// user the other eight answers. What changes is that the substitution is
/// *reported*, and that `doctor` exits non-zero for it like any other failed
/// check. The CLI installs no `tracing` subscriber, so a log line here would go
/// nowhere: the report is the only place this can be said.
fn config_check(path_override: Option<&str>) -> (EngineConfig, Check) {
    let path = config_path(path_override);
    match load_config(path_override) {
        // A file that is simply not there is not a fault: `load_or_default`
        // treats NotFound as the defaults by design, and a fresh install has no
        // `config.json` at all — so this only ever fails on a file that exists
        // and cannot be used. Saying which of the two it is *is* the diagnosis.
        Ok(cfg) => {
            let detail = if path.exists() {
                path.to_string_lossy().into_owned()
            } else {
                format!("{} (none yet — using defaults)", path.display())
            };
            (cfg, ("Config".into(), "pass", detail))
        }
        Err(e) => (
            EngineConfig::default(),
            ("Config".into(), "fail", e.to_string()),
        ),
    }
}

fn doctor(ctx: &Ctx, path_override: Option<&str>) -> CliResult {
    // First row, and the source of `cfg` for every row after it — so what
    // `doctor` reports and what `doctor` uses can never disagree.
    let (cfg, config_row) = config_check(path_override);
    let mut checks: Vec<Check> = vec![config_row];

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

fn writable_check(name: &str, dir: &str) -> Check {
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

/// Listen for `secs` and return what discovery found.
///
/// **A failure here is not an empty network.** This used to fold all three of
/// its failures — no usable identity (`build_engine`, `me`) and discovery that
/// would not start (a port already held, no usable interface) — into
/// `Vec::new()`. Every caller reads that as "nothing out there": `peerbeam
/// list` printed "no devices found", `chat send` reported the peer was not
/// reachable, `pipe --from` silently matched nobody. Each of those is a claim
/// about the user's network made out of a failure on this machine, and it is
/// the claim they act on — by looking at the router, not at us.
///
/// The CLI installs no `tracing` subscriber, so a log line here would go
/// nowhere at all: propagating is the only way the reason reaches anybody. The
/// three best-effort callers (a reaction, a read receipt, resolving a name that
/// may not need discovery at all) print it and carry on instead — see their
/// own notes.
pub(crate) async fn snapshot(
    config: EngineConfig,
    secs: u64,
) -> Result<Vec<ManagedDevice>, CliError> {
    let engine = build_engine(config.clone())?;
    let self_device = me(&config)?;
    engine.start_discovery(self_device).await?;
    tokio::time::sleep(Duration::from_secs(secs)).await;
    let devices = engine.devices();
    // Deliberately not propagated: the answer is already in hand and this
    // engine is about to be dropped, so failing the command *after* it
    // succeeded would report a problem the user has no stake in and cannot act
    // on. The three failures above are the ones that decide the answer.
    let _ = engine.stop_discovery().await;
    Ok(devices)
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

    let devices = snapshot(config, args.timeout).await?;
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
    let mut devices = snapshot(config, 2).await?;
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

    // This device's own status — the same collection a heartbeat would send,
    // so `status` shows exactly what the opt-in would reveal. Computing it is
    // purely local; nothing here puts anything on the wire.
    let own = peerbeam_presence::collect(
        &config.storage.save_directory,
        None, // a one-shot command has no session, so no route to characterise
        env!("CARGO_PKG_VERSION"),
    );
    // Peers, from the live registry. A one-shot `status` holds no sessions, so
    // this is normally empty — that is presence working as designed, not a
    // gap: presence is live state and a fresh process starts with none. It
    // populates for a long-running command that holds sessions open.
    let peers: Vec<Value> = crate::presence::registry()
        .snapshot()
        .iter()
        .map(|(id, e)| presence_row(id, e))
        .collect();

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
            "share_presence": config.device.share_presence,
            "presence": {
                "battery_percent": own.battery_percent,
                "charging": own.charging,
                "storage_free_bytes": own.storage_free_bytes,
                "app_version": own.app_version,
            },
            "peers": peers,
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
        // What this device measures about itself, and whether any of it leaves.
        // Shown even when sharing is off so the toggle's effect is visible
        // before it is flipped.
        ctx.line(&format!("{} {}", ctx.bold("Status:"), own_summary(&own)));
        ctx.line(&format!(
            "{} {}",
            ctx.bold("Sharing:"),
            if config.device.share_presence {
                ctx.green("on (trusted devices only)")
            } else {
                ctx.dim("off — device status is shared with nobody")
            }
        ));
        if peers.is_empty() {
            ctx.line(&ctx.dim(
                "Peers:    no shared status (presence is live; a one-shot command holds no session)",
            ));
        } else {
            ctx.line(&ctx.bold("Peers:"));
            for p in &peers {
                ctx.line(&format!("  {}", peer_summary(p)));
            }
        }
    }
    Ok(())
}

/// One peer's shared status as a JSON row.
///
/// Absent fields are **omitted**, never `null` or `0`: a desktop with no
/// battery must be distinguishable from one at 0%, and a script reading this
/// contract should have to ask whether the key is there.
fn presence_row(id: &peerbeam_domain::id::DeviceId, e: &peerbeam_presence::PeerStatus) -> Value {
    let s = &e.status;
    let mut o = serde_json::Map::new();
    o.insert("device_id".into(), json!(id.0));
    if let Some(v) = s.battery_percent {
        o.insert("battery_percent".into(), json!(v));
    }
    if let Some(v) = s.charging {
        o.insert("charging".into(), json!(v));
    }
    if let Some(v) = s.storage_free_bytes {
        o.insert("storage_free_bytes".into(), json!(v));
    }
    if let Some(v) = &s.network {
        o.insert("network".into(), json!(v));
    }
    if let Some(v) = &s.app_version {
        o.insert("app_version".into(), json!(v));
    }
    // `sent_at` is the peer's own clock and is not synchronised with ours;
    // `age_seconds` counts from OUR receipt time and is what to display.
    o.insert("sent_at".into(), json!(s.sent_at));
    o.insert(
        "age_seconds".into(),
        json!(e.age_seconds(chrono::Utc::now())),
    );
    Value::Object(o)
}

/// Human-readable bytes, e.g. `12.3 GB`. Decimal units, matching the rest of
/// the CLI's size rendering.
pub(crate) fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = bytes as f64;
    let mut i = 0;
    while v >= 1000.0 && i < UNITS.len() - 1 {
        v /= 1000.0;
        i += 1;
    }
    if i == 0 {
        format!("{bytes} B")
    } else {
        format!("{v:.1} {}", UNITS[i])
    }
}

/// This device's own measurements, joined for one line. Fields it cannot
/// measure are left out rather than shown as zero.
fn own_summary(s: &peerbeam_presence::Status) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(p) = s.battery_percent {
        parts.push(match s.charging {
            Some(true) => format!("battery {p}% (charging)"),
            _ => format!("battery {p}%"),
        });
    }
    if let Some(b) = s.storage_free_bytes {
        parts.push(format!("{} free", human_bytes(b)));
    }
    if let Some(v) = &s.app_version {
        parts.push(format!("v{v}"));
    }
    if parts.is_empty() {
        "nothing measurable on this platform".to_string()
    } else {
        parts.join(" · ")
    }
}

/// One peer row, rendered from the JSON so the human and JSON views cannot
/// disagree about which fields a peer actually shared.
fn peer_summary(row: &Value) -> String {
    let id = row["device_id"].as_str().unwrap_or("?");
    let mut parts: Vec<String> = Vec::new();
    if let Some(p) = row.get("battery_percent").and_then(Value::as_u64) {
        parts.push(match row.get("charging").and_then(Value::as_bool) {
            Some(true) => format!("battery {p}% (charging)"),
            _ => format!("battery {p}%"),
        });
    }
    if let Some(b) = row.get("storage_free_bytes").and_then(Value::as_u64) {
        parts.push(format!("{} free", human_bytes(b)));
    }
    if let Some(n) = row.get("network").and_then(Value::as_str) {
        parts.push(n.to_string());
    }
    if let Some(v) = row.get("app_version").and_then(Value::as_str) {
        parts.push(format!("v{v}"));
    }
    let age = row.get("age_seconds").and_then(Value::as_u64).unwrap_or(0);
    if parts.is_empty() {
        // Identity and reachability, not empty gauges.
        format!("{id} — status not shared")
    } else {
        format!("{id} — {} ({age}s ago)", parts.join(" · "))
    }
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
pub(crate) fn clamp_chunk_size(chunk_size: u64) -> u32 {
    chunk_size.clamp(1, u32::MAX as u64) as u32
}

/// Send some paths, for a caller that already has a `SendArgs`.
///
/// Exposed so `watch` can reuse the one send path rather than growing a second
/// one that could drift from it.
pub(crate) async fn send_paths(
    ctx: &Ctx,
    args: SendArgs,
    path_override: Option<&str>,
) -> CliResult {
    send_files(ctx, args, path_override).await
}

/// A duration in the words people use for one.
///
/// Shared with `trust list`, which prints how long a time-limited approval has
/// left: the CLI must say a duration the same way everywhere, or `--for 2h` and
/// the row it produces would not obviously be about the same thing.
pub(crate) fn humantime(d: std::time::Duration) -> String {
    let secs = d.as_secs();
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m", secs / 60)
    } else if secs < 86_400 {
        format!("{}h{:02}m", secs / 3600, (secs % 3600) / 60)
    } else {
        // Days once there are any. A week-long trust window rendered as
        // `168h00m` is a number nobody reads as a week.
        format!("{}d{:02}h", secs / 86_400, (secs % 86_400) / 3600)
    }
}

/// A duration in the words people **type** for one: `45s`, `30m`, `2h`, `7d`.
///
/// The inverse of [`humantime`], on purpose and with the same four units: a
/// vocabulary the CLI already prints is one a person can guess without reading
/// `--help`. Used by `trust approve --for`.
///
/// Deliberately narrow. No compound forms (`1h30m`), because supporting them
/// means deciding what `1h30` and `1h30s` mean; and **no bare numbers**, because
/// `--for 30` is thirty of something the reader and the writer have to agree on
/// out of band, and on this flag the two obvious candidates — half a minute and
/// half an hour — differ by sixty times in how long a stranger keeps access.
///
/// Zero and negative are refused rather than rounded to "already over": a window
/// that has expired before it is written is not what anyone meant by approving,
/// and the error says which of the two things they probably wanted.
pub(crate) fn parse_duration(spec: &str) -> Result<chrono::Duration, CliError> {
    let spec = spec.trim();
    let unreadable = || {
        CliError::Usage(format!(
            "could not read `{spec}` as a duration — use a number and one of \
             s, m, h, d (for example `30m` or `2h`)"
        ))
    };
    // Split on the last *character*, not the last byte: a multi-byte unit is
    // not a unit this understands, but slicing mid-character would panic
    // instead of saying so.
    let (cut, unit) = spec.char_indices().next_back().ok_or_else(unreadable)?;
    let count: i64 = spec[..cut].parse().map_err(|_| unreadable())?;
    let per_unit: i64 = match unit {
        's' | 'S' => 1,
        'm' | 'M' => 60,
        'h' | 'H' => 3_600,
        'd' | 'D' => 86_400,
        _ => return Err(unreadable()),
    };
    if count <= 0 {
        return Err(CliError::Usage(format!(
            "`{spec}` is not a window — omit `--for` to approve until revoked, \
             or use `trust revoke` to withdraw"
        )));
    }
    let too_long = || CliError::Usage(format!("`{spec}` is longer than a window can be"));
    let seconds = count.checked_mul(per_unit).ok_or_else(too_long)?;
    chrono::Duration::try_seconds(seconds).ok_or_else(too_long)
}

/// How long to wait before a `--at` send, or an error naming what was expected.
///
/// Accepts `HH:MM` (the next occurrence, today or tomorrow) and a full RFC-3339
/// local datetime. Deliberately not a general date parser: two formats people
/// actually type beat a dozen that each fail differently.
///
/// **A delay, not a scheduler.** The process waits, so a reboot cancels it —
/// which is why the flag's help says to use cron for anything that must
/// survive one, rather than implying a durability this cannot provide.
pub(crate) fn delay_until(
    now: chrono::DateTime<chrono::Local>,
    when: &str,
) -> Result<std::time::Duration, CliError> {
    use chrono::{Duration, NaiveTime, TimeZone};

    let target = if let Ok(t) = NaiveTime::parse_from_str(when.trim(), "%H:%M") {
        let today = now.date_naive().and_time(t);
        let candidate = chrono::Local
            .from_local_datetime(&today)
            .single()
            .ok_or_else(|| CliError::Usage(format!("ambiguous local time: {when}")))?;
        // A time already past today means tomorrow — nobody typing 09:00 at
        // ten in the morning means "nine hours ago".
        if candidate <= now {
            candidate + Duration::days(1)
        } else {
            candidate
        }
    } else {
        let naive = chrono::NaiveDateTime::parse_from_str(when.trim(), "%Y-%m-%dT%H:%M:%S")
            .map_err(|_| {
                CliError::Usage(format!(
                    "could not read {when} as a time — use HH:MM or YYYY-MM-DDTHH:MM:SS"
                ))
            })?;
        chrono::Local
            .from_local_datetime(&naive)
            .single()
            .ok_or_else(|| CliError::Usage(format!("ambiguous local time: {when}")))?
    };

    let delta = target - now;
    if delta <= Duration::zero() {
        return Err(CliError::Usage(format!(
            "{when} is in the past — nothing to wait for"
        )));
    }
    delta
        .to_std()
        .map_err(|_| CliError::Usage(format!("{when} is too far away")))
}

pub(crate) async fn send_files(
    ctx: &Ctx,
    args: SendArgs,
    path_override: Option<&str>,
) -> CliResult {
    // Waited out **before** the peer is resolved: looking a device up now and
    // dialling it in six hours would use an address that has almost certainly
    // changed. Paths are validated first, though — a typo should fail at once
    // rather than at nine tomorrow morning.
    let wait = args
        .at
        .as_deref()
        .map(|when| delay_until(chrono::Local::now(), when))
        .transpose()?;

    // Validate every path up front so a bad entry fails the whole call
    // before anything is sent.
    for p in &args.paths {
        if !std::path::Path::new(p).exists() {
            return Err(CliError::NotFound(format!("path {p}")));
        }
    }

    if let Some(delay) = wait {
        // Said plainly: this is a delay, and a reboot cancels it. Implying a
        // durability the process cannot provide would be worse than the wait.
        ctx.line(&ctx.dim(&format!(
            "waiting {} — this process must stay running",
            humantime(delay)
        )));
        tokio::time::sleep(delay).await;
    }

    let config = load_config(path_override)?;

    // Resolve the target peer — directly (--addr) or via discovery. The result
    // is a `Device`; the RouteManager decides which of its routes to use.
    let target = if let Some(addr) = &args.addr {
        let sa = resolve_addr(addr)?;
        target_device(addr.clone(), sa.ip().to_string(), sa.port())
    } else {
        let devices = snapshot(config.clone(), 2).await?;
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

    // Chat wiring on this dial too — not just on chat's own call sites. The
    // *receiving* peer's `serve_loop`/`daemon` runs flush-on-connect
    // unconditionally on every accepted session, regardless of what the
    // dialer established it for: if this plain-file dial registered no
    // `ChatHandler`, a message the peer pushes back over this same session
    // would silently decode-and-drop (frame counted in stats, never
    // dispatched, no error on either side) while the pusher marks it `Sent`
    // and dequeues it — permanent, silent loss. Mirrors the FFI's own fix for
    // the identical bug class (`Manager::chat_wiring()` / `open_send_retry`).
    let chat = chat_store(&config, &sc.enc, &sc.ident);
    let sink = crate::chat::received_sink(ctx);

    let hist = history::path_for(&config.storage.data_directory);
    for p in &args.paths {
        let path = std::path::Path::new(p);
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "file.bin".into());
        if path.is_dir() {
            let r = secure_send_folder(
                ctx, &quic, &routes, &target, &sc, &chat, &sink, &storage, p, &name, chunk,
            )
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
                ctx, &quic, &routes, &target, &sc, &chat, &sink, &storage, p, &name, size, chunk,
                None,
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
/// returns total bytes sent. `chat`/`sink` are threaded through so this dial
/// registers chat wiring too — see `send`'s call site for why a plain
/// file/folder transfer still needs to be able to *receive* a peer's pushed
/// chat message over the same session.
#[allow(clippy::too_many_arguments)]
async fn secure_send_folder(
    ctx: &Ctx,
    quic: &Arc<QuicTransport>,
    routes: &RouteManager,
    device: &peerbeam_domain::entity::Device,
    sc: &SecureCtx,
    chat: &peerbeam_chat::ChatStore,
    sink: &peerbeam_chat::ReceivedSink,
    storage: &FsStorage,
    path: &str,
    name: &str,
    chunk: u32,
) -> Result<u64, CliError> {
    let session = crate::session_transfer::dial(
        quic,
        routes,
        device,
        name,
        &sc.ident,
        &sc.enc,
        &sc.trust,
        Some((chat.clone(), sink.clone())),
    )
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
    // Capture the result, close the session, THEN propagate — a `?` here
    // before `session.close()` would skip the close on exactly the failure
    // path where it matters (leaking the session's pump task), the same bug
    // class already fixed twice elsewhere in this feature.
    session.close().await;
    let outcome = r.map_err(CliError::from)?;

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

/// The routing placeholder device id every `--addr` target carries.
///
/// An explicit address names no *discovered* device, so there is no real device
/// id to put here until a session authenticates one. That is harmless for
/// routing — the dial only needs the address — but it is load-bearing for
/// anything durable keyed by it: every drain in this project (`chat::drain_tick`
/// here, `runtime::chat_drain_loop` in the FFI) picks the peers it flushes out of
/// *discovery*, which can never yield this literal. So a queue filed under it is
/// a queue nothing will ever come back for. `chat::queued_lines` is where a user
/// is told that, and `chat cancel addr <id>` is how they undo it.
pub(crate) const ADDR_PEER_ID: &str = "addr";

/// A minimal `Device` for a `--addr` target (a single explicit route).
pub(crate) fn target_device(
    name: String,
    address: String,
    port: u16,
) -> peerbeam_domain::entity::Device {
    use peerbeam_domain::entity::{Device, DeviceType};
    use peerbeam_domain::id::DeviceId;
    Device {
        id: DeviceId::from(ADDR_PEER_ID),
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
    // `--dir` is an explicit destination for *this run*, so it wins outright:
    // rules are not consulted at all. A stored rule quietly overriding a
    // directory the operator just typed on the command line would be the
    // surprising direction, and this doubles as the way to say "ignore my
    // rules once" without editing them.
    let rules: &[peerbeam_config::SaveRule] = match args.dir {
        Some(_) => &[],
        None => &config.storage.rules,
    };
    serve_loop(ctx, &config, port, &dir, rules, args.once).await
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
            serve_loop(ctx, &config, port, &dir, &config.storage.rules, false).await
        }
        DaemonAction::Stop | DaemonAction::Status => Err(CliError::Unavailable(
            "daemon IPC (stop/status) is not implemented; run `daemon start` (it always runs in the foreground)".into(),
        )),
    }
}

/// This device's authentication material (crypto + trust + identity). Held as
/// `Arc`s so they can be shared into `PeerSession::open` (which takes owned
/// trait-object handles). `Clone` is cheap (`enc`/`trust` are `Arc` clones;
/// `ident` is a small fixed-size keypair) — used to hand an owned copy into a
/// spawned task (e.g. `chat::spawn_drain_tick`) that must outlive its
/// caller's stack frame.
#[derive(Clone)]
pub struct SecureCtx {
    pub enc: Arc<AeadCrypto>,
    pub trust: Arc<FsTrust>,
    pub ident: Identity,
}

impl SecureCtx {
    pub(crate) fn build(config: &EngineConfig) -> Result<Self, CliError> {
        let enc = AeadCrypto::new();
        let identity_path =
            std::path::Path::new(&config.storage.data_directory).join("identity.json");
        let ident = peerbeam_transfer::load_or_generate(
            &peerbeam_identity_fs::FsIdentity::open(identity_path),
            &enc,
            config.device.name.clone(),
        )?;
        Ok(Self {
            enc: Arc::new(enc),
            trust: Arc::new(open_trust(config)?),
            ident,
        })
    }
}

/// Open this device's trust store — `<data_directory>/trust.json`.
///
/// The one place that path is written. [`SecureCtx::build`] needs the store for
/// the handshake; `peerbeam trust` needs *only* the store, and must not build a
/// `SecureCtx` to get at it — that would generate an identity keypair as a side
/// effect of listing devices. A second literal of this path would be a store one
/// command writes and another never reads.
pub(crate) fn open_trust(config: &EngineConfig) -> Result<FsTrust, CliError> {
    let path = std::path::Path::new(&config.storage.data_directory).join("trust.json");
    FsTrust::open(path).map_err(CliError::from)
}

/// Build the CLI's chat store: an encrypted [`peerbeam_appstore_fs::FsAppStore`]
/// rooted at `<data_directory>/appstore`, keyed by a domain-separated subkey of
/// the device identity secret — the same construction the FFI runtime uses, so
/// chat history persists across `chat send`/`chat history`/`chat watch`
/// invocations and stays isolated from the transfer identity key.
pub(crate) fn chat_store(
    config: &EngineConfig,
    enc: &Arc<AeadCrypto>,
    ident: &Identity,
) -> peerbeam_chat::ChatStore {
    let root = std::path::Path::new(&config.storage.data_directory).join("appstore");
    let key = peerbeam_crypto::derive_subkey(&ident.keypair.secret.0, b"peerbeam-appstore-v1");
    let store: Arc<dyn peerbeam_domain::port::AppStore> = Arc::new(
        peerbeam_appstore_fs::FsAppStore::open(root, key, enc.clone()),
    );
    peerbeam_chat::ChatStore::new(store)
}

/// Build the CLI's folder-sync index, over the same encrypted AppStore.
///
/// Keyed by this device's own id: the index records which device made each
/// edit, and a counter raised under the wrong name would claim someone else's
/// work as this machine's.
pub(crate) fn sync_index(
    config: &EngineConfig,
    enc: &Arc<AeadCrypto>,
    ident: &Identity,
) -> peerbeam_sync::SyncIndex {
    let root = std::path::Path::new(&config.storage.data_directory).join("appstore");
    let key = peerbeam_crypto::derive_subkey(&ident.keypair.secret.0, b"peerbeam-appstore-v1");
    let store: Arc<dyn peerbeam_domain::port::AppStore> = Arc::new(
        peerbeam_appstore_fs::FsAppStore::open(root, key, enc.clone()),
    );
    peerbeam_sync::SyncIndex::new(store, &ident.device_id.0)
}

/// Build the CLI's clipboard-history store, over the same encrypted AppStore
/// notes and chat use — history lives in its own namespace inside it.
pub(crate) fn clip_history_store(
    config: &EngineConfig,
    enc: &Arc<AeadCrypto>,
    ident: &Identity,
) -> peerbeam_clipboard::ClipHistory {
    let root = std::path::Path::new(&config.storage.data_directory).join("appstore");
    let key = peerbeam_crypto::derive_subkey(&ident.keypair.secret.0, b"peerbeam-appstore-v1");
    let store: Arc<dyn peerbeam_domain::port::AppStore> = Arc::new(
        peerbeam_appstore_fs::FsAppStore::open(root, key, enc.clone()),
    );
    peerbeam_clipboard::ClipHistory::new(store)
}

/// Build the CLI's note store, over the same encrypted AppStore chat uses —
/// notes live in their own namespace inside it, so one machine has one store
/// rather than two that could disagree about the key.
/// Build the CLI's Space store.
///
/// Same `AppStore` and key as every other record this device keeps, plus the
/// trust store — because a Space read answers "is this member still trusted?"
/// before it answers anything else, and a store that could not ask would have
/// to guess.
/// Build the CLI's wake store — the hardware addresses this device remembers.
pub(crate) fn wake_store(config: &EngineConfig) -> Result<peerbeam_wake::WakeStore, CliError> {
    let sc = SecureCtx::build(config)?;
    let root = std::path::Path::new(&config.storage.data_directory).join("appstore");
    let key = peerbeam_crypto::derive_subkey(&sc.ident.keypair.secret.0, b"peerbeam-appstore-v1");
    let app: Arc<dyn peerbeam_domain::port::AppStore> = Arc::new(
        peerbeam_appstore_fs::FsAppStore::open(root, key, sc.enc.clone()),
    );
    Ok(peerbeam_wake::WakeStore::new(app))
}

pub(crate) fn space_store(config: &EngineConfig) -> Result<peerbeam_spaces::SpaceStore, CliError> {
    let sc = SecureCtx::build(config)?;
    let root = std::path::Path::new(&config.storage.data_directory).join("appstore");
    let key = peerbeam_crypto::derive_subkey(&sc.ident.keypair.secret.0, b"peerbeam-appstore-v1");
    let app: Arc<dyn peerbeam_domain::port::AppStore> = Arc::new(
        peerbeam_appstore_fs::FsAppStore::open(root, key, sc.enc.clone()),
    );
    let trust: Arc<dyn peerbeam_domain::port::TrustStore> = sc.trust.clone();
    Ok(peerbeam_spaces::SpaceStore::new(app, trust))
}

pub(crate) fn note_store(
    config: &EngineConfig,
    enc: &Arc<AeadCrypto>,
    ident: &Identity,
) -> peerbeam_notes::NoteStore {
    let root = std::path::Path::new(&config.storage.data_directory).join("appstore");
    let key = peerbeam_crypto::derive_subkey(&ident.keypair.secret.0, b"peerbeam-appstore-v1");
    let store: Arc<dyn peerbeam_domain::port::AppStore> = Arc::new(
        peerbeam_appstore_fs::FsAppStore::open(root, key, enc.clone()),
    );
    peerbeam_notes::NoteStore::new(store)
}

/// Build the CLI's staging store: the outbox's own copy of every file waiting
/// to be sent, rooted at `<data_directory>/outbox-blobs` — the same path the
/// FFI runtime uses, so a single machine running both surfaces has one place
/// where queued bytes live rather than two.
///
/// A sibling of `appstore`, not a directory inside it: `FsAppStore` owns that
/// tree and enumerates it by namespace, and a staged blob is not a record. It is
/// plaintext user content, written `0600`, and deleted the moment the entry that
/// owns it settles.
pub(crate) fn staging_store(config: &EngineConfig) -> peerbeam_chat::StagingStore {
    let root = std::path::Path::new(&config.storage.data_directory).join("outbox-blobs");
    peerbeam_chat::StagingStore::new(
        root.to_string_lossy().into_owned(),
        Arc::new(peerbeam_storage_fs::FsStorage::new()),
    )
}

/// The two bounds a stage is held to, read from configuration here so nothing
/// in `peerbeam_chat` has to know what a config is.
pub(crate) fn staging_limits(config: &EngineConfig) -> peerbeam_chat::StagingLimits {
    peerbeam_chat::StagingLimits {
        max_bytes: config.device.max_queued_file_bytes,
        min_free_bytes: config.device.min_free_bytes,
    }
}

/// Select a route (via the RouteManager), authenticate, wrap in `SecureLink`,
/// and stream one file with progress. The route chosen is the manager's
/// concern — this function never sees it. `chat`/`sink` are threaded through
/// so this dial registers chat wiring too: the receiving peer's `serve_loop`/
/// `daemon` runs flush-on-connect unconditionally on every accepted session,
/// so a session dialed with `chat: None` would silently drop any message the
/// peer pushes back over it (see `send`'s call site doc for the full bug this
/// avoids).
#[allow(clippy::too_many_arguments)]
pub(crate) async fn secure_send_file(
    ctx: &Ctx,
    quic: &Arc<QuicTransport>,
    routes: &RouteManager,
    device: &peerbeam_domain::entity::Device,
    sc: &SecureCtx,
    chat: &peerbeam_chat::ChatStore,
    sink: &peerbeam_chat::ReceivedSink,
    storage: &FsStorage,
    path: &str,
    name: &str,
    size: u64,
    chunk: u32,
    // `transfer_id`: the wire transfer id, when the caller needs a specific
    // one. `None` keeps the historical behaviour of using the file name;
    // `transfers resume` passes the checkpoint's id, because that is the id
    // the checkpoint — and the surface showing it — is keyed by.
    transfer_id: Option<&str>,
) -> CliResult {
    let session = crate::session_transfer::dial(
        quic,
        routes,
        device,
        name,
        &sc.ident,
        &sc.enc,
        &sc.trust,
        Some((chat.clone(), sink.clone())),
    )
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
        transfer_id: transfer_id.unwrap_or(name).to_string(),
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
    // Capture the result, close the session, THEN propagate — a `?` here
    // before `session.close()` would skip the close on exactly the failure
    // path where it matters (leaking the session's pump task), the same bug
    // class already fixed twice elsewhere in this feature.
    session.close().await;
    r.map_err(CliError::from)?;

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

/// Mirror a receive's outcome onto the chat row it might be completing — the
/// CLI-side counterpart of the FFI's `Manager::chat_settle`/
/// `chat_set_local_path`. `chat` file shares mint the wire transfer id off
/// the same id as the `FileRef`'s chat row (`peerbeam_chat::prepare_file_send`),
/// so a receive whose peer-supplied `transfer_id` names a row in *our* store
/// is how that row ever leaves `PendingApproval` — without this bridge, a
/// file shared via chat and received here would save correctly but its
/// conversation row would stay `PendingApproval` forever.
///
/// Delegates the entire authorization decision to `ChatStore::settle_file_row`/
/// `set_file_row_path` (guarded by `ChatRecord::is_settleable_file_row`):
/// `transfer_id` is the peer's own first-frame field on receive (peer-
/// supplied, not further validated) and a chat message id is a wire field the
/// peer has already seen, so a bare id match is not proof this row belongs to
/// this transfer — see that guard's doc for the full rationale (an
/// already-paired peer sending an ordinary file whose `transfer_id` happens
/// to equal an existing message id in the thread must never be able to flip
/// an unrelated `Text`/already-settled row). A missing row, wrong kind/
/// direction, or an already-settled row is therefore a silent no-op here —
/// exactly the ordinary-transfer case, which is the overwhelming majority of
/// receives.
///
/// `landed` and `local_path`, when given, are written **before** `status` —
/// all three share the in-flight leg of the guard, so once the row reads a
/// terminal status it is deliberately closed to further writes (mirrors the
/// FFI's own ordering note on `chat_set_landing`/`chat_set_local_path`).
/// Silently does nothing when `transfer_id` is empty (nothing could be
/// peeked — see `peek_incoming_meta`'s doc).
///
/// `landed` is the `(name, bytes)` the receive *actually* wrote. The row's own
/// name/size came from the peer's separate CHAT-channel `FileRef`, correlated
/// with this stream by id alone and never checked against it, so without this
/// the conversation would keep describing whatever was advertised rather than
/// what is on disk — see `ChatStore::set_file_row_landing`.
///
/// **A store failure is returned, not dropped.** All three writes used to be
/// `let _ = …`, so a conversation row could be left permanently `PendingApproval`
/// — an Accept button for a transfer that has already finished — over a file
/// that landed perfectly, with nothing anywhere recording why. It is returned
/// rather than propagated because the caller must not fail a receive whose
/// bytes are safely on disk; `serve_loop` prints it instead.
///
/// The first failure stops the rest: `set_file_row_landing` failing and the
/// settle going ahead anyway would close the row for good while it still
/// carried the sender's *claim* about the name — which is precisely the lie
/// that reconciliation exists to prevent. A row left in flight is settled as
/// `Interrupted` by the next startup reconcile; a row settled with the wrong
/// name is settled forever.
fn settle_received_chat_file(
    chat: &peerbeam_chat::ChatStore,
    peer_id: &str,
    transfer_id: &str,
    status: peerbeam_chat::Status,
    landed: Option<(&str, u64)>,
    local_path: Option<&str>,
) -> Result<(), peerbeam_chat::ChatError> {
    if transfer_id.is_empty() {
        return Ok(());
    }
    let peer = DeviceId::from(peer_id.to_string());
    if let Some((name, size)) = landed {
        chat.set_file_row_landing(&peer, transfer_id, peerbeam_chat::Direction::In, name, size)?;
    }
    if let Some(path) = local_path {
        chat.set_file_row_path(&peer, transfer_id, peerbeam_chat::Direction::In, path)?;
    }
    chat.settle_file_row(&peer, transfer_id, peerbeam_chat::Direction::In, status)?;
    Ok(())
}

/// Serve inbound QUIC connections as PeerSessions, accept each peer's transfer
/// channel, and receive one file or folder per connection. Advertises presence
/// via discovery so senders find us.
///
/// `dir` is where an item lands when no rule claims it — today's
/// `storage.save_directory`, or whatever `receive --dir` overrode it with.
/// `rules` is the ordered list consulted first; an empty slice is "no rules",
/// which is exactly the behaviour that shipped before rules existed.
async fn serve_loop(
    ctx: &Ctx,
    config: &EngineConfig,
    port: u16,
    dir: &str,
    rules: &[peerbeam_config::SaveRule],
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
    // the loop below so discovery can be stopped on the way out, and doubles as
    // the drain tick's peer-reachability source (see below).
    let engine = build_engine(config.clone()).ok();
    if let (Some(engine), Ok(self_device)) = (&engine, me(config)) {
        let _ = engine.start_discovery(self_device).await;
    }

    // Chat: this session is dual-purpose (transfer + chat) — every accepted
    // connection registers a chat handler too, so a peer's `chat send` is
    // received while we serve files/folders, and a periodic drain tick
    // retries delivery to any peer whose outbox still has queued messages
    // once discovery reports them reachable (`crate::chat::drain_tick`, via
    // the non-blocking `crate::chat::spawn_drain_tick`). The same `quic`
    // instance both serves (above) and dials (the drain tick, through its
    // own `RouteManager::new(quic.clone())`) — `serve_channels_on` binds its
    // own server-side endpoint independent of the client endpoint
    // `dial_channels` uses, so no second transport is needed (mirrors the
    // FFI's `Manager`, which reuses one `self.quic` for both `serve()` and
    // `chat_flush_peer`'s dial).
    let chat = chat_store(config, &sc.enc, &sc.ident);
    let sink = crate::chat::received_sink(ctx);
    // One RouteManager for this serve loop: presence asks it to classify each
    // inbound connection's remote address, so an accepted session reports the
    // same route vocabulary a dialled one does.
    let accept_routes = RouteManager::new(quic.clone());
    let mut drain = tokio::time::interval(crate::chat::DRAIN_EVERY);
    // If a tick is missed (e.g. we were busy serving a long-lived transfer),
    // resume the plain periodic cadence rather than firing a burst of
    // catch-up ticks — mirrors the FFI's `chat_drain_loop`.
    drain.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    // Single-flight guard for `spawn_drain_tick` — see its doc comment: the
    // sweep runs on its own spawned task rather than inline in the `select!`
    // arm below, so a backlog of unreachable peers (each subject to the QUIC
    // connect timeout) can never stall this loop's accept arm — including on
    // the very first tick, which `tokio::time::interval` fires immediately.
    let draining = Arc::new(AtomicBool::new(false));

    loop {
        tokio::select! {
            _ = drain.tick() => {
                if let Some(eng) = &engine {
                    crate::chat::spawn_drain_tick(&draining, eng, &chat, &quic, &sc, &sink);
                }
            }
            item = incoming.next() => {
                let qc = match item {
                    Some(Ok(c)) => c,
                    Some(Err(e)) => {
                        if !ctx.json {
                            ctx.line(&ctx.dim(&format!("inbound rejected: {e}")));
                        }
                        continue;
                    }
                    None => break,
                };
                // Establish the PeerSession (runs the handshake internally);
                // register the chat handler on this side too (see
                // `crate::chat`'s module doc on why dial/accept symmetry
                // matters — an unhandled push is silently dropped, not
                // errored).
                let mut session = match crate::session_transfer::accept(
                    qc,
                    &accept_routes,
                    &sc.ident,
                    &sc.enc,
                    &sc.trust,
                    Some((chat.clone(), sink.clone())),
                )
                .await
                {
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

                // Flush-on-connect: push anything already queued for this peer
                // now that the pairing gate has let this connection through —
                // cheaper/faster than waiting for the next drain tick, and
                // independent of whatever this connection is otherwise for (a
                // transfer, or nothing at all). Deliberately placed AFTER the
                // pairing gate (not before, unlike the FFI's `handle_incoming`,
                // which has no such gate to order against): `PairingGate::Revoke`
                // means the operator suspects this newly-pinned peer of being a
                // MITM and the connection is being torn down above — chat
                // content must not be pushed to a peer we're actively revoking
                // trust in. `Revoke`'s branch always `continue`s/`break`s before
                // reaching here, so this line only ever runs on
                // Proceed/Confirmed.
                let _ = peerbeam_chat::flush_to_session(
                    &session.handle,
                    &chat,
                    &DeviceId::from(peer_id.clone()),
                )
                .await;

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

                // **The listen gate.** This is a `receive`/`daemon`, not a
                // `peerbeam pipe --listen`, so an inbound pipe is refused here
                // — the one place a long-lived background process could
                // otherwise become a remote write to whatever terminal it was
                // started from. The capability is still advertised and the
                // channel type still registered as a stream (see
                // `session_transfer::session_cfg`): that is what routes the
                // pipe here to be refused with a reason, instead of leaving it
                // to hang as an unhandled message channel.
                //
                // `accept_pipe` is the single funnel that decides — this side
                // passes `listening: false` and an `out` that discards, so
                // even a broken gate could not write the peer's bytes
                // anywhere. Refusing does not end the loop: a stranger must not
                // be able to stop a receiver by dialling it.
                if incoming_ch.channel_type == peerbeam_domain::session::ChannelType::PIPE {
                    let consent = peerbeam_transfer::PipeConsent {
                        listening: false,
                        trust: sc.trust.as_ref(),
                        only_from: None,
                        negotiated: session.capabilities(),
                    };
                    let peer = DeviceId::from(peer_id.clone());
                    let mut nowhere = futures::io::sink();
                    let refused = accept_pipe(
                        incoming_ch,
                        &session.handle,
                        &peer,
                        &consent,
                        &mut nowhere,
                    )
                    .await;
                    let msg = match refused {
                        Ok(_) => "a pipe was accepted by a process that must never accept one"
                            .to_string(),
                        Err(e) => e.to_string(),
                    };
                    if ctx.json {
                        ctx.json_line(&json!({"event": "error", "message": msg}));
                    } else {
                        ctx.line(&ctx.dim(&msg));
                    }
                    session.close().await;
                    continue;
                }

                // Peek the sender's transfer id before consuming any bytes, so
                // the chat bridge below can correlate this receive with a
                // FileRef-offered row even if the transfer itself later
                // fails — `peek_incoming_meta` replays the frame it reads, so
                // `receive_on_channel` sees exactly what it would have
                // without this call. `transfer_id` is empty when nothing
                // could be peeked (closed/malformed/slow first frame), which
                // the bridge below reads as "no correlation possible" and
                // skips entirely. Mirrors the FFI's own `handle_incoming`
                // peek (`peek_incoming_meta`).
                let (incoming_ch, preview) = peek_incoming_meta(incoming_ch).await;

                // A chat file row starts life `PendingApproval`, describing the
                // peer's `FileRef` claim. Bytes are now moving, and the peek has
                // just told us what the *stream* says is arriving — so correct
                // the row's name/size against it and move it off
                // `PendingApproval`, mirroring the FFI's `chat_set_landing` +
                // `chat_settle(Transferring)` pair. Without this a `chat
                // history` run mid-receive shows a file still "waiting" and
                // labelled with whatever was advertised. A no-op for every
                // ordinary transfer (no row) and when nothing could be peeked.
                if let Err(e) = settle_received_chat_file(
                    &chat,
                    &peer_id,
                    &preview.transfer_id,
                    peerbeam_chat::Status::Transferring,
                    (!preview.name.is_empty()).then_some((preview.name.as_str(), preview.size)),
                    None,
                ) {
                    // Not fatal — the bytes below are what the peer came for —
                    // but not silent either: the row this could not move stays
                    // "waiting" in `chat history` while the file arrives.
                    report_problem(
                        ctx,
                        &format!("this transfer's conversation row could not be updated: {e}"),
                    );
                }

                // **Where this item lands.** The one call site: the matcher is
                // consulted once, here, after the transfer has been accepted
                // and immediately before its bytes are written. It cannot
                // affect *whether* anything is accepted — everything that
                // decides that has already run above (I6).
                //
                // The three inputs are the authenticated sender id, the
                // *sanitized* name (`preview.name` is what `peek_incoming_meta`
                // put through `sanitize_file_name`, never the raw wire name)
                // and the size. A peek that learned nothing leaves the name
                // empty and the size zero, which simply matches fewer rules —
                // a catch-all still applies, and `dir` is still the answer when
                // nothing matches.
                let dest = peerbeam_config::rules::destination(
                    rules,
                    dir,
                    &peer_id,
                    &preview.name,
                    preview.size,
                );
                // A destination that failed must be *said*, not swallowed. The
                // file is safe — it is going to `dir` — but a user who wrote a
                // rule believes the sort happened.
                if let Some(fb) = &dest.fallback {
                    let msg = format!(
                        "rule destination {} is unusable ({}); saving to {} instead",
                        fb.rule_directory, fb.reason, dest.directory
                    );
                    if ctx.json {
                        ctx.json_line(&json!({
                            "event": "rule_fallback",
                            "rule_directory": fb.rule_directory,
                            "directory": dest.directory,
                            "reason": fb.reason,
                            "peer": peer_id,
                        }));
                    } else {
                        ctx.line(&ctx.yellow(&msg));
                    }
                }
                let dir = dest.directory.as_str();

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
                        // Chat bridge: a completed receive settles Received (+
                        // where it landed); an observed cancellation settles
                        // Failed. Both are no-ops unless this transfer's id
                        // genuinely names our own in-flight file row.
                        let settled = match rcv.outcome {
                            TransferOutcome::Completed => settle_received_chat_file(
                                &chat,
                                &peer_id,
                                &preview.transfer_id,
                                peerbeam_chat::Status::Received,
                                Some((rcv.name.as_str(), rcv.bytes)),
                                Some(&saved),
                            ),
                            TransferOutcome::Cancelled => settle_received_chat_file(
                                &chat,
                                &peer_id,
                                &preview.transfer_id,
                                peerbeam_chat::Status::Failed,
                                None,
                                None,
                            ),
                        };
                        // The file is on disk and has already been reported as
                        // received — the row is the only thing wrong, so this
                        // says so rather than contradicting the line above it.
                        if let Err(e) = settled {
                            report_problem(
                                ctx,
                                &format!(
                                    "{} arrived, but its conversation row could not be updated: {e}",
                                    rcv.name
                                ),
                            );
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
                        // Chat bridge: a transfer failure settles the row
                        // Failed too — a no-op unless the id genuinely names
                        // our own in-flight file row (see
                        // `settle_received_chat_file`'s doc).
                        if let Err(e) = settle_received_chat_file(
                            &chat,
                            &peer_id,
                            &preview.transfer_id,
                            peerbeam_chat::Status::Failed,
                            None,
                            None,
                        ) {
                            // A failed transfer whose row also could not be
                            // marked failed leaves the conversation showing an
                            // in-flight file nothing will ever finish.
                            report_problem(
                                ctx,
                                &format!("the failed transfer's conversation row could not be updated: {e}"),
                            );
                        }
                    }
                }
                session.close().await;
                if once {
                    break;
                }
            }
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
pub(crate) fn resolve_addr(s: &str) -> Result<std::net::SocketAddr, CliError> {
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
pub(crate) fn resolve_peer(
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
        ClipboardAction::History { clear } => clipboard_history(ctx, clear, path_override),
        ClipboardAction::Send { to, addr, text } => {
            clipboard_send(ctx, to, addr, text, path_override).await
        }
    }
}

/// Print the newest received clipboard payload (the `peerbeam-clipboard-*.txt`
/// wire convention) from the save directory.
/// `peerbeam timeline [--limit N]`.
///
/// Reads the same stores the engine does — transfer history, conversations, and
/// clipboard history when it is on — and merges them by time. The CLI builds
/// this itself rather than calling the engine: a running daemon is not required
/// to look at what this machine has done.
fn timeline_cmd(
    ctx: &Ctx,
    args: crate::cli::TimelineArgs,
    path_override: Option<&str>,
) -> CliResult {
    if args.limit == 0 || args.limit > 1000 {
        return Err(CliError::Usage("limit must be between 1 and 1000".into()));
    }
    let config = load_config(path_override)?;
    let sc = SecureCtx::build(&config)?;
    let chat = chat_store(&config, &sc.enc, &sc.ident);

    #[derive(serde::Serialize)]
    struct Entry {
        kind: &'static str,
        at: String,
        peer: String,
        detail: String,
    }
    let mut events: Vec<Entry> = Vec::new();

    for peer in chat.conversations().unwrap_or_default() {
        for rec in chat.history(&peer).unwrap_or_default() {
            events.push(Entry {
                kind: "chat",
                at: rec.timestamp.clone(),
                peer: peer.0.clone(),
                // Never the body. A timeline is for recognising when something
                // happened; `chat history` reads conversations properly.
                detail: match rec.kind {
                    peerbeam_chat::Kind::File => rec
                        .file
                        .as_ref()
                        .map_or_else(String::new, |f| f.name.clone()),
                    _ => String::new(),
                },
            });
        }
    }
    if config.device.clipboard_history {
        let clips = clip_history_store(&config, &sc.enc, &sc.ident);
        for e in clips.list().unwrap_or_default() {
            events.push(Entry {
                kind: "clipboard",
                at: e.at,
                peer: e.from.unwrap_or_default(),
                // No clip text, for the reason the listing abbreviates: this
                // goes into terminal scrollback.
                detail: String::new(),
            });
        }
    }

    events.sort_by(|a, b| b.at.cmp(&a.at).then_with(|| a.kind.cmp(b.kind)));
    let truncated = events.len() > args.limit;
    events.truncate(args.limit);

    if ctx.json {
        ctx.json_line(&serde_json::json!({ "events": events, "truncated": truncated }));
        return Ok(());
    }
    if events.is_empty() {
        ctx.line(&ctx.dim("nothing yet"));
        return Ok(());
    }
    for e in &events {
        let who = if e.peer.is_empty() {
            "this device"
        } else {
            &e.peer
        };
        ctx.line(&format!(
            "{}  {:<9} {}  {}",
            ctx.dim(&e.at),
            e.kind,
            who,
            e.detail
        ));
    }
    if truncated {
        // Silently showing the newest N reads as "that is all there is".
        ctx.line(&ctx.dim(&format!(
            "…showing the newest {} — pass --limit for more",
            args.limit
        )));
    }
    Ok(())
}

/// `peerbeam clipboard history [--clear]`.
///
/// Reading is never gated on the opt-in: an empty list is the honest answer for
/// a device that records nothing, and refusing the command would make "off"
/// indistinguishable from "broken". Clearing likewise works whether or not the
/// setting is on — turning it off stops new entries but does not erase what was
/// already recorded, and someone who wants it gone needs a way to say so.
fn clipboard_history(ctx: &Ctx, clear: bool, path_override: Option<&str>) -> CliResult {
    let config = load_config(path_override)?;
    let sc = SecureCtx::build(&config)?;
    let store = clip_history_store(&config, &sc.enc, &sc.ident);

    if clear {
        let n = store.clear().map_err(|e| CliError::Other(e.to_string()))?;
        if ctx.json {
            ctx.json_line(&serde_json::json!({ "cleared": n }));
        } else {
            ctx.line(&format!("cleared {n} remembered clips"));
        }
        return Ok(());
    }

    let entries = store.list().map_err(|e| CliError::Other(e.to_string()))?;
    if ctx.json {
        ctx.json_line(&serde_json::json!({ "entries": entries }));
        return Ok(());
    }
    if entries.is_empty() {
        ctx.line(&ctx.dim(if config.device.clipboard_history {
            "no clipboard history yet"
        } else {
            "clipboard history is off (device.clipboard_history)"
        }));
        return Ok(());
    }
    for e in &entries {
        // One line each, and the text is **abbreviated**: this prints into a
        // terminal that keeps scrollback, and reproducing a whole remembered
        // clip there would undo the point of bounding the log at all.
        let from = e.from.as_deref().unwrap_or("this device");
        ctx.line(&format!(
            "{}  {}  {}",
            ctx.dim(&e.at),
            from,
            abbreviate(&e.text)
        ));
    }
    Ok(())
}

/// A one-line preview of a remembered clip: first line, hard-capped.
///
/// Never the whole clip. `clipboard get` exists for someone who actually wants
/// the content; a listing is for recognising an entry, and printing everything
/// would put every remembered secret into terminal scrollback at once.
fn abbreviate(text: &str) -> String {
    let line = text.lines().next().unwrap_or("").trim();
    if line.chars().count() <= 60 {
        return line.to_string();
    }
    let cut: String = line.chars().take(59).collect();
    format!("{cut}…")
}

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
        at: None,
        paths: vec![tmp.to_string_lossy().into_owned()],
        to,
        addr,
    };
    let result = send_files(ctx, send_args, path_override).await;
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
/// `snapshot` — what the discovery-backed commands are told when discovery
/// could not run at all.
#[cfg(test)]
mod snapshot_tests {
    use super::{snapshot, EngineConfig};

    /// **"No devices found" used to mean this.** A data directory whose
    /// `identity.json` cannot be parsed fails `build_engine` and `me` — the
    /// device has no id to announce and nothing to authenticate with — and all
    /// three of `snapshot`'s failures were folded into an empty `Vec`. `peerbeam
    /// list` then printed "no devices found": a statement about the user's
    /// network, produced entirely by a broken file on their own disk, and the
    /// one they act on by going to look at their router.
    #[tokio::test]
    async fn a_snapshot_that_could_not_run_is_an_error_not_an_empty_network() {
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().join("data");
        std::fs::create_dir_all(&data).unwrap();
        // Present but unusable — the case `FsIdentity::load` reports rather
        // than quietly replacing (a regenerated identity would break every
        // peer's TOFU pin).
        std::fs::write(data.join("identity.json"), b"not an identity at all").unwrap();

        let mut config = EngineConfig::default();
        config.storage.data_directory = data.to_string_lossy().into_owned();
        // Nothing here reaches the network — this fails before any socket is
        // bound — but an OS-assigned port keeps it that way if it ever does.
        config.discovery.port = 0;

        let err = snapshot(config, 0)
            .await
            .expect_err("a discovery window that never opened is not an empty network");
        assert!(
            err.to_string().contains("identity"),
            "the reason has to survive as far as the user, got: {err}"
        );
    }
}

/// `doctor`'s config row — the check the command did not have.
#[cfg(test)]
mod doctor_tests {
    use super::{config_check, EngineConfig};

    /// **The silent substitution.** A `config.json` that cannot be parsed used
    /// to become the defaults with nothing said — in the one command whose
    /// whole job is saying what is wrong, and about the fault most likely to be
    /// behind whatever sent the user here. The CLI installs no log sink at all,
    /// so this report is the only place it can appear.
    #[test]
    fn a_config_that_cannot_be_parsed_is_reported_as_a_failed_check() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("config.json");
        std::fs::write(&p, br#"{ "device": { "name": "MyLaptop" "#).unwrap();

        let (cfg, (name, status, detail)) = config_check(Some(&p.to_string_lossy()));

        assert_eq!(name, "Config");
        assert_eq!(
            status, "fail",
            "a config that cannot be parsed must be reported, not folded into the defaults"
        );
        assert!(
            detail.contains("config"),
            "the row has to carry the reason, got: {detail}"
        );
        assert_eq!(
            cfg.device.name,
            EngineConfig::default().device.name,
            "the remaining checks still run — on defaults, which is why saying so matters"
        );
    }

    /// A config that does not exist yet is not a fault: `load_or_default`
    /// treats NotFound as the defaults by design and a fresh install has none,
    /// so reporting it as a failure would make `doctor` exit non-zero on every
    /// first run — and teach people to ignore its exit code.
    #[test]
    fn a_config_that_was_never_created_still_passes() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("config.json");

        let (_, (_, status, detail)) = config_check(Some(&p.to_string_lossy()));

        assert_eq!(status, "pass");
        assert!(
            detail.contains("none yet"),
            "the report should distinguish 'no config' from 'this config', got: {detail}"
        );
    }

    /// And a usable config is *returned*, not merely approved: every check
    /// after this row reads its save directory, data directory and ports from
    /// here, so the row and the checks under it describe one config.
    #[test]
    fn a_usable_config_is_the_one_the_rest_of_the_report_uses() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("config.json");
        std::fs::write(&p, br#"{"device":{"name":"Reception iMac"}}"#).unwrap();

        let (cfg, (_, status, _)) = config_check(Some(&p.to_string_lossy()));

        assert_eq!(status, "pass");
        assert_eq!(cfg.device.name, "Reception iMac");
    }
}

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
    use crate::cli::{
        ChannelsArgs, Cli, Command, SessionAction, SessionArgs, TransfersAction, TransfersArgs,
    };
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
            Command::Transfers(TransfersArgs { action: None }),
            Command::Transfers(TransfersArgs {
                action: Some(TransfersAction::List),
            }),
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

// ── regression: plain-send dial must register chat wiring (review round 1) ─
#[cfg(test)]
mod chat_wiring_dial_regression {
    use super::{chat_store, secure_send_file, SecureCtx};
    use crate::output::Ctx;
    use futures::StreamExt;
    use peerbeam_chat::ChatMessage;
    use peerbeam_config::EngineConfig;
    use peerbeam_domain::id::DeviceId;
    use peerbeam_engine::RouteManager;
    use peerbeam_storage_fs::FsStorage;
    use peerbeam_transfer::{receive_on_channel, TransferControl};
    use peerbeam_transfer_quic::QuicTransport;
    use std::sync::Arc;
    use tokio::sync::mpsc;

    fn quiet_ctx() -> Ctx {
        Ctx::new(true, true, 0, true, true)
    }

    fn isolated_config(dir: &std::path::Path) -> EngineConfig {
        let mut config = EngineConfig::default();
        config.storage.data_directory = dir.join("data").to_string_lossy().into_owned();
        config.storage.save_directory = dir.join("recv").to_string_lossy().into_owned();
        config.transfer.port = 0;
        config
    }

    /// Regression guard for review round 1's Finding 1: `secure_send_file`'s
    /// own dial must register chat wiring (`Some`, not `None`), because the
    /// *receiving* peer's `serve_loop` runs flush-on-connect unconditionally
    /// on every accepted session — independent of what the dialer established
    /// it for. Without a `ChatHandler` on the dialer's side, a message pushed
    /// back over that same session silently decodes-and-drops (counted in
    /// stats, never dispatched, no error raised anywhere) while the pusher
    /// marks it `Sent` and dequeues it: permanent, silent loss.
    ///
    /// Scenario: "b" already has a message queued for "a" (as if queued while
    /// "a" was offline in a prior conversation). "a" now dials "b" with a
    /// PLAIN file send (`secure_send_file`, not `chat send`). "b" accepts and
    /// immediately flushes (mirroring `serve_loop`'s flush-on-connect) over
    /// that same session. Asserts "a" actually receives + persists it.
    #[tokio::test]
    async fn secure_send_file_dial_receives_a_pushed_chat_message() {
        let dir_a = tempfile::tempdir().expect("dir a");
        let dir_b = tempfile::tempdir().expect("dir b");
        let cfg_a = isolated_config(dir_a.path());
        let cfg_b = isolated_config(dir_b.path());
        let sc_a = SecureCtx::build(&cfg_a).expect("sc a");
        let sc_b = SecureCtx::build(&cfg_b).expect("sc b");
        let chat_a = chat_store(&cfg_a, &sc_a.enc, &sc_a.ident);
        let chat_b = chat_store(&cfg_b, &sc_b.enc, &sc_b.ident);

        // "b" already has a message queued for "a", keyed by a's real device
        // id — exactly what a legitimate prior conversation leaves behind
        // while a is offline.
        let a_id = sc_a.ident.device_id.clone();
        let queued = ChatMessage::new("queued while you were offline").expect("msg");
        chat_b.enqueue(&a_id, &queued).expect("enqueue");

        // "b": a real QUIC listener (mirrors serve_loop's bind).
        let quic_b = Arc::new(QuicTransport::new().expect("quic b"));
        let (addr_b, mut incoming_b) = quic_b
            .serve_channels_on("127.0.0.1:0".parse().expect("addr"))
            .await
            .expect("serve b");

        let content = b"not a real pdf, just bytes for the test";
        let src = dir_a.path().join("report.pdf");
        std::fs::write(&src, content).expect("write src");

        let ctx = quiet_ctx();
        let device_b = super::target_device("b".into(), addr_b.ip().to_string(), addr_b.port());
        let a_sink = crate::chat::received_sink(&ctx);
        let b_sink = crate::chat::received_sink(&ctx);

        let a_side = async {
            let quic_a = Arc::new(QuicTransport::new().expect("quic a"));
            let routes_a = RouteManager::new(quic_a.clone());
            let storage_a = FsStorage::new();
            secure_send_file(
                &ctx,
                &quic_a,
                &routes_a,
                &device_b,
                &sc_a,
                &chat_a,
                &a_sink,
                &storage_a,
                src.to_str().expect("utf8 src"),
                "report.pdf",
                content.len() as u64,
                64 * 1024,
                None,
            )
            .await
        };

        let b_side = async {
            let qc = incoming_b
                .next()
                .await
                .expect("inbound connection")
                .expect("accepted");
            let mut session_b = crate::session_transfer::accept(
                qc,
                &RouteManager::new(quic_b.clone()),
                &sc_b.ident,
                &sc_b.enc,
                &sc_b.trust,
                Some((chat_b.clone(), b_sink)),
            )
            .await
            .expect("accept b");

            // Flush-on-connect, mirroring `serve_loop`'s (fixed, post-pairing-
            // gate) ordering — there is no pairing gate to simulate here since
            // this test dials straight in without discovery/TOFU-prompt state.
            let peer = DeviceId::from(session_b.peer_id.clone());
            let _ = peerbeam_chat::flush_to_session(&session_b.handle, &chat_b, &peer).await;

            // Service a's incoming file transfer channel so a's send actually
            // completes (secure_send_file would otherwise hang/error waiting
            // for a peer that never opens/drains the channel).
            let incoming_ch = session_b
                .next_incoming()
                .await
                .expect("incoming transfer channel");
            let storage_b = FsStorage::new();
            let (ptx, mut prx) = mpsc::unbounded_channel();
            let ctrl = TransferControl::new();
            let recv_dir = dir_b.path().join("recv");
            std::fs::create_dir_all(&recv_dir).expect("recv dir");
            let handle_b = &session_b.handle;
            let recv = async {
                let r = receive_on_channel(
                    incoming_ch,
                    handle_b,
                    &storage_b,
                    recv_dir.to_str().expect("utf8 recv dir"),
                    &ctrl,
                    &ptx,
                )
                .await;
                drop(ptx);
                r
            };
            let drain = async { while prx.recv().await.is_some() {} };
            let (r, _) = tokio::join!(recv, drain);
            r.expect("receive file");
            session_b.close().await;
        };

        let (a_result, ()) = tokio::join!(a_side, b_side);
        a_result.expect("secure_send_file succeeds");

        // The actual bug this guards: with `chat: None` on a's dial, this
        // history would be empty even though b's outbox/history already show
        // the message delivered — a would simply never know.
        let b_device_id = sc_b.ident.device_id.clone();
        let hist_a = chat_a.history(&b_device_id).expect("history a");
        assert_eq!(
            hist_a.len(),
            1,
            "a must have received b's pushed message: {hist_a:?}"
        );
        assert_eq!(hist_a[0].body, "queued while you were offline");

        // And b's own bookkeeping is consistent: delivered + dequeued.
        assert!(chat_b.outbox_for(&a_id).expect("outbox").is_empty());
    }
}

/// `settle_received_chat_file` (`serve_loop`'s receive-side chat bridge) —
/// unit-tested directly against a real `ChatStore` rather than through a live
/// QUIC receive, since the function's whole job is dispatching into
/// `peerbeam_chat`'s guarded `settle_file_row`/`set_file_row_path` correctly
/// (peer, id, direction, ordering), and that guard's own authorization logic
/// (kind/direction/status, including the hostile-collision cases and their
/// mutation proof) is already exhaustively tested where it lives —
/// `peerbeam-chat/src/store.rs`. These tests are the CLI-side counterpart:
/// do `serve_loop`'s call sites pass the right namespace, direction, and
/// ordering.
#[cfg(test)]
mod chat_receive_bridge_tests {
    use super::settle_received_chat_file;
    use peerbeam_chat::{ChatMessage, ChatRecord, ChatStore, FileMeta, FileRef, Kind, Status};
    use peerbeam_crypto::{derive_subkey, AeadCrypto};
    use peerbeam_domain::id::DeviceId;
    use peerbeam_domain::port::EncryptionProvider;
    use std::sync::Arc;

    fn seeded_store() -> (ChatStore, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("tempdir");
        let enc: Arc<dyn EncryptionProvider> = Arc::new(AeadCrypto::new());
        let key = derive_subkey(&[11u8; 32], b"peerbeam-appstore-v1");
        let app = Arc::new(peerbeam_appstore_fs::FsAppStore::open(
            dir.path().join("appstore"),
            key,
            enc,
        ));
        (ChatStore::new(app), dir)
    }

    /// An `AppStore` that reads like the real one and stops accepting writes on
    /// command — the disk that fills up between a file landing and its row
    /// being settled, without needing one. Reads keep working on purpose: the
    /// guard inside `settle_file_row` has to see the real record it is deciding
    /// about, so what is being tested is the write failing and nothing else.
    struct FailingWrites {
        inner: peerbeam_appstore_fs::FsAppStore,
        refuse: std::sync::atomic::AtomicBool,
    }

    impl FailingWrites {
        fn refuse_writes(&self) {
            self.refuse.store(true, std::sync::atomic::Ordering::SeqCst);
        }
        fn check(&self) -> peerbeam_domain::error::Result<()> {
            if self.refuse.load(std::sync::atomic::Ordering::SeqCst) {
                return Err(peerbeam_domain::error::DomainError::Storage(
                    "No space left on device".into(),
                ));
            }
            Ok(())
        }
    }

    impl peerbeam_domain::port::AppStore for FailingWrites {
        fn put(&self, ns: &str, key: &str, value: &[u8]) -> peerbeam_domain::error::Result<()> {
            self.check()?;
            self.inner.put(ns, key, value)
        }
        fn get(&self, ns: &str, key: &str) -> peerbeam_domain::error::Result<Option<Vec<u8>>> {
            self.inner.get(ns, key)
        }
        fn list(&self, ns: &str) -> peerbeam_domain::error::Result<Vec<(String, Vec<u8>)>> {
            self.inner.list(ns)
        }
        fn namespaces(&self, prefix: &str) -> peerbeam_domain::error::Result<Vec<String>> {
            self.inner.namespaces(prefix)
        }
        fn delete(&self, ns: &str, key: &str) -> peerbeam_domain::error::Result<bool> {
            self.check()?;
            self.inner.delete(ns, key)
        }
        fn clear(&self, ns: &str) -> peerbeam_domain::error::Result<()> {
            self.check()?;
            self.inner.clear(ns)
        }
    }

    /// **A store failure used to leave no trace at all.** All three writes were
    /// `let _ = …`, so a receive whose row could not be updated printed
    /// "received report.pdf" and left the conversation showing the file as
    /// still waiting for approval — an Accept button for a transfer that had
    /// already finished, and nothing anywhere saying why.
    ///
    /// Two things are asserted, and the second is the reason this is returned
    /// rather than propagated: the caller learns about it, and the row is left
    /// *in flight* rather than settled with the sender's unverified claim —
    /// which the next startup reconcile can still turn into `Interrupted`.
    #[test]
    fn a_store_that_cannot_write_reports_the_failure_instead_of_dropping_it() {
        let dir = tempfile::tempdir().expect("tempdir");
        let enc: Arc<dyn EncryptionProvider> = Arc::new(AeadCrypto::new());
        let key = derive_subkey(&[11u8; 32], b"peerbeam-appstore-v1");
        let store = Arc::new(FailingWrites {
            inner: peerbeam_appstore_fs::FsAppStore::open(dir.path().join("appstore"), key, enc),
            refuse: std::sync::atomic::AtomicBool::new(false),
        });
        let chat = ChatStore::new(store.clone());

        let peer = DeviceId::from("pb-bob");
        let r = FileRef::new("report.pdf", 4096).unwrap();
        chat.append(&ChatRecord::file_in(&peer, &r)).unwrap();

        store.refuse_writes();
        let err = settle_received_chat_file(
            &chat,
            "pb-bob",
            &r.id,
            Status::Received,
            Some(("report.pdf", 4096)),
            Some("/home/me/Downloads/report.pdf"),
        )
        .expect_err("a row that could not be written must not report success");
        assert!(
            err.to_string().contains("No space left on device"),
            "the reason has to reach the caller, got: {err}"
        );

        assert_eq!(
            chat.get(&peer, &r.id).unwrap().unwrap().status,
            Status::PendingApproval,
            "the row is left in flight, so a later reconcile can still settle it"
        );
    }

    /// A completed receive settles the chat row `Received` and records where
    /// it landed — this is the fix: without it, a file shared via chat and
    /// successfully received here stayed `PendingApproval` forever.
    #[test]
    fn a_completed_receive_settles_received_with_its_local_path() {
        let (chat, _dir) = seeded_store();
        let peer = DeviceId::from("pb-bob");
        let r = FileRef::new("report.pdf", 4096).unwrap();
        chat.append(&ChatRecord::file_in(&peer, &r)).unwrap(); // In/File/PendingApproval

        settle_received_chat_file(
            &chat,
            "pb-bob",
            &r.id,
            Status::Received,
            Some(("report.pdf", 4096)),
            Some("/home/me/Downloads/report.pdf"),
        )
        .expect("a working store must accept these writes");

        let rec = chat.get(&peer, &r.id).unwrap().expect("row");
        assert_eq!(rec.status, Status::Received);
        assert_eq!(
            rec.file.unwrap().local_path.as_deref(),
            Some("/home/me/Downloads/report.pdf")
        );
    }

    /// **The mismatch.** The peer's CHAT-channel `FileRef` advertised
    /// `holiday.jpg · 180 KB`; the TRANSFER stream it is correlated with —
    /// *by id alone* — landed something else entirely. The settled row must
    /// describe what is on disk, because that is what its "open" action hands
    /// the OS. Before this, a row stayed permanently labelled with the
    /// advertisement while pointing at the other file.
    #[test]
    fn a_receive_settles_with_what_landed_not_with_what_the_file_ref_claimed() {
        let (chat, _dir) = seeded_store();
        let peer = DeviceId::from("pb-bob");
        let mut offered = FileRef::new("holiday.jpg", 184_320).unwrap();
        offered.name = "holiday.jpg".into();
        chat.append(&ChatRecord::file_in(&peer, &offered)).unwrap();

        settle_received_chat_file(
            &chat,
            "pb-bob",
            &offered.id,
            Status::Received,
            Some(("invoice-2026.pdf.exe", 4_000_000_000)),
            Some("/home/me/Downloads/invoice-2026.pdf.exe"),
        )
        .expect("a working store must accept these writes");

        let meta = chat
            .get(&peer, &offered.id)
            .unwrap()
            .expect("row")
            .file
            .expect("file meta");
        assert_eq!(
            meta.name, "invoice-2026.pdf.exe",
            "the row must name the file that landed, not the one advertised"
        );
        assert_eq!(meta.size, 4_000_000_000);
        assert_eq!(
            meta.local_path.as_deref(),
            Some("/home/me/Downloads/invoice-2026.pdf.exe"),
            "and the open target must be the same file the label names"
        );
    }

    /// The row leaves `PendingApproval` the moment bytes start moving, and is
    /// still writable afterwards — `Transferring` is inside the guard's
    /// in-flight set, so the later `Received` + path + landing all still land.
    /// A row that stayed `PendingApproval` for the whole download would read
    /// as still waiting on a decision that was already made.
    #[test]
    fn a_receive_marks_the_row_transferring_and_still_settles_received_after() {
        let (chat, _dir) = seeded_store();
        let peer = DeviceId::from("pb-bob");
        let r = FileRef::new("report.pdf", 4096).unwrap();
        chat.append(&ChatRecord::file_in(&peer, &r)).unwrap();

        // serve_loop's post-peek call.
        settle_received_chat_file(
            &chat,
            "pb-bob",
            &r.id,
            Status::Transferring,
            Some(("report.pdf", 4096)),
            None,
        )
        .expect("a working store must accept these writes");
        assert_eq!(
            chat.get(&peer, &r.id).unwrap().unwrap().status,
            Status::Transferring,
            "the row must not still claim it is awaiting a decision"
        );

        // …and the completion still lands on it.
        settle_received_chat_file(
            &chat,
            "pb-bob",
            &r.id,
            Status::Received,
            Some(("report.pdf", 4096)),
            Some("/home/me/Downloads/report.pdf"),
        )
        .expect("a working store must accept these writes");
        let rec = chat.get(&peer, &r.id).unwrap().expect("row");
        assert_eq!(rec.status, Status::Received);
        assert_eq!(
            rec.file.unwrap().local_path.as_deref(),
            Some("/home/me/Downloads/report.pdf"),
            "going Transferring first must not close the row to its own completion"
        );
    }

    /// A cancelled/failed receive settles `Failed`, with no path (there may
    /// be no complete file to point at).
    #[test]
    fn a_failed_receive_settles_failed_without_a_path() {
        let (chat, _dir) = seeded_store();
        let peer = DeviceId::from("pb-bob");
        let r = FileRef::new("report.pdf", 4096).unwrap();
        chat.append(&ChatRecord::file_in(&peer, &r)).unwrap();

        settle_received_chat_file(&chat, "pb-bob", &r.id, Status::Failed, None, None)
            .expect("a working store must accept these writes");

        let rec = chat.get(&peer, &r.id).unwrap().expect("row");
        assert_eq!(rec.status, Status::Failed);
        assert!(rec.file.unwrap().local_path.is_none());
    }

    /// The ordinary case, by far the most common: a plain (non-chat)
    /// transfer's id has no chat row at all. Must be a complete no-op — no
    /// row is ever invented for an ordinary file transfer.
    #[test]
    fn a_plain_non_chat_receive_writes_nothing() {
        let (chat, _dir) = seeded_store();
        let peer = DeviceId::from("pb-bob");
        settle_received_chat_file(
            &chat,
            "pb-bob",
            "tx-plain-42",
            Status::Received,
            Some(("other.bin", 7)),
            Some("/tmp/x"),
        )
        .expect("a working store must accept these writes");
        assert!(chat.get(&peer, "tx-plain-42").unwrap().is_none());
    }

    /// An empty `transfer_id` (nothing could be peeked — `peek_incoming_meta`
    /// timed out or the first frame was malformed) must be a no-op too,
    /// never an attempted lookup under an empty key.
    #[test]
    fn an_empty_transfer_id_is_a_no_op() {
        let (chat, _dir) = seeded_store();
        let peer = DeviceId::from("pb-bob");
        chat.append(&ChatRecord::sent(&peer, &ChatMessage::new("hi").unwrap()))
            .unwrap();
        settle_received_chat_file(
            &chat,
            "pb-bob",
            "",
            Status::Received,
            Some(("x.bin", 1)),
            Some("/tmp/x"),
        )
        .expect("a working store must accept these writes");
        // The one real row in this conversation must be completely untouched.
        let hist = chat.history(&peer).unwrap();
        assert_eq!(hist.len(), 1);
        assert_eq!(hist[0].body, "hi");
    }

    /// The hostile case (a): an already-paired peer's ordinary transfer's
    /// peer-supplied `transfer_id` collides with the id of OUR OWN outbound
    /// text message in that thread. Must write and emit nothing — proven at
    /// the CLI's own call site, not just in `peerbeam-chat`'s guard tests.
    #[test]
    fn hostile_collision_with_an_existing_text_row_writes_nothing() {
        let (chat, _dir) = seeded_store();
        let peer = DeviceId::from("pb-bob");
        let msg = ChatMessage::new("hello there").unwrap();
        chat.append(&ChatRecord::sent(&peer, &msg)).unwrap(); // Out/Text/Sent

        settle_received_chat_file(
            &chat,
            "pb-bob",
            &msg.id,
            Status::Received,
            Some(("evil.exe", 1)),
            Some("/tmp/x"),
        )
        .expect("a working store must accept these writes");

        let rec = chat
            .get(&peer, &msg.id)
            .unwrap()
            .expect("row still present");
        assert_eq!(rec.kind, Kind::Text, "kind must be untouched");
        assert_eq!(rec.status, Status::Sent, "status must be untouched");
        assert_eq!(rec.body, "hello there", "body must be untouched");
        assert!(
            rec.file.is_none(),
            "the landing write must not conjure file metadata onto a text row"
        );
    }

    /// The hostile case (b): a peer-supplied `transfer_id` collides with a
    /// FILE row we already settled (e.g. one we declined). Must write and
    /// emit nothing — a declined file must never flip to `Received`.
    #[test]
    fn hostile_collision_with_an_already_settled_file_row_writes_nothing() {
        let (chat, _dir) = seeded_store();
        let peer = DeviceId::from("pb-bob");
        let r = FileRef::new("suspicious.exe", 4096).unwrap();
        let mut declined = ChatRecord::file_in(&peer, &r);
        declined.status = Status::Declined;
        chat.append(&declined).unwrap();

        settle_received_chat_file(
            &chat,
            "pb-bob",
            &r.id,
            Status::Received,
            Some(("evil.exe", 1)),
            Some("/tmp/x"),
        )
        .expect("a working store must accept these writes");

        let rec = chat.get(&peer, &r.id).unwrap().expect("row still present");
        assert_eq!(
            rec.status,
            Status::Declined,
            "a declined file must not flip to Received"
        );
        assert_eq!(
            rec.file.expect("file meta").name,
            "suspicious.exe",
            "nor may it be relabelled — the landing write shares the same guard"
        );
    }

    /// A sender's own row (`Out`/`File`/`Transferring`) must not be
    /// settleable by a *receive*-side call — direction must agree, not just
    /// id and kind.
    #[test]
    fn a_sending_row_is_not_settled_by_a_receive_call() {
        let (chat, _dir) = seeded_store();
        let peer = DeviceId::from("pb-bob");
        let r = FileRef::new("report.pdf", 4096).unwrap();
        let meta = FileMeta::new(&r.name, r.size, Some("/tmp/report.pdf".into()));
        chat.append(&ChatRecord::file_out(&peer, &r, meta, Status::Transferring))
            .unwrap();

        settle_received_chat_file(
            &chat,
            "pb-bob",
            &r.id,
            Status::Received,
            Some(("evil.exe", 1)),
            Some("/tmp/x"),
        )
        .expect("a working store must accept these writes");

        assert_eq!(
            chat.get(&peer, &r.id).unwrap().unwrap().status,
            Status::Transferring,
            "an outbound row must not be settled by the receive-side bridge"
        );
    }
}

#[cfg(test)]
mod clipboard_history_tests {
    #[test]
    fn a_history_listing_never_reproduces_a_whole_clip() {
        // This prints into a terminal that keeps scrollback. Bounding the log
        // at fifty entries would mean nothing if listing it dumped every
        // remembered secret at once.
        let long = "s3cr3t-".repeat(40);
        let shown = super::abbreviate(&long);
        assert!(shown.chars().count() <= 60);
        assert!(shown.ends_with('\u{2026}'));
        assert!(!shown.contains(&long));
    }

    #[test]
    fn a_multi_line_clip_is_previewed_by_its_first_line_only() {
        assert_eq!(super::abbreviate("first\nsecond\nthird"), "first");
        assert_eq!(super::abbreviate(""), "");
    }
}

#[cfg(test)]
mod scheduled_send_tests {
    use super::delay_until;
    use chrono::{Local, TimeZone};

    fn at(h: u32, m: u32) -> chrono::DateTime<Local> {
        Local
            .with_ymd_and_hms(2026, 8, 19, h, m, 0)
            .single()
            .expect("a real local time")
    }

    #[test]
    fn a_time_later_today_waits_until_today() {
        let d = delay_until(at(9, 0), "17:30").expect("a valid time");
        assert_eq!(d.as_secs(), 8 * 3600 + 30 * 60);
    }

    /// Nobody typing `09:00` at ten in the morning means nine hours ago.
    #[test]
    fn a_time_already_past_means_tomorrow() {
        let d = delay_until(at(10, 0), "09:00").expect("a valid time");
        assert_eq!(d.as_secs(), 23 * 3600);
    }

    #[test]
    fn a_full_datetime_is_accepted() {
        let d = delay_until(at(9, 0), "2026-08-19T09:30:00").expect("a valid time");
        assert_eq!(d.as_secs(), 30 * 60);
    }

    #[test]
    fn a_past_datetime_is_refused_rather_than_sent_immediately() {
        // Sending at once would be a surprise: the user asked for a time, and
        // silently ignoring it is not the same as honouring it.
        assert!(delay_until(at(9, 0), "2026-08-19T08:00:00").is_err());
    }

    #[test]
    fn unreadable_input_says_what_was_expected() {
        let err = delay_until(at(9, 0), "next tuesday").expect_err("not a time");
        let msg = format!("{err}");
        assert!(
            msg.contains("HH:MM"),
            "the error did not name a format: {msg}"
        );
    }
}

#[cfg(test)]
mod duration_tests {
    use super::{humantime, parse_duration};

    #[test]
    fn the_four_units_parse() {
        assert_eq!(parse_duration("45s").unwrap().num_seconds(), 45);
        assert_eq!(parse_duration("30m").unwrap().num_seconds(), 30 * 60);
        assert_eq!(parse_duration("2h").unwrap().num_seconds(), 2 * 3600);
        assert_eq!(parse_duration("7d").unwrap().num_seconds(), 7 * 86_400);
    }

    /// Case is forgiving and surrounding whitespace is trimmed, because a shell
    /// quoting accident should not cost an operator a second attempt.
    #[test]
    fn case_and_padding_are_forgiven() {
        assert_eq!(parse_duration("30M").unwrap().num_seconds(), 30 * 60);
        assert_eq!(parse_duration(" 2H ").unwrap().num_seconds(), 2 * 3600);
    }

    /// **A bare number is refused.** `--for 30` is thirty of something, and on
    /// this flag the two obvious readings differ by sixty times in how long a
    /// device keeps access to a clipboard. Guessing is the one thing this must
    /// not do.
    #[test]
    fn a_bare_number_is_refused_rather_than_guessed_at() {
        let err = parse_duration("30").expect_err("no unit");
        assert_eq!(err.code(), 2, "a bad duration is a usage error");
        let msg = format!("{err}");
        assert!(msg.contains("30m"), "the error must show the shape: {msg}");
    }

    /// Everything that is not `<number><unit>` is refused, including the
    /// compound form (`1h30m`) this deliberately does not support: accepting it
    /// halfway would mean deciding what `1h30` and `1h30s` mean.
    #[test]
    fn nonsense_is_refused_and_names_the_units() {
        for spec in ["", "m", "abc", "1w", "1 h", "1h30m", "--for", "30mn"] {
            let err = parse_duration(spec).expect_err("`{spec}` is not a duration");
            assert_eq!(err.code(), 2, "{spec}");
            let msg = format!("{err}");
            assert!(
                msg.contains("s, m, h, d"),
                "{spec}: the error must name the units: {msg}"
            );
        }
    }

    /// Zero and negative are not windows. Rounding them to "already expired"
    /// would write a grant that was over before the receipt printed; saying so
    /// points at the two things the operator probably meant instead.
    #[test]
    fn zero_and_negative_are_refused_with_the_alternatives_named() {
        for spec in ["0m", "0s", "-5m"] {
            let err = parse_duration(spec).expect_err("{spec} is not a window");
            assert_eq!(err.code(), 2);
            let msg = format!("{err}");
            assert!(
                msg.contains("revoke") && msg.contains("`--for`"),
                "{spec}: the error must name the alternatives: {msg}"
            );
        }
    }

    /// A window nobody could mean must not panic on the multiply — `chrono`
    /// saturates or wraps depending on the call, and this one has to say no.
    #[test]
    fn an_absurd_window_is_an_error_not_a_panic() {
        let err = parse_duration("9223372036854775807d").expect_err("far too long");
        assert_eq!(err.code(), 2);
    }

    /// A multi-byte final character is not a unit, and must be reported as such
    /// rather than panicking on a slice that lands mid-character.
    #[test]
    fn a_multibyte_unit_is_refused_without_panicking() {
        assert!(parse_duration("30µ").is_err());
        assert!(parse_duration("30日").is_err());
    }

    /// [`parse_duration`] and [`humantime`] are inverses in the vocabulary they
    /// share, which is what lets `--for 2h` and the row it produces obviously be
    /// about the same thing.
    #[test]
    fn what_is_typed_reads_back_as_what_is_printed() {
        for (spec, printed) in [
            ("45s", "45s"),
            ("30m", "30m"),
            ("2h", "2h00m"),
            ("7d", "7d00h"),
        ] {
            let parsed = parse_duration(spec).unwrap();
            let seconds = std::time::Duration::from_secs(parsed.num_seconds() as u64);
            assert_eq!(humantime(seconds), printed, "{spec}");
        }
    }
}
