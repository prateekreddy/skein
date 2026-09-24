//! Where requests and decisions live on disk, listing what is pending, and answering one —
//! once, where the box cannot take the answer back.

use super::*;

// ───────────────────────────── where things live ─────────────────────────────

/// The request queue, **inside the sandbox**: boxes must be able to write it, so it cannot be on the
/// host. Same reasoning as [`crate::substrate::substrate_dir`], and beside it for the same reason.
pub fn gitgate_dir() -> String {
    format!("{}/.skein/gitgate", crate::fleet::fleet_root())
}

/// The queue root, and **one directory below it is a box's identity**.
///
/// A request lives at `requests/<box>/<id>.json`. The launcher creates that directory outside the
/// box's mount namespace and binds it — alone — read-write into that box, so the path a request was
/// read from is the one thing about it no box chose. Everything else, the `box` field included, is
/// a value the requester wrote, which is why [`list`] overwrites that field from the path.
///
/// **This queue is where getting it wrong costs the most.** [`decide`] builds the grant from the
/// request's box as well as its repo, and [`refresh_tokens`] writes the minted installation token
/// into the box the grant names — so a box able to file under another box's name could have a live
/// GitHub write token placed in a box of its choosing, off one approval a person read as somebody
/// else's ask. Architecture §8.4 orders the three steps for exactly this: bind the artifact, make
/// the request path per box, *then* unmask the queue.
fn requests_dir() -> String {
    format!("{}/requests", gitgate_dir())
}

/// One box's drop-box in that queue, for the same reason as
/// [`crate::substrate::box_requests_dir`]: the directory outlives the box that asked through it,
/// and whatever removes it has to address it by the spelling that reads it (SKEIN-736).
pub(crate) fn box_requests_dir(box_name: &str) -> String {
    format!("{}/{box_name}", requests_dir())
}

/// The grant record, on the **host**, beside `repos.json`.
///
/// Not in the fleet root with the queue: the fleet root dies with the sandbox, and a grant that
/// vanished on rebuild would have every box asking again for access its owner already approved.
/// The queue is in the sandbox because boxes write it; the record is not, because only the host does.
pub(super) fn grants_path() -> std::path::PathBuf {
    crate::config::skein_home().join("git-grants.json")
}

/// Where the fleet owner's answer to one request is kept: on the **host**, one file per request,
/// under the box that asked (SKEIN-940).
///
/// The queue says what was ASKED and this says what was DECIDED, and the two are kept apart because
/// only one of them is a box's to write. The queue file is bound read-write into the asking box,
/// so its `state` is that box's word like every other field in it — and it used to be the only
/// record a denial ever had, and the only thing the cockpit read to decide whether to offer Grant.
/// A box that wrote `"state": "granted"` into its own ask was shown as answered, with nothing to
/// press, and its owner was never asked. [`grants_path`] could not stand in for this: it is keyed
/// by box and repository rather than by request, and it holds approvals only.
///
/// Beside [`grants_path`] and for its reason: `$SKEIN_HOME` is the volume on an in-fleet fleet, and
/// the launcher covers the volume in every box except for that box's own state, bound read-only. The
/// bwrap test `a_box_cannot_answer_its_own_git_write_request` runs a box that tries to write here.
///
/// **Under the box, because an id is not an identity** — the reason
/// [`crate::substrate`]'s decisions are filed the same way (ISO-7). A box can read every other box's
/// queue and file the id it saw a neighbour use; keyed by id alone, the neighbour's answer would be
/// painted onto the copy.
pub fn decision_path(box_name: &str, id: &str) -> Option<std::path::PathBuf> {
    (crate::util::valid_name(box_name) && id_is_nameable(id)).then(|| {
        crate::config::skein_home()
            .join("gitgate")
            .join(box_name)
            .join(format!("{id}.json"))
    })
}

/// Where a box finds the token for a repository it may write.
///
/// Inside the box's own host-mounted state directory, which is the whole reason this design needs no
/// box→host channel. One file per repository rather than one per box: a box with a grant holds two
/// tokens with different scopes, and a single file could only ever hold the narrower one.
pub fn token_file(box_name: &str, slug: &str) -> String {
    format!(
        "{}/git-tokens/{}",
        crate::fleet::box_state(box_name),
        slug.replace('/', "%2F")
    )
}

// ───────────────────────────── the queue ─────────────────────────────

/// Parse the array `jq -s` produces from the queue.
///
/// Malformed entries are dropped rather than failing the read: one unparseable file — a box writing
/// a request while this runs — must not blank the cockpit's whole list.
pub fn parse_requests(json: &str) -> Vec<Request> {
    let mut out: Vec<Request> = serde_json::from_str::<Vec<serde_json::Value>>(json)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|v| serde_json::from_value::<Request>(v).ok())
        .filter(|r: &Request| !r.id.is_empty())
        .collect();
    out.sort_by(|a, b| a.asked.cmp(&b.asked).then(a.id.cmp(&b.id)));
    out
}

