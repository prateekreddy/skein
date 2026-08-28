//! The two things skein installs on the host so a box can exist: the kit, and the store.
//!
//! The **kit** is the sbx launch spec — what a new box is created from. The **store** is the
//! `.claude` directory mounted into every box of a repo, and it is shared, so scaffolding it is
//! additive by rule: `write_if_absent` never overwrites a file the user owns, and the probe
//! installer beside it (see [`crate::probes`]) is the only thing here that rewrites anything.
//!
//! The kit's provisioning script is a real file rather than a string inlined in the spec, and that
//! is not tidiness. It is spliced into a YAML block scalar where indentation *is* the syntax, one
//! line at the wrong depth ends the block early, and the failure is not a parse error — sbx writes
//! a truncated script, the box comes up with no hooks, and it looks perfectly healthy.

use crate::config::skein_home;
use crate::probes::ensure_probe_in;
use crate::repos::Repo;
use crate::util::write_atomic;
use std::fs;
use std::path::{Path, PathBuf};

const KIT_SPEC_YAML: &str = include_str!("kit/spec.yaml");

/// The provisioning script, kept as a real file rather than inline in the kit spec.
///
/// It has two callers that must not drift: sbx runs it as this kit's startup hook in a `--clone`
/// sandbox, and [`crate::fleet::provision_script`] runs the same bytes inside a box's namespace in
/// the shared sandbox. Provisioning is a dozen steps — the store link, the settings merge, the
/// branch checkout, the handoff restore, shared-home, the agent guide, the Codex hooks, the skills,
/// the boot report, the sync install — and a second implementation of them for the fleet path would
/// be a second set of ways for a box to come up looking healthy with no hooks wired.
pub(crate) const KIT_STARTUP_SH: &str = include_str!("kit/skein-startup.sh");

/// The marker line in the spec that [`kit_spec`] replaces with the script body.
const KIT_STARTUP_MARKER: &str = "        # @SKEIN_STARTUP_SCRIPT@";

/// The kit spec with the startup script spliced back into its `content:` block.
///
/// A YAML block scalar carries its indentation, so the script is re-indented to the eight spaces the
/// `content: |` level expects — and blank lines stay genuinely blank, because trailing whitespace on
/// an otherwise empty line would change the block's detected indentation.
fn kit_spec() -> String {
    let body: String = KIT_STARTUP_SH
        .lines()
        .map(|l| {
            if l.is_empty() {
                "\n".to_string()
            } else {
                format!("        {l}\n")
            }
        })
        .collect();
    KIT_SPEC_YAML
        .lines()
        .map(|l| {
            if l.starts_with(KIT_STARTUP_MARKER) {
                body.clone()
            } else {
                format!("{l}\n")
            }
        })
        .collect::<String>()
}

/// Install skein's own sbx kit into `~/.skein/kit/spec.yaml` so native launch can `--kit` it without
/// the repo shipping a kit. Embedded via `include_str!`; rewritten each call (idempotent).
pub fn ensure_kit() -> Result<PathBuf, String> {
    let kit = skein_home().join("kit");
    fs::create_dir_all(&kit).map_err(|e| format!("mkdir {}: {e}", kit.display()))?;
    let spec = kit.join("spec.yaml");
    fs::write(&spec, kit_spec()).map_err(|e| format!("write {}: {e}", spec.display()))?;
    Ok(kit)
}

/// Documents the shared-store layout for the user — written into a fresh store only when absent.
const STORE_README: &str = include_str!("store/README.md");

