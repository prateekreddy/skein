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

Payload types are the `Serialize`-deriving structs in `src/prq/` and `src/contracts.rs` — the
queue and the contract signals. The test prints the census it measured when it runs:

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

**Loose about the object, strict about comments.** A line that opens a comment — `//`, `*`, `/*` or
`<!--`, the four forms the scanned files use — is not a read, because the looseness above is only
the right error while the miss stays the expensive direction. Until SKEIN-992 there was no comment
filter at all, so one line of prose naming `.thatname` was enough to excuse a field nothing reads.
Measured when the filter went in: 15 of the 118 serialised fields match on a comment line, and none
is excused only that way, because each also has a real reader. The filter is per line, so the
interior of a comment that runs on past its opening line is still scanned (SKEIN-996); the doc
comment on `page_reads` in `tests/queue_field_readers.rs` carries both the measurement and the
limit.

## The two ways a field gets on this list

**Server-consumed.** The field is not for the page at all. It round-trips through the on-disk queue
cache and is read by skein itself. These are correct and should stay. Each names its
reader, because "used server-side" without a site is the claim that rots.

| field | read by |
|---|---|
| `Pr.labels_total` | `src/prq/types.rs` — `Pr::labels_whole`, which is what makes the blind-spot sentence (`grep -n 'no-label:' src/prq/types.rs`) say a `no-label:` condition cannot hold |
| `Pr.reviews_total`, `Pr.reviews_read` | `src/prq/types.rs` — `Pr::reviews_whole`, the pair being what the blind-spot sentence beside it says out loud: a pull request with more than `REVIEWS_FETCHED` reviewers had the rest cut, so `my_review` and `standing_approvals` are floors rather than answers (SKEIN-386) |
| `Signal.symbol` | `src/shape.rs` — the greppable form of what moved, which is how mention counts are found |

**Dead.** No reader anywhere: not the page, not skein. Kept listed rather than deleted because
deleting a field changes a payload and a cache shape, and that is a decision with an owner.

**Empty, and that is the point.** `Pr.settled` and `Pr.box_name` were the two entries here; both are
deleted (SKEIN-298). `settled` outlived the settle rule by two months and `box_name`'s only reader
was a test asserting it equalled the function it had just been assigned from — which proves an
assignment, not a consumer. The category stays because the next dead field will want it.

**Ahead of its reader — and now empty, which is how it was supposed to end.** SKEIN-318 deleted all
seven the day SKEIN-300's panel drew them; the lines named the item that removes them and it removed
them. Kept as a category because it will be wanted again, not because anything is in it.

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

- `Pr.labels_total` — server-consumed by `Pr::labels_whole` in `src/prq/types.rs`, which is what makes the queue say that a label list was cut off (SKEIN-373)
- `Pr.standing_approvals` — no reader since the merge train was removed; kept because dropping a field changes the payload and the cache shape, which is a separate decision
- `Pr.reviews_total` — server-consumed by `Pr::reviews_whole` in `src/prq/types.rs`, which is what makes the queue say that a pull request's reviews were cut off at `REVIEWS_FETCHED` instead of the row reading as one nobody has approved (SKEIN-386)
- `Pr.reviews_read` — the other half of that pair; `Pr::reviews_whole` is never read without it (SKEIN-386)
- `Pr.cross_repository` — no reader since the merge train was removed; kept for the same reason as `Pr.standing_approvals`
- `Queue.viewer` — no reader since the review engine was removed; kept for the same reason as `Pr.standing_approvals`
- `Queue.whole` — server-consumed in `src/prq/refresh.rs`: a queue that did not see everything is never remembered over one that did, and its set-aside prunes stand down
- `Signal.symbol` — server-consumed by `src/shape.rs`
- The two INPUTS to `Pr.replied_to_me` — its `my_review_at`, and each thread's `last_author`/`last_at` — are `skip_serializing` rather than listed here, because they are consumed at parse time and nothing reads them after a round trip. `replied_to_me` itself left this list on 2026-09-03: the page reads it now (`cockpit/src/move.mjs`, the `replied` lane), which is the exemption having done its job

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
