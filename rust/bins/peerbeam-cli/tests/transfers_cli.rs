//! End-to-end `peerbeam transfers`: the real compiled binary against a
//! throwaway data directory seeded with checkpoints.
//!
//! The command's job is to make the checkpoints on this machine reachable from
//! a shell — list them, refuse the resumes that must not happen, and reclaim
//! the ones the user is done with — so every assertion reads the checkpoint
//! directory back rather than trusting what was printed.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use chrono::Utc;
use peerbeam_config::EngineConfig;
use peerbeam_domain::entity::{Direction, FileEntry, TransferSession, TransferStatus};
use peerbeam_domain::id::{DeviceId, TransferId};
use serde_json::Value;

const BIN: &str = env!("CARGO_BIN_EXE_peerbeam");

fn config_in(dir: &Path) -> PathBuf {
    let data = dir.join("data");
    std::fs::create_dir_all(data.join("checkpoints")).unwrap();
    let mut cfg = EngineConfig::default();
    cfg.storage.data_directory = data.to_string_lossy().into_owned();
    cfg.storage.save_directory = dir.join("recv").to_string_lossy().into_owned();
    // Never touch the well-known discovery port from a test.
    cfg.discovery.port = 0;
    cfg.discovery.enabled = false;
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

fn json_lines(o: &Output) -> Vec<Value> {
    out(o)
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

/// Write a checkpoint straight into the store, as an interrupted run would
/// have left it.
fn seed(dir: &Path, id: &str, direction: Direction, dest: &Path, accepted: bool) {
    let cp = TransferSession {
        id: TransferId::from(id),
        peer: DeviceId::from("pb-peer-1"),
        direction,
        status: TransferStatus::Transferring,
        files: vec![FileEntry {
            path: dest.to_path_buf(),
            name: dest.file_name().unwrap().to_string_lossy().into_owned(),
            size: 4_000,
            mime_type: String::new(),
            checksum: None,
        }],
        total_bytes: 4_000,
        transferred_bytes: 1_500,
        started_at: Utc::now(),
        completed_at: None,
        is_resume: false,
        accepted,
    };
    std::fs::write(
        dir.join("data")
            .join("checkpoints")
            .join(format!("{id}.json")),
        serde_json::to_vec_pretty(&cp).unwrap(),
    )
    .unwrap();
}

fn checkpoint_exists(dir: &Path, id: &str) -> bool {
    dir.join("data")
        .join("checkpoints")
        .join(format!("{id}.json"))
        .exists()
}

#[test]
fn listing_reports_every_checkpoint_and_which_way_it_was_going() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = config_in(dir.path());
    seed(
        dir.path(),
        "out-1",
        Direction::Sending,
        &dir.path().join("movie.mkv"),
        true,
    );
    seed(
        dir.path(),
        "in-1",
        Direction::Receiving,
        &dir.path().join("recv").join("photos.zip"),
        true,
    );

    let o = run(&cfg, &["transfers", "list"]);
    assert_eq!(code(&o), 0, "{}", out(&o));
    let text = out(&o);
    assert!(text.contains("out-1"), "{text}");
    assert!(text.contains("in-1"), "{text}");
    assert!(text.contains("movie.mkv"), "{text}");

    let rows = json_lines(&run(&cfg, &["--json", "transfers", "list"]));
    assert_eq!(rows.len(), 2, "{rows:?}");
    let outgoing = rows.iter().find(|r| r["id"] == "out-1").unwrap();
    assert_eq!(outgoing["direction"], "sending");
    assert_eq!(outgoing["resumable"], true);
    assert_eq!(outgoing["transferred_bytes"], 1_500);
    assert_eq!(outgoing["total_bytes"], 4_000);
    // An incoming transfer cannot be pulled, and the listing says so rather
    // than offering an action that would do nothing.
    let incoming = rows.iter().find(|r| r["id"] == "in-1").unwrap();
    assert_eq!(incoming["resumable"], false);
}

#[test]
fn an_empty_store_lists_nothing_and_still_succeeds() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = config_in(dir.path());
    let o = run(&cfg, &["transfers", "list"]);
    assert_eq!(code(&o), 0, "{}", out(&o));
    assert!(out(&o).contains("no interrupted transfers"), "{}", out(&o));
    assert!(json_lines(&run(&cfg, &["--json", "transfers", "list"])).is_empty());
}

