//! skein — CLI surface over the fleet (status table + attach).
//! The terminal-native client; `skein-server` is the web client. Both share `skein` (lib).

use skein::load_registry;
use std::env;
use std::io::ErrorKind;
use std::process::Command;

const VERSION: &str = "0.1.0";

// ANSI styling (the only thing we hand-roll; the web UI uses CSS).
const RESET: &str = "\x1b[0m";
const BOLD: &str = "\x1b[1m";
const DIM: &str = "\x1b[2m";
const CYAN: &str = "\x1b[36m";

fn main() {
    // Pick up a local .env so $SKEIN_REGISTRY etc. needn't be typed each run (real env vars still
    // win; a malformed file is reported, not silently half-applied). See skein::load_dotenv.
    skein::load_dotenv();
    let args: Vec<String> = env::args().skip(1).collect();
    let cmd = args.first().map(String::as_str).unwrap_or("ls");
    let rest: &[String] = if args.len() > 1 { &args[1..] } else { &[] };

    let result = match cmd {
        "ls" | "status" => cmd_ls(),
        "add" => match rest.first() {
            Some(src) => cmd_add(src, &rest[1..]),
            None => Err("usage: skein add <git-url|path> [--id <id>] [--agent <runtime>]".into()),
        },
        "repos" => cmd_repos(),
        "remove" | "rm" => match rest.first() {
            Some(id) => cmd_remove(id),
            None => Err("usage: skein remove <repo-id>".into()),
        },
        "doctor" => cmd_doctor(),
        "shared" => cmd_shared(rest),
        "start" => match rest.first() {
            Some(name) => cmd_start(name, &rest[1..]),
            None => Err("usage: skein start <box> [--branch <branch>] [--agent <runtime>]".into()),
        },
        "login" => cmd_login(rest.first().map(String::as_str)),
        "resize" => match rest.first() {
            Some(memory) => cmd_resize(memory, rest.get(1).map(String::as_str).unwrap_or("")),
            None => Err("usage: skein resize <memory> [cpus]   e.g. skein resize 26g".into()),
        },
        "migrate" => match rest.first() {
            Some(name) => cmd_migrate(name),
            None => Err("usage: skein migrate <box>   (moves it into the shared sandbox)".into()),
        },
        "recover" => match rest.first() {
            Some(name) => cmd_recover(name),
            None => Err(
                "usage: skein recover <box>   (fetches ignored files its migration left behind)"
                    .into(),
            ),
        },
        "attach" => match rest.first() {
            Some(name) => cmd_attach(name, &rest[1..]),
            None => Err("usage: skein attach <box>".to_string()),
        },
        "version" | "--version" | "-v" => {
            println!("skein {VERSION}");
            Ok(())
        }
        "help" | "--help" | "-h" => {
            print_help();
            Ok(())
        }
        other => Err(format!("unknown command {other:?} (try: skein help)")),
    };

    if let Err(e) = result {
        eprintln!("{DIM}skein:{RESET} {e}");
        std::process::exit(1);
    }
}

fn print_help() {
    print!(
        "skein — see and steer your fleet of agent sandboxes\n\n\
usage:\n  \
skein [ls]            show the fleet (default)\n  \
skein add <url|path>  register a repo (clones a URL; adopts a path in place)\n  \
skein repos           list registered repos\n  \
skein remove <id>     unregister a repo (files left on disk)\n  \
skein start <box>     bring a box up inside the shared sandbox (see fleet_sandbox)\n  \
skein login <runtime> authenticate once in the shared sandbox; every box inherits it\n  \
skein resize <mem>    rebuild the shared sandbox at a new size, carrying every box's work\n  \
skein migrate <box>   move an existing box into the shared sandbox (old one is stopped, not removed)\n  \
skein recover <box>   fetch ignored files (.env, .skein/) an early migration left behind\n  \
skein attach <box>    reconnect; optional: --agent <runtime> --handoff\n  \
skein shared import <box> [--include <name> ...] [--apply]\n  \
                       inspect/import durable files from a box's private home\n  \
skein doctor          check registry, tools, and the shared sandbox if one is on\n  \
skein version\n  \
skein help\n\n\
the web cockpit lives in `skein-server` (run it, open http://127.0.0.1:7878).\n\n\
registry resolution (first match wins):\n  \
$SKEIN_REGISTRY                 full path to sandboxes.json\n  \
$SKEIN_SHARED/sandboxes.json\n  \
<git-toplevel>/../skein-shared/.claude/sandboxes.json\n"
    );
}

