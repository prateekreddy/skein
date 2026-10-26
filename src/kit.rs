//! The two things skein installs on the host so a box can exist: the kit, and the store.
//!
//! The **kit** is the sbx launch spec — what a new box is created from. The **store** is the
//! directory mounted into every box of a repo (`repos/<id>/store/data`), and it is shared, so
//! scaffolding it is additive by rule: `write_if_absent` never overwrites a file the user owns, and
//! the probe installer beside it (see [`crate::probes`]) is the only thing here that rewrites
//! anything.
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

/// Provision a repo's shared-data folder at `store`: scaffold the directory
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
    // A managed store still named `.claude` whose `data` is already there was moved after this
    // caller read `repos.json`. Making the old one again would leave the repo two stores, and the
    // next move would refuse them both (SKEIN-1330).
    if store_has_old_name(store) && !store.exists() && store.with_file_name(STORE_NAME).is_dir() {
        return Err(format!(
            "{} moved to {}; this caller read the repo list before the move, so nothing was made \
             at the old name. Try again.",
            store.display(),
            store.with_file_name(STORE_NAME).display()
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
    ] {
        let p = store.join(d);
        fs::create_dir_all(&p).map_err(|e| format!("mkdir {}: {e}", p.display()))?;
    }
    // Document the layout so the user knows what they can optionally add — written only if absent.
    write_if_absent(&store.join("README.md"), STORE_README);
    ensure_probe_in(store)?;
    // Last, so that a refusal here costs the repo its `$HOME/shared` and nothing else: every other
    // part of the store above is already in place, and the launch that called this goes on.
    ensure_shared_home(store)?;
    Ok(fresh)
}

/// Where a store's shared working files live — the directory every box of the repo links
/// `$HOME/shared` to.
///
/// For a store skein manages, `$SKEIN_HOME/repos/<id>/store/data`, that is
/// `$SKEIN_HOME/repos/<id>/store/shared-home`: **beside the store, not inside it**. Decided by the
/// path's shape, `/<…>/repos/<id>/store/data` ([`shared_home_beside`]), or `store/.claude`, the
/// name the store had until SKEIN-1330 and still has in a box started before it moved. Claude Code
/// guards every path under a `.claude` directory as its own configuration and judges the resolved
/// path, so while the files lived at `store/.claude/shared-home` every write to `~/shared` asked for
/// permission (SKEIN-1324, `docs/decisions/shared-home-outside-claude.md`).
///
/// Any other store keeps `<store>/shared-home`. A store put elsewhere with `skein add --store` is
/// mounted into the fleet on its own (`fleet::fleet_mounts`), so nothing beside it reaches a box,
/// and its parent is a directory of the person's. The box's half of this rule is
/// `skein_shared_home` in `src/probe/box-self.sh`, which knows the shape but not `$SKEIN_HOME`.
pub(crate) fn shared_home_dir(store: &Path) -> PathBuf {
    shared_home_beside(store).unwrap_or_else(|| store.join("shared-home"))
}

/// [`shared_home_dir`] when it is the sibling: `Some` only for a store of the shape skein gives
/// the stores it manages ([`managed_store_name`]).
///
/// **The shape, and not "under `$SKEIN_HOME`"**, because the box decides from the path alone
/// (`skein_shared_home` in `src/probe/box-self.sh` cannot know `$SKEIN_HOME`), and a host that
/// answered differently for the same path would leave the box asking for a directory the host
/// never made.
fn shared_home_beside(store: &Path) -> Option<PathBuf> {
    managed_store_name(store)?;
    Some(store.parent()?.join("shared-home"))
}

/// The name a managed store sits under in `repos/<id>/store/`, since SKEIN-1330.
///
/// `data` and not `.claude`: Claude Code treats any path that is, or resolves to, something under a
/// `.claude` directory as its own configuration, and asks before every write there however it is
/// allowed (`docs/decisions/store-is-not-dot-claude.md`).
pub(crate) const STORE_NAME: &str = "data";

/// The name the store had before [`STORE_NAME`], which [`move_store_off_dot_claude`] moves from.
const OLD_STORE_NAME: &str = ".claude";

/// Which of the two names a store of the managed shape has: `Some` only for
/// `/<…>/repos/<id>/store/data` or `/<…>/repos/<id>/store/.claude`, with no `.` or `..` and no
/// doubled `/`. The box's copy of this rule is `skein_shared_home` in `src/probe/box-self.sh`, and
/// the launcher's is its bind of `store/shared-home` in `src/box-session.sh`.
pub(crate) fn managed_store_name(store: &Path) -> Option<&'static str> {
    use std::path::Component;
    if store.to_string_lossy().contains("//") {
        return None;
    }
    let parts = store.components().collect::<Vec<_>>();
    let (Component::RootDir, rest) = parts.split_first()? else {
        return None;
    };
    if !rest.iter().all(|c| matches!(c, Component::Normal(_))) {
        return None;
    }
    match rest {
        [_, .., Component::Normal(r), Component::Normal(_), Component::Normal(s), Component::Normal(c)]
            if *r == "repos" && *s == "store" =>
        {
            [STORE_NAME, OLD_STORE_NAME].into_iter().find(|n| c == n)
        }
        _ => None,
    }
}

/// Whether `store` is a managed store still at its old name, `/<…>/repos/<id>/store/.claude` — the
/// one shape [`move_store_off_dot_claude`] moves. Asks nothing of the filesystem.
pub(crate) fn store_has_old_name(store: &Path) -> bool {
    managed_store_name(store) == Some(OLD_STORE_NAME)
}

/// Move a managed store from `store/.claude` to `store/data`, once, and say where it is now.
///
/// `Ok(store)` unchanged for any store that is not a managed one named `.claude`: a store put
/// elsewhere with `skein add --store` is a directory of the person's, named by them, and is not
/// renamed here. For `/<…>/repos/<id>/store/.claude` the answer is `/<…>/repos/<id>/store/data`,
/// and the directory is moved there by a rename, by the same rules as the shared home's move:
///
/// * only the old one is there: renamed, contents and all;
/// * only the new one is there: already moved, which is every run after the first;
/// * neither is there: nothing to move, and the store is made at the new name when it is next used;
/// * both are there and the old one is empty: the empty one is removed;
/// * both are there and the old one holds anything: **refused**, naming both, and nothing moves;
/// * either is a link or not a directory: refused, and never followed.
///
/// The caller records the answer in `repos.json` ([`crate::repos::move_stores_off_dot_claude`]).
/// A refusal leaves the store where it was, working as before, and costs only the prompts.
pub(crate) fn move_store_off_dot_claude(store: &Path) -> Result<PathBuf, String> {
    if !store_has_old_name(store) {
        return Ok(store.to_path_buf());
    }
    let Some(parent) = store.parent() else {
        return Ok(store.to_path_buf());
    };
    let new = parent.join(STORE_NAME);
    let what = "this repo's store";
    let old_is_dir = real_dir_of(store, what)?;
    let new_is_dir = real_dir_of(&new, what)?;
    match (old_is_dir, new_is_dir) {
        (false, _) => Ok(new),
        (true, false) => fs::rename(store, &new).map(|()| new.clone()).map_err(|e| {
            format!(
                "could not move this repo's store from {} to {}: {e}. Nothing was moved, and the                  repo keeps its store where it was.",
                store.display(),
                new.display()
            )
        }),
        (true, true) => {
            let empty = fs::read_dir(store)
                .map_err(|e| format!("read {}: {e}", store.display()))?
                .next()
                .is_none();
            if empty {
                fs::remove_dir(store).map_err(|e| format!("rmdir {}: {e}", store.display()))?;
                return Ok(new);
            }
            Err(format!(
                "this repo's store is in two places, {} and {}, and skein will not merge them. \
                 Nothing was moved, and the repo keeps using {} until one is left: move what you \
                 want to keep into {}, remove {}, and restart skein.",
                store.display(),
                new.display(),
                store.display(),
                new.display(),
                store.display()
            ))
        }
    }
}

