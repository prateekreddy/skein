//! A repo's CI build output, which counts against the repo and not against the box that runs its CI
//! (owner, 2026-11-06, SKEIN-1476).
//!
//! synapse-master builds every pull request's CI train, and synapse's own tooling keeps that build
//! in `~/.cache/synapse-target-ci`, 6.4 GB measured. Charged to the box, it kept the box over its
//! 12 GB allowance, and the only way to clear it cost the next run a 20-minute cold build. So the
//! repo declares where its CI keeps its build, and skein charges it the way it already charges the
//! repo's shared download cache: to no box, on a line of its own.
//!
//! Three rules, each argued where it is applied:
//!
//!   * **Declared on skein's side, by the owner.** [`ci_builds_path`] is under `$SKEIN_HOME`, which
//!     no box can write, as `declared/` is (`declared.rs`). A file inside the repo would let any box
//!     of it re-charge its own build output by editing a file in its checkout. There is no request
//!     path for a box: asking to be charged less is asking for more disk, and `skein_request_disk`
//!     already carries that ask to the owner.
//!   * **Bounded by a cap.** What is charged to no box is asked of nobody by the fleet's share
//!     either (`announce::cover` folds over what each box is charged), so something has to bound
//!     it. The repo's cap, or by default one box's default allowance (`box_disk_max`): a repo's CI
//!     may keep as much as one box may. With neither a size, nothing is exempt.
//!   * **Over the cap, everything is the box's again**, exactly as an undeclared directory is: it
//!     is charged in full and listed as the build output it is, so the box is asked as it is today.
//!     Not the excess alone: a target directory cannot be trimmed to a size, so an ask for the part
//!     over the cap could only be met by removing the whole of it anyway.

use super::*;
use std::path::{Path, PathBuf};

/// The kind a declared directory is listed under while the repo's cap covers it. It is still on
/// the box's clean list, so the agent can see it and remove it, but nothing counts it as the box's.
pub const CI_OUTPUT_KIND: &str = "repo CI output";

/// The most directories one repo may declare. A handful is what a CI needs; a long list is a typo
/// or an attempt to exempt a whole home.
const MAX_DIRS: usize = 8;

/// What the owner has said about one repo's CI build output.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CiBuilds {
    /// Directories relative to a box's home, as `.cache/synapse-target-ci`.
    #[serde(default)]
    pub dirs: Vec<String>,
    /// The cap on what all of the repo's boxes hold in them together, as `8g`. `None` takes the
    /// fleet's default box allowance ([`default_cap_mib`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cap: Option<String>,
}

/// Where repo `id`'s CI build settings are kept: beside its per-pull-request triggers, under
/// `$SKEIN_HOME`, which the boxes cannot write. Its own file rather than a field of the repo
/// record, so it can be read and written without the whole-list lock `repos.json` needs.
pub fn ci_builds_path(id: &str) -> PathBuf {
    skein_home().join("repos").join(id).join("ci-builds.json")
}

/// Repo `id`'s CI build settings. An unreadable file reads as none declared, which charges every
/// directory to its box, as before: the direction that never lets a box hold more unasked.
pub fn ci_builds(id: &str) -> CiBuilds {
    if !valid_name(id) {
        return CiBuilds::default();
    }
    crate::util::read_json_or_why(&ci_builds_path(id))
        .ok()
        .flatten()
        .unwrap_or_default()
}

/// The cap a repo gets when it names none: the fleet's default box allowance, `None` when that is
/// unlimited.
pub fn default_cap_mib() -> Option<u64> {
    parse_mib(&load_config().box_disk_max)
}

/// A declared directory, checked: relative, plain components only, and not the home itself.
fn checked_dir(raw: &str) -> Result<String, String> {
    let rel = raw.trim().trim_end_matches('/');
    let rel = rel.strip_prefix("~/").unwrap_or(rel);
    if rel.is_empty() || rel.starts_with('/') {
        return Err(format!(
            "{raw:?} is not a directory under a box's home: write it relative to the home, as \
             .cache/synapse-target-ci"
        ));
    }
    let plain = rel
        .split('/')
        .all(|part| !part.is_empty() && part != "." && part != ".." && !part.contains('\0'));
    if !plain {
        return Err(format!(
            "{raw:?} has an empty, `.` or `..` part, so it could name something other than a \
             directory under the home"
        ));
    }
    Ok(rel.to_string())
}

