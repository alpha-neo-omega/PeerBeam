//! The magic packet, and where it is addressed.

use std::net::{Ipv4Addr, SocketAddrV4};

use crate::mac::{MacAddress, MAC_LEN};

/// The synchronisation stream a magic packet opens with: six `0xFF` bytes.
///
/// It is what the network card's pattern matcher scans for; the sixteen copies
/// of the target address follow it immediately. Named rather than inlined
/// because the packet test asserts against it, and a constant that both the
/// builder and its test read would prove nothing — so the test writes the
/// expected bytes out by hand and this is only used by the builder.
const SYNC_STREAM: [u8; 6] = [0xFF; 6];

/// How many times the target's address is repeated after the sync stream.
///
/// Sixteen, fixed by the format. The card is matching a pattern in a frame it
/// receives while the host is powered down, and this repetition is what makes
/// the pattern unmistakable at any offset within any carrier protocol.
const MAC_REPEATS: usize = 16;

/// A magic packet is exactly 102 bytes: `6 + 6 * 16`.
///
/// Public because a caller that logs or buffers one should not have to
/// rediscover the number, and because "is it 102?" is the first question anyone
/// debugging a wake that did nothing will ask.
pub const MAGIC_PACKET_LEN: usize = SYNC_STREAM.len() + MAC_LEN * MAC_REPEATS;

/// The UDP ports a magic packet is sent to.
///
/// # Neither of these is really "the" Wake-on-LAN port
///
/// The format has no assigned port and does not care about one. The frame is
/// matched by the network card's pattern matcher, which scans the whole frame
/// for the sync stream and the repeated address; the UDP header above it is
/// carried purely because UDP broadcast is the convenient way to get bytes onto
/// every port of a switch. Ports 9 (discard) and 7 (echo) are the two the world
/// settled on, and both are "somewhere harmless to address it".
///
/// # Both are sent, always — "fallback" would be a lie here
///
/// A fallback implies learning that the first attempt failed, and this protocol
/// has nothing to learn it from: there is no acknowledgement, and a sleeping
/// machine sends nothing back by definition. So the second port cannot be tried
/// "if the first does not work" — nobody would ever find out.
///
/// They are therefore both sent unconditionally, which costs 102 extra bytes on
/// one local broadcast and buys the case that actually happens: some routers
/// and managed switches will relay a directed broadcast to one of these ports
/// and not the other, depending on how their Wake-on-LAN helper was configured.
/// A machine that is already awake ignores both — port 9 discards, and port 7
/// echoes to a socket nobody is reading.
pub const WOL_PORTS: [u16; 2] = [9, 7];

/// Build the magic packet that wakes `mac`.
///
/// Pure, total, and allocation-free: given an address there is exactly one
/// correct packet, and nothing about producing it can fail. That matters more
/// here than it usually would — see [`crate::send_magic_packet`] for why a
/// malformed packet would produce no error anywhere, on either machine, ever.
#[must_use]
pub fn magic_packet(mac: MacAddress) -> [u8; MAGIC_PACKET_LEN] {
    let mut packet = [0u8; MAGIC_PACKET_LEN];
    packet[..SYNC_STREAM.len()].copy_from_slice(&SYNC_STREAM);
    let octets = mac.octets();
    for repeat in 0..MAC_REPEATS {
        let at = SYNC_STREAM.len() + repeat * MAC_LEN;
        packet[at..at + MAC_LEN].copy_from_slice(&octets);
    }
    packet
}

