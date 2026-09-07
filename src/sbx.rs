//! What `sbx` says about the boxes it holds — and what to do when it will not say.
//!
//! The sandbox tool is a subprocess, on the path of every board refresh, and it is the one call
//! that made a struggling machine worse: asking a busy sandbox how it feels every two seconds is
//! how a slow daemon became an unresponsive one. So every reader here goes through a
//! [`crate::util::Gate`] — remembered, asked by one caller at a time, and asked *less* often while
//! the answer keeps failing.
//!
//! Two rules that look like defensiveness and are not. A transient failure serves the last good
//! answer rather than an empty one, because an empty list is indistinguishable from "every box is
//! gone" and the board would act on it. And the reason a listing failed is remembered separately
//! from the listing, so "no boxes" and "sbx did not answer" are different sentences on screen.
//!
//! The parsing is tolerant on purpose: `sbx ls` has changed key names across versions, so the
//! readers try several and a field that is missing stays empty rather than failing the whole list.

use crate::fleet::fleet_liveness;
use crate::place::shared_record;
use crate::registry::registry_entry_for_box;
use crate::util::valid_name;
use crate::util::{clip, output_with_timeout_why, Gate};
use std::env;
use std::process::Command;
use std::time::Duration;

/// What sbx itself reports about a box's run state (from `sbx ls`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Liveness {
    Running,
    Stopped,
}

/// A box as sbx itself sees it (`sbx ls --json`) — the source of record for the fleet, so a box
/// shows up because it's a sandbox sbx knows about, not because it wrote itself into a registry.
#[derive(Clone, Debug)]
pub struct SbxBox {
    pub name: String,
    /// "claude" | "codex" | … — the per-runtime seam for the (later) turn-state adapter.
    pub agent: String,
    /// Run state; `None` if sbx reports an unrecognised status (caller falls back to lastSeen).
    pub live: Option<Liveness>,
    /// The repo workspace (the shared `.claude` store mount is excluded). May be empty.
    pub dir: String,
}

/// Micro-cache over `sbx ls`: `load_views` used to re-run it once at the top and then again via
/// `lookup_dir` for every box the registry didn't know (`read_journal` → `lookup_dir` →
/// `fleet_boxes`) — an N-box fleet paid 1+N subprocess spawns per 2s tick, times open browser tabs.
///
/// A [`Gate`] rather than a plain cache, because the tabs were still multiplying: the answer is
/// only remembered once the first `sbx ls` *returns*, so every tab that missed together spawned its
/// own. See the note on `Gate` — it also carries the last good answer and the backoff that lets a
/// struggling daemon recover instead of being re-asked every 1.5s forever.
static FLEET_GATE: Gate<Vec<SbxBox>> = Gate::new();

/// Forget the remembered listing, so the next caller waits for the truth.
///
/// The same property [`crate::fleet::forget_fleet_liveness`] exists for, on the other gate: a `Gate`
/// serves its last good answer while it refreshes behind the caller, which is what stops a slow
/// `sbx` blanking the board — and is exactly why an act that changes the answer has to say so.
///
/// It matters *more* since the listing left the board tick. A remembered answer that was two seconds
/// old and about to be refreshed anyway is now one that nothing refreshes: `sbx ls` is asked when a
/// person asks, and a person asking what sandboxes are on this machine right after destroying one is
/// the case this is for.
pub fn forget_fleet_boxes() {
    FLEET_GATE.invalidate();
}

/// How often skein is willing to spawn `sbx ls` while it is answering.
const FLEET_FRESH: Duration = Duration::from_millis(1500);

