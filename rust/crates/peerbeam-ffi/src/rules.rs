//! Auto-save rules for the app surface: **where** an accepted item lands.
//!
//! The rule type, the matcher and the validation all live in
//! [`peerbeam_config::rules`], which is the single implementation both this
//! bridge and the CLI use. This module is only the app's door to it: read the
//! list, write the list, and refuse to write one that cannot work.
//!
//! # What this cannot do
//!
//! Nothing here touches acceptance. A rule has no verdict to give, this module
//! is not reachable from the approval path, and the setting that *does* skip a
//! prompt — `auto_accept` — is a separate, pre-existing one that neither
//! [`get`] nor [`set`] reads or writes (I6).
//!
//! # Where the app keeps them
//!
//! In the app's own settings document, under [`RULES_KEY`], and from there into
//! `EngineConfig.storage.rules` via [`crate::settings::overlay`] — exactly the
//! path `transfer_directory` and `auto_accept` already take. That is why there
//! is no new file, no new path plumbing and no second writer: a rules list the
//! app edited and a rules list the engine reads are the same list by
//! construction.
//!
//! The CLI keeps *its* rules in `config.json`, as it keeps its save directory
//! and its device name. The two surfaces have always had separate settings on
//! the same machine; rules are not the place to change that.
//!
//! # Android
//!
//! A destination is an absolute filesystem path, and Android receives into a
//! SAF-granted location it cannot address that way. So rules are **unsupported**
//! there and this module says so out loud in two places: [`SUPPORTED`] is
//! `false`, which `pb_settings_get` reports as `rules_supported` so the UI can
//! explain rather than show a list that does nothing, and [`set`] refuses
//! outright so a list cannot even be stored. I12: document the limit, don't
//! weaken the design for the platforms that can honour it.

use serde_json::{json, Value};

use peerbeam_config::SaveRule;

use crate::error::Code;

/// The settings-document key the ordered rule list is stored under.
pub const RULES_KEY: &str = "save_rules";

/// Whether this build can honour rules at all.
///
/// Android and iOS write through a platform-granted location rather than an
/// absolute path, so a rule there could never be applied. Deciding it at
/// compile time keeps the answer identical everywhere it is asked — the
/// settings payload, the write path, and the receive path all read this one
/// constant.
pub const SUPPORTED: bool = !cfg!(any(target_os = "android", target_os = "ios"));

type Op = Result<Value, (Code, String)>;

/// The rules held in a settings document, or an empty list.
///
/// Deliberately total: a document written by a newer build, a hand-edited file
/// with a malformed entry, or no document at all all mean "no rules", which is
/// the same receive behaviour that shipped before rules existed. Refusing to
/// start over an unreadable rule would be a far worse failure than ignoring it.
#[must_use]
pub fn from_settings(settings: &Value) -> Vec<SaveRule> {
    if !SUPPORTED {
        return Vec::new();
    }
    settings
        .get(RULES_KEY)
        .and_then(|v| serde_json::from_value::<Vec<SaveRule>>(v.clone()).ok())
        .unwrap_or_default()
}

/// Replace the whole ordered list: `{rules:[…]}` → `{count}`.
///
/// **The whole list, not one rule.** Reordering is the user's only lever over
/// the first-match tie-break, so the operation the UI actually performs is
/// "here is the list as it now stands" — add, remove and reorder are all that
/// one call, and there is no window in which a partial edit is persisted.
///
/// Every rule is validated ([`SaveRule::validate`]) before any of them is
/// stored, and a single bad one refuses the entire write. Storing the good ones
/// and dropping the rest would leave the user's screen and their disk
/// disagreeing about what the rules are.
pub fn set(value: &Value) -> Op {
    if !SUPPORTED {
        return Err((
            Code::Unsupported,
            "auto-save rules need an absolute destination directory, which this platform does \
             not allow an app to write to"
                .into(),
        ));
    }
    let list = value.get("rules").ok_or((
        Code::InvalidArgument,
        "expected {\"rules\": [...]}".to_string(),
    ))?;
    let rules: Vec<SaveRule> = serde_json::from_value(list.clone())
        .map_err(|e| (Code::InvalidArgument, format!("bad rule: {e}")))?;

    for (i, rule) in rules.iter().enumerate() {
        rule.validate()
            .map_err(|e| (Code::InvalidArgument, format!("rule {i}: {e}")))?;
    }

    // Through `settings::set`, so this write is the same write every other
    // setting makes: one persisted document, one `settings_changed` event, and
    // one `apply_live_settings` that reaches the running engine without a
    // restart.
    crate::settings::set(&json!({ RULES_KEY: rules }))?;
    Ok(json!({ "count": rules.len() }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(dir: &str) -> Value {
        json!({ "directory": dir })
    }

    /// A settings document with no rules key is "no rules" — the state every
    /// existing install is in.
    #[test]
    fn a_document_without_rules_reads_as_no_rules() {
        assert!(from_settings(&json!({})).is_empty());
        assert!(from_settings(&json!({ "auto_accept": true })).is_empty());
    }

    /// A malformed list is ignored rather than fatal. An unreadable rule must
    /// not stop this device receiving files.
    #[test]
    fn a_malformed_list_reads_as_no_rules() {
        assert!(from_settings(&json!({ RULES_KEY: "not a list" })).is_empty());
        assert!(from_settings(&json!({ RULES_KEY: [{ "directory": 7 }] })).is_empty());
    }

    /// A well-formed list is read back in order — the order *is* the tie-break.
    #[test]
    fn a_well_formed_list_is_read_back_in_order() {
        let rules = from_settings(&json!({
            RULES_KEY: [
                { "extension": "pdf", "directory": "/srv/papers" },
                rule("/srv/inbox"),
            ]
        }));
        assert_eq!(rules.len(), 2);
        assert_eq!(rules[0].extension.as_deref(), Some("pdf"));
        assert_eq!(rules[0].directory, "/srv/papers");
        assert_eq!(rules[1].directory, "/srv/inbox");
        assert_eq!(rules[1].extension, None);
    }

    /// `set` refuses a payload that is not `{rules: [...]}` rather than
    /// silently storing nothing.
    #[test]
    fn set_refuses_a_payload_without_a_rules_list() {
        let err = set(&json!({})).expect_err("must refuse");
        assert!(err.1.contains("rules"), "{}", err.1);
    }

    /// **One invalid rule refuses the whole write**, and the message names
    /// which one. A partial save would leave the screen and the disk
    /// disagreeing about what the rules are.
    #[test]
    fn one_invalid_rule_refuses_the_whole_list() {
        let err = set(&json!({
            "rules": [rule("/tmp"), rule("relative/path")]
        }))
        .expect_err("must refuse");
        assert_eq!(err.0.as_str(), Code::InvalidArgument.as_str());
        assert!(err.1.contains("rule 1"), "must name which rule: {}", err.1);
        assert!(err.1.contains("absolute"), "and why: {}", err.1);
    }
}
