#!/usr/bin/env bash
# The README states how many tests this project has. Numbers typed by hand rot:
# it read "377 Rust + 35 Flutter" while the real figures were 1135 and 310, and
# nothing failed in between. This recomputes them from the suites themselves.
#
#   scripts/readme-test-counts.sh          update README.md in place
#   scripts/readme-test-counts.sh --check  exit 1 if it is stale (for CI)
set -euo pipefail
cd "$(dirname "$0")/.."

rust=$(cd rust && cargo test --workspace 2>/dev/null \
  | awk -F'[ ;]' '/^test result: ok/ {s+=$4} END {print s+0}')
flutter=$(cd flutter && flutter test 2>/dev/null \
  | tail -1 | grep -oE '\+[0-9]+' | head -1 | tr -d '+')

if [ -z "${rust:-}" ] || [ "$rust" -eq 0 ] || [ -z "${flutter:-}" ]; then
  echo "could not count tests (rust='${rust:-}' flutter='${flutter:-}')" >&2
  exit 2
fi

want="**${rust} Rust + ${flutter} Flutter tests**"
have=$(grep -oE '\*\*[0-9]+ Rust \+ [0-9]+ Flutter tests\*\*' README.md || true)

if [ "$have" = "$want" ]; then
  echo "README test counts are current: ${rust} Rust + ${flutter} Flutter"
  exit 0
fi

if [ "${1:-}" = "--check" ]; then
  echo "README is stale: says '${have:-<nothing>}', suites report '${want}'" >&2
  echo "run scripts/readme-test-counts.sh to update it" >&2
  exit 1
fi

sed -i -E "s/\*\*[0-9]+ Rust \+ [0-9]+ Flutter tests\*\*/${want//\//\\/}/" README.md
echo "README updated: ${have:-<nothing>} -> ${want}"
