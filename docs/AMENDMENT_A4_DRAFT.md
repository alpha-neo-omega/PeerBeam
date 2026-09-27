# DRAFT — Amendment A4

> **Status: NOT ADOPTED. This file is a proposal, not an authority.**
>
> If approved, the `### A4` section below moves verbatim into
> [ARCHITECTURAL_INVARIANTS.md](ARCHITECTURAL_INVARIANTS.md#amendments), a
> revision note is added to A3's condition 2 pointing at it, and this file is
> deleted. No code may be written against this draft while it is a draft.

---

### A4 — A compiled-in allowlist of redirect hosts (proposed)

**Invariant amended:** **A3's binding condition 2**, whose text reads: *"HTTP
redirects that leave the compiled-in host are refused rather than followed."*
A3 states that all eight of its conditions hold together, so amending one
reopens **I4 — No mandatory cloud, no account, no tracking**, exactly as
amending A1's condition 3 did.

**The conflict.** The clause is unsatisfiable against the place the artifacts
actually live. A GitHub release asset answers `302` to a different host —
verified against the live v0.12.0 release, where all four assets checked
redirect to `release-assets.githubusercontent.com`. So the clause as written
forbids downloading from GitHub at all.

This is a defect in A3 rather than a discovery about GitHub. The condition was
drafted without checking GitHub's redirect behaviour, and adopted with that
error in it.

**Why not simply host the files elsewhere.** That was tried first, and it is
the honest alternative:

* **Cloudflare R2** satisfies condition 2 unchanged and was the original plan.
  It requires completing a subscription checkout, and the project would take on
  a credential in CI, a storage quota, and a pruning chore. Rejected on cost
  and on adding infrastructure to maintain.
* **Cloudflare Pages** cannot hold the files: its per-file limit is 25 MiB and
  the macOS DMG is 33.4 MiB.
* **GitHub Pages** serves from one host and would satisfy condition 2, but the
  artifacts must be committed to a repository, where git keeps them forever —
  a 1 GB site limit against ~154 MiB a release, and manual pruning that never
  fully reclaims the space.
* **A machine of the maintainer's own** was tested end to end over Tailscale
  with a real certificate, and works. It is not reachable by users, and making
  it so would mean a public address, a domain, a certificate, and an uptime
  commitment on a home connection.

**Rationale for amending rather than refusing.** Condition 2 states its own
purpose: *"A served document must never be able to choose what gets written to
a user's disk — this is the property A1 protected by ignoring the manifest's
URL field, and it is the single most important line in this amendment."*

**An allowlist preserves that property exactly.** The set of acceptable hosts
is a literal in the binary. Nothing served is consulted about where to go; a
response can only move the fetch between destinations the build already
trusted before it made the request. What changes is the size of that set, from
one host to two — and the second is the host the first redirects to, operated
by the same party, already trusted to serve the bytes.

It is also worth being clear about which condition is load-bearing. **Condition
3 — the signature — is what protects the user.** An attacker who could redirect
the fetch still cannot produce an artifact that verifies. Host pinning is
defence in depth against a party who has already lost, and A3's own text says
the signature check is what "makes the other two links mean anything".

**What A4 permits.** Following an HTTP redirect to a host named in an allowlist
compiled into the binary, instead of only to the single host the request began
at.

**Binding conditions.** All hold together; a build that drops any one is
outside A4 and back in conflict with A3 and I4.

1. **The allowlist is a literal in source.** Not configuration, not an
   environment variable, not read from any file, and never anything a server
   said. A list that can be edited at runtime by whoever can write a config
   file is not a pin.

2. **It contains only hosts observed to be required**, and today that is
   exactly two: `github.com`, where the request starts, and
   `release-assets.githubusercontent.com`, where it is sent. A host is added by
   a commit citing the redirect that required it — never pre-emptively, and
   never a wildcard. `*.githubusercontent.com` would be a different and much
   larger permission than this one.

3. **Every hop is HTTPS.** A redirect that downgrades the scheme is refused
   even when its host is on the list. (Already enforced; stated so that
   loosening the host rule cannot be read as loosening this one.)

4. **Hops are bounded**, and a redirect to a host not on the list is refused
   with an error that says so distinctly — never folded into a generic network
   failure. A user whose download was diverted and one who is offline must not
   read the same message.

5. **Everything else in A3 is unchanged.** The checksums and their signature
   are still fetched first and the signature still verified before a single
   artifact byte is requested; the artifact is still verified against the
   signed digest before it is usable; there is still no override, no install,
   no execution, and no elevation.

**Approval:** *pending — not granted.*

**Scope.** A4 covers the release download's redirect handling and nothing else.
It is not a precedent for following redirects anywhere else, for trusting a
host because it is adjacent to one already trusted, or for any wildcard. It
does not widen what A3 permits to be fetched, only which hosts may serve it.
