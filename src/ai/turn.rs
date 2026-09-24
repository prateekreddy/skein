//! Which machine a turn runs on and which conversation it belongs to, derived — never stored —
//! with the hash that conversation's id is made from.

use super::*;

/// **Which machine a turn runs on**, and therefore where its conversation is filed.
///
/// A third fact beside [`Turn`]'s id and directory, and it travels for the same reason those two
/// do: a conversation opened in one place can only be resumed in that place. Claude Code keys
/// sessions on the working directory, and a box has its own — its own `$HOME`, its own
/// `~/.claude/projects`, its own filesystem. So a round that opened in a review box and a round
/// that resumes in the sandbox are not two rounds of one conversation; they are two cold reads,
/// and the feature looks like it works while doing nothing at all. That failure has happened once
/// already (SKEIN-376) with only the directory unpinned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Machine<'a> {
    /// Wherever skein's model calls already go, which is **this process**: skein runs inside the
    /// fleet sandbox, and that is where `skein login` put the credential (SKEIN-576). It used to be
    /// a choice — a host shipped the call in through `sbx exec` — and the shipping went with the
    /// host, so this is now the name for "no box was opened for this reading".
    Wherever,
    /// **This pull request's own review box** — `docs/pr-review.md` §11. Reached through its
    /// placement record, standing in a checkout of the commit under review.
    ///
    /// A failure here is not a reason to give up on the reading: the caller falls back to
    /// [`Machine::Wherever`], which is what every reading did before review boxes existed.
    Box(&'a str),
}

/// Which conversation a model call belongs to.
///
/// **Every call skein made before this was [`Turn::Alone`]** — a fresh context, thrown away, paying
/// to be told the diff again on every question about it. That is the right shape for a one-shot
/// classification and the wrong one for a review, which is a conversation: the reader asks a second
/// thing about the change the model just read, and there is no reason to buy the reading twice.
///
/// The CLI supplies both halves and skein CHOOSES the id, which is the part that matters — there is
/// no id to discover, store or keep in sync, so a caller that can name its conversation can resume
/// it. Verified against the real CLI (2026-08-26): `--session-id` on an id that already exists
/// fails with an empty stdout and exit 1, so a collision arrives through [`Unread::Refused`] rather
/// than as an error message parsed as an answer.
///
/// **A conversation carries the directory it is filed under, because it is not findable without
/// it** (SKEIN-376). Claude Code stores sessions under `~/.claude/projects/<slugified-cwd>/`, so
/// `--resume` only finds what a call in the SAME working directory created. Measured against the
/// installed CLI (2026-08-26): a session opened in one directory and resumed from another answers
/// `No conversation found with session ID: <id>` and exits 1, and the same resume from the
/// directory that opened it answers from memory. The id and the directory are therefore one fact,
/// and they travel together so that no caller can pin one and forget the other — unpinned, every
/// resume misses, every round is a cold read, and the feature looks like it works while doing
/// nothing at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Turn<'a> {
    /// No conversation. The context dies with the call, and the directory does not matter.
    Alone,
    /// The first turn of one, under an id skein picked, in the directory it will be found in.
    Opening { id: &'a str, at: &'a Path },
    /// A later turn of one. The model still has what it was shown; do not send it again.
    Resuming { id: &'a str, at: &'a Path },
    /// No conversation, like [`Turn::Alone`], but under an id skein chose so the transcript it
    /// leaves says which call site it was ([`labelled_session`], SKEIN-1074). The id is fresh for
    /// every call, so there is nothing to find and the directory does not matter.
    Labelled { id: &'a str },
}

impl<'a> Turn<'a> {
    /// The flags this turn adds to the command line, in order.
    pub(crate) fn args(&self) -> Vec<&'a str> {
        match self {
            Turn::Alone => Vec::new(),
            Turn::Opening { id, .. } | Turn::Labelled { id } => vec!["--session-id", id],
            Turn::Resuming { id, .. } => vec!["--resume", id],
        }
    }

    /// Where the call must run for this conversation to be found. `None` only for [`Turn::Alone`]
    /// and [`Turn::Labelled`], which have nothing to find.
    pub(crate) fn at(&self) -> Option<&'a Path> {
        match self {
            Turn::Alone | Turn::Labelled { .. } => None,
            Turn::Opening { at, .. } | Turn::Resuming { at, .. } => Some(at),
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
    #[rustfmt::skip]
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
        0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
        0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
        0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
        0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
        0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
        0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
    ];
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];
    let mut data = msg.to_vec();
    let bits = (msg.len() as u64).wrapping_mul(8);
    data.push(0x80);
    while data.len() % 64 != 56 {
        data.push(0);
    }
    data.extend_from_slice(&bits.to_be_bytes());
    for block in data.chunks(64) {
        let mut w = [0u32; 64];
        for (i, word) in w.iter_mut().take(16).enumerate() {
            *word = u32::from_be_bytes([
                block[i * 4],
                block[i * 4 + 1],
                block[i * 4 + 2],
                block[i * 4 + 3],
            ]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let (mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut z) =
            (h[0], h[1], h[2], h[3], h[4], h[5], h[6], h[7]);
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let t1 = z
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);
            z = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        for (slot, add) in h.iter_mut().zip([a, b, c, d, e, f, g, z]) {
            *slot = slot.wrapping_add(add);
        }
    }
    let mut out = [0u8; 32];
    for (i, word) in h.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&word.to_be_bytes());
    }
    out
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
