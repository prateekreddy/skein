//! What a crossing carries into a box: the fixed PATHs on either side of the hop, and the
//! environment it keeps — the launcher's `inherited_env` list, the names the session decides
//! per box, every name the session block exports for the box's tools, and the shell that takes
//! those two from the session's own environment (SKEIN-1085, SKEIN-1095, SKEIN-1354, SKEIN-1371).

/// The PATH every fleet-scope script resolves against — root-owned directories and nothing else.
///
/// bash's own default for a non-login shell, which is the point: what a fleet-scope script needs
/// (`sudo`, `git`, `jq`, `python3`, `bwrap`, `apt-get`, `npm`, `cc`, `curl`, `nsenter`) lives in
/// `/usr/bin`, and the two directories a profile would put ahead of it are writable by every box.
/// See [`Place::shell`](super::Place::shell) for the whole of why.
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

/// The PATH a box runs on, built from the `home` in its placement — `Place::wrap` exports exactly
/// this, and exports it from here so there is only one of it.
///
/// **A second copy of this string is the bug, not the convenience.** `crate::agentpath` asks whether
/// the first `claude` on a box's PATH is the agent, and a check written from its own idea of what
/// that PATH is would be two lists inside one binary — the shape SKEIN-678 caught in the mount
/// check, where the row and the thing it checked were built from different sets, and the row
/// reported healthy on the one fleet it existed to catch.
///
/// The third entry is the box's `~/.cargo/bin`, where `box-session.sh` binds the fleet's shared
/// Rust toolchain read-only (SKEIN-1337). Spelled from `home` like the first, so this stays a
/// function of the placement record alone.
pub fn box_path(home: &str) -> String {
    format!("{home}/.local/bin:{BOX_PATH_HEAD}:{home}/.cargo/bin:{FLEET_PATH}")
}

/// The launcher, embedded for what this module reads out of it: its `inherited_env` list, its
/// `unset`s, and the session block's exports.
const LAUNCHER: &str = include_str!("../box-session.sh");

/// **The names a box receives from the environment of whatever reached into it** — the launcher's
/// `inherited_env` list, read out of the launcher's own text (SKEIN-1085).
///
/// A box's session starts through `src/box-session.sh`, which removes every inherited variable not
/// on that list (SKEIN-972). A crossing is the other way in: `Place::enter` spawns `nsenter` from
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
/// In front of the hop and not in [`Place::wrap`](super::Place::wrap) behind it, because behind it
/// a login shell has already sourced the box's profile, and what a box sets in its own profile is
/// the box's business (the owner's decision on SKEIN-972). Filtering there would strip it.
///
/// Two halves, as in the launcher: `unset` for every exported name, and `env -u` on the `exec` for
/// the names bash cannot hold as variables (`BASH_FUNC_<name>%%`, anything with a `-`), which bash
/// passes through to its children untouched. It leaves `skein_odd` for
/// [`Place::enter`](super::Place::enter)'s `exec`. Names only ever reach an argv — never a value.
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
/// scope, which is ISO-1 (see `Place::enter`). `Place::wrap` gives both their box values past the
/// hop, which is the only place they belong.
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

