//! skein core — read the shared sbx registry and derive fleet views.
//!
//! This is the single source of truth shared by the CLI (`skein`) and the server
//! (`skein-server`). It owns no state the sandboxes don't already write; it only reads
//! `sandboxes.json` and derives status. See ARCHITECTURE.md.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Diff summary a box reports for its branch-vs-base work (written by box-diff.sh).
#[derive(Debug, Default, Clone, Deserialize, Serialize)]
pub struct DiffStat {
    #[serde(default)]
    pub files: u32,
    #[serde(default)]
    pub ins: u32,
    #[serde(default)]
    pub del: u32,
}

/// One cross-box message in the shared `mailbox/` (written by mailbox.sh or skein).
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Message {
    #[serde(default)]
    pub from: String,
    #[serde(default)]
    pub to: String, // a vmid, or "broadcast"
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub branch: String,
    #[serde(default)]
    pub body: String,
    #[serde(default)]
    pub ts: String,
    #[serde(default, rename = "seenBy")]
    pub seen_by: Vec<String>,
}

/// One entry in the shared `sandboxes.json` registry written by sandbox-bootstrap.sh.
#[derive(Debug, Default, Deserialize)]
pub struct Sandbox {
    #[serde(default)]
    pub branch: String,
    #[serde(default)]
    pub dir: String,
    #[serde(default, rename = "lastSeen")]
    pub last_seen: String,
    /// Set by the box status hook (box-status.sh); usually empty until a box reports.
    #[serde(default)]
    pub status: String,
    /// Branch-vs-base diff summary, reported by box-diff.sh. None until first report.
    #[serde(default)]
    pub diff: Option<DiffStat>,
}

/// A registry entry enriched for display — what the CLI table and the web API both render.
#[derive(Debug, Serialize)]
pub struct BoxView {
    pub name: String,
    pub state: String,
    /// 0 waiting/done-attention, sorted up; higher = quieter. See [`Sandbox::state`].
    pub tier: u8,
    pub branch: String,
    pub age: String,
    pub dir: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diff: Option<DiffStat>,
    /// one-line gist of the box's last reported signal (the inbox headline) — the blocking
    /// prompt when it's waiting on you, else the first line of its last message.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub headline: Option<String>,
    /// why the turn ended (the fork-detector) — lets the inbox label & batch the trivial asks.
    pub pause: Pause,
}

impl Sandbox {
    /// Human label + sort/colour tier, ordered "who needs me first" (lower = more urgent):
    ///   0 needs-input (a decision/permission is blocking the agent)
    ///   1 waiting     (turn ended — your move)
    ///   2 done        (task finished — review / merge)
    ///   3 working     (in flight — leave it alone) / `live` when no explicit status
    ///   4 idle        5 stale / unknown
    /// Prefers the explicit status the box's hooks write; falls back to liveness
    /// derived from `lastSeen` when no box has reported a status yet.
    pub fn state(&self) -> (String, u8) {
        match self.status.as_str() {
            "needs-input" | "needs-decision" | "blocked" => return ("needs-input".into(), 0),
            "waiting" => return ("waiting".into(), 1),
            "done" => return ("done".into(), 2),
            "working" | "running" => return ("working".into(), 3),
            "" => {} // derive from lastSeen below
            other => return (other.to_string(), 3),
        }
        match self.age_secs() {
            Some(s) if s < 120 => ("live".into(), 3),
            Some(s) if s < 1800 => ("idle".into(), 4),
            Some(_) => ("stale".into(), 5),
            None => ("unknown".into(), 5),
        }
    }

    fn age_secs(&self) -> Option<i64> {
        let t = DateTime::parse_from_rfc3339(&self.last_seen).ok()?;
        Some((Utc::now() - t.with_timezone(&Utc)).num_seconds())
    }

    pub fn age(&self) -> String {
        match self.age_secs() {
            None => "?".into(),
            Some(s) if s < 60 => format!("{s}s ago"),
            Some(s) if s < 3600 => format!("{}m ago", s / 60),
            Some(s) if s < 86400 => format!("{}h ago", s / 3600),
            Some(s) => format!("{}d ago", s / 86400),
        }
    }
}

/// A box name is a registry key / vmid — never a path or a shell token. Reject anything that
/// could escape the store dir on a filesystem join (`..`, separators, NUL). The server validates
/// every `:name` route with this, and the path-touching lib fns guard with it too so the check
/// can't be bypassed by a non-HTTP caller.
pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && !name.contains("..")
        && !name.contains(['/', '\\', '\0'])
}

/// Write `bytes` to `path` atomically: a temp file in the same dir, then rename (POSIX-atomic),
/// so a concurrent reader sees either the old or the new whole file, never a truncated one.
/// `dir` must be `path`'s parent (same filesystem) for the rename to be atomic.
fn write_atomic(path: &Path, dir: &Path, bytes: &[u8]) -> Result<(), String> {
    let tmp = dir.join(format!(".skein.tmp.{}", std::process::id()));
    fs::write(&tmp, bytes).map_err(|e| format!("writing temp: {e}"))?;
    fs::rename(&tmp, path).map_err(|e| {
        let _ = fs::remove_file(&tmp);
        format!("renaming into place: {e}")
    })
}

pub fn locate_registry() -> Result<PathBuf, String> {
    if let Ok(p) = env::var("SKEIN_REGISTRY") {
        if !p.is_empty() {
            return Ok(PathBuf::from(p));
        }
    }
    if let Ok(p) = env::var("SKEIN_SHARED") {
        if !p.is_empty() {
            return Ok(PathBuf::from(p).join("sandboxes.json"));
        }
    }
    if let Ok(out) = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .output()
    {
        if out.status.success() {
            let top = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if let Some(parent) = PathBuf::from(&top).parent() {
                return Ok(parent
                    .join("skein-shared")
                    .join(".claude")
                    .join("sandboxes.json"));
            }
        }
    }
    Err("can't locate sandboxes.json — set $SKEIN_REGISTRY or $SKEIN_SHARED".into())
}

pub fn load_registry() -> Result<(BTreeMap<String, Sandbox>, PathBuf), String> {
    let path = locate_registry()?;
    let data = fs::read_to_string(&path).map_err(|e| format!("reading {}: {e}", path.display()))?;
    let boxes: BTreeMap<String, Sandbox> =
        serde_json::from_str(&data).map_err(|e| format!("parsing {}: {e}", path.display()))?;
    Ok((boxes, path))
}

