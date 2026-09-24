//! A box's questions for the person who owns the fleet, and the person's answers (box-plugin §2.3
//! and §4, SKEIN-1061).
//!
//! An agent asks with the `skein_ask_person` tool (`src/plugin/bin/skein-mcp`), which writes one
//! file into the box's own drop-box, `<fleet root>/.skein/asks/requests/<box>/<id>.json`. The
//! launcher makes that directory outside the box's mount namespace and binds it, alone, read-write
//! into that box — the `for asking in substrate gitgate asks` loop in `src/box-session.sh` — so, as
//! for the package and git-write queues beside it, **the directory is the identity** and every
//! field in the file is the box's own word.
//!
//! The owner answers from the cockpit's questions panel. The answer is kept on the **host**, at
//! [`decision_path`], where no box can write, and delivered into the box's read-only inbox as one
//! line (`mailbox::send_question_answer`). Nothing a box writes into its own ask is read as an
//! answer: a `state` or an `answer` in the file is thrown away by [`list`], the rule
//! `gitgate::decide`'s `decided_over` keeps for write requests.
//!
//! **Capped, because the queue is a way to reach a person.** The tool refuses a box's sixth waiting
//! question; a box that writes files itself instead of using the tool gets no further, because
//! [`list`] shows at most [`CAP`] waiting questions per box, oldest first. A question — its text
//! and its options together — is at most [`MAX_BYTES`], and one over that is not shown at all: the
//! tool never writes one, so only a box going round it can.

use crate::util::sh_quote;
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// How many questions one box may have waiting at once. The owner's number (box-plugin.md,
/// answer 2).
pub const CAP: usize = 5;

/// The most a question may be, text and options together, in bytes. The owner's number
/// (feature-wording.md, SKEIN-1060).
pub const MAX_BYTES: usize = 2048;

/// One box's question, as the cockpit is shown it.
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
pub struct Ask {
    #[serde(default)]
    pub id: String,
    /// The box that asked: the directory the file was found in, never the file's own field.
    #[serde(default, rename = "box")]
    pub box_name: String,
    /// The box's own words. Rendered escaped, as plain text.
    #[serde(default)]
    pub question: String,
    /// The answers the box offers, each a button whose label is escaped. Empty means the owner
    /// types a one-line reply.
    #[serde(default)]
    pub options: Vec<String>,
    #[serde(default)]
    pub asked: String,
    /// `waiting`, `answered` or `dismissed`. **As [`list`] returns it, this is the host's**, from
    /// [`decision_path`]; whatever the box's file says here is thrown away.
    #[serde(default)]
    pub state: String,
    /// The owner's answer, when `state` is `answered`. The host's, for `state`'s reason.
    #[serde(default)]
    pub answer: String,
    #[serde(default)]
    pub decided: String,
}

impl Ask {
    /// Why this question must not be shown or answered, or `None`.
    pub fn problem(&self) -> Option<String> {
        if !id_is_nameable(&self.id) {
            return Some(format!("unusable question id {:?}", self.id));
        }
        if !crate::util::valid_name(&self.box_name) {
            return Some(format!("unusable box name {:?}", self.box_name));
        }
        if self.question.trim().is_empty() {
            return Some("an empty question".into());
        }
        if self.size() > MAX_BYTES {
            return Some(format!("{} bytes, over {MAX_BYTES}", self.size()));
        }
        if self
            .options
            .iter()
            .any(|o| o.trim().is_empty() || o.contains('\n') || o.contains('\r'))
        {
            return Some("an option that is empty or more than one line".into());
        }
        None
    }

    /// The question's size as the cap counts it: its text and every option.
    pub fn size(&self) -> usize {
        self.question.len() + self.options.iter().map(String::len).sum::<usize>()
    }
}

/// What the owner did with a question.
#[derive(Debug, Clone, PartialEq)]
pub enum Reply {
    /// Answered, with one of the offered labels or a typed line.
    Answer(String),
    /// Dismissed without an answer. Frees the box's slot, as an answer does.
    Dismiss,
}

