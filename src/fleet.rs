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
use crate::repos::{branch_of, is_git_url, load_repos, repo_for_box, Repo};
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
            let failure = if std::io::stdin().is_terminal() {
                match run_attached("sbx", &args)? {
                    0 => None,
                    code => Some(format!("sbx exited {code}")),
                }
            } else {
                let (out, err, code) = run_capture_for("sbx", &args, Duration::from_secs(900))?;
                match code {
                    0 => None,
                    _ => Some({
                        let detail = if err.trim().is_empty() { out } else { err };
                        detail.trim().to_string()
                    }),
                }
            };
            if let Some(detail) = failure {
                return Err(format!(
                    "creating fleet sandbox {sandbox}: {detail}\n\
                     if that was a confirmation you never saw, create it once by hand:\n  sbx {}",
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
         command -v bwrap >/dev/null 2>&1 || { echo 'skein: this sandbox image has no bwrap; boxes cannot be isolated in it' >&2; exit 1; };
         [ -n "$need" ] || exit 0;
         log=/tmp/skein-substrate.log;
         { timeout 180 sudo apt-get install -y -qq $need \
             || { timeout 120 sudo apt-get update -qq && timeout 180 sudo apt-get install -y -qq $need; }; } >"$log" 2>&1;
         missing=''; for t in $need; do command -v "$t" >/dev/null 2>&1 || missing="$missing $t"; done;
         [ -z "$missing" ] || {
             echo "skein: the fleet sandbox is missing required tools:$missing";
             echo "skein: apt said (tail of $log inside the sandbox):";
             tail -n 25 "$log" | sed 's/^/  | /';
             exit 1;
         } >&2"#;
    own_sandbox(sandbox)
        .exec(script, Duration::from_secs(400))
        .map(|_| ())
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
pub fn clone_script(name: &str, url: &str, base: &str, branch: &str) -> String {
    let root = box_root(name);
    let tree = format!("{root}/tree");
    format!(
        "set -e; \
         if [ -e {tree_q}/.git ]; then echo 'skein: {name} already has a checkout; destroy the box first' >&2; exit 1; fi; \
         mkdir -p {root_q}; \
         git clone --branch {base_q} {url_q} {tree_q}; \
         cd {tree_q}; \
         git checkout -B {branch_q}",
        tree_q = sh_quote(&tree),
        root_q = sh_quote(&root),
        base_q = sh_quote(base),
        url_q = sh_quote(url),
        branch_q = sh_quote(branch),
    )
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
    if has_tree {
        eprintln!("skein: {name} already has a checkout; keeping it");
    } else {
        let source = clone_source(repo);
        fleet.exec(
            &clone_script(name, &source, &base_branch(repo), branch),
            Duration::from_secs(600),
        )?;
    }
    if has_session {
        eprintln!("skein: {name} already has a live session; keeping it");
    } else {
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
            home: String::new(),
            tree: format!("{}/tree", box_root(name)),
            sock: box_sock(name),
        },
    )?;

    // Through the placement, so it lands in the box's private HOME rather than the sandbox's.
    let boxed = place_of(name).ok_or_else(|| format!("box {name} was not placed"))?;
    boxed.exec(
        &provision_script(name, &repo.store),
        Duration::from_secs(300),
    )?;

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
    let boxed = place_of(name).ok_or_else(|| format!("box {name} is not placed"))?;
    // Written straight into the store, which is host-mounted and readable from both sides — rather
    // than built in the box's private /tmp and copied out a file at a time.
    let build = format!(
        "set -e; mkdir -p {s}; \
         git bundle create {s}/repo.bundle --all; \
         git diff --cached --binary HEAD > {s}/index.patch; \
         git diff --binary > {s}/worktree.patch; \
         git ls-files --others --exclude-standard -z -- . ':(exclude).claude' ':(exclude).claude/**' > {s}/untracked.list; \
         if [ -s {s}/untracked.list ]; then tar --null -T {s}/untracked.list -czf {s}/untracked.tgz; \
         else tar -czf {s}/untracked.tgz --files-from /dev/null; fi; \
         rm -f {s}/untracked.list; \
         printf '{{\"box\":\"%s\",\"branch\":\"%s\",\"head\":\"%s\"}}\\n' {n} \
           \"$(git rev-parse --abbrev-ref HEAD)\" \"$(git rev-parse HEAD)\" > {s}/manifest.json; \
         {agent_state}",
        s = sh_quote(&snapshot),
        n = sh_quote(name),
        // A box already in the fleet host-binds its transcript; one being migrated in does not.
        agent_state = agent_state_tar(&snapshot, shared_record(name).is_none()),
    );
    boxed.exec(&build, Duration::from_secs(600))?;
    Ok(relative)
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
fn agent_state_tar(snapshot: &str, transcript_is_vm_local: bool) -> String {
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
    format!(
        "have=''; for p in {list}; do [ -e \"$HOME/$p\" ] && have=\"$have $p\"; done; \
         if [ -n \"$have\" ]; then tar -C \"$HOME\" -czf {s}/agent-state.tgz $have; \
         else tar -czf {s}/agent-state.tgz --files-from /dev/null; fi",
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
/// still exist, so a migration that goes wrong costs a `sbx start` rather than a day's work — the
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
             ({e}). Nothing is lost: `sbx start {name}` brings the original back exactly as it was, \
             and the snapshot remains at {dir} in the repo store."
        )
    })?;
    Ok(dir)
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
pub fn resize_fleet(memory: &str, cpus: &str) -> Result<Vec<String>, String> {
    let sandbox = fleet_sandbox();
    if sandbox.is_empty() {
        return Err("no fleet sandbox configured; nothing to resize".into());
    }
    let boxes = placed_boxes(&sandbox);

    // ---- phase 1: get everything out, or change nothing ----
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
        ..config
    })?;
    let (out, err, code) = run_capture("sbx", &["rm", "-f", &sandbox])?;
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
    ensure_fleet(&sandbox, &fleet_mounts())?;

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
/// Asked of the host clone rather than assumed to be `main`: skein already manages that clone, and a
/// repo whose default is `master` or `develop` would otherwise fail to clone at all. `origin/HEAD`
/// first because that is the remote's own answer; the clone's current branch next, for a repo with
/// no remote; `main` only when there is nothing to ask.
pub fn base_branch(repo: &Repo) -> String {
    let git = |args: &[&str]| -> Option<String> {
        let mut argv = vec!["-C", repo.work.as_str()];
        argv.extend_from_slice(args);
        let (out, _, code) = run_capture("git", &argv).ok()?;
        let out = out.trim().to_string();
        (code == 0 && !out.is_empty()).then_some(out)
    };
    git(&["symbolic-ref", "--short", "refs/remotes/origin/HEAD"])
        .and_then(|r| r.rsplit_once('/').map(|(_, b)| b.to_string()))
        .or_else(|| git(&["rev-parse", "--abbrev-ref", "HEAD"]).filter(|b| b != "HEAD"))
        .unwrap_or_else(|| "main".to_string())
}

/// Read back the anchor pid `box-session.sh` recorded, so the host can write the box's placement.
///
/// The pid is knowable only inside the sandbox, and only after the session starts — which is why
/// placement is recorded after launch rather than predicted before it.
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
        let migrating = agent_state_tar("/snap", true);
        assert!(
            migrating.contains(".claude/projects") && migrating.contains(".codex/sessions"),
            "a box moving in from its own sandbox would lose its whole conversation: {migrating}"
        );

        let already_in = agent_state_tar("/snap", false);
        assert!(
            !already_in.contains(".claude/projects"),
            "the transcript is host-bound already; copying it is pure virtiofs waste: {already_in}"
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
        let script = clone_script("web-main", "git@github.com:o/r.git", "main", "feat/auth");
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
    }

    // Every argument is quoted: a branch like `feat/auth` or a name with a space must reach the
    // launcher whole, and the launcher does its own refusing from there.
    #[test]
    fn starting_a_box_hands_the_launcher_quoted_arguments() {
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
    }
}
