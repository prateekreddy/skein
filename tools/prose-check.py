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
`prq::submit_review_with_comments` asks about the function.

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
`cockpit/` and `warden/`, and resolved by the tail of the path. See `bad_citations`, which also
argues why a line that merely points at the WRONG thing cannot be a gate.

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

    python3 tools/prose-check.py                     # the gate
    python3 tools/prose-check.py --show              # every finding, with where it is
    python3 tools/prose-check.py --update            # rewrite prose-symbols.toml from the tree
    python3 tools/prose-check.py --update-attachment # rewrite prose-debt.toml's glued-doc list
    python3 tools/prose-check.py --update-citations  # rewrite prose-debt.toml's citation list
"""

import json
import os
import re
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
    line of `src/place.rs` was enough to satisfy the check that `docs/TODO.md` discusses it as
    live. The drift the gate exists for is precisely "the function went and the name stayed", and
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
    """
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


def absent():
    """{symbol: [where, …]} for every named symbol the tree does not have."""
    code = code_text()
    found = {}
    for label, lines in prose_sources():
        for n, line in enumerate(lines, 1):
            for name in BACKTICKED.findall(line):
                leaf = name.rsplit("::", 1)[-1]
                if not looks_like_a_symbol(leaf) or leaf in code:
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


def bad_citations(index=None, sources=None, lengths=None):
    """{label: [(line, text, why)]} for every citation that cannot be followed.

    `index`, `sources` and `lengths` are injectable so `self_check` can run the whole rule
    against a tree it made up — see `SELF_CHECK_CITATIONS`. `sources` is
    [(label, text, is_markdown)]; `lengths` is {path: line count}.
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
        for line, path, cited, text in citations(body, markdown):
            target = resolve(path, index)
            if target is None:  # ambiguous — the citation names no one file. Counted, not failed.
                continue
            if target == "":
                found.setdefault(label, []).append((line, text, "no such file"))
            elif cited > length_of(target):
                found.setdefault(label, []).append(
                    (line, text, "%s has %d line(s)" % (target, length_of(target)))
                )
    return found


def ambiguous_citations(index=None):
    """[(label, line, text)] for citations that name more than one file — reported, never failed."""
    index = tree_files() if index is None else index
    out = []
    for label, body, markdown in citation_sources():
        for line, cited, _, text in citations(body, markdown):
            if resolve(cited, index) is None:
                out.append((label, line, text))
    return out


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


def self_check():
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
    cited = bad_citations()

    if "--show" in sys.argv:
        for name in sorted(found):
            print(f"{name:34} {', '.join(found[name])}")
        print(f"\n{len(found)} symbol(s) named in prose that the tree does not have")
        for label in sorted(attached):
            for line, text in attached[label]:
                print(f"{label}:{line}  {text[:70]}")
        print(f"{sum(len(v) for v in attached.values())} doc block(s) glued to the one above")
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
        f"({rotted} deferred in docs/prose-debt.toml)"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
