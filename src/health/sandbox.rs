//! The sandbox the boxes run in: its listing, the host warden, the isolation cover every
//! running box should be under, and the git scope a box holds.

use super::*;

/// The `sbx` line, extracted so it can be read without building a whole report — which is why
/// `git_scope_health` sits beside it.
///
/// **Whether `sbx` is on `$PATH` decides nothing, and that is the change** (SKEIN-576). It used to
/// be the first question: a missing `sbx` was a fault with a fix, because `sbx` was how skein
/// reached every box. Skein runs inside the fleet sandbox now — it enters a box by its namespace,
/// and `sbx ls` is a question about the HOST's machine, which this process is not standing on. So
/// the binary's presence became a fact about nothing, while still flipping this row from
/// *satisfied* to *unknown* when it happened to be installed. That is a row that changes for a
/// reason the reader cannot act on, which is worse than one that says the same thing every time.
///
/// A listing still ANSWERS when something can answer it — `$SKEIN_LS_CMD`, a test or a proxy — and
/// those two arms are kept for that: skein reporting from a picture it took a moment ago is neither
/// current nor wrong, which is what `unknown` is for. What is gone is the fault.
///
/// `docs/parity.md` §7 records what a person stops being told.
pub(super) fn sbx_health(fleet: &Option<Vec<crate::sbx::SbxBox>>, degraded: bool) -> HealthCheck {
    match (fleet, degraded) {
        (Some(boxes), true) => HealthCheck::unknown(format!(
            "`sbx ls` did not answer just now; showing the last successful snapshot ({} boxes)",
            boxes.len()
        )),
        (Some(boxes), false) => {
            HealthCheck::satisfied(format!("available ({} boxes)", boxes.len()))
        }
        // Nothing asked, which is the ordinary state rather than a failure to get an answer.
        // `unknown` here would report a question skein deliberately does not put, and on the
        // first-run checklist that reads as a step somebody has to go and fix — the one step a new
        // person cannot fix from inside the cockpit.
        (None, _) => HealthCheck::satisfied(
            "not asked, and not needed: skein is inside the fleet sandbox, so it enters a box by \
             its namespace rather than through sbx, and which boxes exist is read from their \
             placement records",
        ),
    }
}

