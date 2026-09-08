//! `cargo test` runs the suites that prove the **page**, not only the ones that prove the API.
//!
//! The files under `tests/ui/` were the only thing checking the cockpit, and nothing ran them.
//! They rotted, and while they were rotting the main board shipped **blank** for 160 commits: two
//! top-level `slug` declarations collided, the browser abandoned the whole inline script, and every
//! Rust test stayed green throughout (SKEIN-110). By the time anyone ran them again, four crashed on
//! functions that had moved, two had fixtures describing a data path that had been rewritten
//! underneath them, and one was failing on a real product race nobody had seen (SKEIN-116).
//!
//! So the point of this file is not the assertions — every suite carries its own. It is that they
//! are **invoked**, by the command people already run.
//!
//! # Two tiers, because they cost different things
//!
//! **The node suites** need `node` and nothing else. `node` is already a hard requirement of
//! `cargo test` here — `cockpit::tests::the_cockpit_bundle_is_not_stale` shells out to it — so these
//! run every time and a failure is a failure.
//!
//! **The browser suites** need Playwright's chromium and its system libraries, roughly 150 MB, which
//! is not a reasonable thing to demand of somebody building skein. They run when it is installed and
//! are skipped when it is not.
//!
//! # The skip was right for a person and wrong for CI
//!
//! Those are different questions and for a long time they had one answer. A contributor should not
//! need a 150 MB download to run `cargo test`; **CI should**, and did not — `.github/workflows/ci.yml`
//! named neither playwright nor chromium, so [`chromium_ready`] answered `false` on every run this
//! repository has ever had and the whole browser tier was skipped, the two largest included
//! (SKEIN-567). The intent stated below — that a dead page cannot pass every other check — therefore
//! held for the node tier alone, which is the half that does not open a page.
//!
//! CI installs it now, and the cost is the download rather than the run: cached, the whole browser
//! tier is about a minute (measured at `9d8ab710`, 2026-09-07: 61.3s with four lanes, 62.3s with
//! eleven — `review.mjs` is the floor and the lanes are already past it).
//!
//! # The skip is stated, not silent
//!
//! A skipped check that says nothing is the failure mode this whole file exists because of. `cargo
//! test` hides stdout for a passing test, so a skip printed there is a skip nobody reads — which is
//! how a whole directory of them went unrun. Instead the browser tier is **one test whose name is
//! the report**: it passes either way, and the run says which happened, in the list of test names
//! everybody already looks at. `tests/ui/README.md` has the setup.

mod common;

use common::skip;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

/// Needs only node. `lift.mjs` is absent on purpose — it is the shared helper the others import,
/// not a suite, and running it asserts nothing.
const NODE_SUITES: [&str; 24] = [
    "attach",
    "budget",
    "conversation",
    "foreign",
    "gitgate",
    // The harness's own two escape hatches, and the two pins they escape. Here rather than in the
    // browser tier because it drives `skein-server` over HTTP and opens no page — and because the
    // thing it guards, `harness/server.mjs`, is what every suite in BOTH lists starts its server
    // with (SKEIN-624).
    "hatches",
    // The leak check reading `/proc`, which needs no page either — and which starts a process that
    // looks exactly like a leaked one for as long as it takes to ask about it (SKEIN-687).
    "leakcheck",
    "loginban",
    "overlays",
    "provenance",
    "rail",
    "resources",
    "revbadge",
    "review_return",
    "screenhalf",
    "stackread",
    "stacksteps",
    "stream",
    "substrate",
    "train",
    "trainpanel",
    "tabs",
    "undo",
    "voice",
];

/// Needs Playwright's chromium as well.
///
/// `connections` is here rather than in the node tier because the thing it measures does not exist
/// outside a browser: the six-connection-per-origin cap on HTTP/1.1 is a BROWSER behaviour, and
/// diagnosing SKEIN-366 from code constants and curl was not proof of it — curl has no such cap.
const BROWSER_SUITES: [&str; 7] = [
    "actfail",
    "connections",
    "onboarding",
    "panecover",
    "review",
    "smoke",
    "updatepane",
];

fn repo() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

