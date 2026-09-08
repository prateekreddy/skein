#!/usr/bin/env python3
"""Two rules about the process-global environment: hold the lock while you write it, put it back after.

They are separate guarantees and this file checks both, because for a year it checked only the first
and the second was read off it. **Holding the lock protects a CONCURRENT test; restoring protects a
LATER one**, and a test can do the first perfectly while doing none of the second — which is
SKEIN-696. `src/repos.rs` pinned `$SKEIN_FLEET_ROOT` under the lock, never removed it, and the pin
outlived the test and answered a later one that pinned none of its own. Two defects cancelled out
into a green suite; `tools/alone-check.py` saw it and this gate did not, because the lock was held.

`std::env::set_var` and `remove_var` write to a table shared by every thread in the process, and
`cargo test` runs a crate's tests multi-threaded in ONE process. So a test that sets `SKEIN_HOME`
is writing into the middle of whatever else is running. `src/testutil.rs` has the one lock every
such test is supposed to take first, and its own doc comment says why there is exactly one:

    "One lock for the whole crate: a per-module lock would serialize each module against itself and
     nothing else, which is the failure mode that looks like a flaky test."

SKEIN-307 is what that failure mode looks like from the outside: a GitHub request-count assertion in
`src/prq.rs` failed once and passed on re-run with no source change, so the natural reading was that
the batching logic was wrong. It was not. **This checker exists because the class is invisible at
the site of the failure** — the test that breaks is never the test that broke it, and reviewing the
diff that introduced the unlocked `set_var` would not have shown anything either. A missing lock is
a line in a diff only if something looks for it.

RULE ONE — the lock. Per test-scope function body (brace-matched):

  · a body that calls `set_var`/`remove_var` must bind a guard from `env_lock()` (or lock
    `ENV_LOCK` directly);
  · `let _ = env_lock();` is a finding of its own — `_` is not a binding, so the guard is dropped
    on the line it is taken and the test runs unlocked while LOOKING locked. That is worse than no
    lock, because it reads as done.

Helpers are the interesting case: a `#[cfg(test)]` fn that is not itself `#[test]` cannot take the
lock without deadlocking a caller that already holds it. Those are resolved rather than waved
through — a helper is accepted when EVERY `#[test]` in its file that calls it holds the lock, and
the checker says so by name. A helper nothing calls, or one called from an unlocked test, is a
finding against the caller.

`docs/env-lock.toml` is the exemption list, in the same shape as `docs/sources.toml`: a reviewed
allow-list, generated from the code with `--update`, where every entry carries a reason. An
exemption for a site that no longer exists is a finding too — a stale exemption is a permission
nobody granted.

RULE TWO — the restore (`restore_findings` below, and read its comment for what it deliberately
does NOT check). Per `#[test]`, with the same-file helpers it reaches folded in: a variable the
test sets by a literal name must also be removed by a literal name, or read before being set — the
save-and-put-back shape. `docs/env-restore.toml` is the debt of 115 scopes that were not, on the
day the rule was written; a finding not in that file fails the build, and an entry in it that no
longer leaks fails the build too, so the list cannot outlive the debt.

`src/testutil.rs::EnvPins` is the way out of the debt, and the reason a trailing `remove_var` is
not: it restores from `Drop`, so a test that PANICS restores as well as one that passes. The 23
trailing `remove_var`s that repaired SKEIN-696 all sat on the last line of a test, which a failing
assertion unwinds straight past — so those tests leaked exactly when they failed. 26 tests in
`src/repos.rs`, `src/apiauth.rs` and `src/machine.rs` are converted; a converted test sets nothing
by hand and so drops out of rule two entirely, which is why the counts on the last line are
reported separately. An `env_pins()` call counts as touching the environment for rule one, so
converting a test does not quietly drop it out of the LOCK check.

  python3 tools/env-lock-check.py                  check both rules
  python3 tools/env-lock-check.py --show           every env-touching test scope and its verdict
  python3 tools/env-lock-check.py --show-restore   every #[test] judged by rule two, and how
  python3 tools/env-lock-check.py --update         rewrite the exemption list from the code
  python3 tools/env-lock-check.py --update-restore prune docs/env-restore.toml; it never adds
"""

import os, re, sys, tomllib

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import rustcut  # noqa: E402 — the one cutter every gate shares, self-checked at import

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SPEC = os.path.join(ROOT, "docs", "env-lock.toml")
DEBT = os.path.join(ROOT, "docs", "env-restore.toml")

