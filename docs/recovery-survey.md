# Where skein tells a person something went wrong — and whether they can get out

A survey, not a fix. SKEIN-752 set the standard it measures against:

- leaving a person with no way to recover is the worst thing a surface can do;
- so a surface must at least tell them what they are supposed to do;
- and once they have done it, skein should detect that and recover without being asked.

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
none, that is the finding. One row is the written-down exception, and says why: §7's Write token row
for a repo on another host (SKEIN-812).

---

## Summary

**244 rows**, drawn from **465 candidate sites** enumerated by the twelve commands in
[§0](#0-how-this-was-enumerated). Rows are fewer than candidates because identical strings are
collapsed into one row (the eighteen `invalid box name` returns are one row), and because the
candidate sets deliberately over-collect — an enumeration that only catches what is already known
to be a message catches nothing new. Of those rows:

**A row used to name every site, and that is what the `where` column stopped doing** (SKEIN-876).
It named the extra ones as relative addresses — `` `src/fleet/stop.rs:304` (+ `:8958`, `:8964`) `` —
which carry no path, so no citation reader ever saw one, the ledger could hold no anchor for one and
`--relocate` had never moved one. 101 of the column's 345 addresses were in that form: numbers only
a person re-reading the row could check, and of the 77 of them SKEIN-876 measured against the
commit that wrote this document, 66 were already naming the wrong line. The column carries 310
addresses now and every one of them is a `path:line` the gate reads.

**What replaced the lists is the command that enumerates the family**, which is
[§0](#0-how-this-was-enumerated)'s own habit and cannot be stale between two readings; a row still
names the sites whose words it quotes, because those are the addresses the gate can compare with the
row. Doing it turned up **31 addresses in this column that named a line holding no message of their
row's family** — eight of them caught by the gate itself once the site-cell reader stopped being
blinded (see the `where` row of [the rubric](#the-rubric)), three found by reading, and twenty in
the hand-typed lists. The three family rows were wrong in both directions: `invalid box name` named
thirteen addresses for eighteen sites and eight of the thirteen held no message at all, `no such
repo` said seven and is thirteen, and `usage: skein` said eight subcommands and is nine.

| verdict | rows | what it means |
|---|---:|---|
| **C** — conforms | 58 | says what to do, and either the condition is already watched or there is genuinely nothing to watch because the person's own next keystroke is the recovery |
| **W** — says what to do; watchable, not watched | 42 | requirement 2 met, requirement 3 open. A named, cheap condition exists and nothing polls it |
| **N** — names no next step | 130 | the finding. States a failure and stops |
| **U** — says what to do, and it cannot be watched | 12 | the act is off this machine, or it is a person's deliberate "no" |

Two further rows carry no verdict. The Write token row (§7, SKEIN-812) is the one documented
exception to SKEIN-752, added after the 2026-09-09 measurement; and `src/fleet/disk.rs:307` is a
well-written refusal that `src/health/disk.rs:162` swallows, so nobody ever reads it.

**Requirement 1 is met almost everywhere and requirement 3 almost nowhere.** 100 rows say what to
do; 42 of those name a condition skein could watch and does not, and only a handful of the
remaining 58 are watched because somebody built the watching — the health poll, `revStaleTimer`,
the accept-loop retry, and the fleet-create lease.

**The fourth verdict, `W`, is this survey's and not SKEIN-752's**, and it is the whole shape of the gap.
SKEIN-752 offers *conforms* / *names no next step* / *says what to do but cannot be watched*.
Collapsing `W` into *conforms* would hide requirement 3 entirely — 42 surfaces name a step and then
sit there — and collapsing it into *cannot be watched* would be false, because in every one of the
42 the condition is named in the row and is a poll skein already knows how to write.

### And whether anybody can get there (SKEIN-767)

**Every count above is about the wording of a message, and none of it asked whether a person can be
shown one.** The `reach` column added later answers that, one row at a time, and the whole method is
in [§0a](#0a-whether-anybody-can-get-there--the-reach-column).

| reach | rows | |
|---|---:|---|
| **R** — a person can be shown this | 223 | with the concrete trigger in the cell |
| **X** — they cannot | 17 | with the file:line that forecloses it |
| **?** — not established | 4 | with what would settle it |

Against the verdicts above:

| | C | W | N | U | — |
|---|---:|---:|---:|---:|---:|
| **R** | 58 | 39 | 114 | 11 | 1 |
| **X** | 0 | 3 | 12 | 1 | 1 |
| **?** | 0 | 0 | 4 | 0 | 0 |

**Twelve of the 130 `N` rows are unreachable**, and those twelve are the ones worth acting on first
— by taking them off SKEIN-752's queue. A wording fix to a branch nobody can take is effort spent on
text no user will see, and the assertion it comes with is a test that cannot fail, which
`CONTRIBUTING.md` names as worse than no test. Twelve rows is more sites than it sounds: row
`src/fleet.rs:2059` alone stands for six, and every one of them has now been deleted rather than
reworded (SKEIN-756).

**All seventeen `X` rows are now off that queue** (SKEIN-777), each one's `reach` cell saying so in
its own words, and the twelve `N` ones are the point of the exercise. What the dequeue cost in code
is in [§0b](#0b-what-the-dequeue-did-to-the-code-skein-777) — two messages deleted, four already
gone before it looked, and eleven kept, each with a reason. **Nothing in the tables above moved**: they are
the 2026-09-09 measurement still, and §0a's script reproduces every figure in this section against
the document as it now stands.

**The four `?` rows are all one shape** — a handler's `JoinError` arm, reached only if a
`spawn_blocking` closure panics. Settling them means finding a panic in those call trees or proving
there is none, and neither was done here.

### The three worst offenders

**Re-ranked against `reach`, and the old second entry is gone from the list entirely** — it was
`no fleet sandbox configured`, ranked second in the tree on its wording, and it is **X**. That is
what the column is for. What follows is the ranking as it stands after the pass; the original is
still readable in this document's history.

Ranked by reachability × severity — how easily an ordinary session lands here, times how stuck the
person is when it does.

**1. The terminal reconnect overlay waits for a click, over a sentence it hides.**
`src/web/app/board.js:766` paints *"session not connected / click to reconnect"* on any box terminal
whose socket drops with a code that is not `CLOSE_CHILD_ENDED`. There was no retry timer
when this was written: `reconnectSession` had four call sites, and every one of them was a person
acting — a restart, a tab click, the overlay itself, a file drop. **That is no longer true**
(re-read 2026-09-23, `grep -n 'reconnectSession(' src/web/app/*.js` prints its definition and
six calls):
`src/web/app/board.js:269`, `src/web/app/board.js:664`, `src/web/app/board.js:767`,
`src/web/app/board.js:1065` (the recovery card's *Try again*), `src/web/app/board.js:1097` and
`src/web/app/boot.js:177` — and the fifth is not a person: `retryWaiting` reconnects every pane
waiting on a condition the moment the board tick or a `pty-freed` event says it cleared. Worse,
the six failure sentences in `terminal_session`, `login_session` and `pump_pty`
(`src/bin/skein-server.rs:4253`, `src/bin/skein-server.rs:4307`, `src/bin/skein-server.rs:4491`,
`src/bin/skein-server.rs:4501`, `src/bin/skein-server.rs:4512`, `src/bin/skein-server.rs:4521`) are
written to the socket and then the socket is *dropped without a close code* — so the browser reads 1006, decides
the connection went away, and lays the 82%-opaque card over the one line that said why. This is
SKEIN-702's defect, and it is the most reachable failure in the product: a server restart, a stopped
box or an exhausted PTY pool all land here.

**Fixed, after this survey was written — see the note in [§5](#5-the-websocket-refusals--terminal_session-login_session-pump_pty).**
The rest of this document is left as it was measured; that section says what changed and what it
means for the two rows this paragraph names.

**2. A reachable message that is FALSE about why it is there** — a category this ranking did not
have, because a survey of wording cannot see it. `src/health/disk.rs:45` said *"no fleet sandbox is
configured, so there is no filesystem to measure"*. `fleet_resources()` had two ways to answer
`None`, and the blank-name one was already impossible (`load_config` repairs the name); SKEIN-756 has
now deleted that arm outright, so the only reason left is **the sandbox did not answer** — which the
honest sentence one function away, at `src/health/disk.rs:191`, already says. A person was sent to
Settings to fix a field that holds a name. SKEIN-770.

Worse than *names no next step*, because a step is given and it is the wrong one. It ranks here on
that alone rather than on how often it fires — the wording columns cannot express "this sentence is
untrue", and it is the one defect in this document that a reader could not have found by reading the
sentences.

**Fixed, after this survey was written**, and the row in [§2](#2-healthcheckunknown--the-fix-field-is-empty-by-construction)
carries what it says now. Two things were established before the words changed, and both belong
here because getting the first one wrong *was* the original defect:

- **Every remaining path to `None` was enumerated rather than assumed.** They all funnel through the
  one `?` on `RESOURCE_GATE.get` (`src/fleet/resources.rs:221`), and the closure behind it answers `None` in
  exactly two ways: the measuring command did not run — could not be spawned, outlived its 20s
  deadline, or exited non-zero (`src/place/run.rs:92`, `src/place/run.rs:93`) — or it ran and printed nothing
  `parse_resources` could read (`src/fleet/resources.rs:279`). `Option` has no room to tell those apart, so
  the sentence says so rather than picking one. There is no third path: `unreachable_from_fleet`
  cannot refuse this address, because the sandbox in it and the one it is checked against are the
  same `fleet_sandbox()` call.
- **`None` also means nothing has arrived since the process started**, which the `disk_total == 0`
  row below cannot say. `Gate::invalidate` expires the clock and never the last good answer
  (`src/util.rs:1179`), so a fleet that has answered once never comes back here.

The new sentence carries a next step and a watchable condition, which no `HealthCheck::unknown` in
[§2](#2-healthcheckunknown--the-fix-field-is-empty-by-construction) had: it names
`df -Pm <fleet root>` — interpolated from `fleet_root()`, so it is the filesystem actually measured
and not a hard-coded `/boxes` — and says skein re-asks at most every 30 seconds and clears the row
itself, so nobody goes hunting for something to restart.
`health::tests::a_disk_that_could_not_be_measured_blames_the_sandbox_and_not_the_settings` asserts
both halves, and reaches this arm through `seam::doing_nothing`.

**Its near-twin is NOT a defect, and the difference is worth stating** (it was nearly filed as one
here). `src/volume.rs:498` refuses `skein move` with *"the fleet sandbox {sandbox} is up"* and never
checks whether anything is up — `fleet_exists(sandbox)` compares that name against `fleet_sandbox()`
and `sandbox` came from the same place, so it is true by construction and every `skein move` refuses.
That is the settled design, not an accident: SKEIN-574 made moving the volume an Operation skein
reports and never performs, the refusal renders the host recipe rather than a sentence about it, and
`src/volume.rs:449` says "the fleet is up, which is now always" in as many words. The message is
honest about the consequence; only its grammar reads as a condition.

**3. `skein doctor` reports the three things a box cannot start without, and does not say what to
do about any of them.** `src/bin/skein.rs:1050` prints *"{tool} missing in the sandbox — {why}"* for
`bwrap`, `tmux` and `git`, and `src/bin/skein.rs:1132` prints *"no cgroup delegation — boxes run UNCAPPED, so one
runaway build can kill every other box"*. Both are diagnoses of a fleet that cannot work, printed
by the command a person runs precisely because nothing else is working, with no next line. Note
that `skein doctor` prints `HealthCheck::unsatisfied`'s `fix` field wherever it has one
(`src/bin/skein.rs:536`) — these four faults are the ones that bypass that machinery by being
hand-rolled `println!`s.

### Which to fix first

**Offender 1** — and specifically its second half, the missing close code on the six refusal paths
in `src/bin/skein-server.rs`. Reason: it is the only one of the three where skein *already wrote the
right sentence* and then destroyed it. `absent_box_reason` (`src/fleet/start.rs:828`) is the best
recovery message in the tree — it names the exact command, and it names the command *not* to run —
and a person reaching it through the cockpit sees "session not connected" instead. Fixing the
overlay's auto-retry is the larger win; fixing the close code is the one that turns work already
done into work a person can see, and it is decided already (SKEIN-702).

### One-line and egregious

Three, noted for a decision rather than fixed here:

- `src/bin/skein.rs:1449` and `src/bin/skein.rs:1669` say *"unsupported runtime {x}"*. `src/bin/skein.rs:1695`, in
  the same file, says *"unsupported runtime {x}; available: {list}"*. Two of the three ways to name
  a runtime withhold the list the third one prints, from the same `supported_runtimes`.
- `src/bin/skein.rs:518` prints *"{prog} not on PATH — {why} unavailable"* and stops, while
  `src/health/report.rs:524` answers the identical condition with *"install {name}, or start the server
  from a shell whose PATH has it"*. The fix exists; this line does not reach for it.
- `src/bin/skein-server/boxes.rs:82` and twelve sibling sites return *"invalid box name"* with no
  grammar. `warden/src/serve.rs:603` answers the same class of input with the grammar spelled out.

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
for f in src/fleet/*.rs; do awk '/^#\[cfg\(test\)\]/{exit} /eprintln!/{c++} END{print c+0}' "$f"; done \
  | awk '{s+=$1} END{print s}'                                                         # 37
```

**468 candidate sites.** The 200 split 146 `format!` and 54 literal, and the two patterns are
disjoint. The `src/*.rs` glob is flat, so `src/bin/` is excluded from it automatically and counted
separately above; the `awk` bound on the fleet count is each file's own `#[cfg(test)]` line now
that `src/fleet.rs` is `src/fleet/` (SKEIN-934) — one cut per file rather than one line number.

191 of those 200 are outside `#[cfg(test)]`. They are not all rows: the test applied was *the
function is `pub`, and its `Err` is either returned to `main` in `src/bin/skein.rs`, which prints
it at `src/bin/skein.rs:170`, or returned from an axum handler as a body the cockpit renders.*
Where neither could be traced, the row says **not established** rather than guessing. The pure
input-validation guards behind `pub(crate)` writers are named in [§10](#10-the-rest-of-src) as
excluded, with the reason.

**What counts as a person seeing it.** A message on a startup path a person watches is in scope; a
line from a background ticker that only ever reaches a detached server's stderr is not. Where the
two cannot be told apart from the code, the row says so.

Every `file:line` below was re-read with `sed -n 'Np'` before it was written down, **and they are
followed against the working tree** — `sed -n 'Np' <path>`, not `git show`.

That sentence used to say the opposite: that the numbers were as of the commit that added this
document. It was overtaken within a day. `dbe2318` re-derived forty-four of them against the tree
rather than leaving them pinned, and this repository's one machine-readable way to declare a pin —
the sentence `tools/prose-check.py`'s `CITATIONS_AT` matches, which a dated product review of
2026-08 declared — was never written here. A citation a reader has to `git show` to follow is a
citation nobody follows, so the claim this document makes is the live one.

**It is no longer maintained by hand** (SKEIN-778). `tools/line-cite-check.py` records in
`docs/line-cites.toml` what each cited line said when it was cited, fails the build when a line
stops saying it, and `--relocate --write` moves the numbers when the code moves under them.
Seventy-five citations across `docs/` were already stale when it was written, fifty-five of them
in this file. Where a row here cites code this tree no longer has — the old *too many terminals
open* wording, `CLOSE_CHILD_ENDED` — the ledger says so with the reason, because the row is the
record of what was wrong and re-pointing it would destroy that.

## 0a. Whether anybody can get there — the `reach` column

**Added after the fact, because the ranking above it was wrong** (SKEIN-767).

Every other column in this document is about the *wording* of a message: does it name a next step,
is the condition watchable. None of them asks whether a person can be shown it at all — and without
that question a dead branch with bad wording outranks a live message with mediocre wording, which is
exactly what put `no fleet sandbox configured` second in the tree.

The row for it is the record of the method failing rather than of the author: it says
**"cannot tell without running it"** in as many words, and it was ranked anyway. Somebody then ran
it, and it is dead.

### How the verdicts were derived

**The cheap pass first, over all 243 rows: does this branch read a value that some other function
has already normalised, defaulted, repaired or made total?** That one question accounts for most of
the **X** column. Four shapes of it are in this tree:

* **A repaired value.** `place::fleet_sandbox()` is `load_config().fleet_sandbox.trim()`, and
  `load_config` repairs a blank or whitespace-only name to the literal `skein-fleet`
  (`src/config.rs:482-484`). So `fleet_sandbox().is_empty()` is false in every reachable state.
  Proven by running it, with both roots pinned and `skein cockpit-stop` as the probe because it
  branched on nothing else: `{"fleet_sandbox":""}` on disk reported *not running in **skein-fleet***,
  no `config.json` at all reported the same, and `{"fleet_sandbox":"probe-fleet"}` reported
  *probe-fleet* — so the probe reads the real value and the first two are the repair firing.
  SKEIN-756 deleted twenty-one guards on it.
* **A narrowed return.** `fleet::fleet_exists` is `(sandbox == fleet_sandbox()).then_some(true)`,
  which has only `Some(true)` and `None` to give (SKEIN-627, SKEIN-637). `fleet_lifecycle_refusal`
  has one exit and it is `Some`; `create_line` has one and it is `Ok`.
* **A caller that always gates first.** `resize_fleet_inner`'s whole body is production-dead: both
  callers of `resize_fleet` — `cmd_resize` and `api_fleet_resize` — consult
  `fleet_lifecycle_refusal` and return before reaching it, and `tests/resize_rules.rs` asserts the
  first of those no longer mentions `resize_fleet` at all.
* **A cite inside `#[cfg(test)]`**, which is a fixture string rather than a message.

**The same question separates the dead from the live, which is why it is the right one rather than
a shortcut.** `fleet::memory_plan` returns `None` when `parse_mib(&load_config().fleet_memory)` does
— and `load_config` repairs `fleet_sandbox` and nothing else, so `{"fleet_memory":""}` reaches
`parse_mib`, which answers `None` at its first `chars().last()?`. Two rows one screen apart in
`src/health/report.rs`, the same shape, opposite verdicts, and only following the value tells them apart.

**And the cheap pass is not sufficient, which is why the ones it flagged were then run.** It called
`src/volume.rs:122` dead on the reasoning that nothing in this tree writes a `VERSION` other than
`SCHEMA` — true, and beside the point: `schema_of` reads a *file*. `printf '2\n' > $SKEIN_HOME/VERSION`
and `skein-server` prints that sentence and exits, which is what a message about "a newer skein" is
for. It is **R**.

### What `?` means here

**`?` is a real answer and four rows carry it.** All four are the same shape — a handler's
`JoinError` arm, reached only if a `spawn_blocking` closure panics — and settling one means finding
a panic in that call tree or proving there is none. Guessing instead is what put a dead branch second
in the ranking, and **a wrong X is worse than a `?`**, because it takes work off SKEIN-752's queue
that should have stayed on it.

**All three verdicts are bolded, which they were not** (SKEIN-772). The four `?` rows were written
`| ? —` while every `R` and `X` row was `| **R**` / `| **X**`, so the one column added to be counted
mechanically could not be: a grep for the marker matched 239 of the 243 rows and silently missed
exactly the four whose whole point is that nobody has established them.

**Every count in [§Summary](#summary) is derived from the table, not maintained beside it.** This
prints the verdict totals, the `reach` totals and the cross-tab, and it reproduced the figures
already written here before anything on this branch changed:

```sh
python3 - <<'EOF'
import collections
v, reach, cross = collections.Counter(), collections.Counter(), collections.Counter()
for line in open('docs/recovery-survey.md'):
    if not line.startswith('| `'):
        continue
    cells = [c.strip() for c in line.strip().strip('|').split('|')]
    if len(cells) < 7:
        continue
    rk = next((k for k in 'RX?' if cells[2].startswith('**%s**' % k)), '-')
    v[cells[-1]] += 1; reach[rk] += 1; cross[rk, cells[-1]] += 1
print('rows', sum(v.values()), '| verdicts', dict(v), '| reach', dict(reach))
for rk in 'RX?':
    print(rk, [cross[rk, c] for c in ('C', 'W', 'N', 'U', '—')])
EOF
```

### The rubric

| column | meaning |
|---|---|
| where | `file:line`, verified against the tree — the sites whose words the row quotes, with a larger family given as the command that enumerates it rather than a hand-typed list ([§Summary](#summary), SKEIN-876) |
| what a person sees | the sentence, verbatim or elided at `…` |
| reach | **R** a person can be shown this · **X** they cannot, and the cell names what forecloses it · **?** not established, and the cell says what would settle it. See [§0a](#0a-whether-anybody-can-get-there--the-reach-column) |
| reachable how | the concrete thing a person does, or **not established** |
| to do? | **y** / **n** / **partly**, with the words that do it |
| watchable? | the condition skein could detect — named, or **none** with the reason |
| v | **C** conforms · **W** says what to do, watchable, not watched · **N** names no next step · **U** says what to do and it cannot be watched |

## 0b. What the dequeue did to the code (SKEIN-777)

**Re-verified against the tree on 2026-09-11**, three days after the column was measured, because
five of the seventeen were already *claimed* deleted and a claim about the tree ages. Every row was
re-established from what the code **is** rather than from the line number in its cell — and the
addresses in SKEIN-777's own table, copied out on 2026-09-10, were already wrong for two of the
seventeen (the `src/announce.rs` fixture and `substrate_strays`), because `dbe2318` and SKEIN-778
had moved them in this document meanwhile. That is the habit `tools/line-cite-check.py` exists to
make cheap, and it is why nothing below was acted on at the address it was reported at.

**No row turned out to be reachable.** That was the finding worth looking for, and it is absent.

**Each row is named by what it IS rather than by the address it was reported at**, for two
reasons: two of those addresses were stale before this started, and an address repeated here would
be a second citation the gates then have to keep true in both places, which is the duplication that
made the first two stale. The `where` cells in the tables below carry the live addresses, and every
one of them now says which line of this table it belongs to.

| the row | today | what happened to it |
|---|---|---|
| the `HealthCheck::unknown` fixture `announce_disk` is tested against | `src/announce.rs:793`, inside `#[cfg(test)]` (boundary `src/announce.rs:604`) | kept — a fixture is not a message, and the test needs one |
| `cmd_doctor`'s blank-`fleet_sandbox` line | `src/bin/skein.rs:939` | **kept deliberately** — the tree's one statement of that invariant, and the line says so |
| `cmd_doctor`'s `Some(false)` arm on `fleet_exists` | **deleted** | folded into `_`, which is all the compiler wanted |
| `cmd_doctor`'s `Err` arm on `create_line` | **deleted** | the `match` became `if let Ok` |
| `cmd_cockpit_stop`'s blank-name refusal | gone before this pass | SKEIN-756 |
| `run_attach`'s "nothing to run" | `src/bin/skein.rs:1485` | kept — the type does not forbid this one, two producers do |
| `cmd_resize`'s `.ok_or(…)` fallback | `src/bin/skein.rs:1640` | kept — deleting it needs `src/bin/skein-server/` (SKEIN-788) |
| `login_session`'s refusal arm on `login_spawn_argv` | gone before this pass | SKEIN-774 — `login_spawn_argv` returns a tuple now, so no caller has an `Err` arm to write |
| the six `no fleet sandbox configured` guards in `src/fleet.rs` | gone before this pass | SKEIN-756 |
| `save_boxes`'s copy of the same | gone before this pass | SKEIN-756 |
| `ensure_fleet`'s `!= Some(true)` refusal | `src/fleet/create.rs:820` | kept — a guard with a residual path, and the module's own tests assert on it |
| the five inside `resize_fleet_inner` | `src/fleet/resize.rs:545` | kept — still production-dead, and SKEIN-575's to remove |
| `substrate_strays`'s two refusals | `src/fleet/disk.rs:303` onwards | kept — `expect_err` assertions are about exactly these strings |

**Two deletions out of the twelve `N` rows is the honest yield, and the other ten are the more
interesting half.** Four were already gone, which is what a three-day-old survey looks like, and six
are **kept**. Across all seventeen `X` rows it is 2 deleted, 4 already gone, 11 kept, and the split
is derived from the rows rather than counted here:

```sh
python3 - <<'EOF'
import collections
b = collections.Counter()
for line in open('docs/recovery-survey.md'):
    if not line.startswith('| `'):
        continue
    c = [x.strip() for x in line.strip().strip('|').split('|')]
    if len(c) < 7 or not c[2].startswith('**X**'):
        continue
    b[next(m for m in ('code DELETED', 'code already gone', 'code KEPT') if m in c[2]), c[-1]] += 1
for m in ('code DELETED', 'code already gone', 'code KEPT'):
    print('%-18s all X %2d   N only %2d' % (m, sum(n for (k, _), n in b.items() if k == m), b[m, 'N']))
EOF
```

**The six kept `N` rows split into two shapes this column had been treating as one:**

* **Dead because the TYPE cannot hold the state.** `bool::then_some` has no `Some(false)`; a body
  with one `Ok` return has no `Err`. Those are the two that were deleted. Nothing but the compiler
  was asking for the arm, and a tripwire that cannot trip is not a tripwire.
* **Dead because an invariant holds.** A producer that is total, a caller that gates first. Those
  are kept, because an invariant can lapse and the message is the thing that would say so.
  `src/bin/skein.rs:939` is the model, and its own comment makes the argument.

**Sorting the twelve by which shape they are is the part that could not be read off the `reach`
column**, because both shapes answer the same question — *can this be reached?* — with the same
**X**. The column is still right about the queue: no wording belongs on any of them. It is the
second question, *should the code go?*, that needs the distinction, and only two of the twelve
answer yes.

**And a dequeue does not end the work, it moves it.** The two deletions took the sentences and not
the types that forced them: `fleet_exists` still answers `Option<bool>` and `create_line` still
answers `Result`, so SKEIN-787 narrows both — which also reaches the two further dead `Err` arms
those signatures force outside this survey, in `fleet_lifecycle_refusal` and `src/volume.rs`. One
kept row is that shape rather than either of the two above: `cmd_resize`'s fallback exists only
because `fleet_lifecycle_refusal` is typed `Option` while its own doc opens "**Always**" (SKEIN-788).
Both need a file this pass did not own. The job changes from "write a better sentence" to "delete
the state that made a sentence necessary", which is smaller and does not come back.

---

## 1. `HealthCheck::unsatisfied` — the model, and it holds

This is the shape the rest of the tree should copy. `src/health/check.rs:76` takes the fix as its second
*argument*, so "a fault with no way out cannot be written without noticing", and `src/health/check.rs:54`
documents the field as never empty for an unsatisfied check. Twelve production sites, and **all
twelve carry a real fix**.

They are also the only family in this survey that satisfies requirement 3 by construction:
`src/web/app/boot.js:1375` re-polls `/api/health` every 15 seconds, `loadHealth` removes the banner
when `health.ok`, and `skein doctor` prints the same `fix` at `src/bin/skein.rs:536`. Recovery is
detected and the surface clears with nobody pressing anything.

| where | what a person sees | reach | reachable how | to do? | watchable? | v |
|---|---|---|---|---|---|---|
| `src/health/disk.rs:275` | the boxes' disk is N% full … | **R** — the sandbox's image store crosses `DISK_FULL_PCT`; a live `df` reading, nothing normalises it | health banner / `skein doctor`, above `DISK_FULL_PCT` | y — "`skein stop <box>` keeps its checkout, branch and conversation, or clear its build output in place"; the image-store arm adds a `docker system prune -af` line and is marked `destroys` | yes — the next 15s poll re-measures | C |
| `src/health/scratch.rs:84` | {path} in {whose} belongs to uid {owner}, and the fleet runs as uid {mine} — a `claude` that derives its own temp directory refuses to start there | **R** — another uid holds the shared `/tmp` scratch directory; the probe's own three-word answer | health banner; a shared `/tmp` taken by another uid | y — names `rm -rf {path}` on that machine and says why skein will not do it | yes — re-derived each poll | C |
| `src/health/sandbox.rs:102` | the host warden did not answer … | **R** — stop the host warden — `warden_client::sighting()` returns `None` | health banner; warden not running | y — two arms: "something is answering on {addr} and it is not a warden this skein can use", with `$SKEIN_WARDEN` and `$SKEIN_WARDEN_PORT` named | yes — `warden_report` on the poll; also printed once at startup, `src/bin/skein-server/main.rs:242` | C |
| `src/health/sandbox.rs:168` | started before the current isolation and still running under the old one: {boxes} | **R** — upgrade the isolation with boxes still running: `uncovered` non-empty | health banner after an upgrade | y — "`skein restart {box}` … Its checkout and its branch are untouched; whatever the agent was part-way through is not, so pick the moment" | yes — `cover_is_current`, recomputed each poll | C |
| `src/health/sandbox.rs:238` | ON but nothing is scoped, so every box holds … | **R** — gitgate on with no usable credential — `gitgate::scope_status()` answers `Unusable` | health banner; gitgate on with no usable credential | y — "Settings → GitHub & keys → add a GitHub App, or a per-repo token for each repo in use" | yes — `scope_status` on the poll | C |
| `src/health/report.rs:439` | … The kernel has killed N process(es) for memory since skein last looked | **R** — an OOM kill in the fleet; `pressure().killed > 0` is a kernel counter | health banner after an OOM kill | y — "give the fleet more memory (Settings → Fleet), or stop a box you are not …" | yes — the counter re-read each poll | C |
| `src/health/report.rs:474` | no memory ceiling anywhere: not per box, not on the boxes together, not on Docker … | **R** — `{"fleet_memory":""}` — `load_config` repairs `fleet_sandbox` and nothing else, so `parse_mib` returns `None` at its first `chars().last()?` and `memory_plan()` is `None` | health banner on a fleet created without limits | y — "`skein resize 26g` (or Settings → Fleet → memory; it rebuilds the sandbox and carries every box's work across)", marked `destroys` | yes, but the fix is destructive and must not be driven | C |
| `src/health/report.rs:511` | {registry error} | **R** — `SKEIN_REGISTRY=/no/such/file` with no repos added | health banner; `$SKEIN_REGISTRY` pointing at an unreadable file | y — "it is named by $SKEIN_REGISTRY or $SKEIN_SHARED — unset whichever is set, or point it at a readable file" | yes | C |
| `src/health/report.rs:522` | `{name}` is not on PATH, and skein needs it | **R** — start the server from a shell whose PATH lacks `git` | health banner; `git` missing | y — "install {name}, or start the server from a shell whose PATH has it" | yes — `program_on_path` each poll | C |
| `src/health/report.rs:543` | curl is not installed, and skein reads GitHub with it — pull requests, diffs, merges, and minting App tokens | **R** — the same, without `curl` | health banner | y — "install curl" | yes — `have_curl` each poll | C |
| `src/health/report.rs:634` | {box} durable agent guidance is unavailable; … | **R** — a box whose probe install did not run — `agent_guide != "installed"` | health banner; probes not installed | y — "restart the server, which recreates every repo's store and reinstalls the probes into it; a box that is missing tmux or jq needs `skein restart <box>` after that" | yes | C |
| `src/health/report.rs:644` | {mailbox errors} | **R** — a box missing `jq` or its mailbox directory | health banner | y — "a missing mailbox directory is created by restarting the server; a box missing jq needs `skein restart <box>`, which reprovisions it" | yes | C |

## 2. `HealthCheck::unknown` — the fix field is empty by construction

`src/health/check.rs:85` documents `unknown` as "could not be answered — `detail` says why it could not,
not what is wrong", and `src/health/check.rs:155` asserts the `fix` is empty. That is a defensible rule
for *not a fault*, and it is also how a person ends up reading a `!` on their board with nothing
under it. Every row here is watched by the same 15s poll, so requirement 3 is met and requirement 2
is not — **except the first row, which now carries its next step inside `detail`** (SKEIN-770). That
is the shape available to an `unknown`, and the remaining rows in this table could take it too.

| where | what a person sees | reach | reachable how | to do? | watchable? | v |
|---|---|---|---|---|---|---|
| `src/health/disk.rs:45` | the sandbox has not answered the disk reading since skein started, and which half failed cannot be told apart from here … Ask the same question directly with `df -Pm {root}`; skein re-asks at most every 30 seconds and this clears itself the moment one reading arrives, with nothing to restart | **R** — and now for the reason it gives (SKEIN-770). It read *"no fleet sandbox is configured, so there is no filesystem to measure"*: `fleet_resources()` had two `None` paths, the blank-name one was foreclosed by `load_config` (`src/config.rs:482`) and SKEIN-756 deleted it, so what is left is the measurement failing — either the command did not run (`src/place/run.rs:92`, `src/place/run.rs:93`) or it printed nothing `parse_resources` could read (`src/fleet/resources.rs:279`), which this cannot tell apart and says so | health banner and `skein doctor`, whenever the fleet's own `/proc` and `df` reading has not landed once since startup | y — `df -Pm` against the root `resource_script` measures, and the row says nothing needs restarting | yes, and already: the 30s gate keeps asking and the 15s health poll (`src/web/app/boot.js:1375`) redraws | C |
| `src/health/disk.rs:191` | the sandbox is not answering, so its disk figures are the last ones that arrived — and they carry no total | **R** — catch a poll while `sbx` is wedged: `r.disk_total == 0` with `r.stale` | health banner while `sbx` is wedged | n | yes — `sbx` answering; already polled | N |
| `src/health/scratch.rs:69` | {whose} could not be asked whether anything has taken the model's temp directory ({why}) | **R** — the scratch probe's `exec` fails | health banner | n | yes — the probe answering | N |
| `src/health/scratch.rs:99` | {whose} answered something this cannot read ({line}), so whether anything has taken the model's temp directory is unknown | **R** — the probe answers something that is neither one word nor three | health banner | n | yes | N |
| `src/health/sandbox.rs:24` | `sbx ls` did not answer just now; showing the last successful snapshot (N boxes) | **R** — `sbx ls` answers once and is unresponsive at a later poll | board / health banner | n — but it says which reading it is showing, which is the honest half | yes — the next `sbx ls` | N |
| `src/announce.rs:792` | *(a fixture string, not a message)* | **X** — `src/announce.rs:792` is inside that file's `#[cfg(test)] mod tests`, which begins at `src/announce.rs:604`. This row quoted *"no fleet sandbox is configured"* as though it were something a person could be shown; it is a string handed to `HealthCheck::unknown` so `announce_disk` sees an `Unknown` **level**, and nothing has ever read the words. SKEIN-772 replaced it with "the disk could not be measured", because a fixture is the last place a deleted sentence survives — and the wording verdicts in the last three columns are therefore about a string nothing prints, kept as measured rather than restated. **Dequeued by SKEIN-777 — code KEPT**, on the strength of this cell: a `#[cfg(test)]` fixture is not a message, so there is no wording to fix. The fixture itself stays, because the test needs *some* string to hand `HealthCheck::unknown` | not reachable at all: no production path constructs it | n | yes | N |
| `src/bin/skein-server/health.rs:35`, `src/bin/skein-server/health.rs:37-47` | the health check itself failed: {error} — and eleven checks reading "the health check itself failed" | **?** — only on a `JoinError` from `spawn_blocking(health_report)`. No `unwrap`/`expect`/`panic!` outside `mod tests` in `src/health.rs`; a panic could still come from `fleet`, `sbx`, `gitgate` or `warden_client` underneath it. Settled by finding one panic site in that call tree, or by proving there is none | health banner, whenever `health_report` panics | n — a person is told every subsystem is unknown and given nothing | yes, and already: `ok: false` keeps the banner up and the 15s poll clears it when the task next succeeds | N |

### The `no fleet sandbox` family, and the five sites that stay

Rows 205 and 210 were the last two of a family of guards on a blank `fleet_sandbox`. SKEIN-756
deleted 21 of them, SKEIN-772 the remaining four in the files 756 did not own — `src/reviewbox.rs`
(`theirs`), `src/takeover.rs` (`replacement_name`, where it was the *else* that was dead) and
`src/volume.rs` twice (`move_to`'s recipe and `migrate`'s refusal). Census before and after, whole
rather than headed:

```sh
grep -rn "sandbox.is_empty()\|sandbox\.trim()\.is_empty()\|fleet\.is_empty()" src/ | wc -l   # 11 -> 7
```

**Five of the seven that remain are code, and none of them is dead — read them before cutting.**
(The other two are the prose in `src/fleet/mod.rs`'s module doc that records why the rest went.)

- `src/config.rs:482` — the repair itself. Everything above rests on it.
- `src/bin/skein.rs:933` — `skein doctor`'s, kept deliberately: `src/fleet/mod.rs`'s module doc records
  that a command whose job is reporting on invariants is the one place to state this one.
- `src/update.rs:488` — `settle(sandbox: &str)` takes its argument, so a caller may pass anything.
- `src/place/record.rs:266` — a filter over *recorded placements*, where an empty sandbox is a real
  recorded value rather than an impossible configuration.
- `src/bin/skein-server/fleet.rs:269` — **reachable, and the only live member left.** It reads the field
  inside `update_config`'s closure, and `update_config` loads through `read_config()`
  (`src/config.rs:557`) rather than `load_config()` — so the blank-name repair has not run and a
  `config.json` carrying `"fleet_sandbox": ""` present-and-empty reaches it. Not a survey row (it is
  an `Err` string on a create request) and not touched here.


## 3. `skein doctor`

`cmd_doctor` is a one-shot report, so requirement 3 reads differently: re-running the command *is*
the poll. Where a doctor line reproduces a `HealthCheck`, it prints that check's `fix`
(`src/bin/skein.rs:536`, `src/bin/skein.rs:604`, `src/bin/skein.rs:699`, `src/bin/skein.rs:720`,
`src/bin/skein.rs:764`, `src/bin/skein.rs:783`) and the row conforms by inheritance. The
rows below are doctor's own hand-rolled lines.

| where | what a person sees | reach | reachable how | to do? | watchable? | v |
|---|---|---|---|---|---|---|
| `src/bin/skein.rs:460` | ✗ registry {error} | **R** — `skein doctor` with `$SKEIN_REGISTRY` unreadable — the same read as row 205's neighbour above | `skein doctor` with an unreadable registry | partly — the next line prints `registry_origin`, which is where, not what to do | yes — the file becoming readable | N |
| `src/bin/skein.rs:518` | ✗ {prog} not on PATH — {why} unavailable | **R** — `skein doctor` from a shell without `git` or `curl` | `skein doctor` without `git` or `curl` | **n** — and `src/health/report.rs:524` answers the same condition with a fix | yes | N |
| `src/bin/skein.rs:564` | ✗ model {runtimes} on, and {reason} | **R** — a model configured whose test call does not answer | `skein doctor` with a model configured that will not answer | partly — `src/bin/skein.rs:577` prints the transport's own detail, including the whole PATH, where there is room | yes — a test call answering | W |
| `src/bin/skein.rs:630` | ! github token none — the review queue reads PRs as you, and nothing here names a user | **R** — no GitHub credential anywhere — the fresh-fleet default | `skein doctor` with no GitHub credential | y — "`gh auth login` on this host, export GH_TOKEN, or add a read token in Settings → GitHub & keys" | yes | C |
| `src/bin/skein.rs:665` | ✗ logins {runtime} expired {date} | **R** — a runtime credential that has expired | `skein doctor` after a fleet-wide logout | y (`src/bin/skein.rs:668`) — "every box holds the same dead token … one `skein login {runtime}` heals the whole fleet" | yes — and the cockpit already watches it, see §7 | C |
| `src/bin/skein.rs:677` | ! logins none — `skein login <runtime>` signs the fleet in once | **R** — `skein doctor` on a fleet nobody has logged in on | `skein doctor` on a fleet with no login | y — the command is the message | yes | C |
| `src/bin/skein.rs:738` | ✗ box ceilings N running outside the fleet's ceiling: {boxes} | **R** — `uncapped_boxes()` non-empty — real per-box cgroup state | `skein doctor` | y (`src/bin/skein.rs:747`) — "a runaway build in one of these reaches the whole sandbox; `skein restart <box>` puts it under the current plan" | yes — `uncapped_boxes` | C |
| `src/bin/skein.rs:824` | ✗ review {repos}: {why} | **R** — a repo whose review queue will not build | `skein doctor` with a repo whose queue will not build | **n** | yes — the next queue build; the cockpit's own copy of this at `src/web/app/review.js:553` does add "try again" | N |
| `src/bin/skein.rs:835` | ! kit not written yet (server startup / `skein add` installs it) | **R** — `skein doctor` before the first server start writes the kit | `skein doctor` before first start | y — names both things that install it | yes — the file appearing | C |
| `src/bin/skein.rs:842` | ✗ settings unreadable — {why} | **R** — a malformed `config.json` — `config_error()` is `Some` | `skein doctor` with a malformed config | partly (`src/bin/skein.rs:845`) — "every setting below is a fallback default, not your choice; skein has not overwritten the file". Says what is true, not what to do | yes — the file parsing | N |
| `src/bin/skein.rs:870` | ! boxes push nothing chosen — no GitHub credential is placed in a box | **R** — no box credential chosen; all three paths are opt-in, so this is the fresh default | `skein doctor` | y (`src/bin/skein.rs:878`) — "Settings → GitHub & keys: a GitHub App, or a per-repo token" | yes | C |
| `src/bin/skein.rs:896` | ! gh secret not seeded, and nothing can seed one … | **R** — `seed_gh_secret: true` in the config; in-fleet nothing can seed it, so it then fires every run | `skein doctor` with `seed_gh_secret` on | y — "Scope per repo instead — Settings → GitHub & keys" | yes | C |
| `src/bin/skein.rs:918` | ! ssh agent no keys loaded (SSH git push from boxes will fail …) | **R** — no keys in the host's ssh-agent | `skein doctor` | y — "run `ssh-add <key>` on the host; the key file is there and skein cannot read it from in here" | no — the act is on the host, off this machine | U |
| `src/bin/skein.rs:939` | ✗ fleet no sandbox named, which load_config is supposed to make impossible — the board will show this fleet as empty | **X** — `cfg` is `load_config()` (`src/bin/skein.rs:837`), which repairs a blank `fleet_sandbox` (`src/config.rs:482-484`). Kept deliberately as this tree's ONE statement of that invariant — the line says so itself. **Dequeued by SKEIN-777 — code KEPT**, deliberately: re-verified 2026-09-11, still at this address and still the only place the tree states the invariant | unreachable by construction; the comment at `src/bin/skein.rs:934` says `load_config` repairs a blank name and the line is kept as an invariant tripwire | n, deliberately | n/a | N |
| `src/bin/skein.rs:911` | ✗ sandbox reported absent, which cannot be true — skein is running inside this fleet, so this is a bug in the check and not a fact about the fleet | **X** — `fleet_exists` is `(sandbox == fleet_sandbox()).then_some(true)` (`src/fleet/create.rs:569`); `bool::then_some` yields only `Some(true)` or `None`. `Some(false)` is not a producible value. **Dequeued by SKEIN-777 — code DELETED**: the arm is folded into `_`, so this address is historical. The `Option<bool>` that forced it is still there — narrowing it is SKEIN-787 | unreachable by construction (`src/bin/skein.rs:948` argues why) | n, deliberately | n/a | N |
| `src/bin/skein.rs:959` | ✗ sandbox cannot tell if it exists — {sbx failure} | **R** — **and the survey's stated trigger is wrong** — in-fleet `fleet_exists` never calls `sbx`. The real one: save a different `fleet_sandbox` from the cockpit's Settings while `skein doctor` is between its `load_config()` at `src/bin/skein.rs:838` and its `fleet_exists` call, a window seconds wide because the sandbox probes run in it | `skein doctor` with a wedged `sbx` | **n** | yes — `fleet_exists` answering | N |
| `src/bin/skein.rs:944` | ! create line cannot be worked out — {why} | **X** — `create_line` (`src/fleet/create.rs:22`) has one return and it is `Ok(...)`; its only fallible call, `ensure_fleet_kit`, is `eprintln!`'d rather than propagated. **Dequeued by SKEIN-777 — code DELETED**: the `match` became `if let Ok`, so this address is historical. The `Result` that forced it is still there, with two more dead `Err` arms behind it in `src/fleet.rs` and `src/volume.rs` — SKEIN-787 | `skein doctor` | **n**, and this is the line a person is sent to when the cockpit is down | yes — `create_line` succeeding | N |
| `src/bin/skein.rs:1011` | ! this sandbox now reports {n} CPUs, so it is not the one that was approved | **R** — a sandbox whose CPU count no longer matches what was recorded | `skein doctor` on a fleet whose shape changed | n | yes — the recorded size matching | N |
| `src/bin/skein.rs:1018` | ! fleet size nobody stated this fleet's memory or CPUs; it has {n} CPUs | **R** — any fleet predating `~/.skein/fleet-size`, or one whose bootstrap did not write it | `skein doctor` | partly (`src/bin/skein.rs:1022`) — "sbx fixes both at create and has no resize, so changing them means rebuilding the sandbox" is a constraint, not a step | no — it is a fact about `sbx`, not a condition | U |
| `src/bin/skein.rs:1050` | ✗ {tool} missing in the sandbox — {why} | **R** — a sandbox image without `bwrap`, `tmux` or `git` — a live `command -v` probe | `skein doctor` on a fleet without `bwrap`, `tmux` or `git` | **n** — and each `{why}` says a box cannot work without it | yes — the probe finding it | N |
| `src/bin/skein.rs:1131` | ✗ ceilings no cgroup delegation — boxes run UNCAPPED, so one runaway build can kill every other box | **R** — a host without cgroup delegation; the `sudo mkdir /sys/fs/cgroup/skein` probe fails there | `skein doctor` | **n** | yes — the `mkdir` probe succeeding | N |
| `src/bin/skein.rs:1146` | ✗ {cgroup} {said} | **R** — a cgroup left `max` that should be capped, or a numeric cap an older skein wrote | `skein doctor`; text comes from `ceiling_reading` | not established — the sentence is `fleet`'s, and the comment at `src/bin/skein.rs:1142` says the judgement moved there deliberately | yes — the cgroup file being rewritten | N |
| `src/bin/skein.rs:1207`, `src/bin/skein.rs:1214` | ✗ mount {path} — not visible in the sandbox; boxes for it would come up with no store / NOT MOUNTED, though directories under it are … | **R** — a repo registered after the sandbox's mounts were fixed at create | `skein doctor` on a fleet created with a short create line | y — both arms end with "`skein resize {size}` rebuilds it with the create line above and carries every box across" | no — the rebuild is a host act | U |
| `src/bin/skein.rs:1225` | ! mount {path} — the directory is there, but nothing in this namespace is mounted at it | **R** — run `skein doctor` inside a box rather than at fleet scope | `skein doctor` run from inside a box | y — "Run this at fleet scope rather than inside a box" | yes — trivially, the scope it is run at | C |
| `src/bin/skein.rs:1247` | ! review loop {what this repo has built} | **R** — a repo with automatic review plus a merge train | `skein doctor` with automatic review plus a merge train | not established — text is `the_loop_this_repo_has_built`'s | yes — a settings change | W |

## 4. The CLI funnel

Every subcommand's `Err(String)` lands at `src/bin/skein.rs:170`, printed as `skein: {e}` before
`exit(1)`. Twenty-one literal sites; the eight `usage:` strings are one row because they are the
same shape and all of them conform.

| where | what a person sees | reach | reachable how | to do? | watchable? | v |
|---|---|---|---|---|---|---|
| `src/bin/skein.rs:62` | usage: skein add `<git-url>` [--id `<id>`] [--agent `<runtime>`] … | **R** — mistype any of the nine subcommands that print one | mistyping any of the nine: `grep -n 'usage: skein' src/bin/skein.rs` prints eleven lines, ten of them a usage line and the eleventh (`unknown shared action`) carrying the phrase inside another sentence | y — the usage line is the step | no — the person's own next keystroke is the recovery | C |
| `src/bin/skein.rs:143` | no fleet sandbox is configured (fleet_sandbox in config.json) | **X** — `fleet_sandbox().is_empty()`; foreclosed by `src/config.rs:482-484`. Deleted by SKEIN-756. **Dequeued by SKEIN-777 — code already gone**, re-verified 2026-09-11: `grep -rn 'no fleet sandbox is configured' src/` returns no production site | `skein cockpit-stop` with a blank sandbox | partly — names the field, not how to set it or where | yes — config gaining a name | N |
| `src/bin/skein.rs:165` | unknown command {cmd} (try: skein help) | **R** — mistype a verb | mistyping a verb | y | no — a keystroke | C |
| `src/bin/skein.rs:261` | unknown shared action {action}; usage: skein shared import `<box>` | **R** — `skein shared <anything but import>` | `skein shared <anything else>` | y | no — a keystroke | C |
| `src/bin/skein.rs:1368` | could not refresh {repos} — a box created now clones from the remote instead | **R** — `skein pull` with the remote unreachable | `skein pull` with an unreachable remote | partly — states the consequence, not a step | yes — `fetch_mirror` succeeding | N |
| `src/bin/skein.rs:1444` | no branch for box {name}; pass --branch `<branch>` | **R** — a registered box with no recorded branch and no `--branch` | `skein start` on a box with no recorded branch | y | no — a keystroke | C |
| `src/bin/skein.rs:1449` | unsupported runtime {agent} | **R** — `skein start --agent <typo>` | `skein start --agent <typo>` | **n** — and `src/bin/skein.rs:1696` prints the available list for the identical check | no — a keystroke, but the person cannot make it without the list | N |
| `src/bin/skein.rs:1485` | nothing to run | **X** — `argv.split_first()` is `None` only for an empty slice, and both producers are total: `Place::interactive_argv` (`src/place/argv.rs:331`) always pushes at least `wrap(script)`, and its early return `refusal_argv` (`src/sandbox.rs:1017`) is three elements. **Dequeued by SKEIN-777 — code KEPT** — this one is not the shape of the two deleted above it. Their deadness is the TYPE's (`then_some`, one `Ok` return); this one rests on two producers staying total, which a later edit can break. That makes it the `src/bin/skein.rs:939` shape — a tripwire that can still trip — so the message stays and only the wording work stops | not established — an empty argv from `attach_argv` | n | none | N |
| `src/bin/skein.rs:1489` | {program} exited non-zero | **R** — the attach command exits non-zero | `skein attach` where the attach command fails | **n** — no output, no exit code, no next step | yes — the box's session existing | N |
| `src/bin/skein.rs:1491` | sbx not found on PATH — attaching from the host needs the sbx CLI | **R** — `skein attach` from a host without `sbx` on PATH | `skein attach` on a host without `sbx` | y — names what is needed | yes — `sbx` appearing on PATH | W |
| `src/bin/skein.rs:1494` | running {program}: {io error} | **R** — any other IO error spawning the attach program | `skein attach` with a broken program | n | yes | N |
| `src/bin/skein.rs:1602` | N of M boxes could not be copied out ({names}) — that work is still only inside {sandbox}, and a destroy would take it | **R** — `skein save` where some boxes fail to copy out | `skein save` with a partial failure | partly — the stake is stated plainly; no per-box retry is named | yes — a re-run of `save_boxes` for the named boxes | N |
| `src/bin/skein.rs:1640` | skein is running inside the fleet sandbox, so it cannot resize it from here — and no sandbox is named in the settings … `skein doctor` prints what it can work out about this installation | **X** — the `.ok_or(…)` fallback of `fleet_lifecycle_refusal`, which has one exit and it is `Some(why)` — its own doc says "**Always** (SKEIN-576)". SKEIN-756 removed its second `Some` return; there was never a `None`. **Dequeued by SKEIN-777 — code KEPT** for now, because deleting it means narrowing `fleet_lifecycle_refusal` to `String`, which needs `src/bin/skein-server/` — SKEIN-788 | `skein resize` with no sandbox configured | y | no — the act is on the host | U |
| `src/bin/skein.rs:1645` | {refusal}{note} — the `fleet_lifecycle_refusal` text plus "The size you asked for ({asked}) is not in that create line … Edit the flags in the line to the size you want." | **R** — `skein resize <anything>` from inside the fleet — it refuses every time, by design | `skein resize` | y — the refusal renders the destroy and create lines to run on the host | no — off this machine | U |
| `src/bin/skein.rs:1669` | unsupported runtime {runtime} | **R** — `skein login <typo>` | `skein login <typo>` | **n** — same omission as `src/bin/skein.rs:1449` | no | N |
| `src/bin/skein.rs:1695` | unsupported runtime {agent}; available: {list} | **R** — `skein attach --agent <typo>` | `skein attach --agent <typo>` | y — the list is the step | no — a keystroke | C |

## 5. The websocket refusals — `terminal_session`, `login_session`, `pump_pty`

**Already owned: SKEIN-702**, which is decided and blocked only on file ownership. Included so the
table is complete, and because measuring them changed one thing: the defect is not only that some
say nothing, it is that *none of the six early returns sends a close code*, so
`src/web/app/board.js:857` reads 1006, decides the connection went away, and covers the sentence with
the reconnect card. `CLOSE_CHILD_ENDED` (`src/bin/skein-server.rs:4469`) is only sent at `src/bin/skein-server.rs:4405`,
which is reached solely when `pump_pty` returned a child's exit code.

> **This section landed (SKEIN-702), and the table below is the measurement, not the state.**
> The rows are left exactly as they were read on 2026-09-09 — their line numbers and their verdicts
> are what the survey found, and rewriting them would destroy the only record of what was wrong. What
> is true now, in one paragraph, so nothing here reads as an open defect:
>
> * There are **eight** early returns in these three functions, not six. The two the survey did not
>   count are `login_session`'s own pair — the PTY cap at `src/bin/skein-server.rs:4758` and `login_spawn_argv`'s refusal at
>   `src/bin/skein-server.rs:4768`, the row below that reads *no fleet sandbox configured*. Every one of the eight now writes
>   its sentence, closes with a code, and waits for the close to be read.
> * `CLOSE_CHILD_ENDED` **is now `CLOSE_NOTHING_TO_RECONNECT`**, the same 4001. The rename is the
>   fix rather than tidying: a refusal has no child and nothing ended, and the page's question was
>   only ever whether to offer a reconnect. Every mention of the old name in this document is
>   historical.
> * **Two of the conditions are now watched and recover with no click.** The PTY cap: the release of
>   a permit publishes `stream::Tick::PtyFreed`, and a pane refused for the cap reconnects on hearing
>   it. A missing box: the pane reconnects when the board reports that box. `pump_pty`'s four keep
>   the control and say that they keep it — which is the right pane for a condition nothing polls,
>   and is a different claim from there being nothing to poll. The verdicts below turn on that
>   difference.
> * **Verdicts, in the survey's own letters — and this bullet used to name its rows by the addresses
>   they were measured at, which is the habit §0b gave up.** Of the seven labels it carried, exactly
>   one still names the row it meant: `src/bin/skein-server.rs:4253`, the box terminal's cap, which stayed put because the
>   wording it names is gone and the ledger pins it there. Five (`src/bin/skein-server.rs:4307`, `src/bin/skein-server.rs:4758`, `src/bin/skein-server.rs:4501`, `src/bin/skein-server.rs:4512`,
>   `src/bin/skein-server.rs:4521`) are in no `where` cell at all, their citations having been repaired and moved. And
>   `src/bin/skein-server.rs:4491` is worse than either: it is still a `where` cell, one row further down, so that label
>   now resolves to `ensure_box_session`'s row — a verdict this bullet was not talking about. So each
>   row is named here by what it IS, re-read against the tree on 2026-09-12:
>   * **The box terminal's PTY cap — W → C.** `pty_limit_reached` quotes the limit and promises the
>     pane comes back (`src/bin/skein-server/terminal.rs:270`); the refusal closes with `AFTER_WAIT_PTY`
>     (`src/bin/skein-server/terminal.rs:92`); releasing a permit publishes `Tick::PtyFreed`
>     (`src/stream.rs:243`); the server forwards that as `pty-freed` (`src/bin/skein-server/events.rs:79`);
>     and the page reconnects every pane waiting on it (`src/web/app/boot.js:486`), offering no
>     control, because a control beside a promise to come back is an invitation to press something
>     that was not needed (`src/web/app/board.js:715`). Watched, and it recovers unasked.
>   * **The absent box — W → C.** The same shape one token along: the refusal adds
>     `watching_for_box` (`src/bin/skein-server/terminal.rs:276`) and the board's own ticks drive the
>     reconnect (`src/web/app/boot.js:462`).
>   * **The login modal's copy of the PTY cap — W → C on the other arm of C**: it is not watched,
>     deliberately, because that surface is a modal that closes with its socket and one that reopened
>     itself over whatever somebody had moved on to would be worse than the walk back
>     (`src/bin/skein-server/terminal.rs:667`), so it names the control still on screen behind it
>     (`src/bin/skein-server/terminal.rs:677`).
>   * **`pump_pty`'s own four — N → W, and NOT to C** (SKEIN-883, which is the poll). Requirement 2
>     is met: `skeins_own_fault` says whose failure it is, that nothing will reopen the pane by
>     itself, and which control to use (`src/bin/skein-server/terminal.rs:304`), and `AFTER_NO_WATCH`
>     (`src/bin/skein-server/terminal.rs:405`) is what puts that button on the page instead of a spinner
>     (`src/web/app/board.js:719`). Requirement 3 is open, and that is what W is for: three of the four
>     rows below already NAME the condition in their `watchable?` cell — a pty slot freeing, the
>     program appearing on PATH, fd pressure easing — and the fourth is that same fd pressure one
>     call later. Each is a poll this tree knows how to write, `openpty` being the cheapest of them
>     because nothing has been spawned yet when it fails. C asks for the condition to be watched, or
>     for there to be genuinely nothing to watch ([§Summary](#summary)); a button offered in place of
>     a poll is neither, and reading it as C is what would hide requirement 3 on four more surfaces.
>   * **And then W → C for a box terminal, by a bounded poll** (SKEIN-883). None of the four
>     conditions announces the moment it clears, so the poll is the retry itself: a box terminal's
>     pump failures now close with `retry` (`src/bin/skein-server/terminal.rs:410`), and the pane
>     tries again at 3s, 10s, 30s, 60s and 120s through `retryWaiting`, saying when the next try is
>     and offering no button, then hands over to the button after the fifth
>     (`src/web/app/board.js:728`). `openpty` failing spawns nothing, so a try risks nothing. The
>     login modal's copy of the same four stays `no-watch` and keeps its button, for the reason the
>     cap refusal above gives. `tests/ui/recovery.mjs` drives the whole run on Playwright's clock and
>     counts the tries off the wire. The row letters below are still the 2026-09-09 measurement.
> * **Row `src/bin/skein-server.rs:4768` is the one that did not move, and it is a finding rather than an omission.** Its
>   four words now carry what to do *with* the answer — "nothing here changes by itself … press log
>   in again once it is [fixed]" — but the sentence itself is written in `login_spawn_argv`, which
>   SKEIN-702 did not own, and the survey's complaint about it stands: it names no setting, no page
>   and no command. §2 of this document is where that belongs, and it is six copies wide.
> * **Row `src/bin/skein-server.rs:4768` did not need moving at all, and nothing here could have told you so** (SKEIN-767).
>   `login_spawn_argv`'s only `Err` was `fleet_sandbox().is_empty()`, which `load_config` had already
>   made impossible; SKEIN-756 deleted it, so seven of these eight early returns are real and this one
>   is an arm no value can inhabit. Its close code and its "press log in again" line are work spent on
>   a pane nobody can be shown. Narrowing the signature so the arm goes with it is SKEIN-774.
> * **Row `src/bin/skein-server.rs:4768` is now GONE rather than fixed** (SKEIN-774). `login_spawn_argv` returns
>   `(&'static str, Vec<String>)` rather than a `Result`, and the
>   `match` in `login_session` that held the refusal arm is a single `let`. So of the **eight** the
>   bullet above counts, **seven remain** — each still writing its sentence, closing with a code and
>   waiting for the close to be read — and the eighth was not improved, it was deleted, along with
>   the signature that made it expressible. The row below still says what the survey found on
>   2026-09-09; it is the measurement, and the code it measured is no longer there. That is why this
>   is a bullet and not a struck-through row: *fixed* and *gone* are different outcomes, and a table
>   edited in place could not tell you which one this was.
> * The summary counts at the top are the 2026-09-09 reading and are not restated here — a count that
>   is edited in place stops being a measurement.
> * `tests/ui/recovery.mjs` is the check, and it asserts in pixels: `elementFromPoint` over every
>   written row of each refusal, plus the two recoveries happening with no click on the page at all.

| where | what a person sees | reach | reachable how | to do? | watchable? | v |
|---|---|---|---|---|---|---|
| `src/bin/skein-server.rs:4253` | skein: too many terminals open — close one and retry | **R** — hold 24 PTY permits (`PTY_LIMIT`, `src/bin/skein-server/terminal.rs:15`), then open a 25th terminal | opening a box terminal with every `PTY_LIMIT` permit held | partly — "close one and retry" is manual · **reworded by SKEIN-702 — `pty_limit_reached` says what to do, so y now; §5's note re-reads this row's letter as C** | **yes, the cleanest in the tree** — a permit freeing. `try_acquire` is deliberate (`src/bin/skein-server/terminal.rs:89`); a bounded wait would recover with nobody involved | W |
| `src/bin/skein-server/terminal.rs:149` | skein: box {name} does not exist … Run `skein start {name} --branch <branch>` on the host to try again. Do not run `sbx create` … | **R** — open `/terminal/<name>` for a box with no placement record — a stale bookmark, or one just destroyed | opening a terminal for a box with no placement | y — the best recovery sentence in the tree, and it names an anti-command too | yes — `shared_record` becoming `Some`; the browser already reconnects | W |
| `src/bin/skein-server/terminal.rs:110` | skein: handoff brief failed: {e} | **R** — open a handoff terminal for a box with no recorded session digest, or whose shared store will not resolve | opening a terminal with `?handoff=1` when `prepare_handoff` errors | n — and the session then proceeds *without* the brief | not established — what recovery would mean here is unclear from the code | N |
| `src/bin/skein-server/terminal.rs:117` | skein: handoff task failed: {e} | **?** — only on a `JoinError` from the handoff `spawn_blocking` (`src/bin/skein-server/terminal.rs:77`). `src/handoff.rs:23-63` propagates with `?` and has no visible panic; settled by auditing its callees for one | same, on a join error | n | not established | N |
| `src/bin/skein-server/terminal.rs:159` | skein: {e} from `ensure_box_session` | **R** — reattach to a placed box whose tmux/session in the sandbox is erroring | reattaching to a box whose tmux server died with its sandbox | n — a bare error, and the attach then usually fails again | yes — the box's session existing | N |
| `src/bin/skein-server/terminal.rs:240` | skein: {why} from `remember_launch_never_ran` | **R** — create a box in a sandbox with no working `skein` on PATH — the launch shell exits 127 or 126 | a create-a-box launch whose command never ran | inherits — this one *is* followed by the close code | yes | W |
| `src/bin/skein-server.rs:4622` | skein: pty error: {e} | **R** — exhaust the host's pty/fd limit, then open a terminal | any terminal when `openpty` fails — host out of ptys | **n** · **reworded by SKEIN-702 — `skeins_own_fault` says what to do, so y now; §5's note re-reads this row's letter as W** | yes — a pty slot freeing; nothing was spawned, so a backoff retry is safe | N |
| `src/bin/skein-server.rs:4632` | skein: spawn failed: {e} | **R** — `$SKEIN_ATTACH_CMD` naming a program that is not there | `sbx` absent, or `$SKEIN_ATTACH_CMD` naming a missing program | **n** · **reworded by SKEIN-702 — `skeins_own_fault` says what to do, so y now; §5's note re-reads this row's letter as W** | yes — the named program appearing on PATH | N |
| `src/bin/skein-server.rs:4643` | skein: pty reader: {e} | **R** — fd exhaustion right after the child spawns | `try_clone_reader` failing after the child spawned | **n** · **reworded by SKEIN-702 — `skeins_own_fault` says what to do, so y now; §5's note re-reads this row's letter as W** | yes — fd pressure easing | N |
| `src/bin/skein-server.rs:4652` | skein: pty writer: {e} | **R** — the same, one call later | `take_writer` failing | **n** · **reworded by SKEIN-702 — `skeins_own_fault` says what to do, so y now; §5's note re-reads this row's letter as W** | yes | N |
| `src/bin/skein-server.rs:4889` | skein: too many terminals open — close one and retry | **R** — hold 24 PTY permits, then open the login pane | clicking "log in" with every permit held | partly · **reworded by SKEIN-702 — the `{PTY_MAX}` wording names the control, so y now; §5's note re-reads this row's letter as C** | yes — a permit freeing | W |
| `src/bin/skein-server.rs:4899` | skein: no fleet sandbox configured | **X** — the only `Err` `login_spawn_argv` could return was `fleet_sandbox().is_empty()`, foreclosed by `src/config.rs:482-484` and deleted by SKEIN-756 — so this arm is now dead by inhabitedness. SKEIN-774 is narrowing the signature. **Dequeued by SKEIN-777 — code already gone**, re-verified 2026-09-11: SKEIN-774 landed, and `login_spawn_argv` (`src/fleet/fleetlogin.rs:526`) returns a tuple, so there is no `Err` for a caller to render | clicking "log in" on a fleet with no sandbox | **n** — four words, no variable, no page, no command. It was written in `login_spawn_argv` itself, which SKEIN-756 emptied of it and SKEIN-774 narrowed, so no line in the tree writes it now — this `n` is the 2026-09-09 reading | yes — `fleet_sandbox()` becoming non-empty | N |
| `src/bin/skein-server/terminal.rs:718` | logged in, but the post-login share failed: {e} | **?** — the text says the post-login share failed, but the branch is a `JoinError` from `spawn_blocking(after_login)`; a REAL share failure is caught earlier and rendered as a different, softer sentence (`share_outcome`). Settled by finding a panic in `after_login`'s call tree | finishing a cockpit login when `share_login_with_boxes` fails | n — the person cannot tell whether running boxes have the credential | **yes, and it is already fixed silently**: `heal_logins` (`src/bin/skein-server/main.rs:172`) runs a 60s ticker that repairs exactly this. The message does not say so | N |
| `src/bin/skein-server/terminal.rs:729` | skein: login exited {code} — nothing changed | **R** — abort the login TUI — `pump_pty` returns a non-zero code | aborting the runtime's login TUI | partly — "nothing changed" closes the loop | no — a person's own decision | U |

## 6. The server — startup stderr and HTTP bodies

A person watches `skein-server`'s startup output; the background tickers' lines reach a log. The
startup family shares one shape: **what broke, plus what will be worse later, and no step**. The
warden line in `main` (`src/bin/skein-server/main.rs`) is the only one that appends a `fix`, and it
does it by reaching for
`health::warden_report().fix` — the machinery §1 describes.

| where | what a person sees | reach | reachable how | to do? | watchable? | v |
|---|---|---|---|---|---|---|
| `src/bin/skein-server/main.rs:131` | skein-server: {argv complaint}, then exit 2 | **R** — `skein-server --not-a-flag` | a bad flag | y — the message is a usage complaint | no — a keystroke | C |
| `src/bin/skein-server/main.rs:142` | skein-server: {volume refusal}, then exit 1 | **R** — point `$SKEIN_HOME` at a moved or unreadable volume | starting against a moved or unreadable volume; text from `ensure_volume` | y — see §10, `src/volume.rs:104` and siblings name both branches | yes — `$SKEIN_HOME` matching | W |
| `src/bin/skein-server/main.rs:148` | skein: turn-state probe not installed ({e}); boxes will show live/stale only | **R** — a store the probe install cannot write | server start | n | yes — the probe file appearing | N |
| `src/bin/skein-server/main.rs:154` | skein: ensure_fleet_kit: {e} | **R** — the same, for the fleet kit | server start | **n**, and it is the only startup line with neither a consequence nor a step | yes | N |
| `src/bin/skein-server/main.rs:157` | skein: kit not installed ({e}); boxes will fail to provision | **R** — the same, for the box kit | server start | n | yes | N |
| `src/bin/skein-server/main.rs:164` | skein: could not heal the fleet sandbox ({e}); boxes may start with a stale launcher or stale ceilings | **R** — `heal_fleet` failing at startup — it did so on this box while probing SKEIN-756 | server start | n | yes | N |
| `src/bin/skein-server/main.rs:185` | skein: the docker watchdog could not be started ({e}); a dockerd that dies will stay dead and cost a fleet rebuild | **R** — OS thread creation failing (RLIMIT_NPROC) | server start | n — highest stake of the startup family | yes | N |
| `src/bin/skein-server/main.rs:280` | skein: {warden failure} then the `warden_report` fix | **R** — start the server with no host warden — the ordinary local state | server start with no warden | y — inherited from §1 | yes | C |
| `src/bin/skein-server.rs:285` | skein: ssh key not loaded ({e}); SSH git push from boxes may fail | **R** — an ssh key configured while the host agent is unreachable | server start | n | yes — `ssh-add -l` listing a key | N |
| `src/bin/skein-server/main.rs:587` | {doorway::missing()}, then exit 1 | **R** — set the doorway's inherited-only variable and start the binary directly rather than through the supervisor | server start without the doorway | not established — text is `doorway`'s | yes — the doorway appearing | W |
| `src/bin/skein-server/main.rs:597` | skein-server: {why} for a bad inherited fd, then exit 1 | **R** — **and the survey undersold it as "not established"** — a misconfigured socket-activation supervisor: `LISTEN_FDS=2`, or a `LISTEN_PID` naming another process (`src/doorway.rs:80-107`) | not established — a supervisor handing a bad fd | n | none | N |
| `src/bin/skein-server/main.rs:688` | skein-server: cannot accept connections ({e}) — N in a row. The usual cause is running out of file descriptors; the cockpit keeps trying. | **R** — fd exhaustion (EMFILE) in the accept loop | a server under fd exhaustion | y — names the cause, and says it keeps trying | yes, and it *does*: this is a retry loop that narrates itself | C |
| `src/bin/skein-server/main.rs:694` | skein-server: N consecutive accept failures ({e}) and not one connection ever served … Exiting rather than sitting up and quiet, which is indistinguishable from working. | **R** — the same, before a single connection has been served | the same, never having served | y — both branches named | n/a — it exits deliberately | C |
| `src/bin/skein-server.rs:3245` | skein: ssh key not loaded ({e}) | **R** — Settings → Save with an ssh key skein cannot load | pressing Save in cockpit settings | **cannot tell** — a person causes it and a person will not see it: it goes to the server's stderr while the Save returns 200 | yes | N |
| `src/bin/skein-server/boxes.rs:82` | invalid box name | **R** — any API call carrying a box name with `/`, a space, a leading `-`/`.`, or `..`; eighteen sites, derived rather than listed: `cat src/bin/skein-server/*.rs | grep -c '"invalid box name"'` | any cockpit action on a name with a disallowed character | **n** — never states the grammar; `warden/src/serve.rs:603` does, for the same class | no — a keystroke, but not one the person can make blind | N |
| `src/bin/skein-server/repos.rs:44` | no such repo | **R** — remove a repo in Settings while a stale tab still names its id; thirteen returns, derived rather than listed: `grep -n '"no such repo"' src/bin/skein-server/*.rs` prints fourteen lines, the fourteenth a comment quoting the message | a cockpit action against a repo that was removed | n | yes — the repo record appearing | N |
| `src/bin/skein-server/terminal.rs:48`, `src/bin/skein-server/terminal.rs:644`, `src/bin/skein-server/boxes.rs:192` | cross-origin terminal blocked · cross-origin stream blocked | **R** — open the cockpit from a LAN host outside the allowed set with `$SKEIN_ALLOWED_ORIGINS` unset | opening the cockpit on an origin not in `$SKEIN_ALLOWED_ORIGINS` | **n** — the variable that governs it is never named | yes — the origin being added | N |
| `src/bin/skein-server/boxes.rs:86` | a box is created on a branch | **R** — `POST /api/boxes/<name>` with a blank `branch` | POSTing a create with no branch | partly — implies the missing field | no — a keystroke | C |
| `src/bin/skein-server/boxes.rs:175` | no such act — it may have finished longer ago than the warden keeps them | **R** — poll an act more than `RETENTION` (30 min) after it ended — a sleeping laptop | polling an act that has aged out | partly — names the cause | no — the act is gone | U |
| `src/bin/skein-server/events.rs:25` | too many live boards open — close one and retry | **R** — open 64 board event streams, then a 65th | opening more boards than `EVENT_LIMIT` | partly — manual | yes — a permit freeing, exactly as §5's two PTY rows | W |
| `src/bin/skein-server/terminal.rs:647` | unsupported runtime | **R** — open the login websocket with a runtime id skein does not support | opening a login socket for an unknown runtime | n — no list, same omission as `src/bin/skein.rs:1449` | no | N |
| `src/bin/skein-server/boxes.rs:62`, `src/bin/skein-server/repos.rs:48`, `src/bin/skein-server/fleet.rs:168`, `src/bin/skein-server/fleet.rs:168` | {tokio JoinError}, at 500 | **?** — four `spawn_blocking` sites; `stream::acknowledge` was read and has no panic path, and `moduledocs::status`, `machine::sandboxes` and `fleet::pressure` were not traced. Settled by auditing those three | a panicking blocking task | **cannot tell** how the cockpit renders it; either way it is not a sentence | yes | N |
| `src/bin/skein-server/review.rs:481`, `src/bin/skein-server/fleet.rs:120` | {why}, at 503, forwarded from below | **R** — `sbx` wedged while the cockpit polls shape or machine sandboxes | the cockpit polling shape or sandboxes while `sbx` is wedged | inherits; the comment at `src/bin/skein-server/review.rs:478` refuses an empty list because the two causes "send a person to different places" | yes — and both are polled, so recovery is automatic | C |
| `src/bin/skein-server/boxes.rs:112` | {why}, at 409, from `act::begin` | **R** — double-submit a box create for the same name before the first act ends | starting an act while one is running | y — the comment at `src/bin/skein-server/boxes.rs:110` says the message names which act to watch | yes — the other act finishing | C |

## 7. The cockpit

`src/web/index.html` holds **32 failure toasts** and **14 in-place failure renders**. The page has
exactly two self-healing patterns, and they are good: the health poll (§1) and `revStaleTimer`
(`src/web/app/review.js:464`), a 4/8/16/32/64s re-ask that degrades to a visible "try again" control
after `REV_STALE_TRIES`. Everything below either copies those or does not.

| where | what a person sees | reach | reachable how | to do? | watchable? | v |
|---|---|---|---|---|---|---|
| `src/web/app/board.js:766` | session not connected / click to reconnect | **R** — any terminal websocket closing for a reason other than the child ending — a server restart does it | any box terminal whose socket drops with a code that is not `CLOSE_CHILD_ENDED` | partly — a control, but no diagnosis, and it covers the sentence that had one | yes — the page holds `es.readyState`, `lastTickAt` and the box's state on the stream. `reconnectSession` has four call sites, all of them a person acting | W |
| `src/web/app/board.js:258`, `src/web/app/board.js:272`, `src/web/app/box.js:17`, `src/web/app/box.js:27` | continue failed: {server error} · restart failed · stop failed · destroy failed | **R** — any of the four resume/restart/stop/destroy POSTs answering non-ok | the row action buttons on the fleet board. **Not** the other `continue failed` pair, at `src/web/app/box.js:40` and `src/web/app/box.js:44`, which is a different surface (SKEIN-891): the header's Continue N (`#contall`) or saying "continue all", one POST to `/api/resume-batch` for several boxes. Its first arm used to be unreachable — `api_resume_batch` answered `ok: true` on every path, so a batch that failed inside the server toasted "continuing 0" as a success. It now answers `ok: false` whenever a box it was asked for was neither continued nor held, with a sentence naming those boxes, each with its reason (`src/bin/skein-server/boxes.rs:619`, SKEIN-1137), and the page toasts that sentence (SKEIN-1132); "continue failed" is left as the fallback for an answer without one and for the request itself failing | **n** — verb plus "failed", in a 3.5s toast | yes — the box's own state on `/api/events`: a stop that failed leaves the box running | N |
| `src/web/app/voice.js:126` | send failed | **R** — `POST /api/mailbox` failing | Mailbox → Send with a failing POST | **n** — two words. The typed text survives, and nothing says so | yes — the HTTP status distinguishes a safely re-sendable 5xx from a 4xx | N |
| `src/web/app/panels.js:678` | could not run the test | **R** — the git-probe POST failing | Settings → GitHub & keys → test writes | **n** | yes — `health.gitgate` already carries the same fact on the 15s poll | N |
| `src/web/app/box.js:497` | could not load diff | **R** — the diff GET failing with the Diff tab open | box → Diff tab when the endpoint rejects | **n** — the ok path distinguishes causes in `d.note`; this one does not | yes — the box's run state | N |
| `src/web/app/box.js:482` | could not read {path} — {reason} | **R** — opening a file whose read fails — a race with the box vanishing | box → Files → click a file | n | yes | N |
| `src/web/app/box.js:236` | loading… | **R** — trivially: `loadSession` sets it before the fetch, so every open of the pane | box → session digest pane | **n**, and there is no failure arm at all: a rejected fetch leaves the spinner up for ever and only the global handler says anything | yes — the fetch settling. **The worst spinner in the file** | N |
| `src/web/app/review-rows.js:476` | skein could not reach its own summary for this PR: {error} | **R** — the in-flight review-brief read erroring | Review pane, a PR whose in-flight read failed | n — a raw fetch error in a row | yes, and half-built: it is flagged `transient`, and the next full load deletes it (`src/web/app/review.js:336`). Nothing on screen says so, and nothing triggers that load | N |
| `src/web/app/settings.js:744`, `src/web/app/settings.js:730` | — not saved: {server error}, per box · nothing was copied out: {e} | **R** — a save act returning a per-box error, or refusing for want of room | Settings → Save every box's work, partial failure | partly — the headline from `cockpit/src/saved.mjs:22` states the stake ("a destroy would take it"); the per-box line is a bare server string with no per-box retry | yes — the failed names are already a list; one endpoint away | N |
| `src/web/app/boot.js:1147` | the update failed; the log says where | **R** — Settings → Update skein failing | Settings → Update skein | partly — points at the pane below, which `loadUpdate` does refresh | yes — and the log tail at `src/web/app/boot.js:1130` is exemplary, retrying every 1.5s across the binary swap | W |
| `src/web/app/review-rows.js:1290` | Workflows are not running. {error} | **R** — a repo whose workflow file will not parse | Review → expand a PR whose repo has a malformed workflow file | **n** here, while `src/web/app/review.js:1581` answers the same fact with "the workflow file has a problem — fix it before editing here" | yes — the next `loadWorkflows` after an edit | N |
| `src/web/app/review.js:951` | {repo} — {error}, under "N repos' queues could not be built" | **R** — one repo's GitHub queue build failing — a bad token or a rate limit | Review pane, several repos failing | **n**, while the same fact in the clear-screen at `src/web/app/review.js:662` adds "try again" and `src/web/app/review.js:669` adds "Settings → Repos" | yes — `revStaleTimer` already exists, three lines away | N |
| `src/web/app/review.js:927` | GitHub said: {error}, under "the queue could not be built" | **R** — the whole queue fetch failing | Review pane, GitHub refusing | y — `src/web/app/review.js:934` adds "try again" and "Settings → GitHub & keys", and `src/web/app/review.js:928` keeps the remembered queue with a caveat | yes — `revStaleTimer` | C |
| `src/web/app/review.js:661`, `src/web/app/review.js:668` | {repo} — skein could not read this queue: {error} — try again · skein did not ask: {why} — Settings → Repos | **R** — the same data rendered on the cleared screen | Review pane cold, all repos failing | y | yes | C |
| `src/web/app/board.js:154` | sbx could not be asked ({status}) | **R** — type `foreign:` in the board filter while `sbx` is wedged | typing `foreign:` in the board filter | n | yes — but `askForForeign` runs only on demand | N |
| `src/web/app/boot.js:426` | board is Ns stale — the server stopped answering | **R** — the SSE producer stalling with the socket still open | the EventSource open, the producer wedged | **n** — and nothing re-dials. The sibling state at the same site, "reconnecting…", is honest because the browser is retrying | yes — a tick arriving; `lastTickAt` is already the clock | N |
| `src/web/app/boot.js:507` | reconnecting… | **R** — trivially: any `EventSource` error — a server restart | the stream erroring | y — implicit, and true: EventSource retries by itself | yes — already watched | C |
| `src/web/app/boot.js:633` | the fleet's {runtime} login was refused at {time}: {said} — summaries, critiques and workflows are declining model calls | **R** — a runtime credential expiring | an expired fleet credential | y — a "log in" button that opens a PTY (`login_terminal`, `src/bin/skein-server/terminal.rs:638`) | **yes, and watched** — the same 15s health poll clears `expired_logins`, and `src/web/app/boot.js:1221` re-reads on close. **The best surface in the product** | C |
| `src/web/app/boot.js:594` | {check}: {first sentence of detail} (+N more) | **R** — any of the ten checked health keys going unsatisfied | any unsatisfied health check | y — clicking opens Settings → diagnostics, where the `fix` is | yes — §1 | C |
| `src/web/app/review-rows.js:1164` | The brief could not be fetched — {error} Close the row and open it again to retry. | **R** — the brief fetch failing — `s.prose_failed` | Review row whose brief fetch fails | y — names the retry gesture, and `src/web/app/review.js:1414` clears the waiting flag deliberately so the row does not "sit on '…' for ever with nothing to retry it" | yes — but it asks for the gesture instead | W |
| `src/web/app/box.js:66` | something went wrong in the page: {first line of the exception} | **R** — trivially: the page's catch-all for any uncaught exception | any uncaught error or rejected promise | **n** — de-duped 60s, deliberately does not re-render | none — it is the catch-all | N |
| `src/web/app/settings.js:333`, `src/web/app/settings.js:339`, `src/web/app/settings.js:347`, `src/web/app/settings.js:621`, `src/web/app/settings.js:685`, `src/web/app/boot.js:1104` | {server string} · pull failed: {e} · couldn't save: {e} · could not start: {r.error} … | **R** — each a settings/sync/repo/update endpoint answering non-2xx. **Seventeen sites, derived rather than listed** (SKEIN-878): `grep -cE -e 'toast\(e\.message\)' -e 'toast\([^;]*\$\{e\.message\}' -e 'toast\([^;]*\$\{error\.message\}' -e 'toast\([^;]*\$\{r\.error\}' -e 'toast\([^;]*\$\{e\}' src/web/index.html` prints 18, of which `src/web/app/board.js:272` is the box-actions row above. The cells name the three identical bare copies and the three the message column quotes; the 2026-09-09 reading said fourteen, listed fifteen addresses, and one of the fifteen was the success toast one line above `src/web/app/settings.js:339`'s `.catch` | settings fields, repo actions, sync cards, update controls | **n** — a server string in a 3.5s toast, no step, no retry. The 2026-09-09 reading put 13 of §0's 32 failure toasts in this shape; the enumeration beside it derives seventeen by a pattern §0's does not reach, since a bare `toast(e.message)` carries none of the words §0 greps for | varies; each has a status code that distinguishes retryable from not | N |
| `src/web/app/box.js:488` | asking the box… | **R** — trivially: `loadDiff` sets it before the fetch, so every open of the Diff tab | box → Diff tab | n/a — a spinner that waits on a real `git` fork, and has a catch. Honest but unbounded, with no elapsed counter | yes — the fetch settling | C |
| `src/web/app/review.js:919` | asking GitHub… | **R** — a cold open of the Review pane, before the first queue fetch resolves | Review pane, cold open | n/a — waits on `/api/review`; `src/web/app/review.js:926` replaces it on failure | yes | C |
| `src/web/app/panels.js:290` | could not load package requests | **R** — the substrate GET rejecting while the package panel is open; it re-polls, so it clears itself | the package-request panel | n | **yes, and watched** — the panel re-loads every 20s while open (`src/web/app/boot.js:1383`), so it self-heals within a cycle | C |
| `src/web/v2.html:325`, `src/web/v2.html:330` | the change could not be read | **R** — at `src/web/v2.html:330` — click a stale PR row for a repo removed since the queue loaded: `api_pr_shape` answers 404 as **plain text**, so `answer.json()` throws. The sibling at `src/web/v2.html:325` is **X**: `shape_response`'s JSON error arms always set a non-empty `error`, and the plain-text arms throw before that check | v2 board → click a row | **n** — no retry control in the pane at all | yes | N |
| `src/web/v2.html:457`, `src/web/v2.html:515` | it could not be added · it was not accepted ({status}) | **R** — submit a repo source or branch that passes the client's empty-check and fails server-side | v2 add-a-repo, make-a-box | n — the button re-enables, unremarked | no — a keystroke | N |
| `src/web/v2.html:382` | — disconnected — | **R** — the v2 terminal websocket closing, for any reason | v2 terminal socket closing | **n** — written into the buffer, then nothing. No overlay, no retry. It does at least not cover the last line | yes — `src/web/v2.html:257` already auto-reconnects the *stream* every 4s; the terminal does not | N |
| `src/web/app/settings.js:503`, `src/web/app/settings.js:501` | Write token — {host} is not GitHub, and skein only holds GitHub push credentials, so this repo's boxes can commit but skein gives them nothing to push with. Nothing about your repo needs changing; this is a limit of skein. · Write token — this repo was registered from a path, and skein can no longer read a remote from one. Add it again by its GitHub URL and this becomes a token field. | **R** — any repo whose `slug` comes back empty: registered from a GitLab, Bitbucket or self-hosted URL (the first sentence), or a legacy entry registered from a path whose mirror has no GitHub `origin` (the second) | Settings → Repos → the repo's card, its Write token row | the path arm: **y** — re-add by URL; `repos::add_repo` replaces the entry with the same id in place and touches no box. The host arm: **no, and correctly** — see below the table | the path arm: nothing to watch, the person's own add re-renders the card. The host arm: none — nothing about the repo can change that would make a GitHub push credential mean something for it | — |

**The Write token row is the one place in this survey where a row legitimately stops at naming the
problem** (SKEIN-812, decided 2026-09-23), and it carries no verdict because the
three requirements presuppose a next step that this case does not have. The token exists to let a
box push to GitHub; a repo on another host is not there, and nothing its owner could do to the repo
changes that — the missing thing is non-GitHub push credentials in skein, which is a statement about
skein's support rather than a step. So the sentence says whose limit it is and that the repo is
fine, and stops. The add dialog says the same before the clone (`src/web/app/settings.js:1251`), so
nobody meets it for the first time on the card. **This is an exception, written down so it stays
one**: the row used to carry advice to give a host clone an `origin` (removed by SKEIN-588), which
was false advice added to fill the gap, and a rule with an undocumented exception is how the next
person re-adds it. The path arm is not the exception — it has a next step and says it.

## 8. The board's rows

`src/board.rs` has **no `Err(String)` sites at all**; its whole error surface is row fields, drawn
identically by the CLI table and the cockpit (module doc, `src/board.rs:3`). Every row here is
reached by *looking at the board* — no action needed, which makes them the most reachable
diagnoses in the product, and the least explained: each is a token, with the sentence that would
help sitting in a doc comment beside it.

| where | what a person sees | reach | reachable how | to do? | watchable? | v |
|---|---|---|---|---|---|---|
| `src/board.rs:307` | the word `error`, `ended` or `stale` in the state column | **R** — `skein stop` a box — `state_with` maps `Liveness::Stopped` to "stale" | looking at the board | **n** — one word | yes — `box_liveness` returning running, or a new hook edge | N |
| `src/board.rs:334` | the reason the turn failed, verbatim from the box's own probe | **R** — the probe reporting an `error`/`ended` turn outcome — the ordinary turn lifecycle | looking at the board | **cannot tell** — the text originates in the probe and `signals`, not here | yes — a fresh non-outcome status | N |
| `src/board.rs:340` | why the turn stopped | **R** — any paused agent — `classify_message` on an ordinary tick | looking at the board | n | yes — a new turn starting | N |
| `src/board.rs:345` | `permission`, `question`, `trust` or `auth` | **R** — the agent's screen showing a permission, question, trust or auth dialog | a blocked agent | partly — the doc at `src/board.rs:343` says "each wants a different move from you, so the row names it instead of saying 'decision'". It names the *kind* of move, not the move | yes — the dialog clearing | N |
| `src/board.rs:357` | `never`, `misfiled` or `stale` for hooks | **R** — a box just started (no hook file yet), a reused name, or a session predating the installed probe | a box whose signals are not arriving | partly — the doc at `src/board.rs:350` sends the reader to `hook_health` for "what each one asks of a person"; the row carries only the token | yes — a correctly-named signal appearing | N |
| `src/board.rs:367` | `none`, `stale`, `unreadable`, `unsupported`, `newer`, `misfiled` | **R** — all six values are live branches of `screen_health`; the same drift as row 131 | a box blind to its own screen | **n** | yes — a fresh parseable observation, which `pane_usable` already computes | N |
| `src/board.rs:362` | `screen`, `edge`, `edge-ahead` | **R** — the turn-state fusion's three rules, each with a real bug behind it (docs/architecture.md §2.2) | looking at the board | n | yes — the observer catching up | N |
| `src/board.rs:385` | a sandbox skein did not place, hidden until the `foreign:` filter reveals it | **R** — type `foreign:` in the board filter with a sandbox `sbx` made directly | typing `foreign:` | n | yes — a placement record appearing | N |
| `src/board.rs:420` | `older` — the box's isolation cover is out of date | **R** — upgrade the binary while an older-covered box is running | after an upgrade | n at the row; §1's `cover_health` carries the sentence and the cost | yes — `cover_is_current`, computed every tick | N |
| `src/board.rs:435` | `uncapped no-cgroup-delegation` (or `no-limit-computed`, `could-not-join-cgroup`) | **R** — start a box on a host without cgroup v2 delegation — `box-session.sh` writes the reason | a box started outside the ceiling | partly — the doc at `src/board.rs:430` distinguishes the three, "need a different fleet rather than a different setting"; the row shows the token | yes — the launcher writing `capped` at the next start | N |
| `src/board.rs:445` | this box's credential is not scoped to its own repo | **R** — a fleet that can scope credentials with a box whose token is not scoped yet | looking at the board | n | yes — `box_is_scoped` flipping | N |

## 9. `src/fleet/`

The largest module, and the one where the two halves are furthest apart: it holds both the best
recovery messages in the tree and the most-duplicated dead end.

**A correction, because the item that prompted this survey cites it.** SKEIN-679 records
`src/fleet/resize.rs:185` as telling a person "`skein resize 8g` is safe to re-run — creating the sandbox
is idempotent, so it retries only the step that failed". **That sentence is gone.**
`src/fleet/resize.rs:185` is a line inside `save_boxes`, and the only surviving occurrence of the sentence
is a test doc comment at `src/fleet/resize.rs:1214` explaining why it was deleted — with
`the_resize_stops_at_the_destroy_and_promises_nothing_beyond_it` (`src/fleet/resize.rs:1222`) asserting it
stays deleted. The repository now enforces its absence.

**A second finding worth its own line: a well-written family of refusals is currently unreachable.**
`resize_fleet` has one caller (`src/bin/skein-server/fleet.rs:310`), gated at `src/bin/skein-server/fleet.rs:307` by
`fleet_lifecycle_refusal`, which since SKEIN-576 always returns `Some` (`src/fleet/create.rs:126`). So
every refusal inside `resize_fleet_inner` is dead to a person today. Two of them name no next step,
and would ship as dead ends the moment that gate changes. They are kept below rather than
dropped, and every one of their `reach` cells names the gate. The word **gated** used to sit in
their `where` column instead, which is exactly what blinded the misanchor gate to those rows
(SKEIN-876): unbackticked words in a site cell and `claimed` cannot tell the column from prose.

| where | what a person sees | reach | reachable how | to do? | watchable? | v |
|---|---|---|---|---|---|---|
| `src/fleet.rs:2059`, `src/fleet.rs:5112`, `src/fleet.rs:6941`, `src/fleet.rs:6998`, `src/fleet.rs:8329`, `src/fleet.rs:8346` | no fleet sandbox configured | **X** — `fleet_sandbox().is_empty()` at all six sites; foreclosed by `src/config.rs:482-484`, proven by running `skein cockpit-stop` three ways. **This is the row that was ranked second-worst in the tree.** All six deleted by SKEIN-756. **Dequeued by SKEIN-777 — code already gone**, re-verified 2026-09-11: §2's census command answers 11, and not one of the 11 is a production guard in `src/fleet.rs` | `skein start`, `skein login`, login sharing, cgroup application, and the cockpit's login socket | **n** — six identical copies, none naming Settings, a config key or `skein doctor` | yes — `load_config().fleet_sandbox` becoming non-empty. Note `src/board.rs:42` records that `load_config` repairs a blank name, so some of the six may be unreachable in practice — **cannot tell without running it** | N |
| `src/fleet.rs:6032` | no fleet sandbox is configured, so there is no box to save | **X** — the same guard in `save_boxes`. Deleted by SKEIN-756. **Dequeued by SKEIN-777 — code already gone**, re-verified 2026-09-11 by the same census | `skein save` | n — better worded, same absence · **the wording went with the guard (SKEIN-756): nothing prints it now, and this `n` is the 2026-09-09 reading** | yes | N |
| `src/fleet/start.rs:828` (`absent_box_reason`) | box {name} does not exist … Run `skein start {name} --branch <branch>` on the host to try again. Do not run `sbx create` — sbx suggests it, and it would build the per-VM box skein no longer supports … | **R** — open a cockpit terminal, or `skein attach`, for a box with no placement record | cockpit terminal for a missing box; `src/sandbox.rs:559`, `src/sandbox.rs:745` | **y — the model for this whole survey.** Command, reason, and an anti-command | yes — `shared_record` becoming `Some`; the best-instrumented condition in the file, and nothing polls it | W |
| `src/fleet/start.rs:905` | box {name} has no checkout in {sandbox} — `skein start {name}` to build one | **R** — a placement record whose checkout tree is missing — an interrupted `skein start` | `ensure_box_session`, cockpit reconnect, `skein attach` | y | yes — the checkout directory appearing | W |
| `src/fleet/stop.rs:304`, `src/fleet/stop.rs:310`, `src/fleet/stop.rs:316` | {name}'s placement record predates the anchor check … restart it with `skein restart {name}` · … anchor belongs to an earlier boot … · … anchor pid has been reused … | **R** — all three arms: a record written before the field existed, a sandbox rebooted without `skein restart`, or a reused pid | cockpit attach, `skein attach` | y — all three end in the same command | yes — a fresh `anchor_matches`, already computed on every attach | W |
| `src/fleet/create.rs:736` | the fleet sandbox {sandbox} is already being created — that started {age} ago and takes a few minutes. Wait for it rather than starting a second one; if it never finishes, it is given up on automatically. | **R** — press the fleet pane's create twice inside the attempt lease | pressing Launch twice | y — "Wait for it", and it names the auto-expiry | **yes, and already watched** — the attempt lease expiring, the create completing. The best self-recovering message in the module | C |
| `src/fleet/create.rs:822` | {sandbox} is not the fleet sandbox this skein is running inside … | **X** — `ensure_fleet`'s `fleet_exists(sandbox) != Some(true)`. Its one production caller is `start_box_inner` (`src/fleet/start.rs:558`), which passes the `fleet_sandbox()` it computed on the line before — a self-comparison. Every other caller is inside `mod tests` (boundary `src/fleet/create.rs:909`). Residual path: `config.json` rewritten between those two adjacent statements. **Dequeued by SKEIN-777 — code KEPT**: it is a guard rather than a dead arm, this cell names a residual path, and the module's own tests observe the refusal directly (`ensure_fleet("some-other-fleet").unwrap_err()`). It now stands at `src/fleet/create.rs:820` | `ensure_fleet`, box launch | y — "ask for it from the cockpit's fleet pane, which puts the request to the warden and shows you the line to run if no warden answers" | yes — warden reachability | W |
| `src/fleet/create.rs:708` | skein cannot create {sandbox} from here and will not guess. {rendered operation} | **R** — press create with the warden unreachable — `create_fleet_operation` answers `Check::Unknown` | `request_fleet_create` from the cockpit fleet pane | y — renders the exact line | yes — `fleet_exists` flipping | W |
| `src/fleet/create.rs:133` (`fleet_lifecycle_refusal`) | skein is running inside the fleet sandbox, so it cannot {what} it from here … | **R** — unconditionally, on every `skein resize` and every cockpit Rebuild: the function has one exit and it is `Some(why)` | `skein resize`, cockpit Rebuild | y — "On the host, destroy it first: … Then create it on the host with: {line}", and a fallback to "run `skein doctor` and copy the one it prints" | no — the act is off this machine | U |
| `src/fleet/create.rs:214` (`destroy_costs`) | Destroying it is not a restart: every box's checkout lives inside that sandbox and nowhere else … | **R** — the same call path, for any fleet with at least one box | the same | y — "Save that work first if any of it matters: `skein save` copies every box's whole tree onto the host …" | yes — `save_boxes` completing; the destroy line could be gated on a recent save | W |
| `src/fleet/create.rs:209` (`destroy_costs`) | skein could not count the boxes in it ({why}) — and could not ask is not the same as nothing to lose, so read this as every box. | **R** — the same, with `places/` or the fleet root unreadable | the same, with a failing census | partly — tells you how to *read* it | yes — `census_placed_boxes` succeeding | N |
| `src/fleet/start.rs:530` (`refuse_a_repurpose`) | {name} is already a {x} box, and this would start it as a {y} one. Nothing has been changed … | **R** — `skein start` on a branch whose derived box name collides with an auto-created review box (`<repo>-pr-<n>`) | `skein start`, cockpit Launch | y — "destroy {name} first if it is finished with, or start the new one under another name" | yes — the placement record disappearing | W |
| `src/fleet/start.rs:600` | the fleet sandbox cannot see {store}, so box {name} would come up with no store. | **R** — `skein start` for a repo registered after the sandbox's mounts were fixed | `skein start` for a repo whose store is outside the mounts | y — renders the create line and says why remaking is the fix | no — a host act | U |
| `src/fleet/start.rs:741` | {why} — {name} IS running … but it has no hooks or kit, so the board cannot see its turns. Run `skein restart {name}` again{…} | **R** — the provisioning `exec` failing after the session is already up | `skein start` / Launch whose provisioning failed | y — and `what_to_look_at` appends kill-vs-timeout-specific advice | yes — `box_is_ready`, a poll skein already has | W |
| `src/fleet/resize.rs:382` (`room_to_copy_out`) | copying the boxes out needs about {n} MiB and the host has {m} MiB free … | **R** — `skein save` with host free space below `boxes + boxes/5` MiB | `skein save`, cockpit Save | y — "Freeing space, or `skein stop`ping boxes you do not need, makes room." | yes — free space crossing the threshold; a numeric poll | W |
| `src/fleet/disk.rs:308` (`stray_advice`) | N directories in {dir} … are neither skein's own nor any box's … if they are yours to delete: rm -rf {paths} | **R** — `skein doctor` above `DISK_FULL_PCT` with orphaned `target-<name>` directories present | `skein doctor` disk section, above `DISK_FULL_PCT` | y — a literal command | yes — the percentage dropping | C |
| `src/fleet/server.rs:571` (`door_refusal`, for `cockpit_port_advice` and the server-start ask) | the cockpit's port :{port} in {sandbox} is not held by the doorway, so do not publish it … | **R** — a sandbox image without python3, so the doorway never starts | the cockpit port-publish flow | y — "check `tmux -S {sock} capture-pane -p -t {session}` in the sandbox, or that python3 is present" | yes — `door_settles` at `src/fleet/server.rs:365` is literally that poll, already written | W |
| `src/fleet/fleetlogin.rs:562` (`share_outcome`) | logged in, but could not hand it to the boxes already running ({why}) — they pick it up when their session next starts | **R** — `skein login` while the sandbox is too busy to answer the share `exec` | `skein login` tail | y — **and it is self-healing by design**, saying so | yes — already | C |
| `src/fleet/resize.rs:217` | there is no box named {name} in {sandbox} — {root} holds no checkout, and an archive of nothing reads exactly like a save. Nothing was copied out. | **R** — `skein save <box>` for a name with a placement but no tree | `skein save <box>` | partly — explains, names no verb | yes — the checkout appearing | N |
| `src/fleet/resize.rs:225` | no box is placed in {sandbox}, so there is nothing to save | **R** — `skein save` with no arguments on a fleet with no placed boxes | `skein save` | n — true and terminal | yes — a placement appearing | N |
| `src/fleet/resize.rs:205` | {name} is not a box name — nothing was copied out | **R** — `skein save "bad name!"` | `skein save <bad name>` | **n** — does not say what a box name is | no — a keystroke, made blind | N |
| `src/fleet/resize.rs:485`, `src/fleet/resize.rs:513` | listing {dir}: {io error} | **R** — `places/` or the fleet root unreadable, on every `skein save` and every `destroy_costs` | printed on every `skein resize` and every cockpit Rebuild, through `census_placed_boxes` → `destroy_costs` | **n** — a raw I/O error with no framing | yes — the directory becoming readable | N |
| `src/fleet/start.rs:976` | the fleet sandbox reported no usable HOME ({home}); every box command would run with HOME unset and write to the filesystem root | **R** — a sandbox image with `$HOME` unset; the doc records this having actually fired | any `sandbox_home` caller, through `skein start` and the cockpit's box routes | **n** — a severe consequence, no move | yes — one line: the sandbox answering with a non-empty absolute path | N |
| `src/fleet/fleetlogin.rs:510` | login in {sandbox} exited {code} | **R** — cancel the interactive login | `skein login` | **n** — a bare exit code | yes — `login_written_ms` gaining a fresh timestamp; skein already has the reader | N |
| `src/fleet/create.rs:472` | creating fleet sandbox {sandbox}: {warden detail} | **R** — the warden answering ambiguously while creating the sandbox | `heal_fleet` / the warden create path, surfaced by `skein doctor` and the cockpit fleet pane | n — a prefix plus raw detail | yes — `fleet_exists` becoming `Some(true)`; the warden's uncertain outcome is explicitly re-pollable | N |
| `src/fleet/resize.rs:375` | skein: could not measure the space this needs; continuing | **R** — `df` output missing the keys the parse wants, during `skein save` | `skein save`, and any path through `room_to_copy_out` | **n** — hands the reader a risk with no lever, then proceeds | yes — re-running the measurement | N |
| `src/fleet/start.rs:781` | skein: {name} is running WITHOUT a memory ceiling ({why}); a runaway build in it can take down every other box in the fleet | **R** — start a box on a host with no cgroup delegation — row 136's condition, at launch | `skein start` | **n** — the highest-stakes step-free line in the module | yes — `uncapped_reason` returning `None`, which the board already renders | N |
| `src/fleet/heal.rs:60`, `src/fleet/create.rs:844` | skein: the cockpit's door is not open in {sandbox} ({e}); a box in this fleet can bind :{port} before skein does | **R** — a sandbox image without python3, at server start or box start | server start / fleet heal | n | yes — `door_settles` (`src/fleet/server.rs:365`), already written | N |
| `src/fleet/heal.rs:70`, `src/fleet/create.rs:860` | skein: could not point dockerd at the workload cgroup ({e}); containers in {sandbox} stay outside the ceiling | **R** — dockerd unreachable in the sandbox; seen while probing SKEIN-756 | server start / fleet heal | n | yes | N |
| `src/fleet/hosts.rs:74`, `src/fleet/hosts.rs:91` | skein: could not pin SSH host keys in {x} ({e}); a box cloning over SSH will fail host key verification | **R** — a configured SSH host unreachable from the sandbox during provisioning | box provisioning | n | yes — the pinned hosts appearing in the sandbox's known hosts | N |
| `src/fleet/create.rs:89` | skein: could not write the fleet kit ({e}) — the create line below still names it, and the fleet will not put its own door back after a restart until it exists | **R** — the fleet-kit directory unwritable, on every `skein resize` (row 146's always-run path) | fleet create rendering | n | yes — the kit file appearing | N |
| `src/fleet/start.rs:149`, `src/fleet/start.rs:160` | skein: {name} matches no repository skein knows … it starts with the sandbox's whole view, as boxes did before covers · skein: {mount} has a newline in it, so boxes cannot be told about it | **R** — `skein repos remove <id>` while a box under it still exists, then restart it — `remove_repo` does not check for live boxes. The newline-in-mount arm needs a hand-edited `fleet_mounts` | box launch | n — both state an isolation loss and stop | yes — the repo record; the mount being renamed | N |
| `src/fleet/start.rs:619`, `src/fleet/start.rs:648`, `src/fleet/start.rs:765`, `src/fleet/start.rs:773` | skein: {name} is cloning from a mirror that could not be refreshed: {why} · has no write token yet — {problem} · could not set {name}'s git identity ({e}); its first commit will ask who you are · came up, but its conversation could not be located ({e}) | **R** — all four: the mirror fetch failing, a GitHub App token failing, the git-identity `exec` failing, the transcript realign failing — all on `skein start` | `skein start` | n — four consecutive box-launch degradations, none with a step | yes — a fetch succeeding, a token arriving, the identity being set, the conversation file appearing | N |
| `src/fleet/fleetlogin.rs:281`, `src/fleet/fleetlogin.rs:295`, `src/fleet/fleetlogin.rs:367`, `src/fleet/fleetlogin.rs:387` | could not read the {rel} login out of {sandbox} ({why}) — the host's kept copy is left exactly as it was · … left alone. · could not save the {rel} login out of {sandbox} — the copy that was already there is untouched · could not restore the {rel} login into {sandbox}: {e} | **R** — all four: an in-sandbox exec, read or write failing during a login heal | login sync | partly — all four say what was *not* damaged, which is the right instinct; none says what to do | yes — `login_fingerprint` / `login_written_ms` | N |
| `src/fleet/start.rs:923` | skein: could not refresh the launcher in {sandbox} ({e}); {name} starts with whichever copy is already there | **R** — `install_launcher`'s write into the sandbox failing | box start | n | yes | N |
| `src/fleet/start.rs:887`, `src/fleet/start.rs:956` | {name} has a session nothing can enter — {why} — so it is being ended and {name} started again · {name} had no live session, so it was restarted (its work is untouched) | **R** — the sandbox's tmux server cycling under a running session — the code names it "the common cause" | box reattach | y — **these narrate a recovery that already happened.** The pattern the rest of the module should copy | already recovered | C |
| `src/fleet/resize.rs:536` | box {name} belongs to no registered repo, so nothing could say where its work came from or put it back — resize aborted with the sandbox untouched | **X** — inside `resize_fleet_inner`, whose body is production-dead: BOTH callers of `resize_fleet` gate on `fleet_lifecycle_refusal` and return first — `src/bin/skein.rs:1638` (`cmd_resize`, whose `?` always fires) and `src/bin/skein-server/fleet.rs:307` (`api_fleet_resize`, whose `if let Some(why) … return` always fires). `tests/resize_rules.rs:279` asserts `cmd_resize` no longer contains `resize_fleet(` at all. **Dequeued by SKEIN-777 — code KEPT**: re-verified 2026-09-11, the body is still production-dead. Deleting it is SKEIN-575's, not this item's | not reachable today | **n** — names no verb; `skein add` is never mentioned | yes — the box gaining a repo | N |
| `src/fleet/resize.rs:542` | box {name} has no recorded branch to come back on — resize aborted with the sandbox untouched | **X** — same dead body. **Dequeued by SKEIN-777 — code KEPT**; same re-verification, same owner (SKEIN-575) | not reachable today | **n** | yes — a branch being recorded | N |
| `src/fleet/resize.rs:699` | could not tell whether {sandbox} was destroyed: {detail} — every box's work is copied out to its own state directory as {run}.tar, and those copies are the only thing that survives the sandbox either way | **X** — same dead body. **Dequeued by SKEIN-777 — code KEPT**; same re-verification, same owner (SKEIN-575) | not reachable today | partly, deliberately — the comment at `src/fleet/resize.rs:694` argues "a destroy that may have happened is the one answer no command can be offered for" | yes — `fleet_exists` answering definitively; the uncertainty is exactly a re-askable question | N |
| `src/fleet/resize.rs:533` | {root} holds N checkout(s) that {dir} has no placement record for, so nothing could carry {them} out: {names} | **R** — **not** dead, and this is why reachability is per call site rather than per function: `census_placed_boxes` has two other live callers, `destroy_costs` (row 146's always-run path) and `save_boxes`. Trigger: an untracked checkout under the fleet root, then `skein save` | gated on the resize path, but `census_placed_boxes` also runs under `destroy_costs` and `save_boxes`, where a person does see it | **n** | yes — a placement record appearing for each named checkout | N |
| `src/fleet/resize.rs:601` | could not check what Docker is holding ({why}), and a resize destroys /var/lib/docker — resize aborted with the sandbox untouched. | **X** — same dead body as row 174. **Dequeued by SKEIN-777 — code KEPT**; same re-verification, same owner (SKEIN-575) | not reachable today | y — "Restart the daemon and try again, or pass --drop-docker to resize anyway and lose whatever is in there." | yes — dockerd answering | W |
| `src/fleet/resize.rs:206` (`docker_refusal`) | a resize destroys /var/lib/docker, and it is holding N thing(s) nothing can put back … | **X** — `docker_refusal`'s only production call site is inside that dead body; its other two are in `mod tests`. **Dequeued by SKEIN-777 — code KEPT**; same re-verification, same owner (SKEIN-575) | not reachable today | y — a full `docker save` line and a `docker run … tar` line, plus "--drop-docker" | yes — `docker_state_at_risk` emptying | W |
| `src/fleet/disk.rs:307`, `src/fleet/disk.rs:315` | skein could not work out which directories under {dir} are its own … · skein can see no boxes at all … | **X** — confirmed against the survey's own note: `substrate_strays`'s one production reader is `src/health/disk.rs:236`, which does `strays().ok()` and drops the `Err` string. Every other caller is in `mod tests`. **Dequeued by SKEIN-777 — code KEPT**: both `Err`s are the subject of `expect_err` assertions in the module's own tests, so deleting them would delete what a test is about. The row was never on the queue anyway — it carries no verdict | **nobody** — `src/health/disk.rs:162` states outright that "an `Err` from `strays` is silence, not a sentence" | n/a | n/a | — |

## 10. The rest of `src/`

From the 191 production `Err(String)` sites outside `src/bin/`. Excluded as not person-facing, with
the reason: the input-validation one-liners guarding `pub(crate)` writers, whose callers map them —
`src/sandbox.rs:283`, `src/sandbox.rs:298`, `src/sandbox.rs:316`, `src/sandbox.rs:387`,
`src/sandbox.rs:542`, `src/sandbox.rs:606`, `src/sandbox.rs:719`, `src/sandbox.rs:1003`;
`src/repos/list.rs:147`, `src/repos/boxes.rs:121`, `src/repos/boxes.rs:125`;
`src/gitgate/scope.rs:70`, `src/gitgate/scope.rs:63`, `src/gitgate/credentials.rs:188`,
`src/gitgate/credentials.rs:213`, `src/gitgate/credentials.rs:237`; `src/files.rs:60`,
`src/files.rs:67`, `src/files.rs:74`, `src/files.rs:263`, `src/files.rs:315`. For the name guards
in `src/fleet/` — `src/fleet/declared.rs:27`, `src/fleet/declared.rs:62`,
`src/fleet/declared.rs:145`, `src/fleet/declared.rs:154`, `src/fleet/disk.rs:394`,
`src/fleet/create.rs:797`, `src/fleet/start.rs:547`, `src/fleet/install.rs:240` and
`src/fleet/server.rs:140` — **cannot tell**: the callers are in-crate and were not all traced.

That list is derived, not carried (SKEIN-877). This prints those nine and `src/fleet/resize.rs:204`,
whose sentence is person-facing and has its own row below:

```sh
grep -rn -A1 'valid_name(' src/fleet/ | grep 'return Err('
```

This paragraph used to name four addresses in the old `src/fleet.rs`, and two
of them held doc comments on the day they were written, so what was meant by them cannot be
recovered; naming every guard of the kind replaces the guess.

| where | what a person sees | reach | reachable how | to do? | watchable? | v |
|---|---|---|---|---|---|---|
| `src/volume.rs:104` | this installation was moved to {target}, and $SKEIN_HOME still points at {home}. Set it and try again: export SKEIN_HOME={target}. Or, if you meant to keep using this one: rm {marker} | **R** — `skein move /new/path`, then run anything with `$SKEIN_HOME` still on the old location | any CLI verb, through `ensure_volume` at `src/bin/skein.rs:45` | y — two labelled branches, both copyable | yes — `$SKEIN_HOME` matching the recorded path | W |
| `src/volume.rs:113` | … holds a half-finished move … Delete it and move again, or remove {marker} if you know the copy completed. | **R** — interrupt a `skein move` mid-copy, leaving the MIGRATING marker | the same, after an interrupted `skein move` | y | yes — the marker disappearing | W |
| `src/volume.rs:122`, `src/volume.rs:130` | Upgrade skein, or point $SKEIN_HOME at another volume. Reading it anyway would drop every field this binary does not know at the next write. · Use a skein that understands {found}. | **R** — **overturns the mechanical pass, and it took running it.** No code in this tree writes a `VERSION` other than `SCHEMA` — but `schema_of` reads a file, and the file is on disk: `printf '2\n' > $SKEIN_HOME/VERSION` and `skein-server` prints this sentence and exits. Which is what a message about "a newer skein" is for | a volume written by a newer or older skein | y | yes — the binary's schema matching | W |
| `src/volume.rs:182` | … skein here would read and write the volume over there — and everything would look fine until that one was deleted. If you moved or copied it here: skein repoint | **R** — `cp -a` a whole `$SKEIN_HOME` elsewhere, outside `skein move`, then run skein there | starting against a copied volume | y — names the verb, which `src/bin/skein.rs:44` deliberately exempts from the guard so it can be run | yes — the recorded path matching | W |
| `src/volume.rs:478` | Move onto an empty directory — merging two volumes is not something this can do safely. | **R** — `skein move` onto a directory that already holds `VERSION`/`config.json`/`repos.json`/`boxes`/`repos` | `skein move` onto a non-empty target | y | no — a keystroke | C |
| `src/volume.rs:498` | the fleet sandbox {sandbox} is up, and its boxes are reading the volume you are moving … This is a job for the host: {line}. Every box's work is on the volume and travels with it. | **R** — unconditionally, on every `skein move`: `fleet_exists(sandbox)` compares that name against `fleet_sandbox()` and `sandbox` came from the same place. **Deliberate, not a defect** — SKEIN-574 made moving the volume an Operation skein reports and never performs, and `src/volume.rs:449` says "the fleet is up, which is now always". Only the grammar reads as a condition | `skein move` with the fleet running | y | no — a host act | U |
| `src/volume.rs:543` | … has {have} free and this needs about {need} … Free some room or choose another target. | **R** — `skein move` onto a filesystem with less free space than the source needs | `skein move` onto a full disk | y | yes — free space crossing the threshold | W |
| `src/volume.rs:571` | copying to {target} failed, and the half-copy is left in place with its MIGRATING marker so nothing mistakes it for an installation: {stderr} | **R** — `cp -a` failing mid-move — target full, permission denied, read-only mount | `skein move` | **n** — states the safety property, offers no move; contrast `src/volume.rs:113`, which does | yes — the marker being removed | N |
| `src/sandbox.rs:395`, `src/sandbox.rs:401` | box {name} is not running; attach/start it before resuming · cannot tell whether box {name} is running; attach/start it before resuming | **R** — stop a shared box, then press Continue | the cockpit's Continue button | partly — names an act, not a command | yes — `box_liveness` returning running | W |
| `src/sandbox.rs:469` | resume exited {code}{detail}; see {path} | **R** — a resume whose child exits non-zero within 500ms | cockpit Continue | partly — points at a file | yes | W |
| `src/sandbox.rs:575` | stop failed (exit {code}): {stderr} | **R** — the stop shell exiting non-zero | cockpit Stop, `skein stop` | **n** | yes — `box_liveness` returning stopped | N |
| `src/sandbox.rs:781` | teardown failed (exit {code}): {stderr} | **R** — the same, for destroy | cockpit Destroy, `skein destroy` | **n** | yes — the sandbox no longer being listed | N |
| `src/tracking.rs:1002` | the gateway could not be reached ({status}) | **R** — Settings → Work tracking → revoke with the gateway unreachable | Settings → Work tracking | **n** | yes — an HTTP GET to the gateway URL returning 2xx. The most literally pollable condition in this survey | N |
| `src/tracking.rs:712`, `src/tracking.rs:801` | the {label} connection has no gateway URL yet | **R** — save a connection with a token and no gateway — the cockpit's own label calls that a supported state — then press Track work | cockpit work-tracking panes | **n** — and `src/tracking.rs:807` answers the sibling condition with "add it in Settings → Work tracking" | yes — the config gaining a URL | N |
| `src/tracking.rs:807` | no Plane token stored for {label} — add it in Settings → Work tracking | **R** — the same with the halves swapped: a gateway and no token | the same panes | y — the model its two siblings should follow | yes | W |
| `src/tracking.rs:813`, `src/tracking.rs:917` | {name} is not running | **R** — press Track work against a box that is not running | cockpit box registration, docs apply | **n** | yes — `box_liveness` | N |
| `src/tracking.rs:844` | token written, but registration did not confirm{detail} — the box can still be registered by hand against {url} | **R** — Track work on a box whose `.claude` store predates the sync installer | box registration, partial success | y — names the fallback | yes — a confirming re-registration | W |
| `src/tracking.rs:430` | {users} still {uses} it — point {them} at another connection first | **R** — Settings → Work tracking → Remove on a connection a repo still selects | removing a connection in use | y | no — a keystroke | C |
| `src/tracking.rs:153` (+ `src/repos/list.rs:78`) | reading {path}: {e} | **R** — truncate or hand-edit `connections.json` or `repos.json` into unparseable JSON | nearly every command, on an unparseable `repos.json` or `connections.json` | **n** — a raw error on a file almost everything reads | yes — the file parsing | N |
| `src/gitgate/mint.rs:127`, `src/gitgate/mint.rs:136` | no GitHub App configured: Settings → GitHub App ID · the GitHub App ID must be the numeric id, not {id} — Settings → GitHub App ID | **R** — leave the GitHub App ID blank (the default), or type non-digits into it — the field has no validation | Settings → GitHub App | y | yes — the setting arriving | W |
| `src/gitgate/mint.rs:140` | the GitHub App key is not at {key} | **R** — set a valid App ID before putting the key at `~/.skein/github-app.pem` | the same | partly — names the path, no verb | yes — the file appearing at that path | N |
| `src/gitgate/mint.rs:89` | could not sign the App JWT: {stderr} | **R** — a corrupted key at the configured path — existence is checked, validity is not | any token mint, so any push or PR from a box | **n** | yes — a subsequent mint succeeding | N |
| `src/gitgate/mint.rs:228` | could not withdraw {failures} | **R** — a token path that cannot be removed — permission, or a read-only mount | credential withdrawal from the cockpit | **n** | yes — the token files being absent from the box state directory | N |
| `src/files.rs:122` | the box could not read that ({other}) | **R** — **not by any word the guest script emits** — it only ever prints OK/ESCAPE/NOTDIR/NOTFILE. The catch-all fires on corrupted output, e.g. a box whose shell prints a banner before the status line | the cockpit file browser | **n** — surfaces an opaque token | none — the token is the box's | N |
| `src/files.rs:119`, `src/files.rs:120`, `src/files.rs:121`, `src/files.rs:209`, `src/files.rs:245`, `src/files.rs:270`, `src/files.rs:318` | path escapes the workspace · not a directory · not a file · invalid path | **R** — ESCAPE/NOTDIR/NOTFILE by browsing a path the agent deletes or replaces underneath, or a symlinked directory leaving the workspace. The "invalid path" arm is **X** through the UI — the page's own `resolve()` pops `..` client-side — and R only for a request sent directly | the cockpit file browser | n | no — input | N |
| `src/takeover.rs:195`, `src/takeover.rs:200`, `src/takeover.rs:211`, `src/takeover.rs:321` | copying {guest} exceeded 300s · waiting for artifact copy: {e} · copying {guest}: {stderr} · extracting shared context: {stderr} | **R** — a takeover whose guest `sbx exec` wedges past 300s, errors, or exits non-zero; or a truncated `shared-context.tgz` | a cockpit takeover to another runtime | **n** — four ways a takeover stops, none with a step | yes — the artifact file appearing on the host | N |
| `src/takeover.rs:401` | {source} already uses {target_runtime}; use the same-provider tmux restart path | **R** — pick the runtime the box already uses in the takeover dialog | the same | y | no — a choice | C |
| `src/repos.rs:1035`, `src/repos.rs:1114`, `src/repos.rs:1367` | mirroring {from}: {stderr} · fetching {id}: {stderr} · fetching {refspec} of {id}: {stderr} | **R** — `skein pull` with the remote gone, renamed, or its auth revoked | `skein pull`, the cockpit repo pane | **n** — raw git stderr | yes — a subsequent fetch succeeding | N |
| `src/repos/mirror.rs:85` | {id} has nothing to mirror from | **R** — only by hand-editing `repos.json` to blank a `source`: `registrable_source("")` is false, so `add_repo` refuses one earlier, and the cockpit has no editable source field | the same | **n** | yes — the record gaining a source | N |
| `src/repos/mirror.rs:321` | the mirror at {path} has no readable HEAD — an empty repository, or a mirror that did not survive its clone | **R** — kill `git clone --mirror` after the directory exists and before HEAD is written, then open the repo | the same | **n** — a good diagnosis, no step | yes — `git symbolic-ref HEAD` in the mirror answering | N |
| `src/repos/add.rs:64` | {source} is a path, and skein registers repos by remote … Give the remote instead — `git -C {source} remote get-url origin` prints it. | **R** — `skein add /local/path` | `skein add <path>` | y — hands over the command that produces the right argument | no — a keystroke | C |
| `src/repos/add.rs:16` | unsupported runtime {runtime}; available: {list} | **R** — `skein add <url> --agent bogus` | `skein add --agent <typo>` | y — the list, which `src/bin/skein.rs:1449` and `src/bin/skein.rs:1669` omit | no — a keystroke | C |
| `src/repos/list.rs:136` | {id}'s per-pull-request triggers could not be read ({why}), so this would have replaced every one of them with a single entry. Nothing has been changed. | **R** — a corrupt `pr-triggers.json`, then use the per-PR triggers control | cockpit repo triggers | **n** — an excellent "nothing changed" guarantee with no next step | yes — the override file parsing | N |

## 11. The warden

The warden is the most consistently well-worded surface in the tree: most of its refusals explain
the rule they are enforcing, and several print the allow-list. Its gap is different from everyone
else's — it is the one component whose failures are *most* watchable (a terminal appearing, a
secret path becoming writable, an outstanding turn releasing) and where **nothing watches at all**.

`warden/src/wire.rs` is excluded: its eleven terse refusals (`warden/src/wire.rs:115`, `warden/src/wire.rs:121`,
`warden/src/wire.rs:125`, `warden/src/wire.rs:132`, `warden/src/wire.rs:138`, `warden/src/wire.rs:141`, `warden/src/wire.rs:152`,
`warden/src/wire.rs:162`, `warden/src/wire.rs:170`, `warden/src/wire.rs:173`, `warden/src/wire.rs:178`) answer a malformed HTTP request and reach a client, not a
person.

| where | what a person sees | reach | reachable how | to do? | watchable? | v |
|---|---|---|---|---|---|---|
| `warden/src/doer.rs:75` | {what} for {sandbox} was not run: this warden has no approval surface, and a privileged host command needs a human at the host to confirm it (architecture §8.1). Operation {id}. | **R** — `nohup skein-warden &` — no controlling terminal selects `Unattended` — then any Launch | pressing Launch against a warden started with no controlling terminal | partly — names the shape of the fix, not the act. `warden/src/main.rs:152` prints the missing half at boot: "Run it where a person can answer it." | yes — a controlling terminal appearing | N |
| `warden/src/serve.rs:179` | this warden has no secret to check against ({path}), so it refuses everything — it mints one at start when that path is writable | **R** — make the warden's secret path unwritable before it starts, then send any request | every request, on a warden whose secret path is unwritable | y — names the path and the restart | yes — the path becoming writable; nothing retries the mint after start | W |
| `warden/src/serve.rs:189` | this warden does not know who is asking — skein presents the secret from under the mount cover, and nothing else can read it (architecture §9.5 R5) | **R** — point skein at a different volume from the one the warden started with | skein and the warden pointed at different volumes; every warden-backed button fails | **n** — explains the mechanism, names no action. The actionable sentence exists only at `warden/src/main.rs:112`, printed at boot, where the person hitting the 401 is not looking | yes — the two volumes agreeing, comparable at request time | N |
| `warden/src/serve.rs:271` | this warden was built without `{cap}`. Nothing can turn it on: the doer is not in the binary (architecture §8.3). | **R** — a warden built without the capability the button needs | pressing Launch or Destroy against a reduced-capability warden | **n** — and this is exactly the case the standard at the top warns about. "Nothing can turn it on" is true of *this binary*; installing one built with the capability is the step, and it is not said | no — a compile-time fact | N |
| `warden/src/serve.rs:603` | {name} is not a sandbox name — letters, digits, `.`, `_` and `-`, up to 128 of them, and not beginning with `-` | **R** — `POST /v1/create` with a sandbox name carrying an illegal character or a leading `-` | a malformed sandbox name | y — the grammar, which `src/bin/skein-server/boxes.rs:82` and its twelve siblings omit | no — a keystroke | C |
| `warden/src/serve.rs:610` | the {cap} for {name} carries N arguments, and more than {MOST_ARGS} is more than an approval can put in front of a person | **R** — the same with `args` longer than `MOST_ARGS` | an over-long argv | partly | no | C |
| `warden/src/serve.rs:619`, `warden/src/serve.rs:651` | an argument of the {cap} for {name} cannot be shown as what it is, so it cannot be approved: {arg} · the value of `{key}` cannot be shown as what it is … | **R** — an `args` or `env` value over `LONGEST_VALUE`, or with unreadable bytes | an unrenderable argument | partly — names the offending argument | no | C |
| `warden/src/serve.rs:634` | `{key}` is given twice, and `sbx` would take the last one — so what ran would not be the first thing on the line that was approved | **R** — an `env` array repeating a key | a duplicated flag | y | no | C |
| `warden/src/serve.rs:640` | this warden does not pass `{key}` to a {cap}. The environment decides what a relative program name resolves to, so it is an allow-list and not a filter: this one takes {list} | **R** — an `env` key outside `env_a_doer_may_carry` | an unlisted environment key | y — prints the allow-list | no | C |
| `warden/src/serve.rs:676` | the approval for this {cap} would be N bytes of text, and more than {LONGEST_APPROVAL} is more than a person can be asked to read to the end — the operation id they have to type is at the bottom of it | **R** — `args`/`env` whose rendered approval exceeds `LONGEST_APPROVAL` | an over-long approval | y — names both sizes | no | C |
| `warden/src/serve.rs:288` | this warden has no `approved` field, and adding one to the request would not create it: approval is a fact the approving side writes, confirmed by a human at the host (architecture §8.1). {error} | **R** — a body carrying an extra `"approved": true` — `Asked` is `#[serde(deny_unknown_fields)]` | a malformed request | y | no | C |
| `warden/src/serve.rs:210` | this warden serves /v1/fleet, /v1/audit, /v1/create, /v1/destroy, /v1/publish and /v1/unpublish | **R** — request any path outside `/v1/{fleet,audit,create,destroy,publish,unpublish}` | a wrong path | y — lists the surface | no | C |
| `warden/src/serve.rs:240` | the sandboxes could not be listed: {why} | **R** — the host's `sbx ls --json` wedged while the cockpit polls `/v1/fleet` | the cockpit polling the warden's fleet view with a wedged `sbx` | n | yes — `sbx ls --json` answering, and it is already polled, so recovery happens in practice | N |
| `warden/src/flooding.rs:51` | another operation is already waiting for a person at the host: {operation}. One at a time, deliberately (architecture §8.5) — ask about that one, or wait for it. | **R** — press Launch again while the first approval prompt is still up | pressing Launch while an approval is on screen | y — and the doc at `warden/src/flooding.rs:47` states the rule: "'slow down' without an alternative is how a client ends up retrying in a loop" | yes — the outstanding turn releasing. The clearest wait-for-the-condition case in the warden | W |
| `warden/src/flooding.rs:55` | more than N operations were proposed in a minute, which is more than a person can confirm one at a time (architecture §8.5). Nothing was run. | **R** — POST create/destroy faster than the per-minute cap | a client in a loop | partly — "Nothing was run" closes the loop; no retry-after | yes — the rate window rolling forward, which a `Retry-After` would make machine-actionable | N |
| `warden/src/approval.rs:144` | {what} was refused at the host: the operation id was not confirmed. Operation {id}. | **R** — type anything but the operation id at the approval prompt — a person's deliberate no | typing anything but the id at the warden's prompt | n at the point of refusal — the coaching is only in the prompt at `warden/src/approval.rs:104` | no, correctly — this is a person's deliberate "no", and retrying re-asks by design | U |
| `warden/src/approval.rs:138`, `warden/src/approval.rs:128`, `warden/src/approval.rs:131` | the answer could not be read, so nothing was approved: {e} · the approval could not be shown: {e} | **R** — close the host terminal while a prompt is displayed | a tty read or write failing while the prompt is up | **n** | yes — the tty becoming usable | N |
| `warden/src/outcome.rs:307` | {why} — and the warden could not release {path} ({e}), so asking again will be answered with this rather than put to a person | **R** — make the claim file's directory unwritable between the claim and the cleanup | a refusal whose claim file could not be removed | **n** — states a stuck lock and stops. Deleting the named file is the recovery and is not named | yes — the claim file disappearing | N |
| `warden/src/outcome.rs:324` | {id} ran, and recording that failed — treat it as undecided: {e} | **R** — the doer's command succeeds and the outcome write then fails — disk full, permission revoked | a filesystem failure after the command already ran | partly — a next step for a client, not for a person | no — it correctly refuses to guess | U |
| `warden/src/outcome.rs:416` | {path} is unreadable ({e}) — refusing to run, because an operation whose record cannot be read may already have happened | **R** — hand-corrupt an outcome record, then ask about that operation id | a corrupt outcome record | **n** — names the file, not the fix | yes — the record becoming parseable | N |
| `warden/src/doer.rs:457`, `warden/src/doer.rs:460` | could not run `sbx`: {e} · `sbx {argv}` exited {code}: {stderr} | **R** — an approved operation where `sbx` is missing from PATH or exits non-zero | an approved operation that then fails in `sbx` | n — it does forward sbx's own stderr, which is the honest half | no — a genuine failure of the thing asked for | N |
| `warden/src/doer.rs:204`, `warden/src/doer.rs:216`, `warden/src/doer.rs:221`, `warden/src/doer.rs:418`, `warden/src/doer.rs:434` | {argv} does not begin with the `create` verb · passes no `--name`, so sbx would name the sandbox after the agent and the working directory instead · would run as {names} — sbx takes the last `--name` … · the unpublish … is not exactly `ports <sandbox> --unpublish <mapping>` | **R** — only by a crafted request to the warden holding the secret: `args` missing `create`, missing `--name`, or duplicating it. Whether skein's own argv builders could ever produce one was **not** established | **not established** — these are aimed at a skein developer and surface as 400 bodies in the cockpit | partly — they explain why, and the last names the required shape | no | N |
| `warden/src/main.rs:59` | skein-warden: could not listen on port {port} at {wanted}: {e}, then exits | **R** — start a second warden on the same port | starting a warden on a taken port | **n** | yes — the port freeing | N |
| `warden/src/main.rs:112` | skein-warden: volume {v} (from $SKEIN_HOME) — skein must be pointed at the same one, or every request is refused for a mismatched secret; the record is at {r} | **R** — start the warden with `$SKEIN_HOME` set — unconditional on that, and informational rather than a fault | warden start | y | yes — the two volumes agreeing | W |
| `warden/src/main.rs:128` | skein-warden: this is not where skein looks by default — export SKEIN_WARDEN=127.0.0.1:{port} for skein and skein-server, or they keep asking {where} | **R** — start it with `$SKEIN_WARDEN_PORT` set to anything but the default | warden start on a non-default port | y — the exact command | yes | W |
| `warden/src/main.rs:140` | skein-warden: NO SECRET at {path} — it could not be read or minted, so every request is refused. Fix the path and restart. | **R** — row 215's precondition, at the startup banner | warden start with an unwritable secret path | y | yes — the path becoming writable | W |
| `warden/src/main.rs:152` | skein-warden: no controlling terminal, so there is nobody to approve anything and every doer refuses. Run it where a person can answer it. | **R** — row 214's trigger, at the startup banner | warden start under a supervisor | y — the operator-facing half of `warden/src/doer.rs:75` | yes — a terminal appearing | W |
| `warden/src/audit.rs:99` | skein-warden: could not move {a} to {b} ({e}) — it is on the volume, which skein can write, so delete it once you have kept what you want from it | **R** — make the audit rotation target unwritable | an audit rotation failure | y | yes — the file disappearing | W |
| `warden/src/secret.rs:150` | skein-warden: could not move the secret from {a} to {b} ({e}) — a fresh one will be minted under the cover | **R** — make the new secret path's directory unwritable during a volume-root migration | secret migration failure | partly — says what happens next, not what to do; re-pairing is implied | yes | N |
| `warden/src/sightings.rs:142`, `warden/src/sightings.rs:64`, `warden/src/sightings.rs:78` | it did not answer within {n}s · `sbx ls --json` did not print JSON ({e}): {clip} · `sbx ls --json` printed {clip} | **R** — replace `sbx` with a stub that sleeps past the timeout, prints non-JSON, or prints JSON of the wrong shape | a hanging or foreign `sbx` | n | yes — `sbx` answering | N |

---

## What could not be classified, and why

- **`src/board.rs:334`** — the board's principal "why" field. Its text originates in the box's own
  probe and in `signals`, not in `board`, so whether it says what to do is a property of a surface
  this survey did not reach. It is the single largest unexamined text source in the product.
- **`src/bin/skein-server/settings.rs:256`** — a person presses Save, the ssh key does not load, and the
  message goes to the server's stderr while the request returns 200. Whether that counts as a
  person seeing it depends on where they are standing.
- **`src/bin/skein-server/boxes.rs:20`** and three siblings — a tokio `JoinError` rendered into a 500
  body. Whether the cockpit draws it as a sentence was not established.
- **`src/fleet.rs:2059`** and its five twins — `src/board.rs:42` records that `load_config` repairs
  a blank sandbox name, which would make some of the six unreachable. That cannot be settled by
  reading; it needs a run with a blanked config.
- **`src/fleet/declared.rs:27`, `src/fleet/declared.rs:145`, `src/fleet/declared.rs:154`,
  `src/fleet/server.rs:140`** — `pub(crate)` writers whose callers were not
  all traced.
- **`warden/src/doer.rs:202`** and its siblings — refusals aimed at a skein developer that surface
  as 400 bodies. Whether a person ever reads one was not established.
- **`src/announce.rs:792`** — composed into a note whose audience is an agent rather than a person.

## Two things this survey changed about the premise

1. **SKEIN-679's citation is stale.** `src/fleet/resize.rs:185` does not carry the "safe to re-run"
   sentence; it was deleted, and `src/fleet/resize.rs:1222` now asserts it stays deleted. Anything
   planning work from that citation should re-read it first.
2. **A path with no next step is not always a path with no next step *available*.** Three of the
   worst rows — `src/bin/skein-server/terminal.rs:620`, `src/web/app/review-rows.js:325`, `src/fleet/start.rs:791` —
   sit beside machinery that already performs the recovery. The defect in those is not that skein
   cannot recover; it is that it recovers silently and tells the person they are stuck.
