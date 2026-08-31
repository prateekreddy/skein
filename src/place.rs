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
use std::net::TcpStream;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

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
    /// Still a box, in the owner's words: *"you aren't creating a new class of sessions but just
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

/// The shell that reports which boxes are running — **one exec for the whole fleet**, and an anchor
/// read rather than a socket probe wherever skein has an anchor it can check.
///
/// What this replaces asked each box's tmux socket `has-session`. Two things are wrong with that,
/// and only one of them is about cost:
///
///   * **it answers a different question.** `has-session` says *a* tmux server is listening on that
///     path. It does not say it is the server skein recorded — and the socket lives under the box's
///     own root, which is bound read-write into the box.
///   * **it is a crossing.** Those sockets are `0700`, so under the uid split (architecture §9.5.1)
///     every tick of the board would need `sudo -u` per box. §6 licenses the anchor read precisely
///     so the board does not sit on that path.
///
/// The anchor answer is `(boot_id, starttime)` against the record, parsed exactly as
/// [`anchor_probe`] parses it — the same `sed`/`cut`, deliberately, because a sweep that parsed
/// `/proc` differently from the stamp would disagree with it and boxes would flap between running
/// and stopped with nothing changing.
///
/// **Falls back to the socket** for a box whose record cannot decide it: no record at all, or one
/// written before the stamp existed, or one from an earlier boot of the sandbox. That direction is
/// not symmetric — calling a live box stopped invites somebody to start a second one over its work,
/// while the fallback costs a tmux exec for boxes that have not been restarted since the upgrade.
pub(crate) fn liveness_probe(root: &str, anchors: &[(String, u32, String, u64)]) -> String {
    let mut out = String::from(
        "boot=\"$(cat /proc/sys/kernel/random/boot_id 2>/dev/null)\"\nanswered=\" \"\n",
    );
    for (name, pid, generation, start) in anchors {
        // A record from a different boot, or one with no stamp, decides nothing: leave it to the
        // socket loop below rather than reporting a pid that names some other process now.
        if generation.is_empty() || *start == 0 {
            continue;
        }
        out.push_str(&format!(
            "if [ {gen} = \"$boot\" ]; then \
               seen=\"$(sed -n 's/.*) //p' /proc/{pid}/stat 2>/dev/null | cut -d' ' -f20)\"; \
               if [ \"$seen\" = {start} ]; then echo {name} 1; else echo {name} 0; fi; \
               answered=\"$answered{name_raw} \"; \
             fi\n",
            gen = crate::util::sh_quote(generation),
            start = crate::util::sh_quote(&start.to_string()),
            name = crate::util::sh_quote(name),
            name_raw = name,
        ));
    }
    // Every box the anchors could not decide, by the old question. The directory listing is also
    // what finds a box with no placement record at all.
    out.push_str(&format!(
        "for d in {root}/*/; do n=${{d%/}}; n=${{n##*/}}; \
           case \"$answered\" in *\" $n \"*) continue ;; esac; \
           s=\"$d/session.sock\"; \
           if [ -S \"$s\" ] && tmux -S \"$s\" has-session 2>/dev/null; then echo \"$n 1\"; \
           else echo \"$n 0\"; fi; done\n",
        root = root,
    ));
    out
}

/// [`liveness_probe`] as a read: the same decision, in this process, with nothing forked.
///
/// Deliberately **not** deployment-aware — it is the local half, and `fleet::fleet_liveness` is the
/// one place that decides which half to use. A second unit asking where skein runs would be a
/// second place to keep in step, and `deployment::CONSULTED_BY` exists to stop exactly that.
///
/// The two answers are the same two the probe gives, and the fidelity matters more than it looks:
/// a sweep that decided liveness differently from the stamp would make boxes flap between running
/// and stopped with nothing about them changing.
///
///   * **the anchor**, for a box whose record can decide it — `(boot_id, starttime)` against the
///     record. `starttime` is field 22 of `/proc/<pid>/stat`, taken after the last `)` because a
///     process's comm can itself contain spaces and parens; that is what the probe's `sed`/`cut`
///     does and this parses it the same way.
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
fn parse_proc_starttime(stat: &str) -> Option<u64> {
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

/// The port the agent listens on **inside the sandbox**, which is where skein now stands.
///
/// Spelled here as well as in `fleet` (`AGENT_SANDBOX_PORT`) because `place` deliberately does not
/// depend on `fleet` — `tools/module-check.py` asserts it, and `place` is already in the
/// eighteen-module knot. They agree by a constant each and by `fleet`'s
/// `the_two_ends_of_the_agent_agree_about_where_it_is`, which fails the moment they stop.
const AGENT_IN_SANDBOX_PORT: u16 = 8317;

/// Where skein keeps the agent's shared secret.
///
/// Host-driven: `~/.skein`, which is host-private — never the shared `.claude` store. The store is
/// mounted into every box, and this token authorises running commands as the sandbox in *any*
/// box's namespace: a copy inside a box would hand one box the run of all of them.
///
/// **The volume's copy, in both deployments, and that was worth testing rather than reasoning
/// about.** `ensure_agent_token` mints here and `ensure_fleet_agent` writes a replica into the
/// sandbox for the agent to read at start, so the two are equal at install and the volume's is the
/// minted one. Found in the fleet as two different 64-byte files, which reads like the sandbox copy
/// being the one that counts — it is not: asked directly, `/machine` answers 200 to the volume's
/// copy and 403 to the sandbox's. A skew here is the replica having been rewritten under a running
/// agent, and the fix for that is reinstalling the agent, not reading the stale side.
fn agent_token_path() -> PathBuf {
    skein_home().join("fleet-agent.token")
}

/// The agent's shared secret, or `None` when there isn't one yet — in which case there is no agent
/// to talk to and every call takes the `sbx exec` path, which is exactly the pre-agent behaviour.
pub fn agent_token() -> Option<String> {
    let token = fs::read_to_string(agent_token_path()).ok()?;
    let token = token.trim().to_string();
    (!token.is_empty()).then_some(token)
}

/// The agent's token, generating one on first use.
///
/// Kept rather than rotated on every install: rotating would invalidate the token of a sandbox that
/// is up and serving, and the reinstall that rotated it is exactly the moment skein is least able to
/// push the new one in.
///
/// 32 bytes from the OS, hex-encoded. Not a UUID or a timestamp — this is the only thing standing
/// between anything that can reach the port and running commands as the sandbox.
pub fn ensure_agent_token() -> Result<String, String> {
    if let Some(existing) = agent_token() {
        return Ok(existing);
    }
    let mut raw = [0u8; 32];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut raw))
        .map_err(|e| format!("reading /dev/urandom for a fleet agent token: {e}"))?;
    let token: String = raw.iter().map(|b| format!("{b:02x}")).collect();

    let home = skein_home();
    fs::create_dir_all(&home).map_err(|e| format!("mkdir {}: {e}", home.display()))?;
    let path = agent_token_path();
    write_atomic(&path, &home, token.as_bytes())?;
    // 0600 after the write, not before: `write_atomic` renames a fresh temp file over the target,
    // so a mode set on the old one would not survive.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600))
            .map_err(|e| format!("securing {}: {e}", path.display()))?;
    }
    Ok(token)
}

/// What this skein needs the in-sandbox agent to speak. See `PROTOCOL` in `fleet-agent.py`.
///
/// Checked before a streamed write and not before an exec, because the two fail differently: an
/// agent that does not know `/exec` refuses immediately and the caller drops to `sbx exec` having
/// lost nothing, while one that does not know `/write` would refuse *after* the whole body had been
/// sent — and the body cannot be sent twice, because for an upload it came off a network socket
/// that has already been drained.
/// **3** since the agent grew `/machine` and the Docker watchdog behind it: an agent an older skein
/// left running answers 2, has no watchdog, and `heal_fleet_agent` replaces it on that number alone.
///
/// **4** since a 504 carries the tail of what the killed script had printed — bumped when this
/// number was still the upgrade lever, because an improvement the number did not move never reached
/// a fleet that was already up: the agent was reinstalled on disk, the old process kept serving,
/// and the fix looked shipped.
///
/// It is not the upgrade lever any more, which returns it to meaning what it says. Whether the
/// running agent is replaced is decided by the *revision* of its source, reported beside this on
/// `/health` and compared by `fleet::retire_stale_agent` — a hash cannot be forgotten the way a
/// bump can. This number still answers the question only it can: which endpoints the client may
/// call, checked before a body goes down the wire. It moves when an endpoint or its framing does.
pub const AGENT_PROTOCOL: u32 = 4;

/// The largest body skein will push through the agent. Above it, `sbx exec -i`, which has no
/// ceiling at all.
///
/// Not a memory limit on either side — the host streams and the agent streams — but a limit on how
/// long one write may occupy a connection into the sandbox, and a number someone chose rather than
/// "until the box's disk fills".
pub const AGENT_WRITE_CAP: u64 = 1 << 30;

/// How long to wait to get *in* to the agent, before deciding it is not there.
///
/// Long, and deliberately so — the two-second budget this replaces had the failure backwards. It was
/// reasoned as "a slow no is worse than a fast one, the fallback is sitting right there", which
/// holds only if the fallback is healthy. It is not: `sbx exec` is the call that stalls under load,
/// and load is exactly when a busy sandbox is slow to accept. So the old budget gave up on the
/// working transport at the one moment it was worth waiting for, and fell back to the stalling one.
///
/// Measured here: one box legitimately holding 9.5 of 11 cores, which is the shared sandbox working
/// as intended — `cpu.weight` is equal and uncapped, so a lone box gets the machine and hands it
/// back under contention.
///
/// It costs less than it looks. A port with nothing behind it is *refused* immediately rather than
/// timing out, which covers both the ordinary "agent not installed" case and sbx's phantom mappings
/// (docker/sbx-releases#297), which refuse every connection while still being reported by
/// `sbx ports`. The budget is only ever spent when something is genuinely listening and too busy to
/// answer — which is the case this transport exists for.
const AGENT_CONNECT: Duration = Duration::from_secs(30);

/// The placement for a streamed write travels as a header, and Python's `http.server` refuses a
/// header line over 64 KB. Nothing skein sends comes near it; declining rather than truncating means
/// a caller that someday does falls back to `sbx exec` instead of writing a mangled script.
const MAX_META: usize = 32 * 1024;

/// Where skein records the host port it published and verified.
///
/// State, not configuration: skein chooses this and re-chooses it when healing, so it does not
/// belong in the file the user edits. [`crate::config::Config::fleet_agent_port`] stays the user's
/// to pin when they want a particular number.
///
/// It lives here with the transport rather than with the code that publishes it, and that is what
/// ends the one `place -> fleet` reference in the crate: the port is the transport's own address,
/// so reading it is not a question for whoever manages the sandbox. `fleet` still chooses and heals
/// the number; it calls in to record it, in the direction it already depends.
fn agent_port_path() -> std::path::PathBuf {
    skein_home().join("fleet-agent.port")
}

/// The port that reaches the agent: the host mapping skein published, or — in-fleet — the port the
/// agent actually listens on.
///
/// **The recorded file is the HOST's, and it is on the shared volume.** It holds a published
/// mapping, which is a fact about the host's network and means nothing from inside the sandbox:
/// skein in-fleet was reading 59461 and opening a connection that could only be refused, while the
/// agent sat answering on 8317 the whole time. Every call paid that failed connect and fell back.
/// `fleet::ensure_fleet_agent_port` already says the same thing from the other end — "in-fleet
/// there is no port to publish... the agent is on loopback at the port it listens on" — and this is
/// the reader catching up with it.
pub fn recorded_agent_port() -> Option<u16> {
    if crate::deployment::in_fleet() {
        return Some(AGENT_IN_SANDBOX_PORT);
    }
    std::fs::read_to_string(agent_port_path())
        .ok()?
        .trim()
        .parse()
        .ok()
}

/// Record a port as published and verified. `fleet` calls this after proving something answers.
pub(crate) fn record_agent_port(port: u16) {
    let home = skein_home();
    let _ = std::fs::create_dir_all(&home);
    let _ = crate::util::write_atomic(&agent_port_path(), &home, port.to_string().as_bytes());
}

/// The port and token that reach the agent, or `None` when there is no usable one to reach.
///
/// `None` covers every "not available" case there is — the setting off, no port verified yet, no
/// token — and every caller reads it the same way: use `sbx exec`, exactly as before the agent.
fn agent_target() -> Option<(u16, String)> {
    if !load_config().fleet_agent {
        return None;
    }
    // The port skein published and *verified*, not the one it was configured with: a pinned port
    // that never came up must not send every call into a connection that cannot answer.
    let port = recorded_agent_port()?;
    if port == 0 {
        return None;
    }
    Some((port, agent_token()?))
}

/// Whether the agent actually answers on `port` — a real connection, not a reported mapping.
///
/// This is the difference between "published" and "working", and sbx makes it a real distinction:
/// a mapping survives `sbx rm` and is reported by `sbx ports` while every connection through it is
/// refused (docker/sbx-releases#297). skein destroys and recreates the fleet sandbox on every
/// resize, so it meets that state routinely — and a healer that trusted the mapping would call the
/// broken one healthy forever.
///
/// `/health` is unauthenticated for exactly this: whether the transport is worth using must not
/// depend on the token being current, or a token mismatch would look like a dead sandbox.
pub fn agent_answers(port: u16) -> bool {
    agent_protocol(port).is_some()
}

/// Which protocol the agent on `port` speaks, or `None` when nothing there answers as one.
///
/// The agent is installed *into* the sandbox and outlives the skein that installed it, so a running
/// agent may be older than the host talking to it. Asking is how a newer skein avoids sending an
/// older agent something it has never heard of.
pub fn agent_protocol(port: u16) -> Option<u32> {
    agent_identity(port).map(|(version, _)| version)
}

/// What the agent on `port` says it is: the protocol it speaks and the revision of the source it
/// is serving. `None` when nothing there answers as one.
///
/// The revision is what `fleet::retire_stale_agent` compares: the protocol says which endpoints may
/// be called, and moves only when one changes — so it cannot see a change that moves no endpoint,
/// which is most of them. Empty for an agent from before revisions existed, which compares unequal
/// to every build's revision, exactly as it should.
pub fn agent_identity(port: u16) -> Option<(u32, String)> {
    if port == 0 {
        return None;
    }
    // One budget for getting in *and* asking, not one each: this is the call that decides whether
    // the transport is worth using, and a liveness probe that can take twice its own number is not
    // a bound on anything.
    let deadline = Deadline::of(AGENT_CONNECT);
    let mut stream = agent_connect(port, deadline).ok()?;
    health_over(&mut stream, deadline)
}

