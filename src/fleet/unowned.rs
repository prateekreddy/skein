//! What no box owns: running containers that carry no box's label, or a gone box's, and the
//! cgroups of boxes that no longer exist.
//!
//! **The containers are reported and never touched.** skein did not start them, or started them
//! for a box that is gone, and it cannot tell a forgotten database from one somebody is using.
//! The owner decides (box-plugin.md, question 6): the report offers `docker stop` and
//! `docker rm -f` as commands to copy, and nothing here runs either. The only docker verbs this
//! file sends are `ps` and `inspect`, which
//! `the_reconciler_asks_docker_questions_and_never_stops_or_removes_anything` holds it to.
//!
//! **A gone box's cgroup is removed only once it is empty.** An empty cgroup costs nothing and is
//! safe to remove. If a box is later given the same name, it would otherwise start under the old
//! box's limits. A cgroup that still holds processes is never touched. It goes on the report,
//! with the command that shows what is in it and the one that would end it, both for a person to
//! run.
//!
//! One pass every [`RECONCILE_EVERY`], on the server's own loop ([`watch_unowned`]). The health
//! report only reads what the last pass found ([`unowned_report`]), because it is polled and a
//! polled endpoint must not spawn `docker`.

use super::*;
use serde::Serialize;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// How often the pass runs. A leftover container or cgroup is a slow leak and not an emergency, so
/// ten minutes is soon enough. `docker inspect --size` walks each container's writable layer, which
/// is not something to do every few seconds.
pub const RECONCILE_EVERY: Duration = Duration::from_secs(10 * 60);

/// One running container, as `docker inspect` describes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Container {
    /// Its name, without docker's leading `/`.
    pub name: String,
    /// The [`CONTAINER_LABEL`] it carries, or `None` for none (or an empty one).
    pub label: Option<String>,
    /// Its init process, which is how its cgroup, and so its memory, is found.
    pub pid: u32,
    /// Its writable layer (`SizeRw`), in bytes: what removing it would free on disk.
    pub disk_bytes: u64,
}

/// A running container that belongs to no box, and what it is using.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unowned {
    pub name: String,
    /// The box its label names, when that box no longer exists. `None` for a container with no
    /// label at all.
    pub gone_box: Option<String>,
    /// `memory.current` of its cgroup, or `None` when that could not be read.
    pub memory_bytes: Option<u64>,
    pub disk_bytes: u64,
}

/// A gone box's cgroup that still holds processes, so it was left where it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StaleCgroup {
    /// The box it was made for.
    pub owner: String,
    /// Where it is in the sandbox, as a person would type it: under `/sys/fs/cgroup`.
    pub path: String,
    /// Processes in it and in every cgroup under it.
    pub procs: usize,
    /// Its `memory.current`, which counts everything under it.
    pub memory_bytes: u64,
}

/// What one pass did to the cgroups of gone boxes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Reclaimed {
    /// Removed, because they were empty. As paths under `/sys/fs/cgroup`.
    pub removed: Vec<String>,
    /// Left in place, because they still hold processes.
    pub held: Vec<StaleCgroup>,
}

/// The Settings → Diagnostics row: a mark, and what is said under it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct UnownedRow {
    /// `true` draws ✓: every running container belongs to a box and no gone box's cgroup holds
    /// anything. Anything else draws `!`, never ✗, because nothing here is broken (the owner's
    /// decision on the wording).
    pub clear: bool,
    pub said: Vec<Said>,
}

/// One paragraph of the row, and the commands that go under it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Said {
    pub text: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub offers: Vec<Offer>,
}

/// A command for a person to copy. **There is no button for any of these**: the owner declined
/// one, and this file never runs what it offers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Offer {
    /// The words before the command, ending in a colon.
    pub lead: String,
    pub command: String,
    /// Running it destroys something, so the page says so beside it.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub destructive: bool,
}

/// Ask docker something: its arguments in, its stdout out.
pub type Docker<'a> = &'a mut dyn FnMut(&[&str]) -> Result<String, String>;

/// Remove one cgroup directory, and say whether it went.
pub type Rmdir<'a> = &'a mut dyn FnMut(&Path) -> bool;

