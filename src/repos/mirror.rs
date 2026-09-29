//! A repo's bare mirror on the volume: making it, cloning it, fetching it and one pull
//! request's head into it, and reading a repo's files out of it.

use super::*;

/// A repo's **bare mirror** on the volume — `~/.skein/repos/<id>/mirror`.
///
/// The distinction this whole module now turns on: a *mirror* is a remote, and a *checkout* is
/// somebody's working tree. They were the same directory, and the checkout was the one boxes cloned
/// from, which made three separate things true at once and none of them on purpose:
///
///   * a box's clone inherited whatever branch happened to be checked out on the host;
///   * the host's own working tree had to be mounted into the sandbox for a box to clone at all,
///     so every box could read the tree its user works in;
///   * `~/.skein/repos/<id>/work` was a full second checkout on the volume for URL repos — the
///     volume carrying a working tree it never works in.
///
/// Bare, deliberately. A non-bare mirror has a checked-out branch that means nothing and clones
/// inherit, and it would *look* like it solved the gitignored-shared-paths problem while solving
/// nothing: a clone of any shape carries tracked files only, so the files that block is for are
/// absent from a mirror however it is made. Those files reach a box from the repo's store instead,
/// which is a different thing from this — the box bootstrap surfaces whatever `shared-paths.txt`
/// names there (`src/store/sandbox-bootstrap.sh`).
pub fn mirror_path(id: &str) -> PathBuf {
    skein_home().join("repos").join(id).join("mirror")
}

/// Is there a mirror at `path` — a git repository rather than an empty or half-made directory?
///
/// `HEAD` and `objects/`, because a `git clone --mirror` interrupted partway leaves the directory
/// and some of its contents behind, and a mirror that exists but has no objects fails every clone
/// taken from it with a message about the *box*.
pub(super) fn mirror_is_made(path: &Path) -> bool {
    path.join("HEAD").is_file() && path.join("objects").is_dir()
}

/// Make sure this repo has a mirror, cloning one if it has none. Returns its path.
///
/// Cloned from `source`, which [`add_repo`] has already refused unless it is a remote. There used
/// to be a local checkout to copy from instead — cheaper than a trip over the network — and it went
/// with local-path repos; see [`clone_mirror`].
///
/// Idempotent, and the reason it is a function rather than a step in [`add_repo`]: every repo
/// registered before mirrors existed has none, and the alternative to making one on demand is a
/// migration that has to run before anything else works.
pub fn ensure_mirror(repo: &Repo) -> Result<PathBuf, String> {
    ensure_mirror_at_add(repo, None, false)
}

/// [`ensure_mirror`], as the add of a repo runs it: `adding` words a refusal for the add dialog,
/// and `given` is the token the person chose there, pasted or shared, which the clone uses in
/// place of any stored one — see [`fleet_git_at_add`].
pub(super) fn ensure_mirror_at_add(
    repo: &Repo,
    given: Option<(&crate::secret::Secret, &str)>,
    adding: bool,
) -> Result<PathBuf, String> {
    let mirror = mirror_path(&repo.id);
    if mirror_is_made(&mirror) {
        return Ok(mirror);
    }
    // **One clone between all callers, or neither of them gets one.**
    //
    // Two readers that both find no mirror both ran `git clone --mirror` into the same directory,
    // and git refuses the second — `fatal: cannot copy .../templates/description: File exists` —
    // so BOTH failed and the repo was left with no mirror at all. That is not a transient: every
    // caller reads `None` as "this repo has nothing to say" rather than "I could not look", so the
    // PR brief silently stopped attributing ownership and the notes pane offered no modules. And
    // `review::summarise` caches its answer against the head sha, so one race froze a wrong summary
    // for the life of that commit. Found by `tests/ui/review.mjs`, which read as flaky because a
    // later call, alone, made the mirror and the checks after it passed.
    //
    // The half-made-mirror repair below made it sharper rather than safer: on a second attempt one
    // caller `remove_dir_all`s the directory another is mid-clone into.
    //
    // The lock file sits BESIDE the mirror, not in it, because the repair deletes the directory.
    let guard = mirror
        .parent()
        .ok_or_else(|| format!("{} has no parent to lock", mirror.display()))?
        .join(".mirror.lock");
    crate::util::with_lock(&guard, || clone_mirror(repo, &mirror, given, adding))
}

/// Can the token chosen at add time reach this repo's remote? For the add of a repo whose mirror
/// is already on disk — one removed and added again keeps its files — where no clone runs, and a
/// token would otherwise be saved without ever having been shown to the remote.
///
/// `git ls-remote`, the cheapest thing that makes the remote ask for a credential and the same
/// wiring the clone uses, so it is refused in the same words.
pub(super) fn reach_at_add(
    repo: &Repo,
    given: (&crate::secret::Secret, &str),
) -> Result<(), String> {
    let from = repo.source.trim();
    let mut command = Command::new("git");
    let auth = fleet_git_at_add(&mut command, repo, Some(given));
    command.args(["ls-remote", "--heads", "--", from]);
    let out = bounded_output(&mut command, "git ls-remote", Duration::from_secs(60))?;
    match out.status.success() {
        true => Ok(()),
        false => Err(git_refusal(
            &format!("reaching {from}"),
            &String::from_utf8_lossy(&out.stderr),
            &auth,
        )),
    }
}

