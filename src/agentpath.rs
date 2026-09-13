//! **The earliest file named `claude` on a box's PATH is the one that runs** — asked, not assumed.
//!
//! # The class this catches, and why one sentence covers all of it
//!
//! A box resolves its agent off its own PATH, and that PATH is built by [`crate::place::box_path`]:
//! the box's `~/.local/bin`, then `/usr/local/share/npm-global/bin`, then the root-owned six. The
//! first two are writable by the uid every box runs as, so anything can end up in front of the
//! agent the substrate image ships — an interrupted `npm install -g` that wrote the wrapper and
//! never fetched the 215 MB native binary, a shadowing stub, an upgrade that got half way. Naming
//! those cases one at a time is a list that goes stale; the invariant above is one sentence and it
//! is false in every one of them.
//!
//! # Why a shell cannot be the one to ask (SKEIN-870)
//!
//! `command -v claude` answers the first **executable** hit, and that is a different question. On
//! this fleet the shadowing file was mode `-rw-r--r--` — 500 bytes whose entire content is the
//! package's own "native binary not installed" failure stub — so every shell skipped it and
//! answered with the image's copy two entries further down. Every surface therefore reported a
//! healthy agent, and the only thing separating the fleet from a `claude` that prints four lines to
//! stderr and exits 1 on every box at once was a missing `+x` bit.
//!
//! So this walks the PATH entries itself and looks for the file, executable or not, and the file it
//! finds first is the one it asks about. A file's SIZE is not the test and neither is its content:
//! both are proxies for "is this the agent", and the test for that is the agent answering
//! `--version`. Existence is emphatically not the test either — existence is exactly what was true
//! of the stub, and it is what made the fault invisible.
//!
//! # A mention is not an instance
//!
//! Nothing here matches the *word*. [`listing_script`] asks the filesystem about `<entry>/claude`
//! for each PATH entry, and [`parse_listing`] accepts only lines in the shape that script emits —
//! so a directory called `claude-tools`, a script with the word in it, or a shell's own error text
//! contributes nothing. [`tests::a_line_that_merely_mentions_the_agent_is_not_a_file_on_path`]
//! holds that.

use crate::health::HealthCheck;
use crate::util::sh_quote;

/// One file named `claude` a box's PATH would reach, in PATH order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OnPath {
    /// The file itself — `<PATH entry>/claude`.
    pub path: String,
    /// Would the kernel execute it?
    ///
    /// **The bit the whole fault turned on.** A shell skips an entry without it and reports the
    /// next one as the answer, so `false` here is a file that is one `chmod +x` — or one reinstall
    /// that sets a mode without fetching the optional dependency — away from being what every box
    /// on the fleet runs.
    pub executable: bool,
}

/// What running the first one on PATH **did** — and only that.
///
/// There is deliberately no "it was not executable" variant. Whether the file at the head of PATH
/// can run at all is the invariant itself, so [`verdict`] reads it off [`OnPath::executable`]
/// rather than taking the caller's word for it: an earlier draft did take the caller's word, and
/// its own test caught it reporting a pass for a PATH headed by a stub.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ran {
    /// It ran and answered. The string is what it said.
    Answered(String),
    /// It ran and refused — a non-zero exit. The string is what it printed, which for the stub is
    /// its own instructions for repairing itself.
    Refused(String),
    /// The probe could not be made at all: the sandbox did not answer, or the command outlived its
    /// timeout. Nothing is known either way.
    CouldNotAsk,
}

/// The script that lists every file named `claude` on `box_path`, in PATH order.
///
/// One line per file, `x` or `-` for the execute bit, then a tab, then the path. It emits nothing
/// for a PATH entry that holds no such file, so the caller can never mistake a directory's name for
/// a file in it.
///
/// `-e` is paired with `-L` on purpose: `-e` follows the link, so a dangling symlink — which is
/// precisely what an upgrade that removed a target and did not finish leaves behind — would
/// otherwise be invisible to the one check that exists to find it.
///
/// It ends in `true` because a `for` loop carries the exit status of its last command, and the last
/// command on a PATH whose final entry holds no agent is a failing `[ -e ]`. [`crate::place::Place::exec`]
/// turns a non-zero exit into an error, so without this the ordinary case would read as a sandbox
/// that could not be reached.
pub fn listing_script(box_path: &str) -> String {
    let entries = box_path
        .split(':')
        .filter(|e| !e.is_empty())
        .map(sh_quote)
        .collect::<Vec<_>>()
        .join(" ");
    format!(
        "for d in {entries}; do f=\"$d/claude\"; if [ -e \"$f\" ] || [ -L \"$f\" ]; then \
         if [ -x \"$f\" ]; then printf 'x\\t%s\\n' \"$f\"; else printf -- '-\\t%s\\n' \"$f\"; fi; \
         fi; done; true"
    )
}