/// The fleet, enriched and sorted "who needs me first" (tier asc, then name).
pub fn load_views() -> Result<Vec<BoxView>, String> {
    let (boxes, _) = load_registry()?;
    // skein-server runs *inside* one box; that box is provably up, so don't let it derive
    // to idle/stale from a quiet `lastSeen`. Override only when the box reports no explicit
    // status (an agent status always wins). Set $SKEIN_SELF to override the detected vmid.
    let self_box = env::var("SKEIN_SELF")
        .or_else(|_| env::var("SANDBOX_VM_ID"))
        .ok()
        .filter(|s| !s.is_empty());
    let mut views: Vec<BoxView> = boxes
        .iter()
        .map(|(name, b)| {
            let (mut state, mut tier) = b.state();
            if b.status.is_empty() && self_box.as_deref() == Some(name.as_str()) && tier > 3 {
                state = "live".into();
                tier = 3;
            }
            // The narrative signal (box-session.sh): a cheap per-box file read, no model call.
            // Headline = the blocking prompt when waiting on you, else the gist of the last
            // message; pause classifies *why* it stopped so the inbox can rank and batch.
            let sig = session_signal(name);
            let blocked = state == "needs-input";
            let signal_text = sig.as_ref().map(|s| {
                if s.kind == "notification" && !s.prompt.trim().is_empty() {
                    s.prompt.clone()
                } else {
                    s.last_message.clone()
                }
            });
            let headline = signal_text.as_deref().and_then(first_line);
            let pause = if tier == 3 {
                Pause::None // still working — nothing owed
            } else {
                classify_message(signal_text.as_deref().unwrap_or(""), blocked)
            };
            BoxView {
                name: name.clone(),
                state,
                tier,
                branch: b.branch.clone(),
                age: b.age(),
                dir: shorten(&b.dir),
                // prefer the host-computed shortstat (matches the diff pane); fall back to the
                // box-reported number for clone-mode boxes this host can't see.
                diff: host_diffstat(name, &b.dir).or_else(|| b.diff.clone()),
                headline,
                pause,
            }
        })
        .collect();
    views.sort_by(|a, b| {
        a.tier
            .cmp(&b.tier)
            .then(a.pause.rank().cmp(&b.pause.rank()))
            .then(a.name.cmp(&b.name))
    });
    Ok(views)
}

/// The shared store directory (parent of `sandboxes.json`).
fn store_dir() -> Option<PathBuf> {
    locate_registry().ok()?.parent().map(|p| p.to_path_buf())
}

/// All cross-box messages, newest first. Reads `<store>/mailbox/*.json`.
pub fn load_mailbox() -> Vec<Message> {
    let dir = match store_dir() {
        Some(d) => d.join("mailbox"),
        None => return vec![],
    };
    let mut out = Vec::new();
    if let Ok(rd) = fs::read_dir(&dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.extension().and_then(|s| s.to_str()) != Some("json") {
                continue;
            }
            if let Ok(txt) = fs::read_to_string(&p) {
                if let Ok(m) = serde_json::from_str::<Message>(&txt) {
                    out.push(m);
                }
            }
        }
    }
    out.sort_by(|a, b| b.ts.cmp(&a.ts));
    out
}

/// Post a message into the shared mailbox (from `skein`), in the same shape mailbox.sh
/// writes so each box's `inbox` picks it up. `to` is a vmid or "broadcast".
pub fn send_message(to: &str, kind: &str, body: &str) -> Result<(), String> {
    let dir = store_dir()
        .ok_or("can't locate the shared store")?
        .join("mailbox");
    fs::create_dir_all(&dir).map_err(|e| format!("mailbox dir: {e}"))?;
    let now = Utc::now();
    let msg = Message {
        from: "skein".into(),
        to: to.into(),
        kind: if kind.is_empty() {
            "note".into()
        } else {
            kind.into()
        },
        branch: String::new(),
        body: body.into(),
        ts: now.format("%Y-%m-%dT%H:%M:%SZ").to_string(),
        seen_by: vec![],
    };
    let id = format!("{}-skein", now.timestamp_nanos_opt().unwrap_or(0));
    let json = serde_json::to_string(&msg).map_err(|e| e.to_string())?;
    fs::write(dir.join(format!("{id}.json")), json).map_err(|e| format!("write: {e}"))
}

/// Wrap a string for safe single-quoting in a POSIX shell. Used to quote every value substituted
/// into a `*_CMD` template before it reaches `sh -c`, so a branch/box name can't inject commands.
pub fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// The host shell command that launches a new box for `branch`. Override with
/// $SKEIN_LAUNCH_CMD (a template; `{branch}` is substituted); default assumes
/// `setup-sandbox.sh` is on PATH.
pub fn launch_command(branch: &str) -> String {
    if let Ok(t) = env::var("SKEIN_LAUNCH_CMD") {
        if !t.is_empty() {
            return t.replace("{branch}", &sh_quote(branch));
        }
    }
    format!("setup-sandbox.sh {}", sh_quote(branch))
}

/// The box's branch, from the registry.
pub fn branch_of(name: &str) -> Option<String> {
    let (boxes, _) = load_registry().ok()?;
    boxes
        .get(name)
        .map(|b| b.branch.clone())
        .filter(|b| !b.is_empty() && b != "?")
}

/// Run a program in the repo dir ($SKEIN_REPO, else cwd); returns (stdout, stderr, exit-code).
fn run_capture(prog: &str, args: &[&str]) -> Result<(String, String, i32), String> {
    let mut c = Command::new(prog);
    c.args(args);
    if let Ok(repo) = env::var("SKEIN_REPO") {
        if !repo.is_empty() {
            c.current_dir(repo);
        }
    }
    let out = c
        .output()
        .map_err(|e| format!("{prog} not runnable: {e}"))?;
    Ok((
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.code().unwrap_or(-1),
    ))
}

fn run_shell(cmd: &str) -> Result<(String, String, i32), String> {
    run_capture("sh", &["-c", cmd])
}

/// Merge-readiness for a box: does a PR exist, its state, and CI checks. All host-side via `gh`.
#[derive(Debug, Default, Serialize)]
pub struct ShipStatus {
    pub branch: Option<String>,
    pub pr_url: Option<String>,
    pub pr_state: Option<String>,
    /// "passing" | "pending" | "failing" | "none"
    pub checks: Option<String>,
    pub checks_text: Option<String>,
}

pub fn ship_status(name: &str) -> ShipStatus {
    let mut s = ShipStatus::default();
    let branch = match branch_of(name) {
        Some(b) => b,
        None => return s,
    };
    s.branch = Some(branch.clone());
    if let Ok((out, _, 0)) = run_capture("gh", &["pr", "view", &branch, "--json", "url,state"]) {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&out) {
            s.pr_url = v.get("url").and_then(|x| x.as_str()).map(String::from);
            s.pr_state = v.get("state").and_then(|x| x.as_str()).map(String::from);
        }
    }
    if s.pr_url.is_some() {
        if let Ok((out, err, code)) = run_capture("gh", &["pr", "checks", &branch]) {
            s.checks = Some(
                match code {
                    0 => "passing",
                    8 => "pending",
                    _ => "failing",
                }
                .into(),
            );
            let text = if out.trim().is_empty() { err } else { out };
            s.checks_text = Some(text.lines().take(10).collect::<Vec<_>>().join("\n"));
        }
    }
    s
}

