//! Proving that a downloaded release is the one this project built.
//!
//! # Why this exists
//!
//! Every release publishes `SHA256SUMS` over exactly the files it uploads, and
//! that file has always carried an honest disclaimer in the workflow that
//! writes it: *"This is integrity, not authenticity … the file sits beside the
//! artifacts it describes, so whoever could replace one could replace both."*
//! A checksum fetched from the same origin as the thing it describes proves
//! only that the bytes arrived intact, which TLS already proved.
//!
//! Amendment **A3** in `docs/ARCHITECTURAL_INVARIANTS.md` permits PeerBeam to
//! download a release the user asked for, and its third binding condition is
//! what this module implements:
//!
//! > Integrity is verified against a project signature before the file is
//! > usable. … A missing signature, a bad signature, an absent entry, or a
//! > digest mismatch deletes the downloaded bytes and says what happened.
//! > **There is no "unverified but probably fine" path, no override, and no
//! > prompt offering one** — an integrity check a user can click past is
//! > decoration.
//!
//! So every function here fails closed. There is no variant of any of them that
//! returns "probably fine", and [`VerifyError`] has no variant a caller could
//! reasonably choose to ignore.
//!
//! # Why verification only
//!
//! `minisign-verify` is the checking half of minisign: Ed25519 verification
//! with no key generation and no signing code in it. A shipped PeerBeam
//! therefore cannot produce a signature even in principle, which is the
//! property worth having — the secret key never goes near this binary, and no
//! amount of compromising a client yields the ability to sign a release.
//!
//! The release side signs with the ordinary `minisign` CLI, which is packaged
//! for every platform and auditable by anyone, rather than with bespoke code
//! in this repository.
//!
//! # The chain
//!
//! Three links, and all three must hold:
//!
//! 1. The signature over `SHA256SUMS` verifies under [`SIGNING_PUBLIC_KEY`],
//!    which is compiled into this binary and therefore pinned for its life.
//! 2. `SHA256SUMS` lists the file that was downloaded, by the basename the
//!    release published it under.
//! 3. The bytes on disk hash to the digest that file lists.
//!
//! Link 1 is the one that makes the other two mean anything. Without it an
//! attacker who can serve the artifact can serve a matching checksum.

use sha2::{Digest, Sha256};

/// The public half of the key that signs `SHA256SUMS`, base64, exactly as the
/// second line of a minisign `.pub` file.
///
/// **Empty until the project generates its signing key**, and that is
/// deliberate rather than a placeholder waiting to be forgotten: with no key
/// compiled in, [`verify_sums`] returns [`VerifyError::NoKeyCompiledIn`] and
/// every download refuses. The feature fails closed by construction, so a build
/// that ships before the key exists cannot silently accept unverified bytes —
/// it simply declines to download anything.
///
/// # Setting it
///
/// Generate the keypair once, keep the secret half somewhere with a custody
/// story, and paste the public half here. `docs/RELEASE.md` has the procedure.
/// The value belongs in version control: it is public, and committing it is
/// what pins it — a key read from configuration at runtime could be replaced by
/// whoever can write that configuration, which defeats the purpose.
///
/// # Rotation
///
/// Because this is compiled in, every build carries the key it shipped with.
/// Replacing the key does not reach builds already installed: they go on
/// trusting the old one and will refuse releases signed with the new one, which
/// presents to the user as "this download could not be verified" and not as
/// anything worse. That is the safe direction, and it is why rotation needs an
/// answer written down before the first key ships rather than during an
/// incident.
pub const SIGNING_PUBLIC_KEY: &str = "";

/// Why a download was refused.
///
/// Every variant means *do not use these bytes*. None of them is advisory, and
/// there is deliberately no `Unverified` or `Skipped` variant — A3 condition 3
/// forbids a path that proceeds without proof, so there is no way to express
/// one here.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum VerifyError {
    /// This build shipped without a signing key, so nothing can be proved.
    ///
    /// Not an error in the artifact: an error in the build asking the question.
    #[error("this build has no release signing key compiled in, so a download cannot be verified")]
    NoKeyCompiledIn,

    /// [`SIGNING_PUBLIC_KEY`] is set but is not a usable minisign public key.
    #[error("the compiled-in signing key is not a valid minisign public key: {0}")]
    MalformedKey(String),

    /// The `.minisig` alongside the checksums could not be parsed.
    #[error("the signature file could not be read: {0}")]
    MalformedSignature(String),

    /// It parsed, and it is not a signature this key made over these checksums.
    ///
    /// Covers a signature by the wrong key, a signature over different content,
    /// and a signature that is simply forged.
    #[error("the checksums are not signed by this project's key: {0}")]
    BadSignature(String),

    /// The checksum list does not mention the file that was downloaded.
    #[error("{0} is not listed in the signed checksums")]
    NotListed(String),

    /// The bytes on disk are not the bytes the signed list describes.
    #[error("{name} does not match its signed checksum (expected {expected}, got {actual})")]
    DigestMismatch {
        /// The file's published basename.
        name: String,
        /// What the signed list says it should hash to.
        expected: String,
        /// What it actually hashed to.
        actual: String,
    },
}

