//! What skein writes down beside the queue about one pull request.
//!
//! Three little files under the fleet's own state, and they answer three questions a workflow run
//! cannot answer from GitHub: **which workflow governs this pull request** (the assignment),
//! **whether it is stopped** and why, and **what has already happened to it** (the journal).
//! [`standing`] is the three read together, which is what a row in the cockpit shows.
//!
//! They are one file because they are one discipline. Every write goes through [`update_file`],
//! which takes the lock, re-reads, and refuses to write over a file it could not parse — so a
//! corrupt assignment file is never silently replaced, and two writers at once lose neither a stop,
//! a choice, nor a journal entry. Splitting them would be three copies of that rule.

use super::now_ms;
// In scope for a doc link, not for the code: `[`trains`]` below points at `sweep`, and rustdoc
// resolves an intra-doc link against what the file it appears in imports. Without this the
// link goes quiet, and nothing gates `cargo doc`.
#[allow(unused_imports)]
use super::trains;
use crate::workflow::Workflow;
use std::path::PathBuf;

/// What happened when skein tried to take a step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// It happened. The string is what the audit and the row say.
    Did(String),
    /// Nothing to do — the step was `wait`, which is an answer and not an absence.
    Waited(String),
    /// The workflow has stopped on this pull request, and will not act again until a person clears
    /// it. Either because a step said `flag`, or because an action failed.
    Stopped(String),
}

/// Which workflow a pull request carries, and how it came to carry it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Carries {
    /// Somebody chose this one on the row. Outranks every rule, in both directions.
    ///
    /// Outranks every rule about *which* workflow — never a workflow's own conditions. Where those
    /// do not hold, this is [`Carries::Holding`] instead (SKEIN-279).
    Assigned(String),
    /// A workflow's own `matches` claimed it.
    Matched(String),
    /// Assigned by hand, and **holding**: the workflow governs this pull request and its own
    /// `matches` do not hold yet, so it may not act (SKEIN-279).
    ///
    /// A third answer rather than a flavour of [`Carries::Assigned`], because the two are read for
    /// different things and used to be the same word. The row must still say the workflow is on it
    /// — somebody chose it, and it has not been forgotten — while nothing may act, which is
    /// [`Carries::name`] against [`Carries::acting`].
    ///
    /// **This is not a stop and not a wait.** Nothing is written down, no clock runs, and there is
    /// nothing for anybody to clear: `unmet` is recomputed from the queue every pass, so the pull
    /// request starts acting on the pass after the condition becomes true. It is also kept out of
    /// a serial train's line, which is the part that matters most — a pull request that cannot act
    /// standing at the front of a train would hold up everything behind it, which is the failure
    /// this whole design is written against.
    Holding {
        name: String,
        /// The conditions that do not hold, spelled as they are written in the file, so the
        /// sentence on the row is checkable against it. From [`crate::workflow::unmet`].
        unmet: Vec<String>,
    },
    /// Excluded by hand — the row said "no workflow", and no rule may override that.
    ///
    /// A distinct answer from [`Carries::Nothing`] and the whole reason assignment is a
    /// three-valued thing: with the tick sweeping every repo in the registry, "not this one" has to
    /// be sayable about a single pull request. Otherwise the only way to exclude one is to edit the
    /// rule for everybody.
    Excluded,
    /// No rule claims it and nobody assigned one.
    Nothing,
}

impl Carries {
    /// The workflow's name, where there is one. **What governs it**, which is what a row shows —
    /// including one that is holding, because an assignment nobody can see is one that looks lost.
    pub fn name(&self) -> Option<&str> {
        match self {
            Carries::Assigned(name) | Carries::Matched(name) | Carries::Holding { name, .. } => {
                Some(name)
            }
            Carries::Excluded | Carries::Nothing => None,
        }
    }

    /// The workflow that may act on this pull request **now**, which is a different question from
    /// [`Carries::name`] and the whole of SKEIN-279.
    ///
    /// Everything that acts, or that decides who acts next, reads this one: the sweep, and the
    /// serial line it builds. Everything that draws reads [`Carries::name`].
    pub fn acting(&self) -> Option<&str> {
        match self {
            Carries::Assigned(name) | Carries::Matched(name) => Some(name),
            Carries::Holding { .. } | Carries::Excluded | Carries::Nothing => None,
        }
    }
}

fn assign_path(repo_id: &str) -> PathBuf {
    crate::prq::review_dir(repo_id).join("workflow-assigned.json")
}

/// Read one of this module's three per-repo files, change it, and write it back — **with one
/// exclusive lock held across all three, and the write itself atomic** (SKEIN-414).
///
/// Every one of the three is a read-modify-write over a whole map: `assign` inserts one choice,
/// `stop` inserts one stop, `record` appends one line to one pull request's timeline. Two of those
/// interleaving is last-write-wins, and what the loser loses is a whole stop or a whole choice
/// rather than a field. The writers are not hypothetical and never were: the tick sweeps every repo
/// on its own thread while the cockpit's routes call `assign`, `stop` and `clear` from request
/// threads. A bare `std::fs::write` also truncates before it writes, so a crash or a kill mid-write
/// left a half-written file — the *manufacturing* end of SKEIN-359, in the module whose reading end
/// it had already fixed.
///
/// **Not [`crate::util::update_json`], which is otherwise exactly this.** That one words the
/// refusal itself, and these three files each say something different about what would be lost —
/// and the journal is the one file in the fleet that is deliberately written over when it cannot be
/// read (argued at [`record`]). So the recovery is a parameter: `unreadable` is handed the reason
/// [`crate::util::read_json_or_why`] gives and decides, in the caller's own words, whether this
/// write may go ahead at all. The lock file is [`crate::util::lock_beside`]'s, so a file guarded
/// here and a file guarded by `update_json` can never be guarded by two different locks.
///
/// `change` answers whether it changed anything: `false` writes nothing and is `Ok`, because a
/// clear on a pull request with no stop must not rewrite the file — nor fail because it could not.
fn update_file<T>(
    path: &std::path::Path,
    unreadable: impl FnOnce(String) -> Result<T, String>,
    change: impl FnOnce(&mut T) -> bool,
) -> Result<(), String>
where
    T: serde::de::DeserializeOwned + serde::Serialize + Default,
{
    let dir = path
        .parent()
        .ok_or("no directory to write into")?
        .to_path_buf();
    crate::util::with_lock(&crate::util::lock_beside(path)?, || {
        let mut current: T = match crate::util::read_json_or_why(path) {
            Ok(found) => found.unwrap_or_default(),
            Err(why) => unreadable(why)?,
        };
        if !change(&mut current) {
            return Ok(());
        }
        let body = serde_json::to_vec_pretty(&current).map_err(|e| e.to_string())?;
        std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        // Named here rather than left to `write_atomic`, whose failures are "writing temp" and
        // "renaming into place" — true of any file in skein, and the one thing a person reading a
        // row needs is WHICH file could not be written.
        crate::util::write_atomic(path, &dir, &body).map_err(|e| format!("{}: {e}", path.display()))
    })
}

/// What skein says instead of writing over the workflow choices it could not read.
///
/// A parse error on its own says what broke and not what skein declined to do, which is the half a
/// person acts on. Same shape as `repos::unreadable_refusal` and `tracking::unreadable_refusal`, in
/// this file's own words.
fn assignment_refusal(why: String) -> String {
    format!(
        "not saving over the workflow choices skein cannot read ({why}). Saving now would replace \
         every choice made in this repo — including the pull requests somebody excluded by hand — \
         with a default nobody chose. Fix or move the file, then try again."
    )
}

/// Change the assignments, refusing over a file skein could not read.
///
/// A file nobody has written yet is no assignments, which is what every repo starts as. A file that
/// is *there* and will not parse still holds every choice somebody made, and [`assign`] says why
/// replacing those is not a thing one assignment gets to do.
fn update_assigned(
    repo_id: &str,
    change: impl FnOnce(&mut std::collections::BTreeMap<String, String>) -> bool,
) -> Result<(), String> {
    update_file(
        &assign_path(repo_id),
        |why| Err(assignment_refusal(why)),
        change,
    )
}

/// The assignments, with an unreadable file read as none.
///
/// For the readers only — the row, and the sweep deciding what governs a pull request. Both of them
/// fail toward *no workflow acting*, which is the direction a person can see and correct. The
/// writers go through [`update_assigned`], where the same misreading is what destroys the choices.
fn read_assigned(repo_id: &str) -> std::collections::BTreeMap<String, String> {
    crate::util::read_json_or_why(&assign_path(repo_id))
        .ok()
        .flatten()
        .unwrap_or_default()
}

/// Put a workflow on one pull request, or take it off.
///
/// `name` empty means **excluded** — not "no opinion". Clearing the choice entirely is
/// [`unassign`], which lets the rules speak again. Three states, because with a rule sweeping every
/// repo in the fleet, "leave this one alone" is a thing somebody has to be able to say.
///
/// **Refuses over a file it could not read** (SKEIN-359). This is a read-modify-write over every
/// assignment in the repo, so an unparseable file read as empty turned assigning one pull request
/// into forgetting the choice made on all the others — and the choice that matters most is
/// `Excluded`, which is a person saying "no rule may touch this one". Losing it does not leave a
/// pull request idle; it hands it back to the sweep, which then acts on the pull request somebody
/// took out of its reach.
pub fn assign(repo_id: &str, number: u64, name: &str) -> Result<(), String> {
    update_assigned(repo_id, |all| {
        all.insert(number.to_string(), name.to_string());
        true
    })
}

