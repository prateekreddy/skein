#!/usr/bin/env python3
"""Every lib test has to pass on its own, in a process with no neighbours.

`cargo test` runs a crate's unit tests **multi-threaded in one process**, so what one test leaves in
that process is the next test's world. `env_lock()` and `tools/env-lock-check.py` stop two tests
*colliding*; nothing stops one test **depending** on what another left behind. That dependency is
invisible from a green suite, because the thing it rests on is right there in the process.

**The incident this exists for (SKEIN-646).** SKEIN-626 made `config::skein_home()` refuse an
unpinned test rather than answering with the real `~/.skein`, and pinned the 40 tests that then went
red. The suite was green. Run one test per process, and **fifteen more** failed — every one of them
resolving a path under the owner's live `~/.skein` and passing only because a neighbour had left
`$SKEIN_HOME` set. Two of the fifteen were containment tests
(`a_hostile_sha_cannot_escape_the_cache_directory`,
`a_token_file_is_named_so_a_repo_cannot_address_another_boxs_file`) computing their paths inside the
live directory. One of them, `after_login_clears_this_processes_refusal_and_says_what_the_share_did`,
ran the real login-share script against the real `/boxes` and reached into ten live boxes'
credential files — and it reached the arm it asserts only because *another* test removes `$HOME`
from the process, which makes that script's `set -u` abort.

So the rule this gate enforces is **hermeticity by isolation**, checked rather than declared: a test
that needs a neighbour is a test whose fixture is somebody else's leftovers, whatever it is about.

  python3 tools/alone-check.py              build if needed, then run every lib test alone
  python3 tools/alone-check.py --jobs 1     serially (the slow, certain reading)
  python3 tools/alone-check.py --bin PATH   against an already-built binary, skipping cargo
  python3 tools/alone-check.py --self-check only the self-check, and say what it proved

**It is not in `.github/workflows/ci.yml`.** See the RUNTIME note below and CONTRIBUTING.md.

## The environment is scrubbed, on purpose

`$SKEIN_TEST` is what `config::skein_home()`'s guard keys on, and cargo supplies it from
`.cargo/config.toml`'s `[env]` table — which a binary run directly does not get. So this sets it.
And it *removes* `$SKEIN_HOME` and `$SKEIN_FLEET_ROOT` from every child, because a developer who
exports either would otherwise be handing every test the very pin the gate is looking for the
absence of. A gate you can silence by exporting a variable is the shape this
repository keeps getting bitten by, so the self-check below proves the scrub happens.

## RUNTIME

Measured on the fleet box against `.target/debug`, 988 lib tests:

  · one process per test, default `--jobs 8`: **24s**
  · the same sweep at `--jobs 1`: **98s**
  · via `cargo test --lib --exact <name>` per name: seconds each, so tens of minutes — the reason
    this drives the compiled binary rather than cargo

The build is the same `cargo test --lib --no-run` the suite already needs, so on a machine that has
just run the tests this costs only the sweep. The shared-process run that classifies a finding
(green together, red alone) costs another ~70s and happens **only when something failed**.
"""

import argparse
import concurrent.futures
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import time

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))

# Set, because a binary run directly does not get cargo's `[env]` table — and the guard in
# `config::skein_home` keys on exactly this.
MARKER = "SKEIN_TEST"
# Removed from every child. Either of these, left in the ambient environment, answers for a test
# that should have pinned it for itself: `$SKEIN_FLEET_ROOT` unset falls back to the fleet this box
# is living in, so an unpinned test walks the real one.
#
# `SKEIN_IN_FLEET` was a third until SKEIN-643. It is still set in every skein box, and it is not
# scrubbed here any more because nothing reads it — scrubbing a variable no code consults asserts
# that it decides something, which is how it came to be guarded against in four places at once.
SCRUBBED = ("SKEIN_HOME", "SKEIN_FLEET_ROOT")


def child_env():
    env = dict(os.environ)
    env[MARKER] = "1"
    for key in SCRUBBED:
        env.pop(key, None)
    return env


def build():
    """The lib test binary's path, built if it is not current. Cargo's own JSON, not a glob.

    `ls -t .target/debug/deps/skein-*` is the obvious way and it is wrong twice over: a stale
    binary from a previous checkout sorts first if nothing recompiled, and `skein-<hash>` also
    matches the `skein-server` binaries' artifacts. Cargo says which file it just built, so ask it.
    """
    out = subprocess.run(
        ["cargo", "test", "--lib", "--no-run", "--message-format=json"],
        cwd=ROOT,
        env=child_env(),
        stdout=subprocess.PIPE,
        stderr=None,
        text=True,
    )
    if out.returncode != 0:
        sys.exit("alone-check: `cargo test --lib --no-run` failed; fix the build first")
    binary = None
    for line in out.stdout.splitlines():
        try:
            msg = json.loads(line)
        except ValueError:
            continue
        if msg.get("reason") == "compiler-artifact" and msg.get("executable"):
            if msg.get("target", {}).get("kind") == ["lib"]:
                binary = msg["executable"]
    if not binary:
        sys.exit("alone-check: cargo reported no lib test executable")
    return binary


