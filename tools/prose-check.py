#!/usr/bin/env python3
"""Every symbol the prose names in backticks exists in the code, or is declared not to.

CLAUDE.md rule 1 is "derive, do not assert": a claim about the code cites the file and line
or gives the command, and prose that *summarises* code drifts. Nothing enforced that half.

The E3/B-C review cut is what made the gap concrete. It removed a drafted review, its vetting
panel, its thread panel and the routes behind them, and left NINE live references to that
machinery — two of them in `docs/parity.md`, which is the acceptance gate, so the gate was
asserting the rewrite must still do things the owner had explicitly cut. The numeric checks in
that same file reproduced throughout: it counts routes (93) and page functions (443), and both
were right the whole time. Counting cannot see a sentence.

WHAT IS CHECKED, one. A backticked identifier, shaped like a symbol in this tree — snake_case,
or a `rev*`/`api*` page function — that appears nowhere in `src/`, `tests/`, `cockpit/`,
`warden/` or `tools/`. A qualified name is judged by its last segment, so
`prq::submit_review_with_comments` asks about the function. The name has to be there as a WHOLE
identifier, not as a fragment of a longer one — see `code_has`, which is where SKEIN-610's seven
sites were hiding.

WHERE THE PROSE IS. `docs/*.md`, the page's own comments, **every comment in `src/**/*.rs`**,
and the comments in `src/store/*.sh`. A comment that names a deleted function is the same
defect as a document that does, and it reaches more readers — the reader of the code.

WHAT THE CODE IS. Comments are cut out of it first. Without that the gate was satisfied by the
very drift it exists to catch: a function deleted, its name surviving in prose AND in a stale
comment, and the comment counting as the code having it (WTS-8). Rust is cut exactly, by
`tools/rustcut.py`; the other languages lose whole-line comments only.

WHAT IS CHECKED, two. A doc block glued onto the end of the one above it, with no item between
them, so rustdoc renders both on the item below and the first documents the wrong thing —
FLEET-1's class, and the same finding in four other ledgers. See `doc_attachment`.

WHAT IS CHECKED, three. A `path.rs:123` citation whose file is not on disk, or whose line is
past the end of that file. Read from every text file under `docs/`, `src/`, `tests/`, `tools/`,
`cockpit/` and `warden/`, plus the markdown at the repo root, and resolved by the tail of the
path. See `bad_citations`, which also argues why a line that merely points at the WRONG thing
cannot be a gate.

WHAT IS CHECKED, three-and-a-half. A DATED REVIEW — a document that declares, in the sentence
`CITATIONS_AT` matches, the commit its citations name lines at — has them followed **in that
commit's tree** instead. Not skipped: 24 of the 25 citations deferred as unfollowable when the
rule was written resolve at the commit their document declares, and the one that did not was a
real defect that had been added to the document two weeks after it was composed. Re-pointing
those 24 at today's files was the alternative, and it would have made a dated act claim to have
read code it never saw (SKEIN-600). A declared commit this repository no longer has is a skip,
named in the summary line — see `at_commit` for why that is not a failure.

WHAT IS CHECKED, four. A doc comment interrupted by an attribute and then resumed, which is the
seam where two items' docs were run together — the shape rule two structurally cannot see. See
`doc_interrupted`, which also records the generalisation that was measured and rejected.

WHAT IS NOT, and deliberately. Prose that describes something without naming it is untouched;
this is a spell-check for identifiers, not a fact-checker. It cannot tell you a sentence is
wrong about a function that still exists — only that the function is gone. Nor can it tell you
a citation points at the wrong line, only that the line is not there to point at.

TWO LISTS, and the difference between them is the point.

  * `docs/prose-symbols.toml` — names it is RIGHT to keep. Somebody ELSE'S (a kernel
    capability, a variable another tool sets), named in the PAST TENSE deliberately, or
    PROPOSED and not yet built. Permanent, and each one a decision written down once.
  * `docs/prose-debt.toml` — today's defects, recorded so the gate can be green about the tree
    as it is and red about anything added to it. Stale names whose files this gate's author
    could not edit, today's glued doc blocks, and today's unfollowable citations. Phase 5
    (SKEIN-523) empties the first two; every citation row names the item that removes it.

A stale entry in either fails too — an allow-list nobody prunes is a permission nobody granted.

And a THIRD thing that is neither, because it was tried as both and is wrong as both. A dated
review's citations are not debt: `docs/prose-debt.toml` requires every row to name the item that
removes it, and nothing will ever remove these — the fix is to leave them alone. Nor are they an
exemption: `docs/prose-symbols.toml` holds names nobody checks, and these are checkable, just not
here. So the declaration lives in the DOCUMENT, where the reader of the citation is, and the gate
follows it. The general shape, worth stating because the next dated document will want it: a claim
whose subject is a different tree needs the tree named beside the claim, not a list somewhere else
saying to stop asking.

    python3 tools/prose-check.py                     # the gate
    python3 tools/prose-check.py --show              # every finding, with where it is
    python3 tools/prose-check.py --update            # rewrite prose-symbols.toml from the tree
    python3 tools/prose-check.py --update-attachment # rewrite prose-debt.toml's glued-doc list
    python3 tools/prose-check.py --update-citations  # rewrite prose-debt.toml's citation list
"""

import json
import os
import re
import subprocess
import sys
import tomllib

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import rustcut  # noqa: E402 — the one cutter every gate shares, self-checked at import

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SPEC = os.path.join(ROOT, "docs", "prose-symbols.toml")
# Today's known prose defects — stale names and glued doc blocks — live in their own file rather
# than in `prose-symbols.toml`, for two reasons. They are a different KIND of entry: that file
# holds names it is right to keep, and this one holds work Phase 5 has to do. And `--update`
# rewrites that file from scratch, so a list kept there would be deleted by this tool's own
# maintenance command, which is the quietest way to lose a list.
DEBT = os.path.join(ROOT, "docs", "prose-debt.toml")

# Where a symbol may live. The page is both prose and code, so it is on both lists.
CODE_DIRS = ["src", "tests", "cockpit", "warden", "tools"]
CODE_SUFFIXES = (".rs", ".py", ".mjs", ".js", ".html", ".toml", ".sh", ".json")

# Backticked, and either qualified (`a::b`) or bare. The last segment is what is looked up.
BACKTICKED = re.compile(r"`([A-Za-z_][A-Za-z0-9_]*(?:::[A-Za-z_][A-Za-z0-9_]*)*)`")

# A comment in the page, which is where the page explains itself.
PAGE_COMMENT = re.compile(r"^\s*(?://|///)")

# A whole-line comment, per language. Trailing comments are deliberately NOT cut outside Rust:
# `#` is parameter expansion in shell (`${x#prefix}`) and a fragment in a URL, `//` is the middle
# of every `https://`, and a stripper that guesses deletes code — which invents findings instead
# of catching them. Rust is exact, because `rustcut` tokenises it properly.
LINE_COMMENT = {
    ".py": "#",
    ".sh": "#",
    ".toml": "#",
    ".mjs": "//",
    ".js": "//",
    ".html": "//",
}


def looks_like_a_symbol(name):
    """Shaped like something in this tree, rather than an English word in backticks.

    Tight on purpose. A bare word — `main`, `true`, `diff` — is how a gate like this drowns in
    its own output and gets switched off, so a name qualifies only by carrying an underscore or
    by being one of the page's two function prefixes. The cost is that a one-word Rust function
    named in prose is not checked; the benefit is that every hit is worth reading.
    """
    return "_" in name or name.startswith("rev") or name.startswith("api")