/// A question id: a filename and a path component, so the same whitelist of shapes as the other
/// two queues' ids (`gitgate::id_is_nameable`). The tool writes `q-` and eight hex digits.
fn id_is_nameable(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && !id.starts_with('-')
        && !id.contains("..")
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "._-".contains(c))
}

// ───────────────────────────── where things live ─────────────────────────────

/// The queue, **inside the sandbox**, beside `substrate` and `gitgate`: a box must be able to write
/// its own ask, so it cannot be on the host.
pub fn asks_dir() -> String {
    format!("{}/.skein/asks", crate::fleet::fleet_root())
}

fn requests_dir() -> String {
    format!("{}/requests", asks_dir())
}

/// One box's drop-box, spelled once so what removes it with the box is what reads it
/// (`fleet::disk`'s `box_side_state`).
pub(crate) fn box_requests_dir(box_name: &str) -> String {
    format!("{}/{box_name}", requests_dir())
}

/// Where the owner's answer to one question is kept: on the **host**, one file per question, under
/// the box that asked — `gitgate::decision_path`'s place and reasons. Under the box because an id is
/// not an identity: a box can read every other box's queue and file an id it saw there.
pub fn decision_path(box_name: &str, id: &str) -> Option<std::path::PathBuf> {
    (crate::util::valid_name(box_name) && id_is_nameable(id)).then(|| {
        crate::config::skein_home()
            .join("asks")
            .join(box_name)
            .join(format!("{id}.json"))
    })
}

// ───────────────────────────── the queue ─────────────────────────────

/// Parse the array the list script prints. A file that is not an ask is dropped, not fatal.
pub fn parse_asks(json: &str) -> Vec<Ask> {
    let mut out: Vec<Ask> = serde_json::from_str::<Vec<serde_json::Value>>(json)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|v| serde_json::from_value::<Ask>(v).ok())
        .filter(|a| !a.id.is_empty())
        .collect();
    out.sort_by(|a, b| a.asked.cmp(&b.asked).then(a.id.cmp(&b.id)));
    out
}

/// The script that reads the queue, stamping each ask with the box whose directory it is in —
/// `gitgate`'s `list_script`, over this queue. Only `requests/<box>/*.json`: this queue was per box
/// from its first day, so there is no flat layout to read.
fn list_script() -> String {
    format!(
        "d={}; set --; for f in \"$d\"/*/*.json; do [ -f \"$f\" ] && set -- \"$@\" \"$f\"; done; \
         [ $# -gt 0 ] || {{ echo '[]'; exit 0; }}; \
         jq -n --arg d \"$d\" '[inputs | select(type==\"object\") \
           | .box = (input_filename | ltrimstr($d + \"/\") | split(\"/\") | if length == 2 then .[0] else \"\" end)]' \
           \"$@\" 2>/dev/null || echo '[]'",
        sh_quote(&requests_dir())
    )
}

/// Every question the cockpit shows, oldest first: the host's answers over the boxes' asks, and at
/// most [`CAP`] waiting per box.
pub fn list(sandbox: &str) -> Result<Vec<Ask>, String> {
    let out = crate::place::own_sandbox(sandbox).exec(&list_script(), Duration::from_secs(30))?;
    Ok(capped(decided_over(parse_asks(&out))))
}

/// The host's answer wins over the box's copy of it, question by question.
///
/// Separate and pure because it is the rule. What the host recorded replaces the three fields an
/// answer owns — the state, the answer and when — and an ask with no record is `waiting`, whatever
/// its file says: every byte of that file is the box's.
fn decided_over(asked: Vec<Ask>) -> Vec<Ask> {
    asked
        .into_iter()
        .filter(|a| a.problem().is_none())
        .map(|mut a| match decision(&a.box_name, &a.id) {
            Some(host) => {
                a.state = host.state;
                a.answer = host.answer;
                a.decided = host.decided;
                a
            }
            None => {
                a.state = "waiting".into();
                a.answer = String::new();
                a.decided = String::new();
                a
            }
        })
        .collect()
}

