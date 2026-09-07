# The review queue, as a product

Written after the owner said the PR surface was "missing even basic thought", and asked for it to be
looked at by somebody who does this well rather than patched again.

## The brief I gave myself

> You have built review tooling that people kept open — the kind where the queue reaches zero most
> days and somebody notices when it is down. You know the things that are only learned by watching
> people use it:
>
> - **A queue's job is to be emptiable.** One that cannot reach zero stops being read, and a badge
>   that is always lit is worse than no badge, because it trains the eye to skip it.
> - **The unit is a decision, not a pull request.** "Twenty-nine open PRs" is a fact about the
>   repository. "Two things are waiting on you" is a fact about the person.
> - **Triage is the product.** Listing is free — GitHub already does it. The value is entirely in
>   what you are not shown, and in the order of what you are.
> - **The reviewer's real question is "can I do this now?"** which is size, then context, then
>   confidence. A tool that cannot answer the first one is a list.
> - **Whose move is it** is the axis everything hangs off, and it is not the same question as
>   "have I acted on this".
>
> Go and look at what is actually on the screen, with real data, and say what is missing.

## What is actually on the screen

Measured against the owner's live fleet on 23 August 2026 —
`GET /api/repos/gadget-demo/review`, 29 pull requests:

| field | distribution |
|---|---|
| `lane` | `needs-you: 29` |
| `checks` | `failing: 26`, `passing: 2`, `none: 1` |
| `reasons` | `reviewer: 22`, `author: 7` |
| `draft` | `false: 24`, `true: 5` |
| `my_review` | `none: 29` |
| `updated_at` | 21 today, 7 yesterday, 1 four days old |

Read those rows together and the product problem is already stated.

**Every single pull request is in the same lane.** The lane is the queue's only structural claim, and
here it carries exactly zero bits.

**Twenty-six of twenty-nine have failing CI.** On those the next move belongs to the author, not to
the reviewer. Reviewing a red pull request is usually work that has to be done again.

**Seven are the owner's own.** They are in the same lane, under the same heading, as the ones waiting
on the owner's review — while being the exact opposite situation.

**Five are drafts.**

Apply only the filters the payload already supports — not yours, not draft, not red — and the queue
goes from **29 to about 2**. That is the entire gap between what the product shows and what it means.

## The diagnosis: one axis, where three are needed

The lane derivation in `build_pr` was the whole of the triage when this was written:

```rust
let lane = if archived_numbers.contains(&number) {
    Lane::Archived
} else if review_is_current && matches!(my_review.as_str(), "approved" | "changes-requested") {
    Lane::Waiting
} else {
    Lane::NeedsYou
};
```

The queue models **your action history** — have you personally left a review on this commit — and
nothing else. It is a correct answer to a question nobody is asking. The three questions a reviewer
actually has are:

1. **Whose move is it?** Yours, the author's, another reviewer's, or CI's. A red build and an
   unanswered review request are both "not your move", for different reasons.
2. **Is it ready to be reviewed at all?** Draft, red, conflicted, no description.
3. **What will it cost you?** Ten lines in one file, or fourteen hundred across sixty.

Only the first is even partially modelled, and only through the narrowest of its four values.

## Ordering is actively backwards — **half of this was taken up**

The queue sorted by `updated_at` descending. In a review queue that is the wrong direction:
the pull request that has been waiting on you longest sinks to the bottom, and any push — including a
bot's — lifts a PR back to the top regardless of whether it moved toward being reviewable.

The right key is **how long this has been waiting on you**, oldest first. That is the one ordering
where working from the top clears the thing most likely to be blocking a colleague.

It is `newest_first` now — newest by NUMBER — and that doc records the reshuffling half of this
finding as the reason: a comment, a label or a bot's push moved a pull request to the top of the old
order without changing what it is, and a number never moves. The other half stands: newest-first is
still not oldest-waiting-first.

## Things already fetched and thrown away — **this one was taken up (SKEIN-142)**

