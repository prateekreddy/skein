#!/usr/bin/env bash
#
# The gate list. There is one of it.
#
# Every gate that can fail a change in this repository is named once, here, with the command that
# runs it. `.github/workflows/ci.yml` invokes them by name (`tools/gates.sh run <name>`) so that a
# gate's command is written down in exactly one place, and `--check` — itself one of the gates —
# fails when the workflow and `CONTRIBUTING.md` stop naming the same set.
#
# **Why this file exists.** The list used to live twice: once in `ci.yml` and once in a runner that
# every agent on this box was told to run, which was never checked in. The two drifted in BOTH
# directions before anyone looked. `tools/alone-check.py` was in the local runner and in
# `CONTRIBUTING.md` and in no workflow step; `tools/line-cite-check.py` landed in the workflow and
# in `CONTRIBUTING.md` and the local runner never learned about it. Each list was missing a gate the
# other had, so running "the gates" locally and running "the gates" in CI checked different things
# and neither said so. The two copies of `node --test` had drifted apart too, in their arguments.
#
# Usage:
#
#   tools/gates.sh [<worktree>]   run every gate against a worktree (default: this repository)
#   tools/gates.sh run <name>     run one gate and exit its status — what CI calls
#   tools/gates.sh --list         print the list as `name|ci|command`
#   tools/gates.sh --check        the consistency gate: ci.yml and CONTRIBUTING.md name this set
#   tools/gates.sh --verify <f>   is that file ONE run of this, whole? — see SKEIN-903 below
#   tools/gates.sh --provision [<worktree>]
#                                 check out any declared submodule that was never initialised
#                                 there — the step a full run takes first; see SKEIN-885 below
#   tools/gates.sh --exit-codes  the codes below as `code|kind|summary`, for anything that has
#                                 to tell a refusal from a red — see SKEIN-945 below
#
# Exit codes. There are more than two on purpose, and the list below is where a new refusal
# gets written down — a run that did not happen must never read as one that failed:
#
#   0  every gate passed, against the tree named in the footer
#   1  a gate failed — or, under `--verify`, the file is not one whole run of this
#   2  this script was called wrongly
#   3  RESULTS REFUSED — the tree moved or changed underneath the run, so the results describe
#      something other than what the footer would name. Not a green, and not a red. See below.
#   4  RUN REFUSED — the logs cannot be written, so there is nothing to report. Also not a red:
#      "I cannot record what I am about to do" is not a gate failure. See SKEIN-793 below.
#   5  RUN REFUSED — a binary a declared gate's command begins with is not on `$PATH`, so those
#      gates could not have run. Also not a red, for the same reason and after the same incident
#      in a second doorway: a gate that never executed did not fail. See SKEIN-938 below.
#   6  RUN REFUSED — a gate failed having named artefacts it had just built that are not on disk,
#      so what failed is the machine's capacity rather than the suite. Also not a red, for the
#      third time and through a third door: a build that could not be written did not fail a test.
#      See SKEIN-941 below.
#
# **The refusal is the point (SKEIN-784).** The runner this replaces stamped its footer with
# `$(git rev-parse --short HEAD)` evaluated when the footer PRINTED — at the end. A run was started
# against one commit, the agent working in that worktree rebased and committed while it was in
# flight, and the footer read the new sha: "ALL GATES GREEN at 8aa7a0f" for a run that had compiled
# and tested a tree which by then no longer existed and had never been the tree named. That footer
# is what a reviewer copies into a merge commit as evidence, so a wrong sha does not stay in the
# terminal — it is written down as provenance for a merge. It is the same defect family as
# SKEIN-647 (a leak check answering `0` beside 195 matching processes) and SKEIN-732: a status
# display reporting on something other than what it names. So HEAD and `git status --porcelain` are
# both recorded BEFORE the first gate, the footer is stamped with the recorded sha, and if either
# has changed by the end the verdict is withheld and both values are printed. A clean tree that is
# dirty at the end is the same lie as a moved HEAD, and it happened here on the same day.
#
# **A verdict names the TREE, not the commit (SKEIN-800).** The refusal above answers "did the tree
# change under the run?" and the footer answers "what was tested?". Those are different questions
# and SKEIN-784 fixed only the first: a tree that is dirty at the START and unchanged throughout
# compares equal, so nothing is refused — correctly, nothing changed — and the footer then stamped
# a bare `$short_before` anyway. `ALL GATES GREEN at 237a977` records that a COMMIT was tested when
# what was tested was that commit plus uncommitted work, which may differ from what is eventually
# committed. Observed on a full green run whose change was not in `237a977` at all; the agent
# noticed and declined to quote it, which is the only reason it did not become provenance for a
# merge. Gating before committing is the workflow this repository wants, so a dirty run is not
# refused and gets no exit code of its own — the stamp says what it ran on instead:
#
#     === ALL GATES GREEN at 237a977 + 7 uncommitted change(s) ===
#
# which cannot be quoted as provenance without a reader noticing, at the cost of one line. The log
# directory carries a digest of those changes for the same reason: named from `$short_before`
# alone, two runs on one base with different uncommitted trees were indistinguishable once you had
# only the path.
#
# **A verdict is evidence about the lines above it, so it says which lines those are (SKEIN-903).**
# Everything above answers "is this verdict true of the tree it names?". It says nothing about the
# question a reader actually has, which is "is this verdict about the output I am looking at?" —
# and that one has been answered wrongly, twice in one batch, in opposite directions. Two lanes on
# one box each redirected a full run into a file of the same name in one shared directory. Each then
# read back a file carrying the OTHER lane's header and first three gate lines, a run of NUL bytes
# where its own six middle gate lines should have been, and its own green footer:
#
#     === gates for /var/tmp/skein-wt-envlock at c3e17e2 ===
#     fmt                                      ok
#     clippy                                   ok
#     submodule-check                          ok
#     <a long run of NUL bytes>
#     prose-check                              ok
#     === ALL GATES GREEN at 4dc5563 ===
#
# Nothing in that stream was false. Every line in it was printed by a real run that really said it.
# It is a lie made entirely of true lines, and the footer — the line a reviewer copies into a merge
# commit as provenance — is the only part of it that is this run's. Six gates are simply absent and
# the green says nothing about their absence, because a footer has never claimed anything about what
# precedes it.
#
# So it claims it now, and the claim is checkable three ways, each cheaper than the last:
#
#   * **Every line of one run carries that run's id**, header, gate lines, `logs:` and footer alike.
#     Two ids in one file is two runs, visible by eye with no tool at all. The id is the process id
#     folded with `$RANDOM`, and the load-bearing half is the PID: two runs that overlap in time
#     cannot share one, which makes concurrent uniqueness a guarantee rather than a probability. The
#     random half is only so that a pid reused hours later reads as a different run.
#   * **The footer says how many stamped gate lines precede it.** A reader who counts eighteen has
#     checked that none went missing; the run above could not have said "18" and shown twelve.
#   * **`tools/gates.sh --verify <file>`** does both of those and the rest: one verdict in the file,
#     no NUL bytes, no foreign stamp, the gate lines all present — and then it leaves the stream
#     entirely and reads the receipt in that run's own log directory, which is namespaced per sha,
#     per working tree and per worktree, and which in both incidents held the complete uncorrupted
#     truth while the redirected stream did not.
#
# The run verifies itself the same way before it prints a green at all: the log directory must hold
# one log per gate and a receipt still carrying this run's id. A second run that took the directory
# over — `$GATE_LOGS` pointed at one path twice — is refused rather than reported, because "somebody
# else's logs are in my log directory" is not a gate failure and must not read as one.
#
# **What none of this does is depend on the caller redirecting correctly.** That was the other half
# of SKEIN-903 and it was fixed by a dispatch rule, which is a thing people follow rather than a
# property the tool has. A careless reader is the one being protected here.
#
# **And "unchanged" means the content now, not the porcelain lines.** `git status --porcelain`
# prints `M <path>` for a modified file however many times its bytes change, so the very case the
# refusal exists for — somebody editing the worktree mid-run — was invisible to it whenever that
# file was already modified when the run started. The two ends are compared over
# `git status --porcelain` AND `git diff HEAD`, which is the same pair the log directory's digest
# is built from, so an edit to an already-dirty file refuses like any other.
#
# **The failure report is not truncated.** `tail -60` cut the "error: 1 target failed: <name>" line
# twice in one session, which is the truncated-view trap CONTRIBUTING.md names — a report you cannot
# act on is worse than none, because it looks like a report. This prints the lines that NAME the
# failure wherever they are in the log, says how many matched when it shows fewer than all of them,
# and always names the full log.
#
# **No path from any particular box is written here.** A contributor cloning this repository has no
# `/boxes/.skein/toolchain`, and `$SKEIN_UI_FIXTURE_ROOT` already defaults to `/var/tmp/skein-uifix`
# in `tests/ui/lift.mjs:118`. `$CARGO_HOME`, `$RUSTUP_HOME` and `$PATH` are inherited untouched;
# `$CARGO_TARGET_DIR` defaults to `.target` inside the worktree, which `.gitignore:2` covers so it
# cannot make the tree look dirty, and any of them may be set by the caller.