/// Open (or report) a PR for the box's branch. Override the command with $SKEIN_PR_CMD
/// (`{branch}`/`{name}` substituted); default `gh pr create --head <branch> --fill`
/// (+ `--base $SKEIN_BASE` if set). Returns the PR URL on success.
pub fn create_pr(name: &str) -> Result<String, String> {
    let branch = branch_of(name).ok_or("box has no branch in the registry")?;
    let (out, err, code) = match env::var("SKEIN_PR_CMD") {
        Ok(c) if !c.is_empty() => run_shell(
            &c.replace("{branch}", &sh_quote(&branch))
                .replace("{name}", &sh_quote(name)),
        )?,
        _ => {
            let mut args = vec!["pr", "create", "--head", &branch, "--fill"];
            let base = env::var("SKEIN_BASE").unwrap_or_default();
            if !base.is_empty() {
                args.push("--base");
                args.push(&base);
            }
            run_capture("gh", &args)?
        }
    };
    if code == 0 {
        let url = out
            .split_whitespace()
            .chain(err.split_whitespace())
            .find(|w| w.contains("github.com") && w.contains("/pull/"))
            .map(String::from)
            .unwrap_or_else(|| out.trim().to_string());
        Ok(url)
    } else {
        let msg = if err.trim().is_empty() { out } else { err };
        Err(msg.trim().to_string())
    }
}

/// Merge the box's PR (the step that finishes the loop). Override with $SKEIN_MERGE_CMD
/// (`{branch}`/`{name}` substituted); default `gh pr merge <branch> <method>` where method is
/// $SKEIN_MERGE_METHOD (default `--squash`). Returns a short status line on success.
pub fn merge_pr(name: &str) -> Result<String, String> {
    let branch = branch_of(name).ok_or("box has no branch in the registry")?;
    let (out, err, code) = match env::var("SKEIN_MERGE_CMD") {
        Ok(c) if !c.is_empty() => run_shell(
            &c.replace("{branch}", &sh_quote(&branch))
                .replace("{name}", &sh_quote(name)),
        )?,
        _ => {
            let method = env::var("SKEIN_MERGE_METHOD")
                .ok()
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "--squash".into());
            run_capture("gh", &["pr", "merge", &branch, &method])?
        }
    };
    if code == 0 {
        let msg = out.trim();
        Ok(if msg.is_empty() {
            "merged".into()
        } else {
            msg.to_string()
        })
    } else {
        let msg = if err.trim().is_empty() { out } else { err };
        Err(msg.trim().to_string())
    }
}

/// Resume a paused box headlessly — the one-click "continue" primitive (step 6). Sends `prompt` as
/// the box's next turn via a headless `claude --continue --print`, fire-and-forget: the agent runs
/// inside its box and reports progress back through its own hooks (working → waiting/done), so the
/// inbox updates over SSE without skein waiting on the (possibly minutes-long) run. Override the
/// command with $SKEIN_RESUME_CMD (`{name}`/`{prompt}` substituted; both shell-quoted).
///
/// This is NOT silent auto-continue: it only ever runs from an explicit human click, and the cockpit
/// limits *batch* use to boxes the fork-detector tagged a trivial "proceed?" — a real fork or a
/// permission prompt is never auto-resumed. The agent keeps its own "ask when in doubt" instinct, so
/// if a resumed turn hits genuine uncertainty it pauses again and returns to the inbox.
pub fn resume_box(name: &str, prompt: &str) -> Result<(), String> {
    if !valid_name(name) {
        return Err("invalid box name".into());
    }
    let p = if prompt.trim().is_empty() {
        "Yes, please proceed."
    } else {
        prompt
    };
    let inner = match env::var("SKEIN_RESUME_CMD") {
        Ok(c) if !c.is_empty() => c
            .replace("{name}", &sh_quote(name))
            .replace("{prompt}", &sh_quote(p)),
        _ => format!(
            "sbx run --name {} -- --continue --print {}",
            sh_quote(name),
            sh_quote(p)
        ),
    };
    // fire-and-forget: nohup + `&` so the agent runs detached; the inner `sh` returns at once (and is
    // reaped here, no zombie) while the grandchild agent keeps running, reparented away from skein.
    let (_o, err, code) = run_shell(&format!("nohup {inner} >/dev/null 2>&1 &"))?;
    if code == 0 {
        Ok(())
    } else {
        Err(if err.trim().is_empty() {
            "resume failed to launch".into()
        } else {
            err.trim().to_string()
        })
    }
}

// ---------- step 7: rationed, lazy AI enrichment (subscription `claude -p`, no API key) ----------

/// skein runs *inside* an `sbx run` box where `claude` is logged in on the subscription, so every AI
/// call rides the SAME rate-limit window as the fleet doing the real work. AI is therefore OFF unless
/// you opt in with `$SKEIN_AI=on`, and even then it is lazy (on demand only), cached per turn-end, and
/// never a per-tick fleet sweep. The governing rule: AI may only *add* scrutiny, never remove it.
pub fn ai_enabled() -> bool {
    matches!(
        env::var("SKEIN_AI").ok().as_deref(),
        Some("on" | "1" | "true" | "yes")
    )
}

/// One-shot headless Haiku over the subscription: `claude -p --model <haiku>`. Returns trimmed
/// stdout, or None when AI is disabled / `claude` is absent / the call fails or times out — every
/// caller treats None as "fall back to the free deterministic path". `$SKEIN_CLAUDE_BIN` and
/// `$SKEIN_AI_MODEL` override the binary and model (and let tests stub the call).
fn claude_oneshot(prompt: &str) -> Option<String> {
    if !ai_enabled() {
        return None;
    }
    let bin = env::var("SKEIN_CLAUDE_BIN")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "claude".into());
    let model = env::var("SKEIN_AI_MODEL")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "claude-haiku-4-5".into());
    // `timeout` guards a hung subprocess; if it isn't on PATH, fall back to an unguarded call.
    let run = |guarded: bool| {
        if guarded {
            Command::new("timeout")
                .args(["30", &bin, "-p", "--model", &model, prompt])
                .output()
        } else {
            Command::new(&bin)
                .args(["-p", "--model", &model, prompt])
                .output()
        }
    };
    let out = run(true).or_else(|_| run(false)).ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!s.is_empty()).then_some(s)
}

/// Memoize an AI result by a turn-end-scoped key, so repeat views of the same paused box don't
/// re-spend tokens; a new signal (new key) recomputes. Negatives are cached too — a flaky/None
/// answer shouldn't be retried on every poll within the same turn.
fn ai_cached(key: &str, compute: impl FnOnce() -> Option<String>) -> Option<String> {
    use std::sync::OnceLock;
    type Cache = std::collections::HashMap<String, Option<String>>;
    static CACHE: OnceLock<std::sync::Mutex<Cache>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| std::sync::Mutex::new(Cache::new()));
    if let Ok(m) = cache.lock() {
        if let Some(v) = m.get(key) {
            return v.clone();
        }
    }
    let v = compute();
    if let Ok(mut m) = cache.lock() {
        m.insert(key.to_string(), v.clone());
    }
    v
}