def without_comments(text, suffix):
    """`text` with its comments removed — the code, and nothing that merely talks about it.

    This is the WTS-8 fix. `leaf in code` used to be asked of every file's raw text, comments
    included, so a name the code no longer defines passed the gate whenever a stale comment still
    said it — one of the twelve WTS-8 measured has no definition anywhere in the tree, and one
    line of `src/place.rs` was enough to satisfy the check while a working document discussed it
    as live. The drift the gate exists for is precisely "the function went and the name stayed", and
    a comment is where the name most often stays.

    **Nothing in this file may spell a name that is under test.** A dead symbol written into a
    tool's own prose is code to `code_text()`, and the gate goes quiet on it — this docstring
    named three of them in its first draft and hid all three. Cite the finding id, not the name.
    """
    if suffix == ".rs":
        return rustcut.uncommented(text)
    marker = LINE_COMMENT.get(suffix)
    if marker is None:
        return text
    return "\n".join(
        "" if line.lstrip().startswith(marker) else line for line in text.split("\n")
    )


def code_text():
    out = []
    for d in CODE_DIRS:
        for base, dirs, files in os.walk(os.path.join(ROOT, d)):
            dirs[:] = [x for x in dirs if x not in ("node_modules", "target", ".git")]
            for f in files:
                if f.endswith(CODE_SUFFIXES):
                    try:
                        text = open(os.path.join(base, f), encoding="utf-8").read()
                    except (OSError, UnicodeDecodeError):
                        continue
                    out.append(without_comments(text, os.path.splitext(f)[1]))
    return "\n".join(out)


def rust_comment_lines(text):
    """Every line of `text` with everything that is NOT a comment blanked out.

    Line-aligned, so a finding can be reported as `file:line`. Walked with the shared tokeniser
    rather than matched with `^\\s*//`, because a trailing `// see foo_bar` is a claim about the
    code exactly as a full-line one is, and a `//` inside a string literal is not a comment at all.
    """
    out = [""] * (text.count("\n") + 1)
    i, n = 0, len(text)
    while i < n:
        if text.startswith("//", i) or text.startswith("/*", i):
            j = rustcut.skip_token(text, i)
            line = text.count("\n", 0, i)
            for k, piece in enumerate(text[i:j].split("\n")):
                out[line + k] += piece
            i = j
            continue
        past = rustcut.skip_token(text, i)
        i = past if (past is not None and past > i) else i + 1
    return out


def rust_files():
    """Every `.rs` file under `src/`, as (relative label, absolute path)."""
    src = os.path.join(ROOT, "src")
    for base, dirs, files in os.walk(src):
        dirs[:] = [d for d in dirs if d not in ("web", "store", "kit", "probe")]
        for f in sorted(files):
            if f.endswith(".rs"):
                path = os.path.join(base, f)
                yield os.path.relpath(path, ROOT), path


def prose_sources():
    """Every file whose prose is checked, as (label, lines).

    **A comment is prose.** Two functions removed in a949fdf went on being described as live by
    `src/repos.rs:35,849`, `src/fleet.rs:1744` and `src/store/sandbox-bootstrap.sh:90,106` — none
    of it caught, because comments were not a source and the code they were matched against still
    contained them (WTS-8). A comment that names a deleted function is the same defect as a
    document that does, and it reaches more readers. Their names are deliberately not written
    here: see `without_comments`.

    **The markdown at the REPO ROOT is prose too** (SKEIN-598/604), and it was the last of this
    project's prose that no gate read. That is backwards relative to the argument above: a stale
    name reaches more readers in a comment than in `docs/`, and more readers still in the README,
    which is the first file a stranger opens. `citation_sources` had already been widened to the
    root for the citation rule (SKEIN-583); this is the symbol half catching up, and the two now
    read the same set.

    What it cost to turn on, measured before the change and settled in it: four names in the
    root markdown, all four in the README. One is a POSIX socket option and was declared in
    `docs/prose-symbols.toml` as somebody else's name; one was declared there already. The other
    two were environment variables the README's own knob table promised, whose readers had been
    deleted (`78495f34`, SKEIN-521, which `docs/parity.md` §7 records; and `d0eb4581`, whose job
    `base_branch` now does from the config). Both rows are gone from the README rather than
    exempted here — a documented knob nothing reads is the defect this gate is for, not an
    exception to it.

    Their names are not written here for the reason `without_comments` gives: a docstring is not
    a `#` comment, so it is not cut out of `code_text`, and a name written into this file would
    make the tree appear to contain the very symbol the gate was asked about (WTS-8). Turning the
    rule on and watching it fail on this paragraph is how that was established, not assumed.
    """
    for f in sorted(os.listdir(ROOT)):
        path = os.path.join(ROOT, f)
        if f.endswith(".md") and os.path.isfile(path):
            yield f, open(path, encoding="utf-8").read().split("\n")
    docs = os.path.join(ROOT, "docs")
    for f in sorted(os.listdir(docs)):
        if f.endswith(".md"):
            path = os.path.join(docs, f)
            yield os.path.relpath(path, ROOT), open(path, encoding="utf-8").read().split("\n")
    page = os.path.join(ROOT, "src", "web", "index.html")
    if os.path.exists(page):
        lines = open(page, encoding="utf-8").read().split("\n")
        # Only the comments: the page's own source names its own functions constantly, and a
        # function that is defined three lines down is not a claim about anything.
        yield "src/web/index.html", [l if PAGE_COMMENT.match(l) else "" for l in lines]
    for label, path in rust_files():
        yield label, rust_comment_lines(open(path, encoding="utf-8").read())
    store = os.path.join(ROOT, "src", "store")
    if os.path.isdir(store):
        for f in sorted(os.listdir(store)):
            if not f.endswith(".sh"):
                continue
            lines = open(os.path.join(store, f), encoding="utf-8").read().split("\n")
            yield "src/store/" + f, [l if l.lstrip().startswith("#") else "" for l in lines]


def code_has(leaf, code):
    """Whether the tree has `leaf` as a whole identifier, rather than inside a longer one.

    `leaf in code` was the test until SKEIN-610, and a plain substring is satisfied by any FRAGMENT
    of a real name. `tests/platform_gates.rs:21` is the case that made it concrete: a module note
    telling a reader on a Mac which test name to look for in `cargo test` output gave a name three
    words shorter than the one printed, and the gate was green because the short string sits inside
    the long one. The whole class was seven sites on the day this was written — small, as expected,
    since the fragment has to be a real identifier fragment to pass at all.

    AN AFFIX IS ANCHORED AT ITS INNER EDGE ONLY. Prose here names a family of functions by their
    shared prefix, and a naming convention by its shared suffix, and both are written with the
    underscore facing the part that is left out. That is a claim about real identifiers, so it is
    kept CHECKED rather than exempted — a suffix nothing in the tree carries is the same defect as
    a function nothing defines — but it cannot be anchored on the side where the rest of the name
    was deliberately dropped. So a leading `_` drops the left boundary and a trailing `_` drops the
    right. Six of the twelve names the boundary rule first lit up are this shape, and all six
    resolve; the other six were real, and are fixed or recorded.

    `\\b` is the right boundary for this tree: it treats `_` as a word character, which is what a
    Rust identifier needs. Nothing else it could mishandle reaches here — `BACKTICKED` matches only
    an identifier or a `::`-qualified path between the backticks, so a macro's `!`, a `?`, a method
    call's parentheses and a field access's `.` all fail to match in the first place and never
    become a `leaf`. A macro or a method IS found, because `\\b` ends at the `!` or the `(`.

    No name that is under test may be spelled in this docstring: a docstring is not a `#` comment,
    so it is not cut out of `code_text`, and writing one here would make the tree appear to contain
    it (WTS-8). The sites are cited, never quoted.
    """
    left = "" if leaf.startswith("_") else r"\b"
    right = "" if leaf.endswith("_") else r"\b"
    return re.search(left + re.escape(leaf) + right, code) is not None


