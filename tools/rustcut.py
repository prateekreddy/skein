"""One reader of Rust source for every gate that has to cut it.

Three gates read `src/*.rs` and decide something from the text: `module-check.py` (the module
graph), `source-check.py` (the Source law) and `env-lock-check.py` (the env lock). Each needs the
same two cuts — *comments are not code* and *`#[cfg(test)]` is not shipped* — and each grew its own.
The third copy counted braces without skipping strings, so it reported nothing at all for
`src/fleet.rs`, whose test module opens with a shell fixture full of braces: 212 env writes in that
file and the gate saw none of them. The cutter that SKEIN-412 fixed was never ported.

So: one cutter, imported by all three, with the self-check that used to live in `module-check.py`
run on every invocation of every gate. A gate whose cutter has quietly stopped working reports a
clean tree for the wrong reason, which is worse than no gate.

**What this reads is text, not a parse.** It knows enough Rust to step over the four things that
carry braces without being blocks — line and block comments, string literals in every raw/byte
spelling, and char literals — and no more. That is the whole contract, and the self-check pins it.

**What it deliberately does not know.** Macro bodies are ordinary text to it, so a `macro_rules!`
arm with an unbalanced brace would confuse it (none exists here; `grep -c 'macro_rules!' src`
→ 0). Generic `<`/`>` are not counted at all, because `<` is also less-than; only `(` and `[`
depth is tracked, which is enough to keep the `;` in `[u8; 32]` from ending an item.
"""

import os
import re

# `br#"…"#` and every shorter form of it, in one pattern. Matched at the position rather than
# searched for, so a `r"` appearing INSIDE another literal is not mistaken for the start of one.
RAW_STRING = re.compile(r'b?r(#*)"')
# `'x'`, `'\n'`, `'\u{1f600}'` — the last one is why this is not `'..'`: a char literal can
# legitimately contain braces, which is the exact failure this module exists for. A lifetime
# (`'a`) has no closing quote and deliberately does not match.
CHAR_LITERAL = re.compile(r"'(\\u\{[0-9a-fA-F_]+\}|\\.|[^\\'])'", re.S)


def skip_token(text, i):
    """If a non-code token starts at `text[i]`, the index just past it; else None.

    A lifetime is not a token here: the caller steps over the quote as one ordinary character
    rather than hunting for a closing one that does not exist.
    """
    n = len(text)
    if text.startswith("//", i):
        end = text.find("\n", i)
        return n if end < 0 else end
    if text.startswith("/*", i):
        nested, j = 1, i + 2
        while j < n and nested:
            if text.startswith("/*", j):
                nested, j = nested + 1, j + 2
            elif text.startswith("*/", j):
                nested, j = nested - 1, j + 2
            else:
                j += 1
        return j
    boundary = i == 0 or not (text[i - 1].isalnum() or text[i - 1] == "_")
    raw = RAW_STRING.match(text, i)
    if raw and boundary:
        close = '"' + raw.group(1)
        end = text.find(close, raw.end())
        return n if end < 0 else end + len(close)
    c = text[i]
    if c == '"' or (c == "b" and text.startswith('b"', i) and boundary):
        j = i + (2 if c == "b" else 1)
        while j < n:
            if text[j] == "\\":
                j += 2
                continue
            if text[j] == '"':
                return j + 1
            j += 1
        return n
    if c == "'":
        m = CHAR_LITERAL.match(text, i)
        return m.end() if m else None
    return None


def end_of_block(text, start):
    """Index just past the `}` closing the block whose `{` is at `start`; len(text) if unclosed."""
    i, depth, n = start, 0, len(text)
    while i < n:
        past = skip_token(text, i)
        if past is not None and past > i:
            i = past
            continue
        c = text[i]
        if c == "{":
            depth += 1
        elif c == "}":
            depth -= 1
            if depth == 0:
                return i + 1
        i += 1
    return n


def match_brace(text, start):
    """Index one past the `}` closing the first `{` at or after `start`, or -1."""
    i, n = start, len(text)
    while i < n:
        past = skip_token(text, i)
        if past is not None and past > i:
            i = past
            continue
        if text[i] == "{":
            return end_of_block(text, i)
        i += 1
    return -1


def uncommented(text):
    """Source with every comment removed — a doc comment that says "via nsenter" is prose."""
    out, i, n = [], 0, len(text)
    while i < n:
        if text.startswith("//", i) or text.startswith("/*", i):
            j = skip_token(text, i)
            # Keep the newlines a comment spanned, so line numbers downstream still line up.
            out.append("\n" * text.count("\n", i, j))
            i = j
            continue
        past = skip_token(text, i)
        if past is not None and past > i:
            out.append(text[i:past])
            i = past
            continue
        out.append(text[i])
        i += 1
    return "".join(out)


def item_end(text, start):
    """Index just past the item that begins at `start`: its closing `}`, or its `;` if it has no
    block — whichever comes first at depth zero. len(text) if neither does.

    "Depth zero" counts `(` and `[`, because a `;` inside either is punctuation in a *type*, not
    the end of an item: `const K: [u8; 32] = …;` and `fn f(buf: [u8; 32])` both carry one, and
    stopping there would end the item in the middle of its own signature — the same shape of
    mis-cut as WTS-9, one level down. Generic `<…>` is not counted, because `<` is also
    less-than and a type-position `;` always sits inside a bracket or a paren as well.
    """
    i, n, nest = start, len(text), 0
    while i < n:
        past = skip_token(text, i)
        if past is not None and past > i:
            i = past
            continue
        c = text[i]
        if c in "([":
            nest += 1
        elif c in ")]":
            nest -= 1
        elif c == ";" and nest <= 0:
            return i + 1
        elif c == "{" and nest <= 0:
            return end_of_block(text, i)
        i += 1
    return n


