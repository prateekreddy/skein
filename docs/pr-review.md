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

Re-evaluated from scratch every poll, one step per evaluation, a closed set of actions, one global
off switch, every action audited with which-workflow-which-step attribution, and no blind retries.
That is the machine. It does not need rebuilding to review.

## 3. The correction that matters most

The first draft of this design claimed that a stateless engine dissolves the box's failures,
because none of them could survive re-derivation. **That is wrong, and the box said why.**

Its worst near-miss was a stale `APPROVED` silently carrying a verdict from an old commit. That was
never a memory bug. GitHub's `reviewDecision` computes from the latest non-`COMMENTED` review, so it
answers *"does an approval exist?"* while it was being read as *"has the current head been
reviewed?"*. **A guarded step that re-derives every poll reads the same field and is confidently
wrong on every poll.**

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
| reading one at depth | a session **standing in a detached checkout** of the head (`8c49c34`) |
| a reading that continues rather than restarts | it **resumes the pull request's own conversation** (`f64e1ae`, SKEIN-376) |
| acting on GitHub as you | `GH_TOKEN` in the call, so `gh` works as the reviewer (`07ba534`) |
| posting the verdict | **the session posts its own**; skein keeps no copy (`f7099ac`) |
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
| `Read` | resume this pull request's session in its checkout at the current head; the session reads, decides and posts |
| `PostFindings` | submit as `Verdict::Comment` |
| `PostChanges` | submit as `Verdict::RequestChanges` |
| `PostApproval` | submit as `Verdict::Approve` |
| `Audit` | one owed check from §8, recorded against the sha |

`Flag` and `Wait` are reused unchanged. They already mean the two things a reviewer engine needs to
say when it will not act.

## 7. Three adapter rules, and all three fail closed

These are the §3 corrections, and none of them is a step.

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
ran and accounted for every changed file*, which needs no new field on the reading at all.

Access is not the same as having looked, which is why the rule survives its own correction: the box
that shipped an approval and a refusal 53 seconds apart had a checkout the whole time.

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

## 11. What a round may use, and what it must leave behind

Two costs the owner named before any of this is built: *"as long as they clean up after themselves
and use resources without blocking everything else"*, and *"limits on as a whole how much memory,
CPU % cap and so on."* Both are real, and one of them is a live hole today.

### The hole: a review runs outside every ceiling skein has

`/sys/fs/cgroup/skein` is the parent of every box's cgroup — *"the only place the boxes together can
be"* — and on this fleet it holds `memory.max` 23.8 G against a 26 G sandbox. A box is inside it:
this one reports `0::/skein/example-box-6`.

**`skein-server` is in no cgroup under `/skein`** (checked against every `cgroup.procs` beneath it),
and `src/ai.rs` writes no cgroup at all — `box-session.sh` is the only thing in the tree that does,
and it does it for boxes. So a review, which skein-server spawns, competes with the boxes for the
sandbox's memory **from outside the ceiling that exists to bound exactly that**. `memory_plan` says
what that costs: with no swap, *"overshooting is an instant kill rather than a slowdown, and the
victim is chosen across the whole VM — so the cost of being wrong is a dead sandbox, not a slow
one."*

The fix is one cgroup, not a new mechanism: **`/skein/review`, a sibling of `/skein/containers` and
a child of `/skein`.** Then the fleet's single ceiling finally covers everything skein starts, which
is what `/skein` was for, and reviews and boxes contend under one number instead of two.

### Memory is a hard cap; CPU is a weight

**Memory: `memory.max` and `memory.high`**, for `memory_plan`'s reason above. This is the one
resource where being wrong kills the sandbox rather than slowing it, so it is capped rather than
weighted.

**CPU: a weight, and this disagrees with the ask.** A percentage cap was asked for; the tree already
argues the other way, at the one place it made this choice:

> **A weight, not a cap.** A `cpu.max` would idle cores while a container waits… when nothing else
> wants the machine, a container should have all of it. A weight costs nothing while the machine is
> quiet and decides who yields when it is not.

Boxes weigh 100 each, containers 50, *"because a box is somebody waiting at a terminal, and a
container is work that box started and can wait a little longer for."* A review is nobody waiting at
a terminal, so **50, the same as a container, on the same reasoning.**

`cpu.max` is still offered — `review_cpu_max`, unset by default — because a person may want a review
to be provably unable to take the machine even when it is idle, and that is a legitimate thing to
want. The default is the weight; the cap is there for whoever decides idling cores is the price they
want to pay.

**pids**: capped as a box is, for the same reason a box is.

### Concurrency is a decision, not a side effect

Today readings are serial — `read_waiting` walks the queues on one `spawn_blocking` thread, and a
round is 160s+. That is not a policy; it is what sequential code does, and the only thing bounding
it is the daily read ceiling, which is money and not machine.

`review_concurrency`, default **1**, makes today's behaviour the stated default and lets it be
raised deliberately. Within the `/skein/review` ceiling, so raising it divides a fixed budget rather
than multiplying the fleet's exposure.

### Cleaning up

`review::prune` runs per repo against the open pull requests — and it cleans `summaries/` only.
**Nothing prunes `trees/`.** On this fleet `~/.skein/review/gadget-demo/trees/` holds 20
directories; they are empty now, but a populated one is a full clone, and they are on the host mount
rather than the sandbox's disk.

The rule follows from why the directory exists: it is **the conversation's address** (SKEIN-376), so
the *address* must outlive the round and the *checkout* need not. `clear_the_tree` already empties
one while keeping it, and is already called when a head cannot be found. So: empty the tree at the
end of a round the engine started, keep the directory, and let `prune` remove the address itself only
when the pull request is closed — the same test it already applies to summaries.

A reading a person asked for keeps its checkout, because they are standing in it.


## 12. Coordination: there is none to build

The box named an atomic claim as the single most important missing mechanism — two boxes wrote
claim rows in the same minute three times, and once a peer nearly posted an `APPROVED` over a live
`CHANGES_REQUESTED`, which would have silently lifted a block.

**That whole family disappears here.** Those collisions exist because two *boxes* independently did
the work and raced at the post. This engine lives in `skein-server`: one process, one tick, one
writer, by construction. No lease, no claims file, no per-box attribution.

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
and it is **the ordinary case for a large pull request**, not an edge. A 300 KB critique budget is
reached by real diffs, the cut is already taken at a file boundary, and the dropped paths are
already named. The rule is therefore load-bearing rather than theoretical, and it costs one
persisted field.

**The undoability asymmetry is accepted** (owner: "this is fine"). Recorded here because it is the
one place the closed-set argument is weaker on the reviewer side than on the author side: a review
can be dismissed and superseded, but an approval that discharges a block cannot be un-discharged
before somebody merges on it.

Still open: which repos start with `auto_review` on. Nothing here proposes a default beyond the two
in §10 — the trigger set is `requested` alone, and `auto_review_authors` is `mine`.

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
3. **`Read`**, wired to the existing cache. Still no posting.
4. **The posts**, behind §9's gates, defaulting to off.
5. **The per-repo owed-checks file** from §8.

Steps 1 and 2 are where the value is. The box's failures were all adapter and anchoring failures,
not engine failures — and this design would inherit every one of them if built in the other order.
