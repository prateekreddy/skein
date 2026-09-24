//! A git run skein makes fleet-side: never a terminal, an ssh that cannot ask, the credential
//! the fleet holds for that repository, and a refusal said in skein's terms.

use super::*;

/// The environment variable [`FLEET_CREDENTIAL_HELPER`] reads the token out of.
///
/// A name of skein's own, so it cannot collide with anything git or the fleet already sets. The
/// helper below has to spell it a second time — `concat!` takes literals — and renaming one without
/// the other leaves git asking for a variable nothing sets, which is a helper that answers with an
/// EMPTY password rather than one that fails. That is why
/// [`tests::the_token_sent_is_the_one_filed_under_that_repository`] asserts on the stored token's
/// own characters coming back out of real git, and not merely on a `password=` line being there.
const FLEET_TOKEN_VAR: &str = "SKEIN_FLEET_GIT_TOKEN";

/// A `credential.helper` that answers from [`FLEET_TOKEN_VAR`] and from nothing else.
///
/// **The token reaches git through the environment and never through argv or a URL**, which is
/// §9.5's rule about secrets and the reason this is a helper rather than the one-liner that rewrites
/// `https://github.com/…` into `https://<token>@github.com/…`. A rewritten URL is in
/// `/proc/<pid>/cmdline`, in git's own error messages, and in the `origin` of anything cloned from
/// it; an environment variable is readable only by the same uid.
///
/// `git` runs a `!`-prefixed helper through `sh` with the operation appended, so `$1` here is
/// `get`, `store` or `erase`. Only `get` is answered: `store` and `erase` are git offering to
/// remember or forget a credential skein already keeps on disk, and a helper that acted on them
/// would be a second, unmanaged copy of the token.
const FLEET_CREDENTIAL_HELPER: &str = concat!(
    "!f() { test \"$1\" = get && ",
    "printf 'username=x-access-token\\npassword=%s\\n' \"$SKEIN_FLEET_GIT_TOKEN\"; }; f"
);

/// The file one stored credential's token lives in — `~/.skein/github-pats/<id>`.
///
/// The same path `gitgate::credential_token_path` writes and reads, spelled again here because that
/// one is private to its module. The duplication is deliberate and is covered rather than assumed:
/// this path is quoted at a person in [`git_refusal`], so naming the wrong directory would send
/// them to look somewhere skein never reads, and
/// [`tests::the_token_sent_is_the_one_filed_under_that_repository`] writes through this function
/// and reads back through [`crate::gitgate::credential_for`], so the two cannot drift apart in
/// silence.
fn stored_credential_path(id: &str) -> PathBuf {
    skein_home().join("github-pats").join(id)
}

/// The list that says which repository each stored credential covers — `~/.skein/github-pats.json`.
fn stored_credentials_path() -> PathBuf {
    skein_home().join("github-pats.json")
}

/// The fleet's optional read-only PAT — `~/.skein/github-read-token`, as [`crate::gitgate::read_pat`]
/// reads it.
fn read_credential_path() -> PathBuf {
    skein_home().join("github-read-token")
}

/// What skein found to authenticate one **fleet-side** git run with, and where it looked.
///
/// Fleet-side means outside every box. `src/box-session.sh` wires a box's git up completely — a
/// `SKEIN_GIT_TOKENS` directory, `credential.useHttpPath`, a `credential.helper` — and none of
/// [`clone_mirror`], [`fetch_mirror`] or [`fetch_pull_head`] is inside a box, so none of that
/// reached them. Carried from [`fleet_git`] to [`git_refusal`] so a failure can name the
/// credential that was tried, or the files that were read looking for one, instead of passing
/// git's own words through.
pub(super) struct FleetGit {
    /// `owner/name`, when the remote is a GitHub repository a stored credential can be keyed to.
    /// `None` for anything else — a directory, a remote on some other host — which no stored PAT
    /// covers and which it would therefore be wrong to blame a missing PAT for.
    slug: Option<String>,
    /// The credential file whose token was handed to git, if one was.
    sent: Option<PathBuf>,
    /// The remote git was pointed at, as the repo records it. Named in an ssh refusal, where the
    /// remote is the thing a person has to go and fix and the repo id is not.
    remote: String,
    /// The `GIT_SSH_COMMAND` git was given — see [`ssh_that_cannot_ask`].
    ssh: String,
}

