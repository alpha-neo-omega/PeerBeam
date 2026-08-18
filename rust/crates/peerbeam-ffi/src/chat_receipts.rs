//! The read-receipt opt-in.
//!
//! Split into its own module for the same reason presence's lives in
//! `presence`: the setting is a privacy decision, and there should be exactly
//! one place that decides what it means.

use serde_json::Value;

/// The settings key for the single opt-in toggle.
///
/// *"Tell people when you have read their messages"*, **default off**. With it
/// false this device sends no receipts at all, to anyone — while still applying
/// receipts its peers send, so opting out never costs you what others tell you.
/// That asymmetry is deliberate and matches presence: the setting governs what
/// you disclose, not what you will accept.
pub const SHARE_KEY: &str = "share_read_receipts";

/// Whether the opt-in is currently on. **Defaults to off** when the key is
/// absent, unreadable, or not a boolean — a settings file this build cannot
/// parse must not be read as consent.
#[must_use]
pub fn sending_enabled() -> bool {
    crate::settings::get().ok().is_some_and(|s| enabled_in(&s))
}

/// The decision itself, as a pure function of a settings document.
///
/// Split out so it can be pinned by a test: read through the global settings
/// the fallback is unreachable in-process, because `defaults()` writes the key
/// explicitly — which means a test that goes through [`sending_enabled`] cannot
/// tell `unwrap_or(false)` from `unwrap_or(true)`. The case that matters is a
/// settings document from an older build (no key) or one this build cannot make
/// sense of (wrong type), and only a direct call can exercise it.
///
/// **Only an explicit `true` is consent.** Absent, null, a string, a number —
/// every one of them is "no".
#[must_use]
pub fn enabled_in(settings: &Value) -> bool {
    settings.get(SHARE_KEY).and_then(Value::as_bool) == Some(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The default is the feature's entire privacy story, so it is asserted
    /// directly rather than inferred from behaviour elsewhere. A settings
    /// document that predates receipts, one this build cannot parse, and one
    /// that stores the key as something other than a bool must all read as
    /// "no" — none of them is consent.
    #[test]
    fn anything_other_than_an_explicit_true_is_not_consent() {
        for doc in [
            serde_json::json!({}),
            serde_json::json!({ "share_read_receipts": false }),
            serde_json::json!({ "share_read_receipts": "yes" }),
            serde_json::json!({ "share_read_receipts": 1 }),
            serde_json::json!({ "share_read_receipts": null }),
        ] {
            assert!(!enabled_in(&doc), "read as consent: {doc}");
        }
        assert!(
            enabled_in(&serde_json::json!({ "share_read_receipts": true })),
            "an explicit true must still be consent"
        );
    }
}
