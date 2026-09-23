#!/usr/bin/env python3
"""Three rules about the process-global environment: hold the lock, put it back, put it back on every exit.

They are separate guarantees and this file checks all three — for a year it checked only the first
and the second was read off it, and the third (SKEIN-723) is what was still missing once the second
existed. **Holding the lock protects a CONCURRENT test; restoring protects a LATER one; restoring
through `Drop` rather than a trailing statement protects a later test from a FAILING one** — a test
can do any subset of the three while doing none of the others. The first gap is SKEIN-696:
`src/repos.rs` pinned `$SKEIN_FLEET_ROOT` under the lock, never removed it, and the pin outlived the
test and answered a later one that pinned none of its own. Two defects cancelled out into a green
suite; `tools/alone-check.py` saw it and this gate did not, because the lock was held. The second gap
is SKEIN-723, and RULE TWO's own doc below already names it before RULE THREE existed to check it:
the 23 trailing `remove_var`s that repaired SKEIN-696 all sat on the last line of a test, restoring
the environment when the test PASSED and leaking it into every later test in the process when the
test FAILED — the exact shape a failing assertion is supposed to be caught by, silently making the
suite's own signal worse the more of it there was to catch.

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

WHERE TEST CODE IS. Two shapes, and for a year this file saw only one. `#[cfg(test)] mod tests { … }`
carries its attribute in the file it is in, and `rustcut.cfg_test_spans` finds it. `#[cfg(test)] mod
testkit;` carries its attribute in the PARENT, so the file it names holds no attribute anywhere and
the cutter — which reads one file at a time — returns nothing for it. `src/review/testkit.rs` sat in
that blind spot with 11 `set_var` calls in it, in no scope, judged by neither rule, while this gate
printed a clean tree (SKEIN-871). `rustcut.test_only_files` derives those files from the `mod`
declarations in the tree and refuses to run when it derives none; it is never a list of paths. It
lives in `rustcut` because `fleet-pin-check.py` needed the same answer and had grown a second,
worse derivation of it (SKEIN-894).

RULE ONE — the lock. Per test-scope function body (brace-matched):

  · a body that calls `set_var`/`remove_var` must bind a guard from `env_lock()` (or lock
    `ENV_LOCK` directly);
  · `let _ = env_lock();` is a finding of its own — `_` is not a binding, so the guard is dropped
    on the line it is taken and the test runs unlocked while LOOKING locked. That is worse than no
    lock, because it reads as done;
  · a guard can also arrive from a helper in the same module that takes the lock and RETURNS it,
    which two `fresh_home()`s here do. `lock_providers` reads that off the RETURN TYPE — an `EnvGuard` can
    only have come from `env_lock()`, whose struct has private fields and one construction site —
    and remembers which slot of the returned tuple it is, so that
    `let (_, _home, _env) = fresh_home();` stays the finding above rather than becoming a pass.
    Both halves of this were SKEIN-895: without the first, three locked tests in `src/gitgate.rs`
    read as unlocked the moment `no_warden` joined TOUCH; without the second, the fix would have
    hidden the `_` hazard behind one level of indirection.

Helpers are the interesting case: a `#[cfg(test)]` fn that is not itself `#[test]` cannot take the
lock without deadlocking a caller that already holds it. Those are resolved rather than waved
through — a helper is accepted when EVERY `#[test]` in its MODULE that calls it holds the lock, and
the checker says so by name, with the file of any caller that is not beside it. A helper nothing
calls, or one called from an unlocked test, is a finding against the caller.

The module, not the file, since SKEIN-904. A fixture and the `#[test]` that uses it are siblings far
more often than housemates — `src/review/testkit.rs` holds the fixtures for `budget.rs`, `scope.rs`
and `visit.rs` — and the one-file search reported all three of those as "no #[test] in its file
calls it", which put three reviewed fixtures into `docs/env-lock.toml` carrying a caller list a
person had derived by hand because the tool could not. Those three rows are gone; `--show` prints
the derivation that replaced them. The radius is the module because that is the VISIBILITY —
`pub(super)` from `review::testkit` reaches exactly the files `rustcut.units` groups together — and
because a by-name search over a whole crate would mean nothing: `setup` is one function in a module
and a dozen in a crate.

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
assertion unwinds straight past — so those tests leaked exactly when they failed. `tests/*.rs` reach
the same guard through `tests/common/mod.rs`, which carries its own copy: a `#[cfg(test)]` module is
not in the library an integration binary links. A converted test sets nothing by hand and so drops
out of rule two entirely, which is why the counts on the last line are reported separately. An `env_pins()` call counts as touching the environment for rule one, so
converting a test does not quietly drop it out of the LOCK check.

RULE THREE — restore on every exit, not only the one that reaches the last line
(`trailing_findings` below). Rule two asks WHETHER a variable a test set by literal name was also
removed by literal name, anywhere in the same scope; it accepts a trailing `remove_var` as a repair
exactly as readily as a `Drop`-based one, because both read as "set, then removed, somewhere in this
blob" — so it could not see the SKEIN-696 shape even in hindsight, and would call the 23 sites that
caused it "restored". This rule asks a narrower, POSITIONAL question instead: per `#[test]`, does a
literal `remove_var("NAME")` in the test's OWN body (same-file helpers are not folded in — the
hazard is about order within one function's control flow, which a helper reached by name carries
none of) appear textually AFTER the last `assert!`/`assert_eq!`/`assert_ne!`/`panic!`/`.unwrap(`/
`.expect(`/`?` in that body? A `remove_var` inside a nested `impl Drop` runs from `Drop::drop` on
every exit including an unwind and is the fix, not the disease; one inside a closure literal does
not run when the closure is DEFINED, only if and when it is CALLED, so its textual position says
nothing about order — both are excluded by brace-matching their block and blanking it out first. A
`remove_var("NAME")` earned by the `None` arm of a save-and-put-back restore
(`match old { Some(v) => set_var(name, v), None => remove_var(name) }`) is excluded too, on the same
evidence rule two's own `READ` check uses for that shape: the code read `NAME`'s own prior value
before ever writing it, so it did not simply forget to put anything back. `docs/env-trailing.toml`
is the debt of 209 scopes that were not converted on the day the rule was written — the 4 named as
known sites in the tracker item (`src/tracking.rs`, `src/volume.rs`, `src/review/cache.rs`,
`src/prq/credentials.rs`) were converted instead, and are not in it. Same discipline as rule two's
debt: a finding not listed there fails the build, a row that no longer leaks fails the build too.

  python3 tools/env-lock-check.py                  check all three rules
  python3 tools/env-lock-check.py --show           every env-touching test scope and its verdict
  python3 tools/env-lock-check.py --show-restore   every #[test] judged by rule two, and how
  python3 tools/env-lock-check.py --show-trailing  every #[test] judged by rule three, and how
  python3 tools/env-lock-check.py --update         REWRITE docs/env-lock.toml from the code, with
                                                   EVERY reason blank and every comment gone —
                                                   it keeps nothing a person wrote. Snapshot the
                                                   file first and put the reasons back by hand
  python3 tools/env-lock-check.py --update-restore prune docs/env-restore.toml; it never adds
  python3 tools/env-lock-check.py --update-trailing prune docs/env-trailing.toml; it never adds
"""

