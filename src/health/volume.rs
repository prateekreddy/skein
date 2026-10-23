//! Two disk questions the fleet's own filesystems cannot answer (SKEIN-1286), reported on the same
//! "fleet disk" row as theirs, because that is where a person looks for disk.
//!
//! * **The disk skein's volume is on.** `$SKEIN_HOME` is mounted into the sandbox from the
//!   computer running it, so `df` on it reads that computer's disk — which the sandbox's own two
//!   filesystems say nothing about, and which held skein's whole volume when it was found 88% full
//!   with no check anywhere.
//! * **A box over its own allowance.** The allowance is accounting rather than a quota
//!   (`fleet::box_disk_limit`), so nothing stops a box crossing it; until this, nothing on the
//!   health report said so either.

use super::disk::{gib, DISK_FULL_PCT};
use super::*;

/// One `df -Pm` line: MiB total, used and available, and where it was asked about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct VolumeDisk {
    pub path: String,
    pub total: u64,
    pub used: u64,
    /// `df`'s own Available column — not `total - used`, which counts blocks the filesystem keeps
    /// back from ordinary writers.
    pub avail: u64,
}

/// `df -Pm` output into a [`VolumeDisk`], or `None` when it carried no second line of numbers.
pub(crate) fn parse_df(path: &str, out: &str) -> Option<VolumeDisk> {
    let line = out.lines().nth(1)?;
    let mut cols = line.split_whitespace().skip(1);
    let mut next = || cols.next()?.parse::<u64>().ok();
    let (total, used, avail) = (next()?, next()?, next()?);
    (total > 0).then(|| VolumeDisk {
        path: path.to_string(),
        total,
        used,
        avail,
    })
}

/// The disk holding skein's volume, read where this process runs. `None` when `df` did not answer.
pub(crate) fn volume_disk() -> Option<VolumeDisk> {
    let home = crate::config::skein_home();
    let path = home.to_string_lossy();
    let (out, _, code) = crate::util::run_capture("df", &["-Pm", &path]).ok()?;
    if code != 0 {
        return None;
    }
    parse_df(&path, &out)
}

/// Every box holding more than its own allowance, as `(name, MiB held, MiB allowed)`, most over
/// first. A box with no allowance is never over one.
pub(crate) fn over_allowance(
    usage: &std::collections::HashMap<String, u64>,
    limit: impl Fn(&str) -> Option<u64>,
) -> Vec<(String, u64, u64)> {
    let mut over: Vec<(String, u64, u64)> = usage
        .iter()
        .filter_map(|(name, &held)| {
            let allowed = limit(name)?;
            (held > allowed).then(|| (name.clone(), held, allowed))
        })
        .collect();
    over.sort_by(|a, b| (b.1 - b.2).cmp(&(a.1 - a.2)).then(a.0.cmp(&b.0)));
    over
}