def absent(code=None, sources=None):
    """{symbol: [where, …]} for every named symbol the tree does not have.

    `code` and `sources` are injectable so `self_check` can run the rule against a tree that exists
    only in the fixture — the same seam `bad_citations` has, and for the same reason: a rule that
    can only be run against this repository is demonstrated rather than proved, and this one was
    made STRICTER, so before-and-after on a green gate shows nothing at all.
    """
    code = code_text() if code is None else code
    found = {}
    for label, lines in (prose_sources() if sources is None else sources):
        for n, line in enumerate(lines, 1):
            for name in BACKTICKED.findall(line):
                leaf = name.rsplit("::", 1)[-1]
                if not looks_like_a_symbol(leaf) or code_has(leaf, code):
                    continue
                found.setdefault(leaf, []).append(f"{label}:{n}")
    return found


# --------------------------------------------------------------------------------------------
# The doc-attachment rule (FLEET-1, CLI-7, PRP-7, REV-5, SRV-8 — one class, five ledgers).
#
# An item was moved or deleted and its doc comment stayed. The block below it was then written
# directly underneath, and rustdoc — which treats every `///` line as a `#[doc]` attribute on the
# next item — renders both paragraphs on that one item. The function the first block describes is
# left undocumented; the neighbour carries two or three docs run together. About fifty sites.
#
# **How it is detected.** The two blocks are a single unbroken run of `///` lines, so there is no
# separator to look for. What there is, is a *summary line in the middle of a run*: this tree's
# doc style is "one-sentence summary, then an empty `///`, then the body", and the second block's
# summary keeps that shape when it is glued on. So the rule looks for a `///` line that
#
#   * is immediately followed by an empty `///` (it ends a paragraph),
#   * is immediately preceded by a non-empty `///` (it is not the start of the run),
#   * follows a line that ENDS A SENTENCE and starts one of its own, and
#   * follows a line that had room for its first word.
#
# The last is what separates a new block from the last line of a wrapped paragraph. Inside a
# paragraph every line but the last runs to the wrap column; if the previous line stops short and
# the next word would have fitted on it, the break was not a wrap, and the line after it is not a
# continuation. Lists, tables and fenced code blocks are excluded — their short lines are
# deliberate.
#
# **What makes it fire**: gluing a `/// Summary.` + `///` pair onto the end of any doc block
# whose last line has room for the word "Summary". `SELF_CHECK_ATTACH` is that change, made.
#
# It has false negatives — a glued block whose predecessor happens to end at the wrap column is
# missed — and that is the right way round for a rule with a recorded allow-list: a miss costs
# one unfixed site in Phase 5, a false positive costs a rubber-stamped exemption.
# --------------------------------------------------------------------------------------------

# rustfmt's `max_width`, which is what this tree's prose is wrapped to. Read rather than assumed,
# so that changing the setting changes the rule instead of quietly breaking it.
def wrap_column():
    conf = os.path.join(ROOT, "rustfmt.toml")
    if os.path.exists(conf):
        m = re.search(r"^max_width\s*=\s*(\d+)", open(conf, encoding="utf-8").read(), re.M)
        if m:
            return int(m.group(1))
    return 100


WRAP = wrap_column()

# A markdown list item, a numbered step, or a table row: short lines here are the format, not a
# paragraph ending.
LIST_ITEM = re.compile(r"^(?:[*+-]\s|\d+[.)]\s|\|)")
ENDS_A_SENTENCE = re.compile(r"[.!?:][`)*\"']*$")
STARTS_A_SENTENCE = re.compile(r"[A-Z`*\[]")


def doc_body(line):
    """The text of a `///` line, or None if the line is not one. One leading space is the marker's
    own; anything beyond it is indentation and is kept, because it marks a continuation."""
    s = line.lstrip()
    if not s.startswith("///"):
        return None
    t = s[3:]
    return t[1:] if t.startswith(" ") else t


def doc_attachment(text):
    """[(line, text)] for every doc block glued onto the end of the one above it."""
    lines = text.split("\n")
    out, fenced = [], False
    for k in range(1, len(lines) - 1):
        before, here, after = (doc_body(lines[i]) for i in (k - 1, k, k + 1))
        if here is None:
            continue
        if here.strip().startswith("```"):
            fenced = not fenced
            continue
        if fenced or before is None or after is None:
            continue
        if not before.strip() or not here.strip() or after.strip():
            continue
        if LIST_ITEM.match(before) or LIST_ITEM.match(here):
            continue
        if before.startswith(" ") or here.startswith(" "):
            continue
        if not ENDS_A_SENTENCE.search(before.rstrip()):
            continue
        if not STARTS_A_SENTENCE.match(here.strip()):
            continue
        if len(lines[k - 1]) + 1 + len(here.split()[0]) > WRAP:
            continue
        out.append((k + 1, here.strip()))
    return out


# --------------------------------------------------------------------------------------------
# The interrupted-doc rule (SKEIN-582), a second shape of the same defect.
#
# `reading_now` opened with two lines of `spend_a_visit`'s doc, then
# `#[allow(clippy::too_many_arguments)]`, then its own. `doc_attachment` above structurally could
# not see it: the two blocks are ONE contiguous `///` run in its eyes, with no summary line in the
# middle, so the shape it looks for is not there. `spend_a_visit` was left undocumented, and the
# `allow` was inert where it had landed — the function beneath it takes no arguments at all.
#
# WHAT THIS LOOKS FOR: a `///` line, then an attribute, then a `///` line. Nobody writes a doc
# comment, interrupts it with an attribute, and resumes; when it happens, two items' worth of
# lines have been run together and the attribute is the seam. Measured across every tracked `.rs`
# file in the repo: one site, the one above, and no others. A rule with no false positives on the
# whole tree needs no allow-list, so this one simply fails.
#
# WHAT WAS REJECTED, and why it is worth writing down. SKEIN-582 proposed the general form: flag a
# doc block whose first sentence names a symbol that is not the item it sits on. Measured before
# implementing — 254 sites, and reading them shows almost every one is a correct doc that opens by
# relating its item to a neighbour ("The pair a [`Summary`] carries", "Parsed strictly — see
# [`parse_stage1`]"). Worse, it would have missed one of the two defects it was invented for:
# `tried_path` wore an older `summarise` doc that names no symbol at all, so nothing to match.
# A rule that misses half its motivating cases and reports 253 false ones is not a gate.
# --------------------------------------------------------------------------------------------


def doc_interrupted(text):
    """[(line, text)] for every `///` run that resumes after an attribute line."""
    lines = text.split("\n")
    out = []
    for k in range(1, len(lines) - 1):
        if not lines[k].lstrip().startswith("#["):
            continue
        if doc_body(lines[k - 1]) is None or doc_body(lines[k + 1]) is None:
            continue
        out.append((k + 1, lines[k].strip()))
    return out


def doc_interruptions():
    """{label: [(line, text)]} over every `.rs` file under `src/`."""
    found = {}
    for label, path in rust_files():
        hits = doc_interrupted(open(path, encoding="utf-8").read())
        if hits:
            found[label] = hits
    return found


def doc_attachments():
    """{label: [(line, text)]} over every `.rs` file under `src/`."""
    found = {}
    for label, path in rust_files():
        hits = doc_attachment(open(path, encoding="utf-8").read())
        if hits:
            found[label] = hits
    return found


