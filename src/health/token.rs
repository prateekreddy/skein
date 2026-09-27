//! A GitHub credential's deadline, said before it is one, with the step that renews it.

use super::*;

// ---------- a GitHub credential's deadline, before it is one (SKEIN-928) ----------

/// **How much notice the owner gets before a GitHub credential expires.**
///
/// Thirty days, and the number is chosen against what renewing actually costs rather than against a
/// round figure. Nothing inside the fleet can renew one of these: the token is regenerated on
/// github.com by the person whose account it is, and then re-set against the sandbox from the host.
/// So the window has to be long enough to survive the owner being away from the machine, which a
/// week is not — and short enough that the line is not permanently on the board, which a quarter
/// would be.
///
/// It is a fault rather than a note for the same reason `cover` is: an expiring credential has no
/// symptom at all until the day it has no symptoms left, and on that day every box loses GitHub at
/// once and it reads as an auth bug rather than as a date. If the banner does not say it, nothing
/// does.
pub const RENEW_WINDOW_DAYS: i64 = 30;

/// **What to do about this one**, by where it came from. Prose rather than one command for the
/// sources where a command would be a lie: a token stored in Settings is replaced in Settings, and
/// inventing a CLI for it would send somebody to a prompt that cannot help them.
///
/// The `$GH_TOKEN` arm is the fleet's own sandbox-scoped secret, and its recipe carries the two
/// halves that were learned the hard way (SKEIN-928): the permissions the replacement needs, so the
/// new token is not narrower than the one it replaces, and the fact that a fleet rebuild drops it
/// again, because `sbx rm` deletes a sandbox-scoped secret with the sandbox.
pub(crate) fn renew_recipe(source: crate::prq::GhToken, fleet: &str) -> String {
    use crate::prq::GhToken::*;
    match source {
        Environment => format!(
            "regenerate it on github.com under Settings → Developer settings → personal access \
             tokens, with the same repositories and Contents + Pull requests read/write, then on \
             your HOST run:  sbx secret set github --sandbox {fleet}   — and again after any fleet \
             rebuild, because `sbx rm` deletes a sandbox-scoped secret along with the sandbox"
        ),
        // Two places since the read token moved into "Your GitHub identity" (SKEIN-1179): each is
        // replaced where it was stored, and a recipe naming the other would send somebody to a
        // field that does not hold it.
        ReadToken => "regenerate it on github.com with the same repositories and permissions, \
                      then paste it over the old one under Settings → GitHub & keys → Your GitHub \
                      identity"
            .to_string(),
        WritePat => "regenerate it on github.com for the same repository and permissions, then \
                     paste it over the old one on the repository's card under Settings → \
                     Repositories, or under Settings → GitHub & keys → Repository tokens"
            .to_string(),
        GhCli => "run `gh auth login` again on the host that holds this login".to_string(),
        // Unreachable from `credential_lives`, which lists only credentials it found. Written out
        // rather than left to a catch-all so that adding a source to `GhToken` fails to compile
        // here instead of silently acquiring the wrong recipe.
        None => String::new(),
    }
}

/// Turn the readings into the line. **Pure**, so every sentence the owner sees — and the threshold
/// that decides whether they see one at all — is proven without a network or a credential.
///
/// The order of the three arms is the order of urgency, and it is the property worth stating: a
/// deadline inside the window is a fault, a reading skein could not take is an `unknown`, and only
/// when neither of those is true is this a pass. An `unknown` can therefore never hide a fault, and
/// a fault can never be downgraded by something else failing to answer.
pub(crate) fn token_expiry_line(lives: &[crate::prq::CredentialLife], fleet: &str) -> HealthCheck {
    use crate::prq::Life;
    if lives.is_empty() {
        return HealthCheck::satisfied(
            "no GitHub credential is stored, so there is nothing here to expire",
        );
    }
    // The nearest deadline decides the line: it is the one that will strand the fleet first, and a
    // report that leads with anything else buries it.
    let nearest = lives
        .iter()
        .filter_map(|c| match &c.life {
            Life::Expires { when, days } => Some((*days, when, c)),
            _ => None,
        })
        .min_by_key(|(days, _, _)| *days);
    if let Some((days, when, credential)) = nearest {
        if days <= RENEW_WINDOW_DAYS {
            let clock = match days {
                d if d < 0 => format!("expired {} day(s) ago, on {when}", -d),
                0 => format!("expires TODAY, on {when}"),
                d => format!("expires in {d} day(s), on {when}"),
            };
            return HealthCheck::unsatisfied(
                format!(
                    "{} {clock}. Nothing inside the fleet can renew it, and when it goes every \
                     box loses GitHub at once — API calls and `git push` alike, which reads as an \
                     auth bug rather than as a date",
                    credential.label
                ),
                renew_recipe(credential.source, fleet),
            );
        }
    }
    // Something could not be asked. Reported, never counted as a fault — and never as a pass
    // either, which is the arm that matters: GitHub answers a DEAD credential with a 401 and no
    // expiry header at all, so "skein could not tell" is exactly what the worst case looks like.
    if let Some(why) = lives.iter().find_map(|c| match &c.life {
        Life::Unanswered(why) => Some(format!("{}: {why}", c.label)),
        _ => None,
    }) {
        return HealthCheck::unknown(format!(
            "skein could not read an expiry for every credential it holds — {why}"
        ));
    }
    match nearest {
        Some((days, when, credential)) => HealthCheck::satisfied(format!(
            "the nearest deadline is {} in {days} day(s), on {when}",
            credential.label
        )),
        Option::None => HealthCheck::satisfied(format!(
            "{} credential(s), none of which GitHub gives an expiry date",
            lives.len()
        )),
    }
}

