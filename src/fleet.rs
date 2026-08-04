//! The one sandbox that hosts many boxes.
//!
//! An sbx sandbox is a microVM, and its memory is a *reservation*: skein's original model gives each
//! box one, so five boxes reserve five times the peak even though four of them are idle in an editor.
//! On a 36 GB machine that is what makes the host start refusing work. The boxes' needs are spiky and
//! rarely simultaneous — a build wants 5 GB for two minutes, editing wants almost nothing — so the
//! fix is to make memory a pool they share rather than N ceilings that sum.
//!
//! So: one sandbox, many boxes, each in its own bwrap namespace with its own `/tmp`, `$HOME` and
//! checkout (see `box-session.sh`). Inside a VM, cgroup limits are *ceilings* rather than
//! reservations — a box capped at 8 GB that uses 200 MB costs 200 MB — which is the whole reason
//! this shape wins.
//!
//! Everything here is inert until [`crate::place::fleet_sandbox`] names a sandbox. Boxes already
//! running as their own VM keep running that way: [`crate::place::place_of`] follows a box's own
//! record, so turning this on never retroactively reinterprets one.

use crate::config::*;
use crate::place::{
    fleet_sandbox, forget_place, own_sandbox, place_of, placed_boxes, record_place, shared_record,
    Place, PlaceRecord,
};
use crate::repos::{
    branch_of, is_git_url, is_ssh_url, launch_spec, load_repos, remote_origin_url, repo_for_box,
    write_launch_spec_for_agent, Repo,
};
use crate::util::*;
use crate::{agent_for_box, fleet_boxes, skein_home, valid_name, KIT_STARTUP_SH};
use chrono::Utc;
use std::io::IsTerminal;
use std::time::Duration;

/// The launcher, embedded so it can be installed into a sandbox that has never seen this repo.
/// The fleet sandbox hosts boxes from *many* repos, so it cannot be served out of any one repo's
/// store — and shipping it through a store would put runtime tooling in shared data besides.
const BOX_SESSION_SH: &str = include_str!("box-session.sh");

/// Where box roots live inside the fleet sandbox.
///
/// Deliberately not under `$HOME` or `/tmp`: `box-session.sh` binds the box's own directories over
/// both, so a root beneath either would be visible only from inside the box that owns it — and the
/// tmux socket and anchor pidfile that skein reads from outside live in this root.
///
/// `$SKEIN_FLEET_ROOT` overrides it, in the same spirit as `$SKEIN_LS_CMD` and `$SKEIN_LAUNCH_CMD`:
/// `/boxes` needs root to create, so without this seam the launch path could only ever be exercised
/// against a real sandbox — which is precisely the part that kept going untested.
pub fn fleet_root() -> String {
    std::env::var("SKEIN_FLEET_ROOT")
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "/boxes".to_string())
}

/// Where the launcher is installed inside the fleet sandbox.
pub fn box_session_path() -> String {
    format!("{}/.skein/box-session.sh", fleet_root())
}

/// Where the provisioning script is installed inside the fleet sandbox.
///
/// Beside the launcher rather than in `~/.local/bin` (where the kit puts it) for two reasons: a box
/// binds its own `$HOME` over the sandbox's, and the fleet sandbox is created without a kit at all,
/// so nothing would have put it there. Under `--dev-bind / /` this path reads the same from inside
/// every box as it does from the sandbox.
pub fn box_provision_path() -> String {
    format!("{}/.skein/skein-startup.sh", fleet_root())
}

/// One box's root inside the fleet sandbox. Callers must have validated `name`; every path skein
/// derives for a box hangs off this, so a name containing `..` would escape the layout entirely.
pub fn box_root(name: &str) -> String {
    format!("{}/{name}", fleet_root())
}

/// The box's tmux socket — the same path inside the namespace and out, which is what lets skein
/// list, attach to and kill a box without entering it first.
pub fn box_sock(name: &str) -> String {
    format!("{}/session.sock", box_root(name))
}

/// The file `box-session.sh` writes the box's anchor pid to. Read from outside, so it must sit in
/// the box root rather than in the box's private `/tmp`.
pub fn box_pidfile(name: &str) -> String {
    format!("{}/anchor.pid", box_root(name))
}

/// Where boxes keep the state that must outlive the sandbox, on the **host**.
///
/// A host path, mounted into the sandbox at that same absolute path — so this one string addresses
/// it from both sides, exactly as a repo store does. The *parent* is mounted, so a box added later
/// needs no recreate.
///
/// Distinct from a repo's store on purpose: this is per-box and skein-owned, not project-scoped
/// shared data, so nothing here crosses between boxes and it is not somewhere the user puts files.
pub fn box_state_root() -> String {
    skein_home().join("boxes").to_string_lossy().into_owned()
}

/// One box's durable host-side state directory.
pub fn box_state(name: &str) -> String {
    format!("{}/{name}", box_state_root())
}

/// The `sbx create` argv for the fleet sandbox.
///
/// `shell` rather than an agent: nothing runs in the sandbox itself — every agent runs inside a box's
/// namespace, started by `box-session.sh`. The mounts are [`fleet_mounts`]: `~/.skein/repos`, the
/// *parent* of every managed repo's store, so adding one later needs no recreate — plus any adopted
/// repo that lives outside it. A box's checkout is not mounted at all, because boxes clone from the
/// remote onto VM-local disk (measured ~5× faster to write and ~14× faster to read than a virtiofs
/// mount, which matters for a build).
///
/// Memory and CPUs come from [`Config`], and both are ceilings the boxes share rather than one
/// reservation each — which is what makes them safe to set generously. CPUs default to every host
/// core but one, so the machine keeps answering while the fleet compiles.
pub fn create_argv(sandbox: &str, mounts: &[String]) -> Vec<String> {
    let config = load_config();
    let mut argv = vec!["create".to_string(), "--name".into(), sandbox.to_string()];
    let memory = config.fleet_memory.trim();
    if !memory.is_empty() {
        argv.push("-m".into());
        argv.push(memory.to_string());
    }
    let cpus = config.fleet_cpus.trim().to_string();
    let cpus = if cpus.is_empty() {
        host_cpus_less_one()
    } else {
        cpus
    };
    if !cpus.is_empty() {
        argv.push("--cpus".into());
        argv.push(cpus);
    }
    argv.push("shell".into());
    argv.extend(mounts.iter().cloned());
    argv
}

/// The environment `sbx create` needs for what its argv cannot carry — today, the sandbox's disk.
///
/// sbx takes memory and CPUs as flags but reads disk sizes from its *daemon's* environment
/// (documented: root filesystem 20 GB by default, `DOCKER_SANDBOXES_ROOT_SIZE` to change it). So
/// this only lands if the daemon starts with the create — a daemon already running keeps the size it
/// booted with, and the fleet's disk is fixed for the life of the sandbox either way.
///
/// One shared 20 GB disk is the fleet's real ceiling. Memory stopped summing when boxes started
/// sharing a sandbox; disk started summing for exactly the same reason.
pub fn create_env() -> Vec<(String, String)> {
    let disk = load_config().fleet_disk.trim().to_string();
    if disk.is_empty() {
        return Vec::new();
    }
    vec![("DOCKER_SANDBOXES_ROOT_SIZE".to_string(), disk)]
}

/// Every host directory the fleet sandbox must be able to see.
///
/// [`fleet_workspace`] covers repos skein manages, whose work clone and store both live under it.
/// It does **not** cover a repo adopted in place, or one pointed at a store the user already had —
/// `skein add <path> --store …`, which is how this very project is registered. Those sit anywhere on
/// the host, so they are mounted explicitly or the box cannot read its own store, and provisioning
/// fails for a reason that reads as a skein bug rather than a missing mount.
///
/// Deduped against the workspace, and against each other: mounting a path twice is not obviously
/// harmless, and mounting a *parent* of it is what keeps a later repo from needing a recreate.
pub fn fleet_mounts() -> Vec<String> {
    let workspace = fleet_workspace();
    // The box-state parent too: boxes keep their conversation there, on the host, so it survives the
    // sandbox rather than only surviving a planned resize.
    let mut mounts = vec![workspace.clone(), box_state_root()];
    for repo in load_repos() {
        for path in [repo.store.clone(), repo.work.clone()] {
            let path = path.trim().to_string();
            if path.is_empty() {
                continue;
            }
            if mounts.iter().any(|m| under(&path, m)) {
                continue;
            }
            mounts.push(path);
        }
    }
    mounts
}

/// Is `path` inside `dir` (or `dir` itself)? Textual, because both are host absolute paths skein
/// wrote or normalised, and the answer is needed before any sandbox exists to ask.
fn under(path: &str, dir: &str) -> bool {
    let dir = dir.trim_end_matches('/');
    path == dir || path.starts_with(&format!("{dir}/"))
}

/// The per-box cgroup limits, as the `key=value,…` spec `box-session.sh` applies.
///
/// Memory only. **CPU is deliberately not capped**: `cpu.weight` is already equal for every box, so
/// they fair-share under contention and a lone box still gets every core — and capping it would
/// leave cores idle while a box waits, which is the exact waste the shared sandbox exists to end.
/// Memory is different because it is not reclaimable on demand: two boxes wanting 20 GB do not each
/// get 13 slowly, they hit the wall and the kernel starts killing things.
///
/// `max` is what stops one box taking the fleet down with it. `high` sits below it so the kernel
/// throttles and reclaims first — a box that briefly overshoots gets slower rather than losing its
/// turn. Defaults derive from the fleet total: 70% and 55%, so one box can still run a big build
/// while two of them cannot exhaust the VM between them.
///
/// `pids.max` is the fork-bomb guard; a runaway spawn loop in one box would otherwise exhaust the
/// VM's pid space and no box could start a process.
pub fn box_limits() -> String {
    let config = load_config();
    let fleet_mib = parse_mib(&config.fleet_memory);
    let pick = |explicit: &str, fraction: u64| -> Option<String> {
        let explicit = explicit.trim();
        if !explicit.is_empty() {
            return Some(explicit.to_string());
        }
        fleet_mib.map(|total| format!("{}M", (total * fraction / 100).max(512)))
    };
    let mut parts = Vec::new();
    if let Some(max) = pick(&config.box_memory_max, 70) {
        parts.push(format!("max={max}"));
    }
    if let Some(high) = pick(&config.box_memory_high, 55) {
        parts.push(format!("high={high}"));
    }
    parts.push("pids=8192".to_string());
    parts.join(",")
}

/// Why a box has no memory ceiling, or `None` when it has one.
///
/// Read from the file `box-session.sh` leaves behind rather than from its stderr: skein keeps a
/// command's stdout and discards stderr on success, so a warning about a box that started *fine*
/// would be dropped exactly when nothing looked wrong. The fact outlives the launch that produced
/// it, which is what lets anything later — a doctor check, a row on the board — still ask.
pub fn uncapped_reason(name: &str) -> Option<String> {
    let sandbox = fleet_sandbox();
    if sandbox.is_empty() {
        return None;
    }
    let path = format!("{}/limits.state", box_root(name));
    let state = own_sandbox(&sandbox)
        .exec(&format!("cat {}", sh_quote(&path)), Duration::from_secs(15))
        .ok()?;
    let mut parts = state.split_whitespace();
    match parts.next()? {
        "uncapped" => Some(parts.next().unwrap_or("reason not recorded").to_string()),
        _ => None,
    }
}

/// Apply the current per-box ceilings to every box that is already running.
///
/// Unlike the fleet's own memory, a cgroup limit is **live**: writing `memory.max` changes the cap
/// on a running box immediately, with no restart and nothing to save or restore. So a tighter or
/// looser per-box ceiling is a setting you can simply change, and it would be wrong to make the user
/// rebuild the fleet for it — that is the expensive path, and this is not.
///
/// Returns the boxes whose limits could not be written. Best-effort per box on purpose: one box
/// missing its cgroup (started before delegation existed, say) must not stop the others being
/// corrected.
pub fn apply_box_limits() -> Result<Vec<String>, String> {
    let sandbox = fleet_sandbox();
    if sandbox.is_empty() {
        return Err("no fleet sandbox configured".into());
    }
    let limits = box_limits();
    let fleet = own_sandbox(&sandbox);
    let mut failed = Vec::new();
    for (name, _) in placed_boxes(&sandbox) {
        let mut writes = Vec::new();
        for kv in limits.split(',') {
            let Some((key, value)) = kv.split_once('=') else {
                continue;
            };
            let file = match key {
                "max" => "memory.max",
                "high" => "memory.high",
                "pids" => "pids.max",
                _ => continue,
            };
            writes.push(format!(
                "printf '%s\\n' {} | sudo tee /sys/fs/cgroup/skein/{}/{file} >/dev/null",
                sh_quote(value),
                name
            ));
        }
        // `test -d` first, so a box with no cgroup is reported rather than counted as adjusted.
        let script = format!(
            "test -d /sys/fs/cgroup/skein/{name} || exit 1; {}",
            writes.join(" && ")
        );
        if fleet.exec(&script, Duration::from_secs(30)).is_err() {
            failed.push(name);
        }
    }
    Ok(failed)
}

/// A memory size as MiB. Accepts what sbx accepts (`26g`, `512M`, a bare byte count).
///
/// `None` rather than a guess when it cannot be read: a mis-parsed ceiling is worse than no ceiling,
/// because it would silently cap every box at some number nobody chose.
fn parse_mib(value: &str) -> Option<u64> {
    let value = value.trim().to_lowercase();
    // `26gi` and `26g` are the same size, so drop the `i` before looking at the unit — reading it as
    // the unit is exactly the mis-parse this function exists to avoid.
    let value = value.strip_suffix('i').unwrap_or(&value);
    let unit = value.chars().last()?;
    if unit.is_ascii_digit() {
        // A bare number: sbx reads it as bytes.
        return value.parse::<u64>().ok().map(|b| b / (1024 * 1024));
    }
    let digits = value[..value.len() - unit.len_utf8()].trim();
    let n = digits.parse::<u64>().ok()?;
    match unit {
        'g' => Some(n * 1024),
        'm' => Some(n),
        'k' => Some(n / 1024),
        _ => None,
    }
}

/// Every host CPU but one, so the host stays responsive while the fleet is busy. Empty when the
/// count cannot be read, which leaves the flag off and sbx's own default in charge.
fn host_cpus_less_one() -> String {
    std::thread::available_parallelism()
        .map(|n| n.get().saturating_sub(1).max(1).to_string())
        .unwrap_or_default()
}

/// The host directory the fleet sandbox mounts: the parent of every repo's store.
///
/// sbx mounts a workspace at its **host absolute path** (verified in a real sandbox — the host's
/// `/Users/…/.skein/repos` is that same path inside the guest). That is worth more than it looks:
/// `repo.store` is a host path, and it resolves unchanged inside the fleet sandbox, so nothing in
/// skein has to translate one. Mounting the parent rather than each store is what lets a repo be
/// added later without recreating the sandbox.
pub fn fleet_workspace() -> String {
    skein_home().join("repos").to_string_lossy().into_owned()
}

/// Is the fleet sandbox already there?
///
/// `None` when sbx could not be asked at all — which is not the same as "absent", and must not be,
/// or a wedged daemon would have skein try to create a sandbox that already exists.
pub fn fleet_exists(sandbox: &str) -> Option<bool> {
    Some(fleet_boxes()?.iter().any(|b| b.name == sandbox))
}

/// Create the fleet sandbox if it is missing, then install the launcher into it.
///
/// Idempotent: safe to call before every launch, which is how a sandbox the user removed by hand
/// comes back rather than leaving every box unstartable.
pub fn ensure_fleet(sandbox: &str, mounts: &[String]) -> Result<(), String> {
    if !valid_name(sandbox) {
        return Err("invalid fleet sandbox name".into());
    }
    match fleet_exists(sandbox) {
        Some(true) => {}
        Some(false) => {
            let argv = create_argv(sandbox, mounts);
            let args: Vec<&str> = argv.iter().map(String::as_str).collect();
            // `sbx create` confirms before it mounts host directories, and creating the fleet
            // sandbox mounts several. When a terminal is there, hand it over — the question is for
            // the person running the command. When there isn't (the server), capture it, but on a
            // budget that fits booting a microVM rather than the 30s action timeout.
            let env = create_env();
            let failure = if std::io::stdin().is_terminal() {
                match run_attached_env("sbx", &args, &env)? {
                    0 => None,
                    code => Some(format!("sbx exited {code}")),
                }
            } else {
                let (out, err, code) =
                    run_capture_for_env("sbx", &args, Duration::from_secs(900), &env)?;
                match code {
                    0 => None,
                    _ => Some({
                        let detail = if err.trim().is_empty() { out } else { err };
                        detail.trim().to_string()
                    }),
                }
            };
            if let Some(detail) = failure {
                // The hand-run line must carry the environment too, or a fleet configured for a
                // bigger disk is quietly recreated at the default 20 GB by the very command the
                // error told someone to type.
                let prefix = env
                    .iter()
                    .map(|(k, v)| format!("{k}={} ", sh_quote(v)))
                    .collect::<String>();
                return Err(format!(
                    "creating fleet sandbox {sandbox}: {detail}\n\
                     if that was a confirmation you never saw, create it once by hand:\n  {prefix}sbx {}",
                    args.join(" ")
                ));
            }
        }
        None => {
            return Err("sbx did not answer; cannot tell whether the fleet sandbox exists".into())
        }
    }
    ensure_substrate(sandbox)?;
    ensure_fleet_root(sandbox)?;
    // After the substrate (which may have just installed the runtimes) and before any box starts,
    // so a rebuilt sandbox has its login back before the first box seeds from it.
    sync_fleet_login(sandbox);
    ensure_known_hosts(sandbox);
    install_launcher(sandbox)
}

