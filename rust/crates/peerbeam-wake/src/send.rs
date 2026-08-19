//! Putting the packet on the wire, and the honest report of having done so.

use std::io;
use std::net::{Ipv4Addr, SocketAddrV4, UdpSocket};

use crate::error::WakeError;
use crate::mac::MacAddress;
use crate::packet::{broadcast_targets, magic_packet};

/// Somewhere to send a datagram.
///
/// A trait with one method, for one reason: so the send path can be tested
/// **without a network**. A test that needed a real socket would have to bind
/// one, broadcast onto whatever segment the build machine happens to be
/// attached to, and then have no way to observe what arrived — which is to say
/// the bytes that matter most in this crate would be the only ones never
/// asserted. With this, [`send_magic_packet`] is exercised against a double
/// that records the exact payload and the exact destination.
pub trait MagicSocket {
    /// Send `payload` to `target`, answering how many bytes went.
    fn send_to(&self, payload: &[u8], target: SocketAddrV4) -> io::Result<usize>;
}

/// What left this machine.
///
/// # This is not a confirmation, and it must never be read as one
///
/// Wake-on-LAN has no acknowledgement. The recipient is a network card in a
/// powered-down machine; it has no IP stack to answer with, and the sender's
/// UDP socket learns nothing either way. So there are exactly two things anyone
/// can honestly say after a wake, and both are here: *which addresses the
/// packet was sent to*, and *how many bytes each accepted*.
///
/// Whether the machine woke up is a different question with a different answer,
/// and it does not come from here. It comes from **discovery noticing the
/// device**: `peerbeam-discovery-udp` and the mDNS provider will report it when
/// it announces itself, typically some tens of seconds later. A surface that
/// wants to say "woken" must wait for that; one that renders this type may only
/// say "sent".
///
/// The type is named `WakeAttempt` for that reason. `WakeResult` and
/// `WakeOutcome` both read as a verdict on the target, and this is a receipt
/// for a shout into a room.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WakeAttempt {
    /// The address the packet names.
    pub mac: MacAddress,
    /// The broadcast targets that accepted the full packet, in the order it was
    /// sent to them. Never empty: a wake with nothing here is a
    /// [`WakeError::Send`] instead.
    pub sent_to: Vec<SocketAddrV4>,
    /// Bytes in each datagram — always [`crate::MAGIC_PACKET_LEN`], stated so a caller
    /// logging an attempt has the number without importing the constant.
    pub bytes: usize,
}

/// Broadcast the magic packet for `mac` to both [`WOL_PORTS`] of `broadcast`.
///
/// Returns as soon as it has finished trying — see [`WakeAttempt`] for what
/// that does and does not mean.
///
/// # Partial success is success, and a short write is not success
///
/// The two targets are independent shouts, so one being refused while the other
/// goes out is a wake that has genuinely been attempted; the attempt records
/// which ones took it. Only when **neither** accepted the packet is this an
/// error, and then it carries the last operating-system message so there is
/// something to act on.
///
/// A send that reports fewer than [`crate::MAGIC_PACKET_LEN`] bytes counts as a
/// refusal, not as a partial success. A truncated magic packet is not a weaker
/// magic packet — the card's pattern matcher finds no complete pattern and does
/// nothing at all — so counting it would mean reporting a wake for bytes that
/// cannot wake anything.
///
/// [`WOL_PORTS`]: crate::WOL_PORTS
pub fn send_magic_packet(
    socket: &dyn MagicSocket,
    mac: MacAddress,
    broadcast: Ipv4Addr,
) -> Result<WakeAttempt, WakeError> {
    let packet = magic_packet(mac);
    let mut sent_to = Vec::with_capacity(broadcast_targets(broadcast).len());
    let mut last_error: Option<String> = None;

    for target in broadcast_targets(broadcast) {
        match socket.send_to(&packet, target) {
            Ok(n) if n == packet.len() => sent_to.push(target),
            Ok(n) => {
                last_error = Some(format!(
                    "{target} accepted only {n} of {} bytes, which is not a magic packet",
                    packet.len()
                ));
            }
            Err(e) => last_error = Some(format!("{target}: {e}")),
        }
    }

    if sent_to.is_empty() {
        return Err(WakeError::Send(last_error.unwrap_or_else(|| {
            "no broadcast target was addressed".to_string()
        })));
    }
    Ok(WakeAttempt {
        mac,
        sent_to,
        bytes: packet.len(),
    })
}