/// Run one suite. `None` when node itself could not be started.
fn run(suite: &str) -> Option<(bool, String)> {
    let out = Command::new("node")
        .arg(format!("tests/ui/{suite}.mjs"))
        // The `skein-server` this `cargo test` has already built — compiling this test file is what
        // forces cargo to build it (`env!` would not resolve otherwise), so the path exists before
        // any suite runs. Handing it over is what stops the browser suites running `cargo build`
        // themselves: a second cargo inside this one re-takes the build-directory lock — shared, on
        // a busy box, with every other build — and re-walks the dependency graph, and that stall
        // plus burst of load right as the suites' timeouts start ticking is what made review.mjs
        // fail its ownership check about one workspace run in four, never standalone (SKEIN-119).
        // Run by hand, `node tests/ui/<suite>.mjs` has no such variable and builds for itself.
        .env("SKEIN_SERVER_BIN", env!("CARGO_BIN_EXE_skein-server"))
        .current_dir(repo())
        .output()
        .ok()?;
    let said = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    Some((out.status.success(), said))
}

/// How many suites are in flight at once.
///
/// Bounded, not "start all of them". A node suite is a node process; a browser suite is a chromium
/// and a `skein-server` besides. SKEIN-119 is the reason for the bound — what made `review.mjs`
/// fail about one workspace run in four was a *burst* of load arriving while the suite's own
/// timeouts were ticking — and it is an argument about the burst, not about running one at a time:
/// the whole node tier started at once on four cores is that burst, a lane per core on an
/// eleven-core box is not.
///
/// `SKEIN_UI_LANES=1` puts it back to one at a time. That is how the "before" half of the
/// measurement on [`run_all`] was taken, and it is the first thing to try when a suite fails only
/// ever in a full run and never on its own.
fn lanes(total: usize) -> usize {
    let asked = std::env::var("SKEIN_UI_LANES")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or_else(|| std::thread::available_parallelism().map_or(4, |n| n.get()));
    asked.min(total).max(1)
}

/// Every suite, run [`lanes`] at a time, with the ones that failed reported in list order.
///
/// `None` when node itself could not be started.
///
/// # Why they may be concurrent
///
/// Nothing is shared between any two of them. Each builds its own fixture with `mkdtempSync`, and
/// each binds its server on port 0 and asks the kernel which port it got — so there is no fixed
/// path and no fixed port for two of them to collide on, and no suite reads what another wrote:
///
/// ```text
/// grep -c 'mkdtempSync' tests/ui/*.mjs
/// grep -n 'listen(0' tests/ui/*.mjs
/// ```
///
/// # Why the failure output is still readable
///
/// Each child is captured whole, by its own `Command::output()`, before a byte of it is printed —
/// what makes concurrent test output unreadable is two children sharing one pipe, and these share
/// nothing. A worker writes the slot at its own index in `suites`, so the report comes out in the
/// order the list is written in, whatever order the lanes happened to finish in.
///
/// # What it bought
///
/// The browser tier is the whole cost of this file. **What follows is a measurement, so it is
/// dated and it names what it covered** rather than counting it: taken at `f9647ae1` (2026-08-27)
/// on an 11-core box, over the browser tier as it stood that day — `actfail` 48.3s, `connections`
/// 14.8s, `onboarding` 19.1s, `review` 80.6s, `smoke` 30.8s, timed one at a time, 193.6s in all —
/// against 16.7s for the node tier, which runs concurrently with them anyway. So
/// `cargo test --test browser_suites` was 195.12s, and concurrently it is the longest single suite
/// plus change:
///
/// ```text
/// SKEIN_UI_LANES=1 cargo test --test browser_suites   193.94s
///                  cargo test --test browser_suites    81.96s
/// ```
///
/// (195.12s for the same command before this function existed, so the knob costs nothing.)
///
/// `review.mjs` at 80.6s was the floor and 81.96s is one lane's worth above it: splitting that one
/// suite is the only thing left that would move this number.
///
/// `updatepane` joined the browser tier the day after (`3640100d`), so the serial total is short by
/// a suite and the concurrent one is not — which is the argument for the knob, restated by
/// arithmetic. The enumeration above is what was timed; [`BROWSER_SUITES`] is what runs, and the
/// two are meant to be compared by eye rather than reconciled into a number here, because a number
/// here is a fact stated twice and the second copy is the one that rots
/// ([`no_prose_in_this_file_counts_what_the_lists_already_carry`]).
fn run_all(suites: &[&str]) -> Option<Vec<String>> {
    let next = AtomicUsize::new(0);
    let done: Mutex<Vec<Option<(bool, String)>>> = Mutex::new(vec![None; suites.len()]);
    std::thread::scope(|scope| {
        for _ in 0..lanes(suites.len()) {
            scope.spawn(|| loop {
                let at = next.fetch_add(1, Ordering::Relaxed);
                let Some(suite) = suites.get(at) else { return };
                let got = run(suite);
                // Poisoning is uninteresting: the vector is slots with one writer each, and a
                // panicking lane has left no half-written invariant behind it.
                done.lock().unwrap_or_else(|e| e.into_inner())[at] = got;
            });
        }
    });
    let done = done.into_inner().unwrap_or_else(|e| e.into_inner());
    let mut broken: Vec<String> = Vec::new();
    for (suite, got) in suites.iter().zip(done) {
        // `None` is node failing to start, which is one fact about the machine rather than one
        // about this suite. The caller says it once instead of once per suite.
        let (ok, said) = got?;
        if !ok {
            broken.push(format!("── tests/ui/{suite}.mjs ──\n{}", tail(&said)));
        }
    }
    Some(broken)
}

