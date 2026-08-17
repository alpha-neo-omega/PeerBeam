//! End-to-end `peerbeam trust`: the real compiled binary against a throwaway
//! config and trust store.
//!
//! # Why a hand-written store is legitimate *here*
//!
//! These tests write `trust.json` directly, which `pipe_e2e.rs` deliberately
//! does not. The difference is that nothing in this file performs a handshake:
//! `trust` reads and writes this machine's own store and never touches the
//! network, so a synthetic fingerprint is just an opaque string being carried
//! from disk to stdout. A hand-written fingerprint in an *e2e* test would be a
//! key the peer cannot present, and the handshake would correctly reject it —
//! see `pipe_e2e.rs`, which drives approval through a real connection instead.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use peerbeam_config::EngineConfig;
use serde_json::{json, Value};

const BIN: &str = env!("CARGO_BIN_EXE_peerbeam");

/// A 64-hex-character fingerprint, the shape `AeadCrypto::fingerprint` produces.
fn fingerprint(seed: char) -> String {
    std::iter::repeat(seed).take(64).collect()
}

/// A config whose data directory is isolated to `dir`, plus a `trust.json`
/// holding `records`. Returns the config path to pass as `--config`.
fn store(dir: &Path, records: Value) -> PathBuf {
    let data = dir.join("data");
    std::fs::create_dir_all(&data).unwrap();
    std::fs::write(
        data.join("trust.json"),
        serde_json::to_vec_pretty(&records).unwrap(),
    )
    .unwrap();

    let mut cfg = EngineConfig::default();
    cfg.storage.data_directory = data.to_string_lossy().into_owned();
    cfg.storage.save_directory = dir.join("recv").to_string_lossy().into_owned();
    let path = dir.join("cfg.json");
    cfg.save(&path).unwrap();
    path
}

fn record(id: &str, name: &str, fp: char, approved: bool) -> Value {
    json!({
        "device": id,
        "fingerprint": fingerprint(fp),
        "name": name,
        "trusted_at": "2026-08-17T10:30:00Z",
        "approved": approved,
    })
}

/// Two devices: one the user chose, one that merely connected once.
fn laptop_and_stranger(dir: &Path) -> PathBuf {
    store(
        dir,
        json!([
            record("pb-laptop00001", "laptop", 'a', true),
            record("pb-stranger001", "Unknown Peer", 'b', false),
        ]),
    )
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

fn err(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).to_string()
}

/// Every `--json` row, parsed. One object per line (NDJSON), so a script can
/// stream it.
fn rows(cfg: &Path) -> Vec<Value> {
    let o = run(cfg, &["--json", "trust", "list"]);
    assert!(o.status.success(), "list --json failed: {}", err(&o));
    out(&o)
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).expect("each line is one JSON object"))
        .collect()
}

fn row<'a>(rows: &'a [Value], id: &str) -> &'a Value {
    rows.iter()
        .find(|r| r["id"] == json!(id))
        .unwrap_or_else(|| panic!("no row for {id} in {rows:?}"))
}

// ── list ────────────────────────────────────────────────────────────────────

/// **The distinction the command exists for.** An approved device and a
/// pinned-only one must not read the same, because one is the user's laptop and
/// the other is whoever last connected.
#[test]
fn list_distinguishes_an_approved_device_from_a_merely_pinned_one() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = laptop_and_stranger(dir.path());

    let o = run(&cfg, &["trust", "list"]);
    assert!(o.status.success(), "{}", err(&o));
    let text = out(&o);

    let laptop = text
        .lines()
        .find(|l| l.contains("pb-laptop00001"))
        .unwrap_or_else(|| panic!("laptop is missing: {text}"));
    let stranger = text
        .lines()
        .find(|l| l.contains("pb-stranger001"))
        .unwrap_or_else(|| panic!("the stranger is missing: {text}"));

    assert!(
        laptop.contains("approved"),
        "the approved device must say so: {laptop}"
    );
    assert!(
        stranger.contains("pinned") && !stranger.contains("approved"),
        "a pinned-only device must not read as approved: {stranger}"
    );
    // And the operator is told what the weaker word means, since the whole
    // point is that "pinned" looked like trust and was not.
    assert!(
        text.contains("trust approve"),
        "a store holding an unapproved device must say how to approve it: {text}"
    );
}

