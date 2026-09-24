//! Which of skein's own questions a model call was, so its spend can be told apart from an agent's
//! work (SKEIN-1074).
//!
//! **Why this exists.** Every model call skein makes for itself is a `claude -p` spawned on the
//! fleet's login, and nothing about the transcript it leaves behind said which question it was. A
//! call that ran in this process wrote under `fleet-home`, which the usage reader never walked, so
//! it was invisible; a call that ran in a review box wrote beside that box's own work and was
//! counted as if the box had spent it. Neither can be priced as "what one pull-request reading
//! costs" without first knowing which calls were which.
//!
//! **Two ways a call is labelled, because two kinds of call exist.**
//!
//! * A call that belongs to no conversation ([`Turn::Alone`]) is given a fresh `--session-id` whose
//!   first bytes say "skein's own" and whose next byte is the call site. The transcript file is
//!   named after that id, so the label is on disk before anything else is — [`site_of_session`]
//!   reads it straight off the path.
//! * A turn of a pull request's conversation cannot be, because its id is derived from the pull
//!   request ([`conversation_for`]) and the next round resumes it by that id: an id that also
//!   encoded the call site would be a different conversation per question, and every resume would
//!   miss. So those turns are written to a small ledger instead — the session, the call site, and
//!   the moment the call started and ended — and the usage reader attributes each record in that
//!   session to the call whose window it falls in.
//!
//! **Only counts and ids are written.** The ledger holds a call site, a session id and two clock
//! readings per line; no prompt, no answer, no repository name. It is the same property
//! `crate::usage` keeps about what leaves a transcript.

use super::turn::sha256;
use super::*;
use std::cell::Cell;
use std::io::Write;

/// One of skein's own questions to a model — S1 to S11 in `token-spend.md` §1.
///
/// The number is the design's, and it is also what is written into a labelled session id and into
/// the ledger, so it never changes meaning once assigned: a new call site takes a new number.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Site {
    /// S1: the stage-1 summary of a pull request.
    Summary = 1,
    /// S2: the stage-2 detail, when stage 1 says a change needs explaining.
    Detail = 2,
    /// S3: the merged summary and review.
    Review = 3,
    /// S4: the coverage sweep, the review's second turn.
    Sweep = 4,
    /// S5: the audit of a check the repository owes.
    Audit = 5,
    /// S6: a question a person asked about a pull request.
    Ask = 6,
    /// S7: a review comment drafted from a person's notes.
    Draft = 7,
    /// S8: a module note.
    ModuleNote = 8,
    /// S9: a one-line narration of what a box did.
    Narrate = 9,
    /// S10: the hold-or-continue gate in front of batch resume.
    HoldGate = 10,
    /// S11: `skein doctor` asking whether the model answers.
    Doctor = 11,
}

impl Site {
    /// Every call site, in the design's order.
    pub const ALL: [Site; 11] = [
        Site::Summary,
        Site::Detail,
        Site::Review,
        Site::Sweep,
        Site::Audit,
        Site::Ask,
        Site::Draft,
        Site::ModuleNote,
        Site::Narrate,
        Site::HoldGate,
        Site::Doctor,
    ];

    /// The design's number for this site.
    pub fn number(self) -> u8 {
        self as u8
    }

    /// `S1` … `S11`, the name the usage payload carries and the cockpit keys its words on.
    pub fn code(self) -> String {
        format!("S{}", self.number())
    }

    pub fn from_number(n: u8) -> Option<Site> {
        Site::ALL.into_iter().find(|s| s.number() == n)
    }

    pub fn from_code(code: &str) -> Option<Site> {
        code.strip_prefix('S')?
            .parse::<u8>()
            .ok()
            .and_then(Site::from_number)
    }

    /// The sites one pull-request reading is made of: S1 to S5, including the narrow fallback a
    /// merged reading takes when it runs out of time, which is S1 and S2 again.
    pub fn is_reading(self) -> bool {
        matches!(
            self,
            Site::Summary | Site::Detail | Site::Review | Site::Sweep | Site::Audit
        )
    }
}

thread_local! {
    static CURRENT: Cell<Option<Site>> = const { Cell::new(None) };
}

