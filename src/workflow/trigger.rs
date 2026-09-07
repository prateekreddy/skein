//! §10's trigger set: which pull requests this repo's engine looks at at all.
//!
//! A gate in front of the workflow, not a step inside one. A workflow file says what the engine
//! does once it is looking; [`Wake`] says which pull requests it looks at, per repo, without
//! anybody editing a JSON file — and the two compose in the order §10 gives, the trigger set asked
//! BEFORE the step's own conditions.
//!
//! [`read_wake`] answers `None` for a word this build does not know, and that is the one rule
//! keeping a newer skein's vocabulary from reading as an empty set here:
//! `crate::prwork::no_trigger_of_this_repos_fired` reports the inert state from exactly that.

use super::{a_decision, Facts};
use serde::{Deserialize, Serialize};
// In scope for the doc links, not for the code. These items are in sibling files now, and
// rustdoc resolves an intra-doc link against what the file it appears in imports — so
// without this line every `[`Cond::Label`]` below silently stops being a link. Nothing gates
// `cargo doc`, which is exactly why the rot would be invisible.
#[allow(unused_imports)]
use super::Cond;

/// **Which event makes a pull request due a reading** — `docs/pr-review.md` §10's trigger set.
///
/// Named `Wake` rather than `Trigger` deliberately: `review::Trigger` already exists and answers a
/// different question — *did a person ask for this, or was it skein's own idea* — and the two
/// would meet in one function (`prwork::read_now` asks both). Two types called `Trigger` in one
/// call path is how the wrong one gets read.
///
/// **A repo names the ones it wants and the rest do not fire**, which is the third thing the owner
/// asked for: *"another flag where the trigger is just review requested state but not new commits
/// will auto trigger reviews."* The set is `Repo::auto_review_on`, and `requested` alone is its
/// default — the mode described in the ask, carried as THE default rather than as a special case.
///
/// **Not the same thing as a step's conditions, and not a second spelling of them.** A workflow
/// file says what the engine does once it is looking; this says which pull requests it looks at,
/// per repo, without anybody editing a JSON file. Both gates are real and they compose the way
/// §10's chain says: the trigger set is asked BEFORE the step's own conditions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Wake {
    /// GitHub is asking you by name.
    Requested,
    /// There are commits on a pull request you have never decided on.
    UnreviewedCommits,
    /// The head moved on one you asked changes on.
    BlockedCommits,
    /// **The head moved on one you approved** — the stale-approval hole this design exists to
    /// close, and the reason it is a trigger rather than a rule.
    ApprovedCommits,
    /// CI went red on one you approved.
    ApprovedCiRed,
    /// Somebody answered one of your findings.
    ///
    /// **Built, 2026-08-31**, and it was a field and a fetch rather than a mechanism — which is
    /// what the note that used to sit here predicted after reading `prq`'s query instead of
    /// remembering it. An earlier version of that note named the wrong missing thing: it said
    /// `prq::PrComment` has "no notion of which comment it answers", which is true and is not what
    /// this needs. The question was never *which* finding was answered.
    ///
    /// Two fields were missing and both are now asked for:
    ///
    /// * **`submittedAt` on `latestReviews`** — the query fetched `{ state, author, commit }`, so
    ///   skein could say what you said and which commit about, and not *when*. Without it there is
    ///   nothing for an answer to be newer than.
    /// * **`latest: comments(last: 1)` on `reviewThreads`** — it fetched `comments(first: 1)`, the
    ///   thread's OPENING comment, which is your own finding. An answer to it is the last comment,
    ///   and skein never saw it. Both ends are fetched now, under an alias, and the pair is the
    ///   whole rule: `author` is whose finding it is, `last_author` is who spoke last.
    ///
    /// [`crate::prq::Pr::replied_to`] holds that rule and this only reads its answer. It is
    /// three-valued, and only `Some(true)` fires: `None` is skein unable to tell — a truncated
    /// thread list, or no time for your own review — and a trigger that woke on it would spend a
    /// model call on a guess.
    ///
    /// **And it matters more than a missing convenience**, reported from a live board on
    /// 2026-08-31 (`docs/pr-review.md` §7d). In a stacked workflow the fix for a finding lands on a
    /// descendant branch, so the pull request's own head never moves — every head-derived trigger
    /// stays silent while five pull requests sit resolved. The failure is one-directional: never a
    /// false alarm, only a false calm, which is the direction nothing ever prompts you to re-check.
    Reply,
}

