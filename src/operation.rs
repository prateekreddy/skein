//! An Operation: an idempotent intent, with a check that may say "I do not know" (§2.4).
//!
//! # Why this exists as a type at all
//!
//! §2.4 has been the design since the rewrite was written down, and the codebase has been *shaped*
//! like it for longer than that — `docs/inventory.md` counts twenty-one `ensure_*` functions and
//! observes that "skein is already written as idempotent ensures; the Operation primitive names
//! something the codebase does rather than importing a pattern". What it did not have was the two
//! qualifications that make the pattern safe rather than merely tidy, and both of them are here:
//!
//! **`unknown` may never drive a doer.** A binary check makes "the daemon is wedged" and "the fleet
//! is absent" indistinguishable, and a reconciler answers by creating a fleet that already exists.
//! [`Check`] is three-valued and [`Operation::may_drive`] refuses on the third.
//!
//! **`destructive` operations are never auto-driven**, even when a doer exists and the check is
//! unsatisfied. Re-running a destroy is not "running it once". The class is on the operation rather
//! than on the caller, so a new caller cannot acquire the permission by not knowing about it.
//!
//! # What is deliberately not here
//!
//! **A doer that says who, not a callback.** §2.4 makes the doer optional, and the second operation
//! expressed this way is what made the field earn its place: publishing the cockpit's port is
//! *idempotent* and still must never be driven, because the thing that could drive it does not
//! exist. Without the field [`Operation::may_drive`] answers "yes" for an act nothing can perform,
//! which is the same shape as driving on `unknown` — a caller asks permission, is granted it, and
//! then has to invent a performer. [`Doer`] names who may act; it does not carry a closure, because
//! the performing lives in `crate::warden_client::perform` where the approval and audit are.
//!
//! **No lease.** That is about an in-flight attempt, and `crate::attempt` already holds the
//! machinery for the one operation that has one (`ensure_fleet`'s create).
//!
//! **No registry and no reconciler.** An operation is built where it is asked about. A list of every
//! operation is what a reconciler would want, and there is no reconciler; a list nothing iterates is
//! a second place to keep in step.

/// A tri-state level signal: what the check found.
///
/// Each carries the sentence a person reads, because a check that cannot say what it saw is a
/// boolean with extra steps — and the whole reason for the third state is that its sentence is
/// different from the other two.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Check {
    /// The desired state holds. Nothing to do.
    Satisfied(String),
    /// It does not hold, and the check is sure of that.
    Unsatisfied(String),
    /// **The check could not be made.** Not "no": a wedged daemon, an unreadable path, a question
    /// that cannot be put from here. It may be reported and it may never drive anything.
    Unknown(String),
}

impl Check {
    /// The sentence, whichever state it is.
    pub fn detail(&self) -> &str {
        match self {
            Check::Satisfied(d) | Check::Unsatisfied(d) | Check::Unknown(d) => d,
        }
    }

    /// The word a surface renders. Deliberately the same three §2.4 names.
    pub fn word(&self) -> &'static str {
        match self {
            Check::Satisfied(_) => "satisfied",
            Check::Unsatisfied(_) => "unsatisfied",
            Check::Unknown(_) => "unknown",
        }
    }
}

/// **Who may perform this without a person typing it.**
///
/// One variant, and the list is closed for the same reason `warden_client::Act` is: a second doer
/// needs a reason, an approval surface and an audit trail, and making it a type is what forces
/// somebody to supply all three. `None` on the operation is not "we have not written it yet" — it
/// is a positive statement that nothing may act, which is the case for every act §9.4 keeps away
/// from automation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Doer {
    /// A warden with the matching capability, through `crate::warden_client::perform` — which is
    /// where the approval, the outcome store and the audit already are.
    Warden,
}

/// Whether performing this twice is the same as performing it once.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Class {
    /// Safe to re-run, and therefore safe to drive from a check.
    Idempotent,
    /// **Never auto-driven**, whatever the check says. A person decides each time.
    Destructive,
}