# Where test code lives. `tests/` is test code in its entirety; `src/` and `warden/src/` are test
# code only inside `#[cfg(test)]`.
CRATE_DIRS = [os.path.join(ROOT, "src"), os.path.join(ROOT, "warden", "src")]
TEST_DIRS = [os.path.join(ROOT, "tests")]

# The write, not the read. `env::var` is fine from anywhere: it is the mutation that is shared.
# Spelled without a module prefix because all three of `env::set_var`, `std::env::set_var` and a
# bare `set_var` after `use std::env::set_var` appear in this tree.
#
# `env_pins()` is here so that converting a test to `src/testutil.rs::EnvPins` does not take it out
# of rule one. A converted test writes `env.set("SKEIN_HOME", …)` and calls no `set_var` at all, so
# without this line it would stop being an env-touching scope, stop being asked for the lock, and
# read as fixed while having quietly left the gate.
TOUCH = re.compile(r"\b(?:remove_var|set_var|env_pins)\s*\(")

# A guard is a BINDING. `let _g = …` and `let _ = …` differ by one character and by the entire
# lifetime of the lock, which is exactly why this is checked mechanically.
GUARD = re.compile(r"\blet\s+(?:mut\s+)?(?P<name>[A-Za-z_][A-Za-z0-9_]*)\s*=\s*[^;]*?\b(?:env_lock\s*\(\s*\)|ENV_LOCK\s*\.\s*lock\s*\(\s*\))")
DROPPED = re.compile(r"\blet\s+_\s*=\s*[^;]*?\b(?:env_lock\s*\(\s*\)|ENV_LOCK\s*\.\s*lock\s*\(\s*\))")


def uncommented(text):
    """Source with every comment blanked to spaces — `rustcut.blanked`, and not a local copy.

    Length-preserving on purpose: every offset this file computes (function bodies, brace matches,
    the spans it reports) indexes the ORIGINAL source, so a transform that shortens lines silently
    slides every position after the first comment. That is why `rustcut.uncommented`, which is
    newline-preserving but not length-preserving, is the wrong one here.

    The copy that used to live at this line tracked quotes with a boolean flipped on every `"`,
    one line at a time. A raw string is invisible to that: `src/bin/skein-server.rs:5160` is a
    `r#"…"…https://…"#` fixture with an EVEN number of quotes before the `//`, so the tracker
    believed it was outside a string, blanked to end of line, and swallowed the closing `"#`. The
    braces unbalanced, `mod review_routes` (5123-5743) closed at 5465, and 278 lines of tests —
    including one that calls `set_var("SKEIN_REVIEW_AI", …)` — became invisible to this gate. That
    is WTS-4 again, in the one function the port left behind.
    """
    return rustcut.blanked(text)


def match_brace(text, start):
    """Index one past the `}` closing the first `{` at or after `start`, or -1.

    Delegated to `rustcut`, which steps over strings, chars and comments. The counter that lived
    here did not: `src/fleet.rs`'s test module opens with a shell fixture full of braces, so the
    region closed hundreds of lines early and this gate reported *no env-touching scope at all* in
    the file that has 212 of them (`grep -c 'set_var\\|remove_var' src/fleet.rs`). `prq.rs` and
    `prwork.rs` were invisible the same way (WTS-4). The `--show` count that `.config/nextest.toml`
    calls "204 … where grep says 366" was this, not helpers.
    """
    return rustcut.match_brace(text, start)


def test_regions(text, whole_file):
    """[(start, end)] of the parts of `text` that are test code.

    A `#[cfg(test)]` item ends at its block's `}` — or at its `;` when it has no block, which is
    what a `#[cfg(test)] const` looks like. Reading on to "the next `{`" would take the production
    function after it for a test scope (WTS-9, the same cut from the other side).
    """
    if whole_file:
        return [(0, len(text))]
    return rustcut.cfg_test_spans(text)


FN = re.compile(r"^(?P<indent>[ \t]*)(?:pub(?:\([^)]*\))?\s+)?(?:async\s+)?fn\s+(?P<name>[A-Za-z_][A-Za-z0-9_]*)", re.M)


