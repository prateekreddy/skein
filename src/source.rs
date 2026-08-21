//! Source — how a subject is reached (architecture §2.3).
//!
//! A Source is a way of observing and acting on something. Four of them, and the point of naming
//! them is a law that can be checked: **no code reaches anything except through a Source.**
//!
//! **Why this is a primitive at all**, because the first attempt got it wrong twice over. An earlier
//! draft promoted `enter` to the primitive — and the cheapest, most-used signal in the whole system
//! reaches a box **by socket, without nsenter**, so "nothing reaches a box except through `enter`"
//! was false on day one. The same draft left `github` outside the primitive set entirely, which is a
//! world-facing transport carrying credentials.
//!
//! **Not a 4×3 matrix.** Each Source carries only the modes it can support. `http`×pty and
//! `file`×pty are meaningless, and asserting a clean product is an invitation to implement the empty
//! cells — so the empty cells are refused here rather than left to a reviewer to notice.
//!
//! **There is a second `Source` in this crate** and it is a different thing: [`crate::answer::Source`]
//! is *which copy of a fact* was read — the box, the store it wrote at its last turn end, or the
//! host's own checkout. This one is *how* something was reached. Neither determines the other:
//! `Store` and `Host` are both reached by `file`, and `Box` is reached by `enter` or by `socket`
//! depending on what was asked. §2.2 says a signal carries "which Source produced it (§2.3)" — that
//! is this type, and today's `Answer` does not carry it.
//!
//! **Never privileged.** Reaching a subject is precisely the thing that must not require privilege,
//! which is why this module depends on nothing: a Source that had to ask `config` where something
//! lived, or `fleet` who owned it, would be a Source that could be denied.

use std::fmt;

/// What a Source can do to the thing it reaches.
///
/// `stream` exists because "execute" does not cover what the work needs: stdin or stdout as bytes,
/// up to gigabytes, never buffered whole. A file read is a stream and **must be bytes, not text** —
/// a lossy UTF-8 conversion silently corrupts every image and every PDF that passes through.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Run something and collect what it said.
    Exec,
    /// Bytes in or bytes out, unbounded, never held whole.
    Stream,
    /// An interactive terminal, with a size and a lifetime.
    Pty,
}

impl Mode {
    pub const ALL: [Mode; 3] = [Mode::Exec, Mode::Stream, Mode::Pty];

    pub fn name(self) -> &'static str {
        match self {
            Mode::Exec => "exec",
            Mode::Stream => "stream",
            Mode::Pty => "pty",
        }
    }
}

impl fmt::Display for Mode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// The four ways anything is reached.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// A box's namespace, via `nsenter`.
    ///
    /// Two details are load-bearing and both were learned from a real box rather than from a manual
    /// page: the **user and mount namespaces must be joined together** — joining mount alone is
    /// refused — and **credentials must be preserved**, or `setgroups` fails for an unprivileged
    /// caller.
    ///
    /// A third is the reason this is an `exec` and not a library call: joining a user namespace is
    /// refused for a **multithreaded** caller, so an in-process `setns` from the threaded server is
    /// impossible. The extra process is mandatory, not a cost someone could optimise away.
    Enter,
    /// A box's tmux server, without entering it.
    ///
    /// The cheapest and most-used reach in the system, and the one that makes `enter` insufficient
    /// as the primitive. It is still a crossing: the socket is `0700` and owned by the box.
    Socket,
    /// The volume, and paths visible in the fleet.
    File,
    /// GitHub, and the warden.
    Http,
}

impl Source {
    pub const ALL: [Source; 4] = [Source::Enter, Source::Socket, Source::File, Source::Http];

    pub fn name(self) -> &'static str {
        match self {
            Source::Enter => "enter",
            Source::Socket => "socket",
            Source::File => "file",
            Source::Http => "http",
        }
    }

    /// What this Source reaches — the second column of §2.3's first table, kept here so the code and
    /// the design can be compared rather than believed.
    pub fn reaches(self) -> &'static str {
        match self {
            Source::Enter => "a box's namespace, via nsenter",
            Source::Socket => "a box's tmux server, without entering",
            Source::File => "the volume, and paths visible in the fleet",
            Source::Http => "GitHub, and the warden",
        }
    }

    /// The modes this Source carries. §2.3's second table, and the empty cells are absences here
    /// rather than falsehoods.
    pub fn modes(self) -> &'static [Mode] {
        match self {
            Source::Enter => &[Mode::Exec, Mode::Stream, Mode::Pty],
            Source::Socket => &[Mode::Exec, Mode::Pty],
            Source::File => &[Mode::Stream],
            Source::Http => &[Mode::Exec, Mode::Stream],
        }
    }

    pub fn supports(self, mode: Mode) -> bool {
        self.modes().contains(&mode)
    }
}