/// An idempotent intent: what should hold, whether it does, and the exact way to make it so.
#[derive(Debug, Clone)]
pub struct Operation {
    /// Stable across attempts, so a retry names the same operation rather than making a second one.
    ///
    /// Derived rather than minted — see `warden_client::operation_id`, which is where the property
    /// is implemented and asserted. A fresh id per attempt turns at-most-once into
    /// once-per-attempt, which is the exact failure the warden's outcome store exists to prevent.
    pub id: String,
    /// The declared state that should hold, in one sentence.
    pub desired: String,
    /// Whether it holds. Tri-state, and the third state may only be reported.
    pub check: Check,
    /// The exact commands, in order. **Always present, always printable** — that is §2.4's wording
    /// and it is what makes an operation with no doer still useful: a person runs the recipe.
    pub recipe: Vec<String>,
    /// Idempotent or destructive.
    pub class: Class,
    /// Who may perform it, or `None` when nothing may — see [`Doer`]. Optional in §2.4 and optional
    /// here, and the `None` is load-bearing rather than a gap.
    pub doer: Option<Doer>,
}

impl Operation {
    /// **May something perform this without asking a person?**
    ///
    /// `false` for a destructive operation whatever the check says, `false` for a check that came
    /// back `unknown` whatever the class, and `false` when there is no doer — an operation nothing
    /// can perform is not one a caller may be told to go ahead with. All three refusals are here
    /// rather than at each call site because a caller that has not read §2.4 is exactly the caller
    /// that would drive on `unknown`, or invent a performer for an act that deliberately has none.
    pub fn may_drive(&self) -> bool {
        self.doer.is_some()
            && matches!(self.class, Class::Idempotent)
            && matches!(self.check, Check::Unsatisfied(_))
    }

