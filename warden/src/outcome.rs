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
//! **Age never prunes an id; only the answer.** That is the difference between "unknown" and
//! "never seen", and getting it wrong re-runs a destroy. A store that deleted expired records could
//! not tell a returning caller apart from a new one, so an id past the window would execute a second
//! time — the exact outcome the window exists inside. So an expired record keeps its id and drops
//! its payload, and the answer becomes [`Outcome::Unknown`], which is a real answer a caller can act
//! on. The cost is one small file per lifecycle operation ever performed, which for the operations
//! this warden owns is a handful a day.
//!
//! **A record answers a repeat only while the host is still as the act left it** (SKEIN-1493). The
//! id is derived from the work, so it names *what is asked* and not *which time*: `destroy
//! skein-fleet` is one id for ever, and so is a create with the same arguments. Answering every
//! repeat from the record therefore told the second resize of a fleet "destroyed" with nobody
//! asked and nothing run, for the whole window — and "unknown" for ever after it. A retry and a
//! new request cannot be told apart by the id, and they do not need to be: what separates them is
//! whether the thing the record says was done is still done. A destroy is on record and the
//! sandbox is gone — that is a retry, and it is answered. A destroy is on record and the sandbox is
//! standing — whatever the caller meant, the record is not about *this* sandbox, so it is retired
//! and the request is a first execution again, with a person asked like any other. [`Holds`] is the
//! question, and whoever calls [`Store::once`] is the one who can look.
//!
//! That is at-most-once still, said precisely: **one approval buys one execution.** A caller that
//! crashed after sending a destroy and retries after the fleet was recreated does not get the old
//! answer, and does not get a second destroy either — it gets the approval prompt, and the person
//! at the host decides whether the fleet standing there now is one they meant to lose.
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
use std::io::{Read, Write};
use std::os::unix::fs::MetadataExt;
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// How long after crossing into its command an unfinished operation may still be running.
///
/// Longer than the 900 s a create is budgeted, and the same 1,800 s skein waits for a reply. A
/// warden killed mid-`sbx` leaves the child running, so its death alone says nothing about the
/// host. Past this, with no warden holding the record, the host is worth asking (SKEIN-1497).
pub const DOER_DEADLINE: Duration = Duration::from_secs(1800);

/// What a doer returned: its reply, or why it failed. Both are recorded — a failed operation that is
/// retried under the same id must be told it already failed, not run again in the hope of better.
pub type Done = Result<String, String>;

/// What a doer did, and therefore whether there is anything to remember.
///
/// **At-most-once is a property of executions, and this is the line between one and none.** The
/// store's contract (§8.2) is that a *retry* never becomes a second execution. It says nothing about
/// a request that never reached `sbx` at all, and the difference is not academic: the first
/// implementation wrapped [`Store::once`] around the approval as well as the command, so a person
/// who mistyped the id at the terminal locked that operation out for the whole retention window.
///
/// The operation id is derived from the verb, the sandbox, the argv and the environment
/// (`skein::warden_client::operation_id_with_env`), so it is the same id on every attempt at the
/// same work. A create refused once by a typo could then not be re-asked until an argument changed
/// — and the same is true of a warden started under a supervisor: with no controlling terminal it
/// refuses every doer, and the refusals it records would still be answering the operator after they
/// restarted it at a terminal.
///
/// **And it never bought anything against the attacker it was justified by.** Replaying a refusal
/// was argued from approval fatigue (§8.5), but a compromised skein mints whatever id it likes — the
/// module note above says so in as many words — so the replay only ever refused the honest caller.
/// The flood is answered where §8.5 puts it, at [`crate::flooding`], which counts arrivals.
///
/// [`Never`](Did::Never) is therefore constructed only at points that lexically precede the command:
/// the warden's own parse of the argv, and the approver saying no. Everything from `sbx` onwards is
/// [`Ran`](Did::Ran), including a `sbx` that could not be started — that one is genuinely "we do not
/// know whether it did anything".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Did {
    /// The command was reached. This is what came back, and it is remembered.
    ///
    /// The [`Crossed`] is proof that the marker was on disk first — see it for why a doer cannot
    /// say this without having written one.
    Ran(Crossed, Done),
    /// Nothing ran, and nothing is remembered: the id is left free for another attempt.
    Never(String),
}

/// Proof that the marker was written before the command was reached (SKEIN-533).
///
/// **A token rather than a convention, because a convention is what this replaces.** The store has
/// to tell "died with the approval still on the screen" from "died with `sbx` running", and the only
/// difference between those two on disk is a record written in the instant between them. A doer that
/// forgot to write it would leave a record that reads as the first while being the second — and the
/// store would hand the work out again, which for a destroy is the failure this module exists to
/// prevent.
///
/// The field is private, so this cannot be built outside this module: the only way a doer obtains
/// one is [`Reach`], which writes the marker and hands it back. [`Did::Ran`] therefore cannot be
/// spelled without the marker having been written, and the compiler is what checks that rather than
/// a reviewer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Crossed(());

/// The doorway from "being approved" to "running", handed to a doer by [`Store::once`].
///
/// Calling it writes the marker and returns the [`Crossed`] that [`Did::Ran`] needs. It can fail —
/// it writes to the disk the record lives on — and a doer that cannot cross must not run the
/// command; [`cross_then`] is the shape that gets that right.
pub type Reach<'a> = &'a dyn Fn() -> Result<Crossed, String>;

/// Write the marker, then run the command — the one correct order, spelled once.
///
/// Every doer needs the same three lines and the order of them is the whole property, so they live
/// here rather than three times over. **A failed marker does not run the command**: nothing has
/// happened at that point, so the id is released and the caller told why, which is the safe half of
/// at-most-once ([`Did`]) rather than a silent refusal.
pub fn cross_then(reach: Reach<'_>, command: impl FnOnce() -> Done) -> Did {
    match reach() {
        Ok(crossed) => Did::Ran(crossed, command()),
        // Not `Ran`: `command` is below this line and was never called. Releasing the id is right
        // exactly because nothing ran — and running anyway would leave a claim that reads as
        // abandoned, which is how one destroy becomes two.
        Err(why) => Did::Never(format!(
            "the marker that says this operation reached its command could not be written, so it \
             was not run — it would have been indistinguishable from one to hand out again: {why}"
        )),
    }
}

