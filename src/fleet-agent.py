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

Two endpoints, because skein sends a box two shapes of thing:

- `POST /exec` — a script, its stdout back. The small, frequent calls whose availability matters:
  liveness, resources, disk, probes.
- `POST /write` — a script with a **body on its stdin**: an installed file, a credential, a pasted
  screenshot, a dropped 800 MB video. Streamed chunk by chunk into the child rather than read into
  memory first, so the size of the thing being written is the box's problem and never the agent's.

`/write` exists because leaving it out left every upload on `sbx exec` — which is the call that
hangs during exactly the stall this agent was built to survive.
"""

import base64
import hmac
import json
import os
import subprocess
import sys
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

# What this agent can do, announced by `/health` so the host can tell an agent it installed from one
# an older skein left running. The agent lives in the sandbox and outlives the skein that put it
# there, so "is it up" and "does it speak what I am about to send" are separate questions — and the
# second one has to be answerable *before* a gigabyte goes down the wire, since there is no way back
# once it has. Bump this whenever an endpoint or its framing changes.
PROTOCOL = 4

# Bigger than any script skein sends, small enough that a stray POST cannot exhaust the sandbox.
MAX_BODY = 1 << 20
# The ceiling on a streamed write. Nothing is buffered at this size — the body goes through in 64 KB
# pieces — so this is not a memory limit but a deliberate one: the box's disk is finite, and a
# runaway upload should stop at a number someone chose rather than when the sandbox fills up.
MAX_WRITE = 1 << 30
# stderr rides back in a header, so it is capped rather than truncated silently at the HTTP layer:
# a header a proxy refuses would lose the error text that is the whole point of returning it.
MAX_STDERR = 8192
DEFAULT_TIMEOUT = 30.0

# How much of a killed script's own output travels back with the timeout. Enough to name the stage it
# died in, short enough to stay a message rather than a log — the box keeps the whole thing.
TIMEOUT_TAIL = 1200


def _tail(*streams):
    """The last of whatever the script managed to print, for a message about how far it got."""
    said = "\n".join(
        (s.decode("utf-8", "replace") if isinstance(s, bytes) else str(s)).strip()
        for s in streams
        if s
    ).strip()
    if not said:
        # Said explicitly. "Nothing printed" and "we did not look" are different diagnoses, and the
        # second is the one that sent somebody reading the script line by line.
        return " — and it had printed nothing"
    return " — it had got as far as:\n" + said[-TIMEOUT_TAIL:]

# ── the Docker daemon's watchdog ──────────────────────────────────────────────────────────────────
#
# `dockerd` runs as pid 16 with pid 1 for a parent and **nothing supervising it**. A container that
# takes the sandbox to its memory ceiling can get the daemon killed, and when that happens it stays
# dead until somebody rebuilds the fleet — which is a restart of everything to recover one process.
#
# This is the same shape as the `while true` loop that supervises *this* agent, one layer in: the
# thing that must come back is watched by something that is already here for availability reasons.
#
# It deliberately does not diagnose. Why the daemon died is the pressure counters' business; this
# only makes its death a blip.
DOCKER_POLL = 5.0
# How long to wait, after noticing it is gone, before doing anything. `PPID 1` does not say whether
# init spawned dockerd or merely reaped it, so there may be a supervisor here that this one has
# never seen — and **two dockerds is a worse failure than none**. Long enough for anything else to
# win the race, short enough that a fleet is not stuck for a minute.
DOCKER_GRACE = 20.0
# A daemon that will not start must not be respawned every twenty seconds for ever. Doubling from
# the grace, capped, so a broken dockerd costs one attempt every five minutes rather than a busy
# loop on a sandbox that is already unwell.
DOCKER_BACKOFF_MAX = 300.0
# What the daemon's processes are worth to the global OOM killer, relative to everything else.
#
# **Not `-1000`.** That makes a process OOM-immune, and an immune dockerd on a sandbox with nothing
# left to kill is a wedged machine rather than a survivable one. `-500` halves its badness: the
# kernel looks at the container that caused the pressure first, and only comes back here if there is
# genuinely nothing else — which is the honest ordering.
#
# Per process, so it is re-applied on every pass rather than set once: a restarted dockerd is a new
# pid, and a guarantee that does not survive the restart it exists for is not one.
DOCKER_OOM_SCORE = -500
# The daemon is two processes, and killing either one ends the same way.
DOCKER_PROCESSES = ("dockerd", "containerd")


def _argv(req):
    """Run what the host sent, and nothing more.

    **This agent does not build namespace hops, and that is deliberate.** It used to: the host sent
    `ns_pid`, `home` and `tree`, and the `nsenter` was assembled here, so that a host newer than
    this agent could not hand it a command *shape* it did not understand. That reasoning was sound
    for shapes and wrong for checks.

    A box is addressed by a pid, and a pid is only an identity together with the boot it belongs to
    and the process start time — otherwise a cycled sandbox or a reused pid means the address now
    names some other box. An agent that predates that check would have ignored the fields carrying
    the proof and crossed anyway, and the host would have had no way to know which kind of agent it
    was talking to. So the crossing arrives as a script with its own check in front of it, which an
    agent of any age runs correctly *or not at all*.
    """
    return ["bash", "-lc", req["script"]]


class Handler(BaseHTTPRequestHandler):
    # Set by main(); a class attribute so every thread shares the one comparison value.
    token = ""

    # HTTP/1.1, and this line is the whole transport. `BaseHTTPRequestHandler` defaults to HTTP/1.0,
    # where `parse_request` leaves `close_connection` true no matter what the client asks for — so
    # the server hung up after every single response and the host's "held" connection was silently
    # reopened each time. That is precisely the behaviour this agent exists to avoid: a new
    # connection per call is a new channel per call, and new channels are what the stall blocks.
    # The client never noticed because it reconnects on a dead socket by design.
    protocol_version = "HTTP/1.1"

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
        # What the host can ask about the machine itself. Behind the token, unlike `/health`: this is
        # fleet state rather than "is anybody there".
        if self.path == "/machine":
            if not self._authed():
                return self._fail(403, "bad token")
            watch = getattr(self, "docker", None)
            body = json.dumps(
                {
                    "docker": watch.snapshot() if watch else None,
                    "pressure": counters(),
                }
            ).encode()
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)
            return
        # Unauthenticated on purpose and answers nothing but its own name: this is what the host
        # probes to decide whether the agent is worth using, and that decision must not depend on
        # the token being current.
        if self.path == "/health":
            # The version rides along with the name. An older agent answers the name alone, which
            # the host reads as protocol 1 — so "no version" is a version, not a parse failure.
            self._fail(200, f"skein-fleet-agent {PROTOCOL}")
        else:
            self._fail(404, "no")

    def do_POST(self):
        if self.path == "/exec":
            return self._exec()
        if self.path == "/write":
            return self._write()
        return self._fail(404, "no")

    def _exec(self):
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
        except subprocess.TimeoutExpired as e:
            # 504 rather than a 200 with a nonzero exit: a timeout is the agent failing to answer,
            # not the script reporting failure, and the host retries the two differently.
            #
            # **With what the script had already said.** `TimeoutExpired` carries the output captured
            # before the kill, and throwing it away made a provisioning timeout unreadable: the host
            # saw "the command did not finish in time" and nothing else, while the script had been
            # printing a line at every stage it passed. Diagnosing one took reading the script end to
            # end and querying the cockpit's API for corroboration. The tail, because the answer to
            # "how far did it get" is always at the end.
            return self._fail(
                504, f"timed out after {timeout}s{_tail(e.stdout, e.stderr)}"
            )
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

    def _body_pieces(self):
        """Yield the request body in pieces, understanding both framings.

        Chunked is not optional here. An upload is streamed from the browser, through skein, into
        the box, and skein does not always know how many bytes are coming when it starts — it must
        not have to buffer a video on the host just to fill in a `Content-Length`.
        """
        if "chunked" in self.headers.get("Transfer-Encoding", "").lower():
            while True:
                line = self.rfile.readline(80)
                if not line:
                    raise IOError("closed before the chunk size")
                size = int(line.split(b";")[0].strip() or b"0", 16)
                if size == 0:
                    # Trailers. skein sends none, but a body that ends without consuming them
                    # leaves the connection framed wrong for whatever comes next.
                    while self.rfile.readline(1024) not in (b"\r\n", b"\n", b""):
                        pass
                    return
                left = size
                while left > 0:
                    piece = self.rfile.read(min(left, 1 << 16))
                    if not piece:
                        raise IOError("closed mid-chunk")
                    left -= len(piece)
                    yield piece
                self.rfile.read(2)  # the CRLF that ends the chunk
        else:
            left = int(self.headers.get("Content-Length", "0"))
            while left > 0:
                piece = self.rfile.read(min(left, 1 << 16))
                if not piece:
                    raise IOError("closed mid-body")
                left -= len(piece)
                yield piece

    def _write(self):
        """Run a script with the request body on its stdin — `sbx exec -i`, without the `sbx exec`.

        The placement travels in a header rather than the body for the obvious reason: the body is
        the payload, and it may be a gigabyte of video. Nothing here holds more than one 64 KB piece
        at a time, on either side of the child.
        """
        if not self._authed():
            return self._fail(403, "bad token")
        try:
            req = json.loads(base64.b64decode(self.headers.get("X-Skein-Meta", "")))
            argv = _argv(req)
            timeout = float(req.get("timeout", DEFAULT_TIMEOUT))
        except (ValueError, KeyError, TypeError) as e:
            return self._fail(400, f"bad request: {e}")
        deadline = time.monotonic() + timeout

        try:
            child = subprocess.Popen(
                argv,
                stdin=subprocess.PIPE,
                # Nothing reads stdout, and an unread pipe stops the child once its buffer fills —
                # which for a write would look exactly like a stalled upload.
                stdout=subprocess.DEVNULL,
                stderr=subprocess.PIPE,
            )
        except OSError as e:
            return self._fail(500, f"could not run: {e}")

        # stderr drained on a thread for the same reason, and it is the *only* thing that will say
        # why a write failed: "No space left on device" arrives here or nowhere.
        collected, held = [], [0]

        def drain():
            while True:
                piece = child.stderr.read1(4096)
                if not piece:
                    return
                if held[0] < MAX_STDERR:
                    held[0] += len(piece)
                    collected.append(piece)

        pump = threading.Thread(target=drain, daemon=True)
        pump.start()

        total, refused, sink = 0, None, child.stdin
        try:
            for piece in self._body_pieces():
                total += len(piece)
                if total > MAX_WRITE:
                    refused = f"body exceeds {MAX_WRITE} bytes"
                    break
                if sink is None:
                    continue
                try:
                    sink.write(piece)
                except BrokenPipeError:
                    # The child is gone — it could not open the path, or exited early. Its own
                    # words explain why and they arrive below, so this is not the error to report.
                    #
                    # But the body keeps being read, and that is the load-bearing half: a sender
                    # halfway through an upload blocks the moment nobody drains its socket, and it
                    # would sit there until its deadline for a write that had already failed.
                    sink = None
        except (IOError, OSError, ValueError) as e:
            refused = f"reading the body: {e}"
        try:
            child.stdin.close()
        except OSError:
            pass

        # A truncated or over-long body leaves the connection framed mid-message, so it cannot be
        # reused whatever the host intends.
        if refused:
            self.close_connection = True
            child.kill()
            child.wait()
            return self._fail(400, refused)

        try:
            code = child.wait(timeout=max(1.0, deadline - time.monotonic()))
        except subprocess.TimeoutExpired:
            child.kill()
            child.wait()
            return self._fail(504, f"timed out after {timeout}s")
        pump.join(timeout=5)

        self.send_response(200)
        self.send_header("Content-Length", "0")
        self.send_header("X-Skein-Exit", str(code))
        self.send_header(
            "X-Skein-Stderr",
            base64.b64encode(b"".join(collected)[:MAX_STDERR]).decode("ascii"),
        )
        self.end_headers()


# The cgroups whose pressure is worth reporting, and what each one being squeezed would mean.
#
#   skein             the whole workload's ceiling: the boxes and the containers they start
#   skein/containers  a runaway container, once it has a ceiling of its own to hit
#   docker            the daemon itself, which is uncapped — so only its kills matter here
WATCHED = ("skein", "skein/containers", "docker")


def counters(cgroup="/sys/fs/cgroup", vmstat="/proc/vmstat"):
    """What the kernel says about memory pressure, read straight out of the filesystem.

    **Free, and that is the property that matters.** These are file reads with no subprocess, which
    is what lets skein ask for them without the board's tick paying for it — see
    `tests/board_cost.rs`, which measures exactly that.

    Everything here is a counter **since this boot**. Nothing is interpreted: a rate is the host's
    business, because only the host knows when it last looked.
    """
    out = {}
    for name in WATCHED:
        here = {}
        base = os.path.join(cgroup, name)
        try:
            for line in open(os.path.join(base, "memory.events")):
                key, _, value = line.partition(" ")
                here[key] = int(value)
        except (OSError, ValueError):
            continue  # a fleet without this cgroup is not a fleet with a problem to report
        for field in ("memory.current", "memory.max"):
            try:
                here[field] = open(os.path.join(base, field)).read().strip()
            except OSError:
                pass
        out[name] = here
    # The kernel's own tally, which is what says whether anything was killed OUTSIDE the cgroups
    # skein wrote — the case that took the daemon down and left no trace in any of them.
    try:
        for line in open(vmstat):
            if line.startswith("oom_kill "):
                out["vmstat_oom_kill"] = int(line.split()[1])
                break
    except (OSError, ValueError):
        pass
    return out


def dockerd_pid(proc="/proc"):
    """The running dockerd's pid, or `None`.

    Read from `/proc` rather than from `/run/docker.pid`, because the pidfile is exactly what a
    crash leaves behind: a number naming a process that is gone, or worse, one since reused. `comm`
    is the kernel's own answer to "what is this process", and it cannot be argued with.
    """
    try:
        entries = os.listdir(proc)
    except OSError:
        return None
    for entry in entries:
        if not entry.isdigit():
            continue
        try:
            with open(os.path.join(proc, entry, "comm")) as f:
                if f.read().strip() == "dockerd":
                    return int(entry)
        except OSError:
            continue  # it exited between the listing and the read, which is ordinary
    return None


def pids_named(names, proc="/proc"):
    """Every pid whose `comm` is one of `names`."""
    found = []
    try:
        entries = os.listdir(proc)
    except OSError:
        return found
    for entry in entries:
        if not entry.isdigit():
            continue
        try:
            with open(os.path.join(proc, entry, "comm")) as f:
                if f.read().strip() in names:
                    found.append(int(entry))
        except OSError:
            continue
    return found


def shield(pid, score=DOCKER_OOM_SCORE, proc="/proc"):
    """Make the global OOM killer look elsewhere first. `True` if it now reads what was asked.

    Lowering `oom_score_adj` needs privilege, so this falls back to `sudo` exactly as the restart
    does — and reads the value back rather than trusting the write, because the failure is silent:
    a refused write and a successful one both return without complaint through a shell.
    """
    path = os.path.join(proc, str(pid), "oom_score_adj")
    try:
        with open(path) as f:
            if int(f.read().strip()) <= score:
                return True  # already shielded, by us or by whoever started it
    except (OSError, ValueError):
        return False
    try:
        with open(path, "w") as f:
            f.write(str(score))
    except OSError:
        try:
            subprocess.run(
                ["sudo", "-n", "sh", "-c", f'echo {score} > {path}'],
                check=False,
                stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL,
                timeout=10,
            )
        except Exception:  # noqa: BLE001 — a shield that cannot be raised is reported, not fatal
            return False
    try:
        with open(path) as f:
            return int(f.read().strip()) <= score
    except (OSError, ValueError):
        return False


def dockerd_argv(pid, proc="/proc"):
    """The command line dockerd was started with, while there is still a dockerd to ask.

    This is the whole reason the watchdog polls rather than waiting to be told: `/proc/<pid>/cmdline`
    is readable now and gone the moment it matters. Remembering it while the daemon is healthy is
    what makes bringing it back an option — and not remembering it is a fact to report rather than a
    reason to guess at a command line.
    """
    try:
        with open(os.path.join(proc, str(pid), "cmdline"), "rb") as f:
            raw = f.read()
    except OSError:
        return None
    argv = [part.decode("utf-8", "replace") for part in raw.split(b"\0") if part]
    return argv or None


class DockerWatch:
    """Watch the daemon, remember how it was started, and bring it back when nothing else does.

    Every collaborator is injected so the behaviour can be driven in a test: finding the daemon,
    reading its argv, spawning it, the clock and the sleep. What is left in `look()` is the decision,
    which is the part worth asserting.
    """

    def __init__(self, find=dockerd_pid, argv_of=dockerd_argv, spawn=None, now=time.time,
                 sleep=time.sleep, log=None, kin=pids_named, shield=shield):
        self._find = find
        self._argv_of = argv_of
        self._spawn = spawn or self._start
        self._now = now
        self._sleep = sleep
        self._log = log or (lambda line: print(line, flush=True))
        self._kin = kin
        self._shield = shield
        self.shielded = 0
        self.argv = None
        self.restarts = 0
        self.last_restart = ""
        self.note = "watching"
        self.backoff = DOCKER_GRACE
        self._complained = False

    def _start(self, argv):
        """Spawn it detached, so it outlives this agent exactly as the original did.

        `sudo` only when this agent is not already root — dockerd needs root, and asking for it when
        it is already held would fail on a sandbox with no sudo rather than work.
        """
        command = list(argv) if os.geteuid() == 0 else ["sudo", "-n", *argv]
        subprocess.Popen(  # noqa: S603 — the argv is the daemon's own, read from /proc
            command,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            start_new_session=True,
        )

    def snapshot(self):
        """What the host can read. Small on purpose — this is a fact, not a diagnosis."""
        return {
            "pid": self._find(),
            "restarts": self.restarts,
            "last_restart": self.last_restart,
            "argv_known": bool(self.argv),
            "shielded": self.shielded,
            "note": self.note,
        }

    def look(self):
        """One pass. Returns the word for what it did, which is what a test asserts on."""
        pid = self._find()
        if pid:
            # Refreshed rather than remembered once: if something else restarts dockerd with
            # different arguments, the truth is whatever is running now.
            argv = self._argv_of(pid)
            if argv:
                self.argv = argv
            self.note = "watching"
            self.backoff = DOCKER_GRACE
            self._complained = False
            # Every pass, not once. A restarted daemon is a new pid, and a shield that does not
            # survive the restart it exists for is not one. Cheap: two `/proc` reads that return
            # early once the value is already what was asked for.
            self.shielded = sum(1 for p in self._kin(DOCKER_PROCESSES) if self._shield(p))
            return "alive"

        if not self.argv:
            # Nothing to bring back. Said once, because a line every five seconds is noise and the
            # condition does not change on its own.
            if not self._complained:
                self._complained = True
                self._log(
                    "skein-fleet-agent: dockerd is not running and this agent never saw it, so it "
                    "has no command line to restart it with. Rebuild or restart the fleet."
                )
            self.note = "gone, and no command line was ever seen"
            return "unknown"

        # The grace. Anything else that supervises this daemon gets to win, because two dockerds is
        # a worse failure than none — and a `PPID 1` says nothing about whether such a thing exists.
        self._sleep(self.backoff)
        if self._find():
            self.note = "something else restarted it"
            return "recovered"

        try:
            self._spawn(self.argv)
        except Exception as e:  # noqa: BLE001 — a watchdog that dies of a failed restart is no watchdog
            self.note = f"restart failed: {e}"
            self._log(f"skein-fleet-agent: could not restart dockerd: {e}")
            self.backoff = min(self.backoff * 2, DOCKER_BACKOFF_MAX)
            return "failed"

        self.restarts += 1
        self.last_restart = time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime(self._now()))
        self.note = "restarted by this agent"
        self._log(f"skein-fleet-agent: dockerd was gone; restarted it ({self.restarts} so far)")
        # Backed off even on success: a daemon that starts and dies immediately would otherwise be
        # respawned every grace period for ever, which is the busy loop this exists to avoid.
        self.backoff = min(self.backoff * 2, DOCKER_BACKOFF_MAX)
        return "restarted"

    def forever(self):
        while True:
            try:
                self.look()
            except Exception as e:  # noqa: BLE001
                self._log(f"skein-fleet-agent: the docker watchdog stumbled: {e}")
            self._sleep(DOCKER_POLL)


def main():
    if len(sys.argv) < 3:
        sys.exit("usage: fleet-agent.py <port> <token-file>")
    port = int(sys.argv[1])
    with open(sys.argv[2]) as f:
        Handler.token = f.read().strip()
    if not Handler.token:
        sys.exit("refusing to serve with an empty token")

    # All interfaces, and it has to be. sbx forwards a published port to the sandbox's *routable*
    # address the way Docker does, not to its loopback — so a server bound to 127.0.0.1 accepts
    # nothing through the mapping. Measured: three published ports in a row reported success and
    # refused every connection, while `/proc/net/tcp` showed this socket as `0100007F:207D`.
    #
    # What that widens: any box in the fleet can now reach this port. That is not a new capability —
    # boxes already share this network namespace, and any box can already drive any other through
    # the host cockpit — and it is inside the stated boundary, which puts no wall between boxes. The
    # token is what stands between reaching the port and using it, so it stays the only guard that
    # matters and must never be weakened to compensate for the bind address.
    # The watchdog before the server, so a daemon that is already down is noticed while the agent is
    # still starting rather than after the first request that needed it.
    Handler.docker = DockerWatch()
    threading.Thread(target=Handler.docker.forever, daemon=True).start()

    server = ThreadingHTTPServer(("0.0.0.0", port), Handler)
    server.daemon_threads = True
    print(f"skein-fleet-agent {PROTOCOL} listening on 0.0.0.0:{port}", flush=True)
    server.serve_forever()


if __name__ == "__main__":
    main()