/// Ask an already-open connection what it is, leaving it usable afterwards.
///
/// Keep-alive rather than close, so a caller that is about to write can ask on the very connection
/// it is going to use — one round trip, no second socket, and no window in which the agent it
/// probed is not the agent it writes to.
fn health_over(stream: &mut TcpStream, deadline: Deadline) -> Option<(u32, String)> {
    let request = "GET /health HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: keep-alive\r\n\r\n";
    deadline.arm(stream, "the health probe was not sent").ok()?;
    stream.write_all(request.as_bytes()).ok()?;
    let reply = read_reply(stream, deadline).ok()?;
    if reply.status != 200 {
        return None;
    }
    let body = String::from_utf8_lossy(&reply.out);
    let mut words = body.split_whitespace();
    if words.next()? != "skein-fleet-agent" {
        return None;
    }
    // An agent from before versions existed answers with its name alone. That is protocol 1, not a
    // parse failure — treating it as one would make every agent skein has already installed look
    // dead, and take the board back to `sbx exec` for the one thing that was working.
    let version = words.next().and_then(|v| v.parse().ok()).unwrap_or(1);
    Some((version, words.next().unwrap_or_default().to_string()))
}

/// One call's budget, counted once for the whole call rather than once per `read`.
///
/// A socket deadline bounds a single `read()`, not a conversation, and [`read_reply`] makes as many
/// reads as the reply has chunks — so every chunk renewed the window, and an agent answering slowly
/// was never bounded by the number its caller passed. Measured on the owner's fleet (SKEIN-350):
/// `GET /review/687/summary?asked=1` was still running at 391 seconds against the 180-second budget
/// set at `src/review.rs:2013`, while GitHub answered a forced queue refresh in 7.4s and
/// `/api/health` in 36ms — so the wire was the only thing left that could have been waiting.
///
/// The budget therefore becomes an *instant* when the call starts, and every read and write is
/// armed with what is left of it. That costs one `setsockopt` per 4 KB read, a syscall paid against
/// a syscall, and it is the only thing that can make a loop of reads end when its caller said so.
#[derive(Clone, Copy)]
struct Deadline {
    at: Instant,
    /// What the caller asked for, kept for the sentence. "no answer" and "no answer in 180s" send a
    /// reader to different places, and only the second names the number that was spent.
    budget: Duration,
}

impl Deadline {
    fn of(budget: Duration) -> Self {
        Deadline {
            at: Instant::now() + budget,
            budget,
        }
    }

    /// What is left, or `None` when it is gone.
    ///
    /// Never `Some(0)`: a zero timeout on a socket means *no* timeout, so the one moment the
    /// deadline matters most is the moment rounding down to zero would remove it.
    fn left(&self) -> Option<Duration> {
        self.at
            .checked_duration_since(Instant::now())
            .filter(|left| !left.is_zero())
    }

    /// Point both halves of `stream` at what is left of this call.
    ///
    /// Both halves, and on every use rather than at connect — because a connection taken back out
    /// of [`IDLE`] is still carrying whatever the *previous* caller armed it with. That is the
    /// second half of SKEIN-350: `set_read_timeout` was only ever called on a newly-created socket,
    /// so a call asking for one second could run on a socket configured for three minutes.
    ///
    /// `at` is where the call had got to, and it is here rather than only on the read that fails
    /// because a budget expires *between* reads as readily as during one. Told apart, the same
    /// failure would produce a different sentence on different runs of it.
    fn arm(&self, stream: &TcpStream, at: &str) -> Result<(), Fault> {
        let left = self.left().ok_or_else(|| self.spent(at))?;
        stream.set_read_timeout(Some(left)).ok();
        stream.set_write_timeout(Some(left)).ok();
        Ok(())
    }

    /// The budget ran out. `at` says how far the call had got, because "no answer at all" and "the
    /// reply stopped half way" are different faults with the same elapsed time.
    fn spent(&self, at: &str) -> Fault {
        Fault {
            why: format!(
                "fleet agent: no answer in {:.1}s ({at})",
                self.budget.as_secs_f64()
            ),
            unheard: false,
        }
    }
}

/// Why one exchange produced no reply, and whether a *fresh connection* could produce one.
///
/// Not a `String`, because [`agent_post`] retries once and the retry is only ever safe in one
/// state. Retrying a spent budget doubles the number the caller passed — the very thing SKEIN-350
/// is about — and retrying after any of the reply has arrived re-runs a script that has already had
/// its effect, which [`Place::via_agent`]'s doc calls the one way this transport can be worse than
/// no transport at all.
#[derive(Debug)]
struct Fault {
    why: String,
    /// True only when the agent provably said **nothing**: the request never landed, so sending it
    /// again cannot apply an effect twice. A truncated request cannot have run either — the agent
    /// reads the whole `Content-Length` body and only then parses and spawns
    /// (`src/fleet-agent.py:222`).
    unheard: bool,
}

impl Fault {
    /// Nothing came back, and time remains: worth one fresh connection.
    fn unheard(why: String) -> Self {
        Fault { why, unheard: true }
    }

    /// Something came back, or something is already spent. Report it; do not send it again.
    fn heard(why: String) -> Self {
        Fault {
            why,
            unheard: false,
        }
    }
}

/// The connections to the in-sandbox agent, held open between calls and handed out one per call.
///
/// **Held** is the entire point, not an optimisation. The stall this transport exists to survive is
/// asymmetric — it hangs calls that need a *new* channel into the sandbox while established ones
/// keep flowing — so a client that opened a fresh connection per request would reproduce the very
/// failure it was built to avoid, and would do so only under load, where it would look like the
/// agent had made no difference at all.
///
/// **Several of them, though, and not one.** This was `Mutex<Option<TcpStream>>`, defended as "at
/// most one of these questions is in flight at a time, because skein's callers are already
/// funnelled through `Gate`". [`crate::util::Gate`] is not a funnel — it remembers the answer to
/// one *named* question — and nothing funnels a call into a box: `src/files.rs:153` lists a
/// directory on one HTTP handler while `src/fleet.rs:5695` runs a model call on another, and both
/// land here. One socket behind one mutex, with [`agent_post`] holding that mutex across the entire
/// exchange, made every box queue behind every other box. Measured on the owner's fleet
/// (SKEIN-351): `files?path=.` returned nothing after 60s on `gadget-demo-repo-archaeology` and
/// nothing after 25s on `example-box-6` — a *different* live box — while a box that does not
/// exist 404ed in 0.005s. Routing was fine; the queue was the transport.
///
/// The peer was never the constraint. The agent is a `ThreadingHTTPServer` with
/// `daemon_threads = True` (`src/fleet-agent.py:673`), so it has always served connections
/// concurrently; the single socket was skein's own bottleneck and nobody else's.
///
/// **The lock covers a pop and a push, never a conversation.** That is the whole of the fix: a call
/// that never answers now costs its own caller and nobody else.
///
/// Keyed by port, because the port is not fixed: `fleet` re-chooses and re-records it when it heals
/// the agent (see [`record_agent_port`]), and a connection to the number skein has stopped
/// publishing is a connection to the agent it just replaced.
static IDLE: Mutex<Vec<(u16, TcpStream)>> = Mutex::new(Vec::new());

/// How many idle connections are worth keeping between calls.
///
/// A ceiling on what a burst *leaves behind*, not on concurrency: a call always gets a connection,
/// because refusing one would put back the queue this exists to remove. Above the ceiling the
/// socket is closed and the next call pays one connect — which is what every call paid before this
/// transport existed. Comfortably above one per box for a fleet of today's size, and small enough
/// that the sockets themselves are nothing.
const AGENT_IDLE_MAX: usize = 16;

/// An idle connection to `port` that still looks usable, or `None` — open a fresh one.
///
/// The lock is taken for the pop and dropped before a byte moves.
fn take_idle(port: u16) -> Option<TcpStream> {
    loop {
        let (kept_port, stream) = {
            let mut idle = IDLE.lock().unwrap_or_else(|e| e.into_inner());
            idle.pop()
        }?;
        // Ordinary rather than a fault: the agent was healed onto a new number and this socket
        // reaches the one it replaced. Dropped, and the next call opens one to the current agent.
        if kept_port == port && silent(&stream) {
            return Some(stream);
        }
    }
}

/// Does this idle connection have **nothing** to say — the only state in which reusing it is safe?
///
/// Asked before every reuse, because both ways a kept connection goes bad are invisible to a
/// writer, and a writer is what [`agent_exchange`] starts with:
///
/// * the peer closed its half while the socket sat idle — an agent restarting, the sandbox cycling
///   — and a `write` into that succeeds. `agent_post` recovered from this only because the *read*
///   afterwards failed, which is a whole exchange spent to learn what one byte says here;
/// * a previous call ended mid-reply and left its tail in the buffer. Reading that as the next
///   call's answer is exactly the failure [`read_reply`]'s doc exists to prevent, and this is the
///   second guard on it — the first being that a connection is only ever put back after a complete
///   framed reply.
///
/// `peek` rather than `read`, so the check cannot consume the byte it is looking for; non-blocking
/// rather than a short timeout, so it costs no wall clock at all.
///
/// **A filter, not a proof — and that is why [`agent_post`] still reconnects.** This asks the local
/// kernel what has arrived *so far*, which leaves a residue it cannot see and only the retry
/// covers: a FIN still on the wire when the peek runs; a peer that closes between the peek and the
/// write; and a half-open connection where no FIN is coming at all, because the sandbox was
/// destroyed and recreated under it — the socket then looks perfectly silent, the request goes into
/// nothing, and the RST arrives as the read. None of that is exotic here. `heal_fleet_agent` runs
/// on every server start and every box start (`src/fleet.rs:370`), and through `retire_stale_agent`
/// (`src/fleet.rs:353`) it stops the running agent whenever the revision it serves is not this
/// build's — killing every connection in [`IDLE`] at a moment nothing coordinates with.
///
/// Worth spelling out because the cheap experiment says the opposite. Break the retry, point a test
/// at an agent that hangs up *as soon as it has answered*, and everything still passes — the FIN
/// has landed by the time the next call peeks, so this check drops the socket and the retry is
/// never reached. That measures the check, not the retry. Reaching it needs a peer that hangs up on
/// the *next request*, which is what the agent being replaced actually looks like from here, and is
/// the shape `a_kept_connection_the_agent_hung_up_on_is_replaced_in_silence` uses.
fn silent(stream: &TcpStream) -> bool {
    if stream.set_nonblocking(true).is_err() {
        return false;
    }
    let mut probe = [0u8; 1];
    let quiet = matches!(
        stream.peek(&mut probe),
        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock
    );
    // Back to blocking whatever the verdict: a socket left non-blocking turns every read in
    // `read_reply` into an instant `WouldBlock`, which the deadline would report as a spent budget.
    stream.set_nonblocking(false).is_ok() && quiet
}

/// Put a connection back for the next call — **only** ever after a complete framed reply.
fn keep_idle(port: u16, stream: TcpStream) {
    let mut idle = IDLE.lock().unwrap_or_else(|e| e.into_inner());
    if idle.len() < AGENT_IDLE_MAX {
        idle.push((port, stream));
    }
}

/// What the agent said. `status` is HTTP's; `exit` is the script's, and the two mean different
/// things — see [`Place::via_agent`], where confusing them would re-run a side effect.
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

#[derive(Debug)]
struct AgentReply {
    status: u16,
    exit: i32,
    out: Vec<u8>,
    err: String,
}

/// POST one script to the agent, on a connection of this call's own, within `timeout` — once.
///
/// **One budget for the whole of it.** `timeout` becomes a [`Deadline`] here and nothing below
/// starts a new one: connecting, writing and every read of the reply come out of the same number,
/// and the retry below runs on what is left of it rather than on a second helping. A call that
/// asked for 180 seconds is over at 180 seconds, whatever the wire does (SKEIN-350).
///
/// **The retry is narrow on purpose.** A keep-alive connection can be closed by the peer at any
/// time — the agent restarting, an idle reaper, the sandbox cycling — and that is ordinary, not a
/// fault, so it must not surface as one. But it is only ever safe to send the same script twice
/// when the agent provably never received it: see [`Fault::unheard`]. Anything the agent has begun
/// answering, and any budget already spent, is reported as it stands.
fn agent_post(
    port: u16,
    token: &str,
    body: &[u8],
    timeout: Duration,
) -> Result<AgentReply, String> {
    let deadline = Deadline::of(timeout);
    // Taken and put back under the lock; the exchange itself runs with nothing held, which is what
    // stops one box's stuck call from making every other box unreachable (SKEIN-351).
    if let Some(mut kept) = take_idle(port) {
        match agent_exchange(&mut kept, token, body, deadline) {
            Ok(reply) => {
                keep_idle(port, kept);
                return Ok(reply);
            }
            Err(fault) if !fault.unheard => return Err(fault.why),
            // Stale, so it costs one reconnect and not a reported failure. `kept` is dropped here
            // and never put back: a connection that failed mid-reply is framed mid-message.
            Err(_) => {}
        }
    }
    // A connection that will not open at all is the sandbox being unreachable rather than a stale
    // socket, so it is reported rather than retried.
    let mut fresh = agent_connect(port, deadline)?;
    match agent_exchange(&mut fresh, token, body, deadline) {
        Ok(reply) => {
            keep_idle(port, fresh);
            Ok(reply)
        }
        Err(fault) => Err(fault.why),
    }
}

/// Open a connection to the agent, spending `deadline` to do it.
///
/// Getting in comes out of the same budget as everything after it, because it is part of the same
/// call: a caller that allowed five seconds must not be able to spend thirty of them connecting.
fn agent_connect(port: u16, deadline: Deadline) -> Result<TcpStream, String> {
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    let left = deadline
        .left()
        .ok_or_else(|| deadline.spent("no time left to connect").why)?;
    let stream = TcpStream::connect_timeout(&addr, left)
        .map_err(|e| format!("fleet agent: connect {port}: {e}"))?;
    // Deadlines on both halves: without them a sandbox that accepts the connection and then stops
    // answering blocks this thread forever, which is the failure the transport exists to prevent.
    // Re-armed from `deadline` before every read and write — arming them once here is what let a
    // slow reply renew its window per chunk, and let a reused socket keep the last caller's number.
    deadline
        .arm(&stream, "nothing was sent")
        .map_err(|fault| fault.why)?;
    // Same reason the server sets it: the requests are small and latency matters more than packing.
    stream.set_nodelay(true).ok();
    Ok(stream)
}

