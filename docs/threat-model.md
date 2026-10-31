# What a box can and cannot reach

A box runs somebody's coding agent, and skein treats it as hostile. This page says what one ordinary
box can reach and what it cannot, in one table. It covers the code as it stands. The reasoning
behind each boundary is in [`architecture.md`](architecture.md) §9. How to report a boundary that
does not hold is in [`SECURITY.md`](../SECURITY.md).

**Every row is backed by a test, a line of code, or a command you can run.** A claim that cannot
be backed is not in the table; it is under [Open](#open), with its tracker item. The evidence
column says which kind of backing a row has, strongest first:

* **bwrap test**: a test in `tests/isolation_bwrap/` that builds a fleet-shaped tree, runs
  the launcher's own isolation block under a real `bwrap`, and reads the path back from inside. It
  lifts the block out of `src/box-session.sh` instead of copying it, so a change to the launcher is
  a change to what the test runs. It skips, and says so, on a machine where bwrap cannot make a
  user namespace.
* **test**: a unit or integration test of what the launcher builds, not of what a namespace sees.
* **code**: the line that does it. Nothing runs it against a namespace.

## Where a box stands

A box is the sandbox's filesystem (`--dev-bind / /`, `src/box-session.sh:3885`) with a set of
covers applied, run by `bwrap` as the same uid as every other box. It gets its own
mount namespace and its own user namespace, and it shares the network, PID and IPC namespaces with
the whole sandbox: `exec bwrap` asks for no `--unshare-*` flag
(`sed -n '/^exec bwrap/,/^  --$/p' src/box-session.sh`), and `src/box-session.sh:3821` says why
the PID namespace stays shared. **So the boundary is files, not control**, as architecture §9.2
says, and the rows below are mostly about files for that reason.

## The table

| a box… | reach | evidence |
|---|---|---|
| its own checkout | read-write | bwrap test `a_box_run_under_bwrap_can_reach_its_own_repo_and_no_one_elses` |
| its own repo's store (memory, mailbox, skills) | read-write, for every box of the repo and not only this one. So skein runs its own scripts from elsewhere, with one exception at the end of this row. The turn-state hooks load from skein's plugin (SKEIN-1062) and run the plugin's own copies of their scripts (SKEIN-1144). What a box runs as it starts is also the plugin's copy: the kit's helpers, Codex's hook wiring and its installer, the attach shell's and the tracker's helpers, and the renderer of skein's default status line (SKEIN-1149). The copies still under `skein/bin/` are run by nothing skein starts. The store's `settings.json` and `settings.local.json` are no box's settings, so a status line or hook a box writes there runs nowhere (SKEIN-1153). A box's `.claude` is a directory of its own: where the repo tracks nothing there, the store's `skein/` and what Claude Code reads from a `.claude` (`skills/`, `commands/`, `agents/`, `output-styles/`, `rules/`) are linked into it one by one, and nothing else of the store's, and a box whose `.claude` was the store's own link is converted as it starts (SKEIN-1053, SKEIN-1330). The store itself is `store/data`, not `store/.claude` (SKEIN-1330, `docs/decisions/store-is-not-dot-claude.md`), and a box's memory is that store's `memory/` through `autoMemoryDirectory` in its local settings. Its settings are its untracked `.claude/settings.local.json`, where the kit writes skein's defaults from the read-only plugin and copies nothing from the store; that file also overrides the old default status line a past kit copied into a repo's own `settings.json` (SKEIN-1048). What a box's start still reads from the store's `settings.json` is `enabledPlugins`, which installs plugins from Anthropic's official marketplace only. The repo's shared working files (`$HOME/shared`) are the store's too, at the same reach: for a store skein manages they sit beside it, at `store/shared-home`, and the launcher binds that back into a covered box beside the store, only as a real directory and never through a link (SKEIN-1324, `docs/decisions/shared-home-outside-claude.md`) | same test; tests `every_turn_state_hook_runs_a_script_the_read_only_plugin_installs` and `every_start_helper_runs_from_the_read_only_plugin` (`src/probes.rs`); bwrap test `a_box_cannot_plant_a_hook_or_status_line_its_siblings_claude_runs`, in both layouts and with the workshop box as the control; tests `a_repo_that_tracks_its_settings_stays_clean_and_skeins_settings_go_to_the_local_file` and `a_box_whose_claude_is_the_store_link_is_converted_and_keeps_everything` (`src/kit.rs`) |
| its own state directory, including the git token the host minted for it | read-only; its conversation is writable through a separate bind at `$HOME` | same test |
| its own resource signal file, `signals/resources.json` in its state directory, which says what skein asks of it, and lists what in the box can be removed and made again, with its repo's shared download cache and how many of the repo's other boxes are running (SKEIN-1356) | read-only; the box plugin's hooks read it and nothing in the box can clear or invent an ask, add to the list, or claim its siblings stopped. The plugin's `skein_clean` removes only what is on the list, as the box, never through a link and never off the listed directory's filesystem; the repo's cache only while skein said within three minutes that no other box of the repo runs | bwrap tests `a_box_cannot_write_its_own_signal_file`, with the workshop box as the control that can, and `a_box_removes_what_skein_listed_and_nothing_a_link_leads_to`; tests `a_path_not_on_the_list_is_refused_and_nothing_is_removed` and `the_shared_cache_is_refused_while_a_sibling_runs_or_the_word_is_stale` (`tests/plugin_tools.rs`) |
| another box's checkout, conversation or git tokens | **no**. That includes the privileged box, whose home is the sandbox's: a new box's `~/.claude` and `~/.claude.json` used to be copies of it, history, memory and plugin markers included, and now start with the login, its account record and what skein writes for every box (SKEIN-1235) | same test, and `src/box-session.sh:2243`, `src/box-session.sh:2354`, `src/box-session.sh:1118`; fleet test `start_box_leaves_a_box_that_is_actually_usable` |
| another repo's store, or any other host path the sandbox mounts | **no** | same test; the per-mount cover is `src/box-session.sh:2403` |
| the fleet's credentials on the volume: `credentials/`, `github-pats/`, `api-token`, the warden's secret | **no** | bwrap test `a_box_on_a_mounted_volume_cannot_read_the_fleets_credentials` |
| `.skein/private/`: the fleet agent's token and the review call's GitHub token | **no** | bwrap test `a_box_cannot_read_what_skein_keeps_under_private`; cover at `src/box-session.sh:2286` |
| the fleet agent's socket | **no**, `connect()` refused | bwrap test `a_box_cannot_connect_to_the_fleet_agents_socket` |
| the cockpit's tmux socket, at the path current code puts it | **no**, `connect()` refused; see SKEIN-831 under Open for fleets created earlier | bwrap test `a_box_cannot_connect_to_the_fleets_tmux_socket` |
| the launcher, the credential helper, skein's source and toolchain under `.skein` | read-only | bwrap test `a_box_can_read_what_skein_was_built_from_and_cannot_write_it` |
| the fleet's Playwright browsers, `.skein/browsers` | **read-only**, in every box, the workshop box included (SKEIN-1341). skein fills it, outside every box: Playwright's own installer runs in a namespace of its own that has `/usr` and `/etc` read-only, the network and one staging directory, and nothing of the fleet root, `/run` or the sandbox's home. What it staged is refused whole if it holds a link, a special file, a hard link, a setuid bit or a name that is not a browser directory; otherwise it is made read-only and renamed in, and an entry never changes once published. A box asks for a missing version through its own package queue, at most one waiting at a time, and a request that is not a browser and a Playwright version is denied with the reason. Each box's `~/.cache/ms-playwright` holds the links, made by the kit inside the box, so a link a box plants misleads that box alone. **Not defended**: Playwright publishes no digest for its downloads, so the store holds what the official download served, as each box's own download did; and a box using the Docker socket reaches it like everything else | fleet tests `the_browser_store_is_read_only_in_every_box`, `every_start_links_the_fleets_browsers_and_a_boxs_plants_stay_its_own`, `a_fill_sees_no_box_and_writes_only_its_staging` and `a_browser_request_is_answered_without_the_owner` (`tests/fleet_launch/browsers.rs`); tests `a_fill_that_stages_a_link_a_device_or_a_path_escape_is_refused` (`src/fleet/browsers.rs`) and `a_box_has_one_browser_request_waiting_at_a_time` (`tests/substrate_request.rs`) |
| the fleet's agent CLIs and Rust toolchains, `.skein/depot/agent-cli` and `.skein/depot/rust-toolchain` | **read-only**, in every box, the workshop box included (SKEIN-1372). Each version is its own entry, filled by skein outside every box in the same kind of namespace as the browsers (`/usr` and `/etc` read-only, the network, one staging directory; no fleet root, `/run` or sandbox home), checked without following links (a link is accepted only when it is relative and stays inside its entry), made read-only and renamed in, never changed afterwards. A box binds the entry skein rolled out at the path it already runs from: the npm prefix becomes a read-only tmpfs holding the sandbox's own packages read-only and the rolled-out CLI's package from its entry, and `~/.rustup` becomes a read-only tmpfs holding the default toolchain and the repo's own. A rollout is the owner's press and changes only what the next start binds; a running box keeps its entry and is restarted onto the new one only when it is idle. The one thing a box can write about any of this is a `restart-when-idle` file in its own package drop-box, which skein acts on only for a box that is behind and idle at that moment. A box's `rust-toolchain.toml` is read on skein's side as data: the file is opened without following a link, read to 4 KB, and only a pinned channel's exact shape is used, with component and target names of their own exact shapes, at most eight extra of each; one name of any other shape pins nothing (SKEIN-1415). What a box's file can do is have skein fetch the toolchain it names, through rustup's checked download in the fill namespace and within the kind's disk budget. **Not defended**: claude-code's own install script runs in the fill namespace and places a native binary with no digest to check, so the entry holds what npm served; and a box using the Docker socket reaches the depot like everything else | fleet tests `a_rolled_out_cli_is_read_only_in_every_box`, `an_idle_box_moves_onto_a_rollout_and_a_working_box_does_not`, `a_repos_rust_toolchain_file_selects_its_toolchain`, `a_repos_pinned_toolchain_is_bound_without_a_fleet_default_and_a_missing_one_is_named` and `the_clean_refuses_a_version_in_use` (`tests/fleet_launch/versions.rs`); tests `only_a_relative_link_that_stays_in_its_entry_is_published` (`src/fleet/depot.rs`), `a_repos_toolchain_file_names_its_toolchain_and_nothing_else_does` and `a_hostile_toolchain_file_pins_nothing` (`src/fleet/versions.rs`) |
| the scripts its own and its siblings' turn-state hooks run, Claude's and Codex's | read-only: they are installed under `.skein/plugin*/probe/`, beside the launcher | bwrap test `a_box_cannot_rewrite_the_script_its_siblings_turn_state_hook_runs`, with the workshop box as the control that can |
| what it and its siblings run from skein as they start: the kit's helpers, Codex's hook wiring and its installer, the attach shell's and the tracker's helpers, and the status line's renderer | read-only: installed under `.skein/plugin-turn-state/probe/`, beside the turn-state scripts | bwrap test `a_box_cannot_change_what_its_sibling_runs_as_it_starts`, with the workshop box as the control whose start runs every plant |
| a file in a sibling's checkout, through the repo's `shared-paths.txt` and `shared-rw/` in its store | only a regular file at a relative path with no `..` and no hidden or `.git` directory on the way, out of `shared-rw/`. Any other entry is refused, out loud, at the sibling's next start, and a link an earlier start made for it is removed (SKEIN-1256). A file that does pass is shared read-write between the repo's boxes by design. Since SKEIN-1330 the launcher binds the copy over its place in the checkout at the box's start, and asks the same rules itself first, plus one more: where the entry really lands, once the checkout's own links are resolved, must pass the shape rule too, so a link the repo tracks cannot carry it into `.git/hooks`. A file of the box's own at that place is not bound over. There is no exception for `.codex/memory` any more: Codex keeps its memories in its own home | tests `a_shared_path_is_surfaced_only_in_an_ordinary_shape_and_only_as_a_regular_file` and `nothing_under_a_hidden_directory_is_surfaced_and_a_bound_entry_is_left_alone` (`src/kit.rs`); bwrap test `a_covered_box_writes_its_store_at_data_and_edits_a_surfaced_file_in_place` |
| the snapshot a takeover's replacement box is rebuilt from | **no**. The host writes it into the replacement's own state directory, read-only to that box and absent from every other, and the kit restores only from there, listing each archive before opening it and refusing one that holds more than it may carry (SKEIN-1258) | tests `a_handoff_is_restored_only_from_the_boxs_own_host_only_state_directory` and `a_handoff_archive_carrying_more_than_it_may_is_refused_whole` (`src/kit.rs`), `provisioning_tells_the_kit_where_the_boxs_host_only_state_is` (`src/fleet/start.rs`) |
| its own git-write and package request queues | writes its own; reads every other box's; writes no other box's. skein reads each file in a drop-box once, through a descriptor opened without following a link or waiting on a FIFO, checked to be a regular file and read to 64 KB, and the answer it writes back into the box's own request is made from those bytes, never from the path, so a link there cannot make skein read a file the box cannot (SKEIN-1406) | bwrap tests `a_box_can_write_its_own_request_queue_and_no_other_boxs`, `a_box_cannot_make_the_package_queue_read_anything_but_its_own_files` and `a_box_cannot_make_the_git_write_queue_read_anything_but_its_own_files` (`tests/isolation_bwrap/dropbox.rs`); tests `the_write_back_into_a_boxs_request_never_reads_a_link_it_planted` (`src/substrate.rs`, `src/gitgate/decide.rs`) |
| the asks queue, `.skein/asks/requests/<box>/`: its questions for the fleet's owner | writes its own, no other box's. The owner's answer is kept on the volume (`asks/<box>/`) and reaches the box only through its read-only inbox, so a `state` or `answer` it writes into its own question is never shown as one; the cockpit shows at most 5 of a box's questions waiting, and none over 2 KB. skein reads its questions as it reads the request queues, so a link, a FIFO or a file over 64 KB in it is never read (SKEIN-1406) | bwrap tests `a_box_cannot_make_the_ask_queue_read_anything_but_its_own_files` (`tests/isolation_bwrap/dropbox.rs`) and `a_box_can_write_its_own_ask_queue_and_no_other_boxs`, which files through the shipped `skein_ask_person` tool as itself and as a neighbour; server test `a_box_written_state_is_never_shown_as_an_answer` (`tests/server/asks.rs`); `asks::decision_path` |
| the fleet owner's answers to its git-write requests, and the grants they made (`gitgate/<box>/` and `git-grants.json` on the volume) | **no**. So the `state` a box writes into its own request is never what the cockpit shows: a request with no answer on record is shown as waiting, whatever its file says | bwrap test `a_box_cannot_answer_its_own_git_write_request`, which reads the fleet back through `gitgate::list`; `gitgate::decision_path` |
| `/run/user/<uid>` and `/run/secrets` | **no**, a private tmpfs per box | bwrap test `nothing_but_the_socket_directory_comes_through_the_run_cover` (the first); test `the_launcher_covers_what_run_shares_and_names_what_it_does_not` in `src/cockpit.rs` (both); `src/box-session.sh:2659`, `src/box-session.sh:2658` |
| **other boxes' agents, by message** | **yes**, on by default, per repo; no approval gate, since `crossSessionInbound` is seeded to `accept` (`src/box-session.sh:1706`) | bwrap test `a_box_is_never_discoverable_on_a_socket_it_cannot_reach`; off is `src/box-session.sh:2722` |
| **the shared toolchain `~/.local`** | **reads the fleet's copy, writes its own** (SKEIN-963). A copy-on-write overlay per box: the sandbox's `~/.local` is the lower layer, the upper layer is a tmpfs bwrap makes inside the box. So a box sees every tool installed outside it, its own writes are private to it and gone at its next restart, and nothing it writes reaches another box — which matters because `~/.local/bin` is still first on every box's `PATH` (`src/box-session.sh:128`). `~/.local/state` is the exception: bound back from the box's own home, so the work-tracker stamps survive a restart. It starts empty in a new box rather than as a copy of the sandbox's, which belongs to the privileged box and carried every other box's stamps (SKEIN-1234) | code: `src/box-session.sh:1163`, `src/box-session.sh:1854`, `src/box-session.sh:1888`, `src/box-session.sh:1118`; bwrap tests `a_binary_one_box_plants_is_not_what_another_box_runs`, `a_box_still_sees_the_shared_toolchain_under_its_private_overlay`; fleet test `start_box_leaves_a_box_that_is_actually_usable` |
| **`/usr/local/share/npm-global`, the npm prefix `claude` actually runs from** | **read-only** (SKEIN-968). It is second on every box's `PATH` and the first thing `which -a claude` answers with, it is owned by uid 1000, and before this every box could write it — Claude Code's own background auto-updater did, from inside whichever box ran it. Once a version is rolled out (SKEIN-1372, the depot row above), the CLI a box runs is that entry, bound read-only into a read-only tmpfs at this path, and the prefix itself is no longer what any box executes. Updating is skein's now: the in-box updater is stood down by name (`src/box-session.sh:3928`), and `skein update-agents` installs into this prefix rather than root's | code: `src/box-session.sh:2925` and `src/box-session.sh:2927`; bwrap test `the_npm_prefix_a_box_runs_the_agent_from_is_read_only_inside_it` |
| **download caches: cargo's `CARGO_HOME`, npm's cache with `_npx` in it, pip's, uv's; and what the tool managers install (rustup's home, `cargo install`, npm's global prefix, uv, pipx, pyenv, nvm, fnm, volta, mise, `GOPATH`)** | **its own repo's tools area, shared read-write with that repo's boxes and with no other repo's** (SKEIN-1350, SKEIN-1431). One area per repo on the fleet's disk, at `<fleet root>/.repo-tools/<repo id>` and outside `.skein`, held open and bound at `~/.repo-tools` in each box of the repo (`src/box-session.sh:2597`), and each tool pointed into it as the session starts (`src/box-session.sh:3982`); its `tools/bin` is second on every box's `PATH`. A crossing into the box takes the same values from the box's session, and none when the session has none (SKEIN-1354). Inside a repo this is a path one box can put code in that another box runs: `npx` runs what is under `_npx`, cargo compiles `registry/src` without checking it again, and a command in `tools/bin` runs in every box of the repo. That is the owner's decision, not an oversight: a repo is one trust domain, and repos are walled from each other (`docs/decisions/repos-are-the-trust-boundary.md`). A box of another repo cannot see it. pnpm's store is each box's own, beside its checkout, so that it can hard link. The sandbox's own `~/.cargo`, `~/.rustup` and `~/.npm` reach no box; until SKEIN-1350 each was bound read-write into every box of every repo. The old host cache, `store/cache`, is still bound into the repo's boxes (`src/box-session.sh:2546`) so it can be cleaned, and no tool is pointed at it. A repo whose store is kept elsewhere has no area, and its boxes keep their own | bwrap tests `a_repos_boxes_share_its_tools_area_and_another_repos_box_sees_none_of_it`, `a_link_where_the_area_should_be_is_refused`, `the_sandboxs_cargo_and_npm_reach_no_box`; fleet tests `a_crossing_into_a_box_uses_its_repos_download_cache` and `a_crossing_into_a_box_with_no_repo_cache_carries_none_of_its_names` (`tests/fleet_launch/environment.rs`) |
| a binary planted in `~/.local/bin`, run by skein at fleet scope | **no**, fleet-scope scripts and crossings do not resolve through it | tests `a_planted_binary_is_not_what_a_fleet_scope_script_runs`, `a_planted_nsenter_is_not_what_a_crossing_runs` |
| **the sandbox's Docker daemon, `/run/docker.sock`** | **yes, deliberately.** It gives a root container in the sandbox with any bind mount, which reaches every box's files, the fleet root and the volume. **Every "no" above holds only for a box that does not use it.** | code: `grep -n 'docker.sock' src/box-session.sh` finds only the comment saying it is left open, and `the_launcher_covers_what_run_shares_and_names_what_it_does_not` keeps that comment there; the decision is architecture §9.5 R11 (`docs/architecture.md:1793`) |
| every TCP port in the sandbox, including the cockpit's API | **yes, it can connect**. The API needs the token, which is in the "no" rows above — and the switch that would remove that requirement is refused rather than honoured for a cockpit under the fleet's doorway (SKEIN-962), and for a `skein-server` a box starts (SKEIN-1086) | code: no `--unshare-net`; `src/apiauth.rs:56` is the one off-switch; test `the_auth_off_switch_is_refused_under_the_fleets_doorway_and_honoured_outside_it` (`tests/server/door.rs`), test `the_switch_is_refused_inside_a_box_and_honoured_where_neither_marker_is_set` (`src/apiauth.rs`) |
| the cockpit, through a link carrying `?t=` that it has been shown — pasted into its terminal, left in a log it can read | **yes, the whole cockpit, until `api-token` is changed.** A known risk the owner chose to keep (SKEIN-516): the link carries the long-lived `api-token` itself, and the cookie it is swapped for is that same token for a year, so seeing the link is holding the token. Changing it means minting a new `api-token`, which signs out every browser that holds the old one | code: `src/bin/skein-server/door.rs:131-171` |
| the host's `ssh-agent` through `$SSH_AUTH_SOCK` | **no**, a regular file is bound over the socket (scoped boxes) | code: `src/box-session.sh:3378`, under `src/box-session.sh:3250` |
| the host's `ssh-agent` through the gateway on TCP 3129 | **yes**, left reachable by owner decision (SKEIN-929); usable if the host agent holds a key | architecture §9.6 |
| GitHub, from a scoped box's `git` and `gh` | only with the box's own token: the GitHub hosts go into `NO_PROXY`, so those two tools never reach the proxy and are bounded by the token they present. **This routes the normal path around the proxy; it does not contain anything** — `NO_PROXY` is a variable anything in the box can set again, which the launcher says of itself — so what the proxy would do to a request that *is* on it is the row below | test `a_scoped_box_routes_github_direct_and_a_fleet_box_does_not` (`tests/git_write_request.rs`); live test `tests/github_reach_live.rs`, ignored unless `SKEIN_LIVE_FLEET=1` |
| **GitHub through the sandbox proxy** instead of around it | **whatever the proxy decides, and skein cannot bound it — so skein measures it and says so.** Measured *not* injecting on 2026-09-21: an invalid credential sent through the proxy is refused, and an accepted request carries the anonymous hourly ceiling. It **was** injecting on 2026-09-06 and again on 2026-09-15, when the same probe came back authenticated as the account. Nothing in this tree changed between those dates; the substrate did, which is why this is a check and not a sentence | the `proxy_injection` health check — `proxy_injection_line` and `probe_proxy_injection_at` in `src/health/reach.rs` — red on the cockpit's banner when the answer is yes (SKEIN-927); tests `only_an_authenticated_acceptance_through_the_proxy_is_injection` and `the_probe_puts_nothing_but_a_marked_non_credential_on_the_wire`. The command is under [Checking this page](#checking-this-page) |
| the rest of the internet | **yes — every host tried, and not bounded by skein** (SKEIN-926). **Open by decision rather than by oversight**: a box installs from npm, PyPI, crates.io, GitHub and the model APIs, so an allowlist that misses one produces a failure that reads as a broken build rather than as a policy. skein therefore sets no egress policy at all, and a box reaches whatever the host's `sbx` policy allows | measured 2026-09-21 from inside a scoped box with the proxy out of the path — the command is under [Checking this page](#checking-this-page), and all six hosts answered `200`. `grep -rn 'sbx policy' src` finds 9 hits, every one of them advice printed for a person to run on their own host; none sets a policy |
| the fleet's canonical agent login | cannot replace it; a box's login moves up only when the fleet holds none (`src/box-session.sh:1490`) | code |
| another box's files or the fleet's, through a link it leaves in its own root for the launcher to bind or write at its next start | **no**. The launcher refuses to start a box whose `home`, `tmp`, `bin`, `vartmp`, `.local/state` or agent homes are not real directories of the box's own (SKEIN-1254), and writes each file in the root under a temporary name and renames it over the target, which replaces a link instead of following it (SKEIN-1255). A box process still running across the restart cannot race the check either: each directory is held open once checked, and written and bound through the descriptor from then on (SKEIN-1287) | fleet tests `a_box_whose_own_directory_was_swapped_for_a_link_is_refused`, `a_link_where_the_launcher_writes_is_replaced_and_what_it_led_to_is_untouched`, `a_dotfile_link_inside_the_home_is_followed_and_a_foreign_git_directory_is_not_written`, `a_directory_swapped_after_it_was_checked_is_not_what_the_box_gets`; code: `src/box-session.sh:945`, `src/box-session.sh:968`, `src/box-session.sh:1047` |
| the environment the cockpit was started with | **only the names on the launcher's allow-list** (SKEIN-972), whether through the box's session or through a later crossing into it (SKEIN-1085); see [What a box inherits](#what-a-box-inherits). The cockpit's own variables, `SKEIN_HOME` and `SKEIN_LISTEN_INHERITED_ONLY` among them, reach neither. A crossing into a scoped box carries no `GH_TOKEN`, never the fleet's, as its session does (SKEIN-1095, SKEIN-1332) | tests `a_box_session_inherits_only_its_allow_list`, `a_crossing_into_a_box_carries_only_its_allow_list` and `a_crossing_into_a_scoped_box_carries_its_own_github_token_or_none` (`tests/fleet_launch/environment.rs`), which start a real box with a canary in the environment and read the environment back from inside it; `the_crossing_list_is_the_launchers_list` and `the_names_the_session_decides_are_the_launchers_unsets` (`src/place/crossing.rs`) |

**Two kinds of box get less of this, and both say so when they start.**

* **The workshop box** (`SKEIN_BOX_PRIVILEGED=1`) skips the whole isolation block
  (`src/box-session.sh:2196`), so every "no" in the file rows becomes "yes", including the fleet
  agent's token. It is the one deliberate escape hatch; architecture §9.2 path 3 states its terms.
  bwrap test `the_workshop_box_sees_what_an_ordinary_box_cannot`.
* **A box with no mount manifest**, because skein matched it to no repository or its launcher
  predates the manifest, can read other repos' stores and the fleet's credentials on the volume.
  Other boxes' checkouts and state, and `.skein/private/`, are still covered. bwrap test
  `a_box_with_no_mount_manifest_is_uncovered_and_a_matched_box_is_not`; the start-up banner is
  asserted by `an_unmatched_box_announces_that_it_is_uncovered_and_a_covered_box_says_nothing`.

## What a box inherits

A box's session gets environment variables from three places, and only the first is a choice
skein makes about the cockpit's environment:

1. **The allow-list.** `src/box-session.sh` removes every variable it was started with except the
   names on one list, `inherited_env`, before it reads anything. Each name has its reason beside it.
   Print the list with:

   ```sh
   sed -n '/^inherited_env=(/,/^)/p' src/box-session.sh
   ```

   In summary it keeps the sandbox's home and `PATH`; the variables skein itself passes the
   launcher; `SKEIN_FLEET_ROOT`, and one test seam, both unset in production; the sandbox's
   proxy, CA and credential-proxy variables, without which a box has no network and no model;
   `GH_TOKEN` and the ssh-agent variables, which a `fleet`-scoped box keeps on purpose and a
   scoped box has replaced or covered further down the launcher; and the sandbox's name, npm
   prefix, `BASH_ENV` and runtime directory. A name that is not a shell identifier, including an
   exported function, is removed too.
2. **What the launcher exports on purpose** after that point: `SKEIN_BOX`, `SKEIN_STATE`, the box's
   `PATH`, the scoped-git variables, `CLAUDE_CODE_TMPDIR` and the updater switches. The list does not
   apply to these. One of them is `SKEIN_IN_BOX=1`, which marks the session as a box (SKEIN-1086).
   A box shares the fleet's network namespace, so a `skein-server` started in one would answer
   every other box, and `apiauth::off_switch_refused` refuses `$SKEIN_NO_API_AUTH` when this is set,
   as it does under the cockpit's doorway. A box can unset it in its own shell. What it prevents is
   the accident, not a box that means to serve something unauthenticated, which it can do with any
   program.
3. **What the box's own login profile sets** inside the namespace. That is the box's business, and
   the list does not apply to it either.

A variable that is on none of these does not reach a box's session. Adding one means adding it to
the list with its reason. There is no per-box setting for this.

**A crossing gets the same list** (SKEIN-1085). A process skein runs inside a box later, by
crossing into its namespace, is spawned by `skein-server` and used to carry the cockpit's whole
environment in. That covers provisioning, the attach and shell terminals, the pane observer the
attach starts, model calls and file reads and writes. `Place::enter` in `src/place/argv.rs` now removes
every variable not on the same `inherited_env` list before it `nsenter`s. It reads the list out of
the launcher's own text, so there is one list. It sets `SKEIN_IN_BOX=1` on the same step. It
also keeps two names that are not on the list: `TERM` and `COLORTERM`. An attach ends in
`tmux attach-session`, which needs to know the terminal it draws on, and a session start has no
terminal. The filter runs before the hop, so the box's own login profile still applies afterwards,
as it does for a session.

**And a crossing holds what the box's session was given** (SKEIN-1095). The list only says which
names a box may inherit. For some of them the launcher then decides per box: a scoped box has
`GH_TOKEN` removed, and loses `SSH_AUTH_SOCK`; a box with a login
of its own loses `ANTHROPIC_API_KEY` or `OPENAI_API_KEY`. A crossing used to carry skein-server's
values for all of these, so every crossing into a scoped box brought the fleet's `GH_TOKEN` in,
including the pane observer, whose `/proc/<pid>/environ` the box's agent can read.
`place::session_decides` now reads those names out of the launcher: every inherited name the
launcher `unset`s. `Place::enter` unsets each one and exports it again from the environment of the
box's tmux server, the anchor its guard has just checked, only if that process has it. That
environment is the launcher's final one, so the crossing gets exactly what the session got and
the rule is not written twice. A `fleet`-scoped box still gets the fleet's token. Tests
`a_crossing_into_a_scoped_box_carries_its_own_github_token_or_none` (`tests/fleet_launch/environment.rs`)
and `the_names_the_session_decides_are_the_launchers_unsets` (`src/place/crossing.rs`).

**A crossing points cargo, npm, pip, uv and pnpm where the session does** (SKEIN-1354). The
session block points each tool at the repo's download cache with a variable (`CARGO_HOME`,
`npm_config_cache` and the rest), but only when the launcher gave the box a cache. A crossing used
to carry none of them, so its cargo and npm used the box's own `~/.cargo` and `~/.npm`.
`place::session_cache_names` now reads the names out of the session block's exports, and
`Place::enter` takes them from the anchor's environment the same way it takes the names above: the
session's value, or none. So the crossing never works out a cache path itself, and cannot name one
the session does not use. A box with no repo cache gets none of them, and skein-server's own
`CARGO_HOME` stays out. Tests `a_crossing_into_a_box_uses_its_repos_download_cache` and
`a_crossing_into_a_box_with_no_repo_cache_carries_none_of_its_names`
(`tests/fleet_launch/environment.rs`).

**A crossing carries every tool variable the session block exports** (SKEIN-1371). The cache names
above are one block of the session's exports; the toolchain's `RUSTUP_HOME`, `CARGO_INSTALL_ROOT`,
the Playwright names and the agent's auto-update switches are the rest, and a crossing used to get
none of them, so its `cargo` reached rustup's proxy with no `RUSTUP_HOME`. `place::session_exports`
now reads every `export` in the session block, and `Place::enter` takes each from the anchor the
same way: the session's value, or none. `PATH` is read and not carried: the parse accepts only
`PATH="$box_path"`, which `Place::wrap` builds again past the hop, so the box's own `~/.local/bin`
never reaches the `env` and `nsenter` in front of it. `HOME`, `LD_*`, `BASH_ENV`, `ENV`, `IFS`,
`SHELLOPTS` and `BASHOPTS` are refused by the parse. Tests
`a_crossing_into_a_box_carries_its_sessions_tool_variables` (`tests/fleet_launch/environment.rs`)
and `the_names_a_crossing_takes_from_the_session_are_every_export` (`src/place/crossing.rs`).

**A box's own-repo token is read from its file on every call, never held in its environment**
(SKEIN-1240, SKEIN-1332). The host rewrites the placed file the moment a key is replaced and removes
it when one is forgotten or a share ends. git reads it through the credential helper, and a scoped
box's `gh` is a shim, bound over the real binary, that reads it on every call and has no credential
when there is none. The launcher exports no `GH_TOKEN` in a scoped box: an export would be a
snapshot every session and crossing kept until the box restarted, so a forgotten key would go on
working from `$GH_TOKEN`. A `GH_TOKEN` present in such a box was set on purpose, a review
crossing's for one, and `gh` uses it unchanged. Test `gh_in_a_running_box_uses_the_token_placed_now`
(`tests/fleet_launch/environment.rs`).

## Open

Each of these either has no evidence strong enough for a row, or is a known gap in a row above. The
tracker item is the record; this list only points at it.

* **SKEIN-548 / SKEIN-927** are no longer listed here, and the two rows above say why: open egress
  is a decision that is now stated and measured rather than a gap, and injection is now a check
  that runs. What is *not* closed is the underlying property — a process in a box that points
  itself back at the proxy, or clears `NO_PROXY`, is on whatever the proxy does that day. skein
  cannot change that from inside the sandbox; it can only be the thing that notices, which is the
  whole of what the `proxy_injection` row claims.
* **SKEIN-947** is no longer listed here because the route is gone. `/api/path` said whether any
  path on the sandbox's filesystem existed, and what it was, to anyone holding the API token. That
  covered the fleet root, `.skein/private/` and `/etc`. Its last caller, the SSH key field, named a
  key on the host that skein in the fleet cannot read. The field, the route, the `ssh_key` setting
  and `$SKEIN_SSH_KEY` were all deleted, and Settings now says to run `ssh-add` on the host. Test
  `server_serves_ui_vendor_and_guards_routes` (`tests/server/routes.rs`) asserts that the route answers like a
  path that never existed.
* **SKEIN-831**: a fleet that was serving before the socket move keeps the cockpit's tmux socket at
  the old path in the readable half of `.skein`, where a box can `connect()` to it. A tmux client
  can make the server run commands, so that is fleet-scope execution. It lasts until that fleet's
  tmux server restarts.
* **SKEIN-960**: on some live boxes the agent's own config trusts the filesystem root. For a
  working directory outside a git repository, that runs a folder's `.claude/settings.json` hooks
  without asking. skein did not write the entry.
* **SKEIN-1308**: the repo's store is writable by every box of the repo, so a link a sibling puts
  in `memory/` or the shared home after a box starts is followed by that box's agent, into its own
  private files. A fix is held on branch `fix/iso2-1308-held`, scoped to the whole of
  `store/.claude`. The shared home has since moved out of it, to the sibling `store/shared-home`
  (SKEIN-1324), and the store itself to `store/data` (SKEIN-1330), so that fix must cover both
  directories, by their new names, before it lands.
* **SKEIN-964**: architecture §9.4 says a box can signal skein and other boxes through the shared
  PID namespace. The premise is in the code, but no test sends a signal, so it is not a row.

## Checking this page

```sh
cargo test --test isolation_bwrap        # the bwrap rows; a skip names the check that did not run
python3 tools/line-cite-check.py         # every file:line above still says what it said when cited
python3 tools/prose-check.py             # every test and function named above still exists
```

**The two substrate rows are measured, not tested**, because what they are about is not in this
repository. Run them from inside a box. Neither sends a credential: the first sends none at all,
and the second sends a string that is marked as not being one.

```sh
# egress (SKEIN-926) — the proxy deliberately out of the path, so this is the direct reach
for h in httpbin.org www.reddit.com pypi.org discord.com www.bbc.co.uk example.org; do
  curl -s --noproxy '*' -o /dev/null -w "$h %{http_code}\n" "https://$h/"
done

# the proxy's credential (SKEIN-548) — 401 is the proxy adding nothing; a 200 whose
# x-ratelimit-limit is far above 60 is the account answering for a string that cannot be a token
curl -sS -x "$HTTPS_PROXY" --noproxy '' -o /dev/null -D - \
  -H 'Authorization: Bearer skein-test-not-a-credential' \
  https://api.github.com/rate_limit
```

Both were last run on **2026-09-21**, and every date in the two rows above is a date somebody ran
one of them. That is the point of writing the commands here and not the results alone: the
substrate under this page belongs to somebody else, the proxy row already records it answering one
way on 2026-09-15 and the other way six days later with nothing in this tree changing between, and
a reader who needs today's answer rather than that day's can have it in two commands.

When a cover changes, change the row in the same commit. A row that is no longer true is worse
than a missing one, because a reader trusts it.
