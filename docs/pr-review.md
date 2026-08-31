# The reviewer's engine

**Status: a proposal, not a record.** Nothing here is built. It is written down so it can be
argued with before any of it exists, which is the order the owner asked for: *"think through
thoroughly and then ask me any questions … before you propose a design which we can discuss before
you actually start with it."*

The ask, verbatim: *"gadget-demo-repo-archaeology did an awesome PR review cycle in full
automated mode. I want the automated mode for us to be like that. … for fully automated mode that
is the ideal, then manual mode is just automate checkboxes so that I can choose which ones have to
be automated and which shouldn't. So mechanism doesn't change much, just gates change."*

That box was interviewed at length. Its answers are the reason several things below are written the
way they are, and where it corrected an earlier draft of this design the correction is kept in view
rather than tidied away — the wrong version is the useful half.

---

## 1. What the ask actually is

**Not "make skein's automation more autonomous".** Skein's engine already is: `workflow.rs` merges
pull requests and deletes branches with nobody watching. The gap is on a different axis.

| | today |
|---|---|
| **author side** — PRs you wrote | label → wait for CI → rebase → merge → delete branch, unattended |
| **reviewer side** — PRs asking for you | a person presses buttons in the cockpit |

Everything the interviewed box did was the reviewer role: read a pull request at a commit, verify
its claims, post a verdict. Skein can post a review; it has no engine that decides to.

So this is a **second vocabulary over the existing engine**, not a second engine.

## 2. Skein already holds the principle the box arrived at

The box closed its first answer with the lesson it thought was the transferable one: *"build the
habit of re-deriving state from the authoritative source at every step where the loop reports."*

`workflow.rs` has said that since it was written:

> A guarded step set keeps no place: **the state on GitHub is the program counter.** Crash anywhere
> and the next poll resumes from wherever the pull request actually is, because that is the only
> place the position was ever kept.

Re-evaluated from scratch every poll, one step per evaluation (`workflow::next` finds at most one),
and a closed set of exactly six actions with no way to name a seventh from a file. The three
properties that make it safe to run unattended — one global off switch, every action audited with
which-workflow-which-step attribution, and no blind retries — are `prwork`'s rather than
`workflow`'s, which is worth keeping straight because they are what a reviewer flow inherits by
being carried the same way. That is the machine. It does not need rebuilding to review.

## 3. The correction that matters most

The first draft of this design claimed that a stateless engine dissolves the box's failures,
because none of them could survive re-derivation. **That is wrong, and the box said why.**

Its worst near-miss was a stale `APPROVED` silently carrying a verdict from an old commit. That was
never a memory bug — and the mechanism is worth getting right, because a draft of this section took
the box's explanation on trust and **this tree already has the better one**, bought at the cost of
SKEIN-339.

`reviewDecision` does not answer *"does an approval exist"*. It answers **"is this branch's review
requirement satisfied"** — `APPROVED` only where branch protection requires a review and the
requirement is met, `null` on every repository where review is social, however many approvals the
pull request carries. Measured on the owner's own queue: `APPROVED` on **zero of twenty-one** open
pull requests, two of which he had personally approved. And whether a push ends an approval is the
repository's `dismiss_stale_reviews` setting, not a property of the word: with it off, GitHub goes
on saying `APPROVED` across pushes and means it.

So the field is not lying. It is answering a different question correctly, and **a guarded step that
re-derives it every poll reads the same correct answer to the wrong question, confidently, for
ever.**

Its general form is the sentence to design against:

> Re-deriving constantly from a source that lies gives you the wrong answer more often and with more
> confidence.

Two consequences, and they shape everything below:

* **Those fixes live in the adapter, never in the step model.** `prwork::facts_of` is where a
  lying source is corrected. A step vocabulary cannot fix a field that answers the wrong question.
* **One failure gets *worse* under a stateless engine, structurally.** In the box's session,
  reading and posting were seconds apart. Split across polls, the head can move between them *by
  design* — and a memoryless engine cannot know which commit the reading step examined. It would
  post a review describing tree A anchored to tree B.

## 4. So the sha becomes part of the program counter

The box's fix, adopted verbatim because it is better than the alternative considered:

> The reading step records its findings **with the sha it read**. The posting step's guard is
> `finding.sha == head`, and on mismatch the next step is READ again, not POST.

This is nearly free, because the tree already works this way and for the same reason.
`review.rs` caches every reading against `(number, head_sha)`, and says so:

> That key is not an optimisation: it is the same fact that decides whether your review still counts
> in `crate::prq`, so a PR that gains a commit gets a fresh summary and a fresh place in your queue
> from one change of state.

`review::worth_reading` already asks exactly the guard's question. What is missing is a *condition*
that reads it and a *step* that is held back by it.

## 5. How much of this already exists

Most of it. The design is mostly wiring, and saying so is the point — a proposal that reports itself
as bigger than it is buys agreement it has not earned.

| what a reviewer engine needs | already in the tree |
|---|---|
| which pull requests are yours to review | `prq::Lane::NeedsYou`, `prq::Reason::Reviewer` |
| is GitHub asking *you*, by name | `Pr::my_review_requested` |
| what you last said, and whether it was against this head | `Pr::my_review`, `Pr::review_is_current` |
| reading one at depth | today a session in a detached checkout (`8c49c34`); **§11 moves it into a box** |
| a reading that continues rather than restarts | it **resumes the pull request's own conversation** (`f64e1ae`, SKEIN-376) |
| acting on GitHub as you | `GH_TOKEN` in the call, so `gh` works as the reviewer (`07ba534`) |
| posting | the session posts **a comment review** with `gh`; skein keeps no copy (`f7099ac`) |
| posting a **verdict** | **nothing does** — the prompt forbids it in as many words (§13) |
| did that pass cover the change | the sweep — a second turn that accounts for its own coverage (SKEIN-393) |
| a reading pinned to a commit | the `(number, head_sha)` cache key |
| posting a verdict with line comments | `prq::submit_review_with_comments`, with re-anchoring |
| may skein read this at all | `review::in_reading_scope`, the daily spend ceiling |
| a guarded-step evaluator | `workflow::next`, `Cond`, `Act`, `Step` |
| an off switch, an audit trail, no blind retries | `prwork`, module doc |
| one at a time where that matters | `Workflow::serial` |

**Missing:** reviewer conditions, reviewer actions, the sha guard, and three adapter corrections.

## 6. The vocabulary

Added to the existing closed sets. It stays closed — there is still no way to write a seventh kind
of thing into a file, which is what keeps every action describable in the audit.

**Conditions** (all read from the adapter, never from GitHub directly):

| condition | holds when |
|---|---|
| `ReviewRequested` | GitHub is asking you by name — `Pr::my_review_requested` |
| `Unreviewed` | you have never decided on it, **and skein saw every review** |
| `ReadingCurrent` | a reading exists at the current head |
| `ReadingStale` | a reading exists, at an older commit |
| `ReadingWhole` | that reading covered every changed file, at one commit |
| `FindingsBlocking` | the reading found something that must block |
| `VerdictStanding` | your approval or refusal is against the current head |

**Actions:**

| action | what it does |
|---|---|
| `Read` | ensure this pull request's **review box** stands at the current head, and give it the round; it reads, and posts. **Built as far as the reading** (§15 step 3a): the reading happens and is filed against the head. Where it runs is §11, and posting is step 4. |
| `PostFindings` | submit as `Verdict::Comment` |
| `PostChanges` | submit as `Verdict::RequestChanges` |
| `PostApproval` | submit as `Verdict::Approve` |
| `Audit` | one owed check from §8, recorded against the sha |

`Flag` and `Wait` are reused unchanged. They already mean the two things a reviewer engine needs to
say when it will not act.

## 7. Four adapter rules, and all of them fail closed

The first three are the §3 corrections; the fourth was found by reading this document back against the code. None of them is a step.

**a. The reviewer must not read `approved`.** `Facts::approved` answers *"has anybody approved
this, and is nobody's refusal standing"* — a question about the pull request. The reviewer's
question is about *you*: `my_review` together with `review_is_current`. Reading the first for the
second is SKEIN-339 re-committed under a new word, and `Facts::approved` already carries a long
account of what that cost.

**b. Truncation is never absence.** The box read `--limit 60` against 64 open pull requests and took
the missing rows for *"closed or merged"*. The tree already has the discipline and the vocabulary:
`Facts::labels_whole` fails closed, and a name skein never received satisfies neither `Label` nor
`NoLabel`. Every reviewer condition obeys the same rule — `Unreviewed` requires that skein saw the
whole review list, and where it did not, the condition holds neither way and the pull request waits.

**c. A partial pass may never approve.** The box shipped an `APPROVED` and a "not approving" from
the same account 53 seconds apart, because one pass had not opened the file with the defect in it.
So: *a pass that did not cover the whole changed file set at one commit may post findings, and may
never post an approval.* `ReadingWhole` is a required condition of `PostApproval` and of nothing
else.

