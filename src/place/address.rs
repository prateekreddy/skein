//! What a place IS: the `Where` shape of an address, the `Place` that carries it, the `Purpose`
//! a placement records, and the two addresses that name a whole sandbox.

use super::*;

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
pub(super) fn purpose_or_manual<'de, D: serde::Deserializer<'de>>(
    de: D,
) -> Result<Purpose, D::Error> {
    Ok(match String::deserialize(de) {
        Ok(word) => serde_json::from_value(serde_json::Value::String(word)).unwrap_or_default(),
        Err(_) => Purpose::default(),
    })
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
