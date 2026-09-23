//! What skein keeps under `private/` — the agents' socket, the fleet's tmux socket and the
//! credentials — is out of every box's reach, and the per-file cover it replaced is gone.

use super::*;

/// A box cannot read what skein keeps under `.skein/private/` (SKEIN-516 Rule 1, ISO-2, ISO-4).
///
/// `.skein` is bound back into every box READABLE — the launcher, the credential helper and the
/// toolchain live there and a box needs all three. Everything skein authenticates with lived there
/// too, and one of the four was covered: an empty file was bound over `fleet-agent.token`, by name.
/// The review call's GitHub credential was written into the same directory "beside the fleet
/// agent's token and for the same reason" and got the path without the cover, so every box could
/// `cat` the token the review queue acts on GitHub with as the person who owns the fleet.
///
/// The fix is a directory rather than a list, so the next secret is covered by having been put in
/// the right place rather than by somebody remembering to add a line here.
///
/// **What would make this fail**: deleting the `--tmpfs "$private"` line from the isolation block
/// in `box-session.sh`. Both files then read back through the `--ro-bind` of `.skein` as `see`.
#[test]
fn a_box_cannot_read_what_skein_keeps_under_private() {
    if !bwrap_works() {
        return skip(
            "bwrap cannot create a user \
             namespace here, so the private cover was NOT exercised against a real namespace on \
             this machine",
        );
    }
    let fleet = Fleet::make("private");
    let report = fleet.seen_by_box(Born::Covered);

    for secret in [
        fleet.fleet_root.join(".skein/private/fleet-agent.token"),
        fleet.fleet_root.join(".skein/private/review-github.token"),
    ] {
        assert_eq!(
            verdict(&report, &secret),
            "gone",
            "a box can read {} — that credential runs commands as the sandbox, or acts on GitHub \
             as the person who owns the fleet:\n{report}",
            secret.display()
        );
    }
    // The directory is still *there* and still empty, which is what a tmpfs looks like from inside
    // and is the difference between covering it and deleting it: skein writes here at fleet scope
    // on every review call, and a box writing into its own tmpfs copy reaches nobody.
    assert_eq!(
        verdict(&report, &fleet.fleet_root.join(".skein/private")),
        "empty",
        "the private directory is not a tmpfs from inside the box:\n{report}"
    );
    // The rest of `.skein` is untouched: this is a cover over one directory, not over the launcher
    // and the toolchain a box has to be able to read.
    assert_eq!(
        verdict(&report, &fleet.fleet_root.join(".skein")),
        "see",
        "covering `private/` took the fleet's own scripts with it:\n{report}"
    );

    // **The "gone" assertions are only worth having if the files were there to hide.** A fixture
    // that failed to write one would report "gone" for a path that never existed. The workshop box
    // skips the cover deliberately, so it must read back exactly what the ordinary box could not.
    let workshop = fleet.seen_by_box(Born::Workshop);
    for secret in [
        fleet.fleet_root.join(".skein/private/fleet-agent.token"),
        fleet.fleet_root.join(".skein/private/review-github.token"),
    ] {
        assert_eq!(
            verdict(&workshop, &secret),
            "see",
            "the workshop box cannot see {} either, so the cover is not what hid it from the \
             ordinary box and the assertions above prove nothing:\n{workshop}",
            secret.display()
        );
    }
}

/// A box cannot `connect()` to the fleet agent's socket (ISO-4).
///
/// The agent used to bind `0.0.0.0:8317` and be restarted by a `sleep 2` loop, in a network and pid
/// namespace every box shares at the same uid — so a box could kill it, take the port in the gap,
/// and be handed the fleet agent's token by skein's next call, which sends `X-Skein-Token` before
/// it has learned anything about who answered. That token runs commands as the sandbox in any
/// box's namespace.
///
/// The socket is under the `private/` cover, so this asks the question the cover is answering: not
/// "is the file listed" but "does the kernel let this process connect". They are different
/// questions — a read-only mount refuses neither `connect()` nor `bind()` — and only the second one
/// is the boundary.
///
/// **What would make this fail**: deleting the `--tmpfs "$private"` line. The ordinary box then
/// connects, and the assertion below prints `connected`.
#[test]
fn a_box_cannot_connect_to_the_fleet_agents_socket() {
    if !bwrap_works() {
        return skip(
            "bwrap cannot create a user \
             namespace here, so the agent socket was NOT exercised against a real namespace on \
             this machine",
        );
    }
    if Command::new("python3").arg("-V").output().is_err() {
        return skip(
            "no python3, so no connect() \
             was attempted from inside a namespace on this machine",
        );
    }
    let fleet = Fleet::make("agentsock");
    let sock = fleet.fleet_root.join(".skein/private/fleet-agent.sock");
    // A real listener, accepting for the life of the test: `connect()` against nothing would be
    // refused for a reason that has nothing to do with the cover.
    // Bound and never accepted from, deliberately. A `connect()` to a unix socket succeeds as soon
    // as the kernel can queue it on the listen backlog — no `accept()` is involved — so this is a
    // listening socket for the only question being asked, without a thread whose lifetime the test
    // would then have to manage. Held in scope so it stays bound for BOTH probes: a listener closed
    // after the first would refuse the second for a reason that is not the cover, which is exactly
    // the failure this test exists to avoid.
    let _listener = std::os::unix::net::UnixListener::bind(&sock).expect("a listener");

    // The workshop box FIRST, so the refusal below is measured against a socket that is provably
    // accepting. An absence that was never a presence proves nothing.
    assert_eq!(
        fleet.connect_from_box(Born::Workshop, &sock),
        "connected",
        "the workshop box could not reach a socket that is listening, so the refusal below would \
         not be the cover's doing"
    );
    let ordinary = fleet.connect_from_box(Born::Covered, &sock);
    assert!(
        ordinary.starts_with("refused"),
        "a box reached the fleet agent's socket ({ordinary}) — whoever can speak here is handed \
         the token that runs commands as the sandbox"
    );
}

