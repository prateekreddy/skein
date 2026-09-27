//! SSH host trust inside the fleet: which hosts a box may push to, and installing their keys.

use super::*;

/// Every SSH host any box might reach, from both places a repo names one.
///
/// `source` is where a box CLONES from; an entry registered before a path stopped being registrable
/// has a path there and an SSH URL on its `origin`, which is where its boxes PUSH. Reading only
/// `source` meant those four repos contributed no hosts at all — precisely the ones with an SSH origin.
fn ssh_hosts() -> Vec<String> {
    let mut hosts: Vec<String> = load_repos()
        .iter()
        .flat_map(|repo| {
            [
                repo.source.clone(),
                // `repo_origin_url`, not `remote_origin_url(&repo.source_tree)`: in-fleet a repo's
                // checkout is often not there at all, and asking a missing directory for its origin
                // answers `None`. An adopted repo with an SSH origin then pinned NO known-host, and
                // the box's first push met an unknown host with no explanation. Since SKEIN-468 this
                // resolves URL -> mirror origin -> checkout, so it answers wherever the truth is.
                repo_origin_url(repo).unwrap_or_default(),
            ]
        })
        .filter(|url| is_ssh_url(url))
        .filter_map(|url| host_of(&url).map(|h| h.to_string()))
        .collect();
    hosts.sort();
    hosts.dedup();
    hosts
}

/// The shell that pins those hosts into whichever `$HOME` it runs in.
fn known_hosts_script(hosts: &[String]) -> String {
    format!(
        "mkdir -p \"$HOME/.ssh\" && chmod 700 \"$HOME/.ssh\"; \
         for h in {hosts}; do \
           ssh-keygen -F \"$h\" >/dev/null 2>&1 && continue; \
           timeout 25 ssh -o BatchMode=yes -o StrictHostKeyChecking=accept-new -T \"git@$h\" \
             >/dev/null 2>&1; \
         done; exit 0",
        hosts = hosts
            .iter()
            .map(|h| sh_quote(h))
            .collect::<Vec<_>>()
            .join(" "),
    )
}

/// Trust the SSH hosts a box will clone from, once per sandbox.
///
/// A fleet box clones from the remote itself, and an SSH remote needs the host's key in
/// `known_hosts` first. A legacy box got that from its kit; the fleet sandbox never had it, so the
/// first migration of an SSH-remote repo failed with the least helpful pair of errors git produces:
///
///   ssh_askpass: exec(/usr/bin/ssh-askpass): No such file or directory
///   Host key verification failed.
///
/// which reads as a credentials problem and is a host-trust one — with no known host and no
/// terminal, SSH fell back to asking a human who was not there.
///
/// A real connection with `accept-new`, not `ssh-keyscan`: keyscan is answered with "Connection
/// closed by remote host" here while an ordinary `ssh -T` succeeds and records the key itself.
/// (That keyscan failure was first read as "port 22 is closed", which was wrong — the transport is
/// fine.) `accept-new` trusts an unknown host once and still refuses a CHANGED key, which is the
/// property worth keeping. Best-effort: an HTTPS repo needs none of this, and refusing to launch
/// over it would be absurd.
pub fn ensure_known_hosts(sandbox: &str) {
    let hosts = ssh_hosts();
    if hosts.is_empty() {
        return;
    }
    if let Err(e) = own_sandbox(sandbox).exec(&known_hosts_script(&hosts), Duration::from_secs(120))
    {
        eprintln!("skein: could not pin SSH host keys in {sandbox} ({e}); a box cloning over SSH will fail host key verification");
    }
}

/// The same trust, inside the box — where the agent's own `git push` runs.
///
/// The sandbox's `known_hosts` does not reach a box: every box has a private HOME, and that is the
/// point of it. Cloning never noticed because it runs in the sandbox namespace, so the gap only
/// showed when a box first pushed to a real remote and got `Host key verification failed` — read as
/// a credentials problem, and the credentials were fine the whole time.
pub fn ensure_box_known_hosts(name: &str) {
    let hosts = ssh_hosts();
    if hosts.is_empty() {
        return;
    }
    let Some(place) = place_of(name) else { return };
    if let Err(e) = place.exec(&known_hosts_script(&hosts), Duration::from_secs(120)) {
        eprintln!("skein: could not pin SSH host keys in {name} ({e}); pushing over SSH from it will fail host key verification");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repos::save_repos;
    use crate::testutil::*;

    /// The hosts to trust come from where boxes PUSH as well as where they clone, and those are not
    /// always the same string. `source` is a URL for every repo registered now — but a `repos.json`
    /// written before that still carries a path, and its mirror is the only thing that knows the
    /// remote. Reading `source` alone leaves such a box with no `known_hosts` entry for the one
    /// host it actually talks to. Seen live on 2026-08-30: a repo whose `source` was a dead path
    /// while its mirror fetched from GitHub perfectly well.
    #[test]
    fn host_trust_covers_the_remote_a_box_pushes_to() {
        let _g = env_lock();
        let home = tempdir();
        std::env::set_var("SKEIN_HOME", &home);
        let work = home.join("adopted");
        std::fs::create_dir_all(&work).unwrap();
        let git = |args: &[&str]| {
            std::process::Command::new("git")
                .args(args)
                .current_dir(&work)
                .output()
                .expect("git");
        };
        git(&["init", "-q"]);
        git(&["commit", "-q", "--allow-empty", "-m", "one"]);

        save_repos(&[Repo {
            read_prs: false,
            id: "adopted".into(),
            // A path, the way a repos.json written before URLs-only still reads.
            source: work.to_string_lossy().into_owned(),
            store: home.join("store").to_string_lossy().into_owned(),
            plane_project: String::new(),
            sync_connection: String::new(),
            review_queue: true,
            sync_gateway_url: String::new(),
            ..Default::default()
        }])
        .unwrap();
        // The mirror, and then the remote ON the mirror — which is where the SSH URL lives for a
        // repo whose `source` is a path. `repo_origin_url` resolves URL -> mirror -> nothing, so
        // this is the hop under test.
        let repo = &load_repos()[0];
        crate::repos::ensure_mirror(repo).unwrap();
        std::process::Command::new("git")
            .arg("-C")
            .arg(crate::repos::mirror_path("adopted"))
            .args([
                "remote",
                "set-url",
                "origin",
                "git@gitlab.example.com:o/r.git",
            ])
            .output()
            .expect("git");

        assert_eq!(
            ssh_hosts(),
            vec!["gitlab.example.com".to_string()],
            "a repo whose only SSH URL is on its mirror still needs its host trusted"
        );
        assert!(
            known_hosts_script(&ssh_hosts()).contains("StrictHostKeyChecking=accept-new"),
            "trust an unknown host once; still refuse a CHANGED one"
        );
        // Restored, or the next test to take `env_lock` inherits a SKEIN_HOME naming a
        // directory this test's guard has already removed — and writes through it, which
        // recreates the tree as a leak nobody owns.
        std::env::remove_var("SKEIN_HOME");
    }
}
