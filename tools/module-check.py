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

**`--update` merges; it does not regenerate.** `docs/modules.toml` is not an inventory that
happens to carry notes — the reasoning IS the deliverable, and the file exists so that an edge
somebody adds later has to be argued against a written reason rather than merely accepted. This
flag used to rewrite everything below `# @CURRENT` from the parsed graph, which deleted 320 lines
of that reasoning, reordered what was left, and dropped every row whose `depends_on` was empty —
silently, and on the one command whose name says it is how you keep the file current (SKEIN-614).
It now rewrites `depends_on` statements in place and touches nothing else, it adds without
subtracting, and `--prune` is the second gesture that applies a removal, printing what it removed.

Usage:
    python3 tools/module-check.py            # check; non-zero and a reason on any violation
    python3 tools/module-check.py --update   # merge the code's edges in, keeping every written line
    python3 tools/module-check.py --update --prune   # ...and apply the removals it would refuse
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


CURRENT_BLOCK = re.compile(r'^\[current\.(?:"([^"]+)"|([A-Za-z0-9_-]+))\]\s*$')
DEPENDS_ON = re.compile(r"^depends_on\s*=", re.M)

# Written above a row `--update` invented, because the row is the cheap half. TODO is the same
# marker `residue-check --update` leaves against an entry it could not write a reason for.
NEW_ROW_NOTE = (
    "# TODO: no reason written yet. `--update` can write the row; it cannot write the argument for\n"
    "# it, and an edge nobody argued for is the thing this file exists to prevent. Say why `%s` may\n"
    "# depend on each name below, then delete these three lines.\n"
)


def block_name(line):
    """The module a `[current.x]` header names, or None for any other line."""
    match = CURRENT_BLOCK.match(line.rstrip("\n"))
    return (match.group(1) or match.group(2)) if match else None


def key_of(module):
    """`bin/skein` needs quoting; `ai` does not."""
    return module if re.fullmatch(r"[A-Za-z0-9_-]+", module) else '"%s"' % module


def render_depends_on(providers):
    return 'depends_on = [%s]\n' % ", ".join('"%s"' % p for p in sorted(providers))


def split_blocks(tail):
    """(preamble, [(module, text)]) — every block keeping its own bytes, in the file's own order.

    A run of comments and blank lines immediately above a header belongs to the block BELOW it:
    that is where this file's section dividers are written, and where a reader expects to find
    them. Getting it the other way round would make a divider follow the block it introduces the
    moment anything moved.
    """
    lines = tail.splitlines(keepends=True)
    heads = [i for i, line in enumerate(lines) if block_name(line)]
    if not heads:
        return tail, []
    starts = []
    for n, head in enumerate(heads):
        start, floor = head, heads[n - 1] + 1 if n else 0
        while start > floor and (
            not lines[start - 1].strip() or lines[start - 1].lstrip().startswith("#")
        ):
            start -= 1
        starts.append(start)
    blocks = []
    for n, start in enumerate(starts):
        end = starts[n + 1] if n + 1 < len(starts) else len(lines)
        blocks.append((block_name(lines[heads[n]]), "".join(lines[start:end])))
    return "".join(lines[: starts[0]]), blocks


def depends_on_span(text):
    """(start, end) of the whole `depends_on = [...]` statement, brackets balanced across lines."""
    match = DEPENDS_ON.search(text)
    if not match:
        return None
    open_at = text.find("[", match.end())
    if open_at < 0:
        return None
    depth = 0
    for i in range(open_at, len(text)):
        if text[i] == "[":
            depth += 1
        elif text[i] == "]":
            depth -= 1
            if depth == 0:
                end = text.find("\n", i)
                return match.start(), len(text) if end < 0 else end + 1
    return None


def providers_in(text, span):
    if span is None:
        return set()
    try:
        return set(tomllib.loads(text[span[0] : span[1]]).get("depends_on", []))
    except tomllib.TOMLDecodeError:
        return set()


