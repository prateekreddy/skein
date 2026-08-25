# Cockpit smoke test

`cargo test` proves the API is right. This proves the **page** is right — which is not the same
thing, and once wasn't: the Files tab shipped with every directory row rendered and then hidden by
an unrelated CSS rule (`body.docked .dir` matched the file browser's `.fent.dir`). The API returned
those folders perfectly; you just couldn't click them, and nothing inside a folder was reachable.
Unit tests, `tests/server.rs` and clippy were all green. Only a browser could see it.

So this test asserts what is **visible**, never what merely exists in the DOM — that's what
`mustSee()` is for. If you add a check here, make it a click a person would make.

## Setup (once)

```sh
cd tests/ui && npm run setup     # npm install + playwright's chromium (~150MB, cached in ~/.cache)
```

## Run

```sh
node tests/ui/smoke.mjs          # from the repo root; builds skein-server itself

Runnable **inside a box** now: chromium's system libraries used to need a working `sudo`, which a
box does not have. Ask the fleet for them once — `sudo apt-get install libnspr4 libnss3
libasound2t64 libgbm1 libx11-6 libxext6 libcairo2 libpango-1.0-0 libxcomposite1 libxdamage1
libxfixes3 libxrandr2 libatspi2.0-0t64 libatk1.0-0t64 libxkbcommon0` files a request the cockpit
approves — then `npx playwright install chromium`. Playwright's own suggested list is **incomplete**:
it omits `libatk1.0-0t64` and `libxkbcommon0`, and the only way to find that out is `ldd` on
`~/.cache/ms-playwright/*/chrome-linux/headless_shell`.

It had been failing 10 of 52 for months while nobody could run it here. A suite that cannot run
where the work happens does not merely go stale — it goes *wrong*, and it hid a real bug (⌥[ opening
the microphone) behind nine tests describing features that no longer existed.
```

It launches the real binary against a throwaway workspace in `$TMPDIR` (a README, a `docs/` folder
with a doc inside, a symlink pointing in, a symlink pointing out) on a free port, with `$SKEIN_HOME`
redirected — it never touches your real store, registry or boxes. Exit code is 0 or 1; on failure it
prints a screenshot path and keeps the fixture for inspection.

Not wired into `cargo test` on purpose: it needs node and a browser, which the Rust toolchain can't
assume. Run it before shipping anything that touches `src/web/index.html`.

## `onboarding.mjs` — the first run, with nothing on disk

Every other suite here starts from a fixture that has already been onboarded: a repo in
`repos.json`, a placement record, a box on the board. So the path a new person actually walks — open
the cockpit, add a repo, launch the first box — was the one path nothing ever took, and it is the
one that broke.

It starts from an empty `$SKEIN_HOME`: no `config.json`, no `repos.json`, no registry, no sandbox,
and an `sbx` that has never been asked for one. Then it clicks: add a repo by local path, open the
launch dialog, launch. Three defects surfaced on the first run of it, all in the dialog and none
reachable from the CLI — Enter bypassing the no-repo gate, the repo control hidden whenever there
was exactly one, and a footer promising `<repo>-<branch>` instead of the name about to be created.

It also covers the fleet-sizing confirmation: the first launch must open the create dialog with the
host's own numbers in it, `sbx create` must not have run before the confirm, and the sandbox that
results must carry the sizes that were on screen — asserted against the recorded `sbx` argv, not
against the config it was saved to.

Its fixture root is under `target/`, not `$TMPDIR`: a box binds its own `/tmp` and `$HOME` over the
sandbox's, so `box-session.sh` refuses a fleet root under either.

## `tabs.mjs` — do your open tabs survive a reload

```sh
node tests/ui/tabs.mjs            # no setup, no chromium, runs inside a box
```

The restore path is a pure function of (what was saved, what the fleet reports), so it runs without
a page. It pins the case that lost tabs for good: a reload landing while `sbx ls` is slow got an
empty first snapshot, read it as "those boxes are gone", and then persisted an empty list over the
saved one. An empty snapshot is not evidence; a populated one is.

## `resources.mjs` — a box's own cpu, memory and disk on hover

```sh
node tests/ui/resources.mjs       # no setup, no chromium, runs inside a box
```

The card has two halves with very different costs, which is the whole design: disk rides on
`/api/boxes` and is free, while cpu and memory need an `sbx exec` and a half-second cgroup sample.
So the card must be useful *before* the sandbox answers. This pins that it is never empty, that
concurrent hovers share one cached request rather than firing one each, and that disk colours itself
against the box's allowance.

## `substrate.mjs` — what an approval actually sends, and how often it nags

```sh
node tests/ui/substrate.mjs       # no setup, no chromium, runs inside a box
```

Approving a package installs it under every box in the fleet, so the decision the cockpit sends has
to be the one its owner made: "remember" on by default, unticking it means install-now-don't-record,
and a denial records nothing whatever the checkbox shows. It also pins the announcing, because a
request sits pending until a person answers it — precisely the shape that produced the endless
re-announcing fixed once already. Announced once, then quiet, however long it waits.

## `overlays.mjs` — an overlay that is not styled is not an overlay

```sh
node tests/ui/overlays.mjs        # no setup, no chromium, runs inside a box
```

This exists because one wasn't. The Repo write access panel shipped with markup, a button, a
poller, a badge, decision handlers and sixteen passing tests — and no CSS. It was missing from the
two rules that make an overlay an overlay, so it rendered as a static div in normal flow, below the
fold, and clicking its button added `.open` to something nothing styled. A complete UI that could
not be reached, found by a person opening the page rather than by any test here.

Nothing caught it because every other suite lifts functions out of the page and runs them against a
stubbed DOM — the right way to test decision logic, and structurally blind to whether the thing
those decisions render into is visible at all. This checks the invariant textually instead: an
overlay is declared as `<div id=X aria-hidden="true">`, and the stylesheet must style both `#X` and
`#X.open`. Deliberately weaker than "is in the shared rule", since `#pal` carries its own copy of
the same declarations — requiring one particular rule would encode today's grouping rather than the
property that matters.

## `screenhalf.mjs` — does the board say when half the turn signal is missing

```sh
node tests/ui/screenhalf.mjs      # no setup, no chromium, runs inside a box
```

The badge exists because a missing screen half is invisible: the row falls back to hook edges and
looks entirely normal. It has failed at that twice in the same way. `screen_health` gained
`misfiled` in Rust and `SHALF` in the page was never told, so a refused observation rendered
nothing — and nothing is exactly what a healthy screen renders.

So the first check reads the states out of `src/signals.rs` rather than from a list kept here, since
a list kept here is the thing that goes stale; and the rest pin that an unrecognised value names
itself instead of disappearing. A badge that cannot say "I do not know this state" lies by omission,
and it lies in the direction of confidence.

## `voice.mjs` — what the mouth says, without a browser

```sh
node tests/ui/voice.mjs           # no setup, no chromium, runs inside a box
```

The smoke test above cannot run in a box (chromium's system libraries need a working `sudo`), so
everything about the voice shipped unverified — and shipped wrong. `waiting`, a box that ended its
turn and wants your next instruction, counted as "needs you" for the tab title and the needs-you
navigation and was left out of both voice paths. The tab said *3 need you* while the mouth stayed
shut, and "read what needs me" answered *nothing needs you* with boxes waiting on screen.

The sentences are pure functions of a fleet snapshot, so this lifts them out of `index.html` by name
and runs them directly — a test that works in the place the fix gets written. It asserts what is
**said**, and when: the grace period before a standing debt is announced, that the same debt is not
repeated, and that a changed one earns a new sentence.

If you rename one of the lifted declarations the test fails loudly rather than silently testing
nothing.