/// Wire up a git command skein runs fleet-side: never a terminal, and the credential the fleet
/// already holds.
///
/// **`GIT_TERMINAL_PROMPT=0` is the half that matters, and it matters most when no credential is
/// found at all** (SKEIN-951, SKEIN-954). All three call sites were a bare `Command::new("git")` —
/// no credential environment, no helper, and nothing stopping git from reaching its last resort,
/// which is to ask the terminal. On 2026-09-19 that stopped a `skein start` dead for twenty minutes at
/// `Username for 'https://github.com':`, the git process asleep inside `git remote-https` waiting on
/// a tty nobody was watching, with no way for the owner to tell what was being asked for.
///
/// Failing here is survivable and hanging is not, which is why the prompt is the bug rather than the
/// missing token: `start_box_inner` prints a warning and brings the box up from the mirror it
/// already has when [`fetch_mirror`] returns `Err`. Refusing turns a dead fleet into a line of text.
///
/// **The credential is chosen by the repository it covers, not by being first in the file.**
/// `github-pats.json` records the `owner/name` each stored token is good for, so
/// [`crate::gitgate::credential_for`] can pick the one that can actually reach this remote. A
/// read-only PAT is the fallback and not the preference: it is broad by design, so it may well not
/// have been granted the private repository in question, while a per-repo token covers exactly the
/// repository being fetched or it would not be filed under it.
///
/// Nothing is sent to a remote skein cannot key a credential to — see [`FleetGit::slug`]. A token
/// belonging to a person is not something to offer a host on the strength of a URL skein did not
/// recognise.
pub(super) fn fleet_git(command: &mut Command, repo: &Repo) -> FleetGit {
    // First, and unconditionally. Every path below can decide there is no credential to send; none
    // of them may decide that git is allowed to ask a terminal instead.
    command.env("GIT_TERMINAL_PROMPT", "0");
    // The same for ssh, which `GIT_TERMINAL_PROMPT` does not reach (SKEIN-955).
    let ssh = ssh_that_cannot_ask(repo);
    command.env("GIT_SSH_COMMAND", &ssh);
    let remote = repo.source.trim().to_string();
    let Some(slug) = crate::gitgate::repo_slug(repo) else {
        return FleetGit {
            slug: None,
            sent: None,
            remote,
            ssh,
        };
    };
    let found = crate::gitgate::credential_for(&slug)
        .map(|(c, token)| (stored_credential_path(&c.id), token))
        .or_else(|| crate::gitgate::read_pat().map(|token| (read_credential_path(), token)));
    let Some((path, token)) = found else {
        return FleetGit {
            slug: Some(slug),
            sent: None,
            remote,
            ssh,
        };
    };
    command.env(FLEET_TOKEN_VAR, token.expose());
    // `credential.helper` is a LIST, and the empty value is how git is told to forget the entries it
    // has accumulated from the fleet's own config files. Without the reset a helper configured there
    // — `store` pointing at a file with a stale token, say — is asked first and answers first, and
    // skein's token is never reached. With it, the only helper is the one on the line below.
    command.env("GIT_CONFIG_COUNT", "2");
    command.env("GIT_CONFIG_KEY_0", "credential.helper");
    command.env("GIT_CONFIG_VALUE_0", "");
    command.env("GIT_CONFIG_KEY_1", "credential.helper");
    command.env("GIT_CONFIG_VALUE_1", FLEET_CREDENTIAL_HELPER);
    FleetGit {
        slug: Some(slug),
        sent: Some(path),
        remote,
        ssh,
    }
}

/// The ssh command a fleet-side git runs: whatever it would have run anyway, with
/// `-oBatchMode=yes` added so ssh can never ask a terminal anything (SKEIN-955).
///
/// **`GIT_TERMINAL_PROMPT=0` does not reach ssh.** [`registrable_source`] accepts `ssh://` and
/// `git@host:`, and on that transport the asking is done by ssh itself, on `/dev/tty`: `Enter
/// passphrase for key …` for an encrypted key with no agent, `Are you sure you want to continue
/// connecting` for an unknown host key. Either blocks for ever, which is the SKEIN-951 hang by
/// another door. With `BatchMode=yes` ssh fails instead, and [`git_refusal`] says why.
///
/// **An inherited command is kept and the option ADDED to it, never replaced** — the owner's
/// decision (2026-09-19). The fleet may set an ssh command for reasons of its own (a key with
/// `-i`, a port, a proxy), and overwriting it would trade a hang for a fetch that silently uses the
/// wrong key. Which command is "inherited" follows git's own order of precedence (in git's
/// `connect.c`), because `GIT_SSH_COMMAND` outranks all of them and setting
/// it is therefore the same as discarding whichever one git would have used:
///
/// 1. `$GIT_SSH_COMMAND`, as a shell string, with the option appended;
/// 2. `core.sshCommand`, read the way this git would read it — the mirror's config when there is
///    a mirror, else the global and system files — likewise appended;
/// 3. `$GIT_SSH`, a program path rather than a shell string, so it is quoted before the option is
///    appended;
/// 4. otherwise plain `ssh`.
///
/// Appended, not inserted after the program name: the inherited value is a shell string and may
/// not begin with the program (`env FOO=1 ssh …`, a wrapper script), so the end is the only place
/// the option can go without parsing it. ssh takes the FIRST value it sees for an option, so an
/// inherited command that already says `-oBatchMode=no` keeps saying it — that is somebody's
/// explicit choice, not a default this is here to override. `src/store/sync-install.sh` has done
/// the same append, `${GIT_SSH_COMMAND:-ssh} -o BatchMode=yes`, for its own git since before this.
///
/// `StrictHostKeyChecking` is left alone on purpose: under `BatchMode` an unknown host key is a
/// refusal rather than a question, and accepting one silently is a decision nobody has made.
fn ssh_that_cannot_ask(repo: &Repo) -> String {
    let set = |name: &str| env::var(name).ok().filter(|v| !v.trim().is_empty());
    let inherited = set("GIT_SSH_COMMAND")
        .or_else(|| configured_ssh_command(repo))
        .or_else(|| set("GIT_SSH").map(|program| sh_quote(&program)))
        .unwrap_or_else(|| "ssh".to_string());
    format!("{} -oBatchMode=yes", inherited.trim_end())
}

