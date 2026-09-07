//! An attempt at a long operation, recorded — so that **owed** and **in flight** are different
//! observations (architecture §2.4).
//!
//! Without this they are the same one. Creating the fleet sandbox takes minutes, and its check
//! fails for every second of them: anything that reconciles on a failing check starts a second
//! create over the first. `ensure_fleet` runs on every box start, so two boxes started together are
//! exactly that race, and today the only thing between them is that nobody has written the
//! reconciler yet.
//!
//! **Recorded state on the volume** (§5), at `~/.skein/attempts/<name>.json`, so it survives a skein
//! restart — an operation started by a process that then died must not look owed to the next one
//! until its deadline passes, or the restart itself becomes the way to start a second copy.
//!
//! **The deadline is the whole of the reclaim policy.** A holder that dies leaves its record behind,
//! and nothing can distinguish that from one still working; so the only safe answer is time. Held
//! past its deadline, an attempt is taken over and said so.
//!
//! A closure rather than a guard, for the reason [`crate::util::with_lock`] gives: a guard can be
//! dropped early by accident and the accident is invisible.

use crate::util::{update_json_lossy, valid_name};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::Duration;

/// What one attempt records. Every field is here so a person reading the file can answer "who, and
/// since when" without the process that wrote it.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Attempt {
    /// The process that holds it, as `<pid>@<host-boot>` — enough to tell two skeins apart, and
    /// never used to decide anything. Only the deadline decides.
    pub holder: String,
    /// RFC3339, so the report can say how long it has been going without doing arithmetic on it.
    pub started_at: String,
    /// RFC3339. Past this, the attempt is somebody else's to take.
    pub deadline: String,
}

impl Attempt {
    /// Is this record still somebody's? Empty is nobody's; so is one whose deadline has passed.
    fn live(&self, now: chrono::DateTime<chrono::Utc>) -> bool {
        if self.holder.is_empty() {
            return false;
        }
        chrono::DateTime::parse_from_rfc3339(&self.deadline)
            .map(|deadline| deadline > now)
            .unwrap_or(false)
    }

    /// How long this attempt has been going, in words.
    pub fn age(&self) -> String {
        let Ok(started) = chrono::DateTime::parse_from_rfc3339(&self.started_at) else {
            return "an unknown time".into();
        };
        let seconds = (chrono::Utc::now() - started.with_timezone(&chrono::Utc)).num_seconds();
        match seconds {
            s if s < 0 => "no time".into(),
            s if s < 90 => format!("{s}s"),
            s => format!("{}m", s / 60),
        }
    }
}

/// What [`attempt`] did.
#[derive(Debug)]
pub enum Outcome<T> {
    /// It was ours to make, and this is what the work returned.
    Ran(T),
    /// Somebody else is already doing it, and has not run out of time.
    InFlight(Attempt),
}

/// Where one attempt is recorded, under a directory the caller names.
///
/// The directory is a parameter and not `skein_home()`, which is not fastidiousness: depending on
/// `config` to find out where the volume is would put this module inside the eighteen-module knot
/// that `docs/modules.toml` records, and `tools/module-check.py` said so the moment it did. A lease
/// mechanism that had to ask another module anything is a lease that module cannot take.
pub fn attempt_path(dir: &std::path::Path, name: &str) -> PathBuf {
    dir.join(format!("{name}.json"))
}

/// This process, distinguishably. Not an identity anything trusts — see [`Attempt::holder`].
fn holder() -> String {
    format!(
        "{}@{}",
        std::process::id(),
        std::fs::read_to_string("/proc/sys/kernel/random/boot_id")
            .map(|id| id.trim().to_string())
            .unwrap_or_else(|_| "unknown".into())
    )
}

