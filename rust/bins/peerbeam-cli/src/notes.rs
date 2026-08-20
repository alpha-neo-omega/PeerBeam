//! `peerbeam notes` — write and read notes kept on this device.
//!
//! A note is text with a title and a last-edited time. Deleting leaves a
//! **tombstone** rather than removing the row, so that once notes sync, a
//! deletion can reach the devices granted the `notes` permission: a row that
//! simply vanished would be indistinguishable from one they had not seen yet,
//! and the next exchange would offer it back.

use std::io::Read;
use std::sync::Arc;

use peerbeam_domain::id::DeviceId;
use peerbeam_engine::RouteManager;
use peerbeam_notes::Note;
use peerbeam_transfer_quic::QuicTransport;

use crate::session_transfer;

use crate::cli::NotesAction;
use crate::commands::{self, SecureCtx};
use crate::exit::{CliError, CliResult};
use crate::output::Ctx;

pub async fn notes(ctx: &Ctx, action: NotesAction, path_override: Option<&str>) -> CliResult {
    match action {
        NotesAction::List => list(ctx, path_override),
        NotesAction::Add { body, title } => add(ctx, body, title, path_override),
        NotesAction::Edit { id, body, title } => edit(ctx, &id, body, title, path_override),
        NotesAction::Remove { id } => remove(ctx, &id, path_override),
        NotesAction::Sync { peer } => sync(ctx, peer, path_override).await,
    }
}

/// `peerbeam notes sync <PEER>`.
///
/// Sends this device's whole set and merges what comes back. The permission is
/// checked here before anything is dialled, and again against the
/// **authenticated** identity after the handshake — the name on the command
/// line resolves to whatever discovery believes, which is not yet proof of who
/// answered.
async fn sync(ctx: &Ctx, peer: String, path_override: Option<&str>) -> CliResult {
    let config = commands::load_config(path_override)?;
    let sc = SecureCtx::build(&config)?;
    let store = commands::note_store(&config, &sc.enc, &sc.ident);

    let devices = commands::snapshot(config.clone(), 2).await?;
    let candidates: Vec<(String, String)> = devices
        .iter()
        .map(|m| (m.device.id.to_string(), m.device.name.clone()))
        .collect();
    let index = commands::resolve_peer(ctx, &candidates, &Some(peer))?;
    let device = devices[index].device.clone();

    if !peerbeam_notes::may_sync_notes(sc.trust.as_ref(), &device.id) {
        return Err(CliError::Usage(format!(
            "{} has not been granted the notes permission — \
             run `peerbeam trust permit {} notes`",
            device.name, device.id
        )));
    }

    let quic = Arc::new(QuicTransport::new().map_err(CliError::from)?);
    let routes = RouteManager::new(quic.clone());
    let session = session_transfer::dial(
        &quic, &routes, &device, "notes", &sc.ident, &sc.enc, &sc.trust, None,
    )
    .await
    .map_err(|e| CliError::Other(format!("could not reach {}: {e}", device.name)))?;

    let authenticated = DeviceId::from(session.peer_id.clone());
    if !peerbeam_notes::may_sync_notes(sc.trust.as_ref(), &authenticated) {
        session.close().await;
        return Err(CliError::Usage(format!(
            "{authenticated} has not been granted the notes permission"
        )));
    }
    if !session.supports_notes() {
        session.close().await;
        return Err(CliError::Other(format!(
            "{} is running a build without notes",
            device.name
        )));
    }

    let mine = store.all().map_err(|e| CliError::Other(e.to_string()))?;
    let batches = peerbeam_notes::NoteBatch::split(mine, false);
    let count: usize = batches.iter().map(|b| b.notes.len()).sum();
    let sent = session_transfer::send_note_batches(&session.handle, &batches).await;
    session.close().await;
    sent.map_err(|e| CliError::Other(format!("sending notes: {e}")))?;

    if ctx.json {
        ctx.json_line(&serde_json::json!({
            "peer": authenticated.0, "sent": count,
        }));
    } else {
        // What was *sent*, not what the peer now has: its answer merges into
        // this device asynchronously, and claiming a converged total here would
        // be a number nobody measured.
        ctx.line(&format!("sent {count} notes to {}", ctx.bold(&device.name)));
    }
    Ok(())
}

