//! Which socket the cockpit is served on, and whether skein is the one that opened it.
//!
//! architecture §9.5 R3 was decided against — the port stays, because a browser cannot open a
//! filesystem socket and every alternative cost either a proxy the page then depends on or a tunnel
//! before the first load. §9.4 records what that leaves open, and two of the three residues are
//! accepted deliberately: the HTTP surface is reachable from every box, and connecting costs
//! something bounded by [`crate::knock`].
//!
//! **The third has no answer the token can give.** One network namespace plus a mapping that
//! outlives skein — `sbx ports --unpublish` exists but is not a call skein makes (the argument is
//! carried once, at `fleet::stop_serving`), and it withdraws the HOST end, where this race is on
//! the SANDBOX end (§7.4) — means a box that binds the
//! cockpit's port *before skein does* becomes the cockpit — and the browser hands it the fleet token on the first request. A
//! separate uid stops `SO_REUSEPORT` theft from a **live** listener; it says nothing about an empty
//! port at sandbox start. The filesystem socket would have closed this for free, since a box cannot
//! create a socket at a path it cannot see.
//!
//! So the port is never free: the listening socket is opened **once, before any box exists**, and
//! *inherited* by skein rather than re-bound by whoever gets there first. This module is skein's
//! half of that — taking a descriptor it was handed, and refusing one it was handed wrongly.
//!
//! ## The convention, not a dependency on the thing that invented it
//!
//! `LISTEN_FDS` / `LISTEN_PID` is systemd's socket-activation protocol, and it is used here because
//! it is the one spelling every process manager already knows: descriptor 3 upwards, a count in
//! `LISTEN_FDS`, and `LISTEN_PID` naming who the descriptors are for so an exec'd child does not
//! act on its parent's. Nothing systemd is required to produce it — the in-fleet start passes a
//! plain inherited fd and sets two environment variables.
//!
//! ## What "validated" can and cannot mean here
//!
//! An accept loop handed a descriptor that is not a listening socket fails **every** time, forever.
//! That is the shape worth stopping, and the reason it is worth stopping at startup is that a
//! permanent error and a temporary one look identical from inside the loop.
//!
//! Two of the four ways to get it wrong are caught here, with the standard library alone:
//!
//!   * **not a socket** — a regular file, a pipe, a closed descriptor. `local_addr` is `getsockname`
//!     and fails with `ENOTSOCK`.
//!   * **a connected socket** — an accepted stream passed on by mistake. A listening socket has no
//!     peer, so a `peer_addr` that *succeeds* is the proof this is the wrong end of a connection.
//!
//! The other two — a datagram socket, and a socket bound but never listened on — are indistinguish-
//! able from a listener without `getsockopt(SO_ACCEPTCONN)`, which the standard library does not
//! expose. They are not left to spin: the accept loop gives up when it has failed repeatedly and
//! has **never** served a connection, which is the honest reading of "this descriptor was wrong"
//! and is also the one thing that distinguishes it from `EMFILE`, where the server has been working
//! and should keep trying.

use std::os::fd::{FromRawFd, RawFd};

/// The first descriptor the convention passes, and it is 3 because 0, 1 and 2 are already spoken
/// for. `SD_LISTEN_FDS_START` in systemd's headers; a number here, since importing a crate for one
/// constant would be the tail wagging the dog.
pub const FIRST: RawFd = 3;

/// When set, a missing descriptor is a startup failure rather than a reason to bind.
///
/// Two modes want opposite answers and the difference must be *said* rather than defaulted.
/// Host-driven, there is nobody upstream to open a socket, so binding is the only way to start and
/// falling back is right. In-fleet, a missing descriptor means the start sequence did not do its
/// job — and binding anyway is precisely the race this exists to close, run by the one process that
/// would otherwise have closed it.
pub const INHERITED_ONLY: &str = "SKEIN_LISTEN_INHERITED_ONLY";

/// Whether a missing descriptor should stop the server rather than send it to `bind`.
pub fn inherited_only() -> bool {
    std::env::var(INHERITED_ONLY).is_ok_and(|value| value == "1")
}