import collections, os, re, sys, tomllib

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import rustcut  # noqa: E402 — the one cutter every gate shares, self-checked at import

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SPEC = os.path.join(ROOT, "docs", "env-lock.toml")
DEBT = os.path.join(ROOT, "docs", "env-restore.toml")
TRAIL_DEBT = os.path.join(ROOT, "docs", "env-trailing.toml")

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
#
# `no_warden()` (src/testutil.rs:226) is the same wrapper one level further up, and was missed for
# exactly as long as it existed: it pins `$SKEIN_WARDEN` at `127.0.0.1:1` through an `EnvPins` it
# builds and hands back, so a test whose only write to the environment is
# `let _w = crate::testutil::no_warden();` spells none of the three names above and was never asked
# for the lock at all (SKEIN-895). It has 36 callers.
#
# **This list is hand-kept and that is its own hazard** — it has now needed two entries, and each
# was added after the wrapper it names had been in the tree for a while. It is not derived because
# the honest derivation is "every fn that transitively reaches `env::set_var`", which is most of
# `src/testutil.rs` and reaches through `Command::env` into work this rule does not govern. What
# makes the list falsifiable instead is `src/testutil.rs`: `EnvPins`'s field is private and the
# struct is constructed in exactly one place, so a NEW wrapper has to be a fn in that file returning
# an `EnvPins`, and `env_pins(` in its body keeps it a finding here until it is named.
TOUCH = re.compile(r"\b(?:remove_var|set_var|env_pins|no_warden)\s*\(")

# A guard is a BINDING. `let _g = …` and `let _ = …` differ by one character and by the entire
# lifetime of the lock, which is exactly why this is checked mechanically.
GUARD = re.compile(r"\blet\s+(?:mut\s+)?(?P<name>[A-Za-z_][A-Za-z0-9_]*)\s*=\s*[^;]*?\b(?:env_lock\s*\(\s*\)|ENV_LOCK\s*\.\s*lock\s*\(\s*\))")
DROPPED = re.compile(r"\blet\s+_\s*=\s*[^;]*?\b(?:env_lock\s*\(\s*\)|ENV_LOCK\s*\.\s*lock\s*\(\s*\))")

# A helper that takes the lock and HANDS THE GUARD BACK. Its caller is under the lock for the whole
# of its body without ever spelling `env_lock()`, so the two regexes above — which read one body —
# cannot see it, and would report a locked test as unlocked.
#
# This tree has the shape twice and both are called `fresh_home`: `src/gitgate/testkit.rs:14` and
# `src/prq/store.rs:240`, each returning `(EnvGuard, TempDir, EnvPins)` and each destructured by its
# callers as `let (_lock, _home, _env) = fresh_home();`. It is the arrangement `src/prq/store.rs`'s
# own doc comment insists on — the guard FIRST so it drops LAST, after `$SKEIN_HOME` has stopped
# naming a directory that is about to be removed.
#
# **The guard's tuple slot, not just a yes/no**, because `let (_, _home, _env) = fresh_home();` is
# the `let _ = env_lock()` hazard one level of indirection away: one character, the whole lifetime
# of the lock, and a test that reads as locked while running unlocked. That has to stay a finding,
# so the slot the `EnvGuard` occupies is matched against the slot the caller bound.
#
# Reading the RETURN TYPE rather than the body is what makes this textual and still sound:
# `EnvGuard`'s fields are private and it is constructed in exactly one place, `env_lock()` at
# src/testutil.rs:33, so a fn that returns one took the lock to get it. It was restricted to the
# same file after `callers_of` stopped being (SKEIN-904), and the asymmetry was deliberate:
# `callers_of` widening can only ADD a caller to the set every one of which must be locked, so a
# name collision there turns into a finding, while this reads a name and BELIEVES it, so the same
# collision would turn into a pass. Splitting `src/gitgate.rs` put its `fresh_home()` in
# `testkit.rs` and its callers in five files (SKEIN-1100), so the radius is the module now — but
# only for a name defined ONCE among the module's test fns (`module_lock_providers`). A collision
# takes the name off the list, and what leaned on it is a finding again rather than a pass.
RETURNS_GUARD = re.compile(r"\bEnvGuard\b")


