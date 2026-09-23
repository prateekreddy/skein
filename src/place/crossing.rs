//! What a crossing carries into a box: the fixed PATHs on either side of the hop, and the
//! environment it keeps — the launcher's `inherited_env` list, the names the session decides
//! per box, and the shell that takes those from the session's own environment (SKEIN-1085,
//! SKEIN-1095).

/// The PATH every fleet-scope script resolves against — root-owned directories and nothing else.
///
/// bash's own default for a non-login shell, which is the point: what a fleet-scope script needs
/// (`sudo`, `git`, `jq`, `python3`, `bwrap`, `apt-get`, `npm`, `cc`, `curl`, `nsenter`) lives in
/// `/usr/bin`, and the two directories a profile would put ahead of it are writable by every box.
/// See [`Place::shell`] for the whole of why.
pub(super) const FLEET_PATH: &str = "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin";

/// The two user-writable directories a BOX's own PATH carries in front of [`FLEET_PATH`].
///
/// Not a profile's doing and not a guess — **read out of the environment of the processes a live
/// box is running**. Every process inside a box on this fleet carries, verbatim:
///
/// ```text
/// PATH=/home/agent/.local/bin:/usr/local/share/npm-global/bin:/usr/local/sbin:/usr/local/bin:\
/// /usr/sbin:/usr/bin:/sbin:/bin
/// ```
///
/// which is `$HOME/.local/bin`, then this, then `FLEET_PATH` exactly. `~/.local/bin` is the one
/// that matters: it is where Claude Code installs itself, and `box-session.sh` says in as many
/// words that a box handed a home without it "has no agent and no way to authenticate one"
/// (`src/box-session.sh:13-18`).
///
/// **Neither of the two is writable from inside a box any more** (SKEIN-963, SKEIN-968), and the
/// name of this constant is why that had to be done twice. `~/.local` was the first entry of
/// `box-session.sh`'s `share_paths` and is a per-box copy-on-write overlay now; this second entry
/// was never under `$HOME` at all, so making `~/.local` private did not touch it — and it is the
/// one `which -a claude` answers with inside a real box. `box-session.sh` binds it `--ro-bind`.
/// This constant is unchanged: a crossing lands in the box's own mount namespace, so it inherits
/// both of those without having to know about either.
///
/// The PATH stays as it is because the ORDER is what callers depend on — `crate::agentpath` asks
/// whether the first `claude` on it is the agent — and the answer to "can a box change what that
/// resolves to" is now a mount rather than a path edit.
const BOX_PATH_HEAD: &str = "/usr/local/share/npm-global/bin";

/// The PATH a box runs on, built from the `home` in its placement — [`Place::wrap`] exports exactly
/// this, and exports it from here so there is only one of it.
///
/// **A second copy of this string is the bug, not the convenience.** `crate::agentpath` asks whether
/// the first `claude` on a box's PATH is the agent, and a check written from its own idea of what
/// that PATH is would be two lists inside one binary — the shape SKEIN-678 caught in the mount
/// check, where the row and the thing it checked were built from different sets, and the row
/// reported healthy on the one fleet it existed to catch.
pub fn box_path(home: &str) -> String {
    format!("{home}/.local/bin:{BOX_PATH_HEAD}:{FLEET_PATH}")
}

/// The launcher, embedded for the one thing this module reads out of it: its `inherited_env` list.
const LAUNCHER: &str = include_str!("../box-session.sh");

/// **The names a box receives from the environment of whatever reached into it** — the launcher's
/// `inherited_env` list, read out of the launcher's own text (SKEIN-1085).
///
/// A box's session starts through `src/box-session.sh`, which removes every inherited variable not
/// on that list (SKEIN-972). A crossing is the other way in: [`Place::enter`] spawns `nsenter` from
/// skein-server, which carries the server's whole environment into the box's namespace — so the
/// provisioning script, the attach shell, the pane observer it starts, a model call and every other
/// crossing used to see `SKEIN_HOME`, `SKEIN_LISTEN_INHERITED_ONLY` and everything else the cockpit
/// was started with. The contract is the same for both ways in, so the list is the same list.
///
/// **Why parsed from the launcher rather than written again here.** The owner's decision on
/// SKEIN-972 was one written list, each name with its reason; a copy in Rust is two lists that
/// agree until the day somebody edits one. The alternatives were worse on their own terms: asking
/// the INSTALLED launcher for its list at crossing time (`box-session.sh --inherited-env`) would make
/// every crossing depend on a fleet-scope script run first, and on whichever revision a sandbox
/// happens to carry; a third file both read would have to be installed beside the launcher and
/// kept in step with it. The launcher is already compiled into this binary (`fleet::kit`), so this
/// reads the same bytes that get installed, at no cost past the first call.
///
/// Parsed strictly: the block from the line `inherited_env=(` to the line `)`, comments dropped,
/// every remaining word a shell identifier. Anything else panics, and
/// `the_crossing_list_is_the_launchers_list` reads it in every test run, so a launcher edit that
/// breaks the shape fails there rather than in a crossing.
pub fn inherited_env() -> &'static [String] {
    static LIST: std::sync::OnceLock<Vec<String>> = std::sync::OnceLock::new();
    LIST.get_or_init(|| parse_inherited_env(LAUNCHER))
}

