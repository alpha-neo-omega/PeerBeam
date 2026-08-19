//! Building this device's [`Status`] from what it can honestly measure.
//!
//! The governing rule is the schema's: **a device that cannot determine a value
//! omits it**. Every collector returns `Option`, `None` means "could not
//! measure", and no collector ever substitutes a plausible-looking zero. A
//! desktop with no battery is the normal case, not a degraded one.
//!
//! Nothing here reaches for a new dependency. Where a value would need one, the
//! field is simply absent — see [`battery`].

use chrono::Utc;

use peerbeam_domain::entity::RouteKind;

use crate::message::Status;

/// This device's battery charge and whether it is charging.
///
/// Platform coverage is deliberately partial:
///
/// * **Linux** — read from `/sys/class/power_supply`, which is already there
///   and costs one small file read.
/// * **Android** — the platform layer has no access to `BatteryManager`; the
///   Flutter side reads it over the existing `peerbeam/android` method channel
///   and pushes it in with [`Status::with_battery_override`]. This function
///   returns `None` there and that is correct, not a gap.
/// * **Windows / macOS** — omitted. Both would need a new dependency or a
///   hand-rolled FFI binding to a platform API, for a number that is a nicety.
///   They report `None`, which is exactly the case the schema was built to
///   express.
#[must_use]
pub fn battery() -> (Option<u8>, Option<bool>) {
    match peerbeam_platform::battery() {
        Some(b) => (Some(b.percent), b.charging),
        None => (None, None),
    }
}

/// Free bytes on the volume holding `receive_dir`.
///
/// The receive directory is the right volume to report because it is the one
/// that decides whether a transfer to this device will fit — which is the only
/// question a peer looking at a dashboard is actually asking.
#[must_use]
pub fn storage_free(receive_dir: &str) -> Option<u64> {
    peerbeam_platform::available_bytes(receive_dir)
}

/// How this device reaches the network, from the route the engine already
/// classified.
///
/// This deliberately takes the engine's [`RouteKind`] rather than re-deriving
/// anything: `peerbeam_engine::route_classifier` is the one classifier, and a
/// second one here would drift from it the first time either changed.
///
/// Three of the seven kinds have no honest word in the wire vocabulary
/// ([`crate::message::NETWORK_KINDS`]) and report `"unknown"` rather than being
/// squeezed into a neighbouring one. `DirectInternet` in particular is *not*
/// `"lan"` and not `"wifi"`: it says the peer is globally routable, which says
/// nothing about the local link this device is using.
#[must_use]
pub fn network(route: Option<RouteKind>) -> Option<String> {
    let word = match route? {
        RouteKind::Lan => "lan",
        RouteKind::Ethernet => "ethernet",
        RouteKind::Wifi => "wifi",
        RouteKind::TailscaleDirect => "tailscale",
        // No honest word for these. Reporting "unknown" is a real answer —
        // "I reached you, and I cannot characterise how" — and it keeps the
        // vocabulary closed against a peer having to guess at a new one.
        RouteKind::UsbTether | RouteKind::DirectInternet | RouteKind::Relay => "unknown",
    };
    Some(word.to_string())
}

/// Collect everything this device can measure, right now.
///
/// `route` is the class of the route this session is running over, as the
/// engine classified it; `app_version` is this build's own version, so a
/// dashboard can show version skew.
#[must_use]
pub fn collect(receive_dir: &str, route: Option<RouteKind>, app_version: &str) -> Status {
    let (battery_percent, charging) = battery();
    Status {
        battery_percent,
        charging,
        storage_free_bytes: storage_free(receive_dir),
        network: network(route),
        app_version: Some(app_version.to_string()),
        sent_at: Utc::now().to_rfc3339(),
    }
}

impl Status {
    /// Replace the battery reading with one supplied by a platform layer above
    /// this crate (Android's `BatteryManager`, read over the Flutter method
    /// channel).
    ///
    /// Only the *battery* fields move: a caller that can read a battery has no
    /// better view of storage or routing than the collectors above do.
    ///
    /// An out-of-range percentage is **ignored rather than clamped**, for the
    /// same reason [`Status::from_frame`] discards one from the wire: a
    /// collector that can report 137% has not measured anything, and an
    /// invented number presented as a measurement is worse than an absent one.
    #[must_use]
    pub fn with_battery_override(mut self, percent: Option<u8>, charging: Option<bool>) -> Status {
        if let Some(p) = percent {
            if p <= crate::message::MAX_BATTERY_PERCENT {
                self.battery_percent = Some(p);
                self.charging = charging;
            }
        }
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::is_known_network;

    /// Storage free is reported for the receive directory's volume — and it is
    /// a real measurement, not a constant: the temp dir this test creates is on
    /// a mounted filesystem with a nonzero amount of room.
    #[test]
    fn storage_free_is_reported_for_the_receive_directory() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_string_lossy().into_owned();
        let free = storage_free(&path).expect("a real temp dir has measurable free space");
        assert!(free > 0, "free space on a writable volume must be nonzero");

        // And it is the *volume's* figure, from the platform layer rather than
        // invented here. Compared with a tolerance, not for equality: these are
        // two separate live measurements, and on a busy machine the free space
        // genuinely moves between them. Asserting they match to the byte tests
        // whether anything else wrote to the disk, which is not the property
        // this test is about.
        let again = peerbeam_platform::available_bytes(&path).expect("a second reading");
        let drift = free.abs_diff(again);
        assert!(
            drift < free / 10 + 64 * 1024 * 1024,
            "storage_free {free} and available_bytes {again} disagree by {drift}, \
             which is more than ordinary disk activity explains"
        );
    }