/// The tail of a suite's output — the part that says what failed.
///
/// Whole output would bury the answer: a happy `smoke` printed 84 lines when it was last measured
/// (2026-09-08, at 62 checks), and it grows by a line with every check anyone adds. The failures are
/// at the end, and every one of these suites ends with its own summary.
///
/// # The number is derived, not chosen
///
/// It was 25, then 40, each time by picking a size that looked big enough — and 40 was not, because
/// the thing being sized against had no bound. `review.mjs` failed 62 of 82 checks in CI and printed
/// 62 names after the server log; the window held the last 38 of them, so the log, every message and
/// even the `62 of 82 checks failed:` header fell out, and the result was read as a suite that had
/// died before its first check (SKEIN-623). No window survives that, because the list grows with the
/// failures and the window does not.
///
/// So `ledger.report` in `tests/ui/harness/browser.mjs` was given a **bounded closing block**, and
/// this is sized against it. Counting up from the last line of a failing suite:
///
/// ```text
///  2  the suite's own last words (`screenshot: …`, `fixture kept for inspection: …`)
/// 13  report's closing block — the count, and the first 5 failures WITH their messages
///     (28 at its own worst case: `whole: true` messages capped at 3 lines plus an ellipsis)
/// 27  the server log block — a blank, `server log:`, and report's own 25-line slice
/// ── 42 common, 57 worst
/// ```
///
/// 60 is that worst case with a little room. Anything left over goes to the head of the `✗` list,
/// which is the part it costs nothing to lose. **`tests/ui/harness/browser.mjs` is the other half of
/// this arithmetic**, and
/// [`the_reason_a_check_failed_survives_the_window_however_many_failed`] fails if either half moves
/// without the other.
fn tail(said: &str) -> String {
    let lines: Vec<&str> = said.lines().collect();
    lines[lines.len().saturating_sub(60)..].join("\n")
}

