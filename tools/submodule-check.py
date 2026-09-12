#!/usr/bin/env python3
"""Every submodule this repository declares is actually checked out, so the tests that read one run.

WHAT WENT WRONG, WHICH IS THE ONLY REASON THIS EXISTS. `upstream/sync` is a submodule, and two
library tests read it: they hold the vendored copies in `src/store/sync/` against upstream's
originals, because vendoring is the whole risk — a copy that silently falls behind teaches every box
a contract the gateway no longer honours, and nothing announces it.

In this fleet the submodule was never initialised. `git submodule status` answered
`-e113f18… upstream/sync`, the directory was empty, and both tests took their guard and returned.
A skipped test PASSES and `cargo test` captures a passing test's output, so `cargo test --lib` was
1051 passed / 0 failed while the two drift guards checked nothing — on every run, for as long as
anybody has been running them here (SKEIN-826).

`$SKEIN_TESTS_NO_SKIP` is what finally said so, because it turns a skip into a named failure. But
that switch is only ever set by one non-blocking CI step, so what it found sat as a pair of expected
failures instead of being fixed — and two failures everyone has learned to expect is how a switch
stops being run at all (SKEIN-647, one surface further on).

So the fix is not a louder skip. It is this gate: the drift guards' precondition, asserted where the
sixteen other gates are asserted, cheap enough to sit in front of `test`, and with a one-command
remedy in its own failure message. A contributor who clones without `--recursive` learns it here
rather than by reading a notice `cargo test` hides.

WHAT IT DOES NOT ASSERT, AND WHY. Not which commit the submodule sits at. The documented upgrade
flow is `git submodule update --remote`, then run the drift test, then vendor the new copies and
bump the pin in `src/store/sync/UPSTREAM.md` — and for that whole window the checkout is deliberately
ahead of the commit the index records. A gate that failed there would fail during the one procedure
it exists to support, and the content question is already answered, better, by the drift test itself:
it compares bytes. This gate answers only the question the drift test cannot ask about itself, which
is whether it ran.

REFUSES TO RUN RATHER THAN PASS QUIETLY, in both of its derivations. Neither the submodule list nor
the set of tests that depend on one is written down here:

  * the paths come from `.gitmodules`. Deriving none means the reader broke or submodules are gone,
    and either way this gate's verdict is worthless — exit 2, not 0.
  * the READERS come from the tree: the sites under `src/` and `tests/` that name a submodule's path.
    Deriving none for every submodule means nothing depends on a checkout any more, so demanding one
    would be a rule with no reason left — exit 2 again.

That shape is not decoration. CLAUDE.md's leak check answered `0` beside 195 matching processes
because it carried a list that had gone stale, and a check that cannot fail is worse than no check,
because it is trusted. `self_check()` therefore runs on every invocation, over real fixtures: a git
checkout it must accept, and the two shapes it must reject — an absent path, and a plain directory
of files sitting where a checkout belongs.

  python3 tools/submodule-check.py           check
  python3 tools/submodule-check.py --show    every submodule, its state, and who reads it
"""

import os
import re
import subprocess
import sys
import tempfile

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))

# Where a reader of a submodule would live. `src/` is where the two known ones are; `tests/` is
# included because an integration binary could grow one and would be just as silently skipped.
READER_DIRS = ("src", "tests")
READER_SUFFIXES = (".rs",)


def git(*args, cwd=ROOT):
    """Run git, returning (ok, stdout). Never raises — a failure is an answer here."""
    p = subprocess.run(
        ("git",) + args,
        cwd=cwd,
        stdout=subprocess.PIPE,
        stderr=subprocess.DEVNULL,
        text=True,
    )
    return p.returncode == 0, p.stdout.strip()


def declared_paths():
    """The submodule paths `.gitmodules` declares, in file order.

    Read through `git config -f` rather than parsed by hand, so that an unusual but valid
    `.gitmodules` cannot make this gate answer about a subset of the submodules it describes.
    """
    ok, out = git("config", "-f", ".gitmodules", "--get-regexp", r"^submodule\..+\.path$")
    if not ok:
        return []
    return [line.split(" ", 1)[1] for line in out.splitlines() if " " in line]


def is_checkout(path):
    """Is there a git checkout at `path`?

    This is the whole predicate, and it is deliberately about git rather than about files. An empty
    directory is what an uninitialised submodule leaves behind; a directory of hand-copied files is
    what somebody works around it with, and it is not a submodule — nothing updates it, and the pin
    in the index says nothing about what is in it.

    `--show-toplevel` AND NOT `--git-dir`, and the difference is the whole correctness of this gate.
    Git DISCOVERS a repository by walking up the directory tree, so `rev-parse --git-dir` inside an
    empty `upstream/sync` finds the SKEIN repository's own git dir and succeeds — which is to say the
    obvious spelling of this predicate answers "checked out" for an uninitialised submodule, the one
    case the gate exists for. Written that way first and caught by its own sabotage; the fixtures in
    `self_check` missed it because they sat in a temp directory outside any repository, where the
    walk up finds nothing. Same lesson as SKEIN-687: a predicate is only as good as the surface it is
    tried against, so those fixtures now live inside a real repository.

    Comparing the toplevel instead asks the question that was meant: is `path` the ROOT of a
    checkout, rather than merely somewhere inside one.
    """
    abs_path = path if os.path.isabs(path) else os.path.join(ROOT, path)
    if not os.path.isdir(abs_path):
        return False, "absent"
    ok, top = git("rev-parse", "--show-toplevel", cwd=abs_path)
    if not ok or os.path.realpath(top) != os.path.realpath(abs_path):
        if not os.listdir(abs_path):
            return False, "empty — the submodule was never initialised"
        return False, "a plain directory, not a checkout — nothing updates it"
    return True, "checked out"


