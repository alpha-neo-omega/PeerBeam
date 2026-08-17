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

use chrono::SecondsFormat;
use serde_json::json;

use peerbeam_domain::entity::TrustRecord;
use peerbeam_domain::port::TrustStore;

use crate::cli::TrustAction;
use crate::commands::{load_config, open_trust, resolve_peer};
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
        TrustAction::Approve { device } => approve(ctx, &device, path_override),
        TrustAction::Revoke { device } => revoke(ctx, &device, path_override),
    }
}

// ── list ────────────────────────────────────────────────────────────────────

/// `peerbeam trust list` — every record, and which of the two states it is in.
fn list(ctx: &Ctx, path_override: Option<&str>) -> CliResult {
    let config = load_config(path_override)?;
    let records = open_trust(&config)?.list();

    if ctx.json {
        for r in &records {
            ctx.json_line(&row_json(r));
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
    let rows: Vec<Vec<String>> = records.iter().map(row_cells).collect();
    ctx.table(
        &["STATUS", "DEVICE", "NAME", "FINGERPRINT", "PINNED"],
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
    Ok(())
}

/// One `--json` row. `approved` is an explicit bool — never inferred from the
/// presence of the record, which is the confusion the whole command corrects —
/// and the fingerprint is the full 64 characters, because a script comparing
/// keys across two machines needs all of it.
///
/// Field names match the FFI's `pb_trust_list` (`id`/`name`/`fingerprint`/
/// `trusted_at`) so tooling can read either surface uniformly.
fn row_json(r: &TrustRecord) -> serde_json::Value {
    json!({
        "id": r.device.0,
        "name": r.name,
        "fingerprint": r.fingerprint,
        "trusted_at": r.trusted_at.to_rfc3339_opts(SecondsFormat::Secs, true),
        "approved": r.approved,
    })
}

/// One human table row: status first, because it is the column the command
/// exists to show.
fn row_cells(r: &TrustRecord) -> Vec<String> {
    vec![
        status_word(r).to_string(),
        r.device.0.clone(),
        r.name.clone(),
        short_fingerprint(&r.fingerprint),
        r.trusted_at.format("%Y-%m-%d %H:%M").to_string(),
    ]
}

/// The two states, named.
fn status_word(r: &TrustRecord) -> &'static str {
    if r.approved {
        "approved"
    } else {
        "pinned"
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
/// It also says what approval *does*. "Trust this device?" invites yes; naming
/// the clipboard invites a look at the hex.
pub fn approval_question(record: &TrustRecord) -> String {
    format!(
        "  fingerprint  {}\n  pinned       {}\nApproving lets this device receive this \
         machine's presence status, clipboard and pipes.\nApprove {} ({})?",
        record.fingerprint,
        record.trusted_at.to_rfc3339_opts(SecondsFormat::Secs, true),
        record.name,
        record.device.0,
    )
}

/// `peerbeam trust approve <device>` — grant a pinned device standing.
fn approve(ctx: &Ctx, query: &str, path_override: Option<&str>) -> CliResult {
    let config = load_config(path_override)?;
    let store = open_trust(&config)?;
    let records = store.list();
    let record = pick(ctx, &records, query)?;

    // Already approved: say so and succeed. Re-running a provisioning script
    // must not fail, and an error here would say something untrue — the state
    // the operator asked for is the state on disk.
    if record.approved {
        return report(ctx, record, "already approved", false);
    }

    let answer = if ctx.interactive {
        Some(prompt::confirm(ctx, &approval_question(record), false))
    } else {
        None
    };
    if approval_gate(ctx.assume_yes, ctx.json, answer) == ApprovalGate::Abort {
        return Err(CliError::Cancelled);
    }

    store.approve(&record.device)?;
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
    if !stored.approved {
        return Err(CliError::Other(format!(
            "{} is still not approved after approving it",
            stored.device.0
        )));
    }
    report(ctx, &stored, "approved", true)
}

/// The receipt for an `approve`. Carries the fingerprint in every mode — a
/// scripted run never sees the prompt, so this is where its record of *what*
/// was approved comes from.
fn report(ctx: &Ctx, record: &TrustRecord, verb: &str, changed: bool) -> CliResult {
    if ctx.json {
        let mut value = row_json(record);
        if let Some(obj) = value.as_object_mut() {
            obj.insert("event".into(), json!("trust_approved"));
            obj.insert("changed".into(), json!(changed));
        }
        ctx.json_line(&value);
        return Ok(());
    }
    ctx.line(&format!(
        "{} {} ({}) — fingerprint {}",
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
    use chrono::{TimeZone, Utc};
    use peerbeam_domain::id::DeviceId;

    fn record(id: &str, name: &str, approved: bool) -> TrustRecord {
        TrustRecord {
            device: DeviceId::from(id),
            fingerprint: "a".repeat(64),
            name: name.to_string(),
            trusted_at: Utc
                .with_ymd_and_hms(2026, 8, 17, 10, 30, 0)
                .single()
                .unwrap_or_else(Utc::now),
            approved,
        }
    }

    #[test]
    fn the_status_word_names_the_two_states() {
        assert_eq!(status_word(&record("pb-a", "Laptop", true)), "approved");
        assert_eq!(status_word(&record("pb-b", "Stranger", false)), "pinned");
    }

    /// The `--json` row must carry `approved` as a **bool**, not as the mere
    /// presence of the record. A script that filtered on "is it listed?" would
    /// be asking the question this whole change exists to stop asking.
    #[test]
    fn json_rows_carry_approved_as_an_explicit_bool() {
        let approved = row_json(&record("pb-a", "Laptop", true));
        assert_eq!(approved["approved"], json!(true));
        assert_eq!(approved["id"], json!("pb-a"));

        let pinned = row_json(&record("pb-b", "Stranger", false));
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
        assert_eq!(row_json(&r)["fingerprint"], json!(r.fingerprint));
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
        let q = approval_question(&r);
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
}