# `#[cfg(<predicate>)]` alone on its line. The predicate is captured rather than spelled out,
# because `test` is not the only way to write "test only" — see `is_test_cfg`.
def blanked(text):
    """Source with every comment replaced by spaces — same length, same lines, same offsets.

    The other cut, for callers that index the ORIGINAL source. `env-lock-check` computes function
    bodies, brace matches and the spans it reports as offsets into the text it was handed, so a
    transform that shortens a line silently slides every position after the first comment;
    `uncommented` is newline-preserving but not length-preserving and is the wrong one there.

    Built on `skip_token` for the reason the gate that used to own a copy of this is a cautionary
    tale. That copy tracked quotes with a boolean flipped on every `"`, one line at a time, and so
    could not see a raw string: at `src/bin/skein-server.rs:5160` a fixture of the shape
    `r#"…"…https://…"#` has an EVEN number of `"` before the `//`, the tracker believed it was
    outside a string, blanked to end of line, and took the closing `"#` and a `}}]` with it. The
    braces then unbalanced and `mod review_routes` (5123-5743) closed at 5465 — 278 lines of tests
    that the env lock gate read as not being there, which is WTS-4 exactly, one layer up. A `//`
    inside any string is not a comment, and `skip_token` is the one place that knows every spelling
    of "any string".
    """
    out, i, n = [], 0, len(text)
    while i < n:
        if text.startswith("//", i) or text.startswith("/*", i):
            j = skip_token(text, i)
            # One space per character, newlines untouched: same length, same line breaks.
            out.append(re.sub(r"[^\n]", " ", text[i:j]))
            i = j
            continue
        past = skip_token(text, i)
        if past is not None and past > i:
            out.append(text[i:past])
            i = past
            continue
        out.append(text[i])
        i += 1
    return "".join(out)


CFG_ATTR = re.compile(r"[ \t]*#\[cfg\((?P<pred>[^\[\]]*)\)\][ \t]*\n")


def _top_level_commas(pred):
    """`pred` split on the commas that are not inside a nested `(…)`."""
    parts, depth, last = [], 0, 0
    for i, c in enumerate(pred):
        if c == "(":
            depth += 1
        elif c == ")":
            depth -= 1
        elif c == "," and depth == 0:
            parts.append(pred[last:i])
            last = i + 1
    parts.append(pred[last:])
    return [p.strip() for p in parts if p.strip()]


def is_test_cfg(pred):
    """Is an item under `#[cfg(<pred>)]` compiled ONLY into test builds?

    `test` is. `all(test, unix)` is, because every arm must hold and one of them is `test` — so
    the item cannot exist in a release build, which is the whole question the gates ask.

    `any(test, unix)` is NOT: it ships whenever `unix` holds, and cutting it would hide real
    production code from the Source law. `not(test)` is the opposite of the question and must
    never match — `queues_are_cached` in `src/prq/refresh.rs` has one, guarding its shipped half.

    Nothing in the tree spells `all(test, …)` today (`grep -c 'cfg(all(test' src warden/src`
    → 0). The case is decided here rather than at the sighting, so that the first one to be
    written is cut instead of quietly becoming production code to every gate at once.
    """
    pred = pred.strip()
    if pred == "test":
        return True
    if pred.startswith("all(") and pred.endswith(")"):
        return any(is_test_cfg(p) for p in _top_level_commas(pred[4:-1]))
    return False


def cfg_test_spans(text):
    """[(start, end)] of every test-only item, outermost first, non-overlapping.

    The attribute is looked for **at code positions only** — the scan steps over strings and
    comments on its way — so a Rust snippet quoted inside a raw-string fixture cannot be mistaken
    for a real attribute and take the production code after it out of the gate's sight. Nothing
    in the tree does that today; the guard costs one linear pass (0.13s on `src/fleet.rs`, the
    largest file) and the failure it prevents is silent.
    """
    spans, i, n = [], 0, len(text)
    while i < n:
        past = skip_token(text, i)
        if past is not None and past > i:
            i = past
            continue
        if text[i] == "#":
            line_start = text.rfind("\n", 0, i) + 1
            if not text[line_start:i].strip():
                m = CFG_ATTR.match(text, line_start)
                if m and is_test_cfg(m.group("pred")):
                    end = item_end(text, m.end())
                    spans.append((line_start, end))
                    i = end
                    continue
        i += 1
    return spans


def split_tests(text):
    """(shipped, tests): every test-only item cut out, whatever shape it takes.

    The item after the attribute is either a block (`mod tests { … }`, `fn helper() { … }`,
    `impl X { … }`) or brace-less and `;`-terminated (`use …;`, `const X: T = …;`). The cut runs to
    whichever comes first at depth zero — a `;` before any `{` ends a brace-less item. Cutting to
    "the next `{` anywhere" is what deleted `fn detached_script_path` in `src/fleet.rs`, the
    production item after a `#[cfg(test)] const`, so a reach inside it was invisible (WTS-9).
    """
    shipped, tests, pos = [], [], 0
    for start, end in cfg_test_spans(text):
        shipped.append(text[pos:start])
        # A blank line stands where the item was, so the line count of what follows is unchanged.
        shipped.append("\n" * text.count("\n", start, end))
        tests.append(text[start:end])
        pos = end
    shipped.append(text[pos:])
    return "".join(shipped), "".join(tests)


