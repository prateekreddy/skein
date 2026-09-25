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
/// copies under `skein/bin/`; what a box runs is skein's read-only plugin's), and wire it into
/// `settings.json`. Idempotent and safe to run on every launch — an empty folder comes up fully
/// working (memory bridge, mailbox, status line), an already-populated one is left intact (machinery
/// refreshed, settings merged additively). The user only optionally fills `memory/` and `skills/`
/// with their own content.
///
/// **A store path that is not absolute is refused, and the refusal is the fix for SKEIN-551.** See
/// [`store_freshly_created`] for what an empty one used to do.
pub fn ensure_store(store: &Path) -> Result<(), String> {
    // Said here rather than inside, because the note is about a store appearing where the person
    // did not expect one, and the only moment that is observable is the run that creates it. A
    // launch finds the store already there and stays quiet.
    if store_freshly_created(store)? {
        eprintln!(
            "skein: created a new store at {} — if that is not where this repo's data lives, its \
             boxes will come up with none of it",
            store.display()
        );
    }
    Ok(())
}

/// [`ensure_store`]'s body, reporting whether the store's own directory was **created by this
/// call** rather than found. Split out so that "announced on the run that creates it, silent on
/// every run after" is a property a test can assert twice against one path, instead of a `eprintln`
/// nothing in-process can see.
///
/// **The absolute-path check is the whole of SKEIN-551/539/483.** `fs::create_dir_all("")` is
/// `Ok(())` in Rust rather than an error, and every `store.join(d)` below is then a *relative* path,
/// so a repo whose `store` field in `repos.json` is the empty string scaffolded this entire layout
/// into whatever directory the process happened to be standing in. Measured, not reasoned: one
/// `cargo test --test server` put sixteen entries — `settings.json`, `skein/` and fourteen empty
/// directories — at this repository's own checkout root, and a `git add -A` from there once swept a
/// whole scaffold into an unrelated commit (582d017). `.gitignore`'s block of scaffold names at the
/// root was the defence, and a denylist that has to grow every time the layout does is the symptom.
///
/// Refused rather than resolved, deliberately: a repo whose store is empty loses its store and is
/// told so, which is a smaller loss than a person finding a `.claude` layout scattered through the
/// source tree they happened to be standing in, in silence.
fn store_freshly_created(store: &Path) -> Result<bool, String> {
    if !store.is_absolute() {
        return Err(format!(
            "{:?} is not an absolute path, so it is not a store: skein would resolve it against \
             whatever directory this process is standing in and scaffold a store there. A repo's \
             `store` in repos.json is a host path — give one with `skein add --store <path>`, or \
             leave it out and skein manages one under $SKEIN_HOME.",
            store.display().to_string()
        ));
    }
    let fresh = !store.exists();
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
    ensure_probe_in(store)?;
    Ok(fresh)
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

    /// Every entry in the process's working directory, so the property SKEIN-551 is about can be
    /// measured rather than described. A *set*, because the assertion below is about what appeared.
    fn working_directory_entries() -> std::collections::BTreeSet<std::ffi::OsString> {
        fs::read_dir(env::current_dir().expect("a working directory"))
            .expect("the working directory is readable")
            .flatten()
            .map(|e| e.file_name())
            .collect()
    }

    /// **The bug, stated as the property rather than as a message.** A `repos.json` entry with
    /// `"store": ""` reached `ensure_store` as `Path::new("")`; `fs::create_dir_all("")` is
    /// `Ok(())` in Rust, and every `store.join(d)` after it is then relative, so the whole layout
    /// was scaffolded into whatever directory the process was standing in. One
    /// `cargo test --test server` put sixteen entries — `settings.json`, `skein/` and fourteen
    /// empty directories — at this repository's own checkout root, and `.gitignore` carries a
    /// list of their names so a `git add -A` cannot commit them again (it already did once, at
    /// 582d017).
    ///
    /// So this counts the working directory before and after rather than matching on the error
    /// text: a message can be right while the directories are still created, and it is the
    /// directories that cost somebody a commit. The scaffold's names are deliberately not listed —
    /// a denylist that has to grow with the layout is the thing this replaces.
    ///
    /// A relative path that is not empty is here for the same reason and not a separate case: it
    /// is the same resolution against the same cwd, and `skein add --store some/dir` is how a
    /// person reaches it by hand.
    #[test]
    fn a_store_path_that_is_not_absolute_is_refused_and_creates_nothing() {
        // **`$SKEIN_HOME` is pinned for the counterfactual, not for the test.** Nothing below reads
        // it while the guard is in place — the refusal happens before a single directory is made.
        // With the guard deleted, though, the scaffold runs on into `ensure_probe_in`, which
        // publishes the sync gateway and so resolves `config::skein_home`; unset, that panics in a
        // test process (SKEIN-626) three frames below the thing being asserted, and the test then
        // fails for a reason that has nothing to do with the working directory. Pinned, the run
        // completes and the assertion below is the one that fires — which is how it was checked.
        let _g = env_lock();
        let home = tempdir();
        env::set_var("SKEIN_HOME", &home);
        env::set_var("SKEIN_FLEET_ROOT", home.join("fleet"));

        let before = working_directory_entries();
        let refusals: Vec<Result<(), String>> = ["", "store/.claude", "./.claude"]
            .iter()
            .map(|p| ensure_store(Path::new(p)))
            .collect();
        let after = working_directory_entries();
        env::remove_var("SKEIN_HOME");
        env::remove_var("SKEIN_FLEET_ROOT");

        let appeared: Vec<_> = after.difference(&before).collect();
        assert!(
            appeared.is_empty(),
            "a store path that is not absolute scaffolded into the process's own working \
             directory ({}): {appeared:?}",
            env::current_dir().unwrap_or_default().display()
        );
        for (path, got) in ["", "store/.claude", "./.claude"].iter().zip(&refusals) {
            assert!(
                got.is_err(),
                "{path:?} was accepted as a store; it would be resolved against the cwd"
            );
        }
    }

    /// The other half of SKEIN-483: a store that is genuinely created says so, and a store that
    /// was already there does not — otherwise the line is on every launch and nobody reads it.
    ///
    /// Asserted through [`store_freshly_created`] rather than by capturing stderr, because what
    /// matters is the *decision*: the `eprintln` in `ensure_store` is one line over this boolean,
    /// and a test that scraped the message could pass while the message fired every time.
    #[test]
    fn a_store_is_announced_on_the_run_that_creates_it_and_not_after() {
        let _g = env_lock();
        let home = tempdir();
        env::set_var("SKEIN_HOME", &home);
        env::set_var("SKEIN_FLEET_ROOT", home.join("fleet"));
        let store = home.join("repos/demo/store/.claude");
        let first = store_freshly_created(&store);
        let second = store_freshly_created(&store);
        env::remove_var("SKEIN_HOME");
        env::remove_var("SKEIN_FLEET_ROOT");

        assert_eq!(
            first,
            Ok(true),
            "the run that creates a store must be the one that reports it"
        );
        assert_eq!(
            second,
            Ok(false),
            "ensure_store runs on every launch, so a store that was already there must be silent"
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
        // Codex's generated hooks ship in skein's read-only plugin beside their installer, not
        // here, where any box of the repo could rewrite them for its siblings (SKEIN-1149).
        assert!(
            !store.join("skein/codex-hooks.json").exists(),
            "the store was given Codex's hook wiring, which every box of the repo can rewrite"
        );
        assert!(store.join("skein/probe-revision").is_file());
        assert!(store.join("skein/runtimes.tsv").is_file());
        // layout is documented for the user to (optionally) fill
        assert!(store.join("README.md").is_file());
        // settings carry a default statusLine and no skein hooks: those load from skein's plugin
        // (SKEIN-1062), and the store's settings.json is writable by every box of the repo
        let s: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(store.join("settings.json")).unwrap())
                .unwrap();
        assert!(s["statusLine"]["command"]
            .as_str()
            .unwrap()
            .contains("statusline-command.sh"));
        assert_eq!(s["statusLine"]["refreshIntervalMs"], 30_000);
        assert!(s.get("hooks").is_none(), "the store was given hooks: {s}");
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
        // `ensure_store` publishes the sync gateway, which reads `repos.json` — so it resolves
        // `config::skein_home`, refused rather than answered in a test since SKEIN-626. Unpinned it
        // read the owner's live `~/.skein/repos.json`, and it only passed because a neighbour in
        // this process had left `$SKEIN_HOME` set (SKEIN-646).
        let _g = env_lock();
        let skein_home = tempdir();
        env::set_var("SKEIN_HOME", &skein_home);
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
        env::remove_var("SKEIN_HOME");
    }

    #[test]
    fn agent_guide_uses_native_instruction_files_without_prompt_hook_bloat() {
        use std::os::unix::fs::symlink;

        // Same as its sibling above: `ensure_store` reads `repos.json` through the sync-gateway
        // publish, so an unpinned run reads the owner's live `~/.skein/repos.json` (SKEIN-626/646).
        let _g = env_lock();
        let skein_home = tempdir();
        env::set_var("SKEIN_HOME", &skein_home);
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
        env::remove_var("SKEIN_HOME");
    }

    #[test]
    fn a_box_with_no_clone_mount_still_gets_the_repos_shared_paths() {
        use std::os::unix::fs::symlink;
        let _g = env_lock();
        let dir = tempdir();
        // Pinned, because `config::skein_home` refuses an unpinned test rather than answering
        // with the real `~/.skein` — where this fixture's state would otherwise land (SKEIN-626).
        env::set_var("SKEIN_HOME", &dir);
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
        env::remove_var("SKEIN_HOME");
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
        // Pinned, because `config::skein_home` refuses an unpinned test rather than answering
        // with the real `~/.skein` — where this fixture's state would otherwise land (SKEIN-626).
        env::set_var("SKEIN_HOME", &dir);
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
        env::remove_var("SKEIN_HOME");
    }

    /// One box of a repo, laid out the way the fleet lays one out, for running the kit ITSELF rather
    /// than a piece of it: skein's plugin and the kit installed under the fleet root's `.skein`
    /// (`probes::plugin_install_under`'s bytes, the way `fleet::install_launcher` writes them), a
    /// store `ensure_store` scaffolded, and the box's own clone. The tracker's installer is left
    /// out, so the kit's detached network step has nothing to run.
    struct KitBox {
        dir: TempDir,
        root: PathBuf,
        store: PathBuf,
        tree: PathBuf,
        home: PathBuf,
    }

    impl KitBox {
        /// `ships`: the bytes of a `.claude/settings.json` the repo tracks (the kit's case 2), or
        /// `None` for a repo with no `.claude` at all (case 1). Call with `$SKEIN_HOME` pinned:
        /// `ensure_store` reads `repos.json` through it.
        fn new(agent: &str, ships: Option<&str>) -> KitBox {
            use std::os::unix::fs::PermissionsExt;
            let dir = tempdir();
            let root = dir.join("fleet");
            let store = dir.join("repos/web/store/.claude");
            let tree = root.join("web-main/tree");
            let home = dir.join("home");
            let mut install = crate::probes::plugin_install_under(&root.to_string_lossy());
            install.retain(|(path, _)| !path.ends_with("/sync-install.sh"));
            install.push((
                root.join(".skein/skein-startup.sh")
                    .to_string_lossy()
                    .into_owned(),
                KIT_STARTUP_SH.to_string(),
            ));
            for (path, body) in install {
                let path = Path::new(&path);
                fs::create_dir_all(path.parent().unwrap()).unwrap();
                fs::write(path, body).unwrap();
                fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
            }
            ensure_store(&store).unwrap();
            fs::write(
                store.join("skein/launch/web-main.json"),
                format!("{{\"agent\":\"{agent}\",\"branch\":\"\"}}\n"),
            )
            .unwrap();
            fs::create_dir_all(&tree).unwrap();
            fs::create_dir_all(&home).unwrap();
            let kit = KitBox {
                dir,
                root,
                store,
                tree,
                home,
            };
            kit.git(&["init", "-q"]);
            fs::write(kit.tree.join("README.md"), "a repo\n").unwrap();
            kit.git(&["add", "README.md"]);
            kit.git(&["commit", "-qm", "a repo"]);
            if let Some(settings) = ships {
                fs::create_dir_all(kit.tree.join(".claude")).unwrap();
                fs::write(kit.tree.join(".claude/settings.json"), settings).unwrap();
                kit.git(&["add", ".claude/settings.json"]);
                kit.git(&["commit", "-qm", "the repo ships its own .claude"]);
            }
            kit
        }

        /// Git in the clone, with nothing of the machine's own configuration in it: no global
        /// excludes file can hide or show anything these tests ask about.
        fn git(&self, args: &[&str]) -> String {
            let out = Command::new("git")
                .args([
                    "-c",
                    "user.name=example",
                    "-c",
                    "user.email=example@example.com",
                ])
                .arg("-C")
                .arg(&self.tree)
                .args(args)
                .env("HOME", &self.home)
                .env("XDG_CONFIG_HOME", self.home.join(".config"))
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .output()
                .unwrap();
            assert!(out.status.success(), "git {args:?}: {out:?}");
            String::from_utf8_lossy(&out.stdout).into_owned()
        }

        /// The kit, as the fleet runs it for this box.
        fn start(&self) {
            let out = Command::new("bash")
                .arg(self.root.join(".skein/skein-startup.sh"))
                .env("HOME", &self.home)
                .env("XDG_CONFIG_HOME", self.home.join(".config"))
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("SKEIN_HOME", self.dir.join("skein-home"))
                .env("SKEIN_FLEET_ROOT", &self.root)
                .env("SKEIN_PROVISION", "1")
                .env("SKEIN_BOX", "web-main")
                .env("SKEIN_STORE", &self.store)
                .env("WORKSPACE_DIR", &self.tree)
                .env("SKEIN_STARTUP_MARKERS", self.dir.join("markers"))
                .stdin(std::process::Stdio::null())
                .output()
                .unwrap();
            assert!(out.status.success(), "the kit failed: {out:?}");
        }

        fn claude(&self, rel: &str) -> PathBuf {
            self.tree.join(".claude").join(rel)
        }

        fn json(&self, rel: &str) -> serde_json::Value {
            let text =
                fs::read_to_string(self.claude(rel)).unwrap_or_else(|e| panic!("{rel}: {e}"));
            serde_json::from_str(&text).unwrap_or_else(|e| panic!("{rel}: {e}: {text}"))
        }

        fn boot(&self) -> serde_json::Value {
            serde_json::from_str(
                &fs::read_to_string(self.store.join("skein/boot/web-main.json")).unwrap(),
            )
            .unwrap()
        }

        fn exclude(&self) -> Vec<String> {
            fs::read_to_string(self.tree.join(".git/info/exclude"))
                .unwrap_or_default()
                .lines()
                .filter(|l| !l.starts_with('#'))
                .map(str::to_string)
                .collect()
        }
    }

    fn have_jq() -> bool {
        Command::new("jq").arg("--version").output().is_ok()
    }

    /// **A repo that tracks `.claude/settings.json` stays clean when a box starts in it, and skein's
    /// settings go into the box's own `settings.local.json`** (SKEIN-1048).
    ///
    /// Until SKEIN-1048 the kit's case 2 merged skein's settings into the repo's `settings.json`, so
    /// every box's tree was dirty from its first start (the 582d017 class). The repo's file here is
    /// deliberately not in jq's own formatting, so a pass that only reformats it shows as well.
    ///
    /// What would make it fail: the kit's write put back on `$rc/settings.json` (the file's bytes
    /// change and `git status` lists it); the defaults not reaching the local file (its `tui` and
    /// `statusLine` are missing); a second start changing anything (the last two assertions).
    #[test]
    fn a_repo_that_tracks_its_settings_stays_clean_and_skeins_settings_go_to_the_local_file() {
        if !have_jq() {
            return skip("no jq here, and the kit's merge is jq");
        }
        let _lock = env_lock();
        let skein_home = tempdir();
        let mut env = env_pins();
        env.set("SKEIN_HOME", &*skein_home);
        env.set("SKEIN_FLEET_ROOT", skein_home.join("fleet"));
        let tracked = "{ \"model\": \"example-model\" }\n";
        let kit = KitBox::new("claude", Some(tracked));

        kit.start();
        assert_eq!(
            fs::read_to_string(kit.claude("settings.json")).unwrap(),
            tracked,
            "the kit changed the settings file the repo tracks"
        );
        assert_eq!(
            kit.git(&["status", "--porcelain"]),
            "",
            "the box's tree is dirty"
        );
        assert_eq!(kit.boot()["claude_link"], "merged", "{}", kit.boot());
        let local = kit.json("settings.local.json");
        let defaults = crate::probes::settings_defaults();
        assert_eq!(local["tui"], defaults["settings"]["tui"], "{local}");
        assert_eq!(
            local["statusLine"], defaults["settings"]["statusLine"],
            "{local}"
        );

        let first = fs::read(kit.claude("settings.local.json")).unwrap();
        kit.start();
        assert_eq!(
            fs::read(kit.claude("settings.local.json")).unwrap(),
            first,
            "a second start changed the local settings"
        );
        assert_eq!(
            kit.git(&["status", "--porcelain"]),
            "",
            "a second start dirtied the tree"
        );
    }

    /// **The default status line a past merge copied into a repo's `settings.json` is overridden
    /// in the box's local file, without touching the repo's file; a status line or `tui` the repo
    /// chose itself is left to win** (SKEIN-1153's first case).
    ///
    /// The past default runs the store's copy of the renderer, which a sibling box can rewrite.
    /// `settings.local.json` outranks `settings.json` in Claude Code, so setting skein's current
    /// default there replaces it with no diff in a file the repo may have committed.
    ///
    /// What would make it fail: the past default not recognised (the local file gets no status
    /// line, and the store's renderer keeps running); the interval the person set beside it lost;
    /// or skein's defaults set whenever the local file has none (the repo's own status line and
    /// `tui` are then overridden: the last assertion).
    #[test]
    fn a_past_default_status_line_is_overridden_locally_and_a_repos_own_is_kept() {
        if !have_jq() {
            return skip("no jq here, and the kit's merge is jq");
        }
        let _lock = env_lock();
        let skein_home = tempdir();
        let mut env = env_pins();
        env.set("SKEIN_HOME", &*skein_home);
        env.set("SKEIN_FLEET_ROOT", skein_home.join("fleet"));
        let defaults = crate::probes::settings_defaults();
        let past = serde_json::json!({
            "statusLine": {
                "type": "command",
                "command": defaults["storeEraStatusLine"],
                "refreshIntervalMs": 5_000
            }
        })
        .to_string();
        let kit = KitBox::new("claude", Some(&past));
        kit.start();
        assert_eq!(
            fs::read_to_string(kit.claude("settings.json")).unwrap(),
            past
        );
        assert_eq!(
            kit.json("settings.local.json")["statusLine"],
            serde_json::json!({
                "type": "command",
                "command": defaults["settings"]["statusLine"]["command"],
                "refreshIntervalMs": 5_000
            }),
            "the past default status line was not overridden with skein's current one"
        );

        let own = "{\"tui\":\"default\",\"statusLine\":{\"type\":\"command\",\"command\":\"bash tools/line.sh\"}}\n";
        let kit = KitBox::new("claude", Some(own));
        kit.start();
        assert_eq!(
            fs::read_to_string(kit.claude("settings.json")).unwrap(),
            own
        );
        assert_eq!(
            kit.json("settings.local.json"),
            serde_json::json!({}),
            "the box's local settings override what the repo chose"
        );
    }

    /// **A file a contributor adds under a tracked `.claude/` shows in `git status`, and what skein
    /// puts there does not** (SKEIN-1049). The clone starts with the `/.claude` line an earlier
    /// start wrote when `.claude` was the store's link, which has to go too.
    ///
    /// What would make it fail: the kit excluding `/.claude` in case 2 (the new skill is hidden);
    /// the stale line left in place (hidden the same way); either of the two exclusions missing
    /// (`.claude/skein` or `settings.local.json` is listed).
    #[test]
    fn a_contributors_new_file_under_a_tracked_claude_shows_in_git_status() {
        if !have_jq() {
            return skip("no jq here, and the kit's merge is jq");
        }
        let _lock = env_lock();
        let skein_home = tempdir();
        let mut env = env_pins();
        env.set("SKEIN_HOME", &*skein_home);
        env.set("SKEIN_FLEET_ROOT", skein_home.join("fleet"));
        let kit = KitBox::new("claude", Some("{}\n"));
        fs::write(
            kit.tree.join(".git/info/exclude"),
            "# git's own\n*.example\n/.claude\n",
        )
        .unwrap();

        kit.start();
        assert!(
            kit.claude("skein").is_symlink() && kit.claude("settings.local.json").is_file(),
            "the kit put neither of the two things it excludes into .claude"
        );
        fs::create_dir_all(kit.claude("skills/new")).unwrap();
        fs::write(kit.claude("skills/new/SKILL.md"), "a new skill\n").unwrap();
        assert_eq!(
            kit.git(&["status", "--porcelain"]),
            "?? .claude/skills/\n",
            "a contributor's new skill is hidden, or something of skein's is listed"
        );
        assert_eq!(
            kit.exclude(),
            [
                "*.example",
                "/.claude/skein",
                "/.claude/settings.local.json",
                "/.claude/settings.json.skein-old"
            ],
            "the clone's exclude is not what skein put in .claude"
        );
    }

    /// Every store entry a box whose repo tracks nothing under `.claude` reaches through it, and the
    /// two it must not: each is a link to the store's own, and the settings files are absent.
    fn assert_store_linked_in(kit: &KitBox) {
        assert!(
            !kit.tree.join(".claude").is_symlink() && kit.tree.join(".claude").is_dir(),
            "the kit left .claude as the store's link"
        );
        let mut linked = 0;
        for entry in fs::read_dir(&kit.store).unwrap().flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') {
                continue;
            }
            let here = kit.claude(&name);
            if name == "settings.json" || name == "settings.local.json" {
                assert!(
                    !here.is_symlink(),
                    "the store's {name}, which every box can write, is linked into this box"
                );
                continue;
            }
            assert_eq!(
                fs::read_link(&here).unwrap_or_else(|e| panic!(".claude/{name}: {e}")),
                kit.store.join(&name),
                ".claude/{name} is not the store's"
            );
            linked += 1;
        }
        assert!(
            linked > 5,
            "the store had almost nothing to link, so this checked little"
        );
    }

    /// The memory directory Claude's memory tool writes, as the bootstrap bridged it for this box.
    fn memory_bridge(kit: &KitBox) -> PathBuf {
        let projects = kit.home.join(".claude/projects");
        let slugs: Vec<_> = fs::read_dir(&projects)
            .unwrap_or_else(|e| panic!("no bridge at all: {e}"))
            .flatten()
            .collect();
        assert_eq!(slugs.len(), 1, "one checkout, one bridge");
        slugs[0].path().join("memory")
    }

    fn bootstrap(kit: &KitBox) {
        let out = Command::new("bash")
            .arg(kit.store.join("skein/bin/sandbox-bootstrap.sh"))
            .env("CLAUDE_PROJECT_DIR", &kit.tree)
            .env("HOME", &kit.home)
            .env("SKEIN_BOX", "web-main")
            .env("SKEIN_FLEET_ROOT", &kit.root)
            .env_remove("SKEIN_SOURCE")
            .stdin(std::process::Stdio::null())
            .output()
            .unwrap();
        assert!(out.status.success(), "{out:?}");
    }

    /// What must outlive a conversion, byte for byte: every file under the store's `memory/` and
    /// `skills/`, and its settings file.
    fn store_digest(kit: &KitBox) -> Vec<(PathBuf, Vec<u8>)> {
        fn walk(dir: &Path, out: &mut Vec<(PathBuf, Vec<u8>)>) {
            for entry in fs::read_dir(dir).into_iter().flatten().flatten() {
                let path = entry.path();
                if path.is_dir() {
                    walk(&path, out);
                } else {
                    out.push((path.clone(), fs::read(&path).unwrap()));
                }
            }
        }
        let mut out = Vec::new();
        walk(&kit.store.join("memory"), &mut out);
        walk(&kit.store.join("skills"), &mut out);
        out.push((
            kit.store.join("settings.json"),
            fs::read(kit.store.join("settings.json")).unwrap(),
        ));
        out.sort();
        out
    }

    /// The layout every box skein made before SKEIN-1053: `.claude` is the store's own link, the
    /// clone excludes it, and the bootstrap has bridged memory through it. With a memory and a
    /// skill of the person's in the store, so there is something to lose.
    fn old_layout(kit: &KitBox) {
        fs::write(kit.store.join("memory/a-note.md"), "remember this\n").unwrap();
        fs::create_dir_all(kit.store.join("skills/theirs")).unwrap();
        fs::write(kit.store.join("skills/theirs/SKILL.md"), "a skill\n").unwrap();
        std::os::unix::fs::symlink(&kit.store, kit.tree.join(".claude")).unwrap();
        fs::write(kit.tree.join(".git/info/exclude"), "/.claude\n").unwrap();
        bootstrap(kit);
    }

    /// **A box whose repo tracks nothing under `.claude` gets a directory of its own, with each of
    /// the store's entries linked into it and the store's settings files left out** (SKEIN-1053,
    /// the owner's answer to SKEIN-1153). What would make it fail: `.claude` linked to the store
    /// whole, as before (the first assertion); either settings file linked; an entry not linked;
    /// the directory not excluded (`git status` lists it).
    #[test]
    fn a_repo_with_no_claude_gets_a_directory_of_its_own_with_the_store_linked_in() {
        if !have_jq() {
            return skip("no jq here, and the kit's merge is jq");
        }
        let _lock = env_lock();
        let skein_home = tempdir();
        let mut env = env_pins();
        env.set("SKEIN_HOME", &*skein_home);
        env.set("SKEIN_FLEET_ROOT", skein_home.join("fleet"));
        let kit = KitBox::new("claude", None);
        kit.start();
        assert_store_linked_in(&kit);
        assert_eq!(
            kit.git(&["status", "--porcelain"]),
            "",
            "what skein put in .claude is listed"
        );
        assert_eq!(kit.exclude(), ["/.claude"]);
        assert_eq!(kit.boot()["claude_link"], "linked", "{}", kit.boot());
        assert_eq!(kit.boot()["claude_note"], "", "{}", kit.boot());
        let defaults = crate::probes::settings_defaults();
        assert_eq!(
            kit.json("settings.local.json")["statusLine"],
            defaults["settings"]["statusLine"]
        );
    }

    /// **A box whose `.claude` is the store's link is converted, and keeps everything it reached
    /// through it but the store's two settings files** (SKEIN-1053).
    ///
    /// What a converted box keeps, each asserted below: every entry of the store, linked one by one
    /// (memory, skills, the mailbox, skein's machinery and the rest); every byte of the store's
    /// memory and skills and its settings file, since the conversion removes a link and never
    /// what it points at; the memory Claude's memory tool writes, whose bridge resolves into the
    /// store both before the next session's bootstrap and after it; a clean tree. What it no
    /// longer loads: the store's `settings.json` and `settings.local.json`.
    ///
    /// The boot report says so on the start that converted, and not on the next.
    ///
    /// What would make it fail: the kit leaving the link in place (the first assertion of
    /// `assert_store_linked_in`); a conversion that removed through the link rather than the link
    /// (the store's files change); the note missing, or repeated on the second start.
    #[test]
    fn a_box_whose_claude_is_the_store_link_is_converted_and_keeps_everything() {
        if !have_jq() {
            return skip("no jq here, and the kit's merge is jq");
        }
        let _lock = env_lock();
        let skein_home = tempdir();
        let mut env = env_pins();
        env.set("SKEIN_HOME", &*skein_home);
        env.set("SKEIN_FLEET_ROOT", skein_home.join("fleet"));
        let kit = KitBox::new("claude", None);
        old_layout(&kit);
        let before = store_digest(&kit);
        let bridge = memory_bridge(&kit);
        assert_eq!(
            fs::canonicalize(&bridge).unwrap(),
            fs::canonicalize(kit.store.join("memory")).unwrap(),
            "the fixture's old layout did not bridge memory into the store"
        );

        kit.start();
        assert_store_linked_in(&kit);
        assert_eq!(
            store_digest(&kit),
            before,
            "the conversion changed the store"
        );
        assert_eq!(
            fs::read_to_string(bridge.join("a-note.md")).unwrap(),
            "remember this\n",
            "the converted box's memory is not the store's"
        );
        assert_eq!(
            fs::read_to_string(kit.claude("skills/theirs/SKILL.md")).unwrap(),
            "a skill\n"
        );
        assert_eq!(kit.git(&["status", "--porcelain"]), "", "the tree is dirty");
        assert_eq!(
            kit.boot()["claude_note"],
            "converted .claude from the store's link to a directory of this box's own; the \
             store's entries are linked into it, except settings.json and settings.local.json, \
             which are now this box's own",
            "{}",
            kit.boot()
        );

        bootstrap(&kit);
        assert_eq!(
            fs::canonicalize(memory_bridge(&kit)).unwrap(),
            fs::canonicalize(kit.store.join("memory")).unwrap(),
            "the next session's bootstrap bridged memory somewhere other than the store"
        );
        kit.start();
        assert_eq!(
            kit.boot()["claude_note"],
            "",
            "the conversion was reported twice"
        );
        assert_eq!(store_digest(&kit), before);
    }

    /// **A converted box that then pulls a commit tracking `.claude/` ends up with the repo's files
    /// and the store's skein link, and nothing of the store's is touched** (SKEIN-1053's landmine).
    ///
    /// The pull replaces the links the repo's paths need: git overwrites an ignored path, and
    /// `/.claude` is excluded. The next start takes the rest of the layout-1 links out, links
    /// skein, and narrows the exclude, so the tree is clean and `git ls-files .claude` is exactly
    /// the repo's three files.
    ///
    /// What would make it fail: the layout-1 links left in place (they are listed by `git status`
    /// once the exclude narrows); a pull or start that wrote into the store (its files change).
    #[test]
    fn a_converted_box_pulls_a_tracked_claude_and_the_store_is_untouched() {
        if !have_jq() {
            return skip("no jq here, and the kit's merge is jq");
        }
        let _lock = env_lock();
        let skein_home = tempdir();
        let mut env = env_pins();
        env.set("SKEIN_HOME", &*skein_home);
        env.set("SKEIN_FLEET_ROOT", skein_home.join("fleet"));
        let kit = KitBox::new("claude", None);
        old_layout(&kit);
        let before = store_digest(&kit);
        kit.start();

        // Upstream starts tracking `.claude/`, from a checkout of its own.
        let origin = kit.dir.join("origin.git");
        let work = kit.dir.join("work");
        let git = |dir: &Path, args: &[&str]| {
            let out = Command::new("git")
                .args([
                    "-c",
                    "user.name=example",
                    "-c",
                    "user.email=example@example.com",
                ])
                .arg("-C")
                .arg(dir)
                .args(args)
                .env("HOME", &kit.home)
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .output()
                .unwrap();
            assert!(out.status.success(), "git {args:?}: {out:?}");
        };
        git(
            &kit.dir,
            &[
                "clone",
                "-q",
                "--bare",
                &kit.tree.to_string_lossy(),
                "origin.git",
            ],
        );
        git(
            &kit.dir,
            &["clone", "-q", &origin.to_string_lossy(), "work"],
        );
        for (rel, body) in [
            (".claude/README.md", "the folder\n"),
            (".claude/settings.json", "{}\n"),
            (".claude/skills/change-discipline/SKILL.md", "the checks\n"),
        ] {
            fs::create_dir_all(work.join(rel).parent().unwrap()).unwrap();
            fs::write(work.join(rel), body).unwrap();
        }
        git(&work, &["add", ".claude"]);
        git(&work, &["commit", "-qm", "track .claude"]);
        git(&work, &["push", "-q", "origin", "HEAD"]);
        kit.git(&["pull", "-q", &origin.to_string_lossy(), "HEAD"]);

        kit.start();
        assert_eq!(
            kit.git(&["ls-files", ".claude"]),
            ".claude/README.md\n.claude/settings.json\n.claude/skills/change-discipline/SKILL.md\n"
        );
        assert_eq!(
            kit.git(&["status", "--porcelain"]),
            "",
            "the tree is dirty after the repo started tracking .claude"
        );
        assert_eq!(
            fs::read_link(kit.claude("skein")).unwrap(),
            kit.store.join("skein")
        );
        assert_eq!(
            fs::read_to_string(kit.claude("skills/change-discipline/SKILL.md")).unwrap(),
            "the checks\n"
        );
        assert_eq!(store_digest(&kit), before, "the store changed");
        bootstrap(&kit);
        assert_eq!(
            fs::read_to_string(memory_bridge(&kit).join("a-note.md")).unwrap(),
            "remember this\n",
            "the box's memory is not the store's"
        );
    }

    /// **An untracked `.claude/settings.json` in a repo that ships `.claude/` is moved aside once,
    /// and nothing is deleted** (the owner's answer to SKEIN-1049's behaviour question). An older
    /// kit created that file where the repo tracked other files under `.claude/` but not that one;
    /// with the exclude narrowed it would be listed, and Claude would keep loading what an older
    /// merge copied into it from the store.
    ///
    /// What would make it fail: the file left in place (`git status` lists it); the file moved and
    /// changed, or deleted; the move reported on a start that did not move anything; a second
    /// file of that name moved over the first.
    #[test]
    fn an_untracked_settings_file_under_a_shipped_claude_is_moved_aside_once() {
        if !have_jq() {
            return skip("no jq here, and the kit's merge is jq");
        }
        let _lock = env_lock();
        let skein_home = tempdir();
        let mut env = env_pins();
        env.set("SKEIN_HOME", &*skein_home);
        env.set("SKEIN_FLEET_ROOT", skein_home.join("fleet"));
        let kit = KitBox::new("claude", None);
        fs::create_dir_all(kit.claude("commands")).unwrap();
        fs::write(kit.claude("commands/go.md"), "go\n").unwrap();
        kit.git(&["add", ".claude/commands/go.md"]);
        kit.git(&["commit", "-qm", "a tracked command"]);
        let old = "{\"statusLine\":{\"type\":\"command\",\"command\":\"an old copy\"}}\n";
        fs::write(kit.claude("settings.json"), old).unwrap();

        kit.start();
        assert_eq!(
            kit.git(&["status", "--porcelain"]),
            "",
            "the old file is listed"
        );
        assert!(
            !kit.claude("settings.json").exists(),
            "the old file is still loaded"
        );
        assert_eq!(
            fs::read_to_string(kit.claude("settings.json.skein-old")).unwrap(),
            old,
            "the old file was not kept as it was"
        );
        assert_eq!(
            kit.boot()["claude_note"],
            "moved an untracked .claude/settings.json aside to .claude/settings.json.skein-old, \
             which Claude does not load; nothing was deleted",
            "{}",
            kit.boot()
        );

        fs::write(kit.claude("settings.json"), "{\"a\":\"second\"}\n").unwrap();
        kit.start();
        assert_eq!(
            fs::read_to_string(kit.claude("settings.json.skein-old")).unwrap(),
            old,
            "a second file was moved over the first"
        );
        assert_eq!(kit.boot()["claude_note"], "", "{}", kit.boot());
    }

    /// **Every `.claude` or `$HOME` path the guide skein writes into a box names is there, in both
    /// layouts and for both runtimes** (SKEIN-1050).
    ///
    /// The guide is the kit's own output, read back from the runtime's instruction file after the
    /// kit ran. A path is every backticked token starting `.claude`, `$HOME/` or the fleet root
    /// every box is passed (`${SKEIN_FLEET_ROOT:-/boxes}/`, read as this fixture's); a bare `name/`
    /// after a backticked directory on the same line is read inside it, which is how the guide
    /// named the store's folders until SKEIN-1050 ("under `.claude`: `memory/` …"). Other paths,
    /// such as `docs/decisions/`, are the repository's to have or not, so they are not asked about.
    ///
    /// The mailbox it names is the plugin's read-only copy, not the store's `skein/bin/`, which a
    /// sibling box can rewrite: `plugin_probe_scripts_are_every_script_a_hook_runs` holds the
    /// guide's script to that, because the guide is one of the plugin's scripts.
    ///
    /// What would make it fail: the guide naming `.claude/mailbox/` or `.claude/memory/` again,
    /// which a repo that ships its own `.claude/` does not have (case 2); a path misspelt, in
    /// either case; the guide not written at all ("names no path").
    #[test]
    fn every_path_the_agent_guide_names_exists_in_both_layouts() {
        if !have_jq() {
            return skip("no jq here, and the kit's merge is jq");
        }
        let _lock = env_lock();
        let skein_home = tempdir();
        let mut env = env_pins();
        env.set("SKEIN_HOME", &*skein_home);
        env.set("SKEIN_FLEET_ROOT", skein_home.join("fleet"));
        for (agent, file) in [
            ("claude", ".claude/CLAUDE.md"),
            ("codex", ".codex/AGENTS.md"),
        ] {
            for ships in [None, Some("{}\n")] {
                let kit = KitBox::new(agent, ships);
                kit.start();
                let guide = fs::read_to_string(kit.home.join(file))
                    .unwrap_or_else(|e| panic!("{agent} got no guide: {e}"));
                let mut named = Vec::new();
                for line in guide.lines() {
                    let mut dir: Option<&str> = None;
                    for (i, token) in line.split('`').enumerate() {
                        if i % 2 == 0 {
                            continue;
                        }
                        let path = if let Some(rest) = token.strip_prefix("$HOME/") {
                            Some(kit.home.join(rest))
                        } else if let Some(rest) =
                            token.strip_prefix("${SKEIN_FLEET_ROOT:-/boxes}/")
                        {
                            Some(kit.root.join(rest.split(' ').next().unwrap()))
                        } else if token.starts_with(".claude") {
                            let path = token.split(' ').next().unwrap();
                            dir = Some(path);
                            Some(kit.tree.join(path))
                        } else if let (Some(d), false) =
                            (dir, token.trim_end_matches('/').contains('/'))
                        {
                            token.ends_with('/').then(|| kit.tree.join(d).join(token))
                        } else {
                            None
                        };
                        if let Some(path) = path {
                            named.push((token.to_string(), path));
                        }
                    }
                }
                assert!(!named.is_empty(), "{agent}'s guide names no path: {guide}");
                for (token, path) in named {
                    assert!(
                        path.exists(),
                        "{agent}'s guide names `{token}`, which a box whose repo {} does not have",
                        if ships.is_some() {
                            "ships its own .claude/"
                        } else {
                            "has no .claude"
                        }
                    );
                }
            }
        }
    }
}
