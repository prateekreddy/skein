#!/usr/bin/env python3
r"""A `\` line continuation that a heredoc ate, caught before a person reads the gap it left.

THE MECHANISM, WHICH IS THE WHOLE REASON THIS EXISTS. rustfmt cannot break a string literal, so a
long sentence in Rust is written across two source lines with a trailing `\`:

    "the largest boxes are {named} — `skein stop <box>` keeps its checkout, branch and \
     conversation, or clear its build output in place"

rustc drops the backslash, the newline, and the leading whitespace of the next line, and the
literal reads with single spaces. Now write that same edit through a shell or Python heredoc — the
way agents patch this repository all day:

    cat > src/health.rs <<EOF
    ... branch and \
     conversation ...
    EOF

**The heredoc eats the backslash as its own line continuation**, so the `\` never reaches the file.
What lands is one long line with the continuation line's indentation still inside the literal, and
`skein doctor` prints "branch and                  conversation" to the owner. Two more were nearly
added this way under SKEIN-735; fifteen were already in the tree (SKEIN-741). The fix at the
keyboard is to quote the heredoc delimiter — `<<'EOF'` — or to write `\\`; the fix in the tree is
this gate, because the mistake is invisible in a diff and reads as ordinary text in review.

HOW IT TELLS PROSE FROM ALIGNMENT, WHICH IS THE ONLY HARD PART. Deliberate runs of spaces are
everywhere in this repository and must not be touched: `src/bin/skein.rs` aligns the description
column of `skein help` and `skein doctor`, `src/signals.rs` holds captured terminal screens,
`src/contracts.rs` and `src/owed.rs` hold diff fixtures, `src/workflow/` holds hand-aligned JSON.
The distinction is not the run of spaces — both populations have those. It is WHERE the run sits:

  * a collapsed continuation sits where a LINE WRAPPED, so there is a full line of prose in front
    of it. Measured over every string literal in the workspace: each of the fifteen collapses runs
    to at least 14 spaces, carries at least 72 characters of lead, and sits on a line of 16 words
    or more.
  * deliberate alignment sits after a LABEL, so there is almost nothing in front of it. The widest
    such lead in this tree is 15 characters (`src/fleet/containers.rs:149`, a python fixture's `config = {}`),
    and the wordiest such line holds 8 (`src/workflow/file.rs:213`, a JSON row).

So the rule is three measurements with an order of magnitude between the two populations, not a
guess about punctuation, and `--show` prints the margin so a threshold can be re-derived rather
than believed. Line comments and doc comments are not read at all: prose there is prose.

`#[cfg(test)]` is deliberately NOT cut, unlike the other Rust-reading gates. Eleven of the fifteen
collapses are `assert!` messages inside test modules — text a person reads at 2 a.m. when a build
is red — and a gate that cut them would have found four.

REFUSES TO RUN RATHER THAN PASS QUIETLY. CLAUDE.md's leak check answered `0` beside 195 matching
processes because its list had gone stale, and a check that cannot fail is worse than no check. So
this one derives its file list from the tree instead of carrying one, and stops with exit code 2 —
not 0 — when it derives no Rust files or reads no string literals out of them, which is what a
broken reader looks like from the inside. It also runs `self_check()` on every invocation, over
fixtures taken from the real sites: a collapse it must catch, the SAME sentence written with a
working `\` that it must not, and one line each of the alignment shapes above.

  python3 tools/continuation-check.py           check
  python3 tools/continuation-check.py --show    every run of spaces considered, and the margins
"""

import os
import re
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import rustcut  # noqa: E402 — the one Rust reader every gate shares, self-checked at import

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))

# Directories with no Rust of ours in them. `upstream/` is a submodule of somebody else's code.
SKIP_DIRS = {".git", "target", ".target", "node_modules", "upstream"}