fn parse_inherited_env(script: &str) -> Vec<String> {
    let mut lines = script.lines().skip_while(|l| l.trim() != "inherited_env=(");
    assert!(
        lines.next().is_some(),
        "src/box-session.sh has no `inherited_env=(` line, so a crossing has no list to keep"
    );
    let mut names = Vec::new();
    let mut closed = false;
    for line in lines {
        if line.trim() == ")" {
            closed = true;
            break;
        }
        let code = line.split('#').next().unwrap_or("");
        for word in code.split_whitespace() {
            assert!(
                is_identifier(word),
                "{word:?} in the launcher's inherited_env is not a variable name"
            );
            names.push(word.to_string());
        }
    }
    assert!(
        closed,
        "the launcher's inherited_env list has no closing `)`"
    );
    names
}

fn is_identifier(word: &str) -> bool {
    let mut chars = word.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// What a crossing keeps **beyond** the launcher's list, and why each is not on it.
///
/// `TERM` and `COLORTERM` describe a terminal, and a session start has none: the launcher runs
/// `tmux new-session -d`, and tmux gives every pane its own `TERM`. A crossing can carry one —
/// [`crate::sandbox::agent_attach_argv`] and `shell_argv` end in `tmux attach-session`, a client
/// drawing on the terminal a person is looking at, and tmux refuses to attach with no `TERM` at
/// all. `COLORTERM` is how that client learns the terminal takes 24-bit colour. Neither is a
/// path, a credential or a switch skein reads. (`LANG`/`LC_*` are not needed for the same client:
/// every attach passes `tmux -u`.)
const CROSSING_ALSO: &[&str] = &["TERM", "COLORTERM"];

/// The shell, run in front of the `nsenter` hop, that removes every variable not on
/// [`inherited_env`] or [`CROSSING_ALSO`] — the launcher's own filter, for a crossing.
///
/// In front of the hop and not in [`Place::wrap`] behind it, because behind it a login shell has
/// already sourced the box's profile, and what a box sets in its own profile is the box's business
/// (the owner's decision on SKEIN-972). Filtering there would strip it.
///
/// Two halves, as in the launcher: `unset` for every exported name, and `env -u` on the `exec` for
/// the names bash cannot hold as variables (`BASH_FUNC_<name>%%`, anything with a `-`), which bash
/// passes through to its children untouched. It leaves `skein_odd` for [`Place::enter`]'s `exec`.
/// Names only ever reach an argv — never a value.
pub(super) fn keep_only_listed() -> String {
    let keep = inherited_env()
        .iter()
        .map(String::as_str)
        .chain(CROSSING_ALSO.iter().copied())
        .collect::<Vec<_>>()
        .join(" ");
    format!(
        "skein_keep=' {keep} '\n\
         for skein_n in $(compgen -e); do\n\
         \x20 case \"$skein_keep\" in *\" $skein_n \"*) ;; *) unset -v \"$skein_n\" 2>/dev/null ;; esac\n\
         done\n\
         skein_odd=()\n\
         while IFS= read -r -d '' skein_e; do\n\
         \x20 skein_n=\"${{skein_e%%=*}}\"\n\
         \x20 [[ \"$skein_n\" =~ ^[A-Za-z_][A-Za-z0-9_]*$ ]] || skein_odd+=(-u \"$skein_n\")\n\
         done </proc/$$/environ\n"
    )
}

/// **The names the launcher decides per box** — every name on [`inherited_env`] that
/// `src/box-session.sh` goes on to `unset`, read out of the launcher's text (SKEIN-1095).
///
/// The allow-list says which names a box may inherit at all. For these the launcher then decides,
/// box by box, whether the box keeps the inherited value, gets its own, or gets none. `GH_TOKEN`
/// is the one that matters: a scoped box has the fleet's token removed and its own-repo token put
/// in its place, and a `fleet`-scoped box keeps the fleet's. `ANTHROPIC_API_KEY` and
/// `OPENAI_API_KEY` go when the box has a login of its own, and `SSH_AUTH_SOCK` when it is scoped.
/// The rest are skein's channels to the launcher (`SKEIN_FLEET_LIMITS` and the like), which the
/// launcher consumes and no box holds.
///
/// **Read out of the launcher rather than listed here**, for the reason [`inherited_env`] is: a
/// list in Rust is a second copy that agrees until somebody adds an `unset` to the launcher. A name
/// the launcher starts removing is a name a crossing starts taking from the session, with nothing
/// else to edit. `the_names_the_session_decides_are_the_launchers_unsets` pins what this reads
/// today.
///
/// `PATH` and `HOME` are refused outright. The launcher REPLACES both rather than unsetting them,
/// so they are never read here. But if an `unset PATH` were ever added, taking the session's value
/// would put the box's own `~/.local/bin` in front of the `nsenter` a crossing resolves at fleet
/// scope, which is ISO-1 (see [`Place::enter`]). [`Place::wrap`] gives both their box values past
/// the hop, which is the only place they belong.
pub fn session_decides() -> &'static [String] {
    static LIST: std::sync::OnceLock<Vec<String>> = std::sync::OnceLock::new();
    LIST.get_or_init(|| parse_session_decides(LAUNCHER, inherited_env()))
}

