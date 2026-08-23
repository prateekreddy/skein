# The pull request queue — a product review

Written 23 August 2026, against `in-fleet` at `05788ac`, from the code, from a real browser, and
from the owner's live fleet read over `/api/repos/<id>/review`. Every claim below either cites a
`file:line` or names the command that reproduces it. Where I only have one day's data I say so.

There is a prior review at `docs/review-queue.md`; §8 is my response to it, and I did not read it
until §1–§7 were written.

---

## 1. What this surface is for

One person reviews about thirty pull requests a day across nine repositories. That is the whole
brief, and almost everything follows from it.

Thirty a day is roughly one every fifteen minutes of a working day. At that rate the reviewer is not
short of *code reading* — they are short of **decisions about where to spend code reading**. The
scarce act is not "approve"; it is choosing which of thirty-nine open pull requests to open next,
and being right about it.

So this surface has exactly three jobs, in order:

1. **Order the work.** Put the thirty-nine in the sequence a person should actually take them in,
   and say why the top one is first.
2. **Make the decision cheap once taken.** When a row is opened, everything needed to decide must
   already be there — the change, its size, its shape, what is failing, what it depends on.
3. **Let the decision land without leaving.** Approve, request changes, comment, merge, set aside.

Skein's own module doc gets job 1 right in one sentence — *"a queue that lists and lanes PRs
correctly is already the product; summaries only decide how much reading each row saves you"*
(`src/prq.rs:19-20`). I agree with that sentence completely. The problem is that the built surface
does not do job 1, has almost none of job 2, and does job 3 well.

**The one-line judgement:** skein has built a very careful *renderer* for a queue it has not yet
built. The honesty machinery (blind spots, `Depth::Unread`, the head-SHA cache key) is genuinely
better than anything on the market. The queue underneath it is `gh pr list` with a dropdown, and it
is showing the owner one repository at a time when they have nine.

---

## 2. What is actually happening today

### 2.1 The live queue, read from the owner's fleet

```
curl -s -H "Authorization: Bearer $TOKEN" http://host.docker.internal:7878/api/review/counts
curl -s -H "Authorization: Bearer $TOKEN" http://host.docker.internal:7878/api/repos/gadget-demo/review
```

Nine registered repositories. **39 open pull requests need the owner**, in two of them:
`gadget-demo` 29, `lattice` 10, everything else zero.

For `gadget-demo`'s 29, every derived field is constant:

| field | distribution |
|---|---|
| `lane` | `needs-you` × 29 |
| `my_review` | `none` × 29 |
| `review_is_current` | `false` × 29 |
| `checks` | failing 25, passing 2, pending 1, none 1 |
| `draft` | 5 |
| `reasons` | reviewer 22, author 7 |
| `blind_spots` | `[]` |

`lattice`'s 10 are the same story from the other direction: `needs-you` × 10, `checks: none` × 10,
`author: prateekreddy` × 10.

**Across both repos, `lane` takes one value 39 times out of 39.** A field that never varies is not a
classification, it is a constant with a rendering cost. The three-lane taxonomy in `src/prq.rs:35-46`
is, on this fleet, one lane.