# The three measurements, each sitting in the gap between the two populations described above, and
# each proved load-bearing on every run by `self_check`. `--show` reprints the gap from the tree, so
# a threshold can be re-derived rather than believed.
MIN_RUN = 8  # collapses: >= 14   widest alignment clearing the other two: 4
MIN_LEAD_CHARS = 40  # collapses: >= 72   longest lead clearing the other two: 15
MIN_LINE_WORDS = 12  # collapses: >= 16   wordiest line clearing the other two: 8
# A fourth was tried and dropped: at least five spaces of lead. Every collapse has eleven or more,
# so it looked like a measurement — but NO run anywhere in the workspace was held back by it alone,
# and a condition nothing depends on reads as though it were doing work. That is the trap
# `self_check`'s `NEAR_MISSES` now closes for the three above.

# The escape hatch, on the offending line or the one above it, with a reason after the colon.
# There are none in the tree today. An unused one fails the build, on the same bargain as every
# other allow-list here: a permission nobody prunes is a permission nobody granted.
MARKER = re.compile(r"//\s*continuation-ok:\s*(?P<reason>\S.*)$")
MARKER_BARE = re.compile(r"//\s*continuation-ok:\s*$")

# `"…"`, `b"…"`, `r"…"`, `br##"…"##` — the opening of any string literal `rustcut.skip_token`
# stepped over. A `//` comment, a `/* */` comment and a char literal do not match, which is how
# they stay unread.
OPEN = re.compile(r'(?P<b>b?)(?P<r>r?)(?P<hashes>#*)"')

RUN = re.compile(r" {2,}")


def literals(text):
    """(body_start, body_end, is_raw) for every string literal in `text`.

    Built on `rustcut.skip_token` rather than on a fourth private tokenizer: CONTRIBUTING says the
    Rust readers are one reader, and this gets that module's self-check on every invocation of this
    gate for free.
    """
    i, n = 0, len(text)
    while i < n:
        past = rustcut.skip_token(text, i)
        if past is None or past <= i:
            i += 1
            continue
        m = OPEN.match(text, i)
        if m and m.end() <= past:
            close = 1 + len(m.group("hashes"))
            body_start, body_end = m.end(), past - close
            if body_end >= body_start:
                yield body_start, body_end, bool(m.group("r"))
        i = past


SIMPLE_ESCAPES = {"n": "\n", "r": "\r", "t": "\t", "\\": "\\", "0": "\0", "'": "'", '"': '"'}


def decode(raw, is_raw):
    """(text, offsets) — the literal as rustc sees it, and where each character came from.

    The `\\`-newline case is the one that matters: rustc drops the backslash, the newline AND the
    leading whitespace of the next line, which is exactly what the heredoc stopped happening. If
    this stopped stripping it, every correctly continued literal in the tree would be reported as a
    collapse — `self_check` plants one to make sure it does not.
    """
    if is_raw:
        return raw, list(range(len(raw)))
    text, offsets, i, n = [], [], 0, len(raw)
    while i < n:
        if raw[i] != "\\":
            text.append(raw[i])
            offsets.append(i)
            i += 1
            continue
        nxt = raw[i + 1] if i + 1 < n else ""
        if nxt == "\n":
            i += 2
            while i < n and raw[i] in " \t\r\n":
                i += 1
            continue
        if nxt in SIMPLE_ESCAPES:
            text.append(SIMPLE_ESCAPES[nxt])
            offsets.append(i)
            i += 2
            continue
        if nxt == "x":
            text.append(chr(int(raw[i + 2 : i + 4], 16)) if raw[i + 2 : i + 4].strip() else "?")
            offsets.append(i)
            i += 4
            continue
        if nxt == "u":
            m = re.match(r"\\u\{([0-9a-fA-F_]+)\}", raw[i:])
            if m:
                text.append(chr(int(m.group(1).replace("_", ""), 16)))
                offsets.append(i)
                i += m.end()
                continue
        text.append(nxt)
        offsets.append(i)
        i += 2
    return "".join(text), offsets