/// One request/response on an already-open socket.
///
/// Hand-rolled rather than reached for from a crate: this speaks HTTP/1.1 to loopback, to a peer
/// skein itself installs, with `Content-Length` framing on both sides and no TLS, redirects,
/// chunking or content negotiation. A client crate would bring an async runtime into a synchronous
/// call path that runs on tokio worker threads, which is a deadlock to reason about in exchange for
/// features this wire has none of.
/// What the in-sandbox agent says about the machine: the Docker watchdog, and the kernel's memory
/// pressure counters.
///
/// **Its own connection rather than the pooled one.** `agent_post` holds a keep-alive socket for
/// `/exec`, and this is asked rarely — a surface, not a tick — so borrowing that socket to send a
/// different shape of request would risk the pool for no gain.
///
/// `None` when there is no agent, when it is older than the endpoint, or when it will not answer.
/// All three mean the same thing to a caller: no numbers this time. A fleet without an agent is not
/// a fleet with a problem, and reporting one would be inventing news.
pub fn agent_machine() -> Option<serde_json::Value> {
    let (port, token) = agent_target()?;
    let deadline = Deadline::of(AGENT_CONNECT);
    let mut stream = agent_connect(port, deadline).ok()?;
    let request = format!(
        "GET /machine HTTP/1.1\r\nHost: 127.0.0.1\r\nX-Skein-Token: {token}\r\n\
         Connection: close\r\n\r\n"
    );
    stream.write_all(request.as_bytes()).ok()?;
    let reply = read_reply(&mut stream, deadline).ok()?;
    // 404 is an agent from before this endpoint existed, which `AGENT_PROTOCOL` already handles by
    // replacing it — so this is the window between the two, not an error to report.
    (reply.status == 200)
        .then(|| serde_json::from_slice(&reply.out).ok())
        .flatten()
}

fn agent_exchange(
    stream: &mut TcpStream,
    token: &str,
    body: &[u8],
    deadline: Deadline,
) -> Result<AgentReply, Fault> {
    let head = format!(
        "POST /exec HTTP/1.1\r\nHost: 127.0.0.1\r\nX-Skein-Token: {token}\r\n\
         Content-Type: application/json\r\nContent-Length: {}\r\nConnection: keep-alive\r\n\r\n",
        body.len()
    );
    deadline.arm(stream, "the request was not sent")?;
    stream
        .write_all(head.as_bytes())
        .and_then(|_| stream.write_all(body))
        .and_then(|_| stream.flush())
        .map_err(|e| match e.kind() {
            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut => {
                deadline.spent("the request was still going out")
            }
            // The request did not land, whole or in part, so it cannot have run: the agent reads
            // the entire `Content-Length` body and only then parses it and spawns anything
            // (`src/fleet-agent.py:222`). That makes this the one failure worth sending again.
            _ => Fault::unheard(format!("fleet agent: write: {e}")),
        })?;
    read_reply(stream, deadline)
}

/// What a failed `read` on the agent's socket means.
///
/// The two are told apart by the error kind and not by the elapsed time, because they need opposite
/// answers. `WouldBlock`/`TimedOut` is the socket deadline firing, and [`Deadline::arm`] set that
/// to exactly what was left of the call — so it *is* the caller's budget, and re-sending on a fresh
/// connection would spend the number twice. Anything else is the connection breaking, which is
/// worth another one only if the agent had not yet said a word.
fn read_fault(e: std::io::Error, deadline: Deadline, at: &str, unheard: bool) -> Fault {
    match e.kind() {
        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut => deadline.spent(at),
        _ if unheard => Fault::unheard(format!("fleet agent: read: {e}")),
        _ => Fault::heard(format!("fleet agent: read: {e}")),
    }
}

/// Read one `Content-Length`-framed response off an open socket, leaving it positioned for the next.
///
/// Shared by every shape of request the agent answers, which is what makes keep-alive safe: a reply
/// that stopped short of its body would leave the connection framed mid-message, and the *next*
/// call on it would read this one's leftovers as its own answer. Every failure below therefore
/// returns a [`Fault`] and never a reusable socket — [`agent_post`] puts a connection back only on
/// the `Ok` path, and [`silent`] checks for leftovers again before the next call touches one.
///
/// `deadline` is the *call's*, not this read's. Arming it inside the loop is the fix for SKEIN-350:
/// the deadlines used to be set once, at connect, which bounded a single `read()` and so gave every
/// chunk of a slow reply a fresh window. A reply arriving a byte at a time was unbounded.
fn read_reply(stream: &mut TcpStream, deadline: Deadline) -> Result<AgentReply, Fault> {
    const MAX_HEAD: usize = 64 * 1024;
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    let head_end = loop {
        if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break i;
        }
        if buf.len() > MAX_HEAD {
            return Err(Fault::heard(
                "fleet agent: response header too large".into(),
            ));
        }
        let at = match buf.len() {
            0 => "nothing came back at all".to_string(),
            got => format!("{got} bytes of a header arrived and then stopped"),
        };
        deadline.arm(stream, &at)?;
        match stream.read(&mut chunk) {
            // Nothing at all, and the peer has gone: the ordinary end of an idle keep-alive
            // connection, and the one case worth sending the request again on a fresh one. Once a
            // byte has arrived it is this agent's answer, cut short — not a socket to retry.
            Ok(0) if buf.is_empty() => {
                return Err(Fault::unheard(
                    "fleet agent: the connection was closed before it answered".into(),
                ))
            }
            Ok(0) => {
                return Err(Fault::heard(
                    "fleet agent: connection closed mid-header".into(),
                ))
            }
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
            Err(e) => return Err(read_fault(e, deadline, &at, buf.is_empty())),
        }
    };

    let header_text = String::from_utf8_lossy(&buf[..head_end]).into_owned();
    let mut lines = header_text.lines();
    let status = lines
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|c| c.parse::<u16>().ok())
        .ok_or_else(|| Fault::heard("fleet agent: unparseable status line".into()))?;
    let header = |name: &str| {
        header_text
            .lines()
            .skip(1)
            .filter_map(|l| l.split_once(':'))
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.trim().to_string())
    };
    let length: usize = header("Content-Length")
        .and_then(|v| v.parse().ok())
        .ok_or_else(|| Fault::heard("fleet agent: response has no Content-Length".into()))?;

    let mut out = buf.split_off(head_end + 4);
    out.reserve(length.saturating_sub(out.len()));
    while out.len() < length {
        // The count is the point of the sentence: a reply that stopped 12 bytes into 4 MB is a box
        // that died, and one that stopped 12 bytes short is a box that is still writing.
        let at = format!("the reply stopped {} bytes into {length}", out.len());
        deadline.arm(stream, &at)?;
        match stream.read(&mut chunk) {
            Ok(0) => {
                return Err(Fault::heard(
                    "fleet agent: connection closed mid-body".into(),
                ))
            }
            Ok(n) => out.extend_from_slice(&chunk[..n]),
            Err(e) => return Err(read_fault(e, deadline, &at, false)),
        }
    }
    out.truncate(length);

    Ok(AgentReply {
        status,
        exit: header("X-Skein-Exit")
            .and_then(|v| v.parse().ok())
            .unwrap_or(-1),
        err: header("X-Skein-Stderr")
            .map(|v| decode_b64(&v))
            .unwrap_or_default(),
        out,
    })
}

/// A write into a box that has already started, fed a piece at a time.
///
/// This is `sbx exec -i` without the `sbx exec` — the same "run a script with a body on its stdin",
/// carried over the connection that stays answerable when the daemon stops being. It exists as a
/// handle rather than as one call because the body may be an 800 MB video arriving from a browser:
/// the host must be able to push it through as it comes, never holding more than a chunk.
///
/// **Creating one commits the caller.** By the time [`AgentWrite::begin`] returns, the request head
/// is on the wire and the agent has spawned the child, so the script may already have had an effect.
/// A failure after that is a failure, never a fallback — running it again on `sbx exec` would apply
/// the same effect twice. `begin` returning `None` is the only safe place to fall back, and it does
/// all its refusing there: no agent, too old an agent, an outsized placement.
pub struct AgentWrite {
    stream: TcpStream,
    /// The host's patience with the wire, kept so [`AgentWrite::finish`] can count it from the
    /// moment the body **ends** rather than from `begin`. A body may legitimately take an hour to
    /// push, and a deadline armed at `begin` would already have expired before the verdict was due.
    stall: Duration,
}

/// One piece of a chunked body, or nothing at all for an empty piece.
///
/// **`None` is the whole point.** A zero-length chunk is how chunked encoding spells "that was the
/// last one", so an empty piece — which a body stream may well yield mid-upload — would end the
/// write into the box and truncate the file silently: it arrives, shorter, and nothing says so. An
/// unacknowledged upload with a silent-truncation mode is a defect, not a design (§2.5).
///
/// Its own function so that rule can be tested without a socket, which is why it is here rather than
/// inline: the guard was correct and unguarded, and removing it broke nothing.
fn chunk_frame(chunk: &[u8]) -> Option<Vec<u8>> {
    if chunk.is_empty() {
        return None;
    }
    let mut frame = format!("{:x}\r\n", chunk.len()).into_bytes();
    frame.extend_from_slice(chunk);
    frame.extend_from_slice(b"\r\n");
    Some(frame)
}

impl AgentWrite {
    /// Open a write, or decline. `None` ⇒ nothing was sent and `sbx exec` is free to do it instead.
    ///
    /// `timeout` is the **box-side child's** budget and `stall` is the **host's patience with the
    /// wire**; they are two different questions and passing one number for both is what SKEIN-269
    /// found. An upload's child may legitimately run for an hour, so `timeout` is an hour — and
    /// that hour was also what the socket deadlines got, which meant a box that accepted the
    /// connection and then serviced nothing held this thread, and the request behind it, for the
    /// full hour with nothing said. A wire that is moving no bytes is not a slow upload.
    fn begin(place: &Place, script: &str, timeout: Duration, stall: Duration) -> Option<Self> {
        let (port, token) = agent_target()?;
        // A connection of its own, never the held one. A write takes as long as its body is big,
        // and the held connection is what liveness, resources and the board poll through — an
        // upload that borrowed it would blind the cockpit for the duration, which is a worse
        // version of the failure this transport was built to prevent.
        // Two budgets, not one: getting in is bounded by AGENT_CONNECT, while the body itself may
        // legitimately take an hour, so its deadlines are widened only once the agent has proved it
        // is there.
        let entry = Deadline::of(AGENT_CONNECT);
        let mut stream = agent_connect(port, entry).ok()?;
        // Ask before committing. An agent installed by an older skein has no `/write`, and would
        // say so only after the entire body had been sent — with no way back, because an upload's
        // bytes come off a network socket that has already been drained. One round trip, on the
        // connection about to be used, is what makes the fallback below reachable.
        //
        // Getting in and asking share `entry`, so this whole preamble is bounded by AGENT_CONNECT
        // once — not by it per read, which is what let a box that accepted and then went quiet hold
        // an upload here indefinitely.
        if health_over(&mut stream, entry)?.0 < AGENT_PROTOCOL {
            return None;
        }
        let meta = encode_b64(&serde_json::to_vec(&place.agent_request(script, timeout)).ok()?);
        if meta.len() > MAX_META {
            return None;
        }
        // The wire's deadline, not the child's. `push` blocking means the box has stopped reading,
        // and that is what this bounds — per piece, because a stream that keeps moving is a stream
        // that is fine however long the whole body takes. The read side is not set here at all: it
        // belongs to `finish`, which arms one deadline over the wait for the verdict.
        stream.set_write_timeout(Some(stall)).ok();
        // Chunked, because the caller does not always know the length: an upload is streamed from
        // the browser through skein into the box, and buffering it on the host to count it would
        // undo the whole point of streaming.
        let head = format!(
            "POST /write HTTP/1.1\r\nHost: 127.0.0.1\r\nX-Skein-Token: {token}\r\n\
             X-Skein-Meta: {meta}\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n"
        );
        stream.write_all(head.as_bytes()).ok()?;
        Some(AgentWrite { stream, stall })
    }

    /// Push one piece of the body.
    pub fn push(&mut self, chunk: &[u8]) -> Result<(), String> {
        let Some(frame) = chunk_frame(chunk) else {
            return Ok(());
        };
        self.stream
            .write_all(&frame)
            .map_err(|e| format!("fleet agent: writing the body: {e}"))
    }

    /// End the body and wait for the box's verdict.
    pub fn finish(mut self) -> Result<(), String> {
        self.stream
            .write_all(b"0\r\n\r\n")
            .and_then(|_| self.stream.flush())
            .map_err(|e| format!("fleet agent: ending the body: {e}"))?;
        // Counted from here: the body has ended, so the box's verdict is a moment away or is never
        // coming, and `stall` is exactly the patience that question deserves.
        let reply = read_reply(&mut self.stream, Deadline::of(self.stall)).map_err(|f| f.why)?;
        match reply.status {
            200 if reply.exit == 0 => Ok(()),
            // The box's own words, always: "No space left on device" is the entire content of a
            // failed write, and reporting "exited 1" instead is how that reached nobody before.
            200 => Err(if reply.err.trim().is_empty() {
                format!("fleet agent: the write exited {}", reply.exit)
            } else {
                reply.err.trim().to_string()
            }),
            504 => Err("fleet agent: the write did not finish in time".into()),
            _ => Err(format!(
                "fleet agent: the write was refused ({}) {}",
                reply.status,
                String::from_utf8_lossy(&reply.out).trim()
            )),
        }
    }
}

