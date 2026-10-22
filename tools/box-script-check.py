#!/usr/bin/env python3
"""One rule about a test that runs a script skein ships into a box: **through `box_shell`, or not
at all.**

Every script under `src/store/` and `src/probe/` decides for itself where the store it writes into
is, and the first place it looks is not the environment. `src/probe/box-self.sh`'s
`skein_box_store` reads `<git --git-common-dir>/skein-store` for the checkout it was started in, and
the launcher writes that file into the clone a box runs in. Every lane worktree of that clone shares
its common git directory, so a test that runs `mailbox.sh` with the working directory `cargo test`
gave it — the worktree — files its fixture notes into the LIVE store. Pinning `$SKEIN_HOME` or
`$SKEIN_FLEET_ROOT` does nothing about that, because the store is found through git (SKEIN-1229).
Three tests in `src/mailbox.rs` did exactly that from lane worktrees, twice, each time delivering
their "hello from A" fixtures to the owner's box; and the same inherited environment let their
`inbox` runs read that box's real `$SKEIN_STATE/inbox` and record its ids in the real
`$HOME/.skein-mail-seen`, which marks the owner's messages delivered without ever showing them.

`box_shell` (`src/testutil/box_script.rs`, `#[path]`-included by `tests/common/mod.rs`) is the one
way a test starts such a script: it runs in the test's own directory, with `$CLAUDE_PROJECT_DIR`,
`$HOME`, `$SKEIN_STATE` and `$SKEIN_FLEET_ROOT` all under that directory, git unable to climb out of
it, and the box's own identity variables removed. This gate refuses a test that starts one any other
way.

WHAT IS JUDGED. Every `fn` in test code — every file under `tests/`, and every `#[cfg(test)]` item
and whole-file test module under `src/` — whose code NAMES a box script: its file name (string
literals count, since a path is a string), or the name of a constant that `include_str!`s one. In
such a function every `Command::new(…)` is a finding unless its program is a string literal naming
something that is not a shell (`"git"`, `"jq"`, `"tmux"` …). A shell, or any program that is not a
literal — a path held in a variable may well be the script itself — has to come from `box_shell`.

A script name that some other file under `src/` also has (`box-session.sh` is the launcher's at
`src/box-session.sh` as well as a probe's) counts only when spelled with its directory, `probe/…` or
`store/…`, or through its constant: the bare name says nothing about which one is meant.

THE ESCAPE. A spawn that is in such a function but is not running a box script — `sh -c 'command -v
jq'` — carries `box-script-check: not a box script` in a comment on its line or the line above,
followed by why. The reason is the point: a bare marker is refused.

DERIVED, AND REFUSES WHEN IT DERIVES NOTHING. The script set comes from `src/store/*.sh` and
`src/probe/*.sh` as they stand, the constants from the `include_str!` lines that embed them, and the
helper's own name from its definition. No scripts, no constants, no helper, or no judged function at
all is a refusal (exit 2), never a pass: a reader that has stopped matching reports a clean tree,
which is the failure every gate here is written against.

LIMITS. Scope is the enclosing `fn`, closures included. A test that receives a script's path from a
helper defined in another `fn`, and never names it, is not seen; neither is a script run from a
string assembled elsewhere. The bwrap tier (`tests/isolation_bwrap/`) runs scripts inside a namespace
from shell text it builds, and isolates them there itself (its `HOPS` unsets `$SKEIN_STATE` and
`cd`s into the fixture tree); its Rust spawns name no script and so are not judged.

Usage: python3 tools/box-script-check.py [--list]   (`--list` prints every judged function)
"""

import os
import re
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import rustcut  # noqa: E402 — the one cutter every gate shares, self-checked at import

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SRC = os.path.join(ROOT, "src")
TESTS = os.path.join(ROOT, "tests")
SCRIPT_DIRS = ("store", "probe")
HELPER = "box_shell"
HELPER_FILE = os.path.join(SRC, "testutil", "box_script.rs")
MARKER = "box-script-check: not a box script"
SHELLS = {"bash", "sh", "/bin/bash", "/bin/sh", "/usr/bin/bash", "/usr/bin/env", "env", "dash"}