def _slots(text):
    """Top-level comma split of the inside of a tuple, or None if `text` is not one."""
    t = text.strip()
    if not t.startswith("(") or not t.endswith(")"):
        return None
    depth, out, last = 0, [], 0
    inner = t[1:-1]
    for i, c in enumerate(inner):
        if c in "([<{":
            depth += 1
        elif c in ")]>}":
            depth -= 1
        elif c == "," and depth == 0:
            out.append(inner[last:i])
            last = i + 1
    out.append(inner[last:])
    return [x.strip() for x in out]


def lock_providers(text, fns):
    """{name: slot} for same-file fns that return the lock guard.

    `slot` is the index of the `EnvGuard` in the tuple the fn returns, or None when the whole
    return value is the guard. A fn with more than one `EnvGuard` in its return type is left OUT
    rather than guessed at: nothing spells that today, and a wrong slot is a lock this gate would
    stop asking for.
    """
    out = {}
    for fn in fns:
        sig = text[fn["start"]:fn["body_start"]]
        if "->" not in sig:
            continue
        ret = sig.split("->", 1)[1]
        if not RETURNS_GUARD.search(ret):
            continue
        slots = _slots(ret)
        if slots is None:
            out[fn["name"]] = None
            continue
        at = [i for i, x in enumerate(slots) if RETURNS_GUARD.search(x)]
        if len(at) == 1:
            out[fn["name"]] = at[0]
    return out


def module_lock_providers(index, found):
    """{module: {name: slot}} — the lock providers a scope may be judged through, per module.

    **The module, not the file** (SKEIN-1100), for the reason SKEIN-904 gave `callers_of`: a
    fixture and the `#[test]` that uses it are siblings more often than housemates. When
    `src/gitgate.rs` became `src/gitgate/`, its `fresh_home()` went to `testkit.rs` and its callers
    to five files, and three tests that bind its guard read as never taking the lock.

    **But a provider is BELIEVED, not merely suspected**, which is why the file was the radius
    before: a name spelled twice would turn a wrong guess into a pass. So a name is a provider here
    only when it is defined exactly ONCE among the module's test fns. A second fn of that name
    anywhere in the module — a provider or not — takes it off the list, and every scope that leaned
    on it falls back to "never takes `env_lock()`", which is a finding a person reads.
    """
    out = {}
    for module, provided in found.items():
        defined = collections.Counter(fn["name"] for fn in index.get(module, []))
        out[module] = {name: slot for name, slot in provided.items() if defined[name] == 1}
    return out


def through_provider(own, providers):
    """(holds, dropped) for a body that gets its guard out of a lock-returning helper.

    `dropped` is the `let (_, home, pins) = fresh_home();` shape and a binding of the whole call to
    a bare `_`: the guard is released on the line it is taken, and the rest of the body runs
    unlocked while reading as locked.

    An unparsed pattern counts as NEITHER — so the scope falls through to "never takes
    `env_lock()`" and a person looks at it. A gate about a lock must not resolve its own confusion
    in the direction of green.
    """
    holds = dropped = False
    for name, slot in providers.items():
        call = re.compile(r"\blet\s+(?P<pat>[^=;]+?)\s*=\s*[^;]*?\b" + re.escape(name) + r"\s*\(")
        for m in call.finditer(own):
            pat = m.group("pat").strip()
            if pat.startswith("mut "):
                pat = pat[4:].strip()
            pat = pat.split(":", 1)[0].strip() if not pat.startswith("(") else pat
            if pat == "_":
                dropped = True
                continue
            if slot is None:
                holds = True
                continue
            bound = _slots(pat)
            if bound is None or len(bound) <= slot:
                continue
            if bound[slot] == "_":
                dropped = True
            else:
                holds = True
    return holds, dropped


