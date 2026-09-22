//! Where a box's work actually happens, and the one way to reach it.
//!
//! A box is an identity: a name, a branch, a repo, a conversation. *Where it runs* is a separate
//! thing. skein's original model fused them — one sbx sandbox per box, named after the box — so
//! "the box" and "the sandbox" were the same string in six different helpers, and every feature
//! that touched a box hardcoded that assumption.
//!
//! [`Place`] separates them. `place_of(box)` is a lookup, not an identity, and every call into a
//! box goes through [`Place::exec`] / [`Place::write`] / [`Place::bytes`]. That is the whole point:
//! changing what backs a box — several boxes sharing one sandbox, each with its own HOME, tree and
//! cgroup — became a change to `place_of` rather than a sweep through every feature.
//!
//! There are two shapes, and the address says which one it is rather than skein guessing:
//!
//! - [`Where::Shared`] — a box inside the fleet's sandbox, in its own bwrap namespace. **This is
//!   the only shape a box has**: `place_of` resolves a box name to this or to nothing at all.
//!   Memory is a pool the boxes share instead of N reservations that sum, and `/tmp` and `$HOME`
//!   have to be made private deliberately, because a shared VM does not hand them over.
//! - [`Where::SandboxItself`] — a whole sandbox, addressed as itself: no box inside it to enter,
//!   because the address IS the sandbox. In practice that is the fleet's own sandbox, which is how
//!   [`crate::fleet`] provisions the thing the boxes then live in. It is also the shape skein's
//!   original per-box microVMs had, and the reason [`Place::unreachable_from_fleet`] exists: an
//!   address of this shape naming a sandbox *other than* the one this process stands in is one
//!   that in-fleet skein has no way to reach.

use crate::config::skein_home;
use crate::config::*;
use crate::util::valid_name;
use crate::util::*;
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::{Read, Write};
use std::os::unix::process::CommandExt as _;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Duration;

/// How a box's sandbox is reached.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Where {
    /// A whole sandbox, addressed as itself. Nothing to enter — there is no box inside this
    /// address, so [`Place::enter`] contributes no hop and [`Place::tmux`] is the sandbox's own
    /// bare `tmux`.
    ///
    /// Every production caller names the **fleet's own sandbox**: `ensure_substrate`,
    /// `ensure_fleet_root`, `install_launcher` and `install_docker_config` all address the sandbox
    /// the boxes live in, through [`own_sandbox`]. skein's original per-box microVMs had this shape
    /// too — a box that *was* a sandbox named after it — and nothing resolves to that any more
    /// (`place_of` returns `None` for such a name), which is why the one thing this variant still
    /// has to decide is [`Place::unreachable_from_fleet`]: whether the sandbox named is the one
    /// this process is standing in.
    SandboxItself,
    /// The sandbox hosts several boxes. This one lives in a bwrap namespace anchored by `ns_pid`,
    /// with its own `/tmp` and `$HOME` bound in there.
    ///
    /// `ns_pid` is the box's **tmux server**, not the process that launched it. The launcher starts
    /// the session and exits — tmux double-forks away from it — so its pid names a corpse while the
    /// box runs happily. The server is the honest anchor: it is in the namespace, and it lives
    /// exactly as long as the box. Box alive ⇔ server alive ⇔ namespace joinable.
    ///
    /// Reaching in means joining that namespace. Both the user and mount namespaces have to be
    /// joined together — joining the mount namespace alone is refused — and credentials must be
    /// preserved, or `setgroups` fails for an unprivileged caller. Verified inside a real box;
    /// getting either detail wrong looks like a permissions bug rather than a missing flag.
    Shared {
        ns_pid: u32,
        /// The HOME a script runs with — the sandbox's own path, not a private directory.
        ///
        /// Explicit rather than inherited, because `nsenter` carries the caller's environment in and
        /// a script that reads `~` must read the box's view of it. The privacy is in the *mounts*:
        /// `box-session.sh` binds the few paths that must differ per box (`~/.claude.json`,
        /// `~/.claude`, `~/.codex`, `~/.config/sync`) and leaves the rest shared. Replacing HOME
        /// outright was the earlier design and it could not work — `claude` lives under `~/.local/bin`
        /// and its credentials under `~/.claude`, so the box had no agent to start.
        home: String,
        /// The box's checkout. Every script skein sends assumes it starts at the repo root.
        tree: String,
        /// The box's tmux socket, deliberately *outside* the private mounts so it is the same path
        /// inside and out. That is what lets skein list, attach to and kill a box's session from
        /// the sandbox without entering its namespace first — and `ns_pid` is that very server.
        sock: String,
        /// The proof that `ns_pid` is still the process skein recorded — see
        /// [`PlaceRecord::generation`] and [`PlaceRecord::ns_start`].
        ///
        /// Carried in the *address* rather than checked when the address is built, because a check
        /// that ran here would be a check with a gap after it: pids die, and a box that exited
        /// between the check and the `nsenter` would be entered as whatever took its number. The
        /// only place the answer cannot go stale is the same process that crosses, immediately
        /// before it crosses — so this rides along and [`Place::guard`] spends it there.
        generation: String,
        ns_start: u64,
    },
}

/// Where one box runs.
///
/// `sandbox` is the sbx name to exec into; `name` is the box. Under [`Where::SandboxItself`] there
/// is no box — the two are the same string — and this type exists precisely so that they need not
/// stay equal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Place {
    pub name: String,
    pub sandbox: String,
    pub at: Where,
}

/// Why a box exists: a person asked for it, or skein started it to do a job of its own.
///
/// **An enum and not a `bool`.** One bit ("managed or not") is all the board needs today, and a
/// `bool` would carry it. The bit is not the question, though — about a box skein started, the very
/// next question is always *managed for what*, and that answer is what decides how it is announced,
/// what skein may do to it unasked, and when it is finished (a box opened to review a pull request
/// is done when the verdict is posted; a box a person made is never done). A `bool` widened later
/// means a wire break on every surface that reads it, and there are three; a variant added here is
/// a variant. Callers that only want the bit ask [`Self::managed`].
///
/// The variant name is what lands on disk (`"purpose": "review"`), so an unrecognised one is a
/// record a *newer* skein wrote — which is why reading it is lenient rather than an error. A
/// placement record that will not parse is a box skein can no longer reach, and downgrading skein
/// must not strand the boxes it left running.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Purpose {
    /// A person made this box and drives it. Every box on every fleet today, and the reading for
    /// every record written before this field existed — the only one that can be right, since a
    /// skein that wrote no purpose was a skein that only made boxes for people.
    #[default]
    Manual,
    /// skein opened this box itself to read a pull request and post a verdict on it.
    ///
    /// Still a box, as the decision was put: *"you aren't creating a new class of sessions but just
    /// box but managed automatically."* Same placement record, same sandbox, same tmux contract —
    /// the only difference is who asked for it, which is exactly what this field records.
    Review,
}

impl Purpose {
    /// Is this a box skein started and drives, rather than one a person made?
    ///
    /// Written as "not Manual" so a variant added later is managed by default. The mistake to make
    /// here would be listing the managed variants: a new purpose forgotten in that list is a box
    /// skein drives that the board files among the ones a person is responsible for.
    pub fn managed(self) -> bool {
        !matches!(self, Purpose::Manual)
    }

    /// The word for this purpose in a sentence a person reads.
    ///
    /// Separate from the serde name on purpose, though they agree today: what lands on disk is a
    /// wire format and must not drift, while what a refusal says is prose and may be improved. The
    /// caller that needs it is `fleet::start_box_inner`'s collision guard, which has to name both
    /// what is already there and what was asked for — "already a manual box, and this would start
    /// it as a review one" is a sentence somebody can act on; two enum variants printed with
    /// `{:?}` is not.
    pub fn spelled(self) -> &'static str {
        match self {
            Purpose::Manual => "manual",
            Purpose::Review => "review",
        }
    }
}

/// A purpose skein does not recognise reads as [`Purpose::Manual`], never as a parse failure.
///
/// The `#[serde(default)]` beside this covers the absent field — the 13 records already on disk.
/// This covers the other direction: a record written by a later skein with a purpose this one has
/// never heard of. Both are the same judgement, that an unreadable placement record costs a
/// reachable box, and neither is worth that.
fn purpose_or_manual<'de, D: serde::Deserializer<'de>>(de: D) -> Result<Purpose, D::Error> {
    Ok(match String::deserialize(de) {
        Ok(word) => serde_json::from_value(serde_json::Value::String(word)).unwrap_or_default(),
        Err(_) => Purpose::default(),
    })
}

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

/// A command that RAN, whatever it exited with.
///
/// [`Place::exec`] and [`Place::bytes`] collapse a non-zero exit into an error built from stderr and
/// throw stdout away. That is right for the callers they have — a script that fails is a failure and
/// its stderr is the reason — and wrong for anything whose subject writes its diagnosis to stdout.
/// `claude -p` is exactly that: measured against the real CLI, an unknown model exits 1 with the
/// explanation on STDOUT and an unrelated stdin warning on stderr. A caller given only stderr is
/// told "exited 1" and nothing else, which is a failure this codebase shipped once already at the
/// layer above (`215d143`) and would have shipped again the moment the model call moved in here.
#[derive(Debug, Clone)]
pub struct Ran {
    pub code: i32,
    pub out: Vec<u8>,
    pub err: String,
}

/// The PATH every fleet-scope script resolves against — root-owned directories and nothing else.
///
/// bash's own default for a non-login shell, which is the point: what a fleet-scope script needs
/// (`sudo`, `git`, `jq`, `python3`, `bwrap`, `apt-get`, `npm`, `cc`, `curl`, `nsenter`) lives in
/// `/usr/bin`, and the two directories a profile would put ahead of it are writable by every box.
/// See [`Place::shell`] for the whole of why.
const FLEET_PATH: &str = "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin";

/// The two user-writable directories a BOX's own PATH carries in front of [`FLEET_PATH`].
///
/// Not a profile's doing and not a guess — **read out of the environment of the processes a live
/// box is running**. Every process inside a box on this fleet carries, verbatim:
///
/// ```text
/// PATH=/home/agent/.local/bin:/usr/local/share/npm-global/bin:/usr/local/sbin:/usr/local/bin:\
/// /usr/sbin:/usr/bin:/sbin:/bin
/// ```
///
/// which is `$HOME/.local/bin`, then this, then `FLEET_PATH` exactly. `~/.local/bin` is the one
/// that matters: it is where Claude Code installs itself, and `box-session.sh` says in as many
/// words that a box handed a home without it "has no agent and no way to authenticate one"
/// (`src/box-session.sh:13-18`).
///
/// **Neither of the two is writable from inside a box any more** (SKEIN-963, SKEIN-968), and the
/// name of this constant is why that had to be done twice. `~/.local` was the first entry of
/// `box-session.sh`'s `share_paths` and is a per-box copy-on-write overlay now; this second entry
/// was never under `$HOME` at all, so making `~/.local` private did not touch it — and it is the
/// one `which -a claude` answers with inside a real box. `box-session.sh` binds it `--ro-bind`.
/// This constant is unchanged: a crossing lands in the box's own mount namespace, so it inherits
/// both of those without having to know about either.
///
/// The PATH stays as it is because the ORDER is what callers depend on — `crate::agentpath` asks
/// whether the first `claude` on it is the agent — and the answer to "can a box change what that
/// resolves to" is now a mount rather than a path edit.
const BOX_PATH_HEAD: &str = "/usr/local/share/npm-global/bin";

/// The PATH a box runs on, built from the `home` in its placement — [`Place::wrap`] exports exactly
/// this, and exports it from here so there is only one of it.
///
/// **A second copy of this string is the bug, not the convenience.** `crate::agentpath` asks whether
/// the first `claude` on a box's PATH is the agent, and a check written from its own idea of what
/// that PATH is would be two lists inside one binary — the shape SKEIN-678 caught in the mount
/// check, where the row and the thing it checked were built from different sets, and the row
/// reported healthy on the one fleet it existed to catch.
pub fn box_path(home: &str) -> String {
    format!("{home}/.local/bin:{BOX_PATH_HEAD}:{FLEET_PATH}")
}

/// A whole sandbox, addressed as itself — [`Where::SandboxItself`].
///
/// **What every production caller passes is the fleet's own sandbox**, which is how [`crate::fleet`]
/// provisions and heals the sandbox the boxes live in. The name is a hangover from skein's original
/// model, where a box owned a sandbox named after it; nothing resolves a box that way now.
///
/// It is also what the argv builders use for a name `place_of` rejects: they used to interpolate
/// the name directly and had no failure path, so refusing here would turn a bad name from a command
/// that fails in the box into a panic in the server. Such an address reaches
/// [`Place::unreachable_from_fleet`], which refuses it in-band rather than aiming it at whatever
/// sandbox skein happens to be standing in.
pub fn own_sandbox(name: &str) -> Place {
    Place {
        name: name.to_string(),
        sandbox: name.to_string(),
        at: Where::SandboxItself,
    }
}

/// The name of the one sandbox that hosts every box.
///
/// Empty is not a second model any more — it is a fleet with no name, which no box can be started in.
/// See [`crate::config::Config::fleet_sandbox`].
pub fn fleet_sandbox() -> String {
    load_config().fleet_sandbox.trim().to_string()
}

/// **The test seam for fleet-scope execution** — and the shape of it is the whole point (SKEIN-592).
///
/// # Why this exists
///
/// A fixture that wants to stand in for what a fleet-scope script runs used to do it by putting a
/// fake `sbx` on `$PATH`. There is no `sbx` hop — the script runs on this machine — so the fake is
/// bypassed and the *real* command runs. `tests/resize_rules.rs` read this box's live Docker
/// volumes that way and named three belonging to other people's work. That path only read;
/// resize's other arm destroys a sandbox and copies a volume, and the distance between the two is
/// one branch.
///
/// **The reach of that has grown since, which is an argument for this seam rather than against
/// it.** The test above got to the hopless path by declaring the in-fleet deployment; a run that
/// declared nothing took the `sbx` hop and the fake caught it. SKEIN-521 deleted the host-driven
/// alternative, so there is nothing left to declare and no run that hops — every fleet-scope
/// script now runs straight at this machine, and a fixture that forgets this seam reaches the real
/// one by default.
///
/// The `$PATH` route cannot be reopened to fix it. Fleet-scope scripts run under [`Place::shell`]'s
/// **fixed** PATH, which is ISO-1: `~/.local/bin` is bound read-write into every box on a shared
/// uid, so a box that drops a `sudo` there would otherwise have it run at fleet scope. That is a
/// property to keep, not to trade for testability.
///
/// # What makes this safe, stated as a property rather than a hope
///
/// **Nothing a running box can set selects it.** Not an environment variable, not a `$PATH` entry,
/// not a file, not a config key. The substitution is installed by *calling a Rust function in this
/// process*, which is something only this program's own test code can do — and a box is on the
/// other side of a process boundary from all of it. An env-var-driven hook would be ISO-1 deleted
/// and re-spelled under a new name: the property ISO-1 buys is that a box cannot change what a
/// fleet-scope script resolves to, and a hook a box could set is exactly that property gone.
///
/// **And a shipped skein has no seam at all.** The module is behind `debug_assertions`, which
/// `bootstrap.sh` turns off — it builds `--release` (`bootstrap.sh`, `cargo build --release`). In
/// that binary [`taken`] is a function returning `None` with nothing behind it: no static, no lock,
/// no branch on anything.
///
/// [`tests::no_box_can_reach_the_execution_seam_and_a_shipped_skein_has_none`] asserts both halves
/// against the source, because they
/// are properties of what the code *is allowed to contain* rather than of what it computes.
#[cfg(debug_assertions)]
pub mod seam {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;

    /// Given the argv a fleet-scope command would have run, the argv to run instead — or `None` to
    /// leave it alone. A rewrite rather than a replacement of the whole execution, so the timeout,
    /// the output capture and the exit-code handling stay exactly the ones production uses.
    pub type Substitute = Box<dyn Fn(&[String]) -> Option<Vec<String>> + Send + Sync>;

    static INSTALLED: Mutex<Option<Substitute>> = Mutex::new(None);