/// The reason a check failed reaches CI's log even when far more checks failed than the window has
/// lines — and so does the count, and the server's log beside it.
///
/// **Both halves of the arithmetic on [`tail`] are exercised, by running the real
/// `harness/browser.mjs`** rather than by restating its output format here. A copy of the format in
/// this file would go stale in the one direction that matters: green while the thing it describes
/// has stopped being true. No browser and no server — `ledger` is arithmetic over an array — so this
/// costs a node start.
///
/// # What makes each assertion fail
///
/// - the count: put `report` back the way it was (server log, then count, then bare names, no
///   closing block). 62 names printed last are what fills the window, so the count goes with them.
/// - the first reason: keep the closing block but print `✗ name` in it without the message — which
///   is the shape the whole list already had, and the one it would drift back to.
/// - the server log: put [`tail`] back to 40. `server log:` is the 41st line from the end in the
///   common case and the 56th in the `whole: true` one — measured, not estimated — so at 40 the log
///   loses its own label and the top of report's slice with it.
///
/// All three were done, and the assertion each was aimed at is the one that fired.
///
/// The `whole: true` half is not a hypothetical shape: `onboarding.mjs` asks for it, because what it
/// checks is what a first run SAYS and the evidence is the part after the colon.
#[test]
fn the_reason_a_check_failed_survives_the_window_however_many_failed() {
    // 62 of 82, the shape review.mjs really had in run 34122669315 (SKEIN-621, SKEIN-623): more
    // failures than any window holds, and the first one is the one worth reading.
    let probe = r#"
        import { pathToFileURL } from "node:url";
        const { ledger } = await import(pathToFileURL(process.env.HARNESS).href);
        const whole = process.env.WHOLE === "1";
        const { check, report } = ledger({ whole });
        for (let n = 1; n <= 82; n++) {
          await check(`check number ${n}`, () => {
            if (n > 62) return;
            throw new Error(`reason number ${n}` + (whole ? "\nand four\nmore\nlines\nof it" : ""));
          });
        }
        report({ log: () => Array.from({ length: 40 }, (_, i) => `server log line ${i + 1}`).join("\n") });
        console.log("screenshot: /var/tmp/nowhere/failure.png");
        console.log("fixture kept for inspection: /var/tmp/nowhere");
    "#;
    for whole in ["0", "1"] {
        let Ok(out) = Command::new("node")
            .args(["--input-type=module", "-e", probe])
            .env("HARNESS", repo().join("tests/ui/harness/browser.mjs"))
            .env("WHOLE", whole)
            .current_dir(repo())
            .output()
        else {
            return skip("no node on this machine, so a suite's report cannot be run");
        };
        let said = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            out.status.success(),
            "the probe against tests/ui/harness/browser.mjs did not run (whole={whole}):\n{said}"
        );
        let window = tail(&said);
        let at = format!("whole={whole}\n{window}");

        // Without this the rest proves nothing: a probe whose whole output fits in the window would
        // pass every assertion below while saying nothing about truncation.
        assert!(
            !window.contains("  ok    check number 63"),
            "the probe's output fits in the window, so this proves nothing about truncation:\n{at}"
        );
        assert!(
            window.contains("62 of 82 checks failed"),
            "the count did not survive the window — the log reads as a suite that never ran:\n{at}"
        );
        assert!(
            window.contains("reason number 1"),
            "the first failure's reason did not survive the window, which is the whole defect:\n{at}"
        );
        assert!(
            window.contains("\nserver log:\nserver log line 16"),
            "the window no longer holds the whole server log beside the diagnosis:\n{at}"
        );
    }
}

/// Is Playwright's chromium actually installed, rather than just listed in a package.json?
///
/// Asked by resolving it the way the suites do, because `node_modules/playwright` can be present
/// while the browser it downloads separately is not — which fails at `chromium.launch()`, minutes
/// into a run, with a message about a missing executable rather than about setup.
///
/// **From `tests/ui`, not from the repo root.** That is where `node_modules` is (`tests/ui/package.json`
/// is its own), and asking from the root resolves nothing and reports "not installed" on a machine
/// that has it — which would skip the browser tier silently for ever, the exact shape of the bug
/// this file exists to end.
fn chromium_ready() -> bool {
    Command::new("node")
        .args([
            "-e",
            "const fs = require('node:fs'); \
             import('playwright') \
               .then(p => process.exit(fs.existsSync(p.chromium.executablePath()) ? 0 : 1)) \
               .catch(() => process.exit(1))",
        ])
        .current_dir(repo().join("tests/ui"))
        .output()
        .is_ok_and(|out| out.status.success())
}

#[test]
fn the_cockpit_suites_that_need_no_browser_pass() {
    // Same treatment `the_cockpit_bundle_is_not_stale` gives it: a machine without node can still
    // build skein. Returning rather than failing every suite too, because one missing interpreter
    // is one fact, not twenty-five.
    let Some(broken) = run_all(&NODE_SUITES) else {
        return skip("no node on this machine, so the cockpit suites cannot be run");
    };
    assert!(
        broken.is_empty(),
        "{} cockpit suite(s) failed. Run one on its own to see all of it: \
         `node tests/ui/<name>.mjs`\n\n{}",
        broken.len(),
        broken.join("\n\n")
    );
}

