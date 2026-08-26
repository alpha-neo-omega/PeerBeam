//! Argument-parsing tests for the CLI surface.

use clap::{CommandFactory, Parser};
use peerbeam_cli::cli::{
    BenchTarget, ChatAction, Cli, Command, ConfigAction, RulesAction, TrustAction,
};

#[test]
fn command_definition_is_valid() {
    // clap's own consistency check (dup args, bad names, …).
    Cli::command().debug_assert();
}

#[test]
fn parses_discover_with_timeout() {
    let cli = Cli::try_parse_from(["peerbeam", "discover", "--timeout", "5"]).unwrap();
    match cli.command {
        Command::Discover(a) => assert_eq!(a.timeout, 5),
        _ => panic!("expected discover"),
    }
}

#[test]
fn global_flags_work_after_subcommand() {
    let cli = Cli::try_parse_from(["peerbeam", "list", "--json"]).unwrap();
    assert!(cli.global.json);
    assert!(matches!(cli.command, Command::List(_)));
}

#[test]
fn send_requires_at_least_one_path() {
    assert!(Cli::try_parse_from(["peerbeam", "send"]).is_err());
    let cli = Cli::try_parse_from(["peerbeam", "send", "a.txt", "b.txt", "--to", "phone"]).unwrap();
    match cli.command {
        Command::Send(a) => {
            assert_eq!(a.paths.len(), 2);
            assert_eq!(a.to.as_deref(), Some("phone"));
        }
        _ => panic!("expected send"),
    }
}

#[test]
fn send_addr_conflicts_with_to() {
    // --addr and --to are mutually exclusive.
    assert!(Cli::try_parse_from([
        "peerbeam",
        "send",
        "a.txt",
        "--to",
        "phone",
        "--addr",
        "1.2.3.4:9"
    ])
    .is_err());
    let cli =
        Cli::try_parse_from(["peerbeam", "send", "a.txt", "--addr", "1.2.3.4:49600"]).unwrap();
    match cli.command {
        Command::Send(a) => {
            assert_eq!(a.addr.as_deref(), Some("1.2.3.4:49600"));
            assert!(a.to.is_none());
        }
        _ => panic!("expected send"),
    }
}

#[test]
fn receive_accepts_port_and_once() {
    let cli = Cli::try_parse_from(["peerbeam", "receive", "--once", "--port", "50000"]).unwrap();
    match cli.command {
        Command::Receive(a) => {
            assert!(a.once);
            assert_eq!(a.port, Some(50000));
        }
        _ => panic!("expected receive"),
    }
}

#[test]
fn config_subcommands() {
    let cli = Cli::try_parse_from(["peerbeam", "config", "set", "device.name", "Laptop"]).unwrap();
    match cli.command {
        Command::Config(a) => match a.action {
            ConfigAction::Set { key, value } => {
                assert_eq!(key, "device.name");
                assert_eq!(value, "Laptop");
            }
            _ => panic!("expected set"),
        },
        _ => panic!("expected config"),
    }
}

#[test]
fn benchmark_loopback_size() {
    let cli = Cli::try_parse_from([
        "peerbeam",
        "benchmark",
        "loopback",
        "--size",
        "64",
        "--chunk",
        "512",
    ])
    .unwrap();
    match cli.command {
        Command::Benchmark(a) => match a.target {
            BenchTarget::Loopback { size, chunk } => {
                assert_eq!(size, 64);
                assert_eq!(chunk, 512);
            }
            _ => panic!("expected loopback"),
        },
        _ => panic!("expected benchmark"),
    }
}

#[test]
fn chat_send_addr_conflicts_with_to() {
    assert!(Cli::try_parse_from([
        "peerbeam",
        "chat",
        "send",
        "--to",
        "phone",
        "--addr",
        "1.2.3.4:9",
        "hi",
    ])
    .is_err());
    let cli =
        Cli::try_parse_from(["peerbeam", "chat", "send", "--addr", "1.2.3.4:49600", "hi"]).unwrap();
    match cli.command {
        Command::Chat(a) => match a.action {
            ChatAction::Send {
                to,
                addr,
                text,
                file,
                reply_to,
            } => {
                assert!(to.is_none());
                assert_eq!(addr.as_deref(), Some("1.2.3.4:49600"));
                assert_eq!(text.as_deref(), Some("hi"));
                assert!(file.is_none());
                assert!(reply_to.is_none(), "an ordinary send answers nothing");
            }
            _ => panic!("expected send"),
        },
        _ => panic!("expected chat"),
    }
}

