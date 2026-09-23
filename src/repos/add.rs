//! Adding a repo and bringing one up to date, and the one `gh` login skein still reads.

use super::*;

/// Add a repo to skein: mirror its remote, provision its shared store and skein's kit, seed gh
/// auth, and record it in `repos.json`. Returns the stored `Repo`. This is the whole
/// `skein add <git-url>` flow; the box launch then needs nothing from the repo.
pub fn add_repo(
    source: &str,
    id: Option<&str>,
    agent: Option<&str>,
    store: Option<&str>,
) -> Result<Repo, String> {
    if let Some(runtime) = agent.map(str::trim).filter(|value| !value.is_empty()) {
        if !valid_runtime(runtime) {
            return Err(format!(
                "unsupported runtime {runtime:?}; available: {}",
                supported_runtimes()
                    .iter()
                    .map(|r| r.id)
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
    }
    let id = id
        .map(|s| s.to_string())
        .unwrap_or_else(|| repo_id_from_source(source));
    if id.is_empty() {
        return Err("could not derive a repo id — pass one explicitly".into());
    }
    // **This is the one place a repo id is ever minted**, so it is the one place the id has to be
    // checked — and everything that later joins an id onto a path (`mirror_path` here,
    // `prq::review_dir`, `prwork`'s three workflow files, `moduledocs`) reads it back out of
    // `repos.json` and is safe by that. The alternative, a guard at each join, is the shape that
    // left two review routes writing outside `~/.skein`.
    //
    // `POST /api/repos {"id": "../../x"}` wrote a bare git mirror at `<home>/repos/../../x/mirror`
    // before this line existed; reproduced against a running server, not reasoned.
    if !valid_name(&id) {
        return Err(repo_id_refusal(&id));
    }
    let home = skein_home();
    // The repo's shared-data folder (its `.claude` store), shared live across all the repo's boxes —
    // cross-box memory/mailbox/skills/statusline. The caller may point it at an existing rich store
    // (e.g. thing's `skein-shared/.claude`); otherwise skein manages one under its home. Either way
    // `ensure_store` is idempotent (adds the probe, seeds only what's absent), so an existing store is
    // adopted, not clobbered.
    let store = match store.map(str::trim).filter(|s| !s.is_empty()) {
        Some(s) => PathBuf::from(expand_tilde(s)),
        None => home.join("repos").join(&id).join("store").join(".claude"),
    };

    // **A repo is a remote, and a path is not one.** Adopting a local checkout is no longer
    // supported: skein runs inside the fleet sandbox, where no host checkout is reachable at all, so
    // a path-registered repo could not be fetched, could not seed the gitignored files that were its
    // only remaining reason to exist, and differed from a URL repo in nothing a box could observe.
    //
    // Refused rather than resolved. Reading `git remote get-url origin` out of the directory and
    // registering THAT would be skein silently substituting something for what a person typed — and
    // a `source` that disagreed with what its mirror fetches is exactly the state that took a repo's
    // fetch down while its clones went on working, invisibly, until somebody looked.
    if !registrable_source(source) {
        return Err(format!(
            "{source} is a path, and skein registers repos by remote. skein runs inside the fleet \
             sandbox and cannot reach a checkout on your machine, so a path-registered repo has \
             nothing to fetch from.\n  Give the remote instead — `git -C {source} remote get-url \
             origin` prints it."
        ));
    }
    // **A store the fleet would refuse to mount is refused here, before anything is written** —
    // the kit, the store, the mirror, `repos.json`. The mounter's rule is the one asked
    // (`fleet::volume_exposure`), not a copy of it: `add` used to check only that the path was
    // absolute, so `--store ~/.skein/thing` registered cleanly and every box of the repo came up
    // with no store and nothing saying so (SKEIN-943). skein's own default is under `repos/`, which
    // the fleet does mount, so it passes the same question rather than being excused from it.
    if let Some(why) = store_refusal(&store, &id) {
        return Err(why);
    }
    ensure_kit()?;
    ensure_store(&store)?;

    let repo = Repo {
        id: id.clone(),
        source: source.to_string(),
        store: store.to_string_lossy().into_owned(),
        // A repo skein has just been told about reads nothing on its own until somebody says so.
        read_prs: false,
        // And it acts on nothing. Spelled out rather than defaulted, because `add` is the one place
        // a repo's starting state is DECIDED: everything else that builds a `Repo` is a fixture or
        // a file being read back. The rule for the whole feature — "I will toggle on when needed.
        // So no default" — is this line.
        auto_review: false,
        auto_review_on: default_auto_review_on(),
        auto_review_ceiling: Ceiling::default(),
        auto_review_authors: default_auto_review_authors(),
        auto_review_dry_run: false,
        // Never said, which is the whole set — see `Repo::owed_checks`. `add` decides a repo's
        // starting state, and the state a reviewer wants on a repo nobody has configured is the
        // one where a deletion is audited before it is approved.
        owed_checks: None,
        agent: agent
            .map(|s| s.to_string())
            .unwrap_or_else(|| load_config().default_agent),
        plane_project: String::new(),
        // One connection ⇒ adopt it, so a single-tracker fleet needs no ceremony per repo. Two or
        // more ⇒ leave it unset: which backlog this repo belongs to is not skein's guess to make,
        // and a wrong one mints a real credential against the wrong Plane.
        sync_connection: match load_connections().as_slice() {
            [only] => only.id.clone(),
            _ => String::new(),
        },
        // **Off for a repo skein has just met.** Every repo with the queue on costs one batched
        // GraphQL request per refresh, five membership searches inside it, on the badge's cadence —
        // and a fleet of eight repos spends all of that to answer a question asked about one.
        // Measured, not supposed: a live fleet had eight on, exceeded GitHub's rate limit for its
        // user, and the queue anybody was actually watching came back empty because of it.
        //
        // On was the right default for the first repo anybody registers and wrong by the third, and
        // the cost of the two mistakes is not symmetric: a queue switched off is one dropdown away
        // and says so on the repo's own row, while a queue switched on quietly spends somebody's
        // rate limit on pull requests that are none of their business.
        //
        // Note what this does NOT change: an existing `repos.json` that never wrote the field keeps
        // reading as ON (see the field's serde default). A fleet that has been working must not
        // have its queue turned off by an upgrade — that would be skein deciding, silently, that
        // the thing you were watching yesterday is not worth watching today.
        review_queue: false,
        // **On**, and it is the one switch here whose new-repo state matches its serde default.
        // The three above spend money or act on their own, so they wait to be asked; this one only
        // decides whether a box of this repo can be reached by the fleet it lives in without a
        // round trip through a vendor's servers. Registering a repo and finding its boxes deaf to
        // `SendMessage` would read as broken, and the fix — see the field — is a mount, so it
        // would also need a restart to take effect.
        peer_messaging: true,
        sync_gateway_url: String::new(),
    };
    // Before registering it: a repo whose boxes cannot clone is a repo that looks added and does
    // not work, and the failure would surface later as a box that never starts. This is the one
    // clone that happens, where there used to be two.
    ensure_mirror(&repo)?;
    update_repos(|repos| {
        repos.retain(|r| r.id != id); // replace any existing entry with the same id
        repos.push(repo.clone());
        repos.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(repo.clone())
    })
}

/// Why `add` must refuse this store, if the fleet would decline to mount it — worded for the person
/// who typed it, with where a store can go instead. `None` when the fleet would mount it.
fn store_refusal(store: &Path, id: &str) -> Option<String> {
    let path = store.to_string_lossy();
    let home = skein_home();
    let home = home.display();
    let relation = match crate::fleet::volume_exposure(&path)? {
        crate::fleet::VolumeExposure::Contains => "contains",
        crate::fleet::VolumeExposure::Inside => "is inside",
    };
    Some(format!(
        "{path} {relation} skein's own volume ({home}), and skein never mounts that into a box — a \
         box given it would read the API token, the GitHub credentials and every other box's \
         state. So this repo's boxes would come up with no store.\n  Give --store a folder \
         outside {home}, or leave --store out and skein keeps the store at \
         {home}/repos/{id}/store/.claude."
    ))
}

/// Bring a repo up to date: **the mirror, which is the whole of the job.**
///
/// The mirror is what every box clones from, so advancing it is the part that changes what a new box
/// starts with — and a repo is a remote, so there is nothing else on this machine to advance.
///
/// There used to be a second step for a repo adopted from a local path: `git -C <source_tree> pull
/// --ff-only` on somebody else's working tree. It went with local-path repos, and nothing about
/// what a box gets went with it.
pub fn pull_repo(id: &str) -> Result<String, String> {
    let repo = load_repos()
        .into_iter()
        .find(|r| r.id == id)
        .ok_or_else(|| format!("no repo with id {id:?}"))?;
    // The mirror always has somewhere to fetch from: its `origin` is the remote the repo was
    // registered by, and that is the whole of what `pull` means now. There used to be a second half
    // here — fast-forward the user's own checkout — which only ever applied to an adopted repo and
    // could only run on a host.
    fetch_mirror(&repo)?;
    Ok("Mirror updated.".into())
}

/// The token `gh` is logged in with here, or `None` if it has none.
///
/// **The review queue's last resort, and now its only caller.** It was extracted for the fleet-wide
/// seeding as well; that seeding is deleted (§13a's machine-global secret store), so what is left is
/// `prq::host_credential`'s final arm — the one that only answers on a machine where somebody has
/// run `gh auth login`. Inside the fleet that is usually nobody, and `host_token`'s refusal already
/// names the three ways to hand the queue a credential instead.
///
/// **Bounded, and worth being last.** `gh` keeps its token in the system keyring on a modern Linux,
/// so this can unlock one. Every caller should try the sources that cost nothing first and reach
/// this only when they would otherwise have no credential at all.
pub fn gh_cli_token() -> Option<String> {
    let mut command = Command::new("gh");
    command.args(["auth", "token"]);
    let out = bounded_output(&mut command, "gh auth token", Duration::from_secs(15)).ok()?;
    if !out.status.success() {
        return None;
    }
    let token = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!token.is_empty()).then_some(token)
}

/// Was the fleet-wide GitHub secret ever seeded into this machine's `sbx` store?
///
/// **A reader with no writer, deliberately.** The seeding itself is gone — it was `sbx secret set -g`
/// on the host, and §13a deletes the machine-global store because two fleets on one host shared one
/// token through it. The marker file it left behind travels with the volume, so a fleet seeded
/// before that deletion still has a credential in front of its boxes, and this is the only evidence
/// of it. `gitgate::box_credential` reads it to decide whether to claim `Account`, and `skein
/// doctor` reports it. Nothing writes it any more, and a fleet that has never had one never will.
pub fn gh_secret_seeded() -> Option<String> {
    fs::read_to_string(crate::config::skein_home().join("gh-secret-seeded"))
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repos::testkit::*;
    use crate::testutil::{env_lock, env_pins, tempdir};
    use crate::util::expand_tilde;

    /// **A repo is a remote, and a path is refused rather than resolved.**
    ///
    /// skein runs inside the fleet sandbox, where no checkout on the host is reachable — so a
    /// path-registered repo has nothing to fetch from, cannot seed the gitignored files that were
    /// its last remaining purpose, and differs from a URL repo in nothing a box can observe.
    ///
    /// Refused and NOT resolved. Reading `git remote get-url origin` out of the directory and
    /// registering that would be skein silently substituting something for what a person typed, and
    /// a `source` disagreeing with what its mirror fetches is the exact state that took a repo's
    /// fetch down while its clones went on working — invisible until somebody looked (2026-08-30).
    ///
    /// The message has to carry the way forward, or it is a refusal a person cannot act on.
    #[test]
    fn a_repo_registered_from_a_path_is_refused_and_told_what_to_pass_instead() {
        let _g = env_lock();
        let home = tempdir();
        let mut env = env_pins();
        env.set("SKEIN_HOME", &home);
        let checkout = tempdir();
        origin_repo(&checkout);

        let why = add_repo(
            &checkout.to_string_lossy(),
            Some("proj"),
            Some("claude"),
            None,
        )
        .expect_err("a path must not register");
        assert!(
            why.contains("registers repos by remote") && why.contains("remote get-url origin"),
            "the refusal does not say what to pass instead, so it cannot be acted on: {why}"
        );
        assert!(
            load_repos().is_empty(),
            "the repo was refused and registered anyway"
        );

        // Non-vacuity, WITHOUT touching the network: a URL gets past the path check and fails
        // later, at the clone. What matters is which check rejected it — a bare `is_err()` here
        // would pass just as well if `add_repo` refused everything.
        let later = add_repo(
            "https://github.com/acme/thing.git",
            Some("thing"),
            Some("claude"),
            None,
        )
        .expect_err("no such repository exists to clone");
        assert!(
            !later.contains("registers repos by remote"),
            "a URL was rejected by the path check, so the refusal above proves nothing: {later}"
        );
    }

    /// **`--store` is refused at add time when the fleet would refuse to mount it** (SKEIN-943), and
    /// a refused add writes nothing — no kit, no store, no record.
    ///
    /// The four shapes a person can type that land in the volume: a directory of credentials inside
    /// it, the same spelled through `~` and `..`, a path under `repos/` that `..` walks back out of
    /// (textually "under repos", really beside the credentials), and a symlink outside the volume
    /// that points into it. Plus a directory holding the volume, which is worded "contains".
    ///
    /// What fails each, planted and watched: deleting the `store_refusal` call in `add_repo` fails
    /// the exact-message `assert_eq!` on the first case; asking `volume_exposure` only the textual
    /// question fails the `..`-out-of-repos case's `assert_eq!`, with the clone's error where the
    /// refusal should be — the add got all the way to the network.
    #[test]
    fn a_store_inside_skeins_volume_is_refused_at_add_and_nothing_is_written() {
        let _g = env_lock();
        let user = tempdir();
        let home = user.join(".skein");
        std::fs::create_dir_all(&home).unwrap();
        let mut env = env_pins();
        env.set("SKEIN_HOME", &home).set("HOME", &user);
        // Never reached: every case here must be refused before the clone.
        let source = "https://127.0.0.1:1/thing.git";
        let shown = home.display().to_string();
        let refusal = |path: &str, relation: &str| {
            format!(
                "{path} {relation} skein's own volume ({shown}), and skein never mounts that into a \
                 box — a box given it would read the API token, the GitHub credentials and every \
                 other box's state. So this repo's boxes would come up with no store.\n  Give \
                 --store a folder outside {shown}, or leave --store out and skein keeps the store \
                 at {shown}/repos/thing/store/.claude."
            )
        };

        let pats = home.join("github-pats").to_string_lossy().into_owned();
        let why = add_repo(source, Some("thing"), Some("claude"), Some(&pats))
            .expect_err("a store among the credentials was accepted");
        assert_eq!(why, refusal(&pats, "is inside"));

        // Through `~` and `..`: shown as expanded, which is the path skein would have used.
        let typed = "~/.skein/../.skein/github-pats";
        let why = add_repo(source, Some("thing"), Some("claude"), Some(typed))
            .expect_err("a store spelled through ~ and .. was accepted");
        assert_eq!(why, refusal(&expand_tilde(typed), "is inside"));

        // Textually under `repos/`, which the fleet mounts; really beside the credentials.
        let back_out = format!("{}/repos/../github-pats", home.display());
        let why = add_repo(source, Some("thing"), Some("claude"), Some(&back_out))
            .expect_err("a store that .. walks out of repos/ was accepted");
        assert_eq!(why, refusal(&back_out, "is inside"));

        // A symlink outside the volume, into it.
        let outside = tempdir();
        std::os::unix::fs::symlink(&home, outside.join("link")).unwrap();
        let via_link = outside
            .join("link/github-pats")
            .to_string_lossy()
            .into_owned();
        let why = add_repo(source, Some("thing"), Some("claude"), Some(&via_link))
            .expect_err("a store reached through a symlink into the volume was accepted");
        assert_eq!(why, refusal(&via_link, "is inside"));

        // A directory holding the volume.
        let above = user.to_string_lossy().into_owned();
        let why = add_repo(source, Some("thing"), Some("claude"), Some(&above))
            .expect_err("a store holding the volume was accepted");
        assert_eq!(why, refusal(&above, "contains"));

        assert!(
            !home.join("github-pats").exists() && !home.join("kit").exists(),
            "a refused add scaffolded something anyway"
        );
        assert!(load_repos().is_empty(), "a refused add was registered");
    }

    /// **skein's own default store is under the volume and is NOT refused**, and neither is a store
    /// the person keeps outside it — the reason `--store` exists.
    ///
    /// Both get past the guard and fail later, at the clone of an address that answers nothing, and
    /// the proof they got past it is that the store was scaffolded: `ensure_store` runs after the
    /// guard and before the clone. Flipping `exposure_among` to refuse everything under the volume
    /// (dropping the `repos/`/`boxes/` exception) fails the first assertion here.
    #[test]
    fn the_default_store_and_one_outside_the_volume_are_accepted() {
        let _g = env_lock();
        let home = tempdir();
        let mut env = env_pins();
        env.set("SKEIN_HOME", &home);
        let source = "https://127.0.0.1:1/thing.git";

        let why = add_repo(source, Some("thing"), Some("claude"), None)
            .expect_err("nothing answers at that address, so the clone fails");
        assert!(
            !why.contains("skein's own volume") && home.join("repos/thing/store/.claude").is_dir(),
            "skein's default store was refused, or not scaffolded: {why}"
        );

        let elsewhere = tempdir().join("shared/.claude");
        let why = add_repo(
            source,
            Some("other"),
            Some("claude"),
            Some(&elsewhere.to_string_lossy()),
        )
        .expect_err("nothing answers at that address, so the clone fails");
        assert!(
            !why.contains("skein's own volume") && elsewhere.is_dir(),
            "a store outside the volume was refused, or not scaffolded: {why}"
        );
    }

    /// **Nothing in this tree writes a machine-global secret**, and that is the deletion, not a
    /// tidy-up.
    ///
    /// `ensure_gh_secret` ran `sbx secret set -g github -t <token>` once per *machine*. Two things
    /// were wrong with it and only one is obvious: the token rode in on argv, readable from the
    /// host's process table (`github`'s `--config -` document exists to close exactly that), and the
    /// store it wrote to belonged to the machine rather than to the fleet — so two fleets on one
    /// host shared one credential, which architecture §13a is the decision to stop.
    ///
    /// Asserted over the source because the property is an ABSENCE, and an absence has no call to
    /// observe. Same instrument, and the same reason, as `deployment`'s "decided by one variable and
    /// nothing else". `#[cfg(test)]` bodies are cut first: a fixture may spell the command it is
    /// standing in for.
    ///
    /// **The scan asserts on itself.** A matcher that read nothing, or that had been pointed at an
    /// empty directory, would pass by having nothing to compare — so the control is a string this
    /// module really does still contain.
    #[test]
    fn skein_no_longer_writes_a_secret_that_belongs_to_the_machine_rather_than_the_fleet() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut read = 0usize;
        let mut control = false;
        let mut offenders: Vec<String> = Vec::new();
        let mut walk = vec![root.clone()];
        while let Some(dir) = walk.pop() {
            for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
                let path = entry.path();
                if path.is_dir() {
                    walk.push(path);
                    continue;
                }
                if path.extension().is_none_or(|e| e != "rs") {
                    continue;
                }
                let body = std::fs::read_to_string(&path).unwrap_or_default();
                // Production only. The test module below is where a fake `sbx` is allowed to spell
                // the very command this forbids.
                let production: String = body
                    .lines()
                    .take_while(|l| !l.starts_with("#[cfg(test)]"))
                    .collect::<Vec<_>>()
                    .join("\n");
                read += 1;
                if production.contains("gh_secret_seeded") {
                    control = true;
                }
                // Code, not prose. Both halves matter and the second was learned here: the first
                // run of this matched five files, every one of them a doc comment SAYING the
                // command is gone — including this one, which had spelled its own needle. That is
                // `tools/residue-check.py`'s rule ("nothing in this file may spell what it looks
                // for") one level down, so the needle is built at run time from fragments.
                let argv_shape = format!("{q}secret{q}, {q}set{q}", q = '"');
                for line in production.lines() {
                    if line.trim_start().starts_with("//") {
                        continue;
                    }
                    if line.contains(&argv_shape) {
                        offenders.push(format!("{}: {}", path.display(), line.trim()));
                    }
                }
            }
        }
        assert!(
            read > 20 && control,
            "the scan read {read} files and {} find its own control, so it proves nothing",
            match control {
                true => "did",
                false => "did not",
            }
        );
        assert!(
            offenders.is_empty(),
            "the machine-global secret store is back, and with it two fleets on one host sharing \
             one token (architecture §13a, docs/parity.md §7): {}",
            offenders.join("; ")
        );
    }
}