/// Enumerate the fleet from sbx. `None` when sbx can't be consulted (not installed, errored,
/// unparseable, or hung past the timeout) *and* nothing was ever learned — callers then fall back to
/// the registry. Override with `$SKEIN_LS_CMD` (run via `sh -c`; must emit the `sbx ls --json`
/// shape). Micro-cached — see [`FLEET_GATE`].
pub fn fleet_boxes() -> Option<Vec<SbxBox>> {
    // Zero: tests swap $SKEIN_LS_CMD per case and run in parallel — a process-wide gate would serve
    // one test's fleet to another. Prod (server/CLI) keeps it.
    let fresh = if cfg!(test) {
        Duration::ZERO
    } else {
        FLEET_FRESH
    };
    FLEET_GATE.get(fresh, || {
        let asked = env::var("SKEIN_LS_CMD").ok().filter(|s| !s.is_empty());
        // `sbx ls` is a question about the *machine*, and in-fleet skein is not standing on it.
        // `None` already means "sbx could not be consulted, fall back to the registry", which is the
        // right behaviour — but the reason has to be recorded or the board reports a broken sbx for
        // a deployment where its absence is correct. An override still wins: a test or a proxy that
        // can answer the question is answering it, whatever this process is running inside.
        if asked.is_none() {
            remember_fleet_failure(Some(
                "skein is running inside the fleet, and `sbx ls` asks about the host's machine — \
                 which boxes exist is read from their placement records instead"
                    .into(),
            ));
            return None;
        }
        let label = format!(
            "`{}`",
            asked.clone().unwrap_or_else(|| "sbx ls --json".into())
        );
        let mut cmd = match asked {
            Some(c) => {
                let mut sh = Command::new("sh");
                sh.arg("-c").arg(c);
                sh
            }
            None => {
                let mut sbx = Command::new("sbx");
                sbx.args(["ls", "--json"]);
                sbx
            }
        };
        // Bounded: a wedged sbx daemon used to hang this .output() forever — and with it every
        // fleet-snapshot task, accumulating stuck blocking threads until the board went permanently
        // blank. A timeout degrades to the registry fallback instead.
        let got = output_with_timeout_why(&mut cmd, Duration::from_secs(5))
            .and_then(|o| match o.status.success() {
                true => Ok(o),
                false => Err(format!(
                    "{label} exited {}{}",
                    o.status
                        .code()
                        .map(|c| c.to_string())
                        .unwrap_or_else(|| "on a signal".into()),
                    match String::from_utf8_lossy(&o.stderr).trim() {
                        "" => String::new(),
                        said => format!(" — {}", clip(said, 200)),
                    }
                )),
            })
            .and_then(|o| {
                let out = String::from_utf8_lossy(&o.stdout);
                parse_boxes_checked(&out).ok_or_else(|| {
                    format!(
                        "{label} answered, but not with a fleet listing skein can read{}",
                        match out.trim() {
                            "" => " — it printed nothing at all".to_string(),
                            said => format!(": {}", clip(said.lines().next().unwrap_or(""), 200)),
                        }
                    )
                })
            });
        match got {
            Ok(boxes) => {
                remember_fleet_failure(None);
                Some(boxes)
            }
            Err(why) => {
                remember_fleet_failure(Some(why));
                None
            }
        }
    })
}

/// Why the last `sbx ls` produced no fleet, or `None` when it produced one.
///
/// [`fleet_boxes`] answers with an `Option`, and four unrelated failures arrive as the same `None`:
/// sbx is not on this process's PATH, the call outlived its budget, it exited non-zero, or it
/// printed something that is not a listing. Every caller then says a version of "sbx did not
/// answer" — true of one of those four and misleading about the other three. The one that sent a
/// user looking in the wrong place was PATH: `sbx ls` worked in their terminal, so the message read
/// as skein being wrong about a working sbx.
///
/// Written by whichever call actually ran, rather than recomputed by asking again, because a second
/// ask is a different question: a daemon that has recovered would report success while the board is
/// still showing the failure that is being explained.
static FLEET_WHY: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

fn remember_fleet_failure(why: Option<String>) {
    if let Ok(mut slot) = FLEET_WHY.lock() {
        *slot = why;
    }
}

/// The last [`fleet_boxes`] failure in words, for the places that report one: the server's log, the
/// health banner and `skein doctor`.
pub fn fleet_failure() -> Option<String> {
    FLEET_WHY.lock().ok().and_then(|why| why.clone())
}

/// Whether the fleet snapshot on offer is a remembered one because the last `sbx ls` failed. The
/// cockpit says so rather than presenting stale data as live.
pub fn fleet_degraded() -> bool {
    FLEET_GATE.degraded()
}

/// One box's run-state from sbx — a single-box view of [`fleet_boxes`].
pub(crate) fn box_liveness(name: &str) -> Option<Liveness> {
    // A shared box is not in `sbx ls` — no sandbox carries its name — so asking there reports every
    // one of them as gone. Its anchor IS its liveness: the tmux server lives exactly as long as the
    // box, so a live pid is a running box and a dead one is a stopped box with its tree intact.
    if shared_record(name).is_some() {
        // Asked of the sandbox, not of the host's /proc — see `fleet_liveness`. A box missing from
        // the sweep is one the sandbox could not answer for (it is stopped, or sbx did not reply),
        // and "cannot tell" is `None`, not "stopped".
        return fleet_liveness().get(name).map(|live| {
            if *live {
                Liveness::Running
            } else {
                Liveness::Stopped
            }
        });
    }
    fleet_boxes()?
        .into_iter()
        .find(|b| b.name == name)
        .and_then(|b| b.live)
}

const LS_NAME_KEYS: &[&str] = &[
    "name", "Name", "NAME", "sandbox", "SANDBOX", "vmId", "vmid", "VmId", "id", "ID",
];
const LS_STATUS_KEYS: &[&str] = &["status", "Status", "STATUS", "state", "State"];
const LS_AGENT_KEYS: &[&str] = &["agent", "Agent", "AGENT"];
const LS_WS_KEYS: &[&str] = &[
    "workspaces",
    "Workspaces",
    "workspace",
    "Workspace",
    "WORKSPACE",
];

/// Parse `sbx ls --json` defensively: tolerate NDJSON (one object per line — a common Docker-CLI
/// `--json` shape) or a single array / `{sandboxes:[..]}` / `{name:{..}}` document, and varied key
/// casings. Boxes without a (valid) name are skipped.
#[cfg(test)]
pub(crate) fn parse_boxes(json: &str) -> Vec<SbxBox> {
    parse_boxes_checked(json).unwrap_or_default()
}