#[test]
fn chat_send_requires_either_text_or_file() {
    // Neither `text` nor `--file` given: clap must reject it.
    assert!(Cli::try_parse_from(["peerbeam", "chat", "send", "--to", "phone"]).is_err());
}

#[test]
fn chat_send_text_and_file_are_mutually_exclusive() {
    assert!(Cli::try_parse_from([
        "peerbeam",
        "chat",
        "send",
        "--to",
        "phone",
        "--file",
        "/tmp/a.bin",
        "hi",
    ])
    .is_err());
}

#[test]
fn chat_send_file_parses_with_text_omitted() {
    let cli = Cli::try_parse_from([
        "peerbeam",
        "chat",
        "send",
        "--to",
        "bob",
        "--file",
        "/tmp/a.bin",
    ])
    .unwrap();
    match cli.command {
        Command::Chat(a) => match a.action {
            ChatAction::Send {
                to,
                addr,
                text,
                file,
                reply_to,
            } => {
                assert_eq!(to.as_deref(), Some("bob"));
                assert!(addr.is_none());
                assert!(text.is_none());
                assert_eq!(file.as_deref(), Some("/tmp/a.bin"));
                assert!(reply_to.is_none());
            }
            _ => panic!("expected send"),
        },
        _ => panic!("expected chat"),
    }
}

#[test]
fn chat_cancel_parses_peer_and_id() {
    let cli =
        Cli::try_parse_from(["peerbeam", "chat", "cancel", "pb-abc123", "0000000000001"]).unwrap();
    match cli.command {
        Command::Chat(a) => match a.action {
            ChatAction::Cancel { peer, id } => {
                assert_eq!(peer, "pb-abc123");
                assert_eq!(id, "0000000000001");
            }
            _ => panic!("expected cancel"),
        },
        _ => panic!("expected chat"),
    }
    // Both positionals are required — a cancel that names no file must not
    // parse into something that could delete the wrong one.
    assert!(Cli::try_parse_from(["peerbeam", "chat", "cancel", "pb-abc123"]).is_err());
    assert!(Cli::try_parse_from(["peerbeam", "chat", "cancel"]).is_err());
}

#[test]
fn chat_history_and_watch_parse() {
    let cli = Cli::try_parse_from(["peerbeam", "chat", "history", "pb-abc123"]).unwrap();
    match cli.command {
        Command::Chat(a) => match a.action {
            ChatAction::History { peer, mark_read } => {
                assert_eq!(peer, "pb-abc123");
                // Printing a conversation is not consent to report having read
                // it, so the flag must default off.
                assert!(!mark_read, "history marked read without being asked");
            }
            _ => panic!("expected history"),
        },
        _ => panic!("expected chat"),
    }

    let cli = Cli::try_parse_from(["peerbeam", "chat", "watch", "--port", "50100"]).unwrap();
    match cli.command {
        Command::Chat(a) => match a.action {
            ChatAction::Watch { port } => assert_eq!(port, Some(50100)),
            _ => panic!("expected watch"),
        },
        _ => panic!("expected chat"),
    }
}

#[test]
fn completions_accepts_a_shell() {
    assert!(Cli::try_parse_from(["peerbeam", "completions", "bash"]).is_ok());
    assert!(Cli::try_parse_from(["peerbeam", "completions", "notashell"]).is_err());
}

// ── pipe ────────────────────────────────────────────────────────────────────

/// A direction is required: without one there is nothing to do, and a default
/// would have to guess which of stdin and stdout is the payload.
#[test]
fn pipe_requires_exactly_one_direction() {
    assert!(Cli::try_parse_from(["peerbeam", "pipe"]).is_err());
    assert!(Cli::try_parse_from(["peerbeam", "pipe", "--listen", "--to", "laptop"]).is_err());
    assert!(Cli::try_parse_from(["peerbeam", "pipe", "--to", "a", "--addr", "1.2.3.4:1"]).is_err());
    assert!(Cli::try_parse_from(["peerbeam", "pipe", "--listen", "--addr", "1.2.3.4:1"]).is_err());
}

