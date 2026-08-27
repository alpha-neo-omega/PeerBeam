//! PeerBeam CLI library surface (exposed so integration tests can exercise
//! argument parsing and the pure helpers).

pub mod browse;
pub mod chat;
pub mod cli;
pub(crate) mod clipboard;
pub mod commands;
pub mod engine;
pub mod exit;
pub mod groups;
pub mod groups_sync;
pub mod history;
pub mod logs;
pub mod notes;
pub mod output;
pub mod pair;
pub mod pipe;
pub mod presence;
pub mod prompt;
pub mod resolve;
pub mod rules;
pub mod session_transfer;
pub mod spaces;
pub mod transfers;
mod trust;
pub mod wake;
pub mod watch;
