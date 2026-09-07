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

* **Without Playwright's chromium, six browser suites do not open a page.** See below.

## Setting up

```sh
cargo build --release --workspace
```

`--workspace` and not a bare `cargo build`: the workspace's second crate is `warden/`, which
produces `skein-warden`, and the fleet cannot be created or resized without it. The warden is a
separate crate deliberately — architecture §14 gives it an empty depends-on column, because it
exists to authorise the privileged operations skein is not trusted to authorise for itself, and a
shared library would be a shared blast radius.

The browser tier needs a one-time download:

```sh
cd tests/ui && npm run setup     # npm install, then playwright's chromium (~150 MB, cached)
```

Skip it and `cargo test` still passes — it reports that the browser suites were skipped and names
the command that installs them. That is deliberate: a 150 MB download is not a reasonable build
dependency for somebody fixing a typo. **CI is not somebody fixing a typo**, and the workflow now
installs it, so the tier that opens a page runs there whether or not you ran it here. It did not
until SKEIN-567, and every green run before that had skipped all six.

## Running the tests

**Run this, not `cargo test`:**

```sh
env -u SKEIN_IN_FLEET cargo test --all --no-fail-fast
```

Both halves of that are load-bearing.

`--no-fail-fast`, because **`cargo test` stops at the first test *binary* that fails** and there
are 37 of them. `tests/browser_suites.rs` sorts fourth of the 29 in `tests/`
(`ls tests/*.rs | sort`), so a single red browser suite means the report says nothing whatever
about the twenty-five after it. That is not
hypothetical: master was pushed red at `d5d0e95` on a local run that stopped inside
`browser_suites`, hiding a second broken gate that CI — fail-fast too, at the time — then found
while still not reaching a third. One run that reports everything beats two that each report the
first thing, which is the argument for the flag in both places: CI passes it now as well.

`env -u SKEIN_IN_FLEET`, because that variable is set inside every skein box, and a `cargo` that
inherits it hands the tests a `skein` that believes it is running inside the fleet.

There is a third hazard worth knowing before you write a test that starts a server: **pin
`$SKEIN_FLEET_ROOT` as well as `$SKEIN_HOME`.** `config::fleet_root` falls back to `/boxes` when
the first is unset, and on a machine that is running skein that is a *real* fleet. Suites that
pinned only `$SKEIN_HOME` read placement records and gitgate requests out of whoever's fleet
happened to be running — see the note in `tests/ui/README.md`.

**The `$SKEIN_HOME` half of that is now enforced, and it is not only about servers.**
`config::skein_home` panics rather than answering a test that has not pinned it. The marker is an
environment variable — `.cargo/config.toml` puts `SKEIN_TEST=1` in the `[env]` table, so every
`cargo test` run from this tree carries it — and deliberately not `cfg!(test)`, which is false
inside the library when it is linked into a `tests/*.rs` binary and would therefore be absent from
the suites that drive the most machinery (`config::TEST_MARKER` says this at more length;
`tests/harness.rs` asserts the marker actually arrives). It exists because a unit test with no
server anywhere in it wrote `boxes/box-route/resume.log` into the owner's live `~/.skein`, beside
the state of sixteen real boxes, and no search could have found it: that test pinned *neither*
variable, so it matched no grep for either (SKEIN-626). Forty tests across thirteen modules and
three integration binaries were resting on the fallback when the guard went in.

`$SKEIN_FLEET_ROOT` is deliberately **not** guarded the same way: `config::fleet_root`'s own doc
records that ~29 of its readers interpolate the root into a string they never act on, and failing
all of them for a hazard none of them has is how a guard gets reverted. Pin it anyway — the
paragraph above is still the rule — but nothing will stop you.

### The cockpit's own suites

28 suites live in `tests/ui/`, in two tiers, and `tests/browser_suites.rs` is what invokes them
from `cargo test`:

| tier | count | needs | what it is for |
|---|---|---|---|
| node | 22 (`NODE_SUITES`) | node only | the page's pure functions, lifted out and run directly |
| browser | 6 (`BROWSER_SUITES`) | chromium | a real page, asserting what is **visible** |

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

`.github/workflows/ci.yml` has fifteen `- run:` steps. Four prepare the machine, one proves bwrap
actually works, and **ten are gates that can fail your change**:

```sh
grep -c '^      - run:' .github/workflows/ci.yml     # → 15
```

