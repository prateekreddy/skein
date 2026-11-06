//! What kinds of project a repo is (rust, node, python, go), which decide which of the repo's
//! shared caches its boxes are pointed at.
//!
//! The owner's decision of 2026-11-06 on SKEIN-1373: "one shared compiler cache per repo — but we
//! should be able to select what type a repo is: if it's a JS repo there's no point adding the
//! Rust cache, and vice versa." So a repo has kinds, a repo can have several, and by default they
//! are read off the checkout. The owner can choose them instead, on the repo's card, and the choice
//! is kept in skein's own record of the repo ([`Repo::kinds`]), never in the repo's files.
//!
//! **What a kind decides is caches, not tools.** The launcher points a kind's download caches into
//! the repo's tools area only when the repo has that kind
//! (`src/box-session.sh`, the session block). Where a tool manager installs (`cargo install`,
//! `npm i -g`, `uv tool`, `go install`) is pointed into the area for every repo, so an agent of a
//! node repo that installs a cargo tool anyway still has it, on its PATH, after a restart.

use super::*;

/// Every kind skein knows, each with the files whose presence in a checkout says a repo is one.
///
/// Read at the checkout's top and one directory down, so a repo with its web app in `web/` and
/// nothing at the top is still a node repo. Deeper than that is a vendored or example project far
/// more often than the repo's own, and every kind costs a cache.
pub const KINDS: [(&str, &[&str]); 4] = [
    ("rust", &["Cargo.toml"]),
    ("node", &["package.json"]),
    ("python", &["pyproject.toml", "requirements.txt"]),
    ("go", &["go.mod"]),
];

/// Directories one level down that are never the repo's own project, however many markers they
/// hold: installed packages and build output.
const NOT_THE_REPOS: [&str; 4] = ["node_modules", "target", "vendor", "dist"];

/// The kinds a set of repo-relative file paths makes, in [`KINDS`] order. A path counts when it is
/// a marker at the top (`Cargo.toml`) or one directory down (`web/package.json`), in a directory
/// that is neither hidden nor one of [`NOT_THE_REPOS`].
pub fn kinds_of_files<'a>(paths: impl IntoIterator<Item = &'a str>) -> Vec<String> {
    let mut found = [false; KINDS.len()];
    for path in paths {
        let (dir, file) = match path.split_once('/') {
            None => ("", path),
            Some((dir, file)) if !file.contains('/') => (dir, file),
            Some(_) => continue,
        };
        if dir.starts_with('.') || NOT_THE_REPOS.contains(&dir) {
            continue;
        }
        for (i, (_, markers)) in KINDS.iter().enumerate() {
            found[i] |= markers.contains(&file);
        }
    }
    KINDS
        .iter()
        .zip(found)
        .filter(|(_, f)| *f)
        .map(|((k, _), _)| k.to_string())
        .collect()
}

/// The kinds a checkout on disk makes, or `None` when its top cannot be listed, which is "could
/// not tell" and not "no kind". A link is not followed, at the top or below it: a checkout is the
/// box's, and a link in it can name anything.
pub fn kinds_in_dir(tree: &Path) -> Option<Vec<String>> {
    let top = fs::symlink_metadata(tree).ok()?;
    if !top.is_dir() {
        return None;
    }
    let mut paths: Vec<String> = Vec::new();
    for entry in fs::read_dir(tree).ok()?.flatten() {
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_file() {
            paths.push(name);
        } else if kind.is_dir() {
            for inner in fs::read_dir(entry.path()).into_iter().flatten().flatten() {
                let file = inner.file_type().is_ok_and(|t| t.is_file());
                if let (true, Ok(inner)) = (file, inner.file_name().into_string()) {
                    paths.push(format!("{name}/{inner}"));
                }
            }
        }
    }
    Some(kinds_of_files(paths.iter().map(String::as_str)))
}

/// The kinds the repo's own default branch makes, read from its mirror, for the repo's card.
/// `Err` when the mirror could not be read.
pub fn kinds_in_mirror(repo: &Repo) -> Result<Vec<String>, String> {
    let tree = Tree::open_telling(repo)?;
    let files = tree.files("");
    Ok(kinds_of_files(files.iter().map(String::as_str)))
}

/// A list of kinds as a person or the cockpit gave it, checked: each a kind skein knows, each once,
/// in [`KINDS`] order. Empty is a real answer, a repo that needs none of the caches.
pub fn parse_kinds(words: &[String]) -> Result<Vec<String>, String> {
    let known: Vec<&str> = KINDS.iter().map(|(k, _)| *k).collect();
    for w in words {
        if !known.contains(&w.trim()) {
            return Err(format!(
                "{w:?} is not a kind skein knows: it is one of {}",
                known.join(", ")
            ));
        }
    }
    Ok(known
        .iter()
        .filter(|k| words.iter().any(|w| w.trim() == **k))
        .map(|k| k.to_string())
        .collect())
}