/// `skein add <git-url|path> [--id <id>] [--agent <runtime>] [--store <shared-data-folder>]` — register
/// a repo so skein can launch + observe boxes for it with zero repo-side setup. `--store` points the
/// repo at an existing shared `.claude` folder (e.g. thing's `skein-shared/.claude`) so its
/// memory/skills/mailbox/statusline are live across the repo's boxes; omit it to let skein manage one.
fn cmd_add(source: &str, opts: &[String]) -> Result<(), String> {
    let id = flag(opts, "--id");
    let agent = flag(opts, "--agent");
    let store = flag(opts, "--store");
    let repo = skein::add_repo(source, id.as_deref(), agent.as_deref(), store.as_deref())?;
    println!("{BOLD}added{RESET} {CYAN}{}{RESET}", repo.id);
    println!("  {DIM}source{RESET}  {}", repo.source);
    println!("  {DIM}work  {RESET}  {}", repo.work);
    println!("  {DIM}store {RESET}  {}", repo.store);
    if let Some(w) = skein::remote_warning(&repo.work) {
        println!("\n\x1b[33m!\x1b[0m {w}");
    }
    println!(
        "\n{DIM}launch a box:{RESET} open the cockpit and create {CYAN}{}-<branch>{RESET}",
        repo.id
    );
    Ok(())
}

/// Read a `--flag value` pair out of the remaining args (returns the value if present).
fn flag(opts: &[String], name: &str) -> Option<String> {
    opts.iter()
        .position(|a| a == name)
        .and_then(|i| opts.get(i + 1))
        .cloned()
}

fn flags(opts: &[String], name: &str) -> Vec<String> {
    opts.iter()
        .enumerate()
        .filter_map(|(index, value)| (value == name).then_some(opts.get(index + 1)).flatten())
        .cloned()
        .collect()
}

fn cmd_shared(args: &[String]) -> Result<(), String> {
    let Some(action) = args.first().map(String::as_str) else {
        return Err("usage: skein shared import <box> [--include <name> ...] [--apply]".into());
    };
    if action != "import" {
        return Err(format!(
            "unknown shared action {action:?}; usage: skein shared import <box>"
        ));
    }
    let Some(box_name) = args.get(1) else {
        return Err("usage: skein shared import <box> [--include <name> ...] [--apply]".into());
    };
    let opts = &args[2..];
    let selected = flags(opts, "--include");
    let apply = opts.iter().any(|arg| arg == "--apply");
    if apply {
        if selected.is_empty() {
            return Err("--apply requires at least one explicit --include <top-level-name>".into());
        }
        let result = skein::import_shared_home(box_name, &selected)?;
        print!("{result}");
        return Ok(());
    }

    let inventory = skein::shared_home_inventory(box_name)?;
    println!("{BOLD}shared-home import inventory{RESET}  {CYAN}{box_name}{RESET}");
    println!("{DIM}read-only: nothing has been copied{RESET}\n");
    for item in inventory {
        if item.eligible {
            println!(
                "  {BOLD}✓{RESET} {:<36} {:<9} {}",
                item.name,
                item.kind,
                human_bytes(item.bytes)
            );
        } else {
            println!(
                "  {DIM}— {:<36} {:<9} excluded: {}{RESET}",
                item.name, item.kind, item.reason
            );
        }
    }
    println!(
        "\n{DIM}Apply only after reviewing the list:{RESET}\n  skein shared import {box_name} --include <name> [--include <name> ...] --apply\n\n{DIM}Nested hidden files, credentials, dependencies, and build outputs remain excluded. Existing destinations are never overwritten.{RESET}"
    );
    Ok(())
}

