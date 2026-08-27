//! `peerbeam trust` — list, approve and revoke the devices this machine trusts.
//!
//! # Pinned is not approved
//!
//! The trust store holds two states and this command exists to keep them apart.
//!
//! A device is **pinned** by the authenticated handshake itself: `auth.rs`
//! records every never-seen peer as it connects, with `approved: false`, so that
//! a later key change is detectable as a possible MITM. A pin is a *memory*, not
//! a decision — every stranger that has ever completed a handshake with this
//! machine is pinned, and nobody chose any of them.
//!
//! A device is **approved** only when a person says so. That is the state
//! `peerbeam_presence`, `peerbeam_clipboard` and `peerbeam_transfer::pipe` gate
//! on (`TrustStore::is_approved`), because each of those sends something
//! outward on the user's behalf without asking again — a battery level and a
//! free-disk reading, whatever was last copied, raw bytes onto a terminal's
//! stdout — and each is only defensible as "my own devices".
//!
//! A device is **permitted** to do particular things. Approving grants the
//! *frozen approval set* — files, chat, clipboard, presence, pipe — and **not**
//! every permission this build has: `notes` and `browse` were added after that
//! set was frozen and stay opt-in, precisely so that a release cannot widen
//! what an unreviewed device may do. `permit` and `revoke-permission` adjust
//! it afterwards, which is how "this laptop may sync files but must never read
//! my clipboard" is expressed, and how a device is granted `browse` so it can
//! list the folders this machine shares. `approve --no-share` grants nothing at
//! all: the key is vouched for and every capability is left to be granted one
//! at a time. Every gate re-reads the store per operation, so a revoke here
//! stops that device's next message, clip, heartbeat or accept — not its next
//! connection.
//!
//! An approval can also be **time-limited**: `approve --for 30m` writes a
//! deadline, and once it passes the device is back to being merely pinned — it
//! may nothing, and `list` says `expired`. Because every gate re-reads the
//! store, that happens on time with nothing running: there is no sweeper, and a
//! sweeper that had not run yet would be a device still trusted after its
//! window closed. The pin survives, so its key is still remembered and a key
//! change is still caught; `revoke` is what forgets a device.
//!
//! # Why this is a CLI command
//!
//! Until now `FsTrust::approve` was reachable only from the app's
//! accept-and-trust prompt. A headless server, a container, or anything driven
//! over SSH could therefore never approve anything, which made those three
//! features unusable on exactly the machines `CLAUDE.md` names as first-class
//! targets. Approving is a decision, not a transfer, so it needs no daemon, no
//! network and no peer online: it edits this machine's own store.
//!
//! # Output
//!
//! Human text on **stdout** (this is not `pipe`; nothing here reserves the
//! stream), errors on stderr, and `--json` emits one object per line so a script
//! can stream it. Exit codes are the CLI's usual ones: `2` for an ambiguous
//! `<device>`, `3` for one that matches nothing, `6` for a declined
//! confirmation.

use chrono::{DateTime, SecondsFormat, Utc};
use serde_json::json;

use peerbeam_domain::entity::{Permission, PermissionSet, TrustRecord};
use peerbeam_domain::id::DeviceId;
use peerbeam_domain::port::TrustStore;

use crate::cli::TrustAction;
use crate::commands::{humantime, load_config, open_trust, parse_duration, resolve_peer};
use crate::exit::{CliError, CliResult};
use crate::output::Ctx;
use crate::prompt;

/// How much of the 64-hex-character fingerprint `trust list` shows.
///
/// A listing is an inventory, not a verification: 16 characters is far more
/// than enough to tell two devices apart at a glance, and the full value is one
/// `--json` away and is printed in full by `approve`, which is the command
/// where an operator is actually being asked to vouch for a key. `doctor`
/// abbreviates the same way.
const FP_PREVIEW: usize = 16;

/// Dispatch `peerbeam trust`.
pub fn trust(ctx: &Ctx, action: TrustAction, path_override: Option<&str>) -> CliResult {
    match action {
        TrustAction::List => list(ctx, path_override),
        TrustAction::Approve {
            device,
            duration,
            no_share,
        } => approve(ctx, &device, duration.as_deref(), !no_share, path_override),
        TrustAction::AutoAccept { device, no } => set_auto_accept(ctx, &device, !no, path_override),
        TrustAction::Revoke { device } => revoke(ctx, &device, path_override),
        TrustAction::Permit {
            device,
            permissions,
        } => set_permissions(ctx, &device, &permissions, true, path_override),
        TrustAction::RevokePermission {
            device,
            permissions,
        } => set_permissions(ctx, &device, &permissions, false, path_override),
        TrustAction::Mine { device, no } => set_mine(ctx, &device, !no, path_override),
        TrustAction::MyDevices => my_devices(ctx, path_override),
    }
}

// ── mine ────────────────────────────────────────────────────────────────────

/// `peerbeam trust mine <DEVICE> [--no]`.
///
/// **A label, not a grant.** Marking a device as yours changes no permission,
/// no approval, and nothing the device itself can observe — it is a note this
/// machine keeps so it can answer "which of these are mine". The engine
/// enforces that: `mine` is not among the fields `effective_permissions_at`
/// reads.
fn set_mine(ctx: &Ctx, device: &str, mine: bool, path_override: Option<&str>) -> CliResult {
    let config = load_config(path_override)?;
    let store = open_trust(&config)?;
    let id = DeviceId::from(device.to_string());

    let changed = store
        .set_mine(&id, mine)
        .map_err(|e| CliError::Other(e.to_string()))?;

    if ctx.json {
        ctx.json_line(&serde_json::json!({
            "event": "trust_mine",
            "device": device,
            "mine": mine,
            "changed": changed,
        }));
        return Ok(());
    }
    // A device that was never pinned cannot be marked: there is no record to
    // write the label on, and inventing one would pin a key nobody presented.
    if !changed && !store.is_trusted(&id) {
        return Err(CliError::NotFound(format!(
            "no device {device} on this machine — it is marked as yours only after it has connected once"
        )));
    }
    ctx.line(&if mine {
        format!("{device} is one of yours")
    } else {
        format!("{device} is no longer marked as yours")
    });
    Ok(())
}