/// The note's text: the argument when given, otherwise stdin.
///
/// Stdin rather than an editor, so `pbpaste | peerbeam notes add` and
/// `peerbeam notes add < draft.md` both work over SSH, where launching an
/// editor is the wrong thing to do to someone's terminal.
fn body_from(arg: Option<String>) -> Result<String, CliError> {
    if let Some(b) = arg {
        return Ok(b);
    }
    let mut buf = String::new();
    std::io::stdin()
        .read_to_string(&mut buf)
        .map_err(|e| CliError::Other(format!("reading the note from stdin: {e}")))?;
    if buf.trim().is_empty() {
        return Err(CliError::Usage(
            "provide the note's text as an argument or on stdin".into(),
        ));
    }
    Ok(buf)
}

fn store(
    path_override: Option<&str>,
) -> Result<(peerbeam_notes::NoteStore, peerbeam_config::EngineConfig), CliError> {
    let config = commands::load_config(path_override)?;
    let sc = SecureCtx::build(&config)?;
    let store = commands::note_store(&config, &sc.enc, &sc.ident);
    Ok((store, config))
}

fn list(ctx: &Ctx, path_override: Option<&str>) -> CliResult {
    let (store, _config) = store(path_override)?;
    let notes = store.list().map_err(|e| CliError::Other(e.to_string()))?;

    if ctx.json {
        ctx.json_line(&serde_json::json!({ "notes": notes }));
        return Ok(());
    }
    if notes.is_empty() {
        ctx.line(&ctx.dim("no notes yet"));
        return Ok(());
    }
    for n in &notes {
        let heading = if n.title.is_empty() {
            first_line(&n.body)
        } else {
            n.title.clone()
        };
        ctx.line(&format!(
            "{}  {}  {}",
            ctx.dim(&n.id),
            n.updated_at,
            ctx.bold(&heading)
        ));
    }
    Ok(())
}

/// The first line of a note, for a listing that has no title to show. Trimmed
/// to a width that keeps one note to one row — the id and time are what a
/// person copies from here; the text is only there to recognise it by.
fn first_line(body: &str) -> String {
    let line = body.lines().next().unwrap_or("").trim();
    if line.chars().count() <= 60 {
        return line.to_string();
    }
    let cut: String = line.chars().take(59).collect();
    format!("{cut}…")
}

fn add(ctx: &Ctx, body: Option<String>, title: Option<String>, cfg: Option<&str>) -> CliResult {
    let (store, _config) = store(cfg)?;
    let body = body_from(body)?;
    let note = Note::new(title.as_deref().unwrap_or(""), &body)
        .map_err(|e| CliError::Usage(e.to_string()))?;
    store
        .put(&note)
        .map_err(|e| CliError::Other(e.to_string()))?;

    if ctx.json {
        ctx.json_line(&serde_json::json!({ "id": note.id }));
    } else {
        ctx.line(&format!("added {}", ctx.bold(&note.id)));
    }
    Ok(())
}

fn edit(
    ctx: &Ctx,
    id: &str,
    body: Option<String>,
    title: Option<String>,
    cfg: Option<&str>,
) -> CliResult {
    let (store, _config) = store(cfg)?;
    let body = body_from(body)?;
    let updated = store
        .edit(id, title.as_deref().unwrap_or(""), &body)
        .map_err(|e| CliError::Usage(e.to_string()))?;

    // Deliberately an error rather than a quiet "0 changed": the user named a
    // specific note, and being told nothing happened is the point.
    //
    // The same in `--json` as on a terminal. An earlier version returned
    // success with `updated: false` under `--json`, which meant a script could
    // edit a note that no longer exists, get exit 0, and carry on believing the
    // edit landed — the one thing a machine-readable mode must not do.
    if !updated {
        return Err(CliError::NotFound(format!(
            "no note {id} to edit — it does not exist, or it was deleted"
        )));
    }
    if ctx.json {
        ctx.json_line(&serde_json::json!({ "id": id, "updated": true }));
    } else {
        ctx.line(&format!("edited {}", ctx.bold(id)));
    }
    Ok(())
}

