//! End-to-end `peerbeam rules`: the real compiled binary against a throwaway
//! config.
//!
//! The command's job is to get a rule into `storage.rules` — in the right
//! **position**, with the right **device id**, and only when it is **valid** —
//! so every assertion here reads the config file back rather than trusting the
//! receipt printed to stdout.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use peerbeam_config::EngineConfig;
use serde_json::{json, Value};

const BIN: &str = env!("CARGO_BIN_EXE_peerbeam");

/// An isolated config, optionally seeded with a `trust.json`, returning the
/// config path to pass as `--config`.
fn config_in(dir: &Path, trust: Option<Value>) -> PathBuf {
    let data = dir.join("data");
    std::fs::create_dir_all(&data).unwrap();
    if let Some(records) = trust {
        std::fs::write(
            data.join("trust.json"),
            serde_json::to_vec_pretty(&records).unwrap(),
        )
        .unwrap();
    }
    let mut cfg = EngineConfig::default();
    cfg.storage.data_directory = data.to_string_lossy().into_owned();
    cfg.storage.save_directory = dir.join("recv").to_string_lossy().into_owned();
    let path = dir.join("cfg.json");
    cfg.save(&path).unwrap();
    path
}

fn run(cfg: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .arg("--config")
        .arg(cfg)
        .arg("--no-color")
        .args(args)
        .output()
        .expect("run peerbeam")
}

fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).to_string()
}

fn code(o: &Output) -> i32 {
    o.status.code().unwrap_or(-1)
}

/// The rules as they are actually stored.
fn stored(cfg: &Path) -> Vec<peerbeam_config::SaveRule> {
    EngineConfig::load(cfg).expect("load config").storage.rules
}

/// Every `--json` line as a value.
fn json_lines(o: &Output) -> Vec<Value> {
    out(o)
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).expect("each line is one JSON object"))
        .collect()
}

// ── list ────────────────────────────────────────────────────────────────────

/// With no rules, the listing says so **and names where files go instead** —
/// the whole answer to "what happens to my files?" in one line.
#[test]
fn an_empty_list_names_the_save_directory() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = config_in(dir.path(), None);
    let o = run(&cfg, &["rules", "list"]);
    assert_eq!(code(&o), 0);
    assert!(out(&o).contains("no rules"), "{}", out(&o));
    assert!(
        out(&o).contains(&dir.path().join("recv").to_string_lossy().into_owned()),
        "the save directory must be named: {}",
        out(&o)
    );
}

/// The listing is **ordered and numbered**, because the number is both what
/// `remove` takes and what decides which of two matching rules applies.
#[test]
fn the_listing_is_numbered_in_the_order_rules_are_consulted() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = config_in(dir.path(), None);
    let first = dir.path().join("papers");
    let second = dir.path().join("inbox");

    assert_eq!(
        code(&run(
            &cfg,
            &["rules", "add", first.to_str().unwrap(), "--ext", "pdf"]
        )),
        0
    );
    assert_eq!(
        code(&run(&cfg, &["rules", "add", second.to_str().unwrap()])),
        0
    );

    let o = run(&cfg, &["rules", "list"]);
    let text = out(&o);
    let papers_at = text.find("papers").expect("first rule listed");
    let inbox_at = text.find("inbox").expect("second rule listed");
    assert!(papers_at < inbox_at, "listed out of order:\n{text}");
    assert!(
        text.contains("first rule that matches"),
        "the tie-break must be stated:\n{text}"
    );

    let rows = json_lines(&run(&cfg, &["--json", "rules", "list"]));
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0]["index"], json!(0));
    assert_eq!(rows[0]["extension"], json!("pdf"));
    assert_eq!(rows[1]["index"], json!(1));
    assert!(
        rows[1]["extension"].is_null(),
        "an omitted criterion is null, not \"\": {}",
        rows[1]
    );
}

// ── add ─────────────────────────────────────────────────────────────────────

/// A rule with no criteria is a catch-all, and the receipt says so — a
/// catch-all added at the top changes everything, and the user should read that
/// back rather than infer it.
#[test]
fn a_rule_with_no_criteria_is_stored_as_a_catch_all() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = config_in(dir.path(), None);
    let dest = dir.path().join("inbox");

    let o = run(&cfg, &["rules", "add", dest.to_str().unwrap()]);
    assert_eq!(code(&o), 0, "{}", String::from_utf8_lossy(&o.stderr));
    assert!(out(&o).contains("everything"), "{}", out(&o));

    let rules = stored(&cfg);
    assert_eq!(rules.len(), 1);
    assert_eq!(rules[0].device, None);
    assert_eq!(rules[0].extension, None);
    assert_eq!(rules[0].min_bytes, None);
    assert_eq!(rules[0].max_bytes, None);
    assert_eq!(rules[0].directory, dest.to_string_lossy());
}