/// Install the tools a box needs in order to exist at all.
///
/// Measured in a real sandbox: the `shell` image ships `bwrap` and `git` but **not `tmux`**, and a
/// box without tmux cannot start — `box-session.sh` refuses, because the session *is* the box.
///
/// skein's kit installs jq and tmux for ordinary boxes, but it cannot serve this one: its startup
/// hook returns early in non-clone mode ("already has an in-repo .claude"), and a fleet sandbox is
/// neither a clone nor a mounted repo. So it provisions its own substrate rather than bending a hook
/// written for a different shape. jq comes along because the store probes that run inside boxes need it.
///
/// bwrap is checked but never installed: without it there is no isolation to be had, and quietly
/// continuing would give every box the sandbox's own `/tmp` and `$HOME` — the exact collision this
/// design exists to prevent.
pub fn ensure_substrate(sandbox: &str) -> Result<(), String> {
    // apt's output is kept, not discarded: when this step fails it is the only thing that says
    // whether the mirror was unreachable, sudo refused, or the package simply isn't there — and
    // "missing required tools: tmux" with the reason thrown away is a dead end.
    let script = r#"need='';
         command -v tmux >/dev/null 2>&1 || need="$need tmux";
         command -v jq   >/dev/null 2>&1 || need="$need jq";
         # The agent runtimes are substrate too. The `shell` image has neither, and an agent image
         # would only ever carry one of them — so they are installed once, into the sandbox, and
         # every box in it shares them. Measured on a real sandbox: 6s and 3s. What stays per box is
         # the state, which box-session.sh keeps private; only the binaries are shared.
         npm="$SKEIN_RUNTIME_PACKAGES";
         command -v bwrap >/dev/null 2>&1 || { echo 'skein: this sandbox image has no bwrap; boxes cannot be isolated in it' >&2; exit 1; };
         log=/tmp/skein-substrate.log;
         want=""; for p in $npm; do
           case "$p" in
             *claude-code) command -v claude >/dev/null 2>&1 || want="$want $p" ;;
             *codex)       command -v codex  >/dev/null 2>&1 || want="$want $p" ;;
             *)            want="$want $p" ;;
           esac;
         done; npm="$want";
         [ -n "$need" ] || [ -n "$npm" ] || exit 0;
         [ -n "$need" ] || { timeout 300 sudo npm install -g $npm >>"$log" 2>&1 || true; exit 0; };
         # A freshly created sandbox is still running its own first-boot apt, and apt refuses to run
         # twice. Outlast it rather than failing the launch on a race: measured on a real rebuild,
         # where the retry landed on "Could not get lock ... held by process 281 (apt-get)".
         waited=0;
         while [ "$waited" -lt 120 ]; do
           if sudo fuser /var/lib/dpkg/lock-frontend /var/lib/apt/lists/lock >/dev/null 2>&1; then
             sleep 3; waited=$((waited + 3));
           else break; fi;
         done;
         # update FIRST. A fresh image ships an empty index, where install reports "Package 'tmux'
         # has no installation candidate" — which reads as a missing package and is a missing index.
         { timeout 180 sudo apt-get update -qq; \
           timeout 240 sudo apt-get install -y -qq $need \
             || { sleep 5; timeout 180 sudo apt-get update -qq; \
                  timeout 240 sudo apt-get install -y -qq $need; }; } >"$log" 2>&1;
         if [ -n "$npm" ] && command -v npm >/dev/null 2>&1; then
           timeout 300 sudo npm install -g $npm >>"$log" 2>&1 || true;
         fi;
         missing=''; for t in $need; do command -v "$t" >/dev/null 2>&1 || missing="$missing $t"; done;
         [ -z "$missing" ] || {
             echo "skein: the fleet sandbox is missing required tools:$missing";
             echo "skein: apt said (tail of $log inside the sandbox):";
             tail -n 25 "$log" | sed 's/^/  | /';
             exit 1;
         } >&2"#;
    // The packages are named by the caller, not by the script, so a harness can ask for none.
    // Without that seam the integration test — whose `sbx exec` runs on the developer's own machine
    // — npm-installs an agent runtime onto it, which is both a 50s test and software nobody asked
    // for. $SKEIN_RUNTIME_PACKAGES set to empty means "install no runtimes".
    let packages = std::env::var("SKEIN_RUNTIME_PACKAGES")
        .unwrap_or_else(|_| "@anthropic-ai/claude-code @openai/codex".to_string());
    let script = format!(
        "SKEIN_RUNTIME_PACKAGES={}; {script}",
        sh_quote(packages.trim())
    );
    own_sandbox(sandbox)
        .exec(&script, Duration::from_secs(900))
        .map(|_| ())
}

/// Claude's transcript directory for a working directory: every `/` and `.` becomes `-`.
///
/// Not a guess — read off a real box: `/Users/you/.skein/repos/sync/work` is stored as
/// `-Users-you--skein-repos-sync-work` (the `/.` giving the doubled dash).
pub fn transcript_slug(dir: &str) -> String {
    dir.chars()
        .map(|c| if c == '/' || c == '.' { '-' } else { c })
        .collect()
}

/// Point a migrated box's conversation at the directory it now works in.
///
/// The runtimes key a transcript by the cwd it was had in, and migrating a box MOVES that cwd: from
/// the old sandbox's checkout to `/boxes/<name>/tree`. So the conversation travels in the snapshot,
/// lands intact — and the agent looks under a slug for its new path, finds nothing, and starts over.
/// Measured on the first real migration: 25MB of transcript under
/// `-Users-you--skein-repos-sync-work`, and an empty `-boxes-example-box-9-tree` beside it.
///
/// Copy rather than move, and only into an empty destination: the old directory is the record of
/// where that conversation actually happened, and a box that already has a conversation of its own
/// must never have someone else's merged into it.
pub fn realign_transcript(name: &str) -> Result<usize, String> {
    let root = std::path::PathBuf::from(box_state(name)).join("claude-projects");
    let target = root.join(transcript_slug(&format!("{}/tree", box_root(name))));
    let jsonl_count = |dir: &std::path::Path| -> usize {
        std::fs::read_dir(dir)
            .into_iter()
            .flatten()
            .flatten()
            .filter(|e| e.path().extension().is_some_and(|x| x == "jsonl"))
            .count()
    };
    if target.is_dir() && jsonl_count(&target) > 0 {
        return Ok(0); // it has its own conversation; leave it alone
    }
    // The richest sibling is the one worth carrying: a box can accumulate empty slugs from probes
    // and one-off commands run elsewhere.
    let Some(source) = std::fs::read_dir(&root)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir() && *p != target)
        .max_by_key(|p| jsonl_count(p))
        .filter(|p| jsonl_count(p) > 0)
    else {
        return Ok(0);
    };
    std::fs::create_dir_all(&target).map_err(|e| format!("mkdir {}: {e}", target.display()))?;
    let mut moved = 0;
    for entry in std::fs::read_dir(&source).into_iter().flatten().flatten() {
        let from = entry.path();
        if from.extension().is_some_and(|x| x == "jsonl") {
            let to = target.join(entry.file_name());
            if std::fs::copy(&from, &to).is_ok() {
                moved += 1;
            }
        }
    }
    Ok(moved)
}

/// Trust the SSH hosts a box will clone from, once per sandbox.
///
/// A fleet box clones from the remote itself, and an SSH remote needs the host's key in
/// `known_hosts` first. A legacy box got that from its kit; the fleet sandbox never had it, so the
/// first migration of an SSH-remote repo failed with the least helpful pair of errors git produces:
///
///   ssh_askpass: exec(/usr/bin/ssh-askpass): No such file or directory
///   Host key verification failed.
///
/// which reads as a credentials problem and is a host-trust one — with no known host and no
/// terminal, SSH fell back to asking a human who was not there.
///
/// A real connection with `accept-new`, not `ssh-keyscan`: keyscan is answered with "Connection
/// closed by remote host" here while an ordinary `ssh -T` succeeds and records the key itself.
/// (I read that one keyscan failure as "port 22 is closed" and was wrong — the transport is fine.)
/// `accept-new` trusts an unknown host once and still refuses a CHANGED key, which is the property
/// worth keeping. Best-effort: an HTTPS repo needs none of this, and refusing to launch over it
/// would be absurd.
///
/// Every SSH host any box might reach, from both places a repo names one.
///
/// `source` is where a box CLONES from; a repo adopted in place has a path there and an SSH URL on
/// its `origin`, which is where its boxes PUSH. Reading only `source` meant the four adopted repos
/// contributed no hosts at all — precisely the repos whose boxes now have an SSH origin.
fn ssh_hosts() -> Vec<String> {
    let mut hosts: Vec<String> = load_repos()
        .iter()
        .flat_map(|repo| {
            [
                repo.source.clone(),
                remote_origin_url(&repo.work).unwrap_or_default(),
            ]
        })
        .filter(|url| is_ssh_url(url))
        .filter_map(|url| host_of(&url).map(|h| h.to_string()))
        .collect();
    hosts.sort();
    hosts.dedup();
    hosts
}

/// The shell that pins those hosts into whichever `$HOME` it runs in.
fn known_hosts_script(hosts: &[String]) -> String {
    format!(
        "mkdir -p \"$HOME/.ssh\" && chmod 700 \"$HOME/.ssh\"; \
         for h in {hosts}; do \
           ssh-keygen -F \"$h\" >/dev/null 2>&1 && continue; \
           timeout 25 ssh -o BatchMode=yes -o StrictHostKeyChecking=accept-new -T \"git@$h\" \
             >/dev/null 2>&1; \
         done; exit 0",
        hosts = hosts
            .iter()
            .map(|h| sh_quote(h))
            .collect::<Vec<_>>()
            .join(" "),
    )
}

pub fn ensure_known_hosts(sandbox: &str) {
    let hosts = ssh_hosts();
    if hosts.is_empty() {
        return;
    }
    if let Err(e) = own_sandbox(sandbox).exec(&known_hosts_script(&hosts), Duration::from_secs(120))
    {
        eprintln!("skein: could not pin SSH host keys in {sandbox} ({e}); a box cloning over SSH will fail host key verification");
    }
}

/// The same trust, inside the box — where the agent's own `git push` runs.
///
/// The sandbox's `known_hosts` does not reach a box: every box has a private HOME, and that is the
/// point of it. Cloning never noticed because it runs in the sandbox namespace, so the gap only
/// showed when a box first pushed to a real remote and got `Host key verification failed` — read as
/// a credentials problem, and the credentials were fine the whole time.
pub fn ensure_box_known_hosts(name: &str) {
    let hosts = ssh_hosts();
    if hosts.is_empty() {
        return;
    }
    let Some(place) = place_of(name) else { return };
    if let Err(e) = place.exec(&known_hosts_script(&hosts), Duration::from_secs(120)) {
        eprintln!("skein: could not pin SSH host keys in {name} ({e}); pushing over SSH from it will fail host key verification");
    }
}

/// Create the fleet root and hand it to the sandbox user.
///
/// [`fleet_root`] defaults to `/boxes` — at the filesystem root, where a non-root user cannot mkdir.
/// Everything after this point (the launcher, every box root, every tmux socket) is created with a
/// plain `mkdir -p` by the sandbox user, so all of it fails until this runs once. The integration
/// test never caught it precisely because `$SKEIN_FLEET_ROOT` points it at a writable temp dir —
/// the seam that makes the launch path testable is also the seam that hid its first real step.
pub fn ensure_fleet_root(sandbox: &str) -> Result<(), String> {
    let root = sh_quote(&fleet_root());
    let script = format!(
        "[ -w {root} ] && exit 0; \
         sudo mkdir -p {root} && sudo chown \"$(id -u):$(id -g)\" {root} && chmod 755 {root}"
    );
    own_sandbox(sandbox)
        .exec(&script, Duration::from_secs(60))
        .map(|_| ())
        .map_err(|e| format!("preparing the fleet root {root}: {e}"))
}

/// Write `box-session.sh` and the provisioning script into the sandbox, over stdin rather than as
/// arguments — both are large and `sbx exec`'s argv is visible in every process listing on the host.
pub fn install_launcher(sandbox: &str) -> Result<(), String> {
    for (path, body) in [
        (box_session_path(), BOX_SESSION_SH),
        (box_provision_path(), KIT_STARTUP_SH),
    ] {
        let dir = path.rsplit_once('/').map(|(d, _)| d).unwrap_or("/boxes");
        let script = format!(
            "mkdir -p {} && cat > {} && chmod 755 {}",
            sh_quote(dir),
            sh_quote(&path),
            sh_quote(&path)
        );
        // The fleet sandbox itself, not a box inside it — no namespace to enter.
        own_sandbox(sandbox).write(&script, body.as_bytes(), Duration::from_secs(30))?;
    }
    Ok(())
}

/// The shell that prepares a box's checkout inside the fleet sandbox.
///
/// Clones from the **remote** at `base`, then creates or checks out the box's branch. Boxes used to
/// be cloned from the host's own clone, which meant a new box inherited whatever was stale or
/// half-committed there; from the remote it starts from the same base the diff is taken against.
///
/// Refuses rather than reuses when the tree is already populated: a box root left behind by a
/// previous box of the same name would otherwise silently give the new one someone else's work.
///
/// `upstream` repairs the one case where cloning and pushing want different sources. A repo adopted
/// in place has no URL, so the clone comes from the host's own checkout ([`clone_source`]) — and
/// `git clone <path>` sets `origin` to that path, discarding the URL the host clone pushes to. The
/// box then depends on the host for something it should never need it for: skein *validates* the
/// host clone's origin ([`crate::remote_warning`] warns when there isn't one, precisely because "a
/// box can't push or open a PR until one exists") and then provisions a box pointing somewhere else
/// entirely. Pushing worked anyway until a box shared the host's checked-out branch, at which point
/// git refused with a message about `receive.denyCurrentBranch` — a remote-side policy error for
/// what is really a mis-pointed remote.
///
/// So: clone locally, which is fast and carries the host's unpushed commits, then point `origin` at
/// the URL and keep the path as `local`. Nothing is lost and the box pushes where every other box
/// does. Empty when the source is already a URL, or when the host clone has no origin to inherit.
pub fn clone_script(name: &str, url: &str, base: &str, branch: &str, upstream: &str) -> String {
    let root = box_root(name);
    let tree = format!("{root}/tree");
    let tree_q = sh_quote(&tree);
    let url_q = sh_quote(url);
    // An empty base is not a missing value to substitute a guess for — it is skein saying it could
    // not learn the remote's default, and a plain `git clone` asks the remote for it directly.
    //
    // When there IS a base, the clone falls back to the same plain form rather than failing. A base
    // that the remote does not have has always been possible — a configured base a given repo does
    // not use, a cached `origin/HEAD` gone stale after a rename — and the cost was severe out of all
    // proportion: a migration that stops the old sandbox, then cannot start the new box, leaves the
    // box in neither place until someone woke the old sandbox by hand. A tree cloned from the wrong base
    // is a non-event by comparison, since the branch is checked out over it immediately.
    let clone = if base.is_empty() {
        format!("git clone {url_q} {tree_q}")
    } else {
        format!(
            "git clone --branch {base_q} {url_q} {tree_q} || \
             {{ echo 'skein: no {base} on the remote; cloning its default branch instead' >&2; \
                rm -rf {tree_q}; git clone {url_q} {tree_q}; }}",
            base_q = sh_quote(base),
        )
    };
    // Best-effort, and deliberately not under `set -e`: a box whose remote could not be re-pointed
    // is a box that pushes to the host clone, which is where it would have pushed anyway. Failing
    // the whole clone over it would turn a working box into no box.
    let remotes = if upstream.is_empty() {
        String::new()
    } else {
        format!(
            "; git remote add local {url_q} 2>/dev/null || true; \
             git remote set-url origin {up_q} || \
             echo 'skein: could not point origin at {upstream}; this box pushes to the host clone' >&2",
            up_q = sh_quote(upstream),
        )
    };
    format!(
        "set -e; \
         if [ -e {tree_q}/.git ]; then echo 'skein: {name} already has a checkout; destroy the box first' >&2; exit 1; fi; \
         mkdir -p {root_q}; \
         {clone}; \
         cd {tree_q}; \
         git checkout -B {branch_q}{remotes}",
        root_q = sh_quote(&root),
        branch_q = sh_quote(branch),
    )
}

/// How much disk each box is using, in MiB — every box at once, in one round trip.
///
/// Measured rather than enforced, and that distinction is the honest part of this: the fleet's disk
/// is a single filesystem shared by every box, so nothing in the kernel stops one from filling it.
/// A limit here is a number skein checks and reports, not a wall the box hits — which is why it can
/// be changed while a box runs, and why it is worth having at all: the alternative is finding out
/// when some *other* box's build dies with ENOSPC and no indication of who took the space.
///
/// `du -sxm`, one process for the lot: measured at 0.2s for a 3.2 GB tree, so a board tick can pay
/// for it. `-x` keeps it on the sandbox's own filesystem — a box's store is a host mount, and
/// walking virtiofs to count bytes that are not on this disk would be both slow and wrong.
pub fn fleet_disk_usage() -> std::collections::HashMap<String, u64> {
    let sandbox = fleet_sandbox();
    if sandbox.is_empty() {
        return Default::default();
    }
    if !cfg!(test) {
        let cache = DISK_CACHE.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((at, map)) = cache.as_ref() {
            if at.elapsed() < Duration::from_secs(30) {
                return map.clone();
            }
        }
    }
    let script = format!("du -sxm {root}/*/ 2>/dev/null", root = fleet_root());
    let map: std::collections::HashMap<String, u64> = own_sandbox(&sandbox)
        .exec(&script, Duration::from_secs(60))
        .unwrap_or_default()
        .lines()
        .filter_map(|line| {
            let (mb, path) = line.trim().split_once(char::is_whitespace)?;
            let name = path.trim().trim_end_matches('/').rsplit('/').next()?;
            Some((name.to_string(), mb.trim().parse().ok()?))
        })
        .collect();
    if !cfg!(test) {
        *DISK_CACHE.lock().unwrap_or_else(|e| e.into_inner()) =
            Some((std::time::Instant::now(), map.clone()));
    }
    map
}

static DISK_CACHE: std::sync::Mutex<
    Option<(std::time::Instant, std::collections::HashMap<String, u64>)>,
> = std::sync::Mutex::new(None);

