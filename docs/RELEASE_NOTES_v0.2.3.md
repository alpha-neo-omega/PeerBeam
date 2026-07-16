# PeerBeam v0.2.3 — Beta

The stability release. Two full adversarial audits (42 confirmed bugs fixed),
an Android storage overhaul so received files land where you can find them,
cooperative pause that actually stops both sides, and LocalSend-style stacking
selection.

## Highlights

### Stacking selection (LocalSend-style)
- Build **one selection** from files, folders, text, and clipboard, review and
  edit it, then send the whole batch to a device in one go.
- **Content-first taps**: tapping a device sends the current selection (empty
  selection → pick files, as before). A persistent bar on Home shows the item
  count + total size with a one-tap Send.

### Android — files land where you can find them
- **Received files default to Downloads/PeerBeam** via MediaStore — visible in
  Files, images in Gallery, no permission prompt — or a **folder you pick**
  (Storage Access Framework). Engine data (trust/history/settings) moved to the
  persistent app-support dir so it survives.
- **"Save to" applies live** — a new receive folder and the auto-accept toggle
  take effect on the running engine, not only after a restart.
- **Large files no longer crash the app** — picked and shared files are streamed
  to cache instead of loaded into memory.
- **Share → PeerBeam** resolves `content://` URIs to real files and holds a
  wake lock for the whole transfer.

### Transfers — control and correctness
- **Cooperative pause**: either side pauses and **both** stop and show paused
  (receiver-side pause now actually pauses file *and* folder transfers, with no
  lost wakeups); correct speed/ETA after resume.
- **Cancel is reliable** — it interrupts a parked receive, fires exactly one
  terminal event, and abandoned incoming transfers are time-bounded so they
  can't leak the active count (no more stuck "1 transfer in progress").
- **Integrity**: a corrupt `.part` heals on checksum failure, folder receive
  overwrites instead of blind-appending, and folder send skips unreadable files.
- A **"Preparing files…"** spinner shows while a picked file is staged.

### Notifications (Android)
- Idle icon is a **static brand glyph**; the status-bar icon **animates only
  while transferring**. Tapping the notification **opens the app**.
- Notifications for received files and send complete/failed, de-duplicated with
  unique ids, gated by the settings toggle, and the foreground service no longer
  resurrects as a zombie after an OS kill. Background-receive is on by default
  to shield inbound transfers from Doze.

### Security & trust
- **Accepting a transfer no longer auto-trusts the sender** — trust is an
  explicit button, and auto-accept requires explicit approval, not just a pinned
  key.
- The auth handshake **binds the device identity** and carries the peer's human
  name, so History and Transfers show the name, not a raw `app-12345` id.
- Rejects a low-order public key; safe file finalize and symlink listing.

### Engine, discovery & robustness
- **The device list no longer freezes** under a broadcast-lag burst — the engine
  emits a resync hint and the app re-pulls the authoritative device list.
- Offline devices are pruned, DNS resolution is non-blocking, the trust store
  merges instead of clobbering, config loads missing fields as defaults instead
  of failing, init is idempotent, and **Tailscale peers are dialable** (the
  transfer port is stamped on discovery).
- Device rename applies live (identity + discovery announce).

### CLI
- `chunk_size` is clamped (no u32 truncation), `watch` shows all device events,
  the daemon prints honest hints, and `benchmark` cleans up temp files on error.

### UI polish
- Transfer rows carry stable keys, so a completing transfer no longer makes a
  concurrent one's progress bar animate backwards.
- Inline validation on the send-to-address dialog, leaked text controllers
  disposed, partial-batch sends report what failed, the Nearby picker only lists
  reachable devices, Android **back returns to the Home tab** before exiting,
  re-shares coalesce into the open sheet instead of stacking duplicates, and the
  brand mark is announced once by screen readers.
- Theme-tinted brand glyph that stays visible in both light and dark, a
  de-duplicated wordmark, and cleaner dialogs. Processing spinner + message are
  centered.

## Audits
Two adversarial find→verify sweeps this cycle, each with independent
verification of every candidate before it was fixed:
- **Full-repo sweep** — 27 confirmed bugs across engine, crypto, storage, FFI,
  transfer, CLI, and UI.
- **Deep UI/completeness sweep** — 15 confirmed bugs across state handling,
  dialogs, navigation, and lifecycle.

## Platforms
- Verified live this cycle: Linux (desktop + CLI) and Android (real
  cross-network transfers over Tailscale and LAN).
- CI builds and tests Windows and macOS (Intel and Apple Silicon) on every push.

## Gate
- 284 Rust tests, 56 Flutter tests, `clippy -D warnings` clean,
  `flutter analyze` clean, release builds on all desktop targets + Android.

_Signed macOS/Windows installers still pend signing secrets (see
docs/RELEASE.md); unsigned artifacts build in CI._
