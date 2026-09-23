//! Docker inside the fleet: where dockerd keeps its data and its cgroup, the label that ties a
//! container to its box, and the probe that says whether a resize would lose docker's state.

use super::*;

/// The label a box's containers carry, and the only thing that says whose they are.
///
/// Docker records nothing about which box asked. Every box reaches the same daemon over the same
/// socket at the same uid, and [`CONTAINER_CGROUP`] is one parent shared by all of them — which is
/// deliberate, because it is what makes a box's containers count against the fleet's ceilings
/// (SKEIN-89). Shared accounting and per-box ownership are different questions, and the second one
/// had no answer at all: stopping a box left its containers running, with nothing anywhere able to
/// say which they were.
pub const CONTAINER_LABEL: &str = "skein.box";

/// Where one box's containers live: a child of the shared parent, so the fleet ceiling above still
/// applies to every box's containers together while each box's are separately reachable.
pub fn box_container_cgroup(name: &str) -> String {
    format!("/sys/fs/cgroup{CONTAINER_CGROUP}/{name}")
}

/// End the containers a box started.
///
/// **`docker rm -f`, not a signal**, and that is the whole reason this is separate from
/// [`namespace_kill`]: killing a container's processes leaves the daemon believing it runs, so the
/// name stays taken, the volumes stay attached and `docker ps` disagrees with the machine. Removing
/// it is what a person means by "stop the container".
///
/// Then the cgroup, as a backstop for the case the first half cannot cover: a daemon that is not
/// answering. `docker ps` needs dockerd, and a wedged dockerd is one of the ways a box's container
/// got out of hand in the first place — so the kill is aimed at the box's own subtree under the
/// shared parent, which reaches the container's processes with dockerd out of the picture.
///
/// Neither half reaches a container the shim never saw. A box holds the daemon (§9.5 R11 says so),
/// so a container started by talking to the socket directly carries no label and lands in whatever
/// cgroup it asked for. That is stated rather than closed, and [`unattributed_containers`] is what
/// keeps it from being silent.
pub fn box_containers_kill(name: &str) -> String {
    format!(
        "if command -v docker >/dev/null 2>&1; then \
           docker ps -aq --filter {filter} 2>/dev/null | xargs -r docker rm -f >/dev/null 2>&1 || true; \
         fi; \
         sudo sh -c 'echo 1 > \"$1\"' _ {kill} 2>/dev/null || true",
        filter = sh_quote(&format!("label={CONTAINER_LABEL}={name}")),
        kill = sh_quote(&format!("{}/cgroup.kill", box_container_cgroup(name))),
    )
}

/// Running containers no box can be held responsible for, named rather than left unmentioned.
///
/// The shim stamps [`CONTAINER_LABEL`] onto `docker run` and `docker create`, and that is a
/// convention: `docker compose`, a container started by curling the socket, and anything begun
/// before the shim existed all carry nothing. Those are exactly the containers a stop cannot end,
/// so a stop that said nothing about them would report success over the thing it missed — which is
/// the shape of the bug this whole item is about, one layer up.
pub fn unattributed_containers() -> String {
    format!(
        "if command -v docker >/dev/null 2>&1; then \
           orphans=\"$(docker ps --filter label={label} --format '{{{{.Names}}}}' 2>/dev/null)\"; \
           all=\"$(docker ps --format '{{{{.Names}}}}' 2>/dev/null)\"; \
           for c in $all; do \
             case \" $orphans \" in *\" $c \"*) continue ;; esac; \
             echo \"skein: container $c belongs to no box skein can name, so stopping a box does not stop it\" >&2; \
           done; \
         fi",
        label = CONTAINER_LABEL,
    )
}

/// Where dockerd is told to put the containers it runs: **inside** the workload cgroup, beside the
/// boxes rather than in a tree of its own.
///
/// This is what makes the merged pool real. [`memory_plan`] gives the boxes and the containers they
/// start one share between them, and until dockerd is told this, that share was an intention with
/// nothing enforcing it — `/sys/fs/cgroup/skein` bounded only the boxes, and a container could take
/// as much again beside it. Nested here, the one ceiling on `skein` covers both, first-come, which
/// is what "one pool" was supposed to mean.
///
/// It has to be a *child* of `skein` rather than a sibling with its own ceiling, because a second
/// ceiling would be a second reservation — the boxes idling memory the containers may not have and
/// the other way round, which is the thing the merge removed.
pub const CONTAINER_CGROUP: &str = "/skein/containers";

