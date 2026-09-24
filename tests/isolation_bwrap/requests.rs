//! A box may write its own request queues and no other box's, and cannot answer its own
//! git-write request.

use super::*;

/// **A box may write its own request queue entry and no other box's** (ISO-7).
///
/// Architecture §8.4 puts three steps in order — bind the artifact, make the request path per box,
/// *then* unmask the queue — and the middle one was skipped. One shared read-write `requests/`
/// directory let every box delete, rewrite or flip the state of every other box's pending request,
/// and file one in a neighbour's name. On the gitgate queue that last one is not an attribution
/// nicety: `gitgate::decide` builds the grant from the request's box and the refresher writes the
/// minted GitHub token into the box the grant names, so an approval a person read as one box's ask
/// put a live write token in another's.
///
/// **Asserted against a real namespace, because it cannot be asserted anywhere else.** The
/// launcher's refusal is `--ro-bind` on the queue root with `--bind` on one directory under it, and
/// a bind list read as text says only what the arguments were. Every box in this fleet is uid 1000
/// and mode bits stop none of it; what stops it is the mount, and the mount is what bwrap builds.
#[test]
fn a_box_can_write_its_own_request_queue_and_no_other_boxs() {
    if !bwrap_works() {
        return skip(
            "bwrap cannot create \
             a user namespace here, so the per-box drop-box was NOT exercised",
        );
    }
    let fleet = Fleet::make("queues");
    let report = fleet.seen_by_box(Born::Covered);

    for queue in ["substrate", "gitgate"] {
        let root = fleet.fleet_root.join(format!(".skein/{queue}/requests"));
        assert_eq!(
            verdict(&report, &root.join("web-main")),
            "write",
            "the box cannot file a {queue} request at all, which is the defect the unmask was for:\n{report}"
        );
        // The whole finding. `see` and not `gone`: boxes share a uid and the queue root is
        // deliberately readable, so a box can still READ a neighbour's ask — that is what lets a
        // repeated ask collapse across boxes. What it must not do is write one.
        assert_eq!(
            verdict(&report, &root.join("other-main")),
            "see",
            "a box can write another box's {queue} drop-box, so it can rewrite, delete or \
             impersonate that box's pending requests:\n{report}"
        );
        // And it cannot make itself a drop-box under someone else's name either, which is the
        // half a per-directory check would miss.
        assert_eq!(
            verdict(&report, &root),
            "see",
            "the {queue} queue root is writable, so a box can create or remove another box's \
             drop-box:\n{report}"
        );
    }
}

/// **A box can write its own ask queue and no other box's** (box-plugin §4, SKEIN-1061), through
/// the tool an agent actually calls.
///
/// The `skein_ask_person` tool runs inside the box, as the box, and writes one file into
/// `.skein/asks/requests/<box>/`. Which box that is, is the tool's `$SKEIN_BOX` — a variable the box
/// owns — so the name cannot be what stops a box filing a question under a neighbour's name. The
/// mount is: the launcher binds this box's own drop-box read-write and leaves every other one under
/// the read-only `.skein`. So this runs the shipped server inside the namespace twice, once as
/// itself and once claiming to be `other-main`, and reads both drop-boxes back from outside.
///
/// The probe verdicts come first, so a cover that stops holding is named as that before the tool
/// is asked anything.
///
/// **What would make it fail**: `asks` dropped from the launcher's `for asking in substrate gitgate
/// asks` loop (the box cannot ask at all), or the bind widened to `.skein/asks/requests` (the
/// neighbour's question lands).
#[test]
fn a_box_can_write_its_own_ask_queue_and_no_other_boxs() {
    if !bwrap_works() {
        return skip(
            "bwrap cannot create \
             a user namespace here, so the per-box ask queue was NOT exercised",
        );
    }
    let fleet = Fleet::make("asks");
    let report = fleet.seen_by_box(Born::Covered);
    let root = fleet.fleet_root.join(".skein/asks/requests");
    assert_eq!(
        verdict(&report, &root.join("web-main")),
        "write",
        "the box cannot write its own ask queue:\n{report}"
    );
    assert_eq!(
        verdict(&report, &root.join("other-main")),
        "see",
        "a box can write another box's ask queue:\n{report}"
    );
    assert_eq!(
        verdict(&report, &root),
        "see",
        "the ask queue root is writable:\n{report}"
    );

    // The shipped server, where the launcher installs the plugin: under the read-only `.skein`.
    let server = fleet.fleet_root.join(".skein/plugin/bin/skein-mcp");
    fs::create_dir_all(server.parent().unwrap()).unwrap();
    fs::write(&server, include_str!("../../src/plugin/bin/skein-mcp")).unwrap();
    let call = r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"skein_ask_person","arguments":{"question":"keep the table?","options":["Drop it","Keep it"]}}}"#;
    let said = String::from_utf8_lossy(
        &fleet.in_box(
            Born::Covered,
            r#"for who in web-main other-main; do
  printf '%s: ' "$who"
  printf '%s\n' "$1" | SKEIN_BOX="$who" SKEIN_STATE="$3" python3 "$2"
done"#,
            &[
                call.to_string(),
                server.to_string_lossy().into_owned(),
                fleet
                    .state_parent
                    .join("web-main")
                    .to_string_lossy()
                    .into_owned(),
            ],
        ),
    )
    .to_string();

    let questions_in = |owner: &str| -> Vec<String> {
        let mut v: Vec<String> = fs::read_dir(root.join(owner))
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with("q-"))
            .collect();
        v.sort();
        v
    };
    assert_eq!(
        questions_in("web-main").len(),
        1,
        "the tool could not file the box's own question:\n{said}"
    );
    assert_eq!(
        questions_in("other-main"),
        Vec::<String>::new(),
        "a box filed a question in another box's queue by naming it:\n{said}"
    );
    assert!(
        said.contains(r#"not-recorded"#),
        "the tool did not say the neighbour's question was not recorded:\n{said}"
    );
}

