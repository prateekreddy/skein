//! At-most-once execution, keyed by operation id — the warden's own durable state (§8.2).
//!
//! **The question this answers is "did it happen or not?"** A timeout on `destroy-fleet` means
//! exactly that, and an earlier draft of the design congratulated itself on deleting the
//! transport-failure-versus-command-failure distinction. That distinction is a **safety property**,
//! not redundancy: moving to HTTP relocates the hazard rather than removing it. So a request carries
//! an operation id, the warden answers a repeated id with the original outcome, and the outcome
//! store is durable state on the host — a thing to back up, not an implementation detail.
//!
//! **Not [`skein::attempt`]**, which is a different mechanism for a different question, and the
//! shapes are close enough to be worth separating out loud. `attempt` is mutual exclusion keyed by
//! operation *name*: is anybody doing this **right now**, and it forgets everything when the work
//! finishes. This is keyed by operation *id* and its whole job is to **remember** — for a retention
//! window, and then to remember that it has forgotten.
//!
//! **The id is never pruned; only the answer is.** That is the difference between "unknown" and
//! "never seen", and getting it wrong re-runs a destroy. A store that deleted expired records could
//! not tell a returning caller apart from a new one, so an id past the window would execute a second
//! time — the exact outcome the window exists inside. So an expired record keeps its id and drops
//! its payload, and the answer becomes [`Outcome::Unknown`], which is a real answer a caller can act
//! on. The cost is one small file per lifecycle operation ever performed, which for the operations
//! this warden owns is a handful a day.
//!
//! **The outcome is recorded before the caller is answered.** An outcome that exists only in a reply
//! that was never delivered is precisely the failure being prevented, so [`Store::once`] does not
//! return until the record is on disk and fsynced.
//!
//! **Anything it cannot read, it refuses.** A record that will not parse is answered with an error
//! and the work is never run. "Fail closed" here means "do not execute", which is the safe direction
//! for both doers: a create that does not happen is visible immediately, and a destroy that happens
//! twice is not recoverable.
//!
//! What this does **not** claim: it does not stop a caller minting a fresh id for work that already
//! ran. Nothing at this layer can — a new id is indistinguishable from new work. That is what the
//! approval surface is for (§8.1): every execution needs a human at the host, so a duplicate
//! execution needs a duplicate approval. This layer's job is that a **retry** never becomes a second
//! execution, and it does that exactly.

use serde::{Deserialize, Serialize};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// What a doer returned: its reply, or why it failed. Both are recorded — a failed operation that is
/// retried under the same id must be told it already failed, not run again in the hope of better.
pub type Done = Result<String, String>;

/// What [`Store::once`] decided.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// This id had not been seen. The work ran, and this is what it returned.
    Ran(Done),
    /// This id was answered before. This is what it said then, and the work did not run again.
    Replayed(Done),
    /// The id was accepted and no outcome was ever recorded — something died between the two.
    ///
    /// **In flight, and we do not know.** Deliberately not re-run: for a destroy, "we do not know"
    /// and "it did not happen" are different, and only one of them is safe to act on.
    Undecided {
        /// RFC3339, so a caller can say how long it has been that way.
        started_at: String,
    },
    /// The id is older than the retention window. Remembered as having existed; its answer is gone.
    ///
    /// Never re-executed. A caller that gets this has to decide with a human, which is the honest
    /// end of a request nobody kept the answer to.
    Unknown,
}

/// One operation, as the store remembers it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct Record {
    /// RFC3339, set when the id was first accepted.
    started_at: String,
    /// RFC3339, set when the outcome was recorded. Empty while in flight.
    #[serde(default)]
    finished_at: String,
    /// What the doer returned. `None` while in flight, and `None` again once the window has passed —
    /// which is why `finished_at` is what distinguishes those two and not this field.
    #[serde(default)]
    outcome: Option<Done>,
    /// Set when the answer has been dropped for age. The id stays; this says why there is nothing
    /// behind it.
    #[serde(default)]
    forgotten: bool,
}

/// The outcome store: a directory of records, one file per operation id.
///
/// One file per id rather than one file for all of them, for two reasons that are both about
/// failure: a claim is then an atomic filesystem operation with no lock protocol to get wrong, and
/// damage to one record cannot make the store unreadable for every other operation.
pub struct Store {
    dir: PathBuf,
    retention: Duration,
}