Median time since last activity is **0.8 days**; the oldest is **4.2 days** (#218). So this is not a
stale backlog that a single purge would fix. It is a live working queue that the surface is failing
to organise.

### 2.2 What it looks like

Rendered with the real page, driven by a browser, against a fixture rebuilt to the live shape above
— 29 PRs, 25 failing, 5 drafts, 7 authored by the viewer, and the 15-deep dependent chain described
in §3.3. Screenshots at `/tmp/claude-1000/pm/*.png`.

The top of the queue looks like this:

```
NEEDS YOU 29
● #652  fix(documents): stop the content-revision trigger firing on…    [reviewer] dev-vale
        stops the parser crashing on empty input.
● #623  feat(filing): signed example-topic-3/IA copies skip conversion…       [reviewer] dev-vale
        stops the parser crashing on empty input.
…
● #649  fix(prayer): drop the archaic "That" from the no-interim-…      [reviewer] dev-vale
● #646  tenants slice 9d: the module suspends and resumes a member      [reviewer] dev-rhea
● #640  feat(deploy): run the knowledge stack in prod…          [draft] [author] me
```

Six rows carry a second line. Twenty-three do not. Measured, not eyeballed:

```
GIST STATS {"rows":29,"gists":6,"lanes":["needs you 29"]}
```

The bottom two-thirds of the pane is a list of titles with a red dot and the word `reviewer` on
each. There is no age on any row, no size, no base branch, no indication that fifteen of them are
one change.

**Expanding a row is worth almost nothing.** Here is the entire expanded state for a PR skein has
not read (`f-expanded-deep.png`):

```
ladder/tenants-09c-example-topic-14 → ladder/tenants-09b-example-topic-16   checks failing
you have not reviewed this   open on GitHub ↗

Not read yet. Only the first few in this lane are read for you, so the fleet keeps its rate limit.
                                                                            [ read it ]

[approve] [comment…] [request changes…] [ask…]     [box on this branch] [merge] [set aside] [re-read]
```

The reviewer is being offered an `approve` button, first and highlighted, for a change they have
been shown nothing whatsoever about. That is the finding I would put in front of the owner first.

For a PR that *was* summarised it is barely better. `revDetail` falls back to
`<p>${esc(s.line)}</p>` (`src/web/index.html:2825`) when `Depth::Line` produced no `detail` — which
is the majority case, because `Depth::Line` is precisely the "nothing to see" verdict. So expanding
a routine bug fix re-renders the same sentence already on the collapsed row, plus a meta line, plus
eight buttons.

### 2.3 The states around the edges

- **Empty** (`h-empty.png`): `NEEDS YOU 0 / nothing here.` in the top-left of a 1400px page, and 800
  vertical pixels of nothing. In a nine-repo fleet the honest answer is "nothing here, but `lattice`
  has ten" — the server already knows that (§3.1) and does not say it.
- **Error** (`i-error.png`): a single orange box, `the queue could not be built / GitHub said: Bad
  credentials`. No retry, no link to Settings, no offer of the copy on disk that `prq::remember`
  just wrote. The prose is excellent; the affordance is absent.
- **Loading**: `asking GitHub…` as bare text (`src/web/index.html:2585`). Acceptable.

---

## 3. What is wrong, in the order I would fix it

### 3.1 The reviewer has nine repositories and the pane has one — and the data for all nine is already in memory

This is the largest miss on the surface and the cheapest to fix.

`renderReview` builds a repo picker as an HTML `<select>` (`src/web/index.html:2576-2582`).
`openReview(repoId)` resolves to `repoId || view.repo || localStorage("skein.reviewRepo") ||
repos[0].id` (`src/web/index.html:2492-2503`). The header button `#revbtn` calls `openReview()` with
no argument (`src/web/index.html:1108`), so clicking a badge that reads **39** drops you into
whichever repo you happened to look at last.

The per-repo count reaches the button, but only as a `title` attribute
(`src/web/index.html:2483-2489`) — a tooltip. It cannot be scanned, sorted, or clicked through.

And here is the part that makes this indefensible rather than merely unfortunate:

```rust
// src/prq.rs:807-840
pub fn counts() -> Vec<Count> {
    crate::repos::load_repos().into_iter().map(|repo| {
        …
        match queue(&repo, false) {
            Ok(q) => Count {
                repo_id: repo.id,
                needs_you: q.prs.iter().filter(|p| p.lane == Lane::NeedsYou).count(),
                …
```

`counts()` builds the **complete `Queue` for every repo** — every `Pr`, every reason, every check
rollup — every three minutes (`REV_POLL_MS`, `src/web/index.html:2459`), and then throws all of it
away except an integer. The cross-repo queue is already computed, already cached in process for 60s
(`src/prq.rs:389-398`) and already written to disk (`prq::remember`). Serving it costs one new route
and zero additional GitHub calls.

So "one repo at a time" is not a cost decision. It is a UI decision, and for the actual user it is
the wrong one: it turns "what should I review next" — the only question this surface exists to
answer — into a question the product structurally cannot answer, because the answer spans repos and
the pane does not.

**What they were probably optimising for.** The spine of `prq.rs` was deliberately inverted so that
*the repo owns the list* rather than a box owning it (`src/prq.rs:3-6`) — a genuinely good call,
because it makes PRs nobody in the fleet authored visible. But "the repo owns the list" is a
statement about the **data model**, and it was carried through into the **view** without being
re-argued there. The person reviewing does not own a repo. They own a day.

### 3.2 The spend limit is enforced in the wrong place, and every button press resets it

The owner's constraint is explicit: *"only LLM stuff is expensive so we need limits"*. There is a
limit. It does not limit spending.

```js
const REV_SUM_PARALLEL = 3;                       // index.html:2437
const REV_SUM_AUTO = 6;                           // index.html:2450
let revSumAuto = 0;   // read unasked so far, this queue load
…
revSumAuto = 0;                                   // index.html:2528, inside loadReview()
…
while (revSumBusy < REV_SUM_PARALLEL && revSumAuto < REV_SUM_AUTO && want.length) {
  revSumAuto++; revFetchSummary(want.shift());    // index.html:2704-2706
}
```

Two things are wrong with this, and they pull in opposite directions.

**It counts requests, not model calls.** `revFetchSummary` hits `/summary`, and the server returns a
cached summary without touching a model whenever `(number, head_sha)` is on disk
(`src/review.rs:551-555`). But `revSums` is a client-side `Map` that dies with the page, so after a
reload the six newest rows are re-requested, all six are cache hits, all six are free — and the
budget is gone. Measured:

```
first load :  GIST STATS {"rows":29,"gists":6}
reload     :  AFTER RELOAD {"rows":29,"gisted":["#652","#623","#650","#583","#644","#630"]}
```

The same six. **Rows 7 through 29 of the owner's queue will never be summarised, on any reload,
ever**, because the allowance is spent every time on work that costs nothing. The limit protects the
budget from the free operation and lets the expensive one through only once.

**And it is reset by acting.** `revAct` and `archivePr` both finish with `loadReview(true)`
(`src/web/index.html:2903, 2919`), which resets `revSumAuto = 0`. Approving a PR therefore
authorises up to six fresh reads. Setting one aside does too. Measured — after approving one PR, the
number of summarised rows went 6 → 12:

```
ACT {"afterAct":{ … "gisted":12}}
```

Thirty acts a day is up to a hundred and eighty stage-1 calls a day *triggered by button presses*,
with no ceiling anywhere, while the reviewer who simply reloads the page gets nothing new. The cheapest
triage gesture in the product — "set aside" — is also its most expensive.

The limit belongs on the server, counted in **cache misses per repo per day**, not on the client
counted in fetches per page load.

### 3.3 Fifteen of the twenty-nine are one change, and the queue shows them in reverse

Derived from the live payload, matching each PR's `base_ref` against every other PR's `head_ref`:

```
step  1  #613  queue row 27  ladder/chassis-tenants            ← develop
step  2  #614  queue row 26  ladder/tenants-01-compose         ← #613
step  3  #615  queue row 25  ladder/tenants-02-thing-tables   ← #614
step  4  #616  queue row 24  …
step  5  #617  queue row 23
step  6  #618  queue row 14
step  7  #624  queue row 13
step  8  #626  queue row 21
step  9  #628  queue row 19
step 10  #631  queue row 18
step 11  #627  queue row 17
step 12  #632  queue row 15
step 13  #642  queue row 10
step 14  #645  queue row  9
step 15  #646  queue row  8   ladder/tenants-09d-member-lifecycle
```

A linear chain of fifteen pull requests, more than half the repo's queue. To review it in the only
order it *can* be reviewed — bottom-up, because each diff is expressed against the one below it —
the owner must visit rows **27, 26, 25, 24, 23, 14, 13, 21, 19, 18, 17, 15, 10, 9, 8**. Down, up,
down. And note steps 6 and 7: `tenants-05-member-reads` is based on `tenants-06-context-seam`. The
author's own numbering is wrong, and no human reading titles would catch it.

Nothing on any collapsed row says a chain exists. `base_ref` is on the `Pr` struct
(`src/prq.rs:76`), is serialised, reaches the browser, and is rendered only inside the expanded meta
line (`src/web/index.html:2771`) — one PR at a time, behind a click. So the information needed to
reconstruct the chain is present in the payload and requires fifteen clicks and a pencil to extract.

Reviewing #646 first — which is where the queue puts it — means reading a diff whose base is fourteen
unreviewed pull requests deep. That is not a hard problem to detect: `base_ref ∈ {head_ref}` over
the list skein already has, no network, no model, about fifteen lines of Rust.

I would go further and claim this is the single highest-leverage *ordering* fact available, because
it is the only one that changes the count. Fifteen rows become one row with fifteen steps in it.
Twenty-nine decisions become fifteen.

### 3.4 You cannot see the code, and the diff is already crossing the wire

The review pane's only route to the change is `open on GitHub ↗` (`src/web/index.html:2774`).

Meanwhile:

- `prq::pr_diff_text` fetches the whole unified diff (`src/prq.rs:920-926`).
- `review::summarise` calls it with a 140 KB budget (`src/review.rs:332, 578`), hands it to the
  model, and drops it on the floor. It is never returned to the browser.
- `prq::pr_files` fetches the full changed-path list (`src/prq.rs:933-941`), and only the *owned*
  subset survives into the UI — `yours: src/parser.rs · and 1 path you do not own`
  (`src/web/index.html:2818-2820`). The reviewer is told the count of the files they cannot see.
- The cockpit **already has a diff viewer**, with syntax classes, hunk headers, and a click-to-comment
  composer interleaved between lines (`src/web/index.html:631-641, 3375-3417`). It is wired
  exclusively to a box: `openDiff(name)` → `showBox(name, "diff")` → `loadDiff(name)`
  (`src/web/index.html:2025, 3309`).

So skein has a diff, has a diff renderer, has an inline-comment composer, and connects none of them
to pull requests. A reviewer doing thirty a day is being sent to github.com for the primary act,
thirty times, and asked to come back to press the button.

There is a related consequence in what lands: `revPost` sends `{kind, body}` to `/act`
(`src/web/index.html:2887-2892`) — a single top-level review body. Line-anchored comments, which the
box path already produces, are not expressible against a PR at all. The product's weaker review
mechanism is the one pointed at actual pull requests.

### 3.5 A row that was read and a row that was never read look the same

`review.rs` states the rule in its own header and means it: *"A PR skein has not actually read stays
at full attention and says so"* (`src/review.rs:14-16`). `revDetail` honours it loudly —
`Not read yet.` with the specific reason, drawn as an unmissable block (`src/web/index.html:2796-2812`).

`revGist` does not:

```js
function revGist(s) {
  if (!s) return "";                                    // index.html:2755-2756
  if (s === "…") return `<div class="revgist dim">reading it…</div>`;
  if (s.depth === "unread") return `<div class="revgist unread">not summarised — …</div>`;
  return `<div class="revgist">${esc(s.line || "")}</div>`;
}
```

`if (!s) return ""` is the twenty-three-row case, and it returns nothing at all. On the collapsed
line — the only line most rows will ever show — "skein read this and it is routine" and "skein never
looked at this" are rendered as *the same short row*. The module's central invariant is enforced in
the state you have to click to reach and abandoned in the state you actually scan.

This is not pedantry. It is the exact failure mode the module was designed around: silence reading
as reassurance.

### 3.6 The row is missing every field a triage decision is made of

The collapsed row (`src/web/index.html:2734-2751`) carries: a check dot, `#number`, title, a `draft`
chip, a `new commits` chip, flag chips, reason chips, author. It does not carry:

- **age** — nothing anywhere says #218 has been sitting for 4.2 days. `updated_at` is on the struct
  (`src/prq.rs:80`) and is used only for sorting. It is not even in the expanded meta line
  (`src/web/index.html:2770-2775`).
- **size** — files and lines changed. `pr_files` already fetches it (`src/prq.rs:933`).
- **base branch** — see §3.3.
- **which check failed** — the row says `checks failing` (25 times out of 29) with no name and no
  link. The `statusCheckRollup` query already pulls every context
  (`src/prq.rs:584-587`) and the parser reduces the whole thing to one of four words.

Meanwhile every row carries a `reviewer` chip, which is true of 22 of 29 and is the thing the filter
chips above already select on. The row is spending its horizontal budget on its least discriminating
field.

**A signal that takes one value 86% of the time is not a signal.** The red dot is the clearest case:
it is red on 25 of 29 rows. It has stopped meaning "this is broken" and started meaning "this is a
row".

### 3.7 There are no keyboard bindings, at thirty a day

The global keymap (`src/web/index.html:6132-6172`) binds `j`/`k`/`↵`/`d`/`]`/`/` to the **fleet** —
`move(d)` walks `order`, the box list (`src/web/index.html:1983`), and `↵` runs
`openTerminal(sel)`. The review pane binds nothing. Confirmed in the browser: with the pane open and
29 rows on screen, `j` then `Enter` changed neither selection nor view.

```
KEYS {"before":{"mode":"review","sel":null},"afterJ":{…,"sel":null},"afterEnter":{…,"sel":null}}
```

With boxes present those keys are worse than dead — they move a selection behind the pane, and
`Enter` navigates out of the review surface entirely.

The product already knows this matters: `/` was added to the fleet because *"the one binding a fleet
product cannot do without past a dozen boxes"* (`src/web/index.html:6165-6166`). Twenty-nine rows is
past a dozen.

### 3.8 Smaller, but real

- **Sorted by `updated_at` descending** (`src/prq.rs:480-486`). GitHub bumps `updated_at` on any
  activity — a label, a comment, a force-push, a bot. So the sort key answers "what moved most
  recently", and the reviewer's question is "what has been waiting on *me* longest". These are close
  to opposites: a PR that has sat untouched for four days sinks to the bottom precisely because
  nobody has touched it.
- **The pane is 73% of a window whose other 27% is the fleet sidebar** — during review, an unrelated
  box list (in the screenshots, a four-step onboarding checklist). Rows use roughly the left half of
  their own width and then run to badges pinned right, so a 1400px window is showing a 600px queue.
- **The standing-notes panel opens above the queue** (`revModsHtml()`, `src/web/index.html:2672`),
  pushing the first PR below the fold. It is a repo-maintenance surface sitting on top of the daily
  work surface.
- **No search or text filter** within the queue. Nine repositories, thirty-nine PRs, no way to type
  "tenants".

---

## 4. What I would build, in order

The ordering rule is: **anything that reduces the number of decisions beats anything that improves a
single decision.** A reviewer at thirty a day is bounded by decision count.

Filed as SKEIN-145 with children SKEIN-146 … 154, in this order. They are deliberately disjoint from
SKEIN-138 … 144, which came out of `docs/review-queue.md` and which I would keep (§8).

### 1 — One queue across every repo *(largest gain, ~a day, no new GitHub cost)*

Serve what `counts()` already builds. `GET /api/review` returns the merged list with `repo_id` on
each row; the pane opens on it by default; the existing `<select>` becomes a filter chip alongside
`all / mine / to review / mentioned` rather than a mode switch. The badge that reads 39 opens
something with 39 rows in it.

Success test: the owner presses the badge once in the morning and never touches the repo picker.

### 2 — Collapse a stack into one row

Compute `base_ref ∈ {head_ref}` over the merged list (§3.3). Render a chain as a single row —
`tenants · 15 PRs · you are at step 1 of 15` — expanding to its members **in review order**, with
the next unreviewed step marked. No network, no model.

This is the item that changes the count: 29 rows become 15. It is also the one that prevents a wrong
review, which the others only make faster.

### 3 — Put the diff in the pane

The expanded row gets the file list, then the diff, rendered with the components that already exist
(`.diff`, `.ln`, `.hunk`). Server-side: return the text `summarise` already fetched instead of
discarding it. Then reuse the box pane's click-to-comment composer and extend `/act` to carry
`{path, line, body}` so a review can be submitted with line comments attached.

Until this lands, `approve` is a button offered next to no evidence, and I would move it behind the
diff rather than in front of it.

### 4 — Move the model budget to the server, and make it a real one

Count **cache misses**, not requests; hold the counter on the server per repo per day; stop resetting
it on every act. Then spend it correctly: read the *top of the ordered queue* — including a stack's
next step — rather than the six most recently touched. On the client, drop `revSumAuto` entirely and
let the server refuse.

Same money, and the six calls land on the six rows the reviewer is about to open.

### 5 — Rebuild the row around triage fields

Age since last activity, size (files/lines), base branch when it is not the default, and the **name**
of the failing check with a link. Drop the `reviewer` chip. Demote the check dot to a state that is
worth a colour — "failing and it is your change to fix" is a signal; "failing" is wallpaper.

Sort by how long it has waited on *you*, not by `updated_at`.

### 6 — Keyboard

`j`/`k` between rows, `↵` expand, `a` approve, `e` set aside, `o` open on GitHub, `/` filter — scoped
so they only bind when the review pane has focus, and so they stop reaching the fleet keymap behind
it. Every one of the thirty daily decisions should be reachable without the mouse.

### 7 — The edge states

Empty says what the other repos hold. Error offers retry, the remembered queue, and a link to the
setting that would fix it.

---

## 5. What I would deliberately not build

- **An "AI reviews it for you" mode.** The one-line summary is the right ceiling. The rule in
  `src/review.rs:9-16` — AI may only add scrutiny, never remove it — is the best decision on this
  surface and I would not weaken it for any amount of throughput. A model that approves is a model
  that must be checked, which costs more than reviewing.
- **A second stage-2 pass for everything.** Stage 2 is `claude-sonnet-5` with a 180-second timeout
  over 140 KB (`src/review.rs:639-643`). Earning it is correct. What is wrong is *which* PRs earn it
  (§3.2 item 4), not how many.
- **A merge queue, CI orchestration, or re-running checks.** 25-of-29 red is a CI problem in that
  repo, and the answer is to say so once, not to grow a build product inside a review pane.
- **Notifications beyond the badge.** Thirty a day means the reviewer is already coming here. A
  notification per PR is thirty interruptions to save zero decisions.
- **Making the queue non-personal.** `review-requested:you / author:you / mentions:you` plus team
  requests (`src/prq.rs:436-446`) is right and I would not widen it. "All open PRs" is a different
  product with a different owner.
- **Multi-column or split-pane layouts.** Fix the width the rows already waste (§3.8) before adding
  panes to divide it into.

---

## 6. Decisions I would reverse

| decision | probably optimising for | why I disagree |
|---|---|---|
| Repo-scoped pane, `<select>` to switch (`index.html:2576, 2492`) | the data model's spine — *the repo owns the list* (`prq.rs:3-6`), which is right | that is a claim about storage, not about a person's day. The user owns nine repos and one attention span, and the merged list is already computed by `counts()` |
| `REV_SUM_AUTO` on the client (`index.html:2450`) | keeping the guard next to the thing that triggers it, and off the server's hot path | the client cannot see what costs money. It counts fetches; only misses cost. Result: the free operation is rationed and the expensive one is uncapped per act |
| Sort by `updated_at` desc (`prq.rs:480-486`) | "most likely to still be moving" — stated in the comment | GitHub bumps that field for reasons unrelated to you. The oldest thing waiting on you sinks, which inverts the queue |
| Three lanes derived on every fetch (`prq.rs:35-46`) | never storing a state GitHub owns — genuinely correct | the *derivation* is right; the *dimension* is wrong. It splits on "have you acted", which is `false` 39 times out of 39. Split on whose move it is |
| Diff only via `open on GitHub` (`index.html:2774`) | a review queue is triage; GitHub is where you read code | the diff is already fetched (`review.rs:578`), the renderer already exists (`index.html:3309-3374`), and thirty context switches a day is the cost being paid to avoid wiring two things that are already built |
| `revGist` returns `""` for an unread PR (`index.html:2755`) | keeping the collapsed row quiet | it breaks the module's own invariant in the only state most rows ever reach. Absence of a summary must be visible where the summary would have been |

---

## 7. Summary of the argument

Skein's review surface is unusually principled about *not lying* — blind spots, `Depth::Unread`,
head-SHA cache keys, "read 3m ago · refreshing". That work is real and I would keep all of it.

But it has been built as a **viewer for one repository's pull requests**, and the person using it
has nine repositories and thirty decisions a day. Every top finding is a consequence of that one
framing error:

1. The queue is per-repo when the data for all nine is already in memory (§3.1).
2. The money limit counts the free operation and is reset by every button press (§3.2).
3. Half the queue is one change and the queue shows it backwards (§3.3).
4. You cannot see the code, though the diff and the renderer both already exist (§3.4).
5. Unread and routine are indistinguishable on the row you actually scan (§3.5).
6. The row is missing age, size, base and the name of the failing check (§3.6).
7. There is no keyboard, at thirty a day (§3.7).

Fix 1 and 3 and the queue becomes answerable. Fix 4 and the answer becomes actionable. Everything
else is speed.

---

## 8. Against the prior review (`docs/review-queue.md`)

Read after §1–§7 were written. Its recommendations are filed as SKEIN-138 and its six children
(139–144).

It is a good review. Its central diagnosis — *"the queue models your action history and nothing
else… a correct answer to a question nobody is asking"* — is the right sentence, and I arrived at
the same one independently from the same payload. I am not re-filing any of SKEIN-138 through 144.

### Where we agree

- The lane dimension is the wrong dimension, and the axis is *whose move is it* (§2.1, SKEIN-138/139).
  Its `src/prq.rs:670-676` citation is exact; I checked it.
- Sorting by `updated_at` is upside down (§3.8, SKEIN-140).
- The row cannot answer "can I do this now?" without size (§3.6, SKEIN-141).
- Twenty-something red PRs are one decision, not twenty-something (§3.6, SKEIN-144).

Two of its findings are better than anything I had. **`reviewDecision` is fetched and never read**
(SKEIN-142) — I saw the field in `SEARCH_QUERY` and did not check whether anything consumed it.
And **the verdict should move the row, not just annotate it** (SKEIN-143): the most expensive thing
the product computes currently has no effect on the order of the thing it is computed for. That is
the sharpest observation in either document. My §4 item 4 is weaker than its item 5 and should be
read as subordinate to it.

### Where I disagree

**Triage is not the product; it is half the product.** Its brief opens with *"Triage is the product.
Listing is free — GitHub already does it."* The first sentence does not follow from the second.
Reading the diff is also something GitHub already does, and the review does not conclude that
reading is therefore not the product. All six of its recommendations improve the *ordering of the
list*; not one puts a change in front of the reviewer. A queue whose terminal action is `open on
GitHub ↗` is a notification system with good manners. Given that skein has already fetched the diff
(`src/review.rs:578`) and already built the renderer (`src/web/index.html:3309-3374`), I would rank
§3.4 above four of its six items.

**Ordering a per-repo list optimises inside the wrong container.** Sorting `gadget-demo`'s 29
perfectly does not tell the owner whether they should be in `gadget-demo` at all — and on the day
we both measured, `lattice` held ten more. Cross-repo (§3.1) subsumes the ordering work, because you
have to sort the merged list anyway. Doing the ordering first means doing it twice.

**"Collapse red CI into a drawer" is riskier than it reads.** Its headline result — *"apply the
filters the payload already supports and the queue goes from 29 to about 2"* — reproduces: exactly
2 survive `reviewer && !draft && checks != failing`. But look at what is being collapsed. All
**fifteen** stack members are failing, which is one broken base propagating up a chain, not fifteen
broken pull requests. Failing spans all three authors (dev-rhea 15, dev-vale 5, prateekreddy 5).
That is the signature of a repo-wide CI problem, and under its proposal the entire chain — the
biggest real piece of work in the queue — disappears into `23 not ready` behind a click, while the
lane that is supposed to reach zero holds two PRs, **neither of which is green** (#652 `pending`,
#630 `none`). A drawer that swallows 86% of the queue on the strength of one broken pipeline is a
queue that reaches zero by not looking. I would keep red rows in the queue and demote them, and I
would make "which check failed" a visible field (§3.6) before making red a hiding rule.

One small note in the same spirit as the house style. Its distribution table gives `checks:
failing 26, passing 2, none 1` — 29 with no `pending`. The same endpoint today returns failing 25,
passing 2, **pending 1**, none 1. Either the queue moved between reads or `pending` was folded into
`failing`; if the latter, that is the one state where the next move genuinely is "wait", and it is
load-bearing for its recommendation 6.

### What it missed

1. **The stack (§3.3).** Fifteen of the twenty-nine — more than half the queue — form one linear
   dependent chain, presented in near-exact reverse of the order it must be reviewed in. Nothing in
   138–144 mentions `base_ref`. This is the largest count reduction available (29 rows → 15) and the
   only finding in either document where the current design produces a *wrong* review rather than a
   slow one.
2. **The cost bug (§3.2).** The owner's stated constraint is spending, and the mechanism meant to
   enforce it does not: `REV_SUM_AUTO` counts fetches rather than cache misses, so the allowance is
   consumed by free cache hits and rows 7–29 are never summarised on any reload; and it is reset by
   `loadReview(true)` after every approve and every set-aside. Reproduced in a browser, twice.
3. **The whole surface is one repo wide** (§3.1) — and that `counts()` already builds every repo's
   full queue and discards it (`src/prq.rs:830`), which is what makes cross-repo free rather than
   expensive, and which changes the cost side of several of its own recommendations.
4. **The read/unread ambiguity on the collapsed row (§3.5).** Its closing section credits the
   product with *"an unread PR is drawn loudly, never as calm empty space"* and lists it under what
   must not be lost. That is true of `revDetail` and **false of `revGist`**, which returns `""`
   (`src/web/index.html:2755`) — and `revGist` is the state twenty-three of twenty-nine rows are in.
   The invariant holds where you have to click and fails where you actually scan. Of everything
   here, this is the one I would most want it to have caught, because it is a claim about the code
   that reads correct until you render it.
5. **The keyboard (§3.7)** — zero bindings on a surface used thirty times a day, in a product that
   already argued itself into `/` for the fleet.
6. **That the inline-comment composer already exists** (`src/web/index.html:3375-3417`) and is wired
   only to boxes, so the PR path can only post a single top-level review body.

Its blind spot and mine have the same shape from opposite sides. It looked hard at the payload and
not at the pixels; I started from the pixels and would have missed `reviewDecision`. Both of us
should have begun from the same question: **what does the reviewer do in the ten minutes after they
pick one?** Neither document's recommendations answer that until mine reaches item 3.