The queue's GraphQL fragment asks GitHub for `reviewDecision` (`PR_FRAGMENT`), GitHub's own
verdict on whether a pull request still needs review: `REVIEW_REQUIRED`, `APPROVED`, or
`CHANGES_REQUESTED`. This section used to say nothing read it, and that the most authoritative
answer to the queue's central question was being fetched over the wire and dropped.

It is read now, in two places, and neither is a display: `prwork/facts.rs:85` treats
`CHANGES_REQUESTED` as a refusal, and `workflow/facts.rs` folds the same field into whether a pull
request is ready — where its doc records the thing worth knowing, that with CODEOWNERS off
`reviewDecision` stays `APPROVED` across pushes and GitHub means it. `build_pr` says so at the
parse: "GitHub's own verdict is read, not just fetched".

Kept rather than deleted because the *shape* of the finding recurs — the queue fetches more than it
reads, and the next audit should start from the fragment (`PR_FRAGMENT`) rather than from the
page. The enum spelling was also wrong here for as long as the section stood: GitHub's value is
`CHANGES_REQUESTED`, never `CHANGES_REQUIRED`.

Three fields are not asked for at all, and each is one word in the same query — GitHub reads are
cheap here, and the owner has said so explicitly:

- `additions deletions changedFiles` — the cost signal. Its absence is why no row can answer "can I
  do this now?".
- `mergeable` — a conflicted PR is the author's move, not yours.
- `reviewRequests` — whether you are the only reviewer or one of six. Being one of six is a
  materially different obligation and the queue cannot tell you which you are.

## The summary is decorative, and it should be an input

`src/review.rs` reads a pull request and produces a verdict — a one-line gist, tripwire flags for
contract changes, whether it needs explaining. The page renders that text beside the row.

Nothing else uses it. A reading that says "routine dependency bump" does not demote the row. A
reading that flags a moved default does not promote it. The most expensive thing the product
computes has no effect on the order or the shape of the thing it is computed for.

That is backwards. The verdict should be the strongest triage signal available, precisely because it
is the one that cost something.

## What it should be

Three lanes that mean something, in this order:

**Your move.** Awaiting your review, not draft, CI not red, sorted by **how long it has waited on
you**. This is the lane that should reach zero. On the fleet above it would hold about two rows.

**Their move.** Your own pull requests, and ones you have already reviewed. A different question and
therefore a different row: is anyone blocked on me, has anyone looked, is it green enough to merge.
Today these are mixed into the same list under a heading that says the opposite of what they are.

**Not ready.** Drafts, red CI, conflicts — collapsed to a count with a reason. `23 not ready · 21
failing checks, 5 drafts`. One click to open. Nothing here is hidden; it is just not pretending to be
your problem.

And on every row: **±lines and file count**, so the size question is answered before you open
anything.

## What I would build first, in order

1. **Lanes from readiness, not from your history.** The single change that takes 29 rows to 2. Needs
   `mergeable` and the existing `checks`, and nothing new to be invented.
2. **Sort by wait time.** One comparator. Fixes the ordering being upside down.
3. **Size on the row.** Three fields in the existing query.
4. **Use `reviewDecision`.** Already on the wire.
5. **Let the verdict move the row.** Flags promote, "routine" demotes.
6. **Queue-level actions.** "Not until CI is green" as a filter you can keep, so 26 red rows stop
   being 26 decisions.

Items 1–4 are mechanical and are most of the value. Item 5 is where this stops being a better list
and starts being a reviewer's tool.

## What was already right, and should not be lost

Worth stating, because a critique that only lists faults invites a rewrite that throws away the good
parts:

- **Blind spots are surfaced rather than swallowed** (`Queue::blind_spots`). A queue that
  under-reports silently is worse than no queue, and this one knows it.
- **The gist on the collapsed row** is the correct product instinct: most of a queue should never
  need opening.
- **Summaries are keyed to the head commit**, so a stale reading cannot be shown as a fresh one.
- **An unread PR is drawn loudly**, never as calm empty space.

The bones are right. What is missing is that nothing decides anything.