/// The text a box last reported (the blocking prompt when waiting on you, else its last message).
fn signal_text(sig: &SessionSignal) -> &str {
    if sig.kind == "notification" {
        &sig.prompt
    } else {
        &sig.last_message
    }
}

/// A one-line AI narration of what a box last did or is asking — the lazy fallback for the digest
/// when the box keeps no journal. One rationed Haiku call, cached per turn-end; None when AI is off
/// or unavailable (the digest then just shows commits + the raw last message). On demand only.
pub fn narrate(name: &str) -> Option<String> {
    if !valid_name(name) || !ai_enabled() {
        return None;
    }
    let sig = session_signal(name)?;
    let text = signal_text(&sig);
    if text.trim().is_empty() {
        return None;
    }
    let key = format!("narrate:{name}:{}", sig.ts);
    let prompt = format!(
        "Summarise in ONE plain sentence (max 20 words) what this autonomous coding agent just did \
         or is asking. No preamble, no quotes — just the sentence.\n\nAgent message:\n{}",
        text.chars().take(2000).collect::<String>()
    );
    ai_cached(&key, || claude_oneshot(&prompt))
}

/// Conservative AI safety check for batch-resume: given a box the heuristic tagged a trivial
/// "proceed?", ask Haiku whether it is actually a real decision the human should make. Returns
/// `Some(true)` = HOLD it back, unless Haiku affirmatively says ROUTINE — so a flaky, garbled, or
/// absent answer errs toward asking you, never toward auto-continuing. `None` means AI is off (the
/// caller then trusts the heuristic verdict). AI can only *add* a hold here, never grant a continue
/// the heuristic wouldn't already allow.
fn ai_says_hold(name: &str) -> Option<bool> {
    if !ai_enabled() {
        return None;
    }
    let sig = session_signal(name)?;
    let text = signal_text(&sig);
    if text.trim().is_empty() {
        return None;
    }
    let key = format!("gate:{name}:{}", sig.ts);
    let prompt = format!(
        "An autonomous coding agent ended its turn with the message below. Reply with ONE word only: \
         ROUTINE if it is merely asking permission to continue with obvious, safe next steps; or \
         DECISION if it is asking the human to make a real choice or judgement the agent should not \
         make alone.\n\nMessage:\n{}",
        text.chars().take(2000).collect::<String>()
    );
    let ans = ai_cached(&key, || claude_oneshot(&prompt))?;
    // err toward HOLD: only an explicit ROUTINE clears a box for auto-continue
    Some(!ans.to_uppercase().contains("ROUTINE"))
}

/// Resume several paused boxes in one gesture (step 6). When AI is enabled (step 7), each box is run
/// past [`ai_says_hold`] first; any the model flags as a genuine decision are HELD (not auto-resumed)
/// and returned so the cockpit can route you to them. Returns (resumed, held). With AI off this
/// resumes every (valid) box — the heuristic already restricted the set to `proceed`.
pub fn resume_batch(names: &[String]) -> (Vec<String>, Vec<String>) {
    let mut resumed = Vec::new();
    let mut held = Vec::new();
    for name in names {
        if !valid_name(name) {
            continue;
        }
        if ai_says_hold(name) == Some(true) {
            held.push(name.clone());
            continue;
        }
        if resume_box(name, "").is_ok() {
            resumed.push(name.clone());
        }
    }
    (resumed, held)
}

/// Archive a box off the board: remove its registry entry, append it to `<store>/history.jsonl`,
/// and run $SKEIN_ARCHIVE_CMD if set (e.g. `sbx rm {name}`). skein-owned; needs no external tool.
pub fn archive_box(name: &str) -> Result<(), String> {
    if !valid_name(name) {
        return Err("invalid box name".into());
    }
    use fs2::FileExt;
    use std::io::Write as _;
    let path = locate_registry()?;
    let store = path
        .parent()
        .ok_or("registry has no parent dir")?
        .to_path_buf();

    // Share the box hooks' flock discipline (sandbox-bootstrap.sh): an exclusive advisory lock on
    // <store>/.sandboxes.lock held across the whole read-modify-write, then an atomic temp+rename
    // so a concurrent heartbeat can't be lost mid-write and a reader can't see a truncated file.
    let lock = fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(store.join(".sandboxes.lock"))
        .map_err(|e| format!("lock open: {e}"))?;
    lock.lock_exclusive().map_err(|e| format!("lock: {e}"))?;

    let result = (|| {
        let data = fs::read_to_string(&path).map_err(|e| format!("reading registry: {e}"))?;
        let mut v: serde_json::Value =
            serde_json::from_str(&data).map_err(|e| format!("parsing registry: {e}"))?;
        let obj = v.as_object_mut().ok_or("registry is not a JSON object")?;
        let mut removed = obj.remove(name).ok_or("no such box in the registry")?;

        if let Some(m) = removed.as_object_mut() {
            m.insert("name".into(), serde_json::Value::String(name.into()));
            m.insert(
                "archivedAt".into(),
                serde_json::Value::String(Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string()),
            );
        }
        // history is append-only — don't rewrite the whole log (racy + O(n)).
        let hist = store.join("history.jsonl");
        if let Ok(mut f) = fs::OpenOptions::new().create(true).append(true).open(&hist) {
            let _ = writeln!(f, "{removed}");
        }
        let pretty = serde_json::to_string_pretty(&v).map_err(|e| e.to_string())?;
        write_atomic(&path, &store, pretty.as_bytes())
    })();
    let _ = lock.unlock();
    result?;

    if let Ok(c) = env::var("SKEIN_ARCHIVE_CMD") {
        if !c.is_empty() {
            let _ = run_shell(&c.replace("{name}", &sh_quote(name)));
        }
    }
    Ok(())
}

/// Read the full branch-vs-base patch a box wrote to `<store>/diffs/<name>.patch`.
/// (Boxes report their own diff because `sbx run` can't exec an arbitrary command in them.)
pub fn read_diff(name: &str) -> Option<String> {
    if !valid_name(name) {
        return None;
    }
    // Prefer a fresh diff computed host-side when the box's dir is a git repo *on this
    // host* (direct mode); fall back to the patch the box reported (clone mode, where the
    // dir is an in-box path this host can't see). Computed on demand only — never per tick.
    if let Some(dir) = lookup_dir(name) {
        if let Some(p) = git_diff_for(&dir) {
            return Some(p);
        }
    }
    let reg = locate_registry().ok()?;
    let path = reg.parent()?.join("diffs").join(format!("{name}.patch"));
    fs::read_to_string(path).ok()
}

