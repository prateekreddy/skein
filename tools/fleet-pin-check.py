#!/usr/bin/env python3
"""One rule about the environment a test pins: **say something about every coupled variable.**

`$SKEIN_FLEET_ROOT` defaults to `/boxes`, which on any machine running skein is the owner's LIVE
fleet; `$SKEIN_HOME` defaults to the real `~/.skein`. A test that pins one and is silent about the
other therefore runs half-hermetic, and the half it forgot is the real machine. Five tests installed
uncommitted code onto the owner's live fleet that way (SKEIN-530), `tests/server.rs` spawned a real
`skein-server` whose `main` runs `heal_fleet` against whatever root it resolved (SKEIN-685), four box
directories from test fixtures were found sitting in `/boxes` beside eleven real boxes (SKEIN-654),
and a unit test passed or failed on how full the real machine's disk was while its message accused
the code (SKEIN-690).

**There is already a runtime guard, and this gate exists because a runtime guard fires only on
reach.** `util::fleet_root` and `config::skein_home` each `assert!(!in_test(), …)` rather than
falling back, so a test that actually resolves an unpinned path dies instead of touching the real
one. But a test that pins only `$SKEIN_HOME` passes today whenever its code path merely *happens*
not to resolve a fleet path — the pin's absence is invisible, matches no grep, and becomes a failure
later in a test nobody edited, the day something inside the library moves a fleet read onto that
path. The runtime guard answers "is this process about to act on the live fleet"; this gate answers
"did the author of this test say what they meant about both variables", which is a question you can
only ask of the text. Six files were in that state when this was written.

So the two are complements, not duplicates, and neither subsumes the other:

  · the guard covers every process, including the subprocesses this gate pools (see LIMITS);
  · the gate covers every test, including the ones whose paths do not reach the guard today.

WHAT COUNTS AS SAYING SOMETHING. Setting the variable, and also REMOVING it: `env_remove`,
`remove_var` and `EnvPins::unset` are deliberate statements about a variable — `tests/server.rs`
pins `$SKEIN_HOME` and `.env_remove("SKEIN_FLEET_ROOT")` on purpose, to prove the server refuses to
start without one, and that test is exactly right. The finding is silence, not absence.

THE COUPLED SET IS DERIVED, NOT LISTED. `tests/ui/harness/leaks.mjs` is the idiom: it carries no
list of fixture names, reads them out of the call sites that create fixtures, prints what it
derived, and refuses to run when it derives none — so a rename it stops recognising fails loudly
instead of quietly printing zero. The same bargain here, in two steps.

**Step one: which variables does the library refuse to answer for in a test process?** That is a
thing the code says out loud, three times today:

    assert!(!in_test(), "$SKEIN_FLEET_ROOT is unset in a test process …")   src/util.rs
    assert!(!in_test(), "$SKEIN_HOME is unset in a test process …")         src/config.rs
    assert!(!in_test(), "$SKEIN_GITHUB_API is unset in a test process …")   src/github.rs

Each guard is read TWICE and the two readings must agree — the `env::var` it stands in front of, and
the `$NAME` its own refusal message names — because a guard whose message has drifted from its code
is the one thing a single reading cannot see. They disagree, the gate REFUSES.

**Step two, and the first draft of this gate got it wrong: refusing to answer for a variable is not
the same relation as being coupled to another one.** All three guards above have identical shape,
and `$SKEIN_GITHUB_API` is emphatically NOT coupled to the other two — a test that pins `$SKEIN_HOME`
has no need of a fake GitHub, and demanding one would be a ritual. Deriving the set from the guard
shape alone produced exactly that, on the first run.

The coupling is written down too, in the part of each refusal that tells you what else to set:

    util.rs    "Set $SKEIN_FLEET_ROOT to this test's own temp directory — and $SKEIN_HOME with it,
                since anything resolving a fleet path almost certainly resolves a home too."
    config.rs  "Set $SKEIN_HOME to this test's own temp directory — and $SKEIN_FLEET_ROOT with it
                if what you are exercising resolves a fleet path."
    github.rs  "Point this test at its own listener … or at http://127.0.0.1:1" — names no
                companion variable at all.

So: guard A is coupled to variable B when A's refusal names B, and **the naming must be MUTUAL** —
B's own guard must name A back. Mutuality is what makes this a derivation rather than a reading of
one author's phrasing: a one-way mention is a cross-reference, two-way is a pair both sides
committed to. A group is a connected component of size two or more, and the rule is applied per
group, so a third variable that joins the pair — or a second, unrelated pair — needs no edit here.

The gate REFUSES when no group of two survives: one variable has nothing to be coupled to, and zero
reads exactly like a clean tree, which is the failure this whole file is built against.

LIMITS, stated because a gate's blind spots are the part nobody finds out by running it:

  · **Scopes are pooled per `fn`, so a process pin and a subprocess pin in one function count
    together.** A function that pins `$SKEIN_HOME` process-globally and separately spawns a
    `Command` carrying only `$SKEIN_FLEET_ROOT` satisfies this gate while that child is missing a
    home. Per-builder-chain attribution was tried and is not reliable: `tests/git_write_request.rs`
    builds one `Command` across a `let`, a method chain and a `match` arm. The runtime guard is what
    covers that case — `tests/server.rs::a_server_without_a_fleet_root_refuses_to_start` is the
    proof it does — and pooling is the same granularity `tools/env-lock-check.py` uses.
  · **A pin's VALUE is not checked.** `.env("SKEIN_FLEET_ROOT", "/boxes")` satisfies both this gate
    and the runtime guard while pointing at the live fleet. That is a real and separate hazard, and
    it is SKEIN-789's, not this one's: eight unit tests do it today and one of them (SKEIN-644) is
    argued to be the one pin that cannot be hermetic. A half-built value rule with eight exemptions
    would be worse than the item that is open about it.

`docs/fleet-pins.toml` is the exemption list, in the shape `docs/env-lock.toml` and
`docs/sources.toml` use: a reviewed allow-list where every entry carries a reason, so the next
silent pin is a line in a diff and a decision somebody made. An entry that no longer names a real
finding fails the build too — a stale exemption is a permission nobody granted.

    python3 tools/fleet-pin-check.py            check
    python3 tools/fleet-pin-check.py --show     every pinning scope and its verdict
    python3 tools/fleet-pin-check.py --update    rewrite the exemption list from the code
"""

