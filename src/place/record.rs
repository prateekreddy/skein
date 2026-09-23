//! Placement records: writing and forgetting one, the anchor probe and liveness check that
//! prove a record still names a running box, the sweep over every placed box, and `place_of`,
//! which resolves a box name to its place or to nothing.

use super::*;

/// What skein records about a box living in a shared sandbox, written when its session starts.
///
/// A file rather than a lookup, because the namespace's anchor pid is knowable only to whoever
/// launched it, and skein must be able to reach a box after a restart of its own.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PlaceRecord {
    pub sandbox: String,
    /// The box's tmux server — see [`Where::Shared::ns_pid`] for why it is that process and not
    /// the one that launched it.
    pub ns_pid: u32,
    pub home: String,
    pub tree: String,
    #[serde(default)]
    pub sock: String,
    /// Which *boot of the sandbox* [`Self::ns_pid`] belongs to — its `boot_id`.
    ///
    /// A pid is only a name inside one boot. Cycling the sandbox resets the pid space, so every
    /// anchor recorded before it names a different process afterwards, and entering one would put
    /// skein in whatever now happens to hold that number. A *skein* restart does not do this, which
    /// is the distinction that matters: the record has to outlive skein and must not outlive the
    /// sandbox, and only a stamp can tell those two restarts apart.
    ///
    /// Empty in a record written before this existed. Treated as unverifiable, never as a match.
    #[serde(default)]
    pub generation: String,
    /// Field 22 of `/proc/<ns_pid>/stat`, the process start time.
    ///
    /// Pids recycle *within* a boot, so the generation stamp alone is not enough. Together they are
    /// an identity: generation guards the sandbox cycle, start time guards recycling inside one.
    ///
    /// Zero in a record written before this existed. Treated as unverifiable, never as a match.
    #[serde(default)]
    pub ns_start: u64,
    /// Which cover this box was born under — `fleet::launcher_revision` of the `box-session.sh`
    /// that actually built its namespace, as that script reported it on stdout.
    ///
    /// Recorded because it is unreadable afterwards. `install_launcher` refreshes the script in the
    /// sandbox at every start and every heal, so the copy on disk says what the NEXT box will get
    /// and says nothing about the ones already running — and a box keeps its namespace for as long
    /// as it lives. Found the hard way: a box reporting itself ordinary, with `/boxes/` fully
    /// listed and a live `$SSH_AUTH_SOCK`, because it started the day before the cover did.
    ///
    /// Empty where no launcher answered — a record written before this field, a launcher older than
    /// the line that prints it, or the adoption path, where no launcher runs. All three mean the
    /// same thing and it is not "current": the honest reading is *unknown*, and unknown is reported
    /// as an older cover, because the one direction that must never be guessed is this one.
    #[serde(default)]
    pub launcher: String,
    /// What the launcher said about this box's memory ceiling — `capped <limits>`, or `uncapped`
    /// with the reason it could not be.
    ///
    /// Recorded for the same reason [`Self::launcher`] is, and it is the same shape of hole: the
    /// launcher writes `limits.state` into the box's own root, which is *inside the sandbox*, so
    /// nothing on the host has ever been able to read it. A box with no ceiling therefore looked
    /// exactly like a box with one, on every surface skein has — while being the box that can take
    /// the whole fleet down with a runaway build, which is what the launcher's own comment says the
    /// ceiling is there to stop.
    ///
    /// Empty where no launcher answered. Read as *unknown*, never as *capped*.
    #[serde(default)]
    pub ceiling: String,
    /// Whether this box was BORN on the fleet's peer network — [`crate::repos::Repo::peer_messaging`]
    /// for its repo, as it stood when the launcher ran.
    ///
    /// **`Option` is load-bearing.** `None` is "a launcher too old to say", and it must not read as
    /// either position: `Some(false)` would put a box on the uncovered side of a switch nobody
    /// flipped, and `Some(true)` would claim a network the box may not be on. `is_none_or` in
    /// [`crate::fleet::cover_is_current`] is where that third answer is spent — an unanswerable
    /// record is left alone rather than asked to restart.
    ///
    /// Recorded rather than re-read, and that is the whole item. The switch lives in `repos.json`
    /// and flipping it changes **not one byte of `box-session.sh`** — so `fleet::launcher_revision`,
    /// which hashes that file, stays identical and the cover keeps answering *current* over a box
    /// still running the mount it started with. A flag whose effect is a mount has to travel WITH
    /// the box, or it is a switch that silently does nothing until somebody happens to restart.
    #[serde(default)]
    pub peers: Option<bool>,
    /// Why this box exists — see [`Purpose`].
    ///
    /// Recorded here rather than derived, because nothing else on the host can answer it: a box
    /// skein opened to review a pull request has the same checkout, the same store and the same
    /// session as one a person made, and the intention behind it survives only if it is written
    /// down at the moment it is acted on.
    ///
    /// Absent ⇒ [`Purpose::Manual`], which is a fact about the past rather than a guess: every
    /// record written before this field was written by a skein that made boxes only when asked.
    ///
    /// **No constructor, and the derived `Default` carries the fixtures.** Adding this field found
    /// nineteen struct literals that build a record, eighteen of them test fixtures with no opinion
    /// about any of it; a constructor taking every field would have been the same nineteen edits
    /// under another name, and one taking only the purpose would leave the other nine fields
    /// positional. So the fixtures now end at `..Default::default()` and the next field added here
    /// costs them nothing — while the ONE site that actually decides, `fleet::start_box_inner`,
    /// names the variant out loud precisely so a new purpose cannot arrive there by default.
    #[serde(default, deserialize_with = "purpose_or_manual")]
    pub purpose: Purpose,
}

