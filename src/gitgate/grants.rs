//! The grants on record: reading them without writing over what could not be read, recording
//! one, revoking one, and the live grants a box holds.

use super::*;

// ───────────────────────────── the grant record ─────────────────────────────

/// Every grant on record, expired ones included — the cockpit shows those too, because "this box had
/// access until Tuesday" is the answer to a question the list exists to answer.
///
/// A file that will not parse reads as no grants, and that is the safe direction *for this read*:
/// [`access`] denies, the token refresher places nothing, and a box is locked out rather than let
/// in. It is said out loud once, because "no grants" and "skein cannot read your grants" look
/// identical on the screen and only one of them is fixable. The writes below refuse outright —
/// [`record`] and [`revoke`] go through [`crate::util::update_json`], which will not write a
/// default over a file it could not read.
pub fn grants() -> Vec<Grant> {
    match crate::util::read_json_or_why::<Vec<Grant>>(&grants_path()) {
        Ok(found) => found.unwrap_or_default(),
        Err(why) => {
            // Once per process, for `config::load_config`'s reason: this is on the path of the
            // token refresher and of every cockpit poll, and a line per call buries the one line
            // that matters under thousands of copies of itself.
            static TOLD: std::sync::Once = std::sync::Once::new();
            TOLD.call_once(|| {
                eprintln!(
                    "skein: cannot read your GitHub grants ({why}) — every box will be refused \
                     write access until that file parses, and skein will refuse to write over it. \
                     Fix or move the file."
                );
            });
            Vec::new()
        }
    }
}

/// Add a grant, replacing any earlier one for the same box and repository.
///
/// Replacing rather than appending: re-approving after an expiry is the common case, and two records
/// for one pair would leave [`access`] answering from whichever happened to be first.
pub fn merged(mut all: Vec<Grant>, next: Grant) -> Vec<Grant> {
    all.retain(|g| !(g.box_name == next.box_name && same_repo(&g.repo, &next.repo)));
    all.push(next);
    all.sort_by(|a, b| a.box_name.cmp(&b.box_name).then(a.repo.cmp(&b.repo)));
    all
}

pub(super) fn record(req: &Request, hours: Option<i64>) -> Result<(), String> {
    let now = chrono::Utc::now();
    let grant = Grant {
        box_name: req.box_name.clone(),
        repo: req.repo.clone(),
        granted: now.to_rfc3339(),
        expires: match hours {
            // A grant with no hours is permanent, and says so by holding no date at all.
            None => String::new(),
            Some(h) => (now + chrono::Duration::hours(h)).to_rfc3339(),
        },
    };
    // Under the lock, with the existing grants read inside it: `merged` replaces the entry for one
    // (box, repo) pair and keeps the rest, so a stale read here would drop whatever grant another
    // approval had just recorded. And `update_json` refuses on a grants file it cannot read rather
    // than writing this one grant over it — approving one box is not how a fleet loses the access
    // every other box was given (SKEIN-359).
    crate::util::update_json(&grants_path(), |all: &mut Vec<Grant>| {
        *all = merged(std::mem::take(all), grant);
        Ok(())
    })
}

/// Withdraw a grant early. The token file is removed by the next refresh, and until then the grant
/// is already gone from [`access`] — so a revoke is effective the moment it is recorded.
///
/// Refuses on a grants file it cannot read, like [`record`]. A revoke is *already* the fail-closed
/// direction, so answering Ok on a file that was never read would be the one shape that is worse
/// than either: every other grant destroyed, and a person told the withdrawal they asked for is the
/// only thing that happened.
pub fn revoke(box_name: &str, repo: &str) -> Result<(), String> {
    crate::util::update_json(&grants_path(), |all: &mut Vec<Grant>| {
        all.retain(|g| !(g.box_name == box_name && same_repo(&g.repo, repo)));
        Ok(())
    })
}

/// The live grants for one box, which is what the token refresher acts on.
pub fn live_grants_for(box_name: &str, now: chrono::DateTime<chrono::Utc>) -> Vec<Grant> {
    grants()
        .into_iter()
        .filter(|g| g.box_name == box_name && g.is_live(now))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gitgate::testkit::*;

    /// **Approving one box's access is not how a fleet loses every grant it has given.**
    ///
    /// The same shape one file over (SKEIN-359): `record` replaces the entry for one (box, repo)
    /// pair and writes the whole list back, so a grants file read as empty turned an approval into
    /// a mass revocation — and answered Ok, so the cockpit showed the approval as recorded. A
    /// revoke is the same write and the same loss, which is why both are asserted here: a revoke
    /// already fails closed, and answering Ok for one that destroyed the rest is the shape nobody
    /// would go looking for.
    #[test]
    fn a_grant_list_skein_cannot_read_is_never_written_over_by_an_approval() {
        let (_lock, _home, _env) = fresh_home();
        let asked = |box_name: &str, repo: &str| Request {
            id: "20260826-101010-1".into(),
            box_name: box_name.into(),
            repo: repo.into(),
            reason: "to push".into(),
            asked: chrono::Utc::now().to_rfc3339(),
            state: "pending".into(),
            decided: String::new(),
        };
        record(&asked("web-main", "acme/web"), Some(4)).unwrap();
        record(&asked("api-main", "acme/api"), None).unwrap();
        assert_eq!(grants().len(), 2);

        let path = grants_path();
        std::fs::write(&path, b"").unwrap();
        assert!(
            grants().is_empty(),
            "an unreadable grants file must read as no access, never as access"
        );

        let why = record(&asked("web-main", "acme/third"), Some(4))
            .expect_err("an approval over an unreadable grants file reported success");
        assert!(
            why.contains("cannot read") && why.contains("grants"),
            "the refusal has to name the file and say it could not be read: {why}"
        );
        assert_eq!(
            std::fs::read(&path).unwrap(),
            b"",
            "the unreadable grants file was replaced by an approval"
        );

        assert!(
            revoke("web-main", "acme/web").is_err(),
            "withdrawing one grant is not how the others are withdrawn"
        );
        assert_eq!(
            std::fs::read(&path).unwrap(),
            b"",
            "the unreadable grants file was replaced by a revoke"
        );
    }
}
