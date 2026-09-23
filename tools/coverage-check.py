#!/usr/bin/env python3
"""Line coverage, measured, and held at or above a floor that only ever goes up.

WHAT IT MEASURES. Two numbers, each against its own floor in `docs/coverage-floor.toml`:

  * `rust` — line coverage of the workspace (both crates, `skein` and `warden/`) over the same
    test run the `test` gate makes: `cargo test --all --no-fail-fast`, run under `cargo llvm-cov`.
    Library, binaries and every `tests/*.rs` integration binary count, including what those
    binaries exercise by spawning `skein-server` and `skein`, because an instrumented binary
    writes its own profile wherever it is started from. Doctests do not count: collecting them
    needs a nightly compiler. The files counted are the ones under `src/` and `warden/src/` —
    the test harness in `tests/` is excluded, since a test covering itself is not coverage.
  * `cockpit` — line coverage of `cockpit/src`, from the same `node --test` run the
    `cockpit-tests` gate makes, with node's own `--experimental-test-coverage`.

WHY A FLOOR RATHER THAN A TARGET. The owner's standard: "ratchet from measured, no invented
target". The floor is the measured number rounded DOWN to a whole percent. It is not a goal; it is
a record of how much of the code a test already reaches, and a change that lowers that by a full
point fails. When the measured number rises by a point or more this says so and names the value to
write — raising it is a one-line change a person makes, never something this script commits.

REFUSES RATHER THAN PASSES QUIETLY. A report that counted no lines, a floor file that does not
parse, or a measurement that never produced a report is exit 2 with the reason — never a green,
because "I measured nothing" is not "coverage is fine" (the SKEIN-647 shape).

Usage:

    python3 tools/coverage-check.py rust                 # measure, then compare
    python3 tools/coverage-check.py rust --from F        # compare a `cargo llvm-cov --json` export
    python3 tools/coverage-check.py cockpit              # measure, then compare
    python3 tools/coverage-check.py cockpit --from F     # compare an lcov file node wrote

Exit codes: 0 at or above the floor, 1 below it, 2 no valid measurement (or called wrongly).
"""

import json
import math
import os
import subprocess
import sys
import tempfile
import tomllib

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
FLOOR_FILE = os.path.join(ROOT, "docs", "coverage-floor.toml")

# Handed to `cargo llvm-cov`: what is NOT the code under measurement. `tests/` is the harness.
# cargo-llvm-cov already leaves `tests/` out by default, along with the registry, the toolchain and
# the target directory; it is written here as well so that what is counted does not rest on a
# default a later release could change.
#
# `--workspace` (not `--all`, which means the same to `cargo test`) because it is also what picks
# the crates the REPORT covers: without it `warden/src` is compiled, tested, and then left out of
# the number, which is what a first measurement here did.
RUST_IGNORE = r"(^|/)tests/"

# The cockpit's test command, the `cockpit-tests` gate's own glob, plus coverage.
COCKPIT_INCLUDE = "cockpit/src/**"
COCKPIT_TESTS = "cockpit/test/*.test.mjs"


def refuse(msg):
    print(f"coverage-check: REFUSED — {msg}", file=sys.stderr)
    sys.exit(2)


def read_floor(kind):
    try:
        with open(FLOOR_FILE, "rb") as f:
            data = tomllib.load(f)
    except (OSError, tomllib.TOMLDecodeError) as e:
        refuse(f"cannot read {os.path.relpath(FLOOR_FILE, ROOT)}: {e}")
    floor = data.get(kind, {}).get("floor")
    if not isinstance(floor, int) or isinstance(floor, bool) or not 0 <= floor <= 100:
        refuse(
            f"{os.path.relpath(FLOOR_FILE, ROOT)} has no whole-number `floor` between 0 and 100 "
            f"under [{kind}] (found {floor!r})"
        )
    return floor


def rust_from_json(path):
    try:
        with open(path) as f:
            data = json.load(f)
        lines = data["data"][0]["totals"]["lines"]
        return lines["covered"], lines["count"]
    except (OSError, ValueError, KeyError, IndexError, TypeError) as e:
        refuse(f"{path} is not a `cargo llvm-cov --json` export: {e!r}")


