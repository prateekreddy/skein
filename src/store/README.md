# skein shared store

This folder (kept at `.claude` for backward-compatible mounting) is **one project's shared data**.
It serves Claude, Codex, and future runtime adapters; it is not a Claude-only transcript store. Every sandbox skein launches
for this repo mounts it live, so memory and the mailbox are shared across the
project's parallel boxes — and stay scoped to *this* project (see "Talking across
projects" below for the one exception).

**You don't have to put anything here for it to work.** skein installs and maintains
all the machinery itself (under `skein/`): provider-neutral probes, runtime hook adapters, a startup
bootstrap that bridges memory and surfaces mailbox hand-offs, the mailbox, and a
default status line — all wired into `settings.json` automatically. An empty folder
comes up fully working.

Skein installs `jq` during box setup as the single JSON dependency. It does not install Python or a
second agent CLI. A failed `jq` install is recorded in `skein/boot/<vmid>.json` and shown by the
cockpit health banner instead of silently pretending signals work.

**Box-to-box: talk directly, and fall back to the mailbox.** Claude Code's own
session messaging works across this fleet — `ListAgents` names every live box,
`SendMessage` reaches one, and the reply comes back. Skein carries it with two binds and
they are one decision: the session registry `~/.claude/sessions/`, which is how a box is
found, and the sandbox's socket directory `/run/user/<uid>/cc-socks/`, which is how it is
reached. A box gets both or neither — a box that were findable but unreachable would be
addressed by peers and hear nothing, which is the state this fleet was in until SKEIN-572.
Sessions are named after their box, so the name in `ListAgents` is the name you already
use everywhere else, and a peer shown as **local** is one this sandbox reaches without a
network. Repos can turn the whole channel off, in which case `ListAgents` shows no local
peers at all and the mailbox is the only way across.

Prefer it for anything conversational — it is synchronous, and the other box can answer
rather than merely receive. The mailbox keeps the three jobs messaging cannot do, and
they are not edge cases:

- **the box is not running.** A message needs a live session; mail waits on disk and is
  delivered at the box's next turn boundary, whenever that is.
- **the box is Codex.** It has no equivalent, so mail is the only channel that reaches it.
- **the box is in another project.** `all-projects` / `project:<repo-id>`, relayed
  host-side (below). Messaging has no notion of a project at all.

Durability is the real distinction: mail is a file, so it survives a box that dies before
reading it. A message to a session that goes away is simply gone.

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
    shared-home/   durable project working files, exposed as $HOME/shared in every Claude/Codex
                   box. Real $HOME remains box-private. Writes are live across the repo's boxes;
                   coordinate concurrent edits (ordinary last-writer-wins filesystem semantics).
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
                   skein/imports/ contains audit receipts for explicit shared-home imports.
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
    sessions/      sessions/<vmid>.json — the box's narrative signal (box-session.sh): the last
                   assistant message at Stop, or the prompt it's blocked on at Notification. This
                   is what the cockpit's inbox headline, fork-detector, and session digest read.
    hook-log/      hook-log/<vmid>.jsonl — one heartbeat line per probe firing (appended before any
                   real work, size-rotated). How the cockpit tells "hooks broken" apart from "box
                   quiet": a Running box with no heartbeat and no status has dark hooks and gets a
                   "⚠ no signals" badge.
    handoffs/      durable provider-neutral takeover briefs plus one-shot pending copies per target
                   runtime. Native provider transcripts remain separate; replacement boxes receive
                   a bounded context export while the original remains natively resumable.
    skein/handoff-snapshots/ immutable replacement snapshots: Git bundle, staged/unstaged patches,
                   untracked archive, shared memory/skills/hooks backup, transcript export, manifest.
    skein/boot/    skein/boot/<vmid>.json — the kit's boot report: whether the store was found,
                   how .claude was linked (linked | merged | no-store | failed), whether shared-home
                   and durable agent guidance were installed, jq/tmux presence, and branch.
    shared-rw/     the writable copies of `rw`-flagged shared-paths.txt entries.

Everything under `journals/`, `diffs/`, `tasks/`, and `telemetry/` deliberately **outlives** the box
that wrote it — skein's box-teardown path only ever cleans up *live* state (turn-status, launch
spec), never these per-box histories. That's what makes them useful for reviewing past runs, not
just the live one.
