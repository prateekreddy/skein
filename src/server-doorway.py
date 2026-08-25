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

Arguments: <port> <path-to-skein-server>. Run by the tmux session `fleet::start_server` makes.
"""

import os
import socket
import sys
import time

FIRST = 3  # doorway.rs::FIRST — descriptor 0..2 are the process's own streams


def main() -> None:
    if len(sys.argv) != 3:
        sys.stderr.write("usage: server-doorway.py <port> <skein-server>\n")
        sys.exit(2)
    port, server = int(sys.argv[1]), sys.argv[2]

    door = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    door.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    try:
        door.bind(("0.0.0.0", port))
    except OSError as e:
        # The named failure, because it is the exact race this file exists to close: something in
        # the sandbox is already holding the cockpit's port, and serving anyway from another port
        # would leave the browser talking to whatever got there first.
        sys.stderr.write(
            f"server-doorway: cannot bind :{port} ({e}) — something inside the sandbox holds "
            f"the cockpit's port. If that is a previous doorway, stop it first; if it is not, "
            f"a box has taken the port, which is architecture §9.4's squat.\n"
        )
        sys.exit(1)
    door.listen(128)

    # The supervisor IS the socket holder. A `while true` around this whole script would reopen
    # the socket per server crash, and the gap between close and bind is the squat window again.
    while True:
        pid = os.fork()
        if pid == 0:
            if door.fileno() != FIRST:
                os.dup2(door.fileno(), FIRST)  # dup2 makes the copy inheritable
            else:
                os.set_inheritable(FIRST, True)
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
        _, status = os.waitpid(pid, 0)
        sys.stderr.write(
            f"server-doorway: skein-server exited (wait status {status}); "
            f"restarting behind the same socket\n"
        )
        time.sleep(2)


if __name__ == "__main__":
    main()
