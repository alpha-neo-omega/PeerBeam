# Groups

A **Group** is a conversation a set of devices share. Everyone in it holds the
same roster, so a reply reaches everyone — and **everyone in it learns who
everyone else is**.

That last sentence is the entire feature. It is the thing a Group buys and the
thing it costs, and it is why Groups needed a constitutional amendment before
they could exist at all.

---

## Group or Space?

They look similar and trade opposite things. Picking the wrong one discloses
something you did not mean to, so the difference is worth thirty seconds.

| | **Space** | **Group** |
|---|---|---|
| Who knows it exists | only this device | every member |
| Who learns the members | nobody | every member |
| Replies | come back to you alone | reach everyone |
| On the wire | nothing | a roster, and membership messages |
| Undo the disclosure | nothing to undo | **impossible** |

**Use a Space** to send the same file to five devices without any of them
learning about the others. **Use a Group** when the point is that everyone can
answer each other.

Neither is converted into the other, and a Space's fan-out is never renamed
"group chat". See [SPACES.md](SPACES.md).

---

## There is no server, and no host

Every member holds the whole roster. Nobody is asked for it, no device answers
membership questions for anyone else, and there is no message meaning *"who is
in this group?"* — because there is nothing to ask.

A message to a group is **N ordinary one-to-one sends**, each over the same
routes and the same encryption a hand-addressed message uses. There is no group
key: nothing is shared that could be compromised once and expose the whole
conversation.

This is invariant **I3** applied literally, and it is what amendment
[**A2**](ARCHITECTURAL_INVARIANTS.md#a2--peer-held-group-conversations-without-a-hub-2026-08-27)
permits Groups on the strength of. The amendment lists eight binding conditions;
a build that drops any one is outside it.

### What that costs

**Rosters can disagree for a while.** If somebody joins while you are offline,
you learn about them at next contact. There is no authority to ask for the
truth, because having one is precisely what is being refused.

**Names are not shared.** An invitation carries the creator's name as a
suggestion and nothing afterwards reconciles them: renaming is local. Agreeing
on one name would need a device to arbitrate simultaneous renames, and that
device would be a hub. Two members may see different names for one
conversation.

**Leaving is advisory.** Your device forgets the group and stops sending, and
the members are told — but a member that misses the message can keep sending to
you. Nothing here can compel otherwise. To actually refuse them, withhold the
`chat` permission from that device:

```bash
peerbeam trust revoke-permission <device> chat
```

---

## Joining

Nothing joins you to a group without an action by **your own** user.

1. Someone invites your device. That is an **offer**: it appears as a pending
   invitation and changes nothing else.
2. You accept. Only then is a roster written, and only then do the other
   members learn your device.

A peer can say *"I joined"* or *"I left"* — statements about itself, which it is
entitled to make. There is no message by which one person can enrol somebody
else's device, and the message payloads have nowhere to name a third party.

Ignoring an invitation is **local and silent**: the inviter is not told. Turning
an offer down is allowed to look exactly like never having seen it.

---

## Permissions still apply

Membership grants nothing. Every send passes the same per-device `chat`
permission a hand-addressed message passes, checked at the moment of sending
against the trust store.

A member you no longer trust is **named and skipped**, never silently dropped —
in `group list`, when you send, and when you leave. A list that quietly shrank
would leave you wondering whether you had ever added them.

---

## Using it

### In the app

**Groups** in the navigation. Pending invitations appear above your groups,
because holding an offer is not being in a group.

Joining goes through a dialog that names every device that will learn yours, and
says the disclosure cannot be undone. It is not a summary or a count: the
devices are listed, because somebody who would decline over one particular
device cannot act on a number.

### At a shell

```bash
peerbeam group create "Work Trip"
peerbeam group invite "Work Trip" alices-laptop
peerbeam group invite "Work Trip" --addr 100.64.0.7:51000   # a peer you can reach but have not discovered

peerbeam group list                    # groups, and invitations waiting
peerbeam group accept <GROUP-ID>       # join, and tell the members
peerbeam group send "Work Trip" "six works for me"
peerbeam group history "Work Trip"
peerbeam group leave "Work Trip"
```

`<GROUP>` resolves by exact id, then exact name, then unique name prefix — the
same ladder `send --to` and `trust approve` climb. An ambiguous name is refused
with the candidates named rather than guessed between: acting on the wrong group
is not a private mistake.

---

## Where a group's messages live

A group message is N one-to-one sends, so each copy is stored in the namespace
of the member it went to or came from, tagged with the group. There is no group
namespace, because there is no group conversation on the wire to have one.

A transcript is gathered across members; the private conversation with any one
member is what is left when the tagged rows are taken out. An outgoing message
appears **once** in a transcript despite having a row per recipient — every copy
shares one id.

Each member's [disappearing-message window](CLI.md) still applies to their own
copy. A group does not suspend a promise about what this device keeps of a given
peer.

---

## Limits

- **64 members.** A send is 64 dials; a build that let you type a thousand would
  be offering a button that cannot work.
- **No group files yet.** `group send` carries text. Sharing a file with
  everyone means sending it to each member, or using a Space.
- **No read receipts or reactions across a group.** Both exist for one-to-one
  conversations and are not yet carried on the group path.