ITEM = re.compile(r"[ \t]*(?:pub(?:\([^)]*\))?\s+)?(?:const|static)\s+(?P<name>[A-Z][A-Z0-9_]*)\s*:")
FN = re.compile(r"\bfn\s+(?P<name>[A-Za-z_][A-Za-z0-9_]*)")
SPAWN = re.compile(r"\bCommand::new\s*\(")
INCLUDE = re.compile(
    r"\bconst\s+(?P<name>[A-Z][A-Z0-9_]*)\s*:\s*&(?:'static\s+)?str\s*=\s*"
    r"include_str!\(\s*\"(?P<path>[^\"]+)\"\s*\)"
)


class Blind(Exception):
    pass


def scripts():
    """(unambiguous names, {ambiguous name: [qualified spellings]})."""
    names = set()
    for d in SCRIPT_DIRS:
        for f in sorted(os.listdir(os.path.join(SRC, d))):
            if f.endswith(".sh"):
                names.add(f)
    elsewhere = set()
    for base, _, files in os.walk(SRC):
        rel = os.path.relpath(base, SRC).split(os.sep)[0]
        if rel in SCRIPT_DIRS:
            continue
        elsewhere.update(f for f in files if f in names)
    plain = names - elsewhere
    qualified = {n: [f"{d}/{n}" for d in SCRIPT_DIRS] for n in sorted(names & elsewhere)}
    return plain, qualified


def embedded(text):
    """{constant name: is it a box script} for every `include_str!` constant `text` defines."""
    out = {}
    for m in INCLUDE.finditer(text):
        parts = m.group("path").split("/")
        out[m.group("name")] = (
            len(parts) >= 2 and parts[-2] in SCRIPT_DIRS and parts[-1].endswith(".sh")
        )
    return out


def constants():
    """Names of the constants that `include_str!` a box script, anywhere under `src/`.

    A name is not unique across files — `src/fleet/install.rs` has a `BOOTSTRAP_SH` of its own that
    embeds the installer, not the store's `sandbox-bootstrap.sh` — so `scan` drops, per file, any
    name that file defines for something else.
    """
    out = set()
    for path in rustcut.crate_files([SRC]):
        out.update(n for n, box in embedded(open(path, encoding="utf-8").read()).items() if box)
    return out


def mention_pattern(plain, qualified, consts):
    alts = [re.escape(n) for n in sorted(plain)]
    alts += [re.escape(q) for qs in qualified.values() for q in qs]
    words = [r"\b" + re.escape(c) + r"\b" for c in sorted(consts)]
    # A plain name must not be the tail of a longer one: `box-status.sh` inside `my-box-status.sh`.
    return re.compile("|".join([r"(?<![A-Za-z0-9_.-])(?:" + "|".join(alts) + ")"] + words))


def functions(text, lo, hi):
    out = []
    for m in FN.finditer(text, lo, hi):
        end = rustcut.match_brace(text, m.end())
        body = text.find("{", m.end())
        # A signature with no body (a trait item) ends at `;` before any `{`.
        semi = text.find(";", m.end())
        if end < 0 or end > hi or body < 0 or (0 <= semi < body):
            continue
        out.append((m.group("name"), body, end, text.count("\n", 0, m.start()) + 1))
    return out


def innermost(fns, pos):
    best = None
    for fn in fns:
        if fn[1] <= pos < fn[2] and (best is None or fn[1] > best[1]):
            best = fn
    return best


def program_literal(text, at):
    """The string literal `Command::new(` was given, or None when its argument is not one."""
    m = re.compile(r'\s*"([^"\\]*)"\s*\)').match(text, at)
    return m.group(1) if m else None


def marked(lines, line):
    """Is the spawn on 1-based `line` carrying the escape, with a reason after it?"""
    for ln in (line, line - 1):
        if 1 <= ln <= len(lines):
            i = lines[ln - 1].find(MARKER)
            if i >= 0:
                rest = lines[ln - 1][i + len(MARKER):].strip(" \t—-:,.")
                return "bare" if not rest else "ok"
    return None


def files():
    wholly = {os.path.realpath(p) for p in rustcut.test_only_files([SRC])}
    for path in rustcut.crate_files([TESTS]):
        yield path, True
    for path in rustcut.crate_files([SRC]):
        if os.path.realpath(path) == os.path.realpath(HELPER_FILE):
            continue
        yield path, os.path.realpath(path) in wholly


