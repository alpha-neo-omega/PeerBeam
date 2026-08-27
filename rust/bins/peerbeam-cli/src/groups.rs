//! `peerbeam group …` — conversations a set of devices all share.
//!
//! # A Group is not a Space, and the CLI must not let them blur
//!
//! `peerbeam space` is a private label: nothing about it reaches a peer, so no
//! member learns who else is in one, and the price is that there are no group
//! replies. `peerbeam group` is the opposite trade — every member holds the
//! same roster, replies reach everyone, and **every member learns every other
//! member**. That disclosure is the entire cost of the feature and cannot be
//! withdrawn once made, which is why the commands that cause it say so before
//! they do it rather than after.
//!
//! # No hub, and nothing here to make one
//!
//! There is no command to ask another device who is in a group, because there
//! is nothing to ask: every member holds the roster. A send is N ordinary
//! one-to-one sends, each through the same per-device permission check a
//! hand-addressed message passes — membership grants nothing. Permitted by
//! amendment A2 in `docs/ARCHITECTURAL_INVARIANTS.md`, on the conditions it
//! lists.

use peerbeam_groups::{Group, GroupError, GroupStore};

use crate::cli::GroupAction;
use crate::commands::{group_store, load_config};
use crate::exit::{CliError, CliResult};
use crate::output::Ctx;

pub async fn group(ctx: &Ctx, action: GroupAction, path_override: Option<&str>) -> CliResult {
    let config = load_config(path_override)?;
    let store = group_store(&config)?;
    match action {
        GroupAction::List => list(ctx, &store),
        GroupAction::Create { name } => create(ctx, &store, &name),
        GroupAction::Rename { group, name } => {
            let id = resolve(&store, &group)?;
            rename(ctx, &store, &id, &name)
        }
        GroupAction::Invite { group, device } => {
            let id = resolve(&store, &group)?;
            invite(ctx, &store, &id, &device, path_override).await
        }
        GroupAction::Accept { group } => accept(ctx, &store, &group, path_override).await,
        GroupAction::Leave { group } => {
            let id = resolve(&store, &group)?;
            leave(ctx, &store, &id, path_override).await
        }
        GroupAction::Send { group, text } => {
            let id = resolve(&store, &group)?;
            send(ctx, &store, &id, &text, path_override).await
        }
        GroupAction::History { group } => {
            let id = resolve(&store, &group)?;
            history(ctx, &config, &id)
        }
    }
}

