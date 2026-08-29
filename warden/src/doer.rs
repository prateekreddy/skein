//! The two doers, and the thing that has to say yes before either runs (§8.1, §8.3).
//!
//! **Each behind its own Cargo feature**, so a warden built without one does not contain it. §8.3's
//! argument only works if absence is absence: a runtime check falls to a bug in the check, and
//! absent code falls to nothing. Both ship by default — see [`crate::capability`] for why removing
//! `destroy` removes `resize` with it.
//!
//! **Nothing here runs without an [`Approver`] saying so, and the default one always refuses.**
//! That is not a placeholder to be tidied later, it is the order the work has to happen in: §8.1
//! says the warden renders and confirms its own approvals on the host, outside the fleet, and until
//! that surface exists there is no one to ask. A warden that executed privileged host commands in
//! the meantime would be exactly the thing this component exists to prevent, shipped early and
//! justified by nothing calling it yet.
//!
//! So the endpoint exists, is reachable, advertises itself honestly, and refuses — and the refusal
//! names the reason rather than looking like a bug.
//!
//! **Recipes and checks are not here and never will be.** They live in skein, always compiled, never
//! privileged, because they are needed precisely when the doer is absent (§8.3).

/// What the warden was asked to do, after its own parse.
///
/// **Its own parse** is the load-bearing part (§8.4): the approval text is built from this, never
/// from display text the requester supplied. An operation id is correlation, not content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    /// The operation id. Correlation only — it is what makes a retry safe, and it is echoed into
    /// the approval so that what a person sees is what will run.
    pub operation: String,
    /// The fleet sandbox this concerns.
    pub sandbox: String,
    /// The arguments the warden will pass, already resolved by it. Empty for `destroy`.
    pub args: Vec<String>,
    /// Environment for the command, as `(name, value)`.
    ///
    /// It is part of what will run and is rendered as such: `DOCKER_SANDBOXES_ROOT_SIZE=200g` is the
    /// difference between a 20 GB fleet and a 200 GB one, and an approval that showed the argv but
    /// not the environment would be showing most of the command.
    pub env: Vec<(String, String)>,
}

/// Whoever decides whether a privileged thing may happen.
///
/// A trait rather than a function so that the answer can come from a person at a terminal, from a
/// window in a host-side UI, or — in a test — from nothing at all. What it must never be is a field
/// on the request: **approval is a fact the approving side writes, never one the requester
/// supplies** (§8.1), and the type is the shape of that rule.
pub trait Approver: Send + Sync {
    /// `Ok(())` if a human at the host said yes to *this* operation.
    fn approve(&self, request: &Request, what: &str) -> Result<(), String>;
}

/// The approver a warden has before its approval surface is built.
///
/// Refuses everything, and says why in words that name the missing piece rather than reading as a
/// failure. There is deliberately no way to configure it into saying yes.
pub struct Unattended;

impl Approver for Unattended {
    fn approve(&self, request: &Request, what: &str) -> Result<(), String> {
        Err(format!(
            "{what} for {} was not run: this warden has no approval surface, and a privileged host \
             command needs a human at the host to confirm it (architecture §8.1). Operation {}.",
            request.sandbox, request.operation
        ))
    }
}

/// Make the fleet sandbox.
#[cfg(feature = "create")]
pub fn create(approver: &dyn Approver, request: &Request) -> Result<String, String> {
    // The text a person is shown is built HERE, from the resolved arguments this function will
    // itself execute — not from anything in the request that says how to describe it.
    let argv = argv_create(request)?;
    let what = format!("`{}sbx {}`", described_env(request), argv.join(" "));
    approver.approve(request, &what)?;
    run(&argv, &request.env)
}

/// Destroy it.
#[cfg(feature = "destroy")]
pub fn destroy(approver: &dyn Approver, request: &Request) -> Result<String, String> {
    let what = format!("`sbx rm -f {}` — THIS DESTROYS THE FLEET", request.sandbox);
    approver.approve(request, &what)?;
    run(&argv_destroy(request), &request.env)
}

