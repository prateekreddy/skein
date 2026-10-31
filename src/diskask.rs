//! A box's ask for a bigger disk allowance, and the owner's answer (SKEIN-1425).
//!
//! An agent asks with the `skein_request_disk` tool (`src/plugin/bin/skein-mcp`), which writes one
//! file, [`ASK_FILE`], into the box's own package drop-box: the directory the launcher makes and
//! binds, alone, read-write into that box (`substrate::box_requests_dir`). As for the other queues,
//! **the directory is the identity** and every field in the file is the box's own word. Not a
//! `.json`, so the package queue never reads it, the way `fleet::versions`' `restart-when-idle`
//! sits beside it: a box's ask about itself, not a request for the fleet.
//!
//! **One file, so one open ask per box**: a new ask replaces the old one, and carries a new id. The
//! owner's answer is kept on the **host**, in the box's declared state (`fleet::box_declared`,
//! `DECISION_FLAG`), where no box can write, and it answers the ask whose id it names and no other.
//! So a replaced ask is a new question, and an answer cannot be carried over to it.
//!
//! **Approve is the box-settings field's own path**, `fleet::set_box_disk_limit`, so it takes effect
//! at once: the allowance is read at every check. The box is told in its read-only inbox
//! (`mailbox::send_answer`, `disk request <id>, approved` or `denied`), which `skein_requests`
//! reads, and in its resource signal (`resource_signal`, `disk_ask`).
//!
//! **What the owner approves is what the page drew.** The press echoes the box, the id and the size
//! back, and [`decide`] refuses when the box's file no longer says that: a box can rewrite its own
//! ask between the render and the press.

use serde::{Deserialize, Serialize};

/// The ask's name in the box's package drop-box.
pub const ASK_FILE: &str = "disk-allowance";

/// The most a reason may be, in bytes. The owner reads it on one row of the decision inbox; the
/// tool refuses a longer one, and one a box wrote round the tool is not shown.
pub const MAX_WHY: usize = 300;

/// Where the owner's answer is kept, in the box's declared state on the host.
const DECISION_FLAG: &str = "disk-ask";

/// One box's ask, as the cockpit is shown it.
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
pub struct Ask {
    #[serde(default)]
    pub id: String,
    /// The box that asked: the directory the file was found in, never the file's own field.
    #[serde(default, rename = "box")]
    pub box_name: String,
    /// The allowance asked for, as the box wrote it (`20g`).
    #[serde(default)]
    pub size: String,
    /// [`Ask::size`] in MiB, by `fleet::parse_mib`. The host's reading of the box's word; whatever
    /// the file says here is replaced.
    #[serde(default)]
    pub mb: u64,
    /// The box's own reason. Rendered escaped, as plain text.
    #[serde(default)]
    pub why: String,
    #[serde(default)]
    pub asked: String,
    /// `waiting`, `approved` or `denied`: **the host's**, from the declared record. Whatever the
    /// box's file says is thrown away.
    #[serde(default)]
    pub state: String,
    #[serde(default)]
    pub decided: String,
}

/// The host's record of one answer.
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
struct Decision {
    id: String,
    mb: u64,
    state: String,
    decided: String,
}

impl Ask {
    /// Why this ask must not be shown or decided, or `None`.
    pub fn problem(&self) -> Option<String> {
        if !id_is_nameable(&self.id) {
            return Some(format!("unusable ask id {:?}", self.id));
        }
        if !crate::util::valid_name(&self.box_name) {
            return Some(format!("unusable box name {:?}", self.box_name));
        }
        if self.mb == 0 {
            return Some(format!("{:?} is not a size", self.size));
        }
        let why = self.why.trim();
        if why.is_empty() || why.contains('\n') || why.contains('\r') {
            return Some("a reason that is empty or more than one line".into());
        }
        if why.len() > MAX_WHY {
            return Some(format!("a reason of {} bytes, over {MAX_WHY}", why.len()));
        }
        None
    }
}

/// An ask id: a path component and an inbox word, the shapes `asks` allows. The tool writes `d-`
/// and eight hex digits.
fn id_is_nameable(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && !id.starts_with('-')
        && !id.contains("..")
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "._-".contains(c))
}

/// A size as the inbox prints it: `20 GB`, `12.6 GB`.
pub fn gb(mib: u64) -> String {
    let tenths = (mib * 10 + 512) / 1024;
    match tenths % 10 {
        0 => format!("{} GB", tenths / 10),
        t => format!("{}.{t} GB", tenths / 10),
    }
}