/// Parse a syntactically valid sbx fleet response. `Some([])` is materially different from `None`:
/// an empty array/map authoritatively says no boxes exist, while `None` means the command output was
/// not a fleet document and callers may use last-known-good/cold-start fallback.
pub(crate) fn parse_boxes_checked(json: &str) -> Option<Vec<SbxBox>> {
    use serde_json::Value;
    let lines = json
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect::<Vec<_>>();
    if lines.is_empty() {
        return None;
    }
    // Multi-line NDJSON is valid only when every line is an object. Do not silently discard an API
    // error/malformed line and present the remainder as an authoritative fleet.
    let entries = if lines.len() > 1 {
        let ndjson = lines
            .iter()
            .map(|line| {
                serde_json::from_str::<Value>(line)
                    .ok()
                    .filter(Value::is_object)
            })
            .collect::<Option<Vec<_>>>();
        match ndjson {
            Some(entries) => entries,
            None => collect_ls_entries_checked(serde_json::from_str::<Value>(json).ok()?)?,
        }
    } else {
        collect_ls_entries_checked(serde_json::from_str::<Value>(json).ok()?)?
    };
    let mut out = Vec::new();
    for e in entries {
        let obj = match e.as_object() {
            Some(o) => o,
            None => continue,
        };
        let name = match LS_NAME_KEYS
            .iter()
            .find_map(|k| obj.get(*k).and_then(Value::as_str))
        {
            Some(n) if valid_name(n) => n.to_string(),
            _ => continue,
        };
        let agent = LS_AGENT_KEYS
            .iter()
            .find_map(|k| obj.get(*k).and_then(Value::as_str))
            .unwrap_or("")
            .to_string();
        let live = LS_STATUS_KEYS
            .iter()
            .find_map(|k| obj.get(*k).and_then(Value::as_str))
            .and_then(|s| {
                if s.eq_ignore_ascii_case("running") {
                    Some(Liveness::Running)
                } else if s.eq_ignore_ascii_case("stopped") {
                    Some(Liveness::Stopped)
                } else {
                    None
                }
            });
        let dir = LS_WS_KEYS
            .iter()
            .find_map(|k| obj.get(*k))
            .map(repo_workspace)
            .unwrap_or_default();
        out.push(SbxBox {
            name,
            agent,
            live,
            dir,
        });
    }
    Some(out)
}

/// Pick the repo workspace from a box's `workspaces` value: the path that isn't the shared `.claude`
/// store. Accepts an array or a single string; empty if none.
fn repo_workspace(ws: &serde_json::Value) -> String {
    use serde_json::Value;
    let paths: Vec<&str> = match ws {
        Value::Array(a) => a.iter().filter_map(Value::as_str).collect(),
        Value::String(s) => vec![s.as_str()],
        _ => vec![],
    };
    paths
        .iter()
        .find(|p| !p.trim_end_matches('/').ends_with("/.claude"))
        .or_else(|| paths.first())
        .map(|s| s.to_string())
        .unwrap_or_default()
}

/// The current branch of the git working tree at `dir`, read host-side — so skein can show a box's
/// branch without the registry. None if `dir` isn't a repo or HEAD is detached.
///
/// **Read from `HEAD`, not asked of git**, and the reason is a measurement. This is the only per-box
/// signal on the board tick, and it was the only one that forked: `git rev-parse --abbrev-ref HEAD`
/// per row, every two seconds, for every open browser tab, with no gate to amortise it — twelve
/// forks a tick for a twelve-box fleet, counted in `tests/board_cost.rs`. `HEAD` is a symref in a
/// text file, so the fork was never buying anything.
///
/// Four shapes, and all four are real:
///
/// * `ref: refs/heads/<branch>` — the ordinary case. The branch may contain slashes, so the prefix
///   is stripped rather than the last component taken.
/// * a bare object id — a detached HEAD, which is not a branch. `rev-parse --abbrev-ref` printed
///   `HEAD` here and the caller threw it away; this returns `None` for the same reason.
/// * `.git` as a **file** holding `gitdir: <path>` — a worktree or a submodule. Followed one level,
///   which is as deep as git itself goes.
/// * `dir` **is** the git directory — a bare repo or a mirror, which has `HEAD` and `objects` and no
///   `.git` at all.
///
/// And it searches **upward**, because `git -C` does: the board's `dir` is a checkout root today,
/// but a subdirectory used to answer, and silently ceasing to would be a regression nothing reports.
pub(crate) fn git_branch_for(dir: &str) -> Option<String> {
    if dir.is_empty() {
        return None;
    }
    let mut here: &std::path::Path = std::path::Path::new(dir);
    loop {
        if let Some(branch) = branch_at(here) {
            return Some(branch);
        }
        here = here.parent()?;
    }
}

