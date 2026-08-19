//! Optional PIN pairing: proving first contact reached the right device.
//!
//! # The gap this closes
//!
//! Trust-on-first-use pins whatever key answers first. If someone is on the
//! path during that very first handshake, they are pinned instead of the real
//! device, and every later connection looks perfectly consistent — the wrong
//! key, faithfully remembered. The existing 39-character pairing code closes
//! that gap for anyone who reads all 32 hex digits aloud. Most people will not.
//!
//! # Why the short code is not a truncated long one
//!
//! **A six-digit code derived from the two public keys would be worse than
//! useless.** An attacker mounting the very attack this exists to stop can
//! generate keypairs offline until one produces the digits the honest device
//! shows — a million tries, which is seconds of work — and then present a code
//! that matches while holding the wrong key. The same is true of any short code
//! derived from material the attacker sees before choosing their own: the
//! handshake sends `nonce_A` before `nonce_B`, so whoever speaks second can
//! grind their nonce to hit a chosen value.
//!
//! So the PIN here is **not derived from anything on the wire**. It is a fresh
//! random secret, shown on one device and typed into the other by a person, and
//! used as an HMAC key over the handshake transcript:
//!
//! ```text
//! receiver: pin = random 6 digits, shown on screen
//! sender:   person types the pin
//! sender  → receiver: proof = HMAC(pin, transcript)
//! receiver: recompute and compare; wrong ⇒ refuse to pin
//! ```
//!
//! An attacker in the middle sees a proof over *their* transcript with the
//! honest device and needs one over a *different* transcript to fool the other
//! side. Without the PIN they cannot produce it. They get **one online guess**
//! per attempt, with one chance in a million, against a limit of
//! [`MAX_ATTEMPTS`] — not the offline grinding a derived code would allow.
//!
//! This is the same construction as Bluetooth's passkey entry, and the reason
//! it is safe at six digits when a displayed-and-compared code is not.

use hmac::{Hmac, Mac};
use rand::rngs::OsRng;
use rand::RngCore;
use sha2::Sha256;
use subtle::ConstantTimeEq;

type HmacSha256 = Hmac<Sha256>;

/// Digits in a PIN. Six is what a person will retype without resenting it; the
/// security argument does not rest on its length alone but on the guess being
/// **online and counted** (see [`MAX_ATTEMPTS`]).
pub const PIN_DIGITS: usize = 6;

/// How many wrong PINs a pairing accepts before it is abandoned.
///
/// Three guesses out of a million is a 1-in-333,000 chance for an attacker, and
/// a wrong PIN is visible to the person holding the device. Without a limit the
/// online guess becomes an offline one by repetition, which is exactly what the
/// design refuses to allow.
pub const MAX_ATTEMPTS: u8 = 3;

/// A freshly generated pairing PIN.
///
/// Not `Copy`, not `Display` beyond [`Self::as_str`], and never logged: it is a
/// secret for the seconds it is alive.
#[derive(Clone)]
pub struct Pin(String);

impl Pin {
    /// Generate a uniform random PIN from the OS CSPRNG.
    ///
    /// **Uniform, not `rand % 1_000_000`.** Rejection sampling costs nothing
    /// here and modulo bias on a small range is exactly the kind of quiet
    /// weakness that makes a guess cheaper than the arithmetic suggests.
    #[must_use]
    pub fn generate() -> Pin {
        let mut rng = OsRng;
        loop {
            if let Some(v) = reduce(rng.next_u32()) {
                return Pin(format!("{v:0PIN_DIGITS$}"));
            }
        }
    }

    /// Read a PIN a person typed, or `None` if it is not one.
    ///
    /// Spaces and dashes are accepted because people type codes the way they
    /// are displayed. Anything else is rejected rather than repaired: silently
    /// "fixing" a mistyped PIN into a valid one is how a person ends up
    /// confirming a pairing they did not check.
    #[must_use]
    pub fn parse(input: &str) -> Option<Pin> {
        let cleaned: String = input
            .chars()
            .filter(|c| !c.is_whitespace() && *c != '-')
            .collect();
        (cleaned.len() == PIN_DIGITS && cleaned.chars().all(|c| c.is_ascii_digit()))
            .then_some(Pin(cleaned))
    }

    /// The digits, for showing to the person who must read them out.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Grouped for reading aloud: `123 456`.
    #[must_use]
    pub fn display(&self) -> String {
        let (a, b) = self.0.split_at(PIN_DIGITS / 2);
        format!("{a} {b}")
    }
}

/// How many PINs there are.
const RANGE: u32 = 1_000_000;

/// The largest draw that may be reduced without skewing the result: the last
/// value of the final whole `RANGE`-sized block in `u32`.
const LIMIT: u32 = u32::MAX - (u32::MAX % RANGE) - 1;

