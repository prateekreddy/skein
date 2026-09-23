//! The GitHub credential a box is handed, where it is placed and forgotten, and the review
//! token's lifetime.

use super::*;

/// The credential a sandboxed model call may act with: the shell that picks it up, and the file to
/// take away when the call is over.
///
/// Two fields rather than a bare string because the second one is the whole of ISO-2's fix. A
/// caller that is handed only the export line has no way to say "and now forget it", and that is
/// how the owner's own GitHub token came to sit in a directory every box could read, for the life
/// of a fleet.
pub(super) struct GithubCredential {
    /// The line to put at the top of the call's script. Empty when there is no credential, which is
    /// the ordinary case for a fleet that has never been given one.
    pub(super) export: String,
    /// What [`forget_review_token`] unlinks, **as shell** — see [`box_credential_paths`] for why a
    /// path is not enough. `None` when nothing was written.
    path: Option<String>,
}

/// Where a model call's GitHub credential is written, and it is not one place.
///
/// **The script runs on the far side of the crossing.** `place::Place::crossing` puts the whole of
/// what skein sends after `exec nsenter`, so a call into a box evaluates its own `$(cat …)` inside
/// that box's mount namespace. A credential under [`fleet_private_dir`] is therefore exactly the
/// wrong place for a box call once the launcher covers that directory: the read would fail, the
/// export would be empty, and the reading would go ahead with no GitHub access and say nothing —
/// the same silence a failed write already produces.
///
/// The destination followed the reader, and there was a choice to make while a model call could
/// run at **sandbox** scope — under the cover, which is what ISO-2 asks for, with nothing crossing
/// a namespace. That call is gone (SKEIN-576): skein is inside the sandbox, so the only crossing
/// left is into a box, and this is where a box's copy goes.
///
/// **Inside that box's own private HOME**, which is bound from its own root and which no other box
/// can see (`--tmpfs /boxes` removes every sibling). It is written and removed through the box's
/// own placement, so one path is right at both ends. No *other* box can read it and it does not
/// outlive the call, which is the pair of properties ISO-2 is about. What it gives up is protection
/// from the box that is *using* the credential — and there is none to give up: the token is in that
/// call's environment by construction.
///
/// Returns the directory to make and the file to write, both already quoted as shell. `$HOME` is
/// expanded by the box's own shell, beside the scratch directory [`model_scratch_export`] already
/// puts there; `call` is skein's own digits and dash, so there is nothing in it a shell could read
/// as anything else.
pub(super) fn box_credential_paths(call: &str) -> (String, String) {
    (
        "\"$HOME\"/.cache/skein".to_string(),
        format!("\"$HOME\"/.cache/skein/review-{call}.token"),
    )
}

/// The shell that puts the credential on disk, reading it from stdin.
///
/// Its own function for [`crate::place::Place::exec_argv`]'s reason: it is a wire format, and a
/// wire format that can only be seen by running a sandbox is one nothing can pin. Here that is not
/// theoretical — the property it has to have is about the *mode the file is created with*, which is
/// invisible in every other observation of this code.
///
/// `umask` and not a `chmod` afterwards. The two end at the same mode and differ in the window
/// between: `cat > f && chmod 600 f` leaves the credential at the process umask for the whole of
/// the write, which on a sandbox that has never set one is world-readable.
fn place_credential_script(dir: &str, file: &str) -> String {
    format!("mkdir -p {dir} && chmod 700 {dir} && (umask 077; cat > {file})")
}

/// The shell that takes it away again. `-f`, so a file already gone is success.
pub(super) fn forget_credential_script(file: &str) -> String {
    format!("rm -f {file}")
}