# **Everything below is one compound command, and that brace is load-bearing.**
#
# bash reads a script incrementally from disk, by byte offset, as it runs. Edit the file while it
# is executing and the interpreter resumes at an offset that is no longer a statement boundary. This
# script's whole purpose is to notice that somebody wrote to the worktree mid-run — and
# `tools/gates.sh` is exactly the file an agent working on the gates will be editing, so the case
# where the refusal matters most was the case where it was unavailable. Observed, with all fifteen
# gates already green:
#
#     citation-check                           ok
#     ./tools/gates.sh: line 345: syntax error near unexpected token `('
#     EXIT=2
#
# Exit 2 is the usage code, the refusal block sits below that point, and it never ran: the run
# neither passed, failed, nor refused. A check that cannot fire in one of the cases it exists for,
# while looking like it works in all the others, is the shape SKEIN-647 named (SKEIN-792).
#
# bash must parse a compound command in FULL before executing any of it, so `{` here forces the
# whole file to be read up front, and every path exits inside — the interpreter never reads past
# the closing brace, whatever happens to the file meanwhile.
{

set -u

# ---------------------------------------------------------------------------------------------
# The list
# ---------------------------------------------------------------------------------------------

# `name|ci|command`.
#
# `ci` is `yes`, or `no: <reason>` for a gate CI deliberately does not run — `--check` enforces both
# directions, so an exception has to be written here to be allowed and cannot be forgotten into
# existence.
#
# The order is CI's order, and three positions in it are load-bearing:
#
#   * `submodule-check` comes BEFORE `test`, because it is a precondition of two tests inside it —
#     the `upstream/sync` drift guards, which take their guard and return when the submodule is not
#     checked out, and a skipped test PASSES. It costs milliseconds, so learning that a tree is
#     under-provisioned before a four-minute test run is free (SKEIN-826).
#   * `alone-check` needs the lib test binary, so it follows `test` and reuses that build.
#   * `noskip-check` runs that same suite a second time with `$SKEIN_TESTS_NO_SKIP` set, through
#     `tools/gates.sh run test` rather than a second copy of the command, so it follows `test` too
#     and costs execution rather than a compile. It is the gate that can fail a change over a SKIP,
#     scoped to the binaries whose declared requirements this machine answers for (SKEIN-881).
#   * `citation-check` is last, because it is the only gate that reads git history and CI has to
#     deepen its shallow clone before calling it (see the comment on that step in ci.yml).
gates() {
  printf '%s\n' \
    "fmt|yes|cargo fmt --all -- --check" \
    "clippy|yes|cargo clippy --all-targets --all -- -D warnings" \
    "submodule-check|yes|python3 tools/submodule-check.py" \
    "test|yes|cargo test --all --no-fail-fast" \
    "alone-check|no: 988 processes on top of a build, and the finding is a property of the tests rather than of the change — CONTRIBUTING.md argues this under 'The gate that is not in CI'|python3 tools/alone-check.py" \
    "module-check|yes|python3 tools/module-check.py" \
    "source-check|yes|python3 tools/source-check.py" \
    "env-lock-check|yes|python3 tools/env-lock-check.py" \
    "fleet-pin-check|yes|python3 tools/fleet-pin-check.py" \
    "prose-check|yes|python3 tools/prose-check.py" \
    "line-cite-check|yes|python3 tools/line-cite-check.py" \
    "continuation-check|yes|python3 tools/continuation-check.py" \
    "residue-check|yes|python3 tools/residue-check.py" \
    "secret-check|yes|python3 tools/secret-check.py" \
    "cockpit-tests|yes|node --test \"cockpit/test/*.test.mjs\"" \
    "cockpit-bundle|yes|node cockpit/build.mjs --check" \
    "fixture-root-check|yes|node tests/ui/harness/leaks.mjs --fixture-root" \
    "noskip-check|yes|python3 tools/noskip-check.py" \
    "gate-list-check|yes|tools/gates.sh --check" \
    "citation-check|yes|python3 tools/citation-check.py"
}

# ---------------------------------------------------------------------------------------------
# Where we are
# ---------------------------------------------------------------------------------------------

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)

# The worktree to work in. A bare `tools/gates.sh` means "this repository", which is what a fresh
# clone and what CI both want; an explicit path is what a box running several worktrees wants.
resolve_root() {
  local want="${1:-$here/..}"
  if ! cd "$want" 2>/dev/null; then
    echo "gates.sh: no such directory: $want" >&2
    exit 2
  fi
  if ! root=$(git rev-parse --show-toplevel 2>/dev/null); then
    echo "gates.sh: $want is not inside a git worktree" >&2
    exit 2
  fi
  cd "$root" || exit 2
}

# Everything the working tree holds that `HEAD` does not, as one short digest: the porcelain status,
# which NAMES untracked files, and `git diff HEAD`, which carries the CONTENT of every tracked
# change. Two runs on the same commit with different uncommitted work digest differently, which is
# what the log directory needs; and the same value read at both ends of a run is what tells an edit
# to an already-modified file apart from no edit at all, which `git status --porcelain` cannot.
#
# It does not read the CONTENT of an untracked file — `git diff HEAD` does not see one — so a run
# that swaps the bytes of a file git has never been told about still digests the same. Naming it
# rather than implying otherwise: the status line for it is there either way, so the file cannot
# appear or disappear unnoticed.
worktree_digest() {
  { git status --porcelain; git diff HEAD; } | git hash-object --stdin | cut -c1-12
}

# **A worktree is provisioned before it is gated, and only in the one way it cannot be wrong
# (SKEIN-885).** `git worktree add` does not populate submodules, and `git submodule update --init` is
# per worktree, so every fresh lane and integration worktree went red on `submodule-check` until
# somebody ran the command its failure message names — seven times in one session. That red is
# about how the tree was MADE, not about the change being gated.
#
# So a full run initialises a submodule that is declared and was never initialised — the leading
# `-` in `git submodule status` — at the commit the index records, and says it did. Nothing else:
#
#   * a submodule already checked out is left alone, wherever it points. The documented upgrade
#     sits deliberately ahead of the recorded pin, and moving it back would undo that work;
#   * `submodule-check` still runs and still judges. An init that fails — no network, a URL that
#     has moved — leaves the tree as it was, and the gate reports it exactly as before;
#   * `tools/gates.sh run <name>`, which is what CI calls, does not do this. CI checks submodules
#     out itself, and a gate called by name runs that gate and nothing else.
#
# It is not the `$PATH` repair the preflight below refuses to make. That one would leave the shell
# broken for the next command with the run green; this changes the worktree itself, once, into the
# state `CONTRIBUTING.md`'s setup already asks for, and every later command in it sees the same.
provision() {
  local path init
  while IFS= read -r line; do
    case "$line" in
      -*)
        path=$(printf '%s' "$line" | awk '{print $2}')
        if init=$(git submodule update --init -- "$path" 2>&1); then
          echo "provision: $path was declared and never initialised in this worktree — checked out at the recorded commit"
        else
          echo "provision: $path was declared and never initialised, and \`git submodule update --init -- $path\` failed; submodule-check will say so"
          printf '%s\n' "$init" | sed 's/^/    /'
        fi
        ;;
    esac
  done < <(git submodule status 2>/dev/null)
}

field() { # field <line> <n>
  printf '%s' "$1" | cut -d'|' -f"$2"
}

# The command for a gate is the REST of the line, so a command may contain `|` without the list
# needing an escape.
gate_cmd() { printf '%s' "$1" | cut -d'|' -f3-; }

# ---------------------------------------------------------------------------------------------
# --verify: is this file ONE run of this script, whole? (SKEIN-903)
# ---------------------------------------------------------------------------------------------