/// A box cannot `connect()` to the socket the **cockpit's own tmux server** listens on (ISO-3,
/// SKEIN-529).
///
/// This socket sat beside `private/` rather than in it, at the top of `.skein`, which the launcher
/// binds back into every box readable. A read-only bind refuses nothing at all to a socket — the
/// kernel's `sb_permission` returns `EROFS` for regular files, directories and symlinks, and a
/// socket is none of those — and tmux admits any client whose peer uid matches its own, which under
/// one fleet-wide uid is every box's. A tmux client is not a reader: the server honours `MSG_SHELL`
/// and `MSG_EXEC`, so `tmux -S <that path> run-shell …` from inside any box is a command at fleet
/// scope, outside every namespace. The cover was in the right shape and the socket was outside it.
///
/// **The path is taken from `fleet::server_tmux_sock_in` and not spelled here**, which is what makes
/// this a test of the boundary rather than of a string: if the production path moves back out from
/// under `private/`, this probe follows it there and the ordinary box connects.
///
/// **What would make this fail**, run rather than reasoned about: changing `server_tmux_sock_in`
/// back to `format!("{fleet_root}/.skein/server.tmux")`. The workshop leg still says `connected`
/// and the ordinary box says `connected` too, where it must say `refused`. Deleting the
/// `--tmpfs "$private"` line from the launcher's isolation block does the same thing by the other
/// route.
#[test]
fn a_box_cannot_connect_to_the_fleets_tmux_socket() {
    if !bwrap_works() {
        return skip(
            "bwrap cannot create a user \
             namespace here, so the cockpit's tmux socket was NOT exercised against a real \
             namespace on this machine",
        );
    }
    if Command::new("python3").arg("-V").output().is_err() {
        return skip(
            "no python3, so no connect() \
             was attempted from inside a namespace on this machine",
        );
    }
    let fleet = Fleet::make("tmuxsock");
    let sock = PathBuf::from(skein::fleet::server_tmux_sock_in(
        fleet.fleet_root.to_string_lossy().as_ref(),
    ));

    // A listener rather than the real tmux: the question is whether the kernel lets a process in
    // this namespace reach this path, and a tmux server would answer it the same way at the cost of
    // a process the test then has to stop. `bind` failing here is itself a finding — it means the
    // production path no longer lands in a directory the fleet fixture makes.
    let _listener = std::os::unix::net::UnixListener::bind(&sock).unwrap_or_else(|e| {
        panic!(
            "could not bind a listener at the path `fleet::server_tmux_sock_in` returns ({}): {e} \
             — the socket now lands somewhere the fleet does not create, so nothing was tested",
            sock.display()
        )
    });

    // The workshop box FIRST, so the refusal below is measured against a socket that is provably
    // accepting. An absence that was never a presence proves nothing — and the workshop leg is also
    // the half a bind-list test cannot reach: the privileged box skips the tmpfs deliberately, so
    // it must read back exactly what the ordinary box could not.
    assert_eq!(
        fleet.connect_from_box(Born::Workshop, &sock),
        "connected",
        "the workshop box could not reach a socket that is listening at {}, so the refusal below \
         would not be the cover's doing",
        sock.display()
    );
    let ordinary = fleet.connect_from_box(Born::Covered, &sock);
    assert!(
        ordinary.starts_with("refused"),
        "a box reached the cockpit's tmux socket at {} ({ordinary}) — tmux honours MSG_SHELL and \
         MSG_EXEC from the server, so whoever can speak here runs commands at fleet scope, outside \
         every box's namespace",
        sock.display()
    );
}

/// The mechanism `private/` replaces leaves nothing behind (ISO-2).
///
/// The old cover was an empty file at `<box root>/no-fleet-token`, bound over the one credential
/// somebody remembered. A launcher that still created it would leave a file whose only meaning is
/// a mechanism that no longer exists — and, worse, the *shape* that made the review credential
/// reachable: a cover spelled as a file name.
///
/// **What would make this fail**: putting back the `: >"$root/no-fleet-token"` line.
#[test]
fn the_per_file_token_cover_is_gone_from_the_launcher() {
    let src = fs::read_to_string(script("box-session.sh")).unwrap();
    assert!(
        !src.contains(": >\"$root/no-fleet-token\""),
        "the launcher still creates the per-file token cover, so `.skein` is being protected by an \
         enumeration again"
    );
    assert!(
        !src.contains("--ro-bind \"$root/no-fleet-token\""),
        "the launcher still binds the per-file token cover"
    );
    // The replacement, named: one tmpfs over one directory, applied by the isolation block so a
    // privileged box skips it exactly as it skips everything else there.
    let block = isolation_block();
    assert!(
        block.contains("--tmpfs \"$private\""),
        "nothing covers `.skein/private/`, so every secret in it is readable from every box"
    );
}
