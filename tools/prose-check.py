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

WHAT IS CHECKED. A backticked identifier, in `docs/*.md` or in a comment in the page, that is
shaped like a symbol in this tree — snake_case, or a `rev*`/`api*` page function — and appears
nowhere in `src/`, `tests/`, `cockpit/`, `warden/` or `tools/`. A qualified name is judged by
its last segment, so `prq::submit_review_with_comments` asks about the function.

WHAT IS NOT, and deliberately. Prose that describes something without naming it is untouched;
this is a spell-check for identifiers, not a fact-checker. It cannot tell you a sentence is
wrong about a function that still exists — only that the function is gone.

THE EXEMPTION FILE (`docs/prose-symbols.toml`) is the interesting half. Two things belong in
it and nothing else: names that are somebody ELSE'S (a kernel capability, an environment
variable Claude Code sets), and names this tree deliberately discusses in the past tense
("it used to call X"). Both are legitimate, and both are decisions, so they are written down
once with the reason rather than argued again at each sighting.

A stale exemption fails too — an allow-list nobody prunes is a permission nobody granted.

    python3 tools/prose-check.py            # the gate
    python3 tools/prose-check.py --show     # every absent symbol and where it is named
    python3 tools/prose-check.py --update   # rewrite the exemption file from the tree
"""

import os
import re
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SPEC = os.path.join(ROOT, "docs", "prose-symbols.toml")

# Where a symbol may live. The page is both prose and code, so it is on both lists.
CODE_DIRS = ["src", "tests", "cockpit", "warden", "tools"]
CODE_SUFFIXES = (".rs", ".py", ".mjs", ".js", ".html", ".toml", ".sh", ".json")

# Backticked, and either qualified (`a::b`) or bare. The last segment is what is looked up.
BACKTICKED = re.compile(r"`([A-Za-z_][A-Za-z0-9_]*(?:::[A-Za-z_][A-Za-z0-9_]*)*)`")

# A comment in the page, which is where the page explains itself.
PAGE_COMMENT = re.compile(r"^\s*(?://|///)")


def looks_like_a_symbol(name):
    """Shaped like something in this tree, rather than an English word in backticks.

    Tight on purpose. A bare word — `main`, `true`, `diff` — is how a gate like this drowns in
    its own output and gets switched off, so a name qualifies only by carrying an underscore or
    by being one of the page's two function prefixes. The cost is that a one-word Rust function
    named in prose is not checked; the benefit is that every hit is worth reading.
    """
    return "_" in name or name.startswith("rev") or name.startswith("api")


def code_text():
    out = []
    for d in CODE_DIRS:
        for base, dirs, files in os.walk(os.path.join(ROOT, d)):
            dirs[:] = [x for x in dirs if x not in ("node_modules", "target", ".git")]
            for f in files:
                if f.endswith(CODE_SUFFIXES):
                    try:
                        out.append(open(os.path.join(base, f), encoding="utf-8").read())
                    except (OSError, UnicodeDecodeError):
                        pass
    return "\n".join(out)


def prose_sources():
    """Every file whose prose is checked, as (label, lines)."""
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


def render(found, spec):
    out = [
        "# Symbols this project's prose names that the code does not have. Read by",
        "# `tools/prose-check.py`, which fails the build on any other one.",
        "#",
        "# Two things belong here and nothing else:",
        "#",
        "#   * a name that is SOMEBODY ELSE'S — a kernel capability, a variable another tool sets.",
        "#     The tree will never contain it and should not be made to.",
        "#   * a name this tree discusses in the PAST TENSE, deliberately. A design record that",
        "#     explains why a thing was removed has to be able to say what it was called.",
        "#",
        "# Anything else is drift, and the point of the gate is that it stops rather than",
        "# accumulating. `--update` writes the names; the reasons are written by a person.",
        "",
    ]
    for name in sorted(found):
        why = (spec or {}).get(name, "TODO: say why the code does not have this")
        out.append(f'{name} = "{why}"')
    return "\n".join(out) + "\n"


def main():
    found = absent()

    if "--show" in sys.argv:
        for name in sorted(found):
            print(f"{name:34} {', '.join(found[name])}")
        print(f"\n{len(found)} symbol(s) named in prose that the tree does not have")
        return 0

    spec = load_spec()
    if "--update" in sys.argv:
        open(SPEC, "w", encoding="utf-8").write(render(found, spec))
        print(f"wrote {os.path.relpath(SPEC, ROOT)} ({len(found)} symbol(s))")
        return 0

    if spec is None:
        print(
            "prose-check: docs/prose-symbols.toml is missing, so the law is unenforced\n"
            "             rule: run `python3 tools/prose-check.py --update` and write the reasons"
        )
        return 1

    problems = []
    for name in sorted(found):
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
    for p in problems:
        print(p + "\n")
    if problems:
        print(
            f"{len(problems)} problem(s). `python3 tools/prose-check.py --show` lists every "
            f"absent symbol with where it is named."
        )
        return 1

    named = sum(len(v) for v in found.values())
    print(
        f"every symbol the prose names is in the code or declared absent "
        f"({len(spec)} declared, {named} mention(s))"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