    /// Put a substitution in place until the returned guard is dropped.
    ///
    /// A guard rather than a bare `install`/`clear` pair: a test that panics between them would
    /// leave the substitution in place for whatever ran next in the same process, which is the same
    /// shape of cross-test leak as an environment variable nobody put back.
    pub fn install(f: Substitute) -> Installed {
        *INSTALLED.lock().unwrap() = Some(f);
        Installed
    }

    /// The substitution for a test that reaches a crossing only on the way to something else:
    /// every fleet-scope command succeeds, silently, having done nothing.
    ///
    /// One implementation rather than one per suite, because there were about to be two — the lib's
    /// `testutil` and `tests/common/mod.rs` cannot see each other, and a second copy of a rule is
    /// the copy that stops agreeing. It lives beside [`install`] so both can reach it.
    ///
    /// `:` rather than a recording fake: what these tests assert is the DECISION in front of the
    /// crossing — who was asked about, what was spent — and something that succeeds having done
    /// nothing is the smallest thing that lets the decision be reached. A test that asserts on the
    /// argv writes its own substitution and reads it back, which is what `tests/fleet_move.rs` and
    /// `tests/resize_rules.rs` do.
    pub fn doing_nothing() -> Installed {
        install(Box::new(|_argv: &[String]| {
            Some(vec!["sh".to_string(), "-c".into(), ":".into()])
        }))
    }

    /// Removes the substitution on drop.
    pub struct Installed;

    impl Drop for Installed {
        fn drop(&mut self) {
            if let Ok(mut held) = INSTALLED.lock() {
                *held = None;
            }
        }
    }

    /// What production asks: is this argv being stood in for?
    pub fn taken(argv: &[String]) -> Option<Vec<String>> {
        INSTALLED.lock().ok()?.as_ref()?(argv)
    }

    /// Is there a stand-in at all — which is **not** the question [`taken`] answers.
    ///
    /// `taken` returning `None` has two readings and only one of them is a mistake: a substitution
    /// that inspects the argv and hands this one back is a fixture deciding to let it run
    /// (`tests/fleet_move.rs`'s `run` arm does exactly that), while no substitution at all is a
    /// fixture that never thought about it. [`super::Place::spawning`] refuses the second and
    /// allows the first, so it has to be able to tell them apart.
    pub fn installed() -> bool {
        INSTALLED.lock().is_ok_and(|held| held.is_some())
    }

    static REAL: AtomicUsize = AtomicUsize::new(0);

    /// **This process means its fleet-scope commands to run**, so [`super::Place::spawning`] does
    /// not refuse them, until the returned guard is dropped.
    ///
    /// The declared exemption from that guard, and it exists because two shapes carry
    /// `$SKEIN_TEST` and cannot install a substitution:
    ///
    /// * **A skein spawned by a test harness.** `src/bin/skein.rs` and `src/bin/skein-server.rs`
    ///   both say this in `main`. A `skein-server` started by `tests/server.rs` or by
    ///   `tests/ui/harness/server.mjs` inherits the marker from cargo's `[env]` table — correctly,
    ///   because [`crate::config::skein_home`] and [`crate::util::fleet_root`] must still refuse it
    ///   an unpinned path (SKEIN-685) — but a [`Substitute`] is a Rust closure and the test that
    ///   would write one is on the other side of a process boundary. What keeps that server inside
    ///   its fixture is the root it was handed, which is what those two guards are for.
    /// * **A suite whose subject IS the real command.** `tests/fleet_launch.rs` starts a box in a
    ///   fixture fleet and asserts it is usable; standing in for the crossing would delete what it
    ///   proves.
    ///
    /// Said by CALLING SOMETHING rather than by setting a variable, for [`install`]'s reason: a
    /// variable is settable by the very test the guard exists for, and by a box besides.
    pub fn real_crossings() -> Real {
        REAL.fetch_add(1, Ordering::SeqCst);
        Real
    }

    /// Takes the exemption away again on drop.
    pub struct Real;

    impl Drop for Real {
        fn drop(&mut self) {
            REAL.fetch_sub(1, Ordering::SeqCst);
        }
    }

    /// Has this process declared its crossings real? See [`real_crossings`].
    pub fn meant() -> bool {
        REAL.load(Ordering::SeqCst) > 0
    }
}

/// The seam's absence, in a build that ships. Every call is compiled away.
#[cfg(not(debug_assertions))]
pub mod seam {
    #[inline(always)]
    pub fn taken(_argv: &[String]) -> Option<Vec<String>> {
        None
    }

    #[inline(always)]
    pub fn installed() -> bool {
        false
    }

    /// A shipped skein is never a test harness, so its crossings are real by construction and the
    /// declaration is a value rather than a state. `main` still calls [`real_crossings`], because
    /// one `main` for both builds is the point of compiling this module away rather than the calls.
    #[inline(always)]
    pub fn meant() -> bool {
        true
    }

    #[inline(always)]
    pub fn real_crossings() -> Real {
        Real
    }

    pub struct Real;
}

impl Place {
    /// The argv that runs `script` in this place.
    ///
    /// Its own function so the wire format is testable without a sandbox — and because it is the
    /// contract the takeover guard asserts.
    pub fn exec_argv(&self, script: &str) -> Vec<String> {
        if let Some(refusal) = self.unreachable_from_fleet() {
            return refusal;
        }
        let mut argv = self.reach();
        argv.extend(self.enter());
        argv.extend(self.shell());
        argv.push(self.wrap(script));
        argv
    }

    /// How to spell `tmux` for this box: bare when the sandbox is the box, socket-qualified when it
    /// is shared. A shell fragment, because every tmux call skein makes is already part of one.
    ///
    /// Session *names* stay the same in both shapes (`skein-agent`, `skein-agent-<runtime>`) — under
    /// the shared model the socket is what separates one box's sessions from another's. Two boxes
    /// with a `skein-agent` session are then unambiguous, where sharing a server would collide on
    /// the first name and silently attach a box to its neighbour's agent.
    ///
    /// Note there is no `nsenter` here: the socket lives outside the box's private mounts, so the
    /// server answers from the sandbox directly. Commands the *session* runs are inside the
    /// namespace regardless, because the server itself is.
    pub fn tmux(&self) -> String {
        match &self.at {
            Where::SandboxItself => "tmux".into(),
            Where::Shared { sock, .. } => format!("tmux -S {}", sh_quote(sock)),
        }
    }

    /// This box's tmux socket, empty when the sandbox is the box. For the few callers that need the
    /// bare path rather than the `tmux` spelling — the pane observer runs its own tmux commands.
    pub fn tmux_sock(&self) -> &str {
        match &self.at {
            Where::SandboxItself => "",
            Where::Shared { sock, .. } => sock,
        }
    }

    /// The two argv elements that put the rest of a crossing on a **fixed** PATH (ISO-1).
    ///
    /// One function because [`Self::enter`] and [`Self::shell`] both need it, and they are two
    /// halves of one property: everything skein runs at fleet scope resolves its programs from
    /// root-owned directories, whatever PATH the process that built the argv happened to inherit.
    /// They were written apart, only [`Self::shell`] had it, and [`Self::enter`] records what that
    /// cost.
    ///
    /// **`env` itself still resolves from the inherited PATH**, because argv[0] must. That is one
    /// program rather than five, it is the same one [`Self::shell`] has always been exposed to, and
    /// spelling it `/usr/bin/env` would trade a PATH lookup for a hard-coded location nothing else
    /// in this tree assumes. Said out loud rather than left for a reader to find.
    fn path_pin() -> Vec<String> {
        vec!["env".into(), format!("PATH={FLEET_PATH}")]
    }

    /// The `nsenter` hop that puts a command inside this box's namespace — empty when the sandbox
    /// is the box, which is what keeps the original model byte-for-byte unchanged.
    ///
    /// # Everything in front of the hop runs at fleet scope, so it runs on a fixed PATH (ISO-1)
    ///
    /// The outer `bash`, the `nsenter`, and the `cat`, `sed` and `cut` that [`Self::guard`] spends
    /// on `/proc/<ns_pid>/stat` all run **outside the box's namespace, before any hop** — at fleet
    /// scope, where `sudo` works and the fleet root is readable. Unpinned, all five resolved from
    /// whatever PATH the spawning process inherited, and `~/.local/bin` is bound read-**write** into
    /// every box, every one of them uid 1000 ([`Self::shell`] carries ISO-1's measurement). So a box
    /// that dropped a file called `nsenter` there had it run at fleet scope — or one called `cat`,
    /// which is worse, because the guard is the check that stops a crossing entering some *other*
    /// box, and a planted `cat` answers it. A file copy, not an exploit.
    ///
    /// **[`Self::shell`] pinned this for a fleet-scope script and this did not, and the difference
    /// was where the two were written rather than a decision** (SKEIN-832).
    ///
    /// # The PATH a crossing inherits is not skein's to trust — counted, not sampled
    ///
    /// The tempting argument for leaving this unpinned is that `skein-server` is exec'd by
    /// `src/server-doorway.py`, which copies `dict(os.environ)` through untouched
    /// (`src/server-doorway.py:212-220`), from a `tmux new-session` skein started at fleet scope:
    /// [`crate::fleet::start_server`] does go through `own_sandbox(..).exec(..)`, so that one is
    /// under `env PATH={FLEET_PATH}` (`src/fleet.rs:892-905`). A tmux session does take its
    /// environment from the **client** that asked for it, so that much survives contact — measured
    /// on tmux 3.6 here, a session created by a client holding a clean PATH got the clean one even
    /// though the tmux server had been started with a planted directory at its head.
    ///
    /// It is still one start path of several, and not the one that matters. Every process that
    /// spawns a crossing, and the PATH each carries:
    ///
    /// | what spawns the crossing | how it was started | the PATH it resolves `nsenter` from |
    /// |---|---|---|
    /// | `skein-server` | `fleet::start_server`, a `Place` at fleet scope (`src/fleet.rs:903`) | `FLEET_PATH` — the only pinned one |
    /// | `skein-server` | sbx's `commands.startup` runs `start-door.sh` at every sandbox start (`src/fleet-kit-spec.yaml:31`) | sbx's, for a uid-1000 `bash -c`. Not skein's to set |
    /// | `skein-server` | `bootstrap.sh:618` runs `start-door.sh`, having done `export PATH="$CARGO_HOME/bin:$PATH"` (`bootstrap.sh:348`) | a toolchain directory, then whatever ran `bootstrap.sh` |
    /// | `skein-server` | a person putting the door back: `sbx exec -i <sandbox> /boxes/.skein/start-door.sh` (`bootstrap.sh:478`) | that person's shell's |
    /// | `skein-server` | a developer: `./target/release/skein-server` (`README.md:162`) | that developer's shell's |
    /// | `skein` | **a person typing `skein attach <box>`** — `run_attach` spawns the crossing argv with `Command::new(program)` (`src/bin/skein.rs:1346`) | that person's shell's, `~/.local/bin` at its head |
    /// | `skein` | spawned by `skein-server`, which copies its whole environment in (`src/bin/skein-server.rs:4411-4413`) | the server's, whatever the rows above left it |
    ///
    /// `start-door.sh` pins nothing (`bootstrap.sh:556`), and the `SIGUSR1` reload re-execs across
    /// the same environment (`src/server-doorway.py:185-196`), so whatever PATH a fleet's first
    /// `start-door.sh` had is frozen into every `skein-server` after it, upgrades included.
    ///
    /// **The last two rows are why this is pinned rather than written down as safe.** `skein
    /// attach` is a documented command a person runs in their own terminal; there is no wording of
    /// "every start path has a trusted PATH" that is true while it exists. skein's own code says as
    /// much where it can see the consequence — [`crate::ai::Unread`] tells a reader that "the server
    /// inherits the PATH of whatever launched it" and to "start the server from a shell that has
    /// it" (`src/ai.rs:211-216`). Pinning also makes the property local: it is one line here, not a
    /// claim about every start path anyone adds later, which is the list that goes stale.
    ///
    /// **Nothing is lost by pinning.** Every program a crossing runs before the hop — `env`,
    /// `bash`, `nsenter`, `cat`, `sed`, `cut` — is in `/usr/bin` on this substrate and so inside
    /// `FLEET_PATH`: `env PATH={FLEET_PATH} sh -c 'command -v …'` finds all six.
    ///
    /// **It also stops the spawner's PATH riding into the box**, finishing a job [`Self::wrap`]
    /// already does for `HOME`, `SKEIN_BOX` and the working directory — `nsenter` carries the
    /// caller's environment, not the box's.
    ///
    /// **What it must not do is leave `FLEET_PATH` standing as the box's PATH**, and for one commit
    /// it did. This pin was written believing the trailing `-lc` would rebuild PATH from the box's
    /// profile; there is no profile on this substrate to rebuild it from, so the box ran on
    /// `FLEET_PATH` — which has no `~/.local/bin` and therefore no agent CLI. [`Self::wrap`] sets
    /// the box's own PATH past the hop and carries the measurement. The two are one property: **in
    /// front of the hop, root-owned directories; past it, the box's.**
    ///
    /// Asserted by `tests/isolation_bwrap.rs::a_planted_nsenter_is_not_what_a_crossing_runs`.
    fn enter(&self) -> Vec<String> {
        match &self.at {
            Where::SandboxItself => vec![],
            // A shell rather than a bare `nsenter`, because the check has to happen in the process
            // that crosses. `"$@"` carries whatever the caller appends through untouched, so this
            // stays an argv splice and nothing gets re-quoted on the way in.
            // An address that cannot be proved builds no `nsenter` at all, rather than one behind
            // a check. Nothing then has to hold for the refusal to hold.
            //
            // Pinned on this arm too, though its script spends no external program: without the
            // pin argv[0] is `bash`, and a refusal that runs a box's planted `bash` at fleet scope
            // in order to print itself has still run it.
            Where::Shared { .. } if !self.provable() => {
                let mut argv = Self::path_pin();
                argv.extend(["bash".to_string(), "-c".into(), self.guard(), "bash".into()]);
                argv
            }
            Where::Shared { .. } => {
                let mut argv = Self::path_pin();
                argv.extend([
                    "bash".to_string(),
                    "-c".into(),
                    format!("{}exec {} -- \"$@\"", self.guard(), self.nsenter()),
                    "bash".into(),
                ]);
                argv
            }
        }
    }

    /// How skein gets to the **sandbox**, before [`Self::enter`] gets it to the box: **it is
    /// already there, so this is nothing at all.**
    ///
    /// A crossing used to have two hops, and the first was `sbx exec [flags] <sandbox>` — a host
    /// reaching into a guest. Skein runs inside the fleet sandbox now (SKEIN-576), so the machine
    /// an address names is the machine this process is on, and `sbx` is host-only: not merely
    /// unnecessary here but absent.
    ///
    /// **Kept as a named empty rather than deleted at the four call sites.** "Which hops does a
    /// crossing have" is exactly the sort of question that gets answered differently in one builder
    /// after somebody changes the other three, and the answer wants somewhere to live. It took a
    /// `flags` argument while there was a hop to give flags to; passing `-i` to nothing was the
    /// kind of argument that reads as load-bearing and is not, so it went with the hop.
    fn reach(&self) -> Vec<String> {
        Vec::new()
    }