/// `core.sshCommand` as the git about to run would see it, or `None`.
///
/// Asked of git rather than parsed out of files, because config has includes, conditional includes
/// and three scopes, and a second reader of it is a second thing to get wrong. Any failure reads
/// as "not set": the cost is `ssh` in place of a configured command, which is what git itself
/// would do with a config it could not read.
fn configured_ssh_command(repo: &Repo) -> Option<String> {
    let mut ask = Command::new("git");
    let mirror = mirror_path(&repo.id);
    if mirror_is_made(&mirror) {
        ask.arg("-C").arg(&mirror);
    }
    ask.args(["config", "--get", "core.sshCommand"]);
    let out = bounded_output(
        &mut ask,
        "git config core.sshCommand",
        Duration::from_secs(10),
    )
    .ok()?;
    if !out.status.success() {
        return None;
    }
    let command = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!command.is_empty()).then_some(command)
}

/// Did ssh refuse, rather than git or the host? The two answers `BatchMode=yes` turns a prompt
/// into: no key it could use without asking, and a host key it was not allowed to accept.
fn refused_by_ssh(stderr: &str) -> bool {
    let said = stderr.to_ascii_lowercase();
    said.contains("permission denied (publickey") || said.contains("host key verification failed")
}

/// The key an ssh command names with `-i`, if it names one.
fn ssh_identity(command: &str) -> Option<&str> {
    let mut words = command.split_whitespace();
    while let Some(word) = words.next() {
        if word == "-i" {
            return words.next();
        }
        if let Some(key) = word.strip_prefix("-i").filter(|k| !k.is_empty()) {
            return Some(key);
        }
    }
    None
}

/// Does this git failure look like one a credential would have prevented?
///
/// Deliberately a list of what git and GitHub actually say, and deliberately not "anything that
/// failed": a fetch that could not resolve `github.com` is not a missing token, and telling somebody
/// to go and store one would send them to fix the wrong thing. `repository not found` is in the list
/// because that is what GitHub answers for a private repository the caller cannot see — the same
/// words it uses for one that does not exist, which is why [`git_refusal`] offers the credential as
/// an explanation rather than asserting it.
fn refused_for_want_of_a_credential(stderr: &str) -> bool {
    let said = stderr.to_ascii_lowercase();
    [
        "terminal prompts disabled",
        "could not read username",
        "could not read password",
        "authentication failed",
        "invalid username or token",
        "repository not found",
        "403 forbidden",
    ]
    .iter()
    .any(|marker| said.contains(marker))
}