/// A [`Crossed`] for a test that is not testing the marker.
///
/// A doer's own tests are about its parse and its approval text, and they need a [`Reach`] that
/// always succeeds — so that a doer which wrongly skipped its approver would still reach the fake
/// `sbx` those tests watch for, and be caught by that rather than by an unwritten marker.
///
/// Gated on the doers whose tests use it, so a reduced build's test binary (the minimal one, or
/// `publish` or `unpublish` alone) does not carry it unused.
#[cfg(all(test, any(feature = "create", feature = "destroy")))]
pub(crate) fn crossed_for_a_test() -> Result<Crossed, String> {
    Ok(Crossed(()))
}

/// Is the host still as this act leaves it? Asked by [`Store::once`] of its caller, and only when
/// it is holding a finished record for the id (SKEIN-1493).
///
/// The store keeps records and cannot see the host; its caller can, so the question travels as a
/// function. It is handed when the record finished, for the caller's own account of what it found.
///
/// * `Ok(true)` — the record still describes the host, so it is the answer.
/// * `Ok(false)` — it does not: the sandbox a destroy removed is standing, or the one a create made
///   is gone. The record is retired and the work is put to a person as a first execution.
/// * `Err` — nobody could look. Nothing runs and nothing is replayed, because both would be a
///   guess: the error goes to the caller.
///
/// **Not asked about an unfinished record until [`DOER_DEADLINE`] has passed.** One still in flight
/// may have a command running under it — a warden killed mid-`sbx` leaves the child behind — and the
/// host half way through a destroy reads exactly like a host nobody has touched. Past the deadline,
/// with no live warden holding it, `Ok(false)` retires the record so a person is asked again
/// (SKEIN-1497). `Ok(true)` and `Err` leave it [`Outcome::Undecided`].
pub type Holds<'a> = &'a dyn Fn(&str) -> Result<bool, String>;

/// What [`Store::once`] decided.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// This id had not been seen. The work ran, and this is what it returned.
    Ran(Done),
    /// This id was answered before, and the host is still as that left it ([`Holds`]). This is
    /// what it said then, and the work did not run again.
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
    /// Not re-executed while the host is still as the act would have left it ([`Holds`]): a caller
    /// that gets this has to decide with a human, which is the honest end of a request nobody kept
    /// the answer to. Once the host says otherwise the record is retired like any other, which is
    /// what stops `destroy skein-fleet` being unknown for ever after its first thirty days.
    Unknown,
    /// Nothing ran, so there is nothing to be at-most-once about — see [`Did::Never`]. The id was
    /// released and the same request may be put to a person again.
    Refused(String),
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
    /// How far the operation had got when this record was last written. See [`Stage`].
    #[serde(default)]
    stage: Stage,
    /// RFC3339, set when the command was reached. Empty on a record from before this field, whose
    /// deadline then runs from `started_at`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    crossed_at: String,
}

