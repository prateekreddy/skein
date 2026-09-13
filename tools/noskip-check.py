#!/usr/bin/env python3
"""The no-skip run, and a verdict on the part of it this machine is actually able to host.

`$SKEIN_TESTS_NO_SKIP` turns every `common::skip` into a named panic, so a green run under it is a
proof that nothing was skipped — see `tests/common/mod.rs::skip`. CI has set it since SKEIN-558 and
written what it found into the job summary, and the step could not fail the build. The argument for
that was written beside it and it was not a bad one:

    The other honest option was a no-skip run scoped to the test binaries whose requirements this
    image genuinely meets. That needs a hand-kept list of what a runner image happens to provide,
    sitting beside `common::REQUIREMENTS`, which is derived — and a hand-kept list of somebody
    else's platform is exactly the thing that goes stale without saying so.

WHAT THAT ARGUMENT MISSES, WHICH IS WHY THIS FILE EXISTS. A hand-kept list is not the only
alternative to an unscoped run. Whether a runner has `tmux` is not a fact anybody has to write
down: it is a **question you ask the machine you are standing on**, at the moment you need the
answer. The requirements are already declared per binary in `common::REQUIREMENTS` and held against
the code in both directions by `tests/platform_gates.rs`. So both halves of the scope are derived —
the requirement from the source, its presence from a probe — and neither can go stale without
saying so. Nothing here is written down about any platform.

The consequence of the old step is what SKEIN-881 is about. Two `upstream/sync` drift guards skipped
on every run for as long as anyone had been running them; `$SKEIN_TESTS_NO_SKIP` DID find them, the
summary DID list them, and because the step could not fail they sat as expected failures until
somebody was sent to look (SKEIN-826). **Two failures everyone has learned to expect is how a switch
stops being run at all** (SKEIN-647, one surface further on).

WHAT IT DECIDES, IN ONE SENTENCE. A refusal coming out of a test binary that declares nothing this
machine lacks is a build failure; a refusal out of a binary this machine cannot fully host is
reported and nothing more.

WHERE A REFUSAL COMES FROM, AND WHY THAT IS NOT A GUESS. `common::skip` and `testutil::skip` are
`#[track_caller]`, so the panic names the GUARD's own file and line —
`SKIPPED at src/fleet.rs:<line>: no box cgroups on this machine to sample`. A site under `src/` is the
`--lib` binary (`common::LIB`), a site in `tests/<name>.rs` is the `<name>` binary, and anything
else is attributed to nothing and therefore blocks nothing — said out loud rather than assumed.

THE ONE THING THAT IS DECLARED HERE, AND WHY IT HAS TO BE. `REQUIREMENTS` says what TOOLS a binary
needs, and a handful of guards in this tree ask the machine something a tool name cannot express:
whether `/sys/fs/cgroup/skein` exists, whether this run is root, whether the login shell keeps PATH,
whether there is a `/proc`. Those are legitimate refusals on a machine that has every declared tool,
so with no declaration they would be permanent reds — and a permanent red is the failure mode at the
top of this file, reintroduced by the thing meant to fix it. `ENVIRONMENTAL` below is that
declaration: one entry per guard, each with its reason, each **checked against the code** — the test
has to still exist and still skip, or this refuses to run. An entry cannot quietly describe a tree
that has moved on, which is the property a bare allow-list lacks. It is a statement about guards in
THIS repository, not about somebody else's runner image, and it is the list that should not exist:
see the note under `ENVIRONMENTAL` for what would delete it.

REFUSES TO RUN RATHER THAN PASS QUIETLY, in every derivation it makes:

  * the requirements come from `tests/common/mod.rs`. Deriving no entries, or one that declares no
    tools, means the reader broke — exit 2, not 0, because a scope derived from nothing would mark
    every binary hostable and turn every legitimate skip into a red.
  * the PROBES are the ones that file uses, because they are READ OUT OF IT. Which declared names
    are capabilities rather than PATH lookups, and the command each capability's probe runs, are
    both derived from `tests/common/mod.rs` — see `capabilities()`. Nothing about `bwrap` or
    `chromium` is written down here. A probe this cannot read, or one naming a tool
    `REQUIREMENTS` does not declare, means this would be answering a different question than the
    guards do — exit 2.
  * the environmental declaration is checked against the source, as above — exit 2.
  * and the RUN has to have happened. `^test result:` is the positive evidence, one line per test
    binary, and with none of them this reports **Unknown** and fails rather than reading an
    all-clear into a silence (SKEIN-826). The two `tools/gates.sh` exit codes that are documented as
    neither-a-pass-nor-a-failure — 3, the tree moved under the run, and 4, the logs could not be
    written — are reported as Unknown and do NOT fail, because that is that script's own contract.

  python3 tools/noskip-check.py            derive, run, judge — what CI calls
  python3 tools/noskip-check.py --scope    print the derivation and stop, running nothing
"""

