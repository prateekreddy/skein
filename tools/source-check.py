#!/usr/bin/env python3
"""The Source law, made checkable: nothing reaches anything except through a Source.

`docs/architecture.md` §2.3 names four Sources — `enter`, `socket`, `file`, `http` — and says the
law becomes enforceable once they exist. A law nothing checks is a paragraph. This is the check,
and it is the same shape as `tools/module-check.py`: an allow-list of where each Source is spelled
today, generated from the code and then reviewed, so that a NEW way of reaching something is a line
in a diff rather than a call nobody looked at.

What it does not claim: that today's spread is right. `enter` is spelled in six files and belongs
in one. The point is that the spread cannot quietly get wider while the rewrite is under way.

Test code is cut before matching, for the same reason module-check cuts it: a fixture that spells
`nsenter` in an assertion is describing the code, not reaching anything. The cut comes from
`tools/rustcut.py`, the one cutter all three text gates share — it is brace-matched rather than
"everything after the marker" because the cheap version stops reading at the test module and every
item below it becomes invisible, and it ends a brace-less `#[cfg(test)] const` at its `;` rather
than at the next `{`, which used to take the production function after it (WTS-9).

Both crates are read: `src/` and `warden/src/`. The warden runs the privileged commands, so a
checker that stopped at skein would be silent about the reaches that matter most.

  python3 tools/source-check.py           check
  python3 tools/source-check.py --update  rewrite the allow-list from the code
  python3 tools/source-check.py --show    print where each Source is spelled
"""

import os, re, sys, collections, tomllib

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import rustcut  # noqa: E402 — the one cutter every gate shares, self-checked at import

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SRC = os.path.join(ROOT, "src")
# The warden is a separate crate (architecture §14, "separate binary"), and it is the ONE component
# whose reaches matter most: it is the process on the host that runs `sbx create` and `sbx rm`. A
# checker that stopped at `src/` would go quiet exactly there, which is the condition it exists to
# end. Its units are prefixed so `warden/doer` and `doer` can never be confused for each other.
WARDEN = os.path.join(ROOT, "warden", "src")
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
    #
    # `\("sbx"` rather than `Command::new("sbx")`, because skein does not spawn it that way. It goes
    # through `run_capture_for`, `run_attached_env` and friends, and the narrower pattern saw NONE of
    # them: `fleet` spawns `sbx` six times — including the fleet create and the resize's destroy, the
    # two most privileged calls in the system — and the checker reported it as reaching nothing. The
    # program name as the first argument of a call is what "spawning it" looks like here.
    #
    # `\(\s*` and not `\(`, because the same reach goes invisible when rustfmt puts the program name
    # on its own line. Seen: SKEIN-104 split the login into a `(program, argv)` tuple, `fleet`'s
    # count silently fell from two to one, and the call it stopped counting was the one that still
    # runs `sbx`. A pattern that depends on formatting is a law that a reformat can repeal.
    "sbx": [r'\(\s*"sbx"'],
}


def units():
    """Every unit as (name, [paths]), from the one cutter — `src/<name>.rs` AND `src/<name>/**`.

    The local version enumerated with `os.listdir(SRC)` and yielded one path each, so the day
    `src/fleet.rs` becomes `src/fleet/` the unit would have vanished from this gate and every
    reach inside it with it — the allow-list would then have named a unit that no longer reaches
    anything, which reads as an improvement.
    """
    return rustcut.units(SRC, WARDEN)


def shipped_code(text):
    """The part of a unit's text that runs in production: no `#[cfg(test)]`, no comments."""
    return rustcut.uncommented(rustcut.split_tests(text)[0])


def read_reaches():
    """{source: Counter(unit -> hits)} over non-test, non-comment code."""
    found = {name: collections.Counter() for name in SPELLINGS}
    for unit, paths in units():
        # `source.rs` is where the Sources are DESCRIBED, and it reaches nothing. It names `nsenter`
        # in a string — the "reaches" column of §2.3's table — and counting that would put the
        # taxonomy on the list of things that cross into boxes.
        if unit == "source":
            continue
        body = shipped_code(rustcut.read_unit(paths))
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


# The cut this gate depends on, held as a fixture and run on every invocation. `rustcut`'s own
# self-check pins the cutter; this one pins the way THIS gate uses it, which is the pair of
# opposite errors that both end in a wrong count:
#
#   * a reach spelled in a fixture counted as production — the allow-list gets wider than the code;
#   * a reach in production code that the cut swallowed — the allow-list looks clean because part
#     of the crate is invisible. That is WTS-9: `#[cfg(test)] const TMUX_COMMAND_CEILING` at
#     `src/fleet.rs:992` is brace-less, and cutting to "the next `{`" took `fn detached_script_path`
#     (`:1012`) with it. That function spells no Source today, so the count was right by luck.
SELF_CHECK = r'''#[cfg(test)]
const TEST_CEILING: usize = 4;

fn detached_script_path() -> String {
    let _ = std::process::Command::new("curl");
    String::new()
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_fixture_that_merely_NAMES_a_source_reaches_nothing() {
        let fixture = format!("nsenter --target {{}} --mount", 1);
        let _ = std::process::Command::new("curl");
    }
}

fn after_the_tests() {
    let _ = std::process::Command::new("tmux");
}
'''


def self_check():
    body = shipped_code(SELF_CHECK)
    if 'Command::new("curl")' not in body:
        raise SystemExit(
            "source-check: its own cut is broken — the production item after a brace-less "
            "`#[cfg(test)] const` was deleted, so a reach inside it is invisible and this gate "
            "reports a clean tree for the wrong reason (WTS-9)."
        )
    if "nsenter" in body:
        raise SystemExit(
            "source-check: its own cut is broken — a `#[cfg(test)]` fixture that merely NAMES a "
            "Source was counted as a production reach, which is how the allow-list gets wider "
            "than the code (SKEIN-412)."
        )
    if 'Command::new("tmux")' not in body:
        raise SystemExit(
            "source-check: its own cut is broken — the test module swallowed the code BELOW it, "
            "so part of the crate is invisible."
        )
    if len(body.split("\n")) != len(SELF_CHECK.split("\n")):
        raise SystemExit(
            "source-check: its own cut is broken — the cut moved line numbers, so nothing this "
            "gate reports can be located."
        )


def main():
    self_check()
    found = read_reaches()
    if "--show" in sys.argv:
        for source, hits in found.items():
            where = ", ".join(f"{u}({n})" for u, n in sorted(hits.items())) or "nowhere"
            print(f"{source:8} {where}")
        return 0
    if "--update" in sys.argv:
        open(SPEC, "w", encoding="utf-8").write(render(found))
        print(f"wrote {os.path.relpath(SPEC, ROOT)}")
        print(
            "  NOTE: --update rewrites the file from the code and keeps only the unit lists. Every\n"
            "  per-entry comment — the review that says WHY a unit is allowed to reach — is dropped.\n"
            "  Read the diff before keeping it: if the lists are unchanged, the only thing --update\n"
            "  did was delete the reasoning, and the right move is to discard the rewrite."
        )
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