def names(binary):
    """Every test the binary would run, in its own `--list` spelling."""
    out = subprocess.run(
        [binary, "--list"], env=child_env(), stdout=subprocess.PIPE, text=True
    )
    if out.returncode != 0:
        sys.exit("alone-check: `%s --list` failed" % binary)
    found = []
    for line in out.stdout.splitlines():
        line = line.strip()
        if line.endswith(": test"):
            found.append(line[: -len(": test")])
    return found


def run_one(binary, name):
    """(name, ok, output) for one test in a process of its own."""
    out = subprocess.run(
        [binary, "--exact", name],
        env=child_env(),
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
    )
    return name, out.returncode == 0, out.stdout


def sweep(binary, jobs):
    """{name: output} for every test that fails when it is the only one in its process."""
    todo = names(binary)
    if not todo:
        sys.exit("alone-check: the binary listed no tests, which cannot be right")
    failed = {}
    with concurrent.futures.ThreadPoolExecutor(max_workers=jobs) as pool:
        for name, ok, output in pool.map(lambda n: run_one(binary, n), todo):
            if not ok:
                failed[name] = output
    return todo, failed


def alone_again(binary, failed):
    """Split `failed` into (confirmed, crowded) by running each one AGAIN, on its own, serially.

    **The sweep's verdict is "failed while seven other test processes ran beside it", and that is
    not the claim this gate prints** (SKEIN-1018). The sweep runs one test per process, but `--jobs`
    of those processes at once — and on a box another lane is also building on, a test that fails
    under that load was reported as "fails alone", then passed alone four times out of four by
    hand. A red a reader cannot reproduce teaches them to read past the gate.

    A test that leans on a neighbour in its PROCESS — the class this gate exists for — fails
    alone every time, so one serial re-run confirms it. One that passes here failed for a reason
    that is not in-process state (load, or something on the filesystem a concurrent process
    touched), and it is reported as that, with its first output, rather than counted.
    """
    confirmed, crowded = {}, {}
    for name in sorted(failed):
        _, ok, output = run_one(binary, name)
        if ok:
            crowded[name] = failed[name]
        else:
            confirmed[name] = output
    return confirmed, crowded


def passing_together(binary):
    """The names that pass in the ordinary shared-process run — or None if it could not be read."""
    out = subprocess.run(
        [binary], env=child_env(), stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True
    )
    passed = set()
    for line in out.stdout.splitlines():
        line = line.strip()
        if line.startswith("test ") and line.endswith(" ... ok"):
            passed.add(line[len("test ") : -len(" ... ok")])
    return passed or None


# libtest's header for a panic: `thread '<name>' (<tid>) panicked at <file:line:col>:`, the message
# on the lines after it. The thread id in brackets is newer than some toolchains, so it is optional.
PANIC = re.compile(r"^thread '(?P<thread>[^']*)'(?: \(\d+\))? panicked at (?P<at>.*)$")
GUARD = "$SKEIN_HOME is unset in a test process"
GUARDED = "it resolved `config::skein_home` with nothing pinned (SKEIN-626)"


def panics(output):
    """[(thread, header, message lines)] for every panic in `output`, in the order printed."""
    lines = output.splitlines()
    found = []
    for i, line in enumerate(lines):
        m = PANIC.match(line.strip())
        if not m:
            continue
        body = []
        for after in lines[i + 1 :]:
            if not after.strip() or PANIC.match(after.strip()) or after.startswith("note: run with"):
                break
            body.append(after.strip())
        found.append((m.group("thread"), line.strip(), body))
    return found