# Directories under `src/` that are not modules of the crate: assets, shell and python payloads,
# and the two bin-like trees. Kept as a name list because that is what `os.listdir` gives back.
NOT_MODULES = ("bin", "web", "kit", "probe", "store")


def units(src, warden=None):
    """Every compilation unit as (name, [paths]).

    A unit is a module: `src/<name>.rs` **together with** `src/<name>/**/*.rs`, because a module
    split into a directory is still one module to the graph and to the Source law. Enumerating
    with `os.listdir` alone is how a `src/fleet/` tree would have dropped out of every gate the
    moment `fleet.rs` was split — the gate reporting a clean graph for the wrong reason again.
    Binaries are `bin/<name>`; the warden's files are `warden/<name>` when `warden` is given.
    """
    names = set()
    for f in os.listdir(src):
        path = os.path.join(src, f)
        if f.endswith(".rs"):
            names.add(f[:-3])
        elif os.path.isdir(path) and f not in NOT_MODULES:
            if any(g.endswith(".rs") for _, _, files in os.walk(path) for g in files):
                names.add(f)
    for name in sorted(names):
        paths = []
        flat = os.path.join(src, name + ".rs")
        if os.path.exists(flat):
            paths.append(flat)
        tree = os.path.join(src, name)
        if os.path.isdir(tree):
            for base, dirs, files in os.walk(tree):
                dirs.sort()
                for g in sorted(files):
                    if g.endswith(".rs"):
                        paths.append(os.path.join(base, g))
        yield name, paths
    binaries = os.path.join(src, "bin")
    if os.path.isdir(binaries):
        for f in sorted(os.listdir(binaries)):
            if f.endswith(".rs"):
                yield "bin/" + f[:-3], [os.path.join(binaries, f)]
    if warden and os.path.isdir(warden):
        for f in sorted(os.listdir(warden)):
            if f.endswith(".rs"):
                yield "warden/" + f[:-3], [os.path.join(warden, f)]


def read_unit(paths):
    """The text of a unit — its files concatenated, each ending in a newline."""
    out = []
    for p in paths:
        t = open(p, encoding="utf-8").read()
        out.append(t if t.endswith("\n") else t + "\n")
    return "".join(out)


# --------------------------------------------------------------------------------------------
# Test code that is a FILE rather than a block
#
# `cfg_test_spans` above reads ONE file and finds the `#[cfg(test)]` attributes in it. A module
# declared `#[cfg(test)] mod testkit;` carries its attribute in the PARENT, so the file it names has
# no attribute anywhere in it, `cfg_test_spans` returns nothing for it, and everything it contains
# falls into no test region at all. That is SKEIN-871: `src/review/testkit.rs` held 11 env writes
# that neither of `env-lock-check`'s rules had ever judged, while that gate printed a clean tree.
#
# It lives HERE, with the rest of the one cutter, because two gates needed the same answer and
# arrived at it two different ways — the shape this module exists to stop (SKEIN-894).
# `fleet-pin-check.py` had its own reader, and it was wrong in three ways at once: it resolved
# every child against `os.path.dirname(parent)`, which is right only for `lib.rs`, `main.rs` and
# `mod.rs` and silently drops the first `#[cfg(test)] mod X;` written in a flat `src/<name>.rs`;
# it read the blanked source, which blanks comments but NOT string literals, so a `mod` line
# QUOTED in a fixture counted; and it stopped after one hop, so what a test-only module itself
# declares was production code to it.
#
# Derived from the `mod` declarations in the tree, never listed. The four paths that are test-only
# today would be a correct list today and an unfalsifiable one tomorrow — CLAUDE.md's `leaks.mjs`
# is the canonical version of that failure here, a check carrying three fixture names of the day it
# was written, answering `0` beside 195 matching processes.
# --------------------------------------------------------------------------------------------

# `#[cfg(...)]` wherever it sits. Deliberately NOT `CFG_ATTR` above, which requires the attribute
# to end its line: that is right for cutting spans out of a file and wrong here, because
# `#[cfg(test)] mod testkit;` written on ONE line declares exactly the same test-only file and
# would take it back out of every gate's sight. Nothing spells it that way today (checked: every
# same-line `#[cfg(test)]` in `src/` is inside a comment); this reads both.
CFG_ATTR_ANY = re.compile(r"#\[cfg\((?P<pred>[^\[\]]*)\)\]\s*")

# `mod NAME;` — the declaration that puts a module in ANOTHER FILE. `mod NAME { … }` is not this:
# an inline module is already visible to the cutter, which reads the file it is written in.
MOD_DECL = re.compile(r"mod\s+(?P<name>[A-Za-z_][A-Za-z0-9_]*)\s*;")

# The same declaration seen as the whole ITEM under a `#[cfg(test)]`, anchored at the end against
# the span `item_end` measured — so `#[cfg(test)] mod tests { … }` (a block, ending at `}`) and
# `#[cfg(test)] use …;` do not match, and a second attribute between the two does not hide it.
CFG_MOD_ITEM = re.compile(
    r"\s*(?:#\[[^\[\]]*\]\s*)*(?:pub(?:\([^)]*\))?\s+)?"
    r"mod\s+(?P<name>[A-Za-z_][A-Za-z0-9_]*)\s*;\s*$"
)


class Blind(Exception):
    """A derivation produced nothing, so the verdict read off it would be worthless.

    Raised rather than returned, and every caller exits 2 rather than 1: "I could not check" is not
    "nothing is wrong", and the failure this whole module is built against is a gate that reports
    zero problems because it looked at nothing (SKEIN-647).
    """