import os
import re
import sys
import tomllib

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import rustcut  # noqa: E402 — the one cutter every gate shares, self-checked at import

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SPEC = os.path.join(ROOT, "docs", "fleet-pins.toml")

# Where the guards are read FROM.
LIB_DIRS = [os.path.join(ROOT, "src"), os.path.join(ROOT, "warden", "src")]

# Where the rule is APPLIED: the integration binaries, in their entirety.
#
# **Why the crate's own `#[cfg(test)]` tests are read but not judged, which is a real limit and not
# an oversight.** Applied to all test code this rule produces 345 findings, and 328 of them are
# `src/` unit tests that are not hazards: a unit test runs IN PROCESS with the guards, so if its
# path ever resolves an unpinned fleet path the assert fires and the test dies. Reach implies a
# panic there, and demanding the pin as well would be the ritual `tools/env-lock-check.py` declines
# to demand of a test that is alone in its own binary.
#
# `tests/*.rs` is different in the way that matters: it SPAWNS. A `tests/*.rs` binary starts real
# `skein-server`s, `bwrap` namespaces, `tmux` servers and — the case no Rust assert can reach —
# `bash src/box-session.sh`, which resolves `${SKEIN_FLEET_ROOT:-/boxes}` in shell
# (18 occurrences across 12 scripts under `src/`; `grep -rn 'SKEIN_FLEET_ROOT:-' src --include=*.sh`).
# There is no `in_test()` in a shell script, so for that child the parent's pin list is the whole
# answer and silence means the owner's live fleet. Every instance of this class that did damage was
# on this surface: five tests installed uncommitted code onto the live fleet (SKEIN-530), a real
# `skein-server` ran `heal_fleet` against it (SKEIN-685), and four box directories were left sitting
# in `/boxes` beside eleven real boxes (SKEIN-654).
#
# The `src/` scopes are still COUNTED and the count is printed on every run, so the debt is a number
# a reader sees rather than a silence — an exclusion nobody can see is indistinguishable from a gate
# that has stopped reading. SKEIN-789 and SKEIN-676 own that debt, and they own it with a rule about
# a pin's VALUE, which fits those scopes better than this rule about a pin's absence.
TEST_DIRS = [os.path.join(ROOT, "tests")]

# Read, counted, reported — not judged. See the comment on TEST_DIRS.
UNJUDGED_DIRS = [os.path.join(ROOT, "src"), os.path.join(ROOT, "warden", "src")]

