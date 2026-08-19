//! A hardware address, and a parser for the forms people actually paste.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// How many octets a MAC-48 address has. Named because both the parser and the
/// magic packet's repeat count are stated in terms of it.
pub const MAC_LEN: usize = 6;

/// Why a string is not a hardware address.
///
/// Every variant names **what was wrong with the input**, not merely that
/// something was. A wake that never happens reports nothing (see
/// [`crate::send_magic_packet`] for why the protocol cannot), so the parse is
/// the only place in the whole feature that can tell a person they typed
/// something wrong — a bare "invalid MAC address" would spend that one chance.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum MacError {
    #[error("a hardware address was expected, but the text was empty")]
    Empty,

    /// Two different separators in one address — `aa:bb-cc:dd-ee:ff`, or a
    /// colon form with a stray dot. Almost always half of one address pasted
    /// onto half of another, which is worth saying plainly rather than
    /// reporting as a group-count problem two steps later.
    #[error("mixed separators ({first:?} and {second:?}): a hardware address uses one throughout")]
    MixedSeparators { first: char, second: char },

    #[error(
        "{sep:?}-separated hardware addresses have {expected} groups, but this has {found}: {input:?}"
    )]
    GroupCount {
        sep: char,
        expected: usize,
        found: usize,
        input: String,
    },

    #[error("{sep:?}-separated groups are {expected} hex digits each, but {group:?} has {found}")]
    GroupWidth {
        sep: char,
        expected: usize,
        found: usize,
        group: String,
    },

    /// A bare run of hex with no separators at all must be exactly 12 digits.
    #[error(
        "a hardware address with no separators is 12 hex digits, but this has {found}: {input:?}"
    )]
    BareLength { found: usize, input: String },

    #[error("{ch:?} is not a hex digit (0-9, a-f, A-F): {input:?}")]
    NotHex { ch: char, input: String },

    /// `00:00:00:00:00:00`. Syntactically fine, and the single most common
    /// thing to paste by accident: it is what many tunnel and VPN adapters
    /// report as their "MAC address", and what a lookup that failed tends to
    /// fill a field with. Refused here rather than stored, because a stored
    /// zero address produces a perfectly well-formed magic packet that wakes
    /// nothing, forever, while reporting success — Wake-on-LAN has no
    /// acknowledgement to contradict it with.
    #[error("00:00:00:00:00:00 is not a real hardware address (a tunnel or VPN adapter often reports it)")]
    AllZero,

    /// The low bit of the first octet is the IEEE group bit: set means
    /// multicast, and no network card's *own* address has it set. Catching it
    /// here catches `ff:ff:ff:ff:ff:ff` — someone pasting the broadcast address
    /// — along with every transposed-digit typo that lands on an odd first
    /// octet. Same reasoning as [`AllZero`](MacError::AllZero): the alternative
    /// is a silent no-op with no failure signal anywhere.
    #[error("{mac} is a multicast address, not a network card's own address")]
    Multicast { mac: String },
}

/// A MAC-48 hardware address.
///
/// Stored as octets and rendered lower-case colon-separated, which is also how
/// it is serialized: an address on disk stays legible to the person whose
/// machine it is, and a hand-edit that mangles it fails loudly on the next read
/// instead of becoming a packet that wakes nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct MacAddress([u8; MAC_LEN]);

impl MacAddress {
    /// Wrap six octets, applying the same two refusals the parser makes.
    ///
    /// Deliberately fallible, and deliberately the *only* constructor: a
    /// `from_octets` that skipped the checks would be the obvious way to put an
    /// all-zero or multicast address into the store, and the store is precisely
    /// where such an address does its damage — silently, months later, when
    /// someone presses wake.
    pub fn new(octets: [u8; MAC_LEN]) -> Result<Self, MacError> {
        if octets == [0u8; MAC_LEN] {
            return Err(MacError::AllZero);
        }
        // Bit 0 of octet 0 is the IEEE I/G bit. Bit 1 (locally administered) is
        // deliberately *not* checked: `02:…` addresses are ordinary on virtual
        // machines and container bridges, and those are real wake targets.
        if octets[0] & 0x01 != 0 {
            return Err(MacError::Multicast {
                mac: render(&octets),
            });
        }
        Ok(MacAddress(octets))
    }

    /// The six octets, in wire order.
    #[must_use]
    pub const fn octets(&self) -> [u8; MAC_LEN] {
        self.0
    }
}

/// Lower-case, colon-separated — the canonical form, shared by `Display` and by
/// the error that has to quote an address it is about to reject.
fn render(octets: &[u8; MAC_LEN]) -> String {
    octets
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<Vec<_>>()
        .join(":")
}