/// The fleet's disk verdict with these two readings added to it.
///
/// Either one past its line makes the row a fault, whatever the fleet's own filesystems said; and
/// what clears them goes **before** the fleet's own fix, because that one can end in a command on a
/// line of its own, and anything joined after it reads as part of the command (`disk_verdict`).
/// Whether the fleet's fix destroys something is kept as it was: nothing added here does.
pub(crate) fn with_volume_and_allowances(
    fleet: HealthCheck,
    volume: Option<VolumeDisk>,
    over: &[(String, u64, u64)],
) -> HealthCheck {
    let mut details: Vec<String> = vec![fleet.detail.clone()];
    let mut fixes: Vec<String> = Vec::new();
    if let Some(v) = &volume {
        let pct = v.used * 100 / v.total;
        details.push(format!(
            "the disk holding skein's volume ({}) is {}% full ({} of {}, {} available)",
            v.path,
            pct,
            gib(v.used),
            gib(v.total),
            gib(v.avail)
        ));
        if pct >= DISK_FULL_PCT {
            fixes.push(format!(
                "free space on the disk that holds {} — `du -sh {}/*` shows what skein itself \
                 keeps there",
                v.path, v.path
            ));
        }
    }
    if !over.is_empty() {
        let named = over
            .iter()
            .take(3)
            .map(|(name, held, allowed)| format!("{name} ({} of {})", gib(*held), gib(*allowed)))
            .collect::<Vec<_>>()
            .join(", ");
        let more = match over.len() {
            n if n > 3 => format!(" and {} more", n - 3),
            _ => String::new(),
        };
        details.push(format!(
            "{} box(es) hold more than their own disk allowance: {named}{more}",
            over.len()
        ));
        fixes.push(
            "clear that box's build output, or raise its allowance in the box's ⚙ Box settings"
                .to_string(),
        );
    }
    let detail = details.join("; ");
    if fixes.is_empty() {
        return HealthCheck { detail, ..fleet };
    }
    let fix = match fleet.fix.is_empty() {
        true => fixes.join("; "),
        false => format!("{}\n{}", fixes.join("; "), fleet.fix),
    };
    let check = HealthCheck::unsatisfied(detail, fix);
    match fleet.destructive {
        true => check.destroys(),
        false => check,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `df -Pm` as GNU coreutils prints it, and the Available column is the fourth.
    #[test]
    fn df_is_read_by_its_available_column() {
        let out = "Filesystem     1048576-blocks   Used Available Capacity Mounted on\n\
                   /dev/disk3s5          948000 828000    120000      88% /v\n";
        assert_eq!(
            parse_df("/v/.skein", out),
            Some(VolumeDisk {
                path: "/v/.skein".into(),
                total: 948000,
                used: 828000,
                avail: 120000
            })
        );
        assert_eq!(parse_df("/x", "Filesystem 1048576-blocks\n"), None);
        assert_eq!(parse_df("/x", ""), None);
    }

    /// Who is over, measured against each box's own allowance, most over first. What would make it
    /// fail: comparing against the fleet's line instead, or reporting a box with no allowance.
    #[test]
    fn a_box_over_its_own_allowance_is_named_and_one_with_none_never_is() {
        let usage: std::collections::HashMap<String, u64> = [
            ("a".to_string(), 10_517),
            ("b".to_string(), 9_000),
            ("c".to_string(), 50_000),
            ("d".to_string(), 12_000),
        ]
        .into();
        let limit = |name: &str| (name != "c").then_some(10_240);
        assert_eq!(
            over_allowance(&usage, limit),
            [
                ("d".to_string(), 12_000, 10_240),
                ("a".to_string(), 10_517, 10_240)
            ]
        );
    }

    /// The two readings reach the "fleet disk" row, and either one past its line makes it a fault
    /// with a fix — placed before the fleet's own, whose last line can be a command.
    ///
    /// What would make it fail: dropping either reading from the detail, leaving the row satisfied
    /// when the volume's disk is past [`DISK_FULL_PCT`] or a box is over, or appending after the
    /// fleet's fix.
    #[test]
    fn the_volumes_disk_and_a_box_over_its_allowance_reach_the_disk_row() {
        let roomy = VolumeDisk {
            path: "/v".into(),
            total: 1000,
            used: 500,
            avail: 480,
        };
        let full = VolumeDisk {
            used: 880,
            avail: 100,
            ..roomy.clone()
        };
        let fine = || HealthCheck::satisfied("the boxes' disk is 33% full");

        let quiet = with_volume_and_allowances(fine(), Some(roomy), &[]);
        assert_eq!(quiet.level, Level::Satisfied);
        assert!(
            quiet
                .detail
                .contains("the disk holding skein's volume (/v) is 50% full"),
            "{}",
            quiet.detail
        );
        assert!(quiet.detail.contains("available"), "{}", quiet.detail);

        let host = with_volume_and_allowances(fine(), Some(full.clone()), &[]);
        assert_eq!(host.level, Level::Unsatisfied, "{}", host.detail);
        assert!(host.fix.contains("du -sh /v/*"), "{}", host.fix);

        let over = [("a".to_string(), 10_517_u64, 10_240_u64)];
        let boxed = with_volume_and_allowances(fine(), None, &over);
        assert_eq!(boxed.level, Level::Unsatisfied);
        assert!(
            boxed
                .detail
                .contains("1 box(es) hold more than their own disk allowance: a (10.3G of 10.0G)"),
            "{}",
            boxed.detail
        );

        let fleet_full =
            HealthCheck::unsatisfied("the image store is 90% full", "prune it\nrm -rf x")
                .destroys();
        let both = with_volume_and_allowances(fleet_full, Some(full), &over);
        assert!(both.destructive, "the fleet's own fix still destroys");
        assert!(
            both.fix.ends_with("prune it\nrm -rf x"),
            "the fleet's fix stays last, so nothing is read as an argument to its command: {}",
            both.fix
        );
    }
}
