//! What a pull request *means*, at the depth it deserves.
//!
//! The queue in [`crate::prq`] answers which PRs are yours. This answers the question you actually
//! open one for: what is being changed, and is it the kind of change you need to have an opinion
//! about. A bug fix gets a line. A change to how something behaves gets explained.
//!
//! # The rule this module must not break
//!
//! [`crate::ai`] states it: **AI may only add scrutiny, never remove it.** Every other AI feature in
//! skein obeys that easily, because they can only escalate. This one cannot — its whole purpose is
//! to tell you a PR is boring, and "boring" is a claim that removes attention.
//!
//! So the failure direction is fixed in the type: [`Depth::Unread`] is what you get when AI is off,
//! the diff could not be fetched, the model timed out, or its answer did not parse. A PR skein has
//! not actually read stays at full attention and says so. A summary can only ever lower depth by
//! **succeeding**, never by failing quietly.
//!
//! # Three stages, so the expensive one runs rarely
//!
//! 0. **Free.** Which changed paths you own per [`crate::codeowners`], and what
//!    [`crate::contracts`] can prove moved by reading the diff. No model, always available — one
//!    scopes the prompts that follow, the other can overrule their verdict.
//! 1. **Cheap.** One small-model pass: a line, and a verdict on whether this needs expanding.
//! 2. **Earned.** The fuller brief, when stage 1 asks for it *or* stage 0 found evidence. The
//!    scanner escalates and never clears, so the two stages cannot talk each other down.
//!
//! Everything is cached against `(number, head_sha)`. That key is not an optimisation: it is the
//! same fact that decides whether your review still counts in [`crate::prq`], so a PR that gains a
//! commit gets a fresh summary and a fresh place in your queue from one change of state.
//!
//! # Where the parts are
//!
//! One question per file, and the order below is the order a reading passes through them:
//!
//! | file | the question it answers |
//! |---|---|
//! | `summary.rs` | what a reading IS — [`Depth`], [`Summary`], [`Known`], [`Ownership`] |
//! | `cache.rs` | where a reading is filed under `(number, head_sha)`, and what is thrown away |
//! | `scope.rs` | which pull requests skein reads unasked, and in what order |
//! | `budget.rs` | how much a day of unasked reading may cost, and who is exempt ([`Trigger`]) |
//! | `visit.rs` | one reading, from the trigger to the summary on disk |
//! | `checkout.rs` | where the reading stands, and the further turns taken in its session |
//! | `asking.rs` | what the model is shown, and what is believed of what it says back |
//!
//! The public surface is re-exported below and is the whole of it: `crate::review::X` for every `X`
//! there, and nothing reaches a file of this directory by name. That is what makes the boundary
//! checkable rather than a convention — the submodules are private, so a caller outside cannot
//! depend on which file something happens to live in today.

mod asking;
mod budget;
mod cache;
mod checkout;
mod scope;
mod summary;
#[cfg(test)]
mod testkit;
mod visit;

pub use asking::right_side_lines;
pub use budget::Trigger;
pub use cache::{cached, held, known, previous, prune};
pub use checkout::audit_owed;
pub use scope::read_waiting;
pub use summary::{known_at, ownership, summaries_enabled, Depth, Known, Ownership, Summary};
// The composed lost-box sentence, so `prwork::perform`'s test can assert against the text that
// ships rather than against a copy of it. See [`summary::summary_notice_for_test`].
#[cfg(test)]
pub(crate) use summary::summary_notice_for_test;
pub use visit::{
    announce_reading, ask, draft_comment, re_read_and_review, re_read_here_instead, readings,
    subscribe_readings, summarise, Composed, ReadingDone, ReadingNow,
};