    /// **A sandbox that is not the one this process is standing in cannot be reached at all** — so
    /// an address for one is refused rather than aimed.
    ///
    /// That is the whole invariant, and it is about *which sandbox*, not about what kind of box
    /// once lived in it. Found by writing [`Self::reach`] rather than by planning. A crossing used
    /// to have two hops: `sbx exec` to the sandbox, then `nsenter` to the box inside it. The first
    /// is gone, correctly — skein is already in the sandbox, and `sbx` is host-only, so it is
    /// not merely unnecessary but unavailable. [`Where::SandboxItself`] has no second hop either:
    /// `enter()` is empty, because the address is the sandbox and there is no box in it to enter.
    /// Drop both and the command does not fail — it *runs*, in whatever sandbox skein happens to be
    /// standing in, with the same paths on it and other people's files at them.
    ///
    /// So it refuses, in-band, the way [`crate::sandbox::refusal_argv`] does — the caller is
    /// usually a terminal, and an argv that prints why is read where an `Err` several layers up is
    /// not.
    ///
    /// # The sandbox we ARE standing in is the case this exists to let through
    ///
    /// That is not a corner case, it is most of [`crate::fleet`]: `ensure_substrate`,
    /// `ensure_fleet_root`, `install_launcher` and `install_docker_config` all address the fleet's
    /// own sandbox through [`own_sandbox`], which is how a fleet provisions itself. Refusing there
    /// refused skein's own setup: every box start in-fleet printed this message instead of doing
    /// the work, and the fleet could not provision itself at all.
    ///
    /// Having no hops at all is exactly right there. Nothing to the sandbox because skein is
    /// already inside it, and no `nsenter` because the address is a sandbox rather than a box. The
    /// command runs on the machine it was addressed to, which is the whole test.
    ///
    /// An unnamed fleet matches nothing, so it still refuses — a sandbox this build cannot identify
    /// as its own is one it has no business assuming it is standing in.
    fn unreachable_from_fleet(&self) -> Option<Vec<String>> {
        // Nothing to enter: this address names a sandbox, so `enter()` adds no second hop and
        // `reach()`'s first hop is the only one there was.
        let no_hop_inside = matches!(self.at, Where::SandboxItself);
        // The sandbox this process is standing in — the one address that needs no hop at all.
        let ours = fleet_sandbox();
        let the_one_we_are_in = !ours.is_empty() && self.sandbox == ours;
        (no_hop_inside && !the_one_we_are_in).then(|| {
            vec![
                "sh".to_string(),
                "-c".into(),
                format!(
                    "echo 'skein: {sandbox} is not the sandbox this skein is running inside, and \
                     sbx is host-only — there is no sbx here to reach another sandbox with. Move \
                     its work into this fleet.' >&2; exit 1",
                    sandbox = self.sandbox
                ),
            ]
        })
    }

    /// The `nsenter` invocation itself, without the guard in front of it.
    fn nsenter(&self) -> String {
        match &self.at {
            Where::SandboxItself => String::new(),
            Where::Shared { ns_pid, .. } => format!(
                "nsenter --user=/proc/{ns_pid}/ns/user --mount=/proc/{ns_pid}/ns/mnt \
                 --preserve-credentials"
            ),
        }
    }

    /// Does this address carry the two halves that make it checkable?
    ///
    /// False for a record written before the stamp existed. Not "unknown" — unprovable, which is
    /// treated exactly as a mismatch is.
    fn provable(&self) -> bool {
        match &self.at {
            Where::SandboxItself => true,
            Where::Shared {
                generation,
                ns_start,
                ..
            } => !generation.is_empty() && *ns_start > 0,
        }
    }

    /// Refuse the crossing unless the anchor is still the process skein recorded.
    ///
    /// **Why it is here and not where the address is looked up.** A pid is a name that can be
    /// reused, so any check with a gap after it is a check on a different question than the one the
    /// crossing asks. This runs in the shell that is about to `exec nsenter`, one line before it —
    /// the smallest gap available without a kernel handle.
    ///
    /// **Why a refusal and never a fallback.** A pid that no longer names what skein recorded names
    /// something *else in the same sandbox*, and everything else in that sandbox is another box. So
    /// there is no degraded mode to fall back to: "enter this instead" is the vulnerability, not the
    /// recovery from it.
    ///
    /// An address recorded before the stamp existed cannot be checked, so it is refused too. The
    /// alternative is a fleet where the guard is present and silently does nothing for every box
    /// that has not been restarted, which is worse than one that says so.
    fn guard(&self) -> String {
        let Where::Shared {
            ns_pid,
            generation,
            ns_start,
            ..
        } = &self.at
        else {
            return String::new();
        };
        let name = &self.name;
        if generation.is_empty() || *ns_start == 0 {
            return format!(
                "echo \"skein: {name} was placed before skein checked anchors, so its address \
                 cannot be proved to be its own; restart it with: skein restart {name}\" >&2; \
                 exit 78\n"
            );
        }
        // Cut after the LAST `) ` rather than taking whitespace field 22: `comm` is the process's
        // own name in parentheses and may contain spaces and parentheses of its own, so `$22` is
        // right until something is called an awkward name and then it is silently off.
        // **Two facts, two answers.** These were one branch and one sentence — "{name} is gone —
        // pid N is no longer the session skein recorded" — which names neither of the things it
        // just measured. A person who reads that about the box they were working in cannot tell
        // whether they lost one box or the whole sandbox, and those want opposite reactions:
        //
        //   * the boot id differs -> the SANDBOX restarted. Nothing that was running in it
        //     survived, every box is in this same state, and the fleet needs starting again. That
        //     is a fleet-wide fact arriving one box at a time.
        //   * the start time differs -> that one pid was reused by another process. About this box
        //     and nothing else.
        //
        // `fleet::anchor_matches` already separates them on the other path into a box, so this was
        // the odd one out — with both values in hand at the moment it decided.
        format!(
            "skein_gen=\"$(cat /proc/sys/kernel/random/boot_id 2>/dev/null)\"\n\
             skein_start=\"$(sed -n 's/.*) //p' /proc/{ns_pid}/stat 2>/dev/null | cut -d' ' -f20)\"\n\
             if [ \"$skein_gen\" != {gen_q} ]; then\n\
             \x20 echo \"skein: the sandbox has restarted since {name} was placed, so {name} is \
             gone and so is everything else that was running in it — start them again with: skein \
             start <box>\" >&2\n\
             \x20 exit 78\n\
             fi\n\
             if [ \"$skein_start\" != {start_q} ]; then\n\
             \x20 echo \"skein: {name} is gone — pid {ns_pid} has been reused by another process \
             since skein recorded it, so entering it would be entering some other box; restart it \
             with: skein restart {name}\" >&2\n\
             \x20 exit 78\n\
             fi\n",
            gen_q = sh_quote(generation),
            start_q = sh_quote(&ns_start.to_string()),
        )
    }

    /// Put the script where it expects to be: at the repo root, with the box's own HOME **and the
    /// box's own PATH**.
    ///
    /// `nsenter` carries the *caller's* environment and working directory into the namespace, so
    /// neither is inherited from the box. A script that assumed it started at the tree root would
    /// otherwise run somewhere arbitrary, and one reading `~/.config/sync/env` would read skein's.
    ///
    /// # PATH is the third member of that family, and it was the one this forgot (SKEIN-832)
    ///
    /// The same sentence covers it: `nsenter` carries the caller's environment, so the PATH inside
    /// the box was never the box's either. That was invisible for as long as the value being
    /// carried happened to *contain* `~/.local/bin` — a person's login PATH does, and so does the
    /// PATH a doorway-started `skein-server` inherits. [`Self::path_pin`] changed which wrong value
    /// crosses, from the caller's to [`FLEET_PATH`], and [`FLEET_PATH`] contains no `~/.local/bin`
    /// at all. That is where `claude` lives, so `tests/fleet_launch.rs` caught it at once: a box
    /// answered `command -v claude` with `/usr/local/bin/claude` — **the substrate's copy, not the
    /// one the fleet installs and shares** — and a sandbox without one would have answered nothing.
    ///
    /// **The tempting fix is to carry the caller's PATH across the hop instead of the pin, and it
    /// is wrong**: it makes what a box resolves depend on who spawned the crossing.
    /// [`crate::fleet::start_server`] starts the server through `own_sandbox(..).exec(..)`
    /// (`src/fleet.rs:903`), which is itself under `env PATH={FLEET_PATH}` — so on that start path
    /// the caller's PATH *is* `FLEET_PATH`, and carrying it would reproduce this same bug while
    /// looking like a fix. The box's PATH has to be derived from the box.
    ///
    /// **And `-lc` does not rescue it**, which is the assumption [`Self::shell`] used to record.
    /// Measured on this substrate rather than assumed: there is no `~/.profile` in a box's private
    /// home and none in the sandbox's home either, and `/etc/profile` here touches PATH nowhere —
    /// `env PATH={FLEET_PATH} bash -lc 'echo $PATH'` prints `FLEET_PATH` back unchanged, with
    /// `command -v cargo` empty. A login shell rebuilds nothing; the PATH a box runs on is the one
    /// it was handed.
    ///
    /// So it is handed the box's own: `$HOME/.local/bin`, [`BOX_PATH_HEAD`], then [`FLEET_PATH`] —
    /// written from the `home` in the placement rather than from `$HOME` in the shell, so it cannot
    /// depend on the order two words of one `export` are expanded in.
    ///
    /// **This is the far side of the hop and only the far side.** Everything in front of the hop
    /// still runs on [`FLEET_PATH`] ([`Self::enter`]), and so does [`Self::raw_argv`], which builds
    /// no shell and so never reaches here: its one production caller is
    /// [`crate::takeover::copy_guest_file`] running `cat` (`src/takeover.rs:128`, the only one of
    /// ten `raw_argv` mentions outside this file's own tests), which `FLEET_PATH` resolves — and
    /// resolving skein's own utilities from root-owned directories even inside a box is the
    /// stronger answer, not the weaker one.
    fn wrap(&self, script: &str) -> String {
        match &self.at {
            Where::SandboxItself => script.to_string(),
            // SKEIN_BOX as well as HOME, because entering the namespace is not the same as being
            // launched into it. `box-session.sh` exports the identity for the session it starts, but
            // a later `nsenter` gets a fresh environment — so anything skein runs through a
            // placement had only `SANDBOX_VM_ID` to go on, which names the SANDBOX and is the same
            // string for every box in it.
            //
            // Measured: every fleet box's screen observer wrote `skein-fleet.pane.json` into its own
            // repo's store, so no box had a fresh screen observation and the board said "screen
            // lost" for all of them — while each box's *hooks*, which inherit from the agent process
            // that `box-session.sh` did launch, were filing correctly under the box's own name.
            Where::Shared { home, tree, .. } => format!(
                "export HOME={} SKEIN_BOX={} PATH={} && cd {} && {script}",
                sh_quote(home),
                sh_quote(&self.name),
                sh_quote(&box_path(home)),
                sh_quote(tree)
            ),
        }
    }

    /// The shell a script runs under, and **which one depends on whose machine it is**.
    ///
    /// **Fleet scope gets `-c` under a fixed PATH, never `-lc`** (ISO-1). A script addressed to the
    /// sandbox itself runs OUTSIDE every box's mount namespace, where `sudo` works and the whole of
    /// the fleet root is there to read. A login shell sources the profile, and on this substrate
    /// that profile puts `~/.local/bin` and `/usr/local/share/npm-global/bin` at the head of PATH —
    /// both owned by uid 1000, which is what every box runs as, and `.local` is bound read-WRITE
    /// into every box so that eleven of them share one toolchain. So a box that dropped a `sudo`, a
    /// `tmux` or a `python3` into `~/.local/bin` had it run at fleet scope with the real one behind
    /// it: a file copy, not an exploit.
    ///
    /// **This property used to live in the in-sandbox agent**, which built its own argv and was the
    /// only fleet-scope path that closed the hole; the spawned path beside it still used `-lc`. The
    /// agent is deleted (SKEIN-573) and this is the surviving path, so the property moved here
    /// rather than going with it — which is the whole of what "delete the transport, keep what it
    /// was carrying" has to mean.
    ///
    /// **A crossing into a box still ends in `-lc`**, and that is not an oversight: `enter()` has
    /// already put it inside the box's namespace, where a box's own profile is the box's own
    /// business.
    ///
    /// **What `-lc` is NOT is a way to get a PATH.** It was read as one, and the reading was never
    /// measured: on this substrate a box has no `~/.profile` — nor does the sandbox's home — and
    /// `/etc/profile` sets no PATH, so `env PATH={FLEET_PATH} bash -lc 'echo $PATH'` prints
    /// `FLEET_PATH` straight back. The box's PATH is whatever it was handed, which is why
    /// [`Self::wrap`] hands it one (SKEIN-832).
    ///
    /// **What WAS an oversight is the part of a crossing that runs before that hop**, and for a
    /// long time this was the only arm that pinned anything. [`Self::enter`] pins it now, through
    /// the same [`Self::path_pin`] — the two are one property with two halves, and they are written
    /// as one function so they cannot drift (SKEIN-832).
    ///
    /// Nothing skein sends at fleet scope wants the sandbox user's profile — the scripts name what
    /// they need, and the one that builds skein exports its own `CARGO_HOME`/PATH
    /// (`bootstrap.sh`).
    fn shell(&self) -> Vec<String> {
        match &self.at {
            Where::SandboxItself => {
                let mut argv = Self::path_pin();
                argv.extend(["bash".to_string(), "-c".into()]);
                argv
            }
            // No fleet pin here, and none wanted: [`Self::enter`] has already put one in front of
            // this for the arm that runs anything at fleet scope, and a second one would be saying
            // something about a machine the first has already left. Past the hop the PATH is the
            // box's own — set by [`Self::wrap`] in the script this shell runs, because nothing else
            // out here sets it and `-l` does not.
            Where::Shared { .. } => vec!["bash".into(), "-lc".into()],
        }
    }

    /// The **whole** argv for an interactive attach — a terminal, not a captured command.
    ///
    /// This used to return everything *after* the program name, because both callers handed a
    /// literal `"sbx"` to a PTY spawner. That was kept deliberately, on the grounds that changing it
    /// would touch the terminal plumbing on both ends "for no behavioural gain". The gain arrived
    /// with the hop's removal: the program is not `sbx` at all, and a builder that returns arguments
    /// for a program it does not name cannot say so.
    ///
    /// The whole attach runs inside the namespace, not just the tmux call. The shell it carries
    /// refreshes the runtime's instruction file, runs the runtime's setup and starts the pane
    /// observer — all of which read and write the box's own HOME and tree. Outside the hop they
    /// would quietly operate on skein's.
    pub fn interactive_argv(&self, script: &str) -> Vec<String> {
        if let Some(refusal) = self.unreachable_from_fleet() {
            return refusal;
        }
        let mut argv = self.reach();
        argv.extend(self.enter());
        argv.extend(self.shell());
        argv.push(self.wrap(script));
        argv
    }

    /// The argv for running a command here *without* a shell — `["cat", path]` and friends.
    ///
    /// For callers that stream stdout somewhere other than a buffer, so they keep their own
    /// plumbing while the sandbox name still resolves through here rather than being assumed.
    pub fn raw_argv(&self, args: &[&str]) -> Vec<String> {
        if let Some(refusal) = self.unreachable_from_fleet() {
            return refusal;
        }
        let mut argv = self.reach();
        argv.extend(self.enter());
        argv.extend(args.iter().map(|a| a.to_string()));
        argv
    }