def rust_measure(out):
    # `--ignore-run-fail`: a failing test is the `test` gate's red, and this job reports coverage of
    # the run that happened rather than no number at all. A run that broke wholesale reaches the
    # floor check with a number far below it, so it cannot pass by accident. It also runs every
    # binary past a failing one, which is `--no-fail-fast` — and cargo-llvm-cov refuses the two
    # together, so that flag, which the `test` gate passes, is absent here on purpose.
    cmd = [
        "cargo", "llvm-cov", "--workspace", "--ignore-run-fail",
        "--ignore-filename-regex", RUST_IGNORE,
        "--json", "--summary-only", "--output-path", out,
    ]
    print("coverage-check: " + " ".join(cmd), flush=True)
    status = subprocess.call(cmd, cwd=ROOT)
    if status != 0 or not os.path.exists(out):
        refuse(f"`cargo llvm-cov` exited {status} and produced no report")


def cockpit_from_lcov(path):
    found = hit = 0
    try:
        with open(path) as f:
            for line in f:
                if line.startswith("LF:"):
                    found += int(line[3:])
                elif line.startswith("LH:"):
                    hit += int(line[3:])
    except (OSError, ValueError) as e:
        refuse(f"{path} is not an lcov file: {e!r}")
    return hit, found


def cockpit_measure(out):
    cmd = [
        "node", "--test", "--experimental-test-coverage",
        f"--test-coverage-include={COCKPIT_INCLUDE}",
        "--test-reporter=spec", "--test-reporter-destination=stdout",
        "--test-reporter=lcov", f"--test-reporter-destination={out}",
        COCKPIT_TESTS,
    ]
    print("coverage-check: " + " ".join(cmd), flush=True)
    status = subprocess.call(cmd, cwd=ROOT)
    if status != 0:
        # Unlike the Rust side there is no partial report worth reading: node writes coverage only
        # for a run it finished, and a failing cockpit test is `cockpit-tests`' red already.
        refuse(f"`node --test` exited {status}; see `cockpit-tests` for the failure")


def verdict(kind, covered, count, floor):
    if count == 0:
        refuse(f"the {kind} report counted no lines at all, which is no measurement")
    pct = 100.0 * covered / count
    print(
        f"coverage-check: {kind} line coverage {pct:.2f}% ({covered}/{count} lines); floor {floor}%",
        flush=True,
    )
    if pct < floor:
        print(
            f"coverage-check: {kind} is BELOW THE FLOOR — {pct:.2f}% < {floor}%. This change leaves "
            f"{math.ceil(floor * count / 100) - covered} more line(s) untested than the floor allows. "
            f"Add tests for what it adds; the floor in docs/coverage-floor.toml only ever goes up.",
            file=sys.stderr,
        )
        return 1
    measured_floor = math.floor(pct)
    if measured_floor >= floor + 1:
        msg = (
            f"{kind} coverage rose to {pct:.2f}%, {measured_floor - floor} point(s) above the floor. "
            f"Raise `floor` under [{kind}] in docs/coverage-floor.toml to {measured_floor} to keep it."
        )
        print(f"coverage-check: {msg}")
        if os.environ.get("GITHUB_ACTIONS") == "true":
            print(f"::notice title=coverage floor can rise::{msg}")
    return 0


def main(argv):
    if len(argv) not in (1, 3) or argv[0] not in ("rust", "cockpit") or (
        len(argv) == 3 and argv[1] != "--from"
    ):
        print(__doc__.split("Usage:")[1].split("Exit codes")[0].rstrip(), file=sys.stderr)
        return 2
    kind = argv[0]
    floor = read_floor(kind)
    given = argv[2] if len(argv) == 3 else None
    with tempfile.TemporaryDirectory() as tmp:
        if kind == "rust":
            path = given or os.path.join(tmp, "llvm-cov.json")
            if not given:
                rust_measure(path)
            covered, count = rust_from_json(path)
        else:
            path = given or os.path.join(tmp, "cockpit.lcov")
            if not given:
                cockpit_measure(path)
            covered, count = cockpit_from_lcov(path)
    return verdict(kind, covered, count, floor)


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
