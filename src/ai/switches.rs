//! Whether skein may use the model at all, and what wants it — each switch named.

use super::*;

/// skein runs *inside* an `sbx run` box where `claude` is logged in on the subscription, so every AI
/// call rides the SAME rate-limit window as the fleet doing the real work. AI is therefore OFF unless
/// you opt in with the Settings switch, and even then it is lazy (on demand only), cached per turn-end, and
/// never a per-tick fleet sweep. The governing rule: AI may only *add* scrutiny, never remove it.
pub fn ai_enabled() -> bool {
    // The toggle in Settings → Boxes decides, so the feature is discoverable rather than folklore.
    // `$SKEIN_AI=off` can hold it off; nothing in the environment can switch it on
    // (`config::env_holds_off`).
    load_config().ai_enrichment && !crate::config::env_holds_off("SKEIN_AI")
}

/// Whether skein may read a pull request you have already opened a queue to look at.
///
/// A **second** switch, defaulting the opposite way to [`ai_enabled`], and they are separate on
/// purpose: one gates background enrichment that runs whether or not you asked for it, the other
/// gates work you asked for by opening the queue.
///
/// It lives here rather than in `review` because `health` has to be able to ask, and `health` does
/// not depend on `review`. Two copies of the rule would have been the alternative, and the bug this
/// was found by is what two views of the same question costs: the health check asked only about
/// [`ai_enabled`], reported "off", and said nothing at all about a fleet whose summaries were
/// switched ON and failing on every pull request.
pub fn summaries_enabled() -> bool {
    load_config().review_summaries && !crate::config::env_holds_off("SKEIN_REVIEW_AI")
}

/// What, if anything, wants the model — named, so a report can say which switch it is talking about.
pub fn model_wanted() -> Vec<&'static str> {
    let mut wanted = Vec::new();
    if ai_enabled() {
        wanted.push("box summaries");
    }
    if summaries_enabled() {
        wanted.push("review summaries");
    }
    wanted
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The health report asks about both switches, not one of them.
    ///
    /// `review_summaries` defaults ON and `ai_enrichment` defaults off, so a report that consulted
    /// only the second said "off" on the common configuration — while every review summary on that
    /// fleet was failing. The one place somebody would look, saying the feature was not in use.
    #[test]
    fn what_wants_the_model_names_every_switch_that_does() {
        let _g = crate::testutil::env_lock();
        // Shared with every other test in this module, and a panic skips the cleanup at the end:
        // clear the remembered refusal on the way IN. Without it a sibling's failure makes this
        // one's stub never run, and only in a parallel run.
        forget_refusal();
        // No deployment override here: this reads switches and calls no model. Setting one would be
        // a variable nothing in this test depends on, left behind for whichever test ran next —
        // which is what it was, and what broke `board`'s cover test in a parallel run.
        let home = crate::testutil::tempdir();
        let mut env = crate::testutil::env_pins();
        env.set("SKEIN_HOME", &home)
            .unset("SKEIN_AI")
            .unset("SKEIN_REVIEW_AI");
        assert_eq!(
            model_wanted(),
            vec!["review summaries"],
            "review summaries are on by default and the report does not mention them — which is \
             exactly the fleet that reported every summary failing while health said `ai: off`"
        );

        crate::testutil::switch_on(|c| c.ai_enrichment = true);
        assert_eq!(model_wanted(), vec!["box summaries", "review summaries"]);

        env.set("SKEIN_REVIEW_AI", "off");
        assert_eq!(model_wanted(), vec!["box summaries"]);

        env.set("SKEIN_AI", "off");
        assert!(model_wanted().is_empty());
    }

    /// **An environment variable holds a switch off and never switches one on** (the owner,
    /// 2026-09-27). What would make it fail: `$SKEIN_AI=on` or `$SKEIN_REVIEW_AI=on` read as a yes
    /// again, so a switch the person turned off in Settings comes back on from the environment.
    #[test]
    fn a_yes_in_the_environment_does_not_switch_the_model_on() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        let mut env = crate::testutil::env_pins();
        env.set("SKEIN_HOME", &home);
        crate::testutil::switch_on(|c| {
            c.ai_enrichment = false;
            c.review_summaries = false;
        });
        env.set("SKEIN_AI", "on").set("SKEIN_REVIEW_AI", "on");
        assert!(
            !ai_enabled(),
            "$SKEIN_AI=on beat AI enrichment switched off in Settings"
        );
        assert!(
            !summaries_enabled(),
            "$SKEIN_REVIEW_AI=on beat Read pull requests switched off in Settings"
        );
    }
}