    /// A path that does not exist walks up to its nearest existing ancestor
    /// rather than failing — the receive directory may not be created yet.
    #[test]
    fn storage_free_survives_a_receive_directory_that_does_not_exist_yet() {
        let dir = tempfile::tempdir().unwrap();
        let nested = dir.path().join("not").join("created").join("yet");
        assert!(storage_free(&nested.to_string_lossy()).is_some());
    }

    /// Every route kind maps to a word the wire vocabulary actually defines —
    /// otherwise `to_frame` would refuse our own status.
    #[test]
    fn every_route_kind_maps_into_the_wire_vocabulary() {
        for kind in [
            RouteKind::Lan,
            RouteKind::UsbTether,
            RouteKind::Ethernet,
            RouteKind::Wifi,
            RouteKind::TailscaleDirect,
            RouteKind::DirectInternet,
            RouteKind::Relay,
        ] {
            let word = network(Some(kind)).expect("a known route always has a word");
            assert!(
                is_known_network(&word),
                "{kind:?} produced {word:?}, which the wire would blank"
            );
        }
    }

    #[test]
    fn the_engines_classes_map_to_their_own_words() {
        assert_eq!(network(Some(RouteKind::Lan)).as_deref(), Some("lan"));
        assert_eq!(
            network(Some(RouteKind::TailscaleDirect)).as_deref(),
            Some("tailscale")
        );
        assert_eq!(network(Some(RouteKind::Wifi)).as_deref(), Some("wifi"));
        assert_eq!(
            network(Some(RouteKind::Ethernet)).as_deref(),
            Some("ethernet")
        );
    }

    /// A globally-routable peer says nothing about our own local link, so it
    /// must not be reported as LAN or Wi-Fi.
    #[test]
    fn direct_internet_is_unknown_rather_than_guessed() {
        assert_eq!(
            network(Some(RouteKind::DirectInternet)).as_deref(),
            Some("unknown")
        );
    }

    /// No route at all (no session route information) omits the field entirely
    /// — absent, not "unknown", because we were not even asked to characterise
    /// anything.
    #[test]
    fn no_route_omits_the_field() {
        assert_eq!(network(None), None);
    }

    /// A collected status must always be encodable — that is the contract
    /// `to_frame`'s symmetric validation enforces, and this is what catches a
    /// collector that starts producing something the wire refuses.
    #[test]
    fn a_collected_status_always_encodes() {
        let dir = tempfile::tempdir().unwrap();
        let s = collect(&dir.path().to_string_lossy(), Some(RouteKind::Lan), "0.4.1");
        assert!(s
            .to_frame(peerbeam_domain::session::ChannelId::new(1))
            .is_ok());
        assert_eq!(s.app_version.as_deref(), Some("0.4.1"));
        assert!(
            !s.sent_at.is_empty(),
            "the timestamp is the one required field"
        );
        assert!(s.storage_free_bytes.is_some());
    }

    /// Whatever this host's battery situation is, it must be *expressible*: a
    /// present reading is in range, and an absent one is genuinely absent
    /// rather than a zero standing in for one.
    #[test]
    fn the_battery_collector_is_either_absent_or_in_range() {
        let (percent, _charging) = battery();
        if let Some(p) = percent {
            assert!(p <= 100, "a collected battery reading must be in range");
        }
    }

    #[test]
    fn a_platform_supplied_battery_replaces_only_the_battery_fields() {
        let base = Status {
            storage_free_bytes: Some(42),
            network: Some("lan".into()),
            sent_at: "t".into(),
            ..Status::default()
        };
        let overridden = base.clone().with_battery_override(Some(88), Some(true));
        assert_eq!(overridden.battery_percent, Some(88));
        assert_eq!(overridden.charging, Some(true));
        assert_eq!(overridden.storage_free_bytes, Some(42), "untouched");
        assert_eq!(overridden.network.as_deref(), Some("lan"), "untouched");
    }

    /// An out-of-range override is ignored, not clamped — and critically, it
    /// does not corrupt a status that would otherwise encode fine.
    #[test]
    fn an_out_of_range_battery_override_is_ignored_not_clamped() {
        let base = Status {
            battery_percent: Some(50),
            sent_at: "t".into(),
            ..Status::default()
        };
        let bad = base.with_battery_override(Some(200), Some(true));
        assert_eq!(bad.battery_percent, Some(50), "the old reading is kept");
        assert_ne!(bad.battery_percent, Some(100), "not clamped to the max");
        assert!(bad
            .to_frame(peerbeam_domain::session::ChannelId::new(1))
            .is_ok());
    }

    /// `None` means "this platform has no reading to offer", which must leave
    /// whatever the native collector found alone rather than blanking it.
    #[test]
    fn an_absent_override_leaves_the_native_reading_alone() {
        let base = Status {
            battery_percent: Some(33),
            charging: Some(false),
            sent_at: "t".into(),
            ..Status::default()
        };
        let same = base.clone().with_battery_override(None, None);
        assert_eq!(same.battery_percent, Some(33));
        assert_eq!(same.charging, Some(false));
    }
}