# Every shape of a run's output is written down ONCE, here, because `--verify` and the run itself
# have to agree about it or the check drifts into reading a format nothing prints any more. The run
# calls these to print; `--verify` calls them to match.
#
# The stamp is a fixed-width lowercase hex token so that a foreign stamp is recognisable as a stamp
# even when it is not this run's — `--verify` has to be able to say "this line belongs to run
# 40a1c39e, not to yours", which it cannot do if it can only test one exact prefix.
stamp_re='[0-9a-f]\{8\}'
header_re="^=== gates for .* === run $stamp_re\$"
verdict_re="^=== \(ALL GATES GREEN\|SOMETHING FAILED\) at .* === run $stamp_re,"

# The id itself. `$$` is what makes it unique among runs that OVERLAP, which is the only uniqueness
# this needs and the only one it can promise: two processes alive at once have different pids by
# construction. `$RANDOM` is there for the other case — a pid reused a few hours later, reading as
# the same run to somebody comparing two old logs.
new_run_id() { printf '%04x%04x' "$(( $$ & 0xffff ))" "$RANDOM"; }

# What a reader checks, in the line they are most likely to quote. The verdict keeps its historic
# `=== <WORD> at <tested> ===` prefix exactly — CONTRIBUTING.md tells people to read that line and
# things grep for it — and carries the rest after it.
verdict_line() { # verdict_line <word> <tested> <run> <gate lines> <self>
  printf '=== %s at %s === run %s, %s gate lines above carry it, verify: %s --verify <this file>\n' \
    "$1" "$2" "$3" "$4" "$5"
}

