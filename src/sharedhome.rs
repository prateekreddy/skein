//! Importing a box's private home into the shared one — explicitly, and one entry at a time.
//!
//! A box's `$HOME` is private and stays that way; `$HOME/shared` is the project-scoped directory
//! every box of a repo can see. Moving something from the first to the second is a decision, so it
//! is made as one: an inventory that reads and copies nothing, then an import of exactly the names
//! the person picked.
//!
//! The inventory reports what it *excluded* and why, rather than silently offering a shorter list.
//! Credentials, agent runtime state, caches, repositories and build outputs are never eligible, and
//! a reviewer can see that they were considered and refused rather than having to trust that the
//! filter ran.

use crate::kit::ensure_store;
use crate::repos::repo_for_box;
use crate::sandbox::sbx_guest_output;
use crate::util::sh_quote;
use crate::util::valid_name;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::path::Path;
use std::time::Duration;

/// One top-level entry found while inspecting a box's private home for an explicit shared-home
/// import. Inventory is read-only; excluded entries remain visible with the reason so the safety
/// boundary is reviewable rather than hidden in implementation details.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SharedHomeCandidate {
    pub name: String,
    pub kind: String,
    pub bytes: u64,
    pub eligible: bool,
    pub reason: String,
}

const SHARED_HOME_INVENTORY: &str = r####"set -o pipefail; command -v jq >/dev/null 2>&1 || { echo 'jq is required for shared-home inventory' >&2; exit 1; }; find "$HOME" -mindepth 1 -maxdepth 1 -print0 2>/dev/null | sort -z | while IFS= read -r -d '' path; do name="${path##*/}"; kind="other"; [ -f "$path" ] && kind="file"; [ -d "$path" ] && kind="directory"; [ -L "$path" ] && kind="symlink"; eligible=true; reason=""; if [ -L "$path" ]; then eligible=false; reason="symlinks are never imported"; elif [ ! -f "$path" ] && [ ! -d "$path" ]; then eligible=false; reason="sockets/devices/FIFOs are never imported"; else case "$name" in .* ) eligible=false; reason="hidden credential/runtime/cache path" ;; workspace|work|project|projects|src|repos|repositories|node_modules|target|build|dist|vendor|venv ) eligible=false; reason="workspace, dependency, or build-output path" ;; esac; fi; if [ "$eligible" = true ] && [ -d "$path" ] && find "$path" -type d -name .git -print -quit 2>/dev/null | grep -q .; then eligible=false; reason="contains a Git repository"; fi; bytes=0; if [ "$eligible" = true ]; then kb="$(du -sk "$path" 2>/dev/null | awk 'NR==1 {print $1}')"; case "$kb" in ''|*[!0-9]*) kb=0 ;; esac; bytes=$((kb * 1024)); fi; jq -cn --arg name "$name" --arg kind "$kind" --argjson bytes "$bytes" --argjson eligible "$eligible" --arg reason "$reason" '{name:$name,kind:$kind,bytes:$bytes,eligible:$eligible,reason:$reason}'; done"####;

/// Inspect a source box's private `$HOME` without copying anything. Hidden state, workspaces,
/// repositories, dependencies/build outputs, symlinks, and special files are explicitly ineligible.
pub fn shared_home_inventory(name: &str) -> Result<Vec<SharedHomeCandidate>, String> {
    if !valid_name(name) {
        return Err("invalid box name".into());
    }
    let repo = repo_for_box(name).ok_or_else(|| format!("no registered repo for box {name}"))?;
    ensure_store(Path::new(&repo.store))?;
    let raw = sbx_guest_output(name, SHARED_HOME_INVENTORY, Duration::from_secs(120))?;
    raw.lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            serde_json::from_str(line)
                .map_err(|error| format!("invalid inventory response from {name}: {error}"))
        })
        .collect()
}