fn human_bytes(bytes: u64) -> String {
    const KIB: f64 = 1024.0;
    const MIB: f64 = KIB * 1024.0;
    const GIB: f64 = MIB * 1024.0;
    let bytes = bytes as f64;
    if bytes >= GIB {
        format!("{:.1} GiB", bytes / GIB)
    } else if bytes >= MIB {
        format!("{:.1} MiB", bytes / MIB)
    } else if bytes >= KIB {
        format!("{:.1} KiB", bytes / KIB)
    } else {
        format!("{bytes:.0} B")
    }
}

fn cmd_remove(id: &str) -> Result<(), String> {
    let repo = skein::remove_repo(id)?;
    println!(
        "{BOLD}removed{RESET} {CYAN}{}{RESET} {DIM}(unregistered){RESET}",
        repo.id
    );
    println!(
        "  {DIM}files left on disk — delete if you're sure:{RESET}\n    {}\n    {}",
        repo.work, repo.store
    );
    Ok(())
}

fn cmd_repos() -> Result<(), String> {
    let repos = skein::load_repos();
    if repos.is_empty() {
        println!("{DIM}no repos yet — add one with: skein add <git-url|path>{RESET}");
        return Ok(());
    }
    for r in &repos {
        println!(
            "{BOLD}{CYAN}{}{RESET}  {DIM}{}{RESET}\n  {} {DIM}({}){RESET}",
            r.id, r.agent, r.source, r.work
        );
    }
    println!("\n{DIM}{} repos{RESET}", repos.len());
    Ok(())
}

fn cmd_ls() -> Result<(), String> {
    // Best-effort cross-project mailbox relay pass, so the plain CLI (no skein-server running)
    // still makes progress on cross-project mail rather than only ever relaying when the web
    // cockpit happens to be up.
    if let Err(e) = skein::relay_cross_project_mail() {
        eprintln!("skein: mailbox relay: {e}");
    }
    // Same sbx-sourced, "who-needs-me-first"-sorted fleet the web cockpit shows (sbx ∪ registry).
    let views = skein::load_views()?;
    if views.is_empty() {
        println!(
            "{DIM}the skein is empty — add a repo (skein add <url|path>) then launch a box{RESET}"
        );
        return Ok(());
    }

    let w_name = views
        .iter()
        .map(|v| v.name.len())
        .fold("BOX".len(), usize::max);
    let w_st = views
        .iter()
        .map(|v| v.state.len())
        .fold("STATE".len(), usize::max);
    let w_br = views
        .iter()
        .map(|v| v.branch.len())
        .fold("BRANCH".len(), usize::max);
    let w_age = views
        .iter()
        .map(|v| v.age.len())
        .fold("SEEN".len(), usize::max);

    println!(
        "{BOLD}  {}  {}  {}  {}  DIR{RESET}",
        pad("BOX", w_name),
        pad("STATE", w_st),
        pad("BRANCH", w_br),
        pad("SEEN", w_age),
    );

    for v in &views {
        let name_cell = if v.tier >= 4 {
            format!("{DIM}{}{RESET}", v.name)
        } else {
            format!("{BOLD}{}{RESET}", v.name)
        };
        println!(
            "{} {}  {}  {}  {}  {DIM}{}{RESET}",
            dot(v.tier),
            pad_colored(&name_cell, v.name.len(), w_name),
            pad(&v.state, w_st),
            pad_colored(&format!("{CYAN}{}{RESET}", v.branch), v.branch.len(), w_br),
            pad(&v.age, w_age),
            v.dir,
        );
    }

    println!("\n{DIM}{} boxes{RESET}", views.len());
    Ok(())
}

