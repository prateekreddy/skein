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

1. **A real isolation boundary per agent, not just a branch.** One sandbox holds the whole
   fleet and every box is its own mount and user namespace inside it, so a box sees its own
   checkout, its own conversation and its own credentials, and nothing of anybody else's.
   A worktree on your laptop gives you none of that.
2. **A live shared brain across boxes** — shared `.claude` memory + a cross-box mailbox.
   The research was blunt: this is *"notably rare."* It's the thing that makes a fleet
   feel like a team instead of N strangers.

So Skein is not "another Conductor." It's the **thin control surface** that the
sbx + shared-store backend was missing — and it leans into the shared-brain advantage
the GUI tools can't match.

## Principles

1. **Compose, don't reinvent.** Skein writes only the glue that doesn't exist yet. Everything
   with a good existing wheel is reused: **sbx** (the sandbox), **bwrap** (the per-box
   namespace), **tmux** (the box's session, and the thing skein addresses a box by), **git**,
   **GitHub's own API** over HTTP, and the **browser** for the surface. If a feature already
   lives in one of those, Skein calls it — it doesn't grow its own copy. See
   `ARCHITECTURE.md` and the four documents it points at.
2. **The volume is the state.** Everything durable lives under one relocatable root, and each
   kind of state is meant to have one writer — a goal skein is measured against, not a claim
   it has arrived. The half that is already a rule: **what a box writes, skein reads as input
   — never as a decision.** An approval is acted on from the bytes a person was shown, not
   from the file the box can still rewrite. The per-repo shared store is the boxes' own:
   memory, skills and the cross-box mailbox.
3. **One screen, one keystroke.** The default view answers "who needs me?" and every
   action (attach, diff, merge, archive, broadcast) is one key away. Speed is a feature.
4. **Degrade gracefully.** A missing optional dependency narrows what skein offers; it never
   hard-fails the surface. `skein doctor` and the cockpit's first-run checklist say what is
   missing and what it costs, each carrying the action that resolves it. The one deliberate
   exception is a privileged act with no approver — that refuses, and prints the line to run
   by hand, because a silent fallback would be taken on exactly the day something was wrong.
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