/// This box's disk allowance in MiB: its own if it has one, else the fleet-wide default, `None` for
/// unlimited. Read at every check, so changing it takes effect on the next refresh — no restart.
pub fn box_disk_limit(name: &str) -> Option<u64> {
    let own = std::fs::read_to_string(std::path::Path::new(&box_state(name)).join("disk"))
        .ok()
        .map(|s| s.trim().to_string());
    match own {
        // An empty override is a decision — this box is allowed to use the whole disk.
        Some(v) if v.is_empty() => None,
        Some(v) => parse_mib(&v),
        None => parse_mib(&load_config().box_disk_max),
    }
}

/// Give one box a different allowance, or hand it back to the default. Takes effect immediately.
pub fn set_box_disk_limit(name: &str, limit: Option<&str>) -> Result<(), String> {
    let dir = std::path::PathBuf::from(box_state(name));
    let path = dir.join("disk");
    let Some(limit) = limit else {
        return match std::fs::remove_file(&path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                Err(format!("clearing {}: {e}", path.display()))
            }
            _ => Ok(()),
        };
    };
    // Three states, and only two of them are a size: `none` says this box may use the whole disk,
    // which is different from having no opinion (that is `None`, and inherits the default).
    let limit = match limit.trim().to_lowercase().as_str() {
        "none" | "unlimited" => "",
        _ => limit.trim(),
    };
    if !limit.is_empty() && parse_mib(limit).is_none() {
        return Err(format!(
            "{limit:?} is not a size — try 10g, 512m, or `none` for unlimited"
        ));
    }
    std::fs::create_dir_all(&dir).map_err(|e| format!("mkdir {}: {e}", dir.display()))?;
    write_atomic(&path, &dir, limit.as_bytes())
}

/// Who a box commits as: its own choice, else the configured default, else the host clone's.
///
/// Three sources because each answers a different question. A box set at creation is working on
/// someone else's behalf — a shared machine, a different identity per client. The setting is the
/// answer for everything else. And falling back to the host clone means an untouched skein commits
/// as you without anyone configuring anything, because `git -C <work> config user.name` resolves
/// through the host's global gitconfig.
pub fn box_identity(name: &str, repo: &Repo) -> (String, String) {
    let config = load_config();
    let from_host = |key: &str| -> String {
        std::process::Command::new("git")
            .args(["-C", &repo.work, "config", "--get", key])
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_default()
    };
    let pick = |own: Option<String>, configured: &str, key: &str| -> String {
        own.filter(|v| !v.trim().is_empty())
            .unwrap_or_else(|| match configured.trim() {
                "" => from_host(key),
                v => v.to_string(),
            })
    };
    let own = box_identity_override(name);
    (
        pick(own.clone().map(|(n, _)| n), &config.git_name, "user.name"),
        pick(own.map(|(_, e)| e), &config.git_email, "user.email"),
    )
}

/// A box's own committer, recorded at creation. `name\nemail`, beside its other durable state.
pub fn box_identity_override(name: &str) -> Option<(String, String)> {
    let raw =
        std::fs::read_to_string(std::path::Path::new(&box_state(name)).join("identity")).ok()?;
    let mut lines = raw.lines();
    Some((
        lines.next().unwrap_or_default().trim().to_string(),
        lines.next().unwrap_or_default().trim().to_string(),
    ))
}

/// Record (or clear) that committer. `None` returns the box to the configured default.
pub fn set_box_identity(name: &str, who: Option<(&str, &str)>) -> Result<(), String> {
    let dir = std::path::PathBuf::from(box_state(name));
    let path = dir.join("identity");
    let Some((who_name, email)) = who else {
        return match std::fs::remove_file(&path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                Err(format!("clearing {}: {e}", path.display()))
            }
            _ => Ok(()),
        };
    };
    std::fs::create_dir_all(&dir).map_err(|e| format!("mkdir {}: {e}", dir.display()))?;
    let body = format!("{}\n{}\n", who_name.trim(), email.trim());
    write_atomic(&path, &dir, body.as_bytes())
}

/// Set that identity inside the box, so its first commit is not `Author identity unknown`.
///
/// `--global` (the box's own HOME), not the repo: the checkout is re-cloned by a rebuild, a resize
/// or a migration, and a repo-local setting goes with it every time. Only what is missing is
/// written, so an identity someone set in the box by hand is never overwritten.
fn identity_script(name: &str, email: &str) -> String {
    let mut steps = Vec::new();
    if !name.trim().is_empty() {
        steps.push(format!(
            "git config --global --get user.name >/dev/null 2>&1 || git config --global user.name {}",
            sh_quote(name.trim())
        ));
    }
    if !email.trim().is_empty() {
        steps.push(format!(
            "git config --global --get user.email >/dev/null 2>&1 || git config --global user.email {}",
            sh_quote(email.trim())
        ));
    }
    steps.join("; ")
}

/// Point an existing box's `origin` at the repo's remote, if it is still the host clone.
///
/// The clone-time version of this ([`clone_script`]) only helps boxes cloned after it landed. This
/// is the same repair for the ones already on disk, and it runs on every start because there is no
/// other moment that would notice.
///
/// The equality test is the whole safety argument: it rewrites only the URL skein itself put there.
/// An origin someone re-pointed by hand — at a fork, at a mirror — is left exactly alone.
pub fn origin_repair_script(name: &str, source: &str, upstream: &str) -> String {
    format!(
        "cd {tree_q} 2>/dev/null || exit 0; \
         cur=\"$(git remote get-url origin 2>/dev/null || true)\"; \
         [ \"$cur\" = {src_q} ] || exit 0; \
         git remote add local {src_q} 2>/dev/null || true; \
         git remote set-url origin {up_q} && \
         echo 'skein: {name} pushed at the host clone; origin now points at {upstream}' >&2",
        tree_q = sh_quote(&format!("{}/tree", box_root(name))),
        src_q = sh_quote(source),
        up_q = sh_quote(upstream),
    )
}

/// Drop what the *previous* session said about this box, as a new one starts.
///
/// Turn state is a claim about a session — "working", "waiting", "ended" — and it is written into
/// the repo's store, which lives on the host and outlives the box entirely. So a migrated box came
/// up reading `ended`: stopping the old sandbox killed its agent, the SessionEnd hook faithfully
/// recorded that, and the new box inherited a dead session's last word and looked terminated while
/// sitting there perfectly alive. The same would greet every box after a resize.
///
/// Only the claim about the current turn goes. The narrative signal, the telemetry and the hook log
/// are history — they describe what happened, not what is happening, and a box that has just moved
/// is exactly when its history is worth keeping. The in-flight sub-agent counter goes too: it counts
/// processes that died with the old session, and a stale one would make the first Notification of
/// the new session read as "still busy" instead of "needs you".
///
/// Absent files are the normal case (a box being created for the first time), so this is silent.
fn forget_turn_state(repo: &Repo, name: &str) {
    let status = std::path::Path::new(&repo.store).join("status");
    for file in [format!("{name}.json"), format!("{name}.agents")] {
        let _ = std::fs::remove_file(status.join(file));
    }
}

/// The shell that starts a box: its namespace, its tmux server, and the agent inside it.
pub fn session_script(name: &str, session: &str, agent_command: &str) -> String {
    format!(
        "{launcher} {name_q} {root_q} {pid_q} {session_q} {state_q} {limits_q} bash -lc {cmd_q}",
        launcher = sh_quote(&box_session_path()),
        name_q = sh_quote(name),
        root_q = sh_quote(&box_root(name)),
        pid_q = sh_quote(&box_pidfile(name)),
        session_q = sh_quote(session),
        state_q = sh_quote(&box_state(name)),
        limits_q = sh_quote(&box_limits()),
        cmd_q = sh_quote(agent_command),
    )
}

/// The shell that provisions a box: the store link, the branch, the hooks, the guide, the tracker.
///
/// This runs the kit's own startup script — the same bytes sbx runs at startup in a `--clone`
/// sandbox — rather than a fleet-shaped reimplementation of it. Provisioning is a dozen steps and
/// most of them fail *quietly*: a box whose store never got linked looks perfectly healthy and
/// simply never reports. Two implementations of that would be two sets of ways to be silently dark.
///
/// Four env vars carry what the script cannot work out for itself in a shared sandbox, because
/// every signal it normally reads there belongs to the sandbox rather than to the box:
///   * `SKEIN_PROVISION` — say so explicitly, since `/run/sandbox/source` does not exist here;
///   * `SKEIN_BOX`       — the identity, or every box reads one launch spec and one boot report;
///   * `SKEIN_STORE`     — the repo's store, a directory inside the mounted workspace rather than
///     a mount of its own, so the script's scan would find nothing;
///   * `WORKSPACE_DIR`   — the box's checkout, which is not this process's cwd.
///
/// **Must run inside the box's namespace**, not the sandbox: it writes `~/.codex`, `~/.claude` and
/// `~/shared`, and outside the namespace those are the sandbox's, shared by every box.
pub fn provision_script(name: &str, store: &str) -> String {
    format!(
        "SKEIN_PROVISION=1 SKEIN_BOX={name_q} SKEIN_STORE={store_q} WORKSPACE_DIR={tree_q} \
         bash {script_q}",
        name_q = sh_quote(name),
        store_q = sh_quote(store),
        tree_q = sh_quote(&format!("{}/tree", box_root(name))),
        script_q = sh_quote(&box_provision_path()),
    )
}

/// Bring one box up inside the fleet sandbox, from nothing to a running, provisioned agent.
///
/// The ordering is forced by what each step produces rather than chosen: the anchor pid does not
/// exist until the session runs, the placement is meaningless without the anchor, and provisioning
/// must go *through* the placement or it writes the sandbox's `~/.claude` instead of the box's.
///
///   ensure the sandbox → clone the tree → start the session → read the anchor → record the
///   placement → provision inside it
///
/// Idempotent at the sandbox level and deliberately **not** at the box level: `clone_script` refuses
/// a tree that already exists and `box-session.sh` refuses a second server, because both are how a
/// re-run would otherwise hand a box someone else's uncommitted work or strand its namespace.
pub fn start_box(name: &str, repo: &Repo, branch: &str, agent_command: &str) -> Result<(), String> {
    if !valid_name(name) {
        return Err(format!("invalid box name {name:?}"));
    }
    let sandbox = fleet_sandbox();
    if sandbox.is_empty() {
        return Err("no fleet sandbox configured".into());
    }
    ensure_fleet(&sandbox, &fleet_mounts())?;
    // A fleet box has no `/run/sandbox/source`, so this is how it finds the repo's host files to
    // surface `shared-paths.txt` from — `.env`, and the `CLAUDE.md` some repos keep out of git.
    crate::record_repo_mirror(repo);

    // The store is a HOST path used verbatim inside the sandbox, so this is the one precondition
    // worth paying a round-trip for: unreachable, every later step still "succeeds" and the box
    // comes up with no hooks and no probe — the failure this whole path is least able to see.
    let fleet = own_sandbox(&sandbox);
    let probe = format!("test -d {} && echo ok", sh_quote(&repo.store));
    if fleet
        .exec(&probe, Duration::from_secs(20))
        .ok()
        .as_deref()
        .map(str::trim)
        != Some("ok")
    {
        return Err(format!(
            "the fleet sandbox cannot see {}, so box {name} would come up with no store. \
             It is mounted at sandbox creation, so a repo added since then needs \
             `sbx rm {sandbox}` and a relaunch to pick it up.",
            repo.store
        ));
    }

    // A launch that dies partway leaves a checkout, and sometimes a live session, behind. Carry on
    // from there rather than demand the box be destroyed: cloning is the only step here that is not
    // idempotent, and it is also the only one whose work a repeat would throw away.
    let (has_tree, has_session) = box_progress(&fleet, name, "skein-shell")?;
    let source = clone_source(repo);
    // Only an adopted repo needs this: a URL source already clones from the place it pushes to.
    let upstream = match crate::is_git_url(&repo.source) {
        true => String::new(),
        false => remote_origin_url(&repo.work).unwrap_or_default(),
    };
    if has_tree {
        eprintln!("skein: {name} already has a checkout; keeping it");
        // Every box cloned before origin was re-pointed still pushes at the host, and they are not
        // going to be re-cloned to fix it. Guarded on origin still *being* the host clone, so a box
        // whose remote someone set deliberately keeps it.
        if !upstream.is_empty() {
            let _ = fleet.exec(
                &origin_repair_script(name, &source, &upstream),
                Duration::from_secs(60),
            );
        }
    } else {
        fleet.exec(
            &clone_script(name, &source, &base_branch(repo), branch, &upstream),
            Duration::from_secs(600),
        )?;
    }
    if has_session {
        eprintln!("skein: {name} already has a live session; keeping it");
    } else {
        forget_turn_state(repo, name);
        fleet.exec(
            &session_script(name, "skein-shell", agent_command),
            Duration::from_secs(120),
        )?;
    }

    let ns_pid = read_anchor(&sandbox, name)?;
    record_place(
        name,
        &PlaceRecord {
            sandbox: sandbox.clone(),
            ns_pid,
            home: sandbox_home(&fleet)?,
            tree: format!("{}/tree", box_root(name)),
            sock: box_sock(name),
        },
    )?;

    // The launch spec is how the provisioning script learns the box's branch and runtime — without
    // it the box stays on the clone's default branch, which is a silently wrong box rather than a
    // failed one. The legacy path writes it while building the `sbx create` line; this path had no
    // equivalent. Never overwrite a restore's spec: that one also carries the handoff snapshot.
    if launch_spec(repo, name).is_none() {
        write_launch_spec_for_agent(name, branch, repo, &agent_for_box(name))?;
    }

    // Through the placement, so it lands in the box's private HOME rather than the sandbox's.
    let boxed = place_of(name).ok_or_else(|| format!("box {name} was not placed"))?;
    boxed.exec(
        &provision_script(name, &repo.store),
        Duration::from_secs(300),
    )?;

    // Every start, not just a migration's. `migrate_box` used to be the only caller, and its call
    // sits *after* `start_box` — so a migration that failed here left the conversation under the old
    // sandbox's slug with nothing that would ever move it, and the retry (`skein start`, because the
    // placement already exists and `migrate` now refuses the box) started the agent on an empty
    // transcript. Measured on lattice-feat-design-codex-claude: the work restored, the conversation
    // did not. It is idempotent — a box that already has a conversation at its own slug keeps it —
    // so the honest place for it is wherever a box comes up, not on one path through that.
    // After provisioning, because it writes into the box's private HOME, and on every start because
    // a repo registered since the box was built adds a host it has never trusted.
    ensure_box_known_hosts(name);

    // Same reasoning, same moment: a private HOME starts with no committer, and the box finds out
    // when it tries to commit rather than when it was built.
    let (who, email) = box_identity(name, repo);
    let script = identity_script(&who, &email);
    if !script.is_empty() {
        if let Err(e) = boxed.exec(&script, Duration::from_secs(30)) {
            eprintln!("skein: could not set {name}'s git identity ({e}); its first commit will ask who you are");
        }
    }

    match realign_transcript(name) {
        Ok(0) => {}
        Ok(n) => eprintln!("skein: pointed {n} transcript file(s) at {name}'s working directory"),
        Err(e) => {
            eprintln!("skein: {name} came up, but its conversation could not be located ({e})")
        }
    }

    // A box that started without a ceiling started *successfully*, so nothing else would ever say
    // so — and it is the one condition under which one box's runaway build can kill the others.
    if let Some(why) = uncapped_reason(name) {
        eprintln!(
            "skein: {name} is running WITHOUT a memory ceiling ({why}); a runaway build in it can \
             take down every other box in the fleet"
        );
    }
    Ok(())
}

/// Everything in a box that is not already on a remote, written into the repo's host-mounted store.
///
/// The fleet sandbox's memory and CPUs are fixed when it is created, so changing them means
/// destroying it — and every box's checkout is VM-local, which is exactly what makes builds fast and
/// makes this necessary. Committed-but-unpushed work, staged changes, unstaged changes and untracked
/// files each need their own artifact: a bundle preserves history a patch cannot, and `git diff`
/// covers neither untracked files nor the index/worktree distinction.
///
/// Returns the path *relative to the store*, which is the form the launch spec carries and the
/// provisioning script validates — it refuses anything not under `skein/handoff-snapshots/`.
pub fn snapshot_box(name: &str, store: &str, run: &str) -> Result<String, String> {
    let relative = format!("skein/handoff-snapshots/{name}/{run}");
    let snapshot = format!("{store}/{relative}");
    // Addressed from the SANDBOX, not through the box's namespace.
    //
    // A snapshot exists to rescue work, so requiring the box's *session* to be alive to take one is
    // backwards — and it fails exactly when it is needed most: a fleet box loses its tmux server
    // whenever the sandbox cycles, and the first thing resize did was refuse with `nsenter: cannot
    // open /proc/<pid>/ns/user`, leaving the work it was trying to save unreachable.
    //
    // Nothing here needs the namespace anyway. A box's tree and its private HOME are ordinary
    // directories in the sandbox (`/boxes/<name>/{tree,home}`), so naming them directly reads the
    // same bytes without entering anything. A legacy box keeps the old path: its sandbox IS the box.
    let placed = shared_record(name);
    let (boxed, enter, home) = match &placed {
        Some(record) => (
            own_sandbox(&record.sandbox),
            format!("cd {}; ", sh_quote(&format!("{}/tree", box_root(name)))),
            format!("{}/home", box_root(name)),
        ),
        None => (
            place_of(name).ok_or_else(|| format!("box {name} is not placed"))?,
            String::new(),
            "$HOME".to_string(),
        ),
    };
    let build = format!(
        "{enter}{}",
        snapshot_script(&snapshot, name, placed.is_none(), &home)
    );
    boxed.exec(&build, Duration::from_secs(600))?;

    // What the sweep refused to carry, said out loud. A snapshot that quietly leaves things behind
    // is worse than one that carries less: the box comes back looking complete.
    let skipped = std::fs::read_to_string(format!("{snapshot}/{SKIPPED_FILE}")).unwrap_or_default();
    let lines: Vec<&str> = skipped.lines().filter(|l| !l.trim().is_empty()).collect();
    if !lines.is_empty() {
        eprintln!(
            "skein: {name}'s snapshot leaves {} ignored path(s) behind — {}{}. \
             They are build output or dependencies by size; rebuild them in the box.",
            lines.len(),
            lines.iter().take(3).copied().collect::<Vec<_>>().join(", "),
            if lines.len() > 3 { ", …" } else { "" }
        );
    }
    Ok(relative)
}