impl fmt::Display for Source {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// One reach: a Source and a mode it actually carries.
///
/// The constructor is the gate. A pair that is not in the table cannot be built, so "implement the
/// empty cell" is a change to [`Source::modes`] — one line, in the file that documents why the cell
/// is empty — rather than a call somewhere that nobody compares against the design.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Reach {
    source: Source,
    mode: Mode,
}

impl Reach {
    pub fn new(source: Source, mode: Mode) -> Result<Reach, String> {
        if !source.supports(mode) {
            return Err(format!(
                "{source} cannot {mode}: it carries {}. Adding it means changing what a {source} \
                 Source is, in `source.rs`, where the reason it does not is written down.",
                source
                    .modes()
                    .iter()
                    .map(|m| m.name())
                    .collect::<Vec<_>>()
                    .join(" and ")
            ));
        }
        Ok(Reach { source, mode })
    }

    pub fn source(&self) -> Source {
        self.source
    }

    pub fn mode(&self) -> Mode {
        self.mode
    }
}

impl fmt::Display for Reach {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}·{}", self.source, self.mode)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The table here is the table in `docs/architecture.md` §2.3 — checked, not asserted.
    ///
    /// Two tables that mean the same thing, in two files, is exactly the shape that drifts. The
    /// design is the one people read and the code is the one that runs, so the test reads the design
    /// and compares. It fails in both directions: a cell in the document this code does not carry,
    /// and a cell in this code the document does not name.
    #[test]
    fn the_modes_here_are_the_modes_the_architecture_names() {
        let doc = include_str!("../docs/architecture.md");
        let table = doc
            .split_once("### 2.3 Source")
            .expect("§2.3 is where the Source table lives")
            .1;
        // The mode table is the one whose header names the modes; take the rows under it.
        let header = table
            .find("| | exec | stream | pty |")
            .expect("§2.3's mode table moved or was reworded");
        let rows: Vec<&str> = table[header..]
            .lines()
            .skip(2) // the header and its separator
            .take_while(|l| l.starts_with('|'))
            .collect();
        assert_eq!(rows.len(), Source::ALL.len(), "rows: {rows:?}");

        let mut seen = Vec::new();
        for row in rows {
            let cells: Vec<&str> = row.trim_matches('|').split('|').map(str::trim).collect();
            let name = cells[0].trim_matches('`');
            let source = Source::ALL
                .into_iter()
                .find(|s| s.name() == name)
                .unwrap_or_else(|| {
                    panic!("the design names a Source this code does not have: {name}")
                });
            seen.push(source);
            for (mode, cell) in Mode::ALL.into_iter().zip(&cells[1..]) {
                assert_eq!(
                    source.supports(mode),
                    *cell == "✓",
                    "{source}·{mode}: the design says {cell:?} and this code says {}",
                    source.supports(mode)
                );
            }
        }
        for source in Source::ALL {
            assert!(
                seen.contains(&source),
                "{source} is not in the design's table"
            );
        }
    }

    /// An empty cell cannot be reached by accident.
    #[test]
    fn a_source_refuses_a_mode_it_does_not_carry() {
        let why = Reach::new(Source::Http, Mode::Pty).unwrap_err();
        assert!(why.contains("http cannot pty"), "{why}");
        assert!(
            why.contains("source.rs"),
            "the refusal must say where the decision lives: {why}"
        );
        assert!(Reach::new(Source::File, Mode::Pty).is_err());
        assert!(Reach::new(Source::File, Mode::Exec).is_err());
        assert!(Reach::new(Source::Socket, Mode::Stream).is_err());

        // And every cell the design does carry is buildable.
        for source in Source::ALL {
            for mode in source.modes() {
                Reach::new(source, *mode).unwrap();
            }
        }
    }
}