/// The browser tier, reported either way.
///
/// **Passes when chromium is absent, and says so.** Failing would mean nobody can run `cargo test`
/// without a 150 MB download; `#[ignore]` would mean the run says `ignored` with no reason attached.
/// This says which of the two happened where the result already is.
#[test]
fn the_cockpit_suites_that_drive_a_browser_pass_or_report_that_they_were_skipped() {
    if !chromium_ready() {
        // Deliberately loud in the assertion-free path too: the panic message is the only text
        // `cargo test` shows for free, so the skip goes where a reader will hit it if they ever look
        // at this test — and `tests/ui/README.md` is one command away.
        return skip(&format!(
            "Playwright's chromium is not installed, so the browser suites ({}) did not run. \
             `cd tests/ui && npm run setup` installs it — see tests/ui/README.md.",
            BROWSER_SUITES.join(", ")
        ));
    }
    let Some(broken) = run_all(&BROWSER_SUITES) else {
        return skip("no node on this machine, so the browser suites cannot be run");
    };
    assert!(
        broken.is_empty(),
        "{} browser suite(s) failed. Each keeps its fixture and writes a screenshot; run one on \
         its own to see where: `node tests/ui/<name>.mjs`\n\n{}",
        broken.len(),
        broken.join("\n\n")
    );
}

/// Every `.mjs` under `tests/ui`, at any depth, named the way a suite is named — relative to
/// `tests/ui` and without the extension, so `harness/browser.mjs` comes back as `harness/browser`.
///
/// Recursive, because the guard below was not (SKEIN-587). `read_dir` reads one level, and
/// `tests/ui/harness/` and `tests/ui/fixtures/` already exist — so the subdirectory pattern is
/// established in the very directory being guarded, and a suite added one level down joined neither
/// list and nothing said so. Probed rather than argued: a throwaway `zz-probe.mjs` at the top level
/// of `tests/ui/` turned the old guard red, and the same file one directory down, in `harness/`,
/// left it green.
fn every_mjs(dir: &Path) -> Vec<String> {
    fn walk(base: &Path, at: &Path, out: &mut Vec<String>) {
        let Ok(entries) = std::fs::read_dir(at) else {
            return;
        };
        for entry in entries.filter_map(|e| e.ok()) {
            let path = entry.path();
            if path.is_dir() {
                // Somebody else's code, and a lot of it: `npm install` puts playwright here and it
                // ships four `.mjs` entry points, none of them anybody's suite. Named rather than
                // pattern-matched, because this is the one directory in `tests/ui` that is not
                // ours — everything else that appears is something a person in this repo wrote, and
                // the whole point of the walk is that such a thing is never skipped silently.
                if path.file_name().is_some_and(|n| n == "node_modules") {
                    continue;
                }
                walk(base, &path, out);
            } else if let Some(rel) = path.strip_prefix(base).ok().and_then(|r| r.to_str()) {
                if let Some(name) = rel.strip_suffix(".mjs") {
                    out.push(name.replace('\\', "/"));
                }
            }
        }
    }
    let mut out = Vec::new();
    walk(dir, dir, &mut out);
    out
}

/// The files some other file **imports** — which is what makes something a helper rather than a
/// suite, and the only answer that does not depend on where it happens to sit.
///
/// Depth cannot tell them apart: `lift.mjs` is a helper at depth 1 and a suite could be added at
/// depth 2, so "everything below the top level is a helper" would excuse exactly the file this
/// guard exists to catch. A suite is run (`node tests/ui/<name>.mjs`) and imported by nobody; a
/// helper is imported. `lift`, `harness/browser`, `harness/server` and `harness/github` are all
/// named by an `import … from` somewhere, and a new one will be too, or it is dead.
///
/// Note which way the mistakes fall. A helper nobody has imported *yet* reads as an unlisted suite
/// and turns this red — annoying, and the safe direction. The unsafe direction would be treating a
/// suite as a helper, which is why only specifiers that actually follow `from` are collected rather
/// than every string in the file that ends in `.mjs`.
fn imported_by_something(dir: &Path) -> std::collections::BTreeSet<String> {
    let mut out = std::collections::BTreeSet::new();
    for file in every_mjs(dir) {
        let text = std::fs::read_to_string(dir.join(format!("{file}.mjs"))).unwrap_or_default();
        // The importing file's own directory, which relative specifiers resolve against.
        let from_dir: Vec<&str> = file.split('/').collect();
        let from_dir = &from_dir[..from_dir.len() - 1];
        for (at, _) in text.match_indices("from ") {
            let rest = text[at + "from ".len()..].trim_start();
            let Some(quote) = rest.chars().next().filter(|c| *c == '"' || *c == '\'') else {
                continue;
            };
            let Some(end) = rest[1..].find(quote) else {
                continue;
            };
            let Some(spec) = rest[1..1 + end].strip_suffix(".mjs") else {
                continue;
            };
            let mut parts: Vec<&str> = from_dir.to_vec();
            for step in spec.split('/') {
                match step {
                    "." | "" => {}
                    ".." => {
                        parts.pop();
                    }
                    other => parts.push(other),
                }
            }
            out.insert(parts.join("/"));
        }
    }
    out
}