**The evidence is the sweep, not the truncation.** An earlier draft of this said a pass is partial
when the diff was cut to fit the prompt. That was written from half the code: the byte caps are real
— `STAGE1_BYTES` 40 KB, `STAGE2_BYTES` 140 KB, `CRITIQUE_BYTES` 300 KB — but since `8c49c34` the
diff is the reviewer's *opening summary* and not its only window. It stands in a checkout and is
told to go and read; the failure the tests name is the opposite one, *"the reviewer reads the diff
alone and the whole checkout does nothing"*.

So a cut diff no longer means files went unread. What does is the **sweep** — the second turn
(SKEIN-393) that makes the review account for what it actually covered, measured on
`acme/testbed#30` to surface two genuine bugs beyond the planted set. `ReadingWhole` is *the sweep
ran and accounted for every changed file*.

**And a draft of this section claimed that costs no new field, which was wrong** — recorded because
it is the third time in this document that a mechanism was asserted to exist because a related one
did. `sweep()` discarded its own result (`let _ = claude_in_turn(...)`): the coverage answer was
computed, shown to the reviewer, and persisted nowhere. `Summary`'s fields are all about what a
reading *found*, never about what it *read*, so coverage was recoverable only by the inference "a
summary exists, so presumably it looked" — which is the exact failure §7c is about. It costs one
persisted field, `Summary::swept`, absent-means-unknown.

Access is not the same as having looked, which is why the rule survives its own correction: the box
that shipped an approval and a refusal 53 seconds apart had a checkout the whole time.

**d. An engine verdict must not remove the pull request from the engine's scope.** Found late, and
it would have been this design's own worst bug. `prq` puts a pull request in `Lane::Waiting` the
moment `my_review` is `approved` or `changes-requested` and nothing has re-requested you; and
`review::worth_a_visit` keeps a `Waiting` row in scope only where you authored it. So on a pull
request somebody else wrote, **the first verdict the engine posts takes that pull request out of the
engine's own reading scope, permanently.**

That is the stale-approval hole this design exists to close, recreated at the instant it acts: §9's
*"the head moves on one you approved → re-check"* can never fire, because nothing looks again. The
lane rule is right for a person — you decided, it is somebody else's move — and wrong for an engine
that has undertaken to keep watching. So the engine's scope is not the lane: it is the lane **or** an
unfinished trigger this engine owns.

This is also where `review.rs`'s existing rule lands, and it lands exactly right:

> **AI may only add scrutiny, never remove it.** A PR skein has not actually read stays at full
> attention and says so. A summary can only ever lower depth by **succeeding**, never by failing
> quietly.

A reading that failed is `Depth::Unread`, `ReadingWhole` does not hold, and an approval is
unreachable. The failure direction is already fixed in the type.

## 8. The scar becomes a guarded step

The box was asked what the smallest durable artifact would be that carries a lesson to a fresh
agent — because a stateless engine has no memory by construction, and its most valuable carry had
been its own scar: learning mid-session that a deletion audit must be base-versus-head, then
applying it four hours later.

Its answer, and this is the part worth stealing wholesale. Per repo, six lines:

| trigger, on the diff | owed before any verdict |
|---|---|
| any deletions | a base-versus-head audit of what the removed lines guaranteed |
| a new guard or assertion | mutate the rule it protects, confirm it goes red |
| a comment naming a mechanism | check that mechanism is actually called |
| a claim that something is absent | check every branch and pull request before asserting it |
| a doc or plan cited | verify it exists somewhere reachable, and say which branch |
| a test added | check it can fail |

As a step: **if the diff deletes lines and no deletion audit is recorded at this sha, the next step
is `Audit`, not a post.** The lesson stops being prose in a prompt that a fresh agent may or may not
weigh, and becomes a condition that is either satisfied or is not.

This is deliberately a per-repo file rather than a global one. What a repository owes a reviewer is
a property of that repository.

## 9. The gates

The owner's axis, in their own words: *"I was talking about gates like for each PR or what happens
on new PR requesting review and so on."* — per **event**, not per action. That maps onto guarded
steps directly: each step's trigger gets a checkbox, per repo.

| event | full-auto | what "off" means |
|---|---|---|
| a new pull request requests your review | read it | it queues, you press |
| the head moves on one you blocked | re-review | queues |
| the head moves on one you **approved** | re-check | *this is the stale-approval hole* |
| CI goes red on one you approved | re-open it | queues |
| someone replies to your finding | answer it | queues |
| post findings | unattended | you press |
| post changes-requested | unattended | you press |
| post approval | unattended | you press |

