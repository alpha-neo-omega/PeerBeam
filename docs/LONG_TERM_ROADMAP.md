# Long-Term Roadmap

Where PeerBeam goes after the 1.0 line, evaluated as if it were a major public
open-source project. Priorities are ordered by leverage: unblock the Stable
claim first, then durability, then reach.

## Now → Stable v1.0 (release gate)

The [Stable Readiness](STABLE_READINESS.md) blockers, restated as work:

1. **Cross-platform verification** — build + smoke-transfer on real Windows and
   macOS hosts; publish artifacts via `release.yml`.
2. **Transport matrix** — verify Wi-Fi / Ethernet / USB tethering / Tailscale /
   IPv6 / cross-subnet on real hardware; record results in the
   [Compatibility Matrix](FINAL_COMPATIBILITY_MATRIX.md).
3. **Supply-chain gate** — add `cargo audit` + `cargo deny` to CI; run clean.
4. **Version + tag** — bump to `1.0.0`, tag `v1.0.0`.

## v1.1 — Durability & polish (short term)

- **Persistent device identity** — stable keypair across restarts so peers don't
  re-pin (closes the main [Known Issues](KNOWN_ISSUES.md) security item).
- **CLI completeness** — finish `clipboard`, `history`, and `daemon stop|status`.
- **Desktop integration** — OS notifications + tray; drag-and-drop parity.
- **Repo polish** — README badges + screenshots; `CODEOWNERS`; issue-form
  templates; release checklist automation.
- **Perf instrumentation** — memory/CPU profiling harness; concurrent-transfer
  stress test; measured startup-time budget (CLAUDE.md 500 ms target).

## v1.2 — Robustness (medium term)

- **Wire-format versioning** — explicit protocol version negotiation
  (currently governed only by release version).
- **Bandwidth limiting & QoS** — expose the rate limit the engine can already
  model; ETA/statistics surfaced in UI + CLI JSON.
- **Resume across restarts, end to end** — surface the reliability checkpoint in
  UI; test process-kill/resume in CI.
- **Fuzzing** — fuzz the frame parser and folder-manifest path handling.

## v2.0 — Reach (long term)

- **QUIC feature depth / WebRTC transport** — NAT traversal for internet-direct
  and relay routes (the last two `RouteManager` tiers).
- **iOS and Web frontends** — the engine is already frontend-agnostic via FFI;
  add the platform adapters.
- **Plugin ecosystem** — stabilize the discovery/transfer/clipboard/storage
  provider interfaces as a public extension API (ABI/semver policy).
- **Relay server** (optional, self-hostable) — for peers with no shared network
  and no Tailscale, keeping the no-cloud default.

## Sustainability commitments

- **API stability** — after 1.0, follow semver; the FFI ABI version gates
  breaking native changes independently.
- **CI as the contract** — the merge gate (fmt/clippy/test/examples + flutter)
  is the definition of "green"; add coverage + supply-chain checks over time.
- **Docs stay honest** — every release re-runs the documentation link/example
  check; claims marked Verified / Code-reviewed / Environment-limited.
- **Small modules, one responsibility** — resist God crates; new capabilities
  are new adapters implementing a domain port.

## Explicit non-goals

- No accounts, telemetry, analytics, or mandatory cloud — ever.
- No feature that requires manual network configuration for the common case.