/// The SHA-256 of some bytes, lowercase hex.
///
/// Written out rather than pulling in a hex crate for sixteen characters of
/// lookup table (CLAUDE.md: "Avoid unnecessary dependencies").
#[must_use]
pub fn digest_of(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(digest.len() * 2);
    for b in digest {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
    }
    out
}

/// The digest `sums` records for `name`, if it records one.
///
/// Pure, and separate from both the fetching and the signature check, so the
/// file's shape is a rule testable without a network — the same reason
/// [`crate::newest`] is separate from [`crate::check`].
///
/// # The format
///
/// `sha256sum`'s own, which is what `release.yml` writes: a 64-character
/// lowercase hex digest, two spaces, then the file's **basename**. The workflow
/// deliberately lists basenames so that `sha256sum -c SHA256SUMS` works in
/// whatever directory a person downloaded into, and this matches on the same
/// terms.
///
/// A line whose name matches but whose digest is not 64 hex characters is
/// treated as no match rather than as a match to be checked later: a malformed
/// entry cannot be satisfied by any bytes, and letting it through to the
/// comparison would report a digest mismatch, which says the artifact is wrong
/// when the checksum file is.
#[must_use]
pub fn digest_for(sums: &str, name: &str) -> Option<String> {
    for line in sums.lines() {
        let line = line.trim();
        // `split_once` on the two-space separator would break on `sha256sum
        // --binary` output, which uses " *" instead. Splitting on whitespace
        // and taking the ends handles both, and a name with spaces in it is
        // not something this project publishes.
        let mut parts = line.split_whitespace();
        let (Some(digest), Some(file)) = (parts.next(), parts.next()) else {
            continue;
        };
        let file = file.trim_start_matches('*');
        if file != name {
            continue;
        }
        if digest.len() != 64 || !digest.bytes().all(|c| c.is_ascii_hexdigit()) {
            return None;
        }
        return Some(digest.to_ascii_lowercase());
    }
    None
}

/// Check that `signature` is this project's signature over `sums`.
///
/// `allow_legacy` is **false** at the call below, which requires a prehashed
/// signature — the form modern `minisign -S` produces. Accepting the legacy
/// form would accept signatures made by tooling old enough to predate it, for
/// no benefit to a key this project has not generated yet.
pub fn verify_sums(sums: &[u8], signature: &str) -> Result<(), VerifyError> {
    verify_sums_with(SIGNING_PUBLIC_KEY, sums, signature)
}

/// The same check against a caller-supplied key.
///
/// **Private on purpose.** A public function taking a key would be an override
/// path — somewhere a caller could pass a key of their own and get a pass — and
/// A3 condition 3 forbids exactly that. It exists because the tests otherwise
/// cannot reach the Ed25519 check at all: with [`SIGNING_PUBLIC_KEY`] empty,
/// every call short-circuits on [`VerifyError::NoKeyCompiledIn`] and the code
/// that does the actual verifying would ship having never run.
fn verify_sums_with(public_key_b64: &str, sums: &[u8], signature: &str) -> Result<(), VerifyError> {
    let public_key_b64 = public_key_b64.trim();
    if public_key_b64.is_empty() {
        return Err(VerifyError::NoKeyCompiledIn);
    }
    let key = minisign_verify::PublicKey::from_base64(public_key_b64)
        .map_err(|e| VerifyError::MalformedKey(e.to_string()))?;
    let sig = minisign_verify::Signature::decode(signature)
        .map_err(|e| VerifyError::MalformedSignature(e.to_string()))?;
    key.verify(sums, &sig, false)
        .map_err(|e| VerifyError::BadSignature(e.to_string()))
}

/// The whole chain, for one downloaded file.
///
/// `sums` and `signature` are the release's `SHA256SUMS` and
/// `SHA256SUMS.minisig`; `name` is the basename the release published `bytes`
/// under.
///
/// Ordered signature-first on purpose. Checking the digest before the signature
/// would let an unsigned checksum file decide whether the artifact "matches",
/// and a caller reading only the first failure would learn that the bytes were
/// fine when nothing had established that the list was.
pub fn verify_artifact(
    sums: &[u8],
    signature: &str,
    name: &str,
    bytes: &[u8],
) -> Result<(), VerifyError> {
    verify_artifact_with(SIGNING_PUBLIC_KEY, sums, signature, name, bytes)
}

