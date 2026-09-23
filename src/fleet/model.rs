//! The sandboxed model call: the script that runs it in a box, and its scratch directory.

use super::*;

/// **The script a model call is**, wherever it runs.
///
/// Its own function for the reason [`crate::place::Place::exec_argv`] is: this is a wire format, and
/// a wire format that can only be seen by running a sandbox is one nothing can pin. It became worth
/// factoring the day there were two destinations — the fleet sandbox and one pull request's own
/// review box ([`model_call_in_box`]) — because two copies of a shell script that must agree about
/// a heredoc, a credential and an unset list is two copies that will not. The sandbox destination
/// is gone (SKEIN-576): skein runs inside that sandbox, so a call it spawns is already there, and
/// the one crossing left is into a box. The factoring stays for the reason it was worth making
/// before there were two: this is the wire format, and pinning it needs it to be nameable.
///
/// **The prompt is not in this script at all** (SKEIN-799). It used to travel as a quoted heredoc
/// — correct about quoting, and wrong about where the bytes ended up: [`crate::place::Place::exec_argv`]
/// ends `argv.push(self.wrap(script))`, so the whole script, heredoc included, became ONE argv
/// element of the crossing. That cost the two things SKEIN-684 had already taken off the local
/// arm of the same call:
///
/// * **a ceiling.** Linux caps a single argv element at `MAX_ARG_STRLEN`, 32 pages — 131,072 bytes
///   on ordinary 4 KiB-page hardware. 3 of 27 real prompts measured for SKEIN-706 are over it, so
///   the largest readings were exactly the ones whose crossing could not be spawned; and because
///   [`crate::ai::claude_in_turn`] falls through on any `Err`, they lost their box in silence.
/// * **the payload in `ps`.** `/proc/<pid>/cmdline` is world readable for as long as the call runs,
///   and what was in it is the diff of a pull request, private repositories included. The
///   credential one screen up has always obeyed that rule ([`github_export`]); the diff it is
///   reading did not.
///
/// So the prompt rides the crossing's **stdin** instead — `place.attempt(script, prompt, …)` — and
/// arrives where the heredoc used to put it, which is `claude -p`'s own stdin. Nothing about the
/// bytes changes except the trailing newline the heredoc added, which is how `ai::tried` has fed
/// the same prompt on the local arm since SKEIN-684; the two destinations now agree byte for byte.
///
/// **The delimiter grew and no longer has to.** A prompt containing the delimiter would have
/// ended the heredoc early and handed `claude` half a question, so it was grown until
/// the prompt did not contain it. A pipe has no delimiter to collide with, so that hazard is gone
/// rather than handled, and the test that pinned the growing went with it.
///
/// **Nothing before the call may read stdin**, which is the one new rule this shape carries: the
/// prompt is sitting in the pipe while the three preparations below run, and a line that read it
/// would eat the question. None of them does — two are `export`s, one is a `[ -s ]` test, and
/// `github_export`'s is a `$(cat FILE)` with an argument.
/// `the_prompt_is_still_in_the_pipe_when_the_call_reads_it` runs the real script against a fake
/// binary to prove it rather than asserting it from the source.
///
/// **There is no `cd`, and its absence is load-bearing.** It used to be the caller's, and the only
/// difference between the two destinations: a call into the sandbox had to walk to the
/// conversation's directory itself. A call into a box is already standing in the box's own tree,
/// because `place::Place`'s wrapper cds there before this script runs at all — and that tree is
/// both the checkout of the commit under review and the directory Claude Code files the
/// conversation under. Walking anywhere else would read the wrong tree AND lose the session, and
/// the reading would come back looking perfectly ordinary.
fn model_call_script(bin: &str, model: &str, turn: &[&str], gh: &str) -> String {
    // The scratch directory travels with the call. See [`MODEL_SCRATCH`] — a sandbox's /tmp is
    // shared by everything skein runs in it, and the CLI refuses to start when the path it derives
    // from /tmp belongs to somebody else. `$HOME` is expanded WHERE THIS RUNS, by the shell that
    // runs it, because it is that HOME which holds the credential and not the host's. From
    // [`model_scratch_export`], which the login terminal and every box session now share: the rule
    // reached the calls skein MAKES before the ones it HOSTS (SKEIN-289).
    format!(
        "printf '%s\\n' {REACHED} >&2\n\
         {scratch}\n\
         {gh}\
         if [ -s \"$HOME/.claude/.credentials.json\" ]; then unset {overrides}; fi\n\
         {bin} -p --model {model}{turn}\n",
        scratch = model_scratch_export(),
        bin = sh_quote(bin),
        model = sh_quote(model),
        // Quoted like every other value that crosses into the sandbox's shell: these are skein's
        // own flags, but the id in them is a value, and a value that is not quoted is a value that
        // one day contains a space.
        turn = turn
            .iter()
            .map(|a| format!(" {}", sh_quote(a)))
            .collect::<String>(),
        overrides = MODEL_AUTH_OVERRIDES.join(" "),
    )
}