/// At most [`CAP`] waiting questions per box, the oldest. The tool refuses a sixth; this is what
/// holds for a box that writes the files itself. Answered and dismissed ones are not counted.
fn capped(asks: Vec<Ask>) -> Vec<Ask> {
    let mut waiting: std::collections::HashMap<String, usize> = Default::default();
    asks.into_iter()
        .filter(|a| {
            if a.state != "waiting" {
                return true;
            }
            let n = waiting.entry(a.box_name.clone()).or_default();
            *n += 1;
            *n <= CAP
        })
        .collect()
}

/// The recorded answer, keeping "nobody has answered" (`Ok(None)`) apart from "skein cannot read
/// the record" (`Err`) — the distinction [`answer`]'s guard turns on (SKEIN-418).
fn decision_or_why(box_name: &str, id: &str) -> Result<Option<Ask>, String> {
    let path = decision_path(box_name, id).ok_or_else(|| format!("unusable question id {id:?}"))?;
    crate::util::read_json_or_why::<Ask>(&path)
}

/// For the reader: an unreadable record reads as none, which shows the question again with its
/// buttons; pressing one is [`answer`], which refuses.
fn decision(box_name: &str, id: &str) -> Option<Ask> {
    decision_or_why(box_name, id).ok().flatten()
}

/// Answer or dismiss **the question the owner was looking at**, once.
///
/// `rendered` rather than an id, for `gitgate::decide`'s reason: the file is the box's and can
/// change between the render and the press. The box, the id and the offered labels come from what
/// the page drew. An answer to a question that offered options must be one of them; one that
/// offered none is a typed line.
///
/// The record is written first and the inbox line second, so a lost line leaves the box reading
/// its question as waiting — the direction that is not a lie.
pub fn answer(rendered: &Ask, reply: &Reply) -> Result<Ask, String> {
    if let Some(why) = rendered.problem() {
        return Err(format!("refusing to act on this question: {why}"));
    }
    let label = match reply {
        Reply::Answer(label) => {
            let label = label.trim();
            if label.is_empty() || label.contains('\n') || label.contains('\r') {
                return Err("an answer is one line of text".into());
            }
            if label.len() > MAX_BYTES {
                return Err(format!("an answer is at most {MAX_BYTES} bytes"));
            }
            if !rendered.options.is_empty() && !rendered.options.iter().any(|o| o == label) {
                return Err("that answer is not one the question offered".into());
            }
            Some(label.to_string())
        }
        Reply::Dismiss => None,
    };
    let path = decision_path(&rendered.box_name, &rendered.id).ok_or("unusable question id")?;
    match decision_or_why(&rendered.box_name, &rendered.id) {
        Ok(None) => {}
        Ok(Some(already)) => {
            return Err(format!(
                "question {} is already {} — a question is answered once",
                rendered.id, already.state
            ))
        }
        Err(why) => {
            return Err(format!(
                "refusing to answer {} — skein cannot read the answer it may already have given \
                 ({why})",
                rendered.id
            ))
        }
    }
    let mut done = rendered.clone();
    done.state = if label.is_some() {
        "answered"
    } else {
        "dismissed"
    }
    .into();
    done.answer = label.clone().unwrap_or_default();
    done.decided = chrono::Utc::now().to_rfc3339();
    let dir = path.parent().ok_or("no answers directory")?;
    std::fs::create_dir_all(dir).map_err(|e| format!("mkdir {}: {e}", dir.display()))?;
    let body = serde_json::to_vec_pretty(&done).map_err(|e| e.to_string())?;
    crate::util::write_atomic(&path, dir, &body)?;
    if let Err(e) =
        crate::mailbox::send_question_answer(&rendered.box_name, &rendered.id, label.as_deref())
    {
        eprintln!(
            "skein: the answer to question {} did not reach {}'s inbox: {e}",
            rendered.id, rendered.box_name
        );
    }
    Ok(done)
}

