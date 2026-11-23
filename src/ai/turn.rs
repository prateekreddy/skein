//! Which session a model call is filed under, and the hash a test's session id is made from.

/// Which session a model call is filed under.
///
/// [`Turn::Alone`] is a fresh context, thrown away. [`Turn::Labelled`] is the same call with a
/// session id skein chose, so the usage reader can say which of skein's questions it was.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Turn<'a> {
    /// No session of its own.
    Alone,
    /// A fresh session under this id ([`labelled_session`]).
    Labelled { id: &'a str },
}

impl<'a> Turn<'a> {
    /// The CLI flags that put this call in its session.
    pub(crate) fn args(&self) -> Vec<&'a str> {
        match self {
            Turn::Alone => Vec::new(),
            Turn::Labelled { id } => vec!["--session-id", id],
        }
    }
}

/// The conversation a pull request's readings belong to — **derived, never stored** (SKEIN-376).
///
/// `<repo_id>#<number>` hashed into a uuid, so the same pull request produces the same id on every
/// round of every process, on any machine, with no mapping file to write, garbage-collect, or let
/// drift from the thing it names. A stored id can point at the wrong pull request; a derived one
/// cannot be wrong without the pull request itself being different.
///
/// It is keyed on the REPO as well as the number even though [`Turn`] already pins a per-repo
/// directory, and the redundancy is deliberate: two pull requests sharing a conversation is a
/// review answering about the wrong change, and that must not become possible the day somebody
/// changes where the call runs.
///
/// Version nibble 8 — RFC 9562's "custom" — because that is what this is: an id whose bits come
/// from the name rather than from a random source, and saying so costs nothing.
#[cfg(test)]
pub(crate) fn conversation_for(repo_id: &str, number: u64) -> String {
    let d = sha256(format!("{repo_id}#{number}").as_bytes());
    let mut b = [0u8; 16];
    b.copy_from_slice(&d[..16]);
    b[6] = (b[6] & 0x0f) | 0x80;
    b[8] = (b[8] & 0x3f) | 0x80;
    let hex: String = b.iter().map(|x| format!("{x:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}

/// SHA-256, hand-rolled for the same reason [`crate::apiauth::token`] is: this is the whole of what
/// a dependency would be used for, and it is a closed algorithm with a published answer.
///
/// **Proven against `sha256sum` rather than against itself.** An implementation can agree with its
/// own expectations and disagree with the world — `tracking.rs` says the same thing about the same
/// hash for the same reason — so the test that guards this shells out and compares.
pub(super) fn sha256(msg: &[u8]) -> [u8; 32] {
    let mut hasher = crate::util::Sha256::new();
    hasher.update(msg);
    hasher.finish()
}

#[cfg(test)]
mod tests {

    /// SHA-256, checked against `sha256sum` rather than against itself — an implementation can
    /// agree with its own expectations and disagree with the world, and this one decides which
    /// conversation a pull request gets.
    #[test]
    fn the_hash_the_conversation_id_is_derived_from_agrees_with_sha256sum() {
        use std::io::Write;
        for subject in [
            "",
            "acme#41",
            "a much longer subject than one block of sixty-four bytes, \
                         so the padding and the second block are both exercised here",
        ] {
            let mut child = std::process::Command::new("sha256sum")
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .spawn()
                .expect("sha256sum is on this machine");
            child
                .stdin
                .take()
                .unwrap()
                .write_all(subject.as_bytes())
                .unwrap();
            let out = child.wait_with_output().unwrap();
            let want = String::from_utf8_lossy(&out.stdout)
                .split_whitespace()
                .next()
                .unwrap()
                .to_string();
            assert_eq!(
                super::sha256(subject.as_bytes())
                    .iter()
                    .map(|b| format!("{b:02x}"))
                    .collect::<String>(),
                want,
                "skein's hash disagrees with sha256sum on {subject:?}, so the conversation id it \
                 derives is not the one it says it is"
            );
        }
    }
}