def merge_current(tail, modules, edges, prune=False):
    """The allow-list with the code's edges merged in, and every hand-written byte still in it.

    Returns `(text, applied, refused)`. **This does not regenerate the file**, and that is the
    whole design (SKEIN-614): only the `depends_on` statements are rewritten, in place, so the
    prose above them — which is the deliverable, the argument each edge had to win — comes through
    byte for byte, in the order somebody put it in.

    Adding is safe and happens. Subtracting is a decision about somebody's writing, so `--update`
    reports it and leaves it, and only `--prune` applies it:

      - a provider the code no longer references is removed from the list, and the sentence that
        argued for it is left where it is, because no tool can tell which sentence that was;
      - a module with no code left in `src/` keeps its whole row, prose and all, until `--prune`
        — and `--prune` prints the block it removed, so it is in the terminal as well as in git.

    `modules` is every unit in `src/`, NOT every consumer in `edges`: a row whose `depends_on` is
    empty is a claim about the module ("nothing, and that is the rule rather than an accident"),
    and six of them are in the file. Reading the module set off the edges is what made the old
    regenerating `--update` delete all six silently — nothing would have caught it either, since
    `check_current` only demands a row for a module that has an edge.
    """
    wanted = collections.defaultdict(set)
    for consumer, provider in edges:
        wanted[consumer].add(provider)
    preamble, blocks = split_blocks(tail)
    out, applied, refused, seen = [preamble], [], [], set()
    for module, text in blocks:
        seen.add(module)
        span = depends_on_span(text)
        have = providers_in(text, span)
        want = wanted.get(module, set())
        if module not in modules:
            if prune:
                applied.append(
                    "removed the row for `%s`, which has no code in src/ any more:\n%s"
                    % (module, "".join("      | %s\n" % l for l in text.strip().splitlines()))
                )
            else:
                refused.append(
                    "`%s` has no code in src/ any more, and its row here is %d line(s) somebody "
                    "wrote. Read them, then delete the block by hand — or re-run with --prune, "
                    "which prints what it removes." % (module, len(text.strip().splitlines()))
                )
                out.append(text)
            continue
        added, dropped = sorted(want - have), sorted(have - want)
        keep = want if prune else have | want
        if keep != have:
            if span is None:
                refused.append(
                    "[current.%s] has no `depends_on` statement to write into" % key_of(module)
                )
                out.append(text)
                continue
            text = text[: span[0]] + render_depends_on(keep) + text[span[1] :]
        for provider in added:
            applied.append(
                "wrote `%s` -> `%s` into [current.%s] — the note above it does not argue for `%s`"
                % (module, provider, key_of(module), provider)
            )
        for provider in dropped:
            if prune:
                applied.append(
                    "removed `%s` -> `%s`, which no code makes any more — the note above "
                    "[current.%s] may still argue for it" % (module, provider, key_of(module))
                )
            else:
                refused.append(
                    "docs/modules.toml allows `%s` -> `%s` and no code makes it. Removing the "
                    "entry means the sentence that argued for it is now describing nothing, and "
                    "no tool can tell which sentence that is — edit by hand, or --prune."
                    % (module, provider)
                )
        out.append(text)
    for module in sorted(set(wanted) - seen):
        if not "".join(out).endswith("\n\n"):
            out.append("\n")
        out.append(
            "[current.%s]\n%s%s"
            % (key_of(module), NEW_ROW_NOTE % module, render_depends_on(wanted[module]))
        )
        applied.append("added a row for `%s`, with a TODO where its reason goes" % module)
    return "".join(out), applied, refused


def update(modules, edges, prune=False):
    """Merge the code's edges into `docs/modules.toml`, keeping everything a person wrote.

    It used to rewrite everything from `# @CURRENT` to the end of the file out of the parsed graph,
    which deleted 320 lines of comments — the reason each edge is allowed, which is the only thing
    that made the file worth gating — silently, on the command whose name says it is how you keep
    the file current (SKEIN-614).
    """
    where = os.path.relpath(SPEC, ROOT)
    text = open(SPEC, encoding="utf-8").read()
    marker = "# @CURRENT\n"
    if marker not in text:
        raise SystemExit(
            "module-check: %s has no `# @CURRENT` line, so there is no allow-list to merge into. "
            "Refusing to write, because the alternative is to guess where the prose ends." % where
        )
    cut = text.index(marker) + len(marker)
    head, tail = text[:cut], text[cut:]
    merged, applied, refused = merge_current(tail, modules, edges, prune)
    if merged != tail:
        open(SPEC, "w", encoding="utf-8").write(head + merged)
    for line in applied:
        print("%s: %s" % (where, line))
    if not applied and not refused:
        print("%s: already says what the code does; nothing written" % where)
    elif applied:
        print("%s: %d change(s) written. Every TODO and every new name is a reason to write."
              % (where, len(applied)))
    for line in refused:
        print("module-check: %s" % line, file=sys.stderr)
    if refused:
        print(
            "\n%d thing(s) NOT done. `--update` adds; it does not delete what somebody wrote — "
            "this file is an argument, not an inventory, and a regenerator that keeps the edges "
            "and drops the arguments leaves a file that passes its own gate having lost the point "
            "of it (SKEIN-614). `--update --prune` applies the removals above and prints each one."
            % len(refused),
            file=sys.stderr,
        )
    return 1 if refused else 0


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