/// Turn what a person typed into a group id: exact id, then exact name, then
/// unambiguous name prefix — the same ladder `send --to` and `trust approve`
/// climb, so one rule is learned once.
fn resolve(store: &GroupStore, query: &str) -> Result<String, CliError> {
    let groups = store.list().map_err(err)?;
    if let Some(g) = groups.iter().find(|g| g.id == query) {
        return Ok(g.id.clone());
    }
    let folded = peerbeam_groups::normalise(query);
    let exact: Vec<&Group> = groups
        .iter()
        .filter(|g| peerbeam_groups::normalise(&g.name) == folded)
        .collect();
    if exact.len() == 1 {
        return Ok(exact[0].id.clone());
    }
    let prefix: Vec<&Group> = groups
        .iter()
        .filter(|g| peerbeam_groups::normalise(&g.name).starts_with(&folded))
        .collect();
    match prefix.len() {
        1 => Ok(prefix[0].id.clone()),
        0 => Err(CliError::NotFound(format!("no group called {query}"))),
        // Named, never guessed: picking one of several would act on a group the
        // person did not mean, and a group action is visible to other people.
        _ => Err(CliError::Usage(format!(
            "{query} matches {} groups: {}",
            prefix.len(),
            prefix
                .iter()
                .map(|g| g.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ))),
    }
}

fn err(e: GroupError) -> CliError {
    match e {
        GroupError::UnknownGroup { .. } => CliError::NotFound(e.to_string()),
        GroupError::Unreadable { .. } | GroupError::Unwritable { .. } => {
            CliError::Other(e.to_string())
        }
        _ => CliError::Usage(e.to_string()),
    }
}

fn list(ctx: &Ctx, store: &GroupStore) -> CliResult {
    let groups = store.list().map_err(err)?;
    if ctx.json {
        for g in &groups {
            let (live, stale) = store.reachable(&g.id).map_err(err)?;
            ctx.json_line(&serde_json::json!({
                "event": "group",
                "id": g.id,
                "name": g.name,
                "members": g.members.iter().map(|m| m.0.clone()).collect::<Vec<_>>(),
                "reachable": live.iter().map(|m| m.0.clone()).collect::<Vec<_>>(),
                "unreachable": stale.iter().map(|m| m.0.clone()).collect::<Vec<_>>(),
            }));
        }
        return Ok(());
    }
    if groups.is_empty() {
        ctx.line(&ctx.dim("no groups yet — `peerbeam group create <name>` starts one"));
        return Ok(());
    }
    for g in &groups {
        let (live, stale) = store.reachable(&g.id).map_err(err)?;
        ctx.line(&format!("{}  {}", g.name, ctx.dim(&g.id)));
        for m in &live {
            ctx.line(&format!("  {}", m.0));
        }
        // Named rather than hidden: a member this device can no longer message
        // is still in the group, and a list that quietly shrank would leave
        // somebody wondering whether they had ever added them.
        for m in &stale {
            ctx.line(&format!(
                "  {}  {}",
                ctx.dim(&m.0),
                ctx.dim("cannot be messaged — not approved, or chat withheld")
            ));
        }
    }
    Ok(())
}

fn create(ctx: &Ctx, store: &GroupStore, name: &str) -> CliResult {
    let g = store.create(name).map_err(err)?;
    if ctx.json {
        ctx.json_line(&serde_json::json!({
            "event": "group_created", "id": g.id, "name": g.name,
        }));
        return Ok(());
    }
    ctx.line(&format!("created {} ({})", g.name, ctx.dim(&g.id)));
    ctx.line(&ctx.dim(
        "  invite a device with `peerbeam group invite` — it joins when its own user accepts",
    ));
    Ok(())
}

fn rename(ctx: &Ctx, store: &GroupStore, id: &str, name: &str) -> CliResult {
    let g = store.rename(id, name).map_err(err)?;
    if ctx.json {
        ctx.json_line(&serde_json::json!({
            "event": "group_renamed", "id": g.id, "name": g.name,
        }));
        return Ok(());
    }
    ctx.line(&format!("renamed to {}", g.name));
    // Said every time, because it is the surprising half: a rename that looked
    // shared would leave two people describing one conversation differently and
    // each assuming the other saw their name.
    ctx.line(&ctx.dim("  on this device only — names are not shared"));
    Ok(())
}

fn history(ctx: &Ctx, config: &peerbeam_config::EngineConfig, id: &str) -> CliResult {
    let sc = crate::commands::SecureCtx::build(config)?;
    let store = crate::commands::chat_store(config, &sc.enc, &sc.ident);
    let rows = store
        .group_history(id)
        .map_err(|e| CliError::Other(e.to_string()))?;
    if ctx.json {
        for r in &rows {
            ctx.json_line(&serde_json::json!({
                "event": "group_message",
                "id": r.id,
                "from": r.peer_id,
                "direction": format!("{:?}", r.direction).to_lowercase(),
                "timestamp": r.timestamp,
                "body": r.body,
            }));
        }
        return Ok(());
    }
    if rows.is_empty() {
        ctx.line(&ctx.dim("no messages yet"));
        return Ok(());
    }
    for r in &rows {
        let who = match r.direction {
            peerbeam_chat::Direction::Out => "you".to_string(),
            peerbeam_chat::Direction::In => r.peer_id.clone(),
        };
        ctx.line(&format!("{}  {}", ctx.dim(&who), r.body));
    }
    Ok(())
}

/// Everything below needs the network, and each is a thin orchestration over
/// paths that already exist: the group layer decides *who*, and the chat layer
/// does the sending exactly as it would for a message typed by hand.
async fn invite(
    _ctx: &Ctx,
    _store: &GroupStore,
    _id: &str,
    _device: &str,
    _path_override: Option<&str>,
) -> CliResult {
    Err(CliError::Unavailable(
        "group invitations need the session wiring that is not built yet".into(),
    ))
}

async fn accept(
    _ctx: &Ctx,
    _store: &GroupStore,
    _id: &str,
    _path_override: Option<&str>,
) -> CliResult {
    Err(CliError::Unavailable(
        "accepting an invitation needs the session wiring that is not built yet".into(),
    ))
}

async fn leave(
    _ctx: &Ctx,
    _store: &GroupStore,
    _id: &str,
    _path_override: Option<&str>,
) -> CliResult {
    Err(CliError::Unavailable(
        "leaving needs the session wiring that is not built yet".into(),
    ))
}

async fn send(
    _ctx: &Ctx,
    _store: &GroupStore,
    _id: &str,
    _text: &str,
    _path_override: Option<&str>,
) -> CliResult {
    Err(CliError::Unavailable(
        "sending to a group needs the session wiring that is not built yet".into(),
    ))
}