# The least a working derivation can produce. "Coupled" is a relation between two variables, so one
# is not a smaller answer to the same question — it is no answer. See the module docstring.
MIN_COUPLED = 2


# ---------------------------------------------------------------------------------------------
# The derivation: which variables does the library refuse to answer for in a test process?
# ---------------------------------------------------------------------------------------------

# `assert!(` … `!in_test()` as the condition, in any of the spellings this tree uses for the call
# (`in_test()`, `crate::util::in_test()`, `util::in_test()`). The condition must be the WHOLE
# negation — `!(self.defaulted && crate::util::in_test())` in `src/warden_client.rs:565` is a guard
# about something else and must not be read as one of these, which is why the `!` is anchored to the
# open paren of the `assert!` and nothing may sit between them.
GUARD = re.compile(
    r"\bassert!\s*\(\s*!\s*(?:[A-Za-z_][A-Za-z0-9_]*\s*::\s*)*in_test\s*\(\s*\)\s*,",
    re.S,
)

# A read of an environment variable by literal name: `env::var("X")`, `std::env::var_os("X")`, or a
# bare `var_os("X")` after a `use`.
ENV_READ = re.compile(r"\bvar(?:_os)?\s*\(\s*\"(?P<name>[A-Za-z_][A-Za-z0-9_]*)\"")

# `$NAME` inside the refusal message. The message is prose and names the variable the way a reader
# would write it, which is the second, independent reading of the same fact.
DOLLAR = re.compile(r"\$(?P<name>[A-Z][A-Z0-9_]{2,})")

FN = re.compile(
    r"^(?P<indent>[ \t]*)(?:pub(?:\([^)]*\))?\s+)?(?:async\s+)?fn\s+(?P<name>[A-Za-z_][A-Za-z0-9_]*)",
    re.M,
)


class Refused(Exception):
    """The gate cannot answer the question it exists to answer, so it does not pretend to.

    Raised rather than returned, and exits 2 rather than 1: "I could not check" is not "nothing is
    wrong", and the whole failure mode this gate is built against is a check that reports zero
    problems because it looked at nothing (SKEIN-647).
    """


def lib_files():
    for d in LIB_DIRS:
        for base, _, files in os.walk(d):
            for f in sorted(files):
                if f.endswith(".rs"):
                    yield os.path.join(base, f)


def functions(text, lo, hi):
    """Every `fn` whose body lies inside [lo, hi), with the offsets of its body."""
    out = []
    for m in FN.finditer(text, lo, hi):
        end = rustcut.match_brace(text, m.end())
        if end < 0 or end > hi:
            continue
        body_start = text.find("{", m.end())
        if body_start < 0 or body_start > end:
            continue
        out.append(
            {
                "name": m.group("name"),
                "start": m.start(),
                "body_start": body_start,
                "end": end,
                "line": text.count("\n", 0, m.start()) + 1,
            }
        )
    return out


def enclosing(fns, pos):
    """The innermost fn whose body contains `pos`, or None."""
    best = None
    for fn in fns:
        if fn["body_start"] <= pos < fn["end"]:
            if best is None or fn["body_start"] > best["body_start"]:
                best = fn
    return best


