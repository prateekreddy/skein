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
    // win; a malformed file is reported, not silently half-applied). See skein::util::load_dotenv.
    skein::util::load_dotenv();
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
            None => Err(
                "usage: skein start <box> [--branch <branch>] [--agent <runtime>] [--attach]"
                    .into(),
            ),
        },
        "login" => cmd_login(rest.first().map(String::as_str)),
        "resize" => {
            // `--disk` rather than a third positional: disk is the one of the three that is usually
            // changed alone, and `skein resize 26g "" 60g` is a trap worth not building.
            let disk = flag(rest, "--disk").unwrap_or_default();
            // A bare switch, not a value: what it means is "I have accepted the loss", and a flag
            // that takes an argument invites `--drop-docker false` meaning the opposite.
            let drop_docker = rest.iter().any(|a| a == "--drop-docker");
            let positional: Vec<&String> = rest
                .iter()
                .take_while(|a| !a.starts_with("--"))
                .collect::<Vec<_>>();
            match (positional.first(), disk.is_empty()) {
                (Some(memory), _) => cmd_resize(
                    memory,
                    positional.get(1).map(|s| s.as_str()).unwrap_or(""),
                    &disk,
                    drop_docker,
                ),
                // Disk alone still needs a memory size to rebuild at, and the configured one is the
                // right answer — nobody asking for a bigger disk is also asking to be re-sized.
                (None, false) => cmd_resize(
                    &skein::config::load_config().fleet_memory,
                    "",
                    &disk,
                    drop_docker,
                ),
                (None, true) => Err("usage: skein resize <memory> [cpus] [--disk <size>] \
                     [--drop-docker]   \
                     e.g. skein resize 26g   |   skein resize --disk 60g"
                    .into()),
            }
        }
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
                      (--disk <size> for the shared 20G filesystem; sbx fixes it at creation)\n  \
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
    let repo = skein::repos::add_repo(source, id.as_deref(), agent.as_deref(), store.as_deref())?;
    println!("{BOLD}added{RESET} {CYAN}{}{RESET}", repo.id);
    println!("  {DIM}source{RESET}  {}", repo.source);
    println!("  {DIM}work  {RESET}  {}", repo.work);
    println!("  {DIM}store {RESET}  {}", repo.store);
    if let Some(w) = skein::repos::remote_warning(&repo.work) {
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
    let repo = skein::repos::remove_repo(id)?;
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
    let repos = skein::repos::load_repos();
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
    if let Err(e) = skein::mailbox::relay_cross_project_mail() {
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
    // The *openable* URL, not just the address it binds. The address alone is the one thing that
    // does not work: the API needs the fleet's token, so a bare `http://127.0.0.1:7878` loads a page
    // whose every request is refused. A diagnostic that prints the address and stops is handing over
    // the broken half of the answer.
    match skein::apiauth::disabled() {
        true => println!(
            "{DIM}·{RESET} cockpit       http://{addr}  {DIM}(auth off — \
             $SKEIN_NO_API_AUTH){RESET}"
        ),
        false => match skein::apiauth::stored() {
            Some(t) => println!("{OK} cockpit       http://{addr}/?t={t}"),
            None => println!(
                "{DIM}·{RESET} cockpit       http://{addr}/?t=…  {DIM}(the token is minted at the \
                 server's first start){RESET}"
            ),
        },
    }

    for (prog, why) in [
        ("sbx", "attach + launch boxes"),
        ("git", "host-side diffs"),
        // curl, not gh: skein reads GitHub over the API with a token it already holds. `gh` was a
        // hard requirement of the review queue and is now not used at all.
        ("curl", "reading GitHub (PRs, diffs, merges)"),
    ] {
        if have(prog) {
            println!("{OK} {prog:<13} on PATH  {DIM}{why}{RESET}");
        } else {
            println!("{BAD} {prog:<13} not on PATH — {why} unavailable");
        }
    }
    {
        // Which credential the host's own GitHub calls run on. It matters because only one of the
        // three can reach the system keyring, and that is the one people were being pushed onto.
        use skein::prq::GhToken;
        match skein::prq::host_token_source() {
            GhToken::Environment => {
                println!(
                    "{OK} gh token      {DIM}$GH_TOKEN — nothing is read, nothing prompts{RESET}"
                )
            }
            GhToken::WritePat => println!(
                "{OK} github token  {DIM}a repository write token you stored. A read token in \
                 Settings would widen what the queue can see{RESET}"
            ),
            GhToken::ReadToken => {
                println!("{OK} github token  {DIM}the read token in Settings{RESET}")
            }
            GhToken::None => println!(
                "{WARN} github token  none — the review queue reads PRs as you, and nothing here \
                 names a user.\n              {DIM}export GH_TOKEN, or add a read token in \
                 Settings → GitHub & keys. A GitHub App cannot do this one: an installation token \
                 is not a person{RESET}"
            ),
        }
    }

    // Which GitHub credential a box actually gets. Worth a line of its own because when this is
    // wrong there is no symptom until a push comes back 403 inside a box, minutes later — and the
    // reason it is wrong (an App ID GitHub rejects, a key for a different App, an App installed on
    // none of these repos) is known here and was previously only ever printed to a detached
    // server's stderr.
    {
        let g = skein::health::health_report_gitgate();
        let mark = if g.ok { OK } else { BAD };
        println!("{mark} git scope     {}", g.detail);
    }

    // skein-managed repos + its own kit (the repo-agnostic path).
    let repos = skein::repos::load_repos();
    println!(
        "{} repos         {DIM}{} managed{RESET}",
        if repos.is_empty() { WARN } else { OK },
        repos.len()
    );
    // The review queue, per repo, and what it is looking at. Its own line because the badge is the
    // only place this surfaces in the cockpit, and a badge cannot say "I did not look" — a repo whose
    // queue is off, or that resolves to no GitHub repository, produced exactly the empty badge that a
    // clean queue produces. Spends `gh` calls, so it is a thing you run rather than a poll.
    for count in skein::prq::counts() {
        let id = &count.repo_id;
        if !count.skipped.is_empty() {
            println!(
                "{DIM}·{RESET} review        {id}: not looked at — {}",
                count.skipped
            );
        } else if !count.error.is_empty() {
            println!("{BAD} review        {id}: {}", count.error);
        } else {
            println!("{OK} review        {id}: {} need you", count.needs_you);
        }
    }
    let kit = skein::config::skein_home().join("kit").join("spec.yaml");
    if kit.exists() {
        println!("{OK} kit           {DIM}{}{RESET}", kit.display());
    } else {
        println!("{WARN} kit           {DIM}not written yet (server startup / `skein add` installs it){RESET}");
    }
    let cfg = skein::config::load_config();
    // Before the settings line, because it invalidates everything on it. A config skein cannot
    // parse is thrown away whole, so every value below is a default it fell back to rather than
    // anything anyone chose — and a default is indistinguishable from a choice on sight.
    if let Some(why) = skein::config::config_error() {
        println!("{BAD} settings      unreadable — {why}");
        println!(
            "{DIM}                every setting below is a fallback default, not your choice; \
             skein has not overwritten the file{RESET}"
        );
    }
    println!(
        "{DIM}·{RESET} settings      gh-seed:{} ssh-key:{} {DIM}({}){RESET}",
        on_off(cfg.seed_gh_secret),
        if cfg.ssh_key.is_empty() { "—" } else { "set" },
        // A file that is not there is not a fault: skein writes one the first time something is
        // saved, and every setting is at this build's default until then. Naming a path that does
        // not exist reads as "go and look at it", which sends someone after a file to explain
        // behaviour the file has no part in.
        match skein::config::config_path_if_written() {
            Some(path) => path,
            None => "no config.json yet — every setting is this build's default".into(),
        }
    );
    // Which of the three credential paths this fleet is on. First, because "can a box push" is the
    // question every other GitHub line here is a detail of — and because all three are opt-in, so
    // "none" is a state a fresh fleet really sits in rather than a fault to hunt.
    match skein::gitgate::box_credential() {
        skein::gitgate::BoxCredential::None => {
            println!(
                "{WARN} boxes push    nothing chosen — boxes read public repos and cannot push"
            );
            println!("              {DIM}Settings → GitHub & keys: a GitHub App, a per-repo token, or this account's gh token{RESET}");
        }
        other => println!("{OK} boxes push    with {}", other.label()),
    }
    // Whether startup will reach for `gh auth token` — which on a keyring-backed `gh` is a password
    // dialog. Reported because the fix for that dialog is to *skip* the call, and a silent skip is
    // indistinguishable from a broken seed until a box fails to push.
    if cfg.seed_gh_secret {
        match skein::repos::gh_secret_seeded() {
            Some(when) => println!(
                "{OK} gh secret     seeded {when} {DIM}— startup skips `gh auth token`, so no \
                 keyring is unlocked. Settings → Overwrite token on startup re-seeds{RESET}"
            ),
            None => println!(
                "{DIM}·{RESET} gh secret     not seeded yet {DIM}— the next server start runs `gh \
                 auth token`, which asks to unlock your keyring if `gh` stores its token there{RESET}"
            ),
        }
    }
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
        // Not "off — a sandbox per box": that model is gone, and saying it here would name a
        // fallback that no longer exists for a fleet that cannot start anything at all.
        println!(
            "\n{BAD} fleet         no sandbox named (fleet_sandbox is empty) — no box can start \
             until one is set"
        );
    } else {
        println!("\n{BOLD}fleet{RESET} {DIM}({fleet}){RESET}");
        match skein::fleet::fleet_exists(&fleet) {
            Some(true) => println!("{OK} sandbox       up"),
            Some(false) => println!(
                "{WARN} sandbox       not created yet {DIM}(the next launch creates it){RESET}"
            ),
            None => println!(
                "{BAD} sandbox       cannot tell if it exists — {}",
                skein::fleet_failure().unwrap_or_else(|| "sbx did not answer".into())
            ),
        }
        let place = skein::place::own_sandbox(&fleet);
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
                "{OK} ceilings      cgroup delegation works {DIM}(per box: {}){RESET}",
                skein::fleet::box_limits()
            );
        } else {
            println!("{BAD} ceilings      no cgroup delegation — boxes run UNCAPPED, so one runaway build can kill every other box");
        }
        // What actually bounds the sandbox, and read back from the kernel rather than reported from
        // config: a per-box cap can never bound their sum, and it does not reach inside the Docker
        // daemon they share, where a box's `docker build` really runs. A ceiling skein computed and
        // failed to write looks identical from the host until the sandbox stops answering.
        for (cgroup, what) in [
            ("skein", "all boxes together"),
            ("docker", "the sandbox's Docker daemon"),
        ] {
            let live = probe(&format!(
                "cat /sys/fs/cgroup/{cgroup}/memory.max 2>/dev/null"
            ));
            match live.as_str() {
                "" => println!("{DIM}·{RESET} {cgroup:<13} no such cgroup {DIM}({what}){RESET}"),
                "max" => println!(
                    "{BAD} {cgroup:<13} UNBOUNDED — {what} can reach the VM's memory, and with no \
                     swap that ends the sandbox rather than the build"
                ),
                bytes => {
                    let gib = bytes
                        .parse::<u64>()
                        .map(|b| format!("{:.1}G", b as f64 / 1024.0 / 1024.0 / 1024.0))
                        .unwrap_or_else(|_| bytes.to_string());
                    println!("{OK} {cgroup:<13} capped at {gib} {DIM}({what}){RESET}");
                }
            }
        }

        // How host↔sandbox calls actually travel, asked rather than assumed. Every degradation
        // here is silent by design — falling back to `sbx exec` is what skein did before the agent
        // existed, so the fleet keeps working and only its resilience is gone. That makes doctor
        // the only place it can be seen.
        let t = skein::fleet::transport_state();
        let at = |p: u16| {
            if p == 0 {
                "no port published yet".to_string()
            } else {
                format!("port {p}")
            }
        };
        match t {
            _ if !t.configured => println!(
                "{DIM}·{RESET} transport     {DIM}`sbx exec` — this fleet switched the in-sandbox \
                 agent off (\"fleet_agent\": false){RESET}"
            ),
            _ if t.speaks == 0 => println!(
                "{BAD} transport     agent wanted but nothing answers ({}) — every call falls back \
                 to `sbx exec`, so a stalled daemon stalls the board",
                at(t.port)
            ),
            _ if t.speaks < t.wants => println!(
                "{WARN} transport     agent v{} on {}, this build needs v{} — the calls it does not \
                 know fall back to `sbx exec`",
                t.speaks,
                at(t.port),
                t.wants
            ),
            _ => println!(
                "{OK} transport     agent v{} on {} {DIM}(calls survive a stalled daemon){RESET}",
                t.speaks,
                at(t.port)
            ),
        }
        // A mount that is missing produces a box with no store, which looks entirely healthy.
        for path in skein::fleet::fleet_mounts() {
            let seen = probe(&format!("test -d '{path}' && echo yes")) == "yes";
            println!(
                "{} mount         {DIM}{path}{RESET}{}",
                if seen { OK } else { BAD },
                if seen {
                    String::new()
                } else {
                    format!(
                        " — not visible in the sandbox; boxes for it would come up with no store. \
                         `skein resize {}` rebuilds it with this mount and carries every box across",
                        match cfg.fleet_memory.trim() {
                            "" => "26g",
                            size => size,
                        }
                    )
                }
            );
        }
    }

    // The sbx-dependent facts skein can't verify itself — surface them so they're not silent.
    let runtimes = skein::runtime::supported_runtimes()
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
    let out = start_the_box(name, opts);
    // The cockpit runs this in a PTY that closes when it returns, and the browser then reconnects
    // into a fresh terminal holding none of the output. Keep the reason where that reconnect will
    // read it — including the failures that happen before `start_box` is reached at all, which is
    // where "no registered repo for box X" lives.
    if let Err(why) = &out {
        skein::fleet::remember_start_failure(name, why);
    }
    out
}

