# Where skein tells a person something went wrong — and whether they can get out

A survey, not a fix. SKEIN-752 states the standard, in the owner's words:

> *"leaving user with no way to recover is the worst thing that can be done right?"*
> *"we should at least tell user, what they are supposed to do."*
> *"and once they do that, we should detect and connect or whatever."*

Three requirements, in the order they get missed:

1. **Say what happened.** Largely done here, and the reason this document is mostly about the
   other two.
2. **Say what to do about it.** Often missing.
3. **Notice when they have done it, and recover unasked.** Almost never done.

**"Try again" is the fallback, not the mechanism.** A surface must either *be watching for
something specific and say what*, or *not be watching and offer a control*. A spinner that waits
for nothing is worse than a button.

**A path that cannot say what to do next is a defect in that path, not a constraint to design
around.** Nothing below is marked "no next step possible" as an acceptable state; where a path has
none, that is the finding.

---

## Summary

**243 rows**, drawn from **465 candidate sites** enumerated by the twelve commands in
[§0](#0-how-this-was-enumerated). Rows are fewer than candidates because identical strings are
collapsed into one row that names every site (thirteen `invalid box name` returns are one row), and
because the candidate sets deliberately over-collect — an enumeration that only catches what is
already known to be a message catches nothing new. Of those rows:

| verdict | rows | what it means |
|---|---:|---|
| **C** — conforms | 57 | says what to do, and either the condition is already watched or there is genuinely nothing to watch because the person's own next keystroke is the recovery |
| **W** — says what to do; watchable, not watched | 42 | requirement 2 met, requirement 3 open. A named, cheap condition exists and nothing polls it |
| **N** — names no next step | 131 | the finding. States a failure and stops |
| **U** — says what to do, and it cannot be watched | 12 | the act is off this machine, or it is a person's deliberate "no" |

One further row, `src/fleet.rs:4027`, carries no verdict: it is a well-written refusal that
`src/health.rs:241` swallows, so nobody ever reads it.

**Requirement 1 is met almost everywhere and requirement 3 almost nowhere.** 99 rows say what to
do; 42 of those name a condition skein could watch and does not, and only a handful of the
remaining 57 are watched because somebody built the watching — the health poll, `revStaleTimer`,
the accept-loop retry, and the fleet-create lease.

**The fourth verdict, `W`, is mine and not the brief's**, and it is the whole shape of the gap.
The brief offers *conforms* / *names no next step* / *says what to do but cannot be watched*.
Collapsing `W` into *conforms* would hide requirement 3 entirely — 42 surfaces name a step and then
sit there — and collapsing it into *cannot be watched* would be false, because in every one of the
42 the condition is named in the row and is a poll skein already knows how to write.

### The three worst offenders

Ranked by reachability × severity — how easily an ordinary session lands here, times how stuck the
person is when it does.

**1. The terminal reconnect overlay waits for a click, over a sentence it hides.**
`src/web/index.html:2567` paints *"session not connected / click to reconnect"* on any box terminal
whose socket drops with a code that is not `CLOSE_CHILD_ENDED`. There is no retry timer:
`reconnectSession` has exactly four call sites — `src/web/index.html:2129`, `:2524`, `:2568`
and `:10710` — and every one of them is a person acting: a restart, a tab click, the overlay
itself, a file drop. Worse,
the six failure sentences in `terminal_session`, `login_session` and `pump_pty`
(`src/bin/skein-server.rs:4253`, `:4307`, `:4491`, `:4501`, `:4512`, `:4521`) are written to the
socket and then the socket is *dropped without a close code* — so the browser reads 1006, decides
the connection went away, and lays the 82%-opaque card over the one line that said why. This is
SKEIN-702's defect, still open, and it is the most reachable failure in the product: a server
restart, a stopped box or an exhausted PTY pool all land here.

**2. `no fleet sandbox configured`, six identical times, saying nothing.**
`src/fleet.rs:1976`, `:5112`, `:6941`, `:6998`, `:8329`, `:8346` — six copies of a four-word noun
phrase, plus two near-variants at `:5781` and `:6152`. It reaches a person through `skein start`,
`skein login`, `skein save` and the cockpit's login terminal (`src/bin/skein-server.rs:4768`, where
it is the entire message). It names no setting, no page, no command and no `skein doctor`. The
watchable condition is one line — `load_config().fleet_sandbox` becoming non-empty.

**3. `skein doctor` reports the three things a box cannot start without, and does not say what to
do about any of them.** `src/bin/skein.rs:1002` prints *"{tool} missing in the sandbox — {why}"* for
`bwrap`, `tmux` and `git`, and `:1012` prints *"no cgroup delegation — boxes run UNCAPPED, so one
runaway build can kill every other box"*. Both are diagnoses of a fleet that cannot work, printed
by the command a person runs precisely because nothing else is working, with no next line. Note
that `skein doctor` prints `HealthCheck::unsatisfied`'s `fix` field wherever it has one
(`src/bin/skein.rs:504`) — these four faults are the ones that bypass that machinery by being
hand-rolled `println!`s.

### The one I would fix first

**Offender 1** — and specifically its second half, the missing close code on the six refusal paths
in `src/bin/skein-server.rs`. Reason: it is the only one of the three where skein *already wrote the
right sentence* and then destroyed it. `absent_box_reason` (`src/fleet.rs:7931`) is the best
recovery message in the tree — it names the exact command, and it names the command *not* to run —
and a person reaching it through the cockpit sees "session not connected" instead. Fixing the
overlay's auto-retry is the larger win; fixing the close code is the one that turns work already
done into work a person can see, and it is decided already (SKEIN-702).

### One-line and egregious

Three, noted for the owner to rule on, not fixed here:

- `src/bin/skein.rs:1317` and `:1537` say *"unsupported runtime {x}"*. `src/bin/skein.rs:1563`, in
  the same file, says *"unsupported runtime {x}; available: {list}"*. Two of the three ways to name
  a runtime withhold the list the third one prints, from the same `supported_runtimes`.
- `src/bin/skein.rs:486` prints *"{prog} not on PATH — {why} unavailable"* and stops, while
  `src/health.rs:975` answers the identical condition with *"install {name}, or start the server
  from a shell whose PATH has it"*. The fix exists; this line does not reach for it.
- `src/bin/skein-server.rs:908` and twelve sibling sites return *"invalid box name"* with no
  grammar. `warden/src/serve.rs:496` answers the same class of input with the grammar spelled out.

---

## 0. How this was enumerated

Twelve enumerations, each counted whole before anything was concluded from it. Run from the
repository root; the numbers below are what they printed.

