#!/usr/bin/env python3
"""Every file the markdown points at is a file this repository tracks.

SKEIN-512 moved seven working notes out of the repository, and the documents that linked to them
kept the links. Nothing noticed: `prose-check`, `line-cite-check`, `citation-check`,
`continuation-check` and `source-check` all passed with `[docs/live-check.md](docs/live-check.md)`
appended to `ARCHITECTURE.md`, a file that had just been deleted (SKEIN-1157). Each of those gates
reads something narrower — a symbol, a `path:line`, a sha — and none of them asks the plainest
question a reader's first click asks, which is whether the file is there at all. A stranger who
opens a public repository and meets a dead link on its front page reads it as abandoned.

WHAT IS READ. Every `*.md` that `git ls-files` reports, and nothing else — tracked is the tree, so
an installed copy or a scaffold lying around a checkout is not read (the trade `tracked_files` in
`tools/prose-check.py` argues, SKEIN-1146). Fenced code blocks are skipped: what is inside one is
an example of text, not a claim that a file exists.

WHAT FAILS, two shapes and one question.

  * A markdown link, inline `[text](target)`, image `![alt](target)` or reference definition
    `[label]: target`, whose target is a path. `http:`, `https:` and `mailto:` targets are
    somebody else's and are not followed; a bare `#anchor` points inside the same file; and for
    `path#anchor` only the path is checked, because whether a heading still exists is a question
    about prose, not about the tree. A trailing `:123` or `:12-40` line suffix is dropped for the
    same reason — `line-cite-check` already owns whether the line is there.
  * A backticked inline code span that is, as a whole, a repo path: at least one `/`, a file
    extension on the last segment, and a first segment that is a top-level entry of this
    repository (`docs/`, `src/`, `tools/`, ...). That last condition is what keeps runtime paths
    out — `~/.skein/...`, `<store>/status/...`, `/boxes/...`, `skein/bin/...` all name places on a
    running machine rather than in the tree, and none of them starts with a tracked top-level
    name. A span with a space, a glob or a placeholder in it is a command or a pattern, not a
    path, and is not read. Nor is a span ending in `:123`: that is a citation, and whether its
    file exists is already rule three of `tools/prose-check.py`, with its own ledger — reading it
    here too would make one dead file two findings in two gates with two places to declare it.

  The question in both cases: does the path name a file or directory git tracks? A relative link
  is resolved against the linking file's own directory first, as a markdown renderer does, and
  then against the repository root, because much of this tree's prose writes root-relative paths
  from inside `docs/`. Either resolving is enough. The files of a checked-out submodule count as
  tracked (`--recurse-submodules`); the submodule's own markdown is upstream's and is not read.

THE DECISIONS INDEX, the one place the question runs the other way (SKEIN-1159). A record in
`docs/decisions/` is only found by a reader who meets it in `docs/decisions/README.md`'s index, so
a record the index does not list is as lost as a link to a file that is gone, and the dead-link
rule above cannot see it: nothing points at the record, so nothing is dead. So every tracked
`docs/decisions/*.md` other than the README must be the target of a link in a TABLE ROW of that
README — a mention in its prose does not count, because the table is what a reader scans. The
other direction, an index row naming a record that is not there, is the dead-link rule's already.
A README whose table yields no links at all refuses rather than passes: that is a table this no
longer knows how to read, not an index that lists nothing.

WHAT IS DECLARED. A path the prose names on purpose although the tree does not have it goes in
`docs/links.toml`, under the file that names it, with the reason — the shape of
`docs/prose-symbols.toml`, and the same two classes: a path named in the PAST TENSE, as gone or as
it was when something was measured, and a path that is somebody else's (relative to where a file
is installed, or to another repository). An entry that no longer matches anything fails the gate
too, so the list cannot outlive what it excuses. Keying on the file rather than on the path alone
keeps one document's past tense from excusing the same path in another.

Exit 0 when every path resolves, 1 when one does not (each printed as `file:line: target`), 2 when
git could not list the tree — never 0 over zero files read.
"""