#[test]
fn pipe_to_and_addr_parse() {
    let cli = Cli::try_parse_from(["peerbeam", "pipe", "--to", "laptop"]).unwrap();
    match cli.command {
        Command::Pipe(a) => {
            assert_eq!(a.to.as_deref(), Some("laptop"));
            assert!(!a.listen);
        }
        _ => panic!("expected pipe"),
    }

    let cli = Cli::try_parse_from(["peerbeam", "pipe", "--addr", "192.168.1.5:49600"]).unwrap();
    match cli.command {
        Command::Pipe(a) => assert_eq!(a.addr.as_deref(), Some("192.168.1.5:49600")),
        _ => panic!("expected pipe"),
    }
}

#[test]
fn pipe_listen_takes_from_and_port() {
    let cli = Cli::try_parse_from([
        "peerbeam", "pipe", "--listen", "--from", "pb-abc", "--port", "50101",
    ])
    .unwrap();
    match cli.command {
        Command::Pipe(a) => {
            assert!(a.listen);
            assert_eq!(a.from.as_deref(), Some("pb-abc"));
            assert_eq!(a.port, Some(50101));
        }
        _ => panic!("expected pipe"),
    }
}

/// `--from` and `--port` only mean anything to a listener; attaching them to a
/// send is a usage error rather than a silently ignored flag.
#[test]
fn pipe_from_and_port_require_listen() {
    assert!(Cli::try_parse_from(["peerbeam", "pipe", "--to", "a", "--from", "pb-b"]).is_err());
    assert!(Cli::try_parse_from(["peerbeam", "pipe", "--to", "a", "--port", "1"]).is_err());
    assert!(Cli::try_parse_from(["peerbeam", "pipe", "--from", "pb-b"]).is_err());
}

// ── trust ───────────────────────────────────────────────────────────────────

#[test]
fn trust_subcommands_parse() {
    let cli = Cli::try_parse_from(["peerbeam", "trust", "list"]).unwrap();
    assert!(matches!(
        cli.command,
        Command::Trust(a) if matches!(a.action, TrustAction::List)
    ));

    let cli = Cli::try_parse_from(["peerbeam", "trust", "approve", "laptop"]).unwrap();
    match cli.command {
        Command::Trust(a) => match a.action {
            TrustAction::Approve { device, duration } => {
                assert_eq!(device, "laptop");
                assert_eq!(duration, None, "no `--for` means until revoked");
            }
            _ => panic!("expected approve"),
        },
        _ => panic!("expected trust"),
    }

    let cli = Cli::try_parse_from(["peerbeam", "trust", "revoke", "pb-abc"]).unwrap();
    match cli.command {
        Command::Trust(a) => match a.action {
            TrustAction::Revoke { device } => assert_eq!(device, "pb-abc"),
            _ => panic!("expected revoke"),
        },
        _ => panic!("expected trust"),
    }
}

/// Both mutating actions name a device. Defaulting to "all", or prompting for
/// a pick, would make a mistyped command approve or revoke something nobody
/// named — and on `approve` that is a stranger gaining this machine's clipboard.
#[test]
fn trust_approve_and_revoke_require_a_device() {
    assert!(Cli::try_parse_from(["peerbeam", "trust", "approve"]).is_err());
    assert!(Cli::try_parse_from(["peerbeam", "trust", "revoke"]).is_err());
    // `list` takes none.
    assert!(Cli::try_parse_from(["peerbeam", "trust", "list", "extra"]).is_err());
}

/// `--for` carries the window through as typed; the duration is parsed by the
/// command, not by clap, so that an unreadable one is a `peerbeam` usage error
/// naming the four units rather than a clap message about a value.
#[test]
fn trust_approve_takes_an_optional_window() {
    let cli =
        Cli::try_parse_from(["peerbeam", "trust", "approve", "laptop", "--for", "30m"]).unwrap();
    match cli.command {
        Command::Trust(a) => match a.action {
            TrustAction::Approve { device, duration } => {
                assert_eq!(device, "laptop");
                assert_eq!(duration.as_deref(), Some("30m"));
            }
            _ => panic!("expected approve"),
        },
        _ => panic!("expected trust"),
    }
    // The flag needs a value: a bare `--for` must not silently mean "forever".
    assert!(Cli::try_parse_from(["peerbeam", "trust", "approve", "laptop", "--for"]).is_err());
}

