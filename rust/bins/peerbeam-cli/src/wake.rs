//! `peerbeam wake …` — start one of your own machines over the local network.
//!
//! # What this can and cannot do
//!
//! Wake-on-LAN is a broadcast on the local segment. It does **not** travel over
//! Tailscale, a VPN, or the internet, and no amount of plumbing here would
//! change that — those are point-to-point paths and a broadcast has no
//! destination to be routed to. The commands say so rather than letting someone
//! discover it by trying to wake a laptop from another country.
//!
//! There is no acknowledgement in the protocol either. A successful exit means
//! the packet left this machine; whether anything woke is answered by the
//! device turning up in `peerbeam list`, not by anything here.

use chrono::Utc;

use peerbeam_domain::id::DeviceId;
use peerbeam_wake::{MacAddress, UdpBroadcast, WakeError, WakeStore};

use crate::cli::WakeAction;
use crate::commands::{load_config, open_trust, wake_store};
use crate::exit::{CliError, CliResult};
use crate::output::Ctx;

pub fn wake(ctx: &Ctx, action: WakeAction, path_override: Option<&str>) -> CliResult {
    let config = load_config(path_override)?;
    let store = wake_store(&config)?;
    match action {
        WakeAction::Set { device, mac } => set(ctx, &store, &device, &mac),
        WakeAction::Forget { device } => forget(ctx, &store, &device),
        WakeAction::Send { device, broadcast } => send(ctx, &store, &config, &device, &broadcast),
    }
}

fn err(e: WakeError) -> CliError {
    match e {
        WakeError::Storage(_) => CliError::Other(e.to_string()),
        WakeError::Send(_) => CliError::Connection(e.to_string()),
        _ => CliError::Usage(e.to_string()),
    }
}

fn set(ctx: &Ctx, store: &WakeStore, device: &str, mac: &str) -> CliResult {
    let parsed: MacAddress = mac
        .parse()
        .map_err(|e: peerbeam_wake::MacError| CliError::Usage(e.to_string()))?;
    let id = DeviceId::from(device.to_string());
    store.remember(&id, parsed, Utc::now()).map_err(err)?;

    if ctx.json {
        ctx.json_line(&serde_json::json!({
            "event": "wake_address_set",
            "device": device,
            "mac": parsed.to_string(),
        }));
    } else {
        ctx.line(&format!("{device} can be woken at {parsed}"));
    }
    Ok(())
}

fn forget(ctx: &Ctx, store: &WakeStore, device: &str) -> CliResult {
    let id = DeviceId::from(device.to_string());
    let had = store.forget(&id).map_err(err)?;
    if ctx.json {
        ctx.json_line(
            &serde_json::json!({ "event": "wake_address_forgotten", "device": device, "forgotten": had }),
        );
    } else if had {
        ctx.line(&format!("{device} can no longer be woken from here"));
    } else {
        ctx.line(&format!("no address recorded for {device}"));
    }
    Ok(())
}

fn send(
    ctx: &Ctx,
    store: &WakeStore,
    config: &peerbeam_config::EngineConfig,
    device: &str,
    broadcast: &str,
) -> CliResult {
    let addr: std::net::Ipv4Addr = broadcast
        .parse()
        .map_err(|_| CliError::Usage(format!("{broadcast} is not an IPv4 broadcast address")))?;
    let socket = UdpBroadcast::bind()
        .map_err(|e| CliError::Connection(format!("could not open a broadcast socket: {e}")))?;
    let trust = open_trust(config)?;
    let id = DeviceId::from(device.to_string());

    let attempt = peerbeam_wake::wake_device(store, &trust, &socket, &id, addr).map_err(err)?;

    if ctx.json {
        ctx.json_line(&serde_json::json!({
            "event": "wake_sent",
            "device": device,
            "mac": attempt.mac.to_string(),
            "sent_to": attempt.sent_to.iter().map(ToString::to_string).collect::<Vec<_>>(),
        }));
        return Ok(());
    }
    // Deliberately phrased as what happened, not as what it achieved. The
    // protocol carries no reply, and a line reading "woken" would be a claim
    // this command cannot support.
    ctx.line(&format!(
        "wake packet sent to {} for {device}\n{}",
        attempt
            .sent_to
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(", "),
        ctx.dim("nothing replies to a wake — watch `peerbeam list` for it to appear")
    ));
    Ok(())
}