def uncommented(text):
    """Source with every comment blanked to spaces — `rustcut.blanked`, and not a local copy.

    Length-preserving on purpose: every offset this file computes (function bodies, brace matches,
    the spans it reports) indexes the ORIGINAL source, so a transform that shortens lines silently
    slides every position after the first comment. That is why `rustcut.uncommented`, which is
    newline-preserving but not length-preserving, is the wrong one here.

    The copy that used to live at this line tracked quotes with a boolean flipped on every `"`,
    one line at a time. A raw string is invisible to that: `src/bin/skein-server/review.rs:985` is a
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


def unit_name(path, root=ROOT):
    rel = os.path.relpath(path, root)
    return rel[:-3] if rel.endswith(".rs") else rel


def test_binary(path, root=ROOT):
    """The integration binary cargo builds `path` into — `tests/<name>` — or None if it builds none.

    Cargo makes a test target of every `tests/<name>.rs` AND of every `tests/<name>/main.rs`, and
    in the second layout every other file under `tests/<name>/` is a module of that one binary,
    compiled into it and nothing else. `tests/fleet_launch/` and `tests/isolation_bwrap/` have
    been that layout since SKEIN-1109/1110, and until SKEIN-1114 this gate treated each of their
    files as a binary of its own: a lone scope in one file took the one-binary exemption while a
    sibling file of the same process held another, and a helper in `harness.rs` whose callers are
    in `path.rs` was looked for in `harness.rs` alone.

    A directory with no `main.rs` — `tests/common/` — is not a binary: it is a module each binary
    that declares `mod common;` compiles its own copy of, which is SKEIN-722's reason it gets no
    exemption, and that is unchanged.
    """
    rel = os.path.relpath(path, root).replace(os.sep, "/")
    parts = rel.split("/")
    if parts[0] != "tests" or len(parts) < 2:
        return None
    if len(parts) == 2:
        return f"tests/{parts[1][:-3]}" if parts[1].endswith(".rs") else None
    if os.path.isfile(os.path.join(root, "tests", parts[1], "main.rs")):
        return f"tests/{parts[1]}"
    return None


def scan_file(path, whole_file, module_providers=None, root=ROOT):
    """[finding-or-scope dicts] for one file.

    `module_providers` is `module_lock_providers` for the file's module; without it, a provider is
    looked for in the same test region only, which is all a lone file can say.
    """
    raw = open(path, encoding="utf-8").read()
    text = uncommented(raw)
    unit = unit_name(path, root)
    binary = test_binary(path, root)
    scopes = []
    for lo, hi in test_regions(text, whole_file):
        fns = functions(text, lo, hi)
        providers = lock_providers(text, fns) if module_providers is None else module_providers
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
            # A helper that hands the guard back does not itself hold one for its own body — it is
            # judged by `callers_of` like any other helper, and asking it about its own return
            # value would be circular.
            handed, released = through_provider(own, {k: v for k, v in providers.items() if k != fn["name"]})
            scopes.append({
                "unit": unit,
                "binary": binary,
                "fn": fn["name"],
                "line": fn["line"],
                "is_test": any(a.startswith("#[test]") or "::test]" in a or a.startswith("#[tokio::test") for a in fn["attrs"]),
                "guard": bool(GUARD.search(own)) or handed,
                # `dropped` outranks `guard` in `verdict`, and a guard released through a
                # helper is ranked the same way: a body that binds the slot properly ONCE and
                # drops it into `_` somewhere else is a finding, not a pass.
                "dropped": bool(DROPPED.search(own)) or released,
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

    Transitive, because helpers call helpers: `src/review/testkit.rs`'s `drafting_fixture_for` is
    reached only through two other fixtures, and a one-hop search reported it as "called by
    nothing" — a verdict that would have sent somebody looking for dead code instead of at the lock.

    **Over the whole MODULE, not one file** (SKEIN-904). The search used to build its body table
    from the fns of a single path, so a `pub(super)` fixture in `src/review/testkit.rs` whose
    `#[test]`s are in `budget.rs`, `scope.rs` and `visit.rs` came back "no #[test] in its file calls
    it — nothing here can prove a caller holds the lock", and three reviewed fixtures carried
    exemption rows that existed only because the tool could not look next door. A module is the
    right radius because it is the visibility: `pub(super)` from `review::testkit` reaches exactly
    the files `rustcut.units` groups together, and `modules_of()` is that grouping.

    **Resolution is still by NAME, and a name can be spelled twice in a module** — `src/testutil.rs`
    has three `fn drop`s, and `drop(` in a body is as likely to be the prelude's. So the sets below
    are the UNION over every fn of that name, which is deliberately a superset of the true callers:
    it can only add a caller, never lose one, so it turns a collision into a finding to read rather
    than a pass to trust. `src/prq/fixtures::drop` is the live example — eleven `#[test]`s call
    something spelled `drop(` and none of them mean this `Drop::drop`, and the row that exempts it
    was already arguing the real reason.

    A caller in another file of the module is named `<unit>::<fn>`, so the note says which file had
    to be opened to prove it; a caller beside the helper stays a bare name.
    """
    fns = scope["module_fns"]
    reaching, frontier, walked = set(), {scope["fn"]}, set()
    while frontier:
        target = frontier.pop()
        if target in walked:
            continue
        walked.add(target)
        call = re.compile(r"\b" + re.escape(target) + r"\s*\(")
        for fn in fns:
            if fn["name"] == target:
                continue
            if call.search(fn["body"]):
                reaching.add(fn["name"])
                if not is_test_fn(fn):
                    frontier.add(fn["name"])
    locked, unlocked = set(), set()
    for fn in fns:
        if fn["name"] in reaching and is_test_fn(fn):
            shown = fn["name"] if fn["unit"] == scope["unit"] else f"{fn['unit']}::{fn['name']}"
            (locked if GUARD.search(fn["body"]) else unlocked).add(shown)
    return sorted(locked), sorted(unlocked)


def process_of(scope):
    """What `verdict`'s count is per: the test binary a `tests/` scope is compiled into, else its
    file. For `tests/<name>.rs` the two are the same thing; for `tests/<name>/` they are not, and
    counting per file is what let two scopes in one process each read as the only one (SKEIN-1114).
    """
    return scope.get("binary") or scope["unit"]


def per_file_counts(scopes):
    counts = {}
    for s in scopes:
        counts[process_of(s)] = counts.get(process_of(s), 0) + 1
    return counts


def crate_files():
    """Every Rust file under `src/` and `warden/src/`, in a stable order."""
    return rustcut.crate_files(CRATE_DIRS)



# ---------------------------------------------------------------------------------------------
# Test code that is a FILE rather than a block
#
# `rustcut.cfg_test_spans` reads ONE file and finds the `#[cfg(test)]` attributes in it. A module
# declared `#[cfg(test)] mod testkit;` carries its attribute in the PARENT, so the file it names has
# no attribute anywhere in it, `cfg_test_spans` returns nothing for it, and every `set_var` it
# contains falls into no scope at all. That is SKEIN-871: `src/review/testkit.rs` held 11 env writes
# that neither rule had ever judged, while this gate printed a clean tree.
#
# The derivation itself is `rustcut.test_only_files`, not a copy here, because `fleet-pin-check.py`
# needed the same answer and grew its own — which got the parent directory, the string literals and
# the transitivity wrong, three ways at once (SKEIN-894). One cutter, one reader, one self-check
# that runs at import on every invocation of every gate.
# ---------------------------------------------------------------------------------------------

# "I could not check" is not "nothing is wrong": `rustcut.test_only_files` raises rather than
# answering an empty set, and `__main__` below exits 2 on it.
Blind = rustcut.Blind


def test_only_files():
    """Absolute paths of the files that are test code in their ENTIRETY, derived from the tree."""
    return rustcut.test_only_files(CRATE_DIRS)



def rust_files():
    """(path, whole_file) for every Rust file both rules read, in a stable order."""
    whole = test_only_files()
    for path in crate_files():
        yield path, path in whole
    for d in TEST_DIRS:
        for base, dirs, files in os.walk(d):
            dirs.sort()
            for f in sorted(files):
                if f.endswith(".rs"):
                    yield os.path.join(base, f), True


