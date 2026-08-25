//! Turn state: what a box is doing, and whether anyone is waiting on you.
//!
//! Two halves. **Edges** are the runtime's lifecycle hooks — fast, but no runtime fires anything
//! when you answer a permission prompt, dismiss a dialog, interrupt a turn, or when the agent
//! dies, so a state nobody clears used to be shown forever. **Level** is what the box's screen
//! says right now, sampled by an in-box observer. Level clears itself; edges cannot.
//!
//! With no observation present, or a screen the grammar does not recognise, turn state is exactly
//! the edge signal it always was — and says so, rather than quietly degrading.

use crate::digest::read_journal;
use crate::registry::store_for_box;
use crate::util::valid_name;
use crate::util::*;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;

pub(crate) fn probe_is_stale(store: &Path, name: &str) -> bool {
    let current = fs::read_to_string(store.join("skein/probe-revision"))
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    let booted = fs::read_to_string(store.join("skein/boot").join(format!("{name}.json")))
        .ok()
        .and_then(|value| serde_json::from_str::<serde_json::Value>(&value).ok())
        .and_then(|value| value.get("probe_revision")?.as_str().map(str::to_string))
        .filter(|value| !value.is_empty());
    current.is_some() && current != booted
}

/// The narrative signal a box writes on each turn-end (box-session.sh): the last assistant
/// message (Stop) or the prompt it's blocked on (Notification). The free digest source —
/// the agent already wrote the words, so reading them costs no model tokens.
#[derive(Debug, Default, Clone, Deserialize)]
pub struct SessionSignal {
    #[serde(default)]
    pub ts: String,
    #[serde(default)]
    pub kind: String, // "stop" | "notification"
    #[serde(default, rename = "lastMessage")]
    pub last_message: String,
    #[serde(default)]
    pub prompt: String,
}

/// Read `<store>/sessions/<name>.json` — the last narrative signal the box reported.
pub fn session_signal(name: &str) -> Option<SessionSignal> {
    if !valid_name(name) {
        return None;
    }
    let path = store_for_box(name)?
        .join("sessions")
        .join(format!("{name}.json"));
    let txt = fs::read_to_string(path).ok()?;
    serde_json::from_str(&txt).ok()
}

/// The box's *current task* — what it's doing right now, for the fleet's peripheral view. Prefers
/// the live signal (`box-task.sh` writes the in-progress TodoWrite item to `<store>/tasks/<name>.json`)
/// and falls back to the `next …` clause of the box's most recent journal line. No model call.
pub fn current_task(name: &str) -> Option<String> {
    if !valid_name(name) {
        return None;
    }
    if let Some(p) = store_for_box(name).map(|d| d.join("tasks").join(format!("{name}.json"))) {
        if let Ok(txt) = fs::read_to_string(p) {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&txt) {
                if let Some(t) = v.get("task").and_then(|t| t.as_str()).map(str::trim) {
                    if !t.is_empty() {
                        return first_line(t);
                    }
                }
            }
        }
    }
    journal_next(name)
}

/// The agent turn-state skein's own probe (box-status.sh) records for a box, from
/// `<store>/status/<name>.json` — the skein-owned replacement for the registry's `status` field.
pub fn current_status(name: &str) -> Option<String> {
    status_edge(name).map(|(status, _)| status)
}

/// The same edge signal with the timestamp it was written at (epoch seconds), which the level/edge
/// fusion needs to decide which of the two is fresher. `ts` is 0 when the probe wrote none.
pub(crate) fn status_edge(name: &str) -> Option<(String, i64)> {
    if !valid_name(name) {
        return None;
    }
    let p = store_for_box(name)?
        .join("status")
        .join(format!("{name}.json"));
    let v: serde_json::Value = serde_json::from_str(&fs::read_to_string(p).ok()?).ok()?;
    let at = v
        .get("ts")
        .and_then(|t| t.as_str())
        .and_then(|t| DateTime::parse_from_rfc3339(t).ok())
        .map(|t| t.timestamp())
        .unwrap_or(0);
    let status = v
        .get("status")
        .and_then(|s| s.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())?;
    // A *busy* status is a claim of current activity — trust it only while fresh. If the agent
    // process dies mid-turn (crash/OOM: no hook fires again), "working" would otherwise stick to a
    // Running sandbox forever and the user would dutifully leave it alone. Past the threshold,
    // fall back to liveness ("live": sandbox up, no agent claim) — the next real turn event
    // rewrites the file and the state snaps back. Outcome states (waiting/needs-input/error/
    // ended/done) stay sticky: they describe how the turn ENDED, and age is expected.
    if matches!(status.as_str(), "working" | "running" | "compacting") {
        if let Some(ts) = v.get("ts").and_then(|t| t.as_str()) {
            if let Ok(t) = DateTime::parse_from_rfc3339(ts) {
                if (Utc::now() - t.with_timezone(&Utc)).num_seconds() > 45 * 60 {
                    return None;
                }
            }
        }
    }
    Some((status, at))
}

// ---------- the level signal: what a box's screen says *right now* ----------
// Every other signal skein has is an edge (a hook firing). Edge coverage is incomplete — no runtime
// reports "the human answered", "the dialog was dismissed", "the turn was interrupted", "the agent
// died" — so a state nobody clears is shown forever. `box-pane.sh` samples the agent's screen and
// records what it saw; the interpretation lives here, in Rust, where the provider-specific grammar
// is unit-tested against real captures and a fix ships with the binary instead of needing a new
// probe rolled into every store. See docs/turn-state.md.

/// One sample of a box's agent screen, as `box-pane.sh` wrote it to
/// `<store>/<name>.pane.json`. Every field is optional so a probe from a newer/older skein can
/// still be read (an absent field degrades to "unknown", never to a wrong claim).
#[derive(Debug, Clone, Default, Deserialize)]
pub struct PaneObs {
    /// when the observation was taken (epoch seconds)
    #[serde(default)]
    pub ts: i64,
    /// seconds since the pane last produced output — the spinner redraw is what makes this move
    #[serde(default)]
    pub age: i64,
    /// 1 when output changed between the observer's last two ticks
    #[serde(default)]
    pub moving: u8,
    /// 1 when the agent's tmux window is gone or its pane is dead
    #[serde(default)]
    pub dead: u8,
    /// the terminal title, which Claude Code sets to `<spinner> <what it is doing>`
    #[serde(default)]
    pub title: String,
    /// seconds since the observer *saw* the title change, or -1 when it never has. The title lags:
    /// Claude Code was observed carrying a finished tool's description ("Fetch and quote robots.txt
    /// file") through an unrelated later turn, so its text is only usable while demonstrably fresh.
    #[serde(default = "unknown_age")]
    pub title_age: i64,
    #[serde(default)]
    pub cmd: String,
    /// the visible tail of the pane, oldest line first — never scrollback
    #[serde(default)]
    pub tail: Vec<String>,
    /// **Which contract the probe that wrote this was working to.** See [`PANE_CONTRACT`].
    ///
    /// `box-pane.sh` has written `"contract":1` into every observation since it existed, and this
    /// struct had no field for it — serde ignores what it does not know, so the marker the probe
    /// took the trouble to send was read by nothing. A probe from a newer skein could change what
    /// `tail` or `title_age` mean and this build would parse it as if nothing had happened.
    ///
    /// Zero when the probe predates the marker, which is not a fault: the fields all carry serde
    /// defaults and that shape is exactly what shipped.
    #[serde(default)]
    pub contract: u32,
}

/// The pane-observation contract this build understands.
///
/// **Boxes outlive the skein that installed them.** A box keeps whichever probe it was last given,
/// so a fleet is routinely a mixture — and the direction that hurts is a probe NEWER than the reader,
/// which is what happens when skein is upgraded and a long-running box is not restarted.
///
/// The rule is the launcher's, applied to data instead of to a cgroup ceiling: **an unfamiliar
/// version is skipped, not evaluated.** There, evaluating an unrecognised word under `set -u`
/// aborted the shell and took a fleet's boxes down; here, parsing an unfamiliar observation would
/// be quieter and worse — a state read out of fields that no longer mean what this build thinks.
/// So a newer observation is treated as no observation, the board falls back to hook edges exactly
/// as it does for a box with no observer, and [`screen_health`] says which.
pub const PANE_CONTRACT: u32 = 1;

pub(crate) fn unknown_age() -> i64 {
    -1
}

/// How old a pane observation may be and still count. The observer heartbeats every 10s, so this
/// tolerates three missed beats before the level signal is treated as absent (which falls back to
/// exactly the pre-observer behaviour — see `fuse_status`).
pub(crate) const PANE_FRESH_SECS: i64 = 35;

/// How recently the terminal title must have changed for its text to count as "what it is doing
/// now". Beyond this it is the residue of an earlier tool call.
pub(crate) const TITLE_FRESH_SECS: i64 = 90;

/// The last observation as written, freshness *not* applied. For callers that must tell "no observer
/// at all" apart from "an observer that stopped" — see [`screen_health`].
pub fn read_pane_raw(name: &str) -> Option<PaneObs> {
    if !valid_name(name) {
        return None;
    }
    let p = store_for_box(name)?
        .join("status")
        .join(format!("{name}.pane.json"));
    serde_json::from_str(&fs::read_to_string(p).ok()?).ok()
}

/// True when the sample is recent enough to act on. Clock skew between host and guest would otherwise
/// silently disable the whole layer, so a future-dated sample is accepted; only genuinely *old* ones
/// are dropped.
pub(crate) fn pane_is_fresh(obs: &PaneObs) -> bool {
    obs.ts > 0 && Utc::now().timestamp() - obs.ts <= PANE_FRESH_SECS
}

/// Is this observation one this build knows how to read? See [`PANE_CONTRACT`].
pub(crate) fn pane_is_readable(obs: &PaneObs) -> bool {
    obs.contract <= PANE_CONTRACT
}

/// The level observation for a box, or `None` when there is no observer, it died, its last sample
/// is too old to trust, or it was written to a contract this build does not know.
pub fn read_pane(name: &str) -> Option<PaneObs> {
    read_pane_raw(name)
        .filter(pane_is_readable)
        .filter(pane_is_fresh)
}

