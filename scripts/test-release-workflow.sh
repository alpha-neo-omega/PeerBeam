#!/usr/bin/env bash
# Run the `release` job of .github/workflows/release.yml on this machine, and
# check what it would have published.
#
#   scripts/test-release-workflow.sh        needs yq (v4) and minisign
#
# That job runs once per tag, and every mistake in it so far was found in
# production: v0.4.1, v0.5.0 and v0.6.0 were deleted by a re-pushed tag and
# never put back, and v0.12.1 was published unsigned while the job reported
# success. Pushing a tag publishes a release, so this is the way to try it.
#
# Each step's `run:` is read out of the workflow rather than restated here, so
# this fails when the workflow changes instead of when someone remembers to
# update a copy. Steps run the way a GitHub-hosted Ubuntu runner runs them:
# `bash -e`, from the workspace, with only the env the step declares. `gh` and
# `sudo` are fakes that record what they were asked; `gh release create` copies
# the files it was handed into a "published" directory for the checks below.
# minisign is real, with throwaway keys, and stays off PATH until the job's own
# `apt-get install -y minisign` runs, so a step that signs before the install
# cannot pass by accident on a machine that happens to have it.
#
# Anything this does not model — an `if:`, a shell other than bash, `${{ }}`
# inside `run:`, an unknown expression, action or input — stops it with exit 2
# instead of being skipped. A harness that quietly ignores a step proves
# nothing about it.
#
# What the job is held to: a key that is configured but cannot sign fails it
# before the existing release is deleted; no key publishes unsigned; a good key
# publishes a signature that verifies; SHA256SUMS covers exactly what ships;
# and no artifacts, or two artifacts with one name, also stop it before the
# delete. Seen failing against each of: `|| true` put back on the signing step,
# the delete moved ahead of signing, the install moved after it, the previous
# sign-release.sh, and staging with `cp` and no duplicate-name check.
set -euo pipefail

repo=$(cd "$(dirname "$0")/.." && pwd)
wf="$repo/.github/workflows/release.yml"
TAG=v9.9.9
VER=9.9.9
SLUG=example.invalid/peerbeam-sim # never a real repository

command -v yq >/dev/null || { echo "needs yq (mikefarah, v4)" >&2; exit 2; }
command -v minisign >/dev/null || { echo "needs minisign" >&2; exit 2; }
MINISIGN_BIN=$(command -v minisign)

root=$(mktemp -d "${TMPDIR:-/tmp}/release-workflow.XXXXXX")
if [ -n "${KEEP:-}" ]; then echo "keeping $root"; else trap 'rm -rf "$root"' EXIT; fi

die() { echo "test-release-workflow: $*" >&2; exit 2; }

# --- throwaway keys ---------------------------------------------------------
k="$root/keys"
mkdir -p "$k"
minisign -G -W -p "$k/good.pub" -s "$k/good.key" >/dev/null 2>&1
minisign -G -W -p "$k/other.pub" -s "$k/other.key" >/dev/null 2>&1
GOOD_KEY=$(cat "$k/good.key")
GOOD_PUB=$(sed -n 2p "$k/good.pub")
OTHER_PUB=$(sed -n 2p "$k/other.pub")
B64_ONLY=$(sed -n 2p "$k/good.key") # the v0.12.1 mistake
CORRUPT=$(printf '%s\n%s' "$(sed -n 1p "$k/good.key")" \
  "$(sed -n 2p "$k/good.key" | cut -c1-40)") # right shape, wrong bytes

