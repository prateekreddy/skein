//! **Every path that creates the fleet sandbox hands it the same mount set** (SKEIN-678).
//!
//! There were two, and they disagreed. `fleet::create_line` — the line a person is given to type —
//! passed `fleet_serve_mounts()`; the cockpit's `POST /api/fleet/create` passed `fleet_mounts()`.
//! The two differ by exactly one entry, the volume root, and it is the entry the install cannot
//! start without: `bootstrap.sh` finds the volume by scanning `/proc/self/mountinfo` for a mount
//! point ending in `/.skein` and **refuses rather than guess** when it finds none. `fleet_mounts()`
//! contributes only the two directories *beneath* the volume, so a fleet created from the cockpit
//! gave that scan nothing to find and the install stopped at the refusal. sbx fixes mounts at
//! create and no verb adds one to a sandbox that exists, so the only repair was destroying the
//! sandbox and making it again.
//!
//! # Why this is a source scan and not two calls compared
//!
//! The choice under test is *which set a call site passes*, and that is only visible in the source:
//! `api_fleet_create` lives in a binary, and a test that called it would write the settings and ask
//! a warden to build a sandbox. More importantly, a test that drove the two paths it knows about
//! could never see a **third** path added later — and a third path diverging in silence is the
//! whole failure. So the create entry points are found by scanning the whole of `src/`, the
//! expected answer is *derived from `create_line`* rather than written down here, and
//! [`the_only_production_readers_of_a_mount_set_are_the_ones_that_must_read_that_one`] closes the
//! gap a scan of call arguments leaves: a new function that computes the wrong set into a local.
//!
//! Same technique, and the same reason, as `neither_lifecycle_route_reaches_its_work_by_a_path_that_skips_the_check`
//! in `src/bin/skein-server.rs`.

mod common;

use common::{env_lock, Scratch};
use std::path::{Path, PathBuf};

/// The two functions that answer "what does the fleet sandbox mount". Longest first, because
/// `fleet_serve_mounts` must not be read as `fleet_mounts` with a prefix.
const MOUNT_SETS: [&str; 2] = ["fleet_serve_mounts", "fleet_mounts"];

/// Everything that takes a mount set on its way to `sbx create`.
///
/// `create_argv` renders the command, `create_through_warden` and `create_fleet_operation` and
/// `request_fleet_create` carry one down to it. Listing the funnel rather than the callers is what
/// makes a create path added tomorrow visible to this file without anybody editing it.
const CREATE_ENTRY_POINTS: [&str; 4] = [
    "create_argv(",
    "create_through_warden(",
    "create_fleet_operation(",
    "request_fleet_create(",
];

fn repo() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// One file of the repository, production half only.
fn source(relative: &str) -> String {
    let path = repo().join(relative);
    production(&std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{relative}: {e}")))
}

/// Every `.rs` file under `src/`, so the scan covers files that do not exist yet.
fn rust_files(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for entry in std::fs::read_dir(&d).unwrap_or_else(|e| panic!("{}: {e}", d.display())) {
            let path = entry.expect("a directory entry").path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "rs") {
                out.push(path);
            }
        }
    }
    out.sort();
    out
}

