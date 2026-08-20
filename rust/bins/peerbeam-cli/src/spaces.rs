//! `peerbeam space …` — named local sets of trusted devices.
//!
//! # What a Space is, and what it is not
//!
//! A Space is a label this machine keeps over device ids it already trusts. It
//! is never sent anywhere, no peer is told it exists, and there is no group
//! protocol on the wire. Sending to a Space is N ordinary one-to-one sends over
//! sessions that already exist.
//!
//! That is not a shortcut, it is the design: invariant I3 forbids anything that
//! only works through a coordinating hub, and VISION.md rules out hub-brokered
//! group chat. It is also a privacy property worth stating plainly — **no
//! member learns who else is in a Space**, because nothing about the Space
//! reaches them.
//!
//! Membership grants nothing (I6). Every send in a fan-out passes the same
//! per-capability gate a hand-typed send would.

use peerbeam_domain::id::DeviceId;
use peerbeam_spaces::{SpaceStore, SpaceView};

use crate::cli::SpaceAction;
use crate::commands::{load_config, space_store};
use crate::exit::{CliError, CliResult};
use crate::output::Ctx;

pub async fn space(ctx: &Ctx, action: SpaceAction, path_override: Option<&str>) -> CliResult {
    let config = load_config(path_override)?;
    let store = space_store(&config)?;
    match action {
        SpaceAction::List => list(ctx, &store),
        SpaceAction::Create { name } => create(ctx, &store, &name),
        SpaceAction::Rename { id, name } => rename(ctx, &store, &resolve(&store, &id)?, &name),
        SpaceAction::Delete { id } => delete(ctx, &store, &resolve(&store, &id)?),
        SpaceAction::Add { id, device } => add(ctx, &store, &resolve(&store, &id)?, &device),
        SpaceAction::Remove { id, device } => remove(ctx, &store, &resolve(&store, &id)?, &device),
        SpaceAction::Send { space, paths } => send(ctx, &store, space, paths, path_override).await,
    }
}

/// Turn what the user typed into a Space id.
///
/// Names are what a person remembers and ids are what the store keys on, so
/// every command takes either. The name is tried first: an id is 16 hex
/// characters and nobody types one from memory, so treating the argument as a
/// name first is both the common case and the one a typo should be reported
/// against.
fn resolve(store: &SpaceStore, what: &str) -> Result<String, CliError> {
    if let Some(found) = store.by_name(what).map_err(err)? {
        return Ok(found.id);
    }
    if store.get(what).map_err(err)?.is_some() {
        return Ok(what.to_string());
    }
    Err(CliError::NotFound(format!(
        "no space called {what} — `peerbeam space list` shows them"
    )))
}

/// `peerbeam space send <SPACE> <PATH…>`.
///
/// **N ordinary sends.** There is no group transfer: each member gets the files
/// over its own session, through the same permission gate a hand-typed send
/// would pass, and none of them learns who else was in the fan-out. A member
/// this device no longer trusts is named and skipped rather than silently
/// dropped — the user needs to know the file did not go everywhere they meant.
async fn send(
    ctx: &Ctx,
    store: &SpaceStore,
    space: String,
    paths: Vec<String>,
    path_override: Option<&str>,
) -> CliResult {
    let id = resolve(store, &space)?;
    let view = store
        .get(&id)
        .map_err(err)?
        .ok_or_else(|| CliError::NotFound(format!("no space called {space}")))?;

    if view.live.is_empty() {
        // Distinguished from "empty space": one is nothing to do, the other is
        // a fan-out that would reach nobody and should say why.
        return Err(CliError::Usage(if view.stale.is_empty() {
            format!("{} has no members yet", view.name)
        } else {
            format!(
                "{} has {} member(s), none of them still trusted — pair with them again",
                view.name,
                view.stale.len()
            )
        }));
    }

    for m in &view.stale {
        ctx.line(&format!(
            "{}  {}",
            ctx.dim(&m.0),
            ctx.dim("skipped — no longer trusted")
        ));
    }

    let mut failed = 0usize;
    for member in &view.live {
        ctx.line(&format!("→ {}", member.0));
        let args = crate::cli::SendArgs {
            paths: paths.clone(),
            to: Some(member.0.clone()),
            at: None,
            // No override: a Space names devices, and how to reach each one is
            // route selection's job — the same as any other send.
            addr: None,
        };
        // Each send is independent on purpose: one unreachable member must not
        // cancel the rest, which is the whole reason a person used a Space
        // instead of typing the sends out.
        if let Err(e) = crate::commands::send_files(ctx, args, path_override).await {
            failed += 1;
            ctx.line(&format!("  {}", ctx.dim(&e.to_string())));
        }
    }

    if failed > 0 {
        return Err(CliError::Other(format!(
            "{failed} of {} member(s) did not receive it",
            view.live.len()
        )));
    }
    Ok(())
}

