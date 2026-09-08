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
and `$SKEIN_FLEET_ROOT` redirected — it never touches your real store, registry or boxes. Exit code
is 0 or 1; on failure it prints a screenshot path and keeps the fixture for inspection.

**Both of those variables, in every suite that starts a server.** `$SKEIN_HOME` alone is not enough:
placement records, gitgate requests and box sessions live under the fleet root, and `$SKEIN_HOME`
covers the store. `util::fleet_root` — `src/util.rs`, and it has never been in `config` — refuses
an unpinned **test** process (SKEIN-690), which the server is when `cargo test` runs these suites
through `tests/browser_suites.rs`; run one by hand and it answers `/boxes` instead, which on a
machine running skein is a **real fleet** (SKEIN-530). `attach`, `connections` and `updatepane`
pinned only the first, and read placement records and gitgate requests out of whoever's fleet was
running.

## How they are run

`cargo test` runs all of them — `tests/browser_suites.rs` is the file that invokes them, and its
module comment is the argument for why. The node tier runs everywhere; the browser tier runs when
Playwright's chromium is installed and reports that it was skipped when it is not.

**CI installs chromium, so the browser tier runs there too.** It did not until SKEIN-567: the skip
is by design for somebody building skein, and it silently applied to CI as well, so every green run
in this repository's history had skipped every browser suite — the two largest included.

Locally, use `--no-fail-fast`:

```sh
cargo test --all --no-fail-fast
```

`cargo test` stops at the first test **binary** that fails, and `browser_suites` sorts before most of
the others — so on a box where a browser suite is red, the remaining ~20 binaries never run and the
report says nothing whatever about them. That is not hypothetical: master was pushed red at `d5d0e95`
on a local run that looked like the expected browser failure, hiding two unrelated broken gates.

**Your GitHub token is not what the suites run on.** `harness/server.mjs` drops `$GITHUB_TOKEN` and
sets `$GH_TOKEN` to `FIXTURE_GH_TOKEN`, a value that is a credential nowhere. The review queue reads
those two variables first of all — no token at all and `queue_within` fails before it asks GitHub
anything — and every skein box exports one, so `actfail`, `connections` and `review` were passing on
whatever the developer happened to be logged in as. The first CI run of the browser tier had no
token and all three failed with an empty queue, 82 checks between them (SKEIN-621). A suite that
wants the no-credential case asks for it: `GH_TOKEN: ""` in its own `env`.

Run a suite on its own before shipping anything that touches `src/web/index.html`.

## Where the fixtures go, and who cleans them up

Every suite here builds a throwaway tree and drives the real `skein-server` against it. Two
questions follow, and they were answered separately and wrongly for a while.

**Where.** `$SKEIN_UI_FIXTURE_ROOT`, defaulting to `/var/tmp/skein-uifix` (`lift.mjs`,
`fixtureRoot`). Not `/tmp` and not under `$HOME`, because a box binds its own directories over both
and `src/box-session.sh` refuses a fleet root beneath either — the first draft of `onboarding.mjs`
spent a run learning that. Not `$CARGO_TARGET_DIR` either, which is what it used to be: a box's
tmux socket is `<root>/fleet/<box>/session.sock` and **a unix socket path cannot exceed 108 bytes**,
while an agent worktree in this fleet is ~118 characters before the fixture appends anything. So
the old default could not work from any worktree, only from a checkout at a short path, and the
suite refused up front telling each agent in turn to set the variable by hand (SKEIN-603).

**Who cleans up.** `freshFixture` in `lift.mjs`, which every suite that keeps a fixture should use.
A suite deletes its own on the way out but **keeps it when it fails** — the fixture is the only
evidence a failure leaves, and a suite that tidies it away is one nobody can debug. Nothing ever
removed a kept one: 48 directories and 60 MB were measured on this box, and the suite whose
failures somebody is working on is exactly the suite that fails repeatedly, so the debris grows
fastest while it is being looked after (SKEIN-590).

The sweep that fixes that has to be keyed on **the pid, not on an age**, and this is the part worth
reading before writing another one. Sweeping everything with the right prefix at the start of a run
was tried and reverted: the root is one directory shared by every worktree on the box, so it
deletes the fixture a concurrent run is writing into — observed, not theorised. An age rule has the
same defect from the other side, sweeping a slow live run or leaving a dead one for hours. So
`freshFixture` names the directory `<prefix>-<pid>-XXXXXX` and asks the operating system whether
that pid is still alive, which is the same answer `tests/common/mod.rs::sweep_abandoned` already
reached for the Rust harness. Pid reuse can only make it KEEP a dead run's directory, never remove
a live one's.

A directory whose name carries no pid — anything from before this — is left alone deliberately, on
the same "do not delete what you cannot reason about" rule. Remove those by hand once.

## `leakcheck.mjs` — can the leak check see the server it is about

```sh
node tests/ui/leakcheck.mjs       # no setup, no chromium, runs inside a box
```

`node tests/ui/harness/leaks.mjs` is what answers "did that run leave anything behind", and it has
been unable to fail twice now. The first time it restated three fixture names and went stale, which
was fixed by deriving the names from the call sites that create them. The second time it tried
those derived names against `/proc/<pid>/cmdline` alone — and the server a suite starts is exec'd
as a bare binary path, told which fixture it belongs to in `SKEIN_HOME` and `SKEIN_FLEET_ROOT`. So
the process left behind most often was the one shape the check could not see, and it printed
"nothing is running from any of them" beside a `skein-server` that had been up for seven and a half
hours (SKEIN-687).

So this one starts a process carrying a derived prefix **only in its environment**, and checks that
the report finds it, names the prefix it matched, and prints no part of the environment it matched
in — that last because a real server's environment carries a credential. Written the other way
round, with the prefix in the arguments, it would have passed before the fix and proved nothing.

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
