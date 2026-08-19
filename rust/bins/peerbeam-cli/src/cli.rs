//! Command-line surface (clap derive).

use clap::{ArgGroup, Args, Parser, Subcommand};
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
    /// Chat with a peer.
    Chat(ChatArgs),
    /// Pipe stdin to a peer, or a peer's stream to stdout.
    Pipe(PipeArgs),
    /// Show transfer history.
    History(HistoryArgs),
    /// List, approve or revoke the devices this machine trusts.
    Trust(TrustArgs),
    /// List, add or remove the rules that choose where received files land.
    Rules(RulesArgs),
    /// Write and read notes kept on this device.
    Notes(NotesArgs),
    /// Make one of your devices findable — *find my device*.
    Ring(RingArgs),
    /// One chronological view of this device's activity.
    Timeline(TimelineArgs),
    /// Watch a folder and send whatever lands in it.
    Watch(WatchArgs),
    /// List what a device shares, read-only.
    Browse(BrowseArgs),
    /// Mirror a device's shared folder into a local directory.
    Sync(SyncArgs),
    /// Send piped text — a log, a command's output, a snippet — as a message.
    Snippet(SnippetArgs),
    /// PIN-pair with a device, proving first contact reached the right one.
    Pair(PairArgs),
    /// Show or export this device's recent log lines.
    Logs(LogsArgs),
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
    /// Inspect PeerSessions (list / show / watch / stats).
    Session(SessionArgs),
    /// Inspect PeerSession channels.
    Channels(ChannelsArgs),
    /// Show active transfers, and the ones that were interrupted.
    Transfers(TransfersArgs),
    /// Show the transport summary (PeerSession runtime).
    Migration,
    /// Show reconnect / resume (recovery) state.
    Recovery,
    /// Aggregate PeerSession diagnostics (sessions + transport + recovery).
    Diagnostics,
    /// Generate a shell completion script.
    Completions {
        /// Target shell.
        shell: Shell,
    },
}

/// `peerbeam transfers` — what is moving, and what stopped moving.
///
/// With no subcommand it prints the live-session/transport snapshot it always
/// has, plus an `interrupted` array. The subcommands act on that array: a
/// transfer whose checkpoint outlived it can be resumed (outgoing only — the
/// protocol is sender-driven) or discarded along with its partial file.
#[derive(Args)]
pub struct TransfersArgs {
    #[command(subcommand)]
    pub action: Option<TransfersAction>,
}

#[derive(Subcommand)]
pub enum TransfersAction {
    /// List the transfers that were interrupted, newest first.
    List,
    /// Resume an interrupted outgoing transfer from where it stopped.
    Resume {
        /// Transfer id, as shown by `transfers list`.
        #[arg(value_name = "ID")]
        id: String,
    },
    /// Forget an interrupted transfer and the partial file it was holding.
    Discard {
        /// Transfer id, as shown by `transfers list`.
        #[arg(value_name = "ID")]
        id: String,
    },
}

#[derive(Args)]
pub struct SessionArgs {
    #[command(subcommand)]
    pub action: SessionAction,
}

#[derive(Subcommand)]
pub enum SessionAction {
    /// List active sessions.
    List,
    /// Show one session by id.
    Show {
        /// Session id (hex, as shown by `session list`).
        id: String,
    },
    /// Stream session lifecycle changes (snapshot when no daemon is attached).
    Watch,
    /// Session + transport summary.
    Stats,
}