/// The decision inbox's line for an ask: `asks for 20 GB disk (now 10 GB, using 12.6 GB)`.
pub fn headline(ask: &Ask, now_mb: u64, using_mb: Option<u64>) -> String {
    match using_mb {
        Some(using) => format!(
            "asks for {} disk (now {}, using {})",
            gb(ask.mb),
            gb(now_mb),
            gb(using)
        ),
        None => format!("asks for {} disk (now {})", gb(ask.mb), gb(now_mb)),
    }
}

// ───────────────────────────── the queue ─────────────────────────────

/// The box's ask as its file says it, stamped with the box, or `None`. Read through
/// `boxfile::read_in`, so a link, a FIFO or an oversized file in its place is never read.
fn filed(box_name: &str) -> Option<Ask> {
    if !crate::util::valid_name(box_name) {
        return None;
    }
    let dir = crate::substrate::box_requests_dir(box_name);
    let text = crate::boxfile::read_in(
        std::path::Path::new(&dir),
        ASK_FILE,
        crate::boxfile::DROP_BOX_CAP,
    )
    .ok()?;
    let mut ask: Ask = serde_json::from_str(&text).ok()?;
    ask.box_name = box_name.to_string();
    ask.mb = crate::fleet::parse_mib(&ask.size).unwrap_or(0);
    Some(ask)
}

/// The host's answer, or none. An unreadable record reads as none: the ask shows again, and
/// pressing a button is [`decide`], which refuses.
fn decision(box_name: &str) -> Option<Decision> {
    let raw = crate::fleet::declared_read(box_name, DECISION_FLAG)?;
    serde_json::from_str(&raw).ok()
}

/// The host's answer over the box's file, for the one id it names. Pure but for the record's read.
fn decided_over(mut ask: Ask) -> Ask {
    match decision(&ask.box_name).filter(|d| d.id == ask.id) {
        Some(d) => {
            ask.state = d.state;
            ask.decided = d.decided;
        }
        None => {
            ask.state = "waiting".into();
            ask.decided = String::new();
        }
    }
    ask
}

/// This box's ask, with the host's answer over it, or `None` when it has none that can be shown.
pub fn for_box(box_name: &str) -> Option<Ask> {
    filed(box_name)
        .filter(|a| a.problem().is_none())
        .map(decided_over)
}

/// Every box's ask, oldest first: one per box at most, by the file's single name.
pub fn list(sandbox: &str) -> Result<Vec<Ask>, String> {
    crate::place::standing_in(sandbox)?;
    let root = crate::substrate::requests_dir();
    let Ok(entries) = std::fs::read_dir(&root) else {
        return Ok(Vec::new());
    };
    let mut out: Vec<Ask> = entries
        .flatten()
        // The entry's own type, which is not followed: a box's drop-box is a directory.
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .filter_map(|e| e.file_name().into_string().ok())
        .filter_map(|name| for_box(&name))
        .collect();
    out.sort_by(|a, b| a.asked.cmp(&b.asked).then(a.box_name.cmp(&b.box_name)));
    Ok(out)
}

/// Every ask in the fleet, for the cockpit. An unreachable sandbox reads as none, as the other
/// queues do.
pub fn fleet_asks() -> Vec<Ask> {
    list(&crate::place::fleet_sandbox()).unwrap_or_default()
}

/// Each ask with the two figures its inbox row prints: the allowance its box has now, `None` for
/// the whole disk, and what it holds, `None` when the disk walk has no figure for it. The walk is
/// the board's own five-minute figure (`fleet::fleet_disk_usage`), and is asked only when there is
/// an ask.
pub fn with_figures(asks: Vec<Ask>) -> Vec<(Ask, Option<u64>, Option<u64>)> {
    if asks.is_empty() {
        return Vec::new();
    }
    let usage = crate::fleet::fleet_disk_usage();
    asks.into_iter()
        .map(|a| {
            let now = crate::fleet::box_disk_limit(&a.box_name);
            let using = usage.get(&a.box_name).copied();
            (a, now, using)
        })
        .collect()
}

/// Whether an ask is one the owner can be asked: still waiting, and more than the allowance the
/// box has now. An ask for no more than it has, or by a box allowed the whole disk, raises nothing.
pub fn open_above(ask: &Ask, now_mb: Option<u64>) -> bool {
    ask.state == "waiting" && now_mb.is_some_and(|now| ask.mb > now)
}