/// Set repo `id`'s CI build directories and cap. The owner's call, through the cockpit's API; no
/// box reaches it. An empty list with no cap forgets the setting.
pub fn set_ci_builds(id: &str, dirs: &[String], cap: Option<&str>) -> Result<CiBuilds, String> {
    if !load_repos().iter().any(|r| r.id == id) {
        return Err(format!("{id} is not a registered repo"));
    }
    if dirs.len() > MAX_DIRS {
        return Err(format!(
            "{} directories is more than a CI keeps its build in; at most {MAX_DIRS}",
            dirs.len()
        ));
    }
    let mut checked: Vec<String> = Vec::new();
    for d in dirs {
        let d = checked_dir(d)?;
        if !checked.contains(&d) {
            checked.push(d);
        }
    }
    let cap = match cap.map(str::trim).filter(|c| !c.is_empty()) {
        None => None,
        Some(c) => match parse_mib(c) {
            Some(mib) if mib > 0 => Some(c.to_string()),
            _ => {
                return Err(format!(
                    "{c:?} is not a size: try 8g or 512m. Leave it empty for the fleet's default \
                     box allowance"
                ))
            }
        },
    };
    let path = ci_builds_path(id);
    if checked.is_empty() && cap.is_none() {
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.to_string()),
        }
        return Ok(CiBuilds::default());
    }
    let set = CiBuilds { dirs: checked, cap };
    let dir = path
        .parent()
        .ok_or("no directory for the CI build settings")?;
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let body = serde_json::to_vec_pretty(&set).map_err(|e| e.to_string())?;
    crate::util::write_atomic(&path, dir, &body)?;
    Ok(set)
}

/// One declared directory as it is found in one box.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct CiDir {
    #[serde(rename = "box")]
    pub box_name: String,
    /// Absolute, as the fleet sees it.
    pub path: String,
    /// `~/…`, as the box's agent sees it.
    pub shown: String,
    /// What removing it frees (`freed_bytes`), the figure taken off the box's charge.
    pub bytes: u64,
}

/// A repo's CI build output across its boxes, measured, and whether its cap covers it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct RepoCiOutput {
    pub repo: String,
    /// What is declared, as it is stored.
    pub declared: CiBuilds,
    /// The cap in force, in MiB: the repo's own, or the default. `None` is no bound, and then
    /// nothing is exempt.
    pub cap_mib: Option<u64>,
    /// Every declared directory found, biggest first.
    pub dirs: Vec<CiDir>,
    /// What they hold together, in bytes.
    pub bytes: u64,
    /// The cap covers them, so they are charged to no box. Otherwise each is its box's again.
    pub exempt: bool,
}

/// Repo `id`'s CI build output, measured in each of its boxes. Reading only.
pub fn repo_ci_output(id: &str) -> RepoCiOutput {
    let declared = ci_builds(id);
    let cap_mib = match &declared.cap {
        Some(c) => parse_mib(c),
        None => default_cap_mib(),
    };
    let mut dirs = Vec::new();
    if !declared.dirs.is_empty() {
        for name in boxes_of_repo(id) {
            let box_dir = Path::new(&fleet_root()).join(&name);
            use std::os::unix::fs::MetadataExt;
            let Ok(meta) = std::fs::symlink_metadata(&box_dir) else {
                continue;
            };
            let dev = meta.dev();
            for rel in &declared.dirs {
                // A stored value is checked again: the file is skein's, but a hand edit of it must
                // not name anything outside the home either.
                let Ok(rel) = checked_dir(rel) else {
                    continue;
                };
                let path = box_dir.join("home").join(&rel);
                if !real_all_the_way(&box_dir, &path, dev) || !is_real_dir(&path, dev) {
                    continue;
                }
                dirs.push(CiDir {
                    box_name: name.clone(),
                    shown: format!("~/{rel}"),
                    bytes: freed_bytes(&path, Some(dev)),
                    path: path.display().to_string(),
                });
            }
        }
    }
    dirs.sort_by(|a, b| b.bytes.cmp(&a.bytes).then(a.path.cmp(&b.path)));
    let bytes = dirs.iter().map(|d| d.bytes).sum();
    let exempt = !dirs.is_empty() && cap_mib.is_some_and(|cap| bytes <= cap * 1024 * 1024);
    RepoCiOutput {
        repo: id.to_string(),
        declared,
        cap_mib,
        dirs,
        bytes,
        exempt,
    }
}

/// The declared directories in box `name` that its repo's cap covers, so the box is not charged
/// for them. Empty when the box has no repo, declares nothing, or is over the cap.
pub fn exempt_ci_dirs(name: &str) -> Vec<CiDir> {
    let Some(repo) = repo_for_box(name) else {
        return Vec::new();
    };
    let out = repo_ci_output(&repo.id);
    if !out.exempt {
        return Vec::new();
    }
    out.dirs
        .into_iter()
        .filter(|d| d.box_name == name)
        .collect()
}

