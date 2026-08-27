//! Wake-on-LAN: powering up a sleeping device on the local network so that
//! "my devices are reachable" is true rather than aspirational.
//!
//! The single most common reason a device PeerBeam knows about is unreachable
//! is that it is asleep. This crate builds the 102-byte magic packet that wakes
//! it, addresses it at the local broadcast, and refuses to do so for a device
//! the user has not approved.
//!
//! # What this can and cannot reach
//!
//! **Wake-on-LAN works on the local broadcast domain, and nowhere else.** The
//! packet is matched by the target's network card while the host is powered
//! down, and it gets there because a UDP broadcast is flooded to every port of
//! the local switch. That has three consequences worth stating plainly, because
//! PeerBeam's whole pitch is reaching your devices across LAN, Ethernet, Wi-Fi,
//! USB tethering and Tailscale, and this feature does **not** inherit that
//! reach:
//!
//! * **It does not traverse Tailscale, WireGuard, or any other VPN.** Those
//!   carry IP to a host that is running, which is exactly what a sleeping
//!   machine is not: there is no tailnet node answering, no route to it, and
//!   nothing to encapsulate a broadcast into. A device PeerBeam can otherwise
//!   only reach over Tailscale cannot be woken from here at all.
//! * **It does not cross a router** unless someone has deliberately configured
//!   directed-broadcast forwarding, which is off by default on essentially all
//!   equipment and for good reason.
//! * **It works from a machine on the same segment**, which in practice means
//!   the desktop, the laptop or the always-on server on the same LAN as the
//!   sleeping one. A headless box on the LAN makes an excellent wake proxy, and
//!   that is a deployment note, not a feature this crate implements.
//!
//! Nothing in this crate implies otherwise anywhere, and nothing should be
//! built on top of it that does.
//!
//! # There is no acknowledgement, so nothing here confirms a wake
//!
//! The protocol has no reply. The recipient is a network card in a machine with
//! its operating system powered off; it has no IP stack to answer with, and the
//! sender learns nothing either way. [`WakeAttempt`] is therefore a receipt for
//! *what left this machine*, and is named to make claiming more than that
//! awkward. The answer to *"did it come up?"* has to come from somewhere else —
//! **discovery noticing the device**, tens of seconds later, when it announces
//! itself over UDP or mDNS. A surface may say "sent"; only discovery earns
//! "awake".
//!
//! # Where this sits in the architecture
//!
//! A magic packet does not go through `PeerSession`, and cannot: there is no
//! session, no peer process, and no key material available on a machine that is
//! off. That is not a bypass of the central abstraction — it is the same place
//! **discovery** occupies. `peerbeam-discovery-udp` also broadcasts on the LAN
//! with no session, for the same reason: it runs *before* there is a peer to
//! have a session with. Waking is the step before that one. Once the device is
//! up, everything that follows — discovery, the authenticated handshake, the
//! encrypted session — is entirely unchanged.
//!
//! It follows that the magic packet is **not encrypted** and cannot be (I5
//! covers peer channels; this is not one). It is broadcast in the clear and
//! contains the target's own hardware address sixteen times, so anyone already
//! on that segment learns that somebody wants to wake that machine. They could
//! also have read that address off any frame it ever sent. The format offers no
//! alternative, and there is nothing here to encrypt it *with*.
//!
//! Nothing leaves the local segment, so I3 (no hub) and I4 (no cloud, no
//! telemetry) are untouched: there is no relay, no server and no outbound
//! request of any kind.
//!
//! # Consent
//!
//! Waking is gated on the user having recorded the device's address **and** on
//! the device being approved right now — [`may_wake`], whose documentation
//! carries the I6 argument in full.
//!
//! # Layout
//!
//! * [`MacAddress`] — the address, and a parser for the forms people paste.
//! * [`magic_packet`] — the pure format function.
//! * [`MagicSocket`] / [`send_magic_packet`] — the send path, behind a trait so
//!   the exact bytes are assertable without a network.
//! * [`WakeStore`] — addresses on disk, encrypted, keyed by device id.
//! * [`may_wake`] — the gate.
//! * [`wake_device`] — the one call a frontend makes, tying the four together.

mod error;
mod gate;
mod mac;
mod packet;
mod send;
mod store;

use std::net::Ipv4Addr;

pub use error::WakeError;
pub use gate::may_wake;
pub use mac::{MacAddress, MacError, MAC_LEN};
pub use packet::{broadcast_targets, magic_packet, MAGIC_PACKET_LEN, WOL_PORTS};
pub use send::{send_magic_packet, MagicSocket, UdpBroadcast, WakeAttempt};
pub use store::{WakeRecord, WakeStore, NS};

use peerbeam_domain::id::DeviceId;
use peerbeam_domain::port::TrustStore;

