# A test that has not pinned its paths refuses to run

**Decision.** In a test process, skein's two root paths have no default. `$SKEIN_HOME` and
`$SKEIN_FLEET_ROOT` must be set to the test's own directories; a test that resolves either without
setting it panics instead of falling back to the real one. Every test must also pass alone, in a
process with no neighbours.

**Date.** 2026-09-05 to 2026-09-07, over three incidents.

**Decided by.** Settled by the incidents below.

**Why.** On a machine that runs skein, the fallback paths are the live fleet and the live store, and
the code under test is the code that operates both. Any default path in a test is a live action.

- Five tests set `$SKEIN_HOME` and not `$SKEIN_FLEET_ROOT`. Harmless until an agent's address moved
  to a function that reads the fleet root, which then defaulted to the live one: the tests ran their
  scripts at fleet scope, overwrote four installed scripts with uncommitted working-tree versions,
  and left two fleet agents running side by side.
- A fixture that set neither variable wrote a `resume.log` into the live store's per-box state,
  through the store path's fallback. Deleted, it came back within the hour from an ordinary
  `cargo test`. No search for tests that set `SKEIN_HOME` could have found it.
- Running every test in its own process then found fifteen that passed only on a neighbour's leaked
  `$SKEIN_HOME`, two of them containment tests computing paths inside the live store. A parallel
  run samples one interleaving; a green suite is not evidence anything is pinned.

The marker that says "this is a test" comes from cargo's `[env]` table, so a plain `cargo test`
carries it with nothing to export — deliberately, because the write came from a run by someone told
to export nothing. `cfg!(test)` was rejected: it is false inside the library when the library is
linked into a `tests/*.rs` binary, which is where the integration suites that drive the most
machinery run.

**Rules out.**

- A fallback to a real path from any test process.
- Trusting a green parallel run as evidence of isolation.

**Enforced at.**

- `src/util.rs:66-78` — `fleet_root` refuses an unset `$SKEIN_FLEET_ROOT` in a test.
- `src/config.rs:25-30` — `skein_home` refuses an unset `$SKEIN_HOME` in a test.
- `src/util.rs:39` — `in_test`, the predicate both guards read.
- `.cargo/config.toml:13-14` — the marker every `cargo test` carries.
- `tools/alone-check.py` — the gate that runs every lib test alone.