/// Provision a repo's shared-data folder at `store` (a `.claude` dir): scaffold the directory
/// structure (only what's missing — never clobbering data the user already put there), install skein's
/// own machinery (turn-state probe + the SessionStart bootstrap, mailbox, and a default status line,
/// all under `skein/bin/`), and wire it into `settings.json`. Idempotent and safe to run on every
/// launch — an empty folder comes up fully working (memory bridge, mailbox, status line), an
/// already-populated one is left intact (machinery refreshed, settings merged additively). The user
/// only optionally fills `memory/` and `skills/` with their own content.
/// Tell this repo's boxes where its **source tree** is, for the ones that have no
/// `/run/sandbox/source`.
///
/// Called `mirror` until this commit, and the name was the bug in miniature. A mirror is a remote —
/// `repos/<id>/mirror`, bare, what a box clones from ([`crate::repos::mirror_path`]). This is the
/// repo's working checkout, and the only thing it is for is the one job a mirror can never do:
/// **gitignored files**. A clone of any shape carries tracked files only, so the `.env` and the
/// `CLAUDE.md` a project keeps out of git are absent from every mirror however it is made, and
/// present only in somebody's checkout. Two different things under one name, and the one that
/// sounded safer was the one that could not do the job.
///
/// `sbx create --clone` is handed the checkout and mounts it read-only at `/run/sandbox/source`. A
/// fleet box has no such mount — several boxes share one sandbox, and it was created for no single
/// repo — so that whole mechanism was inert in the fleet: not dangling links, *nothing*, including
/// the `CLAUDE.md` an `gadget-demo` box gets its project direction from.
///
/// Written on every launch, so a repo whose checkout moves is not stuck with the old answer.
/// Read as a fallback only: [`seed_shared_paths`] does the copying on the host now, and a box that
/// cannot see the checkout at all still gets its files.
/// **A path that is not a directory is not recorded, and a recorded one that has gone away is
/// removed** (SKEIN-472). skein and its boxes share one sandbox now, so a directory this process
/// cannot open is one no box can open either — and writing it anyway is not a harmless stale
/// answer. `sandbox-bootstrap.sh` reads this file, every `[ -d "$source_tree" ]` guard under it
/// then fails one at a time, and the box surfaces nothing and says nothing about it. Four of nine
/// repos on the live fleet had exactly this written into every box's store on every launch.
///
/// Both names are swept, because `skein/mirror` is what this file was called before a mirror and a
/// checkout were told apart, and a box still falls back to reading it. Only a recorded path that
/// has gone away is deleted, never a file merely because this repo has no checkout — that is the
/// one case where the older file might still hold the only answer anybody has.
pub fn record_repo_source(repo: &Repo) {
    let work = repo.source_tree.trim();
    let dir = Path::new(&repo.store).join("skein");
    for name in ["source", "mirror"] {
        let recorded = fs::read_to_string(dir.join(name)).unwrap_or_default();
        let recorded = recorded.lines().next().unwrap_or_default().trim();
        if !recorded.is_empty() && !Path::new(recorded).is_dir() {
            let _ = fs::remove_file(dir.join(name));
        }
    }
    if work.is_empty() || !Path::new(work).is_dir() {
        return;
    }
    if fs::create_dir_all(&dir).is_ok() {
        let _ = write_atomic(&dir.join("source"), &dir, format!("{work}\n").as_bytes());
    }
}

/// The repo-relative paths a `shared-paths.txt` manifest names.
///
/// `<path> [rw]` with `#` comments — the same shape the box's own reader parses. One parser for the
/// two readers below, because they have to agree on what the manifest said: a warning naming a path
/// the copier would never have tried for is worse than no warning.
fn manifest_paths(manifest: &str) -> Vec<&str> {
    manifest
        .lines()
        .filter_map(|line| {
            line.split('#')
                .next()
                .unwrap_or_default()
                .split_whitespace()
                .next()
        })
        // A manifest entry is repo-relative, and a path that climbs out of the repo would copy
        // something the manifest's author did not name into a directory every box of the repo reads.
        .filter(|p| !p.is_empty() && !p.starts_with('/') && !p.split('/').any(|part| part == ".."))
        .collect()
}

/// What this repo's boxes will not have, when the only place it could come from cannot be read.
///
/// `None` in every ordinary case: a reachable checkout (the copy below is about to happen or has
/// already), a manifest naming nothing, or a manifest whose every path is already in the store —
/// which is the steady state, since seeding happens once and the store keeps it.
///
/// What is left is the case that used to be silent. These files are gitignored by definition, so no
/// clone and no mirror carries them ([`crate::repos::mirror_path`] says why at length) and the
/// checkout is the only source there has ever been. With it unreachable skein cannot tell "this
/// repo does not have that file" from "I could not look" — so the message says the paths did not
/// arrive, which is true either way, rather than claiming the repo lacks them.
fn unseeded_warning(repo: &Repo, manifest: &str) -> Option<String> {
    let work = repo.source_tree.trim();
    if !work.is_empty() && Path::new(work).is_dir() {
        return None;
    }
    let rw = Path::new(repo.store.trim()).join("shared-rw");
    let missing: Vec<&str> = manifest_paths(manifest)
        .into_iter()
        .filter(|p| !rw.join(p).exists())
        .collect();
    if missing.is_empty() {
        return None;
    }
    let why = match work.is_empty() {
        true => "no source tree is recorded for it".to_string(),
        false => format!("its source tree ({work}) cannot be read from here"),
    };
    Some(format!(
        "skein: {id}'s boxes will not see {list} — named in shared-paths.txt, absent from {rw}, and {why}. \
         These are files git does not carry, so no clone brings them: copy them into that directory, \
         or drop the entries if this repo does not have them.",
        id = repo.id,
        list = missing.join(", "),
        rw = rw.display(),
    ))
}

