//! `peerbeam pipe` — an encrypted byte stream between two devices.
//!
//! ```text
//! $ tar cz ./project | peerbeam pipe --to laptop
//! $ peerbeam pipe --listen > project.tgz
//! ```
//!
//! # stdout is reserved
//!
//! **Every human-facing line this module emits goes to stderr**, in both
//! directions, `--json` included ([`Ctx::note`]/[`Ctx::json_note`]). On
//! `--listen` stdout *is* the payload, so a status line printed there would be
//! written into the user's file; on `--to` stdout is unused, and keeping one
//! rule for the command is worth more than the freedom to break it on the half
//! where it would not (yet) hurt.
//!
//! # Consent
//!
//! Sending needs no gate beyond the user's own command: the bytes are theirs.
//! Receiving has two, neither optional, and they are why this command exists as
//! its own listener instead of riding `peerbeam receive`:
//!
//! 1. **Only a `pipe --listen` accepts a pipe.** A running `receive`, `daemon`
//!    or `chat watch` refuses every one, as does the Flutter GUI. Running this
//!    command *is* the approval — which is why there is no prompt, and why
//!    there must not be one: a prompt would read stdin, which on the sending
//!    side is the payload, and would break the scripted headless use the
//!    feature exists for.
//! 2. **Trusted peers only**, optionally narrowed to one device with `--from`.
//!
//! Both are decided by `peerbeam_transfer::may_accept_pipe`, reached through
//! the single `accept_pipe` funnel. See `docs/SECURITY.md` for why this differs
//! from the file-transfer approval prompt.
//!
//! # One stream, then exit
//!
//! A listener takes one stream and exits. A listener that stayed open accepting
//! stream after stream is a far larger surface than the one the user consented
//! to at the shell. A *refused* attempt does not count and does not end the
//! listener — otherwise any stranger could kill it with a single dial.

use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use serde_json::json;
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

use peerbeam_config::EngineConfig;
use peerbeam_domain::id::DeviceId;
use peerbeam_domain::session::ChannelType;
use peerbeam_engine::RouteManager;
use peerbeam_transfer::{accept_pipe, send_pipe_on_session, PipeConsent, PipeStats};
use peerbeam_transfer_quic::QuicTransport;

use crate::cli::PipeArgs;
use crate::commands::{
    chat_store, clamp_chunk_size, human_bytes, load_config, resolve_addr, resolve_peer, snapshot,
    target_device, SecureCtx,
};
use crate::exit::{CliError, CliResult};
use crate::output::Ctx;

/// How long a listener waits, after a peer has connected and authenticated, for
/// that peer to actually open a pipe channel.
///
/// A peer may dial for something else entirely (a queued chat message pushed on
/// connect, say), so this is not an error path — it is how the listener stops
/// holding a session that is never going to carry a pipe and goes back to
/// waiting. Generous, because the cost of erring long is only that one
/// mis-dialled connection lingers, while erring short would drop a legitimate
/// sender that was slow to get going.
const STREAM_GRACE: Duration = Duration::from_secs(30);

/// Dispatch `peerbeam pipe`. Exactly one direction is set — clap's `direction`
/// group makes that a parse error rather than a runtime one.
pub async fn pipe(ctx: &Ctx, args: PipeArgs, path_override: Option<&str>) -> CliResult {
    if args.listen {
        listen(ctx, args, path_override).await
    } else {
        send(ctx, args, path_override).await
    }
}

// ── Sending: stdin → peer ───────────────────────────────────────────────────