**The mechanism is identical either way** — the queue and the reading happen regardless. The
checkbox only decides whether the last step fires or waits. That is what makes manual mode the same
machine rather than a second one, which is what the owner asked for.

The five **event** rows are the trigger set in §10. The three **post** rows are not three switches:
they are one ordered ceiling, for the reason given there.

Unattended approval is the owner's decision, made explicitly. The argument against it is recorded
in §13 rather than re-litigated here.

## 10. The flags

**The flags are layers, and each answers a different question.** Four of the seven already exist,
which is the reason to write them down together: a new switch that overlaps `read_prs` or
`review_queue` would give two places to look for why nothing happened, and "why did it not review
this" must have one answer.

| # | the question it answers | switch | state |
|---|---|---|---|
| 0 | may **anything** act, anywhere in the fleet | `pr_workflows` / `SKEIN_PR_WORKFLOWS` | exists |
| 1 | may skein **read** this repo's pull requests at all — the money door | `read_prs` | exists |
| 2 | does this repo appear in **your queue** | `review_queue` | exists |
| 3 | may the engine **act** on this repo | `auto_review` | **new** |
| 4 | **which events** wake it | `auto_review_on` | **new** |
| 5 | **how far** it may go unattended | `auto_review_ceiling` | **new** |
| 6 | **whose** pull requests | `auto_review_authors` | **new** |
| 7 | **this one** pull request | the workflow assignment on the row | exists |

Layers 1 and 2 are deliberately not folded into 3. Reading costs money and is useful without any
automation; the queue is a view. A repo can reasonably be *read* and *queued* with the engine off,
and that is the state everything starts in.

### The per-PR flag exists already, and in both directions

This is the second thing asked for, and it needs no new mechanism — `workflow.rs` settled it on the
author side and the rule is written down there:

> A per-PR assignment always wins over a match, in both directions — including an explicit "no
> workflow" on a PR a rule would otherwise claim.

`prwork::Standing` already reports which of those happened, as `assigned` | `matched` | `excluded` |
`none` — *"because 'you chose this' and 'a rule chose this' are different things to see on a row."*
A reviewer flow is assigned to a row the same way, so:

* **on, in a repo that is off** — assign the reviewer flow to that one pull request;
* **off, in a repo that is on** — mark it excluded, and no rule reclaims it.

### The trigger set

The third thing asked for — *"the trigger is just review requested state, but not new commits"* — is
a **subset of this list**, and it is the proposed default:

| trigger | fires when | in the default set |
|---|---|---|
| `requested` | GitHub asks you by name | **yes** |
| `unreviewed-commits` | the head moves on one you have not decided | no |
| `blocked-commits` | the head moves on one you asked changes on | no |
| `approved-commits` | the head moves on one you **approved** | no — this is the stale-approval hole |
| `approved-ci-red` | CI goes red on one you approved | no |
| `reply` | somebody answers one of your findings | no |

Per repo, overridable per pull request. Turning them all on is the full-auto mode; `requested` alone
is the mode described in the ask; the empty set is the same as `auto_review` off, and should
therefore *say* it is off rather than presenting as on-and-inert.

### One ceiling, not three checkboxes

§9's table gave posting three separate switches. **One ordered ceiling is better**, and the reason
is that the three values are not independent:

    none  <  comment  <  changes-requested  <  approve

`auto_review_ceiling` names the furthest the engine may go on its own; anything beyond it is drafted
and waits for you. Three booleans permit "approve unattended, but ask me before commenting", which
is not a policy anybody wants and is exactly the kind of state a checkbox grid makes reachable by
accident. A ceiling cannot express it.

**Built as a type rather than a string**, so an unwritable value is unrepresentable, and it reads
leniently in the *narrow* direction: a ceiling a build does not recognise — one a newer skein wrote
— becomes `none` rather than the default. That is the opposite of `place::Purpose`'s lenient reader
and deliberately so. There an unknown value costs a box skein can no longer reach, so it guesses
towards keeping it; here the value is a **permission**, and a downgrade must never widen what skein
does while nobody is looking. Both fail towards the answer that cannot surprise anybody.

**The default when a repo is switched on is `comment`** — findings unattended, verdicts waiting.
Unattended approval is reachable because the owner chose it; it is not what switching a repo on
gives you, and the box's asymmetry argument is why the two are different questions.

### Two more worth having

