//! What may be registered as a repo's upstream, where a repo pushes to, the id a source
//! derives, and the heads-up a push path earns.

use super::*;

/// May this string be **registered** as a repo's upstream?
///
/// A narrower question than [`is_git_url`], and deliberately a separate function rather than a
/// tightening of it: `is_git_url` also answers "what does this mirror fetch from" about repos
/// already on disk ([`repo_origin_url`], [`clone_mirror`]), and narrowing it there would change how
/// an existing fleet reads itself. This one is only ever asked about a string a person or an HTTP
/// body just handed us, which is the only place a new answer can arrive.
///
/// **A scheme is required, where `is_git_url` also accepts anything ending in `.git`.** That suffix
/// is a filename convention, not a transport, and two things get in through it:
///
/// * `ext::sh -c '…' .git`, which git runs as a shell command. Blocked by git's own protocol
///   allow-list at its default setting — and only there: with `protocol.ext.allow = always` in the
///   host's git config, the exact argv this module builds executed the command. Measured on git
///   2.53.0, not reasoned. A default in somebody else's program is not skein's boundary.
/// * `/home/you/private.git`, a path — which the refusal in [`add_repo`] says in as many words is
///   not a remote, while `is_git_url` was letting it through. Reproduced against a running server:
///   a bare repo outside `~/.skein` was cloned into the fleet's volume, where every box of that
///   repo can read it.
///
/// A leading `-` is refused for the third reason: `source` reaches `git clone` as an argument, and
/// argv has no way to tell an option from a value that begins with one.
pub(crate) fn registrable_source(source: &str) -> bool {
    let source = source.trim();
    !source.starts_with('-')
        && (source.starts_with("https://")
            || source.starts_with("http://")
            || source.starts_with("ssh://")
            || (source.starts_with("git@") && source.contains(':')))
}

/// Does this string name a git remote at all?
///
/// The broad question, asked about repos already on disk — what a mirror fetches from, what a box
/// pushes to. [`registrable_source`] is the narrow one asked of a string somebody has just typed,
/// and the difference between them is written out there.
pub(crate) fn is_git_url(source: &str) -> bool {
    source.starts_with("http://")
        || source.starts_with("https://")
        || source.starts_with("git@")
        || source.starts_with("ssh://")
        || source.ends_with(".git")
}

/// Is this an SSH git remote (`git@host:…` / `ssh://…`)? sbx forwards the host SSH *agent*
/// (`SSH_AUTH_SOCK`) into the box, so SSH push works *iff* the host agent is running with the key
/// loaded; otherwise it'll fail and HTTPS (proxy-injected creds) is the no-setup path. See
/// docs.docker.com/ai/sandboxes/security/credentials.
pub(crate) fn is_ssh_url(s: &str) -> bool {
    s.starts_with("git@") || s.starts_with("ssh://")
}

/// Where this repo lives **upstream** — the repository a box pushes to.
///
/// `source` first, because [`add_repo`] refuses anything that is not a remote, so for every repo
/// registered since that refusal it is the answer.
///
/// The mirror's `origin` is the fallback, and it is there for the entries that predate the refusal:
/// a `repos.json` written when a local path could still be registered has a path in `source`, and
/// that repo's mirror has since been repointed at the remote it really fetches from. Taken **only
/// when it is a git URL**, so a mirror still pointing at a checkout answers `None` rather than
/// telling a box to push into a path on the host — which is the whole distinction this function
/// exists to keep.
///
/// **`None` is not a quiet degradation, which is why the order matters** (SKEIN-468). It used to
/// ask the checkout *first*, which was right on a host and wrong in the fleet, where the checkout
/// is the one thing that is never there: `git -C <missing dir>` exits 128 and this returned `None`.
/// [`crate::fleet::clone_script`] emits no `git remote set-url origin` for an empty upstream, so
/// the box's `origin` stayed the bare mirror — which does not even refuse a push, it accepts it
/// into a repository nobody pulls from — and [`crate::gitgate::repo_slug`] found no slug, so that
/// box got no write token either. Four of nine repos on a live fleet were in exactly that state.
pub fn repo_origin_url(repo: &Repo) -> Option<String> {
    if is_git_url(&repo.source) {
        return Some(repo.source.trim().to_string());
    }
    let mirror = mirror_path(&repo.id);
    // Read, never made: this is a question about a repo, and a caller asking it has not asked for a
    // 300-second clone. A repo with no mirror yet simply has no second answer.
    if mirror_is_made(&mirror) {
        if let Some(url) =
            remote_origin_url(&mirror.to_string_lossy()).filter(|url| is_git_url(url))
        {
            return Some(url);
        }
    }
    // There was a third source here — the user's own checkout — and it is gone with local-path
    // repos. The two above are the whole answer now, and both are things skein owns.
    None
}

/// The `origin` URL of the git directory at `dir`, if any. Works on a bare mirror and on a checkout.
pub(crate) fn remote_origin_url(work: &str) -> Option<String> {
    let mut command = Command::new("git");
    command.args(["-C", work, "remote", "get-url", "origin"]);
    let out = bounded_output(&mut command, "git remote", Duration::from_secs(5)).ok()?;
    if !out.status.success() {
        return None;
    }
    let url = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!url.is_empty()).then_some(url)
}

