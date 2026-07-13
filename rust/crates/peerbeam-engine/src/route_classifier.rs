//! Classifies a peer address into a [`RouteKind`] — its transport-priority
//! class. Injected into the [`RouteManager`](crate::RouteManager) so the
//! OS/heuristic-specific part is swappable and testable.

use std::net::IpAddr;

use peerbeam_domain::entity::RouteKind;

/// Maps a peer address to the route class it belongs to.
pub trait RouteClassifier: Send + Sync {
    /// Classify one reachable address of a peer.
    fn classify(&self, address: &str) -> RouteKind;
}

/// Default classifier by address range.
///
/// An address alone cannot distinguish Ethernet / Wi-Fi / USB-tethering — they
/// share the private ranges — so those refine only with local-interface info
/// (a future, interface-aware classifier can override this). What it *can*
/// tell apart reliably:
///
/// - **Tailscale** — `100.64.0.0/10` (CGNAT) or the Tailscale IPv6 ULA.
/// - **LAN** — loopback and RFC1918 / ULA / link-local private addresses.
/// - **Direct internet** — anything globally routable (or a hostname).
pub struct AddressClassifier;

impl RouteClassifier for AddressClassifier {
    fn classify(&self, address: &str) -> RouteKind {
        match address.parse::<IpAddr>() {
            Ok(ip) if is_tailscale(ip) => RouteKind::TailscaleDirect,
            Ok(ip) if ip.is_loopback() => RouteKind::Lan,
            Ok(ip) if is_private(ip) => RouteKind::Lan,
            Ok(_) => RouteKind::DirectInternet,
            // A hostname (e.g. MagicDNS) — assume routable.
            Err(_) => RouteKind::DirectInternet,
        }
    }
}

/// Tailscale's address space: IPv4 `100.64.0.0/10`, IPv6 `fd7a:115c:a1e0::/48`.
fn is_tailscale(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let o = v4.octets();
            o[0] == 100 && (64..=127).contains(&o[1])
        }
        IpAddr::V6(v6) => {
            let s = v6.segments();
            s[0] == 0xfd7a && s[1] == 0x115c && s[2] == 0xa1e0
        }
    }
}

/// Private / non-globally-routable address (treated as LAN).
fn is_private(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_private() || v4.is_link_local(),
        // Unique-local (fc00::/7) or link-local (fe80::/10).
        IpAddr::V6(v6) => {
            let first = v6.segments()[0];
            (first & 0xfe00) == 0xfc00 || (first & 0xffc0) == 0xfe80
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_by_range() {
        let c = AddressClassifier;
        assert_eq!(c.classify("127.0.0.1"), RouteKind::Lan);
        assert_eq!(c.classify("192.168.1.5"), RouteKind::Lan);
        assert_eq!(c.classify("10.0.0.9"), RouteKind::Lan);
        assert_eq!(c.classify("172.16.4.2"), RouteKind::Lan);
        assert_eq!(c.classify("100.73.134.21"), RouteKind::TailscaleDirect);
        assert_eq!(c.classify("203.0.113.7"), RouteKind::DirectInternet);
        assert_eq!(c.classify("peer.tailnet.ts.net"), RouteKind::DirectInternet);
    }

    #[test]
    fn classifies_ipv6() {
        let c = AddressClassifier;
        assert_eq!(c.classify("::1"), RouteKind::Lan);
        assert_eq!(c.classify("fd12:3456::1"), RouteKind::Lan); // ULA
        assert_eq!(c.classify("fd7a:115c:a1e0::1"), RouteKind::TailscaleDirect);
        assert_eq!(c.classify("2606:4700::1111"), RouteKind::DirectInternet);
    }
}
