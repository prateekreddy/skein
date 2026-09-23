//! A box's checkout: the clone, standing a review box at a commit, where a change starts,
//! repairing `origin`, and which branch is the base.

use super::*;

/// The shell that prepares a box's checkout inside the fleet sandbox.
///
/// Clones from the **remote** at `base`, then creates or checks out the box's branch. Boxes used to
/// be cloned from the host's own clone, which meant a new box inherited whatever was stale or
/// half-committed there; from the remote it starts from the same base the diff is taken against.
///
/// Refuses rather than reuses when the tree is already populated: a box root left behind by a
/// previous box of the same name would otherwise silently give the new one someone else's work.
///
/// `upstream` repairs the case where cloning and pushing want different sources, which is now
/// **every** repo rather than only an adopted one. The clone comes from the repo's mirror
/// ([`clone_source`]), a path on the volume — and `git clone <path>` sets `origin` to that path, so
/// a box left unrepaired would push into skein's own mirror instead of the repository it came from.
///
/// That was already the shape of the bug when the clone source was the host's checkout: pushing
/// appeared to work until a box shared the checked-out branch, at which point git refused with
/// `receive.denyCurrentBranch` — a remote-side policy error for what is really a mis-pointed
/// remote. A bare mirror does not even refuse; it accepts the push, into a repository nobody pulls
/// from.
///
/// So: clone locally, which is fast and carries commits that have not reached the remote, then
/// point `origin` at where the repo actually lives ([`crate::repos::repo_origin_url`]) and keep the
/// mirror as `local`. Empty only for a repo with no upstream anywhere — an adopted checkout with no
/// origin — where the mirror is the only thing there is to push to.
pub fn clone_script(name: &str, url: &str, base: &str, branch: &str, upstream: &str) -> String {
    let root = box_root(name);
    let tree = format!("{root}/tree");
    let tree_q = sh_quote(&tree);
    let url_q = sh_quote(url);
    // An empty base is not a missing value to substitute a guess for — it is skein saying it could
    // not learn the remote's default, and a plain `git clone` asks the remote for it directly.
    //
    // When there IS a base, the clone falls back to the same plain form rather than failing. A base
    // that the remote does not have has always been possible — a configured base a given repo does
    // not use, a cached `origin/HEAD` gone stale after a rename — and the cost was severe out of all
    // proportion: a migration that stops the old sandbox, then cannot start the new box, leaves the
    // box in neither place until someone woke the old sandbox by hand. A tree cloned from the wrong base
    // is a non-event by comparison, since the branch is checked out over it immediately.
    let clone = if base.is_empty() {
        format!("git clone --single-branch {url_q} {tree_q}")
    } else {
        format!(
            "git clone --single-branch --branch {base_q} {url_q} {tree_q} || \
             {{ echo 'skein: no {base} on the remote; cloning its default branch instead' >&2; \
                rm -rf {tree_q}; git clone --single-branch {url_q} {tree_q}; }}",
            base_q = sh_quote(base),
        )
    };
    // Best-effort, and deliberately not under `set -e`: a box whose remote could not be re-pointed
    // pushes to the mirror it cloned from, which is recoverable — the commits are on the volume and
    // can be pushed on from there. Failing the whole clone over it would turn a working box into
    // no box.
    let remotes = if upstream.is_empty() {
        String::new()
    } else {
        format!(
            "; git remote add local {url_q} 2>/dev/null || true; \
             git remote set-url origin {up_q} || \
             echo 'skein: could not point origin at {upstream}; this box pushes to the mirror' >&2",
            up_q = sh_quote(upstream),
        )
    };
    format!(
        "set -e; \
         if [ -e {tree_q}/.git ]; then echo 'skein: {name} already has a checkout; destroy the box first' >&2; exit 1; fi; \
         mkdir -p {root_q}; \
         {clone}; \
         cd {tree_q}; \
         git config remote.origin.fetch '+refs/heads/*:refs/remotes/origin/*'; \
         git checkout -B {branch_q}{remotes}",
        root_q = sh_quote(&root),
        branch_q = sh_quote(branch),
    )
}

