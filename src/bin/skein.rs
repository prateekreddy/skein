//! skein — CLI surface over the fleet (status table + attach).
//! The terminal-native client; `skein-server` is the web client. Both share `skein` (lib).

use skein::registry::load_registry;
use std::env;
use std::io::ErrorKind;
use std::process::Command;

// ANSI styling (the only thing we hand-roll; the web UI uses CSS).
const RESET: &str = "\x1b[0m";
const BOLD: &str = "\x1b[1m";
const DIM: &str = "\x1b[2m";
const CYAN: &str = "\x1b[36m";

fn main() {
    // **Ctrl-C has to reach the command, and this is the only `main` where that is true.**
    // `util::run_bounded` puts every child it spawns in a process group of its own so that a
    // deadline ends the work and not the shell in front of it (SKEIN-912) — which also takes that
    // child out of the terminal's foreground group, so the terminal stops delivering Ctrl-C to it.
    // This installs the handler that forwards it. It is here, at the top of the one binary a
    // person types at, and NOT inside the library: a signal disposition belongs to a process, and
    // `skein-server` links the same code and must not acquire one it never asked for.
    // `skein::util::forward_interrupts` carries the argument in full.
    //
    // First, before `ensure_volume` or anything else can spawn: a handler installed after the
    // spawn it is for is a handler that was not installed.
    skein::util::forward_interrupts();
    // **This is a real skein, whatever `$SKEIN_TEST` says.** A `skein` started by a test harness
    // inherits the marker from cargo's `[env]` table, and it should: `config::skein_home` and
    // `util::fleet_root` still have to refuse it an unpinned path (SKEIN-685). What it cannot do is
    // install a stand-in for its own crossings — a stand-in is a Rust closure, and the test that
    // would write one is in another process — so it says which side of `Place::spawning`'s guard it
    // is on instead (SKEIN-530). Held for the whole run.
    let _real = skein::place::seam::real_crossings();
    // Pick up a local .env so $SKEIN_REGISTRY etc. needn't be typed each run (real env vars still
    // win; a malformed file is reported, not silently half-applied). See skein::util::load_dotenv.
    skein::util::load_dotenv();
    let args: Vec<String> = env::args().skip(1).collect();
    let cmd = args.first().map(String::as_str).unwrap_or("ls");
    let rest: &[String] = if args.len() > 1 { &args[1..] } else { &[] };

    // Before anything reads or writes the volume. A volume this binary does not understand, or one
    // that was moved and left an environment variable behind, is a refusal naming the fix — never a
    // half-read, and never a second empty installation filling up beside the real one.
    //
    // Except for the two verbs that exist to *answer* a refusal. `repoint` is named by the one about
    // a volume opened where it was not written, and `migrate` is the way off a volume whatever is
    // wrong with it — so gating both behind the check would make the fix line unrunnable, which is
    // the failure `tests/fix_lines.rs` exists to stop one layer down. `migrate` reads the source's
    // real path rather than what it records, so it is correct on a volume that records the wrong one.
    if !matches!(cmd, "repoint" | "migrate") {
        if let Err(e) = skein::volume::ensure_volume() {
            eprintln!("{DIM}skein:{RESET} {e}");
            std::process::exit(1);
        }
    }

    let result = match cmd {
        "ls" | "status" => cmd_ls(),
        "add" => match rest.first() {
            Some(src) => cmd_add(src, &rest[1..]),
            None => Err("usage: skein add <git-url> [--id <id>] [--store <path>]".into()),
        },
        "repos" => cmd_repos(),
        "remove" | "rm" => match rest.first() {
            Some(id) => cmd_remove(id),
            None => Err("usage: skein remove <repo-id>".into()),
        },
        "doctor" => cmd_doctor(),
        "announce" => cmd_announce(),
        "migrate" => match rest.first() {
            Some(target) => skein::volume::migrate(target).map(|report| println!("{report}")),
            None => Err(
                "usage: skein migrate <directory>   (the volume moves there; nothing is \
                 deleted)"
                    .into(),
            ),
        },
        "shared" => cmd_shared(rest),
        "start" => match rest.first() {
            Some(name) => cmd_start(name, &rest[1..]),
            None => Err(
                "usage: skein start <box> [--branch <branch>] [--agent <runtime>] [--attach] \
                 [--uncovered]"
                    .into(),
            ),
        },
        "stop" => match rest.first() {
            Some(name) => cmd_stop(name),
            None => Err("usage: skein stop <box>   (the box's work stays on disk)".into()),
        },
        "restart" => match rest.first() {
            Some(name) => cmd_restart(name, &rest[1..]),
            None => Err(
                "usage: skein restart <box> [--branch <branch>] [--agent <runtime>] [--attach] \
                 [--uncovered]"
                    .into(),
            ),
        },
        "repoint" => skein::volume::repoint_here().map(|report| println!("{report}")),
        "pull" => cmd_pull(rest.first().map(String::as_str)),
        // Named for what it moves, not for the mechanism: what a person wants is a newer Claude,
        // and "which npm package, in which sandbox" is skein's problem (SKEIN-404).
        "update-agents" => cmd_update_agents(),
        "login" => cmd_login(rest.first().map(String::as_str)),
        // Every argument is a box name, and there are deliberately no flags: the one choice this
        // verb offers is *which* boxes, and a `--something` in that position is a typo worth
        // refusing rather than a switch worth inventing. `save_boxes` refuses anything that is not
        // a box, by name and against the disk.
        "save" => cmd_save(rest),
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
            None => Err(
                "usage: skein attach <box> [--agent <runtime>] [--handoff] [--uncovered]"
                    .to_string(),
            ),
        },
        "cockpit-stop" => cmd_cockpit_stop(&skein::place::fleet_sandbox()),
        "version" | "--version" | "-v" => {
            // Package version from the manifest (a hardcoded copy here had already drifted once),
            // revision from the build stamp — the package version alone is 0.1.0 forever and
            // cannot answer "which build is this".
            println!(
                "skein {} ({})",
                env!("CARGO_PKG_VERSION"),
                skein::health::BUILD_REVISION
            );
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
skein add <git-url>   register a repo by its git URL (skein clones a mirror)\n  \
skein repos           list registered repos\n  \
skein remove <id>     unregister a repo (files left on disk)\n  \
skein start <box>     bring a box up inside the shared sandbox (see fleet_sandbox)\n  \
skein stop <box>      end every process in the box; its checkout and branch stay\n  \
skein restart <box>   stop then start — rebuilds the box's isolation from this skein\n  \
skein pull [<repo>]   refresh the mirror boxes clone from (every repo if none named)\n  \
skein login <runtime> authenticate once in the shared sandbox; every box inherits it\n  \
skein update-agents    update the agent CLIs every box shares (they live in the sandbox,\n  \
                      not in a box, and a running box keeps its version until next session)\n  \
skein save [<box>…]   copy every box's work out of the sandbox onto the host, and say where\n  \
                      each one went (nothing is stopped or destroyed; name boxes for just those)\n  \
skein resize <mem>    rebuild the shared sandbox at a new size, carrying every box's work\n  \
                      (--disk <size> for the shared 20G filesystem; sbx fixes it at creation)\n  \
skein attach <box>    reconnect; optional: --agent <runtime> --handoff\n  \
skein cockpit-stop    stop the cockpit; its port stays held and every box keeps running\n  \
skein shared import <box> [--include <name> ...] [--apply]\n  \
                       inspect/import durable files from a box's private home\n  \
skein doctor          check registry, tools, and the shared sandbox if one is on\n  \
skein announce        tell the fleet if its disk has just crossed the line (nothing is\n  \
                      deleted; silent while the figure has not moved across it)\n  \
skein migrate <dir>   copy this installation onto another volume (nothing is deleted)\n  \
skein repoint         after moving a volume by hand: point what it records at where it now is\n  \
skein version\n  \
skein help\n\n\
the web cockpit lives in `skein-server` (run it, open http://127.0.0.1:7878).\n\n\
registry resolution (first match wins):\n  \
$SKEIN_REGISTRY                 full path to sandboxes.json\n  \
$SKEIN_SHARED/sandboxes.json\n  \
<git-toplevel>/../skein-shared/.claude/sandboxes.json\n"
    );
}

/// `skein add <git-url> [--id <id>] [--store <shared-data-folder>]` — register
/// a repo so skein can launch + observe boxes for it with zero repo-side setup. `--store` points the
/// repo at an existing shared `.claude` folder (e.g. thing's `skein-shared/.claude`) so its
/// memory/skills/mailbox/statusline are live across the repo's boxes; omit it to let skein manage one.
fn cmd_add(source: &str, opts: &[String]) -> Result<(), String> {
    let id = flag(opts, "--id");
    // Refused rather than ignored: a flag that is accepted and then does nothing tells the person
    // their choice was kept when it was not.
    if flag(opts, "--agent").is_some() {
        return Err(
            "a repo has no runtime of its own any more: a box runs the fleet's Default agent \
                    (Settings → Boxes), or the one you pick for it — `skein start <box> --agent \
                    <runtime>`"
                .into(),
        );
    }
    let store = flag(opts, "--store");
    let repo = skein::repos::add_repo(source, id.as_deref(), store.as_deref())?;
    println!("{BOLD}added{RESET} {CYAN}{}{RESET}", repo.id);
    println!("  {DIM}source{RESET}  {}", repo.source);
    println!(
        "  {DIM}mirror{RESET}  {}",
        skein::repos::mirror_path(&repo.id).display()
    );
    println!("  {DIM}store {RESET}  {}", repo.store);
    if let Some(w) = skein::repos::remote_warning(&repo) {
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
        let result = skein::sharedhome::import_shared_home(box_name, &selected)?;
        print!("{result}");
        return Ok(());
    }

    let inventory = skein::sharedhome::shared_home_inventory(box_name)?;
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
    // The mirror and the store are skein's; a source tree, if there is one, is the user's own
    // checkout and was never skein's to make or to delete.
    println!(
        "  {DIM}files left on disk — delete if you're sure:{RESET}\n    {}\n    {}",
        skein::repos::mirror_path(&repo.id).display(),
        repo.store
    );
    Ok(())
}

fn cmd_repos() -> Result<(), String> {
    let repos = skein::repos::load_repos();
    if repos.is_empty() {
        println!("{DIM}no repos yet — add one with: skein add <git-url>{RESET}");
        return Ok(());
    }
    for r in &repos {
        // A repo says where it came from, and that is now always a remote.
        println!("{BOLD}{CYAN}{}{RESET}\n  {}", r.id, r.source);
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
    let views = skein::board::load_views()?;
    if views.is_empty() {
        println!(
            "{DIM}the skein is empty — add a repo (skein add <git-url>) then launch a box{RESET}"
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
    // The build first: half of doctor's use is "did the restart pick up the fix", and every line
    // below is a claim made BY some build — unattributed, they were twice pinned on the wrong one.
    println!(
        "{BOLD}skein doctor{RESET} {DIM}build {}{RESET}\n",
        skein::health::BUILD_REVISION
    );

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
                skein::registry::registry_origin()
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
    //
    // **The URL carries the token even when `$SKEIN_NO_API_AUTH` is set** (SKEIN-962). Until then
    // this branched on `apiauth::disabled` and printed a bare address under the switch — which is
    // now the one URL that cannot work either way. The fleet's cockpit refuses the switch
    // (`apiauth::off_switch_refused`, architecture §9.4): a server the doorway started with it set
    // serves nothing but the refusal, and one started without it wants the token. Neither of them
    // opens on an address alone, so the switch no longer changes what to print — it adds a line
    // saying it will not do what its name says.
    match skein::apiauth::stored() {
        Some(t) => println!("{OK} cockpit       http://{addr}/?t={t}"),
        None => println!(
            "{DIM}·{RESET} cockpit       http://{addr}/?t=…  {DIM}(the token is minted at the \
             server's first start){RESET}"
        ),
    }
    // Read rather than obeyed: this is `skein`, not the cockpit, so what it can report is that the
    // switch is set in an environment the fleet's server is started from — not that any particular
    // server took it. `apiauth::switch_set` is the reading and `apiauth::disabled` is the decision,
    // and the two exist separately because of exactly this line. The sentence says what a server
    // started under it will do.
    if skein::apiauth::switch_set() {
        println!(
            "{WARN} api auth      $SKEIN_NO_API_AUTH is set, and the fleet's cockpit refuses it \
             — a server started with it serves that refusal and nothing else. Unset it where the \
             server is started, then restart the server."
        );
    }

    // **No host tools on this list** (SKEIN-576). `sbx` used to be here whenever skein was on the
    // host; in the fleet it is host-only and absent by design, and a doctor that reported it
    // missing would be handing somebody a fault they cannot clear — which is worse than silence,
    // because the next real fault on the list gets read the same way.
    for (prog, why) in [
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
    // The warden, which is not a tool on PATH and is checked here beside the ones that are — because
    // to somebody reading this list the question is the same: is the thing skein needs present.
    //
    // Information, not a verdict (SKEIN-1184): without a warden skein shows the command instead.
    {
        let w = skein::health::warden_report();
        let mark = match w.level {
            skein::health::Level::Satisfied => OK,
            skein::health::Level::Unsatisfied => BAD,
            skein::health::Level::Unknown => WARN,
        };
        println!(
            "{mark} {:<13} {DIM}{}{RESET}",
            skein::health::label("warden"),
            w.detail
        );
        if !w.fix.is_empty() {
            println!("{DIM}              {}{RESET}", w.fix);
        }
    }

    {
        // Can skein reach the model, and does anything actually want it?
        //
        // Both questions, because they came apart on a live fleet: `review_summaries` defaults ON
        // and `ai_enrichment` defaults off, so the health line — which reports only the second —
        // said "ai off" while every review summary was failing. Reported as "all summarization
        // fails", with no way anywhere to ask why.
        //
        // Here rather than in the health report because this SPAWNS something. `doctor` is a command
        // a person runs; the health endpoint is polled every fifteen seconds by every open board.
        let wanted = skein::ai::model_wanted();
        if wanted.is_empty() {
            println!(
                "{DIM}·{RESET} model         {DIM}nothing asks for it — Settings → Boxes turns on \
                 box summaries, and the review pane its own{RESET}"
            );
        } else {
            match skein::ai::model_reachable() {
                Ok(()) => println!(
                    "{OK} model         {DIM}{} — a test call answered here{RESET}",
                    wanted.join(" and ")
                ),
                Err(unread) => {
                    println!(
                        "{BAD} model         {} on, and {}",
                        wanted.join(" and "),
                        unread.say()
                    );
                    // **This is where the search path lives now** (SKEIN-384). The line above is
                    // the sentence a queue row can carry; the transport's own words — which carry
                    // the entire PATH skein had, ~300 characters of it — are printed here, where
                    // there is room, and `Unread::say` sends the reader to `skein doctor` for
                    // exactly this. `detail` answers only where something WAS left out, so it
                    // cannot contradict the line above it the way a fixed hint did: a sandbox that
                    // could not be reached used to be followed by advice about the model binary's
                    // PATH, which is the confusion `Unreachable` exists to end.
                    if let Some(detail) = unread.detail() {
                        println!("{DIM}              {detail}{RESET}");
                    }
                }
            }
        }
    }

    {
        // Has anything taken the temp directory the model CLI derives for itself?
        //
        // Beside the model line and after it, because it is the same failure asked one layer down:
        // the model line says whether a call answers, this says whether the *shared* path a call
        // would have used is somebody else's. Reported here rather than left to the CLI's own
        // message, which is a good message that lands on whoever happened to be typing — it was
        // met in the middle of a login that had otherwise worked (SKEIN-289).
        //
        // Here rather than in `health_report` for the same reason as the model line: it spawns.
        let s = skein::health::model_scratch_health();
        let mark = match s.level {
            skein::health::Level::Satisfied => OK,
            skein::health::Level::Unsatisfied => BAD,
            skein::health::Level::Unknown => WARN,
        };
        println!("{mark} model scratch {}", s.detail);
        // Printed and never run — the recipe is a delete in a shared /tmp skein does not own, which
        // is the thing the CLI's guard exists to stop.
        if !s.fix.is_empty() {
            println!("{DIM}              → {}{RESET}", s.fix);
        }
    }

    {
        // Which credential answers "who are you" — the one GitHub call that names no repository.
        // It matters because only one of the sources can reach the system keyring, and that is the
        // one people were being pushed onto.
        use skein::prq::GhToken;
        match skein::prq::host_token_source() {
            GhToken::Environment => {
                println!(
                    "{OK} github token  {DIM}$GH_TOKEN — nothing is read, nothing prompts{RESET}"
                )
            }
            GhToken::WritePat => println!(
                "{OK} github token  {DIM}a repository token you stored says who you are; each \
                 repository below uses its own token first{RESET}"
            ),
            GhToken::ReadToken => {
                println!("{OK} github token  {DIM}the read token in Settings{RESET}")
            }
            GhToken::GhCli => println!(
                "{OK} github token  {DIM}the host's `gh` login — asked once when skein starts, so \
                 a new login needs a restart{RESET}"
            ),
            GhToken::None => println!(
                "{WARN} github token  none — the review queue reads PRs as you, and nothing here \
                 names a user.\n              {DIM}Store a token on a repository's card under \
                 Settings → Repositories, add a read token under Settings → GitHub & keys → Your \
                 GitHub identity, export GH_TOKEN, or run `gh auth login` on this host. A GitHub \
                 App cannot do this one: an installation token is not a person{RESET}"
            ),
        }
        // Per repository, which credential its queue reads with and its verdicts, merges and
        // workflows act with (SKEIN-953) — the same answer Settings → GitHub & keys → Your GitHub
        // identity shows, from the same resolver.
        for repo in skein::repos::load_repos() {
            let Some(slug) = skein::prq::repo_slug(&repo) else {
                continue;
            };
            let reads = skein::prq::repo_token_source(&slug, skein::prq::Need::Read);
            let writes = skein::prq::repo_token_source(&slug, skein::prq::Need::Write);
            let named = |source: GhToken| match source {
                GhToken::WritePat => "its own stored token",
                GhToken::None => "nothing",
                other => other.label(),
            };
            let mark = if writes == GhToken::None { WARN } else { OK };
            println!(
                "{mark}   {slug}  {DIM}reads with {}, posts and merges with {}{RESET}",
                named(reads),
                named(writes),
            );
        }
    }

    // Which agent logins the fleet holds, three-valued per runtime. "Expired" gets its own word
    // because it used to be reported as signed in: on a fleet-wide logout every surface said so,
    // and the symptom read as "each box needs a login" instead of "the fleet's credential is dead".
    {
        use skein::fleet::LoginState;
        let logins = skein::fleet::runtime_logins();
        let said = logins
            .iter()
            .map(|l| match l.state {
                LoginState::Live => format!("{} signed in", l.runtime),
                LoginState::Expired { at_ms } => format!(
                    "{} expired {}",
                    l.runtime,
                    chrono::DateTime::<chrono::Utc>::from_timestamp_millis(at_ms)
                        .map(|t| t.format("%Y-%m-%d").to_string())
                        .unwrap_or_else(|| format!("{at_ms}ms"))
                ),
                LoginState::Absent => format!("{} none", l.runtime),
            })
            .collect::<Vec<_>>()
            .join(" · ");
        let expired = logins
            .iter()
            .filter(|l| matches!(l.state, LoginState::Expired { .. }))
            .map(|l| l.runtime)
            .collect::<Vec<_>>();
        if !expired.is_empty() {
            println!("{BAD} logins        {said}");
            println!(
                "{DIM}              → every box holds the same dead token, so each one only LOOKS \
                 like it wants its own sign-in; one `skein login {}` heals the whole fleet{RESET}",
                expired[0]
            );
        } else if logins.iter().any(|l| matches!(l.state, LoginState::Live)) {
            println!("{OK} logins        {DIM}{said} — every new box inherits this{RESET}");
        } else {
            // Not a fault: all three model-credential paths are opt-in and a fleet on API keys
            // never has a login here.
            println!(
                "{WARN} logins        none — `skein login <runtime>` signs the fleet in once \
                 {DIM}(unless the fleet runs on API keys){RESET}"
            );
        }
    }

    // Which GitHub credential a box actually gets. Worth a line of its own because when this is
    // wrong there is no symptom until a push comes back 403 inside a box, minutes later — and the
    // reason it is wrong (an App ID GitHub rejects, a key for a different App, an App installed on
    // none of these repos) is known here and was previously only ever printed to a detached
    // server's stderr.
    {
        let g = skein::health::health_report_gitgate();
        let mark = match g.level {
            skein::health::Level::Satisfied => OK,
            skein::health::Level::Unsatisfied => BAD,
            skein::health::Level::Unknown => WARN,
        };
        println!(
            "{mark} {:<13} {}",
            skein::health::label("gitgate"),
            g.detail
        );
        // The way out, on its own line and indented under the fault it clears. A diagnostic that
        // names a problem and not its remedy has handed over the half nobody can act on.
        if !g.fix.is_empty() {
            println!("{DIM}              → {}{RESET}", g.fix);
        }
    }

    // How long that credential has left (SKEIN-928). Directly under the scope line, because the two
    // are the same credential asked two questions — what it can reach, and whether it will still
    // reach it next month — and because this is the one a person runs `skein doctor` to find out
    // when a push has just started failing for no reason they can see.
    //
    // A command rather than the report, for the reason every line in this function is: it costs a
    // request to GitHub per credential, and a person typing `doctor` is asking for it. The cockpit
    // gets the same check through `health_report`, where a gate stops the poll paying for it.
    {
        let t = skein::health::token_expiry_health();
        let mark = match t.level {
            skein::health::Level::Satisfied => OK,
            skein::health::Level::Unsatisfied => BAD,
            skein::health::Level::Unknown => WARN,
        };
        println!(
            "{mark} {:<13} {}",
            skein::health::label("token_expiry"),
            t.detail
        );
        if !t.fix.is_empty() {
            println!("{DIM}              → {}{RESET}", t.fix);
        }
    }

    // Which boxes nothing bounds. Beside the fleet cgroups above and not folded into them, because
    // they answer different questions: those say what the sandbox as a whole is held to, this says
    // whether a given box is inside it. A box that never joined a cgroup is outside every number
    // printed above, and it is the box whose runaway build ends the sandbox.
    //
    // Free: the answer is in each box's placement record, which the board already reads. There is
    // nothing on the host to read instead — `limits.state` lives in the box's own root, inside the
    // sandbox — which is why nothing reported this until the launcher started saying it out loud.
    {
        let uncapped = skein::health::uncapped_boxes();
        if uncapped.is_empty() {
            println!("{OK} box ceilings  {DIM}every running box is inside the fleet's{RESET}");
        } else {
            println!(
                "{BAD} box ceilings  {} running outside the fleet's ceiling: {}",
                match uncapped.len() {
                    1 => "one box is".to_string(),
                    n => format!("{n} boxes are"),
                },
                uncapped.join(", ")
            );
            println!(
                "{DIM}              → a runaway build in one of these reaches the whole sandbox; \
                 `skein restart <box>` puts it under the current plan{RESET}"
            );
        }
    }

    // How full the fleet's filesystems are. Beside the ceilings above because it is the same
    // question about the other resource — except that nothing enforces this one: one filesystem
    // serves every box, so this is the only warning before a build dies half way through it.
    {
        let d = skein::health::disk_health();
        let mark = match d.level {
            skein::health::Level::Satisfied => OK,
            skein::health::Level::Unsatisfied => BAD,
            skein::health::Level::Unknown => WARN,
        };
        println!("{mark} {:<13} {}", skein::health::label("disk"), d.detail);
        if !d.fix.is_empty() {
            println!("{DIM}              → {}{RESET}", d.fix);
        }
    }

    // Which boxes are running under an older isolation. Its own line because there is no other way
    // to learn it: `box-session.sh` in the sandbox is refreshed at every start, so the copy on disk
    // describes the NEXT box and says nothing about the ones already up — and a box keeps the mount
    // namespace it was born with for as long as it lives. Found by looking at a box that reported
    // itself ordinary while listing every other box in the fleet.
    {
        let uncovered = skein::health::uncovered_boxes();
        let c = skein::health::cover_health(&uncovered);
        let mark = match c.level {
            skein::health::Level::Satisfied => OK,
            skein::health::Level::Unsatisfied => BAD,
            skein::health::Level::Unknown => WARN,
        };
        println!("{mark} {:<13} {}", skein::health::label("cover"), c.detail);
        if !c.fix.is_empty() {
            println!("{DIM}              → {}{RESET}", c.fix);
        }
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
    //
    // **One fault per cause, not one per repo.** Nine repos with no GitHub token produced nine
    // identical paragraphs — the same sentence about `GH_TOKEN` nine times, burying every other line
    // on the page in a wall a reader scrolls past. The rule is the codebase's own, and it is already
    // tested twice elsewhere: `a_missing_tool_is_one_fault_and_not_five` and
    // `a_repo_with_no_store_is_one_fault_and_not_nine`. This list had escaped it.
    let counts = skein::prq::counts();
    let mut by_error: std::collections::BTreeMap<String, Vec<String>> =
        std::collections::BTreeMap::new();
    for count in &counts {
        let id = count.repo_id.clone();
        if !count.skipped.is_empty() {
            println!(
                "{DIM}·{RESET} review        {id}: not looked at — {}",
                count.skipped
            );
        } else if !count.error.is_empty() {
            by_error.entry(count.error.clone()).or_default().push(id);
        } else {
            println!("{OK} review        {id}: {} need you", count.needs_you);
        }
    }
    for (why, repos) in &by_error {
        // The repos are named because which ones failed is a fact, and dropping it to save a line
        // would trade the wall for a mystery. It is the SENTENCE that is said once.
        println!(
            "{BAD} review        {}: {why}",
            match repos.as_slice() {
                [only] => only.clone(),
                many => format!("{} repos ({})", many.len(), many.join(", ")),
            }
        );
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
        "{DIM}·{RESET} settings      gh-seed:{} {DIM}({}){RESET}",
        on_off(cfg.seed_gh_secret),
        // A file that is not there is not a fault: skein writes one the first time something is
        // saved, and every setting is at this build's default until then. Naming a path that does
        // not exist reads as "go and look at it", which sends someone after a file to explain
        // behaviour the file has no part in.
        match skein::config::config_path_if_written() {
            Some(path) => path,
            None => "no config.json yet — every setting is this build's default".into(),
        }
    );
    // Which of the three credential paths this fleet is on. First, because "what does a box hold"
    // is the question every other GitHub line here is a detail of — and because all three are
    // opt-in, so "none" is a state a fresh fleet really sits in rather than a fault to hunt.
    //
    // Holds, not reaches: this line used to read "boxes read public repos and cannot push", which
    // is false — the sandbox proxy answers a request carrying no credential as the account
    // (SKEIN-548, open; `gitgate`'s module note has the measurement).
    match skein::gitgate::box_credential() {
        skein::gitgate::BoxCredential::None => {
            println!(
                "{WARN} boxes push    nothing chosen — no GitHub credential is placed in a box"
            );
            // Two of the three paths need nothing from the host; the third is the host's whole
            // keyring. Offering all three in the fleet would be offering one that cannot be taken
            // from there — and a person who tries it gets a refusal, from the one line that was
            // supposed to be their way out.
            {
                println!("              {DIM}Settings → GitHub & keys: a GitHub App, or a per-repo token. (The account token is seeded from the host's keyring, which is not reachable from inside the fleet.){RESET}");
            }
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
                 keyring is unlocked{RESET}"
            ),
            // What the *next start* will do. It refuses rather than reaching for `gh auth token`,
            // and predicting a keyring prompt that cannot happen sends somebody to look for a
            // dialog nothing shows. There is no skein anywhere that could seed one now, so scoping
            // is the whole of the answer — `docs/parity.md` §7 records that as the loss.
            None => println!(
                "{WARN} gh secret     not seeded, and nothing can seed one {DIM}— both halves were \
                 the host's: `gh auth token` reads its login, `sbx secret set` writes its keyring, \
                 and skein runs in the sandbox. Scope per repo instead — Settings → GitHub & \
                 keys{RESET}"
            ),
        }
    }
    if have("ssh-add") {
        let loaded = Command::new("ssh-add")
            .arg("-l")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        // Whose agent this is. `sbx` forwards the host's into the sandbox at create, and skein runs
        // in that sandbox — the forward is a property of the sandbox rather than of skein — so
        // `ssh-add -l` here lists the HOST's keys, and the fix for an empty list is on the host,
        // where the key file is. There is no key setting to point at: skein in the fleet cannot read
        // a key file on the host, so the field went (SKEIN-947) and `ssh-add` there is the whole fix.
        let whose = "the host's, forwarded into this sandbox";
        match loaded {
            true => println!("{OK} ssh agent     keys loaded {DIM}({whose}){RESET}"),
            false => println!(
                "{WARN} ssh agent     no keys loaded {DIM}(SSH git push from boxes will fail — run \
                 `ssh-add <key>` on the host; the key file is there and skein cannot read it from \
                 in here){RESET}"
            ),
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
        // **Unreachable, and kept for that reason.** `load_config` repairs a blank `fleet_sandbox`
        // to the default (SKEIN-484), so a fleet always has a name and `board::load_views` builds
        // the board from the placements unconditionally. If this ever prints, that repair has been
        // removed or bypassed and the board is showing an empty fleet as a fact — which is why the
        // line names the invariant rather than telling somebody to set a value they did not unset.
        println!(
            "\n{BAD} fleet         no sandbox named, which load_config is supposed to make \
             impossible — the board will show this fleet as empty"
        );
    } else {
        println!("\n{BOLD}fleet{RESET} {DIM}({fleet}){RESET}");
        match skein::fleet::fleet_exists(&fleet) {
            Some(true) => println!("{OK} sandbox       up"),
            // **There is no `Some(false)` arm, and there is no wording to write for one**
            // (SKEIN-777). `fleet_exists` is `(sandbox == fleet_sandbox()).then_some(true)`, and
            // `then_some` has only `Some(true)` and `None` to give — so the sentence that stood
            // here, "reported absent, which cannot be true", described a state no value can hold.
            // That is not the empty-name line above: that one guards an invariant a future edit
            // could break and says so, while this one guarded the type system. A tripwire that
            // cannot trip is not a tripwire, and a message nobody can be shown is not a wording
            // problem — SKEIN-767 measured it as unreachable and SKEIN-777 took it off the queue.
            //
            // `_` rather than `None` so the compiler does not ask the arm back. Narrowing the
            // return type to `bool` is the real fix and belongs beside `fleet_exists` itself
            // (SKEIN-787), which needs `src/volume.rs` as well as this file.
            _ => println!(
                "{BAD} sandbox       cannot tell if it exists — {}",
                skein::sbx::fleet_failure().unwrap_or_else(|| "sbx did not answer".into())
            ),
        }
        // The exact `sbx create` line for THIS installation, printed whether or not the sandbox
        // exists — because the moment it is wanted is the moment there is no cockpit to ask.
        //
        // **Mounts are fixed at create**, and no verb adds one to a sandbox that already exists
        // (`sbx --help`). So a fleet made with a shorter line than this
        // cannot be repaired, and a repo whose store is outside `~/.skein` — `skein add <git-url>
        // --store …` — is invisible to every box until the sandbox is destroyed and remade. That
        // failure reads as a broken box rather than a missing mount, which is why the line is
        // printed rather than described (SKEIN-462).
        //
        // Rendered by `Act::Create::command`, the same function the warden prompt uses, so what is
        // printed here and what skein would ask a person to approve cannot drift apart.
        //
        // The SERVING mount set, not [`fleet_mounts`]: a fleet skein runs inside needs the volume
        // root itself, and `fleet_serve_mounts` is that set — the volume plus every repo that
        // lives outside it. Printing the other one would leave out `~/.skein` and reproduce the
        // exact bug this line exists to prevent.
        //
        // **No `Err` arm** (SKEIN-777). `create_line` has one return and it is `Ok`: its only
        // fallible call, `ensure_fleet_kit`, is `eprintln!`'d rather than propagated, so the
        // "cannot be worked out" line that stood here was written for an `Err` no value inhabits.
        // The `Result` is still in the signature — narrowing it is SKEIN-787, which also has to
        // reach `src/volume.rs` and `fleet_lifecycle_refusal`'s copy of the same dead sentence.
        if let Ok(line) = skein::fleet::create_line(&fleet) {
            println!("{DIM}·{RESET} create line   {line}");
            println!(
                "{DIM}              sbx fixes mounts at create, so a repo registered later \
                 from outside ~/.skein needs this line run again{RESET}"
            );
        }

        // Memory and CPUs are fixed at create and sbx has no resize, so this is the one setting a
        // person cannot fix once they notice it — which makes "did anybody choose this?" worth
        // answering out loud rather than only at the next install. Omitted flags are not an error:
        // sbx takes half the host's memory and all of its cores, and says nothing.
        //
        // In-fleet only. `/proc/meminfo` and `nproc` are the SANDBOX's here, which is exactly the
        // comparison wanted; on a host they describe the wrong machine and the line would be a
        // confident lie.
        {
            let cpus = std::thread::available_parallelism()
                .map(|n| n.get().to_string())
                .unwrap_or_else(|_| "?".into());
            match skein::fleet::recorded_fleet_size() {
                Some((memory, stated_cpus)) => {
                    println!("{DIM}·{RESET} fleet size    {memory}, {stated_cpus} CPUs — stated at install");
                    if stated_cpus != cpus {
                        println!(
                            "{WARN}              this sandbox now reports {cpus} CPUs, so it is not the one \
                             that was approved"
                        );
                    }
                }
                None => {
                    println!(
                        "{WARN} fleet size    nobody stated this fleet's memory or CPUs; it has {cpus} CPUs"
                    );
                    println!(
                        "{DIM}              sbx fixes both at create and has no resize, so changing them \
                         means rebuilding the sandbox{RESET}"
                    );
                }
            }
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

        // **The earliest file named `claude` on a box's PATH is the one that runs** — SKEIN-870.
        //
        // Beside the three tools above because it is the same question one layer in: those ask
        // whether the SANDBOX has what a box needs, this asks whether the thing a box would resolve
        // as its agent is the agent. It is the one that was false here and said nothing — a
        // 500-byte "native binary not installed" stub, mode 644, sat at the head of every box's
        // PATH while every surface reported a healthy fleet, because `command -v` skips a file
        // without the execute bit and answers with the next one down.
        //
        // **Asked of the sandbox, once — not of each box, and that is not an approximation.** A
        // box's PATH is `place::box_path(home)`, and `home` in a placement record is documented as
        // the SANDBOX's own path rather than a private directory: `box-session.sh` binds `.local`
        // back through from the sandbox into every box, and `/usr/local/share/npm-global/bin` is
        // outside $HOME and so is the sandbox's own directory in every namespace. So the files this
        // reads ARE the files a box resolves, and asking eleven boxes would be eleven crossings for
        // eleven copies of one answer — which for a diagnostic somebody runs while something is
        // already wrong is a cost with nothing on the other side of it.
        //
        // The cost of that choice, stated rather than hidden: this cannot see a box that was
        // started under an older launcher and is carrying a different PATH (`PlaceRecord::launcher`
        // records which cover a box was born under, and the isolation row above reports it), and it
        // cannot see a PATH an agent changed inside its own session. Both are narrower than the
        // class this catches, and neither is reachable without entering every box.
        //
        // Two probes, not one, and it spawns — so here rather than in `health_report`, which every
        // open board polls every fifteen seconds. Same reason as the model line above.
        {
            let ask = |script: &str| {
                place
                    .exec(script, std::time::Duration::from_secs(30))
                    .ok()
                    .map(|o| o.to_string())
            };
            // The homes the placements actually record, deduped — in practice one, because every
            // box's home is the sandbox's own path. Read from the records rather than assumed, so
            // that a fleet where that stops being true gets a row per home instead of one confident
            // answer about a PATH no box has. A fleet with no boxes yet has no placement to read,
            // and the sandbox's own $HOME is what the next box would be handed.
            let mut homes: Vec<String> = skein::place::placed_boxes(&fleet)
                .into_iter()
                .map(|(_, record)| record.home)
                .filter(|home| !home.is_empty())
                .collect();
            homes.sort();
            homes.dedup();
            if homes.is_empty() {
                homes.extend(
                    ask("printf '%s' \"$HOME\"")
                        .map(|home| home.trim().to_string())
                        .filter(|home| !home.is_empty()),
                );
            }
            if homes.is_empty() {
                println!(
                    "{WARN} box agent     no box PATH to check — nothing is placed and the sandbox \
                     did not say what HOME a box would get"
                );
            }
            for home in &homes {
                let a = skein::agentpath::agent_on_box_path(home, &ask);
                let mark = match a.level {
                    skein::health::Level::Satisfied => OK,
                    skein::health::Level::Unsatisfied => BAD,
                    skein::health::Level::Unknown => WARN,
                };
                println!("{mark} box agent     {}", a.detail);
                if !a.fix.is_empty() {
                    println!("{DIM}              → {}{RESET}", a.fix);
                }
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
        for (cgroup, what, capped_is_the_goal) in skein::fleet::CEILINGS {
            let live = probe(&format!(
                "cat /sys/fs/cgroup/{cgroup}/memory.max 2>/dev/null"
            ));
            // The judgement is `fleet`'s, beside the code that WRITES these values. It was here, and
            // it disagreed with that code: `docker=max/max` is written on purpose and this printed a
            // red ✗ for it, with no fix under it because there is nothing to fix.
            match skein::fleet::ceiling_reading(what, *capped_is_the_goal, &live) {
                skein::fleet::Ceiling::Good(said) => println!("{OK} {cgroup:<13} {said}"),
                skein::fleet::Ceiling::Bad(said) => println!("{BAD} {cgroup:<13} {said}"),
                skein::fleet::Ceiling::Absent(said) => {
                    println!("{DIM}·{RESET} {cgroup:<13} {said}")
                }
            }
        }

        // A mount that is missing produces a box with no store, which looks entirely healthy.
        //
        // **The set the sandbox was CREATED from, [`skein::fleet::fleet_serve_mounts`] — the same
        // one the create line above prints** (SKEIN-678). This iterated `fleet_mounts()`, so the
        // check and the thing it checks were written from two different lists inside one binary.
        // `fleet_serve_mounts` is `[$SKEIN_HOME] + fleet_mounts()` deduped, and the entry the two
        // differ by is the volume root — the one entry an install cannot proceed without, because
        // `bootstrap.sh` locates the volume by scanning mountinfo for a mount point ending in
        // `/.skein` and refuses rather than guess. A fleet missing exactly that reported every
        // mount present and healthy here.
        //
        // **`test -d` cannot ask about the volume root, which is why the probe changed with the
        // list.** `fleet_serve_mounts` dedupes `repos/` and `boxes/` away *because* they are under
        // the volume root; on a fleet created without it those two are the mounts, and binding
        // them makes `~/.skein` exist as an ordinary directory. `test -d` on the volume root is
        // then true on precisely the fleet this row exists to catch — an assertion with no failing
        // case. Measured, not reasoned: delete the volume root and run this, and the row still says
        // the directory is there, because `create_line` two rows above calls `ensure_fleet_kit`
        // and puts it back.
        //
        // So mountinfo is read once, and it answers two questions rather than one: is anything
        // mounted AT this path, and — the diagnosis — is anything mounted BENEATH it while nothing
        // is mounted at it, which is the hollow shape a short create line leaves behind. Nothing
        // is claimed from the absence of a mount alone: this same command run inside a box reads
        // the box's namespace rather than the sandbox's, where a path can be visible through the
        // `/` dev-bind with no mount of its own. That says "not confirmed", not "missing".
        //
        // Mountinfo escapes space, tab, newline and backslash as octal, so the path is escaped the
        // same way rather than the field unescaped: that direction is total, the other guesses.
        let mountinfo = probe("awk '{print $5}' /proc/self/mountinfo 2>/dev/null");
        let mounted_at: std::collections::HashSet<&str> = mountinfo.lines().collect();
        for path in skein::fleet::fleet_serve_mounts() {
            let escaped = path
                .replace('\\', "\\134")
                .replace(' ', "\\040")
                .replace('\t', "\\011")
                .replace('\n', "\\012");
            let here = mounted_at.contains(escaped.as_str());
            let beneath = mounted_at
                .iter()
                .any(|m| m.starts_with(&format!("{escaped}/")));
            let there = probe(&format!("test -d '{path}' && echo yes")) == "yes";
            let rebuild = format!(
                "`skein resize {}` rebuilds it with the create line above and carries every box \
                 across",
                match cfg.fleet_memory.trim() {
                    "" => "26g",
                    size => size,
                }
            );
            let (mark, note) = if here {
                (OK, String::new())
            } else if !there {
                (
                    BAD,
                    format!(
                        " — not visible in the sandbox; boxes for it would come up with no store. \
                         {rebuild}"
                    ),
                )
            } else if beneath {
                (
                    BAD,
                    format!(
                        " — NOT MOUNTED, though directories under it are: this is the empty shell \
                         those binds created, and it is what the create line was short of. \
                         bootstrap.sh looks for exactly this mount and refuses to install when it \
                         finds none. {rebuild}"
                    ),
                )
            } else {
                (
                    WARN,
                    " — the directory is there, but nothing in this namespace is mounted at it. \
                     Run this at fleet scope rather than inside a box, where the sandbox's mounts \
                     are not the ones on show"
                        .to_string(),
                )
            };
            println!("{mark} mount         {DIM}{path}{RESET}{note}");
        }
    }

    // **The composition nobody chose in one place** — `docs/pr-review.md` §13. Automatic review and
    // a merge train are each a decision somebody made on their own terms; together, on one repo,
    // with the ceiling at `approve`, they are skein approving its own work and merging it with
    // nobody in it. That combination is reachable rather than prevented, by decision, and the one
    // thing the decision came with is that it must not be reachable *silently*.
    //
    // Here rather than only in the cockpit because this is the command somebody runs when they want
    // to know what their fleet is actually set up to do, and because it is the surface a person has
    // when the cockpit is the thing that is not working.
    for repo in skein::repos::load_repos() {
        if let Some(loop_) = skein::prwork::the_loop_this_repo_has_built(&repo) {
            println!("{WARN} review loop   {loop_}");
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

/// `skein announce` — one tick of [`skein::announce`], and say what it did.
///
/// The verb exists because the delivery has to be drivable from somewhere before it can be trusted
/// from anywhere. `doctor` next to it is the pull — a person asking how full the disk is — and this
/// is the push: it says nothing at all unless the fleet has *crossed* the threshold since skein
/// last spoke, so running it twice in a row is silent the second time, on purpose.
///
/// It is deliberately not folded into `doctor`. A diagnostic that also interrupts eleven agents is
/// a diagnostic people stop running, and the two answer different questions to different people.
///
/// **The cadence is not here, and it exists now.** `skein-server` spawns
/// [`skein::announce::watch_fleet_disk`] at startup (SKEIN-734), and that loop is what drives the
/// delivery unattended — it keeps turning when no browser tab is open, which is the day this was
/// built for. This doc said "nothing runs this on a timer yet" for as long as that was false
/// (SKEIN-748); it was written true and went stale in the commit that made the loop.
///
/// So the verb is no longer the only driver, and it is kept for the case the loop cannot serve: a
/// fleet whose server is not running, which is exactly the fleet somebody is debugging. Both call
/// the same entry point, so neither can drift from the other's behaviour.
fn cmd_announce() -> Result<(), String> {
    use skein::announce::{Quiet, Step};
    let policy = skein::announce::Policy::default();
    let outcome = skein::announce::announce_fleet_disk(&policy)?;
    match outcome.step {
        // One note per box and each printed whole, because what each box is asked to free is its
        // own figure — printing the first and the list of names would show a number that is right
        // for one reader and wrong for the other.
        Step::Announce => {
            for note in &outcome.told {
                println!("{BOLD}told {}{RESET}", note.to);
                println!("{DIM}{}{RESET}", note.body);
            }
        }
        Step::Cleared => println!(
            "{DIM}the fleet is back under the line; nobody was interrupted to be told so, and the \
             next crossing will be announced again{RESET}"
        ),
        Step::Quiet(Quiet::RoomLeft) => println!("{DIM}there is room; nothing to say{RESET}"),
        Step::Quiet(Quiet::AlreadySaid) => {
            println!("{DIM}over the line, and already said — `skein doctor` has the figures{RESET}")
        }
        Step::Quiet(Quiet::NotMeasured) => println!(
            "{DIM}the fleet's disk could not be measured, so nothing is claimed about it{RESET}"
        ),
    }
    Ok(())
}

/// `skein pull [<repo-id>]` — refresh the mirror boxes clone from.
///
/// A verb because skein already told people to run it: `ensure_mirror`'s failure says
/// "`skein pull <id>` makes one", and until now that printed `unknown command`. The mirror is what
/// a new box clones from, so a repo without one falls back to cloning from the remote itself —
/// slower, and only possible where the box can reach it.
///
/// Every repo when none is named, because "the mirrors are stale" is the usual shape of the problem
/// and naming them one at a time is a chore. Failures are collected rather than fatal: one
/// unreachable remote must not stop the others being refreshed, and the point of running this is
/// usually the others.
fn cmd_pull(id: Option<&str>) -> Result<(), String> {
    let repos = skein::repos::load_repos();
    let wanted: Vec<_> = match id {
        Some(id) => repos.iter().filter(|r| r.id == id).collect(),
        None => repos.iter().collect(),
    };
    if wanted.is_empty() {
        return Err(match id {
            Some(id) => format!("no registered repo {id:?} — `skein repos` to check"),
            None => "no repos are registered — `skein add <git-url>` first".into(),
        });
    }
    // The doctor's marks, local to it — borrowed here rather than hoisted, because a listing that
    // is going to grow a third state is not the reason to make three constants global.
    const OK: &str = "\x1b[32m✓\x1b[0m";
    const BAD: &str = "\x1b[31m✗\x1b[0m";
    let mut failures = Vec::new();
    for repo in wanted {
        match skein::repos::fetch_mirror(repo) {
            Ok(()) => println!("{OK} {} {DIM}mirror refreshed{RESET}", repo.id),
            Err(why) => {
                println!("{BAD} {} {DIM}{why}{RESET}", repo.id);
                failures.push(repo.id.clone());
            }
        }
    }
    match failures.is_empty() {
        true => Ok(()),
        false => Err(format!(
            "could not refresh {} — a box created now clones from the remote instead",
            failures.join(", ")
        )),
    }
}

/// `skein stop <box>` — end the box, keep its work.
///
/// A verb rather than a cockpit-only button, because the place people are told to stop a box is
/// `skein doctor`, and doctor is what somebody runs when the cockpit is the thing that is not
/// answering. Three of doctor's own fix lines named `skein restart <box>` while neither verb
/// existed, so the only thing said to a person looking at a fault was a command that printed
/// `unknown command`.
fn cmd_stop(name: &str) -> Result<(), String> {
    eprintln!("{DIM}skein:{RESET} stopping {name}…");
    skein::sandbox::stop_box(name)?;
    eprintln!(
        "{DIM}skein:{RESET} {name} is stopped — its checkout, branch and conversation are untouched"
    );
    Ok(())
}

/// `skein restart <box>` — stop it and start it again.
///
/// **Stop, not "restart the agent".** Those are different acts and this is the bigger one: it ends
/// every process in the box, rebuilds its mount namespace from the launcher this skein installs,
/// and re-applies the ceilings. That is what makes it the answer to a box running under an older
/// cover, and it is why the fix lines say what it costs — whatever the agent was part-way through
/// does not survive. Restarting only the agent's session is a different thing and stays where it
/// is, on the cockpit's row.
///
/// The stop is best-effort, deliberately: a box whose session is already gone must still be
/// startable, and refusing here would leave the one case people most want this for — a box that is
/// half-dead — with nothing to run.
fn cmd_restart(name: &str, opts: &[String]) -> Result<(), String> {
    if let Err(why) = skein::sandbox::stop_box(name) {
        eprintln!("{DIM}skein:{RESET} {name} did not stop cleanly ({why}); starting it anyway");
    }
    cmd_start(name, opts)
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
    // **Not answerable with `--uncovered`, and saying so is the point.** A box whose name matches
    // no repository is uncovered (`fleet::refuse_if_uncovered`), but that is the smaller half of
    // what is missing here: there is no remote to clone from and no store to link, so there is no
    // box to start however exposed anyone is willing to have it. The step that works is the name or
    // the registration, and this says both rather than leaving `skein repos` to be interpreted.
    let repo = skein::repos::repo_for_box(name).ok_or_else(|| {
        format!(
            "no registered repo for box {name}, so skein has nothing to clone it from, no store \
             to link into it, and no mounts it could name as the box's own.\n\
             `skein repos` lists the ids skein knows — a box whose name starts with one of them \
             is matched with no further ceremony.\n\
             `skein add <git-url> --id <id>` registers a repository skein does not have yet."
        )
    })?;
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
    // The deliberate act `fleet::refuse_if_uncovered` asks for. Written before the start rather
    // than passed into it, because it has to outlive this command: the same box is restarted by
    // `ensure_box_session` from inside the server, where there is no one to type a flag, and a
    // permission that lasted one launch would refuse every attach after it (SKEIN-846).
    if opts.iter().any(|o| o == "--uncovered") {
        skein::fleet::allow_uncovered(name, true)?;
    }
    skein::fleet::start_box(
        name,
        &repo,
        &branch,
        "exec bash -l",
        skein::place::Purpose::Manual,
    )?;
    eprintln!("{DIM}skein:{RESET} {name} is up on {branch}");
    // `--attach` exists so the cockpit's create-a-box terminal can hand off into the agent without
    // the caller having to name the box's placement — which does not exist until the line above has
    // run. Resolved here, after the box is real.
    if opts.iter().any(|o| o == "--attach") {
        return run_attach(&skein::sandbox::initial_attach_argv_as(name, &agent));
    }
    Ok(())
}

/// Run an already-built attach argv, reporting the one failure worth naming.
///
/// The program is `argv[0]` rather than a literal `sbx`, because in-fleet it is not `sbx` — skein is
/// already in the sandbox and the crossing is the `nsenter` alone. Spelling the program here would
/// be this function deciding a thing the placement already decided.
fn run_attach(argv: &[String]) -> Result<(), String> {
    let Some((program, args)) = argv.split_first() else {
        return Err("nothing to run".into());
    };
    match Command::new(program).args(args).status() {
        Ok(s) if s.success() => Ok(()),
        Ok(_) => Err(format!("{program} exited non-zero")),
        Err(e) if e.kind() == ErrorKind::NotFound => Err(match program.as_str() {
            "sbx" => "sbx not found on PATH — attaching from the host needs the sbx CLI".into(),
            other => format!("{other} not found on PATH"),
        }),
        Err(e) => Err(format!("running {program}: {e}")),
    }
}

/// Update the agent CLIs every box in this fleet shares.
///
/// **The only path that can do it**, and until SKEIN-403 the only one that LOOKED like it could was
/// a `claude update` run inside a box, where the CLI is root-owned and unwritable — so it failed
/// silently every time and the sandbox's runtimes were frozen at whatever version first landed. See
/// [`skein::fleet::update_runtimes`] for why this has to happen in the sandbox instead.
///
/// Prints what actually moved rather than "done": `claude: 1.2.3 -> 1.2.9` is checkable and "done"
/// is not.
fn cmd_update_agents() -> Result<(), String> {
    let sandbox = skein::place::fleet_sandbox();
    eprintln!("{DIM}skein:{RESET} updating the agent CLIs in {CYAN}{sandbox}{RESET} — this is an npm install, so give it a minute");
    let report = skein::fleet::update_runtimes(&sandbox)?;
    println!("{report}");
    // Said rather than done: a box's agent is somebody's live session, and restarting it to pick up
    // an update is not a call this should make on its own.
    eprintln!("{DIM}skein:{RESET} boxes already running keep the version they started with until their next session");
    Ok(())
}

/// `skein cockpit-stop` — stop serving, and keep the door.
///
/// **It used to be a flag on the verb it undid**, and that verb has gone: the fleet-serve verb was a
/// skein OUTSIDE the sandbox carrying the server in, which is `bootstrap.sh`'s job now (SKEIN-576).
/// The stopping is not the installer and did not go with it, so it needed a name of its own. Not a
/// bare `stop`, which already means "stop a box" — a second top-level stop meaning something else
/// is the ambiguity, not the fix.
///
/// The rejected name is described rather than written, and that is not fastidiousness:
/// `tests/fix_lines.rs` fails the build on a backticked `skein <verb>` the dispatch does not have,
/// wherever it appears. It caught this comment naming the verb it was arguing against — which is
/// the gate being exactly right, since a reader who types what they see gets an error either way.
///
/// The two lines it prints are the two things a person is about to get wrong. **The port is still
/// held** — [`skein::fleet::stop_serving`] leaves the doorway standing on purpose, so this is not
/// a way to free :7878 in the sandbox, and it is why stopping is safe to do casually. **Boxes keep
/// running** — the cockpit is how you watch a fleet, not what runs it, and somebody who stops the
/// server expecting their agents to stop with it has stopped watching instead.
fn cmd_cockpit_stop(sandbox: &str) -> Result<(), String> {
    let was = skein::fleet::stop_serving(sandbox)?;
    println!(
        "{}",
        match was.as_str() {
            "running" => format!("skein-server in {sandbox} has been stopped"),
            _ => format!("skein-server was not running in {sandbox} — nothing to stop"),
        }
    );
    println!(
        "{DIM}the doorway still holds the cockpit's port, so nothing else in the fleet can take \
         it; the supervisor puts a server back behind it as soon as one is on disk{RESET}"
    );
    println!("{DIM}every box keeps running — this stops watching the fleet, not the fleet{RESET}");
    Ok(())
}

/// `skein save [<box>…]` — copy every box's work out of the sandbox and onto the host.
///
/// **The terminal half of one act with two front doors** (SKEIN-680). The cockpit has a button for
/// this; `skein resize` refuses in a terminal, and a button is no use to somebody reading that
/// refusal, so the offer it makes has to name something typeable. Both doors call
/// [`skein::fleet::save_boxes`] — there is no second implementation here, and the message that
/// offers it (`fleet::destroy_costs`, reached through `fleet_lifecycle_refusal`) names this verb,
/// which is why `tests/fix_lines.rs` would fail the build if the two ever landed apart.
///
/// **Per box, and the failures are the point.** Every box gets its own line whether it made it out
/// or not, because a partial save is the case that matters: the box that failed is exactly the one
/// whose work is still only inside the sandbox. So a run with any failure exits non-zero *after*
/// printing the whole report — the boxes that did make it are still worth reading.
///
/// The restore line is printed beside each archive rather than described, and it is
/// `fleet::restore_script`'s own text: a copy on the host is half of "your work is safe" only if
/// something can put it back, and a paraphrase of the command would drift from the command.
fn cmd_save(names: &[String]) -> Result<(), String> {
    let sandbox = skein::place::fleet_sandbox();
    let saved = skein::fleet::save_boxes(names)?;
    for one in &saved {
        match one.error.is_empty() {
            true => {
                println!("{BOLD}{}{RESET}  {}", one.name, one.archive);
                println!(
                    "{DIM}  put it back, inside the fleet sandbox:{RESET} {}",
                    one.restore
                );
            }
            // Not styled as a note. This line is the reason the report is per box.
            false => println!("{BOLD}{}{RESET}  NOT SAVED — {}", one.name, one.error),
        }
    }
    let failed: Vec<&str> = saved
        .iter()
        .filter(|one| !one.error.is_empty())
        .map(|one| one.name.as_str())
        .collect();
    if failed.is_empty() {
        eprintln!(
            "{DIM}skein:{RESET} {} box{} copied out of {sandbox} to the host — nothing was stopped \
             and nothing was destroyed",
            saved.len(),
            if saved.len() == 1 { "" } else { "es" },
        );
        return Ok(());
    }
    Err(format!(
        "{} of {} box{} could not be copied out ({}) — that work is still only inside {sandbox}, \
         and a destroy would take it",
        failed.len(),
        saved.len(),
        if saved.len() == 1 { "" } else { "es" },
        failed.join(", "),
    ))
}

/// `skein resize <memory> [cpus]` — refuse, and say what rebuilding the sandbox would cost.
///
/// **The old justification for this verb was the host-driven one**, and it read: a CLI command and
/// not only a cockpit button because this is the one operation that destroys the sandbox — `sbx
/// create` may ask for confirmation, a server has no terminal to answer with, so the riskiest path
/// needs to be runnable somewhere a person is sitting. There is no host skein now (SKEIN-576). The
/// only place a person can sit is *inside the sandbox being destroyed*, so the terminal that would
/// answer the confirmation dies at the destroy, along with the process that was going to run the
/// create. That is not a place from which to drive the riskiest path; it is the reason there is no
/// such place (SKEIN-679).
///
/// So it refuses, through [`skein::fleet::fleet_lifecycle_refusal`] — the cockpit's own refusal
/// rather than the same thing said again here. **Two surfaces, one wall, one message.** The person
/// typing this and the person clicking Rebuild have hit the same limit and need the same four
/// things: what is refused, what a destroy costs in boxes, the save to take first, and the two
/// lines to run on the host. A second copy in the CLI's own voice is two messages that drift apart
/// the first time either is edited, and drift is the whole failure this item is about.
///
/// What is NOT done here is anything else. The numbers are not written to the settings: a command
/// that refused must not quietly change what the next create does, so they are said back instead
/// and the person edits the flags in the line they are given.
fn cmd_resize(memory: &str, cpus: &str, disk: &str, drop_docker: bool) -> Result<(), String> {
    let why = skein::fleet::fleet_lifecycle_refusal("resize", true).ok_or(
        "skein is running inside the fleet sandbox, so it cannot resize it from here — and no \
         sandbox is named in the settings, so there is no line to give you either. `skein doctor` \
         prints what it can work out about this installation.",
    )?;
    let asked = [("memory", memory), ("cpus", cpus), ("disk", disk)]
        .iter()
        .filter(|(_, v)| !v.trim().is_empty())
        .map(|(what, v)| format!("{what} {}", v.trim()))
        .collect::<Vec<_>>()
        .join(", ");
    let mut note = format!(
        "\n\nThe size you asked for ({asked}) is not in that create line — it renders the size \
         this installation is configured for, and sbx fixes all three at create. Edit the flags in \
         the line to the size you want."
    );
    if drop_docker {
        // Said rather than silently ignored. The flag means "I have accepted the loss", and a
        // person who typed it has accepted a loss this command is not going to inflict — but the
        // `sbx rm -f` line above will, whether or not they typed anything.
        note.push_str(
            "\n--drop-docker changes nothing from here: this command destroys nothing, and the \
             destroy line takes /var/lib/docker with the sandbox either way.",
        );
    }
    Err(format!("{why}{note}"))
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
    // The tail — clear the refusal memory, hand the credential to running boxes — lives in
    // `fleet::after_login` so the server's login route runs the SAME tail in the server process,
    // where the refusal memory that matters actually lives. Run from this CLI it clears only this
    // process's copy, which is why the route exists at all.
    for line in skein::fleet::after_login(runtime) {
        eprintln!("{DIM}skein:{RESET} {line}");
    }
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
        let replacement = skein::takeover::replace_box(name, &agent)?;
        eprintln!(
            "{DIM}skein:{RESET} source preserved; replacement is {}",
            replacement.target
        );
        replacement.target
    } else {
        if handoff {
            let path = skein::handoff::prepare_handoff(name, None, &agent)?;
            eprintln!("{DIM}skein:{RESET} handoff prepared at {}", path.display());
        }
        name.to_string()
    };
    // A fleet box loses its tmux server whenever its sandbox cycles; the tree, the private HOME and
    // the cgroup survive. Restart the session before addressing its namespace, or the first thing
    // the user sees is `nsenter: cannot open /proc/<pid>/ns/user`.
    // Attaching is a start for a box whose sandbox has cycled, so it meets the same wall — and it
    // is the wall's likeliest meeting place, because this is the path a box whose repository was
    // unregistered since it came up comes back through.
    if opts.iter().any(|o| o == "--uncovered") {
        skein::fleet::allow_uncovered(&attach_name, true)?;
    }
    skein::fleet::ensure_box_session(&attach_name)?;
    let dir = skein::sbx::lookup_dir(&attach_name).unwrap_or_default();
    run_attach(&skein::sandbox::attach_argv_as(&attach_name, &dir, &agent))
}

#[cfg(test)]
mod tests {
    /// **`skein add --agent` is refused, and says where the runtime is chosen now** (the owner,
    /// 2026-09-27: one fleet default plus a per-box pick). Refused before anything is cloned or
    /// written, so this touches no home.
    ///
    /// What would make it fail: the flag read and dropped, so the add goes ahead and the person
    /// believes their repo runs the runtime they named.
    #[test]
    fn add_refuses_a_runtime_for_the_repo_and_names_where_it_lives() {
        // Pinned, so a regression that lets the add go ahead writes into this directory and not
        // into whatever store the machine running the test has. Planting exactly that regression
        // once, unpinned, scaffolded a store in the owner's live `~/.skein`.
        let home = std::env::temp_dir().join(format!("skein-add-agent-{}", std::process::id()));
        std::fs::create_dir_all(&home).unwrap();
        std::env::set_var("SKEIN_HOME", &home);
        std::env::set_var("SKEIN_FLEET_ROOT", &home);
        let why = super::cmd_add(
            "https://example.com/thing.git",
            &["--agent".to_string(), "codex".to_string()],
        )
        .expect_err("a repo has no runtime of its own to set");
        assert!(
            why.contains("Default agent") && why.contains("--agent <runtime>"),
            "{why}"
        );
        let _ = std::fs::remove_dir_all(&home);
    }
}