**`auto_review_dry_run`.** The engine decides and shows what it *would* post, and posts nothing.
There is direct precedent — `prwork::Standing` is *"the dry run the owner asked to see before
trusting this, and the same `workflow::next` the tick uses… a preview computed a second way is a
preview that can disagree with what happens."* This is how a repo should be turned on for the first
time, and it is worth more here than on the author side: a merge is one visible event, a review is a
paragraph of judgement that is embarrassing rather than reversible.

**`auto_review_authors`** — `mine` or `all`. The intended use is reviewing what your own boxes open;
an outside contributor's pull request is a different risk, a different audience, and the first place
a wrong verdict is seen by somebody who did not opt into any of this. `mine` is the proposed default.

### One I am deliberately not proposing

**A settle or quiet period before re-reading.** It existed, and you removed it on 2026-08-24:
`worth_reading` records that the daily ceiling became *the* money guard and the hour became obsolete.
Nothing about an engine changes that argument, and the churn guard it would duplicate is already
built from two parts — the cache key `(number, head_sha)` means an unchanged head is never re-read,
and an automatic read is an **unasked** one, so it counts against `review_reads_per_day` while a read
you press stays free (`over_budget` returns early for an asked visit). The engine shares that
ledger; it does not get its own.

### How they resolve

Outermost first, and the first `no` ends it: kill switch → `read_prs` → `auto_review` (unless this
pull request is assigned, which overrides it) → is this trigger in the set → `auto_review_authors` →
the step's own conditions → `auto_review_ceiling` on the post.

**One rule about the money door.** A per-PR assignment overrides layer 3, never layer 1. A pull
request explicitly switched on in a repo whose reading is off must **say so on the row** — not
silently do nothing, and not silently spend. Failing quietly in either direction is the thing every
other guard in this file exists to avoid.

## 11. The reviewer is a box, managed

The owner's decision, and it is a **simplification rather than an addition**: *"use boxes instead
but group those boxes separately from manual boxes. That way you aren't creating a new class of
sessions but just box but managed automatically."*

That is right, and it deletes more of this design than it adds. An earlier draft of this section
proposed a `/skein/review` cgroup, a memory cap, a CPU weight, a concurrency setting and a cleanup
rule for `review/<repo>/trees/`. **A box already has all five**, and the checkout-as-conversation
mechanism (SKEIN-376) that the trees existed for is replaced by the box's own transcript.

### What it inherits for nothing

| what §11 was going to build | what a box already has |
|---|---|
| a cgroup under `/skein` | `/skein/<name>`, `max` 70% and `high` 55% of the boxes' share, `pids` 8192 — written before bwrap execs, so everything it forks inherits it |
| filesystem isolation | a bwrap mount namespace: private `$HOME`, private `/tmp`, a tmpfs over the fleet root with only this box's own directories bound back |
| a cleanup rule | `destroy_box` — namespace kill, `cgroup.kill`, container sweep, `rm -rf`, `forget_place`, `delist_box`, **logged to the warden** |
| pausing between rounds | `stop_box` — kills the tmux server; tree, placement and registry all survive |
| the conversation across rounds | `claude --name '<box>' --continue`, with the transcript on the **host mount** (`$SKEIN_HOME/boxes/<name>`), so it survives a stop, a restart and a fleet rebuild |
| somewhere to look when it goes wrong | the box's terminal, the board row, the turn-state probe |

**And the security argument inverts.** The injection surface — a session holding a write token while
reading a pull request somebody else wrote — was the one real objection to handing the write path to
a session. Today that session is a child of `skein-server`: no cgroup under `/skein`, no namespace,
the server's own `$HOME`, and a full view of the fleet volume **including `credentials/` and
`github-pats/`**. A box can reach none of that. Moving the reviewer into a box is the single largest
reduction in that surface available, and it is a side effect of a decision made for other reasons.

### The lifecycle

**Created** on the first round for a pull request. **Stopped** between rounds — non-destructive, frees
the compute, keeps the tree and the conversation. **Started again** when a trigger fires, resuming
the same session. **Destroyed** when the pull request closes, which `review::prune` already knows how
to ask.

That answers *"clean up after themselves"* with a verb that exists, and it makes the round-to-round
memory the box's own rather than a directory that happens to be an address.

### Four things it needs that do not exist

**1. A box comes up at the wrong commit.** *"When you are opening for a PR, it will pull its own PR
files and base files right? What do you need to implement there?"* — the honest answer is that it
splits in two, and one half is much smaller than it looks.

