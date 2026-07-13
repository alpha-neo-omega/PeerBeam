//! Command-line surface (clap derive).

use clap::{Args, Parser, Subcommand};
use clap_complete::Shell;

#[derive(Parser)]
#[command(
    name = "peerbeam",
    version,
    about = "Secure, zero-config file & clipboard sharing",
    propagate_version = true
)]
pub struct Cli {
    #[command(flatten)]
    pub global: GlobalArgs,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Args)]
pub struct GlobalArgs {
    /// Emit machine-readable JSON (NDJSON for streams). Disables colour/prompts.
    #[arg(long, global = true)]
    pub json: bool,

    /// Never use colour (also honoured: NO_COLOR, TERM=dumb, non-TTY).
    #[arg(long, global = true)]
    pub no_color: bool,

    /// Increase verbosity (-v, -vv).
    #[arg(short, long, global = true, action = clap::ArgAction::Count)]
    pub verbose: u8,

    /// Suppress non-essential output.
    #[arg(short, long, global = true)]
    pub quiet: bool,

    /// Assume "yes" to prompts (non-interactive).
    #[arg(short = 'y', long, global = true)]
    pub yes: bool,

    /// Override the config file path.
    #[arg(long, global = true, value_name = "PATH")]
    pub config: Option<String>,
}

#[derive(Subcommand)]
pub enum Command {
    /// Discover nearby devices.
    Discover(DiscoverArgs),
    /// List known devices.
    List(ListArgs),
    /// Send files or folders to a peer.
    Send(SendArgs),
    /// Receive incoming files.
    Receive(ReceiveArgs),
    /// Share or read clipboard content.
    Clipboard(ClipboardArgs),
    /// Show transfer history.
    History(HistoryArgs),
    /// Run the background daemon.
    Daemon(DaemonArgs),
    /// Get or set configuration.
    Config(ConfigArgs),
    /// Diagnose the environment.
    Doctor,
    /// Measure crypto / transfer throughput.
    Benchmark(BenchmarkArgs),
    /// Show overall status.
    Status,
    /// Generate a shell completion script.
    Completions {
        /// Target shell.
        shell: Shell,
    },
}

#[derive(Args)]
pub struct DiscoverArgs {
    /// How long to scan, in seconds.
    #[arg(long, default_value_t = 3)]
    pub timeout: u64,
    /// Keep scanning and stream changes until interrupted.
    #[arg(long)]
    pub watch: bool,
}

#[derive(Args)]
pub struct ListArgs {
    /// Only online devices.
    #[arg(long)]
    pub online: bool,
}

#[derive(Args)]
pub struct SendArgs {
    /// Files or folders to send.
    #[arg(required = true, value_name = "PATH")]
    pub paths: Vec<String>,
    /// Target device (id, name, or name prefix). Omit to pick interactively.
    #[arg(long)]
    pub to: Option<String>,
}

#[derive(Args)]
pub struct ReceiveArgs {
    /// Directory to save into.
    #[arg(long, value_name = "DIR")]
    pub dir: Option<String>,
    /// Exit after one transfer.
    #[arg(long)]
    pub once: bool,
}

#[derive(Args)]
pub struct ClipboardArgs {
    #[command(subcommand)]
    pub action: ClipboardAction,
}

#[derive(Subcommand)]
pub enum ClipboardAction {
    /// Send text (argument, or stdin) to a peer.
    Send {
        #[arg(long)]
        to: Option<String>,
        text: Option<String>,
    },
    /// Print the last received clipboard content.
    Get,
}

#[derive(Args)]
pub struct HistoryArgs {
    /// Limit the number of rows.
    #[arg(long, default_value_t = 20)]
    pub limit: usize,
    /// Clear history.
    #[arg(long)]
    pub clear: bool,
}

#[derive(Args)]
pub struct DaemonArgs {
    #[command(subcommand)]
    pub action: DaemonAction,
}

#[derive(Subcommand)]
pub enum DaemonAction {
    Start {
        #[arg(long)]
        foreground: bool,
    },
    Stop,
    Status,
}

#[derive(Args)]
pub struct ConfigArgs {
    #[command(subcommand)]
    pub action: ConfigAction,
}

#[derive(Subcommand)]
pub enum ConfigAction {
    /// Print the whole config.
    Show,
    /// Print one value (dotted key, e.g. transfer.chunk_size).
    Get { key: String },
    /// Set one value.
    Set { key: String, value: String },
    /// Print the config file path.
    Path,
}

#[derive(Args)]
pub struct BenchmarkArgs {
    #[command(subcommand)]
    pub target: BenchTarget,
}

#[derive(Subcommand)]
pub enum BenchTarget {
    /// AES-256-GCM seal/open throughput.
    Crypto,
    /// SHA-256 throughput (the transfer integrity hash).
    Hash,
    /// End-to-end transfer over an in-process link.
    Loopback {
        /// Payload size in MiB.
        #[arg(long, default_value_t = 128)]
        size: u64,
        /// Chunk size in KiB.
        #[arg(long, default_value_t = 256)]
        chunk: u32,
    },
    /// End-to-end transfer over a real QUIC connection (loopback).
    Quic {
        /// Payload size in MiB.
        #[arg(long, default_value_t = 128)]
        size: u64,
        /// Chunk size in KiB.
        #[arg(long, default_value_t = 1024)]
        chunk: u32,
    },
}