/// **Did somebody choose this pull request's workflow by hand?** — §10's layer 7.
///
/// The one thing that overrides `Repo::auto_review`, and it is asked of the same file
/// [`carries`] reads so the two cannot disagree about what an assignment is.
///
/// **An empty name is not an assignment.** That is [`Carries::Excluded`] — a person saying "no rule
/// may touch this one" — and reading it as "somebody switched this on" would turn the row that
/// means *leave it alone* into the row that means *act on it whatever the repo says*, which is the
/// per-PR flag inverted on exactly the pull request somebody took out of reach.
pub(super) fn chosen_by_hand(repo_id: &str, number: u64) -> bool {
    read_assigned(repo_id)
        .get(&number.to_string())
        .is_some_and(|name| !name.is_empty())
}

/// Forget any choice made on this pull request, and let the rules decide again.
///
/// Refuses on an unreadable file, for [`assign`]'s reason: forgetting one choice is not how the
/// rest are forgotten.
pub fn unassign(repo_id: &str, number: u64) -> Result<(), String> {
    // Written back even when the choice was not there, as it always was: the answer this owes its
    // caller is "there is no choice on this pull request", and that is true either way.
    update_assigned(repo_id, |all| {
        all.remove(&number.to_string());
        true
    })
}

/// What a choice made on the row means, in one place.
///
/// The mapping lived in the route and grew a bug there within an hour of being written: "let it run
/// again" sent no name, "no name" meant *forget the choice*, and so clearing a stop quietly took the
/// workflow off the pull request as well — one button doing a second thing nobody asked for. It is
/// here now because it is a rule about what an assignment IS, and a route is where rules go to be
/// untested.
///
/// - `name: Some("x")` — put x on it.
/// - `name: Some("")` — leave this one out, and let no rule claim it.
/// - `unassign` — forget the choice; the rules speak for it again.
/// - neither — **change nothing**. `clear_stop` alone is not a statement about what governs it.
///
/// A `clear_stop` that could not be written is an `Err` and stops the call there — see [`clear`].
pub fn apply(
    repo_id: &str,
    number: u64,
    name: Option<&str>,
    unassign_it: bool,
    clear_stop: bool,
) -> Result<(), String> {
    // Propagated, not swallowed: this is the half the person actually pressed, and a clear that
    // did not land leaves the pull request stopped. Before the assignment, so a failure stops here
    // rather than reporting an error over a change that did go through.
    if clear_stop {
        clear(repo_id, number)?;
    }
    match (name, unassign_it) {
        (Some(name), _) => assign(repo_id, number, name),
        (None, true) => unassign(repo_id, number),
        (None, false) => Ok(()),
    }
}

/// Which workflow governs this pull request.
///
/// The choice on the row wins over every rule. Where there is none, the first workflow whose
/// `matches` claims it does — and a workflow with no `matches` claims nothing, ever
/// ([`crate::workflow::claims`]).
///
/// **An assignment says which workflow, not that its conditions are met** (SKEIN-279). So an
/// assigned workflow whose own `matches` do not hold comes back [`Carries::Holding`]: it governs
/// the pull request and may not act on it, and both halves are said rather than one silently
/// winning. Until this, `matches` was evaluated on exactly one of the two roads to acting — assign
/// the documented merge train to a draft, or to an unapproved pull request, and it would label,
/// rebase and wait its way through the steps with every guard written in `matches` switched off.
/// The consequence that could not be undone was fixed in SKEIN-237 by moving that one guard into
/// the act; this is the general hole it left behind.
///
/// A **matched** workflow can never be holding: its `matches` were just evaluated to get here.
/// A workflow with no `matches` states no conditions, so assigning it is unconditional — which is
/// what "it only ever runs where somebody assigned it" already meant.
///
/// An assignment naming a workflow that no longer exists is [`Carries::Nothing`] rather than an
/// error: the file it named was edited, and the honest thing is to act on nothing rather than to
/// guess which of the remaining ones was meant. The row says so.
pub fn carries(
    repo_id: &str,
    number: u64,
    facts: &crate::workflow::Facts,
    flows: &[Workflow],
) -> Carries {
    match read_assigned(repo_id).get(&number.to_string()) {
        Some(name) if name.is_empty() => return Carries::Excluded,
        Some(name) => {
            return match flows.iter().find(|f| &f.name == name) {
                Some(flow) => match crate::workflow::unmet(flow, facts) {
                    unmet if unmet.is_empty() => Carries::Assigned(name.clone()),
                    unmet => Carries::Holding {
                        name: name.clone(),
                        unmet,
                    },
                },
                None => Carries::Nothing,
            }
        }
        None => {}
    }
    flows
        .iter()
        .find(|flow| crate::workflow::claims(flow, facts))
        .map(|flow| Carries::Matched(flow.name.clone()))
        .unwrap_or(Carries::Nothing)
}

/// Where a repo's stopped pull requests are written down.
///
/// Beside the review state for that repo, and on the host: this is skein's own memory of a decision
/// it made, not something a box should be able to edit.
fn stops_path(repo_id: &str) -> PathBuf {
    crate::prq::review_dir(repo_id).join("workflow-stops.json")
}

/// Why this pull request's workflow is stopped, if it is.
pub fn stopped(repo_id: &str, number: u64) -> Option<String> {
    read_stops(repo_id).remove(&number.to_string())
}

/// Every stopped pull request in this repo somebody can still act on, in numeric order, in the
/// shape the counts payload carries ([`crate::prq::StoppedPr`] — the type is the payload's, the
/// file is this module's).
///
/// Numeric rather than the file's own: the stops are keyed by strings, and `"10"` sorting before
/// `"9"` is not an order anybody asked to read a banner in.
///
/// # A stop is only worth saying where there is a row to clear it from
///
/// This is what the cockpit's banner is built from, and a banner is a demand for somebody's
/// attention. The stop FILE is a different thing: it is skein's memory of a refusal, and its whole
/// job is to outlive the pass that wrote it. Reading the file straight out onto the banner
/// conflated the two, and the difference showed up as the one failure a banner cannot survive
/// (SKEIN-241): the train stops on #123, a person merges #123 on GitHub, #123 leaves the queue —
/// and an orange row sits above the whole application naming a pull request with no row, for ever.
/// Both "let it run again" buttons are built from the live queue (`src/web/index.html`), so there
/// was no way to dismiss it at all. `crate::prq::queue_within` already makes the argument this
/// rests on: *a banner that is always there stops being read.*
///
/// So the file keeps everything and this answers about what is in front of a person. That also
/// closes the second half of SKEIN-241, without either surface having to know about the other:
/// the panel's set ([`trains`]) is drawn from the queue too, so the banner can no longer name a
/// pull request the panel has never heard of.
///
/// **The queue is read from what is already on this machine — never over the network.** Same rule
/// and same two roads as [`crate::prq::remembered_head`]: this runs on the badge poll, for every
/// repo in the fleet, and a filter that cost a GitHub round trip would be paid for by the one
/// thing the ten-minute badge budget exists to protect (SKEIN-208).
///
/// **Blindness shows everything, rather than nothing.** No queue on this machine yet, or one whose
/// searches did not see every open pull request ([`crate::prq::Queue::whole`]), and the file is
/// answered unfiltered. A refresh that went dark has an empty `prs` list for the same reason a
/// repo with nothing open does, and reading that as "every stop is dismissible" would silence
/// every stop in the fleet during one rate-limit window — which is SKEIN-229's failure, arriving
/// through a different file. Erring toward a banner that is too loud is recoverable; erring toward
/// one that is silent is the failure this whole feature exists to prevent.
pub fn stops(repo_id: &str) -> Vec<crate::prq::StoppedPr> {
    let open = open_pull_requests(repo_id);
    let mut out: Vec<crate::prq::StoppedPr> = read_stops(repo_id)
        .into_iter()
        .filter_map(|(number, why)| {
            number
                .parse::<u64>()
                .ok()
                .map(|number| crate::prq::StoppedPr { number, why })
        })
        .filter(|s| match &open {
            Some(open) => open.contains(&s.number),
            None => true,
        })
        .collect();
    out.sort_by_key(|s| s.number);
    out
}

/// Which pull requests this repo has open, from what is already on this machine — and `None` when
/// nothing here can say.
///
/// `None` is the answer for two different situations and deliberately the same one: no queue has
/// been read for this repo yet, and a queue whose searches were cut off or failed. Both mean a
/// pull request's ABSENCE from the list is evidence about the searches rather than about the pull
/// request, which is exactly what [`crate::prq::Queue::whole`] was added to say — the archive and
/// snooze prunes in `prq::queue_within` read it for the same reason.
fn open_pull_requests(repo_id: &str) -> Option<Vec<u64>> {
    let known = crate::prq::unexpired(repo_id).or_else(|| crate::prq::remembered(repo_id))?;
    known
        .whole
        .then(|| known.prs.iter().map(|pr| pr.number).collect())
}

/// Change the stops, with the refusal worded by whoever is writing.
///
/// A file nobody has written yet is no stops — a repo where nothing has ever gone wrong. A file
/// that is there and will not parse holds every stop in the repo, and the two writers below say
/// different things about that: [`stop`] has nobody to tell but stderr, and [`clear`] is answering
/// a press. Under one lock and written atomically, like the other two files (SKEIN-414).
fn update_stops(
    repo_id: &str,
    unreadable: impl FnOnce(String) -> Result<std::collections::BTreeMap<String, String>, String>,
    change: impl FnOnce(&mut std::collections::BTreeMap<String, String>) -> bool,
) -> Result<(), String> {
    update_file(&stops_path(repo_id), unreadable, change)
}