/// A heads-up about a managed repo's push path, surfaced by `skein add` + the cockpit so it's known
/// up-front (not an error — both cases are workable). Two cases warn: a repo with **no `origin`
/// remote** — a box can't push or open a PR until one exists; and an **SSH `origin`** — in-box push
/// then leans on the host SSH agent (sbx forwards `SSH_AUTH_SOCK`), so it works only when that
/// agent has the key loaded, else switch to HTTPS.
/// `None` for an HTTPS origin (the no-setup happy path; a URL clone always lands here).
pub fn remote_warning(repo: &Repo) -> Option<String> {
    // Where the advice is typed matters, so it names the place a person can actually change: the
    // mirror, which is on the volume and reachable from wherever skein runs.
    //
    // It used to name a repo's recorded checkout instead, and a checkout that is not *there* was
    // neither case (SKEIN-472): it read as "no origin" and told the reader to run
    // `git remote add origin` in a directory the fleet cannot open — advice that cannot be
    // followed, and which hid the one place that can be.
    let where_to_fix = mirror_path(&repo.id).to_string_lossy().into_owned();
    let work = &where_to_fix;
    let Some(url) = repo_origin_url(repo) else {
        return Some(format!(
            "this repo has no `origin` remote — a box can't push or open a PR until one exists. Add it on the host:  git -C {work} remote add origin <url>  (HTTPS needs no setup)."
        ));
    };
    if !is_ssh_url(&url) {
        return None;
    }
    // No "a key is configured, so push should work" branch any more (SKEIN-947). It read the
    // Settings key path, which named a file on the host that skein in the fleet never loaded, so
    // it promised a push that nothing had set up. The agent is the host's, and so is the fix.
    let mut msg = format!(
        "origin is an SSH remote ({url}). In-box push uses your host's forwarded SSH agent, so it works only if a key is loaded there — run `ssh-add` on the host. A box scoped to its own repo has that socket bound over on purpose, so HTTPS is the only path there."
    );
    if let Some(h) = ssh_to_https(&url) {
        msg.push_str(&format!(
            " For a no-setup path, switch to HTTPS:  git -C {work} remote set-url origin {h}"
        ));
    }
    Some(msg)
}

/// Best-effort `git@github.com:org/repo.git` / `ssh://git@host/org/repo.git` → `https://host/org/repo.git`.
/// Returns `None` for shapes we don't recognise (caller just omits the suggestion).
pub(crate) fn ssh_to_https(url: &str) -> Option<String> {
    if let Some(rest) = url.strip_prefix("git@") {
        let (host, path) = rest.split_once(':')?;
        return Some(format!("https://{host}/{path}"));
    }
    if let Some(rest) = url.strip_prefix("ssh://") {
        let rest = rest.strip_prefix("git@").unwrap_or(rest);
        return Some(format!("https://{rest}"));
    }
    None
}

/// Derive a repo id from a source: the last path/URL component, minus a trailing `.git`.
pub(crate) fn repo_id_from_source(source: &str) -> String {
    let last = source
        .trim_end_matches('/')
        .rsplit(['/', ':'])
        .next()
        .unwrap_or(source);
    last.strip_suffix(".git").unwrap_or(last).to_string()
}