/// Run a model call **inside one pull request's review box** — `docs/pr-review.md` §11.
///
/// The same script skein spawns for a call of its own ([`model_call_script`]), and a different
/// address — and the address is the whole point. `place_of` reaches the box through its **placement record**, which is the rule
/// `sandbox::resume_box` had to learn: `sbx exec <box>` names a *sandbox*, and for a fleet box
/// there is none — or worse, an unrelated one wearing the same name.
///
/// Three things the box supplies that the sandbox call has to arrange for itself:
///
/// * **the working directory.** `Place`'s wrapper exports `HOME` and `SKEIN_BOX` and cds to the
///   box's recorded tree, so there is no `cd` in the script. That tree is the checkout of the
///   commit under review (`stand_at_head_script`), which is the substitution §11 is about: the
///   model stands in the change rather than being handed a diff of it.
/// * **the conversation.** Claude Code keys sessions on the working directory, and a box's
///   `~/.claude/projects` is bind-mounted from the host state directory — so the same box in the
///   same tree resumes the same conversation across a stop, a restart and a fleet rebuild.
/// * **the isolation.** A cgroup, a private `$HOME`, a private `/tmp`, and a tmpfs over the fleet
///   root. §11's inversion of the injection argument is exactly this: the reading holds a write
///   token while reading a pull request somebody else wrote, and in a box it can reach almost
///   nothing.
///
/// `Err` when the box has no placement — it was never started, or it is gone. The caller falls back
/// to the reading it would have done before any of this existed, which is why this returns a plain
/// `Result` rather than swallowing it.
pub fn model_call_in_box(
    name: &str,
    bin: &str,
    model: &str,
    prompt: &str,
    timeout: Duration,
    turn: Vec<&str>,
    github: Option<&crate::secret::Secret>,
) -> Result<Ran, String> {
    let place = crate::place::place_of(name)
        .ok_or_else(|| format!("{name} has no placement record, so skein cannot reach it"))?;
    // **Through the box's own placement, not the sandbox's** (ISO-2). It used to be written once at
    // sandbox level, under `.skein`, on the grounds that "one file serves every destination" — and
    // that is the finding: `.skein` is `--ro-bind`ed readable into every box, so one file served
    // every destination and every onlooker. It cannot simply move under the cover either, because
    // `place::Place::crossing` puts this whole script after `exec nsenter`: the `$(cat …)` is
    // evaluated *inside* the box, so a file the box cannot see is a reading with no GitHub access
    // and no message about it. So it goes where the reader is — see [`box_credential_paths`].
    let gh = github_export(&place, github);
    // **The script crosses on argv and the prompt crosses on stdin** (SKEIN-799). They are two
    // parameters because they have two ceilings: the script is one argv element and so is bounded
    // by `MAX_ARG_STRLEN`, and it is now a fixed handful of lines that no input can lengthen; the
    // prompt is a diff somebody else wrote, and a pipe bounds nothing.
    //
    // The credential above cannot use this channel, which is why it is a file and this is not. It
    // has to be a shell *variable* before `claude` starts (`export GH_TOKEN="$(cat …)"`), and a
    // crossing has one stdin — so the payload that can arrive on stdin takes it, and the one that
    // cannot gets the mode-600 file. Reversing that would put the diff on disk in the box AND
    // leave the prompt with nowhere to go.
    let ran = place.attempt(
        &model_call_script(bin, model, &turn, &gh.export),
        prompt.as_bytes(),
        timeout,
    );
    // Whatever the box answered, including nothing: the credential's life is the call's, and a
    // reading that failed is not a reason to leave the owner's GitHub token on disk.
    forget_review_token(&place, &gh);
    ran
}

