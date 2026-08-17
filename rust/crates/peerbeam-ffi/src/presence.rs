//! The Presence capability's FFI surface: what this device collects, what its
//! peers have shared, and the one setting that decides whether anything leaves.
//!
//! The privacy decision itself lives in `peerbeam_presence::may_share_status`
//! and is enforced by `PresenceSender::beat`. Nothing in this module may
//! re-implement it or work around it; what lives here is the *inputs* — the
//! opt-in setting's current value, and a status collected from this host.

use std::sync::{Arc, Mutex};

use serde_json::{json, Value};

use peerbeam_domain::entity::RouteKind;
use peerbeam_domain::id::DeviceId;
use peerbeam_presence::{PeerStatus, PresenceRegistry, Status};

use crate::error::Code;

/// The settings key for the single opt-in toggle.
///
/// *"Share device status with trusted devices"*, **default off**. One key, one
/// meaning: with it false this device sends no status at all, to anyone. It is
/// deliberately not a per-peer list — the trusted-only rule already scopes who
/// can receive, and a second, finer control would only make it harder to answer
/// "who can see my battery level?" at a glance.
pub const SHARE_KEY: &str = "share_presence";

/// A battery reading pushed down from a platform layer above Rust.
///
/// Android is the only caller: the Flutter side reads `BatteryManager` over the
/// existing `peerbeam/android` method channel and pushes it in with
/// `pb_presence_battery`, because the Rust platform layer has no route to that
/// API. Every other platform leaves this empty and
/// `peerbeam_presence::battery()` answers (Linux) or declines to (Windows,
/// macOS).
static BATTERY_OVERRIDE: Mutex<Option<(u8, Option<bool>)>> = Mutex::new(None);

/// Record a platform-supplied battery reading. An absent `percent` clears it,
/// so a surface that loses its own battery access stops asserting a stale one.
pub fn set_battery(percent: Option<u8>, charging: Option<bool>) {
    let mut slot = BATTERY_OVERRIDE.lock().unwrap_or_else(|e| e.into_inner());
    *slot = percent.map(|p| (p, charging));
}

/// The current platform-supplied battery reading, if any.
fn battery_override() -> Option<(u8, Option<bool>)> {
    *BATTERY_OVERRIDE.lock().unwrap_or_else(|e| e.into_inner())
}

/// Collect this device's status for a session running over `route`.
///
/// The `route` class comes from the engine's own `RouteManager` — the one
/// classifier — so nothing here re-derives it.
#[must_use]
pub fn collect_status(save_dir: &str, route: Option<RouteKind>) -> Status {
    let status = peerbeam_presence::collect(save_dir, route, env!("CARGO_PKG_VERSION"));
    match battery_override() {
        Some((p, charging)) => status.with_battery_override(Some(p), charging),
        None => status,
    }
}

/// Whether the opt-in setting is currently on. **Defaults to off** when the key
/// is absent, unreadable, or not a boolean — a settings file this build cannot
/// parse must not be read as consent.
#[must_use]
pub fn sharing_enabled() -> bool {
    crate::settings::get()
        .ok()
        .and_then(|s| s.get(SHARE_KEY).and_then(Value::as_bool))
        .unwrap_or(false)
}

/// Emit a `presence_updated` event so a dashboard re-renders live rather than
/// polling.
pub fn emit_updated(peer: &DeviceId, entry: &PeerStatus) {
    crate::events::emit(&json!({
        "type": "presence_updated",
        "timestamp": chrono::Utc::now().to_rfc3339(),
        "payload": { "device": peer_json(peer, entry) },
    }));
}