#[derive(Args)]
pub struct ChannelsArgs {
    /// Session id to inspect; omit for all tracked sessions.
    #[arg(long)]
    pub session: Option<String>,
    /// Stream channel changes (snapshot when no daemon is attached).
    #[arg(long)]
    pub watch: bool,
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
    /// Files to send.
    #[arg(required = true, value_name = "PATH")]
    pub paths: Vec<String>,
    /// Target device (id, name, or name prefix). Omit to pick interactively.
    #[arg(long)]
    pub to: Option<String>,
    /// Wait until this local time before sending, e.g. `2026-08-20T09:00:00`
    /// or `09:00` for the next occurrence today or tomorrow.
    ///
    /// The process stays running until then — this is a delay, not a system
    /// scheduler, and it is named as one. For anything that must survive a
    /// reboot, use cron or a systemd timer and call `peerbeam send` from it.
    #[arg(long, value_name = "WHEN")]
    pub at: Option<String>,
    /// Dial a peer directly at `IP:PORT`, skipping discovery (headless/testing).
    #[arg(long, value_name = "IP:PORT", conflicts_with = "to")]
    pub addr: Option<String>,
}

#[derive(Args)]
pub struct ReceiveArgs {
    /// Directory to save into.
    #[arg(long, value_name = "DIR")]
    pub dir: Option<String>,
    /// Exit after one transfer.
    #[arg(long)]
    pub once: bool,
    /// Port to listen on (overrides `transfer.port` from config).
    #[arg(long)]
    pub port: Option<u16>,
}

#[derive(Args)]
pub struct ClipboardArgs {
    #[command(subcommand)]
    pub action: ClipboardAction,
}

#[derive(Subcommand)]
pub enum ClipboardAction {
    /// Send text (argument, stdin, or the system clipboard) to a peer.
    Send {
        /// Target device (id, name, or name prefix).
        #[arg(long)]
        to: Option<String>,
        /// Dial a peer directly at `IP:PORT`, skipping discovery.
        #[arg(long, value_name = "IP:PORT", conflicts_with = "to")]
        addr: Option<String>,
        /// Text to send. Omit to read stdin (if piped) or the system clipboard.
        text: Option<String>,
    },
    /// Print the last received clipboard content.
    Get,
    /// Show — or erase — this device's clipboard history.
    ///
    /// Empty unless `device.clipboard_history` is on. History is kept only on
    /// this device and is never sent to a peer; only the current clip syncs.
    History {
        /// Erase every remembered clip. Works whether or not history is
        /// currently on: turning the setting off stops new entries but does
        /// not remove what was already recorded.
        #[arg(long)]
        clear: bool,
    },
}

#[derive(Args)]
pub struct ChatArgs {
    #[command(subcommand)]
    pub action: ChatAction,
}