/// Stand a review box's tree **at the commit under review**, not on a branch.
///
/// `clone_script` ends at `git checkout -B <branch>` with no start point, which creates the branch
/// at whatever the clone's HEAD is — the base tip. That is right for a person's box, where the
/// branch is the thing being worked on and its commits arrive later. It is wrong for a review box,
/// which exists to read one commit: it would stand at the base, holding the pull request's commits
/// without being on them, and a reviewer confidently describing code that is not in the change is
/// the worst failure this whole path has (`docs/pr-review.md` §11, and `review.rs`'s own
/// `stand_the_change_up`).
///
/// Three lines, in the order that costs least, and each one is here for a measured reason:
///
/// * **Fetch only if the commit is absent.** A same-repo pull request's branch is in
///   `refs/heads/*`, so the clone already carries its commits and this costs nothing. A fork's is
///   not, and `refs/pull/<n>/head` is where GitHub keeps it — the same ref
///   [`crate::repos::fetch_pull_head`] asks for on the reading path, spelled by the same function
///   so the two cannot drift. A box's `origin` is the repository's real remote (`clone_script`
///   re-points it), so unlike the host path this asks GitHub directly and the box's own credentials
///   are what answer.
/// * **Detach.** There is no branch to be on, and there must not be: nothing about reviewing should
///   be able to push, and a detached HEAD has nowhere to push to.
/// * **Clean.** A checkout of a moved head leaves the file the new commit *deletes* sitting in the
///   tree, and the reviewer reads it as part of the change. `stand_the_change_up` learned this on
///   the reading path and it is the same tree either way.
///
/// It refuses loudly rather than leaving a tree at the wrong commit, for the reason above: an empty
/// or absent answer is recoverable and a confident wrong one is not.
pub fn stand_at_head_script(name: &str, number: u64, head_sha: &str) -> String {
    let refspec = crate::repos::pull_head_ref(number);
    format!(
        "cd {tree_q} || {{ echo 'skein: {name} has no checkout to stand up' >&2; exit 1; }}; \
         git rev-parse --verify --quiet {sha_q}^{{commit}} >/dev/null 2>&1 || \
           git fetch --quiet origin {fetch_q} || \
           echo 'skein: could not fetch {refspec} for {name}; the commit may be here already' >&2; \
         git checkout --quiet --detach {sha_q} || \
           {{ echo 'skein: {name} cannot stand at {head_sha} — refusing to leave it on another \
commit' >&2; exit 1; }}; \
         git clean --quiet -fdx",
        tree_q = sh_quote(&format!("{}/tree", box_root(name))),
        sha_q = sh_quote(head_sha),
        fetch_q = sh_quote(&format!("+{refspec}:{refspec}")),
    )
}

/// **Where this pull request's change starts**, asked of the box that is standing at its head.
///
/// `git merge-base origin/<base> HEAD`, and the answer is a **sha rather than a ref name** for
/// `review::Standing::Change`'s reason: a ref leaves the model resolving `origin/main` against a
/// clone that may be days behind, and a merge base resolved once here cannot drift between being
/// named and being read.
///
/// Prints nothing when it cannot answer — a base branch this clone has never seen, a tree that is
/// not a checkout — so an empty answer is the caller's cue that the code is readable and the
/// *change* is not. That is a real state and not a failure: `Standing::Head` is what it becomes.
pub fn change_starts_script(name: &str, base_ref: &str) -> String {
    format!(
        "cd {tree_q} 2>/dev/null && git merge-base {base_q} HEAD 2>/dev/null || true",
        tree_q = sh_quote(&format!("{}/tree", box_root(name))),
        base_q = sh_quote(&format!("origin/{base_ref}")),
    )
}

/// Point an existing box's `origin` at the repo's remote, if it is still where it cloned from.
///
/// The clone-time version of this ([`clone_script`]) only helps boxes cloned after it landed. This
/// is the same repair for the ones already on disk, and it runs on every start because there is no
/// other moment that would notice.
///
/// The equality test is the whole safety argument: it rewrites only the URL skein itself put there.
/// An origin someone re-pointed by hand — at a fork, at a mirror — is left exactly alone.
pub fn origin_repair_script(name: &str, source: &str, upstream: &str) -> String {
    format!(
        "cd {tree_q} 2>/dev/null || exit 0; \
         cur=\"$(git remote get-url origin 2>/dev/null || true)\"; \
         [ \"$cur\" = {src_q} ] || exit 0; \
         git remote add local {src_q} 2>/dev/null || true; \
         git remote set-url origin {up_q} && \
         echo 'skein: {name} pushed at what it cloned from; origin now points at {upstream}' >&2",
        tree_q = sh_quote(&format!("{}/tree", box_root(name))),
        src_q = sh_quote(source),
        up_q = sh_quote(upstream),
    )
}

