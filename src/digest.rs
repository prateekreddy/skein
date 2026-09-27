//! What a box has been doing, assembled from sources that cost nothing.
//!
//! The inbox detail view answers "what happened here" without a model call: the registry state, the
//! host-side diff and commits, the agent's own journal, its last reported message, and why the turn
//! ended. Every one of those is a file somebody already wrote, so the answer is free and stays
//! accurate — a summary a model produced would be neither.
//!
//! Each ingredient is also readable on its own, because they have separate callers: `read_journal`
//! feeds turn state, `recent_commits` feeds the board, and `session_digest` is the assembly.

use crate::diff::{git_range, read_diffstat_file, DiffStat};
use crate::registry::Sandbox;
use crate::registry::{registry_entry_for_box, store_for_box};
use crate::repos::agent_for_box;
use crate::repos::branch_of;
use crate::sbx::box_liveness;
use crate::sbx::lookup_dir;
use crate::signals::{classify_message, session_signal, turn_state, Pause};
use crate::util::valid_name;
use crate::util::{bounded_output, keep_tail};
use serde::Serialize;
use std::fs;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

/// Recent commit subjects on the box's branch (newest first) — the agent's own changelog,
/// a free, accurate "what was done" with no model call. For direct-mode boxes, computed host-side.
/// For clone-mode boxes, reads the file box-diff.sh wrote. Empty when neither is available.
pub fn recent_commits(name: &str) -> Vec<String> {
    // Prefer box-diff.sh's commit file: the box is on the feature branch and knows its own
    // commits. Host-side git would run against whatever `dir` resolves to on the host — for
    // clone-mode boxes that's the HOST's checkout (main), which lists the wrong commits.
    if let Some(path) = store_for_box(name)
        .map(|s| s.join("diffs").join(format!("{name}.commits")))
        .filter(|p| p.exists())
    {
        if let Ok(s) = fs::read_to_string(&path) {
            let v: Vec<String> = s
                .lines()
                .map(|l| l.trim().to_string())
                .filter(|l| !l.is_empty())
                .collect();
            if !v.is_empty() {
                return v;
            }
        }
    }
    // Fall back to host-side git — useful for direct-mode boxes where the host dir IS the
    // box's working tree (box-diff.sh may not have run yet on a fresh box).
    if let Some(dir) = lookup_dir(name) {
        if let Some(range) = git_range(&dir) {
            let r = format!("{range}..HEAD");
            let mut command = Command::new("git");
            command.args(["-C", &dir, "log", "--format=%s", "-n", "20", &r]);
            if let Ok(out) = bounded_output(&mut command, "git log", Duration::from_secs(15)) {
                if out.status.success() {
                    let v: Vec<String> = String::from_utf8_lossy(&out.stdout)
                        .lines()
                        .map(|s| s.to_string())
                        .filter(|s| !s.is_empty())
                        .collect();
                    if !v.is_empty() {
                        return v;
                    }
                }
            }
        }
    }
    vec![]
}

/// The agent's own turn-end journal (`.skein/journal.md`), if it keeps one — the best "what was
/// done" source because it's written with full context (see the CLAUDE.md ritual). Returns the tail
/// (last ~40 lines), capped, or None when the box keeps no journal.
///
/// Reads `<store>/journals/<vmid>.md` first — box-journal.sh's Stop-hook copy of the box's own
/// `.skein/journal.md`, the only way the host can see it for a clone-mode box (a box's private clone
/// isn't visible to the host at all; `dir` for a repo box is the *shared* host working clone, not the
/// box's own). Falls back to reading `<dir>/.skein/journal.md` directly for a direct-mode box, where
/// the host-mounted repo genuinely is the box's own working tree.
pub fn read_journal(name: &str) -> Option<String> {
    let from_store = store_for_box(name)
        .map(|s| s.join("journals").join(format!("{name}.md")))
        .and_then(|p| fs::read_to_string(p).ok());
    let dir = lookup_dir(name);
    let from_dir = dir
        .as_deref()
        .and_then(|d| fs::read_to_string(Path::new(d).join(".skein").join("journal.md")).ok());
    let txt = from_store.or(from_dir)?;
    let tail: Vec<&str> = txt.lines().rev().take(40).collect();
    let s: String = tail.into_iter().rev().collect::<Vec<_>>().join("\n");
    // The tail, because a journal's last entry is the one worth showing. Counted in chars: this was
    // a byte slice, and a journal long enough to cut with an `…` anywhere in it panicked the thread
    // doing the cutting — see `keep_tail`.
    const CAP: usize = 4000;
    let s = keep_tail(&s, CAP);
    Some(s).filter(|s| !s.trim().is_empty())
}