/// `peerbeam trust auto-accept <DEVICE> [--no]`.
///
/// **Not a permission.** This decides whether the user is *asked* about this
/// device's files, never whether the device is *allowed* to send them: the
/// admission gate consults it only after the `files` permission has already
/// said yes. Setting it on a device that may not send files is inert, and it
/// is reported as such rather than silently succeeding into nothing.
fn set_auto_accept(
    ctx: &Ctx,
    device: &str,
    auto_accept: bool,
    path_override: Option<&str>,
) -> CliResult {
    let config = load_config(path_override)?;
    let store = open_trust(&config)?;
    let records = store.list();
    let record = pick(ctx, &records, device)?;
    let id = record.device.clone();

    let changed = store
        .set_auto_accept(&id, auto_accept)
        .map_err(|e| CliError::Other(e.to_string()))?;

    if ctx.json {
        ctx.json_line(&json!({
            "event": "trust_auto_accept",
            "device": id.0,
            "auto_accept": auto_accept,
            "changed": changed,
            "effective": store.auto_accepts(&id),
        }));
        return Ok(());
    }

    let name = if record.name.is_empty() {
        &id.0
    } else {
        &record.name
    };
    ctx.line(&if auto_accept {
        format!("{name}'s files will be accepted without asking")
    } else {
        format!("{name}'s files will be asked about")
    });
    // Say so rather than leaving the operator believing a setting is in force.
    // `auto_accepts` is the *effective* answer: it reads approval and expiry
    // too, so this catches both "never approved" and "the window closed".
    if auto_accept && !store.auto_accepts(&id) {
        ctx.line(&ctx.dim(
            "  ...but it is not approved (or its approval has expired), so it \
             may send nothing and this has no effect yet",
        ));
    }
    Ok(())
}

/// `peerbeam trust my-devices` — the machines the user calls their own.
fn my_devices(ctx: &Ctx, path_override: Option<&str>) -> CliResult {
    let config = load_config(path_override)?;
    let devices = open_trust(&config)?
        .my_devices()
        .map_err(|e| CliError::Other(e.to_string()))?;

    if ctx.json {
        for d in &devices {
            ctx.json_line(&serde_json::json!({
                "event": "my_device",
                "device": d.device.0,
                "name": d.name,
            }));
        }
        return Ok(());
    }
    if devices.is_empty() {
        ctx.line(&ctx.dim(
            "none marked yet — `peerbeam trust mine <device>` marks one, and marks nothing else",
        ));
        return Ok(());
    }
    for d in &devices {
        ctx.line(&format!("{}  {}", ctx.dim(&d.device.0), d.name));
    }
    Ok(())
}

// ── list ────────────────────────────────────────────────────────────────────

/// `peerbeam trust list` — every record, and which of the two states it is in.
fn list(ctx: &Ctx, path_override: Option<&str>) -> CliResult {
    let config = load_config(path_override)?;
    let records = open_trust(&config)?.list();
    // **One clock read for the whole listing.** Asking `Utc::now()` per row
    // would let a window close between two rows and report two devices as of
    // two different presents — a table that contradicts itself, on the one
    // command whose job is to say what is true right now.
    let now = Utc::now();

    if ctx.json {
        for r in &records {
            ctx.json_line(&row_json(r, now));
        }
        return Ok(());
    }

    if records.is_empty() {
        ctx.line(&ctx.dim("no devices pinned — a device is pinned the first time it connects"));
        return Ok(());
    }

    // The status is a **word**, not a colour, and the cell is left uncoloured
    // deliberately: `Ctx::table` sizes its columns with `str::len`, so an ANSI
    // sequence in a cell counts toward the width and silently shifts every
    // column after it. Painting this green/yellow would trade the alignment
    // that makes a long list scannable for a cue that `--no-color`, a pipe, a
    // dumb terminal and a screen reader all discard anyway. "approved" versus
    // "pinned" is legible without either.
    let rows: Vec<Vec<String>> = records.iter().map(|r| row_cells(r, now)).collect();
    ctx.table(
        &[
            "STATUS",
            "DEVICE",
            "NAME",
            "FINGERPRINT",
            "PINNED",
            "EXPIRES",
            "PERMISSIONS",
        ],
        &rows,
    );

    // Said only when it applies, so it reads as a fact about *these* devices
    // rather than as boilerplate: a store where everything is approved needs no
    // explanation, and one holding strangers very much does.
    if records.iter().any(|r| !r.approved) {
        ctx.line("");
        ctx.line(&ctx.dim(
            "`pinned` means only that this device's key was recorded when it first \
             connected.\nA device receives this machine's presence status, clipboard \
             or a pipe only once\napproved: `peerbeam trust approve <device>`.",
        ));
    }
    // Same rule again: only when a window has actually run out. It explains the
    // half of `expired` that is easy to get wrong — the device is *not* gone,
    // its key is still pinned, so this is not a device to re-verify from
    // scratch, it is one to approve again.
    if records.iter().any(|r| r.approved && r.has_expired(now)) {
        ctx.line("");
        ctx.line(&ctx.dim(
            "`expired` means a time-limited approval ran out; the device may nothing \
             until it is\napproved again with `peerbeam trust approve <device>`. Its key \
             is still pinned, so\na key change is still caught — `trust revoke` is what \
             forgets a device.",
        ));
    }
    // Said only when a device has actually been narrowed, so it reads as a fact
    // about *these* devices rather than as boilerplate. Approving grants every
    // permission, so an untouched store never sees this.
    if records
        .iter()
        .any(|r| r.approved && r.permissions != PermissionSet::granted_on_approval())
    {
        ctx.line("");
        ctx.line(&ctx.dim(
            "`permissions` lists what each device may do. Change one with \
             `peerbeam trust permit <device>\n<permission>…` or `peerbeam trust \
             revoke-permission <device> <permission>…`; it applies to that\ndevice's \
             next operation, not its next connection.",
        ));
    }
    Ok(())
}

