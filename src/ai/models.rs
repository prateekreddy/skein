//! Which models this `claude` accepts, asked of `claude --help` and remembered.

use super::*;

/// **Which models this `claude` will accept, asked of `claude` itself** (SKEIN-451).
///
/// A dropdown rather than a text box, and one "that is aware of what is possible", as asked for.
/// A list written down here would be a list that goes stale the week a model ships — so it is
/// parsed out of `claude --help`, which names them:
///
/// ```text
///   --model <model>    Model for the current session. Provide
///                      an alias for the latest model (e.g.
///                      'fable', 'opus', or 'sonnet') or a
///                      model's full name (e.g.
///                      'claude-fable-5').
/// ```
///
/// Only the ALIASES, which is everything quoted before the words "full name". The full-name example
/// is an example of a form, not a model anybody should be offered: `claude-fable-5` is real today
/// and will not be forever, while `fable` is defined to mean the latest of its line. A reader who
/// wants an exact build can still type one — the setting stays free text underneath.
///
/// Pure, and separate from the spawn, so the thing that decides what a person is offered can be
/// tested without a CLI on the machine running the test. Run against the real binary on
/// 2026-08-27 (`claude --help`, version 2.1.247) it answers `["fable", "opus", "sonnet"]`.
fn parse_model_aliases(help: &str) -> Vec<String> {
    let Some(at) = help.find("--model <model>") else {
        return Vec::new();
    };
    // To the end of that flag's paragraph: the next line that introduces another flag. Without this
    // bound the scan runs into `--fallback-model`'s prose and offers whatever it happens to quote.
    let rest = &help[at..];
    let block = rest.find("\n  -").map(|end| &rest[..end]).unwrap_or(rest);
    // And stop at the full-name clause, for the reason in the doc above.
    let block = match block.find("full name") {
        Some(end) => &block[..end],
        None => block,
    };
    // **Scanned, not split on quotes.** `model's full name` puts an apostrophe in the middle of
    // the prose, so pairing quotes off in order reads `s full name (e.g. ` as a quoted token and
    // offers `s` as a model. The first two tests written here both caught it. So each candidate
    // must look like a model name in its own right — lowercase letters, digits and dashes, nothing
    // else — and a run that does not is skipped rather than shifting every pair after it.
    let mut out: Vec<String> = Vec::new();
    let chars: Vec<char> = block.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] != '\'' {
            i += 1;
            continue;
        }
        let start = i + 1;
        let Some(close) = (start..chars.len()).find(|&j| chars[j] == '\'') else {
            break;
        };
        let name: String = chars[start..close].iter().collect();
        let looks_like_a_model = !name.is_empty()
            && name.len() <= 40
            && name
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_alphanumeric())
            && name
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
        if looks_like_a_model && !out.contains(&name) {
            out.push(name);
        }
        // From the closing quote either way: an apostrophe that opened nothing must not consume
        // the quote that opens the next real name.
        i = if looks_like_a_model { close + 1 } else { i + 1 };
    }
    out
}

/// The models to offer, remembered — **read, never asked** on the caller's clock, the same rule
/// [`runtime_updates`] follows and for the same reason: this is drawn on a settings page that must
/// not wait on a process spawn.
///
/// Empty means "skein could not ask", and the page falls back to a free-text box — which is what
/// the setting has always been, so nothing is lost when this cannot answer.
pub fn model_choices() -> Vec<String> {
    let known = MODELS.lock().unwrap_or_else(|e| e.into_inner()).clone();
    if known.is_none() && !ASKING_MODELS.swap(true, std::sync::atomic::Ordering::SeqCst) {
        std::thread::spawn(|| {
            let found = ask_model_choices();
            *MODELS.lock().unwrap_or_else(|e| e.into_inner()) = Some(found);
            ASKING_MODELS.store(false, std::sync::atomic::Ordering::SeqCst);
        });
    }
    known.unwrap_or_default()
}

/// `claude --help`, and nothing else. No network, no sandbox hop: the flag's own help text is the
/// same wherever the CLI runs, and this is the one question about it that costs nothing to ask.
fn ask_model_choices() -> Vec<String> {
    let bin = claude_bin();
    // Through [`agent_command`], like the call itself: `--help` spends nothing, but it is still the
    // real CLI on `$PATH` being spawned by a test process that never said to (SKEIN-764).
    let Ok(out) = agent_command(&bin).arg("--help").output() else {
        return Vec::new();
    };
    parse_model_aliases(&String::from_utf8_lossy(&out.stdout))
}

static MODELS: std::sync::Mutex<Option<Vec<String>>> = std::sync::Mutex::new(None);
static ASKING_MODELS: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

#[cfg(test)]
mod tests {

    /// **The dropdown's options come from the CLI, not from a list in here** (SKEIN-451).
    ///
    /// The picker was asked for "aware of what is possible". Anything written down in
    /// skein is a list that goes stale the week a model ships, so this parses `claude --help`. The
    /// fixture is that help text verbatim, wrapped exactly as the CLI wraps it — the wrapping is
    /// the hard part, because the aliases are split across lines and a naive line-wise scan finds
    /// none of them.
    #[test]
    fn the_models_offered_are_the_ones_this_claude_says_it_takes() {
        let help = "  --mcp-config <configs...>             Load MCP servers from a JSON file\n                    \x20 --model <model>                       Model for the current session. Provide\n                    \x20                                       an alias for the latest model (e.g.\n                    \x20                                       'fable', 'opus', or 'sonnet') or a\n                    \x20                                       model's full name (e.g.\n                    \x20                                       'claude-fable-5').\n                    \x20 -n, --name <name>                     Set a display name for this session\n";
        assert_eq!(
            super::parse_model_aliases(help),
            vec!["fable", "opus", "sonnet"],
            "the aliases the CLI names are not what would be offered"
        );
    }

    /// Two bounds, and both are load-bearing. The full-name example is a form rather than a choice
    /// — `claude-fable-5` is real today and will not be forever, while `fable` is defined to mean
    /// the latest of its line — and the scan must stop before the NEXT flag's prose, or it offers
    /// whatever that happens to quote.
    #[test]
    fn the_model_scan_stops_at_the_full_name_example_and_at_the_next_flag() {
        let help = "  --model <model>   Provide an alias (e.g. 'opus') or a model's full name \
                    (e.g. 'claude-fable-5').\n  --other <x>       takes 'yes' or 'no'\n";
        let got = super::parse_model_aliases(help);
        assert_eq!(
            got,
            vec!["opus"],
            "the scan ran past its own paragraph: {got:?}"
        );

        // And an answer it cannot read is no answer, never a guess: the page falls back to the
        // free-text box the setting has always been.
        assert!(
            super::parse_model_aliases("claude: command not found").is_empty(),
            "prose with no --model flag in it was turned into a list of models"
        );
    }
}
