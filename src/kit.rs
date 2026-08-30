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