/// Choose this repo's kinds, or with `None` go back to reading them off the checkout
/// ([`Repo::kinds`]). Like every switch read at launch, it reaches a box at its next start.
pub fn set_repo_kinds(id: &str, kinds: Option<&[String]>) -> Result<Repo, String> {
    let kinds = kinds.map(parse_kinds).transpose()?;
    update_repos(|repos| {
        let repo = repos
            .iter_mut()
            .find(|r| r.id == id)
            .ok_or_else(|| format!("no repo with id {id:?}"))?;
        repo.kinds = kinds;
        Ok(repo.clone())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{env_lock, env_pins, tempdir};

    /// **A checkout's kinds are read off the files it holds, at the top and one directory down.**
    ///
    /// What would make it fail: a marker missing from [`KINDS`] (that kind is never detected); a
    /// depth limit dropped (the `deep/a/Cargo.toml` makes rust); `node_modules` or a hidden
    /// directory read as the repo's own (`node_modules/x/package.json` makes node of a go repo);
    /// a link followed out of the checkout.
    #[test]
    fn a_checkouts_kinds_are_read_off_its_files() {
        let dir = tempdir();
        let tree = dir.join("tree");
        for path in [
            "go.mod",
            "web/package.json",
            "tools/requirements.txt",
            "deep/a/Cargo.toml",
            "node_modules/package.json",
            ".hidden/Cargo.toml",
        ] {
            let p = tree.join(path);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(p, "").unwrap();
        }
        let outside = dir.join("outside");
        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join("Cargo.toml"), "").unwrap();
        std::os::unix::fs::symlink(&outside, tree.join("linked")).unwrap();
        assert_eq!(
            kinds_in_dir(&tree).unwrap(),
            ["node", "python", "go"],
            "the kinds read off a checkout are wrong"
        );
        fs::write(tree.join("Cargo.toml"), "").unwrap();
        assert_eq!(
            kinds_in_dir(&tree).unwrap(),
            ["rust", "node", "python", "go"],
            "a Cargo.toml at the top does not make the repo a rust one"
        );
        assert_eq!(
            kinds_in_dir(&dir.join("absent")),
            None,
            "a checkout that is not there reads as having no kind rather than as unknown"
        );
        let empty = dir.join("empty");
        fs::create_dir_all(&empty).unwrap();
        assert_eq!(kinds_in_dir(&empty), Some(Vec::new()));
        assert_eq!(
            kinds_of_files(["pyproject.toml", "a/b/go.mod", ".cargo/Cargo.toml"]),
            ["python"],
            "the same rule over a mirror's file list"
        );
    }

    /// **The owner's choice of kinds is kept in skein's record of the repo, checked, and clearing
    /// it goes back to detection.**
    ///
    /// What would make it fail: an unknown word stored (it would export nothing and read as
    /// chosen); a choice that does not survive a reload; `None` storing an empty list, which is the
    /// different answer "no kind at all".
    #[test]
    fn the_owners_kinds_are_kept_and_checked() {
        let _env = env_lock();
        let home = tempdir();
        let mut env = env_pins();
        env.set("SKEIN_HOME", &home)
            .set("SKEIN_FLEET_ROOT", home.join("fleet"));
        save_repos(&[Repo {
            id: "web".into(),
            ..Default::default()
        }])
        .unwrap();
        assert_eq!(
            load_repos()[0].kinds,
            None,
            "a new repo has chosen no kinds"
        );
        let words = |w: &[&str]| w.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert!(set_repo_kinds("web", Some(&words(&["ruby"]))).is_err());
        assert_eq!(load_repos()[0].kinds, None, "a refused choice was stored");
        set_repo_kinds("web", Some(&words(&["node", "rust", "node"]))).unwrap();
        assert_eq!(
            load_repos()[0].kinds,
            Some(words(&["rust", "node"])),
            "a choice of kinds did not survive a reload in order and once each"
        );
        set_repo_kinds("web", Some(&[])).unwrap();
        assert_eq!(load_repos()[0].kinds, Some(Vec::new()));
        set_repo_kinds("web", None).unwrap();
        assert_eq!(
            load_repos()[0].kinds,
            None,
            "clearing did not go back to detection"
        );
    }
}