import os
import re
import subprocess
import sys
import tomllib
import urllib.parse

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
LEDGER = os.path.join(ROOT, "docs", "links.toml")

FENCE = re.compile(r"^\s*(```|~~~)")
CODE_SPAN = re.compile(r"(`+)(.+?)\1")
INLINE_LINK = re.compile(r"!?\[(?:[^\[\]]|\[[^\[\]]*\])*\]\(\s*(<[^>]*>|[^)\s]+)(?:\s+[\"'(][^)]*)?\)")
REF_DEF = re.compile(r"^\s{0,3}\[[^\]]+\]:\s*(<[^>]*>|\S+)")
SKIPPED_SCHEMES = ("http:", "https:", "mailto:")
LINE_SUFFIX = re.compile(r":\d+(?:-\d+)?$")
# A backticked span that is a path and only a path: segments of ordinary filename characters,
# at least one `/`, and an extension on the last. Anything else — a space, `*`, `<`, `$`, `~`,
# a leading `/` — makes it a command, a pattern or a runtime path.
REPO_PATH = re.compile(r"^[A-Za-z0-9_.-]+(?:/[A-Za-z0-9_.@+-]+)*/[A-Za-z0-9_.@+-]*\.[A-Za-z0-9]+$")


def git_ls(root, *args):
    try:
        out = subprocess.run(["git", "-C", root, "ls-files", "-z", *args],
                             capture_output=True, check=True).stdout
    except (OSError, subprocess.CalledProcessError):
        return None
    return [p for p in out.decode("utf-8", "replace").split("\0") if p]


def index_of(files):
    """Every tracked file, and every directory that contains one."""
    known = set(files)
    for f in files:
        parts = f.split("/")
        for i in range(1, len(parts)):
            known.add("/".join(parts[:i]))
    return known


def prose_lines(text):
    """(line number, line) outside fenced code blocks."""
    fence = None
    for n, line in enumerate(text.split("\n"), 1):
        m = FENCE.match(line)
        if m:
            if fence is None:
                fence = m.group(1)
            elif m.group(1) == fence:
                fence = None
            continue
        if fence is None:
            yield n, line


def targets(text, top):
    """(line, target, kind) for every path the prose names, before resolution."""
    for n, line in prose_lines(text):
        for m in CODE_SPAN.finditer(line):
            span = m.group(2).strip()  # a `:123` suffix fails REPO_PATH, on purpose
            if REPO_PATH.match(span) and span.split("/", 1)[0] in top:
                yield n, span, "code"
        bare = CODE_SPAN.sub("", line)
        found = [m.group(1) for m in INLINE_LINK.finditer(bare)]
        m = REF_DEF.match(bare)
        if m:
            found.append(m.group(1))
        for t in found:
            yield n, t, "link"


def path_of(target):
    """The repository path a link target names, or None when it names no path."""
    t = target.strip()
    if t.startswith("<") and t.endswith(">"):
        t = t[1:-1].strip()
    if not t or t.startswith("#") or t.lower().startswith(SKIPPED_SCHEMES):
        return None
    t = t.split("#", 1)[0].split("?", 1)[0]
    t = LINE_SUFFIX.sub("", urllib.parse.unquote(t))
    return t or None


def resolves(path, source, known):
    here = os.path.dirname(source)
    tries = [path.lstrip("/")] if path.startswith("/") else [os.path.join(here, path), path]
    for t in tries:
        norm = os.path.normpath(t)
        if norm == ".":
            return True
        if not norm.startswith("..") and norm.rstrip("/") in known:
            return True
    return False


def dead(docs, known, top, exempt):
    """(source, line, target) for every path that resolves to nothing, and the (source, path)
    exemptions that were used. `exempt` is `{source: {path: reason}}`."""
    bad, used = [], set()
    for source, text in docs:
        for n, target, kind in targets(text, top):
            path = target if kind == "code" else path_of(target)
            if path is None or resolves(path, source, known):
                continue
            if path in exempt.get(source, {}):
                used.add((source, path))
                continue
            bad.append((source, n, target))
    return bad, used


