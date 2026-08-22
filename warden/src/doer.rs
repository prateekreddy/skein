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
    let what = format!(
        "`{}sbx create {} {}`",
        described_env(request),
        request.sandbox,
        request.args.join(" ")
    );
    approver.approve(request, &what)?;
    run(&argv_create(request), &request.env)
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
pub fn argv_create(request: &Request) -> Vec<String> {
    let mut argv = vec!["create".to_string(), request.sandbox.clone()];
    argv.extend(request.args.iter().cloned());
    argv
}

/// And a destroy. `-f` because the warden is not interactive; the confirmation happened at the
/// approval surface, which is the only place a person is.
pub fn argv_destroy(request: &Request) -> Vec<String> {
    vec!["rm".to_string(), "-f".into(), request.sandbox.clone()]
}

/// Run `sbx`.
///
/// The program is spelled here as a literal rather than passed in, and that is not a style
/// preference: `tools/source-check.py` finds a reach by looking for `Command::new("sbx")`, and a
/// `Command::new(program)` with the name arriving as an argument is a crossing the law cannot see.
/// The first version of this function took the program as a parameter and the checker went quiet on
/// the most privileged reach in the system.
#[cfg(any(feature = "create", feature = "destroy"))]
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
            args: vec!["--memory".into(), "26g".into()],
            env: vec![("DOCKER_SANDBOXES_ROOT_SIZE".into(), "200g".into())],
        }
    }

    /// Until the approval surface exists, a doer refuses — and the refusal names what is missing.
    ///
    /// The test is not that it errors. It is that **the privileged command was never reached**: an
    /// `sbx` that ran and then failed would look the same in the return value and would not be the
    /// same thing at all. So `sbx` is replaced on `PATH` by something that records being run.
    #[test]
    #[cfg(all(feature = "create", feature = "destroy"))]
    fn a_doer_without_an_approver_refuses_before_it_reaches_the_command() {
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
            args: vec!["--memory".into(), "26g".into()],
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
        // And the argv is the same string the text was made from.
        assert_eq!(
            argv_create(&sneaky),
            vec!["create", "skein-fleet", "--memory", "26g"]
        );
        assert_eq!(argv_destroy(&sneaky), vec!["rm", "-f", "skein-fleet"]);
    }
}
