//! Helpers shared by the FFI integration tests.
//!
//! A directory module rather than a fourth test file: `tests/common/mod.rs` is
//! not a top-level `tests/*.rs`, so cargo compiles it into each test binary
//! that asks for it instead of running it as a test binary of its own.

/// A UDP port the operating system says is free.
///
/// # Why not a literal
///
/// Because a literal is a flake, and this one cost a red `main`.
///
/// `chat_ffi`'s `chat_only_dial_does_not_register_phantom_transfer` failed on a
/// Windows runner with `connect timed out — peer unreachable` after retrying
/// for a full minute, then passed on a rerun of the byte-identical commit.
/// Nothing was wrong with the code under test: the engine could not bind the
/// port the test had picked.
///
/// That failure is invisible where it happens. `pb_init` returns `ok` as soon
/// as it has *spawned* the listener task; `Manager::serve` then binds, and on
/// failure logs, calls `mark_daemon_stopped()` and returns. Nothing propagates
/// back. So a port the machine will not hand over looks exactly like a peer
/// that never answers — and the test spends its whole dial budget before
/// blaming the network for an unbindable socket.
///
/// These tests used 49823..=49912. That is inside the Windows dynamic port
/// range, of which Hyper-V and WSL reserve blocks at boot
/// (`netsh int ipv4 show excludedportrange udp`); whether a given runner has
/// reserved the block a test asked for is luck, which is why this failed on
/// Windows only and only sometimes.
///
/// An OS-assigned port cannot be in a reserved range, because the OS doing the
/// reserving is the one assigning it.
///
/// # The window
///
/// The probe socket closes before the engine binds, so there is a brief window
/// where something else could take the port. Holding it open is not the
/// alternative — that would stop the engine binding at all. UDP has no
/// `TIME_WAIT`, so the rebind is immediate, and the callers all dial through a
/// bounded retry that covers a transient. This trades a systematic failure for
/// a far less likely one.
#[allow(dead_code)]
pub fn free_port() -> u16 {
    let probe = std::net::UdpSocket::bind("127.0.0.1:0").expect("bind a probe socket");
    let port = probe
        .local_addr()
        .expect("probe socket has an address")
        .port();
    drop(probe);
    port
}
