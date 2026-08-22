//! Where skein itself is running: on the host, or inside the fleet it operates.
//!
//! `docs/delivery.md` §2 calls moving skein inside the sandbox "the only point of no return", and
//! §4c's rule for landing it is that it arrives **with host-driven mode still working one
//! environment variable away**. This is that variable, and the one place to ask about it.
//!
//! ## Why it is declared rather than detected
//!
//! Every detection anybody would write is a guess about somebody else's machine — is `/run/sandbox`
//! there, is `sbx` on `$PATH`, does this look like a container. Each is true of things that are not
//! a skein fleet and false of things that are, and each fails in the direction that costs most: a
//! host that decided it was in-fleet would stop running `sbx` for a fleet only it can reach.
//!
//! The deeper reason is that the two deployments do not differ in one fact to be sniffed. §2 lists
//! six things the move invalidates at once — the credential boundary, API authentication,
//! `pick-path`, the forwarded ssh-agent, `sbx` availability, and the review queue's credential path.
//! A deployment is a set of decisions somebody made, and the honest way to learn it is to be told.
//!
//! ## What it is not
//!
//! **Not "is a fleet configured".** `board.rs` has an `in_fleet` local meaning exactly that — a
//! fleet sandbox is named in the config — and it is a different question wearing the same words. A
//! host with a perfectly good fleet is host-driven; a process inside that fleet is not. Confusing
//! them would put skein's own location downstream of a config field a box can see.
//!
//! ## Reading it
//!
//! Read from the environment on each call rather than latched at startup. It cannot change under a
//! running process, so the latch would only ever be an optimisation — and it would be one paid for
//! by making the deployment untestable, since a test that cannot set it has to be told what to
//! believe. `crate::doorway::inherited_only` reads its own variable the same way, for the same
//! reason and on the same kind of path.

/// The variable, and the only thing that decides.
pub const IN_FLEET: &str = "SKEIN_IN_FLEET";

/// Where this process is running.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Deployment {
    /// skein is on the host and reaches the fleet through `sbx`. Everything skein has ever done.
    HostDriven,
    /// skein is a process inside the fleet sandbox, beside the boxes it operates.
    InFleet,
}

/// Where skein is running, as declared.
///
/// **Anything but exactly `1` is host-driven**, including a value somebody meant as true — `yes`,
/// `true`, an empty string left by a shell. That is not strictness for its own sake: the two wrong
/// answers are not equally bad. A fleet process that believes it is on the host tries to run `sbx`,
/// fails, and says so. A host process that believes it is in the fleet stops running `sbx` for a
/// fleet nothing else can reach, and the symptom is a fleet that appears to have no boxes.
pub fn deployment() -> Deployment {
    match std::env::var(IN_FLEET).as_deref() {
        Ok("1") => Deployment::InFleet,
        _ => Deployment::HostDriven,
    }
}

/// Whether skein is inside the fleet it operates.
pub fn in_fleet() -> bool {
    deployment() == Deployment::InFleet
}

