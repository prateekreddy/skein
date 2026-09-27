//! What a person is shown in a box's pane: an uncovered box refused until it is allowed,
//! and a box that already has a login never asked to onboard.

use super::*;

pub(super) fn poll_for(mut done: impl FnMut() -> bool, budget: Duration) -> bool {
    let until = std::time::Instant::now() + budget;
    loop {
        if done() {
            return true;
        }
        if std::time::Instant::now() >= until {
            return false;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// What is actually on a box's own terminal, with the wrapping taken out.
///
/// `capture-pane` reports the pane as the pty holds it, so an 80-column wrap puts a line break in
/// the middle of a sentence — collapsing the whitespace is what stops that from deciding whether an
/// assertion passes. `-J` joins what tmux itself knows is one wrapped line; the collapse covers the
/// rest.
fn pane_text(name: &str) -> String {
    let out = Command::new("tmux")
        .args([
            "-S",
            &box_sock(name),
            "capture-pane",
            "-p",
            "-J",
            "-t",
            "skein-shell",
        ])
        .output()
        .expect("tmux capture-pane");
    String::from_utf8_lossy(&out.stdout)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// The pane, once it has had a chance to print — bounded, and returning whatever it has either way.
///
/// `session_script` returns as soon as tmux has the session and the anchor pid can be read, which
/// is not quite the same instant as the pane's first process having written anything. Waiting on
/// the post-condition rather than sleeping a fixed amount is the same rule `anchor_gone` above is
/// written to; the text is returned unconditionally so the assertion, not this helper, is what
/// fails and says what it wanted.
fn pane_once_it_speaks(name: &str, want: &str) -> String {
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        let text = pane_text(name);
        if text.contains(want) || std::time::Instant::now() > deadline {
            return text;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// A box skein cannot match to a repository is **refused**, and once allowed it **says so where a
/// person is** — on its own terminal and on the stderr of the command that started it (SKEIN-846,
/// SKEIN-836).
///
/// **Why this is one test and not four.** Every step consumes the one before it: there is no pane
/// to read until a box has started, no placement to attach to until `start_box` has recorded one,
/// and no way to prove the refusal is a refusal *of this condition* rather than of everything
/// unless a covered box comes up in the same fixture, from the same call, moments later. An absence
/// that was never a presence proves nothing.
///
/// **What would make each assertion fail — named before it was written, and each one then made to
/// fail by doing exactly this:**
///
///   * *the refusal* — deleting the `refuse_if_uncovered(name)?` line from `fleet::start_box_inner`.
///     The unmatched box then starts, and `.expect_err` finds an `Ok`.
///   * *the covered box still starting* — making `box_exposure` return `Uncovered` for everything,
///     or dropping the `uncovered_is_allowed` arm so the permission is never read. Either way the
///     second and third starts fail and the first assertion goes on passing, which is the whole
///     reason this half is here.
///   * *surface 1, the box's own terminal* — deleting the `pane_cmd=(sh -c …)` wrapper from
///     `box-session.sh`. The banner is still written, still correct, and the pane is empty: exactly
///     the state this item found, reproduced.
///   * *surface 2, the command that started it* — deleting the `printf 'SKEIN_NOTICE %s\n'` from
///     `box-session.sh` (leaving the `echo … >&2` that was there for months), or deleting the
///     `say_what_the_launcher_said` call from `fleet::ensure_box_session`. The real `skein attach`
///     run below then prints nothing about the cover, which is what it did before this.
///
/// None of those are assertions about a string being present in a file. `cockpit.rs`'s
/// `the_workshop_switch_says_what_it_grants` is the one that checks the *wording*, and it says so.
#[test]
fn an_uncovered_box_is_refused_until_it_is_allowed_and_then_says_so_where_someone_is_looking() {
    let _env = env_lock();
    let _real = skein::place::seam::real_crossings();
    if !bwrap_works() || !have("tmux") || !have("git") {
        return skip(
            "this machine cannot make a bwrap namespace, or lacks tmux/git, so it cannot host a box",
        );
    }
    let root = scratch_named("cover");
    write_fake_sbx(&root.join("bin"));
    let remote = write_remote(&root);
    let sandbox_home = sandbox_home_with_agent(&root);

    // `demo-…` matches the repo registered below by the longest-id-prefix rule `repo_for_box` uses;
    // `adrift-…` matches nothing, which is the entire difference between them. Both are cloned from
    // the same remote and started by the same call, so nothing else can explain a difference.
    let covered = "demo-cover";
    let adrift = "adrift-cover";

    let mut pins = env_pins();
    pins.set("SKEIN_WARDEN", "127.0.0.1:1");
    pins.set(
        "PATH",
        format!(
            "{}:{}",
            root.join("bin").display(),
            std::env::var("PATH").unwrap_or_default()
        ),
    )
    .set("SKEIN_HOME", root.join("skein"))
    .set("SKEIN_FLEET_ROOT", root.join("boxes"))
    .set("SKEIN_RUNTIME_PACKAGES", "")
    .set("HOME", &sandbox_home)
    .set("SKEIN_LS_CMD", format!("echo '[{{\"name\":\"{FLEET}\"}}]'"));

    let store = root.join("store");
    fs::create_dir_all(&store).unwrap();
    ensure_store(&store).expect("seed the store");
    ensure_probe_in(&store).expect("seed the store's scripts");
    let repo = Repo {
        read_prs: false,
        id: "demo".into(),
        source: remote.clone(),
        store: store.to_string_lossy().into_owned(),
        plane_project: String::new(),
        sync_connection: String::new(),
        review_queue: true,
        sync_gateway_url: String::new(),
        ..Default::default()
    };
    save_repos(std::slice::from_ref(&repo)).expect("register the repo");
    let mut config = load_config();
    config.fleet_sandbox = FLEET.into();
    save_config(&config).expect("turn the fleet on");
    // The cover binds each box's own state directory back through the tmpfs it lays over their
    // parent, and bwrap refuses a bind whose source does not exist — so a fixture that skipped this
    // would fail at the namespace rather than at the thing under test.
    for name in [covered, adrift] {
        fs::create_dir_all(box_state(name)).unwrap();
    }

    // ---- 1. refused, before anything is built ----
    let refusal = start_box(
        adrift,
        &repo,
        "feat/cover",
        "exec sleep 300",
        skein::place::Purpose::Manual,
    )
    .expect_err("a box skein cannot cover was started with nobody having chosen that");
    assert!(
        refusal.contains("UNCOVERED"),
        "the refusal does not name the condition it is refusing on: {refusal}"
    );
    for step in ["`skein repos`", "`skein add", "--uncovered"] {
        assert!(
            refusal.contains(step),
            "the refusal never mentions `{step}`, so it names a wall and no way over or around \
             it — which is the one thing a refusal must not do: {refusal}"
        );
    }
    assert!(
        !Path::new(&format!("{}/tree", box_root(adrift))).exists(),
        "the refusal came after the clone, so a box nobody may start now has a checkout"
    );

    // ---- 2. and a covered box, from the same call, is not ----
    start_box(
        covered,
        &repo,
        "feat/cover",
        "exec sleep 300",
        skein::place::Purpose::Manual,
    )
    .expect("a box whose name matches a repository must still start");

    // ---- 3. allowed, deliberately, and then it starts ----
    // `skein start <box> --uncovered` writes exactly this, in `src/bin/skein.rs`.
    skein::fleet::allow_uncovered(adrift, true).expect("record the permission");
    start_box(
        adrift,
        &repo,
        "feat/cover",
        "exec sleep 300",
        skein::place::Purpose::Manual,
    )
    .expect("the permission was recorded and the refusal still stood");
    assert!(
        shared_record(adrift).is_some(),
        "the allowed box reported success without leaving a placement"
    );

    // ---- 4. surface 1: the box's own terminal ----
    let said = pane_once_it_speaks(adrift, "UNCOVERED");
    assert!(
        said.contains(&format!("{adrift} came up UNCOVERED")),
        "nothing on the box's own terminal says it came up uncovered, so anyone who attaches or \
         opens a shell in it sees a box exactly like every other one. Pane: {said:?}"
    );
    assert!(
        said.contains("every other repo's store and work tree"),
        "the banner reached the pane with its reach edited out of it: {said:?}"
    );
    let quiet = pane_text(covered);
    assert!(
        !quiet.contains("UNCOVERED") && !quiet.contains("WORKSHOP"),
        "a covered box is told it came up uncovered, so the banner is firing on every box and \
         means nothing. Pane: {quiet:?}"
    );

    // ---- 5. surface 2: the stderr of the command a person actually ran ----
    // The real binary, not this process: the thing under test is that `skein attach` puts the
    // launcher's words in front of whoever typed it, and this process's `eprintln!` is captured by
    // the test harness where nothing can read it. The session is ended first so that attaching has
    // to relaunch — a live session is a no-op and would prove nothing.
    let anchor = shared_record(adrift).unwrap().ns_pid;
    own_sandbox(FLEET)
        .exec(
            &format!("tmux -S {} kill-server", box_sock(adrift)),
            Duration::from_secs(30),
        )
        .expect("end the session so the attach has to start one");
    anchor_gone(anchor);
    let attach = Command::new(env!("CARGO_BIN_EXE_skein"))
        .args(["attach", adrift])
        .env(
            "PATH",
            format!(
                "{}:{}",
                root.join("bin").display(),
                std::env::var("PATH").unwrap_or_default()
            ),
        )
        .env("SKEIN_HOME", root.join("skein"))
        .env("SKEIN_FLEET_ROOT", root.join("boxes"))
        .env("SKEIN_RUNTIME_PACKAGES", "")
        .env("SKEIN_WARDEN", "127.0.0.1:1")
        .env("HOME", &sandbox_home)
        .env("SKEIN_LS_CMD", format!("echo '[{{\"name\":\"{FLEET}\"}}]'"))
        .stdin(std::process::Stdio::null())
        .output()
        .expect("run the real skein");
    // The attach itself has nowhere to attach to — there is no terminal on this pipe — and that is
    // not what is being read. What is being read is everything `skein` said on its way there.
    let told = String::from_utf8_lossy(&attach.stderr).to_string();
    assert!(
        told.contains("came up UNCOVERED"),
        "`skein attach {adrift}` relaunched an uncovered box and told the person nothing about it \
         — which is what every launch did while `box-session.sh` wrote this to a stderr that is \
         piped and dropped on success. Its whole stderr was: {told}"
    );
    // And the permission persisted, which is what keeps this from stranding anybody: nobody typed
    // `--uncovered` here.
    assert!(
        !told.contains("so skein has not started it"),
        "the permission did not outlive the command that gave it, so every attach of this box \
         meets a refusal it cannot answer from inside the server: {told}"
    );

    // ---- 6. and nothing this started is left running ----
    for name in [covered, adrift] {
        let anchor = shared_record(name).map(|r| r.ns_pid);
        stop_box(name).expect("stop the box");
        if let Some(anchor) = anchor {
            anchor_gone(anchor);
        }
        destroy_box(name).expect("destroy the box");
    }
}

// -------------------------------------------------------------------------------------------------
// Seeding a login and seeding "you have logged in before" are one act (SKEIN-957)
// -------------------------------------------------------------------------------------------------
//
// Every new box in the owner's fleet opened on Claude Code's login screen while holding a working
// credential, because the screen is gated on `hasCompletedOnboarding` in `~/.claude.json` and skein
// wrote that key nowhere. The launcher writes it now, next to the credential merge, and what is
// asserted below is the INVARIANT rather than one file's contents: whatever the launcher decides a
// box's login is, a box that HAS one must not be asked to onboard. A test pinned to an example pair
// of JSON blobs would stay green under a seed path that learned to forget the flag somewhere else.

/// A block of the launcher, lifted from the first line beginning `from` up to and including the
/// first line after it that is exactly `to`.
///
/// Read out of `box-session.sh` rather than copied, so a change to the launcher is a change to what
/// these tests run — a copy would keep passing against the version it was written from. It panics
/// rather than returning an empty block: a landmark that has moved must fail loudly here, not
/// quietly hand the shell nothing to run and report that nothing went wrong.
pub(super) fn launcher_block(from: &str, to: &str) -> String {
    let lines: Vec<&str> = LAUNCHER.lines().collect();
    let start = lines
        .iter()
        .position(|l| l.starts_with(from))
        .unwrap_or_else(|| {
            panic!("box-session.sh has no line beginning `{from}` any more, so this harness is not lifting what it names")
        });
    let end = lines[start + 1..]
        .iter()
        .position(|l| *l == to)
        .map(|i| start + 1 + i)
        .unwrap_or_else(|| {
            panic!("the block beginning `{from}` in box-session.sh does not end at a line `{to}`")
        });
    lines[start..=end].join("\n")
}

/// The launcher's own `login_life` — the judgement that decides whether a file is a login at all.
///
/// Asked by RUNNING the launcher's copy rather than by reading the JSON here: a second opinion
/// written in Rust would be a second spelling of the rule, which is how the credential merge and the
/// host's heal once elected opposite winners on the same five files.
fn launcher_says_there_is_a_login(credential: &Path) -> bool {
    let block = launcher_block("login_life() {", "}");
    assert!(
        block.contains("refreshTokenExpiresAt"),
        "the lifted `login_life` no longer asks about an expired refresh token, so this harness is \
         running something other than the launcher's judgement of what a login is"
    );
    Command::new("bash")
        .arg("-c")
        .arg(format!(
            "set -uo pipefail\n{block}\nlogin_life '{}'",
            credential.display()
        ))
        .output()
        .expect("bash runs the launcher's login test")
        .status
        .success()
}

/// Run the launcher's onboarding block against one fixture home, exactly as a box start runs it.
fn run_onboarding_block(home: &Path) -> std::process::Output {
    let block = launcher_block("if command -v python3 >/dev/null 2>&1 && login_life", "fi");
    assert!(
        block.contains("hasCompletedOnboarding"),
        "the lifted block no longer writes the key the onboarding screen is gated on, so this \
         harness is running something that cannot answer the question it was written for"
    );
    let life = launcher_block("login_life() {", "}");
    Command::new("bash")
        .arg("-c")
        .arg(format!(
            "set -uo pipefail\nhome='{}'\n{life}\n{block}",
            home.display()
        ))
        .output()
        .expect("bash runs the launcher's onboarding block")
}

/// Every box home under `fleet_root` the launcher itself would say carries a login.
pub(super) fn homes_carrying_a_login(fleet_root: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let Ok(entries) = fs::read_dir(fleet_root) else {
        return found;
    };
    let mut boxes: Vec<PathBuf> = entries.filter_map(|e| e.ok()).map(|e| e.path()).collect();
    boxes.sort();
    for at in boxes {
        let home = at.join("home");
        if launcher_says_there_is_a_login(&home.join(".claude/.credentials.json")) {
            found.push(home);
        }
    }
    found
}

/// What `~/.claude.json` says about onboarding, for a home that has one.
pub(super) fn onboarding_flag(home: &Path) -> Option<serde_json::Value> {
    let text = fs::read_to_string(home.join(".claude.json")).ok()?;
    let parsed: serde_json::Value = serde_json::from_str(&text).ok()?;
    parsed.get("hasCompletedOnboarding").cloned()
}

/// A box that ends up with a login is never asked to onboard — whatever its `~/.claude.json` was.
///
/// The antecedent is the launcher's own `login_life`, so the case table below says what is on disk
/// and never what the answer should be: swap a credential for a husk, or for a file whose refresh
/// token expired in 2001, and the expectation follows the launcher instead of contradicting it.
///
/// **What makes each assertion fail**, planted and watched before this was believed:
///
///   * deleting `data["hasCompletedOnboarding"] = True` from the launcher — the invariant fails for
///     every case that carries a login;
///   * replacing the read-modify-write with `data = {}` before it, i.e. writing the file from a
///     template — `a key that was there is gone` fails, naming the key;
///   * making the unparseable case write anyway (`except ValueError: data = {}`) — `left exactly as
///     it was` fails on the byte comparison;
///   * dropping `&& login_life …` from the condition, so the flag is written with no credential —
///     `a box with no login must still be asked` fails.
#[test]
fn a_box_that_has_a_login_is_never_asked_to_onboard() {
    if !have("python3") {
        return skip("the launcher writes this key with python3, and there is none here");
    }
    // A live login, a husk a logout leaves behind, and a credential whose refresh token died in
    // 2001 — the last two are files, and neither is a login.
    const LOGIN: &str = r#"{"claudeAiOauth":{"accessToken":"a","refreshToken":"r"}}"#;
    const HUSK: &str = r#"{"claudeAiOauth":{"accessToken":"","refreshToken":""}}"#;
    const SPENT: &str = r#"{"claudeAiOauth":{"accessToken":"a","refreshToken":"r","refreshTokenExpiresAt":1000000000000}}"#;
    // `(case, what is in .claude/.credentials.json, what is in .claude.json)`. `None` is a file
    // that is not there at all, which is what a brand-new box's home looks like.
    let cases: [(&str, Option<&str>, Option<&str>); 10] = [
        ("no .claude.json at all", Some(LOGIN), None),
        ("an empty .claude.json", Some(LOGIN), Some("")),
        (
            "a used one, with a record in it",
            Some(LOGIN),
            Some(r#"{"projects":{"/w":{"history":["a turn"]}},"userID":"u","numStartups":46}"#),
        ),
        (
            "one that already says so",
            Some(LOGIN),
            Some(r#"{"hasCompletedOnboarding":true,"userID":"u"}"#),
        ),
        (
            "one that says the opposite",
            Some(LOGIN),
            Some(r#"{"hasCompletedOnboarding":false,"userID":"u"}"#),
        ),
        ("one that is not JSON", Some(LOGIN), Some("not json at all")),
        (
            "one that is JSON but not an object",
            Some(LOGIN),
            Some("[1, 2, 3]"),
        ),
        ("a husk, not a login", Some(HUSK), Some(r#"{"userID":"u"}"#)),
        (
            "a login whose refresh token is spent",
            Some(SPENT),
            Some(r#"{"userID":"u"}"#),
        ),
        ("no credential at all", None, Some(r#"{"userID":"u"}"#)),
    ];

    // The same fixture family as every other test in this file, so the leak scan keeps deriving
    // one set of names from this binary. Nothing here starts a process; the block under test is
    // bash and a python that exits.
    let dir = scratch_named("onboard");
    let mut seen_with_a_login = 0;
    let mut seen_without = 0;
    for (n, (case, credential, before)) in cases.iter().enumerate() {
        let home = dir.join(format!("home-{n}"));
        fs::create_dir_all(home.join(".claude")).unwrap();
        if let Some(body) = credential {
            fs::write(home.join(".claude/.credentials.json"), body).unwrap();
        }
        if let Some(body) = before {
            fs::write(home.join(".claude.json"), body).unwrap();
        }

        let out = run_onboarding_block(&home);
        assert!(
            out.status.success(),
            "the launcher's onboarding block failed for `{case}`: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let said = String::from_utf8_lossy(&out.stderr).to_string();
        let after = fs::read_to_string(home.join(".claude.json")).ok();
        let has_login = launcher_says_there_is_a_login(&home.join(".claude/.credentials.json"));
        let was: Option<serde_json::Value> = before
            .filter(|b| !b.trim().is_empty())
            .map(|b| serde_json::from_str(b).unwrap_or(serde_json::Value::Null));
        let could_extend = was.as_ref().map(|v| v.is_object()).unwrap_or(true);

        if !has_login {
            seen_without += 1;
            // Skein does not fabricate "you have logged in before" for a box it handed nothing to:
            // that box genuinely has to log in, and hiding the screen it does that on would leave
            // it stranded in front of an agent that cannot answer.
            assert_eq!(
                after.as_deref(),
                *before,
                "a box with no login must still be asked to log in, and `{case}` had its \
                 ~/.claude.json written anyway"
            );
            continue;
        }
        seen_with_a_login += 1;

        if !could_extend {
            // `~/.claude.json` is Claude Code's file and holds the box's whole project record. A
            // shape skein cannot read is left alone and said out loud — the person meets one
            // onboarding prompt, which is where they are today, instead of losing the record.
            assert_eq!(
                after.as_deref(),
                *before,
                "`{case}` was rewritten from a template; a file skein cannot parse must be left \
                 exactly as it was"
            );
            assert!(
                said.contains(".claude.json"),
                "`{case}` was left alone in silence, so nobody can tell why the box still asks to \
                 onboard: {said:?}"
            );
            continue;
        }

        // THE INVARIANT.
        let text = after.expect("a home with a login must end up with a ~/.claude.json");
        let parsed: serde_json::Value = serde_json::from_str(&text)
            .unwrap_or_else(|e| panic!("`{case}` left ~/.claude.json unreadable ({e}): {text}"));
        assert_eq!(
            parsed.get("hasCompletedOnboarding"),
            Some(&serde_json::Value::Bool(true)),
            "the launcher handed `{case}` a credential and left it needing to onboard, which is \
             the login screen on every new box: {text}"
        );
        // And it added a key rather than replacing a file: everything that was there is still
        // there, with the value it had.
        if let Some(serde_json::Value::Object(before)) = &was {
            for (key, value) in before {
                if key == "hasCompletedOnboarding" {
                    continue;
                }
                assert_eq!(
                    parsed.get(key),
                    Some(value),
                    "`{case}`: the key `{key}` was in ~/.claude.json and is not in what skein \
                     wrote back — this file carries the box's project history, and it is not \
                     skein's to replace: {text}"
                );
            }
        }
    }
    // Neither half of the table may quietly empty out: a run that saw no login proves nothing about
    // the invariant, and one that saw no husk proves nothing about the restraint beside it.
    assert!(
        seen_with_a_login >= 7 && seen_without == 3,
        "the launcher's own `login_life` read this table as {seen_with_a_login} logins and \
         {seen_without} non-logins, which is not the split these cases were written to have — \
         either a fixture has stopped being what it says it is, or the judgement moved"
    );
}