# ---------------------------------------------------------------------------------------------
# What `callers_of` is allowed to look at
#
# A helper and the `#[test]` that calls it are in the same MODULE far more often than in the same
# file, and until SKEIN-904 this gate could only see the file. `pub(super) fn drafting_fixture_for`
# in `src/review/testkit.rs` is called from `budget.rs`, `scope.rs` and `visit.rs`, so the gate
# reported "no #[test] in its file calls it" and three reviewed fixtures were carrying exemption
# rows that recorded a human doing by hand what the tool could not do at all.
#
# The module is the radius rather than the crate because it is the VISIBILITY — `pub(super)` from
# `review::testkit` reaches exactly the files below — and because widening further would make a
# by-name search meaningless: `setup` is one function in a module and a dozen in a crate.
#
# Under `tests/` the module is the BINARY, which is the same rule and not an exception: cargo builds
# one binary per `tests/<name>.rs`, which has no siblings to look at, and one per `tests/<name>/`
# holding a `main.rs`, whose every file is a module of that binary (`test_binary`, SKEIN-1114).
# ---------------------------------------------------------------------------------------------


def modules_of():
    """{absolute path: module key} for every file `rust_files()` yields.

    `rustcut.units` is the grouping — `src/<name>.rs` together with `src/<name>/**` — asked once
    per crate, so this gate and the two module gates agree on what a module is rather than each
    deciding.

    REFUSES TO RUN rather than fall back, because the fallback is invisible: a crate file that no
    unit claims would quietly get the old one-file radius back, and a verdict of "every #[test]
    that calls it holds the lock" derived over the wrong set of files is worse than the blindness
    it replaced.
    """
    out = {}
    for base, prefix in ((CRATE_DIRS[0], ""), (CRATE_DIRS[1], "warden/")):
        for name, paths in rustcut.units(base):
            for path in paths:
                out[os.path.abspath(path)] = prefix + name
    missed = sorted(
        os.path.relpath(p, ROOT)
        for p in (os.path.abspath(q) for q in crate_files())
        if os.path.abspath(p) not in out
    )
    if missed:
        raise Blind(
            "no module claims " + ", ".join(missed) + ". `callers_of` resolves over a module, so a "
            "file outside every unit would silently get the one-file radius SKEIN-904 removed, and "
            "the pass it produces would be read as proof"
        )
    return out


def collect(sources=None, groups=None, root=ROOT):
    """Every env-touching scope, each carrying the fn table of its whole module.

    The table is built exactly as `scan_file` builds its own — `uncommented`, then the fns inside
    each test region — so a caller is judged by the same text as a scope. It is shared between the
    scopes of one module rather than copied: `callers_of` only reads it.

    `sources`, `groups` and `root` default to this tree; `self_check` passes a tree of its own.
    """
    groups = modules_of() if groups is None else groups
    sources = rust_files() if sources is None else sources
    index, scopes, found, files = {}, [], {}, []
    for path, whole_file in sources:
        # A `tests/` file is a module of the binary it is compiled into — `tests/<name>.rs` alone,
        # or every file of a `tests/<name>/` with a `main.rs` (SKEIN-1114) — and a file of no
        # binary, `tests/common/mod.rs`, is its own. See the block above and `test_binary`.
        module = groups.get(os.path.abspath(path)) or test_binary(path, root) or os.path.abspath(path)
        files.append((path, whole_file, module))
        text = uncommented(open(path, encoding="utf-8").read())
        unit = unit_name(path, root)
        for lo, hi in test_regions(text, whole_file):
            fns = functions(text, lo, hi)
            for fn in fns:
                index.setdefault(module, []).append({
                    "name": fn["name"],
                    "attrs": fn["attrs"],
                    "unit": unit,
                    "body": text[fn["body_start"]:fn["end"]],
                })
            for name, slot in lock_providers(text, fns).items():
                found.setdefault(module, {})[name] = slot
    providers = module_lock_providers(index, found)
    for path, whole_file, module in files:
        scopes += [
            dict(s, module=module)
            for s in scan_file(path, whole_file, providers.get(module, {}), root)
        ]
    for scope in scopes:
        scope["module_fns"] = index[scope["module"]]
    return scopes


def own_test_binary(scope):
    """Is `scope` in a file cargo compiles into exactly one integration binary?

    Cargo gives every `tests/<name>.rs` its own process, and every `tests/<name>/main.rs` too, with
    the rest of `tests/<name>/` compiled into that one (SKEIN-1114, `test_binary`). A file of no
    binary, such as `tests/common/mod.rs`, is a module reached only through a sibling's
    `mod common;`, compiled once per binary that declares it, so the "nothing in its process can
    race it" argument in `verdict` does not hold for it at all (SKEIN-722).
    """
    return bool(scope.get("binary"))