/// Every question in the fleet, for the cockpit. An unreachable sandbox reads as none, as the
/// other two queues do.
pub fn fleet_asks() -> Vec<Ask> {
    list(&crate::place::fleet_sandbox()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ask(box_name: &str, id: &str, asked: &str) -> Ask {
        Ask {
            id: id.into(),
            box_name: box_name.into(),
            question: "keep the table?".into(),
            options: vec!["Drop it".into(), "Keep it".into()],
            asked: asked.into(),
            ..Default::default()
        }
    }

    /// **A box's own `state` and `answer` are never the answer**, and the host's record is.
    ///
    /// What would make it fail: `decided_over` keeping the file's `state` or `answer` for a
    /// question with no record (the `None` arm), or reading the record from anywhere but
    /// [`decision_path`].
    #[test]
    fn only_the_hosts_record_answers_a_question() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        let mut env = crate::testutil::env_pins();
        env.set("SKEIN_HOME", &home);

        let mut forged = ask("web-main", "q-00000001", "2026-09-24T10:00:00Z");
        forged.state = "answered".into();
        forged.answer = "Drop it".into();
        let real = ask("web-main", "q-00000002", "2026-09-24T10:01:00Z");
        answer(&real, &Reply::Answer("Keep it".into())).expect("answered");

        let shown = decided_over(vec![forged, real]);
        assert_eq!(
            (shown[0].state.as_str(), shown[0].answer.as_str()),
            ("waiting", ""),
            "a box's own file answered its question: {shown:?}"
        );
        assert_eq!(
            (shown[1].state.as_str(), shown[1].answer.as_str()),
            ("answered", "Keep it"),
            "{shown:?}"
        );
    }

    /// **At most [`CAP`] waiting per box are shown**, the oldest, and answered ones do not count.
    ///
    /// What would make it fail: `capped` not applied in `list`, counting across boxes, or counting
    /// answered questions against the cap.
    #[test]
    fn a_box_that_goes_round_the_tool_still_gets_five_waiting() {
        let mut asks: Vec<Ask> = (0..8)
            .map(|i| {
                let mut a = ask(
                    "web-main",
                    &format!("q-{i}"),
                    &format!("2026-09-24T10:0{i}:00Z"),
                );
                a.state = if i == 0 { "answered" } else { "waiting" }.into();
                a
            })
            .collect();
        let mut other = ask("other-main", "q-x", "2026-09-24T10:09:00Z");
        other.state = "waiting".into();
        asks.push(other);
        let ids: Vec<String> = capped(asks).into_iter().map(|a| a.id).collect();
        assert_eq!(
            ids,
            ["q-0", "q-1", "q-2", "q-3", "q-4", "q-5", "q-x"],
            "not the answered one, the five oldest waiting, and the other box's"
        );
    }

    /// **An answer is recorded once, delivered to the asking box in the approved words, and a
    /// dismissal is an answer too.**
    ///
    /// What would make it fail: `answer` not calling `send_question_answer`, or with another body;
    /// a second answer allowed; a label the question did not offer accepted.
    #[test]
    fn an_answer_reaches_the_box_once_in_the_approved_words() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        let mut env = crate::testutil::env_pins();
        env.set("SKEIN_HOME", &home);
        std::fs::create_dir_all(home.join("boxes/web-main/inbox")).unwrap();

        let a = ask("web-main", "q-7f3a0000", "2026-09-24T10:00:00Z");
        assert!(answer(&a, &Reply::Answer("Something else".into())).is_err());
        answer(&a, &Reply::Answer("Keep it".into())).expect("answered");
        assert!(answer(&a, &Reply::Dismiss).is_err(), "answered twice");

        let b = ask("web-main", "q-81c20000", "2026-09-24T10:00:00Z");
        answer(&b, &Reply::Dismiss).expect("dismissed");

        let mut bodies: Vec<String> = std::fs::read_dir(home.join("boxes/web-main/inbox"))
            .unwrap()
            .flatten()
            .map(|e| {
                let m: serde_json::Value =
                    serde_json::from_str(&std::fs::read_to_string(e.path()).unwrap()).unwrap();
                assert_eq!(m["kind"], "answer");
                m["body"].as_str().unwrap().to_string()
            })
            .collect();
        bodies.sort();
        assert_eq!(
            bodies,
            [
                "question q-7f3a0000, \"Keep it\"",
                "question q-81c20000 was dismissed without an answer"
            ]
        );
    }
}
