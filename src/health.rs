//! Is skein's own environment sound? One report the cockpit and `skein doctor` both render, so a
//! misconfiguration is diagnosed in one place rather than guessed at from a failure downstream.

use crate::repos::load_repos;
use crate::runtime::*;
use crate::sbx::{fleet_boxes, fleet_degraded};
use crate::util::*;
use crate::{load_registry, load_views};
use serde::Serialize;
use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

#[derive(Debug, Clone, Serialize)]
pub struct HealthCheck {
    pub ok: bool,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct HealthReport {
    pub ok: bool,
    pub registry: HealthCheck,
    pub sbx: HealthCheck,
    pub git: HealthCheck,
    pub gh: HealthCheck,
    pub probes: HealthCheck,
    pub mailbox: HealthCheck,
    /// Whether AI enrichment is on and can actually run. Never `ok: false` — it is opt-in, so
    /// "off" is a correct state, not a fault; the detail says what turning it on would buy.
    pub ai: HealthCheck,
    /// How the fleet sandbox's memory is divided between the boxes, its inner Docker daemon, and
    /// the reserve that keeps the sandbox itself answering. Worth a line of its own because when
    /// this is wrong the symptom is not a message — it is a sandbox that stops responding.
    pub memory: HealthCheck,
    /// Whether a box's GitHub credential is actually scoped, and why not when it isn't.
    ///
    /// Never `ok: false` for being switched off — scoping is opt-in and "off" is a correct state.
    /// It reports `false` only when the fleet is *trying* to scope and cannot: an App ID that
    /// GitHub rejects, a key for a different App, an App installed on none of the repos in use.
    /// Those failures were previously invisible — `refresh_tokens` produced exact, useful errors
    /// and the server printed them to a detached process's stderr, so the first place anyone
    /// learned of one was a 403 inside a box some minutes later.
    pub gitgate: HealthCheck,
    /// Which agent runtimes have a login every new box will inherit. Empty means `skein login` has
    /// not been run — the single most common way a first run goes quiet, since each box then comes
    /// up sitting at a sign-in prompt doing nothing.
    pub logins: Vec<String>,
    pub dark_boxes: Vec<String>,
    pub stale_boxes: Vec<String>,
    pub runtimes: Vec<RuntimeInfo>,
    /// How boxes get GitHub credentials, named — or empty when nobody has chosen.
    ///
    /// Empty is a real state now, not a theoretical one. All three paths are opt-in, so a fresh fleet
    /// has no way to push until someone picks one, and the first-run checklist asks on the strength of
    /// this field. It used to be unaskable: the account token was seeded by default, so the answer was
    /// always "the account token" and the question would have been noise.
    pub git_credential: String,
}

/// Render [`crate::gitgate::ScopeStatus`] as a health line.
///
/// Presentation only — the states themselves belong to `gitgate`, which is the module that knows
/// what they mean. This file used to derive them by reaching into five of its internals, which is
/// how two callers of the same question end up disagreeing.
fn git_scope_health() -> HealthCheck {
    use crate::gitgate::ScopeStatus::*;
    // What a box holds when scoping is *not* in force is no longer a fixed sentence. It used to be —
    // the account token was seeded by default, so "not scoped" always meant "every box holds your
    // whole account". Now that all three credential paths are chosen, an unscoped box may hold nothing
    // at all, and telling someone their boxes carry a credential they never picked sends them hunting
    // the wrong problem the first time a push fails.
    let unscoped_holds = match crate::gitgate::box_credential() {
        crate::gitgate::BoxCredential::None => {
            "boxes have no GitHub credential at all and cannot push".to_string()
        }
        other => format!("every box holds {}", other.label()),
    };
    match crate::gitgate::scope_status() {
        Off => HealthCheck {
            ok: true,
            detail: format!(
                "off — {unscoped_holds}. Settings → scope each box's access to its own repo"
            ),
        },
        NotConfigured => HealthCheck {
            ok: true,
            detail: format!(
                "not set up — {unscoped_holds}. Settings → GitHub & keys → add a GitHub App or a \
                 per-repo token to scope them"
            ),
        },
        Unusable { why, refused } => HealthCheck {
            ok: false,
            detail: format!(
                "ON but nothing is scoped, so {unscoped_holds}: \
                 {why}.{} Add a GitHub App or a per-repo token under Settings → GitHub & keys.",
                match refused.is_empty() {
                    true => String::new(),
                    false => format!(" Stored tokens refused — {}.", refused.join("; ")),
                }
            ),
        },
        Active { app, tokens } => HealthCheck {
            ok: true,
            detail: format!(
                "on — boxes write only their own repo.{}{}",
                match app.is_empty() {
                    true => String::new(),
                    false => format!(" App {app}"),
                },
                match tokens {
                    0 => String::new(),
                    n => format!(" {n} stored repo token(s)"),
                }
            ),
        },
    }
}

/// The git-scope check alone, so `skein doctor` can print the one line without building the whole
/// report — which probes the sandbox and takes seconds.
pub fn health_report_gitgate() -> HealthCheck {
    git_scope_health()
}

/// Read-only environment diagnosis for detached server deployments. Unlike startup `eprintln!`,
/// this remains inspectable from the cockpit and makes a missing box-side jq dependency explicit.
pub fn health_report() -> HealthReport {
    let ai = HealthCheck {
        ok: true,
        detail: if !crate::ai::ai_enabled() {
            "off — Settings → Boxes turns it on: a one-line summary for boxes with no \
             journal, and a second opinion before Continue N resumes anything"
                .into()
        } else if !program_on_path("claude") {
            "on, but `claude` is not on PATH — every call falls back to the free signals".into()
        } else {
            "on — rationed Haiku over your subscription, on demand and cached per turn-end".into()
        },
    };
    // Named in GiB rather than MiB: these are numbers a person compares against how much memory the
    // Mac has, and 15975 does not read as "about sixteen gigabytes" at a glance.
    let gib = |mib: u64| format!("{:.1}G", mib as f64 / 1024.0);
    // Every box shares one sandbox, so there is always a division to report. This used to have a
    // "one sandbox per box — nothing to divide" arm for a fleet whose name was cleared; that model is
    // gone, and with it the only way to reach it.
    let memory = match crate::fleet::memory_plan() {
        Some(plan) => HealthCheck {
            ok: true,
            detail: format!(
                "{} across all boxes and the containers they start, {} for the sandbox's own \
                 daemons, {} kept back for the VM's services and the kernel",
                gib(plan.boxes),
                gib(plan.plumbing),
                gib(plan.reserve)
            ),
        },
        // A fleet whose total is unset has no ceiling anywhere: not per box, not on the boxes
        // together, not on Docker. One build can then reach the VM's memory, and with no swap the
        // kernel's global OOM killer picks a victim by badness rather than by blame.
        None => HealthCheck {
            ok: false,
            detail: "no memory ceiling anywhere: Settings → Fleet memory names no size, so one \
                     box's build can take the sandbox down with it"
                .into(),
        },
    };
    // The legacy single-repo registry. Managed repos are the supported path and the board does not
    // read this at all — it aggregates per-repo stores via `all_stores` — so a fleet with repos
    // registered is healthy whether or not a `sandboxes.json` exists anywhere.
    //
    // It used to be a hard failure, and on a clean install it failed *by construction*: with no
    // `$SKEIN_REGISTRY`, `locate_registry` falls back to a sibling `skein-shared/` directory named
    // after a different project, which no new user has. So the first thing anyone saw was the
    // product declaring itself broken, permanently, over a file it no longer needs — and every real
    // fault afterwards was noise in a banner that never cleared.
    let repos_registered = !crate::repos::load_repos().is_empty();
    // An unset registry is not a broken registry. It is a fault only when someone has *named* one —
    // `$SKEIN_REGISTRY` or `$SKEIN_SHARED` — and it cannot be read. With neither set and no repos
    // yet, the honest report is "nothing here yet"; the empty state already says to add a repo, and
    // a red banner repeating it is noise on the one screen that should be welcoming.
    let registry_named = std::env::var_os("SKEIN_REGISTRY")
        .or_else(|| std::env::var_os("SKEIN_SHARED"))
        .is_some_and(|v| !v.is_empty());
    let registry = match load_registry() {
        Ok((boxes, path)) => HealthCheck {
            ok: true,
            detail: format!("{} ({} boxes)", path.display(), boxes.len()),
        },
        Err(error) if repos_registered => HealthCheck {
            ok: true,
            detail: format!(
                "not in use — {} repos are managed directly ({error})",
                crate::repos::load_repos().len()
            ),
        },
        Err(error) if !registry_named => HealthCheck {
            ok: true,
            detail: format!("not in use — add a repository with `skein add <url>` ({error})"),
        },
        Err(error) => HealthCheck {
            ok: false,
            detail: format!(
                "{error}. It is named by $SKEIN_REGISTRY or $SKEIN_SHARED — unset it, or point \
                 it at a readable file."
            ),
        },
    };
    let fleet = fleet_boxes();
    let fleet_degraded = fleet_degraded();
    let sbx = HealthCheck {
        ok: program_on_path("sbx") && fleet.is_some() && !fleet_degraded,
        detail: match &fleet {
            Some(boxes) if fleet_degraded => format!(
                "`sbx ls` temporarily unavailable; showing last successful snapshot ({} boxes)",
                boxes.len()
            ),
            Some(boxes) => format!("available ({} boxes)", boxes.len()),
            // The failure in its own words. "installed, but `sbx ls` failed or timed out" is what
            // this said, and it is four different faults wearing one coat — the reader's next move
            // is different for each, and for the PATH one their own shell will contradict it.
            None => crate::sbx::fleet_failure()
                .unwrap_or_else(|| "no fleet listing, and no reason recorded".into()),
        },
    };
    let tool = |name: &str, required: bool| HealthCheck {
        ok: program_on_path(name) || !required,
        detail: if program_on_path(name) {
            "available".into()
        } else if required {
            "not found on PATH".into()
        } else {
            "not found (optional)".into()
        },
    };
    let git = tool("git", true);
    // What actually reads GitHub. It used to be `gh`, which made a third-party CLI a hard
    // requirement of a default-on feature and dragged its keyring in with it; the queue now talks to
    // the API with a token skein already has. curl is what carries that, and gitgate has always
    // needed it to mint App tokens.
    let gh = HealthCheck {
        ok: crate::github::have_curl(),
        detail: match crate::github::have_curl() {
            true => "available".into(),
            false => "curl is not installed — skein reads GitHub with it (pull requests, diffs, \
                      merges, and minting App tokens)"
                .into(),
        },
    };

    let repos = load_repos();
    let fleet_names = fleet.as_ref().map(|boxes| {
        boxes
            .iter()
            .map(|box_| box_.name.as_str())
            .collect::<BTreeSet<_>>()
    });
    let mut probe_errors = Vec::new();
    let mut mailbox_errors = Vec::new();
    for repo in &repos {
        let store = Path::new(&repo.store);
        for relative in [
            "skein/probe-revision",
            "skein/runtimes.tsv",
            "skein/bin/box-status.sh",
            "skein/bin/mailbox.sh",
            "skein/bin/shared-home.sh",
            "skein/bin/agent-guide.sh",
            "skein/bin/install-codex-hooks.sh",
        ] {
            if !store.join(relative).is_file() {
                probe_errors.push(format!("{} missing {relative}", repo.id));
            }
        }
        if !store.join("mailbox").is_dir() {
            mailbox_errors.push(format!("{} mailbox directory missing", repo.id));
        }
        if !store.join("shared-home").is_dir() {
            probe_errors.push(format!("{} shared-home directory missing", repo.id));
        }
        let boot_dir = store.join("skein/boot");
        if let Ok(entries) = fs::read_dir(boot_dir) {
            for path in entries.flatten().map(|entry| entry.path()) {
                let box_name = path.file_stem().and_then(|s| s.to_str()).unwrap_or("box");
                if fleet_names
                    .as_ref()
                    .is_some_and(|names| !names.contains(box_name))
                {
                    continue;
                }
                let boot = fs::read_to_string(&path)
                    .ok()
                    .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok());
                let jq_available = boot.as_ref().and_then(|value| value.get("jq")?.as_bool());
                if jq_available == Some(false) {
                    mailbox_errors.push(format!("{box_name} is missing required jq"));
                }
                let tmux_available = boot.as_ref().and_then(|value| value.get("tmux")?.as_bool());
                if tmux_available == Some(false) {
                    probe_errors.push(format!("{box_name} is missing required tmux"));
                }
                if boot
                    .as_ref()
                    .and_then(|value| value.get("shared_home")?.as_str())
                    .is_some_and(|state| state != "linked")
                {
                    probe_errors.push(format!("{box_name} shared home is unavailable"));
                }
                if boot
                    .as_ref()
                    .and_then(|value| value.get("agent_guide")?.as_str())
                    .is_some_and(|state| state != "installed")
                {
                    probe_errors.push(format!("{box_name} durable agent guidance is unavailable"));
                }
            }
        }
    }
    let mut probes = HealthCheck {
        ok: probe_errors.is_empty(),
        detail: if probe_errors.is_empty() {
            format!("installed for {} managed repos", repos.len())
        } else {
            probe_errors.join("; ")
        },
    };
    let mailbox = HealthCheck {
        ok: mailbox_errors.is_empty(),
        detail: if mailbox_errors.is_empty() {
            "shared stores and required jq available in reporting boxes".into()
        } else {
            mailbox_errors.join("; ")
        },
    };
    let views = load_views().unwrap_or_default();
    let dark_boxes = views
        .iter()
        .filter(|view| view.hook_health == "never")
        .map(|view| view.name.clone())
        .collect::<Vec<_>>();
    let stale_boxes = views
        .into_iter()
        .filter(|view| view.hook_health == "stale")
        .map(|view| view.name)
        .collect::<Vec<_>>();
    if !dark_boxes.is_empty() {
        probes.ok = false;
        probes.detail.push_str(&format!(
            "; no signals from running boxes: {}",
            dark_boxes.join(", ")
        ));
    }
    // Deliberately NOT reported here: a box on hook-only turn state (see `screen_health`) is not
    // unhealthy — it degrades to exactly its pre-observer behaviour. Nagging in the environment
    // banner would be crying wolf; the caveat belongs on the row and tab it applies to.
    let gitgate = git_scope_health();
    let ok = registry.ok
        && sbx.ok
        && git.ok
        && probes.ok
        && mailbox.ok
        && gitgate.ok
        && stale_boxes.is_empty();
    HealthReport {
        ok,
        registry,
        sbx,
        git,
        gh,
        probes,
        mailbox,
        ai,
        memory,
        gitgate,
        logins: crate::fleet::signed_in_runtimes(),
        dark_boxes,
        stale_boxes,
        runtimes: supported_runtimes(),
        git_credential: crate::gitgate::box_credential().label(),
    }
}