/// The branch `HEAD` names in the repository at `dir`, if `dir` is one.
fn branch_at(dir: &std::path::Path) -> Option<String> {
    let dot = dir.join(".git");
    // `read_to_string` on a directory fails, which is how the two `.git` shapes tell themselves
    // apart without a second stat in the common case.
    let git_dir = match std::fs::read_to_string(&dot) {
        Ok(text) => {
            let target = std::path::Path::new(text.trim().strip_prefix("gitdir:")?.trim());
            match target.is_absolute() {
                true => target.to_path_buf(),
                false => dir.join(target),
            }
        }
        Err(_) if dot.is_dir() => dot,
        // No `.git` here. `dir` may still BE a repository — the shape `repos.rs` calls a mirror.
        Err(_) if dir.join("objects").is_dir() => dir.to_path_buf(),
        Err(_) => return None,
    };
    let head = std::fs::read_to_string(git_dir.join("HEAD")).ok()?;
    let branch = head.trim().strip_prefix("ref: refs/heads/")?.trim();
    (!branch.is_empty()).then(|| branch.to_string())
}

/// Reduce a single `sbx ls --json` document to a flat list of per-box objects, covering an array,
/// an `{key: [..]}` wrapper, or a `{name: {..}}` map (the box name is injected as `name`).
fn collect_ls_entries_checked(v: serde_json::Value) -> Option<Vec<serde_json::Value>> {
    use serde_json::Value;
    match v {
        Value::Array(a) => Some(a),
        Value::Object(o) => {
            if o.is_empty() {
                return Some(vec![]);
            }
            if LS_NAME_KEYS
                .iter()
                .any(|key| o.get(*key).and_then(Value::as_str).is_some())
            {
                return Some(vec![Value::Object(o)]); // one-line NDJSON with exactly one box
            }
            if let Some(arr) = o.values().find_map(Value::as_array) {
                return Some(arr.clone());
            }
            if !o.values().all(Value::is_object) {
                return None; // e.g. {"error":"API error"} is not an empty fleet
            }
            Some(
                o.into_iter()
                    .filter_map(|(k, mut val)| match val {
                        Value::Object(ref mut m) => {
                            m.insert("name".into(), Value::String(k));
                            Some(val)
                        }
                        _ => None,
                    })
                    .collect(),
            )
        }
        _ => None,
    }
}

