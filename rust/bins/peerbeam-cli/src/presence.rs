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
