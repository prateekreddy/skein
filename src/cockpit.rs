//! The page, and the joins between what the server sends and what the page reads.
//!
//! The page is one HTML file compiled into the binary, and it is here rather than in
//! `skein-server` for a reason the tests below make concrete: **nothing in the language connects a
//! field the server serialises to the name the page reads.** Rename one side and the feature does
//! not break loudly — the button never appears, the voice goes quiet — which looks exactly like
//! "nothing needed you", the state these features exist to distinguish from silence.
//!
//! So each join is asserted, in both directions, next to the asset that carries it. The browser
//! smoke tests cannot cover most of them (their fixture box belongs to no repo, so the flags are
//! always false), which is why these are here and not left to a reader.
//!
//! **Two kinds of assertion live in these tests, and they are not the same kind.**
//!
//! A **wire** assertion — "the page reads `b.foreign`", "the page reads `r.mem_anon`" — is about a
//! join between two languages, and nothing but a string match can make it. Those stay, and belong
//! here.
//!
//! A **logic** assertion — "voice and alerts are independent switches", "the keyboard shortcut does not
//! fire in a text field" — was a string match standing in for a test, because the function could not
//! be imported. Those are retired: the decisions live in `cockpit/src` and are asserted by *calling*
//! them. What is left of them here is the **join** — that the page still asks — which is the same
//! kind of assertion as the wire ones and is made the same way.
//!
//! The split that made it possible is worth stating, because it is what "not pure" actually meant:
//! deciding *what should happen* takes its inputs as arguments and returns a value; doing it needs a
//! mouth, a notification permission, and a focused element. Only the first half moved.
//!
//! The vendored scripts stay in the binary: they are bytes to hand out, and no join runs through
//! them.

/// The cockpit page. One owner, so the server and the tests below cannot disagree about which
/// bytes are the page.
pub const INDEX: &str = include_str!("web/index.html");

/// The new board (§11), served at `/v2` beside the old one.
///
/// Its own file rather than a mode of [`INDEX`]: the two are meant to diverge, and a flag inside one
/// document would make every change to the old board a change to the new one. `docs/parity.md` §7 is
/// the gate that decides when `/` becomes this, and until then both are shipped.
pub const V2: &str = include_str!("web/v2.html");

/// The cockpit's pure functions, built from `cockpit/src` by `cockpit/build.mjs`.
///
/// Embedded like the vendored scripts, because it is the same kind of thing: bytes the page needs.
/// It lives in a directory of modules rather than in the page because a function that cannot be
/// imported cannot be tested except by reimplementing it — and a reimplementation agrees with the
/// code right up until one of them changes, which is the failure `src/board.rs` has string-matching
/// assertions for.
pub const BUNDLE: &str = include_str!("web/vendor/cockpit.js");

#[cfg(test)]
mod tests {
    use super::*;
    use crate::board::BoxView;

