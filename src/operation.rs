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
//! **No doer, and no lease.** §2.4 makes the doer optional and the lease is about an in-flight
//! attempt; the first operation expressed this way ([`crate::volume::move_to`]) is destructive, so
//! it is never driven and there is nothing in flight to lease. Adding either now would be inventing
//! a shape from one example that does not use it — and `crate::attempt` already holds the lease
//! machinery for the one operation that does (`ensure_fleet`'s create).
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
}

impl Operation {
    /// **May something perform this without asking a person?**
    ///
    /// `false` for a destructive operation whatever the check says, and `false` for a check that
    /// came back `unknown` whatever the class. Both refusals are §2.4's, and they are here rather
    /// than at each call site because a caller that has not read §2.4 is exactly the caller that
    /// would drive on `unknown`.
    pub fn may_drive(&self) -> bool {
        matches!(self.class, Class::Idempotent) && matches!(self.check, Check::Unsatisfied(_))
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
        Operation {
            id: "move-volume-abc".into(),
            desired: "the volume is at /new".into(),
            check,
            recipe: vec!["mv /old /new".into()],
            class,
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
