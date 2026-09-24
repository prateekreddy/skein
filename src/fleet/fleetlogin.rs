//! The fleet's own copy of a login, kept on the host so it outlives the sandbox: syncing it,
//! capturing it, deciding which way a login moves, and the `skein login` command lines.

use super::*;

/// Where the fleet's logins are kept on the HOST, so they outlive the sandbox.
///
/// Not the shared project store — that is data the boxes read, and a credential has no business in
/// it. This is skein's own directory, beside the box state it already keeps there.
pub(crate) fn fleet_home_dir() -> std::path::PathBuf {
    skein_home().join("fleet-home")
}

/// Can this HOME's Claude credential still be used?
///
/// **It reads a field; it does not refresh anything** — this said "refreshing it if need be" for a
/// long time and never did. Which matters, because reading the field is strictly weaker than
/// trying: a credential a sibling superseded by refreshing goes on claiming its original expiry to
/// the day, and this reports it usable right up until something spends it and is refused. Nothing
/// in the file can tell those apart. What CAN, and what `heal_logins_script:rank` uses to choose
/// between copies, is `expiresAt` — a fresh access token is a refresh that actually happened.
///
/// **`refreshTokenExpiresAt`, not `expiresAt`.** The access token expires in hours and Claude Code
/// renews it without being asked, so a past `expiresAt` is the ordinary state of a perfectly good
/// login; a check against it would report every fleet as signed out most of the day. What decides
/// whether a credential is still worth anything is the REFRESH token's expiry. Absent means an older
/// shape that does not record one, and the honest answer there is "usable" — declining to use a
/// credential because it declined to say when it dies is a worse failure than trying and being told.
pub fn refreshable_login_at(home: &std::path::Path) -> bool {
    let Ok(bytes) = std::fs::read(home.join(".claude/.credentials.json")) else {
        return false;
    };
    if !carries_login(&bytes) {
        return false;
    }
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
        return false;
    };
    let Some(dies) = value
        .get("claudeAiOauth")
        .and_then(|b| b.get("refreshTokenExpiresAt"))
        .and_then(|v| v.as_i64())
    else {
        return true;
    };
    dies > chrono::Utc::now().timestamp_millis()
}

/// The HOME skein should run its own `claude` calls with, when the ambient one will not do.
///
/// Skein keeps the fleet's login under `fleet-home` precisely so it always has one — it is what
/// `signed_in_runtimes` reports and what seeds every box. And then `ai::claude_oneshot` spawned
/// `claude` with no environment at all, so the call read whatever HOME the SERVER happened to be
/// started with. On a host where those differ the result is `Not logged in · Please run /login`
/// from a skein whose own health report says `logins: ["claude"]` in the same breath.
///
/// Same shape as the review queue refusing to use the `gh` login it was already seeding boxes from:
/// a credential the user gave skein, held and not used for a job it is capable of.
pub fn login_home() -> Option<std::path::PathBuf> {
    let home = fleet_home_dir();
    refreshable_login_at(&home).then_some(home)
}

/// Every runtime the fleet could hold a login for, each with its [`LoginState`].
///
/// Read from the host's own copy under `fleet-home`, not from the sandbox: this answers the first
/// question a new user has ("did `skein login` work?") and it must answer it with the fleet down,
/// during setup, before any box exists.
pub fn runtime_logins() -> Vec<RuntimeLogin> {
    let dir = fleet_home_dir();
    let now_ms = chrono::Utc::now().timestamp_millis();
    LOGIN_FILES
        .iter()
        .map(|rel| RuntimeLogin {
            runtime: runtime_of(rel),
            state: match std::fs::read(dir.join(rel)) {
                Ok(bytes) => login_state(&bytes, now_ms),
                Err(_) => LoginState::Absent,
            },
        })
        .collect()
}

/// Whose credential one of [`LOGIN_FILES`] is. One definition, because [`login_written_ms`] has to
/// walk the same list backwards and two spellings of the same mapping is how they come apart.
pub(super) fn runtime_of(rel: &str) -> &'static str {
    match rel.starts_with(".codex") {
        true => "codex",
        false => "claude",
    }
}

/// When a credential this fleet's model calls would read was last **written**, in epoch ms.
///
/// **This is evidence that a refusal is out of date**, and that is the only thing it is for. A
/// remembered refusal (`crate::ai::auth_refusal`) is a fact about the credential that was there at
/// one moment; a credential written after that moment is a different credential, and nothing the
/// model said about the old one applies to it. See `crate::ai`'s refusal memory for the rule and
/// `prq::what_github_said` for where this project first wrote it down.
///
/// **Both HOMEs, newest wins**, because `crate::ai::tried` chooses between exactly these two: the
/// fleet's own `fleet-home` when it holds a refreshable login, and the ambient `$HOME` when it does
/// not. Asking only about the first would leave a person who logged in on the host's own HOME
/// staring at a banner that will not clear — which is the shape of the fault this answers.
///
/// **Mtime and not `refreshTokenExpiresAt`.** The question is "has this credential been replaced
/// since we were told it was bad", not "is the replacement any good": if it is also bad the next
/// call is refused again and says so, which costs one call and re-plants a refusal that is true.
/// Reading the expiry instead would trust a claim about the future to overturn a report about the
/// past — and `box-session.sh` has the long version of why those two are not the same question.
///
/// `None` when there is no such file in either HOME, which is not evidence of anything.
pub fn login_written_ms(runtime: &str) -> Option<i64> {
    let rel = LOGIN_FILES.iter().find(|rel| runtime_of(rel) == runtime)?;
    let homes = [
        Some(fleet_home_dir()),
        std::env::var_os("HOME").map(std::path::PathBuf::from),
    ];
    homes
        .into_iter()
        .flatten()
        .filter_map(|home| written_ms(&home.join(rel)))
        .max()
}

/// **The credential itself, as one number** — for deciding whether a remembered refusal has been
/// answered (SKEIN-348).
///
/// It hashes the TOKEN VALUES and not the file, and that distinction is the whole point. An OAuth
/// client rewrites its credentials file when a refresh ATTEMPT FAILS — timestamps, attempt counters,
/// whatever it keeps — so both "is the file newer" and "has the file changed" answer yes to the very
/// event that PROVES the credential is dead. Reported live twice, the second time as "there is no
/// popup though". What a person changes by logging in again is the token, so the token is what is
/// compared.
///
/// `None` when there is no readable credential to fingerprint, and that is deliberately not the
/// same as a fingerprint of nothing: the caller has to tell "I could not look" from "it is
/// different", because only the second may clear a refusal.
pub fn login_fingerprint(runtime: &str) -> Option<u64> {
    use std::hash::{Hash, Hasher};
    let rel = LOGIN_FILES.iter().find(|rel| runtime_of(rel) == runtime)?;
    let homes = [
        Some(fleet_home_dir()),
        std::env::var_os("HOME").map(std::path::PathBuf::from),
    ];
    for home in homes.into_iter().flatten() {
        let Ok(bytes) = std::fs::read(home.join(rel)) else {
            continue;
        };
        let Ok(v) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
            continue;
        };
        // The same walk [`carries_login`] and [`login_state`] make, so the three cannot disagree
        // about which block is the credential: the first block carrying a token wins.
        for block in [v.get("claudeAiOauth"), v.get("tokens"), Some(&v)]
            .into_iter()
            .flatten()
            .filter_map(|b| b.as_object())
        {
            let mut tokens: Vec<&str> = LOGIN_KEYS
                .iter()
                .filter_map(|k| block.get(*k).and_then(|t| t.as_str()))
                .filter(|t| !t.trim().is_empty())
                .collect();
            if tokens.is_empty() {
                continue;
            }
            // Sorted, so a client that reorders its own JSON is not read as a new credential.
            tokens.sort_unstable();
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            tokens.hash(&mut hasher);
            return Some(hasher.finish());
        }
    }
    None
}