/// Pop the host's native folder/file picker and return the chosen absolute path (`Ok(None)` if the
/// user cancelled). `kind` is "file" → file picker, anything else → folder picker. skein-server runs
/// on the host, so this is a *real* OS dialog — which means it only works where that host has a GUI
/// (local use, not a headless / `tailscale serve` box, where the user types the path instead).
/// Best-effort across platforms: macOS `osascript`, then Linux `zenity`, then `kdialog`.
pub fn pick_path(kind: &str) -> Result<Option<String>, String> {
    let folder = kind != "file";
    let clean = |p: &str| -> Option<String> {
        let p = p.trim().trim_end_matches('/');
        (!p.is_empty()).then(|| p.to_string())
    };
    // macOS — AppleScript returns a POSIX path; a cancel exits non-zero with "User canceled".
    if cfg!(target_os = "macos") {
        let script = if folder {
            "POSIX path of (choose folder with prompt \"Pick a folder\")"
        } else {
            "POSIX path of (choose file with prompt \"Pick a file\")"
        };
        let out = std::process::Command::new("osascript")
            .arg("-e")
            .arg(script)
            .output()
            .map_err(|e| format!("osascript: {e}"))?;
        if out.status.success() {
            return Ok(clean(&String::from_utf8_lossy(&out.stdout)));
        }
        let err = String::from_utf8_lossy(&out.stderr).to_lowercase();
        if err.contains("cancel") {
            return Ok(None); // user dismissed the dialog
        }
        return Err(format!("native picker failed: {}", err.trim()));
    }
    // Linux — zenity, then kdialog. Both exit non-zero on cancel with empty stdout.
    for (bin, args) in linux_picker_argv(folder) {
        match std::process::Command::new(bin).args(&args).output() {
            Ok(out) if out.status.success() => {
                return Ok(clean(&String::from_utf8_lossy(&out.stdout)));
            }
            Ok(_) => return Ok(None), // present but cancelled
            Err(_) => continue,       // not installed → try the next
        }
    }
    Err("no native folder picker found (install zenity or kdialog, or type the path)".into())
}

pub(crate) fn linux_picker_argv(folder: bool) -> Vec<(&'static str, Vec<&'static str>)> {
    if folder {
        vec![
            ("zenity", vec!["--file-selection", "--directory"]),
            ("kdialog", vec!["--getexistingdirectory", "."]),
        ]
    } else {
        vec![
            ("zenity", vec!["--file-selection"]),
            ("kdialog", vec!["--getopenfilename", "."]),
        ]
    }
}