# --------------------------------------------------------------------------------------------
# The citation rule (SKEIN-583, SKEIN-585, SKEIN-586).
#
# CLAUDE.md's first rule tells prose to cite the file and line rather than paraphrase, and this
# tree took it at its word. Nothing ever checked one of those citations. When `src/review.rs`
# became `src/review/`, an agent sampled three of the citations it carried and all three were
# already wrong BEFORE the split — one by about 530 lines. The module splits did not cause this;
# they made it countable, by turning silent line drift into named files that are simply gone.
#
# THREE STRICTNESSES, and only two of them can be a gate.
#
#   * A citation naming a file NOT ON DISK is unambiguously wrong: there is nothing a reader can
#     do with it. FAILS.
#   * A line PAST THE END of the file it names is unambiguously wrong for the same reason. FAILS.
#   * A line that exists but does not hold what the sentence says it holds CANNOT BE CHECKED
#     mechanically — that needs a reader who understands the sentence. This rule does not pretend
#     to. `src/review/cache.rs` cited `src/review.rs:1570-1573` for a cache lookup that lived at
#     2101, and a file-and-line check would have called that citation perfectly good. So GREEN
#     HERE MEANS FOLLOWABLE, NEVER RIGHT — the same boundary `absent()` draws between a
#     spell-check and a fact-checker.
#
# The consequence worth stating: this rule cannot make line citations trustworthy, only
# followable. Where a citation can be written as a `symbol_name` instead it should be, because
# `absent()` above checks the thing the sentence is actually about, and a symbol survives every
# edit that moves it. The line-number form is for what genuinely has no name — a comment, a
# match arm, a range.
#
# HOW A PATH IS RESOLVED — from the repo root first, then by its tail. Prose writes
# `place.rs:394` and `bin/skein-server.rs:110` as readily as `src/place.rs:394`, and all three
# name one file to a reader. So: a path that IS a file, relative to the root, is that file; the
# root is what a repo-relative citation means and nothing may outvote it. Otherwise the rule
# looks for exactly one file whose path ends, at a `/` boundary, with the cited path.
#
# Two tail matches — `lib.rs` is both `src/lib.rs` and `warden/src/lib.rs` — means the citation
# does not identify a file, and the rule SKIPS it rather than guessing or failing: guessing runs
# the line check against the wrong file and reports a fabricated defect, and failing would demand
# a rewrite of prose a reader can already follow. `--show` counts the skipped ones, so ambiguity
# that grows is visible rather than absorbed. Trying the root first is what keeps that class
# small: without it `src/lib.rs` is "ambiguous" because `warden/src/lib.rs` ends with it, which
# would have silently un-checked four correct citations.
#
# WHERE A CITATION IS LOOKED FOR — every text file under `docs/`, `src/`, `tests/`, `tools/`,
# `cockpit/` and `warden/`, RAW: comments and code alike. This is where the rule parts company
# with `prose_sources()` above, deliberately. A symbol name appearing in code IS the code, which
# is why WTS-8 forced comments to be stripped before the symbol check; `foo.rs:123` is not valid
# syntax in any language here, so there is nothing to strip and no way for the code to satisfy a
# claim made about it. Reading raw also reaches where `prose_sources()` deliberately does not:
# a dead citation sat inside a docstring in `tools/rustcut.py`, and nine more in `tests/ui/`.
#
# WHERE ELSE: the markdown at the repo ROOT, which `prose_sources()` does not read — see
# `citation_sources`.
#
# EXCEPT a fenced code block in markdown, which is a transcript or a mockup rather than a
# sentence: `docs/review-ux.md` draws a UI mockup whose window shows a file that document is only
# PROPOSING. Same exclusion, and the same reason, as `doc_attachment` makes for fences.
#
# AND EXCEPT two files: this one and the debt list it writes. A rule about unfollowable citations
# has to be able to WRITE one to demonstrate it — `SELF_CHECK_CITATIONS` is made of nothing else,
# and `docs/prose-debt.toml` is a file whose every row quotes a citation that cannot be followed.
# Left in scope, the debt file reports each row it records as a fresh finding and the list grows
# by being written; and the fixture would have to be deleted or the tool exempted in the debt
# list, which is how an exemption table gets its first rubber stamp. `without_comments` records
# the mirror-image hazard for the symbol rule: a tool must not spell what it is testing for.
#
# The cost is that a real citation written in either file goes unchecked, so do not write one:
# name the symbol instead, which is this rule's advice to every other file too.
# --------------------------------------------------------------------------------------------

# `path/to/file.ext:123` or `:123-456`. The extension list is what keeps this from matching a
# clock time or a `host:port`: the shape has to end in a file name this repo could contain.
CITATION = re.compile(
    r"(?<![A-Za-z0-9_./\\-])"
    r"((?:[A-Za-z0-9_.\-]+/)*[A-Za-z0-9_.\-]+\.(?:rs|mjs|js|py|sh|toml|md|html|json))"
    r":(\d+)(?:-(\d+))?"
)

# Directories that are not this repository's own text. `upstream/` is a submodule and `target/`
# is build output; a citation must not be allowed to resolve into either, and neither may add a
# second candidate that turns a good citation ambiguous.
NOT_OURS = (".git", "node_modules", "target", ".target", "upstream", "__pycache__")

# Where citations are read from. Wider than `prose_sources()`, for the reason argued above.
CITATION_DIRS = ["docs", "src", "tests", "tools", "cockpit", "warden"]
CITATION_SUFFIXES = (".rs", ".mjs", ".js", ".py", ".sh", ".toml", ".md", ".html", ".json")


def tree_files():
    """Every repo-relative file path, for resolving a citation against."""
    out = []
    for base, dirs, files in os.walk(ROOT):
        dirs[:] = [d for d in dirs if d not in NOT_OURS]
        for f in files:
            out.append(os.path.relpath(os.path.join(base, f), ROOT))
    return out


def resolve(cited, index):
    """The one file `cited` names, `""` if it names none, or None if it names several.

    `index` is a list of repo-relative paths. A path that is itself in the index wins outright —
    a repo-relative citation means what it says. Otherwise matching is on the tail at a `/`
    boundary, so `place.rs` finds `src/place.rs` and `bin/skein-server.rs` finds
    `src/bin/skein-server.rs`, while `hing.rs` does NOT find `src/thing.rs` — a suffix match
    without the boundary lets any name resolve to a longer one that merely ends with its letters.
    """
    if cited in index:
        return cited
    hits = [p for p in index if p.endswith("/" + cited)]
    if len(hits) == 1:
        return hits[0]
    return "" if not hits else None


def citations(text, markdown):
    """[(line, cited_path, highest_line_cited, whole_text)] for each citation in `text`.

    The HIGHEST line of a range is what gets checked: `foo.rs:10-9999` is wrong about a
    100-line file even though line 10 is fine, and a range's end is as much a claim as its
    start.
    """
    out, fenced = [], False
    for n, line in enumerate(text.split("\n"), 1):
        if markdown and line.lstrip().startswith("```"):
            fenced = not fenced
            continue
        if fenced:
            continue
        for m in CITATION.finditer(line):
            path, first, last = m.group(1), int(m.group(2)), m.group(3)
            out.append((n, path, max(first, int(last or first)), m.group(0)))
    return out


def citation_sources():
    """[(label, text, is_markdown)] for every file a citation is read from."""
    out = []
    # The markdown at the REPO ROOT — README, ARCHITECTURE, VISION, CONTRIBUTING, SECURITY,
    # CHANGELOG. `prose_sources()` above reads `docs/*.md` and stops there, so these are the one
    # piece of this project's prose no gate looks at, and they are the prose a stranger reads
    # first. They carry no citations today, so including them costs nothing and closes the door;
    # the symbol half of the same gap is measured in SKEIN-604 and is not free.
    for f in sorted(os.listdir(ROOT)):
        path = os.path.join(ROOT, f)
        if f.endswith(".md") and os.path.isfile(path):
            try:
                out.append((f, open(path, encoding="utf-8").read(), True))
            except (OSError, UnicodeDecodeError):
                pass
    for d in CITATION_DIRS:
        for base, dirs, files in os.walk(os.path.join(ROOT, d)):
            dirs[:] = [x for x in dirs if x not in NOT_OURS]
            for f in sorted(files):
                if not f.endswith(CITATION_SUFFIXES):
                    continue
                path = os.path.join(base, f)
                # See the block above: the rule cannot be its own subject, and neither can the
                # list of what it found — every row there quotes a citation that cannot be
                # followed, so in scope the list would grow one finding per row it recorded.
                if os.path.abspath(path) in (os.path.abspath(__file__), os.path.abspath(DEBT)):
                    continue
                try:
                    out.append(
                        (
                            os.path.relpath(path, ROOT),
                            open(path, encoding="utf-8").read(),
                            f.endswith(".md"),
                        )
                    )
                except (OSError, UnicodeDecodeError):
                    continue
    return out


