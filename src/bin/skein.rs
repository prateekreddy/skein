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
            None => Err("usage: skein add <git-url|path> [--id <id>] [--agent <claude>]".into()),
        },
        "repos" => cmd_repos(),
        "remove" | "rm" => match rest.first() {
            Some(id) => cmd_remove(id),
            None => Err("usage: skein remove <repo-id>".into()),
        },
        "doctor" => cmd_doctor(),
        "attach" => match rest.first() {
            Some(name) => cmd_attach(name),
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
skein attach <box>    reconnect to the box's running agent session\n  \
skein doctor          check registry + required tools (sbx/git/gh)\n  \
skein version\n  \
skein help\n\n\
the web cockpit lives in `skein-server` (run it, open http://127.0.0.1:7878).\n\n\
registry resolution (first match wins):\n  \
$SKEIN_REGISTRY                 full path to sandboxes.json\n  \
$SKEIN_SHARED/sandboxes.json\n  \
<git-toplevel>/../skein-shared/.claude/sandboxes.json\n"
    );
}

/// `skein add <git-url|path> [--id <id>] [--agent <claude>]` — register a repo so skein can launch
/// + observe boxes for it with zero repo-side setup.
fn cmd_add(source: &str, opts: &[String]) -> Result<(), String> {
    let id = flag(opts, "--id");
    let agent = flag(opts, "--agent");
    let repo = skein::add_repo(source, id.as_deref(), agent.as_deref())?;
    println!("{BOLD}added{RESET} {CYAN}{}{RESET}", repo.id);
    println!("  {DIM}source{RESET}  {}", repo.source);
    println!("  {DIM}work  {RESET}  {}", repo.work);
    println!("  {DIM}store {RESET}  {}", repo.store);
    if let Some(w) = skein::ssh_remote_warning(&repo.work) {
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
    // Same sbx-sourced, "who-needs-me-first"-sorted fleet the web cockpit shows (sbx ∪ registry).
    let views = skein::load_views()?;
    if views.is_empty() {
        println!("{DIM}the skein is empty — add a repo (skein add <url|path>) then launch a box{RESET}");
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
        Err(e) => println!("{BAD} registry      {e}"),
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
        "{DIM}·{RESET} settings      tmux-install:{} gh-seed:{} ssh-key:{} {DIM}(~/.skein/config.json){RESET}",
        on_off(cfg.install_tmux),
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

    // The sbx-dependent facts skein can't verify itself — surface them so they're not silent.
    println!(
        "\n{DIM}host notes:{RESET}\n  {DIM}· reconnect = `sbx run --name <box> -- --continue` (resumes the agent's last session so it shows\n    up). sbx has no live-process attach, so this resumes the transcript in a fresh session.\n  · the shell tab uses a persistent tmux session when tmux is present, else a plain shell;\n    skein auto-installs tmux unless disabled in settings.\n  · push from a box: HTTPS uses the proxy's creds + the seeded gh token (no setup); SSH remotes use\n    your forwarded host SSH agent (works iff it has the key loaded), per sbx's credentials docs.{RESET}"
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

fn cmd_attach(name: &str) -> Result<(), String> {
    // Reconnect to the box's existing agent session (same command the web cockpit uses).
    let dir = skein::lookup_dir(name).unwrap_or_default();
    let argv = skein::attach_argv(name, &dir);
    match Command::new("sbx").args(&argv).status() {
        Ok(s) if s.success() => Ok(()),
        Ok(_) => Err("sbx exited non-zero".into()),
        Err(e) if e.kind() == ErrorKind::NotFound => {
            Err("sbx not found on PATH — attach needs the sbx CLI (host only)".into())
        }
        Err(e) => Err(format!("running sbx: {e}")),
    }
}
