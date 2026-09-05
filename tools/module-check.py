#!/usr/bin/env python3
"""The module graph, read exactly, and checked against what `docs/modules.toml` allows.

**What makes this exact.** `src/lib.rs` used to re-export sixteen modules with `pub use <mod>::*`,
so a cross-module reference was a bare name and no tool could resolve it without guessing. That is
gone (SKEIN-23), and the crate root holds no code at all (SKEIN-25), so every cross-module reference
is now literally `crate::<mod>::` or `skein::<mod>::` and the edge set falls straight out of the
source. This replaces `tools/module-edges.py`, which counted bare names and therefore reported edges
that were not there — `gitgate -> apiauth` because both used the word `token`.

**What it checks**, in the order the failures matter:

1. **No undeclared edge.** Every edge in `src/` appears in `[current.*] depends_on`. A new
   dependency is then a line in a reviewed diff rather than an import nobody looked at.
2. **No growing knot.** The strongly-connected components are recorded with their members. A module
   joining one, or a new one forming, fails — because a cycle is the one structural fault that is
   cheap to add and expensive to remove, and today's is already 18 modules wide.
3. **The warden stays a separate crate.** §14 gives it an empty depends-on column and the note
   "(separate binary)" — it is what a compromised skein has to get past, so a shared library would be
   a shared blast radius. That boundary is enforced by nothing except nobody having written the
   dependency, so this checks the manifest and the source both.
4. **The destination table is consistent with itself.** `[destination.*]` is architecture.md §14.
   Every dependency it names must be a module it declares, and the graph must be acyclic. §14 is a
   design nobody can run yet; this is the only way it can be wrong out loud rather than quietly.

Two deliberate exclusions. **Test code is not an architectural dependency** — a fixture reaching
across modules says nothing about the design — so every `#[cfg(test)]` item is cut before reading,
and its edges are reported separately under `--tests`. **Comments are not references**:
`[`crate::ai`]` in prose is a doc link, and counting it is how the previous tool decided `config`
depends on `ai`.

Both cuts come from `tools/rustcut.py`, which all three text gates share. The local copies drifted:
this one cut `#[cfg(test)]\nmod tests {` and nothing else, so the nine `#[cfg(test)] pub(crate) fn`
helpers in `src/` were read as production and five edge weights counted a fixture's reference as
architecture.

Usage:
    python3 tools/module-check.py            # check; non-zero and a reason on any violation
    python3 tools/module-check.py --update   # rewrite the allow-list from the code
    python3 tools/module-check.py --graph    # print the edges, weighted by path mentions
    python3 tools/module-check.py --tests    # the same for test-only edges
"""
import os, re, sys, collections, tomllib

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import rustcut  # noqa: E402 — the one cutter every gate shares, self-checked at import

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SRC = os.path.join(ROOT, "src")
SPEC = os.path.join(ROOT, "docs", "modules.toml")


def uncommented(text):
    """Source with every comment removed — doc comments included, on purpose.

    `[`crate::ai`]` in a doc comment is a link, not a call. Counting it is how a graph acquires
    edges that no code makes: `config -> ai`, `contracts -> review`, `github -> gitgate` are all
    prose."""
    return rustcut.uncommented(text)


def without_tests(text):
    """(code, tests). A fixture that reaches across modules is not a dependency of the design.

    Brace-matched rather than "everything after the marker", because the cheap version silently
    stops reading at the test module and any item below it becomes invisible — a checker that
    cannot see part of the crate reports a clean graph for the wrong reason.

    It used to cut `#[cfg(test)]\nmod tests {` and that shape alone, so the nine `#[cfg(test)]
    pub(crate) fn` helpers and the four extra test modules in this tree were read as production
    code and their cross-module reaches counted as architecture. `rustcut.split_tests` cuts every
    test-only item, whatever shape it takes.
    """
    return rustcut.split_tests(text)


def end_of_block(text, start):
    """Index just past the `}` closing the block whose `{` is at `start`.

    **Braces inside literals and comments are not braces** (SKEIN-412). Counting them cut a test
    module at the first `}` in a string — `src[at..].find("\\n}\\n")` in `src/util.rs` is a real
    one — so every test below that point was read as shipped code, and the checker then reported a
    module with no row in `docs/modules.toml` and a new cycle. Three findings, none of them true,
    and none of them naming the test module that had actually been mis-cut. A `{` in a literal does
    the opposite and swallows whatever real code sits below the test module, which is the direction
    this function's own doc warns about: a checker that cannot see part of the crate reports a clean
    graph for the wrong reason.
    """
    return rustcut.end_of_block(text, start)