/// Every running container, with what the report needs of each. Two questions to docker, `ps`
/// and `inspect`, and nothing else.
///
/// Each field is printed as JSON, so a name or label containing a tab or a quote cannot shift the
/// columns. `--size` is what fills in `SizeRw`.
pub fn running_containers(docker: Docker) -> Result<Vec<Container>, String> {
    let ids = docker(&["ps", "-q", "--no-trunc"])?;
    let ids: Vec<&str> = ids.split_whitespace().collect();
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let format = format!(
        "{{{{json .Name}}}} {{{{json (index .Config.Labels \"{CONTAINER_LABEL}\")}}}} \
         {{{{json .State.Pid}}}} {{{{json .SizeRw}}}}"
    );
    let mut args = vec!["inspect", "--size", "--format", format.as_str()];
    args.extend(ids);
    Ok(parse_inspect(&docker(&args)?))
}

/// Read what [`running_containers`]' `inspect` printed: one line per container, four JSON values.
/// A line that does not read is skipped. That is a container that exited between the two
/// questions, and there is nothing left of it to report.
fn parse_inspect(text: &str) -> Vec<Container> {
    text.lines()
        .filter_map(|line| {
            let values: Vec<serde_json::Value> = serde_json::Deserializer::from_str(line)
                .into_iter::<serde_json::Value>()
                .collect::<Result<_, _>>()
                .ok()?;
            let [name, label, pid, size] = values.as_slice() else {
                return None;
            };
            Some(Container {
                name: name.as_str()?.trim_start_matches('/').to_string(),
                label: label
                    .as_str()
                    .map(str::trim)
                    .filter(|l| !l.is_empty())
                    .map(str::to_string),
                pid: pid.as_u64()? as u32,
                disk_bytes: size.as_u64().unwrap_or(0),
            })
        })
        .collect()
}

/// A process's memory, read from the cgroup it is in: `/proc/<pid>/cgroup` names it, and its
/// `memory.current` counts it. This works wherever the container's cgroup is, whichever parent it
/// asked for, which is why the path is not worked out from the container's id.
pub fn container_memory(pid: u32, proc_root: &Path, cgroup_root: &Path) -> Option<u64> {
    let membership =
        std::fs::read_to_string(proc_root.join(pid.to_string()).join("cgroup")).ok()?;
    let path = membership
        .lines()
        .find_map(|l| l.strip_prefix("0::"))?
        .trim()
        .trim_start_matches('/');
    std::fs::read_to_string(cgroup_root.join(path).join("memory.current"))
        .ok()?
        .trim()
        .parse()
        .ok()
}

/// The running containers that belong to no box: no label, or a label naming a box that is not
/// in `live`.
pub fn unowned_containers(
    running: Vec<Container>,
    live: &BTreeSet<String>,
    memory: impl Fn(u32) -> Option<u64>,
) -> Vec<Unowned> {
    running
        .into_iter()
        .filter(|c| !c.label.as_ref().is_some_and(|b| live.contains(b)))
        .map(|c| Unowned {
            memory_bytes: memory(c.pid),
            gone_box: c.label,
            name: c.name,
            disk_bytes: c.disk_bytes,
        })
        .collect()
}

/// Is this directory name one docker made for a container, rather than one made for a box?
///
/// `cgroupfs` names a container's cgroup after its 64-hex id and `systemd` after
/// `docker-<id>.scope`. Those are docker's to create and remove, and a container being started has
/// an empty one for a moment, so removing it would break the start. A box's cgroup under
/// `containers` is named after the box by the docker shim's `--cgroup-parent`.
fn made_by_docker(name: &str) -> bool {
    let hex = |s: &str| s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit());
    hex(name) || name.ends_with(".scope") || name.ends_with(".slice")
}

/// Every cgroup skein made for a box, whether the box exists or not: `skein/<box>` and
/// `skein/containers/<box>`, with the box each belongs to.
fn box_cgroups(cgroup_root: &Path) -> Vec<(String, PathBuf)> {
    let dirs = |at: PathBuf| -> Vec<(String, PathBuf)> {
        std::fs::read_dir(&at)
            .into_iter()
            .flatten()
            .flatten()
            .filter(|e| e.path().is_dir())
            .filter_map(|e| Some((e.file_name().to_str()?.to_string(), e.path())))
            .collect()
    };
    let skein = cgroup_root.join("skein");
    let mut found: Vec<(String, PathBuf)> = dirs(skein.clone())
        .into_iter()
        .filter(|(name, _)| name != "containers")
        .chain(
            dirs(skein.join("containers"))
                .into_iter()
                .filter(|(name, _)| !made_by_docker(name)),
        )
        .filter(|(name, _)| valid_name(name))
        .collect();
    found.sort();
    found
}

/// A cgroup and every cgroup under it, the deepest first, which is the order `rmdir` needs.
fn subtree_deepest_first(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        if entry.path().is_dir() {
            out.extend(subtree_deepest_first(&entry.path()));
        }
    }
    out.push(dir.to_path_buf());
    out
}