def guards():
    """{name: (unit, line, [other variables this guard's refusal names])} — step one.

    Read out of the `assert!(!in_test(), …)` guards, and each guard read TWICE: the `env::var` it
    stands in front of, and the `$NAME` its own message names. Two readings of one fact, because a
    message that has drifted from its code is invisible to either reading alone.
    """
    found = {}
    for path in lib_files():
        raw = open(path, encoding="utf-8").read()
        text = rustcut.blanked(raw)
        unit = os.path.relpath(path, ROOT)
        fns = functions(text, 0, len(text))
        for m in GUARD.finditer(text):
            line = text.count("\n", 0, m.start()) + 1
            fn = enclosing(fns, m.start())
            if fn is None:
                raise Refused(
                    f"{unit}:{line}: an `assert!(!in_test(), …)` guard outside any fn — this gate "
                    f"reads the variable each guard protects out of its enclosing function, and "
                    f"cannot read this one"
                )
            # The guarded read is the LAST env read before the assert. `config::skein_home` reads
            # `$HOME` in its fallback BELOW the assert, and taking every read in the body would
            # couple `$HOME` to the fleet — the fallback is what the guard refuses to reach, not
            # what it protects.
            before = text[fn["body_start"] : m.start()]
            reads = ENV_READ.findall(before)
            if not reads:
                raise Refused(
                    f"{unit}::{fn['name']} ({unit}:{line}): `assert!(!in_test(), …)` guards no env "
                    f"read that this gate can see. It reads the guarded variable from the last "
                    f'`env::var("NAME")` above the assert; there is none.'
                )
            protects = reads[-1]

            # The second reading: the refusal message, which runs from the comma this matched to
            # the end of the `assert!` call.
            message = text[m.end() : paren_end(text, m.end())]
            named = DOLLAR.findall(message)
            # **The message must OPEN by naming the variable the code guards**, which is tighter
            # than "names it somewhere" and is tighter on purpose. Every one of these messages
            # mentions its own variable twice — once to say it is unset, once to say what to set —
            # so "somewhere" is satisfied by either mention alone, and a typo in the first one
            # passed: changing `$SKEIN_FLEET_ROOT is unset` to `$SKEIN_FLEET_ROOOT is unset` left
            # this gate green, because the later `Set $SKEIN_FLEET_ROOT to …` still matched. That
            # was found by sabotage, not by reading. All three guards in the tree open by naming
            # their variable, so this costs nothing and makes the second reading a real one.
            if not named or named[0] != protects:
                raise Refused(
                    f"{unit}::{fn['name']} ({unit}:{line}): this guard's code and its own message "
                    f"disagree about which variable it protects. The code guards ${protects}; its "
                    f"message opens by naming "
                    f"{'$' + named[0] if named else '(no $VARIABLE at all)'}"
                    f"{' and goes on to name ' + ', '.join('$' + n for n in named[1:]) if len(named) > 1 else ''}"
                    f". One of the two has drifted, and a gate cannot tell which — fix them to "
                    f"agree. A refusal that names the wrong variable sends a contributor to set "
                    f"the wrong one, which is worse than no message."
                )
            if protects in found:
                continue
            companions = sorted({n for n in named if n != protects})
            found[protects] = (unit, line, companions)
    return found


def coupling_groups(found):
    """[[name, …]] — step two: the mutually-named components of size two or more.

    A one-way mention is a cross-reference; two-way is a pair both guards committed to. See the
    module docstring for why the guard SHAPE alone is the wrong relation — `$SKEIN_GITHUB_API` has
    the same shape and belongs to no pair.
    """
    edges = {
        a: {b for b in companions if b in found and a in found[b][2]}
        for a, (_, _, companions) in found.items()
    }
    groups, seen = [], set()
    for a in sorted(edges):
        if a in seen:
            continue
        component, frontier = set(), [a]
        while frontier:
            n = frontier.pop()
            if n in component:
                continue
            component.add(n)
            frontier.extend(edges[n])
        seen |= component
        if len(component) >= MIN_COUPLED:
            groups.append(sorted(component))
    if not groups:
        raise Refused(
            f"derived no coupled GROUP at all. The guards found were "
            f"{', '.join('$' + n for n in sorted(found)) or 'none'}, and none of them names "
            f"another one that names it back.\n"
            f"    'Coupled' is a relation between variables: one variable is not a smaller answer\n"
            f"    to the same question, it is no answer, and zero reads exactly like a clean tree.\n"
            f"    The set comes from `assert!(!in_test(), …)` guards in "
            f"{', '.join(os.path.relpath(d, ROOT) for d in LIB_DIRS)}, paired by each refusal\n"
            f"    message naming the other variable to set alongside it. If a guard or that\n"
            f"    sentence was removed on purpose, this gate has lost its subject and must be\n"
            f"    changed on purpose too."
        )
    return groups


def paren_end(text, start):
    """Index just past the `)` that closes the paren depth open at `start`, or len(text).

    `start` sits just after the comma inside `assert!(`, i.e. at depth 1 already.
    """
    i, depth, n = start, 1, len(text)
    while i < n:
        past = rustcut.skip_token(text, i)
        if past is not None and past > i:
            i = past
            continue
        c = text[i]
        if c in "([{":
            depth += 1
        elif c in ")]}":
            depth -= 1
            if depth == 0:
                return i
        i += 1
    return n


# ---------------------------------------------------------------------------------------------
# The application: what does each test scope say about those variables?
# ---------------------------------------------------------------------------------------------