fn start_the_box(name: &str, opts: &[String]) -> Result<(), String> {
    let repo = skein::repos::repo_for_box(name)
        .ok_or_else(|| format!("no registered repo for box {name} — `skein repos` to check"))?;
    let branch =
        flag(opts, "--branch").unwrap_or_else(|| skein::repos::branch_of(name).unwrap_or_default());
    if branch.trim().is_empty() {
        return Err(format!("no branch for box {name}; pass --branch <branch>"));
    }
    let agent = flag(opts, "--agent").unwrap_or_else(|| skein::repos::agent_for_box(name));
    if !skein::runtime::valid_runtime(&agent) {
        return Err(format!("unsupported runtime {agent:?}"));
    }
    eprintln!("{DIM}skein:{RESET} starting {name} in the shared sandbox…");
    // The box's own persistent shell, not its agent. `skein attach` starts the runtime — with the
    // full setup it does for every box — into this same tmux server, so the fleet path does not get
    // its own second way of launching an agent to keep in step with the first.
    skein::fleet::start_box(name, &repo, &branch, "exec bash -l")?;
    eprintln!("{DIM}skein:{RESET} {name} is up on {branch}");
    // `--attach` exists so the cockpit's create-a-box terminal can hand off into the agent without
    // the caller having to name the box's placement — which does not exist until the line above has
    // run. Resolved here, after the box is real.
    if opts.iter().any(|o| o == "--attach") {
        return run_sbx(&skein::sandbox::initial_attach_argv_as(name, &agent));
    }
    Ok(())
}

