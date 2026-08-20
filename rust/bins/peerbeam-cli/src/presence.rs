//! The CLI's presence context: one live registry, and the inputs the sharing
//! gate needs.
//!
//! Process-global rather than threaded through every dial and accept, and
//! deliberately so. `session_transfer`'s five call sites all have to wire
//! presence or a peer's status is dropped silently — the exact failure mode
//! `ChatWiring`'s doc comment describes, and one that has already shipped twice
//! in this codebase. A global that `establish` reads unconditionally makes
//! forgetting a call site impossible, which is worth more here than the
//! symmetry with the FFI's explicit parameter.
//!
//! What is **not** global is the decision to send. That stays in
//! `peerbeam_presence::may_share_status`, re-read per heartbeat.

use std::sync::{Mutex, OnceLock};

use peerbeam_config::EngineConfig;
use peerbeam_presence::PresenceRegistry;

/// Every peer's last shared status for this CLI invocation.
///
/// Always available, because *receiving* is unconditional: a `peerbeam receive`
/// holding sessions open records what its peers share whether or not this
/// device shares anything back. Never persisted (I4) — a CLI process that exits
/// takes its presence view with it, which is correct: presence is live state.
pub fn registry() -> &'static PresenceRegistry {
    static REGISTRY: OnceLock<PresenceRegistry> = OnceLock::new();
    REGISTRY.get_or_init(PresenceRegistry::new)
}

/// What a heartbeat needs to collect and gate a status.
#[derive(Clone)]
pub struct Sharing {
    /// The opt-in setting's value for this invocation.
    pub enabled: bool,
    /// Receive directory — the volume whose free space we report.
    pub save_dir: String,
}

static SHARING: Mutex<Option<Sharing>> = Mutex::new(None);

/// Configure sharing from the loaded config. Called by every command that
/// builds an engine, before it dials or accepts.
///
/// Until this is called nothing is shared — an unconfigured process has no
/// opt-in value to read, and absence of consent is not consent.
pub fn configure(config: &EngineConfig) {
    let mut slot = SHARING.lock().unwrap_or_else(|e| e.into_inner());
    *slot = Some(Sharing {
        enabled: config.device.share_presence,
        save_dir: config.storage.save_directory.clone(),
    });
}

/// The current sharing configuration, or `None` when unconfigured.
pub fn sharing() -> Option<Sharing> {
    SHARING.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

/// Whether the opt-in setting is on right now. **False when unconfigured** —
/// see [`configure`].
pub fn enabled() -> bool {
    sharing().is_some_and(|s| s.enabled)
}

/// `peerbeam ring <PEER> [--seconds N]`.
///
/// Opens a session and asks the device to make itself findable. Success here
/// means the request went out, not that anything made a sound: whether a device
/// rings is its own decision, taken against its `presence` permission for this
/// machine, and it deliberately never answers. A caller told "refused" could
/// enumerate which devices are listening and which permission each holds.
pub async fn ring(
    ctx: &crate::output::Ctx,
    args: crate::cli::RingArgs,
    path_override: Option<&str>,
) -> crate::exit::CliResult {
    use crate::exit::CliError;

    let config = crate::commands::load_config(path_override)?;
    let sc = crate::commands::SecureCtx::build(&config)?;
    let devices = crate::commands::snapshot(config.clone(), 2).await?;
    let candidates: Vec<(String, String)> = devices
        .iter()
        .map(|m| (m.device.id.to_string(), m.device.name.clone()))
        .collect();
    let index = crate::commands::resolve_peer(ctx, &candidates, &Some(args.peer))?;
    let device = devices[index].device.clone();

    let quic =
        std::sync::Arc::new(peerbeam_transfer_quic::QuicTransport::new().map_err(CliError::from)?);
    let routes = peerbeam_engine::RouteManager::new(quic.clone());
    let session = crate::session_transfer::dial(
        &quic, &routes, &device, "ring", &sc.ident, &sc.enc, &sc.trust, None,
    )
    .await
    .map_err(|e| CliError::Other(format!("could not reach {}: {e}", device.name)))?;

    if !session.supports_ring() {
        session.close().await;
        return Err(CliError::Other(format!(
            "{} is running a build that cannot ring",
            device.name
        )));
    }

    let mut sender = peerbeam_presence::PresenceSender::new(
        session.handle.clone(),
        peerbeam_domain::id::DeviceId::from(session.peer_id.clone()),
        session.capabilities().clone(),
        sc.trust.clone(),
        std::sync::Arc::new(|| false),
        std::sync::Arc::new(peerbeam_presence::Status::default),
    );
    let sent = sender.ring(args.seconds).await;
    session.close().await;
    sent.map_err(|e| CliError::Other(format!("ringing {}: {e}", device.name)))?;

    if ctx.json {
        ctx.json_line(&serde_json::json!({
            "peer": session_peer(&device), "seconds": args.seconds, "sent": true,
        }));
    } else {
        ctx.line(&format!(
            "asked {} to ring for {}s",
            ctx.bold(&device.name),
            args.seconds
        ));
    }
    Ok(())
}

/// The device id to report, so `--json` names the device rather than the label
/// the user happened to type.
fn session_peer(device: &peerbeam_domain::entity::Device) -> String {
    device.id.0.clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An unconfigured process shares nothing. This is the CLI's version of
    /// "the default is off": a command that never loaded a config has no
    /// consent to act on.
    #[test]
    fn sharing_is_off_until_configured() {
        // `configure` is global, so this asserts the *initial* state only when
        // it runs first; the meaningful half is that `enabled()` is a pure
        // function of the stored value and defaults to false.
        let stored = sharing();
        match stored {
            None => assert!(!enabled(), "unconfigured must never read as consent"),
            Some(s) => assert_eq!(enabled(), s.enabled),
        }
    }

    #[test]
    fn configure_takes_the_opt_in_from_config_and_defaults_off() {
        let mut cfg = EngineConfig::default();
        assert!(
            !cfg.device.share_presence,
            "the config default must be off (I11)"
        );
        configure(&cfg);
        assert!(!enabled());

        cfg.device.share_presence = true;
        configure(&cfg);
        assert!(enabled());
        assert_eq!(
            sharing().unwrap().save_dir,
            cfg.storage.save_directory,
            "the reported volume is the receive directory's"
        );

        // Leave the global off so no other test in this binary inherits an
        // opt-in it did not ask for.
        cfg.device.share_presence = false;
        configure(&cfg);
    }

    /// The registry is one shared handle for the whole process, so a session's
    /// handler and `peerbeam status` see the same map.
    #[test]
    fn the_registry_is_one_shared_instance() {
        let a = registry();
        let b = registry();
        let before = a.len();
        b.record(
            &peerbeam_domain::id::DeviceId::from("pb-presence-registry-test"),
            peerbeam_presence::Status {
                sent_at: "t".into(),
                ..Default::default()
            },
            chrono::Utc::now(),
        );
        assert_eq!(a.len(), before + 1);
        a.forget(&peerbeam_domain::id::DeviceId::from(
            "pb-presence-registry-test",
        ));
    }
}
