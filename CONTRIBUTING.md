# Contributing to skein

Most of what is unusual about this repository is written down already, and this page is mostly a
map to it rather than a second copy. The two things worth reading before anything else are
[`CLAUDE.md`](CLAUDE.md), which is the working discipline, and
[`ARCHITECTURE.md`](ARCHITECTURE.md), which is a signpost to the four design documents.

One rule governs everything below, and it is the one to internalise:

> **Derive, do not assert.** Where a claim is about the code, cite the file and line or give the
> command — never paraphrase from memory. Prose that *summarises* code drifts; a claim that
> *counts* something reproduces.

That applies to a pull request description, a code comment and a commit message alike. It is not a
style preference: `tools/prose-check.py` is a build gate that fails when the prose names a symbol
the code does not have, and it exists because one review cut left nine live references to deleted
machinery, two of them in `docs/parity.md` — the acceptance gate — for as long as nobody looked.

## Be honest with yourself about what you can run

skein drives a **fleet of sandboxes**: `sbx` (Docker Sandboxes) makes one microVM, and skein runs a
`bwrap` namespace per box inside it. Setting that up is the README's "Getting started", it fixes
memory, CPUs and disk for the life of the sandbox, and most people who want to fix a bug here will
not have one and should not need one.

**You do not need a fleet to contribute.** The whole test suite runs on an ordinary Linux or macOS
checkout with a Rust toolchain and node, against fixtures and recording fakes. When this page was
written it was 1,221 tests across 37 binaries, all green, on a machine with **no `sbx` on `PATH`
at all** — so the claim is not that the fleet paths are skipped politely, it is that they are
covered without one. What you *cannot* do without a fleet is watch a real box come up, and the
suite is candid about which checks that costs you rather than pretending otherwise:

* **On anything that is not Linux, eighteen tests do not run.** They drive the shell scripts skein
  installs *into* a box — `readlink -f`, `sort -z`, `/proc/<pid>/stat` — and each of those
  spellings is the correct one where the script actually runs. The set is declared in
  `tests/platform_gates.rs` as `GATED`, with a reason per entry, and
  `every_platform_gated_test_is_declared_with_its_reason` fails the build if a test gets gated
  without being written down. Count them yourself rather than trusting this sentence:

  ```sh
  python3 - <<'EOF'
  import re
  s = open('tests/platform_gates.rs').read()
  body = re.search(r'const GATED: &\[\(&str, &str\)\] = &\[(.*?)\n\];', s, re.S).group(1)
  print(len(re.findall(r'\(\s*\n\s*"([a-z0-9_]+)"', body)))
  EOF
  ```

* **Without `bwrap`, the isolation cover is proved by nothing.** `tests/isolation_bwrap.rs` runs a
  real namespace and reads the paths back; where it cannot make one it skips with a message. That
  skip is right on a laptop and wrong in CI, which is why the workflow installs bubblewrap, turns
  `kernel.apparmor_restrict_unprivileged_userns` off, and then *proves* it with
  `bwrap --dev-bind / / -- /bin/true` as its own step. Installing it was not enough on its own and
  believing it was cost 27 days of red master.

* **Without Playwright's chromium, the browser tier does not open a page.** See below.

## Setting up

```sh
cargo build --release --workspace
```

`--workspace` and not a bare `cargo build`: the workspace's second crate is `warden/`, which
produces `skein-warden`, and the fleet cannot be created or resized without it. The warden is a
separate crate deliberately — architecture §14 gives it an empty depends-on column, because it
exists to authorise the privileged operations skein is not trusted to authorise for itself, and a
shared library would be a shared blast radius.

Then the submodule, if you did not clone with `--recursive`:

```sh
git submodule update --init      # upstream/sync — a few MB, and `submodule-check` requires it
```

`upstream/sync` is the gateway repo, and two library tests hold the vendored copies in
`src/store/sync/` against upstream's originals — which is the entire reason vendoring is safe to do
here. Without the checkout both tests return early, and **a skipped test passes**: in this fleet the
submodule was never initialised, so `cargo test --lib` read 1051 passed / 0 failed while those two
guards compared nothing at all, on every run, for as long as anyone had been running them
(SKEIN-826). The `submodule-check` gate is why that cannot recur quietly — it fails with this
command in its message.

**Contrast it with the download below, which is the honest version of the same trade.** The browser
tier's skip is right: 150 MB is not a reasonable build dependency, so the suites announce themselves
as skipped and CI installs the browser instead. A few MB of markdown is not that, so there is no
version of this one worth skipping — which is what makes it a gate rather than a notice.

The browser tier needs a one-time download:

```sh
cd tests/ui && npm run setup     # npm install, then playwright's chromium (~150 MB, cached)
```