def functions(text, lo, hi):
    """Every `fn` whose body lies inside [lo, hi), innermost last, with its attributes."""
    out = []
    for m in FN.finditer(text, lo, hi):
        end = match_brace(text, m.end())
        if end < 0 or end > hi:
            continue
        # The attribute block immediately above: walk back over contiguous `#[...]` lines.
        head = text[:m.start()].rstrip("\n").split("\n")
        attrs = []
        for line in reversed(head):
            s = line.strip()
            if s.startswith("#[") or s.startswith("#!["):
                attrs.append(s)
            elif s == "" or s.startswith("///") or s.startswith("//"):
                continue
            else:
                break
        out.append({
            "name": m.group("name"),
            "attrs": attrs,
            "start": m.start(),
            "body_start": text.find("{", m.end()),
            "end": end,
            "line": text.count("\n", 0, m.start()) + 1,
        })
    return out


def unit_name(path):
    rel = os.path.relpath(path, ROOT)
    return rel[:-3] if rel.endswith(".rs") else rel


def scan_file(path, whole_file):
    """[finding-or-scope dicts] for one file."""
    raw = open(path, encoding="utf-8").read()
    text = uncommented(raw)
    unit = unit_name(path)
    scopes = []
    for lo, hi in test_regions(text, whole_file):
        fns = functions(text, lo, hi)
        for fn in fns:
            body = text[fn["body_start"]:fn["end"]]
            # Only the part of the body that is not itself a nested fn: a nested fn is its own scope
            # and is reported under its own name.
            inner = [g for g in fns if g is not fn and g["start"] > fn["start"] and g["end"] <= fn["end"]]
            own = body
            for g in sorted(inner, key=lambda g: -g["start"]):
                own = own[: g["start"] - fn["body_start"]] + own[g["end"] - fn["body_start"]:]
            if not TOUCH.search(own):
                continue
            scopes.append({
                "unit": unit,
                "fn": fn["name"],
                "line": fn["line"],
                "is_test": any(a.startswith("#[test]") or "::test]" in a or a.startswith("#[tokio::test") for a in fn["attrs"]),
                "guard": bool(GUARD.search(own)),
                "dropped": bool(DROPPED.search(own)),
                "touches": len(TOUCH.findall(own)),
                "text": text,
                "span": (lo, hi),
                "fns": fns,
            })
    return scopes


def is_test_fn(fn):
    return any(
        a.startswith("#[test]") or "::test]" in a or a.startswith("#[tokio::test")
        for a in fn["attrs"]
    )


def callers_of(scope):
    """`#[test]` fns that reach this helper, split by whether they hold the lock.

    Transitive, because helpers call helpers: `src/review.rs`'s `drafting_fixture_for` is reached
    only through two other fixtures, and a one-hop search reported it as "called by nothing" — a
    verdict that would have sent somebody looking for dead code instead of at the lock.
    """
    text, fns = scope["text"], scope["fns"]
    bodies = {fn["name"]: text[fn["body_start"]:fn["end"]] for fn in fns}
    reaching, frontier = set(), {scope["fn"]}
    while frontier:
        target = frontier.pop()
        call = re.compile(r"\b" + re.escape(target) + r"\s*\(")
        for fn in fns:
            if fn["name"] in reaching or fn["name"] == target:
                continue
            if call.search(bodies[fn["name"]]):
                reaching.add(fn["name"])
                if not is_test_fn(fn):
                    frontier.add(fn["name"])
    locked, unlocked = [], []
    for fn in fns:
        if fn["name"] in reaching and is_test_fn(fn):
            (locked if GUARD.search(bodies[fn["name"]]) else unlocked).append(fn["name"])
    return locked, unlocked


def per_file_counts(scopes):
    counts = {}
    for s in scopes:
        counts[s["unit"]] = counts.get(s["unit"], 0) + 1
    return counts


def rust_files():
    """(path, whole_file) for every Rust file both rules read, in a stable order."""
    for d in CRATE_DIRS:
        for base, _, files in os.walk(d):
            for f in sorted(files):
                if f.endswith(".rs"):
                    yield os.path.join(base, f), False
    for d in TEST_DIRS:
        for base, _, files in os.walk(d):
            for f in sorted(files):
                if f.endswith(".rs"):
                    yield os.path.join(base, f), True


def collect():
    scopes = []
    for path, whole_file in rust_files():
        scopes += scan_file(path, whole_file=whole_file)
    return scopes


