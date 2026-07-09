# Clipboard sharing

Share clipboard content between devices: **text**, **URLs**, **code**, and
**images**. Platform-independent by design — the model and transfer are
OS-agnostic; each platform plugs its real clipboard in behind one port.

## Model (`peerbeam-domain`)

```
ClipboardKind = Text | Url | Code | Image
ClipboardData = Text(String) | Bytes(Vec<u8>)
ClipboardItem { kind, mime, data, at }
```

- `ClipboardItem::text(s, at)` auto-classifies via `classify`:
  - a whitespace-free `http(s)`/`ftp`/`mailto` string → **Url** (`text/uri-list`)
  - text with code markers (or multi-line + one marker) → **Code**
  - otherwise → **Text** (`text/plain`)
- `ClipboardItem::image(bytes, mime, at)` → **Image** (e.g. `image/png`).

Classification is a conservative heuristic, fully unit-tested.

## Transfer (`peerbeam-transfer::clipboard`)

Over any `Link`:

```
text / url / code:  Inline(item)                       one Control frame
image:              BinaryMeta(mime, at, size) → Chunk … → Complete
```

Text-like items are self-contained in a single inline frame. Images stream
as `Chunk` frames so a large image is never one giant frame. `send_clipboard`
/ `receive_clipboard` mirror the file-transfer style and reuse the same retry
helper.

## Providers (cross-platform)

`ClipboardProvider` (read/write a `ClipboardItem`) is the OS seam:

- **`peerbeam-clipboard-mem`** — in-memory provider, shipped now. Default for
  headless servers and the deterministic test double.
- Desktop (`arboard`-backed) and Android adapters implement the *same* port
  and swap in via the engine builder — the transfer layer is unchanged.

The engine flow (once wired): `provider.read()` → `send_clipboard` on the
sender; `receive_clipboard` → `provider.write()` on the receiver.

## Testing

- **Unit**: classification (url/code/text edge cases), constructors, and
  clip-message codec round-trip; `MemoryClipboard` read/write/overwrite and
  empty-read → `NotFound`.
- **Integration** (`peerbeam-transfer/tests/clipboard.rs`): text, URL, code,
  and a 200 KiB image round-tripped over an in-memory link — kind, MIME, and
  bytes preserved exactly.