/// Every suite is in one of the two lists, so adding a file is not the same as running it.
///
/// The bug this whole file is about is a check that existed and was not invoked. A file dropped
/// into `tests/ui/` and never listed here would be exactly that again, and the failure would
/// look like nothing at all.
#[test]
fn every_suite_in_the_directory_is_in_one_of_the_lists() {
    let dir: PathBuf = repo().join("tests/ui");
    let helpers = imported_by_something(&dir);
    let mut found: Vec<String> = every_mjs(&dir)
        .into_iter()
        // Imported by something, so it is a helper and running it would assert nothing.
        .filter(|name| !helpers.contains(name))
        // `lift` as well, belt and braces: it is imported by every suite, so the line above already
        // covers it, and this is the one exclusion that fails in the safe direction if that ever
        // stops being true.
        .filter(|name| name != "lift")
        .collect();
    found.sort();

    let mut listed: Vec<String> = NODE_SUITES
        .iter()
        .chain(BROWSER_SUITES.iter())
        .map(|s| s.to_string())
        .collect();
    listed.sort();

    assert_eq!(
        found, listed,
        "a suite in tests/ui/ is not in NODE_SUITES or BROWSER_SUITES (or a listed one is gone). \
         An unlisted suite is one nothing runs, which is the whole reason this file exists."
    );
}

/// This file's own source, so the prose guards below read what a reader reads.
const THIS_FILE: &str = include_str!("browser_suites.rs");

/// The words that tally something, split by the grammar that tells a count from a turn of phrase.
///
/// `one` and `first` are in neither list on purpose. `one suite` is the singular and `the first
/// suite` is an ordering, so banning them would catch nothing and teach people to write around the
/// guard.
const CARDINALS: [&str; 27] = [
    "two",
    "three",
    "four",
    "five",
    "six",
    "seven",
    "eight",
    "nine",
    "ten",
    "eleven",
    "twelve",
    "thirteen",
    "fourteen",
    "fifteen",
    "sixteen",
    "seventeen",
    "eighteen",
    "nineteen",
    "twenty",
    "thirty",
    "forty",
    "fifty",
    "sixty",
    "seventy",
    "eighty",
    "ninety",
    "hundred",
];

/// An ordinal counts an arrival, and an arrival is the size of the directory plus one — which is
/// how the drift guard below came to warn about a newcomer while the directory already held two and
/// a half times as many.
const ORDINALS: [&str; 19] = [
    "second",
    "third",
    "fourth",
    "fifth",
    "sixth",
    "seventh",
    "eighth",
    "ninth",
    "tenth",
    "eleventh",
    "twelfth",
    "thirteenth",
    "fourteenth",
    "fifteenth",
    "sixteenth",
    "seventeenth",
    "eighteenth",
    "nineteenth",
    "twentieth",
];

/// A cardinal, or a run of digits, or a hyphenated compound of those — so `twenty-five` counts and
/// `SKEIN-613`, `eleven-core` and `HTTP/1.1` do not, because a compound counts only when **every**
/// part of it does.
fn is_a_cardinal(word: &str) -> bool {
    let digits = |p: &str| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit());
    !word.is_empty()
        && word
            .split('-')
            .all(|part| digits(part) || CARDINALS.contains(&part))
}

/// The last part of a compound decides, so `twenty-second` is an ordinal and `second-guess` is not.
fn is_an_ordinal(word: &str) -> bool {
    ORDINALS.contains(&word.rsplit('-').next().unwrap_or(word))
}