/// Make the store's shared home, moving it out of the store's `.claude` the first time.
///
/// Until SKEIN-1324 it was `<store>/shared-home` for every store. For a store skein manages it is
/// now the sibling ([`shared_home_dir`]), and an existing one is moved there **once**, by a rename:
///
/// * only the old one is there: it is renamed to the new place, contents and all;
/// * only the new one is there: nothing to do, which is every run after the first;
/// * both are there and the old one is empty: the empty one is removed;
/// * both are there and the old one holds anything: **refused**, naming both paths, and nothing
///   moves. Merging two sets of somebody's files is a decision about their files, and a silent one
///   is how the copy they meant to keep loses;
/// * either path is a link, or something other than a directory: refused, and never followed. The
///   store is writable from every box of the repo, so a link there is something a box could have
///   put there, and following it would move or create files wherever it points.
///
/// A box refuses to link `$HOME/shared` while the old directory still holds anything
/// (`src/store/shared-home.sh`), so a refusal here is seen in the box as well as here.
fn ensure_shared_home(store: &Path) -> Result<(), String> {
    let new = shared_home_dir(store);
    let old = store.join("shared-home");
    if new == old {
        // A store kept elsewhere: unchanged from before SKEIN-1324.
        return fs::create_dir_all(&new).map_err(|e| format!("mkdir {}: {e}", new.display()));
    }
    let old_is_dir = real_dir(&old)?;
    let new_is_dir = real_dir(&new)?;
    match (old_is_dir, new_is_dir) {
        (false, false) => make_dir_no_follow(&new),
        (false, true) => Ok(()),
        (true, false) => fs::rename(&old, &new).map_err(|e| {
            format!(
                "could not move this repo's shared files from {} to {}: {e}. Nothing was moved; \
                 move the folder there yourself and start the box again.",
                old.display(),
                new.display()
            )
        }),
        (true, true) => {
            let empty = fs::read_dir(&old)
                .map_err(|e| format!("read {}: {e}", old.display()))?
                .next()
                .is_none();
            if empty {
                return fs::remove_dir(&old).map_err(|e| format!("rmdir {}: {e}", old.display()));
            }
            Err(format!(
                "this repo's shared files are in two places, {} and {}, and skein will not merge \
                 them. Nothing was moved, and $HOME/shared stays unlinked in this repo's boxes \
                 until one is left: move what you want to keep into {}, remove {}, and start the \
                 box again.",
                old.display(),
                new.display(),
                new.display(),
                old.display()
            ))
        }
    }
}

/// Whether `path` is a directory, asked without following a link: `false` when nothing is there,
/// and refused when a link or anything other than a directory is.
fn real_dir(path: &Path) -> Result<bool, String> {
    real_dir_of(path, "this repo's shared files")
}

/// [`real_dir`], with `what` naming what was being moved in the refusal.
fn real_dir_of(path: &Path, what: &str) -> Result<bool, String> {
    match fs::symlink_metadata(path) {
        Ok(m) if m.file_type().is_symlink() => Err(format!(
            "{} is a link, and skein does not follow a link for {what}. Nothing was moved; remove \
             the link and start the box again.",
            path.display()
        )),
        Ok(m) if m.is_dir() => Ok(true),
        Ok(_) => Err(format!(
            "{} is not a folder, so {what} cannot go there. Nothing was moved; move it aside and \
             start the box again.",
            path.display()
        )),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(format!("stat {}: {e}", path.display())),
    }
}

