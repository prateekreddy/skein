# skein shared store

This `.claude` folder is **one project's shared data**. Every sandbox skein launches
for this repo mounts it live, so memory and the mailbox are shared across the
project's parallel boxes — and stay scoped to *this* project.

**You don't have to put anything here for it to work.** skein installs and maintains
all the machinery itself (under `skein/`): the turn-state probe, a SessionStart
bootstrap that bridges memory and surfaces mailbox hand-offs, the mailbox, and a
default status line — all wired into `settings.json` automatically. An empty folder
comes up fully working.

What you *optionally* add is your own project content:

    memory/        project memory. The bootstrap bridges ~/.claude/projects/<cwd>/memory
                   → here, so it's live and shared across this project's boxes. Starts
                   empty and accumulates as boxes work; drop in notes to seed it.
    skills/        project skills (one dir per skill).
    hooks/         your own hook scripts, if you reference them from settings.json.
    settings.json  skein adds its probe hooks, SessionStart bootstrap, a statusLine,
                   and fullscreen TUI. Add your own statusLine / enabledPlugins / hooks
                   here — skein only fills what's missing and never clobbers yours.

Managed by skein (don't edit):

    skein/         the probe + machinery scripts + launch specs.
    mailbox/  status/  tasks/   per-box runtime.