/// Which descriptor was passed, from the two environment variables that carry the convention.
///
/// `Ok(None)` is "nobody passed one", which is a normal start and not an error. Everything else is
/// a caller that *meant* to pass one and got it wrong, and each answer says which — a start that
/// silently ignored a malformed `LISTEN_FDS` would bind, which is the thing being prevented.
///
/// `me` is this process's pid, taken as an argument so the rule can be tested: `LISTEN_PID` exists
/// because the variables survive `exec`, and a child that acted on its parent's descriptors would
/// take over a socket meant for somebody else.
pub fn descriptor(fds: Option<&str>, pid: Option<&str>, me: u32) -> Result<Option<RawFd>, String> {
    let Some(fds) = fds.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    // Checked before the count, because a mismatch is not "no descriptor was passed" — it is a
    // descriptor passed to somebody else, and the difference decides whether binding is allowed.
    if let Some(pid) = pid.map(str::trim).filter(|value| !value.is_empty()) {
        let owner: u32 = pid
            .parse()
            .map_err(|_| format!("LISTEN_PID is {pid:?}, which is not a process id"))?;
        if owner != me {
            return Err(format!(
                "LISTEN_PID is {owner} and this process is {me} — the descriptors were passed to \
                 something else, and taking them would be taking over somebody else's socket"
            ));
        }
    }
    let count: u32 = fds
        .parse()
        .map_err(|_| format!("LISTEN_FDS is {fds:?}, which is not a count"))?;
    match count {
        0 => Ok(None),
        // One, and no more. Several would mean choosing, and there is no rule here for which of
        // them is the cockpit — a start that passed two has a bug the server cannot resolve for it.
        1 => Ok(Some(FIRST)),
        many => Err(format!(
            "LISTEN_FDS is {many}, and skein-server serves one socket — it has no way to tell which \
             of them is the cockpit's, so it will not guess"
        )),
    }
}

/// Mark `fd` close-on-exec, so no process this one starts receives it.
///
/// **The descriptor arrives inheritable on purpose, and must stop being so here** (SKEIN-1035).
/// `src/server-doorway.py` `dup2`s the listener onto descriptor [`FIRST`] because that is the only
/// way it survives the doorway's `exec` into skein-server — and `CLOEXEC` is the one flag `exec`
/// consults, so the same bit that carried it across that exec carries it across every later one.
/// Left alone, every child skein-server starts holds the cockpit's LISTENING socket. Measured: the
/// tmux server `heal_fleet` starts at boot held it, so with the doorway and the server both gone
/// the port stays bound by a process that never accepts, and a new doorway cannot bind it.
///
/// Setting it in this process does not reach back across the exec that brought the descriptor in:
/// the doorway's own re-exec across `SIGUSR1` is python's, on the doorway's own copy, which this
/// never touches.
pub fn close_on_exec(fd: RawFd) -> std::io::Result<()> {
    // SAFETY: `F_GETFD` and `F_SETFD` read and write one descriptor's flags and nothing else; no
    // memory is passed, and a descriptor that is not open comes back as `EBADF`, not as harm.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    if flags < 0 {
        return Err(std::io::Error::last_os_error());
    }
    if unsafe { libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC) } < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

/// Withhold the handed-in socket from children, **before any child exists**.
///
/// Not left to [`inherited`], and the reason is the order of `main`, not taste. `inherited` runs
/// where the port is served, near the end of start-up — after `fleet::heal_fleet` has already
/// started the fleet's tmux server and the watchers have been spawned. Every process started in
/// between would inherit the socket whatever `adopt` then did. So this is the first thing `main`
/// does, and it only reads: the descriptor is validated and taken later, by `inherited`, exactly as
/// before. A descriptor that is not ours to take (`LISTEN_PID` naming somebody else, or a malformed
/// count) is left alone here, because `inherited` refuses it with the reason and exits.
pub fn keep_from_children() {
    let fds = std::env::var("LISTEN_FDS").ok();
    let pid = std::env::var("LISTEN_PID").ok();
    if let Ok(Some(fd)) = descriptor(fds.as_deref(), pid.as_deref(), std::process::id()) {
        // An error is `EBADF`: nothing was passed at that number, and `adopt` says so in words.
        let _ = close_on_exec(fd);
    }
}