/// Where dockerd keeps its data when it shares the boxes' disk.
///
/// Beside `.skein` in the fleet root rather than under `/var/lib`, for two reasons. It is plainly
/// skein's doing, next to the other thing skein put there; and the leading dot keeps it out of
/// `/boxes/*/`, which is how every box is enumerated — a `docker` directory there would read as a
/// box with no repo, which is a thing `resize_fleet` aborts on.
pub fn docker_data_root() -> String {
    format!("{}/.docker", fleet_root())
}

/// Point the sandbox's dockerd at [`CONTAINER_CGROUP`].
///
/// **Why this can be done at all, when capping `/sys/fs/cgroup/docker` could not.** That cgroup is
/// where dockerd puts its containers *and* where the sandbox's own container lives — init, socat,
/// dockerd, containerd — so every ceiling written there hit the machinery that answers `sbx exec`
/// rather than the build that overshot. `cgroup-parent` moves only the containers. What is left in
/// `/docker` is the sandbox itself, which is what [`MemoryPlan::plumbing`] and the reserve are for
/// and which nothing caps.
///
/// **It takes effect at the next dockerd start, not now.** `cgroup-parent` is not one of the
/// options dockerd re-reads on SIGHUP, and restarting dockerd here would stop every running
/// container — this sandbox has no live-restore, so a database someone is using would go down to
/// apply a memory ceiling. Written and left for the next cycle instead. Containers already running
/// stay where they are, outside the ceiling, until they are next recreated.
///
/// **Merged, never clobbered, and validated before it lands.** A `daemon.json` that does not parse
/// stops dockerd starting at all, so the failure this guards against is a fleet with no Docker: the
/// existing file is read first and kept if it holds other settings, a file that cannot be parsed is
/// reported and left exactly as it is rather than overwritten with something valid, and the new
/// content is re-read from disk before it replaces the old one.
pub(super) fn install_docker_config(sandbox: &str) -> Result<(), String> {
    // Empty when Docker keeps its own disk. Passed either way so the script has one shape.
    let root = if load_config().fleet_one_disk {
        docker_data_root()
    } else {
        String::new()
    };
    let script = format!(
        "sudo mkdir -p /etc/docker && sudo python3 - /etc/docker/daemon.json {} {}",
        sh_quote(CONTAINER_CGROUP),
        sh_quote(&root),
    );
    own_sandbox(sandbox)
        .write(
            &script,
            DOCKER_CONFIG_PY.as_bytes(),
            Duration::from_secs(30),
        )
        .map(|_| ())
        .map_err(|e| format!("writing /etc/docker/daemon.json in {sandbox}: {e}"))
}

/// The edit [`install_docker_config`] makes, as a program rather than a shell one-liner: it is a
/// read-modify-write of a file that stops dockerd booting when it is wrong, and that is worth being
/// able to read.
const DOCKER_CONFIG_PY: &str = r#"import json, os, sys
path, parent = sys.argv[1], sys.argv[2]
# Empty means "leave Docker on its own disk" — the argument is always passed, so the absent case is
# a value rather than a different invocation.
root = sys.argv[3] if len(sys.argv) > 3 else ""
try:
    config = json.load(open(path))
    if not isinstance(config, dict):
        raise ValueError("the top level is not an object")
except FileNotFoundError:
    config = {}          # no config at all is the normal case, not a problem
except Exception as e:
    # Deliberately not repaired. Something else wrote this, and replacing it with a valid file of
    # our own would take away settings dockerd is running on.
    sys.exit("skein: %s is not readable as JSON (%s); leaving it alone" % (path, e))
want_root = config.get("data-root") if not root else root
if config.get("cgroup-parent") == parent and config.get("data-root") == want_root:
    sys.exit(0)
config["cgroup-parent"] = parent
# One pool rather than two ceilings: dockerd's data goes on the sandbox's root filesystem, the same
# one the boxes are on, so a single number sizes the lot. Only ever *set*, never cleared — turning
# the setting off leaves dockerd reading the data it already has, because removing the key would
# point it back at an empty disk and make every image and volume vanish without deleting any of it.
if root:
    config["data-root"] = root
