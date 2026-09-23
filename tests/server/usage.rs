//! `/api/usage`: the cached reading, and the two ways it is ever re-read.

use super::*;

// ── /api/usage: the cached reading, and the two ways it is ever re-read (SKEIN-835) ──────────────
//
// The rule these two tests exist to hold is the owner's, in his words: "cache, no reread on every
// page load, re-reads have to be intentional by user and once in a while (say 1 hr)". `api_usage`
// serves the stored tally, re-reads when the caller passes `?refresh=1`, and re-reads on a plain
// load only once the stored one is over an hour old.
//
// **The clock is never waited on.** `skein::usage` keeps its tally in `$SKEIN_HOME/usage.json` with
// the moment it was taken as `read_at_unix`, so the way to ask what the route does with a reading
// of a given age is to write that age into the file — [`backdate`] below — and make one request.
// Nothing here sleeps, retries, or has a verdict that depends on how fast this box is: the two
// timing assertions are bounded by the test's own measured elapsed time rather than by a constant
// somebody widened until it stopped failing.

/// The box this fixture's transcripts belong to.
///
/// Invented, and the point is that it is invented: `tools/residue-check.py` fails the build on a
/// real box name (SKEIN-629). `tests/usage.rs` has to hash the names it uses because its oracle is
/// a recording of the owner's actual fleet; there is nothing to hash here, because this fixture
/// creates the box it reads and nothing in it came off a real machine.
const USAGE_BOX: &str = "skein-usage-it-box";

/// One assistant record, as `skein::usage` expects to find it.
///
/// The five token counts arrive as one tuple — `(input, output, cache_read, write_5m, write_1h)` —
/// which is the shape `src/usage.rs`'s own `line` helper uses and for the same reason: five more
/// parameters here is a helper that sits on the argument count the lints allow and needs an
/// `#[allow]` the next time anything is added to it. The last two are the cache-write TTL split,
/// which is a separate field rather than a derived one: `cache_write_5m` and `cache_write_1h`
/// partition `cache_creation_input_tokens` and are priced differently.
fn usage_record(msg: &str, day: &str, tok: (u64, u64, u64, u64, u64)) -> String {
    let (input, output, cache_read, w5, w1) = tok;
    serde_json::json!({
        "type": "assistant",
        "timestamp": format!("{day}T10:00:00Z"),
        "requestId": format!("req-{msg}"),
        "isSidechain": false,
        "message": {
            "id": format!("msg-{msg}"),
            "model": "claude-opus-5",
            "content": [{"type": "text", "text": "this body must never reach the payload"}],
            "usage": {
                "input_tokens": input,
                "output_tokens": output,
                "cache_read_input_tokens": cache_read,
                "cache_creation_input_tokens": w5 + w1,
                "cache_creation": {
                    "ephemeral_5m_input_tokens": w5,
                    "ephemeral_1h_input_tokens": w1,
                },
            },
        },
    })
    .to_string()
}

/// Add one transcript to the fixture box. Each call adds a file, so `transcripts_read` counts them.
fn add_transcript(home: &Path, msg: &str, day: &str, tok: (u64, u64, u64, u64, u64)) {
    let dir = home
        .join("boxes")
        .join(USAGE_BOX)
        .join("claude-projects")
        .join("a-project");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join(format!("session-{msg}.jsonl")),
        usage_record(msg, day, tok),
    )
    .unwrap();
}

/// The three transcripts, and the fleet token total after each has been added.
///
/// Cumulative because each one stays: `(100+200+1000+100) = 1400`, then `+ (10+20+100+10) = 1540`,
/// then `+ (1+2+10+1) = 1554`. The fourth term is the cache WRITE, which is what the TTL split adds
/// up to — `cache_write_5m` and `cache_write_1h` partition `cache_write` rather than extending it,
/// so they are not counted again on top. Deliberately not round multiples of each other: a reading
/// that is off by one file has to be visibly off, not plausibly off.
const USAGE_T1: (u64, u64, u64, u64, u64) = (100, 200, 1000, 40, 60);
const USAGE_T2: (u64, u64, u64, u64, u64) = (10, 20, 100, 4, 6);
const USAGE_T3: (u64, u64, u64, u64, u64) = (1, 2, 10, 1, 0);
const USAGE_TOKENS_1: u64 = 1400;
const USAGE_TOKENS_2: u64 = 1540;
const USAGE_TOKENS_3: u64 = 1554;