/// Adopt `fd` as the listening socket, or say why it cannot be one.
///
/// # Safety
///
/// The descriptor must not be owned by anything else in this process: this takes it, and the
/// returned listener closes it on drop. In the intended use it came from the parent across `exec`
/// and nothing here has ever seen it.
pub unsafe fn adopt(fd: RawFd) -> Result<std::net::TcpListener, String> {
    if fd < FIRST {
        return Err(format!(
            "descriptor {fd} is one of this process's own standard streams, not a socket somebody \
             passed in"
        ));
    }
    // Held as a `TcpStream` for the length of the checks, and only because that is the type with
    // `peer_addr` on it — the same descriptor either way. Owning it here rather than borrowing is
    // what makes every refusal below close the thing it refused instead of leaking it.
    let probe = std::net::TcpStream::from_raw_fd(fd);
    let addr = probe
        .local_addr()
        .map_err(|e| format!("descriptor {fd} is not a socket ({e})"))?;
    // A listening socket has no peer. Asking is the whole check: a `peer_addr` that answers is an
    // accepted connection handed over by mistake, and accepting on one fails forever.
    if let Ok(peer) = probe.peer_addr() {
        return Err(format!(
            "descriptor {fd} is a connection to {peer}, not a socket anybody can arrive on"
        ));
    }
    // Before it becomes a listener, since a blocking accept in an async runtime stalls the whole
    // server on its first arrival rather than failing where anybody would see it.
    probe
        .set_nonblocking(true)
        .map_err(|e| format!("descriptor {fd} ({addr}) could not be made non-blocking: {e}"))?;
    // Once taken, it is this process's and nobody else's — see [`close_on_exec`]. Here as well as in
    // [`keep_from_children`] so that `adopt` keeps its own promise whoever calls it.
    close_on_exec(fd).map_err(|e| {
        format!("descriptor {fd} ({addr}) could not be kept from this process's children: {e}")
    })?;
    Ok(std::net::TcpListener::from_raw_fd(
        std::os::fd::IntoRawFd::into_raw_fd(probe),
    ))
}

/// The socket skein was handed, if it was handed one.
///
/// `Ok(None)` means nobody passed one — which [`inherited_only`] decides what to do about, since
/// that answer is a deployment question and not this function's.
pub fn inherited() -> Result<Option<std::net::TcpListener>, String> {
    let fds = std::env::var("LISTEN_FDS").ok();
    let pid = std::env::var("LISTEN_PID").ok();
    let Some(fd) = descriptor(fds.as_deref(), pid.as_deref(), std::process::id())? else {
        return Ok(None);
    };
    // SAFETY: the descriptor came in across `exec` from whoever started this process; nothing in
    // skein has opened, duplicated or closed it, and `descriptor` refused everything below `FIRST`.
    let listener = unsafe { adopt(fd) }?;
    // Cleared so nothing downstream acts on them a second time. A child skein spawns inherits this
    // environment, and `LISTEN_PID` would no longer match — but a child that ignored it and adopted
    // fd 3 anyway would be adopting the cockpit's own socket.
    // Startup only, before any thread exists — which is what makes these safe. `remove_var` mutates
    // process-global state that other threads may be reading, and Rust 2024 marks it `unsafe` for
    // exactly that reason; this runs once, from `main`, before the runtime is built. Moving this
    // call anywhere later stops being sound, so it says so here rather than in a commit message.
    std::env::remove_var("LISTEN_FDS");
    std::env::remove_var("LISTEN_PID");
    Ok(Some(listener))
}