/// Run `call` as `site`: every model call made inside it, on this thread, is labelled `site`.
///
/// A scope rather than a parameter, and the reason is the size of the change it would otherwise
/// be. Every model call already arrives at [`claude_in_turn`], through four doors with a dozen test
/// callers between them; threading a site through all of them would rewrite each call to add a
/// label and nothing else. The calls themselves are synchronous — `claude_in_turn` blocks until the
/// CLI exits — so the thread that set the label is the thread that spends it.
///
/// Restored on the way out, so a site nested inside another (the narrow fallback running S1 inside
/// an S3 reading) labels only its own calls.
pub fn as_site<T>(site: Site, call: impl FnOnce() -> T) -> T {
    struct Restore(Option<Site>);
    impl Drop for Restore {
        fn drop(&mut self) {
            CURRENT.with(|c| c.set(self.0));
        }
    }
    let _restore = Restore(CURRENT.with(|c| c.replace(Some(site))));
    call()
}

/// The site the calling thread is labelled with, if any.
pub(crate) fn current_site() -> Option<Site> {
    CURRENT.with(|c| c.get())
}

/// The first five bytes of every labelled session id, `5ce10a11-00`; the sixth is the site's.
///
/// Five fixed bytes rather than one so that a conversation id, which is a hash, cannot pass for a
/// labelled one: the chance that a hash begins with these is one in 2⁴⁰.
const LABEL: [u8; 5] = [0x5c, 0xe1, 0x0a, 0x11, 0x00];

/// A fresh session id for one call at `site`, never used before.
///
/// Fresh because `--session-id` on an id that already exists is refused (see [`Turn`]), and a call
/// that belongs to no conversation must never find one. The remaining bytes are a hash of the
/// clock, the process and a counter, which is all the uniqueness one machine's calls need.
pub fn labelled_session(site: Site) -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let seed = format!(
        "{nanos}:{}:{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    );
    let d = sha256(seed.as_bytes());
    let mut b = [0u8; 16];
    b.copy_from_slice(&d[..16]);
    b[..5].copy_from_slice(&LABEL);
    b[5] = site.number();
    // Version 8 and the RFC 9562 variant, as [`conversation_for`] does: the CLI is handed a uuid.
    b[6] = (b[6] & 0x0f) | 0x80;
    b[8] = (b[8] & 0x3f) | 0x80;
    let hex: String = b.iter().map(|x| format!("{x:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}

/// The site a labelled session id names, or `None` for any other id.
pub fn site_of_session(id: &str) -> Option<Site> {
    let hex: String = id.chars().filter(|c| *c != '-').collect();
    if hex.len() != 32 || !id.is_ascii() {
        return None;
    }
    let prefix: String = LABEL.iter().map(|x| format!("{x:02x}")).collect();
    if !hex.to_ascii_lowercase().starts_with(&prefix) {
        return None;
    }
    u8::from_str_radix(&hex[10..12], 16)
        .ok()
        .and_then(Site::from_number)
}

/// Is this id one skein derived rather than one the CLI drew at random?
///
/// Both kinds skein makes — [`conversation_for`] and [`labelled_session`] — carry RFC 9562's
/// version 8; the CLI's own are version 4. So a version-8 session in a box that the ledger does
/// not know is one of skein's calls from before it labelled them.
pub fn is_derived_session(id: &str) -> bool {
    let parts: Vec<&str> = id.split('-').collect();
    parts.len() == 5
        && parts.iter().map(|p| p.len()).collect::<Vec<_>>() == [8, 4, 4, 4, 12]
        && parts[2].starts_with('8')
}

/// Where the ledger lives: beside the usage reader's own files, in skein's home.
pub fn own_ledger_path() -> std::path::PathBuf {
    crate::config::skein_home().join("usage-own.jsonl")
}

/// Milliseconds since the epoch.
pub(crate) fn epoch_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// One line appended to the ledger. Best effort: a ledger that cannot be written costs the
/// attribution of one call, and never the call.
fn append(line: serde_json::Value) {
    let path = own_ledger_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        // One write per line, so two calls finishing at once cannot interleave inside a line.
        let _ = f.write_all(format!("{line}\n").as_bytes());
    }
}

/// Write down that a call at `site` ran in `session` between two moments.
fn note_call(site: Site, session: &str, from_ms: i64, to_ms: i64) {
    append(serde_json::json!({
        "site": site.code(),
        "session": session,
        "from": from_ms,
        "to": to_ms,
    }));
}

/// A call's window, written to the ledger when the call ends — however it ends.
///
/// A guard because [`claude_in_turn`] returns from four places and a window written at only some
/// of them would leave the rest of a session's records unattributed.
pub(crate) struct CallNoted {
    noted: Option<(Site, String, i64)>,
}

impl CallNoted {
    /// Nothing is written for a call outside [`as_site`], or for one that names no session.
    pub(crate) fn begin(turn: Turn<'_>) -> CallNoted {
        let session = match turn {
            Turn::Alone => None,
            Turn::Labelled { id } | Turn::Opening { id, .. } | Turn::Resuming { id, .. } => {
                Some(id)
            }
        };
        CallNoted {
            noted: current_site()
                .zip(session)
                .map(|(site, id)| (site, id.to_string(), epoch_ms())),
        }
    }
}

impl Drop for CallNoted {
    fn drop(&mut self) {
        if let Some((site, session, from_ms)) = self.noted.take() {
            note_call(site, &session, from_ms, epoch_ms());
        }
    }
}

/// Write down that a pull-request reading ended, and whether it finished.
///
/// "Finished" is a reading that produced a summary; one that came back unread — refused, out of
/// time, a box that did not answer, an answer skein could not parse — did not, and its spend is
/// charged to the readings that did.
pub fn note_reading(finished: bool) {
    append(serde_json::json!({
        "reading": if finished { "finished" } else { "unfinished" },
        "at": epoch_ms(),
    }));
}

/// One call's window, as the ledger has it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Window {
    pub site: Site,
    pub from_ms: i64,
    pub to_ms: i64,
}

/// Everything the ledger says, read back.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OwnLedger {
    /// Each labelled session's calls, oldest first.
    pub calls: std::collections::HashMap<String, Vec<Window>>,
    pub readings_finished: u64,
    pub readings_unfinished: u64,
}