def readers(paths):
    """Which source files name each submodule path, so a failure can say what breaks.

    Derived rather than listed for the reason in the docstring: a hand-kept list of the tests that
    depend on a submodule is exactly the thing that goes stale without saying so.
    """
    found = {p: [] for p in paths}
    for d in READER_DIRS:
        for dirpath, _, names in os.walk(os.path.join(ROOT, d)):
            for name in names:
                if not name.endswith(READER_SUFFIXES):
                    continue
                full = os.path.join(dirpath, name)
                rel = os.path.relpath(full, ROOT)
                try:
                    with open(full, encoding="utf-8", errors="replace") as fh:
                        lines = fh.read().splitlines()
                except OSError:
                    continue
                for n, line in enumerate(lines, 1):
                    for p in paths:
                        if p in line:
                            found[p].append(f"{rel}:{n}")
    return found


def recorded_sha(path):
    """The commit the index records for this submodule, for the report. Never an assertion."""
    ok, out = git("ls-files", "-s", "--", path)
    m = re.match(r"^160000 ([0-9a-f]{40})", out) if ok else None
    return m.group(1)[:12] if m else "?"


def self_check():
    """Prove the predicate can say no, on every run.

    Without this the gate is a green line whose ability to fail nobody has watched. Four fixtures,
    each one a shape seen in this repository: a real checkout, the empty directory an uninitialised
    submodule leaves, the hand-copied directory somebody replaces it with, and no directory at all.

    **THE FIXTURES SIT INSIDE A REPOSITORY, AND THAT IS THE LOAD-BEARING PART.** An earlier version
    of them did not, and they passed against an `is_checkout` that answered "checked out" for every
    empty directory in this tree — because git's discovery walk finds the enclosing repository, and
    outside one there is nothing to find. The fixture that cannot reproduce the surface cannot
    reproduce the bug, so the outer `git init` below is the fixture, not scaffolding.
    """
    with tempfile.TemporaryDirectory(prefix="skein-submodule-check-") as tmp:
        # The enclosing repository. Everything below is a path INSIDE it, exactly as a submodule
        # path is inside skein.
        ok, _ = git("init", "-q", tmp, cwd=tmp)
        if not ok:
            sys.stderr.write("submodule-check: `git init` failed, so self_check proves nothing\n")
            return False

        good = os.path.join(tmp, "checkout")
        os.makedirs(good)
        ok, _ = git("init", "-q", good, cwd=tmp)
        if not ok:
            sys.stderr.write("submodule-check: `git init` failed, so self_check proves nothing\n")
            return False

        empty = os.path.join(tmp, "empty")
        os.makedirs(empty)

        plain = os.path.join(tmp, "plain")
        os.makedirs(plain)
        with open(os.path.join(plain, "SKILL.md"), "w", encoding="utf-8") as fh:
            fh.write("hand-copied\n")

        missing = os.path.join(tmp, "missing")

        cases = [(good, True), (empty, False), (plain, False), (missing, False)]
        for path, want in cases:
            got, why = is_checkout(path)
            if got != want:
                sys.stderr.write(
                    f"submodule-check: self_check failed — {os.path.basename(path)} answered "
                    f"{got} ({why}), wanted {want}. The predicate below cannot be trusted.\n"
                )
                return False
    return True


def main():
    show = "--show" in sys.argv[1:]

    if not self_check():
        return 2

    paths = declared_paths()
    if not paths:
        sys.stderr.write(
            "submodule-check: derived no submodule paths from .gitmodules.\n"
            "    Either the reader broke or this repository has no submodules any more. Refusing to\n"
            "    answer rather than printing a pass nobody can distinguish from a real one.\n"
        )
        return 2

    who = readers(paths)
    if not any(who.values()):
        sys.stderr.write(
            f"submodule-check: nothing under {'/, '.join(READER_DIRS)}/ names any of the "
            f"{len(paths)} declared submodule path(s):\n"
            + "".join(f"      {p}\n" for p in paths)
            + "    So no test depends on a checkout, and requiring one is a rule with no reason\n"
            "    left. Drop the submodule, or this gate — but do not leave it passing vacuously.\n"
        )
        return 2

    problems = []
    for p in paths:
        ok, why = is_checkout(p)
        if show or not ok:
            mark = "ok " if ok else "NO "
            print(f"  {mark}{p} @ {recorded_sha(p)} — {why}, read by {len(who[p])} site(s)")
            if show:
                for site in who[p][:8]:
                    print(f"        {site}")
        if not ok:
            problems.append((p, why))

    if problems:
        print()
        for p, why in problems:
            sites = who[p]
            named = ", ".join(sites[:4]) + (" …" if len(sites) > 4 else "")
            print(
                f"submodule-check: {p} is {why}.\n"
                f"                 {len(sites)} site(s) read it, so what they check is not being "
                f"checked: {named}\n"
                "                 A test that reads an absent submodule takes its guard and "
                "returns, and a\n"
                "                 skipped test PASSES — which is why this is a gate and not a "
                "notice.\n"
                "                 Fix: git submodule update --init\n"
            )
        print(f"{len(problems)} of {len(paths)} declared submodule(s) not checked out.")
        return 1

    total = sum(len(v) for v in who.values())
    print(
        f"every declared submodule is checked out: {len(paths)} path(s) "
        f"({', '.join(paths)}), read from {total} site(s) under "
        f"{'/, '.join(READER_DIRS)}/"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