def scan(plain, qualified, consts):
    judged, findings = [], []
    for path, whole in files():
        raw = open(path, encoding="utf-8").read()
        own = {n for n, box in embedded(raw).items() if not box}
        pat = mention_pattern(plain, qualified, consts - own)
        text = rustcut.blanked(raw)
        lines = raw.split("\n")
        rel = os.path.relpath(path, ROOT)
        regions = [(0, len(text))] if whole else rustcut.cfg_test_spans(text)
        for lo, hi in regions:
            fns = functions(text, lo, hi)
            named = {}
            items = []
            for m in pat.finditer(text, lo, hi):
                fn = innermost(fns, m.start())
                if fn is not None:
                    named.setdefault(fn, m.group(0))
                else:
                    items.append(m)
            # A `const`/`static` outside every fn that names a script — `const SCRIPT: &str =
            # "src/probe/box-status.sh"` — is a name for it, and naming that is naming the script.
            for m in items:
                head = text.rfind("\n", lo, m.start()) + 1
                item = ITEM.match(text, head)
                if item:
                    alias = re.compile(r"\b" + re.escape(item.group("name")) + r"\b")
                    for fn in fns:
                        if alias.search(text, fn[1], fn[2]):
                            named.setdefault(fn, f"{item.group('name')} ({m.group(0)})")
            # And a fn that calls one that names a script is judged too, to a fixpoint: a test's
            # `fn script() -> PathBuf` is called from the `fn fire` that spawns it.
            grew = True
            while grew:
                grew = False
                for callee in list(named):
                    # Called bare or through `Self::`/`self.`; `Command::new(` is not a call of a
                    # test's own `fn new`.
                    name = re.escape(callee[0])
                    call = re.compile(
                        r"(?:(?<![\w:.])|(?<=Self::)|(?<=self\.))" + name + r"\s*\("
                    )
                    for fn in fns:
                        if fn in named or fn == callee:
                            continue
                        body = text[fn[1]:fn[2]]
                        if call.search(body):
                            named[fn] = f"{named[callee]} via {callee[0]}()"
                            grew = True
            for fn, what in sorted(named.items(), key=lambda kv: kv[0][3]):
                judged.append((rel, fn[0], fn[3], what))
            for m in SPAWN.finditer(text, lo, hi):
                fn = innermost(fns, m.start())
                if fn is None or fn not in named:
                    continue
                prog = program_literal(text, m.end())
                if prog is not None and prog not in SHELLS:
                    continue
                line = text.count("\n", 0, m.start()) + 1
                mark = marked(lines, line)
                if mark == "ok":
                    continue
                findings.append((rel, line, fn[0], named[fn], prog, mark))
    return judged, findings


def main(argv):
    if not os.path.isfile(HELPER_FILE) or not re.search(
        r"\bfn\s+" + HELPER + r"\b", open(HELPER_FILE, encoding="utf-8").read()
    ):
        raise Blind(f"`fn {HELPER}` is not defined in {os.path.relpath(HELPER_FILE, ROOT)}")
    plain, qualified = scripts()
    if not plain:
        raise Blind("no script under src/store or src/probe")
    consts = constants()
    if not consts:
        raise Blind("no `include_str!` of a src/store or src/probe script")
    judged, findings = scan(plain, qualified, consts)
    if not judged:
        raise Blind("no test function names a box script — the reader has stopped matching")
    if "--list" in argv:
        for rel, fn, line, what in judged:
            print(f"{rel}:{line}: fn {fn} (names {what})")
    print(
        f"box-script-check: {len(plain) + len(qualified)} scripts, {len(consts)} constants, "
        f"{len(judged)} test functions name one"
    )
    if not findings:
        return 0
    for rel, line, fn, what, prog, mark in findings:
        how = f'"{prog}"' if prog is not None else "a program that is not a literal"
        why = " (the escape needs a reason after it)" if mark == "bare" else ""
        print(
            f"{rel}:{line}: fn {fn} names {what} and starts {how} with `Command::new`{why} — "
            f"start it with `{HELPER}(dir)` instead, so it cannot find the store the checkout "
            f"points at, the owner's inbox or their seen-file (SKEIN-1229)"
        )
    print(f"box-script-check: {len(findings)} spawn(s) outside `{HELPER}`")
    return 1


if __name__ == "__main__":
    try:
        sys.exit(main(sys.argv[1:]))
    except (Blind, rustcut.Blind) as e:
        print(f"box-script-check: REFUSED — {e}")
        sys.exit(2)
