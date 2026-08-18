//! `peerbeam rules` — list, add and remove the rules that choose **where** a
//! received file lands.
//!
//! # What a rule is, and what it is not
//!
//! A rule is a **match** plus a **destination**. It decides where an accepted
//! file is written. It never decides **whether** a file is accepted: the
//! approval prompt and `device.auto_accept_trusted` are untouched by everything
//! in this file, and the receive path reads rules only after a transfer has
//! been accepted and is on its way to disk (I6).
//!
//! ```text
//!   #  DEVICE           EXT   SIZE          DESTINATION
//!   0  pb-f4e4d56fce98  any   ≥ 1.1 GB      /mnt/big
//!   1  any              pdf   any           /srv/papers
//!   2  any              any   any           /srv/inbox
//! ```
//!
//! The leading `#` is the point of the listing. Rules are an ordered list and
//! the **first match wins**, so the number is both the identity `remove` takes
//! and the answer to "which of these two will apply?". `add --at` inserts at a
//! position for the same reason: reordering is the user's only lever over the
//! tie-break, and it needs to exist on a headless box too.
//!
//! # Where they are stored
//!
//! In this machine's config file, under `storage.rules` — the same file
//! `peerbeam config show` prints and the same one the daemon reads at startup.
//! There is no separate rules store to get out of step.
//!
//! # Output
//!
//! Human text on stdout, errors on stderr, and `--json` emits one object per
//! line so a script can stream it. Exit codes are the CLI's usual ones: `2` for
//! a rule that cannot be stored (a relative destination, a missing parent, an
//! ambiguous `--from`), `3` for an index or device that matches nothing.

use serde_json::json;

use peerbeam_config::{EngineConfig, SaveRule};

use crate::cli::RulesAction;
use crate::commands::{human_bytes, load_config, open_trust, resolve_peer};
use crate::engine::config_path;
use crate::exit::{CliError, CliResult};
use crate::output::Ctx;

/// Dispatch `peerbeam rules`.
pub fn rules(ctx: &Ctx, action: RulesAction, path_override: Option<&str>) -> CliResult {
    match action {
        RulesAction::List => list(ctx, path_override),
        RulesAction::Add {
            directory,
            from,
            ext,
            min_bytes,
            max_bytes,
            at,
        } => add(
            ctx,
            AddSpec {
                directory,
                from,
                ext,
                min_bytes,
                max_bytes,
                at,
            },
            path_override,
        ),
        RulesAction::Remove { index } => remove(ctx, index, path_override),
    }
}

// ── list ────────────────────────────────────────────────────────────────────

/// `peerbeam rules list` — the rules, in the order they are consulted.
fn list(ctx: &Ctx, path_override: Option<&str>) -> CliResult {
    let config = load_config(path_override)?;
    let rules = &config.storage.rules;

    if ctx.json {
        for (i, r) in rules.iter().enumerate() {
            ctx.json_line(&row_json(i, r));
        }
        return Ok(());
    }

    if rules.is_empty() {
        ctx.line(&ctx.dim(&format!(
            "no rules — every received file goes to {}",
            config.storage.save_directory
        )));
        return Ok(());
    }

    let rows: Vec<Vec<String>> = rules
        .iter()
        .enumerate()
        .map(|(i, r)| row_cells(i, r))
        .collect();
    ctx.table(&["#", "DEVICE", "EXT", "SIZE", "DESTINATION"], &rows);
    ctx.line("");
    ctx.line(&ctx.dim(&format!(
        "The first rule that matches wins. A file matching none goes to {}.",
        config.storage.save_directory
    )));
    Ok(())
}

/// One `--json` row. Criteria that are not set are `null`, never `""` or `0` —
/// a script must be able to tell "any size" from "exactly zero bytes".
fn row_json(index: usize, r: &SaveRule) -> serde_json::Value {
    json!({
        "index": index,
        "device": r.device,
        "extension": r.extension,
        "min_bytes": r.min_bytes,
        "max_bytes": r.max_bytes,
        "directory": r.directory,
    })
}

/// One human table row. An omitted criterion reads `any`, which is what it
/// does — a blank cell would look like a value that failed to render.
fn row_cells(index: usize, r: &SaveRule) -> Vec<String> {
    vec![
        index.to_string(),
        r.device.clone().unwrap_or_else(|| "any".into()),
        r.extension.clone().unwrap_or_else(|| "any".into()),
        size_range(r),
        r.directory.clone(),
    ]
}

/// The size criterion in one cell: `any`, one bound, or both.
fn size_range(r: &SaveRule) -> String {
    match (r.min_bytes, r.max_bytes) {
        (None, None) => "any".to_string(),
        (Some(min), None) => format!("≥ {}", human_bytes(min)),
        (None, Some(max)) => format!("≤ {}", human_bytes(max)),
        (Some(min), Some(max)) => format!("{}–{}", human_bytes(min), human_bytes(max)),
    }
}

// ── add ─────────────────────────────────────────────────────────────────────

