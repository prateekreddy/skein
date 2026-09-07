//! `cargo test` runs the suites that prove the **page**, not only the ones that prove the API.
//!
//! Eleven files under `tests/ui/` were the only thing checking the cockpit, and nothing ran them.
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
//! repository has ever had and all six browser suites were skipped, the two largest included
//! (SKEIN-567). The intent stated below — that a dead page cannot pass every other check — therefore
//! held for the node tier alone, which is the half that does not open a page.
//!
//! CI installs it now, and the cost is the download rather than the run: cached, the whole browser
//! tier is about a minute (measured at 61.3s with four lanes, 62.3s with eleven — `review.mjs` is
//! the floor and the lanes are already past it).
//!
//! # The skip is stated, not silent
//!
//! A skipped check that says nothing is the failure mode this whole file exists because of. `cargo
//! test` hides stdout for a passing test, so a skip printed there is a skip nobody reads — which is
//! how eleven suites went unrun. Instead the browser tier is **one test whose name is the report**:
//! it passes either way, and the run says which happened, in the list of test names everybody
//! already looks at. `tests/ui/README.md` has the setup.

mod common;

use common::skip;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

/// Needs only node. `lift.mjs` is absent on purpose — it is the shared helper the others import,
/// not a suite, and running it asserts nothing.
const NODE_SUITES: [&str; 22] = [
    "attach",
    "budget",
    "conversation",
    "foreign",
    "gitgate",
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
const BROWSER_SUITES: [&str; 6] = [
    "actfail",
    "connections",
    "onboarding",
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
/// 25 node processes started at once on four cores is that burst, five suites on eleven cores is
/// not.
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
/// Nothing is shared between two suites. Each builds its own fixture with `mkdtempSync`, and each
/// binds its server on port 0 and asks the kernel which port it got — so there is no fixed path and
/// no fixed port for two of them to collide on, and no suite reads what another wrote:
///
/// ```text
/// grep -c 'mkdtempSync' tests/ui/{actfail,connections,onboarding,review,smoke}.mjs
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
/// The browser tier is the whole cost of this file: its five suites are 193.6s run one after
/// another (48.3 actfail, 14.8 connections, 19.1 onboarding, 80.6 review, 30.8 smoke, timed one at
/// a time), against 16.7s for all 24 node suites, which run concurrently with them anyway. So
/// `cargo test --test browser_suites` was 195.12s, and concurrently it is the longest single suite
/// plus change. Measured on an 11-core box:
///
/// ```text
/// SKEIN_UI_LANES=1 cargo test --test browser_suites   193.94s
///                  cargo test --test browser_suites    81.96s
/// ```
///
/// (195.12s for the same command before this function existed, so the knob costs nothing.)
///
/// `review.mjs` at 80.6s is the floor and 81.96s is one lane's worth above it: splitting that one
/// suite is the only thing left that would move this number.
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
/// Whole output would bury the answer: `smoke` prints 57 lines when it is happy. The failures are at
/// the end, and every one of these suites ends with its own summary. 40 lines rather than 25 because
/// a failing browser suite now ends with the server's own stderr as well (`skein: reading acme: …`
/// is often the entire diagnosis), and the window has to hold both that and the failed checks.
fn tail(said: &str) -> String {
    let lines: Vec<&str> = said.lines().collect();
    lines[lines.len().saturating_sub(40)..].join("\n")
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
/// list and nothing said so. Probed rather than argued: a file at `tests/ui/zz-probe.mjs` turned the
/// old guard red, and the same file at `tests/ui/harness/zz-probe.mjs` left it green.
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
/// The bug this whole file is about is a check that existed and was not invoked. A twelfth suite
/// dropped into `tests/ui/` and never listed here would be exactly that again, and the failure would
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