impl Store {
    /// `retention` is how long an answer is kept. Past it the id remains and the answer is
    /// [`Outcome::Unknown`] — see the note at the top of this module for why the id remains.
    pub fn new(dir: impl Into<PathBuf>, retention: Duration) -> Store {
        Store {
            dir: dir.into(),
            retention,
        }
    }

    fn path(&self, id: &str) -> PathBuf {
        self.dir.join(format!("{id}.json"))
    }

    /// Run `f` for `id`, at most once, ever.
    ///
    /// The ordering is the contract, and every step of it is a failure somebody has had:
    ///
    /// 1. **Claim the id atomically** — write the record whole, then `link` it into place, which
    ///    fails if the id is already there. A `create_new` followed by a write would leave an empty
    ///    file if the process died in between, and an empty file is a record nothing can read.
    /// 2. **Run the work.** Not under any lock: these operations take minutes, and a lock held
    ///    across one is a lock a crash leaves behind.
    /// 3. **Record the outcome, and fsync it**, before returning. See the module note.
    pub fn once(&self, id: &str, f: impl FnOnce() -> Done) -> Result<Outcome, String> {
        let id = checked_id(id)?;
        fs::create_dir_all(&self.dir).map_err(|e| format!("mkdir {}: {e}", self.dir.display()))?;
        // Ageing runs here rather than on a timer: these operations are rare and minutes long, so a
        // directory walk per request is nothing, and a store that only ages while something is
        // scheduled ages not at all on a host that was asleep.
        self.forget_what_is_old()?;

        let path = self.path(id);
        let mine = Record {
            started_at: chrono::Utc::now().to_rfc3339(),
            finished_at: String::new(),
            outcome: None,
            forgotten: false,
        };
        match self.claim(&path, &mine) {
            Claim::Ours => {}
            Claim::Taken => return self.read(&path).map(answer_from),
            Claim::Failed(why) => return Err(why),
        }

        let done = f();
        let finished = Record {
            finished_at: chrono::Utc::now().to_rfc3339(),
            outcome: Some(done.clone()),
            ..mine
        };
        // If this write fails the work has already happened, so the caller must not be told it
        // ran — it would retry, and the store no longer knows better. An error here is the store
        // saying "it may have happened and I cannot prove it", which is `Undecided` by another name.
        self.put(&path, &finished).map_err(|e| {
            format!("{id} ran, and recording that failed — treat it as undecided: {e}")
        })?;
        Ok(Outcome::Ran(done))
    }

    /// What the store already knows about `id`, without running anything.
    ///
    /// The read half of at-most-once: a caller whose reply was lost asks this rather than retrying
    /// blind. `None` means the id has never been seen, which is the one answer that licenses a
    /// first execution.
    pub fn asked(&self, id: &str) -> Result<Option<Outcome>, String> {
        let id = checked_id(id)?;
        let path = self.path(id);
        if !path.exists() {
            return Ok(None);
        }
        self.read(&path).map(|r| Some(answer_from(r)))
    }

