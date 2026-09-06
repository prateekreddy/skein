# The pull request queue — the design

Written 23 August 2026 against `in-fleet` at `e310a24`, from a real Chromium driven against a fixture
rebuilt to the shape the product review measured: **29 pull requests, 25 failing, 5 drafts, a 15-deep
dependent chain, three authors, nine repositories**. Every number below was measured in the page, not
estimated; the rig and the captures are named where each claim is made.

This takes `docs/review-product-review.md` as settled on *what* the surface should do. It decides
what it should look and feel like. Where I reject something the PM proposed, it is on interaction
grounds and §9 says so.

---

## 0. The rig

```
# 29 PRs, 9 repos, the 15-deep chain, mixed authors, 1400×900
node /tmp/claude-1000/ux/dense.mjs          # captures + measurements
node /tmp/claude-1000/ux/probe.mjs          # the dynamic failures a screenshot cannot show
node /tmp/claude-1000/ux/render-proto.mjs   # the proposed row, injected over the live page
node /tmp/claude-1000/ux/render-proto2.mjs  # the reading view and the edge states
```

`rig.mjs` is `tests/ui/review.mjs` with the fixture replaced and the assertions removed. Nothing in
`src/` was edited: every prototype is `page.addStyleTag` + a `window.PROTO` render over the real
`revQueue`, so what the screenshots show is the real payload through proposed markup. The CSS worth
keeping is inline in §10.

---

## 1. What the eye does today, measured

At 1400×900 with 29 rows on screen:

| | measured |
|---|---|
| rows visible above the fold | **20** of 29 |
| row height, no summary | 36.2 px |
| row height, with summary | 61.7 px |
| rows carrying a summary | **6** of 29 |
| pane / wrap width | 1014 / 1000 px (fleet sidebar takes 380 px — 27% of the window) |
| whole list, scroll height | 1575 px |
| check dot | 7×7 px, **red on 25 of 29** |
| `reviewer` chip | on **22 of 29** |

And the contrast ladder, computed in the page against each element's real composited background:

| element | size | ratio |
|---|---|---|
| title | 13 px | 15.40 : 1 |
| gist | 12.5 px | 9.63 : 1 |
| **`revtag` chip (`reviewer`, `draft`)** | **10 px** | **9.32 : 1** |
| lane heading (`NEEDS YOU 29`) | 11 px | 5.35 : 1 |
| `#number` | 11.5 px | 4.83 : 1 |
| row background vs pane | — | **1.11 : 1** |

Read that table as a design brief and it states the problem without needing an opinion:

- **The least discriminating element on the row is drawn third-brightest.** `reviewer` is true 22
  times out of 29 and is what the filter strip above already selects on. It outranks the PR number
  and the only structural heading on the page.
- **Rows barely exist.** 1.11 : 1 against the pane; the hairline borders do all the separating. At
  29 rows the list reads as one grey texture with a red stipple down the left edge.
- **The biggest, brightest object on screen is the blind-spot banner** — an orange, bordered,
  bold-titled block reporting a *permanent* condition (`gh` has no `read:org`). It is drawn in
  exactly the treatment reserved for "the queue could not be built at all" (`s4-settled-top.png` vs
  `s13-error.png` — same box, same border, same weight). A permanent condition wearing the alarm's
  clothes teaches the eye to skip the alarm. This is the honesty machinery undermining itself, and
  neither prior document caught it.
- **The list is ragged, and the raggedness is meaningless.** Six rows are 61.7 px and twenty-three
  are 36.2 px. The tall ones are not the important ones — they are the six the budget happened to
  buy. The eye reads height as grouping and there is no group.

Scroll to the bottom (`s6-bottom.png`) and the failure is total: seventeen consecutive rows reading
`● #6xx  tenants slice N…  [reviewer]  dev-rhea`, numbered **9c, 9b, 9a, 8, 7, 5, 6, 4, 3, 2, 1,
chassis** — a countdown, which is the exact reverse of the order they can be reviewed in, presented
with the author's own numbering visibly out of order at 5/6 and nothing saying so.

---

## 2. The interaction failures neither document found

These come out of `probe.mjs`. None is visible in a still.

**2.1 Every re-render eats the caret.** `renderReview()` replaces `revpane.innerHTML` wholesale.
Measured with a half-typed comment in the composer:

```
before: { focused: "rev-compose", caret: 4 }
after : { focused: "BODY",        caret: 0 }
```

`renderReview()` fires on: each of up to six summaries landing, the 4-second stale re-poll
(`index.html:2534`), each filter change, and the module fetch. So writing "request changes" on a
queue that is still settling means losing the caret mid-sentence, repeatedly, with the text intact so
you do not notice until you type the next character in the wrong place.

**2.2 Text selection is destroyed the same way.** Selecting a branch name to paste into a terminal
and having a summary land wipes it: `{ had: "wip: pulling the sum", kept: "" }`.

**2.3 The list reflows under the pointer.** One late summary arriving at the head of the list shifts
**28 rows by 26 px**. Six of them arrive over several seconds after the queue paints. A click aimed
at a row lands on its neighbour — and the thing the neighbour opens has `approve` in it, first and
highlighted. This is a mis-click generator pointed at an irreversible-ish act.

**2.4 Expanding near the fold hides the actions.** Opening the last row that fits above the fold puts
its action strip at `top: 969px` in a 900px viewport, with `scrollTop` unchanged. The pane does not
scroll to reveal what it just created.

**2.5 Expansion is unbounded and never resets.** Five rows opened → six open (one was already), and
the scroll height goes 1575 → 2741. Expansions survive filter changes and refetches.

**2.6 Approving changes nothing you can see.** After `approve`: the row is still in the queue, still
open, still says `you have not reviewed this` for the two-to-four seconds the forced refetch takes,
selection does not move, there is no undo, and the summary count jumps 7 → 12 (the budget reset the
PM found, now fixed server-side in `e310a24`). The only feedback is a toast in the bottom-right
corner of a 1400px window, 900 px from where the eye is.

**2.7 The badge and the pane never agree, and there is no path between them.** In the nine-repo rig
the badge read **232** while the pane showed **29**, because `openReview()` with no argument resolves
to `localStorage("skein.reviewRepo")` (`index.html:2493`). Pressing a number opens something that is
not that number and never says why.

**2.8 Head chips are 21 px tall** (42×21 for `all`, 82×21 for `refresh`) — under any pointer-target
minimum, on the controls used most.

---

## 3. The design, in one screen

Proposed collapsed queue, at the same 1400×900, with the same 29 pull requests
(`v5-proto-fixed.png` — measured: **15 rows, all 15 above the fold, 29 px each, 884 px total**):

```
┌ #revpane ─────────────────────────────────────────────────────────────────────────────────────┐
│ [all 39][gadget-demo 29][lattice 10][skein 0][thing-web 0][ladder 0]…  [to review][mine][…] │  ← repo bar, 24px
├───────────────────────────────────────────────────────────────────────────────────────────────┤
│▏incomplete: team review requests are missing — `gh` cannot list your teams. Fix: gh auth …     │  ← standing condition, one line, amber rule
│                                                                                               │
│ your move — 7 decisions   from 29 pull requests, oldest first                                  │  ← the sentence, not "NEEDS YOU 29"
│▏25 of 29 are red — one pipeline, since Tue. Not fifteen broken pull requests; demoted, not hidden.
│                                                                                               │
│ ●   ▸  [stack] ladder — 15 pull requests, one change │ step 1 of 15 · from the bottom │1.2d│ │dev-rhea  │
│ ●  #583  refactor(store): one writer for the revision table │ not read          │1.3d│13f 855±│dev-vale │
│ ●  #630  chore(deps): bump serde to 1.0.219                 │ not read          │ 20h│11f 129±│dev-vale │
│ ●  #644  feat(search): index attachment text on upload      │ not read          │ 12h│19f 832±│dev-vale │
│ ●  #650  fix(render): drop the leading "The" from the sub-… │ stops the parser… │  7h│ 9f 490±│dev-vale │
│ ●  #623  feat(upload): signed images/JPEGs copies skip conv…│ stops the parser… │  5h│ 8f 677±│dev-vale │
│ ●  #652  fix(documents): stop the content-revision trigger… │ stops the parser… │  2h│13f  76±│dev-vale │
│                                                                                               │
│ their move  8   drafts, yours, and ones you have signed off                                    │
│ ○  #218  fix(auth): stop refreshing a token we already know…│ not read          │4.2d│13f 790±│me       │
│ ○  #660  fix(web): the dock no longer steals the fleet's j/k│ not read          │ 14h│ 8f 220±│me       │
│ ○  #649  fix(export): the docx footer loses its page numbe… │ not read          │  9h│18f 697±│dev-vale │
│ ○  #658  feat(prq): carry base_ref into the collapsed row [draft] │ not read    │  8h│ 4f 634±│me       │
│ …                                                                                             │
├───────────────────────────────────────────────────────────────────────────────────────────────┤
│ j k move   ↵ open   a approve   e set aside   n next unreviewed   → into the stack   g r repo  │  ← sticky, 10.5px
└───────────────────────────────────────────────────────────────────────────────────────────────┘
     ↑    ↑     ↑                                    ↑                  ↑    ↑        ↑
    move  #    title (+ chips that earned it)      gist column        age  size    author
    mark 46px  50fr                                32fr               34px 62px    78px
    14px
```

