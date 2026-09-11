//! `peerbeam identify` — ask an address which device answers there.
//!
//! A conversation is filed under a peer's **authenticated** device id: the chat
//! store keys rows by it, and inbound records arrive keyed by it. Two kinds of
//! peer reach a surface without one. A Tailscale-discovered device is known by
//! Tailscale's node id (`ts:<node>`), which is not PeerBeam's name for it and
//! which the store refuses outright — a colon is not a legal namespace
//! character. An address typed by hand has no id at all.
//!
//! Neither can be guessed, so this asks: dial, complete the ordinary
//! authenticated handshake, report who answered, close.
//!
//! **Nothing is sent and nothing is granted.** No file, no message, not a typed
//! frame of any kind. The handshake pins a key exactly as any first contact
//! does; approving a device stays a separate, explicit act. The pairing code is
//! printed for the same reason the receive prompt shows one — on first contact
//! it is the only way to know the right machine answered.

use std::sync::Arc;

use peerbeam_domain::entity::{Device, DeviceType, Platform};
use peerbeam_domain::id::DeviceId;

use crate::cli::IdentifyArgs;
use crate::commands::{self, SecureCtx};
use crate::exit::{CliError, CliResult};
use crate::output::Ctx;
use crate::session_transfer;

pub async fn identify(ctx: &Ctx, args: IdentifyArgs, path_override: Option<&str>) -> CliResult {
    let config = commands::load_config(path_override)?;
    let sc = SecureCtx::build(&config)?;

    // A stand-in device: an address and a port and nothing else, which is
    // exactly what the caller has. The id here is a placeholder that never
    // leaves this process — the whole point is that the real one comes back
    // from the handshake.
    let device = Device {
        id: DeviceId::from("identify"),
        name: args.host.clone(),
        // Placeholders. Neither is known before the handshake and neither is
        // used by the dial; the answer is what this call is for.
        device_type: DeviceType::Desktop,
        platform: Platform::Linux,
        addresses: vec![args.host.clone()],
        port: args.port,
        last_seen: chrono::Utc::now(),
    };

    let quic = Arc::new(peerbeam_transfer_quic::QuicTransport::new().map_err(CliError::from)?);
    let routes = peerbeam_engine::RouteManager::new(quic.clone());
    let session = session_transfer::dial(
        &quic, &routes, &device, "identify", &sc.ident, &sc.enc, &sc.trust, None,
    )
    .await
    .map_err(|e| CliError::Other(format!("could not reach {}:{}: {e}", args.host, args.port)))?;

    let device_id = session.peer_id.clone();
    let newly_trusted = session.newly_trusted;
    let pairing_code = session.pairing_code.clone();
    session.close().await;

    if ctx.json {
        ctx.json_line(&serde_json::json!({
            "event": "identify",
            "host": args.host,
            "port": args.port,
            "device_id": device_id,
            "newly_trusted": newly_trusted,
            "pairing_code": pairing_code,
        }));
        return Ok(());
    }

    ctx.line(&device_id);
    if newly_trusted {
        // Said plainly rather than buried: this is the one moment the key can
        // be checked, and the check is the user's to make.
        ctx.line(&ctx.dim(&format!(
            "first contact — pairing code {pairing_code}; compare it with the \
             other device before trusting this one"
        )));
    }
    Ok(())
}