/// Reduce one random draw to a PIN value, or reject it.
///
/// **Rejection, not `% RANGE` alone.** `u32::MAX + 1` is not a multiple of a
/// million, so the last partial block makes the lowest values fractionally more
/// likely — a small bias, but one that costs nothing to remove and is exactly
/// the kind of quiet weakness that makes guessing cheaper than the arithmetic
/// suggests. Split out from [`Pin::generate`] so the boundary is testable
/// without needing statistics to see it.
#[must_use]
fn reduce(draw: u32) -> Option<u32> {
    (draw <= LIMIT).then_some(draw % RANGE)
}

impl std::fmt::Debug for Pin {
    /// Redacted. A PIN that reaches a log or a crash report is a PIN that was
    /// never out-of-band.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Pin(******)")
    }
}

/// Prove knowledge of `pin` over this handshake's `transcript`.
///
/// The transcript binds both public keys, both nonces and both presented
/// identities, so a proof is worthless against any other handshake — including
/// the second leg of a machine-in-the-middle, which is the whole point.
#[must_use]
pub fn prove(pin: &Pin, transcript: &[u8]) -> Vec<u8> {
    let mut mac =
        HmacSha256::new_from_slice(pin.0.as_bytes()).expect("HMAC accepts any key length");
    mac.update(b"peerbeam-pin-pairing-v1");
    mac.update(transcript);
    mac.finalize().into_bytes().to_vec()
}

/// Check a peer's proof.
///
/// **Constant time.** A byte-by-byte comparison that returns early leaks how
/// much of the proof was right, and a leak like that turns one online guess
/// into a series of cheaper ones.
#[must_use]
pub fn verify(pin: &Pin, transcript: &[u8], proof: &[u8]) -> bool {
    let expected = prove(pin, transcript);
    expected.ct_eq(proof).into()
}

/// A pairing in progress on the side that generated the PIN.
///
/// Owns the attempt count, so the limit cannot be forgotten by a caller that
/// only remembers to call [`verify`].
pub struct Pairing {
    pin: Pin,
    attempts_left: u8,
}

/// What a pairing attempt produced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Attempt {
    /// The peer proved it knows the PIN. Trust may be pinned.
    Verified,
    /// Wrong PIN; this many guesses remain.
    Wrong { attempts_left: u8 },
    /// Out of guesses. **The pairing is dead** — a caller must start over with
    /// a fresh PIN rather than keep asking.
    Exhausted,
}

impl Pairing {
    /// Begin a pairing with a fresh random PIN.
    #[must_use]
    pub fn begin() -> Pairing {
        Pairing {
            pin: Pin::generate(),
            attempts_left: MAX_ATTEMPTS,
        }
    }

    /// The PIN to show the person, so they can read it to the other device.
    #[must_use]
    pub fn pin(&self) -> &Pin {
        &self.pin
    }

