//! PeerBeam CLI library surface (exposed so integration tests can exercise
//! argument parsing and the pure helpers).

pub mod browse;
pub mod chat;
pub mod cli;
pub(crate) mod clipboard;
pub mod commands;
pub mod engine;
pub mod exit;
pub mod history;
pub mod notes;
pub mod output;
pub mod pipe;
pub mod presence;
pub mod prompt;
pub mod resolve;
pub mod rules;
pub mod session_transfer;
pub mod transfers;
pub mod trust;
pub mod watch;