Skip it and `cargo test` still passes — it reports that the browser suites were skipped and names
the command that installs them. That is deliberate: a 150 MB download is not a reasonable build
dependency for somebody fixing a typo. **CI is not somebody fixing a typo**, and the workflow now
installs it, so the tier that opens a page runs there whether or not you ran it here. It did not
until SKEIN-567, and every green run before that had skipped all six.

### Check which history your clone is on, before you push anything

```sh
git merge-base HEAD origin/master     # prints a sha, or exits 1 with nothing
```

**Empty means your clone is not a clone of this repository, whatever its remote says.** Going
public re-rooted the history to strip a prior client's identifiers, so a clone taken before that
shares no commit with `origin/master` at all — not an old one, none. `git status` still calls that
"diverged", `git log` still looks like skein, and the remote is still configured, so nothing in the
normal workflow tells you. A `git push --force origin master` from such a clone puts every stripped
identifier back on a public repository, and a push cannot be unseen.

It cost a real scare (SKEIN-620): a `git pull --ff-only` reported "797 and 887 different commits",
which reads as a bad merge and is in fact two unrelated histories. If the command above prints
nothing, do not push, do not merge, and do not try to reconcile the two — take a fresh clone and
move your work across by patch.

## Running the tests

**Run this, not `cargo test`:**

```sh
cargo test --all --no-fail-fast
```

`--no-fail-fast` is load-bearing, because **`cargo test` stops at the first test *binary* that
fails** and there are 37 of them. `tests/browser_suites.rs` sorts fourth of the 29 in `tests/`
(`ls tests/*.rs | sort`), so a single red browser suite means the report says nothing whatever
about the twenty-five after it. That is not hypothetical: master was pushed red at `d5d0e95` on a local run that stopped inside
`browser_suites`, hiding a second broken gate that CI — fail-fast too, at the time — then found
while still not reaching a third. One run that reports everything beats two that each report the
first thing, which is the argument for the flag in both places: CI passes it now as well.

This line used to begin `env -u SKEIN_IN_FLEET`, and the prefix is gone rather than optional
(SKEIN-643). That variable is set inside every skein box and once chose between two deployments,
but SKEIN-521 deleted the one it chose against and nothing has read it since — so unsetting it
changed nothing, while teaching everybody who ran this command that the tests branch on where they
are running.

There is a second hazard worth knowing before you write a test that starts a server: **pin
`$SKEIN_FLEET_ROOT` as well as `$SKEIN_HOME`.** `util::fleet_root` falls back to `/boxes` when
the first is unset, and on a machine that is running skein that is a *real* fleet. Suites that
pinned only `$SKEIN_HOME` read placement records and gitgate requests out of whoever's fleet
happened to be running — see the note in `tests/ui/README.md`.

**The `$SKEIN_HOME` half of that is now enforced, and it is not only about servers.**
`config::skein_home` panics rather than answering a test that has not pinned it. The marker is an
environment variable — `.cargo/config.toml` puts `SKEIN_TEST=1` in the `[env]` table, so every
`cargo test` run from this tree carries it — and deliberately not `cfg!(test)`, which is false
inside the library when it is linked into a `tests/*.rs` binary and would therefore be absent from
the suites that drive the most machinery (`util::TEST_MARKER` says this at more length;
`tests/harness.rs` asserts the marker actually arrives). It exists because a unit test with no
server anywhere in it wrote `boxes/box-route/resume.log` into the owner's live `~/.skein`, beside
the state of sixteen real boxes, and no search could have found it: that test pinned *neither*
variable, so it matched no grep for either (SKEIN-626). Forty tests across thirteen modules and
three integration binaries were resting on the fallback when the guard went in.

**`$SKEIN_FLEET_ROOT` is now guarded the same way, and this paragraph used to say the opposite.**
It read that a guard here would fail ~29 readers who interpolate the root into a string they never
act on, for a hazard none of them has, and that "nothing will stop you". Something stops you now.
The premise did not survive being measured: the readers that never act on the string cost one line
each to pin — `src/fleet/` already pinned the variable in dozens of places — while the ones that DO act
on it were found by damage, six times, one at a time. Five tests installed uncommitted code onto
the owner's live fleet (SKEIN-530); `tests/server.rs` spawned a real `skein-server` whose
`heal_fleet` rewrote `/boxes/.skein/box-session.sh`, the launcher every real box starts through,
on every `cargo test --all` (SKEIN-685); and one unit test that only READ passed or failed on how
full the machine's disk was, with a message that sent the reader to `health.rs` to find a bug that
was not there (SKEIN-690). A pin that is only recommended is a pin that is sometimes missing, and a
missing pin matches no grep — the defect is the absence. `util::fleet_root` refuses an unpinned test
process now, exactly as `config::skein_home` does, and it names both variables when it does.