/// Put the GitHub credential where a model call can pick it up, and return the line that picks it
/// up — **or nothing at all**.
///
/// Asked and answered rather than assumed: the session gets the token, and it travels as a file
/// rather than as an argument or an inherited env var. The rule is already written down one screen
/// up, for the fleet agent's own token: "an argument would put the secret in `ps` on the host and
/// in the shell history of anything that logged the call". A model call runs for minutes, so an
/// argument would sit in the sandbox's process list for all of them.
///
/// Written on every call rather than once, deliberately: a rotated token then takes effect on the
/// next reading instead of at the next restart, and the write is one round trip on a path that is
/// about to spend a model call worth dollars.
///
/// **`umask` rather than `chmod` after the fact.** This used to `cat > … && chmod 600 …`, which
/// leaves the credential readable at the process umask for as long as the write takes;
/// [`crate::secret::write`] makes the same point about the nine hand-rolled writers it replaced.
/// That function cannot be the one used here — it writes to a path on the filesystem *this* process
/// is standing on, and this write crosses into a box — so it is the rule that is shared and not the
/// code, spelled in the language the write is actually made in. (SKEIN-536 asked for this write to
/// go through `secret::write`; it cannot without moving the file out of the box's namespace, which
/// is the ISO-2 regression [`box_credential_paths`] describes.)
///
/// **Takes the [`crate::secret::Secret`], and is the one place on the model-call path that exposes
/// it** — at the moment its bytes go on the crossing's stdin, and not before.
///
/// **Best-effort, and silent about it.** A write that fails returns no export line, so the call goes
/// ahead with a session that cannot reach GitHub — which is what every reading did before this
/// existed. It must never be the reason a pull request goes unread.
pub(super) fn github_export(
    at: &Place,
    github: Option<&crate::secret::Secret>,
) -> GithubCredential {
    let nothing = GithubCredential {
        export: String::new(),
        path: None,
    };
    let Some(token) = github.map(|t| t.expose()).filter(|t| !t.trim().is_empty()) else {
        return nothing;
    };
    let (dir, file) = box_credential_paths(&review_call_id());
    if at
        .write(
            &place_credential_script(&dir, &file),
            token.trim().as_bytes(),
            Duration::from_secs(30),
        )
        .is_err()
    {
        return nothing;
    }
    GithubCredential {
        export: format!("export GH_TOKEN=\"$(cat {file})\" GITHUB_TOKEN=\"$(cat {file})\"\n"),
        path: Some(file),
    }
}

/// Take the review credential away again, the moment the call it was written for has returned.
///
/// **The unlink is the fix, not tidying.** A credential left behind at a well-known path is ISO-2
/// itself: it is readable for as long as it sits there, by anything that can see the directory, and
/// it is the owner's own GitHub identity. The cover ([`fleet_private_dir`]) and the unlink answer
/// two different halves — the cover says who can look, the unlink says for how long there is
/// anything to look at — and neither makes the other unnecessary.
///
/// Best-effort like the write: a fleet that has stopped answering will not delete a file, and
/// failing the reading over it would trade a credential that outlives its call for a pull request
/// nobody read. The shell is `rm -f`, so a file already gone is success.
pub(super) fn forget_review_token(at: &Place, credential: &GithubCredential) {
    let Some(file) = credential.path.as_deref() else {
        return;
    };
    let _ = at.exec(&forget_credential_script(file), Duration::from_secs(30));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The credential is private from its first byte, and it does not outlive the call.
    ///
    /// **Driven by running the shell**, because both properties are invisible in every other
    /// observation of this code: the mode a file is *created* with cannot be read off the source,
    /// and neither can whether anything removes it. The umask here is deliberately wide open —
    /// 000 is what a sandbox that has never set one gives — so a write that leans on a `chmod`
    /// afterwards is caught in the window it leaves.
    #[test]
    fn a_review_credential_is_private_from_its_first_byte_and_gone_when_the_call_returns() {
        use std::io::Write;
        use std::os::unix::fs::PermissionsExt;
        let home = crate::testutil::tempdir();
        let home = home.as_ref() as &std::path::Path;
        let (dir, file) = box_credential_paths("31337-0");
        let at = home.join(".cache/skein/review-31337-0.token");

        let run = |script: &str, body: &str| {
            let mut child = std::process::Command::new("bash")
                .arg("-c")
                .arg(format!("umask 000; {script}"))
                .env("HOME", home)
                .stdin(std::process::Stdio::piped())
                .spawn()
                .expect("bash");
            child
                .stdin
                .take()
                .unwrap()
                .write_all(body.as_bytes())
                .unwrap();
            assert!(child.wait().expect("bash").success());
        };

        run(&place_credential_script(&dir, &file), "skein-test-gh-token");
        assert_eq!(
            std::fs::read_to_string(&at).expect("the credential was not written where it is read"),
            "skein-test-gh-token"
        );
        assert_eq!(
            std::fs::metadata(&at).unwrap().permissions().mode() & 0o777,
            0o600,
            "the credential was readable by somebody else for the length of the write"
        );
        assert_eq!(
            std::fs::metadata(home.join(".cache/skein"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700,
            "the directory it sits in was left at the ambient umask"
        );

        run(&forget_credential_script(&file), "");
        assert!(
            !at.exists(),
            "the owner's GitHub token outlived the call it was written for"
        );
        // Twice, because the unlink runs whatever the call answered and a second stop must not be
        // an error — a fleet where the first `rm` already ran is the ordinary case, not a fault.
        run(&forget_credential_script(&file), "");
    }
}
