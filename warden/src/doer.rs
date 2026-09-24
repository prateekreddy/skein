//! The doers, and the thing that has to say yes before any of them runs (§8.1, §8.3).
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
//!
//! **A doer answers with [`Did`], not with a `Result`,** because "it failed" and "it never ran" are
//! different facts about the world and the outcome store keys its whole contract on which one it
//! was. See [`crate::outcome::Did`] for what a refusal costs when the two are collapsed.

// Every user of it is a doer, and a doer is behind a feature — so a sink-and-observation warden
// (§8.3's "capabilities are compiled", taken to its limit) would carry this as an unused import.
#[cfg(any(
    feature = "create",
    feature = "destroy",
    feature = "publish",
    feature = "unpublish"
))]
use crate::outcome::{cross_then, Did, Reach};

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

/// The `will run` line, for any verb, whether or not this warden was built with its doer.
///
/// **One renderer, because something else now has to measure this text before a person sees it.**
/// `serve::vetted` refuses an approval that would be too long to read to the end, and a bound on
/// the length of a string is only honest if it is the length of *that* string. A second `format!`
/// beside these would be the two-guards-that-agree-today shape `serve::vetted`'s note about
/// `checked_id` already refuses — except worse, because the two would be a renderer and a ruler,
/// and the way they would come apart is that the ruler stops measuring the thing on the screen.
///
/// Always compiled, for [`argv_create`]'s reason: a warden that cannot describe an operation it
/// does not perform is a warden that cannot explain its own refusal.
///
/// `Err` is the argv refusing to resolve, and it is the same `Err` the doer below would return —
/// which is what makes it safe for a caller to treat as "there is no approval text here", since a
/// request whose argv does not resolve never reaches [`Approver::approve`] at all.
pub fn described(
    request: &Request,
    which: crate::capability::Capability,
) -> Result<String, String> {
    use crate::capability::Capability;
    Ok(match which {
        Capability::Create => format!(
            "`{}sbx {}`",
            described_env(request),
            argv_create(request)?.join(" ")
        ),
        Capability::Destroy => format!(
            "`{}sbx rm -f {}` — THIS DESTROYS THE FLEET",
            described_env(request),
            request.sandbox
        ),
        Capability::Publish => format!(
            "`{}sbx {}` — forwards a host port into the sandbox",
            described_env(request),
            argv_publish(request)?.join(" ")
        ),
        Capability::Unpublish => format!(
            "`{}sbx {}` — withdraws a host port mapping",
            described_env(request),
            argv_unpublish(request)?.join(" ")
        ),
    })
}

/// Make the fleet sandbox.
#[cfg(feature = "create")]
pub fn create(approver: &dyn Approver, request: &Request, reach: Reach<'_>) -> Did {
    // The text a person is shown is built HERE, from the resolved arguments this function will
    // itself execute — not from anything in the request that says how to describe it.
    let argv = match argv_create(request) {
        Ok(argv) => argv,
        Err(why) => return Did::Never(why),
    };
    let what = match described(request, crate::capability::Capability::Create) {
        Ok(what) => what,
        Err(why) => return Did::Never(why),
    };
    match approver.approve(request, &what) {
        // The marker before the command, never beside it (SKEIN-533). `cross_then` is where that
        // order lives, and `Did::Ran` cannot be spelled without having crossed.
        Ok(()) => cross_then(reach, || run(&argv, &request.env)),
        Err(why) => Did::Never(why),
    }
}

/// Destroy it.
#[cfg(feature = "destroy")]
pub fn destroy(approver: &dyn Approver, request: &Request, reach: Reach<'_>) -> Did {
    let what = match described(request, crate::capability::Capability::Destroy) {
        Ok(what) => what,
        Err(why) => return Did::Never(why),
    };
    match approver.approve(request, &what) {
        Ok(()) => cross_then(reach, || run(&argv_destroy(request), &request.env)),
        Err(why) => Did::Never(why),
    }
}