def guard_line(output, name=None):
    """The one line worth quoting from a failure, if it names a reason we recognise.

    **The test's own panic first — the one on the thread libtest named after the test** (SKEIN-1122).
    A thread the test spawned inherits libtest's output capture, so its panic is printed with the
    test's output whenever the test fails, for whatever reason. This used to return the SKEIN-626
    sentence whenever it appeared ANYWHERE in the output, and that is how SKEIN-658 was misread: a
    500ms timing assertion failed under load, a detached version-check thread had hit the guard
    beside it, and the guard was quoted as the reason. The guard is still the reason when it is on
    the test's own thread, and a guard on another thread is mentioned, as the second thing it is.

    The message is read from the lines AFTER the header: libtest prints `panicked at
    src/config.rs:49:5:` and the message below it, so a per-line scan for the sentence would find
    the location first and report a file:line where the reason was already available.

    With no panic on the test's own thread — a test that exited, or a name that is not a thread's —
    it falls back to what it did before: the guard if it is anywhere, else the first panic line.
    """
    found = panics(output)
    own = [p for p in found if name is not None and p[0] == name]
    if own:
        _, header, body = own[0]
        if any(GUARD in line for line in body):
            return GUARDED
        said = header + (" " + body[0] if body else "")
        if any(GUARD in line for thread, _, lines in found if thread != name for line in lines):
            said += (
                "\n             (a thread it spawned hit the SKEIN-626 guard as well — printed "
                "with this test's output, and not why it failed)"
            )
        return said
    if GUARD in output:
        return GUARDED
    for line in output.splitlines():
        if "panicked at" in line:
            return line.strip()
    return ""


SELF_CHECK = r"""#!/usr/bin/env python3
# A stand-in for the lib test binary, in libtest's own `--list` / `--exact` spelling.
import os, sys
if "--list" in sys.argv:
    for n in ("hermetic", "leaky", "marked", "guarded", "crowded"):
        print("%s: test" % n)
    sys.exit(0)
which = sys.argv[sys.argv.index("--exact") + 1]
if which == "leaky":
    sys.exit(1)                    # the planted alone-failure the gate must name
if which == "crowded":
    # Fails its first run and passes every run after: a test that went red beside its neighbours
    # and is green on its own, which the gate must not call a failure alone (SKEIN-1018).
    seen = os.path.join(os.path.dirname(os.path.abspath(sys.argv[0])), "crowded-ran")
    if os.path.exists(seen):
        sys.exit(0)
    open(seen, "w").close()
    print("thread 'crowded' (9) panicked at src/probes.rs:1319:9:")
    print("a newly enabled plugin never reached an existing box")
    sys.exit(101)
if which == "guarded":
    # A test failing its OWN assertion while a thread it spawned hit the SKEIN-626 guard, in
    # libtest's spelling (rustc 1.98): the helper's panic is printed first, with this test's output.
    print('''running 1 test
test guarded ... FAILED

failures:

---- guarded stdout ----

thread '<unnamed>' (1572613) panicked at src/config.rs:49:5:
$SKEIN_HOME is unset in a test process — pin it
note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace

thread 'guarded' (1572612) panicked at src/fleet/substrate.rs:600:9:
the update offer took 612ms to answer

failures:
    guarded
''')
    sys.exit(101)
if which == "marked":
    # Fails unless the gate set the marker AND took the ambient pins away. The runner exports both
    # before calling, so a gate that merely inherited its environment fails here.
    ok = os.environ.get("SKEIN_TEST") == "1" and not any(
        k in os.environ for k in ("SKEIN_HOME", "SKEIN_FLEET_ROOT")
    )
    sys.exit(0 if ok else 1)
sys.exit(0)
"""