/// The shell that reports what `pid` actually is right now: `<boot-id> <starttime>`.
///
/// Run in the SANDBOX, never inside a box: `/proc` is the sandbox's, and a box holds
/// `CAP_SYS_ADMIN` in its own user namespace, so it can mount over its view of `/proc` and answer
/// this question with whatever it likes.
///
/// The start time is cut after the last `) ` rather than taken as whitespace field 22, because the
/// `comm` field is the process's own name in parentheses and may contain both spaces and
/// parentheses — `awk '{print $22}'` is right until a program is called something awkward, and then
/// it is silently off by however many spaces are in the name.
pub(crate) fn anchor_probe(pid: u32) -> String {
    format!(
        "printf '%s %s\n' \
         \"$(cat /proc/sys/kernel/random/boot_id 2>/dev/null)\" \
         \"$(sed -n 's/.*) //p' /proc/{pid}/stat 2>/dev/null | cut -d' ' -f20)\""
    )
}

/// Which boxes are running, decided in this process with nothing forked.
///
/// It was the local half of a pair: `fleet::fleet_liveness` chose between this and a generated
/// shell that `sbx exec`'d the same decision into the sandbox. There is one deployment now
/// (SKEIN-576), which left the shell half unreachable behind an unconditional `return` rather than
/// deleted — it is gone (SKEIN-615), and this is the whole sweep.
///
/// The fidelity to the stamp matters more than it looks: a sweep that decided liveness differently
/// from the way [`anchor_probe`] wrote the stamp would make boxes flap between running and stopped
/// with nothing about them changing.
///
///   * **the anchor**, for a box whose record can decide it — `(boot_id, starttime)` against the
///     record. `starttime` is field 22 of `/proc/<pid>/stat`, taken after the last `)` because a
///     process's comm can itself contain spaces and parens; that is what [`anchor_probe`]'s
///     `sed`/`cut` does, and [`parse_proc_starttime`] parses it the same way.
///   * **the socket**, for every box the anchors could not decide — no record, no stamp, or a
///     record from an earlier boot. `tmux -S <sock> has-session` asked whether *a* tmux server is
///     listening there, and connecting to the socket asks precisely that: a server accepts, a stale
///     socket file refuses. §2.3's `socket`, and the reason `FleetLiveness` names two Sources.
///
/// The fallback's direction is not symmetric and is kept that way: calling a live box stopped
/// invites somebody to start a second one over its work, so an undecidable box is asked rather
/// than assumed dead.
pub(crate) fn local_liveness(
    root: &str,
    anchors: &[(String, u32, String, u64)],
) -> std::collections::HashMap<String, bool> {
    let mut out = std::collections::HashMap::new();
    let boot = fs::read_to_string("/proc/sys/kernel/random/boot_id")
        .map(|b| b.trim().to_string())
        .unwrap_or_default();
    for (name, pid, generation, start) in anchors {
        // A record from a different boot, or one with no stamp, decides nothing — left to the
        // socket loop rather than reporting a pid that names some other process now.
        if generation.is_empty() || *start == 0 || *generation != boot {
            continue;
        }
        let seen = fs::read_to_string(format!("/proc/{pid}/stat"))
            .ok()
            .and_then(|stat| parse_proc_starttime(&stat));
        out.insert(name.clone(), seen == Some(*start));
    }
    // Every box the anchors could not decide, by the old question. The directory listing is also
    // what finds a box with no placement record at all.
    let Ok(entries) = fs::read_dir(root) else {
        return out;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let Some(name) = path.file_name().map(|n| n.to_string_lossy().into_owned()) else {
            continue;
        };
        if out.contains_key(&name) {
            continue;
        }
        let sock = path.join("session.sock");
        // A connect and an immediate drop. Nothing is sent, so this cannot disturb the server it
        // is asking about — and a socket file with no server behind it refuses rather than hanging.
        let live = std::os::unix::net::UnixStream::connect(&sock).is_ok();
        out.insert(name, live);
    }
    out
}

