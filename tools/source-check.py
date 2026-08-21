#!/usr/bin/env python3
"""The Source law, made checkable: nothing reaches anything except through a Source.

`docs/architecture.md` §2.3 names four Sources — `enter`, `socket`, `file`, `http` — and says the
law becomes enforceable once they exist. A law nothing checks is a paragraph. This is the check,
and it is the same shape as `tools/module-check.py`: an allow-list of where each Source is spelled
today, generated from the code and then reviewed, so that a NEW way of reaching something is a line
in a diff rather than a call nobody looked at.

What it does not claim: that today's spread is right. `enter` is spelled in six files and belongs
in one. The point is that the spread cannot quietly get wider while the rewrite is under way.

Test modules are cut before matching, brace-matched, for the same reason module-check cuts them: a
fixture that spells `nsenter` in an assertion is describing the code, not reaching anything. The cut
is brace-matched rather than "everything after the marker" because the cheap version stops reading
at the test module and every item below it becomes invisible.

  python3 tools/source-check.py           check
  python3 tools/source-check.py --update  rewrite the allow-list from the code
  python3 tools/source-check.py --show    print where each Source is spelled
"""

import os, re, sys, collections, tomllib

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SRC = os.path.join(ROOT, "src")
SPEC = os.path.join(ROOT, "docs", "sources.toml")

# How each Source is spelled in Rust. Deliberately the PRIMITIVE, not the wrapper: `place.exec()` is
# skein's own front door and finding it proves nothing, while `nsenter` is the syscall dressed as a
# command and cannot be spelled by accident.
SPELLINGS = {
    "enter": [r"\bnsenter\b"],
    "socket": [r"tmux -S", r'Command::new\("tmux"\)'],
    "file": [],  # every module reads files; the law here is about what a file read may REACH, not
                 # about `fs::read`. Left empty on purpose rather than made up.
    "http": [
        r'Command::new\("curl"\)',
        r'Command::new\("gh"\)',
        r"\bTcpStream\b",
        r"\breqwest\b",
        r"\bureq\b",
    ],
    # Not a Source of its own: `sbx` is how the host reaches the SANDBOX, which is the outer shell
    # of every `enter`. Tracked separately so the two do not get confused when the transport moves
    # in-fleet and `sbx` stops being on the path at all.
    "sbx": [r'Command::new\("sbx"\)'],
}


def uncommented(text):
    """Source with every comment removed. A doc comment that says "via nsenter" is prose."""
    out = []
    for line in text.split("\n"):
        if line.lstrip().startswith("//"):
            continue
        out.append(re.sub(r"//.*$", "", line))
    return "\n".join(out)


def without_tests(text):
    """Everything outside `#[cfg(test)] mod tests { .. }`, brace-matched."""
    m = re.search(r"^#\[cfg\(test\)\]\nmod tests \{", text, re.M)
    if not m:
        return text
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
    return text[: m.start()] + text[i:]


def units():
    for f in sorted(os.listdir(SRC)):
        if f.endswith(".rs"):
            yield f[:-3], os.path.join(SRC, f)
    binaries = os.path.join(SRC, "bin")
    for f in sorted(os.listdir(binaries)):
        if f.endswith(".rs"):
            yield "bin/" + f[:-3], os.path.join(binaries, f)


def read_reaches():
    """{source: Counter(unit -> hits)} over non-test, non-comment code."""
    found = {name: collections.Counter() for name in SPELLINGS}
    for unit, path in units():
        # `source.rs` is where the Sources are DESCRIBED, and it reaches nothing. It names `nsenter`
        # in a string — the "reaches" column of §2.3's table — and counting that would put the
        # taxonomy on the list of things that cross into boxes.
        if unit == "source":
            continue
        body = uncommented(without_tests(open(path, encoding="utf-8").read()))
        for source, patterns in SPELLINGS.items():
            for pattern in patterns:
                hits = len(re.findall(pattern, body))
                if hits:
                    found[source][unit] += hits
    return found


def load_spec():
    if not os.path.exists(SPEC):
        return {}
    with open(SPEC, "rb") as f:
        return tomllib.load(f)


def render(found):
    out = [
        "# Where each Source is spelled today. Read by `tools/source-check.py`, which fails the",
        "# build on a reach from a file that is not listed.",
        "#",
        "# Generated from the code (`--update`) and then reviewed. The list is not an argument that",
        "# today's spread is right — `enter` is spelled in several files and belongs in one. It is",
        "# there so the spread cannot quietly get wider while the rewrite is under way.",
        "#",
        "# architecture.md §2.3 is the design; `src/source.rs` is the taxonomy, and its own test",
        "# checks itself against §2.3.",
        "",
    ]
    for source in SPELLINGS:
        out.append(f"[{source}]")
        units_ = sorted(found[source])
        rendered = ", ".join(f'"{u}"' for u in units_)
        out.append(f"spelled_in = [{rendered}]")
        out.append("")
    return "\n".join(out).rstrip() + "\n"


def main():
    found = read_reaches()
    if "--show" in sys.argv:
        for source, hits in found.items():
            where = ", ".join(f"{u}({n})" for u, n in sorted(hits.items())) or "nowhere"
            print(f"{source:8} {where}")
        return 0
    if "--update" in sys.argv:
        open(SPEC, "w", encoding="utf-8").write(render(found))
        print(f"wrote {os.path.relpath(SPEC, ROOT)}")
        return 0

    spec = load_spec()
    problems = []
    for source, hits in found.items():
        allowed = set(spec.get(source, {}).get("spelled_in", []))
        for unit in sorted(hits):
            if unit not in allowed:
                problems.append(
                    f"source-check: `{unit}` reaches by `{source}` ({hits[unit]} references), and "
                    f"docs/sources.toml does not allow it\n"
                    f"              rule: a new way of reaching something is a decision — add "
                    f"`{unit}` to [{source}] and say why, or route it through an existing Source"
                )
        for unit in sorted(allowed - set(hits)):
            problems.append(
                f"source-check: docs/sources.toml says `{unit}` reaches by `{source}`, and it no "
                f"longer does\n"
                f"              rule: a stale allow-list is a permission nobody granted — drop the "
                f"entry"
            )
    if not spec:
        problems.append(
            "source-check: docs/sources.toml is missing, so the law is unenforced\n"
            "              rule: run `python3 tools/source-check.py --update` and review it"
        )
    for p in problems:
        print(p + "\n")
    if problems:
        print(
            f"{len(problems)} problem(s). docs/sources.toml is the allow-list; "
            f"`python3 tools/source-check.py --update` rewrites it from the code."
        )
        return 1
    total = sum(sum(h.values()) for h in found.values())
    files = len({u for h in found.values() for u in h})
    print(f"every reach is declared in docs/sources.toml ({total} across {files} units)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