/// Read [`listing_script`]'s output back.
///
/// Deliberately strict: a line that is not `x<TAB><path>` or `-<TAB><path>` is dropped. Everything
/// a shell might add to that stream — a warning, an error naming the agent, a directory listing —
/// is prose, and prose is not a file on PATH.
pub fn parse_listing(text: &str) -> Vec<OnPath> {
    text.lines()
        .filter_map(|line| {
            let (mark, path) = line.split_once('\t')?;
            let executable = match mark {
                "x" => true,
                "-" => false,
                _ => return None,
            };
            let path = path.trim_end_matches(['\r']);
            (!path.is_empty()).then(|| OnPath {
                path: path.to_string(),
                executable,
            })
        })
        .collect()
}

/// The script that asks one file whether it is the agent.
///
/// It always exits 0 and says on its first field which way it went, because the caller needs the
/// refusal's own words: the stub's four lines are the diagnosis, and a wrapper that collapsed them
/// into "exited 1" would throw the answer away. `2>&1` because that is where the stub writes.
pub fn version_script(path: &str) -> String {
    let q = sh_quote(path);
    format!("if out=$({q} --version 2>&1); then printf 'ok\\t%s' \"$out\"; else printf 'no\\t%s' \"$out\"; fi")
}

/// Read [`version_script`]'s output back. `None` for anything that is not in its shape.
pub fn parse_version(text: &str) -> Option<Ran> {
    let (mark, said) = text.split_once('\t')?;
    let said = said.trim().lines().next().unwrap_or_default().trim();
    match mark {
        "ok" => Some(Ran::Answered(said.to_string())),
        "no" => Some(Ran::Refused(said.to_string())),
        _ => None,
    }
}

/// The invariant, judged.
///
/// **`found.first()`, never `found.iter().find(executable)`.** The second is `command -v`'s
/// question and it is the reason nobody saw this for a fortnight: it reports the first thing that
/// *would* run, and the invariant is about the first thing that is *there*. A file named `claude`
/// at the head of PATH that the kernel will not execute is not a healthy fleet with a spare file in
/// it — it is a fleet one mode bit from having no agent anywhere.
pub fn verdict(found: &[OnPath], ran: &Ran) -> HealthCheck {
    let Some(first) = found.first() else {
        // A fault rather than a shrug. `box-session.sh` says a box without `~/.local/bin` "has no
        // agent and no way to authenticate one", and `Place::wrap`'s doc records the regression
        // where a box resolved the substrate's copy instead of the fleet's. The one fleet this
        // would misjudge is one running codex boxes and nothing else, which no substrate image here
        // produces — the image ships `/usr/local/bin/claude`, so an empty answer means the PATH
        // itself is wrong, not that somebody chose a different runtime.
        return HealthCheck::unsatisfied(
            "no file named claude anywhere on a box's PATH — a box started now would have no agent",
            "install it into the fleet's ~/.local/bin, or check that the box PATH this was asked \
             about is the one box-session.sh hands out",
        );
    };
    let behind = found
        .iter()
        .skip(1)
        .map(|f| f.path.as_str())
        .collect::<Vec<_>>();
    let and_behind = match behind.as_slice() {
        [] => String::new(),
        many => format!(" (behind it: {})", many.join(", ")),
    };
    // Not executable, so it is not what runs today — and it is the only thing between the fleet and
    // every box running it. Read HERE, before anything is asked of whatever a shell would have
    // picked instead, because this is the invariant: the file at the head of PATH either is the
    // agent or the row is red. The remedy is the package's own, quoted rather than described,
    // because the stub prints these instructions to a box's stderr and nobody reads a box's stderr.
    if !first.executable {
        return HealthCheck::unsatisfied(
            format!(
                "{} is the first claude on a box's PATH and is not executable, so every box \
                 silently runs something further down{and_behind}. One chmod, or one reinstall \
                 that sets a mode without fetching the native binary, and no box has an agent",
                first.path
            ),
            format!(
                "make it the agent or take it off PATH: `node $(dirname {})/../lib/node_modules/\
                 @anthropic-ai/claude-code/install.cjs`, or remove {}",
                first.path, first.path
            ),
        );
    }
    match ran {
        Ran::Refused(said) => HealthCheck::unsatisfied(
            format!(
                "{} runs first on every box's PATH and is not the agent — it said: {said}",
                first.path
            ),
            format!(
                "reinstall it without --ignore-scripts or --omit=optional, or remove {} so the \
                 next claude on PATH is the one that runs",
                first.path
            ),
        ),
        Ran::Answered(said) => HealthCheck::satisfied(format!(
            "{} answers `--version` with {said}, and nothing named claude sits in front of \
             it{and_behind}",
            first.path
        )),
        // Never satisfied and never a fault. Nothing was measured, so saying either would be
        // inventing a reading.
        Ran::CouldNotAsk => HealthCheck::unknown(format!(
            "could not ask {} for its version — the sandbox did not answer, so whether a box has a \
             working agent is unknown",
            first.path
        )),
    }
}