/// How many processes are in a cgroup and every cgroup under it. An unreadable `cgroup.procs`
/// counts as one process, so a cgroup skein cannot see into is one it leaves alone.
fn procs_in(dir: &Path) -> usize {
    subtree_deepest_first(dir)
        .iter()
        .map(|d| match std::fs::read_to_string(d.join("cgroup.procs")) {
            Ok(text) => text.lines().filter(|l| !l.trim().is_empty()).count(),
            Err(_) => 1,
        })
        .sum()
}

/// Remove the cgroups of gone boxes that are empty, and report the ones that are not.
///
/// **Empty means no process anywhere in the subtree.** The kernel refuses to `rmdir` a populated
/// cgroup anyway. The check is here as well because the kernel refuses only the cgroup it is
/// given: a subtree whose leaves are empty and whose middle is not would lose its leaves before
/// the refusal. And because a cgroup holding processes is the one the owner needs to hear about.
/// Children go first, since a cgroup with a child cannot be removed.
pub fn reclaim(cgroup_root: &Path, live: &BTreeSet<String>, rmdir: Rmdir) -> Reclaimed {
    let mut out = Reclaimed::default();
    for (owner, dir) in box_cgroups(cgroup_root) {
        if live.contains(&owner) {
            continue;
        }
        let shown = format!(
            "/sys/fs/cgroup/{}",
            dir.strip_prefix(cgroup_root).unwrap_or(&dir).display()
        );
        let procs = procs_in(&dir);
        if procs > 0 {
            out.held.push(StaleCgroup {
                memory_bytes: std::fs::read_to_string(dir.join("memory.current"))
                    .ok()
                    .and_then(|s| s.trim().parse().ok())
                    .unwrap_or(0),
                owner,
                path: shown,
                procs,
            });
            continue;
        }
        if subtree_deepest_first(&dir).iter().all(|d| rmdir(d)) {
            out.removed.push(shown);
        }
    }
    out
}

/// Memory as the row prints it: `210M` under a gigabyte and `1.4G` from one up.
fn memory(bytes: u64) -> String {
    let mib = bytes / (1024 * 1024);
    match mib < 1024 {
        true => format!("{mib}M"),
        false => gib(mib),
    }
}

/// Disk as the row prints it, `0.4G`.
fn disk(bytes: u64) -> String {
    gib(bytes / (1024 * 1024))
}

/// MiB as `3.2G`, the form `health::gib` prints for the fleet disk row. Written out here rather
/// than called, because `fleet` does not depend on `health` (docs/modules.toml), and one line is
/// not worth a new edge.
fn gib(mib: u64) -> String {
    format!("{:.1}G", mib as f64 / 1024.0)
}