/// The `add` arguments, kept together so `rules()` stays a dispatch table
/// rather than a function with seven positional parameters.
struct AddSpec {
    directory: String,
    from: Option<String>,
    ext: Option<String>,
    min_bytes: Option<u64>,
    max_bytes: Option<u64>,
    at: Option<usize>,
}

/// `peerbeam rules add <directory> [criteria]`.
///
/// The rule is **validated before it is stored** — absolute destination, no
/// `..`, an existing parent, a satisfiable size range — because now is when the
/// person can fix it. A rule that only failed the first time a file arrived
/// would fail on a headless box at 3am, in a log nobody is reading.
fn add(ctx: &Ctx, spec: AddSpec, path_override: Option<&str>) -> CliResult {
    let mut config = load_config(path_override)?;

    let device = match &spec.from {
        Some(q) => Some(resolve_device(ctx, &config, q)?),
        None => None,
    };
    let rule = SaveRule {
        device,
        // Stored without its leading dot so the file reads the same however it
        // was typed; matching tolerates either.
        extension: spec
            .ext
            .as_deref()
            .map(|e| e.trim().trim_start_matches('.').to_string()),
        min_bytes: spec.min_bytes,
        max_bytes: spec.max_bytes,
        directory: spec.directory.trim().to_string(),
    };
    rule.validate()
        .map_err(|e| CliError::Usage(e.to_string()))?;

    // `--at` past the end appends rather than failing: "put it last" is a
    // reasonable thing to mean by a large number, and refusing would make a
    // script that appends have to count first.
    let index = spec
        .at
        .unwrap_or(usize::MAX)
        .min(config.storage.rules.len());
    config.storage.rules.insert(index, rule.clone());
    save(&config, path_override)?;

    if ctx.json {
        let mut value = row_json(index, &rule);
        if let Some(obj) = value.as_object_mut() {
            obj.insert("event".into(), json!("rule_added"));
        }
        ctx.json_line(&value);
        return Ok(());
    }
    ctx.line(&format!(
        "{} rule {} — {} → {}",
        ctx.green("added"),
        index,
        criteria_phrase(&rule),
        rule.directory
    ));
    if index + 1 < config.storage.rules.len() {
        ctx.line(&ctx.dim(
            "Inserted before existing rules; the first match wins, so this one now takes \
             precedence over them.",
        ));
    }
    Ok(())
}

/// The criteria in one human phrase, for a receipt.
fn criteria_phrase(r: &SaveRule) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(d) = &r.device {
        parts.push(format!("from {d}"));
    }
    if let Some(e) = &r.extension {
        parts.push(format!("*.{e}"));
    }
    match (r.min_bytes, r.max_bytes) {
        (None, None) => {}
        _ => parts.push(size_range(r)),
    }
    if parts.is_empty() {
        // Said plainly, because a catch-all added below other rules does
        // nothing visible while a catch-all added above them changes
        // everything — and the user should read that back, not infer it.
        return "everything".to_string();
    }
    parts.join(", ")
}

/// Resolve `--from` to an **authenticated device id**.
///
/// Goes through the CLI's one device resolver ([`resolve_peer`] over
/// `resolve::resolve`) against the trust store, exactly as `peerbeam trust`
/// does: the same string must not name different devices in different commands.
///
/// A device this machine has never met resolves to nothing, and that is a
/// legitimate case — provisioning a rule before the device first connects. So a
/// query that is *already* a well-formed device id is taken verbatim rather
/// than refused. Anything else is a `NotFound`, because a typo'd name silently
/// stored as a criterion would be a rule that never fires and never explains
/// why.
fn resolve_device(ctx: &Ctx, config: &EngineConfig, query: &str) -> Result<String, CliError> {
    let records = open_trust(config)?.list();
    let candidates: Vec<(String, String)> = records
        .iter()
        .map(|r| (r.device.0.clone(), r.name.clone()))
        .collect();
    match resolve_peer(ctx, &candidates, &Some(query.to_string())) {
        Ok(i) => candidates
            .get(i)
            .map(|(id, _)| id.clone())
            .ok_or_else(|| CliError::NotFound(format!("device {query}"))),
        Err(CliError::NotFound(_)) if is_device_id(query) => Ok(query.to_string()),
        Err(e) => Err(e),
    }
}

/// Is this string already a device id — `pb-` plus twelve hex characters?
///
/// The shape `device_id_from_fingerprint` mints (`peerbeam-domain`), checked
/// here so an unknown *id* can be accepted while an unknown *name* is still
/// refused.
fn is_device_id(s: &str) -> bool {
    match s.strip_prefix("pb-") {
        Some(rest) => rest.len() == 12 && rest.chars().all(|c| c.is_ascii_hexdigit()),
        None => false,
    }
}

// ── remove ──────────────────────────────────────────────────────────────────