def units():
    """Every unit as (name, [paths]) — `src/<name>.rs` AND `src/<name>/**`, from the one cutter.

    `os.listdir(SRC)` alone is what made both module gates blind to a module that is a directory:
    the day `src/fleet.rs` becomes `src/fleet/`, the unit and all 297 of its edges would have
    dropped out of the graph and the gate would have called the result clean.
    """
    return rustcut.units(SRC)


def read_edges():
    """(code, tests): Counter of (consumer, provider) -> number of references."""
    modules = {name for name, _ in units() if not name.startswith("bin/") and name != "lib"}
    code, tests = collections.Counter(), collections.Counter()
    for name, paths in units():
        head, tail = without_tests(rustcut.read_unit(paths))
        for bucket, text in ((code, head), (tests, tail)):
            for provider in re.findall(r"\b(?:crate|skein)::([a-z_]+)\b", uncommented(text)):
                if provider in modules and provider != name:
                    bucket[(name, provider)] += 1
    return code, tests


def components(adjacency):
    """Tarjan. Returns every strongly connected component, singletons included."""
    index, low, stack, on_stack, out, counter = {}, {}, [], set(), [], [0]
    sys.setrecursionlimit(10000)

    def visit(v):
        index[v] = low[v] = counter[0]
        counter[0] += 1
        stack.append(v)
        on_stack.add(v)
        for w in sorted(adjacency.get(v, ())):
            if w not in index:
                visit(w)
                low[v] = min(low[v], low[w])
            elif w in on_stack:
                low[v] = min(low[v], index[w])
        if low[v] == index[v]:
            component = []
            while True:
                w = stack.pop()
                on_stack.discard(w)
                component.append(w)
                if w == v:
                    break
            out.append(sorted(component))

    for v in sorted(adjacency):
        if v not in index:
            visit(v)
    return out


def adjacency_of(edges):
    out = collections.defaultdict(set)
    for consumer, provider in edges:
        out[consumer].add(provider)
        out.setdefault(provider, set())
    return out


def load_spec():
    with open(SPEC, "rb") as fh:
        return tomllib.load(fh)


def check_destination(spec, complain):
    """architecture.md §14 against itself: names resolve, and the design is acyclic."""
    table = spec.get("destination", {})
    for module, row in sorted(table.items()):
        for provider in row.get("depends_on", []):
            if provider not in table:
                complain(
                    "§14 gives `%s` a dependency on `%s`, which §14 does not declare"
                    % (module, provider),
                    "every module a row depends on must have a row of its own",
                )
    adjacency = {m: set(row.get("depends_on", [])) & set(table) for m, row in table.items()}
    for component in components(adjacency):
        if len(component) > 1:
            complain(
                "§14's own dependency table is cyclic: %s" % " -> ".join(component + [component[0]]),
                "the destination design must be a DAG, or it cannot be built bottom-up",
            )


def check_current(spec, edges, complain):
    allowed = {m: set(row.get("depends_on", [])) for m, row in spec.get("current", {}).items()}
    for consumer, provider in sorted(edges):
        if consumer not in allowed:
            complain(
                "`%s` has no row in docs/modules.toml, so nothing says what it may depend on"
                % consumer,
                "every module in src/ is declared, or the allow-list means nothing",
            )
            continue
        if provider not in allowed[consumer]:
            complain(
                "`%s` depends on `%s`, and docs/modules.toml does not allow it (%d references)"
                % (consumer, provider, edges[(consumer, provider)]),
                "a new dependency is a decision — add it to [current] for `%s` and say why"
                % consumer,
            )
    # Per edge, not per module: an entry left behind when its last call site went is exactly the
    # case a module-level check misses, because the module still has other edges.
    for module in sorted(allowed):
        for provider in sorted(allowed[module] - {p for c, p in edges if c == module}):
            complain(
                "docs/modules.toml allows `%s` -> `%s`, and no code makes that reference"
                % (module, provider),
                "an allow-list that outlives its edges stops being a statement about the code",
            )


