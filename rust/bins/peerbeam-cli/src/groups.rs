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

/// Dial one member and hand the session to `f`.
///
/// **One dial per member, and that is the design rather than a shortcut.** A
/// group message is N ordinary one-to-one sends over the same routes any other
/// message takes (A2, condition 2); there is no group connection to open,
/// because there is no group on the wire.
///
/// The trust check is made against the **authenticated** peer, not the id we
/// dialled: those differ when a route resolves to a device other than the one
/// expected, and the permission is about who actually answered.
async fn with_member<F>(
    ctx: &Ctx,
    config: &peerbeam_config::EngineConfig,
    member: &peerbeam_domain::id::DeviceId,
    f: F,
) -> Result<(), CliError>
where
    F: for<'a> FnOnce(
        &'a crate::session_transfer::Session,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<(), CliError>> + 'a>,
    >,
{
    let sc = crate::commands::SecureCtx::build(config)?;
    let devices = crate::commands::snapshot(config.clone(), 2).await?;
    let found = devices
        .iter()
        .find(|d| d.device.id.to_string() == member.0)
        .ok_or_else(|| CliError::NotFound(format!("{} is not reachable", member.0)))?;

    let quic =
        std::sync::Arc::new(peerbeam_transfer_quic::QuicTransport::new().map_err(CliError::from)?);
    let routes = peerbeam_engine::RouteManager::new(quic.clone());
    let session = crate::session_transfer::dial(
        &quic,
        &routes,
        &found.device,
        "group",
        &sc.ident,
        &sc.enc,
        &sc.trust,
        None,
    )
    .await?;

    // Asked of the identity that actually answered — see the doc above.
    let peer = peerbeam_domain::id::DeviceId::from(session.peer_id.clone());
    if !peerbeam_chat::may_exchange_chat(sc.trust.as_ref(), &peer) {
        ctx.line(&format!(
            "  {}  {}",
            ctx.dim(&peer.0),
            ctx.dim("skipped — this device may not exchange messages")
        ));
        session.handle.close();
        return Ok(());
    }

    let out = f(&session).await;
    session.handle.close();
    out
}

/// Offer a device a place in a group.
async fn invite(
    ctx: &Ctx,
    store: &GroupStore,
    id: &str,
    device: &str,
    path_override: Option<&str>,
) -> CliResult {
    let config = load_config(path_override)?;
    let group = store.get(id).map_err(err)?;

    // Resolved through discovery like any other peer reference, so a name or a
    // prefix works here exactly as it does for `send --to`.
    let devices = crate::commands::snapshot(config.clone(), 2).await?;
    let candidates: Vec<(String, String)> = devices
        .iter()
        .map(|m| (m.device.id.to_string(), m.device.name.clone()))
        .collect();
    let index = crate::commands::resolve_peer(ctx, &candidates, &Some(device.to_string()))?;
    let target = peerbeam_domain::id::DeviceId::from(devices[index].device.id.to_string());

    let invite = peerbeam_groups::GroupInvite {
        group: group.id.clone(),
        name: group.name.clone(),
        members: group.members.clone(),
    };
    let payload = serde_json::to_vec(&invite)
        .map_err(|e| CliError::Other(format!("could not encode the invitation: {e}")))?;

    with_member(ctx, &config, &target, |session| {
        let payload = payload.clone();
        Box::pin(async move {
            peerbeam_chat::send_foreign(
                &session.handle,
                peerbeam_groups::GroupInvite::message_type(),
                payload,
            )
            .await
            .map_err(|e| CliError::Connection(e.to_string()))
        })
    })
    .await?;

    ctx.line(&format!("invited {} to {}", target.0, group.name));
    // Said at the moment it becomes true, not buried in a help page: this is
    // the disclosure the whole feature costs (A2, condition 5).
    ctx.line(&ctx.dim(
        "  they can see who is already in the group; everyone in it will see them if they accept",
    ));
    ctx.line(&ctx.dim("  nothing happens on their device until they accept"));
    Ok(())
}