fn dot(tier: u8) -> &'static str {
    match tier {
        0 => "\x1b[31m●\x1b[0m", // needs-input — red (decision/permission blocking)
        1 => "\x1b[33m●\x1b[0m", // waiting — amber (your move)
        2 => "\x1b[32m●\x1b[0m", // done — green (finished; green = done everywhere)
        3 => "\x1b[34m●\x1b[0m", // working / live — blue (in progress)
        4 => "\x1b[2m●\x1b[0m",  // idle — dim filled
        _ => "\x1b[2m○\x1b[0m",  // stale / unknown — dim hollow
    }
}

fn pad(s: &str, w: usize) -> String {
    if s.len() >= w {
        s.to_string()
    } else {
        format!("{s}{}", " ".repeat(w - s.len()))
    }
}

fn pad_colored(s: &str, visible: usize, w: usize) -> String {
    if visible >= w {
        s.to_string()
    } else {
        format!("{s}{}", " ".repeat(w - visible))
    }
}

/// Preflight: is the registry resolvable and are the external tools skein drives present?
/// Always returns Ok — it's a report, not a gate. Failures print, they don't abort.
fn cmd_doctor() -> Result<(), String> {
    const OK: &str = "\x1b[32m✓\x1b[0m";
    const BAD: &str = "\x1b[31m✗\x1b[0m";
    const WARN: &str = "\x1b[33m!\x1b[0m";
    println!("{BOLD}skein doctor{RESET}\n");

    match load_registry() {
        Ok((b, p)) => println!(
            "{OK} registry      {DIM}{}{RESET} ({} boxes)",
            p.display(),
            b.len()
        ),
        Err(e) => {
            println!("{BAD} registry      {e}");
            println!(
                "{DIM}              from {}{RESET}",
                skein::registry_origin()
            );
        }
    }
    let addr = env::var("SKEIN_ADDR")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "127.0.0.1:7878".into());
    println!("{DIM}·{RESET} bind          http://{addr}  {DIM}($SKEIN_ADDR){RESET}");

    for (prog, why) in [
        ("sbx", "attach + launch boxes"),
        ("git", "host-side diffs"),
        ("gh", "PR / checks / merge"),
    ] {
        if have(prog) {
            println!("{OK} {prog:<13} on PATH  {DIM}{why}{RESET}");
        } else {
            println!("{BAD} {prog:<13} not on PATH — {why} unavailable");
        }
    }
    if have("gh") {
        let authed = Command::new("gh")
            .args(["auth", "status"])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        if authed {
            println!("{OK} gh auth       authenticated");
        } else {
            println!("{WARN} gh auth       `gh auth status` not OK {DIM}(fine if a proxy injects credentials){RESET}");
        }
    }

    // skein-managed repos + its own kit (the repo-agnostic path).
    let repos = skein::load_repos();
    println!(
        "{} repos         {DIM}{} managed{RESET}",
        if repos.is_empty() { WARN } else { OK },
        repos.len()
    );
    let kit = skein::skein_home().join("kit").join("spec.yaml");
    if kit.exists() {
        println!("{OK} kit           {DIM}{}{RESET}", kit.display());
    } else {
        println!("{WARN} kit           {DIM}not written yet (server startup / `skein add` installs it){RESET}");
    }
    let cfg = skein::load_config();
    println!(
        "{DIM}·{RESET} settings      gh-seed:{} ssh-key:{} {DIM}(~/.skein/config.json){RESET}",
        on_off(cfg.seed_gh_secret),
        if cfg.ssh_key.is_empty() { "—" } else { "set" },
    );
    if have("ssh-add") {
        let loaded = Command::new("ssh-add")
            .arg("-l")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        if loaded {
            println!(
                "{OK} ssh agent     keys loaded {DIM}(forwarded into boxes for SSH push){RESET}"
            );
        } else {
            println!("{WARN} ssh agent     no keys loaded {DIM}(SSH git push from boxes will fail — set an ssh key in settings){RESET}");
        }
    }

    // The shared sandbox, checked against the sandbox itself rather than against config.
    //
    // Every one of these fails *quietly* if it is wrong: a box with no bwrap silently shares another
    // box's /tmp and $HOME, a box with no cgroup delegation runs with no ceiling, and a repo the
    // sandbox cannot see produces a box with no store, no hooks and no probe — all of which look
    // like a healthy box that simply never reports. So they are asked, not assumed.
    let fleet = cfg.fleet_sandbox.trim().to_string();
    if fleet.is_empty() {
        println!(
            "\n{DIM}·{RESET} fleet         {DIM}off — every box gets its own sandbox (skein's original model){RESET}"
        );
    } else {
        println!("\n{BOLD}fleet{RESET} {DIM}({fleet}){RESET}");
        match skein::fleet_exists(&fleet) {
            Some(true) => println!("{OK} sandbox       up"),
            Some(false) => println!(
                "{WARN} sandbox       not created yet {DIM}(the next launch creates it){RESET}"
            ),
            None => println!("{BAD} sandbox       sbx did not answer — cannot tell if it exists"),
        }
        let place = skein::own_sandbox(&fleet);
        let probe = |script: &str| {
            place
                .exec(script, std::time::Duration::from_secs(20))
                .map(|o| o.trim().to_string())
                .unwrap_or_default()
        };
        for (tool, why) in [
            (
                "bwrap",
                "without it a box shares another box's /tmp and $HOME",
            ),
            (
                "tmux",
                "the session IS the box; it cannot start without one",
            ),
            ("git", "boxes clone their own checkout"),
        ] {
            if probe(&format!("command -v {tool} >/dev/null && echo yes")) == "yes" {
                println!("{OK} {tool:<13} in the sandbox");
            } else {
                println!("{BAD} {tool:<13} missing in the sandbox — {why}");
            }
        }
        // The ceiling is the whole reason one runaway box does not take the others down.
        if probe("sudo mkdir -p /sys/fs/cgroup/skein 2>/dev/null && echo yes") == "yes" {
            println!(
                "{OK} ceilings      cgroup delegation works {DIM}({}){RESET}",
                skein::box_limits()
            );
        } else {
            println!("{BAD} ceilings      no cgroup delegation — boxes run UNCAPPED, so one runaway build can kill every other box");
        }
        // A mount that is missing produces a box with no store, which looks entirely healthy.
        for path in skein::fleet_mounts() {
            let seen = probe(&format!("test -d '{path}' && echo yes")) == "yes";
            println!(
                "{} mount         {DIM}{path}{RESET}{}",
                if seen { OK } else { BAD },
                if seen {
                    String::new()
                } else {
                    " — not visible in the sandbox; boxes for it would come up with no store".into()
                }
            );
        }
    }

    // The sbx-dependent facts skein can't verify itself — surface them so they're not silent.
    let runtimes = skein::supported_runtimes()
        .iter()
        .map(|runtime| runtime.id)
        .collect::<Vec<_>>()
        .join(", ");
    println!(
        "\n{DIM}host notes:{RESET}\n  {DIM}· runtimes: {runtimes}; each uses its native resume command and its own tmux session.\n  · jq and tmux are required managed-box dependencies; creation fails if either cannot be installed.\n  · HTTPS uses seeded gh credentials. GitHub SSH first needs host trust in the box, then the\n    forwarded host agent (`ssh-add -l`); private keys never enter a box.{RESET}"
    );
    Ok(())
}