/// Copy the repo's gitignored shared paths out of its checkout and into the store, on the **host**.
///
/// The box used to do this itself, reading the host's checkout through a read-only bind. Same
/// destination and the same once-only semantics — the copy lands in `<store>/shared-rw/<path>` and
/// the box symlinks it from there — but the reading end moves to the side that legitimately has the
/// file. What that buys is the bind: a box needs the host's working tree mounted into the sandbox
/// *only* for this, and nothing else it does requires it.
///
/// **Once, never a refresh.** `shared-rw/` is writable and live across every box of the repo, so a
/// second copy would overwrite whatever a box put there — an `.env` edited in a box, gone at the
/// next launch, with nothing to say why. A path that should be re-seeded is deleted from the store
/// deliberately.
///
/// Best-effort, and quiet about the ordinary case: a manifest that names a path this repo does not
/// have is how a shared manifest works across repos, not an error.
///
/// **Quiet is not the same as silent, and it used to be** (SKEIN-472). An unreachable checkout
/// returned without a word, on every launch, for every repo whose checkout is not in the fleet —
/// and the box end is guarded too, so a repo whose manifest named `.env` and `CLAUDE.md` got
/// neither of them and nothing anywhere said so. [`unseeded_warning`] is that sentence.
pub fn seed_shared_paths(repo: &Repo) {
    let work = Path::new(repo.source_tree.trim());
    let store = Path::new(repo.store.trim());
    let Ok(manifest) = fs::read_to_string(store.join("shared-paths.txt")) else {
        return;
    };
    if repo.source_tree.trim().is_empty() || !work.is_dir() {
        // Once per launch, because this is called once per launch — from
        // [`crate::fleet::start_box_inner`] and from [`crate::sandbox::repo_launch_command_as`],
        // each of which brings up one box.
        if let Some(warning) = unseeded_warning(repo, &manifest) {
            eprintln!("{warning}");
        }
        return;
    }
    for path in manifest_paths(&manifest) {
        let from = work.join(path);
        let to = store.join("shared-rw").join(path);
        if to.exists() || !from.exists() {
            continue;
        }
        let Some(parent) = to.parent() else { continue };
        if fs::create_dir_all(parent).is_err() {
            continue;
        }
        // `cp -a`, not a read-and-write: an entry may name a directory, and preserving modes
        // matters for an `.env` that arrives 0600.
        let copied = std::process::Command::new("cp")
            .arg("-a")
            .arg(&from)
            .arg(&to)
            .status();
        if !matches!(copied, Ok(status) if status.success()) {
            eprintln!(
                "skein: could not seed {} into {}'s store, so its boxes will not see it",
                path, repo.id
            );
        }
    }
}

pub fn ensure_store(store: &Path) -> Result<(), String> {
    fs::create_dir_all(store).map_err(|e| format!("mkdir {}: {e}", store.display()))?;
    // skein-owned runtime (skein/, mailbox/, status/, tasks/) + the user-filled content homes
    // (memory/, skills/, hooks/). create_dir_all is idempotent, so existing dirs are untouched.
    for d in [
        "mailbox",
        "status",
        "tasks",
        "journals",
        "telemetry",
        // durable Claude <-> Codex takeover briefs plus one-shot per-runtime pending copies
        "handoffs",
        // narrative signal per box (box-session.sh): the headline / fork-detector / digest source
        "sessions",
        // per-box hook heartbeats (every probe appends one line per firing) — how the cockpit
        // distinguishes "hooks broken" from "box quiet"; see hook_health in load_views
        "hook-log",
        "skein/launch",
        "skein/bin",
        "memory",
        "skills",
        "hooks",
        // RW-surfaced shared paths (shared-paths.txt entries marked `rw`) live here — the store is
        // a genuinely writable host directory, unlike the RO clone-mode source mirror. See
        // sandbox-bootstrap.sh's surfacing loop.
        "shared-rw",
        // Project-scoped durable user workspace, surfaced as $HOME/shared in every box. Real $HOME
        // remains private so credentials, caches, and concurrent runtime state cannot collide.
        "shared-home",
    ] {
        let p = store.join(d);
        fs::create_dir_all(&p).map_err(|e| format!("mkdir {}: {e}", p.display()))?;
    }
    // Document the layout so the user knows what they can optionally add — written only if absent.
    write_if_absent(&store.join("README.md"), STORE_README);
    ensure_probe_in(store)
}