import os
import re
import shlex
import subprocess
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import rustcut  # noqa: E402 — the one cutter every gate shares, self-checked at import

COMMON = "tests/common/mod.rs"
USAGE = (
    "usage: python3 tools/noskip-check.py [--scope] [<worktree>]\n"
    "       --scope prints the derivation and runs nothing"
)

# ---------------------------------------------------------------------------------------------
# The one declaration in this file
# ---------------------------------------------------------------------------------------------

# Guards that may refuse on a machine holding every tool their binary declares, with the reason
# `REQUIREMENTS` cannot express it. (file, test function, why.)
#
# **What would delete this table.** `REQUIREMENTS` holds tool names, probed with `command -v` —
# except where the name is a CAPABILITY with a probe of its own in `tests/common/mod.rs`, which
# `bwrap` and `chromium` both are. Every entry here is another capability of that shape that has no
# name yet. Give them one — `cgroups`, `not-root`, `login-shell-path`, `proc` — write the nullary
# `pub fn <tool>_<verb>() -> bool` beside `bwrap_works` and `chromium_ready`, and each row here
# becomes an ordinary requirement that scopes the binary instead of excusing a guard inside it.
# **Nothing here or in `capabilities()` needs teaching the new name**: the probe and the command it
# runs are read out of that file. That is strictly better than this table, because it would also
# make the refusal FAIL on a machine that does have the capability.
ENVIRONMENTAL = [
    (
        "src/fleet.rs",
        "the_load_script_and_its_parser_agree_on_real_cgroups",
        "runs the box-load script against real cgroups, which exist inside a fleet and on no "
        "ordinary runner: the guard asks whether /sys/fs/cgroup/skein is a directory",
    ),
    (
        "src/fleet.rs",
        "the_login_terminal_brings_the_same_scratch_directory_the_model_call_does",
        "a login shell that rewrites PATH never reaches the stub the test plants, and which shell "
        "that is is a property of the machine rather than of this tree",
    ),
    (
        "src/fleet.rs",
        "a_places_directory_that_cannot_be_listed_fails_the_census",
        "makes a directory unreadable and asks the census to fail on it — root ignores the mode "
        "bits, so the fixture is not unreadable and the test would prove nothing",
    ),
    (
        "tests/git_write_request.rs",
        "a_box_cannot_ask_for_write_access_in_another_boxs_name",
        "the same mode-bit fixture: `running_as_root()` is a uid question, not a tool question",
    ),
    (
        "tests/substrate_request.rs",
        "a_box_cannot_file_a_request_in_another_boxs_name",
        "the same mode-bit fixture, for the request path",
    ),
    (
        "tests/usage.rs",
        "the_reader_reproduces_an_independent_tally_of_the_same_transcripts",
        "compares the reader against an oracle taken from a corpus of REAL transcripts, named by "
        "$SKEIN_USAGE_TRANSCRIPT_ROOT — somebody's own box, which is not a tool and cannot be "
        "installed on a runner",
    ),
    (
        "tests/server.rs",
        "an_uploaded_body_is_on_the_crossings_stdin_and_not_in_its_cmdline",
        "reads /proc/<pid>/cmdline, which is Linux's — the guard asks whether there is a /proc at "
        "all, and the suite is meant to stay runnable where there is not",
    ),
]