/// One word of prose with the markup a doc comment wraps it in taken off, so `` `suites` ``,
/// `suites,` and `suite(s)` are all the same word — and with a possessive reduced to its noun, so
/// `the suite's own last words` stays singular instead of reading as a plural somebody counted.
fn bare(word: &str) -> String {
    let kept: String = word
        .chars()
        .map(|c| if c == '\u{2019}' { '\'' } else { c })
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '\'')
        .collect::<String>()
        .to_ascii_lowercase();
    let kept = kept.strip_suffix("'s").unwrap_or(&kept);
    kept.replace('\'', "")
}

/// The comment lines of this file, numbered from 1 — where its prose lives, and the only place
/// either guard below looks.
fn prose() -> impl Iterator<Item = (usize, &'static str)> {
    THIS_FILE
        .lines()
        .enumerate()
        .filter(|(_, line)| line.trim_start().starts_with("//"))
        .map(|(n, line)| (n + 1, line))
}

/// The files outside this one that make the same claim about the same lists.
///
/// SKEIN-613 fixed the copies in this file and SKEIN-656 found more of them here — most already
/// disagreeing with the lists, and one in `docs/parity.md` still carrying a figure from an era when
/// the whole directory was that size. Widening the guard immediately named two the item had not
/// found, in `tests/ui/README.md` and in the workflow, and then named this sentence when it first
/// quoted the offending numbers. The guard was scoped to this file first, deliberately — a guard
/// that reddens files an agent may not edit is a guard somebody turns off — and these were
/// brought in once their prose had been fixed.
///
/// Read from disk rather than `include_str!` so that a path which stops existing is a failure
/// here rather than a check that silently covers one file fewer.
const COUNTED_ELSEWHERE: [&str; 3] = ["CONTRIBUTING.md", "docs/parity.md", "tests/ui/README.md"];

/// Every line of prose in the files above, as `(file, line number, line)`.
///
/// Markdown is prose throughout; in YAML only a `#` line is. Neither carries code that could
/// mention `suites`, so this is the whole of what needs reading.
fn prose_elsewhere() -> Vec<(String, usize, String)> {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut out = Vec::new();
    for rel in COUNTED_ELSEWHERE {
        let text = std::fs::read_to_string(root.join(rel))
            .unwrap_or_else(|e| panic!("{rel} is named by this guard and could not be read: {e}"));
        let yaml = rel.ends_with(".yml") || rel.ends_with(".yaml");
        for (n, line) in text.lines().enumerate() {
            if !yaml || line.trim_start().starts_with('#') {
                out.push((rel.to_string(), n + 1, line.to_string()));
            }
        }
    }
    out
}