The predicate the two guards share — `util::in_test` and `util::TEST_MARKER` — lives in `util`
rather than in `config`, where it was written: `fleet_root` is in `util`, `config` depends on `util`
and not the reverse, and the alternative was a second copy of `cfg!(test) || env::var_os(..)`
(SKEIN-690).

### The cockpit's own suites

The cockpit's suites live in `tests/ui/`, in two tiers, and `tests/browser_suites.rs` is what
invokes them from `cargo test`:

| tier | the list | needs | what it is for |
|---|---|---|---|
| node | `NODE_SUITES` | node only | the page's pure functions, lifted out and run directly |
| browser | `BROWSER_SUITES` | chromium | a real page, asserting what is **visible** |

Neither tier's size is written here, and that is deliberate. The lists are the fact;
`every_suite_in_the_directory_is_in_one_of_the_lists` keeps them level with what is on disk. A
tally copied into prose is checked by nothing, so it goes stale in silence — three of them did,
here and in `tests/browser_suites.rs`, while every build stayed green (SKEIN-613, SKEIN-656).

Both lists are constants at the top of `tests/browser_suites.rs`, and
`every_suite_in_the_directory_is_in_one_of_the_lists` fails if a suite is in neither — an unlisted
suite is one nothing runs. Run one on its own to read all of its output:

```sh
node tests/ui/smoke.mjs
```

The browser tier exists because the other tiers are structurally blind to a class of defect: the
Files tab once shipped with every folder rendered and then hidden by an unrelated CSS rule. The API
returned them perfectly and you could not click them. Unit tests, `tests/server.rs` and clippy were
all green. So a check added there asserts what a person can **see**, never what merely exists in
the DOM — that is what `mustSee()` is for. `tests/ui/README.md` is worth reading in full before you
add one.

## The gates

**There is one gate list and it is `tools/gates.sh`.** It takes a worktree path, so a box carrying
several of them can run the gates for one without standing in it:

```sh
tools/gates.sh                  # every gate, against the repository this script is in
tools/gates.sh /path/to/tree    # every gate, against that worktree
tools/gates.sh --list           # the list, and the command each gate runs
tools/gates.sh run prose-check  # one gate, exiting its status — what CI calls
```

It stamps its verdict with the HEAD it read **before** the first gate, and if HEAD moved or the
working tree changed while it ran it prints both values and refuses to give a verdict at all
(SKEIN-784). That is neither a pass nor a failure and it exits `3` to say so: results that describe
a tree you are no longer on must not be quoted as evidence for a commit, which is what the footer
gets used for.

`.github/workflows/ci.yml` invokes those same gates, one step per gate, as `tools/gates.sh run
<name>` — so that a red X in the UI still names the gate that failed while the command it runs is
written down only once. Fifteen of its `- run:` steps are not gates. Six are in the `check` job:
four prepare the machine, one proves bwrap actually works, and one deepens the clone for the step
after it. Two are the `msrv` job, which reads `rust-version` out of `Cargo.toml` and builds every
target at it, and seven are the `coverage` job, which holds line coverage at a floor — both under
"Releases, and what CI covers" below. The step that reports
what the run skipped used to be a seventh — advisory, and therefore never acted on, which is what
made `noskip-check` a gate instead (SKEIN-558, SKEIN-881). **The rest are gates that can fail your
change**, and `tools/gates.sh`
holds one more that CI deliberately does not run. Both numbers below are checked by
`gate-list-check`, so neither can go stale the way the pair here did before SKEIN-741:

```sh
grep -c '^      - run:' .github/workflows/ci.yml     # → 33
tools/gates.sh --list | wc -l                        # → 19
```