fn parse_session_decides(script: &str, inherited: &[String]) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    for line in script.lines() {
        if line.trim_start().starts_with('#') {
            continue;
        }
        let code = line.split(" #").next().unwrap_or("");
        let mut words = code.split_whitespace();
        while let Some(word) = words.next() {
            if word != "unset" {
                continue;
            }
            for arg in words.by_ref() {
                let arg = arg.trim_end_matches(';');
                if arg.is_empty() || arg == "&&" || arg == "||" {
                    break;
                }
                if inherited.iter().any(|n| n == arg) && !names.iter().any(|n| n == arg) {
                    names.push(arg.to_string());
                }
            }
        }
    }
    for pinned in ["PATH", "HOME"] {
        assert!(
            !names.iter().any(|n| n == pinned),
            "src/box-session.sh unsets {pinned}, and a crossing must not take the session's \
             {pinned} in front of its hop — see `place::session_decides`"
        );
    }
    names
}

/// The shell, run after [`keep_only_listed`] and in front of the hop, that gives a crossing **the
/// session's value of every name in [`session_decides`], or none** (SKEIN-1095).
///
/// # Why a crossing reads the answer rather than working it out again
///
/// The launcher's decision rests on things a crossing does not have. `SKEIN_GIT_SCOPE` and
/// `SKEIN_BOX_REPO` ride only on the launcher's own command line (`fleet::session_script`). The
/// own-repo token is read from `$state/git-tokens` at that moment. The login that drops an API key
/// is a file in the box's PRIVATE home, which the namespace binds over `$HOME`. Working all that out
/// again here would be a second copy of the rule, and one that disagrees with the session the first
/// time the switch is flipped under a running box: `gitgate::set_box_scope` takes effect at the
/// box's next start, and a running box keeps what it was given.
///
/// The answer is already written down, in the one process a crossing already trusts: the anchor.
/// `ns_pid` is the box's tmux server, started by the launcher's last line with the launcher's final
/// environment, so `/proc/<ns_pid>/environ` is what the launcher decided for THIS box — scoped or
/// not, own token or none, login or not. [`Place::guard`] has proved `ns_pid` is that server one
/// line earlier in the same shell, and `nsenter` is about to open files under the same
/// `/proc/<ns_pid>/`, so reading one more costs no new trust.
///
/// # What it does
///
/// Each decided name is unset, then exported again from the anchor's environment if and only if
/// the anchor has it. So a scoped box's crossing carries its own-repo token or no `GH_TOKEN` at
/// all, never the fleet's; a `fleet`-scoped box's carries the fleet's, which is what its session
/// holds. An environment that cannot be read leaves every decided name unset. That is the direction
/// the launcher takes when it is unsure of the scope, and the `nsenter` that follows would fail on
/// the same permission anyway.
///
/// **A box can influence what is read here, and gains nothing by it.** The anchor is the box's own
/// process, so a box could arrange a different value. What that buys is a crossing that runs inside
/// that same box with a value the box chose. The only programs that run with it in front of the hop
/// are `env` and `nsenter`, which read none of these names, and the value is handed to `export` as
/// one word: never evaluated, never on an argv.
pub(super) fn as_the_session_holds(ns_pid: u32) -> String {
    let names = session_decides();
    let patterns = names
        .iter()
        .map(|n| format!("{n}=*"))
        .collect::<Vec<_>>()
        .join("|");
    format!(
        "unset -v {unset}\n\
         {{ while IFS= read -r -d '' skein_e; do\n\
         \x20 case \"$skein_e\" in {patterns}) export \"$skein_e\" ;; esac\n\
         done; }} 2>/dev/null </proc/{ns_pid}/environ\n\
         unset skein_e\n",
        unset = names.join(" "),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **The crossing keeps the launcher's list, read the way bash reads it** (SKEIN-1085).
    ///
    /// The list a crossing keeps is parsed out of `src/box-session.sh` rather than written twice, so
    /// what has to hold is that the parse and bash agree about which names are in the array. The
    /// expected side is bash's own: the block cut out with the `sed` `docs/threat-model.md` prints
    /// it with, evaluated, and `"${inherited_env[@]}"` printed back.
    ///
    /// What would make it fail: a parser that keeps a word from a comment (`# The sandbox's …`), one
    /// that stops a line early or drops the last line before `)`, or a launcher edit that moves the
    /// array into a shape this parser does not read — which panics here rather than in a crossing.
    /// And the cockpit's own variables on the list, which is the other half of SKEIN-972.
    #[test]
    fn the_crossing_list_is_the_launchers_list() {
        let block = std::process::Command::new("sed")
            .args(["-n", "/^inherited_env=(/,/^)/p"])
            .arg(concat!(env!("CARGO_MANIFEST_DIR"), "/src/box-session.sh"))
            .output()
            .expect("sed");
        let block = String::from_utf8_lossy(&block.stdout).into_owned();
        let bash = std::process::Command::new("bash")
            .arg("-c")
            .arg(format!("{block}\nprintf '%s\\n' \"${{inherited_env[@]}}\""))
            .output()
            .expect("bash");
        let by_bash: Vec<String> = String::from_utf8_lossy(&bash.stdout)
            .lines()
            .map(str::to_string)
            .collect();
        assert!(
            by_bash.len() > 20 && by_bash.iter().any(|n| n == "HOME"),
            "bash read no list out of the launcher, so the comparison below would be about \
             nothing: {by_bash:?}"
        );
        assert_eq!(
            inherited_env(),
            by_bash.as_slice(),
            "the names a crossing keeps are not the names the launcher's own array holds"
        );
        for cockpit in [
            "SKEIN_HOME",
            "SKEIN_LISTEN_INHERITED_ONLY",
            "SKEIN_IN_FLEET",
        ] {
            assert!(
                !inherited_env().iter().any(|n| n == cockpit) && !CROSSING_ALSO.contains(&cockpit),
                "{cockpit} is the cockpit's and must not cross into a box"
            );
        }
    }

    /// **What a crossing takes from the box's session is what the launcher unsets** (SKEIN-1095).
    ///
    /// Pinned by name, so that a launcher edit which changes the set is seen here and read, rather
    /// than silently changing what every crossing carries. What would make it fail: the parse
    /// missing the `[ … ] && unset ANTHROPIC_API_KEY` form, or an indented `unset GH_TOKEN`; a parse
    /// that reads the filter's own `unset -v "$name"` as a name; the launcher dropping its
    /// `unset GH_TOKEN`, which is the whole of the scoped boundary.
    #[test]
    fn the_names_the_session_decides_are_the_launchers_unsets() {
        let mut got: Vec<&str> = session_decides().iter().map(String::as_str).collect();
        got.sort_unstable();
        assert_eq!(
            got,
            [
                "ANTHROPIC_API_KEY",
                "GH_TOKEN",
                "OPENAI_API_KEY",
                "SKEIN_BOX_STORE",
                "SKEIN_FLEET_LIMITS",
                "SKEIN_FLEET_MOUNTS",
                "SKEIN_MODEL_SCRATCH",
                "SSH_AUTH_SOCK",
            ],
            "the names a crossing takes from the box's session are not the inherited names \
             src/box-session.sh unsets"
        );
        let list = |names: &[&str]| names.iter().map(|n| n.to_string()).collect::<Vec<_>>();
        let inherited = list(&["GH_TOKEN", "OPENAI_API_KEY", "PATH", "HOME", "SANDBOX_NAME"]);
        assert_eq!(
            parse_session_decides(
                "# unset SANDBOX_NAME in a comment\n\
                 [ -s x ] && unset OPENAI_API_KEY\n\
                 \x20 unset gh_direct GH_TOKEN; unset -v \"$name\"\n",
                &inherited
            ),
            list(&["OPENAI_API_KEY", "GH_TOKEN"]),
            "the parse reads a comment, misses a guarded or indented unset, or reads a local"
        );
        let refused =
            std::panic::catch_unwind(|| parse_session_decides("unset PATH\n", &inherited));
        assert!(
            refused.is_err(),
            "a launcher that unsets PATH would have the crossing take the box's PATH in front of \
             its hop, and the parse let it through"
        );
    }
}