**The base needs nothing.** `clone_script` runs a full `git clone --branch <base>` — no `--depth`,
no `--single-branch` — so the box gets the base branch's whole history, every other branch as
`origin/*`, and `git merge-base origin/<base> HEAD` resolves. That is already what the reading path
does to name the range.

**A same-repo pull request needs one line, not a fetch.** Its branch is in `refs/heads/*`, so the
mirror has it and the clone brings it down as `origin/<branch>`. **The objects are already there.**
What is wrong is the last line of `clone_script`: `git checkout -B <branch>` with **no start
point**, which creates a fresh local branch at whatever HEAD is — the base tip — and the kit's
`git checkout "$branch"` then finds that local branch and stops. So the box is standing at the base,
holding the PR's commits and not on them. A review box wants `git checkout --detach <head_sha>`
instead: detached because there is no branch to be on, and because nothing about reviewing should be
able to push.

**A fork's pull request is the real gap.** Its head is in the contributor's repository, so it is in
no `refs/heads/*` of the base repo and neither the mirror nor the clone has ever seen it. GitHub
serves it as `refs/pull/<n>/head`, which `fetch_mirror`'s refspec —
`+refs/heads/*` and `+refs/tags/*` — does not ask for.

Fetch it **per pull request, in the box**, rather than widening the mirror. `+refs/pull/*` on every
mirror fetch would drag every pull request ever opened into every repo's mirror for ever, on repos
where that is thousands of refs nobody asked for; `git fetch origin pull/<n>/head` brings exactly
the one commit this box exists to read. The reading path already fetches GitHub directly when the
mirror is behind, so this is the same move at a narrower scope.

**And a round that moves the head needs the tree cleaned.** `git checkout` of a moved head leaves a
file the new commit deletes sitting in the tree, and the reviewer reads it as part of the change —
which is why the current path runs `git clean -fdx` after every move, and a review box must too.

So: stand detached at `head_sha`, fetch `pull/<n>/head` when the head is not already present, clean
after every move. Three lines in the box's own checkout step, and no change to how mirrors work.

**2. A name collision is silent adoption, not a refusal.** Boxes are named `<repo>-<slug(branch)>`,
and nothing enforces uniqueness at creation. A review box for a pull request on `feat/x` would take
the name of the owner's own box on `feat/x` — and `start_box_inner` does not refuse: it prints
*"already has a checkout; keeping it"*, re-provisions, and re-records the placement of somebody
else's box. So a review box needs a name that cannot collide — the pull request number, which the
branch does not carry — and a managed create must refuse rather than adopt.

**3. Nothing records why a box exists.** `PlaceRecord` carries `sandbox`, `ns_pid`, `home`, `tree`,
`sock`, `generation`, `ns_start`, `launcher` and `ceiling` — no origin, no purpose, no owner. The
launch spec is `{branch, agent}`. Grouping managed boxes apart therefore starts with a field, and it
has a precedent to copy exactly: `foreign` is set in `board.rs`, filtered server-side, given a term
in `cockpit/src/filter.mjs`, hidden by default, rendered as a tag, and pinned by wire assertions in
`board.rs` and `cockpit.rs`. A `managed` grouping follows that path and invents nothing.

**4. No box starts with an instruction.** `start_box`'s `agent_command` is `exec bash -l`; nothing in
the create path takes a prompt. The closest seam is the handoff brief — a `pending.md` under the
store, consumed once at the box's first `SessionStart` — and for later rounds `sandbox::resume_box`
already delivers a headless turn into a running box. So round one is create-with-a-brief and round
N is start-and-resume, both on existing shapes.

### What is still a resource question

A box's ceiling is **70% of the whole pool** — on this fleet, 16.8 GiB of 23.8 GiB, and five boxes
each carry that same ceiling. Ceilings are not reservations: they stop one box killing the sandbox,
not five exhausting it together. So a cap on how many review boxes run at once is real, and it is a
cap on **boxes** — the same unanswered question skein already has, not a new one.

**CPU is uncapped for every box** by deliberate choice: `box-session.sh` gives boxes an equal
`cpu.weight` and writes no `cpu.max`, on the argument that *"a `cpu.max` would idle cores while a
container waits."* A review box inherits that. If CPU is to be bounded, the honest place is the box
mechanism for all boxes, not a special case for reviews — otherwise the fleet has a limit on the
work nobody is waiting for and none on the work somebody is.

## 12. Coordination: there is none to build