/// One `--json` row. `approved` is an explicit bool — never inferred from the
/// presence of the record, which is the confusion the whole command corrects —
/// and the fingerprint is the full 64 characters, because a script comparing
/// keys across two machines needs all of it.
///
/// `permissions` is an explicit **array of names**, for the same reason
/// `approved` is an explicit bool: a script must be able to ask "may this device
/// read my clipboard?" without knowing which permissions this build happens to
/// have, and an array it can `contains` is that question. It is emitted even
/// when empty, so an absent key never has to be interpreted.
///
/// `approved` is the **effective** answer, as of `now`: a device whose window
/// has closed reports `false`, with `expired: true` and `expires_at` saying why.
/// The alternative — reporting the stored bit — would leave the documented
/// one-liner `jq 'select(.approved | not)'` quietly ignoring exactly the devices
/// this feature exists to catch, and would disagree with `permissions` on the
/// same row, which has always been the effective set.
///
/// Field names match the FFI's `pb_trust_list` (`id`/`name`/`fingerprint`/
/// `trusted_at`/`expires_at`) so tooling can read either surface uniformly.
fn row_json(r: &TrustRecord, now: DateTime<Utc>) -> serde_json::Value {
    json!({
        "id": r.device.0,
        "name": r.name,
        "fingerprint": r.fingerprint,
        "trusted_at": r.trusted_at.to_rfc3339_opts(SecondsFormat::Secs, true),
        "approved": r.is_approved_at(now),
        "expires_at": r
            .expires_at
            .map(|at| at.to_rfc3339_opts(SecondsFormat::Secs, true)),
        "expired": r.approved && r.has_expired(now),
        "permissions": r
            .effective_permissions_at(now)
            .granted()
            .into_iter()
            .map(|p| p.as_str())
            .collect::<Vec<_>>(),
    })
}

/// One human table row: status first, because it is the column the command
/// exists to show.
fn row_cells(r: &TrustRecord, now: DateTime<Utc>) -> Vec<String> {
    vec![
        status_word(r, now).to_string(),
        r.device.0.clone(),
        r.name.clone(),
        short_fingerprint(&r.fingerprint),
        r.trusted_at.format("%Y-%m-%d %H:%M").to_string(),
        expiry_cell(r, now),
        permission_cell(r, now),
    ]
}

/// The permissions column: the granted names, in slot order, or the word
/// `none`.
///
/// Names rather than an `ls -l`-style flag string: `files` and `pipe` and
/// `presence` do not abbreviate to distinct memorable letters, and a legend
/// nobody can read from the row itself is worse than a wide column. It is the
/// **last** column so its variable width disturbs nothing to its left, and
/// `none` is a word rather than a dash because a dash reads as "unknown" and
/// this is a fact.
fn permission_cell(r: &TrustRecord, now: DateTime<Utc>) -> String {
    let granted = r.effective_permissions_at(now).granted();
    if granted.is_empty() {
        return "none".to_string();
    }
    granted
        .into_iter()
        .map(Permission::as_str)
        .collect::<Vec<_>>()
        .join(",")
}

/// The three states, named.
///
/// `expired` is its own word rather than a flavour of `pinned`, because the two
/// need different things said about them and have different fixes: a stranger
/// nobody chose is a device to look at, while a device whose half-hour ran out
/// is one somebody already vouched for and can simply approve again.
fn status_word(r: &TrustRecord, now: DateTime<Utc>) -> &'static str {
    if r.is_approved_at(now) {
        "approved"
    } else if r.approved {
        "expired"
    } else {
        "pinned"
    }
}

/// The `EXPIRES` cell: `never`, `in 29m`, or `12m ago`.
///
/// **Relative, not a timestamp.** The question a person runs this to answer is
/// "how long have I got"; `2026-08-19 11:00` makes them do the subtraction, and
/// do it in whatever timezone they guess the column is in. The absolute instant
/// is in `--json`, where a script wants it and a clock is not being read aloud.
///
/// Rendered for every row, including the ones with no deadline: a column that
/// appeared only when some device happened to be time-limited would change the
/// table's shape depending on its contents, and `never` is a fact worth stating
/// on a screen about how long trust lasts.
fn expiry_cell(r: &TrustRecord, now: DateTime<Utc>) -> String {
    let Some(deadline) = r.expires_at else {
        return "never".to_string();
    };
    let delta = deadline - now;
    let span = humantime(std::time::Duration::from_secs(
        delta.num_seconds().unsigned_abs(),
    ));
    if r.has_expired(now) {
        format!("{span} ago")
    } else {
        format!("in {span}")
    }
}

/// Abbreviate a fingerprint for a listing; see [`FP_PREVIEW`]. Short values are
/// returned whole rather than padded, so a malformed or legacy record is shown
/// as it actually is instead of being made to look like a truncation.
fn short_fingerprint(fp: &str) -> String {
    match fp.char_indices().nth(FP_PREVIEW) {
        Some((cut, _)) => format!("{}…", &fp[..cut]),
        None => fp.to_string(),
    }
}

// ── approve ─────────────────────────────────────────────────────────────────

/// Whether an `approve` may go ahead.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalGate {
    /// Confirmed, or explicitly declared non-interactive.
    Proceed,
    /// Declined, or unanswerable — do not approve.
    Abort,
}

/// Decide whether `trust approve` may proceed, given how the command was
/// invoked and what (if anything) the operator answered.
///
/// The same shape as `commands::pairing_gate`, and for the same reason: the
/// decision is a pure function of its inputs, so it can be tested without a
/// terminal, and there is one confirmation idiom in this CLI rather than two.
///
/// * `assume_yes` — `--yes`. The operator has said in advance that nobody is at
///   the terminal. This is what makes the command scriptable, which is the
///   point of adding it: a headless box provisioning itself cannot answer a
///   prompt.
/// * `json` — `--json` is non-interactive by construction ([`Ctx::new`] clears
///   `interactive` for it), and a machine consuming NDJSON has no way to reply.
///   Asking would be a hang, not a safeguard.
/// * `answer` — `Some(true)`/`Some(false)` for an explicit reply, or `None`
///   when no confirmation could be obtained (no TTY, redirected stdin, EOF).
///
/// `None` is a **decline**, exactly as `commands::pairing_gate` treats it: a
/// question nobody answered is not consent, and approval is the act that opens
/// this machine's clipboard to another device.
pub fn approval_gate(assume_yes: bool, json: bool, answer: Option<bool>) -> ApprovalGate {
    if assume_yes || json {
        return ApprovalGate::Proceed;
    }
    match answer {
        Some(true) => ApprovalGate::Proceed,
        _ => ApprovalGate::Abort,
    }
}

