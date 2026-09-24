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

WHAT IS CHECKED, one. An identifier the prose names — backticked, or written as a bare lower-case
`a::b` path (see `names_in`) — shaped like a symbol in this tree, snake_case or a `rev*`/`api*`
page function, that appears nowhere in `src/`, `tests/`, `cockpit/`,
`warden/` or `tools/`. A qualified name is judged by its last segment, so
`prq::submit_review_with_comments` asks about the function. The name has to be there as a WHOLE
identifier, not as a fragment of a longer one — see `code_has`, which is where SKEIN-610's seven
sites were hiding.

WHAT IS CHECKED, one-and-a-half. The MODULE HALF of that same name, which rule one throws away:
a `mod::name` whose `mod` is a module this tree has and does not contain `name` anywhere in its
own code. Five comments under `tests/ui/` named a function in a module it has never been in, and
rule one was green on all five because the leaf exists (SKEIN-695). The rule reports far less
than it could, on purpose — `misqualified` lists what it declines to check and what that cost,
because an honest stated limit is worth more than a rule that over-reports and gets switched off.

WHERE THE PROSE IS. `docs/**/*.md`, the markdown at the repo root, the page's own comments, **every
comment in `src/**/*.rs`**, the cockpit suites, the `#` comments in **every `.sh` file in the
tree** — `find . -name '*.sh' -not -path './.git/*' -not -path './target/*'` is the set, and
`shell_files` is the walk — and the `#` comments in **`docs/*.toml`** but for two ledgers,
`toml_files`. A comment that names a deleted function is the same defect as a document that does,
and it reaches more readers — the reader of the code.

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

AND `docs/*.toml`, WHICH IS NOW READ RATHER THAN DOCUMENTED — the third and last of this project's
prose that no gate opened. It was written down here as a measured blind spot for one commit, and
the paragraph that stood in this place predicted what turning it on would cost. Two of its three
predictions held and the third did not, which is the part worth keeping:

  * IT HELD that the shape is `#` comments and not whole files, and by a wider margin than it
    said — 33 names whole against 6 in the comments, 31 of the 33 being this gate reading its own
    ledgers back to itself. `toml_files` carries both numbers.
  * IT HELD that ledgers need exempting, and named three. TWO earned it, on the evidence in
    `TOML_PROSE_EXEMPT`; the third, `docs/prose-symbols.toml`, measures zero under the shape that
    was chosen, because what makes it a ledger is its VALUES. It is left in scope. An exemption
    that was right about a file type and wrong about a file is still an exemption nobody earned.
  * IT MISSED ONE, and the miss is the interesting half. It was measured with rule one and
    reported as three real mentions in `docs/modules.toml`; the MODULE rule, run over the same
    nine files, finds a fourth that rule one cannot see — in `docs/env-lock.toml`, a file the old
    paragraph does not mention at all, because a leaf that exists is invisible to rule one no
    matter which module the prose puts it in. That is SKEIN-695's shape exactly, and it is the
    second time in this tool's history that a measurement taken with one rule was read as a
    measurement of the gate: `43577e3` records the first. **Run both rules, or report neither.**

EIGHT FINDINGS IN TOTAL, and they divide three ways. FOUR of rule one's six and ONE of the module
rule's two are sited in `docs/prose-debt.toml`, which the exemption removes. The other TWO of rule
one's six are one sentence in `docs/modules.toml`, past tense and correct, and are now declared.
The module rule's remaining one is the live finding in `docs/env-lock.toml`, and it was fixed in
the prose rather than declared. No name is spelled out here, for the reason
`prose_sources` gives at the end of its own docstring: a name written into THIS file joins
`code_text` and makes the tree appear to contain the very symbol somebody was asking about
(WTS-8). To reproduce any of it, import this module, build the `docs/*.toml` into the
`(label, lines)` pairs `prose_sources` yields, and hand them to `absent` AND to `misqualified` as
their `sources` — the seam that exists so a rule can be run against something other than this
repository.

The `.sh` half of that same blind spot was closed the same way and for the same reason, the
measurement and nothing else: it cost two mentions, both already declared. `shell_files` has it.

AND THE SHAPE, which was the other way to be invisible here and is now closed (SKEIN-829). This
rule read BACKTICKED names and nothing else, so a sentence naming a symbol in bare parentheses was
not a claim it could see — which is how SKEIN-561's two dead names survived a scan that DID read
their file, `60addb5` says so, and why widening the file types above would have fixed the incident
only by luck. It reproduced on purpose here: the same dead name planted in the same script passed
when written bare and failed when written in backticks, same file, same scan, only the punctuation
different. `BARE_QUALIFIED` and `names_in` are the fix, and the argument for their two
restrictions — lower-case segments, at least one `::` — is written there with the measurement:
49 mentions across every source, the whole population read rather than sampled, 0 findings under
rule one and 0 under the module rule. A bare single WORD is still deliberately unread, because it
is indistinguishable from English and would flood.

TWO LISTS, and the difference between them is the point.

  * `docs/prose-symbols.toml` — names it is RIGHT to keep. Somebody ELSE'S (a kernel
    capability, a variable another tool sets), named in the PAST TENSE deliberately, or
    PROPOSED and not yet built. Permanent, and each one a decision written down once.
  * `docs/prose-debt.toml` — today's defects, recorded so the gate can be green about the tree
    as it is and red about anything added to it. Stale names whose files this gate's author
    could not edit, today's glued doc blocks, and today's unfollowable citations. Phase 5
    (SKEIN-523) empties the first two; every citation row names the item that removes it.

A key in either may be QUALIFIED (`mod::name`) — that is the module rule's entry, pruned against
the module rule's findings, exactly as a bare key is pruned against rule one's.

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
import tempfile
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

# The files this rule may not read its own subject out of, and the argument for each is the same
# one: a tool about unfollowable citations has to be able to WRITE one, and a list of them has to
# be able to QUOTE one. In scope, each would report what it deliberately contains as a fresh
# finding, and the only ways back would be to delete the demonstration or to rubber-stamp it in
# `docs/prose-debt.toml` — which is how an exemption table gets its first entry that nobody meant.
#
#   this file             `SELF_CHECK_CITATIONS` is made of nothing but citations that cannot be
#                         followed, because that is what it is testing for.
#   docs/prose-debt.toml  every row quotes a citation that cannot be followed. Left in scope, the
#                         list grows by one finding per row it records.
#   tools/line-cite-check.py
#                         its `self_check()` builds a `src/fake.rs` in memory and cites lines in
#                         it, to prove on every run that it catches a citation the code moved out
#                         from under. Those eight citations are the demonstration (SKEIN-778).
#   docs/line-cites.toml  the ledger that tool writes. Its KEYS are `path:line` strings and its
#                         values are lines of code, so this rule would read every entry as a
#                         citation the document never made — and a `historical = "<why>"` entry
#                         is one that DELIBERATELY names code this tree has not got, which is
#                         exactly what this rule fails on and what that declaration exists to say.
#
# The cost, in all four, is that a real citation written in them goes unchecked. Name the symbol
# instead, which is this rule's advice to every other file too.
LINE_CITES = os.path.join(ROOT, "docs", "line-cites.toml")
CITATION_EXEMPT = {
    os.path.abspath(__file__),
    os.path.abspath(DEBT),
    os.path.abspath(os.path.join(ROOT, "tools", "line-cite-check.py")),
    os.path.abspath(os.path.join(ROOT, "docs", "line-cites.toml")),
}

# Where a symbol may live. The page is both prose and code, so it is on both lists.
CODE_DIRS = ["src", "tests", "cockpit", "warden", "tools"]
CODE_SUFFIXES = (".rs", ".py", ".mjs", ".js", ".html", ".toml", ".sh", ".json")

# Backticked, and either qualified (`a::b`) or bare. The last segment is what is looked up.
#
# A leading `$` is allowed and dropped (SKEIN-648): prose names an environment variable the way a
# shell spells it, and until this the gate read `SKEIN_THING` in backticks and was blind to the
# same variable written with its `$` — so a retired variable survived in prose exactly when it was
# spelled the usual way. The `$` is outside the group, so what is looked up is the name the code
# reads. The span still has to be the WHOLE of the backticks: a path like `$SKEIN_HOME/x` or a
# `${NAME}` expansion is not a name on its own and stays unread, as `a::b(` does.
BACKTICKED = re.compile(r"`\$?([A-Za-z_][A-Za-z0-9_]*(?:::[A-Za-z_][A-Za-z0-9_]*)*)`")