def verdict(scope, per_file):
    """(ok, note). `ok` is False when this scope needs an exemption or a fix.

    `per_file` counts the env-touching scopes in the same file, which is what decides the question
    for `tests/*.rs`: cargo builds **one binary per integration test file**, so those tests share a
    process with each other and with nothing else. A file whose only env-touching scope is this one
    has nothing in its process to race against, and demanding a lock there would be a ritual. Two
    or more in one file is the same hazard as the lib, in a smaller process.
    """
    if scope["dropped"]:
        return False, (
            "binds the guard to `_`, so it is dropped on the line it is taken — the test runs "
            "unlocked while reading as locked"
        )
    if scope["guard"]:
        return True, "holds the lock"
    if scope["unit"].startswith("tests/") and per_file == 1:
        return True, (
            "is the only env-touching scope in its own test binary — cargo gives every "
            "tests/*.rs its own process, so nothing here can race it"
        )
    if scope["is_test"]:
        return False, "sets env vars and never takes `env_lock()`"
    locked, unlocked = callers_of(scope)
    if unlocked:
        return False, (
            "is a helper that sets env vars, called by "
            + ", ".join(f"`{c}`" for c in sorted(unlocked))
            + " which do not hold the lock"
        )
    if locked:
        return True, (
            "is a helper; every #[test] that calls it holds the lock ("
            + ", ".join(sorted(locked))
            + ")"
        )
    return False, (
        "is a helper that sets env vars and no #[test] in its file calls it — nothing here can "
        "prove a caller holds the lock"
    )


def key(scope):
    return f"{scope['unit']}::{scope['fn']}"


# ---------------------------------------------------------------------------------------------
# Rule two: the environment a test found is the environment it leaves
# ---------------------------------------------------------------------------------------------

# `set_var("NAME", …)` / `remove_var("NAME")` with the name captured — a quoted literal, or None
# when it is anything else (a variable, a `format!`, a loop over a list).
NAMED = re.compile(r'\b(?P<op>set_var|remove_var)\s*\(\s*(?P<arg>"(?:[^"\\]|\\.)*"|[^,()"]*)')

# A read of the same name, which is how the save-and-put-back shape is recognised. `env::var("PATH")`
# followed by `set_var("PATH", real)` restores without ever removing, and there are 31 of them.
READ = r'\bvar(?:_os)?\s*\(\s*"%s"'


def named_touches(body):
    """[(op, name-or-None)] for every `set_var`/`remove_var` in `body`."""
    out = []
    for m in NAMED.finditer(body):
        arg = m.group("arg").strip()
        literal = arg.startswith('"') and arg.endswith('"') and len(arg) >= 2
        out.append((m.group("op"), arg[1:-1] if literal else None))
    return out


def reached(name, fns, bodies, seen=None):
    """Names of the same file's non-`#[test]` fns that `name` reaches, transitively.

    The mirror of `callers_of`: rule one asks who calls a helper, rule two asks what a test calls,
    because a fixture and its teardown are usually two helpers and the pairing only exists across
    the pair. `tests/review_queue.rs::setup` is the shape — twenty tests, one fixture, and the
    fixture is where every `set_var` lives.
    """
    if seen is None:
        seen = set()
    for fn in fns:
        n = fn["name"]
        if n == name or n in seen or is_test_fn(fn):
            continue
        if re.search(r"\b" + re.escape(n) + r"\s*\(", bodies.get(name, "")):
            seen.add(n)
            reached(n, fns, bodies, seen)
    return seen