/// The environment, as it appears in front of the command a person is shown.
///
/// Rendered the way it would be typed, so the approval text and the hand-run line a caller is given
/// on failure are the same string in a different place.
pub fn described_env(request: &Request) -> String {
    request
        .env
        .iter()
        .map(|(k, v)| format!("{k}={v} "))
        .collect()
}

/// The argv a create will run, as its own function so it is a contract rather than a detail.
///
/// Public and always compiled, even where the doer is not: the approval text and the audit entry
/// both need to say what would run, and a warden that cannot describe an operation it does not
/// perform is a warden that cannot explain its own refusal.
///
/// **The requester sends the whole argv and the warden adds nothing to it.** This used to prepend
/// its own `["create", <sandbox>]`, while skein's `fleet::create_argv` already begins
/// `["create", "--name", <sandbox>]` — so what a real host would have run was
/// `sbx create skein-fleet create --name skein-fleet …`, and sbx rejects it: the usage is
/// `sbx create [flags] AGENT PATH [PATH...]`, so `skein-fleet` in second place is read as the
/// AGENT. Creating the fleet is the one thing skein cannot do without a warden, which made this the
/// first command of every install on a fresh host, failing.
///
/// The prepend was there for a real reason — §8.4's rule that the name a person is shown is the
/// name that gets executed — and removing it does not give that up. It is **checked** instead:
/// exactly one `--name`, whose value is [`Request::sandbox`], the field the approval text is built
/// from. Two `--name`s are refused rather than merged, because sbx takes the last one and the
/// person would have approved the first.
pub fn argv_create(request: &Request) -> Result<Vec<String>, String> {
    let argv = request.args.clone();
    if argv.first().map(String::as_str) != Some("create") {
        return Err(format!(
            "the create for {} does not begin with the `create` verb, so it is not a create: `sbx \
             {}`",
            request.sandbox,
            argv.join(" ")
        ));
    }
    let named: Vec<&str> = argv
        .windows(2)
        .filter(|w| w[0] == "--name")
        .map(|w| w[1].as_str())
        .collect();
    match named.as_slice() {
        [only] if *only == request.sandbox => Ok(argv),
        [] => Err(format!(
            "the create for {} passes no `--name`, so sbx would name the sandbox after the agent \
             and the working directory instead — and nothing afterwards would find {}",
            request.sandbox, request.sandbox
        )),
        _ => Err(format!(
            "the create approved for {} would run as {} — sbx takes the last `--name`, so what ran \
             would not be what was shown",
            request.sandbox,
            named.join(", then ")
        )),
    }
}

/// And a destroy. `-f` because the warden is not interactive; the confirmation happened at the
/// approval surface, which is the only place a person is.
pub fn argv_destroy(request: &Request) -> Vec<String> {
    vec!["rm".to_string(), "-f".into(), request.sandbox.clone()]
}

/// Withdraw a host port mapping.
///
/// The one doer whose whole purpose is to CLOSE something, which is why it exists where a `publish`
/// does not (`capability::Capability::Unpublish`). Skein publishes host ports and could not take
/// them back, so until this every mapping made by mistake — a probe that judged a live port dead, a
/// fleet whose agent never came up — was a line somebody had to be asked to run.
#[cfg(feature = "unpublish")]
pub fn unpublish(approver: &dyn Approver, request: &Request) -> Result<String, String> {
    let argv = argv_unpublish(request)?;
    let what = format!("`sbx {}` — withdraws a host port mapping", argv.join(" "));
    approver.approve(request, &what)?;
    run(&argv, &request.env)
}