/// Accept an invitation: adopt the roster, then tell every member.
async fn accept(
    ctx: &Ctx,
    store: &GroupStore,
    group: &str,
    path_override: Option<&str>,
) -> CliResult {
    let config = load_config(path_override)?;
    let pending = store
        .invite(group)
        .map_err(err)?
        .ok_or_else(|| CliError::NotFound(format!("no pending invitation to {group}")))?;

    // Adopted first, so a join that half-fails leaves this device in the group
    // it agreed to join rather than in nothing. The members who did not hear
    // find out when they next receive anything from here.
    let joined = store
        .adopt(&pending.group, &pending.name, &pending.members)
        .map_err(err)?;
    store.forget_invite(&pending.group).map_err(err)?;

    let announce = peerbeam_groups::GroupJoined {
        group: joined.id.clone(),
    };
    let payload = serde_json::to_vec(&announce)
        .map_err(|e| CliError::Other(format!("could not encode the announcement: {e}")))?;

    let mut told = 0usize;
    for member in &pending.members {
        let payload = payload.clone();
        // One member being unreachable must not stop the rest being told —
        // the same rule `space send` follows, and for the same reason.
        match with_member(ctx, &config, member, move |session| {
            let payload = payload.clone();
            Box::pin(async move {
                peerbeam_chat::send_foreign(
                    &session.handle,
                    peerbeam_groups::GroupJoined::message_type(),
                    payload,
                )
                .await
                .map_err(|e| CliError::Connection(e.to_string()))
            })
        })
        .await
        {
            Ok(()) => told += 1,
            Err(e) => ctx.line(&format!(
                "  {}  {}",
                ctx.dim(&member.0),
                ctx.dim(&e.to_string())
            )),
        }
    }

    ctx.line(&format!("joined {}", joined.name));
    ctx.line(&ctx.dim(&format!(
        "  told {told} of {} member(s); the rest find out when they next hear from you",
        pending.members.len()
    )));
    Ok(())
}

/// Leave a group: tell the members, then forget it here.
async fn leave(ctx: &Ctx, store: &GroupStore, id: &str, path_override: Option<&str>) -> CliResult {
    let config = load_config(path_override)?;
    let group = store.get(id).map_err(err)?;
    let me_out = peerbeam_groups::GroupLeft {
        group: group.id.clone(),
    };
    let payload = serde_json::to_vec(&me_out)
        .map_err(|e| CliError::Other(format!("could not encode the message: {e}")))?;

    let (live, stale) = store.reachable(&group.id).map_err(err)?;
    for m in &stale {
        ctx.line(&format!(
            "  {}  {}",
            ctx.dim(&m.0),
            ctx.dim("not told — this device may not message it")
        ));
    }
    for member in &live {
        let payload = payload.clone();
        if let Err(e) = with_member(ctx, &config, member, move |session| {
            let payload = payload.clone();
            Box::pin(async move {
                peerbeam_chat::send_foreign(
                    &session.handle,
                    peerbeam_groups::GroupLeft::message_type(),
                    payload,
                )
                .await
                .map_err(|e| CliError::Connection(e.to_string()))
            })
        })
        .await
        {
            ctx.line(&format!(
                "  {}  {}",
                ctx.dim(&member.0),
                ctx.dim(&e.to_string())
            ));
        }
    }

    // Forgotten regardless of who heard. Leaving is a decision about this
    // device, and a member that never got the message keeps sending — which is
    // what `trust revoke-permission <device> chat` is for, and is said here
    // rather than left to be discovered.
    store.forget(&group.id).map_err(err)?;
    ctx.line(&format!("left {}", group.name));
    ctx.line(
        &ctx.dim(
            "  a member that did not hear may keep sending; withhold `chat` from it to refuse",
        ),
    );
    Ok(())
}