fn err(e: peerbeam_spaces::SpaceError) -> CliError {
    match e {
        peerbeam_spaces::SpaceError::NotFound(_) => CliError::NotFound(e.to_string()),
        peerbeam_spaces::SpaceError::Storage(_) => CliError::Other(e.to_string()),
        _ => CliError::Usage(e.to_string()),
    }
}

fn list(ctx: &Ctx, store: &SpaceStore) -> CliResult {
    let spaces = store.list().map_err(err)?;
    if ctx.json {
        for s in &spaces {
            ctx.json_line(&serde_json::to_value(s).unwrap_or_default());
        }
        return Ok(());
    }
    if spaces.is_empty() {
        ctx.line(&ctx.dim("no spaces — `peerbeam space create <name>` makes one"));
        return Ok(());
    }
    for s in &spaces {
        ctx.line(&format!("{}  {}", ctx.dim(&s.id), ctx.bold(&s.name)));
        for m in &s.live {
            ctx.line(&format!("    {}", m.0));
        }
        // Shown, never hidden. A member silently dropped from a list leaves
        // someone wondering whether they ever added it; naming it as stale
        // tells them the device needs re-pairing or removing.
        for m in &s.stale {
            ctx.line(&format!("    {}  {}", m.0, ctx.dim("(no longer trusted)")));
        }
    }
    Ok(())
}

fn say(ctx: &Ctx, s: &SpaceView, what: &str) -> CliResult {
    if ctx.json {
        ctx.json_line(&serde_json::to_value(s).unwrap_or_default());
    } else {
        ctx.line(&format!("{what} {} ({})", s.name, ctx.dim(&s.id)));
    }
    Ok(())
}

fn create(ctx: &Ctx, store: &SpaceStore, name: &str) -> CliResult {
    say(ctx, &store.create(name).map_err(err)?, "created")
}

fn rename(ctx: &Ctx, store: &SpaceStore, id: &str, name: &str) -> CliResult {
    say(ctx, &store.rename(id, name).map_err(err)?, "renamed to")
}

fn delete(ctx: &Ctx, store: &SpaceStore, id: &str) -> CliResult {
    let gone = store.delete(id).map_err(err)?;
    if ctx.json {
        ctx.json_line(&serde_json::json!({ "event": "space_deleted", "id": id, "deleted": gone }));
    } else if gone {
        // Worth saying out loud: deleting the label does not un-trust anybody.
        ctx.line("space deleted — the devices in it keep their trust");
    } else {
        ctx.line("no such space");
    }
    Ok(())
}

fn add(ctx: &Ctx, store: &SpaceStore, id: &str, device: &str) -> CliResult {
    let d = DeviceId::from(device.to_string());
    let added = store.add_member(id, &d).map_err(err)?;
    if ctx.json {
        ctx.json_line(
            &serde_json::json!({ "event": "space_member_added", "id": id, "device": device, "added": added }),
        );
    } else if added {
        ctx.line(&format!("{device} added — this grants it nothing new"));
    } else {
        ctx.line(&format!("{device} was already in it"));
    }
    Ok(())
}

fn remove(ctx: &Ctx, store: &SpaceStore, id: &str, device: &str) -> CliResult {
    let d = DeviceId::from(device.to_string());
    let removed = store.remove_member(id, &d).map_err(err)?;
    if ctx.json {
        ctx.json_line(
            &serde_json::json!({ "event": "space_member_removed", "id": id, "device": device, "removed": removed }),
        );
    } else if removed {
        ctx.line(&format!("{device} removed"));
    } else {
        ctx.line(&format!("{device} was not in it"));
    }
    Ok(())
}