def check_cycles(spec, edges, complain):
    recorded = {frozenset(c["modules"]): c for c in spec.get("cycle", [])}
    found = [c for c in components(adjacency_of(edges)) if len(c) > 1]
    for component in found:
        key = frozenset(component)
        if key in recorded:
            continue
        overlap = [r for r in recorded if r & key]
        if overlap:
            grew = sorted(key - set().union(*overlap))
            # **A cycle that SHRANK is not a cycle somebody joined**, and saying so was this
            # check's own bug. Nothing grew: every module in the live component was already
            # recorded, so what happened is that one or more modules LEFT — exactly the case the
            # rule below says needs no permission. It was reported as a join anyway, with an empty
            # list where the joiner should be, which reads as an accusation with no defendant.
            #
            # Found by deleting `review::context`, whose diff download was the last `review ->
            # moduledocs` edge: `moduledocs` left the cycle, the graph improved, and the tool
            # complained that a module had joined one.
            #
            # Still worth saying, because a declaration that outlives its edges stops being a
            # statement about the code — but as the stale record it is, not as a decision to make.
            if not grew:
                left = sorted(set().union(*overlap) - key)
                complain(
                    "the recorded cycle {%s} has shrunk to {%s} — %s left it"
                    % (
                        ", ".join(sorted(set().union(*overlap))),
                        ", ".join(sorted(key)),
                        ", ".join("`%s`" % m for m in left),
                    ),
                    "a module may leave a cycle without asking; update the record to match",
                )
                continue
            complain(
                "the dependency cycle {%s} now also holds %s"
                % (", ".join(sorted(set().union(*overlap))), ", ".join("`%s`" % m for m in grew)),
                "a module may leave a cycle without asking; joining one is a decision",
            )
        else:
            complain(
                "a new dependency cycle: %s" % " -> ".join(component + [component[0]]),
                "record it in docs/modules.toml with a reason and the work item that ends it, "
                "or break it",
            )
    live = [frozenset(c) for c in found]
    for key in sorted(recorded, key=sorted):
        if key in live:
            continue
        if any(key & other for other in live):
            continue  # it moved, and the growth complaint above already says how
        complain(
            "docs/modules.toml records a cycle among %s that no longer exists"
            % ", ".join("`%s`" % m for m in sorted(key)),
            "delete the [[cycle]] entry — a resolved cycle left on the page reads as a live one",
        )


def render_allow_list(edges):
    by_consumer = collections.defaultdict(set)
    for consumer, provider in edges:
        by_consumer[consumer].add(provider)
    lines = []
    for consumer in sorted(by_consumer):
        key = '"%s"' % consumer if not re.fullmatch(r"[A-Za-z0-9_-]+", consumer) else consumer
        lines.append("[current.%s]" % key)
        lines.append(
            "depends_on = [%s]"
            % ", ".join('"%s"' % p for p in sorted(by_consumer[consumer]))
        )
        lines.append("")
    return "\n".join(lines)


def update(edges):
    """Rewrite everything from `# @CURRENT` to the end of the file."""
    text = open(SPEC, encoding="utf-8").read()
    marker = "# @CURRENT\n"
    head = text[: text.index(marker) + len(marker)]
    open(SPEC, "w", encoding="utf-8").write(head + "\n" + render_allow_list(edges))
    print("docs/modules.toml: rewrote [current.*] from %d edges" % len(edges))


def show(edges, title):
    """Weight is *path mentions*, not call sites: `use crate::fleet::{a, b, c}` counts once, and so
    does every later `crate::fleet::` written out in full. It measures how many places name the
    module, which is what a reader has to change to break the edge — not how hard the code leans on
    it."""
    print("# %s\n# consumer\tprovider\tmentions" % title)
    for (consumer, provider), n in sorted(edges.items(), key=lambda kv: (-kv[1], kv[0])):
        print("%s\t%s\t%d" % (consumer, provider, n))


def check_warden_is_separate(complain):
    """§14 gives the `warden` module an empty depends-on column and the note "(separate binary)".

    Everything else in this file is about edges *within* one crate. This one is the opposite claim
    and needs its own check, because a crate boundary is enforced by nothing except nobody having
    written the dependency — and the moment someone does, the warden is sharing a blast radius with
    the component it exists to be independent of.

    Read off the manifest and off the source, because either alone can be defeated: a `[dependencies]`
    entry with no `use` is still a linked crate, and a `use skein::` with no entry does not build but
    says what somebody meant.
    """
    manifest = os.path.join(ROOT, "warden", "Cargo.toml")
    if not os.path.exists(manifest):
        return
    text = open(manifest, encoding="utf-8").read()
    body = text.split("[dependencies]", 1)[-1]
    for line in body.splitlines():
        name = line.split("=")[0].strip()
        if name in ("skein", "skein-warden"):
            complain(
                "`warden/Cargo.toml` depends on `%s`" % name,
                "architecture §14 gives the warden an empty depends-on column: it is what a "
                "compromised skein has to get past, and a shared library is a shared blast radius",
            )
    src = os.path.join(ROOT, "warden", "src")
    if not os.path.isdir(src):
        return
    for f in sorted(os.listdir(src)):
        if not f.endswith(".rs"):
            continue
        code = uncommented(open(os.path.join(src, f), encoding="utf-8").read())
        if re.search(r"\bskein::", code):
            complain(
                "`warden/src/%s` names `skein::`" % f,
                "the warden must not reach into skein's code — see §14 and the crate note in "
                "warden/src/lib.rs",
            )