fn on_off(b: bool) -> &'static str {
    if b {
        "on"
    } else {
        "off"
    }
}

/// Is `prog` runnable on PATH? (NotFound = absent; any other outcome means it exists.)
fn have(prog: &str) -> bool {
    !matches!(
        Command::new(prog).arg("--version").output(),
        Err(e) if e.kind() == ErrorKind::NotFound
    )
}

/// Bring a box up inside the shared sandbox — the fleet's stand-in for `sbx create`.
///
/// A subcommand rather than a shell line in the launch command because starting a box is a sequence
/// of round-trips into the sandbox, each consuming the previous one's side effects: the anchor pid
/// does not exist until the session runs, and provisioning has to go through the placement that pid
/// produces. `skein attach` then behaves exactly as it always has.
fn cmd_start(name: &str, opts: &[String]) -> Result<(), String> {
    let repo = skein::repo_for_box(name)
        .ok_or_else(|| format!("no registered repo for box {name} — `skein repos` to check"))?;
    let branch =
        flag(opts, "--branch").unwrap_or_else(|| skein::branch_of(name).unwrap_or_default());
    if branch.trim().is_empty() {
        return Err(format!("no branch for box {name}; pass --branch <branch>"));
    }
    let agent = flag(opts, "--agent").unwrap_or_else(|| skein::agent_for_box(name));
    if !skein::valid_runtime(&agent) {
        return Err(format!("unsupported runtime {agent:?}"));
    }
    eprintln!("{DIM}skein:{RESET} starting {name} in the shared sandbox…");
    // The box's own persistent shell, not its agent. `skein attach` starts the runtime — with the
    // full setup it does for every box — into this same tmux server, so the fleet path does not get
    // its own second way of launching an agent to keep in step with the first.
    skein::start_box(name, &repo, &branch, "exec bash -l")?;
    eprintln!("{DIM}skein:{RESET} {name} is up on {branch}");
    Ok(())
}