impl Wake {
    /// The word a repo's trigger set spells this with. The same string serde writes, and the same
    /// one `docs/pr-review.md` §10's table uses.
    pub fn spelled(self) -> &'static str {
        match self {
            Wake::Requested => "requested",
            Wake::UnreviewedCommits => "unreviewed-commits",
            Wake::BlockedCommits => "blocked-commits",
            Wake::ApprovedCommits => "approved-commits",
            Wake::ApprovedCiRed => "approved-ci-red",
            Wake::Reply => "reply",
        }
    }
}

// **`Wake::computable` is gone, and its absence is the record of what changed.**
//
// It existed for one variant: `reply` was in §10's table and no field in the queue could say it had
// fired, so a repo whose whole set was `["reply"]` sat switched on and inert. Both fields it needed
// are now asked for — `submittedAt` on `latestReviews`, and `latest: comments(last: 1)` on
// `reviewThreads` — so every trigger in the table is answerable and the method would return `true`
// for all six.
//
// A method that cannot return `false` is a guard that cannot fail, which is the shape this repo
// bans in tests and should not keep in production either. The rule it carried has not gone
// anywhere: `read_wake` answers `None` for a word this build does not know, and
// `prwork::no_trigger_of_this_repos_fired` reports exactly the same inert state from that. One
// rule, in one place, instead of two that could disagree.

/// Read a trigger word, or `None` for one this build does not know.
///
/// `None` rather than an error, and it lands in the same place an uncomputable trigger does: a word
/// from a newer skein is one this build cannot tell has fired, which is the same fact as
/// [`Wake::Reply`]'s. Both fail towards *not* reading, which is the direction a permission has to
/// fail in — see `repos::Ceiling`, which fails narrow for the same reason.
pub fn read_wake(word: &str) -> Option<Wake> {
    match word.trim() {
        "requested" => Some(Wake::Requested),
        "unreviewed-commits" => Some(Wake::UnreviewedCommits),
        "blocked-commits" => Some(Wake::BlockedCommits),
        "approved-commits" => Some(Wake::ApprovedCommits),
        "approved-ci-red" => Some(Wake::ApprovedCiRed),
        "reply" => Some(Wake::Reply),
        _ => None,
    }
}

/// **Which triggers have fired on this pull request.** Pure, like everything else here.
///
/// More than one can fire at once and all of them are returned — a pull request whose head moved
/// after you approved it, with CI red, is woken by two — because the caller's question is *"is any
/// of them in this repo's set"* and answering it from one arbitrarily chosen trigger would make
/// the answer depend on the order this function happens to test them in.
///
/// **Each one obeys `Facts`' own rule about what a short list can claim** (§7b). "You have never
/// decided" is a claim about the reviews that did NOT arrive, so [`Wake::UnreviewedCommits`]
/// requires [`Facts::reviews_whole`], exactly as [`Cond::Unreviewed`] does. A verdict that DID
/// arrive is still your verdict whatever the cap did, so the three that read one do not — the same
/// asymmetry as [`Cond::Label`] against [`Cond::NoLabel`].
pub fn woke(facts: &Facts) -> Vec<Wake> {
    let mut fired = Vec::new();
    // GitHub's own answer, carried rather than inferred, and a floor rather than a census: false
    // can mean "GitHub asked and skein could not see that it did" (`Facts::review_requested`).
    if facts.review_requested {
        fired.push(Wake::Requested);
    }
    if facts.reviews_whole && !a_decision(&facts.my_review) {
        fired.push(Wake::UnreviewedCommits);
    }
    // "The head moved" IS `!my_review_current`: your verdict was left against an older commit.
    // Read the other way round, a verdict that still stands is not a wake — there is nothing new
    // to look at, which is what makes these triggers and not conditions.
    if facts.my_review == "changes-requested" && !facts.my_review_current {
        fired.push(Wake::BlockedCommits);
    }
    if facts.my_review == "approved" && !facts.my_review_current {
        fired.push(Wake::ApprovedCommits);
    }
    if facts.my_review == "approved" && facts.checks == "failing" {
        fired.push(Wake::ApprovedCiRed);
    }
    // **Answerable now, and `computable` flipped in the same edit** — which the note that used to
    // sit here asked for. `Pr::replied_to` is the rule; this only reads its answer, and only
    // `Some(true)` fires. `None` is skein unable to tell (the thread list was cut, or it does not
    // know when you last spoke) and must not wake a reading it would then spend on.
    if facts.replied_to_me == Some(true) {
        fired.push(Wake::Reply);
    }
    fired
}
#[cfg(test)]
mod tests {
    // `super::*` for this file's own items, private ones included; `crate::workflow::*` for
    // the rest of the engine, which was one namespace before this module became a directory
    // and still is from outside. A test reads the vocabulary the way a caller does.
    use super::*;
    #[allow(unused_imports)]
    use crate::workflow::*;