def pin_pattern(names):
    """Every spelling of "this code says something about $NAME", for the derived `names`.

    Setting and REMOVING both count: see the module docstring. The spellings are the four the tree
    uses — `std::env::set_var`/`remove_var`, `EnvPins::set`/`unset`, and `Command::env`/`env_remove`
    — and a name is only recognised as a literal, because a pin built by `format!` or looped over a
    list is not something a text gate can attribute.
    """
    alt = "|".join(re.escape(n) for n in names)
    return re.compile(
        r"\b(?:set_var|remove_var|set|unset|env|env_remove)\s*\(\s*\"(?P<name>" + alt + r")\"",
    )


def test_regions(text, whole_file):
    if whole_file:
        return [(0, len(text))]
    return rustcut.cfg_test_spans(text)


# `#[cfg(test)] mod <name>;` — a module whose WHOLE FILE is test code, declared in the parent.
CFG_TEST_MOD = re.compile(
    r"#\[cfg\(test\)\]\s*(?:pub(?:\([^)]*\))?\s+)?mod\s+(?P<name>[A-Za-z_][A-Za-z0-9_]*)\s*;"
)


def test_only_modules():
    """Every `src/` file that is test code in its entirety because its `mod` line says so.

    **Clause C found this, on the first run of this gate, and it is the reason that clause exists.**
    A file under `src/` is normally test code only inside its own `#[cfg(test)]` items, so that is
    what the structural walk cut for. But four files here carry no such attribute and are test code
    all the way down, because the attribute is on the `mod` line in the PARENT:

        src/lib.rs:72        #[cfg(test)] mod testutil;
        src/prq/mod.rs:44    #[cfg(test)] mod fixtures;
        src/prwork/mod.rs:41 #[cfg(test)] mod testkit;
        src/review/mod.rs:56 #[cfg(test)] mod testkit;

    `src/review/testkit.rs::drafting_fixture_for` sets `$SKEIN_HOME` at file scope with no
    attribute above it, so the span cut placed it in no test region and the structural walk did not
    see it at all — while the flat walk did. Derived from the declarations rather than listed,
    because the fifth one of these will be written by somebody who has not read this comment.
    """
    out = set()
    for path in lib_files():
        raw = open(path, encoding="utf-8").read()
        text = rustcut.blanked(raw)
        here = os.path.dirname(path)
        for m in CFG_TEST_MOD.finditer(text):
            name = m.group("name")
            for candidate in (
                os.path.join(here, name + ".rs"),
                os.path.join(here, name, "mod.rs"),
            ):
                if os.path.exists(candidate):
                    out.add(os.path.realpath(candidate))
    return out


def test_files():
    """(path, whole_file) for every Rust file the rule is applied to, in a stable order."""
    wholly = test_only_modules()
    out = []
    for d in TEST_DIRS:
        for base, _, files in os.walk(d):
            for f in sorted(files):
                if f.endswith(".rs"):
                    out.append((os.path.join(base, f), True))
    for d in UNJUDGED_DIRS:
        for base, _, files in os.walk(d):
            for f in sorted(files):
                if f.endswith(".rs"):
                    path = os.path.join(base, f)
                    out.append((path, os.path.realpath(path) in wholly))
    return out


def judged(unit):
    """Is this unit's verdict binding? See the comment on TEST_DIRS for why `src/` is not."""
    return any(unit.startswith(os.path.relpath(d, ROOT) + "/") for d in TEST_DIRS)


def unit_name(path):
    rel = os.path.relpath(path, ROOT)
    return rel[:-3] if rel.endswith(".rs") else rel


def scan(names):
    """(scopes, unattributed) over every test file.

    `scopes` is one entry per (fn that pins at least one coupled variable), carrying the set it
    mentions. `unattributed` is clause C — see `flat_sites`.
    """
    pat = pin_pattern(names)
    scopes, unattributed = [], []
    for path, whole_file in test_files():
        raw = open(path, encoding="utf-8").read()
        text = rustcut.blanked(raw)
        unit = unit_name(path)

        # --- the structural walk: brace-matched fn bodies inside test regions ---
        attributed = set()
        for lo, hi in test_regions(text, whole_file):
            fns = functions(text, lo, hi)
            per_fn = {}
            for m in pat.finditer(text, lo, hi):
                fn = enclosing(fns, m.start())
                if fn is None:
                    continue
                key = (fn["name"], fn["line"])
                per_fn.setdefault(key, set()).add(m.group("name"))
                attributed.add(m.start())
            for (fn_name, fn_line), mentioned in sorted(per_fn.items(), key=lambda kv: kv[0][1]):
                scopes.append(
                    {
                        "unit": unit,
                        "fn": fn_name,
                        "line": fn_line,
                        "mentions": mentioned,
                    }
                )

        # --- clause C: the second walk, and what it found that the first could not place ---
        for pos, line, name in flat_sites(text, pat):
            if pos not in attributed:
                unattributed.append((unit, line, name))
    return scopes, unattributed