/// The environment, as it appears in front of the command a person is shown.
///
/// Rendered the way it would be typed, so the approval text and the hand-run line a caller is given
/// on failure are the same string in a different place.
///
/// **Every doer renders it, and that is not symmetry for its own sake.** `destroy` and `unpublish`
/// showed the argv alone while [`run`] passed the request's whole environment to the child, so
/// `PATH` — which is what decides *which* `sbx` a relative program name resolves to — was a thing
/// the request could set and the approval could not show. [`crate::serve`] now refuses the keys
/// those two have no use for, and this renders whatever survives that: a guard and a renderer that
/// each assume the other is doing the work is how the gap opened in the first place.
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

/// Open a host port mapping into the sandbox.
///
/// Publishing opens a host port into the network namespace every box shares, which is why it stays
/// a prompted act (§9.4, `capability::Capability::Publish`). The warden performs it, but only after
/// the person types the operation id — the approver is asked here exactly as it is for every other
/// doer, and there is no path to [`run`] that does not go through it.
#[cfg(feature = "publish")]
pub fn publish(approver: &dyn Approver, request: &Request, reach: Reach<'_>) -> Did {
    let argv = match argv_publish(request) {
        Ok(argv) => argv,
        Err(why) => return Did::Never(why),
    };
    let what = match described(request, crate::capability::Capability::Publish) {
        Ok(what) => what,
        Err(why) => return Did::Never(why),
    };
    match approver.approve(request, &what) {
        Ok(()) => cross_then(reach, || run(&argv, &request.env)),
        Err(why) => Did::Never(why),
    }
}