/// **A box cannot answer its own git-write request, and cannot hide it from its owner** (SKEIN-940).
///
/// A box's drop-box is bound read-write into it, so every byte of its request is the box's —
/// `state` included. The cockpit offers Grant and Deny only on a request whose state is `pending`,
/// so a box that wrote `"state": "granted"` into its own ask used to be drawn as answered with
/// nothing to press, and its owner was never asked. The answer now lives on the host
/// (`gitgate::decision_path`, beside `git-grants.json`) and `gitgate::list` believes only that.
///
/// **So the defence is a boundary, and this runs it.** The box writes `granted` into its own ask
/// and then tries to write the answer where the host keeps it, and a grant where the host keeps
/// those. Afterwards the real `gitgate::list` reads the fixture fleet the way the cockpit's poll
/// does, and must still say `pending`.
///
/// **The workshop box runs the same forgery as the control.** It skips the isolation block, so its
/// writes land, and `list` must then say `granted`. That is what makes the covered box's `pending`
/// a property of the cover rather than of a forgery aimed at a path nobody reads: if the paths the
/// script writes were not the ones `list` and `grants` read, the control would fail first.
///
/// **What would make it fail**: `list` returning the box's own state (`Ok(parse_requests(&out))`),
/// or `gitgate::decision_path` moved somewhere the box can write, such as under its own drop-box.
#[test]
fn a_box_cannot_answer_its_own_git_write_request() {
    if !bwrap_works() {
        return skip(
            "bwrap cannot create \
             a user namespace here, so a box answering its own git-write request was NOT exercised",
        );
    }
    if !common::have("jq") {
        return skip("no jq, so the git-write queue cannot be read at all");
    }
    let id = "20260922-120000-4242";
    // Held across both runs: each pins `$SKEIN_HOME` and `$SKEIN_FLEET_ROOT` to its own fixture
    // while `gitgate` reads them.
    let _lock = common::env_lock();

    let covered = forge_an_answer(&Fleet::make_on_volume("git-answer"), Born::Covered, id);
    let workshop = forge_an_answer(&Fleet::make_on_volume("git-answer-ws"), Born::Workshop, id);

    // The premise: the box really did put `granted` in the file it controls. Without this, a
    // `pending` below could be a request that never said anything else.
    assert!(
        covered.queue_file.contains("\"granted\""),
        "the box could not write `granted` into its own request, so nothing below is tested:\n{}",
        covered.said
    );
    // The boundary: nothing the box wrote reached where the host keeps its answers and grants.
    // Before the display assertion, so a cover that stops holding is named as that.
    assert!(
        !covered.decision_on_host && covered.grants == 0,
        "a covered box wrote the host's record of what its owner decided (answer on disk: {}, \
         grants: {})\nthe box said:\n{}",
        covered.decision_on_host,
        covered.grants,
        covered.said
    );
    // And what the owner is shown: the request, still waiting on them.
    let ask = covered
        .shown
        .iter()
        .find(|r| r.id == id)
        .unwrap_or_else(|| {
            panic!(
                "the box's request is not in what the cockpit is shown at all — hidden from its \
                 owner:\n{:?}\n{}",
                covered.shown, covered.said
            )
        });
    assert_eq!(
        ask.state, "pending",
        "a box wrote `granted` into its own request and the cockpit is shown it as {:?}: no Grant \
         button, and its owner is never asked\nthe box said:\n{}",
        ask.state, covered.said
    );

    // The control: the same script, from the one box the cover deliberately leaves out.
    assert_eq!(
        workshop
            .shown
            .iter()
            .find(|r| r.id == id)
            .map(|r| r.state.as_str()),
        Some("granted"),
        "the workshop box's forgery did not reach what `list` reads either, so the covered box's \
         `pending` above says nothing about the cover\nthe box said:\n{}",
        workshop.said
    );
    assert_eq!(
        workshop.grants, 1,
        "the workshop box's forged grant is not what `gitgate::grants` reads, so the covered \
         box's empty grants above prove nothing\nthe box said:\n{}",
        workshop.said
    );
}