/// Where the snapshot records the ignored paths it decided not to carry.
const SKIPPED_FILE: &str = "skipped-ignored.txt";

/// Fetch the ignored files a box left behind in the sandbox it was migrated out of.
///
/// For boxes migrated before the snapshot swept ignored files at all: `.env`, local dev config and
/// the box's own `.skein/journal.md` stayed in the old VM, and the box has been running without them
/// ever since. This exists because the old sandbox is *stopped rather than destroyed* — that
/// decision was made so a migration could be undone, and it turns out to be what makes this
/// recoverable too.
///
/// Deliberately additive: it refuses to overwrite anything the new box already has. The box has been
/// working since it moved, and a file it wrote itself is newer and more correct than the copy in the
/// VM it left. Recovering work must never be a way to lose some.
///
/// Leaves the old sandbox stopped again, whatever happens — it holds a full memory reservation while
/// it runs, which is the thing the migration existed to reclaim.
pub fn recover_ignored(name: &str) -> Result<String, String> {
    if !valid_name(name) {
        return Err(format!("invalid box name {name:?}"));
    }
    let record = shared_record(name).ok_or_else(|| {
        format!("{name} is not in the fleet, so it has no old sandbox to recover from")
    })?;
    let repo =
        repo_for_box(name).ok_or_else(|| format!("box {name} belongs to no registered repo"))?;

    // The old sandbox, addressed as a sandbox — `place_of` would hand back the box's fleet placement,
    // which is exactly the copy that is missing the files.
    //
    // No explicit start, because sbx has no such verb: `stop` halts a sandbox and exec'ing into one
    // wakes it again. Asking for `sbx start` failed with `unknown command: "start"` — advice skein
    // had also been printing after every migration, in a message about how to undo one.
    let old = own_sandbox(name);

    let carried = (|| -> Result<String, String> {
        let staging = format!("{}/skein/handoff-snapshots/{name}", repo.store);
        let archive = format!("{staging}/ignored-rescue.tgz");
        let script = format!(
            "set -e; cd \"$(git rev-parse --show-toplevel 2>/dev/null || pwd)\"; mkdir -p {stage}; \
             {sweep} {pack} \
             tar -tzf {archive} | wc -l",
            stage = sh_quote(&staging),
            archive = sh_quote(&archive),
            sweep = ignored_sweep(&staging, "list", &format!("{staging}/{SKIPPED_FILE}")),
            pack = pack_carried(
                &staging,
                "list",
                "ignored-rescue.tgz",
                &format!("{staging}/{SKIPPED_FILE}")
            ),
        );
        let count = old
            .exec(&script, Duration::from_secs(600))
            .map_err(|e| {
                format!(
                    "could not read {name}'s old sandbox ({e}) — if it has already been removed, \
                     its ignored files are gone and there is nothing left to recover"
                )
            })?
            .trim()
            .to_string();
        // `-k` is the whole safety property: extract only what is not already there.
        //
        // Except a broken symlink, which is not a file the box is maintaining — it is the wreckage
        // of an earlier move. `/run/sandbox/source` is a `--clone` sandbox's bind of the host repo,
        // and a `.env` symlinked into it arrives in the fleet pointing at a mount that is not there.
        // Left in place it would also *win* against `-k` and block the very content that fixes it.
        // Only links into that mount, and only ones that already resolve to nothing: narrow enough
        // that nothing else can be caught by it.
        let restore = format!(
            "cd {tree}; \
             find . -xtype l -lname '/run/sandbox/*' -print -delete 2>/dev/null || true; \
             tar -xzkf {archive} 2>/dev/null || true; rm -f {archive}",
            tree = sh_quote(&format!("{}/tree", box_root(name))),
            archive = sh_quote(&archive),
        );
        own_sandbox(&record.sandbox).exec(&restore, Duration::from_secs(300))?;
        Ok(count)
    })();

    // Stopped again either way. A rescue that leaves a second VM running has undone the migration.
    if let Err(e) = run_capture_for("sbx", &["stop", name], Duration::from_secs(300)) {
        eprintln!("skein: {name}'s old sandbox could not be stopped again ({e}) — `sbx stop {name}` frees its reservation");
    }
    carried
}

/// List the ignored paths worth carrying, into `<dir>/<list>` — shared by the snapshot and the
/// rescue so the two can never disagree about what counts as work.
fn ignored_sweep(dir: &str, list: &str, skipped: &str) -> String {
    const FILE_KB: u64 = 10 * 1024;
    const DIR_KB: u64 = 20 * 1024;
    const DIR_FILES: u64 = 2000;
    let d = sh_quote(dir);
    format!(
        "git ls-files --others --ignored --exclude-standard --directory -z \
           -- . ':(exclude).claude' ':(exclude).claude/**' > {d}/ignored.list; \
         : > {skipped_q}; \
         while IFS= read -r -d '' p; do \
           case \"$p\" in \
             */) \
               n=$(find \"$p\" -type f 2>/dev/null | head -n {over} | wc -l); \
               if [ \"$n\" -ge {over} ]; then \
                 printf '%s (over {DIR_FILES} files)\\n' \"$p\" >> {skipped_q}; continue; \
               fi; \
               kb=$(du -sk \"$p\" 2>/dev/null | cut -f1); \
               case \"$kb\" in ''|*[!0-9]*) kb=0 ;; esac; \
               if [ \"$kb\" -gt {DIR_KB} ]; then \
                 printf '%s (%s MB)\\n' \"$p\" \"$((kb/1024))\" >> {skipped_q}; continue; \
               fi ;; \
             *) \
               kb=$(( $(wc -c < \"$p\" 2>/dev/null || echo 0) / 1024 )); \
               if [ \"$kb\" -gt {FILE_KB} ]; then \
                 printf '%s (%s MB)\\n' \"$p\" \"$((kb/1024))\" >> {skipped_q}; continue; \
               fi ;; \
           esac; \
           printf '%s\\0' \"$p\" >> {d}/{list}; \
         done < {d}/ignored.list; \
         rm -f {d}/ignored.list; ",
        over = DIR_FILES + 1,
        skipped_q = sh_quote(skipped),
    )
}

/// Everything a box's work is, written into the store: its commits, its index, its worktree, the
/// files git is not tracking, and the agent's own state.
///
/// **Ignored files are work too.** The sweep used to be `--others --exclude-standard`, which lists
/// untracked files and deliberately omits ignored ones — so `.env`, `.envrc`, local dev config and
/// the box's own `.skein/journal.md` were silently left behind on every migration and every resize.
/// The box came back looking complete and failed at runtime, or came back having forgotten what it
/// had been doing, which is worse than an error because nothing announces it.
///
/// The reason it cannot simply carry everything ignored is `node_modules/` and `target/`: this runs
/// for every box, into a host directory, and a resize does the whole fleet at once. So the rule is
/// **size, not names** — a hand-written list of build directories is a list that is wrong for the
/// next language. An ignored *file* is carried unless it is very large; an ignored *directory* is
/// carried when it is small enough to be config rather than artefacts. `.skein/` and `.env` pass;
/// a dependency tree does not. The file-count probe short-circuits at its threshold, so a directory
/// with 200k files costs one bounded `find` rather than a walk of the whole thing.
///
/// Whatever is refused is written to [`SKIPPED_FILE`] and reported by the caller.
///
/// **The bundle carries what the remote does not have, not the whole history.** `--all` on its own
/// wrote every object the repository has ever held into the store, over a virtiofs mount that is
/// several times slower to write than local disk: one box with a 1.8 GB `.git` took a migration past
/// its ten-minute budget doing nothing but copying history that already exists on the remote. A
/// resize does that for every box at once.
///
/// `--not --remotes` leaves a bundle of exactly the commits that would otherwise be lost, with the
/// rest recorded as prerequisites. That is safe *because of how the box is rebuilt*: the replacement
/// is cloned from the same source this box was, so every prerequisite is already in it before the
/// bundle is opened. Unpushed local commits are by definition not reachable from a remote ref, so
/// they are all still in there — which is the entire job.
///
/// The trimmed bundle is then **checked for the box's own branch**, and that check is the whole
/// safety of this. `--not --remotes` drops any ref whose tip the remote already has, so a box whose
/// checked-out branch is fully pushed gets a bundle without it — and if some *other* local ref is
/// unpushed the bundle is still non-empty, so it looks perfectly healthy. Measured: a 127 KB bundle
/// with no `refs/heads/<branch>` and no HEAD, and a restore that died on `couldn't find remote ref
/// HEAD` after the old sandbox had already been stopped.
///
/// When the branch is missing — or the bundle would be empty, which git refuses to write — the
/// fallback carries the tip commit alone, its parent recorded as a prerequisite. That is a ref the
/// restore can find, and still nothing like the full history. `--all` remains the last resort, for a
/// repository too young to have a parent commit.
fn snapshot_script(snapshot: &str, name: &str, transcript_is_vm_local: bool, home: &str) -> String {
    let s = sh_quote(snapshot);
    let skipped = format!("{snapshot}/{SKIPPED_FILE}");
    let sweep_ignored = ignored_sweep(snapshot, "untracked.list", &skipped);
    format!(
        "set -e; mkdir -p {s}; \
         b=\"$(git rev-parse --abbrev-ref HEAD)\"; \
         if ! {{ git bundle create {s}/repo.bundle --all --not --remotes 2>/dev/null \
                && git bundle list-heads {s}/repo.bundle 2>/dev/null \
                   | awk -v r=\"refs/heads/$b\" '$2==r{{f=1}} END{{exit !f}}'; }}; then \
           git bundle create {s}/repo.bundle 'HEAD~1..HEAD' 2>/dev/null \
             || git bundle create {s}/repo.bundle --all; \
         fi; \
         git diff --cached --binary HEAD > {s}/index.patch; \
         git diff --binary > {s}/worktree.patch; \
         git ls-files --others --exclude-standard -z -- . ':(exclude).claude' ':(exclude).claude/**' > {s}/untracked.list; \
         {sweep_ignored}\
         {pack}\
         printf '{{\"box\":\"%s\",\"branch\":\"%s\",\"head\":\"%s\"}}\\n' {n} \
           \"$(git rev-parse --abbrev-ref HEAD)\" \"$(git rev-parse HEAD)\" > {s}/manifest.json; \
         {agent_state}",
        n = sh_quote(name),
        pack = pack_carried(snapshot, "untracked.list", "untracked.tgz", &skipped),
        // A box already in the fleet host-binds its transcript; one being migrated in does not.
        agent_state = agent_state_tar(snapshot, transcript_is_vm_local, home),
    )
}

/// Tar the swept paths, resolving the symlinks that would not survive the move.
///
/// A `--clone` sandbox bind-mounts the host repository at `/run/sandbox/source`, and a box's `.env`
/// is often a symlink into it — that is how the box reads the host's environment file without a
/// copy. tar preserves a symlink *as a symlink*, so the rescue faithfully carried a pointer to a
/// mount that does not exist in the fleet, and the box got a dangling link where its config should
/// be. It looked like the file was there, which is the worst of both outcomes.
///
/// So a symlink is carried as a symlink only while it still points inside the tree, where it will
/// mean the same thing after the move. One that points outside is carried as its *content*, since
/// the thing worth keeping is what it resolves to. One that already resolves to nothing is reported
/// rather than carried — there is nothing behind it to take.
///
/// Two passes into one archive rather than two archives: `-r` appends, `-h` dereferences, and each
/// path appears exactly once, so an extraction that refuses to overwrite still lands the right thing.
fn pack_carried(dir: &str, list: &str, archive: &str, skipped: &str) -> String {
    let d = sh_quote(dir);
    format!(
        "root=\"$(git rev-parse --show-toplevel 2>/dev/null || pwd)\"; \
         : > {d}/carry.list; : > {d}/deref.list; \
         while IFS= read -r -d '' p; do \
           if [ -L \"$p\" ]; then \
             if [ ! -e \"$p\" ]; then \
               printf '%s (dangling symlink -> %s)\\n' \"$p\" \"$(readlink \"$p\")\" >> {skipped_q}; \
               continue; \
             fi; \
             case \"$(readlink -f \"$p\" 2>/dev/null)\" in \
               \"$root\"/*) ;; \
               *) printf '%s\\0' \"$p\" >> {d}/deref.list; continue ;; \
             esac; \
           fi; \
           printf '%s\\0' \"$p\" >> {d}/carry.list; \
         done < {d}/{list}; \
         if [ -s {d}/carry.list ]; then tar --null -T {d}/carry.list -cf {d}/carry.tar; \
         else tar -cf {d}/carry.tar --files-from /dev/null; fi; \
         if [ -s {d}/deref.list ]; then tar --null -T {d}/deref.list -rhf {d}/carry.tar; fi; \
         gzip -c {d}/carry.tar > {archive_q}; \
         rm -f {d}/carry.tar {d}/carry.list {d}/deref.list {d}/{list}; ",
        skipped_q = sh_quote(skipped),
        archive_q = sh_quote(&format!("{dir}/{archive}")),
    )
}

/// The parts of a box's private `$HOME` that a rebuilt box needs and cannot get any other way.
///
/// An **allowlist**, and that is the whole design. The same argument that made `box-session.sh`
/// private-by-default applies in reverse here: an agent harness keeps state wherever it likes, and
/// with an exclude list anything unanticipated — a new token cache, a new auth file — would be
/// copied into the repo's store, which is host-side shared data. Named paths mean a harness change
/// costs a lost todo list, never a leaked credential.
///
/// `.credentials.json` is therefore not here, and does not need to be: `box-session.sh` seeds the
/// box's `~/.claude` from the sandbox's on first start, so the rebuilt box is already logged in.
///
/// Whether the **transcript** is carried depends on where the box keeps it, which is the one thing
/// that differs between the two callers:
///
/// * A **fleet box** host-binds `~/.claude/projects`, so the conversation is already durable and
///   already exactly where the rebuilt box will look. Tarring it would copy a virtiofs directory out
///   to the store and straight back — twice over the slow path — to arrive at the file that never
///   left.
/// * A **box being migrated in** from its own sandbox has it on VM-local disk, and the VM is about
///   to stop. Leaving it out there is not an optimisation, it is losing the conversation; this is
///   precisely the case where the box has years of context and no host copy of any of it.
fn agent_state_tar(snapshot: &str, transcript_is_vm_local: bool, home: &str) -> String {
    let mut carried: Vec<&str> = vec![
        ".claude/history.jsonl", // the prompt history
        ".claude/todos",         // in-flight task list
        ".claude.json",          // per-box MCP registration + project state
        ".codex/history.jsonl",
    ];
    if transcript_is_vm_local {
        carried.push(".claude/projects"); // the record --continue reads
        carried.push(".codex/sessions");
    }
    let list = carried
        .iter()
        .map(|p| sh_quote(p))
        .collect::<Vec<_>>()
        .join(" ");
    // Only the paths that exist: tar fails the whole archive on a missing member, and which of these
    // a box has depends on which runtime it ran.
    // `home` is the box's private HOME as seen from wherever this runs: an absolute path in the
    // sandbox for a fleet box, and literally `$HOME` for a legacy one entered through its own place.
    format!(
        "have=''; for p in {list}; do [ -e \"{h}/$p\" ] && have=\"$have $p\"; done; \
         if [ -n \"$have\" ]; then tar -C \"{h}\" -czf {s}/agent-state.tgz $have; \
         else tar -czf {s}/agent-state.tgz --files-from /dev/null; fi",
        h = home,
        s = sh_quote(snapshot),
    )
}