/// Where a model call skein makes keeps its scratch: under the HOME skein already chose for it.
///
/// **Why skein decides this rather than the CLI.** Claude Code puts its temp directory at
/// `${os.tmpdir()}/claude-<uid>` and REFUSES to start when that path exists and is not owned by the
/// calling uid — a deliberate guard against a directory somebody else planted. In a fleet that path
/// is the sandbox's SHARED `/tmp`, which everything skein runs there writes into, and on a live
/// fleet something running as root had got there first. Every review summary then came back:
///
/// ```text
/// `claude` exited 1: Temp directory /tmp/claude-1000 is owned by uid 0, expected 1000.
/// Refusing to use it — another user may have pre-created it.
/// ```
///
/// Skein already decides which HOME the call reads its credential from ([`login_home`]). Choosing
/// the HOME and then letting whoever ran first choose the temp directory is how a fleet ends up
/// with a login that works and a model that will not start — and it is not a state anything can
/// recover from by retrying, because the offending directory outlives every call.
///
/// Measured against the real CLI, not inferred from the message: with the derived path poisoned it
/// refuses; with `CLAUDE_CODE_TMPDIR` pointed at a private path the same call answers, and the CLI
/// creates the directory itself, 0700, without being asked.
pub const MODEL_SCRATCH: &str = ".cache/skein/claude";

/// Printed on stderr by the model-call script before it does anything else, so that a failure can
/// be attributed to the program that actually failed.
///
/// **Why a marker and not an exit code.** `sbx exec` exits non-zero with its own message when the
/// daemon is not responding, when the sandbox is not running, or when it does not exist — and the
/// payload never runs. Exit 1 means whatever the program that exited chose it to mean, so nothing
/// in the code or the text can be relied on to say WHICH program that was. Skein was reading those
/// as the model refusing:
///
/// ```text
/// `claude` exited 1: <something sbx said>
/// ```
///
/// which names `claude` for a failure `claude` was never part of — the defect SKEIN-171 was filed
/// to end, surviving in the arm that fires most often. This is the evidence version of the same
/// question: the marker is absent unless a shell inside the sandbox ran the script, so its absence
/// on a failure PROVES the payload never started.
pub const REACHED: &str = "SKEIN_IN_SANDBOX";

/// The environment variables that decide WHICH CREDENTIAL a model call authenticates with, and
/// which skein removes before making one — but only when it has a login of its own to fall back on.
///
/// **Why.** `src/ai/mod.rs` opens with the contract: *rationed, lazy AI enrichment over the Claude
/// subscription — no API key*. Skein already decides which HOME the call reads its credential from
/// and which temp directory it writes; leaving the auth source to whatever launched the server is
/// the same mistake a third time. An `ANTHROPIC_API_KEY` inherited from a shell, a launch agent or
/// the sandbox's own environment silently OUTRANKS the subscription login — the one skein seeds
/// every box with, heals across the fleet, and reports as `logins: ["claude"]`.
///
/// Reported live, with every summary failing:
///
/// ```text
/// `claude` exited 1: Invalid API key · Fix external API key / ⚠ claude.ai connectors are
/// disabled because ANTHROPIC_API_KEY or another auth source is set and takes precedence over
/// your claude.ai login
/// ```
///
/// Measured here, and it is worse than the message: a stale key made the CLI HANG until the call's
/// budget ran out, so the same misconfiguration also shows up as "`claude` was still going after
/// 30s" — a sentence that sends the reader looking at diff sizes.
///
/// **Only when skein has a login to fall back on.** Somebody whose only credential IS a key would
/// otherwise have it taken away and get no authentication at all, which is a worse failure than the
/// one this fixes. The rule is "prefer the login skein manages", not "refuse keys".
///
/// The provider switches are here for the same reason as the keys: a call routed to Bedrock or
/// Vertex is not on the subscription either. `ANTHROPIC_BASE_URL` is deliberately NOT here — a
/// proxy in front of the API is still the subscription's own credential going through it.
pub const MODEL_AUTH_OVERRIDES: [&str; 4] = [
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_AUTH_TOKEN",
    "CLAUDE_CODE_USE_BEDROCK",
    "CLAUDE_CODE_USE_VERTEX",
];

/// [`MODEL_SCRATCH`] under a HOME that is already known, for the calls skein spawns itself.
pub fn model_scratch_dir(home: &std::path::Path) -> std::path::PathBuf {
    home.join(MODEL_SCRATCH)
}

