//! Small, dependency-free helpers shared across skein: shell quoting, atomic writes, bounded
//! subprocess capture, and string trimming.
//!
//! Nothing here knows what a box is. If a helper needs to know, it belongs in the module that
//! owns that concept — this one stays safe to call from anywhere.

use std::env;
use std::fs;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

/// Load a local `.env` (searched from the cwd upward) so the registry/repo paths and `*_CMD`
/// templates needn't be passed on every invocation. Variables already set in the real
/// environment win — dotenv never overrides — so a command-line `VAR=… skein …` still takes
/// precedence. A missing file is fine and silent; a *malformed* file is reported on stderr
/// rather than silently dropping every line after the bad one (which once made a quoting slip
/// look like a "command not found"). The binaries call this once at startup.
pub fn load_dotenv() {
    match dotenvy::dotenv() {
        Ok(_) => {}
        Err(e) if e.not_found() => {}
        Err(e) => eprintln!("skein: ignoring malformed .env — {e}"),
    }
}

/// Write `bytes` to `path` atomically: a temp file in the same dir, then rename (POSIX-atomic),
/// so a concurrent reader sees either the old or the new whole file, never a truncated one.
/// `dir` must be `path`'s parent (same filesystem) for the rename to be atomic.
pub(crate) fn write_atomic(path: &Path, dir: &Path, bytes: &[u8]) -> Result<(), String> {
    // pid + per-call counter: a pid-only temp name let two threads of the same process writing
    // into the same dir clobber each other's temp mid-write and rename the wrong bytes into place.
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let tmp = dir.join(format!(".skein.tmp.{}.{n}", std::process::id()));
    fs::write(&tmp, bytes).map_err(|e| format!("writing temp: {e}"))?;
    fs::rename(&tmp, path).map_err(|e| {
        let _ = fs::remove_file(&tmp);
        format!("renaming into place: {e}")
    })
}

/// Run a command with a hard wall-clock bound: kill + reap on expiry, `None` on timeout/spawn
/// failure. Pipes are drained on their own threads so a chatty child can't fill the pipe buffer
/// and deadlock against the polling loop. Dependency-free; callers are all off the async runtime
/// (blocking pool / CLI).
pub(crate) fn output_with_timeout(
    cmd: &mut Command,
    timeout: Duration,
) -> Option<std::process::Output> {
    use std::io::Read as _;
    use std::process::Stdio;
    let mut child = cmd
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .stdin(Stdio::null())
        .spawn()
        .ok()?;
    let mut out_pipe = child.stdout.take()?;
    let mut err_pipe = child.stderr.take()?;
    let out_h = std::thread::spawn(move || {
        let mut v = Vec::new();
        let _ = out_pipe.read_to_end(&mut v);
        v
    });
    let err_h = std::thread::spawn(move || {
        let mut v = Vec::new();
        let _ = err_pipe.read_to_end(&mut v);
        v
    });
    let start = std::time::Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(st)) => break st,
            Ok(None) if start.elapsed() >= timeout => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(10)),
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    };
    Some(std::process::Output {
        status,
        stdout: out_h.join().unwrap_or_default(),
        stderr: err_h.join().unwrap_or_default(),
    })
}

pub(crate) fn bounded_output(
    cmd: &mut Command,
    label: &str,
    timeout: Duration,
) -> Result<std::process::Output, String> {
    output_with_timeout(cmd, timeout).ok_or_else(|| {
        format!(
            "{label} failed to start or exceeded the {}s timeout",
            timeout.as_secs()
        )
    })
}

/// Wrap a string for safe single-quoting in a POSIX shell. Used to quote every value substituted
/// into a `*_CMD` template before it reaches `sh -c`, so a branch/box name can't inject commands.
pub fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

// ───────────────────────────── skein-owned repo registry ─────────────────────────────
//
// skein is no longer single-repo. `~/.skein/repos.json` lists every repo skein manages; each box
// is `<repo-id>-<branch>` and maps back to its repo by id-prefix. This is skein's OWN config —
// distinct from the per-box `sandboxes.json` we dropped — and it's what makes "add a repo URL and
// go" work without the repo shipping anything for skein.

pub(crate) fn program_on_path(name: &str) -> bool {
    env::var_os("PATH").is_some_and(|path| {
        env::split_paths(&path).any(|dir| {
            let candidate = dir.join(name);
            candidate.is_file()
        })
    })
}

/// Expand a leading `~/` to `$HOME` (ssh-add doesn't do shell tilde expansion when called directly).
pub(crate) fn expand_tilde(p: &str) -> String {
    if let Some(rest) = p.strip_prefix("~/") {
        if let Some(home) = env::var_os("HOME") {
            return Path::new(&home).join(rest).to_string_lossy().into_owned();
        }
    }
    p.to_string()
}

/// Sbx sandbox names can't carry every branch character (notably `/`), so the box name is a *slug* of
/// the branch: anything outside `[A-Za-z0-9._-]` becomes `-`, runs collapse, ends trimmed. The real
/// branch (`feat/auth`) is preserved separately (launch spec → `git checkout`); only the *name* is
/// slugged (`<repo>-feat-auth`). Same branch ⇒ same name (stable), so reconnect/lookup are consistent.
pub fn slug(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev_dash = false;
    for c in s.chars() {
        if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
            out.push(c);
            prev_dash = false;
        } else if !prev_dash {
            out.push('-');
            prev_dash = true;
        }
    }
    out.trim_matches('-').to_string()
}