def runs(text):
    """Every run of >= 2 spaces inside one Rust source text, as a dict per run.

    Reported whether or not it is a finding, so `--show` can print what was considered and the
    thresholds can be re-derived from the tree rather than trusted.
    """
    lines, ln = [], 1
    for ch in text:
        lines.append(ln)
        if ch == "\n":
            ln += 1
    lines.append(ln)
    out = []
    for body_start, body_end, is_raw in literals(text):
        decoded, offsets = decode(text[body_start:body_end], is_raw)
        for m in RUN.finditer(decoded):
            a, b = m.start(), m.end()
            # At either edge of the literal, or against a newline in it, a run of spaces is
            # indentation — of a shell script, a JSON fixture, a captured screen. Legitimate.
            if a == 0 or b == len(decoded):
                continue
            if decoded[a - 1] == "\n" or decoded[b] == "\n":
                continue
            lead = decoded[:a].rsplit("\n", 1)[-1]
            rest = decoded[b:].split("\n", 1)[0]
            if not rest.strip():
                continue
            offset = body_start + (offsets[a] if a < len(offsets) else 0)
            out.append(
                {
                    "line": lines[min(offset, len(lines) - 1)],
                    "spaces": b - a,
                    "lead_chars": len(lead),
                    "lead_spaces": lead.count(" "),
                    "words": len(re.sub(r" +", " ", lead + " " + rest).split()),
                    "lead": lead,
                    "rest": rest,
                }
            )
    return out


def is_collapse(run):
    """A run of spaces that a `\\` used to hold apart, by the three measurements above."""
    return (
        run["spaces"] >= MIN_RUN
        and run["lead_chars"] >= MIN_LEAD_CHARS
        and run["words"] >= MIN_LINE_WORDS
    )


def rust_files():
    """Every `.rs` file in the tree, walked rather than listed.

    Walked and not `git ls-files`, deliberately: a file written and not yet staged is the one most
    likely to carry a fresh collapse, and `residue-check.py`'s reader would be blind to exactly
    that (CONTRIBUTING says so about itself).
    """
    found = []
    for base, dirs, files in os.walk(ROOT):
        dirs[:] = sorted(d for d in dirs if d not in SKIP_DIRS and not d.startswith("."))
        for f in sorted(files):
            if f.endswith(".rs"):
                found.append(os.path.join(base, f))
    return found


def excused(source_lines, line):
    """(reason, marker_line) if this line carries a `// continuation-ok:` marker, else None."""
    for n in (line, line - 1):
        if 1 <= n <= len(source_lines):
            m = MARKER.search(source_lines[n - 1])
            if m:
                return m.group("reason").strip(), n
    return None


def bare_markers(source_lines):
    """Line numbers carrying `// continuation-ok:` with no reason after it."""
    return [n for n, text in enumerate(source_lines, 1) if MARKER_BARE.search(text)]


def marker_lines(source_lines):
    """Line numbers carrying a `// continuation-ok:` marker of any shape."""
    return [n for n, text in enumerate(source_lines, 1) if "continuation-ok:" in text]


def refuse(message):
    """Stop with exit code 2 — "I could not check", which is neither pass nor fail.

    Separated from the 1 a finding exits with, so that a gate whose reader has broken cannot be
    mistaken for a gate that looked and found nothing. Every caller's message says what it could
    not do; none of them says a tree is clean.
    """
    print(message, file=sys.stderr)
    sys.exit(2)