# A qualified name the prose does NOT put in readable backticks — SKEIN-829, and the shape that
# caused SKEIN-561. Both dead names there were written bare, in a parenthetical, inside a file this
# gate already read; widening the FILES it reads left them invisible, because the file type was
# never the defect. It is also, in practice, an UNPARSEABLE-BACKTICK rule: a span like `a::b(` or
# `a::b()` carries a character BACKTICKED cannot take, so the author DID mark the name up and the
# gate dropped it anyway. `names_in` runs this over what is left once the readable spans are
# removed, so a name is never counted twice.
#
# Lower-case segments, and at least one `::`. Both restrictions are why this can be turned on at
# all. A BARE word is indistinguishable from English — `main`, `diff`, `check` — and stays unread
# deliberately; an upper-case segment is a type or a variant (`Lane::NeedsYou`, `Config::load`),
# not the module path this asks about. What is left is a shape English does not produce by
# accident.
#
# MEASURED over every source before turning it on, and it is the whole population rather than a
# sample: 49 un-backticked `a::b` mentions in this tree's prose, 0 naming a leaf the tree has not
# got, and 0 the module rule would report either. Only 11 of the 49 are symbol-shaped enough to be
# judged at all; the rest are `std::`/`tokio::` paths, CSS pseudo-elements and `crate::`-rooted
# rustdoc links, which `looks_like_a_symbol` and `NOT_A_MODULE` were already dropping. Two of the
# 11 are somebody else's name and pass only because this tree happens to call the same function;
# if that stops being true they become one line in `docs/prose-symbols.toml`, which is exactly
# what that file's first section exists for — a future false positive is a declaration with a
# reason, not a broken build.
BARE_QUALIFIED = re.compile(r"\b([a-z_][a-z0-9_]*(?:::[a-z_][a-z0-9_]*)+)\b")


def names_in(line):
    """Every symbol name a line claims: the backticked ones, then the bare qualified ones.

    One function because both rules must see the same set. They read the same sentence, and a name
    that is a claim to one of them is a claim to the other; two extractions would be the "same
    fact answered in two places" this repository keeps paying for. The readable spans are removed
    before the second pass, so a name in backticks comes back once.
    """
    names = BACKTICKED.findall(line)
    names += BARE_QUALIFIED.findall(BACKTICKED.sub(" ", line))
    return names

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


def code_files():
    """(label, uncommented text) for every file `code_text` reads — the same walk, kept per file."""
    for d in CODE_DIRS:
        for base, dirs, files in os.walk(os.path.join(ROOT, d)):
            dirs[:] = [x for x in dirs if x not in ("node_modules", "target", ".git")]
            for f in sorted(files):
                if f.endswith(CODE_SUFFIXES):
                    path = os.path.join(base, f)
                    try:
                        text = open(path, encoding="utf-8").read()
                    except (OSError, UnicodeDecodeError):
                        continue
                    yield os.path.relpath(path, ROOT), without_comments(text, os.path.splitext(f)[1])


def code_sites(name, files=None, limit=3):
    """Where the code has `name` as `code_has` means it: `[(label:line, the line), …]`, at most `limit`.

    Only called on a failure path, to say WHY an entry stopped being needed. The case that asked
    for it is SKEIN-559: a test naming a test BINARY as a bare string literal, where that binary's
    name was also a stale symbol in the debt file. `code_text` rightly counts a string as code —
    an environment variable or a route is only ever a string — so the entry went stale and the
    gate said "the prose no longer names it", which was false and pointed nowhere near the test.
    """
    found = []
    left = "" if name.startswith("_") else r"\b"
    right = "" if name.endswith("_") else r"\b"
    pattern = re.compile(left + re.escape(name) + right)
    for label, text in (code_files() if files is None else files):
        for n, line in enumerate(text.split("\n"), 1):
            if pattern.search(line):
                found.append((f"{label}:{n}", line.strip()[:100]))
                if len(found) >= limit:
                    return found
    return found


def prose_sites(name, sources=None):
    """Every `label:line` where the prose names `name` as a bare (last-segment) symbol."""
    return [
        f"{label}:{n}"
        for label, lines in (prose_sources() if sources is None else sources)
        for n, line in enumerate(lines, 1)
        if any(found.rsplit("::", 1)[-1] == name for found in names_in(line))
    ]


def no_longer_needed(ledger, name, rule, prose_at, code_at):
    """The finding for a ledger entry rule one no longer reports, saying which of two reasons.

    Either the prose stopped naming it — the case every such entry used to be reported as — or the
    prose still does and the CODE gained the name. The second needs a different sentence, because
    it is often a coincidence the gate cannot see through: a string literal that shares the name
    with a different thing (SKEIN-559).
    """
    if not prose_at or not code_at:
        return (
            f"prose-check: {ledger} {rule}, and "
            + ("the prose no longer names it" if not prose_at else "nothing needs it")
            + "\n"
            f"             rule: " + (
                "the debt list only shrinks by being edited — delete the entry in the same "
                "change that fixed the sentence" if "debt" in ledger else
                "either the code has it again or no prose names it — an allow-list nobody "
                "prunes is a permission nobody granted. Drop the entry"
            )
        )
    more = f" (+{len(prose_at) - 4} more)" if len(prose_at) > 4 else ""
    return (
        f"prose-check: {ledger} {rule}, and the prose still names it at "
        f"{', '.join(prose_at[:4])}{more} — but the code now has `{name}`:\n"
        + "".join(f"               {where}  {line}\n" for where, line in code_at)
        + f"             rule: every non-comment line is code to this gate, string literals "
        f"included, because an environment variable or a route is only ever a string. If the "
        f"code gained `{name}` because the prose became true, delete the entry. If it is a "
        f"DIFFERENT thing that happens to share the name — a test binary's name written as a "
        f"bare string literal is the case that found this (SKEIN-559) — spell that one so it is "
        f"not the bare word (`file!()`, `env!(\"CARGO_BIN_NAME\")`, a `concat!`), and the entry "
        f"stays honest"
    )


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


# Where the three `.rs` rules look. `tests/` is here for the same reason `src/` is, and its
# absence was the last hole in this gate: `tests/` was already in `CODE_DIRS`, so a test file
# counted as code to match a name AGAINST and never as prose to be checked. A test's module note
# names functions, is read by whoever runs the suite, and goes stale exactly the way a doc comment
# does — `tests/platform_gates.rs` exists to tell a reader which names to look for in `cargo test`
# output, which is a claim about the code and nothing else (SKEIN-640).
RUST_PROSE_ROOTS = ("src", "tests")

# Not walked. `web`, `store`, `kit` and `probe` are other languages' directories under `src/`;
# `node_modules` is vendored and enormous.
RUST_PROSE_SKIP = {"web", "store", "kit", "probe", "node_modules"}