def restore_findings():
    """{scope key: [variable names it sets and never puts back]}, plus how much went unjudged.

    Unit of judgement is the `#[test]`, not the function body, with every same-file helper it
    reaches folded into one blob — nested `fn`s included, since a fixture that restores from `Drop`
    keeps its `remove_var`s in a nested `impl Drop` (`tests/merge_train_shape.rs::empty_home`).

    ## What this deliberately does NOT check, and why

    The rule is built to under-report. Taken naively it names 206 scopes, of which 91 are correct
    code; a gate that is wrong 44% of the time gets switched off, and then the 115 real ones are not
    checked either. So every shape below is skipped rather than guessed at, and the number it is
    skipping is printed on every ordinary run rather than buried here.

      · **A `remove_var` whose name is not a literal disables the whole scope.** `for key in
        ["GH_TOKEN", "SKEIN_GITHUB_API", "SKEIN_HOME"] { remove_var(key) }` is the tidiest teardown
        in this tree and appears in `src/prq/fixtures.rs`, `src/review/testkit.rs` and 82 more
        scopes; nothing textual can say which names it removes, and reading it as removing NOTHING
        is where 82 of the 91 false positives came from — the other nine are the save-and-put-back
        shape below. So a scope containing one is not judged at all. That is the largest hole by far, and it is where the second half of SKEIN-693 lives:
        `review::scope::your_own_pull_requests_are_read_and_reviewed_in_one_call` never calls its
        teardown, but a sibling in the same blob does, and this rule cannot tell them apart.
      · **A helper in another file is invisible.** Resolution is by name within one file, so a test
        whose fixture lives in `src/review/testkit.rs` while the test is in `src/review/scope.rs`
        shows no touch and is not judged. Following that would need a crate-wide call graph with
        `use` resolution, which this deliberately textual gate does not have.
      · **A `set_var` whose name is not a literal is not paired.** Six of them here. Only the name
        is lost, so this under-reports and never invents a finding.
      · **Order is not checked.** `remove_var("X"); set_var("X", v);` reads as paired and leaks.
        Checking "the last touch of X must be a remove" instead would report every one of the 31
        save-and-put-back sites — `src/knock.rs` restores through `match before { Some(v) =>
        set_var(…), None => remove_var(…) }`, and that is correct code. Pairing by name keeps those
        silent at the cost of this one.
      · **Nothing is checked about a value.** A test that pins `$SKEIN_HOME` to the wrong directory
        and dutifully removes it afterwards passes.

    ## What it does NOT skip, and this is the difference from rule one

    Rule one waves through a `tests/*.rs` file with a single env-touching scope: cargo gives every
    integration file its own process, so a lone writer has nothing to race. **That argument does not
    transfer.** Racing needs two writers; being answered out of somebody else's fixture needs one
    writer and one reader, and `tests/review_queue.rs` is exactly that — one `setup` helper, twenty
    tests, and any test there that stopped calling `setup` would read the last one's `$SKEIN_HOME`.
    So those 27 scopes are judged here and exempt there, on purpose.
    """
    findings, judged, skipped, pinned = {}, 0, 0, 0
    for path, whole_file in rust_files():
        text = uncommented(open(path, encoding="utf-8").read())
        unit = unit_name(path)
        for lo, hi in test_regions(text, whole_file):
            fns = functions(text, lo, hi)
            # Outermost only: a nested fn is part of the blob of the one that encloses it, not a
            # scope of its own — the opposite of what rule one wants.
            top = [
                f
                for f in fns
                if not any(g is not f and g["start"] < f["start"] and g["end"] >= f["end"] for g in fns)
            ]
            bodies = {f["name"]: text[f["body_start"]:f["end"]] for f in fns}
            for fn in top:
                if not is_test_fn(fn):
                    continue
                blob = bodies[fn["name"]] + "".join(
                    bodies[h] for h in sorted(reached(fn["name"], top, bodies))
                )
                if "env_pins(" in blob:
                    # Converted: `EnvPins` restores from `Drop`, so there is no pairing to check.
                    # Counted rather than ignored, because a rule whose denominator shrinks as the
                    # debt is paid off looks like the rule losing its grip.
                    pinned += 1
                touched = named_touches(blob)
                if not touched:
                    continue
                judged += 1
                if any(op == "remove_var" and n is None for op, n in touched):
                    skipped += 1
                    continue
                sets = {n for op, n in touched if op == "set_var" and n}
                removed = {n for op, n in touched if op == "remove_var" and n}
                leaked = [
                    n
                    for n in sorted(sets - removed)
                    if not re.search(READ % re.escape(n), blob)
                ]
                if leaked:
                    findings[f"{unit}::{fn['name']}"] = leaked
    return findings, judged, skipped, pinned


def load_debt():
    if not os.path.exists(DEBT):
        return {}
    with open(DEBT, "rb") as f:
        return tomllib.load(f).get("leaks", {})


