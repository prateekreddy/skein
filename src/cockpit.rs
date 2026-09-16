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
            crate::testutil::skip("no node on this machine to run the cockpit bundle");
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
    }

    /// **The URL the change view asks for is a URL this router answers** (SKEIN-246).
    ///
    /// The view shipped whole — `shape::of_diff`, two handlers, the page that renders them — and a
    /// pull request could not reach it from `c5ddc46` until this change, because `shapeUrl` built
    /// `/api/pr/:repo/:n/shape` and nothing has ever registered that. Every click 404'd,
    /// `answer.json()` threw on the body, and `drawChange`'s catch printed "the change could not be
    /// read" — the page reporting a routing bug as its own failure.
    ///
    /// **It survived because the two tests over it asserted the string, not the join.** This one
    /// stood here: `assert!(BUNDLE.contains("/api/boxes/") && BUNDLE.contains("/api/pr/"))`, with no
    /// message — it pinned the broken spelling in place, and the only edit that could fail it was
    /// the fix. So this replaces it by *running* the function rather than matching it, and checking
    /// the answer against the router's real table:
    ///
    /// 1. a real [`crate::queue::Row`], its fields spelled as `queue` spells them — `name` is
    ///    `#412` and `repo` is the registered repo's id, which is what makes the `#`-strip in
    ///    `shapeUrl` and the `:id` in the route load-bearing rather than incidental;
    /// 2. the bundle the browser is served, evaluated in node, `shapeUrl` called on that row;
    /// 3. the answer matched, segment by segment, against the `.route(…)` literals read out of
    ///    `bin/skein-server.rs`.
    ///
    /// Fails if the URL moves on either side: change `shapeUrl` back and no route matches; rename
    /// the route and no route matches; drop the `#`-strip and `/api/repos/web/review/%23412/shape`
    /// is not what the handler's `Path<(String, u64)>` can bind.
    ///
    /// Skips where node is absent, exactly as `the_cockpit_bundle_is_not_stale` does — this is a gate
    /// on a developer machine and CI, not a runtime property of the binary.
    #[test]
    fn the_change_view_asks_a_url_this_router_answers() {
        use crate::queue::{Need, Row, Source};
        // The two ways a change arrives, spelled the way the queue really spells them: a pull
        // request's `name` is `format!("#{}", pr.number)` and its `repo` is the registered repo's
        // `id` (`queue.rs:231-234`); a box's `name` is the sandbox's (`queue.rs:208-211`). The `#`
        // is written as the queue writes it — interpolated from the number — so a test that stopped
        // exercising the strip would have to change this line to do it.
        let number = 412u64;
        let row = |source, name: String| Row {
            source,
            need: Need::YourAttention,
            repo: "web".into(),
            name,
            headline: String::new(),
            state: String::new(),
            waiting_secs: None,
            url: String::new(),
            fix: String::new(),
        };
        let rows = [
            row(Source::PullRequest, format!("#{number}")),
            row(Source::Box, "web-main".into()),
        ];
        let asked: Vec<String> = rows
            .iter()
            .map(|row| {
                let json = serde_json::to_string(row).unwrap();
                let script = format!("{BUNDLE}\nprocess.stdout.write(shapeUrl({json}));");
                let out = match std::process::Command::new("node")
                    .args(["-e", &script])
                    .output()
                {
                    Ok(out) => out,
                    Err(_) => return String::new(),
                };
                assert!(
                    out.status.success(),
                    "the bundle would not run: {}",
                    String::from_utf8_lossy(&out.stderr)
                );
                String::from_utf8(out.stdout).unwrap()
            })
            .collect();
        if asked.iter().any(String::is_empty) {
            crate::testutil::skip("no node on this machine to run the cockpit bundle");
            return;
        }

        // The router's own table, read out of the binary's source. Assembled rather than written,
        // because `docs/parity.md` counts this project's routes by grepping for the router's call
        // and a literal here would add to that count (`tests/parity_numbers.rs`).
        let call = concat!(".", "route", "(\"");
        let server = include_str!("bin/skein-server.rs");
        let routes: Vec<&str> = server
            .match_indices(call)
            .filter_map(|(i, _)| {
                let rest = &server[i + call.len()..];
                rest.find('"').map(|end| &rest[..end])
            })
            .filter(|path| path.starts_with("/api/"))
            .collect();
        assert!(
            routes.len() > 60,
            "the route scan found {} routes — it stopped reading the router, so what follows \
             proves nothing",
            routes.len()
        );

        for url in &asked {
            let matched = routes.iter().any(|route| {
                let r: Vec<&str> = route.split('/').collect();
                let a: Vec<&str> = url.split('/').collect();
                r.len() == a.len()
                    && r.iter()
                        .zip(a.iter())
                        .all(|(rs, as_)| rs.starts_with(':') || rs == as_)
            });
            assert!(
                matched,
                "the change view asks {url}, and no route this server registers answers it — so \
                 the tab 404s and reports it as \"the change could not be read\""
            );
        }
        // And the join the route's extractor makes: `api_pr_shape` binds `Path<(String, u64)>`, so
        // the number segment has to be a number. The row's name is `#412`.
        assert!(
            asked[0].ends_with(&format!("/{number}/shape")),
            "the pull request's number reached the URL as {}, which the handler's `u64` cannot bind",
            asked[0]
        );
    }

    /// **There is no rebuild button, and no condition under which one appears** (SKEIN-467).
    ///
    /// It used to be a gate: `api_health` merged a `deployment` object onto the health report and
    /// the page branched on its `in_fleet`, hiding the destructive row until the server said
    /// pressing it was safe. That gate was the module's own failure mode at its worst — nothing in
    /// the language connected the two names, so renaming one end did not break loudly; it silently
    /// restored a button whose press destroys the machine skein is running on and then reports
    /// `resize failed:` at the moment the destroy irreversibly worked.
    ///
    /// The gate is gone because the thing it was gating on is (SKEIN-576), and this is the stronger
    /// property in its place: **the control is absent, not conditional.** `docs/architecture.md`
    /// §7.5 puts fleet lifecycle outside the fleet permanently — create and destroy both kill
    /// skein, and a resize is a destroy followed by a create — so there is no deployment for the
    /// button to be safe in and nothing for the page to ask.
    ///
    /// Asserted as absences on both sides, which is what makes this hold: a button reintroduced
    /// with any wiring at all fails here, and so does a server that starts reporting a deployment
    /// for a page to branch on again.
    ///
    /// Fails if the rebuild control comes back, if nothing stands in its place, or if the two
    /// sentences `/api/fleet/plan` carries stop being sent or stop being read.
    #[test]
    fn no_deployment_decides_whether_the_fleet_can_be_rebuilt_from_here() {
        let server = include_str!("bin/skein-server.rs");
        // The wire the gate ran on, gone at the source.
        for sent in ["\"deployment\"", "\"in_fleet\"", "\"implies\""] {
            assert!(
                !server.contains(sent),
                "/api/health is reporting {sent} again — a page that can read where skein runs is \
                 a page that can offer a rebuild on the strength of it"
            );
        }
        // The affordance itself, absent — the id, the button, and the handler that pressed it.
        for gone in [
            r#"id="set-resize-field""#,
            r#"id="set-resize""#,
            "function resizeFleet",
            "applyDeployment",
        ] {
            assert!(
                !INDEX.contains(gone),
                "`{gone}` is back in the page: pressing it destroys the sandbox this page is served \
                 from, and the last thing rendered would be `resize failed:`"
            );
        }
        // And something still stands in its place, or the pane is a dead end: a person who came
        // here to change fleet memory has to leave knowing what to do instead.
        assert!(
            INDEX.contains(r#"id="set-resize-infleet""#),
            "nothing takes the rebuild row's place, so the pane offers nothing at all"
        );
        // The two sentences `/api/fleet/plan` has always carried. `why` is why sbx could not be
        // asked — in-fleet the correct answer rather than a fault — and without it an `exists` of
        // null reads as a screen that failed to load.
        assert!(
            server.contains("\"why\": skein::sbx::fleet_failure()"),
            "the plan stopped carrying why sbx could not be asked"
        );
        assert!(
            INDEX.contains("p.why"),
            "the page still never renders why sbx could not be asked"
        );
        assert!(
            server.contains("\"lifecycle_refusal\"") && INDEX.contains("p.lifecycle_refusal"),
            "the in-fleet row has no lines to run on the host, which is the whole of what it can \
             usefully say"
        );
    }

    /// **There is no create-fleet dialog, and no field on the wire that could open one**
    /// (SKEIN-627).
    ///
    /// The same shape as the rebuild button above, arrived at the same way. The dialog opened on
    /// exactly one condition — `fleetPlan.exists === false`, which the page's own comment called
    /// "the only state that means 'there is none'" — and `/api/fleet/plan` fed that from
    /// `fleet::fleet_exists`. In-fleet that function is `(sandbox == fleet_sandbox()).then_some(true)`:
    /// `Some(true)` for the fleet this process is standing in, `None` for every other name, and no
    /// `Some(false)` at all. Skein's own fleet exists by construction, and it cannot see the machine
    /// to answer about a second one — so the dialog could not open, and nobody ever met it.
    ///
    /// Asserted as absences on both sides, because that is what stops it coming back by halves: a
    /// page that grows the markup again fails here even with no server field to open it on, and a
    /// server that starts sending `exists` again fails here even with no dialog to read it.
    ///
    /// **`/api/fleet/create` is deliberately NOT in this list.** Creating a *differently-named*
    /// sandbox is still coherent — the warden is on the host with the capability — and the owner's
    /// decision on SKEIN-627 declined that reading without refuting it. What went is the sizing
    /// surface, not the route.
    ///
    /// These are substring matches over the page, so the page's own **prose** must not spell them
    /// either. That is not an accident to work around: the first draft of this test failed on a
    /// comment explaining the deleted gate, and a comment that spells out a live-looking branch is
    /// the thing a reader has to check anyway. Describe what went; do not write it out.
    #[test]
    fn no_field_on_the_wire_can_offer_to_create_the_fleet_skein_is_inside() {
        let server = include_str!("bin/skein-server.rs");
        assert!(
            !server.contains("\"exists\": exists"),
            "/api/fleet/plan is reporting `exists` again — the only value it can carry in here is \
             `Some(true)`, and the one branch that ever read it opened a dialog for a state that \
             cannot arise"
        );
        // The dialog itself: the modal, the handlers that opened and submitted it, and the fields
        // whose only reader was `openFleetNew`.
        for gone in [
            r#"id="fleetnew""#,
            "function openFleetNew",
            "function closeFleetNew",
            "async function createFleet",
            r#"id="fn-memory""#,
            r#"id="fn-mem-of""#,
            r#"id="fn-cpu-of""#,
        ] {
            assert!(
                !INDEX.contains(gone),
                "`{gone}` is back in the page: it belongs to a dialog that opens on \
                 `exists === false`, which an in-fleet skein can never report about its own fleet"
            );
        }
        // And the launch is not gated on it. This is the half that mattered to a person: with the
        // gate still in the page and the field gone from the wire, `fleetPlan.exists === false`
        // would simply never be true — silently right, for a reason nothing states.
        assert!(
            !INDEX.contains("fleetPlan.exists"),
            "launching a box still branches on `fleetPlan.exists`, which no longer exists on the \
             wire — a gate that is passed because its input is missing is not a gate"
        );
    }

    /// The CPU controller is delegated wherever a weight is written, or the weight lands nowhere.
    ///
    /// `cpu.weight` on a child exists only if the parent handed the controller down, and a write to
    /// a file that does not exist is silently nothing — so a delegation that stopped mentioning
    /// `+cpu` would leave containers weighing whatever the default is, with the launcher looking
    /// like it had set them.
    #[test]
    fn the_cpu_controller_reaches_the_cgroup_whose_weight_is_set() {
        let launcher = include_str!("box-session.sh");
        assert!(
            launcher.contains(r#"echo "+memory +pids +cpu" > "$1/cgroup.subtree_control""#),
            "the cpu controller is not delegated, so `cpu.weight` has nowhere to land"
        );
        assert!(
            launcher.contains("/sys/fs/cgroup/skein/containers/cpu.weight"),
            "nothing weighs the containers against the boxes"
        );
    }

    /// The launcher covers the two shared `/run` directories, and says why it leaves the third.
    ///
    /// Every cover in that script is about a path skein chose; `/run` is not one, and everything
    /// under it is shared because every box is the same uid. `/run/user/<uid>` is empty today, which
    /// is exactly when closing it costs nothing, and `/run/secrets` is world-writable.
    ///
    /// `/run/docker.sock` is deliberately **not** covered — skein points the sandbox's dockerd at
    /// the workload cgroup so containers a box starts are accounted for — and the assertion is that
    /// the script keeps *saying* so, because an unexplained gap in a list of covers reads as an
    /// oversight and gets closed by whoever notices it next.
    #[test]
    fn the_launcher_covers_what_run_shares_and_names_what_it_does_not() {
        let launcher = include_str!("box-session.sh");
        assert!(
            launcher.contains(r#"binds+=(--tmpfs "$run_user")"#),
            "the per-user runtime directory is shared by every box and is not covered"
        );
        assert!(
            launcher.contains("binds+=(--tmpfs /run/secrets)"),
            "/run/secrets is world-writable and shared by every box"
        );
        // Under a privileged check, like every other cover — the workshop box is the escape hatch,
        // and it is the one box that has to keep reaching the fleet. Found by walking back from the
        // cover to the nearest guard, because the script has several and splitting on the first one
        // asserts something about a different block.
        let at = launcher.find("run_user=").expect("checked above");
        let guard = launcher[..at]
            .rfind("SKEIN_BOX_PRIVILEGED")
            .expect("the cover is not under any privileged check at all");
        assert!(
            launcher[guard..at].contains("!= \"1\""),
            "the /run covers are under the wrong side of the privileged check, so the workshop box \
             gets them and every ordinary box does not"
        );
        assert!(
            launcher.contains("/run/docker.sock` — **deliberately left reachable**"),
            "the one thing under /run that is left open no longer says that it is on purpose"
        );
    }

    /// **The runtime directory the cover names is the real one** (SKEIN-572).
    ///
    /// `$SKEIN_RUNTIME_DIR` exists so `tests/isolation_bwrap.rs` can plant a file under the runtime
    /// directory and ask a real namespace whether it is there — which against `/run/user/1000`
    /// would mean writing into the live fleet's own runtime directory, beside running agents' inbox
    /// sockets. A seam like that is only honest while the default is still the path production
    /// uses, and the whole peer-network suite runs through the override: if the fallback drifted,
    /// every one of those tests would keep passing over a directory no box has.
    ///
    /// **What would make this fail:** changing the fallback, or making the variable mandatory —
    /// which would leave production covering nothing, since nothing sets it.
    #[test]
    fn the_run_cover_still_defaults_to_the_real_runtime_directory() {
        let launcher = include_str!("box-session.sh");
        assert!(
            launcher
                .contains(r#"runtime_dir="${SKEIN_RUNTIME_DIR:-/run/user/$(id -u 2>/dev/null || echo 0)}""#),
            "the runtime directory is no longer `/run/user/<uid>` when nobody overrides it, so the \
             cover and every test of it are aimed somewhere production never looks"
        );
    }

    /// **Discovery and transport are never independently switchable** (§9.5 R11, SKEIN-572).
    ///
    /// The mount test in `tests/isolation_bwrap.rs` is the real one — it runs bwrap and asks the
    /// kernel. This is the cheap guard beside it, and it asserts the thing that argv can actually
    /// see: that both binds are decided **in one block**, from one variable, rather than 800 lines
    /// apart the way they were when a `--tmpfs` silently severed a channel a comment upstream still
    /// described as shared.
    ///
    /// **What would make this fail:** putting `.claude/sessions` back in `share_paths`, where it
    /// would be bound whatever the switch says and nothing would tie it to the socket again.
    #[test]
    fn the_peer_networks_two_halves_are_decided_in_one_place() {
        let launcher = include_str!("box-session.sh");
        assert!(
            !launcher.contains(r#"share_paths+=(".claude/sessions")"#),
            "the session registry is shared from `share_paths` again, which is the one place that \
             cannot see whether the socket directory came with it"
        );
        let at = launcher
            .find("peer_socks=")
            .expect("the peer network block is gone, so nothing binds the socket directory back");
        let block = &launcher[at..];
        let end = block.find("printf 'SKEIN_PEERS").unwrap_or(block.len());
        let block = &block[..end];
        for bind in [
            r#"binds+=(--bind "$HOME/.claude/sessions" "$HOME/.claude/sessions")"#,
            r#"binds+=(--bind "$peer_socks" "$peer_socks")"#,
        ] {
            assert!(
                block.contains(bind),
                "`{bind}` is not decided with the other half of the peer network, so the two can \
                 drift apart again"
            );
        }
        // And the box carries the answer out, because the switch lives in `repos.json` and changes
        // no byte of this script — so `launcher_revision` cannot see it and `cover_is_current`
        // would call a box current while it ran the mount it was born with.
        assert!(
            launcher.contains(r#"printf 'SKEIN_PEERS %s\n' "$peers""#),
            "nothing reports which side of the switch this box was born on, so flipping it would \
             leave the board saying the box is under the current cover when it is not"
        );
    }

    /// The workshop switch names what it grants, in the launcher and on the switch alike (§9.5 R9).
    ///
    /// The risk of this one is not that somebody turns it on by accident — it is that they turn it
    /// on for a reason and then forget which box carries it. So the box says what it is at every
    /// start, and the assertion is that the wording keeps naming the grants rather than drifting
    /// into "workshop box" and a shrug. Two of the three cannot be discovered by using the box: it
    /// holds the fleet agent's token, and the mount cover is off for it — which is what the guards
    /// on the git-token directory and the resize archive lean on.
    ///
    /// **This test is about WORDS, and it was read for years as though it were about delivery**
    /// (SKEIN-846). It greps `box-session.sh` for a line and checks what that line says; it has
    /// nothing to say about whether anybody ever reads it, and for the whole of its green life
    /// nobody did — both production callers pipe the launcher's stderr and drop it on success. The
    /// wording is still worth pinning, so the check stays as it is; what changed is that it no
    /// longer stands alone. Delivery is asserted where delivery happens: `tests/fleet_launch.rs`
    /// starts real boxes under real bwrap and reads the banner back out of the tmux pane and out of
    /// a real `skein attach`'s stderr, and `tests/isolation_bwrap.rs` reads it back through
    /// `fleet::notices_from_launch`, the parser skein itself uses. If those go, this one is back to
    /// proving that a string exists in a file.
    #[test]
    fn the_workshop_switch_says_what_it_grants() {
        let launcher = include_str!("box-session.sh");
        let banner = launcher
            .lines()
            .find(|l| l.contains("WORKSHOP box"))
            .expect("a privileged box announces itself at every start");
        for term in [
            "every box's files",
            "fleet scope",
            "fleet agent token",
            "mount cover is off",
        ] {
            assert!(
                banner.contains(term),
                "the start-up banner no longer names `{term}`: {banner}"
            );
        }
        // And the switch itself, where the decision is actually made.
        assert!(
            INDEX.contains("holds the fleet agent token"),
            "the switch offers a grant it does not name"
        );
        assert!(
            INDEX.contains("the mount cover is off for it"),
            "the one term a person cannot discover by using the box is not on the switch"
        );
    }

    /// Setting up reads and writes the names the server uses, on both surfaces.
    ///
    /// This is the first surface `/v2` has that *writes*, and a write that names a field wrongly is
    /// worse than a read that does: a read renders nothing and a write is accepted, silently
    /// dropping what it did not spell right — `AddRepoReq` takes `#[serde(default)]` on three of its
    /// four fields, so `stor` instead of `store` is a repo whose shared-data folder is quietly
    /// skein's own.
    ///
    /// **`store` is now the field this surface must NOT send** (SKEIN-535). The route refuses it
    /// outright, because `add_repo` scaffolds a `.claude` tree wherever an absolute path points and
    /// the API token that reaches this route is printed into every cockpit URL. So the pairing this
    /// test exists for is unchanged and its subject is inverted: the page and the server have to
    /// agree about the field's ABSENCE, and a form that sent it again would get a 400 that the
    /// person reads as "adding a repo is broken".
    ///
    /// Asserted rather than deleted. Dropping the `store` row would have left the surface free to
    /// grow the field back with nothing to notice.
    #[test]
    fn the_setup_surface_writes_the_fields_the_server_reads() {
        // Add a repo: the one field this surface sends, and the two it reads back.
        //
        // **Scoped to the request literal, because the file-wide version could not fail.** This was
        // `V2.contains("source,") || V2.contains("source:")`, and a queue row a hundred and eighty
        // lines away reads `{ source: row.dataset.source, … }` — so the assertion was satisfied by
        // code that has nothing to do with this form, and would have passed if the form sent no
        // `source` at all. Measured, not reasoned: renaming the form's field to `src:` left it
        // green. It had been that way since before SKEIN-535 and survived the round that lifted
        // `store` out of the loop beside it.
        //
        // So both halves now read the body the form actually sends: the bytes between
        // `JSON.stringify({` and the `})` that closes it, inside the `/api/repos` request. Not the
        // whole request — a first attempt sliced that, and swept up the `// No \`store\`` comment
        // sitting in it, so the store assertion fired on a comment saying the field was gone.
        let body = {
            let request = V2
                .split_once(r#"fetch("/api/repos", {"#)
                .expect("the add-a-repo form no longer posts to /api/repos")
                .1;
            let arg = request
                .split_once("JSON.stringify({")
                .expect("the add-a-repo form sends no JSON body")
                .1;
            &arg[..arg.find("})").expect("the request body is never closed")]
        };
        // The field the route refuses (SKEIN-535), named on its own so it fails in its own words.
        assert!(
            !body.contains("store"),
            "the add-a-repo form sends `store` again, and POST /api/repos refuses it (SKEIN-535) — \
             adopting an existing store is `skein add --store <path>` from the CLI now: {{{body}}}"
        );
        // **The exact shape, because a key is not a value.** `body.contains("source")` cannot tell
        // `{ source }` from `{ src: source }` — the second sends the field under a name the server
        // does not read, and `AddRepoReq` would take the default for it. That is precisely the
        // silent-drop this test was written about, so the one field it sends is pinned whole.
        // Adding a field here is meant to fail: it is the reviewable event.
        assert_eq!(
            body.trim(),
            "source",
            "the add-a-repo request body is no longer exactly `{{ source }}` — if a field was added \
             deliberately, check the server reads it under that name before updating this"
        );
        assert!(
            !V2.contains(r#"getElementById("store")"#) && !V2.contains(r#"id="store""#),
            "the shared-data folder field is back on /v2, and the route refuses `store` (SKEIN-535)"
        );
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
        //
        // **Both boards now**, which is the whole of SKEIN-106. `/v2` never had Browse; `/` had it
        // on three fields and it needed the HOST's native dialog — a display skein-in-fleet does not
        // have, on a filesystem it is not standing on. It was already unusable over Tailscale, where
        // the advice was "keep typing", so typing became the path and this check is what makes
        // typing bearable.
        //
        // **`/v2` no longer has one, and that is a consequence rather than a second decision**
        // (SKEIN-535). The probe answers a field, and on `/v2` the shared-data folder was the only
        // field it ever answered — the source field is a remote and was deliberately never wired
        // (SKEIN-806). Deleting the field left `resolves`/`checkLater` with no caller, so they went
        // with it. `/` still probes, because the SSH key path is a host path skein really does read.
        //
        // The `pick-path` half is asserted of BOTH boards, because that one is about Browse rather
        // than about any particular field, and it must not come back on either.
        assert!(
            INDEX.contains("/api/path?p="),
            "/ does not ask what a typed path resolves to"
        );
        assert!(
            INDEX.contains("found.resolved") && INDEX.contains("found.kind"),
            "/ does not read the path check's answer"
        );
        assert!(
            !V2.contains("/api/path?p="),
            "/v2 probes a typed path again, but it has no path field to probe — the shared-data \
             folder went with SKEIN-535 and the source field is a remote (SKEIN-806). If a path \
             field came back, this assertion is the wrong thing to fix: see parity §7"
        );
        for (board, page) in [("/", INDEX), ("/v2", V2)] {
            assert!(
                !page.contains("pick-path"),
                "Browse came back on {board} — it needs a host display the in-fleet skein cannot \
                 have (parity §7)"
            );
        }
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
            crate::testutil::skip("no node on this machine to rebuild the cockpit bundle");
            return;
        };
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// The first-run checklist reads the warden by the name the server sends it under.
    ///
    /// A wire assertion, and it is load-bearing rather than tidy: creating the fleet is what a first
    /// Launch does and it goes only through the warden, so a checklist that cannot see that field
    /// says "ready" and then hands somebody a 500. That happened. If the field is ever renamed, this
    /// fails here rather than on somebody's first afternoon.
    #[test]
    fn the_first_run_checklist_reads_the_warden_the_server_reports() {
        assert!(
            INDEX.contains("h.warden"),
            "the checklist does not read the warden, so a first run can reach Launch with none"
        );
        // `warden_report` ASKS a warden, which a wire-shape assertion has no need to do — and
        // unpinned it asks whatever warden the machine running the suite can reach, which
        // `warden_client` refuses in a test process now (SKEIN-762). Pinned where nothing listens:
        // the shape of the check is the same on the reachable and unreachable arms, which is what
        // makes it safe to assert it from the arm that costs nothing.
        let _g = crate::testutil::env_lock();
        let _warden = crate::testutil::no_warden();
        let json = serde_json::to_string(&crate::health::warden_report()).unwrap();
        assert!(
            json.contains("\"level\"") && json.contains("\"fix\""),
            "the page reads `.level` and `.fix` off this check and the wire carries neither: {json}"
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
    /// 1. **The words.** Speaking `headline` is the whole point — "PROJ-S6 wants permission. Run
    ///    rm -rf build?" is actionable where "PROJ-S6 needs a decision" is only a reason to go and
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