/// Do the work, unless somebody else already is.
///
/// The claim is a read-modify-write under the file's own lock
/// ([`crate::util::update_json_lossy`]), so two processes arriving together cannot both come away
/// holding it — which is the entire point, and the reason this is not a bare "does the file exist".
///
/// **`_lossy` is chosen here, and skein chooses it in exactly two places (SKEIN-359)** — this and
/// the review budget's day ledger (`review::budget::reserve_a_read`), which argues it the same way
/// and cites this one. A
/// lease file that will not parse is taken as no lease and written over. Everywhere else that would
/// be destroying somebody's grants or credentials; here the file's entire content is one claim with
/// a deadline on it, held by a process that may not even be running, and there is nothing in it a
/// person would miss. Refusing instead would leave that one operation blocked for ever — never
/// claimable, never releasable — on a file nobody reads and nothing repairs. Erring toward the
/// work running (and, at worst, a second copy of an operation that is already guarded elsewhere) is
/// recoverable; erring toward an operation that can never run again is not.
///
/// `ttl` bounds how long a *dead* holder can block the work, not how long the work may take: it is
/// released as soon as `f` returns, on every path including an error. Choose it from how long the
/// operation plausibly runs, and err long — reclaiming early is starting the second copy this exists
/// to prevent.
pub fn attempt<T>(
    dir: &std::path::Path,
    name: &str,
    ttl: Duration,
    f: impl FnOnce() -> Result<T, String>,
) -> Result<Outcome<T>, String> {
    if !valid_name(name) {
        return Err(format!("{name:?} is not an operation name"));
    }
    let path = attempt_path(dir, name);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("mkdir {}: {e}", parent.display()))?;
    }
    let now = chrono::Utc::now();
    let mine = Attempt {
        holder: holder(),
        started_at: now.to_rfc3339(),
        deadline: (now + chrono::Duration::from_std(ttl).unwrap_or(chrono::Duration::hours(1)))
            .to_rfc3339(),
    };
    let claimed = update_json_lossy(&path, |held: &mut Attempt| {
        if held.live(now) {
            return Ok(Some(held.clone()));
        }
        *held = mine.clone();
        Ok(None)
    })?;
    if let Some(theirs) = claimed {
        return Ok(Outcome::InFlight(theirs));
    }

    let out = f();

    // Released whatever happened, and only if it is still ours: an attempt that ran past its
    // deadline has been taken over, and clearing it then would release somebody else's.
    let _ = update_json_lossy(&path, |held: &mut Attempt| {
        if held.holder == mine.holder && held.started_at == mine.started_at {
            *held = Attempt::default();
        }
        Ok(())
    });
    out.map(Outcome::Ran)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{env_lock, tempdir};

    /// Two callers, one run — and the second is told what the first is doing rather than being told
    /// the work is owed.
    #[test]
    fn a_second_caller_is_told_it_is_in_flight_rather_than_doing_it_again() {
        let _g = env_lock();
        let home = tempdir();
        let dir = home.join("attempts");

        let ran = std::sync::atomic::AtomicUsize::new(0);
        let outer = attempt(&dir, "create-fleet", Duration::from_secs(60), || {
            ran.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            // Re-entered from inside the work, which is the shape of two box starts overlapping.
            let inner = attempt(&dir, "create-fleet", Duration::from_secs(60), || {
                ran.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Ok(())
            })
            .unwrap();
            assert!(
                matches!(inner, Outcome::InFlight(_)),
                "the second caller started the work a second time"
            );
            Ok("done")
        })
        .unwrap();

        assert!(matches!(outer, Outcome::Ran("done")));
        assert_eq!(ran.load(std::sync::atomic::Ordering::SeqCst), 1);

        // And it is released, so the next caller is not blocked by a finished attempt.
        let again = attempt(&dir, "create-fleet", Duration::from_secs(60), || Ok(())).unwrap();
        assert!(
            matches!(again, Outcome::Ran(())),
            "a finished attempt still blocks"
        );
    }

    /// A holder that died does not block for ever — but only once its deadline has passed.
    ///
    /// Time is the only reclaim policy available. A process that dies leaves its record exactly as a
    /// process still working does, and nothing can tell them apart from the outside.
    #[test]
    fn an_attempt_past_its_deadline_is_taken_over_and_not_before() {
        let _g = env_lock();
        let home = tempdir();
        let dir = home.join("attempts");

        // A holder that will never come back, still inside its deadline.
        let path = attempt_path(&dir, "create-fleet");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let write = |deadline: chrono::DateTime<chrono::Utc>| {
            std::fs::write(
                &path,
                serde_json::to_vec(&Attempt {
                    holder: "999999@dead".into(),
                    started_at: (chrono::Utc::now() - chrono::Duration::minutes(5)).to_rfc3339(),
                    deadline: deadline.to_rfc3339(),
                })
                .unwrap(),
            )
            .unwrap();
        };

        write(chrono::Utc::now() + chrono::Duration::minutes(10));
        let held = attempt(&dir, "create-fleet", Duration::from_secs(60), || Ok(())).unwrap();
        match held {
            Outcome::InFlight(theirs) => assert_eq!(theirs.holder, "999999@dead"),
            Outcome::Ran(()) => panic!("a live attempt was taken over"),
        }

        write(chrono::Utc::now() - chrono::Duration::seconds(1));
        let taken = attempt(&dir, "create-fleet", Duration::from_secs(60), || Ok(())).unwrap();
        assert!(
            matches!(taken, Outcome::Ran(())),
            "a holder that ran out of time blocked the work for ever"
        );
    }

    /// The work runs once even when it fails, and the attempt is released either way.
    #[test]
    fn a_failed_attempt_is_released_rather_than_left_holding() {
        let _g = env_lock();
        let home = tempdir();
        let dir = home.join("attempts");

        let failed: Result<Outcome<()>, String> =
            attempt(&dir, "create-fleet", Duration::from_secs(60), || {
                Err("sbx said no".to_string())
            });
        assert_eq!(failed.unwrap_err(), "sbx said no");
        let next = attempt(&dir, "create-fleet", Duration::from_secs(60), || Ok(())).unwrap();
        assert!(
            matches!(next, Outcome::Ran(())),
            "a failed attempt held the operation shut"
        );
    }

    /// **A lease file that will not parse is taken over, not honoured for ever.**
    ///
    /// One of the two places in skein that ask for [`crate::util::update_json_lossy`] — the other
    /// is the review budget's day ledger — and the argument for it, asserted rather than left in a
    /// comment (SKEIN-359). Everywhere else an unreadable
    /// file is refused, because what it holds is somebody's grants or credentials. Here it holds a
    /// single claim with a deadline, belonging to a process that may be long dead, and refusing
    /// would mean this operation could never run and never be released again — a permanent outage
    /// caused by a file no person ever reads.
    #[test]
    fn a_lease_file_that_will_not_parse_is_taken_over_rather_than_blocking_for_ever() {
        let home = tempdir();
        let dir = home.join("attempts");
        std::fs::create_dir_all(&dir).unwrap();
        // The crash artifact: present, zero-length, unparseable.
        std::fs::write(attempt_path(&dir, "sweep"), b"").unwrap();

        let out = attempt(&dir, "sweep", Duration::from_secs(60), || Ok(7)).unwrap();
        assert!(
            matches!(out, Outcome::Ran(7)),
            "an unreadable lease blocked the work it was supposed to guard"
        );
        // And it is released afterwards, so the next caller runs too rather than inheriting the
        // claim this one had to invent.
        let again = attempt(&dir, "sweep", Duration::from_secs(60), || Ok(8)).unwrap();
        assert!(matches!(again, Outcome::Ran(8)));
    }
}