def mod_decls(text):
    r"""[(name, test_only)] for every `mod NAME;` in `text`, read at CODE positions only.

    The scan steps over strings and comments exactly as `cfg_test_spans` does, because the trap
    next door to this one is counting a MENTION as an instance — and here the two sets do not
    overlap at all. `grep -rn '#\[cfg(test)\] mod' src warden/src tests` returns six lines and
    **every one of them is prose**, four of them describing this exact arrangement; the four real
    declarations are written over two lines and that grep finds none of them. So a derivation
    seeded off the obvious grep would be wrong twice over: it would take `src/prwork/facts.rs`'s
    `mod tests` — prose, and not a file — for a declaration, and miss all four that are.
    """
    out, i, n = [], 0, len(text)
    while i < n:
        past = skip_token(text, i)
        if past is not None and past > i:
            i = past
            continue
        if text[i] == "#":
            m = CFG_ATTR_ANY.match(text, i)
            if m and is_test_cfg(m.group("pred")):
                end = item_end(text, m.end())
                item = CFG_MOD_ITEM.match(text, m.end(), end)
                if item:
                    out.append((item.group("name"), True))
                # Past the whole item either way: what follows a `#[cfg(test)] mod tests { … }` is
                # the code after the block, not the code inside it.
                i = max(end, i + 1)
                continue
        if text.startswith("mod", i) and (i == 0 or not (text[i - 1].isalnum() or text[i - 1] == "_")):
            m = MOD_DECL.match(text, i)
            if m:
                out.append((m.group("name"), False))
                i = m.end()
                continue
        i += 1
    return out


def child_of(parent, name):
    """The file `mod <name>;` in `parent` refers to, or None.

    `lib.rs`, `main.rs` and `mod.rs` declare their children beside themselves; any other file
    declares them in a directory named after it. Both `<name>.rs` and `<name>/mod.rs` are legal
    spellings of the child, so both are tried.

    The stem test is the whole point and is what `fleet-pin-check.py`'s copy did not have: it
    resolved every child against the parent's own directory, which happens to be right for the four
    declarations this tree has today — all four parents are a `lib.rs` or a `mod.rs` — and is wrong
    for the fifth, the moment somebody writes `#[cfg(test)] mod testkit;` in a flat `src/foo.rs`.
    """
    d, stem = os.path.dirname(parent), os.path.basename(parent)[:-3]
    if stem not in ("lib", "main", "mod"):
        d = os.path.join(d, stem)
    for cand in (os.path.join(d, name + ".rs"), os.path.join(d, name, "mod.rs")):
        if os.path.exists(cand):
            return cand
    return None


def crate_files(dirs):
    """Every Rust file under `dirs`, in a stable order."""
    for d in dirs:
        for base, subdirs, files in os.walk(d):
            subdirs.sort()
            for f in sorted(files):
                if f.endswith(".rs"):
                    yield os.path.join(base, f)


_TEST_ONLY = {}


def test_only_files(dirs):
    """Absolute paths of the files under `dirs` that are test code in their ENTIRETY.

    Transitive: whatever a test-only module itself declares is test code too, `#[cfg(test)]` or
    not, because none of it is compiled into a release build either.

    REFUSES TO RUN RATHER THAN PASS QUIETLY, in both directions of the derivation:

      * deriving NO `#[cfg(test)] mod X;` at all means the reader broke, not that the tree stopped
        having any — so every whole-file test module just became invisible to the caller, and its
        clean verdict would mean nothing. `Blind`, not an empty set.
      * a declaration that resolves to NO FILE means the resolver has stopped understanding how
        this tree lays modules out, and a resolver that is wrong about one is not to be trusted
        about the ones it did resolve. `Blind` again.
    """
    dirs = tuple(dirs)
    if dirs in _TEST_ONLY:
        return _TEST_ONLY[dirs]
    seeds, unresolved = [], []
    for path in crate_files(dirs):
        for name, test_only in mod_decls(open(path, encoding="utf-8").read()):
            if not test_only:
                continue
            child = child_of(path, name)
            if child is None:
                unresolved.append(f"{path}: `#[cfg(test)] mod {name};`")
            else:
                seeds.append(child)
    found, frontier = set(seeds), list(seeds)
    while frontier:
        path = frontier.pop()
        for name, _ in mod_decls(open(path, encoding="utf-8").read()):
            child = child_of(path, name)
            if child is None:
                unresolved.append(f"{path}: `mod {name};`")
            elif child not in found:
                found.add(child)
                frontier.append(child)
    if unresolved:
        raise Blind(
            "a module declaration resolves to no file — "
            + "; ".join(sorted(set(unresolved)))
            + ". The resolver no longer understands how this tree lays modules out, so its "
            "verdict on the declarations it DID resolve is worth nothing either"
        )
    if not seeds:
        raise Blind(
            "no `#[cfg(test)] mod X;` declaration under "
            + " or ".join(dirs)
            + ". This tree has had them since SKEIN-871; deriving none means the reader stopped "
            "working, and every whole-file test module is now invisible"
        )
    _TEST_ONLY[dirs] = found
    return found