# --------------------------------------------------------------------------------------------
# The self-check, run on every invocation. Every fixture below is copied from a real site, and the
# fixture lines are cited in the assertions so a fixture that moves fails loudly rather than
# quietly matching nothing.
#
# Two of them are the ones that would otherwise go unnoticed:
#
#   * `still_continued` is the SAME sentence as the collapse, written with a working `\`, and the
#     assertion is that both DECODE TO THE SAME TEXT. Asserting only that it produces no finding is
#     an assertion that cannot fail — delete the `\`-newline branch of `decode` and the continuation
#     decodes to a newline, which this gate excludes as indentation, so the fixture stays silent
#     beside a broken decoder. That sabotage passed, once, before the comparison replaced it.
#   * `near_misses` holds three real lines, each held back by exactly ONE of the three thresholds.
#     Every threshold is then proved load-bearing on every run: relax it and its line fires. A
#     fourth threshold (five spaces before the run) was dropped rather than kept, because no run
#     anywhere in the workspace was held back by it alone and a condition nothing depends on reads
#     as though it were doing work.
# --------------------------------------------------------------------------------------------
SELF_CHECK = r"""
fn collapsed() -> &'static str {
    "the largest boxes are {named} — `skein stop <box>` keeps its checkout, branch and                  conversation, or clear its build output in place"
}

fn still_continued() -> &'static str {
    "the largest boxes are {named} — `skein stop <box>` keeps its checkout, branch and \
     conversation, or clear its build output in place"
}

// "a comment that keeps its checkout, branch and                  conversation is prose, and this
// gate does not read it"
fn alignment() {
    println!("{OK} registry      {n} repos, {b} boxes {DIM}(~/.skein/registry.json){RESET}");
    println!(
        "usage:\n  \
         skein repos           list registered repos\n  \
         skein remove <id>     unregister a repo (files left on disk)\n"
    );
    let _screen = "  ? for shortcuts                                          100% context left";
}

fn near_misses() {
    let _run = "  ◯ general-purpose  Screen grammar vs real fleet panes    4m 48s · ↓ 91.8k tokens";
    let _lead = "    config = {}          # no config at all is the normal case, not a problem";
    let _words = "              { \"when\": [\"checks:pending\"],                 \"do\": \"wait:CI is running\" },";
}
"""

# Fixture lines, derived from the fixture rather than counted by hand — a hardcoded line number
# silently stops matching when a fixture moves, and every assertion below is about a line.
def fixture_line(needle):
    for n, text in enumerate(SELF_CHECK.split("\n"), 1):
        if needle in text:
            return n
    refuse(
        f"continuation-check: its own fixture no longer contains {needle!r}, so the assertion that "
        "cites it would be checking a line that is not there."
    )


COLLAPSE_LINE = fixture_line("and                  conversation")
QUIET_LINES = {
    fixture_line("branch and \\"): "the same sentence written with a working `\\`",
    fixture_line("// \"a comment"): "a line comment — comments are prose and are not read",
    fixture_line("{OK} registry"): "a `skein doctor` column, aligned on purpose",
    fixture_line("skein repos "): "the `skein help` usage table, aligned on purpose",
    fixture_line("? for shortcuts"): "a captured terminal screen",
}
# line -> (threshold name, a value that lets it through, where the line came from)
NEAR_MISSES = {
    fixture_line("let _run ="): (
        "MIN_RUN", 2,
        "src/signals.rs:1576, a captured subagent row: 4 spaces, 55 characters of lead, 14 words",
    ),
    fixture_line("let _lead ="): (
        "MIN_LEAD_CHARS", 10,
        "src/fleet/containers.rs:149, a python fixture's aligned comment: 10 spaces, 15 characters of lead, "
        "15 words",
    ),
    fixture_line("let _words ="): (
        "MIN_LINE_WORDS", 5,
        "src/workflow/file.rs:213, a hand-aligned JSON row: 17 spaces, 43 characters of lead, "
        "8 words",
    ),
}


def fires_at(text, **thresholds):
    """The fixture lines reported as collapses, with the named thresholds temporarily replaced."""
    saved = {name: globals()[name] for name in thresholds}
    globals().update(thresholds)
    try:
        return {run["line"] for run in runs(text) if is_collapse(run)}
    finally:
        globals().update(saved)