#[derive(Subcommand)]
pub enum ChatAction {
    /// Send a message — or a file attachment — to a peer.
    Send {
        /// Target device (id, name, or name prefix).
        #[arg(long)]
        to: Option<String>,
        /// Dial a peer directly at `IP:PORT`, skipping discovery.
        #[arg(long, value_name = "IP:PORT", conflicts_with = "to")]
        addr: Option<String>,
        /// Message text. Required unless `--file` is given.
        #[arg(required_unless_present = "file")]
        text: Option<String>,
        /// Share a file in the conversation instead of text. The bytes are
        /// copied into the outbox first (subject to
        /// `device.max_queued_file_bytes` and `device.min_free_bytes`) and an
        /// unreachable peer is queued, not an error.
        #[arg(long, value_name = "PATH", conflicts_with = "text")]
        file: Option<String>,
    },
    /// Call off a file we are sharing: drop it from the queue and delete the
    /// copy the outbox made of it.
    ///
    /// One share, named by its message id — **not** the conversation it sits
    /// in. To forget the whole thread, see `chat delete`.
    Cancel {
        /// Peer device id (`pb-…`), or a name that is discoverable right now.
        peer: String,
        /// The share's message id, as shown by `chat history --json`.
        id: String,
    },
    /// Forget a whole conversation on this device: every message in it.
    ///
    /// **Not `chat cancel`.** That calls off one file we are still sharing and
    /// leaves the conversation alone; this erases the conversation and leaves
    /// the queue alone. The two words are not interchangeable in either
    /// direction.
    ///
    /// **Local only.** Nothing goes on the wire and the peer keeps its own
    /// copy — this is "forget these messages here", never "unsend".
    ///
    /// A row still backing a **queued** outbound message survives, along with
    /// the file it owns: the drain reads a missing record as "nothing will ever
    /// settle this" and would throw those bytes away. The receipt says how many
    /// were kept for that reason.
    ///
    /// Irreversible and unprompted-for by anything else on the device, so it
    /// asks first; `--yes` answers in advance for a script.
    Delete {
        /// Peer device id (`pb-…`), or a name that is discoverable right now.
        peer: String,
    },
    /// React to a message with an emoji — or withdraw a reaction.
    ///
    /// Applies to this device's own history whether or not the peer can be
    /// reached, and reports separately whether it was delivered: an older peer
    /// that never negotiated reactions, or one that is simply offline, leaves
    /// `delivered` false. Reactions are not queued for later delivery.
    React {
        /// Peer device id (`pb-…`), or a name that is discoverable right now.
        peer: String,
        /// The message id to react to, as shown by `chat history --json`.
        id: String,
        /// The reaction itself, e.g. an emoji.
        emoji: String,
        /// Withdraw this reaction instead of adding it.
        #[arg(long)]
        remove: bool,
    },
    /// Print a conversation's history.
    History {
        /// Peer device id.
        peer: String,
        /// Also tell the peer you have read it, up to the newest message shown.
        ///
        /// Never implied by printing: reading a conversation is not consent to
        /// report having read it. Sends nothing unless
        /// `device.share_read_receipts` is on.
        #[arg(long)]
        mark_read: bool,
    },
    /// Search this device's stored conversations.
    ///
    /// A local read of history already on disk: no peer is contacted, nothing
    /// goes on the wire, and a thread whose device is long gone is searchable
    /// exactly like one that is online. Matches a case-insensitive substring of
    /// a message's text or a shared file's name — never a file's path on this
    /// machine, which is where it sits on disk rather than anything anyone
    /// said.
    Search {
        /// What to look for. Matched literally, not as a regular expression.
        #[arg(value_name = "QUERY")]
        query: String,
        /// How many matches to print at most. The newest are kept, and a note
        /// says so when there were more.
        #[arg(
            long,
            default_value_t = peerbeam_chat::DEFAULT_SEARCH_LIMIT as u64,
            value_name = "N",
            value_parser = clap::value_parser!(u64).range(1..=peerbeam_chat::MAX_SEARCH_LIMIT as u64),
        )]
        limit: u64,
    },
    /// Listen for and print incoming chat messages.
    Watch {
        /// Port to listen on (overrides `transfer.port` from config).
        #[arg(long)]
        port: Option<u16>,
    },
}