def split_unit(paths, dirs):
    """(shipped, tests) for one unit's files, with a WHOLE-FILE test module wholly in `tests`.

    `split_tests` reads one file and cuts the `#[cfg(test)]` items written IN it. A file that is
    test code because its PARENT declares it `#[cfg(test)] mod X;` carries no attribute anywhere in
    itself, so `split_tests` hands its entire body back as shipped code — which is how
    `module-check` counted a fixture's cross-module reaches as architecture and `source-check`
    judged 70,000 characters of code that is in no release build against the Source law
    (SKEIN-905). Concatenating the unit first, as both gates did, cannot be fixed downstream: once
    the files are one string the boundary between them is gone.

    So the cut is per FILE and the classification comes from `test_only_files`, which derives the
    set from the `mod` declarations in the tree and refuses to run rather than answer none.

    **Both sides of the membership test are `abspath`-normalised on purpose.** A caller that walks
    `dirs` given relative and enumerates its units absolute — which is exactly how the two gates
    are written, `ROOT`-joined units against whatever `dirs` it passes — would match nothing, and
    an empty match is indistinguishable from "this unit has no whole-file test module". That is a
    fix that silently does nothing, in a tool whose entire subject is gates that pass for the wrong
    reason.

    The shipped half keeps the unit's line count, as `split_tests` does: a blank line stands where
    each cut line was.
    """
    whole = {os.path.abspath(p) for p in test_only_files(dirs)}
    shipped, tests = [], []
    for path in paths:
        text = open(path, encoding="utf-8").read()
        if not text.endswith("\n"):
            text += "\n"
        if os.path.abspath(path) in whole:
            shipped.append("\n" * text.count("\n"))
            tests.append(text)
        else:
            head, tail = split_tests(text)
            shipped.append(head)
            tests.append(tail)
    return "".join(shipped), "".join(tests)


# --------------------------------------------------------------------------------------------
# The self-check. Run at import, so on every invocation of every gate.
#
# It is written as assertions that name the sabotage each one catches, because the first draft of
# this module had a fixture whose brace hazards were all BALANCED — `"{{ … }}"`, `'}'` twice — and
# deleting the whole string branch of `skip_token` left the self-check green. A fixture that
# cannot fail is the failure this repo names in `change-discipline` rule 3, in the one file whose
# job is to keep three gates from going quietly blind.
# --------------------------------------------------------------------------------------------

# One token per line, each UNBALANCED, so that mishandling any single one moves a brace count.
TOKENS = [
    # (source, index just past the token, what breaks if it is not skipped)
    ('"a } brace"', 11, "a plain string"),
    ('"an escaped \\" then }"', 22, "the backslash escape inside a plain string"),
    ('r#"a } and a "quote""#', 22, "a raw string with a quote in it"),
    ('r"a } brace"', 12, "a raw string"),
    ('b"a } byte"', 11, "a byte string"),
    ('br#"a } byte"#', 14, "a raw byte string"),
    ("'}'", 3, "a char literal"),
    ("'\\u{7d}'", 8, "a unicode escape in a char literal"),
    ("'\\''", 4, "an escaped quote in a char literal"),
    ("// a } comment\n", 14, "a line comment"),
    ("/* a } /* nested */ comment */", 30, "a nested block comment"),
]

# The closing direction: a `}` that is not a brace cuts the test module short, and everything
# below it — including the fixture's own `crate::tests_only` — is read as shipped code.
SELF_CHECK = r'''pub fn shipped() {
    let _ = crate::config::load();
    // a } in a comment, and crate::comment_only::x
    /* a } in a block /* nested */ comment */
    /* and one that spans
       three lines, so that a cutter which drops
       the newlines it swallowed moves every line below this one */
}

#[cfg(test)]
const TEST_ONLY: [u8; 2] = *b"; }";

pub fn after_the_const() {
    let _ = crate::still_shipped::y();
}

#[cfg(test)]
impl<T> Helper for Fixture<T>
where
    T: Into<[u8; 4]>,
{
    fn helper(&self) {
        let fixture = format!("#!/bin/sh\nif [ x ]; then {{ echo }}; fi");
        let unbalanced = "} } }";
        let raw = r#"} } "quoted" }"#;
        let bytes = b"}}}";
        let rawbytes = br#"}}}"#;
        let ch = '}';
        let uni = '\u{7d}';
        let lt: &'static str = "a lifetime is not a char literal";
        let _ = crate::tests_only::z();
    }
}

pub fn after_the_impl() {
    let _ = crate::also_shipped::w();
}
'''

# The opening direction, on its own: in the fixture above the closing hazards fire first and would
# mask it. An `{` inside a literal extends the cut over whatever real code follows, and the gate
# then comes back clean because part of the crate is invisible.
SELF_CHECK_SWALLOW = r'''#[cfg(test)]
mod tests {
    fn opener() {
        let plain = "{{{ and nothing closes these";
        let raw = r#"{{{"#;
        let ch = '{';
    }
}

pub fn shipped_below_the_tests() {
    let _ = crate::signal::of();
}
'''

# `#[cfg(test)]` written inside a raw-string fixture is text, not an attribute. Cutting from it
# would take `crate::quoted_is_shipped` — and everything else below the fixture — out of sight.
SELF_CHECK_QUOTED = r'''pub fn holds_a_snippet() {
    let snippet = r#"
#[cfg(test)]
mod tests {
"#;
    let _ = crate::quoted_is_shipped::v();
}
'''

# The shape that defeated `env-lock-check`'s own blanker: a raw string holding an EVEN number of
# quotes before a `//`. A per-line quote tracker reads the `//` in `https://` as the start of a
# comment, blanks to end of line, and swallows the closing `"#` and the `}}]` after it — the braces
# unbalance and the test module closes hundreds of lines early. Held here rather than in that gate
# because `blanked` is now shared, and a blanker that has quietly stopped skipping raw strings makes
# every gate that uses it clean for the wrong reason.
SELF_CHECK_BLANKED = r"""#[cfg(test)]
mod tests {
    fn repos_json(home: &std::path::Path) -> String {
        let plain = "https://example.invalid/plain";
        format!(
            r#"[{{"id":"demo","source":"https://github.com/acme/thing.git","store":"{s}","p":"{p}"}}]"#,
            s = home.display(),
            p = plain,
        )
    }

    #[test]
    fn a_test_below_the_fixture() {
        let _ = crate::inside_the_module::z();
    }
}

pub fn after_the_module() {
    let _ = crate::after_the_module::w();
}
"""


