//! Argument-parsing tests for the CLI surface.

use clap::{CommandFactory, Parser};
use peerbeam_cli::cli::{BenchTarget, Cli, Command, ConfigAction};

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
fn completions_accepts_a_shell() {
    assert!(Cli::try_parse_from(["peerbeam", "completions", "bash"]).is_ok());
    assert!(Cli::try_parse_from(["peerbeam", "completions", "notashell"]).is_err());
}