    /// Check one proof, consuming an attempt if it is wrong.
    ///
    /// A correct proof does **not** consume an attempt, and an exhausted
    /// pairing never verifies again even if handed the right proof — once the
    /// budget is spent the only safe answer is to start over.
    pub fn attempt(&mut self, transcript: &[u8], proof: &[u8]) -> Attempt {
        if self.attempts_left == 0 {
            return Attempt::Exhausted;
        }
        if verify(&self.pin, transcript, proof) {
            return Attempt::Verified;
        }
        self.attempts_left -= 1;
        if self.attempts_left == 0 {
            Attempt::Exhausted
        } else {
            Attempt::Wrong {
                attempts_left: self.attempts_left,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TRANSCRIPT: &[u8] = b"pubkeys-nonces-and-identities";

    /// The bias is invisible to any honest test of the *output* — it is about
    /// one part in four thousand. Testing the **decision** instead makes it
    /// plain: draws past the last whole block are rejected rather than folded
    /// onto the low values.
    #[test]
    fn draws_past_the_last_whole_block_are_rejected_not_folded() {
        assert_eq!(reduce(0), Some(0));
        assert_eq!(reduce(RANGE - 1), Some(RANGE - 1));
        assert_eq!(reduce(RANGE), Some(0), "a whole block wraps normally");
        assert_eq!(reduce(LIMIT), Some(RANGE - 1), "the last usable draw");
        assert_eq!(reduce(LIMIT + 1), None, "the partial block must be refused");
        assert_eq!(reduce(u32::MAX), None);
    }

    #[test]
    fn every_accepted_draw_is_inside_the_pin_range() {
        for draw in [0, 1, RANGE, LIMIT, LIMIT / 2, 7_777_777] {
            if let Some(v) = reduce(draw) {
                assert!(v < RANGE, "{draw} reduced to {v}, outside the range");
            }
        }
    }

    #[test]
    fn a_generated_pin_is_six_digits() {
        for _ in 0..200 {
            let p = Pin::generate();
            assert_eq!(p.as_str().len(), PIN_DIGITS);
            assert!(p.as_str().chars().all(|c| c.is_ascii_digit()));
        }
    }

    #[test]
    fn generated_pins_keep_their_leading_zeros() {
        // `000123` is a valid PIN and must not be shown or compared as `123`;
        // formatting it as a bare number would make a sixth of the space
        // unreachable and the typed value fail to match.
        let p = Pin("000123".to_string());
        assert_eq!(p.as_str(), "000123");
        assert_eq!(p.display(), "000 123");
    }

    #[test]
    fn a_correct_proof_verifies_and_a_wrong_pin_does_not() {
        let pin = Pin("123456".to_string());
        let proof = prove(&pin, TRANSCRIPT);
        assert!(verify(&pin, TRANSCRIPT, &proof));
        assert!(!verify(&Pin("123457".to_string()), TRANSCRIPT, &proof));
    }

    /// **The machine-in-the-middle case.** An attacker relaying between two
    /// devices runs two different handshakes, so a proof captured from one leg
    /// is worthless on the other — which is why the PIN signs the transcript
    /// and not merely itself.
    #[test]
    fn a_proof_from_one_handshake_does_not_verify_against_another() {
        let pin = Pin("123456".to_string());
        let leg_one = prove(&pin, b"transcript-attacker-to-alice");
        assert!(!verify(&pin, b"transcript-attacker-to-bob", &leg_one));
    }

    #[test]
    fn a_truncated_or_padded_proof_is_rejected() {
        let pin = Pin("123456".to_string());
        let proof = prove(&pin, TRANSCRIPT);
        assert!(!verify(&pin, TRANSCRIPT, &proof[..proof.len() - 1]));
        let mut longer = proof.clone();
        longer.push(0);
        assert!(!verify(&pin, TRANSCRIPT, &longer));
        assert!(!verify(&pin, TRANSCRIPT, &[]));
    }

    #[test]
    fn a_typed_pin_may_carry_the_spaces_it_was_shown_with() {
        assert_eq!(Pin::parse("123 456").unwrap().as_str(), "123456");
        assert_eq!(Pin::parse("123-456").unwrap().as_str(), "123456");
        assert_eq!(Pin::parse("  123456 ").unwrap().as_str(), "123456");
    }

    #[test]
    fn a_malformed_pin_is_rejected_rather_than_repaired() {
        // Quietly turning `12345` into something valid is how a person confirms
        // a pairing they never actually checked.
        assert!(Pin::parse("12345").is_none());
        assert!(Pin::parse("1234567").is_none());
        assert!(Pin::parse("12345a").is_none());
        assert!(Pin::parse("").is_none());
    }

    #[test]
    fn a_pin_never_prints_itself() {
        // A PIN in a log or a crash report was never out-of-band.
        let p = Pin("123456".to_string());
        assert_eq!(format!("{p:?}"), "Pin(******)");
        assert!(!format!("{p:?}").contains("123456"));
    }

    /// **The limit is what keeps the guess online.** Without it, a million
    /// tries against a six-digit secret is patience, not an attack.
    #[test]
    fn wrong_guesses_run_out_and_the_pairing_stays_dead() {
        let mut pairing = Pairing::begin();
        let right = prove(pairing.pin(), TRANSCRIPT);
        // A proof under a PIN that is definitely not the generated one.
        let other = if pairing.pin().as_str() == "000000" {
            Pin("111111".to_string())
        } else {
            Pin("000000".to_string())
        };
        let wrong = prove(&other, TRANSCRIPT);

        assert_eq!(
            pairing.attempt(TRANSCRIPT, &wrong),
            Attempt::Wrong { attempts_left: 2 }
        );
        assert_eq!(
            pairing.attempt(TRANSCRIPT, &wrong),
            Attempt::Wrong { attempts_left: 1 }
        );
        assert_eq!(pairing.attempt(TRANSCRIPT, &wrong), Attempt::Exhausted);
        assert_eq!(
            pairing.attempt(TRANSCRIPT, &right),
            Attempt::Exhausted,
            "an exhausted pairing accepted the right PIN — the budget must be \
             final, or it is not a budget"
        );
    }

    #[test]
    fn a_correct_proof_does_not_spend_an_attempt() {
        let mut pairing = Pairing::begin();
        let right = prove(pairing.pin(), TRANSCRIPT);
        assert_eq!(pairing.attempt(TRANSCRIPT, &right), Attempt::Verified);
        assert_eq!(pairing.attempt(TRANSCRIPT, &right), Attempt::Verified);
    }

    /// Not a proof of uniformity — that needs statistics this suite has no
    /// business running — but it does catch a generator stuck on one value or
    /// obviously confined to a fraction of the range.
    #[test]
    fn generated_pins_are_not_all_the_same_and_span_the_range() {
        let pins: Vec<String> = (0..500).map(|_| Pin::generate().0).collect();
        let distinct: std::collections::HashSet<&String> = pins.iter().collect();
        assert!(distinct.len() > 400, "only {} distinct", distinct.len());
        assert!(
            pins.iter().any(|p| p.starts_with('0')),
            "no PIN began with 0 — leading digits look constrained"
        );
        assert!(pins.iter().any(|p| p.starts_with('9')));
    }
}
