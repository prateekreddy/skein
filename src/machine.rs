//! What else is on this machine — asked when somebody wants to know, and about the machine rather
//! than about a box.
//!
//! # Why this is not "foreign boxes"
//!
//! When the sandbox listing left the board's tick, the rows for sandboxes skein did not place were
//! kept alive as `BoxView`s fetched on demand. That was right at that moment — without it the
//! `foreign:` filter would have become a silently empty list, and a silent regression is worse than
//! either keeping or removing a feature deliberately — but it is not what the call is for.
//!
//! `docs/parity.md` §7 lists **foreign sandbox display** as a deliberate removal: "That feature
//! mitigated skein listing every sandbox on the host; the rewrite does not list sandboxes, so the
//! confusion cannot arise." What survives is a different question, asked by a person: *what fleets
//! are on this machine* — because somebody running more than one needs to see them. Same `sbx ls`
//! call; different subject, and only one of the two survives.
//!
//! So a sandbox is reported as a **sandbox**: it has a name and a run state, and it is or is not one
//! of skein's fleets. It is not a box with an empty branch and no signals, which is what made the
//! old rows read as a fleet full of broken ones.
//!
//! # On demand, and that is the whole point
//!
//! No gate and no tick. A person asking what is on their machine is asking *now*, and handing them a
//! remembered answer would be answering a different question — most sharply right after they
//! destroyed something.

use serde::Serialize;

/// One sandbox on this machine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Sandbox {
    pub name: String,
    /// `None` when `sbx` reported a run state this skein does not recognise. Not `false`: "stopped"
    /// and "I could not tell" send a person to different places.
    pub running: Option<bool>,
    /// Is this a skein fleet? True for the one this skein is configured with, and for any sandbox a
    /// placement record names — which is how a *second* skein's fleet is recognised, and is the
    /// question somebody running more than one is actually asking.
    pub skein_fleet: bool,
    /// Is it *this* skein's fleet? The distinction matters: one of these is yours to act on and the
    /// rest are somebody else's, possibly another person's session on a shared host.
    pub ours: bool,
}

/// Every sandbox `sbx` knows about, said as sandboxes.
///
/// `Err` rather than an empty list when `sbx` cannot be asked. "Nothing else is here" and "I could
/// not be told" are different answers and a caller renders them differently — collapsing them is how
/// a machine with three fleets on it reports as empty.
pub fn sandboxes() -> Result<Vec<Sandbox>, String> {
    let ours = crate::place::fleet_sandbox();
    let listed = crate::sbx::fleet_boxes().ok_or_else(|| {
        crate::sbx::fleet_failure().unwrap_or_else(|| "sbx did not answer".to_string())
    })?;
    // Every sandbox skein has ever placed a box into, which is what makes another skein's fleet
    // recognisable rather than merely present.
    let placed: std::collections::HashSet<String> = crate::place::placed_sandboxes();
    Ok(listed
        .into_iter()
        .map(|b| Sandbox {
            running: match b.live {
                Some(crate::sbx::Liveness::Running) => Some(true),
                Some(crate::sbx::Liveness::Stopped) => Some(false),
                None => None,
            },
            skein_fleet: b.name == ours || placed.contains(&b.name),
            ours: b.name == ours,
            name: b.name,
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::*;

    /// A sandbox is reported as a sandbox, and the two questions it answers are separate.
    #[test]
    fn a_second_fleet_is_recognised_and_is_not_mistaken_for_ours() {
        let _g = env_lock();
        let home = tempdir();
        std::env::set_var("SKEIN_HOME", &home);
        std::env::set_var(
            "SKEIN_LS_CMD",
            r#"printf '[{"name":"skein-fleet","status":"running"},{"name":"other-fleet","status":"stopped"},{"name":"someone-elses","status":"running"}]'"#,
        );
        let mut config = crate::config::load_config();
        config.fleet_sandbox = "skein-fleet".into();
        crate::config::save_config(&config).unwrap();
        // A box placed in another sandbox: that is what makes it recognisable as a skein fleet
        // rather than merely as something running.
        crate::place::record_place(
            "a-box",
            &crate::place::PlaceRecord {
                sandbox: "other-fleet".into(),
                ns_pid: 1,
                home: "/boxes/a-box/home".into(),
                tree: "/boxes/a-box/tree".into(),
                sock: "/boxes/a-box/session.sock".into(),
                generation: String::new(),
                ns_start: 0,
                ..Default::default()
            },
        )
        .unwrap();

        let seen = sandboxes().expect("sbx answers here");
        std::env::remove_var("SKEIN_LS_CMD");
        let by = |name: &str| seen.iter().find(|s| s.name == name).cloned().unwrap();

        let ours = by("skein-fleet");
        assert!(ours.ours && ours.skein_fleet);
        assert_eq!(ours.running, Some(true));

        // The question somebody running more than one is asking.
        let other = by("other-fleet");
        assert!(other.skein_fleet, "a second skein fleet was not recognised");
        assert!(!other.ours, "another fleet was reported as this one");
        assert_eq!(other.running, Some(false), "stopped is not unknown");

        // And something that is simply a sandbox stays one — no branch, no signals, no pretence.
        let stranger = by("someone-elses");
        assert!(!stranger.skein_fleet && !stranger.ours);
    }

    /// "Nothing else is here" and "I could not be told" are different answers.
    #[test]
    fn a_machine_that_cannot_be_asked_is_not_an_empty_one() {
        let _g = env_lock();
        let home = tempdir();
        std::env::set_var("SKEIN_HOME", &home);
        std::env::set_var("SKEIN_LS_CMD", "false");
        let refused = sandboxes();
        std::env::set_var("SKEIN_LS_CMD", "printf '[]'");
        let empty = sandboxes();
        std::env::remove_var("SKEIN_LS_CMD");

        assert!(
            refused.is_err(),
            "a machine that could not be asked reported as empty"
        );
        assert_eq!(
            empty.unwrap(),
            Vec::new(),
            "an empty machine is a real answer"
        );
    }
}