/// The line itself: ask GitHub about every credential skein holds, and say what it answered.
///
/// **Behind a gate, and the gate is not an optimisation.** `/api/health` is polled every fifteen
/// seconds by every open board, and this costs one HTTP request per stored credential — so without
/// one, a fleet with three tokens and four tabs open would spend a thousand requests an hour asking
/// a question whose answer changes once a day. **An hour, not the six that a date's own pace would
/// justify**, because the gate remembers an `unknown` exactly as readily as an answer: a moment's
/// unreachable GitHub would otherwise sit on the board saying so all afternoon.
///
/// **It does not reach out from a test.** Same rule and same reason as `github_reach_health`: a
/// test must not spend the box's shared api.github.com budget, and a unit test that depends on a
/// network is a test that fails for somebody else's reason. The sentences are proven through
/// [`token_expiry_line`], which needs neither.
pub fn token_expiry_health() -> HealthCheck {
    static GATE: crate::util::Gate<HealthCheck> = crate::util::Gate::new();
    if crate::util::in_test() {
        return HealthCheck::unknown("not asked from a test");
    }
    let fleet = crate::place::fleet_sandbox();
    GATE.get(std::time::Duration::from_secs(60 * 60), move || {
        Some(token_expiry_line(
            &crate::prq::credential_lives(chrono::Utc::now()),
            &fleet,
        ))
    })
    .unwrap_or_else(|| HealthCheck::unknown("skein has not been able to ask GitHub yet"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One credential reading, for the expiry tests below. Nothing here reaches GitHub — the whole
    /// point of [`token_expiry_line`] being pure is that the sentence and the threshold are proven
    /// without a network or a credential.
    fn reading(label: &str, life: crate::prq::Life) -> crate::prq::CredentialLife {
        crate::prq::CredentialLife {
            source: crate::prq::GhToken::Environment,
            label: label.to_string(),
            life,
        }
    }

    fn expires(label: &str, days: i64) -> crate::prq::CredentialLife {
        reading(
            label,
            crate::prq::Life::Expires {
                when: "2026-10-15 13:19:49 UTC".into(),
                days,
            },
        )
    }

    /// **The warning fires inside the window and is silent outside it** (SKEIN-928).
    ///
    /// This is the assertion the whole item is for: skein has to see the deadline coming with
    /// enough notice to be acted on, and it has to stay quiet the rest of the time, because a line
    /// that is always on the board is a line nobody reads on the day it matters.
    ///
    /// Counterfactuals, each one named before the assertion was written and each one proven by
    /// sabotage: narrowing `days <= RENEW_WINDOW_DAYS` to `days < 0` — warn only once it is already
    /// too late — makes `a deadline inside the window must be a fault` fail; widening it to
    /// `days <= RENEW_WINDOW_DAYS * 3` makes `a deadline outside the window must be silent` fail.
    /// The boundary day is asserted on purpose: an off-by-one there is a whole day of notice, and
    /// it is exactly the sort of thing that is never noticed from the outside.
    #[test]
    fn a_deadline_inside_the_window_is_a_fault_and_one_outside_it_is_silent() {
        let near = token_expiry_line(
            &[expires("$GH_TOKEN", RENEW_WINDOW_DAYS - 1)],
            "thing-fleet",
        );
        assert!(
            near.is_fault(),
            "a deadline inside the window must be a fault: {near:?}"
        );
        assert!(
            near.detail.contains("$GH_TOKEN") && near.detail.contains("2026-10-15 13:19:49 UTC"),
            "the fault must name the credential and the date: {}",
            near.detail
        );
        assert!(
            !near.fix.is_empty(),
            "an unsatisfied check must never have an empty fix"
        );

        let boundary = token_expiry_line(&[expires("$GH_TOKEN", RENEW_WINDOW_DAYS)], "thing-fleet");
        assert!(
            boundary.is_fault(),
            "the window is inclusive: {RENEW_WINDOW_DAYS} days out must still warn"
        );

        let far = token_expiry_line(
            &[expires("$GH_TOKEN", RENEW_WINDOW_DAYS + 1)],
            "thing-fleet",
        );
        assert_eq!(
            far.level,
            Level::Satisfied,
            "a deadline outside the window must be silent: {far:?}"
        );
        assert!(
            far.fix.is_empty(),
            "a satisfied check must carry no recipe, or the board shows a fix for nothing: {}",
            far.fix
        );
        assert!(
            !far.detail.contains("sbx secret set"),
            "nothing outside the window may print the renewal command: {}",
            far.detail
        );

        // Past the date entirely. Distinguished from "expires today" because they are different
        // sentences and only one of them is still a warning rather than a post-mortem.
        let gone = token_expiry_line(&[expires("$GH_TOKEN", -3)], "thing-fleet");
        assert!(
            gone.is_fault(),
            "an expired credential is a fault: {gone:?}"
        );
        assert!(
            gone.detail.contains("expired 3 day(s) ago"),
            "a credential already past its date must say so, not count down to it: {}",
            gone.detail
        );
    }

    /// **A reading skein could not take is never a pass, and never hides a fault** (SKEIN-928).
    ///
    /// GitHub answers a dead credential with a 401 and no expiry header at all, so "no header" is
    /// what the worst case looks like as well as the best. The three arms are ordered fault →
    /// unknown → pass, and both directions of that order are asserted here.
    ///
    /// Counterfactual: moving the `Unanswered` arm above the deadline arm makes
    /// `a fault outranks an unknown` fail; returning `satisfied` instead of `unknown` for an
    /// unanswered reading makes `an unanswered reading must never read as a pass` fail.
    #[test]
    fn an_unanswered_reading_is_never_a_pass_and_never_outranks_a_fault() {
        let unsure = token_expiry_line(
            &[reading(
                "$GH_TOKEN",
                crate::prq::Life::Unanswered("GitHub said 401: Bad credentials".into()),
            )],
            "thing-fleet",
        );
        assert_eq!(
            unsure.level,
            Level::Unknown,
            "an unanswered reading must never read as a pass: {unsure:?}"
        );
        assert!(
            unsure.detail.contains("Bad credentials"),
            "the reason skein could not tell is the only useful part of an unknown: {}",
            unsure.detail
        );

        let both = token_expiry_line(
            &[
                reading(
                    "the read token in Settings",
                    crate::prq::Life::Unanswered("GitHub did not answer within 20s".into()),
                ),
                expires("$GH_TOKEN", 2),
            ],
            "thing-fleet",
        );
        assert!(
            both.is_fault(),
            "a fault outranks an unknown — a credential skein could not ask about must not hide \
             one it could: {both:?}"
        );

        let endless = token_expiry_line(
            &[reading("$GH_TOKEN", crate::prq::Life::Endless)],
            "thing-fleet",
        );
        assert_eq!(
            endless.level,
            Level::Satisfied,
            "a token GitHub gives no expiry for is a supported state, not a fault: {endless:?}"
        );
        assert!(
            token_expiry_line(&[], "thing-fleet").level == Level::Satisfied,
            "holding no GitHub credential at all is not a fault either"
        );
    }

    /// **Each source gets the recipe that would actually replace it** (SKEIN-928).
    ///
    /// The fleet's own sandbox-scoped secret is renewed with a host command naming the sandbox, and
    /// a token stored in Settings is pasted back into Settings. One recipe for both would send
    /// somebody to the wrong place at the moment they are already blocked.
    ///
    /// Counterfactual: collapsing `renew_recipe` to a single string for every source makes
    /// `must not be told to run an sbx command` fail; dropping `{fleet}` from the environment arm
    /// makes `must name the sandbox` fail — and a recipe with a placeholder in it is the half of
    /// the answer nobody can copy.
    #[test]
    fn the_recipe_names_the_step_that_replaces_this_credential() {
        use crate::prq::GhToken;
        let fleet = renew_recipe(GhToken::Environment, "thing-fleet");
        assert!(
            fleet.contains("sbx secret set github --sandbox thing-fleet"),
            "the fleet secret's recipe must name the sandbox, copyable: {fleet}"
        );
        assert!(
            fleet.contains("Contents") && fleet.contains("Pull requests"),
            "a replacement narrower than what it replaces breaks pushes a week later: {fleet}"
        );
        assert!(
            fleet.contains("sbx rm"),
            "a fleet rebuild drops a sandbox-scoped secret, and that is the half people are bitten \
             by twice: {fleet}"
        );

        for stored in [GhToken::ReadToken, GhToken::WritePat] {
            let recipe = renew_recipe(stored, "thing-fleet");
            assert!(
                recipe.contains("Settings → GitHub & keys"),
                "a token stored in Settings is replaced in Settings: {recipe}"
            );
            assert!(
                !recipe.contains("sbx secret set"),
                "a token stored in Settings must not be told to run an sbx command: {recipe}"
            );
        }
        // And each stored token is sent to the field that holds it (SKEIN-1179): the read token
        // lives in "Your GitHub identity", a repository's token on its card. Collapsing the two
        // arms back into one sentence makes one of these fail.
        let read = renew_recipe(GhToken::ReadToken, "thing-fleet");
        assert!(
            read.contains("Your GitHub identity") && !read.contains("Settings → Repositories"),
            "the read token's recipe names somewhere else: {read}"
        );
        let repo = renew_recipe(GhToken::WritePat, "thing-fleet");
        assert!(
            repo.contains("Settings → Repositories") && !repo.contains("Your GitHub identity"),
            "a repository token's recipe names somewhere else: {repo}"
        );
    }
}
