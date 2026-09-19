# What a box can and cannot reach

A box runs somebody's coding agent, and skein treats it as hostile. This page says what one ordinary
box can reach and what it cannot, in one table. It covers the code as it stands. The reasoning
behind each boundary is in [`architecture.md`](architecture.md) §9. How to report a boundary that
does not hold is in [`SECURITY.md`](../SECURITY.md).

**Every row is backed by a test, a line of code, or a command you can run.** A claim that cannot
be backed is not in the table; it is under [Open](#open), with its tracker item. The evidence
column says which kind of backing a row has, strongest first:

* **bwrap test**: a test in `tests/isolation_bwrap.rs` that builds a fleet-shaped tree, runs
  the launcher's own isolation block under a real `bwrap`, and reads the path back from inside. It
  lifts the block out of `src/box-session.sh` instead of copying it, so a change to the launcher is
  a change to what the test runs. It skips, and says so, on a machine where bwrap cannot make a
  user namespace.
* **test**: a unit or integration test of what the launcher builds, not of what a namespace sees.
* **code**: the line that does it. Nothing runs it against a namespace.

## Where a box stands

A box is the sandbox's filesystem (`--dev-bind / /`, `src/box-session.sh:2404`) with a set of
covers applied, run by `bwrap` as the same uid as every other box. It gets its own
mount namespace and its own user namespace, and it shares the network, PID and IPC namespaces with
the whole sandbox: `exec bwrap` asks for no `--unshare-*` flag
(`sed -n '/^exec bwrap/,/^  --$/p' src/box-session.sh`), and `src/box-session.sh:2356` says why
the PID namespace stays shared. **So the boundary is files, not control**, as architecture §9.2
says, and the rows below are mostly about files for that reason.

## The table

| a box… | reach | evidence |
|---|---|---|
| its own checkout | read-write | bwrap test `a_box_run_under_bwrap_can_reach_its_own_repo_and_no_one_elses` |
| its own repo's store (memory, mailbox, skills) | read-write | same test |
| its own state directory, including the git token the host minted for it | read-only; its conversation is writable through a separate bind at `$HOME` | same test |
| another box's checkout, conversation or git tokens | **no** | same test, and `src/box-session.sh:1532`, `src/box-session.sh:1640` |
| another repo's store, or any other host path the sandbox mounts | **no** | same test; the per-mount cover is `src/box-session.sh:1688` |
| the fleet's credentials on the volume: `credentials/`, `github-pats/`, `api-token`, the warden's secret | **no** | bwrap test `a_box_on_a_mounted_volume_cannot_read_the_fleets_credentials` |
| `.skein/private/`: the fleet agent's token and the review call's GitHub token | **no** | bwrap test `a_box_cannot_read_what_skein_keeps_under_private`; cover at `src/box-session.sh:1575` |
| the fleet agent's socket | **no**, `connect()` refused | bwrap test `a_box_cannot_connect_to_the_fleet_agents_socket` |
| the cockpit's tmux socket, at the path current code puts it | **no**, `connect()` refused; see SKEIN-831 under Open for fleets created earlier | bwrap test `a_box_cannot_connect_to_the_fleets_tmux_socket` |
| the launcher, the credential helper, skein's source and toolchain under `.skein` | read-only | bwrap test `a_box_can_read_what_skein_was_built_from_and_cannot_write_it` |
| its own git-write and package request queues | writes its own; reads every other box's; writes no other box's | bwrap test `a_box_can_write_its_own_request_queue_and_no_other_boxs` |
| `/run/user/<uid>` and `/run/secrets` | **no**, a private tmpfs per box | bwrap test `nothing_but_the_socket_directory_comes_through_the_run_cover` (the first); test `the_launcher_covers_what_run_shares_and_names_what_it_does_not` in `src/cockpit.rs` (both); `src/box-session.sh:1752`, `src/box-session.sh:1753` |
| **other boxes' agents, by message** | **yes**, on by default, per repo; no approval gate, since `crossSessionInbound` is seeded to `accept` (`src/box-session.sh:1179`) | bwrap test `a_box_is_never_discoverable_on_a_socket_it_cannot_reach`; off is `src/box-session.sh:1816` |
| **the shared toolchains `~/.local`, `~/.cargo`, `~/.rustup`, `~/.npm`** | **read-write, and shared with every box**. `~/.local/bin` is first on every box's `PATH` (`src/box-session.sh:113`), so one box can replace a binary the others run. See SKEIN-963 | code: `src/box-session.sh:735`, `src/box-session.sh:1202` |
| a binary planted in `~/.local/bin`, run by skein at fleet scope | **no**, fleet-scope scripts and crossings do not resolve through it | tests `a_planted_binary_is_not_what_a_fleet_scope_script_runs`, `a_planted_nsenter_is_not_what_a_crossing_runs` |
| **the sandbox's Docker daemon, `/run/docker.sock`** | **yes, deliberately.** It gives a root container in the sandbox with any bind mount, which reaches every box's files, the fleet root and the volume. **Every "no" above holds only for a box that does not use it.** | code: `grep -n 'docker.sock' src/box-session.sh` finds only the comment saying it is left open, and `the_launcher_covers_what_run_shares_and_names_what_it_does_not` keeps that comment there; the decision is architecture §9.5 R11 (`docs/architecture.md:1722`) |
| every TCP port in the sandbox, including the cockpit's API | **yes, it can connect**. The API needs the token, which is in the "no" rows above | code: no `--unshare-net`; `src/apiauth.rs:56` is the one off-switch (SKEIN-962) |
| the host's `ssh-agent` through `$SSH_AUTH_SOCK` | **no**, a regular file is bound over the socket (scoped boxes) | code: `src/box-session.sh:2002`, under `src/box-session.sh:1941` |
| the host's `ssh-agent` through the gateway on TCP 3129 | **yes**, left reachable by owner decision (SKEIN-929); usable if the host agent holds a key | architecture §9.6 |
| GitHub, from a scoped box's `git` and `gh` | only with the box's own token: the GitHub hosts go into `NO_PROXY`, so the sandbox proxy cannot inject the account's credential | test `a_scoped_box_routes_github_direct_and_a_fleet_box_does_not` (`tests/git_write_request.rs`); live test `tests/github_reach_live.rs`, ignored unless `SKEIN_LIVE_FLEET=1` |
| the rest of the internet | **not bounded by skein** (SKEIN-926) | `grep -rn 'sbx policy' src`: every hit is advice printed for a person, and none sets a policy |
| the fleet's canonical agent login | cannot replace it; a box's login moves up only when the fleet holds none (`src/box-session.sh:979`) | code |

**Two kinds of box get less of this, and both say so when they start.**

* **The workshop box** (`SKEIN_BOX_PRIVILEGED=1`) skips the whole isolation block
  (`src/box-session.sh:1485`), so every "no" in the file rows becomes "yes", including the fleet
  agent's token. It is the one deliberate escape hatch; architecture §9.2 path 3 states its terms.
  bwrap test `the_workshop_box_sees_what_an_ordinary_box_cannot`.
* **A box with no mount manifest**, because skein matched it to no repository or its launcher
  predates the manifest, can read other repos' stores and the fleet's credentials on the volume.
  Other boxes' checkouts and state, and `.skein/private/`, are still covered. bwrap test
  `a_box_with_no_mount_manifest_is_uncovered_and_a_matched_box_is_not`; the start-up banner is
  asserted by `an_unmatched_box_announces_that_it_is_uncovered_and_a_covered_box_says_nothing`.

## Open

Each of these either has no evidence strong enough for a row, or is a known gap in a row above. The
tracker item is the record; this list only points at it.

* **SKEIN-926**: the sandbox reaches arbitrary internet hosts directly. skein sets no egress
  policy, so a box reaches whatever the host's `sbx` policy allows, and on the fleet where this
  was measured that was everything tried.
* **SKEIN-927**: the sandbox proxy can inject the account's GitHub credential for a request that
  goes through it. Scoped `git` and `gh` go around it, but a process that points itself back at
  the proxy, or clears `NO_PROXY`, is back on it. skein does not yet detect injection at start.
* **SKEIN-831**: a fleet that was serving before the socket move keeps the cockpit's tmux socket at
  the old path in the readable half of `.skein`, where a box can `connect()` to it. A tmux client
  can make the server run commands, so that is fleet-scope execution. It lasts until that fleet's
  tmux server restarts.
* **SKEIN-940**: a box can write `"state": "granted"` into its own git-write request, and the
  cockpit then shows the request as answered and offers no button. No grant is recorded, so the box
  gains nothing, but the owner is never asked.
* **SKEIN-947**: `/api/path` answers whether any host path exists. It needs the API token, so it
  is not a box's reach unless the token has leaked, but it answers for the whole host
  filesystem.
* **SKEIN-960**: on some live boxes the agent's own config trusts the filesystem root. For a
  working directory outside a git repository, that runs a folder's `.claude/settings.json` hooks
  without asking. skein did not write the entry.
* **SKEIN-961**: `SECURITY.md` lists reaching the Docker socket as in scope, but the table above
  shows every ordinary box reaches it by design. One of the two has to change.
* **SKEIN-962**: architecture §9.4 says `SKEIN_NO_API_AUTH` is refused in-fleet. No code refuses
  it.
* **SKEIN-963**: the shared `~/.local` row above breaks architecture §9.2's rule that no shared
  writable path holds anything another box executes.
* **SKEIN-964**: architecture §9.4 says a box can signal skein and other boxes through the shared
  PID namespace. The premise is in the code, but no test sends a signal, so it is not a row.

## Checking this page

```sh
cargo test --test isolation_bwrap        # the bwrap rows; a skip names the check that did not run
python3 tools/line-cite-check.py         # every file:line above still says what it said when cited
python3 tools/prose-check.py             # every test and function named above still exists
```

When a cover changes, change the row in the same commit. A row that is no longer true is worse
than a missing one, because a reader trusts it.
