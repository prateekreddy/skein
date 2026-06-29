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

fn cmd_ls() -> Result<(), String> {
    let (boxes, path) = load_registry()?;
    if boxes.is_empty() {
        println!("{DIM}the skein is empty — launch a box with: setup-sandbox.sh <branch>{RESET}");
        return Ok(());
    }

    // (tier, name, state, branch, age, dir)
    let mut rows: Vec<(u8, String, String, String, String, String)> = boxes
        .iter()
        .map(|(name, b)| {
            let (st, tier) = b.state();
            (
                tier,
                name.clone(),
                st,
                b.branch.clone(),
                b.age(),
                skein::shorten(&b.dir),
            )
        })
        .collect();
    rows.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));

    let w_name = rows.iter().map(|r| r.1.len()).fold("BOX".len(), usize::max);
    let w_st = rows
        .iter()
        .map(|r| r.2.len())
        .fold("STATE".len(), usize::max);
    let w_br = rows
        .iter()
        .map(|r| r.3.len())
        .fold("BRANCH".len(), usize::max);
    let w_age = rows
        .iter()
        .map(|r| r.4.len())
        .fold("SEEN".len(), usize::max);

    println!(
        "{BOLD}  {}  {}  {}  {}  DIR{RESET}",
        pad("BOX", w_name),
        pad("STATE", w_st),
        pad("BRANCH", w_br),
        pad("SEEN", w_age),
    );

    for (tier, name, st, br, age, dir) in &rows {
        let name_cell = if *tier >= 4 {
            format!("{DIM}{name}{RESET}")
        } else {
            format!("{BOLD}{name}{RESET}")
        };
        println!(
            "{} {}  {}  {}  {}  {DIM}{dir}{RESET}",
            dot(*tier),
            pad_colored(&name_cell, name.len(), w_name),
            pad(st, w_st),
            pad_colored(&format!("{CYAN}{br}{RESET}"), br.len(), w_br),
            pad(age, w_age),
        );
    }

    println!("\n{DIM}{} boxes · {}{RESET}", boxes.len(), path.display());
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
    Ok(())
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