/// One peer's status as the surfaces see it.
///
/// Absent fields are **omitted from the object**, not sent as `null` or `0`: a
/// desktop with no battery must render as having no battery, and a UI that
/// received `battery_percent: 0` would draw an empty gauge instead.
fn peer_json(peer: &DeviceId, entry: &PeerStatus) -> Value {
    let s = &entry.status;
    let mut o = serde_json::Map::new();
    o.insert("device_id".into(), json!(peer.0));
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
    // The sender's own clock, kept for transparency but paired with the two
    // numbers a surface should actually use: our receipt time and the age
    // derived from it. Peer clocks are not synchronised.
    o.insert("sent_at".into(), json!(s.sent_at));
    o.insert("received_at".into(), json!(entry.received_at.to_rfc3339()));
    o.insert(
        "age_seconds".into(),
        json!(entry.age_seconds(chrono::Utc::now())),
    );
    Value::Object(o)
}

/// The full presence snapshot: whether we are sharing, what we would share, and
/// what every peer has shared with us.
///
/// `self_status` is what this device *would* send — shown so a user can see
/// exactly what the toggle would reveal before turning it on. It is computed
/// locally and is never on the wire while `sharing` is false.
pub fn snapshot(registry: &PresenceRegistry, save_dir: &str) -> Result<Value, (Code, String)> {
    let devices: Vec<Value> = registry
        .snapshot()
        .iter()
        .map(|(id, entry)| peer_json(id, entry))
        .collect();
    let own = collect_status(save_dir, None);
    Ok(json!({
        "sharing": sharing_enabled(),
        "self": {
            "battery_percent": own.battery_percent,
            "charging": own.charging,
            "storage_free_bytes": own.storage_free_bytes,
            "app_version": own.app_version,
        },
        "devices": devices,
    }))
}

/// Everything a session needs to take part in presence, in the shape
/// `session_exec` consumes.
///
/// Mirrors `ChatWiring`, and for the same reason: **every** dial and accept
/// call site must pass it. A session with no `PresenceHandler` registered does
/// not error on an inbound Status frame — the channel dispatch loop silently
/// drops it — so a missed call site means a peer's status vanishes with no
/// error on either side.
#[derive(Clone)]
pub struct PresenceWiring {
    /// The one live registry every surface reads.
    pub registry: PresenceRegistry,
    /// Where received files go — the volume whose free space we report.
    pub save_dir: String,
}