/// The real socket: a UDP socket with broadcast enabled.
///
/// Bound to `0.0.0.0:0` — an ephemeral port on every interface. Nothing is ever
/// received on it, because there is nothing to receive: the machine being woken
/// cannot reply, and any echo from port 7 of a machine that is already awake is
/// of no interest. The socket exists solely as somewhere for datagrams to leave
/// from.
pub struct UdpBroadcast {
    socket: UdpSocket,
}

impl UdpBroadcast {
    /// Open a broadcast-capable socket.
    ///
    /// `set_broadcast(true)` is not optional decoration: without it the
    /// operating system refuses a datagram addressed to a broadcast address
    /// with `EACCES`, and the wake fails with a permission error that has
    /// nothing to do with the user's permissions.
    pub fn bind() -> io::Result<Self> {
        let socket = UdpSocket::bind(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 0))?;
        socket.set_broadcast(true)?;
        Ok(UdpBroadcast { socket })
    }
}

impl MagicSocket for UdpBroadcast {
    fn send_to(&self, payload: &[u8], target: SocketAddrV4) -> io::Result<usize> {
        // Blocking, and that is fine even on an async caller's thread: a
        // connectionless `send_to` hands 102 bytes to the kernel and returns.
        // It does not wait for a peer, a route probe or an acknowledgement —
        // there is no peer awake to provide one.
        self.socket.send_to(payload, target)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::packet::MAGIC_PACKET_LEN;
    use std::cell::RefCell;

    /// Records every datagram handed to it, and can be told to refuse some of
    /// them, so the partial-failure rules are exercised without a network.
    ///
    /// `RefCell` rather than a `Mutex`: [`send_magic_packet`] is synchronous
    /// and single-threaded by construction, and a lock here would only hide
    /// that.
    struct FakeSocket {
        sent: RefCell<Vec<(Vec<u8>, SocketAddrV4)>>,
        /// Ports that refuse outright, as an `io::Error`.
        refuse: Vec<u16>,
        /// Ports that accept, but report fewer bytes than they were given.
        truncate: Vec<u16>,
    }

    impl FakeSocket {
        fn accepting() -> Self {
            FakeSocket {
                sent: RefCell::new(Vec::new()),
                refuse: Vec::new(),
                truncate: Vec::new(),
            }
        }
        fn refusing(ports: &[u16]) -> Self {
            FakeSocket {
                refuse: ports.to_vec(),
                ..Self::accepting()
            }
        }
        fn truncating(ports: &[u16]) -> Self {
            FakeSocket {
                truncate: ports.to_vec(),
                ..Self::accepting()
            }
        }
        fn datagrams(&self) -> Vec<(Vec<u8>, SocketAddrV4)> {
            self.sent.borrow().clone()
        }
    }

    impl MagicSocket for FakeSocket {
        fn send_to(&self, payload: &[u8], target: SocketAddrV4) -> io::Result<usize> {
            if self.refuse.contains(&target.port()) {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "broadcast not permitted",
                ));
            }
            self.sent.borrow_mut().push((payload.to_vec(), target));
            if self.truncate.contains(&target.port()) {
                return Ok(payload.len() - 1);
            }
            Ok(payload.len())
        }
    }

    fn deadbeef() -> MacAddress {
        "de:ad:be:ef:00:01".parse().unwrap()
    }

    /// **The bytes and the destinations, asserted exactly.** This is the test
    /// the whole `MagicSocket` trait exists for: what goes out is the packet
    /// the format calls for, addressed to ports 9 and 7 of the broadcast the
    /// caller chose, and nothing else goes out at all.
    #[test]
    fn a_wake_sends_the_magic_packet_to_ports_9_and_7_and_nothing_else() {
        let socket = FakeSocket::accepting();
        let attempt = send_magic_packet(&socket, deadbeef(), Ipv4Addr::BROADCAST).unwrap();

        let sent = socket.datagrams();
        assert_eq!(sent.len(), 2, "one datagram per port, and no more");

        let expected = magic_packet(deadbeef());
        assert_eq!(sent[0].0, expected.to_vec(), "port 9 payload");
        assert_eq!(sent[1].0, expected.to_vec(), "port 7 payload");
        assert_eq!(sent[0].1, SocketAddrV4::new(Ipv4Addr::BROADCAST, 9));
        assert_eq!(sent[1].1, SocketAddrV4::new(Ipv4Addr::BROADCAST, 7));

        assert_eq!(attempt.mac, deadbeef());
        assert_eq!(attempt.bytes, MAGIC_PACKET_LEN);
        assert_eq!(attempt.sent_to, vec![sent[0].1, sent[1].1]);
    }

    /// The caller's broadcast address is used, not a hardcoded one — a
    /// multi-homed host waking a machine on a particular subnet depends on it.
    #[test]
    fn the_callers_broadcast_address_is_the_one_addressed() {
        let socket = FakeSocket::accepting();
        let subnet = Ipv4Addr::new(10, 42, 0, 255);
        send_magic_packet(&socket, deadbeef(), subnet).unwrap();

        for (_, target) in socket.datagrams() {
            assert_eq!(*target.ip(), subnet);
        }
    }

    /// One port refused, one accepted: the wake happened, and the attempt says
    /// which target took it rather than claiming both did.
    #[test]
    fn one_refused_port_still_leaves_a_wake_that_was_sent() {
        let socket = FakeSocket::refusing(&[9]);
        let attempt = send_magic_packet(&socket, deadbeef(), Ipv4Addr::BROADCAST).unwrap();

        assert_eq!(
            attempt.sent_to,
            vec![SocketAddrV4::new(Ipv4Addr::BROADCAST, 7)],
            "only port 7 accepted it, and only port 7 may be reported"
        );
        assert_eq!(socket.datagrams().len(), 1);
    }

    /// Both refused: an error, carrying what the operating system said, because
    /// there is no other signal anywhere that this wake did not happen.
    #[test]
    fn both_ports_refused_is_an_error_that_says_why() {
        let socket = FakeSocket::refusing(&[9, 7]);
        let err = send_magic_packet(&socket, deadbeef(), Ipv4Addr::BROADCAST)
            .expect_err("a wake that reached no target must not report success");

        let WakeError::Send(reason) = err else {
            panic!("expected a send failure, got {err:?}");
        };
        assert!(
            reason.contains("broadcast not permitted"),
            "the operating system's reason must survive: {reason}"
        );
    }

    /// **A short write is a refusal.** A truncated magic packet matches no
    /// pattern and wakes nothing, so counting it as sent would report a wake
    /// for bytes that cannot cause one.
    #[test]
    fn a_truncated_send_does_not_count_as_a_wake() {
        let socket = FakeSocket::truncating(&[9, 7]);
        let err = send_magic_packet(&socket, deadbeef(), Ipv4Addr::BROADCAST)
            .expect_err("101 of 102 bytes is not a magic packet");

        let WakeError::Send(reason) = err else {
            panic!("expected a send failure, got {err:?}");
        };
        assert!(
            reason.contains("101 of 102"),
            "the reason must name the short write: {reason}"
        );
    }

    /// The packet is rebuilt for the address it is given; sending twice to two
    /// devices does not send the first one's address twice.
    #[test]
    fn each_wake_carries_its_own_address() {
        let socket = FakeSocket::accepting();
        let other: MacAddress = "de:ad:be:ef:00:02".parse().unwrap();
        send_magic_packet(&socket, deadbeef(), Ipv4Addr::BROADCAST).unwrap();
        send_magic_packet(&socket, other, Ipv4Addr::BROADCAST).unwrap();

        let sent = socket.datagrams();
        assert_eq!(sent[0].0, magic_packet(deadbeef()).to_vec());
        assert_eq!(sent[2].0, magic_packet(other).to_vec());
        assert_ne!(sent[0].0, sent[2].0);
    }
}
