#!/usr/bin/env python3
"""The env lock, made checkable: no test touches a process-global env var without holding it.

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

What it checks, per test-scope function body (brace-matched):

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

  python3 tools/env-lock-check.py           check
  python3 tools/env-lock-check.py --show    print every env-touching test scope and its verdict
  python3 tools/env-lock-check.py --update  rewrite the exemption list from the code
"""

import os, re, sys, tomllib

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SPEC = os.path.join(ROOT, "docs", "env-lock.toml")

# Where test code lives. `tests/` is test code in its entirety; `src/` and `warden/src/` are test
# code only inside `#[cfg(test)]`.
CRATE_DIRS = [os.path.join(ROOT, "src"), os.path.join(ROOT, "warden", "src")]
TEST_DIRS = [os.path.join(ROOT, "tests")]

# The write, not the read. `env::var` is fine from anywhere: it is the mutation that is shared.
# Spelled without a module prefix because all three of `env::set_var`, `std::env::set_var` and a
# bare `set_var` after `use std::env::set_var` appear in this tree.
TOUCH = re.compile(r"\b(?:remove_var|set_var)\s*\(")

# A guard is a BINDING. `let _g = …` and `let _ = …` differ by one character and by the entire
# lifetime of the lock, which is exactly why this is checked mechanically.
GUARD = re.compile(r"\blet\s+(?:mut\s+)?(?P<name>[A-Za-z_][A-Za-z0-9_]*)\s*=\s*[^;]*?\b(?:env_lock\s*\(\s*\)|ENV_LOCK\s*\.\s*lock\s*\(\s*\))")
DROPPED = re.compile(r"\blet\s+_\s*=\s*[^;]*?\b(?:env_lock\s*\(\s*\)|ENV_LOCK\s*\.\s*lock\s*\(\s*\))")


def uncommented(text):
    """Source with line comments blanked to spaces — same length, same lines, same offsets.

    Length-preserving on purpose: every offset this file computes (function bodies, brace matches,
    the spans it reports) indexes the ORIGINAL source, so a transform that shortens lines silently
    slides every position after the first comment. A `//` inside a string literal is left alone,
    because blanking through the closing quote of `format!("{}//{}", …)` would eat a brace and
    unbalance the match.
    """
    out = []
    for line in text.split("\n"):
        cut, quoted, i = None, False, 0
        while i < len(line) - 1:
            c = line[i]
            if c == "\\":
                i += 2
                continue
            if c == '"':
                quoted = not quoted
            elif c == "/" and line[i + 1] == "/" and not quoted:
                cut = i
                break
            i += 1
        out.append(line if cut is None else line[:cut] + " " * (len(line) - cut))
    return "\n".join(out)


def match_brace(text, start):
    """Index one past the `}` closing the first `{` at or after `start`, or -1."""
    brace = text.find("{", start)
    if brace < 0:
        return -1
    depth, i = 0, brace
    while i < len(text):
        c = text[i]
        if c == "{":
            depth += 1
        elif c == "}":
            depth -= 1
            if depth == 0:
                return i + 1
        i += 1
    return -1


def test_regions(text, whole_file):
    """[(start, end)] of the parts of `text` that are test code."""
    if whole_file:
        return [(0, len(text))]
    spans, pos = [], 0
    while True:
        m = re.compile(r"^#\[cfg\(test\)\]\s*$", re.M).search(text, pos)
        if not m:
            return spans
        end = match_brace(text, m.end())
        if end < 0:
            return spans
        spans.append((m.start(), end))
        pos = end


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


def collect():
    scopes = []
    for d in CRATE_DIRS:
        for base, _, files in os.walk(d):
            for f in sorted(files):
                if f.endswith(".rs"):
                    scopes += scan_file(os.path.join(base, f), whole_file=False)
    for d in TEST_DIRS:
        for base, _, files in os.walk(d):
            for f in sorted(files):
                if f.endswith(".rs"):
                    scopes += scan_file(os.path.join(base, f), whole_file=True)
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

    bad = {key(s): note for s, ok, note in judged if not ok}
    if "--update" in sys.argv:
        open(SPEC, "w", encoding="utf-8").write(render(bad))
        print(f"wrote {os.path.relpath(SPEC, ROOT)} ({len(bad)} entries)")
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
    for p in problems:
        print(p + "\n")
    if problems:
        print(
            f"{len(problems)} problem(s). `python3 tools/env-lock-check.py --show` lists every "
            f"env-touching test scope and how it was judged."
        )
        return 1
    touches = sum(s["touches"] for s in scopes)
    print(
        f"every env-touching test holds the lock "
        f"({touches} set_var/remove_var calls across {len(scopes)} scopes, "
        f"{len(spec)} declared exemption(s))"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