/// `starttime` — field 22 of `/proc/<pid>/stat`, counted from after the final `)`.
///
/// Split on the last `)` rather than the first, and on `)` rather than on whitespace, because a
/// process's `comm` is arbitrary bytes in parens: a box named `foo bar)baz` would break every
/// simpler parse, and the failure would be a box reported dead while it ran.
pub(crate) fn parse_proc_starttime(stat: &str) -> Option<u64> {
    let after = stat.rsplit_once(')')?.1;
    // Field 22 overall is field 20 of what follows the comm — the probe's `cut -d' ' -f20`.
    after.split_whitespace().nth(19)?.parse().ok()
}

/// Read what [`anchor_probe`] printed: `(generation, start)`, or `None` if either is missing.
pub(crate) fn parse_anchor_probe(out: &str) -> Option<(String, u64)> {
    let line = out.lines().rev().find(|l| !l.trim().is_empty())?;
    let (generation, start) = line.trim().split_once(' ')?;
    let start: u64 = start.trim().parse().ok()?;
    (!generation.is_empty() && start > 0).then(|| (generation.to_string(), start))
}

fn place_record_path(name: &str) -> PathBuf {
    skein_home().join("places").join(format!("{name}.json"))
}

/// Record where a box was started, so later calls can reach it.
pub fn record_place(name: &str, record: &PlaceRecord) -> Result<(), String> {
    if !valid_name(name) {
        return Err("invalid box name".into());
    }
    let dir = skein_home().join("places");
    fs::create_dir_all(&dir).map_err(|e| format!("mkdir {}: {e}", dir.display()))?;
    let bytes = serde_json::to_vec_pretty(record).map_err(|e| e.to_string())?;
    write_atomic(&place_record_path(name), &dir, &bytes)
}

/// Forget a box's placement — its namespace died with it.
pub fn forget_place(name: &str) {
    if valid_name(name) {
        let _ = fs::remove_file(place_record_path(name));
    }
}

/// The placement skein recorded for a box, alive or not.
///
/// The same source [`place_of`] uses, exposed for the callers that need the *record* rather than an
/// address: a box with a record is a shared box whether or not it is currently running, and asking
/// sbx about a sandbox named after it would report on something that was never there.
pub fn shared_record(name: &str) -> Option<PlaceRecord> {
    if !valid_name(name) {
        return None;
    }
    read_place_record(name)
}