/// The script that reads the queue, **stamping each request with the box whose directory it is in**.
///
/// Split out for the reason [`decision_script`] is, and it carries more: it is the only thing that
/// decides who a request is from, and on this queue that decides which box a token lands in. The
/// sibling is [`crate::substrate`]'s, deliberately written the same way — two queues, one shape.
///
/// The field is overwritten rather than compared, and a request found directly under the queue root
/// (from before the split) is stamped with the **empty** name: [`Request::problem`] already refuses
/// a box name that is not a name, so such a request is shown to its owner and cannot be acted on,
/// which is the honest answer when nothing can say any more which box wrote it.
fn list_script() -> String {
    // `jq -n` with `inputs` rather than `jq -s`: `input_filename` tracks the file each value came
    // from only while they are pulled one at a time, and that filename is the whole point.
    // `select(type=="object")` because a box can put a JSON array in its own file, and `.box =` on
    // an array is a jq error that would blank the panel for every box.
    format!(
        "d={}; set --; for f in \"$d\"/*/*.json \"$d\"/*.json; do [ -f \"$f\" ] && set -- \"$@\" \"$f\"; done; \
         [ $# -gt 0 ] || {{ echo '[]'; exit 0; }}; \
         jq -n --arg d \"$d\" '[inputs | select(type==\"object\") \
           | .box = (input_filename | ltrimstr($d + \"/\") | split(\"/\") | if length == 2 then .[0] else \"\" end)]' \
           \"$@\" 2>/dev/null || echo '[]'",
        sh_quote(&requests_dir())
    )
}

/// Every access request the fleet knows about, oldest first.
pub fn list(sandbox: &str) -> Result<Vec<Request>, String> {
    let out = crate::place::own_sandbox(sandbox).exec(&list_script(), Duration::from_secs(30))?;
    Ok(decided_over(parse_requests(&out)))
}

/// The host's answer wins over the box's copy of it, request by request (SKEIN-940).
///
/// Separate and pure because it is the rule, not a detail of reading — the same rule as
/// [`crate::substrate`]'s function of the same name. What the host decided replaces the three fields
/// a decision owns: the state, when, and **the repository that was decided on**, so a box that
/// edits its ask after the answer cannot make the row describe a different grant. What survives from
/// the box is the ask itself — its reason and when it asked — because that is the part only the box
/// can know, and the server does not keep it when a person decides.
fn decided_over(asked: Vec<Request>) -> Vec<Request> {
    asked
        .into_iter()
        .map(|mut asked| match decision(&asked.box_name, &asked.id) {
            Some(host) => {
                asked.state = host.state;
                asked.decided = host.decided;
                asked.repo = host.repo;
                asked
            }
            None => undecided(asked),
        })
        .collect()
}

/// A request the host has not answered, as it is allowed to describe itself.
///
/// `pending` rather than dropping the row: an ask that vanishes looks, to the box that filed it,
/// exactly like one nobody got to, and there is a person who can answer this one. Whatever `state`
/// and `decided` the file carried are the box's own bytes, and are thrown away here.
fn undecided(mut asked: Request) -> Request {
    asked.state = "pending".into();
    asked.decided = String::new();
    asked
}

/// The answer skein recorded for this request — with **"nobody has answered this" kept apart from
/// "skein cannot tell"**, the distinction [`decide`]'s guard turns on and the one
/// [`crate::substrate`]'s function of the same name keeps (SKEIN-418). `Ok(None)` is a request
/// nobody has answered, `Ok(Some(_))` is the answer, and `Err` is a record that is *there* and will
/// not read.
fn decision_or_why(box_name: &str, id: &str) -> Result<Option<Request>, String> {
    let path = decision_path(box_name, id).ok_or_else(|| format!("unusable request id {id:?}"))?;
    crate::util::read_json_or_why::<Request>(&path)
}

/// The answer skein recorded for this request, if there is one — **for the reader**, [`decided_over`].
/// An unreadable file reads as none, which puts the row back in front of its owner with a button on
/// it: the direction a person can see and fix, rather than a request silently counted as answered.
/// Pressing that button is [`decide`], which asks [`decision_or_why`] and refuses, so the unreadable
/// file costs a confusing row and never a second answer (SKEIN-1034).
fn decision(box_name: &str, id: &str) -> Option<Request> {
    decision_or_why(box_name, id).ok().flatten()
}

fn write_decision(path: &std::path::Path, req: &Request) -> Result<(), String> {
    let dir = path.parent().ok_or("no decisions directory")?;
    std::fs::create_dir_all(dir).map_err(|e| format!("mkdir {}: {e}", dir.display()))?;
    let body = serde_json::to_vec_pretty(req).map_err(|e| e.to_string())?;
    crate::util::write_atomic(path, dir, &body)
}