# A DATED REVIEW declares the commit its `file:line` citations are claims about, and this is the
# sentence that declares it. Written out in full so the note a person reads and the note the gate
# reads are the same characters: a machine-only marker in an HTML comment could disagree with the
# prose beside it, which is the drift class this whole tool exists for.
#
# WHY THIS IS NOT THE DOCUMENT'S "written against X" LINE, which both dated reviews already carry
# and which would have been free. Those are different claims. "Written against `12ae61a`" says when
# the document was composed; this says what its citations MEAN. `docs/review-ux.md` is the proof
# that the two come apart: it was written against `e310a24`, and two weeks later a commit replaced
# a symbol in it with `src/prq.rs:1722` — a line that never existed at `e310a24` and was a claim
# about the tree that day. Reading the composition date as a licence over every citation would have
# made that one correct-by-declaration instead of the finding it was.
CITATIONS_AT = re.compile(
    r"citations in this document name lines as they stood at `([0-9a-f]{7,40})`", re.I
)


def at_commit(commit):
    """`(index, length_of)` for the tree at `commit`, or `None` if this repository has not got it.

    The point of resolving a dated review's citations HERE rather than against the working tree is
    that it is the only reading under which they are true, and it is still a check: 24 of the 25
    deferred citations resolved at the commit their document declares, and the one that did not was
    a real defect (see `CITATIONS_AT`).

    **`None` is a skip, not a failure, and the summary line says how many were skipped.** A commit
    can leave a repository — a squash before publication is the obvious way, and this repo is
    heading for one (SKEIN-492). Failing then would break the build over history nobody can restore
    from inside the gate, and the citations would be unverifiable by ANY means at that point, so a
    red build would report a defect that no edit to the prose could fix. A typo'd sha lands in the
    same place, which is the cost of the choice: it is counted and named rather than silent.
    """
    try:
        listing = subprocess.run(
            ["git", "-C", ROOT, "ls-tree", "-r", "-z", "--name-only", commit],
            capture_output=True,
            text=True,
            errors="replace",
            timeout=60,
        )
    except (OSError, subprocess.SubprocessError):
        return None
    if listing.returncode != 0:
        return None
    index = [p for p in listing.stdout.split("\0") if p]
    seen = {}

    def length_of(rel):
        if rel not in seen:
            got = subprocess.run(
                ["git", "-C", ROOT, "show", f"{commit}:{rel}"],
                capture_output=True,
                text=True,
                errors="replace",
            )
            body = got.stdout if got.returncode == 0 else ""
            seen[rel] = body.count("\n") + (0 if not body or body.endswith("\n") else 1)
        return seen[rel]

    return index, length_of


def bad_citations(index=None, sources=None, lengths=None, at=at_commit, notes=None):
    """{label: [(line, text, why)]} for every citation that cannot be followed.

    `index`, `sources`, `lengths` and `at` are injectable so `self_check` can run the whole rule
    against a tree it made up — see `SELF_CHECK_CITATIONS`. `sources` is
    [(label, text, is_markdown)]; `lengths` is {path: line count}; `at` is `at_commit`'s shape.
    `notes`, if given, collects `(label, commit, "checked" | "absent")` for every source that
    declared one, so the caller can say in the summary how many documents were read against a
    commit of their own and how many asked for one this repository no longer has.
    """
    index = tree_files() if index is None else index
    sources = citation_sources() if sources is None else sources
    lengths = {} if lengths is None else dict(lengths)

    def length_of(rel):
        if rel not in lengths:
            try:
                body = open(os.path.join(ROOT, rel), encoding="utf-8", errors="replace").read()
            except OSError:
                return 0
            # A file with no trailing newline still has a last line; one with a trailing newline
            # does not have an empty line after it. Both are `count("\n") + 1` minus that case.
            lengths[rel] = body.count("\n") + (0 if body.endswith("\n") else 1)
        return lengths[rel]

    found = {}
    for label, body, markdown in sources:
        here, measure, when = index, length_of, ""
        dated = CITATIONS_AT.search(body)
        if dated:
            commit = dated.group(1)
            tree = at(commit) if at else None
            if notes is not None:
                notes.append((label, commit, "absent" if tree is None else "checked"))
            if tree is None:
                # The skip `at_commit` documents, and it has to happen HERE. Falling through
                # would resolve a dated review's citations against TODAY's tree — the one
                # reading under which they are knowingly false — and report every one as a
                # defect no edit could fix. It only shows where the commit is unreachable,
                # which is not this box: `actions/checkout@v4` clones shallow, so CI is the
                # first place this rule ever ran without the history it asks for.
                continue
            here, measure = tree
            when = " at " + commit
        for line, path, cited, text in citations(body, markdown):
            target = resolve(path, here)
            if target is None:  # ambiguous — the citation names no one file. Counted, not failed.
                continue
            if target == "":
                found.setdefault(label, []).append((line, text, "no such file" + when))
            elif cited > measure(target):
                found.setdefault(label, []).append(
                    (line, text, "%s has %d line(s)%s" % (target, measure(target), when))
                )
    return found


def ambiguous_citations(index=None):
    """[(label, line, text)] for citations that name more than one file — reported, never failed.

    A document that declares a commit under `CITATIONS_AT` is skipped whole: its citations are
    resolved against a different tree, so ambiguity measured against this one would be a report
    about a question nobody asked.
    """
    index = tree_files() if index is None else index
    out = []
    for label, body, markdown in citation_sources():
        if CITATIONS_AT.search(body):
            continue
        for line, cited, _, text in citations(body, markdown):
            if resolve(cited, index) is None:
                out.append((label, line, text))
    return out


def dated_note(notes):
    """What the summary line says about documents read against a commit of their own.

    Silent when there are none, so the ordinary run reads exactly as it did. A skip is named with
    the document and the commit rather than counted, because a skipped document is a document
    nobody is checking and that is worth one line of anybody's attention.
    """
    checked = [label for label, _, how in notes if how == "checked"]
    missing = [(label, commit) for label, commit, how in notes if how == "absent"]
    said = ""
    if checked:
        said += f"; {len(checked)} dated review(s) read against the commit each declares"
    for label, commit in missing:
        said += f"; {label} declares {commit}, which is not in this repository — NOT CHECKED"
    return said


def load_debt():
    """`docs/prose-debt.toml`: (stale-symbol names, {file: [texts]}, {file: [texts]}).

    The third is the citation list. Its rows must each name the work item that removes them —
    see `check_citation_debt`.
    """
    if not os.path.exists(DEBT):
        return None, None, None
    with open(DEBT, "rb") as f:
        debt = tomllib.load(f)
    attached = {}
    for row in debt.get("doc-attachment", []):
        attached.setdefault(row["file"], []).append(row["text"])
    cited = {}
    for row in debt.get("stale-citation", []):
        cited.setdefault(row["file"], []).append(row["text"])
    return debt.get("stale-symbol", {}), attached, cited


def check_citation_debt():
    """Problems with the debt list itself: a row that does not name the work item that ends it.

    This is the whole anti-permanence mechanism, so it is a rule and not a convention. An
    exemption table with no owner is how a gate becomes a gate nobody reads: entries go in, the
    build is green, and five years later the list IS the standard. A row here costs one work
    item id, which means somebody had to decide the defect was scheduled rather than accepted.
    """
    if not os.path.exists(DEBT):
        return []
    with open(DEBT, "rb") as f:
        debt = tomllib.load(f)
    out = []
    for row in debt.get("stale-citation", []):
        if not re.match(r"^[A-Z]+-\d+$", str(row.get("item", ""))):
            out.append(
                "prose-check: docs/prose-debt.toml records the citation `%s` in %s with no "
                "`item`\n"
                "             rule: every deferred citation names the work item that removes "
                "it. A row with no owner is how this list stops being debt and becomes the "
                "standard" % (row.get("text", "?"), row.get("file", "?"))
            )
    return out