/// Host component of an SSH git URL (for the network-policy hint). `None` if unparseable.
pub(crate) fn host_of(url: &str) -> Option<&str> {
    if let Some(rest) = url.strip_prefix("git@") {
        return rest.split(':').next();
    }
    if let Some(rest) = url.strip_prefix("ssh://") {
        let rest = rest.split_once('@').map(|(_, h)| h).unwrap_or(rest);
        return rest.split(['/', ':']).next();
    }
    None
}

/// Run a program in the repo dir ($SKEIN_REPO, else cwd); returns (stdout, stderr, exit-code).
pub(crate) fn run_capture(prog: &str, args: &[&str]) -> Result<(String, String, i32), String> {
    let mut c = Command::new(prog);
    c.args(args);
    if let Ok(repo) = env::var("SKEIN_REPO") {
        if !repo.is_empty() {
            c.current_dir(repo);
        }
    }
    let timeout = env::var("SKEIN_ACTION_TIMEOUT_SECS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .map(Duration::from_secs)
        .unwrap_or_else(|| Duration::from_secs(30));
    let out = output_with_timeout(&mut c, timeout).ok_or_else(|| {
        format!(
            "{prog} failed to start or exceeded the {}s action timeout",
            timeout.as_secs()
        )
    })?;
    Ok((
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.code().unwrap_or(-1),
    ))
}

pub(crate) fn run_shell(cmd: &str) -> Result<(String, String, i32), String> {
    run_capture("sh", &["-c", cmd])
}

// ───────────────────────────── attachments: paste / drop into a box ─────────────────────────────
//
// The agent runs *inside* the sandbox: it can't see the user's clipboard, their Downloads folder, or
// anything else on the host. Anything the user wants to hand it — a screenshot, a PDF, a video, a
// whole folder of samples — has to be copied into the box first, then referenced by its in-box path.
// One drop (paste, drag-and-drop, file picker) becomes one `/tmp/skein-drop-<batch>/` directory:
// per-batch so a folder keeps its structure and the agent can be handed the directory itself, and so
// same-named files from different drops never clobber each other.

/// Sanitise one browser-supplied path component into a plain, single-segment filename. Letters and
/// digits of any script are kept — `née deed.pdf` and CJK names stay readable rather than turning into
/// hyphen soup — and everything else collapses to `-`: no separator, quote, glob, space, or control
/// character survives, so the name is safe both as a path and as a bare token pasted into a prompt.
/// Leading dots are stripped (kills `..` and dotfiles that would hide the drop) and the name is capped
/// at 80 chars **keeping its extension**, since the suffix is what tells the agent it got an `.mp4`.
pub(crate) fn safe_component(s: &str) -> String {
    let mut out: String = s
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '-'
            }
        })
        .collect();
    while out.starts_with('.') {
        out.remove(0);
    }
    if out.chars().count() > 80 {
        let ext = out
            .rsplit_once('.')
            .map(|(_, e)| e)
            .filter(|e| !e.is_empty() && e.chars().count() <= 8)
            .map(|e| format!(".{e}"))
            .unwrap_or_default();
        // char-wise, not `truncate`: a multibyte name would panic on a byte boundary.
        let stem: String = out.chars().take(80 - ext.chars().count()).collect();
        out = stem + &ext;
    }
    out
}

/// Percent-decode a header value. Filenames are arbitrary UTF-8 (`née.pdf`, CJK, emoji) but HTTP
/// headers are ASCII, so the UI sends `encodeURIComponent(name)` and this reverses it. Invalid
/// escapes are left verbatim rather than erroring — `safe_component` sanitises whatever comes out.
pub fn pct_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            let hex = std::str::from_utf8(&b[i + 1..i + 3]).ok();
            if let Some(v) = hex.and_then(|h| u8::from_str_radix(h, 16).ok()) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// JSON-quote a string without building a Value for it.
pub(crate) fn json_str(s: &str) -> String {
    serde_json::Value::String(s.to_string()).to_string()
}

/// Cut a string to `max` chars on a char boundary, marking that it was cut.
pub(crate) fn clip(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let kept: String = text.chars().take(max).collect();
    format!("{kept}…")
}

/// Keep the END of the output — a test suite says what failed at the bottom.
pub(crate) fn tail_of(text: &str, bytes: usize) -> String {
    if text.len() <= bytes {
        return text.to_string();
    }
    let mut cut = text.len() - bytes;
    while cut < text.len() && !text.is_char_boundary(cut) {
        cut += 1;
    }
    let rest = &text[cut..];
    let from_line = rest.find('\n').map(|i| &rest[i + 1..]).unwrap_or(rest);
    format!("… earlier output trimmed …\n{from_line}")
}

/// First non-empty line of `s`, whitespace-collapsed and capped — the inbox headline. None when
/// `s` is blank.
pub(crate) fn first_line(s: &str) -> Option<String> {
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

pub fn shorten(p: &str) -> String {
    if let Ok(home) = env::var("HOME") {
        if !home.is_empty() && p.starts_with(&home) {
            return format!("~{}", &p[home.len()..]);
        }
    }
    p.to_string()
}