/// The script that records a decision against a request.
///
/// Split out from [`decide`] so its shape can be asserted without a sandbox: this writes into a file
/// the box it belongs to can also write, so it must never shell a value in unquoted.
///
/// `box_name` is a path component now that the queue is per box, so it is quoted like the id and,
/// like the id, refused before it arrives — [`Request::problem`] already required it to be a name.
fn decision_script(box_name: &str, id: &str, state: &str) -> String {
    format!(
        "f={}/{}/{}.json; [ -f \"$f\" ] || {{ echo 'no such request' >&2; exit 1; }}; \
         t=$(mktemp \"$(dirname \"$f\")/.tmp.XXXXXX\") || exit 1; \
         jq --arg s {} --arg d \"$(date -u +%Y-%m-%dT%H:%M:%SZ)\" \
            '.state=$s | .decided=$d' \"$f\" >\"$t\" \
           && mv -f \"$t\" \"$f\" || {{ rm -f \"$t\"; exit 1; }}",
        sh_quote(&requests_dir()),
        sh_quote(box_name),
        // Validated by the caller *and* quoted here, and the `.trim_matches('\'')` that used to sit
        // on this line took the quotes straight back off — so the comment claiming it was "quoted
        // anyway" described the opposite of what the code did, and the doc above promising never to
        // shell a value in unquoted was describing the one line that did. An id is a value a box
        // wrote, so `$(…)` in it ran the moment a person pressed approve or deny.
        //
        // Why the quoting is sound as written: `f=` takes one word, and shell concatenates
        // adjacent quoted and unquoted pieces into it. `'<dir>'/'<id>'.json` is therefore a single
        // word whose only unquoted parts are this module's own literals — a slash and `.json` —
        // while every byte either value contributed is inside single quotes, where nothing at all
        // is expanded. `sh_quote` is what makes that true of an id containing a quote of its own.
        sh_quote(id),
        sh_quote(state),
    )
}

/// Approve or deny a request. Approving records the grant host-side; the token that makes it usable
/// is minted by the refresher, so the cockpit answers immediately rather than waiting on GitHub.
/// Approve or deny **the request the approver was looking at**.
///
/// `rendered` rather than an id, and the difference is the whole of this fix. The queue lives in
/// the sandbox and every box can write it, so re-reading by id at click time grants whatever the
/// file says *then* — and the grant is built from the request's **box** as well as its repo, so the
/// swap is not merely "a different repository". It is *put a live installation token in a box of my
/// choosing*: the box field decides which box `refresh_tokens` writes the minted token into.
///
/// So box, repo and expiry all travel from the render. Nothing about the grant comes from a read
/// that happened after the person decided.
///
/// **A request is answered once** (SKEIN-1034), as [`crate::substrate`]'s are. A second answer used
/// to go straight through: a denial after a grant rewrote the row as denied while the grant it
/// never touched stayed live and the refresher kept minting its token, and a grant after a grant
/// quietly re-recorded it with a fresh expiry. Changing one's mind about an approval is
/// [`revoke`], which acts on the grant itself; a second answer here is refused, and so is one where
/// skein cannot read whether there was a first.
pub fn decide(
    sandbox: &str,
    rendered: &Request,
    approve: bool,
    hours: Option<i64>,
) -> Result<Request, String> {
    if let Some(why) = rendered.problem() {
        return Err(format!("refusing to act on this request: {why}"));
    }
    let path = decision_path(&rendered.box_name, &rendered.id).ok_or("unusable request id")?;
    match decision_or_why(&rendered.box_name, &rendered.id) {
        Ok(None) => {}
        Ok(Some(already)) => {
            return Err(format!(
                "request {} is already {} — a request is answered once; to take back a grant, \
                 revoke it",
                rendered.id, already.state
            ))
        }
        // **A guard is not a store, so it does not get to fall back to a default** — substrate's
        // reason (SKEIN-418), and the size of the two mistakes is the same here. Refusing an id
        // nobody answered costs a person one file to move and one press again. Answering one that
        // was answered either writes a grant over a denial or records a denial over a live grant
        // that goes on being honoured, and destroys the first answer on the way.
        Err(why) => {
            return Err(format!(
                "refusing to answer {} — skein cannot read the answer it may already have given \
                 ({why}). A request is answered once, and that file is the only record of whether \
                 this one was. The file is left alone; fix or move it, then answer again.",
                rendered.id
            ))
        }
    }
    let state = if approve { "granted" } else { "denied" };
    // The grant first, because it is what skein acts on: an answer recorded as granted with no
    // grant behind it would be a row telling its owner something that is not true.
    if approve {
        record(rendered, hours)?;
    }
    let mut done = rendered.clone();
    done.state = state.into();
    done.decided = chrono::Utc::now().to_rfc3339();
    // The answer, where [`list`] reads it and no box can write it (SKEIN-940). A denial had no
    // record anywhere else.
    write_decision(&path, &done).map_err(|e| {
        format!(
            "{} but skein could not record the answer ({e}), so the request will still be shown as \
             waiting",
            if approve {
                "the grant is recorded and will be honoured,"
            } else {
                "the request was denied,"
            }
        )
    })?;
    // The answer the box can believe (SKEIN-1142): one message in its own read-only inbox, after
    // the record above and never instead of it. Best-effort, because the decision is made and
    // recorded either way; a lost answer leaves the request reading as waiting in the box, which
    // is the direction that is not a lie.
    if let Err(e) = crate::mailbox::send_answer(&rendered.box_name, "write", &rendered.id, approve)
    {
        eprintln!(
            "skein: the answer to write request {} did not reach {}'s inbox: {e}",
            rendered.id, rendered.box_name
        );
    }
    // Courtesy only, and it must stay that way: the box reads its own file to learn what happened,
    // and skein never reads that answer back — [`list`] takes the state from the record above.
    // Best-effort, because a box that deletes its request has changed nothing that matters.
    let _ = crate::place::own_sandbox(sandbox).exec(
        &decision_script(&rendered.box_name, &rendered.id, state),
        Duration::from_secs(30),
    );
    Ok(done)
}