/// The argv for a withdrawal, **validated rather than trusted**.
///
/// The same rule `argv_create` applies for the same reason: the warden runs what it resolved, not
/// what it was handed. Three things are checked and each is load-bearing.
///
/// **The verb is `ports`** and **the sandbox is this request's sandbox**, so the endpoint cannot be
/// aimed at another sandbox on the host by a caller that got past the token.
///
/// **The flag is `--unpublish`, and `--publish` is refused by name.** Without that check this doer
/// is a general `sbx ports` executor and the capability's whole safety argument — that withdrawing
/// only ever closes an opening — is decided by the caller rather than by the warden. A warden that
/// can be talked into publishing is a warden with a `publish` capability it never declared.
#[cfg(feature = "unpublish")]
pub fn argv_unpublish(request: &Request) -> Result<Vec<String>, String> {
    let argv = request.args.clone();
    let refuse = |why: &str| {
        Err(format!(
            "the unpublish for {} {why}, so it is not a withdrawal: `sbx {}`",
            request.sandbox,
            argv.join(" ")
        ))
    };
    match argv.first().map(String::as_str) {
        Some("ports") => {}
        _ => return refuse("does not begin with the `ports` verb"),
    }
    if argv.get(1).map(String::as_str) != Some(request.sandbox.as_str()) {
        return refuse("names a sandbox other than the one it was sent for");
    }
    if argv.iter().any(|a| a == "--publish") {
        return refuse("asks to PUBLISH a port, which this warden has no capability for");
    }
    if argv.get(2).map(String::as_str) != Some("--unpublish") || argv.len() != 4 {
        return refuse("is not exactly `ports <sandbox> --unpublish <mapping>`");
    }
    Ok(argv)
}