/// A `$SKEIN_HOME` with the API token and one transcript already in it.
fn usage_home(tag: &str) -> Scratch {
    let home = token_home(tag);
    add_transcript(&home, "one", "2026-09-01", USAGE_T1);
    home
}

/// Spawn a server over `home` with nothing of the real fleet or registry reachable from it.
fn usage_server(home: &Scratch) -> (Child, String) {
    serving(
        Command::new(env!("CARGO_BIN_EXE_skein-server"))
            .env("SKEIN_HOME", home.path())
            .env("SKEIN_FLEET_ROOT", fleet_root_in(home))
            // Where nothing listens, as every other spawn in this file does: a server that goes
            // looking for a warden on the developer's machine is a test reading someone else's state.
            .env("SKEIN_WARDEN", "127.0.0.1:1")
            .env_remove("SKEIN_REGISTRY")
            .env_remove("SKEIN_SHARED")
            .stdout(Stdio::null())
            .stderr(Stdio::null()),
    )
}

/// Move the stored reading's timestamp back by `secs`, leaving the tally itself untouched.
///
/// **Both fields, and that is the whole reason this is a function.** `usage.json` carries the
/// moment twice — `read_at_unix`, which is what the age is computed from, and `report.read_at`,
/// the RFC3339 string the panel prints. Backdating one and not the other would manufacture exactly
/// the disagreement these tests exist to detect, and the test would then be asserting against a
/// file no `refresh` could ever have written.
///
/// Returns the unix second it moved them to.
fn backdate(home: &Path, secs: u64) -> u64 {
    let path = home.join("usage.json");
    let body = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("no stored usage reading at {}: {e}", path.display()));
    let mut stored: serde_json::Value = serde_json::from_str(&body).unwrap();
    let was = stored["read_at_unix"].as_u64().unwrap_or_else(|| {
        panic!("the stored reading has no read_at_unix — the cache shape moved: {body:.400}")
    });
    let then = was - secs;
    stored["read_at_unix"] = serde_json::json!(then);
    stored["report"]["read_at"] = serde_json::json!(rfc3339(then));
    std::fs::write(&path, serde_json::to_string(&stored).unwrap()).unwrap();
    then
}

/// The same spelling `skein::usage` writes: seconds, UTC, `Z`.
fn rfc3339(secs: u64) -> String {
    chrono::DateTime::from_timestamp(secs as i64, 0)
        .expect("a representable instant")
        .format("%Y-%m-%dT%H:%M:%SZ")
        .to_string()
}

fn usage_json(addr: &str, path: &str) -> serde_json::Value {
    let (status, body) = http_get(addr, path);
    assert_eq!(status, 200, "GET {path} answered {status}: {body}");
    let payload = body
        .split_once("\r\n\r\n")
        .map(|(_, b)| b)
        .unwrap_or(&body)
        .trim();
    serde_json::from_str(payload)
        .unwrap_or_else(|e| panic!("GET {path} did not answer JSON ({e}): {payload:.400}"))
}