/// **The names the session points at the repo's download cache** — every `NAME=` the launcher's
/// session block exports inside its `if [ -n "$repo_cache" ] && [ -d "$repo_cache" ]` (SKEIN-1350),
/// read out of the launcher's text (SKEIN-1354).
///
/// A crossing carries these as part of [`session_exports`], which reads every export in the
/// block; this list is the part of it that names the cache, for what is said about the cache. A
/// crossing takes them from the session as it takes [`session_decides`]: the value the box's own
/// session holds, or none. So a crossing into a box whose repo has a managed store gets
/// `CARGO_HOME`, `npm_config_cache` and the rest with the session's own values, and a crossing into
/// a box the launcher gave no cache (a store kept elsewhere by `skein add --store`, a cache that is
/// missing or is a link) gets none of them and keeps the box's own `~/.cargo` and `~/.npm`. Whether
/// a box has a cache is decided once, by the launcher, from `$SKEIN_BOX_STORE`; a crossing working
/// it out again from the repo's store would be a second copy of that rule, and the first one to be
/// wrong about a box whose store was moved after it started.
///
/// **Read out of the launcher rather than listed here**, for the reason [`inherited_env`] is: a
/// tool the session block starts pointing at the cache is a name every crossing carries, with
/// nothing else to edit. `the_names_a_crossing_takes_for_the_repo_cache_are_the_session_blocks`
/// pins what this reads today.
///
/// Refused outright: `PATH`, `HOME`, and every name that the `env`, `nsenter` or `bash` a crossing
/// runs in front of its hop would read (`LD_*`, `BASH_ENV`, `ENV`, `IFS`, `SHELLOPTS`,
/// `BASHOPTS`). The value is the box's session's, which a box can influence, so it must never be
/// a name anything at fleet scope acts on. The cache names are read by cargo, npm, pip, uv and
/// pnpm, and none of those runs in front of the hop.
pub fn session_cache_names() -> &'static [String] {
    static LIST: std::sync::OnceLock<Vec<String>> = std::sync::OnceLock::new();
    LIST.get_or_init(|| parse_session_cache_names(LAUNCHER))
}

/// The line that opens the session block's repo-cache exports, as `src/box-session.sh` spells it.
const CACHE_EXPORTS_OPEN: &str = r#"if [ -n "$repo_cache" ] && [ -d "$repo_cache" ]; then"#;

fn parse_session_cache_names(script: &str) -> Vec<String> {
    let mut lines = script
        .lines()
        .skip_while(|l| l.trim() != CACHE_EXPORTS_OPEN);
    assert!(
        lines.next().is_some(),
        "src/box-session.sh has no `{CACHE_EXPORTS_OPEN}` line, so a crossing cannot tell which \
         names point a box at its repo's download cache"
    );
    let mut names: Vec<String> = Vec::new();
    let mut closed = false;
    for line in lines {
        let code = line.trim();
        if code == "fi" {
            closed = true;
            break;
        }
        if code.starts_with('#') {
            continue;
        }
        for word in code.split_whitespace() {
            let Some((name, _)) = word.split_once('=') else {
                continue;
            };
            if is_identifier(name) && !names.iter().any(|n| n == name) {
                names.push(name.to_string());
            }
        }
    }
    assert!(
        closed && !names.is_empty(),
        "the launcher's repo-cache exports have no `fi`, or export nothing"
    );
    for name in &names {
        assert!(
            !matches!(
                name.as_str(),
                "PATH" | "HOME" | "BASH_ENV" | "ENV" | "IFS" | "SHELLOPTS" | "BASHOPTS"
            ) && !name.starts_with("LD_"),
            "src/box-session.sh points {name} at the repo cache, and a crossing must not take \
             the session's {name} in front of its hop — see `place::session_cache_names`"
        );
    }
    names
}

/// **Every name the session block exports for the box's tools** — the toolchain (`RUSTUP_HOME`,
/// `CARGO_HOME`, `CARGO_INSTALL_ROOT`), the browsers (`PLAYWRIGHT_*`), the agent's auto-update
/// switches (`DISABLE_*`) and the repo cache's names of [`session_cache_names`] among them, read out
/// of the launcher's `bash -lc` block (SKEIN-1371).
///
/// The owner's decision on SKEIN-1371: a crossing carries **every** tool variable the launcher
/// exports to the session, not named blocks of them. So this reads every `export` in the block and
/// not only the ones inside some `if`, and an export the block gains, inside the repo-cache `if` or
/// anywhere else, is one every crossing carries with nothing else to edit. The value is the
/// session's, taken from the anchor by [`as_the_session_holds`], so an export the block makes only
/// on a condition (the toolchain, the cache) reaches a crossing exactly when the session has it.
///
/// **`PATH` is read and not carried.** The block exports `PATH="$box_path"`, and `Place::wrap`
/// already gives a crossing `box_path(home)` past the hop, the same string the launcher builds
/// (`tests/isolation_bwrap/path.rs` measures the two agree). Taking the session's would put the
/// box's own `~/.local/bin` in front of the `env` and `nsenter` a crossing resolves at fleet scope,
/// which is ISO-1. So the parse accepts PATH only with exactly that value, and panics on any other:
/// a session PATH computed some other way is one `Place::wrap` no longer reproduces.
///
/// Refused outright, as [`session_cache_names`] refuses them: `HOME` and every name the `env`,
/// `nsenter` or `bash` in front of the hop would read (`LD_*`, `BASH_ENV`, `ENV`, `IFS`,
/// `SHELLOPTS`, `BASHOPTS`). The value is the box's session's, which a box can influence.
///
/// Parsed strictly: the block from the line `bash -lc '` to the line that opens `' bash `, with
/// `\` continuations joined, comments dropped, and every word after an `export` a shell
/// identifier, alone or with `=`. `the_names_a_crossing_takes_from_the_session_are_every_export`
/// pins what this reads today.
pub fn session_exports() -> &'static [String] {
    static LIST: std::sync::OnceLock<Vec<String>> = std::sync::OnceLock::new();
    LIST.get_or_init(|| parse_session_exports(LAUNCHER))
}