/// Read stdin to EOF and stream it to a peer.
///
/// **Nothing prompts.** `send`'s "Send N file(s) to X?" confirmation has no
/// counterpart here on purpose: a prompt reads a line from stdin, and stdin is
/// the payload. Even with a terminal on stdin — a user typing input — the first
/// line would be eaten as an answer. Running the command is the intent.
async fn send(ctx: &Ctx, args: PipeArgs, path_override: Option<&str>) -> CliResult {
    let config = load_config(path_override)?;
    crate::presence::configure(&config);

    let target = match (&args.addr, &args.to) {
        (Some(addr), _) => {
            let sa = resolve_addr(addr)?;
            target_device(addr.clone(), sa.ip().to_string(), sa.port())
        }
        (None, Some(_)) => {
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
            dev
        }
        // Unreachable: clap's `direction` group requires one of the three, and
        // `listen` was handled by the caller. Reported rather than unwrapped.
        (None, None) => return Err(CliError::Usage("specify --to, --addr or --listen".into())),
    };
    if target.port == 0 {
        return Err(CliError::NotFound(format!(
            "{} did not advertise a transfer port",
            target.name
        )));
    }

    let sc = SecureCtx::build(&config)?;
    let quic = Arc::new(QuicTransport::new().map_err(CliError::from)?);
    let routes = RouteManager::new(quic.clone());
    // Chat wiring on this dial too, with a sink that displays nothing: the
    // handler still persists whatever the peer pushes back over this session,
    // so nothing is silently dropped, while stdout stays reserved. See
    // `crate::chat::silent_sink`.
    let chat = chat_store(&config, &sc.enc, &sc.ident);
    let session = crate::session_transfer::dial(
        &quic,
        &routes,
        &target,
        "pipe",
        &sc.ident,
        &sc.enc,
        &sc.trust,
        Some((chat, crate::chat::silent_sink())),
    )
    .await?;

    let peer_id = session.peer_id.clone();
    // **Before a byte of stdin is read.** A peer whose build predates
    // `peerbeam pipe` negotiates the capability away, and the honest thing is
    // to say so and exit rather than consume the user's stdin — which, for
    // `tar cz . | peerbeam pipe`, cannot be handed back.
    if !session.supports_pipe() {
        session.close().await;
        return Err(CliError::Unavailable(format!(
            "{peer_id} cannot receive a pipe — its build predates `peerbeam pipe`, so it never \
             negotiated the pipe capability. Nothing was read from stdin"
        )));
    }
    if session.newly_trusted {
        ctx.note(&ctx.err_dim(&format!("pinned new peer {peer_id}")));
        ctx.note(&format!(
            "  pairing code: {}",
            ctx.err_bold(&session.pairing_code)
        ));
    }

    let chunk = clamp_chunk_size(config.transfer.chunk_size);
    let mut stdin = tokio::io::stdin().compat();
    let result = send_pipe_on_session(&session.handle, &mut stdin, chunk).await;
    // Capture, close, THEN propagate — a `?` before `close()` would leak the
    // session's pump task on exactly the failure path where it matters.
    session.close().await;
    let stats = result.map_err(|e| refusal_hint(e, &peer_id))?;

    report(ctx, "out", &peer_id, stats);
    Ok(())
}

/// Turn an engine error from the send path into a CLI error, adding the one
/// thing the sender cannot learn from the wire.
///
/// A refusal reaches the sender as its channel dying, with no reason attached —
/// deliberately, since "you are not running `pipe --listen`" and "I revoked your
/// device" must look identical to a peer. So the *likely* cause is named here,
/// as a hint rather than as a claim, because an unreachable receiver and a
/// refusing one are genuinely indistinguishable from this side.
///
/// Both stream-death variants are covered: a refusal surfaces as
/// `Transfer("link closed before verify")` when the writes landed before the
/// channel died and as `Connection(_)` when they did not, and the user's
/// question is the same either way. An `Integrity` failure is left alone — that
/// one has a definite cause and the hint would be a lie.
fn refusal_hint(e: peerbeam_domain::error::DomainError, peer: &str) -> CliError {
    use peerbeam_domain::error::DomainError as D;
    match e {
        D::Connection(msg) | D::Transfer(msg) => CliError::Connection(format!(
            "{msg} — {peer} may not be running `peerbeam pipe --listen`, which is the only \
             thing that accepts a pipe (a running receive/daemon/watch refuses one)"
        )),
        other => CliError::from(other),
    }
}

// ── Listening: peer → stdout ────────────────────────────────────────────────