def self_check():
    seen = list(literals(SELF_CHECK))
    if len(seen) < 7:
        refuse(
            "continuation-check: its own reader is broken — it found "
            f"{len(seen)} string literals in a fixture that has at least seven, so it would report "
            "a clean tree because it read nothing."
        )
    found = {r["line"]: r for r in runs(SELF_CHECK) if is_collapse(r)}
    if COLLAPSE_LINE not in found:
        refuse(
            f"continuation-check: its own check is broken — the planted collapse at fixture line "
            f"{COLLAPSE_LINE}, the exact text of `health::disk_verdict`, was not reported. A gate "
            "that cannot fail is worse than no gate (CLAUDE.md, SKEIN-647)."
        )
    if found[COLLAPSE_LINE]["spaces"] != 18:
        refuse(
            "continuation-check: its own check is broken — the planted collapse was measured at "
            f"{found[COLLAPSE_LINE]['spaces']} spaces rather than 18, so its report would send a "
            "reader to the wrong place."
        )

    # The two fixtures are the same sentence, one collapsed and one still continued, so the
    # assertion is that `decode` turns them into the same text. See the note above for the sabotage
    # that made this replace "the continued one produces no finding".
    bodies = [decode(SELF_CHECK[a:b], raw)[0] for a, b, raw in literals(SELF_CHECK)]
    collapsed = next(t for t in bodies if "and                  conversation" in t)
    continued = next(t for t in bodies if t.startswith("the largest boxes") and t is not collapsed)
    if continued != re.sub(r" {2,}", " ", collapsed):
        refuse(
            "continuation-check: its own decoder is broken — a literal written with a working "
            "`\\` continuation no longer decodes to the same sentence as the collapsed one.\n"
            f"                    continued: {continued!r}\n"
            f"                    collapsed: {re.sub(r' {2,}', ' ', collapsed)!r}\n"
            "                    rule: rustc drops the backslash, the newline AND the next line's "
            "indentation. A decoder that keeps any\n"
            "                          of them either floods the tree with false findings or hides "
            "real ones behind a fake newline."
        )

    for line, what in QUIET_LINES.items():
        for reported in range(line, line + 4):
            if reported in found:
                refuse(
                    "continuation-check: its own check is broken — it reported fixture line "
                    f"{reported}, which is {what}. A gate that reddens deliberate alignment is a "
                    "gate somebody turns off."
                )

    for line, (name, loosened, where) in NEAR_MISSES.items():
        if line in found:
            refuse(
                f"continuation-check: its own check is broken — it reported fixture line {line} "
                f"({where}), which every threshold but `{name}` already lets through. It is "
                "deliberate alignment and must stay quiet."
            )
        if line not in fires_at(SELF_CHECK, **{name: loosened}):
            refuse(
                f"continuation-check: `{name}` is not load-bearing — fixture line {line} "
                f"({where}) stays quiet even with `{name}` relaxed to {loosened}, so the threshold "
                "separates nothing and reads as though it were doing work. Re-derive it with "
                "`--show` or drop it."
            )

    excuse_fixture = ["x", '    "a b c d e f g h i j k l m         n o p"  // continuation-ok: because']
    if not excused(excuse_fixture, 2):
        refuse(
            "continuation-check: its own check is broken — a `// continuation-ok: <reason>` marker "
            "on the offending line no longer excuses it, so the only escape hatch is gone and the "
            "next person deletes the gate instead."
        )
    if bare_markers(["x  // continuation-ok:"]) != [1]:
        refuse(
            "continuation-check: its own check is broken — a `// continuation-ok:` with no reason "
            "after it is accepted, which is an exception nobody wrote down."
        )