/// `peerbeam rules remove <index>` — drop one rule by its listed position.
///
/// No confirmation: removing a rule only ever sends files back to the save
/// directory, which is where they went before any of this existed. The
/// destructive direction here is *adding* a rule that diverts them, and that is
/// the one that validates.
fn remove(ctx: &Ctx, index: usize, path_override: Option<&str>) -> CliResult {
    let mut config = load_config(path_override)?;
    if index >= config.storage.rules.len() {
        return Err(CliError::NotFound(format!(
            "rule {index} (there {} {})",
            if config.storage.rules.len() == 1 {
                "is"
            } else {
                "are"
            },
            match config.storage.rules.len() {
                0 => "no rules".to_string(),
                1 => "1 rule".to_string(),
                n => format!("{n} rules"),
            }
        )));
    }
    let removed = config.storage.rules.remove(index);
    save(&config, path_override)?;

    if ctx.json {
        let mut value = row_json(index, &removed);
        if let Some(obj) = value.as_object_mut() {
            obj.insert("event".into(), json!("rule_removed"));
        }
        ctx.json_line(&value);
        return Ok(());
    }
    ctx.line(&format!(
        "{} rule {} — {} → {}",
        ctx.yellow("removed"),
        index,
        criteria_phrase(&removed),
        removed.directory
    ));
    // Every later rule just moved up, and the next `remove` takes the *new*
    // number. Saying so costs one line and saves removing the wrong rule.
    if !config.storage.rules.is_empty() {
        ctx.line(&ctx.dim("Remaining rules were renumbered; run `rules list` to see them."));
    }
    Ok(())
}

// ── shared ──────────────────────────────────────────────────────────────────

/// Write the config back, to the same path `load_config` read it from.
///
/// Whole-config save, matching `peerbeam config set`: the config file is one
/// document and there is no partial writer for it. `EngineConfig::save` is
/// atomic (temp + fsync + rename), so an interrupted `rules add` cannot leave a
/// half-written rule list behind.
fn save(config: &EngineConfig, path_override: Option<&str>) -> CliResult {
    config
        .save(&config_path(path_override))
        .map_err(|e| CliError::Other(format!("save config: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(dir: &str) -> SaveRule {
        SaveRule {
            directory: dir.to_string(),
            ..SaveRule::default()
        }
    }

    /// An omitted criterion reads `any`, and the index — the thing `remove`
    /// takes and the thing that decides ties — is the first column.
    #[test]
    fn a_row_shows_any_for_every_omitted_criterion() {
        let cells = row_cells(3, &rule("/srv/inbox"));
        assert_eq!(cells[0], "3");
        assert_eq!(cells[1], "any");
        assert_eq!(cells[2], "any");
        assert_eq!(cells[3], "any");
        assert_eq!(cells[4], "/srv/inbox");
    }

    /// `--json` must carry an unset criterion as `null`. `""` or `0` would be
    /// indistinguishable from a real value — and `0` is a legitimate
    /// `min_bytes`.
    #[test]
    fn json_rows_carry_an_unset_criterion_as_null() {
        let value = row_json(0, &rule("/srv/inbox"));
        assert!(value["device"].is_null());
        assert!(value["extension"].is_null());
        assert!(value["min_bytes"].is_null());
        assert_eq!(value["directory"], json!("/srv/inbox"));

        let zero = SaveRule {
            min_bytes: Some(0),
            ..rule("/srv/inbox")
        };
        assert_eq!(
            row_json(0, &zero)["min_bytes"],
            json!(0),
            "a real zero must not be rendered as 'unset'"
        );
    }

    #[test]
    fn the_size_cell_renders_each_shape_of_range() {
        let mut r = rule("/d");
        assert_eq!(size_range(&r), "any");
        r.min_bytes = Some(2_000);
        assert_eq!(size_range(&r), "≥ 2.0 KB");
        r.max_bytes = Some(5_000);
        assert_eq!(size_range(&r), "2.0 KB–5.0 KB");
        r.min_bytes = None;
        assert_eq!(size_range(&r), "≤ 5.0 KB");
    }

    /// A rule with no criteria is a catch-all, and the receipt says so rather
    /// than printing an empty phrase.
    #[test]
    fn a_catch_all_receipt_says_everything() {
        assert_eq!(criteria_phrase(&rule("/srv/inbox")), "everything");
        let r = SaveRule {
            device: Some("pb-abc123abc123".into()),
            extension: Some("pdf".into()),
            ..rule("/srv/papers")
        };
        assert_eq!(criteria_phrase(&r), "from pb-abc123abc123, *.pdf");
    }

    /// The device-id shape, so an unknown *id* can be provisioned while an
    /// unknown *name* is still refused as a typo.
    #[test]
    fn a_device_id_is_pb_plus_twelve_hex_characters() {
        assert!(is_device_id("pb-f4e4d56fce98"));
        assert!(is_device_id("pb-000000000000"));
        assert!(!is_device_id("pb-f4e4d56fce9"), "eleven is not twelve");
        assert!(!is_device_id("pb-f4e4d56fce987"), "thirteen is not twelve");
        assert!(!is_device_id("pb-zzzzzzzzzzzz"), "not hex");
        assert!(!is_device_id("laptop"), "a name is not an id");
        assert!(!is_device_id(""));
    }
}