const SHARED_HOME_IMPORT: &str = r####"
set -euo pipefail
root="$(git -C "$PWD" rev-parse --show-toplevel 2>/dev/null || pwd)"
store="$root/.claude"
if [ -L "$store/skein" ]; then store="$(dirname "$(readlink "$store/skein")")"; fi
canonical="$store/shared-home"
[ -d "$canonical" ] && [ -w "$canonical" ] \
  || { echo "shared-home unavailable or not writable: $canonical" >&2; exit 1; }
[ "$#" -gt 0 ] || { echo 'no import entries selected' >&2; exit 1; }

# Structural safety remains strict. Read failures are handled separately by tar below: the user may
# choose a best-effort import, but a symlink/device or destination collision is never safe to guess.
for name in "$@"; do
  src="$HOME/$name"
  [ -f "$src" ] || [ -d "$src" ] || { echo "source entry unavailable: $name" >&2; exit 1; }
  [ ! -L "$src" ] || { echo "source entry became a symlink: $name" >&2; exit 1; }
  [ ! -e "$canonical/$name" ] && [ ! -L "$canonical/$name" ] \
    || { echo "destination already exists: $name" >&2; exit 1; }
  unsafe="$(find "$src" -mindepth 1 \( -type l -o -type s -o -type b -o -type c -o -type p \) -print -quit 2>/dev/null || true)"
  [ -z "$unsafe" ] || { echo "unsafe nested entry blocks import: $unsafe" >&2; exit 1; }
done

stage="$store/.shared-home-import.$(printf '%s' "${SANDBOX_VM_ID:-box}" | tr / -).$$"
mkdir -p "$stage"
trap 'rm -rf "$stage"' EXIT
warnings="$stage/.tar-warnings"

# `--ignore-failed-read` skips only source entries tar cannot stat/read. Capture every warning so the
# import is explicitly best-effort rather than silently claiming parity with the old home.
tar -C "$HOME" -cf - --ignore-failed-read \
  --exclude='*/.*' \
  --exclude='*/node_modules' --exclude='*/node_modules/*' \
  --exclude='*/target' --exclude='*/target/*' \
  --exclude='*/build' --exclude='*/build/*' \
  --exclude='*/dist' --exclude='*/dist/*' \
  --exclude='*/vendor' --exclude='*/vendor/*' \
  --exclude='*/venv' --exclude='*/venv/*' \
  --exclude='*/__pycache__' --exclude='*/__pycache__/*' \
  --exclude='*/credentials' --exclude='*/credentials/*' --exclude='*/credentials.json' \
  --exclude='*/secrets' --exclude='*/secrets/*' --exclude='*/secrets.*' \
  --exclude='*/auth.json' --exclude='*/token.json' \
  --exclude='*/id_rsa*' --exclude='*/id_ed25519*' --exclude='*.pem' --exclude='*.key' \
  -- "$@" 2>"$warnings" | tar -C "$stage" -xf -

imported_list="$stage/.imported"
: >"$imported_list"
imported=0
for name in "$@"; do
  if [ -e "$stage/$name" ]; then
    mv "$stage/$name" "$canonical/$name"
    printf '%s\n' "$name" >>"$imported_list"
    imported=$((imported + 1))
  else
    printf 'skipped entirely (nothing readable): %s\n' "$name" >>"$warnings"
  fi
done
[ "$imported" -gt 0 ] || { cat "$warnings" >&2; echo 'nothing readable was imported' >&2; exit 1; }

mkdir -p "$store/skein/imports"
items="$(jq -Rsc 'split("\n")[:-1]' <"$imported_list")"
warning_text="$(cat "$warnings")"
jq -cn --arg from "${SANDBOX_VM_ID:-unknown}" --arg ts "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
  --argjson items "$items" --arg warnings "$warning_text" \
  '{from:$from,ts:$ts,items:$items,warnings:$warnings}' \
  > "$store/skein/imports/$(date -u +%Y%m%dT%H%M%SZ)-${SANDBOX_VM_ID:-box}.json"