/// The argv for a publish, **validated rather than trusted** — [`argv_unpublish`]'s rules, mirrored.
///
/// **The verb is `ports`** and **the sandbox is this request's sandbox**, so the endpoint cannot be
/// aimed at another sandbox on the host. **The flag is `--publish`, and `--unpublish` is refused by
/// name**, so each port doer runs only its own act.
///
/// **And the mapping is exactly `HOST:SANDBOX/tcp`, two port numbers and nothing else.** sbx takes
/// an address in front of the host port as well, and that address is what decides which network
/// the opened port answers on — so a mapping the warden did not check could open the port beyond
/// this machine while the approval still read like the cockpit's ordinary line.
///
/// Ungated, for [`argv_unpublish`]'s reason: [`described`] renders every verb so that
/// `serve::vetted` can measure the approval in every build.
pub fn argv_publish(request: &Request) -> Result<Vec<String>, String> {
    let argv = request.args.clone();
    let refuse = |why: &str| {
        Err(format!(
            "the publish for {} {why}, so it is not a port forwarded into it: `sbx {}`",
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
    if argv.iter().any(|a| a == "--unpublish") {
        return refuse("asks to WITHDRAW a port, which is the other doer's act");
    }
    if argv.get(2).map(String::as_str) != Some("--publish") || argv.len() != 4 {
        return refuse("is not exactly `ports <sandbox> --publish <mapping>`");
    }
    if !plain_mapping(&argv[3]) {
        return refuse("maps something other than one host port to one sandbox port over tcp");
    }
    Ok(argv)
}

/// **Is the mapping this publish asks for already there?** The read a publish makes before it asks.
///
/// The owner's decision on SKEIN-1130 ("keep Publish for repair"): `sbx create` already publishes
/// the cockpit's port, so a publish is a repair, and the person is asked only when the mapping is
/// actually missing. `sbx ports <sandbox>` is a read, so it needs no approval, and it goes through
/// [`run`] rather than through `cross_then`, because nothing privileged is reached.
///
/// `Err` when the listing cannot be read or parsed. **The caller must then not publish blind**: a
/// listing it cannot read would turn "I could not look" into "it is missing", and that would put a
/// publish in front of the person for a mapping that may already be there.
#[cfg(feature = "publish")]
pub fn already_published(request: &Request) -> Result<bool, String> {
    let argv = argv_publish(request)?;
    let listing = run(
        &["ports".to_string(), request.sandbox.clone()],
        &request.env,
    )
    .map_err(|why| unreadable(request, &why))?;
    mapped_in(&listing, &argv[3]).map_err(|why| unreadable(request, &why))
}

#[cfg(feature = "publish")]
fn unreadable(request: &Request, why: &str) -> String {
    format!(
        "the ports {} publishes could not be read, so nothing was published blind: {why}",
        request.sandbox
    )
}

/// Does the `sbx ports` table hold `mapping` (`HOST:SANDBOX/tcp`)?
///
/// The table is `HOST IP / HOST PORT / SANDBOX PORT / PROTOCOL`, with one row per address family.
/// Every line must be the header or a row whose two port columns are numbers. A line that is
/// neither means the output is not the table this was written against, and that is an `Err`,
/// never "missing". An empty listing is a table with no rows.
///
/// Ungated and pure, so the parse is testable without a process.
pub fn mapped_in(table: &str, mapping: &str) -> Result<bool, String> {
    let (ports, protocol) = mapping
        .split_once('/')
        .ok_or_else(|| format!("`{mapping}` is not HOST:SANDBOX/PROTOCOL"))?;
    let (host, sandbox) = ports
        .split_once(':')
        .ok_or_else(|| format!("`{mapping}` is not HOST:SANDBOX/PROTOCOL"))?;
    let mut found = false;
    for line in table.lines().filter(|l| !l.trim().is_empty()) {
        if line.contains("HOST PORT") && line.contains("SANDBOX PORT") {
            continue;
        }
        let cols: Vec<&str> = line.split_whitespace().collect();
        let row_ports =
            cols.len() == 4 && cols[1].parse::<u16>().is_ok() && cols[2].parse::<u16>().is_ok();
        if !row_ports {
            return Err(format!(
                "`sbx ports` printed a line that is not a mapping: {line}"
            ));
        }
        found |= cols[1] == host && cols[2] == sandbox && cols[3].eq_ignore_ascii_case(protocol);
    }
    Ok(found)
}

/// `HOST:SANDBOX/tcp`, both of them port numbers from 1 to 65535, and nothing else.
fn plain_mapping(mapping: &str) -> bool {
    let port = |p: &str| {
        !p.is_empty()
            && p.len() <= 5
            && p.bytes().all(|b| b.is_ascii_digit())
            && p.parse::<u16>().is_ok_and(|n| n != 0)
    };
    match mapping.strip_suffix("/tcp").and_then(|m| m.split_once(':')) {
        Some((host, sandbox)) => port(host) && port(sandbox),
        None => false,
    }
}

/// Withdraw a host port mapping.
///
/// The doer whose whole purpose is to CLOSE something, and the mirror of [`publish`]. Skein could
/// not take a mapping back, so until this every mapping made by mistake — a probe that judged a live
/// port dead, a fleet whose agent never came up — was a line somebody had to be asked to run.
#[cfg(feature = "unpublish")]
pub fn unpublish(approver: &dyn Approver, request: &Request, reach: Reach<'_>) -> Did {
    let argv = match argv_unpublish(request) {
        Ok(argv) => argv,
        Err(why) => return Did::Never(why),
    };
    let what = match described(request, crate::capability::Capability::Unpublish) {
        Ok(what) => what,
        Err(why) => return Did::Never(why),
    };
    match approver.approve(request, &what) {
        Ok(()) => cross_then(reach, || run(&argv, &request.env)),
        Err(why) => Did::Never(why),
    }
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
/// is a general `sbx ports` executor, and what it opens or closes is decided by the caller rather
/// than by the warden. Opening a port is [`publish`]'s act, which a warden can be built without —
/// and a warden that could be talked into publishing through this endpoint would have a `publish`
/// capability its build never declared.
///
/// Ungated, where the doer above is not, for the reason [`argv_create`] gives and one more that is
/// new: [`described`] renders every verb so that `serve::vetted` can measure the approval before a
/// person is shown it, and a builder that vanished with its feature would have made that bound
/// exist in the default build and not in a reduced one.
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
        return refuse("asks to PUBLISH a port, which is the other doer's act");
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
#[cfg(any(
    feature = "create",
    feature = "destroy",
    feature = "publish",
    feature = "unpublish"
))]
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

    /// The reason a doer gives for never reaching its command — and an assertion that it did not.
    ///
    /// `Did::Ran(Err(..))` reads the same as a refusal in a message and is the opposite fact, so a
    /// test that took the string out of either would pass against a warden that ran `sbx` and got
    /// an error back. Unwrapping through the variant is what makes it a test of the distinction.
    #[cfg(all(feature = "create", feature = "destroy"))]
    fn never(did: Did) -> String {
        match did {
            Did::Never(why) => why,
            Did::Ran(_, done) => {
                panic!("the command was reached, and it should not have been: {done:?}")
            }
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
    /// Opening a host port is its own doer, removable on its own (`capability::Capability::Publish`),
    /// because it opens a way into the network namespace every box shares (§9.4). That separation is
    /// about what each doer will run, so it has to be the warden deciding and not the caller:
    /// without the checks below, `/v1/unpublish` is a general `sbx ports` executor and a warden
    /// built without `publish` could still be asked to open a port.
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
            why.contains("PUBLISH") && why.contains("the other doer's act"),
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

    /// **A publish runs one mapping into this sandbox, and nothing else it could be talked into.**
    ///
    /// The mirror of the withdrawal test above, plus the check that only a publish needs: the mapping
    /// is two port numbers over tcp. sbx also takes an address in front of the host port, and that
    /// address decides which network the opened port answers on — so `0.0.0.0:7878:7878/tcp` is the
    /// cockpit's line with the one change that matters most, and it is refused.
    ///
    /// **What makes this fail**: dropping any one check in `argv_publish` — the case that names it
    /// then comes back `Ok` and `unwrap_err` panics on it.
    #[test]
    fn a_publish_that_is_anything_but_one_port_into_this_sandbox_is_refused() {
        let and_args = |args: Vec<&str>| Request {
            args: args.iter().map(|a| a.to_string()).collect(),
            ..asked()
        };
        let good = vec!["ports", "skein-fleet", "--publish", "7878:7878/tcp"];
        assert_eq!(
            argv_publish(&and_args(good.clone())).unwrap(),
            good.iter().map(|a| a.to_string()).collect::<Vec<_>>(),
            "the one shape this doer exists to run must run"
        );
        for (args, expected) in [
            (
                vec!["ports", "skein-fleet", "--unpublish", "7878:7878/tcp"],
                "WITHDRAW",
            ),
            (
                vec!["ports", "someone-elses-fleet", "--publish", "7878:7878/tcp"],
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
                    "--publish",
                    "7878:7878/tcp",
                    "--publish",
                    "1:1/tcp",
                ],
                "is not exactly",
            ),
            (
                vec!["ports", "skein-fleet", "--publish", "0.0.0.0:7878:7878/tcp"],
                "one host port to one sandbox port",
            ),
            (
                vec!["ports", "skein-fleet", "--publish", "7878:7878/udp"],
                "one host port to one sandbox port",
            ),
            (
                vec!["ports", "skein-fleet", "--publish", "0:7878/tcp"],
                "one host port to one sandbox port",
            ),
            (
                vec!["ports", "skein-fleet", "--publish", "7878-7890:7878/tcp"],
                "one host port to one sandbox port",
            ),
        ] {
            let why = argv_publish(&and_args(args.clone())).unwrap_err();
            assert!(why.contains(expected), "{args:?} was refused as: {why}");
        }
        // And what a person is shown is that argv, whole, with the approved suffix.
        assert_eq!(
            described(
                &Request {
                    env: Vec::new(),
                    ..and_args(good)
                },
                crate::capability::Capability::Publish
            )
            .unwrap(),
            "`sbx ports skein-fleet --publish 7878:7878/tcp` — forwards a host port into the sandbox"
        );
    }

    /// **The listing a publish reads before it asks: found, missing, or not a listing at all.**
    ///
    /// Three answers that must stay three. "Not a listing" read as "missing" would put a publish in
    /// front of the person for a mapping that may already be there, so that case is an `Err`.
    ///
    /// **What makes this fail**: dropping the row check in `mapped_in` (the garbage line comes
    /// back `Ok(false)`), or matching on the host port alone (the `9999:22` row, or a `7878:1`
    /// row, would count).
    #[test]
    fn the_listing_says_found_or_missing_and_anything_else_is_not_an_answer() {
        const HEADER: &str = "HOST IP\tHOST PORT\tSANDBOX PORT\tPROTOCOL\n";
        let asked = "7878:7878/tcp";
        let with = |rows: &str| format!("{HEADER}{rows}");
        assert_eq!(mapped_in(&with("::1\t7878\t7878\ttcp\n"), asked), Ok(true));
        assert_eq!(
            mapped_in(&with("127.0.0.1\t7878\t7878\ttcp\n"), asked),
            Ok(true)
        );
        for missing in [
            with(""),
            String::new(),
            with("127.0.0.1\t9999\t22\ttcp\n"),
            with("127.0.0.1\t7878\t1\ttcp\n"),
            with("127.0.0.1\t7878\t7878\tudp\n"),
        ] {
            assert_eq!(mapped_in(&missing, asked), Ok(false), "{missing:?}");
        }
        for garbage in [
            "error: sandbox not found\n".to_string(),
            with("127.0.0.1\tseven\t7878\ttcp\n"),
        ] {
            assert!(
                mapped_in(&garbage, asked).is_err(),
                "{garbage:?} was read as a listing"
            );
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
        let dir = crate::Scratch::new("skein-warden-doer");
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

        // A marker that always succeeds, so that a doer which wrongly skipped its approver would
        // still reach the fake `sbx` and be caught by the assertion below — rather than be stopped
        // by an unwritten marker, which would pass this test for the wrong reason.
        let anywhere = &crate::outcome::crossed_for_a_test;
        let refused = never(create(&Unattended, &asked(), anywhere));
        let refused_destroy = never(destroy(&Unattended, &asked(), anywhere));
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
        let anywhere = &crate::outcome::crossed_for_a_test;
        #[cfg(feature = "create")]
        let _ = create(&seen, &sneaky, anywhere);
        #[cfg(feature = "destroy")]
        let _ = destroy(&seen, &sneaky, anywhere);

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

    /// The environment on a request decides which binary the host uid executes.
    ///
    /// **This is the hazard the wire's allow-list exists for, executed rather than reasoned.**
    /// `serve::env_a_doer_may_carry` and `described_env` are both justified by a sentence about
    /// std's behaviour — that [`run`] spells the program as the relative name `"sbx"`, and that the
    /// `PATH` set on the `Command` is what a relative name is resolved through, so an approval
    /// could read `sbx rm -f skein-fleet` while a different program ran. Everything downstream of
    /// that sentence is a guard resting on a premise nothing in this crate checks; std's lookup is
    /// free to differ from the reading, and the guard would then be protecting against nothing
    /// while looking exactly as it does now.
    ///
    /// So it is run: a real `sbx` first on the parent's `PATH`, a decoy in a directory the request
    /// names, and the assertion is **which of the two answered**. The decoy is what makes this a
    /// test of resolution rather than of spawning — an assertion that "something ran" would pass
    /// whichever binary it was.
    ///
    /// Each script says which it is on stdout, which [`run`] hands back, rather than touching a
    /// marker file: the child's `PATH` is the decoy directory and nothing else, so `touch` is not
    /// on it. That version of this test passed its "the honest one did not run" assertion for the
    /// wrong reason — neither marker was ever written — and only the stderr said so.
    ///
    /// No approver is involved on purpose. [`run`] is the half below the approval, and the question
    /// here is only what `sbx` resolves to.
    #[test]
    #[cfg(any(feature = "create", feature = "destroy", feature = "unpublish"))]
    fn an_environment_on_the_request_decides_which_binary_runs() {
        // $PATH is process-wide and this test puts a fake `sbx` on it — the lock SKEIN-307 added.
        let _env = crate::env_lock();
        use std::os::unix::fs::PermissionsExt;
        let dir = crate::Scratch::fresh("skein-warden-resolve");
        let (honest, elsewhere) = (dir.join("honest"), dir.join("elsewhere"));
        for (at, says) in [(&honest, "honest"), (&elsewhere, "decoy")] {
            std::fs::create_dir_all(at).unwrap();
            let sbx = at.join("sbx");
            std::fs::write(&sbx, format!("#!/bin/sh\necho {says}\n")).unwrap();
            std::fs::set_permissions(&sbx, std::fs::Permissions::from_mode(0o755)).unwrap();
        }

        let real = std::env::var("PATH").unwrap_or_default();
        std::env::set_var("PATH", format!("{}:{real}", honest.display()));
        // What the warden would run for `sbx rm -f skein-fleet`, with the environment a request
        // carries — which is the whole of the difference between the two directories.
        let did = run(
            &["rm".to_string(), "-f".into(), "skein-fleet".into()],
            &[("PATH".to_string(), elsewhere.display().to_string())],
        );
        // And the same command with nothing on it, so the honest `sbx` is reachable and the
        // assertion above is about resolution rather than about a directory that does not work.
        let unchanged = run(&["rm".to_string(), "-f".into(), "skein-fleet".into()], &[]);
        std::env::set_var("PATH", real);

        assert_eq!(
            unchanged.as_deref(),
            Ok("honest"),
            "the `sbx` first on the warden's own PATH is what an untouched environment reaches"
        );
        assert_eq!(
            did.as_deref(),
            Ok("decoy"),
            "the `PATH` on the request did not choose the program, so the allow-list at the wire \
             is guarding a hazard that is not there — check this before deleting the guard"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Every doer shows the environment it will run under, not only `create`.
    ///
    /// [`run`] passes `request.env` to the child whichever doer called it, and `Command::new("sbx")`
    /// is a relative program name — so the environment decides which binary the host uid executes.
    /// `create` rendered it and `destroy` and `unpublish` did not, which made the most dangerous
    /// part of the most dangerous operation the one part not on the screen.
    ///
    /// `serve::vetted` now refuses the keys those two have no use for, and this is the other half:
    /// a guard and a renderer that each assume the other is doing the work is exactly how the gap
    /// opened. What survives the guard is shown.
    #[test]
    #[cfg(all(feature = "destroy", feature = "unpublish"))]
    fn a_doer_that_carries_an_environment_shows_it() {
        struct Watcher(std::sync::Mutex<Vec<String>>);
        impl Approver for Watcher {
            fn approve(&self, _: &Request, what: &str) -> Result<(), String> {
                self.0.lock().unwrap().push(what.to_string());
                Err("not today".into())
            }
        }
        let seen = Watcher(std::sync::Mutex::new(Vec::new()));
        let carrying = |args: Vec<&str>| Request {
            operation: "op-3".into(),
            sandbox: "skein-fleet".into(),
            args: args.into_iter().map(String::from).collect(),
            env: vec![("SOMETHING".into(), "chosen-by-the-caller".into())],
        };
        let anywhere = &crate::outcome::crossed_for_a_test;
        let _ = destroy(&seen, &carrying(vec![]), anywhere);
        let _ = unpublish(
            &seen,
            &carrying(vec!["ports", "skein-fleet", "--unpublish", "8317:8317/tcp"]),
            anywhere,
        );

        let shown = seen.0.lock().unwrap().clone();
        assert_eq!(
            shown.len(),
            2,
            "a doer never reached the approver: {shown:?}"
        );
        for text in &shown {
            assert!(
                text.contains("SOMETHING=chosen-by-the-caller sbx "),
                "the environment is part of what will run, so it is part of what is shown: {text}"
            );
        }
    }
}