#[test]
fn list_json_carries_approved_as_a_bool_and_the_whole_fingerprint() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = laptop_and_stranger(dir.path());
    let rows = rows(&cfg);
    assert_eq!(rows.len(), 2);

    let laptop = row(&rows, "pb-laptop00001");
    assert_eq!(laptop["approved"], json!(true));
    assert!(
        laptop["approved"].is_boolean(),
        "not a string — a script filters on this"
    );
    assert_eq!(laptop["name"], json!("laptop"));
    assert_eq!(
        laptop["fingerprint"].as_str().map(str::len),
        Some(64),
        "the listing abbreviates; --json must not"
    );

    assert_eq!(row(&rows, "pb-stranger001")["approved"], json!(false));
}

#[test]
fn list_on_an_empty_store_says_so_and_exits_zero() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = store(dir.path(), json!([]));

    let o = run(&cfg, &["trust", "list"]);
    assert!(o.status.success());
    assert!(out(&o).contains("no devices pinned"), "{}", out(&o));

    // Empty NDJSON is an empty stream, not `[]` and not a message: a script
    // reading line by line must simply see nothing.
    let j = run(&cfg, &["--json", "trust", "list"]);
    assert!(j.status.success());
    assert!(out(&j).trim().is_empty(), "got: {}", out(&j));
}

// ── approve ─────────────────────────────────────────────────────────────────

/// **The mutation target.** `approve` must set `approved` — not merely touch or
/// rewrite the record. Breaking `FsTrust::approve` so it persists without
/// setting the flag must fail this, and the pipe e2e with it.
#[test]
fn approve_sets_approved_and_leaves_the_pinned_key_alone() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = laptop_and_stranger(dir.path());
    let before = rows(&cfg);
    let fp = row(&before, "pb-stranger001")["fingerprint"].clone();

    let o = run(&cfg, &["-y", "trust", "approve", "pb-stranger001"]);
    assert!(o.status.success(), "{}", err(&o));

    let after = rows(&cfg);
    let stranger = row(&after, "pb-stranger001");
    assert_eq!(stranger["approved"], json!(true), "approval must persist");
    assert_eq!(
        stranger["fingerprint"], fp,
        "approving must not disturb the pinned key — that key is what makes a \
         later change detectable"
    );
    // Nobody else moved.
    assert_eq!(row(&after, "pb-laptop00001")["approved"], json!(true));
    assert_eq!(after.len(), 2);
}

/// The command must show the key it is being asked to vouch for, in the
/// scripted path too — a `--yes` run never sees the prompt, so this line is the
/// only record of *what* was approved.
#[test]
fn approve_prints_the_full_fingerprint_it_is_approving() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = laptop_and_stranger(dir.path());

    let o = run(&cfg, &["-y", "trust", "approve", "Unknown Peer"]);
    assert!(o.status.success(), "{}", err(&o));
    assert!(
        out(&o).contains(&fingerprint('b')),
        "the fingerprint must be in the receipt: {}",
        out(&o)
    );

    // And under --json, in the event.
    let dir2 = tempfile::tempdir().unwrap();
    let cfg2 = laptop_and_stranger(dir2.path());
    let j = run(&cfg2, &["--json", "trust", "approve", "pb-stranger001"]);
    assert!(j.status.success(), "{}", err(&j));
    let v: Value = serde_json::from_str(out(&j).trim()).expect("one JSON object");
    assert_eq!(v["fingerprint"], json!(fingerprint('b')));
    assert_eq!(v["approved"], json!(true));
    assert_eq!(v["changed"], json!(true));
}