/// Whether the **screen** half of turn-state is contributing for this box, and if not, why.
///
/// This exists because a missing level signal is invisible by construction: the board simply reverts
/// to hook edges and looks entirely normal, which is the behaviour that showed an answered decision
/// for twenty minutes. If half the signal is off, the row should say so rather than imply a
/// confidence it doesn't have.
///
/// * `""` — reading the screen (or the box isn't running, where a screen means nothing)
/// * `"none"` — nothing has ever been written: the observer isn't running. Reattach the box.
/// * `"stale"` — observations stopped arriving: the agent session or the observer is gone. Reattach.
/// * `"unreadable"` — a fresh sample the grammar does not recognise: a TUI change, worth reporting.
///   The sample itself is on disk at `<store>/status/<box>.pane.json`.
/// * `"unsupported"` — this runtime has no screen grammar at all, so hooks only, by design.
/// * `"newer"` — the probe is writing a contract this skein does not know, so its observations are
///   skipped rather than parsed as if they meant what they used to. Different fault, different fix
///   from `unreadable`: that one is a TUI this grammar has not seen, this one is a skein that is
///   behind its own probe.
pub fn screen_health(runtime: &str, raw: Option<&PaneObs>, running: bool) -> &'static str {
    if !running {
        return "";
    }
    if !has_screen_grammar(runtime) {
        return "unsupported";
    }
    match raw {
        None => "none",
        // Before staleness, because a newer contract is a statement about the whole observation:
        // calling it stale would send somebody to restart an observer that is working perfectly.
        Some(obs) if !pane_is_readable(obs) => "newer",
        Some(obs) if !pane_is_fresh(obs) => "stale",
        // A dead pane is a real answer ("the agent is gone"), not a failure to read one.
        Some(obs) if classify_pane(runtime, obs) == Screen::Unknown => "unreadable",
        Some(_) => "",
    }
}

/// Which kind of dialog is blocking. Each wants a different move from the human, which is why the
/// board says which one rather than a single "decision".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Blocked {
    /// "Do you want to allow…" — a tool wants permission
    Permission,
    /// a question or plan approval — a judgement call
    Question,
    /// "Do you trust the files in this folder?" — nothing can start until you answer
    Trust,
    /// signed out, or out of quota
    Auth,
}

impl Blocked {
    pub fn key(self) -> &'static str {
        match self {
            Blocked::Permission => "permission",
            Blocked::Question => "question",
            Blocked::Trust => "trust",
            Blocked::Auth => "auth",
        }
    }
}

/// What the screen says. `Unknown` is a first-class answer: an unrecognised screen must fall back to
/// the edge signal, never invent a state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Screen {
    Busy,
    Waiting,
    Blocked(Blocked),
    Error(String),
    /// the agent's window is gone — crashed, exited, or never started
    Dead,
    Unknown,
}

/// True when `line` is a dialog's **selected** option — the numbered row carrying the runtime's
/// selection marker (`❯ 1. Yes`, `› 1. Yes, proceed (y)`, `> 1. Sign in with ChatGPT`).
///
/// Two deliberate narrowings, both of which false-positived before:
///
/// * `markers` is one runtime's glyph set, never the union. skein already knows which agent a box
///   runs (`load_views` resolves it from sbx metadata, then the box's launch spec, then the repo
///   default), so nothing here has to guess — and a Claude pane displaying a pasted *Codex* dialog,
///   which happens routinely in this repo, must not read as a live one.
/// * the marker is required. An unmarked numbered row is just a numbered list, and agents write those
///   constantly ("2. The observer was capturing scrollback"); only the selected row is decorated, and
///   one selected row is all the evidence a dialog needs.
pub(crate) fn is_option_line(line: &str, markers: &[char]) -> bool {
    let trimmed = line.trim_start();
    let Some(l) = markers
        .iter()
        .find_map(|m| trimmed.strip_prefix(*m))
        .map(str::trim_start)
    else {
        return false;
    };
    matches!(l.chars().next(), Some(c) if c.is_ascii_digit())
        && l.split_once('.')
            .is_some_and(|(n, rest)| n.chars().all(|c| c.is_ascii_digit()) && rest.starts_with(' '))
}

/// Claude Code's status line while a turn is running, matched on its **shape**:
///
/// ```text
/// ✽ Beboppin'… (3m 43s · ↓ 12.9k tokens · thinking)
/// ✻ Beboppin'… (4m 6s · ↓ 13.7k tokens · thought for 10s)
/// ✽ Beboppin'… (3m 39s · ↓ 12.5k tokens)
/// ```
///
/// A spinner glyph, a present-tense verb ending in an ellipsis, then a parenthesised **elapsed
/// time**. Only that much is invariant: everything after the elapsed time comes and goes between
/// consecutive samples, which is why the old predicate (`tokens)`, i.e. the *end* of the line)
/// flipped a real box between `working` and `waiting` every couple of seconds.
///
/// Deliberately excluded: tool announcements (`● Running 4 shell commands…`) carry the ellipsis but
/// no elapsed time, and the completion marker (`✻ Sautéed for 24m 3s`) carries neither — it sits
/// above an idle composer for the whole of the following turn.
pub(crate) fn is_working_status_line(line: &str) -> bool {
    let l = line.trim();
    // Status lines open with a spinner glyph — never a message bullet (`●`), a tool result (`⎿`), a
    // composer prompt, a quotation, or ordinary prose. Deliberately a *denylist*: the animation
    // cycles through at least `· ✢ * ✶ ✻ ✽` — all six observed live, including the plain ASCII `*` —
    // and an allowlist would quietly start flapping again the day a release adds a frame.
    let spinner =
        matches!(l.chars().next(), Some(c) if !c.is_alphanumeric() && !"●⎿>❯›\"'".contains(c));
    if !spinner {
        return false;
    }
    let Some((_, rest)) = l.split_once("… (") else {
        return false;
    };
    let digits = rest.chars().take_while(|c| c.is_ascii_digit()).count();
    digits > 0 && matches!(rest[digits..].chars().next(), Some('s' | 'm' | 'h'))
}

/// Claude Code's status line while the turn is parked on background agents — a busy state
/// [`is_working_status_line`] cannot see, because the line carries no parenthesised elapsed time:
///
/// ```text
/// ✻ Waiting for 4 background agents to finish
/// ```
///
/// Captured live 2026-08-24 (example-box-6, mid-turn, operator statusline): the pane redrew
/// every second — age 0-1, moving 1 across eight samples 4s apart — while the board read `waiting`.
/// A box parked on its own agents resumes by itself; it needs nobody. Same glyph denylist as the
/// status line, so the agents panel's own rows (`◯ general-purpose …`), a quoted copy, and prose
/// about waiting cannot fake it.
pub(crate) fn is_waiting_on_agents_line(line: &str) -> bool {
    let l = line.trim();
    starts_like_status(l) && l.contains("Waiting for") && l.contains("background agent")
}

/// The spinner-glyph opening every status line carries, and the denylist that keeps the pane's own
/// transcript from wearing it: `●` is the agent speaking, `⎿` a tool result, `>`/`❯`/`›` the
/// composer, a quote mark a captured copy.
fn starts_like_status(trimmed: &str) -> bool {
    matches!(trimmed.chars().next(), Some(c) if !c.is_alphanumeric() && !"●⎿>❯›\"'".contains(c))
}

/// Claude Code compacting *now*, told apart from the record of a compact that already finished.
/// Skein's own PreCompact hook prints `[… box-status.sh compacting] completed successfully` into
/// the transcript of every box in the fleet, and `⎿  Compacted (ctrl+o…)` sits above it — both
/// carry the word, neither is the pane's state. Captured live 2026-08-24
/// (lattice-feat-design-codex-claude): idle 8 minutes after a `/compact`, called `working` for 37.
pub(crate) fn is_compacting_line(line: &str) -> bool {
    let l = line.trim();
    starts_like_status(l) && l.to_lowercase().contains("compacting")
}

/// A spinner glyph in the terminal title is how both runtimes say "busy" — Claude Code writes
/// `⠂ Claude Code` while working and `✳ Claude Code` when idle, Codex writes `⠋ <dir>`. Braille is
/// the animated set in both. A bonus signal only: Claude Code's glyph is braille in some frames and
/// `_` in others while working, so nothing may depend on it alone.
pub(crate) fn title_is_spinning(title: &str) -> bool {
    matches!(title.trim().chars().next(), Some(c) if ('\u{2800}'..='\u{28FF}').contains(&c))
}

/// The activity text Claude Code puts in the terminal title (`✳ Run bash command true` → "Run bash
/// command true"). `None` for the generic idle title, which names no activity.
pub fn title_activity(title: &str) -> Option<String> {
    let rest = title
        .trim()
        .trim_start_matches(|c: char| !c.is_alphanumeric())
        .trim();
    let generic = matches!(rest, "Claude Code" | "Codex" | "codex" | "claude");
    (!rest.is_empty() && !generic).then(|| rest.to_string())
}

/// Interpret a pane observation for `runtime`. Matched against the visible tail only — never
/// scrollback — so an agent that prints "Do you want to…" in its own prose cannot fake a dialog,
/// and a dialog is only believed when it also carries an option list *and* the composer is gone
/// (a dialog replaces it).
///
/// Both runtimes are implemented from live captures (docs/turn-state.md §6, §6b); anything else
/// returns `Unknown`, which defers to the hook edges — i.e. exactly the old behaviour — rather than
/// guess at a grammar nobody has read.
/// The runtimes whose screens skein can read. Kept next to `classify_pane`'s dispatch so the two
/// cannot drift, and used by [`screen_health`] to say "hooks only, by design" rather than "broken".
pub fn has_screen_grammar(runtime: &str) -> bool {
    matches!(runtime, "claude" | "codex")
}

pub fn classify_pane(runtime: &str, obs: &PaneObs) -> Screen {
    // A gone window needs no grammar, so it reports for every runtime.
    if obs.dead == 1 {
        return Screen::Dead;
    }
    let lower: Vec<String> = obs.tail.iter().map(|l| l.to_lowercase()).collect();
    match runtime {
        "claude" => classify_claude(obs, &lower),
        "codex" => classify_codex(obs, &lower),
        _ => Screen::Unknown,
    }
}