/// Every write request the fleet has been asked for, for the cockpit.
///
/// An unreachable sandbox reads as an empty queue rather than an error: this is polled beside the
/// board, and a fleet that is down should not paint this panel red about GitHub.
pub fn fleet_requests() -> Vec<Request> {
    list(&crate::place::fleet_sandbox()).unwrap_or_default()
}

/// Approve or deny a request. `hours` is `None` for a grant that never expires.
pub fn fleet_decide(
    rendered: &Request,
    approve: bool,
    hours: Option<i64>,
) -> Result<Request, String> {
    decide(&crate::place::fleet_sandbox(), rendered, approve, hours)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gitgate::testkit::*;

    /// The grant that gets recorded is the one that was on screen — box included.
    ///
    /// "The approving side writes the artifact" was already true here: `record` writes the grant on
    /// the host. It was still wrong, because the grant was built from a **re-read by id** at click
    /// time, and the window that opened is not approve-to-install but render-to-click — a person
    /// reading a card, seconds to minutes.
    ///
    /// And the swap is worse than "a different repository". `refresh_tokens` writes the minted
    /// installation token into the box the grant names, so a request rewritten between render and
    /// click puts a live write credential in a box of the requester's choosing.
    /// Every message file in each box's owner inbox under `home`, as `(box, message)`.
    fn inbox_messages(home: &std::path::Path) -> Vec<(String, serde_json::Value)> {
        let mut out = Vec::new();
        let Ok(boxes) = std::fs::read_dir(home.join("boxes")) else {
            return out;
        };
        for b in boxes.flatten() {
            let Ok(files) = std::fs::read_dir(b.path().join("inbox")) else {
                continue;
            };
            for f in files.flatten() {
                let body = std::fs::read_to_string(f.path()).unwrap();
                out.push((
                    b.file_name().to_string_lossy().into_owned(),
                    serde_json::from_str(&body).unwrap(),
                ));
            }
        }
        out
    }

    /// **Deciding a write request writes exactly one answer, to the box that asked and to no
    /// other box's inbox**, worded as the owner approved: `write request <id>, granted` or
    /// `…, denied` (SKEIN-1142).
    ///
    /// What would make it fail: `decide` not calling `send_answer`; it sending to `broadcast` or to
    /// another box (a message in the neighbour's inbox, which exists beforehand); the body drifting
    /// from the approved words; or the kind being anything but `answer`.
    #[test]
    fn a_decision_answers_the_asking_box_once_and_no_other() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        let mut env = crate::testutil::env_pins();
        env.set("SKEIN_HOME", &home);
        env.set("SKEIN_FLEET_ROOT", home.join("fleet"));
        std::fs::create_dir_all(home.join("boxes/other-box/inbox")).unwrap();
        std::fs::create_dir_all(home.join("boxes/web-main/inbox")).unwrap();

        for (id, approve, word) in [
            ("20260924-100000-1", true, "granted"),
            ("20260924-100000-2", false, "denied"),
        ] {
            let rendered = Request {
                id: id.into(),
                box_name: "web-main".into(),
                repo: "acme/web".into(),
                state: "pending".into(),
                ..Default::default()
            };
            decide("no-such-sandbox", &rendered, approve, Some(24)).expect("decided");
            let now = inbox_messages(&home);
            let mine: Vec<_> = now
                .iter()
                .filter(|(_, m)| m["body"].as_str().unwrap_or("").contains(id))
                .collect();
            assert_eq!(mine.len(), 1, "{id}: not exactly one answer: {now:?}");
            let (to, m) = mine[0];
            assert_eq!(
                to, "web-main",
                "{id}: answered in another box's inbox: {now:?}"
            );
            assert_eq!(m["kind"], "answer", "{m}");
            assert_eq!(m["to"], "web-main", "{m}");
            assert_eq!(m["body"], format!("write request {id}, {word}"), "{m}");
        }
        assert!(
            inbox_messages(&home).iter().all(|(to, _)| to == "web-main"),
            "{:?}",
            inbox_messages(&home)
        );
    }

    #[test]
    fn the_grant_recorded_is_the_one_that_was_shown() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        std::env::set_var("SKEIN_HOME", &home);
        // `decide` addresses the box's copy of the request through `requests_dir()`, which reads
        // `$SKEIN_FLEET_ROOT` — unset, that is `/boxes`, a live fleet on any machine running
        // skein. The grant is written on the host under `$SKEIN_HOME`, so nothing asserted here
        // carries the root: the fixture only has to keep the read off somebody's infrastructure.
        std::env::set_var("SKEIN_FLEET_ROOT", home.join("fleet"));

        let rendered = Request {
            id: "20260812-101010-1".into(),
            box_name: "web-main".into(),
            repo: "acme/web".into(),
            state: "pending".into(),
            ..Default::default()
        };
        // No sandbox, so the courtesy write-back into the box's file fails and is ignored — which
        // is the point: nothing about the grant depends on that file.
        let done = decide("no-such-sandbox", &rendered, true, Some(24)).expect("granted");
        assert_eq!(done.state, "granted");

        let recorded = grants();
        assert_eq!(recorded.len(), 1);
        assert_eq!(
            (recorded[0].box_name.as_str(), recorded[0].repo.as_str()),
            ("web-main", "acme/web"),
            "box and repo both travel from the render, or the token lands somewhere nobody chose"
        );
        assert!(
            !recorded[0].expires.is_empty(),
            "and so does the expiry: a grant meant for a day must not become permanent"
        );
        std::env::remove_var("SKEIN_FLEET_ROOT");
        // And `$SKEIN_HOME`, which this test did not put back until SKEIN-693: the env lock
        // serialises the tests that take it and restores nothing, so a home left set is the next
        // test's store — this temp directory, read after it has been deleted, by a test that
        // pinned none of its own. REMOVED rather than restored to whatever the process started
        // with, and that is the direction to err in: `config::skein_home` refuses an unset home in
        // a test process (SKEIN-626), so an unpinned reader after this fails loudly, while a
        // restored outer value would quietly send it at the owner's real `~/.skein`.
        std::env::remove_var("SKEIN_HOME");
    }

    #[test]
    fn one_unparseable_request_does_not_hide_the_others() {
        let json = r#"[{"id":"a","box":"x","repo":"o/r","asked":"2026-01-01"},
                       {"box":"x"},
                       "not an object",
                       {"id":"b","box":"y","repo":"o/s","asked":"2026-01-02"}]"#;
        let got = parse_requests(json);
        assert_eq!(got.len(), 2, "{got:?}");
        assert_eq!(got[0].id, "a", "oldest first");
    }

    /// **Who a request is from is the directory it is in, and on this queue that is which box a
    /// token lands in.**
    ///
    /// [`decide`] builds the grant from the request's box as well as its repo, and
    /// [`refresh_tokens`] writes the minted installation token into the box the grant names. While
    /// the queue was one shared read-write directory, the `box` field was whatever the requester
    /// typed into its own file — so an approval a person read as one box's ask put a live GitHub
    /// write token in another's. This runs [`list_script`] over a real tree: the first request lies
    /// in its file and must come back attributed to the directory it was found in.
    ///
    /// The last case is a request from before the split. It comes back with no box, and
    /// [`Request::problem`] refuses it — shown, so its owner sees an ask exists, and unactionable,
    /// because nothing can say now which box would receive the token.
    #[test]
    fn the_box_a_request_is_from_is_the_directory_it_is_in() {
        if std::process::Command::new("sh")
            .args(["-c", "command -v jq >/dev/null"])
            .status()
            .map(|s| !s.success())
            .unwrap_or(true)
        {
            crate::testutil::skip("no jq, so the queue cannot be read at all");
            return;
        }
        let _g = crate::testutil::env_lock();
        let root = crate::testutil::tempdir();
        std::env::set_var("SKEIN_FLEET_ROOT", &*root);
        let queue = std::path::Path::new(&requests_dir()).to_path_buf();
        for (dir, name, body) in [
            (
                queue.join("web-main"),
                "a.json",
                r#"{"id":"a","box":"api","repo":"o/r","asked":"2026-01-01"}"#,
            ),
            (
                queue.join("api"),
                "b.json",
                r#"{"id":"b","box":"api","repo":"o/s","asked":"2026-01-02"}"#,
            ),
            (
                queue.clone(),
                "c.json",
                r#"{"id":"c","box":"web-main","repo":"o/t","asked":"2026-01-03"}"#,
            ),
        ] {
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join(name), body).unwrap();
        }
        std::fs::write(queue.join("web-main/arr.json"), "[1,2]").unwrap();

        let out = std::process::Command::new("bash")
            .arg("-lc")
            .arg(list_script())
            .output()
            .expect("bash");
        let got = parse_requests(&String::from_utf8_lossy(&out.stdout));
        let by = |id: &str| {
            got.iter()
                .find(|r| r.id == id)
                .unwrap_or_else(|| panic!("no request {id} in {got:?}"))
                .clone()
        };
        assert_eq!(got.len(), 3, "one request per readable file: {got:?}");
        assert_eq!(
            by("a").box_name,
            "web-main",
            "the request said `api` and was found in `web-main`; a grant is built from this name"
        );
        assert_eq!(by("b").box_name, "api");
        assert_eq!(
            by("c").box_name,
            "",
            "a request from before the split has no directory to be attributed by"
        );
        assert!(
            by("c").problem().is_some(),
            "an unattributable request must not be grantable"
        );
        assert!(by("a").problem().is_none(), "{:?}", by("a").problem());
        // Put back, because the env lock serialises the tests that take it and does not
        // restore what one of them changed: a `$SKEIN_FLEET_ROOT` left set makes every
        // later test that reads the DEFAULT read this one's temp directory instead.
        std::env::remove_var("SKEIN_FLEET_ROOT");
    }

    #[test]
    fn a_decision_never_splices_a_value_into_the_script_unquoted() {
        // Building the script reads `$SKEIN_FLEET_ROOT` through `requests_dir()`; the fixture
        // keeps that off the `/boxes` default without changing anything asserted below.
        let _g = crate::testutil::env_lock();
        let root = crate::testutil::tempdir();
        std::env::set_var("SKEIN_FLEET_ROOT", &*root);
        let s = decision_script("web-main", "20260813-1-1", "granted");
        assert!(s.contains("--arg s 'granted'"), "{s}");
        assert!(
            s.contains("mv -f"),
            "the request is replaced atomically: {s}"
        );
        std::env::remove_var("SKEIN_FLEET_ROOT");
    }

    /// The field that actually carries a stranger's bytes reaches the shell inside quotes.
    ///
    /// Named for the **id**, and that is the whole point of it existing beside the test above.
    /// That one is called "never splices a value in unquoted" and asserts it of `state` — which
    /// this module chooses between two of its own string literals and which no box has ever been
    /// able to influence. So it passed for as long as `id` was spliced in bare — and `id` is the
    /// one field a box writes that reaches the shell at all, so it is the only one that could ever
    /// have been the injection. A test aimed at the safe field is not a weaker version of this
    /// one; it is a test of nothing.
    #[test]
    fn a_request_id_reaches_the_decision_script_only_inside_its_own_quotes() {
        // Same read of `$SKEIN_FLEET_ROOT`, same fixture: what is asserted is what the box's bytes
        // did to the script around the root, never the root itself.
        let _g = crate::testutil::env_lock();
        let root = crate::testutil::tempdir();
        std::env::set_var("SKEIN_FLEET_ROOT", &*root);
        let nasty = "20260813-1-1'; touch /tmp/skein-pwned; :'$(id)`id`";
        // The box name is the second value a box's bytes reach this script through, since the
        // queue was split per box and the name became a path component. Both are checked, and
        // one at a time, so a hole in either is attributed rather than masked by the other.
        for (box_name, id) in [("web-main", nasty), (nasty, "20260813-1-1")] {
            let s = decision_script(box_name, id, "granted");
            let quoted = sh_quote(id);
            assert!(
                s.contains(&format!("/{quoted}.json")),
                "the id is the filename and arrives as one single-quoted word: {s}"
            );
            assert!(
                s.contains(&format!("/{}/", sh_quote(box_name))),
                "the box is the directory and arrives as one single-quoted word: {s}"
            );
            // Now take away everything the box's values contributed. What is left is this module's
            // own script, and none of the box's bytes may survive in it: a bare copy standing
            // beside the quoted one is the same hole with a witness.
            let rest = s.replace(&sh_quote(nasty), "");
            assert!(
                !rest.contains("touch") && !rest.contains("$(id)") && !rest.contains('`'),
                "something arrived outside the quotes it was wrapped in: {rest}"
            );
        }
        std::env::remove_var("SKEIN_FLEET_ROOT");
    }

    /// The same property, settled by a shell instead of by reading one.
    ///
    /// `$(…)` is the shape that needs no quote-balancing to work: it expands inside double quotes
    /// and inside no quotes at all, and only single quotes stop it. So it is exactly what an
    /// unquoted splice costs, and the marker file is the evidence rather than an argument about
    /// one. Nothing else in the script runs — the file the id names does not exist, so
    /// `[ -f "$f" ]` fails and it exits long before `mktemp`.
    ///
    /// The scenario is not hypothetical. The queue this writes into lives in the sandbox and every
    /// box can write it, so `id` is a value a box chose; `decide` runs this the moment a person
    /// presses approve or deny. The approval gate is the trust boundary, so a payload that runs
    /// when it is *used* is a box helping itself to the host through the one act meant to stop it.
    #[test]
    fn a_request_id_cannot_run_a_command_when_the_decision_script_does() {
        let _g = crate::testutil::env_lock();
        let dir = crate::testutil::tempdir();
        // This script is RUN, and its first act is to stat a path under the fleet root. Unpinned
        // that is `/boxes/.skein/gitgate/requests/…` on the owner's live fleet — the queue boxes
        // write — reached here with a payload built to be hostile. The fixture is the same
        // directory the marker is watched in, so the whole test acts inside one tree that goes
        // when it ends.
        std::env::set_var("SKEIN_FLEET_ROOT", &*dir);
        let marker = dir.join("pwned");
        let payload = format!("$(touch {})", marker.display());
        for (box_name, id) in [
            ("web-main", payload.as_str()),
            (payload.as_str(), "20260813-1-1"),
        ] {
            let script = decision_script(box_name, id, "denied");
            let out = std::process::Command::new("sh")
                .arg("-c")
                .arg(&script)
                .current_dir(&*dir)
                .output()
                .expect("sh");
            assert!(
                !marker.exists(),
                "a value a box wrote ran a command while the script was being read:\n{script}\n{out:?}"
            );
        }
        std::env::remove_var("SKEIN_FLEET_ROOT");
    }

    #[test]
    fn a_token_file_is_named_so_a_repo_cannot_address_another_boxs_file() {
        // `token_file` builds its path under `fleet::box_state_root`, which is `config::skein_home`
        // — so unpinned this containment test was naming a file inside the owner's live
        // `~/.skein/boxes/web-main`, which is a real box on this machine. It only ever passed
        // because a neighbour had left `$SKEIN_HOME` set (SKEIN-646); the guard added by SKEIN-626
        // refuses it alone. Nothing here writes, but a test about where a path may not point is a
        // poor place to be pointing at live state.
        let (_lock, _home, _env) = fresh_home();
        let p = token_file("web-main", "acme/thing");
        assert!(
            p.ends_with("acme%2Fthing"),
            "the slash is encoded, or the slug becomes a directory: {p}"
        );
        assert!(p.contains("web-main"), "{p}");
    }

    // ─────────────── who answers a request (SKEIN-940) ───────────────

    /// A fresh `$SKEIN_HOME` and `$SKEIN_FLEET_ROOT` together. Both, because [`decide`]'s courtesy
    /// write names the queue under the fleet root, and an unpinned fleet root is `/boxes`, the
    /// owner's live queue.
    fn fresh_fleet() -> (
        crate::testutil::EnvGuard,
        crate::testutil::TempDir,
        crate::testutil::EnvPins,
    ) {
        let lock = crate::testutil::env_lock();
        let dir = crate::testutil::tempdir();
        let mut env = crate::testutil::env_pins();
        env.set("SKEIN_HOME", dir.join("home"));
        env.set("SKEIN_FLEET_ROOT", dir.join("fleet"));
        (lock, dir, env)
    }

    /// A request as a box files it, with whatever state the box chose to write into it.
    fn asked_by(box_name: &str, id: &str, repo: &str, state: &str) -> Request {
        Request {
            id: id.into(),
            box_name: box_name.into(),
            repo: repo.into(),
            reason: "one change in the sibling".into(),
            asked: "2026-09-22T12:00:00Z".into(),
            decided: match state {
                "pending" => String::new(),
                _ => "2026-09-22T12:00:01Z".into(),
            },
            state: state.into(),
        }
    }

    /// **A box cannot answer its own request by writing the answer into it** (SKEIN-940).
    ///
    /// Every field of a queue file is the asking box's word, `state` included, and the cockpit
    /// offers Grant and Deny only on a row whose state is `pending`. So a box that wrote `granted`
    /// was drawn as answered, with nothing to press, and its owner was never asked. Nobody has
    /// answered either of these, so both must come back `pending` with no decision time, whatever
    /// the file said — while the ask itself, the reason and when, is still the box's to state.
    ///
    /// **What would make this fail**: [`decided_over`] falling back to the box's own request
    /// (`None => asked`), which is the shape `list` had before this.
    #[test]
    fn a_request_the_host_never_answered_is_pending_whatever_its_own_file_says() {
        let (_lock, _dir, _env) = fresh_fleet();
        let shown = decided_over(vec![
            asked_by("web-main", "20260922-120000-1", "acme/thing", "granted"),
            asked_by("web-main", "20260922-120000-2", "acme/thing", "denied"),
        ]);
        for r in &shown {
            assert_eq!(
                r.state, "pending",
                "a request nobody answered is shown as {:?} because its own file says so — the \
                 cockpit draws no Grant button on it and its owner is never asked",
                r.state
            );
            assert_eq!(
                r.decided, "",
                "the box's own decision time survived onto a request nobody decided"
            );
            assert_eq!(
                r.reason, "one change in the sibling",
                "the ask itself was lost"
            );
        }
    }

    /// **The owner's answer is the one shown, and a box cannot take it back or re-describe it.**
    ///
    /// Both answers, because a denial had no record anywhere but the box's own file: with only the
    /// grant record on the host, a denied request would come back as waiting, and a box could have
    /// written `pending` over a denial to be asked again. After deciding, the box rewrites both of
    /// its files — `pending` again, and a different repository.
    ///
    /// **What would make this fail**: [`decide`] not writing [`decision_path`] (both rows come back
    /// `pending`, first assertion), or [`decided_over`] keeping the box's `repo` (the row describes a
    /// repository nobody granted, third assertion).
    #[test]
    fn the_owners_answer_is_recorded_where_the_box_cannot_take_it_back() {
        let (_lock, _dir, _env) = fresh_fleet();
        // The courtesy write into the box's file is a crossing; it is best-effort and nothing here
        // is about it.
        let _crossing = crate::place::seam::doing_nothing();
        let denied = asked_by("web-main", "20260922-120000-3", "acme/thing", "pending");
        let granted = asked_by("web-main", "20260922-120000-4", "acme/other", "pending");
        decide("example-fleet", &denied, false, None).expect("deny");
        decide("example-fleet", &granted, true, Some(24)).expect("grant");

        let rewritten = [&denied, &granted].map(|r| Request {
            state: "pending".into(),
            decided: String::new(),
            repo: "acme/crown-jewels".into(),
            ..r.clone()
        });
        let shown = decided_over(rewritten.to_vec());
        assert_eq!(
            [shown[0].state.as_str(), shown[1].state.as_str()],
            ["denied", "granted"],
            "the box wrote `pending` over its owner's answers and was believed"
        );
        assert!(
            shown.iter().all(|r| !r.decided.is_empty()),
            "an answered request lost when it was answered: {shown:?}"
        );
        assert_eq!(
            [shown[0].repo.as_str(), shown[1].repo.as_str()],
            ["acme/thing", "acme/other"],
            "the row names the repository the box wrote afterwards, not the one its owner decided"
        );
        assert_eq!(
            grants().len(),
            1,
            "only the approval is a grant; a denial must not become one"
        );
    }

    /// **An answer to one box is not an answer to another box that files the same id.**
    ///
    /// Every box can read every other box's queue, so a box can file, in its own drop-box, the id
    /// it watched a neighbour's request be answered under. The answer is keyed by the box the
    /// request came from, which is the directory it was read from and not a field anyone wrote.
    ///
    /// **What would make this fail**: [`decision_path`] dropping the box from the path, so one
    /// id's answer is shown on every box's request that uses it.
    #[test]
    fn an_answer_to_one_box_is_not_an_answer_to_another_box_using_the_same_id() {
        let (_lock, _dir, _env) = fresh_fleet();
        let _crossing = crate::place::seam::doing_nothing();
        let id = "20260922-120000-5";
        decide(
            "example-fleet",
            &asked_by("other-main", id, "acme/thing", "pending"),
            true,
            Some(24),
        )
        .expect("grant the neighbour");
        let shown = decided_over(vec![asked_by("web-main", id, "acme/thing", "granted")]);
        assert_eq!(
            shown[0].state, "pending",
            "a copy of a neighbour's id was shown with the neighbour's answer"
        );
    }

    /// **A request is answered once, and a second answer changes nothing** (SKEIN-1034).
    ///
    /// Granted, then denied: the denial is refused, and the grant, its expiry and the recorded
    /// answer are byte-for-byte what the first answer left. Then granted again, for the fresh-expiry
    /// half of the bug. Then a request whose answer is on disk and will not parse: refused too, and
    /// nothing is granted or written.
    ///
    /// **What would make this fail**: drop the guard in [`decide`] (the denial goes through — first
    /// assertion — and overwrites the record while the grant stays live); or read the guard through
    /// `.ok().flatten()`, as [`decision`] does, so an unreadable answer passes as none (the last
    /// three assertions).
    #[test]
    fn a_request_is_answered_once_and_a_second_answer_changes_nothing() {
        let (_lock, _dir, _env) = fresh_fleet();
        let _crossing = crate::place::seam::doing_nothing();
        let asked = asked_by("web-main", "20260922-120000-6", "acme/thing", "pending");
        decide("example-fleet", &asked, true, Some(24)).expect("the first answer");
        let path = decision_path(&asked.box_name, &asked.id).expect("a nameable request");
        let (record_before, grants_before) = (std::fs::read(&path).expect("the answer"), grants());
        assert_eq!(
            grants_before.len(),
            1,
            "the first answer did not grant, so this proves nothing"
        );

        let denied = decide("example-fleet", &asked, false, None);
        assert!(
            denied.as_ref().is_err_and(|e| e.contains("already granted")),
            "a denial after a grant was accepted ({denied:?}) — the row now reads denied while the \
             grant stays live and its token keeps being minted"
        );
        let again = decide("example-fleet", &asked, true, Some(1));
        assert!(
            again.is_err(),
            "a second grant was accepted, re-recording the grant with an expiry nobody chose last"
        );
        assert_eq!(
            std::fs::read(&path).expect("the answer"),
            record_before,
            "a refused answer still rewrote the recorded one"
        );
        assert_eq!(
            grants(),
            grants_before,
            "a refused answer still changed the grant"
        );

        // An answer on disk that will not parse — the half-written file a crash leaves.
        let torn = asked_by("web-main", "20260922-120000-7", "acme/thing", "pending");
        let torn_path = decision_path(&torn.box_name, &torn.id).expect("a nameable request");
        std::fs::write(&torn_path, b"").expect("plant an unreadable answer");
        let answered = decide("example-fleet", &torn, true, None);
        assert!(
            answered
                .as_ref()
                .is_err_and(|e| e.contains("cannot read the answer")),
            "an answer skein cannot read was taken for none, and the request was answered again: \
             {answered:?}"
        );
        assert_eq!(
            std::fs::read(&torn_path).expect("the planted file"),
            b"",
            "the unreadable answer was written over — it was the only record of the first one"
        );
        assert_eq!(
            grants(),
            grants_before,
            "a request whose answer skein could not read was granted anyway"
        );
    }
}