DECISIONS = "docs/decisions"
DECISIONS_INDEX = DECISIONS + "/README.md"


def unindexed(files, index_text):
    """(records the index does not list, how many records the index table links to).

    `files` is every tracked path; `index_text` is `docs/decisions/README.md`, or None when that
    file is not tracked. A record is a direct child of `docs/decisions/` ending `.md`, the README
    aside. Only links inside a table row (a line starting `|`) count as listed."""
    records = sorted(f for f in files if os.path.dirname(f) == DECISIONS and f.endswith(".md")
                     and f != DECISIONS_INDEX)
    listed = set()
    for _, line in prose_lines(index_text or ""):
        if not line.lstrip().startswith("|"):
            continue
        for m in INLINE_LINK.finditer(CODE_SPAN.sub("", line)):
            path = path_of(m.group(1))
            if path is not None:
                listed.add(os.path.normpath(os.path.join(DECISIONS, path)))
    return [r for r in records if r not in listed], len(listed)


def self_check():
    """The rules, against text this file holds, so a broken rule fails here rather than passing
    the tree. The comment on each numbered line is the change that line exists to catch.

    A `path:line` suffix is spelled `+ LN` below because `tools/prose-check.py` reads string
    literals in this directory for citations, and a citation of a file that does not exist is
    exactly what these cases are made of."""
    ln = ":" + "12"
    known = index_of(["docs/a.md", "docs/sub/b.md", "src/x.rs", "README.md"])
    top = {"docs", "src", "README.md"}
    doc = "\n".join([
        # 1: resolving against the root only, or the file's directory only, or keeping a suffix
        "[ok](a.md) [ok](sub/b.md#h) [ok](../src/x.rs" + ln + ") [ok](docs/a.md) [ok](/README.md)",
        "[gone](gone.md)",                                   # 2: links not read at all
        "![img](pic.png)",                                   # 3: images dropped from INLINE_LINK
        # 4: following http(s), mailto or a bare anchor as if it were a path
        "[web](https://example.com/x.md) [a](#top) [m](mailto:someone@example.com)",
        # 5: runtime paths flagged, or a citation read here as well as by prose-check
        "`docs/a.md` `src/x.rs" + ln + "` `~/.skein/x.md` `<store>/y.md` `skein/bin/z.sh`",
        "`docs/missing.md`",                                 # 6: backticked paths not read
        "`tools/*.py` `python3 docs/nope.py` `docs/`",       # 7: commands and globs read as paths
        "```",
        "[fenced](nope.md) `docs/fenced.md`",                # 9: fences not skipped
        "```",
        "`[not a link](nope.md)` and `src/gone.rs" + ln + "`",  # 11: links inside code, citations
        "[r]: sub/b.md",                                     # 12: a reference definition misread
        "[r2]: docs/refgone.md",                             # 13: reference definitions not read
        "[spaced](<docs/a b.md>)",                           # 14: angle-bracket targets not read
        "[t](docs/a.md \"title\") [x](%64ocs/a.md)",         # 15: a title or an escape kept
    ])
    bad, _ = dead([("docs/a.md", doc)], known, top, {})
    got = sorted((n, t) for _, n, t in bad)
    want = [(2, "gone.md"), (3, "pic.png"), (6, "docs/missing.md"), (13, "docs/refgone.md"),
            (14, "<docs/a b.md>")]
    assert got == want, f"link-check self-check: expected {want}, got {got}"
    # An exemption keyed on the path alone, not on the file that names it.
    bad, used = dead([("docs/a.md", "[x](gone.md)"), ("src/x.md", "[x](gone.md)")], known, top,
                     {"docs/a.md": {"gone.md": "why"}})
    assert [(s, t) for s, _, t in bad] == [("src/x.md", "gone.md")] and \
        used == {("docs/a.md", "gone.md")}, \
        "link-check self-check: an exemption must excuse its own file's path and no other file's"
    # The decisions index: a record listed only in prose, or only in a fenced block, is unlisted;
    # a record in a subdirectory, the README itself and a non-record file are not records.
    files = ["docs/decisions/README.md", "docs/decisions/a.md", "docs/decisions/b.md",
             "docs/decisions/c.md", "docs/decisions/d.md", "docs/decisions/sub/e.md",
             "docs/decisions/f.txt", "docs/g.md"]
    index = "\n".join([
        "| record | what |", "|---|---|",
        "| [a.md](a.md) | listed |",                 # a row: listed
        "See [b.md](b.md) in passing.",              # prose, not a row: b is unlisted
        "```", "| [c.md](c.md) | fenced |", "```",    # fenced: c is unlisted
        "| `[d.md](d.md)` | in code |",              # inside a code span: d is unlisted
    ])
    missing, rows = unindexed(files, index)
    assert missing == ["docs/decisions/b.md", "docs/decisions/c.md", "docs/decisions/d.md"] \
        and rows == 1, f"link-check self-check: unindexed records {missing}, {rows} row link(s)"