/// Run `sbx`.
///
/// The program is spelled here as a literal rather than passed in, and that is not a style
/// preference: `tools/source-check.py` finds a reach by looking for `Command::new("sbx")`, and a
/// `Command::new(program)` with the name arriving as an argument is a crossing the law cannot see.
/// The first version of this function took the program as a parameter and the checker went quiet on
/// the most privileged reach in the system.
#[cfg(any(feature = "create", feature = "destroy", feature = "unpublish"))]
fn run(argv: &[String], env: &[(String, String)]) -> Result<String, String> {
    let out = std::process::Command::new("sbx")
        .args(argv)
        .envs(env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
        .output()
        .map_err(|e| format!("could not run `sbx`: {e}"))?;
    match out.status.success() {
        true => Ok(String::from_utf8_lossy(&out.stdout).trim().to_string()),
        false => Err(format!(
            "`sbx {}` exited {}: {}",
            argv.join(" "),
            out.status
                .code()
                .map(|c| c.to_string())
                .unwrap_or_else(|| "on a signal".into()),
            String::from_utf8_lossy(&out.stderr).trim()
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn asked() -> Request {
        Request {
            operation: "op-1".into(),
            sandbox: "skein-fleet".into(),
            args: create_line("skein-fleet"),
            env: vec![("DOCKER_SANDBOXES_ROOT_SIZE".into(), "200g".into())],
        }
    }

    /// A create argv shaped like the one skein really sends — `sbx create [flags] AGENT PATH
    /// [PATH...]`, which is sbx's own usage line, so verb, `--name`, the flags, the agent, then the
    /// workspaces.
    ///
    /// **A fixture that is only the tail of an argv tests the fixture.** This was
    /// `["--memory", "26g"]`, and that is the whole reason the warden could prepend a second verb
    /// and a second name to every create without one test in this file noticing (SKEIN-456).
    fn create_line(name: &str) -> Vec<String> {
        [
            "create",
            "--name",
            name,
            "-m",
            "26g",
            "--cpus",
            "7",
            "shell",
            "/h/.skein",
        ]
        .iter()
        .map(|a| a.to_string())
        .collect()
    }

    /// The warden runs the argv it was sent, whole, and adds nothing to the front of it.
    ///
    /// Asserted with `assert_eq` against the entire vector on purpose. The bug this replaces —
    /// `["create", <sandbox>]` prepended to an argv already beginning `["create", "--name",
    /// <sandbox>]` — survived a `contains` in `tests/warden_roundtrip.rs` and a starts-with count
    /// beside it, because a doubled argv still contains and still starts with the right words.
    #[test]
    fn the_warden_runs_the_whole_create_argv_it_was_sent_and_prepends_nothing() {
        assert_eq!(argv_create(&asked()).unwrap(), create_line("skein-fleet"));
    }

    /// **A withdrawal endpoint that can be talked into publishing is a publish capability.**
    ///
    /// The capability's entire safety argument is that withdrawing a mapping only ever CLOSES an
    /// opening — which is why `Unpublish` exists where `Publish` deliberately does not (§9.4 makes
    /// opening a host port a prompted act). That argument is about what this doer will run, so it
    /// has to be the warden deciding and not the caller: without the checks below, `/v1/unpublish`
    /// is a general `sbx ports` executor and a warden that got past the token could be asked to
    /// open a host port into the network namespace every box shares.
    ///
    /// The sandbox check is §8.4's rule, the same one `argv_create` applies: what is approved names
    /// the sandbox it was sent for, or it is refused.
    #[cfg(feature = "unpublish")]
    #[test]
    fn a_withdrawal_that_is_anything_but_a_withdrawal_of_this_sandbox_is_refused() {
        let and_args = |args: Vec<&str>| Request {
            args: args.iter().map(|a| a.to_string()).collect(),
            ..asked()
        };
        let good = vec!["ports", "skein-fleet", "--unpublish", "7878:7878/tcp"];
        assert_eq!(
            argv_unpublish(&and_args(good.clone())).unwrap(),
            good.iter().map(|a| a.to_string()).collect::<Vec<_>>(),
            "the one shape this doer exists to run must run"
        );

        // The one that matters most: the flag flipped, everything else identical.
        let opening = and_args(vec!["ports", "skein-fleet", "--publish", "7878:7878/tcp"]);
        let why = argv_unpublish(&opening).unwrap_err();
        assert!(
            why.contains("PUBLISH") && why.contains("no capability"),
            "a publish smuggled through the withdrawal endpoint was not named as one: {why}"
        );

        for (args, expected) in [
            (
                vec![
                    "ports",
                    "someone-elses-fleet",
                    "--unpublish",
                    "7878:7878/tcp",
                ],
                "names a sandbox other than",
            ),
            (
                vec!["rm", "-f", "skein-fleet"],
                "does not begin with the `ports` verb",
            ),
            (
                vec![
                    "ports",
                    "skein-fleet",
                    "--unpublish",
                    "7878:7878/tcp",
                    "--publish",
                    "1:1/tcp",
                ],
                "PUBLISH",
            ),
            (vec!["ports", "skein-fleet"], "is not exactly"),
        ] {
            let why = argv_unpublish(&and_args(args.clone())).unwrap_err();
            assert!(why.contains(expected), "{args:?} was refused as: {why}");
        }
    }

    /// And it refuses a create that would make a sandbox other than the one being approved.
    ///
    /// This is §8.4's rule surviving the change above. The name used to be safe because the warden
    /// wrote it; now the warden checks it, and the three ways it can be wrong are all refusals —
    /// no name, a different name, or two names, which sbx resolves to the last and a person would
    /// have approved the first.
    #[test]
    fn a_create_naming_a_sandbox_other_than_the_approved_one_is_refused() {
        let and_args = |args: Vec<&str>| Request {
            args: args.iter().map(|a| a.to_string()).collect(),
            ..asked()
        };

        let no_name = and_args(vec!["create", "-m", "26g", "shell", "/h/.skein"]);
        let why = argv_create(&no_name).unwrap_err();
        assert!(why.contains("no `--name`"), "{why}");

        let other = and_args(vec!["create", "--name", "not-the-fleet", "shell", "/h"]);
        let why = argv_create(&other).unwrap_err();
        assert!(why.contains("not-the-fleet"), "{why}");

        let twice = and_args(vec![
            "create",
            "--name",
            "skein-fleet",
            "--name",
            "somewhere-else",
            "shell",
            "/h",
        ]);
        let why = argv_create(&twice).unwrap_err();
        assert!(
            why.contains("skein-fleet, then somewhere-else"),
            "a second --name has to be refused rather than merged: {why}"
        );

        let not_a_create = and_args(vec!["rm", "-f", "skein-fleet"]);
        let why = argv_create(&not_a_create).unwrap_err();
        assert!(why.contains("is not a create"), "{why}");
    }

    /// Until the approval surface exists, a doer refuses — and the refusal names what is missing.
    ///
    /// The test is not that it errors. It is that **the privileged command was never reached**: an
    /// `sbx` that ran and then failed would look the same in the return value and would not be the
    /// same thing at all. So `sbx` is replaced on `PATH` by something that records being run.
    #[test]
    #[cfg(all(feature = "create", feature = "destroy"))]
    fn a_doer_without_an_approver_refuses_before_it_reaches_the_command() {
        // $PATH decides what EVERY spawn in this process resolves to, and six sibling tests hold
        // this lock. Without it, this test points them at a directory containing a fake `sbx` for
        // as long as it runs — the worst blast radius of the three sites SKEIN-307 found.
        let _env = crate::env_lock();
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("skein-warden-doer-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let marker = dir.join("sbx-was-run");
        let fake = dir.join("sbx");
        std::fs::write(
            &fake,
            format!("#!/bin/sh\ntouch {}\nexit 0\n", marker.display()),
        )
        .unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        let real = std::env::var("PATH").unwrap_or_default();
        std::env::set_var("PATH", format!("{}:{real}", dir.display()));

        let refused = create(&Unattended, &asked()).unwrap_err();
        let refused_destroy = destroy(&Unattended, &asked()).unwrap_err();
        std::env::set_var("PATH", real);

        assert!(
            !marker.exists(),
            "a privileged host command ran with nobody having approved it"
        );
        for why in [&refused, &refused_destroy] {
            assert!(
                why.contains("no approval surface") && why.contains("§8.1"),
                "the refusal must name what is missing, not read as a fault: {why}"
            );
            assert!(
                why.contains("op-1"),
                "a refusal has to say which operation it is about: {why}"
            );
        }
    }

    /// The approval text is built from the arguments the warden will itself run.
    ///
    /// §8.4, stated the right way round: an operation id is correlation, not content, and the warden
    /// renders **its own parse**. The test is that a request carrying misleading text cannot change
    /// what a person is shown — because there is no field it could put that text in.
    ///
    /// Needs a doer to put something to the approver, so it is a test about the builds that have
    /// one. In the sink-and-observation build there is no approval surface, which is the property
    /// §8.3 wants and not a case this assertion can speak about.
    #[cfg(any(feature = "create", feature = "destroy"))]
    #[test]
    fn what_a_person_is_shown_comes_from_what_will_run() {
        struct Watcher(std::sync::Mutex<Vec<String>>);
        impl Approver for Watcher {
            fn approve(&self, _: &Request, what: &str) -> Result<(), String> {
                self.0.lock().unwrap().push(what.to_string());
                Err("not today".into())
            }
        }
        let seen = Watcher(std::sync::Mutex::new(Vec::new()));
        let sneaky = Request {
            operation: "op-2".into(),
            env: Vec::new(),
            // The only place a requester's string reaches the text is the one the warden also
            // executes, so the two cannot disagree.
            sandbox: "skein-fleet".into(),
            args: create_line("skein-fleet"),
        };
        #[cfg(feature = "create")]
        let _ = create(&seen, &sneaky);
        #[cfg(feature = "destroy")]
        let _ = destroy(&seen, &sneaky);

        let shown = seen.0.lock().unwrap().clone();
        assert!(!shown.is_empty(), "nothing was put to the approver");
        for text in &shown {
            assert!(text.contains("skein-fleet"), "{text}");
        }
        // And the text is the argv, not a description of it — the whole line, so a warden that
        // added a word to what it ran could not show the line without the word.
        #[cfg(feature = "create")]
        assert!(
            shown
                .iter()
                .any(|t| *t == format!("`sbx {}`", create_line("skein-fleet").join(" "))),
            "the create put to the approver was not the argv it will run: {shown:?}"
        );
        assert_eq!(argv_create(&sneaky).unwrap(), create_line("skein-fleet"));
        assert_eq!(argv_destroy(&sneaky), vec!["rm", "-f", "skein-fleet"]);
    }
}
