#!/usr/bin/env python3
"""Open the cockpit's socket, then run skein-server behind it — in that order, always.

This is the in-fleet start's half of the handover `src/doorway.rs` is skein-server's half of.
One network namespace plus a port mapping nothing can unpublish (architecture §9.4) means a box
that binds the cockpit's port before skein does *becomes* the cockpit, and the browser hands it
the fleet token on the first request. The only thing that closes that race is a socket opened
before any box exists and never given up — so this process binds first, holds the listener for
as long as it lives, and every skein-server it starts inherits the same descriptor. A server
crash or upgrade restarts the *server*; the door stays open throughout.

The convention is systemd's socket-activation spelling, because it is the one every process
manager already knows and the one `doorway.rs` validates: the listener at descriptor 3,
LISTEN_FDS=1, LISTEN_PID naming the process the descriptor is for. Nothing systemd is required.

Bound on the wildcard address, not loopback: the port is published to the host with
`sbx ports --publish`, and a published port reaches the sandbox's *address* — a loopback bind
here is a mapping that connects and then refuses, which is the bug the fleet agent already met.

## Surviving this process's own restart

The door being open across a *server* restart was never the hard half; the hard half is what
happens to the socket when the doorway itself has to go. Four things answer it, and each closes
a way the port becomes free with boxes already running:

  * **A reload is an exec, not a re-bind.** `SIGUSR1` stops the server and re-execs *this* script
    across the same descriptor, using the same convention it hands downwards — so the doorway is
    both halves of the protocol, and `skein fleet-serve` against a live fleet upgrades the binary
    with the listening socket never once closed. `exec` keeps the pid, so `LISTEN_PID` is still
    this process and the successor's own validation passes.
  * **The server dies with the doorway.** The child inherits the listener, so a doorway killed on
    its own would leave an orphan still holding the port — and the replacement doorway could then
    never bind, which is a wedge rather than a window. `PR_SET_PDEATHSIG` makes the server's death
    part of the doorway's.
  * **A bind that loses to the outgoing doorway waits for it.** `EADDRINUSE` in the first seconds
    of a restart is the predecessor still exiting, not a squatter; past that window it is treated
    as the squat and named as one.
  * **The supervisor restarts it at once.** The `while` loop around this script sleeps only when
    the doorway died in its first seconds — a crash-loop backs off, and a doorway that had been up
    is replaced in the time it takes python to start, because that gap is the port standing empty.

## The stamp

After acquiring the socket — bound or inherited — the doorway writes `<pid> <port>` to the stamp
path it was given. It is how skein tells "the doorway holds the port" from "*something* answers on
the port", and those are different facts: a squatter answers too, and publishing the cockpit's
mapping to one is how the browser is handed to it. Read the way `places/` records are read — the
pid must be alive *and* be this doorway — because a pid on its own is a number that gets reused.

Arguments: <port> <path-to-skein-server> <path-to-stamp>. Run by the tmux session
`fleet::start_server` makes; started at fleet *create* by `fleet::ensure_fleet_door`, before the
first box exists, which is the moment that actually closes the race.
"""

import errno
import os
import signal
import socket
import sys
import time

FIRST = 3  # doorway.rs::FIRST — descriptor 0..2 are the process's own streams

# How long `EADDRINUSE` is read as "the doorway being replaced has not finished exiting" rather
# than as a squat. Generous against a slow sandbox and still far shorter than the window a
# `sleep` in the supervisor would open, because nothing here is *waiting* on it in the good case.
BIND_RETRY_SECONDS = 10.0

PR_SET_PDEATHSIG = 1  # linux/prctl.h; the sandbox is Linux whichever host made it


def refuse(port: int, why: OSError) -> None:
    """The named failure, because it is the exact race this file exists to close."""
    sys.stderr.write(
        f"server-doorway: cannot bind :{port} ({why}) — something inside the sandbox holds "
        f"the cockpit's port. If that is a previous doorway, stop it first; if it is not, "
        f"a box has taken the port, which is architecture §9.4's squat.\n"
    )
    sys.exit(1)


