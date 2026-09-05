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
    never match — `src/prq.rs:1553` has one, guarding the shipped half of `queues_are_cached`.

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


def self_check():
    _check_tokens()
    _check_cuts()
    _check_blanked()
    _check_item_end()
    _check_units()


self_check()