/// The question an interactive `approve` asks — **fingerprint first**.
///
/// The fingerprint is in the prompt rather than printed above it because
/// `prompt::confirm` writes its question unconditionally while [`Ctx::line`]
/// honours `--quiet`. Putting it here means there is no combination of flags
/// under which someone is asked to approve a key they were not shown.
///
/// It also says what approval *does*, and for **how long**. "Trust this
/// device?" invites yes; naming the clipboard invites a look at the hex, and
/// naming the deadline — or its absence — is the difference between the two
/// grants this command can write.
pub fn approval_question(
    record: &TrustRecord,
    expires_at: Option<DateTime<Utc>>,
    share: bool,
) -> String {
    let window = match expires_at {
        None => "until revoked".to_string(),
        Some(at) => format!(
            "until {}, after which it may nothing again",
            at.to_rfc3339_opts(SecondsFormat::Secs, true)
        ),
    };
    // This used to end "— every permission", which was wrong in two directions
    // at once. It is not every permission: `notes` and `browse` were added
    // after `granted_on_approval` was frozen and are not granted here, so a
    // reader who approved a device and then found it could not list their
    // shared folders had been told otherwise. And with `--no-share` nothing at
    // all is granted. A confirmation prompt that overstates what it is about to
    // do is the one kind of copy a security control must never carry.
    let grants = if share {
        "Approving lets this device receive this machine's presence status, clipboard\nand \
         pipes, and exchange files and messages with it. It does **not** grant `notes`\nor \
         `browse` — those stay opt-in via `trust grant-permission`. Narrow the rest\nwith \
         `trust revoke-permission`."
    } else {
        "Approving vouches for this device's key and grants it **nothing**: it stops\ncounting \
         as a stranger, and may do nothing at all until you grant a permission\nwith `trust \
         grant-permission`."
    };
    format!(
        "  fingerprint  {}\n  pinned       {}\n  for          {}\n{}\nApprove {} ({})?",
        record.fingerprint,
        record.trusted_at.to_rfc3339_opts(SecondsFormat::Secs, true),
        window,
        grants,
        record.name,
        record.device.0,
    )
}

/// `peerbeam trust approve <device> [--for DURATION]` — grant a pinned device
/// standing, for a while or until it is revoked.
fn approve(
    ctx: &Ctx,
    query: &str,
    window: Option<&str>,
    share: bool,
    path_override: Option<&str>,
) -> CliResult {
    // Parsed before the store is opened or a device resolved, so `--for 30mn`
    // fails as a usage error immediately rather than after a prompt has been
    // answered — the same rule `permit` follows for permission names.
    let window = window.map(parse_duration).transpose()?;
    let config = load_config(path_override)?;
    let store = open_trust(&config)?;
    let records = store.list();
    let record = pick(ctx, &records, query)?;

    let now = Utc::now();
    // An **absolute deadline**, computed once here. Storing "30 minutes from
    // whenever you read this" would restart the window on every read; storing
    // the instant means a machine asleep through the whole window wakes with it
    // shut.
    let expires_at =
        match window {
            None => None,
            Some(w) => Some(now.checked_add_signed(w).ok_or_else(|| {
                CliError::Usage("that window ends past the end of time".to_string())
            })?),
        };

    // Already exactly what was asked for: say so and succeed. Re-running a
    // provisioning script must not fail, and an error here would say something
    // untrue — the state the operator asked for is the state on disk.
    //
    // The window is part of "what was asked for", so this is deliberately
    // narrower than `record.approved`: a device that is approved-but-expired, or
    // approved-until-11:00 when the operator just asked for indefinitely, is not
    // in the state they requested and must fall through and be written.
    if record.is_approved_at(now) && record.expires_at.is_none() && expires_at.is_none() {
        return report(ctx, record, "already approved", false, now);
    }

    // With PIN pairing required, approving from here would be a lie. A PIN
    // proves who is on the other end of a *live handshake* — it signs that
    // handshake's transcript — and this command has no handshake, only a record
    // on disk. Approving anyway would satisfy the setting's letter while
    // proving nothing, which is worse than refusing: the operator would believe
    // a check had happened.
    //
    // Asked of the *live* standing, not the stored bit: renewing a device whose
    // window has closed is granting standing again, and must go through the same
    // proof the first grant did. Narrowing a window on a device that still holds
    // standing is not, so it stays allowed.
    if config.encryption.require_pin_pairing && !record.is_approved_at(now) {
        return Err(CliError::Usage(format!(
            "{} requires PIN pairing, which needs a live connection — run \
             `peerbeam pair {}` instead",
            if record.name.is_empty() {
                &record.device.0
            } else {
                &record.name
            },
            record.device.0
        )));
    }

    // Confirmation is for **granting** standing, which is the direction that
    // cannot be taken back once a clipboard has crossed. A device that already
    // holds it is only having its window rewritten — usually shortened — and
    // that is no more dangerous than `revoke-permission`, which asks nothing
    // either. Prompting there would also make `trust approve x --for 30m` in a
    // cron job need `--yes` to shorten a window it is trying to shorten.
    let granting = !record.is_approved_at(now);
    if granting {
        let answer = if ctx.interactive {
            Some(prompt::confirm(
                ctx,
                &approval_question(record, expires_at, share),
                false,
            ))
        } else {
            None
        };
        if approval_gate(ctx.assume_yes, ctx.json, answer) == ApprovalGate::Abort {
            return Err(CliError::Cancelled);
        }
    }

    // `approve_with`, so `--no-share` can ask for the empty set. It writes the
    // permissions only on the transition to approved, which is why passing
    // `none()` at a device that is already approved cannot strip it — see the
    // flag's own documentation.
    let grant = if share {
        PermissionSet::granted_on_approval()
    } else {
        PermissionSet::none()
    };
    store.approve_with(&record.device, true, expires_at, grant)?;
    // Report the store, not the request. `FsTrust::approve` is documented as a
    // silent no-op for a device it does not hold — which is exactly what a
    // concurrent `trust revoke` in another process leaves behind — so without
    // this read-back that case would print "approved" over a device that is not.
    // A receipt for something that did not happen is worse than an error.
    let stored = store
        .lookup(&record.device)
        .map_err(CliError::from)?
        .ok_or_else(|| {
            CliError::NotFound(format!(
                "device {} was revoked while approving",
                record.device.0
            ))
        })?;
    if !stored.is_approved_at(now) {
        return Err(CliError::Other(format!(
            "{} is still not approved after approving it",
            stored.device.0
        )));
    }
    report(ctx, &stored, "approved", true, now)
}