/// The line that opens the session block, as `src/box-session.sh` spells it, and the start of the
/// line that closes it.
const SESSION_OPEN: &str = "bash -lc '";
const SESSION_CLOSE: &str = "' bash ";

/// The one value of `PATH` the session block may export: the launcher's `box_path`, which
/// `Place::wrap` reproduces as [`box_path`].
const SESSION_PATH: &str = "\"$box_path\"";

fn parse_session_exports(script: &str) -> Vec<String> {
    let mut lines = script.lines().skip_while(|l| l.trim() != SESSION_OPEN);
    assert!(
        lines.next().is_some(),
        "src/box-session.sh has no `{SESSION_OPEN}` line, so a crossing cannot tell which names \
         the session exports for the box's tools"
    );
    let mut logical: Vec<String> = Vec::new();
    let mut pending = String::new();
    let mut closed = false;
    for line in lines {
        if line.trim_start().starts_with(SESSION_CLOSE) {
            closed = true;
            break;
        }
        if pending.is_empty() && line.trim_start().starts_with('#') {
            continue;
        }
        match line.strip_suffix('\\') {
            Some(head) => {
                pending.push_str(head);
                pending.push(' ');
            }
            None => {
                pending.push_str(line);
                logical.push(std::mem::take(&mut pending));
            }
        }
    }
    assert!(
        closed,
        "the launcher's session block has no line opening `{SESSION_CLOSE}`"
    );
    let mut names: Vec<String> = Vec::new();
    let mut path_seen = false;
    for line in &logical {
        for statement in shell_statements(line) {
            let mut words = statement.iter();
            if !words.by_ref().any(|w| w == "export") {
                continue;
            }
            for word in words {
                let (name, value) = match word.split_once('=') {
                    Some((name, value)) => (name, Some(value)),
                    None => (word.as_str(), None),
                };
                assert!(
                    is_identifier(name),
                    "{word:?} after an `export` in the launcher's session block is not a variable \
                     name, so a crossing cannot tell what the session exports"
                );
                assert!(
                    !matches!(
                        name,
                        "HOME" | "BASH_ENV" | "ENV" | "IFS" | "SHELLOPTS" | "BASHOPTS"
                    ) && !name.starts_with("LD_"),
                    "src/box-session.sh exports {name} to the session, and a crossing must not \
                     take the session's {name} in front of its hop — see `place::session_exports`"
                );
                if name == "PATH" {
                    assert_eq!(
                        value,
                        Some(SESSION_PATH),
                        "src/box-session.sh exports a session PATH other than {SESSION_PATH}, which \
                         `Place::wrap` no longer reproduces — see `place::session_exports`"
                    );
                    path_seen = true;
                    continue;
                }
                if !names.iter().any(|n| n == name) {
                    names.push(name.to_string());
                }
            }
        }
    }
    assert!(
        path_seen && !names.is_empty(),
        "the launcher's session block exports no PATH, or nothing else, so this parse is reading \
         something other than the block"
    );
    names
}

