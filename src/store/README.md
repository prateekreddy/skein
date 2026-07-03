# skein shared store

This `.claude` folder is **one project's shared data**. Every sandbox skein launches
for this repo mounts it live, so memory and the mailbox are shared across the
project's parallel boxes — and stay scoped to *this* project (see "Talking across
projects" below for the one exception).

**You don't have to put anything here for it to work.** skein installs and maintains
all the machinery itself (under `skein/`): the turn-state probe, a SessionStart
bootstrap that bridges memory and surfaces mailbox hand-offs, the mailbox, and a
default status line — all wired into `settings.json` automatically. An empty folder
comes up fully working.

**Mailbox delivery is turn-boundary, not just SessionStart.** `mailbox.sh inbox` runs
on every `UserPromptSubmit` (surfaces unread mail as context at the start of a turn)
and `mailbox.sh stop-check` runs on every `Stop` (blocks the stop with an error if mail
arrived mid-turn, so it can't be missed just because nobody asked). A message is
marked seen the instant it's delivered, so it never fires twice. `mailbox.sh send`
takes `--body-file <path|->` (a file, or stdin with `-`) in addition to `--body TEXT` —
prefer it for anything with backticks/`$`/quotes, since a shell-interpolated `--body`
argument is mangled before the script ever sees it.

**Talking across projects.** A box normally only ever sees its own project's mailbox
(separate microVM, separate mount). To reach a box in a *different* managed project,
address a message to `all-projects` (broadcasts to every project skein manages) or
`project:<repo-id>` (exactly one other named project) — skein's host process (or the
plain `skein` CLI, best-effort) relays these across store boundaries on a short
interval, rewriting the copy to that project's own `broadcast` so its boxes' normal
local delivery picks it up unchanged. This only runs while something host-side
(`skein-server`, or a `skein ls` invocation) is actually running.

What you *optionally* add is your own project content:

    memory/        project memory. The bootstrap bridges ~/.claude/projects/<cwd>/memory
                   → here, so it's live and shared across this project's boxes. Starts
                   empty and accumulates as boxes work; drop in notes to seed it.
    skills/        project skills (one dir per skill).
    hooks/         your own hook scripts, if you reference them from settings.json.
    shared-paths.txt   one repo-relative path per line of GITIGNORED files/dirs the box
                   needs (CLAUDE.md, .env, local config, …). A --clone carries only
                   tracked files, so the bootstrap symlinks each of these from the
                   read-only host mirror into the box. Without this, CLAUDE.md and
                   .env are absent in the box. Lines starting with # are comments.
                   Add ` rw` after a path (e.g. `LOCAL_DEV.md rw`) to make it editable:
                   the file is seeded once into shared-rw/ (a writable store dir) and
                   symlinked from there instead — edits persist there and are shared
                   live across the project's boxes. Default (no suffix) is read-only.
    settings.json  skein adds its probe hooks, SessionStart bootstrap, a statusLine,
                   and fullscreen TUI. Add your own statusLine / enabledPlugins / hooks
                   here — skein only fills what's missing and never clobbers yours.

Managed by skein (don't edit):

    skein/         the probe + machinery scripts + launch specs.
    mailbox/  status/  tasks/  journals/   per-box runtime. journals/<vmid>.md is a Stop-hook copy
                   of that box's own .skein/journal.md — the only way the host can see it for a
                   clone-mode box (its private clone isn't otherwise visible to the host at all).
    telemetry/     telemetry/<vmid>.jsonl — one line per turn, appended by box-token-usage.sh:
                   {ts, input, output, cache_read, cache_creation, total, duration_secs, tools}:
                   token usage + tool-call counts (e.g. {"Bash":2,"Read":1}) summed from the turn's
                   transcript entries, plus wall-clock turn duration (from box-status.sh's
                   mark_turn_start marker at UserPromptSubmit). Durable and structured (unlike
                   journal.md's free prose) so cost/usage stays reviewable across a box's whole
                   lifetime — feeds the same cross-run learn-loop as journals/diffs. Never deleted
                   when a box is torn down (a --clone's own transcript dies with it; this is the
                   only durable record).
    shared-rw/     the writable copies of `rw`-flagged shared-paths.txt entries.

Everything under `journals/`, `diffs/`, `tasks/`, and `telemetry/` deliberately **outlives** the box
that wrote it — skein's box-teardown path only ever cleans up *live* state (turn-status, launch
spec), never these per-box histories. That's what makes them useful for reviewing past runs, not
just the live one.