/// The stops, with an unreadable file read as none.
///
/// For the readers, where "no stop" is the loud answer rather than the quiet one: a stop that
/// cannot be read means the banner and the train view show a pull request as free to move, which is
/// wrong in the direction somebody notices. The writers go through [`update_stops`], because for
/// them the same misreading is what *destroys* the stops.
pub(super) fn read_stops(repo_id: &str) -> std::collections::BTreeMap<String, String> {
    crate::util::read_json_or_why(&stops_path(repo_id))
        .ok()
        .flatten()
        .unwrap_or_default()
}

/// Stop this pull request's workflow, and say why.
///
/// **Never over a stop file skein could not read** (SKEIN-359). Every stop in the repo is in that
/// file, and writing this one over a read that answered "no stops" would let every other stopped
/// pull request move again — each of them stopped because acting on it went wrong once, which is
/// the loop this whole rule exists to prevent, arriving all at once and for every pull request
/// rather than for one.
pub fn stop(repo_id: &str, number: u64, why: &str) {
    let wrote = update_stops(
        repo_id,
        |unreadable| {
            Err(format!(
                "the stop file will not parse ({unreadable}), and replacing it would let every \
                 other stopped pull request in {repo_id} move again. Fix or move that file"
            ))
        },
        |stops| {
            stops.insert(number.to_string(), why.to_string());
            true
        },
    );
    // Loudest of the three, because nothing else will say it: this one is not on a person's button,
    // so the sentence on stderr is the only place the failure exists. Both ways of failing are said
    // here rather than one each side of the read, because what a person does about them is the
    // same, and the consequence certainly is: the next poll re-attempts an action that has already
    // failed once, which is the loop this whole rule exists to prevent.
    if let Err(e) = wrote {
        eprintln!(
            "skein: #{number}'s workflow stopped ({why}) and skein could not write it down — {e}. \
             Until that is fixed this pull request may be attempted again."
        );
    }
}

/// Let it run again. What a person does after fixing whatever the reason was.
///
/// **The write is the act, and it is reported.** This used to discard it — `let _ = …` on the write
/// — and then journal the clear unconditionally, so a stops file that could not be written (a
/// read-only host state directory, a full disk) left the stop exactly where it was, put
/// a line in the timeline saying a person had lifted it, and answered the button "done". The
/// person is told it worked, shown a record saying it worked, and the train never moves
/// (SKEIN-249). Of everything skein writes, this was the only place a journal entry could describe
/// an act that had not happened — its sibling [`stop`] already says a failed write out loud, and
/// says it for the same reason in the opposite direction.
///
/// So the journal entry is written only after the file is, and the error travels back through
/// [`apply`] to the row. The sentence names the consequence rather than the syscall, because what
/// somebody needs to know is not that a write failed but that the pull request is still stopped.
///
/// **A pull request with no stop is `Ok`, not an error.** Nothing needed doing and nothing was
/// written, which is the same rule [`crate::prq::set_archived`] keeps and for the same reason: a
/// retried request must not report a failure for having arrived twice.
pub fn clear(repo_id: &str, number: u64) -> Result<(), String> {
    // Both ways this can fail end the same sentence, and the sentence is the point: what somebody
    // needs to know is not that a read or a write failed but that the pull request is still
    // stopped. An unreadable file is refused for the reason one paragraph up — read as "no stops"
    // this would answer the button Ok for a stop it never saw, and then write an empty file over
    // every other stop in the repo (SKEIN-359).
    let mut removed = false;
    update_stops(
        repo_id,
        |unreadable| {
            Err(format!(
                "skein cannot read the stop file, and will not replace it with one holding no \
                 stops at all: {unreadable}"
            ))
        },
        |stops| {
            removed = stops.remove(&number.to_string()).is_some();
            // Nothing to clear is not a failure and is not a write: a retried press must not report
            // one, and must not depend on a file it has no reason to touch.
            removed
        },
    )
    .map_err(|e| format!("#{number} is still stopped — {e}"))?;
    if !removed {
        return Ok(());
    }
    // A person clearing a stop is an event the timeline must show — without it, a journal
    // reads "stopped … did …" with no sign of the hand that let it move again. AFTER the write,
    // so the timeline can only ever describe something that happened.
    record(
        repo_id,
        number,
        "",
        0,
        "cleared",
        "the stop was cleared — the workflow may act again",
    );
    Ok(())
}

/// One line of a pull request's workflow history — the durable answer to "what happened to this
/// one, and in what order".
///
/// The stop file says only the *latest* reason; the audit log belongs to the host and mixes every
/// box's events. This is skein's own per-PR timeline, written the moment something happens, read
/// oldest-first.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct JournalEntry {
    /// When, as epoch milliseconds.
    pub at_ms: i64,
    /// Which workflow acted. Empty for events no workflow owns — a person clearing a stop.
    pub flow: String,
    /// Which step, 1-based. `0` means "not a step": a clear, or a stop written by hand.
    pub step: usize,
    /// `"did"` | `"stopped"` | `"cleared"`.
    pub kind: String,
    /// The human sentence — the same one the audit and the row carry.
    pub what: String,
}

/// Where a repo's workflow journal lives: beside the stops, keyed the same way.
pub(super) fn journal_path(repo_id: &str) -> PathBuf {
    crate::prq::review_dir(repo_id).join("workflow-journal.json")
}

fn read_journal(repo_id: &str) -> std::collections::BTreeMap<String, Vec<JournalEntry>> {
    // A corrupt or absent file reads as empty, never as an error: the journal is a record of what
    // happened, and losing it must not stop anything from happening.
    crate::util::read_json_or_why(&journal_path(repo_id))
        .ok()
        .flatten()
        .unwrap_or_default()
}

/// Write one journal entry down, now.
///
/// Write-through like the stops file: every event lands on disk before the function returns, so a
/// server that dies mid-pass has still said what it did. Each pull request keeps its newest 50
/// entries — a train PR sees a handful of acts on its way to merged, so 50 covers weeks of
/// stop/clear churn without the file growing without bound.
///
/// **This is the one file in SKEIN-359's list that is deliberately written over when it cannot be
/// read, and the argument is not "best effort".** It is that both answers lose the same thing —
/// history — and only one of them ever gets it back. Refusing would keep the unreadable bytes and
/// end journalling for this repo permanently: nothing repairs the file, `record` is best-effort so
/// no caller is stopped by the refusal, and the sweep would go on acting with no record that it
/// did. Writing over it loses what was there and journalling resumes on the next event. Nothing
/// reads this file to decide anything — [`stops`] is what decides, and it refuses — so what is lost
/// is a timeline somebody reads a week later, not a stop somebody's pull request depends on. It is
/// said out loud each time rather than once per process, because the line names the repo whose
/// timeline was discarded and there is more than one repo.
pub(super) fn record(repo_id: &str, number: u64, flow: &str, step: usize, kind: &str, what: &str) {
    let at_ms = now_ms();
    // Under the same lock and the same atomic write as the other two (SKEIN-414). The sweep
    // journals on its own thread while a person's clear journals from a request thread, and two
    // appends interleaving lose a whole entry — which for a timeline is the one kind of loss that
    // cannot be noticed, because what is missing is the line that would have said so.
    let written = update_file(
        &journal_path(repo_id),
        |unreadable| {
            eprintln!(
                "skein: {repo_id}'s workflow journal will not parse ({unreadable}) — the timeline \
                 it held is being written over so that journalling can carry on. Nothing acts on \
                 this file; the stops it sits beside are refused instead."
            );
            Ok(Default::default())
        },
        |all: &mut std::collections::BTreeMap<String, Vec<JournalEntry>>| {
            let entries = all.entry(number.to_string()).or_default();
            entries.push(JournalEntry {
                at_ms,
                flow: flow.to_string(),
                step,
                kind: kind.to_string(),
                what: what.to_string(),
            });
            if entries.len() > 50 {
                let drop = entries.len() - 50;
                entries.drain(..drop);
            }
            true
        },
    );
    if let Err(e) = written {
        // Best-effort, said out loud: a journal that could not be written loses history, not
        // safety — the stop file is the one whose loss re-attempts an action.
        eprintln!("skein: could not journal #{number}'s workflow event ({e})");
    }
}

/// One pull request's workflow history, oldest first.
pub fn journal(repo_id: &str, number: u64) -> Vec<JournalEntry> {
    read_journal(repo_id)
        .remove(&number.to_string())
        .unwrap_or_default()
}

/// Every journaled pull request in this repo, in numeric order, each timeline oldest first.
pub fn journals(repo_id: &str) -> std::collections::BTreeMap<u64, Vec<JournalEntry>> {
    read_journal(repo_id)
        .into_iter()
        .filter_map(|(number, entries)| number.parse::<u64>().ok().map(|n| (n, entries)))
        .collect()
}

/// What a workflow would do to one pull request, and why — without doing any of it.
///
/// The dry run the owner asked to see before trusting this, and the same [`crate::workflow::next`]
/// the tick uses. Deliberately the same function: a preview computed a second way is a preview that
/// can disagree with what happens, and the whole point of showing it is that it cannot.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Standing {
    /// The workflow's name, or empty.
    pub workflow: String,
    /// `assigned` | `matched` | `excluded` | `none` — how it came to carry that workflow, because
    /// "you chose this" and "a rule chose this" are different things to see on a row.
    pub how: String,
    /// The step it would take next, spelled as it is written in the file. Empty when nothing
    /// applies, which is what a healthy workflow says most of the time.
    pub next: String,
    /// Which step that is, 1-based, for a row that wants to say "waiting on step 2".
    pub step: usize,
    /// Why it is stopped, if it is. A stopped workflow does nothing until this is cleared.
    pub stopped: String,
    /// Why the workflow on this pull request is not acting, though nothing is wrong (SKEIN-279):
    /// somebody assigned it and its own `matches` do not hold yet. Empty when it is acting
    /// normally.
    ///
    /// Not a stop and not a wait, and it says so in those words — there is nothing to clear and no
    /// clock running. The conditions are spelled as the file spells them, so the sentence is
    /// checkable against the workflow somebody is reading.
    ///
    /// A separate field rather than a sentence in [`Standing::next`], because `next` is a step
    /// spelled as it is written in the file and this is the reason there is no step. An older
    /// cockpit that does not read this still says "nothing to do right now", which is true; a
    /// cockpit that does says which condition.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub holding: String,
}