/// What one run of [`forge_an_answer`] left behind, read from outside the box.
struct Forged {
    /// The box's own report of each write it attempted.
    said: String,
    /// The request as it is on disk afterwards, which the box wrote.
    queue_file: String,
    /// What `gitgate::list` — what the cockpit's poll calls — says about the fleet's requests.
    shown: Vec<skein::gitgate::Request>,
    /// Whether an answer exists where the host keeps them.
    decision_on_host: bool,
    grants: usize,
}

/// A box of `web-main` files a git-write request already marked `granted`, and tries to write the
/// matching answer and grant where the host keeps them; then the host reads the fleet back.
///
/// `$SKEIN_HOME` is the fixture's volume and `$SKEIN_FLEET_ROOT` its fleet root, pinned through
/// `common::env_pins` for the length of the read and put back before anything is asserted — on a
/// panic too. The caller holds `common::env_lock`, which is not re-entrant. The two host paths are derived from the library under
/// those pins rather than spelled here.
fn forge_an_answer(fleet: &Fleet, born: Born, id: &str) -> Forged {
    let volume = fleet.volume.clone().expect("this fleet is on a volume");
    let queue = fleet
        .fleet_root
        .join(format!(".skein/gitgate/requests/web-main/{id}.json"));

    let mut env = common::env_pins();
    env.set("SKEIN_HOME", &volume)
        .set("SKEIN_FLEET_ROOT", &fleet.fleet_root);

    let decision = skein::gitgate::decision_path("web-main", id).expect("a nameable request");
    let grants_file = volume.join("git-grants.json");
    let forge = r#"
q="$1"; d="$2"; g="$3"; id="$4"
body=$(printf '{"id":"%s","box":"web-main","repo":"acme/thing","reason":"x","asked":"2026-09-22T12:00:00Z","state":"granted","decided":"2026-09-22T12:00:01Z"}' "$id")
printf '%s\n' "$body" >"$q" 2>/dev/null && echo "asked $q" || echo "could not ask $q"
mkdir -p "$(dirname "$d")" 2>/dev/null && printf '%s\n' "$body" >"$d" 2>/dev/null \
  && echo "wrote the answer $d" || echo "could not write the answer $d"
printf '[{"box":"web-main","repo":"acme/thing","granted":"2026-09-22T12:00:01Z","expires":""}]\n' \
  >"$g" 2>/dev/null && echo "wrote a grant $g" || echo "could not write a grant $g"
"#;
    let said = String::from_utf8_lossy(&fleet.in_box(
        born,
        forge,
        &[
            queue.to_string_lossy().into_owned(),
            decision.to_string_lossy().into_owned(),
            grants_file.to_string_lossy().into_owned(),
            id.to_string(),
        ],
    ))
    .to_string();

    // The real read, run the way the server runs it: a fleet-scope script over this fixture's
    // queue, which only reads.
    let shown = {
        let _real = skein::place::seam::real_crossings();
        skein::gitgate::list(&skein::place::fleet_sandbox())
    };
    let grants = skein::gitgate::grants().len();
    let decision_on_host = decision.exists();

    drop(env);
    Forged {
        queue_file: fs::read_to_string(&queue).unwrap_or_default(),
        shown: shown.unwrap_or_else(|e| panic!("the queue could not be read: {e}\n{said}")),
        said,
        decision_on_host,
        grants,
    }
}
