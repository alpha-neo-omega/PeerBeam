#!/usr/bin/env bash
# Sign a release's SHA256SUMS with the project's minisign key.
#
# Usage:
#   scripts/sign-release.sh <path-to-SHA256SUMS> [tag]
#
# Writes <path>.minisig beside it, and prints the path on stdout so a caller
# can attach it. Prints nothing and exits 0 when no key is available. Exits 1
# when a key IS available and cannot be used — not a whole key file, not
# loadable, no minisign to load it with, or a signature that does not verify
# against MINISIGN_PUBLIC_KEY. release.yml runs this before it deletes
# anything, so that stops the release rather than shipping it unsigned.
#
# ---------------------------------------------------------------------------
# Why this exists
#
# `SHA256SUMS` proves a download is not corrupt. It does not prove the bytes
# came from this project: it sits beside the artifacts it describes, so whoever
# could replace one could replace both. Amendment A3 in
# docs/ARCHITECTURAL_INVARIANTS.md permits PeerBeam to download a release the
# user asked for, and requires the artifact to be checked against a checksum
# list this project *signed*. This is what signs it.
#
# ---------------------------------------------------------------------------
# Where the key comes from, in order
#
#   MINISIGN_SECRET_KEY       the key file's contents (CI: a repository secret)
#   MINISIGN_SECRET_KEY_FILE  a path to the key file (local signing)
#   ~/.minisign/minisign.key  minisign's own default (local signing)
#
# ---------------------------------------------------------------------------
# No key is not an error
#
# Releases shipped unsigned for this project's whole life, and refusing to
# publish one would make a missing secret an outage. So this warns and exits 0,
# exactly as scripts/package-macos.sh does without PB_SIGN_ID.
#
# Nothing is weakened by that, because the fail-closed half lives in the
# client: peerbeam-update refuses any download it cannot verify, so an unsigned
# release is simply one the app will not fetch. A person can still download it
# from the website by hand, which is the situation today.
set -euo pipefail

sums="${1:?usage: sign-release.sh <path-to-SHA256SUMS> [tag]}"
tag="${2:-${GITHUB_REF_NAME:-unknown}}"

if [ ! -f "$sums" ]; then
  echo "::error::sign-release: $sums does not exist" >&2
  exit 1
fi

# Resolve the key, and remember whether we made a temporary file so the trap
# only ever deletes our own.
keyfile=""
tmpkey=""
# An `if` rather than `[ -n "$tmpkey" ] && rm -f "$tmpkey"`. The trap is the
# last thing to run, so its status becomes the script's: with no temporary key
# the `&&` form evaluates false, returns 1, and turns every `exit 0` into an
# exit 1. That is the path taken on a release with no signing key — which is
# every release today — so it would have failed the publish step outright.
cleanup() {
  if [ -n "$tmpkey" ]; then
    rm -f "$tmpkey"
  fi
}
trap cleanup EXIT

if [ -n "${MINISIGN_SECRET_KEY:-}" ]; then
  # `mktemp` then `chmod` rather than writing into a predictable path: on a
  # shared runner /tmp is everyone's, and the key must never exist
  # world-readable even for the instant between creation and chmod — mktemp
  # creates at 0600 already, and the chmod is belt and braces.
  tmpkey="$(mktemp)"
  chmod 600 "$tmpkey"
  printf '%s\n' "$MINISIGN_SECRET_KEY" > "$tmpkey"
  keyfile="$tmpkey"
elif [ -n "${MINISIGN_SECRET_KEY_FILE:-}" ]; then
  keyfile="$MINISIGN_SECRET_KEY_FILE"
elif [ -f "$HOME/.minisign/minisign.key" ]; then
  keyfile="$HOME/.minisign/minisign.key"
else
  echo "::warning::sign-release: no minisign key (set MINISIGN_SECRET_KEY) — publishing without a signature." >&2
  exit 0
fi

# Only once there is a key does a missing minisign mean anything, and then it
# is an error. This used to be checked first, as a warning and exit 0, which
# made "no key" and "a key, but nothing to sign with" the same outcome: a
# release that lost its install step would have published unsigned with the
# key sitting right there.
if ! command -v minisign >/dev/null 2>&1; then
  echo "::error::sign-release: a minisign key is configured but minisign is not installed." >&2
  exit 1
fi

# A minisign secret key file is two lines: an "untrusted comment:" header and
# the base64 key. Checked here because the likely way this goes wrong is a
# secret pasted without its comment line — which is exactly what cost v0.12.1
# its signature — and minisign's own answer to that, "Error while loading the
# secret key file", is true but not enough to act on in a CI log.
if ! head -1 "$keyfile" | grep -q '^untrusted comment:' || [ -z "$(sed -n '2p' "$keyfile")" ]; then
  echo "::error::sign-release: that does not look like a minisign secret key file." >&2
  echo "::error::It must be the COMPLETE file: two lines, the first starting" >&2
  echo "::error::'untrusted comment:'. Not just the base64 line on its own." >&2
  echo "::error::Set it from the file itself: gh secret set MINISIGN_SECRET_KEY < minisign.key" >&2
  exit 1
fi

# The trusted comment is signed, unlike the untrusted one, so it is the right
# place for anything a verifier might want to trust. The tag is there so a
# signature lifted from one release cannot be presented as another's without
# the mismatch being visible to a person running `minisign -V`.
#
# A key with a password cannot be used non-interactively, which is why CI's key
# is generated with `-W` and the secret store is what protects it — see
# docs/RELEASE.md.
#
# minisign's stderr is captured and shown, never discarded. It used to go to
# /dev/null, so the v0.12.1 job reported only "minisign failed to sign" while
# minisign was saying precisely what was wrong. (Its exit status was always
# reliable — 2 on a key it cannot load — it was only the reason that was lost.)
if ! err=$(minisign -S -s "$keyfile" -m "$sums" \
     -t "PeerBeam $tag SHA256SUMS" 2>&1 >/dev/null); then
  echo "::error::sign-release: minisign failed to sign $sums" >&2
  [ -n "$err" ] && echo "::error::minisign said: $err" >&2
  exit 1
fi

sig="${sums}.minisig"
if [ ! -f "$sig" ]; then
  echo "::error::sign-release: minisign reported success but $sig is missing" >&2
  exit 1
fi

# Verify what we just produced, with the public half, before anyone relies on
# it. A signature that does not verify is worse than none: it would be
# published, shipped, and only discovered by the first user whose download the
# app refused.
if [ -n "${MINISIGN_PUBLIC_KEY:-}" ]; then
  if ! minisign -V -P "$MINISIGN_PUBLIC_KEY" -m "$sums" >/dev/null 2>&1; then
    echo "::error::sign-release: the signature just written does not verify against MINISIGN_PUBLIC_KEY" >&2
    exit 1
  fi
  echo "sign-release: signature verified against the published public key" >&2
else
  echo "::warning::sign-release: MINISIGN_PUBLIC_KEY unset — signature written but not checked back." >&2
fi

printf '%s\n' "$sig"