fn git_ok(dir: &str, args: &[&str]) -> bool {
    let mut a = vec!["-C", dir];
    a.extend_from_slice(args);
    Command::new("git")
        .args(&a)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// The git ref the branch-vs-base diff is measured against, or None if `dir` isn't a git repo
/// here. Base = the first of origin/main|origin/master|main|master that resolves; we diff against
/// the merge-base→working-tree. With no common ancestor it falls back to `HEAD` (uncommitted only)
/// rather than exploding into an unrelated-history diff. Shared by the full diff and the shortstat.
fn git_range(dir: &str) -> Option<String> {
    if !Path::new(dir).join(".git").exists() {
        return None;
    }
    let base = ["origin/main", "origin/master", "main", "master"]
        .into_iter()
        .find(|b| git_ok(dir, &["rev-parse", "--verify", "-q", b]));
    let merge_base = base.and_then(|b| {
        let o = Command::new("git")
            .args(["-C", dir, "merge-base", "HEAD", b])
            .output()
            .ok()?;
        let s = String::from_utf8_lossy(&o.stdout).trim().to_string();
        (o.status.success() && !s.is_empty()).then_some(s)
    });
    Some(merge_base.unwrap_or_else(|| "HEAD".into()))
}

/// The full branch-vs-base patch for a working tree at `dir`. Output is capped so a huge patch
/// can't wedge the browser. None if `dir` isn't a git repo here (clone mode → reported patch).
fn git_diff_for(dir: &str) -> Option<String> {
    let range = git_range(dir)?;
    let out = Command::new("git")
        .args(["-C", dir, "diff", &range])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let mut patch = String::from_utf8_lossy(&out.stdout).to_string();
    const CAP: usize = 2_000_000;
    if patch.len() > CAP {
        patch.truncate(CAP);
        patch.push_str("\n\n# … diff truncated by skein (too large to render) …\n");
    }
    Some(patch)
}

/// The host-side branch-vs-base shortstat for `dir` (same range as [`git_diff_for`]), so the
/// fleet's diff± badge matches the diff *pane* for direct-mode boxes instead of drifting from the
/// box-reported number. None if not a host git repo or there are no changes.
fn git_diffstat_for(dir: &str) -> Option<DiffStat> {
    let range = git_range(dir)?;
    let out = Command::new("git")
        .args(["-C", dir, "diff", "--shortstat", &range])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    // e.g. " 3 files changed, 12 insertions(+), 4 deletions(-)"
    let s = String::from_utf8_lossy(&out.stdout);
    let num = |kw: &str| {
        s.split(',')
            .find(|p| p.contains(kw))
            .and_then(|p| p.split_whitespace().next())
            .and_then(|n| n.parse::<u32>().ok())
            .unwrap_or(0)
    };
    let stat = DiffStat {
        files: num("file"),
        ins: num("insertion"),
        del: num("deletion"),
    };
    (stat.files != 0 || stat.ins != 0 || stat.del != 0).then_some(stat)
}

/// Host-computed diff± for the badge, cached briefly so the per-tick fleet stream doesn't fork a
/// `git` per box every poll. Clone-mode boxes (no host `.git`) cost only a `stat`, not a fork.
fn host_diffstat(name: &str, dir: &str) -> Option<DiffStat> {
    use std::sync::OnceLock;
    use std::time::{Duration, Instant};
    type Cache = std::collections::HashMap<String, (Instant, Option<DiffStat>)>;
    static CACHE: OnceLock<std::sync::Mutex<Cache>> = OnceLock::new();
    const TTL: Duration = Duration::from_secs(8);
    let cache = CACHE.get_or_init(|| std::sync::Mutex::new(Cache::new()));
    if let Ok(map) = cache.lock() {
        if let Some((t, v)) = map.get(name) {
            if t.elapsed() < TTL {
                return v.clone();
            }
        }
    }
    let v = git_diffstat_for(dir);
    if let Ok(mut map) = cache.lock() {
        map.insert(name.to_string(), (Instant::now(), v.clone()));
    }
    v
}

/// Look up a box's clone root (the `dir` it registered) by name.
pub fn lookup_dir(name: &str) -> Option<String> {
    let (boxes, _) = load_registry().ok()?;
    boxes
        .get(name)
        .map(|b| b.dir.clone())
        .filter(|d| !d.is_empty())
}

// ---------- step 9: collision radar — warn before two boxes' work overwrites the same file ----------

/// The files a box changed on its branch (host-side `git diff --name-only`, or parsed from the
/// box-reported patch in clone mode where this host can't see the box's `.git`).
pub fn changed_files(name: &str) -> Vec<String> {
    if !valid_name(name) {
        return vec![];
    }
    if let Some(dir) = lookup_dir(name) {
        if let Some(range) = git_range(&dir) {
            if let Ok(out) = Command::new("git")
                .args(["-C", &dir, "diff", "--name-only", &range])
                .output()
            {
                if out.status.success() {
                    let v: Vec<String> = String::from_utf8_lossy(&out.stdout)
                        .lines()
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .collect();
                    if !v.is_empty() {
                        return v;
                    }
                }
            }
        }
    }
    // clone-mode fallback: pull the file list out of the patch the box reported.
    let mut files = Vec::new();
    if let Some(reg) = locate_registry()
        .ok()
        .and_then(|r| r.parent().map(|p| p.to_path_buf()))
    {
        if let Ok(patch) = fs::read_to_string(reg.join("diffs").join(format!("{name}.patch"))) {
            for line in patch.lines() {
                if let Some(rest) = line.strip_prefix("+++ b/") {
                    let f = rest.trim();
                    if !f.is_empty() && f != "/dev/null" {
                        files.push(f.to_string());
                    }
                }
            }
        }
    }
    files.sort();
    files.dedup();
    files
}

/// One file that more than one box has touched — a merge collision waiting to happen.
#[derive(Debug, Clone, Serialize)]
pub struct Collision {
    pub file: String,
    pub boxes: Vec<String>,
}

/// Files changed by two or more boxes at once, so you can reconcile before they fight at merge.
/// Cached briefly (forks a `git` per box) so the cockpit can poll it without hammering the host.
pub fn collisions() -> Vec<Collision> {
    use std::sync::OnceLock;
    use std::time::{Duration, Instant};
    static CACHE: OnceLock<std::sync::Mutex<(Instant, Vec<Collision>)>> = OnceLock::new();
    const TTL: Duration = Duration::from_secs(8);
    if let Some(lock) = CACHE.get() {
        if let Ok(g) = lock.lock() {
            if g.0.elapsed() < TTL {
                return g.1.clone();
            }
        }
    }
    let computed = compute_collisions();
    let lock = CACHE.get_or_init(|| std::sync::Mutex::new((Instant::now(), Vec::new())));
    if let Ok(mut g) = lock.lock() {
        *g = (Instant::now(), computed.clone());
    }
    computed
}

fn compute_collisions() -> Vec<Collision> {
    let boxes = match load_registry() {
        Ok((b, _)) => b,
        Err(_) => return vec![],
    };
    let mut by_file: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for name in boxes.keys() {
        for f in changed_files(name) {
            by_file.entry(f).or_default().push(name.clone());
        }
    }
    let mut out: Vec<Collision> = by_file
        .into_iter()
        .filter_map(|(file, mut bs)| {
            bs.sort();
            bs.dedup();
            (bs.len() > 1).then_some(Collision { file, boxes: bs })
        })
        .collect();
    // most-contended files first, then alphabetical
    out.sort_by(|a, b| b.boxes.len().cmp(&a.boxes.len()).then(a.file.cmp(&b.file)));
    out
}

/// The full `sbx` argv that reconnects to box `name` (the leading program is `sbx`;
/// this returns only its arguments). `_dir` is currently unused for the default agent
/// invocation (claude resolves the conversation by the box's cwd) but is kept in the
/// signature so callers needn't special-case it and the `{dir}` override stays uniform.
///
/// `sbx run --name <box>` runs the box's agent (claude); a bare run starts a *fresh*
/// conversation. To *continue* the session we pass claude's own `--continue` flag. The
/// `--` is load-bearing twice over: `sbx` is a cobra CLI that would otherwise parse
/// `--continue` as its own flag, and it marks where sbx stops and the agent's args begin.
pub fn attach_argv(name: &str, _dir: &str) -> Vec<String> {
    vec![
        "run".into(),
        "--name".into(),
        name.into(),
        "--".into(),
        "--continue".into(),
    ]
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
    let path = store_dir()?.join("sessions").join(format!("{name}.json"));
    let txt = fs::read_to_string(path).ok()?;
    serde_json::from_str(&txt).ok()
}

/// Recent commit subjects on the box's branch (newest first) — the agent's own changelog,
/// a free, accurate "what was done" with no model call. Bounded; empty if not a host repo.
pub fn recent_commits(name: &str) -> Vec<String> {
    let dir = match lookup_dir(name) {
        Some(d) => d,
        None => return vec![],
    };
    let range = match git_range(&dir) {
        Some(r) => format!("{r}..HEAD"),
        None => return vec![],
    };
    let out = Command::new("git")
        .args(["-C", &dir, "log", "--format=%s", "-n", "20", &range])
        .output();
    match out {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout)
            .lines()
            .map(|s| s.to_string())
            .filter(|s| !s.is_empty())
            .collect(),
        _ => vec![],
    }
}