/// Take each box's exempt CI output off what it is charged (`usage`, MiB per box, as
/// `local_disk_usage` walks it). Every consumer of the per-box figure, the box's allowance, the
/// fleet's share, the board and the box's own signal file, reads it after this, so a box is
/// charged one figure everywhere.
///
/// Rounded so that the box is never charged less than it holds outside the exempt directories:
/// the box's figure is rounded up, so what comes off it is rounded down.
pub(super) fn charge_ci_builds(usage: &mut std::collections::HashMap<String, u64>) {
    const MIB: u64 = 1024 * 1024;
    for repo in load_repos() {
        let out = repo_ci_output(&repo.id);
        if !out.exempt {
            continue;
        }
        for d in out.dirs {
            if let Some(mb) = usage.get_mut(&d.box_name) {
                *mb = mb.saturating_sub(d.bytes / MIB);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::*;

    const MIB: u64 = 1024 * 1024;

    /// Box `synapse-master` of repo `synapse`, holding 3 MiB of CI build output in
    /// `~/.cache/synapse-target-ci` (a cargo target: `.rustc_info.json` is what the build walk
    /// knows one by) and 1 MiB of its own work in its checkout. Both variables pinned at the
    /// fixture, so nothing here reads the live fleet. Returns (fleet root, pins).
    fn planted(dir: &TempDir) -> (std::path::PathBuf, EnvPins) {
        let base: &std::path::Path = dir.as_ref();
        let root = base.join("fleet");
        let home = base.join("skein");
        let mut pins = env_pins();
        pins.set("SKEIN_FLEET_ROOT", &root);
        pins.set("SKEIN_HOME", &home);
        let store = home.join("repos/synapse/store/data");
        std::fs::create_dir_all(&store).unwrap();
        crate::repos::save_repos(&[crate::repos::Repo {
            id: "synapse".into(),
            store: store.display().to_string(),
            ..Default::default()
        }])
        .unwrap();
        let b = root.join("synapse-master");
        let ci = b.join("home/.cache/synapse-target-ci");
        std::fs::create_dir_all(ci.join("debug/deps")).unwrap();
        std::fs::write(ci.join(".rustc_info.json"), "{}").unwrap();
        std::fs::write(ci.join("debug/deps/enclave"), vec![1u8; 3 * MIB as usize]).unwrap();
        std::fs::create_dir_all(b.join("tree")).unwrap();
        std::fs::write(b.join("tree/work.bin"), vec![2u8; MIB as usize]).unwrap();
        (root, pins)
    }

    fn charged() -> u64 {
        fleet_disk_usage()["synapse-master"]
    }

    fn kind_of(rel: &str) -> Option<&'static str> {
        box_cleanables("synapse-master")
            .unwrap()
            .into_iter()
            .find(|c| c.path.ends_with(rel))
            .map(|c| c.kind)
    }

    /// **A declared directory is out of the box's total and listed on its own, as the repo's**;
    /// and the same directory undeclared counts against the box as before, as build output.
    ///
    /// What would make it fail: `fleet_disk_usage` not calling `charge_ci_builds` (the declared
    /// figure is then the undeclared one, and the first `assert!` fails), or `box_cleanables` not
    /// listing the exempt directory under its own kind (it is then `build output`). Watched: with
    /// the `charge_ci_builds` call removed from `fleet_disk_usage`, the first assertion failed with
    /// 5 not below 3; with the exempt directories left out of `box_cleanables`, the kind assertion
    /// failed with `Some("build output")`.
    #[test]
    fn a_declared_ci_dir_is_charged_to_no_box_and_an_undeclared_one_is() {
        let _lock = env_lock();
        let dir = tempdir();
        let (_root, _pins) = planted(&dir);

        let before = charged();
        assert!(
            before >= 4,
            "the box holds its work and the CI build: {before}"
        );
        assert_eq!(kind_of("synapse-target-ci"), Some("build output"));

        set_ci_builds("synapse", &[".cache/synapse-target-ci".into()], Some("8g")).unwrap();
        let after = charged();
        assert!(
            after + 3 <= before && after < 3,
            "the 3 MiB CI build still counts against the box: {after} of {before}"
        );
        assert_eq!(kind_of("synapse-target-ci"), Some(CI_OUTPUT_KIND));
        let out = repo_ci_output("synapse");
        assert!(out.exempt, "{out:?}");
        assert_eq!(out.dirs.len(), 1, "{out:?}");
        assert_eq!(out.dirs[0].shown, "~/.cache/synapse-target-ci");
        assert!(out.dirs[0].bytes >= 3 * MIB, "{out:?}");

        // Undeclared again: charged as before.
        set_ci_builds("synapse", &[], None).unwrap();
        assert_eq!(charged(), before);
        assert_eq!(kind_of("synapse-target-ci"), Some("build output"));
    }

    /// **Over the cap, the directory is the box's again and the box is asked as it is today.** The
    /// box's allowance is 4 MiB: with the cap covering the CI build it holds 1 MiB and is asked
    /// nothing; with the cap below it, it is charged everything and the allowance's ask is made.
    ///
    /// What would make it fail: ignoring the cap (`exempt` true whatever the size: the box is then
    /// never asked), or charging only the excess (the box then holds about 1 + 1 MiB, under its
    /// allowance, and is not asked). Watched: with `exempt` computed as `!dirs.is_empty()` alone,
    /// the over-cap `asks` came back empty ("2 MiB of 4"); with `charge_ci_builds` taking off up to
    /// the cap from a box over it, the same ("3 MiB of 4").
    #[test]
    fn over_its_cap_ci_output_is_the_boxs_again_and_the_box_is_asked() {
        let _lock = env_lock();
        let dir = tempdir();
        let (_root, _pins) = planted(&dir);
        let reading = |mb: u64| crate::resource_signal::Reading {
            at: chrono::Utc::now(),
            disk_mb: Some(mb),
            disk_limit_mb: Some(4),
            fleet: crate::resource_signal::FleetDisk::NotAsked { over_by: 0 },
            cgroup: None,
            clean: None,
        };
        let asked = |mb: u64| {
            crate::resource_signal::next(None, &reading(mb))
                .asks
                .iter()
                .any(|a| a.kind == "disk")
        };

        set_ci_builds("synapse", &[".cache/synapse-target-ci".into()], Some("8m")).unwrap();
        let covered = charged();
        assert!(
            !asked(covered),
            "under the cap the box is asked nothing ({covered} MiB)"
        );

        set_ci_builds("synapse", &[".cache/synapse-target-ci".into()], Some("2m")).unwrap();
        let over = charged();
        assert!(
            asked(over),
            "over the cap the box is asked ({over} MiB of 4)"
        );
        assert!(
            over >= covered + 3,
            "over the cap it is charged in full: {over}"
        );
        assert!(!repo_ci_output("synapse").exempt);
        assert_eq!(kind_of("synapse-target-ci"), Some("build output"));
    }

    /// **The default cap is the fleet's default box allowance, and no allowance is no exemption.**
    ///
    /// What would make it fail: a default that is not `box_disk_max` (the 3 MiB build is then
    /// exempt under a 2m allowance), or an unlimited default read as an unlimited exemption.
    /// Watched: with `default_cap_mib` returning `Some(u64::MAX)`, the 2m case read exempt.
    #[test]
    fn with_no_cap_set_the_default_box_allowance_bounds_it() {
        let _lock = env_lock();
        let dir = tempdir();
        let (_root, _pins) = planted(&dir);
        set_ci_builds("synapse", &[".cache/synapse-target-ci".into()], None).unwrap();
        let with_default = |max: &str| {
            let mut c = crate::config::load_config();
            c.box_disk_max = max.into();
            crate::config::save_config(&c).unwrap();
            repo_ci_output("synapse").exempt
        };
        assert!(with_default("8m"));
        assert!(!with_default("2m"));
        assert!(!with_default("none"));
    }

    /// **Only a plain directory under the home can be declared.**
    ///
    /// What would make it fail: dropping the component check in `checked_dir` (`..` then passes).
    /// Watched: with `plain` forced true, `../../other-box/home` was stored.
    #[test]
    fn a_declared_dir_cannot_leave_the_home() {
        let _lock = env_lock();
        let dir = tempdir();
        let (_root, _pins) = planted(&dir);
        for bad in ["/etc", "", "../../other-box/home", ".cache/../..", "./x"] {
            assert!(
                set_ci_builds("synapse", &[bad.into()], None).is_err(),
                "{bad:?} was accepted"
            );
        }
        assert!(set_ci_builds("nope", &[".cache/x".into()], None).is_err());
        let set = set_ci_builds("synapse", &["~/.cache/x/".into()], Some("4g")).unwrap();
        assert_eq!(set.dirs, vec![".cache/x".to_string()]);
        assert_eq!(ci_builds("synapse"), set);
    }
}