/// The bare command keeps the session/transport snapshot it always printed and
/// gains the interrupted list beside it — additive, so a script reading the old
/// shape keeps working.
#[test]
fn the_bare_command_still_prints_sessions_and_now_also_what_stopped() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = config_in(dir.path());
    seed(
        dir.path(),
        "out-1",
        Direction::Sending,
        &dir.path().join("movie.mkv"),
        true,
    );
    let o = run(&cfg, &["--json", "transfers"]);
    assert_eq!(code(&o), 0, "{}", out(&o));
    let v = &json_lines(&o)[0];
    assert!(v["sessions"].is_object(), "{v}");
    assert!(v["transport"].is_object(), "{v}");
    assert_eq!(v["interrupted"][0]["id"], "out-1", "{v}");
}

/// The binding check, from a shell: a source file that is no longer the file
/// the receiver holds a prefix of cannot be appended to it.
#[test]
fn resuming_a_send_whose_source_changed_size_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = config_in(dir.path());
    let src = dir.path().join("movie.mkv");
    // The checkpoint says 4,000 bytes; the file on disk is not that file.
    std::fs::write(&src, vec![7u8; 9_000]).unwrap();
    seed(dir.path(), "out-1", Direction::Sending, &src, true);

    let o = run(&cfg, &["transfers", "resume", "out-1"]);
    assert_eq!(code(&o), 2, "a refused resume is a usage error: {o:?}");
    assert!(
        checkpoint_exists(dir.path(), "out-1"),
        "a refusal must not destroy the checkpoint it refused"
    );
}

/// Consent is not laundered through a shell either.
#[test]
fn resuming_a_never_accepted_transfer_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = config_in(dir.path());
    let src = dir.path().join("movie.mkv");
    std::fs::write(&src, vec![7u8; 4_000]).unwrap();
    // Everything matches — peer, name, size. The only difference is that
    // nobody ever agreed to this transfer.
    seed(dir.path(), "out-1", Direction::Sending, &src, false);

    let o = run(&cfg, &["transfers", "resume", "out-1"]);
    assert_eq!(code(&o), 2, "{o:?}");
    let err = String::from_utf8_lossy(&o.stderr).to_string();
    assert!(err.contains("never accepted"), "{err}");
}

#[test]
fn an_incoming_transfer_says_its_sender_must_offer_it_again() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = config_in(dir.path());
    seed(
        dir.path(),
        "in-1",
        Direction::Receiving,
        &dir.path().join("recv").join("photos.zip"),
        true,
    );
    let o = run(&cfg, &["transfers", "resume", "in-1"]);
    assert_eq!(code(&o), 2, "{o:?}");
    let err = String::from_utf8_lossy(&o.stderr).to_string();
    assert!(err.contains("sender"), "{err}");
}

#[test]
fn resuming_an_id_that_matches_nothing_is_a_not_found() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = config_in(dir.path());
    let o = run(&cfg, &["transfers", "resume", "ghost"]);
    assert_eq!(code(&o), 3, "{o:?}");
}

#[test]
fn discarding_removes_the_checkpoint_and_the_partial_file() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = config_in(dir.path());
    let recv = dir.path().join("recv");
    std::fs::create_dir_all(&recv).unwrap();
    let dest = recv.join("photos.zip");
    let part = recv.join("photos.zip.part");
    std::fs::write(&part, vec![1u8; 1_500]).unwrap();
    seed(dir.path(), "in-1", Direction::Receiving, &dest, true);

    let o = run(&cfg, &["--json", "transfers", "discard", "in-1"]);
    assert_eq!(code(&o), 0, "{}", out(&o));
    let v = &json_lines(&o)[0];
    assert_eq!(v["event"], "discarded");
    assert_eq!(v["partial_removed"], true, "{v}");
    assert!(
        !part.exists(),
        "the partial bytes go with the record — otherwise a transfer the user \
         threw away would seed the next one of the same name"
    );
    assert!(!checkpoint_exists(dir.path(), "in-1"));

    // And doing it again is a clear "there is nothing there".
    assert_eq!(code(&run(&cfg, &["transfers", "discard", "in-1"])), 3);
}

/// Discarding an outgoing transfer touches only the record: the source file is
/// the user's, and nothing about giving up on a send makes it ours to delete.
#[test]
fn discarding_a_send_never_touches_the_source_file() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = config_in(dir.path());
    let src = dir.path().join("movie.mkv");
    std::fs::write(&src, vec![7u8; 4_000]).unwrap();
    seed(dir.path(), "out-1", Direction::Sending, &src, true);

    let o = run(&cfg, &["--json", "transfers", "discard", "out-1"]);
    assert_eq!(code(&o), 0, "{}", out(&o));
    assert_eq!(json_lines(&o)[0]["partial_removed"], false);
    assert!(src.exists(), "the source file is not ours to delete");
    assert!(!checkpoint_exists(dir.path(), "out-1"));
}