def render_citations(found, owners):
    """The `[[stale-citation]]` rows. `owners` is {(file, text): item}, carried over so that a
    regeneration does not silently drop the ids a person wrote — the same courtesy `render`
    does for `prose-symbols.toml`'s reasons, and for the same reason: a maintenance command
    that erases the human half of a file is how the human half stops being written."""
    out = []
    for label in sorted(found):
        for line, text, why in found[label]:
            out.append("[[stale-citation]]")
            out.append('file = "%s"' % label)
            out.append("line = %d" % line)
            out.append("text = %s" % json.dumps(text, ensure_ascii=False))
            out.append('item = "%s"' % owners.get((label, text), "TODO-0"))
            out.append("reason = %s" % json.dumps(why, ensure_ascii=False))
            out.append("")
    return "\n".join(out)


def update_citations(found):
    """Rewrite everything between `# @STALE-CITATION` and the doc-attachment heading."""
    text = open(DEBT, encoding="utf-8").read()
    start = "# @STALE-CITATION\n"
    end = "\n# ---- doc-attachment"
    head = text[: text.index(start) + len(start)]
    tail = text[text.index(end) :]
    with open(DEBT, "rb") as f:
        owners = {
            (row["file"], row["text"]): row.get("item", "TODO-0")
            for row in tomllib.load(f).get("stale-citation", [])
        }
    open(DEBT, "w", encoding="utf-8").write(head + "\n" + render_citations(found, owners) + tail)
    n = sum(len(v) for v in found.values())
    print("wrote %s (%d citation(s))" % (os.path.relpath(DEBT, ROOT), n))


def render_attachment(found):
    out = []
    for label in sorted(found):
        for line, text in found[label]:
            out.append("[[doc-attachment]]")
            out.append('file = "%s"' % label)
            out.append("line = %d" % line)
            out.append("text = %s" % json.dumps(text, ensure_ascii=False))
            out.append("")
    return "\n".join(out)


def update_attachment(found):
    """Rewrite everything from `# @DOC-ATTACHMENT` to the end of the debt file."""
    text = open(DEBT, encoding="utf-8").read()
    marker = "# @DOC-ATTACHMENT\n"
    head = text[: text.index(marker) + len(marker)]
    open(DEBT, "w", encoding="utf-8").write(head + "\n" + render_attachment(found))
    n = sum(len(v) for v in found.values())
    print("wrote %s (%d site(s))" % (os.path.relpath(DEBT, ROOT), n))


def load_spec():
    """The exemption file: {symbol: reason}. Parsed by hand — this is two shapes of line."""
    if not os.path.exists(SPEC):
        return None
    spec, reason = {}, []
    for line in open(SPEC, encoding="utf-8"):
        if line.startswith("#"):
            reason.append(line.lstrip("# ").rstrip())
            continue
        m = re.match(r'^([A-Za-z_][A-Za-z0-9_]*)\s*=\s*"(.*)"\s*$', line)
        if m:
            spec[m.group(1)] = m.group(2)
        if not line.strip():
            reason = []
    return spec


def render(found, spec, debt):
    out = [
        "# Symbols this project's prose names that the code does not have. Read by",
        "# `tools/prose-check.py`, which fails the build on any other one.",
        "#",
        "# Three things belong here and nothing else:",
        "#",
        "#   * a name that is SOMEBODY ELSE'S — a kernel capability, a variable another tool sets.",
        "#     The tree will never contain it and should not be made to.",
        "#   * a name this tree discusses in the PAST TENSE, deliberately. A design record that",
        "#     explains why a thing was removed has to be able to say what it was called.",
        "#   * a name for something NOT BUILT YET, in a document that is proposing it.",
        "#",
        "# Anything else is drift, and the point of the gate is that it stops rather than",
        "# accumulating. `--update` writes the names; the reasons are written by a person.",
        "#",
        "# A name already recorded in docs/prose-debt.toml is NOT written here: that one is a",
        "# defect with a schedule, and copying it into this file would retire it by declaring",
        "# it fine.",
        "",
    ]
    for name in sorted(found):
        if name in debt:
            continue
        why = (spec or {}).get(name, "TODO: say why the code does not have this")
        out.append(f'{name} = "{why}"')
    return "\n".join(out) + "\n"


# A doc block with a second one glued onto the end of it, and the same text written correctly.
# Held as a fixture and run on every invocation: this rule's failure mode is to find nothing and
# be believed, and the allow-list it feeds is one Phase 5 has to trust.
#
# The concrete change that makes it fire is the difference between these two strings — one `///`
# line moved from before the second summary to after it.
SELF_CHECK_ATTACH = '''/// What the first item does, in a sentence.
///
/// The body of the first block, which runs on for a line or two and ends here.
/// What the SECOND item does, in a sentence.
///
/// The body of the second block.
pub fn only_item() {}
'''

SELF_CHECK_CLEAN = '''/// What the first item does, in a sentence.
///
/// The body of the first block, which runs on for a line or two and ends here.
pub fn first_item() {}

/// What the SECOND item does, in a sentence.
///
/// The body of the second block.
pub fn second_item() {}
'''

# The shapes that must NOT fire: a wrapped paragraph whose last line is short only because the
# next word did not fit, a markdown list, and a fenced code block.
SELF_CHECK_INNOCENT = (
    "/// A summary.\n"
    "///\n"
    "/// %s\n"
    "/// Continued.\n"
    "///\n"
    "/// * a list item.\n"
    "/// * another, which ends a sentence.\n"
    "/// * a third.\n"
    "///\n"
    "/// ```text\n"
    "/// short line.\n"
    "/// Another line.\n"
    "/// ```\n"
    "///\n"
    "pub fn innocent() {}\n"
) % ("x" * (WRAP - 12))


# The citation rule, run against a tree that exists only here — three files, two of them sharing
# a name. Held as a fixture and run on every invocation for the reason the attachment fixture is:
# a rule of this shape fails by finding NOTHING and being believed, and the debt list it feeds is
# one a later phase has to trust.
#
# The concrete changes that make each assertion below fail: dropping the `endswith` line check
# (`thing.rs:101` stops being a finding), dropping the empty-`hits` case (`gone.rs:1` stops being
# one), taking `first` instead of `max` (`thing.rs:90-200` stops being one), matching a bare
# suffix without the `/` boundary (`hing.rs:1` resolves to `src/thing.rs` and stops being one),
# skipping ambiguity (`lib.rs:7` becomes a finding against whichever file sorted first), and
# honouring fences outside markdown or not at all (the fenced and the `.rs` lines swap places).
SELF_CHECK_INDEX = ["src/thing.rs", "src/lib.rs", "warden/src/lib.rs"]
SELF_CHECK_LENGTHS = {"src/thing.rs": 100, "src/lib.rs": 10, "warden/src/lib.rs": 10}
SELF_CHECK_CITATIONS = [
    (
        "fixture.md",
        "A citation of `thing.rs:50` is fine.\n"
        "`thing.rs:101` is one line past the end.\n"
        "`gone.rs:1` names no file at all.\n"
        "`lib.rs:7` names two, so it names none.\n"
        "```\n"
        "`gone.rs:2` is inside a fence and is a transcript, not a claim.\n"
        "```\n"
        "`thing.rs:90-200` starts inside the file and ends past it.\n"
        "`hing.rs:1` is not `thing.rs` with a letter missing, it is another file.\n",
        True,
    ),
    ("fixture.rs", "let x = 0; // and a comment citing `gone2.rs:1`\n", False),
]