/// The clone itself, with the lock in [`ensure_mirror`] already held.
fn clone_mirror(
    repo: &Repo,
    mirror: &Path,
    given: Option<(&crate::secret::Secret, &str)>,
    adding: bool,
) -> Result<PathBuf, String> {
    // Asked again under the lock. The caller that held it may have been making exactly this mirror,
    // and cloning over a finished one is the collision this function exists to stop.
    if mirror_is_made(mirror) {
        return Ok(mirror.to_path_buf());
    }
    // The remote, and only ever the remote. This used to prefer an adopted repo's checkout because
    // cloning from it was a local copy rather than a trip over the network; there is no checkout to
    // prefer any more, and `source` is a URL by construction (see [`add_repo`]).
    let from = repo.source.trim();
    if from.is_empty() {
        return Err(format!("{} has nothing to mirror from", repo.id));
    }
    // A half-made mirror from an interrupted clone: git refuses to clone into a non-empty directory,
    // so it would fail here for ever. Nothing in it is anybody's only copy — it is objects that
    // exist in the remote it was made from.
    if mirror.exists() {
        fs::remove_dir_all(mirror).map_err(|e| format!("clearing a half-made mirror: {e}"))?;
    }
    fs::create_dir_all(mirror.parent().unwrap()).map_err(|e| format!("mkdir: {e}"))?;
    let mut command = Command::new("git");
    // Never a terminal, and the fleet's own credential if it has one (SKEIN-951) — or, when a repo
    // is being added, the token the person chose for it (SKEIN-1231).
    let auth = match adding {
        true => fleet_git_at_add(&mut command, repo, given),
        false => fleet_git(&mut command, repo),
    };
    // `--` before the two positionals, so a `source` beginning with `-` is a repository name git
    // cannot find rather than an option git obeys. Belt to `registrable_source`'s braces, and worth
    // saying what it is NOT: with the argv this builds, an injected option was *not* exploitable —
    // it steals the repository positional, `mirror` becomes the repository, and git dies with
    // "repository … does not exist" before the option can act. Checked at git 2.53.0, both
    // `--upload-pack=<cmd>` and `-u<cmd>`. That is one argument order away from being untrue, and
    // the separator costs nothing.
    command.args(["clone", "--mirror", "--", from]).arg(mirror);
    let out = bounded_output(&mut command, "git clone --mirror", Duration::from_secs(300))?;
    if !out.status.success() {
        let _ = fs::remove_dir_all(mirror);
        return Err(git_refusal(
            &format!("mirroring {from}"),
            &String::from_utf8_lossy(&out.stderr),
            &auth,
        ));
    }
    // Point it at where the code really comes from. A URL repo's mirror fetches from the URL; an
    // older path-registered entry's fetches from the directory it was made from, which is the only
    // source such a repo has.
    let origin = match is_git_url(&repo.source) {
        true => repo.source.trim().to_string(),
        false => from.to_string(),
    };
    let mut set = Command::new("git");
    set.arg("-C")
        .arg(mirror)
        .args(["remote", "set-url", "origin", &origin]);
    let _ = bounded_output(&mut set, "git remote set-url", Duration::from_secs(10));
    // **Pack it, once, before any box ever clones from it** (SKEIN-406).
    //
    // `from` is usually a working CHECKOUT, and cloning over git's local transport copies that
    // checkout's object store as it stands — loose objects and all. A checkout somebody has been
    // committing in for months carries thousands of them, and every box that clones this mirror
    // then copies every one, across the sandbox boundary, for ever.
    //
    // Measured on this repository (2026-08-26): the mirror arrived at 88 MB — 4,394 loose objects
    // holding 85 MB of it, against 1 MB actually in a pack. One `git gc` took 1.4s and left 6 MB.
    // A clone from it went from 154 ms and 88 MB of `.git` to 36 ms and 6 MB: four times faster,
    // 93% less data, and 82 MB off the volume permanently.
    //
    // **This is the fix instead of `--shared` or `--reference`, and the measurement is why.** On a
    // packed mirror `--shared` saves nothing at all (36 ms either way) while making every box's
    // objects depend on this directory surviving — and `ensure_mirror` DELETES it above to repair a
    // half-made clone. `--reference … --dissociate` measured 68 ms, nearly twice a plain clone.
    // Neither risk buys anything once the loose objects are gone.
    //
    // Best-effort: a mirror that could not be packed is a slower mirror, not a broken one, and
    // failing the clone over housekeeping would turn a working repo into no repo.
    let mut pack = Command::new("git");
    pack.arg("-C").arg(mirror).args(["gc", "--quiet"]);
    let _ = bounded_output(&mut pack, "git gc", Duration::from_secs(300));
    Ok(mirror.to_path_buf())
}

