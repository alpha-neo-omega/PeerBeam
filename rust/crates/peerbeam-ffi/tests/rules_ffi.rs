//! Auto-save rules across the C ABI: `pb_rules_set` writes the ordered list,
//! `pb_settings_get` reads it back, and neither can touch acceptance.
//!
//! Uses the C-ABI functions directly and serialized, like the other FFI suites
//! — the engine's state is a process-wide global.

use std::ffi::{CStr, CString};
use std::os::raw::c_char;

use serde_json::{json, Value};

use peerbeam_config::EngineConfig;
use peerbeam_ffi::*;

fn take(ptr: *mut c_char) -> Value {
    let s = unsafe { CStr::from_ptr(ptr).to_str().unwrap().to_string() };
    unsafe { pb_free_string(ptr) };
    serde_json::from_str(&s).unwrap()
}

fn call(f: unsafe extern "C" fn(*const c_char) -> *mut c_char, v: &Value) -> Value {
    let c = CString::new(v.to_string()).unwrap();
    take(unsafe { f(c.as_ptr()) })
}

/// Init against a throwaway data directory, and hand back the path so a test
/// can point rules at real directories under it.
fn init(dir: &std::path::Path) {
    let mut cfg = EngineConfig::default();
    cfg.storage.data_directory = dir.join("data").to_string_lossy().into_owned();
    cfg.storage.save_directory = dir.join("recv").to_string_lossy().into_owned();
    // An ephemeral port: the well-known one may be held by something else on
    // the machine running these tests.
    cfg.transfer.port = 0;
    cfg.discovery.port = 0;
    let c = CString::new(serde_json::to_string(&cfg).unwrap()).unwrap();
    let v = take(unsafe { pb_init(c.as_ptr()) });
    assert_eq!(v["ok"], true, "init: {v}");
}

fn settings() -> Value {
    let v = take(pb_settings_get());
    assert_eq!(v["ok"], true, "settings_get: {v}");
    v["data"].clone()
}

/// **Nothing changed for an install that never opens this.** A fresh engine
/// reports an empty list, and the save directory is still where files go.
#[test]
#[serial_test::serial]
fn a_fresh_engine_has_no_rules() {
    let dir = tempfile::tempdir().unwrap();
    init(dir.path());

    let s = settings();
    assert_eq!(
        s["save_rules"],
        json!([]),
        "a fresh install must have no rules: {s}"
    );
    assert_eq!(
        s["rules_supported"], true,
        "this is a desktop test build; rules apply here"
    );
    pb_shutdown();
}

/// The list round-trips **in order** — the order is the tie-break, so a store
/// that reordered it would change which rule applies.
#[test]
#[serial_test::serial]
fn the_list_round_trips_in_order() {
    let dir = tempfile::tempdir().unwrap();
    init(dir.path());
    let papers = dir.path().join("papers");
    let inbox = dir.path().join("inbox");

    let r = call(
        pb_rules_set,
        &json!({ "rules": [
            { "extension": "pdf", "directory": papers.to_string_lossy() },
            { "directory": inbox.to_string_lossy() },
        ]}),
    );
    assert_eq!(r["ok"], true, "{r}");
    assert_eq!(r["data"]["count"], 2);

    let stored = settings()["save_rules"].clone();
    assert_eq!(stored.as_array().map(Vec::len), Some(2));
    assert_eq!(stored[0]["extension"], "pdf");
    assert_eq!(stored[0]["directory"], json!(papers.to_string_lossy()));
    assert_eq!(stored[1]["directory"], json!(inbox.to_string_lossy()));
    assert!(
        stored[1]["extension"].is_null(),
        "an omitted criterion stays omitted: {stored}"
    );

    // Reordering is the same call with the list the other way round.
    let r = call(
        pb_rules_set,
        &json!({ "rules": [
            { "directory": inbox.to_string_lossy() },
            { "extension": "pdf", "directory": papers.to_string_lossy() },
        ]}),
    );
    assert_eq!(r["ok"], true, "{r}");
    let stored = settings()["save_rules"].clone();
    assert_eq!(stored[0]["directory"], json!(inbox.to_string_lossy()));
    pb_shutdown();
}

/// **Validation happens on the way in**, and a single bad rule refuses the
/// whole write — leaving the stored list untouched rather than half-applied.
#[test]
#[serial_test::serial]
fn an_invalid_rule_refuses_the_write_and_leaves_the_stored_list_alone() {
    let dir = tempfile::tempdir().unwrap();
    init(dir.path());
    let good = dir.path().join("papers");

    // Establish a known-good list first.
    assert_eq!(
        call(
            pb_rules_set,
            &json!({ "rules": [{ "directory": good.to_string_lossy() }] })
        )["ok"],
        true
    );

    for bad in [
        json!({ "directory": "relative/path" }),
        json!({ "directory": dir.path().join("..").join("up").to_string_lossy() }),
        json!({ "directory": dir.path().join("missing").join("leaf").to_string_lossy() }),
    ] {
        let r = call(
            pb_rules_set,
            &json!({ "rules": [
                { "directory": good.to_string_lossy() },
                bad,
            ]}),
        );
        assert_eq!(r["ok"], false, "must refuse: {r}");
        assert_eq!(r["error"]["code"], "invalid_argument");
        assert!(
            r["error"]["message"].as_str().unwrap().contains("rule 1"),
            "must name which rule: {r}"
        );

        let stored = settings()["save_rules"].clone();
        assert_eq!(
            stored.as_array().map(Vec::len),
            Some(1),
            "a refused write must not disturb what is stored: {stored}"
        );
    }
    pb_shutdown();
}

