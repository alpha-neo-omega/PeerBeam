//! `peerbeam chat` — send / history / watch.
//!
//! Every chat session rides the same PeerSession machinery as file transfers
//! (`session_transfer`): the Chat capability is advertised on every dial and
//! every accept, so a `chat send` is accepted regardless of what the session
//! was otherwise established for. This module only resolves peers, wires the
//! store, and presents the result — the actual send/receive logic lives in
//! `peerbeam_chat` (`send_message`, `ChatHandler`), reused unchanged.

use std::sync::Arc;

use futures::StreamExt;
use serde_json::json;

use peerbeam_chat::{send_message, ChatRecord, Direction, ReceivedSink};
use peerbeam_domain::id::DeviceId;
use peerbeam_engine::RouteManager;
use peerbeam_transfer_quic::QuicTransport;

use crate::cli::ChatAction;
use crate::commands::{self, SecureCtx};
use crate::engine::{build_engine, me};
use crate::exit::{CliError, CliResult};
use crate::output::Ctx;
use crate::session_transfer;

pub async fn chat(ctx: &Ctx, action: ChatAction, path_override: Option<&str>) -> CliResult {
    match action {
        ChatAction::Send { to, addr, text } => send(ctx, to, addr, text, path_override).await,
        ChatAction::History { peer } => history(ctx, peer, path_override),
        ChatAction::Watch { port } => watch(ctx, port, path_override).await,
    }
}