# A dated review, and the tree its declaration points at. `thing.rs` was longer then and `old.rs`
# existed then, so BOTH of its citations are unfollowable against `SELF_CHECK_INDEX` and both are
# fine against the declared commit — which is the whole claim the rule makes. `gone.rs:1` is in it
# so the fixture can also show the rule still FAILING inside a dated review: a declaration is a
# different tree to check against, never permission to stop checking.
SELF_CHECK_DATED = (
    "dated.md",
    "Written long ago. Citations in this document name lines as they stood at `abc1234`.\n"
    "`thing.rs:140` was inside the file then.\n"
    "`old.rs:3` was a file then.\n"
    "`gone.rs:1` was never a file in either tree.\n",
    True,
)
SELF_CHECK_THEN = (
    ["src/thing.rs", "src/old.rs", "src/lib.rs", "warden/src/lib.rs"],
    {"src/thing.rs": 200, "src/old.rs": 10, "src/lib.rs": 10, "warden/src/lib.rs": 10},
)


# One function, and five sentences about it — one of them naming three words less of its name than
# it has. That truncation, and a name the fixture tree has nothing like, are the two findings; the
# full name and the two affixes are not. The concrete changes that make the assertion fail: matching
# a bare substring again (the truncation stops being a finding), and anchoring an affix on the side
# its name was cut on (both affixes become findings).
SELF_CHECK_SYMBOL_CODE = "fn a_fixture_gate_that_is_named_in_full_and_then_some() {}\n"
SELF_CHECK_SYMBOL_PROSE = [
    (
        "fixture.rs",
        [
            "// The gate is `a_fixture_gate_that_is_named_in_full`, which is not what it is called.",
            "// `a_fixture_gate_that_is_named_in_full_and_then_some` is the name it actually has.",
            "// `_and_then_some` is a suffix: the part left out is on the left of it.",
            "// `a_fixture_gate_` is a prefix, and the part left out is on the right.",
            "// `a_fixture_gate_that_never_existed` is in the tree under no reading at all.",
        ],
    )
]


# Two items' docs run together with an attribute at the seam, and the same lines written
# correctly. The concrete change that makes the first assertion fail is deleting the attribute
# line from `SELF_CHECK_INTERRUPTED`; the second fails if the rule stops requiring a `///` on
# BOTH sides, since every attribute in the tree that follows a doc comment would then fire.
SELF_CHECK_INTERRUPTED = '''/// What the first item does, in a sentence.
#[allow(some::lint)]
/// What the SECOND item does, in a sentence.
pub fn only_item() {}
'''

SELF_CHECK_UNINTERRUPTED = '''/// What the first item does, in a sentence.
#[allow(some::lint)]
pub fn first_item() {}

/// What the second item does, in a sentence.
pub fn second_item() {}
'''


def self_check():
    # THE SYMBOL RULE, against a tree of one function. The prose beside it names a TRUNCATION of
    # that function's name, which is the whole of SKEIN-610: a name that is only a prefix of a real
    # one used to pass, so a module note could tell a reader to look for a test name that is never
    # printed. Run on every invocation, because this rule was made stricter and a stricter green
    # gate is still green — the only proof it rejects anything is an input it rejects.
    #
    # The names are INVENTED rather than quoted from the tree: a string literal in this file is code
    # to `code_text` (see `without_comments`), so quoting the real truncation would put it in the
    # tree and switch the gate off for the very finding under test.
    symbols = absent(SELF_CHECK_SYMBOL_CODE, SELF_CHECK_SYMBOL_PROSE)
    want_symbols = {
        "a_fixture_gate_that_is_named_in_full": ["fixture.rs:1"],
        "a_fixture_gate_that_never_existed": ["fixture.rs:5"],
    }
    if symbols != want_symbols:
        raise SystemExit(
            "prose-check: the symbol rule is broken — against a tree of one function it did not "
            "report exactly the names that tree does not have. A truncation of a real name must "
            "be a finding (it is not, if the match is a substring again), and an affix must not "
            "be one (it is, if the boundary is applied to the side the name was cut on).\n"
            "  wanted %r\n  got    %r" % (want_symbols, symbols)
        )
    interrupted = doc_interrupted(SELF_CHECK_INTERRUPTED)
    if [t for _, t in interrupted] != ["#[allow(some::lint)]"]:
        raise SystemExit(
            "prose-check: the interrupted-doc rule is broken — it did not see a doc comment "
            "resumed after an attribute, which is the seam between two items' docs run together "
            "(SKEIN-582, found: %r)" % interrupted
        )
    if doc_interrupted(SELF_CHECK_UNINTERRUPTED):
        raise SystemExit(
            "prose-check: the interrupted-doc rule is broken — it fired on an attribute that "
            "merely follows a doc block, which is every derive and every allow in the tree"
        )
    cited = bad_citations(SELF_CHECK_INDEX, SELF_CHECK_CITATIONS, SELF_CHECK_LENGTHS)
    want = {
        "fixture.md": [
            (2, "thing.rs:101", "src/thing.rs has 100 line(s)"),
            (3, "gone.rs:1", "no such file"),
            (8, "thing.rs:90-200", "src/thing.rs has 100 line(s)"),
            (9, "hing.rs:1", "no such file"),
        ],
        "fixture.rs": [(1, "gone2.rs:1", "no such file")],
    }
    if cited != want:
        raise SystemExit(
            "prose-check: the citation rule is broken — against a made-up tree of three files it "
            "did not report exactly the citations that cannot be followed\n  wanted %r\n  got    %r"
            % (want, cited)
        )

    # THE DATED-REVIEW RULE, both halves, against a made-up past. Read against today's fixture tree
    # `thing.rs:140` and `old.rs:3` are both unfollowable; read against the declared commit both are
    # fine and `gone.rs:1` is STILL a finding. So this fails if the declaration is ignored (the
    # first two appear), and it fails if the declaration is read as an exemption (the third
    # disappears). Nothing here touches git: `at` is the seam, and passing a fake through it is what
    # lets the rule be proved rather than demonstrated on one repository's actual history.
    then_index, then_lengths = SELF_CHECK_THEN
    notes = []
    dated = bad_citations(
        SELF_CHECK_INDEX,
        [SELF_CHECK_DATED],
        SELF_CHECK_LENGTHS,
        at=lambda c: (then_index, then_lengths.get) if c == "abc1234" else None,
        notes=notes,
    )
    if dated != {"dated.md": [(4, "gone.rs:1", "no such file at abc1234")]}:
        raise SystemExit(
            "prose-check: the dated-review rule is broken — a document declaring the commit its "
            "citations name did not have them resolved against that commit, or stopped being "
            "checked at all (SKEIN-600, found: %r)" % dated
        )
    if notes != [("dated.md", "abc1234", "checked")]:
        raise SystemExit(
            "prose-check: the dated-review rule did not report which document it read against "
            "which commit, so the summary line cannot say how many were skipped (found: %r)" % notes
        )
    gone = []
    if bad_citations(SELF_CHECK_INDEX, [SELF_CHECK_DATED], SELF_CHECK_LENGTHS, at=lambda _: None,
                     notes=gone) != {} or gone != [("dated.md", "abc1234", "absent")]:
        raise SystemExit(
            "prose-check: a declared commit this repository has not got must make that document's "
            "citations a SKIP — reported in the summary, never a finding. Falling back to the "
            "working tree resolves them against the one tree under which they are knowingly "
            "false, and `at_commit` argues why that must not fail: the citations are unverifiable "
            "by any means at that point, so every finding names a defect no edit could fix."
        )
    glued = doc_attachment(SELF_CHECK_ATTACH)
    if [t for _, t in glued] != ["What the SECOND item does, in a sentence."]:
        raise SystemExit(
            "prose-check: the doc-attachment rule is broken — it did not see a second doc block "
            "glued onto the end of the first, which is the whole of FLEET-1 (found: %r)" % glued
        )
    if doc_attachment(SELF_CHECK_CLEAN):
        raise SystemExit(
            "prose-check: the doc-attachment rule is broken — it fired on two blocks that each "
            "have their own item, so every correct doc comment in the tree is a finding"
        )
    if doc_attachment(SELF_CHECK_INNOCENT):
        raise SystemExit(
            "prose-check: the doc-attachment rule is broken — it fired on a wrapped paragraph, a "
            "markdown list or a fenced code block, none of which is a doc block at all"
        )
    if "// a } comment" in without_comments("let x = 0; // a } comment\n", ".rs"):
        raise SystemExit(
            "prose-check: comments are not being stripped from the code, so a stale comment "
            "still satisfies the gate it exists to fail (WTS-8)"
        )


