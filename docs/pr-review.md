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
| reading one at depth, cheaply, in stages | `review::summarise`, staged 0/1/2 |
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
| `Read` | run the critique at the current head; record findings against that sha |
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

Unattended approval is the owner's decision, made explicitly. The argument against it is recorded
in §11 rather than re-litigated here.

## 10. Coordination: there is none to build

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

## 11. What is not decided

**Unattended approvals.** Chosen by the owner. The box argued against it, and its argument is not
caution but asymmetry, so it is recorded rather than dropped: *a wrong changes-requested is loud and
somebody argues with it; a wrong approval is silent and it discharges the review.* The partial-pass
rule in §7c is proposed as absolute regardless of where that checkbox sits — it is a correctness
rule, not a gate — and that is the one thing in this document asked for explicitly.

**Which repos start with it on.** Nothing here proposes a default. `review_queue` is already
per-repo and off unless switched on.

**Whether posting is undoable enough.** Every author-side action was chosen partly because a person
can undo it. A review can be dismissed and superseded; an approval that discharges a block cannot be
un-discharged before somebody merges on it. This is the one place the closed-set argument is weaker
on the reviewer side than on the author side, and it should be said out loud before it is built
rather than discovered.

## 12. Where it hooks in

| file | change |
|---|---|
| `src/workflow.rs` | the added conditions and actions; `next` itself unchanged |
| `src/prwork.rs` | `facts_of` gains the reviewer fields under §7's rules; the sweep gains a reviewer pass |
| `src/review.rs` | `Read` calls the existing critique; the existing cache is the finding store |
| `src/prq.rs` | `submit_review_with_comments` is the post, unchanged |
| `workflows.json` | reviewer flows beside author flows, same file, same shape |

## 13. Build order

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
