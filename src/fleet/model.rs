//! Where a model call keeps its scratch directory, and the environment it must not inherit.


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