/// Whether the host warden is answering, and what that changes — which is **information, not a
/// verdict** (SKEIN-1184).
///
/// The warden is optional, and the owner's decision says so in as many words
/// (`docs/decisions/warden-or-prompt.md`): a privileged host act is done by a warden that can, or
/// the person is shown the command. So no warden is a supported state with an ordinary path through
/// it, and this line says which of the two routes this fleet is on rather than grading it.
///
/// **It used to be a fault, counted on the banner, and the argument for that is overturned rather
/// than forgotten.** It read: fleet create and destroy go only through the warden, so without one
/// skein cannot do something it offers — and finding out late means a fleet you cannot resize at
/// the moment you need to. Both premises went: a fleet is created by the person on the host before
/// skein exists (`README.md`, "Getting started"), a resize from inside the fleet is always the line
/// to run on the host (`fleet_lifecycle_refusal`), and every act that does go through
/// `warden_client::perform` falls back to a prompt carrying the command, why, and what declining
/// costs. What the fault's fix told a first run to do was compile Rust on a host the install
/// deliberately keeps free of it.
///
/// **Something answering and refusing is still a fault**, because that is a warden somebody tried
/// to run and got wrong — a port the two ends disagree about — and it has one fix. It no longer
/// reaches the banner (`OnBanner::NotCounted` in `report.rs`); it stays on the diagnostics pane,
/// where a person looking at their warden will see it.
///
/// **What it does not do is trust the answer.** `capabilities` is what the far end SAYS it can do,
/// and §8.3 is blunt that this is never evidence — a malicious endpoint advertises whatever makes
/// skein show a button. So it is reported, in the warden's own words, and nothing here decides
/// anything from it.
pub(super) fn warden_health(seen: Option<crate::warden_client::Sighting>) -> HealthCheck {
    // Said whichever way the check goes, because setting the warden's own variable on a client is a
    // mistake even when something happens to answer: it means this process is not asking where the
    // person thinks it is. It rides on the check rather than being a line of its own — a reader
    // looking at the warden is exactly the reader who needs it.
    let misdirected = crate::warden_client::misdirected();
    let note = |text: String| match &misdirected {
        Some(said) => format!("{text}\n{said}"),
        None => text,
    };
    match seen {
        Some(sighting) => {
            let doers = match sighting.capabilities.is_empty() {
                true => "it advertises no doers, so it can report but not create or destroy".into(),
                false => format!("it says it can {}", sighting.capabilities.join(" and ")),
            };
            HealthCheck::satisfied(note(format!(
                "answering on {}, and {doers} ({} sandbox(es) in view)",
                crate::warden_client::where_it_asks(),
                sighting.sandboxes.len()
            )))
        }
        None => match crate::warden_client::sighting_trouble() {
            // It answered, and it is not a warden this skein can use. Do not send anybody to a
            // compiler: the detail stays the client's own words and the fix is about the port.
            Some(crate::warden_client::Unseen::Answered) => HealthCheck::unsatisfied(
                note(crate::warden_client::sighting_failure().unwrap_or_else(|| {
                    "the host warden did not answer, and no reason was recorded".into()
                })),
                format!(
                    "something is answering on {} and it is not a warden this skein can use. Check \
                     what is on that port, and that both ends agree about which one it is: \
                     `$SKEIN_WARDEN` moves the client, `$SKEIN_WARDEN_PORT` moves the warden, and \
                     setting only one of them aims skein at whatever else happens to be listening.",
                    crate::warden_client::where_it_asks()
                ),
            ),
            // Nothing there: the ordinary state of an install that never ran one. Where it looked
            // is still said, because somebody who IS running a warden and reads this needs the
            // address to see why this one cannot reach it.
            _ => HealthCheck::satisfied(note(format!(
                "none answering at {} \u{2014} that is fine. When skein needs something done on \
                 the host, it shows you the command to run. `skein-warden`, running on the host, \
                 does those for you instead, after you approve each one in its terminal.",
                crate::warden_client::where_it_asks()
            ))),
        },
    }
}

/// The warden line on its own, for `skein doctor`.
///
/// Public for the same reason [`health_report_gitgate`] is: the CLI builds its own list rather than
/// rendering the whole report, so a check that is only reachable through `health_report` is one the
/// terminal never shows.
pub fn warden_report() -> HealthCheck {
    warden_health(crate::warden_client::sighting())
}

/// The isolation line: whether every running box is under the cover this skein installs.
///
/// A fault rather than a note, and the argument had two sides. Against: the fix costs whatever the
/// agent in that box had half-finished, so somebody may reasonably put it off, and a red mark they
/// cannot clear without losing work is the shape of an alarm people learn to ignore. For, and it
/// wins: every other thing on this panel is skein failing at something, and this is the fleet being
/// less isolated than the person running it believes. That belief is exactly what a per-box cover
/// was built to make safe, and a quiet note is how the gap went unnoticed long enough to be found
/// by looking at a box rather than by reading the board.
///
/// The wording says what a restart BUYS. "Stale" describes a file and leaves the reader to work out
/// why they should care; the cover is the reason, so the cover is what the sentence names — and it
/// says what a restart costs too, because this is a decision about somebody's unfinished work
/// rather than an instruction.
pub fn cover_health(uncovered: &[String]) -> HealthCheck {
    match uncovered.is_empty() {
        true => HealthCheck::satisfied("every running box is under the current isolation"),
        false => HealthCheck::unsatisfied(
            format!(
                "started before the current isolation and still running under the old one: {}",
                uncovered.join(", ")
            ),
            format!(
                "`skein restart {}` — restarting it rebuilds the box's namespace with the covers \
                 this skein installs. Its checkout and its branch are untouched; whatever the \
                 agent was part-way through is not, so pick the moment",
                uncovered.first().map(String::as_str).unwrap_or("<box>")
            ),
        ),
    }
}