/// One logical shell line split into statements of words: whitespace outside double quotes
/// separates words, an unquoted `;`, `&&` or `||` ends a statement, and an unquoted `#` at the
/// start of a word ends the line. The session block is one single-quoted argument, so it holds no
/// single quotes, and nothing in it is escaped inside a double-quoted word.
fn shell_statements(line: &str) -> Vec<Vec<String>> {
    let mut statements = vec![Vec::new()];
    let mut word = String::new();
    let mut quoted = false;
    let mut chars = line.chars().peekable();
    let end_word = |word: &mut String, statements: &mut Vec<Vec<String>>| {
        if !word.is_empty() {
            statements.last_mut().unwrap().push(std::mem::take(word));
        }
    };
    while let Some(c) = chars.next() {
        match c {
            '"' => {
                quoted = !quoted;
                word.push(c);
            }
            _ if quoted => word.push(c),
            '#' if word.is_empty() => break,
            ';' => {
                end_word(&mut word, &mut statements);
                statements.push(Vec::new());
            }
            '&' | '|' if chars.peek() == Some(&c) => {
                chars.next();
                end_word(&mut word, &mut statements);
                statements.push(Vec::new());
            }
            c if c.is_whitespace() => end_word(&mut word, &mut statements),
            _ => word.push(c),
        }
    }
    end_word(&mut word, &mut statements);
    statements.retain(|s| !s.is_empty());
    statements
}