    /// The argv a fleet-scope command is about to be spawned with: the substitution applied, and
    /// **a test process that installed none refused rather than run for real**.
    ///
    /// [`seam`]'s own note already says what happens without this — "every fleet-scope script now
    /// runs straight at this machine, and a fixture that forgets this seam reaches the real one by
    /// default". [`Place::reach`] is empty and [`Where::SandboxItself`] adds no `nsenter`, so for
    /// the addresses most of [`crate::fleet`] uses the argv here is a shell command on **this**
    /// machine, at fleet scope, beside every real box on it. Forgetting the seam is invisible:
    /// nothing fails, the command runs, and what it touched is only discoverable afterwards by
    /// looking. That is the shape SKEIN-530 is the general version of — five tests installed
    /// uncommitted code onto the owner's live fleet, and the run was green.
    ///
    /// So the omission is made loud instead. In production [`crate::util::in_test`] is false and
    /// this is one predicate on an environment variable; in a test it is the difference between a
    /// panic naming the seam and a command nobody meant to run.
    ///
    /// **A test that means it says so** — [`seam::real_crossings`], which `skein`'s and
    /// `skein-server`'s `main` and `tests/fleet_launch.rs` call, because a spawned skein cannot be
    /// handed a closure and an end-to-end suite's subject is the real command. That is a declared
    /// exemption in the shape of `tests/platform_gates.rs`'s `GATED`: it costs a line in the diff,
    /// where the omission it replaces cost nothing and said nothing.
    ///
    /// # The gap this used to have, and the one it still has
    ///
    /// This is a checkpoint on the argv, not on the builder, and for a while only [`Self::command`]
    /// and [`Self::write`] passed through it — the two paths where `Place` spawns the process
    /// itself. [`Self::exec_argv`], [`Self::raw_argv`] and [`Self::interactive_argv`] *return* an
    /// argv the caller spawns, so a caller that took one of those and spawned it was outside both
    /// halves of the seam: no substitution, and no refusal. Three such callers were in this crate,
    /// and they now call this before they spawn (SKEIN-764):
    ///
    /// | caller | builder | how it spawns |
    /// |---|---|---|
    /// | [`crate::takeover::copy_guest_file`] | `raw_argv` | `Command::new` with the guest file streamed to a host file |
    /// | `sandbox::resume_box` | `exec_argv` | quoted into a bigger shell string, run as `sh -c` |
    /// | `sandbox::restart_agent_session` | `exec_argv` | `util::run_capture` |
    ///
    /// None of the three can use [`Self::command`] instead — one redirects stdout to a file, one
    /// needs the argv as *text* inside another command, one wants the capture helper — which is
    /// why the seam is a checkpoint they call rather than a wrapper they go through.
    ///
    /// **The remaining callers are in `src/bin/`, and a checkpoint cannot help them.** `shell_argv`,
    /// `attach_argv_as`, `initial_attach_argv_as` and `box_write_argv` hand their argv across the
    /// crate boundary to `skein.rs` and `skein-server.rs`, which spawn it — and both of those
    /// `main`s open with [`seam::real_crossings`] (`src/bin/skein.rs:22`,
    /// `src/bin/skein-server.rs:111`), so the refusal is declared away before the argv is built.
    /// That is not an oversight to close: a spawned skein cannot be handed a closure, which is the
    /// whole reason the exemption exists.
    ///
    /// **And the checkpoint deliberately does not move up into those builders**, which is where it
    /// would have to go to cover them. Building an argv touches nothing, and most of what calls a
    /// builder never spawns what it gets. Count them rather than take that on trust:
    ///
    /// ```sh
    /// grep -rnE '(exec|raw|interactive|write)_argv\(' src/ tests/ | grep -vE 'fn |///'
    /// ```
    ///
    /// Twenty-nine lines, and every one is one of four things. Five are production spawns and all
    /// five now pass through here ([`Self::command`], [`Self::write`], and the three in the table
    /// above). Four are the builders whose argv leaves the crate for `src/bin/` and the
    /// `skein-server` line that spawns one — `box_write_argv`, `agent_attach_argv`,
    /// `shell_argv`. Two are tests whose subject IS the
    /// crossing, `tests/fleet_launch.rs`'s shape in miniature: this module's
    /// `a_crossing_in_the_fleet_enters_the_box_without_sbx` runs its argv into a bwrap namespace it
    /// built itself, and `tests/isolation_bwrap.rs`'s
    /// `a_planted_binary_is_not_what_a_fleet_scope_script_runs` runs it with a `$HOME` and `$PATH`
    /// of its own — neither can be stood in for without deleting what it proves.
    ///
    /// **The remaining eighteen assert the wire format and spawn nothing**, and
    /// refusing those would be [`crate::warden_client::Warden::configured`]'s mistake exactly:
    /// guarding the address rather than the connection, so that asking *what skein would run*
    /// costs a fixture it does not need.
    pub(crate) fn spawning(&self, argv: Vec<String>) -> Vec<String> {
        if let Some(instead) = seam::taken(&argv) {
            return instead;
        }
        // **A refusal is not a crossing.** For an address that cannot be reached from here the argv
        // builders short-circuit to [`Self::unreachable_from_fleet`]'s in-band
        // `echo …; exit 1` — a message and an exit code, touching nothing — and it is deliberately
        // spawned rather than returned as an `Err` so a terminal shows it. Refusing that would fail
        // the tests that assert skein *declines* to reach another sandbox, which is the opposite of
        // what this guard is for.
        if self.unreachable_from_fleet().is_some() {
            return argv;
        }
        assert!(
            !crate::util::in_test() || seam::installed() || seam::meant(),
            "a fleet-scope command is about to run FOR REAL in a test process \
             (${marker}), because no stand-in is installed. `Place::reach` is empty, so this \
             runs on the machine skein is standing on — the owner's live fleet on any machine \
             running skein, where five tests once installed uncommitted code (SKEIN-530). \
             Install one first:\n    \
             let _seam = skein::place::seam::install(Box::new(|argv| Some(vec![..])));\n\
             A substitution that returns `None` for an argv still lets it run, which is how a \
             fixture says it meant that one. If the real command IS the subject — an end-to-end \
             suite against a fixture fleet — say so instead:\n    \
             let _real = skein::place::seam::real_crossings();\n\
             The argv this would have spawned: {argv:?}",
            marker = crate::util::TEST_MARKER,
        );
        argv
    }

    fn command(&self, script: &str) -> Command {
        // The one place a fleet-scope command is turned into a process, and therefore the one place
        // a test may stand in for it. See [`seam`] for why this is a compile-time substitution and
        // not a `$PATH` entry or an environment variable.
        let argv = self.spawning(self.exec_argv(script));
        let mut command = Command::new(&argv[0]);
        command.args(&argv[1..]);
        command
    }

    /// Run `script` and return its stdout as text. A non-zero exit is an error carrying the box's
    /// own stderr, because the box's words are always more use than "exited 1".
    pub fn exec(&self, script: &str, timeout: Duration) -> Result<String, String> {
        let out = self.bytes(script, timeout)?;
        Ok(String::from_utf8_lossy(&out).into_owned())
    }

    /// Run `script` with `feed` on its **stdin**, and report what happened, rather than whether it
    /// worked.
    ///
    /// `Err` means it did not run at all — the sandbox was unreachable, or it outlived `timeout`.
    /// Any exit code is `Ok`, because "it ran and said no" is an answer, and only the caller knows
    /// what to make of it. See [`Ran`].
    ///
    /// **`feed` is a parameter and not a second method** (SKEIN-799). There is one caller —
    /// [`crate::fleet::model_call_in_box`] — and a twin that differed only in its stdin would be
    /// one function nothing calls plus one place for the two to stop agreeing. `&[]` is a pipe
    /// that closes immediately rather than `/dev/null`, which is the same thing to every script
    /// this could ever carry: a script that reads stdin gets EOF either way.
    ///
    /// **Why this grew a stdin at all.** [`Self::exec_argv`] ends `argv.push(self.wrap(script))`,
    /// so the whole script is ONE argv element — and Linux caps a single element at
    /// `MAX_ARG_STRLEN`, 32 pages, which is 131,072 bytes on ordinary 4 KiB-page hardware. A model
    /// call's prompt carries a diff and 3 of 27 real ones measured for SKEIN-706 are over that, so
    /// a script with the prompt inside it could not be spawned at all. A pipe has no such ceiling,
    /// and it is also not `/proc/<pid>/cmdline` — the rule [`Self::write`] was written for, applied
    /// to the payload rather than only to the credential beside it.
    pub fn attempt(&self, script: &str, feed: &[u8], timeout: Duration) -> Result<Ran, String> {
        let mut command = self.command(script);
        // `output_with_timeout_fed`, not `bounded_output`: the second says "it failed to start or
        // exceeded the 30s timeout" for both, and this is the one caller where the difference is
        // the whole answer. A model call that never left the host was reported to a
        // person as "skein could not start `claude` … set SKEIN_CLAUDE_BIN to its full path", for a
        // host where `claude` was fine and `sbx` was missing. The message it gets instead names the
        // program that failed AND the PATH skein had, which is the one fact the reader cannot
        // recover afterwards — by the time they look, they are looking at their shell's PATH.
        //
        // `_fed` rather than `_why` only for the stdin: it is the same `run_bounded`, and it is
        // what already carries the prompt on the LOCAL arm of this same call (`ai::tried`), so the
        // two destinations now deliver the prompt by the same mechanism as well as to the same
        // place.
        let out = crate::util::output_with_timeout_fed(&mut command, feed.to_vec(), timeout)?;
        Ok(Ran {
            code: out.status.code().unwrap_or(-1),
            out: out.stdout,
            err: String::from_utf8_lossy(&out.stderr).trim().to_string(),
        })
    }

    /// Run `script` here and return its stdout as **raw bytes**.
    ///
    /// **One way in.** This used to try the in-sandbox agent first and fall back to spawning the
    /// crossing — a pairing that existed because the crossing was a host-to-guest hop that could
    /// stall, and the agent was the thing built to survive it. The hop is gone and so is the agent
    /// (architecture §13a, SKEIN-521), which takes the fallback twin with it: there is no
    /// transport-failure-versus-command-failure distinction left to draw, because there is no
    /// transport between the caller and the command.
    pub fn bytes(&self, script: &str, timeout: Duration) -> Result<Vec<u8>, String> {
        let mut command = self.command(script);
        let out = bounded_output(&mut command, "the crossing", timeout)?;
        if !out.status.success() {
            let detail = String::from_utf8_lossy(&out.stderr).trim().to_string();
            return Err(if detail.is_empty() {
                format!("the crossing exited {}", out.status)
            } else {
                detail
            });
        }
        Ok(out.stdout)
    }

    /// The argv that runs `script` here with a body arriving on its stdin.
    ///
    /// It used to differ from [`Self::exec_argv`] by an `-i` on the hop, without which `sbx exec`
    /// wired no pipe to the guest and the body was silently discarded. There is no hop to flag now
    /// (SKEIN-576): the pipe is [`Self::write`]'s own `Stdio::piped()`, on a process this one
    /// spawns directly. The two argvs are the same shape, and this stays its own function because
    /// the *call* still differs — a write has a body and a deadline that has to cover sending it.
    pub fn write_argv(&self, script: &str) -> Vec<String> {
        if let Some(refusal) = self.unreachable_from_fleet() {
            return refusal;
        }
        let mut argv = self.reach();
        argv.extend(self.enter());
        argv.extend(self.shell());
        argv.push(self.wrap(script));
        argv
    }

    /// Run `script` with `body` on its **stdin**.
    ///
    /// **The only way skein sends a box anything sensitive**, and that predates everything else
    /// here: an argv is visible in `ps` to anything sharing this machine, so a token passed as an
    /// argument is a token in every process listing and every shell history. A body on stdin is
    /// not.
    ///
    /// There used to be a second implementation of this — a chunked write to the in-sandbox agent,
    /// chosen for bodies under a cap — because the spawned path crossed a host-to-guest hop that
    /// could stall. There is no hop and no agent (SKEIN-521), and the surviving path is the one
    /// that never had a ceiling: it streams from a thread with a deadline.
    pub fn write(&self, script: &str, body: &[u8], timeout: Duration) -> Result<(), String> {
        // The seam covers this path too, and it has to: a write is a fleet-scope command like any
        // other, and a test that could stand in for `exec` but not for `write` would run the real
        // one — which is the hazard `seam` exists for, on the path that carries a body. Through
        // `spawning` for the same reason: the refusal has to be on every path that spawns, or the
        // one it is missing from is the one a fixture reaches the real fleet through.
        let argv = self.spawning(self.write_argv(script));
        let mut child = Command::new(&argv[0])
            .args(&argv[1..])
            // Nothing reads stdout here, and an unread pipe blocks the child once its buffer fills
            // (~64KB) — a chatty command would look like a hang until the deadline killed it.
            .stdout(Stdio::null())
            .stdin(Stdio::piped())
            .stderr(Stdio::piped())
            // Its own process group, so the deadline below ends the WORK. Nothing on this path is
            // a leaf program: `sbx` starts work of its own, and the shell it runs the script under
            // is `dash` here, which FORKS a `-c` command rather than exec'ing it — so the guest
            // command is already a grandchild by the time there is anything to kill. Killing the
            // recorded pid left it running with `ppid` 1, on the path EVERY install skein does
            // takes (SKEIN-912, SKEIN-916). A NEW group and not the inherited one: a negative kill
            // against skein's own group is skein killing itself.
            //
            // **The cost, paid rather than taken**, as `util::run_bounded` states it: a child in
            // its own group no longer shares the terminal's foreground group, so Ctrl-C stops
            // reaching this command by that route. The guard below hands the group to
            // `util::forward_interrupts`'s handler instead, which is the same payment
            // `run_bounded` makes. This loop has no grace arm of its own, so a script that ignores
            // `SIGINT` is waited on until `timeout`; a second Ctrl-C ends the group at once.
            .process_group(0)
            .spawn()
            .map_err(|e| format!("the crossing could not be started: {e}"))?;
        // Registered before this side blocks on anything, so a Ctrl-C arriving between the spawn
        // and the first `try_wait` finds the group rather than an empty table. Underscored because
        // every way out of the loop below is a `return`: the guard is dropped by the scope ending,
        // on the line after the reap rather than somewhere a `drop` call could be written.
        let _forwarding = crate::util::forwarding(child.id() as libc::pid_t);
        // Drained on a thread for the same reason, and kept: this used to pipe stderr and never
        // read it, so every failure here reported a bare "exited 1" with the cause discarded.
        let errors = child.stderr.take().map(|mut pipe| {
            std::thread::spawn(move || {
                let mut buf = String::new();
                let _ = pipe.read_to_string(&mut buf);
                buf
            })
        });
        // Written on its own thread, and that is not symmetry with the stderr drain — it is the one
        // way this call has a deadline at all. A pipe holds ~64KB; past that the write blocks
        // until the guest reads, and the guest is `cat` in a sandbox that may be exactly the thing
        // that has stopped answering. The deadline below starts *after* this returns, so a blocking
        // write was an unbounded wait no timeout covered — with an `sbx exec` held open for its
        // whole duration. Every install skein does goes through here, so a sandbox that went quiet
        // took the caller with it.
        let mut pipe = child.stdin.take().ok_or("sbx exec: no stdin")?;
        let wanted = body.len();
        let body = body.to_vec();
        // How many bytes the pipe has ACCEPTED, published as they are accepted — and the reason
        // there is a counter here at all rather than a `write_all` (SKEIN-944). The deadline arm
        // below has to say which half stalled, and it used to ask `writing.is_finished()`. That
        // question is asked *after* `end_group`, and `end_group` is what kills the guest — which
        // closes the read end, which releases a writer blocked on a full pipe, which finishes the
        // thread. So the kill created the answer the message then reported: on a loaded box the
        // main thread could be descheduled between the two, the writer woke with `EPIPE` first,
        // and a body that had never left the pipe buffer was reported as "sent, so the box has
        // it". Measured at 11 failures in 40 runs with 16 busy loops on 11 CPUs, and a probe on
        // either side of `end_group` read `false` before it in all 40 and `true` after it in 7.
        //
        // A byte count cannot race that way. Bytes are only added when the kernel has taken them,
        // nothing can be accepted once the read end is gone, and — the second thing
        // `is_finished()` got wrong — a write that FAILED also finishes its thread, so a body that
        // died with `EPIPE` at byte zero read as fully sent too.
        //
        // **In chunks, and the chunk size is the whole reason the count is worth reading.** A
        // blocking pipe write does not return when the buffer fills — the kernel holds the call
        // until every requested byte has landed — so handing the write the whole body reports 0
        // until it reports all of it, which is the same single bit `is_finished()` gave. Measured:
        // against a guest that never reads, a one-call 1 MB write sat on `0 of 1048576 bytes
        // accepted` with a quarter of a megabyte provably in the pipe. A chunk smaller than any
        // pipe's buffer turns that into progress, so the refusal can tell a guest that stopped
        // reading immediately from one that read most of the body and then stopped.
        const CHUNK: usize = 8 * 1024;
        let accepted = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counting = std::sync::Arc::clone(&accepted);
        let writing = std::thread::spawn(move || {
            let mut at = 0;
            while at < body.len() {
                match pipe.write(&body[at..body.len().min(at + CHUNK)]) {
                    // What `write_all` calls `WriteZero`, spelled out: the pipe stopped taking
                    // bytes without saying why, and looping on it would spin for ever.
                    Ok(0) => {
                        return Err("sbx exec: writing stdin: the pipe accepted nothing".into())
                    }
                    Ok(n) => {
                        at += n;
                        counting.store(at, std::sync::atomic::Ordering::SeqCst);
                    }
                    // Exactly what `write_all` does with it: a signal interrupted the call before
                    // any bytes moved, so retry rather than fail.
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                    Err(e) => return Err(format!("sbx exec: writing stdin: {e}")),
                }
            }
            pipe.flush()
                .map_err(|e| format!("sbx exec: writing stdin: {e}"))
            // `pipe` drops here, which is the EOF the guest command is waiting for.
        });
        // Deadlined rather than a bare wait: a box that never exits would otherwise hang the
        // caller — and one of this function's callers is holding a freshly minted credential.
        let deadline = std::time::Instant::now() + timeout;
        loop {
            match child.try_wait().map_err(|e| e.to_string())? {
                Some(status) if status.success() => {
                    // Joined only once the child is gone, so this cannot be the thing that blocks:
                    // a writer still stuck on a full pipe is released by the child's exit closing
                    // the read end.
                    return match writing.join() {
                        Ok(Err(e)) => Err(e),
                        _ => Ok(()),
                    };
                }
                Some(status) => {
                    let detail = errors
                        .and_then(|h| h.join().ok())
                        .unwrap_or_default()
                        .trim()
                        .to_string();
                    return Err(if detail.is_empty() {
                        format!("sbx exec exited {status}")
                    } else {
                        detail
                    });
                }
                None if std::time::Instant::now() >= deadline => {
                    // The GROUP killed AND reaped. This comment used to say "killed and reaped",
                    // which was true of the direct child and of nothing it had started — see the
                    // spawn above. A kill without a wait also leaves a zombie per timed-out write,
                    // and this is the path a struggling fleet takes over and over, so `end_group`
                    // does both.
                    crate::util::end_group(&mut child);
                    // Read from the counter above rather than from `writing.is_finished()`, and
                    // the byte figures are in the message because a reader who is told which half
                    // stalled deserves the number that says so (SKEIN-944).
                    let sent = accepted.load(std::sync::atomic::Ordering::SeqCst);
                    return Err(format!(
                        "sbx exec did not finish within {}s — the body was {} ({sent} of {wanted} \
                         bytes accepted)",
                        timeout.as_secs(),
                        match sent == wanted {
                            true => "sent, so the box has it and did not finish with it",
                            // The distinction worth having: a guest that never drained the pipe is
                            // a sandbox that has stopped, not a script that is slow.
                            false => "still being sent, so nothing in the box read it",
                        }
                    ));
                }
                None => std::thread::sleep(Duration::from_millis(50)),
            }
        }
    }
}