/// What a box clones from: **the repo's mirror on the volume**, whatever the repo was registered as.
///
/// One answer instead of two, and the two it replaces were each wrong in their own way. A URL repo
/// cloned from the network — a fresh clone per box, over the wire, needing the box to hold a
/// credential for a repository it is only reading. A repo adopted in place cloned from the host's
/// own working checkout, which meant the checkout had to be mounted into the sandbox for every box
/// of that repo, and a box's clone inherited whichever branch the host had checked out.
///
/// The mirror is local, is on the volume already mounted at [`fleet_workspace`], has every branch,
/// and has no working tree to inherit anything from.
///
/// The freshness question does not go away, it moves: what a new box starts from is now the
/// mirror's last fetch rather than the host checkout's last pull. [`crate::repos::fetch_mirror`] is
/// what advances it, and `start_box_inner` calls it before cloning so that a box created right now
/// starts from what the remote has right now.
pub(crate) fn clone_source(repo: &Repo) -> String {
    match crate::repos::ensure_mirror(repo) {
        Ok(mirror) => mirror.to_string_lossy().into_owned(),
        // Said out loud, and then the old answer: a fleet whose mirror cannot be made should still
        // be able to start a box, and this is the one place where the difference is invisible from
        // inside the box. For a URL repo the fallback is a slower clone over the network; for a
        // path-sourced legacy entry it is that checkout, reachable only if it is still mounted.
        Err(why) => {
            eprintln!(
                "skein: {} has no mirror ({why}), so its boxes clone from the remote instead — \
                 `skein pull {}` makes one",
                repo.id, repo.id
            );
            repo.source.clone()
        }
    }
}