/// Serving the cache must mean serving the cache's own age, not the age of the answer.
///
/// **The defect this is shaped against** is the one this repository keeps producing: a status
/// display reporting on something other than the thing it names. A route that reads a stored tally
/// and stamps `read_at` with the moment it replied is indistinguishable from a correct one in every
/// figure a person looks at — the dollars are right — while the one line the panel uses to decide
/// whether to trust them says "read just now" about a reading taken an hour ago.
///
/// Each assertion below, and the change that makes it fail — named before it was written, and each
/// one then made to fail by hand (see the branch's commit message):
///
/// | assertion | what breaks it |
/// |---|---|
/// | the forced read sees 1 transcript / `USAGE_TOKENS_1` | a route that never scans at all |
/// | the plain load still sees 1 transcript, after a second was added | a route that calls `usage::refresh` instead of `usage::report`, i.e. re-reads on every page load |
/// | `read_at` is the backdated instant | a route that stamps `read_at` when it answers |
/// | `age_secs` is the backdated age | a route that zeroes the age, or recomputes it from its own clock |
/// | `fresh` is true | a route that hardcodes the flag rather than letting the window decide |
#[test]
fn the_usage_route_serves_the_cached_reading_and_reports_its_real_age() {
    let home = usage_home("usage-cached");
    let (child, addr) = usage_server(&home);
    let _kid = Kid(child);

    // One forced read, which is the only thing in this test that opens a transcript.
    let first = usage_json(&addr, "/api/usage?refresh=1");
    assert_eq!(
        first["transcripts_read"], 1,
        "the forced read did not read the one transcript in the fixture: {first}"
    );
    assert_eq!(first["totals"]["tokens"], USAGE_TOKENS_1);
    assert_eq!(first["boxes_read"], 1);
    assert!(
        first["totals"]["cost"].as_f64().unwrap_or(0.0) > 0.0,
        "a priced model came back with no cost, so nothing was priced: {first}"
    );
    assert_eq!(
        first["unpriced"].as_array().map(|a| a.len()),
        Some(0),
        "claude-opus-5 is in the price table and must not surface as unpriced: {first}"
    );

    // A second transcript, which the cached reading knows nothing about. From here on, a payload
    // that mentions it is a payload that re-read the fleet.
    add_transcript(&home, "two", "2026-09-02", USAGE_T2);

    // Half an hour old — inside the hour, so a plain load must not pay to look.
    let then = backdate(&home, 1800);
    let backdated_at = Instant::now();
    let cached = usage_json(&addr, "/api/usage");

    assert_eq!(
        cached["transcripts_read"], 1,
        "a plain page load re-read the fleet: it found the transcript that was added after the \
         stored reading was taken. The rule is that only ?refresh=1 or an hour may do that: {cached}"
    );
    assert_eq!(cached["totals"]["tokens"], USAGE_TOKENS_1);
    assert_eq!(
        cached["read_at"].as_str(),
        Some(rfc3339(then).as_str()),
        "the route served the stored tally under a different timestamp than the one it was taken \
         at — the figures are the old reading's and the clock is the answer's: {cached}"
    );

    // Bounded by this test's own elapsed time rather than by a slack constant: everything between
    // `backdated_at` and the reply is the only thing that can legitimately have aged the reading.
    let age = cached["age_secs"].as_u64().unwrap_or_default();
    let ceiling = 1800 + backdated_at.elapsed().as_secs() + 1;
    assert!(
        (1800..=ceiling).contains(&age),
        "the reading was stamped 1800s old and the route called it {age}s old (at most {ceiling} \
         could have elapsed) — the age does not belong to the body being served: {cached}"
    );
    assert_eq!(
        cached["fresh"], true,
        "a reading half an hour old is inside the hour skein re-reads at, so nothing should be \
         marking it out of date: {cached}"
    );
}