/// Look up a box's clone root (the `dir` it registered) by name.
pub fn lookup_dir(name: &str) -> Option<String> {
    // registry first (no subprocess); else the box's workspace from sbx, for boxes the registry
    // doesn't know about (sbx-only / not-yet-registered).
    if let Some(dir) = registry_entry_for_box(name)
        .map(|box_| box_.dir)
        .filter(|dir| !dir.is_empty())
    {
        return Some(dir);
    }
    // A box in the fleet is not a sandbox, so `sbx ls` has never heard of it — but its placement
    // record names its checkout, which is the very thing being asked for.
    if let Some(tree) = shared_record(name)
        .map(|r| r.tree)
        .filter(|t| !t.is_empty())
    {
        return Some(tree);
    }
    fleet_boxes()?
        .into_iter()
        .find(|b| b.name == name)
        .map(|b| b.dir)
        .filter(|d| !d.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::board::load_views;
    use crate::config::{load_config, save_config, Config};
    use crate::place::{forget_place, record_place, PlaceRecord};
    use crate::repos::{Repo, REPOS_CACHE};
    use crate::takeover::replacement_name;
    use crate::testutil::*;
    use std::env;
    use std::fs;

    /// Every shape of `HEAD` this has to read, against real directories rather than a mock.
    ///
    /// Written when the fork became a file read: `git rev-parse` handled all of these and a naive
    /// `read HEAD` handles two, so the cases that would regress silently are the point of the test.
    /// Nothing here runs git — the fixtures are the files git writes, which is exactly the claim
    /// being made.
    #[test]
    fn a_branch_is_read_from_head_in_every_shape_a_repo_takes() {
        let root = tempdir();

        // The ordinary checkout, and a branch with slashes in it: the prefix is stripped rather
        // than the last component taken, or `feat/auth` reads as `auth`.
        let plain = root.join("plain");
        fs::create_dir_all(plain.join(".git")).unwrap();
        fs::write(plain.join(".git/HEAD"), "ref: refs/heads/feat/auth\n").unwrap();
        assert_eq!(
            git_branch_for(plain.to_str().unwrap()).as_deref(),
            Some("feat/auth")
        );

        // Upward, because `git -C` does. A subdirectory answered before and must keep answering.
        let deep = plain.join("src/web");
        fs::create_dir_all(&deep).unwrap();
        assert_eq!(
            git_branch_for(deep.to_str().unwrap()).as_deref(),
            Some("feat/auth")
        );

        // Detached: HEAD is an object id, which is not a branch. `rev-parse --abbrev-ref` printed
        // "HEAD" here and the caller threw it away.
        let loose = root.join("detached");
        fs::create_dir_all(loose.join(".git")).unwrap();
        fs::write(
            loose.join(".git/HEAD"),
            "9f1c0a2b3d4e5f60718293a4b5c6d7e8f9012345\n",
        )
        .unwrap();
        assert_eq!(git_branch_for(loose.to_str().unwrap()), None);

        // A worktree or a submodule: `.git` is a FILE pointing at the real directory.
        let real = root.join("linked-gitdir");
        fs::create_dir_all(&real).unwrap();
        fs::write(real.join("HEAD"), "ref: refs/heads/wt\n").unwrap();
        let linked = root.join("worktree");
        fs::create_dir_all(&linked).unwrap();
        fs::write(
            linked.join(".git"),
            format!("gitdir: {}\n", real.to_string_lossy()),
        )
        .unwrap();
        assert_eq!(
            git_branch_for(linked.to_str().unwrap()).as_deref(),
            Some("wt")
        );

        // And the relative form of the same thing, which is what git writes for a submodule.
        let rel = root.join("submodule");
        fs::create_dir_all(&rel).unwrap();
        fs::write(rel.join(".git"), "gitdir: ../linked-gitdir\n").unwrap();
        assert_eq!(git_branch_for(rel.to_str().unwrap()).as_deref(), Some("wt"));

        // A bare repo or a mirror: no `.git` at all, and `HEAD` sits beside `objects`.
        let bare = root.join("mirror.git");
        fs::create_dir_all(bare.join("objects")).unwrap();
        fs::write(bare.join("HEAD"), "ref: refs/heads/main\n").unwrap();
        assert_eq!(
            git_branch_for(bare.to_str().unwrap()).as_deref(),
            Some("main")
        );

        // Not a repository, and an empty path. Neither may walk out of the filesystem looking.
        let bare_dir = root.join("just-a-folder");
        fs::create_dir_all(&bare_dir).unwrap();
        assert_eq!(git_branch_for(""), None);
        // `tempdir()` is not inside a checkout, so nothing above it answers either.
        assert_eq!(git_branch_for(bare_dir.to_str().unwrap()), None);
    }

    /// The claim the measurement rests on: reading a branch forks nothing.
    ///
    /// **Asserted against a git that is there and would answer**, which is a stronger claim than
    /// the one this made before and costs the rest of the crate nothing. It used to set `PATH=""`
    /// for the length of the call. `PATH` is process-global; the env lock serialises the tests that
    /// *write* it and does nothing for the ones that read it by *spawning*; so for that window no
    /// test anywhere in this process could find `sh`, `bash` or anything else. That is SKEIN-421 —
    /// and it was never one module's problem: under a harness holding this narrowing and `ai.rs`'s
    /// at a 50% duty cycle, `cargo test --lib` failed **10 runs of 10**, with 21 distinct tests
    /// across `ai`, `diff`, `fleet`, `gitgate`, `place`, `sandbox` and `substrate` red over those
    /// runs. With both narrowings given a tail: 0 of 10 (SKEIN-428).
    ///
    /// The decoy replaces the narrowing and keeps the meaning. It goes first on a `PATH` that still
    /// finds a shell, and it **hands every call on to the real git**, so a test spawning git beside
    /// this one is not touched. What it does is leave a mark when it is asked about *this*
    /// repository — so the assertion below is about git not being FORKED, where emptying the `PATH`
    /// only ever showed it was not FOUND.
    #[test]
    fn reading_a_branch_does_not_need_git_on_the_path() {
        use std::os::unix::fs::PermissionsExt;
        let _g = env_lock();
        let root = tempdir();
        let repo = root.join("repo");
        fs::create_dir_all(repo.join(".git")).unwrap();
        fs::write(repo.join(".git/HEAD"), "ref: refs/heads/no-git\n").unwrap();

        let real = env::var("PATH").unwrap_or_default();
        let decoy = root.join("decoy");
        fs::create_dir_all(&decoy).unwrap();
        let forked = root.join("git-was-forked");
        // `$PWD` as well as the arguments, because a fork could name the repository with `-C` or
        // just run in it. Matching on the path is what keeps another test's git — which lands here
        // too, since this `PATH` is the whole process's — from being read as evidence about this
        // one.
        fs::write(
            decoy.join("git"),
            format!(
                "#!/bin/sh\ncase \"$* $PWD\" in *{repo}*) : > {forked} ;; esac\nPATH={real} exec git \"$@\"\n",
                repo = crate::util::sh_quote(repo.to_str().unwrap()),
                forked = crate::util::sh_quote(forked.to_str().unwrap()),
                real = crate::util::sh_quote(&real),
            ),
        )
        .unwrap();
        fs::set_permissions(decoy.join("git"), fs::Permissions::from_mode(0o755)).unwrap();

        // `/bin:/usr/bin` is what glibc falls back to when `PATH` is unset — the smallest tail that
        // leaves every other test in this process able to start a program.
        env::set_var("PATH", format!("{}:/bin:/usr/bin", decoy.display()));
        let branch = git_branch_for(repo.to_str().unwrap());
        env::set_var("PATH", real);
        assert_eq!(branch.as_deref(), Some("no-git"));
        assert!(
            !forked.exists(),
            "git_branch_for forked git for {} — the decoy first on the PATH was run, so the \
             branch on the board costs a process per box after all",
            repo.display()
        );
    }

    // The takeover path reaches into the source box for its branch and HEAD, and until now nothing
    // exercised that. The guard is deliberately the *argv*: `Place` is about to change how skein
    // addresses a box, and this is the contract it must not silently alter.
    /// Four different faults, four different sentences.
    ///
    /// `fleet_boxes` answers `Option`, so a caller can only say "no fleet" — and every one of them
    /// said a version of "sbx did not answer". That is true of one of the four, and it sent a user
    /// looking at the wrong thing: their `sbx ls` worked in a terminal, so the message read as skein
    /// being wrong rather than as skein being started without the PATH that finds it.
    #[test]
    fn a_fleet_that_could_not_be_listed_says_why_not() {
        let _g = env_lock();
        let home = tempdir();
        env::set_var("SKEIN_HOME", &home);

        env::set_var("SKEIN_LS_CMD", "echo 'sbx: daemon not running' >&2; exit 3");
        assert!(fleet_boxes().is_none());
        let why = fleet_failure().expect("a failure with no reason is the bug being fixed");
        assert!(why.contains("exited 3"), "{why}");
        // What it said on the way out. A non-zero exit with the tool's own complaint discarded is a
        // diagnosis thrown away at the only moment it was available.
        assert!(why.contains("daemon not running"), "{why}");

        // Answered, but not with a listing — an older sbx, a `--json` it does not know, a wrapper
        // printing a banner. Reported as "did not answer" this looks like a dead daemon.
        env::set_var("SKEIN_LS_CMD", "echo not-json-at-all");
        assert!(fleet_boxes().is_none());
        let why = fleet_failure().expect("a reason");
        assert!(why.contains("not with a fleet listing"), "{why}");
        assert!(
            why.contains("not-json-at-all"),
            "the output it did give: {why}"
        );

        // And a success clears it, so a stale reason is never reported over a working fleet.
        env::set_var("SKEIN_LS_CMD", "echo '[]'");
        assert_eq!(fleet_boxes().map(|b| b.len()), Some(0));
        assert_eq!(fleet_failure(), None);

        env::remove_var("SKEIN_LS_CMD");
        env::remove_var("SKEIN_HOME");
    }

    fn by_name<'a>(v: &'a [SbxBox], n: &str) -> &'a SbxBox {
        v.iter().find(|b| b.name == n).expect("box present")
    }

    #[test]
    fn parse_boxes_tolerates_shapes() {
        // NDJSON (Docker-CLI --json), mixed casing, an unknown status, and a stopped box.
        let nd = r#"{"name":"a","status":"running"}
{"SANDBOX":"b","STATUS":"stopped"}
{"name":"c","status":"paused"}"#;
        let v = parse_boxes(nd);
        assert_eq!(by_name(&v, "a").live, Some(Liveness::Running));
        assert_eq!(by_name(&v, "b").live, Some(Liveness::Stopped));
        assert_eq!(by_name(&v, "c").live, None); // unknown status → box still listed, falls back

        // A single JSON array document with a workspace path.
        let arr = r#"[{"name":"x","state":"running","workspace":"/repo/x"}]"#;
        assert_eq!(by_name(&parse_boxes(arr), "x").dir, "/repo/x");
        assert_eq!(
            by_name(
                &parse_boxes(r#"{"name":"solo","status":"running"}"#),
                "solo"
            )
            .live,
            Some(Liveness::Running)
        );

        // A name-keyed object map: {name: {..}}.
        let obj = r#"{"z":{"status":"running"}}"#;
        assert_eq!(
            by_name(&parse_boxes(obj), "z").live,
            Some(Liveness::Running)
        );

        // Garbage is unavailable; a valid empty document is authoritative.
        assert!(parse_boxes("not json").is_empty());
        assert!(parse_boxes("[]").is_empty());
        assert!(parse_boxes_checked("not json").is_none());
        assert_eq!(parse_boxes_checked("[]").unwrap().len(), 0);
        assert_eq!(parse_boxes_checked(r#"{"sandboxes":[]}"#).unwrap().len(), 0);
        assert!(parse_boxes_checked(r#"{"error":"API error"}"#).is_none());
        // Invalid names are skipped.
        assert!(parse_boxes(r#"[{"name":"../escape","status":"running"}]"#).is_empty());
    }

    #[test]
    fn parse_boxes_real_sbx_schema() {
        // The actual `sbx ls --json` shape (captured from the host): a {"sandboxes":[...]} wrapper,
        // lowercase name/status/agent, and a workspaces array (repo + shared .claude store).
        let real = r#"{
          "sandboxes": [
            { "name": "claude-agent-memory-consolidation", "id": "59eb", "agent": "claude",
              "status": "stopped", "workspaces": ["/x/agent-memory-consolidation"] },
            { "name": "thing-feat-calender", "id": "2b78", "agent": "claude", "status": "running",
              "ports": [{"host_ip":"127.0.0.1","host_port":49161,"sandbox_port":9418,"protocol":"tcp"}],
              "workspaces": ["/x/gadget-demo", "/x/skein-shared/.claude"] },
            { "name": "thing-master", "id": "f1bf", "agent": "claude", "status": "running",
              "workspaces": ["/x/thing"] }
          ]
        }"#;
        let v = parse_boxes(real);
        assert_eq!(v.len(), 3);
        let calender = by_name(&v, "thing-feat-calender");
        assert_eq!(calender.live, Some(Liveness::Running));
        assert_eq!(calender.agent, "claude");
        // the repo workspace is chosen, the shared .claude store is excluded.
        assert_eq!(calender.dir, "/x/gadget-demo");
        assert_eq!(
            by_name(&v, "claude-agent-memory-consolidation").live,
            Some(Liveness::Stopped)
        );
        assert_eq!(by_name(&v, "thing-master").dir, "/x/thing");
    }

    #[test]
    fn transient_fleet_failure_reuses_last_good_and_empty_success_replaces_it() {
        let box_ = SbxBox {
            name: "live-box".into(),
            agent: "codex".into(),
            live: Some(Liveness::Running),
            dir: "/work".into(),
        };
        // A minute, so nothing here ages out mid-test: what is under test is which answer the gate
        // hands back, not when it decides to ask again.
        let long = Duration::from_secs(60);
        // Static because the gate now refreshes behind its caller, which needs it to outlive the
        // call. Every `get` here is preceded by an `invalidate`, so nothing carries between cases.
        static GATE: Gate<Vec<SbxBox>> = Gate::new();
        let gate = &GATE;
        gate.invalidate();

        assert_eq!(gate.get(long, || Some(vec![box_])).unwrap().len(), 1);
        assert!(!gate.degraded());

        // A failure must not report the fleet gone — the board would blank itself on one slow tick.
        gate.invalidate();
        assert_eq!(gate.get(long, || None).unwrap()[0].name, "live-box");
        assert!(gate.degraded());

        // An empty fleet is an *answer*, not a failure: it replaces last-known-good, and the boxes
        // that were there do not come back the next time sbx cannot be reached.
        gate.invalidate();
        assert!(gate.get(long, || Some(vec![])).unwrap().is_empty());
        assert!(!gate.degraded());
        gate.invalidate();
        assert!(gate.get(long, || None).unwrap().is_empty());
        assert!(gate.degraded());
    }

    // `shared-paths.txt` surfaces a repo's gitignored essentials into a box — `.env`, and for some
    // repos the `CLAUDE.md` the box takes its direction from. It read them from
    // `/run/sandbox/source`, a mount only `--clone` mode has, so in the fleet the whole mechanism
    // was inert: not broken links, *nothing*, and nothing said so.
    //
    // In the fleet the mirror is the repo's real work tree, mounted read-write — so every entry
    // takes the `rw` shape there, seeded into the store and linked from there. A box must not be
    // able to edit the host's own checkout, which the read-only bind used to guarantee for free.
    // Three resolutions that all asked `sbx ls` who a box is. A box in the fleet is not a sandbox
    // and never appears there, so each one quietly failed for the whole fleet: its workspace was
    // unknown, a takeover of it refused, and a replacement could be handed the name of a live box.
    #[test]
    fn a_box_in_the_fleet_can_still_be_found_by_name() {
        let _g = env_lock();
        let home = tempdir();
        env::set_var("SKEIN_HOME", &home);
        env::set_var("SKEIN_REGISTRY", home.join("sandboxes.json"));
        fs::write(home.join("sandboxes.json"), "{}").unwrap();
        // `sbx ls` knows nothing — the fleet's normal state.
        env::set_var("SKEIN_LS_CMD", "echo '[]'");
        save_config(&Config {
            fleet_sandbox: "skein-fleet".into(),
            ..load_config()
        })
        .unwrap();
        record_place(
            "web-main",
            &PlaceRecord {
                sandbox: "skein-fleet".into(),
                ns_pid: 1,
                home: "/boxes/web-main/home".into(),
                tree: "/boxes/web-main/tree".into(),
                sock: "/boxes/web-main/session.sock".into(),
                generation: "test-boot".into(),
                ns_start: 1,
                ..Default::default()
            },
        )
        .unwrap();

        assert_eq!(
            lookup_dir("web-main").as_deref(),
            Some("/boxes/web-main/tree"),
            "a placed box knows its own checkout even when sbx has never heard of it"
        );
        // And a name already taken by a fleet box must not be handed out again.
        let repo = Repo {
            read_prs: false,
            id: "web".into(),
            source: String::new(),
            store: String::new(),
            agent: "claude".into(),
            plane_project: String::new(),
            sync_connection: String::new(),
            review_queue: true,
            sync_gateway_url: String::new(),
            ..Default::default()
        };
        record_place(
            "web-main-codex",
            &PlaceRecord {
                sandbox: "skein-fleet".into(),
                ns_pid: 2,
                home: "/boxes/web-main-codex/home".into(),
                tree: "/boxes/web-main-codex/tree".into(),
                sock: "/boxes/web-main-codex/session.sock".into(),
                generation: "test-boot".into(),
                ns_start: 1,
                ..Default::default()
            },
        )
        .unwrap();
        let picked = replacement_name(&repo, "web-main", "main", "codex");
        assert_ne!(
            picked, "web-main-codex",
            "that name is a live box in the fleet; `start_box` would refuse its existing tree"
        );
        forget_place("web-main-codex");

        env::remove_var("SKEIN_LS_CMD");
        env::remove_var("SKEIN_REGISTRY");
        forget_place("web-main");
        env::remove_var("SKEIN_HOME");
        *REPOS_CACHE.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }

    // Migration STOPS the old sandbox rather than destroying it — that is the undo. So `sbx ls`
    // goes on listing a stopped sandbox with the box's name long after the box itself moved into
    // the fleet and is running there. Read liveness off that row and every migrated box shows
    // `stale` on the board while working, which is exactly what happened: the board and the per-box
    // session view disagreed about the same box at the same moment, because only one of them asked
    // `box_liveness`.
    #[test]
    fn a_migrated_boxs_stopped_old_sandbox_does_not_make_it_look_dead() {
        let _g = env_lock();
        let home = tempdir();
        env::set_var("SKEIN_HOME", &home);
        env::set_var("SKEIN_REGISTRY", home.join("sandboxes.json"));
        fs::write(
            home.join("sandboxes.json"),
            format!(
                r#"{{"demo-task":{{"branch":"b","dir":"/boxes/demo-task/tree","lastSeen":"{}","status":"waiting"}}}}"#,
                secs_ago(20)
            ),
        )
        .unwrap();
        // The husk the migration left behind, still carrying the box's name.
        env::set_var(
            "SKEIN_LS_CMD",
            r#"echo '[{"name":"skein-fleet"},{"name":"demo-task","status":"stopped"}]'"#,
        );
        let mut config = load_config();
        config.fleet_sandbox = "skein-fleet".into();
        save_config(&config).unwrap();
        // The fleet's own liveness sweep — the only thing that knows whether the box is running.
        //
        // It was a fake `sbx` on `$PATH`, standing in for the sweep's `sbx exec` hop; there is no
        // hop (SKEIN-576), so the fake was bypassed and the sweep read the real `/boxes`
        // (SKEIN-530). The sweep's actual question for a box whose anchor is from another boot is
        // whether something accepts on its socket, so this answers it by binding one — and the two
        // states below are a live listener and the stale socket file it leaves behind.
        let root = home.join("boxes");
        env::set_var("SKEIN_FLEET_ROOT", &root);
        fs::create_dir_all(root.join("demo-task")).unwrap();
        let sock = root.join("demo-task/session.sock");

        record_place(
            "demo-task",
            &PlaceRecord {
                sandbox: "skein-fleet".into(),
                ns_pid: 1,
                home: "/boxes/demo-task/home".into(),
                tree: "/boxes/demo-task/tree".into(),
                sock: sock.to_string_lossy().into_owned(),
                generation: "test-boot".into(),
                ns_start: 1,
                ..Default::default()
            },
        )
        .unwrap();

        let state_of = || {
            load_views()
                .unwrap()
                .into_iter()
                .find(|v| v.name == "demo-task")
                .expect("the migrated box must still be on the board")
                .state
        };

        let listening = std::os::unix::net::UnixListener::bind(&sock).expect("a listener");
        assert_eq!(
            state_of(),
            "waiting",
            "the box's own turn state, not the state of the sandbox it moved out of"
        );

        // And the reverse, so this is a fix rather than a suppression: when the box's session really
        // is gone, the board still says so. The socket file outlives the server, which is precisely
        // the husk-shaped case this test is about — one layer down.
        drop(listening);
        assert_eq!(state_of(), "stale");

        forget_place("demo-task");
        env::remove_var("SKEIN_FLEET_ROOT");
        env::remove_var("SKEIN_LS_CMD");
        env::remove_var("SKEIN_REGISTRY");
        env::remove_var("SKEIN_HOME");
        *REPOS_CACHE.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }

    /// `sbx ls` is a question about the machine, and in-fleet skein is not standing on it.
    ///
    /// The answer is the same `None` a missing or wedged sbx gives — callers already fall back to
    /// the registry, so nothing new has to learn a third state. What is new is the reason: without
    /// it the board reports a broken sbx for a deployment where its absence is correct, which is a
    /// fault nobody can clear sitting on top of the one thing they wanted to know.
    #[test]
    fn in_the_fleet_the_machine_listing_is_a_question_skein_cannot_put() {
        let _g = crate::testutil::env_lock();
        env::remove_var("SKEIN_LS_CMD");
        crate::fleet::disturbing_liveness(|| ());
        FLEET_GATE.invalidate();

        assert!(
            fleet_boxes().is_none(),
            "skein answered a question about the host's machine from inside the sandbox"
        );
        let why = fleet_failure().unwrap_or_default();
        assert!(
            why.contains("inside the fleet") && why.contains("placement records"),
            "the reason does not say it is the deployment rather than a broken sbx: {why:?}"
        );

        // An override still wins. Something that CAN answer the question is answering it, whatever
        // this process happens to be running inside — a test, a proxy, a future host-side reporter.
        env::set_var("SKEIN_LS_CMD", r#"echo '[{"name":"skein-fleet"}]'"#);
        FLEET_GATE.invalidate();
        let listed = fleet_boxes().expect("an override answers even in-fleet");
        assert!(listed.iter().any(|b| b.name == "skein-fleet"));

        env::remove_var("SKEIN_LS_CMD");
        FLEET_GATE.invalidate();
    }
}
