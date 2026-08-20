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
3. **The destination table is consistent with itself.** `[destination.*]` is architecture.md §14.
   Every dependency it names must be a module it declares, and the graph must be acyclic. §14 is a
   design nobody can run yet; this is the only way it can be wrong out loud rather than quietly.

Two deliberate exclusions. **Test code is not an architectural dependency** — a fixture reaching
across modules says nothing about the design — so `#[cfg(test)] mod tests` is cut before reading, and
its edges are reported separately under `--tests`. **Comments are not references**: `[`crate::ai`]`
in prose is a doc link, and counting it is how the previous tool decided `config` depends on `ai`.

Usage:
    python3 tools/module-check.py            # check; non-zero and a reason on any violation
    python3 tools/module-check.py --update   # rewrite the allow-list from the code
    python3 tools/module-check.py --graph    # print the edges, weighted by path mentions
    python3 tools/module-check.py --tests    # the same for test-only edges
"""
import os, re, sys, collections, tomllib

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SRC = os.path.join(ROOT, "src")
SPEC = os.path.join(ROOT, "docs", "modules.toml")


def uncommented(text):
    """Source with every comment removed — doc comments included, on purpose.

    `[`crate::ai`]` in a doc comment is a link, not a call. Counting it is how a graph acquires
    edges that no code makes: `config -> ai`, `contracts -> review`, `github -> gitgate` are all
    prose."""
    out = []
    for line in text.split("\n"):
        if line.lstrip().startswith("//"):
            continue
        out.append(re.sub(r"//.*$", "", line))
    return "\n".join(out)


def without_tests(text):
    """(code, tests). A fixture that reaches across modules is not a dependency of the design.

    Brace-matched rather than "everything after the marker", because the cheap version silently
    stops reading at the test module and any item below it becomes invisible — a checker that
    cannot see part of the crate reports a clean graph for the wrong reason."""
    m = re.search(r"^#\[cfg\(test\)\]\nmod tests \{", text, re.M)
    if not m:
        return text, ""
    depth, i = 0, m.end() - 1
    while i < len(text):
        if text[i] == "{":
            depth += 1
        elif text[i] == "}":
            depth -= 1
            if depth == 0:
                i += 1
                break
        i += 1
    return text[: m.start()] + text[i:], text[m.start() : i]


def units():
    for f in sorted(os.listdir(SRC)):
        if f.endswith(".rs"):
            yield f[:-3], os.path.join(SRC, f)
    binaries = os.path.join(SRC, "bin")
    for f in sorted(os.listdir(binaries)):
        if f.endswith(".rs"):
            yield "bin/" + f[:-3], os.path.join(binaries, f)


def read_edges():
    """(code, tests): Counter of (consumer, provider) -> number of references."""
    modules = {name for name, _ in units() if not name.startswith("bin/") and name != "lib"}
    code, tests = collections.Counter(), collections.Counter()
    for name, path in units():
        head, tail = without_tests(open(path, encoding="utf-8").read())
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


def main():
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