/// Accept one incoming stream, write it to stdout, and exit.
async fn listen(ctx: &Ctx, args: PipeArgs, path_override: Option<&str>) -> CliResult {
    let config = load_config(path_override)?;
    crate::presence::configure(&config);
    let port = args.port.unwrap_or(config.transfer.port);
    // Resolved BEFORE the socket is bound, so a `--from` naming a device that
    // cannot be resolved fails immediately instead of after a sender has
    // already connected and been refused for a reason nobody can act on.
    let only_from = resolve_from(ctx, &config, args.from.as_deref()).await?;

    let sc = SecureCtx::build(&config)?;
    let quic = Arc::new(QuicTransport::new().map_err(CliError::from)?);
    let addr = std::net::SocketAddr::from((std::net::Ipv4Addr::UNSPECIFIED, port));
    let (local, mut incoming) = quic.serve_channels_on(addr).await.map_err(CliError::from)?;
    announce(ctx, &local, only_from.as_ref());

    let accept_routes = RouteManager::new(quic.clone());
    let chat = chat_store(&config, &sc.enc, &sc.ident);

    while let Some(item) = incoming.next().await {
        let qc = match item {
            Ok(c) => c,
            Err(e) => {
                ctx.note(&ctx.err_dim(&format!("inbound rejected: {e}")));
                continue;
            }
        };
        let mut session = match crate::session_transfer::accept(
            qc,
            &accept_routes,
            &sc.ident,
            &sc.enc,
            &sc.trust,
            Some((chat.clone(), crate::chat::silent_sink())),
        )
        .await
        {
            Ok(s) => s,
            Err(e) => {
                ctx.note(&ctx.err_dim(&format!("session failed: {e}")));
                continue;
            }
        };
        let peer = DeviceId::from(session.peer_id.clone());

        let incoming_ch = match tokio::time::timeout(STREAM_GRACE, session.next_incoming()).await {
            Ok(Some(c)) => c,
            // Connected but opened no stream: a chat-only dial, or a sender that
            // gave up. Not an error — go back to waiting.
            Ok(None) | Err(_) => {
                ctx.note(&ctx.err_dim(&format!("{} connected without opening a pipe", peer.0)));
                session.close().await;
                continue;
            }
        };

        let consent = PipeConsent {
            listening: true,
            trust: sc.trust.as_ref(),
            only_from: only_from.as_ref(),
            negotiated: session.capabilities(),
        };
        // The same predicate `accept_pipe` enforces, asked here for a second
        // purpose: it decides the *loop's* control flow. A refusal wrote nothing
        // and must not end the listener, or any stranger could kill it with one
        // dial; a failure once bytes are out must end it, because they cannot be
        // un-written and a second stream would concatenate onto the wreckage.
        // Sniffing the error variant instead would confuse a refusal with a
        // mid-stream link error, which are both `Connection`.
        let permitted = incoming_ch.channel_type == ChannelType::PIPE && consent.permits(&peer);

        let mut stdout = tokio::io::stdout().compat_write();
        let result = accept_pipe(incoming_ch, &session.handle, &peer, &consent, &mut stdout).await;
        drop(stdout);
        session.close().await;

        match result {
            Ok(stats) => {
                report(ctx, "in", &peer.0, stats);
                return Ok(()); // one stream, then exit
            }
            Err(e) if !permitted => {
                ctx.note(&ctx.err_dim(&e.to_string()));
                continue;
            }
            Err(e) => return Err(CliError::from(e)),
        }
    }
    Err(CliError::Connection(
        "stopped listening before any pipe arrived".into(),
    ))
}

/// Resolve `--from` to an **authenticated** device id.
///
/// A `pb-…` value is taken verbatim, so a headless script needs no discovery. A
/// human name is resolved through discovery exactly as `send --to` resolves one,
/// and resolved **to an id** — the gate never compares the name a peer presents,
/// since a peer chooses its own name and matching on one would make `--from` a
/// suggestion rather than a restriction.
async fn resolve_from(
    ctx: &Ctx,
    config: &EngineConfig,
    from: Option<&str>,
) -> Result<Option<DeviceId>, CliError> {
    let Some(query) = from else {
        return Ok(None);
    };
    if query.starts_with("pb-") {
        return Ok(Some(DeviceId::from(query.to_string())));
    }
    let devices = snapshot(config.clone(), 2).await;
    let candidates: Vec<(String, String)> = devices
        .iter()
        .map(|m| (m.device.id.to_string(), m.device.name.clone()))
        .collect();
    let index = resolve_peer(ctx, &candidates, &Some(query.to_string()))?;
    Ok(Some(devices[index].device.id.clone()))
}

/// Say where we are listening — on **stderr**, so `> file` gets bytes only.
fn announce(ctx: &Ctx, local: &std::net::SocketAddr, only_from: Option<&DeviceId>) {
    let restricted = only_from.map(|d| d.0.clone());
    if ctx.json {
        ctx.json_note(&json!({
            "event": "pipe_listening",
            "addr": local.to_string(),
            "port": local.port(),
            "from": restricted,
        }));
        return;
    }
    let scope = match &restricted {
        Some(id) => format!(" (only from {id})"),
        None => String::new(),
    };
    ctx.note(&format!(
        "listening for one pipe on {}{scope} — writing it to stdout",
        ctx.err_bold(&local.to_string())
    ));
}

/// Report a completed stream on **stderr**, in both directions.
fn report(ctx: &Ctx, direction: &str, peer: &str, stats: PipeStats) {
    if ctx.json {
        ctx.json_note(&json!({
            "event": "piped",
            "direction": direction,
            "bytes": stats.bytes,
            "chunks": stats.chunks,
            "peer": peer,
        }));
    } else {
        let verb = if direction == "in" { "from" } else { "to" };
        ctx.note(&format!("piped {} {verb} {peer}", human_bytes(stats.bytes)));
    }
}
