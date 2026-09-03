# Every serialised queue field, and what reads it

A field on a payload type exists to be read by something. When the thing that read it goes away, the
field does not: it keeps being computed on every refresh, keeps being written into the cache, keeps
being sent over the wire, and keeps *looking* like a fact the cockpit uses. Nothing fails. The only
way anyone finds out is a census — and a census is a photograph, so it starts going stale the moment
it is taken.

`tests/queue_field_readers.rs` is the standing version of that census. This document is the half a
person writes: **the list of fields the page does not read, each with the reason it is serialised
anyway.** The test refuses any field that is in neither.

## The rule

> Every field serialised on a queue payload type is read by `src/web/index.html`, or is named below
> with the reader that justifies it.

Payload types are the `Serialize`-deriving structs in `src/prq.rs`, `src/prwork.rs`,
`src/review.rs` and `src/contracts.rs` — the queue, the merge train, the review pane and the
contract signals. The test prints the census it measured when it runs:

```sh
cargo test --test queue_field_readers -- --nocapture
```

**The check is by field name, and deliberately loose.** A name counts as read if it appears anywhere
in the page as a property access — `.name` or `["name"]` — without proving the object it was read
from is this payload. That is the wrong error to make on purpose: a false accusation gets the test
deleted, a miss is one census away. It is the same trade `tests/page_scripts.rs` states for the same
reason. Two consequences worth knowing: a field whose name is shared with another payload is covered
if *either* is read, and a field named something very common (`name`, `url`, `error`) is nearly
impossible to catch here.

## The two ways a field gets on this list

**Server-consumed.** The field is not for the page at all. It round-trips through the on-disk queue
cache and is read by skein itself — most of them by the merge train, which decides from a `Pr` it
read back rather than from one it just fetched. These are correct and should stay. Each names its
reader, because "used server-side" without a site is the claim that rots.

| field | read by |
|---|---|
| `Pr.labels` | `src/prwork.rs` — copied into `workflow::Facts`, then `Cond::Label` / `Cond::NoLabel` in `src/workflow.rs` |
| `Pr.review_decision` | `src/prwork.rs` — `approved` and `changes_requested` in `facts_of` |
| `Pr.merge_state` | `src/prwork.rs` — `behind`, which is what `Cond::Behind` answers from |
| `Pr.labels_total` | `src/prq.rs` — `Pr::labels_whole`, which is what makes the blind-spot sentence at `src/prq.rs:1223` say a `no-label:` condition cannot hold |
| `Pr.standing_approvals` | `src/prwork.rs` — `somebody_approved` in `facts_of` (SKEIN-356), which is how anybody's approval and not just yours reaches `Facts::approved` |
| `Pr.reviews_total`, `Pr.reviews_read` | `src/prq.rs` — `Pr::reviews_whole`, the pair being what the blind-spot sentence beside it says out loud: a pull request with more than `REVIEWS_FETCHED` reviewers had the rest cut, so `my_review` and `standing_approvals` are floors rather than answers (SKEIN-386) |
| `Queue.viewer` | `src/review.rs` — `read_waiting` builds `identities` from it, and `src/prwork.rs` / `src/bin/skein-server.rs` pass it to `facts_of_in` — `facts_of` itself is `cfg(test)` now, because it answers a reading it has no repository to look up. It had a page reader (`revViewerOf`) until the drafted-review surfaces went; the SERVER reads it back off the cached queue on every pass, which is what this table is for |
| `Signal.symbol` | `src/shape.rs` — the greppable form of what moved, which is how mention counts are found |
| `Pr.replied_to_me` | `src/review.rs` — `triggers_read_from`, and `src/prwork.rs` — `facts_of_in`, which is what `workflow::Wake::Reply` fires from (`docs/pr-review.md` §10). Answered in `prq` where the viewer's login is in scope. **And the page reads it now**: `cockpit/src/move.mjs` — `answered`, which gives `moveOf` a `replied` bucket above `decided`, so a pull request whose author has answered your verdict stops sitting in "waiting on others" |
| `Summary.owed_triggered` | `src/prwork.rs` — `what_this_change_still_owes` and `the_first_check_still_owed`, which read it back off the cached summary to answer `Cond::ChecksOwed` (`docs/pr-review.md` §8). It is the one place the whole diff was in hand, so the answer is computed once at reading time and stored against the sha; `Known::thin` clears it so it never rides a row |
| `Summary.swept` | `src/prwork.rs` — `the_reading_skein_holds_at`, which is what `facts_of_in` turns into `workflow::Facts::reading_whole`. The reading's coverage is the one thing an approval waits on (`docs/pr-review.md` §7c), and the engine reads it back off the cached summary rather than out of the pass that wrote it |