/// `--yes` is what makes approval scriptable on a headless box; it is a global
/// flag, so it parses on either side of the subcommand.
#[test]
fn trust_approve_accepts_yes_on_either_side() {
    for args in [
        ["peerbeam", "--yes", "trust", "approve", "laptop"],
        ["peerbeam", "trust", "approve", "laptop", "--yes"],
    ] {
        let cli = Cli::try_parse_from(args).unwrap();
        assert!(cli.global.yes);
    }
}

// ── rules ───────────────────────────────────────────────────────────────────

#[test]
fn rules_subcommands_parse() {
    let cli = Cli::try_parse_from(["peerbeam", "rules", "list"]).unwrap();
    assert!(matches!(
        cli.command,
        Command::Rules(a) if matches!(a.action, RulesAction::List)
    ));

    let cli = Cli::try_parse_from(["peerbeam", "rules", "remove", "2"]).unwrap();
    match cli.command {
        Command::Rules(a) => match a.action {
            RulesAction::Remove { index } => assert_eq!(index, 2),
            _ => panic!("expected remove"),
        },
        _ => panic!("expected rules"),
    }
}

/// Every criterion is optional, and the destination is not. A rule with no
/// destination has nowhere to put anything; a rule with no criteria is a
/// legitimate catch-all.
#[test]
fn rules_add_needs_a_destination_and_nothing_else() {
    let cli = Cli::try_parse_from(["peerbeam", "rules", "add", "/srv/inbox"]).unwrap();
    match cli.command {
        Command::Rules(a) => match a.action {
            RulesAction::Add {
                directory,
                from,
                ext,
                min_bytes,
                max_bytes,
                at,
            } => {
                assert_eq!(directory, "/srv/inbox");
                assert_eq!(from, None);
                assert_eq!(ext, None);
                assert_eq!(min_bytes, None);
                assert_eq!(max_bytes, None);
                assert_eq!(at, None);
            }
            _ => panic!("expected add"),
        },
        _ => panic!("expected rules"),
    }

    assert!(Cli::try_parse_from(["peerbeam", "rules", "add"]).is_err());
}

/// Every criterion, and the position — the flag that makes the tie-break
/// reachable from a script.
#[test]
fn rules_add_parses_every_criterion_and_a_position() {
    let cli = Cli::try_parse_from([
        "peerbeam",
        "rules",
        "add",
        "/mnt/big",
        "--from",
        "laptop",
        "--ext",
        "mkv",
        "--min-bytes",
        "1000",
        "--max-bytes",
        "2000",
        "--at",
        "0",
    ])
    .unwrap();
    match cli.command {
        Command::Rules(a) => match a.action {
            RulesAction::Add {
                directory,
                from,
                ext,
                min_bytes,
                max_bytes,
                at,
            } => {
                assert_eq!(directory, "/mnt/big");
                assert_eq!(from.as_deref(), Some("laptop"));
                assert_eq!(ext.as_deref(), Some("mkv"));
                assert_eq!(min_bytes, Some(1000));
                assert_eq!(max_bytes, Some(2000));
                assert_eq!(at, Some(0));
            }
            _ => panic!("expected add"),
        },
        _ => panic!("expected rules"),
    }
}

/// `chat send --reply-to` parses, and cannot be combined with `--file`: a file
/// share is not an answer to a message, and accepting the pair would leave the
/// marker with nowhere to go.
#[test]
fn chat_send_takes_a_reply_target_and_refuses_it_with_a_file() {
    let cli = Cli::try_parse_from([
        "peerbeam",
        "chat",
        "send",
        "--to",
        "bob",
        "--reply-to",
        "m-0001",
        "sure, go ahead",
    ])
    .unwrap();
    match cli.command {
        Command::Chat(a) => match a.action {
            ChatAction::Send { text, reply_to, .. } => {
                assert_eq!(text.as_deref(), Some("sure, go ahead"));
                assert_eq!(reply_to.as_deref(), Some("m-0001"));
            }
            _ => panic!("expected send"),
        },
        _ => panic!("expected chat"),
    }

    assert!(
        Cli::try_parse_from([
            "peerbeam",
            "chat",
            "send",
            "--to",
            "bob",
            "--file",
            "/tmp/a.bin",
            "--reply-to",
            "m-0001",
        ])
        .is_err(),
        "--file and --reply-to must not be accepted together"
    );
}