# The predicate spellings, decided in `is_test_cfg` rather than at each sighting.
SELF_CHECK_PREDICATES = r'''#[cfg(all(test, unix))]
fn only_in_test_builds() {
    let _ = crate::all_test_is_cut::a();
}

#[cfg(any(test, unix))]
fn ships_on_unix() {
    let _ = crate::any_test_is_kept::b();
}

#[cfg(not(test))]
fn never_in_test_builds() {
    let _ = crate::not_test_is_kept::c();
}
'''


def _check_tokens():
    for src, past, what in TOKENS:
        got = skip_token(src, 0)
        assert got == past, "rustcut self-check: %s is not skipped (%r → %r, want %r)" % (
            what,
            src,
            got,
            past,
        )
    # A lifetime has no closing quote: it is not a token, and the callers step over the `'` as one
    # ordinary character. Returning a match here is how `&'static str` eats the rest of a file.
    assert skip_token("'static str", 0) is None, "rustcut self-check: a lifetime matched as a token"
    # Each token, on its own, must not be seen as opening a block.
    for src, _, what in TOKENS:
        assert match_brace(src + "{}", 0) == len(src) + 2, (
            "rustcut self-check: a `}` inside %s was counted as a brace" % what
        )


def _check_cuts():
    shipped, tests = split_tests(SELF_CHECK)
    code = uncommented(shipped)
    for must in ("still_shipped", "also_shipped", "config"):
        assert "crate::%s" % must in code, "rustcut self-check: `%s` was cut" % must
    for must_not in ("tests_only", "comment_only", "TEST_ONLY"):
        assert must_not not in code, "rustcut self-check: `%s` survived" % must_not
    assert "tests_only" in tests and "TEST_ONLY" in tests, "rustcut self-check: tests lost"
    assert shipped.count("\n") == SELF_CHECK.count("\n"), "rustcut self-check: line count moved"

    below, _ = split_tests(SELF_CHECK_SWALLOW)
    assert "crate::signal" in below, (
        "rustcut self-check: a `{` inside a literal extended the test cut over the code BELOW it, "
        "so part of the crate is invisible and every gate is clean for the wrong reason"
    )
    assert below.count("\n") == SELF_CHECK_SWALLOW.count("\n"), (
        "rustcut self-check: line count moved (swallow fixture)"
    )

    # `uncommented` is length-agnostic but NOT line-agnostic: `prose-check` reports `file:line`
    # against it, so a comment that takes its newlines with it slides every finding below it.
    assert code.count("\n") == shipped.count("\n"), (
        "rustcut self-check: uncommented() dropped the newlines a comment spanned, so every line "
        "number derived from it is wrong below the first comment"
    )
    raw_lines, cut_lines = SELF_CHECK.split("\n"), uncommented(SELF_CHECK).split("\n")
    for needle in ("crate::still_shipped", "crate::also_shipped"):
        before = next(i for i, l in enumerate(raw_lines) if needle in l)
        after = next(i for i, l in enumerate(cut_lines) if needle in l)
        assert before == after, (
            "rustcut self-check: uncommented() moved `%s` from line %d to line %d — the multi-line "
            "block comment above it took its newlines with it" % (needle, before + 1, after + 1)
        )

    quoted, quoted_tests = split_tests(SELF_CHECK_QUOTED)
    assert "crate::quoted_is_shipped" in quoted and not quoted_tests, (
        "rustcut self-check: `#[cfg(test)]` inside a raw string was read as an attribute"
    )

    preds, pred_tests = split_tests(SELF_CHECK_PREDICATES)
    assert "all_test_is_cut" in pred_tests, (
        "rustcut self-check: `#[cfg(all(test, …))]` is test-only and was not cut"
    )
    for kept in ("any_test_is_kept", "not_test_is_kept"):
        assert "crate::%s" % kept in preds, (
            "rustcut self-check: `%s` was cut — `any(test, …)` and `not(test)` both ship" % kept
        )


def _check_blanked():
    """Comments become spaces, nothing else moves — and a `//` inside a raw string is not one."""
    for name, src in (
        ("SELF_CHECK", SELF_CHECK),
        ("SELF_CHECK_BLANKED", SELF_CHECK_BLANKED),
        ("SELF_CHECK_QUOTED", SELF_CHECK_QUOTED),
    ):
        out = blanked(src)
        assert len(out) == len(src), (
            "rustcut self-check: blanked() changed the LENGTH of %s (%d -> %d) — every offset a "
            "caller computed into the original source is now wrong" % (name, len(src), len(out))
        )
        assert [i for i, c in enumerate(out) if c == "\n"] == [
            i for i, c in enumerate(src) if c == "\n"
        ], "rustcut self-check: blanked() moved a newline in %s" % name
    assert "comment_only" not in blanked(SELF_CHECK), (
        "rustcut self-check: blanked() left a comment behind"
    )

    # The strings must survive whole. If they do not, a `//` inside one was read as a comment and
    # everything after it on that line — the closing `"#` included — went with it. Asked of BOTH
    # cuts: they are two functions with one job, and the gates split between them.
    for cut in (blanked, uncommented):
        out = cut(SELF_CHECK_BLANKED)
        assert 'https://github.com/acme/thing.git' in out and '}}]"#' in out, (
            "rustcut self-check: %s() ate a RAW string that contains `//` — the exact defect that "
            "made env-lock-check blind to 278 lines of src/bin/skein-server.rs, where the quotes "
            "before the `//` happen to be even" % cut.__name__
        )
        assert "https://example.invalid/plain" in out, (
            "rustcut self-check: %s() ate a PLAIN string that contains `//`" % cut.__name__
        )
    out = blanked(SELF_CHECK_BLANKED)
    # And the observable consequence: the test module must still close where it really closes.
    assert cfg_test_spans(out) == cfg_test_spans(SELF_CHECK_BLANKED), (
        "rustcut self-check: blanking changed where the `#[cfg(test)]` module ends"
    )
    shipped, tests = split_tests(out)
    assert "crate::inside_the_module" in tests, (
        "rustcut self-check: the test module closed early after blanking, so the tests below its "
        "raw-string fixture are invisible"
    )
    assert "crate::after_the_module" in shipped, (
        "rustcut self-check: the test module swallowed the code below it after blanking"
    )