class Refusal(Exception):
    """This gate cannot answer the question it exists to answer. Exit 2, never 0."""


# ---------------------------------------------------------------------------------------------
# Deriving the scope: what each binary needs, and what this machine has
# ---------------------------------------------------------------------------------------------


def read(root, rel):
    path = os.path.join(root, rel)
    try:
        with open(path, encoding="utf-8") as fh:
            return fh.read()
    except OSError as exc:
        raise Refusal(f"cannot read {rel}: {exc}") from exc


def requirements(root):
    """`(lib_binary_name, {binary: [tool, ...]})`, read out of `tests/common/mod.rs`.

    Comments are cut first — the list carries several, and one of them names tools in prose.
    """
    src = rustcut.uncommented(read(root, COMMON))

    lib = re.search(r'pub const LIB:\s*&str\s*=\s*"([^"]+)"', src)
    if not lib:
        raise Refusal(
            f"{COMMON} no longer defines `LIB`, so the name of the library's test binary — the "
            "one thing here that is not a tests/*.rs — cannot be derived"
        )

    block = re.search(
        r"pub const REQUIREMENTS:[^=]*=\s*&\[(.*?)\n\];",
        src,
        re.S,
    )
    if not block:
        raise Refusal(
            f"{COMMON} no longer holds a `REQUIREMENTS` list this can read, so there is no scope "
            "to derive. Every binary would look hostable and every skip would become a red."
        )

    found = {}
    for name, tools in re.findall(
        r'\(\s*(LIB|"[A-Za-z0-9_]+")\s*,\s*&\[([^\]]*)\]', block.group(1)
    ):
        binary = lib.group(1) if name == "LIB" else name.strip('"')
        found[binary] = re.findall(r'"([^"]+)"', tools)

    if len(found) < 2:
        raise Refusal(
            f"read {len(found)} entries out of `REQUIREMENTS` in {COMMON}, which is not a list — "
            "the reader has stopped matching its shape"
        )
    for binary, tools in found.items():
        if not tools:
            raise Refusal(
                f"`REQUIREMENTS` declares `{binary}` needing nothing, which this reader cannot "
                "tell apart from a parse that lost the tools"
            )
    return lib.group(1), found


# A `pub fn` in `tests/common/mod.rs` that answers yes-or-no about this machine. The PARAMETER LIST
# is the whole discriminator and it is not a convention invented here: `have(tool: &str)` takes the
# name of the thing to look for because it asks one question about any name, while a capability
# probe takes NOTHING because it asks a fixed question about one capability. See `capabilities()`.
PROBE_FN = re.compile(r"(?m)^pub fn ([A-Za-z0-9_]+)\(([^)]*)\)\s*->\s*bool\s*\{")

# `\n`, `\t`, … inside a Rust string literal. A `\` before a newline is the CONTINUATION and is
# handled separately, because it eats the newline and the next line's indentation rather than
# standing for a character — which is how `chromium_ready`'s node script is written.
ESCAPES = {"n": "\n", "r": "\r", "t": "\t", "0": "\0", "\\": "\\", '"': '"', "'": "'"}