/// This file's prose never states a count that its lists already carry.
///
/// A number written in front of the word `suites` is one fact written twice, and the copy in prose
/// is the one that rots — *silently*, because it goes on passing while it describes a repository
/// that is not the one on disk. SKEIN-613 found three of them here, in the file whose entire subject
/// is those numbers: the browser tier was called `five` when [`BROWSER_SUITES`] beside it was
/// already longer, the node tier `24` when [`NODE_SUITES`] was shorter, and the drift guard warned
/// about a `twelfth` arrival when the directory already held more than twice that.
///
/// The corrected figures are deliberately not written here either. They would be a fourth copy, and
/// the lists are two hundred lines up.
///
/// # Why a ban and not a comparison
///
/// The list lengths are compile-time facts, the directory is a run-time one, and
/// [`every_suite_in_the_directory_is_in_one_of_the_lists`] already ties those two together. A doc
/// comment can be neither: there is no stable way to interpolate a length into rustdoc prose, so a
/// number written there can only be compared against something or forbidden outright. Forbidding is
/// the stronger of the two, because a compared number still has to be edited by hand every time the
/// directory changes — and not editing them is exactly how all three went stale.
///
/// **Enumerate instead.** Naming the members — as the measurement on [`run_all`] does, and as the
/// skip message built from `BROWSER_SUITES.join(", ")` does — says everything a tally says and
/// cannot silently disagree with the directory, because names are checkable against disk (which is
/// what [`every_suite_file_this_file_names_exists`] then does) and a bare number is not.
///
/// # The grammar, and why it is not just "a number near the word"
///
/// The first cut was any number within three words, and it fired four times on prose that was
/// perfectly honest: a sentence about what any pair of them shares, an ASCII table whose row labels
/// are line counts, a mention of a helper at depth 1 with the noun three words later, and this
/// test's own examples. So the rule follows the grammar of a tally instead. A **cardinal** counts a
/// population, and a population is plural: it must sit within two words of `suites`, the two being
/// what lets a qualifier through. An **ordinal** counts an arrival and is singular: it must sit
/// directly in front of `suite`. Both remaining shapes are what a stale count actually looks like.
///
/// # What makes it fail
///
/// Putting any of the three copies back. Proved by restoring the exact sentence SKEIN-613 reported
/// to [`run_all`]'s doc comment, whereupon this test named the line, quoted it, and pointed at the
/// word; taken out again, green. The sentence cannot be reproduced here as an example, because it
/// would then be prose in this file and this test would name itself — which is its own small proof
/// that the rule bites.
#[test]
fn no_prose_in_this_file_counts_what_the_lists_already_carry() {
    let mut stated: Vec<String> = Vec::new();
    let here = prose().map(|(n, l)| ("tests/browser_suites.rs".to_string(), n, l.to_string()));
    for (file, n, line) in here.chain(prose_elsewhere()) {
        let words: Vec<String> = line.split_whitespace().map(bare).collect();
        for (at, word) in words.iter().enumerate() {
            let looked_at = match word.as_str() {
                // Two back, so one qualifier between the number and the noun does not hide it.
                "suites" => (
                    &words[at.saturating_sub(2)..at],
                    is_a_cardinal as fn(&str) -> bool,
                ),
                "suite" => (
                    &words[at.saturating_sub(1)..at],
                    is_an_ordinal as fn(&str) -> bool,
                ),
                _ => continue,
            };
            let (before, counts) = looked_at;
            if let Some(tally) = before.iter().find(|w| counts(w)) {
                stated.push(format!("{file}:{n}: `{tally}` — {}", line.trim()));
            }
        }
    }
    assert!(
        stated.is_empty(),
        "prose states a count that NODE_SUITES, BROWSER_SUITES and the directory \
         already carry between them. Such a number is not checked by anything and goes stale in \
         silence — name the members instead, or say what was measured and when, or rephrase so the \
         number is not a claim about how many exist:\n\n{}",
        stated.join("\n")
    );
}

/// Every `tests/ui/…mjs` file this file's prose names is a file that is there.
///
/// The other half of the same lesson. A count rots into a number that is merely wrong; a name rots
/// into a path that resolves to nothing, and a reader who follows it learns less than silence would
/// have told them. `tools/alone-check.py` is this repo's standing example: a pattern that restated
/// fixture names instead of deriving them answered `0` on a box carrying 122 live processes, and
/// nobody could tell, because a pattern that has never matched anything looks exactly like one that
/// matches nothing.
///
/// Globs and placeholders — anything holding `*`, `{` or `<` — are skipped. They are instructions
/// to a shell or to a reader rather than claims about a file, and `tests/ui/*.mjs` is in fact the
/// derived form this whole exercise argues for.
///
/// # What makes it fail
///
/// Renaming or deleting a suite the prose names. Proved by adding a letter to one such mention,
/// which this test then named with its line; spelled back the way it was, green.
#[test]
fn every_suite_file_this_file_names_exists() {
    let mut missing: Vec<String> = Vec::new();
    for (n, line) in prose() {
        for (at, _) in line.match_indices("tests/ui/") {
            let rest = &line[at..];
            let end = rest
                .find(|c: char| c.is_whitespace() || "`\"'(),;".contains(c))
                .unwrap_or(rest.len());
            let named = &rest[..end];
            if !named.ends_with(".mjs") || named.contains(['*', '{', '<']) {
                continue;
            }
            if !repo().join(named).is_file() {
                missing.push(format!("tests/browser_suites.rs:{n}: {named}"));
            }
        }
    }
    assert!(
        missing.is_empty(),
        "this file's prose names a suite file that is not on disk. A path a reader cannot follow is \
         worse than no path — fix the name, or say what replaced it:\n\n{}",
        missing.join("\n")
    );
}