/// The same rule as shell, for the calls skein hands to a shell instead of spawning.
///
/// **`$HOME` is left for the shell to expand, and that is the point.** [`model_scratch_dir`] can
/// only answer where skein already knows the HOME; a login in the sandbox, a box's agent inside its
/// own mount namespace, and a model call made through `sbx exec` all run under a HOME this process
/// cannot name — and it is that HOME which holds the credential, so it is that HOME the scratch
/// must sit under. Handing them a path resolved here would name a directory the other side does not
/// have.
///
/// One definition with several users, which is the whole reason this is a function rather than a
/// line repeated per call site. [`MODEL_SCRATCH`] argues why every one of them needs it: the CLI
/// derives `<tmp>/claude-<uid>` and refuses to start when that path belongs to somebody else, a
/// sandbox's `/tmp` is shared by everything skein runs there, and the offending directory outlives
/// every call. The reasoning was applied to the calls skein MAKES before it was applied to the
/// calls skein HOSTS — the login terminal and every box session — which is SKEIN-289.
///
/// Guarded on HOME being set, because `$HOME/…` under an unset HOME is `/…`: a scratch directory
/// at the filesystem root is a worse answer than letting the CLI derive its own.
pub fn model_scratch_export() -> String {
    format!(
        "if [ -n \"${{HOME:-}}\" ]; then export CLAUDE_CODE_TMPDIR=\"$HOME/{MODEL_SCRATCH}\"; fi"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fleet::testkit::*;

    /// **The GitHub credential reaches the box call as a `Secret`, not as characters** (SKEIN-536).
    ///
    /// A pin on the type, checked by the compiler: this coercion stops compiling the moment the
    /// parameter goes back to `Option<&str>`, which is what `src/ai/` would need to `.expose()` the
    /// credential again before the call rather than inside `github_export`.
    #[test]
    fn a_model_call_in_a_box_takes_the_github_credential_as_a_secret() {
        type Github<'a> = Option<&'a crate::secret::Secret>;
        type BoxCall =
            fn(&str, &str, &str, &str, Duration, Vec<&str>, Github) -> Result<Ran, String>;
        let _: BoxCall = model_call_in_box;
        let _: fn(&Place, Option<&crate::secret::Secret>) -> GithubCredential = github_export;
    }

    /// **Both destinations take the credential away again**, and neither may quietly stop.
    ///
    /// Read out of the source for `the_login_tick_saves_the_fleets_copy_after_healing_it`'s
    /// reason: what has to hold is a property of these two functions' *bodies*, and there is no
    /// fleet in a unit test to observe it in. A call that writes a credential and does not remove
    /// it is ISO-2 restored, and nothing about the fleet would look any different.
    #[test]
    fn a_model_call_takes_the_github_credential_away_again_wherever_it_ran() {
        // One destination, and it used to be two: the other shipped the call into the fleet sandbox
        // from a host, and went with the host (SKEIN-576). The property is unchanged for the
        // crossing that is left, and the loop went with the second name rather than the rule.
        let signature = "pub fn model_call_in_box(";
        let body = code_of(fn_body(include_str!("model.rs"), signature));
        assert!(
            body.contains("github_export"),
            "`{signature}` no longer writes the credential; this test is reading the wrong fn"
        );
        assert!(
            body.contains("forget_review_token"),
            "`{signature}` writes the owner's GitHub token and leaves it there"
        );
    }

    /// **The model call is one script, and everything it has to do it has to do before the call.**
    ///
    /// It was two destinations for a while — the fleet sandbox and a review box — and the risk of
    /// two was two scripts: one grows an `unset`, or a credential test, or a different heredoc, and
    /// the reading behaves differently depending on where it ran, which is the hardest kind of bug
    /// to see because both halves work. The sandbox destination is gone (SKEIN-576), and with it
    /// the `cd` that was the only difference between them.
    ///
    /// So what is pinned here is the script's own shape, and the ORDER in it. Three things have to
    /// happen before `-p`, and each of them is silent when it does not: the scratch directory (the
    /// CLI derives one from the shared `/tmp` and refuses to start when that path is somebody
    /// else's — root, on a live fleet, SKEIN-289), the `unset` of the API-key overrides (so the
    /// login skein put in this HOME is what is spent, and only where there IS one to prefer), and
    /// the marker that says the payload started at all. These moved here from `ai`, where they were
    /// asserted through a transport that no longer exists.
    ///
    /// **What would make this fail:** moving any of the three after the `-p`, or resolving the
    /// scratch path on this side of the crossing so it names a directory the box does not have.
    #[test]
    fn a_model_call_is_one_script_that_prepares_itself_before_it_calls() {
        let turn = ["--resume", "an id with a space"];
        let script = model_call_script("claude", "sonnet", &turn, "");
        let call = script.find("-p").expect("the model call itself");

        // A box is already standing in its own tree, so the script must carry no `cd` — one would
        // take the conversation out of the directory it is filed under, and the reading would come
        // back looking perfectly ordinary.
        assert!(
            !script.contains("cd "),
            "a box call walked somewhere: {script}"
        );
        // The id is a value and values are quoted. Unquoted, an id with a space becomes two
        // arguments and the resume silently becomes a fresh conversation.
        assert!(
            script.contains("'an id with a space'"),
            "the turn's id was not quoted: {script}"
        );
        // The marker that says the script reached the far side at all — `ai::from_sandbox` reads
        // it to tell "the CLI refused" apart from "the payload never ran". First, and before the
        // call, or its absence stops being evidence of anything.
        assert!(script.trim_start().starts_with("printf"), "{script}");
        assert!(
            script.find(REACHED).is_some_and(|at| at < call),
            "the call cannot prove it reached the far side, so a transport failure will be \
             reported as the model refusing: {script}"
        );
        // `$HOME` unexpanded, because it is the BOX's home that holds the credential, not this
        // process's — and a path resolved here names a directory the box does not have.
        let scratch = script
            .find("CLAUDE_CODE_TMPDIR")
            .expect("the model call took a shared /tmp, which anything can poison");
        assert!(
            script.contains(&format!("\"$HOME/{MODEL_SCRATCH}\"")),
            "the scratch path was resolved on this side of the crossing: {script}"
        );
        assert!(
            scratch < call,
            "the scratch directory is exported after the call it is for: {script}"
        );
        // And the far side's own environment does not get to choose the credential either.
        // Conditional on there being a login to prefer, so a HOME authenticated by a key keeps it.
        let unset = script
            .find("unset ANTHROPIC_API_KEY")
            .expect("an API key on the far side still outranks the login skein put there");
        assert!(
            script.contains(".claude/.credentials.json") && unset < call,
            "the key is unset unconditionally, or after the call it is for: {script}"
        );
    }

    /// **The prompt is still in the pipe when the call reads it** (SKEIN-799).
    ///
    /// This replaces the test that pinned the heredoc delimiter growing past a prompt that
    /// contained it, and the replacement is the point: the prompt no longer travels inside this
    /// script, so there is no delimiter to collide with and that hazard is gone rather than
    /// handled. What took its place is a
    /// property of the same shape — **nothing before `-p` may read stdin.** The prompt sits in the
    /// pipe while the script's preparations run, and a line that consumed it would hand `claude`
    /// half a question, or none, which is exactly the failure the grown delimiter existed to
    /// prevent with nothing left to blame it on.
    ///
    /// **Run, not read**, the way
    /// `a_review_credential_is_private_from_its_first_byte_and_gone_when_the_call_returns` runs the
    /// credential's wire format: a real `bash` is handed the real script with a prompt on its stdin
    /// and a fake `claude` that keeps whatever it is asked. Grepping the source for `< ` or `read `
    /// would miss `$(cat)` and every other spelling, and would pass on a script that never ran.
    ///
    /// The credential export is included deliberately — it is the only preparation that runs a
    /// command at all, so it is the only one that could plausibly reach for stdin.
    ///
    /// **What would make this fail:** putting the prompt back in a heredoc, which hands the fake
    /// nothing; or putting anything that consumes stdin in front of the call.
    #[cfg(unix)]
    #[test]
    fn the_prompt_is_still_in_the_pipe_when_the_call_reads_it() {
        use std::io::Write as _;
        use std::os::unix::fs::PermissionsExt;
        let home = crate::testutil::tempdir();
        let home = home.as_ref() as &std::path::Path;
        let got = home.join("what-the-model-was-asked");
        let bin = home.join("claude-that-keeps-what-it-was-asked");
        std::fs::write(
            &bin,
            format!("#!/usr/bin/env bash\ncat > {}\n", got.display()),
        )
        .unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        let token = home.join("a-token");
        std::fs::write(&token, "skein-test-gh-token").unwrap();
        let gh = format!(
            "export GH_TOKEN=\"$(cat {t})\" GITHUB_TOKEN=\"$(cat {t})\"\n",
            t = sh_quote(&token.display().to_string())
        );

        // Carrying the delimiter the heredoc used to grow past, so a revert to that shape is
        // failed by this test on the same input the deleted one was written for.
        let prompt = "read this change.\nSKEIN_PROMPT\nand that line ended the question.";
        let script = model_call_script(&bin.display().to_string(), "sonnet", &[], &gh);
        let mut child = std::process::Command::new("bash")
            .arg("-c")
            .arg(&script)
            .env("HOME", home)
            .stdin(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("bash");
        child
            .stdin
            .take()
            .unwrap()
            .write_all(prompt.as_bytes())
            .unwrap();
        assert!(child.wait().expect("bash").success(), "{script}");

        assert_eq!(
            std::fs::read_to_string(&got).expect("the model was never run at all"),
            prompt,
            "something between the crossing and `-p` read the prompt out of the pipe, or the \
             prompt did not arrive whole: {script}"
        );
    }
}