/// A shell prompt where the TUI should be: the agent exited and the launch guard dropped to bash.
pub(crate) fn dropped_to_shell(obs: &PaneObs) -> bool {
    obs.tail
        .iter()
        .any(|l| l.contains('@') && (l.trim_end().ends_with('$') || l.trim_end().ends_with('#')))
}

pub(crate) fn classify_claude(obs: &PaneObs, lower: &[String]) -> Screen {
    let any = |needle: &str| lower.iter().any(|l| l.contains(needle));

    // Quota/auth first: it reads like an error but the fix is yours, so it belongs in "needs you".
    if any("usage limit reached") || any("invalid api key") || any("please run /login") {
        return Screen::Blocked(Blocked::Auth);
    }
    if any("do you trust the files") {
        return Screen::Blocked(Blocked::Trust);
    }
    // The composer: proof the TUI is alive and accepting input — *not* proof that it is idle, since
    // Claude Code keeps the composer on screen while it works. Its footer says which mode is on, and
    // `? for shortcuts` when none is; a configured statusline can push those around, so the bare
    // prompt line between the two rules (`❯` and a non-breaking space, nothing else) counts too.
    let composer = any("? for shortcuts")
        || any("auto mode on")
        || any("manual mode on")
        || any("bypass permissions on")
        || obs.tail.iter().any(|l| matches!(l.trim(), "❯" | ">"));
    let options = obs.tail.iter().any(|l| is_option_line(l, &['❯', '>']));
    if options && !composer {
        // "Do you want to …?" is a permission ask; anything else with options is a question or a
        // plan approval — a judgement call rather than a yes/no on a tool.
        if any("do you want to") {
            return Screen::Blocked(Blocked::Permission);
        }
        return Screen::Blocked(Blocked::Question);
    }
    // Busy: the status line's *shape* (see `is_working_status_line`), which is the only signal that
    // survived contact with a real box. `esc to interrupt` and a braille title glyph say the same
    // thing from other angles, but neither is dependable: across four minutes of continuous work in a
    // real box, `esc to interrupt` never appeared on screen at all, and the title's glyph was
    // sometimes braille and sometimes `_`. `Baked for…`/`Sautéed for…` are *completion* markers and
    // deliberately not matched: they sit above an idle composer for the whole of the next turn.
    // Ranked ABOVE the error line on purpose: a turn that is visibly running outranks an error
    // string still sitting in the tail from the *previous* turn (or from a retry in this one).
    // Only the bottom of the screen counts: the status line lives directly above the composer, so
    // anything matching further up is the agent *displaying* one — a captured fixture in a diff, a
    // log being catted — not the pane's own. (Caught on live data: this file's own test fixtures were
    // on screen while being edited.)
    // …and the bottom is the *composer*, not the last line of the pane. Rules, the context meter,
    // the mode footer and — the case that named this — the agents panel all sit below it, and a box
    // running four agents puts twelve lines of chrome under its own status line (captured live,
    // 2026-08-24: the board read `waiting` for nine minutes at a pane that was mid-turn). Cutting at
    // the composer keeps the window pointed at the status region however much chrome follows it.
    // …and when the composer is on screen it, not the last line of the pane, is the bottom: rules,
    // the context meter, the mode footer and — the case that named this — the agents panel all sit
    // *below* it, and a box running four agents puts twelve lines of chrome under its own status
    // line (captured live 2026-08-24: the board read `waiting` for nine minutes at a pane that was
    // mid-turn). Above the composer the window is tight, because the status line is the line
    // directly above it, give or take a right-aligned notice and the rule.
    let region: Vec<&String> = match obs.tail.iter().rposition(|l| matches!(l.trim(), "❯" | ">")) {
        Some(composer) => obs.tail[..composer]
            .iter()
            .rev()
            .filter(|l| !l.trim().is_empty())
            .take(4)
            .collect(),
        None => obs
            .tail
            .iter()
            .rev()
            .filter(|l| !l.trim().is_empty())
            .take(10)
            .collect(),
    };
    if region
        .iter()
        .any(|l| is_working_status_line(l) || is_waiting_on_agents_line(l) || is_compacting_line(l))
        || any("esc to interrupt")
        || title_is_spinning(&obs.title)
    {
        return Screen::Busy;
    }
    if any("api error") || any("overloaded") {
        let detail = if any("api error") {
            "API error"
        } else {
            "overloaded"
        };
        return Screen::Error(detail.to_string());
    }
    if composer {
        return Screen::Waiting;
    }
    if dropped_to_shell(obs) {
        return Screen::Dead;
    }
    Screen::Unknown
}

/// Codex 0.145.0, captured live (docs/turn-state.md §6b). Two things make its screen easier to read
/// than Claude's: a dialog replaces the composer *and* carries a fixed footer (`Press enter to
/// confirm…`), and the terminal title says `[ ! ] Action Required` while — and only while — a
/// decision is pending. That marker is animated (`[ ! ]` → `[ . ]`), so only the words can be matched.
pub(crate) fn classify_codex(obs: &PaneObs, lower: &[String]) -> Screen {
    let any = |needle: &str| lower.iter().any(|l| l.contains(needle));

    // Onboarding sign-in: no credentials, so nothing runs until a human authenticates. No hook can
    // ever report this — hooks belong to a session that has not started.
    if any("sign in with chatgpt") || any("provide your own api key") {
        return Screen::Blocked(Blocked::Auth);
    }
    // The composer's footer, present exactly when no dialog is up.
    let composer = any("? for shortcuts");
    // Every dialog — approval, picker, onboarding — ends in a confirm footer: "Press enter to
    // confirm or esc to cancel" / "…or esc to go back" / "Press enter to continue".
    let confirm = any("press enter to confirm") || any("press enter to continue");
    // `›` in dialogs, plain `>` in the onboarding screens.
    let options = obs.tail.iter().any(|l| is_option_line(l, &['›', '>']));
    if confirm && options && !composer {
        // "Would you like to run …?" / "… make the following edits?" is a yes/no on a tool.
        if any("would you like to") {
            return Screen::Blocked(Blocked::Permission);
        }
        // Codex gates *hooks* rather than the folder, and skein installs hooks into every store —
        // so "19 hooks are new or changed" is the trust wall a skein box actually hits, and it
        // blocks before any hook could fire to say so.
        if any("hooks need review") || any("trust all and continue") || any("do you trust") {
            return Screen::Blocked(Blocked::Trust);
        }
        return Screen::Blocked(Blocked::Question);
    }
    // The title carries the same claim and survives a dialog body we have no phrasing for, so it is
    // the fallback: attention pending, kind unknown. Verified to clear the instant a dialog is
    // answered (approve or esc) and to stay clear through a finished turn.
    if title_has_attention(&obs.title) && !composer {
        return Screen::Blocked(Blocked::Question);
    }
    if any("esc to interrupt") || title_is_spinning(&obs.title) {
        return Screen::Busy;
    }
    // Codex prints failures as a `■ ` line carrying a JSON payload. Prose `■ ` lines are notices
    // ("■ Conversation interrupted - tell the model what to do differently"), not failures, and the
    // monthly-limit "⚠ Heads up…" is a warning beside a perfectly live composer.
    if obs
        .tail
        .iter()
        .any(|l| l.trim_start().starts_with('■') && l.contains("{\""))
    {
        return Screen::Error("API error".into());
    }
    if composer {
        return Screen::Waiting;
    }
    if dropped_to_shell(obs) {
        return Screen::Dead;
    }
    Screen::Unknown
}

/// Codex's explicit "a human must act" title marker, `[ ! ] Action Required | <dir>`. The bracket
/// animates, so the words are the signal.
pub(crate) fn title_has_attention(title: &str) -> bool {
    title.to_lowercase().contains("action required")
}

/// Where a fused status came from — architecture §2.2's rule that **no displayed state may rest on
/// an edge alone without saying so**.
///
/// An edge-triggered latch with incomplete edge coverage cannot recover: a state nobody clears is
/// shown for ever, which is the twenty-minute answered-decision bug. Three of the fusion rules
/// display an edge with no level behind it, and each is right — showing nothing would be worse.
/// What makes them honest is that the row says which.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusFrom {
    /// A level observation decided it. The screen is being read and it is what you are looking at.
    Screen,
    /// The edge alone: there is no usable level observation (rules 1 and 4). The row already says
    /// so through [`screen_health`], which names *why* the screen is not contributing.
    Edge,
    /// **Rule 3, and the gap this enum was added to close.** There IS a fresh, readable level
    /// observation, and an edge newer than it led anyway. `screen_health` is empty here — the
    /// observer is perfectly healthy — so nothing said that what is displayed came from an edge,
    /// and the next sample either confirms it or silently corrects it.
    EdgeAheadOfScreen,
}

impl StatusFrom {
    pub fn key(self) -> &'static str {
        match self {
            StatusFrom::Screen => "screen",
            StatusFrom::Edge => "edge",
            StatusFrom::EdgeAheadOfScreen => "edge-ahead",
        }
    }
}