/// Fetch the mirror from its origin, pruning **branches and tags** the origin no longer has.
///
/// `--prune` matters more here than in a checkout: a mirror keeps every branch, so without it a
/// branch deleted upstream a year ago is still offered to every box that clones from this.
///
/// **What prune may reach is the whole of SKEIN-466, and it is spelled out on the command line
/// rather than left to the mirror's config.** A `git clone --mirror` configures `+refs/*:refs/*`,
/// and `git remote update --prune` under that refspec deletes *every* ref the origin lacks — which
/// for a mirror repointed from a checkout at a real remote is `refs/sandboxes/*`, `refs/stash` and
/// `refs/remotes/*`, none of which any origin carries. That is not hypothetical: repointing skein's
/// own mirror on 2026-08-28 deleted four refs, and `refs/stash` (`5c3a2fe`, a WIP from 2026-08-05)
/// was reachable from nothing else. `gc --auto` runs below, so those objects were on a countdown
/// rather than merely unreferenced.
///
/// Refspecs given here decide what prune considers, so the narrow pair keeps the reason `--prune`
/// is here — a branch genuinely deleted upstream still goes, and so does a deleted tag — while a
/// ref outside `refs/heads/` and `refs/tags/` is no longer prune's to delete. Verified against git
/// directly before the change: with these refspecs a deleted branch and a deleted tag are pruned
/// and `refs/sandboxes/x`, `refs/stash` and `refs/remotes/foo/bar` survive; with
/// `remote update --prune` the same three are deleted.
///
/// Errors are returned rather than swallowed, and the callers decide. A box created while the
/// network is down should still be created — from a mirror that is a day old — and a `skein pull`
/// that could not reach the remote should say so.
pub fn fetch_mirror(repo: &Repo) -> Result<(), String> {
    let mirror = ensure_mirror(repo)?;
    let mut command = Command::new("git");
    // Never a terminal, and the fleet's own credential if it has one (SKEIN-951).
    let auth = fleet_git(&mut command, repo);
    command.arg("-C").arg(&mirror).args([
        "fetch",
        "--prune",
        "origin",
        "+refs/heads/*:refs/heads/*",
        "+refs/tags/*:refs/tags/*",
    ]);
    let out = bounded_output(&mut command, "git fetch --prune", Duration::from_secs(300))?;
    if !out.status.success() {
        return Err(git_refusal(
            &format!("fetching {}", repo.id),
            &String::from_utf8_lossy(&out.stderr),
            &auth,
        ));
    }
    // Keep it packed as it grows (SKEIN-406). `--auto` rather than a plain `gc`: it is a no-op
    // below git's own threshold, so an idle repo pays nothing and a busy one is tidied without
    // skein deciding how often that should be. The one-off cost is at creation, above, where the
    // mirror inherits a checkout's whole loose object store in a single copy.
    //
    // After the fetch and not before, and never in a way that can fail it: what a caller asked for
    // is the new commits, and a repo that could not be packed still has them.
    let mut pack = Command::new("git");
    pack.arg("-C")
        .arg(&mirror)
        .args(["gc", "--auto", "--quiet"]);
    let _ = bounded_output(&mut pack, "git gc --auto", Duration::from_secs(120));
    Ok(())
}

/// Bring ONE pull request's head into the mirror — the ref `fetch_mirror` deliberately does not ask
/// for.
///
/// **A pull request from a fork is in no `refs/heads/*` of the base repository.** Its commits live
/// in the contributor's own repository, and the only place the base repo can serve them from is
/// `refs/pull/<n>/head`, which GitHub maintains and [`fetch_mirror`]'s refspec —
/// `+refs/heads/*` and `+refs/tags/*` — does not fetch. So a fork's pull request could not be stood
/// up at all: `review::stand_the_change_up` checked the head out, failed, fetched, failed again,
/// and answered "nothing here". Correct rather than wrong — a reviewer handed the base branch and
/// told it is the change is the worst outcome that path has — but it meant the reviewer read a
/// diff where it could have read a tree.
///
/// **One ref, on demand, and never in the mirror's own refspec.** Adding `+refs/pull/*` to
/// [`fetch_mirror`] would drag every pull request ever opened into every repo's mirror on every
/// fetch, for ever; `gadget-demo` on a live fleet is past 700 alone. This asks for the one pull
/// request something is about to read, and only when its head is not already reachable — which for
/// a same-repo pull request it always is, because that branch IS in `refs/heads/*`.
///
/// Fetched into the mirror rather than into the reader's checkout on purpose: the mirror is the one
/// place in skein that talks to the remote and the one place its credentials are arranged, so a
/// second door onto GitHub would be a second thing to authenticate and to get wrong. The checkout
/// then takes it from the mirror, which is the hop it already makes for everything else.
pub fn fetch_pull_head(repo: &Repo, number: u64) -> Result<String, String> {
    let mirror = ensure_mirror(repo)?;
    let refspec = pull_head_ref(number);
    let mut command = Command::new("git");
    // Never a terminal, and the fleet's own credential if it has one (SKEIN-954). The third
    // fleet-side network git, and the one SKEIN-951 left out: a fork's pull-request head comes from
    // nowhere else, so a private base repo with a missing or expired token hung the REVIEWER here
    // on `Username for 'https://github.com':` exactly as `skein start` hung in `fetch_mirror`.
    let auth = fleet_git(&mut command, repo);
    command
        .arg("-C")
        .arg(&mirror)
        .args(["fetch", "--quiet", "origin"])
        .arg(format!("+{refspec}:{refspec}"));
    let out = bounded_output(
        &mut command,
        "git fetch pull head",
        Duration::from_secs(300),
    )?;
    if !out.status.success() {
        return Err(git_refusal(
            &format!("fetching {refspec} of {}", repo.id),
            &String::from_utf8_lossy(&out.stderr),
            &auth,
        ));
    }
    Ok(refspec)
}

/// Where GitHub keeps a pull request's head, whoever opened it.
///
/// Its own function because two files name this ref and a ref spelled two ways is a ref that works
/// in one of them: the mirror fetches it here, and the reader fetches the same string out of the
/// mirror.
pub fn pull_head_ref(number: u64) -> String {
    format!("refs/pull/{number}/head")
}

/// A repo's files, read out of its **mirror** rather than off somebody's disk.
///
/// Three host-side features — the diff, the module notes and CODEOWNERS — read `repo.source_tree`
/// directly, which is a working checkout: it has a branch somebody chose, edits nobody committed,
/// and in-fleet it is not reachable at all. What they actually want is "this repo, as committed",
/// and that is what a mirror holds.
///
/// `HEAD` is the ref, and it is the right one without asking anybody: a `--mirror` clone takes the
/// origin's `HEAD`, so this is the remote's own default branch rather than whatever the person at
/// the keyboard has checked out.
///
/// Reading through git rather than off disk changes one thing worth saying: uncommitted work is
/// invisible. For a note about what a module *is*, and for the ownership rules a repo has agreed
/// on, that is the answer that was wanted anyway.
pub struct Tree {
    mirror: PathBuf,
}