/// The receipt for an `approve`. Carries the fingerprint in every mode — a
/// scripted run never sees the prompt, so this is where its record of *what*
/// was approved comes from — and the deadline whenever there is one, because
/// *for how long* is the other half of what was just granted.
///
/// The window is named only when the grant has one, following the same rule as
/// this command's footnotes: a store where nothing is time-limited should not
/// have to read the word.
fn report(
    ctx: &Ctx,
    record: &TrustRecord,
    verb: &str,
    changed: bool,
    now: DateTime<Utc>,
) -> CliResult {
    if ctx.json {
        let mut value = row_json(record, now);
        if let Some(obj) = value.as_object_mut() {
            obj.insert("event".into(), json!("trust_approved"));
            obj.insert("changed".into(), json!(changed));
        }
        ctx.json_line(&value);
        return Ok(());
    }
    let window = match record.expires_at {
        None => String::new(),
        Some(at) => format!(
            " {} — until {}",
            expiry_cell(record, now),
            at.to_rfc3339_opts(SecondsFormat::Secs, true)
        ),
    };
    ctx.line(&format!(
        "{} {} ({}){window} — fingerprint {}",
        ctx.green(verb),
        record.name,
        record.device.0,
        record.fingerprint
    ));
    Ok(())
}

// ── revoke ──────────────────────────────────────────────────────────────────

/// `peerbeam trust revoke <device>` — forget a device entirely.
///
/// Removes the record, not just the approval, so the next connection is a fresh
/// first contact: re-pinned, and unapproved until someone says otherwise. That
/// is stronger than clearing the flag and it is what the app's revoke already
/// does (`FsTrust::remove`), so both surfaces mean the same thing by the word.
///
/// **No confirmation.** Revoking only ever removes standing, so an accidental
/// one costs a re-approval, while an accidental *approval* costs a clipboard.
/// Prompting on the safe direction would also make the obvious incident-response
/// one-liner — revoke everything, then re-approve what you recognise — need a
/// flag to work.
fn revoke(ctx: &Ctx, query: &str, path_override: Option<&str>) -> CliResult {
    let config = load_config(path_override)?;
    let store = open_trust(&config)?;
    let records = store.list();
    let record = pick(ctx, &records, query)?;

    // `pick` already failed for anything absent; a `false` here means the record
    // went away between the two calls (another process revoked it). Report that
    // rather than claiming a removal this process did not make.
    if !store.remove(&record.device).map_err(CliError::from)? {
        return Err(CliError::NotFound(format!(
            "device {} was no longer pinned",
            record.device.0
        )));
    }

    if ctx.json {
        ctx.json_line(&json!({
            "event": "trust_revoked",
            "id": record.device.0,
            "name": record.name,
            "fingerprint": record.fingerprint,
            "removed": true,
        }));
    } else {
        ctx.line(&format!(
            "{} {} ({}) — its next connection is a fresh first contact",
            ctx.yellow("revoked"),
            record.name,
            record.device.0
        ));
    }
    Ok(())
}

// ── permit / revoke-permission ──────────────────────────────────────────────

/// `peerbeam trust permit <device> <permission>…` and its inverse.
///
/// One function for both directions because they differ only in a bool: two
/// would be two places for the resolution, the receipt and the read-back to
/// drift apart, on a command where the two directions must stay exact mirrors.
///
/// **Every name is parsed before anything is written.** A typo in the third of
/// three permissions must not leave the first two applied — a half-applied
/// permission change is the one outcome an operator cannot reason about, and
/// this command is expected to be run from provisioning scripts.
///
/// **No confirmation, in either direction.** `revoke-permission` only removes
/// standing, exactly as `revoke` does; and `permit` can only ever restore
/// something `approve` already granted once, so neither is the irreversible act
/// `approve` is.
fn set_permissions(
    ctx: &Ctx,
    query: &str,
    names: &[String],
    granted: bool,
    path_override: Option<&str>,
) -> CliResult {
    let permissions = parse_permissions(names)?;
    let config = load_config(path_override)?;
    let store = open_trust(&config)?;
    let records = store.list();
    let record = pick(ctx, &records, query)?;
    let now = Utc::now();
    // Permissions narrow a standing; without one there is nothing to narrow, and
    // `TrustStore::may` would answer `false` for every bit written here. Saying
    // so is better than a receipt for a change with no effect — and it stops a
    // "staged" grant that the next `approve` would silently overwrite anyway.
    //
    // A closed window is the same situation reached from the other direction, so
    // it is refused too — but named separately, because the two have different
    // stories and the same fix would otherwise read as a non-sequitur against a
    // device the operator knows perfectly well they approved.
    if !record.is_approved_at(now) {
        return Err(CliError::Usage(if record.approved {
            format!(
                "{}'s approval expired, so it may nothing to begin with — \
                 `peerbeam trust approve {}` first",
                record.device.0, record.device.0
            )
        } else {
            format!(
                "{} is pinned but not approved, so it may nothing to begin with — \
                 `peerbeam trust approve {}` first",
                record.device.0, record.device.0
            )
        }));
    }
    let device = record.device.clone();

    let mut changed = Vec::new();
    for permission in &permissions {
        if store
            .set_permission(&device, *permission, granted)
            .map_err(CliError::from)?
        {
            changed.push(*permission);
        }
    }

    // Report the store, not the request — the same rule `approve` follows. A
    // concurrent `trust revoke` in another process leaves nothing to change, and
    // printing "permitted" over a device that is no longer pinned would be a
    // receipt for something that did not happen.
    let stored = store
        .lookup(&device)
        .map_err(CliError::from)?
        .ok_or_else(|| {
            CliError::NotFound(format!("device {} was revoked while permitting", device.0))
        })?;
    for permission in &permissions {
        if stored.permissions.grants(*permission) != granted {
            return Err(CliError::Other(format!(
                "{} still {} `{permission}` after the change",
                stored.device.0,
                if granted { "lacks" } else { "has" }
            )));
        }
    }

    report_permissions(ctx, &stored, &permissions, granted, &changed, now)
}