/// Fold the level observation into the edge status. Returns the effective status key, the blocking
/// kind when there is one, and **where the answer came from**.
///
/// The four rules (docs/turn-state.md §4.3), in order:
///   1. no level observation ⇒ the edge, unchanged — older boxes behave exactly as before;
///   2. attention never latches: a fresh level observation *overrides* a stale edge that still
///      claims `blocked`/`error`, which is the twenty-minute bug;
///   3. edges lead: an edge newer than the sample wins, so a Notification shows instantly and the
///      next sample confirms or corrects it;
///   4. `Unknown` defers to the edge rather than guessing.
///
/// The provenance is not decoration. Rules 1, 3 and 4 all display a state no level observation
/// supports, and only rules 1 and 4 were visible — `screen_health` names them because the observer
/// is absent or unreadable. Rule 3 fires with a *healthy* observer, so nothing disclosed it at all.
pub(crate) fn fuse_status(
    edge: Option<(String, i64)>,
    level: Option<(Screen, i64)>,
) -> (Option<String>, &'static str, StatusFrom) {
    let (screen, level_ts) = match level {
        None => return (edge.map(|(s, _)| s), "", StatusFrom::Edge), // rule 1
        Some(pair) => pair,
    };
    let edge_leads = edge.as_ref().is_some_and(|(_, ts)| *ts > level_ts + 1);
    let edge_status = edge.as_ref().map(|(s, _)| s.as_str()).unwrap_or("");
    let edge_is_outcome = matches!(
        edge_status,
        "blocked" | "needs-input" | "needs-decision" | "error" | "ended" | "done" | "waiting"
    );
    match screen {
        Screen::Unknown => (edge.map(|(s, _)| s), "", StatusFrom::Edge), // rule 4
        // An edge that arrived *after* the sample is the fresher truth (rule 3) — and this is the
        // one case where the screen is healthy and still not what is shown.
        _ if edge_leads && edge_is_outcome => {
            (edge.map(|(s, _)| s), "", StatusFrom::EdgeAheadOfScreen)
        }
        Screen::Blocked(kind) => (Some("blocked".into()), kind.key(), StatusFrom::Screen),
        Screen::Busy => (Some("working".into()), "", StatusFrom::Screen),
        Screen::Waiting => (Some("waiting".into()), "", StatusFrom::Screen),
        Screen::Error(_) => (Some("error".into()), "", StatusFrom::Screen),
        // `done` is a human-set outcome, not something a screen can contradict.
        Screen::Dead if edge_status == "done" => (Some("done".into()), "", StatusFrom::Screen),
        Screen::Dead => (Some("ended".into()), "", StatusFrom::Screen),
    }
}