def load_ledger():
    try:
        with open(LEDGER, "rb") as fh:
            data = tomllib.load(fh)
    except FileNotFoundError:
        return {}
    except tomllib.TOMLDecodeError as exc:
        raise SystemExit(f"link-check: docs/links.toml does not parse: {exc}")
    for source, paths in data.items():
        for path, reason in paths.items() if isinstance(paths, dict) else [(None, None)]:
            if not isinstance(reason, str) or not reason.strip():
                raise SystemExit(f"link-check: docs/links.toml [{source}] {path}: every entry is "
                                 "a path with the reason it is not in this tree")
    return data


def main():
    self_check()
    own = git_ls(ROOT)
    tracked = git_ls(ROOT, "--recurse-submodules") or own
    if not own:
        print("link-check: git could not list this tree, so nothing was read", file=sys.stderr)
        return 2
    known = index_of(tracked)
    top = {p.split("/", 1)[0] for p in own}
    exempt = load_ledger()
    docs, unread = [], []
    for rel in sorted(p for p in own if p.endswith(".md")):
        try:
            with open(os.path.join(ROOT, rel), encoding="utf-8") as f:
                docs.append((rel, f.read()))
        except (OSError, UnicodeDecodeError) as exc:
            unread.append((rel, exc))
    for rel, exc in unread:
        print(f"{rel}: tracked, and could not be read, so nothing it links to was checked ({exc})")
    if not docs:
        print("link-check: read zero markdown files, which cannot be a clean tree", file=sys.stderr)
        return 2
    bad, used = dead(docs, known, top, exempt)
    stale = sorted({(s, p) for s, paths in exempt.items() for p in paths} - used)
    for source, n, target in bad:
        print(f"{source}:{n}: {target} is not a file this repository tracks")
    for source, path in stale:
        print(f"docs/links.toml: [\"{source}\"] \"{path}\" excuses nothing any more; delete it")
    index_text = dict(docs).get(DECISIONS_INDEX)
    unlisted, rows = unindexed(own, index_text)
    for record in unlisted:
        print(f"{record}: a decision record {DECISIONS_INDEX}'s index table does not list; add "
              f"its row, which is how a reader finds it")
    if index_text is not None and rows == 0:
        print(f"{DECISIONS_INDEX}: its index table yielded no record links, so which records it "
              f"lists could not be read")
        unlisted = unlisted or [DECISIONS_INDEX]
    if bad or stale or unread or unlisted:
        print(f"link-check: {len(bad)} dead path(s), {len(stale)} stale exemption(s), "
              f"{len(unlisted)} unindexed decision record(s)", file=sys.stderr)
        return 1
    print(f"link-check: {len(docs)} markdown files, every linked or backticked repo path is tracked"
          f" ({len(used)} declared exemption(s)); every decision record is in its index table ({rows} row link(s))")
    return 0


if __name__ == "__main__":
    sys.exit(main())