impl fmt::Display for MacAddress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&render(&self.0))
    }
}

impl From<MacAddress> for String {
    fn from(mac: MacAddress) -> String {
        mac.to_string()
    }
}

impl TryFrom<String> for MacAddress {
    type Error = MacError;
    fn try_from(s: String) -> Result<Self, MacError> {
        s.parse()
    }
}

impl FromStr for MacAddress {
    type Err = MacError;

    /// Accepts the four shapes a hardware address is written in, in any case:
    ///
    /// * `aa:bb:cc:dd:ee:ff` — IEEE / Linux / macOS,
    /// * `AA-BB-CC-DD-EE-FF` — Windows `ipconfig`,
    /// * `aabb.ccdd.eeff` — Cisco,
    /// * `aabbccddeeff` — bare, as several web UIs and BIOS screens print it.
    ///
    /// # Why the shape is validated and not just the digits
    ///
    /// The cheap implementation strips every separator and checks for twelve
    /// hex digits. It also accepts `aa:bb:ccdd:ee:ff` and `aa::bb:cc:dd:eeff`
    /// — text that is not a hardware address in any notation, and that a person
    /// produced by mis-selecting when they copied. Since a wrong-but-plausible
    /// address is indistinguishable from a right one once it is on the wire,
    /// the separator structure is checked as strictly as the digits are.
    fn from_str(s: &str) -> Result<Self, MacError> {
        let input = s.trim();
        if input.is_empty() {
            return Err(MacError::Empty);
        }

        match separator(input)? {
            // 6 groups of 2: `aa:bb:cc:dd:ee:ff` and `AA-BB-CC-DD-EE-FF`.
            Some(sep @ (':' | '-')) => grouped(input, sep, 6, 2),
            // 3 groups of 4: Cisco's `aabb.ccdd.eeff`.
            Some(sep @ '.') => grouped(input, sep, 3, 4),
            Some(_) => unreachable!("`separator` only ever reports ':', '-' or '.'"),
            None => {
                if input.chars().count() != MAC_LEN * 2 {
                    return Err(MacError::BareLength {
                        found: input.chars().count(),
                        input: input.to_string(),
                    });
                }
                MacAddress::new(octets_from_hex(input, input)?)
            }
        }
    }
}

/// Which separator this address uses, or `None` for a bare run of hex.
///
/// Refuses two different ones rather than picking the first: an address that
/// mixes them is not a near-miss of either notation, it is two fragments.
fn separator(input: &str) -> Result<Option<char>, MacError> {
    let mut found: Option<char> = None;
    for ch in input.chars().filter(|c| matches!(c, ':' | '-' | '.')) {
        match found {
            None => found = Some(ch),
            Some(first) if first != ch => {
                return Err(MacError::MixedSeparators { first, second: ch })
            }
            Some(_) => {}
        }
    }
    Ok(found)
}

/// Split on `sep`, require exactly `groups` groups of exactly `width` hex
/// digits, and fold them into six octets.
///
/// `groups * width` is always 12, so the two notations differ only in how the
/// same twelve digits are punctuated — which is why one function serves both
/// and why a third notation (were one ever needed) is a call, not a branch.
fn grouped(input: &str, sep: char, groups: usize, width: usize) -> Result<MacAddress, MacError> {
    let parts: Vec<&str> = input.split(sep).collect();
    if parts.len() != groups {
        return Err(MacError::GroupCount {
            sep,
            expected: groups,
            found: parts.len(),
            input: input.to_string(),
        });
    }
    for part in &parts {
        // Counted in `char`s, not bytes: a group of non-ASCII text would
        // otherwise report a length nobody typed.
        let found = part.chars().count();
        if found != width {
            return Err(MacError::GroupWidth {
                sep,
                expected: width,
                found,
                group: (*part).to_string(),
            });
        }
    }
    MacAddress::new(octets_from_hex(&parts.concat(), input)?)
}