The box named an atomic claim as the single most important missing mechanism — two boxes wrote
claim rows in the same minute three times, and once a peer nearly posted an `APPROVED` over a live
`CHANGES_REQUESTED`, which would have silently lifted a block.

**That whole family disappears here.** Those collisions exist because two *boxes* independently did
the work and raced at the post. This engine lives in `skein-server`: one process, one tick, one
*engine*. No lease, no claims file, no per-box attribution.

**Not "one writer", though — a draft of this said that and it is false.** Three things already post
to a pull request under the owner's login: the cockpit's `act` route, when a person presses it; the
reading session itself, straight to `gh` from inside its checkout; and any box, which also holds the
token. What the single tick buys is that **no two engine rounds race**, which is the collision the
box actually hit. A person pressing the button while a round is in flight is a different case, and
the sha guard in §4 is what makes it safe rather than a lease: a verdict is only posted against a
head whose reading is current, so a post that raced loses the guard rather than the data.

What does **not** disappear, and is worth separating out: the box's compare-and-set was doing two
jobs. The coordination half is gone. The freshness half — is my standing verdict against the
current head — is needed anyway, because it is what stops a stale approval reading as current.
That is correctness, not coordination, and it is `VerdictStanding`.

## 13. What was decided, and what it cost to ask

**Unattended approvals: yes** (owner, 2026-08-30), with the note that automatic review is behind
`auto_review` regardless, so the exposure is opt-in per repo before it is anything else.

The box's argument against is kept rather than dropped, because it is not caution but asymmetry: *a
wrong changes-requested is loud and somebody argues with it; a wrong approval is silent and it
discharges the review.* §10's ceiling is where that argument now lives — a repo can run everything
else unattended with the ceiling at `changes-requested`, which is the shape the box was asking for
without denying the owner the mode they chose.

**§7c stands as a correctness rule, not a gate** (owner: "sure"). A pass that did not cover the
whole changed file set at one commit may post findings and may never post an approval, wherever the
ceiling sits. The question that came back with the agreement — *"what is the case in which it does
not cover the whole changed file?"* — turned out to matter more than the rule: the answer is in §7c
and it turned out to be a **correction rather than an answer**: the cut diff is not the evidence,
because the reviewer stands in a checkout and can open what the cut dropped. The evidence is the
sweep. The rule is load-bearing rather than theoretical — and it costs one persisted field, because
the sweep was throwing its own answer away.

**The prohibition is lifted** (owner, 2026-08-30: *"lift the prohibition"*). Recorded at length
because it is a reversal of an argument this tree makes in eight places, and a decision nobody can
audit later from a diff that only deletes a sentence. The reading session is today *forbidden* to
give a verdict, in as many words —

> Post it as a COMMENT review and nothing else… **Never** approve and **never** request changes.
> Those are verdicts and they are the reviewer's to give, not yours — they have controls for exactly
> that.

— and the argument for it, which is a test's own doc rather than a comment, is the one to answer
rather than delete:

> the one place in skein where a model writes on a pull request unprompted, under the reader's name
> — so the boundary it is given is the assertion… **a model that can approve on their behalf is a
> different product from one that can leave a review.**

It is stated in **eight places**, not one: the merged prompt, `SWEEP_PROMPT`, two test assertions and
their doc comments, `prq`'s lane doc, and three passages in these docs. So lifting it is a survey
and not an edit, and §10's ceiling becomes the guard that remains.

Two things it must not take with it. **Nothing records who posted** — skein keeps no copy of a review
any more, by design, so an engine verdict is indistinguishable from the owner's, on GitHub and in the
queue. Against §2's *"every action audited, with which-workflow-which-step attribution"*, the
reviewer side needs an equivalent and has none. And **two tests go vacuous rather than red**:
`a_pull_request_you_have_reviewed_stays_in_the_queue` builds a comment-only fixture and would keep
asserting *"a comment is deliberately not a decision"* about behaviour the engine no longer has.

### The loop this opens — **decided: kept apart, per repo**

`prwork::facts_of` builds the merge train's `approved` from `standing_approvals` — *"the approvals
GitHub holds against the current head from any reviewer"* — and an approval the engine posts under
the owner's login is one of those. So wherever a merge train is switched on:

    engine reviews → engine approves → Facts::approved → label, await CI, merge, delete branch

skein approves its own work and merges it, with nobody in it. Neither half is wrong alone and both
were chosen deliberately; the composition had simply never been put to anybody, because until the
prohibition is lifted it cannot happen.

**The owner's answer (2026-08-30): keep them apart, per repo** — *"If needed, we can just chain them
by saying merge all approved ones, how it reach approved is not needed by merge train right."*