impl PresenceWiring {
    /// The status source for a session running over `route`.
    #[must_use]
    pub fn source(&self, route: Option<RouteKind>) -> Arc<dyn Fn() -> Status + Send + Sync> {
        let dir = self.save_dir.clone();
        // Collected per heartbeat, not once: a draining battery and a filling
        // disk are the whole point of a cadence.
        Arc::new(move || collect_status(&dir, route))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn entry(status: Status) -> PeerStatus {
        PeerStatus {
            status,
            received_at: Utc::now(),
        }
    }

    fn full() -> Status {
        Status {
            battery_percent: Some(55),
            charging: Some(true),
            storage_free_bytes: Some(1_000),
            network: Some("lan".into()),
            app_version: Some("0.4.1".into()),
            sent_at: "2026-08-17T10:00:00Z".into(),
        }
    }

    /// A device that shares nothing must produce an object with its identity
    /// and no status keys at all — **not** keys set to null or zero. This is
    /// what lets a surface render "status not shared" instead of an empty
    /// gauge, and it is the difference the whole optional-field design exists
    /// to preserve.
    #[test]
    fn a_peer_sharing_nothing_omits_the_keys_rather_than_nulling_them() {
        let bare = Status {
            sent_at: "2026-08-17T10:00:00Z".into(),
            ..Status::default()
        };
        let v = peer_json(&DeviceId::from("pb-bob"), &entry(bare));
        let o = v.as_object().unwrap();
        assert_eq!(o.get("device_id").unwrap(), "pb-bob");
        for absent in [
            "battery_percent",
            "charging",
            "storage_free_bytes",
            "network",
            "app_version",
        ] {
            assert!(
                !o.contains_key(absent),
                "{absent} must be absent, not null or 0"
            );
        }
        // Identity and timing are always present — that is the "identity +
        // reachability, not empty gauges" half.
        assert!(o.contains_key("received_at"));
        assert!(o.contains_key("age_seconds"));
    }

    #[test]
    fn a_fully_shared_peer_carries_every_field() {
        let v = peer_json(&DeviceId::from("pb-bob"), &entry(full()));
        let o = v.as_object().unwrap();
        assert_eq!(o.get("battery_percent").unwrap(), 55);
        assert_eq!(o.get("charging").unwrap(), true);
        assert_eq!(o.get("storage_free_bytes").unwrap(), 1000);
        assert_eq!(o.get("network").unwrap(), "lan");
        assert_eq!(o.get("app_version").unwrap(), "0.4.1");
    }

    /// Partial sharing is the common case (a desktop: no battery, everything
    /// else) and each field must be independently present or absent.
    #[test]
    fn a_partially_shared_peer_carries_only_what_it_sent() {
        let partial = Status {
            storage_free_bytes: Some(42),
            sent_at: "t".into(),
            ..Status::default()
        };
        let v = peer_json(&DeviceId::from("pb-desk"), &entry(partial));
        let o = v.as_object().unwrap();
        assert_eq!(o.get("storage_free_bytes").unwrap(), 42);
        assert!(
            !o.contains_key("battery_percent"),
            "a desktop has no battery"
        );
        assert!(!o.contains_key("network"));
    }

    /// The whole platform-supplied-battery lifecycle, in one test **on
    /// purpose**: the override is a process-global (there is one device, and
    /// one battery on it), so splitting these into separate `#[test]` fns lets
    /// cargo's thread-parallel runner interleave their writes and fail for
    /// reasons that have nothing to do with the code under test. One
    /// sequential test is the honest shape for shared state.
    #[test]
    fn the_pushed_battery_reading_is_used_ignored_when_absurd_and_clearable() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_string_lossy().into_owned();

        // Used: Android's reading replaces whatever the native collector says.
        set_battery(Some(37), Some(true));
        let s = collect_status(&path, Some(RouteKind::Lan));
        assert_eq!(s.battery_percent, Some(37));
        assert_eq!(s.charging, Some(true));
        assert_eq!(s.network.as_deref(), Some("lan"), "route still classified");

        // Absurd: ignored rather than clamped, and it must never leave a status
        // this build's own encoder would refuse.
        set_battery(Some(250), Some(true));
        let s = collect_status(&path, Some(RouteKind::Lan));
        assert_ne!(s.battery_percent, Some(250));
        assert_ne!(s.battery_percent, Some(100), "ignored, not clamped");
        assert!(
            s.to_frame(peerbeam_domain::session::ChannelId::new(1))
                .is_ok(),
            "a poisoned reading must never reach the wire encoder"
        );

        // Cleared: a surface that loses battery access stops asserting a stale
        // level. Back to whatever this host natively reports — nothing on a
        // desktop, and in any case no longer 37.
        set_battery(None, None);
        assert_ne!(collect_status(&path, None).battery_percent, Some(37));
    }

    /// The snapshot's `self` block is a *preview*, not a broadcast: it lets a
    /// user see what the toggle would reveal before turning it on. It must
    /// therefore be computable while sharing is off.
    #[test]
    fn the_snapshot_previews_our_own_status_and_reports_the_sharing_flag() {
        let dir = tempfile::tempdir().unwrap();
        let reg = PresenceRegistry::new();
        reg.record(&DeviceId::from("pb-bob"), full(), Utc::now());
        let v = snapshot(&reg, &dir.path().to_string_lossy()).unwrap();
        assert!(v.get("sharing").unwrap().is_boolean());
        assert!(v["self"]["storage_free_bytes"].is_number());
        assert_eq!(v["devices"].as_array().unwrap().len(), 1);
        assert_eq!(v["devices"][0]["device_id"], "pb-bob");
    }
}