printf 'imported %s item(s) into %s\n' "$imported" "$canonical"
if [ -s "$warnings" ]; then
  echo 'Skipped source entries:'
  cat "$warnings"
fi
"####;

/// Copy explicitly selected, inventory-approved top-level entries from one box's private home into
/// the repo's canonical shared home. Existing destinations never get merged or overwritten. Nested
/// hidden state, credentials, dependencies, and build outputs remain excluded during the copy.
pub fn import_shared_home(name: &str, selected: &[String]) -> Result<String, String> {
    if selected.is_empty() {
        return Err("choose at least one inventory entry to import".into());
    }
    let inventory = shared_home_inventory(name)?;
    let repo = repo_for_box(name).ok_or_else(|| format!("no registered repo for box {name}"))?;
    let canonical = Path::new(&repo.store).join("shared-home");
    let mut unique = BTreeSet::new();
    for entry in selected {
        if entry.contains(['\n', '\r', '\0']) {
            return Err(format!("unsafe control character in entry name: {entry:?}"));
        }
        if !unique.insert(entry) {
            return Err(format!("duplicate import entry: {entry:?}"));
        }
        let candidate = inventory
            .iter()
            .find(|candidate| candidate.name == *entry)
            .ok_or_else(|| format!("{entry:?} is not a top-level entry in {name}"))?;
        if !candidate.eligible {
            return Err(format!("{entry:?} is excluded: {}", candidate.reason));
        }
        if canonical.join(entry).exists() || canonical.join(entry).is_symlink() {
            return Err(format!(
                "shared-home destination already exists: {entry:?} (nothing was copied)"
            ));
        }
    }
    let selection = selected
        .iter()
        .map(|entry| sh_quote(entry))
        .collect::<Vec<_>>()
        .join(" ");
    let command = format!("set -- {selection}; {SHARED_HOME_IMPORT}");
    sbx_guest_output(name, &command, Duration::from_secs(600))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::place::{record_place, PlaceRecord};
    use crate::repos::{save_repos, Repo};
    use crate::testutil::*;
    use std::env;
    use std::fs;
    use std::process::Command;

    /// Linux only: the inventory and import shells use `sort -z` and `tar --ignore-failed-read`,
    /// both GNU-only and both deliberate — they run in a box, where that is the userland.
    #[cfg(target_os = "linux")]
    #[test]
    fn shared_home_import_is_dry_run_first_explicit_and_filtered() {
        use std::os::unix::fs::{symlink, PermissionsExt};

        let _g = env_lock();
        let skein_home = tempdir();
        let work_tmp = tempdir();
        let work = work_tmp.join("work");
        let store_tmp = tempdir();
        let store = store_tmp.join("store/.claude");
        let box_home_tmp = tempdir();
        let box_home = box_home_tmp.join("box-home");
        fs::create_dir_all(&work).unwrap();
        fs::create_dir_all(&box_home).unwrap();
        assert!(Command::new("git")
            .arg("init")
            .arg(&work)
            .status()
            .unwrap()
            .success());
        ensure_store(&store).unwrap();
        symlink(&store, work.join(".claude")).unwrap();
        fs::write(box_home.join("CASE_PREP.md"), "questions").unwrap();
        fs::create_dir_all(box_home.join("samples/target")).unwrap();
        fs::write(box_home.join("samples/reference.pdf"), "pdf").unwrap();
        fs::write(box_home.join("samples/locked.json"), "unreadable").unwrap();
        fs::set_permissions(
            box_home.join("samples/locked.json"),
            fs::Permissions::from_mode(0o000),
        )
        .unwrap();
        fs::write(box_home.join("samples/.env"), "SECRET=never").unwrap();
        fs::write(box_home.join("samples/target/build.bin"), "large").unwrap();
        fs::create_dir_all(box_home.join(".ssh")).unwrap();
        fs::write(box_home.join(".ssh/id_ed25519"), "private").unwrap();
        fs::create_dir_all(box_home.join("workspace")).unwrap();
        symlink("CASE_PREP.md", box_home.join("shortcut")).unwrap();

        env::set_var("SKEIN_HOME", &skein_home);
        save_repos(&[Repo {
            read_prs: false,
            id: "demo".into(),
            source: work.to_string_lossy().into_owned(),
            store: store.to_string_lossy().into_owned(),
            agent: "codex".into(),
            plane_project: String::new(),
            sync_connection: String::new(),
            review_queue: true,
            sync_gateway_url: String::new(),
        }])
        .unwrap();

        let bin_tmp = tempdir();
        let bin = bin_tmp.join("bin");
        fs::create_dir_all(&bin).unwrap();
        let sbx = bin.join("sbx");
        fs::write(
            &sbx,
            // `sbx exec <sandbox> nsenter … bash -lc <shell>`: the shell is the LAST argument whatever
            // the prefix, and it already carries its own `cd` and `export HOME` from `Place::wrap`.
            // `nsenter` itself is dropped — there is no namespace to enter in a test.
            r#"#!/usr/bin/env bash
set -e
[ "$1" = exec ]
shell="${@: -1}"
SANDBOX_VM_ID=demo-old-claude bash -c "$shell"
"#,
        )
        .unwrap();
        fs::set_permissions(&sbx, fs::Permissions::from_mode(0o755)).unwrap();
        let old_path = env::var("PATH").unwrap_or_default();
        env::set_var("PATH", format!("{}:{old_path}", bin.display()));
        env::set_var("FAKE_BOX_HOME", &box_home);
        env::set_var("FAKE_BOX_WORK", &work);
        // A real placement pointing at the fixture's own directories. That is what makes this exercise
        // `Place::wrap` rather than work around it: the wrapper cds into `tree` and exports `home`, so
        // naming the fixture there means the script under test runs where it expects to, and the fake
        // sbx below only has to be a transport.
        record_place(
            "demo-old-claude",
            &PlaceRecord {
                sandbox: "skein-fleet".into(),
                ns_pid: std::process::id(),
                home: box_home.to_string_lossy().into_owned(),
                tree: work.to_string_lossy().into_owned(),
                sock: "/boxes/demo-old-claude/session.sock".into(),
                generation: "test-boot".into(),
                ns_start: 1,
                ..Default::default()
            },
        )
        .unwrap();

        let inventory = shared_home_inventory("demo-old-claude").unwrap();
        let candidate = |name: &str| inventory.iter().find(|item| item.name == name).unwrap();
        assert!(candidate("CASE_PREP.md").eligible);
        assert!(candidate("samples").eligible);
        assert!(!candidate(".ssh").eligible);
        assert!(!candidate("workspace").eligible);
        assert!(!candidate("shortcut").eligible);
        assert!(!store.join("shared-home/CASE_PREP.md").exists());

        let result = import_shared_home(
            "demo-old-claude",
            &["CASE_PREP.md".into(), "samples".into()],
        )
        .unwrap();
        assert!(
            result.contains("locked.json"),
            "skipped path must be reported"
        );
        assert_eq!(
            fs::read_to_string(store.join("shared-home/CASE_PREP.md")).unwrap(),
            "questions"
        );
        assert!(store.join("shared-home/samples/reference.pdf").is_file());
        assert!(!store.join("shared-home/samples/locked.json").exists());
        assert!(!store.join("shared-home/samples/.env").exists());
        assert!(!store.join("shared-home/samples/target").exists());
        assert!(store
            .join("skein/imports")
            .read_dir()
            .unwrap()
            .next()
            .is_some());

        env::set_var("PATH", old_path);
        env::remove_var("FAKE_BOX_HOME");
        env::remove_var("FAKE_BOX_WORK");
        env::remove_var("SKEIN_HOME");
    }
}