That reading is correct and it is the reason this costs nothing to build. The train reads
`Facts::approved` and has no interest in **provenance**: an approval is an approval, whoever left
it. So keeping the two apart is a *configuration* — do not switch both on for one repo — and
chaining them is the same configuration with both switched on, deliberately, by somebody who wants
exactly that. No mechanism has to know the difference, and none should: a train that asked who
approved would be a second place where "does this count" is decided, which is how
[`Facts::approved`] came to be wrong in the first place.

**What that does buy is one obligation: the composition must be visible.** A person who switches
auto-review on for a repo that already has a train has just built the loop, and nothing today would
say so. So wherever the two are both on, the surface says it in a sentence — *an approval this
engine posts will merge* — and `skein doctor` reports it. That is the house rule applied to a
configuration rather than to a failure: say it rather than let it be discovered.

**The reviewer is a box** (owner, 2026-08-30: *"use boxes instead but group those boxes separately
from manual boxes… you aren't creating a new class of sessions"*). §11. It deletes four mechanisms
this document had proposed, and inverts the injection-surface objection rather than answering it.

**The undoability asymmetry is accepted** (owner: "this is fine"). Recorded here because it is the
one place the closed-set argument is weaker on the reviewer side than on the author side: a review
can be dismissed and superseded, but an approval that discharges a block cannot be un-discharged
before somebody merges on it.

**No repo starts with `auto_review` on** (owner: *"auto review I will toggle on when needed. So no
default."*). It ships off everywhere and is switched on per repo by hand. The other two defaults in
§10 stand: the trigger set is `requested` alone, and `auto_review_authors` is `mine`.

## 14. Where it hooks in

| file | change |
|---|---|
| `src/workflow.rs` | the added conditions and actions; `next` itself unchanged |
| `src/prwork.rs` | `facts_of` gains the reviewer fields under §7's rules; the sweep gains a reviewer pass |
| `src/review.rs` | `Read` calls the existing critique; the existing cache is the finding store |
| `src/prq.rs` | `submit_review_with_comments` is the post, unchanged |
| `workflows.json` | reviewer flows beside author flows, same file, same shape |

## 15. Build order

The author side's own order, which exists for a reason worth repeating: *"nothing can act until the
thing that decides can be shown to be right."*

1. **The vocabulary and the evaluator.** Pure, no network, tested against every state — including
   the box's eight near-misses as fixtures. Nothing can act at this stage because no action is
   wired.
2. **The adapter**, with §7's three rules, and a test per rule that fails when the rule is removed.
   The truncation rule in particular needs a fixture where skein was sent a short list.
3. **`Read`**, wired to the existing cache. Still no posting. — **done**, and it split in two.
   **3a** is the wiring: `prwork::read_now` behind §10's flags, anchored on the commit the step was
   decided about, filing through `review::summarise` so `ReadingCurrent` and `ReadingWhole` become
   answerable on the next evaluation. **3b** is §11 — moving where that reading *runs*, out of
   `skein-server`'s own process and into the pull request's review box. 3a is what makes the engine
   demonstrably alive; 3b changes nothing about what `Read` means, which is why it is a substitution
   behind it rather than a prerequisite for it.
4. **The posts**, behind §9's gates, defaulting to off.
5. **The per-repo owed-checks file** from §8.

**What 3a settled that was open.** `Read` is a *wait* in three cases and a stop in three others, and
the split is not the one the other acts use. Every other act in `prwork::perform` turns a failure
into a stop, on the module's own argument: a decision made from facts a failure has just proved
stale must not be made again. A reading is the one act that is not like that — it changes nothing
outside skein, and its failures are the transient kind (the day's ceiling, a diff that would not
download, a model call that timed out). So an unread pull request *waits*, carrying
`unread_because` verbatim; the fail-closed behaviour is already in the type, because
`Depth::Unread` leaves `ReadingWhole` false and `PostApproval` unreachable. What stops is a step
skein may not take at all: automatic review switched off, or a reading that would be filed against
a commit the pass did not evaluate.

**And it cost one module edge.** `prwork -> review`, recorded in `docs/modules.toml`. It also paid
for itself: `prwork` had been reading `review`'s cache through a hand-copy of `cache_path` and a
walk over the JSON by field name, and that copy is now gone from production.

Steps 1 and 2 are where the value is. The box's failures were all adapter and anchoring failures,
not engine failures — and this design would inherit every one of them if built in the other order.
