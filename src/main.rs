//! skein — see and steer your fleet of agent sandboxes.
//!
//! v0 bootstrap: a small status view over the shared sbx registry, useful from day one.
//! It deliberately does NOT reinvent heavy machinery — the roadmap composes existing
//! tools (honker for the bus/queue, ratatui for the live TUI, gh for PRs, sbx for
//! isolation). See ARCHITECTURE.md.

use chrono::{DateTime, Utc};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::io::ErrorKind;
use std::path::PathBuf;
use std::process::Command;

const VERSION: &str = "0.1.0";

// ANSI styling (the only thing we hand-roll here; the real TUI uses ratatui/crossterm).
const RESET: &str = "\x1b[0m";
const BOLD: &str = "\x1b[1m";
const DIM: &str = "\x1b[2m";
const CYAN: &str = "\x1b[36m";

/// One entry in the shared `sandboxes.json` registry written by sandbox-bootstrap.sh.
#[derive(Debug, Default, Deserialize)]
struct Sandbox {
    #[serde(default)]
    branch: String,
    #[serde(default)]
    dir: String,
    #[serde(default, rename = "lastSeen")]
    last_seen: String,
    /// Set by the box status hook (roadmap phase 2); usually empty today.
    #[serde(default)]
    status: String,
}

impl Sandbox {
    /// Human label + sort/colour tier: 0 live, 1 idle, 2 stale.
    /// Prefers an explicit status; falls back to liveness derived from `lastSeen`.
    fn state(&self) -> (String, u8) {
        match self.status.as_str() {
            "working" | "running" | "live" => return (self.status.clone(), 0),
            "waiting" | "needs-input" | "blocked" => return (self.status.clone(), 1),
            "" => {} // derive from lastSeen below
            other => return (other.to_string(), 1),
        }
        match self.age_secs() {
            Some(s) if s < 120 => ("live".into(), 0),
            Some(s) if s < 1800 => ("idle".into(), 1),
            Some(_) => ("stale".into(), 2),
            None => ("unknown".into(), 2),
        }
    }

    fn age_secs(&self) -> Option<i64> {
        let t = DateTime::parse_from_rfc3339(&self.last_seen).ok()?;
        Some((Utc::now() - t.with_timezone(&Utc)).num_seconds())
    }

    fn age(&self) -> String {
        match self.age_secs() {
            None => "?".into(),
            Some(s) if s < 60 => format!("{s}s ago"),
            Some(s) if s < 3600 => format!("{}m ago", s / 60),
            Some(s) if s < 86400 => format!("{}h ago", s / 3600),
            Some(s) => format!("{}d ago", s / 86400),
        }
    }
}

fn main() {
    let args: Vec<String> = env::args().skip(1).collect();
    let cmd = args.first().map(String::as_str).unwrap_or("ls");
    let rest: &[String] = if args.len() > 1 { &args[1..] } else { &[] };

    let result = match cmd {
        "ls" | "status" => cmd_ls(),
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
skein attach <box>    reconnect to a box (runs: sbx run --name <box>)\n  \
skein version\n  \
skein help\n\n\
registry resolution (first match wins):\n  \
$SKEIN_REGISTRY                 full path to sandboxes.json\n  \
$SKEIN_SHARED/sandboxes.json\n  \
<git-toplevel>/../skein-shared/.claude/sandboxes.json\n"
    );
}

// ---- registry (reads the existing shared store; does not own it) ----

fn locate_registry() -> Result<PathBuf, String> {
    if let Ok(p) = env::var("SKEIN_REGISTRY") {
        if !p.is_empty() {
            return Ok(PathBuf::from(p));
        }
    }
    if let Ok(p) = env::var("SKEIN_SHARED") {
        if !p.is_empty() {
            return Ok(PathBuf::from(p).join("sandboxes.json"));
        }
    }
    if let Ok(out) = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .output()
    {
        if out.status.success() {
            let top = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if let Some(parent) = PathBuf::from(&top).parent() {
                return Ok(parent
                    .join("skein-shared")
                    .join(".claude")
                    .join("sandboxes.json"));
            }
        }
    }
    Err("can't locate sandboxes.json — set $SKEIN_REGISTRY or $SKEIN_SHARED".into())
}

fn load_registry() -> Result<(BTreeMap<String, Sandbox>, PathBuf), String> {
    let path = locate_registry()?;
    let data = fs::read_to_string(&path).map_err(|e| format!("reading {}: {e}", path.display()))?;
    let boxes: BTreeMap<String, Sandbox> =
        serde_json::from_str(&data).map_err(|e| format!("parsing {}: {e}", path.display()))?;
    Ok((boxes, path))
}

// ---- ls ----

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
            (tier, name.clone(), st, b.branch.clone(), b.age(), shorten(&b.dir))
        })
        .collect();
    // live first, then alphabetical — the "who needs me?" ordering.
    rows.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));

    let w_name = rows.iter().map(|r| r.1.len()).fold("BOX".len(), usize::max);
    let w_st = rows.iter().map(|r| r.2.len()).fold("STATE".len(), usize::max);
    let w_br = rows.iter().map(|r| r.3.len()).fold("BRANCH".len(), usize::max);
    let w_age = rows.iter().map(|r| r.4.len()).fold("SEEN".len(), usize::max);

    println!(
        "{BOLD}  {}  {}  {}  {}  {}{RESET}",
        pad("BOX", w_name),
        pad("STATE", w_st),
        pad("BRANCH", w_br),
        pad("SEEN", w_age),
        "DIR"
    );

    for (tier, name, st, br, age, dir) in &rows {
        let name_cell = if *tier == 2 {
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
        0 => "\x1b[32m●\x1b[0m",
        1 => "\x1b[33m●\x1b[0m",
        _ => "\x1b[2m○\x1b[0m",
    }
}

fn pad(s: &str, w: usize) -> String {
    if s.len() >= w {
        s.to_string()
    } else {
        format!("{s}{}", " ".repeat(w - s.len()))
    }
}

/// Pad a string that already contains ANSI codes, using its VISIBLE length.
fn pad_colored(s: &str, visible: usize, w: usize) -> String {
    if visible >= w {
        s.to_string()
    } else {
        format!("{s}{}", " ".repeat(w - visible))
    }
}

fn shorten(p: &str) -> String {
    if let Ok(home) = env::var("HOME") {
        if !home.is_empty() && p.starts_with(&home) {
            return format!("~{}", &p[home.len()..]);
        }
    }
    p.to_string()
}

// ---- attach (delegates to sbx; does not reimplement the session) ----

fn cmd_attach(name: &str) -> Result<(), String> {
    match Command::new("sbx").args(["run", "--name", name]).status() {
        Ok(s) if s.success() => Ok(()),
        Ok(_) => Err("sbx exited non-zero".into()),
        Err(e) if e.kind() == ErrorKind::NotFound => {
            Err("sbx not found on PATH — attach needs the sbx CLI (host only)".into())
        }
        Err(e) => Err(format!("running sbx: {e}")),
    }
}