impl Tree {
    /// The repo's tree, or `None` when it could not be read — with the reason kept only in the
    /// server log.
    ///
    /// A convenience for callers whose behaviour is already absence-shaped: they do less, safely,
    /// whichever of the two `None` means. A caller that must *tell* "this repo has nothing" from
    /// "I could not look" reads [`Tree::open_telling`] instead — those are different sentences on
    /// screen, and only one of them may be cached (SKEIN-117).
    pub fn open(repo: &Repo) -> Option<Tree> {
        Tree::open_telling(repo)
            .map_err(|why| eprintln!("skein: reading {}: {why}", repo.id))
            .ok()
    }

    /// The repo's tree, or the reason it could not be read.
    ///
    /// [`crate::health::Level`]'s register, applied to reading a repo. `Ok` is *checked*: the
    /// mirror answered, so an absence found through it — no CODEOWNERS, no such file, no modules
    /// — is the repo's own answer and may be acted on and remembered. `Err` is *could not be
    /// checked*: nothing about the repo was learned, and the honest response to "I could not
    /// tell" is to say so and wait — render it as "skein could not read this repo", never as
    /// "nothing there", and never store a decision made while blind. `Unknown` may never drive a
    /// doer.
    pub fn open_telling(repo: &Repo) -> Result<Tree, String> {
        let mirror = ensure_mirror(repo)?;
        // A mirror whose HEAD resolves to nothing answers nothing, and every call below would
        // fail one at a time rather than once here. An empty repository and a mirror that did
        // not survive its clone both land in this probe, and neither can support "the repo has
        // no X" — so both read as could-not-read, the direction that widens attention rather
        // than narrowing it.
        let tree = Tree { mirror };
        if tree.git(&["rev-parse", "--verify", "HEAD"]).is_none() {
            return Err(format!(
                "the mirror at {} has no readable HEAD — an empty repository, or a mirror that \
                 did not survive its clone",
                tree.mirror.display()
            ));
        }
        Ok(tree)
    }

    fn git(&self, args: &[&str]) -> Option<String> {
        let mut command = Command::new("git");
        command.arg("-C").arg(&self.mirror).args(args);
        let out = bounded_output(&mut command, "git", Duration::from_secs(30)).ok()?;
        out.status
            .success()
            .then(|| String::from_utf8_lossy(&out.stdout).to_string())
    }

    /// One file's contents, or `None` if the tree has no such file.
    pub fn read(&self, path: &str) -> Option<String> {
        self.git(&["show", &format!("HEAD:{path}")])
    }

    /// Every file under `prefix` (the whole tree when it is empty), repo-relative.
    pub fn files(&self, prefix: &str) -> Vec<String> {
        let mut args = vec!["ls-tree", "-r", "--name-only", "-z", "HEAD"];
        if !prefix.is_empty() {
            args.push("--");
            args.push(prefix);
        }
        // NUL-separated: a path with a newline in it is legal in git and would otherwise split into
        // two files that do not exist.
        self.git(&args)
            .unwrap_or_default()
            .split('\0')
            .filter(|p| !p.is_empty())
            .map(str::to_string)
            .collect()
    }

    /// The directories directly under the tree's root.
    pub fn top_level_dirs(&self) -> Vec<String> {
        self.git(&["ls-tree", "--name-only", "-z", "-d", "HEAD"])
            .unwrap_or_default()
            .split('\0')
            .filter(|p| !p.is_empty())
            .map(|p| p.trim_end_matches('/').to_string())
            .collect()
    }

    /// Is there a directory at this path?
    pub fn is_dir(&self, path: &str) -> bool {
        let path = path.trim_matches('/');
        !path.is_empty()
            && self
                .git(&["ls-tree", "-d", "--name-only", "HEAD", path])
                .map(|out| !out.trim().is_empty())
                .unwrap_or(false)
    }

    /// How many **lines of the tree mention** a symbol.
    ///
    /// Deliberately not "call sites", and the difference is the whole honesty of the number: `git
    /// grep` cannot tell a call from a comment, a string, an unrelated field of the same name, or
    /// the declaration itself. What it can say is how widely the word appears, which is a real
    /// measure of how far a change reaches and is not a claim about calls. §11.1's mock-up says
    /// "call sites"; this says mentions, because that is what can be derived rather than asserted.
    ///
    /// Whole words only (`-w`) and a fixed string (`-F`), so `check` does not match `checked` and a
    /// symbol containing regex punctuation is not a pattern. Short symbols are refused outright: a
    /// two-character name matches everything and the count would be noise wearing a number.
    ///
    /// `None` means *nothing was counted* — the tree could not be searched, or the symbol was not
    /// worth searching for. It never means zero. A caller must render the two differently: "0
    /// mentions" reads as "nothing uses this", which is the opposite of "we did not look".
    pub fn mentions(&self, symbol: &str) -> Option<usize> {
        let symbol = symbol.trim();
        if symbol.len() < 3 || symbol.chars().any(|c| c.is_whitespace()) {
            return None;
        }
        // Its own invocation rather than `git()`, because here a **failure exit is an answer**:
        // `git grep` exits 1 when it matched nothing, and `git()` reports that as "no output" —
        // indistinguishable from a tree it could not read. Anything above 1 is a real error.
        let mut command = Command::new("git");
        command
            .arg("-C")
            .arg(&self.mirror)
            .args(["grep", "-I", "-F", "-w", "-c", "-e", symbol, "HEAD"]);
        let out = bounded_output(&mut command, "git grep", Duration::from_secs(30)).ok()?;
        match out.status.code() {
            Some(0) => {}
            Some(1) => return Some(0),
            _ => return None,
        }
        // `HEAD:path/to/file:7` — the count is after the last colon, and a path may contain one.
        Some(
            String::from_utf8_lossy(&out.stdout)
                .lines()
                .filter_map(|l| l.rsplit(':').next()?.trim().parse::<usize>().ok())
                .sum(),
        )
    }