/// A file's mtime in epoch milliseconds, or `None` when there is no file to ask.
fn written_ms(path: &std::path::Path) -> Option<i64> {
    let at = std::fs::metadata(path).ok()?.modified().ok()?;
    // A pre-1970 mtime is nonsense on a credential and still has an answer; inventing 0 for it
    // would read as "written at the epoch", which is older than every refusal rather than newer.
    Some(match at.duration_since(std::time::UNIX_EPOCH) {
        Ok(since) => since.as_millis() as i64,
        Err(before) => -(before.duration().as_millis() as i64),
    })
}

/// Which runtimes have a login the fleet can hand to a new box **that still works**.
///
/// [`LoginState::Live`] only. An expired credential is deliberately not "signed in": reporting it
/// as one is how a fleet-wide logout read as "each box needs a login". It is also not dropped from
/// what flows — seeding and healing keep the dead token, which a heal can refresh where a void
/// cannot be; [`expired_logins`] is where it is reported instead.
pub fn signed_in_runtimes() -> Vec<String> {
    runtime_logins()
        .into_iter()
        .filter(|l| matches!(l.state, LoginState::Live))
        .map(|l| l.runtime.to_string())
        .collect()
}

/// The runtimes whose kept credential has died, with when — see [`ExpiredLogin`].
///
/// **Two witnesses, and the second is the one that catches what the first cannot.** The file says
/// when its refresh token is due to expire, and a token that was revoked — or that simply fails to
/// refresh — passes that test while every model call comes back `Failed to authenticate: OAuth
/// session expired and could not be refreshed`. Reported live from a cockpit in exactly that state:
/// it put that sentence on a pull request row and no banner anywhere, because nothing asked the
/// model what it had just been told. `ai::auth_refusal` is that answer, and it outranks the file:
/// the file is a claim about the future, the refusal is what happened.
///
/// **The two clear differently, which is why [`Witness`] rides along.** The file half needs nothing
/// pressed — a login completed anywhere rewrites it and the next poll is clean. The refusal half is
/// remembered in this process, and used to be cleared only by this process's own `skein login`, by a
/// call that then worked, or by a restart; a login done in a box, in a second skein or in the
/// desktop app left the banner up over a credential that was fine. `ai::refusal_still_standing` is
/// where that was fixed — a credential written after the refusal contradicts it, and a refusal
/// nothing can contradict expires on a clock — and the witness is how a reader can tell which of
/// the two sentences they are looking at.
pub fn expired_logins() -> Vec<ExpiredLogin> {
    let refused = crate::ai::auth_refusal();
    let mut out: Vec<ExpiredLogin> = runtime_logins()
        .into_iter()
        .filter_map(|l| match l.state {
            LoginState::Expired { at_ms } => Some(ExpiredLogin {
                runtime: l.runtime.to_string(),
                expired_at: chrono::DateTime::<chrono::Utc>::from_timestamp_millis(at_ms)
                    .map(|t| t.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
                    // A timestamp outside chrono's range is still a death date, just not a sayable
                    // one; the raw milliseconds beat inventing a calendar date.
                    .unwrap_or_else(|| format!("{at_ms}ms")),
                witness: Witness::Credential,
                said: String::new(),
            }),
            _ => None,
        })
        .collect();
    // Added rather than replacing: a file that says expired and a model that says refused are the
    // same fact told twice, and the file's own death date is the better one to show when it has it.
    if let Some(refusal) = refused {
        if !out.iter().any(|e| e.runtime == refusal.runtime) {
            out.push(ExpiredLogin {
                runtime: refusal.runtime.to_string(),
                expired_at: chrono::DateTime::<chrono::Utc>::from_timestamp_millis(refusal.at_ms)
                    .map(|t| t.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
                    .unwrap_or_else(|| format!("{}ms", refusal.at_ms)),
                // Which stamp this is, said out loud: `expired_at` here is when skein was TOLD, not
                // when the credential died — nothing can know the second — and the two read
                // identically on a banner that does not distinguish them.
                witness: Witness::Refusal,
                said: refusal.said,
            });
        }
    }
    out
}

/// Keep the fleet's login on the host, and put it back into a sandbox that has none.
///
/// `skein login` writes into the sandbox's own HOME, which is VM-local — so a resize destroyed it
/// along with everything else, and "log in once" quietly became "log in after every resize".
/// Measured: after a rebuild the sandbox came back with `cred=GONE`.
///
/// Newest wins, in one direction at a time: a sandbox that has the credential is the live copy and
/// refreshes the host's; a sandbox without one is freshly built and gets the host's back. Boxes
/// already reconcile into the sandbox at session start, so a re-login anywhere reaches here too.
///
/// Best-effort by design: a fleet running on API keys has no login to carry, and failing a launch
/// over that would be absurd.
///
/// And a credentials file is not a login. `box-session.sh` learned that — a logged-out agent leaves
/// the file in place with its tokens blanked, and that husk is *newer* than the working copy it
/// replaced — but this side was still "non-empty wins", so the sandbox's husk overwrote the host's
/// saved login and the fleet lost the copy it keeps precisely so a rebuild can restore it. Same bug,
/// one layer up. [`carries_login`] is the same test the launcher applies, kept in step by
/// `the_host_and_the_launcher_agree_on_what_a_login_is`.
pub fn sync_fleet_login(sandbox: &str) {
    for (rel, why) in sync_fleet_login_with(sandbox, true) {
        eprintln!(
            "skein: could not read the {rel} login out of {sandbox} ({why}) — the host's kept copy \
             is left exactly as it was, because a read that failed says nothing about what is in \
             there."
        );
    }
}

/// The same last leg, refusing to move a credential DOWN into the sandbox.
///
/// For the one caller that has just elected a login into `$HOME` itself ([`heal_logins`]): there,
/// the sandbox's copy is newer than the host's by construction, so a `Restore` would undo the heal
/// it was called to finish.
pub fn sync_fleet_login_saving_only(sandbox: &str) {
    for (rel, why) in sync_fleet_login_with(sandbox, false) {
        eprintln!("skein: could not read the {rel} login out of {sandbox} ({why}) — left alone.");
    }
}

/// Capture the fleet's logins to the host, **and say so if any of them could not be read**.
///
/// For the one caller that is about to destroy the sandbox. Everywhere else a failed read is a
/// minute lost and the next tick tries again; at a resize it is the last chance the credential
/// will ever have, and the comment at that call site records the loss having already happened once
/// (SKEIN-347). Same reading as the Docker check beside it: could not ask is not the same as
/// nothing to lose.
pub fn capture_fleet_login(sandbox: &str) -> Result<(), String> {
    let failed = sync_fleet_login_with(sandbox, true);
    if failed.is_empty() {
        return Ok(());
    }
    Err(failed
        .into_iter()
        .map(|(rel, why)| format!("{rel}: {why}"))
        .collect::<Vec<_>>()
        .join("; "))
}

/// Returns the login files that could not be read out of the sandbox, and why.
///
/// **A failed read is not an absent login.** This asked for the bytes with `.unwrap_or_default()`,
/// so an exec that timed out, or a sandbox that was not answering, produced an empty `Vec<u8>` —
/// and [`login_move`] then judged that emptiness as the sandbox carrying no credential. From there
/// `(absent here, present there)` is `Restore`, which pushes the host's copy back down over a live
/// login the read simply failed to see; and at a resize the same emptiness means nothing is saved
/// before the HOME that held it is destroyed. The liveness rule below (SKEIN-294) reasons about
/// what the bytes say, and was written assuming the bytes are what is on disk.
///
/// So a file that could not be read is skipped entirely — neither direction moves — and named to
/// the caller, which decides whether that is a warning or a reason to stop.
fn sync_fleet_login_with(sandbox: &str, allow_restore: bool) -> Vec<(&'static str, String)> {
    let fleet = own_sandbox(sandbox);
    let dir = fleet_home_dir();
    let now_ms = chrono::Utc::now().timestamp_millis();
    let mut unread: Vec<(&'static str, String)> = Vec::new();
    for rel in LOGIN_FILES {
        let host = dir.join(rel);
        // `|| true` inside the script keeps an ABSENT file a successful, empty read — which is a
        // real answer and the one this loop is mostly given. An `Err` here is the transport
        // failing: no such sandbox, a timeout, an exec that could not start.
        let in_sandbox = match fleet.bytes(
            &format!("cat \"$HOME\"/{} 2>/dev/null || true", sh_quote(rel)),
            Duration::from_secs(20),
        ) {
            Ok(bytes) => bytes,
            Err(why) => {
                unread.push((rel, why));
                continue;
            }
        };
        let saved = std::fs::read(&host).ok();
        match login_move(&in_sandbox, saved.as_deref(), now_ms) {
            LoginMove::Save => {
                // This is the fleet's kept copy of its own login: the file that exists so a rebuild
                // can put the credential back. It was written with a bare `fs::write` straight over
                // the target and chmodded afterwards, which is two faults on the one file that must
                // survive. A crash mid-write leaves a truncated credential where a whole one was —
                // and `carries_login` reads a truncated file as NO login, so the fleet would quietly
                // believe it had never been signed in. The chmod-after leaves it readable at the
                // process umask in between. Both are `crate::secret::write_bytes`'s job now.
                //
                // `write_bytes` and not `write`: this is a JSON document that has to land exactly
                // as it came out of the sandbox, and a `Secret` would trim it.
                if let Some(parent) = host.parent() {
                    let _ = std::fs::create_dir_all(parent);
                    if crate::secret::write_bytes(&host, &in_sandbox).is_err() {
                        eprintln!(
                            "skein: could not save the {rel} login out of {sandbox} — the copy \
                             that was already there is untouched"
                        );
                    }
                }
            }
            LoginMove::Restore if !allow_restore => {}
            LoginMove::Restore => {
                // `umask 077` around the `cat`, so a login this CREATES is owner-only from its
                // first byte rather than from the chmod after — the rule `crate::secret::write`
                // keeps, spelled in the shell this write is made in. The chmod stays for a file
                // that already existed, whose mode a `cat >` keeps. Not a temp and a rename, which
                // would replace a symlink at this path where the `cat` writes through it, and
                // nothing here says which of the two the sandbox's HOME relies on.
                let restore = format!(
                    "mkdir -p \"$(dirname \"$HOME\"/{r})\" && (umask 077; cat > \"$HOME\"/{r}) && chmod 600 \"$HOME\"/{r}",
                    r = sh_quote(rel)
                );
                let saved = saved.expect("Restore is only returned when the host has a copy");
                if let Err(e) = fleet.write(&restore, &saved, Duration::from_secs(30)) {
                    eprintln!("skein: could not restore the {rel} login into {sandbox}: {e}");
                }
            }
            LoginMove::Neither => {}
        }
    }
    unread
}

/// Which way a login should move between the sandbox and the host's kept copy.
#[derive(Debug, PartialEq, Eq)]
enum LoginMove {
    /// The sandbox has the live login; the host's copy is refreshed from it.
    Save,
    /// The sandbox has none; the host puts its copy back.
    Restore,
    /// Nobody has a login to move. Not an error — a fleet on API keys never has one.
    Neither,
}

/// The rule, as a decision rather than as control flow — because getting it wrong is silent, and
/// costs the fleet the copy it keeps precisely so a rebuild can restore it.
///
/// "The sandbox has a file" was the old test, and a husk is a file. A logged-out sandbox therefore
/// overwrote a perfectly good saved login, and after that there was nothing left to heal from.
///
/// **Then the same hole one notch along.** The husk was caught and the *expired* credential was
/// not: [`carries_login`] never looks at expiry, so a sandbox holding an invalidated token still
/// answered "I have a login" and still overwrote the host's live one. That was survivable while
/// this ran twice in a fleet's life — at `ensure_fleet` and at a resize. [`heal_logins`] now runs
/// it every minute, and at that frequency the window is not a window: any minute the sandbox's copy
/// is dead and the host's is not, the kept copy is destroyed and there is nothing left to restore
/// from. So the question this asks is [`login_state`]'s, not [`carries_login`]'s — which is also
/// the question the launcher's `login_life` asks one layer down — though only since SKEIN-488,
/// which is worth knowing before trusting this sentence: it was written as "has always asked" and
/// it was not true. `login_life` read `expiresAt` and this reads `refreshTokenExpiresAt`, so on a
/// live fleet the two ranked the same five files in opposite orders. They ask one question each
/// now, deliberately: `refreshTokenExpiresAt` whether a copy may be seeded FROM, `expiresAt` which
/// of the copies that may is best.
///
/// A dead token is still not worthless: a heal can refresh one where it cannot conjure a void. So
/// it fills an empty kept copy, and never displaces a working one.
fn login_move(in_sandbox: &[u8], on_host: Option<&[u8]>, now_ms: i64) -> LoginMove {
    // Two copies of the same bytes have nowhere to move. **Not an optimisation.**
    // [`login_written_ms`] reads this file's MTIME as the evidence that a credential was replaced
    // since a refusal was recorded against it (`crate::ai`'s refusal memory), so rewriting
    // identical bytes on every tick would report a brand-new credential every minute and clear
    // every remembered refusal forever — a memo that can never stand is the same bug as one that
    // can never be cleared, pointed the other way.
    if on_host == Some(in_sandbox) {
        return LoginMove::Neither;
    }
    // **Presence decides the direction; expiry is only ever allowed to guard it** (SKEIN-349).
    //
    // This asked `login_state` for a day and it broke the shared login fleet-wide. The rule it
    // violated is written twice in this file — on [`LoginState`], "propagation keeps asking
    // `carries_login`", and on [`RuntimeLogin`], "nothing that seeds or heals reads this" — and the
    // reason is given there too: a dead token still seeds boxes, because a heal can replace it and
    // nothing can replace a void. Judged on expiry, two expired copies matched neither arm, fell to
    // `Neither`, and the last leg of the chain moved nothing at all — which is precisely the state
    // a fleet is in while somebody is trying to log back in. Reported live: "the shared login stuff
    // also seems to be failing while it was working earlier."
    let here = carries_login(in_sandbox);
    let there = on_host.is_some_and(carries_login);
    let live = |bytes: &[u8]| matches!(login_state(bytes, now_ms), LoginState::Live);
    match (here, there) {
        // **The one thing expiry may still say**, and the concern that produced the wrong answer
        // first time round: at once a minute, an invalidated copy must not destroy a live one. Where
        // both sides carry a credential and only the HOST's still works, the host's wins.
        (true, true) if !live(in_sandbox) && on_host.is_some_and(live) => LoginMove::Restore,
        // Otherwise the sandbox wins, because the sandbox is where a person logs in. This is the leg
        // a box-side `/login` travels: the heal script has just carried it from the box's private
        // HOME into the sandbox's, and this carries it the rest of the way to the file every surface
        // reads. A credential skein cannot vouch for still travels — see above.
        (true, _) => LoginMove::Save,
        // Nothing in the sandbox and something on the host: put the host's back. Covers a freshly
        // rebuilt sandbox (empty HOME) and a logged-out one (a husk).
        (false, true) => LoginMove::Restore,
        // Two voids. Not an error — a fleet on API keys never has one.
        (false, false) => LoginMove::Neither,
    }
}

/// The command that authenticates `runtime` inside the fleet sandbox, and why it differs per runtime.
///
/// There is no uniform spelling to guess at: `codex login` exists, `claude login` does not.
///
/// Claude's headless-looking option, `setup-token`, was tried here and does not do what this needs:
/// it returns a long-lived token to export as an environment variable and leaves no credential
/// behind, so the sandbox still answered "Not logged in · Please run /login" and `~/.claude` held
/// nothing but `backups`. Seeding a box copies FILES, so the flow that writes one is the flow that
/// works — `/login` inside the TUI. An unknown runtime gets a plain shell rather than a command
/// that fails in an unhelpful way.
///
/// **It brings its own scratch directory**, from [`model_scratch_export`], for the same reason
/// [`MODEL_SCRATCH`] gives — and this is the entry point that reported the fault. The login runs the
/// CLI in the fleet's shared `/tmp`, where something root-owned had already made `claude-1000`, so
/// the flow that OAuth completed for still ended in "Refusing to use it" (SKEIN-289). Here rather
/// than at the two call sites: `skein login` and the cockpit's login terminal both come through
/// this function, and a fix at one of them would have left the other reporting it.
///
/// Prefixed on the front of every arm, including `exec bash -l` — a runtime skein does not know
/// still gets a shell whose agent, whatever it is, is not standing in the shared `/tmp`.
pub fn fleet_login_command(runtime: &str) -> String {
    let command = match runtime {
        "codex" => "codex login",
        "claude" => "claude", // then /login inside it
        _ => "exec bash -l",
    };
    format!("{}\n{command}", model_scratch_export())
}

/// Log in to a runtime once, in the sandbox's own HOME, so every box inherits it.
///
/// The sandbox is deliberately not on the board — it is not a box — so there is otherwise no way to
/// reach the one HOME that seeds all the others. Interactive by construction: every one of these
/// flows prints a URL and waits, so the terminal has to be the user's.
pub fn fleet_login(runtime: &str) -> Result<(), String> {
    let sandbox = fleet_sandbox();
    let (program, argv) = login_argv(&sandbox, runtime);
    let args: Vec<&str> = argv.iter().map(String::as_str).collect();
    match run_attached(program, &args)? {
        0 => Ok(()),
        code => Err(format!("login in {sandbox} exited {code}")),
    }
}

/// The login command as something a server can spawn on a PTY it owns, rather than run attached
/// to its own terminal. Same argv as [`fleet_login`] — this exists because the
/// cockpit's login route needs the (program, argv) shape and `login_argv` is deliberately private:
/// which sandbox the login runs in is this module's decision, not a caller's.
///
/// **It returns the pair rather than a `Result`, and the narrowing is the fix** (SKEIN-774). This
/// had exactly one failure — `fleet_sandbox().is_empty()` — which [`crate::config::load_config`]
/// forecloses and SKEIN-756 deleted along with the other sixteen copies of it, leaving an `Err` no
/// value could inhabit. A signature that promises a failure that cannot happen is the same fiction
/// one level up, and it is not free: the cockpit's login route carried a refusal arm for it, with a
/// sentence, a close code and a "press log in again" step that SKEIN-702 spent effort writing for a
/// pane nobody can be shown. [`login_argv`] returns a tuple and cannot fail, so neither can this.
pub fn login_spawn_argv(runtime: &str) -> (&'static str, Vec<String>) {
    let sandbox = fleet_sandbox();
    login_argv(&sandbox, runtime)
}

/// Everything a successful login must be followed by, run **in the process that calls it** — which
/// is the point. `skein login` used to run this tail inline in the CLI process, and the
/// long-running server's refusal memory (`ai`'s remembered refusal) was untouched: the fleet kept
/// declining model calls over a credential that had just been fixed. The server's login route
/// calls this so the memory that clears is the one that was refusing.
///
/// Returns the outcome as sentences rather than printing them, because the two callers speak
/// different surfaces: `cmd_login` eprintlns them, the login WebSocket sends them down the socket.
pub fn after_login(runtime: &str) -> Vec<String> {
    // The model may have been refusing every call because of the credential that just changed.
    crate::ai::forget_refusal();
    // And into the boxes that already exist. Without this the line above was the whole story, and
    // the story was "restart twelve boxes or sign in twelve times" — which is what sharing a login
    // exists to prevent. Best-effort: the login itself succeeded, and failing now would report a
    // working login as a failure.
    vec![share_outcome(runtime, share_login_with_boxes())]
}

/// The one sentence [`after_login`] says about handing the login to running boxes. Split from the
/// call so every arm is testable without a sandbox to share into.
fn share_outcome(runtime: &str, shared: Result<Vec<String>, String>) -> String {
    match shared {
        Ok(reached) if reached.is_empty() => format!(
            "every new box inherits this {runtime} login; there are no existing boxes to give it to"
        ),
        Ok(reached) => format!(
            "every new box inherits this {runtime} login, and {} existing box(es) now hold it: {}",
            reached.len(),
            reached.join(", ")
        ),
        Err(why) => format!(
            "logged in, but could not hand it to the boxes already running ({why}) — they pick it \
             up when their session next starts"
        ),
    }
}

/// The program and arguments a `skein login` runs, split out so both shapes can be read.
///
/// The same command either way; what differs is whether skein has to get to the sandbox first. In
/// the fleet it is already there, so the login runs directly — and it must, because `sbx` is not in
/// the sandbox to run it with. The terminal is the user's in both, which is the whole reason this is
/// an attached run rather than a captured one: the device flows print a URL and wait for somebody to
/// open it.
pub(super) fn login_argv(sandbox: &str, runtime: &str) -> (&'static str, Vec<String>) {
    // **One arm** (SKEIN-576). The `sbx exec -it` hop was the host's way of getting to the sandbox
    // before running the same command; skein is already in it, and `sbx` is not here to run. The
    // terminal is the person's either way, which is why this is an attached run rather than a
    // captured one: the device flows print a URL and wait for somebody to open it.
    let _ = sandbox;
    (
        "bash",
        vec!["-lc".to_string(), fleet_login_command(runtime)],
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::*;

    /// The cockpit's login banner fires on what the MODEL said, not only on what the file claims
    /// (reported live: the row said the OAuth session had expired and no banner appeared anywhere).
    #[test]
    fn a_model_that_says_the_login_is_dead_is_a_dead_login() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        std::env::set_var("SKEIN_HOME", home.as_ref() as &std::path::Path);
        crate::ai::forget_refusal();

        // No credential on disk at all: the file half has nothing to say, which is exactly the
        // state that used to leave the banner silent while every reading failed.
        assert!(
            expired_logins().is_empty(),
            "an absent credential is not an expired one — that is `logins: []`, a different sentence"
        );

        crate::ai::plant_refusal_saying(
            "`claude` exited 1: Failed to authenticate: OAuth session expired and could not be refreshed",
        );
        let dead = expired_logins();
        assert_eq!(
            dead.iter().map(|e| e.runtime.as_str()).collect::<Vec<_>>(),
            vec!["claude"],
            "the model said the credential is dead and nothing reported it"
        );
        assert!(
            !dead[0].expired_at.is_empty(),
            "the banner shows when it was found out; an empty stamp reads as a bug in the banner"
        );

        // A refusal that says nothing about the credential must NOT be reported as a dead login:
        // sending somebody to log in over a rate limit is a cure for a problem they do not have.
        crate::ai::forget_refusal();
        crate::ai::plant_refusal_saying("`claude` exited 1: rate limit reached, try again later");
        assert!(
            expired_logins().is_empty(),
            "a rate limit was reported as a dead login"
        );

        crate::ai::forget_refusal();
        std::env::remove_var("SKEIN_HOME");
    }

    /// The banner goes away when the login is completed **somewhere else**, with nothing pressed.
    ///
    /// The question, in the form it was asked: *"when login is complete from other session or
    /// something does the bar go away?"* Partly — and which half you were looking at was not
    /// visible from the banner. The file half clears itself on the next poll. The refusal half is
    /// remembered in the server's own process and reached none of the ways a login can happen
    /// elsewhere: a `/login` inside a box, a second skein, the desktop app. It stood until a call
    /// succeeded or somebody restarted the server, so a person who logged in, looked, and concluded
    /// it had not worked was reading a true statement about the past.
    ///
    /// Driven through `expired_logins`, which is what `/api/health` serializes and what the banner
    /// renders — the surface the question was actually about.
    #[test]
    fn a_login_completed_anywhere_takes_the_banner_down_with_nothing_pressed() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        let home = home.as_ref() as &std::path::Path;
        let mut env = crate::testutil::env_pins();
        env.set("SKEIN_HOME", home);
        // The ambient HOME counts as evidence too, so it is pointed somewhere this test owns
        // rather than at whatever the machine running the suite happens to hold.
        env.set("HOME", home.join("ambient"));
        crate::ai::forget_refusal();

        crate::ai::plant_refusal_aged(
            "`claude` exited 1: Failed to authenticate: OAuth session expired and could not be refreshed",
            std::time::Duration::from_secs(600),
        );
        let dead = expired_logins();
        assert_eq!(
            dead.iter().map(|e| e.runtime.as_str()).collect::<Vec<_>>(),
            vec!["claude"],
            "the banner is not up, so this test would pass over the bug it is about"
        );
        // The two witnesses are different sentences and lead to different actions, so the banner is
        // told which one it has — and given the words, which only this one has.
        assert_eq!(
            dead[0].witness,
            Witness::Refusal,
            "a refusal is reported as the credential file's own expiry date, which is a date \
             nobody can check and an action that may not be needed"
        );
        assert!(
            dead[0].said.contains("OAuth session expired"),
            "the refusal reached the banner with its own words dropped: {:?}",
            dead[0].said
        );

        // The login happens somewhere this process cannot see. Nothing here is pressed, nothing
        // calls `forget_refusal`, nothing restarts: a credential is simply written.
        let credential = fleet_home_dir().join(".claude/.credentials.json");
        std::fs::create_dir_all(credential.parent().unwrap()).unwrap();
        std::fs::write(
            &credential,
            br#"{"claudeAiOauth":{"accessToken":"a","refreshToken":"r","refreshTokenExpiresAt":32503680000000}}"#,
        )
        .unwrap();
        assert!(
            expired_logins().is_empty(),
            "the bar is still up after a login completed elsewhere, which reads as the login having \
             failed: {:?}",
            expired_logins()
        );
        // And the file half of the same answer is now the live one, on the same poll.
        assert_eq!(
            signed_in_runtimes(),
            vec!["claude".to_string()],
            "the credential that cleared the banner is not reported as a login"
        );

        crate::ai::forget_refusal();
    }

    /// The host's copy of the fleet login is the last resort, so a husk must never reach it.
    ///
    /// `fleet-home` exists for one job: a sandbox rebuild destroys its HOME, and this is what puts
    /// the login back. The test was "the sandbox has a file", and a logged-out sandbox has a file —
    /// so a single logout overwrote the saved login and there was then nothing left to restore
    /// from. Same shape as the bug the launcher was already fixed for, one layer up and untested.
    #[test]
    fn a_logged_out_sandbox_cannot_destroy_the_fleets_kept_login() {
        let now = 1_700_000_000_000i64;
        let login = br#"{"claudeAiOauth":{"accessToken":"skein-test-sk-live","refreshToken":"r"}}"#;
        let husk = br#"{"claudeAiOauth":{"accessToken":"","refreshToken":""}}"#;

        // The regression, and the only case that loses data.
        assert_eq!(
            login_move(husk, Some(login), now),
            LoginMove::Restore,
            "a logged-out sandbox overwrote the host's saved login"
        );
        // A live sandbox is the live copy; the host follows it, including across a token refresh.
        assert_eq!(login_move(login, Some(husk), now), LoginMove::Save);
        assert_eq!(login_move(login, None, now), LoginMove::Save);
        // A freshly rebuilt sandbox: empty HOME, and the host has the answer.
        assert_eq!(login_move(b"", Some(login), now), LoginMove::Restore);
        // Nothing anywhere is normal on API keys, and writing a husk into a sandbox helps nobody.
        assert_eq!(login_move(b"", None, now), LoginMove::Neither);
        assert_eq!(login_move(husk, Some(husk), now), LoginMove::Neither);
        assert_eq!(login_move(husk, None, now), LoginMove::Neither);
    }

    /// The same hole one notch along: an *invalidated* sandbox login is not a login either.
    ///
    /// A husk was caught; an expired credential was not, because the test was [`carries_login`],
    /// which never looks at expiry. That was survivable while [`sync_fleet_login`] ran twice in a
    /// fleet's life. [`heal_logins`] now runs it every minute, and at that rate "a window where the
    /// sandbox's copy is dead and the host's is not" is simply Tuesday: the kept copy is destroyed
    /// and there is then nothing left to restore from — which is the exact sentence
    /// `a_logged_out_sandbox_cannot_destroy_the_fleets_kept_login` exists to prevent.
    #[test]
    fn an_invalidated_sandbox_login_cannot_destroy_the_fleets_kept_one_either() {
        let now = 1_700_000_000_000i64;
        let live = br#"{"claudeAiOauth":{"accessToken":"skein-test-sk-live","refreshToken":"r"}}"#;
        let dead = format!(
            r#"{{"claudeAiOauth":{{"accessToken":"skein-test-sk-old","refreshToken":"r","refreshTokenExpiresAt":{}}}}}"#,
            now - 1
        );
        let dead = dead.as_bytes();

        // The point of the whole test. Before this, `carries_login(dead)` was true and this Saved.
        assert_eq!(
            login_move(dead, Some(live), now),
            LoginMove::Restore,
            "an expired sandbox credential overwrote the host's working one"
        );
        // A dead token still beats a void: a heal can refresh one and cannot conjure the other, so
        // an empty kept copy is filled rather than left empty.
        assert_eq!(login_move(dead, None, now), LoginMove::Save);
        assert_eq!(login_move(b"", Some(dead), now), LoginMove::Restore);
        // Two corpses, and the sandbox's wins — the ordinary `carries_login` direction (SKEIN-349).
        //
        // This asserted `Neither` for a day, on the reasoning that "moving one over the other buys
        // nothing and costs the mtime". Both halves have since gone: the mtime cost is not a cost
        // any more, because a remembered refusal is judged against the credential's TOKEN and not
        // its mtime (`fleet::login_fingerprint`, SKEIN-348); and "buys nothing" was wrong on the
        // fleet, because with both copies expired this arm was the whole of the shared login and
        // it moved nothing at all.
        let other_dead = format!(
            r#"{{"claudeAiOauth":{{"accessToken":"skein-test-sk-other","refreshToken":"r","refreshTokenExpiresAt":{}}}}}"#,
            now - 2
        );
        assert_eq!(
            login_move(dead, Some(other_dead.as_bytes()), now),
            LoginMove::Save
        );
        // And a live sandbox still wins over a dead host copy, which is the leg a box-side login
        // travels once the heal script has carried it into the sandbox's HOME.
        assert_eq!(login_move(live, Some(dead), now), LoginMove::Save);
    }

    /// The whole journey a box-side `/login` has to walk, driven end to end.
    ///
    /// **This is SKEIN-294.** The two legs were each right and the second one did not run. Leg one
    /// is [`heal_logins_script`], which elects the best credential among the sandbox's `$HOME` and
    /// every box root and heals the dead ones — it already carried a box's login UP into the
    /// sandbox, every minute, on the host's authority rather than the box's. Leg two is
    /// [`sync_fleet_login`], which writes `fleet-home` — the ONLY file anything reporting a login
    /// reads ([`runtime_logins`] and everything built on it: `expired_logins`,
    /// [`signed_in_runtimes`], `skein doctor`, the cockpit's banner). Leg two was called from
    /// `ensure_fleet` and from a resize, and nowhere else. So a person could log in inside a box,
    /// watch leg one carry it up within the minute, and still be looking at a banner saying the
    /// fleet is signed out — correctly, because `fleet-home` still held the dead one. The work was
    /// done and thrown away.
    ///
    /// Driven by RUNNING the real script against a fixture and feeding its actual output bytes to
    /// the real decision, because the failure was precisely that two correct halves did not meet:
    /// asserting on either half alone is what let this survive.
    #[test]
    fn a_login_typed_inside_a_box_reaches_the_file_the_banner_reads() {
        // `SKEIN_FLEET_ROOT` is process-wide and three tests in this file set it to build a script.
        // Without this they interleave and each reads a fixture root belonging to another test —
        // which is how `a_shared_login_reaches_every_box_and_never_comes_back_up` began failing
        // when this test was added, having been latent since it was written.
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        let home = home.as_ref() as &std::path::Path;
        let boxes = crate::testutil::tempdir();
        let boxes = boxes.as_ref() as &std::path::Path;

        let now_ms = chrono::Utc::now().timestamp_millis();
        let live = format!(
            r#"{{"claudeAiOauth":{{"accessToken":"skein-test-sk-typed-in-a-box","refreshToken":"r","refreshTokenExpiresAt":{}}}}}"#,
            now_ms + 86_400_000
        );
        // The state the fleet is in at the exact moment somebody logs in inside a box: invalidated
        // everywhere. Not a husk — a husk was already caught. This is a real token that died.
        let dead = format!(
            r#"{{"claudeAiOauth":{{"accessToken":"skein-test-sk-invalidated","refreshToken":"r","refreshTokenExpiresAt":{}}}}}"#,
            now_ms - 1
        );

        let sandbox_home = home.join(".claude/.credentials.json");
        std::fs::create_dir_all(sandbox_home.parent().unwrap()).unwrap();
        std::fs::write(&sandbox_home, dead.as_bytes()).unwrap();
        for (name, cred) in [("web-main", &live), ("api-worker", &dead)] {
            let h = boxes.join(name).join("home/.claude");
            std::fs::create_dir_all(&h).unwrap();
            std::fs::write(h.join(".credentials.json"), cred.as_bytes()).unwrap();
        }

        std::env::set_var("SKEIN_FLEET_ROOT", boxes);
        let script = heal_logins_script(None);
        std::env::remove_var("SKEIN_FLEET_ROOT");
        let out = std::process::Command::new("bash")
            .arg("-c")
            .arg(&script)
            .env("HOME", home)
            .output()
            .expect("bash");
        assert!(out.status.success(), "the heal script failed: {out:?}");

        // Leg one: the box's login is now the sandbox's. This part already worked.
        let in_sandbox = std::fs::read(&sandbox_home).unwrap();
        assert!(
            String::from_utf8_lossy(&in_sandbox).contains("skein-test-sk-typed-in-a-box"),
            "the heal did not carry the box's login up into the sandbox's HOME"
        );

        // Leg two, and the whole point: the host's kept copy — the file the banner reads — takes
        // it. Before SKEIN-294 nothing called this between one `ensure_fleet` and the next.
        assert_eq!(
            login_move(&in_sandbox, Some(dead.as_bytes()), now_ms),
            LoginMove::Save,
            "the login reached the sandbox and stopped there; `fleet-home` kept the dead token, so \
             every surface that reports a login still says the fleet is signed out"
        );
        // And the tick that now runs both legs must be idle once they agree, or the mtime
        // `login_written_ms` reads would be reset every minute.
        assert_eq!(
            login_move(&in_sandbox, Some(&in_sandbox), now_ms),
            LoginMove::Neither
        );
    }

    /// **Propagation does not consult expiry, and this is the test whose absence let it** (SKEIN-349).
    ///
    /// The rule is written twice in this file — "propagation keeps asking `carries_login`" on
    /// [`LoginState`], and "nothing that seeds or heals reads this" on [`RuntimeLogin`] — and for a
    /// day `login_move` broke it anyway. The pairing tests above all passed, because every one of
    /// them asserts an OUTCOME for a pair of inputs and none of them asserted the INVARIANT. So a
    /// change that swapped the question ("is there a credential" for "does it still work") was
    /// invisible until the owner's shared login stopped moving fleet-wide.
    ///
    /// What it holds: an expired credential travels exactly as far as a live one. The direction may
    /// only differ where the guard applies — a live copy is never overwritten by a dead one.
    #[test]
    fn an_expired_credential_propagates_exactly_as_far_as_a_live_one() {
        let now = 1_700_000_000_000i64;
        let cred = |tag: &str, dies: i64| {
            format!(
                r#"{{"claudeAiOauth":{{"accessToken":"skein-test-sk-{tag}","refreshToken":"r","refreshTokenExpiresAt":{dies}}}}}"#
            )
        };
        let live = |tag: &str| cred(tag, now + 60_000);
        let dead = |tag: &str| cred(tag, now - 60_000);
        // The same three moves, once for a credential that still works and once for one that does
        // not. Identical expectations is the whole assertion.
        for dies in [now + 60_000, now - 60_000] {
            let a = cred("a", dies);
            let b = cred("b", dies);
            assert_eq!(
                login_move(a.as_bytes(), None, now),
                LoginMove::Save,
                "a credential in the sandbox reaches the host whether or not it has expired"
            );
            assert_eq!(
                login_move(b"", Some(a.as_bytes()), now),
                LoginMove::Restore,
                "and comes back the other way when the sandbox has none"
            );
            assert_eq!(
                login_move(a.as_bytes(), Some(b.as_bytes()), now),
                LoginMove::Save,
                "two of the same kind move the ordinary way: the sandbox is where a person logs in"
            );
        }
        // The guard, which is the ONLY thing expiry is allowed to decide.
        assert_eq!(
            login_move(dead("x").as_bytes(), Some(live("y").as_bytes()), now),
            LoginMove::Restore,
            "and a working host copy is never overwritten by a dead sandbox one"
        );
        // Not the other way round: a live sandbox credential still wins.
        assert_eq!(
            login_move(live("x").as_bytes(), Some(dead("y").as_bytes()), now),
            LoginMove::Save
        );
    }

    /// Rewriting the same bytes is not free, because something reads the mtime.
    ///
    /// `login_written_ms` treats `fleet-home`'s mtime as evidence that the credential was REPLACED
    /// since a refusal was recorded against it, and `crate::ai`'s refusal memory clears itself on
    /// that evidence. [`heal_logins`] runs [`sync_fleet_login`] every minute; if an unchanged file
    /// were rewritten each time, every remembered refusal would be cleared every minute and the
    /// memory would never hold — the mirror image of the memo that can never be cleared, and just
    /// as useless.
    #[test]
    fn an_unchanged_login_is_not_rewritten_every_minute() {
        let now = 1_700_000_000_000i64;
        let login = br#"{"claudeAiOauth":{"accessToken":"skein-test-sk-live","refreshToken":"r"}}"#;
        assert_eq!(
            login_move(login, Some(login), now),
            LoginMove::Neither,
            "the tick would rewrite an identical credential and reset the mtime a refusal is judged against"
        );
        // Including when there is nothing to write on either side.
        assert_eq!(login_move(b"", Some(b""), now), LoginMove::Neither);
    }

    /// In-fleet the login is a shell, not a hop through `sbx` to a sandbox that is not there.
    ///
    /// This half used to ride along on a test about the agent's socket address, and would have gone
    /// out with the agent (SKEIN-521) had it not been rehomed: it is about the login rather than the
    /// transport. `sbx` does not exist inside the sandbox to be run, so a login that still addressed
    /// one fails at exec — with the fleet's terminal as the surface that reports it.
    ///
    /// The neighbouring test RUNS this argv, which is stronger, but it returns early when a login
    /// shell rewrites PATH out from under the stub. So the shape is pinned here, where nothing can
    /// skip it.
    #[test]
    fn the_in_fleet_login_is_a_shell_rather_than_a_hop_to_a_sandbox() {
        let _g = crate::testutil::env_lock();
        let dir = crate::testutil::tempdir();
        let dir = dir.as_ref() as &std::path::Path;
        std::env::set_var("SKEIN_FLEET_ROOT", dir.join("fleet"));
        std::env::set_var("SKEIN_HOME", dir.join("home"));

        let (program, argv) = login_argv("skein-fleet", "claude");
        std::env::remove_var("SKEIN_FLEET_ROOT");
        std::env::remove_var("SKEIN_HOME");

        assert_eq!(
            program, "bash",
            "the in-fleet login runs a program the sandbox does not have"
        );
        assert_eq!(&argv[..1], ["-lc"]);
        assert!(
            !argv.iter().any(|a| a == "skein-fleet"),
            "the in-fleet login still addresses a sandbox: {argv:?}"
        );
    }

    /// The cockpit's login pane spawns exactly what `skein login` attaches to.
    ///
    /// [`login_spawn_argv`] is the server's entry point and [`fleet_login`] is the CLI's, and both
    /// are one line over [`login_argv`] — which is the whole reason the private builder exists.
    /// Nothing asserted they agree, because until SKEIN-774 `login_spawn_argv` returned a `Result`
    /// and the two shapes could not be compared without unwrapping a failure that could not happen.
    /// Now they can, so the claim in the doc comment is checked rather than stated: a divergence
    /// here is a cockpit login that runs a different command from the one a person is told to run.
    ///
    /// Both halves are read under one [`crate::testutil::env_lock`], because
    /// [`crate::place::fleet_sandbox`] resolves the config and an equality between two readings of
    /// a moving value proves nothing.
    #[test]
    fn the_cockpit_login_pane_spawns_exactly_what_skein_login_attaches_to() {
        let _g = crate::testutil::env_lock();
        let dir = crate::testutil::tempdir();
        let dir = dir.as_ref() as &std::path::Path;
        std::env::set_var("SKEIN_FLEET_ROOT", dir.join("fleet"));
        std::env::set_var("SKEIN_HOME", dir.join("home"));

        let sandbox = fleet_sandbox();
        let attached = login_argv(&sandbox, "codex");
        let spawned = login_spawn_argv("codex");

        std::env::remove_var("SKEIN_FLEET_ROOT");
        std::env::remove_var("SKEIN_HOME");

        assert_eq!(
            spawned, attached,
            "the cockpit login pane and `skein login` no longer run the same command"
        );
        assert!(
            spawned.1.iter().any(|a| a.contains("codex login")),
            "the runtime never reached the command the pane spawns: {spawned:?}"
        );
    }

    /// The login terminal brings its own scratch directory, and it is the same one the model call
    /// brings — asserted on the environment the login's child actually receives.
    ///
    /// This is the entry point that reported SKEIN-289: the OAuth flow completed, and the CLI then
    /// refused with `Temp directory /tmp/claude-1000 is owned by uid 0`. Both surfaces that log a
    /// fleet in — `skein login` and the cockpit's login terminal — come through `login_argv`, so
    /// this drives that argv rather than either caller.
    ///
    /// Run rather than matched: a substring check would pass on an export the shell never reached
    /// (after the `exec`, inside a false branch, quoted so it is one word). The stub stands in for
    /// `claude` and prints what it was handed.
    #[cfg(unix)]
    #[test]
    fn the_login_terminal_brings_the_same_scratch_directory_the_model_call_does() {
        use std::os::unix::fs::PermissionsExt;
        let _g = crate::testutil::env_lock();
        let dir = crate::testutil::tempdir();
        let dir = dir.as_ref() as &std::path::Path;
        let bin = dir.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let stub = bin.join("claude");
        std::fs::write(
            &stub,
            "#!/usr/bin/env bash\nprintf '%s' \"${CLAUDE_CODE_TMPDIR:-the shared /tmp}\"\n",
        )
        .unwrap();
        std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();

        // In-fleet, because that is the arm whose argv this machine can actually run: the host arm
        // is the same command behind an `sbx exec` hop, which the assertion below pins separately.
        let (program, argv) = login_argv("skein-fleet", "claude");

        let home = dir.join("sandbox-home");
        std::fs::create_dir_all(&home).unwrap();
        let path = format!(
            "{}:{}",
            bin.display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let out = std::process::Command::new(program)
            .args(&argv)
            .env("HOME", &home)
            .env("PATH", &path)
            .output()
            .expect("run the login command");
        let said = String::from_utf8_lossy(&out.stdout).trim().to_string();
        // A login shell on some machines rewrites PATH, and then this ran nothing at all. Said out
        // loud rather than passed over: a test that cannot run must not be silently green.
        if said.is_empty() {
            crate::testutil::skip(
                "this machine's login shell did not reach the stub on PATH, so the login \
                 environment was NOT exercised here",
            );
            return;
        }
        assert_eq!(
            said,
            model_scratch_dir(&home).display().to_string(),
            "the login started the runtime in the shared /tmp, where anything that got there first \
             makes it refuse — the failure the owner met with a login that had otherwise worked"
        );

        // And the host arm carries the identical command, so the hop is a hop and not a second
        // login with its own environment.
        let (_, host_argv) = login_argv("skein-fleet", "claude");
        assert_eq!(
            host_argv.last(),
            argv.last(),
            "the login through `sbx exec` runs a different command from the in-fleet one"
        );
    }

    /// `after_login` exists so the post-login tail runs in the process whose state it fixes. Run
    /// from the CLI it cleared the CLI's refusal memory while the server kept declining model
    /// calls over a credential that had just been renewed — the gap the login route closes. The
    /// refusal half is observable through `ai`'s test probes; the share half is pinned on the one
    /// arm a unit test reaches deterministically (a config that names no sandbox), because every
    /// other arm needs a sandbox to actually share into.
    #[test]
    fn after_login_clears_this_processes_refusal_and_says_what_the_share_did() {
        // env_lock guards the refusal memory as well as the env: `REFUSED` is process-global, and
        // every test that manufactures refusals serializes on this lock (see ai's tests).
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        std::env::set_var("SKEIN_HOME", &home);
        // A fleet root of its own. The seam below means the share script never runs at all, so
        // this is the second lock on the same door rather than the one that matters — and it is
        // here because of what is behind the door: `share_login_script` globs `<fleet root>/*/`
        // and MERGES this process's credentials into every box's `home/.claude/`. Run against the
        // default `/boxes` that is the owner's live fleet, which is what happened for as long as
        // this test rested on a neighbour (SKEIN-646).
        let fleet = crate::testutil::tempdir();
        std::env::set_var("SKEIN_FLEET_ROOT", &fleet);
        // A sandbox that cannot exist.
        //
        // This used to be the whole reason the share failed, and it stopped being one: skein runs
        // *inside* the fleet sandbox now (SKEIN-576), so `own_sandbox(name).exec` no longer hops
        // anywhere and the name it is given is never looked up. What made the share fail after that
        // was an accident in another test — a neighbour that leaves `$HOME` unset, which makes the
        // share script's `set -u` abort. Alone, with a real `$HOME`, this test ran the live share
        // against the live fleet and asserted the wrong arm.
        //
        // The config still names an impossible sandbox, because `fleet_sandbox()` must answer
        // something and a test that let it answer `skein-fleet` would be naming this machine's real
        // fleet in a string it hands to a shell.
        std::fs::write(
            crate::config::config_json(),
            r#"{"fleet_sandbox": "skein-no-such-sandbox-for-tests"}"#,
        )
        .unwrap();
        // What actually makes the share fail, said by the test rather than inherited: the one seam
        // a fleet-scope command passes through, standing in with a command that refuses. The timeout
        // and the exit-code handling stay production's, so the `Err` arm is reached the way a real
        // failure reaches it.
        let _stood_in = crate::place::seam::install(Box::new(|_argv: &[String]| {
            Some(vec![
                "sh".to_string(),
                "-c".into(),
                "echo 'no sandbox to share into' >&2; exit 1".into(),
            ])
        }));

        crate::ai::plant_refusal_for_test();
        assert!(
            crate::ai::refusal_standing_for_test(),
            "the probe must plant a refusal for this test to mean anything"
        );
        let said = after_login("claude");
        assert!(
            !crate::ai::refusal_standing_for_test(),
            "a fresh login must clear the standing refusal in THIS process — that is after_login's \
             reason to exist"
        );
        assert_eq!(said.len(), 1, "one outcome sentence, got: {said:?}");
        // The failure's own words belong to whatever refused — a missing sandbox, a timeout, a
        // transport that could not start — and pinning them here would make this a test of that
        // message rather than of the wiring. What must hold is the SHAPE `share_outcome` promises:
        // the login worked, the running boxes did not get it, and here is what happens next.
        assert!(
            said[0].contains("logged in, but could not hand it to the boxes already running")
                && said[0].contains("session next starts"),
            "the share outcome must say why it could not hand the login over and what happens \
             instead: {}",
            said[0]
        );

        crate::ai::forget_refusal(); // leave the shared static as found
        std::env::remove_var("SKEIN_FLEET_ROOT");
        std::env::remove_var("SKEIN_HOME");
    }

    /// Every arm of the share sentence, without a sandbox to share into. The wording is the
    /// contract: the CLI prints these lines and the login socket sends them, so a drifted sentence
    /// drifts on two surfaces at once.
    #[test]
    fn the_share_outcome_has_a_sentence_for_each_arm() {
        let none = share_outcome("claude", Ok(vec![]));
        assert!(
            none.contains("no existing boxes"),
            "an empty fleet must be told it lost nothing: {none}"
        );
        let some = share_outcome("claude", Ok(vec!["alpha".into(), "beta".into()]));
        assert!(
            some.contains("2 existing box(es)") && some.contains("alpha, beta"),
            "the boxes that now hold the login must be named: {some}"
        );
        let err = share_outcome("claude", Err("sandbox down".into()));
        assert!(
            err.contains("sandbox down") && err.contains("session next starts"),
            "a failed share must carry the why and the recovery: {err}"
        );
    }

    /// **A login skein could not read is not a login that is absent** (SKEIN-347).
    ///
    /// The capture asked the sandbox for the bytes with `.unwrap_or_default()`, so an exec that
    /// timed out, or a sandbox that was not answering, produced an empty `Vec<u8>` — and
    /// `login_move` judged that emptiness as "the sandbox carries no credential". From there
    /// `(absent here, present there)` is `Restore`, which pushes the host's copy back DOWN over a
    /// live login the read simply failed to see. At a resize the same emptiness means nothing is
    /// saved before the HOME holding it is destroyed, which is the loss the comment at that call
    /// site already records having happened once.
    ///
    /// Driven against a sandbox that does not exist, because that is a read that genuinely fails
    /// rather than one that answers "no file" — and the two used to be the same value.
    #[test]
    fn a_login_read_that_failed_moves_nothing_and_says_so() {
        let _g = env_lock();
        let home = tempdir();
        std::env::set_var("SKEIN_HOME", &home);
        // Pinned because this reaches a `Place`. It used to be safe by a SECOND condition rather
        // than by design — the agent client needed an address AND a token, and an empty temp
        // `$SKEIN_HOME` gave it no token, so the call fell to the fake `sbx` below. That was one
        // minted token away from not being true: the five tests
        // that reached this machine's live fleet on 2026-09-05 were exactly the ones that minted a
        // token into their own `$SKEIN_HOME` first (SKEIN-530).
        std::env::set_var("SKEIN_FLEET_ROOT", home.join("fleet"));

        let kept = fleet_home_dir().join(LOGIN_FILES[0]);
        std::fs::create_dir_all(kept.parent().unwrap()).unwrap();
        let live = br#"{"claudeAiOauth":{"accessToken":"skein-test-sk-live","refreshToken":"r"}}"#;
        std::fs::write(&kept, live).unwrap();

        let refused = capture_fleet_login("skein-no-such-sandbox-for-a-test")
            .expect_err("a capture that could not read anything reported success");
        assert!(
            refused.contains(LOGIN_FILES[0]),
            "the failure has to name the login file it could not read: {refused}"
        );
        assert_eq!(
            std::fs::read(&kept).unwrap(),
            live,
            "a failed read was treated as an absent login and moved the kept copy"
        );

        std::env::remove_var("SKEIN_HOME");
        std::env::remove_var("SKEIN_FLEET_ROOT");
    }
}
