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
}