```sh
grep -rn 'HealthCheck::unsatisfied(' src/ --include='*.rs' | wc -l                    # 18
grep -rn 'HealthCheck::unknown('     src/ --include='*.rs' | wc -l                    # 20
grep -c  '{BAD}\|{WARN}'             src/bin/skein.rs                                 # 24
grep -c  'Err("\|Err(format!'        src/bin/skein.rs                                 # 21
grep -c  'eprintln!'                 src/bin/skein-server.rs                          # 22
grep -cE '\(StatusCode::[A-Z_]+, "'  src/bin/skein-server.rs                          # 32
grep -cE 'toast\((`|")[^`"]*(fail|could not|couldn|not connected|refused|blocked|did not|went wrong)' src/web/index.html   # 32
grep -cE '(innerHTML|textContent) *= *[^;]*(could not|couldn|fail|error|not read|no session|blocked)' src/web/index.html   # 14
grep -rcE 'Response::fault\(|Err\(format!' warden/src/*.rs | awk -F: '{s+=$2} END {print s}'   # 41
grep -cE 'error|failed|refus'        src/board.rs                                     # 7
grep -rnE 'Err\((format!\(|")'       src/*.rs | wc -l                                 # 200
grep -n  'eprintln!'                 src/fleet.rs | awk -F: '$1<8589' | wc -l         # 34
```

**465 candidate sites.** The 200 split 146 `format!` and 54 literal, and the two patterns are
disjoint. The `src/*.rs` glob is flat, so `src/bin/` is excluded from it automatically and counted
separately above; `src/fleet.rs`'s test module begins at line 8589, which is what the `awk` bounds.

191 of those 200 are outside `#[cfg(test)]`. They are not all rows: the test applied was *the
function is `pub`, and its `Err` is either returned to `main` in `src/bin/skein.rs`, which prints
it at `src/bin/skein.rs:152`, or returned from an axum handler as a body the cockpit renders.*
Where neither could be traced, the row says **not established** rather than guessing. The pure
input-validation guards behind `pub(crate)` writers are named in [§10](#10-the-rest-of-src) as
excluded, with the reason.

**What counts as a person seeing it.** A message on a startup path a person watches is in scope; a
line from a background ticker that only ever reaches a detached server's stderr is not. Where the
two cannot be told apart from the code, the row says so.

Every `file:line` below was re-read with `sed -n 'Np'` before it was written down.

### The rubric

| column | meaning |
|---|---|
| where | `file:line`, verified against the tree |
| what a person sees | the sentence, verbatim or elided at `…` |
| reachable how | the concrete thing a person does, or **not established** |
| to do? | **y** / **n** / **partly**, with the words that do it |
| watchable? | the condition skein could detect — named, or **none** with the reason |
| v | **C** conforms · **W** says what to do, watchable, not watched · **N** names no next step · **U** says what to do and it cannot be watched |

---

## 1. `HealthCheck::unsatisfied` — the model, and it holds

This is the shape the rest of the tree should copy. `src/health.rs:85` takes the fix as its second
*argument*, so "a fault with no way out cannot be written without noticing", and `src/health.rs:63`
documents the field as never empty for an unsatisfied check. Twelve production sites, and **all
twelve carry a real fix**.

They are also the only family in this survey that satisfies requirement 3 by construction:
`src/web/index.html:11517` re-polls `/api/health` every 15 seconds, `loadHealth` removes the banner
when `health.ok`, and `skein doctor` prints the same `fix` at `src/bin/skein.rs:504`. Recovery is
detected and the surface clears with nobody pressing anything.

| where | what a person sees | reachable how | to do? | watchable? | v |
|---|---|---|---|---|---|
| `src/health.rs:320` | the boxes' disk is N% full … | health banner / `skein doctor`, above `DISK_FULL_PCT` | y — "`skein stop <box>` keeps its checkout, branch and conversation, or clear its build output in place"; the image-store arm adds a `docker system prune -af` line and is marked `destroys` | yes — the next 15s poll re-measures | C |
| `src/health.rs:407` | {path} in {whose} belongs to uid {owner}, and the fleet runs as uid {mine} — a `claude` that derives its own temp directory refuses to start there | health banner; a shared `/tmp` taken by another uid | y — names `rm -rf {path}` on that machine and says why skein will not do it | yes — re-derived each poll | C |
| `src/health.rs:680` | the host warden did not answer … | health banner; warden not running | y — two arms: "something is answering on {addr} and it is not a warden this skein can use", with `$SKEIN_WARDEN` and `$SKEIN_WARDEN_PORT` named | yes — `warden_report` on the poll; also printed once at startup, `src/bin/skein-server.rs:230` | C |
| `src/health.rs:746` | started before the current isolation and still running under the old one: {boxes} | health banner after an upgrade | y — "`skein restart {box}` … Its checkout and its branch are untouched; whatever the agent was part-way through is not, so pick the moment" | yes — `cover_is_current`, recomputed each poll | C |
| `src/health.rs:816` | ON but nothing is scoped, so every box holds … | health banner; gitgate on with no usable credential | y — "Settings → GitHub & keys → add a GitHub App, or a per-repo token for each repo in use" | yes — `scope_status` on the poll | C |
| `src/health.rs:890` | … The kernel has killed N process(es) for memory since skein last looked | health banner after an OOM kill | y — "give the fleet more memory (Settings → Fleet), or stop a box you are not …" | yes — the counter re-read each poll | C |
| `src/health.rs:925` | no memory ceiling anywhere: not per box, not on the boxes together, not on Docker … | health banner on a fleet created without limits | y — "`skein resize 26g` (or Settings → Fleet → memory; it rebuilds the sandbox and carries every box's work across)", marked `destroys` | yes, but the fix is destructive and must not be driven | C |
| `src/health.rs:962` | {registry error} | health banner; `$SKEIN_REGISTRY` pointing at an unreadable file | y — "it is named by $SKEIN_REGISTRY or $SKEIN_SHARED — unset whichever is set, or point it at a readable file" | yes | C |
| `src/health.rs:973` | `{name}` is not on PATH, and skein needs it | health banner; `git` missing | y — "install {name}, or start the server from a shell whose PATH has it" | yes — `program_on_path` each poll | C |
| `src/health.rs:988` | curl is not installed, and skein reads GitHub with it — pull requests, diffs, merges, and minting App tokens | health banner | y — "install curl" | yes — `have_curl` each poll | C |
| `src/health.rs:1078` | {box} durable agent guidance is unavailable; … | health banner; probes not installed | y — "restart the server, which recreates every repo's store and reinstalls the probes into it; a box that is missing tmux or jq needs `skein restart <box>` after that" | yes | C |
| `src/health.rs:1088` | {mailbox errors} | health banner | y — "a missing mailbox directory is created by restarting the server; a box missing jq needs `skein restart <box>`, which reprovisions it" | yes | C |

## 2. `HealthCheck::unknown` — the fix field is empty by construction

`src/health.rs:94` documents `unknown` as "could not be answered — `detail` says why it could not,
not what is wrong", and `src/health.rs:1310` asserts the `fix` is empty. That is a defensible rule
for *not a fault*, and it is also how a person ends up reading a `!` on their board with nothing
under it. Every row here is watched by the same 15s poll, so requirement 3 is met and requirement 2
is not.

| where | what a person sees | reachable how | to do? | watchable? | v |
|---|---|---|---|---|---|
| `src/health.rs:139` | no fleet sandbox is configured, so there is no filesystem to measure | health banner; blank `fleet_sandbox` | n | yes — config gaining a name | N |
| `src/health.rs:253` | the sandbox is not answering, so its disk figures are the last ones that arrived — and they carry no total | health banner while `sbx` is wedged | n | yes — `sbx` answering; already polled | N |
| `src/health.rs:392` | {whose} could not be asked whether anything has taken the model's temp directory ({why}) | health banner | n | yes — the probe answering | N |
| `src/health.rs:422` | {whose} answered something this cannot read ({line}), so whether anything has taken the model's temp directory is unknown | health banner | n | yes | N |
| `src/health.rs:602` | `sbx ls` did not answer just now; showing the last successful snapshot (N boxes) | board / health banner | n — but it says which reading it is showing, which is the honest half | yes — the next `sbx ls` | N |
| `src/announce.rs:787` | no fleet sandbox is configured | not established — composed into a note for an agent, not obviously a person's surface | n | yes | N |
| `src/bin/skein-server.rs:2838` (+ 11 siblings, `:2841`–`:2851`) | the health check itself failed: {error} — and eleven checks reading "the health check itself failed" | health banner, whenever `health_report` panics | n — a person is told every subsystem is unknown and given nothing | yes, and already: `ok: false` keeps the banner up and the 15s poll clears it when the task next succeeds | N |

## 3. `skein doctor`

`cmd_doctor` is a one-shot report, so requirement 3 reads differently: re-running the command *is*
the poll. Where a doctor line reproduces a `HealthCheck`, it prints that check's `fix`
(`src/bin/skein.rs:504`, `:572`, `:667`, `:711`, `:730`) and the row conforms by inheritance. The
rows below are doctor's own hand-rolled lines.

| where | what a person sees | reachable how | to do? | watchable? | v |
|---|---|---|---|---|---|
| `src/bin/skein.rs:443` | ✗ registry {error} | `skein doctor` with an unreadable registry | partly — the next line prints `registry_origin`, which is where, not what to do | yes — the file becoming readable | N |
| `src/bin/skein.rs:486` | ✗ {prog} not on PATH — {why} unavailable | `skein doctor` without `git` or `curl` | **n** — and `src/health.rs:975` answers the same condition with a fix | yes | N |
| `src/bin/skein.rs:533` | ✗ model {runtimes} on, and {reason} | `skein doctor` with a model configured that will not answer | partly — `src/bin/skein.rs:546` prints the transport's own detail, including the whole PATH, where there is room | yes — a test call answering | W |
| `src/bin/skein.rs:599` | ! github token none — the review queue reads PRs as you, and nothing here names a user | `skein doctor` with no GitHub credential | y — "`gh auth login` on this host, export GH_TOKEN, or add a read token in Settings → GitHub & keys" | yes | C |
| `src/bin/skein.rs:634` | ✗ logins {runtime} expired {date} | `skein doctor` after a fleet-wide logout | y (`:635`) — "every box holds the same dead token … one `skein login {runtime}` heals the whole fleet" | yes — and the cockpit already watches it, see §7 | C |
| `src/bin/skein.rs:646` | ! logins none — `skein login <runtime>` signs the fleet in once | `skein doctor` on a fleet with no login | y — the command is the message | yes | C |
| `src/bin/skein.rs:686` | ✗ box ceilings N running outside the fleet's ceiling: {boxes} | `skein doctor` | y (`:693`) — "a runaway build in one of these reaches the whole sandbox; `skein restart <box>` puts it under the current plan" | yes — `uncapped_boxes` | C |
| `src/bin/skein.rs:772` | ✗ review {repos}: {why} | `skein doctor` with a repo whose queue will not build | **n** | yes — the next queue build; the cockpit's own copy of this at `src/web/index.html:3598` does add "try again" | N |
| `src/bin/skein.rs:783` | ! kit not written yet (server startup / `skein add` installs it) | `skein doctor` before first start | y — names both things that install it | yes — the file appearing | C |
| `src/bin/skein.rs:790` | ✗ settings unreadable — {why} | `skein doctor` with a malformed config | partly (`:791`) — "every setting below is a fallback default, not your choice; skein has not overwritten the file". Says what is true, not what to do | yes — the file parsing | N |
| `src/bin/skein.rs:819` | ! boxes push nothing chosen — no GitHub credential is placed in a box | `skein doctor` | y (`:826`) — "Settings → GitHub & keys: a GitHub App, or a per-repo token" | yes | C |
| `src/bin/skein.rs:845` | ! gh secret not seeded, and nothing can seed one … | `skein doctor` with `seed_gh_secret` on | y — "Scope per repo instead — Settings → GitHub & keys" | yes | C |
| `src/bin/skein.rs:867` | ! ssh agent no keys loaded (SSH git push from boxes will fail …) | `skein doctor` | y — "run `ssh-add <key>` on the host; the key file is there and skein cannot read it from in here" | no — the act is on the host, off this machine | U |
| `src/bin/skein.rs:888` | ✗ fleet no sandbox named, which load_config is supposed to make impossible — the board will show this fleet as empty | unreachable by construction; the comment at `:882` says `load_config` repairs a blank name and the line is kept as an invariant tripwire | n, deliberately | n/a | N |
| `src/bin/skein.rs:911` | ✗ sandbox reported absent, which cannot be true — skein is running inside this fleet, so this is a bug in the check and not a fact about the fleet | unreachable by construction (`:895` argues why) | n, deliberately | n/a | N |
| `src/bin/skein.rs:915` | ✗ sandbox cannot tell if it exists — {sbx failure} | `skein doctor` with a wedged `sbx` | **n** | yes — `fleet_exists` answering | N |
| `src/bin/skein.rs:944` | ! create line cannot be worked out — {why} | `skein doctor` | **n**, and this is the line a person is sent to when the cockpit is down | yes — `create_line` succeeding | N |
| `src/bin/skein.rs:964` | ! this sandbox now reports {n} CPUs, so it is not the one that was approved | `skein doctor` on a fleet whose shape changed | n | yes — the recorded size matching | N |
| `src/bin/skein.rs:971` | ! fleet size nobody stated this fleet's memory or CPUs; it has {n} CPUs | `skein doctor` | partly (`:973`) — "sbx fixes both at create and has no resize, so changing them means rebuilding the sandbox" is a constraint, not a step | no — it is a fact about `sbx`, not a condition | U |
| `src/bin/skein.rs:1002` | ✗ {tool} missing in the sandbox — {why} | `skein doctor` on a fleet without `bwrap`, `tmux` or `git` | **n** — and each `{why}` says a box cannot work without it | yes — the probe finding it | N |
| `src/bin/skein.rs:1012` | ✗ ceilings no cgroup delegation — boxes run UNCAPPED, so one runaway build can kill every other box | `skein doctor` | **n** | yes — the `mkdir` probe succeeding | N |
| `src/bin/skein.rs:1027` | ✗ {cgroup} {said} | `skein doctor`; text comes from `ceiling_reading` | not established — the sentence is `fleet`'s, and the comment at `:1022` says the judgement moved there deliberately | yes — the cgroup file being rewritten | N |
| `src/bin/skein.rs:1088` / `:1096` | ✗ mount {path} — not visible in the sandbox; boxes for it would come up with no store / NOT MOUNTED, though directories under it are … | `skein doctor` on a fleet created with a short create line | y — both arms end with "`skein resize {size}` rebuilds it with the create line above and carries every box across" | no — the rebuild is a host act | U |
| `src/bin/skein.rs:1106` | ! mount {path} — the directory is there, but nothing in this namespace is mounted at it | `skein doctor` run from inside a box | y — "Run this at fleet scope rather than inside a box" | yes — trivially, the scope it is run at | C |
| `src/bin/skein.rs:1128` | ! review loop {what this repo has built} | `skein doctor` with automatic review plus a merge train | not established — text is `the_loop_this_repo_has_built`'s | yes — a settings change | W |

## 4. The CLI funnel

Every subcommand's `Err(String)` lands at `src/bin/skein.rs:152`, printed as `skein: {e}` before
`exit(1)`. Twenty-one literal sites; the eight `usage:` strings are one row because they are the
same shape and all of them conform.

| where | what a person sees | reachable how | to do? | watchable? | v |
|---|---|---|---|---|---|
| `src/bin/skein.rs:43` (+ `:48`, `:70`, `:116`, `:124`, `:239`, `:247`, `:254`) | usage: skein add `<git-url>` [--id `<id>`] [--agent `<runtime>`] … | mistyping any of eight subcommands | y — the usage line is the step | no — the person's own next keystroke is the recovery | C |
| `src/bin/skein.rs:129` | no fleet sandbox is configured (fleet_sandbox in config.json) | `skein cockpit-stop` with a blank sandbox | partly — names the field, not how to set it or where | yes — config gaining a name | N |
| `src/bin/skein.rs:148` | unknown command {cmd} (try: skein help) | mistyping a verb | y | no — a keystroke | C |
| `src/bin/skein.rs:242` | unknown shared action {action}; usage: skein shared import `<box>` | `skein shared <anything else>` | y | no — a keystroke | C |
| `src/bin/skein.rs:1247` | could not refresh {repos} — a box created now clones from the remote instead | `skein pull` with an unreachable remote | partly — states the consequence, not a step | yes — `fetch_mirror` succeeding | N |
| `src/bin/skein.rs:1313` | no branch for box {name}; pass --branch `<branch>` | `skein start` on a box with no recorded branch | y | no — a keystroke | C |
| `src/bin/skein.rs:1317` | unsupported runtime {agent} | `skein start --agent <typo>` | **n** — and `:1563` prints the available list for the identical check | no — a keystroke, but the person cannot make it without the list | N |
| `src/bin/skein.rs:1347` | nothing to run | not established — an empty argv from `attach_argv` | n | none | N |
| `src/bin/skein.rs:1351` | {program} exited non-zero | `skein attach` where the attach command fails | **n** — no output, no exit code, no next step | yes — the box's session existing | N |
| `src/bin/skein.rs:1353` | sbx not found on PATH — attaching from the host needs the sbx CLI | `skein attach` on a host without `sbx` | y — names what is needed | yes — `sbx` appearing on PATH | W |
| `src/bin/skein.rs:1356` | running {program}: {io error} | `skein attach` with a broken program | n | yes | N |
| `src/bin/skein.rs:1469` | N of M boxes could not be copied out ({names}) — that work is still only inside {sandbox}, and a destroy would take it | `skein save` with a partial failure | partly — the stake is stated plainly; no per-box retry is named | yes — a re-run of `save_boxes` for the named boxes | N |
| `src/bin/skein.rs:1502` | skein is running inside the fleet sandbox, so it cannot resize it from here — and no sandbox is named in the settings … `skein doctor` prints what it can work out about this installation | `skein resize` with no sandbox configured | y | no — the act is on the host | U |
| `src/bin/skein.rs:1526` | {refusal}{note} — the `fleet_lifecycle_refusal` text plus "The size you asked for ({asked}) is not in that create line … Edit the flags in the line to the size you want." | `skein resize` | y — the refusal renders the destroy and create lines to run on the host | no — off this machine | U |
| `src/bin/skein.rs:1537` | unsupported runtime {runtime} | `skein login <typo>` | **n** — same omission as `:1317` | no | N |
| `src/bin/skein.rs:1563` | unsupported runtime {agent}; available: {list} | `skein attach --agent <typo>` | y — the list is the step | no — a keystroke | C |

## 5. The websocket refusals — `terminal_session`, `login_session`, `pump_pty`

**Already owned: SKEIN-702**, which is decided and blocked only on file ownership. Included so the
table is complete, and because measuring them changed one thing: the defect is not only that some
say nothing, it is that *none of the six early returns sends a close code*, so
`src/web/index.html:2645` reads 1006, decides the connection went away, and covers the sentence with
the reconnect card. `CLOSE_CHILD_ENDED` (`src/bin/skein-server.rs:4469`) is only sent at `:4405`,
which is reached solely when `pump_pty` returned a child's exit code.

| where | what a person sees | reachable how | to do? | watchable? | v |
|---|---|---|---|---|---|
| `src/bin/skein-server.rs:4253` | skein: too many terminals open — close one and retry | opening a box terminal with every `PTY_LIMIT` permit held | partly — "close one and retry" is manual | **yes, the cleanest in the tree** — a permit freeing. `try_acquire` is deliberate (`src/bin/skein-server.rs:4248`); a bounded wait would recover with nobody involved | W |
| `src/bin/skein-server.rs:4307` | skein: box {name} does not exist … Run `skein start {name} --branch <branch>` on the host to try again. Do not run `sbx create` … | opening a terminal for a box with no placement | y — the best recovery sentence in the tree, and it names an anti-command too | yes — `shared_record` becoming `Some`; the browser already reconnects | W |
| `src/bin/skein-server.rs:4273` | skein: handoff brief failed: {e} | opening a terminal with `?handoff=1` when `prepare_handoff` errors | n — and the session then proceeds *without* the brief | not established — what recovery would mean here is unclear from the code | N |
| `src/bin/skein-server.rs:4280` | skein: handoff task failed: {e} | same, on a join error | n | not established | N |
| `src/bin/skein-server.rs:4314` | skein: {e} from `ensure_box_session` | reattaching to a box whose tmux server died with its sandbox | n — a bare error, and the attach then usually fails again | yes — the box's session existing | N |
| `src/bin/skein-server.rs:4395` | skein: {why} from `remember_launch_never_ran` | a create-a-box launch whose command never ran | inherits — this one *is* followed by the close code | yes | W |
| `src/bin/skein-server.rs:4491` | skein: pty error: {e} | any terminal when `openpty` fails — host out of ptys | **n** | yes — a pty slot freeing; nothing was spawned, so a backoff retry is safe | N |
| `src/bin/skein-server.rs:4501` | skein: spawn failed: {e} | `sbx` absent, or `$SKEIN_ATTACH_CMD` naming a missing program | **n** | yes — the named program appearing on PATH | N |
| `src/bin/skein-server.rs:4512` | skein: pty reader: {e} | `try_clone_reader` failing after the child spawned | **n** | yes — fd pressure easing | N |
| `src/bin/skein-server.rs:4521` | skein: pty writer: {e} | `take_writer` failing | **n** | yes | N |
| `src/bin/skein-server.rs:4758` | skein: too many terminals open — close one and retry | clicking "log in" with every permit held | partly | yes — a permit freeing | W |
| `src/bin/skein-server.rs:4768` | skein: no fleet sandbox configured | clicking "log in" on a fleet with no sandbox | **n** — four words, no variable, no page, no command. `src/fleet.rs:8346` is where it is written | yes — `fleet_sandbox()` becoming non-empty | N |
| `src/bin/skein-server.rs:4798` | logged in, but the post-login share failed: {e} | finishing a cockpit login when `share_login_with_boxes` fails | n — the person cannot tell whether running boxes have the credential | **yes, and it is already fixed silently**: `heal_logins` (`src/bin/skein-server.rs:160`) runs a 60s ticker that repairs exactly this. The message does not say so | N |
| `src/bin/skein-server.rs:4809` | skein: login exited {code} — nothing changed | aborting the runtime's login TUI | partly — "nothing changed" closes the loop | no — a person's own decision | U |

## 6. The server — startup stderr and HTTP bodies

A person watches `skein-server`'s startup output; the background tickers' lines reach a log. The
startup family shares one shape: **what broke, plus what will be worse later, and no step**. Line
`:230` is the only one that appends a `fix`, and it does it by reaching for
`health::warden_report().fix` — the machinery §1 describes.

| where | what a person sees | reachable how | to do? | watchable? | v |
|---|---|---|---|---|---|
| `src/bin/skein-server.rs:83` | skein-server: {argv complaint}, then exit 2 | a bad flag | y — the message is a usage complaint | no — a keystroke | C |
| `src/bin/skein-server.rs:94` | skein-server: {volume refusal}, then exit 1 | starting against a moved or unreadable volume; text from `ensure_volume` | y — see §10, `src/volume.rs:104` and siblings name both branches | yes — `$SKEIN_HOME` matching | W |
| `src/bin/skein-server.rs:100` | skein: turn-state probe not installed ({e}); boxes will show live/stale only | server start | n | yes — the probe file appearing | N |
| `src/bin/skein-server.rs:106` | skein: ensure_fleet_kit: {e} | server start | **n**, and it is the only startup line with neither a consequence nor a step | yes | N |
| `src/bin/skein-server.rs:109` | skein: kit not installed ({e}); boxes will fail to provision | server start | n | yes | N |
| `src/bin/skein-server.rs:116` | skein: could not heal the fleet sandbox ({e}); boxes may start with a stale launcher or stale ceilings | server start | n | yes | N |
| `src/bin/skein-server.rs:135` | skein: the docker watchdog could not be started ({e}); a dockerd that dies will stay dead and cost a fleet rebuild | server start | n — highest stake of the startup family | yes | N |
| `src/bin/skein-server.rs:230` | skein: {warden failure} then the `warden_report` fix | server start with no warden | y — inherited from §1 | yes | C |
| `src/bin/skein-server.rs:240` | skein: ssh key not loaded ({e}); SSH git push from boxes may fail | server start | n | yes — `ssh-add -l` listing a key | N |
| `src/bin/skein-server.rs:523` | {doorway::missing()}, then exit 1 | server start without the doorway | not established — text is `doorway`'s | yes — the doorway appearing | W |
| `src/bin/skein-server.rs:533` | skein-server: {why} for a bad inherited fd, then exit 1 | not established — a supervisor handing a bad fd | n | none | N |
| `src/bin/skein-server.rs:600` | skein-server: cannot accept connections ({e}) — N in a row. The usual cause is running out of file descriptors; the cockpit keeps trying. | a server under fd exhaustion | y — names the cause, and says it keeps trying | yes, and it *does*: this is a retry loop that narrates itself | C |
| `src/bin/skein-server.rs:606` | skein-server: N consecutive accept failures ({e}) and not one connection ever served … Exiting rather than sitting up and quiet, which is indistinguishable from working. | the same, never having served | y — both branches named | n/a — it exits deliberately | C |
| `src/bin/skein-server.rs:2988` | skein: ssh key not loaded ({e}) | pressing Save in cockpit settings | **cannot tell** — a person causes it and a person will not see it: it goes to the server's stderr while the Save returns 200 | yes | N |
| `src/bin/skein-server.rs:908` (13 sites: `:1149`, `:1733`, `:2443`, `:2473`, `:2610`, `:2629`, `:2648`, `:2666`, `:2690`, `:2707`, `:2765`, `:4212`) | invalid box name | any cockpit action on a name with a disallowed character | **n** — never states the grammar; `warden/src/serve.rs:496` does, for the same class | no — a keystroke, but not one the person can make blind | N |
| `src/bin/skein-server.rs:1214` (7 sites: `:1513`, `:1715`, `:1842`, `:1955`, `:2053`, `:2138`) | no such repo | a cockpit action against a repo that was removed | n | yes — the repo record appearing | N |
| `src/bin/skein-server.rs:4209` (+ `:4733`, and `:1018` "cross-origin stream blocked") | cross-origin terminal blocked | opening the cockpit on an origin not in `$SKEIN_ALLOWED_ORIGINS` | **n** — the variable that governs it is never named | yes — the origin being added | N |
| `src/bin/skein-server.rs:912` | a box is created on a branch | POSTing a create with no branch | partly — implies the missing field | no — a keystroke | C |
| `src/bin/skein-server.rs:1001` | no such act — it may have finished longer ago than the warden keeps them | polling an act that has aged out | partly — names the cause | no — the act is gone | U |
| `src/bin/skein-server.rs:4098` | too many live boards open — close one and retry | opening more boards than `EVENT_LIMIT` | partly — manual | yes — a permit freeing, exactly as §5's two PTY rows | W |
| `src/bin/skein-server.rs:4736` | unsupported runtime | opening a login socket for an unknown runtime | n — no list, same omission as `src/bin/skein.rs:1317` | no | N |
| `src/bin/skein-server.rs:886` (+ `:1758`, `:3370`, `:3413`) | {tokio JoinError}, at 500 | a panicking blocking task | **cannot tell** how the cockpit renders it; either way it is not a sentence | yes | N |
| `src/bin/skein-server.rs:1753` (+ `:3365`) | {why}, at 503, forwarded from below | the cockpit polling shape or sandboxes while `sbx` is wedged | inherits; the comment at `:1750` refuses an empty list because the two causes "send a person to different places" | yes — and both are polled, so recovery is automatic | C |
| `src/bin/skein-server.rs:938` | {why}, at 409, from `act::begin` | starting an act while one is running | y — the comment at `:935` says the message names which act to watch | yes — the other act finishing | C |

## 7. The cockpit

`src/web/index.html` holds **32 failure toasts** and **14 in-place failure renders**. The page has
exactly two self-healing patterns, and they are good: the health poll (§1) and `revStaleTimer`
(`src/web/index.html:3401`), a 4/8/16/32/64s re-ask that degrades to a visible "try again" control
after `REV_STALE_TRIES`. Everything below either copies those or does not.

| where | what a person sees | reachable how | to do? | watchable? | v |
|---|---|---|---|---|---|
| `src/web/index.html:2567` | session not connected / click to reconnect | any box terminal whose socket drops with a code that is not `CLOSE_CHILD_ENDED` | partly — a control, but no diagnosis, and it covers the sentence that had one | yes — the page holds `es.readyState`, `lastTickAt` and the box's state on the stream. `reconnectSession` has four call sites, all of them a person acting | W |
| `src/web/index.html:2118` (+ `:2132`, `:7162`, `:7172`, `:7185`) | continue failed: {server error} · restart failed · stop failed · destroy failed | the row action buttons on the fleet board | **n** — verb plus "failed", in a 3.5s toast | yes — the box's own state on `/api/events`: a stop that failed leaves the box running | N |
| `src/web/index.html:8579` | send failed | Mailbox → Send with a failing POST | **n** — two words. The typed text survives, and nothing says so | yes — the HTTP status distinguishes a safely re-sendable 5xx from a 4xx | N |
| `src/web/index.html:8916` | could not run the test | Settings → GitHub & keys → test writes | **n** | yes — `health.gitgate` already carries the same fact on the 15s poll | N |
| `src/web/index.html:7637` | could not load diff | box → Diff tab when the endpoint rejects | **n** — the ok path distinguishes causes in `d.note`; this one does not | yes — the box's run state | N |
| `src/web/index.html:7622` | could not read {path} — {reason} | box → Files → click a file | n | yes | N |
| `src/web/index.html:7376` | loading… | box → session digest pane | **n**, and there is no failure arm at all: a rejected fetch leaves the spinner up for ever and only the global handler says anything | yes — the fetch settling. **The worst spinner in the file** | N |
| `src/web/index.html:5147` | skein could not reach its own summary for this PR: {error} | Review pane, a PR whose in-flight read failed | n — a raw fetch error in a row | yes, and half-built: it is flagged `transient`, and the next full load deletes it (`src/web/index.html:3381`). Nothing on screen says so, and nothing triggers that load | N |
| `src/web/index.html:9730` (+ `:9716` "nothing was copied out: {e}") | — not saved: {server error}, per box | Settings → Save every box's work, partial failure | partly — the headline from `cockpit/src/saved.mjs:22` states the stake ("a destroy would take it"); the per-box line is a bare server string with no per-box retry | yes — the failed names are already a list; one endpoint away | N |
| `src/web/index.html:11289` | the update failed; the log says where | Settings → Update skein | partly — points at the pane below, which `loadUpdate` does refresh | yes — and the log tail at `:11279` is exemplary, retrying every 1.5s across the binary swap | W |
| `src/web/index.html:5929` | Workflows are not running. {error} | Review → expand a PR whose repo has a malformed workflow file | **n** here, while `:4517` answers the same fact with "the workflow file has a problem — fix it before editing here" | yes — the next `loadWorkflows` after an edit | N |
| `src/web/index.html:3892` | {repo} — {error}, under "N repos' queues could not be built" | Review pane, several repos failing | **n**, while the same fact in the clear-screen at `:3598` adds "try again" and `:3605` adds "Settings → Repos" | yes — `revStaleTimer` already exists, three lines away | N |
| `src/web/index.html:3868` | GitHub said: {error}, under "the queue could not be built" | Review pane, GitHub refusing | y — `:3875` adds "try again" and "Settings → GitHub & keys", and `:3869` keeps the remembered queue with a caveat | yes — `revStaleTimer` | C |
| `src/web/index.html:3598` (+ `:3605`) | {repo} — skein could not read this queue: {error} — try again · skein did not ask: {why} — Settings → Repos | Review pane cold, all repos failing | y | yes | C |
| `src/web/index.html:2014` | sbx could not be asked ({status}) | typing `foreign:` in the board filter | n | yes — but `askForForeign` runs only on demand | N |
| `src/web/index.html:10958` | board is Ns stale — the server stopped answering | the EventSource open, the producer wedged | **n** — and nothing re-dials. The sibling state at the same site, "reconnecting…", is honest because the browser is retrying | yes — a tick arriving; `lastTickAt` is already the clock | N |
| `src/web/index.html:11026` | reconnecting… | the stream erroring | y — implicit, and true: EventSource retries by itself | yes — already watched | C |
| `src/web/index.html:11122` | the fleet's {runtime} login was refused at {time}: {said} — summaries, critiques and workflows are declining model calls | an expired fleet credential | y — a "log in" button that opens a PTY (`login_terminal`, `src/bin/skein-server.rs:4727`) | **yes, and watched** — the same 15s health poll clears `expired_logins`, and `:11355` re-reads on close. **The best surface in the product** | C |
| `src/web/index.html:11088` | {check}: {first sentence of detail} (+N more) | any unsatisfied health check | y — clicking opens Settings → diagnostics, where the `fix` is | yes — §1 | C |
| `src/web/index.html:5835` | The brief could not be fetched — {error} Close the row and open it again to retry. | Review row whose brief fetch fails | y — names the retry gesture, and `:4352` clears the waiting flag deliberately so the row does not "sit on '…' for ever with nothing to retry it" | yes — but it asks for the gesture instead | W |
| `src/web/index.html:7212` | something went wrong in the page: {first line of the exception} | any uncaught error or rejected promise | **n** — de-duped 60s, deliberately does not re-render | none — it is the catch-all | N |
| `src/web/index.html:9354` (+ `:9275`, `:9289`, `:9359`, `:9368`, `:9607`, `:9618`, `:9626`, `:9671`, `:9689`, `:10103`, `:10385`, `:11250`, `:11310`, `:11316`) | {server string} · pull failed: {e} · couldn't save: {e} · could not start: {r.error} … | settings fields, repo actions, sync cards, update controls | **n** — 13 of the 32 failure toasts are this shape: a server string in a 3.5s toast, no step, no retry | varies; each has a status code that distinguishes retryable from not | N |
| `src/web/index.html:7628` | asking the box… | box → Diff tab | n/a — a spinner that waits on a real `git` fork, and has a catch. Honest but unbounded, with no elapsed counter | yes — the fetch settling | C |
| `src/web/index.html:3860` | asking GitHub… | Review pane, cold open | n/a — waits on `/api/review`; `:3425` replaces it on failure | yes | C |
| `src/web/index.html:8644` | could not load package requests | the package-request panel | n | **yes, and watched** — the panel re-loads every 20s while open (`:8696`), so it self-heals within a cycle | C |
| `src/web/v2.html:313` (+ `:318`) | the change could not be read | v2 board → click a row | **n** — no retry control in the pane at all | yes | N |
| `src/web/v2.html:466` (+ `:526`) | it could not be added · it was not accepted ({status}) | v2 add-a-repo, make-a-box | n — the button re-enables, unremarked | no — a keystroke | N |
| `src/web/v2.html:370` | — disconnected — | v2 terminal socket closing | **n** — written into the buffer, then nothing. No overlay, no retry. It does at least not cover the last line | yes — `src/web/v2.html:245` already auto-reconnects the *stream* every 4s; the terminal does not | N |

## 8. The board's rows

`src/board.rs` has **no `Err(String)` sites at all**; its whole error surface is row fields, drawn
identically by the CLI table and the cockpit (module doc, `src/board.rs:3`). Every row here is
reached by *looking at the board* — no action needed, which makes them the most reachable
diagnoses in the product, and the least explained: each is a token, with the sentence that would
help sitting in a doc comment beside it.

| where | what a person sees | reachable how | to do? | watchable? | v |
|---|---|---|---|---|---|
| `src/board.rs:317` | the word `error`, `ended` or `stale` in the state column | looking at the board | **n** — one word | yes — `box_liveness` returning running, or a new hook edge | N |
| `src/board.rs:344` | the reason the turn failed, verbatim from the box's own probe | looking at the board | **cannot tell** — the text originates in the probe and `signals`, not here | yes — a fresh non-outcome status | N |
| `src/board.rs:350` | why the turn stopped | looking at the board | n | yes — a new turn starting | N |
| `src/board.rs:355` | `permission`, `question`, `trust` or `auth` | a blocked agent | partly — the doc at `:353` says "each wants a different move from you, so the row names it instead of saying 'decision'". It names the *kind* of move, not the move | yes — the dialog clearing | N |
| `src/board.rs:367` | `never`, `misfiled` or `stale` for hooks | a box whose signals are not arriving | partly — the doc at `:364` sends the reader to `hook_health` for "what each one asks of a person"; the row carries only the token | yes — a correctly-named signal appearing | N |
| `src/board.rs:377` | `none`, `stale`, `unreadable`, `unsupported`, `newer`, `misfiled` | a box blind to its own screen | **n** | yes — a fresh parseable observation, which `pane_usable` already computes | N |
| `src/board.rs:372` | `screen`, `edge`, `edge-ahead` | looking at the board | n | yes — the observer catching up | N |
| `src/board.rs:395` | a sandbox skein did not place, hidden until the `foreign:` filter reveals it | typing `foreign:` | n | yes — a placement record appearing | N |
| `src/board.rs:430` | `older` — the box's isolation cover is out of date | after an upgrade | n at the row; §1's `cover_health` carries the sentence and the cost | yes — `cover_is_current`, computed every tick | N |
| `src/board.rs:445` | `uncapped no-cgroup-delegation` (or `no-limit-computed`, `could-not-join-cgroup`) | a box started outside the ceiling | partly — the doc at `:441` distinguishes the three, "need a different fleet rather than a different setting"; the row shows the token | yes — the launcher writing `capped` at the next start | N |
| `src/board.rs:455` | this box's credential is not scoped to its own repo | looking at the board | n | yes — `box_is_scoped` flipping | N |

## 9. `src/fleet.rs`

The largest module, and the one where the two halves are furthest apart: it holds both the best
recovery messages in the tree and the most-duplicated dead end.

**A correction, because the item that prompted this survey cites it.** SKEIN-679 records
`src/fleet.rs:5784` as telling a person "`skein resize 8g` is safe to re-run — creating the sandbox
is idempotent, so it retries only the step that failed". **That sentence is gone.**
`src/fleet.rs:5784` is a line inside `save_boxes`, and the only surviving occurrence of the sentence
is a test doc comment at `src/fleet.rs:19755` explaining why it was deleted — with
`the_resize_stops_at_the_destroy_and_promises_nothing_beyond_it` (`src/fleet.rs:19763`) asserting it
stays deleted. The repository now enforces its absence.

**A second finding worth its own line: a well-written family of refusals is currently unreachable.**
`resize_fleet` has one caller (`src/bin/skein-server.rs:3555`), gated at `:3552` by
`fleet_lifecycle_refusal`, which since SKEIN-576 always returns `Some` (`src/fleet.rs:1308`). So
every refusal inside `resize_fleet_inner` is dead to a person today. Two of them name no next step,
and would ship as dead ends the moment that gate changes. They are marked **gated** below rather
than dropped.

| where | what a person sees | reachable how | to do? | watchable? | v |
|---|---|---|---|---|---|
| `src/fleet.rs:1976` (+ `:5112`, `:6941`, `:6998`, `:8329`, `:8346`) | no fleet sandbox configured | `skein start`, `skein login`, login sharing, cgroup application, and the cockpit's login socket | **n** — six identical copies, none naming Settings, a config key or `skein doctor` | yes — `load_config().fleet_sandbox` becoming non-empty. Note `src/board.rs:42` records that `load_config` repairs a blank name, so some of the six may be unreachable in practice — **cannot tell without running it** | N |
| `src/fleet.rs:5781` | no fleet sandbox is configured, so there is no box to save | `skein save` | n — better worded, same absence | yes | N |
| `src/fleet.rs:7931` (`absent_box_reason`) | box {name} does not exist … Run `skein start {name} --branch <branch>` on the host to try again. Do not run `sbx create` — sbx suggests it, and it would build the per-VM box skein no longer supports … | cockpit terminal for a missing box; `src/sandbox.rs:557`, `:759` | **y — the model for this whole survey.** Command, reason, and an anti-command | yes — `shared_record` becoming `Some`; the best-instrumented condition in the file, and nothing polls it | W |
| `src/fleet.rs:8007` | box {name} has no checkout in {sandbox} — `skein start {name}` to build one | `ensure_box_session`, cockpit reconnect, `skein attach` | y | yes — the checkout directory appearing | W |
| `src/fleet.rs:8569` (+ `:8575`, `:8581`) | {name}'s placement record predates the anchor check … restart it with `skein restart {name}` · … anchor belongs to an earlier boot … · … anchor pid has been reused … | cockpit attach, `skein attach` | y — all three end in the same command | yes — a fresh `anchor_matches`, already computed on every attach | W |
| `src/fleet.rs:2789` | the fleet sandbox {sandbox} is already being created — that started {age} ago and takes a few minutes. Wait for it rather than starting a second one; if it never finishes, it is given up on automatically. | pressing Launch twice | y — "Wait for it", and it names the auto-expiry | **yes, and already watched** — the attempt lease expiring, the create completing. The best self-recovering message in the module | C |
| `src/fleet.rs:2845` | {sandbox} is not the fleet sandbox this skein is running inside … | `ensure_fleet`, box launch | y — "ask for it from the cockpit's fleet pane, which puts the request to the warden and shows you the line to run if no warden answers" | yes — warden reachability | W |
| `src/fleet.rs:2761` | skein cannot create {sandbox} from here and will not guess. {rendered operation} | `request_fleet_create` from the cockpit fleet pane | y — renders the exact line | yes — `fleet_exists` flipping | W |
| `src/fleet.rs:1308` (`fleet_lifecycle_refusal`) | skein is running inside the fleet sandbox, so it cannot {what} it from here … | `skein resize`, cockpit Rebuild | y — "On the host, destroy it first: … Then create it on the host with: {line}", and a fallback to "run `skein doctor` and copy the one it prints" | no — the act is off this machine | U |
| `src/fleet.rs:1374` (`destroy_costs`) | Destroying it is not a restart: every box's checkout lives inside that sandbox and nowhere else … | the same | y — "Save that work first if any of it matters: `skein save` copies every box's whole tree onto the host …" | yes — `save_boxes` completing; the destroy line could be gated on a recent save | W |
| `src/fleet.rs:1374` (Err arm) | skein could not count the boxes in it ({why}) — and could not ask is not the same as nothing to lose, so read this as every box. | the same, with a failing census | partly — tells you how to *read* it | yes — `census_placed_boxes` succeeding | N |
| `src/fleet.rs:5089` (`refuse_a_repurpose`) | {name} is already a {x} box, and this would start it as a {y} one. Nothing has been changed … | `skein start`, cockpit Launch | y — "destroy {name} first if it is finished with, or start the new one under another name" | yes — the placement record disappearing | W |
| `src/fleet.rs:5159` | the fleet sandbox cannot see {store}, so box {name} would come up with no store. | `skein start` for a repo whose store is outside the mounts | y — renders the create line and says why remaking is the fix | no — a host act | U |
| `src/fleet.rs:5296` | {why} — {name} IS running … but it has no hooks or kit, so the board cannot see its turns. Run `skein restart {name}` again{…} | `skein start` / Launch whose provisioning failed | y — and `what_to_look_at` appends kill-vs-timeout-specific advice | yes — `box_is_ready`, a poll skein already has | W |
| `src/fleet.rs:5980` (`room_to_copy_out`) | copying the boxes out needs about {n} MiB and the host has {m} MiB free … | `skein save`, cockpit Save | y — "Freeing space, or `skein stop`ping boxes you do not need, makes room." | yes — free space crossing the threshold; a numeric poll | W |
| `src/fleet.rs:4069` (`stray_advice`) | N directories in {dir} … are neither skein's own nor any box's … if they are yours to delete: rm -rf {paths} | `skein doctor` disk section, above `DISK_FULL_PCT` | y — a literal command | yes — the percentage dropping | C |
| `src/fleet.rs:902` (`cockpit_port_advice`) | the cockpit's port :{port} in {sandbox} is not held by the doorway, so do not publish it … | the cockpit port-publish flow | y — "check `tmux -S {sock} capture-pane -p -t {session}` in the sandbox, or that python3 is present" | yes — `door_settles` at `src/fleet.rs:752` is literally that poll, already written | W |
| `src/fleet.rs:8371` (`share_outcome`) | logged in, but could not hand it to the boxes already running ({why}) — they pick it up when their session next starts | `skein login` tail | y — **and it is self-healing by design**, saying so | yes — already | C |
| `src/fleet.rs:5815` | there is no box named {name} in {sandbox} — {root} holds no checkout, and an archive of nothing reads exactly like a save. Nothing was copied out. | `skein save <box>` | partly — explains, names no verb | yes — the checkout appearing | N |
| `src/fleet.rs:5823` | no box is placed in {sandbox}, so there is nothing to save | `skein save` | n — true and terminal | yes — a placement appearing | N |
| `src/fleet.rs:5803` | {name} is not a box name — nothing was copied out | `skein save <bad name>` | **n** — does not say what a box name is | no — a keystroke, made blind | N |
| `src/fleet.rs:6084` (+ `:6112`) | listing {dir}: {io error} | printed on every `skein resize` and every cockpit Rebuild, through `census_placed_boxes` → `destroy_costs` | **n** — a raw I/O error with no framing | yes — the directory becoming readable | N |
| `src/fleet.rs:8422` | the fleet sandbox reported no usable HOME ({home}); every box command would run with HOME unset and write to the filesystem root | any `sandbox_home` caller, through `skein start` and the cockpit's box routes | **n** — a severe consequence, no move | yes — one line: the sandbox answering with a non-empty absolute path | N |
| `src/fleet.rs:8335` | login in {sandbox} exited {code} | `skein login` | **n** — a bare exit code | yes — `login_written_ms` gaining a fresh timestamp; skein already has the reader | N |
| `src/fleet.rs:2311` | creating fleet sandbox {sandbox}: {warden detail} | `heal_fleet` / the warden create path, surfaced by `skein doctor` and the cockpit fleet pane | n — a prefix plus raw detail | yes — `fleet_exists` becoming `Some(true)`; the warden's uncertain outcome is explicitly re-pollable | N |
| `src/fleet.rs:5974` | skein: could not measure the space this needs; continuing | `skein save`, and any path through `room_to_copy_out` | **n** — hands the reader a risk with no lever, then proceeds | yes — re-running the measurement | N |
| `src/fleet.rs:5336` | skein: {name} is running WITHOUT a memory ceiling ({why}); a runaway build in it can take down every other box in the fleet | `skein start` | **n** — the highest-stakes step-free line in the module | yes — `uncapped_reason` returning `None`, which the board already renders | N |
| `src/fleet.rs:2375` (+ `:2867`) | skein: the cockpit's door is not open in {sandbox} ({e}); a box in this fleet can bind :{port} before skein does | server start / fleet heal | n | yes — `door_settles` (`src/fleet.rs:752`), already written | N |
| `src/fleet.rs:2385` (+ `:2883`) | skein: could not point dockerd at the workload cgroup ({e}); containers in {sandbox} stay outside the ceiling | server start / fleet heal | n | yes | N |
| `src/fleet.rs:3427` (+ `:3444`) | skein: could not pin SSH host keys in {x} ({e}); a box cloning over SSH will fail host key verification | box provisioning | n | yes — the pinned hosts appearing in the sandbox's known hosts | N |
| `src/fleet.rs:1271` | skein: could not write the fleet kit ({e}) — the create line below still names it, and the fleet will not put its own door back after a restart until it exists | fleet create rendering | n | yes — the kit file appearing | N |
| `src/fleet.rs:4816` (+ `:4825`) | skein: {name} matches no repository skein knows … it starts with the sandbox's whole view, as boxes did before covers · skein: {mount} has a newline in it, so boxes cannot be told about it | box launch | n — both state an isolation loss and stop | yes — the repo record; the mount being renamed | N |
| `src/fleet.rs:5179` (+ `:5208`, `:5321`, `:5329`) | skein: {name} is cloning from a mirror that could not be refreshed: {why} · has no write token yet — {problem} · could not set {name}'s git identity ({e}); its first commit will ask who you are · came up, but its conversation could not be located ({e}) | `skein start` | n — four consecutive box-launch degradations, none with a step | yes — a fetch succeeding, a token arriving, the identity being set, the conversation file appearing | N |
| `src/fleet.rs:7707` (+ `:7722`, `:7793`, `:7808`) | could not read the {rel} login out of {sandbox} ({why}) — the host's kept copy is left exactly as it was · … left alone. · could not save the {rel} login out of {sandbox} — the copy that was already there is untouched · could not restore the {rel} login into {sandbox}: {e} | login sync | partly — all four say what was *not* damaged, which is the right instinct; none says what to do | yes — `login_fingerprint` / `login_written_ms` | N |
| `src/fleet.rs:8021` | skein: could not refresh the launcher in {sandbox} ({e}); {name} starts with whichever copy is already there | box start | n | yes | N |
| `src/fleet.rs:7989` (+ `:8053`) | {name} has a session nothing can enter — {why} — so it is being ended and {name} started again · {name} had no live session, so it was restarted (its work is untouched) | box reattach | y — **these narrate a recovery that already happened.** The pattern the rest of the module should copy | already recovered | C |
| `src/fleet.rs:6239` (**gated**) | box {name} belongs to no registered repo, so nothing could say where its work came from or put it back — resize aborted with the sandbox untouched | not reachable today | **n** — names no verb; `skein add` is never mentioned | yes — the box gaining a repo | N |
| `src/fleet.rs:6245` (**gated**) | box {name} has no recorded branch to come back on — resize aborted with the sandbox untouched | not reachable today | **n** | yes — a branch being recorded | N |
| `src/fleet.rs:6300` (**gated**) | could not tell whether {sandbox} was destroyed: {detail} — every box's work is copied out to its own state directory as {run}.tar, and those copies are the only thing that survives the sandbox either way | not reachable today | partly, deliberately — the comment at `:6295` argues "a destroy that may have happened is the one answer no command can be offered for" | yes — `fleet_exists` answering definitively; the uncertainty is exactly a re-askable question | N |
| `src/fleet.rs:6131` | {root} holds N checkout(s) that {dir} has no placement record for, so nothing could carry {them} out: {names} | gated on the resize path, but `census_placed_boxes` also runs under `destroy_costs` and `save_boxes`, where a person does see it | **n** | yes — a placement record appearing for each named checkout | N |
| `src/fleet.rs:6202` (**gated**) | could not check what Docker is holding ({why}), and a resize destroys /var/lib/docker — resize aborted with the sandbox untouched. | not reachable today | y — "Restart the daemon and try again, or pass --drop-docker to resize anyway and lose whatever is in there." | yes — dockerd answering | W |
| `src/fleet.rs:5909` (`docker_refusal`, **gated**) | a resize destroys /var/lib/docker, and it is holding N thing(s) nothing can put back … | not reachable today | y — a full `docker save` line and a `docker run … tar` line, plus "--drop-docker" | yes — `docker_state_at_risk` emptying | W |
| `src/fleet.rs:4027` (+ `:4034`) | skein could not work out which directories under {dir} are its own … · skein can see no boxes at all … | **nobody** — `src/health.rs:241` states outright that "an `Err` from `strays` is silence, not a sentence" | n/a | n/a | — |

## 10. The rest of `src/`

From the 191 production `Err(String)` sites outside `src/bin/`. Excluded as not person-facing, with
the reason: the input-validation one-liners guarding `pub(crate)` writers, whose callers map them —
`src/sandbox.rs:283`, `:298`, `:316`, `:393`, `:535`, `:606`, `:734`, `:958`; `src/repos.rs:310`,
`:1684`, `:1688`; `src/gitgate.rs:669`, `:677`, `:989`, `:1014`, `:1038`; `src/files.rs:60`, `:67`,
`:74`, `:263`, `:315`. For `src/fleet.rs:1087`, `:1170`, `:1179` and `:536`, **cannot tell** — the
callers are in-crate and were not all traced.

| where | what a person sees | reachable how | to do? | watchable? | v |
|---|---|---|---|---|---|
| `src/volume.rs:104` | this installation was moved to {target}, and $SKEIN_HOME still points at {home}. Set it and try again: export SKEIN_HOME={target}. Or, if you meant to keep using this one: rm {marker} | any CLI verb, through `ensure_volume` at `src/bin/skein.rs:33` | y — two labelled branches, both copyable | yes — `$SKEIN_HOME` matching the recorded path | W |
| `src/volume.rs:113` | … holds a half-finished move … Delete it and move again, or remove {marker} if you know the copy completed. | the same, after an interrupted `skein move` | y | yes — the marker disappearing | W |
| `src/volume.rs:122` (+ `:130`) | Upgrade skein, or point $SKEIN_HOME at another volume. Reading it anyway would drop every field this binary does not know at the next write. · Use a skein that understands {found}. | a volume written by a newer or older skein | y | yes — the binary's schema matching | W |
| `src/volume.rs:182` | … skein here would read and write the volume over there — and everything would look fine until that one was deleted. If you moved or copied it here: skein repoint | starting against a copied volume | y — names the verb, which `src/bin/skein.rs:32` deliberately exempts from the guard so it can be run | yes — the recorded path matching | W |
| `src/volume.rs:471` | Move onto an empty directory — merging two volumes is not something this can do safely. | `skein move` onto a non-empty target | y | no — a keystroke | C |
| `src/volume.rs:488` | the fleet sandbox {sandbox} is up, and its boxes are reading the volume you are moving … This is a job for the host: {line}. Every box's work is on the volume and travels with it. | `skein move` with the fleet running | y | no — a host act | U |
| `src/volume.rs:533` | … has {have} free and this needs about {need} … Free some room or choose another target. | `skein move` onto a full disk | y | yes — free space crossing the threshold | W |
| `src/volume.rs:561` | copying to {target} failed, and the half-copy is left in place with its MIGRATING marker so nothing mistakes it for an installation: {stderr} | `skein move` | **n** — states the safety property, offers no move; contrast `:113`, which does | yes — the marker being removed | N |
| `src/sandbox.rs:401` (+ `:406`) | box {name} is not running; attach/start it before resuming · cannot tell whether box {name} is running; attach/start it before resuming | the cockpit's Continue button | partly — names an act, not a command | yes — `box_liveness` returning running | W |
| `src/sandbox.rs:469` | resume exited {code}{detail}; see {path} | cockpit Continue | partly — points at a file | yes | W |
| `src/sandbox.rs:561` | stop failed (exit {code}): {stderr} | cockpit Stop, `skein stop` | **n** | yes — `box_liveness` returning stopped | N |
| `src/sandbox.rs:763` | teardown failed (exit {code}): {stderr} | cockpit Destroy, `skein destroy` | **n** | yes — the sandbox no longer being listed | N |
| `src/tracking.rs:949` | the gateway could not be reached ({status}) | Settings → Work tracking | **n** | yes — an HTTP GET to the gateway URL returning 2xx. The most literally pollable condition in this survey | N |
| `src/tracking.rs:660` (+ `:749`) | the {label} connection has no gateway URL yet | cockpit work-tracking panes | **n** — and `src/tracking.rs:755` answers the sibling condition with "add it in Settings → Work tracking" | yes — the config gaining a URL | N |
| `src/tracking.rs:755` | no Plane token stored for {label} — add it in Settings → Work tracking | the same panes | y — the model its two siblings should follow | yes | W |
| `src/tracking.rs:761` (+ `:853`) | {name} is not running | cockpit box registration, docs apply | **n** | yes — `box_liveness` | N |
| `src/tracking.rs:802` | token written, but registration did not confirm{detail} — the box can still be registered by hand against {url} | box registration, partial success | y — names the fallback | yes — a confirming re-registration | W |
| `src/tracking.rs:430` | {users} still {uses} it — point {them} at another connection first | removing a connection in use | y | no — a keystroke | C |
| `src/tracking.rs:153` (+ `src/repos.rs:241`) | reading {path}: {e} | nearly every command, on an unparseable `repos.json` or `connections.json` | **n** — a raw error on a file almost everything reads | yes — the file parsing | N |
| `src/gitgate.rs:1334` (+ `:1342`) | no GitHub App configured: Settings → GitHub App ID · the GitHub App ID must be the numeric id, not {id} — Settings → GitHub App ID | Settings → GitHub App | y | yes — the setting arriving | W |
| `src/gitgate.rs:1347` | the GitHub App key is not at {key} | the same | partly — names the path, no verb | yes — the file appearing at that path | N |
| `src/gitgate.rs:799` | could not sign the App JWT: {stderr} | any token mint, so any push or PR from a box | **n** | yes — a subsequent mint succeeding | N |
| `src/gitgate.rs:1435` | could not withdraw {failures} | credential withdrawal from the cockpit | **n** | yes — the token files being absent from the box state directory | N |
| `src/files.rs:122` | the box could not read that ({other}) | the cockpit file browser | **n** — surfaces an opaque token | none — the token is the box's | N |
| `src/files.rs:119` (+ `:120`, `:121`, `:209`, `:245`, `:270`, `:318`) | path escapes the workspace · not a directory · not a file · invalid path | the cockpit file browser | n | no — input | N |
| `src/takeover.rs:137` (+ `:143`, `:151`, `:243`) | copying {guest} exceeded 300s · waiting for artifact copy: {e} · copying {guest}: {stderr} · extracting shared context: {stderr} | a cockpit takeover to another runtime | **n** — four ways a takeover stops, none with a step | yes — the artifact file appearing on the host | N |
| `src/takeover.rs:323` | {source} already uses {target_runtime}; use the same-provider tmux restart path | the same | y | no — a choice | C |
| `src/repos.rs:1035` (+ `:1114`, `:1172`) | mirroring {from}: {stderr} · fetching {id}: {stderr} · fetching {refspec} of {id}: {stderr} | `skein pull`, the cockpit repo pane | **n** — raw git stderr | yes — a subsequent fetch succeeding | N |
| `src/repos.rs:1014` | {id} has nothing to mirror from | the same | **n** | yes — the record gaining a source | N |
| `src/repos.rs:1241` | the mirror at {path} has no readable HEAD — an empty repository, or a mirror that did not survive its clone | the same | **n** — a good diagnosis, no step | yes — `git symbolic-ref HEAD` in the mirror answering | N |
| `src/repos.rs:1513` | {source} is a path, and skein registers repos by remote … Give the remote instead — `git -C {source} remote get-url origin` prints it. | `skein add <path>` | y — hands over the command that produces the right argument | no — a keystroke | C |
| `src/repos.rs:1465` | unsupported runtime {runtime}; available: {list} | `skein add --agent <typo>` | y — the list, which `src/bin/skein.rs:1317` and `:1537` omit | no — a keystroke | C |
| `src/repos.rs:299` | {id}'s per-pull-request triggers could not be read ({why}), so this would have replaced every one of them with a single entry. Nothing has been changed. | cockpit repo triggers | **n** — an excellent "nothing changed" guarantee with no next step | yes — the override file parsing | N |

## 11. The warden

The warden is the most consistently well-worded surface in the tree: most of its refusals explain
the rule they are enforcing, and several print the allow-list. Its gap is different from everyone
else's — it is the one component whose failures are *most* watchable (a terminal appearing, a
secret path becoming writable, an outstanding turn releasing) and where **nothing watches at all**.

`warden/src/wire.rs` is excluded: its eleven terse refusals (`:115`, `:121`, `:125`, `:138`, `:141`,
`:152`, `:162`, `:170`, `:173`, `:178`) answer a malformed HTTP request and reach a client, not a
person.

| where | what a person sees | reachable how | to do? | watchable? | v |
|---|---|---|---|---|---|
| `warden/src/doer.rs:70` | {what} for {sandbox} was not run: this warden has no approval surface, and a privileged host command needs a human at the host to confirm it (architecture §8.1). Operation {id}. | pressing Launch against a warden started with no controlling terminal | partly — names the shape of the fix, not the act. `warden/src/main.rs:152` prints the missing half at boot: "Run it where a person can answer it." | yes — a controlling terminal appearing | N |
| `warden/src/serve.rs:179` | this warden has no secret to check against ({path}), so it refuses everything — it mints one at start when that path is writable | every request, on a warden whose secret path is unwritable | y — names the path and the restart | yes — the path becoming writable; nothing retries the mint after start | W |
| `warden/src/serve.rs:189` | this warden does not know who is asking — skein presents the secret from under the mount cover, and nothing else can read it (architecture §9.5 R5) | skein and the warden pointed at different volumes; every warden-backed button fails | **n** — explains the mechanism, names no action. The actionable sentence exists only at `warden/src/main.rs:112`, printed at boot, where the person hitting the 401 is not looking | yes — the two volumes agreeing, comparable at request time | N |
| `warden/src/serve.rs:269` | this warden was built without `{cap}`. Nothing can turn it on: the doer is not in the binary (architecture §8.3). | pressing Launch or Destroy against a reduced-capability warden | **n** — and this is exactly the case the brief warns about. "Nothing can turn it on" is true of *this binary*; installing one built with the capability is the step, and it is not said | no — a compile-time fact | N |
| `warden/src/serve.rs:496` | {name} is not a sandbox name — letters, digits, `.`, `_` and `-`, up to 128 of them, and not beginning with `-` | a malformed sandbox name | y — the grammar, which `src/bin/skein-server.rs:908` and its twelve siblings omit | no — a keystroke | C |
| `warden/src/serve.rs:503` | the {cap} for {name} carries N arguments, and more than {MOST_ARGS} is more than an approval can put in front of a person | an over-long argv | partly | no | C |
| `warden/src/serve.rs:512` (+ `:544`) | an argument of the {cap} for {name} cannot be shown as what it is, so it cannot be approved: {arg} · the value of `{key}` cannot be shown as what it is … | an unrenderable argument | partly — names the offending argument | no | C |
| `warden/src/serve.rs:527` | `{key}` is given twice, and `sbx` would take the last one — so what ran would not be the first thing on the line that was approved | a duplicated flag | y | no | C |
| `warden/src/serve.rs:533` | this warden does not pass `{key}` to a {cap}. The environment decides what a relative program name resolves to, so it is an allow-list and not a filter: this one takes {list} | an unlisted environment key | y — prints the allow-list | no | C |
| `warden/src/serve.rs:569` | the approval for this {cap} would be N bytes of text, and more than {LONGEST_APPROVAL} is more than a person can be asked to read to the end — the operation id they have to type is at the bottom of it | an over-long approval | y — names both sizes | no | C |
| `warden/src/serve.rs:286` | this warden has no `approved` field, and adding one to the request would not create it: approval is a fact the approving side writes, confirmed by a human at the host (architecture §8.1). {error} | a malformed request | y | no | C |
| `warden/src/serve.rs:208` | this warden serves /v1/fleet, /v1/audit, /v1/create, /v1/destroy and /v1/unpublish | a wrong path | y — lists the surface | no | C |
| `warden/src/serve.rs:238` | the sandboxes could not be listed: {why} | the cockpit polling the warden's fleet view with a wedged `sbx` | n | yes — `sbx ls --json` answering, and it is already polled, so recovery happens in practice | N |
| `warden/src/flooding.rs:51` | another operation is already waiting for a person at the host: {operation}. One at a time, deliberately (architecture §8.5) — ask about that one, or wait for it. | pressing Launch while an approval is on screen | y — and the doc at `:47` states the rule: "'slow down' without an alternative is how a client ends up retrying in a loop" | yes — the outstanding turn releasing. The clearest wait-for-the-condition case in the warden | W |
| `warden/src/flooding.rs:55` | more than N operations were proposed in a minute, which is more than a person can confirm one at a time (architecture §8.5). Nothing was run. | a client in a loop | partly — "Nothing was run" closes the loop; no retry-after | yes — the rate window rolling forward, which a `Retry-After` would make machine-actionable | N |
| `warden/src/approval.rs:144` | {what} was refused at the host: the operation id was not confirmed. Operation {id}. | typing anything but the id at the warden's prompt | n at the point of refusal — the coaching is only in the prompt at `:104` | no, correctly — this is a person's deliberate "no", and retrying re-asks by design | U |
| `warden/src/approval.rs:138` (+ `:128`, `:131`) | the answer could not be read, so nothing was approved: {e} · the approval could not be shown: {e} | a tty read or write failing while the prompt is up | **n** | yes — the tty becoming usable | N |
| `warden/src/outcome.rs:194` | {why} — and the warden could not release {path} ({e}), so asking again will be answered with this rather than put to a person | a refusal whose claim file could not be removed | **n** — states a stuck lock and stops. Deleting the named file is the recovery and is not named | yes — the claim file disappearing | N |
| `warden/src/outcome.rs:210` | {id} ran, and recording that failed — treat it as undecided: {e} | a filesystem failure after the command already ran | partly — a next step for a client, not for a person | no — it correctly refuses to guess | U |
| `warden/src/outcome.rs:256` | {path} is unreadable ({e}) — refusing to run, because an operation whose record cannot be read may already have happened | a corrupt outcome record | **n** — names the file, not the fix | yes — the record becoming parseable | N |
| `warden/src/doer.rs:301` (+ `:304`) | could not run `sbx`: {e} · `sbx {argv}` exited {code}: {stderr} | an approved operation that then fails in `sbx` | n — it does forward sbx's own stderr, which is the honest half | no — a genuine failure of the thing asked for | N |
| `warden/src/doer.rs:190` (+ `:204`, `:209`, `:266`) | {argv} does not begin with the `create` verb · passes no `--name`, so sbx would name the sandbox after the agent and the working directory instead · would run as {names} — sbx takes the last `--name` … · the unpublish … is not exactly `ports <sandbox> --unpublish <mapping>` | **not established** — these are aimed at a skein developer and surface as 400 bodies in the cockpit | partly — they explain why, and the last names the required shape | no | N |
| `warden/src/main.rs:59` | skein-warden: could not listen on port {port} at {wanted}: {e}, then exits | starting a warden on a taken port | **n** | yes — the port freeing | N |
| `warden/src/main.rs:112` | skein-warden: volume {v} (from $SKEIN_HOME) — skein must be pointed at the same one, or every request is refused for a mismatched secret; the record is at {r} | warden start | y | yes — the two volumes agreeing | W |
| `warden/src/main.rs:128` | skein-warden: this is not where skein looks by default — export SKEIN_WARDEN=127.0.0.1:{port} for skein and skein-server, or they keep asking {where} | warden start on a non-default port | y — the exact command | yes | W |
| `warden/src/main.rs:140` | skein-warden: NO SECRET at {path} — it could not be read or minted, so every request is refused. Fix the path and restart. | warden start with an unwritable secret path | y | yes — the path becoming writable | W |
| `warden/src/main.rs:152` | skein-warden: no controlling terminal, so there is nobody to approve anything and every doer refuses. Run it where a person can answer it. | warden start under a supervisor | y — the operator-facing half of `warden/src/doer.rs:70` | yes — a terminal appearing | W |
| `warden/src/audit.rs:99` | skein-warden: could not move {a} to {b} ({e}) — it is on the volume, which skein can write, so delete it once you have kept what you want from it | an audit rotation failure | y | yes — the file disappearing | W |
| `warden/src/secret.rs:150` | skein-warden: could not move the secret from {a} to {b} ({e}) — a fresh one will be minted under the cover | secret migration failure | partly — says what happens next, not what to do; re-pairing is implied | yes | N |
| `warden/src/sightings.rs:135` (+ `:64`, `:78`) | it did not answer within {n}s · `sbx ls --json` did not print JSON ({e}): {clip} · `sbx ls --json` printed {clip} | a hanging or foreign `sbx` | n | yes — `sbx` answering | N |

---

## What could not be classified, and why

- **`src/board.rs:344`** — the board's principal "why" field. Its text originates in the box's own
  probe and in `signals`, not in `board`, so whether it says what to do is a property of a surface
  this survey did not reach. It is the single largest unexamined text source in the product.
- **`src/bin/skein-server.rs:2988`** — a person presses Save, the ssh key does not load, and the
  message goes to the server's stderr while the request returns 200. Whether that counts as a
  person seeing it depends on where they are standing.
- **`src/bin/skein-server.rs:886`** and three siblings — a tokio `JoinError` rendered into a 500
  body. Whether the cockpit draws it as a sentence was not established.
- **`src/fleet.rs:1976`** and its five twins — `src/board.rs:42` records that `load_config` repairs
  a blank sandbox name, which would make some of the six unreachable. That cannot be settled by
  reading; it needs a run with a blanked config.
- **`src/fleet.rs:1087`, `:1170`, `:1179`, `:536`** — `pub(crate)` writers whose callers were not
  all traced.
- **`warden/src/doer.rs:190`** and its siblings — refusals aimed at a skein developer that surface
  as 400 bodies. Whether a person ever reads one was not established.
- **`src/announce.rs:787`** — composed into a note whose audience is an agent rather than a person.

## Two things this survey changed about the premise

1. **SKEIN-679's citation is stale.** `src/fleet.rs:5784` does not carry the "safe to re-run"
   sentence; it was deleted, and `src/fleet.rs:19763` now asserts it stays deleted. Anything
   planning work from that citation should re-read it first.
2. **A path with no next step is not always a path with no next step *available*.** Three of the
   worst rows — `src/bin/skein-server.rs:4798`, `src/web/index.html:5147`, `src/fleet.rs:7989` —
   sit beside machinery that already performs the recovery. The defect in those is not that skein
   cannot recover; it is that it recovers silently and tells the person they are stuck.