/// A failed fleet-side git run, said in skein's terms rather than only in git's.
///
/// git's last word for a missing credential is `fatal: could not read Username for
/// 'https://github.com': terminal prompts disabled`. True, and useless: it names neither the
/// repository nor anywhere a person could put a token, and the warning `start_box_inner` prints
/// around it ("is cloning from a mirror that could not be refreshed") names the box rather than the
/// credential. So this says which repository was being reached, which files skein read looking for
/// something to reach it with, and what to do — and then quotes git, because git's words are what a
/// search engine and the next person both recognise.
///
/// Only for failures that look like a credential problem. Everything else passes through unchanged,
/// so a network outage is still reported as a network outage.
pub(super) fn git_refusal(doing: &str, stderr: &str, auth: &FleetGit) -> String {
    let stderr = stderr.trim();
    if refused_by_ssh(stderr) {
        let key = match ssh_identity(&auth.ssh) {
            Some(key) => format!("the key it names, {key},"),
            None => "the key ssh picks for itself (an `IdentityFile` in its config, else its \
                     default `~/.ssh/id_*`)"
                .to_string(),
        };
        return format!(
            "{doing}: ssh refused {} and was not allowed to ask anything, because skein runs it as \
             `{}`. So {key} was only usable if it needs no passphrase or is loaded in an ssh-agent \
             this process can reach through $SSH_AUTH_SOCK, and the host's key had to be in \
             known_hosts already. Fix whichever of those it is, or register the repo by its https \
             URL, which uses a stored token instead. git said: {stderr}",
            auth.remote, auth.ssh
        );
    }
    if !refused_for_want_of_a_credential(stderr) {
        return format!("{doing}: {stderr}");
    }
    let account = match (&auth.slug, &auth.sent) {
        (_, Some(path)) => format!(
            "skein sent the token stored at {}, and it was refused — so it has expired, or it does \
             not cover this repository",
            path.display()
        ),
        (Some(slug), None) => format!(
            "skein had no GitHub credential to send for {slug}, which is exactly how a private \
             repository answers. It looks in {} for a stored token naming {slug} (the token itself \
             is then at {}), and for a read-only token at {}; neither had one. Store one under \
             GitHub in the cockpit's settings",
            stored_credentials_path().display(),
            stored_credential_path("<id>").display(),
            read_credential_path().display(),
        ),
        (None, None) => format!(
            "skein keys its GitHub credentials by `owner/name` and could not read one out of this \
             remote, so nothing in {} can be matched to it and no token was sent",
            stored_credentials_path().display(),
        ),
    };
    format!("{doing}: {account}. git said: {stderr}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{env_lock, env_pins, tempdir};

    /// A repo record pointing at a GitHub URL, with no mirror and nothing on disk but its store.
    ///
    /// Deliberately not [`registered`]: these three tests are about what skein hands the git it
    /// spawns, and `registered` makes a mirror, which means running the real git before the
    /// interesting one.
    fn at_github(home: &Path, slug: &str) -> Repo {
        let id = slug.replace('/', "-");
        serde_json::from_value(serde_json::json!({
            "id": id,
            "source": format!("https://github.com/{slug}.git"),
            "store": home.join("store/.claude").to_string_lossy(),
            "agent": "claude",
        }))
        .unwrap()
    }

    /// A `git` on `$PATH` that records the environment it was spawned with and then answers exactly
    /// what git answers when it has no credential and no terminal to ask at. Returns the log.
    fn recording_git(home: &Path, env: &mut crate::testutil::EnvPins) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let bin = home.join("bin");
        fs::create_dir_all(&bin).unwrap();
        let log = home.join("git-spawns.log");
        fs::write(
            bin.join("git"),
            format!(
                "#!/bin/sh\n{{ echo \"ARGV: $*\"; env; echo '--- end ---'; }} >> '{}'\n\
                 echo \"fatal: could not read Username for 'https://github.com': terminal prompts \
                 disabled\" >&2\nexit 128\n",
                log.display()
            ),
        )
        .unwrap();
        fs::set_permissions(bin.join("git"), fs::Permissions::from_mode(0o755)).unwrap();
        env.set(
            "PATH",
            format!("{}:{}", bin.display(), env::var("PATH").unwrap_or_default()),
        );
        log
    }

    /// The one record in a [`recording_git`]-style log whose argv contains `argv`, from its
    /// `ARGV:` line to the next one.
    ///
    /// Picked out rather than searched as a whole, because fleet-side git is no longer one spawn:
    /// [`configured_ssh_command`] asks `git config` first, and an assertion that any line of the
    /// whole log reads `GIT_TERMINAL_PROMPT=0` would be satisfied by a spawn that is not the one
    /// that talks to the remote.
    fn spawn_of(log: &str, argv: &str) -> String {
        log.split("ARGV: ")
            .find(|record| record.lines().next().is_some_and(|l| l.contains(argv)))
            .map(|record| format!("ARGV: {record}"))
            .unwrap_or_default()
    }

    /// **Fleet-side git never waits on a terminal, and says which credential was missing**
    /// (SKEIN-951).
    ///
    /// `clone_mirror` and `fetch_mirror` run git *outside* every box, so none of the credential
    /// wiring `src/box-session.sh` does reached them and nothing set `GIT_TERMINAL_PROMPT`. A
    /// private remote with no usable token therefore reached git's last resort — asking the tty —
    /// and on 2026-09-19 a `skein start` sat on `Username for 'https://github.com':` for twenty
    /// minutes with nobody watching the terminal it was asking.
    ///
    /// Two assertions, and they fail for two different reasons on purpose. The first reads the
    /// environment of the process that was actually spawned, not a string in the source: deleting
    /// `command.env("GIT_TERMINAL_PROMPT", "0")` from [`fleet_git`] is the change that makes it
    /// fail, and it is the change that brings the hang back. The second is the refusal's own text —
    /// having [`git_refusal`] pass git's stderr through unchanged is the change that makes it fail,
    /// and that is the state where a person is told "could not read Username" and nothing about
    /// where skein looked.
    #[test]
    fn fleet_side_git_refuses_rather_than_asking_a_terminal_for_a_username() {
        let _g = env_lock();
        let home = tempdir();
        let mut env = env_pins();
        env.set("SKEIN_HOME", &home);
        env.set("SKEIN_FLEET_ROOT", home.join("boxes"));
        let log = recording_git(&home, &mut env);

        let repo = at_github(&home, "acme/thing");
        let why = ensure_mirror(&repo).expect_err("this git cannot make a mirror");

        let spawned = spawn_of(
            &fs::read_to_string(&log).unwrap_or_default(),
            "clone --mirror",
        );
        assert!(
            spawned.contains("ARGV: clone --mirror"),
            "the clone is not what got recorded, so the environment below is some other git's:\n\
             {spawned}"
        );
        assert!(
            spawned.lines().any(|line| line == "GIT_TERMINAL_PROMPT=0"),
            "the git skein spawned may still ask a terminal, which is the hang itself. Its \
             environment was:\n{spawned}"
        );

        for wanted in [
            "acme/thing",
            &home.join("github-pats.json").display().to_string(),
            &home.join("github-read-token").display().to_string(),
        ] {
            assert!(
                why.contains(wanted),
                "the refusal never names {wanted}, so it cannot be acted on: {why}"
            );
        }

        // And the other direction, which is the part a decoration that fires on every failure would
        // get wrong: a fetch that could not resolve the host is not a missing token, and sending
        // somebody to store one would send them to fix the wrong thing. Making
        // `refused_for_want_of_a_credential` answer `true` unconditionally is the change that makes
        // this fail.
        let offline = git_refusal(
            "fetching thing",
            "fatal: unable to access 'https://github.com/acme/thing.git/': \
             Could not resolve host: github.com",
            &FleetGit {
                slug: Some("acme/thing".into()),
                sent: None,
                remote: "https://github.com/acme/thing.git".into(),
                ssh: "ssh -oBatchMode=yes".into(),
            },
        );
        assert!(
            !offline.contains("github-pats.json"),
            "a host that could not be resolved was reported as a credential to go and store: \
             {offline}"
        );
    }

    /// **The token sent is the one filed under this repository, and real git reads it back.**
    ///
    /// Two halves that a single-credential fixture would conflate. The first is selection: the file
    /// holds a credential for another repo *first*, so picking by position rather than by the
    /// `repos` field — which is what `gitgate::any_user_pat` does, and is SKEIN-953 — sends a token
    /// that cannot reach this remote. The second is that the wiring works at all: `git credential
    /// fill` is the real git binary, asked the same question the real fetch asks it, so a helper
    /// that is misspelled or reads the wrong variable answers nothing and the assertion fails.
    ///
    /// It also pins the path drift that [`stored_credential_path`] warns about: the token is
    /// written through that function and read back through `gitgate::credential_for`, so changing
    /// one directory name without the other fails here rather than in a refusal that points a
    /// person at a directory skein never reads.
    #[test]
    fn the_token_sent_is_the_one_filed_under_that_repository() {
        let _g = env_lock();
        let home = tempdir();
        let mut env = env_pins();
        env.set("SKEIN_HOME", &home);
        env.set("SKEIN_FLEET_ROOT", home.join("boxes"));

        fs::create_dir_all(home.join("github-pats")).unwrap();
        fs::write(
            home.join("github-pats.json"),
            serde_json::json!([
                {"id": "first", "label": "some other repo", "repos": ["acme/other"]},
                {"id": "second", "label": "this repo", "repos": ["acme/thing"]},
            ])
            .to_string(),
        )
        .unwrap();
        let wrong = "token-filed-under-the-other-repo";
        let right = "token-filed-under-this-repo";
        crate::secret::write(
            &stored_credential_path("first"),
            &crate::secret::Secret::new(wrong),
        )
        .unwrap();
        crate::secret::write(
            &stored_credential_path("second"),
            &crate::secret::Secret::new(right),
        )
        .unwrap();

        let (read_back, _) = crate::gitgate::credential_for("acme/thing")
            .expect("gitgate reads tokens back from somewhere other than stored_credential_path");
        assert_eq!(
            read_back.id, "second",
            "the credential filed under acme/thing is not the one that came back"
        );

        let repo = at_github(&home, "acme/thing");
        let mut command = Command::new("git");
        command
            .args(["credential", "fill"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        let auth = fleet_git(&mut command, &repo);
        assert_eq!(
            auth.sent,
            Some(stored_credential_path("second")),
            "skein did not choose the credential filed under the repo it is fetching"
        );

        let mut child = command.spawn().unwrap();
        {
            use std::io::Write;
            child
                .stdin
                .take()
                .unwrap()
                .write_all(b"protocol=https\nhost=github.com\npath=acme/thing.git\n\n")
                .unwrap();
        }
        let out = child.wait_with_output().unwrap();
        let filled = String::from_utf8_lossy(&out.stdout).to_string();
        assert!(
            filled.contains(&format!("password={right}")),
            "git was given no usable credential by skein's wiring; it filled:\n{filled}\nand said:\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            !filled.contains(wrong),
            "git was handed the token filed under a different repository:\n{filled}"
        );
    }

    /// **A remote skein cannot key a credential to is sent no credential at all.**
    ///
    /// `github-pats` tokens belong to a person and are filed by `owner/name`. Offering one to a host
    /// skein did not recognise would be skein deciding, on the strength of an unparsed URL, to hand
    /// somebody's PAT to whoever is on the other end. Making [`fleet_git`] fall back to
    /// `gitgate::read_pat` before it has a slug is the change that makes this fail.
    ///
    /// `GIT_TERMINAL_PROMPT` is asserted here as well, and for its own reason: the credential search
    /// returns early on this path, so a fix that set the variable *after* finding a token would
    /// leave exactly this case — a remote skein knows least about — still able to hang.
    #[test]
    fn a_remote_skein_cannot_name_is_sent_no_credential() {
        let _g = env_lock();
        let home = tempdir();
        let mut env = env_pins();
        env.set("SKEIN_HOME", &home);
        env.set("SKEIN_FLEET_ROOT", home.join("boxes"));
        crate::secret::write(
            &read_credential_path(),
            &crate::secret::Secret::new("skein-test-fleet-wide-read-token"),
        )
        .unwrap();

        let elsewhere: Repo = serde_json::from_value(serde_json::json!({
            "id": "elsewhere",
            "source": "https://git.example.invalid/acme/thing.git",
            "store": home.join("store/.claude").to_string_lossy(),
            "agent": "claude",
        }))
        .unwrap();
        let mut command = Command::new("git");
        let auth = fleet_git(&mut command, &elsewhere);
        assert_eq!(
            auth.slug, None,
            "a non-GitHub remote parsed as a repository"
        );
        assert_eq!(
            auth.sent, None,
            "skein offered a stored PAT to a host it could not name"
        );

        let sent: std::collections::HashMap<_, _> = command
            .get_envs()
            .filter_map(|(k, v)| {
                Some((
                    k.to_string_lossy().into_owned(),
                    v?.to_string_lossy().into_owned(),
                ))
            })
            .collect();
        assert_eq!(
            sent.get("GIT_TERMINAL_PROMPT").map(String::as_str),
            Some("0"),
            "a remote with no credential is the one that hangs, and this one may still prompt: \
             {sent:?}"
        );
        assert!(
            !sent.contains_key(FLEET_TOKEN_VAR),
            "a token reached a remote skein could not key it to: {sent:?}"
        );
    }

    /// **A fork's pull-request head is fetched the same way as the mirror: never a terminal, the
    /// token filed under this repository, and a refusal that names where skein looked** (SKEIN-954).
    ///
    /// [`fetch_pull_head`] is the third fleet-side network git and SKEIN-951 wired only the other
    /// two, so `review::stand_the_change_up` — the only path that serves a fork's commits — could
    /// still hang a reviewer on `Username for 'https://github.com':`. Nothing here reaches the
    /// network: the mirror is made by hand (all [`mirror_is_made`] asks for is `HEAD` and
    /// `objects/`), and the `git` on `$PATH` is a shim.
    ///
    /// The shim does two things with the environment it was SPAWNED with, so none of this reads a
    /// string in the source. It records it, and it hands it, unchanged, to the REAL git's
    /// `credential fill` — the same question the real fetch would ask — and records the answer.
    ///
    /// What makes each assertion fail:
    /// - dropping `fleet_git` from [`fetch_pull_head`], or deleting its `GIT_TERMINAL_PROMPT` line:
    ///   the recorded environment has no `GIT_TERMINAL_PROMPT=0`;
    /// - going back to `format!("fetching {} of {}: {}", …)` for the refusal: it no longer names
    ///   `github-pats.json` / `github-read-token`, nor the token file that was sent;
    /// - choosing a credential by position instead of by the repository it covers, or breaking the
    ///   helper wiring: real git fills the other repository's token, or none.
    #[test]
    fn a_forks_pull_head_is_fetched_without_a_terminal_and_with_this_repos_token() {
        use std::os::unix::fs::PermissionsExt;
        let _g = env_lock();
        let home = tempdir();
        let mut env = env_pins();
        env.set("SKEIN_HOME", &home);
        env.set("SKEIN_FLEET_ROOT", home.join("boxes"));

        // The real git, found BEFORE the shim goes on `$PATH`, for the credential half.
        let real = Command::new("sh")
            .args(["-c", "command -v git"])
            .output()
            .unwrap();
        let real = String::from_utf8_lossy(&real.stdout).trim().to_string();
        assert!(
            real.starts_with('/'),
            "no real git to check the credential with: {real:?}"
        );

        let bin = home.join("bin");
        fs::create_dir_all(&bin).unwrap();
        let log = home.join("git-spawns.log");
        let ask = home.join("credential-question");
        fs::write(
            &ask,
            "protocol=https\nhost=github.com\npath=acme/thing.git\n\n",
        )
        .unwrap();
        fs::write(
            bin.join("git"),
            format!(
                "#!/bin/sh\n{{ echo \"ARGV: $*\"; env; echo '--- end ---'; \
                 echo '--- filled ---'; '{real}' credential fill < '{ask}' 2>&1; \
                 echo '--- end filled ---'; }} >> '{log}'\n\
                 echo \"fatal: could not read Username for 'https://github.com': terminal prompts \
                 disabled\" >&2\nexit 128\n",
                ask = ask.display(),
                log = log.display()
            ),
        )
        .unwrap();
        fs::set_permissions(bin.join("git"), fs::Permissions::from_mode(0o755)).unwrap();
        env.set(
            "PATH",
            format!("{}:{}", bin.display(), env::var("PATH").unwrap_or_default()),
        );

        let repo = at_github(&home, "acme/thing");
        let mirror = mirror_path(&repo.id);
        fs::create_dir_all(mirror.join("objects")).unwrap();
        fs::write(mirror.join("HEAD"), "ref: refs/heads/main\n").unwrap();

        // No credential anywhere: the case that hung, and the one the refusal has to explain.
        let why = fetch_pull_head(&repo, 7).expect_err("this git fetches nothing");
        let spawned = spawn_of(
            &fs::read_to_string(&log).unwrap_or_default(),
            "refs/pull/7/head",
        );
        assert!(
            spawned.contains("ARGV: -C") && spawned.contains("refs/pull/7/head"),
            "the pull-head fetch is not what got recorded, so the environment below is some \
             other git's:\n{spawned}"
        );
        assert!(
            spawned.lines().any(|line| line == "GIT_TERMINAL_PROMPT=0"),
            "the git fetching a fork's pull-request head may still ask a terminal, which is the \
             hang itself. Its environment was:\n{spawned}"
        );
        for wanted in [
            "refs/pull/7/head",
            "acme/thing",
            &home.join("github-pats.json").display().to_string(),
            &home.join("github-read-token").display().to_string(),
        ] {
            assert!(
                why.contains(wanted),
                "the pull-head refusal never names {wanted}, so it cannot be acted on: {why}"
            );
        }

        // Now a token filed under ANOTHER repository first, and this repository's second.
        fs::create_dir_all(home.join("github-pats")).unwrap();
        fs::write(
            home.join("github-pats.json"),
            serde_json::json!([
                {"id": "first", "label": "some other repo", "repos": ["acme/other"]},
                {"id": "second", "label": "this repo", "repos": ["acme/thing"]},
            ])
            .to_string(),
        )
        .unwrap();
        let wrong = "token-filed-under-the-other-repo";
        let right = "token-filed-under-this-repo";
        crate::secret::write(
            &stored_credential_path("first"),
            &crate::secret::Secret::new(wrong),
        )
        .unwrap();
        crate::secret::write(
            &stored_credential_path("second"),
            &crate::secret::Secret::new(right),
        )
        .unwrap();
        fs::write(&log, "").unwrap();

        let why = fetch_pull_head(&repo, 7).expect_err("this git fetches nothing");
        let whole = fs::read_to_string(&log).unwrap_or_default();
        let spawned = spawn_of(&whole, "refs/pull/7/head");
        assert!(
            spawned.lines().any(|line| line == "GIT_TERMINAL_PROMPT=0"),
            "with a token to send, the pull-head fetch may still ask a terminal:\n{spawned}"
        );
        let filled = spawned
            .split("--- filled ---")
            .nth(1)
            .and_then(|rest| rest.split("--- end filled ---").next())
            .unwrap_or_default();
        assert!(
            filled.contains(&format!("password={right}")),
            "real git, given the environment the pull-head fetch was spawned with, filled no \
             usable credential:\n{filled}"
        );
        assert!(
            !whole.contains(wrong),
            "the pull-head fetch was handed the token filed under a different repository:\n\
             {whole}"
        );
        assert!(
            !whole
                .lines()
                .filter(|l| l.starts_with("ARGV: "))
                .any(|l| l.contains(right)),
            "the token reached git's argv, where /proc/<pid>/cmdline shows it to anyone:\n{whole}"
        );
        assert!(
            why.contains(&stored_credential_path("second").display().to_string()),
            "the refusal does not say which stored token was sent and refused: {why}"
        );
    }

    /// **An ssh remote cannot make fleet-side git wait on a terminal either, and whatever ssh
    /// command was inherited is the one that runs** (SKEIN-955).
    ///
    /// Real git, a real bare mirror whose `origin` is an `ssh://` URL, and no network: the "ssh"
    /// git runs is a script that records its argv and answers the way ssh does under `BatchMode`
    /// when a key would have needed a passphrase. So every assertion below is about the argv of the
    /// process git actually spawned for the transport — not a string in the source, and not the
    /// environment skein meant to hand over. Four inheritances, in git's own order of precedence:
    ///
    /// 1. nothing — `ssh` on `$PATH` must run WITH `-oBatchMode=yes`. Deleting the
    ///    `command.env("GIT_SSH_COMMAND", …)` line from [`fleet_git`] makes it fail;
    /// 2. `$GIT_SSH_COMMAND` — THAT command must run, with its own `-i` and the option added.
    ///    Making [`ssh_that_cannot_ask`] return a plain `ssh -oBatchMode=yes` (overwriting what was
    ///    inherited) makes it fail, because the inherited script never runs;
    /// 3. `core.sshCommand` — the same, read through git. Dropping [`configured_ssh_command`] from
    ///    the chain makes it fail;
    /// 4. `$GIT_SSH` — a program path, which must still be what runs.
    ///
    /// And the refusal has to name the remote and the key rather than pass `Permission denied
    /// (publickey)` through: removing the ssh branch of [`git_refusal`] makes that fail.
    #[test]
    fn an_ssh_remote_is_fetched_with_batch_mode_added_to_the_inherited_command() {
        use std::os::unix::fs::PermissionsExt;
        let _g = env_lock();
        let home = tempdir();
        let mut env = env_pins();
        env.set("SKEIN_HOME", &home);
        env.set("SKEIN_FLEET_ROOT", home.join("boxes"));
        env.unset("GIT_SSH_COMMAND");
        env.unset("GIT_SSH");
        let gitconfig = home.join("gitconfig");
        fs::write(&gitconfig, "").unwrap();
        env.set("GIT_CONFIG_GLOBAL", &gitconfig);
        env.set("GIT_CONFIG_NOSYSTEM", "1");

        let log = home.join("ssh-spawns.log");
        let fake_ssh = |dir: &Path, tag: &str| {
            fs::create_dir_all(dir).unwrap();
            let path = dir.join("ssh");
            fs::write(
                &path,
                format!(
                    "#!/bin/sh\necho \"{tag}: $*\" >> '{}'\n\
                     echo 'git@example.invalid: Permission denied (publickey).' >&2\nexit 255\n",
                    log.display()
                ),
            )
            .unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
            path
        };
        fake_ssh(&home.join("bin"), "path");
        let inherited = fake_ssh(&home.join("inherited"), "inherited");
        env.set(
            "PATH",
            format!(
                "{}:{}",
                home.join("bin").display(),
                env::var("PATH").unwrap_or_default()
            ),
        );

        let remote = "ssh://git@example.invalid/acme/thing.git";
        let repo: Repo = serde_json::from_value(serde_json::json!({
            "id": "thing",
            "source": remote,
            "store": home.join("store/.claude").to_string_lossy(),
            "agent": "claude",
        }))
        .unwrap();
        let mirror = mirror_path(&repo.id);
        fs::create_dir_all(mirror.parent().unwrap()).unwrap();
        for args in [
            vec!["init", "--bare", "-q"],
            vec!["remote", "add", "origin", remote],
        ] {
            let mut git = Command::new("git");
            if args[0] == "init" {
                git.args(&args).arg(&mirror);
            } else {
                git.arg("-C").arg(&mirror).args(&args);
            }
            assert!(git.status().unwrap().success(), "git {args:?} failed");
        }

        // Each case returns the ssh lines it caused, and the refusal.
        let fetch = || {
            fs::write(&log, "").unwrap();
            let why = fetch_mirror(&repo).expect_err("no ssh here lets anything through");
            (fs::read_to_string(&log).unwrap_or_default(), why)
        };

        // 1. Nothing inherited.
        let (ran, why) = fetch();
        assert!(
            ran.lines()
                .any(|l| l.starts_with("path: ") && l.contains("-oBatchMode=yes")),
            "with nothing inherited, ssh ran without -oBatchMode=yes, so a key with a passphrase \
             asks the terminal:\n{ran}"
        );
        for wanted in [remote, "BatchMode=yes", "ssh-agent", "IdentityFile"] {
            assert!(
                why.contains(wanted),
                "the ssh refusal never names {wanted}, so it cannot be acted on: {why}"
            );
        }

        // 2. `$GIT_SSH_COMMAND`, with a key of its own.
        let key = home.join("keys/deploy");
        env.set(
            "GIT_SSH_COMMAND",
            format!("'{}' -i {}", inherited.display(), key.display()),
        );
        let (ran, why) = fetch();
        assert!(
            ran.lines().any(|l| l.starts_with("inherited: ")
                && l.contains(&format!("-i {}", key.display()))
                && l.contains("-oBatchMode=yes")),
            "the inherited GIT_SSH_COMMAND did not run with its own -i AND -oBatchMode=yes — it \
             was overwritten, or the option was not added:\n{ran}"
        );
        assert!(
            !ran.lines().any(|l| l.starts_with("path: ")),
            "a plain ssh ran in place of the inherited GIT_SSH_COMMAND:\n{ran}"
        );
        assert!(
            why.contains(&key.display().to_string()),
            "the refusal does not name the key the inherited command offered: {why}"
        );
        env.unset("GIT_SSH_COMMAND");

        // 3. `core.sshCommand`.
        let configured = home.join("keys/configured");
        fs::write(
            &gitconfig,
            format!(
                "[core]\n\tsshCommand = '{}' -i {}\n",
                inherited.display(),
                configured.display()
            ),
        )
        .unwrap();
        let (ran, _) = fetch();
        assert!(
            ran.lines().any(|l| l.starts_with("inherited: ")
                && l.contains(&format!("-i {}", configured.display()))
                && l.contains("-oBatchMode=yes")),
            "core.sshCommand was not the command that ran with -oBatchMode=yes added:\n{ran}"
        );
        fs::write(&gitconfig, "").unwrap();

        // 4. `$GIT_SSH`, a program path rather than a shell string.
        env.set("GIT_SSH", &inherited);
        let (ran, _) = fetch();
        assert!(
            ran.lines()
                .any(|l| l.starts_with("inherited: ") && l.contains("-oBatchMode=yes")),
            "the inherited GIT_SSH program was not what ran with -oBatchMode=yes added:\n{ran}"
        );
    }
}