| gate | what it enforces | where the exceptions are declared |
|---|---|---|
| `fmt` | formatting | — |
| `clippy` | lints, both crates | — |
| `submodule-check` | every declared submodule is checked out, so the tests that read one run rather than skip | — |
| `test` | the suite, every binary | — |
| `alone-check` | every lib test still passes in a process of its own — **the one CI does not run**, below | — |
| `module-check` | the module graph of architecture §14 | `docs/modules.toml` |
| `source-check` | the Source law of §2.3 | `docs/sources.toml` |
| `env-lock-check` | no `set_var` outside `env_lock()` | `docs/env-lock.toml` |
| `fleet-pin-check` | a test that pins `$SKEIN_HOME` or `$SKEIN_FLEET_ROOT` says something about the other | `docs/fleet-pins.toml` |
| `prose-check` | every backticked symbol in prose exists, and every `file:line` citation can be followed | `docs/prose-symbols.toml`, `docs/prose-debt.toml` |
| `line-cite-check` | every `file:line` cited in `docs/` still says what it said when it was cited | `docs/line-cites.toml`, and `historical = "<why>"` in it |
| `continuation-check` | no `\`-continuation collapsed into a run of spaces | a `// continuation-ok:` marker, with its reason |
| `residue-check` | no identifier from before this repository | `docs/residue.toml`, `docs/residue-banned.txt` |
| `cockpit-tests` | the cockpit's pure functions | — |
| `cockpit-bundle` | the committed bundle is not stale | — |
| `fixture-root-check` | every browser suite that drives a real server builds its fixture under the fixture root, not `os.tmpdir()` | — |
| `noskip-check` | the suite again under `$SKEIN_TESTS_NO_SKIP`, failing on a skip inside a test binary whose every declared requirement this machine has — and reporting, only, about the rest | `ENVIRONMENTAL` in `tools/noskip-check.py` |
| `gate-list-check` | this table, `ci.yml` and `tools/` itself still name the set `tools/gates.sh` defines | the `ci` column, and `not_a_gate`, in `tools/gates.sh` |
| `citation-check` | every commit sha cited in `docs/` is still reachable | `docs/citations.toml` |

This table and the count above it were both wrong until SKEIN-741 — fifteen steps and ten gates,
when the workflow had seventeen and eleven, with `citation-check.py` in neither. That is the drift
the rest of this page is about, in the paragraph describing the machinery that exists to stop it.
It went wrong again immediately afterwards and in both directions at once, which is why the table
is now keyed on gate names rather than on commands and why `gate-list-check` is in it. Measured on
`94d6776`, the day before this section was rewritten: the workflow ran **13** gates and the runner
every agent was told to use ran **13**, and they were not the same thirteen — the workflow had
`line-cite-check` and no `alone-check`, the runner the reverse. Union **14**, intersection **12**,
and each list was missing one the other had (SKEIN-786). Neither drift was visible from inside
either copy. A list written down three times drifts whatever the intentions are, so the commands
live in `tools/gates.sh` and these two places hold only a membership claim that a gate checks.

Those four numbers describe a state that no longer exists, which is the only kind of count that is
safe to write in prose here — it cannot go stale, because nothing will change it.

### `line-cite-check` goes red at a merge, and that is not a lane's regression

**Two branches that are each green alone can merge to a red `line-cite-check`, and neither branch
is at fault** (SKEIN-937). A citation is an address, so it is the one kind of claim that line
shifts break without anybody editing it. Lane A repairs its citations against its own line
numbers; lane B repairs its own against *its* line numbers; the merge carries both sets of shifts,
and now neither repair names the merged tree. The gate can only be red on a tree that exists after
the merge — **and the integrator is the only person who ever builds that tree.**

Measured twice in one afternoon on the same integration branch: **14 of 523 citations** at the
first merge point, and **81 of 523** at the next — 73 `moved`, 4 `unrecorded`, 3 `misanchored`,
1 `ambiguous`.

So:

* **Do not send it back to a lane.** The lane's tree really is green, and it will re-run its gates,
  find nothing, and say so. That round trip has been paid for once already.
* **The repair belongs on the integration branch, as its own commit** — separate from the merges,
  so a reader can see that the addresses moved and nothing else did.
* **Repair ONCE, on the complete tree, after the LAST merge.** Repairing after each merge is work
  the next merge invalidates: those 14 were repaired, and then 81 needed repairing anyway.
* **Measure on the tree you are about to repair, never on the previous one.** The second wasted
  trip was a repair briefed with the tally from the earlier merge point, which sent an agent
  hunting two findings that did not exist on the tree in front of it. The run is one command and
  it prints the verdicts; a count taken anywhere else is a guess about a tree that no longer
  exists.

Most of it is mechanical — `python3 tools/line-cite-check.py --relocate --write` moves the line
numbers the ledger can still resolve, and `--record` anchors citations the merge brought in new.
`misanchored` is the one that is not: it means no relocation names the row's words, so a person
reads the row (see the verdict guide the tool prints).

Where a gate is python, it is python because Rust cannot express what it checks. "This module may not depend on that
one" has no compiler behind it, so `module-check.py` **is** the compiler; the same argument makes
`source-check.py` the compiler for "nothing reaches anything except through a declared Source". A
law nothing checks is a paragraph.

**`residue-check.py` is the one to run before you push, not after:**

```sh
python3 tools/residue-check.py
```

