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
#
# Exit codes, and they are deliberately four rather than two:
#
#   0  every gate passed, against the tree named in the footer
#   1  a gate failed
#   2  this script was called wrongly
#   3  RESULTS REFUSED — the tree moved or changed underneath the run, so the results describe
#      something other than what the footer would name. Not a green, and not a red. See below.
#   4  RUN REFUSED — the logs cannot be written, so there is nothing to report. Also not a red:
#      "I cannot record what I am about to do" is not a gate failure. See SKEIN-793 below.
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
# The order is CI's order, and two positions in it are load-bearing:
#
#   * `alone-check` needs the lib test binary, so it follows `test` and reuses that build.
#   * `citation-check` is last, because it is the only gate that reads git history and CI has to
#     deepen its shallow clone before calling it (see the comment on that step in ci.yml).
gates() {
  printf '%s\n' \
    "fmt|yes|cargo fmt --all -- --check" \
    "clippy|yes|cargo clippy --all-targets --all -- -D warnings" \
    "test|yes|cargo test --all --no-fail-fast" \
    "alone-check|no: 988 processes on top of a build, and the finding is a property of the tests rather than of the change — CONTRIBUTING.md argues this under 'The gate that is not in CI'|python3 tools/alone-check.py" \
    "module-check|yes|python3 tools/module-check.py" \
    "source-check|yes|python3 tools/source-check.py" \
    "env-lock-check|yes|python3 tools/env-lock-check.py" \
    "prose-check|yes|python3 tools/prose-check.py" \
    "line-cite-check|yes|python3 tools/line-cite-check.py" \
    "continuation-check|yes|python3 tools/continuation-check.py" \
    "residue-check|yes|python3 tools/residue-check.py" \
    "cockpit-tests|yes|node --test \"cockpit/test/*.test.mjs\"" \
    "cockpit-bundle|yes|node cockpit/build.mjs --check" \
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

field() { # field <line> <n>
  printf '%s' "$1" | cut -d'|' -f"$2"
}

# The command for a gate is the REST of the line, so a command may contain `|` without the list
# needing an escape.
gate_cmd() { printf '%s' "$1" | cut -d'|' -f3-; }

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
  local not_a_gate="rustcut"
  local on_disk in_list
  on_disk=$(ls tools/*.py 2>/dev/null | sed 's|^tools/||; s|\.py$||' | grep -vxF "$not_a_gate" | sort)
  in_list=$(gates | grep -o 'tools/[a-z0-9-]*\.py' | sed 's|^tools/||; s|\.py$||' | sort -u)
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

fail=0
step() {
  local name="$1" cmd="$2" slug matched
  slug=$(printf '%s' "$name" | tr -c 'a-zA-Z0-9' '-')
  # The same argument as the check above, for the case where the directory goes away mid-run: a
  # redirect that cannot be opened means the command did not run, and "did not run" is never "failed".
  if ! : >"$LOGS/$slug.log" 2>/dev/null; then
    echo
    echo "=== RUN REFUSED: the logs stopped being writable partway through ==="
    echo "    could not open $LOGS/$slug.log, so '$name' did not run"
    echo "    Gates reported above did run; this one and every gate after it did not."
    exit 4
  fi
  if bash -c "$cmd" >"$LOGS/$slug.log" 2>&1; then
    printf '%-40s ok\n' "$name"
  else
    printf '%-40s FAILED\n' "$name"
    fail=1
    # The lines that NAME the failure, wherever they are in the log — not its last N lines.
    matched=$(grep -cE "^error|FAILED|^failures:|panicked at|✗|^ *FAIL " "$LOGS/$slug.log")
    grep -nE "^error|FAILED|^failures:|panicked at|✗|^ *FAIL " "$LOGS/$slug.log" | head -60
    if [ "${matched:-0}" -gt 60 ]; then
      echo "    ($matched lines name a failure here; 60 shown — the log has all of them)"
    fi
    echo "    full log: $LOGS/$slug.log"
  fi
}

echo "=== gates for $root at $tested ==="
while IFS= read -r line; do
  step "$(field "$line" 1)" "$(gate_cmd "$line")"
done < <(gates)
echo "logs: $LOGS"

# ---------------------------------------------------------------------------------------------
# The verdict, and the refusal
# ---------------------------------------------------------------------------------------------

head_after=$(git rev-parse HEAD)
status_after=$(git status --porcelain)
worktree_after=$(worktree_digest)

if [ "$head_before" != "$head_after" ] || [ "$worktree_before" != "$worktree_after" ]; then
  echo
  echo "=== RESULTS REFUSED: the tree changed while the gates ran ==="
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

echo "=== $( [ $fail = 0 ] && echo ALL GATES GREEN || echo SOMETHING FAILED ) at $tested ==="
exit "$fail"

}