do_verify() { # do_verify <file>
  local f="${1:-}"
  if [ -z "$f" ]; then
    echo "gates.sh: usage: tools/gates.sh --verify <file a full run was written to>" >&2
    exit 2
  fi
  if [ ! -r "$f" ]; then
    echo "gates.sh --verify: cannot read $f" >&2
    exit 2
  fi

  local bad=0 verdicts n_verdicts

  # 0. NUL bytes, FIRST — and every grep below carries `-a` for the same reason. The signature of
  #    two processes writing at independent offsets into one path is a run of NULs, which makes the
  #    file binary; and `grep` on a binary file reports "binary file matches" and matches no LINES,
  #    so without `-a` every clause below reads a corrupted stream as an empty one and this reports
  #    the wrong fault about the right file. Found by running the reproduction rather than by
  #    reading the code: the first version of this said a NUL-filled interleave was "an unstamped
  #    verdict from an older gates.sh".
  local size nulless nuls
  size=$(wc -c <"$f" | tr -d ' ')
  nulless=$(tr -d '\000' <"$f" | wc -c | tr -d ' ')
  nuls=$((size - nulless))
  if [ "$nuls" -gt 0 ]; then
    echo "gates.sh --verify: $f holds $nuls NUL byte(s) — two processes wrote into it at"
    echo "    independent offsets. Whatever is missing from it is missing silently."
    bad=1
  fi

  verdicts=$(grep -an "$verdict_re" "$f")
  n_verdicts=$(printf '%s' "$verdicts" | grep -c . )

  # 1. Refuse rather than pass when there is nothing to check. A file with no verdict in it is not
  #    a file whose verdict is fine, and a bare `=== ALL GATES GREEN at <sha> ===` with no stamp is
  #    output from a gates.sh older than this check — which is exactly the stream this exists to
  #    stop being trusted, so it is a refusal and not a pass either.
  if [ "$n_verdicts" = 0 ]; then
    echo "gates.sh --verify: $f carries no verdict this can check."
    if grep -aq '^=== \(ALL GATES GREEN\|SOMETHING FAILED\) at ' "$f"; then
      echo "    It does carry an UNSTAMPED verdict line, which means it was written by a gates.sh"
      echo "    from before SKEIN-903. Nothing can tell you whether the lines above that line are"
      echo "    the same run's. Run the gates again and verify that."
    else
      echo "    Nothing in it looks like the end of a run. Did the run finish, and is this the file"
      echo "    it was redirected to?"
    fi
    exit 1
  fi
  if [ "$n_verdicts" -gt 1 ]; then
    echo "gates.sh --verify: $f carries $n_verdicts verdicts, so it is not one run:"
    printf '%s\n' "$verdicts" | sed 's/^/    /'
    bad=1
  fi

  # The LAST verdict is the one a reader quotes, so it is the one everything else is measured
  # against — including the earlier verdicts, which the count above has already reported.
  local footer run claimed word
  footer=$(printf '%s\n' "$verdicts" | tail -1 | cut -d: -f2-)
  run=$(printf '%s' "$footer" | sed 's/^.*=== run \([0-9a-f]*\),.*$/\1/')
  claimed=$(printf '%s' "$footer" | sed 's/^.*, \([0-9]*\) gate lines above.*$/\1/')
  word=$(printf '%s' "$footer" | sed 's/^=== \(.*\) at .*$/\1/')

  # 2. A stamp in this file that is not this run's. One file, two runs.
  local foreign
  foreign=$(grep -ao "^$stamp_re " "$f" | tr -d ' ' | sort -u | grep -vxF "$run")
  if [ -n "$foreign" ]; then
    echo "gates.sh --verify: $f carries lines stamped by other runs, so the verdict's run is not"
    echo "    the only one in it. The verdict says $run; these are also here: $(printf '%s' "$foreign" | tr '\n' ' ')"
    bad=1
  fi

  # 3. The header. A stream that starts inside somebody else's run has none of its own.
  if ! grep -aq "^=== gates for .* === run $run\$" "$f"; then
    echo "gates.sh --verify: $f has no '=== gates for ... === run $run' header, so what it holds"
    echo "    starts partway through run $run — or not in it at all."
    bad=1
  fi

  # 4. The count. This is the clause the incident needed: the six missing gate lines were missing,
  #    not wrong, and a verdict that says nothing about how many lines precede it cannot notice.
  local present
  present=$(grep -ac "^$run .* \(ok\|FAILED\)\$" "$f")
  if [ "$present" != "$claimed" ]; then
    echo "gates.sh --verify: $f claims $claimed gate line(s) for run $run and holds $present."
    echo "    The missing ones ran; this file is not where they landed."
    bad=1
  fi

  # 5. And then off the stream entirely, into the log directory the run made for itself. This is
  #    the part that cannot be forged by an interleaving, because an interleaving only ever mixes
  #    two streams — it never writes a receipt.
  local logs_line logs=""
  logs_line=$(grep -a "^$run logs: " "$f" | tail -1)
  if [ -z "$logs_line" ]; then
    echo "gates.sh --verify: $f does not say where run $run put its logs, so the verdict cannot be"
    echo "    checked against anything but itself."
    bad=1
  else
    logs=${logs_line#"$run logs: "}
    if [ ! -r "$logs/receipt" ]; then
      echo "gates.sh --verify: $logs/receipt is not there. The logs a verdict is evidence about are"
      echo "    gone (they live in /var/tmp), so there is nothing left to check it against."
      bad=1
    else
      local r_run r_gates r_verdict
      r_run=$(sed -n 's/^run //p' "$logs/receipt")
      r_gates=$(grep -c '^gate ' "$logs/receipt")
      r_verdict=$(sed -n 's/^verdict //p' "$logs/receipt")
      if [ "$r_run" != "$run" ]; then
        echo "gates.sh --verify: $logs/receipt was written by run $r_run, not $run."
        bad=1
      fi
      if [ "$r_gates" != "$claimed" ]; then
        echo "gates.sh --verify: $logs/receipt records $r_gates gate(s); the verdict claims $claimed."
        bad=1
      fi
      if [ "$r_verdict" != "$word" ]; then
        echo "gates.sh --verify: $logs/receipt ends '$r_verdict'; this file ends '$word'."
        bad=1
      fi
    fi
  fi

  if [ "$bad" = 0 ]; then
    echo "gates.sh --verify: $f is one whole run — $run, $claimed gate lines, $word, and"
    echo "    $logs/receipt agrees."
  fi
  return "$bad"
}

# ---------------------------------------------------------------------------------------------
# --check: the two other places this list is written are still writing the same list
# ---------------------------------------------------------------------------------------------

# `ci.yml` and `CONTRIBUTING.md` do not hold the gates' COMMANDS any more — that is what removes the
# drift this file was written for. What they still hold is a membership claim each: the workflow
# runs some set of gates, and the table tells a contributor what the set is. This fails when either
# stops agreeing with the list above.
#
# What makes it fail, and each of these was run before this was committed: delete a `- run:
# tools/gates.sh run <name>` line from ci.yml; add one naming a gate that is not in the list; add
# one naming `alone-check`, which is in the list and marked not-for-CI; delete a row from the gate
# table in CONTRIBUTING.md; change the step count written beside the `grep -c` in that file.
do_check() {
  local ci=".github/workflows/ci.yml" doc="CONTRIBUTING.md" bad=0 line name ci_flag

  local declared_ci="" declared_all=""
  while IFS= read -r line; do
    name=$(field "$line" 1)
    ci_flag=$(field "$line" 2)
    declared_all="$declared_all $name"
    case "$ci_flag" in yes) declared_ci="$declared_ci $name" ;; esac
  done < <(gates)

  # 1. What the workflow actually invokes.
  local invoked
  invoked=$(sed -n 's|^ *- run: tools/gates\.sh run \([a-z0-9-]*\) *$|\1|p' "$ci" | sort)

  local want_ci
  want_ci=$(printf '%s\n' $declared_ci | sort)

  if [ "$invoked" != "$want_ci" ]; then
    echo "gates.sh --check: $ci does not invoke the gates this list marks for CI." >&2
    diff <(printf '%s\n' "$want_ci") <(printf '%s\n' "$invoked") \
      | sed 's/^</    only in tools\/gates.sh: /; s/^>/    only in ci.yml:         /' >&2
    bad=1
  fi

  # 2. A name the workflow invokes that this list does not define at all would already have shown up
  #    above; a DUPLICATE would not, because `sort` keeps both and the sets would differ only if one
  #    were also missing. Count them.
  local dupes
  dupes=$(printf '%s\n' "$invoked" | uniq -d)
  if [ -n "$dupes" ]; then
    echo "gates.sh --check: $ci invokes these more than once: $dupes" >&2
    bad=1
  fi

  # 3. The gate table in CONTRIBUTING.md, which is what a contributor reads. Scoped to the `## The
  #    gates` section so that another table in that file cannot be mistaken for this one, and keyed
  #    on the gate NAME in the first cell rather than on a command, which is the copy that rotted.
  local tabled
  tabled=$(awk '
      /^## The gates$/       { inside = 1; next }
      inside && /^## /       { inside = 0 }
      inside && /^\| `[a-z0-9-]+` \|/ { gsub(/^\| `|` \|.*$/, ""); print }
    ' "$doc" | sort)

  local want_all
  want_all=$(printf '%s\n' $declared_all | sort)

  if [ "$tabled" != "$want_all" ]; then
    echo "gates.sh --check: the gate table in $doc does not name the gates this list defines." >&2
    diff <(printf '%s\n' "$want_all") <(printf '%s\n' "$tabled") \
      | sed 's/^</    only in tools\/gates.sh: /; s/^>/    only in CONTRIBUTING.md: /' >&2
    bad=1
  fi

  # 4. The one number CONTRIBUTING.md still states about the workflow. It is written with the
  #    command that produces it, which is this repository's pattern for a countable claim — and a
  #    written-down command whose written-down answer is never re-run is the claim, not the check.
  local claimed actual
  claimed=$(sed -n "s|^ *grep -c '\^      - run:' \.github/workflows/ci\.yml *# → \([0-9]*\).*|\1|p" "$doc")
  actual=$(grep -c '^      - run:' "$ci")
  if [ -z "$claimed" ]; then
    echo "gates.sh --check: $doc no longer carries the '- run:' step count this checks." >&2
    echo "    Either restore it, or delete this clause rather than leaving it passing vacuously." >&2
    bad=1
  elif [ "$claimed" != "$actual" ]; then
    echo "gates.sh --check: $doc says $ci has $claimed '- run:' steps; it has $actual." >&2
    bad=1
  fi

  # 5. And the other number it states, about this list. Same argument as clause 4, and the reason
  #    both are here rather than left to a reader: `CONTRIBUTING.md` said the workflow had fifteen
  #    steps and ten gates while it actually had seventeen and eleven, with `citation-check.py` in
  #    neither, and that went unnoticed long enough to need its own item (SKEIN-741). A count in
  #    prose is a fact written twice, and the copy in prose is the one that rots.
  local claimed_n actual_n
  claimed_n=$(sed -n 's@^ *tools/gates\.sh --list | wc -l *# → \([0-9]*\).*@\1@p' "$doc")
  actual_n=$(gates | wc -l | tr -d ' ')
  if [ -z "$claimed_n" ]; then
    echo "gates.sh --check: $doc no longer carries the gate count this checks." >&2
    echo "    Either restore it, or delete this clause rather than leaving it passing vacuously." >&2
    bad=1
  elif [ "$claimed_n" != "$actual_n" ]; then
    echo "gates.sh --check: $doc says there are $claimed_n gates; this list defines $actual_n." >&2
    bad=1
  fi

  # 6. A gate that exists in the tree and is in no list at all. Clauses 1-5 compare three lists
  #    with each other, and three lists can agree perfectly about a gate that nobody ever wired up
  #    — which is SKEIN-786's point, and the half of it that adding a `- run:` line would not have
  #    fixed: "adding alone-check to ci.yml today fixes today's instance and leaves the mechanism
  #    that produced it". So the tree itself is the fourth opinion. Every `tools/*.py` is a gate
  #    unless it is named below with its reason.
  #
  #    `rustcut.py` is the only one: it is the shared Rust reader that four gates import rather
  #    than a gate of its own (`grep -l '^import rustcut' tools/*.py` names them), and it has no
  #    verdict to give — its self-check runs inside every one of those four instead.
  #
  #    A tool that a workflow step runs DIRECTLY counts as run too — read out of ci.yml's own
  #    `- run: python3 tools/<name>.py` lines, not listed. `coverage-check.py` is the one today: it
  #    runs in the `coverage` job rather than from this list, for the reason given on that job
  #    (an instrumented build is a second full compile, and this list is every local run's bill).
  #    That is still a gate something runs, which is all this clause asks; delete its step and the
  #    tool is reported here like any other orphan (SKEIN-508).
  local not_a_gate="rustcut"
  local on_disk in_list
  on_disk=$(ls tools/*.py 2>/dev/null | sed 's|^tools/||; s|\.py$||' | grep -vxF "$not_a_gate" | sort)
  in_list=$({ gates | grep -o 'tools/[a-z0-9-]*\.py'
              sed -n 's|^ *- run: python3 \(tools/[a-z0-9-]*\.py\).*$|\1|p' "$ci"; } \
            | sed 's|^tools/||; s|\.py$||' | sort -u)
  if [ "$on_disk" != "$in_list" ]; then
    echo "gates.sh --check: the gates in tools/ and the gates in this list are not the same set." >&2
    diff <(printf '%s\n' "$in_list") <(printf '%s\n' "$on_disk") \
      | sed 's|^<|    in tools/gates.sh, not in tools/: |; s|^>|    in tools/, in no list at all: |' >&2
    echo "    A gate nothing runs is a gate that does not exist. Add it to the list, or declare it" >&2
    echo "    in not_a_gate above with the reason it is not one." >&2
    bad=1
  fi

  if [ "$bad" = 0 ]; then
    echo "gate-list-check: $(printf '%s\n' $declared_all | wc -l | tr -d ' ') gates, and $ci and $doc both name that set"
  fi
  return "$bad"
}

# ---------------------------------------------------------------------------------------------
# The exit codes, for anything downstream that has to tell a refusal from a red (SKEIN-945)
# ---------------------------------------------------------------------------------------------
#
# Every refusal above is worth nothing if what reads this script turns it back into a red, and that
# is what was happening. `tools/noskip-check.py` carried its own copy of the set — `status in (3, 4)`
# — written when 3 and 4 were all there was. Exit 5 landed (SKEIN-938) and that copy did not learn
# about it; exit 6 landed (SKEIN-941) and it did not learn about that either. A run refused at
# either would have fallen through to "the run reported on no test binary at all" and FAILED THE
# BUILD: the refusal rendered as the very red it exists to prevent, one layer up.
#
# **The tuple was the defect, not its contents.** Writing `3, 4, 5, 6` there fixes today and is
# wrong again at 7 — a second list that is right on the day it is written and silently partial
# afterwards, which is the SKEIN-647 shape this repository has now paid for four times. So the set
# is DERIVED, from the one place a person adding a refusal already writes it down: the exit-code
# list in this file's own header. A code added there is honoured downstream with nobody editing
# anything else, which is the property, and it is the one worth testing for.
#
# **Two things are checked before a single code is printed**, because a derivation that comes back
# with the wrong answer is worse than no derivation:
#
#   * it must parse SOME codes, and some of them must be REFUSALS. An empty set silently turns
#     every refusal back into a red — this bug again, wearing a derivation's clothes — and the two
#     cases print the same nothing unless one of them says so.
#   * the documented set must match the codes this script actually EXITS WITH. A literal `exit <n>`
#     the header does not mention is a refusal nobody downstream can honour; a documented code the
#     body never exits with is a promise this script does not keep. Either way the header has
#     stopped describing the file, and a list read out of a stale comment is a stale list.
#
# `kind` is `refused` for a code whose entry carries the word REFUSED — the same word the banners
# themselves print — and `verdict` otherwise. One word rather than a list of phrases, so a fifth
# refusal spelled a new way is still caught.
do_exit_codes() {
  local doc documented literal only_doc only_code refused_n
  # `#   <n>  <text>`, continued by `#      <more>`, from the header block above.
  doc=$(awk '
    /^# Exit codes\./ { inlist = 1; next }
    inlist && /^#   [0-9]+  / {
      if (code != "") print code "|" text
      code = $2
      line = $0; sub(/^#   [0-9]+  /, "", line); text = line
      next
    }
    inlist && /^#      / { line = $0; sub(/^#     /, "", line); text = text line; next }
    inlist && /^#$/ { next }
    inlist && code != "" { print code "|" text; code = ""; inlist = 0 }
    END { if (code != "") print code "|" text }
  ' "${BASH_SOURCE[0]}")

  refused_n=$(printf '%s\n' "$doc" | grep -c 'REFUSED' || true)
  if [ -z "$doc" ] || [ "$refused_n" = 0 ]; then
    echo "gates.sh --exit-codes: REFUSED — the exit-code list in this file's own header parsed as" >&2
    echo "    $(printf '%s\n' "$doc" | grep -c . || true) code(s), $refused_n of them refusals." >&2
    echo "    Printing that would tell every reader downstream that this script never refuses," >&2
    echo "    which is how a refusal becomes a red (SKEIN-945). Either the header's list is gone" >&2
    echo "    or this parser no longer reads the shape it is written in." >&2
    return 2
  fi

  documented=$(printf '%s\n' "$doc" | cut -d'|' -f1 | sort -un)
  # What the body can actually hand back. `exit $?` and `exit "$fail"` are not literals and are not
  # enumerable; every code this file chooses on purpose is written as one.
  #
  # **Comment lines go first, and both halves of that were learned the hard way.** Reading only
  # line-initial `exit <n>` missed `cd "$root" || exit 2` and would have called a refusal reached
  # that way undocumented; widening it to accept `;`, `&&` and `||` then matched the word `exit`
  # inside the PROSE above, where this very defect is described — a checker reading its own
  # explanation of itself as if it were code. Dropping whole comment lines first is what makes the
  # wider pattern safe, and a comment line here is one that starts with `#`.
  literal=$(grep -vE '^[[:space:]]*#' "${BASH_SOURCE[0]}" \
    | grep -oE '(^[[:space:]]*|[;&|][[:space:]]*)exit [0-9]+' | awk '{print $NF}' | sort -un)
  only_doc=$(comm -23 <(printf '%s\n' "$documented") <(printf '%s\n' "$literal"))
  only_code=$(comm -13 <(printf '%s\n' "$documented") <(printf '%s\n' "$literal"))
  if [ -n "$only_doc" ] || [ -n "$only_code" ]; then
    echo "gates.sh --exit-codes: REFUSED — the header's exit codes and this file's own \`exit\`" >&2
    echo "    statements do not describe the same script." >&2
    [ -n "$only_doc" ] && echo "    documented but never exited with: $(printf '%s' "$only_doc" | tr '\n' ' ')" >&2
    [ -n "$only_code" ] && echo "    exited with but not documented: $(printf '%s' "$only_code" | tr '\n' ' ')" >&2
    echo "    A set read out of a comment is only as good as the comment. Fix whichever is wrong;" >&2
    echo "    until then nothing downstream can be told which codes are refusals (SKEIN-945)." >&2
    return 2
  fi

  printf '%s\n' "$doc" | while IFS='|' read -r code text; do
    case "$text" in
      *REFUSED*) printf '%s|refused|%s\n' "$code" "$text" ;;
      *)         printf '%s|verdict|%s\n' "$code" "$text" ;;
    esac
  done
}

# ---------------------------------------------------------------------------------------------
# Entry points
# ---------------------------------------------------------------------------------------------

case "${1:-}" in
  --list)
    gates
    exit 0
    ;;
  --check)
    resolve_root ""
    do_check
    exit $?
    ;;
  --exit-codes)
    # No worktree needed: the subject is this file's own header, not any tree.
    do_exit_codes
    exit $?
    ;;
  --provision)
    resolve_root "${2:-}"
    provision
    exit 0
    ;;
  --verify)
    # No worktree needed and none resolved: a stream and a receipt are the whole subject, and the
    # tree the run was about may be a worktree that has since gone away.
    do_verify "${2:-}"
    exit $?
    ;;
  run)
    if [ $# -ne 2 ]; then
      echo "gates.sh: usage: tools/gates.sh run <name>" >&2
      exit 2
    fi
    resolve_root ""
    want="$2"
    while IFS= read -r line; do
      if [ "$(field "$line" 1)" = "$want" ]; then
        export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$root/.target}"
        exec bash -c "$(gate_cmd "$line")"
      fi
    done < <(gates)
    echo "gates.sh: no gate named '$want'. tools/gates.sh --list says what there is." >&2
    exit 2
    ;;
  -h|--help)
    # The usage and the exit codes, down to the end of the paragraph that explains the refusal —
    # a boundary read out of the text rather than written as a line number, because `2,50p` was a
    # line number and it had already drifted to a cut mid-sentence. Worst case here is that the
    # anchor goes away and the whole comment header prints, which is verbose and not wrong.
    awk 'NR == 1 { next }
         !/^#/ { exit }
         /^# \*\*The failure report/ { exit }
         { sub(/^# ?/, ""); print }' "${BASH_SOURCE[0]}"
    exit 0
    ;;
  -*)
    echo "gates.sh: unknown option: $1" >&2
    exit 2
    ;;
esac

# ---------------------------------------------------------------------------------------------
# The full run
# ---------------------------------------------------------------------------------------------

resolve_root "${1:-}"

export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$root/.target}"

# ---------------------------------------------------------------------------------------------
# Can these gates be carried out at all? (SKEIN-938)
# ---------------------------------------------------------------------------------------------
#
# A gate whose command begins with a binary that is not on `$PATH` does not fail. It does not RUN.
# bash answers 127, one line goes to that gate's log, `step` reads the non-zero status, and the run
# prints a red. From a shell with no toolchain on its `$PATH`:
#
#     fmt                                      FAILED
#     clippy                                   FAILED
#     test                                     FAILED
#     alone-check                              FAILED
#     noskip-check                             FAILED
#     === SOMETHING FAILED at <sha> ===
#
# Nothing was compiled and nothing was tested. A lane read those five as five breakages and acted
# on them. That is SKEIN-793's incident through a different door — "fifteen gates FAILED having
# executed not one command" — and it gets the same answer: a run that cannot be carried out is
# REFUSED whole, with an exit code of its own, rather than reported as a result. Five FAILED lines
# about commands that never ran are not a weaker kind of red; they are a report about a run that
# did not happen, and a reader cannot tell them apart from the real thing.
#
# **What to check is derived from the list**: the leading word of every declared gate command. A
# written-down list of interpreters beside the list of gates would be right on the day it was
# written and silently partial afterwards, which is the shape SKEIN-647 named and this repository
# has paid for three times. A gate added tomorrow whose command begins with `deno` is preflighted
# for `deno` by this same code, with nobody remembering to say so. And the other half of that
# lesson: a derivation that comes back with NOTHING refuses too, because "no interpreter is
# missing" and "I read no interpreters" otherwise print the same green.
#
# What it does not look at is what a gate reaches for AFTER its first word, and two of the five
# reds above were exactly that: `alone-check` and `noskip-check` begin with `python3`, which was
# present, and run cargo themselves further in. They are covered here only because a missing
# `cargo` refuses the WHOLE run — which is the right granularity anyway. A run that cannot execute
# one declared gate has nothing to say about any of them, and half a run reported as a whole one is
# the thing this file exists to stop.
#
# **`$PATH` is not repaired here, and that is deliberate rather than a limitation.** The header
# above says the environment is inherited untouched. A runner that quietly prepended a toolchain to
# its own `$PATH` would turn a misconfigured shell into a passing run, leaving the lane, the next
# command that lane types, and CI misconfigured and unwarned. So this names the binary, names a
# copy of it if the machine has one, prints the line that would supply it, and stops.

# The leading word of each declared gate command, deduplicated. The leading word is what bash has
# to find before any of the rest of the command means anything.
gate_binaries() {
  local line
  while IFS= read -r line; do
    gate_cmd "$line" | awk '{print $1}'
  done < <(gates) | sort -u
}

# Which gates need it, for a refusal that says what would not have run.
gates_needing() { # gates_needing <binary>
  local line out=""
  while IFS= read -r line; do
    [ "$(gate_cmd "$line" | awk '{print $1}')" = "$1" ] || continue
    out="$out${out:+, }$(field "$line" 1)"
  done < <(gates)
  printf '%s' "$out"
}

# Is it there? A leading word with a `/` in it is a path relative to the worktree root, which is
# where the gates run, rather than a `$PATH` lookup — `tools/gates.sh --check` is one.
have() { # have <leading word>
  case "$1" in
    */*) [ -x "$1" ] ;;
    *)   command -v "$1" >/dev/null 2>&1 ;;
  esac
}