Nothing that identifies a person, a client or an account gets back into this tree. CI runs it now
— it did not for most of this repository's life, which made the only check standing between a
prior client's names and a public git history the one that depended on somebody remembering. Run it
locally anyway, because this is the gate whose failure a red build cannot undo: by then the push has
happened, and a push cannot be unseen. Four of its five
rules are about *shape* — a host, a home directory, an email address, a credential prefix — each
with an allow-list in `docs/residue.toml` carrying a reason per entry, so a new host is a line in a
diff that somebody decided on. **It reads `git ls-files`**, so a file you have written but not
staged is invisible to it: `git add` first, or it will be green about a tree that does not include
your change.

The fifth rule is a literal denylist, and it is the one whose list is **not in this repository**.
Publishing the strings that were removed from every commit, each with a sentence saying whose it
was, is a search-term list pointed at anything that was never rewritten — so the register moved to
the project's shared store and the tree carries `docs/residue-banned.txt`, sha256 of each needle
(SKEIN-630). The gate enforces from those hashes, which is why it works in CI, where no shared
store exists. If you can reach the register, a finding is named and its reason quoted and
`--update` regenerates the hash file from it; if you cannot, every one of those says so rather
than behaving as though the list were empty. A red line you cannot read names the file and the
line — open it.

An entry in one of those allow-lists that nothing uses fails the build too. That is the same
bargain everywhere in this repository: an allow-list nobody prunes is a permission nobody granted.

Four of the gates read Rust source and need the same two cuts — comments are not code, and
`#[cfg(test)]` is not shipped. They share one reader, `tools/rustcut.py`, whose self-check runs on
every invocation of every gate (`grep -l '^import rustcut' tools/*.py` names all four). Do not
write a fifth cutter; the third copy counted braces without skipping strings and reported nothing
at all for `src/fleet.rs`, whose test module opens with a shell fixture full of braces — and the
gate saw none of them:

```sh
grep -rc 'set_var\|remove_var' src/fleet/*.rs | awk -F: '{s+=$2} END{print s}'     # → 234
```

### The gate that is not in CI: every test alone

```sh
python3 tools/alone-check.py
```

`cargo test` runs a crate's unit tests **multi-threaded in one process**, so what one test leaves in
that process is the next test's world. `env-lock-check.py` stops two tests *colliding*; nothing
stops one test **depending** on what another left behind, and a green suite cannot show you that,
because the thing it rests on is right there in the process. This runs every lib test in a process
of its own and fails on any that passes in the suite and fails alone.

It found fifteen (SKEIN-646), the day after `config::skein_home()` was made to refuse an unpinned
test and forty tests were fixed and the suite went green. Every one of the fifteen resolved a path
under the owner's live `~/.skein` and passed only because a neighbour had left `$SKEIN_HOME` set.
Two of them were containment tests computing their paths inside the live directory; one ran the real
login-share script against the real `/boxes`, and reached the arm it asserts only because a
*different* test removes `$HOME` from the process.

**Why it is not a CI step.** It needs the lib test binary built and then runs 988 processes: 24s at
the default `--jobs 8`, 98s at `--jobs 1`, on top of a build CI already does. That is affordable, and
the argument against adding it is not cost — it is that the finding is a property of the *tests*,
which changes when tests change, so the honest place for it is beside `residue-check.py` in the
pre-push list rather than as one more `- run:` nobody reads. Run it when you add or move a test,
and when a test starts passing for a reason you cannot name. If it does become a CI step, it belongs
after the `test` gate, reusing that job's build — which is where `tools/gates.sh` already puts it.

That exception is **declared, not remembered**: `alone-check`'s row in the list carries `no:` and the
reason, and `gate-list-check` fails if `ci.yml` starts running it or if any other gate stops being
run there. It used to be remembered, and for that reason it was also in the pre-push list, in the
table above, and in no workflow step at all — which is how nobody noticed (SKEIN-786).

`gate-list-check` asks `tools/` itself as well, which is the part that would have caught this rather
than reporting it after somebody looked. Three lists that agree with each other can agree perfectly
about a gate nobody ever wired up, so every `tools/*.py` has to be in the list or be declared in
`not_a_gate` with its reason. `rustcut.py` is the only declared one: four gates import it as their
shared Rust reader and its self-check runs inside each of them, so it has no verdict of its own.

It carries a self-check that runs on every invocation, in `rustcut.py`'s spirit: three fabricated
tests, one planted to fail alone and one that fails unless the runner set `$SKEIN_TEST` and stripped
`$SKEIN_HOME` and `$SKEIN_FLEET_ROOT` from the child. `--self-check` runs only
that, and says what it proved. A gate you can silence by exporting a variable is exactly the shape
this repository keeps getting bitten by.

## Before you change anything

Every rule below was bought with a real failure in this repository, and they share one shape: not a
bad edit, but **a wrong premise, confidently implemented.** They are worth more than the diff you
came to write. Each one names the incident that bought it, because the incident is the part you can
argue with — a prohibition on its own is just something to route around.

