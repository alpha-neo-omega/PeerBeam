//! Strongly-typed identifiers.
//!
//! Newtypes prevent mixing a device id with a transfer id at compile time,
//! which stringly-typed ids (the v1 mistake) allowed.

use serde::{Deserialize, Serialize};
use std::fmt;

/// Uniquely identifies a device/peer.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct DeviceId(pub String);

/// Uniquely identifies a transfer session.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TransferId(pub String);

/// Uniquely identifies a registered provider (plugin) instance.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ProviderId(pub String);

macro_rules! impl_id {
    ($t:ty) => {
        impl $t {
            /// Borrow the inner string.
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }
        impl fmt::Display for $t {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }
        impl From<String> for $t {
            fn from(s: String) -> Self {
                Self(s)
            }
        }
        impl From<&str> for $t {
            fn from(s: &str) -> Self {
                Self(s.to_string())
            }
        }
    };
}

impl_id!(DeviceId);
impl_id!(TransferId);
impl_id!(ProviderId);