/// The agent's own turn-end journal (`<dir>/.skein/journal.md`), if it keeps one — the best
/// "what was done" source because it's written with full context (see the CLAUDE.md ritual).
/// Returns the tail (last ~40 lines), capped, or None when the box keeps no journal.
pub fn read_journal(name: &str) -> Option<String> {
    let dir = lookup_dir(name)?;
    let txt = fs::read_to_string(Path::new(&dir).join(".skein").join("journal.md")).ok()?;
    let tail: Vec<&str> = txt.lines().rev().take(40).collect();
    let mut s: String = tail.into_iter().rev().collect::<Vec<_>>().join("\n");
    const CAP: usize = 4000;
    if s.len() > CAP {
        let cut = s.len() - CAP;
        s = format!("…\n{}", &s[cut..]);
    }
    Some(s).filter(|s| !s.trim().is_empty())
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
    pub fn as_str(self) -> &'static str {
        match self {
            Pause::NeedsInput => "needs-input",
            Pause::Proceed => "proceed",
            Pause::Fork => "fork",
            Pause::Statement => "statement",
            Pause::None => "none",
        }
    }

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

/// First non-empty line of `s`, whitespace-collapsed and capped — the inbox headline. None when
/// `s` is blank.
fn first_line(s: &str) -> Option<String> {
    let line = s.lines().map(str::trim).find(|l| !l.is_empty())?;
    let collapsed = line.split_whitespace().collect::<Vec<_>>().join(" ");
    const CAP: usize = 120;
    let out = if collapsed.chars().count() > CAP {
        let mut t: String = collapsed.chars().take(CAP).collect();
        t.push('…');
        t
    } else {
        collapsed
    };
    Some(out).filter(|s| !s.is_empty())
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

/// A glanceable "what happened here" for one box — the inbox detail view (steps 1–5), assembled
/// entirely from free sources: the registry state, the host-side diff/commits, the agent's own
/// journal, and its last reported message. No model tokens spent.
#[derive(Debug, Default, Serialize)]
pub struct SessionDigest {
    pub name: String,
    pub branch: String,
    pub state: String,
    pub tier: u8,
    pub age: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diff: Option<DiffStat>,
    /// the agent's own changelog (recent commit subjects, newest first)
    pub commits: Vec<String>,
    /// the agent's turn-end journal, if it keeps `.skein/journal.md`
    #[serde(skip_serializing_if = "Option::is_none")]
    pub journal: Option<String>,
    /// the last assistant message (Stop) — the turn's own sign-off
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_message: Option<String>,
    /// the prompt the agent is blocked on (Notification), if any
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blocked_on: Option<String>,
    /// when the box last reported a narrative signal (RFC3339)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signal_ts: Option<String>,
    /// why the turn ended — drives the inbox ranking and CTA
    pub pause: Pause,
}

/// Assemble the free session digest for a box. Pure reads + a couple of cached `git` calls;
/// never a model call. Returns None for an unknown box name.
pub fn session_digest(name: &str) -> Option<SessionDigest> {
    if !valid_name(name) {
        return None;
    }
    let (boxes, _) = load_registry().ok()?;
    let sb = boxes.get(name)?;
    let (state, tier) = sb.state();
    let blocked = state == "needs-input";

    let sig = session_signal(name);
    let last_message = sig
        .as_ref()
        .map(|s| s.last_message.clone())
        .filter(|m| !m.trim().is_empty());
    let blocked_on = sig
        .as_ref()
        .filter(|s| s.kind == "notification")
        .map(|s| s.prompt.clone())
        .filter(|p| !p.trim().is_empty());
    // classify over whichever text we have (the prompt when blocked, else the last message)
    let class_text = blocked_on
        .clone()
        .or_else(|| last_message.clone())
        .unwrap_or_default();
    let pause = match tier {
        3 => Pause::None, // still working — nothing owed
        _ => classify_message(&class_text, blocked),
    };

    Some(SessionDigest {
        name: name.to_string(),
        branch: sb.branch.clone(),
        state,
        tier,
        age: sb.age(),
        diff: host_diffstat(name, &sb.dir).or_else(|| sb.diff.clone()),
        commits: recent_commits(name),
        journal: read_journal(name),
        last_message,
        blocked_on,
        signal_ts: sig.map(|s| s.ts).filter(|t| !t.is_empty()),
        pause,
    })
}

pub fn shorten(p: &str) -> String {
    if let Ok(home) = env::var("HOME") {
        if !home.is_empty() && p.starts_with(&home) {
            return format!("~{}", &p[home.len()..]);
        }
    }
    p.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Mutex;

    // Env vars are process-global; serialize the tests that read/write them.
    static ENV_LOCK: Mutex<()> = Mutex::new(());
    static SEQ: AtomicU64 = AtomicU64::new(0);

    fn tempdir() -> PathBuf {
        let d = env::temp_dir().join(format!(
            "skein-test-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&d).unwrap();
        d
    }
    fn secs_ago(s: i64) -> String {
        (Utc::now() - chrono::Duration::seconds(s)).to_rfc3339()
    }
    fn sb(status: &str, last_seen: &str) -> Sandbox {
        Sandbox {
            branch: "b".into(),
            dir: "/d".into(),
            last_seen: last_seen.into(),
            status: status.into(),
            diff: None,
        }
    }

    #[test]
    fn valid_name_guards_paths() {
        assert!(valid_name("thing-feature"));
        assert!(valid_name("box_123"));
        for bad in ["", "../etc", "a/b", "a\\b", "..", "x..y", "a\0b"] {
            assert!(!valid_name(bad), "should reject {bad:?}");
        }
        assert!(!valid_name(&"x".repeat(200)));
    }

    #[test]
    fn state_prefers_explicit_status() {
        assert_eq!(sb("needs-input", "").state(), ("needs-input".into(), 0));
        assert_eq!(sb("waiting", "").state().1, 1);
        assert_eq!(sb("done", "").state().1, 2);
        assert_eq!(sb("working", "").state().1, 3);
        assert_eq!(sb("compiling", "").state(), ("compiling".into(), 3)); // passthrough
    }

    #[test]
    fn state_derives_liveness_from_last_seen() {
        assert_eq!(sb("", &secs_ago(10)).state().0, "live");
        assert_eq!(sb("", &secs_ago(600)).state().0, "idle");
        assert_eq!(sb("", &secs_ago(7200)).state().0, "stale");
        assert_eq!(sb("", "not-a-date").state().0, "unknown");
    }

    #[test]
    fn age_buckets() {
        assert!(sb("", &secs_ago(5)).age().ends_with("s ago"));
        assert!(sb("", &secs_ago(120)).age().ends_with("m ago"));
        assert!(sb("", &secs_ago(7200)).age().ends_with("h ago"));
        assert_eq!(sb("", "nope").age(), "?");
    }

    #[test]
    fn sh_quote_escapes() {
        assert_eq!(sh_quote("a b"), "'a b'");
        assert_eq!(sh_quote("x'; rm -rf ~"), "'x'\\''; rm -rf ~'");
    }

    #[test]
    fn shorten_replaces_home() {
        let _g = ENV_LOCK.lock().unwrap();
        env::set_var("HOME", "/home/me");
        assert_eq!(shorten("/home/me/work/x"), "~/work/x");
        assert_eq!(shorten("/other/x"), "/other/x");
    }

    #[test]
    fn load_views_promotes_only_the_self_box_when_quiet() {
        let _g = ENV_LOCK.lock().unwrap();
        let dir = tempdir();
        let reg = dir.join("sandboxes.json");
        fs::write(
            &reg,
            format!(
                r#"{{"thing-self":{{"branch":"s","dir":"/d","lastSeen":"{}","status":""}},
                    "thing-other":{{"branch":"o","dir":"/d","lastSeen":"{}","status":""}}}}"#,
                secs_ago(7200),
                secs_ago(7200)
            ),
        )
        .unwrap();
        env::set_var("SKEIN_REGISTRY", &reg);
        env::remove_var("SKEIN_SHARED");
        env::set_var("SKEIN_SELF", "thing-self");

        let v = load_views().unwrap();
        let self_v = v.iter().find(|b| b.name == "thing-self").unwrap();
        let other_v = v.iter().find(|b| b.name == "thing-other").unwrap();
        assert_eq!(self_v.state, "live"); // promoted despite a 2h-old lastSeen
        assert_eq!(other_v.state, "stale"); // a peer is never promoted

        env::remove_var("SKEIN_SELF");
        env::remove_var("SKEIN_REGISTRY");
    }

    #[test]
    fn archive_box_removes_records_and_guards() {
        let _g = ENV_LOCK.lock().unwrap();
        let dir = tempdir();
        let reg = dir.join("sandboxes.json");
        fs::write(
            &reg,
            r#"{"thing-x":{"branch":"x","dir":"/d","lastSeen":"2026-01-01T00:00:00Z","status":"done"},
               "thing-y":{"branch":"y","dir":"/d","lastSeen":"2026-01-01T00:00:00Z","status":""}}"#,
        )
        .unwrap();
        env::set_var("SKEIN_REGISTRY", &reg);
        env::remove_var("SKEIN_SHARED");
        env::remove_var("SKEIN_ARCHIVE_CMD");

        archive_box("thing-x").unwrap();
        let after: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&reg).unwrap()).unwrap();
        assert!(after.get("thing-x").is_none());
        assert!(after.get("thing-y").is_some()); // didn't clobber the rest
        let hist = fs::read_to_string(dir.join("history.jsonl")).unwrap();
        assert!(hist.contains("thing-x") && hist.contains("archivedAt"));
        assert!(dir.join(".sandboxes.lock").exists()); // shares the hooks' flock file
        assert!(archive_box("thing-x").is_err()); // already gone
        assert!(archive_box("../escape").is_err()); // name guard

        env::remove_var("SKEIN_REGISTRY");
    }

    #[test]
    fn git_diff_for_handles_repo_and_nonrepo() {
        if Command::new("git").arg("--version").output().is_err() {
            return; // git not available in this environment
        }
        let dir = tempdir();
        let d = dir.to_str().unwrap();
        let git = |args: &[&str]| {
            let mut a = vec!["-C", d];
            a.extend_from_slice(args);
            assert!(Command::new("git")
                .args(&a)
                .output()
                .unwrap()
                .status
                .success());
        };
        git(&["init", "-q"]);
        git(&["config", "user.email", "t@t"]);
        git(&["config", "user.name", "t"]);
        fs::write(dir.join("a.txt"), "hello\n").unwrap();
        git(&["add", "."]);
        git(&["commit", "-q", "-m", "x"]);
        fs::write(dir.join("a.txt"), "hello world\n").unwrap();
        let patch = git_diff_for(d).expect("a repo with changes yields a patch");
        assert!(patch.contains("hello world"));
        let stat = git_diffstat_for(d).expect("a repo with changes yields a shortstat");
        assert!(stat.files >= 1 && stat.ins + stat.del >= 1);

        let empty = tempdir(); // not a git repo → None, never explodes
        assert!(git_diff_for(empty.to_str().unwrap()).is_none());
        assert!(git_diffstat_for(empty.to_str().unwrap()).is_none());
    }

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
        let _g = ENV_LOCK.lock().unwrap();
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
    fn resume_box_guards_name_and_launches() {
        let _g = ENV_LOCK.lock().unwrap();
        assert!(resume_box("../escape", "go").is_err()); // name guard
                                                         // a stub command stands in for `sbx run …` so the test never spawns a real agent; the
                                                         // fire-and-forget wrapper returns Ok once it has *launched*, regardless of agent outcome.
        env::set_var("SKEIN_RESUME_CMD", "true {name} {prompt}");
        env::remove_var("SKEIN_REPO");
        assert!(resume_box("thing-x", "").is_ok());
        env::remove_var("SKEIN_RESUME_CMD");
    }

    // A stub standing in for the `claude` CLI: it reads the prompt (its last arg) and echoes a
    // canned reply, so the AI paths are exercised without a real model call.
    #[cfg(unix)]
    fn write_claude_stub(dir: &Path) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let p = dir.join("claude-stub.sh");
        fs::write(
            &p,
            "#!/bin/sh\nfor last; do :; done\ncase \"$last\" in\n  *'wire it up'*) echo ROUTINE ;;\n  *'which database'*) echo DECISION ;;\n  *Summarise*) echo 'It wired up the parser.' ;;\n  *) echo '?' ;;\nesac\n",
        )
        .unwrap();
        fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).unwrap();
        p
    }

    #[cfg(unix)]
    fn write_session(dir: &Path, name: &str, last_message: &str) {
        fs::create_dir_all(dir.join("sessions")).unwrap();
        let body = serde_json::json!({"ts":"2026-06-29T00:00:00Z","kind":"stop","lastMessage":last_message});
        fs::write(
            dir.join("sessions").join(format!("{name}.json")),
            body.to_string(),
        )
        .unwrap();
    }

    #[test]
    #[cfg(unix)]
    fn narrate_uses_stubbed_claude_and_respects_kill_switch() {
        if Command::new("sh").arg("-c").arg("true").output().is_err() {
            return;
        }
        let _g = ENV_LOCK.lock().unwrap();
        let dir = tempdir();
        let reg = dir.join("sandboxes.json");
        fs::write(
            &reg,
            r#"{"thing-n":{"branch":"x","dir":"/d","lastSeen":"","status":"waiting"}}"#,
        )
        .unwrap();
        write_session(&dir, "thing-n", "Refactored the parser module today.");
        env::set_var("SKEIN_REGISTRY", &reg);
        env::remove_var("SKEIN_SHARED");
        env::set_var("SKEIN_CLAUDE_BIN", write_claude_stub(&dir));

        env::remove_var("SKEIN_AI"); // kill switch: off → no spend, None
        assert_eq!(narrate("thing-n"), None);

        env::set_var("SKEIN_AI", "on");
        assert_eq!(
            narrate("thing-n").as_deref(),
            Some("It wired up the parser.")
        );

        env::remove_var("SKEIN_AI");
        env::remove_var("SKEIN_CLAUDE_BIN");
        env::remove_var("SKEIN_REGISTRY");
    }

    #[test]
    #[cfg(unix)]
    fn resume_batch_holds_real_decisions_when_ai_on() {
        if Command::new("sh").arg("-c").arg("true").output().is_err() {
            return;
        }
        let _g = ENV_LOCK.lock().unwrap();
        let dir = tempdir();
        let reg = dir.join("sandboxes.json");
        fs::write(
            &reg,
            r#"{"box-route":{"branch":"a","dir":"/d","lastSeen":"","status":"waiting"},
               "box-decide":{"branch":"b","dir":"/d","lastSeen":"","status":"waiting"}}"#,
        )
        .unwrap();
        write_session(&dir, "box-route", "Parser done — shall I wire it up next?");
        write_session(&dir, "box-decide", "Stuck: which database should I target?");
        env::set_var("SKEIN_REGISTRY", &reg);
        env::remove_var("SKEIN_SHARED");
        env::set_var("SKEIN_CLAUDE_BIN", write_claude_stub(&dir));
        env::set_var("SKEIN_RESUME_CMD", "true {name} {prompt}"); // don't spawn a real agent
        env::remove_var("SKEIN_REPO");

        env::set_var("SKEIN_AI", "on");
        let (resumed, held) = resume_batch(&["box-route".to_string(), "box-decide".to_string()]);
        assert_eq!(resumed, vec!["box-route".to_string()]); // ROUTINE → continued
        assert_eq!(held, vec!["box-decide".to_string()]); // DECISION → held for the human

        env::remove_var("SKEIN_AI");
        env::remove_var("SKEIN_CLAUDE_BIN");
        env::remove_var("SKEIN_RESUME_CMD");
        env::remove_var("SKEIN_REGISTRY");
    }

    #[test]
    fn collisions_flag_files_touched_by_multiple_boxes() {
        let _g = ENV_LOCK.lock().unwrap();
        let dir = tempdir();
        let reg = dir.join("sandboxes.json");
        // dirs aren't git repos here → changed_files falls back to parsing the reported patches
        fs::write(
            &reg,
            r#"{"box-a":{"branch":"a","dir":"/nope-a","lastSeen":"","status":""},
               "box-b":{"branch":"b","dir":"/nope-b","lastSeen":"","status":""}}"#,
        )
        .unwrap();
        fs::create_dir_all(dir.join("diffs")).unwrap();
        fs::write(
            dir.join("diffs").join("box-a.patch"),
            "+++ b/src/shared.rs\n+++ b/src/only_a.rs\n",
        )
        .unwrap();
        fs::write(
            dir.join("diffs").join("box-b.patch"),
            "+++ b/src/shared.rs\n+++ b/src/only_b.rs\n",
        )
        .unwrap();
        env::set_var("SKEIN_REGISTRY", &reg);
        env::remove_var("SKEIN_SHARED");

        assert_eq!(
            changed_files("box-a"),
            vec!["src/only_a.rs", "src/shared.rs"]
        );
        let cols = compute_collisions();
        assert_eq!(cols.len(), 1, "only the shared file collides");
        assert_eq!(cols[0].file, "src/shared.rs");
        assert_eq!(
            cols[0].boxes,
            vec!["box-a".to_string(), "box-b".to_string()]
        );

        env::remove_var("SKEIN_REGISTRY");
    }

    #[test]
    fn index_html_is_well_formed() {
        // The whole UI is one include_str!'d file; a missing close tag silently blanks the page.
        let html = include_str!("web/index.html");
        assert_eq!(
            html.matches("<script").count(),
            html.matches("</script>").count(),
            "unbalanced <script> tags"
        );
        assert!(html.trim_end().ends_with("</html>"));
        assert!(html.contains("id=\"fleet\""));
        assert!(html.contains("/vendor/xterm.js")); // vendored, not CDN
        assert!(!html.contains("cdn.jsdelivr"));
    }
}