/// The branch a box's own branch is cut from.
///
/// Asked of the SAME SOURCE the clone will use — the mirror ([`clone_source`]) — because an answer
/// from anywhere else can name a branch the clone will not find. `refs/remotes/origin/HEAD` in the
/// host checkout looks like the remote's answer and is not: it is a cache written when that clone
/// was made, and it goes stale when the default is renamed on the far side. A box migration failed
/// on exactly that — the cached ref said `main`, the remote had only `master`, and
/// `git clone --branch main` refused, leaving the box's old sandbox stopped with no fleet box to
/// replace it.
///
/// So: `ls-remote --symref HEAD` against the mirror, which answers from its own refs and cannot be
/// unreachable. That makes the freshness of the answer the freshness of the mirror, which is why
/// `start_box_inner` fetches it first; the local guesses remain the fallback for a repo with no mirror
/// at all, and `main` is never assumed.
pub fn base_branch(repo: &Repo) -> String {
    // No `-C`: this only ever ran `ls-remote` against a source named in full, and the working
    // directory was the adopted repo's checkout, which no longer exists. Two candidates came off
    // that checkout — `refs/remotes/origin/HEAD` and its current branch — and both were caches of
    // the remote that the `ls-remote` below asks directly and cannot get stale.
    let git = |args: &[&str]| -> Option<String> {
        let (out, _, code) = run_capture("git", args).ok()?;
        let out = out.trim().to_string();
        (code == 0 && !out.is_empty()).then_some(out)
    };

    // What this repo's base might be called. Only the configured one now — the user's own answer.
    let mut wanted: Vec<String> = Vec::new();
    let configured = load_config().base_branch.trim().to_string();
    if !configured.is_empty() {
        wanted.push(configured);
    }

    // One round trip settles all of them: `HEAD` comes back as `ref: refs/heads/<default>` — the
    // remote's own name for its default, whatever it is — and each candidate comes back only if the
    // remote really has it. Naming the refs explicitly keeps the reply small on a repo with
    // thousands of branches.
    let source = clone_source(repo);
    let mut argv: Vec<String> = ["ls-remote", "--symref", &source, "HEAD"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    argv.extend(wanted.iter().map(|b| format!("refs/heads/{b}")));
    let listing = git(&argv.iter().map(String::as_str).collect::<Vec<_>>());

    if let Some(listing) = listing {
        // The first candidate the remote actually has wins — that is how a configured base of
        // `develop` is honoured on the repos that have one without breaking the repos that do not.
        if let Some(found) = wanted.iter().find(|b| {
            listing
                .lines()
                .any(|line| line.split_whitespace().nth(1) == Some(&format!("refs/heads/{b}")))
        }) {
            return found.clone();
        }
        // None of them exist there — so take the remote's own default, which always does.
        if let Some(default) = listing
            .lines()
            .find_map(|line| line.strip_prefix("ref:"))
            .and_then(|rest| rest.split_whitespace().next())
            .and_then(|r| r.rsplit_once('/').map(|(_, b)| b.to_string()))
        {
            return default;
        }
    }

    // Offline, or a source that cannot be reached: fall back to the local guesses in the same order
    // and let the clone's own fallback cover a wrong one. Empty rather than `main` when there is
    // nothing to go on at all — a guess that names a branch fails the clone outright, while naming
    // none asks git for the remote's default and cannot be wrong.
    wanted.into_iter().next().unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::*;

    /// **Where a change starts is asked of the box, and answered as a commit or not at all.**
    ///
    /// **What would make this fail:** dropping the `|| true`, so a repo whose base branch this
    /// clone has never seen makes the whole exec non-zero and the caller reads a failure where the
    /// honest answer is "the code is here and the change is not"; or naming the base bare rather
    /// than as `origin/<base>`, which resolves against a local branch a review box does not have.
    #[test]
    fn the_change_start_is_asked_of_the_box_and_survives_not_knowing() {
        // Locked and pinned: `$SKEIN_FLEET_ROOT` is process-global and this test only READS it,
        // which was already a race against the setters in this file and is a loud one now that
        // `util::fleet_root` refuses an unset root instead of answering `/boxes` (SKEIN-690).
        // Nothing asserted below carries the root's value. The last assertion reads
        // `box_root` a SECOND time, so an unlocked run could compare two different roots.
        let _g = crate::testutil::env_lock();
        let dir = crate::testutil::tempdir();
        std::env::set_var("SKEIN_FLEET_ROOT", dir.join("fleet"));
        let script = change_starts_script("acme-pr-42", "main");
        assert!(
            script.contains("git merge-base 'origin/main' HEAD"),
            "the base must be named as the remote's, or it resolves against a branch a detached \
             review box does not have: {script}"
        );
        assert!(
            script.trim_end().ends_with("|| true"),
            "a base this clone has never seen must answer nothing rather than fail: {script}"
        );
        assert!(
            script.contains(&format!("{}/tree", box_root("acme-pr-42"))),
            "the question was asked somewhere other than the box's checkout: {script}"
        );
        std::env::remove_var("SKEIN_FLEET_ROOT");
    }

    /// **A review box stands at the commit, and never on a branch.**
    ///
    /// Four properties, and each is here because getting it wrong is silent rather than loud — the
    /// tree would exist, the reviewer would read it, and only the review would be wrong.
    ///
    /// Sabotages, in the order asserted: swap `--detach` for `-B` and the box holds the pull
    /// request's commits without being on them; drop the `rev-parse` guard and every same-repo
    /// round pays GitHub for commits already in the clone; move `git clean` above the checkout and
    /// a file the reviewed commit DELETES is still on disk for the reviewer to read as part of the
    /// change; drop the `exit 1` and a box that could not reach the commit is left standing at
    /// whatever it had, which is the one outcome worse than an empty tree.
    #[test]
    fn a_review_box_stands_at_the_commit_and_never_on_a_branch() {
        // Locked and pinned, both halves. `$SKEIN_FLEET_ROOT` is process-global and this test only
        // READS it — which was already a race against the setters in this file, and is a loud one
        // now that `util::fleet_root` refuses an unset root instead of answering `/boxes`: the
        // window a setter leaves when it removes the variable used to be harmless and is now a
        // panic. A reader has to take the lock too (SKEIN-690). Nothing asserted below carries the root.
        let _g = crate::testutil::env_lock();
        let dir = crate::testutil::tempdir();
        std::env::set_var("SKEIN_FLEET_ROOT", dir.join("fleet"));
        let script = stand_at_head_script("acme-pr-42", 42, "abc1234");

        assert!(
            script.contains("git checkout --quiet --detach 'abc1234'"),
            "a review box must be detached at the commit under review: {script}"
        );
        assert!(
            !script.contains("checkout -B"),
            "a branch is a thing to push, and nothing about reviewing should be able to: {script}"
        );

        // The fetch is GUARDED. A same-repo pull request's branch is in `refs/heads/*`, so its
        // commits are already in the clone; an unconditional fetch would pay GitHub on every round
        // of every review for something already on disk.
        let before_fetch = script.split("git fetch").next().unwrap_or_default();
        assert!(
            before_fetch.contains("rev-parse --verify"),
            "the pull-ref fetch is unconditional, so every same-repo round pays for it: {script}"
        );
        assert!(
            script.contains("refs/pull/42/head"),
            "the fork case reaches GitHub by the ref it keeps the head under: {script}"
        );

        // Cleaning BEFORE the checkout would remove yesterday's leftovers and then re-create
        // today's: the file this commit deletes is only stale once the checkout has moved.
        let after_checkout = script.split("--detach").nth(1).unwrap_or_default();
        assert!(
            after_checkout.contains("git clean"),
            "the tree is cleaned before it moves, so a file this commit deletes survives into the \
             reading: {script}"
        );

        assert!(
            script.contains("refusing to leave it on another commit"),
            "a box that cannot reach the commit must refuse rather than be read at the wrong one: \
             {script}"
        );
        std::env::remove_var("SKEIN_FLEET_ROOT");
    }

    /// A box pushes to the repo's remote. For an entry whose `source` is a path the clone came from
    /// that checkout — fast, and it carried commits the host had not pushed — but `git clone
    /// <path>` names that path `origin`, and a box whose origin is a directory on someone's laptop
    /// is a box that cannot open a PR. It also fails outright the moment the box works on the branch
    /// the host has checked out, which is the normal state of affairs for skein's own box.
    #[test]
    fn a_box_cloned_from_the_host_still_pushes_to_the_remote() {
        let _g = env_lock();
        let dir = tempdir();
        // A fixture fleet root: `util::fleet_root` refuses an unpinned test rather than answering
        // `/boxes`, which on any machine running skein is the live fleet (SKEIN-690). Nothing
        // asserted below carries the root, so a fixture is the whole of what this needs.
        std::env::set_var("SKEIN_FLEET_ROOT", dir.join("fleet"));
        let script = clone_script(
            "web-main",
            "/Users/you/work/web",
            "main",
            "feat/auth",
            "git@github.com:o/r.git",
        );
        assert!(
            script.contains("git clone --single-branch --branch 'main' '/Users/you/work/web'"),
            "still cloned locally — the point is the push target, not the fetch: {script}"
        );
        assert!(
            script.contains("git remote set-url origin 'git@github.com:o/r.git'"),
            "origin must be the remote the repo actually pushes to: {script}"
        );
        assert!(
            script.contains("git remote add local '/Users/you/work/web'"),
            "the host clone stays reachable by name; re-pointing origin must not lose it: {script}"
        );
        let after_checkout = script.split("git checkout -B").nth(1).unwrap_or_default();
        assert!(
            after_checkout.contains("set-url"),
            "re-pointing before the branch exists would leave a box with no checkout: {script}"
        );

        // A URL source already clones from where it pushes; touching origin there could only break it.
        let direct = clone_script(
            "web-main",
            "git@github.com:o/r.git",
            "main",
            "feat/auth",
            "",
        );
        assert!(
            !direct.contains("remote set-url") && !direct.contains("remote add"),
            "nothing to repair when the source is the remote: {direct}"
        );

        // And the same repair for the boxes already on disk, which will never be re-cloned.
        let repair =
            origin_repair_script("web-main", "/Users/you/work/web", "git@github.com:o/r.git");
        assert!(
            repair.contains("[ \"$cur\" = '/Users/you/work/web' ] || exit 0"),
            "only the URL skein put there may be rewritten; a hand-set origin is someone's \
             deliberate choice: {repair}"
        );
        assert!(repair.contains("git remote set-url origin 'git@github.com:o/r.git'"));
        std::env::remove_var("SKEIN_FLEET_ROOT");
    }

    // A box that already has a checkout is refused, not reused. Silently adopting the tree left by a
    // previous box of the same name would hand the new one someone else's uncommitted work.
    #[test]
    fn preparing_a_checkout_starts_from_the_remote_base_and_never_reuses_a_tree() {
        // **Pinned, because `/boxes` below is `fleet_root()`'s DEFAULT, not a constant.** Without
        // this the test asserts on whatever `$SKEIN_FLEET_ROOT` happens to be when it runs: it
        // passed alone and failed under the full suite, on a clean tree, because a neighbour that
        // legitimately sets the variable was still holding it. A test whose answer depends on what
        // ran before it will one day pass for the wrong reason instead of failing (SKEIN-471).
        let _g = crate::testutil::env_lock();
        std::env::set_var("SKEIN_FLEET_ROOT", "/boxes");
        let script = clone_script(
            "web-main",
            "git@github.com:o/r.git",
            "main",
            "feat/auth",
            "",
        );
        // The quoting closes before `/.git`, which the shell concatenates back into one word.
        assert!(script.contains("if [ -e '/boxes/web-main/tree'/.git ]"));
        assert!(script.contains("exit 1"));
        assert!(
            script.contains("git clone --single-branch --branch 'main' 'git@github.com:o/r.git'"),
            "from the remote at the base branch, and only that branch — the same base the diff is \
             taken against"
        );
        assert!(
            script.contains("git checkout -B 'feat/auth'"),
            "a branch with a slash is one argument, not a path"
        );
        // A base the remote does not have must not be fatal. It cost a real migration: the old
        // sandbox was already stopped, the clone refused `--branch main` on a repo whose default is
        // `master`, and the box existed in neither place until someone woke the old sandbox by hand.
        assert!(
            script.contains("|| {") && script.matches("git clone").count() == 2,
            "a wrong base must fall back to the remote's default, not strand the box: {script}"
        );
        // And when skein could not learn the base at all, it asks the remote instead of guessing.
        let blind = clone_script("web-main", "git@github.com:o/r.git", "", "feat/auth", "");
        assert!(
            blind.contains("git clone --single-branch 'git@github.com:o/r.git'")
                && !blind.contains("--branch '"),
            "no base means let git use the remote's default — still one branch, just not a named \
             one: {blind}"
        );
        std::env::remove_var("SKEIN_FLEET_ROOT");
    }

    // The base is a ladder, not a guess: the branch the user configured, then the local caches of
    // the remote's default — and every rung is checked against the remote before it is used, so a
    // configured `develop` is honoured on the repos that have one without breaking those that don't.
    #[test]
    fn the_base_branch_is_whatever_the_remote_actually_has() {
        use std::fs;
        let _g = env_lock();
        let home = tempdir();
        std::env::set_var("SKEIN_HOME", &home);

        // A real repository whose default branch is `master`, which is the case that failed.
        let origin = home.join("origin");
        fs::create_dir_all(&origin).unwrap();
        let git = |dir: &std::path::Path, args: &[&str]| {
            let ok = std::process::Command::new("git")
                .current_dir(dir)
                .args(args)
                .env("GIT_AUTHOR_NAME", "t")
                .env("GIT_AUTHOR_EMAIL", "t@e")
                .env("GIT_COMMITTER_NAME", "t")
                .env("GIT_COMMITTER_EMAIL", "t@e")
                .output()
                .unwrap();
            assert!(ok.status.success(), "git {args:?}: {ok:?}");
        };
        git(&origin, &["init", "-b", "master"]);
        fs::write(origin.join("f"), "x").unwrap();
        git(&origin, &["add", "-A"]);
        git(&origin, &["commit", "-m", "one"]);

        let repo = Repo {
            read_prs: false,
            id: "bridge".into(),
            source: origin.to_string_lossy().into_owned(),
            store: String::new(),
            agent: "claude".into(),
            plane_project: String::new(),
            sync_connection: String::new(),
            review_queue: true,
            sync_gateway_url: String::new(),
            ..Default::default()
        };

        let mut config = load_config();
        config.base_branch = String::new();
        save_config(&config).unwrap();
        assert_eq!(
            base_branch(&repo),
            "master",
            "the remote's own default, never an assumed `main`"
        );

        // A configured base the repo does not have must not be taken at face value: it is one
        // setting shared by every repo, so trusting it blindly reintroduces the same failure.
        let mut config = load_config();
        config.base_branch = "main".into();
        save_config(&config).unwrap();
        assert_eq!(
            base_branch(&repo),
            "master",
            "no `main` here — fall through"
        );

        // A configured base the repo DOES have wins, ahead of the remote's default — once the
        // mirror has been told the branch exists.
        git(&origin, &["branch", "develop"]);
        let mut config = load_config();
        config.base_branch = "develop".into();
        save_config(&config).unwrap();
        // The staleness this question now has, stated rather than discovered: the answer comes from
        // the mirror, so a branch created since its last fetch is one the mirror has never heard of.
        // `start_box_inner` fetches before it asks, which is why this is a property and not a bug.
        assert_eq!(
            base_branch(&repo),
            "master",
            "an unfetched mirror cannot know about a branch made a moment ago"
        );
        crate::repos::fetch_mirror(&repo).unwrap();
        assert_eq!(base_branch(&repo), "develop", "the user's own answer leads");

        std::env::remove_var("SKEIN_HOME");
    }
}