DEBT_HEAD = '''# Tests that set an environment variable and never put it back — the debt of the day rule two
# was written (SKEIN-701). Read by `tools/env-lock-check.py`.
#
# Every row is a DEFECT, not a permission. A test here leaks a process-global variable into
# whatever runs after it in the same process, which is how SKEIN-696 happened: `src/repos.rs` left
# `$SKEIN_FLEET_ROOT` set, a later test read it instead of failing for want of a pin of its own, and
# the suite was green because both defects were present. The gate fails on a leak that is NOT
# listed here, and on a row here that no longer leaks — so the list can only shrink, and it cannot
# outlive the debt.
#
# **The fix is `src/testutil.rs::env_pins()`, not a trailing `remove_var`.** A `remove_var` on the
# last line of a test is unwound past by a failing assertion, so a test repaired that way restores
# the environment when it passes and leaks when it fails. `EnvPins` restores from `Drop`, on every
# path out. `src/repos.rs`, `src/apiauth.rs` and `src/machine.rs` are converted; the rest are not.
#
# `python3 tools/env-lock-check.py --update-restore` prunes rows that no longer leak. **It never
# adds one** once this file exists, so a new leak is a build failure rather than a regenerated file,
# and getting it into this list means writing the row by hand and defending it in the review. The
# one thing it will write in full is a file that is not there at all, which is how this snapshot was
# taken and is reproducible with `rm docs/env-restore.toml && python3 tools/env-lock-check.py
# --update-restore`.
#
# Nothing here stops somebody doing exactly that to launder a new leak. It is a whole-file rewrite
# in a diff, which is the visibility this list exists for; there is no machine check behind it.
#
# `vars` is the exact set of names the scope leaks. A scope that starts leaking a SECOND variable is
# a fresh finding even though its row is already here.
'''


def render_debt(findings):
    out = [DEBT_HEAD]
    for k in sorted(findings):
        out.append('[leaks."%s"]' % k)
        out.append("vars = [%s]" % ", ".join('"%s"' % v for v in findings[k]))
        out.append("")
    return "\n".join(out).rstrip() + "\n"


def check_restore(findings, debt):
    """Problems from rule two: an unlisted leak, a stale row, and a row that leaks something new."""
    problems = []
    for k in sorted(findings):
        row = debt.get(k)
        listed = set(row.get("vars", [])) if row else set()
        fresh = sorted(set(findings[k]) - listed)
        if row is None:
            problems.append(
                f"env-lock-check: `{k}` sets {', '.join('$' + v for v in findings[k])} and never "
                f"puts {'them' if len(findings[k]) > 1 else 'it'} back\n"
                f"                rule: a variable a test sets outlives the test, and answers the "
                f"next one in the process that pinned none of its own (SKEIN-696). Pin it with "
                f"`env_pins()` (src/testutil.rs), which restores from `Drop` and so survives a "
                f"failing assertion — a trailing `remove_var` does not."
            )
        elif fresh:
            problems.append(
                f"env-lock-check: `{k}` now also leaks {', '.join('$' + v for v in fresh)}\n"
                f"                rule: docs/env-restore.toml records what this scope leaked on "
                f"the day it was written; a new name is a new defect, not covered by the old row"
            )
    for k in sorted(set(debt) - set(findings)):
        problems.append(
            f"env-lock-check: docs/env-restore.toml records `{k}` as leaking, and it no longer "
            f"does — or no longer exists\n"
            f"                rule: delete the row (`--update-restore`). A debt list that outlives "
            f"its debt stops being read."
        )
    return problems


def load_spec():
    if not os.path.exists(SPEC):
        return {}
    with open(SPEC, "rb") as f:
        return tomllib.load(f).get("exempt", {})


def render(bad):
    out = [
        "# Test scopes that touch process-global env vars without holding `env_lock()`.",
        "# Read by `tools/env-lock-check.py`, which fails the build on an undeclared one AND on an",
        "# entry here that no longer names a real site.",
        "#",
        "# Every entry needs a reason. \"It has always been like that\" is not one: `cargo test` runs",
        "# a crate's tests in one process, so an unlocked `set_var` is a write into whatever else is",
        "# running, and the test that fails is never the test that broke it (SKEIN-307).",
        "",
    ]
    for k in sorted(bad):
        out.append("[exempt.\"%s\"]" % k)
        # Deliberately empty: `--update` writes the skeleton, and the CHECK still fails until a
        # person has said why. An exemption that arrives pre-justified by a generator is one nobody
        # read.
        out.append('reason = ""')
        out.append("")
    return "\n".join(out).rstrip() + "\n"