def self_check(loud=False):
    """Prove the mechanism on a fabricated binary, on every invocation.

    A gate nobody has watched fail is the thing this repository keeps getting bitten by — the
    browser tier skipped on every green CI run in its history, and the leaked-process check answers
    `0` while nine-hour-old servers run, because its pattern does not match their names. So the
    machinery here is exercised against a binary whose answers are known: four tests, one that
    fails alone, one that fails unless the environment was scrubbed, one that fails its own
    assertion beside a helper thread's guard panic (SKEIN-1122), and one that fails only on its
    first run, the way a test does beside a loaded neighbour (SKEIN-1018).
    """
    tmp = tempfile.mkdtemp(prefix="alone-check-self-")
    try:
        fake = os.path.join(tmp, "fake-test-binary")
        with open(fake, "w", encoding="utf-8") as f:
            f.write(SELF_CHECK)
        os.chmod(fake, 0o755)
        # Dirty the ambient environment with exactly what the gate must strip.
        was = {k: os.environ.get(k) for k in SCRUBBED}
        for k in SCRUBBED:
            os.environ[k] = "/the-ambient-answer-a-test-must-not-be-given"
        try:
            listed, failed = sweep(fake, jobs=2)
        finally:
            for k, v in was.items():
                if v is None:
                    os.environ.pop(k, None)
                else:
                    os.environ[k] = v
        confirmed, crowded = alone_again(fake, failed)
        assert listed == ["hermetic", "leaky", "marked", "guarded", "crowded"], (
            "alone-check self-check: --list was misread (%r)" % (listed,)
        )
        assert "leaky" in failed, (
            "alone-check self-check: the planted alone-failure was NOT reported — this gate cannot "
            "fail, and a gate that cannot fail is worse than none"
        )
        assert "marked" not in failed, (
            "alone-check self-check: the child environment was not scrubbed — `%s` was missing or "
            "an ambient pin survived, so every real test would be answered by the environment "
            "instead of by its own fixture" % MARKER
        )
        assert "hermetic" not in failed, (
            "alone-check self-check: a passing test was reported as failing"
        )
        # The re-run: `leaky` fails alone every time and stays a finding; `crowded` failed once and
        # passes on its own, so it must be reported as that and not as a failure alone.
        assert "leaky" in confirmed, (
            "alone-check self-check: the serial re-run lost the planted alone-failure — a "
            "confirmation step that clears real findings is this gate unable to fail again"
        )
        assert "crowded" in failed and "crowded" in crowded and "crowded" not in confirmed, (
            "alone-check self-check: a test that failed beside its neighbours and passed on its "
            "own was reported as failing alone (SKEIN-1018): confirmed=%r crowded=%r"
            % (sorted(confirmed), sorted(crowded))
        )
        # The planted SKEIN-658 shape: the reason is the test's own assertion, whatever a thread
        # it spawned said first (SKEIN-1122).
        said = guard_line(failed.get("guarded", ""), "guarded")
        assert said.startswith("thread 'guarded'") and "took 612ms" in said, (
            "alone-check self-check: a test that failed its own assertion was reported as %r — "
            "a helper thread's panic was quoted as the reason, which is how SKEIN-658 was misread"
            % said
        )
        # And the guard is still the reason when it is on the test's own thread.
        own = "thread 'guarded' (7) panicked at src/config.rs:49:5:\n%s — pin it\n" % GUARD
        assert guard_line(own, "guarded") == GUARDED, (
            "alone-check self-check: the SKEIN-626 guard on the test's own thread was not named"
        )
        if loud:
            print(
                "self-check: 5 fabricated tests, the planted alone-failure `leaky` was named and "
                "confirmed on a serial re-run, `marked` proves %s is set and %s are stripped from "
                "every child, `guarded` is reported with its own assertion rather than a helper "
                "thread's guard, and `crowded`, red once and green on its own, is not called a "
                "failure alone"
                % (MARKER, "/".join(SCRUBBED))
            )
    finally:
        shutil.rmtree(tmp, ignore_errors=True)


def main():
    ap = argparse.ArgumentParser(add_help=True)
    ap.add_argument("--jobs", type=int, default=min(8, (os.cpu_count() or 2)))
    ap.add_argument("--bin", default=None, help="an already-built lib test binary")
    ap.add_argument("--self-check", action="store_true", help="only the self-check")
    args = ap.parse_args()

    self_check(loud=args.self_check)
    if args.self_check:
        return 0

    binary = args.bin or build()
    started = time.time()
    listed, swept = sweep(binary, jobs=args.jobs)
    took = time.time() - started
    failed, crowded = alone_again(binary, swept)

    for name in sorted(crowded):
        why = guard_line(crowded[name], name)
        print(
            "alone-check: `%s` failed beside the sweep's other processes (--jobs %d) and passed "
            "when run again on its own — not a failure alone, so not counted here. Whatever it "
            "shares with a concurrent process is on the filesystem or the box, not in its own "
            "process." % (name, args.jobs)
        )
        if why:
            print("             the first run said: %s" % why)
        print()

    if not failed:
        print(
            "every one of the %d lib tests passes alone (%.0fs at --jobs %d%s)"
            % (
                len(listed),
                took,
                args.jobs,
                ", %d of them only on a serial re-run" % len(crowded) if crowded else "",
            )
        )
        return 0

    # Only now is the shared run worth its 70 seconds: it separates the class this gate is for —
    # green together, red alone — from an ordinary broken test, which `cargo test` already reports.
    together = passing_together(binary)
    leaning, broken = [], []
    for name in sorted(failed):
        (leaning if together and name in together else broken).append(name)

    for name in leaning:
        why = guard_line(failed[name], name)
        print("alone-check: `%s` passes in the suite and fails alone" % name)
        if why:
            print("             %s" % why)
        print(
            "             rule: a test's fixture is its own. Pin what it resolves — "
            "`crate::testutil::env_lock()`, a `tempdir()`, `$SKEIN_HOME` (and `$SKEIN_FLEET_ROOT` "
            "if it reaches a fleet path) — and remove them at the end.\n"
        )
    for name in broken:
        print(
            "alone-check: `%s` fails alone AND in the suite — an ordinary red test, which "
            "`cargo test` reports too\n" % name
        )
    print(
        "%d of %d lib tests fail alone, %d of them green in the suite. %.0fs at --jobs %d."
        % (len(failed), len(listed), len(leaning), took, args.jobs)
    )
    return 1


if __name__ == "__main__":
    sys.exit(main())