    /// The whole operation as a person reads it: what it wants, what is true, and what to run.
    pub fn render(&self) -> String {
        let mut out = format!(
            "{}\n  wanted: {}\n  now:    {} — {}\n",
            self.id,
            self.desired,
            self.check.word(),
            self.check.detail()
        );
        if matches!(self.class, Class::Destructive) {
            out.push_str("  this one is destructive, so skein never runs it for you\n");
        } else if self.doer.is_none() {
            // Said in a different sentence from the destructive one, because it is a different
            // reason and a person acts on it differently: a destroy is withheld from them, this is
            // simply theirs to run. Collapsing the two would tell somebody their `sbx ports` line
            // is dangerous, which it is not.
            out.push_str("  nothing can do this for you — it is yours to run\n");
        }
        for line in &self.recipe {
            out.push_str(&format!("    {line}\n"));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn op(check: Check, class: Class) -> Operation {
        with_doer(check, class, Some(Doer::Warden))
    }

    fn with_doer(check: Check, class: Class, doer: Option<Doer>) -> Operation {
        Operation {
            id: "move-volume-abc".into(),
            desired: "the volume is at /new".into(),
            check,
            recipe: vec!["mv /old /new".into()],
            class,
            doer,
        }
    }

    /// **`unknown` may never drive a doer**, and neither may a destructive operation.
    ///
    /// The two refusals are independent and both are asserted, because the tempting implementation
    /// is one condition: "drive when the check is not satisfied". That reading drives on `unknown`,
    /// which is §2.4's named failure — a reconciler answering "I could not tell whether the fleet
    /// is there" by creating one.
    ///
    /// **What makes this fail**: writing `!matches!(self.check, Check::Satisfied(_))`, which passes
    /// the unsatisfied case and every `unknown` with it; or dropping the class test, which lets a
    /// destroy be re-run by whatever noticed it was needed.
    #[test]
    fn nothing_is_driven_on_a_check_that_could_not_be_made_or_an_act_that_cannot_be_repeated() {
        let said = "the reason".to_string();
        assert!(op(Check::Unsatisfied(said.clone()), Class::Idempotent).may_drive());

        assert!(
            !op(Check::Unknown(said.clone()), Class::Idempotent).may_drive(),
            "a check that could not be made drove a doer, which is how a fleet gets created twice"
        );
        assert!(
            !op(Check::Satisfied(said.clone()), Class::Idempotent).may_drive(),
            "an operation that is already satisfied was performed again"
        );
        // And the class alone is enough, on the one check that would otherwise drive.
        assert!(
            !op(Check::Unsatisfied(said.clone()), Class::Destructive).may_drive(),
            "a destructive operation was auto-driven — re-running a destroy is not running it once"
        );
        assert!(!op(Check::Unknown(said), Class::Destructive).may_drive());
    }

    /// **An operation nothing can perform is never one a caller may go ahead with.**
    ///
    /// The case that made [`Doer`] a field rather than a comment: publishing the cockpit's port is
    /// idempotent, so the class does not withhold it, and on a host the check answers `unsatisfied`
    /// rather than `unknown`, so the check does not either. What withholds it is that no doer
    /// exists — `warden_client::Act::Publish` deliberately has none (§9.4: opening a hole and
    /// closing one are not the same act). Without this clause `may_drive` grants permission for an
    /// act, and the caller then has to invent a performer, which is `sbx` — the exact fallback
    /// `docs/delivery.md` says must not exist, "because that fallback would be taken on exactly the
    /// day something was wrong".
    ///
    /// **What makes this fail**: dropping `self.doer.is_some()` from `may_drive`.
    #[test]
    fn an_act_with_nobody_to_perform_it_is_not_granted_to_whoever_asked() {
        let unsatisfied = Check::Unsatisfied("no mapping forwards to :7878".to_string());
        assert!(
            with_doer(unsatisfied.clone(), Class::Idempotent, Some(Doer::Warden)).may_drive(),
            "the case this is contrasted with stopped driving, so the assertion below proves nothing"
        );
        assert!(
            !with_doer(unsatisfied, Class::Idempotent, None).may_drive(),
            "an operation with no doer was cleared to run, and the only way to obey that is to \
             invent a performer"
        );
    }

    /// A person reading a doer-less operation is told it is theirs to run, and not told it is
    /// dangerous — those are different sentences because they call for different things.
    #[test]
    fn an_operation_with_no_doer_says_so_without_calling_itself_destructive() {
        let said = with_doer(
            Check::Unsatisfied("no mapping forwards to :7878".into()),
            Class::Idempotent,
            None,
        )
        .render();
        assert!(said.contains("yours to run"), "{said}");
        assert!(
            !said.contains("destructive"),
            "an ordinary command was described as destructive: {said}"
        );
        // And an operation that a warden CAN do carries neither line.
        let driveable = op(Check::Unsatisfied("x".into()), Class::Idempotent).render();
        assert!(!driveable.contains("yours to run"), "{driveable}");
    }

    /// The recipe is always printable, and a destructive operation says so where it is read.
    #[test]
    fn what_a_person_reads_carries_the_recipe_and_the_warning() {
        let said = op(
            Check::Unsatisfied("it is at /old".into()),
            Class::Destructive,
        )
        .render();
        assert!(said.contains("move-volume-abc"), "{said}");
        assert!(said.contains("the volume is at /new"), "{said}");
        assert!(said.contains("unsatisfied — it is at /old"), "{said}");
        assert!(said.contains("mv /old /new"), "{said}");
        assert!(
            said.contains("skein never runs it for you"),
            "a destructive operation was rendered without saying so: {said}"
        );
        // And an idempotent one does not carry the warning, or the warning stops meaning anything.
        let ordinary = op(Check::Unsatisfied("x".into()), Class::Idempotent).render();
        assert!(!ordinary.contains("destructive"), "{ordinary}");
    }
}
