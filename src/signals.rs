//! Turn state: what a box is doing, and whether anyone is waiting on you.
//!
//! Two halves. **Edges** are the runtime's lifecycle hooks — fast, but no runtime fires anything
//! when you answer a permission prompt, dismiss a dialog, interrupt a turn, or when the agent
//! dies, so a state nobody clears used to be shown forever. **Level** is what the box's screen
//! says right now, sampled by an in-box observer. Level clears itself; edges cannot.
//!
//! With no observation present, or a screen the grammar does not recognise, turn state is exactly
//! the edge signal it always was — and says so, rather than quietly degrading.

use crate::util::*;
use crate::{read_journal, store_for_box, valid_name};
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
}

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

/// The level observation for a box, or `None` when there is no observer, it died, or its last
/// sample is too old to trust.
pub fn read_pane(name: &str) -> Option<PaneObs> {
    read_pane_raw(name).filter(pane_is_fresh)
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
pub fn screen_health(runtime: &str, raw: Option<&PaneObs>, running: bool) -> &'static str {
    if !running {
        return "";
    }
    if !has_screen_grammar(runtime) {
        return "unsupported";
    }
    match raw {
        None => "none",
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
    let status_region = obs
        .tail
        .iter()
        .rev()
        .filter(|l| !l.trim().is_empty())
        .take(10);
    if status_region.clone().any(|l| is_working_status_line(l))
        || any("esc to interrupt")
        || any("compacting")
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

/// Fold the level observation into the edge status. Returns the effective status key plus the
/// blocking kind when there is one.
///
/// The four rules (docs/turn-state.md §4.3), in order:
///   1. no level observation ⇒ the edge, unchanged — older boxes behave exactly as before;
///   2. attention never latches: a fresh level observation *overrides* a stale edge that still
///      claims `blocked`/`error`, which is the twenty-minute bug;
///   3. edges lead: an edge newer than the sample wins, so a Notification shows instantly and the
///      next sample confirms or corrects it;
///   4. `Unknown` defers to the edge rather than guessing.
pub(crate) fn fuse_status(
    edge: Option<(String, i64)>,
    level: Option<(Screen, i64)>,
) -> (Option<String>, &'static str) {
    let (screen, level_ts) = match level {
        None => return (edge.map(|(s, _)| s), ""), // rule 1
        Some(pair) => pair,
    };
    let edge_leads = edge.as_ref().is_some_and(|(_, ts)| *ts > level_ts + 1);
    let edge_status = edge.as_ref().map(|(s, _)| s.as_str()).unwrap_or("");
    let edge_is_outcome = matches!(
        edge_status,
        "blocked" | "needs-input" | "needs-decision" | "error" | "ended" | "done" | "waiting"
    );
    match screen {
        Screen::Unknown => (edge.map(|(s, _)| s), ""), // rule 4
        // An edge that arrived *after* the sample is the fresher truth (rule 3).
        _ if edge_leads && edge_is_outcome => (edge.map(|(s, _)| s), ""),
        Screen::Blocked(kind) => (Some("blocked".into()), kind.key()),
        Screen::Busy => (Some("working".into()), ""),
        Screen::Waiting => (Some("waiting".into()), ""),
        Screen::Error(_) => (Some("error".into()), ""),
        // `done` is a human-set outcome, not something a screen can contradict.
        Screen::Dead if edge_status == "done" => (Some("done".into()), ""),
        Screen::Dead => (Some("ended".into()), ""),
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