    /// Take the id if nobody has it. The `link` is what makes this atomic: two processes arriving
    /// together cannot both come away with it, and the record they arrive at is always complete.
    fn claim(&self, path: &Path, record: &Record) -> Claim {
        let staging = staging(path, "claim");
        if let Err(e) = write_durable(&staging, record) {
            return Claim::Failed(e);
        }
        let linked = fs::hard_link(&staging, path);
        let _ = fs::remove_file(&staging);
        match linked {
            Ok(()) => {
                // The directory entry itself has to be durable, or a crash loses the claim and the
                // id becomes runnable again — which is the whole hazard.
                let _ = sync_dir(path);
                Claim::Ours
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Claim::Taken,
            Err(e) => Claim::Failed(format!("claim {}: {e}", path.display())),
        }
    }

    fn read(&self, path: &Path) -> Result<Record, String> {
        let raw = fs::read(path).map_err(|e| format!("read {}: {e}", path.display()))?;
        // Refused rather than treated as absent. An unreadable record is the one case where "never
        // seen" and "already ran" look identical, and guessing between them re-runs a destroy.
        serde_json::from_slice(&raw).map_err(|e| {
            format!(
                "{} is unreadable ({e}) — refusing to run, because an operation whose record cannot \
                 be read may already have happened",
                path.display()
            )
        })
    }

    fn put(&self, path: &Path, record: &Record) -> Result<(), String> {
        let staging = staging(path, "put");
        write_durable(&staging, record)?;
        fs::rename(&staging, path).map_err(|e| format!("record {}: {e}", path.display()))?;
        let _ = sync_dir(path);
        Ok(())
    }

    /// Drop the answers that are older than the window, keeping the ids.
    ///
    /// Only *finished* records age. One still in flight has no answer to drop, and forgetting it
    /// would turn "we do not know" into "we never heard of it" — which licenses a re-run.
    fn forget_what_is_old(&self) -> Result<(), String> {
        let Ok(entries) = fs::read_dir(&self.dir) else {
            return Ok(());
        };
        let cutoff = chrono::Utc::now()
            - chrono::Duration::from_std(self.retention).unwrap_or(chrono::Duration::days(30));
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let Ok(record) = self.read(&path) else {
                // A record that will not parse is left exactly as it is: `read` refuses it when it
                // is asked for, and rewriting it here would destroy the evidence of whatever went
                // wrong. Ageing is housekeeping and must not be a repair.
                continue;
            };
            if record.forgotten || record.finished_at.is_empty() {
                continue;
            }
            let stale = chrono::DateTime::parse_from_rfc3339(&record.finished_at)
                .map(|at| at.with_timezone(&chrono::Utc) < cutoff)
                .unwrap_or(false);
            if stale {
                self.put(
                    &path,
                    &Record {
                        outcome: None,
                        forgotten: true,
                        ..record
                    },
                )?;
            }
        }
        Ok(())
    }
}

enum Claim {
    Ours,
    Taken,
    Failed(String),
}

fn answer_from(record: Record) -> Outcome {
    match record {
        Record {
            forgotten: true, ..
        } => Outcome::Unknown,
        Record {
            outcome: Some(done),
            ..
        } => Outcome::Replayed(done),
        Record { started_at, .. } => Outcome::Undecided { started_at },
    }
}

/// An operation id, or why it is not one.
///
/// The id names a file, so this is a path guard before it is anything else — but it is also the
/// string the approval text is built around (§8.4), and one carrying control characters or a
/// newline is one that can make an approval say something other than what will run.
fn checked_id(id: &str) -> Result<&str, String> {
    let ok = !id.is_empty()
        && id.len() <= 128
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    match ok {
        true => Ok(id),
        false => Err(format!(
            "{id:?} is not an operation id — letters, digits, `-` and `_`, up to 128 of them"
        )),
    }
}

/// A scratch path nothing else is using.
///
/// The pid alone is not enough and the first version used it: two threads in one process then share
/// a staging path, and one deletes the file the other is about to `link`. The concurrency test found
/// it as `ENOENT` from `link`, which is a confusing way to be told that a claim raced — and had the
/// timing been slightly different it would have been two claims on one id instead.
fn staging(path: &Path, what: &str) -> PathBuf {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    path.with_extension(format!("{what}.{}.{n}", std::process::id()))
}

/// Write `record` to `path` and make sure it is actually on the disk.
///
/// `fsync` on the file, because the whole value of this store is that it survives the crash that
/// made the caller retry. A record in the page cache when the machine lost power is a record that
/// says the operation never happened.
fn write_durable(path: &Path, record: &Record) -> Result<(), String> {
    let bytes = serde_json::to_vec_pretty(record).map_err(|e| e.to_string())?;
    let mut file = fs::File::create(path).map_err(|e| format!("create {}: {e}", path.display()))?;
    file.write_all(&bytes)
        .map_err(|e| format!("write {}: {e}", path.display()))?;
    file.sync_all()
        .map_err(|e| format!("fsync {}: {e}", path.display()))?;
    Ok(())
}