/// Wake `device`: look up its recorded address, check the gate, and broadcast
/// the magic packet.
///
/// The one entry point a frontend needs — the CLI, the FFI and any future
/// surface all call this, so the order of the checks and the meaning of the
/// answer have exactly one implementation (I7: the capability is reachable
/// headless, and identically from every frontend).
///
/// # The route this takes is not the route a session would
///
/// `broadcast` names a **local** broadcast address, and that is the only place
/// this packet goes. It deliberately ignores whatever route PeerBeam's route
/// manager would have chosen for a transfer to this device — a device the
/// engine last reached over Tailscale is, for waking purposes, unreachable
/// unless it is also on the local segment. See the module documentation.
///
/// # What the answer means
///
/// [`Ok`] is *"the packet went out"*, not *"the device is awake"*. There is no
/// acknowledgement in this protocol to build the second on; that answer comes
/// from discovery noticing the device, later. [`Err`] is always about a packet
/// that never left this machine.
///
/// Repeating a wake is harmless and sometimes useful: the packet is idempotent
/// (a machine already awake ignores it), and a single broadcast can be dropped
/// with nothing to notice the loss. Whether to repeat is the caller's decision,
/// because only the caller knows whether a person is waiting.
pub fn wake_device(
    store: &WakeStore,
    trust: &dyn TrustStore,
    socket: &dyn MagicSocket,
    device: &DeviceId,
    broadcast: Ipv4Addr,
) -> Result<WakeAttempt, WakeError> {
    // Looked up first because the gate needs to know whether an address exists,
    // and because doing it here means the gate stays a pure function of facts
    // rather than a thing that reads storage. An unreadable record errors out
    // of `lookup` rather than arriving here as `None` — see its doc for why
    // that distinction is worth the extra branch.
    let recorded = store.lookup(device)?;

    if !may_wake(recorded.is_some(), trust, device) {
        // Two refusals, told apart, because the remedies are different and a
        // person who is told the wrong one will do the wrong thing: record an
        // address that is already recorded, or hunt for a missing address on a
        // device that simply is not approved.
        return Err(if recorded.is_none() {
            WakeError::NotRecorded {
                device: device.to_string(),
            }
        } else {
            WakeError::NotPermitted {
                device: device.to_string(),
            }
        });
    }

    let record = recorded.expect("the gate refuses a device with no recorded address");
    tracing::info!(
        device = %device,
        mac = %record.mac,
        %broadcast,
        "broadcasting a magic packet (no acknowledgement exists; this is not a confirmed wake)"
    );
    send_magic_packet(socket, record.mac, broadcast)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::io;
    use std::net::SocketAddrV4;
    use std::sync::Arc;

    use chrono::{DateTime, Duration, Utc};
    use peerbeam_appstore_fs::FsAppStore;
    use peerbeam_crypto::{derive_subkey, AeadCrypto};
    use peerbeam_domain::entity::{PermissionSet, TrustRecord};
    use peerbeam_domain::error::Result as DomainResult;
    use peerbeam_domain::port::{AppStore, EncryptionProvider};

    /// Records what was sent, so the end-to-end path is asserted on bytes and
    /// destinations rather than on "it returned Ok".
    struct FakeSocket(RefCell<Vec<(Vec<u8>, SocketAddrV4)>>);

    impl MagicSocket for FakeSocket {
        fn send_to(&self, payload: &[u8], target: SocketAddrV4) -> io::Result<usize> {
            self.0.borrow_mut().push((payload.to_vec(), target));
            Ok(payload.len())
        }
    }

    /// Approved, approved-but-expired, pinned-only, or unknown — the four
    /// states that decide whether a wake is permitted.
    enum FakeTrust {
        Approved,
        Expired,
        PinnedOnly,
        Unknown,
    }

    impl TrustStore for FakeTrust {
        fn record(&self, _record: TrustRecord) -> DomainResult<()> {
            Ok(())
        }
        fn lookup(&self, device: &DeviceId) -> DomainResult<Option<TrustRecord>> {
            let approved = match self {
                FakeTrust::Approved | FakeTrust::Expired => true,
                FakeTrust::PinnedOnly => false,
                FakeTrust::Unknown => return Ok(None),
            };
            Ok(Some(TrustRecord {
                device: device.clone(),
                fingerprint: "ff".into(),
                name: "Desktop".into(),
                trusted_at: Utc::now(),
                approved,
                permissions: PermissionSet::granted_on_approval(),
                expires_at: match self {
                    FakeTrust::Expired => Some(Utc::now() - Duration::hours(1)),
                    _ => None,
                },
                mine: false,
                auto_accept: false,
            }))
        }
    }

    fn new_store() -> (WakeStore, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let enc: Arc<dyn EncryptionProvider> = Arc::new(AeadCrypto::new());
        let key = derive_subkey(&[3u8; 32], b"peerbeam-appstore-v1");
        let app: Arc<dyn AppStore> =
            Arc::new(FsAppStore::open(dir.path().join("appstore"), key, enc));
        (WakeStore::new(app), dir)
    }

    fn desktop() -> DeviceId {
        DeviceId::from("pb-desktop")
    }

    fn recorded_at() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-08-20T09:30:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    /// The whole path, asserted on what went out: the recorded address becomes
    /// the magic packet, broadcast to ports 9 and 7 of the address given.
    #[test]
    fn waking_an_approved_recorded_device_broadcasts_its_magic_packet() {
        let (store, _dir) = new_store();
        let mac: MacAddress = "de:ad:be:ef:00:01".parse().unwrap();
        store.remember(&desktop(), mac, recorded_at()).unwrap();
        let socket = FakeSocket(RefCell::new(Vec::new()));

        let attempt = wake_device(
            &store,
            &FakeTrust::Approved,
            &socket,
            &desktop(),
            Ipv4Addr::BROADCAST,
        )
        .unwrap();

        assert_eq!(attempt.mac, mac);
        assert_eq!(attempt.bytes, MAGIC_PACKET_LEN);

        let sent = socket.0.borrow();
        assert_eq!(sent.len(), 2);
        assert_eq!(sent[0].0, magic_packet(mac).to_vec());
        assert_eq!(sent[0].1, SocketAddrV4::new(Ipv4Addr::BROADCAST, 9));
        assert_eq!(sent[1].1, SocketAddrV4::new(Ipv4Addr::BROADCAST, 7));
    }

    /// **Nothing goes on the wire for a device the user has not approved**, and
    /// the refusal names approval rather than the missing-address case — the
    /// two have different remedies.
    #[test]
    fn an_unapproved_device_is_refused_and_no_packet_is_sent() {
        let (store, _dir) = new_store();
        store
            .remember(
                &desktop(),
                "de:ad:be:ef:00:01".parse().unwrap(),
                recorded_at(),
            )
            .unwrap();

        for trust in [
            FakeTrust::PinnedOnly,
            FakeTrust::Unknown,
            FakeTrust::Expired,
        ] {
            let socket = FakeSocket(RefCell::new(Vec::new()));
            let err = wake_device(&store, &trust, &socket, &desktop(), Ipv4Addr::BROADCAST)
                .expect_err("an unapproved device must not be woken");
            assert!(
                matches!(err, WakeError::NotPermitted { .. }),
                "expected a permission refusal, got {err:?}"
            );
            assert!(
                socket.0.borrow().is_empty(),
                "a refused wake must put nothing on the wire"
            );
        }
    }

    /// An approved device with no address recorded is refused for *that*
    /// reason, so the user is told to record one instead of being sent to
    /// review permissions they already granted.
    #[test]
    fn a_device_with_no_recorded_address_says_so_specifically() {
        let (store, _dir) = new_store();
        let socket = FakeSocket(RefCell::new(Vec::new()));

        let err = wake_device(
            &store,
            &FakeTrust::Approved,
            &socket,
            &desktop(),
            Ipv4Addr::BROADCAST,
        )
        .expect_err("there is no address to wake");
        assert!(
            matches!(err, WakeError::NotRecorded { .. }),
            "expected a not-recorded refusal, got {err:?}"
        );
        assert!(socket.0.borrow().is_empty());
    }

    /// **Forgetting the address revokes waking.** The wake that worked a moment
    /// ago now sends nothing, with the device still approved and nothing else
    /// changed — which is what makes the consent revocable per capability (I6).
    #[test]
    fn forgetting_the_address_revokes_waking_without_touching_trust() {
        let (store, _dir) = new_store();
        store
            .remember(
                &desktop(),
                "de:ad:be:ef:00:01".parse().unwrap(),
                recorded_at(),
            )
            .unwrap();
        let socket = FakeSocket(RefCell::new(Vec::new()));
        wake_device(
            &store,
            &FakeTrust::Approved,
            &socket,
            &desktop(),
            Ipv4Addr::BROADCAST,
        )
        .expect("precondition: it wakes while the address is recorded");

        store.forget(&desktop()).unwrap();

        let socket = FakeSocket(RefCell::new(Vec::new()));
        let err = wake_device(
            &store,
            &FakeTrust::Approved,
            &socket,
            &desktop(),
            Ipv4Addr::BROADCAST,
        )
        .expect_err("a forgotten address is a revoked wake");
        assert!(matches!(err, WakeError::NotRecorded { .. }), "got {err:?}");
        assert!(socket.0.borrow().is_empty());
    }

    /// A wake goes to the broadcast address the caller chose — the case a
    /// multi-homed host waking a machine on one particular subnet depends on.
    #[test]
    fn the_wake_uses_the_broadcast_address_the_caller_named() {
        let (store, _dir) = new_store();
        store
            .remember(
                &desktop(),
                "de:ad:be:ef:00:01".parse().unwrap(),
                recorded_at(),
            )
            .unwrap();
        let socket = FakeSocket(RefCell::new(Vec::new()));
        let subnet = Ipv4Addr::new(192, 168, 8, 255);

        let attempt =
            wake_device(&store, &FakeTrust::Approved, &socket, &desktop(), subnet).unwrap();

        assert_eq!(
            attempt.sent_to,
            vec![SocketAddrV4::new(subnet, 9), SocketAddrV4::new(subnet, 7)]
        );
    }
}