# --- a PATH with no minisign, gh or sudo on it ------------------------------
# The real gh must be unreachable: a slip here would talk to GitHub.
sys="$root/sys"
mkdir -p "$sys"
ln -s /usr/bin/* "$sys/" 2>/dev/null || true
rm -f "$sys/minisign" "$sys/gh" "$sys/sudo"

# --- fakes -------------------------------------------------------------------
fakes="$root/fakes"
mkdir -p "$fakes"
cat > "$fakes/gh" <<'EOF'
#!/usr/bin/env bash
# Models the two calls the release job makes, and nothing else.
set -euo pipefail
case "${1:-} ${2:-}" in
  "release delete")
    # What exists, signing-wise, at the instant the old release is destroyed.
    seen=$(find "$GITHUB_WORKSPACE" "$RUNNER_TEMP" -type f -name 'SHA256SUMS*' \
             -printf '%f\n' | sort | tr '\n' ' ')
    echo "delete $3 [saw: ${seen}]" >> "$SIM_LOG"
    if [ -f "$SIM_STATE/existing-release" ]; then
      rm "$SIM_STATE/existing-release"
      exit 0
    fi
    echo "release not found" >&2
    exit 1 ;;
  "release create")
    shift 2; tag=$1; shift; files=()
    while [ $# -gt 0 ]; do
      case "$1" in
        --repo|--title|--notes-file) shift 2 ;;
        --verify-tag|--generate-notes) shift ;;
        -*) echo "fake gh: unmodelled flag $1" >&2; exit 2 ;;
        *) files+=("$1"); shift ;;
      esac
    done
    mkdir "$SIM_STATE/published" # a second create fails, as a real one would
    for f in "${files[@]}"; do
      [ -f "$f" ] || { echo "fake gh: no such file $f" >&2; exit 1; }
      b=$(basename "$f")
      [ ! -e "$SIM_STATE/published/$b" ] || { echo "HTTP 422: duplicate asset $b" >&2; exit 1; }
      cp "$f" "$SIM_STATE/published/$b"
    done
    echo "create $tag (${#files[@]} files)" >> "$SIM_LOG"
    echo new > "$SIM_STATE/existing-release" ;;
  *) echo "fake gh: unmodelled: $*" >&2; exit 2 ;;
esac
EOF
cat > "$fakes/sudo" <<'EOF'
#!/usr/bin/env bash
# Installing minisign means: the real binary is on PATH from now on.
echo "sudo $*" >> "$SIM_LOG"
case "$*" in
  "apt-get update") ;;
  "apt-get install -y minisign") ln -sf "$SIM_MINISIGN" "$SIM_INSTALLED/minisign" ;;
  *) echo "fake sudo: unmodelled: $*" >&2; exit 2 ;;
esac
EOF
chmod +x "$fakes/gh" "$fakes/sudo"

# --- artifacts, laid out as download-artifact left v0.12.1's ----------------
EXPECTED_ARTIFACTS=23
make_store() { # <dir> full|none|collide
  local a=$1 mode=$2 f
  mkdir -p "$a"
  [ "$mode" = none ] && return 0
  for f in \
    linux/peerbeam-$VER-x86_64.AppImage linux/peerbeam-$VER-linux-x64.tar.gz \
    linux/peerbeam-$VER-x86_64.rpm linux/peerbeam-$VER-amd64.deb \
    linux-arm64/peerbeam-$VER-aarch64.rpm linux-arm64/peerbeam-$VER-linux-arm64.tar.gz \
    linux-arm64/peerbeam-$VER-arm64.deb linux-arm64/peerbeam-$VER-aarch64.AppImage \
    android/peerbeam-$VER-android.apk android/peerbeam-$VER-android.aab \
    cli-linux/peerbeam-linux-x64 cli-linux/peerbeam.fish cli-linux/peerbeam.bash \
    cli-linux/_peerbeam cli-linux-arm64/peerbeam-linux-arm64 \
    cli-macos/peerbeam-macos-arm64 cli-macos/peerbeam-macos-universal \
    cli-macos/peerbeam-macos-x64 cli-windows/peerbeam-windows-x64.exe \
    cli-windows-arm64/peerbeam-windows-arm64.exe \
    windows/dist/peerbeam-$VER-windows-x64-portable.zip \
    windows-arm64/dist/peerbeam-$VER-windows-arm64-portable.zip \
    macos/PeerBeam-$VER.dmg; do
    mkdir -p "$a/$(dirname "$f")"
    printf 'fake bytes of %s\n' "$f" > "$a/$f"
  done
  # Build debris that must not ship: subtrees whose files collide on
  # basename, and the MSIX that awaits signing.
  mkdir -p "$a/linux/stage/usr/bin" "$a/linux/deb/usr/bin" \
    "$a/windows/flutter/build/windows/x64/runner/Release"
  echo stage > "$a/linux/stage/usr/bin/peerbeam"
  echo deb > "$a/linux/deb/usr/bin/peerbeam"
  echo msix > "$a/windows/flutter/build/windows/x64/runner/Release/peerbeam.msix"
  if [ "$mode" = collide ]; then
    # Completions emitted by the arm64 leg too: one name, two directories.
    echo "arm64 completions" > "$a/cli-linux-arm64/peerbeam.bash"
  fi
}

# --- one run of the job -----------------------------------------------------
# Sets C (this run's directory), STATUS and FAILED_STEP.
run_job() { # <name> <secret> <public key> <store: full|none|collide> <notes: yes|no> <existing release: yes|no>
  local name=$1 secret=$2 pub=$3 mode=$4 notes=$5 existing=$6
  C="$root/$name"
  local ws="$C/ws" rt="$C/_temp" inst="$C/installed" st="$C/state"
  mkdir -p "$ws" "$rt" "$inst" "$st" "$C/home"
  : > "$C/gh.log"
  : > "$C/job.log"
  if [ "$existing" = yes ]; then echo old > "$st/existing-release"; fi
  make_store "$C/store" "$mode"

  STATUS=success FAILED_STEP=""
  local n i
  n=$(yq '.jobs.release.steps | length' "$wf")
  for ((i = 0; i < n; i++)); do
    local s=".jobs.release.steps[$i]" key sname uses
    for key in $(yq "$s | keys | .[]" "$wf"); do
      case $key in name|uses|with|run|env|shell|id) ;; *) die "step $i: '$key' is not modelled" ;; esac
    done
    sname=$(yq "$s.name // $s.uses" "$wf")
    uses=$(yq "$s.uses // \"\"" "$wf")
    echo "::step:: $sname" >> "$C/job.log"

    if [ -n "$uses" ]; then
      case $uses in
        actions/checkout@*)
          [ "$(yq "$s.with // {} | length" "$wf")" -eq 0 ] || die "checkout inputs are not modelled"
          cp -r "$repo/scripts" "$ws/"
          mkdir -p "$ws/docs"
          if [ "$notes" = yes ]; then echo "notes" > "$ws/docs/RELEASE_NOTES_$TAG.md"; fi ;;
        actions/download-artifact@*)
          [ "$(yq "$s.with | keys | join(\",\")" "$wf")" = path ] || die "download-artifact inputs other than path are not modelled"
          local dest
          dest=$(yq "$s.with.path" "$wf")
          mkdir -p "$ws/$dest"
          cp -r "$C/store/." "$ws/$dest/" ;;
        *) die "action $uses is not modelled" ;;
      esac
      continue
    fi

    local script="$rt/step-$i.sh" shell shellcmd=(bash -e)
    yq "$s.run" "$wf" > "$script"
    ! grep -q '\${{' "$script" || die "step '$sname': \${{ }} inside run: is not modelled"
    shell=$(yq "$s.shell // \"\"" "$wf")
    case $shell in
      "") ;;
      bash) shellcmd=(bash --noprofile --norc -eo pipefail) ;;
      *) die "step '$sname': shell '$shell' is not modelled" ;;
    esac
    local envs=() var raw
    while IFS= read -r var; do
      [ -n "$var" ] || continue
      raw=$(yq "$s.env.\"$var\"" "$wf")
      case $raw in
        '${{ secrets.GITHUB_TOKEN }}') envs+=("$var=fake-token") ;;
        '${{ secrets.MINISIGN_SECRET_KEY }}') envs+=("$var=$secret") ;;
        '${{ vars.MINISIGN_PUBLIC_KEY }}') envs+=("$var=$pub") ;;
        *'${{'*) die "step '$sname': $raw is not modelled" ;;
        *) envs+=("$var=$raw") ;;
      esac
    done < <(yq "$s.env // {} | keys | .[]" "$wf")

    : > "$rt/github_env"
    : > "$rt/github_path"
    if ! (cd "$ws" && env -i PATH="$fakes:$inst:$sys" HOME="$C/home" LANG=C.UTF-8 \
          GITHUB_WORKSPACE="$ws" GITHUB_REF_NAME="$TAG" GITHUB_REPOSITORY="$SLUG" \
          RUNNER_TEMP="$rt" GITHUB_ENV="$rt/github_env" GITHUB_PATH="$rt/github_path" \
          GITHUB_OUTPUT="$rt/github_output" GITHUB_STEP_SUMMARY="$rt/summary" \
          SIM_LOG="$C/gh.log" SIM_STATE="$st" SIM_MINISIGN="$MINISIGN_BIN" SIM_INSTALLED="$inst" \
          "${envs[@]}" "${shellcmd[@]}" "$script") >> "$C/job.log" 2>&1; then
      STATUS=failure FAILED_STEP=$sname
      break
    fi
    [ ! -s "$rt/github_env" ] && [ ! -s "$rt/github_path" ] \
      || die "step '$sname' wrote GITHUB_ENV or GITHUB_PATH, which is not modelled"
  done
}

# --- checks -----------------------------------------------------------------
FAILURES=0
ok() { # <description> <command...>
  local what=$1
  shift
  if "$@"; then echo "   ok    $what"; else echo "   FAIL  $what"; FAILURES=$((FAILURES + 1)); fi
}
gh_did() { grep -q -- "$1" "$C/gh.log"; }
gh_never() { ! grep -q -- "$1" "$C/gh.log"; }
log_says() { grep -qF -- "$1" "$C/job.log"; }
shipped() { [ -f "$C/state/published/$1" ]; }
not_shipped() { [ ! -e "$C/state/published/$1" ]; }
release_is() { [ "$(cat "$C/state/existing-release" 2>/dev/null)" = "$1" ]; }
artifact_count_is() {
  [ "$(ls "$C/state/published" | grep -cvx 'SHA256SUMS\|SHA256SUMS.minisig')" -eq "$1" ]
}
sums_cover_exactly_what_shipped() {
  local p="$C/state/published"
  (cd "$p" && sha256sum --quiet -c SHA256SUMS) || return 1
  diff <(cut -d' ' -f3- "$p/SHA256SUMS" | sort) \
       <(ls "$p" | grep -vx 'SHA256SUMS\|SHA256SUMS.minisig' | sort) >/dev/null
}
verifies_with() { (cd "$C/state/published" && minisign -Vq -P "$1" -m SHA256SUMS); }
trusted_comment_names_tag() {
  (cd "$C/state/published" && minisign -V -P "$GOOD_PUB" -m SHA256SUMS 2>&1) \
    | grep -qF "PeerBeam $TAG SHA256SUMS"
}
failed_while_signing() {
  [ "$STATUS" = failure ] \
    && yq ".jobs.release.steps[] | select(.name == \"$FAILED_STEP\") | .run" "$wf" \
       | grep -q 'sign-release.sh'
}
summary() { echo "   -- $STATUS${FAILED_STEP:+ in \"$FAILED_STEP\"}; $(tr '\n' ';' < "$C/gh.log")"; }

publishes_unsigned() {
  ok "job succeeds" [ "$STATUS" = success ]
  ok "the old release is replaced" release_is new
  ok "all $EXPECTED_ARTIFACTS artifacts ship, and no debris" artifact_count_is "$EXPECTED_ARTIFACTS"
  ok "SHA256SUMS ships and covers exactly what shipped" sums_cover_exactly_what_shipped
  ok "no signature ships" not_shipped SHA256SUMS.minisig
  ok "says it is unsigned" log_says "publishing without a signature"
}
publishes_signed() {
  ok "job succeeds" [ "$STATUS" = success ]
  ok "sums and signature existed before the delete" gh_did "^delete $TAG \[saw: SHA256SUMS SHA256SUMS.minisig \]"
  ok "the old release is replaced" release_is new
  ok "all $EXPECTED_ARTIFACTS artifacts ship, and no debris" artifact_count_is "$EXPECTED_ARTIFACTS"
  ok "SHA256SUMS covers exactly what shipped" sums_cover_exactly_what_shipped
  ok "SHA256SUMS.minisig ships" shipped SHA256SUMS.minisig
  ok "it verifies against the public key" verifies_with "$GOOD_PUB"
  ok "its trusted comment names the tag" trusted_comment_names_tag
}
stops_before_the_delete() { # <what the log must say>
  ok "job fails" [ "$STATUS" = failure ]
  ok "the old release is never deleted" gh_never "^delete"
  ok "nothing is created" gh_never "^create"
  ok "the old release is intact" release_is old
  ok "the log says: $1" log_says "$1"
}

echo "steps: $(yq '[.jobs.release.steps[] | .name // .uses] | join(" -> ")' "$wf")"

echo "(a) no key configured, as in a fork"
run_job a "" "" full no yes; summary
publishes_unsigned

echo "(a2) no secret, but the public-key variable still set"
run_job a2 "" "$GOOD_PUB" full yes yes; summary
publishes_unsigned

echo "(b) the secret holds only the base64 line (v0.12.1)"
run_job b "$B64_ONLY" "$GOOD_PUB" full yes yes; summary
stops_before_the_delete "does not look like a minisign secret key file"
ok "it failed in the signing step" failed_while_signing

echo "(b2) the right shape, but bytes minisign cannot load"
run_job b2 "$CORRUPT" "$GOOD_PUB" full yes yes; summary
stops_before_the_delete "minisign failed to sign"
ok "it failed in the signing step" failed_while_signing

echo "(b3) a valid key, but not the one MINISIGN_PUBLIC_KEY names"
run_job b3 "$GOOD_KEY" "$OTHER_PUB" full yes yes; summary
stops_before_the_delete "does not verify against MINISIGN_PUBLIC_KEY"
ok "it failed in the signing step" failed_while_signing

echo "(c) the correct key"
run_job c "$GOOD_KEY" "$GOOD_PUB" full yes yes; summary
publishes_signed

echo "(c2) the correct key, stored with its trailing newline (gh secret set < file)"
run_job c2 "$GOOD_KEY"$'\n' "$GOOD_PUB" full yes yes; summary
publishes_signed

echo "(c3) the correct key, and no release to replace (a first publish)"
run_job c3 "$GOOD_KEY" "$GOOD_PUB" full no no; summary
ok "job succeeds" [ "$STATUS" = success ]
ok "the release is created, signed" shipped SHA256SUMS.minisig

echo "(d) no artifacts at all"
run_job d "$GOOD_KEY" "$GOOD_PUB" none yes yes; summary
stops_before_the_delete "no artifacts found to publish"

echo "(e) two artifacts with the same name"
run_job e "$GOOD_KEY" "$GOOD_PUB" collide yes yes; summary
stops_before_the_delete "are both named peerbeam.bash"

# sign-release.sh on its own, with no minisign on PATH.
sign_alone() { # <dir> [VAR=value ...]
  local d=$1
  shift
  mkdir -p "$d"
  printf '%064d  x\n' 0 > "$d/SHA256SUMS"
  RC=0
  env -i PATH="$sys" HOME="$d" "$@" "$repo/scripts/sign-release.sh" "$d/SHA256SUMS" "$TAG" \
    > "$d/out" 2> "$d/err" || RC=$?
}

echo "(f) sign-release.sh with a key, and no minisign to use it"
sign_alone "$root/f" MINISIGN_SECRET_KEY="$GOOD_KEY"
ok "exits 1" [ "$RC" -eq 1 ]
ok "says minisign is missing" grep -qF "minisign is not installed" "$root/f/err"
ok "prints no signature path" [ ! -s "$root/f/out" ]

echo "(f2) sign-release.sh with no key and no minisign"
sign_alone "$root/f2"
ok "exits 0" [ "$RC" -eq 0 ]
ok "says it is unsigned" grep -qF "publishing without a signature" "$root/f2/err"
ok "prints no signature path" [ ! -s "$root/f2/out" ]

echo
if [ "$FAILURES" -eq 0 ]; then
  echo "release workflow: all checks passed"
else
  echo "release workflow: $FAILURES check(s) failed (KEEP=1 keeps each run's job.log)" >&2
  exit 1
fi