**Twenty-nine pull requests became fifteen rows, and all fifteen fit above the fold with 200 px
spare.** That is the density target stated as a rule: *the whole day's queue is one screen, or the
queue is lying about how big the day is.*

Five things carry that:

1. **One line per row, 29 px, five aligned columns.** `display:grid`, not `flex` — flex is what let
   the title take whatever width it wanted and pushed the gist onto a second line, which is what
   produced the 36/62 raggedness.
2. **The gist is a column, not a second line.** This is the single most important density decision,
   and its real payoff is dynamic: **a summary landing cannot change a row's height**, which kills
   the 28-row / 26-px reflow of §2.3 outright.
3. **The move mark replaces the check dot** in the leftmost, highest-value position on the row.
4. **`reviewer` is gone** — a chip must be true of a minority *and* change what you do.
5. **The right rail** carries the three fields the row was missing: age (the sort key), size, author.

---

## 4. The row, field by field

```
 ●   #652   fix(documents): stop the content-revision trigger…  │ stops the parser crashing…  │  2h │ 13f 76± │ dev-vale
 └┬┘  └─┬┘  └──────────────────┬──────────────────────────────┘   └───────────┬────────────┘   └┬┘   └───┬──┘   └───┬──┘
  │     │                      │                                              │                 │        │          │
 move  addr                  the thing                                    what it is          waited   costs      who
 14px  46px                  50fr, ellipsis                               32fr, ellipsis      34px     62px       78px
```

**The move mark** (`.mv`, 9 px). Not "checks". *Whose move is it* — the only field that changes what
you do next, and the one the prior review correctly named as the missing axis.

| | drawn | means |
|---|---|---|
| `.yours` | filled `--accent`, 3 px accent-bg halo | your move. **The only lit thing in the column.** |
| `.theirs` | 1 px `--border-3` ring, hollow | draft, yours, red-and-not-yours, awaiting someone else |
| `.blocked` | hollow rotated square | a stack step whose base is unreviewed |
| `.done` | `--done`, 55% opacity | you decided and it holds |

Scanning down the left edge is now a scan for *periwinkle*, and periwinkle is scarce — 7 of 29 in
this queue. Today that same column is red 25 times, which is not a signal, it is a texture.

**The gist column is never empty.** This is §3.5 of the PM's review fixed in the state it actually
fails in. `revGist` returns `""` for the 23 unread rows, so "skein read this and it is routine" and
"skein never looked" render as the same short row.

| state | rendered |
|---|---|
| read, has a line | the line, `--text-2` |
| read, nothing to say | the line — `Depth::Line` always produces one |
| **never read** | `not read`, `--faint`, italic, **dotted underline** |
| could not be read | `not read — <reason>`, same treatment |
| in flight | `reading it…`, animated ellipsis, **same height** |

The dotted underline is doing specific work: it is the page's only "this is a stated absence, not a
value" mark, and it is legible at a glance down a column of real sentences. `s4` vs `v5`: today the
23 unread rows say nothing; in the proposal they say `not read`, in a way that reads as a fact about
skein rather than a fact about the PR.

**Age is first in the rail because it is the sort key.** A queue sorted by a field it does not
display is a queue you cannot audit. `>3d` goes amber — in the rig `#218` at `4.2d` is the only amber
number on the page and it is the row the current `updated_at` sort buries at the bottom.

**Size is `13f 855±`** — files, then total lines. Two tokens, tabular-nums, because the question is
"can I do this now" and the answer is a shape, not a precise integer. GitHub returns
`additions deletions changedFiles` in the same GraphQL search; three words in `PR_FRAGMENT`
(`src/prq.rs:1722`).

**What earns a chip.** A test, applied to the current row:

| candidate | true of | earns it? |
|---|---|---|
| `reviewer` | 22 / 29 | **no** — and the filter strip above already selects on it |
| red check dot | 25 / 29 | **no** — moves to one repo-level line |
| `draft` | 5 / 29 | yes |
| `new commits` / `moved` | rare | yes |
| tripwire flags (`default`, `schema`) | rare, and computed | yes |
| **the failing check's name**, when it is yours to fix | rare | yes |

The rule: *a chip is for something true of fewer than a third of rows that changes what you do.*

**Red is repo-level.** 25 red rows is one broken pipeline, not 25 decisions — the PM's §8 answer to
the prior review's drawer proposal, drawn:

```
▏25 of 29 are red — one pipeline, since Tue. Not fifteen broken pull requests; demoted, not hidden.
```

Said once, at the top, in a 2 px red rule. Red on a *row* is then reserved for the one case that
deserves it: `[ci: build (nightly)]` on a PR that is failing **and** yours to fix.

---

## 5. The stack — the highest-value piece of work here

Fifteen of twenty-nine are one change. The queue shows them scattered across positions 6, 9, 11, 13,
15, 16, 18, 19, 20, 22, 23, 24, 25, 26, 27 — verified in the rig:

```
STACK-DETECTED {"total":29,"inChain":15,
  "queueOrder":[[6,646],[9,645],[11,642],[13,632],[15,627],[16,631],[18,628],[19,626],[20,624],
                [22,618],[23,617],[24,616],[25,615],[26,614],[27,613]]}
```

Detection is `base_ref ∈ {head_ref}` over the list skein already has. No network, no model. My
prototype computes it in the browser in nineteen lines (§10); it belongs in `prq.rs`.

### 5.1 Collapsed

```
 ●   ▸  [stack] ladder — 15 pull requests, one change │ you are at step 1 of 15 · review from the bottom │ 1.2d │ dev-rhea
```

Sorted into the queue by **the age of its next actionable step**, not its tip. The `[stack]` mark is
an accent pill; the head row gets a 5%-accent wash so it reads as a container, not a row.

### 5.2 Expanded (`v6-proto-stack2.png` — 15 steps, 32 px each, 543 px, fits)

```
 ●   ▾  [stack] ladder — 15 pull requests, one change │ step 1 of 15 · review from the bottom │1.2d│dev-rhea
        bottom-up: each diff is expressed against the one below it. The order here is
        base_ref, not the numbers in the titles.
   step 1  ●──  #613  chore(ladder): the tenants chassis, empty                        9f  47±  1.2d
   step 2  ○│   #614  tenants slice 1: compose brings the module up                   22f 740±  1.1d
   step 3  ○│   #615  tenants slice 2: the thing tables and their migrations         13f 533±  1.0d
   step 4  ○│   #616  tenants slice 3: identity rows carry a tenant                    4f 326±   23h
   step 5  ○│   #617  tenants slice 4: membership, and who may write it               17f 119±   22h
   step 6  ○│   #618  tenants slice 6: the context seam every read passes              9f 812±   20h
   step 7  ○│   #624  tenants slice 5: member reads go through the seam
                                            ⟨named 05, sits after 06⟩                 21f 470±   18h
   step 8  ○│   #626  tenants slice 7: invitations, expiry and replay                  3f  57±   17h
   …
   step 15 ○──  #646  tenants slice 11: cut the old single-tenant path over            1f 418±    6h
```