| gate | what it enforces | where the exceptions are declared |
|---|---|---|
| `cargo fmt --all -- --check` | formatting | — |
| `cargo clippy --all-targets --all -- -D warnings` | lints, both crates | — |
| `cargo test --all --no-fail-fast` | the suite, every binary | — |
| `python3 tools/module-check.py` | the module graph of architecture §14 | `docs/modules.toml` |
| `python3 tools/source-check.py` | the Source law of §2.3 | `docs/sources.toml` |
| `python3 tools/env-lock-check.py` | no `set_var` outside `env_lock()` | `docs/env-lock.toml` |
| `python3 tools/prose-check.py` | every backticked symbol in prose exists | `docs/prose-symbols.toml` |
| `python3 tools/residue-check.py` | no identifier from before this repository | `docs/residue.toml`, `docs/residue-banned.txt` |
| `node --test "cockpit/test/*.test.mjs"` | the cockpit's pure functions | — |
| `node cockpit/build.mjs --check` | the committed bundle is not stale | — |

Five of those are python because Rust cannot express them. "This module may not depend on that
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
grep -c 'set_var\|remove_var' src/fleet.rs     # → 183
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
pre-push list rather than as a sixteenth `- run:` nobody reads. Run it when you add or move a test,
and when a test starts passing for a reason you cannot name. If it does become a CI step, it belongs
after `cargo test --all`, reusing that job's build.

It carries a self-check that runs on every invocation, in `rustcut.py`'s spirit: three fabricated
tests, one planted to fail alone and one that fails unless the runner set `$SKEIN_TEST` and stripped
`$SKEIN_HOME`, `$SKEIN_FLEET_ROOT` and `$SKEIN_IN_FLEET` from the child. `--self-check` runs only
that, and says what it proved. A gate you can silence by exporting a variable is exactly the shape
this repository keeps getting bitten by.

## Before you change anything

Every rule below was bought with a real failure in this repository, and they share one shape: not a
bad edit, but **a wrong premise, confidently implemented.** They are worth more than the diff you
came to write.

1. **Find out what was already decided.** A whole fleet-migration path was built here against a
   design that had been settled two days earlier, and reverted in full. Read the design documents
   and search the history before you build, not after.

2. **Trace the whole path before you call anything dead.** Never conclude "nothing calls this" from
   a truncated search — count the result set first (`grep -rn "the_symbol" src/ tests/ | wc -l`)
   and then read all of it. A function was reported as dead code twice over: once from a
   `grep | head` cut before the production caller, and once from a scan of the wrong file. It was
   live, and cutting it would have broken line notes.

3. **A test you cannot make fail is worse than no test.** Before writing the assertion, name the
   concrete change that would make it fail. Then prove it: break the behaviour, watch *that named
   assertion* fail, restore, and say in the commit message that you did. Two tests here could not
   fail — one asked `tmux has-session` about a socket inside the directory it deletes, and a
   missing socket read as a dead session; one compared a `$HOME`-relative list against a path that
   can never be under `$HOME`. Both were green and both were decoration. Prefer asserting a
   property of the real mechanism over a property of a string.

4. **Never send a field you did not mean to change.** Read-modify-write, or omit the field.

5. **Commit by explicit path.** A `git add -A` from the repo root once swept a whole scaffolded
   store into an unrelated commit — `settings.json` and 28 files under `skein/`, none of them the
   author's — and `.gitignore:17-24` is the block written because of it.

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
cargo fmt --all -- --check
cargo clippy --all-targets --all -- -D warnings
env -u SKEIN_IN_FLEET cargo test --all --no-fail-fast
python3 tools/module-check.py && python3 tools/source-check.py
python3 tools/env-lock-check.py && python3 tools/prose-check.py
python3 tools/residue-check.py
python3 tools/alone-check.py
node --test "cockpit/test/*.test.mjs" && node cockpit/build.mjs --check
```

`alone-check.py` is the one of those CI does not run — see "The gate that is not in CI" above. It
matters most when you added or moved a test.

If you touched `src/web/index.html`, run the browser suite for what you touched as well; the
cockpit bundle is embedded in the binary and `cargo build` does not run node, so a stale bundle is
a cockpit quietly serving last week's code.

For anything larger than a bug fix, **open an issue first**. The design here is argued in four
documents that are meant to be read together, and the second standing rule of the project is that
a feature which cannot be written as a composition of the five primitives means the primitive set
is wrong — and the fix is then the primitive set, not a mechanism beside it. That is a conversation
worth having before the code, not after.

Found a security issue? Do not open a pull request for it. [`SECURITY.md`](SECURITY.md) says where
to send it.

## Licence

skein is dual-licensed under [MIT](LICENSE-MIT) and [Apache 2.0](LICENSE-APACHE), at your option.
Unless you state otherwise, any contribution you intentionally submit for inclusion in this
repository, as defined in the Apache-2.0 licence, is licensed on those same terms, with no
additional conditions.