fn remove(ctx: &Ctx, id: &str, cfg: Option<&str>) -> CliResult {
    let (store, _config) = store(cfg)?;
    let deleted = store
        .delete(id)
        .map_err(|e| CliError::Other(e.to_string()))?;

    // Same rule as `edit`: a named note that was not there is an error in every
    // output mode, so a script cannot mistake "nothing happened" for success.
    if !deleted {
        return Err(CliError::NotFound(format!(
            "no note {id} to delete — it does not exist, or it was already deleted"
        )));
    }
    if ctx.json {
        ctx.json_line(&serde_json::json!({ "id": id, "deleted": true }));
    } else {
        ctx.line(&format!("deleted {}", ctx.bold(id)));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use peerbeam_config::EngineConfig;

    fn quiet_ctx() -> Ctx {
        Ctx::new(true, true, 0, true, true)
    }

    /// An isolated `EngineConfig` rooted under `dir`, so nothing here touches
    /// the real `~/.config/peerbeam`.
    fn isolated_config(dir: &std::path::Path) -> EngineConfig {
        let mut config = EngineConfig::default();
        config.storage.data_directory = dir.join("data").to_string_lossy().into_owned();
        config.storage.save_directory = dir.join("recv").to_string_lossy().into_owned();
        config.transfer.port = 0;
        config
    }

    fn cfg(dir: &std::path::Path) -> (EngineConfig, std::path::PathBuf) {
        let config = isolated_config(dir);
        let path = dir.join("config.json");
        config.save(&path).expect("save config");
        (config, path)
    }

    #[test]
    fn add_list_edit_and_remove_round_trip() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (_config, path) = cfg(dir.path());
        let ctx = quiet_ctx();
        let p = path.to_str().expect("utf8");

        add(&ctx, Some("milk".into()), Some("Shopping".into()), Some(p)).expect("add");

        let (ns, _c) = store(Some(p)).expect("store");
        let listed = ns.list().expect("list");
        assert_eq!(listed.len(), 1);
        let id = listed[0].id.clone();
        assert_eq!(listed[0].body, "milk");

        edit(&ctx, &id, Some("milk, bread".into()), None, Some(p)).expect("edit");
        assert_eq!(ns.list().expect("list")[0].body, "milk, bread");

        remove(&ctx, &id, Some(p)).expect("remove");
        assert!(ns.list().expect("list").is_empty());
    }

    #[test]
    fn editing_or_removing_a_deleted_note_is_an_error_not_a_silent_no_op() {
        // The user named a specific note. Being told nothing happened is the
        // point; a quiet success would read as "done".
        let dir = tempfile::tempdir().expect("tempdir");
        let (_config, path) = cfg(dir.path());
        let ctx = quiet_ctx();
        let p = path.to_str().expect("utf8");

        add(&ctx, Some("temporary".into()), None, Some(p)).expect("add");
        let (ns, _c) = store(Some(p)).expect("store");
        let id = ns.list().expect("list")[0].id.clone();
        remove(&ctx, &id, Some(p)).expect("remove");

        let (check, _c2) = store(Some(p)).expect("store");
        let row = check.get(&id).expect("get");
        assert!(
            row.as_ref().is_some_and(|n| n.deleted),
            "precondition: the row should be a tombstone, got {row:?}"
        );
        let got = edit(&ctx, &id, Some("back".into()), None, Some(p));
        assert!(
            matches!(got, Err(CliError::NotFound(_))),
            "editing a deleted note did not report NotFound: {got:?}"
        );
        assert!(matches!(
            remove(&ctx, &id, Some(p)),
            Err(CliError::NotFound(_))
        ));
    }

    #[test]
    fn a_listing_shows_the_first_line_when_a_note_has_no_title() {
        assert_eq!(first_line("hello\nworld"), "hello");
        assert_eq!(first_line(""), "");
        let long = "x".repeat(80);
        let shown = first_line(&long);
        assert_eq!(shown.chars().count(), 60, "60 chars: 59 plus the ellipsis");
        assert!(shown.ends_with('\u{2026}'));
    }
}