/// Why an id was refused, in words that fit the id that was actually refused.
///
/// Its own function so the sentence can be tested; [`add_repo`] is the only caller and reaching it
/// means mirroring a remote, which is not a thing a unit test should need to do to check wording.
///
/// **A leading dot is the case the general sentence cannot explain.** That sentence lists the
/// permitted characters and `.` is one of them, so a reader refused for `.github` would go looking
/// for a character that is not the problem. And `.github` is not a strange thing to type: it is a
/// convention GitHub itself defines, and [`repo_id_from_source`] takes the last path segment.
///
/// The refusal belongs at the id rather than at the box because [`box_name`] is
/// `<repo-id>-<slug(branch)>` — a dotted id mints a dotted box name in the FLEET ROOT, where
/// `.skein` is, and where everything that tells a box from the substrate drops a dotted entry
/// (SKEIN-742). Refusing it later would mean refusing it after the mirror had been cloned.
pub(super) fn repo_id_refusal(id: &str) -> String {
    let why = match id.starts_with('.') {
        true => {
            "a repo id starting with a dot would name boxes starting with a dot, and skein \
             cannot tell those apart from its own substrate directory. Pass `--id <name>` to \
             choose one"
        }
        false => {
            "a repo id is a directory name, so it may hold only letters, digits, `.`, `_` and \
             `-`, and may not begin with `-`"
        }
    };
    format!("{id:?} cannot be a repo id: {why}")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `.github` is an ordinary repository name, and the id derived from it is refused — with a
    /// sentence that says why, because the character-class sentence would not.
    ///
    /// This case is reachable without anybody doing anything strange: `org/.github` is a
    /// convention GitHub itself defines. [`repo_id_from_source`] takes the last path segment, so
    /// the id it mints is `.github`, and [`box_name`] would then mint `.github-main` — a box in
    /// the fleet root beginning with a dot, which is what `util::valid_name` refuses since
    /// SKEIN-742 and what everything telling a box from the substrate drops.
    ///
    /// **What would make this fail**: deriving the refusal from the character class alone, which
    /// is what it did before — that sentence lists `.` as permitted and would leave the reader
    /// looking for a character that is not the problem.
    #[test]
    fn a_dot_github_repo_is_refused_by_a_sentence_that_names_the_reason() {
        let id = repo_id_from_source("https://github.com/acme/.github.git");
        assert_eq!(id, ".github", "the id GitHub's own convention mints");
        assert!(
            !crate::util::valid_name(&id),
            "if this is accepted, `box_name` mints `.github-main` in the fleet root and the \
             substrate filters swallow it"
        );
        let said = repo_id_refusal(&id);
        assert!(
            said.contains("`--id <name>`"),
            "the refusal does not say how to get past it: {said}"
        );
        assert!(
            !said.contains("may hold only letters"),
            "the refusal fell back to the character-class sentence, which lists `.` as \
             permitted — so it names a cause that is not the cause: {said}"
        );
        // And the general sentence is still what an actually-illegal character gets.
        assert!(
            repo_id_refusal("a b").contains("may hold only letters"),
            "a space is a character-class problem and should be told so"
        );
    }

    #[test]
    fn repo_id_and_url_detection() {
        assert_eq!(
            repo_id_from_source("https://github.com/acme/gadget-demo.git"),
            "gadget-demo"
        );
        assert_eq!(
            repo_id_from_source("git@github.com:org/My-Repo.git"),
            "My-Repo"
        );
        assert_eq!(repo_id_from_source("/Users/you/work/thing/"), "thing");
        assert!(is_git_url("https://github.com/x/y.git"));
        assert!(is_git_url("git@github.com:x/y.git"));
        assert!(is_git_url("ssh://git@host/x.git"));
        assert!(!is_git_url("/Users/you/work/thing"));
    }

    /// **What may be registered as an upstream, and what a `.git` suffix is not.**
    ///
    /// [`is_git_url`] answers "does this look like a git URL" and is asked about repos already on
    /// disk; [`registrable_source`] answers "may a request put this in `repos.json`", which is a
    /// question about a string somebody just typed. The two differ on the `.git` suffix, and the
    /// difference is the whole point of the second function existing.
    ///
    /// Both halves are here. The refusals prove nothing on their own — a gate that refused
    /// everything would satisfy them — so the four shapes skein actually clones are asserted
    /// accepted, and they are the four `is_git_url` already lists minus the suffix rule.
    #[test]
    fn only_a_real_remote_may_be_registered_as_a_repos_upstream() {
        for good in [
            "https://github.com/x/y.git",
            "http://internal.example/x/y.git",
            "git@github.com:x/y.git",
            "ssh://git@host/x.git",
            "https://github.com/x/y",
        ] {
            assert!(registrable_source(good), "{good} is a remote skein clones");
        }

        // `ext::` is git's shell-command transport. Its default protocol policy refuses it, which
        // is git's decision and not skein's — with `protocol.ext.allow = always` in the host's git
        // config, the argv `clone_mirror` builds ran the command (git 2.53.0, measured).
        assert!(!registrable_source("ext::sh -c touch% /tmp/x% #.git"));
        // A path is not a remote — the refusal in `add_repo` says so in as many words, and this is
        // what makes that true. `is_git_url` accepted it, and the API cloned a bare repo from
        // outside `~/.skein` into the fleet's volume.
        assert!(!registrable_source("/home/somebody/private.git"));
        assert!(!registrable_source("../../elsewhere.git"));
        // Anything argv would read as an option, whatever follows it.
        assert!(!registrable_source("--upload-pack=touch /tmp/x"));
        assert!(!registrable_source("-uwhatever https://github.com/x/y.git"));
        // `git@` without a host separator is not the scp-like form; it is a filename.
        assert!(!registrable_source("git@thing.git"));
    }

    #[test]
    fn ssh_url_detection_and_https_conversion() {
        assert!(is_ssh_url("git@github.com:org/repo.git"));
        assert!(is_ssh_url("ssh://git@github.com/org/repo.git"));
        assert!(!is_ssh_url("https://github.com/org/repo.git"));
        assert_eq!(
            ssh_to_https("git@github.com:org/repo.git").as_deref(),
            Some("https://github.com/org/repo.git")
        );
        assert_eq!(
            ssh_to_https("ssh://git@gitlab.com/org/repo.git").as_deref(),
            Some("https://gitlab.com/org/repo.git")
        );
        assert_eq!(host_of("git@github.com:org/repo.git"), Some("github.com"));
        assert_eq!(
            host_of("ssh://git@gitlab.com/org/repo.git"),
            Some("gitlab.com")
        );
    }
}
