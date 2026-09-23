//! What one check answered — the three-valued `Level` and the `HealthCheck` it is carried in —
//! and the revision this binary was built from.

use super::*;

/// What a check answered. **Three states, and the third is the point.**
///
/// A binary check makes "the daemon is wedged" and "the fleet is absent" indistinguishable, and
/// anything that reconciles answers that ambiguity by doing the work again — creating a fleet that
/// already exists. The codebase already knew this in one place and said so:
/// [`crate::fleet::fleet_exists`] returns `Option<bool>` with exactly this comment. This is that
/// knowledge, everywhere a check is made.
///
/// The revision this binary was built from: `git describe --always --dirty`, stamped by build.rs.
///
/// This is the answer to "which build is serving?", and it exists because the question was
/// unanswerable twice at real cost: a restart mis-diagnosed as a stale fleet agent because nothing
/// could name the binary, and "is the fix deployed" settled only by grepping served HTML for marker
/// strings. `--dirty` is load-bearing — a binary from an edited tree is the other thing that looks
/// like a clean deploy and is not. "unknown" when git was absent at build time; never the package
/// version, which is 0.1.0 forever and answers a different question.
pub const BUILD_REVISION: &str = env!("SKEIN_BUILD_REVISION");

/// **`Unknown` may never drive a doer.** It may only be reported. Whatever would act on
/// `Unsatisfied` must do nothing at all on `Unknown` — the honest response to "I could not tell" is
/// to say so and wait, never to guess in the direction that happens to be cheap to write.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    /// Checked, and it holds.
    Satisfied,
    /// Checked, and it does not. This is the only state that is a fault.
    Unsatisfied,
    /// Could not be checked. Not a fault, and not a pass either.
    Unknown,
}

#[derive(Debug, Clone, Serialize)]
pub struct HealthCheck {
    pub level: Level,
    /// What is true. The diagnosis, and only the diagnosis.
    pub detail: String,
    /// **What would clear it** — architecture §2.4's `recipe`, and the reason the parent property
    /// holds at all: skein can only be blocked in a way it can explain if the check that found the
    /// block carries the way out with it.
    ///
    /// A command where there is one, so it can be copied rather than transcribed. Prose where the
    /// answer is a place in the UI rather than a command, because "Settings → Fleet → memory" is
    /// the honest recipe for a setting and inventing a CLI for it would not be.
    ///
    /// Empty for a satisfied or unknown check — there is nothing to fix, and nothing known to be
    /// wrong. **Never empty for an unsatisfied one**, which the tests enforce rather than trust.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub fix: String,
    /// Would running the fix destroy something? §2.4's `destructive` class.
    ///
    /// A destructive recipe is **printed and never run**. Nothing auto-drives one however
    /// unsatisfied its check is, because the cost of being wrong is not a wasted minute — it is a
    /// sandbox with every box's unpushed work on it.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub destructive: bool,
}

impl HealthCheck {
    pub fn satisfied(detail: impl Into<String>) -> HealthCheck {
        HealthCheck {
            level: Level::Satisfied,
            detail: detail.into(),
            fix: String::new(),
            destructive: false,
        }
    }

    /// A fault, and what would clear it. Both, always — the second argument exists so that a fault
    /// with no way out cannot be written without noticing.
    pub fn unsatisfied(detail: impl Into<String>, fix: impl Into<String>) -> HealthCheck {
        HealthCheck {
            level: Level::Unsatisfied,
            detail: detail.into(),
            fix: fix.into(),
            destructive: false,
        }
    }

    /// Could not be answered — `detail` says why it could not, not what is wrong.
    pub fn unknown(detail: impl Into<String>) -> HealthCheck {
        HealthCheck {
            level: Level::Unknown,
            detail: detail.into(),
            fix: String::new(),
            destructive: false,
        }
    }