def rust_string(lit, where):
    """The VALUE of a Rust string literal, given its source text including the quotes."""
    if lit.startswith(("r", "br")):
        hashes = lit[lit.index('"') - 1 :].split('"')[0]
        return lit[lit.index('"') + 1 : len(lit) - 1 - len(hashes)]
    body = lit[2:-1] if lit.startswith("b") else lit[1:-1]
    out, i = [], 0
    while i < len(body):
        if body[i] != "\\":
            out.append(body[i])
            i += 1
            continue
        nxt = body[i + 1 : i + 2]
        if nxt == "\n":
            i += 2
            while i < len(body) and body[i] in " \t\r\n":
                i += 1
            continue
        if nxt in ESCAPES:
            out.append(ESCAPES[nxt])
            i += 2
            continue
        raise Refusal(
            f"{where} contains the escape `\\{nxt}`, which this reader cannot decode — it would "
            "then run a command that is not the one the guard runs"
        )
    return "".join(out)


def literals_between(text, start, end, where):
    """Every string literal in `text[start:end]`, in source order, decoded.

    Tokenised with `rustcut.skip_token` rather than matched with a regex, so a `"` inside a char
    literal or a raw string cannot open one.
    """
    out, i = [], start
    while i < end:
        past = rustcut.skip_token(text, i)
        if past is not None and past > i:
            tok = text[i:past]
            if '"' in tok[:3]:
                out.append(rust_string(tok, where))
            i = past
            continue
        i += 1
    return out


def closes(text, start, opening, closing, where):
    """Index of the `closing` matching the first `opening` at or after `start`."""
    try:
        i = text.index(opening, start)
    except ValueError as exc:
        raise Refusal(f"{where}: expected a `{opening}` and there is none") from exc
    depth, j, n = 0, i, len(text)
    while j < n:
        past = rustcut.skip_token(text, j)
        if past is not None and past > j:
            j = past
            continue
        if text[j] == opening:
            depth += 1
        elif text[j] == closing:
            depth -= 1
            if depth == 0:
                return i + 1, j
        j += 1
    raise Refusal(f"{where}: a `{opening}` opened at {i} and never closes")


def probe_command(root, fn, body):
    """`(argv, cwd)` — the command a capability probe's BODY runs, read out of that body.

    The alternative was to write the two commands down here beside the two names. That is the
    second hand-kept list this file's header argues against, one surface in: `bwrap --dev-bind`
    and the node script are facts about `tests/common/mod.rs`, and a copy of a fact is a thing
    that drifts from it. So the body is parsed — `Command::new`, `.arg`/`.args`, and the
    `.current_dir` that makes `chromium_ready` resolve Playwright from `tests/ui` and not from the
    repository root — and a body this cannot parse is a REFUSAL rather than a fallback to
    `command -v`, which would be exactly the under-blocking SKEIN-899 was about.
    """
    where = f"{COMMON}::{fn}"
    at = body.find("Command::new(")
    if at < 0:
        raise Refusal(
            f"{where} is a capability probe and does not run a `Command`, so this cannot ask the "
            "machine what that guard asks"
        )
    lo, hi = closes(body, at, "(", ")", where)
    program = literals_between(body, lo, hi, where)
    if len(program) != 1:
        raise Refusal(
            f"{where} does not name its program as a single string literal, so the command this "
            f"would run is a guess (read: {program})"
        )
    argv = [program[0]]
    for m in re.finditer(r"\.args?\(", body):
        lo, hi = closes(body, m.start(), "(", ")", where)
        args = literals_between(body, lo, hi, where)
        if not args:
            raise Refusal(
                f"{where} passes an argument this reader cannot read as a string literal — the "
                f"command it would run is not the command the guard runs: {body[m.start():hi]!r}"
            )
        argv += args

    cwd = root
    dir_at = body.find(".current_dir(")
    if dir_at >= 0:
        lo, hi = closes(body, dir_at, "(", ")", where)
        parts = literals_between(body, lo, hi, where)
        if not parts or parts[0] != "CARGO_MANIFEST_DIR":
            raise Refusal(
                f"{where} sets a working directory this reader cannot resolve to a path in the "
                f"worktree (read: {parts}). `chromium_ready` asks from `tests/ui` because that is "
                "where `node_modules` is, and asking from the root reports `not installed` on a "
                "machine that has the browser"
            )
        cwd = os.path.join(root, *parts[1:])
    return argv, cwd


