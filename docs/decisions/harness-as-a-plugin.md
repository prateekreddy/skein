# Research, parked: which part of the box harness could be a Claude Code plugin

**Decision.** Parked, with no code written. About half of skein's box harness could be packaged as a
Claude Code plugin and half provably cannot. Two blockers are to be tested before any of it is
built. This record is the research, kept so the question is not re-derived from scratch; the
plugin claims below are as the documentation stood on the date, not re-verified since.

**Date.** 2026-08-06.

**Decided by.** The project owner, who parked it in favour of the more immediate work of the day.

**What would move cleanly.** The hooks skein wires into each store (`src/probe/box-status.sh`,
`src/probe/box-diff.sh`, `src/probe/box-journal.sh`, `src/probe/box-token-usage.sh`,
`src/probe/box-task.sh`, `src/probe/box-handoff.sh` and the mailbox), the skills, and agent
guidance — guidance as a skill, because a plugin's own `CLAUDE.md` is not loaded as context. Loaded
with `--plugin-dir` from a path skein controls: no marketplace, no network, versioned with the
binary. That would also retire the network-dependent plugin install the bootstrap runs in the
background at every box start (`src/store/sandbox-bootstrap.sh:268-286`).

**What provably cannot.** `statusLine` (a plugin's settings honour only `agent` and
`subagentStatusLine`), `tui`, `permissions` and `env`; and the whole provisioning layer, because an
installed plugin cannot reference paths outside its own directory, so the store bridge, the memory
bridge and the kit stay in skein.

**Capabilities skein does not use yet.** Channels — an MCP server pushing events into a live
session, with native permission relay by request id, where today the mailbox only surfaces when the
person types; background monitors; the `PermissionRequest` and `PermissionDenied` hook events, which
would replace string matching on `Notification`; plugin user configuration with keychain-backed
sensitive values; and `claude plugin validate --strict` in CI, the missing preflight for a
malformed-settings outage.

**The two blockers, to settle before writing any of it.**

1. A self-hosted channel is off the curated allowlist, so it needs
   `--dangerously-load-development-channels`, which shows a full-screen confirmation at session
   start. A box launched in tmux waiting on a keypress is a regression. Test this first: it alone
   decides whether the channel direction is viable.
2. Codex gets nothing. Plugins are Claude-only, and the runtime adapter table exists to keep skein's
   seam provider-neutral (`src/runtime.rs:71`). Moving shared hooks into a plugin un-shares them.
   The portable unit is the skill; hooks are not portable.

**Rules out.** Starting a plugin port before both blockers are tested.

**Enforced at.** Nothing; this is research. The kit is where skein's settings are merged into a
box's own untracked `settings.local.json` today (`src/kit/skein-startup.sh:527`).