def main():
    self_check()
    files = rust_files()
    if not files:
        print(
            "continuation-check: no `.rs` file anywhere under "
            f"{ROOT} — this gate derives its file list from the tree and there is nothing to "
            "derive.\n"
            "                    rule: refusing is the answer, because scanning nothing and "
            "printing `0 findings` is indistinguishable from a clean tree (CLAUDE.md, SKEIN-647)."
        )
        return 2

    all_runs, findings, literal_count, excuses, markers = [], [], 0, [], []
    for path in files:
        text = open(path, encoding="utf-8").read()
        source_lines = text.split("\n")
        literal_count += sum(1 for _ in literals(text))
        rel = os.path.relpath(path, ROOT)
        used_markers = set()
        for run in runs(text):
            all_runs.append((rel, run))
            if not is_collapse(run):
                continue
            excuse = excused(source_lines, run["line"])
            if excuse:
                reason, marker_line = excuse
                used_markers.add(marker_line)
                excuses.append((rel, run["line"], reason))
                continue
            findings.append((rel, run))
        for n in bare_markers(source_lines):
            findings.append((rel, {"line": n, "bare_marker": True}))
        for n in marker_lines(source_lines):
            if n not in used_markers and n not in bare_markers(source_lines):
                markers.append((rel, n))

    if literal_count == 0:
        print(
            f"continuation-check: read {len(files)} Rust files and found no string literal in any "
            "of them.\n"
            "                    rule: that is what a broken reader looks like from the inside, "
            "not a clean tree. Refusing rather than passing (exit 2)."
        )
        return 2

    if "--show" in sys.argv:
        for rel, run in sorted(all_runs, key=lambda r: (r[0], r[1]["line"])):
            mark = "COLLAPSE" if is_collapse(run) else "        "
            print(
                f"{mark} {rel}:{run['line']} spaces={run['spaces']} lead={run['lead_chars']}c/"
                f"{run['lead_spaces']}sp words={run['words']}"
            )
        print()
        collapses = [r for _, r in all_runs if is_collapse(r)]
        others = [r for _, r in all_runs if not is_collapse(r)]
        for name, key, floor in (
            ("spaces", "spaces", MIN_RUN),
            ("lead chars", "lead_chars", MIN_LEAD_CHARS),
            ("line words", "words", MIN_LINE_WORDS),
        ):
            near = [
                r[key]
                for r in others
                if all(
                    r[k] >= f
                    for k, f in (
                        ("spaces", MIN_RUN),
                        ("lead_chars", MIN_LEAD_CHARS),
                        ("words", MIN_LINE_WORDS),
                    )
                    if k != key
                )
            ]
            low = min((r[key] for r in collapses), default=None)
            print(
                f"{name:12} threshold {floor:3}  lowest collapse {low}  "
                f"highest run that clears every other rule {max(near, default=None)}"
            )
        return 0

    for rel, run in findings:
        if run.get("bare_marker"):
            print(
                f"continuation-check: {rel}:{run['line']} carries `// continuation-ok:` with no "
                "reason after it\n"
                "                    rule: an exception nobody wrote down is an exception nobody "
                "granted — say why, or take the marker off\n"
            )
            continue
        gap = "·" * run["spaces"]
        lead = run["lead"][-56:]
        rest = run["rest"][:56]
        print(
            f"continuation-check: {rel}:{run['line']} — {run['spaces']} spaces inside a string "
            "literal, mid-sentence\n"
            f"                    …{lead}{gap}{rest}…\n"
            "                    rule: a `\\` at the end of that line was eaten by the heredoc that "
            "wrote it, so the\n"
            "                          continuation line's indentation stayed in the literal and a "
            "reader sees the gap.\n"
            "                          Put the `\\` back and re-indent; write the heredoc as "
            "<<'EOF' (quoted) or\n"
            "                          escape it as `\\\\` so the shell stops eating it. "
            "CONTRIBUTING.md, 'Before you change anything'.\n"
        )
    for rel, line in markers:
        print(
            f"continuation-check: {rel}:{line} carries a `// continuation-ok:` marker and the line "
            "it excuses is clean\n"
            "                    rule: an allow-list nobody prunes is a permission nobody granted "
            "— drop the marker\n"
        )

    problems = len(findings) + len(markers)
    if problems:
        print(f"{problems} problem(s) across {len(files)} Rust files.")
        return 1
    excused_note = f", {len(excuses)} excused" if excuses else ""
    print(
        f"no collapsed `\\` continuation: {len(all_runs)} runs of spaces inside "
        f"{literal_count} string literals across {len(files)} Rust files{excused_note}"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