/// Twelve hex digits to six octets. `input` is carried only so an error can
/// quote what the user actually typed rather than the de-punctuated form.
fn octets_from_hex(digits: &str, input: &str) -> Result<[u8; MAC_LEN], MacError> {
    // Every digit is checked before any is converted, because
    // `u8::from_str_radix` is *not* the validator it looks like: it accepts a
    // leading sign, so `"+f"` parses happily as 15 and `"-1"` as an error for
    // the wrong reason. A sign character in a hardware address is a typo, and
    // it has to be reported as one.
    if let Some(ch) = digits.chars().find(|c| !c.is_ascii_hexdigit()) {
        return Err(MacError::NotHex {
            ch,
            input: input.to_string(),
        });
    }
    let bytes = digits.as_bytes();
    let mut octets = [0u8; MAC_LEN];
    for (i, octet) in octets.iter_mut().enumerate() {
        // Safe to index: `is_ascii_hexdigit` above proves the string is ASCII,
        // so `chars().count()` — already checked by both callers — equals the
        // byte length.
        let pair = std::str::from_utf8(&bytes[i * 2..i * 2 + 2])
            .expect("ASCII hex digits are valid UTF-8");
        *octet = u8::from_str_radix(pair, 16).expect("two ASCII hex digits always fit in a u8");
    }
    Ok(octets)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The one address every other test in this crate is written against.
    fn deadbeef() -> MacAddress {
        MacAddress::new([0xde, 0xad, 0xbe, 0xef, 0x00, 0x01]).unwrap()
    }

    /// **The accept table.** Every notation, in every case, must land on the
    /// same six octets — the whole point of a parser here is that a person can
    /// paste whatever their operating system showed them.
    #[test]
    fn every_notation_people_paste_parses_to_the_same_address() {
        let accepted = [
            "de:ad:be:ef:00:01",
            "DE:AD:BE:EF:00:01",
            "De:Ad:Be:Ef:00:01",
            "de-ad-be-ef-00-01",
            "DE-AD-BE-EF-00-01",
            "dead.beef.0001",
            "DEAD.BEEF.0001",
            "deadbeef0001",
            "DEADBEEF0001",
            "  de:ad:be:ef:00:01  ",
            "\tDE-AD-BE-EF-00-01\n",
        ];
        for text in accepted {
            let mac: MacAddress = text.parse().unwrap_or_else(|e| panic!("{text:?}: {e}"));
            assert_eq!(
                mac,
                deadbeef(),
                "{text:?} parsed to {mac} instead of de:ad:be:ef:00:01"
            );
        }
    }

    /// **The reject table**, each row paired with the reason it must give.
    ///
    /// The reason is asserted, not just the failure: a parser that answered
    /// "invalid" to all of these would pass a test that only checked
    /// `is_err()`, and the person who mistyped would learn nothing. Note the
    /// third block especially — those are the strings a "strip the separators
    /// and count to twelve" parser accepts.
    #[test]
    fn malformed_addresses_are_refused_with_the_reason() {
        let rejected: &[(&str, MacError)] = &[
            ("", MacError::Empty),
            ("   ", MacError::Empty),
            // Mixed notations: two fragments, not a near-miss of either.
            (
                "de:ad-be:ef-00:01",
                MacError::MixedSeparators {
                    first: ':',
                    second: '-',
                },
            ),
            (
                "dead.beef:0001",
                MacError::MixedSeparators {
                    first: '.',
                    second: ':',
                },
            ),
            // Wrong number of groups.
            (
                "de:ad:be:ef:00",
                MacError::GroupCount {
                    sep: ':',
                    expected: 6,
                    found: 5,
                    input: "de:ad:be:ef:00".into(),
                },
            ),
            (
                "de:ad:be:ef:00:01:02",
                MacError::GroupCount {
                    sep: ':',
                    expected: 6,
                    found: 7,
                    input: "de:ad:be:ef:00:01:02".into(),
                },
            ),
            (
                "de:ad:be:ef:00:01:",
                MacError::GroupCount {
                    sep: ':',
                    expected: 6,
                    found: 7,
                    input: "de:ad:be:ef:00:01:".into(),
                },
            ),
            (
                "dead.beef",
                MacError::GroupCount {
                    sep: '.',
                    expected: 3,
                    found: 2,
                    input: "dead.beef".into(),
                },
            ),
            // The shapes a lenient "strip and count" parser lets through.
            (
                "de:ad:beef:00:01",
                MacError::GroupCount {
                    sep: ':',
                    expected: 6,
                    found: 5,
                    input: "de:ad:beef:00:01".into(),
                },
            ),
            (
                "de::ad:be:ef:00:01",
                MacError::GroupCount {
                    sep: ':',
                    expected: 6,
                    found: 7,
                    input: "de::ad:be:ef:00:01".into(),
                },
            ),
            (
                "d:ead:be:ef:00:01",
                MacError::GroupWidth {
                    sep: ':',
                    expected: 2,
                    found: 1,
                    group: "d".into(),
                },
            ),
            (
                "dead.bee.f0001",
                MacError::GroupWidth {
                    sep: '.',
                    expected: 4,
                    found: 3,
                    group: "bee".into(),
                },
            ),
            // Bare runs of hex that are the wrong length.
            (
                "deadbeef000",
                MacError::BareLength {
                    found: 11,
                    input: "deadbeef000".into(),
                },
            ),
            (
                "deadbeef00011",
                MacError::BareLength {
                    found: 13,
                    input: "deadbeef00011".into(),
                },
            ),
            // Not hex. The `+`/`-` rows are the ones `from_str_radix` would
            // have swallowed on its own.
            (
                "de:ad:be:ef:00:0g",
                MacError::NotHex {
                    ch: 'g',
                    input: "de:ad:be:ef:00:0g".into(),
                },
            ),
            (
                "de:ad:be:ef:00:+1",
                MacError::NotHex {
                    ch: '+',
                    input: "de:ad:be:ef:00:+1".into(),
                },
            ),
            (
                "de:ad:be:ef:00:0 ",
                MacError::GroupWidth {
                    sep: ':',
                    expected: 2,
                    found: 1,
                    group: "0".into(),
                },
            ),
            (
                "de:ad:be:ef:00:zz",
                MacError::NotHex {
                    ch: 'z',
                    input: "de:ad:be:ef:00:zz".into(),
                },
            ),
            // Syntactically fine, semantically not an address.
            ("00:00:00:00:00:00", MacError::AllZero),
            ("000000000000", MacError::AllZero),
            (
                "ff:ff:ff:ff:ff:ff",
                MacError::Multicast {
                    mac: "ff:ff:ff:ff:ff:ff".into(),
                },
            ),
            (
                "01:00:5e:00:00:fb",
                MacError::Multicast {
                    mac: "01:00:5e:00:00:fb".into(),
                },
            ),
        ];

        for (text, expected) in rejected {
            let err = match text.parse::<MacAddress>() {
                Err(e) => e,
                Ok(mac) => panic!("{text:?} must not parse, but produced {mac}"),
            };
            assert_eq!(&err, expected, "{text:?} was refused for the wrong reason");
        }
    }

    /// A locally-administered address (`02:…`, the second bit set) is what
    /// every virtual machine, container bridge and USB tether presents, and
    /// those are real wake targets. Only the *group* bit is refused.
    #[test]
    fn a_locally_administered_address_is_accepted() {
        let mac: MacAddress = "02:42:ac:11:00:02".parse().unwrap();
        assert_eq!(mac.octets()[0], 0x02);
    }

    /// The boundary the multicast check actually turns on: bit 0 of octet 0,
    /// and nothing else about the address.
    #[test]
    fn only_an_odd_first_octet_is_refused_as_multicast() {
        for first in 0u8..=255 {
            let result = MacAddress::new([first, 0xad, 0xbe, 0xef, 0x00, 0x01]);
            assert_eq!(
                result.is_err(),
                first % 2 == 1,
                "first octet {first:#04x}: the group bit is bit 0 and nothing else"
            );
        }
    }

    /// Rendering is canonical and round-trips: whatever a person pasted, what
    /// is stored and shown back is one form.
    #[test]
    fn display_is_lower_case_colon_separated_and_round_trips() {
        assert_eq!(deadbeef().to_string(), "de:ad:be:ef:00:01");
        for text in ["DE-AD-BE-EF-00-01", "dead.beef.0001", "DEADBEEF0001"] {
            let once: MacAddress = text.parse().unwrap();
            let twice: MacAddress = once.to_string().parse().unwrap();
            assert_eq!(once, twice, "{text:?} did not survive a render/parse cycle");
        }
    }

    /// Serialized as its canonical text, so a store stays readable — and a
    /// hand-edited address that is no longer valid fails on read rather than
    /// becoming a packet that wakes nothing.
    #[test]
    fn serde_uses_the_canonical_text_and_revalidates_on_read() {
        let json = serde_json::to_string(&deadbeef()).unwrap();
        assert_eq!(json, "\"de:ad:be:ef:00:01\"");
        assert_eq!(
            serde_json::from_str::<MacAddress>(&json).unwrap(),
            deadbeef()
        );

        for hand_edited in ["\"00:00:00:00:00:00\"", "\"nonsense\"", "\"de:ad:be:ef\""] {
            assert!(
                serde_json::from_str::<MacAddress>(hand_edited).is_err(),
                "{hand_edited} must not deserialize into an address"
            );
        }
    }

    /// Every octet position is carried through independently — a parser that
    /// dropped, duplicated or transposed one would pass every test above that
    /// uses a single fixed address.
    #[test]
    fn each_octet_position_is_preserved() {
        for pos in 0..MAC_LEN {
            let mut octets = [0x22u8; MAC_LEN];
            octets[pos] = 0x9c;
            let mac = MacAddress::new(octets).unwrap();
            let reparsed: MacAddress = mac.to_string().parse().unwrap();
            assert_eq!(
                reparsed.octets(),
                octets,
                "octet {pos} did not survive render/parse"
            );
        }
    }
}