/// Move a box that has its own sandbox into the shared one, keeping its work and its conversation.
///
/// The reason to want this is the reason the fleet exists: every per-VM box holds a memory
/// *reservation* whether or not it is doing anything, and those are what the shared sandbox stops
/// summing. A fleet nobody can move their existing boxes into only helps the boxes they have not
/// created yet.
///
/// The old sandbox is **stopped, never destroyed**. Its checkout, its history and its snapshot all
/// still exist, so a migration that goes wrong costs one `sbx exec` to wake rather than a day's work — the
/// same rule the cross-runtime takeover follows, and worth more here because this path cannot be
/// rehearsed against a fake. Removing it is left to the user, once they are satisfied.
///
/// One asymmetry with a resize, and it is the whole reason this is a separate function: a per-VM box
/// keeps its transcript on VM-local disk, so the snapshot has to carry it (see [`agent_state_tar`]).
/// For a box already in the fleet that would be redundant; for this one, skipping it loses the
/// conversation — which for a long-lived box is most of its value.
pub fn migrate_box(name: &str) -> Result<String, String> {
    if !valid_name(name) {
        return Err(format!("invalid box name {name:?}"));
    }
    let sandbox = fleet_sandbox();
    if sandbox.is_empty() {
        return Err("no fleet sandbox configured; set one before migrating into it".into());
    }
    if shared_record(name).is_some() {
        return Err(format!("{name} is already in the fleet"));
    }
    let repo = repo_for_box(name).ok_or_else(|| {
        format!("box {name} belongs to no registered repo, so it cannot be moved")
    })?;

    // Its own branch, asked of the box itself rather than of the registry: it may have moved (a
    // branch-per-slice box does), and restoring it onto the branch skein last recorded would quietly
    // put the agent's work somewhere it does not expect to find it.
    let branch = place_of(name)
        .ok_or_else(|| format!("box {name} is not reachable"))?
        .exec("git rev-parse --abbrev-ref HEAD", Duration::from_secs(30))?
        .trim()
        .to_string();
    if branch.is_empty() || branch == "HEAD" {
        return Err(format!(
            "{name} has no attached branch to restore onto — check it out in the box first"
        ));
    }

    ensure_fleet(&sandbox, &fleet_mounts())?;
    let run = format!("migrate-{}", Utc::now().format("%Y%m%dT%H%M%SZ"));
    let dir = snapshot_box(name, &repo.store, &run).map_err(|e| {
        format!("could not save {name}'s work ({e}) — nothing was changed, the box is untouched")
    })?;
    // The spec is written into the repo's store, which the OLD box reads too — and it tells whoever
    // reads it to restore this snapshot. That is right for the fleet box being built and wrong for
    // the box the snapshot came from, whose tree already holds every byte of it. It matters on the
    // failure path, which is the only path that starts the old box again: a migration that stopped
    // the sandbox and then could not clone left waking the old sandbox as the way back, and the restore would
    // have met patches already applied and failed the box's startup outright.
    //
    // So the source is marked as already-restored before it is stopped, in the file the kit checks.
    // Best-effort: the mark prevents a bad recovery, and failing to write it must not fail a
    // migration that has otherwise succeeded.
    if let Some(place) = place_of(name) {
        // Asked of git rather than built from a recorded path, so it lands in the right place for a
        // box in either shape — every script skein sends already starts at the box's repo root.
        let _ = place.exec(
            "root=\"$(git rev-parse --show-toplevel 2>/dev/null || pwd)\"; \
             touch \"$root/.git/skein-handoff-restored\" 2>/dev/null || true",
            Duration::from_secs(30),
        );
    }
    write_restore_launch_spec(&BoxSnapshot {
        name: name.to_string(),
        repo: repo.clone(),
        branch: branch.clone(),
        agent: agent_for_box(name),
        dir: dir.clone(),
    })?;

    // Stop the old sandbox before starting the new box, not after: the reservation is the entire
    // point, and for a moment otherwise the fleet box and the VM it replaces would both hold one.
    // `sbx stop` rather than `rm` — see above.
    let (out, err, code) = run_capture("sbx", &["stop", name])?;
    if code != 0 {
        let detail = if err.trim().is_empty() { out } else { err };
        return Err(format!(
            "could not stop the old sandbox for {name}: {} — its work is saved under {dir} in the \
             repo store, so nothing is lost; resolve this and retry",
            detail.trim()
        ));
    }

    start_box(name, &repo, &branch, "exec bash -l").map_err(|e| {
        format!(
            "{name} was snapshotted and its old sandbox stopped, but the fleet box did not start \
             ({e}). Nothing is lost: `sbx exec -it {name} bash -l` wakes the original exactly as it was, \
             and the snapshot remains at {dir} in the repo store."
        )
    })?;
    // The checkout moved, so the conversation has to be told where it lives now — otherwise the box
    // starts up with a transcript on disk that its agent will never look at.
    match realign_transcript(name) {
        Ok(0) => {}
        Ok(n) => {
            eprintln!("skein: carried {n} transcript file(s) onto {name}'s new working directory")
        }
        Err(e) => eprintln!(
            "skein: {name} migrated, but its conversation could not be pointed at the new checkout \
             ({e}); the files are in {}/claude-projects and `claude --continue` will start fresh",
            box_state(name)
        ),
    }
    remint_tracker_token(name);
    Ok(dir)
}

/// Re-issue a migrated box's work-tracker credential, if it had one.
///
/// The snapshot carries `~/.config/sync/env` across with the rest of the box's home, and a bearer
/// token is the one thing here that a faithful copy does not preserve: it is bound to a box's
/// lifetime at the gateway, not to the bytes on disk. Destroying a box revokes it (`destroy_box`),
/// so a box that is destroyed and migrated again — the ordinary way to retry a migration — comes
/// back holding a token the gateway has already retired.
///
/// It fails in the worst available shape. The MCP server registers fine, the agent is *told* by its
/// own CLAUDE.md to claim work before starting, and the 401 arrives mid-turn on the first `capture`
/// — while `sync-install.sh`'s once-per-box stamp guarantees no restart will ever re-register it.
/// Measured on the first migrated box, which sat with a dead `sync` server through three `/mcp`
/// attempts before anyone worked out why.
///
/// Minting unconditionally rather than probing first: a fresh mint of the same agent name keeps the
/// project binding and invalidates only its own predecessor, which belonged to this same box. One
/// round trip, and no "is it still good?" answer to get wrong.
///
/// **Only for a box that already had one.** This never wires up a box the user never wired up — a
/// migration is not consent to mint a credential, and provisioning stays an explicit act everywhere
/// else (see the note above `guest_write`). And it is fail-soft: the migration itself has already
/// succeeded by the time this runs, and a tracker that cannot be reached must not turn a moved box
/// into a failed one.
fn remint_tracker_token(name: &str) {
    let carried = place_of(name).and_then(|p| {
        p.exec(
            "[ -s \"$HOME/.config/sync/env\" ] && echo yes",
            Duration::from_secs(30),
        )
        .ok()
    });
    if !carried.unwrap_or_default().contains("yes") {
        return;
    }
    match crate::sync_provision_box(name) {
        Ok(note) => eprintln!("skein: re-issued {name}'s tracker token — {note}"),
        Err(e) => eprintln!(
            "skein: {name} moved, but its work-tracker token could not be re-issued ({e}). The one \
             it carried over was revoked when its old box went, so `sync` will fail to connect \
             until you provision it again from the cockpit."
        ),
    }
}

/// What one box needs in order to be rebuilt after the sandbox is destroyed.
#[derive(Debug, Clone)]
pub struct BoxSnapshot {
    pub name: String,
    pub repo: Repo,
    pub branch: String,
    pub agent: String,
    /// The snapshot path *relative to the repo store* — the form the launch spec carries.
    pub dir: String,
}

/// Change the fleet sandbox's memory or CPUs, carrying every box's work across.
///
/// sbx fixes both at creation, so this destroys the sandbox and rebuilds it. Every box's checkout is
/// VM-local — the thing that makes builds fast — so all of it has to come out first and go back
/// after. The sequence is:
///
///   snapshot every box → record each snapshot in its launch spec → destroy → recreate → restart
///
/// **Nothing is destroyed until every box has been snapshotted.** A partial snapshot is not a
/// partial resize, it is lost work, and the boxes that would lose it are exactly the ones whose
/// state could not be read — so a single failure aborts with the sandbox still standing and every
/// box still in it. That ordering is the entire safety property of this function.
///
/// Restarting is best-effort *by design*: once the snapshots are written they are durable, on the
/// host, in each repo's store. A box that fails to come back can be retried with `skein start`, and
/// the provisioning script restores it from the launch spec it already carries. Failing the whole
/// resize because the fourth box's clone timed out would help nobody.
pub fn resize_fleet(memory: &str, cpus: &str, disk: &str) -> Result<Vec<String>, String> {
    let sandbox = fleet_sandbox();
    if sandbox.is_empty() {
        return Err("no fleet sandbox configured; nothing to resize".into());
    }
    let boxes = placed_boxes(&sandbox);

    // ---- phase 1: get everything out, or change nothing ----
    // The login first, because it lives in the sandbox's HOME and the rebuild destroys it.
    // `ensure_fleet` restores it afterwards — but only if something captured it BEFORE the destroy,
    // and its own call runs after `sbx create`, when the sandbox is empty and there is nothing left
    // to save. Measured the hard way: a login made between two resizes was gone after the second.
    sync_fleet_login(&sandbox);
    let mut snapshots: Vec<BoxSnapshot> = Vec::new();
    let run = format!("resize-{}", Utc::now().format("%Y%m%dT%H%M%SZ"));
    for (name, _) in &boxes {
        let repo = repo_for_box(name).ok_or_else(|| {
            format!(
                "box {name} belongs to no registered repo, so its work has nowhere to be saved — \
                 resize aborted with the sandbox untouched"
            )
        })?;
        let branch = branch_of(name).unwrap_or_default();
        if branch.trim().is_empty() {
            return Err(format!(
                "box {name} has no recorded branch to restore onto — resize aborted with the \
                 sandbox untouched"
            ));
        }
        let dir = snapshot_box(name, &repo.store, &run).map_err(|e| {
            format!(
                "could not save {name}'s work ({e}) — resize aborted with the sandbox untouched"
            )
        })?;
        snapshots.push(BoxSnapshot {
            name: name.clone(),
            agent: agent_for_box(name),
            repo: repo.clone(),
            branch,
            dir,
        });
    }
    // Only now, with every box's work on the host, is the launch spec rewritten to restore from it.
    for snap in &snapshots {
        write_restore_launch_spec(snap)?;
    }

    // ---- phase 2: the destructive part ----
    let config = load_config();
    save_config(&Config {
        fleet_memory: memory.trim().to_string(),
        fleet_cpus: cpus.trim().to_string(),
        // Empty keeps the configured disk rather than resetting it to sbx's 20 GB: `skein resize
        // 32g` is a memory change, and it must not silently shrink the disk back on the way past.
        fleet_disk: match disk.trim() {
            "" => config.fleet_disk.clone(),
            d => d.to_string(),
        },
        ..config
    })?;
    // Not the 30s action budget: tearing a microVM down is slower than a status query, and a
    // timeout here is reported as a failed destroy while the destroy carries on regardless.
    let (out, err, code) =
        run_capture_for("sbx", &["rm", "-f", &sandbox], Duration::from_secs(300))?;
    if code != 0 {
        let detail = if err.trim().is_empty() { out } else { err };
        return Err(format!(
            "could not destroy {sandbox}: {} — every box's work is saved in its repo store under \
             {run}, and `skein start <box>` restores it once the sandbox is rebuilt",
            detail.trim()
        ));
    }
    // The namespaces died with the sandbox. Forget them before rebuilding, or `place_of` would hand
    // out pids into a VM that no longer exists.
    for (name, _) in &boxes {
        forget_place(name);
    }
    // The one moment when there is no sandbox at all. If the rebuild fails here — most likely a
    // confirmation `sbx create` asked for and nobody could answer — say where the work is, because
    // the boxes are gone and their checkouts went with the VM.
    let again = format!(
        "skein resize {}{}",
        memory.trim(),
        match cpus.trim() {
            "" => String::new(),
            cpus => format!(" {cpus}"),
        }
    );
    ensure_fleet(&sandbox, &fleet_mounts()).map_err(|e| {
        format!(
            "{sandbox} is not usable yet: {e}\n\
             every box's work is saved in its repo store under {run}, and nothing is lost. \
             `{again}` is safe to re-run — creating the sandbox is idempotent, so it retries only \
             the step that failed, and each box restores on its next `skein start`"
        )
    })?;

    // ---- phase 3: bring them back ----
    let mut failed = Vec::new();
    for snap in &snapshots {
        if let Err(e) = start_box(&snap.name, &snap.repo, &snap.branch, "exec bash -l") {
            eprintln!("skein: {} did not come back: {e}", snap.name);
            failed.push(snap.name.clone());
        }
    }
    Ok(failed)
}

/// Point a box's launch spec at the snapshot it must restore from on its next start.
///
/// The same `handoff.dir` channel a cross-runtime takeover uses, because it is the same problem:
/// a new checkout that has to become an old box. The provisioning script validates the path against
/// the `skein/handoff-snapshots/` prefix and restores once, guarded by a marker in `.git`.
fn write_restore_launch_spec(snap: &BoxSnapshot) -> Result<(), String> {
    let dir = std::path::Path::new(&snap.repo.store)
        .join("skein")
        .join("launch");
    std::fs::create_dir_all(&dir).map_err(|e| format!("mkdir {}: {e}", dir.display()))?;
    let body = serde_json::json!({
        "branch": snap.branch,
        "agent": snap.agent,
        "handoff": { "source": snap.name, "dir": snap.dir },
    });
    let bytes = serde_json::to_vec_pretty(&body).map_err(|e| e.to_string())?;
    write_atomic(&dir.join(format!("{}.json", snap.name)), &dir, &bytes)
}

/// What a box clones from. The registered source when it is a URL — a box should start from the
/// same base the diff is taken against, not from whatever is stale or half-committed in the host's
/// clone. For a repo adopted in place there is no URL, so the host clone is it; that is mounted
/// (see [`fleet_mounts`]) at the same path, and git is content to clone a local directory.
///
/// That fallback carries a real consequence, stated here because nothing else would say it: for an
/// adopted repo the host clone's freshness *is* what every new box starts from. Nobody pulling it
/// means every new box quietly starts behind, with a green checkout and no signal at all. That is
/// what [`crate::repos::pull_repo`] is for, and why it did not become redundant when boxes started
/// cloning from remotes.
pub(crate) fn clone_source(repo: &Repo) -> String {
    if is_git_url(&repo.source) {
        repo.source.clone()
    } else {
        repo.work.clone()
    }
}