/// Approve or deny **the ask the owner was looking at**, once.
///
/// `rendered` carries the box, the id and the size in MiB the page drew. An approval sets the
/// allowance first, through the box-settings field's path, so a record never says approved over an
/// allowance that did not change; then the record, then the inbox line, so a lost line leaves the
/// box reading its ask as waiting, the direction that is not a lie.
pub fn decide(rendered: &Ask, approve: bool) -> Result<Ask, String> {
    let current = filed(&rendered.box_name)
        .ok_or_else(|| format!("{} has no disk ask now", rendered.box_name))?;
    if let Some(why) = current.problem() {
        return Err(format!("refusing to act on this ask: {why}"));
    }
    if current.id != rendered.id || current.mb != rendered.mb {
        return Err(format!(
            "{}'s ask has changed since this page drew it. Look at it again.",
            rendered.box_name
        ));
    }
    if let Some(d) = decision(&current.box_name).filter(|d| d.id == current.id) {
        return Err(format!(
            "this ask is already {}; an ask is decided once",
            d.state
        ));
    }
    let state = if approve {
        let now = crate::fleet::box_disk_limit(&current.box_name);
        if !now.is_some_and(|now| current.mb > now) {
            return Err(format!(
                "{} would not raise {}'s allowance, so there is nothing to approve",
                gb(current.mb),
                current.box_name
            ));
        }
        crate::fleet::set_box_disk_limit(&current.box_name, Some(&format!("{}m", current.mb)))?;
        "approved"
    } else {
        "denied"
    };
    let record = Decision {
        id: current.id.clone(),
        mb: current.mb,
        state: state.into(),
        decided: chrono::Utc::now().to_rfc3339(),
    };
    let body = serde_json::to_vec_pretty(&record).map_err(|e| e.to_string())?;
    crate::fleet::declared_write(&current.box_name, DECISION_FLAG, &body)?;
    if let Err(e) = crate::mailbox::send_answer(&current.box_name, "disk", &current.id, state) {
        eprintln!(
            "skein: the answer to disk ask {} did not reach {}'s inbox: {e}",
            current.id, current.box_name
        );
    }
    let mut done = current;
    done.state = record.state;
    done.decided = record.decided;
    Ok(done)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Plant `body` as `box_name`'s ask, the way the tool writes it.
    fn plant(box_name: &str, body: serde_json::Value) {
        let dir = crate::substrate::box_requests_dir(box_name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(std::path::Path::new(&dir).join(ASK_FILE), body.to_string()).unwrap();
    }

    fn asked(id: &str, size: &str) -> serde_json::Value {
        serde_json::json!({"id": id, "size": size, "why": "the index is 14G",
                           "asked": "2026-10-31T10:00:00Z"})
    }

    fn rendered(box_name: &str, id: &str, mb: u64) -> Ask {
        Ask {
            id: id.into(),
            box_name: box_name.into(),
            mb,
            ..Default::default()
        }
    }

    /// **Approve changes the allowance at once, through the settings field's path, and the box is
    /// told; a second press is refused.**
    ///
    /// What would make it fail: `decide` recording without calling `set_box_disk_limit`, the inbox
    /// line in other words than `disk request <id>, approved`, or a decided ask decided again.
    #[test]
    fn approving_raises_the_allowance_and_tells_the_box() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        let fleet = crate::testutil::tempdir();
        let mut env = crate::testutil::env_pins();
        env.set("SKEIN_HOME", &home);
        env.set("SKEIN_FLEET_ROOT", &fleet);
        plant("web-main", asked("d-0000aaaa", "20g"));
        assert_eq!(crate::fleet::box_disk_limit("web-main"), Some(10 * 1024));

        let done = decide(&rendered("web-main", "d-0000aaaa", 20 * 1024), true).expect("approved");
        assert_eq!(done.state, "approved");
        assert_eq!(crate::fleet::box_disk_limit("web-main"), Some(20 * 1024));
        assert_eq!(for_box("web-main").unwrap().state, "approved");
        let inbox = crate::mailbox::owner_inbox("web-main");
        let said: Vec<String> = std::fs::read_dir(&inbox)
            .unwrap()
            .flatten()
            .map(|e| std::fs::read_to_string(e.path()).unwrap())
            .collect();
        assert!(
            said.iter()
                .any(|m| m.contains("disk request d-0000aaaa, approved")),
            "{said:?}"
        );
        assert!(decide(&rendered("web-main", "d-0000aaaa", 20 * 1024), true).is_err());
    }

    /// **Deny changes nothing but the record, and the box is told.**
    ///
    /// What would make it fail: a denial that sets the allowance, or one recorded as anything but
    /// `denied`.
    #[test]
    fn denying_leaves_the_allowance_and_tells_the_box() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        let fleet = crate::testutil::tempdir();
        let mut env = crate::testutil::env_pins();
        env.set("SKEIN_HOME", &home);
        env.set("SKEIN_FLEET_ROOT", &fleet);
        plant("web-main", asked("d-0000bbbb", "20g"));
        let done = decide(&rendered("web-main", "d-0000bbbb", 20 * 1024), false).expect("denied");
        assert_eq!(done.state, "denied");
        assert_eq!(crate::fleet::box_disk_limit("web-main"), Some(10 * 1024));
        assert_eq!(for_box("web-main").unwrap().state, "denied");
    }

    /// **A new ask replaces the old one, and the old one's answer is not the new one's.** And the
    /// owner's press acts only on the ask the page drew.
    ///
    /// What would make it fail: `decided_over` applying a record whatever id it names, or `decide`
    /// not comparing the id and the size with what the box's file says now.
    #[test]
    fn a_new_ask_replaces_the_old_and_is_asked_afresh() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        let fleet = crate::testutil::tempdir();
        let mut env = crate::testutil::env_pins();
        env.set("SKEIN_HOME", &home);
        env.set("SKEIN_FLEET_ROOT", &fleet);
        plant("web-main", asked("d-0000cccc", "15g"));
        decide(&rendered("web-main", "d-0000cccc", 15 * 1024), false).expect("denied");
        plant("web-main", asked("d-0000dddd", "20g"));
        let now = for_box("web-main").unwrap();
        assert_eq!(
            (now.id.as_str(), now.state.as_str()),
            ("d-0000dddd", "waiting")
        );
        assert!(
            decide(&rendered("web-main", "d-0000cccc", 15 * 1024), true).is_err(),
            "the replaced ask was approved"
        );
        assert!(
            decide(&rendered("web-main", "d-0000dddd", 15 * 1024), true).is_err(),
            "approved a size the page did not draw"
        );
        assert_eq!(crate::fleet::box_disk_limit("web-main"), Some(10 * 1024));
    }

    /// **A size that is not a size is never shown, and an ask for no more than the box has is not
    /// one the owner is asked.**
    ///
    /// What would make it fail: `problem` not refusing `mb == 0`, or `open_above` letting through an
    /// ask at or below the allowance, or one from a box allowed the whole disk.
    #[test]
    fn a_bad_size_is_not_shown_and_a_smaller_one_is_not_asked() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        let fleet = crate::testutil::tempdir();
        let mut env = crate::testutil::env_pins();
        env.set("SKEIN_HOME", &home);
        env.set("SKEIN_FLEET_ROOT", &fleet);
        plant("web-main", asked("d-0000eeee", "plenty"));
        assert_eq!(for_box("web-main"), None);
        plant("web-main", asked("d-0000eeee", "8g"));
        let small = for_box("web-main").unwrap();
        assert!(!open_above(&small, Some(10 * 1024)));
        assert!(decide(&rendered("web-main", "d-0000eeee", 8 * 1024), true).is_err());
        plant("web-main", asked("d-0000ffff", "20g"));
        let big = for_box("web-main").unwrap();
        assert!(open_above(&big, Some(10 * 1024)));
        assert!(
            !open_above(&big, None),
            "a box allowed everything was asked"
        );
    }

    /// **A box's own `state` is thrown away**: an ask whose file says `approved` or `denied`, with no
    /// host record, reads as waiting and is a row in the decision inbox.
    ///
    /// What would make it fail: `decided_over`'s `None` arm keeping the file's `state` — the ask
    /// then reads as decided and `open_above` drops it from the inbox.
    #[test]
    fn a_box_that_says_its_own_ask_is_decided_still_waits() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        let fleet = crate::testutil::tempdir();
        let mut env = crate::testutil::env_pins();
        env.set("SKEIN_HOME", &home);
        env.set("SKEIN_FLEET_ROOT", &fleet);
        for claimed in ["approved", "denied"] {
            let mut body = asked("d-0000abab", "20g");
            body["state"] = claimed.into();
            body["decided"] = "2026-10-31T10:01:00Z".into();
            plant("web-main", body);
            let ask = for_box("web-main").expect("the box's ask");
            assert_eq!(
                (ask.state.as_str(), ask.decided.as_str()),
                ("waiting", ""),
                "a box's own {claimed:?} was believed"
            );
            let now = crate::fleet::box_disk_limit("web-main");
            // The inbox's own filter (`queue::disk_decisions`).
            assert!(
                open_above(&ask, now),
                "a box that says {claimed:?} left the inbox"
            );
        }
    }

    /// The inbox's figures, as the owner approved the row.
    #[test]
    fn the_headline_reads_in_gigabytes() {
        let ask = Ask {
            mb: 20 * 1024,
            ..Default::default()
        };
        assert_eq!(
            headline(&ask, 10 * 1024, Some(12_902)),
            "asks for 20 GB disk (now 10 GB, using 12.6 GB)"
        );
        assert_eq!(
            headline(&ask, 10 * 1024, None),
            "asks for 20 GB disk (now 10 GB)"
        );
    }
}