# Where a missing binary actually IS on this machine, discovered at refusal time rather than listed.
# No path belonging to any particular box is written in this file (see the header), and this needs
# none: the environment already names directories — `$CARGO_HOME`, `$RUSTUP_HOME`, anything else
# whose value is an absolute path — and a toolchain is that directory, its `bin`, or a sibling's.
# A shell that exports `RUSTUP_HOME=<somewhere>/toolchain/rustup` is telling this function where
# `<somewhere>/toolchain/cargo/bin/cargo` is without either of them being written down here. On a
# machine where it finds nothing it says nothing was found, which is the true answer and a
# different instruction to the reader.
found_beside_env() { # found_beside_env <name> — directories holding an executable <name>
  local v c d hits=""
  while IFS= read -r v; do
    [ -d "$v" ] || continue
    for c in "$v" "$v/bin" "$v"/../*/bin "$v"/../*; do
      [ -x "$c/$1" ] || continue
      d=$(cd "$c" 2>/dev/null && pwd -P) || continue
      case " $hits " in *" $d "*) continue ;; esac
      hits="$hits $d"
    done
  done < <(printenv | sed -n 's/^[A-Za-z_][A-Za-z0-9_]*=\(\/.*\)$/\1/p')
  [ -n "$hits" ] && printf '%s\n' $hits
  return 0
}

needed=$(gate_binaries)
needed_n=$(printf '%s\n' "$needed" | grep -c .)
# An empty word among several is a different fault from no words at all, and the report says which.
blank=no
[ "$needed_n" -gt 0 ] && printf '%s\n' "$needed" | grep -q '^$' && blank=yes
if [ "$needed_n" = 0 ] || [ "$blank" = yes ]; then
  echo "=== RUN REFUSED: the preflight cannot read the interpreters out of the gate list ==="
  echo "    $(gates | wc -l | tr -d ' ') gate(s) are declared; $needed_n leading command word(s) came back."
  if [ "$blank" = yes ]; then
    echo "    At least one of them is EMPTY: a declared gate whose command has no leading word,"
    echo "    so what that gate would execute is unknown and cannot be preflighted."
  fi
  echo
  echo "    No gate has run. This is the SKEIN-647 arm of this check: a preflight that derives"
  echo "    nothing cannot find anything missing, and would report a clean environment on every"
  echo "    machine including a bare one. It refuses instead. Either the list above has a gate"
  echo "    with no command, or gate_binaries no longer reads the shape the list is written in."
  exit 5
fi

missing=""
for want in $needed; do
  have "$want" || missing="$missing $want"
done
if [ -n "$missing" ]; then
  echo "=== RUN REFUSED: a gate's interpreter is missing, so the gates have not been run ==="
  echo "    worktree: $root"
  echo "    \$PATH as inherited: $PATH"
  for want in $missing; do
    echo
    echo "    $want is not there, and these gates begin with it: $(gates_needing "$want")"
    beside=$(found_beside_env "$want")
    if [ -n "$beside" ]; then
      for dir in $beside; do
        echo "        there is a $want off \$PATH at $dir/$want"
      done
      echo "        the prelude that supplies it, in the shell you run the gates from:"
      echo "            export PATH=\"$(printf '%s\n' $beside | head -1):\$PATH\""
    else
      echo "        and no $want is in any directory this environment points at, so this is not a"
      echo "        \$PATH that lost it — install it, or run the gates where it is installed."
    fi
  done
  echo
  echo "    NO GATE HAS RUN and none is reported above: not one passed, not one failed. Without"
  echo "    this refusal each of those gates would have printed FAILED having executed nothing,"
  echo "    which is a red a reader acts on and a run that did not happen (SKEIN-938, and the same"
  echo "    report SKEIN-793's unwritable log directory produced)."
  echo "    \$PATH is left exactly as it was handed in. A runner that repaired its own environment"
  echo "    would pass here and leave this shell, the next command typed in it, and CI still"
  echo "    misconfigured — with nothing on the terminal to say so."
  exit 5
fi

# ---------------------------------------------------------------------------------------------
# Could this machine CARRY them? (SKEIN-941)
# ---------------------------------------------------------------------------------------------
#
# The block above asks whether a gate's command could START. This one asks whether the machine had
# room to finish it, and it exists because that answer arrived wearing a red's clothes.
#
# Gate run `718272db` reported `test FAILED` across 33 targets on a tree that was green twenty
# minutes earlier and green ten minutes later at the same commit. What it printed:
#
#     run the real skein: Os { code: 2, kind: NotFound }
#     ... every other target failing with "No such file or directory" on its OWN test binary
#         under $CARGO_TARGET_DIR/debug/deps/
#     error: extern location for serde does not exist: .../libserde-<hash>.rlib
#
# and **not one "No space left on device" line anywhere in it**. The single overlay behind
# `/var/tmp`, `/tmp` and `/` had reached 98% with 2.6G free, four lanes' `.target` directories
# holding ~19G of it. That is the whole mechanism, and the reason the word "disk" never appears: a
# build that cannot write an artefact leaves a MISSING FILE, and by the time anything reports, the
# failing syscall is the EXEC of that file, which answers ENOENT. The disk is upstream of the error
# and absent from it. Decisive, inside that same run: `noskip-check` re-ran that very suite minutes
# later and it PASSED.
#
# So thirty-three FAILED lines named thirty-three innocent targets and a reader had no way to tell
# them from the real thing. That is the same family as everything above — a report about a run that
# did not happen — and it gets the same answer: REFUSED, with an exit code of its own.
#
# **What is checked is decisive, and it is deliberately not a number.** A free-space floor was the
# obvious shape and is the wrong one: "under 2G" is right on the day it is written and rots as the
# build grows, and a floor tuned to this box would be a box's path by another name. The question
# with an exact answer is the one the failure itself poses — *the run named a file it had just
# built and could not open it; is that file there?* So the paths under `$CARGO_TARGET_DIR` are read
# out of the failing gate's OWN log and tested for existence. Nothing is remembered, nothing is
# compared against a constant, and a failure whose artefacts are all present is reported as the red
# it is.
#
# **The filesystem is derived from `$CARGO_TARGET_DIR`** and never written down, because no path
# belonging to any particular box may enter this file (see the header) and none is needed: the
# variable names the directory and `df` names the filesystem under it.
#
# **The SKEIN-647 arm, which is the half that is easy to leave out.** The free-space figure is
# printed beside every red, so the quantity this check is about is visible rather than implied. A
# run that could not obtain it and printed the red anyway would be saying "the machine was fine" by
# omission, in exactly the run where it was not — so a red whose capacity question could not be
# asked is refused too, instead of being shown with the answer silently missing.
#
# **What this does NOT cover, said out loud so nobody reads more into a green.** `tools/gates.sh
# run <name>` — the form CI calls, and the form `tools/noskip-check.py` calls — `exec`s the gate's
# command and carries its status, so it reaches none of this. Extending it there means teaching
# `tools/noskip-check.py` about this exit code first: it lists `tools/gates.sh`'s non-verdicts as
# `(3, 4)`, which is already one short of the exit 5 added this morning, and a refusal it does not
# recognise becomes a red one layer up — the very defect this exists for. That file belongs to
# nobody in this change; the gap is named here rather than half-closed.

# The nearest existing ancestor of $CARGO_TARGET_DIR. A path's filesystem is that directory's, and
# in a fresh worktree the target directory itself does not exist until the first build.
target_fs_dir() {
  local d="$CARGO_TARGET_DIR"
  while [ ! -d "$d" ]; do
    case "$d" in /|.|"") return 1 ;; esac
    d=$(dirname "$d")
  done
  printf '%s' "$d"
}

# `<free KiB> <mount point>`, or nothing at all when `df` will not answer for it.
target_fs() {
  local d
  d=$(target_fs_dir) || return 1
  df -Pk "$d" 2>/dev/null | awk 'NR == 2 && $4 ~ /^[0-9]+$/ { print $4, $6; f = 1 } END { exit !f }'
}

human_kib() { # human_kib <KiB>
  [ -n "${1:-}" ] || { printf 'unmeasured'; return 0; }
  awk -v k="$1" 'BEGIN { if (k >= 1048576) printf "%.1fG", k / 1048576; else printf "%.0fM", k / 1024 }'
}

# The paths under $CARGO_TARGET_DIR that a failed gate's log NAMES and that are not on disk.
#
# Matched by prefix with awk's `index()` rather than by a regex, because $CARGO_TARGET_DIR is a
# VALUE and a value dropped into a pattern is a pattern: one `.` or `+` in somebody's path and the
# match quietly becomes a wider one than the reader of this line would expect.
unbuilt_named_in() { # unbuilt_named_in <log>
  awk -v pre="$CARGO_TARGET_DIR/" '
    {
      line = $0
      while ((i = index(line, pre)) > 0) {
        line = substr(line, i)
        n = 0
        while (n < length(line)) {
          c = substr(line, n + 1, 1)
          if (c == " " || c == "\t" || c == "\"" || c == "\047" || c == "`" || c == ")") break
          n++
        }
        p = substr(line, 1, n)
        sub(/[.,:;]+$/, "", p)
        print p
        line = substr(line, n + 1)
      }
    }' "$1" 2>/dev/null | sort -u | while IFS= read -r p; do
      [ -e "$p" ] || printf '%s\n' "$p"
    done
}

# Set by `carried` for `step` to print beside the red it allowed through.
carried_note=""

# Called the moment a gate fails and BEFORE its red is printed. It either refuses the whole run or
# returns, having written the line that puts the free space next to the failure.
carried() { # carried <gate name> <log> <free KiB before that gate, may be empty>
  local name="$1" log="$2" before="$3" now free mount unbuilt n
  now=$(target_fs) || now=""
  if [ -z "$now" ]; then
    echo
    echo "=== RUN REFUSED: '$name' failed and this run cannot say whether the machine carried it === run $run_id"
    echo "    \$CARGO_TARGET_DIR: $CARGO_TARGET_DIR"
    echo "    df would not answer for that path, so there is no free-space figure to put beside"
    echo "    this failure and no way to tell a suite that failed from a machine that could not"
    echo "    hold one."
    echo
    echo "    Not reported as a red, for the reason every refusal in this file gives. A red shown"
    echo "    with the capacity question silently unanswered asserts 'the machine was fine' by"
    echo "    omission — the SKEIN-647 shape, and exactly what SKEIN-941 cost when 33 innocent"
    echo "    targets were reported as broken."
    exit 6
  fi
  free=${now%% *}
  mount=${now##* }
  unbuilt=$(unbuilt_named_in "$log")
  if [ -n "$unbuilt" ]; then
    n=$(printf '%s\n' "$unbuilt" | grep -c .)
    echo
    echo "=== RUN REFUSED: this machine did not carry '$name' === run $run_id"
    echo "    That gate's failure NAMES $n path(s) under \$CARGO_TARGET_DIR that are not on disk:"
    printf '%s\n' "$unbuilt" | head -10 | sed 's/^/        /'
    [ "$n" -gt 10 ] && echo "        (…and $((n - 10)) more — the full log is named below)"
    echo
    echo "    \$CARGO_TARGET_DIR is on $mount, which has $(human_kib "$free") free now${before:+ and had $(human_kib "$before") before this gate}."
    echo "    full log: $log"
    echo
    echo "    The run named a file it had just built and then could not open it. That is not a"
    echo "    suite that failed; it is a build whose artefacts are not there — and the failing"
    echo "    syscall by this point is the exec, which reports NotFound rather than ENOSPC, so the"
    echo "    log can say 'No such file or directory' on every target and never once say 'disk'."
    echo
    echo "    NO GATE IS REPORTED AS FAILING and none of the lines that gate printed are shown:"
    echo "    in the run this was written for, all 33 of them named targets that were fine."
    echo "    Make room — several lanes' \$CARGO_TARGET_DIR on one filesystem is how this happens,"
    echo "    and \`cargo clean\` in the worktrees not in use is the cheapest metre — then run it"
    echo "    again."
    exit 6
  fi
  carried_note="$mount: $(human_kib "$free") free${before:+, $(human_kib "$before") before this gate} — every artefact this gate names is on disk, so this red is a red"
}

# Before the tree is recorded, so what it records is the tree the gates will see (SKEIN-885).
provisioned=$(provision)

# Recorded BEFORE anything runs. Everything printed at the end is about THIS.
head_before=$(git rev-parse HEAD)
short_before=$(git rev-parse --short HEAD)
status_before=$(git status --porcelain)
worktree_before=$(worktree_digest)

# What every line printed about this run names, and it is a TREE (SKEIN-800). A bare sha only when
# the tree is clean, because only then is the sha the whole truth about what ran.
uncommitted=0
[ -n "$status_before" ] && uncommitted=$(printf '%s\n' "$status_before" | wc -l | tr -d ' ')
if [ "$uncommitted" = 0 ]; then
  tested="$short_before"
  tree_tag="clean"
else
  tested="$short_before + $uncommitted uncommitted change(s)"
  tree_tag="dirty$uncommitted-$worktree_before"
fi

# The log directory carries the sha, the state of the working tree over it, and the worktree,
# because two runs used to be indistinguishable once you had only the path — first every log was
# named by its gate and nothing else, and then, with the sha in it, two runs on one base with
# different uncommitted work still were.
LOGS="${GATE_LOGS:-$(mktemp -d "/var/tmp/gatelogs-$short_before-$tree_tag-$(basename "$root")-XXXX")}"

# **And prove it can be written to, before running anything (SKEIN-793).**
#
# `mktemp -d` creates the directory; a caller-supplied `$GATE_LOGS` does not, and nothing else did.
# `step` then opened `>"$LOGS/$slug.log"`, the redirect failed, bash never ran the gate's command at
# all, and the non-zero status of the FAILED REDIRECT was read as the gate failing. The run reported
# fifteen gates FAILED having executed not one command:
#
#     ./tools/gates.sh: line 336: /var/tmp/gatelogs-verify809/fmt.log: No such file or directory
#     fmt                                      FAILED
#     ...  the same fifteen times ...
#     === SOMETHING FAILED at 809506d ===
#
# That is worse than an ordinary red. A red says "your change broke something" and a reader acts on
# it; this said it about a run in which nothing was tested — SKEIN-647's shape one level down. And
# it was reachable by the one thing a CI integration does, which is pass a log directory in.
#
# So: create it, then WRITE to it, because a directory that exists is not the same as a directory
# you may write to — and refuse with a code of its own rather than reporting a gate failure.
cannot_record() { # cannot_record <what the system said>
  echo "=== RUN REFUSED: the logs cannot be written, so there is nothing to report ==="
  echo "    log directory: $LOGS"
  echo "    the system said: ${1:-(nothing)}"
  echo
  echo "    No gate has run. This is not a gate failure and must not be read as one: the runner"
  echo "    cannot record what it is about to do, so it declines to do it. Point \$GATE_LOGS at a"
  echo "    writable directory, or unset it and let the runner make its own."
  exit 4
}
# The reason is captured from the attempt itself rather than by re-running it afterwards — a second
# attempt can fail differently, or succeed, and then the report explains something that did not
# happen.
why=$(mkdir -p "$LOGS" 2>&1) || cannot_record "$why"
why=$( { : >"$LOGS/.gates-writable"; } 2>&1 ) || cannot_record "$why"
rm -f "$LOGS/.gates-writable"

# The id every line of this run carries, and the receipt it is carried in (SKEIN-903). The receipt
# is the copy that an interleaving cannot produce: mixing two streams mixes two streams, and neither
# of them is a file in the other's log directory.
run_id=$(new_run_id)
declared_n=$(gates | wc -l | tr -d ' ')
receipt="$LOGS/receipt"
{
  echo "run $run_id"
  echo "tree $root"
  echo "tested $tested"
  echo "logs $LOGS"
  echo "declared $declared_n"
} >"$receipt" || cannot_record "could not write $receipt"

fail=0
step() {
  local name="$1" cmd="$2" slug matched free_before
  slug=$(printf '%s' "$name" | tr -c 'a-zA-Z0-9' '-')
  # Read BEFORE the gate, so a refusal can say which way the figure moved while it ran (SKEIN-941).
  free_before=$(target_fs) || free_before=""
  free_before=${free_before%% *}
  # The same argument as the check above, for the case where the directory goes away mid-run: a
  # redirect that cannot be opened means the command did not run, and "did not run" is never "failed".
  if ! : >"$LOGS/$slug.log" 2>/dev/null; then
    echo
    echo "=== RUN REFUSED: the logs stopped being writable partway through === run $run_id"
    echo "    could not open $LOGS/$slug.log, so '$name' did not run"
    echo "    Gates reported above did run; this one and every gate after it did not."
    exit 4
  fi
  if bash -c "$cmd" >"$LOGS/$slug.log" 2>&1; then
    printf '%s %-40s ok\n' "$run_id" "$name"
    echo "gate $name ok" >>"$receipt"
  else
    # Before the red is printed at all: a gate that failed because the machine could not hold its
    # artefacts did not fail, and the lines below would name targets that are fine (SKEIN-941).
    carried "$name" "$LOGS/$slug.log" "$free_before"
    printf '%s %-40s FAILED\n' "$run_id" "$name"
    echo "gate $name FAILED" >>"$receipt"
    fail=1
    echo "    $carried_note"
    # The lines that NAME the failure, wherever they are in the log — not its last N lines.
    matched=$(grep -cE "^error|FAILED|^failures:|panicked at|✗|^ *FAIL " "$LOGS/$slug.log")
    grep -nE "^error|FAILED|^failures:|panicked at|✗|^ *FAIL " "$LOGS/$slug.log" | head -60
    if [ "${matched:-0}" -gt 60 ]; then
      echo "    ($matched lines name a failure here; 60 shown — the log has all of them)"
    fi
    echo "    full log: $LOGS/$slug.log"
  fi
}

echo "=== gates for $root at $tested === run $run_id"
# What the preflight above looked for, printed like `tests/ui/harness/leaks.mjs` prints the fixture
# names it derived: a check whose subject is invisible is a check nobody can tell has gone narrow.
echo "$run_id preflight: $needed_n interpreter(s) derived from $declared_n declared gate command(s), each one found: $(printf '%s\n' $needed | tr '\n' ' ' | sed 's/ *$//')"
# What the provisioning did, if anything — a tree this run changed before recording it says so.
[ -n "$provisioned" ] && printf '%s\n' "$provisioned" | sed "s/^/$run_id /"
# And the quantity the capacity check is measured in, printed for the same reason: a check whose
# subject never appears is one nobody can tell has gone narrow (SKEIN-941).
capacity_now=$(target_fs) || capacity_now=""
if [ -n "$capacity_now" ]; then
  echo "$run_id capacity: \$CARGO_TARGET_DIR is on ${capacity_now##* }, $(human_kib "${capacity_now%% *}") free"
else
  echo "$run_id capacity: df will not answer for \$CARGO_TARGET_DIR ($CARGO_TARGET_DIR) — any gate that fails will be REFUSED rather than reported"
fi
while IFS= read -r line; do
  step "$(field "$line" 1)" "$(gate_cmd "$line")"
done < <(gates)
echo "$run_id logs: $LOGS"

# ---------------------------------------------------------------------------------------------
# The verdict, and the refusal
# ---------------------------------------------------------------------------------------------

head_after=$(git rev-parse HEAD)
status_after=$(git status --porcelain)
worktree_after=$(worktree_digest)

if [ "$head_before" != "$head_after" ] || [ "$worktree_before" != "$worktree_after" ]; then
  echo
  echo "=== RESULTS REFUSED: the tree changed while the gates ran === run $run_id"
  echo "    worktree: $root"
  if [ "$head_before" != "$head_after" ]; then
    echo "    HEAD MOVED: $short_before -> $(git rev-parse --short HEAD)"
  else
    echo "    HEAD did not move: $short_before"
  fi
  if [ "$status_before" != "$status_after" ]; then
    echo "    WORKING TREE CHANGED, and these entries of git status --porcelain differ:"
    diff <(printf '%s\n' "$status_before") <(printf '%s\n' "$status_after") \
      | sed -n 's/^[<>]/     &/p'
  elif [ "$worktree_before" != "$worktree_after" ]; then
    echo "    WORKING TREE CHANGED, with git status --porcelain identical at both ends: the"
    echo "    CONTENT of a file that was already modified differs. $worktree_before -> $worktree_after"
    echo "    git diff HEAD says what, and the run that made it is not in these logs."
  else
    echo "    working tree unchanged"
  fi
  echo
  echo "    These results describe a tree you are no longer on. They are neither a pass nor a"
  echo "    failure, and the gate outcomes above must not be quoted as evidence for any commit."
  echo "    Run it again on a tree nobody else is writing to."
  exit 3
fi

# **The verdict is checked against the log directory before it is printed (SKEIN-903).** Everything
# above this point is about the TREE; this is about the RUN. The log directory is namespaced per
# sha, per working-tree digest and per worktree, and it is the one artefact of a run that two
# interleaved streams cannot forge between them — so the last thing that happens before a green is
# to go and read it back.
#
# **What makes it fail:** point `$GATE_LOGS` at one directory from two concurrent runs. The second
# to start truncates the receipt and writes its own id into it; the first then reads an id that is
# not its own and refuses here, instead of printing a green over a directory holding somebody
# else's logs. Proved by doing exactly that, with two throwaway repositories.
#
# Refusal, not failure, and for the reason every other refusal in this file gives: "I cannot tell
# you what this run did" is not "this run found something wrong", and a reader who cannot tell those
# apart acts on the wrong one.
logs_n=$(ls "$LOGS"/*.log 2>/dev/null | wc -l | tr -d ' ')
receipt_run=$(sed -n 's/^run //p' "$receipt" 2>/dev/null)
receipt_gates=$(grep -c '^gate ' "$receipt" 2>/dev/null)
if [ "$receipt_run" != "$run_id" ] || [ "$logs_n" != "$declared_n" ] || [ "$receipt_gates" != "$declared_n" ]; then
  echo
  echo "=== RESULTS REFUSED: this run's own log directory does not describe this run === run $run_id"
  echo "    log directory: $LOGS"
  if [ "$receipt_run" != "$run_id" ]; then
    echo "    ITS RECEIPT SAYS RUN '${receipt_run:-(none)}', NOT $run_id — another run wrote into this"
    echo "    directory while this one was using it. Two runs sharing one \$GATE_LOGS is the same"
    echo "    mistake as two runs sharing one output file (SKEIN-903); give each its own, or unset"
    echo "    \$GATE_LOGS and let each make its own."
  fi
  [ "$logs_n" != "$declared_n" ] && \
    echo "    $declared_n gates were declared and $logs_n log file(s) are on disk"
  [ "$receipt_gates" != "$declared_n" ] && \
    echo "    $declared_n gates were declared and the receipt records $receipt_gates"
  echo
  echo "    No verdict is printed. The gate lines above may each be true and still not add up to a"
  echo "    run, which is exactly what a footer is normally read as proving."
  exit 3
fi

word=$( [ "$fail" = 0 ] && echo ALL GATES GREEN || echo SOMETHING FAILED )
echo "verdict $word" >>"$receipt"
verdict_line "$word" "$tested" "$run_id" "$declared_n" "tools/gates.sh"
exit "$fail"

}