/// The sentence a start with no descriptor gets when one was required.
pub fn missing() -> String {
    format!(
        "skein-server: {INHERITED_ONLY}=1 and no listening socket was passed in. In the fleet the \
         cockpit's socket is opened before any box exists, precisely so no box can bind that port \
         first (architecture §9.4) — binding here would run the race it exists to close. Whatever \
         starts skein-server must pass the socket as descriptor {FIRST} with LISTEN_FDS=1 and \
         LISTEN_PID set to this process, or unset {INHERITED_ONLY} to bind (host-driven mode)."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_descriptor_is_a_normal_start_rather_than_an_error() {
        assert_eq!(descriptor(None, None, 42), Ok(None));
        assert_eq!(descriptor(Some(""), None, 42), Ok(None));
        assert_eq!(descriptor(Some("0"), Some("42"), 42), Ok(None));
    }

    #[test]
    fn one_descriptor_is_the_one_the_convention_names() {
        assert_eq!(descriptor(Some("1"), Some("42"), 42), Ok(Some(FIRST)));
        // No `LISTEN_PID` at all is the older half of the convention, and it is accepted: something
        // that passes a socket without one has still passed a socket.
        assert_eq!(descriptor(Some("1"), None, 42), Ok(Some(FIRST)));
    }

    /// The variables survive `exec`, so a child sees its parent's. Acting on them would be taking
    /// over a socket meant for somebody else — and in this system that socket is the cockpit, whose
    /// first request carries the fleet's token.
    #[test]
    fn descriptors_passed_to_another_process_are_not_taken() {
        let why = descriptor(Some("1"), Some("41"), 42).expect_err("another pid's fds were taken");
        assert!(why.contains("41") && why.contains("42"), "{why}");
        assert!(why.contains("somebody else"), "{why}");
    }

    /// Every malformed answer is an error rather than a fall-through to `None`. A start that read
    /// `LISTEN_FDS=two` as "nobody passed one" would bind — which is the race this closes.
    #[test]
    fn a_start_that_meant_to_pass_a_socket_and_got_it_wrong_is_not_read_as_passing_none() {
        for (fds, pid) in [("two", Some("42")), ("1", Some("later"))] {
            assert!(
                descriptor(Some(fds), pid, 42).is_err(),
                "LISTEN_FDS={fds:?} LISTEN_PID={pid:?} was read as a normal start"
            );
        }
        let many = descriptor(Some("3"), Some("42"), 42).expect_err("three sockets were accepted");
        assert!(many.contains("will not guess"), "{many}");
    }

    /// The two ways a wrong descriptor is caught before the accept loop ever sees it. The loop that
    /// would otherwise get them fails on every iteration, forever, and cannot tell that from
    /// `EMFILE` — which is temporary and must be waited out rather than exited on.
    #[test]
    fn a_descriptor_that_cannot_be_a_door_is_refused_at_the_door() {
        use std::io::Write;
        use std::os::fd::IntoRawFd;

        // Not a socket: a file. `getsockname` on it is ENOTSOCK.
        let dir = crate::testutil::tempdir();
        let path = dir.join("not-a-socket");
        let mut file = std::fs::File::create(&path).unwrap();
        file.write_all(b"x").unwrap();
        let fd = file.into_raw_fd();
        let why = unsafe { adopt(fd) }.expect_err("a regular file was adopted as a listener");
        assert!(why.contains("not a socket"), "{why}");
        // `adopt` took it and dropped it on the error path, so there is nothing left to close.

        // A connected socket: the wrong end of a connection, which accepts nothing ever.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let client = std::net::TcpStream::connect(addr).unwrap();
        let served = listener.accept().unwrap().0;
        let why = unsafe { adopt(served.into_raw_fd()) }
            .expect_err("an accepted connection was adopted as a listener");
        assert!(why.contains("is a connection to"), "{why}");
        drop(client);

        // And the real thing is taken, non-blocking, with its address intact — the assertion that
        // stops the two above from passing against a function that refuses everything.
        let real = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = real.local_addr().unwrap();
        let adopted = unsafe { adopt(real.into_raw_fd()) }.expect("a listening socket is a door");
        assert_eq!(adopted.local_addr().unwrap(), addr);
        assert!(
            adopted.accept().is_err(),
            "the adopted listener still blocks, so one arrival would stall the whole server"
        );
    }

    /// `adopt` takes the descriptor away from every process this one will start (SKEIN-1035).
    ///
    /// Arrived at through `dup`, because that is the shape the doorway hands it over in — `dup2`
    /// onto descriptor 3, which never copies `CLOEXEC` — and because a listener std opened itself is
    /// close-on-exec already, so asserting on one would pass against an `adopt` that did nothing.
    /// **What makes it fail**: drop the `close_on_exec` call from `adopt`.
    #[test]
    fn an_adopted_socket_is_not_handed_on_to_children() {
        use std::os::fd::AsRawFd;
        let cloexec = |fd| unsafe { libc::fcntl(fd, libc::F_GETFD) } & libc::FD_CLOEXEC != 0;
        let real = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let handed = unsafe { libc::dup(real.as_raw_fd()) };
        assert!(
            handed >= FIRST,
            "dup failed: {}",
            std::io::Error::last_os_error()
        );
        assert!(
            !cloexec(handed),
            "the descriptor was close-on-exec before `adopt` saw it, so this proves nothing"
        );
        let adopted = unsafe { adopt(handed) }.expect("a listening socket is a door");
        assert!(
            cloexec(adopted.as_raw_fd()),
            "descriptor {handed} is still inheritable after `adopt`, so every process the server \
             starts holds the cockpit's listening socket"
        );
    }

    /// Below `FIRST` is this process's own stdin/stdout/stderr. Adopting one would close it.
    #[test]
    fn the_servers_own_standard_streams_are_never_adopted() {
        for fd in 0..FIRST {
            let why = unsafe { adopt(fd) }.expect_err("a standard stream was adopted as a socket");
            assert!(why.contains("standard streams"), "{why}");
        }
    }

    /// The refusal has to name the way out, and both of them: pass the socket, or say this is the
    /// host-driven mode where binding is correct.
    #[test]
    fn the_refusal_names_both_ways_out() {
        let why = missing();
        assert!(why.contains("LISTEN_FDS=1"), "{why}");
        assert!(why.contains(INHERITED_ONLY), "{why}");
        assert!(why.contains("§9.4"), "{why}");
    }
}
