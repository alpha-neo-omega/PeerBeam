//! `peerbeam pair` — PIN pairing over a live connection.
//!
//! Two roles, and they are not symmetric. One device **shows** a PIN; the other
//! **types** it. The PIN never crosses the connection — only a proof derived
//! from it, over this handshake's transcript. That is what makes the six digits
//! worth anything: a machine in the middle runs two different handshakes, so a
//! proof it captures from one is useless on the other.

use std::sync::Arc;

use peerbeam_pairing::{Attempt, Pairing, Pin};

use crate::cli::PairArgs;
use crate::commands::{self, SecureCtx};
use crate::exit::{CliError, CliResult};
use crate::output::Ctx;
use crate::session_transfer;

pub async fn pair(ctx: &Ctx, args: PairArgs, path_override: Option<&str>) -> CliResult {
    let config = commands::load_config(path_override)?;
    let sc = SecureCtx::build(&config)?;
    let devices = commands::snapshot(config.clone(), 2).await;
    let candidates: Vec<(String, String)> = devices
        .iter()
        .map(|m| (m.device.id.to_string(), m.device.name.clone()))
        .collect();
    let index = commands::resolve_peer(ctx, &candidates, &Some(args.peer.clone()))?;
    let device = devices[index].device.clone();

    let quic = Arc::new(peerbeam_transfer_quic::QuicTransport::new().map_err(CliError::from)?);
    let routes = peerbeam_engine::RouteManager::new(quic.clone());
    let session = session_transfer::dial(
        &quic, &routes, &device, "pair", &sc.ident, &sc.enc, &sc.trust, None,
    )
    .await
    .map_err(|e| CliError::Other(format!("could not reach {}: {e}", device.name)))?;

    // A resumed session has no handshake, so there is no transcript to bind a
    // proof to. Refusing is the only honest answer: a PIN checked against
    // nothing would report success while proving nothing at all.
    if session.transcript.is_empty() {
        session.close().await;
        return Err(CliError::Other(
            "this connection was resumed rather than freshly negotiated, so \
             there is no handshake to bind a PIN to — reconnect and try again"
                .into(),
        ));
    }

    let result = if args.show {
        show_and_verify(ctx, &session, &device.name).await
    } else {
        enter_and_prove(ctx, &session, &device.name, args.pin.as_deref()).await
    };
    session.close().await;
    result
}

/// This device shows the PIN; the peer proves it.
async fn show_and_verify(ctx: &Ctx, session: &session_transfer::Session, peer: &str) -> CliResult {
    let mut pairing = Pairing::begin();
    ctx.line(&format!(
        "PIN for {peer}: {}",
        ctx.bold(&pairing.pin().display())
    ));
    ctx.line(&ctx.dim(
        "read these digits to the other device — they are never sent over the \
         connection",
    ));

    loop {
        let Some(proof) = session_transfer::await_pairing_proof(session).await else {
            return Err(CliError::Other(format!("{peer} did not attempt the PIN")));
        };
        match pairing.attempt(&session.transcript, &proof) {
            Attempt::Verified => {
                session_transfer::send_pairing_result(session, true, 0).await;
                // Approve only now. This is the single act the whole flow
                // exists to gate.
                let device = peerbeam_domain::id::DeviceId::from(session.peer_id.clone());
                crate::commands::open_trust(&crate::commands::load_config(None)?)?
                    .approve_gated(&device, true, None)?;
                ctx.line(&format!("{peer} {}", ctx.bold("paired and approved")));
                return Ok(());
            }
            Attempt::Wrong { attempts_left } => {
                session_transfer::send_pairing_result(session, false, attempts_left).await;
                ctx.line(&format!("wrong PIN — {attempts_left} left"));
            }
            Attempt::Exhausted => {
                session_transfer::send_pairing_result(session, false, 0).await;
                return Err(CliError::Other(
                    "too many wrong PINs — start again with a fresh one".into(),
                ));
            }
        }
    }
}

/// This device types the PIN the peer is showing.
async fn enter_and_prove(
    ctx: &Ctx,
    session: &session_transfer::Session,
    peer: &str,
    supplied: Option<&str>,
) -> CliResult {
    let raw = match supplied {
        Some(p) => p.to_string(),
        None if ctx.interactive => {
            ctx.line(&format!("PIN shown on {peer}:"));
            let mut buf = String::new();
            std::io::stdin()
                .read_line(&mut buf)
                .map_err(|e| CliError::Other(format!("reading the PIN: {e}")))?;
            buf
        }
        None => {
            return Err(CliError::Usage(
                "no PIN given and nothing to prompt on — pass --pin".into(),
            ))
        }
    };
    let pin = Pin::parse(&raw).ok_or_else(|| {
        // Rejected, never repaired: quietly "fixing" a mistyped PIN is how a
        // person confirms a pairing they did not actually check.
        CliError::Usage("a PIN is six digits".into())
    })?;

    let proof = peerbeam_pairing::prove(&pin, &session.transcript);
    let Some((verified, attempts_left)) =
        session_transfer::send_pairing_proof(session, &proof).await
    else {
        return Err(CliError::Other(format!("{peer} did not answer the PIN")));
    };
    if verified {
        ctx.line(&format!("{peer} {}", ctx.bold("paired")));
        Ok(())
    } else if attempts_left == 0 {
        Err(CliError::Other(
            "wrong PIN, and no attempts remain — ask for a fresh one".into(),
        ))
    } else {
        Err(CliError::Other(format!(
            "wrong PIN — {attempts_left} attempts remain"
        )))
    }
}