    /// The commit that last touched `path`, or empty when git cannot answer.
    pub fn last_commit(&self, path: &str) -> String {
        self.git(&["log", "-1", "--format=%H", "HEAD", "--", path])
            .unwrap_or_default()
            .trim()
            .to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repos::testkit::*;
    use crate::testutil::{env_lock, env_pins, tempdir};

    /// **The mirror a box clones from is PACKED** (SKEIN-406), and that is the whole of why a box
    /// creation stopped copying a repository's history byte by byte.
    ///
    /// A mirror is made with `git clone --mirror` from a working CHECKOUT, over git's local
    /// transport, which copies that checkout's object store exactly as it stands — loose objects
    /// and all. A checkout somebody has been committing in for months carries thousands, and every
    /// box that clones the mirror then copies every one of them across the sandbox boundary.
    ///
    /// Measured on this repository (2026-08-26): the mirror arrived at 88 MB, 4,394 loose objects
    /// holding 85 MB of it against 1 MB in a pack. One `git gc` took 1.4s and left 6 MB, and a
    /// clone from it went from 154 ms / 88 MB to 36 ms / 6 MB.
    ///
    /// The counter-half matters as much: on a packed mirror `--shared` measured 36 ms against a
    /// plain clone's 36 ms — no saving at all, in exchange for every box's objects depending on
    /// this directory, which `ensure_mirror` deletes to repair a half-made clone. So this asserts
    /// the loose objects are gone, NOT that a flag is present.
    #[test]
    fn the_mirror_a_box_clones_from_carries_no_loose_objects() {
        let _g = env_lock();
        let home = tempdir();
        let mut env = env_pins();
        env.set("SKEIN_HOME", &home);

        let checkout = tempdir();
        origin_repo(&checkout);
        // Commits made one at a time, which is how a person makes them and how the loose objects
        // that cost the box its clone accumulate. Committing once would pack nothing and prove
        // nothing.
        for i in 0..12 {
            fs::write(checkout.join(format!("f{i}.txt")), format!("{i}\n")).unwrap();
            git(&checkout, &["add", "-A"]);
            git(&checkout, &["commit", "-m", &format!("c{i}")]);
        }
        assert!(
            loose_objects(&checkout.join(".git")) > 0,
            "the fixture checkout has no loose objects, so it cannot show that the mirror \
             inherited any — the thing under test never happens"
        );

        registered("proj", &checkout);

        let mirror = mirror_path("proj");
        assert_eq!(
            loose_objects(&mirror),
            0,
            "the mirror carries loose objects inherited from the checkout it was made from, so \
             every box that clones it copies each one across the sandbox boundary — measured at \
             85MB of a 88MB mirror on the skein repo itself"
        );
        // And it is still a mirror afterwards: packing must not cost the refs a box clones.
        assert!(
            mirror_is_made(&mirror),
            "packing the mirror left something that is no longer a mirror"
        );
        assert!(
            !git(&mirror, &["rev-parse", "HEAD"]).is_empty(),
            "the packed mirror has no HEAD to clone"
        );
    }

    /// How many objects are sitting loose in a git directory, asked of git rather than counted by
    /// walking `objects/` — the layout is git's to change and the question is not.
    fn loose_objects(gitdir: &Path) -> u64 {
        git(gitdir, &["count-objects", "-v"])
            .lines()
            .find_map(|l| l.strip_prefix("count: "))
            .and_then(|n| n.trim().parse().ok())
            .unwrap_or_default()
    }

    /// A registered repo has a mirror, the mirror is bare, and it is what a box clones from.
    ///
    /// Bare is the part worth asserting. A non-bare mirror carries a checked-out branch that every
    /// clone inherits and a working tree that is nobody's — and it would read as the safer choice,
    /// because `$mirror/<path>` would then find tracked files and look like it had solved the
    /// gitignored-shared-paths problem it has not touched.
    #[test]
    fn a_repo_is_mirrored_and_the_mirror_is_what_a_box_clones_from() {
        let _g = env_lock();
        let home = tempdir();
        let mut env = env_pins();
        env.set("SKEIN_HOME", &home);

        let checkout = tempdir();
        origin_repo(&checkout);
        let repo = registered("proj", &checkout);

        let mirror = mirror_path("proj");
        assert!(mirror_is_made(&mirror), "no mirror at {}", mirror.display());
        assert!(
            !mirror.join(".git").exists(),
            "the mirror has a working tree, so it is a checkout wearing the name"
        );
        assert_eq!(git(&mirror, &["config", "--get", "core.bare"]), "true");
        assert_eq!(
            crate::fleet::clone_source(&repo),
            mirror.to_string_lossy(),
            "a box must clone from the mirror, not from anybody's checkout"
        );
        // And its origin is where the code really comes from, so a fetch goes to the right place.
        assert_eq!(
            crate::util::resolved(&remote_origin_url(&mirror.to_string_lossy()).unwrap()),
            crate::util::resolved(&checkout.to_string_lossy()),
            "a mirror fetches from the source its repo was registered with"
        );

        // A clone of it carries the tracked file and, by construction, not the gitignored one.
        let clone = tempdir();
        let dst = clone.join("box");
        git(
            &clone,
            &["clone", &mirror.to_string_lossy(), &dst.to_string_lossy()],
        );
        assert_eq!(
            fs::read_to_string(dst.join("tracked.txt")).unwrap(),
            "in git\n"
        );
        assert!(
            !dst.join("secret.env").exists(),
            "a gitignored file cannot come out of a mirror, which is why those files reach a box \
             from the repo's store instead"
        );
    }

    /// Two readers of a repo with no mirror yet get one mirror between them, not neither.
    ///
    /// Both used to run `git clone --mirror` into the same directory and git refused the second —
    /// `fatal: cannot copy .../templates/description: File exists` — so **both** failed and the repo
    /// was left with nothing. That is not a transient, because of what the callers do with `None`:
    /// `review::ownership` returns `(vec![], 0)`, which its own doc says is deliberately
    /// indistinguishable from "this repo has no CODEOWNERS", and `moduledocs::modules` returns an
    /// empty list. So a PR brief quietly stopped saying which half of a change was yours. And
    /// `review::summarise` caches against the head sha, so one race froze that answer for the life
    /// of the commit.
    ///
    /// **What this test can and cannot see.** It asserts the outcome — every caller gets a usable
    /// mirror — because "both failed" IS the bug, and against the unlocked code it fails reliably at
    /// this width. It cannot prove only one clone RAN; a version that happened to serialise would
    /// pass. Counting the clones would mean putting a wrapper `git` on `$PATH`, which is
    /// process-global and would break every sibling test that shells out — the same trap
    /// `a_missing_tool_is_one_fault_and_not_five` records. The re-check under the lock is what makes
    /// the count one, and it is one line above this comment's subject.
    #[test]
    fn two_readers_of_a_new_repo_make_one_mirror_between_them() {
        let _g = env_lock();
        let home = tempdir();
        let mut env = env_pins();
        env.set("SKEIN_HOME", &home);

        let checkout = tempdir();
        origin_repo(&checkout);
        // Registered WITHOUT going through `add_repo`, which would make the mirror as a side effect
        // and leave nothing for the racing readers to do.
        let repo = Repo {
            read_prs: false,
            id: "proj".into(),
            source: checkout.to_string_lossy().to_string(),
            store: home.join("store").to_string_lossy().to_string(),
            plane_project: String::new(),
            sync_connection: String::new(),
            review_queue: true,
            sync_gateway_url: String::new(),
            ..Default::default()
        };
        assert!(
            !mirror_is_made(&mirror_path("proj")),
            "the fixture already has a mirror, so this races nothing"
        );

        let outcomes: Vec<Result<PathBuf, String>> = std::thread::scope(|s| {
            let handles: Vec<_> = (0..8).map(|_| s.spawn(|| ensure_mirror(&repo))).collect();
            handles.into_iter().map(|h| h.join().unwrap()).collect()
        });

        let failed: Vec<&String> = outcomes.iter().filter_map(|r| r.as_ref().err()).collect();
        assert!(
            failed.is_empty(),
            "{} of 8 concurrent readers were left with no mirror: {failed:?}",
            failed.len()
        );
        // And what they were handed is a mirror rather than a directory that exists.
        let mirror = mirror_path("proj");
        assert!(mirror_is_made(&mirror), "no mirror at {}", mirror.display());
        assert!(
            Tree::open(&repo).is_some_and(|t| t.read("tracked.txt").is_some()),
            "the mirror every caller got cannot answer for the tree"
        );
    }

    /// A repo that cannot be read says why, distinguishably from a repo with nothing to say.
    ///
    /// `Tree::open` folds both into `None`, and every caller used to read that as "nothing there":
    /// `review::ownership` answered "you own none of this" and `moduledocs::modules` answered
    /// "this repo has no modules" about a repo skein could not look at (SKEIN-117). The telling
    /// form is the seam those callers now stand on, so what it promises is pinned here: an
    /// unreadable repo is `Err` with a reason, and a readable one whose files are then absent is
    /// `Ok` — an absence found *through* the tree, which is the repo's own answer.
    #[test]
    fn a_repo_that_cannot_be_read_is_told_apart_from_one_with_nothing_to_say() {
        let _g = env_lock();
        let home = tempdir();
        let mut env = env_pins();
        env.set("SKEIN_HOME", &home);

        let checkout = tempdir();
        let repo = Repo {
            read_prs: false,
            id: "proj".into(),
            source: checkout.to_string_lossy().to_string(),
            store: home.join("store").to_string_lossy().to_string(),
            plane_project: String::new(),
            sync_connection: String::new(),
            review_queue: true,
            sync_gateway_url: String::new(),
            ..Default::default()
        };

        // No mirror, and none can be made: could-not-read, with the reason carried out.
        let why = match Tree::open_telling(&repo) {
            Err(why) => why,
            Ok(_) => panic!("a repo with nothing to mirror from was reported as readable"),
        };
        assert!(!why.is_empty(), "could-not-read must say why");
        assert!(
            Tree::open(&repo).is_none(),
            "the convenience form stays absence-shaped"
        );

        // The same repo, now readable: `Ok`, and a file it does not have is an absence found
        // through the tree — the repo's own answer, not a failure to look.
        origin_repo(&checkout);
        // The mirror is what `Tree` reads, so making it is what makes the repo readable — this used
        // to also assign a `source_tree`, back when a checkout was a thing a repo could have.
        crate::repos::ensure_mirror(&repo).unwrap();
        fetch_mirror(&repo).unwrap();
        let tree = Tree::open_telling(&repo).expect("a made mirror must open");
        assert!(tree.read("tracked.txt").is_some());
        assert!(tree.read("no-such-file").is_none());
    }

    /// The mirror advances when it is fetched, and not before.
    ///
    /// This is what replaced "every box clones from the host checkout, so the checkout's freshness
    /// is what a box starts from". The staleness did not disappear — it moved somewhere with a name
    /// and a command that advances it.
    #[test]
    fn the_mirror_is_as_fresh_as_its_last_fetch() {
        let _g = env_lock();
        let home = tempdir();
        let mut env = env_pins();
        env.set("SKEIN_HOME", &home);

        let checkout = tempdir();
        origin_repo(&checkout);
        let repo = registered("proj", &checkout);
        let mirror = mirror_path("proj");

        git(&checkout, &["branch", "release"]);
        let branches = |()| git(&mirror, &["branch", "--list", "release"]);
        assert_eq!(
            branches(()),
            "",
            "a mirror that had never fetched knew about a branch made after it"
        );
        fetch_mirror(&repo).unwrap();
        assert!(
            branches(()).contains("release"),
            "the fetch did not bring the new branch across"
        );

        // And a mirror deleted underneath skein is remade rather than reported.
        fs::remove_dir_all(&mirror).unwrap();
        assert!(!mirror_is_made(&mirror));
        ensure_mirror(&repo).unwrap();
        assert!(mirror_is_made(&mirror));
    }

    /// A repo registered from a URL gets a mirror and no checkout.
    ///
    /// It used to get both: `git clone <url>` into `repos/<id>/work`, a full working tree on the
    /// volume that nothing in a box could see and skein only read for host-side conveniences —
    /// every one of which the mirror answers, and answers about the committed state rather than
    /// about whatever branch that clone happened to be left on.
    ///
    /// The URL here is a bare repository on disk, because a path ending in `.git` IS a git URL and
    /// git is content to clone one. That is the same code path a `https://` source takes, without
    /// a test that needs the network to pass.
    #[test]
    fn a_repo_added_from_a_url_gets_a_mirror_and_no_checkout() {
        let _g = env_lock();
        let home = tempdir();
        let mut env = env_pins();
        env.set("SKEIN_HOME", &home);

        let elsewhere = tempdir();
        let checkout = elsewhere.join("built-from");
        origin_repo(&checkout);
        let upstream = elsewhere.join("upstream.git");
        git(
            &elsewhere,
            &[
                "clone",
                "--bare",
                "-q",
                &checkout.to_string_lossy(),
                &upstream.to_string_lossy(),
            ],
        );
        assert!(
            is_git_url(&upstream.to_string_lossy()),
            "the fixture is not exercising the URL path"
        );

        let repo = registered("proj", &upstream);

        assert!(
            !skein_home().join("repos/proj/work").exists(),
            "the volume still carries a working tree skein never works in"
        );
        let mirror = mirror_path("proj");
        assert!(mirror_is_made(&mirror));
        assert_eq!(git(&mirror, &["config", "--get", "core.bare"]), "true");
        assert_eq!(
            crate::fleet::clone_source(&repo),
            mirror.to_string_lossy(),
            "a box must still have somewhere to clone from"
        );
        // The mirror fetches from the URL, and that is also where a box pushes — for a URL repo the
        // two questions have the same answer, which is exactly why they had to be separated for the
        // path-source case below.
        assert_eq!(
            remote_origin_url(&mirror.to_string_lossy()).unwrap(),
            upstream.to_string_lossy()
        );
        assert_eq!(
            repo_origin_url(&repo).unwrap(),
            upstream.to_string_lossy(),
            "a box would push somewhere other than the repository it came from"
        );

        // A commit made upstream reaches the mirror, with no checkout in between.
        git(&checkout, &["checkout", "-q", "-b", "later"]);
        git(
            &elsewhere,
            &[
                "-C",
                &upstream.to_string_lossy(),
                "fetch",
                "-q",
                &checkout.to_string_lossy(),
                "later:later",
            ],
        );
        fetch_mirror(&repo).unwrap();
        assert!(git(&mirror, &["branch", "--list", "later"]).contains("later"));
    }

    /// The two "where does this repo live" questions do not have the same answer for an adopted
    /// repo, and reading one for the other tells a box to push into a path on the host.
    ///
    /// The mirror's `origin` is where the MIRROR fetches from; `repo_origin_url` is where the REPO
    /// lives, and a box pushes to the second. They were told apart because an adopted repo's mirror
    /// fetched from a checkout whose own origin was the remote — three hops. There are no adopted
    /// repos now, and the two are still not the same thing: the mirror is a local directory and the
    /// repo is a URL, so a box that pushed to "where the mirror fetches from" would push into
    /// skein's own copy on the volume and reach nobody.
    #[test]
    fn where_the_mirror_fetches_from_is_not_where_a_box_pushes() {
        let _g = env_lock();
        let home = tempdir();
        let mut env = env_pins();
        env.set("SKEIN_HOME", &home);

        let checkout = tempdir();
        origin_repo(&checkout);
        git(
            &checkout,
            &["remote", "add", "origin", "git@github.com:acme/skein.git"],
        );
        // The mirror is made from the checkout so nothing here touches the network; the repo's own
        // source is the remote, which is what every repo is now.
        registered("skein", &checkout);
        let repo = update_repos(|repos| {
            let r = repos.iter_mut().find(|r| r.id == "skein").unwrap();
            r.source = "git@github.com:acme/skein.git".into();
            Ok(r.clone())
        })
        .unwrap();

        assert_eq!(
            crate::util::resolved(
                &remote_origin_url(&mirror_path("skein").to_string_lossy()).unwrap()
            ),
            crate::util::resolved(&checkout.to_string_lossy()),
            "the mirror must fetch from the checkout it was made from"
        );
        assert_eq!(
            repo_origin_url(&repo).unwrap(),
            "git@github.com:acme/skein.git",
            "a box must push to the repository, not into skein's own mirror"
        );
    }

    /// A repo whose recorded checkout is **gone** still knows where its boxes push (SKEIN-468).
    ///
    /// This is `gadget-demo` on a live fleet, reproduced: registered from a host path months ago,
    /// that path unreachable now that skein runs inside the sandbox, and its mirror repointed at the
    /// real remote — the right answer sitting on the volume while `repo_origin_url` asked the dead
    /// directory and returned `None`. What `None` costs is asserted here rather than described:
    /// [`crate::fleet::clone_script`] emits no `git remote set-url origin` for an empty upstream, so
    /// the box's `origin` stays the bare mirror and its pushes land where nobody pulls from, and
    /// [`crate::gitgate::repo_slug`] finds no slug, so the box is given no write token.
    #[test]
    fn a_repo_whose_checkout_is_gone_pushes_to_the_remote_its_mirror_names() {
        let _g = env_lock();
        let home = tempdir();
        let mut env = env_pins();
        env.set("SKEIN_HOME", &home);
        // A fixture fleet root, for `fleet::clone_script` below: `util::fleet_root` refuses an
        // unpinned test rather than answering `/boxes`, the live fleet (SKEIN-690). Nothing here
        // asserts the root — the subject is which URL `origin` ends up at — so a fixture is the
        // whole of what it needs. **This test passed without the pin for as long as it has
        // existed**, and not because it did not need one: `a_repo_switched_off_...` above set the
        // variable and never put it back, so a suite run answered this test out of another's
        // fixture and `alone-check` was the only thing that could see it (SKEIN-696).
        env.set("SKEIN_FLEET_ROOT", home.join("boxes"));

        let checkout = tempdir();
        origin_repo(&checkout);
        let repo = registered("demo", &checkout);
        // The repair somebody already did by hand on the volume: the mirror fetches from the remote
        // the repo really lives at, because the checkout it was made from is not there any more.
        let mirror = mirror_path("demo");
        let url = "https://github.com/acme/thing.git";
        git(&mirror, &["remote", "set-url", "origin", url]);
        // And the directory the mirror was made from goes away, which is every repo's state now:
        // there is no checkout anywhere, and the mirror is the only thing that can answer.
        fs::remove_dir_all(&checkout).unwrap();

        assert_eq!(
            repo_origin_url(&repo).as_deref(),
            Some(url),
            "the answer was on disk in the mirror and a directory that is not there shadowed it"
        );
        assert_eq!(
            crate::gitgate::repo_slug(&repo).as_deref(),
            Some("acme/thing"),
            "no slug means no own-repo write token, so the box is given no way to push"
        );
        let script = crate::fleet::clone_script(
            "demo-main",
            &crate::fleet::clone_source(&repo),
            "master",
            "main",
            &repo_origin_url(&repo).unwrap_or_default(),
        );
        assert!(
            script.contains(&format!("remote set-url origin '{url}'")),
            "the box would come up pushing into skein's own mirror:\n{script}"
        );
    }

    /// Pruning a mirror drops a branch deleted upstream and **nothing else** (SKEIN-466).
    ///
    /// Both halves are the test. `--prune` is there so a branch deleted upstream a year ago stops
    /// being offered to every box, and under a mirror's `+refs/*:refs/*` it also deleted every ref
    /// no origin carries: `refs/sandboxes/*`, `refs/stash`, `refs/remotes/*`. That happened —
    /// repointing skein's own mirror deleted four refs, one of them a stash reachable from nothing
    /// else — and `gc --auto` runs right after the fetch, so it is a countdown rather than a
    /// dangling ref.
    #[test]
    fn a_mirror_fetch_prunes_a_deleted_branch_and_nothing_else() {
        let _g = env_lock();
        let home = tempdir();
        let mut env = env_pins();
        env.set("SKEIN_HOME", &home);

        let checkout = tempdir();
        origin_repo(&checkout);
        git(&checkout, &["branch", "doomed"]);
        let repo = registered("proj", &checkout);
        let mirror = mirror_path("proj");
        let head = git(&mirror, &["rev-parse", "HEAD"]);

        // The three shapes of ref that live only in a mirror. A ref nobody planted is a ref whose
        // loss cannot be noticed, which is how this went unseen until it cost a stash.
        let kept = ["refs/sandboxes/x", "refs/stash", "refs/remotes/foo/bar"];
        for r in kept {
            git(&mirror, &["update-ref", r, &head]);
        }
        assert!(
            git(&checkout, &["branch", "--list", "doomed"]).contains("doomed"),
            "the branch this test prunes was never there to prune"
        );
        git(&checkout, &["branch", "-D", "doomed"]);

        fetch_mirror(&repo).unwrap();

        assert_eq!(
            git(&mirror, &["branch", "--list", "doomed"]),
            "",
            "a branch deleted upstream is still offered to every box that clones this"
        );
        // Listed rather than resolved one at a time: `rev-parse` on a deleted ref fails, and the
        // failure would be the helper's rather than this assertion's — the sabotage that proves
        // this test can fail has to land on the sentence that explains it.
        let refs = git(
            &mirror,
            &["for-each-ref", "--format=%(refname) %(objectname)"],
        );
        for r in kept {
            assert!(
                refs.lines().any(|l| l == format!("{r} {head}")),
                "{r} exists in no origin, so pruning against one deleted it — and the objects are \
                 then on gc's countdown, not merely unreferenced. What is left:\n{refs}"
            );
        }
    }
}
