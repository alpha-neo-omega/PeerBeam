//! Decoding hex that arrived from a peer.
//!
//! # Why this is not two lines at each call site
//!
//! It was, and both copies panicked on the same input.
//!
//! The obvious decoder walks the string two characters at a time — `(0..s.len())
//! .step_by(2)` and `&s[i..i + 2]`. But `str::len` counts **bytes** and `&s[..]`
//! slices by **bytes**, so the moment the string holds a character wider than
//! one byte the slice lands in the middle of it and Rust panics: *"byte index is
//! not a char boundary"*.
//!
//! A peer does not have to send anything unusual to get there. JSON escapes are
//! pure ASCII on the wire, so `"€a"` passes every byte-level filter and
//! serde hands the decoder `€a` — four bytes, an even length, and a first slice
//! that cuts `€` in half. The panic lands inside a channel actor, where it kills
//! the channel silently rather than refusing the frame.
//!
//! Both decoders that had this — `peerbeam-sync`'s chunk data and
//! `peerbeam-pairing`'s proof — run **before** the peer is approved.

/// Decode an even-length ASCII hex string.
///
/// Returns `None` for anything else: a non-ASCII character, an odd length, or a
/// digit outside `0-9a-fA-F`. It cannot panic on any input.
///
/// Bytes are taken in pairs rather than the string sliced, so the char-boundary
/// question the panic came from cannot be asked at all.
#[must_use]
pub fn decode(s: &str) -> Option<Vec<u8>> {
    if !s.is_ascii() || s.len() % 2 != 0 {
        return None;
    }
    s.as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            let hi = (pair[0] as char).to_digit(16)?;
            let lo = (pair[1] as char).to_digit(16)?;
            Some((hi * 16 + lo) as u8)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordinary_hex_decodes() {
        assert_eq!(decode("00ff10"), Some(vec![0x00, 0xff, 0x10]));
        assert_eq!(decode("DEADBEEF"), Some(vec![0xde, 0xad, 0xbe, 0xef]));
        assert_eq!(decode(""), Some(vec![]));
    }

    /// **The panic.** `€` is three bytes, so `"€a"` has an even byte length and
    /// the old `&s[0..2]` cut it in half. A peer reaches this with the pure
    /// ASCII JSON escape `"€a"`, so no byte-level filter upstream helps.
    #[test]
    fn a_multibyte_character_is_refused_rather_than_panicking() {
        assert_eq!(decode("€a"), None);
        assert_eq!(decode("é"), None, "two bytes, even length, one character");
        for s in ["\u{20ac}a", "a\u{20ac}", "\u{1F600}", "ff\u{e9}"] {
            assert_eq!(decode(s), None, "{s:?} must be refused, not panic");
        }
    }

    #[test]
    fn odd_lengths_and_non_digits_are_refused() {
        assert_eq!(decode("abc"), None);
        assert_eq!(decode("zz"), None);
        assert_eq!(decode("0g"), None);
    }

    /// Whatever the bytes, the answer is a value and never a panic.
    #[test]
    fn no_input_panics() {
        for n in 0u32..=0x2FF {
            if let Some(c) = char::from_u32(n) {
                let s: String = std::iter::repeat(c).take(3).collect();
                let _ = decode(&s);
            }
        }
    }
}