1. **Find out what was already decided.** A whole fleet-migration path was built here — a config
   field, mount dispatch, and a rebuild descriptor threaded through `resize_fleet_inner` — against a
   design that had been settled two days earlier, and it was reverted in full. The settled design
   was: download one file, run one sandbox command, build and run nothing on the host. There is no
   host-side migration anywhere in that flow, so the feature had nowhere to live. Two working notes
   already said so, and one of them said in as many words that the smaller question had been
   answered first once before and had to be corrected.

   Read the design documents and search the tracker before you build, not after — and before you
   file something, too. Two items in this tracker are duplicates of a decision that was already
   written down, filed by someone who searched afterwards.

2. **Trace the whole path before you call anything dead.** Never conclude "nothing calls this" from
   a truncated search. Count the result set first, then read all of it:

   ```sh
   grep -rn "the_symbol" src/ tests/ | wc -l
   ```

   `prq::submit_review_with_comments` was reported as dead code twice over: once from a
   `grep … | head -20` whose output was cut before the production caller, and once from a scan
   restricted to a single module when the caller was in another. It is live — it posts a reader's
   line notes together with their verdict — and cutting it would have broken line notes. Both
   reports were confident and both were produced in under a minute.

   When the question is "is this reachable", the answer needs the whole result set, and the
   production/test split made explicit. A `#[cfg(test)]` boundary is not a call graph.

3. **A test you cannot make fail is worse than no test.** Before writing the assertion, name the
   concrete change that would make it fail. Write that sentence down; if you cannot name one, the
   assertion is decoration, so delete it or rewrite it. Then prove it: break the behaviour, watch
   *that named assertion* fail, restore, and say in the commit message that you did. Restore from a
   copy you made yourself rather than from git — `git restore` takes the whole file and silently
   drops unrelated work — and verify the restore with `md5sum`.

   Two tests here could not fail, found on the same day.
   `a_supervisor_whose_fleet_is_gone_stops_rather_than_restarting_for_ever` asked `tmux has-session`
   about a session whose socket lives *inside* the directory the test deletes. The missing socket
   answered "No such file or directory", a missing socket read as a dead session, and the test went
   green in 0.26s with 105 leaked processes still spinning. It was fixed by counting processes, and
   by asserting they were there **before** the deletion — an absence that was never a presence
   proves nothing. `nothing_the_sandbox_builds_skein_with_is_writable_by_a_box` asked whether a path
   was under a `share_paths` entry; those entries are `$HOME`-relative and the fleet root is not
   under `$HOME`, so the two can never overlap however wrong the placement gets. It passed just as
   happily with a deliberately wrong path added.

   Prefer asserting a **property of the real mechanism** over a property of a string. The placement
   test above is worth little beside `tests/isolation_bwrap.rs`, which runs actual bwrap and reads
   the resulting paths back.

4. **Never send a field you did not mean to change.** Read-modify-write, or omit the field —
   placeholder values in a write call are how records get erased. An attempt to clear one work
   item's parent sent `{"parent": null, "name": "…", "description_html": "unchanged"}`, because the
   schema listed those as required. Had it not failed on the parent field, it would have replaced
   that item's entire body — the settled design from rule 1, some 1,800 words — with the word
   "unchanged". The schema's `required` was the *create* schema; a partial update was accepted fine.

5. **Commit by explicit path, and do not stage early.** `git add -A` from the repo root once swept a
   whole scaffolded store into an unrelated commit — `settings.json` and 28 files under `skein/`,
   none of them the author's — and `.gitignore:18-25` is the block written because of it. It
   happened again to work in progress rather than to generated files: an `add -A` swept seven of
   another author's half-written test files into a commit about something else entirely.

   Explicit paths are not enough on their own, because `git add` writes to the repository's one
   shared index, and from the moment one author stages, another's commit can carry that content if
   the paths overlap. Two authors keeping to deliberately disjoint file sets still collided: one
   staged its six files early — `residue-check.py` reads `git ls-files`, so a new file is invisible
   to it until staged — and before it committed, the other committed one of the two overlapping
   files by explicit path, correctly by its own brief, and carried two staged lines away with it.
   The code was right at `HEAD` afterwards and only the authorship was wrong, which is luck rather
   than design. **Stage and commit in one breath**, or use `git add -N`, which lets the gate see a
   new file without its content entering the index.

6. **Structural cuts: snapshot, match at the symbol's own indent, check the delta.** Before deleting
   a function or a block: copy the file somewhere of your own first (not git — you will want the
   unrelated work back); brace-count from the signature and stop at the **signature's own
   indentation**, never at column 0; print the first, last and following lines of the span and read
   them before applying; then check the line-count delta is the one you predicted. A brace-matcher
   that walked to the first `}` at column 0, applied to an *indented* function, deleted 558
   unrelated lines of the server binary — an entire module of review routes. The file had not been
   copied first, so recovery cost the rest of that session's work on it.