/// Send one message to every reachable member.
async fn send(
    ctx: &Ctx,
    store: &GroupStore,
    id: &str,
    text: &str,
    path_override: Option<&str>,
) -> CliResult {
    let config = load_config(path_override)?;
    let group = store.get(id).map_err(err)?;
    let (live, stale) = store.reachable(&group.id).map_err(err)?;

    for m in &stale {
        // Named, never silently dropped (A2, condition 3).
        ctx.line(&format!(
            "  {}  {}",
            ctx.dim(&m.0),
            ctx.dim("skipped — not approved, or chat withheld")
        ));
    }
    if live.is_empty() {
        return Err(CliError::Usage(format!(
            "{} has nobody this device may message",
            group.name
        )));
    }

    let sc = crate::commands::SecureCtx::build(&config)?;
    let chat = crate::commands::chat_store(&config, &sc.enc, &sc.ident);

    // **One message, one id, N sends.** The id is minted once so every copy is
    // the same message — that is what lets `group_history` show it once instead
    // of once per member.
    let mut msg =
        peerbeam_chat::ChatMessage::new(text).map_err(|e| CliError::Usage(e.to_string()))?;
    msg.group = Some(group.id.clone());

    let mut sent = 0usize;
    for member in &live {
        // Queued first, so an unreachable member's copy is delivered later by a
        // running host rather than lost — the same path a one-to-one message
        // takes, because that is exactly what this is.
        chat.enqueue(member, &msg)
            .map_err(|e| CliError::Other(e.to_string()))?;
        let flushed = with_member(ctx, &config, member, |session| {
            let chat = chat.clone();
            Box::pin(async move {
                peerbeam_chat::flush_to_session(
                    &session.handle,
                    &chat,
                    &session.peer_id.clone().into(),
                )
                .await
                .map_err(|e| CliError::Connection(e.to_string()))
                .map(|_| ())
            })
        })
        .await;
        match flushed {
            Ok(()) => sent += 1,
            Err(e) => ctx.line(&format!(
                "  {}  {}",
                ctx.dim(&member.0),
                ctx.dim(&e.to_string())
            )),
        }
    }

    ctx.line(&format!("sent to {sent} of {} member(s)", live.len()));
    if sent < live.len() {
        ctx.line(&ctx.dim("  the rest are queued and go out when they are reachable"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use peerbeam_domain::id::DeviceId;
    use std::sync::Arc;

    /// A group store over a real encrypted `AppStore`, with nothing trusted —
    /// `resolve` never asks the trust question, so the answer does not matter
    /// here and a fixture that pretended otherwise would be noise.
    fn store() -> (GroupStore, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let enc: Arc<dyn peerbeam_domain::port::EncryptionProvider> =
            Arc::new(peerbeam_crypto::AeadCrypto::new());
        let key = peerbeam_crypto::derive_subkey(&[3u8; 32], b"peerbeam-appstore-v1");
        let app: Arc<dyn peerbeam_domain::port::AppStore> = Arc::new(
            peerbeam_appstore_fs::FsAppStore::open(dir.path().join("appstore"), key, enc),
        );
        let trust: Arc<dyn peerbeam_domain::port::TrustStore> =
            Arc::new(peerbeam_trust_fs::FsTrust::open(dir.path().join("trust.json")).unwrap());
        (GroupStore::new(app, trust, DeviceId::from("pb-me")), dir)
    }

    /// The ladder: exact id, then exact name, then unique prefix — the same one
    /// `send --to` and `trust approve` climb.
    #[test]
    fn a_group_resolves_by_id_then_name_then_prefix() {
        let (store, _dir) = store();
        let trip = store.create("Work Trip").unwrap();
        store.create("Holiday").unwrap();

        assert_eq!(resolve(&store, &trip.id).unwrap(), trip.id);
        assert_eq!(resolve(&store, "Work Trip").unwrap(), trip.id);
        // Case and spacing are forgiving, exactly as they are for the name rule.
        assert_eq!(resolve(&store, "  work   trip ").unwrap(), trip.id);
        assert_eq!(resolve(&store, "Work").unwrap(), trip.id);
    }

    /// **An ambiguous name is named, never guessed.** Acting on the wrong group
    /// is not a private mistake — the message goes to other people.
    #[test]
    fn an_ambiguous_prefix_lists_the_candidates_instead_of_choosing() {
        let (store, _dir) = store();
        store.create("Work Trip").unwrap();
        store.create("Work Party").unwrap();

        let err = resolve(&store, "Work").expect_err("two matches must not be guessed between");
        let msg = err.to_string();
        assert!(
            msg.contains("Work Trip"),
            "the candidates are not named: {msg}"
        );
        assert!(
            msg.contains("Work Party"),
            "the candidates are not named: {msg}"
        );
    }

    #[test]
    fn a_name_that_matches_nothing_is_not_found() {
        let (store, _dir) = store();
        store.create("Work Trip").unwrap();
        assert!(matches!(
            resolve(&store, "Holiday"),
            Err(CliError::NotFound(_))
        ));
    }

    /// An exact name wins over a prefix that also matches it, so a group called
    /// "Work" is reachable even when "Work Trip" exists.
    #[test]
    fn an_exact_name_beats_a_longer_one_it_prefixes() {
        let (store, _dir) = store();
        let work = store.create("Work").unwrap();
        store.create("Work Trip").unwrap();
        assert_eq!(resolve(&store, "Work").unwrap(), work.id);
    }
}