/// `--at` inserts rather than appends. Order is the tie-break, so a headless
/// box needs a way to say "this one first" without rewriting the list.
#[test]
fn add_at_inserts_and_renumbers_the_rest() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = config_in(dir.path(), None);
    let inbox = dir.path().join("inbox");
    let papers = dir.path().join("papers");

    run(&cfg, &["rules", "add", inbox.to_str().unwrap()]);
    let o = run(
        &cfg,
        &[
            "rules",
            "add",
            papers.to_str().unwrap(),
            "--ext",
            "pdf",
            "--at",
            "0",
        ],
    );
    assert_eq!(code(&o), 0, "{}", String::from_utf8_lossy(&o.stderr));
    assert!(
        out(&o).contains("takes precedence"),
        "inserting above existing rules must say so: {}",
        out(&o)
    );

    let rules = stored(&cfg);
    assert_eq!(rules[0].directory, papers.to_string_lossy());
    assert_eq!(rules[1].directory, inbox.to_string_lossy());
}

/// A leading dot on `--ext` is accepted and normalised away, so the stored
/// config reads the same however it was typed.
#[test]
fn an_extension_is_stored_without_its_leading_dot() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = config_in(dir.path(), None);
    run(
        &cfg,
        &[
            "rules",
            "add",
            dir.path().join("papers").to_str().unwrap(),
            "--ext",
            ".PDF",
        ],
    );
    assert_eq!(stored(&cfg)[0].extension.as_deref(), Some("PDF"));
}

// ── add: validation, at the point of adding ─────────────────────────────────

/// **A relative destination is refused when it is added**, not when a file
/// arrives — that is the difference between an error the user can act on and
/// one that fails at 3am on a headless box.
#[test]
fn a_relative_destination_is_refused_and_stores_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = config_in(dir.path(), None);

    let o = run(&cfg, &["rules", "add", "videos/incoming"]);
    assert_eq!(code(&o), 2, "a bad rule is a usage error");
    assert!(
        String::from_utf8_lossy(&o.stderr).contains("absolute"),
        "the message must say what is wrong: {}",
        String::from_utf8_lossy(&o.stderr)
    );
    assert!(
        stored(&cfg).is_empty(),
        "a refused rule must not reach the config file"
    );
}

/// A `..` component is refused for the same reason.
#[test]
fn a_parent_traversal_is_refused_and_stores_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = config_in(dir.path(), None);
    let sneaky = dir.path().join("..").join("elsewhere");

    let o = run(&cfg, &["rules", "add", sneaky.to_str().unwrap()]);
    assert_eq!(code(&o), 2);
    assert!(
        String::from_utf8_lossy(&o.stderr).contains(".."),
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );
    assert!(stored(&cfg).is_empty());
}

/// A destination whose parent does not exist is refused — the typo is caught
/// while the user is still looking at the path they typed.
#[test]
fn a_missing_parent_is_refused_and_stores_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = config_in(dir.path(), None);
    let nested = dir.path().join("no-such-parent").join("videos");

    let o = run(&cfg, &["rules", "add", nested.to_str().unwrap()]);
    assert_eq!(code(&o), 2);
    assert!(
        String::from_utf8_lossy(&o.stderr).contains("parent"),
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );
    assert!(stored(&cfg).is_empty());
}

/// An inverted size range can never match, so it is refused rather than stored
/// as a rule that silently does nothing forever.
#[test]
fn an_impossible_size_range_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = config_in(dir.path(), None);
    let o = run(
        &cfg,
        &[
            "rules",
            "add",
            dir.path().join("mid").to_str().unwrap(),
            "--min-bytes",
            "500",
            "--max-bytes",
            "100",
        ],
    );
    assert_eq!(code(&o), 2);
    assert!(stored(&cfg).is_empty());
}

// ── add --from: the sender criterion ────────────────────────────────────────

fn trust_records() -> Value {
    json!([
        {
            "device": "pb-laptop00001",
            "fingerprint": "a".repeat(64),
            "name": "laptop",
            "trusted_at": "2026-08-17T10:30:00Z",
            "approved": true,
        },
        {
            "device": "pb-phone000002",
            "fingerprint": "b".repeat(64),
            "name": "phone",
            "trusted_at": "2026-08-17T10:30:00Z",
            "approved": false,
        },
    ])
}

/// **`--from` stores the authenticated device id, never the name it was typed
/// as.** A name is peer-supplied; storing one would let any peer calling itself
/// "laptop" inherit the laptop's rule.
#[test]
fn from_a_name_is_stored_as_that_devices_id() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = config_in(dir.path(), Some(trust_records()));

    let o = run(
        &cfg,
        &[
            "rules",
            "add",
            dir.path().join("from-laptop").to_str().unwrap(),
            "--from",
            "laptop",
        ],
    );
    assert_eq!(code(&o), 0, "{}", String::from_utf8_lossy(&o.stderr));
    assert_eq!(
        stored(&cfg)[0].device.as_deref(),
        Some("pb-laptop00001"),
        "the id must be stored, not the string that was typed"
    );
}