    /// The browser's box-name rule and skein's are the same rule, checked by running both.
    ///
    /// `cockpit/src/naming.mjs` is a **mirror** of `util::slug` and `repos::box_name`, and a mirror
    /// is a thing that drifts. The alternative was asking the server for the name while somebody
    /// types, which is a round trip per keystroke to compute a string neither end disagrees about —
    /// and the alternative to that is a placeholder shaped like a name, which is how you end up with
    /// a box called `thing-<branch>`.
    ///
    /// So it is checked rather than trusted: node evaluates the committed bundle over a list of
    /// cases and Rust evaluates its own, and they are compared. Skipped where node is absent, like
    /// the staleness check above.
    #[test]
    fn the_browser_names_a_box_the_way_skein_does() {
        let cases = [
            "feat/auth",
            "feat/auth/v2",
            "user@host~weird",
            "keep.dots_and-dashes",
            "/leading/and/trailing/",
            "a///b",
            "///",
            "",
            "caffè",
            "日本語",
            "WIP: try 2",
            "release/2026.08.21",
        ];
        let script = format!(
            "{BUNDLE}\nconst cases = {};\nconsole.log(cases.map(c => boxNameFor('demo', c)).join('\\n'));",
            serde_json::to_string(&cases).unwrap()
        );
        let ran = std::process::Command::new("node")
            .arg("-e")
            .arg(&script)
            .output();
        let Ok(out) = ran else {
            eprintln!("skipping: no node on this machine to run the cockpit bundle");
            return;
        };
        assert!(
            out.status.success(),
            "the bundle would not run: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let theirs: Vec<String> = String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(str::to_string)
            .collect();
        let ours: Vec<String> = cases
            .iter()
            .map(|c| crate::repos::box_name("demo", c))
            .collect();
        assert_eq!(
            theirs, ours,
            "the browser would create a box under a different name than skein gives it"
        );
    }

    /// Every name `/v2` reads off the wire, asserted against the value the server actually sends.
    ///
    /// The same reason the joins above exist, and it bites harder here: this page is new, so there
    /// is no fleet of users to notice that a row never appears. Rename a field on either side and
    /// the board renders `undefined` in a corner, or renders nothing and looks calm — which is the
    /// one thing it must never do by accident.
    #[test]
    fn the_new_board_reads_the_names_the_server_sends() {
        use crate::queue::{Need, Row, Source, Standing};
        let row = Row {
            source: Source::Box,
            need: Need::You,
            repo: "web".into(),
            name: "web-main".into(),
            headline: "shall I proceed?".into(),
            state: "needs-input".into(),
            waiting_secs: Some(30),
            url: "/box/web-main".into(),
            fix: String::new(),
        };
        let wire = serde_json::to_string(&row).unwrap();
        for field in [
            "source",
            "need",
            "repo",
            "name",
            "headline",
            "state",
            "waiting_secs",
            "url",
        ] {
            assert!(
                wire.contains(&format!("\"{field}\"")),
                "Row stopped sending {field}"
            );
            assert!(
                V2.contains(&format!("r.{field}")),
                "the page does not read {field}"
            );
        }
        // `fix` is skipped when empty, so it is asserted on a row that has one.
        let fixable = Row {
            fix: "run `skein doctor`".into(),
            ..row
        };
        assert!(serde_json::to_string(&fixable).unwrap().contains("\"fix\""));
        assert!(V2.contains("r.fix"), "a fault's way out is never rendered");

        // The two keys `/api/queue` wraps its answer in.
        assert!(V2.contains("body.waiting") && V2.contains("body.standing"));

        // The three states, by the tags serde emits. The words are in the bundle, not the page.
        for standing in [
            Standing::SetupIncomplete { faults: 1 },
            Standing::NeedsYou { rows: 1 },
            Standing::Calm,
        ] {
            let tag = serde_json::to_value(&standing).unwrap()["standing"]
                .as_str()
                .unwrap()
                .to_string();
            assert!(
                BUNDLE.contains(&format!("\"{tag}\"")),
                "nothing renders the {tag} state, so it falls through to the unknown one"
            );
        }

        // The needs, by the same rule: a tone is chosen per need, and a need with no tone is grey —
        // which would render a box that is asking you something as though nothing were happening.
        for need in [
            Need::You,
            Need::YourAttention,
            Need::Done,
            Need::Machine,
            Need::Quiet,
            Need::Gone,
        ] {
            let tag = serde_json::to_value(need)
                .unwrap()
                .as_str()
                .unwrap()
                .to_string();
            assert!(
                BUNDLE.contains(&format!("\"{tag}\":")),
                "no tone is defined for the {tag} need, so a row that has it renders as though \
                 nothing were happening"
            );
        }
    }

    /// The change view reads every field `shape::of_diff` sends, by the name it sends it under.
    ///
    /// Two of these are the ones worth the test on their own. `note_state` decides whether a note is
    /// shown at all — a stale note reads exactly like a current one, which is why it is not — and
    /// `mentions` is absent rather than zero when nothing was counted, where a zero would read as
    /// "nothing uses this".
    #[test]
    fn the_change_view_reads_what_the_shape_route_sends() {
        use crate::contracts::Signal;
        use crate::shape::{ModuleChange, Movement, Sighted};
        let module = ModuleChange {
            path: "src/warden".into(),
            owners: vec!["@core".into()],
            movement: Movement::Shrank,
            added: 12,
            removed: 80,
            files: vec!["src/warden/doer.rs".into()],
            signals: vec![Sighted {
                signal: Signal {
                    kind: "interface".into(),
                    what: "check was removed or renamed".into(),
                    file: "src/warden/doer.rs".into(),
                    symbol: "check".into(),
                },
                mentions: Some(12),
            }],
            note: "the host side of create and destroy".into(),
            note_state: "fresh".into(),
        };
        let wire = serde_json::to_value(&module).unwrap();
        for field in wire.as_object().unwrap().keys() {
            // Read by the page, or by the bundle's `noteOf`/`summaryOf` — either is reading it; a
            // field read by neither is one the server serialises for nobody.
            assert!(
                V2.contains(&format!("m.{field}")) || BUNDLE.contains(&format!("m.{field}")),
                "nothing reads a module's `{field}`"
            );
        }
        let signal = serde_json::to_value(&module.signals[0]).unwrap();
        assert!(
            signal.get("mentions").is_some() && signal.get("symbol").is_some(),
            "the signal lost the two fields the count is built from"
        );
        for field in ["symbol", "what"] {
            assert!(
                V2.contains(&format!("g.{field}")),
                "the signal's `{field}` is not rendered"
            );
        }
        assert!(
            BUNDLE.contains("signal.mentions")
                || BUNDLE.contains("s.mentions")
                || BUNDLE.contains(".mentions"),
            "nothing reads the count"
        );

        // The four classifications, by the words serde emits. An unrendered one shows as blank
        // beside a module that moved, which reads as "nothing happened to it".
        for movement in [
            Movement::New,
            Movement::Gone,
            Movement::Shrank,
            Movement::Changed,
        ] {
            let word = serde_json::to_value(movement)
                .unwrap()
                .as_str()
                .unwrap()
                .to_string();
            assert!(
                !word.is_empty() && word.chars().all(|c| c.is_ascii_lowercase()),
                "{word} is not the kebab-case word the page uppercases in CSS"
            );
        }
        assert!(
            V2.contains("text-transform: uppercase"),
            "the classification is not shouted"
        );

        // A branch and a pull request reach the same view. The URLs are built in the bundle, so the
        // page cannot grow a second opinion about which route answers.
        assert!(
            V2.contains("shapeUrl("),
            "the page does not ask for a shape at all"
        );
        // The order is the server's. `worth_looking_at` puts a module carrying a contract signal
        // first however small its change, and a page that sorted would be a second opinion about
        // attention — the one thing §11.7 says is the scarce resource.
        assert!(
            !V2.contains(".sort("),
            "the page re-orders what the server already ranked"
        );
        assert!(BUNDLE.contains("/api/boxes/") && BUNDLE.contains("/api/pr/"));
    }

    /// Setting up reads and writes the names the server uses, on both surfaces.
    ///
    /// This is the first surface `/v2` has that *writes*, and a write that names a field wrongly is
    /// worse than a read that does: a read renders nothing and a write is accepted, silently
    /// dropping what it did not spell right — `AddRepoReq` takes `#[serde(default)]` on three of its
    /// four fields, so `stor` instead of `store` is a repo whose shared-data folder is quietly
    /// skein's own.
    #[test]
    fn the_setup_surface_writes_the_fields_the_server_reads() {
        // Add a repo: the two fields this surface sends, and the two it reads back.
        assert!(V2.contains(r#"fetch("/api/repos", {"#));
        for field in ["source", "store"] {
            assert!(
                V2.contains(&format!("{field},")) || V2.contains(&format!("{field}:")),
                "the add-a-repo form does not send `{field}`"
            );
        }
        assert!(
            V2.contains("body.repo?.id") && V2.contains("body.warning"),
            "the answer's repo and warning are not read — a push path that will not work is \
             exactly what that warning is for"
        );

        // Make a box: an Act, and read as one.
        assert!(V2.contains("/create") && V2.contains("JSON.stringify({ branch })"));
        assert!(
            V2.contains("answer.status !== 202"),
            "a create is accepted, not answered — a surface treating 200 as success would report a \
             box that was never started"
        );
        assert!(
            V2.contains("/api/acts/")
                && V2.contains("look.state === \"ended\"")
                && V2.contains("look.code"),
            "the act's outcome is not read, so a failed create looks like a slow one"
        );

        // The path check that replaces Browse, and the three answers it distinguishes.
        assert!(V2.contains("/api/path?p="));
        assert!(
            V2.contains("found.resolved") && V2.contains("found.kind"),
            "the path check's answer is not read"
        );
        assert!(
            !V2.contains("pick-path"),
            "Browse came back — it needs a host display the in-fleet skein cannot have (parity §7)"
        );
    }

    /// The two live joins: the event names the stream sends, and the frame the PTY parses.
    #[test]
    fn the_new_board_listens_and_resizes_in_the_words_the_server_uses() {
        for event in ["snapshot", "changed", "behind"] {
            assert!(
                V2.contains(&format!("\"{event}\"")),
                "the board ignores the {event} event, so it stops updating without saying so"
            );
        }
        // `{"resize":{"cols":N,"rows":M}}` is what `terminal` parses; anything else is silently
        // dropped and the box runs at 100x30 for ever.
        assert!(
            V2.contains("resize: { cols:"),
            "the resize frame is not the shape the server reads"
        );
        assert!(V2.contains("/api/boxes/${encodeURIComponent(name)}/terminal"));
    }

    /// It is one document, it uses the bundle rather than a second copy of it, and it fetches
    /// nothing from the network.
    #[test]
    fn the_new_board_is_one_self_contained_document() {
        assert_eq!(
            V2.matches("<script").count(),
            V2.matches("</script>").count(),
            "unbalanced <script> tags silently blank the page"
        );
        assert!(V2.trim_end().ends_with("</html>"));
        assert!(
            !V2.contains("cdn."),
            "the cockpit must work with no network"
        );
        assert!(V2.contains("/vendor/cockpit.js"));
        assert!(V2.contains("/vendor/xterm.js") && V2.contains("/vendor/xterm.css"));
        // The decisions live in the bundle. A copy of one here is a copy that drifts.
        for called in ["toneOf(", "headlineOf(", "densityFor(", "ageNow("] {
            assert!(V2.contains(called), "the page does not call {called})");
        }
        assert!(
            !V2.contains("function toneOf") && !V2.contains("function densityFor"),
            "a decision was reimplemented in the page instead of imported from the bundle"
        );
        // Beside the old board, not instead of it: the way back is on the page.
        assert!(
            V2.contains("href=\"/\""),
            "there is no way back to the old board"
        );
    }

    /// The committed bundle is what `cockpit/src` builds.
    ///
    /// `cargo build` does not run node, so the bundle is committed — and a committed build artefact
    /// is one that can go stale silently, which here means a cockpit quietly running last week's
    /// code. So the build is run again and compared. Skipped where node is absent, which keeps a
    /// machine without it able to build skein; CI has node and does not skip.
    #[test]
    fn the_cockpit_bundle_is_not_stale() {
        let checked = std::process::Command::new("node")
            .args(["cockpit/build.mjs", "--check"])
            .output();
        let Ok(out) = checked else {
            eprintln!("skipping: no node on this machine to rebuild the cockpit bundle");
            return;
        };
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// A row ages itself, from the observation rather than from a string the server formatted.
    ///
    /// The stream stopped re-sending a box that only got older, so a page rendering `b.age` would
    /// freeze between real changes and a fleet quiet for ten minutes would say "2m ago" for ever.
    /// This is a wire assertion in both directions: the page must read the field the server sends
    /// (`age_secs`), and must not read the one it no longer maintains.
    #[test]
    fn the_page_ages_a_row_rather_than_reading_a_frozen_string() {
        assert!(
            INDEX.contains("b.age_secs"),
            "the page does not read the observation's age, so it cannot age the row itself"
        );
        assert!(
            !INDEX.contains(".textContent = b.age;"),
            "the page still renders the server's formatted string, which no longer advances"
        );
        assert!(
            INDEX.contains("setInterval(tickAges"),
            "nothing advances the ages, so they move only when something else changes"
        );
        // The other half of the sum: a row's age is what it was when observed plus how long ago
        // that was, so the page has to remember when each row arrived.
        assert!(
            INDEX.contains("receivedAt.set("),
            "the page does not remember when a row arrived, so it has nothing to add to age_secs"
        );
    }

    /// The page loads the bundle, and does not carry its own copy of what is in it.
    ///
    /// Two sources of truth is worse than one: the page would keep working while the tested copy
    /// drifted, and every node test would be passing against code nobody runs.
    #[test]
    fn the_page_uses_the_bundle_rather_than_a_second_copy() {
        assert!(
            INDEX.contains("/vendor/cockpit.js"),
            "the page does not load the bundle, so the tested functions are not the ones it runs"
        );
        for gone in [
            "function matchesFilter(",
            "function boardRows(",
            "const fmtGB =",
            "const groupOf =",
            "const NEEDS_YOU =",
            "function ago(",
        ] {
            assert!(
                !INDEX.contains(gone),
                "`{gone}` is still defined in the page as well as in the bundle"
            );
        }
        // And the bundle really does define them, so removing them from the page did not remove
        // them from the product.
        for defined in [
            "function matchesFilter(",
            "function boardRows(",
            "const fmtGB =",
            "const groupOf =",
            "const NEEDS_YOU =",
            "function ago(",
            "function ageNow(",
        ] {
            assert!(
                BUNDLE.contains(defined),
                "the bundle is missing `{defined}`"
            );
        }
    }

    /// The signal is computed in Rust and read in the page by name, and nothing else connects them:
    /// rename one side and the button silently never appears, which looks exactly like "nothing to
    /// update" — the failure this whole feature exists to end. The browser smoke test cannot reach
    /// this path (its fixture box belongs to no repo, so the flag is always false), so the join is
    /// asserted here instead of left to a reader.
    #[test]
    fn the_page_reads_the_update_flag_by_the_name_the_fleet_sends() {
        let view = BoxView {
            docs_update: true,
            ..BoxView::default()
        };
        let json = serde_json::to_string(&view).unwrap();
        assert!(
            json.contains("\"docs_update\":true"),
            "the fleet snapshot stopped carrying the flag: {json}"
        );
        assert!(
            INDEX.contains("b.docs_update"),
            "the cockpit no longer reads docs_update, so the update button can never appear"
        );
    }

    /// The cockpit speaks the box's *own ask*, and it speaks on its own switch.
    ///
    /// Two things about the mouth fail silently, which is the worst way for a voice to fail — you
    /// cannot tell "nothing needs me" from "it stopped talking". Both are one careless edit away:
    ///
    /// 1. **The words.** Speaking `headline` is the whole point — "example-box-1 wants permission. Run
    ///    rm -rf build?" is actionable where "example-box-1 needs a decision" is only a reason to go and
    ///    look, which is the trip this feature exists to save. Folding it back onto the notification
    ///    text would sound identical to someone who never heard the good version.
    /// 2. **The switch.** Notifications need a browser permission that may have been refused;
    ///    speaking needs none. Gating voice on `alertsOn` would silence the half that still works,
    ///    for people who had already said no to the half that does not.
    #[test]
    fn the_cockpit_speaks_the_boxs_own_ask_on_a_switch_of_its_own() {
        let page = INDEX;
        let view = BoxView {
            headline: Some("Run rm -rf build?".into()),
            ..BoxView::default()
        };
        let json = serde_json::to_string(&view).unwrap();
        assert!(
            json.contains("\"headline\":\"Run rm -rf build?\""),
            "the fleet snapshot stopped carrying the ask, so there is nothing to say: {json}"
        );
        assert!(
            page.contains("b.headline") && page.contains("forSpeech"),
            "the cockpit no longer speaks the box's own words"
        );
        // **The switch is no longer asserted here.** It used to be two string matches looking for
        // the shape of the code — `if (voiceOn) say(` and the absence of `voiceOn && alertsOn` —
        // which is a test of the source text rather than of the behaviour. The decision is
        // `announcementsFor` in `cockpit/src/announce.mjs` now, and `cockpit/test/announce.test.mjs`
        // asserts the property by *calling* it: voice on with alerts off still speaks, and alerts on
        // with voice off stays silent. What is left in the page is the speaking and the notifying,
        // which cannot be tested without a mouth.
        assert!(
            page.contains("announcementsFor("),
            "the page decides for itself again, so the independence of the two switches is untested"
        );
    }

    /// Nothing a misheard word can reach is hard to undo.
    ///
    /// Speech recognition is wrong sometimes — that is not a defect to engineer away, it is the
    /// medium. So the design constraint is not accuracy, it is *blast radius*: every verb the ear
    /// accepts is either read-only or reversible, and the destructive ones are absent rather than
    /// confirmed. A confirmation is the wrong answer here because the whole point of the ear is that
    /// you are not looking at the screen; a dialog you cannot see is a dialog you will dismiss by
    /// saying the next thing.
    ///
    /// The second half is subtler and just as easy to lose: the ear has to reach you *inside a
    /// focused terminal*. The fleet keymap deliberately yields every key to one, so push-to-talk
    /// cannot live there — answering a box while heads-down in another one is the entire use, and an
    /// ear that only works on the board is an ear you would never reach for.
    #[test]
    fn a_misheard_word_cannot_cost_a_branch() {
        let page = INDEX;
        let verbs: String = page
            .lines()
            .skip_while(|l| !l.contains("const VOICE_VERBS"))
            .take_while(|l| !l.starts_with("];"))
            .collect();
        assert!(
            verbs.contains("resumeBox"),
            "the verb table was not found at all"
        );
        for reckless in ["destroyBox", "mergePr", "stopBox", "shipBox", "takeover"] {
            assert!(
                !verbs.contains(reckless),
                "`{reckless}` is reachable by voice; a word heard wrong must cost a glance, not work"
            );
        }
        // Push-to-talk on its own handler, keyed by code so it survives a focused terminal.
        assert!(
            page.contains("AltRight"),
            "the ear has no push-to-talk key, so it can only be reached from the board"
        );
        // **The typing guard is no longer asserted here.** It used to be a match on
        // `if (inTerm || inField) return;` — the shape of the code standing in for the behaviour.
        // The decision is `shortcutFor` in `cockpit/src/keys.mjs` now, and
        // `cockpit/test/keys.test.mjs` asserts it by *calling* it, for every key the board
        // dispatches: each one fires with focus nowhere and returns nothing with focus in a field or
        // a terminal. What is left here is the join — the page must still ask.
        assert!(
            page.contains("shortcutFor(e, { inTerm, inField })"),
            "the fleet keymap decides for itself again, so nothing tests that it yields to a \
             focused terminal — and the ear works only because that guard is there"
        );
    }

    #[test]
    fn index_html_is_well_formed() {
        // The whole UI is one include_str!'d file; a missing close tag silently blanks the page.
        let html = INDEX;
        assert_eq!(
            html.matches("<script").count(),
            html.matches("</script>").count(),
            "unbalanced <script> tags"
        );
        assert!(html.trim_end().ends_with("</html>"));
        assert!(html.contains("id=\"fleet\""));
        assert!(html.contains("/vendor/xterm.js")); // vendored, not CDN
        assert!(!html.contains("/vendor/addon-webgl.js"));
        assert!(html.contains("customGlyphs:true"));
        assert!(html.contains(".agent-statusline"));
        assert!(html.contains("white-space:pre;"));
        assert!(html.contains("replace(/ /g,\"&nbsp;\")"));
        assert!(html.contains(".agent-statusline { display:block; }"));
        assert!(!html.contains("cdn.jsdelivr"));
        assert!(html.contains("id=\"drestart\""));
        assert!(!html.contains(">Create PR</button>"));
    }
}