/// Write `body` to `path` only when nothing is there yet — so scaffolding never overwrites the user's
/// own files. Best-effort: a write failure is logged, not fatal.
fn write_if_absent(path: &Path, body: &str) {
    if path.exists() {
        return;
    }
    if let Err(e) = fs::write(path, body) {
        eprintln!("skein: write {}: {e}", path.display());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::*;
    use std::env;
    use std::process::Command;

    // The provisioning script is one file with two callers — the kit hook and the fleet path — and
    // the kit's copy is spliced into a YAML block scalar, where indentation IS the syntax. A line
    // that lands at the wrong depth ends the block early, and the failure is not a parse error: sbx
    // writes a truncated script, the box comes up with no hooks, and it looks perfectly healthy.
    #[test]
    fn the_kit_carries_the_same_provisioning_script_the_fleet_runs() {
        let spec = kit_spec();
        assert!(
            !spec.contains("@SKEIN_STARTUP_SCRIPT@"),
            "the marker survived, so the kit would install a script that is only a comment"
        );
        // Every line of the script, at the block's indentation — including the ones the shell needs
        // at column 0 and the ones already indented inside it.
        for line in KIT_STARTUP_SH.lines().filter(|l| !l.is_empty()) {
            assert!(
                spec.contains(&format!("\n        {line}\n")),
                "not spliced at the block's depth: {line:?}"
            );
        }
        // A blank line carrying eight spaces would deepen the block's detected indentation and take
        // the rest of the script with it.
        assert!(
            !spec.contains("\n        \n"),
            "a blank line was padded, which re-indents everything after it"
        );
        assert!(
            spec.contains("\n  startup:\n"),
            "the splice must not disturb what follows the block"
        );
    }

    #[test]
    fn ensure_store_and_kit_provision_layout() {
        let _g = env_lock();
        let home = tempdir();
        env::set_var("SKEIN_HOME", &home);
        let store = home.join("repos").join("x").join("store").join(".claude");
        ensure_store(&store).unwrap();
        // the full structure: skein runtime + the user-filled content homes.
        for d in [
            "mailbox",
            "status",
            "tasks",
            "journals",
            "handoffs",
            "skein/launch",
            "skein/bin",
            "memory",
            "skills",
            "hooks",
            "shared-rw",
            "shared-home",
        ] {
            assert!(store.join(d).is_dir(), "missing {d}");
        }
        // skein installs all the machinery so an empty store works end-to-end
        for f in [
            "skein/bin/box-status.sh",
            "skein/bin/box-journal.sh",
            "skein/bin/box-token-usage.sh",
            "skein/bin/box-codex-task.sh",
            "skein/bin/box-codex-telemetry.sh",
            "skein/bin/box-handoff.sh",
            "skein/bin/sandbox-bootstrap.sh",
            "skein/bin/shared-home.sh",
            "skein/bin/agent-guide.sh",
            "skein/bin/install-codex-hooks.sh",
            "skein/SHARED-HOME.md",
            "skein/bin/mailbox.sh",
            "skein/bin/statusline-command.sh",
            // Work tracking rides the store, not the kit — that is what lets a box created before
            // the feature existed still be wired up.
            "skein/bin/sync-install.sh",
            "skein/sync/work-tracking.block.md",
            "skein/sync/work-tracking.memory.md",
            "skein/sync/work-tracking.skill.md",
            "skein/sync/work-tracking.organising.md",
            "skein/sync/work-tracking.troubleshooting.md",
        ] {
            assert!(store.join(f).is_file(), "missing {f}");
        }
        assert!(store.join("settings.json").is_file());
        assert!(store.join("skein/codex-hooks.json").is_file());
        assert!(store.join("skein/probe-revision").is_file());
        assert!(store.join("skein/runtimes.tsv").is_file());
        // layout is documented for the user to (optionally) fill
        assert!(store.join("README.md").is_file());
        // settings wire the SessionStart bootstrap + a default statusLine
        let s: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(store.join("settings.json")).unwrap())
                .unwrap();
        assert!(s["statusLine"]["command"]
            .as_str()
            .unwrap()
            .contains("statusline-command.sh"));
        assert_eq!(s["statusLine"]["refreshIntervalMs"], 30_000);
        assert!(s["hooks"]["SessionStart"][0]["hooks"][0]["command"]
            .as_str()
            .unwrap()
            .contains("sandbox-bootstrap.sh"));
        let kit = ensure_kit().unwrap();
        assert!(kit.join("spec.yaml").is_file());
        let kit_text = fs::read_to_string(kit.join("spec.yaml")).unwrap();
        assert!(
            !kit_text.contains("${"),
            "sbx treats dollar-brace shell expansions as kit placeholders"
        );
        env::remove_var("SKEIN_HOME");
    }

    #[test]
    fn ensure_store_scaffolds_without_clobbering_user_data() {
        let _g = env_lock();
        let home = tempdir();
        env::set_var("SKEIN_HOME", &home);
        let store = home.join("shared").join(".claude");

        // the user pre-populates the folder with their own data + a custom README.
        fs::create_dir_all(store.join("memory")).unwrap();
        fs::write(store.join("memory").join("mine.md"), "user memory").unwrap();
        fs::write(store.join("README.md"), "MY OWN README").unwrap();

        ensure_store(&store).unwrap();

        // scaffolding added the missing structure + machinery …
        assert!(store.join("skills").is_dir());
        assert!(store.join("skein/bin/box-status.sh").is_file());
        assert!(store.join("skein/bin/sandbox-bootstrap.sh").is_file());
        // … but never clobbered what the user already put there.
        assert_eq!(
            fs::read_to_string(store.join("memory").join("mine.md")).unwrap(),
            "user memory"
        );
        assert_eq!(
            fs::read_to_string(store.join("README.md")).unwrap(),
            "MY OWN README",
            "an existing README is left alone"
        );
        env::remove_var("SKEIN_HOME");
    }

    #[test]
    fn shared_home_links_two_private_homes_and_refuses_real_path() {
        let store_tmp = tempdir();
        let store = store_tmp.join("store/.claude");
        ensure_store(&store).unwrap();
        let helper = store.join("skein/bin/shared-home.sh");
        let home_a_tmp = tempdir();
        let home_a = home_a_tmp.join("home-a");
        let home_b_tmp = tempdir();
        let home_b = home_b_tmp.join("home-b");
        fs::create_dir_all(&home_a).unwrap();
        fs::create_dir_all(&home_b).unwrap();

        let run = |home: &Path| {
            Command::new("bash")
                .arg(&helper)
                .arg(&store)
                .env("HOME", home)
                .output()
                .unwrap()
        };
        assert!(run(&home_a).status.success());
        assert!(run(&home_b).status.success());
        assert_eq!(
            fs::read_link(home_a.join("shared")).unwrap(),
            store.join("shared-home")
        );
        assert_eq!(
            fs::read_link(home_b.join("shared")).unwrap(),
            store.join("shared-home")
        );

        fs::write(home_a.join("shared/from-a.txt"), "visible in b").unwrap();
        assert_eq!(
            fs::read_to_string(home_b.join("shared/from-a.txt")).unwrap(),
            "visible in b"
        );
        fs::write(home_a.join("private-sentinel"), "private").unwrap();
        assert!(!home_b.join("private-sentinel").exists());

        fs::remove_file(home_b.join("shared")).unwrap();
        fs::create_dir(home_b.join("shared")).unwrap();
        fs::write(home_b.join("shared/do-not-clobber"), "mine").unwrap();
        let conflict = run(&home_b);
        assert!(!conflict.status.success());
        assert!(String::from_utf8_lossy(&conflict.stderr).contains("refusing to replace real path"));
        assert_eq!(
            fs::read_to_string(home_b.join("shared/do-not-clobber")).unwrap(),
            "mine"
        );
    }

    #[test]
    fn agent_guide_uses_native_instruction_files_without_prompt_hook_bloat() {
        use std::os::unix::fs::symlink;

        let store_tmp = tempdir();
        let store = store_tmp.join("store/.claude");
        let home_tmp = tempdir();
        let home = home_tmp.join("home");
        let work_tmp = tempdir();
        let work = work_tmp.join("work");
        ensure_store(&store).unwrap();
        fs::create_dir_all(home.join(".codex")).unwrap();
        fs::create_dir_all(&work).unwrap();
        symlink(&store, work.join(".claude")).unwrap();
        fs::write(home.join(".codex/AGENTS.md"), "# My existing guidance\n").unwrap();
        let helper = store.join("skein/bin/agent-guide.sh");
        let run = |normal: &str, override_: &str| {
            Command::new("bash")
                .arg(&helper)
                .arg(&store)
                .arg(normal)
                .arg(override_)
                .env("HOME", &home)
                .output()
                .unwrap()
        };

        assert!(run(".codex/AGENTS.md", ".codex/AGENTS.override.md")
            .status
            .success());
        assert!(run(".codex/AGENTS.md", ".codex/AGENTS.override.md")
            .status
            .success());
        let agents = fs::read_to_string(home.join(".codex/AGENTS.md")).unwrap();
        assert!(agents.contains("My existing guidance"));
        assert_eq!(agents.matches("skein:shared-home:start").count(), 1);

        fs::write(
            home.join(".codex/AGENTS.override.md"),
            "# My temporary override\n",
        )
        .unwrap();
        assert!(run(".codex/AGENTS.md", ".codex/AGENTS.override.md")
            .status
            .success());
        let override_ = fs::read_to_string(home.join(".codex/AGENTS.override.md")).unwrap();
        assert!(override_.contains("My temporary override"));
        assert_eq!(override_.matches("skein:shared-home:start").count(), 1);

        assert!(run(".claude/CLAUDE.md", "").status.success());
        assert!(fs::read_to_string(home.join(".claude/CLAUDE.md"))
            .unwrap()
            .contains("$HOME/shared"));

        // Without a real takeover, the turn-scoped handoff hook must emit no context at all.
        let handoff = Command::new("bash")
            .arg(store.join("skein/bin/box-handoff.sh"))
            .arg("codex")
            .env("CLAUDE_PROJECT_DIR", &work)
            .env("SANDBOX_VM_ID", "box-a")
            .output()
            .unwrap();
        assert!(handoff.status.success());
        assert!(handoff.stdout.is_empty());
    }

    #[test]
    fn a_box_with_no_clone_mount_still_gets_the_repos_shared_paths() {
        use std::os::unix::fs::symlink;
        let _g = env_lock();
        let dir = tempdir();
        let store = dir.join("store").join(".claude");
        let work = dir.join("work"); // the host checkout: what /run/sandbox/source used to be
        let tree = dir.join("tree"); // the box's own clone
        ensure_store(&store).unwrap();
        for d in [&work, &tree] {
            fs::create_dir_all(d).unwrap();
        }
        fs::write(work.join(".env"), "SECRET=from-host\n").unwrap();
        fs::write(work.join("CLAUDE.md"), "# direction\n").unwrap();
        fs::write(store.join("shared-paths.txt"), ".env\nCLAUDE.md\n").unwrap();
        fs::write(
            store.join("skein").join("source"),
            format!("{}\n", work.display()),
        )
        .unwrap();
        assert!(Command::new("git")
            .args(["init", "-q"])
            .arg(&tree)
            .status()
            .unwrap()
            .success());
        symlink(&store, tree.join(".claude")).unwrap();
        // The wreckage an earlier migration left: a link into a mount this box does not have.
        symlink("/run/sandbox/source/.env", tree.join(".env")).unwrap();

        let home = dir.join("home");
        fs::create_dir_all(&home).unwrap();
        let out = Command::new("bash")
            .arg(store.join("skein/bin/sandbox-bootstrap.sh"))
            .env("CLAUDE_PROJECT_DIR", &tree)
            .env("HOME", &home)
            .env("SKEIN_BOX", "demo-main")
            // This box may itself be clone-mode, so name the source tree rather than letting the
            // script find the harness's own /run/sandbox/source.
            .env("SKEIN_SOURCE", &work)
            .stdin(std::process::Stdio::null())
            .output()
            .unwrap();
        assert!(out.status.success(), "{out:?}");

        for name in [".env", "CLAUDE.md"] {
            let dst = tree.join(name);
            let target = fs::read_link(&dst).unwrap_or_else(|e| panic!("{name}: {e}"));
            // Canonical, because the box reaches its store through a symlink: the same directory
            // has two spellings and only one of them is the one written here.
            let target = fs::canonicalize(&target).unwrap();
            assert!(
                target.starts_with(fs::canonicalize(store.join("shared-rw")).unwrap()),
                "{name} must resolve through the store, never straight at the host checkout: {}",
                target.display()
            );
            assert!(
                fs::read_to_string(&dst).unwrap().contains("from-host") || name == "CLAUDE.md",
                "{name} must carry the host's content"
            );
        }
        // The point of routing through the store: writing here must not touch the host checkout.
        fs::write(tree.join(".env"), "SECRET=changed\n").unwrap();
        assert_eq!(
            fs::read_to_string(work.join(".env")).unwrap(),
            "SECRET=from-host\n",
            "a box must never be able to edit the host's own working copy"
        );
    }

    /// A box that cannot see the repo's source tree at all still gets its gitignored shared paths.
    ///
    /// This is the destination, and the one the old shape could not reach. The box used to read the
    /// user's working checkout through a read-only bind — which is why that bind exists, and the
    /// only reason it exists. `seed_shared_paths` moves the reading end to the host, so the box
    /// works from its store and the tree its user works in need never be mounted.
    ///
    /// `$SKEIN_SOURCE` is pointed at a path that does not exist, which is what a box in a fleet
    /// with no source bind actually sees.
    #[test]
    fn a_box_that_cannot_see_the_source_tree_still_gets_its_shared_paths() {
        use std::os::unix::fs::symlink;
        let _g = env_lock();
        let dir = tempdir();
        let store = dir.join("store").join(".claude");
        let work = dir.join("work"); // the host checkout — reachable HERE, and not from the box
        let tree = dir.join("tree"); // the box's own clone
        ensure_store(&store).unwrap();
        for d in [&work, &tree] {
            fs::create_dir_all(d).unwrap();
        }
        fs::write(work.join(".env"), "SECRET=from-host\n").unwrap();
        fs::create_dir_all(work.join("config")).unwrap();
        fs::write(work.join("config").join("local.yaml"), "k: v\n").unwrap();
        fs::write(
            store.join("shared-paths.txt"),
            ".env\nconfig rw\nabsent.txt\n",
        )
        .unwrap();

        // The host end: skein copies what the manifest names into the store.
        let repo = Repo {
            read_prs: false,
            id: "demo".into(),
            source: work.to_string_lossy().into_owned(),
            source_tree: work.to_string_lossy().into_owned(),
            store: store.to_string_lossy().into_owned(),
            agent: "claude".into(),
            plane_project: String::new(),
            sync_connection: String::new(),
            review_queue: true,
            sync_gateway_url: String::new(),
        };
        seed_shared_paths(&repo);
        assert_eq!(
            fs::read_to_string(store.join("shared-rw").join(".env")).unwrap(),
            "SECRET=from-host\n"
        );
        assert!(store
            .join("shared-rw")
            .join("config")
            .join("local.yaml")
            .is_file());
        assert!(
            !store.join("shared-rw").join("absent.txt").exists(),
            "a manifest naming a path this repo does not have is how a shared manifest works"
        );

        // The box end: no source tree in sight.
        assert!(Command::new("git")
            .args(["init", "-q"])
            .arg(&tree)
            .status()
            .unwrap()
            .success());
        symlink(&store, tree.join(".claude")).unwrap();
        let home = dir.join("home");
        fs::create_dir_all(&home).unwrap();
        let out = Command::new("bash")
            .arg(store.join("skein/bin/sandbox-bootstrap.sh"))
            .env("CLAUDE_PROJECT_DIR", &tree)
            .env("HOME", &home)
            .env("SKEIN_BOX", "demo-main")
            .env("SKEIN_SOURCE", dir.join("no-such-checkout"))
            .stdin(std::process::Stdio::null())
            .output()
            .unwrap();
        assert!(out.status.success(), "{out:?}");

        for (name, body) in [
            (".env", "SECRET=from-host\n"),
            ("config/local.yaml", "k: v\n"),
        ] {
            let dst = tree.join(name);
            assert_eq!(
                fs::read_to_string(&dst).unwrap_or_default(),
                body,
                "{name} did not reach the box, and no source tree means nothing to fall back on"
            );
        }
        // Through the store, as always — there is nowhere else it could have come from.
        let target = fs::canonicalize(fs::read_link(tree.join(".env")).unwrap()).unwrap();
        assert!(target.starts_with(fs::canonicalize(store.join("shared-rw")).unwrap()));

        // And seeding twice does not overwrite what a box has since written.
        fs::write(tree.join(".env"), "SECRET=edited-in-a-box\n").unwrap();
        seed_shared_paths(&repo);
        assert_eq!(
            fs::read_to_string(store.join("shared-rw").join(".env")).unwrap(),
            "SECRET=edited-in-a-box\n",
            "a re-seed threw away what a box had put there"
        );
    }

    /// A repo whose checkout is unreachable **says which files did not arrive**, and stops writing
    /// the dead path into every box's store (SKEIN-472).
    ///
    /// Four of nine repos on the live fleet are in this state, and every part of the path was quiet
    /// about it: `seed_shared_paths` returned without a word, `record_repo_source` wrote the dead
    /// path on every launch, and the box's own guards then failed one at a time. The warning is
    /// asserted through [`unseeded_warning`] rather than by reading stderr, because the two
    /// silences worth testing are the ones it must NOT break — a healthy repo, and a repo already
    /// seeded — and those are absences a printed line cannot demonstrate.
    #[test]
    fn a_repo_whose_checkout_is_unreachable_says_what_did_not_arrive() {
        let _g = env_lock();
        let dir = tempdir();
        let store = dir.join("store").join(".claude");
        let work = dir.join("work");
        ensure_store(&store).unwrap();
        fs::create_dir_all(&work).unwrap();
        fs::write(work.join(".env"), "SECRET=from-host\n").unwrap();
        fs::write(work.join("CLAUDE.md"), "direction\n").unwrap();
        let manifest = ".env\nCLAUDE.md\n# a comment\n";
        fs::write(store.join("shared-paths.txt"), manifest).unwrap();

        let repo = |tree: &Path| Repo {
            read_prs: false,
            id: "demo".into(),
            source: "https://github.com/acme/demo.git".into(),
            source_tree: tree.to_string_lossy().into_owned(),
            store: store.to_string_lossy().into_owned(),
            agent: "claude".into(),
            plane_project: String::new(),
            sync_connection: String::new(),
            review_queue: true,
            sync_gateway_url: String::new(),
        };
        let gone = dir.join("no-such-checkout");
        let dead = repo(&gone);

        let warning = unseeded_warning(&dead, manifest).unwrap_or_default();
        for named in [".env", "CLAUDE.md", &gone.to_string_lossy()] {
            assert!(
                warning.contains(&*named),
                "a box will come up without {named} and this is all anyone is told: {warning:?}"
            );
        }
        // And the two silences it must not cost. A repo whose checkout is right there is about to
        // be seeded from it, and a repo already seeded is the steady state of every launch after
        // the first — a line on either would be a false alarm on every launch, for ever.
        assert_eq!(
            unseeded_warning(&repo(&work), manifest),
            None,
            "a healthy repo was warned about"
        );
        seed_shared_paths(&repo(&work));
        assert!(store.join("shared-rw").join(".env").is_file());
        assert_eq!(
            unseeded_warning(&dead, manifest),
            None,
            "the files are in the store, so nothing failed to arrive"
        );

        // The recorded path: a dead one is removed rather than rewritten, under both names, so the
        // box stops reading a directory that is not there.
        let skein = store.join("skein");
        for name in ["source", "mirror"] {
            fs::write(skein.join(name), format!("{}\n", gone.display())).unwrap();
        }
        record_repo_source(&dead);
        for name in ["source", "mirror"] {
            assert!(
                !skein.join(name).exists(),
                "skein/{name} still names a directory no box can open"
            );
        }
        record_repo_source(&repo(&work));
        assert_eq!(
            fs::read_to_string(skein.join("source")).unwrap().trim(),
            work.to_string_lossy(),
            "a reachable checkout must still be recorded — it is the box's fallback"
        );
    }

    /// The box reads the first recorded source path that is **there**, not the first one written.
    ///
    /// `skein/mirror` is what `skein/source` was called before a mirror and a checkout were told
    /// apart, and the box still falls back to it. The fallback was unreachable in the one case it
    /// exists for: a `skein/source` naming a directory that has gone away is non-empty, so it won
    /// the `[ -n ]` test and then failed every `-d` test below it, and the box surfaced nothing.
    #[test]
    fn a_box_skips_a_recorded_source_path_that_is_not_there() {
        let _g = env_lock();
        let dir = tempdir();
        let store = dir.join("store").join(".claude");
        let work = dir.join("work");
        let tree = dir.join("tree");
        ensure_store(&store).unwrap();
        for d in [&work, &tree] {
            fs::create_dir_all(d).unwrap();
        }
        fs::write(work.join(".env"), "SECRET=from-host\n").unwrap();
        fs::write(store.join("shared-paths.txt"), ".env\n").unwrap();
        // Nothing seeded into shared-rw: the store is the box's first choice, and this test is
        // about the fallback it takes when the store has nothing.
        assert!(!store.join("shared-rw").join(".env").exists());
        fs::write(
            store.join("skein").join("source"),
            format!("{}\n", dir.join("no-such-checkout").display()),
        )
        .unwrap();
        fs::write(
            store.join("skein").join("mirror"),
            format!("{}\n", work.display()),
        )
        .unwrap();

        assert!(Command::new("git")
            .args(["init", "-q"])
            .arg(&tree)
            .status()
            .unwrap()
            .success());
        std::os::unix::fs::symlink(&store, tree.join(".claude")).unwrap();
        let home = dir.join("home");
        fs::create_dir_all(&home).unwrap();
        let out = Command::new("bash")
            .arg(store.join("skein/bin/sandbox-bootstrap.sh"))
            .env("CLAUDE_PROJECT_DIR", &tree)
            .env("HOME", &home)
            .env("SKEIN_BOX", "demo-main")
            // Unset on purpose: this test is about the recorded paths, and $SKEIN_SOURCE and the
            // clone-mode bind both come first.
            .env_remove("SKEIN_SOURCE")
            .stdin(std::process::Stdio::null())
            .output()
            .unwrap();
        assert!(out.status.success(), "{out:?}");
        assert_eq!(
            fs::read_to_string(tree.join(".env")).unwrap_or_default(),
            "SECRET=from-host\n",
            "a dead path recorded under the newer name hid a live one under the older"
        );
    }
}