/// [`verify_artifact`] against a caller-supplied key. Private, for the reason
/// given on [`verify_sums_with`].
fn verify_artifact_with(
    public_key_b64: &str,
    sums: &[u8],
    signature: &str,
    name: &str,
    bytes: &[u8],
) -> Result<(), VerifyError> {
    verify_sums_with(public_key_b64, sums, signature)?;
    let text = std::str::from_utf8(sums)
        .map_err(|e| VerifyError::MalformedSignature(format!("checksums are not UTF-8: {e}")))?;
    let expected =
        digest_for(text, name).ok_or_else(|| VerifyError::NotListed(name.to_string()))?;
    let actual = digest_of(bytes);
    if actual != expected {
        return Err(VerifyError::DigestMismatch {
            name: name.to_string(),
            expected,
            actual,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `SHA256SUMS` in exactly the shape `release.yml` writes: digest, two
    /// spaces, basename, sorted by name.
    const SUMS: &str = "\
3b2d1c4e5f60718293a4b5c6d7e8f90a1b2c3d4e5f60718293a4b5c6d7e8f90a  PeerBeam-0.12.0.dmg
0000000000000000000000000000000000000000000000000000000000000000  peerbeam-0.12.0-amd64.deb
";

    #[test]
    fn a_listed_file_yields_its_digest() {
        assert_eq!(
            digest_for(SUMS, "peerbeam-0.12.0-amd64.deb").as_deref(),
            Some("0000000000000000000000000000000000000000000000000000000000000000")
        );
    }

    #[test]
    fn an_unlisted_file_yields_nothing() {
        assert_eq!(digest_for(SUMS, "peerbeam-0.12.0-arm64.deb"), None);
    }

    /// A name that is a prefix of a listed one is not that one. `.deb` and
    /// `.deb.sig` differing by a suffix is exactly how this would go wrong.
    #[test]
    fn matching_is_on_the_whole_name() {
        assert_eq!(digest_for(SUMS, "peerbeam-0.12.0-amd64"), None);
        assert_eq!(digest_for(SUMS, "PeerBeam-0.12.0.dm"), None);
    }

    /// `sha256sum --binary` writes " *name". Both forms must resolve.
    #[test]
    fn binary_mode_entries_resolve_too() {
        let sums = "0000000000000000000000000000000000000000000000000000000000000000 *peerbeam-0.12.0-amd64.deb\n";
        assert!(digest_for(sums, "peerbeam-0.12.0-amd64.deb").is_some());
    }

    /// A truncated digest cannot be satisfied by any bytes, so it is no match
    /// rather than a mismatch waiting to be reported against the artifact.
    #[test]
    fn a_malformed_digest_is_not_a_match() {
        let sums = "abc123  peerbeam-0.12.0-amd64.deb\n";
        assert_eq!(digest_for(sums, "peerbeam-0.12.0-amd64.deb"), None);
    }

    #[test]
    fn a_non_hex_digest_is_not_a_match() {
        let sums = "zzzz1c4e5f60718293a4b5c6d7e8f90a1b2c3d4e5f60718293a4b5c6d7e8f90a  x.deb\n";
        assert_eq!(digest_for(sums, "x.deb"), None);
    }

    #[test]
    fn the_empty_input_hashes_to_the_known_sha256() {
        assert_eq!(
            digest_of(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn digests_are_lowercase_hex_of_the_right_length() {
        let d = digest_of(b"peerbeam");
        assert_eq!(d.len(), 64);
        assert!(d
            .bytes()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
    }

    /// The property the whole module exists for: with no key compiled in,
    /// nothing verifies. A build that shipped before the project generated its
    /// signing key refuses every download rather than accepting any.
    #[test]
    fn without_a_compiled_in_key_nothing_verifies() {
        // Guards the shipped default. If someone sets the key, this test is
        // the one that tells them to revisit the rest of these.
        if !SIGNING_PUBLIC_KEY.trim().is_empty() {
            return;
        }
        assert_eq!(
            verify_sums(b"anything", "untrusted comment: x\nRWQ\n"),
            Err(VerifyError::NoKeyCompiledIn)
        );
        assert_eq!(
            verify_artifact(SUMS.as_bytes(), "sig", "peerbeam-0.12.0-amd64.deb", b""),
            Err(VerifyError::NoKeyCompiledIn)
        );
    }

    /// Signature first: a file that is not even listed must still fail on the
    /// signature, never on its absence from an unverified list.
    #[test]
    fn the_signature_is_checked_before_anything_else() {
        let err =
            verify_artifact(SUMS.as_bytes(), "nonsense", "not-a-release-file", b"x").unwrap_err();
        assert!(
            matches!(
                err,
                VerifyError::NoKeyCompiledIn | VerifyError::MalformedSignature(_)
            ),
            "expected a signature failure, got {err:?}"
        );
    }

    // ---------------------------------------------------------------------
    // The real Ed25519 path.
    //
    // These vectors come from a **throwaway** keypair generated once for this
    // file and then discarded; it is not, and must never become, the project's
    // signing key. Their job is to make the verification code actually run:
    // with `SIGNING_PUBLIC_KEY` empty, every other test here stops at
    // `NoKeyCompiledIn` and the Ed25519 check would ship unexecuted.
    //
    // Static rather than generated at test time on purpose. Signing needs a
    // signing library, and the point of `minisign-verify` is that no such
    // library is anywhere in PeerBeam's dependency tree — not even under
    // `[dev-dependencies]`, where it would still be a crate this project
    // compiles and audits.
    // ---------------------------------------------------------------------

    /// Throwaway test key. Not the project's.
    const TEST_PUB: &str = "RWRV8YX8Eh5Nx++uQIDqGoKBfGq7FFYFvSxTmF5qIgBTSgJKWyA9VNE7";

    /// A real minisign signature over exactly [`SUMS`], by [`TEST_PUB`]'s
    /// secret half. `RUR...` is the prehashed algorithm, which is what
    /// `verify(.., false)` requires.
    const TEST_SIG: &str = "untrusted comment: signature from rsign secret key\nRURV8YX8Eh5Nxw0GBdjXuIDPzN2cHu6zbGM/8b1zDskpgvmRL9oWaKo731qmb3fJdU0iXGv2ferbfDHLzjt9QL9waIMLCJWYpgI=\ntrusted comment: timestamp:1790496116\nEY9878kaWJPh61rlfIl0LBn4kQXnMlfjpFAgmChOtY5vqsfbkuY2W6dLv1/YgiJuhFNI8HwZonNp7Wqey4hEDQ==\n";

    #[test]
    fn a_real_signature_over_the_real_checksums_verifies() {
        verify_sums_with(TEST_PUB, SUMS.as_bytes(), TEST_SIG)
            .expect("the vector must verify, or none of the other cases mean anything");
    }

    /// The property that matters: change one byte of the signed content and
    /// the signature no longer holds.
    #[test]
    fn a_tampered_checksum_list_fails() {
        let tampered = SUMS.replace(
            "0000000000000000000000000000000000000000000000000000000000000000",
            "1111111111111111111111111111111111111111111111111111111111111111",
        );
        assert!(matches!(
            verify_sums_with(TEST_PUB, tampered.as_bytes(), TEST_SIG),
            Err(VerifyError::BadSignature(_))
        ));
    }

    /// A valid signature by a key that is not ours is not a pass. This is the
    /// attack the whole module exists to stop: anyone can make a signature,
    /// the question is whose key made it.
    #[test]
    fn a_signature_by_another_key_fails() {
        // TEST_PUB with its key id and point altered -- a well-formed, valid
        // minisign public key that did not make TEST_SIG.
        let other = "RWSV8YX8Eh5Nx++uQIDqGoKBfGq7FFYFvSxTmF5qIgBTSgJKWyA9VNE7";
        assert!(matches!(
            verify_sums_with(other, SUMS.as_bytes(), TEST_SIG),
            Err(VerifyError::BadSignature(_) | VerifyError::MalformedKey(_))
        ));
    }

    #[test]
    fn a_garbage_signature_is_refused_as_malformed() {
        assert!(matches!(
            verify_sums_with(TEST_PUB, SUMS.as_bytes(), "not a signature at all"),
            Err(VerifyError::MalformedSignature(_))
        ));
    }

    #[test]
    fn a_garbage_key_is_refused_as_malformed() {
        assert!(matches!(
            verify_sums_with("not-a-key", SUMS.as_bytes(), TEST_SIG),
            Err(VerifyError::MalformedKey(_))
        ));
    }

    // A second throwaway vector, whose signed list holds the **real** digest of
    // [`SATISFIABLE_BYTES`]. The first vector's digests are placeholders that
    // no bytes produce, so it can only ever demonstrate failures; this one is
    // what proves a good download is actually accepted. A verifier tested only
    // on rejections would pass every test while refusing everything.

    /// Throwaway test key. Not the project's, and not the one above.
    const TEST_PUB_2: &str = "RWTGv4T53Kto3Y1jPhk8GDzBQBhaG3XffVBtJZiSLEmOOm1JSGhExa8c";

    /// Signed list naming the true SHA-256 of [`SATISFIABLE_BYTES`].
    const SUMS_2: &str =
        "79d18c4052db9ba10b3f01f7c13f1bd8e769a3012f6ea556982c49f514bf0484  peerbeam-0.12.0-amd64.deb\n";

    /// A real signature over [`SUMS_2`] by [`TEST_PUB_2`]'s secret half.
    const TEST_SIG_2: &str = "untrusted comment: signature from rsign secret key\nRUTGv4T53Kto3fuhYc50As59OOdDUYiqfgwgV7dN4o/4LS2+bCduOF4EeeLMjjWDbJC7kQ8oNPzWhfEimpy4KCH+qU5dQlCDwAM=\ntrusted comment: timestamp:1790496196\ntnltuiUURNNBYEdAo2ooLDhqwoUCmU4JKpuG1QlSiVkYsoiTqYE+v1xm7WGSyT6iQS8ZTufP3g2Ifmv4mBVODg==\n";

    /// The bytes [`SUMS_2`] vouches for.
    const SATISFIABLE_BYTES: &[u8] = b"pretend this is a .deb";

    /// The positive case, end to end: a signature this key made, over a list
    /// naming this file, whose digest these bytes actually have.
    #[test]
    fn the_full_chain_passes_for_bytes_that_match_a_signed_digest() {
        verify_artifact_with(
            TEST_PUB_2,
            SUMS_2.as_bytes(),
            TEST_SIG_2,
            "peerbeam-0.12.0-amd64.deb",
            SATISFIABLE_BYTES,
        )
        .expect("a correctly signed, correctly listed, correctly hashed artifact must pass");
    }

    /// And one byte's difference in the artifact breaks it, with the signature
    /// and the listing both still intact.
    #[test]
    fn one_changed_byte_in_the_artifact_breaks_the_chain() {
        let mut tampered = SATISFIABLE_BYTES.to_vec();
        *tampered.last_mut().unwrap() ^= 0x01;
        assert!(matches!(
            verify_artifact_with(
                TEST_PUB_2,
                SUMS_2.as_bytes(),
                TEST_SIG_2,
                "peerbeam-0.12.0-amd64.deb",
                &tampered,
            ),
            Err(VerifyError::DigestMismatch { .. })
        ));
    }

    /// Bytes that are not what the signed list describes are refused, and the
    /// error names both digests so the person can see which is which.
    #[test]
    fn the_full_chain_rejects_bytes_that_do_not_match() {
        let err = verify_artifact_with(
            TEST_PUB,
            SUMS.as_bytes(),
            TEST_SIG,
            "peerbeam-0.12.0-amd64.deb",
            b"these are not the bytes",
        )
        .unwrap_err();
        match err {
            VerifyError::DigestMismatch {
                name,
                expected,
                actual,
            } => {
                assert_eq!(name, "peerbeam-0.12.0-amd64.deb");
                assert_eq!(expected, "0".repeat(64));
                assert_eq!(actual, digest_of(b"these are not the bytes"));
            }
            other => panic!("expected a digest mismatch, got {other:?}"),
        }
    }

    /// A correctly-signed list that simply does not mention the file.
    #[test]
    fn the_full_chain_rejects_a_file_the_signed_list_omits() {
        assert_eq!(
            verify_artifact_with(
                TEST_PUB,
                SUMS.as_bytes(),
                TEST_SIG,
                "peerbeam-0.12.0-arm64.deb",
                b"anything",
            ),
            Err(VerifyError::NotListed("peerbeam-0.12.0-arm64.deb".into()))
        );
    }

    /// There must be no way to express "unverified but proceed".
    #[test]
    fn no_error_variant_means_proceed_anyway() {
        for e in [
            VerifyError::NoKeyCompiledIn,
            VerifyError::MalformedKey("x".into()),
            VerifyError::MalformedSignature("x".into()),
            VerifyError::BadSignature("x".into()),
            VerifyError::NotListed("x".into()),
            VerifyError::DigestMismatch {
                name: "x".into(),
                expected: "a".into(),
                actual: "b".into(),
            },
        ] {
            // Every variant renders as a refusal a person can act on, and none
            // of them is empty or reads as advisory.
            let msg = e.to_string();
            assert!(!msg.is_empty());
            assert!(
                !msg.to_ascii_lowercase().contains("warning"),
                "{msg} reads as advisory"
            );
        }
    }
}
