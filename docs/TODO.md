# TODO

Work that is known, wanted, and not done. Ordered by what hurts most, not by effort.

Anything with a diagnosis attached has had it verified — the lead is the useful part, so it is kept
with the item rather than rediscovered.

---

## Broken now

### Shared login: the poisoning is fixed, the recovery is on box-restart cadence

The cause was that credentials synced by mtime alone, and a logout leaves a *newer* file than the
login it replaced — so one logged-out box propagated its emptiness to the sandbox and from there to
everything else. Fixed: a file only competes if it carries a login.

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

The mouth is built (skein speaks the ask when a box needs you, `index.html`). What is not:

### The ear — push-to-talk and a closed verb set

Hold a key, speak, release. `yes / no / next / skip / show me / stop / merge it`, plus `tell it …`
which sends the rest verbatim to `/api/boxes/:name/resume`. Box names resolved against the live
board rather than transcribed — the vocabulary is small and already in the page, which is the whole
reason this can work where general dictation cannot. Pronoun binding (`it` = the box just announced)
so a name rarely has to be said at all.

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

Once every box is on the fleet. Note the capability difference before doing it: a legacy box had
real root, a fleet box cannot (see the sudo shim in `box-session.sh`).

### The docs still describe one sandbox per box

`README.md`, `ARCHITECTURE.md`, `docs/self-sufficient.md`.

### Merge `modules-and-shared-sandbox` into `master`

### `enabledPlugins` in `sandbox-bootstrap.sh` is a setting nobody retries

Same shape as the fleet-agent bug already fixed: a marker file is written once and the work never
runs again if it failed.

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
`src/web/index.html` change; the voice checks in it are currently unrun.
