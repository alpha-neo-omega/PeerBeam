# Spaces

A **Space** is a name you give to a set of devices you already trust, so you can
message or send to all of them in one action.

```
peerbeam space create "Work Laptops"
peerbeam space add "Work Laptops" pb-3f9a12cd48b1
peerbeam space add "Work Laptops" pb-77b2e0451aa9
peerbeam space send "Work Laptops" quarterly.pdf
```

That last line is two ordinary sends, one to each device, over the peer sessions
that already exist.

---

## What a Space deliberately is **not**

This is the important half, and it is why the feature looks the way it does.

**Not a group.** A Space has no shared roster, no group key, no group id, and no
membership messages. Nothing about it is ever put on the wire — this crate
defines no message type and registers no channel. Two devices in the same Space
have no idea that they are. (Since
[A2](ARCHITECTURAL_INVARIANTS.md#a2--peer-held-group-conversations-without-a-hub-2026-08-27)
a separate **Group** construct does exist, with a roster every member holds.
It is a different thing with a different trade — see the amendment note below —
and none of this paragraph is weakened by it.)

**Not synced.** A Space exists on the device that defined it and nowhere else.
Your laptop's "Work Laptops" and your phone's "Work Laptops" are two unrelated
labels that happen to share a name. Nobody is told when you create, rename,
fill, empty or delete one.

**Not a hub, and not a room.** There is no coordinator, no server, no host
device, and nothing to join or leave. This is invariant **I3** — *no feature
requires a central hub or server for its common case* — applied literally: a
Space cannot require a hub, because a Space never leaves the machine.
[VISION.md](VISION.md)'s permanent non-goal says the same thing from the other
side: *"No hub-brokered group chat, feeds, discovery of strangers, or public
rooms."* A2 leaves this untouched for Spaces, and satisfies it for Groups by
having no broker rather than by having no roster.

**Not a permission.** Being in a Space grants a device nothing at all. Each of
the N sends a fan-out performs passes through exactly the gate it would have
passed through if you had sent to that device by hand — `may_exchange_chat`, the
`files` permission, the transfer admission gate. That is invariant **I6**:
sensitive actions need explicit, revocable, per-capability consent, and a label
you typed is not consent. See [SECURITY.md](SECURITY.md).

**Not discovery.** You cannot add a device you have not already paired with.
There is no way to find people, and no directory to be found in.

---

## Nobody learns who else is in it

Every member receives a normal direct message, indistinguishable from one you
typed to them alone. They cannot enumerate the other members, cannot tell a
fan-out from a direct send, and cannot learn that the Space exists.

**This is a privacy feature, not a limitation, and it is the reason no group
identity travels.** Group metadata is the part of a messaging system that leaks
most: who knows whom, which is precisely what a peer-to-peer tool with no
accounts should never be in a position to reveal by default.

The cost is real and worth naming: **a Space has no group replies.** A member's
answer comes back to you alone, because that is the only party who knows the
message went to more than one device. If you want everyone to see everyone, tell
them who else you sent to — that disclosure is yours to make, not PeerBeam's.

> **Amended 2026-08-27 by [A2](ARCHITECTURAL_INVARIANTS.md#a2--peer-held-group-conversations-without-a-hub-2026-08-27).**
> This section previously argued that no roster may exist on the wire at all,
> on the grounds that "some device would have to hold the roster and answer
> questions about it — and a device that brokers membership on everyone else's
> behalf is a hub, whatever it is called." A2 rejects that step: a roster every
> member holds **in full**, and that no member is asked for, has no broker. No
> device others must query, none whose absence stops the conversation, none that
> learns what the rest do not.
>
> So **Groups** exist alongside Spaces, and they do have group replies. What
> they cost is exactly the property this section defends: in a Group, every
> member learns every other member, and A2 requires the UI to say so at the
> point of joining.
>
> **Everything above about Spaces still holds, unchanged.** A Space remains
> local, rosterless, unsynced and invisible to peers; it is not a Group, is
> never converted into one, and its fan-out send is not renamed "group chat".
> Choose a Space when nobody should learn who else received it, and a Group when
> everyone is meant to. Groups are documented in [GROUPS.md](GROUPS.md), which
> opens with a table for exactly that choice.

---

## Membership is checked when it is read, not when it is written

A Space stores device ids. Trust in those devices can end without anything
writing to the Space:

- you revoke the device (`peerbeam trust revoke`), or
- a time-limited grant (*"trust this device for 30 minutes"*) simply runs out.

So membership is reconciled against the trust store **on every read**. Each read
returns two lists — the members this device still trusts, and the members it
does not:

```
Work Laptops
  pb-3f9a12cd48b1   laptop
  pb-77b2e0451aa9   desktop
  pb-9c01ff23de77   old-phone     no longer trusted — not sent to
```

Stale members are **reported, never silently dropped**: a count that quietly
shrank would tell you nothing, and re-pairing the device is usually what you
actually want to do.

Nothing prunes them, either. A sweeper is a second source of truth and the
slower one — the gap between sweeps is a window in which a fan-out still reaches
a device you revoked — and pruning would destroy a membership you never asked to
lose, since trust comes back when a window is renewed or a device is re-paired.
This is the same choice, for the same reason, that trust expiry itself makes:
see `TrustRecord::expires_at` in `peerbeam-domain`, enforced by the predicates
every gate already asks rather than by a background pass.

The predicate is *"do we still hold a live pin for this device"*
(`TrustStore::is_trusted`), not *"did the user explicitly approve it"*
(`is_approved`). Chat has never required approval — see
`peerbeam_chat::gate::may_exchange_chat` — so the stronger predicate would
report devices as stale that are perfectly able to receive. This is safe
precisely because a Space is not a gate: what may leave the machine is still
decided per peer, per capability, at send time.

---

## Refusals

Every refusal names what was wrong.

| Refused | Why |
| --- | --- |
| An empty or whitespace-only name | A Space is addressed by name; a nameless one cannot be. |
| A name over 128 bytes | It is echoed into log lines, CLI tables and app events. Refused rather than truncated — a truncated name is one you did not choose, and could silently collide with another. |
| A name holding a control or bidi-override character | A newline paints extra rows into a list and an escape sequence is acted on by a terminal; `work<U+202E>nimda` renders as `workadmin`. Refused here, unlike a peer-supplied *file* name (which is defanged instead, because refusing it would make a file unreceivable) — a label you cannot read back is not a label, and you can type another. |
| A name another Space already answers to | Names are compared ignoring case and surrounding spaces, so `Work`, `work` and `  WORK  ` are one name. Two Spaces sharing one would make `peerbeam space send work …` ambiguous. |
| A member id that is empty, over 128 bytes, or holding whitespace or a control character | It has to survive a CLI argument, a log line and a storage key unambiguously. |
| A member id naming a device this machine does not trust | A typo, or a device that was revoked. Pair with it first. Note this is a *message*, not a gate: what keeps a fan-out honest is the read-time check above, since a device trusted today can be revoked tomorrow. |
| Any of the above when the trust store cannot be read | Fails closed. A store that cannot answer is not permission. |

Removing a member and deleting a Space validate nothing — a revoked member is
exactly the one you most need to be able to take out.

---

## Where it lives

`rust/crates/peerbeam-spaces` — an adapter over two existing ports, holding no
state of its own:

```
SpaceStore ──> AppStore    (peerbeam-domain::port)  encrypted local records
           └─> TrustStore  (peerbeam-domain::port)  who is still trusted
```

Records live in the `spaces` namespace of the same encrypted `AppStore` that
holds the chat log and notes, so a Space name is not sitting in cleartext on
disk (**I11**). A Space keeps an opaque id separate from its name so a rename is
a single write that keeps the id and the members — keying records by name would
make a rename a write-then-delete pair, and a crash between the two would either
duplicate a Space or lose one.

## Testing

- **Unit** (`src/space.rs`): the name and member-id rules, and the read-time
  partition — including that an unreadable trust store makes every member stale.
- **Unit** (`src/store.rs`): create, rename, delete, membership, every refusal,
  ordering, and an undecodable record (skipped by `list`, reported by `get`).
- **Integration** (`tests/spaces.rs`): the whole thing over a real encrypted
  `FsAppStore` and a real `FsTrust` — a round trip through a reopened store, a
  real `trust revoke` dropping a member from every Space at the next read, an
  expired window doing the same with nothing running, and a check that no Space
  operation writes a byte to the trust store.

No test sleeps. A closed trust window is expressed as a deadline already in the
past, which is what makes it an assertion about the predicate rather than about
how long the test waited.