impl Deployment {
    /// The word a surface prints.
    pub fn label(self) -> &'static str {
        match self {
            Deployment::HostDriven => "host-driven",
            Deployment::InFleet => "in-fleet",
        }
    }

    /// What this deployment means for the person reading it, in one sentence.
    ///
    /// Said rather than left to the label, because "in-fleet" tells somebody nothing about what
    /// they can expect to work. The six things §2 says the move invalidates are the ones people
    /// will meet, and two of them — the host's file picker and the host's keyring — are surfaces
    /// somebody clicks rather than errors they read.
    pub fn implies(self) -> &'static str {
        match self {
            Deployment::HostDriven => {
                "skein is on the host and reaches the fleet with `sbx`; the host's file picker, \
                 keyring and ssh-agent are all available to it"
            }
            Deployment::InFleet => {
                "skein is inside the sandbox beside the boxes; `sbx` is not reachable from here, \
                 and neither is the host's file picker, keyring or ssh-agent"
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The default is the deployment skein has always had, and every other value lands there too.
    ///
    /// The asymmetry is the point and it is worth stating where somebody changing this will read
    /// it: a fleet process that wrongly thinks it is on the host runs `sbx`, fails, and reports a
    /// failure somebody can act on. A host process that wrongly thinks it is in the fleet stops
    /// running `sbx` at all — and the fleet it was managing appears to have no boxes in it.
    #[test]
    fn anything_that_is_not_a_deliberate_yes_is_the_deployment_skein_has_always_had() {
        let _g = crate::testutil::env_lock();
        std::env::remove_var(IN_FLEET);
        assert_eq!(
            deployment(),
            Deployment::HostDriven,
            "unset must be the host"
        );
        for near_miss in ["", "0", "true", "yes", "1 ", "on", "In-Fleet"] {
            std::env::set_var(IN_FLEET, near_miss);
            assert_eq!(
                deployment(),
                Deployment::HostDriven,
                "{near_miss:?} was read as in-fleet, and a host that believes that stops reaching \
                 its own fleet"
            );
        }
        std::env::set_var(IN_FLEET, "1");
        assert_eq!(deployment(), Deployment::InFleet);
        assert!(in_fleet());
        std::env::remove_var(IN_FLEET);
    }

    /// The deployment is declared, never sniffed. A detector would be a guess about somebody else's
    /// machine, and this is the assertion that keeps one from being added quietly.
    #[test]
    fn where_skein_runs_is_decided_by_one_variable_and_nothing_else() {
        let me = include_str!("deployment.rs");
        let body: Vec<&str> = me
            .lines()
            .take_while(|l| !l.starts_with("#[cfg(test)]"))
            .filter(|l| !l.trim_start().starts_with("//"))
            .collect();
        let source = body.join("\n");
        assert!(
            source.contains("std::env::var(IN_FLEET)"),
            "the variable is no longer what decides"
        );
        // The *shape* of a guess, not the words for one. An earlier version of this looked for
        // "sbx" and failed on `implies()`, which names it in a sentence — describing the deployment
        // is exactly what this module is for. What must not appear is skein going and looking:
        // spawning something, or asking the filesystem what it is standing in.
        for sniff in [
            "Command::new",
            ".exists()",
            "fs::metadata",
            "read_to_string",
            "canonicalize",
        ] {
            assert!(
                !source.contains(sniff),
                "this module started looking around ({sniff:?}) instead of being told where it is"
            );
        }
    }

    /// Everywhere the deployment is allowed to change what skein does, and nothing else.
    ///
    /// The point of no return is a set of changes, not one, and they land one at a time
    /// (`docs/delivery.md` §4c). What makes that safe is being able to answer "what does this flag
    /// change so far" at any moment — and the answer degrades to "grep and hope" the first time
    /// somebody adds a branch without saying so. This is the list, and the test below is what
    /// keeps it true.
    ///
    /// Each entry is a unit, not a line, because line numbers move. Adding one is the point at
    /// which somebody decides the deployment may decide this too.
    const CONSULTED_BY: &[(&str, &str)] = &[
        (
            "config",
            "loading the SSH key into an agent. The agent is reachable in-fleet \u{2014} sbx \
             forwards the host's into the sandbox, and that forward belongs to the sandbox rather \
             than to skein \u{2014} but the key *file* is a path on the host. Refuses with where to \
             run it, because failing on the file instead reads as a mistyped path.",
        ),
        (
            "gitgate",
            "whether a box holds the account token. Both halves of the seeding are the host's, so \
             in-fleet it is not there unless it was seeded before the move \u{2014} and the \
             `gh-secret-seeded` marker, which travels with the volume, is the evidence. Reported as \
             unseeded rather than as the token, because the label is what the first-run checklist \
             reads as \"boxes can push\".",
        ),
        (
            "fleet",
            "two host-only calls, answered from inside rather than refused. The fleet agent's port \
             is not published at all in-fleet \u{2014} the agent is on loopback at the port it \
             listens on, and publishing would forward a port to the machine skein is standing on. \
             And `skein login` runs its command directly instead of through `sbx exec -it`; the \
             terminal is still the user's either way, which is why it is an attached run.",
        ),
        (
            "sbx",
            "`sbx ls` asks about the host's machine, which skein-in-fleet is not standing on. It \
             returns the same `None` that a missing or wedged sbx returns \u{2014} callers already \
             fall back to the registry \u{2014} but records the reason, or the board reports a \
             broken sbx for a deployment where its absence is correct. `$SKEIN_LS_CMD` still wins: \
             something that can answer the question is answering it.",
        ),
        (
            "repos",
            "the fleet's GitHub secret is seeded from the host on both halves \u{2014} `gh auth \
             token` reads the host's login, `sbx secret set` writes the host's keyring. Refuses \
             in-fleet with what to do instead, rather than succeeding quietly: seeding is how boxes \
             get a credential, so a silent success is a 403 inside a box minutes later.",
        ),
        (
            "health",
            "two lines, and they answer opposite ways. A missing `sbx` is a fault on a host and \
             correct in the fleet: reporting it red there would hand somebody a fault they cannot \
             clear, and hide behind a false alarm the thing they want to know \u{2014} that this \
             deployment reaches boxes another way. An unreachable **warden** is a fault in BOTH, \
             and only its fix changes: on the host it is not running, while in the fleet the \
             default address is the sandbox's own loopback rather than the host's, so \
             `$SKEIN_WARDEN` is the thing to look at. The two failures are indistinguishable from \
             in here and only one of them is fixed by starting something.",
        ),
        (
            "place",
            "the first hop of a crossing. Host-driven it is `sbx exec [flags] <sandbox>`; in-fleet \
             it is nothing, because skein is already in the sandbox and `sbx` is host-only. Also \
             refuses a box whose sandbox is its own \u{2014} that has no second hop, so dropping \
             the first as well would run the command in skein's own sandbox instead.",
        ),
        (
        "bin/skein",
        "`skein doctor` reports which deployment it is and what is reachable from it. Reporting \
         only \u{2014} the first caller, and deliberately one that changes no behaviour, so the \
         seam exists before anything leans on it. Also drops `sbx` from the host-tools list it \
             checks for, for the reason `health` gives.",
        ),
    ];

    /// The flag cannot acquire meaning quietly.
    ///
    /// Scanned from the source rather than trusted, and the scan asserts on itself: a matcher that
    /// found nothing would make this pass by having nothing to compare.
    #[test]
    fn what_the_deployment_changes_is_written_down_and_nothing_more() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut found: Vec<String> = Vec::new();
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
                let unit = path
                    .strip_prefix(&root)
                    .unwrap()
                    .with_extension("")
                    .to_string_lossy()
                    .into_owned();
                if unit == "deployment" {
                    continue; // the module itself, which is where the question lives
                }
                let body = std::fs::read_to_string(&path).unwrap_or_default();
                // The two spellings that make a decision. Naming the module in a doc comment is
                // describing the design, not branching on it.
                let asks = body
                    .lines()
                    .filter(|l| !l.trim_start().starts_with("//"))
                    .any(|l| {
                        l.contains("deployment::in_fleet(") || l.contains("deployment::deployment(")
                    });
                if asks {
                    found.push(unit);
                }
            }
        }
        assert!(
            !found.is_empty(),
            "the scan found no caller at all, so this test proves nothing"
        );
        let declared: Vec<&str> = CONSULTED_BY.iter().map(|(unit, _)| *unit).collect();
        for unit in &found {
            assert!(
                declared.contains(&unit.as_str()),
                "`{unit}` decides something on where skein is running and CONSULTED_BY does not \
                 say so. Add it with what it changes \u{2014} the move lands one change at a time, \
                 and a list nobody updates is how it stops being one."
            );
        }
        for (unit, _) in CONSULTED_BY {
            assert!(
                found.contains(&unit.to_string()),
                "CONSULTED_BY says `{unit}` asks where skein is running, and it no longer does \
                 \u{2014} a stale entry is a permission nobody granted"
            );
        }
    }

    /// A label nobody can act on is half a report. Both say what is reachable from where skein is,
    /// because that is the question behind every one of §2's six invalidations.
    #[test]
    fn each_deployment_says_what_it_means_for_the_person_reading_it() {
        for (deployment, expect) in [
            (Deployment::HostDriven, "host-driven"),
            (Deployment::InFleet, "in-fleet"),
        ] {
            assert_eq!(deployment.label(), expect);
            let implies = deployment.implies();
            assert!(
                implies.contains("sbx") && implies.contains("ssh-agent"),
                "{expect} does not say what is reachable: {implies}"
            );
        }
        assert_ne!(
            Deployment::HostDriven.implies(),
            Deployment::InFleet.implies()
        );
    }
}