/// `a`, `a and b`, `a, b and c`.
fn and_list(items: &[String]) -> String {
    match items {
        [] => String::new(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

/// A container name as a shell word. Docker allows only `[a-zA-Z0-9][a-zA-Z0-9_.-]*`, so this
/// quotes nothing in practice; it is here so a name docker did not check cannot become two words.
fn word(name: &str) -> String {
    match valid_name(name) {
        true => name.to_string(),
        false => sh_quote(name),
    }
}

/// The row, from what one pass found. `containers` is `Err` when docker did not answer.
pub fn unowned_row(
    containers: &Result<Vec<Unowned>, String>,
    held: &[StaleCgroup],
    sandbox: &str,
) -> UnownedRow {
    let mut said = Vec::new();
    match containers {
        Err(_) => said.push(Said {
            text: "docker did not answer, so skein cannot say which containers belong to no box"
                .into(),
            offers: Vec::new(),
        }),
        Ok(found) if found.is_empty() => said.push(Said {
            text: "every running container belongs to a box".into(),
            offers: Vec::new(),
        }),
        Ok(found) => said.extend(containers_said(found, sandbox)),
    }
    for stale in held {
        let (holds, them, count) = match stale.procs {
            1 => ("1 process".to_string(), "it", "it counts"),
            n => (format!("{n} processes"), "them", "they count"),
        };
        said.push(Said {
            text: format!(
                "{} no longer exists, but its cgroup still holds {holds} using {}, and {count} \
                 against the fleet's memory. skein removes a gone box's cgroup only once it is \
                 empty.",
                stale.owner,
                memory(stale.memory_bytes),
            ),
            offers: vec![
                Offer {
                    lead: format!("see {them}:"),
                    command: format!("sbx exec {sandbox} cat {}/cgroup.procs", stale.path),
                    destructive: false,
                },
                Offer {
                    lead: match stale.procs {
                        1 => "if it is yours to end:".into(),
                        _ => "if they are yours to end:".into(),
                    },
                    // `sudo`, because `cgroup.kill` is root's, and `sbx exec`, because the cgroup
                    // is in the sandbox and the person reading this is not.
                    command: format!(
                        "sbx exec {sandbox} sudo sh -c 'echo 1 > {}/cgroup.kill'",
                        stale.path
                    ),
                    destructive: true,
                },
            ],
        });
    }
    UnownedRow {
        clear: matches!(containers, Ok(found) if found.is_empty()) && held.is_empty(),
        said,
    }
}

/// The paragraphs about containers that belong to no box, and the two commands under them.
fn containers_said(found: &[Unowned], sandbox: &str) -> Vec<Said> {
    let mem = |u: &Unowned| u.memory_bytes.map(memory).unwrap_or_else(|| "?".into());
    let items: Vec<String> = found
        .iter()
        .enumerate()
        .map(|(i, u)| match i {
            0 => format!(
                "{} ({} memory, {} on disk)",
                u.name,
                mem(u),
                disk(u.disk_bytes)
            ),
            _ => format!("{} ({}, {})", u.name, mem(u), disk(u.disk_bytes)),
        })
        .collect();
    let first = match found.len() {
        1 => format!(
            "1 container belongs to no box skein can name: {}. Stopping or destroying a box never \
             stops it, and what it uses counts against every box.",
            items[0]
        ),
        n => format!(
            "{n} containers belong to no box skein can name: {}, {} memory and {} disk in all. \
             Stopping or destroying a box never stops them, and what they use counts against \
             every box.",
            and_list(&items),
            // Unknown if any one of them is: a total that left one out would be a smaller number
            // than the truth, printed as though it were the truth.
            found
                .iter()
                .map(|u| u.memory_bytes)
                .sum::<Option<u64>>()
                .map(memory)
                .unwrap_or_else(|| "?".into()),
            disk(found.iter().map(|u| u.disk_bytes).sum()),
        ),
    };
    let names = found
        .iter()
        .map(|u| word(&u.name))
        .collect::<Vec<_>>()
        .join(" ");
    let one = found.len() == 1;
    let offers = vec![
        Offer {
            lead: match one {
                true => "stopping keeps it and frees its memory:".into(),
                false => "stopping keeps them and frees their memory:".into(),
            },
            command: format!("sbx exec {sandbox} docker stop {names}"),
            destructive: false,
        },
        Offer {
            lead: match one {
                true => "skein did not start it and does not remove it; if it is yours to delete:"
                    .into(),
                false => "skein did not start them and does not remove them; if they are yours \
                          to delete:"
                    .into(),
            },
            command: format!("sbx exec {sandbox} docker rm -f {names}"),
            destructive: true,
        },
    ];
    let gone: Vec<String> = found
        .iter()
        .filter_map(|u| {
            let owner = u.gone_box.as_ref()?;
            Some(format!(
                "{} was started by {owner}, which no longer exists.",
                u.name
            ))
        })
        .collect();
    match gone.is_empty() {
        true => vec![Said {
            text: first,
            offers,
        }],
        false => vec![
            Said {
                text: first,
                offers: Vec::new(),
            },
            Said {
                text: format!(
                    "A container whose label names a gone box: {}",
                    gone.join(" ")
                ),
                offers,
            },
        ],
    }
}

/// One whole pass against the cgroup tree and `/proc` it is given: remove what is empty, then say
/// what is left. The set of live boxes is [`live_box_names`], which reads `$SKEIN_FLEET_ROOT` and
/// `$SKEIN_HOME/places`. A test pins both and hands in a scratch tree, and never reaches the live
/// `/sys/fs/cgroup/skein`.
///
/// **Nothing is removed when the fleet root cannot be read.** Every box would then look gone, so
/// the pass reports and removes nothing, and the row keeps whatever the previous pass said.
pub fn reconcile_under(
    cgroup_root: &Path,
    proc_root: &Path,
    docker: Docker,
    rmdir: Rmdir,
) -> Option<UnownedRow> {
    std::fs::read_dir(fleet_root()).ok()?;
    let live = super::disk::live_box_names();
    let reclaimed = reclaim(cgroup_root, &live, rmdir);
    let containers = running_containers(docker).map(|running| {
        unowned_containers(running, &live, |pid| {
            container_memory(pid, proc_root, cgroup_root)
        })
    });
    Some(unowned_row(&containers, &reclaimed.held, &fleet_sandbox()))
}

/// The last row a pass produced, or `None` before the first one.
static LAST: std::sync::Mutex<Option<UnownedRow>> = std::sync::Mutex::new(None);

/// What the last pass found, for the health report. **Reads only**: the report is polled, and a
/// polled endpoint must not spawn `docker`. `None` until a pass has run, and the page then draws
/// no row, since it has nothing true to say yet.
pub fn unowned_report() -> Option<UnownedRow> {
    LAST.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

/// The real `docker`, bounded, because a wedged daemon is one of the things this reports on and
/// must not hang the pass. A non-zero exit that still printed something is kept: `inspect` exits
/// 1 when one container of many has just gone, and prints the rest.
fn real_docker(args: &[&str]) -> Result<String, String> {
    let out = crate::util::output_with_timeout_why(
        std::process::Command::new("docker").args(args),
        Duration::from_secs(60),
    )?;
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    match out.status.success() || !stdout.trim().is_empty() {
        true => Ok(stdout),
        false => Err(String::from_utf8_lossy(&out.stderr).trim().to_string()),
    }
}

/// `rmdir` on cgroupfs, with `sudo` when skein is not allowed to itself. The launcher made these
/// cgroups with `sudo mkdir`, so they are root's.
fn real_rmdir(dir: &Path) -> bool {
    if std::fs::remove_dir(dir).is_ok() {
        return true;
    }
    crate::util::output_with_timeout(
        std::process::Command::new("sudo")
            .args(["-n", "rmdir"])
            .arg(dir),
        Duration::from_secs(15),
    )
    .is_some_and(|out| out.status.success())
}

/// The server's loop: one pass now and one every [`RECONCILE_EVERY`]. A sandbox with no
/// `/sys/fs/cgroup/skein` has no boxes' cgroups and no fleet to reconcile, so the pass does nothing
/// there and the row stays absent.
pub async fn watch_unowned() {
    let mut tick = tokio::time::interval(RECONCILE_EVERY);
    loop {
        tick.tick().await;
        let pass = tokio::task::spawn_blocking(|| {
            let root = Path::new("/sys/fs/cgroup");
            if !root.join("skein").is_dir() {
                return None;
            }
            reconcile_under(root, Path::new("/proc"), &mut real_docker, &mut real_rmdir)
        })
        .await;
        match pass {
            Ok(Some(row)) => *LAST.lock().unwrap_or_else(|e| e.into_inner()) = Some(row),
            Ok(None) => {}
            Err(e) => eprintln!("skein: watch_unowned: {e}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::*;

    const MIB: u64 = 1024 * 1024;
    const PG_ID: &str = "1111111111111111111111111111111111111111111111111111111111111111";
    const SCRATCH_ID: &str = "2222222222222222222222222222222222222222222222222222222222222222";
    const THING_ID: &str = "3333333333333333333333333333333333333333333333333333333333333333";

    fn write(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    /// A cgroup directory with a `cgroup.procs`, as cgroupfs always has one.
    fn cgroup(root: &Path, rel: &str, procs: &str) -> PathBuf {
        let dir = root.join(rel);
        write(&dir.join("cgroup.procs"), procs);
        dir
    }

    /// The tree every test here starts from, and what it is meant to show:
    ///
    /// * `skein/box-a`: a live box's cgroup, empty. Kept, because its box exists.
    /// * `skein/box-old`: a gone box's cgroup still holding three processes. Kept and reported.
    /// * `skein/box-gone` with a child, and `skein/containers/box-gone`: a gone box's, empty.
    ///   Removed, child first.
    /// * `skein/containers/<id>`: docker's own cgroups for three running containers, which are
    ///   docker's to remove and never skein's, even when empty.
    fn fleet_tree(root: &Path) {
        cgroup(root, "skein/box-a", "");
        let old = cgroup(root, "skein/box-old", "11\n12\n13\n");
        write(&old.join("memory.current"), &(180 * MIB).to_string());
        cgroup(root, "skein/box-gone", "");
        cgroup(root, "skein/box-gone/sub", "");
        cgroup(root, "skein/containers/box-gone", "");
        for (id, bytes) in [
            (PG_ID, 1_503_238_554u64),
            (SCRATCH_ID, 210 * MIB),
            (THING_ID, 64 * MIB),
        ] {
            let dir = cgroup(root, &format!("skein/containers/{id}"), "");
            write(&dir.join("memory.current"), &bytes.to_string());
        }
    }

    /// `/proc/<pid>/cgroup` for each container's init.
    fn proc_tree(root: &Path) {
        for (pid, id) in [(101, PG_ID), (102, SCRATCH_ID), (103, THING_ID)] {
            write(
                &root.join(pid.to_string()).join("cgroup"),
                &format!("0::/skein/containers/{id}\n"),
            );
        }
    }

    /// A docker that answers `ps` and `inspect` for three running containers and writes down
    /// every question it was asked:
    ///
    /// * `pg-scratch`, whose label names `box-old`, a box that no longer exists;
    /// * `a3f9c01e`, with no label at all;
    /// * `thing`, labelled for `box-a`, which exists. Never reported.
    fn fake_docker(
        asked: &mut Vec<Vec<String>>,
    ) -> impl FnMut(&[&str]) -> Result<String, String> + '_ {
        move |args: &[&str]| {
            asked.push(args.iter().map(|a| a.to_string()).collect());
            match args.first() {
                Some(&"ps") => Ok(format!("{PG_ID}\n{SCRATCH_ID}\n{THING_ID}\n")),
                Some(&"inspect") => Ok(format!(
                    "\"/pg-scratch\" \"box-old\" 101 {}\n\
                     \"/a3f9c01e\" \"\" 102 {}\n\
                     \"/thing\" \"box-a\" 103 {}\n",
                    3_435_973_837u64,
                    429_496_730u64,
                    5 * MIB
                )),
                _ => Err(format!(
                    "this fake docker answers ps and inspect, not {args:?}"
                )),
            }
        }
    }

    /// A stand-in for `rmdir` on cgroupfs, where a cgroup's own files go with it. It writes down
    /// what it removed.
    fn fake_rmdir(removed: &mut Vec<PathBuf>) -> impl FnMut(&Path) -> bool + '_ {
        move |dir: &Path| {
            removed.push(dir.to_path_buf());
            std::fs::remove_dir_all(dir).is_ok()
        }
    }

    /// A fleet with `box-a` in it and nothing else, pinned so nothing here reads the live one.
    /// The pins are undone when what this returns is dropped, a failing assertion included.
    fn pinned_fleet(home: &Path) -> EnvPins {
        let mut env = env_pins();
        env.set("SKEIN_HOME", home);
        env.set("SKEIN_FLEET_ROOT", home.join("fleet"));
        std::fs::create_dir_all(home.join("fleet/box-a")).unwrap();
        save_config(&Config {
            fleet_sandbox: "example".into(),
            ..Config::default()
        })
        .unwrap();
        env
    }

    /// A gone box's cgroup is removed when it is empty, and one still holding processes is not.
    ///
    /// What makes it fail: dropping the `procs > 0` check sends `skein/box-old` to `rmdir` (the
    /// "holding processes" assertion). Dropping the `live` check removes `skein/box-a`. Treating
    /// docker's own id-named cgroups as a box's removes the empty container cgroup.
    #[test]
    fn an_empty_cgroup_of_a_gone_box_is_removed_and_one_still_holding_processes_is_kept() {
        let dir = tempdir();
        let root = dir.join("cgroup");
        fleet_tree(&root);
        cgroup(&root, &format!("skein/containers/{}", "4".repeat(64)), "");
        let live: BTreeSet<String> = ["box-a".to_string()].into();
        let mut removed = Vec::new();
        let got = reclaim(&root, &live, &mut fake_rmdir(&mut removed));

        assert!(
            !removed.contains(&root.join("skein/box-old")) && root.join("skein/box-old").is_dir(),
            "a cgroup still holding processes was removed: {removed:?}"
        );
        assert!(
            root.join("skein/box-a").is_dir(),
            "a live box's cgroup was removed: {removed:?}"
        );
        assert!(
            root.join("skein/containers").join("4".repeat(64)).is_dir(),
            "a cgroup docker made for a container was removed: {removed:?}"
        );
        assert_eq!(
            got.removed,
            vec![
                "/sys/fs/cgroup/skein/box-gone".to_string(),
                "/sys/fs/cgroup/skein/containers/box-gone".to_string(),
            ],
            "the empty cgroups of a gone box were not removed"
        );
        assert!(!root.join("skein/box-gone").exists());
        assert_eq!(
            removed.first(),
            Some(&root.join("skein/box-gone/sub")),
            "a cgroup's child has to go before it can"
        );
        assert_eq!(
            got.held,
            vec![StaleCgroup {
                owner: "box-old".into(),
                path: "/sys/fs/cgroup/skein/box-old".into(),
                procs: 3,
                memory_bytes: 180 * MIB,
            }],
            "the cgroup left in place is not the one reported"
        );
    }

    /// The row says what the owner approved, word for word, about the fixture above: the example
    /// in the approved wording, built for real from a fake docker, `/proc` and cgroupfs.
    ///
    /// What makes it fail: a container labelled for a live box being reported (`thing` would
    /// appear), a size read from the wrong place, or any change to the approved words.
    #[test]
    fn the_row_says_what_the_owner_approved_about_what_no_box_owns() {
        let _g = env_lock();
        let home = tempdir();
        let _env = pinned_fleet(&home);
        let root = home.join("cgroup");
        let proc_root = home.join("proc");
        fleet_tree(&root);
        proc_tree(&proc_root);
        let mut asked = Vec::new();
        let mut removed = Vec::new();
        let row = reconcile_under(
            &root,
            &proc_root,
            &mut fake_docker(&mut asked),
            &mut fake_rmdir(&mut removed),
        )
        .expect("a pinned fleet root reads, so the pass runs");

        let offer = |lead: &str, command: &str, destructive: bool| Offer {
            lead: lead.into(),
            command: command.into(),
            destructive,
        };
        assert_eq!(
            row,
            UnownedRow {
                clear: false,
                said: vec![
                    Said {
                        text: "2 containers belong to no box skein can name: pg-scratch (1.4G \
                               memory, 3.2G on disk) and a3f9c01e (210M, 0.4G), 1.6G memory and \
                               3.6G disk in all. Stopping or destroying a box never stops them, \
                               and what they use counts against every box."
                            .into(),
                        offers: vec![],
                    },
                    Said {
                        text: "A container whose label names a gone box: pg-scratch was started \
                               by box-old, which no longer exists."
                            .into(),
                        offers: vec![
                            offer(
                                "stopping keeps them and frees their memory:",
                                "sbx exec example docker stop pg-scratch a3f9c01e",
                                false,
                            ),
                            offer(
                                "skein did not start them and does not remove them; if they are \
                                 yours to delete:",
                                "sbx exec example docker rm -f pg-scratch a3f9c01e",
                                true,
                            ),
                        ],
                    },
                    Said {
                        text: "box-old no longer exists, but its cgroup still holds 3 processes \
                               using 180M, and they count against the fleet's memory. skein \
                               removes a gone box's cgroup only once it is empty."
                            .into(),
                        offers: vec![
                            offer(
                                "see them:",
                                "sbx exec example cat /sys/fs/cgroup/skein/box-old/cgroup.procs",
                                false
                            ),
                            offer(
                                "if they are yours to end:",
                                "sbx exec example sudo sh -c 'echo 1 > \
                                 /sys/fs/cgroup/skein/box-old/cgroup.kill'",
                                true,
                            ),
                        ],
                    },
                ],
            }
        );
        assert!(
            !root.join("skein/box-gone").exists() && root.join("skein/box-a").is_dir(),
            "the pass did not reclaim against the pinned fleet's own boxes: {removed:?}"
        );
    }

    /// **The reconciler never calls `docker stop` or `docker rm`**, or anything else that
    /// changes a container. The owner declined a Stop button so that this could stay true.
    ///
    /// Every question the pass asks docker is written down by the fake, and each one's verb has
    /// to be `ps` or `inspect`. What makes it fail: any call to `docker stop`, `rm`, `kill`,
    /// `pause` or `update` anywhere in the pass, which is the named assertion below.
    #[test]
    fn the_reconciler_asks_docker_questions_and_never_stops_or_removes_anything() {
        let _g = env_lock();
        let home = tempdir();
        let _env = pinned_fleet(&home);
        let root = home.join("cgroup");
        let proc_root = home.join("proc");
        fleet_tree(&root);
        proc_tree(&proc_root);
        let mut asked = Vec::new();
        let mut removed = Vec::new();
        reconcile_under(
            &root,
            &proc_root,
            &mut fake_docker(&mut asked),
            &mut fake_rmdir(&mut removed),
        );
        assert!(
            asked.iter().any(|a| a[0] == "inspect"),
            "the pass never asked docker anything, so this proves nothing: {asked:?}"
        );
        for call in &asked {
            assert!(
                matches!(call[0].as_str(), "ps" | "inspect"),
                "the reconciler changed a container rather than asking about it: docker {}",
                call.join(" ")
            );
        }
        assert!(
            removed.iter().all(|d| d.starts_with(&root)),
            "an rmdir outside the cgroup tree it was given: {removed:?}"
        );
    }

    /// Nothing is removed when the fleet root cannot be read: every box would look gone.
    ///
    /// What makes it fail: taking out the `read_dir(fleet_root())` guard, after which `box-a`,
    /// empty and apparently gone, is removed.
    #[test]
    fn a_fleet_whose_boxes_cannot_be_listed_has_nothing_removed() {
        let _g = env_lock();
        let home = tempdir();
        let mut env = pinned_fleet(&home);
        env.set("SKEIN_FLEET_ROOT", home.join("no-such-fleet"));
        let root = home.join("cgroup");
        fleet_tree(&root);
        let mut asked = Vec::new();
        let mut removed = Vec::new();
        let row = reconcile_under(
            &root,
            &home.join("proc"),
            &mut fake_docker(&mut asked),
            &mut fake_rmdir(&mut removed),
        );
        assert_eq!(row, None);
        assert!(
            removed.is_empty() && root.join("skein/box-a").is_dir(),
            "cgroups were removed on a fleet whose boxes could not be listed: {removed:?}"
        );
    }

    /// The other two states of the row: ✓ when there is nothing to say, and `!` when docker did
    /// not answer, in the approved words.
    ///
    /// What makes it fail: an unanswered docker read as "nothing unowned" (the `clear` assertion),
    /// or a stale cgroup left off a row whose containers are all accounted for.
    #[test]
    fn nothing_unowned_is_a_tick_and_a_silent_docker_is_not() {
        let none = unowned_row(&Ok(Vec::new()), &[], "example");
        assert!(none.clear);
        assert_eq!(
            none.said,
            vec![Said {
                text: "every running container belongs to a box".into(),
                offers: vec![],
            }]
        );

        let silent = unowned_row(&Err("Cannot connect".into()), &[], "example");
        assert!(
            !silent.clear,
            "docker not answering was read as nothing unowned"
        );
        assert_eq!(
            silent.said[0].text,
            "docker did not answer, so skein cannot say which containers belong to no box"
        );

        let stale = StaleCgroup {
            owner: "box-old".into(),
            path: "/sys/fs/cgroup/skein/box-old".into(),
            procs: 2,
            memory_bytes: 3 * 1024 * MIB,
        };
        let held = unowned_row(&Ok(Vec::new()), std::slice::from_ref(&stale), "example");
        assert!(
            !held.clear,
            "a gone box's cgroup holding processes was drawn as a tick"
        );
        assert!(
            held.said[1]
                .text
                .contains("still holds 2 processes using 3.0G"),
            "{:?}",
            held.said
        );
        // Both stale-cgroup commands run in the sandbox, where the cgroup is, and the kill as root,
        // because `cgroup.kill` is root's. What makes it fail: either command losing its
        // `sbx exec <sandbox>` prefix (it would then read the host's own `/sys`), or the kill losing
        // its `sudo` (the write is refused).
        let [see, end] = held.said[1].offers.as_slice() else {
            panic!(
                "a stale cgroup offers two commands: {:?}",
                held.said[1].offers
            );
        };
        for offer in [see, end] {
            assert!(
                offer.command.starts_with("sbx exec example "),
                "a stale-cgroup command would run on the host, not in the sandbox: {}",
                offer.command
            );
        }
        assert!(
            end.command
                .starts_with("sbx exec example sudo sh -c 'echo 1 > ")
                && end.command.ends_with("/cgroup.kill'"),
            "the kill is not written as root: {}",
            end.command
        );

        // A memory skein could not read is `?`, and so is any total it would have been part of.
        // What makes it fail: summing only the known ones, which prints a total smaller than the
        // truth as though it were the truth.
        let unread = |name: &str, memory_bytes| Unowned {
            name: name.into(),
            gone_box: None,
            memory_bytes,
            disk_bytes: 0,
        };
        let partly = unowned_row(
            &Ok(vec![unread("box-a-db", Some(MIB)), unread("thing", None)]),
            &[],
            "example",
        );
        assert!(
            partly.said[0]
                .text
                .contains("thing (?, 0.0G), ? memory and 0.0G disk in all"),
            "an unread memory was left out of the total: {}",
            partly.said[0].text
        );
    }

    /// `inspect` output is read for what it says, and a line that does not read is skipped
    /// rather than guessed at.
    #[test]
    fn a_container_is_read_from_inspect_and_a_torn_line_is_skipped() {
        let got = parse_inspect(
            "\"/pg-scratch\" \"box-old\" 101 3435973837\n\
             \"/a3f9c01e\" \"\" 102 null\n\
             \"/half\n",
        );
        assert_eq!(
            got,
            vec![
                Container {
                    name: "pg-scratch".into(),
                    label: Some("box-old".into()),
                    pid: 101,
                    disk_bytes: 3_435_973_837,
                },
                Container {
                    name: "a3f9c01e".into(),
                    label: None,
                    pid: 102,
                    disk_bytes: 0,
                },
            ]
        );
    }
}
