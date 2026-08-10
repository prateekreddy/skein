# TODO

Work that is known, wanted, and not done. Ordered by what hurts most, not by effort.

Anything with a diagnosis attached has had it verified — the lead is the useful part, so it is kept
with the item rather than rediscovered.

---

## Broken now

### The fleet agent still is not installed — the cause is now testable

`fleet-agent.py` in the sandbox is still the Aug 7 v1 and nothing answers on 8317, across two
server restarts, while the launcher beside it is rewritten on every box start. In `ensure_fleet` the
agent install runs *immediately before* the launcher install and is non-fatal, so "launcher fresh,
agent stale" is the signature of the agent step failing or being skipped.

The leading explanation, now fixed but not yet confirmed as the cause: `load_config` silently
discarded a `config.json` it could not parse and returned defaults, in which `fleet_agent` is false —
so `heal_fleet_agent` returned on its first line with no message, while the file said `true` and was
right.

**Settle it after the next deploy**: `skein doctor` now prints the parse error above the settings
line, and the board's transport row shows `settings / unreadable` in place of `off`. If neither
appears and the transport row is amber, the config was fine and the failure is downstream — the
server's `skein: the in-sandbox agent is not serving (…)` line names it.

### Codex is parked, deliberately

`sync-install.sh` still writes Codex's `[mcp_servers.sync]` TOML when a token exists, but nothing
about the Codex path has been verified since the plugin took over, and the vendored skill it depends
on is now skipped on any box where the plugin installed. Revisit when Codex is actually used —
including whether the token is still worth minting at all.

### The installed sync plugin is 0.2.0, whose lease monitor keeps nothing alive

**Not an upstream report any more — upstream fixed it.** `plugin/bin/sync-monitor` on `main` now
resolves through a shared `sync-paths.sh` (`sync_session_id` → `CLAUDE_CODE_SESSION_ID`), and its
comment describes the same defect in the same terms: it "used to read CLAUDE_SESSION_ID — a variable
Claude Code does not set — and fall back to `default.watch`, so it polled a file nothing ever writes
and kept nothing alive, quietly, forever."

What is left is a **version** problem, and it is live. The plugin installed in these boxes is
**0.2.0** (marketplace at `392d6ab`), whose `sync-monitor` still has
`WATCH_FILE="$TOKEN_DIR/${CLAUDE_SESSION_ID:-default}.watch"` and no `sync_session_id` at all. So the
lease keepalive in every box does nothing, silently — the failure its own README warns about, "a
guard nobody knows is disabled". Upstream knows: `sync-monitor` calls out 0.2.0 by name as predating
the fix, and `8fac188` makes a monitor left behind by an update stop rather than pretend.

**Done.** This box updated to 0.4.7 (marketplace `c573fd1`) and `sync-install.sh` now refreshes the
marketplace on every start, so no box can freeze on a version again — the marker goes back to
answering only "does this box have the plugin", which is the one question it should ever have
answered. Every other box picks it up on its next start.

The vendored skill needed nothing: the sixteen commits between `e113f18` and `c573fd1` are all
server, lease and monitor logic (reconciliation about to return every finished item to the pool, one
410 blinding the monitor for 17 minutes, a credential that aged out reading as a takeover) and
`plugin/skills/` is byte-identical across the range. So the submodule pin can be bumped whenever
convenient without re-vendoring or touching the always-on block.

### Shared login: the poisoning is fixed, the recovery is on box-restart cadence

The cause was that credentials synced by mtime alone, and a logout leaves a *newer* file than the
login it replaced — so one logged-out box propagated its emptiness to the sandbox and from there to
everything else. Fixed: a file only competes if it carries a login.

Two more of the same shape, found by surveying the live fleet on 2026-08-10 and now fixed:

- **the host side never got that fix.** `sync_fleet_login` tested "the sandbox has a file", and a
  husk is a file — so a logged-out sandbox overwrote the copy in `fleet-home`, which exists solely
  so a rebuild can restore it. The decision is now `login_move`, tested rather than inline.
