# Skein — vision

> A *skein* is the V-formation geese fly in. Many birds, one direction, almost no
> energy wasted. That's the goal: many agents in flight, and a single calm surface
> from which you see and steer them.

## The one question

Tools like Conductor are really built around a single moment: **"which of my agents
needs me right now — and let me act on it without breaking flow."** Everything else
(diffs, merges, terminals) hangs off that. Skein optimizes for that moment, on top of
the sandbox fleet you already run.

## Why build instead of adopt

The off-the-shelf tools (Conductor, Crystal, Vibe Kanban, uzi, Claude Squad) are almost
all **git-worktree-on-the-host**. Two things you already have are *better* and **not
available in any of them**:

1. **MicroVM isolation per agent** (sbx `--clone`) — a real boundary, not just a branch.
2. **A live shared brain across boxes** — shared `.claude` memory + a cross-box mailbox.
   The research was blunt: this is *"notably rare."* It's the thing that makes a fleet
   feel like a team instead of N strangers.

So Skein is not "another Conductor." It's the **thin control surface** that the
sbx + shared-store backend was missing — and it leans into the shared-brain advantage
the GUI tools can't match.

## Principles

1. **Compose, don't reinvent.** Skein writes only the glue that doesn't exist yet
   (sbx awareness). Everything with a good existing wheel is reused: **sbx** (isolation),
   **honker** (event bus + job queue), **ratatui + crossterm** (TUI), **gh** (PRs), and the
   **existing registry / mailbox / shared store**. If a feature already lives in one of
   those, Skein calls it — it doesn't grow its own copy. See `ARCHITECTURE.md`.
2. **The shared store is the source of truth.** Skein *reads and reacts to*
   `sandboxes.json`, `mailbox/`, and the git branches the boxes push. It doesn't own
   state the bootstrap already owns.
3. **One screen, one keystroke.** The default view answers "who needs me?" and every
   action (attach, diff, merge, archive, broadcast) is one key away. Speed is a feature.
4. **Degrade gracefully.** No sbx? Show the registry read-only. No honker yet? Poll the
   files. Nothing should hard-fail because an optional dependency is missing.
5. **Dogfood first, share later.** Built for one creative dev to drastically improve
   their own loop; hardened into a team tool only once it's earned its keep.

## The feel

Open `skein` in your browser. A quiet formation of boxes, live ones on top, the one
waiting on you glowing. Click in to **talk to that agent right there** (its terminal is
embedded), review its diff, merge and archive — all on the page. The fleet's shared memory
means every box already knows the house rules. You never go hunting for "which terminal
was that again."

**Single pane of glass.** The goal is that the web cockpit is the *only* surface you touch
day-to-day: fleet overview **and** the per-box agent conversation both live here. The
terminal is for one-time host setup (`sbx`/Docker) and rare deep debugging — not the loop.