7. **A heredoc eats the `\` that holds a Rust sentence together.** rustfmt will not break a string
   literal, so every long sentence in this tree is written across two source lines with a trailing
   backslash, and rustc drops the backslash, the newline and the next line's indentation:

   ```rust
   "the largest boxes are {named} — `skein stop <box>` keeps its checkout, branch and \
    conversation, or clear its build output in place"
   ```

   Write that same edit through an **unquoted** heredoc — `cat > src/health/disk.rs <<EOF`, the way an
   agent patches this repository all day — and the shell takes the backslash as *its own* line
   continuation and joins the two lines before the file is ever written. The `\` never arrives. What
   lands is one long line with the continuation line's indentation still inside the literal, and
   `skein doctor` prints "branch and&nbsp;&nbsp;…(18 spaces)…&nbsp;conversation" to the fleet's
   owner. Fifteen of these were in the tree when they were counted, in nine files, and two more were
   nearly added the same way while fixing something else (SKEIN-741, SKEIN-735).

   It survives review because a diff shows nothing: one line replaced by one line, the words
   unchanged, the gap indistinguishable from indentation in most viewers. So:

   * quote the delimiter — `<<'EOF'` — which stops the shell touching backslashes at all, or write
     `\\`, or use a tool that does not go through a shell;
   * `python3 tools/continuation-check.py` after any heredoc patch to a `.rs` file. It refuses to
     run rather than pass when it derives no files or reads no string literals, and it tells prose
     from the deliberate column alignment in `src/bin/skein.rs` and `src/signals.rs` by measuring
     how much of a sentence stands in front of the gap — `--show` prints the margin.

## Commit messages

Conventional commits, `type(scope): …`, and 875 of the 878 in this history match that shape
(`git log --format='%s' 02ad7cfb | grep -cE '^[a-z]+(\(.+\))?!?: '`). The types in use, most to
least common, are `fix`, `feat`, `docs`, `refactor`, `test`, `perf`, `chore`, `build`, `ci`,
`style`, and one each of `wip` and `tools`, which are the right shape and not conventional types.
The three that are not the shape at all are two merges and one subject whose type has a space in
it (`git log --format='%s' 02ad7cfb | grep -vE '^[a-z]+(\(.+\))?!?: '`).

Every count in this section names `02ad7cfb`, because a count of a growing history is wrong by the
next push otherwise. Naming the commit makes them reproduce for good; the version that did not is
how the ones above them came to be off by twenty-three.

The **subject** is the part a contributor cannot guess, so read twenty of them before you write
one:

```sh
git log -20 --format='%s'
```

The shape is: **a sentence in the present tense saying what is now true for a user, not what moved
in the code.** Lower case after the colon, no full stop at the end (none of the 878 has one), often
two clauses joined by "and", and long — the median is 75 characters and the longest is 196, because
naming the behaviour precisely matters more than fitting 50 columns.

```
feat(gitgate): a stored token covers exactly one repo, enforced where it is used
fix(isolation): a path and a request id stay inside their own quotes
test(isolation): the cover is proved by running bwrap, not by reading its arguments
```

Not `feat(gitgate): add per-repo token scoping`. The difference is that the first says what is true
afterwards and the second says what the author did.

The **body** is where the argument goes, and it is expected to be long. What belongs in it: the
wrong premise the change corrects, cited to a file and line; what you *proved* rather than assumed,
and how; what you deliberately did not do. Bodies here routinely run twenty lines and occasionally
a hundred, and that is the house style rather than an excess. If a test changed behaviour, say
which sabotage you ran and what message it produced.

A subject may end with a tracker reference in parentheses — `(SKEIN-576)` — where one exists. Only
20 of 878 carry one, so its absence is normal.

## Opening a pull request

Run everything before you open it:

```sh
tools/gates.sh
```

That is every gate including `alone-check`, which CI does not run — see "The gate that is not in CI"
above. It matters most when you added or moved a test. This used to be a block of nine commands
copied out of the workflow, and keeping a copy of a list in the document that tells people to run
the list is how the two stopped agreeing.

Read the last line before you quote it. `ALL GATES GREEN at <sha>` names the commit the gates
actually ran against; a `RESULTS REFUSED` means somebody wrote to the worktree while the run was in
flight and there is no verdict to quote.

**If you redirected the run to a file, check the file before you quote its footer:**

```sh
tools/gates.sh > /var/tmp/my-gates.log 2>&1
tools/gates.sh --verify /var/tmp/my-gates.log
```

Reading the last line tells you what that line says. `--verify` answers the question the line
cannot: **are the lines above this verdict the same run's?** Every line of a run carries that run's
id and the footer states how many stamped gate lines precede it, so `--verify` can see a file two
runs wrote into (NUL bytes, a second verdict, a foreign stamp, a gate line missing) — and then it
leaves the stream entirely and matches the receipt in that run's own log directory, which is
namespaced per sha and per worktree. Two worktrees sharing one log file is not hypothetical; it is
what SKEIN-903 was, and the verdict a reader quoted named a run they had not made. Quote a footer
only after `--verify` says the file is one whole run.

If you touched the page, run the browser suite for what you touched as well. The page's sources are
`src/web/app/`, and `node cockpit/build.mjs` assembles them into `src/web/index.html`; both it and
the cockpit bundle are embedded in the binary and `cargo build` does not run node, so a stale one is
a cockpit quietly serving last week's code.

For anything larger than a bug fix, **open an issue first**. The design here is argued in four
documents that are meant to be read together, and the second standing rule of the project is that
a feature which cannot be written as a composition of the five primitives means the primitive set
is wrong — and the fix is then the primitive set, not a mechanism beside it. That is a conversation
worth having before the code, not after.

Found a security issue? Do not open a pull request for it. [`SECURITY.md`](SECURITY.md) says where
to send it.

## Releases, and what CI covers

**Linux only, on purpose.** skein runs inside an sbx sandbox, and a sandbox is a Linux machine
whatever the host is — so Linux is what executes it, and Linux is what CI builds and tests. There is
no macOS job and no macOS binary, and nothing is published to crates.io. The suite does run on a Mac
with the exceptions in `GATED` (see the README's Build section), but no CI run stands behind that.

**What CI runs.** `.github/workflows/ci.yml`, on every branch push and pull request, on
`ubuntu-latest`:

- the `check` job: every gate `tools/gates.sh` marks for CI, one step each. That is 18 of the 19;
  `alone-check` is the one it does not run, for the reason under "The gate that is not in CI"
  above, and `gate-list-check` fails if that ever stops being the only one.
- the `msrv` job: `cargo build --locked --all --all-targets` on the toolchain `rust-version` in
  `Cargo.toml` names. That version was measured, not picked, and the comment beside it says how;
  the job reads it from `Cargo.toml`, so the number exists once. It is a build, not a second run of
  the gates — though `clippy` in the `check` job also reads it, and fails on an API newer than it.
- the `coverage` job: line coverage, which fails a change that lowers it below the floor.

**The coverage floor.** `tools/coverage-check.py` measures two numbers and compares each with its
floor in `docs/coverage-floor.toml`: `rust`, the line coverage of `src/` and `warden/src/` over the
whole `cargo test --all --no-fail-fast` run under `cargo llvm-cov` (library, binaries and every
integration binary, including the servers they spawn; `tests/` itself and doctests are not
counted), and `cockpit`, the line coverage of `cockpit/src` from the `cockpit-tests` run under
node's `--experimental-test-coverage`. Each floor is the number measured when it was set, rounded
**down** to a whole percent — not a target anybody chose — and it only ever goes up. When a run
measures a point or more above it, the job says so and names the value; raising it is a one-line
change to that file in your own commit, never something CI writes. Run it yourself with
`python3 tools/coverage-check.py cockpit` (seconds) or `python3 tools/coverage-check.py rust`
(`rustup component add llvm-tools-preview` and `cargo install cargo-llvm-cov --locked` first; it is
a second full build and test run). It is a job rather than a gate in `tools/gates.sh` for that last
reason: every local run of the gates would pay for it.

**Cutting a release** is pushing a `v*` tag whose version matches `version` in `Cargo.toml`.
`.github/workflows/release.yml` then runs the whole of `ci.yml` on the tagged commit — the tag push
does not also run it separately — and only if that is green does it build `skein` and
`skein-server` for `x86_64-unknown-linux-gnu` and `aarch64-unknown-linux-gnu`, run each one's
`--version`, and attach one archive per target, with its SHA-256, to a GitHub release. It refuses a
tag that disagrees with `Cargo.toml`, and a binary that reports a dirty tree. The binaries link
glibc from `ubuntu-24.04`, so they need 2.39 or newer. `skein-warden` is not in the archive: it runs
on the host, not in a box, and building it stays `cargo build --release --workspace`.

## Licence

skein is dual-licensed under [MIT](LICENSE-MIT) and [Apache 2.0](LICENSE-APACHE), at your option.
Unless you state otherwise, any contribution you intentionally submit for inclusion in this
repository, as defined in the Apache-2.0 licence, is licensed on those same terms, with no
additional conditions.