def main():
    scopes = collect()
    counts = per_file_counts(scopes)
    judged = [(s, *verdict(s, counts[s["unit"]])) for s in scopes]
    if "--show" in sys.argv:
        for s, ok, note in sorted(judged, key=key.__call__ if False else (lambda t: key(t[0]))):
            print(f"{'ok  ' if ok else 'BAD '} {key(s):<60} {s['touches']:>3} touch(es)  {note}")
        print(f"\n{len(scopes)} env-touching test scopes, {sum(1 for _, ok, _ in judged if not ok)} unlocked")
        return 0

    leaks, tests_judged, unjudged, pinned = restore_findings()
    if "--show-restore" in sys.argv:
        for k in sorted(leaks):
            print(f"BAD  {k:<70} leaks {', '.join('$' + v for v in leaks[k])}")
        print(
            f"\n{tests_judged} #[test]s set or remove a variable by hand, {unjudged} of them not "
            f"judged (a `remove_var` whose name is not a literal), {len(leaks)} leaking; "
            f"{pinned} more pin through `env_pins()` and restore by construction"
        )
        return 0

    bad = {key(s): note for s, ok, note in judged if not ok}
    if "--update" in sys.argv:
        open(SPEC, "w", encoding="utf-8").write(render(bad))
        print(f"wrote {os.path.relpath(SPEC, ROOT)} ({len(bad)} entries)")
        return 0

    if "--update-restore" in sys.argv:
        # Prune, never add — see DEBT_HEAD. A leak that is not already recorded stays a build
        # failure, so this command cannot be used to make one go away. The one exception is the
        # file's first writing, when there is nothing to prune and nothing to launder: that is how
        # the snapshot below was taken, and deleting the file to get it back is a whole-file rewrite
        # in the diff rather than a quiet one.
        was = load_debt()
        if not os.path.exists(DEBT):
            kept = dict(leaks)
        else:
            kept = {k: leaks[k] for k in sorted(set(was) & set(leaks))}
        open(DEBT, "w", encoding="utf-8").write(render_debt(kept))
        print(
            f"wrote {os.path.relpath(DEBT, ROOT)} ({len(kept)} row(s), "
            f"{len(set(was) - set(kept))} pruned; {len(set(leaks) - set(kept))} unrecorded leak(s) "
            f"left for the check to report)"
        )
        return 0

    spec = load_spec()
    problems = []
    for k in sorted(bad):
        entry = spec.get(k)
        if entry is None:
            problems.append(
                f"env-lock-check: `{k}` {bad[k]}\n"
                f"                rule: take `env_lock()` first — one lock for the whole crate, "
                f"bound to a NAME so it lives to the end of the test (src/testutil.rs). If it "
                f"genuinely cannot, add `{k}` to docs/env-lock.toml with the reason."
            )
        elif not str(entry.get("reason", "")).strip():
            problems.append(
                f"env-lock-check: docs/env-lock.toml exempts `{k}` with no reason\n"
                f"                rule: an exemption nobody can read is one nobody can retire"
            )
    for k in sorted(set(spec) - set(bad)):
        problems.append(
            f"env-lock-check: docs/env-lock.toml exempts `{k}`, which now holds the lock or no "
            f"longer exists\n"
            f"                rule: delete the line — a stale exemption is a permission nobody "
            f"granted"
        )
    debt = load_debt()
    problems += check_restore(leaks, debt)
    for p in problems:
        print(p + "\n")
    if problems:
        print(
            f"{len(problems)} problem(s). `python3 tools/env-lock-check.py --show` lists every "
            f"env-touching test scope and how it was judged, `--show-restore` every #[test] that "
            f"leaks one."
        )
        return 1
    touches = sum(s["touches"] for s in scopes)
    print(
        f"every env-touching test holds the lock "
        f"({touches} set_var/remove_var calls across {len(scopes)} scopes, "
        f"{len(spec)} declared exemption(s))"
    )
    # Said separately and with its own denominator, because the two rules cover different ground and
    # reporting them as one number is how "the lock is held" came to be read as "the environment is
    # put back". `unjudged` is on this line for the same reason: a rule that skips 84 of the 534
    # tests it looks at should say so every run, not in a comment.
    print(
        f"every leak of an environment variable is a recorded one "
        f"({len(debt)} in docs/env-restore.toml, out of {tests_judged} #[test]s that set one by "
        f"hand — {unjudged} of those not judged, having a `remove_var` whose name is not a "
        f"literal; {pinned} more pin through `env_pins()` and restore by construction)"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
