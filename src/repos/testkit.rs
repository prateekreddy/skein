//! Test helpers shared by more than one of this module's test files.

use super::*;

/// Register a repo whose upstream is a directory on this disk.
///
/// [`add_repo`] refuses a path — a repo is a remote now — and these tests are about the MIRROR,
/// which needs an upstream that exists without reaching the network. So they write the record
/// and make the mirror directly, which is all `add_repo` did for them anyway.
pub(super) fn registered(id: &str, from: &Path) -> Repo {
    let store = skein_home().join("repos").join(id).join("store/.claude");
    let repo: Repo = serde_json::from_value(serde_json::json!({
        "id": id,
        "source": from.to_string_lossy(),
        "store": store.to_string_lossy(),
        "agent": "claude",
    }))
    .unwrap();
    crate::kit::ensure_store(&store).unwrap();
    ensure_mirror(&repo).unwrap();
    update_repos(|repos| {
        repos.retain(|r| r.id != id);
        repos.push(repo.clone());
        Ok(repo.clone())
    })
    .unwrap()
}

/// Run git in `dir`, with an identity, and refuse to continue if it failed.
pub(super) fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .current_dir(dir)
        .args(args)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@e")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@e")
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {out:?}");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// A git repository with one commit on `master`, at `dir`.
pub(super) fn origin_repo(dir: &Path) {
    fs::create_dir_all(dir).unwrap();
    git(dir, &["init", "-b", "master"]);
    fs::write(dir.join("tracked.txt"), "in git\n").unwrap();
    // The point of the mirror/checkout distinction, in one file: this is in the tree and never
    // in any clone taken from it.
    fs::write(dir.join(".gitignore"), "secret.env\n").unwrap();
    fs::write(dir.join("secret.env"), "KEY=1\n").unwrap();
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-m", "one"]);
}