/// `skein resize <memory> [cpus]` — rebuild the shared sandbox at a new size.
///
/// A CLI command and not only a cockpit button because this is the one operation that destroys the
/// sandbox: `sbx create` may ask for confirmation, and a server has no terminal to answer with — so
/// the riskiest path needs to be runnable somewhere a person is sitting.
fn cmd_resize(memory: &str, cpus: &str) -> Result<(), String> {
    eprintln!(
        "{DIM}skein:{RESET} saving every box's work, then rebuilding the sandbox at {memory}…"
    );
    let failed = skein::resize_fleet(memory, cpus)?;
    if failed.is_empty() {
        eprintln!("{DIM}skein:{RESET} resized; every box came back");
    } else {
        eprintln!(
            "{DIM}skein:{RESET} resized, but these did not come back: {}\n  their work is in the \
             repo store; `skein start <box>` restores it",
            failed.join(", ")
        );
    }
    Ok(())
}

/// `skein login <runtime>` — authenticate once, in the sandbox HOME every box seeds from.
///
/// The credential then flows to every box: new ones seed from it at first start, and running ones
/// reconcile by recency at their next session start. The sandbox is not on the board (it is not a
/// box), so without this there is no way to reach the HOME that seeds all the others.
fn cmd_login(runtime: Option<&str>) -> Result<(), String> {
    let runtime = runtime.unwrap_or("claude");
    if !skein::valid_runtime(runtime) {
        return Err(format!("unsupported runtime {runtime:?}"));
    }
    if runtime == "claude" {
        eprintln!("{DIM}skein:{RESET} type {CYAN}/login{RESET} once it starts, then {CYAN}/exit{RESET} — `setup-token` returns a token to export and leaves no credential to seed boxes with");
    }
    skein::fleet_login(runtime)?;
    eprintln!(
        "{DIM}skein:{RESET} every new box now inherits this login; running boxes pick it up when their session next starts"
    );
    Ok(())
}

