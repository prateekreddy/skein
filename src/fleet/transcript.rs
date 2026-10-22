//! Moving a box's conversation transcript to follow the box to a new checkout.

use super::*;

/// Claude's transcript directory for a working directory: every `/` and `.` becomes `-`.
///
/// Not a guess — read off a real box: `/Users/you/.skein/repos/sync/work` is stored as
/// `-Users-you--skein-repos-sync-work` (the `/.` giving the doubled dash).
pub fn transcript_slug(dir: &str) -> String {
    dir.chars()
        .map(|c| if c == '/' || c == '.' { '-' } else { c })
        .collect()
}

/// Point a migrated box's conversation at the directory it now works in.
///
/// The runtimes key a transcript by the cwd it was had in, and migrating a box MOVES that cwd: from
/// the old sandbox's checkout to `/boxes/<name>/tree`. So the conversation travels in the snapshot,
/// lands intact — and the agent looks under a slug for its new path, finds nothing, and starts over.
/// Measured on the first real migration: 25MB of transcript under
/// `-Users-you--skein-repos-sync-work`, and an empty `-boxes-<name>-tree` beside it.
///
/// Copy rather than move, and only into an empty destination: the old directory is the record of
/// where that conversation actually happened, and a box that already has a conversation of its own
/// must never have someone else's merged into it.
///
/// **Nothing here follows a link** (SKEIN-1253). `claude-projects/` is bound read-write into the box,
/// so every name under it is the box's to choose, and this runs as skein. A `<slug>/a.jsonl` that is
/// a link to a credential would be copied in as a conversation, for the box to read on its next
/// start; a link at the destination slug would be written through. So each level is opened with
/// [`crate::boxfile`] — real directories and regular files only — and a destination that is a link
/// is refused, as `gitgate::refresh_tokens` refuses one at a token directory (§9.5 R8).
pub fn realign_transcript(name: &str) -> Result<usize, String> {
    use crate::boxfile::{self, Dir};
    use std::ffi::OsString;
    use std::os::unix::fs::OpenOptionsExt;

    let root_path = std::path::PathBuf::from(box_state(name)).join("claude-projects");
    let target_name = OsString::from(transcript_slug(&format!("{}/tree", box_root(name))));
    let refused = |path: &std::path::Path| {
        format!(
            "{} is a symbolic link, so copying the conversation through it would write or read \
             somewhere skein did not choose. Nothing was copied. Remove the link and start {name} \
             again.",
            path.display()
        )
    };
    let is_link = |path: &std::path::Path| {
        std::fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink())
    };
    if is_link(&root_path) {
        return Err(refused(&root_path));
    }
    // No directory is nothing to carry, as it always was.
    let Ok(root) = Dir::open(&root_path) else {
        return Ok(0);
    };
    // The regular `*.jsonl` files directly in `dir`, with their lengths. `DirEntry::metadata` does
    // not follow a link, so a link is neither a file nor a directory here.
    let jsonl = |dir: &Dir| -> Vec<(OsString, u64)> {
        std::fs::read_dir(dir.path(""))
            .into_iter()
            .flatten()
            .flatten()
            .filter(|e| {
                std::path::Path::new(&e.file_name())
                    .extension()
                    .is_some_and(|x| x == "jsonl")
            })
            .filter_map(|e| {
                let meta = e.metadata().ok()?;
                meta.file_type()
                    .is_file()
                    .then(|| (e.file_name(), meta.len()))
            })
            .collect()
    };
    // BYTES, not files. A long conversation is a couple of enormous files, while the junk beside it
    // — a probe, a one-off `claude` run in /tmp, a scratchpad — is many small ones. Counting files
    // ranked a 6-file scratchpad above the 87MB conversation it was supposed to rescue, which is
    // exactly the transcript this function exists for.
    let size = |dir: &Dir| -> u64 { jsonl(dir).iter().map(|(_, len)| len).sum() };
    let richest = || -> Option<(OsString, Dir, u64)> {
        std::fs::read_dir(root.path(""))
            .into_iter()
            .flatten()
            .flatten()
            .filter(|e| e.file_name() != target_name)
            .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
            .filter_map(|e| {
                let dir = Dir::open(&root.path(e.file_name())).ok()?;
                let bytes = size(&dir);
                Some((e.file_name(), dir, bytes))
            })
            .filter(|(_, _, bytes)| *bytes > 0)
            .max_by_key(|(_, _, bytes)| *bytes)
    };

    let target_path = root_path.join(&target_name);
    if is_link(&target_path) {
        return Err(refused(&target_path));
    }
    let here = Dir::open(&root.path(&target_name))
        .map(|dir| size(&dir))
        .unwrap_or(0);
    if here > 0 {
        // Never merge one conversation into another. But say so when the box is plainly sitting on
        // the wrong one: a migration that failed part-way leaves a few small sessions here and the
        // real history beside it, and silence at this point reads as "there was nothing to carry".
        if let Some((source, _, bytes)) = richest() {
            if bytes > here.saturating_mul(4) {
                eprintln!(
                    "skein: {name} has a {}MB conversation of its own, but {} holds {}MB — if this \
                     box was migrated, that is the older one and copying its *.jsonl across (cp -p) \
                     restores it",
                    here / 1_000_000,
                    root_path.join(source).display(),
                    bytes / 1_000_000,
                );
            }
        }
        return Ok(0);
    }

    let Some((_, source, _)) = richest() else {
        return Ok(0);
    };
    match std::fs::create_dir(root.path(&target_name)) {
        Err(e) if e.kind() != std::io::ErrorKind::AlreadyExists => {
            return Err(format!("mkdir {}: {e}", target_path.display()));
        }
        _ => {}
    }
    // Opened without following, so a link put here since the check above is refused rather than
    // written through.
    let target = Dir::open(&root.path(&target_name)).map_err(|e| match is_link(&target_path) {
        true => refused(&target_path),
        false => format!("open {}: {e}", target_path.display()),
    })?;
    let mut moved = 0;
    for (file, _) in jsonl(&source) {
        let Ok((mut from, meta)) = boxfile::open(&source.path(&file)) else {
            continue;
        };
        // Into a temp beside the destination, then renamed over it: `create_new` refuses a link at
        // the temp's name, and a rename replaces a link at the destination rather than following it.
        let mut tmp_name = OsString::from(".");
        tmp_name.push(&file);
        tmp_name.push(format!(".skein-copy.{}", std::process::id()));
        let tmp = target.path(&tmp_name);
        let copied = (|| -> std::io::Result<()> {
            let mut to = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&tmp)?;
            std::io::copy(&mut from, &mut to)?;
            to.set_permissions(meta.permissions())?;
            // Carry the modification time too: `claude --continue` opens the most recently modified
            // transcript, and a copy stamped `now` makes whichever file landed last look like the
            // newest conversation. Best-effort — a wrong mtime is worse than no copy only in
            // ordering.
            if let Ok(modified) = meta.modified() {
                let _ = to.set_times(std::fs::FileTimes::new().set_modified(modified));
            }
            std::fs::rename(&tmp, target.path(&file))
        })();
        if copied.is_err() {
            let _ = std::fs::remove_file(&tmp);
            continue;
        }
        moved += 1;
    }
    Ok(moved)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::*;

    // A migrated box works in a new directory, and the runtimes key a transcript by that directory.
    // Without this the conversation arrives intact and invisible.
    #[test]
    fn a_migrated_boxs_conversation_follows_it_to_the_new_checkout() {
        use std::{env, fs};
        let _g = env_lock();
        let home = tempdir();
        env::set_var("SKEIN_HOME", &home);
        env::set_var("SKEIN_FLEET_ROOT", "/boxes");
        assert_eq!(
            transcript_slug("/Users/you/.skein/repos/sync/work"),
            "-Users-you--skein-repos-sync-work",
            "the slug rule is read off a real box, not invented"
        );

        let projects = std::path::PathBuf::from(box_state("sample-master")).join("claude-projects");
        let old = projects.join("-Users-you--skein-repos-sync-work");
        fs::create_dir_all(&old).unwrap();
        fs::write(old.join("a.jsonl"), "{}").unwrap();
        fs::write(old.join("b.jsonl"), "{}").unwrap();
        // an empty slug from a one-off command elsewhere must not win
        fs::create_dir_all(projects.join("-tmp")).unwrap();

        assert_eq!(realign_transcript("sample-master").unwrap(), 2);
        let now = projects.join("-boxes-sample-master-tree");
        assert!(now.join("a.jsonl").exists() && now.join("b.jsonl").exists());
        assert!(
            old.join("a.jsonl").exists(),
            "copied, not moved — the old directory is the record of where it happened"
        );

        // A box with its own conversation must never have another merged into it.
        fs::write(now.join("own.jsonl"), "{}").unwrap();
        fs::write(old.join("c.jsonl"), "{}").unwrap();
        assert_eq!(realign_transcript("sample-master").unwrap(), 0);
        assert!(!now.join("c.jsonl").exists());

        env::remove_var("SKEIN_FLEET_ROOT");
        env::remove_var("SKEIN_HOME");
    }

    /// Which sibling is "the conversation" is a question about BYTES, not files.
    ///
    /// Read off lattice-feat-design-codex-claude, whose real history was 2 files totalling 87MB
    /// while the scratchpad slugs beside it held 6 small ones each. Ranking by file count picked a
    /// scratchpad — the one outcome this function exists to prevent, arrived at silently.
    #[test]
    fn the_conversation_carried_across_is_the_biggest_one_not_the_busiest_directory() {
        use std::{env, fs};
        let _g = env_lock();
        let home = tempdir();
        env::set_var("SKEIN_HOME", &home);
        env::set_var("SKEIN_FLEET_ROOT", "/boxes");

        let projects = std::path::PathBuf::from(box_state("lattice-main")).join("claude-projects");
        let real = projects.join("-Users-you-work-lattice");
        fs::create_dir_all(&real).unwrap();
        fs::write(real.join("long.jsonl"), vec![b'x'; 60_000]).unwrap();
        fs::write(real.join("second.jsonl"), vec![b'x'; 20_000]).unwrap();
        // More files, a fraction of the content — a scratchpad, not a conversation.
        let junk = projects.join("-tmp-scratchpad");
        fs::create_dir_all(&junk).unwrap();
        for i in 0..6 {
            fs::write(junk.join(format!("{i}.jsonl")), vec![b'x'; 500]).unwrap();
        }

        assert_eq!(realign_transcript("lattice-main").unwrap(), 2);
        let now = projects.join("-boxes-lattice-main-tree");
        assert!(
            now.join("long.jsonl").exists(),
            "the 80KB conversation, not the 3KB spread over six files"
        );
        assert!(
            !now.join("0.jsonl").exists(),
            "the scratchpad must not arrive"
        );

        // `claude --continue` opens the most recently modified transcript, so a copy stamped `now`
        // would make whichever file landed last look like the newest conversation.
        let src = fs::metadata(real.join("long.jsonl"))
            .unwrap()
            .modified()
            .unwrap();
        let dst = fs::metadata(now.join("long.jsonl"))
            .unwrap()
            .modified()
            .unwrap();
        assert_eq!(
            src, dst,
            "the copy must keep the original's modification time"
        );

        env::remove_var("SKEIN_FLEET_ROOT");
        env::remove_var("SKEIN_HOME");
    }

    // `claude-projects/` is the box's to write, and this copy runs as skein on every start. Two
    // links a box could plant in the source, both of which the old copy followed: a `*.jsonl`
    // linked to a file outside, copied in as conversation for the box to read, and a slug directory
    // linked to a directory outside, ranked on its bytes and copied from. Fails if the walk follows
    // either: the linked slug outranks the real one, or the linked file arrives in the box's slug.
    #[test]
    fn a_conversation_is_never_carried_through_a_link_the_box_planted() {
        use std::{fs, os::unix::fs::symlink};
        let _g = env_lock();
        let home = tempdir();
        let mut pins = env_pins();
        pins.set("SKEIN_HOME", &home)
            .set("SKEIN_FLEET_ROOT", home.join("fleet"));
        let outside = home.join("outside");
        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join("credential"), "skein-test-not-a-secret").unwrap();

        let projects = std::path::PathBuf::from(box_state("link-box")).join("claude-projects");
        let old = projects.join("-Users-you-work-link");
        fs::create_dir_all(&old).unwrap();
        fs::write(old.join("real.jsonl"), "{\"the\":\"conversation\"}").unwrap();
        symlink(outside.join("credential"), old.join("planted.jsonl")).unwrap();
        // A bigger "conversation" outside, reached by a linked slug: it would win on bytes.
        let big = home.join("big");
        fs::create_dir_all(&big).unwrap();
        fs::write(big.join("huge.jsonl"), vec![b'x'; 100_000]).unwrap();
        symlink(&big, projects.join("-linked-slug")).unwrap();

        assert_eq!(realign_transcript("link-box").unwrap(), 1);
        let now = projects.join(transcript_slug(&format!("{}/tree", box_root("link-box"))));
        assert!(
            now.join("real.jsonl").is_file(),
            "the box's own file is carried"
        );
        assert!(
            fs::symlink_metadata(now.join("planted.jsonl")).is_err(),
            "a linked file was copied in"
        );
        assert!(
            fs::symlink_metadata(now.join("huge.jsonl")).is_err(),
            "a linked slug was ranked and copied from"
        );
    }

    // The destination slug a link to a directory outside, on a box with nothing of its own yet.
    // Fails if the link is followed: the copy lands in `redirected`, and `realign_transcript`
    // answers `Ok` rather than refusing.
    #[test]
    fn a_conversation_is_never_written_through_a_linked_destination() {
        use std::{fs, os::unix::fs::symlink};
        let _g = env_lock();
        let home = tempdir();
        let mut pins = env_pins();
        pins.set("SKEIN_HOME", &home)
            .set("SKEIN_FLEET_ROOT", home.join("fleet"));
        let projects = std::path::PathBuf::from(box_state("dest-box")).join("claude-projects");
        let old = projects.join("-Users-you-work-dest");
        fs::create_dir_all(&old).unwrap();
        fs::write(old.join("real.jsonl"), "{}").unwrap();
        let dest = projects.join(transcript_slug(&format!("{}/tree", box_root("dest-box"))));
        let redirected = home.join("redirected");
        fs::create_dir_all(&redirected).unwrap();
        symlink(&redirected, &dest).unwrap();

        let answer = realign_transcript("dest-box");
        assert_eq!(
            fs::read_dir(&redirected).unwrap().count(),
            0,
            "the copy was written through a linked destination"
        );
        let refused = answer.expect_err("a linked destination is refused, and says so");
        assert!(refused.contains("symbolic link"), "{refused}");
    }
}