def capabilities(root, tools):
    """`{tool: (argv, cwd)}` — which declared names are CAPABILITIES, derived, not written down.

    `REQUIREMENTS` is a list of names and most of them are tools on PATH, asked with `have()`,
    which is `command -v`. Two are not: `bwrap` installs cleanly on `ubuntu-24.04` and is then
    refused the user namespace it needs, and Playwright's `chromium` lives in a cache and is never
    on PATH at all. Probing either with `command -v` answers a different question from the one the
    suite's own guard asks — which left two tests dead on CI for 27 days (SKEIN-549) and then left
    the whole browser tier report-only on the runner that installs the browser on purpose
    (SKEIN-899).

    **So which names those are is read out of the guards, not listed here.** A capability probe is
    a `pub fn` in `tests/common/mod.rs` that returns `bool` and takes NO argument; it means the
    declared tool its own name begins with. `have(tool: &str)` takes the name because it asks about
    any name; `bwrap_works()` and `chromium_ready()` take nothing because each asks about one
    thing. That is one rule for every capability rather than one written-down fact per capability,
    and it is the difference between adding `chromium` here and not having to.

    **It refuses rather than guessing**, in every direction it can be wrong:

      * the probes that TAKE a name must be exactly `have`. It certainly exists, so failing to see
        it means `PROBE_FN` has stopped matching — and a derivation that silently finds no
        capabilities probes `chromium` with `command -v` and under-blocks in silence, which is the
        whole defect. This is the floor, and it is a function that has to be there rather than a
        count written down here. It fires in the other direction too: a capability probe that grows
        a parameter stops being attributable to one name, and reading it as a second `have` would
        lose the capability just as quietly.
      * a probe whose name begins with a tool `REQUIREMENTS` does not declare — a rename on one
        side only — is a refusal naming both sides, not a probe quietly attached to nothing.
      * two probes claiming one tool is a refusal: which of them the guards use is not this file's
        to decide.
      * and a body it cannot read is a refusal — see `probe_command`.
    """
    src = rustcut.uncommented(read(root, COMMON))
    found = list(PROBE_FN.finditer(src))
    asks_a_name = sorted(m.group(1) for m in found if m.group(2).strip())
    if asks_a_name != ["have"]:
        raise Refusal(
            f"the `pub fn … -> bool` in {COMMON} that TAKE an argument should be exactly `have`, "
            f"and they are {asks_a_name} (of {[m.group(1) for m in found]}). Either the reader has "
            "stopped matching — in which case every capability falls back to `command -v` and "
            "under-blocks in silence, which is what this derivation exists to end — or a probe has "
            "grown a parameter and is no longer a capability this can attribute to one name"
        )

    out = {}
    for m in found:
        if m.group(2).strip():
            continue
        fn = m.group(1)
        tool = fn.split("_")[0]
        if tool not in tools:
            raise Refusal(
                f"{COMMON} defines the capability probe `{fn}()`, whose name says it answers for "
                f"`{tool}`, and `REQUIREMENTS` declares no such tool (it declares {sorted(tools)}). "
                "One side of a rename has moved: whichever it was, this prober would now ask "
                "`command -v` about a name that is not on PATH by design"
            )
        if tool in out:
            raise Refusal(
                f"{COMMON} has more than one capability probe for `{tool}`, and which one the "
                "guards take is not this file's to decide"
            )
        lo = src.index("{", m.end() - 1)
        out[tool] = probe_command(root, fn, src[lo : rustcut.end_of_block(src, lo)])

    if "command -v {tool}" not in src:
        raise Refusal(
            f"{COMMON}'s `have()` no longer asks `command -v {{tool}}`, so this prober no longer "
            "asks what the guards ask about every name that is NOT a capability"
        )
    for tool in tools:
        if tool not in out and not re.fullmatch(r"[A-Za-z0-9_.+-]+", tool):
            raise Refusal(
                f"`REQUIREMENTS` names `{tool}`, which is not a tool name this can safely probe"
            )
    return out