Four decisions in that block, each load-bearing:

**The rail is drawn, not described.** A 1 px line with a node per step, capped top and bottom. "Deep"
becomes a *length* you see rather than a count you read. The next step's node is filled accent with a
halo; done steps are `--done` and their titles strike through. Fifteen deep is 543 px of visible
rail and it feels like fifteen — which is the correct feeling and the one the current queue hides.

**The steps run bottom-up, and the view says why.** One line of prose, once, at the top of the
expansion. Not a tooltip: the whole failure mode is that this order looks wrong to somebody reading
titles.

**It contradicts the titles where they lie.** `#624` is titled "slice 5" and sits topologically after
"slice 6". The step carries `⟨named 05, sits after 06⟩` in amber. This is the piece I would fight
hardest for. The reason reverse order is dangerous is not that the queue's order is arbitrary — it is
that the *titles carry a competing order that looks authoritative*. A stack view that silently
re-sorts and says nothing leaves the reviewer believing the queue is wrong. The stack must actively
disagree with the titles, out loud, or the reviewer will trust the numbers.

**Expansion is exclusive.** Opening a stack or a row closes every other one. This is the fix for
§2.5, and for a stack it is not optional: 543 px inside an 850 px viewport is the whole screen.

### 5.3 Traversal, and the count

Entering the stack (`→` or `↵`) selects the next actionable step and scopes `j`/`k` to the steps.
`↵` on a step opens the reading view with `step 6 of 15 · ladder` in its header and a `next step →`
button in the verdict bar. Approving from inside a stack advances to the next step **without
returning to the queue** — the whole point is that you sat down to review a change, not fifteen
things.

One honesty note on the PM's framing. "Fifteen rows become one row" is right; "twenty-nine decisions
become fifteen" is not quite. A stack is one *entry point* and fifteen decisions taken in a forced
order. So the heading reads:

```
your move — 7 decisions   from 29 pull requests, oldest first
```

Seven things you can start. Twenty-nine pull requests. Both true, on one line, and it reconciles the
badge (which counts PRs) with the queue (which counts starts) — fixing §2.7's disagreement rather
than hiding it.

### 5.4 What a stack IS, once real data got hold of it (SKEIN-288)

The prototype above assumes a stack is a line. On the owner's own queue it is not, and assuming it
was produced the report *"PR ordering in stack is broken. For example, PR 586 is 4th on the list
while it shows up as 1st"* — then *"stacking seems incorrect altogether now"*. Three decisions came
out of that, and each replaces a guess with something knowable.

**A trunk is knowable, so it is known rather than guessed.** Detection walks `base_ref → head_ref`,
which means a pull request whose head IS the trunk turns the trunk into a seam: #625 is
`develop → master`, so `develop` became "a head" and every develop-rooted stack dissolved into it.
The first fix used *"this base has more than one open child"* as a proxy for *"this base is the
trunk"*. The proxy is the bug: **it cannot tell a trunk from a fork.** The queue already reports the
trunk (`prq::Queue::trunk`), so `revChains` severs at it by name and a fork is left alone. Where the
trunk could not be read the old proxy stays as the fallback — without a name skein genuinely cannot
tell the two apart, and shattering forks is the lesser failure against dissolving every stack.