def rust_files():
    """Every `.rs` file under `src/` and `tests/`, as (relative label, absolute path)."""
    for root in RUST_PROSE_ROOTS:
        for base, dirs, files in os.walk(os.path.join(ROOT, root)):
            dirs[:] = [d for d in dirs if d not in RUST_PROSE_SKIP]
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

    **The cockpit's own suites are prose too** (SKEIN-699), and they were the last body of it this
    rule did not read: neither the `//` comments in `tests/ui/*.mjs` nor the `README.md`s beside
    them. That gap is why the five misqualified comments of SKEIN-695 were invisible twice
    over — the module half was unchecked, and the files were not a source at all — and it is the
    larger of the two holes, because a `.mjs` suite explains the PAGE, whose functions are renamed
    and deleted faster than anything in `src/`.

    What it cost to turn on, measured before the change: 18 names in 6 files, 20 mentions. Most
    are in the module notes of `tests/ui/lift.mjs` and `tests/ui/review_return.mjs`, which record
    which page functions a deleted section used to drive — the past-tense category
    `docs/prose-symbols.toml` exists for, and the most valuable prose in either file. One is a
    defect: two comments in `tests/ui/review.mjs` name a page function that has never existed
    under that spelling, describing behaviour that belongs to `revReadAgain`. It is recorded in
    `docs/prose-debt.toml` rather than fixed, because that suite was another agent's on the day.

    **THE LIMIT THIS SOURCE HAS, and it is the JavaScript that makes it sharper than the others.**
    A name clears the gate by appearing as a whole identifier anywhere in the tree's code, and in
    a `.mjs` file an object key, a string-keyed environment name and a function name are all the
    same token. So this rule cannot tell an environment variable ANOTHER tool reads from a
    function this tree could define, and the difference between the two is an accident of whether
    something in the tree happens to set it: `tests/ui/onboarding.mjs` names two proxy variables
    in one sentence, the suite sets one of them a line below and does not set the other, and only
    the unset one was reported. That is not a rule to tighten — the two spellings are
    genuinely indistinguishable — so it is a limit to know: a lower-case environment variable read
    by git, curl or the sandbox is somebody else's name, and it goes in the first section of
    `docs/prose-symbols.toml` beside the kernel capabilities and the `open(2)` flags, where the
    upper-case ones already are. SKEIN-648 was the other half of the same shape: an
    environment variable written `$LIKE_THIS` was not matched by `BACKTICKED` at all, so the gate's
    treatment of a variable depended on how the sentence spelled it. It is matched now, `$` dropped.

    Their names are not written here for the reason `without_comments` gives: a docstring is not
    a `#` comment, so it is not cut out of `code_text`, and a name written into this file would
    make the tree appear to contain the very symbol the gate was asked about (WTS-8). Turning the
    rule on and watching it fail on this paragraph is how that was established, not assumed.
    """
    for f in sorted(os.listdir(ROOT)):
        path = os.path.join(ROOT, f)
        if f.endswith(".md") and os.path.isfile(path):
            yield f, open(path, encoding="utf-8").read().split("\n")
    # Every tracked `docs/**/*.md`, not `docs/*.md` (SKEIN-1160). This was `os.listdir(docs)`, which
    # stops at the first level, and `docs/decisions/` is a level down: its records name symbols as
    # evidence ("Enforced at"), and they were outside this rule while `citation_sources` already
    # read them for the citation rule. `docs_markdown` has the list; `main`'s count guard holds it.
    for label, path in docs_markdown():
        yield label, open(path, encoding="utf-8").read().split("\n")
    # The page's SOURCES, not the page: `src/web/index.html` is assembled from `src/web/app/` by
    # `cockpit/build.mjs` (SKEIN-1104), byte for byte, so its comments are the same comments — but a
    # finding labelled with the assembled file sends its reader to the one copy that must not be
    # edited, and at a line number that exists nowhere they could fix it. Reading both would report
    # every finding twice.
    app = os.path.join(ROOT, "src", "web", "app")
    for f in sorted(os.listdir(app)) if os.path.isdir(app) else []:
        lines = open(os.path.join(app, f), encoding="utf-8").read().split("\n")
        # Only the comments: the page's own source names its own functions constantly, and a
        # function that is defined three lines down is not a claim about anything.
        yield "src/web/app/" + f, [l if PAGE_COMMENT.match(l) else "" for l in lines]
    for label, path in rust_files():
        yield label, rust_comment_lines(open(path, encoding="utf-8").read())
    for label, path in shell_files():
        lines = open(path, encoding="utf-8").read().split("\n")
        yield label, [l if l.lstrip().startswith("#") else "" for l in lines]
    for label, path in toml_files():
        lines = open(path, encoding="utf-8").read().split("\n")
        # The same `#`-comments-only shape as the shell half, and `toml_files` has the measurement
        # that chose it over reading the whole file.
        yield label, [l if l.lstrip().startswith("#") else "" for l in lines]
    yield from ui_prose()


# Vendored, generated, or not text at all: `target` and `.target` are cargo's, `node_modules` is
# playwright and its dependencies, and `.git` is an object store. Only used by the FALLBACK walk
# below, now that `tracked_files()` answers this question directly — see there for why a name-list
# like this one can never be the whole answer.
SHELL_PROSE_SKIP = {".git", "target", ".target", "node_modules"}


def tracked_files(root=None):
    """Every path `git ls-files` reports under `root` (default `ROOT`), or `None` if git could not
    answer.

    Tracked is never gitignored and gitignored is never tracked, so this is "the tree, minus
    whatever an install or a scaffold left lying around" without hand-listing a single directory
    name — the shape `residue-check.py`'s `tracked()` already settled, including keeping `None`
    ("git could not say") apart from `[]` ("git said this tree tracks nothing"): a caller that
    mistakes the second for the first scans zero files and calls the tree clean.

    THIS IS SKEIN-1146. `shell_files()` and `tree_files()` used to walk `os.walk(ROOT)` filtered
    only by a name-list (`SHELL_PROSE_SKIP`, `NOT_OURS`), and `.gitignore` is not a name-list —
    `/skein/` is one entry (`.gitignore:28`) that covers a box's entire installed copy of this
    repository's own store scripts. A checkout that is ALSO a skein box has that directory on disk,
    the walk read `skein/bin/box-status.sh` as prose the same as the real `src/probe/box-status.sh`,
    and after SKEIN-1062 renamed a symbol the installed copy still named the old one — so the gate
    went red in that one checkout, over a file this repository does not own, while a fresh
    `git worktree` of the identical commit had no `/skein/` and passed. Untracked-but-not-ignored
    files stop being read the moment this lands — the same trade `residue-check.py` already made,
    and named in `CONTRIBUTING.md`: "a new file is invisible to it until staged."

    `root` is injectable, the same way `bad_citations`' `index` and `misqualified`'s `code` are,
    so `self_check` can point this at a scratch git repository of its own instead of `ROOT` — the
    one call in this file that has to touch a real `.git` to be tested at all, since the defect it
    guards against is a property of git and `.gitignore`, not of any string this file could hold.
    """
    root = ROOT if root is None else root
    try:
        out = subprocess.run(
            ["git", "-C", root, "ls-files", "-z"],
            capture_output=True,
            check=True,
            timeout=30,
        )
    except (OSError, subprocess.SubprocessError):
        return None
    return [p for p in out.stdout.decode("utf-8", "replace").split("\0") if p]


def docs_markdown():
    """Every `.md` file this repository tracks under `docs/`, at any depth, as (label, path).

    `tracked_files()` first, for the reason it gives: tracked is the tree. Falls back to walking
    `docs/` when git could not answer, the same degraded path `shell_files` takes.
    """
    listed = tracked_files()
    if listed is not None:
        for rel in sorted(listed):
            if rel.startswith("docs/") and rel.endswith(".md"):
                yield rel, os.path.join(ROOT, rel)
        return
    for base, dirs, files in os.walk(os.path.join(ROOT, "docs")):
        dirs.sort()
        for f in sorted(files):
            if f.endswith(".md"):
                path = os.path.join(base, f)
                yield os.path.relpath(path, ROOT), path


def shell_files():
    """Every `.sh` file this repository tracks, as (relative label, absolute path).

    This read `src/store/` alone until SKEIN-561, and the directory was never the reason — the
    reason was that the store's scripts are installed into a box and read there. So are the eleven
    under `src/probe/`: the install table in `src/probes.rs` writes NINETEEN scripts into a
    project's store, and only eight of them live under `src/store/`. They carry the same prose
    too — commit `60addb5` lifted one 22-line argument out of twelve of these files at once,
    because the identical paragraph had been pasted into all twelve. TEN of that twelve were
    probes, and so were invisible to the very gate the same commit was fixing two comments for.
    A gate that read eight of the tree's twenty-four was not making a judgement about the other
    sixteen; it was not looking, and it said nothing about not looking, which is the defect this
    repository hits most.

    What it cost to turn on, measured over the sixteen files this adds BEFORE the change: two
    mentions, two names, and BOTH already declared in `docs/prose-symbols.toml` — one at
    `bootstrap.sh:340` and one at `src/box-session.sh:1314`, an environment variable and a sandbox
    kernel hook, which is the "somebody else's name" category that file opens with. So: no new
    declaration, no new debt row, nothing to fix. The module rule found nothing at all, which is
    what a shell file should do, since it names no Rust module paths. Their spellings are not
    written here for the reason the last paragraph of `prose_sources` gives.

    `tracked_files()` first — `git ls-files '*.sh'` reads exactly twenty-four, by construction
    rather than by coincidence (see `tracked_files` for why a walk cannot make that same claim).
    `main` compares this count with what came out of `prose_sources` and refuses the gate when
    they differ. Falls back to the old `os.walk(ROOT)`, ignored paths and all, only when git could
    not answer — the same degraded path `residue-check.py` takes for the same reason.
    """
    listed = tracked_files()
    if listed is not None:
        for rel in sorted(listed):
            if rel.endswith(".sh"):
                yield rel, os.path.join(ROOT, rel)
        return
    for base, dirs, files in os.walk(ROOT):
        dirs[:] = [d for d in dirs if d not in SHELL_PROSE_SKIP]
        for f in sorted(files):
            if f.endswith(".sh"):
                path = os.path.join(base, f)
                yield os.path.relpath(path, ROOT), path


# The `docs/*.toml` whose symbol-shaped text is DATA rather than a claim, and so may not be read
# as prose. It is the same argument `CITATION_EXEMPT` makes further up, about the same two files,
# for the other rule — which is itself the reason to believe it: two independent rules arriving at
# one pair of paths is a property of the files, not of either rule.
#
#   docs/line-cites.toml  EVERY line of it is machine-written, the header included:
#                         `write_ledger` in `tools/line-cite-check.py` emits `LEDGER_HEADER` and
#                         then nothing but generated entries, so `--relocate --write` reproduces
#                         the whole file from a string literal in another tool. There is no line
#                         in it a person could be asked to fix. Its VALUES are lines of code read
#                         verbatim out of the tree, and a `historical = "<why>"` entry is one that
#                         DELIBERATELY records code this tree no longer has — which is exactly
#                         what rule one fails on and what that declaration exists to say.
#   docs/prose-debt.toml  a ledger of names the tree has not got, so in scope it grows by one
#                         finding per row it RECORDS. And measurably worse than that: its `#`
#                         comments narrate the rows it has REPAID, by name, so it grows a finding
#                         per row it retires too — five of them today, four under rule one and one
#                         under the module rule, which is where this exemption was derived rather
#                         than predicted.
#
# The cost is the same one `CITATION_EXEMPT` pays: a real claim written in either file goes
# unchecked. It is the right trade only because neither file is somewhere a person writes prose —
# one is generated, and the other is a list this gate reads back to itself.
#
# NOT exempt, and deliberately: `docs/prose-symbols.toml`, the other ledger. Under this source's
# shape it measures ZERO findings, because what makes it a ledger is its VALUES and this rule
# reads only its `#` comments. An exemption nothing has yet earned is a permission nobody granted,
# and leaving it in scope is the direction to err: its headings are prose a person maintains.
TOML_PROSE_EXEMPT = {"line-cites.toml", "prose-debt.toml"}


def toml_files():
    """Every `docs/*.toml` whose prose is in scope, as (relative label, absolute path).

    WHY `docs/*.toml` IS PROSE AT ALL, and it took three widenings to see it. These files carry
    argument, not only configuration: `docs/modules.toml` justifies every dependency EDGE in a `#`
    comment above it, `docs/sources.toml` and `docs/env-lock.toml` each explain why the entries
    they allow are allowed, and those paragraphs name functions the way any other document does.
    They are read by people deciding whether an edge or an exemption is still right, which is the
    same reader `docs/*.md` has. A gate that read the markdown beside them and not these was not
    making a judgement about them; it was not looking.

    WHY THE `#` COMMENTS AND NOT THE WHOLE FILE, which is the whole of the design here and was
    measured both ways rather than argued. Over the nine files, at this commit:

      * `#` comment lines only — 6 names, 6 mentions under rule one and 2 under the module rule.
        Five of those eight go with the exemption and 3 remain, every one of them real prose that
        a person wrote and a reader would follow. All five are in `docs/prose-debt.toml`:
        `docs/line-cites.toml` is exempt on the structural argument above rather than on a count,
        because its machine-written header happens to name no symbol TODAY — a fact about today,
        and `--record` in this very commit gave it an entry that names two.
      * the whole files — 33 names, 38 mentions, and THIRTY-ONE of the 33 are sited in the three
        ledger files, this gate reading its own lists back to itself. `docs/prose-symbols.toml`'s
        keys are BY DEFINITION names the tree has not got, so in that shape the rule reports its
        own allow-list as findings.

    So the whole-file shape is not a wider version of this one, it is a different and wrong
    question. `LINE_COMMENT` already records that `#` is toml's marker, and the `.sh` source two
    functions up already reads exactly this way; the cost is a `key = "…name…"` in a real document
    going unread, which is the same cost the shell half pays and the same advice applies — put the
    claim in a comment, where a reader looking for the argument will find it.

    What it cost to turn on, measured BEFORE the change and settled in it: two names at
    `docs/modules.toml:405`, both named in the PAST TENSE and correct, both declared in
    `docs/prose-symbols.toml` by the commit that adds this; and one at `docs/env-lock.toml:15`
    under the module rule, which is a REAL defect and is fixed rather than declared — the prose
    qualified a test through a file path whose last segment collides with a different module of
    this tree, so a reader following it arrives where there is nothing to find, which is the exact
    shape the module rule was written for (SKEIN-695). Their spellings are not written here for
    the reason the last paragraph of `prose_sources` gives.

    `main` compares what came out of `prose_sources` with `prose_source_count(".toml")` — a
    SECOND listing of `docs/`, not this generator asked how many it found — and refuses the gate
    when they differ. That distinction is the whole value of the guard and it was bought by a
    sabotage: written the obvious way, with `sum(1 for _ in toml_files())` on one side, narrowing
    this function to a single file left both sides reading 1 and the gate green.
    """
    docs = os.path.join(ROOT, "docs")
    for f in sorted(os.listdir(docs)):
        if f.endswith(".toml") and f not in TOML_PROSE_EXEMPT:
            path = os.path.join(docs, f)
            yield os.path.relpath(path, ROOT), path


# Vendored, or not text of ours: `node_modules` is playwright and its dependencies.
UI_PROSE_SKIP = {"node_modules"}


def ui_prose():
    """The cockpit suites' prose: whole-line `//` in `.mjs`, and `.md` whole.

    A separate function only so that `main` can ask whether it yielded anything — see the guard
    there. Whole-line comments and not trailing ones, for the reason `LINE_COMMENT` gives: `//` is
    the middle of every `https://` and sits inside string literals, and a stripper that guesses
    deletes code, which invents findings rather than catching them. The cost is a trailing `// see
    someFunction` that goes unread; `rustcut` is what makes the `.rs` half exact, and there is no
    `.mjs` tokeniser here to be exact with.
    """
    base = os.path.join(ROOT, "tests", "ui")
    if not os.path.isdir(base):
        return
    for b, dirs, files in os.walk(base):
        dirs[:] = [d for d in dirs if d not in UI_PROSE_SKIP]
        for f in sorted(files):
            path = os.path.join(b, f)
            label = os.path.relpath(path, ROOT)
            if f.endswith(".md"):
                yield label, open(path, encoding="utf-8").read().split("\n")
            elif f.endswith(".mjs"):
                lines = open(path, encoding="utf-8").read().split("\n")
                yield label, [l if l.lstrip().startswith("//") else "" for l in lines]


def ui_prose_files(suffix):
    """How many `suffix` files `ui_prose` ought to have found — the same walk, without the reads.

    Separate from `ui_prose` so that `main`'s guard can compare what came out with what is there,
    rather than asserting that something is there. The two share `UI_PROSE_SKIP`, which is what
    keeps them from drifting into two answers.
    """
    n = 0
    for _, dirs, files in os.walk(os.path.join(ROOT, "tests", "ui")):
        dirs[:] = [d for d in dirs if d not in UI_PROSE_SKIP]
        n += sum(1 for f in files if f.endswith(suffix))
    return n


def prose_source_count(suffix):
    """How many `suffix` files `prose_sources` OUGHT to have yielded — by a SECOND walk.

    **A counting guard whose two sides come from one generator cannot see the walk shrink**, and
    both of `main`'s count guards were written that way. The `.sh` one read
    `sum(1 for _ in shell_files())` on one side and counted `.sh` labels out of `prose_sources()`
    on the other — but `prose_sources` BUILDS those labels by calling `shell_files`, so the two
    sides were one number wearing two hats. Measured, not reasoned: re-root `shell_files` at
    `src/store` and it reads **8 against 8**.

    THAT WAS NOT A HOLE IN THE `.sh` SOURCE, and the distinction is the point. SKEIN-561 knew the
    count could not see a re-root and split the job in two, which its commit message says in as
    many words: the count catches a walk made smaller than the BRANCH that reads it — sabotage the
    `.sh` branch in `prose_sources` and it reads 24 against 23 — and `self_check` catches the
    re-root, by recognising the one shape that actually happened, everything under `src/store/` or
    nothing at all. Both still stand, and the second one still raises. What the pair cannot see is
    a re-root ANYWHERE ELSE: point `shell_files` at `src/probe` and `self_check`'s pattern does not
    match, while the old count read 11 against 11. With a second walk it reads 24 against 11.

    THE HOLE WAS IN THE `.toml` SOURCE ADDED HERE, which has no `self_check` half — so its count
    guard was the only thing standing, and it could not fire. Narrowing `toml_files` to a single
    file left both sides reading 1 and the gate green, on the first draft of this very change: a
    guard written against a source going quiet, which was itself quiet. It was found by sabotaging
    the guard rather than the prose, which is the only way it could have been found.

    `ui_prose_files` already had the right shape and is what this generalises: count the files a
    second time, in code that a sabotage of the reader does not touch, so that the comparison has
    two independent sources of truth. What the two sides DO share is the constant —
    `SHELL_PROSE_SKIP`, `TOML_PROSE_EXEMPT` — and that sharing is deliberate. A skip list or an
    exemption set is a decision somebody makes in a diff a reviewer reads; widening one is not the
    failure this guards against, and a guard that also re-derived the constant would be asserting
    a number nobody is allowed to change. The failure this guards against is the WALK being
    narrowed, re-rooted or switched off while the constant stays put, and against that these two
    sides now genuinely disagree.

    THE `.sh` HALF NOW SHARES `tracked_files()` WITH `shell_files()` (SKEIN-1146), the same way it
    used to share `SHELL_PROSE_SKIP` with a second, independently-written `os.walk(ROOT)` — one
    call here, one there, each free to be narrowed without moving the other. `tracked_files()`
    replaces the name-list `os.walk(ROOT)` needed and could never finish (`.gitignore` grows a line
    at a time; a walk that skips by name has to grow with it, or it reads an installed `/skein/` the
    same as this repository's own scripts, which is exactly what happened). Sabotage the count above
    on its own — narrow its filter, or point it back at the old walk over one directory — and it
    still disagrees with `shell_files()`, because the two sides remain two call sites.
    """
    if suffix == ".sh":
        listed = tracked_files()
        if listed is not None:
            return sum(1 for p in listed if p.endswith(".sh"))
        n = 0
        for _, dirs, files in os.walk(ROOT):
            dirs[:] = [d for d in dirs if d not in SHELL_PROSE_SKIP]
            n += sum(1 for f in files if f.endswith(".sh"))
        return n
    if suffix == ".toml":
        present = [f for f in os.listdir(os.path.join(ROOT, "docs")) if f.endswith(".toml")]
        return sum(1 for f in present if f not in TOML_PROSE_EXEMPT)
    if suffix == "docs/.md":
        # Every `.md` on disk under `docs/`, at any depth, by a walk `docs_markdown` does not call:
        # put the old `os.listdir(docs)` back and `docs/decisions/` stops being read, and this
        # still counts it (SKEIN-1160). A walk rather than git, so that narrowing the git filter in
        # `docs_markdown` is caught too; an untracked `.md` under `docs/` makes the two disagree,
        # and the message says to stage it.
        return sum(
            1
            for _, _, files in os.walk(os.path.join(ROOT, "docs"))
            for f in files
            if f.endswith(".md")
        )
    raise ValueError(f"no second walk is written for {suffix!r}")


def stale_toml_exemptions():
    """The names in `TOML_PROSE_EXEMPT` that no longer name a file in `docs/`.

    An exemption that exempts nothing is a permission nobody granted — the argument
    `docs/env-lock.toml` makes about its own list, and `tools/env-lock-check.py` enforces. Here it
    is also the one half of "the exemption grew" that a gate honestly CAN catch: renaming or
    deleting a ledger leaves a name behind that silently covers nothing, and the next file to take
    that name would be exempt without anybody deciding it.
    """
    present = set(os.listdir(os.path.join(ROOT, "docs")))
    return sorted(name for name in TOML_PROSE_EXEMPT if name not in present)


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
    # Memoised (SKEIN-721): `code_has` over ~5 MB of code, once per mention, was 70 of this gate's
    # 91 seconds — a name written in twenty sentences was scanned for twenty times. `has_name`
    # answers the same question from one pass; see `whole_names` for why it is the same question.
    code_names = whole_names(code)
    found = {}
    for label, lines in (prose_sources() if sources is None else sources):
        for n, line in enumerate(lines, 1):
            for name in names_in(line):
                leaf = name.rsplit("::", 1)[-1]
                if not looks_like_a_symbol(leaf) or has_name(leaf, code, code_names):
                    continue
                found.setdefault(leaf, []).append(f"{label}:{n}")
    return found


# --------------------------------------------------------------------------------------------
# The module-qualifier rule (SKEIN-695): the half of a qualified name the rule above throws away.


# Qualifiers that are not a module of this tree whatever a file name says, so a path through one
# is no claim about where an item lives. `skein` is the CRATE — `src/bin/skein.rs` gives its name
# to a binary, not to a module — and a path rooted at the crate, at `crate`, at `self` or at
# `super` is routinely a re-export away from the module that defines the item. `tests` is the
# inline `#[cfg(test)] mod tests` that most files here carry, which belongs to no file of its own.
NOT_A_MODULE = {"skein", "crate", "self", "super", "tests"}

# Not modules of the library: a binary cannot be qualified through, and nothing may be reached by
# naming one. `src/bin/` is skipped for the index only — its comments are still prose, and rule
# one still reads them.
NOT_A_MODULE_DIR = {"bin", "node_modules", "target", ".git"}

# Every whole identifier in a text, which is what `code_has` asks about one name at a time. Both
# rules ask it thousands of times — rule one ~4000, once per mention against the whole tree; the
# module rule ~1600, against the tree and again against a module — and a regex scan of megabytes
# apiece was 70 of this gate's 91 seconds (SKEIN-721). One pass builds a set instead. `code_has`
# remains the definition of the question; `has_name` is that same question memoised, and falls
# back to it for every shape a set of tokens cannot express.
IDENTIFIER = re.compile(r"[A-Za-z_][A-Za-z0-9_]*")

# NOT `IDENTIFIER.findall`, which is what the module rule used until SKEIN-721 and is a SUPERSET of
# what `\b<leaf>\b` matches: findall starts a token after a digit, so `9foo` yielded `foo` where
# `\b` sees no boundary at all — and a superset can only silence a finding. The lookarounds make
# each match an ASCII identifier with no word character on either side, which is exactly an
# occurrence `\b<leaf>\b` would accept, and they use the same `\w` `\b` does (Unicode, on a str),
# so `éfoo` and `fooé` are refused here as they are there. Every occurrence is found: a match
# cannot run over the start of another, since that start has a non-word character before it.
WHOLE_IDENTIFIER = re.compile(r"(?<!\w)[A-Za-z_][A-Za-z0-9_]*(?!\w)")


def whole_names(text):
    """Every `leaf` for which `code_has(leaf, text)` holds, for identifier-shaped non-affix leaves."""
    return set(WHOLE_IDENTIFIER.findall(text))


def has_name(leaf, text, names):
    """`code_has(leaf, text)`, answered from `names = whole_names(text)`.

    An affix (`_and_then_some`, `a_fixture_gate_`) is not a whole identifier, and `code_has`
    deliberately drops the boundary on the side the rest of the name was cut off, so those go the
    slow way — as does anything that is not an identifier at all, which no token set holds.
    Everything else is exactly `\\b<leaf>\\b`, which `whole_names` answers for every leaf at once.
    """
    if leaf.startswith("_") or leaf.endswith("_") or not IDENTIFIER.fullmatch(leaf):
        return code_has(leaf, text)
    return leaf in names


def module_index():
    """{module name: the code of every file that module covers}.

    A file gives its stem (`src/util.rs` → `util`) and every directory above it inside `src/` or
    `tests/` (`src/review/scope.rs` → `scope` AND `review`), so a directory module answers for its
    children and `review::open_at` is not a finding merely because the item is in a submodule.
    `mod.rs`, `lib.rs` and `main.rs` contribute their directory and no name of their own.

    Two modules of the same name — `scope` under `src/review/` and a `scope` elsewhere — are
    UNIONED rather than kept apart. The rule cannot tell which one a sentence meant, and a union
    can only make it quieter, which is the direction to err in.

    Comments are cut out with `without_comments`, for the reason it gives: a stale comment naming
    the symbol would otherwise vouch for the module having it, which is the WTS-8 shape again.
    """
    parts = {}
    for root in RUST_PROSE_ROOTS:
        base = os.path.join(ROOT, root)
        for b, dirs, files in os.walk(base):
            dirs[:] = [d for d in dirs if d not in NOT_A_MODULE_DIR]
            for f in sorted(files):
                if not f.endswith(".rs"):
                    continue
                path = os.path.join(b, f)
                names = set(os.path.relpath(path, base).split(os.sep)[:-1])
                if f[:-3] not in ("mod", "lib", "main"):
                    names.add(f[:-3])
                text = without_comments(open(path, encoding="utf-8").read(), ".rs")
                for name in names:
                    parts.setdefault(name, []).append(text)
    return {name: "\n".join(texts) for name, texts in parts.items()}


def misqualified(code=None, sources=None, modules=None):
    """{qualified name: [where, …]} for a `module::symbol` whose module has not got the symbol.

    Rule one looks up the LAST segment of a qualified name and throws the rest away, so
    `config::fleet_root` asked about `fleet_root` — which exists, in `util`, and has never been in
    `config`. Five comments under `tests/ui/` sent their reader to the wrong file for a year and
    this gate was green on every one of them (SKEIN-695).

    WHAT IT DELIBERATELY DOES NOT CHECK, because a rule that reports every qualifier it cannot
    resolve drowns the gate and gets switched off, which is worse than the gap:

      * a qualifier this tree has no module for is SKIPPED — `Pr::facts` and `Config::load` name
        types, `serde_json::from_str` names somebody else's crate, and none of them is answerable
        here. 212 of the 812 qualified names in this tree's prose skip out this way.
      * `NOT_A_MODULE` skips four more by name, for the reason written there.
      * a MENTION clears a module, not a definition. `pub use` and `use crate::util::*` both make
        a symbol genuinely reachable under a second path, and the module's own text is the only
        thing that can say so — so a `use` line, a call or a field is enough. The cost is that a
        module which merely CALLS an item is credited with holding it; the benefit is no false
        positive on a re-export, and re-exports are how this crate is written.
      * only the segment immediately BEFORE the leaf is read. `review::scope::your_own_pr` is a
        claim about `scope`, and whether `scope` is really under `review` is left unchecked.
      * a document that declares the commit its citations name is NOT exempt here — that
        declaration covers citations, and this rule reads today's tree. Nothing in the tree needed
        the exemption on the day it was written; a dated document that ever does declares the
        whole qualified name in `docs/prose-symbols.toml`, like any other name it is right to keep.

    WHAT IT COSTS, measured on the day: 600 qualified names resolve to a module this tree has and
    six mentions are reported, of four names — three defects and one name a document PROPOSES and
    says it is proposing. Rule one is left to do its own job: a leaf the tree has nowhere at all is
    skipped here, so nothing is reported twice.

    `code`, `sources` and `modules` are injectable for `self_check`, the seam every rule in this
    file has.
    """
    code = code_text() if code is None else code
    modules = module_index() if modules is None else modules
    code_names = whole_names(code)
    module_names = {}
    found = {}
    for label, lines in (prose_sources() if sources is None else sources):
        for n, line in enumerate(lines, 1):
            for name in names_in(line):
                if "::" not in name:
                    continue
                leaf, qualifier = name.split("::")[-1], name.split("::")[-2]
                if not looks_like_a_symbol(leaf) or not has_name(leaf, code, code_names):
                    continue  # rule one's business — reported there, or nowhere
                if qualifier in NOT_A_MODULE or qualifier not in modules:
                    continue
                if qualifier not in module_names:
                    module_names[qualifier] = whole_names(modules[qualifier])
                if has_name(leaf, modules[qualifier], module_names[qualifier]):
                    continue
                found.setdefault(name, []).append(f"{label}:{n}")
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
# sentence: a UX review once in `docs/` drew a UI mockup whose window showed a file it was only
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
    """Every repo-relative file path, for resolving a citation against.

    `tracked_files()` first, for the same reason `shell_files()` reads it (SKEIN-1146): `NOT_OURS`
    is a name-list the way `SHELL_PROSE_SKIP` was, and a checkout that is also a skein box has an
    ignored `/skein/` this list never named. Left unfixed, a citation naming a real file by its bare
    name — `box-status.sh:26`, say — would find it a SECOND time under `/skein/` and `resolve()`
    would call the pair ambiguous, over a file this repository does not own. Falls back to the old
    walk, `NOT_OURS` and all, only when git could not answer.
    """
    listed = tracked_files()
    if listed is not None:
        return list(listed)
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
    # CHANGELOG. `prose_sources()` above read only `docs/` when this was written, so these were the one
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
                if os.path.abspath(path) in CITATION_EXEMPT:
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
# WHY THIS IS NOT THE DOCUMENT'S "written against X" LINE, which both dated reviews carried
# and which would have been free. Those are different claims. "Written against `12ae61a`" says when
# the document was composed; this says what its citations MEAN. A UX review once in `docs/` is the proof
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


def historical_citations(path=None):
    """{`path:line`: {document, ...}} for every citation `docs/line-cites.toml` declares historical.

    A `historical = "<why>"` entry is a person's written reason for a citation that DELIBERATELY
    names code this tree has not got — the record of a refusal that was deleted, left at the
    address it was measured at — and `CITATION_EXEMPT` above already says that is exactly what
    this rule fails on. Such a citation stayed green here only while its FILE happened to exist:
    the number fell inside some unrelated stretch of it. When `src/fleet.rs` became `src/fleet/`
    (SKEIN-934), nine of them in `docs/recovery-survey.md` turned red for a reason no edit to the
    prose could fix, because what they record is still true and still gone.

    Keyed by the address AND the documents the entry names in `cited_by`, so a declaration excuses
    the citation it was written for and not the same address cited anywhere else.
    """
    try:
        with open(LINE_CITES if path is None else path, "rb") as fh:
            ledger = tomllib.load(fh)
    except FileNotFoundError:
        return {}
    return {k: set(v.get("cited_by", [])) for k, v in ledger.items() if "historical" in v}


def bad_citations(
    index=None, sources=None, lengths=None, at=at_commit, notes=None, historical=None, excused=None
):
    """{label: [(line, text, why)]} for every citation that cannot be followed.

    `historical` is `historical_citations()`'s shape and read from the ledger when not given; a
    citation it declares for this document is not a finding, and is appended to `excused` as
    `(label, line, text)` so the summary can say how many were excused rather than go quiet.

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
    historical = historical_citations() if historical is None else historical
    excused = [] if excused is None else excused

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
            # The ledger's key is the address as `line-cite-check.py` resolved it — the FIRST line
            # of a range, where this rule reads the last — and the path as written where it
            # resolves to nothing, which is the state a historical one is in once its file is gone.
            first = re.search(r":(\d+)(?:-\d+)?$", text)
            key = f"{target or path}:{first.group(1) if first else cited}"
            unfollowable = target == "" or cited > measure(target)
            if unfollowable and not when and label in historical.get(key, ()):
                excused.append((label, line, text))
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
    """The exemption file: {symbol: reason}. Parsed by hand — this is two shapes of line.

    **A line this cannot read is an error, not a skip**, and that is the whole of the change made
    here. The pattern below accepts a BARE key; TOML also allows a quoted one, and
    `"name" = "reason"` is a perfectly legal spelling of the same entry that this parser used to
    drop on the floor. Three declarations written that way were silently ignored, the gate went on
    reporting the symbols as undeclared, and nothing anywhere said the entries had not been read —
    a declaration that looks made and is not, which is exactly the shape `residue-check`'s
    `misfiled` was written for after eleven entries sat above the first table header enforcing
    nothing (SKEIN-540, SKEIN-640).

    Refusing is better than widening the pattern to accept quoted keys. Widening fixes the one
    spelling somebody happened to try; refusing fixes every spelling nobody has tried yet, and
    costs one sentence to the person who wrote it.

    THE ONE QUOTED KEY THIS DOES ACCEPT is a QUALIFIED one — `"mod::name" = "reason"`, the module
    rule's entry (SKEIN-695) — and the exception is forced rather than chosen. TOML has no bare
    key that can hold a `::`, so an unquoted `mod::name = "…"` would make this file unparseable by
    every TOML reader including `tomllib`, which is how the debt file beside it is read. A bare
    name written in quotes is still refused, because for that one there IS a spelling that works
    and the paragraph above is why it has to be the only one.
    """
    if not os.path.exists(SPEC):
        return None
    spec, reason = {}, []
    for n, line in enumerate(open(SPEC, encoding="utf-8"), 1):
        if line.startswith("#"):
            reason.append(line.lstrip("# ").rstrip())
            continue
        if not line.strip():
            reason = []
            continue
        m = re.match(r'^([A-Za-z_][A-Za-z0-9_]*)\s*=\s*"(.*)"\s*$', line) or re.match(
            r'^"([A-Za-z_][A-Za-z0-9_]*(?:::[A-Za-z_][A-Za-z0-9_]*)+)"\s*=\s*"(.*)"\s*$', line
        )
        if not m:
            print(
                f"prose-check: docs/prose-symbols.toml:{n} is not an entry this file can read\n"
                f"             {line.rstrip()[:96]}\n"
                f"             rule: an entry here is `name = \"reason\"` on one line, with a BARE "
                f"name for rule one and a QUOTED `\"mod::name\"` for the module rule. TOML would "
                f"accept other spellings and this parser would not, so a declaration written "
                f"another way would sit here exempting nothing"
            )
            sys.exit(2)
        spec[m.group(1)] = m.group(2)
    return spec


def render(found, qualified, spec, debt):
    """The file `--update` writes. `found` is rule one's names, `qualified` the module rule's.

    Both, because this command rewrites the file from scratch: a qualified entry left out here
    would be deleted by the tool's own maintenance command, which is the quietest way to lose a
    declaration and the reason `docs/prose-debt.toml` is a separate file at all.
    """
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
        "# A key may be QUALIFIED — `\"mod::name\"`, in quotes because TOML has no bare key that",
        "# can hold a `::`. That is the module rule's entry: the name is in the tree, and the",
        "# module the prose puts it in is the part being declared right.",
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
    for name in sorted(qualified):
        if name in debt:
            continue
        why = (spec or {}).get(name, "TODO: say why this module has not got this name")
        out.append(f'"{name}" = "{why}"')
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


# One function, and seven sentences about it — one naming three words less of its name than it has,
# and two naming something gone without putting it in readable backticks. That truncation, the name
# the fixture tree has nothing like, and both unbackticked names are the findings; the full name and
# the two affixes are not. The concrete changes that make the assertion fail: matching a bare
# substring again (the truncation stops being a finding), anchoring an affix on the side its name
# was cut on (both affixes become findings), and reading only `BACKTICKED` (the last two stop being
# findings — which is the state SKEIN-829 replaced, and the one that let SKEIN-561 happen). The
# eighth sentence names a variable with its `$`, and stops being a finding if `BACKTICKED` stops
# taking the `$` (SKEIN-648).
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
            "// Written bare, in a parenthetical (kit::a_fixture_gate_written_bare) — the exact"
            " shape of SKEIN-561's two dead names, in a file the gate did read.",
            "// And `kit::a_fixture_gate_in_a_broken_span(` IS marked up, but the paren sits"
            " inside the span, so the readable-backtick rule cannot take it either.",
            "// `$A_FIXTURE_VARIABLE_NOBODY_READS` is a variable, spelled the way a shell spells it.",
        ],
    )
]