def probe(tool, caps):
    """Is this capability here, asked of this machine, now — the way the suite's own guard asks."""
    if tool in caps:
        cmd, cwd = caps[tool]
    else:
        cmd, cwd = ["sh", "-c", f"command -v {tool} >/dev/null 2>&1"], None
    try:
        return (
            subprocess.run(
                cmd, cwd=cwd, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL
            ).returncode
            == 0
        )
    except OSError:
        return False


# ---------------------------------------------------------------------------------------------
# The environmental declaration, held against the code
# ---------------------------------------------------------------------------------------------


def fn_span(text, name):
    """`(first_line, last_line)` of `fn <name>`, 1-based and inclusive, or None.

    Found by INDENT, the way `tests/platform_gates.rs::lib_tests` finds one and for the same
    reason: `src/fleet.rs` embeds whole shell scripts in string literals, and a brace counter that
    does not tokenise Rust ran one span over 11,000 lines there.
    """
    lines = text.splitlines()
    for i, line in enumerate(lines):
        m = re.match(r"(\s*)(?:pub(?:\(\w+\))?\s+)?(?:async\s+)?fn\s+" + re.escape(name) + r"\s*\(", line)
        if not m:
            continue
        close = m.group(1) + "}"
        for j in range(i + 1, len(lines)):
            if lines[j] == close:
                return i + 1, j + 1
        return i + 1, len(lines)
    return None


def calls_skip(body):
    """Is `skip` *called* here — as opposed to `.skip(2)` or `.skip_while(…)`?

    The same two spellings and the same test as `tests/platform_gates.rs::calls_skip`.
    """
    for line in body.splitlines():
        for at in (m.start() for m in re.finditer(re.escape("skip("), line)):
            if at == 0 or not (line[at - 1] in "._" or line[at - 1].isalnum()):
                return True
    return False


def environmental_spans(root):
    """`{(file, first, last): why}` — and each entry proved against the source first."""
    spans = {}
    for path, name, why in ENVIRONMENTAL:
        if not why.strip():
            raise Refusal(f"{path}:{name} is declared environmental with no reason")
        text = read(root, path)
        span = fn_span(text, name)
        if span is None:
            raise Refusal(
                f"this file declares `{name}` in {path} as a guard that may refuse anywhere, and "
                "there is no such test there any more. A declaration that describes a tree which "
                "has moved on is the thing it exists to prevent: delete the entry or fix the name."
            )
        body = "\n".join(text.splitlines()[span[0] : span[1]])
        if not calls_skip(body):
            raise Refusal(
                f"`{name}` in {path} is declared here as a guard that may refuse, and it no longer "
                "skips at all — delete the entry rather than leaving it excusing nothing."
            )
        spans[(path, span[0], span[1])] = why
    return spans


# ---------------------------------------------------------------------------------------------
# Reading the run
# ---------------------------------------------------------------------------------------------

REFUSED = re.compile(r"(?m)^SKIPPED at ([^\s:]+):(\d+): (.*)$")
RESULT = re.compile(r"(?m)^test result:")


def attribute(root, path, lib):
    """Which test binary does a guard at `path` belong to? `None` when that cannot be answered.

    `src/**` is the library's binary, `tests/<name>.rs` is `<name>`. A guard in `tests/common/`
    is compiled into every integration binary and belongs to none of them, so it is attributed to
    nothing — and a site attributed to nothing blocks nothing.
    """
    if not os.path.exists(os.path.join(root, path)):
        return None
    if path.startswith("src/"):
        return lib
    m = re.fullmatch(r"tests/([A-Za-z0-9_]+)\.rs", path)
    return m.group(1) if m else None