/// Every box skein has placed in `sandbox`, running or not.
///
/// Read off the placement records rather than by asking the sandbox what is inside it: a resize has
/// to account for boxes that are *stopped* too — their checkouts are still VM-local and still hold
/// unpushed work, and a sandbox that is about to be destroyed cannot be asked about them.
/// Sorted, so a resize processes them in the same order every time and its log can be followed.
/// Every sandbox skein has placed a box into.
///
/// The question [`placed_boxes`] answers upside down, and it exists for a different one: what makes
/// another skein's fleet *recognisable as one* rather than merely present in `sbx ls`. A sandbox
/// nobody has ever placed a box in is somebody else's container; one that has been placed in is a
/// fleet, whether or not it is this skein's.
pub fn placed_sandboxes() -> std::collections::HashSet<String> {
    let dir = skein_home().join("places");
    fs::read_dir(&dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            let name = name.strip_suffix(".json")?.to_string();
            read_place_record(&name).map(|record| record.sandbox)
        })
        .filter(|sandbox| !sandbox.is_empty())
        .collect()
}

pub fn placed_boxes(sandbox: &str) -> Vec<(String, PlaceRecord)> {
    let dir = skein_home().join("places");
    let mut found: Vec<(String, PlaceRecord)> = fs::read_dir(&dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            let name = name.strip_suffix(".json")?.to_string();
            let record = read_place_record(&name)?;
            (record.sandbox == sandbox).then_some((name, record))
        })
        .collect();
    found.sort_by(|a, b| a.0.cmp(&b.0));
    found
}

fn read_place_record(name: &str) -> Option<PlaceRecord> {
    serde_json::from_str(&fs::read_to_string(place_record_path(name)).ok()?).ok()
}

