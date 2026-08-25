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
| `Queue.viewer` | `src/prwork.rs` (`facts_of`), `src/review.rs`, `src/bin/skein-server.rs` — who "you" are, for authorship and review attribution |
| `Summary.computed` | `src/review.rs` — whether a reading was actually produced, which gates what the pane is told |
| `Signal.symbol` | `src/shape.rs` — the greppable form of what moved, which is how mention counts are found |

**Dead.** No reader anywhere: not the page, not skein. Kept listed rather than deleted because
deleting a field changes a payload and a cache shape, and that is a decision with an owner.

| field | why it is still here |
|---|---|
| `Pr.settled` | The settle rule was deleted and the field was not (SKEIN-234). Nothing reads it — the only `settled` left in the crate is prose and an unrelated field on `queue.rs`'s own type. |
| `Pr.box_name` | Its one reader is a test that asserts it equals the function it was just assigned from, which proves the assignment and not a consumer. The page recomputes the same string for itself. |

Deleting those two is SKEIN-298, not this document's job.

**Ahead of its reader.** The field is for the page, the page does not draw it *yet*, and the work
that will is a live item. This is the one category with an expiry: the line names the item that
deletes it, and the document's closing rule — "a declaration naming a field the page now reads is
an exemption that outlived its reason: delete the line" — is what ends it. It is deliberately
narrow. "The page will read it one day" with no item is the same claim as "used server-side" with
no site.

| field | drawn by |
|---|---|
| `Pr.review_threads`, `Pr.review_threads_total`, `Pr.comments_total`, `Pr.review_requests`, `ReviewThread.outdated`, `ReviewThread.started_at`, `PrComment.created_at` | SKEIN-300's PR panel — fetched by SKEIN-301, exemption removed by SKEIN-318 |

## Declarations, machine-readable

The test parses this section and nothing else, so a field is declared exactly when it has a line
here. Format: a list item whose first backticked span is `Type.field`.

- `Pr.labels` — server-consumed by the merge train (`src/prwork.rs`, `src/workflow.rs`)
- `Pr.review_decision` — server-consumed by the merge train (`src/prwork.rs`)
- `Pr.merge_state` — server-consumed by the merge train (`src/prwork.rs`)
- `Queue.viewer` — server-consumed by `src/prwork.rs`, `src/review.rs`, `src/bin/skein-server.rs`
- `Summary.computed` — server-consumed by `src/review.rs`
- `Signal.symbol` — server-consumed by `src/shape.rs`
- `Pr.settled` — DEAD, delete pending (SKEIN-234, SKEIN-298)
- `Pr.box_name` — DEAD, delete pending (SKEIN-298)
- `Pr.review_threads` — AHEAD OF ITS READER: fetched by SKEIN-301, drawn by SKEIN-300's panel (delete this line with SKEIN-318)
- `Pr.review_threads_total` — AHEAD OF ITS READER (SKEIN-301 / SKEIN-300, delete with SKEIN-318)
- `Pr.comments_total` — AHEAD OF ITS READER (SKEIN-301 / SKEIN-300, delete with SKEIN-318)
- `Pr.review_requests` — AHEAD OF ITS READER (SKEIN-301 / SKEIN-300, delete with SKEIN-318)
- `ReviewThread.outdated` — AHEAD OF ITS READER (SKEIN-301 / SKEIN-300, delete with SKEIN-318)
- `ReviewThread.started_at` — AHEAD OF ITS READER (SKEIN-301 / SKEIN-300, delete with SKEIN-318)
- `PrComment.created_at` — AHEAD OF ITS READER (SKEIN-301 / SKEIN-300, delete with SKEIN-318)

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