# Two modules and four sentences, one per thing the module rule has to get right. The names are
# INVENTED for the reason `SELF_CHECK_SYMBOL_CODE` gives: a literal in this file is code to
# `code_text`, so quoting a real one would switch the gate off for the name under test.
#
# The concrete change that makes each line of the fixture fail, which is why there are four:
#   1. drop the qualifier and judge the leaf again — line 1 stops being a finding, which is the
#      whole of SKEIN-695;
#   2. require a DEFINITION in the module instead of a mention — line 3 becomes a finding, and so
#      does every re-exported name in the tree;
#   3. report an unresolvable qualifier instead of skipping it — line 4 becomes a finding, and so
#      does every `Type::method` and every other crate's path;
#   4. any error that makes line 2 fire takes every correctly qualified name in the tree with it.
SELF_CHECK_MODULE_CODE = (
    "fn a_fixture_helper_that_belongs_to_beta() {}\n"
    "fn a_fixture_helper_that_beta_shares() {}\n"
)
SELF_CHECK_MODULES = {
    "alpha": "pub use crate::beta::a_fixture_helper_that_beta_shares;\n",
    "beta": SELF_CHECK_MODULE_CODE,
}
SELF_CHECK_MODULE_PROSE = [
    (
        "fixture.rs",
        [
            "// `alpha::a_fixture_helper_that_belongs_to_beta` is in a module that has not got it.",
            "// `beta::a_fixture_helper_that_belongs_to_beta` is in the module that defines it.",
            "// `alpha::a_fixture_helper_that_beta_shares` is reachable there through a re-export.",
            "// `gamma::a_fixture_helper_that_belongs_to_beta` names no module of this tree at all.",
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
        "a_fixture_gate_written_bare": ["fixture.rs:6"],
        "a_fixture_gate_in_a_broken_span": ["fixture.rs:7"],
        "A_FIXTURE_VARIABLE_NOBODY_READS": ["fixture.rs:8"],
    }
    if symbols != want_symbols:
        raise SystemExit(
            "prose-check: the symbol rule is broken — against a tree of one function it did not "
            "report exactly the names that tree does not have. A truncation of a real name must "
            "be a finding (it is not, if the match is a substring again); an affix must not be "
            "one (it is, if the boundary is applied to the side the name was cut on); and a name "
            "written WITHOUT readable backticks must be a finding (it is not, if the extraction "
            "is `BACKTICKED` alone — the state that let SKEIN-561's two dead names through a "
            "scan of the very file they lived in); and a variable written `$LIKE_THIS` must be "
            "one (it is not, if `BACKTICKED` stops taking the `$` — SKEIN-648).\n"
            "  wanted %r\n  got    %r" % (want_symbols, symbols)
        )
    # A LEDGER ENTRY THE CODE MADE UNNECESSARY SAYS SO (SKEIN-559). A fixture test names a binary
    # as a bare string literal that is also a declared name; the finding must point at the literal,
    # and a comment naming it must not count as the code having it.
    binary = "a_fixture_" + "binary_name"
    literal = [("tests/t.rs", 'fn t() {\n    let b = "' + binary + '";\n}'),
               ("tests/u.rs", without_comments("// " + binary + "\nfn u() {}", ".rs"))]
    said = no_longer_needed("docs/prose-debt.toml", binary, "records it as stale",
                            ["docs/x.md:3"], code_sites(binary, literal))
    if "tests/t.rs:2" not in said or "tests/u.rs" in said or "no longer names" in said:
        raise SystemExit(
            "prose-check: a ledger entry made unnecessary by a string literal in the code is not "
            "reported as that — it must name the literal's line, not a comment, and must not "
            "claim the prose stopped naming it (SKEIN-559).\n  got: " + said
        )
    # THE MEMOISED MATCH IS THE SAME QUESTION (SKEIN-721). Both rules answer `code_has` from a set
    # now, so the set has to agree with it on every shape where they could part: a name after a
    # digit and after a non-ASCII letter (a `findall` token, and no `\b` match), a name before a
    # non-ASCII letter, at either end of the text, inside a longer name, and a name that is only
    # there as an affix. Built from fragments, for the reason the symbol fixture above gives.
    tricky = "9" + "alpha x" + "\u00e9" + "beta gamma" + "\u00e9 (delta) epsilon_zeta eta"
    names = whole_names(tricky)
    for leaf in ("alpha", "beta", "gamma", "delta", "epsilon", "zeta", "epsilon_zeta", "eta",
                 "x", "_zeta", "epsilon_", "theta"):
        if has_name(leaf, tricky, names) != code_has(leaf, tricky):
            raise SystemExit(
                "prose-check: the memoised name match disagrees with `code_has` about %r in %r — "
                "it answers %r where the definition answers %r, so one of the two rules is now "
                "asking a different question from the other (SKEIN-721)."
                % (leaf, tricky, has_name(leaf, tricky, names), code_has(leaf, tricky))
            )
    # THE MODULE RULE, against two fixture modules. One name is named under the module that has
    # not got it, one under the module that defines it, one under a module that re-exports it, and
    # one under a module this fixture tree does not have. Exactly the first is a finding.
    qualified = misqualified(SELF_CHECK_MODULE_CODE, SELF_CHECK_MODULE_PROSE, SELF_CHECK_MODULES)
    want_qualified = {"alpha::a_fixture_helper_that_belongs_to_beta": ["fixture.rs:1"]}
    if qualified != want_qualified:
        raise SystemExit(
            "prose-check: the module rule is broken — against two fixture modules it did not "
            "report exactly the name whose module has not got it. A name under the wrong module "
            "must be a finding (it is not, if the qualifier is dropped again); a re-exported one "
            "and one under an unknown qualifier must not be (they are, if a definition is "
            "required or an unresolvable qualifier is reported), and neither must a correctly "
            "qualified name (SKEIN-695).\n  wanted %r\n  got    %r" % (want_qualified, qualified)
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
    cited = bad_citations(SELF_CHECK_INDEX, SELF_CHECK_CITATIONS, SELF_CHECK_LENGTHS, historical={})
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

    # A `historical` DECLARATION EXCUSES THE CITATION IT WAS WRITTEN FOR, and nothing else. The
    # same fixture with `gone.rs:1` declared for `fixture.md` and `gone2.rs:1` declared for a
    # DIFFERENT document: the first stops being a finding and is counted as excused, the second
    # stays a finding. Fails if the ledger is ignored (the first reappears), if a declaration
    # excuses its address everywhere (the second disappears), or if an excuse is not counted.
    excused = []
    declared = bad_citations(
        SELF_CHECK_INDEX,
        SELF_CHECK_CITATIONS,
        SELF_CHECK_LENGTHS,
        historical={"gone.rs:1": {"fixture.md"}, "gone2.rs:1": {"elsewhere.md"}},
        excused=excused,
    )
    still = {k: [c for c in v if c[1] != "gone.rs:1"] for k, v in want.items()}
    if declared != still or excused != [("fixture.md", 3, "gone.rs:1")]:
        raise SystemExit(
            "prose-check: a citation `docs/line-cites.toml` declares historical for its document "
            "was not excused exactly — only that one, only there, and counted\n  wanted %r and "
            "[('fixture.md', 3, 'gone.rs:1')] excused\n  got    %r and %r excused"
            % (still, declared, excused)
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
        historical={},
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
                     notes=gone, historical={}) != {} or gone != [("dated.md", "abc1234", "absent")]:
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
    # SKEIN-561's shape, guarded where it happened: a source that reads ONE directory while the
    # gate reports on the tree. `main` compares counts, which catches a walk that has been made
    # smaller; this catches the other half — a walk that still reaches the tree but has been
    # re-rooted, so the count is whatever that root holds and agrees with itself. The sabotage
    # that makes it fail is the state this replaced: point `shell_files` at `src/store` and every
    # label it yields starts there.
    labels = [label for label, _ in shell_files()]
    if not labels or all(label.startswith("src/store/") for label in labels):
        raise SystemExit(
            "prose-check: the shell source %s, so the sixteen scripts elsewhere in the tree — "
            "eleven of them under src/probe/, which the install table in src/probes.rs writes "
            "into a project's store beside the eight this reaches — are prose this gate claims to "
            "read and cannot see. A gate silent about a file type is read as having found it "
            "clean (SKEIN-561)"
            % ("derives no files at all" if not labels else "reaches nothing outside src/store/")
        )
    # SKEIN-1146: an ignored directory must not be read as this repository's own prose. Built in a
    # REAL scratch git repository rather than fixture strings, because the defect is a property of
    # git and `.gitignore` that no in-memory tree can stand in for — and NOT under `ROOT`, since an
    # ignored path under `ROOT` is invisible to every git command run here, which is exactly the
    # property a broken fix would fail to have.
    with tempfile.TemporaryDirectory(prefix="prose-check-self-check-") as scratch:

        def run(*args):
            subprocess.run(["git", "-C", scratch] + list(args), check=True, capture_output=True)

        run("init", "-q")
        run("config", "user.email", "prose-check-self-check@example.invalid")
        run("config", "user.name", "prose-check self_check")
        os.makedirs(os.path.join(scratch, "skein", "bin"))
        with open(os.path.join(scratch, ".gitignore"), "w", encoding="utf-8") as f:
            f.write("/skein/\n")
        stale = "a_fixture_symbol_" + "the_tree_does_not_have"
        with open(os.path.join(scratch, "skein", "bin", "box-status.sh"), "w", encoding="utf-8") as f:
            f.write("#!/bin/sh\n# " + stale + "\n")
        run("add", ".gitignore")
        run("commit", "-q", "-m", "gitignore /skein/")
        ignored = tracked_files(scratch)
        if ignored is None or any(p.startswith("skein/") for p in ignored):
            raise SystemExit(
                "prose-check: tracked_files() reports a path under a directory `.gitignore` "
                "names — a checkout that is also a skein box would have its installed copy of "
                "the store scripts read as this repository's own prose again (SKEIN-1146)\n"
                "  got %r" % ignored
            )
        run("add", "-f", "skein/bin/box-status.sh")
        run("commit", "-q", "-m", "track the installed copy")
        tracked = tracked_files(scratch)
        if tracked is None or "skein/bin/box-status.sh" not in tracked:
            raise SystemExit(
                "prose-check: tracked_files() does not report a file once it is tracked, even "
                "though `.gitignore` still names its directory — `git ls-files` itself would not "
                "do that, so the result is being filtered on the way out (SKEIN-1146)\n"
                "  got %r" % tracked
            )


def main():
    self_check()
    # Read once and handed to both rules. `prose_sources` walks the markdown, the page, every
    # `.rs` file under `src/` and `tests/`, the store's shell and the cockpit suites, and each
    # rule calling it for itself read all of that twice.
    code, sources = code_text(), list(prose_sources())

    # **A source that is silently off is worse than a source nobody added**, because the gate goes
    # on saying it read the prose. That is SKEIN-647's lesson exactly — a derived check that
    # answered 0 on a box carrying 195 matching processes — so this one refuses to run rather than
    # passing on an empty half. Both halves are named because they are two branches of `ui_prose`
    # and either can go on its own: deleting the `.mjs` branch leaves the READMEs and a green gate
    # over every suite's comments, which is the state this exists to make loud.
    ui = [label for label, _ in sources if label.startswith("tests/ui/")]
    for what in (".mjs", ".md"):
        on_disk = ui_prose_files(what)
        if on_disk and not any(label.endswith(what) for label in ui):
            print(
                f"prose-check: tests/ui/ holds {on_disk} `{what}` file(s) and none of their prose "
                f"came out of prose_sources()\n"
                f"             rule: the cockpit's suites are a prose source (SKEIN-699). A rule "
                f"that reads nothing reports nothing and is indistinguishable from a rule that "
                f"found nothing wrong — so this refuses to run rather than answer about prose it "
                f"did not read"
            )
            return 2

    # The same refusal for the shell half, and a COUNT rather than a presence, because that is the
    # failure this one actually has. `ui_prose`'s guard asks whether anything came out; the shell
    # source spent its life reading `src/store/` and answering as though it had read the tree, and
    # an emptiness check is green on exactly that. So this compares what came out with what is on
    # disk: narrow the walk back to one directory and the numbers disagree and the gate refuses.
    #
    # THE COUNT NOW COMES FROM A SECOND WALK. It read `sum(1 for _ in shell_files())` against a
    # count of what `prose_sources` yielded — and `prose_sources` yields exactly what `shell_files`
    # gives it, so narrowing the walk moved both sides together and only the `self_check` guard
    # below saw it. That guard recognises the one root this went wrong at; the count now sees any
    # of them. `prose_source_count` has the reproduction and the numbers.
    on_disk = prose_source_count(".sh")
    read = sum(1 for label, _ in sources if label.endswith(".sh"))
    if on_disk == 0 or read != on_disk:
        print(
            f"prose-check: the tree holds {on_disk} `.sh` file(s) and prose_sources() read "
            f"{read}\n"
            f"             rule: a shell script installed into a box is read by whoever opens the "
            f"box, so its comments are prose (SKEIN-561). Deriving none, or fewer than are there, "
            f"means this gate is silent about scripts it reports on — which is worse than not "
            f"reading them, because the green is read as `the tree is clean`"
        )
        return 2

    # And the same COUNT guard for the toml half, from the same second walk and for the same
    # reason. What it catches is the READER going quiet — `toml_files` narrowed, or the branch in
    # `prose_sources` deleted — because `prose_source_count` lists `docs/` itself rather than
    # asking `toml_files` how many it found. What it deliberately does NOT catch is
    # `TOML_PROSE_EXEMPT` GROWING, since both sides read that constant: an exemption is a decision
    # in a diff with an argument written beside it, not a slip, and a guard that re-derived it
    # would be asserting a number nobody is allowed to change. The half of "the exemption grew"
    # that IS a slip — a name left behind by a renamed or deleted ledger — is the check below it.
    stale = stale_toml_exemptions()
    if stale:
        print(
            f"prose-check: TOML_PROSE_EXEMPT names {', '.join(stale)}, which is not in docs/\n"
            f"             rule: an exemption that exempts nothing is a permission nobody "
            f"granted, and the next file to take that name would be out of scope without anyone "
            f"deciding it. Drop the name, or point it at the file the ledger became"
        )
        return 2
    on_disk = prose_source_count(".toml")
    read = sum(1 for label, _ in sources if label.endswith(".toml"))
    if on_disk == 0 or read != on_disk:
        print(
            f"prose-check: {on_disk} `docs/*.toml` file(s) are in scope and prose_sources() read "
            f"{read}\n"
            f"             rule: a `#` comment in `docs/modules.toml` or `docs/env-lock.toml` is "
            f"the ARGUMENT for an edge or an exemption, and is read by whoever has to decide "
            f"whether it is still right — so it is prose. Deriving none, or fewer than are in "
            f"scope, means this gate answers about documents it did not open"
        )
        return 2

    # And the markdown under `docs/`, at every depth (SKEIN-1160): `os.listdir(docs)` read one level
    # and `docs/decisions/` was below it, so a record could name a function the tree never had.
    on_disk = prose_source_count("docs/.md")
    read = sum(1 for label, _ in sources if label.startswith("docs/") and label.endswith(".md"))
    if on_disk == 0 or read != on_disk:
        print(
            f"prose-check: {on_disk} `.md` file(s) are under docs/ and prose_sources() read "
            f"{read}\n"
            f"             rule: a document under docs/, at any depth, is prose, and "
            f"docs/decisions/ is a level down (SKEIN-1160). A new file is read once git tracks it, "
            f"so `git add -N` it; otherwise the reader has been narrowed, and this refuses to "
            f"answer about documents it did not open"
        )
        return 2

    found = absent(code=code, sources=sources)
    qualified = misqualified(code=code, sources=sources)
    attached = doc_attachments()
    interrupted = doc_interruptions()
    dated, excused = [], []
    cited = bad_citations(notes=dated, excused=excused)

    if "--show" in sys.argv:
        for name in sorted(found):
            print(f"{name:34} {', '.join(found[name])}")
        print(f"\n{len(found)} symbol(s) named in prose that the tree does not have")
        for name in sorted(qualified):
            print(f"{name:34} {', '.join(qualified[name])}")
        print(f"{len(qualified)} name(s) whose module has not got them")
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
        open(SPEC, "w", encoding="utf-8").write(
            render(found, qualified, spec, load_debt()[0] or {})
        )
        print(
            "  NOTE: --update rewrites the file from the tree and keeps only the\n"
            "  name = reason lines. The section headings that group them by KIND are\n"
            "  dropped; read the diff before keeping it."
        )
        print(
            f"wrote {os.path.relpath(SPEC, ROOT)} "
            f"({len(found)} symbol(s), {len(qualified)} qualified)"
        )
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

    # One key space, two rules. A qualified key belongs to the module rule and a bare one to rule
    # one, in both files, so each list is pruned against the findings it is actually about — a
    # qualified entry checked against rule one's names would be reported as exempting nothing on
    # every run, which is how a gate teaches people to ignore it.
    bare_spec = {k: v for k, v in spec.items() if "::" not in k}
    qualified_spec = {k: v for k, v in spec.items() if "::" in k}
    bare_stale = {k: v for k, v in stale.items() if "::" not in k}
    qualified_stale = {k: v for k, v in stale.items() if "::" in k}

    problems = []
    for name in sorted(found):
        if name in bare_stale:
            continue
        if name not in bare_spec:
            where = ", ".join(found[name][:4])
            more = f" (+{len(found[name]) - 4} more)" if len(found[name]) > 4 else ""
            problems.append(
                f"prose-check: the prose names `{name}` and the tree does not have it\n"
                f"             at {where}{more}\n"
                f"             rule: a claim about the code that names something gone is worse "
                f"than no claim — fix the sentence, or declare the name in "
                f"docs/prose-symbols.toml with why it is not here"
            )
    for ledger, rule, names in (
        ("docs/prose-symbols.toml", "exempts `%s`", set(bare_spec) - set(found)),
        ("docs/prose-debt.toml", "records `%s` as stale", set(bare_stale) - set(found)),
    ):
        for name in sorted(names):
            prose_at = prose_sites(name)
            problems.append(
                no_longer_needed(
                    ledger, name, rule % name, prose_at, code_sites(name) if prose_at else []
                )
            )
    # The module rule, with the same two lists and the same pruning. The message names the module
    # rather than the symbol, because that is the half the reader followed and lost.
    for name in sorted(qualified):
        if name in qualified_stale or name in qualified_spec:
            continue
        where = ", ".join(qualified[name][:4])
        more = f" (+{len(qualified[name]) - 4} more)" if len(qualified[name]) > 4 else ""
        module, leaf = name.split("::")[-2], name.split("::")[-1]
        problems.append(
            f"prose-check: the prose names `{name}` and `{module}` has not got `{leaf}` — not a "
            f"definition, not a re-export, not a mention\n"
            f"             at {where}{more}\n"
            f"             rule: a qualified name is a direction to a file, and one that sends "
            f"the reader to a module the symbol has never been in costs them the rest of the "
            f"sentence too — they open it, do not find the name, and have no reason to believe "
            f"anything else you wrote. Fix the qualifier, or declare the whole name in "
            f"docs/prose-symbols.toml with why that module is the right one to name"
        )
    for name in sorted(set(qualified_spec) - set(qualified)):
        problems.append(
            f"prose-check: docs/prose-symbols.toml exempts `{name}` and nothing needs it\n"
            f"             rule: either that module has the name now or no prose qualifies it "
            f"that way — an allow-list nobody prunes is a permission nobody granted. Drop the "
            f"entry"
        )
    for name in sorted(set(qualified_stale) - set(qualified)):
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
    misqualifications = sum(len(v) for v in qualified.values())
    print(
        f"every symbol the prose names is in the code or declared absent "
        f"({len(bare_spec)} declared, {len(bare_stale)} stale and scheduled, {named} mention(s); "
        f"{glued} glued doc block(s) recorded in docs/prose-debt.toml); "
        f"every `mod::name` whose module this tree has names something that module has "
        f"({len(qualified_spec)} declared, {len(qualified_stale)} stale and scheduled, "
        f"{misqualifications} mention(s)); "
        f"every `file:line` citation names a file that exists and a line inside it "
        f"({rotted} deferred in docs/prose-debt.toml{dated_note(dated)}"
        f"; {len(excused)} declared historical in docs/line-cites.toml)"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
