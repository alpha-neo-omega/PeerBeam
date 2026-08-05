//! Additive PeerSession diagnostics surface (M8).
//!
//! Thin, read-only wrappers over the engine's [`SessionDiagnostics`] (the single
//! source of truth). No state lives here — every call reads the shared registry /
//! state the running sessions already populate.
//!
//! [`SessionDiagnostics`]: peerbeam_engine::SessionDiagnostics

use serde_json::Value;

use crate::error::Code;
use crate::runtime;

fn id_field(v: &Value) -> Result<String, (Code, String)> {
    v.get("id")
        .and_then(|i| i.as_str())
        .map(|s| s.to_string())
        .ok_or((Code::InvalidArgument, "id required".into()))
}

/// `{ sessions:[...], count }` — every active session.
pub fn sessions() -> Result<Value, (Code, String)> {
    Ok(runtime::diagnostics()?.sessions_json())
}

/// `{ session: {...} | null }` for `{id}`.
pub fn session_get(v: &Value) -> Result<Value, (Code, String)> {
    let id = id_field(v)?;
    Ok(runtime::diagnostics()?.session_json(&id))
}

/// Live channel snapshot for `{id}` (or all tracked sessions if `id` is absent).
pub fn channels(v: &Value) -> Result<Value, (Code, String)> {
    let diag = runtime::diagnostics()?;
    match v.get("id").and_then(|i| i.as_str()) {
        Some(id) => {
            let id = id.to_string();
            Ok(runtime::block_on(
                async move { diag.channels_json(&id).await },
            ))
        }
        None => Ok(runtime::block_on(
            async move { diag.all_channels_json().await },
        )),
    }
}

/// `{ transport, active_sessions, recovering }` — the live transport summary.
pub fn migration() -> Result<Value, (Code, String)> {
    Ok(runtime::diagnostics()?.migration_json())
}

/// `{ recovering, sessions_recovering:[...] }`.
pub fn recovery() -> Result<Value, (Code, String)> {
    Ok(runtime::diagnostics()?.recovery_json())
}

/// Aggregate `{ sessions, transport, recovery }` diagnostics.
pub fn diagnostics() -> Result<Value, (Code, String)> {
    Ok(runtime::diagnostics()?.diagnostics_json())
}