/// A file's production half, with comments blanked.
///
/// Two things a naive `contains` would get wrong. The unit tests below `#[cfg(test)] mod tests`
/// name both mount sets constantly, and so do the doc comments — `create_line`'s own doc is a
/// sentence about choosing one over the other. Comment *lines* are blanked rather than removed so
/// that a line number reported by this file still matches the file on disk.
///
/// The cut is the `#[cfg(test)]` that carries a `mod`, not the first one in the file: `src/fleet.rs`
/// declares test-only *items* — a `const` and two `fn`s — up among the production code, and cutting
/// at the first would drop everything after it, `create_line` included, which is the one thing this
/// file derives from.
fn production(text: &str) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let end = lines
        .windows(2)
        .position(|w| w[0] == "#[cfg(test)]" && w[1].starts_with("mod "))
        .unwrap_or(lines.len());
    lines[..end]
        .iter()
        .map(|line| match line.trim_start().starts_with("//") {
            true => "",
            false => *line,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Which mount set does this text name, and where. Earliest first.
fn mount_sets_named(text: &str) -> Vec<(usize, &'static str)> {
    let mut found: Vec<(usize, &'static str)> = Vec::new();
    for name in MOUNT_SETS {
        let mut from = 0;
        while let Some(at) = text[from..].find(name) {
            let at = from + at;
            from = at + name.len();
            // `fleet_mounts` is not a hit inside `fleet_serve_mounts`, and neither is a hit inside
            // some longer identifier that happens to end in one of them.
            let before_is_ident = text[..at]
                .chars()
                .next_back()
                .is_some_and(|c| c.is_alphanumeric() || c == '_');
            if !before_is_ident && text[from..].starts_with('(') {
                found.push((at, name));
            }
        }
    }
    found.sort();
    found
}

/// The 1-based line `at` falls on.
fn line_of(text: &str, at: usize) -> usize {
    text[..at].matches('\n').count() + 1
}

/// The top-level `fn` `at` sits in, and its whole body.
///
/// Column 0 to the next column-0 `}`, rather than balancing braces: every function in play here is
/// top-level, and brace-balancing Rust source with a regex-free scanner means being wrong about
/// format strings, which these functions are full of.
fn enclosing_fn(text: &str, at: usize) -> (String, &str) {
    let mut start = 0;
    let mut name = "<top level>".to_string();
    for (offset, line) in line_offsets(text) {
        if offset > at {
            break;
        }
        for prefix in ["pub async fn ", "pub fn ", "async fn ", "fn "] {
            if let Some(rest) = line.strip_prefix(prefix) {
                start = offset;
                name = rest
                    .split(|c: char| !(c.is_alphanumeric() || c == '_'))
                    .next()
                    .unwrap_or("")
                    .to_string();
            }
        }
    }
    let end = line_offsets(text)
        .find(|(offset, line)| *offset > at && line.starts_with('}'))
        .map(|(offset, _)| offset)
        .unwrap_or(text.len());
    (name, &text[start..end])
}

/// Every line with its byte offset.
fn line_offsets(text: &str) -> impl Iterator<Item = (usize, &str)> {
    let mut offset = 0;
    text.lines().map(move |line| {
        let at = offset;
        offset += line.len() + 1;
        (at, line)
    })
}

/// The mount set `create_line` hands the create — the one every other path is measured against.
///
/// Derived, not written down: this test says "all the create paths agree with the one a person
/// types", and the runtime half below says what that set must contain. Between them, swapping
/// *either* call site to the other function fails a named assertion.
fn blessed(fleet_rs: &str) -> &'static str {
    let at = fleet_rs
        .find("pub fn create_line(")
        .expect("`create_line` is gone from src/fleet.rs — this whole file derives from it");
    let (_, name) = *mount_sets_named(&fleet_rs[at..])
        .first()
        .expect("`create_line` no longer names a mount set, so there is nothing to derive from");
    name
}

/// **Nobody creates a fleet with a different mount set from the one `skein doctor` prints.**
///
/// Would fail if: `api_fleet_create` went back to `fleet_mounts()` (SKEIN-678's bug), or a third
/// create path were added passing it, or `create_line` were changed and the cockpit route left
/// behind. Both directions, because the expected value is one of the two call sites.
#[test]
fn every_fleet_create_path_hands_over_the_mount_set_create_line_hands_over() {
    let src = repo().join("src");
    let files = rust_files(&src);
    assert!(
        files.len() > 50,
        "only {} .rs files found under {} — the scan is not reading the source tree, so every \
         assertion below is about nothing",
        files.len(),
        src.display()
    );

    let fleet_rs = source("src/fleet.rs");
    let want = blessed(&fleet_rs);

    // A create path is a function that BOTH names a mount set and reaches the create funnel. Found
    // that way rather than by reading call arguments, because `create_line` puts its set in a local
    // first (`create_argv(sandbox, &mounts)`) — an argument scan would have missed the one call
    // site this test derives from, and been green.
    let mut sites: Vec<(String, usize, String, &'static str)> = Vec::new();
    for file in &files {
        let text = production(&std::fs::read_to_string(file).expect("a source file"));
        let shown = file
            .strip_prefix(repo())
            .unwrap_or(file)
            .display()
            .to_string();
        for (at, named) in mount_sets_named(&text) {
            // Its own definition names it without choosing it.
            if text[..at].ends_with("fn ") {
                continue;
            }
            let (owner, body) = enclosing_fn(&text, at);
            if CREATE_ENTRY_POINTS.iter().any(|entry| body.contains(entry)) {
                sites.push((shown.clone(), line_of(&text, at), owner, named));
            }
        }
    }

    assert!(
        !sites.is_empty(),
        "nothing in src/ both names a mount set and reaches one of {CREATE_ENTRY_POINTS:?} — the \
         create funnel has been renamed and this scan now recognises nothing, which is green about \
         anything"
    );
    for expected in ["src/fleet.rs", "src/bin/skein-server.rs"] {
        assert!(
            sites.iter().any(|(file, ..)| file == expected),
            "{expected} chose a mount set for a create when this was written and no longer does; \
             if the path moved, the scan must be able to see where it moved to. Found: {sites:?}"
        );
    }
    for (file, line, owner, named) in &sites {
        assert_eq!(
            *named, want,
            "{file}:{line} — `{owner}` hands `{named}()` to a fleet create while `create_line` \
             hands `{want}()`. The two differ by the volume root, and a fleet created without it \
             cannot be installed into (bootstrap.sh refuses) or repaired (sbx fixes mounts at \
             create)"
        );
    }
}

/// **The set they agree on is the one bootstrap.sh can find**, which is the half a source scan
/// cannot see: agreement on the wrong function would satisfy the test above on its own.
///
/// The predicate is read out of `bootstrap.sh` rather than restated, so the day that scan changes
/// shape this stops claiming to check it.
#[test]
fn the_mount_set_every_create_path_uses_carries_the_volume_root() {
    let _env = env_lock();
    let scratch = Scratch::temp("skein-createmounts-it");
    // Named `.skein` because that is what bootstrap.sh's scan requires of it.
    let home = scratch.path().join(".skein");
    std::fs::create_dir_all(&home).expect("a fixture volume root");
    let home = std::fs::canonicalize(&home).expect("a canonical fixture volume root");
    std::env::set_var("SKEIN_HOME", &home);

    let bootstrap = std::fs::read_to_string(repo().join("bootstrap.sh")).expect("bootstrap.sh");
    assert!(
        bootstrap.contains(r"$5 ~ /\/\.skein$/"),
        "bootstrap.sh no longer finds the fleet volume by matching a mount point ending in \
         `/.skein`, so what this test asserts about the mount set is no longer what the install \
         needs"
    );

    let fleet_rs = source("src/fleet.rs");
    // Bound to the function `create_line` actually names, so this is an assertion about the SET
    // and not about a spelling: pointing every create path at `fleet_mounts` fails here.
    let mounts = match blessed(&fleet_rs) {
        "fleet_serve_mounts" => skein::fleet::fleet_serve_mounts(),
        "fleet_mounts" => skein::fleet::fleet_mounts(),
        other => panic!("`create_line` names {other}, which is not a mount set this test knows"),
    };

    let home = home.to_string_lossy().into_owned();
    assert!(
        mounts.contains(&home),
        "the mount set every create path uses does not carry the volume root {home:?}: {mounts:?}. \
         bootstrap.sh scans mountinfo for a mount point ending in `/.skein`, finds none, and \
         refuses to install — and sbx fixes mounts at create, so the sandbox cannot be repaired"
    );
    assert!(
        mounts.iter().any(|m| m.ends_with("/.skein")),
        "nothing in {mounts:?} would match bootstrap.sh's `$5 ~ /\\/\\.skein$/`"
    );
}

/// **`skein doctor` checks the set the fleet was created from**, which is the second half of
/// SKEIN-678: the check and the thing it checked were written from two different lists inside one
/// binary, so a fleet missing the volume root reported every mount present and healthy.
///
/// Would fail if the doctor's loop went back to `fleet_mounts()`.
#[test]
fn skein_doctor_checks_the_mount_set_the_fleet_was_created_from() {
    let fleet_rs = source("src/fleet.rs");
    let want = blessed(&fleet_rs);

    let cli = source("src/bin/skein.rs");
    let named = mount_sets_named(&cli);
    assert!(
        !named.is_empty(),
        "src/bin/skein.rs no longer reads a mount set at all — `skein doctor`'s mount rows are the \
         only report a person has when the cockpit is the thing that is broken"
    );
    for (at, name) in named {
        assert_eq!(
            name,
            want,
            "src/bin/skein.rs:{} ({}) reads `{name}()` while the fleet is created from `{want}()`. \
             A doctor blind to the volume root cannot diagnose the one mount whose absence stops \
             the install",
            line_of(&cli, at),
            enclosing_fn(&cli, at).0
        );
    }
}

/// **The gap a scan of call arguments leaves**: a create path that computes the wrong set into a
/// local variable, in a function nothing here lists, would pass everything above.
///
/// So every production naming of a mount set is accounted for. Two functions legitimately read the
/// narrower one and the reasons are opposite, which is why this is a list of exceptions with
/// reasons rather than a rule:
///
///   * `fleet_serve_mounts` composes itself out of `fleet_mounts`;
///   * `mount_manifest` tells a box's launcher what to uncover, and a box must **not** see the
///     volume root — `exposes_the_volume` exists to keep every credential skein holds out of it.
///
/// Anything else naming `fleet_mounts` is a third reader, and a third reader is how this bug
/// happened. Failing here is the request to say which of the two it wants and why.
#[test]
fn the_only_production_readers_of_a_mount_set_are_the_ones_that_must_read_that_one() {
    let narrower = [
        ("fleet_serve_mounts", "it is defined as the wider set"),
        ("mount_manifest", "a box must not be handed the volume root"),
    ];
    let mut readers: Vec<(String, usize, String, &'static str)> = Vec::new();
    for file in rust_files(&repo().join("src")) {
        let text = production(&std::fs::read_to_string(&file).expect("a source file"));
        let shown = file
            .strip_prefix(repo())
            .unwrap_or(&file)
            .display()
            .to_string();
        for (at, name) in mount_sets_named(&text) {
            // Its own definition is not a reading of it.
            if text[..at].ends_with("fn ") {
                continue;
            }
            let (owner, _) = enclosing_fn(&text, at);
            readers.push((shown.clone(), line_of(&text, at), owner, name));
        }
    }
    // Five when this was written, and the scan names each with its enclosing function:
    // `create_line`, `api_fleet_create`, `cmd_doctor`, and the two exceptions above.
    assert!(
        readers.len() >= 5,
        "only {} production readers of a mount set found, where there were five — the scan has \
         stopped seeing them, and a scan that sees nothing agrees with everything: {readers:?}",
        readers.len()
    );
    for (file, line, owner, name) in &readers {
        if *name == "fleet_serve_mounts" {
            continue;
        }
        let why = narrower.iter().find(|(fun, _)| fun == owner);
        assert!(
            why.is_some(),
            "{file}:{line} — `{owner}` reads `{name}()`, the mount set WITHOUT the volume root, \
             and it is not one of the two functions that must: {narrower:?}. If it is creating a \
             fleet it wants `fleet_serve_mounts()`; if it is deciding what a box may see it wants \
             this one, and belongs in that list with its reason"
        );
    }
}
