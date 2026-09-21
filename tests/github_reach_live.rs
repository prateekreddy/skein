//! The per-repo GitHub boundary, proven against a REAL fleet — the only place it can be (SKEIN-548).
//!
//! Every other test of scoping in this tree inspects what the launcher builds or what the credential
//! helper hands over. None of them can prove the property that matters — *a box scoped to one repo
//! cannot READ another private repo* — because that property is enforced by the interaction between
//! skein's routing and sbx's GitHub proxy, and the proxy is not reproducible under bwrap. What the
//! proxy does with a credential through it is not skein's to fix, and it has already gone both ways:
//! measured from inside a box, a garbage `Bearer` came back `200` through the proxy — the fleet
//! ACCOUNT, unbounded — on 2026-09-06 and again on 2026-09-15, and `401` on 2026-09-21.
//! `docs/threat-model.md`'s "GitHub through the sandbox proxy" row and the hourly `proxy_injection`
//! health check (`src/health.rs`, SKEIN-927) carry which of those is true today; this file does not.
//! Making the boundary real does not depend on which answer that is: the launcher puts the GitHub
//! hosts in `NO_PROXY` for a scoped box, so its git and gh reach GitHub DIRECT and are bounded by
//! the token the box actually holds — and GitHub refuses a private repo it was not granted, on an
//! injecting day and a non-injecting one alike.
//!
//! So this test needs a live fleet: a box that is genuinely scoped (its environment carries the
//! `NO_PROXY` the launcher set), and a second private repo the account can see but this box was not
//! granted. It is `#[ignore]` unless `SKEIN_LIVE_FLEET=1`, and it uses only garbage `skein-test-`
//! credentials — it never sends a real token anywhere.
//!
//! **Run un-ignored on a box that is NOT scoped (e.g. `SKEIN_GIT_SCOPE=fleet`), it fails in the
//! right place only while the proxy is injecting**: such a box then reaches GitHub through the proxy
//! and is answered as the account, so the second private repo is readable and the "refused"
//! assertion fails — which is precisely the hole scoping closes. On a day the proxy is not
//! injecting, an unscoped box gets the proxy's own `401` for a garbage credential and this sanity
//! check passes for the wrong reason; the proxy-credential command under `docs/threat-model.md`'s
//! "Checking this page" section says which day it is. On a scoped box the request goes direct
//! regardless of the proxy, and it passes for the right reason either way.

use std::process::Command;

/// The git smart-HTTP ref-advertisement endpoint for a repo, which answers `200` with real refs to a
/// credential that may read it and `401`/`404` to one that may not — the wire protocol itself, not
/// the REST API, so no `insteadOf` rewrite can quietly send this over SSH instead.
fn info_refs_url(slug: &str) -> String {
    format!("https://github.com/{slug}/info/refs?service=git-upload-pack")
}

/// The HTTP status curl gets for `url`, honouring THIS process's proxy environment exactly as a box
/// would — no `--noproxy` override, so on a scoped box `NO_PROXY` sends it direct and on an unscoped
/// one it rides the proxy. A garbage credential, always: what is under test is what the *route*
/// grants, never a real token.
fn status_honouring_env(url: &str) -> Option<u16> {
    let out = Command::new("curl")
        .args([
            "-sS",
            "-o",
            "/dev/null",
            "-w",
            "%{http_code}",
            "-m",
            "20",
            "-u",
            "x:skein-test-garbage-000",
            url,
        ])
        .output()
        .ok()?;
    String::from_utf8_lossy(&out.stdout).trim().parse().ok()
}

/// The same request forced through the proxy (no `--noproxy`), used only to DISCOVER a private repo
/// the account can see when the test was not handed one — never to assert the boundary.
fn discover_other_private_repo() -> Option<String> {
    if let Ok(slug) = std::env::var("SKEIN_LIVE_OTHER_PRIVATE") {
        if !slug.trim().is_empty() {
            return Some(slug.trim().to_string());
        }
    }
    // Ask the account (through whatever the environment provides) for a private repo. This is the
    // over-reach the boundary is about, used here only to find a target; its name is never printed.
    let out = Command::new("curl")
        .args([
            "-sS",
            "-m",
            "20",
            "https://api.github.com/user/repos?visibility=private&per_page=1",
        ])
        .output()
        .ok()?;
    let body = String::from_utf8_lossy(&out.stdout);
    let marker = "\"full_name\":";
    let start = body.find(marker)? + marker.len();
    let rest = body[start..].trim_start().strip_prefix('"')?;
    let slug = rest.split('"').next()?.to_string();
    (!slug.is_empty() && slug.contains('/')).then_some(slug)
}

/// A box scoped to one repo must not be able to READ another private repo.
///
/// The assertion that would make this fail: a scoped box's own route (proxy bypassed for GitHub)
/// returns `200` with refs for a private repo it holds no token for. On a correctly scoped box the
/// route is direct and GitHub answers `401`/`404`; on an unscoped box the proxy answers `200`, which
/// is the failure this exists to catch.
#[test]
#[ignore = "needs a live fleet: run with --ignored and SKEIN_LIVE_FLEET=1 (sbx proxy behaviour is not reproducible under bwrap)"]
fn a_box_scoped_to_one_repo_cannot_read_another_private_repo() {
    if std::env::var("SKEIN_LIVE_FLEET").ok().as_deref() != Some("1") {
        eprintln!("skipping: SKEIN_LIVE_FLEET is not 1");
        return;
    }

    let Some(other) = discover_other_private_repo() else {
        panic!(
            "no second private repo to test against — set SKEIN_LIVE_OTHER_PRIVATE=<owner>/<name> \
             to a private repo the fleet account can see but this box was not granted"
        );
    };

    // The box's OWN route. On a scoped box NO_PROXY carries github.com, so this goes direct; on an
    // unscoped box it rides the proxy. Either way it carries a garbage credential.
    let via_box = status_honouring_env(&info_refs_url(&other));
    assert!(
        matches!(via_box, Some(401) | Some(403) | Some(404)),
        "a scoped box read a private repo it was never granted (HTTP {via_box:?}): the boundary is \
         not holding. On a box in `fleet` mode this is expected — it rides the credential-injecting \
         proxy — and is exactly the hole scoping closes by routing GitHub direct (NO_PROXY)."
    );
}