/// A grandchild that outlives the process skein holds a handle on, and the two questions to ask
/// about it.
///
/// **Shared by the four sites SKEIN-916 names**, and shared rather than copied because what it
/// encodes is one rule: a deadline has to end the WORK, and the only way to see the difference is
/// to watch something that is NOT the direct child. `src/takeover.rs`, `src/github.rs` and the
/// `write` above all reach it from their own test modules. `src/bin/skein-server.rs` carries its
/// own copy and says so, because a `#[cfg(test)]` item in this crate's library is not visible from
/// a binary target — that is a process boundary, not an oversight.
///
/// **Why a fractional `sleep` and not a marker file.** The token IS the argument, so it is in the
/// grandchild's `/proc/<pid>/cmdline` and nowhere else on the machine: "is it still running" is
/// then a question about that process rather than about a pid, which a fast machine could have
/// recycled between the two readings. It is also what keeps a neighbouring suite's `sleep` from
/// answering for this one's.
#[cfg(test)]
pub(crate) mod grouptest {
    use std::path::{Path, PathBuf};

    /// Somewhere between one and two of these per test. This process's pid is in the token as well
    /// as the counter, so two test BINARIES running at once cannot mint the same one — which is
    /// what makes the `/proc` scan below a scan for THIS fixture's processes and not a pattern
    /// over a name a neighbour might share.
    static MINTED: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

    pub(crate) struct Escapee {
        token: String,
        pidfile: PathBuf,
    }

    /// A grandchild to come, named after `label` and this process.
    pub(crate) fn escapee(dir: &Path, label: &str) -> Escapee {
        let n = MINTED.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Escapee {
            // A duration, because it is passed to `sleep` — and an unlikely one, because it is
            // also the name being searched for.
            token: format!("600.{}{n:03}", std::process::id()),
            pidfile: dir.join(format!("{label}.grandchild")),
        }
    }

    impl Escapee {
        /// A script that starts the grandchild, records its pid, and then does not finish.
        ///
        /// The background `sleep` is a child of the shell, which is itself the child skein spawned
        /// — so it is exactly the process a `kill` on the recorded pid does not reach. `/bin/sh` on
        /// this box is dash, which FORKS a `-c` command rather than exec'ing it, so a production
        /// site's real work sits where this `sleep` sits.
        ///
        /// **The shell's own wait carries the token too**, and that is not decoration: dash forks
        /// for it as well, so a `kill` on the recorded pid leaves TWO processes behind. Naming both
        /// is what lets [`Self::gone`] ask whether the group ended rather than whether one pid did,
        /// and what lets the drop below take everything this fixture started with it.
        pub(crate) fn script(&self) -> String {
            format!(
                "sleep {} & echo $! > {}; sleep {}",
                self.token,
                self.pidfile.display(),
                self.token
            )
        }

        /// The argv a test seam or a `$PATH` stand-in hands back for [`Self::script`].
        pub(crate) fn argv(&self) -> Vec<String> {
            vec!["/bin/sh".into(), "-c".into(), self.script()]
        }