/// Resolve a box to where it runs.
///
/// `None` for a name that isn't one — every path into a box is gated here, so no caller has to
/// remember to validate before building an argv.
///
/// A box that skein started in a shared sandbox says so in its own record; everything else is the
/// original one-sandbox-per-box mapping. That ordering is deliberate: turning the fleet sandbox on
/// must not retroactively claim boxes that are still running as their own VM, or skein would exec
/// into a namespace that was never created.
pub fn place_of(name: &str) -> Option<Place> {
    if !valid_name(name) {
        return None;
    }
    if let Some(rec) = read_place_record(name) {
        // The record is authoritative, and deliberately not gated on the anchor being alive.
        //
        // This used to check `/proc/<ns_pid>` — on the HOST, where that pid means nothing: the
        // anchor lives inside the fleet sandbox's own pid namespace, and on macOS there is no
        // `/proc` at all. So the check failed for every box, always, and the fallback below then
        // addressed a fleet box as a sandbox named after itself — `sbx exec skein-fleetsmoke` for a
        // sandbox that does not exist and never will.
        //
        // A dead anchor is a real condition, but it is liveness, not address: `box_liveness` asks
        // the box's tmux socket, and an exec against a dead namespace fails loudly on its own. What
        // must never happen is a *placed* box being reached as though it were unplaced.
        return Some(Place {
            name: name.to_string(),
            sandbox: rec.sandbox,
            at: Where::Shared {
                ns_pid: rec.ns_pid,
                home: rec.home,
                tree: rec.tree,
                sock: rec.sock,
                generation: rec.generation,
                ns_start: rec.ns_start,
            },
        });
    }
    // No record ⇒ not a box skein placed, and there is nothing truthful to return.
    //
    // This used to fall back to a sandbox named after the box — skein's original per-VM model, where
    // a box *was* a sandbox. That model is gone, and the fallback outlived it as a silent guess: any
    // name at all resolved to a `Place`, so a plain `sbx` sandbox nobody made with skein, or a box
    // whose start failed, was addressed as though skein owned it. The failure then arrived from sbx
    // (`no sandbox named …`) rather than from the code that knew the answer.
    //
    // `None` is the honest answer and it is the useful one: callers now have to say what they mean by
    // an unplaced box, and every one of them wanted to report rather than guess.
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::*;

    // Gating resolution means no caller can build an argv from a name that was never checked.
    // What `valid_name` guarantees is that a name is not a *path* — it may contain spaces and
    // shell metacharacters, which are inert here because the name is an argv element, never
    // interpolated into a shell string. The paths that DO build shell strings quote it.
    #[test]
    fn a_name_that_could_be_a_path_has_no_place() {
        let _g = env_lock();
        let dir = tempdir();
        std::env::set_var("SKEIN_HOME", &dir);
        for bad in ["", "../etc", "a/b", "a\\b", "x\0y", &"n".repeat(129)] {
            assert!(place_of(bad).is_none(), "resolved {bad:?}");
        }
        // A dot is not a traversal, so such a name is placeable — checked through a real record now
        // that an unrecorded name resolves to nothing at all.
        //
        // This used to be `"a b"`, on the grounds that "a space is not a traversal". True of a path
        // and false of everything else a name reaches: the fleet sweep's generated shell accumulated
        // answered names into a SPACE-SEPARATED string and matched `*" $n "*` against it, so a box
        // with a space in its name could never have been matched by it. That shell is deleted
        // (SKEIN-615), and `valid_name`'s allow-list refuses such a name anyway — which is what
        // actually closed the injection.
        record_place(
            "a.b",
            &PlaceRecord {
                sandbox: "skein-fleet".into(),
                ns_pid: std::process::id(),
                home: "/boxes/a.b/home".into(),
                tree: "/boxes/a.b/tree".into(),
                sock: "/boxes/a.b/session.sock".into(),
                generation: "test-boot".into(),
                ns_start: 1,
                ..Default::default()
            },
        )
        .unwrap();
        let spaced = place_of("a.b").expect("a dot is not a traversal");
        // The argv no longer names a sandbox anywhere: there is no hop to address one with
        // (SKEIN-576), so what used to be asserted at index 2 has no position to be at. What the
        // record still decides is below — the box, and the paths that come from it.
        assert!(
            spaced.exec_argv("true").iter().any(|a| a.contains("a.b")),
            "the name stays one argv element, so nothing can split it"
        );
        std::env::remove_var("SKEIN_HOME");
    }

    // A recorded placement is only good while its namespace is alive. A stale record pointing at a
    // recycled pid would send a box's commands into whatever process now holds that number.
    #[test]
    fn a_placed_box_is_addressed_by_its_record_never_by_its_own_name() {
        let _g = env_lock();
        let dir = tempdir();
        std::env::set_var("SKEIN_HOME", &dir);

        // No record ⇒ nothing to address. It used to mean "a sandbox named after the box", skein's
        // per-VM model; with that gone, guessing would hand any name at all a Place — including a
        // sandbox skein never made.
        assert!(
            place_of("web-main").is_none(),
            "an unrecorded name must not resolve to a sandbox"
        );

        // A live pid: this test process itself, which is certainly running.
        record_place(
            "web-main",
            &PlaceRecord {
                sandbox: "skein-fleet".into(),
                ns_pid: std::process::id(),
                home: "/boxes/web-main/home".into(),
                tree: "/boxes/web-main/tree".into(),
                sock: "/boxes/web-main/session.sock".into(),
                generation: "test-boot".into(),
                ns_start: 1,
                ..Default::default()
            },
        )
        .unwrap();
        let p = place_of("web-main").unwrap();
        assert_eq!(p.sandbox, "skein-fleet");
        assert!(matches!(p.at, Where::Shared { .. }));

        // A pid that cannot be running: pid 0 is never a process. It must STILL resolve to the
        // fleet. This assertion used to be the opposite, and that was the bug: the pid names a
        // process in the sandbox's namespace, so checking it against the host's `/proc` asks the
        // wrong kernel — and on macOS asks nothing at all, since there is no `/proc`. Every fleet
        // box therefore fell through to `SandboxItself` and was addressed as a sandbox named after
        // itself, which is both wrong and, if a same-named sandbox exists, dangerous.
        record_place(
            "web-main",
            &PlaceRecord {
                sandbox: "skein-fleet".into(),
                ns_pid: 0,
                home: "/h".into(),
                tree: "/t".into(),
                sock: "/s".into(),
                generation: "test-boot".into(),
                ns_start: 1,
                ..Default::default()
            },
        )
        .unwrap();
        let p = place_of("web-main").unwrap();
        assert_eq!(p.sandbox, "skein-fleet", "a placed box stays placed");
        assert!(
            matches!(p.at, Where::Shared { ns_pid: 0, .. }),
            "liveness is the tmux socket's answer, not a pid lookup in the wrong namespace"
        );

        // Forgetting a placement is how a destroyed box stops being addressable at all — not how it
        // reverts to being its own sandbox, which is what this asserted while that model existed.
        forget_place("web-main");
        assert!(place_of("web-main").is_none());
        std::env::remove_var("SKEIN_HOME");
    }

    /// A placement record written before boxes had a purpose still reaches its box.
    ///
    /// This is not a serde formality. There are thirteen of these on a live fleet right now,
    /// each one the only thing that knows which namespace a running box lives in — `read_place_record`
    /// swallows a parse error into `None`, so a field that failed to deserialise would not raise
    /// anything: every one of those boxes would simply stop being reachable, and the board would show
    /// them as sandboxes skein never placed. Dropping `#[serde(default)]` from `purpose` is what this
    /// catches; the unknown-word case below catches dropping `deserialize_with`.
    #[test]
    fn a_placement_record_that_predates_purpose_still_reaches_its_box() {
        let _g = env_lock();
        let dir = tempdir();
        std::env::set_var("SKEIN_HOME", &dir);
        std::fs::create_dir_all(dir.join("places")).unwrap();

        // Byte for byte the shape skein wrote before this field existed — nine fields, no purpose.
        let old = format!(
            r#"{{"sandbox":"skein-fleet","ns_pid":{},"home":"/boxes/web-main/home",
                 "tree":"/boxes/web-main/tree","sock":"/boxes/web-main/session.sock",
                 "generation":"test-boot","ns_start":1,"launcher":"","ceiling":""}}"#,
            std::process::id()
        );
        std::fs::write(dir.join("places").join("web-main.json"), &old).unwrap();

        let rec = shared_record("web-main").expect("a record skein wrote yesterday still parses");
        assert_eq!(
            rec.sandbox, "skein-fleet",
            "the rest of the record survived too"
        );
        assert_eq!(
            rec.purpose,
            Purpose::Manual,
            "a record that says nothing about purpose was written by a skein that only made boxes \
             when a person asked — reading it any other way invents an intention"
        );
        assert!(!rec.purpose.managed());
        assert!(
            place_of("web-main").is_some(),
            "the box became unreachable, which is what an unparseable placement record costs"
        );

        // A purpose from a LATER skein, read by this one — a downgrade, or a fleet mid-upgrade.
        // Unknown must read as manual, never as a record that will not parse, for exactly the same
        // reason: the cost of guessing wrong is a mislabelled row, the cost of failing is a lost box.
        let ahead = r#"{"sandbox":"skein-fleet","ns_pid":1,"home":"/h","tree":"/t","sock":"/s",
             "generation":"g","ns_start":1,"launcher":"","ceiling":"","purpose":"audit"}"#;
        std::fs::write(dir.join("places").join("later-box.json"), ahead).unwrap();
        let read =
            shared_record("later-box").expect("an unknown purpose is not a reason to lose the box");
        assert_eq!(read.purpose, Purpose::Manual);

        // And the shape that goes onto disk, which is the half a `default` cannot check: a purpose
        // that serialised as `"Review"` or as `1` would be a record the NEXT skein cannot read back.
        record_place(
            "review-box",
            &PlaceRecord {
                sandbox: "skein-fleet".into(),
                ns_pid: 1,
                purpose: Purpose::Review,
                ..Default::default()
            },
        )
        .unwrap();
        let json = std::fs::read_to_string(dir.join("places").join("review-box.json")).unwrap();
        assert!(
            json.contains(r#""purpose": "review""#),
            "the variant is the wire format and this is what a later skein reads back: {json}"
        );
        assert_eq!(
            shared_record("review-box").unwrap().purpose,
            Purpose::Review
        );
        assert!(
            shared_record("review-box").unwrap().purpose.managed(),
            "a box skein opened to review a pull request is one skein manages"
        );

        std::env::remove_var("SKEIN_HOME");
    }

    /// The sweep, run against this machine's real `/proc`.
    ///
    /// Four boxes, one per answer the sweep has to give, and the process it verifies is this test:
    /// a live anchor whose start time matches, one whose pid cannot exist, one whose pid is live but
    /// whose start time is somebody else's, and one whose record predates the stamp and therefore
    /// falls through to the socket.
    ///
    /// **The start time is read here by hand, not by [`parse_proc_starttime`].** Calling the parser
    /// to build the value the parser is then checked against is an assertion that cannot fail: both
    /// sides would move together. Cutting field 22 out independently is what makes an off-by-one in
    /// it turn `live-one` red, and agreeing with the stamp is the property — a sweep that read
    /// `/proc` differently from [`anchor_probe`] would flap boxes between running and stopped with
    /// nothing about them changing.
    ///
    /// This asserted the same four answers of the generated-shell twin `fleet::fleet_liveness`
    /// used to be able to choose between. SKEIN-521 left that twin unreachable behind an
    /// unconditional `return` and SKEIN-615 deleted it, so the property moved here — onto the half
    /// that actually runs, which had no direct test of its own until now.
    ///
    /// Linux only: the sweep proves an anchor against `/proc/<pid>`, which is the whole subject.
    #[cfg(target_os = "linux")]
    #[test]
    fn the_sweep_verifies_the_anchor_and_falls_back_only_when_it_cannot() {
        let stat = std::fs::read_to_string("/proc/self/stat").expect("a linux /proc");
        let after = stat.rsplit_once(") ").expect("a stat line").1.to_string();
        let start: u64 = after
            .split_whitespace()
            .nth(19)
            .and_then(|f| f.parse().ok())
            .expect("field 22 of /proc/self/stat");
        let boot = std::fs::read_to_string("/proc/sys/kernel/random/boot_id")
            .expect("a boot id")
            .trim()
            .to_string();
        let me = std::process::id();

        let root = crate::testutil::tempdir();
        for name in ["live-one", "dead-one", "reused-one", "old-one"] {
            fs::create_dir_all(root.join(name)).unwrap();
        }
        let seen = local_liveness(
            root.to_string_lossy().as_ref(),
            &[
                ("live-one".into(), me, boot.clone(), start),
                // A pid above the kernel's maximum cannot name anything.
                ("dead-one".into(), 0x7fff_fffe, boot.clone(), start),
                // Live pid, somebody else's start time: this is pid reuse, and it must read as gone
                // rather than as the box that used to be there.
                ("reused-one".into(), me, boot.clone(), start + 1),
                // No stamp: undecidable, so the socket answers — and there is no socket here.
                ("old-one".into(), me, String::new(), 0),
            ],
        );
        let verdict = |name: &str| -> bool {
            *seen
                .get(name)
                .unwrap_or_else(|| panic!("nothing said about {name}: {seen:?}"))
        };
        assert!(verdict("live-one"), "{seen:?}");
        assert!(!verdict("dead-one"), "{seen:?}");
        assert!(
            !verdict("reused-one"),
            "a recycled pid was reported as the box that used to hold it: {seen:?}"
        );
        assert!(
            !verdict("old-one"),
            "an unstamped record must fall through to the socket, not be decided by the pid: \
             {seen:?}"
        );

        // And a record from an earlier boot decides nothing either — same fallback, different
        // reason: the pid space was reset, so that number now names some other process.
        let cycled = local_liveness(
            root.to_string_lossy().as_ref(),
            &[("live-one".into(), me, "not-this-boot".into(), start)],
        );
        assert_eq!(
            cycled.get("live-one"),
            Some(&false),
            "an anchor from another boot was believed: {cycled:?}"
        );
    }
}
