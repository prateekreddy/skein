#!/usr/bin/env python3
"""skein's in-sandbox agent — one long-lived process that runs what `sbx exec` used to spawn.

Why this exists, precisely: under load the sandbox's service path stalls, and the stall is
asymmetric. **New** `sbx exec` calls hang; **established** streams keep flowing. Measured during the
2026-08-05 wedge — four attached terminals carried on while every one of skein's gate refreshes hung
to its timeout, went degraded, and took the board stale. The cockpit went blind while the boxes it
was watching were perfectly healthy.

So this is not a throughput optimisation. skein's polling is already collapsed to well under one call
a second by `Gate`, and moving those calls to HTTP would not make them fewer. It is an availability
one: a held-open connection is an established stream, and answers through exactly the condition that
blinds the board today.

Python's standard library rather than a compiled binary, and that is a constraint rather than a
preference: the host is a Mac, the sandbox is Linux, and cross-compiling would mean installing a
Linux toolchain on the host — the one thing skein's owner has ruled out. stdlib means no install
step at all, and `python3` is already here (dockerd's config is written with it).

Scope is deliberately narrow. This carries the small, frequent calls whose *availability* matters:
liveness, resources, disk, probes. Large uploads and anything with a secret on stdin stay on
`sbx exec`, which streams and keeps the payload out of argv. See `Place::write` on the host side.
"""

import base64
import hmac
import json
import os
import subprocess
import sys
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

# Bigger than any script skein sends, small enough that a stray POST cannot exhaust the sandbox.
MAX_BODY = 1 << 20
# stderr rides back in a header, so it is capped rather than truncated silently at the HTTP layer:
# a header a proxy refuses would lose the error text that is the whole point of returning it.
MAX_STDERR = 8192
DEFAULT_TIMEOUT = 30.0


def _argv(req):
    """The same argv `Place::exec_argv` builds, minus the `sbx exec` hop we are replacing.

    The namespace hop and the HOME/tree wrapping are reproduced here rather than sent pre-built by
    the host, so a host that is newer than this agent cannot hand it a shell it does not understand.
    That failure mode is not hypothetical: a launcher older than the skein driving it is what took
    the whole fleet down once already.
    """
    script = req["script"]
    ns_pid = req.get("ns_pid")
    if ns_pid:
        home, tree, name = req.get("home", ""), req.get("tree", ""), req.get("name", "")
        # Order and flags matter and were verified inside a real box: both namespaces must be joined
        # together, and credentials preserved or setgroups fails for an unprivileged caller.
        prefix = [
            "nsenter",
            f"--user=/proc/{int(ns_pid)}/ns/user",
            f"--mount=/proc/{int(ns_pid)}/ns/mnt",
            "--preserve-credentials",
            "--",
        ]
        wrapped = (
            f"export HOME={_quote(home)} SKEIN_BOX={_quote(name)} "
            f"&& cd {_quote(tree)} && {script}"
        )
        return prefix + ["bash", "-lc", wrapped]
    return ["bash", "-lc", script]


def _quote(s):
    return "'" + str(s).replace("'", "'\\''") + "'"


class Handler(BaseHTTPRequestHandler):
    # Set by main(); a class attribute so every thread shares the one comparison value.
    token = ""

    def log_message(self, *args):
        """Silence per-request logging. The agent's stdout is the sandbox's log; a line per liveness
        poll every 1.5s would bury anything worth reading in a day's worth of noise."""

    def _authed(self):
        # compare_digest, not ==: the token is a secret and == leaks its prefix through timing.
        supplied = self.headers.get("X-Skein-Token", "")
        return self.token and hmac.compare_digest(supplied, self.token)

    def _fail(self, code, why):
        body = why.encode()
        self.send_response(code)
        self.send_header("Content-Type", "text/plain")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):
        # Unauthenticated on purpose and answers nothing but its own name: this is what the host
        # probes to decide whether the agent is worth using, and that decision must not depend on
        # the token being current.
        if self.path == "/health":
            self._fail(200, "skein-fleet-agent")
        else:
            self._fail(404, "no")

    def do_POST(self):
        if self.path != "/exec":
            return self._fail(404, "no")
        if not self._authed():
            return self._fail(403, "bad token")
        try:
            length = int(self.headers.get("Content-Length", "0"))
        except ValueError:
            return self._fail(400, "bad length")
        if length <= 0 or length > MAX_BODY:
            return self._fail(413, "body too large")
        try:
            req = json.loads(self.rfile.read(length))
            argv = _argv(req)
        except (ValueError, KeyError, TypeError) as e:
            return self._fail(400, f"bad request: {e}")

        timeout = float(req.get("timeout", DEFAULT_TIMEOUT))
        try:
            done = subprocess.run(
                argv, capture_output=True, timeout=timeout, check=False
            )
            code, out, err = done.returncode, done.stdout, done.stderr
        except subprocess.TimeoutExpired:
            # 504 rather than a 200 with a nonzero exit: a timeout is the agent failing to answer,
            # not the script reporting failure, and the host retries the two differently.
            return self._fail(504, f"timed out after {timeout}s")
        except OSError as e:
            return self._fail(500, f"could not run: {e}")

        self.send_response(200)
        self.send_header("Content-Type", "application/octet-stream")
        self.send_header("Content-Length", str(len(out)))
        self.send_header("X-Skein-Exit", str(code))
        # Base64 because stderr is arbitrary bytes and a header is not: a newline in it would end
        # the header block early and the rest would be read as the body.
        self.send_header(
            "X-Skein-Stderr", base64.b64encode(err[:MAX_STDERR]).decode("ascii")
        )
        self.end_headers()
        self.wfile.write(out)


def main():
    if len(sys.argv) < 3:
        sys.exit("usage: fleet-agent.py <port> <token-file>")
    port = int(sys.argv[1])
    with open(sys.argv[2]) as f:
        Handler.token = f.read().strip()
    if not Handler.token:
        sys.exit("refusing to serve with an empty token")

    # Loopback only. The port reaches the host through sbx's own publishing, which binds the host
    # side; binding 0.0.0.0 here would additionally expose command execution to anything that can
    # route to the sandbox, which is a different and much larger promise than the one being made.
    server = ThreadingHTTPServer(("127.0.0.1", port), Handler)
    server.daemon_threads = True
    print(f"skein-fleet-agent listening on 127.0.0.1:{port}", flush=True)
    server.serve_forever()


if __name__ == "__main__":
    main()