/// Running boxes with no memory ceiling on them, for the CLI, which prints its lines one at a time
/// rather than from a report.
pub fn uncapped_boxes() -> Vec<String> {
    crate::board::load_views()
        .unwrap_or_default()
        .into_iter()
        .filter(|view| !view.ceiling.is_empty() && !crate::fleet::is_capped(&view.ceiling))
        .map(|view| view.name)
        .collect()
}

/// The boxes that line is about: running, and placed by a launcher that is not the current one.
///
/// A separate entry point because `skein doctor` prints its lines one at a time rather than from a
/// report, and the CLI is where somebody looks when the cockpit is the thing that is not running.
pub fn uncovered_boxes() -> Vec<String> {
    crate::board::load_views()
        .unwrap_or_default()
        .into_iter()
        .filter(|view| view.cover == "older")
        .map(|view| view.name)
        .collect()
}

/// Render [`crate::gitgate::ScopeStatus`] as a health line.
///
/// Presentation only — the states themselves belong to `gitgate`, which is the module that knows
/// what they mean. This file used to derive them by reaching into five of its internals, which is
/// how two callers of the same question end up disagreeing.
pub(super) fn git_scope_health() -> HealthCheck {
    use crate::gitgate::ScopeStatus::*;
    // What a box holds when scoping is *not* in force is no longer a fixed sentence. It used to be —
    // the account token was seeded by default, so "not scoped" always meant "every box holds your
    // whole account". Now that all three credential paths are chosen, an unscoped box may hold nothing
    // at all, and telling someone their boxes carry a credential they never picked sends them hunting
    // the wrong problem the first time a push fails.
    //
    // Every line here says what a box HOLDS, and none of them says what a box can REACH. They used
    // to — "cannot push", "boxes write only their own repo" without the token named — and that was
    // false: the sandbox proxy answers a request carrying no credential as the account
    // (SKEIN-548, open; `gitgate`'s module note has the measurement).
    let unscoped_holds = match crate::gitgate::box_credential() {
        crate::gitgate::BoxCredential::None => {
            "boxes hold no GitHub credential of their own".to_string()
        }
        other => format!("every box holds {}", other.label()),
    };
    match crate::gitgate::scope_status() {
        Off => HealthCheck::satisfied(format!(
            "off — {unscoped_holds}. Settings → GitHub & keys → Scope each box's GitHub \
             access to its own repo"
        )),
        NotConfigured => HealthCheck::satisfied(format!(
            "not set up — {unscoped_holds}. Settings → GitHub & keys → add a GitHub App or a \
             per-repo token to scope them"
        )),
        Unusable { why, refused } => HealthCheck::unsatisfied(
            format!(
                "ON but nothing is scoped, so {unscoped_holds}: {why}.{}",
                match refused.is_empty() {
                    true => String::new(),
                    false => format!(" Stored tokens refused — {}.", refused.join("; ")),
                }
            ),
            "Settings → GitHub & keys → add a GitHub App, or a per-repo token for each repo in use",
        ),
        Active { app, tokens } => HealthCheck::satisfied(format!(
            "on — a box's own token writes only its own repo{}.{}{}",
            // A shared token is the exception, and the line is untrue without it (SKEIN-1231).
            match crate::gitgate::write_credentials()
                .iter()
                .filter(|c| c.shared && c.repos.len() > 1 && c.problem().is_none())
                .count()
            {
                0 => String::new(),
                n => format!(
                    ", or every repo its token is shared with ({n} token(s) shared on purpose)"
                ),
            },
            match app.is_empty() {
                true => String::new(),
                false => format!(" App {app}"),
            },
            match tokens {
                0 => String::new(),
                n => format!(" {n} stored repo token(s)"),
            }
        )),
    }
}