/// `chat send` — resolve the peer exactly like `commands::send`, dial a
/// PeerSession advertising Chat (no handler needed on this side — we only
/// send), and send one message.
async fn send(
    ctx: &Ctx,
    to: Option<String>,
    addr: Option<String>,
    text: String,
    path_override: Option<&str>,
) -> CliResult {
    let config = commands::load_config(path_override)?;

    // Resolve the target peer — directly (--addr) or via discovery. Mirrors
    // `commands::send`'s resolution exactly; not duplicated logic, just reuse
    // of the same helpers.
    let target = if let Some(addr) = &addr {
        let sa = commands::resolve_addr(addr)?;
        commands::target_device(addr.clone(), sa.ip().to_string(), sa.port())
    } else {
        let devices = commands::snapshot(config.clone(), 2).await;
        let candidates: Vec<(String, String)> = devices
            .iter()
            .map(|m| (m.device.id.to_string(), m.device.name.clone()))
            .collect();
        let index = commands::resolve_peer(ctx, &candidates, &to)?;
        let dev = devices[index].device.clone();
        if dev.addresses.is_empty() {
            return Err(CliError::NotFound(format!(
                "no reachable address for {}",
                dev.name
            )));
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
    let store = commands::chat_store(&config, &sc.enc, &sc.ident);
    let quic = Arc::new(QuicTransport::new().map_err(CliError::from)?);
    let routes = RouteManager::new(quic.clone());

    // Sender: the session advertises Chat (alongside Transfer) but registers
    // no handler here — this side only ever sends, never receives.
    let session = session_transfer::dial(
        &quic, &routes, &target, "chat", &sc.ident, &sc.enc, &sc.trust, None,
    )
    .await?;
    let newly_trusted = session.newly_trusted;
    let pairing_code = session.pairing_code.clone();
    if newly_trusted && !ctx.json {
        ctx.line(&ctx.dim(&format!("pinned new peer {}", session.peer_id)));
        ctx.line(&format!("  pairing code: {}", ctx.bold(&pairing_code)));
    }
    // The record's peer must be the session's *authenticated* peer id, not the
    // (possibly placeholder, for `--addr`) routing target id — chat history is
    // namespaced by device id, so using the routing id would let two different
    // `--addr` targets collide under (or split across) the same conversation.
    let peer = DeviceId::from(session.peer_id.clone());

    // Always close the session on every path: a post-dial failure here (peer
    // rejects the Chat channel, send times out) must not leak the QUIC
    // connection, its run-loop task, or the diagnostics it would otherwise
    // hold open. Capture the result WITHOUT short-circuiting, close, THEN
    // propagate — mirrors the FFI's `chat_send` fix.
    let result = send_message(&session.handle, &store, &peer, &text)
        .await
        .map_err(|e| CliError::Connection(e.to_string()));
    session.close().await;
    let rec = result?;

    if ctx.json {
        ctx.json_line(&json!({
            "event": "chat_sent",
            "id": rec.id,
            "peer": peer.0,
        }));
    } else {
        ctx.line(&ctx.green(&format!("sent to {}", target.name)));
    }
    Ok(())
}

/// `chat history <peer>` — print a conversation's persisted history.
fn history(ctx: &Ctx, peer: String, path_override: Option<&str>) -> CliResult {
    let config = commands::load_config(path_override)?;
    let sc = SecureCtx::build(&config)?;
    let store = commands::chat_store(&config, &sc.enc, &sc.ident);
    let peer_id = DeviceId::from(peer);
    let records = store
        .history(&peer_id)
        .map_err(|e| CliError::Other(e.to_string()))?;

    if ctx.json {
        ctx.json_line(&render_history_json(&records));
        return Ok(());
    }
    if records.is_empty() {
        ctx.line(&ctx.dim("no messages yet"));
        return Ok(());
    }
    for r in &records {
        let dir = match r.direction {
            Direction::Out => "out",
            Direction::In => "in",
        };
        ctx.line(&format!("[{}] {}: {}", r.timestamp, dir, r.body));
    }
    Ok(())
}

/// Render a conversation's records as `{"messages": [...]}` — the CLI's JSON
/// contract for `chat history --json`. Factored out so it's unit-testable
/// against a seeded store without a live PeerSession.
fn render_history_json(records: &[ChatRecord]) -> serde_json::Value {
    let messages: Vec<serde_json::Value> = records
        .iter()
        .map(|r| {
            json!({
                "id": r.id,
                "peer_id": r.peer_id,
                "direction": r.direction,
                "timestamp": r.timestamp,
                "body": r.body,
                "status": r.status,
            })
        })
        .collect();
    json!({ "messages": messages })
}

/// `chat watch` — serve inbound PeerSessions and print each received chat
/// message. Mirrors `commands::serve_loop`'s accept pattern (one connection at
/// a time, best-effort discoverability, no daemon IPC), but for Chat instead
/// of Transfer: there is no stream channel to await, since the `ChatHandler`
/// dispatches inbound frames from within the session's own pump task.
async fn watch(ctx: &Ctx, port: Option<u16>, path_override: Option<&str>) -> CliResult {
    let config = commands::load_config(path_override)?;
    let sc = SecureCtx::build(&config)?;
    let store = commands::chat_store(&config, &sc.enc, &sc.ident);
    let quic = Arc::new(QuicTransport::new().map_err(CliError::from)?);
    let bind_port = port.unwrap_or(config.transfer.port);
    let addr = std::net::SocketAddr::from((std::net::Ipv4Addr::UNSPECIFIED, bind_port));
    let (local, mut incoming) = quic.serve_channels_on(addr).await.map_err(CliError::from)?;

    if ctx.json {
        ctx.json_line(&json!({
            "event": "listening",
            "addr": local.to_string(),
            "port": local.port(),
        }));
    } else {
        ctx.line(&format!(
            "watching for chat messages on {} (Ctrl-C to stop)",
            ctx.bold(&local.to_string())
        ));
    }

    // Best-effort discoverability (so `chat send --to <name>` can find us).
    // Identity load failure here shouldn't abort an otherwise-working watch —
    // `SecureCtx::build` above already proved the identity file is usable,
    // but this stays defensive rather than assuming that (mirrors
    // `serve_loop`).
    let engine = build_engine(config.clone()).ok();
    if let (Some(engine), Ok(self_device)) = (&engine, me(&config)) {
        let _ = engine.start_discovery(self_device).await;
    }

    // Extracted (rather than capturing `ctx` by reference) because the sink
    // must be `'static` — it outlives this call, held by the `ChatHandler`
    // inside each accepted session's config.
    let json = ctx.json;
    let color = ctx.color;
    let sink: ReceivedSink = Arc::new(move |rec: ChatRecord| {
        if json {
            let line = json!({
                "event": "chat_received",
                "id": rec.id,
                "peer": rec.peer_id,
                "body": rec.body,
                "timestamp": rec.timestamp,
            });
            println!("{}", serde_json::to_string(&line).unwrap_or_default());
        } else {
            let line = format!("[{}] {}", rec.peer_id, rec.body);
            if color {
                println!("\x1b[32m{line}\x1b[0m");
            } else {
                println!("{line}");
            }
        }
    });

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
        let mut session = match session_transfer::accept(
            qc,
            &sc.ident,
            &sc.enc,
            &sc.trust,
            Some((store.clone(), sink.clone())),
        )
        .await
        {
            Ok(s) => s,
            Err(e) => {
                if !ctx.json {
                    ctx.line(&ctx.dim(&format!("session failed: {e}")));
                }
                continue;
            }
        };
        // Chat frames are dispatched entirely inside the session's own pump
        // task (via the just-registered `ChatHandler`); there is no stream
        // channel to await here. `next_incoming` resolves to `None` once the
        // peer closes its side — a `chat send` always closes right after
        // sending — which is the signal to close our side too and move on to
        // the next inbound connection.
        while session.next_incoming().await.is_some() {}
        session.close().await;
    }

    if let Some(engine) = &engine {
        let _ = engine.stop_discovery().await;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::render_history_json;
    use peerbeam_chat::{ChatMessage, ChatRecord, ChatStore};
    use peerbeam_crypto::{derive_subkey, AeadCrypto};
    use peerbeam_domain::id::DeviceId;
    use peerbeam_domain::port::EncryptionProvider;
    use std::sync::Arc;

    fn seeded_store() -> (ChatStore, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("tempdir");
        let enc: Arc<dyn EncryptionProvider> = Arc::new(AeadCrypto::new());
        let key = derive_subkey(&[3u8; 32], b"peerbeam-appstore-v1");
        let app = Arc::new(peerbeam_appstore_fs::FsAppStore::open(
            dir.path().join("appstore"),
            key,
            enc,
        ));
        (ChatStore::new(app), dir)
    }

    #[test]
    fn render_history_json_renders_seeded_records_chronologically() {
        let (store, _dir) = seeded_store();
        let peer = DeviceId::from("pb-bob");
        let m1 = ChatMessage::new("hi").expect("msg1");
        let m2 = ChatMessage::new("there").expect("msg2");
        store
            .append(&ChatRecord::sent(&peer, &m1))
            .expect("append sent");
        store
            .append(&ChatRecord::received(&peer, &m2))
            .expect("append received");

        let records = store.history(&peer).expect("history");
        let value = render_history_json(&records);
        let messages = value["messages"].as_array().expect("messages array");
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0]["id"], m1.id);
        assert_eq!(messages[0]["peer_id"], "pb-bob");
        assert_eq!(messages[0]["body"], "hi");
        assert_eq!(messages[0]["direction"], "out");
        assert_eq!(messages[0]["status"], "sent");
        assert_eq!(messages[1]["id"], m2.id);
        assert_eq!(messages[1]["body"], "there");
        assert_eq!(messages[1]["direction"], "in");
        assert_eq!(messages[1]["status"], "received");
    }

    #[test]
    fn render_history_json_empty_history_is_empty_array() {
        let value = render_history_json(&[]);
        assert_eq!(value["messages"].as_array().expect("array").len(), 0);
    }
}