/// `mkdir` of one directory whose parent exists, refusing a link at the name rather than making
/// the directory wherever it points (which `create_dir_all` would accept as "already there").
fn make_dir_no_follow(path: &Path) -> Result<(), String> {
    if real_dir(path)? {
        return Ok(());
    }
    match fs::create_dir(path) {
        Ok(()) => Ok(()),
        // Made by somebody else between the two calls: asked again, without following.
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => real_dir(path).map(|_| ()),
        Err(e) => Err(format!("mkdir {}: {e}", path.display())),
    }
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
        let store = home.join("repos/demo/store/data");
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
        let store = home.join("repos").join("x").join("store").join("data");
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
        ] {
            assert!(store.join(d).is_dir(), "missing {d}");
        }
        // The shared working files sit beside `.claude`, never in it (SKEIN-1324).
        assert!(store.parent().unwrap().join("shared-home").is_dir());
        assert!(!store.join("shared-home").exists());
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

    /// A store skein manages, under a pinned `$SKEIN_HOME`, with nothing in it yet.
    fn managed_store(home: &Path) -> PathBuf {
        home.join("repos").join("x").join("store").join("data")
    }

    /// The same store at the name it had before SKEIN-1330.
    fn old_managed_store(home: &Path) -> PathBuf {
        home.join("repos").join("x").join("store").join(".claude")
    }

    /// **A store at `store/.claude` is moved to `store/data` by a rename, once** (SKEIN-1330).
    ///
    /// What would make this fail: `move_store_off_dot_claude` copying rather than renaming (the old
    /// directory is still there), answering the old path, or doing anything on the second run.
    #[test]
    fn an_old_store_is_moved_to_data_once() {
        let home = tempdir();
        let old = old_managed_store(&home);
        let new = managed_store(&home);
        fs::create_dir_all(old.join("memory")).unwrap();
        fs::write(old.join("memory/MEMORY.md"), "- kept\n").unwrap();
        fs::create_dir_all(old.join("mailbox")).unwrap();
        fs::write(old.join("mailbox/1.json"), "{}").unwrap();
        let inode = std::os::unix::fs::MetadataExt::ino(&fs::metadata(&old).unwrap());

        assert_eq!(move_store_off_dot_claude(&old), Ok(new.clone()));
        assert!(!old.exists(), "the old store is still there");
        assert_eq!(
            fs::read_to_string(new.join("memory/MEMORY.md")).unwrap(),
            "- kept\n"
        );
        assert_eq!(
            std::os::unix::fs::MetadataExt::ino(&fs::metadata(&new).unwrap()),
            inode,
            "the store was copied, not renamed: a running box's bind follows the directory"
        );

        // The second run, and a store already at the new name, change nothing.
        fs::write(new.join("later.md"), "x").unwrap();
        assert_eq!(move_store_off_dot_claude(&old), Ok(new.clone()));
        assert_eq!(move_store_off_dot_claude(&new), Ok(new.clone()));
        assert!(new.join("later.md").is_file() && !old.exists());
    }

    /// **Two stores with files are refused and nothing moves**; an empty old one beside a real new
    /// one is a leftover and goes. And `ensure_store` will not make the old one again.
    ///
    /// What would make this fail: merging the two, renaming one over the other, answering `Ok`
    /// with files at both, or `ensure_store` re-creating `store/.claude` from a stale record.
    #[test]
    fn two_stores_with_files_are_refused_and_nothing_moves() {
        let _g = env_lock();
        let home = tempdir();
        let mut pins = env_pins();
        pins.set("SKEIN_HOME", &home);
        let old = old_managed_store(&home);
        let new = managed_store(&home);
        fs::create_dir_all(&old).unwrap();
        fs::create_dir_all(&new).unwrap();
        fs::write(old.join("a.md"), "old").unwrap();
        fs::write(new.join("b.md"), "new").unwrap();

        let err = move_store_off_dot_claude(&old).unwrap_err();
        assert!(
            err.contains(&old.display().to_string()) && err.contains(&new.display().to_string()),
            "the refusal must name both places: {err}"
        );
        assert_eq!(fs::read_to_string(old.join("a.md")).unwrap(), "old");
        assert!(
            !new.join("a.md").exists() && !old.join("b.md").exists(),
            "merged"
        );

        fs::remove_file(old.join("a.md")).unwrap();
        assert_eq!(move_store_off_dot_claude(&old), Ok(new.clone()));
        assert!(!old.exists(), "an empty leftover stayed");

        // A caller holding the record from before the move.
        let err = ensure_store(&old).unwrap_err();
        assert!(err.contains("moved to"), "{err}");
        assert!(!old.exists(), "ensure_store made the old store again");
    }

    /// **A link at either name is refused and never followed**, and a store that is not of the
    /// managed shape is left exactly where the person put it.
    ///
    /// What would make this fail: `fs::metadata` (which follows) in place of `symlink_metadata`,
    /// so the link is renamed or read as "already moved", or the shape test accepting a store kept
    /// elsewhere, a `..` in the path, or a doubled `/`.
    #[test]
    fn a_link_at_either_store_is_refused_and_other_stores_are_not_renamed() {
        use std::os::unix::fs::symlink;
        let home = tempdir();
        let elsewhere = tempdir();
        fs::write(elsewhere.join("private.txt"), "not the store's").unwrap();
        let old = old_managed_store(&home);
        let new = managed_store(&home);
        fs::create_dir_all(old.parent().unwrap()).unwrap();

        symlink(&elsewhere, &old).unwrap();
        let err = move_store_off_dot_claude(&old).unwrap_err();
        assert!(err.contains("is a link"), "{err}");
        assert!(old.is_symlink() && !new.exists() && !new.is_symlink());
        fs::remove_file(&old).unwrap();

        fs::create_dir_all(&old).unwrap();
        symlink(&elsewhere, &new).unwrap();
        let err = move_store_off_dot_claude(&old).unwrap_err();
        assert!(err.contains("is a link"), "{err}");
        assert!(old.is_dir() && new.is_symlink());
        assert_eq!(fs::read_dir(&elsewhere).unwrap().count(), 1);

        let theirs = elsewhere.join("thing-shared/.claude");
        fs::create_dir_all(&theirs).unwrap();
        for kept in [
            theirs.clone(),
            PathBuf::from("/x/repos/a/store/../store/.claude"),
            PathBuf::from("/x/repos/a//store/.claude"),
            PathBuf::from("/x/repos/a/stores/.claude"),
        ] {
            assert_eq!(move_store_off_dot_claude(&kept), Ok(kept.clone()));
        }
        assert!(theirs.is_dir());
    }

    /// **A box whose store moved under it follows it to `store/data`, and the shared home is
    /// beside the store under either name** — `box-self.sh`'s half of SKEIN-1330.
    ///
    /// An uncovered box sees the volume itself, so after the host's rename its checkout's record
    /// and `.claude/skein` link name a store that is gone. What would make this fail:
    /// `_skein_moved_store` gone or not consulted (the box finds no store at all and its hooks go
    /// quiet until a restart), following a name that is still there (an old `store/.claude` that is
    /// not a store would be passed over for `data`), following any shape but the managed one, or
    /// `skein_shared_home` knowing only one of the two names.
    #[test]
    fn a_box_follows_its_store_to_its_new_name() {
        use std::os::unix::fs::symlink;
        let dir = tempdir();
        let lib = dir.join("box-self.sh");
        fs::write(&lib, crate::probes::BOX_SELF_SH).unwrap();
        let ask = |call: &str, arg: &Path| {
            let out = box_shell(&dir)
                .arg("-c")
                .arg(format!(r#". "$1" && {call} "$2""#))
                .arg("box-self")
                .arg(&lib)
                .arg(arg)
                .output()
                .unwrap();
            (
                out.status.success(),
                String::from_utf8_lossy(&out.stdout).trim().to_string(),
            )
        };
        let old = dir.join("repos/web/store/.claude");
        let new = dir.join("repos/web/store/data");
        fs::create_dir_all(new.join("skein/bin")).unwrap();

        // The record the launcher wrote before the move.
        let clone = dir.join("clone");
        fs::create_dir_all(&clone).unwrap();
        let init = Command::new("git")
            .args(["init", "-q"])
            .arg(&clone)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .unwrap();
        assert!(init.status.success(), "{init:?}");
        fs::write(
            clone.join(".git/skein-store"),
            format!("{}\n", old.display()),
        )
        .unwrap();
        assert_eq!(
            ask("skein_box_store", &clone),
            (true, new.display().to_string())
        );

        // The `.claude/skein` link the kit made before the move, with no record.
        fs::remove_file(clone.join(".git/skein-store")).unwrap();
        fs::create_dir_all(clone.join(".claude")).unwrap();
        symlink(old.join("skein"), clone.join(".claude/skein")).unwrap();
        assert_eq!(
            ask("skein_box_store", &clone),
            (true, new.display().to_string())
        );

        // The old name still there and not a store: not passed over for the new one.
        fs::create_dir_all(&old).unwrap();
        assert_eq!(ask("skein_box_store", &clone), (false, String::new()));
        fs::remove_dir(&old).unwrap();

        // A store of any other shape that is gone is gone.
        let theirs = dir.join("thing-shared/.claude");
        fs::create_dir_all(dir.join("thing-shared/data/skein")).unwrap();
        fs::remove_file(clone.join(".claude/skein")).unwrap();
        symlink(theirs.join("skein"), clone.join(".claude/skein")).unwrap();
        assert_eq!(ask("skein_box_store", &clone), (false, String::new()));

        for (store, home) in [
            ("/v/repos/web/store/data", "/v/repos/web/store/shared-home"),
            (
                "/v/repos/web/store/.claude/",
                "/v/repos/web/store/shared-home",
            ),
            (
                "/v/thing-shared/.claude",
                "/v/thing-shared/.claude/shared-home",
            ),
            (
                "/v/repos/web/store/other",
                "/v/repos/web/store/other/shared-home",
            ),
        ] {
            assert_eq!(
                ask("skein_shared_home", Path::new(store)),
                (true, home.to_string()),
                "{store}"
            );
        }
    }

    /// **An existing `store/.claude/shared-home` is moved beside `.claude`, once** (SKEIN-1324).
    ///
    /// What would make this fail: `ensure_shared_home` making the new directory without moving the
    /// old one (the file stays behind and the new one is empty), copying instead of renaming (the
    /// old one is still there), or `shared_home_dir` answering the old path for a managed store.
    #[test]
    fn an_old_shared_home_is_moved_beside_the_store_once() {
        let _g = env_lock();
        let home = tempdir();
        let mut pins = env_pins();
        pins.set("SKEIN_HOME", &home);
        let store = managed_store(&home);
        let old = store.join("shared-home");
        let new = store.parent().unwrap().join("shared-home");
        fs::create_dir_all(old.join("notes")).unwrap();
        fs::write(old.join("notes/case.md"), "kept").unwrap();

        ensure_store(&store).unwrap();
        assert_eq!(shared_home_dir(&store), new);
        assert_eq!(
            fs::read_to_string(new.join("notes/case.md")).unwrap(),
            "kept"
        );
        assert!(!old.exists(), "the old directory is still inside .claude");

        // Every launch runs this again; the second run changes nothing.
        fs::write(new.join("later.md"), "written after the move").unwrap();
        ensure_store(&store).unwrap();
        assert_eq!(
            fs::read_to_string(new.join("notes/case.md")).unwrap(),
            "kept"
        );
        assert!(new.join("later.md").is_file());
        assert!(!old.exists(), "a second run made the old directory again");
    }

    /// **Two non-empty shared homes are refused, and nothing moves** — but an empty old one beside
    /// a real new one is only a leftover, and goes.
    ///
    /// What would make this fail: merging the two, renaming one over the other, or answering `Ok`.
    #[test]
    fn two_shared_homes_with_files_are_refused_and_nothing_moves() {
        let _g = env_lock();
        let home = tempdir();
        let mut pins = env_pins();
        pins.set("SKEIN_HOME", &home);
        let store = managed_store(&home);
        let old = store.join("shared-home");
        let new = store.parent().unwrap().join("shared-home");
        fs::create_dir_all(&old).unwrap();
        fs::create_dir_all(&new).unwrap();
        fs::write(old.join("a.md"), "old").unwrap();
        fs::write(new.join("b.md"), "new").unwrap();

        let err = ensure_store(&store).unwrap_err();
        assert!(
            err.contains(&old.display().to_string()) && err.contains(&new.display().to_string()),
            "the refusal must name both places: {err}"
        );
        assert_eq!(fs::read_to_string(old.join("a.md")).unwrap(), "old");
        assert_eq!(fs::read_to_string(new.join("b.md")).unwrap(), "new");
        assert!(
            !new.join("a.md").exists() && !old.join("b.md").exists(),
            "merged"
        );
        // The rest of the store was still made: the refusal costs `~/shared`, nothing else.
        assert!(store.join("skein/bin/shared-home.sh").is_file());

        // An empty leftover beside a real one is removed, and that is all.
        fs::remove_file(old.join("a.md")).unwrap();
        ensure_store(&store).unwrap();
        assert!(!old.exists());
        assert_eq!(fs::read_to_string(new.join("b.md")).unwrap(), "new");
    }

    /// **A link at either place is refused and never followed.** The store is writable from every
    /// box of the repo, so a link there is something a box could have planted.
    ///
    /// What would make this fail: `fs::metadata` (which follows) in place of `symlink_metadata` —
    /// the old link then reads as a directory and is renamed to the new place, or the new link
    /// reads as "already there" — or `create_dir_all`, which accepts a link to a directory.
    #[test]
    fn a_link_at_either_shared_home_is_refused_and_not_followed() {
        use std::os::unix::fs::symlink;
        let _g = env_lock();
        let home = tempdir();
        let mut pins = env_pins();
        pins.set("SKEIN_HOME", &home);
        let elsewhere = tempdir();
        fs::write(elsewhere.join("private.txt"), "not the store's").unwrap();

        // A link at the old place, with nothing at the new one.
        let store = managed_store(&home);
        fs::create_dir_all(&store).unwrap();
        symlink(&elsewhere, store.join("shared-home")).unwrap();
        let new = store.parent().unwrap().join("shared-home");
        let err = ensure_store(&store).unwrap_err();
        assert!(err.contains("is a link"), "{err}");
        assert!(
            !new.exists() && !new.is_symlink(),
            "the link was moved or followed"
        );
        assert!(store.join("shared-home").is_symlink());

        // A link at the new place, with nothing at the old one.
        fs::remove_file(store.join("shared-home")).unwrap();
        symlink(&elsewhere, &new).unwrap();
        let err = ensure_store(&store).unwrap_err();
        assert!(err.contains("is a link"), "{err}");
        assert!(!store.join("shared-home").exists());
        assert_eq!(
            fs::read_dir(&elsewhere).unwrap().count(),
            1,
            "something was made through the link"
        );
    }

    /// **`$HOME/shared` points beside `.claude` for a managed store, and an old link is repaired**
    /// — the box's half of SKEIN-1324. Nothing under a `.claude` directory is its target.
    ///
    /// Also what it refuses: files still at the old place (linking past them would hide them), and
    /// a missing sibling, which it must NOT make — in a covered box that would be a tmpfs, and the
    /// files written there would vanish with the box.
    ///
    /// What would make this fail: `shared-home.sh` going back to `$store/shared-home`, its
    /// `mkdir -p` of the canonical directory coming back, or `skein_shared_home` in box-self.sh
    /// losing the managed shape.
    #[test]
    fn shared_home_links_beside_the_store_and_repairs_an_old_link() {
        use std::os::unix::fs::symlink;
        let _g = env_lock();
        let home = tempdir();
        let mut pins = env_pins();
        pins.set("SKEIN_HOME", &home);
        let store = managed_store(&home);
        ensure_store(&store).unwrap();
        let new = store.parent().unwrap().join("shared-home");
        let helper = store.join("skein/bin/shared-home.sh");
        let box_home = home.join("box-home");
        fs::create_dir_all(&box_home).unwrap();
        // What a box from before the move has: a link into the store's `.claude`.
        symlink(store.join("shared-home"), box_home.join("shared")).unwrap();
        let run = || {
            box_shell(&home)
                .arg(&helper)
                .arg(&store)
                .env("HOME", &box_home)
                .output()
                .unwrap()
        };
        let out = run();
        assert!(out.status.success(), "{out:?}");
        let target = fs::read_link(box_home.join("shared")).unwrap();
        assert_eq!(target, new);
        assert!(
            !target.components().any(|c| c.as_os_str() == ".claude"),
            "$HOME/shared resolves under a .claude directory: {}",
            target.display()
        );

        // Files back at the old place: refused, and the link is left alone.
        fs::create_dir_all(store.join("shared-home")).unwrap();
        fs::write(store.join("shared-home/stray.md"), "x").unwrap();
        let out = run();
        assert!(!out.status.success(), "linked past files at the old place");
        assert!(String::from_utf8_lossy(&out.stderr).contains("still holds shared files"));
        fs::remove_dir_all(store.join("shared-home")).unwrap();

        // The sibling gone: refused, and not made.
        fs::remove_dir(&new).unwrap();
        let out = run();
        assert!(
            !out.status.success(),
            "linked to a directory that is not there"
        );
        assert!(!new.exists(), "the box made the shared home itself");
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
            box_shell(&store_tmp)
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
            box_shell(&work_tmp)
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
        let handoff = box_shell(&work_tmp)
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
        let out = box_shell(&dir)
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
        let out = box_shell(&dir)
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

    /// The store is writable by every box of the repo, so `shared-paths.txt` is one box's say over
    /// where a file lands in another box's checkout (SKEIN-1256). Only an entry of an ordinary shape
    /// is surfaced — relative, no `..`, no hidden or `.git` directory on the way — and only as a
    /// regular file that really sits in `shared-rw/`. A link an earlier start made for an entry that
    /// no longer passes is taken away again.
    ///
    /// Fails if `shared_path_shape_ok` accepts every entry (the hook and the dot-directory config
    /// are linked), or if `file_is_inside` accepts every file (the directory and the link are).
    #[test]
    fn a_shared_path_is_surfaced_only_in_an_ordinary_shape_and_only_as_a_regular_file() {
        use std::os::unix::fs::symlink;
        let _g = env_lock();
        let dir = tempdir();
        let mut env = env_pins();
        env.set("SKEIN_HOME", &dir);
        let store = dir.join("store").join(".claude");
        let tree = dir.join("tree");
        let empty_source = dir.join("no-source"); // there is no checkout to seed from, as in-fleet
        ensure_store(&store).unwrap();
        fs::create_dir_all(&empty_source).unwrap();
        assert!(Command::new("git")
            .args(["init", "-q"])
            .arg(&tree)
            .status()
            .unwrap()
            .success());
        symlink(&store, tree.join(".claude")).unwrap();
        let home = dir.join("home");
        fs::create_dir_all(&home).unwrap();
        let rw = store.join("shared-rw");
        let put = |rel: &str, body: &str| {
            let p = rw.join(rel);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(p, body).unwrap();
        };
        let run = || {
            box_shell(&dir)
                .arg(store.join("skein/bin/sandbox-bootstrap.sh"))
                .env("CLAUDE_PROJECT_DIR", &tree)
                .env("HOME", &home)
                .env("SKEIN_BOX", "demo-main")
                .env("SKEIN_SOURCE", &empty_source)
                .stdin(std::process::Stdio::null())
                .output()
                .unwrap()
        };

        // First, an ordinary start, to learn the exact spelling this script links through.
        put("CLAUDE.md", "# direction\n");
        fs::write(store.join("shared-paths.txt"), "CLAUDE.md\n").unwrap();
        let out = run();
        assert!(out.status.success(), "{out:?}");
        let linked = fs::read_link(tree.join("CLAUDE.md")).expect("an ordinary file is surfaced");
        let rw_spelled = linked.parent().unwrap().to_path_buf();

        // What an older start would have left behind: a hook linked out of shared-rw/.
        put(".git/hooks/pre-commit", "#!/bin/sh\n");
        let hook = tree.join(".git/hooks/pre-commit");
        symlink(rw_spelled.join(".git/hooks/pre-commit"), &hook).unwrap();
        assert!(
            fs::symlink_metadata(&hook).is_ok(),
            "the planted link must exist before the run"
        );

        put(".cargo/config.toml", "[build]\n");
        put("backend/.env", "SECRET=skein-test-shared\n");
        fs::create_dir_all(rw.join("confdir")).unwrap();
        fs::write(rw.join("confdir/inner"), "x").unwrap();
        fs::write(dir.join("outside.txt"), "outside\n").unwrap();
        symlink(dir.join("outside.txt"), rw.join("linked.txt")).unwrap();
        fs::write(store.join("escape"), "escape\n").unwrap();
        fs::write(
            store.join("shared-paths.txt"),
            "CLAUDE.md\n.git/hooks/pre-commit\n.cargo/config.toml\n../escape\n/etc/hostname\n\
             confdir\nlinked.txt\nbackend/.env\n",
        )
        .unwrap();
        let out = run();
        assert!(out.status.success(), "{out:?}");
        let stderr = String::from_utf8_lossy(&out.stderr);

        for refused in [
            ".git/hooks/pre-commit",
            ".cargo/config.toml",
            "confdir",
            "linked.txt",
        ] {
            assert!(
                fs::symlink_metadata(tree.join(refused)).is_err(),
                "{refused} must not be surfaced into the checkout; stderr: {stderr}"
            );
            assert!(
                stderr.contains(&format!("not surfacing '{refused}'")),
                "{refused} must be refused out loud: {stderr}"
            );
        }
        assert!(
            fs::symlink_metadata(dir.join("escape")).is_err(),
            "an entry with '..' must not land outside the checkout"
        );
        assert!(
            stderr.contains("not surfacing '../escape'")
                && stderr.contains("not surfacing '/etc/hostname'"),
            "{stderr}"
        );
        let exclude = fs::read_to_string(tree.join(".git/info/exclude")).unwrap_or_default();
        assert!(!exclude.contains("/.git/hooks"), "{exclude}");

        // And the ordinary entries still arrive, through the store.
        assert_eq!(
            fs::read_to_string(tree.join("backend/.env")).unwrap(),
            "SECRET=skein-test-shared\n"
        );
        assert_eq!(fs::read_link(tree.join("CLAUDE.md")).unwrap(), linked);
    }

    /// `.codex/memory` is the one directory skein shares through `shared-paths.txt`, so it is a
    /// named exception to the shape rule, fixed in the bootstrap rather than read from the store
    /// (SKEIN-1256). It must be a real directory in `shared-rw/` holding only files and
    /// directories; every other entry under a hidden directory is still refused.
    ///
    /// Fails if `.codex/memory` leaves the exception list (the directory is not surfaced), if the
    /// list stops being exact (`.codex/config.toml` or `.git/hooks/x` is surfaced), if
    /// `is_excepted_dir` matches a prefix rather than the whole entry (`.codex/memory/../../.git/
    /// hooks` is linked into `.git/hooks`, and `.codex/memoryX` is surfaced), or if the
    /// `find` in `dir_is_inside` goes (a directory holding a link out of `shared-rw/` stays
    /// surfaced).
    #[test]
    fn codex_memory_is_the_named_exception_and_nothing_else_under_a_hidden_directory_is() {
        use std::os::unix::fs::symlink;
        let _g = env_lock();
        let dir = tempdir();
        let mut env = env_pins();
        env.set("SKEIN_HOME", &dir);
        let store = dir.join("store").join(".claude");
        let tree = dir.join("tree");
        let empty_source = dir.join("no-source");
        ensure_store(&store).unwrap();
        fs::create_dir_all(&empty_source).unwrap();
        assert!(Command::new("git")
            .args(["init", "-q"])
            .arg(&tree)
            .status()
            .unwrap()
            .success());
        symlink(&store, tree.join(".claude")).unwrap();
        let home = dir.join("home");
        fs::create_dir_all(&home).unwrap();
        let rw = store.join("shared-rw");
        for (rel, body) in [
            (".codex/memory/note.md", "remembered\n"),
            (".codex/config.toml", "[x]\n"),
            (".git/hooks/x", "#!/bin/sh\n"),
            (".git/hooks/pre-commit", "#!/bin/sh\nexit 0\n"),
            (".codex/memoryX/note.md", "not the exception\n"),
        ] {
            let p = rw.join(rel);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(p, body).unwrap();
        }
        fs::write(
            store.join("shared-paths.txt"),
            // The climbing entry comes first: the exception is matched exactly, so it is shaped
            // like any other entry and refused before `.codex/memory` is ever linked.
            ".codex/memory/../../.git/hooks\n.codex/memoryX\n.codex/memory\n.codex/config.toml\n\
             .git/hooks/x\n",
        )
        .unwrap();
        let run = || {
            box_shell(&dir)
                .arg(store.join("skein/bin/sandbox-bootstrap.sh"))
                .env("CLAUDE_PROJECT_DIR", &tree)
                .env("HOME", &home)
                .env("SKEIN_BOX", "demo-main")
                .env("SKEIN_SOURCE", &empty_source)
                .stdin(std::process::Stdio::null())
                .output()
                .unwrap()
        };
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(
            rw.join(".git/hooks/pre-commit"),
            fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        let out = run();
        assert!(out.status.success(), "{out:?}");
        let stderr = String::from_utf8_lossy(&out.stderr);
        // An entry that only STARTS with the exception's name is no exception: matched as a
        // prefix, the climbing one would skip the shape rule and link shared-rw/'s hooks into
        // this checkout's .git.
        let hooks = tree.join(".git/hooks");
        assert!(
            !fs::symlink_metadata(&hooks)
                .unwrap()
                .file_type()
                .is_symlink()
                && fs::read_dir(&hooks).unwrap().all(|e| !e
                    .unwrap()
                    .file_type()
                    .unwrap()
                    .is_symlink()),
            "nothing in .git/hooks may be a link into shared-rw/: {stderr}"
        );
        assert!(
            fs::symlink_metadata(tree.join(".codex/memoryX")).is_err(),
            ".codex/memoryX is not the exception: {stderr}"
        );
        for refused in [".codex/memory/../../.git/hooks", ".codex/memoryX"] {
            assert!(
                stderr.contains(&format!("not surfacing '{refused}'")),
                "{refused} must be refused out loud: {stderr}"
            );
        }
        assert_eq!(
            fs::read_to_string(tree.join(".codex/memory/note.md")).unwrap_or_default(),
            "remembered\n",
            "the named exception must be surfaced: {stderr}"
        );
        for refused in [".codex/config.toml", ".git/hooks/x"] {
            assert!(
                fs::symlink_metadata(tree.join(refused)).is_err(),
                "{refused} is under a hidden directory and is no exception: {stderr}"
            );
        }

        // A link beneath the excepted directory that leads out of shared-rw/: the whole directory
        // is refused at the next start, and the link the last start made is taken away.
        fs::write(dir.join("outside.txt"), "outside\n").unwrap();
        symlink(dir.join("outside.txt"), rw.join(".codex/memory/out.md")).unwrap();
        let out = run();
        assert!(out.status.success(), "{out:?}");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            fs::symlink_metadata(tree.join(".codex/memory")).is_err(),
            "a directory holding a link out of shared-rw/ must not stay surfaced: {stderr}"
        );
        assert!(stderr.contains("not surfacing '.codex/memory'"), "{stderr}");
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
            let store = dir.join("repos/web/store/data");
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

    /// A takeover's snapshot, written the way `takeover::prepare_replacement` writes one: a bundle
    /// of the clone's HEAD, its manifest, and `untracked.tgz` built from `untracked` (relative
    /// path, contents). `unproduced` builds an `agent-state.tgz` beside them, which nothing has
    /// written since SKEIN-1289 and the kit must leave alone (SKEIN-1314). Built in `stage`, a
    /// directory of the test's own.
    fn handoff_snapshot(
        kit: &KitBox,
        at: &Path,
        untracked: &[(&str, &str)],
        unproduced: &[(&str, &str)],
    ) {
        fs::create_dir_all(at).unwrap();
        kit.git(&[
            "bundle",
            "create",
            &at.join("repo.bundle").to_string_lossy(),
            "HEAD",
        ]);
        let head = kit.git(&["rev-parse", "HEAD"]);
        fs::write(
            at.join("manifest.json"),
            format!("{{\"head\":\"{}\"}}\n", head.trim()),
        )
        .unwrap();
        for (archive, members) in [
            ("untracked.tgz", untracked),
            ("agent-state.tgz", unproduced),
        ] {
            if members.is_empty() {
                continue;
            }
            let stage = at.join(format!("{archive}.stage"));
            for (rel, body) in members {
                let p = stage.join(rel);
                fs::create_dir_all(p.parent().unwrap()).unwrap();
                fs::write(p, body).unwrap();
            }
            let names: Vec<&str> = members.iter().map(|(rel, _)| *rel).collect();
            let ok = Command::new("tar")
                .arg("-C")
                .arg(&stage)
                .arg("-czf")
                .arg(at.join(archive))
                .args(&names)
                .status()
                .unwrap()
                .success();
            assert!(ok, "could not build {archive}");
            fs::remove_dir_all(&stage).unwrap();
        }
    }

    impl KitBox {
        /// The kit, as [`KitBox::start`] runs it, with the box's host-only state directory named
        /// (or not) and the launch spec asking for a handoff from `dir`; the outcome is returned
        /// rather than asserted, because refusing is what some of these runs are for.
        fn start_handoff(&self, dir: &str, state: Option<&Path>) -> std::process::Output {
            let branch = self.git(&["rev-parse", "--abbrev-ref", "HEAD"]);
            fs::write(
                self.store.join("skein/launch/web-main.json"),
                serde_json::json!({
                    "agent": "claude",
                    "branch": branch.trim(),
                    "handoff": {"source": "web-old", "dir": dir},
                })
                .to_string(),
            )
            .unwrap();
            // Through `box_shell`, because the kit runs the store's scripts in turn and must not
            // find anything of the machine running the suite (SKEIN-1229).
            let mut kit = box_shell(&self.dir);
            kit.arg(self.root.join(".skein/skein-startup.sh"))
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
                .env_remove("SKEIN_STATE")
                .stdin(std::process::Stdio::null());
            if let Some(state) = state {
                kit.env("SKEIN_STATE", state);
            }
            kit.output().unwrap()
        }
    }

    /// **A takeover's snapshot is restored only from the box's own host-only state directory**
    /// (SKEIN-1258). The repo's store is writable by every box of the repo, so a snapshot there is
    /// whatever a box made of it before the kit ran; `$SKEIN_STATE` is bound read-only into this box
    /// and no other.
    ///
    /// What would make it fail: the kit accepting `skein/handoff-snapshots/…` in the store again
    /// (the first run succeeds and restores the store's file); the kit restoring without
    /// `$SKEIN_STATE` (the second run succeeds); the kit no longer restoring from the state
    /// directory at all (the last run leaves `notes.txt` absent).
    #[test]
    fn a_handoff_is_restored_only_from_the_boxs_own_host_only_state_directory() {
        if !have_jq() {
            return skip(
                "no jq here, and the kit reads the handoff out of the launch spec with jq",
            );
        }
        let _lock = env_lock();
        let skein_home = tempdir();
        let mut env = env_pins();
        env.set("SKEIN_HOME", &skein_home);
        let kit = KitBox::new("claude", None);
        let state = kit.dir.join("state/web-main");

        // The old place: the repo's store. Complete and well-formed, and still refused.
        handoff_snapshot(
            &kit,
            &kit.store.join("skein/handoff-snapshots/web-main/r1"),
            &[("notes.txt", "from the store\n")],
            &[],
        );
        let out = kit.start_handoff("skein/handoff-snapshots/web-main/r1", Some(&state));
        assert!(
            !out.status.success(),
            "a snapshot in the store must be refused: {out:?}"
        );
        assert!(!kit.tree.join("notes.txt").exists());

        // The right place, but the kit was not told where this box's state is.
        handoff_snapshot(
            &kit,
            &state.join("handoff/r1"),
            &[("notes.txt", "from the host\n")],
            &[],
        );
        let out = kit.start_handoff("handoff/r1", None);
        assert!(!out.status.success(), "no SKEIN_STATE, no restore: {out:?}");
        assert!(!kit.tree.join("notes.txt").exists());

        // A spec that tries to climb out of the state directory.
        let out = kit.start_handoff("handoff/../../web-main/handoff/r1", Some(&state));
        assert!(!out.status.success(), "{out:?}");

        let out = kit.start_handoff("handoff/r1", Some(&state));
        assert!(out.status.success(), "{out:?}");
        assert_eq!(
            fs::read_to_string(kit.tree.join("notes.txt")).unwrap(),
            "from the host\n"
        );
    }

    /// **An archive in a snapshot is listed before it is opened, and one carrying more than it may
    /// is refused whole** (SKEIN-1258). The untracked-files archive may not reach under `.git`.
    ///
    /// What would make it fail: `archive_members_ok` returning success for everything (the hook
    /// lands under `.git/hooks`).
    #[test]
    fn a_handoff_archive_carrying_more_than_it_may_is_refused_whole() {
        if !have_jq() {
            return skip(
                "no jq here, and the kit reads the handoff out of the launch spec with jq",
            );
        }
        let _lock = env_lock();
        let skein_home = tempdir();
        let mut env = env_pins();
        env.set("SKEIN_HOME", &skein_home);

        let kit = KitBox::new("claude", None);
        let state = kit.dir.join("state/web-main");
        handoff_snapshot(
            &kit,
            &state.join("handoff/r1"),
            &[
                ("notes.txt", "work\n"),
                (".git/hooks/post-checkout", "#!/bin/sh\n"),
            ],
            &[],
        );
        let out = kit.start_handoff("handoff/r1", Some(&state));
        assert!(
            !out.status.success(),
            "an archive reaching under .git must be refused: {out:?}"
        );
        assert!(!kit.tree.join(".git/hooks/post-checkout").exists());
        assert!(
            !kit.tree.join("notes.txt").exists(),
            "refused whole, not member by member"
        );
    }

    /// **An agent-state archive in a snapshot is not restored** (SKEIN-1314).
    ///
    /// The kit unpacked `agent-state.tgz` into `$HOME` — prompt history, todos, `.claude.json` —
    /// for a fleet resize's snapshot, whose producer SKEIN-1289 deleted; a takeover writes none.
    /// The restore is gone, and an archive found anyway is left where it is while the tree is
    /// restored as usual.
    ///
    /// What would make it fail: the kit's restore branch put back (the todo and `.claude.json`
    /// land in `$HOME`). The tree assertion is the control: the snapshot was restored, so the
    /// absence below is not a start that never reached the handoff.
    #[test]
    fn an_agent_state_archive_in_a_snapshot_is_left_alone() {
        if !have_jq() {
            return skip(
                "no jq here, and the kit reads the handoff out of the launch spec with jq",
            );
        }
        let _lock = env_lock();
        let skein_home = tempdir();
        let mut env = env_pins();
        env.set("SKEIN_HOME", &skein_home);

        let kit = KitBox::new("claude", None);
        let state = kit.dir.join("state/web-main");
        handoff_snapshot(
            &kit,
            &state.join("handoff/r1"),
            &[("notes.txt", "work\n")],
            &[(".claude/todos/t.json", "[]\n"), (".claude.json", "{}\n")],
        );
        let out = kit.start_handoff("handoff/r1", Some(&state));
        assert!(out.status.success(), "{out:?}");
        assert_eq!(
            fs::read_to_string(kit.tree.join("notes.txt")).unwrap(),
            "work\n"
        );
        assert!(
            !kit.home.join(".claude/todos/t.json").exists(),
            "an agent-state archive nothing produces was unpacked into $HOME"
        );
        assert_ne!(
            fs::read_to_string(kit.home.join(".claude.json"))
                .ok()
                .as_deref(),
            Some("{}\n"),
            "an agent-state archive nothing produces replaced .claude.json"
        );
    }

    /// **A snapshot's bundle is restored from its HEAD, the commit its manifest records**
    /// (SKEIN-1321). The one producer, `takeover::prepare_replacement`, bundles HEAD alone; the kit
    /// used to prefer `refs/heads/<branch>` from the bundle, for a fleet resize's `--all` bundle
    /// that nothing has written since SKEIN-1289.
    ///
    /// What would make it fail: the all-branches attempt put back (the bundle here also carries
    /// the box's branch at an older commit, and the restore lands there instead of on HEAD).
    #[test]
    fn a_handoff_bundle_is_restored_from_its_head() {
        if !have_jq() {
            return skip(
                "no jq here, and the kit reads the handoff out of the launch spec with jq",
            );
        }
        let _lock = env_lock();
        let skein_home = tempdir();
        let mut env = env_pins();
        env.set("SKEIN_HOME", &skein_home);

        let kit = KitBox::new("claude", None);
        let state = kit.dir.join("state/web-main");
        let at = state.join("handoff/r1");
        fs::create_dir_all(&at).unwrap();
        let base = kit.git(&["rev-parse", "HEAD"]).trim().to_string();
        let branch = kit
            .git(&["rev-parse", "--abbrev-ref", "HEAD"])
            .trim()
            .to_string();
        kit.git(&["commit", "-q", "--allow-empty", "-m", "on the branch"]);
        kit.git(&["checkout", "-q", "--detach"]);
        kit.git(&["commit", "-q", "--allow-empty", "-m", "the box's HEAD"]);
        let head = kit.git(&["rev-parse", "HEAD"]).trim().to_string();
        kit.git(&[
            "bundle",
            "create",
            &at.join("repo.bundle").to_string_lossy(),
            "HEAD",
            &format!("refs/heads/{branch}"),
        ]);
        fs::write(
            at.join("manifest.json"),
            format!("{{\"head\":\"{head}\"}}\n"),
        )
        .unwrap();
        kit.git(&["checkout", "-q", &branch]);
        kit.git(&["reset", "-q", "--hard", &base]);

        let out = kit.start_handoff("handoff/r1", Some(&state));
        assert!(out.status.success(), "{out:?}");
        assert_eq!(kit.git(&["rev-parse", "HEAD"]).trim(), head);
        assert_eq!(
            kit.git(&["rev-parse", "--abbrev-ref", "HEAD"]).trim(),
            branch
        );
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
        // The repo set `tui` and a status line but no subagent cache TTL, so skein's is the one
        // default that goes in.
        assert_eq!(
            kit.json("settings.local.json"),
            serde_json::json!({
                "subagentPromptCacheTtl": defaults["settings"]["subagentPromptCacheTtl"],
                // Not defaults: where this box's memory and shared files are (SKEIN-1330).
                "autoMemoryDirectory": kit.store.join("memory"),
                "permissions": { "additionalDirectories": [kit.home.join("shared")] },
            }),
            "the box's local settings override what the repo chose"
        );
    }

    /// **Skein's `subagentPromptCacheTtl` of `"1h"` goes into a box's own `settings.local.json`
    /// only where neither that file nor the repo's `settings.json` sets one** (SKEIN-1218), so a
    /// repo or a box that wants Claude Code's `"5m"` keeps it.
    ///
    /// What would make it fail: the kit's jq losing its `subagentPromptCacheTtl` rule, or the key
    /// leaving [`crate::probes::settings_defaults`] (no box gets `"1h"`: the first two
    /// assertions); the rule ignoring the repo's file (the local file gets `"1h"`, which outranks
    /// the repo's `"5m"`: the third); the rule ignoring the local file (the box's own `"5m"` is
    /// overwritten: the last).
    #[test]
    fn a_box_gets_an_hour_of_subagent_prompt_cache_unless_its_repo_or_itself_sets_one() {
        if !have_jq() {
            return skip("no jq here, and the kit's merge is jq");
        }
        let _lock = env_lock();
        let skein_home = tempdir();
        let mut env = env_pins();
        env.set("SKEIN_HOME", &*skein_home);
        env.set("SKEIN_FLEET_ROOT", skein_home.join("fleet"));

        // Neither file sets it: both layouts, a repo with no `.claude` and one that ships its own.
        for ships in [None, Some("{}\n")] {
            let kit = KitBox::new("claude", ships);
            kit.start();
            assert_eq!(
                kit.json("settings.local.json")["subagentPromptCacheTtl"],
                "1h",
                "a box whose settings set no subagent cache TTL did not get skein's (ships {ships:?})"
            );
        }
        let kit = KitBox::new("claude", Some("{}\n"));
        kit.start();
        let first = fs::read(kit.claude("settings.local.json")).unwrap();
        kit.start();
        assert_eq!(
            fs::read(kit.claude("settings.local.json")).unwrap(),
            first,
            "a second start changed the local settings"
        );

        // The repo's own `"5m"`: the local file stays without the key, so the repo's wins.
        let repo = "{\"subagentPromptCacheTtl\":\"5m\"}\n";
        let kit = KitBox::new("claude", Some(repo));
        kit.start();
        assert_eq!(
            fs::read_to_string(kit.claude("settings.json")).unwrap(),
            repo
        );
        let local = kit.json("settings.local.json");
        assert!(
            local.get("subagentPromptCacheTtl").is_none(),
            "the box's local file overrides the repo's own subagent cache TTL: {local}"
        );

        // The box's own `"5m"` in its local file is not overwritten.
        let kit = KitBox::new("claude", Some("{}\n"));
        fs::write(
            kit.claude("settings.local.json"),
            "{\"subagentPromptCacheTtl\":\"5m\"}\n",
        )
        .unwrap();
        kit.start();
        assert_eq!(
            kit.json("settings.local.json")["subagentPromptCacheTtl"],
            "5m",
            "the box's own subagent cache TTL was overwritten"
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
                "/.claude/settings.json.skein-old",
                "/.claude/skein.skein-old"
            ],
            "the clone's exclude is not what skein put in .claude"
        );
    }

    /// What a box whose repo tracks nothing under `.claude` reaches through it: the store's `skein/`
    /// and the entries Claude Code reads from a project's `.claude`, each a link to the store's own,
    /// and nothing else of the store's — not its settings files, which every box can write, and not
    /// its memory, mailbox or runtime, where a write through `.claude` asks for permission
    /// (SKEIN-1330).
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
            let read_by_claude = [
                "skein",
                "skills",
                "commands",
                "agents",
                "output-styles",
                "rules",
            ];
            if !read_by_claude.contains(&name.as_str()) {
                assert!(
                    !here.is_symlink(),
                    "the store's {name} is linked into this box's .claude"
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
            linked >= 2,
            "neither skein/ nor skills/ was linked, so this checked little"
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
        let out = box_shell(&kit.dir)
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

    /// **A box whose repo tracks nothing under `.claude` gets a directory of its own, with the
    /// store's `skein/` and what Claude Code reads linked into it and the store's settings files,
    /// memory and runtime left out** (SKEIN-1053, the owner's answer to SKEIN-1153, SKEIN-1330).
    /// What would make it fail: `.claude` linked to the store whole, as before (the first
    /// assertion); either settings file, or `memory/`, linked; `skein/` or `skills/` not linked;
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
    /// What a converted box keeps, each asserted below: the store's `skein/` and what Claude Code
    /// reads from `.claude`, linked one by one (SKEIN-1330); every byte of the store's
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
             store's skein/ and what Claude Code reads from .claude are linked into it, and \
             settings.json and settings.local.json are now this box's own",
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

    /// **The checkout's `.claude` links the store's `skein/` and what Claude Code reads from a
    /// project's `.claude`, and nothing else; a link an earlier start made to anything else goes,
    /// and one to the store's old name is redone** (SKEIN-1330).
    ///
    /// Claude Code judges a path as written as well as where it leads, so `.claude/memory` asked
    /// before every write even once the store was not called `.claude`.
    ///
    /// What would make it fail: the kit linking every store entry again (`memory` and `mailbox` are
    /// back), the prune keyed on the new name only (the old-name `memory` link survives), the
    /// prune touching a link skein did not make (`notes` goes), or a kept entry's old-name link left
    /// dangling (`skills` and, in a repo that ships `.claude`, `skein` point at a store that is gone).
    #[test]
    fn the_checkout_links_only_what_claude_code_reads_from_the_store() {
        if !have_jq() {
            return skip("no jq here, and the kit's merge is jq");
        }
        use std::os::unix::fs::symlink;
        let _lock = env_lock();
        let skein_home = tempdir();
        let mut env = env_pins();
        env.set("SKEIN_HOME", &*skein_home);
        env.set("SKEIN_FLEET_ROOT", skein_home.join("fleet"));
        for ships in [None, Some("{}\n")] {
            let kit = KitBox::new("claude", ships);
            let old = kit.store.with_file_name(".claude");
            let rc = kit.tree.join(".claude");
            fs::create_dir_all(&rc).unwrap();
            fs::create_dir_all(kit.store.join("commands")).unwrap();
            // What a start before SKEIN-1330 left: every entry linked, to the store's old name.
            symlink(old.join("memory"), rc.join("memory")).unwrap();
            symlink(old.join("skills"), rc.join("skills")).unwrap();
            symlink(old.join("skein"), rc.join("skein")).unwrap();
            symlink(kit.store.join("mailbox"), rc.join("mailbox")).unwrap();
            symlink(kit.dir.join("elsewhere"), rc.join("notes")).unwrap();

            kit.start();

            let link = |name: &str| fs::read_link(rc.join(name)).ok();
            assert_eq!(link("skein"), Some(kit.store.join("skein")), "{ships:?}");
            assert_eq!(
                link("memory"),
                None,
                "{ships:?}: .claude/memory is still linked"
            );
            assert_eq!(
                link("mailbox"),
                None,
                "{ships:?}: .claude/mailbox is still linked"
            );
            assert_eq!(link("notes"), Some(kit.dir.join("elsewhere")), "{ships:?}");
            match ships {
                None => {
                    assert_eq!(link("skills"), Some(kit.store.join("skills")));
                    assert_eq!(link("commands"), Some(kit.store.join("commands")));
                }
                Some(_) => assert_eq!(
                    link("skills"),
                    None,
                    "a shipped .claude got the store's skills"
                ),
            }
            // Nothing else of the store's is reachable through the checkout's `.claude`.
            for entry in fs::read_dir(&rc).unwrap() {
                let name = entry.unwrap().file_name().to_string_lossy().into_owned();
                if let Some(to) = link(&name) {
                    assert!(
                        !to.starts_with(&kit.store) && !to.starts_with(&old)
                            || ["skein", "skills", "commands"].contains(&name.as_str()),
                        "{ships:?}: .claude/{name} links into the store"
                    );
                }
            }
            let note = kit.boot()["claude_note"].as_str().unwrap_or("").to_string();
            assert!(
                note.contains("memory") && note.contains("mailbox"),
                "{note}"
            );

            // The next start changes nothing.
            kit.start();
            assert_eq!(link("skein"), Some(kit.store.join("skein")));
            assert_eq!(link("memory"), None);
        }
    }

    /// **The box's own settings send Claude's memory to the store and let it write `~/shared`,
    /// with no link under `.claude` and no prompt** (SKEIN-1330), in both layouts, once.
    ///
    /// Claude Code's own memory directory is `~/.claude/projects/<slug>/memory`, and with that a
    /// link into the store every memory write in default or acceptEdits mode asked, whatever the
    /// store was called; `autoMemoryDirectory` set to the store's `memory/` asked nothing.
    ///
    /// What would make it fail: the merge not writing `autoMemoryDirectory` (absent), writing it
    /// over the person's own (`/mine`), not replacing skein's old value (the `.claude` path stays),
    /// or adding `$HOME/shared` again on every start (it appears twice).
    #[test]
    fn the_box_settings_point_memory_at_the_store_and_allow_the_shared_home() {
        if !have_jq() {
            return skip("no jq here, and the kit's merge is jq");
        }
        let _lock = env_lock();
        let skein_home = tempdir();
        let mut env = env_pins();
        env.set("SKEIN_HOME", &*skein_home);
        env.set("SKEIN_FLEET_ROOT", skein_home.join("fleet"));
        for ships in [None, Some("{}\n")] {
            let kit = KitBox::new("claude", ships);
            let shared = kit.home.join("shared").to_string_lossy().into_owned();
            let mem = kit.store.join("memory").to_string_lossy().into_owned();
            fs::create_dir_all(kit.claude("")).unwrap();
            // Skein's value from before the move, and a directory the person added themselves.
            fs::write(
                kit.claude("settings.local.json"),
                serde_json::json!({
                    "autoMemoryDirectory": kit.store.with_file_name(".claude").join("memory"),
                    "permissions": { "additionalDirectories": ["/theirs"], "allow": ["Bash(ls)"] },
                })
                .to_string(),
            )
            .unwrap();
            kit.start();
            kit.start();
            let local = kit.json("settings.local.json");
            assert_eq!(
                local["autoMemoryDirectory"],
                mem.as_str(),
                "{ships:?}: {local}"
            );
            assert_eq!(
                local["permissions"]["additionalDirectories"],
                serde_json::json!(["/theirs", shared]),
                "{ships:?}: {local}"
            );
            assert_eq!(
                local["permissions"]["allow"],
                serde_json::json!(["Bash(ls)"])
            );

            // A memory directory the person chose is theirs.
            let mut mine = local.clone();
            mine["autoMemoryDirectory"] = "/mine".into();
            fs::write(kit.claude("settings.local.json"), mine.to_string()).unwrap();
            kit.start();
            assert_eq!(
                kit.json("settings.local.json")["autoMemoryDirectory"],
                "/mine"
            );
        }
    }

    /// **A Codex box may write the shared home and the mailbox: both are `writable_roots` in its
    /// `config.toml`, in a line skein owns, and never over the person's own** (SKEIN-1330).
    ///
    /// What would make it fail: the kit not writing the line (nothing writable), writing the link
    /// `$HOME/shared` rather than where it leads (Codex checks the real path), a second line on the
    /// second start (a key twice is a file Codex will not load — checked by parsing it), dropping a
    /// key of the person's in the same table, or writing beside a `writable_roots` of theirs.
    #[test]
    fn codex_may_write_the_shared_home_and_the_mailbox() {
        if !have_jq() {
            return skip("no jq here, and the kit's merge is jq");
        }
        let parse = |p: &Path| -> Option<serde_json::Value> {
            let out = Command::new("python3")
                .args(["-I", "-c", "import json,sys,tomllib; print(json.dumps(tomllib.load(open(sys.argv[1],'rb'))))"])
                .arg(p)
                .output()
                .ok()?;
            assert!(out.status.success(), "not TOML Codex can load: {out:?}");
            serde_json::from_slice(&out.stdout).ok()
        };
        let _lock = env_lock();
        let skein_home = tempdir();
        let mut env = env_pins();
        env.set("SKEIN_HOME", &*skein_home);
        env.set("SKEIN_FLEET_ROOT", skein_home.join("fleet"));
        let kit = KitBox::new("codex", None);
        let cfg = kit.home.join(".codex/config.toml");
        let want = serde_json::json!([
            kit.store.with_file_name("shared-home"),
            kit.store.join("mailbox"),
        ]);

        // No config at all, then the same start again. Twice before the comparison: the kit trusts
        // the checkout in `config.toml` only once the file is there, which the first start makes.
        kit.start();
        kit.start();
        let first = fs::read_to_string(&cfg).unwrap();
        kit.start();
        assert_eq!(
            fs::read_to_string(&cfg).unwrap(),
            first,
            "the third start changed it"
        );
        if let Some(v) = parse(&cfg) {
            assert_eq!(
                v["sandbox_workspace_write"]["writable_roots"], want,
                "{first}"
            );
        }
        assert_eq!(kit.boot()["codex_roots"], "set");

        // The person's own table: the line goes in it, and their key stays.
        fs::write(
            &cfg,
            "[sandbox_workspace_write]\nnetwork_access = true\n\n[tui]\nx = 1\n",
        )
        .unwrap();
        kit.start();
        kit.start();
        let text = fs::read_to_string(&cfg).unwrap();
        if let Some(v) = parse(&cfg) {
            assert_eq!(
                v["sandbox_workspace_write"]["writable_roots"], want,
                "{text}"
            );
            assert_eq!(
                v["sandbox_workspace_write"]["network_access"], true,
                "{text}"
            );
            assert_eq!(v["tui"]["x"], 1, "{text}");
        }

        // Their own list: left exactly as it was, and said.
        // With the checkout's trust already in it, so the only thing that could change is skein's.
        let theirs = format!(
            "[sandbox_workspace_write]\nwritable_roots = [\"/theirs\"]\n\n[projects.\"{}\"]\n\
             trust_level = \"trusted\"\n",
            kit.tree.display()
        );
        fs::write(&cfg, &theirs).unwrap();
        kit.start();
        assert_eq!(fs::read_to_string(&cfg).unwrap(), theirs);
        assert_eq!(kit.boot()["codex_roots"], "theirs");
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