# A module whose tests carry braces in literals — the shape that produced three false findings.
#
# Held as a fixture rather than as a comment because this checker's failure mode is to report a
# clean graph, or somebody else's module, and be believed. It runs on every invocation: it costs
# microseconds, and a cutter that has quietly stopped working is worse than no cutter at all.
SELF_CHECK = r'''pub fn shipped() {
    let _ = crate::config::load();
}

#[cfg(test)]
mod tests {
    #[test]
    fn braces_in_literals_and_comments_are_not_braces() {
        // Every hazard below carries TWO closing braces, which is what makes each of them
        // load-bearing on its own: the module is two deep here, so any one of them that is counted
        // cuts it short, and a fixture where they only matter together proves nothing about any of
        // them. The opener at the end carries two of the other kind, for the swallowing direction.
        assert_eq!(find("\n}\n}\n"), 1);
        // The lone quote first, deliberately: without raw-string handling the `"` opens and the
        // next one closes an empty plain string, which leaves the braces after it exposed. A raw
        // string whose braces sit between two quotes is covered by the plain-string skipper and
        // proves nothing about this one.
        let raw = r#"" }} "#;
        // Twice, because the module is two deep: one counted `}` is not enough to cut it.
        let ch = '}';
        let ch2 = '}';
        let uni = '\u{7d}';
        // }} in a line comment
        /* }} in a block comment /* nested */ */
        let _ = crate::testutil::tempdir();
    }
}
'''

# The other direction, on its own, because in the fixture above the closing-brace hazards fire
# first and would mask it: an OPENING brace inside a literal extends the test module over whatever
# real code follows it, and the graph then comes back clean because part of the crate is invisible.
SELF_CHECK_SWALLOW = r'''#[cfg(test)]
mod tests {
    #[test]
    fn an_opening_brace_in_a_string_does_not_swallow_the_code_below() {
        let opener = "{{ and nothing closes these";
    }
}

pub fn shipped_below_the_tests() {
    let _ = crate::signal::of();
}
'''


def self_check():
    """The cutter can see a whole test module, braces in literals and all (SKEIN-412).

    Run on every invocation rather than kept in a suite nobody runs. This checker's failure mode is
    to report a clean graph, or a finding about an innocent module, and be believed — so it proves
    its own eyes before it says anything about the crate.
    """
    code, tests = without_tests(SELF_CHECK)
    if "crate::testutil" not in tests:
        raise SystemExit(
            "module-check: its own cutter is broken — a test module was cut short at a brace "
            "inside a literal or a comment, so tests below that point are being read as shipped "
            "code. Every finding about a module with tests is suspect (SKEIN-412)."
        )
    if "crate::testutil" in code:
        raise SystemExit(
            "module-check: its own cutter is broken — a fixture's `crate::testutil` was counted as "
            "a CODE edge, which is how a mis-cut test module reports a module with no row in "
            "docs/modules.toml and a cycle that does not exist (SKEIN-412)."
        )
    below_code, _ = without_tests(SELF_CHECK_SWALLOW)
    if "crate::signal" not in below_code:
        raise SystemExit(
            "module-check: its own cutter is broken — a `{` inside a literal extended the test "
            "module over the code BELOW it, so part of the crate is invisible and the graph is "
            "clean for the wrong reason, which is the failure this function's own doc warns "
            "about (SKEIN-412)."
        )


def main():
    self_check()
    code, tests = read_edges()
    if "--update" in sys.argv:
        update(code)
        return 0
    if "--graph" in sys.argv:
        show(code, "cross-module references in src/, tests excluded")
        return 0
    if "--tests" in sys.argv:
        show(tests, "cross-module references from test code only")
        return 0

    spec = load_spec()
    problems = []

    def complain(what, rule):
        problems.append((what, rule))

    check_destination(spec, complain)
    check_current(spec, code, complain)
    check_cycles(spec, code, complain)
    check_warden_is_separate(complain)

    if problems:
        for what, rule in problems:
            print("module-check: %s\n              rule: %s\n" % (what, rule), file=sys.stderr)
        print(
            "%d problem(s). docs/modules.toml is the allow-list; "
            "`python3 tools/module-check.py --update` rewrites it from the code."
            % len(problems),
            file=sys.stderr,
        )
        return 1
    print(
        "module graph is within docs/modules.toml (%d edges over %d units)"
        % (len(code), len(adjacency_of(code)))
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