/// And the directory, because a durable file nobody can find is not durable.
fn sync_dir(path: &Path) -> std::io::Result<()> {
    let dir = path.parent().unwrap_or(Path::new("."));
    fs::File::open(dir)?.sync_all()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// No `tempfile` dependency: this crate's dependency list is part of its argument (see the
    /// crate note), so a directory it makes itself is cheaper than a reason to add one.
    fn scratch(what: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "skein-warden-{what}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn forever() -> Duration {
        Duration::from_secs(3600)
    }

    /// The whole point: a retry is answered, not obeyed.
    ///
    /// Counted rather than inspected. Asserting the store *contains* one record would pass against
    /// an implementation that ran the work twice and recorded it once, which is the failure that
    /// matters — a fleet destroyed twice, or created twice.
    #[test]
    fn a_retry_is_answered_rather_than_run_again() {
        let dir = scratch("retry");
        let store = Store::new(&dir, forever());
        let ran = AtomicUsize::new(0);
        let work = || {
            ran.fetch_add(1, Ordering::SeqCst);
            Ok("fleet destroyed".to_string())
        };

        let first = store.once("op-1", work).unwrap();
        assert_eq!(first, Outcome::Ran(Ok("fleet destroyed".into())));

        // The reply was lost and the caller asks again with the SAME id, which is the contract the
        // client is required to keep (§8.2).
        let again = store.once("op-1", work).unwrap();
        assert_eq!(
            again,
            Outcome::Replayed(Ok("fleet destroyed".into())),
            "a repeated id must be told what happened, not made to happen again"
        );
        assert_eq!(ran.load(Ordering::SeqCst), 1, "the work ran twice");

        // And it can be asked without offering to run anything at all.
        assert_eq!(
            store.asked("op-1").unwrap(),
            Some(Outcome::Replayed(Ok("fleet destroyed".into())))
        );
        assert_eq!(
            store.asked("op-2").unwrap(),
            None,
            "an id nobody has seen is the one answer that licenses a first execution"
        );
        assert_eq!(ran.load(Ordering::SeqCst), 1);
    }

    /// A failure is an outcome. Retrying a failed id in the hope of a better answer is how one
    /// half-completed destroy becomes two.
    #[test]
    fn a_failure_is_replayed_as_a_failure() {
        let dir = scratch("failed");
        let store = Store::new(&dir, forever());
        let ran = AtomicUsize::new(0);
        let work = || {
            ran.fetch_add(1, Ordering::SeqCst);
            Err("sbx said no".to_string())
        };

        assert_eq!(
            store.once("op-fail", work).unwrap(),
            Outcome::Ran(Err("sbx said no".into()))
        );
        assert_eq!(
            store.once("op-fail", work).unwrap(),
            Outcome::Replayed(Err("sbx said no".into()))
        );
        assert_eq!(ran.load(Ordering::SeqCst), 1);
    }

    /// Past the window the answer is gone and the **id is not**, which is the difference between
    /// "unknown" and "never seen" — and the difference between refusing and destroying a fleet a
    /// second time.
    #[test]
    fn an_answer_past_the_window_is_unknown_and_still_does_not_run() {
        let dir = scratch("window");
        let ran = AtomicUsize::new(0);
        let work = || {
            ran.fetch_add(1, Ordering::SeqCst);
            Ok("done".to_string())
        };

        Store::new(&dir, forever()).once("op-old", work).unwrap();
        assert_eq!(ran.load(Ordering::SeqCst), 1);

        // The same store directory, read by a warden that keeps nothing. Ageing runs on the way in,
        // so this call both forgets the answer and then has to decide what to do without it.
        let forgetful = Store::new(&dir, Duration::ZERO);
        assert_eq!(
            forgetful.once("op-old", work).unwrap(),
            Outcome::Unknown,
            "an expired id must be answered, not executed"
        );
        assert_eq!(
            ran.load(Ordering::SeqCst),
            1,
            "an id whose answer aged out was executed a second time"
        );
        // And it stays unknown: forgetting is not a step towards being runnable again.
        assert_eq!(forgetful.once("op-old", work).unwrap(), Outcome::Unknown);
        assert_eq!(ran.load(Ordering::SeqCst), 1);
    }

    /// An id in flight when the process died. "We do not know" is the answer, and it is not the same
    /// as "it did not happen" — for a destroy those two license opposite actions.
    #[test]
    fn an_operation_that_was_never_finished_is_undecided() {
        let dir = scratch("undecided");
        let store = Store::new(&dir, forever());
        let ran = AtomicUsize::new(0);

        // A panic inside the work unwinds past the record step, which is what a crash between
        // running and recording looks like from the store's side.
        let crashed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            store.once("op-crash", || {
                ran.fetch_add(1, Ordering::SeqCst);
                panic!("the host went down mid-destroy");
            })
        }));
        assert!(crashed.is_err());
        assert_eq!(ran.load(Ordering::SeqCst), 1);

        match store.once("op-crash", || Ok("second run".to_string())) {
            Ok(Outcome::Undecided { started_at }) => assert!(
                !started_at.is_empty(),
                "a caller has to be able to say how long it has been undecided"
            ),
            other => panic!("an unfinished operation was re-run or mis-answered: {other:?}"),
        }
        assert_eq!(
            ran.load(Ordering::SeqCst),
            1,
            "an operation that may have happened was made to happen again"
        );
    }

    /// A record it cannot read is the one case where "never seen" and "already ran" look the same,
    /// so it refuses. Failing closed here means not executing, which is the safe direction for both
    /// doers: a create that does not happen is visible at once, a destroy that happens twice is not
    /// recoverable.
    #[test]
    fn a_record_it_cannot_read_refuses_rather_than_running() {
        let dir = scratch("corrupt");
        let store = Store::new(&dir, forever());
        let ran = AtomicUsize::new(0);
        let work = || {
            ran.fetch_add(1, Ordering::SeqCst);
            Ok("done".to_string())
        };

        store.once("op-torn", work).unwrap();
        // Truncated, which is what a write interrupted by power loss leaves behind.
        fs::write(dir.join("op-torn.json"), b"").unwrap();
        let why = store.once("op-torn", work).unwrap_err();
        assert!(
            why.contains("unreadable") && why.contains("may already have happened"),
            "the refusal must say why it is refusing: {why}"
        );
        assert_eq!(ran.load(Ordering::SeqCst), 1);

        // Garbage, rather than merely truncated. Same answer.
        fs::write(dir.join("op-torn.json"), b"{\"started_at\": ").unwrap();
        assert!(store.once("op-torn", work).is_err());
        assert!(store.asked("op-torn").is_err());
        assert_eq!(ran.load(Ordering::SeqCst), 1);
    }

    /// Two callers arriving together, which is what a client that retried on a slow reply looks
    /// like from here. The claim is a `link`, so exactly one of them can come away with it.
    #[test]
    fn two_callers_at_once_produce_one_execution() {
        let dir = scratch("race");
        let ran = AtomicUsize::new(0);
        std::thread::scope(|scope| {
            let ran = &ran;
            let dir = &dir;
            let hands: Vec<_> = (0..8)
                .map(|_| {
                    scope.spawn(move || {
                        Store::new(dir, forever()).once("op-race", || {
                            ran.fetch_add(1, Ordering::SeqCst);
                            // Long enough that the others are inside `once` while this one works.
                            std::thread::sleep(Duration::from_millis(50));
                            Ok("once".to_string())
                        })
                    })
                })
                .collect();
            for hand in hands {
                let got = hand.join().unwrap().unwrap();
                assert!(
                    matches!(
                        got,
                        Outcome::Ran(_) | Outcome::Replayed(_) | Outcome::Undecided { .. }
                    ),
                    "a concurrent caller got {got:?}"
                );
            }
        });
        assert_eq!(
            ran.load(Ordering::SeqCst),
            1,
            "eight callers, one id, and the work ran more than once"
        );
    }

    /// The id names a file and shapes the approval text, so it is checked before either.
    #[test]
    fn an_id_that_is_not_one_is_refused_before_anything_else() {
        let dir = scratch("ids");
        let store = Store::new(&dir, forever());
        let ran = AtomicUsize::new(0);
        let work = || {
            ran.fetch_add(1, Ordering::SeqCst);
            Ok(String::new())
        };
        for bad in [
            "../escape",
            "a/b",
            "",
            "op 1",
            "op\nApproved: yes",
            "op\u{0}",
        ] {
            let why = store.once(bad, work).unwrap_err();
            assert!(
                why.contains("is not an operation id"),
                "{bad:?} was accepted or refused for the wrong reason: {why}"
            );
            assert!(store.asked(bad).is_err());
        }
        assert_eq!(ran.load(Ordering::SeqCst), 0);
        for good in ["op-1", "create_fleet_2026", "A1"] {
            store.once(good, work).unwrap();
        }
        assert_eq!(ran.load(Ordering::SeqCst), 3);
    }
}