/// The git-scope check alone, so `skein doctor` can print the one line without building the whole
/// report — which probes the sandbox and takes seconds.
pub fn health_report_gitgate() -> HealthCheck {
    git_scope_health()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::health::testkit::*;

    /// The warden line, against a warden rather than by reading the code.
    ///
    /// Both arms matter and they fail differently. A warden that is not there is **information,
    /// not a fault** (SKEIN-1184, `docs/decisions/warden-or-prompt.md`): the person is shown the
    /// command instead, so nothing is blocked, and the line says what a warden would add without
    /// telling anybody to compile one. A warden that IS there has to be believed about being
    /// reachable and quoted, never trusted, about what it can do: §8.3 says the advertised
    /// capability set may decide what skein offers and may never stand in for a check.
    ///
    /// **The concrete change that makes it fail, named before it was written:** putting the old
    /// `HealthCheck::unsatisfied` arm back for a warden that is not there fails `a missing warden
    /// is not a fault`; restoring its `cargo build --release --workspace` advice fails `nobody is
    /// sent to a compiler`.
    #[test]
    fn a_warden_that_is_not_answering_is_information_that_says_what_one_adds() {
        let _g = crate::testutil::env_lock();

        // A port nothing is listening on. Bound and dropped, so the number is real and free —
        // picking one out of the air races another test that happens to have bound it.
        let free = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let dead = free.local_addr().unwrap().port();
        drop(free);
        std::env::set_var("SKEIN_WARDEN", format!("127.0.0.1:{dead}"));
        let missing = warden_health(crate::warden_client::sighting());
        assert!(
            !missing.is_fault(),
            "a missing warden is not a fault — without one skein shows the person the command, \
             which is a supported route and not a broken one: {missing:?}"
        );
        // Not a fault, so no fix: `every_fault_says_what_would_fix_it` holds the other half.
        assert!(
            missing.fix.is_empty(),
            "information with a fix attached: {missing:?}"
        );
        // What it adds, and what happens without it — the two halves of the sentence a first run
        // reads on its checklist.
        for needed in ["skein-warden", "shows you the command"] {
            assert!(
                missing.detail.contains(needed),
                "the line does not say {needed:?}: {:?}",
                missing.detail
            );
        }
        for absent in ["cargo build", "--workspace"] {
            assert!(
                !missing.detail.contains(absent),
                "nobody is sent to a compiler over an optional piece: {:?}",
                missing.detail
            );
        }
        // Where it looked, because somebody who IS running a warden needs the address to see why
        // this one cannot reach it.
        assert!(
            missing.detail.contains(&dead.to_string()),
            "the line does not say where it looked: {:?}",
            missing.detail
        );

        // And one that answers, advertising both doers.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let (ready_tx, ready_rx) = std::sync::mpsc::channel::<()>();
        std::thread::spawn(move || {
            let _ = ready_tx.send(());
            for mut stream in listener.incoming().flatten() {
                use std::io::{Read, Write};
                let mut raw = [0u8; 4096];
                let _ = stream.read(&mut raw);
                let body = r#"{"sandboxes":["skein-fleet"],"capabilities":["create","destroy"]}"#;
                let _ = stream.write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    )
                    .as_bytes(),
                );
            }
        });
        // Confirmed accepting before the probe starts (SKEIN-1024): `sighting()`'s own budget
        // (`GLANCE`, 2s) is tighter than the injection probe's, so a brand-new thread racing it for
        // a first scheduler slot is, if anything, more exposed to the same guess about load.
        wait_until_accepting(ready_rx);
        std::env::set_var("SKEIN_WARDEN", format!("127.0.0.1:{port}"));
        let answering = warden_health(crate::warden_client::sighting());
        std::env::remove_var("SKEIN_WARDEN");

        assert!(
            !answering.is_fault(),
            "a warden that answered was still reported broken: {answering:?}"
        );
        assert!(
            answering.fix.is_empty(),
            "a satisfied check carries a fix for a problem it does not have: {:?}",
            answering.fix
        );
        // Quoted, not believed. What it says it can do is in the sentence because a person deciding
        // whether to trust a Launch button wants to see it — and nothing in `health` reads it.
        for said in ["create", "destroy"] {
            assert!(
                answering.detail.contains(said),
                "the report does not pass on what the warden said it can do: {:?}",
                answering.detail
            );
        }
    }

    /// Something answering on the warden's port is not the same fault as nothing being there.
    ///
    /// Written because the first version of this check got it wrong in the way that wastes somebody's
    /// afternoon: every unsatisfied arm printed `cargo build --release --workspace`, so a person
    /// looking at a warden they had just started and were watching log to their terminal was told to
    /// go and build one. The two failures send a reader to opposite places — a compiler, or the
    /// question of what is actually on that port — and the advice has to know which it is looking at.
    #[test]
    fn a_warden_that_answers_and_refuses_is_not_told_to_go_and_build_one() {
        let _g = crate::testutil::env_lock();

        // Something on the port that is not a warden: answers, refuses, says nothing useful. That is
        // exactly the shape a wrong port produces, which is now a thing somebody can arrange by
        // setting `$SKEIN_WARDEN_PORT` without `$SKEIN_WARDEN`.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let (ready_tx, ready_rx) = std::sync::mpsc::channel::<()>();
        std::thread::spawn(move || {
            let _ = ready_tx.send(());
            for mut stream in listener.incoming().flatten() {
                use std::io::{Read, Write};
                let mut raw = [0u8; 2048];
                let _ = stream.read(&mut raw);
                let _ = stream.write_all(
                    b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                );
            }
        });
        // Confirmed accepting before the probe starts -- see SKEIN-1024, noted at the sibling test
        // above.
        wait_until_accepting(ready_rx);
        std::env::set_var("SKEIN_WARDEN", format!("127.0.0.1:{port}"));
        let refused = warden_health(crate::warden_client::sighting());
        std::env::remove_var("SKEIN_WARDEN");

        assert!(
            refused.is_fault(),
            "a warden that would not answer read as fine"
        );
        for absent in ["cargo build", "--workspace"] {
            assert!(
                !refused.fix.contains(absent),
                "the advice tells somebody to build a warden that is plainly running: {:?}",
                refused.fix
            );
        }
        // And it names where it looked, because a wrong port is the likeliest cause and the reader
        // cannot check a number nothing printed.
        assert!(
            refused.fix.contains(&port.to_string()),
            "the advice does not say which address was asked: {:?}",
            refused.fix
        );
        // Both variables, because setting one without the other is how somebody gets here.
        for named in ["$SKEIN_WARDEN", "$SKEIN_WARDEN_PORT"] {
            assert!(
                refused.fix.contains(named),
                "the advice does not name {named}, and the two ends have to agree: {:?}",
                refused.fix
            );
        }
    }

    /// Setting the WARDEN's variable on a CLIENT is said, whichever way the check goes.
    ///
    /// The mistake is invisible from where somebody makes it: `$SKEIN_WARDEN_PORT` on a `skein`
    /// command looks like it moves where skein asks, and moves nothing — skein keeps asking the
    /// default, where something else may well answer. The failure that follows is a refusal from a
    /// stranger, which reads as the warden being broken rather than as being asked the wrong place.
    ///
    /// Said on the satisfied arm too, deliberately. Something answering does not mean it is the
    /// warden the person just started, and "it works" is the reading this has to prevent.
    #[test]
    fn the_wardens_own_variable_set_on_a_client_is_pointed_out() {
        let _g = crate::testutil::env_lock();
        std::env::remove_var("SKEIN_WARDEN");
        std::env::set_var("SKEIN_WARDEN_PORT", "7880");

        // The address it offers is the one this process would have used, not a fixed string: the
        // warden is on the host and skein is not, so that is `host.docker.internal` and a note
        // offering `127.0.0.1` would name the sandbox somebody is already inside.
        //
        // `None` rather than `crate::warden_client::sighting()`, which would ASK — and with
        // `$SKEIN_WARDEN` deliberately unset here, ask `host.docker.internal:7879`: whatever warden
        // the machine running the suite can reach, which `warden_client` refuses in a test process
        // now (SKEIN-762). It cannot be pinned away either, because an unset `$SKEIN_WARDEN` is the
        // condition `misdirected` fires on and the subject of the assertions below. `None` is
        // exactly what a warden that could not be asked gives back, so this is the same arm — and
        // now the same arm on every machine, rather than one that depends on whether whoever ran
        // the tests happens to have a warden up (SKEIN-690's shape, in the check about wardens).
        let said = warden_health(None);
        for needed in [
            "SKEIN_WARDEN_PORT",
            "SKEIN_WARDEN=host.docker.internal:7880",
            "7879",
        ] {
            assert!(
                said.detail.contains(needed),
                "the note does not mention {needed}: {:?}",
                said.detail
            );
        }

        // Both set is somebody who meant it, and the note goes away — otherwise it becomes noise on
        // every run of a fleet that has deliberately moved its warden.
        std::env::set_var("SKEIN_WARDEN", "127.0.0.1:7880");
        let quiet = warden_health(crate::warden_client::sighting());
        assert!(
            !quiet.detail.contains("is the WARDEN's variable"),
            "the note fires at somebody who set both: {:?}",
            quiet.detail
        );

        std::env::remove_var("SKEIN_WARDEN_PORT");
        std::env::remove_var("SKEIN_WARDEN");
    }

    /// A missing `sbx` is the normal state, and never a fault.
    ///
    /// Reporting it red would hand somebody a fault they cannot clear — `sbx` is host-only and
    /// cannot be installed into the sandbox — and, worse, would hide behind a false alarm the one
    /// thing they wanted to know: that skein reaches boxes another way. A banner that is red for a
    /// correct state is how the next real fault gets read as noise too.
    ///
    /// This had a second arm: on a host, no `sbx` meant no box could be created, started or
    /// entered, and that was a fault with `PATH` in the fix. There is no such host any more
    /// (SKEIN-576), so the arm that was true for it went with it — recorded in `docs/parity.md`
    /// §7, because the row it produced is one a person used to be able to see.
    ///
    /// **What would make this fail**: making the `(false, _, _)` arm of `sbx_health` unsatisfied
    /// again, which is precisely the deleted arm coming back.
    #[test]
    fn a_missing_sbx_is_the_normal_state_and_never_a_fault() {
        let _g = crate::testutil::env_lock();

        // **Nothing asked**, which is the production state: `fleet_boxes` returns `None` unless
        // something can answer for the host's machine. Satisfied, not unknown — the first-run
        // checklist reads `unknown` as a step somebody must go and fix, and this is the one step a
        // new person cannot fix from inside the cockpit (`tests/ui/onboarding.mjs` asserts it).
        let absent = sbx_health(&None, false);
        assert_eq!(
            absent.level,
            Level::Satisfied,
            "skein reported a question it deliberately does not put as one it could not get an \
             answer to: {}",
            absent.detail
        );
        assert!(
            absent.detail.contains("namespace"),
            "it says sbx is not asked without saying how boxes are reached instead: {}",
            absent.detail
        );

        // And a listing that DID answer still reports what it saw, so the seam that lets something
        // answer for the host has not been collapsed away with the fault.
        let answered = sbx_health(&Some(Vec::new()), false);
        assert_eq!(answered.level, Level::Satisfied);
        assert!(
            answered.detail.contains("available"),
            "a listing that answered stopped saying so: {}",
            answered.detail
        );
        // A stale snapshot is the one case that is neither current nor wrong, which is what the
        // third state is for — and it is not a fault either.
        let stale = sbx_health(&Some(Vec::new()), true);
        assert_eq!(stale.level, Level::Unknown, "{}", stale.detail);
        assert!(!stale.is_fault(), "{}", stale.detail);
    }
}