/// The two ways a reading is ever re-read, and the one way it is not.
///
/// Each assertion, and the change that makes it fail:
///
/// | assertion | what breaks it |
/// |---|---|
/// | a two-hour-old reading is replaced on a plain load | a route that serves the cache whatever its age — the hourly ceiling gone |
/// | its `age_secs` is near zero afterwards | a route that re-reads but keeps the old timestamp |
/// | a third transcript is NOT picked up by the next plain load | a route that re-reads on every page load, which is the thing the owner asked for by name |
/// | `?refresh=1` picks it up | a route that ignores the parameter, or parses it as something other than "the caller asked" |
#[test]
fn a_usage_reading_is_re_read_when_the_hour_is_up_and_when_the_caller_asks() {
    let home = usage_home("usage-reread");
    let (child, addr) = usage_server(&home);
    let _kid = Kid(child);

    let first = usage_json(&addr, "/api/usage?refresh=1");
    assert_eq!(first["totals"]["tokens"], USAGE_TOKENS_1);

    // Two hours old, and a second transcript on disk: the ceiling is up, so a plain load pays.
    add_transcript(&home, "two", "2026-09-02", USAGE_T2);
    backdate(&home, 7200);
    let reread_at = Instant::now();
    let aged = usage_json(&addr, "/api/usage");
    assert_eq!(
        aged["transcripts_read"], 2,
        "a reading two hours old was served rather than retaken — the hourly ceiling is not \
         being applied: {aged}"
    );
    assert_eq!(aged["totals"]["tokens"], USAGE_TOKENS_2);
    let age = aged["age_secs"].as_u64().unwrap_or_default();
    assert!(
        age <= reread_at.elapsed().as_secs() + 1,
        "the fleet was re-read and the answer still carries the old reading's age of {age}s: {aged}"
    );

    // A third transcript against a reading taken seconds ago. This is the page-load case, and it
    // must cost nothing: the file on disk is invisible until someone asks.
    add_transcript(&home, "three", "2026-08-15", USAGE_T3);
    let inside = usage_json(&addr, "/api/usage");
    assert_eq!(
        inside["transcripts_read"], 2,
        "a page load walked the fleet's transcripts. The owner's rule is that it never does: \
         {inside}"
    );
    assert_eq!(inside["totals"]["tokens"], USAGE_TOKENS_2);

    // And asking is what makes it visible, inside the hour or not.
    let asked = usage_json(&addr, "/api/usage?refresh=1");
    assert_eq!(
        asked["transcripts_read"], 3,
        "?refresh=1 did not re-read the fleet, so the pane's Refresh button does nothing: {asked}"
    );
    assert_eq!(asked["totals"]["tokens"], USAGE_TOKENS_3);
    assert_eq!(
        asked["months"].as_array().map(|a| a.len()),
        Some(2),
        "the third transcript is in a different month from the other two, so a re-read that \
         found it must report two months: {asked}"
    );
}

/// The very first page load, on a host that has never counted anything, answers with a reading.
///
/// **This pins a judgement call rather than a rule.** There is no stored tally before the first
/// scan, so the 5.2 s that scan costs on a real fleet has to happen somewhere, and the route blocks
/// the first request rather than answering "nothing read yet" and scanning in the background. The
/// reason is what the page does next: `loadUsage` in `src/web/index.html` fetches once when the
/// pane is opened and never polls, so a background scan would finish into a page with no way to
/// hear about it — the person would be looking at "not read yet" until they pressed Refresh, which
/// would either queue behind the scan already running or start a second one over the same bytes.
///
/// So this is the assertion that a plain load with no cache is answered with the numbers, not with
/// an empty state. The change that breaks it is exactly the alternative design: return early when
/// `usage::cached()` is `None`, and scan on a detached task.
#[test]
fn the_first_usage_load_on_a_host_that_has_never_counted_takes_the_reading() {
    let home = usage_home("usage-cold");
    // Nothing has ever been counted here — this is what makes it the cold path rather than a
    // repeat of the other two, and it is asserted rather than assumed, because `usage_home` could
    // grow a cache one day and this test would quietly stop testing what it is named for.
    assert!(
        !home.join("usage.json").exists(),
        "the fixture already has a stored reading, so this is not the cold start it claims to be"
    );
    let (child, addr) = usage_server(&home);
    let _kid = Kid(child);

    // A plain load. No `?refresh=1` anywhere in this test.
    let first = usage_json(&addr, "/api/usage");
    assert_eq!(
        first["transcripts_read"], 1,
        "the first plain load did not read the fleet, so the pane's first ever open shows no \
         figure and nothing will ever fetch again: {first}"
    );
    assert_eq!(first["totals"]["tokens"], USAGE_TOKENS_1);
    assert!(
        first["totals"]["cost"].as_f64().unwrap_or(0.0) > 0.0,
        "the cold read produced no cost: {first}"
    );
    assert!(
        home.join("usage.json").exists(),
        "the cold read answered without storing anything, so the next page load pays for the \
         whole walk again — which is the cost the cache exists to stop"
    );
}
