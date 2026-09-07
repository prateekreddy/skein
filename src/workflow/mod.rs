//! What a pull request should have happen to it, written down once and applied by skein.
//!
//! The owner's ask, verbatim: *"once the PR where I am an author is approved, then apply a specific
//! tag that enabled CI and then let the CI be completed, if not successful flag so that we can fix
//! it. If successful and mergeble merge automatically and delete the branch. If not mergable for say
//! if the base branch moved, then rebase without losing the approvals … then do the same."*
//!
//! # Guarded steps, not a script
//!
//! A workflow is an ordered list of **steps**, each a set of conditions on GitHub's current answer
//! and one action. It is re-evaluated from scratch every time the queue is read, and at most one
//! step fires per evaluation.
//!
//! That is not a stylistic choice. A script would have to survive a forty-minute CI run, a restarted
//! server, a rate limit and a closed laptop, and would have to keep its own place in the sequence. A
//! guarded step set keeps no place: **the state on GitHub is the program counter.** Crash anywhere
//! and the next poll resumes from wherever the pull request actually is, because that is the only
//! place the position was ever kept.
//!
//! One step per evaluation for the same reason. The moment an action lands, what skein believes is
//! one action out of date — the label is on but no check has been queued yet, so "checks passing" is
//! still true *from the previous run* — and a cascade would merge on it.
//!
//! # A closed set of actions
//!
//! Eleven actions, and no way to add a twelfth from a file. An open-ended "run this command" would
//! be a different feature with a different blast radius: every action here is one skein can
//! describe in the audit and a person can undo, and that property does not survive an escape
//! hatch. The set grew by five — [`Act::Read`], [`Act::PostFindings`], [`Act::PostChanges`],
//! [`Act::PostApproval`], [`Act::Audit`] — and grew the same way: named variants that take no
//! command, so a reviewer flow inherits the audit and the undo rather than a hole beside them.
//!
//! # The reviewer half posts, under the reader's own name
//!
//! `docs/pr-review.md` adds a second vocabulary over this same engine, and §15 put it in an order
//! that was not negotiable: *nothing can act until the thing that decides can be shown to be
//! right.* So the reviewer conditions and actions were defined, spelled, parsed and evaluated here
//! before anything could act on one — and then §15 steps 3, 4 and 5 all landed on 2026-08-31, and
//! four of the five now reach GitHub.
//!
//! **This heading said the opposite for six days.** It was written at 14:08 that day with the read
//! step; `post_verdict` landed at 16:44 and the audit at 20:24, and the sentence never caught up.
//! Worth naming rather than quietly fixing: a safety property asserted at the top of a module is
//! what a reader consults before deciding how carefully to read the rest, so a false one is worse
//! than none. What `crate::prwork::perform` actually does:
//!
//! * [`Act::Read`] reads at the head the step was decided about (`prwork::read_now`) — and **the
//!   reading session posts its own comment review**, with `gh`, from inside its own checkout,
//!   under the credential it was handed. It is told never to approve and never to request changes,
//!   and that is a rule in the prompt rather than a gate in the code.
//! * [`Act::PostChanges`] and [`Act::PostApproval`] submit the verdict from here
//!   (`prwork::post_verdict`). Four gates stand in front of them and none can be written in a
//!   workflow file: the repo's `auto_review` flags, §10's trigger set and author filter, the repo's
//!   `auto_review_ceiling`, and the sha the step was decided about. [`Act::PostApproval`] is
//!   additionally out of reach without [`Cond::ReadingWhole`] — see
//!   [`instead_of_approving_what_was_not_wholly_read`], which the evaluator applies whatever the
//!   file asked for.
//! * [`Act::Audit`] answers one owed check in that same session, and **that turn posts too**, as
//!   an addition to the review already on the pull request.
//! * [`Act::PostFindings`] is the one that refuses, and it is a vestige rather than a step nobody
//!   has built: the reading has already posted, so this would put one reading on the pull request
//!   twice in two voices. `post_findings_refuses_as_a_vestige_rather_than_as_something_unbuilt`
//!   holds the refusal to saying that.
//!
//! So **everything that leaves this fleet leaves under the reader's own credential**, and what
//! keeps that honest is the list of gates above — not anything being unwired.
//!
//! # What this module does NOT do
//!
//! Perform anything. It defines the vocabulary, reads it back off disk, and **decides**: [`next`]
//! picks at most one step from GitHub's current answer, and the two overrides beside it refuse
//! choices no workflow file may make. Acting is the doer's job (`crate::prwork::perform`),
//! deliberately in that order — the evaluator is pure and can be tested against every state in the
//! owner's example without a network, and nothing can act until the thing that decides can be
//! shown to be right.
//!
//! See `docs/pr-workflow.md`, which also carries GitHub's own answer on what rebasing does to an
//! approval — the fact that decides what the `update-branch` step may promise.

mod evaluate;
mod facts;
mod file;
mod trigger;
mod vocabulary;

// Every name this module had before it became a directory, at the path it had. The submodules are
// private and the re-exports are globs on purpose: the split is a change of where the text lives,
// not of what anything outside can reach, and a hand-written list of names is a second place for
// the two to disagree. `crate::workflow::Cond` still resolves, and so does every other item.
pub use evaluate::*;
pub use facts::*;
pub use file::*;
pub use trigger::*;
pub use vocabulary::*;
