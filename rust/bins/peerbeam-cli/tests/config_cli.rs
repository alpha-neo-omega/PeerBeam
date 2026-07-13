//! End-to-end CLI config tests: drive the real compiled `peerbeam` binary
//! against a throwaway config file. Cross-process, cross-platform, and exercises
//! the whole `config` command incl. dotted-key get/set and exit codes.

use std::path::Path;
use std::process::{Command, Output};

fn run(cfg: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_peerbeam"))
        .arg("--config")
        .arg(cfg)
        .args(args)
        .output()
        .expect("run peerbeam")
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).trim().to_string()
}

#[test]
fn set_then_get_round_trips_through_the_file() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = dir.path().join("cfg.json");

    // Set writes (and creates) the file from defaults.
    let set = run(&cfg, &["config", "set", "device.name", "IntegrationBox"]);
    assert!(set.status.success(), "set failed: {:?}", set);
    assert!(cfg.exists(), "set must create the config file");

    // Get reads it back.
    let got = run(&cfg, &["config", "get", "device.name"]);
    assert!(got.status.success());
    assert_eq!(stdout(&got), "IntegrationBox");
}

#[test]
fn set_coerces_json_typed_values() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = dir.path().join("cfg.json");

    assert!(
        run(&cfg, &["config", "set", "transfer.max_concurrent", "9"])
            .status
            .success()
    );
    let got = run(
        &cfg,
        &["config", "get", "transfer.max_concurrent", "--json"],
    );
    assert!(got.status.success());
    // `--json` emits the raw JSON value: an integer, not a quoted string.
    assert_eq!(stdout(&got), "9");
}

#[test]
fn get_unknown_key_exits_not_found() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = dir.path().join("cfg.json");
    let out = run(&cfg, &["config", "get", "device.bogus"]);
    // CliError::NotFound → exit code 3.
    assert_eq!(
        out.status.code(),
        Some(3),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn set_unknown_key_exits_usage() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = dir.path().join("cfg.json");
    let out = run(&cfg, &["config", "set", "nope.field", "1"]);
    // CliError::Usage → exit code 2.
    assert_eq!(
        out.status.code(),
        Some(2),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn path_prints_the_override() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = dir.path().join("cfg.json");
    let out = run(&cfg, &["config", "path"]);
    assert!(out.status.success());
    assert_eq!(stdout(&out), cfg.to_string_lossy());
}

#[test]
fn show_emits_valid_json() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = dir.path().join("cfg.json");
    let out = run(&cfg, &["config", "show", "--json"]);
    assert!(out.status.success());
    let v: serde_json::Value =
        serde_json::from_str(&stdout(&out)).expect("show --json is valid json");
    assert!(
        v.get("device").is_some(),
        "config must contain device section"
    );
}
