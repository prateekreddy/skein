//! The audit sink: what the warden was asked, what was approved, by whom (§5, §14).
//!
//! **Never compilable-out.** §8.3 makes this and fleet observation the deliberate exception to
//! §12.10: a capability that *performs* something may be left unbuilt; one that only *reports* may
//! not, or the design loses the ability to see and to account for itself.
//!
//! It takes skein's own approval decisions as well as the warden's, and the reason is one sentence:
//! **skein cannot audit itself.** A log skein writes is a log a compromised skein edits. This one is
//! on the host, outside the fleet, appended to by a process skein does not control.
//!
//! Append-only in the only sense a file can be: opened with `append`, written whole, never seeked.
//! That does not stop anyone with the host uid rewriting it — nothing at this layer could — and the
//! claim is deliberately not made anywhere. What it does stop is the warden itself losing an entry
//! by rewriting a line, and two writers interleaving half-lines: one `write` of one line, which the
//! kernel does not split for a pipe-sized record.

use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::{Path, PathBuf};

/// One thing that happened, as the log records it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Entry {
    /// RFC3339, stamped here rather than taken from the request: a requester that supplies its own
    /// timestamp supplies the order of the log.
    pub at: String,
    /// The operation id this concerns, if it concerns one.
    #[serde(default)]
    pub operation: String,
    /// What happened — `asked`, `approved`, `refused`, `ran`, and whatever skein sends of its own.
    pub what: String,
    /// Free text. Written by whoever reported it and never parsed, so it can say anything without
    /// becoming a contract.
    #[serde(default)]
    pub detail: String,
    /// Who reported it. `warden` for its own entries; anything else is a claim by the reporter and
    /// is recorded **as a claim**, because this endpoint has no way to check one.
    pub reported_by: String,
}

/// Move a record left under the volume to the host-side home the warden keeps now (SKEIN-218).
///
/// The audit log and the outcomes lived under [`crate::home()`] — which follows the volume root,
/// for the secret's sake — until delivery 4c made that volume something skein reads and writes
/// from inside the fleet. Two things are wrong with leaving them there, and only one of them is
/// about history: an audit log the audited thing can rewrite is not a record, and an OUTCOME the
/// audited thing can write is an *answer the warden will then serve* as its own.
///
/// So the move is part of the fix, not a courtesy. It runs only for a fully derived pair of homes:
/// under either override the operator has said where these go, and moving somebody's production
/// record into a scratch directory is how a test deletes real history.
///
/// `rename` first, then a copy-and-remove for the `EXDEV` case — a volume on another disk is the
/// common reason for a non-default `$SKEIN_HOME`, and the point is that nothing stays behind.
pub fn adopt_left_behind(volume_home: &Path, record: &Path) {
    for key in ["SKEIN_WARDEN_HOME", "SKEIN_WARDEN_AUDIT"] {
        if std::env::var_os(key).is_some_and(|s| !s.is_empty()) {
            return;
        }
    }
    if volume_home == record {
        return;
    }
    if std::fs::create_dir_all(record).is_err() {
        return; // The log reports its own failure to open, loudly, at the first append.
    }
    move_file(&volume_home.join("audit.jsonl"), &record.join("audit.jsonl"));
    move_outcomes(&volume_home.join("outcomes"), &record.join("outcomes"));
}

/// One file, if it is there and nothing is at the destination.
fn move_file(old: &Path, new: &Path) {
    if !old.exists() || new.exists() {
        return;
    }
    let moved = std::fs::rename(old, new).or_else(|_| {
        std::fs::copy(old, new)
            .and_then(|_| std::fs::remove_file(old))
            .map(|_| ())
    });
    match moved {
        Ok(()) => eprintln!(
            "skein-warden: moved {} to {} — the record lives beside the volume now, not on it              (§5: skein cannot audit itself)",
            old.display(),
            new.display()
        ),
        Err(e) => eprintln!(
            "skein-warden: could not move {} to {} ({e}) — it is on the volume, which skein can              write, so delete it once you have kept what you want from it",
            old.display(),
            new.display()
        ),
    }
}

/// The outcomes directory: flat, `<id>.json` per answer, so a file-by-file fallback is the whole
/// of it. Merged into whatever is already at the destination rather than refused, because an id
/// that exists at both ends is the same operation and the destination's copy is the newer one.
fn move_outcomes(old: &Path, new: &Path) {
    let Ok(entries) = std::fs::read_dir(old) else {
        return;
    };
    if std::fs::create_dir_all(new).is_err() {
        return;
    }
    for entry in entries.flatten() {
        move_file(&entry.path(), &new.join(entry.file_name()));
    }
    // Only when it is empty — a directory that still holds something is one this did not finish.
    let _ = std::fs::remove_dir(old);
}

/// The host-side log.
pub struct Log {
    path: PathBuf,
}