/// A rule with no criteria at all is a legitimate catch-all, not a rejected
/// empty rule.
#[test]
#[serial_test::serial]
fn a_rule_with_no_criteria_is_accepted_as_a_catch_all() {
    let dir = tempfile::tempdir().unwrap();
    init(dir.path());
    let inbox = dir.path().join("inbox");

    let r = call(
        pb_rules_set,
        &json!({ "rules": [{ "directory": inbox.to_string_lossy() }] }),
    );
    assert_eq!(r["ok"], true, "{r}");
    let stored = settings()["save_rules"].clone();
    assert!(stored[0]["device"].is_null());
    assert!(stored[0]["extension"].is_null());
    assert!(stored[0]["min_bytes"].is_null());
    assert!(stored[0]["max_bytes"].is_null());
    pb_shutdown();
}

/// **Rules are not an acceptance setting.** Writing them must not disturb
/// `auto_accept` — the one setting that *does* skip the prompt — in either
/// direction, and `pb_rules_set` has no way to express one (I6).
#[test]
#[serial_test::serial]
fn writing_rules_does_not_touch_auto_accept() {
    let dir = tempfile::tempdir().unwrap();
    init(dir.path());

    // Whatever auto-accept is, rules must leave it there.
    for want in [true, false] {
        let s = call(pb_settings_set, &json!({ "auto_accept": want }));
        assert_eq!(s["ok"], true, "{s}");

        let r = call(
            pb_rules_set,
            &json!({ "rules": [{ "directory": dir.path().join("inbox").to_string_lossy() }] }),
        );
        assert_eq!(r["ok"], true, "{r}");

        assert_eq!(
            settings()["auto_accept"],
            json!(want),
            "a rules write must not change the approval setting"
        );
    }

    // And a rule cannot smuggle one in: unknown fields are not acceptance
    // knobs, they are simply not part of the type.
    let r = call(
        pb_rules_set,
        &json!({ "rules": [{
            "directory": dir.path().join("inbox").to_string_lossy(),
            "auto_accept": true,
            "accept": true,
        }]}),
    );
    assert_eq!(r["ok"], true, "unknown keys are ignored, not honoured: {r}");
    let stored = settings()["save_rules"].clone();
    assert!(
        stored[0].get("auto_accept").is_none() && stored[0].get("accept").is_none(),
        "no acceptance field may survive into a stored rule: {stored}"
    );
    assert_eq!(
        settings()["auto_accept"],
        json!(false),
        "and the real setting is still where it was left"
    );
    pb_shutdown();
}

/// `rules_supported` is a fact about the build, so `pb_settings_set` must not
/// be able to overwrite it — the same protection `trusted_devices` has.
#[test]
#[serial_test::serial]
fn rules_supported_is_managed_and_cannot_be_set() {
    let dir = tempfile::tempdir().unwrap();
    init(dir.path());

    let s = call(pb_settings_set, &json!({ "rules_supported": false }));
    assert_eq!(s["ok"], true, "{s}");
    assert_eq!(
        settings()["rules_supported"],
        true,
        "a claim from the UI must not override what the build can do"
    );
    pb_shutdown();
}

/// A malformed payload is refused rather than storing nothing silently.
#[test]
#[serial_test::serial]
fn a_payload_without_a_rules_list_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    init(dir.path());
    let r = call(pb_rules_set, &json!({ "nope": 1 }));
    assert_eq!(r["ok"], false);
    assert_eq!(r["error"]["code"], "invalid_argument");
    pb_shutdown();
}

/// The persisted rules reach the **engine's** config on the next init, which is
/// what the receive path actually reads. Without this the list would be a
/// setting the UI could edit and nothing would ever consult.
#[test]
#[serial_test::serial]
fn stored_rules_reach_the_engine_config_on_the_next_init() {
    let dir = tempfile::tempdir().unwrap();
    init(dir.path());
    let papers = dir.path().join("papers");
    assert_eq!(
        call(
            pb_rules_set,
            &json!({ "rules": [{ "extension": "pdf", "directory": papers.to_string_lossy() }] })
        )["ok"],
        true
    );
    pb_shutdown();

    // A second init against the same data directory reads the document back.
    init(dir.path());
    let stored = settings()["save_rules"].clone();
    assert_eq!(stored[0]["directory"], json!(papers.to_string_lossy()));

    // And the config the engine was built from carries it, which is the half
    // that matters: `settings::overlay` is what puts it there.
    let raw =
        std::fs::read_to_string(dir.path().join("data").join("ffi_settings.json")).expect("doc");
    let doc: Value = serde_json::from_str(&raw).unwrap();
    let rules: Vec<peerbeam_config::SaveRule> =
        serde_json::from_value(doc["save_rules"].clone()).expect("the stored list is the type");
    assert_eq!(rules.len(), 1);
    assert_eq!(rules[0].extension.as_deref(), Some("pdf"));
    assert_eq!(
        peerbeam_config::rules::matching_directory(&rules, "pb-anyone", "paper.pdf", 10),
        Some(papers.to_string_lossy().as_ref()),
        "the stored list must be the list the matcher reads"
    );
    pb_shutdown();
}