def _check_item_end():
    # A `;` inside a type is not the end of the item.
    for src, tail in (
        ("const K: [u8; 32] = [0; 32];\nnext", ";"),
        ("fn f(buf: [u8; 32]) {\n    let x = 1;\n}\nnext", "}"),
        ("static S: &str = \"a ; inside\";\nnext", ";"),
        ("mod testutil;\nnext", ";"),
        ("impl<T> A for B<T>\nwhere\n    T: Into<[u8; 4]>,\n{\n    fn g() {}\n}\nnext", "}"),
    ):
        end = item_end(src, 0)
        assert src[end - 1] == tail and src[end:].lstrip() == "next", (
            "rustcut self-check: item_end stopped in the middle of `%s` (at %r)"
            % (src.split("\n")[0], src[max(0, end - 12) : end])
        )


def _check_units():
    """`src/<name>/**/*.rs` folds into unit `<name>` — proved against a temp tree, not the repo.

    `src/fleet.rs` is the file the whole rewrite splits into `src/fleet/`. Both module gates used
    to enumerate with `os.listdir(src)` and would have dropped the unit entirely on the day of the
    split, reporting a graph with no `fleet` in it and calling that clean.
    """
    import shutil
    import tempfile

    root = tempfile.mkdtemp(prefix="rustcut-selfcheck-")
    try:
        src = os.path.join(root, "src")
        os.makedirs(os.path.join(src, "fleet", "deep"))
        os.makedirs(os.path.join(src, "bin"))
        os.makedirs(os.path.join(src, "web"))
        for rel in (
            "flat.rs",
            "fleet.rs",
            os.path.join("fleet", "mod.rs"),
            os.path.join("fleet", "deep", "heal.rs"),
            os.path.join("bin", "skein.rs"),
            os.path.join("web", "index.html"),
        ):
            open(os.path.join(src, rel), "w").write("\n")
        found = dict(units(src))
        assert sorted(found) == ["bin/skein", "flat", "fleet"], (
            "rustcut self-check: units() enumerated %s" % sorted(found)
        )
        assert [os.path.relpath(p, src) for p in found["fleet"]] == [
            "fleet.rs",
            os.path.join("fleet", "mod.rs"),
            os.path.join("fleet", "deep", "heal.rs"),
        ], "rustcut self-check: `src/fleet/**` did not fold into unit `fleet`"
        # A directory unit with no `<name>.rs` beside it is still a unit.
        os.remove(os.path.join(src, "fleet.rs"))
        assert "fleet" in dict(units(src)), (
            "rustcut self-check: a module that is only a directory dropped out of units()"
        )
        # `src/web` carries no `.rs` and is not a module whatever it holds.
        assert "web" not in dict(units(src)), "rustcut self-check: `src/web` became a unit"
    finally:
        shutil.rmtree(root, ignore_errors=True)



# The fixture for `mod_decls`, built out of the ways a MENTION is not an instance, because that is
# the trap this repo keeps hitting. Every line in it is UNBALANCED with respect to the answer, and
# each of the four was checked by making the change and reading the failure, not by argument:
#
#   · delete the `skip_token` call in `mod_decls` -> `quoted` and `ghost` appear (and `inline` is
#     swallowed by the commented attribute above it);
#   · drop the `is_test_cfg` test -> `shipped_only`, which is `#[cfg(not(test))]`, is reported as
#     test-only;
#   · cut the item at the end of its line instead of with `item_end` -> `swallowed`, which is
#     inside `mod tests { … }` and is not a file declaration at all, appears;
#   · use `CFG_ATTR`, which requires the attribute to end its line -> `narrow` is lost.
SELF_CHECK_DECLS = r"""
const SNIPPET: &str = "#[cfg(test)] mod quoted;";

/// A doc comment that says `#[cfg(test)] mod ghost;` while declaring nothing.
// #[cfg(test)]
// mod ghost;

#[cfg(test)]
mod inline;

#[cfg(all(test, unix))] pub(crate) mod narrow;

#[cfg(test)]
mod tests {
    mod swallowed;
}

#[cfg(not(test))]
mod shipped_only;

pub mod ordinary;
"""

_EXPECTED_DECLS = [("inline", True), ("narrow", True), ("shipped_only", False), ("ordinary", False)]


def _check_mod_decls():
    got = mod_decls(SELF_CHECK_DECLS)
    assert got == _EXPECTED_DECLS, "rustcut self-check: mod_decls read %r, not %r" % (
        got,
        _EXPECTED_DECLS,
    )