/// Base64 for the placement header. The other direction of [`decode_b64`], and standard alphabet
/// with padding, because the peer is Python's `base64.b64decode`, which requires it.
fn encode_b64(raw: &[u8]) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(raw.len().div_ceil(3) * 4);
    for group in raw.chunks(3) {
        let mut bits = 0u32;
        for (i, byte) in group.iter().enumerate() {
            bits |= (*byte as u32) << (16 - 8 * i);
        }
        for i in 0..4 {
            if i <= group.len() {
                out.push(ALPHABET[(bits >> (18 - 6 * i)) as usize & 0x3f] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// Just enough base64 to read the agent's stderr header back. Standard alphabet, padding tolerated,
/// anything unrecognised dropped — this decodes an error message, so a malformed one must degrade to
/// a poor message rather than to a failure that hides the error it was carrying.
fn decode_b64(s: &str) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut bits: u32 = 0;
    let mut have = 0;
    let mut out = Vec::new();
    for byte in s.bytes() {
        let Some(value) = ALPHABET.iter().position(|c| *c == byte) else {
            continue;
        };
        bits = (bits << 6) | value as u32;
        have += 6;
        if have >= 8 {
            have -= 8;
            out.push((bits >> have) as u8);
        }
    }
    String::from_utf8_lossy(&out).into_owned()
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

impl Place {
    /// The argv that runs `script` in this place.
    ///
    /// Its own function so the wire format is testable without a sandbox — and because it is the
    /// contract the takeover guard asserts.
    pub fn exec_argv(&self, script: &str) -> Vec<String> {
        if let Some(refusal) = self.unreachable_from_fleet() {
            return refusal;
        }
        let mut argv = self.reach(&[]);
        argv.extend(self.enter());
        argv.push("bash".into());
        argv.push("-lc".into());
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

    /// The `nsenter` hop that puts a command inside this box's namespace — empty when the sandbox
    /// is the box, which is what keeps the original model byte-for-byte unchanged.
    fn enter(&self) -> Vec<String> {
        match &self.at {
            Where::SandboxItself => vec![],
            // A shell rather than a bare `nsenter`, because the check has to happen in the process
            // that crosses. `"$@"` carries whatever the caller appends through untouched, so this
            // stays an argv splice and nothing gets re-quoted on the way in.
            // An address that cannot be proved builds no `nsenter` at all, rather than one behind
            // a check. Nothing then has to hold for the refusal to hold.
            Where::Shared { .. } if !self.provable() => {
                vec!["bash".into(), "-c".into(), self.guard(), "bash".into()]
            }
            Where::Shared { .. } => vec![
                "bash".into(),
                "-c".into(),
                format!("{}exec {} -- \"$@\"", self.guard(), self.nsenter()),
                "bash".into(),
            ],
        }
    }

    /// How skein gets to the **sandbox**, before [`Self::enter`] gets it to the box.
    ///
    /// Two hops, and only the first of them depends on where skein is running. Host-driven it is
    /// `sbx exec [flags] <sandbox>`; in-fleet it is nothing at all, because skein is already there —
    /// and `sbx` is host-only, so it is not merely unnecessary but unavailable.
    ///
    /// One function rather than the same three lines in four builders, because "which hops does a
    /// crossing have" is exactly the sort of thing that gets answered differently in one place after
    /// somebody changes the other three.
    fn reach(&self, flags: &[&str]) -> Vec<String> {
        if crate::deployment::in_fleet() {
            return Vec::new();
        }
        let mut argv = vec!["sbx".to_string(), "exec".into()];
        argv.extend(flags.iter().map(|f| f.to_string()));
        argv.push(self.sandbox.clone());
        argv
    }

    /// **In-fleet, a sandbox that is not the one this process is standing in cannot be reached at
    /// all** — so an address for one is refused rather than aimed.
    ///
    /// That is the whole invariant, and it is about *which sandbox*, not about what kind of box
    /// once lived in it. Found by writing [`Self::reach`] rather than by planning. A crossing has
    /// two hops: `sbx exec` to the sandbox, then `nsenter` to the box inside it. In-fleet the first
    /// is dropped, correctly — skein is already in the sandbox, and `sbx` is host-only, so it is
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
    /// Dropping both hops is exactly right there. No `sbx exec` because skein is already inside,
    /// and no `nsenter` because the address is a sandbox rather than a box. The command runs on the
    /// machine it was addressed to, which is the whole test.
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
        (no_hop_inside && !the_one_we_are_in && crate::deployment::in_fleet()).then(|| {
            vec![
                "sh".to_string(),
                "-c".into(),
                format!(
                    "echo 'skein: {sandbox} is not the sandbox this skein is running inside, and \
                     sbx is host-only — there is no sbx here to reach another sandbox with. Drive \
                     it from a skein on the host, or move its work into this fleet.' >&2; exit 1",
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

    /// The whole crossing as one shell string — what the fleet agent is sent.
    ///
    /// The agent used to be handed the *address* and build the `nsenter` itself, so that a host
    /// newer than the agent could not hand it a command shape it did not understand. That reasoning
    /// holds for shapes and not for this: every agent ever installed runs `bash -lc <script>`, so
    /// sending the crossing as a script is understood by all of them — and it means an agent too
    /// old to know about anchor checks cannot cross without one, which sending the address would
    /// have let it do silently.
    fn crossing(&self, script: &str) -> String {
        match &self.at {
            Where::SandboxItself => script.to_string(),
            Where::Shared { .. } if !self.provable() => self.guard(),
            Where::Shared { .. } => format!(
                "{}exec {} -- bash -lc {}",
                self.guard(),
                self.nsenter(),
                sh_quote(&self.wrap(script))
            ),
        }
    }

    /// Put the script where it expects to be: at the repo root, with the box's own HOME.
    ///
    /// `nsenter` carries the *caller's* environment and working directory into the namespace, so
    /// neither is inherited from the box. A script that assumed it started at the tree root would
    /// otherwise run somewhere arbitrary, and one reading `~/.config/sync/env` would read skein's.
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
                "export HOME={} SKEIN_BOX={} && cd {} && {script}",
                sh_quote(home),
                sh_quote(&self.name),
                sh_quote(tree)
            ),
        }
    }

    /// The **whole** argv for an interactive attach — a terminal, not a captured command.
    ///
    /// This used to return everything *after* the program name, because both callers handed a
    /// literal `"sbx"` to a PTY spawner. That was kept deliberately, on the grounds that changing it
    /// would touch the terminal plumbing on both ends "for no behavioural gain". There is one now:
    /// in-fleet the program is not `sbx` at all, and a builder that returns arguments for a program
    /// it does not name cannot say so.
    ///
    /// The whole attach runs inside the namespace, not just the tmux call. The shell it carries
    /// refreshes the runtime's instruction file, runs the runtime's setup and starts the pane
    /// observer — all of which read and write the box's own HOME and tree. Outside the hop they
    /// would quietly operate on skein's.
    pub fn interactive_argv(&self, script: &str) -> Vec<String> {
        if let Some(refusal) = self.unreachable_from_fleet() {
            return refusal;
        }
        let mut argv = self.reach(&["-it"]);
        argv.extend(self.enter());
        argv.push("bash".into());
        argv.push("-lc".into());
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
        let mut argv = self.reach(&[]);
        argv.extend(self.enter());
        argv.extend(args.iter().map(|a| a.to_string()));
        argv
    }

    /// The JSON `fleet-agent.py` expects — the placement, not a pre-built shell.
    ///
    /// Sending the *address* rather than the argv is deliberate. The agent lives inside the sandbox
    /// and therefore outlives the skein that installed it, so a host newer than the agent must not
    /// be able to hand it a command shape it does not understand. That is not a hypothetical
    /// failure: a launcher older than the skein driving it is what took the whole fleet down once.
    /// Fields the agent does not recognise are ignored; a field it needs and does not get makes the
    /// request fail loudly, and the caller falls back to `sbx exec`.
    fn agent_request(&self, script: &str, timeout: Duration) -> serde_json::Value {
        // The crossing goes over as a *script*, not as an address.
        //
        // This reverses an earlier decision, and the reason it reverses is version skew rather than
        // taste. Sending the address let the agent build the `nsenter` itself, so a host newer than
        // the agent could not hand it a command shape it did not understand — sound, for shapes.
        // But an anchor check is not a shape: an agent that predates it would ignore the two fields
        // that carry the proof and cross anyway, and skein would have no way to tell. Sending the
        // whole crossing means an old agent cannot cross unchecked, because the check is inside the
        // only thing it is given. Every agent ever installed runs `bash -lc <script>`.
        serde_json::json!({
            "script": self.crossing(script),
            "timeout": timeout.as_secs_f64(),
        })
    }

    /// Run `script` through the held connection, or `None` when there is no usable agent.
    ///
    /// The `Option`/`Result` nesting is the whole contract and is not decoration:
    ///
    /// - `None` — the *transport* was unavailable (not configured, no token, connection refused,
    ///   the agent answered 5xx). The script never ran, so the caller may safely try `sbx exec`.
    /// - `Some(Err)` — the script ran and *failed*. The caller must not retry: half of what skein
    ///   sends a box has a side effect, and a fallback here would apply it twice.
    ///
    /// Getting that distinction backwards is the one way this can be worse than no transport at all.
    fn via_agent(&self, script: &str, timeout: Duration) -> Option<Result<Vec<u8>, String>> {
        let (port, token) = agent_target()?;
        let body = serde_json::to_vec(&self.agent_request(script, timeout)).ok()?;
        // A generous margin over the script's own budget: the agent enforces `timeout` itself and
        // answers 504, and a socket deadline that fired first would turn its answer into silence.
        let reply = agent_post(port, &token, &body, timeout + Duration::from_secs(5)).ok()?;
        match reply.status {
            200 if reply.exit == 0 => Some(Ok(reply.out)),
            200 => Some(Err(if reply.err.trim().is_empty() {
                format!("fleet agent: command exited {}", reply.exit)
            } else {
                reply.err.trim().to_string()
            })),
            // The agent's own timeout. The script *started*, so this is not a fallback case however
            // much it looks like one: re-running it on `sbx exec` would apply any side effect twice.
            //
            // **And say when the agent is older than this build**, because that changes what the
            // reader should do about it. A timeout from a current agent is a slow script; a timeout
            // from a stale one is a transport that should have been replaced and was not, and it
            // blocks every box start with no fallback by design. Without this the message is the
            // same in both cases and only one of them has an action.
            // The agent's body carries the budget that actually expired and the tail of what the
            // script had printed before the kill. Both were thrown away here, and the cost was
            // real: "the command did not finish in time" is the same sentence whichever binary you
            // are running and whichever stage died, so diagnosing one meant reading the provisioning
            // script end to end. An older agent says only the first half, and that is fine — this
            // prints whatever it was told rather than deciding what it should have been told.
            504 => Some(Err({
                let said = String::from_utf8_lossy(&reply.out).trim().to_string();
                let detail = match said.is_empty() {
                    true => String::new(),
                    false => format!(" ({said})"),
                };
                match agent_protocol(port) {
                    Some(speaks) if speaks < AGENT_PROTOCOL => format!(
                        "fleet agent: the command did not finish in time{detail} — and this agent \
                         speaks v{speaks} where this build needs v{AGENT_PROTOCOL}. A stale agent \
                         takes the call and cannot hand it back, so nothing falls back to `sbx \
                         exec`. Restart skein-server: that retires it and installs the current one. \
                         If it still will not come up, set \"fleet_agent\": false in \
                         ~/.skein/config.json and restart — every call then goes over `sbx exec`, \
                         which is what it did before the agent existed."
                    ),
                    _ => format!("fleet agent: the command did not finish in time{detail}"),
                }
            })),
            // 400/403/404/5xx — the agent refused or broke before running anything, so the script
            // never started and `sbx exec` is free to try it.
            _ => None,
        }
    }

    fn command(&self, script: &str) -> Command {
        let argv = self.exec_argv(script);
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

    /// Run `script` and return its stdout as **raw bytes**.
    ///
    /// Separate from [`Place::exec`] because a lossy UTF-8 hop corrupts every image and PDF the
    /// Files tab serves — the bug is silent and the file merely looks broken.
    /// Run `script` and report what happened, rather than whether it worked.
    ///
    /// `Err` means it did not run at all — the sandbox was unreachable, or it outlived `timeout`.
    /// Any exit code is `Ok`, because "it ran and said no" is an answer, and only the caller knows
    /// what to make of it. See [`Ran`].
    pub fn attempt(&self, script: &str, timeout: Duration) -> Result<Ran, String> {
        if let Some(answered) = self.attempt_via_agent(script, timeout) {
            return answered;
        }
        let mut command = self.command(script);
        // `output_with_timeout_why`, not `bounded_output`: the second says "sbx exec failed to
        // start or exceeded the 30s timeout" for both, and this is the one caller where the
        // difference is the whole answer. A model call that never left the host was reported to a
        // person as "skein could not start `claude` … set SKEIN_CLAUDE_BIN to its full path", for a
        // host where `claude` was fine and `sbx` was missing. The message it gets instead names the
        // program that failed AND the PATH skein had, which is the one fact the reader cannot
        // recover afterwards — by the time they look, they are looking at their shell's PATH.
        let out = crate::util::output_with_timeout_why(&mut command, timeout)?;
        Ok(Ran {
            code: out.status.code().unwrap_or(-1),
            out: out.stdout,
            err: String::from_utf8_lossy(&out.stderr).trim().to_string(),
        })
    }

    /// `None` when the agent is not there or refused before running anything, which is the one case
    /// where falling through to `sbx exec` is safe.
    fn attempt_via_agent(&self, script: &str, timeout: Duration) -> Option<Result<Ran, String>> {
        let (port, token) = agent_target()?;
        let body = serde_json::to_vec(&self.agent_request(script, timeout)).ok()?;
        let reply = agent_post(port, &token, &body, timeout + Duration::from_secs(5)).ok()?;
        match reply.status {
            200 => Some(Ok(Ran {
                code: reply.exit,
                out: reply.out,
                err: reply.err.trim().to_string(),
            })),
            // The agent's own timeout. The script STARTED, so this must not fall through to
            // `sbx exec`: re-running it would apply any side effect twice.
            504 => Some(Err(format!(
                "the command did not finish in time ({})",
                String::from_utf8_lossy(&reply.out).trim()
            ))),
            _ => None,
        }
    }

    pub fn bytes(&self, script: &str, timeout: Duration) -> Result<Vec<u8>, String> {
        // The agent first when there is one, `sbx exec` when there is not — and `sbx exec` is what
        // every fleet has until someone configures a port, so this changes nothing by upgrading.
        if let Some(answered) = self.via_agent(script, timeout) {
            return answered;
        }
        self.bytes_via_sbx(script, timeout)
    }

    /// Run `script` on `sbx exec`, whatever the agent's state — the one call that must not use it.
    ///
    /// For the handful of commands whose *subject* is the agent: restarting a stale one cannot be
    /// sent through the connection it is about to kill, or the answer dies with the process and a
    /// successful restart reports as a failure.
    pub fn exec_sbx(&self, script: &str, timeout: Duration) -> Result<String, String> {
        let out = self.bytes_via_sbx(script, timeout)?;
        Ok(String::from_utf8_lossy(&out).into_owned())
    }

    fn bytes_via_sbx(&self, script: &str, timeout: Duration) -> Result<Vec<u8>, String> {
        let mut command = self.command(script);
        let out = bounded_output(&mut command, "sbx exec", timeout)?;
        if !out.status.success() {
            let detail = String::from_utf8_lossy(&out.stderr).trim().to_string();
            return Err(if detail.is_empty() {
                format!("sbx exec exited {}", out.status)
            } else {
                detail
            });
        }
        Ok(out.stdout)
    }

    /// The argv that runs `script` here with stdin attached. `-i` is not decoration: without it
    /// `sbx exec` does not wire a pipe to the guest, and the body is silently discarded.
    pub fn write_argv(&self, script: &str) -> Vec<String> {
        if let Some(refusal) = self.unreachable_from_fleet() {
            return refusal;
        }
        let mut argv = self.reach(&["-i"]);
        argv.extend(self.enter());
        argv.push("bash".into());
        argv.push("-lc".into());
        argv.push(self.wrap(script));
        argv
    }

    /// Begin a streamed write here, or `None` when there is no agent to stream to.
    ///
    /// The handle is for callers that have the body arriving in pieces rather than in hand — an
    /// upload off a browser socket. Callers holding the whole thing want [`Place::write`], which is
    /// this with the pieces filled in.
    ///
    /// `timeout` is how long the script may run in the box; `stall` is how long the host will wait
    /// on a socket carrying nothing. See [`AgentWrite::begin`] for why they are separate.
    pub fn begin_write(
        &self,
        script: &str,
        timeout: Duration,
        stall: Duration,
    ) -> Option<AgentWrite> {
        AgentWrite::begin(self, script, timeout, stall)
    }

    /// Run `script` with `body` on its **stdin**.
    ///
    /// The only way skein sends a box anything sensitive, and that predates the agent: `sbx exec`'s
    /// argv is visible in `ps` on the host, so a token passed as an argument is a token in every
    /// process listing and every shell history. The agent keeps that property — the body is the
    /// request's body, never an argument — and adds the one this path was missing, which is that it
    /// still answers when `sbx exec` has stopped.
    ///
    /// That mattered more than it looked: every path that *installs* something in a box goes
    /// through here, so leaving it on `sbx exec` meant starting a box still needed the daemon two
    /// or three times however healthy the transport was.
    pub fn write(&self, script: &str, body: &[u8], timeout: Duration) -> Result<(), String> {
        // Over the cap it is `sbx exec -i`, which streams from a pipe and has no ceiling at all.
        // One budget for both here, which is today's behaviour kept deliberately: the caller holds
        // the whole body, so there is no streaming for a stall to be measured against, and the
        // scripts that come through here install things and may legitimately be quiet while they
        // run. The upload path is the one that streams, and it passes its own.
        if body.len() as u64 <= AGENT_WRITE_CAP {
            if let Some(mut write) = self.begin_write(script, timeout, timeout) {
                // No `?`-then-fall-back here, deliberately: the script is already running in the
                // box, so a failure is the box's answer and not a reason to send it twice.
                write.push(body)?;
                return write.finish();
            }
        }
        let argv = self.write_argv(script);
        let mut child = Command::new(&argv[0])
            .args(&argv[1..])
            // Nothing reads stdout here, and an unread pipe blocks the child once its buffer fills
            // (~64KB) — a chatty command would look like a hang until the deadline killed it.
            .stdout(Stdio::null())
            .stdin(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("sbx exec: {e}"))?;
        // Drained on a thread for the same reason, and kept: this used to pipe stderr and never read
        // it, so every failure here reported a bare `sbx exec exited 1` with the cause discarded.
        let errors = child.stderr.take().map(|mut pipe| {
            std::thread::spawn(move || {
                let mut buf = String::new();
                let _ = pipe.read_to_string(&mut buf);
                buf
            })
        });
        // Written on its own thread, and that is not symmetry with the stderr drain — it is the one
        // way this call has a deadline at all. A pipe holds ~64KB; past that `write_all` blocks
        // until the guest reads, and the guest is `cat` in a sandbox that may be exactly the thing
        // that has stopped answering. The deadline below starts *after* this returns, so a blocking
        // write was an unbounded wait no timeout covered — with an `sbx exec` held open for its
        // whole duration. Every install skein does goes through here, so a sandbox that went quiet
        // took the caller with it.
        let mut pipe = child.stdin.take().ok_or("sbx exec: no stdin")?;
        let body = body.to_vec();
        let writing = std::thread::spawn(move || {
            pipe.write_all(&body)
                .and_then(|()| pipe.flush())
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
                    // Killed AND reaped. A kill without a wait leaves a zombie per timed-out write,
                    // and this is the path a struggling fleet takes over and over.
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(format!(
                        "sbx exec did not finish within {}s — the body was {}",
                        timeout.as_secs(),
                        match writing.is_finished() {
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

#[cfg(test)]
mod tests {
    use super::*;

    /// An empty piece must not end the body, because ending it truncates the file in silence.
    ///
    /// A zero-length chunk is how chunked encoding spells "that was the last one". A body stream
    /// yields one readily — a flush boundary, a keep-alive frame — and forwarding it would close the
    /// write mid-upload: the file lands in the box, shorter than it should be, and nothing says so.
    /// The guard was there and nothing tested it: removing it broke no test at all.
    #[test]
    fn an_empty_piece_is_not_the_end_of_the_body() {
        assert_eq!(chunk_frame(b""), None, "an empty piece would end the body");
        assert_eq!(
            chunk_frame(b"hello"),
            Some(b"5\r\nhello\r\n".to_vec()),
            "the length is hex and the piece is framed by CRLFs"
        );
        // Sixteen bytes is `10` in hex, not `16` — decimal here would desynchronise the whole body
        // and the box would read the next frame's header as content.
        let sixteen = vec![b'x'; 16];
        let framed = chunk_frame(&sixteen).unwrap();
        assert!(
            framed.starts_with(b"10\r\n"),
            "the chunk length must be hex: {:?}",
            String::from_utf8_lossy(&framed[..4])
        );
        assert_eq!(framed.len(), 4 + 16 + 2);
    }
    use crate::testutil::*;

    /// A guest that never reads its stdin must time out, not hang for ever.
    ///
    /// A pipe holds about 64KB. Past that `write_all` blocks until something on the other end reads,
    /// and the deadline in `Place::write` only started *after* the write returned — so a body larger
    /// than the pipe, sent to a sandbox that had stopped answering, was an unbounded wait that no
    /// timeout covered, holding an `sbx exec` open for its whole duration. Every install skein does
    /// goes through this call, including the ones at server start.
    ///
    /// Driven with a fake `sbx` that never reads stdin, and a body far larger than the buffer.
    #[test]
    fn a_write_to_a_box_that_never_reads_it_gives_up_instead_of_hanging() {
        use std::os::unix::fs::PermissionsExt;
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        std::env::set_var("SKEIN_HOME", home.as_ref() as &std::path::Path);

        let bin = home.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let fake = bin.join("sbx");
        // Never reads stdin, never exits on its own: the sandbox that has gone quiet.
        std::fs::write(&fake, "#!/bin/sh\nsleep 60\n").unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        let path = std::env::var("PATH").unwrap_or_default();
        std::env::set_var("PATH", format!("{}:{path}", bin.display()));

        let place = crate::place::own_sandbox("skein-fleet");
        let body = vec![b'x'; 1 << 20]; // 1 MB — sixteen times the pipe
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

        std::env::set_var("PATH", path);
        std::env::remove_var("SKEIN_HOME");
    }

    /// The whole justification for a 30-second connect budget is that a dead port never spends it.
    ///
    /// If that is wrong, every call into a fleet whose agent is not installed — which is the state
    /// skein degrades to, and the state it must stay usable in — pays half a minute before falling
    /// back to `sbx exec`, and the board becomes unusable exactly when it is already unwell. The
    /// kernel refuses a connection to a closed local port outright, and this asserts that skein is
    /// relying on a refusal rather than on a timeout.
    #[test]
    fn nothing_listening_costs_nothing_however_long_the_budget_is() {
        // Bound then dropped: the OS gave us this number, so nothing else is on it, and by the time
        // the probe runs the listener is gone.
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .and_then(|l| l.local_addr())
            .map(|a| a.port())
            .expect("a free port");

        let started = std::time::Instant::now();
        assert_eq!(agent_protocol(port), None, "nothing is listening there");
        let spent = started.elapsed();
        assert!(
            spent < Duration::from_secs(5),
            "a closed port must be refused, not waited out — took {spent:?} of a {AGENT_CONNECT:?} \
             budget, so every call in a fleet with no agent would pay it"
        );
    }

    /// Drives the **real** `fleet-agent.py` over the **real** client, because the thing worth
    /// testing here is the wire, and a stubbed transport would agree with whatever the client
    /// happened to send. Python is present wherever this runs for the same reason it is present in
    /// the sandbox.
    struct RunningAgent {
        child: std::process::Child,
        port: u16,
    }

    impl Drop for RunningAgent {
        fn drop(&mut self) {
            let _ = self.child.kill();
            let _ = self.child.wait();
            // The idle connections outlive this process otherwise, and the next test to use the
            // transport would reuse a socket whose peer is gone.
            IDLE.lock().unwrap_or_else(|e| e.into_inner()).clear();
        }
    }

    /// Start the agent on a free port with `token`, and wait for it to actually answer.
    fn start_agent(home: &std::path::Path, token: &str) -> RunningAgent {
        let port = std::net::TcpListener::bind(("127.0.0.1", 0))
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let token_file = home.join("fleet-agent.token");
        fs::write(&token_file, token).unwrap();
        let script = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("src")
            .join("fleet-agent.py");
        let child = Command::new("python3")
            .arg(&script)
            .arg(port.to_string())
            .arg(&token_file)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("python3 to start the agent");
        let agent = RunningAgent { child, port };
        // Poll rather than sleep a guessed interval: a fixed wait is either flaky or slow.
        for _ in 0..100 {
            if TcpStream::connect(("127.0.0.1", port)).is_ok() {
                return agent;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        panic!("agent never listened on {port}");
    }

    /// Turn the transport on and tell it which port is *verified* — the two the client reads.
    fn point_config_at(home: &std::path::Path, port: u16) {
        let config = serde_json::json!({ "fleet_agent": true });
        fs::write(home.join("config.json"), config.to_string()).unwrap();
        record_agent_port(port);
    }

    #[test]
    fn without_an_agent_every_call_takes_the_sbx_path() {
        let _g = env_lock();
        let home = tempdir();
        std::env::set_var("SKEIN_HOME", home.as_ref() as &std::path::Path);
        // Switched off: the transport declines before it looks at anything else. This is the
        // default, so it is also the assertion that upgrading changes nothing.
        assert!(own_sandbox("fleet")
            .via_agent("echo hi", Duration::from_secs(5))
            .is_none());
        // On, but no port has been verified yet — publishing may not have happened or may have
        // failed. Not a reason to fail the call; a reason to use `sbx exec`.
        fs::write(
            home.join("config.json"),
            serde_json::json!({ "fleet_agent": true }).to_string(),
        )
        .unwrap();
        assert!(own_sandbox("fleet")
            .via_agent("echo hi", Duration::from_secs(5))
            .is_none());
        // A port but no token is the same answer — skein must not fall through to an unauthenticated
        // request, and must not fail the call either.
        point_config_at(&home, 1);
        assert!(own_sandbox("fleet")
            .via_agent("echo hi", Duration::from_secs(5))
            .is_none());
        // A port nothing is listening on: still "never ran", so the caller may retry on sbx.
        fs::write(home.join("fleet-agent.token"), "t").unwrap();
        assert!(own_sandbox("fleet")
            .via_agent("echo hi", Duration::from_secs(5))
            .is_none());
    }

    #[test]
    fn the_token_is_generated_once_and_kept_out_of_reach() {
        let _g = env_lock();
        let home = tempdir();
        std::env::set_var("SKEIN_HOME", home.as_ref() as &std::path::Path);

        let first = ensure_agent_token().expect("a token to be generated");
        assert_eq!(first.len(), 64, "32 bytes, hex: {first}");
        assert!(first.chars().all(|c| c.is_ascii_hexdigit()));

        // Kept, not rotated: rotating on a reinstall would invalidate the token of a sandbox that
        // is up and serving, at the moment skein is least able to push a new one in.
        assert_eq!(ensure_agent_token().unwrap(), first);
        assert_eq!(agent_token().as_deref(), Some(first.as_str()));

        // Host-private, and readable by nobody else: this authorises running commands as the
        // sandbox in any box's namespace.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(home.join("fleet-agent.token"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o077, 0, "token is group/world readable: {mode:o}");
        }

        // A blank token is not a token. Left as one, skein would offer it to the agent and the
        // agent would refuse — but the interesting half is that a *fresh* one gets generated.
        fs::write(home.join("fleet-agent.token"), "   \n").unwrap();
        assert!(agent_token().is_none());
        assert_ne!(ensure_agent_token().unwrap(), first);
    }

    #[test]
    fn the_agent_runs_the_script_and_tells_the_two_kinds_of_failure_apart() {
        let _g = env_lock();
        let home = tempdir();
        std::env::set_var("SKEIN_HOME", home.as_ref() as &std::path::Path);
        let agent = start_agent(&home, "s3cret");
        point_config_at(&home, agent.port);
        let fleet = own_sandbox("fleet");

        // stdout comes back whole, and as bytes — the Files tab serves images through this path.
        let out = fleet
            .via_agent("printf 'hi\\n'", Duration::from_secs(10))
            .expect("the agent to answer")
            .expect("the script to succeed");
        assert_eq!(out, b"hi\n");

        // Raw bytes survive: a lossy UTF-8 hop here is the bug that made every PDF look broken.
        let raw = fleet
            .via_agent("printf '\\xff\\xfe'", Duration::from_secs(10))
            .unwrap()
            .unwrap();
        assert_eq!(raw, vec![0xff, 0xfe]);

        // A script that FAILS is `Some(Err)` — never `None` — or the caller would run it again on
        // sbx and apply its side effect twice. The box's own words come back, not "exited 1".
        let failed = fleet
            .via_agent(
                "echo 'the box said this' >&2; exit 3",
                Duration::from_secs(10),
            )
            .expect("the agent to answer")
            .expect_err("the script to fail");
        assert!(failed.contains("the box said this"), "{failed}");

        // A script that OUTRUNS its budget also ran, so it is a failure and not a fallback.
        let timed_out = fleet
            .via_agent("sleep 5", Duration::from_millis(300))
            .expect("the agent to answer")
            .expect_err("the script to time out");
        assert!(timed_out.contains("did not finish"), "{timed_out}");

        // A wrong token is the agent refusing before it runs anything, so sbx may still try.
        fs::write(home.join("fleet-agent.token"), "wrong").unwrap();
        assert!(fleet.via_agent("echo hi", Duration::from_secs(5)).is_none());
    }

    #[test]
    fn the_connection_is_reused_and_survives_the_agent_closing_it() {
        let _g = env_lock();
        let home = tempdir();
        std::env::set_var("SKEIN_HOME", home.as_ref() as &std::path::Path);
        let agent = start_agent(&home, "s3cret");
        point_config_at(&home, agent.port);
        let fleet = own_sandbox("fleet");

        for i in 0..5 {
            let out = fleet
                .via_agent(&format!("echo {i}"), Duration::from_secs(10))
                .unwrap()
                .unwrap();
            assert_eq!(out, format!("{i}\n").into_bytes());
        }
        // The held socket is the point of the whole transport: five sequential calls hand the one
        // connection back and forth, so after them the idle set holds exactly one, not five.
        assert_eq!(
            idle_for(agent.port),
            1,
            "five calls in series must reuse one connection, not open five"
        );

        // Now sever it the way a restarting agent or an idle reaper would. The next call must
        // reconnect rather than report a failure the caller would read as "the sandbox is gone".
        {
            let idle = IDLE.lock().unwrap_or_else(|e| e.into_inner());
            for (_, dead) in idle.iter() {
                let _ = dead.shutdown(std::net::Shutdown::Both);
            }
        }
        let after = fleet
            .via_agent("echo back", Duration::from_secs(10))
            .expect("a severed connection to be re-established")
            .unwrap();
        assert_eq!(after, b"back\n");
    }

    /// An `sbx` on PATH that fails loudly, so a test can tell "the agent carried it" from "it
    /// A timeout carries what the script had already printed, so it says how far it got.
    ///
    /// Drives the real agent with a script that reports a stage and then hangs — which is the shape
    /// of the failure this exists for. `skein-startup.sh` prints a `[skein-kit] …` line at every
    /// stage it passes, and a provisioning timeout used to arrive as one sentence with none of them
    /// in it: "fleet agent: the command did not finish in time", identical whichever stage died and
    /// whichever budget expired. Finding out which cost an end-to-end read of the script and two
    /// queries against a live fleet's API, for an answer the killed process had already written down.
    #[test]
    fn a_timeout_carries_what_the_script_managed_to_say_before_it_was_killed() {
        let _g = env_lock();
        let home = tempdir();
        std::env::set_var("SKEIN_HOME", home.as_ref() as &std::path::Path);
        let agent = start_agent(home.as_ref() as &std::path::Path, "tok");
        point_config_at(home.as_ref() as &std::path::Path, agent.port);

        let why = own_sandbox("fleet")
            .via_agent(
                "echo '[skein-kit] linked the store'; echo '[skein-kit] wiring the tracker' >&2; \
                 sleep 30",
                Duration::from_secs(1),
            )
            .expect("the agent answered")
            .expect_err("a script that outlives its budget is an error");

        assert!(
            why.contains("did not finish in time"),
            "the timeout stopped saying what it was: {why}"
        );
        // Both streams. The stage lines a script prints go to whichever it happens to use, and a
        // message that carries only one of them is a message that is empty half the time.
        assert!(
            why.contains("linked the store") && why.contains("wiring the tracker"),
            "the timeout dropped what the script had printed before the kill, which is the only \
             account of how far it got: {why}"
        );
        // And the budget that actually expired, because "did not finish in time" is the same
        // sentence whether the caller allowed five minutes or fifteen.
        assert!(
            why.contains('1'),
            "the timeout does not say which budget expired: {why}"
        );
    }

    /// A timeout from a STALE agent says what to do; one from a current agent has nothing to add.
    ///
    /// The two look identical and only one of them has an action. A stale agent takes the call and
    /// cannot hand it back — the 504 deliberately does not fall back, since the script started and
    /// re-running it would apply side effects twice — so every box start blocks on a transport that
    /// should have been replaced. Reported from a live fleet as "restart never really completes",
    /// with `agent v2 … this build needs v3` sitting unread on the board the whole time.
    #[test]
    fn a_timeout_from_an_agent_older_than_this_build_says_so_and_says_what_to_do() {
        let _g = crate::testutil::env_lock();
        // An agent that accepts, reports an old protocol, and then times out — which is the shape
        // that produced the report.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for mut stream in listener.incoming().flatten() {
                use std::io::{Read, Write};
                let mut raw = [0u8; 8192];
                let read = stream.read(&mut raw).unwrap_or(0);
                let asked = String::from_utf8_lossy(&raw[..read]).to_string();
                // `/health` carries the protocol; anything else is the call, and it times out.
                // `skein-fleet-agent <version>`, plain text and space-separated — the shape
                // `health_over` parses. JSON here would read as a dead agent rather than an old one,
                // which is the distinction this test is about.
                let (code, body) = match asked.contains("/health") {
                    true => (200, format!("skein-fleet-agent {}", AGENT_PROTOCOL - 1)),
                    false => (504, String::new()),
                };
                let _ = stream.write_all(
                    format!(
                        "HTTP/1.1 {code} X\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    )
                    .as_bytes(),
                );
            }
        });

        let stale = agent_protocol(port);
        assert_eq!(
            stale,
            Some(AGENT_PROTOCOL - 1),
            "the fixture is not standing in for an old agent: {stale:?}"
        );
    }

    /// quietly fell back". Returns the previous PATH for the caller to put back.
    fn sbx_must_not_be_used(home: &std::path::Path) -> String {
        use std::os::unix::fs::PermissionsExt;
        let bin = home.join("bin");
        fs::create_dir_all(&bin).unwrap();
        let fake = bin.join("sbx");
        fs::write(
            &fake,
            "#!/bin/sh\ncat >/dev/null\necho 'sbx was used' >&2\nexit 1\n",
        )
        .unwrap();
        fs::set_permissions(&fake, fs::Permissions::from_mode(0o755)).unwrap();
        let path = std::env::var("PATH").unwrap_or_default();
        std::env::set_var("PATH", format!("{}:{path}", bin.display()));
        path
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
    #[test]
    fn the_agent_keeps_the_connection_open_between_calls() {
        let _g = env_lock();
        let home = tempdir();
        std::env::set_var("SKEIN_HOME", home.as_ref() as &std::path::Path);
        let agent = start_agent(&home, "s3cret");
        point_config_at(&home, agent.port);

        let fleet = own_sandbox("fleet");
        fleet
            .via_agent("true", Duration::from_secs(10))
            .unwrap()
            .unwrap();

        // Take the held socket and use it directly. `agent_post`'s reconnect is what masks a server
        // that hung up, so this deliberately goes without it: if the agent closed after the first
        // response, this second exchange on the same socket fails.
        let (_, mut held) = IDLE
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .pop()
            .expect("a connection to have been kept");
        let body =
            serde_json::to_vec(&fleet.agent_request("echo still-here", Duration::from_secs(10)))
                .unwrap();
        let reply = agent_exchange(
            &mut held,
            "s3cret",
            &body,
            Deadline::of(Duration::from_secs(10)),
        )
        .expect("the very same connection to still be usable");
        assert_eq!(reply.out, b"still-here\n");
    }

    // Everything skein installs in a box goes through `write`, so leaving it on `sbx exec` meant
    // starting a box still needed the daemon two or three times however healthy the transport was.
    #[test]
    fn a_body_is_streamed_into_the_box_and_arrives_whole() {
        let _g = env_lock();
        let home = tempdir();
        std::env::set_var("SKEIN_HOME", home.as_ref() as &std::path::Path);
        let agent = start_agent(&home, "s3cret");
        point_config_at(&home, agent.port);
        // From here on, reaching `sbx` at all is a failure with a name.
        let path = sbx_must_not_be_used(&home);
        let fleet = own_sandbox("fleet");
        let target = home.join("landed.bin");
        let script = format!("cat > {}", sh_quote(target.to_str().unwrap()));

        // Bigger than one chunk on either side, and not text: an attachment is a screenshot or a
        // video far more often than it is a file of a's, and a lossy hop anywhere in this path is
        // silent — the file merely looks broken.
        let body: Vec<u8> = (0..300_000u32).map(|i| (i % 251) as u8).collect();
        fleet
            .write(&script, &body, Duration::from_secs(30))
            .unwrap();
        assert_eq!(fs::read(&target).unwrap(), body);

        // The same file arriving in pieces — which is how every upload actually arrives, off a
        // browser socket a chunk at a time. `write` sends one big piece and would never exercise
        // the chunk framing between them: a missing CRLF or a miscounted length shows up only here,
        // and shows up as a file that is subtly wrong rather than as an error.
        let mut streamed = fleet
            .begin_write(&script, Duration::from_secs(30), Duration::from_secs(30))
            .expect("the agent to take a streamed write");
        for piece in body.chunks(9_973) {
            streamed.push(piece).unwrap();
        }
        streamed.finish().unwrap();
        assert_eq!(fs::read(&target).unwrap(), body);

        // An empty body is a real case (a zero-byte file, a cleared credential) and chunked
        // encoding spells the end of a body as a zero-length chunk — so an empty piece must not be
        // sent as one, or the write would end before it began.
        fleet.write(&script, b"", Duration::from_secs(30)).unwrap();
        assert_eq!(fs::read(&target).unwrap(), Vec::<u8>::new());

        // And an empty *piece* mid-stream is the same hazard from the other direction: a body
        // stream may yield one, and it must not be mistaken for the end of the body.
        let mut interrupted = fleet
            .begin_write(&script, Duration::from_secs(30), Duration::from_secs(30))
            .expect("the agent to take a streamed write");
        interrupted.push(b"before").unwrap();
        interrupted.push(b"").unwrap();
        interrupted.push(b"after").unwrap();
        interrupted.finish().unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"beforeafter");

        // A write that fails carries the box's own words back. "No space left on device" is the
        // entire content of a failed write, and "exited 1" is not a substitute for it.
        let refused = fleet
            .write(
                "cat > /proc/definitely/not/here",
                &body,
                Duration::from_secs(30),
            )
            .expect_err("writing into /proc to fail");
        assert!(
            refused.contains("No such file") || refused.contains("Not a directory"),
            "{refused}"
        );

        std::env::set_var("PATH", path);
    }

    // The agent is installed *into* the sandbox and outlives the skein that installed it, so a
    // running agent may be older than the host talking to it. `/write` has to be declined before
    // the body is sent, because an upload's bytes come off a network socket that has already been
    // drained — there is no second copy to fall back with.
    #[test]
    fn an_agent_too_old_for_a_streamed_write_is_declined_before_anything_is_sent() {
        let _g = env_lock();
        let home = tempdir();
        std::env::set_var("SKEIN_HOME", home.as_ref() as &std::path::Path);

        // An agent from before versions existed: it answers `/health` with its name alone, and 404s
        // everything else. Which is exactly what is running in the fleet right now.
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let mut seen = [0u8; 1024];
                let _ = stream.read(&mut seen);
                let _ = stream
                    .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 17\r\n\r\nskein-fleet-agent");
            }
        });
        fs::write(home.join("fleet-agent.token"), "s3cret").unwrap();
        point_config_at(&home, port);

        // It is alive and it is usable — for `/exec`, which it has always had.
        assert_eq!(agent_protocol(port), Some(1));
        assert!(agent_answers(port));
        // But not for a write, and the refusal comes with nothing on the wire, so `sbx exec` is
        // still free to do it.
        assert!(own_sandbox("fleet")
            .begin_write(
                "cat > /tmp/x",
                Duration::from_secs(10),
                Duration::from_secs(10)
            )
            .is_none());
    }

    /// A box that takes the write and never confirms it is given up on at the STALL, not at the
    /// script's own budget (SKEIN-269).
    ///
    /// The two were one number, and one number cannot tell "this upload is legitimately an hour
    /// long" from "this box has stopped answering" — so the second was waited out as though it were
    /// the first, holding the request and the blocking thread carrying it for the whole hour with
    /// nothing said. Here the budget is an hour and the stall is well under a second, and the point
    /// of the test is which of the two the failure arrives on.
    ///
    /// The fake agent answers `/health` as a current one — so the write is accepted, which is what
    /// makes this the stall case rather than the decline case — and then reads the body and says
    /// nothing at all, which is the shape of a box whose child has wedged.
    #[test]
    fn a_write_the_box_never_confirms_ends_at_the_stall_not_at_the_budget() {
        let _g = env_lock();
        let home = tempdir();
        std::env::set_var("SKEIN_HOME", home.as_ref() as &std::path::Path);

        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                std::thread::spawn(move || {
                    // `/health` first, answered as the current protocol so the write is taken.
                    let mut seen = [0u8; 4096];
                    let _ = stream.read(&mut seen);
                    let body = format!("skein-fleet-agent {AGENT_PROTOCOL} test");
                    let _ = stream.write_all(
                        format!(
                            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{body}",
                            body.len()
                        )
                        .as_bytes(),
                    );
                    // Then the write: read whatever arrives and answer none of it. The socket stays
                    // open, which is precisely why a deadline is the only thing that ends this.
                    loop {
                        match stream.read(&mut seen) {
                            Ok(0) | Err(_) => break,
                            Ok(_) => {}
                        }
                    }
                    std::thread::sleep(Duration::from_secs(30));
                });
            }
        });
        fs::write(home.join("fleet-agent.token"), "s3cret").unwrap();
        point_config_at(&home, port);

        let mut write = own_sandbox("fleet")
            .begin_write(
                "cat > /tmp/x",
                Duration::from_secs(3600),
                Duration::from_millis(300),
            )
            .expect("the agent to accept the write");
        write.push(b"some bytes").unwrap();
        let began = std::time::Instant::now();
        let said = write.finish();
        let spent = began.elapsed();

        assert!(
            said.is_err(),
            "a box that never confirms must be a failure, not a success: {said:?}"
        );
        assert!(
            spent < Duration::from_secs(10),
            "the wait must end at the stall (300ms), not at the script's hour-long budget — \
             it took {spent:?}"
        );
    }

    // A write that fails used to report `sbx exec exited 1` and drop the reason on the floor, which
    // is how "mkdir: cannot create directory '/boxes': Permission denied" reached nobody.
    #[test]
    fn a_failed_write_reports_what_the_sandbox_said() {
        use std::os::unix::fs::PermissionsExt;
        let _g = env_lock();
        let home = tempdir();
        // Explicitly, so this exercises the `sbx exec` path whatever another test left behind: with
        // no config there is no agent, which is the default and the pre-agent behaviour.
        std::env::set_var("SKEIN_HOME", home.as_ref() as &std::path::Path);
        let bin = home.join("bin");
        fs::create_dir_all(&bin).unwrap();
        let fake = bin.join("sbx");
        fs::write(
            &fake,
            "#!/bin/sh\ncat >/dev/null\necho \"mkdir: cannot create directory\" >&2\nexit 1\n",
        )
        .unwrap();
        fs::set_permissions(&fake, fs::Permissions::from_mode(0o755)).unwrap();
        let path = std::env::var("PATH").unwrap_or_default();
        std::env::set_var("PATH", format!("{}:{path}", bin.display()));

        let place = Place {
            name: "b".into(),
            sandbox: "fleet".into(),
            at: Where::SandboxItself,
        };
        let err = place
            .write("cat > /boxes/x", b"body", Duration::from_secs(10))
            .unwrap_err();
        assert!(err.contains("cannot create directory"), "{err}");

        std::env::set_var("PATH", path);
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
        std::env::set_var(crate::deployment::IN_FLEET, "1");

        let ours = Place {
            name: "skein-fleet".into(),
            sandbox: "skein-fleet".into(),
            at: Where::SandboxItself,
        };
        assert_eq!(
            ours.exec_argv("echo hi"),
            ["bash", "-lc", "echo hi"],
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

        std::env::remove_var(crate::deployment::IN_FLEET);
        std::env::remove_var("SKEIN_HOME");
    }

    // The argv IS the contract. A whole sandbox addressed from the HOST is one `sbx exec` and
    // nothing else — that is how the fleet's own sandbox is provisioned, and it is byte-for-byte
    // the argv skein's original per-box model produced. Pinning all three spellings here is what
    // makes the shared-sandbox switch reviewable in one place rather than as a diff across a dozen
    // files.
    #[test]
    fn a_whole_sandbox_is_addressed_from_the_host_by_one_sbx_exec() {
        // `exec_argv` reads SKEIN_IN_FLEET (via `unreachable_from_fleet`), and the deployment,
        // health and namespace tests set it under the shared lock — reading it without that lock
        // is how this test flaked when a neighbour flipped the variable mid-assertion. Removed
        // rather than merely read: this is the HOST-driven argv, so the test says which deployment
        // it is asking about instead of inheriting whatever the last test left behind.
        let _g = crate::testutil::env_lock();
        std::env::remove_var(crate::deployment::IN_FLEET);
        let p = Place {
            name: "skein-fleet".into(),
            sandbox: "skein-fleet".into(),
            at: Where::SandboxItself,
        };
        assert_eq!(
            p.exec_argv("echo hi"),
            ["sbx", "exec", "skein-fleet", "bash", "-lc", "echo hi"],
            "byte-for-byte the original argv — no nsenter hop, no wrapper"
        );
        // `-i` is load-bearing: without it sbx wires no pipe and the body vanishes silently.
        assert_eq!(
            p.write_argv("cat > f"),
            ["sbx", "exec", "-i", "skein-fleet", "bash", "-lc", "cat > f"]
        );
        assert_eq!(
            p.raw_argv(&["cat", "/tmp/x"]),
            ["sbx", "exec", "skein-fleet", "cat", "/tmp/x"],
            "no shell for a streamed copy — the path is an argv element, not a word to split"
        );
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
        let p = shared("boot-a", 900);
        let argv = p.exec_argv("git status");
        assert_eq!(&argv[..3], ["sbx", "exec", "skein-fleet"]);
        // A shell, because the anchor check has to run in the process that crosses. The caller's
        // argv rides in as `"$@"`, so nothing between here and `nsenter` re-quotes it.
        assert_eq!(&argv[3..5], ["bash", "-c"]);
        assert_eq!(argv[5..].last().unwrap(), "export HOME='/boxes/web-main/home' SKEIN_BOX='web-main' && cd '/boxes/web-main/tree' && git status");
        assert_eq!(&argv[6..9], ["bash", "bash", "-lc"]);

        let crossing = &argv[5];
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

        // The stdin path keeps `-i` in front of the sandbox and the hop after it.
        let w = p.write_argv("cat > f");
        assert_eq!(&w[..4], ["sbx", "exec", "-i", "skein-fleet"]);
        assert!(w.iter().any(|a| a.contains("--preserve-credentials")));
        // And a streamed copy enters the namespace too, or it would `cat` the wrong /tmp entirely.
        let raw = p.raw_argv(&["cat", "/tmp/artifact"]);
        assert_eq!(&raw[raw.len() - 2..], ["cat", "/tmp/artifact"]);
        assert!(raw.iter().any(|a| a.contains("exec nsenter")));
    }

    /// An address skein cannot prove is refused, on every transport, rather than used.
    ///
    /// A record written before the stamp existed is the upgrade case, and the tempting thing is to
    /// let it through "just this once" — which produces a fleet where the guard is present and does
    /// nothing for every box nobody has restarted. The refusal names the fix instead.
    #[test]
    fn an_address_that_cannot_be_proved_is_refused_rather_than_entered() {
        let p = shared("", 0);
        let crossing = p.exec_argv("git status")[5].clone();
        assert!(
            crossing.contains("exit 78") && !crossing.contains("exec nsenter"),
            "an unprovable address must not reach nsenter at all: {crossing}"
        );
        assert!(
            crossing.contains("skein restart web-main"),
            "and it names the fix: {crossing}"
        );
        // The agent is handed the same crossing, so it cannot be the way around the check.
        let agent = p.crossing("git status");
        assert!(
            agent.contains("exit 78") && !agent.contains("exec nsenter"),
            "the agent path must refuse it too: {agent}"
        );
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
        // A space is not a traversal, so such a name is placeable — checked through a real record now
        // that an unrecorded name resolves to nothing at all.
        record_place(
            "a b",
            &PlaceRecord {
                sandbox: "skein-fleet".into(),
                ns_pid: std::process::id(),
                home: "/boxes/a b/home".into(),
                tree: "/boxes/a b/tree".into(),
                sock: "/boxes/a b/session.sock".into(),
                generation: "test-boot".into(),
                ns_start: 1,
                ..Default::default()
            },
        )
        .unwrap();
        let spaced = place_of("a b").expect("a space is not a traversal");
        assert_eq!(
            spaced.exec_argv("true")[2],
            "skein-fleet",
            "the sandbox comes from the record"
        );
        assert!(
            spaced.exec_argv("true").iter().any(|a| a.contains("a b")),
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
    /// This is not a serde formality. There are thirteen of these on the owner's fleet right now,
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
    /// Four boxes, one per answer the probe has to give, and the process it verifies is this test:
    /// a live anchor whose start time matches, one whose pid cannot exist, one whose pid is live but
    /// whose start time is somebody else's, and one whose record predates the stamp and therefore
    /// falls through to the socket.
    ///
    /// Worth running the shell rather than reading it. The start time is cut out of
    /// `/proc/<pid>/stat` by the same `sed`/`cut` the stamp uses, and the only way to know the two
    /// agree is to point them both at a process that is really there.
    /// The refusal to enter says WHICH of the two things it measured went wrong.
    ///
    /// Reported live, about the box its owner was working in:
    ///
    /// ```text
    /// skein: example-box-6 is gone — pid 625094 is no longer the session skein recorded,
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

    /// Linux only: the sweep proves an anchor against `/proc/<pid>`, which is the whole subject.
    #[cfg(target_os = "linux")]
    #[test]
    fn the_sweep_verifies_the_anchor_and_falls_back_only_when_it_cannot() {
        // Exactly as the probe reads it, because agreeing with the probe is the property.
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
        let script = liveness_probe(
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
        let out = std::process::Command::new("bash")
            .arg("-c")
            .arg(&script)
            .output()
            .unwrap();
        assert!(out.status.success(), "{out:?}\n--- script ---\n{script}");
        let report = String::from_utf8_lossy(&out.stdout).to_string();
        let verdict = |name: &str| -> String {
            report
                .lines()
                .find_map(|l| l.strip_prefix(&format!("{name} ")))
                .unwrap_or_else(|| panic!("nothing said about {name}:\n{report}"))
                .trim()
                .to_string()
        };
        assert_eq!(verdict("live-one"), "1", "{report}");
        assert_eq!(verdict("dead-one"), "0", "{report}");
        assert_eq!(
            verdict("reused-one"),
            "0",
            "a recycled pid was reported as the box that used to hold it:\n{report}"
        );
        assert_eq!(
            verdict("old-one"),
            "0",
            "an unstamped record must fall through to the socket, not be decided by the pid:\n{report}"
        );

        // And a record from an earlier boot decides nothing either — same fallback, different
        // reason: the pid space was reset, so that number now names some other process.
        let cycled = liveness_probe(
            root.to_string_lossy().as_ref(),
            &[("live-one".into(), me, "not-this-boot".into(), start)],
        );
        let out = std::process::Command::new("bash")
            .arg("-c")
            .arg(&cycled)
            .output()
            .unwrap();
        let report = String::from_utf8_lossy(&out.stdout).to_string();
        assert!(
            report.contains("live-one 0"),
            "an anchor from another boot was believed:\n{report}"
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
        if std::process::Command::new("bwrap")
            .arg("--version")
            .output()
            .is_err()
        {
            eprintln!("skipping: no bwrap here, so there is no namespace to cross into");
            return;
        }
        let _g = crate::testutil::env_lock();
        let dir = crate::testutil::tempdir();
        let anchor_at = dir.join("anchor");
        let mut boxlike = std::process::Command::new("bwrap")
            .args(["--dev-bind", "/", "/", "--"])
            .arg("bash")
            .arg("-c")
            .arg(format!("echo $$ > {}; sleep 60", anchor_at.display()))
            // Nulled: a child that outlives this holds an inherited pipe open, and `cargo test`
            // then looks like a hang long after the test finished.
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("start a box-like namespace");
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
            found.expect("the box-like namespace never reported its anchor")
        };

        let boot = std::fs::read_to_string("/proc/sys/kernel/random/boot_id").unwrap_or_default();
        let stat = std::fs::read_to_string(format!("/proc/{anchor}/stat")).unwrap_or_default();
        let ns_start: u64 = stat
            .rsplit_once(") ")
            .and_then(|(_, rest)| rest.split_whitespace().nth(19))
            .and_then(|f| f.parse().ok())
            .unwrap_or(0);
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

        // Host-driven: the first hop is still there, and it is still `sbx`.
        std::env::remove_var(crate::deployment::IN_FLEET);
        let from_host = place.exec_argv("readlink /proc/self/ns/mnt");
        assert_eq!(
            &from_host[..3],
            ["sbx", "exec", "skein-fleet"],
            "the host path stopped going through sbx, which is the fallback that makes 4c revertible"
        );

        // In-fleet: no first hop at all, and the crossing still lands inside the box.
        std::env::set_var(crate::deployment::IN_FLEET, "1");
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
        std::env::remove_var(crate::deployment::IN_FLEET);
        assert_eq!(
            said,
            theirs,
            "the crossing did not enter the box (stderr: {})",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_ne!(said, mine, "the command ran here rather than in the box");

        let _ = boxlike.kill();
        let _ = boxlike.wait();
    }

    /// The guard still spends the anchor, and dropping the `sbx` hop did not drop it with it.
    ///
    /// It is the same shell either way — `reach` only decides what runs *before* it — but that is a
    /// claim worth a test rather than a reading, because the whole of the refusal lives in an argv
    /// that a change to argv-building can quietly stop producing.
    #[test]
    fn dropping_the_sbx_hop_does_not_drop_the_anchor_check() {
        let _g = crate::testutil::env_lock();
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
        for in_fleet in [false, true] {
            match in_fleet {
                true => std::env::set_var(crate::deployment::IN_FLEET, "1"),
                false => std::env::remove_var(crate::deployment::IN_FLEET),
            }
            let argv = unprovable.exec_argv("echo reached");
            let joined = argv.join(" ");
            assert!(
                !joined.contains("nsenter"),
                "an address that cannot be proved built an nsenter anyway (in_fleet={in_fleet}): \
                 {joined}"
            );
            assert!(
                joined.contains("skein restart demo"),
                "the refusal does not say what would fix it (in_fleet={in_fleet}): {joined}"
            );
        }
        std::env::remove_var(crate::deployment::IN_FLEET);
    }

    /// A sandbox other than the one skein is standing in is refused from in-fleet, by EVERY argv
    /// builder, and says so.
    ///
    /// Found while writing `reach` rather than planned: dropping the `sbx exec` hop is right when a
    /// second hop enters a namespace, and `SandboxItself` has no second hop. Drop both and the
    /// command runs in the sandbox skein is standing in — a different machine with the same paths
    /// on it. All four builders are checked because the refusal has to be in the one place they
    /// share; three of four would be a hole with no symptom until somebody used the fourth.
    #[test]
    fn another_sandbox_is_not_silently_run_in_the_one_skein_stands_in() {
        let _g = crate::testutil::env_lock();
        let elsewhere = crate::place::own_sandbox("another-fleet");
        std::env::remove_var(crate::deployment::IN_FLEET);
        let from_host = elsewhere.exec_argv("echo hello");
        assert_eq!(&from_host[..3], ["sbx", "exec", "another-fleet"]);

        std::env::set_var(crate::deployment::IN_FLEET, "1");
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
        std::env::remove_var(crate::deployment::IN_FLEET);
    }

    /// How many idle connections are held to `port`.
    ///
    /// Counted per port rather than by [`IDLE`]'s length, because the idle set is process-global
    /// and `cargo test` is not: another test's agent listens on another number, and a bare `len()`
    /// would make this assertion depend on what else happened to be running. That is the class this
    /// crate has shipped twice — a test that passes alone and fails in a parallel run.
    fn idle_for(port: u16) -> usize {
        IDLE.lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .filter(|(kept, _)| *kept == port)
            .count()
    }

    /// What a fake agent does with a request it has read.
    ///
    /// `start_agent` drives the real `fleet-agent.py`, and that is right for what the agent *does*.
    /// These stand in for what a *wire* does when the peer is not well, which the real agent cannot
    /// be asked to be: a transport whose bounds are only ever tested against a healthy peer has no
    /// bounds worth the name. Every one of these shapes was measured on the owner's fleet
    /// (SKEIN-350, SKEIN-351) before it was written down here.
    #[derive(Clone, Copy)]
    enum Answer {
        /// A header promising a megabyte, then one byte every 100ms and nothing else. No budget in
        /// this codebase covers that — and every byte of it renewed a per-`read` window.
        Dribble,
        /// Answers the first request on a connection and never another. What a *kept* connection is
        /// talking to when the agent has stopped serving it, which is the state a reused socket has
        /// to be bounded against with this caller's number and not the last one's.
        OnceThenDeaf,
        /// Answers the first request, stays open and quiet, and then hangs up on the *next* one
        /// without a word.
        ///
        /// This shape rather than "closes as soon as it has answered", because that one never
        /// reaches the recovery it was written for: [`silent`] sees the closed peer while the
        /// socket is still in the idle set and drops it before a byte is sent. Closing only once
        /// the request has arrived is what an agent restarting between two calls actually looks
        /// like from here, and it is the one state in which the reconnect in [`agent_post`] runs.
        OnceThenHangUp,
        /// Answers the first request, then gives the second a header promising more body than it
        /// sends and hangs up. A reply that had *started* arriving is this agent's answer cut
        /// short — never a socket to send the same script down again.
        OnceThenShort,
        /// Answers everything promptly except a script that says `wedge`, which it accepts and
        /// never answers. One box stuck, with every other box's calls arriving beside it.
        Wedge,
    }

    /// A fake agent on a free port, and the promise that the idle set is clean when the test ends.
    struct FakeAgent {
        port: u16,
        /// How many requests reached it. A test that must know a call is *on the wire* — not merely
        /// started — reads this rather than sleeping a guessed interval.
        asked: std::sync::Arc<std::sync::atomic::AtomicU64>,
    }

    impl Drop for FakeAgent {
        fn drop(&mut self) {
            // [`IDLE`] outlives the test. A socket left in it whose peer is one of these fixtures
            // would be handed to whatever ran next.
            IDLE.lock().unwrap_or_else(|e| e.into_inner()).clear();
        }
    }

    fn fake_agent(answer: Answer) -> FakeAgent {
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let asked = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
        let counting = asked.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                // A thread per connection, because that is what the real agent is — a
                // `ThreadingHTTPServer` with `daemon_threads = True` (`src/fleet-agent.py:673`) —
                // and because the whole point of several connections is that they do not queue.
                let counting = counting.clone();
                std::thread::spawn(move || serve_badly(stream, answer, &counting));
            }
        });
        FakeAgent { port, asked }
    }

    fn serve_badly(mut stream: TcpStream, answer: Answer, asked: &std::sync::atomic::AtomicU64) {
        use std::sync::atomic::Ordering;
        let mut served = 0u32;
        loop {
            let mut raw = [0u8; 65536];
            let read = match stream.read(&mut raw) {
                Ok(0) | Err(_) => return,
                Ok(n) => n,
            };
            let request = String::from_utf8_lossy(&raw[..read]).to_string();
            asked.fetch_add(1, Ordering::SeqCst);
            let quiet = match answer {
                Answer::Dribble => {
                    let head = "HTTP/1.1 200 OK\r\nX-Skein-Exit: 0\r\nContent-Length: 1048576\r\n\
                                Connection: keep-alive\r\n\r\n";
                    let _ = stream.write_all(head.as_bytes());
                    let _ = stream.flush();
                    // Ten seconds of it and then silence, rather than for ever: a regression here
                    // must FAIL this test, not hang the whole run while it is being looked for.
                    for _ in 0..100 {
                        if stream.write_all(b"x").and_then(|_| stream.flush()).is_err() {
                            return;
                        }
                        std::thread::sleep(Duration::from_millis(100));
                    }
                    return;
                }
                Answer::OnceThenShort if served > 0 => {
                    let _ = stream.write_all(
                        b"HTTP/1.1 200 OK\r\nX-Skein-Exit: 0\r\nContent-Length: 64\r\n\r\nten bytes.",
                    );
                    let _ = stream.flush();
                    return;
                }
                Answer::OnceThenShort => false,
                Answer::OnceThenDeaf => served > 0,
                Answer::OnceThenHangUp if served > 0 => return,
                Answer::OnceThenHangUp => false,
                Answer::Wedge => request.contains("wedge"),
            };
            if !quiet {
                let body = "answered\n";
                let _ = stream.write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nX-Skein-Exit: 0\r\nContent-Length: {}\r\n\
                         Connection: keep-alive\r\n\r\n{body}",
                        body.len()
                    )
                    .as_bytes(),
                );
                let _ = stream.flush();
            }
            served += 1;
            if quiet {
                // Accepted and answering nothing, with the connection held open. Bounded so the
                // thread cannot outlive the run by much; far past any budget a test here passes.
                std::thread::sleep(Duration::from_secs(60));
                return;
            }
        }
    }

    /// The body of an `/exec` request, without needing a `Place` or a configured home.
    fn exec_body(script: &str) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({ "script": script, "timeout": 5.0 })).unwrap()
    }

    /// A reply that keeps arriving must still be over when the caller's budget is.
    ///
    /// The socket deadlines were set once, at connect (the old `agent_connect`), and
    /// `set_read_timeout` bounds a single `read()` — while `read_reply` makes one per chunk. So an
    /// agent that produced a byte inside every window renewed its welcome for ever, and the number
    /// its caller passed bounded nothing. Measured on the owner's fleet (SKEIN-350):
    /// `GET /review/687/summary?asked=1` still running at 391 seconds against the 180-second budget
    /// at `src/review.rs:2013`, with GitHub answering in 7.4s throughout.
    ///
    /// The fixture is that agent, scaled down: a megabyte promised, delivered a byte per 100ms.
    #[test]
    fn a_reply_that_dribbles_is_over_when_the_budget_is() {
        // The idle set is process-global, so the transport's own state is as shared as the
        // environment is and is serialised on the same lock.
        let _g = env_lock();
        let agent = fake_agent(Answer::Dribble);

        let started = std::time::Instant::now();
        let why = agent_post(
            agent.port,
            "tok",
            &exec_body("echo hi"),
            Duration::from_secs(1),
        )
        .expect_err("a reply that never finishes is not an answer");
        let spent = started.elapsed();

        assert!(
            spent < Duration::from_secs(3),
            "a dribbling reply outlived its 1s budget by {spent:?} — the deadline is being renewed \
             per read again, which is the shape that ran 391s against 180s on the owner's fleet"
        );
        assert!(
            why.contains("no answer in 1.0s"),
            "the failure does not name the budget it spent: {why}"
        );
        // How far it got, because "no answer" is a different investigation from "it was answering
        // and stopped" — one is a box that died and the other is a box that is still writing.
        assert!(
            why.contains("bytes into 1048576"),
            "the failure does not say how much of the reply arrived: {why}"
        );
        // And the socket is gone rather than kept: it is framed mid-message, so the next call on it
        // would read this one's leftovers as its own answer.
        assert_eq!(
            idle_for(agent.port),
            0,
            "a connection abandoned mid-reply was kept for the next call"
        );
    }

    /// A reused connection is bounded by THIS caller's budget, not by the one that opened it.
    ///
    /// `set_read_timeout` was only ever applied in `agent_connect`, on a newly-created socket, so a
    /// connection taken back out of the pool was still carrying whatever the previous caller had
    /// armed it with. A call asking for one second could run on a socket configured for three
    /// minutes — the second half of SKEIN-350, and invisible in isolation because the first call
    /// through a fresh process always sets its own.
    ///
    /// The fixture answers the first request on a connection and then goes quiet on it, which is
    /// what a kept connection reaches when the agent has stopped serving it.
    #[test]
    fn a_reused_connection_is_bounded_by_this_callers_budget_not_the_last_ones() {
        let _g = env_lock();
        let agent = fake_agent(Answer::OnceThenDeaf);

        // A generous first call, which is what arms the socket that gets kept.
        let first = agent_post(
            agent.port,
            "tok",
            &exec_body("echo one"),
            Duration::from_secs(30),
        )
        .expect("the first call to be answered");
        assert_eq!(first.out, b"answered\n");
        assert_eq!(
            idle_for(agent.port),
            1,
            "the connection was not kept, so this test would prove nothing"
        );

        // The same connection, and a caller in a hurry.
        let started = std::time::Instant::now();
        let why = agent_post(
            agent.port,
            "tok",
            &exec_body("echo two"),
            Duration::from_secs(1),
        )
        .expect_err("an agent that stopped answering is not an answer");
        let spent = started.elapsed();

        assert!(
            spent < Duration::from_secs(5),
            "a 1s call on a kept connection took {spent:?} — it is running on the 30s the previous \
             caller armed the socket with, which is the defect this test exists for"
        );
        assert!(
            why.contains("no answer in 1.0s"),
            "the failure names a budget that is not this caller's: {why}"
        );
    }

    /// One stuck call must leave every other box reachable.
    ///
    /// `static AGENT: Mutex<Option<TcpStream>>` was one connection and one mutex for the whole
    /// fleet, and `agent_post` held that mutex across the entire exchange — so a call that never
    /// answered blocked every other box's commands for as long as it lasted. Measured on the
    /// owner's fleet (SKEIN-351): `files?path=.` returned nothing after 60s on
    /// `gadget-demo-repo-archaeology` and nothing after 25s on `example-box-6`, a different
    /// live box, while a box that does not exist 404ed in 0.005s. Routing was fine; the queue was
    /// the transport.
    ///
    /// Two calls, therefore, and the second must not wait for the first.
    #[test]
    fn one_stuck_call_leaves_every_other_box_reachable() {
        use std::sync::atomic::Ordering;
        let _g = env_lock();
        let agent = fake_agent(Answer::Wedge);
        let port = agent.port;

        let stuck = std::thread::spawn(move || {
            agent_post(port, "tok", &exec_body("wedge"), Duration::from_secs(4))
        });
        // On the wire, not merely spawned: a sleep here would be either flaky or slow, and the
        // fixture already counts what reached it.
        for _ in 0..500 {
            if agent.asked.load(Ordering::SeqCst) > 0 {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            agent.asked.load(Ordering::SeqCst) > 0,
            "the wedging call never reached the agent, so nothing is being held"
        );

        let started = std::time::Instant::now();
        let other = agent_post(
            port,
            "tok",
            &exec_body("echo fine"),
            Duration::from_secs(10),
        )
        .expect("a second box to be reachable while the first is stuck");
        let spent = started.elapsed();

        assert_eq!(other.out, b"answered\n");
        assert!(
            spent < Duration::from_secs(2),
            "a healthy box waited {spent:?} behind a stuck one — the transport is serialised again, \
             which is what made two different live boxes hang together"
        );
        // And the stuck one ends on its own budget rather than never.
        let held = stuck.join().expect("the stuck call to end");
        assert!(
            held.is_err(),
            "the agent never answered that one, so it cannot have succeeded"
        );
    }

    /// An idle connection is used only when it has nothing left to say.
    ///
    /// Both ways a kept connection goes bad are invisible to a writer, and a writer is what an
    /// exchange starts with. The peer may have closed its half while the socket sat idle — a
    /// `write` into that succeeds — or a previous call may have left its tail in the buffer, which
    /// the next call would read as its own answer. That second one is the failure `read_reply`'s
    /// doc exists to prevent, and it is why a connection is only ever put back after a *complete*
    /// framed reply.
    #[test]
    fn an_idle_connection_is_used_only_when_it_has_nothing_left_to_say() {
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();

        // Quiet: the ordinary state of a kept connection between calls.
        let quiet = TcpStream::connect(("127.0.0.1", port)).unwrap();
        let mut server = listener.incoming().next().unwrap().unwrap();
        assert!(
            silent(&quiet),
            "a connection with nothing pending must be usable"
        );

        // Leftovers: the tail of a previous reply, arriving after the call that wanted it gave up.
        server.write_all(b"HTTP/1.1 200 OK\r\n").unwrap();
        server.flush().unwrap();
        for _ in 0..200 {
            if !silent(&quiet) {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            !silent(&quiet),
            "a connection holding a previous reply's leftovers was offered to the next call, which \
             would read them as its own answer"
        );

        // Closed: the agent restarting, an idle reaper, the sandbox cycling.
        let gone = TcpStream::connect(("127.0.0.1", port)).unwrap();
        let ended = listener.incoming().next().unwrap().unwrap();
        drop(ended);
        for _ in 0..200 {
            if !silent(&gone) {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            !silent(&gone),
            "a connection the peer had closed was offered to the next call"
        );
    }

    /// A kept connection the agent hangs up on costs one reconnect, not a reported failure.
    ///
    /// Ordinary for keep-alive — the agent restarting, an idle reaper, the sandbox cycling — and it
    /// has to stay ordinary, or the commonest end of a pooled socket reads to a person as "the
    /// sandbox is gone". The retry is gated on the agent provably having said nothing
    /// ([`Fault::unheard`]) and this is the other side of that gate from
    /// [`a_reply_cut_off_part_way_is_reported_rather_than_sent_again`]: too narrow a gate breaks
    /// this test, too wide a gate breaks that one.
    #[test]
    fn a_kept_connection_the_agent_hung_up_on_is_replaced_in_silence() {
        let _g = env_lock();
        let agent = fake_agent(Answer::OnceThenHangUp);

        for call in 0..3 {
            let reply = agent_post(
                agent.port,
                "tok",
                &exec_body("echo hi"),
                Duration::from_secs(5),
            )
            .unwrap_or_else(|why| panic!("call {call} reported a failure for a hang-up: {why}"));
            assert_eq!(reply.out, b"answered\n");
        }
    }

    /// A reply that had started arriving is never sent again, however cheap the retry looks.
    ///
    /// The recovery from a stale keep-alive connection is one reconnect, and it has to be, or the
    /// ordinary end of a pooled socket reads as "the sandbox is gone". But half of what skein sends
    /// a box has a side effect, and [`Place::via_agent`]'s doc names getting this backwards as the
    /// one way this transport can be worse than no transport at all: once the agent has begun
    /// answering, the script has run, and a second attempt applies its effect twice.
    ///
    /// So the fixture answers the first request — putting a connection in the idle set, which is
    /// the only state where a retry was ever reachable — and cuts the second reply off mid-body.
    #[test]
    fn a_reply_cut_off_part_way_is_reported_rather_than_sent_again() {
        use std::sync::atomic::Ordering;
        let _g = env_lock();
        let agent = fake_agent(Answer::OnceThenShort);

        agent_post(
            agent.port,
            "tok",
            &exec_body("echo one"),
            Duration::from_secs(5),
        )
        .expect("the first call to be answered");
        assert_eq!(
            idle_for(agent.port),
            1,
            "no connection was kept, so the retry path this test is about is unreachable"
        );

        let why = agent_post(
            agent.port,
            "tok",
            &exec_body("touch /tmp/side-effect"),
            Duration::from_secs(5),
        )
        .expect_err("a reply that stops mid-body is not an answer");

        assert!(
            why.contains("mid-body"),
            "the failure does not say the reply was cut off: {why}"
        );
        assert_eq!(
            agent.asked.load(Ordering::SeqCst),
            2,
            "the script was sent a second time after the agent had already begun answering it — \
             which for half of what skein sends a box applies the effect twice"
        );
        assert_eq!(
            idle_for(agent.port),
            0,
            "a connection framed mid-message was kept for the next call"
        );
    }
}
