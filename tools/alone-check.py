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


def guard_line(output):
    """The one line worth quoting from a failure, if it names a reason we recognise.

    The guard's own sentence is checked against the WHOLE output rather than line by line: libtest
    prints `panicked at src/config.rs:49:5:` and the message on the lines after it, so a per-line
    scan finds the location first and reports a file:line where the reason was already available.
    """
    if "$SKEIN_HOME is unset in a test process" in output:
        return "it resolved `config::skein_home` with nothing pinned (SKEIN-626)"
    for line in output.splitlines():
        if "panicked at" in line:
            return line.strip()
    return ""


SELF_CHECK = r"""#!/usr/bin/env python3
# A stand-in for the lib test binary, in libtest's own `--list` / `--exact` spelling.
import os, sys
if "--list" in sys.argv:
    for n in ("hermetic", "leaky", "marked"):
        print("%s: test" % n)
    sys.exit(0)
which = sys.argv[sys.argv.index("--exact") + 1]
if which == "leaky":
    sys.exit(1)                    # the planted alone-failure the gate must name
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
    machinery here is exercised against a binary whose answers are known: three tests, one that
    fails alone and one that fails unless the environment was scrubbed.
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
        assert listed == ["hermetic", "leaky", "marked"], (
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
        if loud:
            print(
                "self-check: 3 fabricated tests, the planted alone-failure `leaky` was named, "
                "`marked` proves %s is set and %s are stripped from every child"
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
    listed, failed = sweep(binary, jobs=args.jobs)
    took = time.time() - started

    if not failed:
        print(
            "every one of the %d lib tests passes alone (%.0fs at --jobs %d)"
            % (len(listed), took, args.jobs)
        )
        return 0

    # Only now is the shared run worth its 70 seconds: it separates the class this gate is for —
    # green together, red alone — from an ordinary broken test, which `cargo test` already reports.
    together = passing_together(binary)
    leaning, broken = [], []
    for name in sorted(failed):
        (leaning if together and name in together else broken).append(name)

    for name in leaning:
        why = guard_line(failed[name])
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