        /// **It is THERE**: the pid of the running grandchild, or a panic naming what was looked
        /// for.
        ///
        /// This half is not optional. An absence that was never a presence proves nothing
        /// (SKEIN-833): without it, a stand-in that failed to start anything at all would pass the
        /// "gone" assertion below and report the fix working.
        pub(crate) fn there(&self) -> u32 {
            let until = std::time::Instant::now() + std::time::Duration::from_millis(2500);
            while std::time::Instant::now() < until {
                if let Some(pid) = self.recorded() {
                    if self.naming().contains(&(pid as libc::pid_t)) {
                        return pid;
                    }
                }
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            panic!(
                "no grandchild naming {} was running before the deadline — the command under test \
                 never started one, so the absence afterwards would prove nothing about the kill \
                 (pidfile {}: {:?})",
                self.token,
                self.pidfile.display(),
                std::fs::read_to_string(&self.pidfile).ok()
            );
        }

        /// **It is GONE**: nothing named by this fixture is running any more, `pid` included.
        ///
        /// The whole set rather than the one pid, because the defect leaves more than one process
        /// behind and a test that looked at one of them would report the other as fixed.
        pub(crate) fn gone(&self, pid: u32) {
            let until = std::time::Instant::now() + std::time::Duration::from_millis(3000);
            let mut left = Vec::new();
            while std::time::Instant::now() < until {
                left = self.naming();
                if left.is_empty() {
                    return;
                }
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            // `drop` takes them, so a red test does not also leave behind the thing it is
            // complaining about — and the panic below says what it saw before that happened.
            panic!(
                "the deadline passed and {left:?} were still running — pid {pid} is the GRANDCHILD \
                 the script recorded, and every one of these names `sleep {}`, so the kill reached \
                 the handle this side recorded and not the work it started",
                self.token
            );
        }

        /// The pid the script wrote, once it has written all of it.
        fn recorded(&self) -> Option<u32> {
            std::fs::read_to_string(&self.pidfile)
                .ok()?
                .trim()
                .parse()
                .ok()
        }

        /// Every process whose argv carries this fixture's token.
        ///
        /// Read out of `/proc/<pid>/cmdline` rather than matched against a name, so a recycled pid
        /// answers no and a neighbour's `sleep` is not this one. The environment is not read: this
        /// fixture puts its token on the argv itself, which is the surface a scan of `cmdline`
        /// can see (SKEIN-687 is the same lesson from the other direction).
        fn naming(&self) -> Vec<libc::pid_t> {
            let Ok(entries) = std::fs::read_dir("/proc") else {
                return Vec::new();
            };
            let mut found = Vec::new();
            for entry in entries.flatten() {
                let Ok(pid) = entry.file_name().to_string_lossy().parse::<libc::pid_t>() else {
                    continue;
                };
                if let Ok(raw) = std::fs::read(entry.path().join("cmdline")) {
                    if String::from_utf8_lossy(&raw).contains(&self.token) {
                        found.push(pid);
                    }
                }
            }
            found
        }
    }

    impl Drop for Escapee {
        /// Nothing this fixture started outlives it — on the panicking path as much as the
        /// returning one, which is the path that matters, because a failing deadline test is
        /// exactly the one that has something still running.
        ///
        /// Only pids whose argv carries this fixture's own token: never a pattern over a program
        /// name. A stale alternation that matches nothing is indistinguishable from a clean box by
        /// its output alone (SKEIN-647), so this derives the name it kills from the same string it
        /// spawned.
        fn drop(&mut self) {
            for pid in self.naming() {
                // SAFETY: `kill` has no memory effects, and `pid` names a process whose argv
                // carries a token minted by this process — so it is one this fixture's own script
                // started.
                unsafe { libc::kill(pid, libc::SIGKILL) };
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **A box cannot reach the execution seam**, and a shipped skein does not have one.
    ///
    /// The seam stands in for what a fleet-scope script runs, so anything that could select it from
    /// outside this process would be ISO-1 deleted and re-spelled under a new name: ISO-1 buys the
    /// property that a box cannot change what a fleet-scope script resolves to, and `~/.local/bin`
    /// is bound read-WRITE into every box on a shared uid. A hook a box could set is that property
    /// gone, with the added insult of being the mechanism that was added to make the tests safe.
    ///
    /// Asserted against the source, because both halves are properties of what the module is
    /// *allowed to contain* rather than of what it computes — the same technique as
    /// `tests/fix_lines.rs` and `neither_lifecycle_route_reaches_its_work_by_a_path_that_skips_the_check`.
    /// A behavioural test cannot cover this: it would have to enumerate the variable names nobody
    /// has thought of yet, which is the wrong quantifier. This one says "consults nothing".
    ///
    /// **What makes this fail**, and it is the obvious change somebody reaches for when a fixture
    /// is awkward to install: making the executor selectable at runtime — an `std::env::var` in the
    /// seam, a path it reads, a config key it consults. Any of those, and the first assertion
    /// fires. Dropping the `debug_assertions` gate fires the second.
    #[test]
    fn no_box_can_reach_the_execution_seam_and_a_shipped_skein_has_none() {
        let source = include_str!("place.rs");
        // The module as written, from its declaration to the one that replaces it in a release
        // build. Bounded rather than "to the end of the file" so the test module below — which
        // legitimately installs substitutions — is not what gets scanned.
        let start = source
            .find("#[cfg(debug_assertions)]\npub mod seam {")
            .expect("the seam module is gone, or no longer behind `debug_assertions`");
        // To the module's own closing brace — the first `}` at column 0 after it opens — and not
        // to the next thing that looks like a boundary. An earlier version of this ended the span
        // at the release module, and deleting that module silently widened the scan into the test
        // code below, which reads environment variables for its own reasons: the assertion still
        // fired, for entirely the wrong reason, and said so in a message about ISO-1.
        let end = source[start..]
            .find("\n}\n")
            .expect("the seam module has no closing brace at column 0")
            + start;
        let module = &source[start..end];

        // **It consults nothing.** Every way of asking the world what to do, by the spelling this
        // codebase uses for it.
        for reach in [
            "std::env::var",
            "env::var",
            "var_os",
            "read_to_string",
            "File::open",
            "load_config",
            "fleet_sandbox()",
            "PATH",
        ] {
            assert!(
                !module.contains(reach),
                "the execution seam reads `{reach}`, which makes what a fleet-scope script runs \
                 selectable from outside this process — a box writes into `~/.local/bin` on a \
                 shared uid, and ISO-1 exists because of it"
            );
        }

        // **And the release build has no seam.** Found by its own exact declaration rather than by
        // where it happens to sit, so deleting it fails here rather than widening the scan above.
        let ships = source
            .find("#[cfg(not(debug_assertions))]\npub mod seam {")
            .expect("nothing replaces the seam in a release build, so a shipped skein has one");
        let shipped = &source[ships..];
        let shipped = &shipped[..shipped
            .find("\n}\n")
            .expect("the release seam has no closing brace at column 0")];
        assert!(
            !shipped.contains("static") && !shipped.contains("Mutex"),
            "the release build carries the machinery to hold a substitution:\n{shipped}"
        );
        assert!(
            shipped.contains("None"),
            "the release build's seam does not answer `None`, so it stands in for something:\n\
             {shipped}"
        );
    }

    /// **A crossing that outruns its deadline loses the work, not only the shell in front of it.**
    ///
    /// `Place::write` is the path EVERY install skein does takes, and its timeout used to end with
    /// `child.kill()` + `child.wait()` under a comment saying "Killed AND reaped" — true of the
    /// direct child and of nothing it had started. `/bin/sh` here is dash, which FORKS a `-c`
    /// command rather than exec'ing it, and `sbx` starts work of its own besides, so the guest
    /// command is a grandchild by the time there is anything to kill (SKEIN-916).
    ///
    /// **Both halves, in that order.** The grandchild is asserted RUNNING while the crossing is
    /// still inside its deadline, and gone after it. Only the second is about the fix; without the
    /// first, a substitution that failed to start anything at all would pass this test and report
    /// the kill working (SKEIN-833).
    ///
    /// **What makes it fail:** removing `.process_group(0)` from the spawn in [`Place::write`].
    /// The kill then reaches the shell, the backgrounded `sleep` is reparented to init and goes on
    /// running, and `gone` fires naming the pid it can still see.
    #[test]
    fn a_crossing_that_misses_its_deadline_takes_its_grandchildren_with_it() {
        let _g = crate::testutil::env_lock();
        let dir = crate::testutil::tempdir();
        let dir = dir.as_ref() as &std::path::Path;
        // Pinned rather than set: `EnvPins` puts them back from `Drop`, so a failing assertion
        // below does not leave them pointing at a `TempDir` the next test will not find.
        let mut pins = crate::testutil::env_pins();
        pins.set("SKEIN_FLEET_ROOT", dir.join("fleet"));
        pins.set("SKEIN_HOME", dir.join("home"));

        let escapee = grouptest::escapee(dir, "place-write");
        let argv = escapee.argv();
        let _stood_in = seam::install(Box::new(move |_argv: &[String]| Some(argv.clone())));

        // On a thread, because the assertion that matters first is about the world WHILE the call
        // is still inside its deadline.
        let here = own_sandbox("skein-fleet");
        let crossing = std::thread::spawn(move || {
            here.write("cat > /dev/null", b"a body", Duration::from_secs(4))
        });

        let pid = escapee.there();
        let outcome = crossing.join().expect("the crossing thread panicked");
        let said = outcome.expect_err("a script that sleeps for 9999s came back inside 4s");
        assert!(
            said.contains("did not finish within"),
            "the deadline is not what ended this, so what follows is not about the deadline: {said}"
        );
        escapee.gone(pid);
    }

    /// The seam actually stands in — otherwise the two assertions above guard nothing.
    ///
    /// Paired with the test above deliberately: "nothing can reach it" is cheap to satisfy by
    /// having it not work at all, and a guard on a mechanism that does nothing is the shape of the
    /// tests this repo has been bitten by. This one drives a real fleet-scope `exec` through the
    /// substitution and reads back what the substitute printed.
    #[test]
    fn the_seam_stands_in_for_what_a_fleet_scope_command_would_have_run() {
        let _g = crate::testutil::env_lock();
        let dir = crate::testutil::tempdir();
        let dir = dir.as_ref() as &std::path::Path;
        std::env::set_var("SKEIN_FLEET_ROOT", dir.join("fleet"));
        std::env::set_var("SKEIN_HOME", dir.join("home"));

        let here = own_sandbox("skein-fleet");
        let installed = seam::install(Box::new(|_argv: &[String]| {
            Some(vec![
                "sh".to_string(),
                "-c".into(),
                "printf %s the-substitute".into(),
            ])
        }));
        let stood_in = here
            .exec("echo the-real-thing", std::time::Duration::from_secs(20))
            .expect("the substitute did not run");
        assert_eq!(stood_in, "the-substitute", "the seam did not stand in");
        drop(installed);

        // And it is gone again once the guard is dropped, which is what stops one test's
        // substitution from being the next test's world.
        //
        // **Asked of the seam rather than by running the command again**, which is what this used
        // to do: it ran `echo the-real-thing` with nothing installed, before and after, and
        // compared the two. Both of those are now refused — `Place::spawning` will not run a
        // fleet-scope command for real in a test process (SKEIN-530) — and refusing them is
        // correct, because "it runs for real when nothing stands in" is a property of PRODUCTION
        // and the two spawns only ever demonstrated it here by being harmless. What the drop has
        // to leave behind is an empty seam, and that is asked directly, in one assertion that
        // cannot pass for having run something innocuous.
        assert!(
            !seam::installed() && seam::taken(&["sh".to_string()]).is_none(),
            "dropping the guard left the substitution in place"
        );

        std::env::remove_var("SKEIN_FLEET_ROOT");
        std::env::remove_var("SKEIN_HOME");
    }

    use crate::testutil::*;

    /// The `(N of M bytes accepted)` a deadline refusal from [`Place::write`] carries, as numbers.
    ///
    /// Written as a parse rather than as a `contains("65536 of 1048576")`, because the claim the
    /// test is making is a RELATION between the two figures and not either figure: a literal would
    /// be asserting this machine's pipe capacity, which is a property of the kernel it happens to
    /// be running on. It panics rather than returning an `Option` so that a refusal which stopped
    /// carrying the numbers fails here, naming the message, instead of silently satisfying a
    /// comparison of two zeroes.
    fn accepted_of(why: &str) -> (usize, usize) {
        let figures = why
            .rsplit_once('(')
            .and_then(|(_, tail)| tail.strip_suffix(" bytes accepted)"))
            .and_then(|figures| figures.split_once(" of "))
            .unwrap_or_else(|| panic!("the refusal carries no byte count: {why}"));
        let read = |n: &str| {
            n.parse()
                .unwrap_or_else(|e| panic!("{n:?} is not a byte count ({e}): {why}"))
        };
        (read(figures.0), read(figures.1))
    }

    /// A guest that never reads its stdin must time out, not hang for ever.
    ///
    /// A pipe holds a bounded buffer — 64 KiB on a stock kernel, 262144 bytes on the box this
    /// sentence was measured on. Past that the write blocks until something on the other end reads,
    /// and the deadline in `Place::write` only started *after* the write returned — so a body larger
    /// than the pipe, sent to a sandbox that had stopped answering, was an unbounded wait that no
    /// timeout covered, holding an `sbx exec` open for its whole duration. Every install skein does
    /// goes through this call, including the ones at server start.
    ///
    /// Stood in for through the seam, with a body far larger than the buffer.
    ///
    /// It used to put a `sleep 60` on `$PATH` as `sbx`. There is no `sbx` hop to intercept now, so
    /// that fake was bypassed and the write ran for real — against this machine (SKEIN-592). The
    /// seam is the sanctioned way for a test to say what a fleet-scope command runs, and nothing
    /// outside this process can select it.
    ///
    /// **It also reads the byte count out of the refusal, and that is the part that used to be a
    /// race** (SKEIN-944, and SKEIN-984/SKEIN-985 are the same failure seen twice more). The
    /// refusal named its half from `writing.is_finished()`, read after `end_group` — and
    /// `end_group` kills the guest, which closes the read end, which finishes the writer. So the
    /// kill manufactured the answer, and on a loaded box this test failed with `the body was sent`
    /// against a guest that provably never read a byte. It reproduced at 11 failures in 40 runs
    /// under 16 busy loops on 11 CPUs. Nothing about the test's own clock was wrong: its only
    /// wall-clock assertion allows 20s for a 2s deadline, and the 2s belongs to the test, while
    /// every production caller of `write` passes 30s or 60s (`src/fleet.rs`, `src/sandbox.rs`).
    #[test]
    fn a_write_to_a_box_that_never_reads_it_gives_up_instead_of_hanging() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        std::env::set_var("SKEIN_HOME", home.as_ref() as &std::path::Path);

        // Never reads stdin, never exits on its own: the box that has gone quiet.
        let _stood_in = seam::install(Box::new(|_argv: &[String]| {
            Some(vec!["sh".to_string(), "-c".into(), "sleep 60".into()])
        }));

        let place = crate::place::own_sandbox("skein-fleet");
        // Bigger than any pipe this test could be handed, and the size is chosen against a
        // MEASURED ceiling rather than a remembered one. It read `1 << 20` with the comment
        // "sixteen times the pipe", which assumed the 64 KiB a stock kernel gives: on this box
        // `F_GETPIPE_SZ` answers 262144, so it was four times, and
        // `/proc/sys/fs/pipe-max-size` is 1048576 — the body's exact size. A kernel that gave a
        // pipe its own maximum would have swallowed the whole body, and the refusal would have
        // been right to say it was sent while this test called that a bug.
        let body = vec![b'x'; 4 << 20];
        let started = std::time::Instant::now();
        let why = place
            .write("cat > /tmp/x", &body, Duration::from_secs(2))
            .expect_err("a guest that never reads must not succeed");
        let spent = started.elapsed();

        assert!(
            spent < Duration::from_secs(20),
            "the write hung past its own deadline ({spent:?}) — this is the shape that took the \
             whole server with it"
        );
        assert!(why.contains("did not finish"), "{why}");
        // And it says which half stalled, because they are different faults: a body that was sent
        // means the box has it and is slow, one still being sent means nothing read it at all.
        assert!(why.contains("nothing in the box read it"), "{why}");

        // And the half it names is DERIVED, which is what stops this being a coin toss on a busy
        // box. The refusal carries the two numbers it decided from, and the only thing that can
        // put a figure here strictly between nothing and the whole body is a pipe that took a
        // bufferful and then stopped — which is the guest not reading. A classification taken
        // from the writer thread's liveness could not produce this line at all.
        let (sent, wanted) = accepted_of(&why);
        assert_eq!(
            wanted,
            body.len(),
            "the refusal misreports the body size: {why}"
        );
        assert!(
            0 < sent && sent < wanted,
            "the refusal claims {sent} of {wanted} bytes reached a guest that never read: {why}"
        );

        std::env::remove_var("SKEIN_HOME");
    }

    // The connection is *held*, and that is the entire transport rather than an optimisation: the
    // stall this exists to survive blocks new channels into the sandbox while established ones keep
    // flowing, so a client that reconnected per call would reproduce the failure it was built to
    // avoid — and only under load, where it would look like the agent had made no difference.
    //
    // It reconnects silently on a dead socket, which is why this has to be asserted from the other
    // end: the agent defaulted to HTTP/1.0, where `BaseHTTPRequestHandler` hangs up after every
    // response no matter what the client asks for, and every call had been paying for a fresh
    // connection with nothing to show it.
    // Everything skein installs in a box goes through `write`, so leaving it on `sbx exec` meant
    // starting a box still needed the daemon two or three times however healthy the transport was.
    // The agent is installed *into* the sandbox and outlives the skein that installed it, so a
    // running agent may be older than the host talking to it. `/write` has to be declined before
    // the body is sent, because an upload's bytes come off a network socket that has already been
    // drained — there is no second copy to fall back with.
    // A write that fails used to report `sbx exec exited 1` and drop the reason on the floor, which
    // is how "mkdir: cannot create directory '/boxes': Permission denied" reached nobody.
    #[test]
    fn a_failed_write_reports_what_the_sandbox_said() {
        let _g = env_lock();
        let home = tempdir();
        // A home of its own, so `unreachable_from_fleet` reads a config this test wrote rather than
        // whatever a neighbour left behind. Bound after `home`, so the pin goes back before the
        // directory it names is removed.
        let mut env = env_pins();
        env.set("SKEIN_HOME", home.as_ref() as &std::path::Path);
        // **Stood in for through the seam, not through `$PATH`.** This used to put a failing
        // `sbx` on PATH; there is no `sbx` hop to intercept now, so the fake was bypassed and the
        // command ran for real (SKEIN-592). The seam is the sanctioned way for a test to say what
        // a fleet-scope command runs, and nothing outside this process can select it.
        let _stood_in = seam::install(Box::new(|_argv: &[String]| {
            Some(vec![
                "sh".to_string(),
                "-c".into(),
                "cat >/dev/null; echo 'mkdir: cannot create directory' >&2; exit 1".into(),
            ])
        }));

        let place = Place {
            name: "b".into(),
            sandbox: "fleet".into(),
            at: Where::SandboxItself,
        };
        let err = place
            .write("cat > /boxes/x", b"body", Duration::from_secs(10))
            .unwrap_err();
        assert!(err.contains("cannot create directory"), "{err}");
    }

    /// In-fleet, the sandbox skein is STANDING IN is reached by running the command; any other
    /// sandbox is refused.
    ///
    /// That distinction is the whole of [`Place::unreachable_from_fleet`], and both arms are
    /// asserted in one test because a version that simply ran everything locally would pass the
    /// first and be exactly the bug the refusal was written to prevent — a command aimed at another
    /// sandbox, executed against this one's files at the same paths.
    ///
    /// The refusal caught the fleet's own sandbox once, and that is the reason for the first arm:
    /// most of [`crate::fleet`] addresses it through [`own_sandbox`] (`ensure_substrate`,
    /// `ensure_fleet_root`, `install_launcher`, `install_docker_config`), so every box start
    /// in-fleet printed
    ///
    /// ```text
    /// skein: skein-fleet is not the sandbox this skein is running inside …
    /// ```
    ///
    /// and provisioned nothing.
    #[test]
    fn in_fleet_runs_in_the_sandbox_it_stands_in_and_refuses_every_other() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        std::env::set_var("SKEIN_HOME", home.as_ref() as &std::path::Path);
        std::fs::write(
            home.join("config.json"),
            r#"{"fleet_sandbox":"skein-fleet"}"#,
        )
        .unwrap();

        let ours = Place {
            name: "skein-fleet".into(),
            sandbox: "skein-fleet".into(),
            at: Where::SandboxItself,
        };
        assert_eq!(
            ours.exec_argv("echo hi"),
            [
                "env",
                "PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin",
                "bash",
                "-c",
                "echo hi"
            ],
            "skein refused to run a command in the sandbox it is standing in, which is where its \
             own substrate, launcher and fleet root are installed"
        );

        // The case the refusal was written for: any OTHER sandbox — a second fleet, someone's own
        // sbx box, or one of skein's original per-box VMs — really is a different machine, and
        // there is no sbx in here to reach it with.
        let elsewhere = Place {
            name: "web-main".into(),
            sandbox: "another-fleet".into(),
            at: Where::SandboxItself,
        };
        let argv = elsewhere.exec_argv("echo hi");
        assert_eq!(argv.first().map(String::as_str), Some("sh"), "{argv:?}");
        assert!(
            argv.iter()
                .any(|a| a.contains("is not the sandbox this skein is running inside")),
            "a sandbox other than this one is now addressed rather than refused, so the command \
             runs in the sandbox skein is standing in — a different machine with the same paths \
             and other people's files at them: {argv:?}"
        );

        std::env::remove_var("SKEIN_HOME");
    }

    // The argv IS the contract. The fleet's own sandbox is the address skein provisions itself
    // through, and pinning all three spellings here is what makes a change to the crossing
    // reviewable in one place rather than as a diff across a dozen files.
    //
    // It used to open with `sbx exec skein-fleet` — the host's hop into the guest — and every
    // spelling below carried it. Skein runs inside that sandbox now (SKEIN-576), so there is no
    // hop and the argv starts at the shell. What is pinned is what was always the interesting
    // half: **`env PATH=… bash -c`, never `bash -lc`**. This address is fleet scope, outside every
    // box's mount namespace, and a login shell would source a profile that puts the box-writable
    // `~/.local/bin` at the head of PATH (ISO-1 — see `Place::shell`, and `tests/isolation_bwrap.rs`,
    // which runs a planted binary against it). That property did not depend on the hop, and it is
    // the one somebody could lose without noticing.
    #[test]
    fn a_whole_sandbox_is_addressed_by_the_shell_alone() {
        // `exec_argv` reads the config (via `unreachable_from_fleet`, which asks which sandbox this
        // process is standing in), and the neighbours that write `$SKEIN_HOME` do it under the
        // shared lock — reading it without that lock is how this test flaked.
        let _g = crate::testutil::env_lock();
        // Pinned, because what this exercises resolves `config::skein_home`, which refuses an
        // unpinned test rather than answering with the real `~/.skein` (SKEIN-626).
        let skein_home = crate::testutil::tempdir();
        std::env::set_var("SKEIN_HOME", &skein_home);
        let p = Place {
            name: "skein-fleet".into(),
            sandbox: "skein-fleet".into(),
            at: Where::SandboxItself,
        };
        assert_eq!(
            p.exec_argv("echo hi"),
            [
                "env",
                "PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin",
                "bash",
                "-c",
                "echo hi"
            ],
            "no hop, no nsenter, no wrapper — and no login shell"
        );
        // A write is the same argv: the body arrives on the stdin of the process `Place::write`
        // spawns, rather than through an `-i` on a hop that no longer exists.
        assert_eq!(
            p.write_argv("cat > f"),
            [
                "env",
                "PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin",
                "bash",
                "-c",
                "cat > f"
            ]
        );
        assert_eq!(
            p.raw_argv(&["cat", "/tmp/x"]),
            ["cat", "/tmp/x"],
            "no shell for a streamed copy — the path is an argv element, not a word to split"
        );
        std::env::remove_var("SKEIN_HOME");
    }

    // The three details that are easy to get wrong and all look like permissions bugs: the user
    // and mount namespaces must be joined TOGETHER (mount alone is refused), credentials must be
    // preserved (or setgroups fails unprivileged), and HOME/cwd/SKEIN_BOX must be set explicitly
    // because nsenter carries the caller's environment, not the box's.
    fn shared(generation: &str, ns_start: u64) -> Place {
        Place {
            name: "web-main".into(),
            sandbox: "skein-fleet".into(),
            at: Where::Shared {
                ns_pid: 4242,
                home: "/boxes/web-main/home".into(),
                tree: "/boxes/web-main/tree".into(),
                sock: "/boxes/web-main/session.sock".into(),
                generation: generation.into(),
                ns_start,
            },
        }
    }

    #[test]
    fn a_shared_sandbox_is_entered_by_namespace_with_the_boxs_own_home() {
        // `exec_argv` reads the config (via `unreachable_from_fleet`), so it resolves
        // `config::skein_home`, which refuses an unpinned test rather than answering with the real
        // `~/.skein` (SKEIN-626). This one passed only because a neighbour in the same process had
        // left `$SKEIN_HOME` set — including the lock, without which reading it flaked (SKEIN-646).
        let _g = crate::testutil::env_lock();
        let skein_home = crate::testutil::tempdir();
        std::env::set_var("SKEIN_HOME", &skein_home);
        let p = shared("boot-a", 900);
        let argv = p.exec_argv("git status");
        // **The crossing starts at the namespace, with nothing in front of it.** This used to open
        // with `sbx exec skein-fleet`, the host's hop into the guest; skein is inside that guest
        // now (SKEIN-576). Asserted as an absence rather than by index alone, because an index
        // that shifted back would still pass a length check while the hop was back.
        assert!(
            !argv.iter().any(|a| a == "sbx"),
            "a hop into the sandbox came back, and there is no sbx here to run it: {argv:?}"
        );
        // **The PATH is pinned before anything runs** (ISO-1, SKEIN-832). The outer `bash`, the
        // `nsenter` and the guard's `cat`/`sed`/`cut` all run at fleet scope, before the hop, and
        // unpinned they resolved from the PATH of whoever spawned this — a person's shell, for
        // `skein attach`. Asserted by value rather than by presence: an `env` with some other
        // PATH in it would satisfy a `contains("env")`.
        assert_eq!(&argv[..2], ["env", &format!("PATH={FLEET_PATH}")]);
        // A shell, because the anchor check has to run in the process that crosses. The caller's
        // argv rides in as `"$@"`, so nothing between here and `nsenter` re-quotes it.
        assert_eq!(&argv[2..4], ["bash", "-c"]);
        // The far side of the hop, by value: the box's HOME, its name, **its PATH**, and its tree.
        // The PATH's directories are spelled out rather than built from the constants, so that
        // changing either constant has to be a deliberate edit to a string a reader can compare
        // against a real box's environment — which is where it came from (see `BOX_PATH_HEAD`).
        //
        // The box's home is the one part held in a variable, and only because `residue-check` reads
        // the literal that would otherwise appear here as somebody's home directory. The assertion
        // is unchanged by that: `box_home` is a constant of this test, not a value from the code
        // under test.
        let box_home = "/boxes/web-main/home";
        assert_eq!(
            argv.last().unwrap(),
            &format!(
                "export HOME='{box_home}' SKEIN_BOX='web-main' PATH='{box_home}/.local/bin:\
                 /usr/local/share/npm-global/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:\
                 /usr/bin:/sbin:/bin' && cd '/boxes/web-main/tree' && git status"
            )
        );
        // Stated as a property as well as a value, because the value above passes for a PATH that
        // merely CONTAINS the box's bin directory somewhere behind `/usr/local/bin` — which is the
        // failure this is here to stop. `~/.local/bin` must come first, and it must be THIS box's.
        let wrapped = argv.last().unwrap();
        let path = wrapped
            .split("PATH='")
            .nth(1)
            .and_then(|rest| rest.split('\'').next())
            .expect("the wrapper exports a PATH");
        assert!(
            path.starts_with(&format!("{box_home}/.local/bin:")),
            "the box's own `~/.local/bin` leads its PATH, or `claude` resolves to the substrate's \
             copy instead of the fleet's: {path}"
        );
        assert!(
            path.ends_with(FLEET_PATH),
            "and the root-owned directories are still behind it: {path}"
        );
        assert_eq!(&argv[5..8], ["bash", "bash", "-lc"]);

        let crossing = &argv[4];
        // The order is the property: refuse, THEN cross. Reversed, the check is a log line.
        let checked = crossing
            .find("skein_start=")
            .expect("the anchor is re-read");
        let entered = crossing.find("exec nsenter").expect("and then entered");
        assert!(
            checked < entered,
            "the check must precede the crossing: {crossing}"
        );
        assert!(
            crossing.contains("!= 'boot-a'") && crossing.contains("!= '900'"),
            "both halves of the identity are compared: {crossing}"
        );
        assert!(
            crossing.contains("--user=/proc/4242/ns/user")
                && crossing.contains("--mount=/proc/4242/ns/mnt")
                && crossing.contains("--preserve-credentials"),
            "both namespaces together, credentials preserved: {crossing}"
        );

        // The stdin path crosses the same way. It used to carry an `-i` in front of the sandbox so
        // `sbx exec` would wire a pipe; the pipe is `Place::write`'s own now, and what still has to
        // be true is that the body lands inside the box's namespace rather than the sandbox's.
        let w = p.write_argv("cat > f");
        assert_eq!(
            &w[..4],
            ["env", &format!("PATH={FLEET_PATH}"), "bash", "-c"]
        );
        assert!(w.iter().any(|a| a.contains("--preserve-credentials")));
        // And a streamed copy enters the namespace too, or it would `cat` the wrong /tmp entirely.
        let raw = p.raw_argv(&["cat", "/tmp/artifact"]);
        assert_eq!(&raw[raw.len() - 2..], ["cat", "/tmp/artifact"]);
        assert!(raw.iter().any(|a| a.contains("exec nsenter")));
        std::env::remove_var("SKEIN_HOME");
    }

    /// An address skein cannot prove is refused, on every transport, rather than used.
    ///
    /// A record written before the stamp existed is the upgrade case, and the tempting thing is to
    /// let it through "just this once" — which produces a fleet where the guard is present and does
    /// nothing for every box nobody has restarted. The refusal names the fix instead.
    #[test]
    fn an_address_that_cannot_be_proved_is_refused_rather_than_entered() {
        // Pinned for the same reason as the test above: `exec_argv` resolves `config::skein_home`,
        // and unpinned that is the owner's real `~/.skein` (SKEIN-626/646).
        let _g = crate::testutil::env_lock();
        let skein_home = crate::testutil::tempdir();
        std::env::set_var("SKEIN_HOME", &skein_home);
        let p = shared("", 0);
        // Index 4, because the crossing is the third element of `bash -c <crossing> bash …` and
        // that shell is behind the PATH pin `enter` puts in front of every crossing (SKEIN-832).
        // There is still no hop in front of THAT (SKEIN-576) — the pin is two argv elements, not a
        // machine boundary.
        let argv = p.exec_argv("git status");
        assert_eq!(&argv[..2], ["env", &format!("PATH={FLEET_PATH}")]);
        let crossing = argv[4].clone();
        assert!(
            crossing.contains("exit 78") && !crossing.contains("exec nsenter"),
            "an unprovable address must not reach nsenter at all: {crossing}"
        );
        assert!(
            crossing.contains("skein restart web-main"),
            "and it names the fix: {crossing}"
        );
        std::env::remove_var("SKEIN_HOME");
    }

    // A box's tmux server is addressed by socket, never by nsenter — the socket sits outside the
    // private mounts precisely so liveness and attach work from the sandbox. A whole sandbox
    // addressed as itself has no box in it and so no per-box socket: the spelling is the bare
    // `tmux` the sandbox's own server answers on, which is what the fleet's supervisor uses.
    #[test]
    fn a_shared_box_tmux_server_is_addressed_by_its_own_socket() {
        let whole = Place {
            name: "skein-fleet".into(),
            sandbox: "skein-fleet".into(),
            at: Where::SandboxItself,
        };
        assert_eq!(whole.tmux(), "tmux");

        let shared = Place {
            name: "web-main".into(),
            sandbox: "skein-fleet".into(),
            at: Where::Shared {
                ns_pid: 4242,
                home: "/boxes/web-main/home".into(),
                tree: "/boxes/web-main/tree".into(),
                sock: "/boxes/web-main/session.sock".into(),
                generation: "boot-a".into(),
                ns_start: 900,
            },
        };
        assert_eq!(shared.tmux(), "tmux -S '/boxes/web-main/session.sock'");
        assert!(
            !shared.tmux().contains("nsenter"),
            "the server answers from the sandbox; entering its namespace to talk to it would be \
             both unnecessary and wrong — the socket does not exist inside the private /tmp"
        );
    }

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

    /// The refusal to enter says WHICH of the two things it measured went wrong.
    ///
    /// Reported live, about the box its owner was working in — its name stood in for here:
    ///
    /// ```text
    /// skein: example-work is gone — pid 625094 is no longer the session skein recorded,
    /// so entering it would be entering some other box
    /// ```
    ///
    /// The refusal itself is correct and is the whole point of the anchor — a stale pid is ANOTHER
    /// box, and a wrong address is not a degraded address. But the guard checks two facts and that
    /// sentence named neither. A boot id that has moved means **the sandbox restarted**: nothing
    /// that was running in it survived, every box is in the same state, and it is a fleet-wide fact
    /// arriving one box at a time. A start time that has moved means **one pid was reused**, about
    /// that box alone. A person cannot act on "is gone" without knowing which.
    ///
    /// Run rather than read, and against a process that is really there: the start time is cut out
    /// of `/proc/<pid>/stat` by a `sed`/`cut` pair, and the only way to know the guard agrees with
    /// what stamped the record is to point both at the same live pid.
    ///
    /// Linux only: the guard's whole subject is `/proc/<pid>/stat` and the kernel's boot id.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_box_that_cannot_be_entered_says_which_proof_failed() {
        let stat = std::fs::read_to_string("/proc/self/stat").expect("a linux /proc");
        let start: u64 = stat
            .rsplit_once(") ")
            .expect("a stat line")
            .1
            .split_whitespace()
            .nth(19)
            .and_then(|f| f.parse().ok())
            .expect("field 22 of /proc/self/stat");
        let boot = std::fs::read_to_string("/proc/sys/kernel/random/boot_id")
            .expect("a boot id")
            .trim()
            .to_string();

        let placed = |generation: &str, ns_start: u64| Place {
            name: "web-main".into(),
            sandbox: "skein-fleet".into(),
            at: Where::Shared {
                ns_pid: std::process::id(),
                home: "/home/agent".into(),
                tree: "/boxes/web-main/tree".into(),
                sock: "/tmp/skein-web-main".into(),
                generation: generation.to_string(),
                ns_start,
            },
        };
        let run = |place: Place| {
            let out = std::process::Command::new("bash")
                .arg("-c")
                .arg(place.guard())
                .output()
                .expect("bash");
            (
                out.status.code().unwrap_or(-1),
                String::from_utf8_lossy(&out.stderr).trim().to_string(),
            )
        };

        // Both proofs hold: the guard says nothing and gets out of the way.
        let (code, said) = run(placed(&boot, start));
        assert_eq!((code, said.as_str()), (0, ""), "a live box was refused");

        // The sandbox restarted. Every box in it is in this state, so the sentence has to be about
        // the sandbox — being told "web-main is gone", one box at a time, is what sent a person
        // looking for what happened to one box.
        let (code, said) = run(placed("a-different-boot", start));
        assert_eq!(code, 78, "a restarted sandbox was entered anyway");
        assert!(
            said.contains("the sandbox has restarted") && said.contains("everything else"),
            "a sandbox restart was reported as one box going missing: {said}"
        );

        // One pid, reused. About this box and nothing else — and it must still refuse, because the
        // process at that number now is somebody else's.
        let (code, said) = run(placed(&boot, start + 1));
        assert_eq!(code, 78, "a reused pid was entered");
        assert!(
            said.contains("reused") && said.contains("web-main"),
            "a reused pid was not named as one: {said}"
        );
        assert!(
            !said.contains("the sandbox has restarted"),
            "a reused pid was reported as a sandbox restart, which would send a person to start a \
             fleet that is running: {said}"
        );
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

    /// The in-fleet crossing, entering a real namespace.
    ///
    /// bwrap and tmux are here even though `sbx` is not, which is the whole reason this is
    /// testable: a box is a bwrap mount namespace anchored by a pid, and `nsenter` into one is the
    /// same call whether skein reached the sandbox first or was already in it. What differs is only
    /// the hop before it, which is what `Place::reach` decides.
    ///
    /// The proof is the namespace itself. Running `readlink /proc/self/ns/mnt` through the crossing
    /// and comparing it with the *test process's* is the one assertion that cannot pass by
    /// accident: an argv that failed to enter reports this process's namespace, and an argv that
    /// entered reports the box's.
    #[test]
    fn a_crossing_in_the_fleet_enters_the_box_without_sbx() {
        if !crate::testutil::bwrap_works() {
            crate::testutil::skip(
                "bwrap cannot make a namespace here, so there is none to cross into",
            );
            return;
        }
        let _g = crate::testutil::env_lock();
        let dir = crate::testutil::tempdir();
        // Pinned, because `config::skein_home` refuses an unpinned test rather than answering
        // with the real `~/.skein` — where this fixture's state would otherwise land (SKEIN-626).
        std::env::set_var("SKEIN_HOME", &dir);
        let anchor_at = dir.join("anchor");
        let bwrap_err = dir.join("bwrap.err");
        // **Into the guard at the spawn, not at the end.** Everything below unwinds past a plain
        // teardown — four assertions, an `expect`, and the `panic!` in the anchor wait a few lines
        // down, which is itself a panic that would strand the bwrap it is complaining about
        // (SKEIN-1008). `BoxlikeNamespace` kills the anchor and then the bwrap from its `Drop`, so
        // the failing run leaves as little behind as the passing one.
        //
        // There is **no `--unshare-pid`**, which is what makes the anchor a separate thing to kill:
        // it is an ordinary process in this pid namespace, and `bash -c` execs the last command of
        // its string, so the anchor pid IS the `sleep 60`. Killing the bwrap reaches the bwrap and
        // nothing else, and bwrap is what was WAITING on the anchor — so bwrap first reparents the
        // sleep to pid 1 to run out its full minute (SKEIN-1005, which is SKEIN-861/892 in the Rust
        // tier long after `tests/ui/lift.mjs` fixed the same two lines).
        let mut boxlike = crate::testutil::BoxlikeNamespace::holding(
            std::process::Command::new("bwrap")
                .args(["--dev-bind", "/", "/", "--"])
                .arg("bash")
                .arg("-c")
                .arg(format!("echo $$ > {}; sleep 60", anchor_at.display()))
                // stdout nulled: a child that outlives this holds an inherited pipe open, and
                // `cargo test` then looks like a hang long after the test finished. stderr goes to
                // a FILE rather than to `/dev/null` for the same reason inverted — a file holds no
                // pipe open, so it costs nothing here and it is the only place bwrap's own refusal
                // is recorded. Nulling it is why 179KB of CI log never said `apparmor` or `userns`.
                .stdout(std::process::Stdio::null())
                .stderr(std::fs::File::create(&bwrap_err).expect("a file for bwrap's stderr"))
                .spawn()
                .expect("start a box-like namespace"),
        );
        let anchor: u32 = {
            let mut found = None;
            for _ in 0..100 {
                if let Ok(text) = std::fs::read_to_string(&anchor_at) {
                    if let Ok(pid) = text.trim().parse() {
                        found = Some(pid);
                        break;
                    }
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            found.unwrap_or_else(|| {
                let said = std::fs::read_to_string(&bwrap_err).unwrap_or_default();
                panic!(
                    "the box-like namespace never reported its anchor; bwrap said: {}",
                    said.trim()
                )
            })
        };

        // **The first statement after the pid is known**, so there is no window at all in which
        // this test has an anchor the guard has not been told about. The guard hands back the stamp
        // it recorded, so the number this placement record is addressed by IS the number compared
        // against before anything is signalled: read twice it could differ twice, read once it
        // cannot.
        let ns_start: u64 = boxlike.inside(anchor);

        let boot = std::fs::read_to_string("/proc/sys/kernel/random/boot_id").unwrap_or_default();
        let place = Place {
            name: "demo".into(),
            sandbox: "skein-fleet".into(),
            at: Where::Shared {
                ns_pid: anchor,
                home: std::env::var("HOME").unwrap_or_else(|_| "/root".into()),
                tree: "/".into(),
                sock: dir.join("session.sock").to_string_lossy().into_owned(),
                generation: boot.trim().to_string(),
                ns_start,
            },
        };
        let mine = std::fs::read_link("/proc/self/ns/mnt")
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default();
        let theirs = std::fs::read_link(format!("/proc/{anchor}/ns/mnt"))
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default();
        assert_ne!(mine, theirs, "the fixture is not in a namespace of its own");

        // **No first hop at all**, and the crossing still lands inside the box. The half that
        // used to sit above this asserted the `sbx exec` prefix, calling it "the fallback that
        // makes 4c revertible" — 4c is not revertible now (SKEIN-576), so that assertion was
        // about a decision rather than a behaviour, and it went with the decision.
        let argv = place.exec_argv("readlink /proc/self/ns/mnt");
        assert!(
            !argv.iter().any(|a| a == "sbx"),
            "the in-fleet crossing still spells sbx, which does not exist here: {argv:?}"
        );
        let out = std::process::Command::new(&argv[0])
            .args(&argv[1..])
            .output()
            .expect("run the in-fleet crossing");
        let said = String::from_utf8_lossy(&out.stdout).trim().to_string();
        assert_eq!(
            said,
            theirs,
            "the crossing did not enter the box (stderr: {})",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_ne!(said, mine, "the command ran here rather than in the box");

        // **No teardown here, and that is the point.** SKEIN-1005 put the kill at the bottom of
        // this body — anchor first and bwrap second, which is the right order and was the whole of
        // its finding — but four assertions and an `expect` stand between it and the spawn, so the
        // run that failed was exactly the run that leaked. It is `crate::testutil::BoxlikeNamespace`
        // now, and `boxlike` is dropped by every way out of this function (SKEIN-1008).
        std::env::remove_var("SKEIN_HOME");
    }

    /// The guard still spends the anchor, and dropping the `sbx` hop did not drop it with it.
    ///
    /// `reach` only ever decided what ran *before* the crossing, so the shell is the same one it
    /// always was — but that is a claim worth a test rather than a reading, because the whole of
    /// the refusal lives in an argv that a change to argv-building can quietly stop producing.
    ///
    /// It used to run the loop below twice, once per deployment. There is one (SKEIN-576), and the
    /// property was never about the hop: an address that cannot be proved must not reach `nsenter`,
    /// whatever is or is not in front of it.
    #[test]
    fn dropping_the_sbx_hop_does_not_drop_the_anchor_check() {
        let _g = crate::testutil::env_lock();
        // Pinned, because what this exercises resolves `config::skein_home`, which refuses an
        // unpinned test rather than answering with the real `~/.skein` (SKEIN-626).
        let skein_home = crate::testutil::tempdir();
        std::env::set_var("SKEIN_HOME", &skein_home);
        let unprovable = Place {
            name: "demo".into(),
            sandbox: "skein-fleet".into(),
            at: Where::Shared {
                ns_pid: 4242,
                home: "/home/agent".into(),
                tree: "/boxes/demo/tree".into(),
                sock: "/boxes/demo/session.sock".into(),
                // Written before skein stamped anchors: exactly the record `provable` refuses.
                generation: String::new(),
                ns_start: 0,
            },
        };
        let argv = unprovable.exec_argv("echo reached");
        let joined = argv.join(" ");
        assert!(
            !joined.contains("nsenter"),
            "an address that cannot be proved built an nsenter anyway: {joined}"
        );
        assert!(
            joined.contains("skein restart demo"),
            "the refusal does not say what would fix it: {joined}"
        );
        std::env::remove_var("SKEIN_HOME");
    }

    /// A sandbox other than the one skein is standing in is refused, by EVERY argv builder, and
    /// says so.
    ///
    /// Found while writing `reach` rather than planned: dropping the `sbx exec` hop is right when a
    /// second hop enters a namespace, and `SandboxItself` has no second hop. Drop both and the
    /// command runs in the sandbox skein is standing in — a different machine with the same paths
    /// on it. All four builders are checked because the refusal has to be in the one place they
    /// share; three of four would be a hole with no symptom until somebody used the fourth.
    ///
    /// This used to open by asserting the *other* deployment still aimed such an address, at
    /// `sbx exec another-fleet`. That was the escape hatch — drive the other sandbox from a host —
    /// and it went with the host (SKEIN-576). What is left is the refusal, which is now the only
    /// answer rather than one of two.
    ///
    /// **What would make this fail**: dropping the `!the_one_we_are_in` guard's companion, so
    /// `unreachable_from_fleet` returns `None` for a foreign sandbox. Every builder would then
    /// produce a runnable argv, and `echo hello` would appear in the joined string.
    #[test]
    fn another_sandbox_is_not_silently_run_in_the_one_skein_stands_in() {
        let _g = crate::testutil::env_lock();
        // Pinned, because what this exercises resolves `config::skein_home`, which refuses an
        // unpinned test rather than answering with the real `~/.skein` (SKEIN-626).
        let skein_home = crate::testutil::tempdir();
        std::env::set_var("SKEIN_HOME", &skein_home);
        let elsewhere = crate::place::own_sandbox("another-fleet");
        for argv in [
            elsewhere.exec_argv("echo hello"),
            elsewhere.write_argv("cat > /tmp/x"),
            elsewhere.raw_argv(&["cat", "/etc/hostname"]),
            elsewhere.interactive_argv("bash -l"),
        ] {
            let joined = argv.join(" ");
            assert!(
                joined.contains("no sbx here") && joined.contains("exit 1"),
                "another sandbox was addressed from inside the fleet instead of refused: {joined}"
            );
            assert!(
                !joined.contains("echo hello") && !joined.contains("/etc/hostname"),
                "the refusal still carries the command it refused: {joined}"
            );
        }
        std::env::remove_var("SKEIN_HOME");
    }

    /// What a spawned crossing reported about how it was spawned.
    struct Captured {
        script: String,
        cmdline_at: PathBuf,
        stdin_at: PathBuf,
        done_at: PathBuf,
    }

    /// A fixture that reports how it was spawned: the spawned process's **own**
    /// `/proc/<pid>/cmdline`, and everything it was fed.
    ///
    /// **Shared by the two tests below deliberately, and what makes that honest is worth stating**
    /// (SKEIN-822). [`Place::attempt`] and [`Place::write`] differ in the *call*, not in the
    /// capture: [`Place::write_argv`] and [`Place::exec_argv`] have the same body — `reach()`,
    /// `enter()`, `shell()`, then `wrap(script)` as ONE element — both go through
    /// [`Place::spawning`], so both are seam-covered, and both end in a `bash -c` whose `$$` is the
    /// process the kernel lists.
    ///
    /// **What differs is everything after the argv, which is why one test cannot cover both.** The
    /// body reaches stdin by two separate implementations: `attempt` hands it to
    /// `util::output_with_timeout_fed`, `write` pipes it from a thread of its own under a deadline
    /// it starts before the write rather than after. Either could grow a payload on argv without
    /// the other changing at all.
    ///
    /// **`done` exists because [`Place::write`] nulls stdout.** `attempt` returns what the program
    /// printed and can prove it ran that way; `write` reads no stdout at all — an unread pipe
    /// blocks a chatty guest at ~64KB — so the script's last act is the only evidence left that it
    /// got past `cat`. Without it a crossing that died before the redirect would leave the same
    /// "no marker found" as a clean one.
    fn reporting_how_it_was_spawned(dir: &std::path::Path, called: &str) -> Captured {
        let cmdline_at = dir.join(format!("cmdline-the-{called}-ran-under"));
        let stdin_at = dir.join(format!("stdin-the-{called}-was-fed"));
        let done_at = dir.join(format!("the-{called}-got-to-the-end"));
        let script = format!(
            "tr '\\0' '\\n' < /proc/$$/cmdline > {cmdline}\n\
             cat > {stdin}\n\
             printf 'the crossing read it'\n\
             : > {done}\n",
            cmdline = sh_quote(&cmdline_at.display().to_string()),
            stdin = sh_quote(&stdin_at.display().to_string()),
            done = sh_quote(&done_at.display().to_string()),
        );
        Captured {
            script,
            cmdline_at,
            stdin_at,
            done_at,
        }
    }

    /// The property, asked of one crossing that really ran: the payload is on its stdin, and in
    /// neither the argv skein built nor the argv the kernel lists.
    ///
    /// **Every assertion here is preceded by a guard that fails loudly on a capture holding
    /// nothing**, because "the marker is not in it" is true of an empty string — a fixture that
    /// never ran, a `/proc` read that failed, a path nothing wrote. That is the SKEIN-647 shape,
    /// and it is the half of this that decides whether the rest is worth anything.
    ///
    /// **The guards ask `contains`, and a sabotage is what settled that.** They asked `==` first,
    /// and an equality guard is the assertion that speaks when the payload goes back into the
    /// script element: it reported "the argv does not carry the script" about an argv carrying the
    /// script AND the payload. A guard has to survive the regression it guards an assertion for,
    /// or it replaces that assertion's message with its own.
    fn payload_only_on_stdin(
        called: &str,
        argv: &[String],
        it: &Captured,
        marker: &str,
        fed: &str,
    ) {
        assert!(
            it.done_at.exists(),
            "the {called} did not run to the end, so every capture below is whatever was at that \
             path beforehand — which is nothing"
        );
        assert!(
            argv.iter().any(|a| a.contains(it.script.as_str())),
            "the argv skein built for the {called} does not carry the script, so the assertion \
             below is about nothing: {argv:?}"
        );
        assert!(
            !argv.iter().any(|a| a.contains(marker)),
            "the payload is in the argv skein built for the {called}"
        );

        // And the same thing asked of the kernel rather than of skein's own value.
        let cmdline = fs::read_to_string(&it.cmdline_at)
            .unwrap_or_else(|e| panic!("the {called} captured no /proc/<pid>/cmdline at all: {e}"));
        assert!(
            cmdline.contains(it.script.as_str()),
            "the capture does not hold the {called}'s own script element, so it would satisfy the \
             assertion below however the payload was sent: {} bytes captured",
            cmdline.len()
        );
        assert!(
            !cmdline.contains(marker),
            "the payload is in the spawned {called}'s /proc/<pid>/cmdline — world readable for as \
             long as the call runs, to anything sharing this machine"
        );
        assert_eq!(
            fs::read_to_string(&it.stdin_at)
                .unwrap_or_else(|e| panic!("the {called} was fed nothing at all: {e}")),
            fed,
            "the {called} was not handed the payload on stdin, or not all of it"
        );
    }

    /// **The payload a crossing carries rides its stdin, and is nowhere in that process's own
    /// `/proc/<pid>/cmdline`** (SKEIN-813).
    ///
    /// The property was stated in three places and asserted in none of them: `fleet.rs`'s "the
    /// payload in `ps` … what was in it is the diff of a pull request, private repositories
    /// included", [`Place::attempt`]'s own "a pipe … is also not `/proc/<pid>/cmdline`", and
    /// SKEIN-706's done-when. What guarded it was that `model_call_script` no longer takes a
    /// prompt, so the exact revert fails to *compile* — a guard against one revert rather than
    /// against the property. Any new caller of [`Place::attempt`] that inlines its payload into
    /// the script, or a convenience that appends it to the argv, passes every other gate here.
    ///
    /// **Measured on a real process, running production's own argv.** The seam records the argv
    /// and hands it straight back, which is how a fixture says it meant *this* crossing rather
    /// than standing in for it (`tests/fleet_move.rs`'s run arm does the same) — so what
    /// `/proc/$$/cmdline` holds is the argv skein built, not a stand-in carrying a copy of it.
    /// [`Where::SandboxItself`] aimed at the sandbox this process stands in is the mode that makes
    /// that safe to run: [`Place::reach`] and [`Place::enter`] are both empty, so there is no
    /// `sbx` hop and no `nsenter`, and what spawns is `env PATH=… bash -c <script>` doing exactly
    /// what the fixture above says.
    ///
    /// **The payload is deliberately SMALL, and that is what this adds to
    /// `ai::tests::a_call_with_a_box_reaches_the_box_and_carries_its_prompt_on_stdin`.** That one
    /// sends 600,000 bytes because its subject is the ceiling: past `MAX_ARG_STRLEN` — 32 pages,
    /// 131,072 bytes on 4 KiB-page hardware — a payload back inside the script cannot be spawned
    /// at all, so a regression fails there on "the box was never reached" and its argv assertion
    /// never gets to speak. Under the cap the spawn *succeeds* and the leak is silent. That is the
    /// case this covers, and it is the one a reader of `ps` would actually have got.
    ///
    /// **What makes it fail**, named before it was written and then done: build the argv the old
    /// shape built, by putting `feed` back inside the script in [`Place::attempt`]. The
    /// `/proc/<pid>/cmdline` assertion is the one that fires.
    #[cfg(unix)]
    #[test]
    fn the_payload_a_crossing_carries_is_on_its_stdin_and_not_in_its_cmdline() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        let home = home.as_ref() as &std::path::Path;
        let mut env = crate::testutil::env_pins();
        env.set("SKEIN_HOME", home);
        // Pinned beside it: `$SKEIN_FLEET_ROOT` unpinned falls back to `/boxes`, and this fixture
        // spawns for real — an unpinned one is it operating the live fleet (SKEIN-685).
        env.set("SKEIN_FLEET_ROOT", home);
        fs::write(
            home.join("config.json"),
            r#"{"fleet_sandbox":"skein-fleet"}"#,
        )
        .unwrap();

        // Where the crossing reports what it was actually handed. The argv the seam RECORDS is
        // skein's own value; these files are what any process on this machine could read off it.
        let it = reporting_how_it_was_spawned(home, "crossing");

        // Shaped so a grep for it finds this test and nothing else.
        let marker = "SKEIN-813-PAYLOAD-MARKER";
        let feed = format!("{marker} ").repeat(2_000);
        assert!(
            feed.len() < 131_072,
            "the payload has grown past MAX_ARG_STRLEN on 4 KiB-page hardware, so a payload put \
             back inside the script would fail to SPAWN and this test would stop being about what \
             `ps` shows: {} bytes",
            feed.len()
        );

        let seen: std::sync::Arc<std::sync::Mutex<Vec<Vec<String>>>> = Default::default();
        let recorder = std::sync::Arc::clone(&seen);
        let _at = seam::install(Box::new(move |argv: &[String]| {
            recorder.lock().unwrap().push(argv.to_vec());
            // Handed back unchanged: the process that runs is the crossing skein built, so the
            // cmdline read below is production's, not a copy of it passed to a stand-in.
            Some(argv.to_vec())
        }));

        let ran = own_sandbox("skein-fleet")
            .attempt(&it.script, feed.as_bytes(), Duration::from_secs(30))
            .expect("the crossing never ran, so nothing below is about a process");
        assert_eq!(
            (ran.code, String::from_utf8_lossy(&ran.out).into_owned()),
            (0, "the crossing read it".to_string()),
            "the crossing did not run to completion: {}",
            ran.err
        );

        // **The crossing was reached exactly once**, or the argv below is about some other call.
        let seen = seen.lock().unwrap().clone();
        assert_eq!(
            seen.len(),
            1,
            "the crossing was not spawned exactly once: {} times",
            seen.len()
        );
        payload_only_on_stdin("crossing", &seen[0], &it, marker, &feed);

        drop(_at);
    }

    /// **A body sent to a box rides the write's stdin, and is nowhere in that process's own
    /// `/proc/<pid>/cmdline` — and this is the path that carries CREDENTIALS** (SKEIN-822).
    ///
    /// [`Place::write`] says it in its own words: "the only way skein sends a box anything
    /// sensitive … an argv is visible in `ps` to anything sharing this machine, so a token passed
    /// as an argument is a token in every process listing and every shell history. A body on stdin
    /// is not." Nothing asserted it. The same shape as SKEIN-813, one path along.
    ///
    /// **Why the sibling test does not cover this, checked rather than assumed.** The argv half is
    /// shared — [`Place::write_argv`] and [`Place::exec_argv`] have the same body, and both go
    /// through [`Place::spawning`], so the seam covers this path too. Everything after it is a
    /// second implementation: `write` spawns the child itself, pipes the body from a thread of its
    /// own, and waits under a deadline that starts before the body is sent rather than after. It
    /// also **nulls stdout**, so the evidence that the crossing ran cannot be what it printed —
    /// hence the `done` file in the fixture above.
    ///
    /// **The body is deliberately the size of a real credential**, and that is the sharper form of
    /// the SKEIN-813 argument. A prompt is large enough that `MAX_ARG_STRLEN` would eventually
    /// refuse to spawn one on argv; a token is ~2 KB at the outside, so **nothing structural would
    /// ever stop it going on argv**. On this path the assertion is the only thing there is.
    ///
    /// **What makes it fail**: put the body back into the script in [`Place::write`] — the shape
    /// where a caller interpolates a token into the command it is about to run. Done, and both the
    /// argv assertion and the `/proc/<pid>/cmdline` assertion fire.
    #[cfg(unix)]
    #[test]
    fn the_body_a_write_carries_is_on_its_stdin_and_not_in_its_cmdline() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        let home = home.as_ref() as &std::path::Path;
        let mut env = crate::testutil::env_pins();
        env.set("SKEIN_HOME", home);
        env.set("SKEIN_FLEET_ROOT", home);
        fs::write(
            home.join("config.json"),
            r#"{"fleet_sandbox":"skein-fleet"}"#,
        )
        .unwrap();

        let it = reporting_how_it_was_spawned(home, "write");

        // Shaped like the thing this path actually carries — a credential — and so a grep for it
        // finds this test and nothing else.
        let marker = "SKEIN-822-CREDENTIAL-MARKER";
        let body = format!("{marker}.").repeat(64);
        assert!(
            body.len() < 131_072,
            "the body has grown past MAX_ARG_STRLEN, so this test would start being rescued by a \
             spawn failure rather than asserting anything: {} bytes",
            body.len()
        );

        let seen: std::sync::Arc<std::sync::Mutex<Vec<Vec<String>>>> = Default::default();
        let recorder = std::sync::Arc::clone(&seen);
        let _at = seam::install(Box::new(move |argv: &[String]| {
            recorder.lock().unwrap().push(argv.to_vec());
            Some(argv.to_vec())
        }));

        own_sandbox("skein-fleet")
            .write(&it.script, body.as_bytes(), Duration::from_secs(30))
            .expect("the write never ran, or the crossing it spawned exited non-zero");

        let seen = seen.lock().unwrap().clone();
        assert_eq!(
            seen.len(),
            1,
            "the write was not spawned exactly once: {} times",
            seen.len()
        );
        payload_only_on_stdin("write", &seen[0], &it, marker, &body);

        drop(_at);
    }
}