/// The branch a box's own branch is cut from.
///
/// Asked of the REMOTE skein is about to clone from, because that is the only copy whose answer is
/// necessarily true. `refs/remotes/origin/HEAD` looks like the remote's answer and is not: it is a
/// local cache written when the host clone was made, and it goes stale when the default is renamed
/// on the far side. A box migration failed on exactly that — the cached ref said `main`, the remote
/// had only `master`, and `git clone --branch main` refused, leaving the box's old sandbox stopped
/// with no fleet box to replace it.
///
/// So: `ls-remote --symref HEAD` first, one round trip against the same source the clone will use.
/// The cached ref and the host clone's own branch remain the offline fallbacks, and `main` only when
/// there is nothing left to ask.
pub fn base_branch(repo: &Repo) -> String {
    let git = |args: &[&str]| -> Option<String> {
        let mut argv = vec!["-C", repo.work.as_str()];
        argv.extend_from_slice(args);
        let (out, _, code) = run_capture("git", &argv).ok()?;
        let out = out.trim().to_string();
        (code == 0 && !out.is_empty()).then_some(out)
    };

    // What this repo's base might be called, most specific first. The configured one is the user's
    // own answer and leads; the two local reads are caches of the remote and follow it.
    let mut wanted: Vec<String> = Vec::new();
    let configured = load_config().base_branch.trim().to_string();
    if !configured.is_empty() {
        wanted.push(configured);
    }
    for candidate in [
        git(&["symbolic-ref", "--short", "refs/remotes/origin/HEAD"])
            .and_then(|r| r.rsplit_once('/').map(|(_, b)| b.to_string())),
        git(&["rev-parse", "--abbrev-ref", "HEAD"]).filter(|b| b != "HEAD"),
    ]
    .into_iter()
    .flatten()
    {
        if !wanted.contains(&candidate) {
            wanted.push(candidate);
        }
    }

    // One round trip settles all of them: `HEAD` comes back as `ref: refs/heads/<default>` — the
    // remote's own name for its default, whatever it is — and each candidate comes back only if the
    // remote really has it. Naming the refs explicitly keeps the reply small on a repo with
    // thousands of branches. A local-path source answers this too, from its own refs.
    let source = clone_source(repo);
    let mut argv: Vec<String> = ["ls-remote", "--symref", &source, "HEAD"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    argv.extend(wanted.iter().map(|b| format!("refs/heads/{b}")));
    let listing = git(&argv.iter().map(String::as_str).collect::<Vec<_>>());

    if let Some(listing) = listing {
        // The first candidate the remote actually has wins — that is how a configured base of
        // `develop` is honoured on the repos that have one without breaking the repos that do not.
        if let Some(found) = wanted.iter().find(|b| {
            listing
                .lines()
                .any(|line| line.split_whitespace().nth(1) == Some(&format!("refs/heads/{b}")))
        }) {
            return found.clone();
        }
        // None of them exist there — so take the remote's own default, which always does.
        if let Some(default) = listing
            .lines()
            .find_map(|line| line.strip_prefix("ref:"))
            .and_then(|rest| rest.split_whitespace().next())
            .and_then(|r| r.rsplit_once('/').map(|(_, b)| b.to_string()))
        {
            return default;
        }
    }

    // Offline, or a source that cannot be reached: fall back to the local guesses in the same order
    // and let the clone's own fallback cover a wrong one. Empty rather than `main` when there is
    // nothing to go on at all — a guess that names a branch fails the clone outright, while naming
    // none asks git for the remote's default and cannot be wrong.
    wanted.into_iter().next().unwrap_or_default()
}

/// Read back the anchor pid `box-session.sh` recorded, so the host can write the box's placement.
///
/// The pid is knowable only inside the sandbox, and only after the session starts — which is why
/// placement is recorded after launch rather than predicted before it.
/// Where the fleet's logins are kept on the HOST, so they outlive the sandbox.
///
/// Not the shared project store — that is data the boxes read, and a credential has no business in
/// it. This is skein's own directory, beside the box state it already keeps there.
fn fleet_home_dir() -> std::path::PathBuf {
    skein_home().join("fleet-home")
}

/// The files that make a login a login, relative to a HOME.
const LOGIN_FILES: [&str; 2] = [".claude/.credentials.json", ".codex/auth.json"];

/// Keep the fleet's login on the host, and put it back into a sandbox that has none.
///
/// `skein login` writes into the sandbox's own HOME, which is VM-local — so a resize destroyed it
/// along with everything else, and "log in once" quietly became "log in after every resize".
/// Measured: after a rebuild the sandbox came back with `cred=GONE`.
///
/// Newest wins, in one direction at a time: a sandbox that has the credential is the live copy and
/// refreshes the host's; a sandbox without one is freshly built and gets the host's back. Boxes
/// already reconcile into the sandbox at session start, so a re-login anywhere reaches here too.
///
/// Best-effort by design: a fleet running on API keys has no login to carry, and failing a launch
/// over that would be absurd.
pub fn sync_fleet_login(sandbox: &str) {
    let fleet = own_sandbox(sandbox);
    let dir = fleet_home_dir();
    for rel in LOGIN_FILES {
        let host = dir.join(rel);
        let in_sandbox = fleet
            .bytes(
                &format!("cat \"$HOME\"/{} 2>/dev/null || true", sh_quote(rel)),
                Duration::from_secs(20),
            )
            .unwrap_or_default();
        if !in_sandbox.is_empty() {
            if let Some(parent) = host.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            if std::fs::write(&host, &in_sandbox).is_ok() {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(&host, std::fs::Permissions::from_mode(0o600));
            }
            continue;
        }
        let Ok(saved) = std::fs::read(&host) else {
            continue;
        };
        let restore = format!(
            "mkdir -p \"$(dirname \"$HOME\"/{r})\" && cat > \"$HOME\"/{r} && chmod 600 \"$HOME\"/{r}",
            r = sh_quote(rel)
        );
        if let Err(e) = fleet.write(&restore, &saved, Duration::from_secs(30)) {
            eprintln!("skein: could not restore the {rel} login into {sandbox}: {e}");
        }
    }
}

/// Make sure a placed box has a live session, restarting it from its own tree if not.
///
/// The box is its **tree**; the session is disposable. A fleet box's tmux server does not survive
/// the sandbox stopping — measured: `skein start` brought a box up at 14:12 with a live server, and
/// after the sandbox cycled the checkout, the private HOME and the cgroup ceiling were all intact
/// while the server was gone. Without this, every such box is unreachable until someone re-runs
/// `skein start`, and what they see first is `nsenter: cannot open /proc/<pid>/ns/user` — an error
/// about a namespace, for a box that simply needs starting again.
///
/// A no-op for a box with a live session, and for a box that isn't placed (its sandbox is its box,
/// and sbx starts that itself). Never clones: a missing tree is a different problem and saying so is
/// more useful than silently rebuilding one.
pub fn ensure_box_session(name: &str) -> Result<(), String> {
    let Some(record) = shared_record(name) else {
        return Ok(()); // legacy box — sbx owns its lifecycle
    };
    if fleet_liveness().get(name).copied().unwrap_or(false) {
        return Ok(());
    }
    let fleet = own_sandbox(&record.sandbox);
    let (has_tree, has_session) = box_progress(&fleet, name, "skein-shell")?;
    if has_session {
        return Ok(()); // raced with someone else's restart, or the sweep was stale
    }
    if !has_tree {
        return Err(format!(
            "box {name} has no checkout in {} — `skein start {name}` to build one",
            record.sandbox
        ));
    }
    fleet.exec(
        &session_script(name, "skein-shell", "exec bash -l"),
        Duration::from_secs(120),
    )?;
    // The anchor is a new process, so the old record addresses nothing. Re-record before anyone
    // tries to enter the namespace — that is the whole point of doing this here.
    let ns_pid = read_anchor(&record.sandbox, name)?;
    record_place(
        name,
        &PlaceRecord {
            ns_pid,
            ..record.clone()
        },
    )?;
    // The sweep just became wrong in the other direction; a stale "dead" answer would send the very
    // next caller through this again.
    *LIVENESS_CACHE.lock().unwrap_or_else(|e| e.into_inner()) = None;
    // What was observed is that the tmux server was gone, not *why*. A cycled sandbox is the common
    // cause and the one this exists for, but it is not the only one — a killed server or an OOM'd
    // box reach here identically — and asserting it sends anyone debugging to look for a restart
    // that never happened. The tree, the private HOME and the ceiling are all intact either way.
    eprintln!("skein: {name} had no live session, so it was restarted (its work is untouched)");
    Ok(())
}

/// 1.5s micro-cache over the fleet's liveness sweep, for the same reason [`crate::fleet_boxes`] has
/// one: the board asks per box, and a refresh must not become one `sbx exec` per box per tick.
static LIVENESS_CACHE: std::sync::Mutex<
    Option<(std::time::Instant, std::collections::HashMap<String, bool>)>,
> = std::sync::Mutex::new(None);

/// Which boxes in the fleet sandbox have a live session — asked of the sandbox, in one round-trip.
///
/// A shared box's liveness *is* its tmux server: box alive ⇔ server alive ⇔ namespace joinable. That
/// question cannot be answered from the host. The anchor pid belongs to the sandbox's pid namespace,
/// so `/proc/<pid>` on the host asks about an unrelated process — and on macOS there is no `/proc`
/// at all, which reported every running box as stopped.
///
/// Every box at once because the board refreshes all of them, and a stopped sandbox answers for none
/// of them: an empty map means "cannot tell", which the caller reports rather than inventing.
pub fn fleet_liveness() -> std::collections::HashMap<String, bool> {
    let sandbox = fleet_sandbox();
    if sandbox.is_empty() {
        return Default::default();
    }
    if !cfg!(test) {
        let cache = LIVENESS_CACHE.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((at, map)) = cache.as_ref() {
            if at.elapsed() < Duration::from_millis(1500) {
                return map.clone();
            }
        }
    }
    let script = format!(
        "for d in {root}/*/; do n=${{d%/}}; n=${{n##*/}}; s=\"$d/session.sock\"; \
         if [ -S \"$s\" ] && tmux -S \"$s\" has-session 2>/dev/null; then echo \"$n 1\"; \
         else echo \"$n 0\"; fi; done",
        root = fleet_root()
    );
    let map: std::collections::HashMap<String, bool> = own_sandbox(&sandbox)
        .exec(&script, Duration::from_secs(15))
        .unwrap_or_default()
        .lines()
        .filter_map(|line| {
            let (name, live) = line.trim().split_once(' ')?;
            Some((name.to_string(), live == "1"))
        })
        .collect();
    if !cfg!(test) {
        *LIVENESS_CACHE.lock().unwrap_or_else(|e| e.into_inner()) =
            Some((std::time::Instant::now(), map.clone()));
    }
    map
}

/// The command that authenticates `runtime` inside the fleet sandbox, and why it differs per runtime.
///
/// There is no uniform spelling to guess at: `codex login` exists, `claude login` does not.
///
/// Claude's headless-looking option, `setup-token`, was tried here and does not do what this needs:
/// it returns a long-lived token to export as an environment variable and leaves no credential
/// behind, so the sandbox still answered "Not logged in · Please run /login" and `~/.claude` held
/// nothing but `backups`. Seeding a box copies FILES, so the flow that writes one is the flow that
/// works — `/login` inside the TUI. An unknown runtime gets a plain shell rather than a command
/// that fails in an unhelpful way.
pub fn fleet_login_command(runtime: &str) -> String {
    match runtime {
        "codex" => "codex login".into(),
        "claude" => "claude".into(), // then /login inside it
        _ => "exec bash -l".into(),
    }
}

/// Log in to a runtime once, in the sandbox's own HOME, so every box inherits it.
///
/// The sandbox is deliberately not on the board — it is not a box — so there is otherwise no way to
/// reach the one HOME that seeds all the others. Interactive by construction: every one of these
/// flows prints a URL and waits, so the terminal has to be the user's.
pub fn fleet_login(runtime: &str) -> Result<(), String> {
    let sandbox = fleet_sandbox();
    if sandbox.is_empty() {
        return Err("no fleet sandbox configured".into());
    }
    let argv = [
        "exec".to_string(),
        "-it".into(),
        sandbox.clone(),
        "bash".into(),
        "-lc".into(),
        fleet_login_command(runtime),
    ];
    let args: Vec<&str> = argv.iter().map(String::as_str).collect();
    match run_attached("sbx", &args)? {
        0 => Ok(()),
        code => Err(format!("login in {sandbox} exited {code}")),
    }
}

/// The sandbox user's `$HOME`, which is the HOME every command in a box must run with.
///
/// Not a private directory and not empty: `box-session.sh` binds the box's own `home` *over* this
/// path, so entering the namespace with it is what gives the box its private view. Recording an
/// empty string here instead made `Place::wrap` export `HOME=`, and every provisioning step then
/// resolved `$HOME/x` to `/x` — which is how a box tried to symlink `/shared` at the filesystem root
/// and reported "shared home unavailable" and a bare "mkdir: Permission denied".
///
/// Asked of the sandbox rather than assumed to be `/home/agent`: the image chooses the user.
fn sandbox_home(fleet: &Place) -> Result<String, String> {
    let home = fleet
        .exec("printf %s \"$HOME\"", Duration::from_secs(20))?
        .trim()
        .to_string();
    if home.is_empty() || !home.starts_with('/') {
        return Err(format!(
            "the fleet sandbox reported no usable HOME ({home:?}); every box command would run with \
             HOME unset and write to the filesystem root"
        ));
    }
    Ok(home)
}

/// How far a previous launch of this box got: does it have a checkout, and is its session alive?
///
/// One round-trip rather than two, and asked of the sandbox rather than inferred from a placement
/// record — after a failed launch the record is exactly what may be missing.
fn box_progress(fleet: &Place, name: &str, session: &str) -> Result<(bool, bool), String> {
    let script = format!(
        "tree=0; sess=0; \
         [ -e {tree_q}/.git ] && tree=1; \
         tmux -S {sock_q} has-session -t {session_q} 2>/dev/null && sess=1; \
         echo \"$tree$sess\"",
        tree_q = sh_quote(&format!("{}/tree", box_root(name))),
        sock_q = sh_quote(&box_sock(name)),
        session_q = sh_quote(session),
    );
    let out = fleet.exec(&script, Duration::from_secs(30))?;
    let out = out.trim();
    Ok((out.starts_with('1'), out.ends_with('1')))
}

pub fn read_anchor(sandbox: &str, name: &str) -> Result<u32, String> {
    let script = format!("cat {}", sh_quote(&box_pidfile(name)));
    let out = own_sandbox(sandbox).exec(&script, Duration::from_secs(10))?;
    out.trim()
        .parse::<u32>()
        .map_err(|_| format!("box {name} did not report an anchor pid"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::save_repos;
    use crate::testutil::*;

    // The layout is load-bearing rather than cosmetic: box-session.sh binds the box's own /tmp and
    // $HOME over the sandbox's, so anything skein must read from OUTSIDE the box — the tmux socket
    // and the anchor pid — has to live somewhere neither bind covers.
    #[test]
    fn a_boxs_paths_avoid_everything_its_namespace_binds_over() {
        for path in [
            box_root("web-main"),
            box_sock("web-main"),
            box_pidfile("web-main"),
            box_session_path(),
        ] {
            assert!(path.starts_with("/boxes/"), "{path} escaped the layout");
            assert!(
                !path.starts_with("/tmp/"),
                "{path} is under the private /tmp"
            );
            assert!(!path.contains("/home/"), "{path} is under a private HOME");
        }
        assert_eq!(box_sock("web-main"), "/boxes/web-main/session.sock");
        assert_eq!(box_pidfile("web-main"), "/boxes/web-main/anchor.pid");
    }

    // Migration copies a box's home faithfully, and the tracker token is the one thing a faithful
    // copy does not preserve — the gateway binds it to a box's lifetime, not to the bytes. So a
    // migrated box that HAD one has to be re-issued one, and a box that never had one must be left
    // exactly as it is: moving a box is not consent to mint it a credential.
    #[test]
    fn a_migrated_box_reissues_only_a_tracker_token_it_actually_carried() {
        use std::fs;
        use std::os::unix::fs::PermissionsExt;
        let _g = env_lock();
        let dir = tempdir();
        std::env::set_var("SKEIN_HOME", &dir);
        *LIVENESS_CACHE.lock().unwrap_or_else(|e| e.into_inner()) = None;

        record_place(
            "web-main",
            &PlaceRecord {
                sandbox: "skein-fleet".into(),
                ns_pid: 4242,
                home: "/boxes/web-main/home".into(),
                tree: "/boxes/web-main/tree".into(),
                sock: "/boxes/web-main/session.sock".into(),
            },
        )
        .unwrap();
        let mut config = load_config();
        config.fleet_sandbox = "skein-fleet".into();
        save_config(&config).unwrap();
        // Configured far enough that a mint would be ATTEMPTED — otherwise both cases would refuse
        // for the same unrelated reason and the test would prove nothing about the gate.
        crate::upsert_connection(Some("shared"), "shared", "http://127.0.0.1:9", None).unwrap();
        crate::set_connection_token("shared", "plane_api_x").unwrap();

        let log = dir.join("sbx.log");
        let bin = dir.join("bin");
        fs::create_dir_all(&bin).unwrap();
        let sbx = bin.join("sbx");
        // Answers the credential probe, and nothing else: silence from the liveness sweep reads as
        // "cannot tell", which stops `sync_provision_box` before it reaches the network.
        fs::write(
            &sbx,
            "#!/bin/sh\necho \"$@\" >> \"$FAKE_SBX_LOG\"\n\
             case \"$*\" in *.config/sync/env*) [ -n \"$FAKE_BOX_HAS_CRED\" ] && echo yes ;; esac\n\
             exit 0\n",
        )
        .unwrap();
        fs::set_permissions(&sbx, fs::Permissions::from_mode(0o755)).unwrap();
        let path = std::env::var("PATH").unwrap_or_default();
        std::env::set_var("PATH", format!("{}:{path}", bin.display()));
        std::env::set_var("FAKE_SBX_LOG", &log);

        let calls = || fs::read_to_string(&log).unwrap_or_default().lines().count();

        // No credential: the probe runs, finds nothing, and that is the end of it.
        std::env::remove_var("FAKE_BOX_HAS_CRED");
        fs::write(&log, "").unwrap();
        remint_tracker_token("web-main");
        assert_eq!(
            calls(),
            1,
            "a box that never had a tracker token must cost one probe and no mint"
        );

        // Carried one: it goes on to re-issue. It gets no further than the liveness check here, and
        // that is the point — the assertion is that it TRIED, without a live gateway to try against.
        std::env::set_var("FAKE_BOX_HAS_CRED", "1");
        *LIVENESS_CACHE.lock().unwrap_or_else(|e| e.into_inner()) = None;
        fs::write(&log, "").unwrap();
        remint_tracker_token("web-main");
        assert!(
            calls() > 1,
            "a box carrying a tracker credential must be re-issued one, not left holding a dead token"
        );

        std::env::set_var("PATH", path);
        std::env::remove_var("FAKE_SBX_LOG");
        std::env::remove_var("FAKE_BOX_HAS_CRED");
        forget_place("web-main");
        std::env::remove_var("SKEIN_HOME");
    }

    // A repo's store is a HOST path, and sbx mounts a workspace at its host absolute path — so the
    // very same string addresses the store on the host and inside the fleet sandbox. Nothing in
    // skein translates paths across that boundary, and this is why it never has to.
    #[test]
    fn a_repos_store_is_reachable_at_the_same_path_inside_the_fleet_sandbox() {
        let _g = env_lock();
        let home = tempdir();
        std::env::set_var("SKEIN_HOME", &home);
        let workspace = fleet_workspace();
        let store = home.join("repos/web/store/.claude");
        assert!(
            store.starts_with(&workspace),
            "{} must sit under the mounted workspace {workspace}",
            store.display()
        );
        std::env::remove_var("SKEIN_HOME");
    }

    // The sandbox is agentless and mounts the store parent, not any one repo — the two properties
    // that let it host boxes from every repo without being recreated when one is added.
    #[test]
    fn the_fleet_sandbox_is_agentless_and_mounts_the_store_parent() {
        let _g = env_lock();
        std::env::set_var("SKEIN_HOME", tempdir());
        let argv = create_argv("skein-fleet", &["/h/.skein/repos".to_string()]);
        assert_eq!(&argv[..3], ["create", "--name", "skein-fleet"]);
        assert_eq!(
            &argv[argv.len() - 2..],
            ["shell", "/h/.skein/repos"],
            "agentless, and the workspace is the store parent"
        );
        // Both are ceilings the boxes SHARE rather than one reservation each, which is what makes
        // them safe to set generously — and why a default is better here than deferring to sbx's.
        let flags = argv.join(" ");
        assert!(flags.contains("-m 26g"), "{flags}");
        assert!(
            flags.contains("--cpus "),
            "CPUs default to every host core but one, so the host keeps answering: {flags}"
        );
        assert!(
            !argv.contains(&"--clone".to_string()),
            "the sandbox is not a checkout; boxes clone from the remote onto VM-local disk"
        );
        std::env::remove_var("SKEIN_HOME");
    }

    // A repo skein manages keeps its work clone and its store under the one mounted workspace, and
    // that is the case the design was built around. A repo ADOPTED in place — or pointed at a store
    // the user already had, which is how this very project is registered — sits anywhere on the host.
    // Missing that mount does not fail loudly: the clone succeeds, the session starts, and the box
    // comes up with no store to link, no hooks, and no probe, looking entirely healthy.
    #[test]
    fn a_repo_that_lives_outside_the_workspace_is_still_mounted() {
        let _g = env_lock();
        let home = tempdir();
        std::env::set_var("SKEIN_HOME", &home);
        let managed = home.join("repos/web");
        let mounts = {
            let workspace = fleet_workspace();
            // Managed: both paths are already covered by the workspace, so neither is mounted twice.
            assert!(under(
                &managed.join("store/.claude").to_string_lossy(),
                &workspace
            ));
            assert!(under(&managed.join("work").to_string_lossy(), &workspace));
            // Adopted: outside it, so it must be named explicitly.
            assert!(!under("/Users/y/dev/skein-shared/.claude", &workspace));
            vec![workspace]
        };
        assert_eq!(mounts.len(), 1, "the workspace is always mounted first");
        std::env::remove_var("SKEIN_HOME");
    }

    // A mis-parsed ceiling is worse than no ceiling: it would silently cap every box at a number
    // nobody chose, and the symptom is builds dying with no explanation. So an unreadable size
    // yields None and the box runs uncapped-but-loud, rather than capped-and-wrong.
    #[test]
    fn a_memory_size_is_read_or_refused_never_guessed() {
        assert_eq!(parse_mib("26g"), Some(26624));
        assert_eq!(parse_mib(" 26G "), Some(26624));
        assert_eq!(
            parse_mib("26gi"),
            Some(26624),
            "the i suffix is the same size"
        );
        assert_eq!(parse_mib("512m"), Some(512));
        assert_eq!(
            parse_mib("2097152"),
            Some(2),
            "a bare number is bytes, as sbx reads it"
        );
        for bad in ["", "lots", "26 gigs", "g", "-4g"] {
            assert_eq!(parse_mib(bad), None, "{bad:?} must not parse to a number");
        }
    }

    // The ceiling exists so ONE runaway box cannot take the fleet down with it. That means max sits
    // below the fleet total (or it protects nothing) and high sits below max (or the kernel kills
    /// The fleet's disk is one shared filesystem, and sbx takes its size from the environment rather
    /// than from `sbx create`'s argv — so a knob that only reached the argv would set nothing at all.
    #[test]
    fn the_fleets_disk_size_travels_in_the_environment_not_the_argv() {
        let _g = env_lock();
        let home = tempdir();
        std::env::set_var("SKEIN_HOME", &home);

        save_config(&Config::default()).unwrap();
        assert!(
            create_env().is_empty(),
            "unset must leave sbx on its own default rather than pinning one skein invented"
        );

        save_config(&Config {
            fleet_disk: "60g".into(),
            ..Config::default()
        })
        .unwrap();
        assert_eq!(
            create_env(),
            vec![("DOCKER_SANDBOXES_ROOT_SIZE".to_string(), "60g".to_string())]
        );
        let argv = create_argv("skein-fleet", &[]);
        assert!(
            !argv.iter().any(|a| a.contains("60g")),
            "sbx create has no disk flag; putting one in the argv would be rejected: {argv:?}"
        );
    }

    // the box instead of throttling it, turning a slow build into a lost turn).
    #[test]
    fn a_boxs_ceiling_protects_the_fleet_and_throttles_before_it_kills() {
        let _g = env_lock();
        let home = tempdir();
        std::env::set_var("SKEIN_HOME", &home);
        save_config(&Config {
            fleet_memory: "26g".into(),
            ..Config::default()
        })
        .unwrap();
        let spec = box_limits();
        let get = |k: &str| -> u64 {
            let raw = spec
                .split(',')
                .find_map(|p| p.strip_prefix(&format!("{k}=")))
                .unwrap_or_else(|| panic!("{k} missing from {spec}"));
            parse_mib(raw).unwrap()
        };
        let (max, high) = (get("max"), get("high"));
        assert!(
            max < 26624,
            "a cap at or above the fleet total protects nothing: {spec}"
        );
        assert!(high < max, "high must throttle before max kills: {spec}");
        assert!(
            max > 26624 / 2,
            "a cap this tight makes a normal build fail; the point is one box CAN be big: {spec}"
        );
        assert!(
            spec.contains("pids="),
            "a fork bomb in one box starves every other: {spec}"
        );
        // CPU is deliberately absent — see box_limits.
        assert!(
            !spec.contains("cpu"),
            "capping CPU idles cores while a box waits, which is the waste this design ends: {spec}"
        );

        // An explicit value always wins over the derivation.
        save_config(&Config {
            fleet_memory: "26g".into(),
            box_memory_max: "4g".into(),
            ..Config::default()
        })
        .unwrap();
        assert!(box_limits().contains("max=4g"), "{}", box_limits());
        std::env::remove_var("SKEIN_HOME");
    }

    // The one thing that differs between a resize and a migration, and it decides whether a
    // long-lived box keeps its conversation.
    //
    // A box already in the fleet host-binds ~/.claude/projects, so tarring it copies a virtiofs
    // directory out to the store and straight back to arrive at the file that never left. A box
    // being MIGRATED in has it on VM-local disk, and that VM is about to stop — leaving it out is
    // not an optimisation, it is losing the conversation, which for an old box is most of its value.
    //
    // Tested here rather than in the launch harness on purpose: there, `sbx exec` runs locally, so a
    // "legacy box" would read the developer's own $HOME. The first version of this test did exactly
    // that and tarred up real transcripts — the harness cannot fake a VM it has to enter.
    #[test]
    fn a_migrating_box_carries_its_conversation_and_a_fleet_box_does_not() {
        let migrating = agent_state_tar("/snap", true, "$HOME");
        assert!(
            migrating.contains(".claude/projects") && migrating.contains(".codex/sessions"),
            "a box moving in from its own sandbox would lose its whole conversation: {migrating}"
        );

        let already_in = agent_state_tar("/snap", false, "/boxes/web-main/home");
        assert!(
            !already_in.contains(".claude/projects"),
            "the transcript is host-bound already; copying it is pure virtiofs waste: {already_in}"
        );

        // A fleet box's HOME is named absolutely, because the tar runs in the SANDBOX rather than
        // inside the box — that is what lets a box whose session has died still have its work saved.
        assert!(
            already_in.contains("/boxes/web-main/home") && !already_in.contains("$HOME"),
            "a dead box's private HOME must still be addressable: {already_in}"
        );

        // The allowlist rule holds on both paths — this is host-side shared data.
        for spec in [&migrating, &already_in] {
            assert!(
                !spec.contains("credentials"),
                "a credential would be copied into the repo store: {spec}"
            );
            // Everything genuinely VM-local travels either way.
            assert!(spec.contains(".claude.json"), "{spec}");
            assert!(spec.contains(".claude/todos"), "{spec}");
        }
    }

    // A migrated box works in a new directory, and the runtimes key a transcript by that directory.
    // Without this the conversation arrives intact and invisible.
    #[test]
    fn a_migrated_boxs_conversation_follows_it_to_the_new_checkout() {
        use std::{env, fs};
        let _g = env_lock();
        let home = tempdir();
        env::set_var("SKEIN_HOME", &home);
        env::set_var("SKEIN_FLEET_ROOT", "/boxes");
        assert_eq!(
            transcript_slug("/Users/you/.skein/repos/sync/work"),
            "-Users-you--skein-repos-sync-work",
            "the slug rule is read off a real box, not invented"
        );

        let projects = std::path::PathBuf::from(box_state("example-box-9")).join("claude-projects");
        let old = projects.join("-Users-you--skein-repos-sync-work");
        fs::create_dir_all(&old).unwrap();
        fs::write(old.join("a.jsonl"), "{}").unwrap();
        fs::write(old.join("b.jsonl"), "{}").unwrap();
        // an empty slug from a one-off command elsewhere must not win
        fs::create_dir_all(projects.join("-tmp")).unwrap();

        assert_eq!(realign_transcript("example-box-9").unwrap(), 2);
        let now = projects.join("-boxes-example-box-9-tree");
        assert!(now.join("a.jsonl").exists() && now.join("b.jsonl").exists());
        assert!(
            old.join("a.jsonl").exists(),
            "copied, not moved — the old directory is the record of where it happened"
        );

        // A box with its own conversation must never have another merged into it.
        fs::write(now.join("own.jsonl"), "{}").unwrap();
        fs::write(old.join("c.jsonl"), "{}").unwrap();
        assert_eq!(realign_transcript("example-box-9").unwrap(), 0);
        assert!(!now.join("c.jsonl").exists());

        env::remove_var("SKEIN_FLEET_ROOT");
        env::remove_var("SKEIN_HOME");
    }

    /// A box pushes to the repo's remote. For a repo adopted in place the clone comes from the
    /// host's checkout — fast, and it carries commits the host has not pushed — but `git clone
    /// <path>` names that path `origin`, and a box whose origin is a directory on someone's laptop
    /// is a box that cannot open a PR. It also fails outright the moment the box works on the branch
    /// the host has checked out, which is the normal state of affairs for skein's own box.
    #[test]
    fn a_box_cloned_from_the_host_still_pushes_to_the_remote() {
        let script = clone_script(
            "web-main",
            "/Users/you/work/web",
            "main",
            "feat/auth",
            "git@github.com:o/r.git",
        );
        assert!(
            script.contains("git clone --branch 'main' '/Users/you/work/web'"),
            "still cloned locally — the point is the push target, not the fetch: {script}"
        );
        assert!(
            script.contains("git remote set-url origin 'git@github.com:o/r.git'"),
            "origin must be the remote the repo actually pushes to: {script}"
        );
        assert!(
            script.contains("git remote add local '/Users/you/work/web'"),
            "the host clone stays reachable by name; re-pointing origin must not lose it: {script}"
        );
        let after_checkout = script.split("git checkout -B").nth(1).unwrap_or_default();
        assert!(
            after_checkout.contains("set-url"),
            "re-pointing before the branch exists would leave a box with no checkout: {script}"
        );

        // A URL source already clones from where it pushes; touching origin there could only break it.
        let direct = clone_script(
            "web-main",
            "git@github.com:o/r.git",
            "main",
            "feat/auth",
            "",
        );
        assert!(
            !direct.contains("remote set-url") && !direct.contains("remote add"),
            "nothing to repair when the source is the remote: {direct}"
        );

        // And the same repair for the boxes already on disk, which will never be re-cloned.
        let repair =
            origin_repair_script("web-main", "/Users/you/work/web", "git@github.com:o/r.git");
        assert!(
            repair.contains("[ \"$cur\" = '/Users/you/work/web' ] || exit 0"),
            "only the URL skein put there may be rewritten; a hand-set origin is someone's \
             deliberate choice: {repair}"
        );
        assert!(repair.contains("git remote set-url origin 'git@github.com:o/r.git'"));
    }

    /// The hosts to trust come from where boxes PUSH as well as where they clone. An adopted repo
    /// has a path for a source and its remote only on `origin` — so reading `source` alone left the
    /// box with no `known_hosts` entry for the one host it actually talks to.
    #[test]
    fn host_trust_covers_the_remote_a_box_pushes_to() {
        let _g = env_lock();
        let home = tempdir();
        std::env::set_var("SKEIN_HOME", &home);
        let work = home.join("adopted");
        std::fs::create_dir_all(&work).unwrap();
        let git = |args: &[&str]| {
            std::process::Command::new("git")
                .args(args)
                .current_dir(&work)
                .output()
                .expect("git");
        };
        git(&["init", "-q"]);
        git(&["remote", "add", "origin", "git@gitlab.example.com:o/r.git"]);

        save_repos(&[Repo {
            id: "adopted".into(),
            // Adopted in place: the source is the checkout, not a URL.
            source: work.to_string_lossy().into_owned(),
            work: work.to_string_lossy().into_owned(),
            store: home.join("store").to_string_lossy().into_owned(),
            agent: "claude".into(),
            plane_project: String::new(),
            sync_connection: String::new(),
            sync_gateway_url: String::new(),
        }])
        .unwrap();

        assert_eq!(
            ssh_hosts(),
            vec!["gitlab.example.com".to_string()],
            "a repo whose only SSH URL is on origin still needs its host trusted"
        );
        assert!(
            known_hosts_script(&ssh_hosts()).contains("StrictHostKeyChecking=accept-new"),
            "trust an unknown host once; still refuse a CHANGED one"
        );
    }

    /// A box has a private HOME and a freshly cloned tree, so it starts with no committer at all —
    /// and finds out at `git commit`, which is after the work, not before it.
    #[test]
    fn a_box_is_told_who_it_commits_as_before_it_needs_to_know() {
        let _g = env_lock();
        let home = tempdir();
        // Deliberately NOT setting SKEIN_FLEET_ROOT: nothing here reads it, and a test that sets a
        // global other tests read is a test that breaks them from another thread.
        std::env::set_var("SKEIN_HOME", &home);
        let work = home.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let git = |args: &[&str]| {
            std::process::Command::new("git")
                .args(args)
                .current_dir(&work)
                .output()
                .expect("git");
        };
        git(&["init", "-q"]);
        git(&["config", "user.name", "Host Default"]);
        git(&["config", "user.email", "host@example.com"]);
        let repo = Repo {
            id: "web".into(),
            source: work.to_string_lossy().into_owned(),
            work: work.to_string_lossy().into_owned(),
            store: home.join("store").to_string_lossy().into_owned(),
            agent: "claude".into(),
            plane_project: String::new(),
            sync_connection: String::new(),
            sync_gateway_url: String::new(),
        };

        save_config(&Config::default()).unwrap();
        assert_eq!(
            box_identity("web-main", &repo),
            ("Host Default".into(), "host@example.com".into()),
            "with nothing configured, the host clone already knows — asking the user would be a \
             question skein can answer itself"
        );

        save_config(&Config {
            git_name: "Fleet".into(),
            git_email: "fleet@example.com".into(),
            ..Config::default()
        })
        .unwrap();
        assert_eq!(
            box_identity("web-main", &repo).0,
            "Fleet",
            "the setting is the answer for every box that did not choose one"
        );

        set_box_identity("web-main", Some(("Client A", "a@client.example"))).unwrap();
        assert_eq!(
            box_identity("web-main", &repo),
            ("Client A".into(), "a@client.example".into()),
            "a box created on someone else's behalf commits as them"
        );
        assert_eq!(
            box_identity("web-other", &repo).0,
            "Fleet",
            "and only that box — its siblings keep the default"
        );

        set_box_identity("web-main", None).unwrap();
        assert_eq!(box_identity("web-main", &repo).0, "Fleet");

        // --global, because the checkout is re-cloned by every rebuild, resize and migration; and
        // never over an identity already set inside the box.
        let script = identity_script("Fleet", "fleet@example.com");
        assert!(script.contains("git config --global user.name 'Fleet'"));
        assert!(
            script.contains("--get user.name >/dev/null 2>&1 ||"),
            "only what is missing: an identity set in the box by hand is someone's choice: {script}"
        );
        assert!(
            identity_script("", "").is_empty(),
            "nothing configured and nothing on the host ⇒ nothing to run"
        );
        std::env::remove_var("SKEIN_HOME");
    }

    /// Memory has a kernel ceiling per box; disk has one filesystem and no ceiling at all. So the
    /// allowance is a number skein measures against — which is exactly what lets it change under a
    /// running box, and why it must never be described as a quota.
    #[test]
    fn a_boxs_disk_allowance_is_its_own_and_changes_without_a_restart() {
        let _g = env_lock();
        let home = tempdir();
        std::env::set_var("SKEIN_HOME", &home);

        save_config(&Config::default()).unwrap();
        assert_eq!(
            box_disk_limit("web-main"),
            Some(10 * 1024),
            "10g by default — a fleet where every box may take the whole disk has no default at all"
        );

        set_box_disk_limit("web-main", Some("40g")).unwrap();
        assert_eq!(box_disk_limit("web-main"), Some(40 * 1024));
        assert_eq!(
            box_disk_limit("web-other"),
            Some(10 * 1024),
            "one box's allowance is not a decision about the rest"
        );

        // `none` is a decision — this box may use the whole disk — and distinct from having no
        // opinion, which is what a cleared field means and inherits the default again.
        set_box_disk_limit("web-main", Some("none")).unwrap();
        assert_eq!(box_disk_limit("web-main"), None);
        set_box_disk_limit("web-main", Some("unlimited")).unwrap();
        assert_eq!(box_disk_limit("web-main"), None);
        set_box_disk_limit("web-main", None).unwrap();
        assert_eq!(box_disk_limit("web-main"), Some(10 * 1024));

        // A size that parses to nothing would read as "limited" and behave as "unlimited".
        assert!(set_box_disk_limit("web-main", Some("plenty")).is_err());
        assert_eq!(box_disk_limit("web-main"), Some(10 * 1024));

        save_config(&Config {
            box_disk_max: String::new(),
            ..Config::default()
        })
        .unwrap();
        assert_eq!(
            box_disk_limit("web-main"),
            None,
            "blank default ⇒ unlimited"
        );
        std::env::remove_var("SKEIN_HOME");
    }

    #[test]
    fn one_mount_covers_everything_beneath_it() {
        assert!(under("/a/b", "/a"), "a child is covered");
        assert!(under("/a", "/a"), "the directory itself is covered");
        assert!(
            under("/a/b", "/a/"),
            "a trailing slash is not a different path"
        );
        assert!(
            !under("/ab", "/a"),
            "a prefix is not a parent — /ab would go unmounted while looking covered"
        );
    }

    // A box that already has a checkout is refused, not reused. Silently adopting the tree left by a
    // previous box of the same name would hand the new one someone else's uncommitted work.
    #[test]
    fn preparing_a_checkout_starts_from_the_remote_base_and_never_reuses_a_tree() {
        let script = clone_script(
            "web-main",
            "git@github.com:o/r.git",
            "main",
            "feat/auth",
            "",
        );
        // The quoting closes before `/.git`, which the shell concatenates back into one word.
        assert!(script.contains("if [ -e '/boxes/web-main/tree'/.git ]"));
        assert!(script.contains("exit 1"));
        assert!(
            script.contains("git clone --branch 'main' 'git@github.com:o/r.git'"),
            "from the remote at the base branch — the same base the diff is taken against"
        );
        assert!(
            script.contains("git checkout -B 'feat/auth'"),
            "a branch with a slash is one argument, not a path"
        );
        // A base the remote does not have must not be fatal. It cost a real migration: the old
        // sandbox was already stopped, the clone refused `--branch main` on a repo whose default is
        // `master`, and the box existed in neither place until someone woke the old sandbox by hand.
        assert!(
            script.contains("|| {") && script.matches("git clone").count() == 2,
            "a wrong base must fall back to the remote's default, not strand the box: {script}"
        );
        // And when skein could not learn the base at all, it asks the remote instead of guessing.
        let blind = clone_script("web-main", "git@github.com:o/r.git", "", "feat/auth", "");
        assert!(
            blind.contains("git clone 'git@github.com:o/r.git'") && !blind.contains("--branch"),
            "no base means let git use the remote's default: {blind}"
        );
    }

    // The snapshot carries what the remote does not have. `--all` copied every object the repo had
    // ever held into the store, over a mount several times slower than local disk: a box with a
    // 1.8 GB .git took a migration past its ten-minute budget copying history the remote already
    // had, and a resize would do that for every box at once.
    #[test]
    fn a_snapshot_bundles_the_unpushed_work_not_the_whole_history() {
        use std::fs;
        let dir = tempdir();
        let origin = dir.join("origin");
        let tree = dir.join("tree");
        let home = dir.join("home");
        fs::create_dir_all(&origin).unwrap();
        fs::create_dir_all(&home).unwrap();
        let sh = |cwd: &std::path::Path, script: &str| -> std::process::Output {
            std::process::Command::new("bash")
                .current_dir(cwd)
                .arg("-c")
                .arg(script)
                .env("HOME", &home)
                .env("GIT_AUTHOR_NAME", "t")
                .env("GIT_AUTHOR_EMAIL", "t@e")
                .env("GIT_COMMITTER_NAME", "t")
                .env("GIT_COMMITTER_EMAIL", "t@e")
                .output()
                .unwrap()
        };
        // History the remote already has: one large blob, the stand-in for a 1.8 GB .git.
        let out = sh(&origin, "git init -q -b main .");
        assert!(out.status.success(), "{out:?}");
        fs::write(origin.join("big.bin"), vec![7u8; 6 * 1024 * 1024]).unwrap();
        let out = sh(&origin, "git add -A && git commit -qm history");
        assert!(out.status.success(), "{out:?}");

        let out = sh(
            &dir,
            &format!("git clone -q {} {}", origin.display(), tree.display()),
        );
        assert!(out.status.success(), "{out:?}");
        // The only thing that would actually be lost: one small unpushed commit.
        fs::write(tree.join("work.txt"), "the unpushed work\n").unwrap();
        let out = sh(&tree, "git add -A && git commit -qm unpushed");
        assert!(out.status.success(), "{out:?}");

        let snapshot = dir.join("snap");
        let script = snapshot_script(
            &snapshot.to_string_lossy(),
            "demo-main",
            true,
            &home.to_string_lossy(),
        );
        let out = sh(&tree, &script);
        assert!(out.status.success(), "snapshot failed: {out:?}");

        let bundle = snapshot.join("repo.bundle");
        let size = fs::metadata(&bundle).unwrap().len();
        assert!(
            size < 1024 * 1024,
            "the bundle re-copied history the remote already has: {size} bytes"
        );
        // Small, and still complete: the unpushed commit is in there, and a fresh clone of the same
        // origin — which is exactly what the box is rebuilt from — can open it.
        let restored = dir.join("restored");
        let out = sh(
            &dir,
            &format!("git clone -q {} {}", origin.display(), restored.display()),
        );
        assert!(out.status.success(), "{out:?}");
        let out = sh(
            &restored,
            &format!(
                "git fetch -q {} 'refs/heads/*:refs/remotes/snapshot/*' && \
                 git checkout -q -B main refs/remotes/snapshot/main && cat work.txt",
                bundle.display()
            ),
        );
        assert!(
            out.status.success() && String::from_utf8_lossy(&out.stdout).contains("unpushed work"),
            "a thin bundle must still restore the work it was taken for: {out:?}"
        );

        // A box with nothing unpushed asks git for an empty bundle, which git refuses to write. The
        // fallback carries the tip alone — the restore needs *a* bundle, and the kit treats a
        // missing one as a failed snapshot.
        let clean = dir.join("clean");
        let out = sh(
            &dir,
            &format!("git clone -q {} {}", origin.display(), clean.display()),
        );
        assert!(out.status.success(), "{out:?}");
        let snapshot2 = dir.join("snap2");
        let out = sh(
            &clean,
            &snapshot_script(
                &snapshot2.to_string_lossy(),
                "demo-main",
                true,
                &home.to_string_lossy(),
            ),
        );
        assert!(
            out.status.success(),
            "snapshot of a clean box failed: {out:?}"
        );
        let size2 = fs::metadata(snapshot2.join("repo.bundle")).unwrap().len();
        assert!(
            size2 > 0,
            "the kit reads a missing bundle as a failed snapshot"
        );
        assert!(
            size2 < 1024 * 1024,
            "a clean box must not fall back to the whole history: {size2} bytes"
        );

        // The shape that actually broke a migration: the checked-out branch is fully pushed, but
        // ANOTHER local ref is not. `--not --remotes` drops the pushed branch's ref while the other
        // keeps the bundle non-empty — so it looked healthy and carried nothing the restore could
        // find, and the box's old sandbox had already been stopped by the time that surfaced.
        let out = sh(
            &clean,
            "git checkout -q -b side && git commit -q --allow-empty -m side && git checkout -q main",
        );
        assert!(out.status.success(), "{out:?}");
        let snapshot3 = dir.join("snap3");
        let out = sh(
            &clean,
            &snapshot_script(
                &snapshot3.to_string_lossy(),
                "demo-main",
                true,
                &home.to_string_lossy(),
            ),
        );
        assert!(out.status.success(), "{out:?}");
        let restored2 = dir.join("restored2");
        let out = sh(
            &dir,
            &format!("git clone -q {} {}", origin.display(), restored2.display()),
        );
        assert!(out.status.success(), "{out:?}");
        // The invariant the restore depends on: a ref it can actually check the branch out from.
        let bundle3 = snapshot3.join("repo.bundle");
        let out = sh(
            &restored2,
            &format!(
                "git fetch -q {b} 'refs/heads/*:refs/remotes/snapshot/*' 2>/dev/null && \
                 git rev-parse --verify -q refs/remotes/snapshot/main >/dev/null || \
                 git fetch -q {b} HEAD",
                b = bundle3.display()
            ),
        );
        assert!(
            out.status.success(),
            "the restore must find a ref for the box's branch: {out:?}"
        );
    }

    // Ignored files are work too — `.env`, `.envrc`, local dev config, and the box's own
    // `.skein/journal.md`. The sweep omitted every one of them, so a migrated box came back looking
    // complete and either failed at runtime or had forgotten what it was doing. What it must still
    // refuse is `node_modules/` and `target/`, which is why the rule is size rather than a list of
    // names that would be wrong for the next language.
    //
    // Runs the real script against a real repository: it is shell, and shell is where the bug was.
    #[test]
    fn a_snapshot_carries_ignored_config_but_not_the_build_output() {
        use std::fs;
        let dir = tempdir();
        let tree = dir.join("tree");
        let snapshot = dir.join("snap");
        let home = dir.join("home");
        fs::create_dir_all(&tree).unwrap();
        fs::create_dir_all(&home).unwrap();
        let sh = |cwd: &std::path::Path, script: &str| -> std::process::Output {
            std::process::Command::new("bash")
                .current_dir(cwd)
                .arg("-c")
                .arg(script)
                .env("HOME", &home)
                .env("GIT_AUTHOR_NAME", "t")
                .env("GIT_AUTHOR_EMAIL", "t@e")
                .env("GIT_COMMITTER_NAME", "t")
                .env("GIT_COMMITTER_EMAIL", "t@e")
                .output()
                .unwrap()
        };
        fs::write(
            tree.join(".gitignore"),
            ".env\n.skein/\nnode_modules/\nbig.bin\n",
        )
        .unwrap();
        fs::write(tree.join("src.rs"), "fn main() {}").unwrap();
        let out = sh(
            &tree,
            "git init -q -b main . && git add -A && git commit -qm one",
        );
        assert!(out.status.success(), "{out:?}");

        // Ignored, and all of it work: local config and the box's own journal.
        fs::write(tree.join(".env"), "SECRET=1\n").unwrap();
        fs::create_dir_all(tree.join(".skein")).unwrap();
        fs::write(tree.join(".skein/journal.md"), "what I did\n").unwrap();
        // Ignored, and none of it work: reproducible output, too big to keep copying.
        fs::create_dir_all(tree.join("node_modules/pkg")).unwrap();
        for i in 0..2100 {
            fs::write(tree.join(format!("node_modules/pkg/f{i}")), "x").unwrap();
        }
        fs::write(tree.join("big.bin"), vec![0u8; 11 * 1024 * 1024]).unwrap();
        // Plain untracked files must still be carried, exactly as before.
        fs::write(tree.join("notes.txt"), "scratch\n").unwrap();

        // The three symlink shapes, which is where the rescue went wrong. A `--clone` box's `.env`
        // is a link into `/run/sandbox/source` — the host repo's bind mount — and carrying the LINK
        // hands the fleet box a pointer to a mount that does not exist there.
        use std::os::unix::fs::symlink;
        let outside = dir.join("host-repo");
        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join("env.real"), "FROM_HOST=1\n").unwrap();
        symlink(outside.join("env.real"), tree.join(".env.host")).unwrap();
        symlink("notes.txt", tree.join("notes.link")).unwrap();
        symlink("/run/sandbox/source/.env.gone", tree.join(".env.dangling")).unwrap();
        fs::write(
            tree.join(".gitignore"),
            ".env\n.env.host\n.env.dangling\n.skein/\nnode_modules/\nbig.bin\n",
        )
        .unwrap();

        let script = snapshot_script(
            &snapshot.to_string_lossy(),
            "demo-main",
            true,
            &home.to_string_lossy(),
        );
        let out = sh(&tree, &script);
        assert!(out.status.success(), "snapshot failed: {out:?}");

        let listing = sh(&snapshot, "tar -tzf untracked.tgz");
        let carried = String::from_utf8_lossy(&listing.stdout);
        for want in [".env", ".skein/journal.md", "notes.txt"] {
            assert!(
                carried.lines().any(|l| l.trim_end_matches('/') == want),
                "{want} was left behind: {carried}"
            );
        }
        assert!(
            !carried.contains("node_modules"),
            "a dependency tree does not belong in the store: {carried}"
        );
        assert!(!carried.contains("big.bin"), "{carried}");

        // And it says so, rather than leaving them behind quietly.
        let skipped = fs::read_to_string(snapshot.join(SKIPPED_FILE)).unwrap();
        assert!(skipped.contains("node_modules/"), "{skipped}");
        assert!(skipped.contains("big.bin"), "{skipped}");

        // A link out of the tree is carried as its CONTENT: what it points at will not be there
        // after the move, and the content is the thing worth keeping.
        let unpacked = dir.join("unpacked");
        fs::create_dir_all(&unpacked).unwrap();
        let out = sh(
            &unpacked,
            &format!("tar -xzf {}/untracked.tgz", snapshot.display()),
        );
        assert!(out.status.success(), "{out:?}");
        assert!(
            !unpacked.join(".env.host").is_symlink(),
            "a link into a mount the fleet does not have is a dangling link there"
        );
        assert_eq!(
            fs::read_to_string(unpacked.join(".env.host")).unwrap(),
            "FROM_HOST=1\n"
        );
        // A link INSIDE the tree still means the same thing after the move, so it stays a link.
        assert!(
            unpacked.join("notes.link").is_symlink(),
            "an in-tree symlink must not be flattened into a copy"
        );
        // And one that already resolves to nothing is reported, not carried: there is nothing there.
        assert!(
            skipped.contains(".env.dangling"),
            "a broken link must be named, not silently dropped: {skipped}"
        );
        assert!(!unpacked.join(".env.dangling").exists());
    }

    // The rescue reads a stopped VM and writes into a live box, so its guards matter more than its
    // happy path: it must refuse a box that has no old sandbox rather than start something, and it
    // must never overwrite a file the box has been maintaining since it moved.
    #[test]
    fn recovering_ignored_files_refuses_a_box_with_nothing_to_recover_from() {
        let _g = env_lock();
        let home = tempdir();
        std::env::set_var("SKEIN_HOME", &home);

        let e = recover_ignored("never-migrated").unwrap_err();
        assert!(
            e.contains("not in the fleet"),
            "a box that still owns its sandbox has nothing to fetch: {e}"
        );
        // Reached without spending anything: the old sandbox is never woken, since a box with no
        // placement record never gets that far.
        assert!(recover_ignored("../escape")
            .unwrap_err()
            .contains("invalid"));

        std::env::remove_var("SKEIN_HOME");
    }

    // Both the snapshot and the rescue decide what counts as work worth carrying. They must decide
    // it the same way — two copies of this rule would drift, and the drift would be silent.
    #[test]
    fn the_snapshot_and_the_rescue_agree_on_what_is_worth_carrying() {
        let sweep = ignored_sweep("/snap", "list", "/snap/skipped");
        let snapshot = snapshot_script("/snap", "demo-main", true, "/home/agent");
        assert!(
            snapshot.contains("--others --ignored --exclude-standard --directory"),
            "the snapshot must sweep ignored files at all"
        );
        for rule in ["over 2000 files", "-gt 20480", "-gt 10240"] {
            assert!(sweep.contains(rule), "the sweep lost a rule: {rule}");
            assert!(snapshot.contains(rule), "the snapshot lost a rule: {rule}");
        }
    }

    // A box's turn state describes a SESSION, and the store it is written to outlives the box. So a
    // migrated box came up reading `ended` — the old sandbox's agent died on the way out, its
    // SessionEnd hook recorded that faithfully, and the new box inherited it and looked terminated
    // while sitting there alive. What the box did stays; what it is doing right now does not.
    #[test]
    fn a_new_session_does_not_inherit_the_previous_ones_turn_state() {
        use std::fs;
        let store = tempdir().join("store").join(".claude");
        for dir in ["status", "sessions", "hook-log", "telemetry"] {
            fs::create_dir_all(store.join(dir)).unwrap();
        }
        let repo = Repo {
            id: "bridge".into(),
            source: String::new(),
            work: String::new(),
            store: store.to_string_lossy().into_owned(),
            agent: "claude".into(),
            plane_project: String::new(),
            sync_connection: String::new(),
            sync_gateway_url: String::new(),
        };
        let write = |rel: &str, body: &str| fs::write(store.join(rel), body).unwrap();
        write(
            "status/bridge-main.json",
            r#"{"status":"ended","detail":"session ended: other"}"#,
        );
        write("status/bridge-main.agents", "2");
        write(
            "sessions/bridge-main.json",
            r#"{"lastMessage":"shipped it"}"#,
        );
        write("hook-log/bridge-main.jsonl", "{\"event\":\"ended\"}\n");
        write("telemetry/bridge-main.jsonl", "{\"total\":1}\n");
        // Another box's state must be untouched: these all live in one shared directory.
        write("status/other-box.json", r#"{"status":"working"}"#);

        forget_turn_state(&repo, "bridge-main");

        assert!(
            !store.join("status/bridge-main.json").exists(),
            "a dead session's last word must not greet the new one"
        );
        assert!(
            !store.join("status/bridge-main.agents").exists(),
            "the counter counts processes that died with the old session"
        );
        for kept in [
            "sessions/bridge-main.json",
            "hook-log/bridge-main.jsonl",
            "telemetry/bridge-main.jsonl",
            "status/other-box.json",
        ] {
            assert!(store.join(kept).exists(), "{kept} is history, not a claim");
        }

        // A box being created for the first time has none of this, and that is not an error.
        forget_turn_state(&repo, "brand-new");
    }

    // The base is a ladder, not a guess: the branch the user configured, then the local caches of
    // the remote's default — and every rung is checked against the remote before it is used, so a
    // configured `develop` is honoured on the repos that have one without breaking those that don't.
    #[test]
    fn the_base_branch_is_whatever_the_remote_actually_has() {
        use std::fs;
        let _g = env_lock();
        let home = tempdir();
        std::env::set_var("SKEIN_HOME", &home);

        // A real repository whose default branch is `master`, which is the case that failed.
        let origin = home.join("origin");
        fs::create_dir_all(&origin).unwrap();
        let git = |dir: &std::path::Path, args: &[&str]| {
            let ok = std::process::Command::new("git")
                .current_dir(dir)
                .args(args)
                .env("GIT_AUTHOR_NAME", "t")
                .env("GIT_AUTHOR_EMAIL", "t@e")
                .env("GIT_COMMITTER_NAME", "t")
                .env("GIT_COMMITTER_EMAIL", "t@e")
                .output()
                .unwrap();
            assert!(ok.status.success(), "git {args:?}: {ok:?}");
        };
        git(&origin, &["init", "-b", "master"]);
        fs::write(origin.join("f"), "x").unwrap();
        git(&origin, &["add", "-A"]);
        git(&origin, &["commit", "-m", "one"]);

        let repo = Repo {
            id: "bridge".into(),
            source: origin.to_string_lossy().into_owned(),
            work: origin.to_string_lossy().into_owned(),
            store: String::new(),
            agent: "claude".into(),
            plane_project: String::new(),
            sync_connection: String::new(),
            sync_gateway_url: String::new(),
        };

        let mut config = load_config();
        config.base_branch = String::new();
        save_config(&config).unwrap();
        assert_eq!(
            base_branch(&repo),
            "master",
            "the remote's own default, never an assumed `main`"
        );

        // A configured base the repo does not have must not be taken at face value: it is one
        // setting shared by every repo, so trusting it blindly reintroduces the same failure.
        let mut config = load_config();
        config.base_branch = "main".into();
        save_config(&config).unwrap();
        assert_eq!(
            base_branch(&repo),
            "master",
            "no `main` here — fall through"
        );

        // A configured base the repo DOES have wins, ahead of the remote's default.
        git(&origin, &["branch", "develop"]);
        let mut config = load_config();
        config.base_branch = "develop".into();
        save_config(&config).unwrap();
        assert_eq!(base_branch(&repo), "develop", "the user's own answer leads");

        std::env::remove_var("SKEIN_HOME");
    }

    // Every argument is quoted: a branch like `feat/auth` or a name with a space must reach the
    // launcher whole, and the launcher does its own refusing from there.
    #[test]
    fn starting_a_box_hands_the_launcher_quoted_arguments() {
        // Takes the env lock and pins its own SKEIN_HOME: `session_script` reads the box's cgroup
        // limits out of the config, so without this it can be handed another test's home mid-run
        // and fail on an assertion about a string it never built. Latent for a long time; it only
        // started firing once there were more env-setting tests to race with.
        let _g = env_lock();
        std::env::set_var("SKEIN_HOME", tempdir());
        let script = session_script("web-main", "skein-agent", "claude --continue");
        assert!(
            script.starts_with("'/boxes/.skein/box-session.sh' 'web-main'"),
            "{script}"
        );
        assert!(script.contains("'/boxes/web-main' '/boxes/web-main/anchor.pid' 'skein-agent'"));
        // The host-side state dir the box binds its conversation from — a HOST path, not a /boxes
        // one, because the point of it is to outlive the sandbox that /boxes lives in.
        assert!(
            script.contains(&format!("'{}'", box_state("web-main"))),
            "the box must be told where its durable state lives: {script}"
        );
        assert!(
            !box_state("web-main").starts_with("/boxes"),
            "box state on VM-local disk would defeat the entire point"
        );
        assert!(
            script.ends_with("bash -lc 'claude --continue'"),
            "the agent command stays one argument: {script}"
        );
        std::env::remove_var("SKEIN_HOME");
    }
}