def flat_sites(text, pat):
    """[(offset, line, name)] — every pin site, found WITHOUT any structural machinery.

    **This is the second, independent walk, and it is the reason this gate cannot report "0
    problems" while reading nothing.** The walk above resolves `#[cfg(test)]` spans, matches braces
    to find function bodies, and attributes each site to the innermost one. Every one of those steps
    has failed in this repository before: `tools/env-lock-check.py`'s own brace counter closed a
    module 278 lines early and the gate reported *no env-touching scope at all* in a file that has
    212 of them, and it looked exactly like a clean tree (that incident is what `tools/rustcut.py`
    exists for). A count whose two sides both come out of the same walk cannot see that.

    So this pass does none of it: no braces, no `fn`, no cfg spans — just "where does the pin
    pattern match". Every match the structural pass could not place is reported. If the structural
    walk goes blind, these two numbers diverge and the gate goes RED naming the file, instead of
    green naming nothing.

    What the two walks DO share is the comment cutter and the derived variable names. That is
    deliberate and it is covered elsewhere rather than here: `rustcut.self_check()` runs at import
    on every invocation and pins the cutter against unbalanced-brace fixtures in every string
    spelling, and the derivation refuses rather than shrinking (`MIN_COUPLED`). Each half of this
    gate is held up by something that can fail.
    """
    return [(m.start(), text.count("\n", 0, m.start()) + 1, m.group("name")) for m in pat.finditer(text)]


def key(scope):
    return f"{scope['unit']}::{scope['fn']}"


def missing_in(scope, groups):
    """Which coupled variables this scope says nothing about, group by group.

    A scope that mentions no member of a group is not judged against that group at all: the rule is
    "having said something about one of a pair, say something about the other", not "every test must
    pin every variable in the tree".
    """
    missing = set()
    for group in groups:
        touched = scope["mentions"] & set(group)
        if touched:
            missing |= set(group) - touched
    return sorted(missing)


# ---------------------------------------------------------------------------------------------
# The exemption list
# ---------------------------------------------------------------------------------------------


def load_spec():
    if not os.path.exists(SPEC):
        return {}
    with open(SPEC, "rb") as fh:
        data = tomllib.load(fh)
    return data.get("exempt", {})


def render(findings, spec):
    """The exemption file, as `--update` writes it: every finding, with any reason already given."""
    out = [
        "# Test scopes that pin one coupled environment variable and say nothing about another.",
        "#",
        "# Read by `tools/fleet-pin-check.py`, which fails the build on an undeclared one AND on an",
        "# entry here that no longer names a real finding — a stale exemption is a permission nobody",
        "# granted. Run `python3 tools/fleet-pin-check.py --update` to regenerate the SHAPE; the",
        "# reasons are written by hand and are the only part that matters.",
        "#",
        "# **This list is not an argument that its entries are fine.** It is the same kind of list as",
        "# `docs/env-lock.toml`: what the code does today, written down, so that the NEXT silent pin",
        "# is a line in a diff instead of a box directory in the owner's live fleet (SKEIN-654).",
        "#",
        "# Two kinds of entry live here and they are not the same kind of debt:",
        "#",
        "#   * the callee cannot read the other variable — a shell script, or a process that is not",
        "#     a skein binary at all. Permanent, and an argument rather than an apology.",
        "#   * the file belongs to another lane this round. Temporary; the reason names the item.",
        "#",
        "# Each entry is an `[exempt.\"<unit>::<fn>\"]` table, where `<unit>` is the path with `.rs`",
        "# dropped. The loader reads `exempt` and nothing else, so a top-level",
        "# `\"<unit>::<fn>\" = \"reason\"` line parses fine and is then silently ignored — the entry",
        "# looks declared and the gate still fails.",
        "",
    ]
    for scope in findings:
        k = key(scope)
        prior = spec.get(k, {})
        reason = prior.get("reason", "TODO: why this scope is silent about the variable below.")
        out.append(f'[exempt."{k}"]')
        out.append("missing = [" + ", ".join(f'"{n}"' for n in scope["missing"]) + "]")
        out.append(f'reason = """{reason}"""')
        out.append("")
    return "\n".join(out)