/// An ambiguous `--from` lists the candidates and refuses, rather than picking
/// one — on this command a wrong guess sends someone else's files somewhere.
#[test]
fn an_ambiguous_from_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = config_in(
        dir.path(),
        Some(json!([
            {"device":"pb-aaaaaaaaaaaa","fingerprint":"a".repeat(64),"name":"laptop-work","trusted_at":"2026-08-17T10:30:00Z","approved":true},
            {"device":"pb-bbbbbbbbbbbb","fingerprint":"b".repeat(64),"name":"laptop-home","trusted_at":"2026-08-17T10:30:00Z","approved":true},
        ])),
    );
    let o = run(
        &cfg,
        &[
            "rules",
            "add",
            dir.path().join("d").to_str().unwrap(),
            "--from",
            "laptop",
        ],
    );
    assert_eq!(code(&o), 2, "ambiguity is a usage error");
    assert!(stored(&cfg).is_empty());
}

/// An unknown *name* is a typo and is refused: a rule stored against a name
/// nothing resolves to would never fire and never explain why.
#[test]
fn an_unknown_name_in_from_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = config_in(dir.path(), Some(trust_records()));
    let o = run(
        &cfg,
        &[
            "rules",
            "add",
            dir.path().join("d").to_str().unwrap(),
            "--from",
            "tablet",
        ],
    );
    assert_eq!(code(&o), 3);
    assert!(stored(&cfg).is_empty());
}

/// An unknown *id* is taken verbatim: writing a rule for a device before it
/// first connects is exactly what provisioning a new machine looks like.
#[test]
fn an_unknown_but_well_formed_device_id_is_accepted() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = config_in(dir.path(), Some(trust_records()));
    let o = run(
        &cfg,
        &[
            "rules",
            "add",
            dir.path().join("d").to_str().unwrap(),
            "--from",
            "pb-abcdef123456",
        ],
    );
    assert_eq!(code(&o), 0, "{}", String::from_utf8_lossy(&o.stderr));
    assert_eq!(stored(&cfg)[0].device.as_deref(), Some("pb-abcdef123456"));
}

// ── remove ──────────────────────────────────────────────────────────────────

#[test]
fn remove_drops_the_named_rule_and_keeps_the_order_of_the_rest() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = config_in(dir.path(), None);
    for name in ["a", "b", "c"] {
        run(
            &cfg,
            &["rules", "add", dir.path().join(name).to_str().unwrap()],
        );
    }

    let o = run(&cfg, &["rules", "remove", "1"]);
    assert_eq!(code(&o), 0, "{}", String::from_utf8_lossy(&o.stderr));
    assert!(
        out(&o).contains("renumbered"),
        "the remaining indices moved; say so: {}",
        out(&o)
    );

    let rules = stored(&cfg);
    assert_eq!(rules.len(), 2);
    assert_eq!(rules[0].directory, dir.path().join("a").to_string_lossy());
    assert_eq!(rules[1].directory, dir.path().join("c").to_string_lossy());
}

#[test]
fn removing_an_index_that_does_not_exist_is_not_found() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = config_in(dir.path(), None);
    run(
        &cfg,
        &["rules", "add", dir.path().join("a").to_str().unwrap()],
    );

    let o = run(&cfg, &["rules", "remove", "7"]);
    assert_eq!(code(&o), 3);
    assert_eq!(stored(&cfg).len(), 1, "nothing may be removed on an error");
}

/// `--json` remove reports what it removed, so a script has a receipt for a
/// rule that no longer exists to be listed.
#[test]
fn json_remove_reports_the_rule_it_removed() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = config_in(dir.path(), None);
    let dest = dir.path().join("papers");
    run(
        &cfg,
        &["rules", "add", dest.to_str().unwrap(), "--ext", "pdf"],
    );

    let rows = json_lines(&run(&cfg, &["--json", "rules", "remove", "0"]));
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["event"], json!("rule_removed"));
    assert_eq!(rows[0]["extension"], json!("pdf"));
    assert_eq!(rows[0]["directory"], json!(dest.to_string_lossy()));
    assert!(stored(&cfg).is_empty());
}

// ── the rest of the config is untouched ─────────────────────────────────────

/// Editing rules rewrites the config file, so it must not lose anything else in
/// it. A `rules add` that silently reset the transfer port would be a far worse
/// bug than the feature is a feature.
#[test]
fn editing_rules_preserves_the_rest_of_the_config() {
    let dir = tempfile::tempdir().unwrap();
    let cfg_path = dir.path().join("cfg.json");
    let mut cfg = EngineConfig::default();
    cfg.storage.data_directory = dir.path().join("data").to_string_lossy().into_owned();
    cfg.storage.save_directory = dir.path().join("recv").to_string_lossy().into_owned();
    cfg.device.name = "chosen-name".into();
    cfg.transfer.port = 51234;
    cfg.device.require_pairing_confirmation = true;
    cfg.save(&cfg_path).unwrap();

    run(
        &cfg_path,
        &["rules", "add", dir.path().join("inbox").to_str().unwrap()],
    );

    let back = EngineConfig::load(&cfg_path).unwrap();
    assert_eq!(back.device.name, "chosen-name");
    assert_eq!(back.transfer.port, 51234);
    assert!(back.device.require_pairing_confirmation);
    assert_eq!(back.storage.rules.len(), 1);
}