#[test]
fn approving_an_already_approved_device_is_a_no_op_that_exits_zero() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = laptop_and_stranger(dir.path());

    let o = run(&cfg, &["-y", "trust", "approve", "laptop"]);
    assert!(
        o.status.success(),
        "re-running a provisioning script must not fail: {}",
        err(&o)
    );
    assert!(
        out(&o).contains("already approved"),
        "it must say it did nothing rather than imply a change: {}",
        out(&o)
    );
    assert_eq!(row(&rows(&cfg), "pb-laptop00001")["approved"], json!(true));

    let j = run(&cfg, &["--json", "trust", "approve", "laptop"]);
    let v: Value = serde_json::from_str(out(&j).trim()).expect("one JSON object");
    assert_eq!(v["changed"], json!(false), "nothing changed");
    assert_eq!(v["approved"], json!(true));
}

/// An ambiguous `<device>` is an error naming the candidates, never a guess —
/// on this command a wrong guess approves a stranger.
#[test]
fn approve_on_an_ambiguous_prefix_errors_and_names_the_candidates() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = store(
        dir.path(),
        json!([
            record("pb-lab000000001", "lab-alpha", 'a', false),
            record("pb-lab000000002", "lab-beta", 'b', false),
        ]),
    );

    let o = run(&cfg, &["-y", "trust", "approve", "lab"]);
    assert_eq!(o.status.code(), Some(2), "usage error: {}", err(&o));
    let msg = err(&o);
    assert!(
        msg.contains("lab-alpha") && msg.contains("lab-beta"),
        "{msg}"
    );

    // And nothing was approved on the way to failing.
    let rows = rows(&cfg);
    assert!(rows.iter().all(|r| r["approved"] == json!(false)));
}

#[test]
fn approve_on_an_unknown_device_is_not_found() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = laptop_and_stranger(dir.path());
    let o = run(&cfg, &["-y", "trust", "approve", "ghost"]);
    assert_eq!(o.status.code(), Some(3), "{}", err(&o));
}

/// **Fail-closed.** No `--yes`, no `--json`, and no terminal to ask at — the
/// child's stdin is null — must not approve. `Command::output` gives the child
/// a non-TTY stdin, which is exactly the shape of a cron job or an SSH command
/// that forgot the flag, and an unanswered question is not consent.
#[test]
fn approve_without_yes_and_without_a_terminal_refuses_and_changes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = laptop_and_stranger(dir.path());

    let o = run(&cfg, &["trust", "approve", "pb-stranger001"]);
    assert_eq!(
        o.status.code(),
        Some(6),
        "an unconfirmed approve must exit `cancelled`: {}",
        err(&o)
    );
    assert_eq!(
        row(&rows(&cfg), "pb-stranger001")["approved"],
        json!(false),
        "nothing may be approved without an answer"
    );
}

// ── revoke ──────────────────────────────────────────────────────────────────

/// Revoke removes the whole record, not just the approval, so the next
/// connection is a fresh first contact.
#[test]
fn revoke_removes_the_record_entirely() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = laptop_and_stranger(dir.path());

    let o = run(&cfg, &["trust", "revoke", "laptop"]);
    assert!(o.status.success(), "{}", err(&o));

    let after = rows(&cfg);
    assert_eq!(after.len(), 1);
    assert_eq!(after[0]["id"], json!("pb-stranger001"));
}

#[test]
fn revoking_an_absent_device_exits_non_zero() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = store(dir.path(), json!([]));

    let o = run(&cfg, &["trust", "revoke", "pb-never-seen"]);
    assert_eq!(
        o.status.code(),
        Some(3),
        "revoking nothing must fail rather than report a success it did not \
         perform: {}",
        err(&o)
    );
    assert!(!err(&o).trim().is_empty(), "and say so");
}