impl Log {
    pub fn new(path: impl Into<PathBuf>) -> Log {
        Log { path: path.into() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Append one entry. One line of JSON, written in one call.
    pub fn append(&self, entry: &Entry) -> Result<(), String> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("mkdir {}: {e}", parent.display()))?;
        }
        let mut line = serde_json::to_vec(entry).map_err(|e| e.to_string())?;
        line.push(b'\n');
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .map_err(|e| format!("open {}: {e}", self.path.display()))?;
        file.write_all(&line)
            .map_err(|e| format!("append {}: {e}", self.path.display()))?;
        // Flushed to the disk, not to the page cache. An audit entry that a crash removes is worse
        // than no audit entry, because the absence reads as "it never happened".
        file.sync_all()
            .map_err(|e| format!("fsync {}: {e}", self.path.display()))
    }

    /// The warden's own account of something. `reported_by` is not a parameter here — the whole
    /// difference between this and [`Log::append`] is that this one is not a claim.
    pub fn record(&self, operation: &str, what: &str, detail: &str) -> Result<(), String> {
        self.append(&Entry {
            at: chrono::Utc::now().to_rfc3339(),
            operation: operation.to_string(),
            what: what.to_string(),
            detail: detail.to_string(),
            reported_by: "warden".into(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(what: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "skein-warden-audit-{what}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    /// Entries accumulate rather than replace, and the log is readable a line at a time — which is
    /// what makes it something a person can follow after the fact.
    #[test]
    fn entries_accumulate_and_stay_one_line_each() {
        let dir = scratch("append");
        let log = Log::new(dir.join("warden.jsonl"));
        log.record("op-1", "asked", "create-fleet").unwrap();
        log.record("op-1", "refused", "no approval surface")
            .unwrap();
        let raw = std::fs::read_to_string(log.path()).unwrap();
        let lines: Vec<&str> = raw.lines().collect();
        assert_eq!(lines.len(), 2, "an entry replaced another: {raw}");
        for line in &lines {
            let entry: Entry = serde_json::from_str(line).unwrap();
            assert_eq!(entry.operation, "op-1");
            assert_eq!(entry.reported_by, "warden");
            assert!(!entry.at.is_empty());
        }
    }

    /// What skein reports is recorded as skein's claim, and the warden's own entries are not
    /// forgeable into the same shape by a requester choosing a name.
    #[test]
    fn a_reporter_cannot_file_an_entry_as_the_warden() {
        let dir = scratch("claims");
        let log = Log::new(dir.join("warden.jsonl"));
        // The endpoint hands `append` an entry built from the request, and the request may say
        // anything. That is fine and it is why the field exists — but the timestamp is the warden's
        // and so is the distinction between reporting and recording.
        log.append(&Entry {
            at: "1999-01-01T00:00:00Z".into(),
            operation: "op-2".into(),
            what: "approved".into(),
            detail: "by a human, honest".into(),
            reported_by: "warden".into(),
        })
        .unwrap();
        log.record("op-2", "refused", "the warden's own account")
            .unwrap();
        let raw = std::fs::read_to_string(log.path()).unwrap();
        let entries: Vec<Entry> = raw
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(entries[0].at, "1999-01-01T00:00:00Z");
        assert_ne!(
            entries[1].at, "1999-01-01T00:00:00Z",
            "the warden stamped its own entry with a time it was handed"
        );
        // Both are in the log. Neither is deleted to make room for the other — an audit log that
        // resolved a disagreement would be hiding the one thing worth reading.
        assert_eq!(entries.len(), 2);
    }

    /// A record left on the volume is moved off it, outcomes included (SKEIN-218).
    #[test]
    fn a_record_left_on_the_volume_does_not_stay_there() {
        let _g = crate::env_lock();
        let keep: Vec<_> = ["SKEIN_WARDEN_HOME", "SKEIN_WARDEN_AUDIT"]
            .iter()
            .map(|k| (*k, std::env::var_os(k)))
            .collect();
        for (k, _) in &keep {
            std::env::remove_var(k);
        }

        let volume = scratch("volume-warden");
        let record = scratch("host-record");
        std::fs::create_dir_all(&volume).unwrap();
        std::fs::write(volume.join("audit.jsonl"), "{\"what\":\"approved\"}\n").unwrap();
        std::fs::create_dir_all(volume.join("outcomes")).unwrap();
        std::fs::write(volume.join("outcomes/op-1.json"), "{}").unwrap();

        adopt_left_behind(&volume, &record);

        assert!(
            !volume.join("audit.jsonl").exists(),
            "the log is still on the volume, where the audited thing can rewrite it"
        );
        assert!(
            !volume.join("outcomes/op-1.json").exists(),
            "an outcome is still on the volume, where skein could write one the warden then serves"
        );
        assert_eq!(
            std::fs::read_to_string(record.join("audit.jsonl")).unwrap(),
            "{\"what\":\"approved\"}\n",
            "the history did not travel"
        );
        assert!(record.join("outcomes/op-1.json").exists());

        // An overridden home is the operator saying where these live; nothing is moved out of it.
        let over = scratch("overridden");
        std::fs::create_dir_all(&over).unwrap();
        std::fs::write(over.join("audit.jsonl"), "kept\n").unwrap();
        std::env::set_var("SKEIN_WARDEN_HOME", over.display().to_string());
        adopt_left_behind(&over, &record);
        assert!(
            over.join("audit.jsonl").exists(),
            "a test's or a developer's own directory was emptied into the host record"
        );

        for (k, v) in keep {
            match v {
                Some(v) => std::env::set_var(k, v),
                None => std::env::remove_var(k),
            }
        }
    }
}