def verdict(scope, per_file):
    """(ok, note). `ok` is False when this scope needs an exemption or a fix.

    `per_file` counts the env-touching scopes in the same PROCESS (`process_of`), which is what
    decides the question for `tests/`: cargo builds **one binary per integration test**, a file or a
    directory with a `main.rs`, so those tests share a process with each other and with nothing
    else. A binary whose only env-touching scope is this one has nothing in its process to race
    against, and demanding a lock there would be a ritual. Two or more in one binary is the same
    hazard as the lib, in a smaller process — whichever of its files they are in.

    That argument needs `own_test_binary`, not just a `tests/` prefix — see there.
    """
    if scope["dropped"]:
        return False, (
            "binds the guard to `_`, so it is dropped on the line it is taken — the test runs "
            "unlocked while reading as locked"
        )
    if scope["guard"]:
        return True, "holds the lock"
    if own_test_binary(scope) and per_file == 1:
        return True, (
            "is the only env-touching scope in its own test binary — cargo gives every "
            "tests/<name>.rs and tests/<name>/ its own process, so nothing here can race it"
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
        "is a helper that sets env vars and no #[test] in its module calls it — nothing here can "
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
      · **A helper in another file is invisible — to THIS rule.** `reached` resolves by name within
        one file, so a test whose fixture lives in `src/review/testkit.rs` while the test is in
        `src/review/scope.rs` shows no touch and is not paired. Rule one's `callers_of` stopped
        being file-bound in SKEIN-904 and this did not follow it, on purpose: widening `callers_of`
        can only add a caller to a set that must be entirely locked, so a name collision there
        becomes a finding, while widening this would fold another file's body into the blob whose
        `set_var`s and `remove_var`s are then netted off against each other — a collision here
        cancels a leak. Written down as its own item rather than done as a side effect of that one.
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


# ---------------------------------------------------------------------------------------------
# Rule three: a remove_var after the test's own last risky line is the unwind-leak shape, by name
# ---------------------------------------------------------------------------------------------

# Rule two's pairing (set_var(X) .. remove_var(X), anywhere in the blob) accepts a trailing
# `remove_var` on the last line exactly as readily as one guarded by `Drop` — it only asks whether
# the names balance, never WHEN the removal runs relative to what could panic first. That is the
# SKEIN-696/SKEIN-703 shape: a `remove_var` on the last line of a test is unwound past by a failing
# assertion, so the test restores the environment when it passes and leaks it when it fails —
# which rule two is structurally unable to tell apart from a `Drop`-based restore, because both
# read as "set, then removed, somewhere in this blob".
#
# This rule asks a narrower, POSITIONAL question instead: does a literal `remove_var("NAME")`
# appear, in the test's OWN body, textually AFTER the last line that could panic or return early —
# `assert!`/`assert_eq!`/`assert_ne!`/`panic!`, `.unwrap(`, `.expect(`, or the `?` operator? If so,
# that removal is unreachable on every path out through one of those, which is every path a real
# bug takes.
#
# Two shapes are excluded, deliberately, because they are not the hazard above:
#
#   · a `remove_var` inside a NESTED `impl Drop for X { .. }` written inline in the test body runs
#     from `Drop::drop`, on every way out including an unwind — it is the FIX, not the disease, and
#     is textually "after" an assert only by coincidence of where the struct happens to be
#     declared;
#   · a `remove_var` inside a closure literal (`|..| { .. }`) does not run when the closure is
#     DEFINED, only if and when it is CALLED — its textual position relative to an assert outside
#     the closure says nothing about execution order at all.
#
# Both are found by brace-matching their opening `{` (`_mask_blocks` below) and blanked out of the
# body before either the risky-line search or the remove_var search runs, so neither contributes to
# "last risky line" and neither can BE a finding.
#
# The one shape this rule DOES except on top of the two block kinds above: save-and-put-back (read
# the old value, set, and later restore exactly that value). Its restore is not always `set_var` —
# a name that was ABSENT before the test has to go back to absent, so the honest restore reads
# `match old { Some(v) => set_var(name, v), None => remove_var(name) }`, and that `None` arm is a
# `remove_var` like any other, indistinguishable from the naive kind by shape alone. What tells them
# apart is not the removal, it is what happens BEFORE it: a save-and-put-back site reads the name's
# own prior value, with `var`/`var_os`, before ever writing it — literally `READ % re.escape(name)`,
# the same test rule two's own restore check uses to keep this shape out of ITS findings (see
# `restore_findings` above). A `remove_var(name)` earned by that match arm is excepted here for the
# identical reason: the code already proved it knows what belongs in `name` — nothing — it did not
# simply forget to put anything back.
RISKY = re.compile(
    r"\b(?:assert(?:_eq|_ne)?!|panic!)" r"|\.unwrap\s*\(" r"|\.expect\s*\(" r"|\?(?=[\s;,)\].])"
)

TRAILING_REMOVE = re.compile(r'\b(?:std::)?env::remove_var\s*\(\s*"(?P<name>[A-Za-z0-9_]+)"')

NESTED_DROP = re.compile(r"\bimpl\s+Drop\s+for\s+\w+\s*\{")
CLOSURE_BLOCK = re.compile(r"\|[^|\n]*\|\s*\{")


def _mask_blocks(text, opener):
    """Blank out (length-preserving) the brace-matched span of every match of `opener`.

    `opener` must match up to and including the block's opening `{`. Blanking rather than cutting
    keeps every later offset valid — the same discipline `uncommented` follows.
    """
    out = list(text)
    for m in opener.finditer(text):
        start = m.end() - 1
        end = match_brace(text, start)
        if end < 0:
            continue
        for i in range(m.start(), end):
            if out[i] != "\n":
                out[i] = " "
    return "".join(out)


def trailing_findings():
    """{scope key: [names removed after the test's own last risky line]}, plus how many `#[test]`s
    this rule actually judged (had at least one risky line to measure a position against).

    Unit of judgement is the `#[test]` body ALONE — unlike rule two, same-file helpers are not
    folded in: the hazard is about ORDER within one function's control flow, and a helper reached
    by name carries no ordering information relative to the caller's own risky lines.
    """
    findings = {}
    judged = 0
    for path, whole_file in rust_files():
        text = uncommented(open(path, encoding="utf-8").read())
        unit = unit_name(path)
        for lo, hi in test_regions(text, whole_file):
            for fn in functions(text, lo, hi):
                if not is_test_fn(fn):
                    continue
                body = text[fn["body_start"] : fn["end"]]
                masked = _mask_blocks(body, NESTED_DROP)
                masked = _mask_blocks(masked, CLOSURE_BLOCK)
                risky = [m.end() for m in RISKY.finditer(masked)]
                if not risky:
                    continue
                judged += 1
                last = max(risky)
                bad = sorted(
                    {
                        m.group("name")
                        for m in TRAILING_REMOVE.finditer(masked)
                        if m.start() > last
                        and not re.search(READ % re.escape(m.group("name")), masked)
                    }
                )
                if bad:
                    findings[f"{unit}::{fn['name']}"] = bad
    return findings, judged


TRAIL_DEBT_HEAD = '''# Tests with a `remove_var` textually after their own last risky line (SKEIN-723) —
# `assert!`/`assert_eq!`/`assert_ne!`/`panic!`, `.unwrap(`, `.expect(`, or `?` — which a failing one
# of those unwinds straight past. Same shape and same rules as `docs/env-restore.toml`, read by the
# same tool: a finding not listed here fails the build, a row here that no longer applies fails the
# build too, and `python3 tools/env-lock-check.py --update-trailing` prunes but never adds.
'''


def render_trailing_debt(findings):
    out = [TRAIL_DEBT_HEAD]
    for k in sorted(findings):
        out.append('[leaks."%s"]' % k)
        out.append("vars = [%s]" % ", ".join('"%s"' % v for v in findings[k]))
        out.append("")
    return "\n".join(out).rstrip() + "\n"


def load_trailing_debt():
    if not os.path.exists(TRAIL_DEBT):
        return {}
    with open(TRAIL_DEBT, "rb") as f:
        return tomllib.load(f).get("leaks", {})


def check_trailing(findings, debt):
    """Problems from rule three: an unlisted trailing remove_var, a stale row, or one that grew."""
    problems = []
    for k in sorted(findings):
        row = debt.get(k)
        listed = set(row.get("vars", [])) if row else set()
        fresh = sorted(set(findings[k]) - listed)
        if row is None:
            problems.append(
                f"env-lock-check: `{k}` removes {', '.join('$' + v for v in findings[k])} after its "
                f"own last risky line\n"
                f"                rule: a failing assert!/panic!/.unwrap(/.expect(/`?` before that "
                f"point unwinds past a trailing remove_var (the SKEIN-696/SKEIN-703 shape). Convert "
                f"to `env_pins()` (src/testutil.rs), which restores from `Drop` and so survives a "
                f"failing assertion — or add `{k}` to docs/env-trailing.toml if it cannot be, with "
                f"the reason."
            )
        elif fresh:
            problems.append(
                f"env-lock-check: `{k}` now also removes {', '.join('$' + v for v in fresh)} after "
                f"its own last risky line\n"
                f"                rule: docs/env-trailing.toml records what this scope did on the "
                f"day it was written; a new name is a new instance of the same defect"
            )
    for k in sorted(set(debt) - set(findings)):
        problems.append(
            f"env-lock-check: docs/env-trailing.toml records `{k}`, and it no longer has a trailing "
            f"remove_var — or no longer exists\n"
            f"                rule: delete the row (`--update-trailing`). A debt list that outlives "
            f"its debt stops being read."
        )
    return problems


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
# **The fix is `env_pins()`, not a trailing `remove_var`.** A `remove_var` on the last line of a
# test is unwound past by a failing assertion, so a test repaired that way restores the environment
# when it passes and leaks when it fails. `EnvPins` restores from `Drop`, on every path out.
# `src/testutil.rs` holds the library's copy; `tests/common/mod.rs` holds the one every integration
# binary shares, because a `#[cfg(test)]` module is not in the library those binaries link.
#
# Which files have been converted is NOT listed here. The count is on the last line of an ordinary
# run, derived from the code; a list copied into prose is checked by nothing and goes stale in
# silence, which is the failure this whole file exists to make impossible.
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


def render(bad, sites):
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
        if len(sites[k]) > 1:
            out.append("covers = %d" % len(sites[k]))
        out.append("")
    return "\n".join(out).rstrip() + "\n"


SELF_CHECK_TREE = {
    # One binary in a directory: the helper's only caller is in a sibling file, and the second
    # scope keeps the count above one so the helper has to be resolved through its callers.
    "tests/dirbin/main.rs": "mod harness;\nmod path;\n#[test]\nfn other() {\n    let _g = env_lock();\n    std::env::set_var(\"B\", \"1\");\n}\n",
    "tests/dirbin/harness.rs": "pub fn set_up() {\n    std::env::set_var(\"A\", \"1\");\n}\n",
    "tests/dirbin/path.rs": "#[test]\nfn caller() {\n    let _g = env_lock();\n    super::harness::set_up();\n}\n",
    # One binary in a directory with ONE env-touching scope: nothing in its process can race it.
    "tests/solo/main.rs": "mod one;\n",
    "tests/solo/one.rs": "#[test]\nfn lone() {\n    std::env::set_var(\"C\", \"1\");\n}\n",
    # One binary in a directory with an unlocked scope in each of two files: one process, two.
    "tests/pair/main.rs": "mod a;\nmod b;\n",
    "tests/pair/a.rs": "#[test]\nfn first() {\n    std::env::set_var(\"D\", \"1\");\n}\n",
    "tests/pair/b.rs": "#[test]\nfn second() {\n    std::env::set_var(\"E\", \"1\");\n}\n",
    # A directory with no `main.rs` is no binary: compiled into every binary declaring it (SKEIN-722).
    "tests/common/mod.rs": "#[test]\nfn shared() {\n    std::env::set_var(\"F\", \"1\");\n}\n",
    # A flat file is still its own binary.
    "tests/flat.rs": "#[test]\nfn flat() {\n    std::env::set_var(\"G\", \"1\");\n}\n",
}

SELF_CHECK_EXPECT = {
    "tests/dirbin/harness::set_up": True,   # was "no #[test] in its module calls it"
    "tests/dirbin/main::other": True,
    "tests/solo/one::lone": True,           # was refused the one-binary exemption
    "tests/pair/a::first": False,           # a per-file count of one each would exempt both
    "tests/pair/b::second": False,
    "tests/common/mod::shared": False,      # SKEIN-722: not a binary of its own
    "tests/flat::flat": True,
}


def self_check():
    """Judge a small tree of each `tests/` layout and fail on any verdict it does not expect.

    Each direction of SKEIN-1114 has a case that goes the wrong way under the old per-file rule —
    the comment beside each expectation says which — and `tests/common/` and `tests/flat.rs` hold
    the two layouts that must not have moved. Returns the list of problems; empty means sound.
    """
    import tempfile
    problems = []
    with tempfile.TemporaryDirectory(prefix="env-lock-self-check-") as root:
        paths = []
        for rel, body in sorted(SELF_CHECK_TREE.items()):
            path = os.path.join(root, rel)
            os.makedirs(os.path.dirname(path), exist_ok=True)
            with open(path, "w", encoding="utf-8") as f:
                f.write(body)
            paths.append((path, True))
        scopes = collect(paths, {}, root)
        counts = per_file_counts(scopes)
        got = {key(s): verdict(s, counts[process_of(s)]) for s in scopes}
    for name, want in SELF_CHECK_EXPECT.items():
        if name not in got:
            problems.append(f"{name}: not found as an env-touching scope at all")
        elif got[name][0] != want:
            problems.append(f"{name}: judged {'ok' if got[name][0] else 'BAD'} — {got[name][1]}")
    for name in sorted(set(got) - set(SELF_CHECK_EXPECT)):
        problems.append(f"{name}: an env-touching scope the self-check did not plant")
    return problems


def main():
    problems = self_check()
    if problems:
        print("env-lock-check: SELF-CHECK FAILED — the gate misjudges a `tests/` layout it was built to "
              "read, so its verdicts on this tree cannot be trusted:", file=sys.stderr)
        for p in problems:
            print(f"  · {p}", file=sys.stderr)
        return 2
    scopes = collect()
    counts = per_file_counts(scopes)
    judged = [(s, *verdict(s, counts[process_of(s)])) for s in scopes]
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

    trailing, trailing_judged = trailing_findings()
    if "--show-trailing" in sys.argv:
        for k in sorted(trailing):
            print(f"BAD  {k:<70} removes {', '.join('$' + v for v in trailing[k])} after its own last risky line")
        print(
            f"\n{trailing_judged} #[test]s had at least one risky line to measure a position "
            f"against, {len(trailing)} of them removing a variable after it"
        )
        return 0

    bad = {key(s): note for s, ok, note in judged if not ok}
    # The key is `<unit>::<fn>`, so two functions of the same name in one file share one row. That
    # is not hypothetical: `src/testutil.rs` has three `fn drop`s, two of which touch the
    # environment, and one exemption has always covered both with no spelling that could separate
    # them — so a NEW `impl Drop` in that file would have been exempt the moment it was written,
    # by a reason argued about two other functions (SKEIN-896).
    #
    # The count is the guard, rather than a longer key. Qualifying the key by the enclosing `impl`
    # would move five of the eight rows and every line number in their reasons, to buy a
    # distinction the reasons already draw in prose — while the hazard is only ever "a site
    # appeared under a key somebody already justified". A count sees exactly that, and sees it for
    # every key rather than for `drop`.
    sites = {}
    for s, ok, _ in judged:
        if not ok:
            sites.setdefault(key(s), []).append(s["line"])
    if "--update" in sys.argv:
        open(SPEC, "w", encoding="utf-8").write(render(bad, sites))
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

    if "--update-trailing" in sys.argv:
        # Prune, never add — see TRAIL_DEBT_HEAD, same discipline as --update-restore above.
        was = load_trailing_debt()
        if not os.path.exists(TRAIL_DEBT):
            kept = dict(trailing)
        else:
            kept = {k: trailing[k] for k in sorted(set(was) & set(trailing))}
        open(TRAIL_DEBT, "w", encoding="utf-8").write(render_trailing_debt(kept))
        print(
            f"wrote {os.path.relpath(TRAIL_DEBT, ROOT)} ({len(kept)} row(s), "
            f"{len(set(was) - set(kept))} pruned; {len(set(trailing) - set(kept))} unrecorded "
            f"trailing remove_var(s) left for the check to report)"
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
        elif entry.get("covers", 1) != len(sites[k]):
            problems.append(
                f"env-lock-check: docs/env-lock.toml exempts `{k}` as covering "
                f"{entry.get('covers', 1)} site(s), and it now covers {len(sites[k])} — "
                f"{', '.join(f'{k.split(chr(58))[0]}.rs:{ln}' for ln in sorted(sites[k]))}\n"
                f"                rule: the key is `<unit>::<fn>`, so same-named functions in one "
                f"file share a row. A site that appeared under a key somebody already justified "
                f"was never justified. Read the reason against every line above, extend it, and "
                f"set `covers`."
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
    trail_debt = load_trailing_debt()
    problems += check_trailing(trailing, trail_debt)
    for p in problems:
        print(p + "\n")
    if problems:
        print(
            f"{len(problems)} problem(s). `python3 tools/env-lock-check.py --show` lists every "
            f"env-touching test scope and how it was judged, `--show-restore` every #[test] that "
            f"leaks one, `--show-trailing` every #[test] that removes one after its own last risky "
            f"line."
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
    # Rule three (SKEIN-723): unlike rule two's pairing-by-name, this asks WHEN a remove_var runs
    # relative to what could panic first in the test's OWN body — see `trailing_findings`.
    print(
        f"every remove_var after a test's own last risky line is a recorded one "
        f"({len(trail_debt)} in docs/env-trailing.toml, out of {trailing_judged} #[test]s with a "
        f"risky line to measure a position against)"
    )
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except Blind as blind:
        # Not a finding and not a pass: the gate cannot see what it is supposed to judge. Exit 2 so
        # a caller that only distinguishes 0 from non-zero still fails, and a human reading the log
        # can tell "nothing wrong" from "nothing looked at".
        print(f"env-lock-check: REFUSING TO RUN — {blind}", file=sys.stderr)
        sys.exit(2)