def handed_over(port: int):
    """The listener a predecessor doorway passed across `exec`, or None if nobody passed one.

    The same three questions `doorway.rs::descriptor` asks, for the same reason: the variables
    survive `exec`, so acting on somebody else's would be taking over their socket. A descriptor
    that is not the cockpit's port is a handover mistake and is refused rather than served on —
    the whole point of inheriting is that the port never changed.
    """
    if os.environ.get("LISTEN_FDS") != "1":
        return None
    owner = os.environ.get("LISTEN_PID")
    if owner and owner != str(os.getpid()):
        return None
    try:
        door = socket.socket(fileno=FIRST)
        bound = door.getsockname()[1]
    except OSError as e:
        sys.stderr.write(f"server-doorway: LISTEN_FDS=1 but descriptor {FIRST} is not a socket ({e})\n")
        return None
    if bound != port:
        sys.stderr.write(
            f"server-doorway: the descriptor handed over is bound to :{bound}, not the cockpit's "
            f":{port} — serving on it would move the cockpit and leave :{port} free\n"
        )
        sys.exit(1)
    return door


def acquire(port: int) -> socket.socket:
    """The listening socket: taken from a predecessor if there is one, bound if there is not."""
    door = handed_over(port)
    if door is not None:
        # Already listening — `listen` again would only reset the backlog, and the queue that
        # accumulated during the exec is exactly what must not be thrown away.
        return door
    door = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    door.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    deadline = time.monotonic() + BIND_RETRY_SECONDS
    while True:
        try:
            door.bind(("0.0.0.0", port))
            break
        except OSError as e:
            if e.errno == errno.EADDRINUSE and time.monotonic() < deadline:
                time.sleep(0.02)
                continue
            refuse(port, e)
    door.listen(128)
    return door


def stamp(path: str, port: int) -> None:
    """Record that *this* process holds the port, once it does. See "The stamp" above."""
    try:
        with open(path, "w") as f:
            f.write(f"{os.getpid()} {port}\n")
    except OSError as e:
        # Not fatal: the door is open, which is the property that matters. What is lost is
        # skein's ability to *prove* it is the doorway answering, so it is said out loud.
        sys.stderr.write(f"server-doorway: cannot write the stamp {path} ({e})\n")


def pass_down(door: socket.socket) -> None:
    """Put the listener at descriptor 3, inheritable, ready for the exec that follows."""
    if door.fileno() != FIRST:
        os.dup2(door.fileno(), FIRST)  # dup2 makes the copy inheritable
    else:
        os.set_inheritable(FIRST, True)


def die_with_parent(parent: int) -> None:
    """Ask the kernel to kill this child when the doorway dies.

    Without it a doorway killed on its own leaves the server behind, still holding the inherited
    listener — so the port is not free (which is fine) and the replacement doorway can never bind
    it (which is not). Best-effort: on a libc without `prctl` the loss is that wedge, not the
    door, so it is not worth refusing to start a server over.

    Imported here rather than at the top of the file, and it is not a style choice: loading a
    shared library opens a descriptor, and doing that before the socket exists is how the listener
    ends up at 4 instead of 3 in the doorway that owns it. In the child it is harmless — this runs
    after the fork and the exec is a line away.
    """
    try:
        import ctypes

        ctypes.CDLL(None, use_errno=True).prctl(PR_SET_PDEATHSIG, signal.SIGTERM, 0, 0, 0)
    except Exception:
        return
    # The signal is delivered on the parent's death, so a parent that died between the fork and
    # the line above delivered nothing and this child would outlive it anyway.
    if os.getppid() != parent:
        os._exit(0)