/// A glanceable "what happened here" for one box — the inbox detail view (steps 1–5), assembled
/// entirely from free sources: the registry state, the host-side diff/commits, the agent's own
/// journal, and its last reported message. No model tokens spent.
#[derive(Debug, Default, Serialize)]
pub struct SessionDigest {
    pub name: String,
    pub branch: String,
    pub state: String,
    pub tier: u8,
    pub age: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diff: Option<DiffStat>,
    /// the agent's own changelog (recent commit subjects, newest first)
    pub commits: Vec<String>,
    /// the agent's turn-end journal, if it keeps `.skein/journal.md`
    #[serde(skip_serializing_if = "Option::is_none")]
    pub journal: Option<String>,
    /// the last assistant message (Stop) — the turn's own sign-off
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_message: Option<String>,
    /// the prompt the agent is blocked on (Notification), if any
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blocked_on: Option<String>,
    /// when the box last reported a narrative signal (RFC3339)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signal_ts: Option<String>,
    /// why the turn ended — drives the inbox ranking and CTA
    pub pause: Pause,
}

/// Assemble the free session digest for a box. Pure reads + a couple of cached `git` calls;
/// never a model call. Returns None for an unknown box name.
pub fn session_digest(name: &str) -> Option<SessionDigest> {
    if !valid_name(name) {
        return None;
    }
    // Registry-independent: dir/branch from sbx + host git, state from sbx liveness + skein's probe,
    // the registry only a fallback. The box must be known to sbx or the registry (else nothing to show).
    let reg = registry_entry_for_box(name);
    let live = box_liveness(name);
    let dir = lookup_dir(name).unwrap_or_default();
    if reg.is_none() && live.is_none() && dir.is_empty() {
        return None;
    }
    let branch = branch_of(name).unwrap_or_default();
    let sb = Sandbox {
        branch: branch.clone(),
        dir: dir.clone(),
        last_seen: reg
            .as_ref()
            .map(|r| r.last_seen.clone())
            .unwrap_or_default(),
        // The board's own reader, fused with the screen, so this digest — and the session API and
        // handoff brief built from it — says what the board's row for this box says.
        status: turn_state(name, &agent_for_box(name))
            .status
            .unwrap_or_default(),
    };
    let (state, tier) = sb.state_with(live);
    let blocked = state == "needs-input";

    let sig = session_signal(name);
    let last_message = sig
        .as_ref()
        .map(|s| s.last_message.clone())
        .filter(|m| !m.trim().is_empty());
    let blocked_on = sig
        .as_ref()
        .filter(|s| s.kind == "notification")
        .map(|s| s.prompt.clone())
        .filter(|p| !p.trim().is_empty());
    // classify over whichever text we have (the prompt when blocked, else the last message)
    let class_text = blocked_on
        .clone()
        .or_else(|| last_message.clone())
        .unwrap_or_default();
    let pause = match tier {
        3 => Pause::None, // still working — nothing owed
        _ => classify_message(&class_text, blocked),
    };

    Some(SessionDigest {
        name: name.to_string(),
        branch,
        state,
        tier,
        age: sb.age(),
        diff: read_diffstat_file(name),
        commits: recent_commits(name),
        journal: read_journal(name),
        last_message,
        blocked_on,
        signal_ts: sig.map(|s| s.ts).filter(|t| !t.is_empty()),
        pause,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::*;
    use std::env;

    #[test]
    fn read_journal_prefers_store_over_host_dir() {
        // Simulates the clone-mode bug directly: `dir` (the registered box dir) is the HOST's
        // shared working clone, which never has the box's own `.skein/journal.md` — only
        // box-journal.sh's copy in the store does. read_journal must find it there.
        let _g = env_lock();
        let dir = tempdir();
        // Pinned, because `config::skein_home` refuses an unpinned test rather than answering
        // with the real `~/.skein` — where this fixture's state would otherwise land (SKEIN-626).
        env::set_var("SKEIN_HOME", &dir);
        let work = dir.join("work");
        fs::create_dir_all(&work).unwrap(); // no .skein/journal.md here — the clone-mode case
        let reg = dir.join("sandboxes.json");
        fs::write(
            &reg,
            format!(
                r#"{{"thing-x":{{"branch":"x","dir":"{}","lastSeen":"","status":""}}}}"#,
                work.display()
            ),
        )
        .unwrap();
        env::set_var("SKEIN_REGISTRY", &reg);
        env::remove_var("SKEIN_SHARED");

        // Nothing in the store yet either → None, not a panic.
        assert_eq!(read_journal("thing-x"), None);

        // box-journal.sh's copy lands in <store>/journals/<name>.md.
        fs::create_dir_all(dir.join("journals")).unwrap();
        fs::write(
            dir.join("journals").join("thing-x.md"),
            "did: x / next: y / blocked-on: reviewer\n",
        )
        .unwrap();
        assert!(read_journal("thing-x")
            .unwrap()
            .contains("blocked-on: reviewer"));

        // Direct-mode compatibility: with no store copy, falls back to the host dir directly.
        fs::remove_file(dir.join("journals").join("thing-x.md")).unwrap();
        fs::create_dir_all(work.join(".skein")).unwrap();
        fs::write(
            work.join(".skein").join("journal.md"),
            "did: a / next: b / blocked-on: nothing\n",
        )
        .unwrap();
        assert!(read_journal("thing-x")
            .unwrap()
            .contains("blocked-on: nothing"));

        env::remove_var("SKEIN_REGISTRY");
        env::remove_var("SKEIN_HOME");
    }
}