/// Run `sbx` with an already-built argv, reporting the one failure worth naming.
fn run_sbx(argv: &[String]) -> Result<(), String> {
    match Command::new("sbx").args(argv).status() {
        Ok(s) if s.success() => Ok(()),
        Ok(_) => Err("sbx exited non-zero".into()),
        Err(e) if e.kind() == ErrorKind::NotFound => {
            Err("sbx not found on PATH — attach needs the sbx CLI (host only)".into())
        }
        Err(e) => Err(format!("running sbx: {e}")),
    }
}

/// `skein resize <memory> [cpus]` — rebuild the shared sandbox at a new size.
///
/// A CLI command and not only a cockpit button because this is the one operation that destroys the
/// sandbox: `sbx create` may ask for confirmation, and a server has no terminal to answer with — so
/// the riskiest path needs to be runnable somewhere a person is sitting.
fn cmd_resize(memory: &str, cpus: &str, disk: &str, drop_docker: bool) -> Result<(), String> {
    let size = match disk.trim() {
        "" => memory.to_string(),
        d => format!("{memory}, disk {d}"),
    };
    eprintln!("{DIM}skein:{RESET} saving every box's work, then rebuilding the sandbox at {size}…");
    if drop_docker {
        eprintln!(
            "{DIM}skein:{RESET} --drop-docker: /var/lib/docker goes with the sandbox, images and \
             volumes included"
        );
    }
    let failed = skein::fleet::resize_fleet(memory, cpus, disk, drop_docker)?;
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
    if !skein::runtime::valid_runtime(runtime) {
        return Err(format!("unsupported runtime {runtime:?}"));
    }
    if runtime == "claude" {
        eprintln!("{DIM}skein:{RESET} type {CYAN}/login{RESET} once it starts, then {CYAN}/exit{RESET} — `setup-token` returns a token to export and leaves no credential to seed boxes with");
    }
    skein::fleet::fleet_login(runtime)?;
    eprintln!(
        "{DIM}skein:{RESET} every new box now inherits this login; running boxes pick it up when their session next starts"
    );
    Ok(())
}

fn cmd_attach(name: &str, opts: &[String]) -> Result<(), String> {
    // Reconnect to the box's existing agent session (same command the web cockpit uses).
    let configured = skein::repos::agent_for_box(name);
    let agent = flag(opts, "--agent").unwrap_or(configured.clone());
    if !skein::runtime::valid_runtime(&agent) {
        let available = skein::runtime::supported_runtimes()
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
    skein::fleet::ensure_box_session(&attach_name)?;
    let dir = skein::lookup_dir(&attach_name).unwrap_or_default();
    run_sbx(&skein::sandbox::attach_argv_as(&attach_name, &dir, &agent))
}