def reexec(door: socket.socket, stamp_path: str) -> None:
    """Replace this image with the doorway on disk, across the same listening socket.

    This is the whole answer to "the doorway survives its own restart": there is no re-bind, so
    there is no window. The script at `sys.argv[0]` is whatever the last install renamed into
    place, which is why a reload picks up a new doorway as well as a new server.
    """
    pass_down(door)
    env = dict(os.environ)
    env["LISTEN_FDS"] = "1"
    # exec replaces the image, not the pid, so this is still the process the descriptor is for.
    env["LISTEN_PID"] = str(os.getpid())
    python = sys.executable or "python3"
    # argv[0] is the *basename*, not the interpreter's full path, because skein finds this process
    # with `pkill -f '^python[0-9.]* <doorway>( |$)'` — the same anchored pattern the fleet agent
    # uses. Re-execing as `/usr/bin/python3 …` would rewrite the command line out from under that
    # pattern, and the first thing to break would be the next reload.
    argv = [os.path.basename(python), os.path.abspath(sys.argv[0])] + sys.argv[1:]
    try:
        os.execve(python, argv, env)
    except OSError as e:
        # The door is still open in this process, so carrying on serving the old image is
        # strictly better than exiting and freeing the port to say so.
        sys.stderr.write(f"server-doorway: cannot re-exec {argv[1]}: {e}; keeping the old image\n")
        stamp(stamp_path, int(sys.argv[1]))


def spawn(door: socket.socket, server: str) -> int:
    """Fork and exec skein-server behind the open door. Returns the child's pid."""
    parent = os.getpid()
    pid = os.fork()
    if pid != 0:
        return pid
    die_with_parent(parent)
    pass_down(door)
    env = dict(os.environ)
    env["LISTEN_FDS"] = "1"
    # exec replaces the image, not the pid, so this child's pid is the server's.
    env["LISTEN_PID"] = str(os.getpid())
    # Binding here would be the race run by the process meant to close it: doorway.rs
    # turns a start that lost its descriptor into a refusal instead of a bind.
    env["SKEIN_LISTEN_INHERITED_ONLY"] = "1"
    # The one variable (src/deployment.rs). Only this start path sets it; a skein-server
    # run by hand on a host is host-driven with nothing set, exactly as it always was.
    env["SKEIN_IN_FLEET"] = "1"
    try:
        os.execve(server, [server], env)
    except OSError as e:
        sys.stderr.write(f"server-doorway: cannot exec {server}: {e}\n")
    os._exit(127)


def main() -> None:
    if len(sys.argv) != 4:
        sys.stderr.write("usage: server-doorway.py <port> <skein-server> <stamp>\n")
        sys.exit(2)
    port, server, stamp_path = int(sys.argv[1]), sys.argv[2], sys.argv[3]

    door = acquire(port)
    stamp(stamp_path, port)

    # A handler rather than a flag alone, because the doorway spends its life in `waitpid`: the
    # signal has to end the wait, and the only thing that ends it is the child going away.
    state = {"child": 0, "reload": False}

    def on_reload(_signum, _frame):
        state["reload"] = True
        if state["child"]:
            try:
                os.kill(state["child"], signal.SIGTERM)
            except OSError:
                pass

    signal.signal(signal.SIGUSR1, on_reload)

    # The supervisor IS the socket holder. A `while true` around this whole script would reopen
    # the socket per server crash, and the gap between close and bind is the squat window again.
    said_missing = False
    while True:
        if state["reload"]:
            state["reload"] = False
            reexec(door, stamp_path)
        if not os.access(server, os.X_OK):
            # The create-time state, and the reason the door can open before there is anything to
            # put behind it: `ensure_fleet_door` starts this the moment the sandbox exists, and
            # the binary arrives later at `skein fleet-serve`. Said once — the door is open, and a
            # line every half-second would bury the pane that says so.
            if not said_missing:
                sys.stderr.write(
                    f"server-doorway: holding :{port} with no server at {server} yet — the door is "
                    f"open and waiting for `skein fleet-serve` to install one\n"
                )
                said_missing = True
            time.sleep(0.5)
            continue
        said_missing = False
        state["child"] = spawn(door, server)
        _, status = os.waitpid(state["child"], 0)
        state["child"] = 0
        if state["reload"]:
            continue  # straight to the re-exec at the top, with no restart delay in between
        sys.stderr.write(
            f"server-doorway: skein-server exited (wait status {status}); "
            f"restarting behind the same socket\n"
        )
        time.sleep(2)


if __name__ == "__main__":
    main()