/// The whole check, for the box whose HOME is `home`, over an injected prober.
///
/// **The PATH comes from [`crate::place::box_path`] and cannot be passed in.** That is the one edge
/// this module has and it is the reason it is trustworthy: the string being searched is literally
/// the string [`crate::place::Place::wrap`] exports into a box, so the check cannot go on agreeing
/// with itself after the PATH moves underneath it. A caller allowed to hand in its own idea of a
/// box's PATH is SKEIN-678's mount row — a check and its subject built from two different lists
/// inside one binary.
///
/// `probe` runs a script somewhere and returns its stdout, or `None` if it could not be run at all.
/// It is a parameter rather than a [`crate::place::Place`] so that every branch below is drivable
/// from a test without a sandbox — which is the only way the unsatisfied ones can be shown to fail
/// on purpose rather than asserted to.
pub fn agent_on_box_path(home: &str, probe: &dyn Fn(&str) -> Option<String>) -> HealthCheck {
    let Some(listing) = probe(&listing_script(&crate::place::box_path(home))) else {
        return HealthCheck::unknown(
            "could not list the agents on a box's PATH — the sandbox did not answer".to_string(),
        );
    };
    let found = parse_listing(&listing);
    // Nothing is run unless the file at the head of PATH is the one that would run. `CouldNotAsk`
    // for the other two cases is not a claim about them: `verdict` returns on both before it reads
    // this, because "there is no claude at all" and "the first one will not execute" are findings
    // about the listing and need nothing spawned to reach.
    let ran = match found.first() {
        Some(first) if first.executable => match probe(&version_script(&first.path)) {
            None => Ran::CouldNotAsk,
            Some(said) => parse_version(&said).unwrap_or(Ran::CouldNotAsk),
        },
        _ => Ran::CouldNotAsk,
    };
    verdict(&found, &ran)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::health::Level;

    fn stub() -> OnPath {
        OnPath {
            path: "/usr/local/share/npm-global/bin/claude".into(),
            executable: false,
        }
    }

    fn real() -> OnPath {
        OnPath {
            path: "/usr/local/bin/claude".into(),
            executable: true,
        }
    }

    /// **The assertion this module exists for.**
    ///
    /// What would make it fail, concretely: change [`verdict`] to judge
    /// `found.iter().find(|f| f.executable)` instead of `found.first()` — which is what `command -v`
    /// does, and how the fleet ran for a fortnight with a 500-byte stub at the head of every box's
    /// PATH. Done, and this assertion fails on `Satisfied`; restored, and it passes.
    ///
    /// It is handed [`Ran::Answered`] deliberately: the point is that a working agent answering
    /// somewhere on PATH does **not** discharge the invariant.
    #[test]
    fn a_stub_ahead_of_a_working_agent_is_a_fault_even_when_the_working_one_answers() {
        let found = vec![stub(), real()];
        let v = verdict(&found, &Ran::Answered("2.1.263 (Claude Code)".into()));
        assert_eq!(
            v.level,
            Level::Unsatisfied,
            "a file named claude at the head of PATH that will not execute is the fault, however \
             healthy the one behind it is — this verdict said: {}",
            v.detail
        );
        assert!(
            v.detail.contains("/usr/local/share/npm-global/bin/claude"),
            "the verdict has to name the file somebody must act on, and said: {}",
            v.detail
        );
    }

    /// The other half of the same invariant: first, executable, and it refuses.
    #[test]
    fn the_first_one_on_path_answering_is_the_only_thing_that_passes() {
        let answers = verdict(&[real()], &Ran::Answered("2.1.263".into()));
        assert_eq!(answers.level, Level::Satisfied, "{}", answers.detail);

        let refuses = verdict(
            &[real()],
            &Ran::Refused("Error: claude native binary not installed.".into()),
        );
        assert_eq!(refuses.level, Level::Unsatisfied, "{}", refuses.detail);
        assert!(
            refuses.detail.contains("native binary not installed"),
            "the refusal's own words are the diagnosis, and said: {}",
            refuses.detail
        );
    }

    /// A check that cannot ask must not print a tick. Fails if [`Ran::CouldNotAsk`] is ever mapped
    /// onto a pass.
    #[test]
    fn a_probe_that_could_not_ask_is_not_a_pass() {
        let v = verdict(&[real()], &Ran::CouldNotAsk);
        assert_eq!(v.level, Level::Unknown, "{}", v.detail);

        let refused_everything = agent_on_box_path("/home/agent", &|_| None);
        assert_ne!(
            refused_everything.level,
            Level::Satisfied,
            "a sandbox that answered nothing cannot produce a pass, and said: {}",
            refused_everything.detail
        );
    }

    /// **A mention is not an instance.** Nothing that merely contains the word counts.
    #[test]
    fn a_line_that_merely_mentions_the_agent_is_not_a_file_on_path() {
        let noise = "claude: command not found\n/opt/claude-tools/bin\nsh: 1: claude: not found\n";
        assert_eq!(
            parse_listing(noise),
            vec![],
            "only the shape listing_script emits is a file on PATH"
        );
        assert_eq!(
            parse_listing("x\t/usr/local/bin/claude\n").len(),
            1,
            "and that shape is read"
        );
    }

    /// The script asks the filesystem about `<entry>/claude`, per entry, quoted — it does not
    /// search for the word and it cannot be steered by a directory name with a space in it.
    #[test]
    fn the_listing_script_asks_per_entry_and_quotes_what_it_is_given() {
        let script = listing_script("/home/agent/.local/bin:/a dir:");
        assert!(script.contains("'/home/agent/.local/bin'"), "{script}");
        assert!(script.contains("'/a dir'"), "{script}");
        assert!(script.contains("$d/claude"), "{script}");
        assert!(
            !script.contains("::") && !script.contains("''"),
            "an empty PATH entry means the current directory to a shell and must not be asked \
             about: {script}"
        );
        assert!(script.ends_with("true"), "{script}");
    }

    /// Every fault carries a way out — `health::HealthCheck::fix`'s own rule, held here rather than
    /// trusted.
    #[test]
    fn every_unsatisfied_verdict_says_what_would_clear_it() {
        let shadowed = verdict(&[stub(), real()], &Ran::Answered("2.1.263".into()));
        assert_eq!(shadowed.level, Level::Unsatisfied, "{}", shadowed.detail);
        assert!(
            !shadowed.fix.is_empty(),
            "no way out offered for a shadowing stub"
        );

        let refused = verdict(
            &[real()],
            &Ran::Refused("Error: claude native binary not installed.".into()),
        );
        assert_eq!(refused.level, Level::Unsatisfied, "{}", refused.detail);
        assert!(!refused.fix.is_empty(), "no way out offered for a refusal");

        assert!(
            !verdict(&[], &Ran::CouldNotAsk).fix.is_empty(),
            "an empty PATH is a fault and needs a way out too"
        );
    }

    /// End to end over a fake sandbox: the two probes, in order, with the stub first.
    #[test]
    fn the_whole_check_reads_the_head_of_path_and_asks_it() {
        let asked = std::cell::RefCell::new(Vec::<String>::new());
        let probe = |script: &str| {
            asked.borrow_mut().push(script.to_string());
            if script.contains("$d/claude") {
                Some("-\t/usr/local/share/npm-global/bin/claude\nx\t/usr/local/bin/claude\n".into())
            } else {
                Some("ok\t2.1.263".to_string())
            }
        };
        let v = agent_on_box_path("/home/agent", &probe);
        assert_eq!(v.level, Level::Unsatisfied, "{}", v.detail);
        assert_eq!(
            asked.borrow().len(),
            1,
            "a file that will not execute must not be run, and nothing further down is asked in \
             its place: {:?}",
            asked.borrow()
        );

        let healthy = agent_on_box_path("/home/agent", &|script: &str| {
            Some(if script.contains("$d/claude") {
                "x\t/usr/local/bin/claude\n".to_string()
            } else {
                "ok\t2.1.263 (Claude Code)".to_string()
            })
        });
        assert_eq!(healthy.level, Level::Satisfied, "{}", healthy.detail);
        assert!(healthy.detail.contains("2.1.263"), "{}", healthy.detail);
    }
}