    // ─────────────── §10's trigger set: which event wakes the reviewer ───────────────

    /// **Every trigger fires on its own event, and on nothing else.** Driven as a table so a new
    /// trigger cannot be added with a rule that also claims another one's event.
    ///
    /// **What would make this fail:** widening any arm of `woke` — writing
    /// `Wake::ApprovedCommits` as `my_review == "approved"` without `!my_review_current`, say.
    /// That pull request already appears in the `approved-ci-red` row with a standing review, and
    /// the exclusivity check below would catch it.
    #[test]
    fn each_trigger_fires_on_its_own_event_and_on_no_other() {
        let requested = Facts {
            review_requested: true,
            ..Default::default()
        };
        // Never decided, and skein saw the whole review list — both halves, see below.
        let unreviewed = Facts {
            reviews_whole: true,
            my_review: "none".into(),
            ..Default::default()
        };
        let blocked = Facts {
            my_review: "changes-requested".into(),
            my_review_current: false,
            ..Default::default()
        };
        let approved_moved = Facts {
            my_review: "approved".into(),
            my_review_current: false,
            ..Default::default()
        };
        let approved_red = Facts {
            my_review: "approved".into(),
            my_review_current: true,
            checks: "failing".into(),
            ..Default::default()
        };
        for (what, facts, want) in [
            ("requested", &requested, Wake::Requested),
            ("unreviewed", &unreviewed, Wake::UnreviewedCommits),
            ("blocked", &blocked, Wake::BlockedCommits),
            ("approved and moved", &approved_moved, Wake::ApprovedCommits),
            ("approved and red", &approved_red, Wake::ApprovedCiRed),
        ] {
            let fired = woke(facts);
            assert!(
                fired.contains(&want),
                "{what} did not wake {}: {fired:?}",
                want.spelled()
            );
            // `approved and moved` legitimately wakes nothing else here; `requested` and the rest
            // are one-event fixtures, so anything extra is an arm reaching past its own event.
            assert_eq!(
                fired.len(),
                1,
                "{what} woke more than the one trigger it is: {fired:?}"
            );
        }
    }

    /// **A pull request can wake more than one trigger, and all of them are reported.**
    ///
    /// The caller's question is "is any of these in the repo's set", and answering it from one
    /// arbitrarily chosen trigger would make the answer depend on the order `woke` tests them in.
    ///
    /// **What would make this fail:** returning early from `woke` after the first match.
    #[test]
    fn a_pull_request_that_wakes_two_triggers_reports_both() {
        // You approved it, then the head moved, and CI is red on the new one.
        let both = Facts {
            my_review: "approved".into(),
            my_review_current: false,
            checks: "failing".into(),
            ..Default::default()
        };
        let fired = woke(&both);
        assert!(
            fired.contains(&Wake::ApprovedCommits) && fired.contains(&Wake::ApprovedCiRed),
            "one of the two events this pull request is went unreported: {fired:?}"
        );
    }