**Dead.** No reader anywhere: not the page, not skein. Kept listed rather than deleted because
deleting a field changes a payload and a cache shape, and that is a decision with an owner.

**Empty, and that is the point.** `Pr.settled` and `Pr.box_name` were the two entries here; both are
deleted (SKEIN-298). `settled` outlived the settle rule by two months and `box_name`'s only reader
was a test asserting it equalled the function it had just been assigned from — which proves an
assignment, not a consumer. The category stays because the next dead field will want it.

**Ahead of its reader — and now empty, which is how it was supposed to end.** SKEIN-318 deleted all
seven the day SKEIN-300's panel drew them; the lines named the item that removes them and it removed
them. Kept as a category because it will be wanted again, not because anything is in it.

`Pr.review_decision` and `Pr.merge_state` left the **server-consumed** list at the same time and for
a different reason: their server readers are still real (`src/prwork.rs`'s `facts_of`), but the panel
reads them now too, and the closing rule below does not care why a line was written — a declaration
naming a field the page reads is an exemption that outlived its reason. Their rows stay in the prose
table above, which the test does not parse; only the machine-readable lines went.

The definition, for when the category is next used. The field is for the page, the page does not draw
it *yet*, and the work that will is a live item. This is the one category with an expiry: the line names the item that
deletes it, and the document's closing rule — "a declaration naming a field the page now reads is
an exemption that outlived its reason: delete the line" — is what ends it. It is deliberately
narrow. "The page will read it one day" with no item is the same claim as "used server-side" with
no site.

| field | drawn by |
|---|---|
| `Pr.review_threads`, `Pr.review_threads_total`, `Pr.comments_total`, `Pr.review_requests`, `PrComment.created_at` | SKEIN-300's PR panel — fetched by SKEIN-301, exemption removed by SKEIN-318. `ReviewThread.outdated` and `ReviewThread.started_at` were here too and are gone: the thread PANEL is deleted, and what is left of threads on the page is `moveNote`'s "N threads unresolved", which reads `resolved` and nothing else |

## Declarations, machine-readable

The test parses this section and nothing else, so a field is declared exactly when it has a line
here. Format: a list item whose first backticked span is `Type.field`.

- `Pr.labels` — server-consumed by the merge train (`src/prwork.rs`, `src/workflow.rs`)
- `Pr.labels_total` — server-consumed by `Pr::labels_whole` in `src/prq.rs`, which is what stops a `no-label:` workflow condition holding on a truncated label list (SKEIN-373)
- `Pr.standing_approvals` — server-consumed by `facts_of` in `src/prwork.rs` (SKEIN-356)
- `Pr.reviews_total` — server-consumed by `Pr::reviews_whole` in `src/prq.rs`, which is what makes the queue say that a pull request's reviews were cut off at `REVIEWS_FETCHED` instead of the row reading as one nobody has approved (SKEIN-386)
- `Pr.reviews_read` — the other half of that pair; `Pr::reviews_whole` is never read without it (SKEIN-386)
- `Queue.viewer` — server-consumed by `read_waiting` in `src/review.rs` (it is what `identities` is built from) and by `facts_of_in` in `src/prwork.rs`. It had a page reader until the drafted-review surfaces went with `revViewerOf`; the server reads it back off the cached queue on every pass
- `Signal.symbol` — server-consumed by `src/shape.rs`
- The two INPUTS to `Pr.replied_to_me` — its `my_review_at`, and each thread's `last_author`/`last_at` — are `skip_serializing` rather than listed here, because they are consumed at parse time and nothing reads them after a round trip. `replied_to_me` itself left this list on 2026-09-03: the page reads it now (`cockpit/src/move.mjs`, the `replied` lane), which is the exemption having done its job
- `Summary.owed_triggered` — server-consumed by `what_this_change_still_owes` in `src/prwork.rs`, which is what makes `Cond::ChecksOwed` answerable and an `audit` step reachable (`docs/pr-review.md` §8)
- `Summary.swept` — server-consumed by `the_reading_skein_holds_at` in `src/prwork.rs`, which is what makes `Cond::ReadingWhole` answerable and an approval reachable (`docs/pr-review.md` §7c). The page shows the reading, not what it covered

## What to do when this test fails

It names the field and which of the two things happened.

**A field is serialised and nothing reads it.** Either the page should read it — that is usually the
bug, and the field is the evidence — or it belongs in the list above with its reader named, or it
should not be serialised. Adding a line here is fine; adding one that says "used server-side" with
no site is how this list stops meaning anything.

**A declaration names a field the page now reads.** The exemption outlived its reason: delete the
line. This direction is not politeness. It is what the census that produced this document got wrong
— it listed `Queue.trunk` and `MergedQueue.skipped` as unread, and by the time the test was written
the page read both. A list of exceptions nobody prunes becomes a list of things nobody checks.