# Written beside the real file and re-read before it replaces it, so a half-written or unparseable
# result can never become the file dockerd starts from. `os.replace` is atomic within a filesystem.
scratch = path + ".skein-new"
with open(scratch, "w") as f:
    f.write(json.dumps(config, indent=2) + "\n")
json.load(open(scratch))
os.replace(scratch, path)
print("skein: dockerd will place containers under %s from its next start" % parent)
if root:
    print("skein: and keep its data in %s, on the same disk as the boxes" % root)
"#;

/// A memory size as MiB. Accepts what sbx accepts (`26g`, `512M`, a bare byte count).
///
/// `None` rather than a guess when it cannot be read: a mis-parsed ceiling is worse than no ceiling,
/// because it would silently cap every box at some number nobody chose.
pub(super) fn parse_mib(value: &str) -> Option<u64> {
    let value = value.trim().to_lowercase();
    // `26gi` and `26g` are the same size, so drop the `i` before looking at the unit — reading it as
    // the unit is exactly the mis-parse this function exists to avoid.
    let value = value.strip_suffix('i').unwrap_or(&value);
    let unit = value.chars().last()?;
    if unit.is_ascii_digit() {
        // A bare number: sbx reads it as bytes.
        return value.parse::<u64>().ok().map(|b| b / (1024 * 1024));
    }
    let digits = value[..value.len() - unit.len_utf8()].trim();
    let n = digits.parse::<u64>().ok()?;
    match unit {
        'g' => Some(n * 1024),
        'm' => Some(n),
        'k' => Some(n / 1024),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::*;

    /// One pool: dockerd's data moved onto the disk the boxes are on, so one number sizes both.
    ///
    /// Run for real, because what this is really testing is a file that stops dockerd booting when
    /// it is wrong — and `data-root` is the one key in it that can make every image and volume on
    /// the machine disappear from view. Three properties matter, and the last is the sharp one:
    ///
    /// 1. It lands, alongside the cgroup setting rather than instead of it.
    /// 2. It is idempotent, since this runs on every server start.
    /// 3. **Turning the setting off never removes it.** Removing the key would point dockerd back
    ///    at a disk it has not written to since, and every image and volume would vanish — none of
    ///    them deleted, all of them gone as far as anything asking Docker is concerned. Off must
    ///    mean "stop moving it", not "move it back".
    #[test]
    fn sharing_one_disk_with_docker_is_set_once_and_never_silently_undone() {
        let dir = tempdir();
        let path = std::path::Path::new(&dir).join("daemon.json");
        let run = |root: &str| -> std::process::Output {
            use std::io::Write;
            let mut child = std::process::Command::new("python3")
                .args(["-", &path.to_string_lossy(), CONTAINER_CGROUP, root])
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .expect("python3");
            child
                .stdin
                .take()
                .unwrap()
                .write_all(DOCKER_CONFIG_PY.as_bytes())
                .unwrap();
            child.wait_with_output().unwrap()
        };
        let read = || -> serde_json::Value {
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap()
        };
        let pool = "/boxes/.docker";

        assert!(run(pool).status.success());
        assert_eq!(
            read()["data-root"],
            pool,
            "docker was not moved onto the pool"
        );
        assert_eq!(
            read()["cgroup-parent"],
            CONTAINER_CGROUP,
            "moving the data must not cost the memory ceiling"
        );

        let again = run(pool);
        assert!(again.status.success());
        assert!(
            String::from_utf8_lossy(&again.stdout).is_empty(),
            "a config already pointed at the pool is not news, and this runs every server start"
        );

        // The sharp one. Off means stop moving it, not move it back.
        assert!(run("").status.success());
        assert_eq!(
            read()["data-root"],
            pool,
            "turning the setting off pointed dockerd back at an empty disk, and every image and \
             volume on the fleet would read as gone"
        );

        // And it is added to a config someone else owns, not substituted for it.
        std::fs::write(&path, r#"{"dns":["1.1.1.1"]}"#).unwrap();
        assert!(run(pool).status.success());
        assert_eq!(read()["dns"][0], "1.1.1.1");
        assert_eq!(read()["data-root"], pool);
    }

    /// `/etc/docker/daemon.json` is a file that stops dockerd starting *at all* when it is wrong, so
    /// the failure being guarded against is a fleet with no Docker. Run for real rather than
    /// asserted about, because what matters is what Python does to the file, not what this file
    /// believes it does.
    #[test]
    fn pointing_dockerd_at_the_workload_cgroup_never_costs_an_existing_config() {
        let dir = tempdir();
        let path = std::path::Path::new(&dir).join("daemon.json");
        // The empty third argument is Docker keeping its own disk — the shape every existing fleet
        // runs in, and the one this test has always been about.
        let run = || -> std::process::Output {
            use std::io::Write;
            let mut child = std::process::Command::new("python3")
                .args(["-", &path.to_string_lossy(), CONTAINER_CGROUP, ""])
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .expect("python3");
            child
                .stdin
                .take()
                .unwrap()
                .write_all(DOCKER_CONFIG_PY.as_bytes())
                .unwrap();
            child.wait_with_output().unwrap()
        };
        let read = || -> serde_json::Value {
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap()
        };

        // No file is the normal case, not an error.
        assert!(run().status.success());
        assert_eq!(read()["cgroup-parent"], CONTAINER_CGROUP);

        // Settings someone else put there survive: this adds a key, it does not own the file.
        std::fs::write(&path, r#"{"log-driver":"json-file","dns":["1.1.1.1"]}"#).unwrap();
        assert!(run().status.success());
        assert_eq!(read()["log-driver"], "json-file");
        assert_eq!(read()["dns"][0], "1.1.1.1");
        assert_eq!(read()["cgroup-parent"], CONTAINER_CGROUP);

        // Idempotent — this runs on every server start.
        let again = run();
        assert!(again.status.success());
        assert!(
            String::from_utf8_lossy(&again.stdout).is_empty(),
            "a config already pointed at the right place is not news"
        );

        // And a file that cannot be parsed is LEFT ALONE. Replacing it with a valid file of our own
        // would take away whatever dockerd is currently running on; refusing costs only the ceiling.
        let broken = "{ this is not json";
        std::fs::write(&path, broken).unwrap();
        let refused = run();
        assert!(!refused.status.success());
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            broken,
            "a config skein cannot read is one it must not overwrite"
        );
        assert!(
            String::from_utf8_lossy(&refused.stderr).contains("leaving it alone"),
            "and it has to say so, or the ceiling is silently absent"
        );
    }

    // A mis-parsed ceiling is worse than no ceiling: it would silently cap every box at a number
    // nobody chose, and the symptom is builds dying with no explanation. So an unreadable size
    // yields None and the box runs uncapped-but-loud, rather than capped-and-wrong.
    #[test]
    fn a_memory_size_is_read_or_refused_never_guessed() {
        assert_eq!(parse_mib("26g"), Some(26624));
        assert_eq!(parse_mib(" 26G "), Some(26624));
        assert_eq!(
            parse_mib("26gi"),
            Some(26624),
            "the i suffix is the same size"
        );
        assert_eq!(parse_mib("512m"), Some(512));
        assert_eq!(
            parse_mib("2097152"),
            Some(2),
            "a bare number is bytes, as sbx reads it"
        );
        for bad in ["", "lots", "26 gigs", "g", "-4g"] {
            assert_eq!(parse_mib(bad), None, "{bad:?} must not parse to a number");
        }
    }

    /// The docker shim, run for real against a fake `docker` that reports the argv it was given.
    ///
    /// Extracted from `box-session.sh` rather than restated: a copy of the shim in a test is a
    /// second thing to keep in step, and it would go on passing after the real one broke.
    fn docker_shim_argv(name: &str, args: &[&str]) -> Vec<String> {
        let dir = crate::testutil::tempdir();
        let real = dir.join("docker.real");
        let seen = dir.join("argv");
        std::fs::write(
            &real,
            format!(
                "#!/bin/sh\nfor a in \"$@\"; do printf '%s\\n' \"$a\"; done > {}\n",
                seen.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(
            &real,
            <std::fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o755),
        )
        .unwrap();

        let block = BOX_SESSION_SH
            .split_once("cat <<'DOCKERSHIM'\n")
            .and_then(|(_, rest)| rest.split_once("\nDOCKERSHIM"))
            .map(|(body, _)| body.to_string())
            .expect("the docker shim block is still in box-session.sh");
        let shim = dir.join("docker");
        std::fs::write(
            &shim,
            format!(
                "#!/bin/sh\nskein_box='{name}'\nskein_docker='{}'\n{block}\n",
                real.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(
            &shim,
            <std::fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o755),
        )
        .unwrap();

        let ran = std::process::Command::new("sh")
            .arg(&shim)
            .args(args)
            .output()
            .expect("run the docker shim");
        assert!(
            ran.status.success(),
            "the shim failed: {}",
            String::from_utf8_lossy(&ran.stderr)
        );
        std::fs::read_to_string(&seen)
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }

    /// A container a box starts says which box started it — the fact nothing recorded.
    #[test]
    fn a_container_a_box_starts_carries_the_boxs_name() {
        let argv = docker_shim_argv("web-feat-x", &["run", "-it", "img", "sh", "-c", "a b"]);
        assert_eq!(
            argv,
            vec![
                "run",
                "--label",
                "skein.box=web-feat-x",
                "--cgroup-parent",
                "/skein/containers/web-feat-x",
                "-it",
                "img",
                "sh",
                "-c",
                // The container's own command, still one argument. Rebuilding the list through a
                // string would have split this, and the failure would be a container running the
                // wrong command rather than an error anybody sees.
                "a b",
            ]
        );
    }

    /// The verb is not always `$1`, and the options that precede it can take arguments of their own.
    /// Reading `unix:///var/run/docker.sock` as the verb would stamp nothing and say nothing.
    #[test]
    fn the_stamp_finds_the_verb_past_dockers_own_options() {
        let argv = docker_shim_argv("api-x", &["-H", "unix:///run/docker.sock", "run", "img"]);
        assert_eq!(
            argv,
            vec![
                "-H",
                "unix:///run/docker.sock",
                "run",
                "--label",
                "skein.box=api-x",
                "--cgroup-parent",
                "/skein/containers/api-x",
                "img",
            ]
        );
    }

    /// Everything that is not a container being made passes through untouched. The shim is on every
    /// `docker` call in every box, so the shape matters more than the logic.
    #[test]
    fn the_docker_shim_is_out_of_the_way_of_everything_else() {
        assert_eq!(docker_shim_argv("api-x", &["ps", "-a"]), vec!["ps", "-a"]);
        assert_eq!(
            docker_shim_argv("api-x", &["compose", "up", "-d"]),
            vec!["compose", "up", "-d"],
            "compose composes its own create calls; stamping its argv would stamp nothing and \
             claim otherwise"
        );
        // A caller who named a cgroup parent meant it. Overruling them would be skein deciding
        // placement on the one flag whose whole purpose is placement.
        assert_eq!(
            docker_shim_argv("api-x", &["run", "--cgroup-parent", "/mine", "img"]),
            vec!["run", "--cgroup-parent", "/mine", "img"]
        );
    }

    /// The stop's two halves, and what each is for.
    #[test]
    fn stopping_a_box_removes_its_containers_and_can_still_reach_them_without_the_daemon() {
        let script = box_containers_kill("web-feat-x");
        assert!(
            script.contains("docker rm -f"),
            "killing a container's processes leaves the daemon believing it runs: {script}"
        );
        assert!(
            script.contains("--filter 'label=skein.box=web-feat-x'"),
            "the removal is not aimed at this box's containers: {script}"
        );
        // The backstop: a wedged dockerd cannot answer `docker ps`, and a container that wedged it
        // is one of the ways a box gets here.
        assert!(
            script.contains("/sys/fs/cgroup/skein/containers/web-feat-x/cgroup.kill"),
            "nothing reaches the containers when the daemon has stopped answering: {script}"
        );
        // And it is a child of the shared parent, so SKEIN-89's fleet ceiling still covers it.
        assert!(
            box_container_cgroup("web-feat-x")
                .starts_with(&format!("/sys/fs/cgroup{CONTAINER_CGROUP}/")),
            "the box's containers left the parent the fleet's ceiling is on"
        );
    }
}