    /// **§7b, applied to the trigger set.** "You have never decided" is a claim about the reviews
    /// that did NOT arrive, so it cannot be made from a list skein only partly saw — while a
    /// verdict that DID arrive is still your verdict whatever the cap did.
    ///
    /// The same asymmetry `Cond::Unreviewed` and `Cond::VerdictStanding` already obey, and the
    /// reason it matters here is the box this design was interviewed from: it read `--limit 60`
    /// against 64 open pull requests and took the missing rows for closed ones.
    ///
    /// **What would make this fail:** dropping `facts.reviews_whole` from the `UnreviewedCommits`
    /// arm — a truncated review list would then wake a reading on every pull request whose
    /// verdicts fell past the cap.
    #[test]
    fn only_the_trigger_that_claims_an_absence_needs_the_whole_review_list() {
        let blind = Facts {
            reviews_whole: false,
            my_review: "none".into(),
            ..Default::default()
        };
        assert!(
            !woke(&blind).contains(&Wake::UnreviewedCommits),
            "a truncated review list was read as 'you have never decided'"
        );

        // The other direction: a verdict skein DID see wakes its trigger on the same short list.
        let seen_on_a_short_list = Facts {
            reviews_whole: false,
            my_review: "changes-requested".into(),
            my_review_current: false,
            ..Default::default()
        };
        assert!(
            woke(&seen_on_a_short_list).contains(&Wake::BlockedCommits),
            "a verdict skein has in hand was discarded because the list was capped"
        );
    }

    /// **`reply` fires, and only on a sighting** — the row of §10's table that could not be
    /// computed at all until the queue was asked for two more fields.
    ///
    /// It is the one trigger whose fact is three-valued, and the two failing values are not the
    /// same thing: `Some(false)` is *skein saw every thread and nobody answered you*, `None` is
    /// *skein cannot tell* — the thread list was cut, or it does not know when you last spoke.
    /// Neither may wake a reading, because waking one spends money on a guess.
    ///
    /// **What would make this fail:** writing the arm as `!= Some(false)`, which fires on `None`
    /// and turns every truncated thread list into a reading nobody asked for; or dropping the arm,
    /// which puts `reply` back to never firing while §10's table still offers it.
    #[test]
    fn a_reply_wakes_a_reading_only_where_skein_actually_saw_one() {
        assert!(
            woke(&Facts {
                replied_to_me: Some(true),
                ..Default::default()
            })
            .contains(&Wake::Reply),
            "a reply skein saw did not wake anything"
        );
        for quiet in [None, Some(false)] {
            assert!(
                !woke(&Facts {
                    replied_to_me: quiet,
                    ..Default::default()
                })
                .contains(&Wake::Reply),
                "{quiet:?} woke a reading — only a sighting may"
            );
        }
        // And it does not ride along on the other five: a pull request woken by a moved head must
        // not also report a reply nobody left.
        assert!(!woke(&Facts {
            review_requested: true,
            reviews_whole: true,
            my_review: "approved".into(),
            my_review_current: false,
            checks: "failing".into(),
            ..Default::default()
        })
        .contains(&Wake::Reply));
    }