impl OwnLedger {
    /// The call in `session` whose window holds `at_ms`, and its index among that session's calls.
    pub fn window_at(&self, session: &str, at_ms: i64) -> Option<(usize, Site)> {
        self.calls
            .get(session)?
            .iter()
            .enumerate()
            .find(|(_, w)| w.from_ms <= at_ms && at_ms <= w.to_ms)
            .map(|(i, w)| (i, w.site))
    }
}

/// Read the ledger. A line that does not parse is skipped: it is a record of accounting, and one
/// torn line must not cost every other.
pub fn own_ledger() -> OwnLedger {
    let mut out = OwnLedger::default();
    let Ok(body) = std::fs::read_to_string(own_ledger_path()) else {
        return out;
    };
    for line in body.lines() {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if let Some(r) = v.get("reading").and_then(|r| r.as_str()) {
            match r {
                "finished" => out.readings_finished += 1,
                "unfinished" => out.readings_unfinished += 1,
                _ => {}
            }
            continue;
        }
        let site = v
            .get("site")
            .and_then(|s| s.as_str())
            .and_then(Site::from_code);
        let session = v.get("session").and_then(|s| s.as_str());
        let from = v.get("from").and_then(|n| n.as_i64());
        let to = v.get("to").and_then(|n| n.as_i64());
        if let (Some(site), Some(session), Some(from_ms), Some(to_ms)) = (site, session, from, to) {
            out.calls
                .entry(session.to_string())
                .or_default()
                .push(Window {
                    site,
                    from_ms,
                    to_ms,
                });
        }
    }
    for windows in out.calls.values_mut() {
        windows.sort_by_key(|w| w.from_ms);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Fails if a labelled id stops carrying its site, or if a conversation id reads as labelled.
    #[test]
    fn a_labelled_session_names_its_call_site_and_a_conversation_does_not() {
        for site in Site::ALL {
            let id = labelled_session(site);
            assert_eq!(site_of_session(&id), Some(site), "{id} lost its call site");
            assert!(
                is_derived_session(&id),
                "{id} is not a uuid the CLI will accept"
            );
            assert_ne!(
                labelled_session(site),
                id,
                "two calls at one site got one session id, and the CLI refuses the second"
            );
        }
        let conversation = conversation_for("acme", 41);
        assert_eq!(site_of_session(&conversation), None);
        assert!(is_derived_session(&conversation));
        assert!(!is_derived_session("0b7f7d0e-3c1a-4d2e-9f00-123456789abc"));
    }

    /// Fails if a nested site leaks out of its scope, which would label the rest of a reading as
    /// whatever its fallback was.
    #[test]
    fn a_site_labels_only_the_calls_inside_it() {
        assert_eq!(current_site(), None);
        as_site(Site::Review, || {
            assert_eq!(current_site(), Some(Site::Review));
            as_site(Site::Summary, || {
                assert_eq!(current_site(), Some(Site::Summary))
            });
            assert_eq!(current_site(), Some(Site::Review));
        });
        assert_eq!(current_site(), None);
    }
}