/// How far an operation had got — the difference between a claim worth taking back and one that is
/// never taken back (SKEIN-533).
///
/// **`forget_what_is_old` skips unfinished records on purpose, so an `Undecided` never ages out.**
/// That is right for a destroy whose `sbx` may have run, and it was wrong for everything else: a
/// warden killed while its approval sat on the screen left the id claimed for good, and because the
/// id is derived from the work (`skein::warden_client::operation_id_with_env`) rather than minted,
/// every later attempt at the same work was answered `409 undecided` — with no way round it but
/// deleting a file on the host.
///
/// So the record says which side of the command the warden died on.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum Stage {
    /// Written by a warden that did not say, which is every warden from before this field existed.
    ///
    /// **The default, and never released.** Such a record may have been mid-destroy when it died,
    /// and nothing in it distinguishes that from mid-approval — so it keeps the old behaviour, which
    /// is to stay undecided until a person looks. Being the `#[default]` is what makes that true of
    /// a record whose JSON has no `stage` at all.
    #[default]
    Unmarked,
    /// Claimed, and in front of a person. Nothing has run, and the claim is released if the warden
    /// holding it is gone — which is what [`abandoned`] decides.
    Asking,
    /// The command was reached. Never released by [`abandoned`]: "we do not know" is the honest
    /// answer until [`DOER_DEADLINE`], and after it only the host can retire the record.
    Running,
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

    /// Run `f` for `id`, at most once for as long as the host is as that run left it.
    ///
    /// The ordering is the contract, and every step of it is a failure somebody has had:
    ///
    /// 1. **Claim the id atomically** — write the record whole, then `link` it into place, which
    ///    fails if the id is already there. A `create_new` followed by a write would leave an empty
    ///    file if the process died in between, and an empty file is a record nothing can read.
    /// 2. **Run the work.** Not under any lock: these operations take minutes, and a lock held
    ///    across one is a lock a crash leaves behind.
    /// 3. **Record the outcome, and fsync it**, before returning. See the module note.
    ///
    /// A [`Did::Never`] undoes step 1 instead of reaching step 3: nothing ran, so the id is released
    /// rather than answered for the next thirty days. [`Did`] carries the argument for why that is
    /// the safe half of at-most-once and not a hole in it.
    ///
    /// `f` is handed a [`Reach`] and has to cross it to run anything, which is step 1½ and the
    /// subject of [`Stage`]: the record says which side of the command a death happened on, so a
    /// claim abandoned with the approval still on the screen can be taken back, and one abandoned
    /// with `sbx` already running never is.
    ///
    /// `holds` is consulted when step 1 finds a **finished** record in the way, and decides whether
    /// that record is the answer or is retired — see [`Holds`] and the module note.
    pub fn once(
        &self,
        id: &str,
        holds: Holds<'_>,
        f: impl FnOnce(Reach<'_>) -> Did,
    ) -> Result<Outcome, String> {
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
            stage: Stage::Asking,
            crossed_at: String::new(),
        };
        // Held open — and flocked — for as long as this operation is in front of a person. The
        // kernel drops it however this process dies, which is what lets the next warden tell an
        // abandoned claim from a live one without trusting a pid.
        let _held = match self.take(&path, &mine, holds)? {
            Taken::Ours(held) => held,
            Taken::Answered(outcome) => return Ok(outcome),
        };

        // The marker, and the only way to obtain a `Crossed`. Writing it before the command is what
        // makes a death during `sbx` distinguishable from a death before it.
        //
        // The rename below replaces the inode `_held` locks, so the new record is locked as well:
        // that lock is how a later request tells a live warden with a slow `sbx` from a dead one.
        let running = std::cell::RefCell::new(None);
        let reach = || -> Result<Crossed, String> {
            let held = self.put_held(
                &path,
                &Record {
                    stage: Stage::Running,
                    crossed_at: chrono::Utc::now().to_rfc3339(),
                    ..mine.clone()
                },
            )?;
            *running.borrow_mut() = Some(held);
            Ok(Crossed(()))
        };
        let done = match f(&reach) {
            Did::Ran(_crossed, done) => done,
            // The claim is given back, and a failure to give it back is reported rather than
            // swallowed: an id left claimed by a refusal is the very lock this branch exists to
            // remove, and it would otherwise reappear as an `Undecided` nobody could explain.
            Did::Never(why) => {
                return match fs::remove_file(&path) {
                    Ok(()) => {
                        let _ = sync_dir(&path);
                        Ok(Outcome::Refused(why))
                    }
                    Err(e) => Err(format!(
                        "{why} — and the warden could not release {} ({e}), so asking again will \
                         be answered with this rather than put to a person",
                        path.display()
                    )),
                }
            }
        };
        let finished = Record {
            finished_at: chrono::Utc::now().to_rfc3339(),
            outcome: Some(done.clone()),
            stage: Stage::Running,
            ..mine.clone()
        };
        // If this write fails the work has already happened, so the caller must not be told it
        // ran — it would retry, and the store no longer knows better. An error here is the store
        // saying "it may have happened and I cannot prove it", which is `Undecided` by another name.
        self.put(&path, &finished).map_err(|e| {
            format!("{id} ran, and recording that failed — treat it as undecided: {e}")
        })?;
        Ok(Outcome::Ran(done))
    }

    /// Claim the id, or say what it is already answered with — releasing a claim whose warden is
    /// gone, retiring a finished record the host no longer bears out, and looping because either
    /// one frees the id for anybody, not for us.
    ///
    /// Eight laps rather than forever: each lap needs another process to have claimed the id in the
    /// window between our release and our claim, and a caller that loses eight of those in a row is
    /// better told to ask again than spun on.
    fn take(&self, path: &Path, mine: &Record, holds: Holds<'_>) -> Result<Taken, String> {
        for _ in 0..8 {
            match self.claim(path, mine) {
                Claim::Ours(held) => return Ok(Taken::Ours(held)),
                Claim::Failed(why) => return Err(why),
                Claim::Taken => {}
            }
            let record = self.read(path)?;
            if abandoned(path, &record) {
                continue;
            }
            // In flight, and not abandoned: somebody is at the approval, or a command may be
            // running. The host is not asked until the deadline — see `Holds`. After it, a host
            // that shows the act did not take effect frees the id for a person to be asked. A host
            // that agrees, or cannot be read, leaves it undecided.
            if record.finished_at.is_empty() {
                if overdue(path, &record)
                    && matches!(
                        holds(&format!("was left running at {}", ran_from(&record))),
                        Ok(false)
                    )
                {
                    let _ = retired(path, &record);
                    continue;
                }
                return Ok(Taken::Answered(answer_from(record)));
            }
            // Finished — with an answer, or with one the window has dropped. It answers this
            // request only if the host is still as that run left it. An `Err` leaves by `?`:
            // nobody could look, so nothing is replayed and nothing is run.
            if holds(&format!("finished at {}", record.finished_at))? {
                return Ok(Taken::Answered(answer_from(record)));
            }
            // Not `return` on a refusal to retire: it means the record moved under us — another
            // request retired it first and may hold the id now — so the next lap reads what is
            // there and answers from that.
            let _ = retired(path, &record);
        }
        Err(format!(
            "{} was claimed and released repeatedly while this request waited — ask again",
            path.display()
        ))
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
    ///
    /// **The lock is taken before the link, on the descriptor the link is made from.** `link` gives
    /// the new name the same inode, so the lock a later warden tests is a lock on the very record it
    /// is reading — and there is no window in which the record exists unlocked, which is the window
    /// a lock taken after the link would leave.
    fn claim(&self, path: &Path, record: &Record) -> Claim {
        let staging = staging(path, "claim");
        let held = match write_into(&staging, record) {
            Ok(file) => file,
            Err(why) => return Claim::Failed(why),
        };
        let held = match lock(&held) {
            true => held,
            // No lock means nothing can ever tell this claim's owner from a dead one, so it is
            // recorded as the stage that is never released. An operation stuck undecided costs a
            // person their morning; one released while it is still running destroys a fleet twice.
            false => match write_into(
                &staging,
                &Record {
                    stage: Stage::Unmarked,
                    ..record.clone()
                },
            ) {
                Ok(file) => file,
                Err(why) => return Claim::Failed(why),
            },
        };
        let linked = fs::hard_link(&staging, path);
        let _ = fs::remove_file(&staging);
        match linked {
            Ok(()) => {
                // The directory entry itself has to be durable, or a crash loses the claim and the
                // id becomes runnable again — which is the whole hazard.
                let _ = sync_dir(path);
                Claim::Ours(held)
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

    /// Write `record` over `path` and hand back a lock on the file now at that name.
    ///
    /// A record that could not be locked is written as [`Stage::Unmarked`], the stage nothing ever
    /// judges: without the lock a live warden could not be told from a dead one.
    fn put_held(&self, path: &Path, record: &Record) -> Result<fs::File, String> {
        let staging = staging(path, "put");
        let mut held = write_into(&staging, record)?;
        if !lock(&held) {
            held = write_into(
                &staging,
                &Record {
                    stage: Stage::Unmarked,
                    ..record.clone()
                },
            )?;
        }
        fs::rename(&staging, path).map_err(|e| format!("record {}: {e}", path.display()))?;
        let _ = sync_dir(path);
        Ok(held)
    }

    /// Where the record for `id` lives, for the recovery line a person is given.
    pub fn record_path(&self, id: &str) -> Result<PathBuf, String> {
        checked_id(id).map(|id| self.path(id))
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
    /// Ours, with the open, locked descriptor the claim was made through. Dropping it — or dying —
    /// releases the lock, which is the whole signal.
    Ours(fs::File),
    Taken,
    Failed(String),
}

/// What [`Store::take`] came away with.
enum Taken {
    Ours(fs::File),
    Answered(Outcome),
}

/// Whether the claim on `path` belonged to a warden that is gone — and if it did, release it.
///
/// **Every "no" here is a refusal to release**, which is the safe direction: a claim left in place
/// is answered [`Outcome::Undecided`] and a person can look, where a claim released while its owner
/// is alive is the same operation approved and run twice.
///
/// Four things have to hold, and each one closes a way of being wrong:
///
/// 1. **Unfinished and [`Stage::Asking`]** — nothing has run. `Running` and `Unmarked` are never
///    released, whatever else is true of them.
/// 2. **The lock is free.** `flock` is held by the claiming process for as long as it lives, and the
///    kernel drops it on every way of dying — SIGKILL, a panic, the power going. A pid would not do:
///    pids are reused, and a live warden wearing a dead one's pid would be robbed of its claim.
/// 3. **The inode under the lock is still the one at `path`.** An owner crossing to `Running`
///    renames a new file over this name, so a lock on what was there a moment ago can be a lock on a
///    record nobody will ever read again.
/// 4. **The record read back through the locked descriptor still says `Asking`.** Steps 1 and 2 are
///    two reads with a gap between them; this one is of the exact bytes under the lock.
fn abandoned(path: &Path, record: &Record) -> bool {
    if !record.finished_at.is_empty() || record.stage != Stage::Asking {
        return false;
    }
    let Ok(mut file) = fs::File::open(path) else {
        return false;
    };
    if !lock(&file) {
        return false;
    }
    let (Ok(locked), Ok(named)) = (file.metadata(), fs::metadata(path)) else {
        return false;
    };
    if (locked.dev(), locked.ino()) != (named.dev(), named.ino()) {
        return false;
    }
    let mut raw = Vec::new();
    if file.read_to_end(&mut raw).is_err() {
        return false;
    }
    let Ok(fresh) = serde_json::from_slice::<Record>(&raw) else {
        return false;
    };
    if !fresh.finished_at.is_empty() || fresh.stage != Stage::Asking {
        return false;
    }
    // Nobody holds this and nothing ran under it. Removing it is what makes the id askable again —
    // and it is not the pruning the module note forbids, because that is about answers and this
    // record has none: it was claimed and abandoned before anything could happen.
    fs::remove_file(path).is_ok()
}

/// When the command under this record started, as near as the record can say.
fn ran_from(record: &Record) -> &str {
    match record.crossed_at.is_empty() {
        true => &record.started_at,
        false => &record.crossed_at,
    }
}

/// Whether this unfinished record is past [`DOER_DEADLINE`] with no live warden holding it.
///
/// Every "no" leaves the record undecided. [`Stage::Unmarked`] is never overdue, because it may be
/// a claim whose owner could not be detected at all, and a time that will not parse is not old.
fn overdue(path: &Path, record: &Record) -> bool {
    if record.stage != Stage::Running {
        return false;
    }
    let Ok(from) = chrono::DateTime::parse_from_rfc3339(ran_from(record)) else {
        return false;
    };
    let age = chrono::Utc::now().signed_duration_since(from);
    if age.to_std().map_or(true, |age| age < DOER_DEADLINE) {
        return false;
    }
    fs::File::open(path).is_ok_and(|file| lock(&file))
}

/// Remove the record at `path` — the exact one `judged` — so its id can be claimed again.
///
/// Called once [`Holds`] has said the host no longer bears the record out. **Every "no" here leaves
/// the record where it is**, as in [`abandoned`], and for the same reason: two requests can reach
/// this point together, and the second must not remove the claim the first has just made in the
/// old record's place — that would be one approval's worth of work handed out twice.
///
/// So the same three checks, about a different record: the lock is free (two retirers contend, and
/// a claim in front of a person is held by its warden); the inode under the lock is still the one
/// at `path`; and the bytes under the lock are still the record that was judged, whole — not merely
/// "finished", because a later run that finished in between is a different answer about a different
/// host, and it has not been judged at all.
///
/// Removal is not the pruning the module note forbids. That rule keeps an *unanswered* id from
/// reading as never seen; this record was answered, and the host itself now says its answer is
/// about something that is no longer there. What survives it is the audit log, which has the
/// `asked` and `settled` lines of the run this record was for.
fn retired(path: &Path, judged: &Record) -> bool {
    let Ok(mut file) = fs::File::open(path) else {
        return false;
    };
    if !lock(&file) {
        return false;
    }
    let (Ok(locked), Ok(named)) = (file.metadata(), fs::metadata(path)) else {
        return false;
    };
    if (locked.dev(), locked.ino()) != (named.dev(), named.ino()) {
        return false;
    }
    let mut raw = Vec::new();
    if file.read_to_end(&mut raw).is_err() {
        return false;
    }
    if serde_json::from_slice::<Record>(&raw).ok().as_ref() != Some(judged) {
        return false;
    }
    let gone = fs::remove_file(path).is_ok();
    let _ = sync_dir(path);
    gone
}

/// `flock(LOCK_EX|LOCK_NB)`: true if we now hold it, false if somebody else does.
///
/// Per open file description, so two threads of one process contend exactly as two processes do —
/// which `fcntl` locks would not, and this store is reached from a thread per request.
fn lock(file: &fs::File) -> bool {
    unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) == 0 }
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
///
/// Public so that [`crate::serve`] can apply the same grammar at the wire rather than a second one
/// beside it. It used to be reachable only from here, which meant an id was checked after the
/// audit entry naming it had already been written.
pub fn checked_id(id: &str) -> Result<&str, String> {
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
    write_into(path, record).map(|_| ())
}

/// The same write, handing back the open descriptor — which [`Store::claim`] locks and then holds
/// for the life of the operation.
fn write_into(path: &Path, record: &Record) -> Result<fs::File, String> {
    let bytes = serde_json::to_vec_pretty(record).map_err(|e| e.to_string())?;
    let mut file = fs::File::create(path).map_err(|e| format!("create {}: {e}", path.display()))?;
    file.write_all(&bytes)
        .map_err(|e| format!("write {}: {e}", path.display()))?;
    file.sync_all()
        .map_err(|e| format!("fsync {}: {e}", path.display()))?;
    Ok(file)
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
    fn scratch(what: &str) -> crate::Scratch {
        crate::Scratch::new(&format!("skein-warden-{what}"))
    }

    fn forever() -> Duration {
        Duration::from_secs(3600)
    }

    /// The host is as every record says, for a test whose subject is the record. What happens when
    /// it is not is `a_record_the_host_no_longer_bears_out_…`, below.
    fn believed(_: &str) -> Result<bool, String> {
        Ok(true)
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
        let work = |reach: Reach<'_>| {
            cross_then(reach, || {
                ran.fetch_add(1, Ordering::SeqCst);
                Ok("fleet destroyed".to_string())
            })
        };

        let first = store.once("op-1", &believed, work).unwrap();
        assert_eq!(first, Outcome::Ran(Ok("fleet destroyed".into())));

        // The reply was lost and the caller asks again with the SAME id, which is the contract the
        // client is required to keep (§8.2).
        let again = store.once("op-1", &believed, work).unwrap();
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
        let work = |reach: Reach<'_>| {
            cross_then(reach, || {
                ran.fetch_add(1, Ordering::SeqCst);
                Err("sbx said no".to_string())
            })
        };

        assert_eq!(
            store.once("op-fail", &believed, work).unwrap(),
            Outcome::Ran(Err("sbx said no".into()))
        );
        assert_eq!(
            store.once("op-fail", &believed, work).unwrap(),
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
        let work = |reach: Reach<'_>| {
            cross_then(reach, || {
                ran.fetch_add(1, Ordering::SeqCst);
                Ok("done".to_string())
            })
        };

        Store::new(&dir, forever())
            .once("op-old", &believed, work)
            .unwrap();
        assert_eq!(ran.load(Ordering::SeqCst), 1);

        // The same store directory, read by a warden that keeps nothing. Ageing runs on the way in,
        // so this call both forgets the answer and then has to decide what to do without it.
        let forgetful = Store::new(&dir, Duration::ZERO);
        assert_eq!(
            forgetful.once("op-old", &believed, work).unwrap(),
            Outcome::Unknown,
            "an expired id must be answered, not executed"
        );
        assert_eq!(
            ran.load(Ordering::SeqCst),
            1,
            "an id whose answer aged out was executed a second time"
        );
        // And it stays unknown: forgetting is not a step towards being runnable again.
        assert_eq!(
            forgetful.once("op-old", &believed, work).unwrap(),
            Outcome::Unknown
        );
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
            store.once("op-crash", &believed, |reach| {
                // Crossed first, so this is a death with `sbx` already running — the case that has
                // to stay undecided. The assertion below is that it does.
                let _crossed = reach().expect("the marker must be writable");
                ran.fetch_add(1, Ordering::SeqCst);
                panic!("the host went down mid-destroy");
            })
        }));
        assert!(crashed.is_err());
        assert_eq!(ran.load(Ordering::SeqCst), 1);

        match store.once("op-crash", &believed, |reach| {
            cross_then(reach, || Ok("second run".to_string()))
        }) {
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
        let work = |reach: Reach<'_>| {
            cross_then(reach, || {
                ran.fetch_add(1, Ordering::SeqCst);
                Ok("done".to_string())
            })
        };

        store.once("op-torn", &believed, work).unwrap();
        // Truncated, which is what a write interrupted by power loss leaves behind.
        fs::write(dir.join("op-torn.json"), b"").unwrap();
        let why = store.once("op-torn", &believed, work).unwrap_err();
        assert!(
            why.contains("unreadable") && why.contains("may already have happened"),
            "the refusal must say why it is refusing: {why}"
        );
        assert_eq!(ran.load(Ordering::SeqCst), 1);

        // Garbage, rather than merely truncated. Same answer.
        fs::write(dir.join("op-torn.json"), b"{\"started_at\": ").unwrap();
        assert!(store.once("op-torn", &believed, work).is_err());
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
                        Store::new(dir, forever()).once("op-race", &believed, |reach| {
                            cross_then(reach, || {
                                ran.fetch_add(1, Ordering::SeqCst);
                                // Long enough that the others are inside `once` while this one
                                // works.
                                std::thread::sleep(Duration::from_millis(50));
                                Ok("once".to_string())
                            })
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
        let work = |reach: Reach<'_>| {
            cross_then(reach, || {
                ran.fetch_add(1, Ordering::SeqCst);
                Ok(String::new())
            })
        };
        for bad in [
            "../escape",
            "a/b",
            "",
            "op 1",
            "op\nApproved: yes",
            "op\u{0}",
        ] {
            let why = store.once(bad, &believed, work).unwrap_err();
            assert!(
                why.contains("is not an operation id"),
                "{bad:?} was accepted or refused for the wrong reason: {why}"
            );
            assert!(store.asked(bad).is_err());
        }
        assert_eq!(ran.load(Ordering::SeqCst), 0);
        for good in ["op-1", "create_fleet_2026", "A1"] {
            store.once(good, &believed, work).unwrap();
        }
        assert_eq!(ran.load(Ordering::SeqCst), 3);
    }

    /// A warden that died with the approval still on the screen does not hold the id for ever
    /// (SKEIN-533).
    ///
    /// **This is the bug in the shape it reaches a person.** The id is derived from the work rather
    /// than minted (`skein::warden_client::operation_id_with_env`), so "ask again" is the *same*
    /// id: a claim nothing releases is that piece of work refused for ever, and the only way out was
    /// deleting a file on the host. `forget_what_is_old` cannot help — it skips unfinished records
    /// on purpose, because ageing out a destroy that may have run would license a re-run.
    ///
    /// What a dead warden leaves is exactly this: an `Asking` record with nobody holding its lock.
    /// The counterfactual is [`abandoned`] returning false — then this is `Undecided` and `ran` is 0.
    #[test]
    fn an_operation_abandoned_before_its_command_can_be_asked_again() {
        let dir = scratch("abandoned");
        let store = Store::new(&dir, forever());
        let ran = AtomicUsize::new(0);

        // Claimed, put in front of a person, and then the process went away — so no lock is held.
        fs::write(
            dir.join("op-gone.json"),
            br#"{"started_at":"2026-09-15T00:00:00Z","finished_at":"","outcome":null,
                 "forgotten":false,"stage":"asking"}"#,
        )
        .unwrap();

        let got = store
            .once("op-gone", &believed, |reach| {
                cross_then(reach, || {
                    ran.fetch_add(1, Ordering::SeqCst);
                    Ok("made".to_string())
                })
            })
            .unwrap();
        assert_eq!(
            got,
            Outcome::Ran(Ok("made".into())),
            "a claim whose warden is gone must be askable again, not undecided for ever"
        );
        assert_eq!(ran.load(Ordering::SeqCst), 1);
    }

    /// And a claim that is still in front of a person is not taken from them.
    ///
    /// The other half, and the one that makes the first safe: [`abandoned`] asks the kernel rather
    /// than a clock, so a warden that is merely *slow* — an approval nobody has answered yet — keeps
    /// its id. The counterfactual is dropping the lock check: the second caller then releases a live
    /// claim and runs, and `ran` is 2 for one operation.
    ///
    /// Two threads of one process, which is the harder case: `flock` is per open file description,
    /// so this contends exactly as two wardens would, where an `fcntl` lock would quietly succeed.
    #[test]
    fn a_claim_still_in_front_of_a_person_is_not_taken_from_them() {
        let dir = scratch("in-front-of-a-person");
        let ran = AtomicUsize::new(0);
        let (approve, approved) = std::sync::mpsc::channel::<()>();
        let (claimed, waiting) = std::sync::mpsc::channel::<()>();

        std::thread::scope(|scope| {
            let ran = &ran;
            let dir = &dir;
            let holder = scope.spawn(move || {
                Store::new(dir, forever()).once("op-slow", &believed, |reach| {
                    // Claimed, and now sitting at the approval. The marker is deliberately not
                    // crossed yet, so the record on disk says `Asking` — the releasable stage.
                    claimed.send(()).unwrap();
                    // Not `recv()`. If the assertions below fail, `thread::scope` joins this thread
                    // before it propagates the panic — and a holder waiting for a signal the
                    // panicking thread will now never send deadlocks the suite instead of failing
                    // it. The first sabotage run of this test hung here for fifteen minutes, which
                    // is the same bug as a red that never arrives.
                    let _ = approved.recv_timeout(Duration::from_secs(20));
                    cross_then(reach, || {
                        ran.fetch_add(1, Ordering::SeqCst);
                        Ok("approved at last".to_string())
                    })
                })
            });
            waiting.recv().unwrap();

            let got = Store::new(dir, forever())
                .once("op-slow", &believed, |reach| {
                    cross_then(reach, || {
                        ran.fetch_add(1, Ordering::SeqCst);
                        Ok("stolen".to_string())
                    })
                })
                .unwrap();
            assert!(
                matches!(got, Outcome::Undecided { .. }),
                "a claim still being approved was taken from the warden holding it: {got:?}"
            );
            assert_eq!(
                ran.load(Ordering::SeqCst),
                0,
                "the work ran while the first warden was still waiting for an approval"
            );

            approve.send(()).unwrap();
            assert_eq!(
                holder.join().unwrap().unwrap(),
                Outcome::Ran(Ok("approved at last".into()))
            );
            assert_eq!(ran.load(Ordering::SeqCst), 1);
        });
    }

    /// A record from before the marker existed is never given back, whatever else is true of it.
    ///
    /// **The migration hazard, and the reason [`Stage::Unmarked`] is the `#[default]`.** A warden
    /// from before this field died mid-destroy and left an unfinished record with no `stage` in its
    /// JSON at all. Nothing in that record says which side of `sbx` it died on — so releasing it
    /// would be a coin toss on whether a fleet is destroyed twice, and it stays undecided.
    ///
    /// The counterfactual is `#[default]` moving to `Asking`, or [`abandoned`] dropping its stage
    /// check: either one makes this run the work, and `ran` is 1.
    #[test]
    fn a_record_from_before_the_marker_is_never_given_back() {
        let dir = scratch("legacy");
        let store = Store::new(&dir, forever());
        let ran = AtomicUsize::new(0);

        // Exactly what the previous version of this file wrote: no `stage` key at all.
        fs::write(
            dir.join("op-legacy.json"),
            br#"{"started_at":"2026-09-01T00:00:00Z","finished_at":"","outcome":null,
                 "forgotten":false}"#,
        )
        .unwrap();

        let got = store
            .once("op-legacy", &believed, |reach| {
                cross_then(reach, || {
                    ran.fetch_add(1, Ordering::SeqCst);
                    Ok("destroyed a second time".to_string())
                })
            })
            .unwrap();
        assert!(
            matches!(got, Outcome::Undecided { .. }),
            "a record from before the marker was released, and it may have been mid-destroy: \
             {got:?}"
        );
        assert_eq!(
            ran.load(Ordering::SeqCst),
            0,
            "an operation that may already have destroyed a fleet was run again"
        );
    }
    /// **A record the host no longer bears out is retired, and the work is a first execution
    /// again** (SKEIN-1493).
    ///
    /// The id is derived from the work, so the second destroy of a fleet carries the id of the
    /// first. Answered from the record it was told "destroyed" with nothing run; so the store asks
    /// its caller whether the host is still as the record says, and a "no" makes the id runnable.
    /// All three kinds of finished record, because each one locked the id in its own way: a success
    /// replayed as done, a failure replayed for thirty days, and an answer past the window that was
    /// `Unknown` for ever.
    ///
    /// Counted, as everywhere here: the assertion is that the work RAN a second time and the reply
    /// said `Ran`. Its counterfactual is `take` answering from a finished record without reading
    /// what `holds` said — then every second call below is `Replayed` or `Unknown` and each count
    /// stays at 1.
    #[test]
    fn a_record_the_host_no_longer_bears_out_is_retired_and_the_work_runs_again() {
        let dir = scratch("superseded");
        let ran = AtomicUsize::new(0);
        let asked = AtomicUsize::new(0);
        let moved_on = |_: &str| -> Result<bool, String> {
            asked.fetch_add(1, Ordering::SeqCst);
            Ok(false)
        };
        let well = |reach: Reach<'_>| {
            cross_then(reach, || {
                ran.fetch_add(1, Ordering::SeqCst);
                Ok("fleet destroyed".to_string())
            })
        };
        let badly = |reach: Reach<'_>| {
            cross_then(reach, || {
                ran.fetch_add(1, Ordering::SeqCst);
                Err("sbx said no".to_string())
            })
        };

        // A first execution never consults the host: there is no record to doubt.
        let store = Store::new(&dir, forever());
        assert_eq!(
            store.once("op-ok", &moved_on, well).unwrap(),
            Outcome::Ran(Ok("fleet destroyed".into()))
        );
        assert_eq!(asked.load(Ordering::SeqCst), 0);

        // The fleet is back, and it is destroyed again under the same id.
        assert_eq!(
            store.once("op-ok", &moved_on, well).unwrap(),
            Outcome::Ran(Ok("fleet destroyed".into())),
            "a destroy whose sandbox is standing again was answered from the record of the last one"
        );
        assert_eq!(
            ran.load(Ordering::SeqCst),
            2,
            "the second destroy never ran"
        );
        assert_eq!(asked.load(Ordering::SeqCst), 1);
        // And the retry of THAT one — the host now as it left it — is answered, not run.
        assert_eq!(
            store.once("op-ok", &believed, well).unwrap(),
            Outcome::Replayed(Ok("fleet destroyed".into()))
        );
        assert_eq!(ran.load(Ordering::SeqCst), 2, "a retry ran the work again");

        // A failure on record, and the thing it failed to do still not done.
        ran.store(0, Ordering::SeqCst);
        store.once("op-failed", &believed, badly).unwrap();
        assert_eq!(
            store.once("op-failed", &moved_on, well).unwrap(),
            Outcome::Ran(Ok("fleet destroyed".into())),
            "a failed run was replayed although the host shows the work is still to do"
        );
        assert_eq!(ran.load(Ordering::SeqCst), 2);

        // An answer the window dropped. Ageing runs on the way in, so the first call here both
        // forgets the answer and has to decide without it.
        ran.store(0, Ordering::SeqCst);
        store.once("op-old", &believed, well).unwrap();
        let forgetful = Store::new(&dir, Duration::ZERO);
        assert_eq!(
            forgetful.once("op-old", &believed, well).unwrap(),
            Outcome::Unknown
        );
        assert_eq!(ran.load(Ordering::SeqCst), 1);
        assert_eq!(
            forgetful.once("op-old", &moved_on, well).unwrap(),
            Outcome::Ran(Ok("fleet destroyed".into())),
            "an id past the window stayed unknown although the host shows the work is to do"
        );
        assert_eq!(ran.load(Ordering::SeqCst), 2);
    }

    /// **The host is not asked about an operation still in flight, and its answer could not free
    /// one** (SKEIN-1493).
    ///
    /// A warden killed inside `sbx rm` leaves the child running. For those seconds the sandbox is
    /// still listed, so "the host does not bear this record out" is true of a destroy that is
    /// happening right now — and retiring the record on it would put the same destroy to a person
    /// while the first is still under way. So `holds` is for finished records only.
    ///
    /// The record below is what such a death leaves: `running`, unfinished, nobody holding its
    /// lock. The counterfactual is `take` losing its `finished_at.is_empty()` return: `holds` is
    /// then asked (the count is 1), says no, the record is retired, and the work runs.
    #[test]
    fn the_host_is_not_asked_about_an_operation_still_in_flight() {
        let dir = scratch("in-flight");
        let store = Store::new(&dir, forever());
        let ran = AtomicUsize::new(0);
        let asked = AtomicUsize::new(0);
        left_running(&dir, "op-mid", Duration::from_secs(60));

        let got = store
            .once(
                "op-mid",
                &|_| {
                    asked.fetch_add(1, Ordering::SeqCst);
                    Ok(false)
                },
                |reach| {
                    cross_then(reach, || {
                        ran.fetch_add(1, Ordering::SeqCst);
                        Ok("destroyed a second time".to_string())
                    })
                },
            )
            .unwrap();
        assert!(
            matches!(got, Outcome::Undecided { .. }),
            "an operation that may be running was not answered undecided: {got:?}"
        );
        assert_eq!(
            asked.load(Ordering::SeqCst),
            0,
            "the host was asked about an operation that may be half way through changing it"
        );
        assert_eq!(
            ran.load(Ordering::SeqCst),
            0,
            "an operation that may still be running was run again"
        );
    }

    /// A `running` record whose command crossed `age` ago, as a killed warden leaves it.
    fn left_running(dir: &Path, id: &str, age: Duration) {
        fs::create_dir_all(dir).unwrap();
        let at = (chrono::Utc::now() - chrono::Duration::from_std(age).unwrap()).to_rfc3339();
        let record = Record {
            started_at: at.clone(),
            finished_at: String::new(),
            outcome: None,
            forgotten: false,
            stage: Stage::Running,
            crossed_at: at,
        };
        fs::write(
            dir.join(format!("{id}.json")),
            serde_json::to_vec(&record).unwrap(),
        )
        .unwrap();
    }

    fn past_the_deadline() -> Duration {
        DOER_DEADLINE + Duration::from_secs(60)
    }

    /// **A killed warden's operation is asked again once the host shows it did not take effect**
    /// (SKEIN-1497). The counterfactual is `take` never consulting `overdue`: the answer is then
    /// `Undecided` for ever and `ran` stays 0.
    #[test]
    fn past_its_deadline_a_host_that_disagrees_frees_the_operation() {
        let dir = scratch("overdue-frees");
        left_running(&dir, "op-killed", past_the_deadline());
        let store = Store::new(&dir, forever());
        let ran = AtomicUsize::new(0);
        let got = store
            .once("op-killed", &|_| Ok(false), |reach| {
                cross_then(reach, || {
                    ran.fetch_add(1, Ordering::SeqCst);
                    Ok("asked and run".to_string())
                })
            })
            .unwrap();
        assert_eq!(got, Outcome::Ran(Ok("asked and run".to_string())));
        assert_eq!(ran.load(Ordering::SeqCst), 1);
    }

    /// The host agreeing, or being unreadable, proves nothing about a half-finished command. The
    /// counterfactual is `overdue` followed by a retire on anything but `Ok(false)`.
    #[test]
    fn past_its_deadline_a_host_that_agrees_or_cannot_be_read_stays_undecided() {
        let answers: [fn(&str) -> Result<bool, String>; 2] =
            [|_| Ok(true), |_| Err("sbx ls failed".to_string())];
        for (n, holds) in answers.into_iter().enumerate() {
            let dir = scratch(&format!("overdue-stays-{n}"));
            left_running(&dir, "op-killed", past_the_deadline());
            let ran = AtomicUsize::new(0);
            let got = Store::new(&dir, forever())
                .once("op-killed", &holds, |reach| {
                    cross_then(reach, || {
                        ran.fetch_add(1, Ordering::SeqCst);
                        Ok("ran".to_string())
                    })
                })
                .unwrap();
            assert!(matches!(got, Outcome::Undecided { .. }), "{n}: {got:?}");
            assert_eq!(ran.load(Ordering::SeqCst), 0, "answer {n} re-ran the work");
        }
    }

    /// A slow `sbx` under a live warden is not a dead one's leftovers, however old. The
    /// counterfactual is `overdue` losing its lock probe: `holds` is then asked.
    #[test]
    fn past_its_deadline_a_record_a_live_warden_holds_is_not_judged() {
        let dir = scratch("overdue-held");
        left_running(&dir, "op-slow", past_the_deadline());
        let held = fs::File::open(dir.join("op-slow.json")).unwrap();
        assert!(lock(&held), "the test could not take the lock it relies on");
        let asked = AtomicUsize::new(0);
        let got = Store::new(&dir, forever())
            .once(
                "op-slow",
                &|_| {
                    asked.fetch_add(1, Ordering::SeqCst);
                    Ok(false)
                },
                |_| Did::Never("must not be reached".into()),
            )
            .unwrap();
        assert!(matches!(got, Outcome::Undecided { .. }), "{got:?}");
        assert_eq!(asked.load(Ordering::SeqCst), 0);
    }

    /// The marker itself holds the lock while the command runs, or the test above describes a
    /// warden that does not exist. The counterfactual is `reach` going back to a plain `put`.
    #[test]
    fn a_running_command_holds_the_lock_on_its_marker() {
        let dir = scratch("marker-locked");
        let store = Store::new(&dir, forever());
        let path = dir.join("op-live.json");
        store
            .once("op-live", &believed, |reach| {
                cross_then(reach, || {
                    let probe = fs::File::open(&path).unwrap();
                    assert!(
                        !lock(&probe),
                        "the marker was unlocked while its command ran"
                    );
                    Ok("ran".to_string())
                })
            })
            .unwrap();
    }

    /// **A host nobody could read is neither "still so" nor "no longer so"** (SKEIN-1493).
    ///
    /// Taken as the first, a destroy is replayed over a fleet that may be standing — the bug this
    /// check exists for, back whenever `sbx ls` hiccups. Taken as the second, a create is put to a
    /// person for a sandbox that may be there. So it is an error, the work does not run, and the
    /// record is still there to answer once somebody can look.
    ///
    /// Counterfactual: `holds(..)?` becoming `holds(..).unwrap_or(true)` — the middle call is then
    /// `Ok(Replayed)` and the `expect_err` fails.
    #[test]
    fn a_host_nobody_could_read_neither_replays_the_record_nor_runs_the_work() {
        let dir = scratch("unread");
        let store = Store::new(&dir, forever());
        let ran = AtomicUsize::new(0);
        let work = |reach: Reach<'_>| {
            cross_then(reach, || {
                ran.fetch_add(1, Ordering::SeqCst);
                Ok("fleet destroyed".to_string())
            })
        };

        store.once("op-1", &believed, work).unwrap();
        let why = store
            .once("op-1", &|_| Err("sbx ls did not answer".to_string()), work)
            .expect_err("a record nobody could check was answered from, or run over");
        assert!(why.contains("sbx ls did not answer"), "{why}");
        assert_eq!(ran.load(Ordering::SeqCst), 1, "the work ran on a guess");
        assert_eq!(
            store.once("op-1", &believed, work).unwrap(),
            Outcome::Replayed(Ok("fleet destroyed".into())),
            "the record did not survive a check that could not be made"
        );
        assert_eq!(ran.load(Ordering::SeqCst), 1);
    }

    /// **Only the record that was judged is retired — not whatever is at its name by then**
    /// (SKEIN-1493).
    ///
    /// Two requests for one id can both find the stale record and both be told the host has moved
    /// on. The first retires it and claims the id; the second, a moment behind, must not remove
    /// that claim in the old record's place — it would then claim the id too, and one piece of work
    /// would be in front of a person twice. So [`retired`] compares the bytes under its lock with
    /// the record it was handed, whole.
    ///
    /// Counterfactual: comparing nothing, or only "is it finished" — the first call below then
    /// removes a record nobody judged, and the `exists` assertion fails.
    #[test]
    fn only_the_record_that_was_judged_is_retired() {
        let dir = scratch("retired");
        let store = Store::new(&dir, forever());
        let path = store.path("op-1");
        let finished = |at: &str| Record {
            started_at: at.to_string(),
            finished_at: at.to_string(),
            outcome: Some(Ok("fleet destroyed".into())),
            forgotten: false,
            stage: Stage::Running,
            crossed_at: String::new(),
        };
        let judged = finished("2026-11-01T00:00:00+00:00");
        let later = finished("2026-11-17T00:00:00+00:00");

        // What is at the name is a LATER run's record: the one judged is already gone.
        store.put(&path, &later).unwrap();
        assert!(
            !retired(&path, &judged),
            "a record nobody judged was retired"
        );
        assert!(path.exists(), "a record nobody judged was removed");

        // The permitted case, so the refusal above is not simply `retired` never saying yes.
        assert!(retired(&path, &later));
        assert!(!path.exists());

        // And a claim in front of a person — locked by the warden holding it — is not removed even
        // by a caller holding the very bytes it was written with.
        let asking = Record {
            finished_at: String::new(),
            outcome: None,
            stage: Stage::Asking,
            ..later.clone()
        };
        let Claim::Ours(_held) = store.claim(&path, &asking) else {
            panic!("the id was free and could not be claimed");
        };
        assert!(
            !retired(&path, &asking),
            "a claim its warden still holds was retired"
        );
        assert!(path.exists());
    }
}