/// Everything the cockpit needs to draw one pull request's workflow state.
pub fn standing(
    repo_id: &str,
    number: u64,
    facts: &crate::workflow::Facts,
    flows: &[Workflow],
) -> Standing {
    let carried = carries(repo_id, number, facts, flows);
    // A holding workflow is still an ASSIGNED one — that is how it came to carry the pull request,
    // and it is what the row's chooser has to show as chosen. What holds it back is `holding`, so
    // the two facts are separate rather than one overwriting the other.
    let how = match &carried {
        Carries::Assigned(_) | Carries::Holding { .. } => "assigned",
        Carries::Matched(_) => "matched",
        Carries::Excluded => "excluded",
        Carries::Nothing => "none",
    };
    let holding = match &carried {
        Carries::Holding { name, unmet } => format!(
            "{name} is on this pull request and is not acting yet: its own {} {} not true — \
             {}. Nothing is stopped and nothing is waiting on a clock; it joins in on the next \
             pass after that changes.",
            match unmet.len() {
                1 => "condition",
                _ => "conditions",
            },
            match unmet.len() {
                1 => "is",
                _ => "are",
            },
            unmet.join(", "),
        ),
        _ => String::new(),
    };
    // `acting`, not `name`: a holding workflow has no next step, and asking for one would spell
    // out a step it is not going to take.
    let flow = carried
        .acting()
        .and_then(|name| flows.iter().find(|f| f.name == name));
    let chosen = flow.and_then(|flow| crate::workflow::next(flow, facts));
    Standing {
        workflow: carried.name().unwrap_or_default().to_string(),
        how: how.to_string(),
        holding,
        next: chosen
            .as_ref()
            .map(|c| crate::workflow::spell_act(&c.act))
            .unwrap_or_default(),
        step: chosen.as_ref().map(|c| c.step + 1).unwrap_or(0),
        stopped: stopped(repo_id, number).unwrap_or_default(),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[allow(unused_imports)]
    use crate::prwork::facts::facts_of;
    #[allow(unused_imports)]
    use crate::prwork::testkit::*;
    #[allow(unused_imports)]
    use crate::prwork::*;
    #[allow(unused_imports)]
    use crate::workflow::{Act, Chosen, Merge, MergeAs, Update, Workflow};
    #[allow(unused_imports)]
    use std::io::{Read, Write};
    #[allow(unused_imports)]
    use std::sync::{Arc, Mutex};

    /// Which workflow governs a pull request, and who gets the last word.
    ///
    /// The owner's answer was "rules, plus a per-PR override" — and the override matters more than
    /// it looks, because the tick sweeps every repo in the registry. Without a way to say "not this
    /// one" about a single pull request, the only way to exclude one is to edit the rule for
    /// everybody, which is how a rule stops being written honestly.
    ///
    /// So the choice on a row wins in BOTH directions: it can put a workflow on a pull request no
    /// rule claims, and it can keep every rule off one.
    #[test]
    fn the_row_has_the_last_word_over_a_rule() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        std::env::set_var("SKEIN_HOME", home.as_ref() as &std::path::Path);

        let flows = crate::workflow::from_bytes(
            br#"{"workflow":[
              {"name":"ship-mine","matches":["mine"],"steps":[{"when":[],"do":"merge:squash"}]},
              {"name":"by-hand","steps":[{"when":[],"do":"flag:look at this"}]}]}"#,
        )
        .unwrap();
        let mine = crate::workflow::Facts {
            mine: true,
            ..Default::default()
        };
        let theirs = crate::workflow::Facts {
            mine: false,
            ..Default::default()
        };

        // A rule claims what it matches, and nothing else.
        assert_eq!(
            carries("demo", 1, &mine, &flows),
            Carries::Matched("ship-mine".into())
        );
        assert_eq!(carries("demo", 2, &theirs, &flows), Carries::Nothing);

        // A workflow with no rule of its own is never picked up by matching — it exists to be
        // chosen, and choosing it works on a pull request no rule would have claimed.
        assign("demo", 2, "by-hand").unwrap();
        assert_eq!(
            carries("demo", 2, &theirs, &flows),
            Carries::Assigned("by-hand".into())
        );

        // And the row overrules a rule that would otherwise have claimed it.
        assign("demo", 1, "by-hand").unwrap();
        assert_eq!(
            carries("demo", 1, &mine, &flows),
            Carries::Assigned("by-hand".into())
        );

        // "No workflow" is a thing you can say, and it is not the same as saying nothing. This is
        // the one that keeps a fleet-wide rule usable.
        assign("demo", 1, "").unwrap();
        assert_eq!(
            carries("demo", 1, &mine, &flows),
            Carries::Excluded,
            "a rule reclaimed a pull request that was excluded by hand"
        );

        // Clearing the choice is different again: the rules speak for it once more.
        unassign("demo", 1).unwrap();
        assert_eq!(
            carries("demo", 1, &mine, &flows),
            Carries::Matched("ship-mine".into())
        );

        // An assignment naming a workflow that has since been deleted acts on nothing, rather than
        // guessing which of the survivors was meant.
        assign("demo", 3, "the-one-that-was-deleted").unwrap();
        assert_eq!(carries("demo", 3, &mine, &flows), Carries::Nothing);

        std::env::remove_var("SKEIN_HOME");
    }

    /// Letting a stopped workflow run again does not also take the workflow off.
    ///
    /// The bug this exists for was written and found within an hour: "let it run again" sends no
    /// workflow name, no name meant "forget the choice", and so one button quietly did two things —
    /// the second being to un-assign the workflow somebody had chosen. On a fleet where a rule
    /// would then re-claim the pull request, that is a change of behaviour nobody asked for,
    /// arriving through a button labelled something else.
    #[test]
    fn clearing_a_stop_does_not_change_what_governs_it() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        std::env::set_var("SKEIN_HOME", home.as_ref() as &std::path::Path);
        let flows = crate::workflow::from_bytes(
            br#"{"workflow":[{"name":"by-hand","steps":[{"when":[],"do":"flag:hi"}]}]}"#,
        )
        .unwrap();
        let facts = crate::workflow::Facts::default();

        apply("demo", 5, Some("by-hand"), false, false).unwrap();
        stop("demo", 5, "CI is red");
        assert!(stopped("demo", 5).is_some());

        // The button, and only the button.
        apply("demo", 5, None, false, true).unwrap();
        assert_eq!(stopped("demo", 5), None, "the stop was not cleared");
        assert_eq!(
            carries("demo", 5, &facts, &flows),
            Carries::Assigned("by-hand".into()),
            "letting it run again silently took the workflow off it"
        );

        // And forgetting the choice is its own request, which still works.
        apply("demo", 5, None, true, false).unwrap();
        assert_eq!(carries("demo", 5, &facts, &flows), Carries::Nothing);

        std::env::remove_var("SKEIN_HOME");
    }

    /// A stacked child never boards the train, and no stack model was needed.
    ///
    /// The one rule from docs/pr-workflow.md ("Stacks need no stack model"): the train only
    /// touches a PR whose base is the trunk. A child's base is its parent's *branch* — merging it
    /// would merge into the parent, not ship it — so `base:trunk` in `matches` keeps it out until
    /// GitHub retargets it onto the trunk after the parent merges.
    #[test]
    fn a_stacked_child_is_kept_out_by_its_matches() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        std::env::set_var("SKEIN_HOME", home.as_ref() as &std::path::Path);

        let flows = crate::workflow::from_bytes(
            br#"{"workflow":[{"name":"merge-train","serial":true,
                 "matches":["ready","approved","review-satisfied","base:trunk"],
                 "steps":[{"when":[],"do":"merge:squash+delete"}]}]}"#,
        )
        .unwrap();
        let pr = |base_ref: &str| -> crate::prq::Pr {
            serde_json::from_value(serde_json::json!({
                "number": 12, "title": "t", "author": "me", "url": "u",
                "head_ref": "feat-child", "head_sha": "abc", "base_ref": base_ref,
                "draft": false, "updated_at": "", "committed_at": "",
                "labels": [], "review_decision": "APPROVED", "mergeable": true,
                "checks": "passing", "my_review": "none", "review_is_current": false,
                "reasons": [], "lane": "needs-you", "box_name": "demo-feat",
            }))
            .unwrap()
        };

        let child = facts_of(&pr("feat-parent"), "me", "main");
        assert_eq!(
            carries("demo", 12, &child, &flows),
            Carries::Nothing,
            "a stacked child was claimed by the train — it would merge into its parent's branch"
        );
        // And the same pull request, retargeted onto the trunk after its parent merged, is an
        // ordinary trunk-based PR the train claims — that is the whole stack mechanism.
        let retargeted = facts_of(&pr("main"), "me", "main");
        assert_eq!(
            carries("demo", 12, &retargeted, &flows),
            Carries::Matched("merge-train".into())
        );

        std::env::remove_var("SKEIN_HOME");
    }

    /// **An assignment says WHICH workflow, not that its conditions are met** (SKEIN-279).
    ///
    /// The rule this pins, in one sentence: a workflow's `matches` are conditions on ACTING, read
    /// on both roads to acting, and an assignment overrides only which workflow is responsible.
    /// Until this, `matches` was evaluated by [`crate::workflow::claims`] and nowhere else, so a
    /// hand assignment switched off every guard written there — the documented merge train,
    /// assigned to a pull request nobody had approved, would label it, rebase it and merge it.
    ///
    /// Both candidate answers are asserted here, because the value of this test is that it fails
    /// under either of the other two:
    ///
    /// * **"this one, guards and all"** — the behaviour that was there. It fails on `#11`, which
    ///   would have had `ci-queue` put on it and started CI on an unapproved change.
    /// * **"the conditions hold, and holding is a stop or a wait"** — the objection that left this
    ///   item open, since "put this on the train, it will go when it is approved" is an ordinary
    ///   thing to want. It fails on the three assertions that nothing was written down, and on the
    ///   second sweep, where approval alone is enough to make it act: no stop to clear, no
    ///   re-assignment, and no clock that could have run out in between.
    ///
    /// And the assertion that is neither: `#11` is the LOWER number, so under the serial train's
    /// oldest-first rule it would be the front. A pull request that cannot act must not be able to
    /// stand at the front of a train — that would park everything behind it on a condition its own
    /// workflow stated — so `#12` merges in the same pass that `#11` is held.
    #[test]
    fn an_assigned_workflow_holds_for_its_own_conditions_without_blocking_the_train() {
        let _g = crate::testutil::env_lock();
        // Pinned where nothing listens, because this reaches `warden_client`: it refuses a
        // test process that has not said which warden to ask rather than opening a connection
        // to whatever warden the machine running the suite can reach (SKEIN-762).
        let _warden = crate::testutil::no_warden();
        let _h = crate::github::HoldClear::new();
        let home = crate::testutil::tempdir();
        let home = home.as_ref() as &std::path::Path;
        let mut env = crate::testutil::env_pins();
        env.set("SKEIN_HOME", home);
        crate::testutil::switch_on(|c| c.pr_workflows = true);
        env.set("GH_TOKEN", "skein-test-gho");
        std::env::remove_var("GITHUB_TOKEN");
        crate::prq::forget_host_token();
        crate::prq::forget_trunks();

        // The train from docs/pr-workflow.md, "The train, written down".
        std::fs::write(
            home.join("workflows.json"),
            br#"{"workflow":[{"name":"merge-train","serial":true,
              "matches":["ready","approved","review-satisfied","base:trunk"],
              "steps":[
                {"when":["changes-requested"],"do":"flag:changes were requested"},
                {"when":["not-mergeable"],"do":"flag:conflicts with the base"},
                {"when":["behind"],"do":"update-branch:rebase"},
                {"when":["checks:failing"],"do":"flag:CI failed"},
                {"when":["no-label:ci-queue"],"do":"add-label:ci-queue"},
                {"when":["label:ci-queue","checks:pending"],"do":"wait:CI is running"},
                {"when":["label:ci-queue","checks:passing","mergeable","current"],
                 "do":"merge:squash+delete"},
                {"when":[],"do":"wait:waiting for GitHub to catch up"}]}]}"#,
        )
        .unwrap();
        std::fs::write(
            home.join("repos.json"),
            br#"[{"id":"demo","source":"https://github.com/acme/thing.git","source_tree":"","store":""}]"#,
        )
        .unwrap();

        // #11 is `ready`, trunk-based, green — and NOT approved. #12 is all four, and the train's
        // own rule claims it. The review decision on #11 is what the second sweep changes.
        let pr = |number: u64, decision: &str, labels: &str| {
            format!(
                r#"{{"number":{number},"title":"t","url":"u","isDraft":false,
                  "author":{{"login":"me"}},"headRefName":"feat-{number}","headRefOid":"abc{number}",
                  "baseRefName":"main","updatedAt":"2026-08-23T00:00:00Z",
                  "reviewDecision":"{decision}","mergeable":"MERGEABLE","mergeStateStatus":"CLEAN",
                  "labels":{{"nodes":[{labels}]}},"latestReviews":{{"nodes":[]}},
                  "commits":{{"nodes":[{{"commit":{{
                    "committedDate":"2026-08-23T00:00:00Z",
                    "statusCheckRollup":{{"contexts":{{"nodes":[
                      {{"status":"COMPLETED","conclusion":"SUCCESS"}}]}}}}}}}}]}}}}"#
            )
        };
        let queue_answer = |eleven: &str| {
            format!(
                r#"{{"data":{{"q0":{{"nodes":[{},{}]}},"q1":{{"nodes":[]}},"q2":{{"nodes":[]}},"q3":{{"nodes":[]}}}}}}"#,
                pr(11, eleven, r#"{"name":"ready"}"#),
                pr(12, "APPROVED", r#"{"name":"ready"},{"name":"ci-queue"}"#),
            )
        };
        let answer = Arc::new(Mutex::new(queue_answer("REVIEW_REQUIRED")));
        let heard: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
        let (seen, queue) = (heard.clone(), answer.clone());
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let mut stream = stream;
                let mut buf = [0u8; 16384];
                let n = stream.read(&mut buf).unwrap_or(0);
                let said = String::from_utf8_lossy(&buf[..n]).to_string();
                let head = said.lines().next().unwrap_or_default().to_string();
                seen.lock().unwrap().push(head.clone());
                let answer = if head.contains("/user") {
                    r#"{"login":"me"}"#.to_string()
                } else if head.starts_with("GET /repos/acme/thing") && !head.contains("/pulls") {
                    r#"{"full_name":"acme/thing","default_branch":"main"}"#.to_string()
                } else if head.contains("/graphql") {
                    queue.lock().unwrap().clone()
                } else {
                    r#"{"merged":true}"#.to_string()
                };
                let _ = stream.write_all(
                    format!(
                        "HTTP/1.1 200 X\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{answer}",
                        answer.len()
                    )
                    .as_bytes(),
                );
            }
        });
        env.set("SKEIN_GITHUB_API", &base);

        assign("demo", 11, "merge-train").unwrap();
        let did = sweep();

        // **Nothing was done to #11**, on the wire, which is the only place it would show.
        let calls = heard.lock().unwrap().clone();
        let touched_11 = |c: &String| c.contains("/11/") || c.contains("/pulls/11");
        assert!(
            !calls.iter().any(touched_11),
            "the train acted on a pull request nobody has approved: {did:?} / {calls:?}"
        );
        // And nothing was written down about it either: no stop to clear, no timeline entry, and
        // therefore no clock that could later turn this into one.
        assert_eq!(stopped("demo", 11), None, "holding became a stop");
        assert!(
            journal("demo", 11).is_empty(),
            "holding was recorded as an event: {:?}",
            journal("demo", 11)
        );

        // **The train moved anyway.** #11 is the lower number and would have been the front.
        assert!(
            calls.iter().any(|c| c.contains("/pulls/12/merge")),
            "a held pull request blocked the train behind it: {did:?} / {calls:?}"
        );

        // **The row says both halves**: the train is on #11 — it must still show as chosen in the
        // pane's chooser — and it is not acting, naming the condition off the file.
        let flows = crate::workflow::load().unwrap();
        let held = crate::prq::queue(&crate::repos::load_repos()[0], false)
            .unwrap()
            .prs
            .into_iter()
            .find(|p| p.number == 11)
            .expect("#11 is in the queue");
        let facts = facts_of(&held, "me", "main");
        assert_eq!(
            carries("demo", 11, &facts, &flows),
            Carries::Holding {
                name: "merge-train".into(),
                // Both, and they are two different sentences to a reader: nobody has approved it,
                // AND this repository's protection is still asking for a review. #11 is
                // `REVIEW_REQUIRED`, which is the state where those really are the same event —
                // on a repo that requires no review they come apart, which is the whole of
                // SKEIN-339.
                unmet: vec!["approved".into(), "review-satisfied".into()]
            },
            "the condition it is holding for is not the one the file states"
        );
        let seen = standing("demo", 11, &facts, &flows);
        assert_eq!(
            (
                seen.how.as_str(),
                seen.workflow.as_str(),
                seen.next.as_str()
            ),
            ("assigned", "merge-train", ""),
            "the dry run lost the assignment, or promised a step: {seen:?}"
        );
        assert!(
            seen.holding.contains("approved"),
            "the row does not say which condition it is not moving on: {seen:?}"
        );
        assert_eq!(
            seen.stopped, "",
            "holding was reported to the pane as a stop, which is something to clear: {seen:?}"
        );

        // **And approval alone starts it.** Nothing is cleared, re-assigned or waited out: the
        // next pass reads the same assignment against new facts.
        *answer.lock().unwrap() = queue_answer("APPROVED");
        crate::prq::invalidate("demo");
        heard.lock().unwrap().clear();
        sweep();
        let calls = heard.lock().unwrap().clone();
        assert!(
            calls
                .iter()
                .any(|c| c.starts_with("POST") && c.contains("/issues/11/labels")),
            "an approved pull request did not rejoin the train it was assigned to: {calls:?}"
        );

        crate::prq::forget_host_token();
        crate::prq::forget_trunks();
    }

    /// A stacked child somebody put the train on by hand is never merged into its parent, and no
    /// merge reaches the wire.
    ///
    /// The other half of the rule above, and the one that was missing (SKEIN-237). `matches` used
    /// to be read by [`crate::workflow::claims`] and by nothing else: [`carries`] returned
    /// `Carries::Assigned` straight from the assignment file, [`sweep`] took its name, and
    /// [`crate::workflow::next`] evaluates only `steps` — so on a documented merge train, whose
    /// `base:trunk` lives in `matches`, one hand assignment merged a child into its PARENT's
    /// branch and deleted the child's branch. Putting a workflow on a row by hand is an ordinary
    /// cockpit act; on an eighteen-deep stack it takes the rest of the stack with it, and there is
    /// no undo for a landed merge and a deleted branch.
    ///
    /// **What refuses it moved earlier, and the test says which** (SKEIN-279). SKEIN-237 could
    /// only refuse this at the act, because the train had already claimed the pull request and
    /// walked its steps: the merge became a `flag`, and a stop somebody had to clear. Now the
    /// train's own `base:trunk` holds on both roads, so the pull request is never carried for
    /// acting at all — no step, no stop, nothing written down, and it rejoins by itself when its
    /// parent merges and GitHub retargets it. The act-level guard has NOT gone anywhere and is
    /// still what catches a workflow whose `matches` never mentioned the base: it is pinned by
    /// `workflow::tests::a_merge_is_refused_on_a_base_that_is_not_known_to_be_the_trunk`, on
    /// [`crate::workflow::next`] directly.
    ///
    /// Driven through [`sweep`] against a GitHub that records every request, because the assertion
    /// that matters is about the wire: a doer tested through its return value would pass while
    /// merging. The workflow is the train exactly as `docs/pr-workflow.md` writes it down.
    #[test]
    fn a_hand_assigned_stacked_child_is_never_merged_into_its_parent() {
        let _g = crate::testutil::env_lock();
        let _h = crate::github::HoldClear::new();
        let home = crate::testutil::tempdir();
        let home = home.as_ref() as &std::path::Path;
        let mut env = crate::testutil::env_pins();
        env.set("SKEIN_HOME", home);
        crate::testutil::switch_on(|c| c.pr_workflows = true);
        env.set("GH_TOKEN", "skein-test-gho");
        std::env::remove_var("GITHUB_TOKEN");
        crate::prq::forget_host_token();
        crate::prq::forget_trunks();

        // The train from docs/pr-workflow.md, "The train, written down".
        std::fs::write(
            home.join("workflows.json"),
            br#"{"workflow":[{"name":"merge-train","serial":true,
              "matches":["ready","approved","review-satisfied","base:trunk"],
              "steps":[
                {"when":["changes-requested"],"do":"flag:changes were requested"},
                {"when":["not-mergeable"],"do":"flag:conflicts with the base"},
                {"when":["behind"],"do":"update-branch:rebase"},
                {"when":["checks:failing"],"do":"flag:CI failed"},
                {"when":["no-label:ci-queue"],"do":"add-label:ci-queue"},
                {"when":["label:ci-queue","checks:pending"],"do":"wait:CI is running"},
                {"when":["label:ci-queue","checks:passing","mergeable","current"],
                 "do":"merge:squash+delete"},
                {"when":[],"do":"wait:waiting for GitHub to catch up"}]}]}"#,
        )
        .unwrap();
        std::fs::write(
            home.join("repos.json"),
            br#"[{"id":"demo","source":"https://github.com/acme/thing.git","source_tree":"","store":""}]"#,
        )
        .unwrap();

        // #12 is a stacked child: based on `feat-parent`, and otherwise in the exact state that
        // makes the train's last real step fire — approved, ready, labelled, green, CLEAN.
        let heard: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
        let seen = heard.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let mut stream = stream;
                let mut buf = [0u8; 16384];
                let n = stream.read(&mut buf).unwrap_or(0);
                let said = String::from_utf8_lossy(&buf[..n]).to_string();
                let head = said.lines().next().unwrap_or_default().to_string();
                seen.lock().unwrap().push(head.clone());
                let answer = if head.contains("/user") {
                    r#"{"login":"me"}"#.to_string()
                } else if head.starts_with("GET /repos/acme/thing") && !head.contains("/pulls") {
                    r#"{"full_name":"acme/thing","default_branch":"main"}"#.to_string()
                } else if head.contains("/graphql") {
                    r#"{"data":{"q0":{"nodes":[{"number":12,"title":"t","url":"u",
                      "isDraft":false,"author":{"login":"me"},"headRefName":"feat-12",
                      "headRefOid":"abc","baseRefName":"feat-parent",
                      "updatedAt":"2026-08-23T00:00:00Z","reviewDecision":"APPROVED",
                      "mergeable":"MERGEABLE","mergeStateStatus":"CLEAN",
                      "labels":{"nodes":[{"name":"ci-queue"}]},
                      "latestReviews":{"nodes":[]},
                      "commits":{"nodes":[{"commit":{
                        "committedDate":"2026-08-23T00:00:00Z",
                        "statusCheckRollup":{"contexts":{"nodes":[
                          {"status":"COMPLETED","conclusion":"SUCCESS"}]}}}}]}}]},
                      "q1":{"nodes":[]},"q2":{"nodes":[]},"q3":{"nodes":[]}}}"#
                        .to_string()
                } else {
                    r#"{"merged":true}"#.to_string()
                };
                let _ = stream.write_all(
                    format!(
                        "HTTP/1.1 200 X\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{answer}",
                        answer.len()
                    )
                    .as_bytes(),
                );
            }
        });
        env.set("SKEIN_GITHUB_API", &base);

        // The rule refuses it — that is `a_stacked_child_is_kept_out_by_its_matches` above. A
        // person puts the train on it by hand, which is the road that skipped every guard.
        assign("demo", 12, "merge-train").unwrap();
        let did = sweep();

        let calls = heard.lock().unwrap().clone();
        assert!(
            !calls.iter().any(|c| c.contains("/pulls/12/merge")),
            "the train merged a stacked child into feat-parent: {did:?} / {calls:?}"
        );
        assert!(
            !calls.iter().any(|c| c.starts_with("DELETE")),
            "a branch was deleted on a pull request that was never merged: {calls:?}"
        );
        // And it was refused BEFORE the train took it up, so there is nothing to clear: no stop
        // written down, and no journal entry, because nothing happened to record. A stop the
        // owner has to clear on a pull request that will retarget itself is work manufactured out
        // of a condition the workflow already stated.
        assert_eq!(
            stopped("demo", 12),
            None,
            "a stop was written for a pull request the train never took up: {did:?}"
        );
        let entries = journal("demo", 12);
        assert!(
            entries.is_empty(),
            "nothing acted, so nothing may be in the timeline: {entries:?}"
        );
        // The dry run is where this has to be visible, and it says both halves: the train is on
        // this pull request (somebody chose it, and the chooser must show it as chosen) and it is
        // not acting, naming the condition off the file. This is what a person reads with the
        // switch off, so silence here is the whole failure SKEIN-279 is about.
        let flows = crate::workflow::load().unwrap();
        let facts = facts_of(
            &crate::prq::queue(&crate::repos::load_repos()[0], false)
                .unwrap()
                .prs[0],
            "me",
            "main",
        );
        let seen = standing("demo", 12, &facts, &flows);
        assert_eq!(
            (seen.how.as_str(), seen.workflow.as_str(), seen.step),
            ("assigned", "merge-train", 0),
            "the row must still say the train is on it, with no step it is about to take: {seen:?}"
        );
        assert_eq!(
            seen.next, "",
            "the dry run promised a step on a pull request nothing will act on: {seen:?}"
        );
        assert!(
            seen.holding.contains("base:trunk"),
            "the dry run says nothing about why the train is not moving on it: {seen:?}"
        );

        crate::prq::forget_host_token();
        crate::prq::forget_trunks();
    }

    /// The journal keeps the timeline: an act, a flag, and the hand that cleared it, in order.
    ///
    /// The owner's words: *"I need to know exactly what is it working on, which step is it on,
    /// status of previous steps and so on."* The stops file answers only "why is it stopped now";
    /// this is the record of what already happened — written where the events happen, so a
    /// timeline read tomorrow says what the audit said today.
    #[test]
    fn the_journal_keeps_the_timeline_of_did_stopped_and_cleared() {
        let _g = crate::testutil::env_lock();
        // Pinned where nothing listens, because this reaches `warden_client`: it refuses a
        // test process that has not said which warden to ask rather than opening a connection
        // to whatever warden the machine running the suite can reach (SKEIN-762).
        let _warden = crate::testutil::no_warden();
        let home = crate::testutil::tempdir();
        let mut env = crate::testutil::env_pins();
        env.set("SKEIN_HOME", home.as_ref() as &std::path::Path);
        crate::testutil::switch_on(|c| c.pr_workflows = true);
        let (base, _heard) = github(200);
        env.set("SKEIN_GITHUB_API", &base);

        // An action that lands is a "did", carrying the flow, the 1-based step, and the sentence.
        let out = perform(
            &subject("abc"),
            &flow(),
            &chosen(Act::Merge(Merge {
                how: MergeAs::Squash,
                delete_branch: false,
            })),
            &fixture_token(),
        );
        assert!(matches!(out, Outcome::Did(_)), "{out:?}");
        let entries = journal("demo", 41);
        assert_eq!(entries.len(), 1, "{entries:?}");
        assert_eq!(
            (
                entries[0].kind.as_str(),
                entries[0].flow.as_str(),
                entries[0].step
            ),
            ("did", "ship-mine", 4),
            "the entry must name the flow and the 1-based step: {entries:?}"
        );
        assert!(entries[0].what.contains("merged #41"), "{entries:?}");
        assert!(entries[0].at_ms > 0, "no timestamp: {entries:?}");

        // A flag is a "stopped", with the workflow's own reason.
        let out = perform(
            &subject("abc"),
            &flow(),
            &chosen(Act::Flag("CI is red".into())),
            &fixture_token(),
        );
        assert!(matches!(out, Outcome::Stopped(_)), "{out:?}");

        // And a person clearing the stop is an event too — flow-less, step-less, but on record.
        clear("demo", 41).expect("the stop must clear");

        let entries = journal("demo", 41);
        let kinds: Vec<&str> = entries.iter().map(|e| e.kind.as_str()).collect();
        assert_eq!(
            kinds,
            vec!["did", "stopped", "cleared"],
            "the timeline is not in the order things happened: {entries:?}"
        );
        assert_eq!(entries[1].what, "CI is red");
        assert_eq!(entries[1].step, 4, "a flag is a step and must say which");
        assert_eq!(
            (entries[2].flow.as_str(), entries[2].step),
            ("", 0),
            "a clear is nobody's step: {entries:?}"
        );

        // The other reader carries the same timelines, keyed numerically.
        let all = journals("demo");
        assert_eq!(all.keys().copied().collect::<Vec<_>>(), vec![41]);
        assert_eq!(all[&41].len(), 3);
    }

    /// A failed action journals the same "stopped" it writes to the stops file.
    #[test]
    fn a_failed_action_reaches_the_journal_as_stopped() {
        let _g = crate::testutil::env_lock();
        // Pinned where nothing listens, because this reaches `warden_client`: it refuses a
        // test process that has not said which warden to ask rather than opening a connection
        // to whatever warden the machine running the suite can reach (SKEIN-762).
        let _warden = crate::testutil::no_warden();
        let home = crate::testutil::tempdir();
        let mut env = crate::testutil::env_pins();
        env.set("SKEIN_HOME", home.as_ref() as &std::path::Path);
        crate::testutil::switch_on(|c| c.pr_workflows = true);
        let (base, _heard) = github(409);
        env.set("SKEIN_GITHUB_API", &base);

        let out = perform(
            &subject("abc"),
            &flow(),
            &chosen(Act::Merge(Merge {
                how: MergeAs::Squash,
                delete_branch: false,
            })),
            &fixture_token(),
        );
        assert!(matches!(out, Outcome::Stopped(_)), "{out:?}");
        let entries = journal("demo", 41);
        assert_eq!(entries.len(), 1, "{entries:?}");
        assert_eq!(entries[0].kind, "stopped");
        assert!(
            entries[0].what.contains("ship-mine") && entries[0].what.contains("could not be done"),
            "the journal must keep the failure's own sentence: {entries:?}"
        );
    }

    /// Each pull request keeps its newest fifty entries, and the oldest fall off.
    #[test]
    fn the_journal_caps_each_pull_request_at_its_newest_fifty() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        std::env::set_var("SKEIN_HOME", home.as_ref() as &std::path::Path);

        for i in 1..=55 {
            record("demo", 7, "train", 1, "did", &format!("event {i}"));
        }
        let entries = journal("demo", 7);
        assert_eq!(entries.len(), 50, "the cap did not hold");
        assert_eq!(
            entries[0].what, "event 6",
            "the OLDEST must fall off, not the newest"
        );
        assert_eq!(entries[49].what, "event 55");

        // Another pull request's timeline is untouched by #7's churn.
        record("demo", 9, "train", 1, "did", "only one");
        assert_eq!(journal("demo", 9).len(), 1);
        assert_eq!(journal("demo", 7).len(), 50);

        std::env::remove_var("SKEIN_HOME");
    }

    /// A corrupt journal reads as empty, never as an error — history lost is not action stopped.
    #[test]
    fn a_corrupt_journal_reads_as_empty() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        std::env::set_var("SKEIN_HOME", home.as_ref() as &std::path::Path);

        std::fs::create_dir_all(crate::prq::review_dir("demo")).unwrap();
        std::fs::write(journal_path("demo"), b"{ this is not json").unwrap();
        assert!(journal("demo", 7).is_empty());
        assert!(journals("demo").is_empty());
        // And the next write recovers rather than failing forever on the bad file. This is the
        // deliberate half of SKEIN-359: every other unreadable file in skein is refused, and this
        // one is written over, because refusing here would end journalling for the repo for good —
        // nothing repairs the file, and `record` is best-effort, so no caller would ever be told.
        record("demo", 7, "train", 1, "did", "back on the rails");
        assert_eq!(
            journal("demo", 7).len(),
            1,
            "the journal stopped recording after one unreadable file — a timeline that gives up \
             permanently is a worse answer than one that starts again, which is the whole reason \
             this file is written over rather than refused"
        );

        std::env::remove_var("SKEIN_HOME");
    }

    /// The banner names only stops a person can reach — and the panel cannot name one the banner
    /// has never heard of (SKEIN-241).
    ///
    /// The reported failure, in order: the train stops on a pull request, a human merges it on
    /// GitHub, it leaves the queue, and the orange block row above the whole application goes on
    /// naming it. Clicking it opens a queue without it; both "let it run again" buttons are built
    /// from the live queue, so there is no way to dismiss it at all.
    ///
    /// The second half of the same defect is asserted here rather than in a second test on
    /// purpose: the two surfaces were two computations over two different sets, and what makes
    /// that a defect is only visible by comparing them. `trains` is the panel's set and `stops` is
    /// the banner's, and the departed pull request must be in neither.
    #[test]
    fn a_stop_on_a_pull_request_that_has_left_the_queue_is_not_shouted_about() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        std::env::set_var("SKEIN_HOME", home.as_ref() as &std::path::Path);

        let flows = crate::workflow::from_bytes(
            br#"{"workflow":[{"name":"train","serial":true,"steps":[{"when":[],"do":"merge:squash"}]}]}"#,
        )
        .unwrap();
        stop("gone", 123, "CI is red");
        stop("gone", 124, "conflicts");
        // #123 was merged by hand and has left the queue. #124 is still there.
        remember_open("gone", &[124], true);

        let banner = stops("gone");
        assert_eq!(
            banner.iter().map(|s| s.number).collect::<Vec<_>>(),
            vec![124],
            "the banner still names a pull request that is not in the queue, and no row in the \
             cockpit can clear it: {banner:?}"
        );
        // The file keeps it. The banner is a demand for attention; the file is skein's memory of a
        // refusal, and forgetting that is how an action that failed gets attempted again.
        assert_eq!(
            stopped("gone", 123).as_deref(),
            Some("CI is red"),
            "the stop itself was deleted — the workflow may now re-attempt what it refused"
        );

        // The panel, from the same queue. Nothing it names may be missing from the banner, which
        // is the drift the two-computations defect was.
        let carrying = vec![(124u64, "train".to_string())];
        let panel = trains("gone", &carrying, &flows);
        assert_eq!(panel.len(), 1);
        for skipped in &panel[0].stopped {
            assert!(
                banner.iter().any(|s| s.number == skipped.number),
                "the panel names #{} as stopped and the banner does not — the two surfaces are \
                 reading different sets again",
                skipped.number
            );
        }
        assert!(
            !panel[0].line.contains(&123) && !banner.iter().any(|s| s.number == 123),
            "the merged pull request survives on one surface or the other"
        );

        std::env::remove_var("SKEIN_HOME");
    }

    /// A clear that could not be written says so, and puts nothing in the timeline (SKEIN-249).
    ///
    /// The reported failure: a person presses "let it run again", the stops file cannot be
    /// written, and all three of the things they can see say it worked — the button answers
    /// `{"ok": true}`, the journal gains a line saying the stop was cleared, and the stop is still
    /// on disk. The train never moves, and nothing anywhere disagrees with the story.
    ///
    /// This is a claim about ORDER, so it is asserted on the two things order decides: what came
    /// back, and what the timeline says. The journal is the sharper of the two — a `Result` nobody
    /// reads is a smaller lie than a record of an act that did not happen, because the record
    /// outlives the press and is what somebody debugging this reads a week later.
    ///
    /// **The DIRECTORY is what is made read-only, and it has to be** (SKEIN-414). This used to
    /// make the stops file itself mode 0400 — the report's own repro, and the right one while the
    /// write was `std::fs::write`, which opens the existing path for truncation and so needs write
    /// permission on the file and none on the directory. The write is now
    /// `util::write_atomic`: a temp file in the same directory, then a rename over the path. A
    /// rename does not open the target at all, so it succeeds on a read-only file and the old setup
    /// stops provoking anything. What it needs is a writable directory, so that is what is taken
    /// away — the temp cannot be created, the rename never happens, and the stop is untouched.
    /// Mode 0500 rather than 0400 because the lock beside the file still has to be opened, and the
    /// test would otherwise be measuring the traverse rather than the write.
    ///
    /// The guarantee is unchanged and so is everything asserted below; only the way a failed write
    /// is arranged moved. It still stands in for the two causes nobody can arrange in a test: a
    /// read-only host state directory, and a full disk.
    #[test]
    fn a_clear_that_could_not_be_written_reports_it_and_journals_nothing() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        std::env::set_var("SKEIN_HOME", home.as_ref() as &std::path::Path);

        stop("demo", 41, "CI is red");
        assert_eq!(stopped("demo", 41).as_deref(), Some("CI is red"));

        let dir = crate::prq::review_dir("demo");
        let mode = |bits: u32| {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(bits)).unwrap();
        };
        mode(0o500);
        let refused = clear("demo", 41);
        mode(0o700);

        let why = refused.expect_err(
            "a clear that never reached disk answered the button `ok` — the person is told the \
             workflow may act again, and it is still stopped",
        );
        assert!(
            why.contains("#41") && why.contains("still stopped"),
            "the refusal does not say which pull request is still stopped: {why}"
        );
        assert_eq!(
            stopped("demo", 41).as_deref(),
            Some("CI is red"),
            "the stop is gone from disk after a write that failed"
        );
        assert!(
            !journal("demo", 41).iter().any(|e| e.kind == "cleared"),
            "the timeline says a person lifted this stop, and nobody did: {:?}",
            journal("demo", 41)
        );

        // And the same press through the route's own entry point, which is what the cockpit calls
        // — `apply` must not answer Ok for a clear that did not happen.
        mode(0o500);
        let refused = apply("demo", 41, None, false, true);
        mode(0o700);
        assert!(
            refused.is_err(),
            "apply swallowed the failure, so the row reports success"
        );

        // Cleared for real: the write lands, the timeline gains its line, and it is idempotent —
        // a second press has nothing to do and is not a failure.
        clear("demo", 41).expect("a writable stops file must clear");
        assert_eq!(stopped("demo", 41), None);
        assert_eq!(
            journal("demo", 41)
                .iter()
                .filter(|e| e.kind == "cleared")
                .count(),
            1,
            "the clear that DID happen is missing from the timeline, or is in it twice"
        );
        clear("demo", 41).expect("clearing a pull request with no stop is not a failure");

        std::env::remove_var("SKEIN_HOME");
    }

    /// **Two writers at once lose neither a stop, a choice, nor a journal entry** (SKEIN-414).
    ///
    /// All three of this module's files are a read-modify-write over a whole map, and until now
    /// none of them took a lock. Two of those interleaving is last-write-wins where the loser is a
    /// whole entry: a stop somebody's pull request depends on, a person's `Excluded`, or a line of
    /// the timeline. The two writers are the real ones — the tick sweeps every repo on its own
    /// thread while the cockpit's routes call `assign`, `stop` and `clear` from request threads —
    /// and threads here stand in for that, at the only rate that makes a microsecond-wide window
    /// reproducible.
    ///
    /// The journal entries all land on ONE pull request on purpose: the other two files lose an
    /// entry when two writers pick different keys, and the journal loses one when they pick the
    /// same key and both append. Below the fifty-entry cap, so what is asserted is the loss and not
    /// the trim.
    #[test]
    fn two_writers_at_once_lose_neither_a_stop_a_choice_nor_a_journal_entry() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        std::env::set_var("SKEIN_HOME", home.as_ref() as &std::path::Path);

        let (writers, each) = (8u64, 6u64);
        let hands: Vec<_> = (0..writers)
            .map(|w| {
                std::thread::spawn(move || {
                    for i in 0..each {
                        let number = w * each + i;
                        assign("race", number, "train").expect("assigned");
                        stop("race", number, "CI is red");
                        record("race", 999, "train", 1, "did", "merged it");
                    }
                })
            })
            .collect();
        for hand in hands {
            hand.join().expect("a writer panicked");
        }

        let total = (writers * each) as usize;
        assert_eq!(
            read_stops("race").len(),
            total,
            "a stop was lost to another writer, and the pull request it belonged to will be \
             attempted again"
        );
        assert_eq!(
            read_assigned("race").len(),
            total,
            "a person's choice about what may touch a pull request was lost to another writer"
        );
        assert_eq!(
            journal("race", 999).len(),
            total,
            "a journal entry was lost to another writer, and a timeline cannot show what is \
             missing from it"
        );

        std::env::remove_var("SKEIN_HOME");
    }

    /// **A stop file is replaced whole, never truncated and refilled** (SKEIN-414).
    ///
    /// `std::fs::write` truncates the file and then writes it, so a crash, a kill or a full disk in
    /// that window leaves a half-written file — and a half-written file is exactly the unreadable
    /// input SKEIN-359 spent its length teaching this module to refuse. Refusing is the reading end;
    /// this is the end that manufactures it. `util::write_atomic` writes a temp beside the file and
    /// renames over it, so a reader sees the old whole file or the new one.
    ///
    /// Asserted on the inode, because that is what tells the two mechanisms apart from the outside:
    /// a truncate-and-rewrite keeps it, a rename replaces it. The temp is checked for too — one
    /// left behind is a file nothing will rename into place and nothing will clean up.
    #[test]
    fn a_stop_is_written_by_replacing_the_file_rather_than_truncating_it() {
        use std::os::unix::fs::MetadataExt;
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        std::env::set_var("SKEIN_HOME", home.as_ref() as &std::path::Path);

        stop("demo", 41, "CI is red");
        let path = stops_path("demo");
        let before = std::fs::metadata(&path).expect("the stops file").ino();
        stop("demo", 42, "conflicts");
        let after = std::fs::metadata(&path).expect("the stops file").ino();

        assert_ne!(
            before, after,
            "the stops file was rewritten in place — a crash mid-write leaves half a file, and \
             everything that reads it is then refused"
        );
        assert_eq!(stopped("demo", 41).as_deref(), Some("CI is red"));
        assert_eq!(stopped("demo", 42).as_deref(), Some("conflicts"));

        let strays: Vec<String> = std::fs::read_dir(crate::prq::review_dir("demo"))
            .expect("the review dir")
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|name| name.starts_with(".skein.tmp."))
            .collect();
        assert!(
            strays.is_empty(),
            "a temp file was left beside the stops: {strays:?}"
        );

        std::env::remove_var("SKEIN_HOME");
    }

    /// A queue that could not see everything silences nothing.
    ///
    /// The direction this must fail in. A refresh that went dark produces the same empty `prs`
    /// list as a repo with nothing open, and reading that as "every stop is dismissible" would
    /// take every banner in the fleet down for one rate-limit window — SKEIN-229's failure through
    /// a different file. Both the partial queue and the repo skein has never read must show
    /// everything the file holds.
    #[test]
    fn a_queue_that_did_not_see_everything_hides_no_stop() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        std::env::set_var("SKEIN_HOME", home.as_ref() as &std::path::Path);

        stop("blind", 7, "CI is red");
        stop("blind", 9, "conflicts");

        // Never read at all — the cold start. Absence is evidence about skein, not about #7.
        assert_eq!(
            stops("blind").iter().map(|s| s.number).collect::<Vec<_>>(),
            vec![7, 9],
            "a repo with no queue on this machine had its stops hidden"
        );

        // Read, and cut off at its page: every pull request past the page is absent for a reason
        // that has nothing to do with it.
        remember_open("blind", &[], false);
        assert_eq!(
            stops("blind").iter().map(|s| s.number).collect::<Vec<_>>(),
            vec![7, 9],
            "a partial queue was read as proof that nothing is open"
        );

        // And the whole answer, which is allowed to be empty: it answered.
        remember_open("blind", &[], true);
        assert!(
            stops("blind").is_empty(),
            "a queue that saw everything and found nothing open still shouted about its stops"
        );

        std::env::remove_var("SKEIN_HOME");
    }

    /// **A person's choice about which pull request a workflow may touch survives a file skein
    /// cannot read** (SKEIN-359).
    ///
    /// `assign` is a read-modify-write over every assignment in the repo. Read through
    /// `read_to_string(..).ok()`, an unparseable `workflow-assigned.json` read as *no assignments*,
    /// so putting a workflow on one pull request wrote that single entry over all the others. The
    /// entry that matters most is the empty one — `Excluded`, a person saying no rule may claim
    /// this pull request — and losing it does not leave that pull request idle: it hands it back to
    /// the sweep, which then acts on the one somebody took out of its reach.
    #[test]
    fn an_assignment_file_skein_cannot_read_is_never_written_over() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        std::env::set_var("SKEIN_HOME", home.as_ref() as &std::path::Path);

        assign("demo", 7, "train").unwrap();
        assign("demo", 9, "").unwrap(); // excluded by hand: the one nobody may override
        let path = assign_path("demo");

        for corrupt in [&b""[..], &b"{\"7\":\"train\""[..]] {
            std::fs::write(&path, corrupt).unwrap();
            let why = assign("demo", 11, "loose")
                .expect_err("assigning over an unreadable file reported success");
            assert!(
                why.contains("cannot read") && why.contains("workflow-assigned.json"),
                "the refusal has to say what skein declined to do and name the file somebody must \
                 go and look at, not just report a parse error: {why}"
            );
            assert!(
                unassign("demo", 7).is_err(),
                "clearing one choice is not how the others are cleared"
            );
            assert_eq!(
                std::fs::read(&path).unwrap(),
                corrupt,
                "the unreadable assignment file was written over"
            );
        }

        std::env::remove_var("SKEIN_HOME");
    }

    /// **A stop file skein cannot read is never replaced by one holding no stops at all**
    /// (SKEIN-359).
    ///
    /// A stop is the record that acting on a pull request went wrong and must not be tried again.
    /// Every stop in the repo is in one file, so writing one stop over a read that answered "no
    /// stops" lets every *other* stopped pull request move again — the re-attempt loop the rule
    /// exists to prevent, arriving for the whole repo at once. `clear` is the sharper half: read as
    /// empty it finds no stop, returns Ok, and tells a person their press worked while the file it
    /// would have written is the one that destroys the rest.
    #[test]
    fn a_stop_file_skein_cannot_read_is_never_written_over() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        std::env::set_var("SKEIN_HOME", home.as_ref() as &std::path::Path);

        stop("demo", 41, "CI is red");
        stop("demo", 42, "conflicts");
        let path = stops_path("demo");

        for corrupt in [&b""[..], &b"{\"41\":\"CI is red\""[..]] {
            std::fs::write(&path, corrupt).unwrap();

            stop("demo", 43, "a third thing");
            assert_eq!(
                std::fs::read(&path).unwrap(),
                corrupt,
                "a new stop was written over a stop file skein could not read"
            );

            let why = clear("demo", 41)
                .expect_err("a clear over an unreadable stop file answered the button `ok`");
            assert!(
                why.contains("#41") && why.contains("still stopped"),
                "the refusal has to say which pull request is still stopped: {why}"
            );
            assert_eq!(
                std::fs::read(&path).unwrap(),
                corrupt,
                "the unreadable stop file was written over by a clear"
            );
        }

        // And it recovers by itself once the file parses: nothing was destroyed in between.
        std::fs::write(&path, b"{\"41\":\"CI is red\",\"42\":\"conflicts\"}").unwrap();
        clear("demo", 41).unwrap();
        assert_eq!(stopped("demo", 42).as_deref(), Some("conflicts"));

        std::env::remove_var("SKEIN_HOME");
    }
}