def _check_test_only_files():
    """`child_of` resolves a FLAT parent's children into its directory, and the walk is transitive.

    Both halves are the defects `fleet-pin-check.py` shipped with (SKEIN-894), so both are planted
    here as trees that answer differently if either is lost:

      · `flat.rs` declares `#[cfg(test)] mod kit;`, whose file is `flat/kit.rs`. Resolving against
        `os.path.dirname("src/flat.rs")` looks for `src/kit.rs`, finds nothing, and `Blind` fires —
        so losing the stem test cannot go quiet here.
      · `flat/kit.rs` declares a plain `mod deeper;`. It carries no `#[cfg(test)]` of its own and a
        one-hop walk misses it, though it is just as absent from a release build.
    """
    import shutil
    import tempfile

    root = tempfile.mkdtemp(prefix="rustcut-testonly-")
    try:
        src = os.path.join(root, "src")
        os.makedirs(os.path.join(src, "flat", "kit"))
        open(os.path.join(src, "lib.rs"), "w").write("mod flat;\n")
        open(os.path.join(src, "flat.rs"), "w").write("#[cfg(test)]\nmod kit;\n")
        open(os.path.join(src, "flat", "kit.rs"), "w").write("mod deeper;\n")
        open(os.path.join(src, "flat", "kit", "deeper.rs"), "w").write("// nothing\n")
        _TEST_ONLY.clear()
        got = {os.path.relpath(p, src) for p in test_only_files([src])}
        assert got == {"flat/kit.rs", "flat/kit/deeper.rs"}, (
            "rustcut self-check: test_only_files derived %r" % sorted(got)
        )

        # And it REFUSES on a tree with no such declaration, rather than answering "none".
        os.remove(os.path.join(src, "flat.rs"))
        open(os.path.join(src, "flat.rs"), "w").write("pub fn shipped() {}\n")
        os.remove(os.path.join(src, "flat", "kit.rs"))
        os.remove(os.path.join(src, "flat", "kit", "deeper.rs"))
        os.rmdir(os.path.join(src, "flat", "kit"))
        os.rmdir(os.path.join(src, "flat"))
        _TEST_ONLY.clear()
        try:
            test_only_files([src])
        except Blind:
            pass
        else:
            raise AssertionError(
                "rustcut self-check: test_only_files answered a tree with no "
                "`#[cfg(test)] mod X;` instead of refusing"
            )
    finally:
        _TEST_ONLY.clear()
        shutil.rmtree(root, ignore_errors=True)


def _check_split_unit():
    """A whole-file test module contributes NOTHING to the production half, and its unit is intact.

    Three sabotages, each of which this catches and none of which `_check_test_only_files` above
    would (it proves only which files the derivation NAMES, not that anything acts on the answer):

      · cutting the unit as one concatenated string again — `split_tests(read_unit(paths))`, which
        is what both module gates did until SKEIN-905 — puts `fixture_reach` back into `shipped`.
      · dropping the `abspath` normalisation makes the membership test compare a relative path
        against an absolute one, match nothing, and return the unit UNCUT while looking like it
        worked. The fixture asks for exactly that by spelling the two sides differently.
      · blanking the file away entirely rather than line-for-line loses the unit's line count, and
        nothing this or any gate reports can then be located.
    """
    import shutil
    import tempfile

    root = tempfile.mkdtemp(prefix="rustcut-splitunit-")
    try:
        src = os.path.join(root, "src")
        os.makedirs(os.path.join(src, "flat"))
        open(os.path.join(src, "lib.rs"), "w").write("mod flat;\n")
        flat = os.path.join(src, "flat.rs")
        open(flat, "w").write(
            "pub fn shipped() {\n"
            "    let _ = crate::shipped_reach::x();\n"
            "}\n"
            "#[cfg(test)]\n"
            "mod kit;\n"
            "#[cfg(test)]\n"
            "mod tests {\n"
            "    fn t() { let _ = crate::block_reach::z(); }\n"
            "}\n"
        )
        kit = os.path.join(src, "flat", "kit.rs")
        open(kit, "w").write(
            "pub fn fixture() {\n"
            "    let _ = crate::fixture_reach::y();\n"
            "}\n"
        )
        _TEST_ONLY.clear()
        # Deliberately mismatched spellings: the unit is enumerated absolute, `dirs` is given
        # relative to the cwd. This is the two gates' own arrangement, and the reason for the
        # `abspath` on both sides of the membership test.
        here = os.getcwd()
        os.chdir(root)
        try:
            shipped, tests = split_unit([flat, kit], ["src"])
        finally:
            os.chdir(here)
        assert "crate::shipped_reach" in shipped, (
            "rustcut self-check: split_unit cut production code out of the file that DECLARES the "
            "test module, not just out of the module"
        )
        for gone in ("fixture_reach", "block_reach"):
            assert gone not in shipped, (
                "rustcut self-check: `crate::%s` is in the production half — a file that is test "
                "code because its parent says so is being read as shipped code, which is SKEIN-905"
                % gone
            )
        for kept in ("fixture_reach", "block_reach"):
            assert kept in tests, (
                "rustcut self-check: `crate::%s` reached neither half, so the cut is losing code "
                "rather than classifying it" % kept
            )
        whole = open(flat).read() + open(kit).read()
        assert shipped.count("\n") == whole.count("\n"), (
            "rustcut self-check: split_unit moved the unit's line count (%d vs %d)"
            % (shipped.count("\n"), whole.count("\n"))
        )
    finally:
        _TEST_ONLY.clear()
        shutil.rmtree(root, ignore_errors=True)


def self_check():
    _check_tokens()
    _check_cuts()
    _check_blanked()
    _check_item_end()
    _check_units()
    _check_mod_decls()
    _check_test_only_files()
    _check_split_unit()


self_check()
