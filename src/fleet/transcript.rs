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
pub fn realign_transcript(name: &str) -> Result<usize, String> {
    let root = std::path::PathBuf::from(box_state(name)).join("claude-projects");
    let target = root.join(transcript_slug(&format!("{}/tree", box_root(name))));
    let jsonl = |dir: &std::path::Path| -> Vec<std::path::PathBuf> {
        std::fs::read_dir(dir)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "jsonl"))
            .collect()
    };
    // BYTES, not files. A long conversation is a couple of enormous files, while the junk beside it
    // — a probe, a one-off `claude` run in /tmp, a scratchpad — is many small ones. Counting files
    // ranked a 6-file scratchpad above the 87MB conversation it was supposed to rescue, which is
    // exactly the transcript this function exists for.
    let size = |dir: &std::path::Path| -> u64 {
        jsonl(dir)
            .iter()
            .filter_map(|p| p.metadata().ok())
            .map(|m| m.len())
            .sum()
    };
    let richest = |exclude: &std::path::Path| -> Option<(std::path::PathBuf, u64)> {
        std::fs::read_dir(&root)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.is_dir() && p != exclude)
            .map(|p| {
                let bytes = size(&p);
                (p, bytes)
            })
            .filter(|(_, bytes)| *bytes > 0)
            .max_by_key(|(_, bytes)| *bytes)
    };

    let here = size(&target);
    if target.is_dir() && here > 0 {
        // Never merge one conversation into another. But say so when the box is plainly sitting on
        // the wrong one: a migration that failed part-way leaves a few small sessions here and the
        // real history beside it, and silence at this point reads as "there was nothing to carry".
        if let Some((source, bytes)) = richest(&target) {
            if bytes > here.saturating_mul(4) {
                eprintln!(
                    "skein: {name} has a {}MB conversation of its own, but {} holds {}MB — if this \
                     box was migrated, that is the older one and copying its *.jsonl across (cp -p) \
                     restores it",
                    here / 1_000_000,
                    source.display(),
                    bytes / 1_000_000,
                );
            }
        }
        return Ok(0);
    }

    let Some((source, _)) = richest(&target) else {
        return Ok(0);
    };
    std::fs::create_dir_all(&target).map_err(|e| format!("mkdir {}: {e}", target.display()))?;
    let mut moved = 0;
    for from in jsonl(&source) {
        let Some(file) = from.file_name() else {
            continue;
        };
        let to = target.join(file);
        if std::fs::copy(&from, &to).is_err() {
            continue;
        }
        moved += 1;
        // Carry the modification time too: `claude --continue` opens the most recently modified
        // transcript, and a copy stamped `now` makes whichever file landed last look like the
        // newest conversation. Best-effort — a wrong mtime is worse than no copy only in ordering.
        if let Ok(modified) = from.metadata().and_then(|m| m.modified()) {
            if let Ok(handle) = std::fs::File::options().write(true).open(&to) {
                let _ = handle.set_times(std::fs::FileTimes::new().set_modified(modified));
            }
        }
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
}