/// The human-readable `detail` the probe attaches to a state that carries one — the StopFailure
/// `error_type` ("API error: rate limit") or the SessionEnd `reason` ("session ended: logout").
/// Empty/absent for the common states. Surfaced as the box's headline so the row says *why*.
pub fn current_status_detail(name: &str) -> Option<String> {
    if !valid_name(name) {
        return None;
    }
    let p = store_for_box(name)?
        .join("status")
        .join(format!("{name}.json"));
    let v: serde_json::Value = serde_json::from_str(&fs::read_to_string(p).ok()?).ok()?;
    v.get("detail")
        .and_then(|s| s.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// The `next …` clause of the box's most recent journal line (the ritual is `did … / next … /
/// blocked-on …`) — a free, end-of-turn "what's next" when there's no live task signal.
pub(crate) fn journal_next(name: &str) -> Option<String> {
    let j = read_journal(name)?;
    for line in j.lines().rev() {
        // case-insensitive find of "next"; ASCII fold keeps byte offsets valid in the original line.
        let lower = line.to_ascii_lowercase();
        let Some(i) = lower.find("next") else {
            continue;
        };
        let rest = line[i + 4..].trim_start_matches([':', ' ', '-', '\t', '…']);
        // a clause runs to the next "/" separator, or to an inline "blocked" if not slash-delimited.
        let clause = rest.split('/').next().unwrap_or(rest);
        let clause = match clause.to_ascii_lowercase().find("blocked") {
            Some(b) => &clause[..b],
            None => clause,
        };
        if let Some(h) = first_line(clause.trim()) {
            return Some(h);
        }
    }
    None
}

/// The Notification signal carries only Claude Code's generic "waiting for your input" text — it says
/// a box needs you but not what it was doing. Detect it so the headline can fall back to the task.
pub(crate) fn is_generic_wait(h: &str) -> bool {
    h.to_ascii_lowercase().contains("waiting for your input")
}

/// Why a turn ended — the heuristic fork-detector (step 5). A deterministic classification of
/// the agent's last message into what the human owes it, so the inbox can rank and (later) batch
/// only the *trivial* asks. Read-only: this never decides for you, it only routes attention.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Pause {
    /// blocked on a permission/decision (the Notification signal) — most urgent
    NeedsInput,
    /// ended on a trivial "shall I proceed?" — a candidate for one-click batch resolve
    Proceed,
    /// ended on a real question that needs a judgement call — a genuine fork
    Fork,
    /// ended on a statement (work reported, nothing asked) — review at your leisure
    Statement,
    /// nothing to act on (still working, or no signal yet)
    #[default]
    None,
}

impl Pause {
    /// Inbox tie-breaker within a status tier: a genuine decision outranks a rote "proceed?",
    /// which outranks a bare statement. Lower = wants your attention sooner.
    pub fn rank(self) -> u8 {
        match self {
            Pause::NeedsInput => 0,
            Pause::Fork => 1,
            Pause::Proceed => 2,
            Pause::Statement => 3,
            Pause::None => 4,
        }
    }
}

/// Classify the last assistant message. `blocked` is true when the box's status is the
/// permission-prompt signal (Notification), which dominates regardless of the text.
pub fn classify_message(msg: &str, blocked: bool) -> Pause {
    if blocked {
        return Pause::NeedsInput;
    }
    let trimmed = msg.trim();
    if trimmed.is_empty() {
        return Pause::None;
    }
    // Only the tail matters — a turn that *ends* on a question is asking; a "?" buried in the
    // middle of a long report is not. Look at the last non-empty line.
    let last_line = trimmed
        .lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .unwrap_or(trimmed)
        .trim();
    let ends_question = last_line.ends_with('?');
    let hay = last_line.to_lowercase();
    // Trivial "may I continue" endings — the residual that batch-resolve is for.
    const PROCEED: &[&str] = &[
        "shall i proceed",
        "should i proceed",
        "want me to proceed",
        "shall i continue",
        "should i continue",
        "want me to continue",
        "want me to go ahead",
        "shall i go ahead",
        "should i go ahead",
        "ok to proceed",
        "okay to proceed",
        "proceed?",
        "continue?",
        "go ahead?",
        "want me to start",
        "shall i start",
        "should i start",
        "ready to proceed",
        "let me know if you want me to",
    ];
    if PROCEED.iter().any(|p| hay.contains(p)) {
        return Pause::Proceed;
    }
    if ends_question {
        return Pause::Fork;
    }
    Pause::Statement
}

#[cfg(test)]
mod tests {
    use super::*;
    #[allow(unused_imports)]
    use crate::testutil::*;
    #[allow(unused_imports)]
    use std::{env, fs};

    #[test]
    fn classify_message_routes_pauses() {
        // a permission prompt dominates regardless of text
        assert_eq!(classify_message("anything", true), Pause::NeedsInput);
        // trivial "may I continue" endings → batch-resolvable
        assert_eq!(
            classify_message("Done with step 1.\nShall I proceed to step 2?", false),
            Pause::Proceed
        );
        assert_eq!(
            classify_message("Want me to continue?", false),
            Pause::Proceed
        );
        assert_eq!(
            classify_message("Tests pass. Should I go ahead and merge?", false),
            Pause::Proceed
        );
        // a real question that isn't a rote proceed → a genuine fork
        assert_eq!(
            classify_message("Two schemas are possible. Which one do you want?", false),
            Pause::Fork
        );
        // a report with a '?' earlier but a statement ending → not asking
        assert_eq!(
            classify_message("Is the cache stale? I checked and refreshed it.", false),
            Pause::Statement
        );
        // plain sign-off
        assert_eq!(
            classify_message("All done; pushed the branch.", false),
            Pause::Statement
        );
        assert_eq!(classify_message("   ", false), Pause::None);
    }

    #[test]
    fn session_signal_reads_store_file() {
        let _g = env_lock();
        let dir = tempdir();
        let reg = dir.join("sandboxes.json");
        fs::write(
            &reg,
            r#"{"thing-x":{"branch":"x","dir":"/d","lastSeen":"","status":""}}"#,
        )
        .unwrap();
        fs::create_dir_all(dir.join("sessions")).unwrap();
        fs::write(
            dir.join("sessions").join("thing-x.json"),
            r#"{"ts":"2026-06-28T00:00:00Z","kind":"stop","lastMessage":"hi"}"#,
        )
        .unwrap();
        env::set_var("SKEIN_REGISTRY", &reg);
        env::remove_var("SKEIN_SHARED");

        let s = session_signal("thing-x").expect("signal present");
        assert_eq!(s.kind, "stop");
        assert_eq!(s.last_message, "hi");
        assert!(session_signal("thing-missing").is_none());
        assert!(session_signal("../escape").is_none());

        env::remove_var("SKEIN_REGISTRY");
    }

    #[test]
    fn current_task_prefers_live_then_journal() {
        let _g = env_lock();
        let dir = tempdir();
        let work = dir.join("work");
        fs::create_dir_all(work.join(".skein")).unwrap();
        let reg = dir.join("sandboxes.json");
        fs::write(
            &reg,
            format!(
                r#"{{"thing-x":{{"branch":"x","dir":"{}","lastSeen":"","status":""}}}}"#,
                work.display()
            ),
        )
        .unwrap();
        env::set_var("SKEIN_REGISTRY", &reg);
        env::remove_var("SKEIN_SHARED");

        // journal-only fallback: the `next …` clause of the last line, stopping at the blocked-on part.
        fs::write(
            work.join(".skein").join("journal.md"),
            "did: scaffolded api / next: wire the reducer / blocked-on: nothing\n",
        )
        .unwrap();
        assert_eq!(
            current_task("thing-x").as_deref(),
            Some("wire the reducer")
        );

        // the live task signal wins over the journal.
        fs::create_dir_all(dir.join("tasks")).unwrap();
        fs::write(
            dir.join("tasks").join("thing-x.json"),
            r#"{"ts":"2026-06-29T00:00:00Z","task":"Running the tests"}"#,
        )
        .unwrap();
        assert_eq!(
            current_task("thing-x").as_deref(),
            Some("Running the tests")
        );

        // an empty live task falls back to the journal again.
        fs::write(
            dir.join("tasks").join("thing-x.json"),
            r#"{"ts":"2026-06-29T00:00:00Z","task":""}"#,
        )
        .unwrap();
        assert_eq!(
            current_task("thing-x").as_deref(),
            Some("wire the reducer")
        );

        assert!(current_task("../escape").is_none()); // name guard

        env::remove_var("SKEIN_REGISTRY");
    }

    #[test]
    fn classify_pane_reads_a_real_permission_prompt() {
        assert_eq!(
            classify_pane("claude", &obs(PERMISSION_TAIL)),
            Screen::Blocked(Blocked::Permission)
        );
        // …and the moment it is answered the same predicate says "not blocked" — the whole point of
        // a level signal: nothing has to fire an event for the chip to clear.
        assert_eq!(
            classify_pane("claude", &obs(ANSWERED_TAIL)),
            Screen::Waiting
        );
    }

    #[test]
    fn classify_pane_separates_the_four_blocking_kinds() {
        let question = &[
            " Would you like to proceed with this plan?",
            " ❯ 1. Yes, and auto-accept edits",
            "   2. No, keep planning",
        ];
        assert_eq!(
            classify_pane("claude", &obs(question)),
            Screen::Blocked(Blocked::Question)
        );
        let trust = &[
            " Do you trust the files in this folder?",
            " ❯ 1. Yes, proceed",
            "   2. No, exit",
        ];
        assert_eq!(
            classify_pane("claude", &obs(trust)),
            Screen::Blocked(Blocked::Trust)
        );
        // Quota reads like an error but the move is yours, so it ranks as "needs you", not "error".
        let quota = &[
            "✗ Claude usage limit reached · resets at 3pm",
            "❯ ",
            "  ? for shortcuts",
        ];
        assert_eq!(
            classify_pane("claude", &obs(quota)),
            Screen::Blocked(Blocked::Auth)
        );
        let api = &[
            "  ⎿  API Error: 500 Internal Server Error",
            "❯ ",
            "  ? for shortcuts",
        ];
        assert!(matches!(
            classify_pane("claude", &obs(api)),
            Screen::Error(_)
        ));
    }

    #[test]
    fn classify_pane_busy_survives_the_composer_being_visible() {
        // A real working pane: the spinner line carries the token counter, and the hint line is
        // still on screen — so "composer present" must not be read as "idle".
        let busy = &[
            "  Searched for 6 patterns, read 2 files",
            "✢ Whirlpooling… (5m 13s · ↓ 16.1k tokens)",
            "  ⏵⏵ auto mode on (shift+tab to cycle)",
        ];
        assert_eq!(classify_pane("claude", &obs(busy)), Screen::Busy);
        // The other half of the same claim, from the title: braille spins, ✳ does not.
        let mut spinning = obs(&["  ⏸ manual mode on · ? for shortcuts"]);
        spinning.title = "⠂ Claude Code".into();
        assert_eq!(classify_pane("claude", &spinning), Screen::Busy);
        let mut idle = obs(&["  ⏸ manual mode on · ? for shortcuts"]);
        idle.title = "✳ Claude Code".into();
        assert_eq!(classify_pane("claude", &idle), Screen::Waiting);
    }

    #[test]
    fn classify_pane_will_not_be_fooled_by_the_agent_talking_about_dialogs() {
        // This very repo's docs contain the sentence below. Prose is not a dialog: without an option
        // list, and with the composer on screen, it is just a box that is waiting for you.
        let prose = &[
            "● I asked: \"Do you want to allow Claude to fetch this content?\" and it said yes.",
            "✻ Baked for 12s",
            "❯ ",
            "  ⏸ manual mode on · ? for shortcuts",
        ];
        assert_eq!(classify_pane("claude", &obs(prose)), Screen::Waiting);
    }

    #[test]
    fn classify_pane_sees_a_dead_agent_and_defers_on_other_runtimes() {
        let mut dead = obs(&[]);
        dead.dead = 1;
        assert_eq!(classify_pane("claude", &dead), Screen::Dead);
        // The launch guard dropped to a shell: the TUI is gone, so the box is not "working".
        let shell = &[
            "skein: claude could not start — see the error above; keeping this session as a shell",
            "agent@skein-box:~/work/skein$",
        ];
        assert_eq!(classify_pane("claude", &obs(shell)), Screen::Dead);
        // A runtime nobody has read the screen of must defer to the hook edges rather than guess.
        assert_eq!(
            classify_pane("gemini", &obs(PERMISSION_TAIL)),
            Screen::Unknown
        );
        // …but a dead window needs no grammar, so that still reports across runtimes.
        assert_eq!(classify_pane("gemini", &dead), Screen::Dead);
        assert_eq!(classify_pane("codex", &dead), Screen::Dead);
    }

    /// **Boxes outlive the skein that installed them**, and the marker that says so was read by
    /// nothing.
    ///
    /// `box-pane.sh` has written `"contract":1` into every observation since it existed. `PaneObs`
    /// had no field for it, and serde ignores what it does not know — so a probe from a newer skein
    /// could change what `tail` or `title_age` mean and this build would parse it as though nothing
    /// had happened, then show a state derived from fields that no longer say what it thinks.
    ///
    /// The rule is the launcher's, applied to data: an unfamiliar version is SKIPPED, not
    /// evaluated. There, evaluating an unrecognised ceiling under `set -u` aborted the shell and
    /// took a fleet's boxes down. Here it would be quieter and worse.
    #[test]
    fn an_observation_from_a_newer_probe_is_skipped_rather_than_parsed() {
        let at = |contract: u32| PaneObs {
            contract,
            ts: Utc::now().timestamp(),
            tail: vec!["❯ ".into()],
            ..Default::default()
        };
        // What ships today, and what shipped before the marker existed. Both are readable.
        assert!(pane_is_readable(&at(PANE_CONTRACT)));
        assert!(
            pane_is_readable(&at(0)),
            "a probe older than the marker is not a fault — that shape is what shipped"
        );
        assert!(!pane_is_readable(&at(PANE_CONTRACT + 1)));

        // And the row says which, with its own advice: restarting the box reinstalls the probe this
        // build matches. Calling it `stale` would send somebody to restart an observer that is
        // working perfectly, and `unreadable` would send them to report a TUI change that has not
        // happened.
        assert_eq!(
            screen_health("claude", Some(&at(PANE_CONTRACT + 1)), true),
            "newer"
        );
        assert_eq!(screen_health("claude", Some(&at(PANE_CONTRACT)), true), "");

        // The probe and the reader agree on the number, which is the whole point of having one.
        assert!(
            crate::probes::PROBE_PANE_SH.contains(&format!("\"contract\":{PANE_CONTRACT},")),
            "the probe writes a contract this build does not claim to understand"
        );
    }

    #[test]
    fn screen_health_says_which_half_of_turn_state_is_actually_running() {
        let fresh = || PaneObs {
            ts: Utc::now().timestamp(),
            tail: ANSWERED_TAIL.iter().map(|l| l.to_string()).collect(),
            ..Default::default()
        };
        // Reading the screen: no caveat to show.
        assert_eq!(screen_health("claude", Some(&fresh()), true), "");
        // No observer has ever written: the common case until a box is reattached.
        assert_eq!(screen_health("claude", None, true), "none");
        // An observer that stopped — the agent session went away, or it was killed.
        let stopped = PaneObs {
            ts: Utc::now().timestamp() - (PANE_FRESH_SECS + 5),
            ..fresh()
        };
        assert_eq!(screen_health("claude", Some(&stopped), true), "stale");
        // A screen the grammar does not recognise. Distinct from "stale" because the fix is
        // different: this one is a skein bug to report, not a box to reattach.
        let unreadable = PaneObs {
            tail: vec!["something no grammar has ever seen".into()],
            ..fresh()
        };
        assert_eq!(
            screen_health("claude", Some(&unreadable), true),
            "unreadable"
        );
        // A runtime with no grammar at all is hooks-only by design, not by fault.
        assert_eq!(screen_health("gemini", None, true), "unsupported");
        assert!(has_screen_grammar("claude") && has_screen_grammar("codex"));
        assert!(!has_screen_grammar("gemini"));
        // A box that isn't running has no screen to read, so there is nothing to caveat.
        for h in [None, Some(&fresh()), Some(&stopped)] {
            assert_eq!(screen_health("claude", h, false), "");
        }
        // A crashed agent is a real reading, not a failure to read one.
        let dead = PaneObs { dead: 1, ..fresh() };
        assert_eq!(screen_health("claude", Some(&dead), true), "");
    }

    #[test]
    fn each_runtimes_grammar_owns_its_glyphs_because_skein_knows_the_runtime() {
        // The runtime is never inferred from the screen — `load_views` resolves it from sbx metadata,
        // the box's launch spec, or the repo default, and hands it to `classify_pane`. So each table
        // reads only its own selection glyph, and one agent showing the *other's* dialog — pasted into
        // a message, or quoted in a doc, both routine in this repo — is not a live dialog.
        let codex_dialog = &[
            "  Would you like to run the following command?",
            "› 1. Yes, proceed (y)",
            "  2. No, and tell Codex what to do differently (esc)",
            "  Press enter to confirm or esc to cancel",
        ];
        assert_eq!(
            classify_pane("codex", &obs(codex_dialog)),
            Screen::Blocked(Blocked::Permission)
        );
        assert_eq!(classify_pane("claude", &obs(codex_dialog)), Screen::Unknown);
        assert!(is_option_line("› 1. Yes, proceed (y)", &['›', '>']));
        assert!(!is_option_line("› 1. Yes, proceed (y)", &['❯', '>']));
        // And the marker itself is required: an unmarked numbered row is just a numbered list, which
        // agents write all the time — including in the message this test was written from.
        assert!(!is_option_line("  2. No, keep planning", &['❯', '>']));
        assert!(!is_option_line(
            "  2. The observer was capturing scrollback, so a status line stayed current.",
            &['❯', '>']
        ));
    }

    #[test]
    fn claude_busy_holds_still_across_the_status_lines_shifting_tail() {
        // Sampled from a real skein box every 2s through four minutes of continuous work. The old
        // predicate matched the *end* of this line (`tokens)`), so consecutive samples classified
        // Busy / Waiting / Waiting / Busy — the board flapping the user reported. All four are the
        // same state and must classify identically.
        for line in [
            "✽ Beboppin'… (3m 39s · ↓ 12.5k tokens)",
            "✽ Beboppin'… (3m 43s · ↓ 12.9k tokens · thinking)",
            "✻ Beboppin'… (4m 6s · ↓ 13.7k tokens · thought for 10s)",
            "· Beboppin'… (1m 16s · ↓ 500 tokens · thought for 70s)",
            "✢ Whirlpooling… (5m 13s · ↓ 16.1k tokens)",
            "✻ Thinking… (2s)",
            // The animation's whole observed frame set — `· ✢ * ✶ ✻ ✽`, including the plain ASCII
            // `*`, which an earlier draft of this predicate threw out as "markdown, not a spinner".
            "* Beboppin'… (13m 7s · ↓ 40.4k tokens)",
            "✶ Beboppin'… (13m 19s · ↓ 40.6k tokens)",
        ] {
            assert!(
                is_working_status_line(line),
                "should read as working: {line}"
            );
        }
        // …and the whole pane, as the observer really recorded it mid-turn: a configured statusline,
        // no `esc to interrupt` anywhere on screen, and a title whose glyph is `_`, not braille. Every
        // other busy signal is absent, so the status line has to carry this alone.
        let mut real = obs(&[
            "✻ Sautéed for 24m 3s",
            "● Running 4 shell commands…",
            "  ⎿  $ python3 -c \"import json\"",
            "✽ Beboppin'… (3m 43s · ↓ 12.9k tokens · thinking)",
            "──────────────────────────────────────────",
            "❯\u{a0}",
            "──────────────────────────────────────────",
            "  CTX █░░░░ 16% 163.4k/1.0M │ 5H ░░░░ 0%→0% 4h50m left │ $8.80 │ Opus 5",
            "  ⏵⏵ auto mode on (shift+tab to cycle) · ← for agents",
        ]);
        real.title = "_ Claude Code".into();
        real.title_age = 2000;
        assert_eq!(classify_pane("claude", &real), Screen::Busy);
    }

    #[test]
    fn claude_is_waiting_once_the_status_line_becomes_a_completion_marker() {
        // The same pane after the turn ends: the status line is replaced in place by `… for <time>`,
        // which must NOT read as working — it stays on screen for the whole of the next turn.
        let mut idle = obs(&[
            "● Right. First, evidence from a real skein box — my own.",
            "✻ Sautéed for 24m 3s",
            "● Running 4 shell commands…",
            "──────────────────────────────────────────",
            "❯\u{a0}",
            "──────────────────────────────────────────",
            "  CTX █░░░░ 16% 163.4k/1.0M │ $8.80 │ Opus 5",
            "  ⏵⏵ auto mode on (shift+tab to cycle) · ← for agents",
        ]);
        idle.title = "_ Claude Code".into();
        assert_eq!(classify_pane("claude", &idle), Screen::Waiting);
        // A tool announcement carries the ellipsis but no elapsed time, so it is not the status line.
        assert!(!is_working_status_line("● Running 4 shell commands…"));
        assert!(!is_working_status_line(
            "● Searching for 2 patterns, running 5 shell commands…"
        ));
        assert!(!is_working_status_line("✻ Sautéed for 24m 3s"));
        // The agent's own prose about elapsed times is not a status line either.
        assert!(!is_working_status_line(
            "  and the sampler ran… (30s of wall clock) before I stopped it"
        ));
        // Nor is a captured status line the agent is *displaying* — a quoted fixture, or a diff line
        // (both were on this box's screen while this very test was being written).
        assert!(!is_working_status_line(
            "            \"✽ Beboppin'… (3m 39s · ↓ 12.5k tokens)\","
        ));
        assert!(!is_working_status_line(
            "      8171 +            \"✽ Beboppin'… (3m 39s)\""
        ));
    }

    #[test]
    fn a_status_line_scrolled_up_the_screen_is_not_the_current_one() {
        // Same line, two positions. Directly above the composer it is the pane's own status line;
        // twelve lines up it is the agent showing one — a log, a capture, an earlier turn left on a
        // screen that has since gone quiet.
        let composer = [
            "──────────────────────────────────────────",
            "❯\u{a0}",
            "──────────────────────────────────────────",
            "  ⏵⏵ auto mode on (shift+tab to cycle)",
        ];
        let mut live: Vec<&str> =
            vec!["● reading a file", "✽ Beboppin'… (3m 39s · ↓ 12.5k tokens)"];
        live.extend(composer);
        assert_eq!(classify_pane("claude", &obs(&live)), Screen::Busy);
        let mut displayed: Vec<&str> = vec!["✽ Beboppin'… (3m 39s · ↓ 12.5k tokens)"];
        displayed.extend([
            "● one", "  two", "  three", "  four", "  five", "  six", "  seven",
        ]);
        displayed.extend(composer);
        assert_eq!(classify_pane("claude", &obs(&displayed)), Screen::Waiting);
    }

    /// A pane captured from a live fleet box, byte-for-byte as `box-pane.sh` recorded it (trailing
    /// whitespace stripped). The metadata travels with it because the grammar reads the title too.
    fn captured(fixture: &str, title: &str, title_age: i64) -> PaneObs {
        PaneObs {
            ts: 1,
            title: title.into(),
            title_age,
            tail: fixture.lines().map(str::to_string).collect(),
            ..Default::default()
        }
    }

    #[test]
    fn a_turn_parked_on_background_agents_reads_busy_not_waiting() {
        // Captured live 2026-08-24 from example-box-6 (a real box, operator statusline, agents
        // panel open): the pane redrew every second (age 0-1, moving 1 on eight samples 4s apart)
        // with `✻ Waiting for 4 background agents to finish` as its status line — a turn in
        // progress — while the board read `waiting` for nine minutes. Two defects at once, and a
        // clean-room box shows neither: the agents panel below the footer (one row per agent)
        // pushed the status line out of a bottom-anchored 10-line window, and the line itself
        // carries no parenthesised elapsed time for `is_working_status_line` to match.
        for fixture in [
            include_str!(
                "../tests/fixtures/panes/claude-waiting.example-box-6.agents-panel.2026-08-24.a.txt"
            ),
            include_str!(
                "../tests/fixtures/panes/claude-waiting.example-box-6.agents-panel.2026-08-24.b.txt"
            ),
        ] {
            let real = captured(fixture, "_ example-box-6", 12325);
            assert_eq!(
                classify_pane("claude", &real),
                Screen::Busy,
                "a box waiting on its own background agents is mid-turn — it needs nobody"
            );
        }
        assert!(is_waiting_on_agents_line(
            "✻ Waiting for 4 background agents to finish"
        ));
        // The panel's own rows, and a quoted copy of the line, are not the status line.
        assert!(!is_waiting_on_agents_line(
            "  ◯ general-purpose  Screen grammar vs real fleet panes    4m 48s · ↓ 91.8k tokens"
        ));
        assert!(!is_waiting_on_agents_line(
            "\"✻ Waiting for 4 background agents to finish\","
        ));
    }

    #[test]
    fn a_finished_compact_in_the_hook_log_is_not_live_compaction() {
        // Captured live 2026-08-24 from lattice-feat-design-codex-claude: idle for 8+ minutes after
        // a `/compact` (age 501, moving 0, completion marker `✻ Brewed for 21m 29s`, bare composer),
        // yet the board said `working` — and kept saying it for 37 minutes — because the *hook log*
        // of the finished compact (`PreCompact [… box-status.sh compacting] completed successfully`)
        // sat in the tail and `any("compacting")` read it as live compaction. The word only counts
        // in the status region; this pane's own store-installed hooks put it on screen after every
        // compact, so this is every fleet box, not an exotic one.
        let real = captured(
            include_str!(
                "../tests/fixtures/panes/claude-working.lattice.idle-after-compact.2026-08-24.txt"
            ),
            "_ lattice-feat-design-codex-claude",
            11841,
        );
        assert_eq!(
            classify_pane("claude", &real),
            Screen::Waiting,
            "a finished compact's hook log is history, not a live compaction"
        );
        // …while the word directly above the composer is the pane's own state. (Constructed, not a
        // capture: no live compaction was on any fleet screen while this was written — its shape is
        // unverified, which is why the needle stays a bare substring rather than a line shape.)
        let compacting = &[
            "✻ Compacting conversation…",
            "──────────────────────────────────────────",
            "❯\u{a0}",
            "──────────────────────────────────────────",
            "  ⏵⏵ auto mode on (shift+tab to cycle)",
        ];
        assert_eq!(classify_pane("claude", &obs(compacting)), Screen::Busy);
    }

    #[test]
    fn claude_composer_survives_a_configured_statusline() {
        // A box whose statusline replaces the hint line, and whose mode footer is off screen: the
        // bare prompt row between the rules is still proof the TUI is alive and taking input, so the
        // box reads `waiting` instead of falling back to a stale edge that says `working`.
        let bare = &[
            "✻ Sautéed for 14m 21s",
            "──────────────────────────────────────────",
            "❯\u{a0}",
            "──────────────────────────────────────────",
            "  CTX █░░░ 0% 0/1.0M │ 5H ███ 53%→54% 4m left │ 7D ███ 28%→31%",
        ];
        assert_eq!(classify_pane("claude", &obs(bare)), Screen::Waiting);
    }

    #[test]
    fn claude_busy_outranks_an_error_line_left_over_in_the_tail() {
        // An "API Error" from the previous turn is still in the visible tail while the next turn is
        // running. The turn is demonstrably live, so there is nothing for a human to do — reading
        // history as current state is the same fault as latching an edge.
        let retrying = &[
            "  ⎿  API Error: 500 Internal Server Error",
            "✻ Whirlpooling… (12s · ↓ 1.2k tokens)",
            "  ⏸ manual mode on · ? for shortcuts",
        ];
        assert_eq!(classify_pane("claude", &obs(retrying)), Screen::Busy);
    }

    // ---------- Codex 0.145.0, captured live from a box (docs/turn-state.md §6b) ----------

    /// A shell-command approval, exactly as Codex draws it. Note there is no composer footer: a
    /// dialog *replaces* it.
    const CODEX_APPROVAL: &[&str] = &[
        "› run the shell command: date",
        "• I’ll run date and report its output.",
        "• Running date",
        "  Would you like to run the following command?",
        "  Environment: local",
        "  $ date",
        "› 1. Yes, proceed (y)",
        "  2. Yes, and don't ask again for commands that start with `date` (p)",
        "  3. No, and tell Codex what to do differently (esc)",
        "  Press enter to confirm or esc to cancel",
    ];

    /// The same pane after pressing esc — and a minefield: a `■ ` notice line, a monthly-limit
    /// warning, and the user's own prompt echoed with the same `› ` glyph the options use.
    const CODEX_ANSWERED: &[&str] = &[
        "› run the shell command: date",
        "• I’ll run date and report its output.",
        "✗ You canceled the request to run date",
        "• Ran date",
        "  └ (no output)",
        "⚠ Heads up, you have less than 25% of your monthly limit left. Run /status for a breakdown.",
        "■ Conversation interrupted - tell the model what to do differently. Something went wrong? Hit `/feedback` to report the",
        "issue.",
        "› Improve documentation in @filename",
        "  ? for shortcuts                                          100% context left",
    ];

    #[test]
    fn classify_pane_reads_codexs_approval_and_watches_it_clear() {
        let mut dialog = obs(CODEX_APPROVAL);
        dialog.title = "[ ! ] Action Required | skein".into();
        assert_eq!(
            classify_pane("codex", &dialog),
            Screen::Blocked(Blocked::Permission)
        );
        // One keystroke later: no confirm footer, composer back, title back to the plain directory.
        let mut answered = obs(CODEX_ANSWERED);
        answered.title = "skein".into();
        assert_eq!(classify_pane("codex", &answered), Screen::Waiting);
    }

    #[test]
    fn classify_pane_reads_codexs_edit_approval_too() {
        // A different body under the same footer — which is why the footer, not the wording of any
        // one dialog, is the predicate.
        let mut edits = obs(&[
            "• Added scratch-hello.txt (+1 -0)",
            "    1 +hello",
            "  Would you like to make the following edits?",
            "› 1. Yes, proceed (y)",
            "  2. Yes, and don't ask again for these files (a)",
            "  3. No, and tell Codex what to do differently (esc)",
            "  Press enter to confirm or esc to cancel",
        ]);
        edits.title = "[ . ] Action Required | skein".into();
        assert_eq!(
            classify_pane("codex", &edits),
            Screen::Blocked(Blocked::Permission)
        );
    }

    #[test]
    fn classify_pane_names_the_wall_codex_puts_in_front_of_skeins_own_hooks() {
        // skein installs its probes into every store, so "N hooks are new or changed" is the trust
        // gate a skein box really hits — and it blocks *before* any hook could fire to report it.
        let trust = &[
            "  Hooks need review",
            "  19 hooks are new or changed.",
            "  Hooks can run outside the sandbox after you trust them.",
            "› 1. Review hooks",
            "  2. Trust all and continue",
            "  3. Continue without trusting (hooks won't run)",
            "  Press enter to confirm or esc to go back",
        ];
        assert_eq!(
            classify_pane("codex", &obs(trust)),
            Screen::Blocked(Blocked::Trust)
        );
        // Onboarding with no credentials: the options switch to a plain `> 1.` and the footer to
        // "continue", and nothing will ever run until a human signs in.
        let signin = &[
            "  Welcome to Codex, OpenAI's command-line coding agent",
            "  Sign in with ChatGPT to use Codex as part of your paid plan",
            "  or connect an API key for usage-based billing",
            "> 1. Sign in with ChatGPT",
            "     Usage included with Plus, Pro, Business, and Enterprise plans",
            "  2. Sign in with Device Code",
            "  3. Provide your own API key",
            "  Press enter to continue",
        ];
        assert_eq!(
            classify_pane("codex", &obs(signin)),
            Screen::Blocked(Blocked::Auth)
        );
        // A picker is a question, not a permission: nothing is waiting on a yes.
        let picker = &[
            "  Select a model",
            "› 1. gpt-5.6-sol (current)",
            "  2. gpt-5.6-terra",
            "  Press enter to confirm or esc to go back",
        ];
        assert_eq!(
            classify_pane("codex", &obs(picker)),
            Screen::Blocked(Blocked::Question)
        );
    }

    #[test]
    fn codexs_action_required_title_covers_a_dialog_we_have_no_wording_for() {
        // The next Codex release can reword any dialog body; the title marker is the backstop, and it
        // is honest about not knowing which kind. Verified to appear only while a decision is
        // pending — it clears on approve *and* on esc, and stays clear through a finished turn.
        let mut unknown_dialog = obs(&["  Some future prompt nobody has captured", "  ▸ pick one"]);
        unknown_dialog.title = "[ ! ] Action Required | skein".into();
        assert_eq!(
            classify_pane("codex", &unknown_dialog),
            Screen::Blocked(Blocked::Question)
        );
        // The bracket animates, so only the words can be matched.
        assert!(title_has_attention("[ ! ] Action Required | skein"));
        assert!(title_has_attention("[ . ] Action Required | skein"));
        assert!(!title_has_attention("⠧ skein"));
    }

    #[test]
    fn classify_pane_reads_codexs_working_line_and_its_notices() {
        // Codex keeps the composer on screen while it works, so "composer present" cannot mean idle.
        let mut busy = obs(&[
            "› run the shell command: date",
            "• Working (1s • esc to interrupt)",
            "› Improve documentation in @filename",
            "  ? for shortcuts                                          100% context left",
        ]);
        busy.title = "⠴ skein".into();
        assert_eq!(classify_pane("codex", &busy), Screen::Busy);
        // A failure is a `■ ` line carrying a JSON payload…
        let failed = &[
            "› run the shell command: date",
            "■ {\"detail\":\"The 'gpt-5.6-sol' model is not supported when using Codex with a ChatGPT account.\"}",
            "  ? for shortcuts                                          100% context left",
        ];
        assert!(matches!(
            classify_pane("codex", &obs(failed)),
            Screen::Error(_)
        ));
        // …while the prose `■ ` notice and the monthly-limit warning in CODEX_ANSWERED are neither an
        // error nor an auth block. That pane is simply waiting for you.
        assert_eq!(
            classify_pane("codex", &obs(CODEX_ANSWERED)),
            Screen::Waiting
        );
    }

    /// **No displayed state rests on an edge alone without saying so.** The law, as a matrix.
    ///
    /// Three of the four fusion rules can display a state no level observation supports, and each
    /// is right — showing nothing would be worse than showing an edge. What makes them honest is
    /// that the row says which, and rule 3 said nothing at all until `StatusFrom` existed: it fires
    /// with a healthy observer, so `screen_health` is empty and the badge renders nothing.
    ///
    /// Every case here is one where an edge is what you see. If one of them ever reports
    /// `StatusFrom::Screen`, the board is claiming an observation it does not have.
    #[test]
    fn every_state_that_rests_on_an_edge_says_so() {
        let edge = |ts: i64| Some(("blocked".to_string(), ts));
        struct Case {
            what: &'static str,
            level: Option<(Screen, i64)>,
            edge: Option<(String, i64)>,
            want: StatusFrom,
        }
        let case = |what, level, edge, want| Case {
            what,
            level,
            edge,
            want,
        };
        let cases = [
            // Rule 1 — no observation at all. Disclosed by `screen_health` as "none"/"stale".
            case("no observation", None, edge(10), StatusFrom::Edge),
            // Rule 4 — a sample the grammar does not recognise. Disclosed as "unreadable".
            case(
                "unreadable screen",
                Some((Screen::Unknown, 10)),
                edge(5),
                StatusFrom::Edge,
            ),
            // Rule 3 — a fresh, READABLE screen, beaten by a newer edge. Nothing else discloses it.
            case(
                "edge newer than a healthy screen",
                Some((Screen::Waiting, 100)),
                edge(200),
                StatusFrom::EdgeAheadOfScreen,
            ),
            // And the case that is NOT edge-led, so the matrix cannot pass by always saying "edge".
            case(
                "the screen decided",
                Some((Screen::Waiting, 200)),
                edge(100),
                StatusFrom::Screen,
            ),
        ];
        for Case {
            what,
            level,
            edge,
            want,
        } in cases
        {
            let (_, _, from) = fuse_status(edge, level);
            assert_eq!(from, want, "{what}");
        }
    }

    /// **Recovery**, case by case — the property the fusion rules exist for.
    ///
    /// The bug class is precise: *an edge-triggered latch with incomplete edge coverage cannot
    /// recover, and a state nobody clears is shown for ever.* So the cases worth enumerating are not
    /// the ones where the edges arrive correctly — they are the ones where an edge is missing, late,
    /// unknown or impossible, and the question each asks is the same: **does the next level sample
    /// clear it, or does it latch?**
    ///
    /// Each case names what it is a case OF, because a table of inputs and outputs with no names is
    /// a list of things somebody once observed rather than a statement about the system.
    #[test]
    fn a_state_nobody_cleared_is_cleared_by_the_next_level_sample() {
        struct Case {
            /// What went wrong upstream — the reason this edge is the way it is.
            what: &'static str,
            edge: Option<(String, i64)>,
            /// The level sample that arrives afterwards, and must settle it.
            level: Option<(Screen, i64)>,
            recovers_to: Option<&'static str>,
        }
        let blocked = |ts: i64| Some(("blocked".to_string(), ts));
        let cases = [
            Case {
                // A Notification fired and the Stop that would clear it never did — the turn ended
                // in a way the hook did not see. This is the twenty-minute bug in one line.
                what: "a hook that never fired",
                edge: blocked(1000),
                level: Some((Screen::Waiting, 1200)),
                recovers_to: Some("waiting"),
            },
            Case {
                // The box died between the event and the write, so the edge is the last thing
                // anyone said about it and the screen is now a shell prompt.
                what: "a process that died after the event",
                edge: blocked(1000),
                level: Some((Screen::Dead, 1200)),
                recovers_to: Some("ended"),
            },
            Case {
                // Two writers, and the older one landed last. The level sample is newer than both,
                // so it decides — which is what makes ordering a non-problem rather than a fix.
                what: "an edge that arrived out of order",
                edge: blocked(900),
                level: Some((Screen::Busy, 1200)),
                recovers_to: Some("working"),
            },
            Case {
                // A probe from a newer skein, or a typo in a hook: a status string this build has
                // no meaning for. It must not be treated as an outcome that outranks the screen.
                what: "an edge whose status is not one skein knows",
                edge: Some(("marinating".to_string(), 1300)),
                level: Some((Screen::Waiting, 1200)),
                recovers_to: Some("waiting"),
            },
            Case {
                // Clock skew between host and guest. `pane_is_fresh` tolerates a future-dated
                // sample on purpose — dropping it would silently disable the whole level layer —
                // and fusion must too, or skew becomes a latch.
                what: "a level sample from a clock that runs fast",
                edge: blocked(1000),
                level: Some((Screen::Waiting, 9_999_999_999)),
                recovers_to: Some("waiting"),
            },
            Case {
                // Nothing to recover from and nothing to recover with: liveness decides downstream,
                // and fusion must not invent a state to fill the gap.
                what: "no edge and no sample at all",
                edge: None,
                level: None,
                recovers_to: None,
            },
        ];
        for Case {
            what,
            edge,
            level,
            recovers_to,
        } in cases
        {
            let (state, _, _) = fuse_status(edge.clone(), level);
            assert_eq!(
                state.as_deref(),
                recovers_to,
                "{what}: the state did not settle where the level sample says it should"
            );
            // And the latch itself: with NO level sample the edge is still shown, which is right —
            // it is the only thing anyone has said. The pairing is the point. A rule that cleared
            // it here would throw away the only signal on a box with no observer at all.
            if let Some((status, _)) = &edge {
                let (latched, _, from) = fuse_status(edge.clone(), None);
                assert_eq!(
                    latched.as_deref(),
                    Some(status.as_str()),
                    "{what}: the edge was discarded when it was the only thing there was"
                );
                assert_eq!(
                    from,
                    StatusFrom::Edge,
                    "{what}: an edge shown alone must say that is what it is"
                );
            }
        }
    }

    /// An edge and a sample in the same second: the sample decides.
    ///
    /// The tie-break is `>` with a second of slack (`ts > level_ts + 1`), and it leans towards the
    /// screen on purpose. Hook and observer clocks are not the same clock, and the screen is
    /// re-readable while the edge is lossy — so when they are indistinguishable in time, the one
    /// that can correct itself next second should win.
    #[test]
    fn an_edge_and_a_sample_in_the_same_second_let_the_sample_decide() {
        for skew in [-1, 0, 1] {
            let (state, _, from) = fuse_status(
                Some(("blocked".to_string(), 1200 + skew)),
                Some((Screen::Waiting, 1200)),
            );
            assert_eq!(
                state.as_deref(),
                Some("waiting"),
                "an edge {skew}s from the sample outranked it"
            );
            assert_eq!(from, StatusFrom::Screen);
        }
        // Two seconds ahead is a genuinely later event, and leads.
        let (state, _, from) = fuse_status(
            Some(("blocked".to_string(), 1202)),
            Some((Screen::Waiting, 1200)),
        );
        assert_eq!(state.as_deref(), Some("blocked"));
        assert_eq!(from, StatusFrom::EdgeAheadOfScreen);
    }

    #[test]
    fn fuse_status_clears_an_edge_that_nothing_ever_cleared() {
        // The bug, as recorded in this box's own hook-log: `blocked` written at 13:27, nothing until
        // Stop at 13:47. A screen observation taken at 13:30 showing a composer ends it.
        let edge = Some(("blocked".to_string(), 1000));
        let level = Some((Screen::Waiting, 1180));
        assert_eq!(
            fuse_status(edge, level),
            (Some("waiting".into()), "", StatusFrom::Screen)
        );
    }

    #[test]
    fn fuse_status_lets_a_newer_edge_lead_then_the_next_sample_confirms() {
        // A Notification fires 5s after the last sample: show it at once (latency), don't wait.
        let edge = Some(("blocked".to_string(), 1205));
        let level = Some((Screen::Waiting, 1200));
        // Rule 3: the screen is fresh and readable, and the edge led anyway. The provenance is
        // the whole point — nothing else on the row says the state came from an edge, because the
        // observer is perfectly healthy.
        assert_eq!(
            fuse_status(edge.clone(), level),
            (Some("blocked".into()), "", StatusFrom::EdgeAheadOfScreen)
        );
        // The next sample sees the dialog and names which kind it is.
        assert_eq!(
            fuse_status(edge, Some((Screen::Blocked(Blocked::Permission), 1210))),
            (Some("blocked".into()), "permission", StatusFrom::Screen)
        );
    }

    #[test]
    fn fuse_status_without_an_observation_is_exactly_todays_behaviour() {
        for status in ["blocked", "working", "waiting", "error", "ended"] {
            assert_eq!(
                fuse_status(Some((status.to_string(), 10)), None),
                (Some(status.to_string()), "", StatusFrom::Edge)
            );
        }
        // An unreadable screen defers too, rather than inventing a state.
        assert_eq!(
            fuse_status(Some(("blocked".into(), 10)), Some((Screen::Unknown, 99))),
            (Some("blocked".into()), "", StatusFrom::Edge)
        );
        // No edge and no observation: nothing claimed, so liveness decides downstream.
        assert_eq!(fuse_status(None, None), (None, "", StatusFrom::Edge));
    }

    #[test]
    fn fuse_status_reports_a_crashed_agent_but_keeps_a_human_set_outcome() {
        assert_eq!(
            fuse_status(Some(("working".into(), 10)), Some((Screen::Dead, 20))),
            (Some("ended".into()), "", StatusFrom::Screen)
        );
        // `done` is a human's verdict on the work, not a claim about the process.
        assert_eq!(
            fuse_status(Some(("done".into(), 10)), Some((Screen::Dead, 20))),
            (Some("done".into()), "", StatusFrom::Screen)
        );
    }

    #[test]
    fn title_activity_names_the_tool_but_not_the_idle_title() {
        assert_eq!(
            title_activity("✳ Run bash command true").as_deref(),
            Some("Run bash command true")
        );
        assert_eq!(title_activity("⠂ Claude Code"), None);
        assert_eq!(title_activity("✳ Claude Code"), None);
        assert_eq!(title_activity(""), None);
    }

    #[test]
    fn title_text_is_only_a_task_while_it_is_demonstrably_fresh() {
        // Observed live: Claude Code kept a finished tool's description in the title through a later,
        // unrelated turn. So "busy" alone is not enough to believe the title's text — the observer
        // must have watched it change, and recently.
        let stale = PaneObs {
            ts: 1,
            title: "⠂ Fetch and quote robots.txt file".into(),
            title_age: 600,
            ..Default::default()
        };
        let fresh = PaneObs {
            title_age: 3,
            ..stale.clone()
        };
        let unwitnessed = PaneObs {
            title_age: -1,
            ..stale.clone()
        };
        let task_from_title = |runtime: &str, o: &PaneObs| {
            runtime == "claude"
                && (0..=TITLE_FRESH_SECS).contains(&o.title_age)
                && title_activity(&o.title).is_some()
        };
        let usable = |o: &PaneObs| task_from_title("claude", o);
        assert!(
            usable(&fresh),
            "a title seen changing 3s ago names current work"
        );
        assert!(
            !usable(&stale),
            "ten minutes old is the residue of an earlier tool call"
        );
        assert!(
            !usable(&unwitnessed),
            "never seen changing ⇒ no claim at all"
        );
        // Codex's title is the working directory, not the running tool, so it names no task however
        // fresh it is — otherwise the task column would just repeat the box's folder name.
        let codex = PaneObs {
            title: "⠧ skein".into(),
            title_age: 2,
            ..stale.clone()
        };
        assert!(!task_from_title("codex", &codex));
    }

    #[test]
    fn read_pane_ignores_an_observation_that_has_gone_stale() {
        let _g = env_lock();
        let store_tmp = tempdir();
        let store = store_tmp.join(".claude");
        fs::create_dir_all(store.join("status")).unwrap();
        let reg = store.join("sandboxes.json");
        fs::write(
            &reg,
            r#"{"thing-x":{"branch":"x","dir":"/d","lastSeen":"","status":""}}"#,
        )
        .unwrap();
        env::set_var("SKEIN_REGISTRY", &reg);
        let now = Utc::now().timestamp();
        let write = |ts: i64| {
            fs::write(
                store.join("status/thing-x.pane.json"),
                format!(r#"{{"contract":1,"ts":{ts},"age":0,"moving":1,"dead":0,"title":"⠂ Claude Code","cmd":"bash","tail":["❯ ","  ? for shortcuts"]}}"#),
            )
            .unwrap();
        };
        write(now);
        assert!(read_pane("thing-x").is_some(), "a fresh sample counts");
        write(now - (PANE_FRESH_SECS + 5));
        assert!(
            read_pane("thing-x").is_none(),
            "a sample older than three heartbeats must stop counting, so a dead observer degrades \
             to hook-only turn-state instead of freezing the board"
        );
        env::remove_var("SKEIN_REGISTRY");
    }

    fn obs(tail: &[&str]) -> PaneObs {
        PaneObs {
            ts: 1,
            tail: tail.iter().map(|l| l.to_string()).collect(),
            ..Default::default()
        }
    }

    /// The exact bottom-of-pane of a Claude permission prompt (a WebFetch approval).
    const PERMISSION_TAIL: &[&str] = &[
        "● Fetch(https://example.com/robots.txt)",
        "────────────────────────────────────────────────────────────",
        " Fetch",
        "   url: \"https://example.com/robots.txt\", prompt: \"Return the raw content\"",
        "   Claude wants to fetch content from example.com",
        " Do you want to allow Claude to fetch this content?",
        " ❯ 1. Yes",
        "   2. Yes, and don't ask again for example.com",
        "   3. No, and tell Claude what to do differently (esc)",
    ];

    /// The same pane one keystroke later: dialog gone, composer and hint line back.
    const ANSWERED_TAIL: &[&str] = &[
        "  ⎿  Interrupted · What should Claude do instead?",
        "✻ Baked for 1m 26s",
        "                                              ● high · /effort",
        "────────────────────────────────────────────────────────────",
        "❯ ",
        "────────────────────────────────────────────────────────────",
        "  ⏸ manual mode on · ? for shortcuts · ← for agents",
    ];
}