    /// Mark the fix as one that destroys something, so nothing drives it.
    pub fn destroys(mut self) -> HealthCheck {
        self.destructive = true;
        self
    }

    /// Is this a fault? `Unknown` is not one — see [`Level`].
    pub fn is_fault(&self) -> bool {
        self.level == Level::Unsatisfied
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A running skein must say which build it is — with a revision, not a version number.
    ///
    /// The package version is 0.1.0 forever, so a `--version` or health field carrying it answers
    /// nothing; and "unknown" is the honest fallback for a build outside git, which this repo is
    /// not. Both mis-answers cost real time: a restart mis-diagnosed as a stale fleet agent, and
    /// "is the fix deployed" settled by grepping served HTML for marker strings. This test runs in
    /// a git checkout by construction, so a placeholder here means the stamp in build.rs broke.
    #[test]
    fn the_build_names_a_real_revision() {
        assert!(
            !BUILD_REVISION.trim().is_empty(),
            "the build stamp is empty — nothing skein serves can say which build it is"
        );
        assert_ne!(
            BUILD_REVISION, "unknown",
            "built inside a git checkout, yet the stamp is the no-git fallback"
        );
        assert_ne!(
            BUILD_REVISION,
            env!("CARGO_PKG_VERSION"),
            "the package version masquerading as a revision — it is 0.1.0 forever and identifies \
             nothing"
        );
    }

    /// The three states, and what each one is allowed to cause.
    ///
    /// `Unknown` is the whole point of the type: a check that could not be answered is not a pass
    /// and not a fault, and treating it as either is a bug with a name. As a fault it cries wolf —
    /// telling somebody `sbx` is broken because a listing timed out once sends them to reinstall a
    /// working tool. As a pass it is worse: whatever would have acted on `Unsatisfied` does nothing,
    /// silently, and the thing that was actually wrong is never reported.
    #[test]
    fn only_a_fault_is_a_fault() {
        assert!(HealthCheck::unsatisfied("x", "do y").is_fault());
        assert!(!HealthCheck::satisfied("x").is_fault());
        assert!(
            !HealthCheck::unknown("x").is_fault(),
            "a question skein could not put is not an answer it got"
        );
        // Only a fault carries a way out. A satisfied check has nothing to fix, and an unknown one
        // has nothing KNOWN to fix — offering a remedy for a question skein could not put is how a
        // diagnostic sends somebody to change a working setting.
        assert!(HealthCheck::satisfied("x").fix.is_empty());
        assert!(HealthCheck::unknown("x").fix.is_empty());
        assert_eq!(HealthCheck::unsatisfied("x", "do y").fix, "do y");
        // Destructive is off unless said, and saying it does not change the level: a destructive
        // fix is still the fix, it just may not be driven.
        let destructive = HealthCheck::unsatisfied("x", "do y").destroys();
        assert!(destructive.destructive && destructive.is_fault());
        assert!(!HealthCheck::unsatisfied("x", "do y").destructive);
    }

    /// The three states reach the cockpit under the names it renders.
    ///
    /// The page switches on this string. A rename here that the page does not follow shows every
    /// check as unknown, which is the one failure mode that looks like a working screen.
    #[test]
    fn the_wire_names_are_the_names_the_page_switches_on() {
        let page = include_str!("../web/index.html");
        for (level, name) in [
            (Level::Satisfied, "satisfied"),
            (Level::Unsatisfied, "unsatisfied"),
            (Level::Unknown, "unknown"),
        ] {
            let json = serde_json::to_string(&HealthCheck {
                level,
                detail: String::new(),
                fix: String::new(),
                destructive: false,
            })
            .unwrap();
            assert!(
                json.contains(&format!("\"level\":\"{name}\"")),
                "{level:?} does not serialise as {name}: {json}"
            );
            assert!(
                page.contains(&format!("{name}:")) || page.contains(&format!("\"{name}\"")),
                "the cockpit does not mention the `{name}` level at all"
            );
        }
    }
}