- **mtime was still the tiebreak between two real logins**, and it answers the wrong question: it
  says when a file was *written*, not which credential is better. A box that starts rewrites its own
  copy, so it holds the newer mtime whether or not its token is the older one. Ordering is now by
  `expiresAt`, which can only ever prefer the longer-lived credential; mtime remains the tiebreak for
  shapes that record no expiry (codex `auth.json`).

The survey itself is the useful artefact — nine boxes held **nine distinct token pairs**, so the
fleet does not share a live credential in steady state and is not meant to: each box diverges the
moment it first refreshes, from one seeded login. What was broken was four boxes that had not
started since their token lapsed (`bridge-a-b-master` a husk from Aug 4, `chassis-statement-
parsing` and `example-box-7` expired ~Aug 7, `example-box-1` Aug 9) — the restart cadence below, not the
propagation rule.

What remains is a lag rather than a fault. Healing happens when a box **starts**, so the fleet
recovers as boxes restart and not before. If a login is needed sooner than that, restarting any
logged-in box pushes its credentials up and every later start picks them up.

Worth considering, not yet done: heal from the whole fleet rather than only from the sandbox's copy
— on start, if neither this box nor the sandbox carries a login, take the newest one that does from
`/boxes/*/home/.claude/.credentials.json`. It would repair everything from a single box start
instead of needing the *right* box to restart. It reads other boxes' private homes, which is
consistent with the stated model (boxes are isolated from each other's state, not their identity)
but is still a widening, so it wants a decision rather than a commit.

The `mcpOAuth` half is fixed: the sync now merges the login keys instead of copying the file, so a
box keeps its own per-repo MCP grants and never receives another repo's.

---

## Voice — the rest of it

The mouth and the ear are both built. What is not:

### Try it for a day and cut what annoys

Two defects are already fixed and worth knowing about when judging the rest: `waiting` was missing
from both voice paths, and the mouth only ever spoke at the *instant* of a change while you were
away — so a box that turned while you were looking at the board was never announced at all.
`tests/ui/voice.mjs` covers both.

The verb set is a guess until it is used: `yes / continue / next / show me / open / what needs me /
continue all`, plus `tell it …`. Expect the ordering of `voiceTarget` to be the part that feels
wrong first — it prefers the box the mouth last named, then the selected row, then the only one
owed an answer.

Deliberately absent, and worth keeping absent unless the daily use argues otherwise: stop, merge,
ship, destroy. A confirmation is not the answer for those, because the point of the ear is that you
are not looking at the screen.

### Settle whether on-device recognition actually engages

A ten-minute probe page: `SpeechRecognition.available({langs:['en-US'], processLocally:true})`, then
`install()`. Decides whether voice works with the network down. Chrome has an open macOS bug in this
path; Safari routes through Apple's engine. Does not change the design either way.

### Intent fallback for anything the grammar misses

Transcript + board state → Haiku via the existing `claude_oneshot` in `ai.rs` → `{action, box, args}`.
Pennies a day. Must feel like a distinct, slower path so the latency reads as deliberate.

---

## Cross-session messaging — one bind away from working

Claude Code v2.1.224+ ships `SendMessage` / `ListAgents` between sessions. Measured inside a fleet
box on 2026-08-09:

- the **transport is already shared**: `/run/user/1000/cc-socks/` holds every box's inbox socket,
  four of them visible from inside one box;
- the **discovery registry is not**: each session registers in `~/.claude/sessions/<pid>.json`, and
  `~/.claude` is bound private per box — so `ListAgents` reports "No reachable agents" while the
  sockets it would use are sitting right there.

Sharing `~/.claude/sessions/` alone (not the rest of `~/.claude`, which holds credentials and
history) would give every box in the fleet native box-to-box messaging with no skein code. That is
close to what the cross-box mailbox already does, so the question is whether it replaces the mailbox,
complements it, or is not worth the extra shared surface.

If taken up: pass `--name "$SKEIN_BOX"` when launching, or every box derives the same name from its
`tree` directory. And check `crossSessionInbound` — boxes running with permission prompts bypassed
hold each other's messages for approval by default, which would look like silent loss.

Docs: <https://code.claude.com/docs/en/cross-session-messaging>

---

## The HTTP transport

### It has not yet faced a real stall

The only test that matters for it. Next time the daemon hangs:

```sh
curl -s http://127.0.0.1:$(cat ~/.skein/fleet-agent.port)/health
```

An answer means the VM is alive and the daemon path is stuck — which is evidence for
docker/sbx-releases#163, currently resting on inference.

### Leaked port mappings

51957–51959 on `skein-fleet`, from before the agent bound `0.0.0.0`. Harmless: sbx has no unpublish
verb, and the reuse logic skips them because they never answer.

### `copy_guest_file` still uses `sbx exec`

Deliberate — it is a download, and `/exec` would buffer a multi-GB bundle in memory at both ends.
Revisit only if a streaming download endpoint earns its keep.

---

## Migration and cleanup

### Retire the per-VM box model

**Blocked on a check only the host can do**, and worth stating because it is not obvious from a box:
a legacy box is its own sbx sandbox, so it does not appear under `/boxes/` at all. `sbx ls` on the
host is the only way to know none remains, and removing the fallback while one exists strands it.

Scope, measured: `place_of(name).unwrap_or_else(|| own_sandbox(name))` is the shape everywhere, and
there are **62 legacy references across 7 files** (`config.rs`, `repos.rs`, `sandbox.rs`,
`tracking.rs`, `mailbox.rs`, `fleet.rs`, `lib.rs`). That is its own session, not a tail-end cleanup.

Note the capability difference before doing it: a legacy box had real root, a fleet box cannot (see
the sudo shim in `box-session.sh`).

### The docs still describe one sandbox per box

**Done for `README.md` and `ARCHITECTURE.md`.** The latter mattered more than a tidy-up: its "hard
constraint" section asserted boxes were separate microVM kernels and ruled out SQLite across boxes
on that basis. They share one kernel now, so the constraint was forbidding a design that is
available — the file-based mailbox stays, but on the honest grounds (mail must outlive a box that
never read it), not a kernel boundary that no longer exists.

`docs/self-sufficient.md` still describes the old `sbx create --clone` per-box launcher. It reads as
a design note about how the launcher was derived rather than as current reference, so it wants a
decision — update it, or mark it historical — rather than an edit.

### Merge `modules-and-shared-sandbox` into `master`

### `agent-memory-consolidation` has no `origin` remote

### The cockpit's white background has never been diagnosed

---

## Verification gaps

### `cfg!(test)` is false in `tests/`, so process-global gates leak between integration tests

The library an integration test links was built without `cfg(test)`, so every "no gate under test"
escape inside `src/` is inactive there. A warm gate then serves one test the previous test's answer
— immediately, while refreshing behind the caller, which is the behaviour that stops the board
blanking and is worth keeping.

`fleet_liveness` is handled (`forget_fleet_liveness`). The other gates — `FLEET_GATE`, `DISK_GATE`,
resources — have the same exposure the moment an integration test touches them.

### The browser smoke test cannot run in a box

`tests/ui/smoke.mjs` needs chromium's system libraries, and a box has no working `sudo` to install
them — the shim above explains why. It downloads but will not launch. Run it on the host after any
`src/web/index.html` change.

This is not academic: it is why the voice shipped with `waiting` missing from both of its paths and
spoke nothing while boxes sat waiting. `tests/ui/voice.mjs` closes that particular hole by lifting
the pure sentence-building functions out of the page and running them in plain node, which works in
a box — but everything about the page that needs a *browser* is still only covered on the host.
Anything testable without one belongs in the node test, precisely because that is the one that gets
run where the code is written.