def run_suite(root):
    """`SKEIN_TESTS_NO_SKIP=1 tools/gates.sh run test` — the `test` gate's own command, not a copy.

    The command is written down in `tools/gates.sh` and nowhere else, which is the point of that
    file. This gate adds one environment variable to it.
    """
    env = dict(os.environ, SKEIN_TESTS_NO_SKIP="1")
    proc = subprocess.run(
        [os.path.join(root, "tools/gates.sh"), "run", "test"],
        cwd=root,
        env=env,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
    )
    return proc.returncode, proc.stdout


# ---------------------------------------------------------------------------------------------
# The report
# ---------------------------------------------------------------------------------------------


def emit(lines):
    text = "\n".join(lines)
    print(text)
    summary = os.environ.get("GITHUB_STEP_SUMMARY")
    if summary:
        with open(summary, "a", encoding="utf-8") as fh:
            fh.write(text + "\n")


def main(argv):
    scope_only = "--scope" in argv
    rest = [a for a in argv if a != "--scope"]
    if len(rest) > 1:
        print(USAGE, file=sys.stderr)
        return 2
    root = os.path.abspath(rest[0]) if rest else os.path.dirname(os.path.dirname(os.path.abspath(__file__)))

    lib, declared = requirements(root)
    tools = sorted({t for ts in declared.values() for t in ts})
    caps = capabilities(root, set(tools))
    present = {t: probe(t, caps) for t in tools}
    spans = environmental_spans(root)

    missing = {b: [t for t in ts if not present[t]] for b, ts in declared.items()}
    hostable = sorted(b for b, m in missing.items() if not m)
    unhostable = sorted(b for b, m in missing.items() if m)

    if scope_only:
        print(f"tools this suite declares, asked of this machine ({len(tools)}):")
        for tool in tools:
            how = "command -v"
            if tool in caps:
                argv, cwd = caps[tool]
                at = "" if cwd == root else f" in {os.path.relpath(cwd, root)}"
                said = shlex.join(argv)
                how = f"capability{at}: {said if len(said) <= 64 else said[:61] + '…'}"
            print(f"  {'present' if present[tool] else 'ABSENT '}  {tool}  [{how}]")
        print()
        print(
            f"{len(caps)} of those are CAPABILITIES, derived from the nullary `pub fn … -> bool` "
            f"probes in {COMMON} rather than declared here: {', '.join(sorted(caps))}"
        )
        print()
        print(f"binaries this machine can fully host, where a skip is a FAILURE ({len(hostable)}):")
        for b in hostable:
            print(f"  {b} — declares {declared[b]}")
        others = sorted(
            os.path.splitext(f)[0]
            for f in os.listdir(os.path.join(root, "tests"))
            if f.endswith(".rs") and os.path.splitext(f)[0] not in declared
        )
        print(f"  and {len(others)} binaries that declare nothing: {', '.join(others)}")
        print()
        print(f"binaries this machine cannot fully host, report-only ({len(unhostable)}):")
        for b in unhostable:
            print(f"  {b} — missing {missing[b]}")
        print()
        print(f"guards declared environmental ({len(spans)}):")
        for (path, first, last), why in sorted(spans.items()):
            print(f"  {path}:{first}-{last}  {why}")
        return 0

    status, log = run_suite(root)
    logpath = os.path.join(os.environ.get("RUNNER_TEMP", "/var/tmp"), "noskip.log")
    try:
        with open(logpath, "w", encoding="utf-8") as fh:
            fh.write(log)
    except OSError:
        logpath = "(not written)"

    binaries = len(RESULT.findall(log))
    out = ["### What this run skipped, and what that costs"]

    # No test binary reported, so there is nothing to read a verdict out of — and the one thing
    # that must not happen here is reading an all-clear into that silence, which is what this step
    # did until SKEIN-826. It is a FAILURE: the switch asked for a run and did not get one.
    #
    # The exception is `tools/gates.sh`'s own two non-verdicts, 3 (the tree moved under the run) and
    # 4 (the logs could not be written), which that script documents as neither a pass nor a
    # failure. `run` mode execs the gate's command and so normally carries cargo's status rather
    # than either of those; they are honoured here anyway, because the alternative is this gate
    # deciding that a refusal is a red, one layer up from the defect it exists for.
    if status in (3, 4) or binaries == 0:
        out.append(f"**Unknown — the run reported on no test binary at all** (exit {status}).")
        out.append(
            "This is NOT an all-clear. The switch asked for a run with no skips and got no run."
        )
        out.append("```")
        out += log.splitlines()[-20:]
        out.append("```")
        out.append(f"Full log: `{logpath}`")
        if status in (3, 4):
            out.append(
                f"`tools/gates.sh` exited {status}, which it documents as neither a pass nor a "
                "failure, so this gate declines to make it one."
            )
            emit(out)
            return 0
        emit(out)
        return 1

    refusals = sorted({(f, int(n), why) for f, n, why in REFUSED.findall(log)})
    findings, expected, reported = [], [], []
    for path, line, why in refusals:
        binary = attribute(root, path, lib)
        excuse = next(
            (w for (p, a, b), w in spans.items() if p == path and a <= line <= b),
            None,
        )
        if excuse:
            expected.append((path, line, why, excuse))
        elif binary is None:
            reported.append((path, line, why, "belongs to no single test binary"))
        elif missing.get(binary):
            reported.append(
                (path, line, why, f"`{binary}` is missing {missing[binary]} on this machine")
            )
        else:
            findings.append((path, line, why, binary))

    plural = "binary" if binaries == 1 else "binaries"
    out.append(
        f"Ran {binaries} test {plural} under `SKEIN_TESTS_NO_SKIP=1`. "
        f"{len(hostable)} of the {len(declared)} binaries with declared requirements are fully "
        f"hosted here, and every binary that declares nothing always is — a skip in any of them "
        f"fails this gate."
    )
    if findings:
        out.append("")
        out.append(f"**{len(findings)} skip(s) this machine has no excuse for:**")
        for path, line, why, binary in findings:
            out.append(f"- `{path}:{line}` ({binary}) — {why}")
        out.append("")
        out.append(
            "Every tool `common::REQUIREMENTS` declares for those binaries is present here, so "
            "these guards refused for a reason nothing has written down. Fix what they need, or "
            "declare the requirement."
        )
    if reported:
        out.append("")
        out.append(f"**{len(reported)} skip(s) reported, not counted** — this machine cannot host them:")
        for path, line, why, because in reported:
            out.append(f"- `{path}:{line}` — {why} ({because})")
    if expected:
        out.append("")
        out.append(f"**{len(expected)} declared environmental refusal(s):**")
        for path, line, why, excuse in expected:
            out.append(f"- `{path}:{line}` — {why} ({excuse})")
    if not refusals:
        out.append("")
        out.append(f"Nothing: every guard found what it needed, across {binaries} test {plural}.")
    out.append("")
    out.append(f"Full log: `{logpath}`")
    emit(out)
    # `tools/gates.sh` shows the lines of a failed gate's log that NAME the failure, matching
    # `^error` among others. The report above is markdown for a job summary and matches none of
    # them, so the verdict is said again here in the shape that runner reads.
    for path, line, why, binary in findings:
        print(f"error: {path}:{line} skipped in `{binary}`, which this machine can fully host: {why}",
              file=sys.stderr)
    return 1 if findings else 0


if __name__ == "__main__":
    try:
        sys.exit(main(sys.argv[1:]))
    except Refusal as exc:
        print(f"error: noskip-check REFUSES TO RUN — {exc}", file=sys.stderr)
        print(
            "    No verdict was reached, and this is not a pass. A scope derived from nothing "
            "would mark every binary hostable.",
            file=sys.stderr,
        )
        sys.exit(2)