/// The two addresses one wake is sent to.
///
/// `broadcast` is an IPv4 broadcast address and the caller's choice of
/// **which broadcast domain to shout into**:
///
/// * [`Ipv4Addr::BROADCAST`] (`255.255.255.255`) — the limited broadcast, the
///   right default. It is never routed, so it reaches the segment the packet
///   leaves on and no further.
/// * A subnet's own broadcast (`192.168.1.255`) — useful on a multi-homed host,
///   where the limited broadcast goes out whichever interface the routing table
///   picks and that may not be the one the sleeping machine is plugged into.
///
/// There is no IPv6 form. Wake-on-LAN is an Ethernet-frame pattern and IPv6 has
/// no broadcast address at all; every deployment of this in the world is IPv4
/// broadcast, and inventing a multicast variant nothing listens for would be
/// worse than not offering one.
#[must_use]
pub fn broadcast_targets(broadcast: Ipv4Addr) -> [SocketAddrV4; WOL_PORTS.len()] {
    WOL_PORTS.map(|port| SocketAddrV4::new(broadcast, port))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **The wire format, written out by hand.**
    ///
    /// This is deliberately not built with a loop or from the constants above —
    /// a test that constructs its expectation the same way the code does only
    /// proves the code is self-consistent, and self-consistent is exactly what
    /// a wrong magic packet is. Wake-on-LAN has no acknowledgement, so a
    /// packet with fifteen repeats, or five `0xFF`s, or the address reversed,
    /// fails by doing nothing at all: no error on this machine, no error on
    /// the other, no log line anywhere. This literal is the only thing standing
    /// between that and a shipped feature.
    ///
    /// 6 × `0xFF`, then `de:ad:be:ef:00:01` sixteen times. 102 bytes.
    #[test]
    fn the_packet_matches_a_hand_written_vector() {
        let expected: Vec<u8> = vec![
            0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, // sync stream
            0xde, 0xad, 0xbe, 0xef, 0x00, 0x01, // 1
            0xde, 0xad, 0xbe, 0xef, 0x00, 0x01, // 2
            0xde, 0xad, 0xbe, 0xef, 0x00, 0x01, // 3
            0xde, 0xad, 0xbe, 0xef, 0x00, 0x01, // 4
            0xde, 0xad, 0xbe, 0xef, 0x00, 0x01, // 5
            0xde, 0xad, 0xbe, 0xef, 0x00, 0x01, // 6
            0xde, 0xad, 0xbe, 0xef, 0x00, 0x01, // 7
            0xde, 0xad, 0xbe, 0xef, 0x00, 0x01, // 8
            0xde, 0xad, 0xbe, 0xef, 0x00, 0x01, // 9
            0xde, 0xad, 0xbe, 0xef, 0x00, 0x01, // 10
            0xde, 0xad, 0xbe, 0xef, 0x00, 0x01, // 11
            0xde, 0xad, 0xbe, 0xef, 0x00, 0x01, // 12
            0xde, 0xad, 0xbe, 0xef, 0x00, 0x01, // 13
            0xde, 0xad, 0xbe, 0xef, 0x00, 0x01, // 14
            0xde, 0xad, 0xbe, 0xef, 0x00, 0x01, // 15
            0xde, 0xad, 0xbe, 0xef, 0x00, 0x01, // 16
        ];
        assert_eq!(expected.len(), 102, "the vector itself must be 102 bytes");

        let mac: MacAddress = "de:ad:be:ef:00:01".parse().unwrap();
        assert_eq!(magic_packet(mac).as_slice(), expected.as_slice());
    }

    /// The length is 102 for every address, and the constant says so.
    #[test]
    fn every_packet_is_102_bytes() {
        assert_eq!(MAGIC_PACKET_LEN, 102);
        for text in [
            "de:ad:be:ef:00:01",
            "02:42:ac:11:00:02",
            "fe:ff:ff:ff:ff:fe",
        ] {
            let mac: MacAddress = text.parse().unwrap();
            assert_eq!(magic_packet(mac).len(), 102, "{text}");
        }
    }

    /// The prefix is six `0xFF` bytes and the seventh byte is already the
    /// address — an off-by-one in the sync stream is the classic way to build
    /// a packet that looks right in a hex dump and matches nothing.
    #[test]
    fn the_sync_stream_is_exactly_six_bytes_long() {
        let mac: MacAddress = "de:ad:be:ef:00:01".parse().unwrap();
        let packet = magic_packet(mac);
        assert_eq!(&packet[..6], &[0xFF; 6]);
        assert_eq!(
            packet[6], 0xde,
            "the address must start at offset 6, not 5 or 7"
        );
    }

    /// Each of the sixteen repeats is the address in wire order — not
    /// reversed, and not the same repeat written sixteen times by accident.
    #[test]
    fn the_address_is_repeated_sixteen_times_in_wire_order() {
        // Six distinct octets, so a transposition inside one repeat shows up.
        // `02:…` rather than the tidier `01:…` because an odd first octet is
        // multicast and the parser rightly refuses it.
        let mac: MacAddress = "02:23:45:67:89:ab".parse().unwrap();
        let packet = magic_packet(mac);
        for repeat in 0..16 {
            let at = 6 + repeat * 6;
            assert_eq!(
                &packet[at..at + 6],
                &mac.octets(),
                "repeat {repeat} (offset {at}) is not the address in wire order"
            );
        }
        // ...and nothing follows the sixteenth.
        assert_eq!(6 + 16 * 6, packet.len());
    }

    /// Two different addresses produce two different packets, everywhere but
    /// the sync stream. A builder that ignored its argument would pass every
    /// length and prefix check above.
    #[test]
    fn the_packet_depends_on_the_address() {
        let a = magic_packet("de:ad:be:ef:00:01".parse().unwrap());
        let b = magic_packet("de:ad:be:ef:00:02".parse().unwrap());
        assert_ne!(a.as_slice(), b.as_slice());
        assert_eq!(&a[..6], &b[..6], "only the sync stream is shared");
    }

    /// One wake goes to both ports of the same broadcast address, port 9
    /// first — see [`WOL_PORTS`] for why both, and why "fallback" is the wrong
    /// word for the second.
    #[test]
    fn a_wake_is_addressed_to_ports_9_and_7_of_the_given_broadcast() {
        assert_eq!(
            broadcast_targets(Ipv4Addr::BROADCAST),
            [
                SocketAddrV4::new(Ipv4Addr::BROADCAST, 9),
                SocketAddrV4::new(Ipv4Addr::BROADCAST, 7),
            ]
        );
        let subnet = Ipv4Addr::new(192, 168, 1, 255);
        assert_eq!(
            broadcast_targets(subnet),
            [SocketAddrV4::new(subnet, 9), SocketAddrV4::new(subnet, 7)]
        );
    }
}