/// Parse every name up front, rejecting the whole invocation on the first bad
/// one. Exit `2` (usage), naming what this build actually knows — a permission
/// this engine cannot enforce must not be accepted and quietly dropped.
fn parse_permissions(names: &[String]) -> Result<Vec<Permission>, CliError> {
    names
        .iter()
        .map(|n| {
            Permission::parse(n).ok_or_else(|| {
                CliError::Usage(format!(
                    "unknown permission `{n}` — expected one of: {}",
                    Permission::ALL.map(|p| p.as_str()).join(", ")
                ))
            })
        })
        .collect()
}

/// The receipt. Names the device, what it may do **now** (read back from the
/// store, not assumed), and stays exit `0` when nothing changed so a
/// provisioning script is safe to re-run.
fn report_permissions(
    ctx: &Ctx,
    record: &TrustRecord,
    asked: &[Permission],
    granted: bool,
    changed: &[Permission],
    now: DateTime<Utc>,
) -> CliResult {
    let verb = if granted { "permitted" } else { "revoked" };
    if ctx.json {
        let mut value = row_json(record, now);
        if let Some(obj) = value.as_object_mut() {
            obj.insert("event".into(), json!("trust_permissions_changed"));
            obj.insert("granted".into(), json!(granted));
            obj.insert(
                "requested".into(),
                json!(asked.iter().map(|p| p.as_str()).collect::<Vec<_>>()),
            );
            obj.insert(
                "changed".into(),
                json!(changed.iter().map(|p| p.as_str()).collect::<Vec<_>>()),
            );
        }
        ctx.json_line(&value);
        return Ok(());
    }
    let list = asked
        .iter()
        .map(|p| p.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    let painted = if granted {
        ctx.green(verb)
    } else {
        ctx.yellow(verb)
    };
    ctx.line(&format!(
        "{painted} {list} for {} ({}) — it may now: {}",
        record.name,
        record.device.0,
        permission_cell(record, now)
    ));
    Ok(())
}

// ── shared ──────────────────────────────────────────────────────────────────

/// Resolve a `<device>` argument against the pinned records.
///
/// Reuses the CLI's single device resolver ([`resolve_peer`] over
/// `resolve::resolve`): exact id, then exact name, then unique name prefix,
/// with an ambiguous prefix reported as a usage error naming the candidates
/// instead of guessed at. A second resolver here would drift from `send --to`
/// and let the same string mean different devices in different commands — and
/// on *this* command a wrong guess approves a stranger.
fn pick<'a>(
    ctx: &Ctx,
    records: &'a [TrustRecord],
    query: &str,
) -> Result<&'a TrustRecord, CliError> {
    let candidates: Vec<(String, String)> = records
        .iter()
        .map(|r| (r.device.0.clone(), r.name.clone()))
        .collect();
    let index = resolve_peer(ctx, &candidates, &Some(query.to_string()))?;
    records
        .get(index)
        .ok_or_else(|| CliError::NotFound(format!("device {query}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, TimeZone};
    use peerbeam_domain::id::DeviceId;

    /// A fixed instant to read the records at, so every assertion here is about
    /// the predicate rather than about when the test happened to run.
    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 8, 17, 12, 0, 0)
            .single()
            .unwrap_or_else(Utc::now)
    }

    fn record(id: &str, name: &str, approved: bool) -> TrustRecord {
        permissive(id, name, approved, PermissionSet::granted_on_approval())
    }

    fn permissive(id: &str, name: &str, approved: bool, permissions: PermissionSet) -> TrustRecord {
        TrustRecord {
            device: DeviceId::from(id),
            fingerprint: "a".repeat(64),
            name: name.to_string(),
            trusted_at: Utc
                .with_ymd_and_hms(2026, 8, 17, 10, 30, 0)
                .single()
                .unwrap_or_else(Utc::now),
            approved,
            permissions,
            expires_at: None,
            mine: false,
            auto_accept: false,
        }
    }

    /// An approved device whose window closes `after` [`now`] — negative for one
    /// that has already run out.
    fn expiring(id: &str, name: &str, after: Duration) -> TrustRecord {
        let mut r = record(id, name, true);
        r.expires_at = Some(now() + after);
        r
    }

    #[test]
    fn the_status_word_names_the_three_states() {
        assert_eq!(
            status_word(&record("pb-a", "Laptop", true), now()),
            "approved"
        );
        assert_eq!(
            status_word(&record("pb-b", "Stranger", false), now()),
            "pinned"
        );
        assert_eq!(
            status_word(&expiring("pb-c", "Loaner", Duration::minutes(30)), now()),
            "approved",
            "a window still open is just approved"
        );
        assert_eq!(
            status_word(&expiring("pb-c", "Loaner", Duration::minutes(-1)), now()),
            "expired"
        );
    }

    /// The `--json` row must carry `approved` as a **bool**, not as the mere
    /// presence of the record. A script that filtered on "is it listed?" would
    /// be asking the question this whole change exists to stop asking.
    #[test]
    fn json_rows_carry_approved_as_an_explicit_bool() {
        let approved = row_json(&record("pb-a", "Laptop", true), now());
        assert_eq!(approved["approved"], json!(true));
        assert_eq!(approved["id"], json!("pb-a"));

        let pinned = row_json(&record("pb-b", "Stranger", false), now());
        assert_eq!(pinned["approved"], json!(false));
        assert!(
            pinned["approved"].is_boolean(),
            "a string \"false\" is truthy in most languages a script is written in"
        );
    }

    /// A listing abbreviates; `--json` does not. Comparing keys between two
    /// machines is the reason the field exists at all.
    #[test]
    fn json_carries_the_whole_fingerprint_even_though_the_table_abbreviates() {
        let r = record("pb-a", "Laptop", true);
        assert_eq!(row_json(&r, now())["fingerprint"], json!(r.fingerprint));
        assert_eq!(r.fingerprint.len(), 64);

        let short = short_fingerprint(&r.fingerprint);
        assert_eq!(short, format!("{}…", "a".repeat(FP_PREVIEW)));
        assert!(short.len() < r.fingerprint.len());
    }

    /// A record shorter than the preview is shown whole. Truncating it to
    /// itself and appending "…" would claim there is more to see.
    #[test]
    fn a_short_fingerprint_is_not_dressed_up_as_a_truncation() {
        assert_eq!(short_fingerprint("beef"), "beef");
        assert_eq!(short_fingerprint(""), "");
    }

    /// **The prompt is where the fingerprint is guaranteed to appear.** It is
    /// printed by `prompt::confirm`, which `--quiet` cannot suppress, so no
    /// combination of flags asks anyone to vouch for a key they were not shown.
    #[test]
    fn the_question_shows_the_full_fingerprint_and_what_approval_grants() {
        let r = record("pb-a", "Laptop", false);
        let q = approval_question(&r, None, true);
        assert!(
            q.contains(&r.fingerprint),
            "the fingerprint must be in it: {q}"
        );
        assert!(q.contains("pb-a"), "the id must be in it: {q}");
        assert!(
            q.contains("clipboard"),
            "the question must say what approval grants: {q}"
        );
    }

    /// `--yes` and `--json` are the two ways of saying "nobody is here"; both
    /// proceed, which is what makes the command usable from a provisioning
    /// script on a headless box — the machine this command was added for.
    #[test]
    fn yes_and_json_proceed_without_an_answer() {
        assert_eq!(approval_gate(true, false, None), ApprovalGate::Proceed);
        assert_eq!(approval_gate(false, true, None), ApprovalGate::Proceed);
        assert_eq!(approval_gate(true, true, None), ApprovalGate::Proceed);
    }

    /// An explicit yes proceeds; an explicit no does not.
    #[test]
    fn an_interactive_answer_is_obeyed_in_both_directions() {
        assert_eq!(
            approval_gate(false, false, Some(true)),
            ApprovalGate::Proceed
        );
        assert_eq!(
            approval_gate(false, false, Some(false)),
            ApprovalGate::Abort
        );
    }

    /// **The fail-closed case.** No flags and no answer — a redirected stdin,
    /// no TTY, an EOF — must not approve. Deleting the `matches!` arm's
    /// fallthrough, or defaulting `answer` to true, must make this fail.
    #[test]
    fn an_unanswered_question_is_not_consent() {
        assert_eq!(approval_gate(false, false, None), ApprovalGate::Abort);
    }

    /// The `--json` row must carry `permissions` as an explicit **array**, for
    /// the same reason `approved` is an explicit bool: a script asking "may this
    /// device read my clipboard?" must be able to `contains` the answer rather
    /// than infer it from the row existing.
    #[test]
    fn json_rows_carry_permissions_as_an_explicit_array() {
        let full = row_json(&record("pb-a", "Laptop", true), now());
        assert_eq!(
            full["permissions"],
            json!(["files", "chat", "clipboard", "presence", "pipe"]),
            "in slot order, by name"
        );

        let narrowed = row_json(
            &permissive(
                "pb-a",
                "Laptop",
                true,
                PermissionSet::granted_on_approval().set(Permission::Clipboard, false),
            ),
            now(),
        );
        assert_eq!(
            narrowed["permissions"],
            json!(["files", "chat", "presence", "pipe"])
        );

        let stranger = row_json(
            &permissive("pb-b", "Stranger", false, PermissionSet::none()),
            now(),
        );
        assert_eq!(stranger["permissions"], json!([]));
        assert!(
            stranger["permissions"].is_array(),
            "empty must still be an array — an absent key would have to be guessed at"
        );
    }

    /// The table cell names what the device may do, and says `none` as a word
    /// rather than leaving a blank a reader has to interpret.
    #[test]
    fn the_permission_cell_names_the_grants_or_says_none() {
        assert_eq!(
            permission_cell(&record("pb-a", "Laptop", true), now()),
            "files,chat,clipboard,presence,pipe"
        );
        assert_eq!(
            permission_cell(
                &permissive(
                    "pb-a",
                    "Laptop",
                    true,
                    PermissionSet::granted_on_approval().set(Permission::Pipe, false)
                ),
                now()
            ),
            "files,chat,clipboard,presence"
        );
        assert_eq!(
            permission_cell(
                &permissive("pb-b", "Stranger", false, PermissionSet::none()),
                now()
            ),
            "none"
        );
    }

    /// **Every name is parsed before anything is written.** A typo in the last
    /// of three must fail the whole invocation, because a half-applied
    /// permission change is the one outcome an operator cannot reason about.
    #[test]
    fn a_single_bad_permission_name_rejects_the_whole_invocation() {
        let good = parse_permissions(&["files".into(), "clipboard".into()]).unwrap();
        assert_eq!(good, vec![Permission::Files, Permission::Clipboard]);

        let bad = parse_permissions(&["files".into(), "clip".into(), "pipe".into()])
            .expect_err("a typo must not be accepted");
        assert_eq!(bad.code(), 2, "unknown permission is a usage error");
        let message = bad.to_string();
        assert!(message.contains("clip"), "it must name the typo: {message}");
        assert!(
            message.contains("clipboard"),
            "and list what is valid: {message}"
        );
    }

    /// Case is forgiving; spelling is not.
    #[test]
    fn permission_names_parse_case_insensitively() {
        assert_eq!(
            parse_permissions(&["FILES".into(), "Chat".into()]).unwrap(),
            vec![Permission::Files, Permission::Chat]
        );
    }

    /// The approval prompt must say what it grants — and must not overstate it.
    ///
    /// This test previously asserted the prompt said "every permission", which
    /// was false in this build: `notes` and `browse` were added after
    /// `granted_on_approval` was frozen and are not granted by approving. A
    /// user who read that sentence, approved a device and then found it could
    /// not list their shared folders had been told the opposite of the truth by
    /// a confirmation prompt — the one place overstating is least excusable.
    #[test]
    fn the_question_names_what_approval_grants_without_overstating_it() {
        let q = approval_question(&record("pb-a", "Laptop", false), None, true);
        assert!(
            !q.contains("every permission"),
            "approval does not grant every permission this build has: {q}"
        );
        for named in ["presence", "clipboard", "pipes", "files", "messages"] {
            assert!(q.contains(named), "the question does not name {named}: {q}");
        }
        assert!(
            q.contains("browse") && q.contains("notes"),
            "it must say which permissions it does NOT grant: {q}"
        );
        assert!(q.contains("revoke-permission"), "and how to narrow it: {q}");
    }

    /// The `--no-share` prompt must promise nothing, in as many words.
    #[test]
    fn the_no_share_question_says_it_grants_nothing() {
        let q = approval_question(&record("pb-a", "Laptop", false), None, false);
        assert!(
            q.contains("nothing"),
            "trust-without-sharing must say it grants nothing: {q}"
        );
        assert!(
            q.contains("grant-permission"),
            "and how to grant something later: {q}"
        );
        // It must not read like the sharing variant: someone skimming two
        // near-identical prompts is exactly who this distinction is for.
        assert!(
            !q.contains("exchange files and messages"),
            "the no-share prompt describes powers it is not granting: {q}"
        );
    }

    // ── time-limited trust ──────────────────────────────────────────────────

    /// **An expired device is visibly expired, never silently absent.** The row
    /// is still rendered — the device is still pinned, and hiding it would make
    /// a store look like it had lost a machine — but every cell that speaks for
    /// it says the grant is gone.
    #[test]
    fn an_expired_row_reads_as_expired_and_grants_nothing() {
        let r = expiring("pb-c", "Loaner", Duration::minutes(-12));
        let cells = row_cells(&r, now());

        assert_eq!(cells[0], "expired", "the status column says so");
        assert_eq!(cells[1], "pb-c", "and the device is still listed");
        assert_eq!(cells[5], "12m ago", "and says when it lapsed");
        assert_eq!(
            cells[6], "none",
            "and it may nothing, however its stored bits read"
        );
        assert_eq!(
            r.permissions,
            PermissionSet::granted_on_approval(),
            "precondition: the stored grant is untouched, so `none` is the \
             predicate's doing and not the record's"
        );
    }

    /// The `EXPIRES` cell answers "how long have I got" in the CLI's own
    /// vocabulary, in both directions, and says `never` for the ordinary case.
    #[test]
    fn the_expiry_cell_counts_toward_the_deadline_and_away_from_it() {
        assert_eq!(expiry_cell(&record("pb-a", "Laptop", true), now()), "never");
        assert_eq!(
            expiry_cell(&expiring("pb-c", "Loaner", Duration::minutes(29)), now()),
            "in 29m"
        );
        assert_eq!(
            expiry_cell(&expiring("pb-c", "Loaner", Duration::hours(6)), now()),
            "in 6h00m"
        );
        assert_eq!(
            expiry_cell(&expiring("pb-c", "Loaner", Duration::days(7)), now()),
            "in 7d00h",
            "a week must not be printed as 168h00m"
        );
        assert_eq!(
            expiry_cell(&expiring("pb-c", "Loaner", Duration::minutes(-90)), now()),
            "1h30m ago"
        );
        assert_eq!(
            expiry_cell(&expiring("pb-c", "Loaner", Duration::zero()), now()),
            "0s ago",
            "the deadline instant is already past — `>=`, not `>`"
        );
    }

    /// **The `--json` row fails closed.** `approved` is the effective answer, so
    /// the documented `select(.approved | not)` one-liner catches a device whose
    /// window has closed rather than skipping exactly the case that matters;
    /// `expired` and `expires_at` are there to say why, and to tell an expired
    /// device apart from a stranger nobody ever chose.
    #[test]
    fn json_rows_report_expiry_and_report_it_fail_closed() {
        let live = row_json(&expiring("pb-c", "Loaner", Duration::minutes(30)), now());
        assert_eq!(live["approved"], json!(true));
        assert_eq!(live["expired"], json!(false));
        assert_eq!(live["expires_at"], json!("2026-08-17T12:30:00Z"));
        assert_eq!(
            live["permissions"],
            json!(["files", "chat", "clipboard", "presence", "pipe"])
        );

        let lapsed = row_json(&expiring("pb-c", "Loaner", Duration::minutes(-1)), now());
        assert_eq!(
            lapsed["approved"],
            json!(false),
            "an expired device must not read as approved"
        );
        assert_eq!(lapsed["expired"], json!(true));
        assert_eq!(lapsed["permissions"], json!([]));

        let stranger = row_json(&record("pb-b", "Stranger", false), now());
        assert_eq!(stranger["approved"], json!(false));
        assert_eq!(
            stranger["expired"],
            json!(false),
            "never approved is not the same as expired, and a script must be \
             able to tell them apart"
        );
        assert_eq!(stranger["expires_at"], json!(null));
    }

    /// A device with no deadline is untouched by any of this — the ordinary
    /// case, and the one every store written before this feature is in.
    #[test]
    fn a_record_with_no_window_reads_exactly_as_it_did_before() {
        let r = record("pb-a", "Laptop", true);
        assert_eq!(status_word(&r, now()), "approved");
        assert_eq!(expiry_cell(&r, now()), "never");
        assert_eq!(row_json(&r, now())["expires_at"], json!(null));
        assert_eq!(row_json(&r, now())["expired"], json!(false));
        // ...and it is still approved at an instant far beyond any window a
        // person would have set.
        let far = Utc.with_ymd_and_hms(2099, 1, 1, 0, 0, 0).single().unwrap();
        assert_eq!(status_word(&r, far), "approved");
        assert_eq!(
            permission_cell(&r, far),
            "files,chat,clipboard,presence,pipe"
        );
    }

    /// The prompt says **how long**, in both directions. An operator answering
    /// "y" to a 30-minute loan and an operator answering "y" to permanent
    /// access must not be shown the same question.
    #[test]
    fn the_question_names_the_window_or_says_there_is_none() {
        let r = record("pb-a", "Laptop", false);

        let forever = approval_question(&r, None, true);
        assert!(
            forever.contains("until revoked"),
            "an unlimited grant must say so: {forever}"
        );

        let deadline = now() + Duration::minutes(30);
        let limited = approval_question(&r, Some(deadline), true);
        assert!(
            limited.contains("2026-08-17T12:30:00Z"),
            "a limited grant must name the instant it ends: {limited}"
        );
        assert!(
            limited.contains("may nothing again"),
            "and say what happens then: {limited}"
        );
    }
}