**A fork is not a break.** Branching two fixes off one step of a stack is ordinary, and it happened
twice in the live queue (`fix/readiness-abstention-kinds` carries #586 and #671;
`fix/readiness-named-findings` carries #711 and #672). Cutting there split a 21-step change into two
stacks and stranded three more rows loose. The stack is therefore a **tree**, and it is drawn
depth-first: parents before children, so *"review from the bottom"* stays true reading downwards
along any path, and the step that starts a second branch says which step it left from. A list that
pretends to be a line is what the report was about; a list that says where it branches is a list you
can read the tree out of.

Rejected: drawing an actual tree with connectors. The stack row is already the densest thing in the
pane, indentation fights the five-cell grid §4 fixed, and the reader's question is *"what do I review
next"* — which depth-first order answers directly.

**A step number means depth, not position.** The number that lied was a position in whatever
fragment the page had managed to assemble. It is the step's depth in the stack now — and where the
bottom is out of sight it says so. #650's own base is `parser-results-file-hash`, in nobody's queue
(merged, closed, or past the end of a truncated search), so that stack is deeper than anything skein
can see and every number in it carries a `+`: `step 1+` means *at least the first*. Numbers are exact
only for a stack whose root sits on the trunk, because that is the only case where skein can see the
bottom. **Say what is known, or do not number** — a number that claims more than it knows is the
defect, not the rounding.

---

## 6. The keyboard

Zero bindings today, on a surface used thirty times a day, in a page that already argued itself into
`/` for the fleet. Confirmed in the rig: with 29 rows on screen, `j` then `Enter` changed nothing
(`KEYS {"before":{"mode":"review","sel":null},"after":{…,"sel":null}}`). With boxes present those
keys are worse than dead — `j` moves a selection behind the pane and `Enter` navigates out of review
entirely.

**Scoping.** `FLEET` in `src/web/vendor/cockpit.js:383` stays one table; `shortcutFor(event, where)`
gains `pane` to `where` and consults `REVIEW` first when `pane === "review"`. One table, one guard,
still testable in node — the property the existing design was built for. Keys not in `REVIEW` fall
through to `FLEET`, so `⌘K`, `⌘N` and `?` keep working; `j`/`k`/`↵`/`d`/`]` are shadowed while the
review pane has the dock, which is exactly the bug.

### Queue

| key | does | focus after |
|---|---|---|
| `j` `↓` | next row | selection moves; row scrolled into view with 60 px of lead |
| `k` `↑` | previous row | same |
| `g g` / `G` | first / last row | |
| `n` / `N` | next / previous row **you have not decided this session** | skips stacks you have finished |
| `→` | enter the selected stack; `j`/`k` now walk its steps | selection lands on the next actionable step |
| `←` `Esc` | leave the stack; selection returns to the stack's head row | |
| `↵` `o` | **open the reading view** for the selected row or step | focus moves into the diff |
| `e` | set aside | row greys **in place**, toast offers `u`, selection advances |
| `u` | undo the last act, 8-second window | selection returns to the row |
| `x` | re-read this one (spend a stage-1 call, deliberately) | |
| `/` | filter within the queue by text | focus in the field; `Esc` clears and returns |
| `g` `1`…`9` | jump to the *n*th repo in the repo bar | selection on its first row |
| `g` `r` | repo switcher, fuzzy, over all nine with counts | |
| `1` `2` `3` | audience: to review / mine / mentioned | selection preserved by number where possible |
| `?` | the key sheet | |

### Reading view

| key | does |
|---|---|
| `j` `k` | next / previous **hunk** (not line — a line-at-a-time diff at 30/day is a scroll wheel with extra steps) |
| `]` `[` | next / previous file |
| `c` | comment on the focused hunk's line; composer opens focused |
| `⌘↵` | save the comment (**already bound**, `index.html:3406`) |
| `a` | approve — posts the verdict *with* the pending line comments |
| `r` | request changes — opens the composer, focused, posts on `⌘↵` |
| `e` | set aside |
| `→` | next step in the stack (only inside one) |
| `Esc` `←` | back to the queue, at the row you came from, selection intact |
| `g` `h` | open on GitHub |

### Three deliberate absences

**`m` (merge) is unbound.** It is the one act in this pane that cannot be undone from this pane —
`revAct` already singles it out for a `confirm()` (`index.html:2898`). On a keyboard surface used
thirty times a day, one letter must not land a commit on a base branch. Chip only.

**`a` does nothing in the queue.** Pressing it on a selected-but-unopened row flashes the reason
inline — *open it first; `a` is bound where the diff is*. This is the PM's "move approve behind the
diff" expressed as a rule rather than a layout: **you cannot approve from a surface that is not
showing you the change.** It also removes any need to reorder the buttons, because there are no
verdict buttons in the queue at all.

**`o` is not "open on GitHub".** The fleet keymap already binds `o` → `open`, meaning "open the
thing, here". Rebinding it to leave the product would train exactly the wrong reflex. GitHub is
`g h`.

#### The one verdict outside the reading view, and why it did not break the rule

> **Cut, and the rule outlived it.** The control this section argues for is gone: skein keeps no
> review to approve *with*, because the session posts its own to GitHub under the reader's account.
> The section is kept because the argument is what survives — it is the worked example of when an
> exception to the rule below is allowed, and the next surface that wants one has to make it again.
> Still true in the code: the verdicts are on the row (`revVerdictHtml`), they carry the reader's
> own line notes (`revNotesFor` → `prq::submit_review_with_comments`), and `a` off the diff refuses.

**skein's own review block approved with the review it was showing** — one chip, no key
(SKEIN-273). Read against the rule above it looks like the thing the rule forbids, and it is worth
writing down why it was not.

The rule is *"you cannot approve from a surface that is not showing you the change"*, not *"approve
lives only in the reading view"*. The reading view was where a verdict could go because it is where
the evidence is. skein's review block is a second such surface: it prints a reading **of the named
commit** — the overall note, every drafted comment, the file and line each one sits on, and a stale
marking when the branch has moved past it — directly above the control. Approving there is agreeing
with what is on screen, which is exactly what the rule protects.

Three things hold it to that, and a change that drops any of them puts the rule back in play:

1. **The words that post are the ones printed above the control**, assembled by the same function
   that describes them, with the exact body on the control's own tooltip. A
   control that could post something other than what it shows is a verdict next to nothing again.
2. **The approval is the reader's, and reads as theirs.** The post goes out under their GitHub
   account, to their colleague, and the body is the review's own words and nothing after them. It
   used to end with a trailer naming skein and the commit it read; the owner's decision of
   2026-08-25 removed it — *"It should be as if I am writing it."* What the READER is told did not
   change and must not: the exact body is on the control's own tooltip, character for character, and
   the reading is printed above it. Knowing what you are sending is a different question from what
   the person receiving it reads, and only the first of those is skein's to answer.
3. **Nothing else moved.** The bare queue row still offers no verdict, `a` outside the reading view
   still refuses out loud, and the reading view's `revBarHtml` is still gated on a fetched diff. The
   exception is one block, reached by a press, on a surface that had to be opened to exist.

It rides `revPending` — the same hold, receipt and `u` as every other verdict (§7.1) — rather than
the critique panel's own hold, because it *is* a verdict: it must mark the row approved in place, be
undoable from the queue, and be replaced rather than doubled when a second verdict lands inside the
same eight seconds.

### 7.4 One control, and what it is allowed to throw away (SKEIN-293)

The reader asked *"when I click re read, does it give review as well? If so why is there separate re
read and review the code buttons?"* — and the honest answer was that on most rows the two were the
same button twice. Since summary and review became one reading, both pressed the same visit, forced
the same read, downloaded the diff once and spent one model call. They differed in one argument, on
one kind of row: where a review was already drafted at this head, "re-read" kept it and "review the
code" replaced it. Neither label said so, and the conservative one was the one whose name sounded
like it did everything. Worse, on that row they did not even buy the same reading — "re-read" fell
through to the cheap two-stage summary path, a different prompt on a weaker model, because the
draft check refused a head it had already drafted. (That check and the drafting it guarded are both
gone now; this paragraph is the case for one control, not a description of code.)

**There is one control.** *"Read it again"* reads the whole change and drafts a new review from that
reading, always, and its title says so. The owner chose this over renaming two controls or
explaining the difference on the row: two buttons was the "always triggered together" decision
half-applied at the surface after it had been fully applied underneath.

**What the one control may destroy, and when it asks — no longer either.** This is the half of the
decision the review cut removed rather than kept, and *why* it stopped applying is the point: the
confirmation existed to protect a drafted review the reader had edited, and skein stores no drafted
review. There is nothing left to throw away, so the press just goes, every time.

The line it drew is what to carry forward. **"Vetted" meant decisions, not attention** — opening a
panel is not vetting, keeping or dropping a comment is, and so is editing the text. An untouched
draft is exactly what skein produced, so reading again lost nothing of the reader's, and asking them
to confirm that is how a confirmation becomes noise and stops being read by the third row. Any
future surface that guards a destructive press owes the same distinction: asking about work the
person did not do is worse than not asking.

The intent travels from the surface to the server as `?redraft=1`, and the server's default stays
conservative — because the server cannot make this judgement: it does not know what the reader has
vetted.

---

### Focus rules — the part that has to be written down

1. **Selection is a PR number, never an index.** Given §2.3, an index-based selection would drift off
   the row you are looking at every time a summary lands. If the selected number leaves the list, the
   selection moves to the row that took its place *by position*, and the row flashes once.
2. **`renderReview` may never replace a subtree containing `document.activeElement`.** Today it
   replaces all of it and the caret dies (§2.1). Two acceptable implementations: diff the row list
   and patch, or defer the re-render while a composer is focused and coalesce. A one-line assertion
   an engineer can test: *type into the composer, fire `renderReview()`, `document.activeElement.id`
   must still be `rev-compose` and `selectionStart` unchanged.*
3. **Selection survives a refetch, a filter change and an act.** Scroll already survives
   (`scrollSurvivesRerender: {was: 800, now: 800}`); selection has nothing to survive with yet.
4. **Opening a row scrolls it into view** with its actions fully visible (§2.4) — `scrollIntoView`
   is not enough when the expansion is 543 px; the target is *the top of the expansion at 25% of the
   viewport*.
5. **Entering review collapses the fleet sidebar to its icon rail.** 380 px of a 1400 px window
   (27%) is currently showing an onboarding checklist while you work. `--fleet-w` is already
   drag-resized and persisted and the `@container` rules already degrade the rail gracefully at
   270 px — so this is a stored value, not a new layout. Restored on exit.

---

## 7. Flow and feedback — the whole loop

### 7.1 Approving

The current loop: press → `revPost` → toast bottom-right → `loadReview(true)` → 2–4 s of network →
the whole pane rebuilds → the row is still there, still open, still saying *you have not reviewed
this*. No undo, no focus move (§2.6).

Proposed:

```
   ┌ reading view ──────────────────────────────────────────────────────────────┐
   │ ← queue  #618  tenants slice 6: the context seam…  step 6 of 15 · ladder    │
   │                                    19h · 6 files · +154 −8  [ci: build ↗]   │
   ├──────────────┬─────────────────────────────────────────────────────────────┤
   │ YOURS        │  FOUND IN THE DIFF                                          │
   │  seam.rs +11 │   [default]  tenant resolved from session, not header        │
   │  mod.rs   +2 │   [contract] Denied gains NotAMember                         │
   │  session +18 │                                                             │
   │ NOT YOURS    │  @@ -41,10 +41,18 @@ impl Seam {                            │
   │  …seam.rs+96 │  -   let tenant = req.header("x-tenant")…                    │
   │  app.js   +3 │  +   let tenant = req.session().and_then(…)                  │
   │  tenants +24 │  ┌──────────────────────────────────────────────────┐        │
   │              │  │ membership() is cached per-request — refreshed   │        │
   │              │  │ after a suspend?                                 │        │
   │              │  │ src/tenants/seam.rs:46      [Cancel]  [Save]     │        │
   │              │  └──────────────────────────────────────────────────┘        │
   ├──────────────┴─────────────────────────────────────────────────────────────┤
   │ 2 line comments waiting — they post with your verdict                       │
   │              [set aside e] [request changes r] [ approve step 6  a ] [next →]│
   └────────────────────────────────────────────────────────────────────────────┘
```

Captured at `v7-proto-reading.png`, built entirely from components already in the page:
`renderDiff()` (`index.html:3355`), `.diff/.ln/.hunk/.add/.del`, `.ln.cmtable`, `openComposer()`,
`.cmt.composer`, `.revsignals`.

Press `a`:

1. **Immediately**, before any network: the verdict bar collapses to
   `✓ approved · undo (u) · 7s` and the button strip is replaced by it. Feedback is where the eye
   is — in the bar you just pressed — not in a toast 900 px away in the opposite corner.
2. In a stack, focus moves to **step 7** and the diff loads under you. Outside a stack, `Esc`-less
   return to the queue with selection on the next undecided row.
3. The queue row is marked `.done` (green node, struck title) **in place**. It does **not** vanish.
   Vanishing rows in a list you are keyboard-navigating destroy your place; the row leaves on the
   next load, by which time you are elsewhere.
4. `u` within 8 seconds cancels the request if it has not gone out, and posts a dismissal if it has.
5. **If it fails**, the bar turns to
   `✗ GitHub refused: <reason>  [try again] [open on GitHub ↗]` **and stays** — it does not toast and
   disappear. A verdict that silently did not land is the worst outcome this surface can produce.
   The queue row keeps the accent halo (still your move) rather than going green.

`e` (set aside) follows the same shape and is the one to get right, because it is the cheapest
gesture and therefore the one most often mis-aimed after a reflow: grey in place, `undo (u)`,
selection advances, gone on the next load.

### 7.2 Getting back

`Esc` from the reading view returns to the queue **at the same scroll offset with the same row
selected**, whether you decided anything or not. The reading view is a mode of `#revpane`
(`.revpane.reading`), not a dock tab and not a modal:

- a dock tab would be box-scoped, and this is repo-scoped — `openDiff(name)` → `showBox(name,
  "diff")` is wired to a box by construction (`index.html:2025`);
- a modal would put the queue behind a scrim, and the queue is the thing you are returning to.

The comment store `comments` (`index.html:3374`) is keyed by box; it gains a `repo#number` key for
PRs. `assembleReview` gets a sibling that posts `{kind, body, comments:[{path,line,body}]}` to
`/act`, which is the missing half the PM identified in §3.4 — the box path already produces
line-anchored comments and the PR path can only post one top-level body.

### 7.3 Where the money goes

The budget now counts cache misses on the server (`e310a24`). The design's contribution is deciding
*which* rows it lands on, and that is an ordering question, not a budget question:

- read the **top of the ordered queue** — after the wait-time sort, so the six reads land on the six
  rows you are about to open;
- read the **next actionable step of each stack** — not its tip. Today the tip is what floats to the
  top of an `updated_at` sort, and the tip is the one PR you cannot review yet;
- **opening the reading view is a revealed request**: it triggers a read if there is not one, and it
  does not come out of the unasked allowance. Somebody who opened a PR asked for it.
- the gist column makes the budget *auditable*: `not read` on 23 rows is the spend, visible, instead
  of 23 rows that look complete.

**And who is spending it, which took two bugs to state properly.** The rule, whole:

> If you pressed it, it is free and unconditional. If skein decided to read it, that happens only in
> a repo you switched read-ahead on for, and it is counted against the day.

There is no third case, and the pane is not one. The pane's own pump — the thing that fills in rows
you have not opened — is *skein's* initiative however present you are, so it obeys exactly the scope
the ten-minute background pass obeys and pays from the same ledger. What made this worth writing
down is that the pane used to be a third case by accident: `read_prs` and `worth_reading` lived in
the background reader alone, so an open pane read pull requests in repos the owner had switched
read-ahead **off** for, and pull requests whose only reason was that somebody mentioned them, and
charged the day for both — while the chip beside the queue said "nothing is read unless you ask"
(SKEIN-242). The mirror image was live at the same time: the server had read and drafted the pull
requests you *opened* since `41de066`, on one merged model call, but the pump still asked only in
the your-move lane, so your own stack filled in from the background tick and never from the pane in
front of you (SKEIN-277).

Both are one rule now, asked in one place. The scope is enforced at the model call
(`review::unasked_scope`) rather than in the caller, for the reason the budget moved there first: a
scope the client holds is a scope any client can widen. The page keeps a copy — `revReadsAhead` and
`revSkeinsToRead`, read by `revPumpSummaries` and by `revReadAgain` — but only so it does not *ask*
for what would be refused, and a copy that drifts can paint a refusal, never spend.

**And what one reading buys, which is both halves.** The summary and the review are different tasks
needing different mindsets, but they are never separate surfaces and they are always wanted
together — so they are never two analyses (the owner, 2026-08-25). One model call over one
download produces both and stores both, and that includes the panel's "draft again", which re-runs
the reading rather than drafting beside the summary already on the row. The standalone drafter that
used to serve that button is gone (SKEIN-263): it had its own prompt and its own download, and what
it produced had no reason to agree with the summary sitting above it about what the commit even
contained. The pane follows: after a draft lands, the row's reading is re-read off disk, because
the one that is on screen is now the older answer.

The consequence for §8.6's sentence: a row nobody is going to read on its own must say **which** of
the two rules left it unread, because they have different answers — one is a switch on this repo,
the other is what skein reads at all. "Nothing has asked for this one yet" was the whole of what it
said, on rows that would never be read however long anyone waited.

---

## 8. The states

Each of these is a designed artifact with a job, not a fallback.

### 8.1 Cold load — skeleton, not a sentence

Today: `asking GitHub…` as bare text in the top-left of a 1400 px page (`s1-loading.png`), then 29
rows arrive at once and everything moves.

```
│ [all —][gadget-demo 29][lattice 10]…                    ← the repo bar paints from cached counts
│ ▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓  ▓▓▓▓▓▓▓▓▓▓▓▓▓        ▓▓▓  ▓▓▓▓▓  ▓▓▓▓
│ ▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓  ▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓       ▓▓▓  ▓▓▓▓▓  ▓▓▓▓   × 12, at 29px
│ ▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓  ▓▓▓▓▓▓▓▓▓        ▓▓▓  ▓▓▓▓▓  ▓▓▓▓
```

Twelve skeleton rows at the real 29 px, in the real grid, so the arrival costs **zero layout shift**.
The repo bar is populated from the last known counts immediately — so the one thing you might want
during the wait (going somewhere else) is available during the wait.

### 8.2 Stale-while-refreshing

The server already answers `fresh: false` with a remembered queue and the head already renders
`read 4m ago · refreshing`. The design keeps it and moves it: it belongs *above the rows it
describes*, as a rule, not in a header strip beside `refresh`.

```
▏these rows are 11 minutes old · CI state and new commits are not in them        ⟳
```

Naming *what is stale about them* is the point — the titles have not changed; the check states and
the "moved" marks have.

### 8.3 Partial failure — one repo of nine

The case that does not exist today because the pane is one repo wide. It is the common case in a
nine-repo fleet.

```
│ [all 39][gadget-demo 29][lattice 10][beacon !][skein 0]…
│▏beacon could not be read — 403 from GitHub. The other 8 are current. [retry] [why?]
│  your move — 7 decisions   from 29 pull requests in 8 repos, oldest first
```

The failed repo's chip goes `!` in `--error`; the merged list carries the other eight and **says so
in the heading** (`in 8 repos`). A cross-repo queue that silently under-reports one repo is the exact
failure the blind-spot machinery exists to prevent, one level up.

### 8.4 Hard failure (`v9-proto-error.png`)

Today: the orange box replaces the entire queue, including 29 rows that were on screen a second ago
and are still on disk in `prq::remember`.

```
┌ the queue could not be built ─────────────────────────────────────────────────┐
│ gadget-demo could not be refreshed — showing the copy from 11 minutes ago    │
│ GitHub said: 401 Bad credentials. The token skein reads the queue with no      │
│ longer names a user.                                                           │
│ [ try again ] [ Settings → GitHub & keys ] [ the other 8 repos are fine — show ]│
└───────────────────────────────────────────────────────────────────────────────┘
▏these rows are 11 minutes old · CI state and new commits are not in them
  your move — 7 decisions   as of 11 minutes ago
  ●  #583  refactor(store): one writer for the revision table …          ← at 55% opacity
```

Three changes. **The remembered queue stays**, at 55% — an old queue is worth vastly more than an
empty one and skein already has it. **The prose keeps its title** but gains the three affordances the
PM found missing. **The 55% is load-bearing**: it is how you tell at a glance that what you are
looking at is not live, without reading anything.

Built (SKEIN-154): `loadReview`'s catch keeps the copy in `revSeen` and marks it `remembered`
rather than replacing it — and does **not** write it back, because a failure is not an answer. The
box carries `try again` and `Settings → GitHub & keys`; the third affordance is not needed here,
because one repo failing no longer reaches this path at all (§8.3 — it is a `.revfail` line above
rows the other repos still fill). `.revwrap.notlive .revlane` is the 55%.

### 8.5 Empty (`v8-proto-empty.png`)

Today: `NEEDS YOU 0 / nothing here.` in the top-left corner and 800 px of nothing, while the badge in
the same window reads 39.

```
   gadget-demo is clear.
   Nothing here is waiting on you. You cleared 6 today; the last one landed 14 minutes ago.

   ┌ 10 · lattice — oldest has waited 3.1d                                   ┐
   ├  1 · beacon — skein could not read this queue: 403 from GitHub           ┤
   └  5 · set aside in this repo — bring one back                            ┘
```

A cleared queue is the best moment this product has and it should feel like one — a headline in
grotesk, a sentence of evidence, and then **the honest next thing**, which is that another repo has
ten. The failed repo is listed here too: "empty" and "not looked at" must never be the same screen.

Built (SKEIN-154): `revClearHtml`, reached from `revLaneEmpty` only for the your-move lane and only
when nothing has narrowed it — a search that matches nothing says nothing about whether anything
needs you, so that keeps the plain line. Every number comes off the merged queue already on screen,
so the calm screen costs no request, and the set-aside count rides in the sentence.

**The headline is a claim, and skein only makes it about queues it read** (SKEIN-245). `revUnasked`
decides that, in one place, for the SCOPE the headline speaks about:

* a repo whose review queue is switched off, or that has no GitHub remote, arrives in
  `MergedQueue::skipped` — reported rather than omitted, so that "never looked" and "nothing
  waiting" are not the same silence. It reads **`skein did not ask about acme.`**, with the reason
  and `Settings → Repos`, never "acme is clear.";
* a repo whose queue could not be built reads **`skein could not read acme.`** — what is in it is
  unknown, not nothing — with `try again`;
* with no repo chosen the claim is about the whole fleet, and **one queue that was read earns it**.
  Only when *no* queue came back does it become `skein has not read any queue.` That asymmetry is
  deliberate: a partial failure keeps the calm screen, because the repos that answered are a real
  reading and the ones that did not are rows on it.

The rows follow the same rule as the failed repo above them: an unasked repo is listed with a **`—`**
where a count would be, not a `0`. The repo picker does the same, for the same reason — `acme · 0`
was byte-for-byte a repo with a clean queue, and that is where the confusion started.

### 8.6 "skein could not read this one"

`not read — <reason>` in the gist column, and — the rule that matters — **the reading view still
opens**. The diff needs no model. An unread summary must never gate reading the code; today an
unsummarised PR expands to a dashed box explaining the rate limit, next to an `approve` button, with
no way to see the change at all.

### 8.7 "skein could not DRAW this one" — an exception is a row's problem

Reported by the owner: *"any small error anywhere in the review page just blanks the entire page and
gives the error."* The pane is built as one string and assigned in one shot, so a throw in any of
the thirty-odd helpers that string calls meant `revpane.innerHTML = …` was never reached — blank if
it threw before the first paint, frozen at the last good paint if later, and the reason in a
devtools console nobody had open. Three layers, cheapest first (SKEIN-268):

1. **`revRowSafe`** wraps each row's `make()`. A row that throws becomes a row *saying so* — which
   pull request, and why — with `open on GitHub ↗` as the way out, while the rest of the queue
   draws. This is the layer that matters: everything touching model output, a diff, a draft or a
   workflow is per-row, and that is where malformed data arrives. The broken row **keeps its
   `data-rk`**, so it stays in `revNav` — dropping it would silently shorten `j`/`k` for as long as
   the fault lasted, a second failure hiding behind the first.
2. **`renderReview` guards the whole paint** and `revRenderFailed` keeps the last good HTML rather
   than blanking, prepending a red strip that names the reason in the page, selectable, with `read
   it again`. A surface that blanks has also thrown away the way out.
3. **`window.onerror` / `unhandledrejection`** land in the cockpit's own toast, de-duplicated to
   once a minute, so a fault arrives with a sentence attached instead of a description. It is also
   the backstop for layer 2: a throw inside `revRenderFailed` still leaves the last good queue on
   screen and still says something.

---

## 9. What I reject from the PM's brief, and why

**"The existing `<select>` becomes a filter chip alongside `all / mine / to review / mentioned`."**
Rejected. Repo and audience are orthogonal axes and a single chip group can only express one. Merged,
either picking a repo deselects `to review`, or the group holds two selections and stops reading as a
radio group — the most common chip-strip bug there is. Two strips: a **repo bar with counts on the
left**, an **audience group on the right** (`v5-proto-fixed.png`). And the counts are the whole point
of the repo strip — the number is currently a `title` attribute, and a chip inside a mixed group
cannot carry one legibly.

**"Demote the check dot to a state that is worth a colour."** Rejected as insufficient. Demoting
leaves it in the leftmost, pre-title position — the highest-value pixel on the row — where it is red
25 times out of 29. The scarce resource is the *scan position*, not the colour. The dot is removed
and the position is given to whose-move; the check state becomes a repo-level line plus a named chip
in the one case that earns it.

**"Move `approve` behind the diff rather than in front of it."** Right instinct, wrong mechanism.
"Behind" is a claim about ordering inside the expanded row, which leaves a verdict button reachable
from a surface that shows no evidence — one `Tab`+`Space` away. Stronger: **there are no verdict
buttons in the queue at all.** The row's only acts are *open* and *set aside*; verdicts live in the
reading view, and `a` in the queue explains why it did nothing. That makes the rule structural rather
than positional, and it is checkable in a test.

**"Render a chain as a single row, expanding to its members."** Accepted, with two conditions the
proposal does not carry. (1) **Expansion must be exclusive** — 15 steps is 543 px in an 850 px
viewport, so a second open row makes the queue unnavigable. (2) **The step list must contradict the
titles.** Re-sorting silently is not enough: `#624` says "slice 5" and belongs after "slice 6", and a
view that quietly reorders without saying so will read as broken to a reviewer who trusts the
numbers. Silent correction is worse than no correction here, because it makes skein look wrong at the
exact moment it is right.

**"29 rows become 15; 29 decisions become 15."** The first half is right and I have measured it (29
rows → 15, all above the fold). The second overstates: a stack is one *entry point* and fifteen
decisions in a forced order. The heading says both — `7 decisions from 29 pull requests` — which also
happens to reconcile the badge with the pane.

**`o` for "open on GitHub".** Rejected: `o` already means `open` in `FLEET`
(`cockpit.js:390`), meaning "open the thing, here". `g h` for GitHub.

**Merge on the keyboard.** Not proposed by the PM, but worth stating as a boundary: unbound, chip
only, confirm retained.

---

## 10. The visual rules, stated so they can be checked

**The emphasis ladder.** Every element on a row has a rank, and *no element may exceed the contrast
of the rank above it*. This is the rule the current row breaks — the `reviewer` chip at 9.32 : 1
outranks the `#number` at 4.83 : 1 and the lane heading at 5.35 : 1.

| rank | element | target |
|---|---|---|
| 1 | title | ≥ 14 : 1 |
| 2 | gist | 9–10 : 1 |
| 3 | rail: age, size, author | 5–6 : 1 |
| 4 | `#number`, chips | 4.5–5.5 : 1 |

Chips move **down** to rank 4: background-filled at 14–16% of their hue rather than
outlined-and-bright, which keeps them legible while stopping them competing (`.revtag` in §10.1).

**Colour is a vocabulary of five words, and each says one thing.**

| token | means | never |
|---|---|---|
| `--accent` periwinkle | **your move · your selection · your position** | decoration |
| `--done` green | you decided and it holds | "passing CI" |
| `--waiting` amber | true, you should know, not an error — an old row's age, a standing blind spot, an out-of-order step | failure |
| `--attn` red | **your problem right now** — failing *and* yours to fix; the one repo-level CI line | 25 rows of wallpaper |
| `--error` orange | **skein failed** — distinct from GitHub's red and from amber | a permanent condition |
| `--working` blue | movement since you last looked (`moved`) | |

The separation of `--error` from `--waiting` is the fix for the largest object on the screen: today a
permanent `read:org` gap and a hard 401 wear the same orange box, and drawing the permanent one as an
alarm is what makes the alarm stop working.

**Density.** 29 px rows, 5-column grid, one line. Target: 30 rows above the fold at 1400×900, because
that is a day. Measured: 15 rows / 884 px / all visible.

**Degradation** (the pane is a `container`, so these key off its own width, not the viewport — same
technique as the fleet rail at `index.html:49`):

| pane width | change |
|---|---|
| ≥ 1180 | five columns as drawn |
| 1000–1180 | drop `size` from the rail |
| 820–1000 | drop `who`; keep age |
| < 820 | gist drops to a second line (44 px rows), rail is age only |

Entering review collapses the fleet sidebar, which is what buys the ≥1180 case at a 1400 px window.

**Motion.** One rule: **nothing that arrives late may change a row's height.** The gist column, the
fixed rail widths and the same-height `reading it…` all exist to serve it. The only permitted motion
is the `reading it…` ellipsis and a single 200 ms flash when a selected row's number is displaced.

### 10.1 The CSS worth keeping

Written in the page's existing tokens; no new variables, no new type families.

```css
/* THE ROW — grid, not flex. Flex is what let the title take what it wanted and
   pushed the gist to a second line: 6 rows at 62px among 23 at 36px. */
.revrow  { margin:0; border:0; border-radius:0; background:transparent;
           border-bottom:1px solid var(--line); }
.revrow:first-child { border-top:1px solid var(--line); }
.revline { display:grid; grid-template-columns:14px 46px minmax(0,50fr) minmax(0,32fr) auto;
           align-items:center; gap:0 10px; padding:4px 10px; min-height:28px; }
.revrow.sel { background:var(--elevated); box-shadow:inset 2px 0 0 var(--accent); }
.revrow.sel .revtitle { color:#fff; }
.revline:hover { background:color-mix(in srgb,var(--elevated) 60%,transparent); }

/* WHOSE MOVE — replaces the check dot in the row's highest-value position.
   Exactly one of the four states is lit, and it is the one that is yours. */
.mv         { width:9px; height:9px; border-radius:50%; flex:none; }
.mv.yours   { background:var(--accent); box-shadow:0 0 0 3px var(--accent-bg); }
.mv.theirs  { background:transparent; border:1px solid var(--border-3); }
.mv.blocked { background:transparent; border:1px solid var(--border-3);
              border-radius:2px; transform:rotate(45deg); width:7px; height:7px; }
.mv.done    { background:var(--done); opacity:.55; }

.revnum   { font:11px var(--mono); color:var(--faint); text-align:right; }
.revtitle { font-size:13px; color:var(--text);
            overflow:hidden; text-overflow:ellipsis; white-space:nowrap; }

/* THE GIST COLUMN — never empty, and never changes the row's height. Both
   properties are the point: the first is the module's own invariant, the second
   is what stops 28 rows shifting 26px when a summary lands. */
.gist         { font-size:12px; color:var(--text-2);
                overflow:hidden; text-overflow:ellipsis; white-space:nowrap; }
.gist.unknown { color:var(--faint); font-style:italic;
                text-decoration:underline dotted color-mix(in srgb,var(--faint) 55%,transparent);
                text-underline-offset:3px; }
.gist.reading { color:var(--faint); font-style:italic; }

/* THE RAIL — age first, because it is the sort key, and a sort key you cannot
   see is a sort you cannot trust. */
.rail          { display:grid; grid-template-columns:34px 62px 78px; gap:0 10px;
                 font:11px var(--mono); color:var(--dim); align-items:center; }
.rail .age     { text-align:right; font-variant-numeric:tabular-nums; }
.rail .age.old { color:var(--waiting); }
.rail .size    { text-align:right; font-variant-numeric:tabular-nums; color:var(--faint); }
.rail .who     { overflow:hidden; text-overflow:ellipsis; white-space:nowrap; }

/* CHIPS drop to rank 4 of the ladder: filled at 14-16%, not outlined and bright.
   `reviewer` is gone — 22 of 29, and the filter above already selects on it. */
.revtag       { font:10px var(--mono); border:0; border-radius:3px; padding:0 5px; flex:none;
                background:var(--elevated); color:var(--dim); }
.revtag.draft { background:color-mix(in srgb,var(--waiting) 16%,transparent); color:var(--waiting); }
.revtag.moved { background:color-mix(in srgb,var(--working) 16%,transparent); color:var(--working); }
.revtag.flag  { background:var(--accent-bg); color:var(--accent-2); }
.revtag.fail  { background:color-mix(in srgb,var(--attn) 14%,transparent); color:#ff9d97; }

/* THE STACK — the rail is drawn, so "fifteen deep" is a length you see. */
.revrow.stack > .revline { background:color-mix(in srgb,var(--accent) 5%,transparent); }
.stackmark { font:10px var(--mono); color:var(--accent-2); background:var(--accent-bg);
             border-radius:3px; padding:0 5px; margin-right:7px; }
.steps     { padding:2px 0 8px; background:var(--inset); border-top:1px solid var(--line); }
.step      { display:grid; grid-template-columns:40px 20px 44px minmax(0,1fr) auto;
             align-items:center; gap:0 9px; padding:3px 10px; min-height:26px; font-size:12.5px; }
.step .sn  { font:10px var(--mono); color:var(--faint); text-align:right; white-space:nowrap; }
.step .node { position:relative; height:26px; }
.step .node::before { content:""; position:absolute; left:9px; top:0; bottom:0;
                      width:1px; background:var(--border-2); }
.step:first-child .node::before { top:13px; }
.step:last-child  .node::before { bottom:13px; }
.step .node::after  { content:""; position:absolute; left:6px; top:10px; width:7px; height:7px;
                      border-radius:50%; background:var(--inset); border:1px solid var(--border-3); }
.step.done .node::after { background:var(--done); border-color:var(--done); }
.step.next .node::after { background:var(--accent); border-color:var(--accent);
                          box-shadow:0 0 0 3px var(--accent-bg); }
.step.done .st { color:var(--faint); text-decoration:line-through;
                 text-decoration-color:var(--border-3); }
.step .st      { overflow:hidden; text-overflow:ellipsis; white-space:nowrap; color:var(--text-2); }
.step.next .st { color:var(--text); }
/* the step whose own title numbers it out of order — say so, do not silently re-sort */
.step .misnamed { font:10px var(--mono); color:var(--waiting); white-space:nowrap;
                  background:color-mix(in srgb,var(--waiting) 14%,transparent);
                  border-radius:3px; padding:0 5px; }
.stackhint { font:11px var(--mono); color:var(--faint); padding:4px 10px 6px 44px; }

/* A STANDING CONDITION IS NOT A FAILURE. Today both are an orange bordered box
   with a bold title, and drawing the permanent one as an alarm is what makes the
   alarm stop working. */
/* Built (SKEIN-164), and quieter than drafted here: the standing condition kept
   the amber left rule and lost the surrounding border as well as the box — a
   rule and a word is the whole treatment, and the word is a label rather than a
   title. `.revfail` below is unchanged and is now the only thing wearing it. */
.revblind   { border-left:2px solid var(--waiting); padding:2px 0 2px 9px;
              margin-bottom:12px; font:12px var(--mono); color:var(--text-2); }
.revblind .revblindwhat { color:var(--waiting); }
.revfail    { border:1px solid var(--error); border-left-width:3px; border-radius:var(--radius-sm);
              background:rgba(240,136,62,.08); padding:11px 13px; margin-bottom:14px;
              font:12.5px var(--mono); color:var(--text-2); }
.revfail b  { color:var(--error); display:block; margin-bottom:5px; font-size:13px; }
.revfail .fixes { display:flex; gap:6px; margin-top:9px; }
.stalebar   { display:flex; align-items:center; gap:8px; font:11px var(--mono); color:var(--waiting);
              background:color-mix(in srgb,var(--waiting) 8%,transparent);
              border-left:2px solid var(--waiting); padding:4px 10px; margin-bottom:10px; }
.revrow.stale-body { opacity:.55; }

/* THE REPO BAR — nine repos with their counts, replacing a <select> whose
   contents were a tooltip. */
.repobar         { display:flex; gap:4px; align-items:center; overflow-x:auto;
                   scrollbar-width:none; padding-bottom:2px; }
.repobar .rb     { display:flex; align-items:center; gap:6px; height:24px; padding:3px 9px;
                   border:1px solid var(--border-2); border-radius:999px; background:var(--panel);
                   color:var(--dim); font:11px var(--mono); cursor:pointer; white-space:nowrap; }
.repobar .rb.on  { border-color:var(--accent); background:var(--accent-bg); color:var(--accent-2); }
.repobar .rb .n  { color:var(--text); font-variant-numeric:tabular-nums; }
.repobar .rb.zero{ opacity:.45; }
.repobar .rb.bad { border-color:var(--error); color:var(--error); }

/* The lane heading was the faintest thing on the page and it is the only
   structural claim on it. It becomes a sentence about the day. */
.revlane > h4      { font:11px var(--mono); text-transform:none; letter-spacing:0;
                     color:var(--text-2); margin:0 0 6px; }
.revlane > h4 em   { font-style:normal; color:var(--faint); }
/* 25 red rows is one broken pipeline. Said once, here. */
.cinote   { font:11px var(--mono); color:var(--dim); margin:0 0 10px; padding:4px 10px;
            border-left:2px solid color-mix(in srgb,var(--attn) 50%,transparent);
            background:color-mix(in srgb,var(--attn) 5%,transparent); }
.cinote b { color:#ff9d97; font-weight:500; }

/* THE READING VIEW — a mode of #revpane, reusing .diff/.ln/.cmt verbatim. */
.readwrap  { position:absolute; inset:0; display:flex; flex-direction:column; background:var(--inset); }
.readhead  { display:flex; align-items:center; gap:10px; padding:8px 14px;
             border-bottom:1px solid var(--line); font:11.5px var(--mono); color:var(--dim); }
.readhead .t { color:var(--text); font:13px var(--ui); flex:1;
               overflow:hidden; text-overflow:ellipsis; white-space:nowrap; }
.readcols  { flex:1; min-height:0; display:grid; grid-template-columns:210px minmax(0,1fr); }
.readfiles { border-right:1px solid var(--line); overflow-y:auto; padding:8px 0; }
.readfiles .f      { display:flex; gap:8px; align-items:baseline; padding:3px 12px;
                     font:11px var(--mono); color:var(--text-2); cursor:pointer; }
.readfiles .f:hover, .readfiles .f.on { background:var(--elevated); }
/* rtl truncation keeps the FILENAME, which is the part you are looking for */
.readfiles .f .p   { flex:1; overflow:hidden; text-overflow:ellipsis; white-space:nowrap;
                     direction:rtl; text-align:left; }
.readfiles .f .d   { color:var(--faint); font-variant-numeric:tabular-nums; }
.readfiles .f.mine { box-shadow:inset 2px 0 0 var(--accent); }
.readfiles h5      { margin:10px 12px 4px; font:10px var(--mono); font-weight:500;
                     text-transform:uppercase; letter-spacing:.06em; color:var(--faint); }
.readdiff  { overflow-y:auto; }
.verdict   { display:flex; align-items:center; gap:8px; padding:8px 14px;
             border-top:1px solid var(--line); background:var(--panel); }
.verdict .pend       { font:11px var(--mono); color:var(--accent-2); margin-right:auto; }
.verdict .revchip    { height:24px; padding:3px 12px; }
.verdict .revchip.go { background:var(--accent); border-color:var(--accent); color:#fff; }

/* Empty is the best moment this product has. */
.revzero        { padding:28px 0 0; max-width:620px; }
.revzero h3     { font:16px var(--ui); font-weight:600; color:var(--text); margin:0 0 6px; }
.revzero p      { font-size:13px; color:var(--dim); margin:0 0 14px; line-height:1.6; }
.revzero .next  { display:flex; flex-direction:column; gap:4px; }
.revzero .next a{ display:flex; gap:10px; align-items:baseline; padding:6px 10px;
                  border:1px solid var(--line); border-radius:var(--radius-sm);
                  background:var(--panel); font:12px var(--mono); color:var(--text-2); }
.revzero .next a .n { color:var(--accent-2); font-variant-numeric:tabular-nums; }

/* The key sheet, present rather than discoverable. */
.revkeys     { position:sticky; bottom:0; margin:12px -18px -40px; padding:6px 18px;
               background:var(--inset); border-top:1px solid var(--line);
               font:10.5px var(--mono); color:var(--faint); display:flex; gap:14px; }
.revkeys kbd { font:inherit; color:var(--text-2); background:var(--elevated);
               border:1px solid var(--border-2); border-radius:3px; padding:0 4px; }

/* Head controls were 21px tall. */
.revhead .revchip { height:24px; padding:4px 10px; white-space:nowrap; flex:none; }
```

### 10.2 Stack detection, in full

Nineteen lines, no network, no model. Prototyped in the browser against the real payload; it belongs
in `prq.rs` beside the lane derivation.

```js
function chains(prs) {
  const byHead = new Map(prs.map(p => [p.head_ref, p]));
  const child  = new Map();                    // head_ref -> the PR based on it
  for (const p of prs) if (byHead.has(p.base_ref)) child.set(p.base_ref, p);
  const roots = prs.filter(p => !byHead.has(p.base_ref) && child.has(p.head_ref));
  const out = [];
  for (const r of roots) {
    const steps = [r];
    let cur = r;
    while (child.has(cur.head_ref)) { cur = child.get(cur.head_ref); steps.push(cur); }
    if (steps.length > 1) out.push(steps);
  }
  return out;
}
// The chain's name: the longest branch-name prefix its members share.
// The step whose own title numbers it out of order — the reason reverse order is
// dangerous is that the TITLES carry a competing order, so the view contradicts them.
function misnamed(steps) {
  const num = s => { const m = /\bslice\s+(\d+)/i.exec(s.title) || /-(\d\d)-/.exec(s.head_ref);
                     return m ? +m[1] : null; };
  const out = new Map(); let prev = null;
  for (const s of steps) {
    const n = num(s);
    if (n != null && prev != null && n < prev) out.set(s.number, `named ${n}, sits after ${prev}`);
    if (n != null) prev = n;
  }
  return out;
}
```

Against the rig this returns one chain of 15 and one misnaming — `#624 "named 05, sits after 06"` —
which is the finding the PM extracted by hand from fifteen clicks and a pencil.

---

## 11. What to build, in order

Disjoint from the PM's `SKEIN-146…154`, which own the data and behaviour; these own the surface.

1. **The row grid, the gist column and the move mark.** Highest ratio of scan improvement to work,
   and it is the prerequisite for everything else because it removes the reflow. Depends on nothing:
   it renders from the payload that exists today.
2. **The keyboard, with focus rules.** Cheap, and it is the difference between a page and a tool at
   thirty a day. Selection-by-number must land with it, not after it.
3. **The stack row, its rail, and its traversal.** The single highest-value piece: it is the only one
   that prevents a *wrong* review rather than a slow one.
4. **The reading view.** Reuses `renderDiff`, `.ln.cmtable` and `openComposer`; needs the diff on the
   wire and `/act` to carry line comments (`SKEIN-149`).
5. **The states**: skeleton, stale bar, partial failure, keep-the-remembered-queue-on-error, and the
   empty screen that names the other repos.
6. **Collapse the fleet sidebar on entering review.** One stored value.

Items 1, 2, 5 and 6 need no server change at all.