/// `skein migrate <box>` — move a box off its own sandbox and into the shared one.
///
/// One box at a time, deliberately: this is the path that cannot be rehearsed against a fake sbx, so
/// the useful thing is to move one, look at it, and only then move the rest. The old sandbox is
/// stopped rather than removed, and the command says how to undo.
fn cmd_migrate(name: &str) -> Result<(), String> {
    eprintln!("{DIM}skein:{RESET} saving {name}'s work and conversation…");
    let dir = skein::migrate_box(name)?;
    println!("{BOLD}migrated{RESET} {CYAN}{name}{RESET} into the shared sandbox");
    println!("  {DIM}snapshot{RESET}  {dir} {DIM}(in the repo store){RESET}");
    println!(
        "\n{DIM}the original sandbox is STOPPED, not removed — check the box, then:{RESET}\n  \
         sbx exec -it {name} bash -l   {DIM}wakes it, to go back{RESET}\n  \
         sbx rm {name}      {DIM}once you are satisfied (this frees its memory reservation for good){RESET}"
    );
    Ok(())
}

/// `skein recover <box>` — fetch the ignored files an early migration left in the old sandbox.
///
/// Only useful for a box migrated before snapshots swept ignored files, and harmless to run on one
/// that was not: it copies nothing it cannot find, and overwrites nothing the box already has.
fn cmd_recover(name: &str) -> Result<(), String> {
    eprintln!("{DIM}skein:{RESET} starting {name}'s old sandbox to read what it kept…");
    let carried = skein::recover_ignored(name)?;
    let n: usize = carried.trim().parse().unwrap_or(0);
    if n == 0 {
        println!("{BOLD}nothing to recover{RESET} for {CYAN}{name}{RESET} — it had no ignored files worth carrying");
    } else {
        println!("{BOLD}recovered{RESET} {n} ignored path(s) into {CYAN}{name}{RESET}");
        println!("  {DIM}files the box already had were left alone — its own copies are the newer ones{RESET}");
    }
    println!("\n{DIM}its old sandbox has been stopped again.{RESET}");
    Ok(())
}

fn cmd_attach(name: &str, opts: &[String]) -> Result<(), String> {
    // Reconnect to the box's existing agent session (same command the web cockpit uses).
    let configured = skein::agent_for_box(name);
    let agent = flag(opts, "--agent").unwrap_or(configured.clone());
    if !skein::valid_runtime(&agent) {
        let available = skein::supported_runtimes()
            .iter()
            .map(|runtime| runtime.id)
            .collect::<Vec<_>>()
            .join(", ");
        return Err(format!(
            "unsupported runtime {agent:?}; available: {available}"
        ));
    }
    let handoff = opts.iter().any(|o| o == "--handoff");
    let attach_name = if handoff && configured != agent {
        eprintln!(
            "{DIM}skein:{RESET} snapshotting {name} and creating a lightweight {agent} replacement…"
        );
        let replacement = skein::replace_box(name, &agent)?;
        eprintln!(
            "{DIM}skein:{RESET} source preserved; replacement is {}",
            replacement.target
        );
        replacement.target
    } else {
        if handoff {
            let path = skein::prepare_handoff(name, None, &agent)?;
            eprintln!("{DIM}skein:{RESET} handoff prepared at {}", path.display());
        }
        name.to_string()
    };
    // A fleet box loses its tmux server whenever its sandbox cycles; the tree, the private HOME and
    // the cgroup survive. Restart the session before addressing its namespace, or the first thing
    // the user sees is `nsenter: cannot open /proc/<pid>/ns/user`.
    skein::ensure_box_session(&attach_name)?;
    let dir = skein::lookup_dir(&attach_name).unwrap_or_default();
    let argv = skein::attach_argv_as(&attach_name, &dir, &agent);
    match Command::new("sbx").args(&argv).status() {
        Ok(s) if s.success() => Ok(()),
        Ok(_) => Err("sbx exited non-zero".into()),
        Err(e) if e.kind() == ErrorKind::NotFound => {
            Err("sbx not found on PATH — attach needs the sbx CLI (host only)".into())
        }
        Err(e) => Err(format!("running sbx: {e}")),
    }
}