    /// **A word this build does not know is not a trigger** — which is the whole of the rule that
    /// `Wake::computable` used to carry a second copy of.
    ///
    /// `reply` is answerable now, so no word in §10's table is uncomputable and a `computable()`
    /// that returned `true` for all six would be a guard that cannot fail. What is left is the case
    /// that is still real: a trigger written by a NEWER skein, which this build cannot tell has
    /// fired. `read_wake` answers `None`, `prwork::no_trigger_of_this_repos_fired` drops it, and a
    /// set made only of such words reads as the inert state §10 says must present as off.
    ///
    /// **What would make this fail:** `read_wake` guessing — falling back to a default trigger for
    /// an unknown word, which would silently widen what a repo acts on.
    #[test]
    fn a_trigger_word_from_a_newer_skein_is_not_one_this_build_acts_on() {
        assert_eq!(read_wake("reply-with-a-quote"), None);
        assert_eq!(read_wake("on-a-tuesday"), None);
        // And every word §10's table does name reads back, or the test above passes by
        // `read_wake` answering `None` to everything.
        for wake in [
            Wake::Requested,
            Wake::UnreviewedCommits,
            Wake::BlockedCommits,
            Wake::ApprovedCommits,
            Wake::ApprovedCiRed,
            Wake::Reply,
        ] {
            assert_eq!(read_wake(wake.spelled()), Some(wake), "{}", wake.spelled());
        }
    }

    /// **`woke` reads five facts and no others** — the coupling `review::triggers_read_from` cannot
    /// state in the type system.
    ///
    /// That function fills `review_requested`, `reviews_whole`, `my_review`, `my_review_current`
    /// and `checks` off a `prq::Pr` and leaves every other field at its default, because `Facts` is
    /// `prwork`'s to build and `review` may not reach it. If an arm of [`woke`] ever reads a sixth
    /// field, that call site would answer from a default nobody looked anything up for — silently,
    /// and in the direction of a trigger that never fires.
    ///
    /// **What would make this fail:** adding `&& !facts.draft` to any arm, or reading `labels`, or
    /// `mine`, or `reading_whole`. The loud fixture below differs from the quiet one in every field
    /// a trigger does NOT read, so any new reach changes the answer and this catches it.
    #[test]
    fn only_the_five_facts_a_trigger_reads_can_change_what_woke_says() {
        // The five, set so that several triggers fire — an answer of `[]` would agree with
        // everything and prove nothing.
        let five = Facts {
            review_requested: true,
            reviews_whole: true,
            my_review: "approved".into(),
            my_review_current: false,
            checks: "failing".into(),
            ..Default::default()
        };
        assert!(
            woke(&five).len() >= 2,
            "the fixture must wake something, or this test agrees with anything"
        );
        // Everything else, moved off its default. If `woke` reaches for any of it, the answer moves.
        let loud = Facts {
            approved: true,
            changes_requested: true,
            review_requirement_met: Some(true),
            labels: vec!["ci-queue".into(), "hold".into()],
            labels_whole: true,
            mergeable: Some(true),
            draft: true,
            mine: true,
            behind: Some(true),
            base_is_trunk: Some(true),
            head_sha: "abc1234".into(),
            reading_sha: Some("abc1234".into()),
            reading_whole: Some(true),
            findings_blocking: Some(true),
            ..five.clone()
        };
        assert_eq!(
            woke(&five),
            woke(&loud),
            "a trigger read a fact outside the five `review::triggers_read_from` fills, so that \
             call site now answers from a default nobody looked anything up for"
        );
    }

    /// Every trigger's word round-trips, and a word from nowhere is `None` rather than a guess.
    ///
    /// **What would make this fail:** a `spelled()` arm that disagrees with `read_wake` — which is
    /// how a repo's stored set would silently stop matching what the engine computes, leaving a
    /// switched-on repo inert with nothing to say why.
    #[test]
    fn every_trigger_word_reads_back_as_the_trigger_it_spells() {
        for wake in [
            Wake::Requested,
            Wake::UnreviewedCommits,
            Wake::BlockedCommits,
            Wake::ApprovedCommits,
            Wake::ApprovedCiRed,
            Wake::Reply,
        ] {
            assert_eq!(
                read_wake(wake.spelled()),
                Some(wake),
                "{} did not read back",
                wake.spelled()
            );
            // The serde name and the spoken word are the same string, and a repo's set is stored
            // through serde — so a drift between them is a set that stops matching.
            assert_eq!(
                serde_json::to_value(wake).unwrap(),
                serde_json::Value::String(wake.spelled().into())
            );
        }
        assert_eq!(read_wake("on-a-tuesday"), None);
        assert_eq!(read_wake(""), None);
    }
}