# ---------------------------------------------------------------------------------------------
# Entry points
# ---------------------------------------------------------------------------------------------


def main(argv):
    try:
        found = guards()
        groups = coupling_groups(found)
    except Refused as e:
        print("fleet-pin-check: REFUSED TO RUN", file=sys.stderr)
        print(f"    {e}", file=sys.stderr)
        return 2
    names = sorted({n for group in groups for n in group})

    # Printed above the answer, always, the way `leaks.mjs` prints the names it looked for: a
    # verdict about a set you cannot see is not a verdict you can act on. The guards that are NOT
    # in a group are printed too — a reader who expects one of them to be coupled should be able to
    # see that this gate considered it and why it is out.
    print(
        "fleet-pin-check: coupled variables, derived from the library's own "
        "`assert!(!in_test(), …)` guards and paired by the companion each refusal names:"
    )
    for i, group in enumerate(groups, 1):
        print(f"    group {i}: " + ", ".join("$" + n for n in group))
        for n in group:
            unit, line, _ = found[n]
            print(f"        ${n:<18} {unit}:{line}")
    for n in sorted(set(found) - set(names)):
        unit, line, companions = found[n]
        why = (
            "names " + ", ".join("$" + c for c in companions) + ", which does not name it back"
            if companions
            else "names no companion variable"
        )
        print(f"    not coupled: ${n:<16} {unit}:{line} — {why}")

    scopes, unattributed = scan(names)

    for s in scopes:
        s["judged"] = judged(s["unit"])

    # **Asked of the JUDGED scopes, not of every scope this gate read — and that distinction is the
    # difference between a check and a ritual.** The first draft asked `if not scopes`, and it could
    # not fire: `src/` and `warden/src/` are read too and contribute hundreds of scopes, so the
    # count stayed high however completely the gate stopped seeing `tests/`, which is the only
    # directory whose verdict is binding. Proved by sabotage — pointing TEST_DIRS at an empty
    # directory left this branch silent, and the run went red only incidentally, on the eleven
    # exemptions that had stopped matching anything. With an empty exemption file it would have been
    # GREEN having judged nothing at all. An absence that was never a presence proves nothing.
    #
    # (The message also interpolated a directory-list name that no longer exists, so the one path
    # that reached it would have died of a NameError rather than printing this. Found the same way.)
    if not [s for s in scopes if s["judged"]]:
        print("fleet-pin-check: REFUSED TO RUN", file=sys.stderr)
        print(
            f"    Not one scope in {', '.join(os.path.relpath(d, ROOT) for d in TEST_DIRS)} — the "
            f"only directory this gate's verdict is binding for — pins any of "
            f"{', '.join('$' + n for n in names)}.\n"
            f"    That is not a clean tree: those variables are what this suite's fixtures are\n"
            f"    built on, and {len(scopes)} scopes elsewhere in the tree pin them. It is a gate\n"
            f"    that has stopped reading the files it names.",
            file=sys.stderr,
        )
        return 2

    for s in scopes:
        s["missing"] = missing_in(s, groups)
    findings = [s for s in scopes if s["missing"] and s["judged"]]
    unjudged = [s for s in scopes if s["missing"] and not s["judged"]]

    # Printed on every run, so the part of the tree this gate does not judge is a NUMBER a reader
    # sees rather than a silence. An exclusion nobody can see reads exactly like a gate that has
    # stopped reading the files it names, which is the whole failure this file is built against.
    print(
        f"    judging {', '.join(os.path.relpath(d, ROOT) for d in TEST_DIRS)} "
        f"({len([s for s in scopes if s['judged']])} pinning scopes); "
        f"{len(unjudged)} scopes in "
        f"{', '.join(os.path.relpath(d, ROOT) for d in UNJUDGED_DIRS)} are silent about a coupled "
        f"variable and are read but NOT judged — SKEIN-789 and SKEIN-676 own that debt (see the "
        f"comment on TEST_DIRS)"
    )

    spec = load_spec()

    if "--update" in argv:
        open(SPEC, "w", encoding="utf-8").write(render(findings, spec))
        print(f"fleet-pin-check: wrote {len(findings)} entries to {os.path.relpath(SPEC, ROOT)}")
        return 0

    if "--show" in argv:
        for s in sorted(scopes, key=key):
            verdict = "ok" if not s["missing"] else "MISSING " + ", ".join("$" + n for n in s["missing"])
            said = ", ".join("$" + n for n in sorted(s["mentions"]))
            print(f"  {key(s)}:{s['line']}\n      says {said}\n      {verdict}")
        print(f"\n{len(scopes)} pinning scopes, {len(findings)} of them silent about something")
        return 0

    bad = 0

    # Clause C first: if the structural walk went blind, every verdict below is about nothing.
    if unattributed:
        print(
            f"\nfleet-pin-check: {len(unattributed)} pin site(s) that the flat walk found and the "
            f"structural walk could not place in any function.",
            file=sys.stderr,
        )
        print(
            "    Both walks read the same files. One resolves `#[cfg(test)]` spans, matches braces\n"
            "    and attributes each site to the innermost `fn`; the other only matches the pattern.\n"
            "    They have diverged, so the verdicts below are about a subset of the tree and the\n"
            "    size of that subset is unknown. This is the shape of SKEIN-647 — a check reporting\n"
            "    zero because it looked at nothing — and it is refused rather than reported.",
            file=sys.stderr,
        )
        for unit, line, name in unattributed[:40]:
            print(f"      {unit}.rs:{line}  ${name}", file=sys.stderr)
        if len(unattributed) > 40:
            print(f"      ({len(unattributed)} in all; 40 shown)", file=sys.stderr)
        return 2

    undeclared = []
    for s in findings:
        k = key(s)
        entry = spec.get(k)
        if entry is None:
            undeclared.append(s)
            continue
        declared = set(entry.get("missing", []))
        if declared != set(s["missing"]):
            print(
                f"fleet-pin-check: {k} is declared as missing "
                f"{', '.join('$' + n for n in sorted(declared)) or 'nothing'}, but it is missing "
                f"{', '.join('$' + n for n in s['missing'])}.",
                file=sys.stderr,
            )
            bad = 1
        if not entry.get("reason", "").strip():
            print(f"fleet-pin-check: {k} is declared with no reason.", file=sys.stderr)
            bad = 1

    if undeclared:
        print(
            f"\nfleet-pin-check: {len(undeclared)} test scope(s) pin one coupled variable and say "
            f"nothing about another:",
            file=sys.stderr,
        )
        for s in sorted(undeclared, key=key):
            said = ", ".join("$" + n for n in sorted(s["mentions"]))
            miss = ", ".join("$" + n for n in s["missing"])
            print(f"    {s['unit']}.rs:{s['line']}  {s['fn']}", file=sys.stderr)
            print(f"        says {said}, and nothing about {miss}", file=sys.stderr)
        print(
            "\n    $SKEIN_FLEET_ROOT unpinned means /boxes, which on a machine running skein is the\n"
            "    owner's LIVE fleet; $SKEIN_HOME unpinned means the real ~/.skein. Pin it at this\n"
            "    test's own scratch directory, or REMOVE it deliberately (`.env_remove`, `unset`)\n"
            "    if the point of the test is what happens without one — a removal counts as saying\n"
            f"    something. If neither fits, declare it in {os.path.relpath(SPEC, ROOT)} with a\n"
            "    reason: `python3 tools/fleet-pin-check.py --update` writes the shape.",
            file=sys.stderr,
        )
        bad = 1

    live = {key(s) for s in findings}
    stale = sorted(set(spec) - live)
    if stale:
        print(
            f"\nfleet-pin-check: {len(stale)} exemption(s) in {os.path.relpath(SPEC, ROOT)} that no "
            f"longer name a finding:",
            file=sys.stderr,
        )
        for k in stale:
            print(f"    {k}", file=sys.stderr)
        print(
            "\n    Either the scope was fixed — delete the entry, that is the good case — or it was\n"
            "    renamed or removed and the entry now grants a permission to nothing. A stale\n"
            "    exemption is how an allow-list stops being a list of decisions.",
            file=sys.stderr,
        )
        bad = 1

    if not bad:
        print(
            f"fleet-pin-check: {len(scopes)} pinning scopes across "
            f"{len({s['unit'] for s in scopes})} files, {len(findings)} declared in "
            f"{os.path.relpath(SPEC, ROOT)}, none undeclared"
        )
    return bad


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
