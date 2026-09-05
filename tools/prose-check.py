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

WHAT IS NOT, and deliberately. Prose that describes something without naming it is untouched;
this is a spell-check for identifiers, not a fact-checker. It cannot tell you a sentence is
wrong about a function that still exists — only that the function is gone.

TWO LISTS, and the difference between them is the point.

  * `docs/prose-symbols.toml` — names it is RIGHT to keep. Somebody ELSE'S (a kernel
    capability, a variable another tool sets), named in the PAST TENSE deliberately, or
    PROPOSED and not yet built. Permanent, and each one a decision written down once.
  * `docs/prose-debt.toml` — today's defects, recorded so the gate can be green about the tree
    as it is and red about anything added to it. Stale names whose files this gate's author
    could not edit, and today's glued doc blocks. Phase 5 (SKEIN-523) empties it.

A stale entry in either fails too — an allow-list nobody prunes is a permission nobody granted.

    python3 tools/prose-check.py                     # the gate
    python3 tools/prose-check.py --show              # every finding, with where it is
    python3 tools/prose-check.py --update            # rewrite prose-symbols.toml from the tree
    python3 tools/prose-check.py --update-attachment # rewrite prose-debt.toml's glued-doc list
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


def load_debt():
    """`docs/prose-debt.toml`: (stale-symbol names, {file: [texts]})."""
    if not os.path.exists(DEBT):
        return None, None
    with open(DEBT, "rb") as f:
        debt = tomllib.load(f)
    attached = {}
    for row in debt.get("doc-attachment", []):
        attached.setdefault(row["file"], []).append(row["text"])
    return debt.get("stale-symbol", {}), attached


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


def self_check():
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

    if "--show" in sys.argv:
        for name in sorted(found):
            print(f"{name:34} {', '.join(found[name])}")
        print(f"\n{len(found)} symbol(s) named in prose that the tree does not have")
        for label in sorted(attached):
            for line, text in attached[label]:
                print(f"{label}:{line}  {text[:70]}")
        print(f"{sum(len(v) for v in attached.values())} doc block(s) glued to the one above")
        return 0

    spec = load_spec()
    if "--update-attachment" in sys.argv:
        update_attachment(attached)
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
    stale, allowed = load_debt()
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
    print(
        f"every symbol the prose names is in the code or declared absent "
        f"({len(spec)} declared, {len(stale)} stale and scheduled, {named} mention(s); "
        f"{glued} glued doc block(s) recorded in docs/prose-debt.toml)"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