# An allow-list in miniature: prose, a row whose module has left `src/`, and a row whose emptiness
# is the claim. Held as a fixture rather than a comment for the same reason as the cutter's, and
# checked on every invocation — the failure this guards against is silent by construction, since a
# file with the edges kept and the arguments gone still passes every check in this tool.
SELF_CHECK_ALLOW_LIST = """
[current.alpha]
# `beta` because a reader has to be able to find out WHY. This sentence is the deliverable: the
# file exists so that an edge somebody adds later is argued against a written reason.
depends_on = ["beta", "delta"]

[current.gone]
# Deleted from src/ this morning. The argument for where its boundary sat is still here, and a
# tool that removes it unasked has thrown away the only part worth keeping.
depends_on = ["beta"]

[current.leaf]
# Nothing, and that is the claim rather than an accident.
depends_on = []
"""
SELF_CHECK_MODULES = {"alpha", "beta", "gamma", "leaf", "added"}
SELF_CHECK_EDGES = {("alpha", "beta"): 3, ("alpha", "gamma"): 1, ("added", "beta"): 2}


def section_of(text, module):
    """One `[current.x]` block out of a merged allow-list, for the assertions below."""
    at = text.index("[current.%s]" % module)
    rest = text[at + 1 :]
    end = rest.find("\n[current.")
    return rest if end < 0 else rest[:end]


def self_check_merge():
    """`--update` keeps every line a person wrote — the whole point of the file (SKEIN-614).

    Named concretely so it can be watched failing: delete `depends_on` from the fixture's `alpha`
    block and the first assertion goes red; go back to rendering the file from the graph and every
    one of them does.
    """
    tail = SELF_CHECK_ALLOW_LIST
    prose = [line for line in tail.splitlines() if line.startswith("#")]

    def survived(merged, allowed_losses=0):
        lost = [line for line in prose if line not in merged.splitlines()]
        if len(lost) != allowed_losses:
            raise SystemExit(
                "module-check: its own `--update` is destroying the file it maintains — %d of %d "
                "hand-written line(s) did not survive the merge, starting with %r. "
                "docs/modules.toml is an argument, not an inventory (SKEIN-614)."
                % (len(lost), len(prose), lost[0] if lost else "")
            )

    def wrong(what):
        raise SystemExit("module-check: its own `--update` is broken — %s (SKEIN-614)." % what)

    merged, applied, refused = merge_current(tail, SELF_CHECK_MODULES, SELF_CHECK_EDGES)
    survived(merged)
    if '"gamma"' not in section_of(merged, "alpha"):
        wrong("a new edge `alpha -> gamma` was not written into the row that needs it")
    if "[current.added]" not in merged or '"beta"' not in section_of(merged, "added"):
        wrong("a module that is new to src/ got no row, so the gate would stay red after --update")
    if "[current.leaf]" not in merged:
        wrong("a row whose `depends_on` is empty was dropped — that row is a claim, not an absence")
    if "[current.gone]" not in merged or '"delta"' not in section_of(merged, "alpha"):
        wrong("it deleted somebody's line without --prune, which is the whole of SKEIN-614")
    if len(refused) != 2 or not applied:
        wrong("it did not report both removals it declined to make (%d reported)" % len(refused))

    pruned, applied, refused = merge_current(
        tail, SELF_CHECK_MODULES, SELF_CHECK_EDGES, prune=True
    )
    survived(pruned, allowed_losses=2)  # `gone`'s own two lines, and nothing else
    if "[current.gone]" in pruned:
        wrong("--prune left the row of a module that has no code in src/ any more")
    if '"delta"' in section_of(pruned, "alpha"):
        wrong("--prune left an allow-list entry no code makes")
    if "[current.alpha]" not in pruned or "the deliverable" not in section_of(pruned, "alpha"):
        wrong("--prune took a neighbouring block's prose with the one it removed")
    if refused or not any("gone" in line for line in applied):
        wrong("--prune removed a row without printing it")


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
    self_check_merge()


def main():
    self_check()
    code, tests = read_edges()
    if "--update" in sys.argv:
        # Every unit in `src/`, not every consumer in `code`: a row with an empty `depends_on` is a
        # claim, and six of them are in the file. Reading the module set off the edges is what made
        # the old regenerator delete all six without a word.
        return update({name for name, _ in units()}, code, prune="--prune" in sys.argv)
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
            "`python3 tools/module-check.py --update` merges the code's edges into it, keeping "
            "every line somebody wrote — then write the reason for each new one."
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