/// The shell, run after [`keep_only_listed`] and in front of the hop, that gives a crossing **the
/// session's value of every name in [`session_decides`] and [`session_exports`], or none**
/// (SKEIN-1095, SKEIN-1354, SKEIN-1371).
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
/// not, own token or none, login or not. [`Place::guard`](super::Place::guard) has proved `ns_pid`
/// is that server one line earlier in the same shell, and `nsenter` is about to open files under
/// the same `/proc/<ns_pid>/`, so reading one more costs no new trust.
///
/// # What it does
///
/// Each decided name is unset, then exported again from the anchor's environment if and only if
/// the anchor has it. So a scoped box's crossing carries no `GH_TOKEN` at all, never the
/// fleet's; a `fleet`-scoped box's carries the fleet's, which is what its session
/// holds. An environment that cannot be read leaves every decided name unset. That is the direction
/// the launcher takes when it is unsure of the scope, and the `nsenter` that follows would fail on
/// the same permission anyway.
///
/// The cache names go the same way: a session the launcher gave a repo cache holds them, so its
/// crossings get the same paths into that cache; a session it gave none holds none, so its
/// crossings get none either and whatever skein-server had for `CARGO_HOME` stays out of the box,
/// as [`keep_only_listed`] already made it. Never a path the box cannot see: the value is the one
/// the box's own session runs on, and the crossing runs in that box's namespace. The rest of the
/// block's exports go the same way: a session that found the fleet's toolchain holds its
/// `RUSTUP_HOME`, so a crossing's `cargo` reaches the same toolchain, and one that found none holds
/// none.
///
/// **A box can influence what is read here, and gains nothing by it.** The anchor is the box's own
/// process, so a box could arrange a different value. What that buys is a crossing that runs inside
/// that same box with a value the box chose. The only programs that run with it in front of the hop
/// are `env` and `nsenter`, which read none of these names, and the value is handed to `export` as
/// one word: never evaluated, never on an argv.
pub(super) fn as_the_session_holds(ns_pid: u32) -> String {
    let mut names: Vec<&String> = Vec::new();
    for name in session_decides().iter().chain(session_exports()) {
        if !names.contains(&name) {
            names.push(name);
        }
    }
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
        unset = names
            .iter()
            .map(|n| n.as_str())
            .collect::<Vec<_>>()
            .join(" "),
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

    /// **What a crossing takes from the box's session for the repo cache is what the session
    /// block points at it** (SKEIN-1354).
    ///
    /// Pinned by name, for the reason the test below is. What would make it fail: the session
    /// block gaining or losing a tool without this list being read again; a parse that stops at
    /// the first line's `\\` or reads the `if` line's `$repo_cache`; the launcher moving the
    /// exports out of their `if`, which panics here rather than in a crossing. And a name that a
    /// crossing's `env`, `nsenter` or `bash` reads in front of the hop, which the parse refuses.
    #[test]
    fn the_names_a_crossing_takes_for_the_repo_cache_are_the_session_blocks() {
        let mut got: Vec<&str> = session_cache_names().iter().map(String::as_str).collect();
        got.sort_unstable();
        assert_eq!(
            got,
            [
                "CARGO_HOME",
                "PIP_CACHE_DIR",
                "UV_CACHE_DIR",
                "UV_LINK_MODE",
                "npm_config_cache",
                "npm_config_store_dir",
                "pnpm_config_store_dir",
            ],
            "the names a crossing takes from the box's session for the repo cache are not the \
             names src/box-session.sh exports inside its repo-cache `if`"
        );
        for bad in ["PATH", "LD_PRELOAD", "BASH_ENV"] {
            let script =
                format!("  {CACHE_EXPORTS_OPEN}\n    export {bad}=\"$repo_cache/x\"\n  fi\n");
            assert!(
                std::panic::catch_unwind(|| parse_session_cache_names(&script)).is_err(),
                "a session block that points {bad} at the cache would have a crossing take the \
                 box's {bad} in front of its hop, and the parse let it through"
            );
        }
    }

    /// **What a crossing takes from the box's session for its tools is every name the session
    /// block exports** (SKEIN-1371).
    ///
    /// Pinned by name, so that a launcher edit which changes the set is seen and read. What would
    /// make it fail: a parse that reads only the repo-cache `if` (it then misses `RUSTUP_HOME` and
    /// the rest, which is SKEIN-1371); one that drops the second line of a `\\` continuation
    /// (`CARGO_INSTALL_ROOT`, `RUSTUP_AUTO_INSTALL`); one that reads a comment. And the session's
    /// `PATH` carried rather than reproduced, or a name read in front of the hop let through, which
    /// the parse refuses.
    #[test]
    fn the_names_a_crossing_takes_from_the_session_are_every_export() {
        let mut got: Vec<&str> = session_exports().iter().map(String::as_str).collect();
        got.sort_unstable();
        assert_eq!(
            got,
            [
                "CARGO_HOME",
                "CARGO_INSTALL_ROOT",
                "DISABLE_AUTOUPDATER",
                "DISABLE_UPDATES",
                "PIP_CACHE_DIR",
                "PLAYWRIGHT_BROWSERS_PATH",
                "PLAYWRIGHT_SKIP_BROWSER_GC",
                "RUSTUP_AUTO_INSTALL",
                "RUSTUP_HOME",
                "UV_CACHE_DIR",
                "UV_LINK_MODE",
                "npm_config_cache",
                "npm_config_store_dir",
                "pnpm_config_store_dir",
            ],
            "the names a crossing takes from the box's session are not the names \
             src/box-session.sh's session block exports"
        );
        for name in session_cache_names() {
            assert!(
                session_exports().contains(name),
                "{name} points the session at the repo cache and a crossing does not carry it"
            );
        }
        let block =
            |body: &str| format!("  bash -lc '\n    export PATH=\"$box_path\"\n{body}  ' bash x\n");
        assert_eq!(
            parse_session_exports(&block(
                "    # export SKEIN_TEST_COMMENT=1\n\
                 \x20   if [ -d x ]; then export A=\"$HOME/a b\" \\\n\
                 \x20     B=1; fi; export C # D=1\n"
            )),
            ["A", "B", "C"],
            "the parse reads a comment, splits a quoted value, or drops a continuation line"
        );
        for bad in [
            "export PATH=\"$box_path:/x\"\n",
            "export HOME=/x\n",
            "export LD_PRELOAD=/x\n",
            "export BASH_ENV=/x\n",
        ] {
            assert!(
                std::panic::catch_unwind(|| parse_session_exports(&block(bad))).is_err(),
                "a session block with `{}` would have a crossing take or lose a name it must \
                 not, and the parse let it through",
                bad.trim()
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
                // SKEIN-1225: the telemetry switch, consumed by the launcher and held by no box.
                "SKEIN_BOX_TELEMETRY",
                // SKEIN-1372: the versions to bind, and the tests' npm prefix, both consumed by
                // the launcher and held by no box.
                "SKEIN_BOX_VERSIONS",
                "SKEIN_FLEET_LIMITS",
                "SKEIN_FLEET_MOUNTS",
                "SKEIN_MODEL_SCRATCH",
                "SKEIN_NPM_PREFIX",
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