def main():
    self_check()
    found = absent()
    attached = doc_attachments()
    interrupted = doc_interruptions()
    dated = []
    cited = bad_citations(notes=dated)

    if "--show" in sys.argv:
        for name in sorted(found):
            print(f"{name:34} {', '.join(found[name])}")
        print(f"\n{len(found)} symbol(s) named in prose that the tree does not have")
        for label in sorted(attached):
            for line, text in attached[label]:
                print(f"{label}:{line}  {text[:70]}")
        print(f"{sum(len(v) for v in attached.values())} doc block(s) glued to the one above")
        for label in sorted(interrupted):
            for line, text in interrupted[label]:
                print(f"{label}:{line}  {text}")
        print(f"{sum(len(v) for v in interrupted.values())} doc run(s) resumed after an attribute")
        for label in sorted(cited):
            for line, text, why in cited[label]:
                print(f"{label}:{line}  {text}  ({why})")
        print(f"{sum(len(v) for v in cited.values())} citation(s) that cannot be followed")
        ambiguous = ambiguous_citations()
        for label, line, text in ambiguous:
            print(f"{label}:{line}  {text}  (names more than one file — not checked)")
        print(f"{len(ambiguous)} citation(s) too ambiguous to check")
        return 0

    spec = load_spec()
    if "--update-attachment" in sys.argv:
        update_attachment(attached)
        return 0
    if "--update-citations" in sys.argv:
        update_citations(cited)
        return 0
    if "--update" in sys.argv:
        open(SPEC, "w", encoding="utf-8").write(render(found, spec, load_debt()[0] or {}))
        print(
            "  NOTE: --update rewrites the file from the tree and keeps only the\n"
            "  name = reason lines. The section headings that group them by KIND are\n"
            "  dropped; read the diff before keeping it."
        )
        print(f"wrote {os.path.relpath(SPEC, ROOT)} ({len(found)} symbol(s))")
        return 0

    if spec is None:
        print(
            "prose-check: docs/prose-symbols.toml is missing, so the law is unenforced\n"
            "             rule: run `python3 tools/prose-check.py --update` and write the reasons"
        )
        return 1
    stale, allowed, allowed_citations = load_debt()
    if stale is None:
        print(
            "prose-check: docs/prose-debt.toml is missing, so today's known defects are"
            " undeclared\n"
            "             rule: the debt list is what keeps the gate green about the tree as it"
            " is and red about anything added to it"
        )
        return 1

    problems = []
    for name in sorted(found):
        if name in stale:
            continue
        if name not in spec:
            where = ", ".join(found[name][:4])
            more = f" (+{len(found[name]) - 4} more)" if len(found[name]) > 4 else ""
            problems.append(
                f"prose-check: the prose names `{name}` and the tree does not have it\n"
                f"             at {where}{more}\n"
                f"             rule: a claim about the code that names something gone is worse "
                f"than no claim — fix the sentence, or declare the name in "
                f"docs/prose-symbols.toml with why it is not here"
            )
    for name in sorted(set(spec) - set(found)):
        problems.append(
            f"prose-check: docs/prose-symbols.toml exempts `{name}` and nothing needs it\n"
            f"             rule: either the code has it again or no prose names it — an "
            f"allow-list nobody prunes is a permission nobody granted. Drop the entry"
        )
    for name in sorted(set(stale) - set(found)):
        problems.append(
            f"prose-check: docs/prose-debt.toml records `{name}` as stale and the prose no longer "
            f"names it\n"
            f"             rule: the debt list only shrinks by being edited — delete the entry in "
            f"the same change that fixed the sentence"
        )
    # The doc-attachment rule, against the recorded sites. Matched on the finding's own text, not
    # on its line number: an edit anywhere above a site moves the line and would otherwise fail
    # the build for a file nobody touched.
    for label in sorted(attached):
        recorded = list(allowed.get(label, []))
        for line, text in attached[label]:
            if text in recorded:
                recorded.remove(text)
                continue
            problems.append(
                f"prose-check: {label}:{line} starts a second doc block glued to the one above "
                f"it\n"
                f"             `{text[:78]}`\n"
                f"             rule: rustdoc attaches every `///` line to the NEXT item, so both "
                f"blocks land on it and the item the first one describes is left undocumented "
                f"(FLEET-1). Move the block to its item"
            )
    for label in sorted(allowed):
        live = [t for _, t in attached.get(label, [])]
        for text in allowed[label]:
            if text in live:
                live.remove(text)
                continue
            problems.append(
                f"prose-check: docs/prose-debt.toml records a glued doc block in {label} that is "
                f"no longer there\n"
                f"             `{text[:78]}`\n"
                f"             rule: Phase 5 empties this list by deleting entries as it fixes "
                f"them — run `--update-attachment` and read the diff"
            )
    # The interrupted-doc rule. No allow-list: the tree is at zero and the rule has no measured
    # false positives, so anything it finds is new and is a defect.
    for label in sorted(interrupted):
        for line, text in interrupted[label]:
            problems.append(
                f"prose-check: {label}:{line} interrupts a doc comment with `{text}` and then "
                f"resumes it\n"
                f"             rule: nobody writes a doc, an attribute, and then more doc — the "
                f"attribute is the seam where two items' docs were run together, and rustdoc puts "
                f"both on the item below while the one above them is left undocumented "
                f"(SKEIN-582). Move the first block to its item, and take the attribute with it "
                f"only if it belongs there"
            )
    # The citation rule, against its recorded sites. Matched on (file, citation text) for the
    # same reason the attachment rule is: a line number goes stale the moment anything above it
    # moves, and the whole point of this rule is that line numbers rot.
    for label in sorted(cited):
        recorded = list(allowed_citations.get(label, []))
        for line, text, why in cited[label]:
            if text in recorded:
                recorded.remove(text)
                continue
            problems.append(
                f"prose-check: {label}:{line} cites `{text}` and it cannot be followed — {why}\n"
                f"             rule: a citation is a claim about the code, and one that lands "
                f"nowhere is worse than no citation — the reader cannot even tell what was "
                f"meant. Prefer a `symbol_name`, which the check above keeps honest and which "
                f"survives every edit that moves it; where a line is genuinely the only handle, "
                f"make it a line that is there"
            )
    for label in sorted(allowed_citations):
        live = [t for _, t, _ in cited.get(label, [])]
        for text in allowed_citations[label]:
            if text in live:
                live.remove(text)
                continue
            problems.append(
                f"prose-check: docs/prose-debt.toml defers a citation of `{text}` in {label} and "
                f"it is no longer there\n"
                f"             rule: this list only shrinks by being edited — delete the entry in "
                f"the same change that fixed the citation, or `--update-citations` and read the "
                f"diff"
            )
    problems.extend(check_citation_debt())
    for p in problems:
        print(p + "\n")
    if problems:
        print(
            f"{len(problems)} problem(s). `python3 tools/prose-check.py --show` lists every "
            f"absent symbol with where it is named."
        )
        return 1

    named = sum(len(v) for v in found.values())
    glued = sum(len(v) for v in attached.values())
    rotted = sum(len(v) for v in cited.values())
    print(
        f"every symbol the prose names is in the code or declared absent "
        f"({len(spec)} declared, {len(stale)} stale and scheduled, {named} mention(s); "
        f"{glued} glued doc block(s) recorded in docs/prose-debt.toml); "
        f"every `file:line` citation names a file that exists and a line inside it "
        f"({rotted} deferred in docs/prose-debt.toml{dated_note(dated)})"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