/// `peerbeam pipe` — an encrypted byte stream between two devices.
///
/// ```text
/// $ tar cz ./project | peerbeam pipe --to laptop
/// $ peerbeam pipe --listen > project.tgz
/// ```
///
/// Exactly one of `--to`, `--addr` or `--listen` is required: without a
/// direction there is nothing to do, and a default would have to guess which of
/// stdin and stdout the user meant to be the payload.
///
/// **`stdout` carries piped bytes and nothing else.** Every human-facing line
/// this command emits — the listening address, the peer's name, progress,
/// errors, and `--json` events — goes to **stderr**, in both directions, or
/// `peerbeam pipe --listen > project.tgz` would write status text into the
/// archive.
#[derive(Args)]
#[command(group(
    ArgGroup::new("direction")
        .args(["to", "addr", "listen"])
        .required(true)
))]
pub struct PipeArgs {
    /// Send stdin to this device (id, name, or name prefix).
    #[arg(long, conflicts_with_all = ["listen", "from", "port"])]
    pub to: Option<String>,
    /// Send stdin to a peer dialled directly at `IP:PORT`, skipping discovery.
    #[arg(long, value_name = "IP:PORT", conflicts_with_all = ["to", "listen", "from", "port"])]
    pub addr: Option<String>,
    /// Accept **one** incoming stream, write it to stdout, and exit.
    #[arg(long)]
    pub listen: bool,
    /// With `--listen`: accept only this device (`pb-…` id, or a discoverable
    /// name). Any other peer is refused, trusted or not.
    #[arg(long, requires = "listen", value_name = "DEVICE")]
    pub from: Option<String>,
    /// With `--listen`: port to listen on (overrides `transfer.port`).
    #[arg(long, requires = "listen")]
    pub port: Option<u16>,
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

/// `peerbeam trust` — the devices this machine has pinned, and the ones the
/// user actually chose.
///
/// Two states, and the difference is the whole command. A device is **pinned**
/// by the authenticated handshake the first time it connects: that records its
/// key so a later change is detectable, and nothing more — every stranger that
/// has ever reached this machine is pinned. A device is **approved** only when
/// a person says so, here or in the app, and that is what lets it receive this
/// machine's presence status, its clipboard, and a `pipe --listen`.
///
/// Without this command those three features are unusable on a CLI-only or
/// headless box, because approval was reachable from the GUI alone.
#[derive(Args)]
pub struct TrustArgs {
    #[command(subcommand)]
    pub action: TrustAction,
}

#[derive(Subcommand)]
pub enum TrustAction {
    /// List every pinned device, and whether it is approved or only pinned.
    List,
    /// Approve a device and grant it every permission this build has: files,
    /// chat, clipboard, presence, pipe. Narrow it afterwards with
    /// `trust revoke-permission`. Prints the fingerprint and asks, unless `--yes`.
    Approve {
        /// Device id, name, or unambiguous name prefix (as shown by `trust list`).
        #[arg(value_name = "DEVICE")]
        device: String,
    },
    /// Forget a device entirely: its pin, its approval and its permissions. The
    /// next connection from it is a fresh first contact.
    Revoke {
        /// Device id, name, or unambiguous name prefix (as shown by `trust list`).
        #[arg(value_name = "DEVICE")]
        device: String,
    },
    /// Grant one or more permissions to a device: what it may actually do.
    Permit {
        /// Device id, name, or unambiguous name prefix (as shown by `trust list`).
        #[arg(value_name = "DEVICE")]
        device: String,
        /// One or more of: files, chat, clipboard, presence, pipe.
        #[arg(value_name = "PERMISSION", required = true)]
        permissions: Vec<String>,
    },
    /// Withhold one or more permissions from a device, keeping it approved.
    /// Takes effect on its next operation, not its next connection.
    RevokePermission {
        /// Device id, name, or unambiguous name prefix (as shown by `trust list`).
        #[arg(value_name = "DEVICE")]
        device: String,
        /// One or more of: files, chat, clipboard, presence, pipe.
        #[arg(value_name = "PERMISSION", required = true)]
        permissions: Vec<String>,
    },
}

/// `peerbeam rules` — where received files land.
///
/// A rule is a match plus a destination, and it decides **where** an accepted
/// file is written, never **whether** it is accepted. Nothing here touches the
/// approval prompt or `device.auto_accept_trusted`; rules are read after a
/// transfer has been accepted and is on its way to disk.
/// Mirror a device's shared folder into a local directory.
///
/// **A one-way pull.** Nothing local is deleted, nothing is pushed back, and a
/// local file newer than the peer's copy is left alone — overwriting work done
/// here would lose it with no warning and no undo. Removing files is a decision
/// you make with your own tools.
///
/// Requires the peer to have granted this machine both `browse` (to see what
/// exists) and `files` (to receive any of it).
/// Show or export recent log lines.
///
/// Reads the engine's **log file**, not a buffer in this process. A one-shot
/// command has its own empty ring, so answering from memory would print nothing
/// while looking like it worked — the file is what makes the logs of a running
/// engine, or of one that has since crashed, readable at all.
///
/// Requires `log.to_file` (on by default). With it off there is nothing on disk
/// to read, and this says so rather than printing an empty list.
#[derive(Args)]
pub struct LogsArgs {
    /// How many of the most recent lines to show.
    #[arg(long, default_value_t = 100)]
    pub limit: u64,
    /// Copy them to PATH and print where they went.
    #[arg(long, value_name = "PATH")]
    pub export: Option<String>,
}

/// PIN-pair with a device.
///
/// Trust-on-first-use pins whatever key answers first; if someone is on the
/// path during that first handshake, they are pinned instead of the real
/// device. PIN pairing closes that window: the other device shows six digits, a
/// person reads them across, and this proves knowledge of them over *this*
/// handshake's transcript — so a proof is worthless to anyone relaying between
/// two connections.
///
/// The PIN is never sent. Only a proof derived from it is.
#[derive(Args)]
pub struct PairArgs {
    /// Target device (id, name, or unambiguous name prefix).
    pub peer: String,
    /// The six digits shown on the other device. Omit to be prompted.
    #[arg(long)]
    pub pin: Option<String>,
    /// Show a PIN and wait for the other device to prove it, rather than
    /// entering one.
    #[arg(long, conflicts_with = "pin")]
    pub show: bool,
}

/// Send whatever is on stdin to a device as a chat message.
///
/// `cargo test 2>&1 | peerbeam snippet --to laptop`. It is `chat send` with the
/// text coming from a pipe, which is the shape terminal output actually has —
/// and it goes to the conversation rather than becoming a file, because a
/// snippet is something you read, not something you save.
#[derive(Args)]
pub struct SnippetArgs {
    /// Target device (id, name, or unambiguous name prefix).
    #[arg(long)]
    pub to: String,
    /// A line to put above the snippet, e.g. what produced it.
    #[arg(long)]
    pub title: Option<String>,
}

#[derive(Args, Clone)]
pub struct SyncArgs {
    /// Keep syncing, re-checking every INTERVAL seconds.
    ///
    /// A file is only acted on once it has stopped changing, so saving a large
    /// file mid-poll does not sync a half-written copy.
    #[arg(long, value_name = "SECONDS")]
    pub watch: Option<u64>,
    /// Target device (id, name, or unambiguous name prefix).
    pub peer: String,
    /// Share-relative path on the device, e.g. `photos`.
    pub path: String,
    /// Local directory to mirror into. Must already exist.
    pub into: String,
}

/// List what a device shares.
///
/// Read-only, and shows only what that device chose to share **and** granted
/// this machine permission to see. An empty listing is the same answer whether
/// the device shares nothing, has not granted the permission, or the path does
/// not exist — it deliberately does not say which.
#[derive(Args)]
pub struct BrowseArgs {
    /// Target device (id, name, or unambiguous name prefix).
    pub peer: String,
    /// A share-relative path, e.g. `photos/2026`. Omit to list the shares.
    pub path: Option<String>,
}

/// Watch a folder and send each new file to a device.
///
/// Polls rather than subscribing to filesystem events: a poll needs no
/// platform-specific watcher, behaves the same on every OS PeerBeam supports,
/// and works over the network filesystems people actually drop files onto.
///
/// A file is only sent once it has **stopped growing**, so a large copy still
/// in progress is not sent half-written. Files already in the folder when the
/// watch starts are left alone: they are not "new", and sending someone's whole
/// Downloads directory because they pointed a watch at it would be the wrong
/// surprise.
#[derive(Args)]
pub struct WatchArgs {
    /// Directory to watch. Only its top level; subdirectories are ignored.
    pub directory: String,
    /// Target device (id, name, or unambiguous name prefix).
    #[arg(long)]
    pub to: String,
    /// Seconds between scans.
    #[arg(long, default_value_t = 3)]
    pub interval: u64,
    /// Send what is already in the folder when the watch starts, too.
    #[arg(long)]
    pub existing: bool,
}

/// This device's activity, newest first.
///
/// A read across records this device already keeps — transfers, conversations,
/// and clipboard history when it is on. Nothing is recorded to build it, and
/// nothing here carries message bodies or clip text: the timeline says *that*
/// something happened and when.
#[derive(Args)]
pub struct TimelineArgs {
    /// How many entries to show (1..=1000).
    #[arg(long, default_value_t = 50)]
    pub limit: usize,
}

/// Ask a device to make itself findable.
///
/// The device decides *how* — a sound, a notification, a flashing window —
/// because a sender cannot see hardware it is not holding. It rings only if it
/// has granted this machine the `presence` permission, and this side is never
/// told whether it did: reporting that back would let anyone map which devices
/// are listening.
#[derive(Args)]
pub struct RingArgs {
    /// Target device (id, name, or unambiguous name prefix).
    pub peer: String,
    /// How long to keep signalling, in seconds (max 60).
    #[arg(long, default_value_t = 15)]
    pub seconds: u16,
}

/// Notes kept on this device.
///
/// Text with a title and a last-edited time, nothing more. Deleting leaves a
/// tombstone rather than removing the row, so a deletion can reach the devices
/// you have granted the `notes` permission once sync lands — a row that simply
/// vanished would be indistinguishable from one they had not seen yet.
#[derive(Args)]
pub struct NotesArgs {
    #[command(subcommand)]
    pub action: NotesAction,
}

#[derive(Subcommand)]
pub enum NotesAction {
    /// List every note, newest edit first. Deleted notes are not shown.
    List,
    /// Write a new note. The body is read from the argument, or from stdin
    /// when it is omitted — so `pbpaste | peerbeam notes add` works.
    Add {
        /// The note's text. Omit to read it from stdin.
        body: Option<String>,
        /// An optional heading.
        #[arg(long)]
        title: Option<String>,
    },
    /// Replace a note's text. Refuses a deleted note rather than resurrecting
    /// one.
    Edit {
        /// The note's id, as shown by `notes list --json`.
        id: String,
        /// The replacement text. Omit to read it from stdin.
        body: Option<String>,
        /// A replacement heading. Omitted clears it.
        #[arg(long)]
        title: Option<String>,
    },
    /// Exchange notes with a device you have granted the `notes` permission.
    ///
    /// Sends this device's whole set — tombstones included, so deletions travel
    /// — and merges what the peer answers with. Two passes, then done.
    Sync {
        /// Target device (id, name, or unambiguous name prefix).
        peer: String,
    },
    /// Delete a note, leaving a tombstone so the deletion can propagate.
    Remove {
        /// The note's id, as shown by `notes list --json`.
        id: String,
    },
}

#[derive(Args)]
pub struct RulesArgs {
    #[command(subcommand)]
    pub action: RulesAction,
}

#[derive(Subcommand)]
pub enum RulesAction {
    /// List the rules in order. The first one that matches a file wins.
    List,
    /// Add a rule. Omitted criteria match everything; with none at all the
    /// rule is a catch-all.
    Add {
        /// Absolute destination directory. Its parent must already exist.
        #[arg(value_name = "DIRECTORY")]
        directory: String,
        /// Only files from this device (id, name, or unambiguous name prefix,
        /// as shown by `trust list`). Stored as the device's authenticated id.
        #[arg(long, value_name = "DEVICE")]
        from: Option<String>,
        /// Only files with this extension (with or without the leading dot).
        #[arg(long, value_name = "EXT")]
        ext: Option<String>,
        /// Only files of at least this many bytes (inclusive).
        #[arg(long, value_name = "BYTES")]
        min_bytes: Option<u64>,
        /// Only files of at most this many bytes (inclusive).
        #[arg(long, value_name = "BYTES")]
        max_bytes: Option<u64>,
        /// Insert at this position instead of appending. Order is the
        /// tie-break between two rules that both match.
        #[arg(long, value_name = "INDEX")]
        at: Option<usize>,
    },
    /// Remove the rule at this position (as shown by `rules list`).
    Remove {
        /// Rule position, from `rules list`.
        #[arg(value_name = "INDEX")]
        index: usize,
    },
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
}
