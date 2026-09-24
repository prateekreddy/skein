# Fixtures for the cockpit suites

## `thing-queue.json`

The **shape** of a real queue, with synthetic names on it.

It began as `GET /api/repos/gadget-demo/review` on the owner's fleet, 2026-08-25 — 54 open pull
requests, trunk `develop` — trimmed even then to the four fields the stack code reads: `number`,
`head_ref`, `base_ref`, `lane`. Titles and author logins were never carried; no assertion reads
them, and a fixture in this repository is not the place for another project's work.

The branch names were. They are the part of a queue that says what a team is building, and about
forty of these said it in the vocabulary of somebody else's business — its documents, its filings,
its customers' records — so those are replaced with synthetic names of the same shape, keeping the
prefix (`fix/`, `feat/`, `worktree-`, the numbered ladder) and, where a document draws an aligned
table around one, the character width. The rest are ordinary software English and are left alone,
because a name that identifies nobody is worth more standing than replaced.

What was NOT touched is the graph: the same 54 pull requests, the same numbers, the same
`head_ref → base_ref` edges, the same lanes (26 `waiting`, 25 `needs-you`, 3 `not-ready`), the same
trunk, and the same two bases that are in nobody's queue. Checked, not asserted: rebuild the edge
set from the numbers alone — `{(number, number-of-whatever-opened-its-base)}` — before and after,
and the two sets are equal. That set is the whole of the evidence.

It is the evidence for SKEIN-288: a real branch graph with a real trunk pull request (#625,
`develop → master`) AND two real forks (`fix/readiness-abstention-kinds` carries #586 and #671;
`fix/readiness-named-findings` carries #711 and #672). No hand-made pair of pull requests contains
both at once, which is why the bug survived a suite full of them. It is also the evidence for
SKEIN-302, whose claim is about ORDER across four stacks (26, 18, 3 and 2 steps) and five loose
rows — 49 of the 54 would interleave under a plain per-pull-request age sort, and a hand-made pair
proves nothing about that.

**The numbers are kept, and that is a decision rather than an oversight.** A bare integer names
nothing once the slug and every branch are synthetic, and the numbers are how the assertions and the
comment that explains the bug (`src/web/index.html`'s `revChains` comment; a UX review since moved
out of the repository did too) refer to individual rows. Renaming them would cost every one of those sentences its
subject and buy nothing.

**One suite reads this file**: `tests/ui/review_return.mjs`, in its SKEIN-288 and SKEIN-302
sections. Nothing else opens it — `grep -rn thing-queue` outside this directory returns exactly
that one line — so a change here can only break that suite, and running it is the whole check.
