//! The agent CLIs and Rust toolchains every box runs, each a known version that skein fetched,
//! checked and rolled out, and the "restart to apply" that follows a rollout (SKEIN-1372).
//!
//! Two kinds of entry in the fleet depot (`fleet::depot`), and one rule for when a box moves onto a
//! new one. The owner's decisions of 2026-10-30, which this module is the whole of:
//!
//!  1. **A new CLI version is fetched and tested automatically, and rolled out by the owner.**
//!     [`watch_pass`] asks the npm registry what the newest claude-code and codex are, fills each
//!     one the depot lacks in a namespace that sees no box, and tests it from the published entry
//!     ([`test_cli`]). The cockpit then offers "Roll out", and nothing changes until it is pressed
//!     ([`rollout`]).
//!  2. **A box moves only when it is idle.** A rollout changes what the NEXT start binds. A running
//!     box keeps the entry it started with (each entry is immutable) and is shown "restart to
//!     apply" ([`stale_keys`], read by `fleet::why_cover_is_stale`). [`restart_onto_rollouts`]
//!     restarts it once it is idle, and never while it works.
//!  3. **Rust is per repo.** A repo whose tree names a toolchain in `rust-toolchain.toml` (or
//!     `rust-toolchain`) gets that toolchain, fetched once into the depot and shared read-only.
//!     Every other repo gets the fleet's default, which is rolled out like a CLI.
//!
//! **How a box gets its versions.** skein computes the map ([`wanted_versions`]) and hands it to the
//! launcher in `SKEIN_BOX_VERSIONS`; the launcher binds each named entry read-only where the box's
//! PATH or rustup already looks, and reports what it bound on its first `SKEIN_VERSIONS` line; the
//! placement record keeps that ([`versions_from_launch`]). "Restart to apply" is then a diff of the
//! recorded map against the one skein would hand over now, which is the binding table's stale diff
//! (`docs/design/box-environment.md` §2.1, task A2) for these two rows.
//!
//! **And one kind no box is given**: the TypeScript skein's mod is type-checked with ([`TYPESCRIPT`],
//! SKEIN-1396), pinned in `src/typescript.version`, fetched by [`watch_pass`] and bound only into the
//! check of a claude-code entry.
//!
//! **What is kept.** Every entry a placed box records, the current version of each component and
//! the one before it, for rollback. Removing the rest is offered ([`clean`]) and never automatic.

use super::depot::{self, Kind, Links};
use super::*;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

// ------------------------------------------------------------------------------------------------
// The two kinds
// ------------------------------------------------------------------------------------------------

/// An agent CLI as the depot holds it: one npm prefix per version, holding that one package.
pub const AGENT_CLI: Kind = Kind {
    name: "agent-cli",
    entry_is_nameable: cli_entry_is_nameable,
    // npm's `bin/<command>` is a relative link into the package, and that is how a prefix says
    // which file the command is.
    links: Links::Internal,
    // Measured on the fleet's sandbox, 2026-10-30: claude-code 2.1.314 is 558 MB with its native
    // binary, codex 390 MB. Twice the larger, for growth.
    fill_cap: 1200 << 20,
    // About four versions of each, which covers what running boxes use plus one for rollback.
    budget: 8 << 30,
    deadline: Duration::from_secs(20 * 60),
};

/// A Rust toolchain as the depot holds it: the directory rustup keeps under `toolchains/<name>`.
pub const RUST_TOOLCHAIN: Kind = Kind {
    name: "rust-toolchain",
    entry_is_nameable: toolchain_is_nameable,
    // The fleet's toolchain has none (`find -type l`, 2026-10-30); a relative one inside the entry
    // is harmless, so a future component that adds one is not refused for it.
    links: Links::Internal,
    // The fleet's stable-aarch64 toolchain with the default profile is 1.6 GB.
    fill_cap: 3 << 30,
    budget: 12 << 30,
    deadline: Duration::from_secs(30 * 60),
};

/// The TypeScript compiler skein's mod is type-checked with (SKEIN-1396): one npm prefix per
/// version, `typescript@<version>`, holding that one package. Only the claude-code check reads it,
/// bound read-only at `/tsc` ([`checks_for`]); no box is given it, and it is no rollout component,
/// so it is not in [`CLIS`].
pub const TYPESCRIPT: Kind = Kind {
    name: "typescript",
    entry_is_nameable: typescript_entry_is_nameable,
    // npm's `bin/tsc` is a relative link into the package, as an agent CLI's command is.
    links: Links::Internal,
    // Measured on the fleet's sandbox, 2026-10-30: typescript 5.9.3 installed with
    // `--ignore-scripts` into an empty prefix is 23.6 MB (22.5 MiB), two links and no other special
    // file, in under 3 s. About five times that, for growth.
    fill_cap: 128 << 20,
    // The pin and the one before it, many times over.
    budget: 1 << 30,
    deadline: Duration::from_secs(5 * 60),
};

/// One agent CLI skein manages: the runtime's name, the npm package, the command it installs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cli {
    pub runtime: &'static str,
    pub package: &'static str,
    pub command: &'static str,
}

/// The CLIs skein fetches, the same table the launcher's projection carries (`box-session.sh`, "the
/// fleet's agent CLIs"); `the_launchers_cli_table_is_this_one` holds the two together.
pub const CLIS: [Cli; 2] = [
    Cli {
        runtime: "claude",
        package: "@anthropic-ai/claude-code",
        command: "claude",
    },
    Cli {
        runtime: "codex",
        package: "@openai/codex",
        command: "codex",
    },
];

/// The component the fleet's default Rust toolchain is rolled out under.
pub const RUST: &str = "rust";
/// The key a box's own repo toolchain is recorded under.
pub const RUST_REPO: &str = "rust-repo";
/// The key for a toolchain the box's repo pins and the depot does not hold yet (SKEIN-1415). The
/// launcher binds nothing for it: it names the entry in the box's rustup shim, which tells a cargo
/// run in the repo what is missing in place of rustup's read-only-file-system error, and reports
/// it back like a bound entry. So the box records what it was told, and when the entry arrives
/// the box is stale for `rust-toolchain` and moves onto it when idle, as onto any rollout.
pub const RUST_REPO_MISSING: &str = "rust-repo-missing";

fn cli(runtime: &str) -> Option<&'static Cli> {
    CLIS.iter().find(|c| c.runtime == runtime)
}

/// A plain release number, `<major>.<minor>.<patch>`, no leading zeros, no pre-release tag. It
/// reaches `npm install` on a command line and becomes a path, so it is a whitelist.
pub fn version_is_nameable(v: &str) -> bool {
    let number = |s: &str| {
        !s.is_empty()
            && s.len() <= 6
            && s.chars().all(|c| c.is_ascii_digit())
            && (s == "0" || !s.starts_with('0'))
    };
    let parts: Vec<&str> = v.split('.').collect();
    parts.len() == 3 && parts.iter().all(|p| number(p))
}

/// `<runtime>@<version>`, split, for a runtime skein manages and a plain version.
pub fn cli_entry_parts(entry: &str) -> Option<(&'static Cli, &str)> {
    let (runtime, version) = entry.split_once('@')?;
    let cli = cli(runtime)?;
    version_is_nameable(version).then_some((cli, version))
}

fn cli_entry_is_nameable(entry: &str) -> bool {
    cli_entry_parts(entry).is_some()
}

/// `typescript@<version>`, a plain version, and nothing else.
fn typescript_entry_is_nameable(entry: &str) -> bool {
    entry
        .strip_prefix("typescript@")
        .is_some_and(version_is_nameable)
}

/// The host's target triple, as rustup names toolchains for it.
pub fn host_triple() -> String {
    format!("{}-unknown-linux-gnu", std::env::consts::ARCH)
}

/// A dated channel, `nightly-2026-08-01` or `beta-2026-08-01`.
fn dated_channel(s: &str) -> bool {
    let Some(date) = s
        .strip_prefix("nightly-")
        .or_else(|| s.strip_prefix("beta-"))
    else {
        return false;
    };
    let b = date.as_bytes();
    b.len() == 10
        && b[4] == b'-'
        && b[7] == b'-'
        && date
            .chars()
            .enumerate()
            .all(|(i, c)| i == 4 || i == 7 || c.is_ascii_digit())
}

/// A channel that names one toolchain for ever: `1.<minor>.<patch>` or a dated nightly or beta.
/// `stable`, `beta`, `nightly` and `1.80` name whatever upstream has today, which an immutable entry
/// cannot be.
pub fn channel_is_pinned(c: &str) -> bool {
    (version_is_nameable(c) && c.starts_with("1.")) || dated_channel(c)
}

/// `<pinned channel>-<triple>`, the name rustup gives the toolchain, split into its channel and
/// triple.
fn toolchain_base_parts(name: &str) -> Option<(&str, &str)> {
    if name.len() > 80 {
        return None;
    }
    let triple_ok = |t: &str| {
        let parts: Vec<&str> = t.split('-').collect();
        (3..=4).contains(&parts.len())
            && parts.iter().all(|p| {
                !p.is_empty()
                    && p.chars()
                        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
            })
    };
    // A dated channel has two dashes of its own; a version has none.
    let split = if name.starts_with("nightly-") || name.starts_with("beta-") {
        name.match_indices('-').nth(3).map(|(i, _)| i)
    } else {
        name.find('-')
    };
    let at = split?;
    let (channel, triple) = (&name[..at], &name[at + 1..]);
    (channel_is_pinned(channel) && triple_ok(triple)).then_some((channel, triple))
}

/// The components every toolchain entry holds, so an entry's name never lists them: the minimal
/// profile's three, and the clippy and rustfmt the fleet's agents run on every repo.
const BASELINE_COMPONENTS: [&str; 5] = ["cargo", "clippy", "rust-std", "rustc", "rustfmt"];

/// The most extra components, and the most extra targets, one entry may hold. A repo's file names
/// them and a box writes that file, so each one is a fetch skein makes on the box's say.
const MAX_EXTRAS: usize = 8;

/// A rustup component name: `llvm-tools`, `rust-src`, `rust-analyzer`. Lowercase words joined by
/// single dashes, starting with a letter. It reaches rustup's command line and the entry's name.
fn component_is_nameable(c: &str) -> bool {
    c.len() <= 40
        && c.starts_with(|ch: char| ch.is_ascii_lowercase())
        && c.split('-').all(|w| {
            !w.is_empty()
                && w.chars()
                    .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit())
        })
}

/// A target triple: `wasm32-unknown-unknown`, `thumbv8m.main-none-eabi`. Two to five dashed parts
/// of lowercase letters, digits, `_` and `.`, each starting with a letter or digit, so never `..`.
fn target_is_nameable(t: &str) -> bool {
    let parts: Vec<&str> = t.split('-').collect();
    t.len() <= 60
        && (2..=5).contains(&parts.len())
        && parts.iter().all(|p| {
            p.starts_with(|c: char| c.is_ascii_lowercase() || c.is_ascii_digit())
                && !p.contains("..")
                && p.chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '.')
        })
}

/// A toolchain entry, read from its name: the toolchain rustup knows it by, and the components and
/// targets it holds beyond [`BASELINE_COMPONENTS`] and its own host target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolchainEntry<'a> {
    /// `<pinned channel>-<triple>`: the directory name rustup looks for under `toolchains/`.
    pub toolchain: &'a str,
    pub components: Vec<&'a str>,
    pub targets: Vec<&'a str>,
}

/// **A toolchain entry's name says what it holds** (SKEIN-1415):
/// `<channel>-<triple>[+<component>…][@<target>…]`, for example
/// `1.98.1-aarch64-unknown-linux-gnu@wasm32-unknown-unknown`, or
/// `1.98.1-aarch64-unknown-linux-gnu+llvm-tools+rust-src@wasm32-unknown-unknown`. Two repos pinning
/// one version with different targets are two entries, and two that ask for the same set are one.
/// Neither `+` nor `@` can appear in a channel, component or target, so the split is unambiguous,
/// and both are in the launcher's `box_version` shape check. Only the canonical spelling is an entry:
/// each list sorted, without repeats, without a baseline component or the host target, so one set
/// has one name.
pub fn toolchain_entry_parts(entry: &str) -> Option<ToolchainEntry<'_>> {
    if entry.len() > 240 {
        return None;
    }
    let (head, targets) = match entry.split_once('@') {
        Some((h, t)) => (h, t.split('@').collect::<Vec<_>>()),
        None => (entry, Vec::new()),
    };
    let (toolchain, components) = match head.split_once('+') {
        Some((tc, c)) => (tc, c.split('+').collect::<Vec<_>>()),
        None => (head, Vec::new()),
    };
    let (_, triple) = toolchain_base_parts(toolchain)?;
    let ascending = |v: &[&str]| v.windows(2).all(|w| w[0] < w[1]);
    let ok = components.len() <= MAX_EXTRAS
        && targets.len() <= MAX_EXTRAS
        && ascending(&components)
        && ascending(&targets)
        && components
            .iter()
            .all(|c| component_is_nameable(c) && !BASELINE_COMPONENTS.contains(c))
        && targets
            .iter()
            .all(|t| target_is_nameable(t) && *t != triple);
    ok.then_some(ToolchainEntry {
        toolchain,
        components,
        targets,
    })
}

/// The entry name for `toolchain` with these components and targets, in the canonical spelling
/// [`toolchain_entry_parts`] reads: the baseline components and the host target are dropped, the
/// rest sorted. `None` when any name is not the shape of one, or there are too many.
pub fn toolchain_entry(
    toolchain: &str,
    components: &[String],
    targets: &[String],
) -> Option<String> {
    let (_, triple) = toolchain_base_parts(toolchain)?;
    // Each name is checked before it is joined: `a+b` is not a component, though its two halves,
    // read back from the joined name, would each be one.
    if !(components.iter().all(|c| component_is_nameable(c))
        && targets.iter().all(|t| target_is_nameable(t)))
    {
        return None;
    }
    let components: BTreeSet<&str> = components
        .iter()
        .map(String::as_str)
        .filter(|c| !BASELINE_COMPONENTS.contains(c))
        .collect();
    let targets: BTreeSet<&str> = targets
        .iter()
        .map(String::as_str)
        .filter(|t| *t != triple)
        .collect();
    let mut name = toolchain.to_string();
    for c in &components {
        name.push('+');
        name.push_str(c);
    }
    for t in &targets {
        name.push('@');
        name.push_str(t);
    }
    toolchain_entry_parts(&name).is_some().then_some(name)
}

fn toolchain_is_nameable(name: &str) -> bool {
    toolchain_entry_parts(name).is_some()
}

/// The kind a component's entries are of.
pub fn kind_of(component: &str) -> Option<&'static Kind> {
    if cli(component).is_some() {
        Some(&AGENT_CLI)
    } else if component == RUST {
        Some(&RUST_TOOLCHAIN)
    } else {
        None
    }
}

// ------------------------------------------------------------------------------------------------
// What is rolled out, and what was tested
// ------------------------------------------------------------------------------------------------

/// One component's rollout: the entry new starts bind, and the one before it, kept for rollback.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Rolled {
    pub entry: String,
    #[serde(default)]
    pub previous: Option<String>,
    /// Epoch seconds.
    #[serde(default)]
    pub at: i64,
}

/// One check run against a CLI entry.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CheckResult {
    pub name: String,
    pub ok: bool,
    /// What it printed, at most a few lines, for the cockpit.
    pub said: String,
}

/// The outcome of [`test_cli`] for one entry.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Tested {
    pub ok: bool,
    pub at: i64,
    pub checks: Vec<CheckResult>,
    /// The TypeScript entry the mod's check ran with, for a claude-code entry. A record that names
    /// another, or none, is from before the pin it would be tested with now, and is tested again.
    #[serde(default)]
    pub typescript: Option<String>,
}

/// skein's record of rollouts and tests: on the host, with skein's other declared state, never in a
/// box's reach. Owner decisions (a rollout) are in it, so it survives a fleet rebuild.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Rollouts {
    #[serde(default)]
    pub current: BTreeMap<String, Rolled>,
    /// Keyed by entry name.
    #[serde(default)]
    pub tested: BTreeMap<String, Tested>,
    /// The newest version the registry reported per CLI, and when it was asked.
    #[serde(default)]
    pub upstream: BTreeMap<String, String>,
    /// What the last fill of each entry said when it failed, so the cockpit can show it.
    #[serde(default)]
    pub failed: BTreeMap<String, String>,
}

fn rollouts_path() -> PathBuf {
    crate::config::skein_home().join("versions.json")
}

/// The record, or an empty one when there is none yet or it does not parse: then nothing is rolled
/// out, and every box binds what it bound before this existed.
pub fn rollouts() -> Rollouts {
    crate::util::read_json_or_why(&rollouts_path())
        .ok()
        .flatten()
        .unwrap_or_default()
}

fn update_rollouts<R>(f: impl FnOnce(&mut Rollouts) -> Result<R, String>) -> Result<R, String> {
    crate::util::update_json(&rollouts_path(), f)
}

/// Make `entry` the version every box of `component` starts on from now. The owner's press.
///
/// Refused unless the entry is published, is of that component, and, for a CLI, passed its tests.
/// A running box is not touched: it shows "restart to apply" and moves when it is idle.
pub fn rollout(component: &str, entry: &str) -> Result<Rolled, String> {
    let kind = kind_of(component).ok_or_else(|| format!("{component:?} is not a component"))?;
    if !depot::is_published(kind, entry) {
        return Err(format!("{entry} is not in the fleet's {} store", kind.name));
    }
    if let Some((cli, version)) = cli_entry_parts(entry) {
        if cli.runtime != component {
            return Err(format!("{entry} is not a version of {component}"));
        }
        if let Some(pin) = pinned(component).filter(|p| *p != version) {
            return Err(format!(
                "skein's mod is checked against {component} {pin}, so only {component}@{pin} can \
                 be rolled out. To move to {version}, change src/claude-code.version in a \
                 reviewed commit; its CI checks the mod against {version} first."
            ));
        }
    } else if component != RUST {
        return Err(format!("{entry} is not a version of {component}"));
    }
    update_rollouts(|r| {
        if kind.name == AGENT_CLI.name && !r.tested.get(entry).is_some_and(|t| t.ok) {
            return Err(format!(
                "{entry} has not passed its tests, so it cannot be rolled out. Run its tests \
                 first."
            ));
        }
        let was = r.current.get(component).map(|c| c.entry.clone());
        let rolled = Rolled {
            entry: entry.to_string(),
            previous: was.filter(|w| w != entry),
            at: chrono::Utc::now().timestamp(),
        };
        r.current.insert(component.to_string(), rolled.clone());
        Ok(rolled)
    })
}

// ------------------------------------------------------------------------------------------------
// What a box gets
// ------------------------------------------------------------------------------------------------

/// The toolchain entry a box's tree names, read **as data**: `rust-toolchain.toml`'s
/// `[toolchain]` `channel`, `components` and `targets`, or the one line of a legacy
/// `rust-toolchain`, named as [`toolchain_entry`] names it. `None` when there is no file, the file
/// is a link or larger than 4 KB, it names a channel that moves (`stable`, `nightly`, `1.80`),
/// which the fleet's default then serves, or any component or target is not the shape of one.
///
/// The tree is the box's, so this is hostile input read on skein's side: opened without following
/// a link at its last component, read to a bound, and only a pinned channel's exact shape, and
/// component and target names of their exact shapes, are used. A file with one bad name pins
/// nothing, rather than a toolchain that silently lacks what the repo asked for. The file's
/// `profile` is not read: every entry is the minimal profile plus the baseline and what the file
/// names, and a box's rustup never installs, so the profile has nothing to choose.
pub fn repo_toolchain(tree: &Path) -> Option<String> {
    use std::io::Read;
    use std::os::unix::fs::OpenOptionsExt;
    for (file, toml) in [("rust-toolchain.toml", true), ("rust-toolchain", false)] {
        let Ok(f) = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(tree.join(file))
        else {
            continue;
        };
        if !f.metadata().is_ok_and(|m| m.is_file()) {
            return None;
        }
        let mut text = String::new();
        if f.take(4097).read_to_string(&mut text).is_err() || text.len() > 4096 {
            return None;
        }
        let (channel, components, targets) = if toml || text.contains("[toolchain]") {
            (
                toml_string(&toml_value(&text, "channel")?)?,
                toml_list(toml_value(&text, "components").as_deref())?,
                toml_list(toml_value(&text, "targets").as_deref())?,
            )
        } else {
            let line = text.lines().map(str::trim).find(|l| !l.is_empty())?;
            (line.to_string(), Vec::new(), Vec::new())
        };
        let triple = host_triple();
        let channel = channel
            .strip_suffix(&format!("-{triple}"))
            .unwrap_or(&channel)
            .to_string();
        if !channel_is_pinned(&channel) {
            return None;
        }
        return toolchain_entry(&format!("{channel}-{triple}"), &components, &targets);
    }
    None
}

/// The raw value of `key = …` in the `[toolchain]` table, comments dropped, and nothing cleverer.
/// An array that opens on the key's line and closes on a later one is joined into one line.
fn toml_value(text: &str, key: &str) -> Option<String> {
    let mut in_table = false;
    let mut lines = text.lines();
    while let Some(line) = lines.next() {
        let line = line.split('#').next().unwrap_or("").trim();
        if line.starts_with('[') {
            in_table = line == "[toolchain]";
            continue;
        }
        if !in_table {
            continue;
        }
        let Some((k, value)) = line.split_once('=') else {
            continue;
        };
        if k.trim() != key {
            continue;
        }
        let mut v = value.trim().to_string();
        if v.starts_with('[') {
            // An array never closed comes back unclosed, for `toml_list` to refuse, rather than
            // as no value, which would read as no list at all.
            while !v.contains(']') {
                let Some(more) = lines.next() else { break };
                v.push_str(more.split('#').next().unwrap_or("").trim());
            }
        }
        return Some(v);
    }
    None
}

/// A TOML basic or literal string, `"x"` or `'x'`, unquoted.
fn toml_string(v: &str) -> Option<String> {
    let v = v.trim();
    v.strip_prefix('"')
        .and_then(|v| v.strip_suffix('"'))
        .or_else(|| v.strip_prefix('\'').and_then(|v| v.strip_suffix('\'')))
        .map(str::to_string)
}

/// A TOML array of strings, `["a", 'b',]`; empty when the key is absent, `None` when it is not
/// that shape. The names in it are checked by [`toolchain_entry`], not here.
fn toml_list(v: Option<&str>) -> Option<Vec<String>> {
    let Some(v) = v else {
        return Some(Vec::new());
    };
    let inner = v.trim().strip_prefix('[')?.strip_suffix(']')?;
    let mut out = Vec::new();
    for item in inner.split(',') {
        let item = item.trim();
        if item.is_empty() {
            continue;
        }
        out.push(toml_string(item)?);
    }
    (out.len() <= 2 * MAX_EXTRAS).then_some(out)
}

/// The versions skein would hand box `name` if it started now: each CLI's and the default
/// toolchain's current entry, each only when it is published, and the box's own repo toolchain:
/// under [`RUST_REPO`] when it is published, under [`RUST_REPO_MISSING`] when it is not. A
/// component with nothing rolled out is absent, and the box binds what it did before.
///
/// **A repo's pin does not wait for a fleet default** (SKEIN-1415). With none rolled out, the
/// launcher puts the box's rustup home together from the bootstrap's own toolchains, still the
/// default, and the repo's entry beside them (`box-session.sh`, "Which toolchains").
pub fn wanted_versions(name: &str) -> BTreeMap<String, String> {
    wanted_with(&rollouts(), &Path::new(&box_root(name)).join("tree"))
}

fn wanted_with(r: &Rollouts, tree: &Path) -> BTreeMap<String, String> {
    let mut want = BTreeMap::new();
    for c in CLIS {
        if let Some(rolled) = r.current.get(c.runtime) {
            if depot::is_published(&AGENT_CLI, &rolled.entry) {
                want.insert(c.runtime.to_string(), rolled.entry.clone());
            }
        }
    }
    let default = r
        .current
        .get(RUST)
        .map(|c| c.entry.clone())
        .filter(|e| depot::is_published(&RUST_TOOLCHAIN, e));
    if let Some(default) = &default {
        want.insert(RUST.to_string(), default.clone());
    }
    if let Some(own) = repo_toolchain(tree).filter(|own| default.as_ref() != Some(own)) {
        let key = match depot::is_published(&RUST_TOOLCHAIN, &own) {
            true => RUST_REPO,
            false => RUST_REPO_MISSING,
        };
        want.insert(key.to_string(), own);
    }
    want
}

/// The map as the launcher takes it, `key=entry` separated by spaces.
pub fn versions_env(want: &BTreeMap<String, String>) -> String {
    want.iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join(" ")
}

/// What the launcher bound, from its **first** `SKEIN_VERSIONS` line (the reason
/// `fleet::launcher_from_launch` gives: a later line is one the box's profile can print). `None`
/// for a launcher too old to say, which leaves the box alone rather than reading as stale.
/// Only known keys with the shape of an entry survive.
pub fn versions_from_launch(out: &str) -> Option<BTreeMap<String, String>> {
    let line = out
        .lines()
        .find_map(|l| l.trim().strip_prefix("SKEIN_VERSIONS"))?;
    if !(line.is_empty() || line.starts_with(' ')) {
        return None;
    }
    Some(
        line.split_whitespace()
            .filter_map(|kv| kv.split_once('='))
            .filter(|(k, v)| match *k {
                RUST | RUST_REPO | RUST_REPO_MISSING => toolchain_is_nameable(v),
                k => cli_entry_parts(v).is_some_and(|(c, _)| c.runtime == k),
            })
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
    )
}

/// The stale-reason keys for a box born with `born` when skein would now hand it `now`:
/// `agent-cli` when a CLI moved, `rust-toolchain` when its toolchain did. Each is "restart to
/// apply" on the board (`src/web/app/board.js`, `RESTART_REASONS`).
pub fn stale_keys(
    born: &BTreeMap<String, String>,
    now: &BTreeMap<String, String>,
) -> Vec<&'static str> {
    let moved = |k: &str| born.get(k) != now.get(k);
    let mut keys = Vec::new();
    if CLIS.iter().any(|c| moved(c.runtime)) {
        keys.push("agent-cli");
    }
    if moved(RUST) || moved(RUST_REPO) || moved(RUST_REPO_MISSING) {
        keys.push("rust-toolchain");
    }
    keys
}

/// The stale keys this module owns, for the restart rule: a box stale for nothing else is restarted
/// by [`restart_onto_rollouts`] when it is idle.
pub const VERSION_KEYS: [&str; 2] = ["agent-cli", "rust-toolchain"];

/// One component a running box is behind on, for the box's own signal file and the cockpit.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Pending {
    pub component: String,
    /// What the box runs, or empty when it runs the sandbox's own copy.
    pub have: String,
    /// What a restart gives it, or empty when a restart takes it back to the sandbox's own copy.
    pub new: String,
}

/// What box `name` would get from a restart, against what it was born with. A missing repo
/// toolchain is not a version a restart gives, so [`RUST_REPO_MISSING`] is never one; when its
/// entry arrives, the `rust-repo` row says so.
pub fn pending_for(record: &crate::place::PlaceRecord, name: &str) -> Vec<Pending> {
    let Some(born) = &record.versions else {
        return Vec::new();
    };
    let now = wanted_versions(name);
    let keys: BTreeSet<&String> = born.keys().chain(now.keys()).collect();
    keys.into_iter()
        .filter(|k| *k != RUST_REPO_MISSING && born.get(*k) != now.get(*k))
        .map(|k| Pending {
            component: k.clone(),
            have: born.get(k).cloned().unwrap_or_default(),
            new: now.get(k).cloned().unwrap_or_default(),
        })
        .collect()
}

// ------------------------------------------------------------------------------------------------
// Fill and test
// ------------------------------------------------------------------------------------------------

/// npm, in the depot's namespace, installing `package@version` into an empty prefix named for the
/// entry. `scripts` says whether the package's install scripts run there.
fn npm_prefix_fill(entry: &str, package: &str, version: &str, scripts: bool) -> depot::Fill {
    let mut fetch: Vec<String> = vec![
        "npm".into(),
        "install".into(),
        "--global".into(),
        "--prefix".into(),
        format!("/stage/out/{entry}"),
        "--no-fund".into(),
        "--no-audit".into(),
    ];
    if !scripts {
        fetch.push("--ignore-scripts".into());
    }
    fetch.push(format!("{package}@{version}"));
    depot::Fill {
        fetch,
        env: vec![
            ("npm_config_cache".into(), "/stage/npm".into()),
            ("npm_config_update_notifier".into(), "false".into()),
        ],
        read_only: Vec::new(),
        out: "out".into(),
    }
}

/// The fill for a CLI version: one package into an empty prefix named for the entry. The package's
/// install script runs there (claude-code's places its native binary), as upstream code,
/// unprivileged, writing only staging.
pub fn cli_fill(entry: &str) -> Result<depot::Fill, String> {
    let (cli, version) = cli_entry_parts(entry)
        .ok_or_else(|| format!("{entry:?} is not a CLI and a version, like claude@2.1.314"))?;
    Ok(npm_prefix_fill(entry, cli.package, version, true))
}

/// The fill for a TypeScript version: the same, with `--ignore-scripts`. TypeScript has no install
/// script, so the tarball npm checks against its integrity is all that lands (trust class A,
/// `docs/design/box-environment.md` §6.3).
pub fn typescript_fill(entry: &str) -> Result<depot::Fill, String> {
    let version = entry
        .strip_prefix("typescript@")
        .filter(|v| version_is_nameable(v))
        .ok_or_else(|| {
            format!("{entry:?} is not typescript and a version, like typescript@5.9.3")
        })?;
    Ok(npm_prefix_fill(entry, "typescript", version, false))
}

/// The fill for a toolchain entry: the sandbox's own rustup, bound read-only into the depot's
/// namespace, installing into a rustup home inside staging. rustup checks every component against
/// the channel manifest's sha256.
///
/// It installs the entry's toolchain with the minimal profile, [`BASELINE_COMPONENTS`]' clippy and
/// rustfmt and the components the entry's name lists, and the targets it lists (SKEIN-1415), then
/// moves rustup's `toolchains/<toolchain>` to `out/<entry>`, since the entry is named for all of
/// that and rustup names the directory for the toolchain alone. Every name reaches the command as
/// its own argument, after the shell's script, so none of it is ever read as shell.
pub fn toolchain_fill(name: &str) -> Result<depot::Fill, String> {
    let entry = toolchain_entry_parts(name).ok_or_else(|| {
        format!(
            "{name:?} is not a pinned toolchain, like 1.98.0-{} or 1.98.0-{}@wasm32-unknown-unknown",
            host_triple(),
            host_triple()
        )
    })?;
    let rustup = format!("{}/cargo/bin/rustup", skein_toolchain_path());
    let components = ["clippy", "rustfmt"]
        .into_iter()
        .chain(entry.components.iter().copied())
        .collect::<Vec<_>>()
        .join(",");
    let mut fetch: Vec<String> = vec![
        "/bin/sh".into(),
        "-c".into(),
        r#"set -e; r=$1; e=$2; t=$3; shift 3; "$r" toolchain install "$t" "$@"; mv -- "/stage/rh/toolchains/$t" "/stage/out/$e""#.into(),
        "sh".into(),
        "/opt/skein-rustup/rustup".into(),
        name.into(),
        entry.toolchain.into(),
        "--profile".into(),
        "minimal".into(),
        "--component".into(),
        components,
    ];
    if !entry.targets.is_empty() {
        fetch.push("--target".into());
        fetch.push(entry.targets.join(","));
    }
    fetch.push("--no-self-update".into());
    Ok(depot::Fill {
        fetch,
        env: vec![
            ("RUSTUP_HOME".into(), "/stage/rh".into()),
            ("CARGO_HOME".into(), "/stage/ch".into()),
        ],
        read_only: vec![(rustup, "/opt/skein-rustup/rustup".into())],
        out: "out".into(),
    })
}

/// The one Claude Code version skein's mod is checked against, and so the one the fleet may roll
/// out (SKEIN-1380, the owner's option (a)). `src/claude-code.version` is the only place it is
/// written: CI's `plugin` job reads the same file, and `the_ci_pin_is_the_fleets_pin` fails if
/// either stops doing so. Moving it is a reviewed commit, whose CI runs `tools/plugin-check.sh`
/// against the new build before any box can be given it.
pub const CLAUDE_CODE_PIN: &str = include_str!("../claude-code.version");

/// The one TypeScript version skein's mod is type-checked with, by CI and by the fleet's check of a
/// claude-code entry alike (SKEIN-1396). `src/typescript.version` is the only place it is written:
/// CI's `plugin` job reads the same file, and `the_ci_typescript_pin_is_the_fleets_pin` fails if
/// either stops doing so.
pub const TYPESCRIPT_PIN: &str = include_str!("../typescript.version");

/// The depot entry of the pinned TypeScript, `typescript@<pin>`.
pub fn typescript_entry() -> String {
    format!("typescript@{}", TYPESCRIPT_PIN.trim())
}

/// The version `runtime` is pinned to, when it is: claude-code is; codex follows npm's newest.
pub fn pinned(runtime: &str) -> Option<&'static str> {
    (runtime == "claude").then(|| CLAUDE_CODE_PIN.trim())
}

/// A check run against a CLI entry before it may be rolled out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CliCheck {
    pub name: &'static str,
    /// The command, run in [`check_script`]'s namespace: the entry read-only at `/cli`, with
    /// `/cli/bin` first on `PATH`, and skein's own source read-only at `/skein-src`.
    pub argv: Vec<String>,
    /// Variables set for it.
    pub env: Vec<(String, String)>,
    /// Paths bound read-only into its namespace besides, `(source in the sandbox, path inside)`.
    pub binds: Vec<(String, String)>,
}

/// Every check for `version` of `cli`. First the version check (SKEIN-870's lesson: the version is
/// what the binary says when asked, not what a file name claims). Then, for Claude Code, **the
/// mod's own check** (SKEIN-1359): `tools/plugin-check.sh` from skein's source, with the entry's
/// `claude` first on `PATH` and `CLAUDE_CODE_VERSION` set, so it refuses a build other than the one
/// under test. It validates and tests `src/plugin` with that build, runs it headless offline, and
/// type-checks the hooks module against the declarations that build lays, with the pinned
/// TypeScript at `typescript` bound read-only at `/tsc` and named by `TSC` (SKEIN-1396). A check the
/// script says it cannot run (exit 2, a tool missing) is a failure: an untested build is not rolled
/// out.
pub fn checks_for(cli: &Cli, version: &str, typescript: &Path) -> Vec<CliCheck> {
    let mut all = vec![CliCheck {
        name: "version",
        argv: vec![format!("/cli/bin/{}", cli.command), "--version".into()],
        env: Vec::new(),
        binds: Vec::new(),
    }];
    if cli.runtime == "claude" {
        all.push(CliCheck {
            name: "plugin-check",
            argv: vec!["bash".into(), "/skein-src/tools/plugin-check.sh".into()],
            env: vec![
                ("CLAUDE_CODE_VERSION".into(), version.to_string()),
                ("TSC".into(), "/tsc/bin/tsc".into()),
            ],
            binds: vec![(typescript.display().to_string(), "/tsc".into())],
        });
    }
    all
}

/// The namespace a check runs in: the entry read-only at `/cli`, skein's source read-only at
/// `/skein-src`, the check's own binds, `/usr` and `/etc`, all read-only, a private `/tmp` as `HOME` and `TMPDIR`, **no
/// network**, a cleared environment, its own PID namespace, ended with its parent.
fn check_script(entry_dir: &str, source: &str, check: &CliCheck) -> String {
    let argv = check
        .argv
        .iter()
        .map(|a| sh_quote(a))
        .collect::<Vec<_>>()
        .join(" ");
    let env: String = check
        .env
        .iter()
        .map(|(k, v)| format!(" --setenv {} {}", sh_quote(k), sh_quote(v)))
        .collect();
    let binds: String = check
        .binds
        .iter()
        .map(|(from, to)| format!(" --ro-bind {} {}", sh_quote(from), sh_quote(to)))
        .collect();
    format!(
        "timeout -k 5 600 bwrap --unshare-all --unshare-pid --die-with-parent --new-session \
         --clearenv --ro-bind /usr /usr --ro-bind-try /bin /bin --ro-bind-try /lib /lib \
         --ro-bind-try /lib64 /lib64 --ro-bind /etc /etc --proc /proc --dev /dev --tmpfs /tmp \
         --ro-bind {entry} /cli --ro-bind-try {source} /skein-src{binds} --chdir /tmp \
         --setenv HOME /tmp --setenv TMPDIR /tmp --setenv PATH /cli/bin:/usr/local/bin:/usr/bin:/bin \
         --setenv DISABLE_AUTOUPDATER 1{env} -- {argv} 2>&1",
        entry = sh_quote(entry_dir),
        source = sh_quote(source),
    )
}

/// Does the entry look like the prefix the launcher projects: `bin/<command>` a link into
/// `lib/node_modules/<package>/`?
fn cli_entry_shape(entry_dir: &Path, cli: &Cli) -> Result<(), String> {
    let link = entry_dir.join("bin").join(cli.command);
    let target = std::fs::read_link(&link)
        .map_err(|_| format!("bin/{} is not a link npm made", cli.command))?;
    let want = format!("../lib/node_modules/{}/", cli.package);
    let t = target.to_string_lossy();
    if !t.starts_with(&want) || t.contains("/../") {
        return Err(format!(
            "bin/{} leads to {t}, not into {}",
            cli.command, cli.package
        ));
    }
    if !entry_dir
        .join("lib/node_modules")
        .join(cli.package)
        .is_dir()
    {
        return Err(format!("the entry has no lib/node_modules/{}", cli.package));
    }
    Ok(())
}

/// The TypeScript entry a test of `cli` is run with: the pinned one for Claude Code, whose mod's
/// check type-checks with it, and none for any other CLI.
fn typescript_for(cli: &Cli) -> Option<String> {
    (cli.runtime == "claude").then(typescript_entry)
}

/// Run every check for a published CLI entry, and record the outcome. A check passes when it exits
/// 0; the version check also needs the entry's version in what the binary said.
///
/// A claude-code entry is not tested at all until the pinned TypeScript is published: its mod's
/// check would only say a tool is missing, and that is no verdict on the build, so nothing is
/// recorded and the next pass tries again.
pub fn test_cli(sandbox: &str, entry: &str) -> Result<Tested, String> {
    let (cli, version) =
        cli_entry_parts(entry).ok_or_else(|| format!("{entry:?} is not a CLI and a version"))?;
    if !depot::is_published(&AGENT_CLI, entry) {
        return Err(format!("{entry} is not in the fleet's agent-cli store"));
    }
    let typescript = typescript_for(cli);
    if let Some(ts) = typescript
        .as_deref()
        .filter(|ts| !depot::is_published(&TYPESCRIPT, ts))
    {
        return Err(format!(
            "{entry} waits for {ts}, which its mod's check needs and the fleet's store does not \
             have yet"
        ));
    }
    let ts_dir = depot::kind_dir(&TYPESCRIPT).join(typescript.as_deref().unwrap_or_default());
    let dir = depot::kind_dir(&AGENT_CLI).join(entry);
    let mut tested = Tested {
        ok: true,
        at: chrono::Utc::now().timestamp(),
        checks: Vec::new(),
        typescript,
    };
    if let Err(why) = cli_entry_shape(&dir, cli) {
        tested.ok = false;
        tested.checks.push(CheckResult {
            name: "shape".into(),
            ok: false,
            said: why,
        });
    } else {
        let source = skein_source_path();
        for check in checks_for(cli, version, &ts_dir) {
            let ran = crate::place::own_sandbox(sandbox).attempt(
                &check_script(&dir.display().to_string(), &source, &check),
                &[],
                Duration::from_secs(660),
            );
            let (ok, said) = match ran {
                Ok(r) => {
                    let said = String::from_utf8_lossy(&r.out).trim().to_string();
                    let ok = r.code == 0 && (check.name != "version" || said.contains(version));
                    let lines: Vec<&str> = said.lines().collect();
                    let tail = lines[lines.len().saturating_sub(8)..].join("\n");
                    (ok, tail)
                }
                Err(e) => (false, e),
            };
            tested.ok &= ok;
            tested.checks.push(CheckResult {
                name: check.name.into(),
                ok,
                said,
            });
        }
    }
    let record = tested.clone();
    update_rollouts(|r| {
        r.tested.insert(entry.to_string(), record);
        Ok(())
    })?;
    Ok(tested)
}

/// Fill `entry` of `kind` with `fill`, recording a failure for the cockpit.
pub fn fill_entry(
    sandbox: &str,
    kind: &Kind,
    entry: &str,
    fill: &depot::Fill,
) -> Result<(), String> {
    if depot::is_published(kind, entry) {
        return Ok(());
    }
    let outcome = depot::fill_with(sandbox, kind, fill).and_then(|(staged, _)| {
        staged
            .entries
            .iter()
            .any(|e| e == entry)
            .then_some(())
            .ok_or_else(|| {
                format!(
                    "the fetch staged {} and not {entry}",
                    staged.entries.join(", ")
                )
            })
    });
    let failed = outcome.as_ref().err().cloned();
    update_rollouts(|r| {
        match &failed {
            Some(why) => r.failed.insert(entry.to_string(), why.clone()),
            None => r.failed.remove(entry),
        };
        Ok(())
    })?;
    outcome
}

/// The newest version of each CLI on the npm registry: `npm view <package> version`, which reads
/// the registry's metadata and runs nothing of the package's.
fn upstream_versions(sandbox: &str) -> BTreeMap<String, String> {
    let mut found = BTreeMap::new();
    for c in CLIS {
        let script = format!(
            "timeout 60 npm view {} version 2>/dev/null",
            sh_quote(c.package)
        );
        if let Ok(out) = crate::place::own_sandbox(sandbox).exec(&script, Duration::from_secs(90)) {
            let v = out.trim();
            if version_is_nameable(v) {
                found.insert(c.runtime.to_string(), v.to_string());
            }
        }
    }
    found
}

/// **The automatic half of a CLI update** (the owner's decision 1): ask the registry, fill every
/// newest version the depot lacks and the pinned TypeScript if it lacks that, and test every entry
/// not yet tested ([`test_pass`]). Rolls nothing out.
/// Returns a line for each thing it did, for the server's log.
pub fn watch_pass(sandbox: &str) -> Vec<String> {
    let mut said = Vec::new();
    depot::sweep_staging(&AGENT_CLI);
    depot::sweep_staging(&RUST_TOOLCHAIN);
    depot::sweep_staging(&TYPESCRIPT);
    let upstream = upstream_versions(sandbox);
    let _ = update_rollouts(|r| {
        r.upstream.extend(upstream.clone());
        Ok(())
    });
    // The version each CLI is fetched at: its pin when it has one (SKEIN-1380), npm's newest
    // otherwise. A pinned CLI's newer releases are only shown.
    let mut wanted: BTreeMap<String, String> = upstream.clone();
    for c in CLIS {
        if let Some(pin) = pinned(c.runtime) {
            wanted.insert(c.runtime.to_string(), pin.to_string());
        }
    }
    for (runtime, version) in &wanted {
        let entry = format!("{runtime}@{version}");
        if depot::is_published(&AGENT_CLI, &entry) {
            continue;
        }
        match cli_fill(&entry).and_then(|f| fill_entry(sandbox, &AGENT_CLI, &entry, &f)) {
            Ok(()) => said.push(format!("fetched {entry} for the fleet")),
            Err(e) => said.push(format!(
                "could not fetch {entry}: {}",
                e.lines().last().unwrap_or("")
            )),
        }
    }
    // The TypeScript claude-code's mod is type-checked with (SKEIN-1396), before anything is
    // tested with it.
    let ts = typescript_entry();
    if !depot::is_published(&TYPESCRIPT, &ts) {
        match typescript_fill(&ts).and_then(|f| fill_entry(sandbox, &TYPESCRIPT, &ts, &f)) {
            Ok(()) => said.push(format!("fetched {ts} for the mod's check")),
            Err(e) => said.push(format!(
                "could not fetch {ts}: {}",
                e.lines().last().unwrap_or("")
            )),
        }
    }
    said.extend(test_pass(sandbox));
    said
}

/// Test every published CLI entry that has no test record, or whose record is from a TypeScript
/// other than the one it would be tested with now. Returns a line for each.
pub fn test_pass(sandbox: &str) -> Vec<String> {
    let mut said = Vec::new();
    let tested = rollouts().tested;
    for entry in depot::published(&AGENT_CLI) {
        let Some((cli, _)) = cli_entry_parts(&entry) else {
            continue;
        };
        if tested
            .get(&entry)
            .is_some_and(|t| t.typescript == typescript_for(cli))
        {
            continue;
        }
        match test_cli(sandbox, &entry) {
            Ok(t) if t.ok => said.push(format!("{entry} passed its tests; it can be rolled out")),
            Ok(_) => said.push(format!("{entry} failed its tests")),
            Err(e) => said.push(format!("could not test {entry}: {e}")),
        }
    }
    said
}

/// Fill the toolchain any placed box's repo names that the depot lacks, one per pass, whether or
/// not the fleet has a default rolled out: the launcher binds a repo's entry beside the
/// bootstrap's toolchain as well as beside a default (SKEIN-1415). A fill that failed is not tried
/// again; its reason is kept in [`Rollouts::failed`].
pub fn serve_repo_toolchains(sandbox: &str) -> Vec<String> {
    let failed = rollouts().failed;
    let mut wants: BTreeSet<String> = BTreeSet::new();
    for (name, _) in crate::place::placed_boxes(sandbox) {
        if let Some(t) = repo_toolchain(&Path::new(&box_root(&name)).join("tree")) {
            wants.insert(t);
        }
    }
    for want in wants {
        if depot::is_published(&RUST_TOOLCHAIN, &want) || failed.contains_key(&want) {
            continue;
        }
        return vec![match toolchain_fill(&want)
            .and_then(|f| fill_entry(sandbox, &RUST_TOOLCHAIN, &want, &f))
        {
            Ok(()) => format!("fetched Rust {want} for the fleet"),
            Err(e) => format!(
                "could not fetch Rust {want}: {}",
                e.lines().last().unwrap_or("")
            ),
        }];
    }
    Vec::new()
}

/// Publish the toolchain `bootstrap.sh` installed as the depot's first Rust entry, named for the
/// version its `rustc` reports, so it can be rolled out as the fleet's default (§9 M3 of the
/// design). A copy, made in the depot's namespace from the toolchain bound read-only.
pub fn adopt_bootstrap_toolchain(sandbox: &str) -> Result<String, String> {
    let triple = host_triple();
    let from = format!(
        "{}/rustup/toolchains/stable-{triple}",
        skein_toolchain_path()
    );
    let said = crate::place::own_sandbox(sandbox)
        .exec(
            &format!("{}/bin/rustc --version", sh_quote(&from)),
            Duration::from_secs(30),
        )
        .map_err(|e| format!("the fleet's toolchain does not answer: {e}"))?;
    let version = said
        .split_whitespace()
        .nth(1)
        .filter(|v| version_is_nameable(v))
        .ok_or_else(|| format!("cannot read a version from {said:?}"))?;
    let name = format!("{version}-{triple}");
    let fill = depot::Fill {
        fetch: vec![
            "cp".into(),
            "-a".into(),
            "--".into(),
            "/adopt".into(),
            format!("/stage/out/{name}"),
        ],
        env: Vec::new(),
        read_only: vec![(from, "/adopt".into())],
        out: "out".into(),
    };
    fill_entry(sandbox, &RUST_TOOLCHAIN, &name, &fill)?;
    Ok(name)
}

// ------------------------------------------------------------------------------------------------
// The view, and the clean
// ------------------------------------------------------------------------------------------------

/// Every entry a placed box records, running or stopped, plus each component's current and previous.
/// A stopped box starts on the current entry, but its record is kept as a use until it does, so a
/// clean never removes something a person could still be looking at.
pub fn kept(kind: &Kind) -> Result<BTreeSet<String>, String> {
    let mut keep = BTreeSet::new();
    let r = rollouts();
    for (component, rolled) in &r.current {
        if kind_of(component).is_some_and(|k| k.name == kind.name) {
            keep.insert(rolled.entry.clone());
            keep.extend(rolled.previous.clone());
        }
    }
    let places = crate::config::skein_home().join("places");
    for item in std::fs::read_dir(&places).into_iter().flatten().flatten() {
        let file = item.file_name().to_string_lossy().into_owned();
        let Some(name) = file.strip_suffix(".json") else {
            continue;
        };
        let Some(record) = crate::place::shared_record(name) else {
            return Err(format!(
                "skein cannot read {name}'s placement record, so it cannot say which versions \
                 {name} uses"
            ));
        };
        keep.extend(record.versions.unwrap_or_default().into_values());
    }
    Ok(keep)
}

/// One entry, for the cockpit.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct EntryView {
    pub name: String,
    pub bytes: u64,
    /// The boxes whose placement record names it.
    pub boxes: Vec<String>,
    pub tested: Option<Tested>,
    pub current: bool,
    pub previous: bool,
    /// Whether the clean would remove it now.
    pub removable: bool,
}

/// One component, for the cockpit.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ComponentView {
    pub component: String,
    pub kind: String,
    pub current: Option<String>,
    pub previous: Option<String>,
    /// The newest version upstream, for a CLI.
    pub upstream: Option<String>,
    /// The only version that can be rolled out, for a pinned CLI (`src/claude-code.version`).
    pub pinned: Option<String>,
    pub entries: Vec<EntryView>,
    /// Running boxes still on another entry, which move when they are idle.
    pub waiting_to_restart: Vec<String>,
}

/// What the depot holds per component, who uses what, and what can be rolled out or removed.
pub fn view() -> Vec<ComponentView> {
    let r = rollouts();
    let mut users: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut behind: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (name, record) in crate::place::placed_boxes(&crate::place::fleet_sandbox()) {
        for entry in record.versions.clone().unwrap_or_default().into_values() {
            users.entry(entry).or_default().push(name.clone());
        }
        if running_record(&name).is_some() {
            for p in pending_for(&record, &name) {
                let c = if p.component == RUST_REPO {
                    RUST
                } else {
                    &p.component
                };
                behind.entry(c.to_string()).or_default().push(name.clone());
            }
        }
    }
    let kept_cli = kept(&AGENT_CLI).ok();
    let kept_rust = kept(&RUST_TOOLCHAIN).ok();
    let mut out = Vec::new();
    let components: Vec<&str> = CLIS.iter().map(|c| c.runtime).chain([RUST]).collect();
    for component in components {
        let kind = kind_of(component).expect("a component");
        let kept = if kind.name == AGENT_CLI.name {
            &kept_cli
        } else {
            &kept_rust
        };
        let rolled = r.current.get(component);
        let entries = depot::published(kind)
            .into_iter()
            .filter(|e| match cli_entry_parts(e) {
                Some((c, _)) => c.runtime == component,
                None => component == RUST,
            })
            .map(|name| EntryView {
                bytes: depot::entry_bytes(kind, &name),
                boxes: users.get(&name).cloned().unwrap_or_default(),
                tested: r.tested.get(&name).cloned(),
                current: rolled.is_some_and(|c| c.entry == name),
                previous: rolled.is_some_and(|c| c.previous.as_deref() == Some(name.as_str())),
                removable: kept.as_ref().is_some_and(|k| !k.contains(&name)),
                name,
            })
            .collect();
        out.push(ComponentView {
            component: component.to_string(),
            kind: kind.name.to_string(),
            current: rolled.map(|c| c.entry.clone()),
            previous: rolled.and_then(|c| c.previous.clone()),
            upstream: r.upstream.get(component).cloned(),
            pinned: pinned(component).map(str::to_string),
            entries,
            waiting_to_restart: behind.remove(component).unwrap_or_default(),
        });
    }
    out
}

/// The TypeScript skein's mod is type-checked with, for the cockpit: read-only, since it is moved
/// only by a reviewed change to `src/typescript.version`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct TypeScriptView {
    /// `typescript@<pin>`.
    pub entry: String,
    /// Whether the fleet's store has it yet.
    pub published: bool,
}

/// The pinned TypeScript, and whether the depot has it.
pub fn typescript_view() -> TypeScriptView {
    let entry = typescript_entry();
    TypeScriptView {
        published: depot::is_published(&TYPESCRIPT, &entry),
        entry,
    }
}

/// Remove entries the person confirmed, each only if nothing keeps it at the moment of removal.
pub fn clean(kind_name: &str, names: &[String]) -> Result<depot::Cleaned, String> {
    let kind = [&AGENT_CLI, &RUST_TOOLCHAIN]
        .into_iter()
        .find(|k| k.name == kind_name)
        .ok_or_else(|| format!("{kind_name:?} is not a kind of version skein keeps"))?;
    let done = depot::remove_unused(kind, names, || kept(kind))?;
    let _ = update_rollouts(|r| {
        for n in names {
            r.tested.remove(n);
        }
        Ok(())
    });
    Ok(done)
}

// ------------------------------------------------------------------------------------------------
// Restart to apply
// ------------------------------------------------------------------------------------------------

/// How long a box must have been idle, by the fallback reading, before skein restarts it onto a
/// rollout on its own. Long enough that a person reading the box's last answer is not cut off.
pub const IDLE_GRACE: Duration = Duration::from_secs(10 * 60);

/// Where a box asks to be restarted onto a rollout once its turn ends: one file in its own package
/// drop-box, the one path under `.skein` the box can write (`substrate::box_requests_dir`). Not a
/// `.json`, so the package queue never reads it. Requested state: its presence is all it says.
pub fn restart_request_path(name: &str) -> PathBuf {
    Path::new(&crate::substrate::box_requests_dir(name)).join("restart-when-idle")
}

/// Has box `name` asked? Only a plain file counts; a link the box planted there is not an ask.
pub fn restart_requested(name: &str) -> bool {
    valid_name(name)
        && std::fs::symlink_metadata(restart_request_path(name)).is_ok_and(|m| m.is_file())
}

/// A box as the restart pass needs it: its name and its fused turn state as the board reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoxNow {
    pub name: String,
    pub state: String,
}

/// When each box was first seen idle in the current stretch, so the grace is a stretch of idleness
/// and not one reading.
#[derive(Debug, Default)]
pub struct IdleMemory(std::sync::Mutex<BTreeMap<String, std::time::Instant>>);

/// **The restart rule** (the owner's decision 2): a running box that is behind on a rolled-out
/// version is restarted onto it when, and only when, it is idle (`fleet::idle`), at the moment of
/// the restart as well as before.
///
/// A box that asked ([`restart_requested`]) is restarted at its first idle reading; one that did
/// not, after [`IDLE_GRACE`] of continuous idleness. A box that is working, waiting on a person, or
/// whose state skein cannot read is never restarted, and its idle stretch starts again.
///
/// `stale_of`, `idle_now` and `restart` are arguments so a test can stand in for the board and the
/// box; the server hands it `why_cover_is_stale`, the board's state read again, and the whole-box
/// restart (`skein restart`'s own path, then the agent reopened on its conversation). Returns what it did, for the server's log.
pub fn restart_onto_rollouts(
    boxes: &[BoxNow],
    now: std::time::Instant,
    memory: &IdleMemory,
    stale_of: impl Fn(&str) -> Vec<&'static str>,
    idle_now: impl Fn(&str) -> super::idle::Idle,
    restart: impl Fn(&str) -> Result<(), String>,
) -> Vec<String> {
    use super::idle::Idle;
    let mut said = Vec::new();
    let mut since = memory.0.lock().unwrap_or_else(|e| e.into_inner());
    since.retain(|n, _| boxes.iter().any(|b| &b.name == n));
    for b in boxes {
        let behind = stale_of(&b.name)
            .into_iter()
            .any(|k| VERSION_KEYS.contains(&k));
        if !behind || super::idle::idle(None, &b.state) != Idle::Idle {
            since.remove(&b.name);
            continue;
        }
        let first = *since.entry(b.name.clone()).or_insert(now);
        let asked = restart_requested(&b.name);
        if !asked && now.saturating_duration_since(first) < IDLE_GRACE {
            continue;
        }
        // Read again at the moment of the restart: the reading above is up to a minute old, and a
        // box that started working since is exactly the one this must not cut off.
        if idle_now(&b.name) != Idle::Idle {
            since.remove(&b.name);
            continue;
        }
        since.remove(&b.name);
        match restart(&b.name) {
            Ok(()) => {
                let _ = std::fs::remove_file(restart_request_path(&b.name));
                said.push(format!(
                    "restarted {} onto the rolled-out versions, {}",
                    b.name,
                    if asked {
                        "as it asked"
                    } else {
                        "after it was idle"
                    }
                ));
            }
            Err(e) => said.push(format!(
                "could not restart {} onto the rollout: {e}",
                b.name
            )),
        }
    }
    said
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{env_lock, env_pins, tempdir};

    /// **Only a CLI and a plain version, or a pinned toolchain and a triple, is an entry.**
    ///
    /// What would make it fail: `version_is_nameable` accepting a pre-release, a leading zero or a
    /// fourth part; `cli_entry_parts` accepting a runtime skein does not manage;
    /// `toolchain_is_nameable` accepting a moving channel (`stable-…`) or a path.
    #[test]
    fn only_a_known_cli_and_a_plain_version_or_a_pinned_toolchain_is_an_entry() {
        for good in ["claude@2.1.314", "codex@0.170.0", "claude@0.0.1"] {
            assert!(cli_entry_is_nameable(good), "{good}");
        }
        for bad in [
            "claude",
            "claude@",
            "claude@2.1",
            "claude@2.1.314.1",
            "claude@02.1.3",
            concat!("claude@2.1.3-", "beta"),
            "gemini@1.0.0",
            "claude@2.1.3/..",
            "../claude@2.1.3",
            "claude@latest",
        ] {
            assert!(!cli_entry_is_nameable(bad), "{bad:?}");
        }
        for good in [
            "1.98.0-aarch64-unknown-linux-gnu",
            "1.97.0-x86_64-unknown-linux-gnu",
            "nightly-2026-08-01-x86_64-unknown-linux-gnu",
            "beta-2026-08-01-aarch64-unknown-linux-musl",
        ] {
            assert!(toolchain_is_nameable(good), "{good}");
        }
        for bad in [
            "stable-aarch64-unknown-linux-gnu",
            "nightly-aarch64-unknown-linux-gnu",
            "1.98-aarch64-unknown-linux-gnu",
            "1.98.0",
            "1.98.0-../../etc",
            "1.98.0-Aarch64-unknown-linux-gnu",
            "nightly-2026-8-01-x86_64-unknown-linux-gnu",
            "2.0.0-x86_64-unknown-linux-gnu",
        ] {
            assert!(!toolchain_is_nameable(bad), "{bad:?}");
        }
    }

    /// **The launcher's report is read from its first line, and only known keys with an entry's
    /// shape survive.**
    ///
    /// What would make it fail: reading the last line (the box's profile prints after the
    /// launcher); keeping a key skein does not know, or a value that is a path.
    #[test]
    fn what_a_box_bound_is_read_from_the_launchers_own_line() {
        let out = "SKEIN_ANCHOR 1\nSKEIN_VERSIONS claude=claude@2.1.314 rust=1.98.0-aarch64-unknown-linux-gnu evil=/etc codex=claude@1.0.0\nSKEIN_VERSIONS claude=claude@9.9.9\n";
        let got = versions_from_launch(out).expect("the launcher said");
        assert_eq!(
            got,
            BTreeMap::from([
                ("claude".to_string(), "claude@2.1.314".to_string()),
                (
                    "rust".to_string(),
                    "1.98.0-aarch64-unknown-linux-gnu".to_string()
                ),
            ])
        );
        assert_eq!(versions_from_launch("SKEIN_ANCHOR 1\n"), None);
        assert_eq!(
            versions_from_launch("SKEIN_VERSIONS\n"),
            Some(BTreeMap::new())
        );
        assert_eq!(versions_from_launch("SKEIN_VERSIONSX a=b\n"), None);
    }

    /// **A moved CLI is `agent-cli`, a moved default or repo toolchain is `rust-toolchain`, and
    /// nothing moved is nothing.**
    ///
    /// What would make it fail: `stale_keys` comparing only keys present on one side (a component
    /// rolled out for the first time would not show), or forgetting `rust-repo`.
    #[test]
    fn a_moved_version_is_named_by_its_own_key() {
        let m = |pairs: &[(&str, &str)]| -> BTreeMap<String, String> {
            pairs
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect()
        };
        let born = m(&[("claude", "claude@1.0.0"), ("rust", "1.97.0-x")]);
        assert!(stale_keys(&born, &born).is_empty());
        assert_eq!(
            stale_keys(
                &born,
                &m(&[("claude", "claude@1.0.1"), ("rust", "1.97.0-x")])
            ),
            ["agent-cli"]
        );
        assert_eq!(
            stale_keys(
                &born,
                &m(&[
                    ("claude", "claude@1.0.0"),
                    ("codex", "codex@1.0.0"),
                    ("rust", "1.97.0-x")
                ])
            ),
            ["agent-cli"]
        );
        assert_eq!(
            stale_keys(
                &born,
                &m(&[
                    ("claude", "claude@1.0.0"),
                    ("rust", "1.97.0-x"),
                    ("rust-repo", "1.96.0-x")
                ])
            ),
            ["rust-toolchain"]
        );
    }

    /// **A repo's toolchain file is read as data, and only a pinned channel is used.**
    ///
    /// What would make it fail: following a link at the file's name; taking `channel` from another
    /// table; accepting `stable` (then the default never applies); dropping the size bound.
    #[test]
    fn a_repos_toolchain_file_names_its_toolchain_and_nothing_else_does() {
        let dir = tempdir();
        let t = host_triple();
        let tree = |files: &[(&str, &str)]| {
            let d = dir.join(format!("t{}", rand_suffix()));
            std::fs::create_dir_all(&d).unwrap();
            for (f, body) in files {
                std::fs::write(d.join(f), body).unwrap();
            }
            d
        };
        let toml = tree(&[(
            "rust-toolchain.toml",
            "[other]\nchannel = \"1.1.1\"\n[toolchain]\n# pinned\nchannel = \"1.97.0\" # here\ncomponents = [\"clippy\"]\n",
        )]);
        assert_eq!(repo_toolchain(&toml), Some(format!("1.97.0-{t}")));
        let legacy = tree(&[("rust-toolchain", "nightly-2026-08-01\n")]);
        assert_eq!(
            repo_toolchain(&legacy),
            Some(format!("nightly-2026-08-01-{t}"))
        );
        let full = tree(&[(
            "rust-toolchain.toml",
            &format!("[toolchain]\nchannel = \"1.97.0-{t}\"\n"),
        )]);
        assert_eq!(repo_toolchain(&full), Some(format!("1.97.0-{t}")));
        for moving in ["stable", "nightly", "1.80", "1.97.0; rm -rf /"] {
            let d = tree(&[(
                "rust-toolchain.toml",
                &format!("[toolchain]\nchannel = \"{moving}\"\n"),
            )]);
            assert_eq!(repo_toolchain(&d), None, "{moving}");
        }
        let big = tree(&[(
            "rust-toolchain.toml",
            &format!("[toolchain]\nchannel = \"1.97.0\"\n{}", "#".repeat(5000)),
        )]);
        assert_eq!(repo_toolchain(&big), None);
        let linked = tree(&[]);
        std::os::unix::fs::symlink(
            toml.join("rust-toolchain.toml"),
            linked.join("rust-toolchain.toml"),
        )
        .unwrap();
        assert_eq!(repo_toolchain(&linked), None, "a link was followed");
        assert_eq!(repo_toolchain(&tree(&[])), None);
    }

    /// A tree holding one `rust-toolchain.toml` with `body`.
    fn toml_tree(dir: &Path, body: &str) -> PathBuf {
        let d = dir.join(format!("t{}", rand_suffix()));
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("rust-toolchain.toml"), body).unwrap();
        d
    }

    /// **A toolchain entry is named for everything it holds: the toolchain, then the components and
    /// targets beyond the baseline, in one spelling per set** (SKEIN-1415).
    ///
    /// What would make it fail: `repo_toolchain` reading only the channel (synapse's file names
    /// `1.98.1-<triple>` and the box gets no wasm32); `toolchain_entry` not sorting or not dropping
    /// repeats (the two spellings of one set are two entries); not dropping the baseline or the host
    /// target (a file naming only clippy and rustfmt is no longer the plain entry a default is);
    /// `toml_value` not joining a multi-line array.
    #[test]
    fn a_toolchain_entry_is_named_for_everything_it_holds() {
        let dir = tempdir();
        let t = host_triple();
        let synapse = toml_tree(
            &dir,
            "[toolchain]\nchannel = \"1.98.1\"\ncomponents = [\"clippy\", \"rustfmt\"]\ntargets = [\"wasm32-unknown-unknown\"]\n",
        );
        assert_eq!(
            repo_toolchain(&synapse),
            Some(format!("1.98.1-{t}@wasm32-unknown-unknown"))
        );
        let other_target = toml_tree(
            &dir,
            "[toolchain]\nchannel = \"1.98.1\"\ntargets = [\"thumbv8m.main-none-eabi\"]\n",
        );
        assert_eq!(
            repo_toolchain(&other_target),
            Some(format!("1.98.1-{t}@thumbv8m.main-none-eabi"))
        );
        let one = toml_tree(
            &dir,
            &format!(
                "[toolchain]\nchannel = \"1.98.1\"\ncomponents = [\n  \"rust-src\", # for the analyzer\n  'llvm-tools',\n  \"clippy\",\n]\ntargets = [\"wasm32-unknown-unknown\", \"{t}\", \"x86_64-unknown-linux-musl\"]\n"
            ),
        );
        let same = toml_tree(
            &dir,
            "[toolchain]\nchannel = \"1.98.1\"\ntargets = [\"x86_64-unknown-linux-musl\", \"wasm32-unknown-unknown\", \"wasm32-unknown-unknown\"]\ncomponents = [\"llvm-tools\", \"rust-src\", \"llvm-tools\"]\n",
        );
        let named = format!(
            "1.98.1-{t}+llvm-tools+rust-src@wasm32-unknown-unknown@x86_64-unknown-linux-musl"
        );
        assert_eq!(repo_toolchain(&one), Some(named.clone()));
        assert_eq!(repo_toolchain(&same), Some(named.clone()));
        let parts = toolchain_entry_parts(&named).expect("the name reads back");
        assert_eq!(parts.toolchain, format!("1.98.1-{t}"));
        assert_eq!(parts.components, ["llvm-tools", "rust-src"]);
        assert_eq!(
            parts.targets,
            ["wasm32-unknown-unknown", "x86_64-unknown-linux-musl"]
        );
        let baseline = toml_tree(
            &dir,
            &format!("[toolchain]\nchannel = \"1.98.1\"\ncomponents = [\"clippy\", \"rustfmt\", \"rust-std\"]\ntargets = [\"{t}\"]\n"),
        );
        assert_eq!(repo_toolchain(&baseline), Some(format!("1.98.1-{t}")));
    }

    /// **A toolchain file with one name that is not the shape of one pins nothing, and no entry
    /// name but the canonical one is an entry.** The file is in the box's tree, so every name in it
    /// is the box's say.
    ///
    /// What would make it fail: `target_is_nameable` or `component_is_nameable` letting a path, a
    /// space, a shell word, a capital or `..` through; `toml_list` skipping an item it cannot read
    /// instead of refusing the file; dropping `MAX_EXTRAS`; `toolchain_entry_parts` accepting an
    /// unsorted list, a baseline component, the host target or an empty list (two names for one
    /// set, or a name the launcher would bind at a path skein never fetched).
    #[test]
    fn a_hostile_toolchain_file_pins_nothing() {
        let dir = tempdir();
        let t = host_triple();
        for targets in [
            "[\"../../etc\"]",
            "[\"wasm32/unknown-unknown\"]",
            "[\"wasm32-unknown..-unknown\"]",
            "[\"wasm32-unknown-unknown; rm -rf ~\"]",
            "[\"$(id)-unknown-unknown\"]",
            "[\"WASM32-unknown-unknown\"]",
            "[\"wasm32 unknown unknown\"]",
            "[\"wasm32\"]",
            "[\"wasm32-unknown-unknown\", 7]",
            "[\"wasm32-unknown-unknown\"",
            "\"wasm32-unknown-unknown\"",
            "[\"a+b-c-d\"]",
            "[\"a@b-c-d\"]",
            "[\"t0-a-b\", \"t1-a-b\", \"t2-a-b\", \"t3-a-b\", \"t4-a-b\", \"t5-a-b\", \"t6-a-b\", \"t7-a-b\", \"t8-a-b\"]",
        ] {
            let d = toml_tree(
                &dir,
                &format!("[toolchain]\nchannel = \"1.98.1\"\ntargets = {targets}\n"),
            );
            assert_eq!(repo_toolchain(&d), None, "targets = {targets}");
        }
        for components in [
            "[\"clippy;id\"]",
            "[\"rust src\"]",
            "[\"Rust-src\"]",
            "[\"-src\"]",
            "[\"rust--src\"]",
            "[\"../x\"]",
            "[\"a+b\"]",
        ] {
            let d = toml_tree(
                &dir,
                &format!("[toolchain]\nchannel = \"1.98.1\"\ncomponents = {components}\n"),
            );
            assert_eq!(repo_toolchain(&d), None, "components = {components}");
        }
        let base = format!("1.98.1-{t}");
        for bad in [
            format!("{base}@x86_64-unknown-linux-musl@wasm32-unknown-unknown"),
            format!("{base}@wasm32-unknown-unknown@wasm32-unknown-unknown"),
            format!("{base}+rust-src+llvm-tools"),
            format!("{base}+clippy"),
            format!("{base}@{t}"),
            format!("{base}@"),
            format!("{base}+"),
            format!("{base}+@wasm32-unknown-unknown"),
            format!("{base}@wasm32-unknown-unknown/.."),
            format!("{base}@..-a"),
            format!("stable-{t}@wasm32-unknown-unknown"),
            // Each target the right shape, the whole past the 240 bytes a name may be.
            format!(
                "{base}{}",
                (0..4)
                    .map(|i| format!("@t{i}-{}", "a".repeat(55)))
                    .collect::<String>()
            ),
        ] {
            assert!(!toolchain_is_nameable(&bad), "{bad:?}");
            assert!(toolchain_fill(&bad).is_err(), "{bad:?} was filled");
        }
    }

    /// **The fill installs the entry's toolchain with clippy, rustfmt and every component and
    /// target its name lists, and names the result for the entry.**
    ///
    /// What would make it fail: `toolchain_fill` dropping `--target` or the entry's components (the
    /// wasm32 entry is filled without wasm32, the bug SKEIN-1415 was filed for); not moving rustup's
    /// directory to the entry's name (the staged name is the toolchain's, and `fill_entry` refuses
    /// it as "staged … and not …"); passing a name inside the shell script rather than after it.
    #[test]
    fn the_fill_installs_what_the_entry_is_named_for() {
        let _g = env_lock();
        let dir = tempdir();
        let mut pins = env_pins();
        pins.set("SKEIN_FLEET_ROOT", dir.join("fleet"))
            .set("SKEIN_HOME", dir.join("home"));
        let t = host_triple();
        let script = r#"set -e; r=$1; e=$2; t=$3; shift 3; "$r" toolchain install "$t" "$@"; mv -- "/stage/rh/toolchains/$t" "/stage/out/$e""#;
        let entry =
            format!("1.98.1-{t}+llvm-tools@wasm32-unknown-unknown@x86_64-unknown-linux-musl");
        let fill = toolchain_fill(&entry).expect("a canonical entry is filled");
        assert_eq!(
            fill.fetch,
            [
                "/bin/sh",
                "-c",
                script,
                "sh",
                "/opt/skein-rustup/rustup",
                entry.as_str(),
                &format!("1.98.1-{t}"),
                "--profile",
                "minimal",
                "--component",
                "clippy,rustfmt,llvm-tools",
                "--target",
                "wasm32-unknown-unknown,x86_64-unknown-linux-musl",
                "--no-self-update",
            ]
        );
        assert_eq!(fill.out, "out");
        let plain = toolchain_fill(&format!("1.98.1-{t}")).expect("a plain entry is filled");
        assert_eq!(
            plain.fetch[5..],
            [
                format!("1.98.1-{t}"),
                format!("1.98.1-{t}"),
                "--profile".into(),
                "minimal".into(),
                "--component".into(),
                "clippy,rustfmt".into(),
                "--no-self-update".into(),
            ]
        );
    }

    /// **A repo's pin is handed to its boxes with no fleet default rolled out, as `rust-repo` when
    /// the depot has it and as `rust-repo-missing` when it does not; and a box told it is missing
    /// is stale once it arrives.**
    ///
    /// What would make it fail: `wanted_with` looking at the repo's file only inside `if let
    /// Some(default)` (the SKEIN-1415 gap: nothing comes back without a default); handing over an
    /// unpublished entry as `rust-repo` (the launcher binds nothing and the record says it did);
    /// `stale_keys` not comparing `rust-repo-missing`; `versions_from_launch` dropping the key (a
    /// box told of a missing toolchain would read as stale for ever, and be restarted each idle).
    #[test]
    fn a_repos_pin_is_handed_over_without_a_fleet_default() {
        let _g = env_lock();
        let dir = tempdir();
        let mut pins = env_pins();
        pins.set("SKEIN_FLEET_ROOT", dir.join("fleet"))
            .set("SKEIN_HOME", dir.join("home"));
        let t = host_triple();
        let tree = toml_tree(
            &dir,
            "[toolchain]\nchannel = \"1.98.1\"\ntargets = [\"wasm32-unknown-unknown\"]\n",
        );
        let entry = format!("1.98.1-{t}@wasm32-unknown-unknown");
        let none = Rollouts::default();
        let missing = wanted_with(&none, &tree);
        assert_eq!(
            missing,
            BTreeMap::from([(RUST_REPO_MISSING.to_string(), entry.clone())])
        );
        publish_fixture(&RUST_TOOLCHAIN, &entry);
        let bound = wanted_with(&none, &tree);
        assert_eq!(
            bound,
            BTreeMap::from([(RUST_REPO.to_string(), entry.clone())])
        );
        assert_eq!(stale_keys(&missing, &bound), ["rust-toolchain"]);
        // The same toolchain without wasm32 as the default is another entry, so the repo's is
        // still handed over beside it.
        let plain = format!("1.98.1-{t}");
        publish_fixture(&RUST_TOOLCHAIN, &plain);
        let mut with_default = Rollouts::default();
        with_default.current.insert(
            RUST.into(),
            Rolled {
                entry: plain.clone(),
                ..Default::default()
            },
        );
        assert_eq!(
            wanted_with(&with_default, &tree),
            BTreeMap::from([
                (RUST.to_string(), plain.clone()),
                (RUST_REPO.to_string(), entry.clone()),
            ])
        );
        with_default.current.get_mut(RUST).unwrap().entry = entry.clone();
        assert_eq!(
            wanted_with(&with_default, &tree),
            BTreeMap::from([(RUST.to_string(), entry.clone())])
        );
        let said = format!("SKEIN_VERSIONS rust-repo-missing={entry}\n");
        assert_eq!(versions_from_launch(&said), Some(missing));
    }

    fn rand_suffix() -> u64 {
        use std::sync::atomic::{AtomicU64, Ordering};
        static N: AtomicU64 = AtomicU64::new(0);
        N.fetch_add(1, Ordering::SeqCst)
    }

    fn publish_fixture(kind: &Kind, entry: &str) {
        let out = tempdir().join("out");
        std::fs::create_dir_all(out.join(entry).join("bin")).unwrap();
        std::fs::write(out.join(entry).join("bin/x"), "x").unwrap();
        let staged = depot::verify(&out, kind).unwrap();
        depot::publish(&out, &depot::kind_dir(kind), &staged).unwrap();
    }

    /// **A CLI is rolled out only once it has passed its tests, the version before it is kept as
    /// the rollback, and a pinned CLI only at its pin.**
    ///
    /// What would make it fail: `rollout` skipping the `tested` check (the untested entry rolls out),
    /// not carrying `previous`, or not holding claude to `src/claude-code.version` (the tested
    /// `claude@1.0.0` rolls out).
    #[test]
    fn an_untested_cli_is_not_rolled_out_and_the_last_one_is_kept() {
        let _g = env_lock();
        let dir = tempdir();
        let mut pins = env_pins();
        pins.set("SKEIN_FLEET_ROOT", dir.join("fleet"))
            .set("SKEIN_HOME", dir.join("home"));
        let pin = format!("claude@{}", pinned("claude").expect("claude is pinned"));
        for e in ["codex@1.0.0", "codex@1.0.1", "claude@1.0.0", pin.as_str()] {
            publish_fixture(&AGENT_CLI, e);
        }
        let why = rollout("codex", "codex@1.0.0").expect_err("untested");
        assert!(why.contains("has not passed its tests"), "{why}");
        update_rollouts(|r| {
            for e in ["codex@1.0.0", "codex@1.0.1", "claude@1.0.0", pin.as_str()] {
                r.tested.insert(
                    e.into(),
                    Tested {
                        ok: true,
                        ..Default::default()
                    },
                );
            }
            Ok(())
        })
        .unwrap();
        assert!(
            rollout("codex", "claude@1.0.0").is_err(),
            "a claude entry rolled out as codex"
        );
        let off_pin = rollout("claude", "claude@1.0.0").expect_err("claude is pinned");
        assert!(off_pin.contains("src/claude-code.version"), "{off_pin}");
        rollout("claude", &pin).expect("the pin rolls out");
        rollout("codex", "codex@1.0.0").unwrap();
        let rolled = rollout("codex", "codex@1.0.1").unwrap();
        assert_eq!(rolled.previous.as_deref(), Some("codex@1.0.0"));
        let want = wanted_versions("web-main");
        assert_eq!(want.get("codex").map(String::as_str), Some("codex@1.0.1"));
        assert_eq!(want.get("claude"), Some(&pin));
        depot::make_writable(&dir);
    }

    /// **CI checks the mod against the version the fleet rolls out: both read
    /// `src/claude-code.version`, and nothing writes the number a second time** (SKEIN-1380).
    ///
    /// What would make it fail: the `plugin` job naming a version of its own again (a
    /// `CLAUDE_CODE_VERSION:` literal, or the pin's number anywhere in the workflow), or no longer
    /// reading the file into `CLAUDE_CODE_VERSION`; a pin file that is not one plain version.
    #[test]
    fn the_ci_pin_is_the_fleets_pin() {
        let pin = pinned("claude").expect("claude is pinned");
        assert!(
            version_is_nameable(pin),
            "src/claude-code.version is {pin:?}"
        );
        let ci = std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR")).join(".github/workflows/ci.yml"),
        )
        .expect("the CI workflow");
        let job = ci
            .split("\n  plugin:\n")
            .nth(1)
            .expect("the CI workflow has no `plugin` job");
        let job = job.split("\n\n").next().unwrap_or(job);
        assert!(
            job.lines().any(|l| l.contains("src/claude-code.version")
                && l.contains("CLAUDE_CODE_VERSION=")
                && l.contains("GITHUB_ENV")),
            "the plugin job does not read CLAUDE_CODE_VERSION from src/claude-code.version:\n{job}"
        );
        assert!(
            !ci.contains("CLAUDE_CODE_VERSION:"),
            "the workflow sets CLAUDE_CODE_VERSION to a value of its own"
        );
        assert!(
            !ci.contains(pin),
            "the workflow writes the pin's number {pin} itself, beside the file it should read"
        );
    }

    /// **CI type-checks the mod with the TypeScript the fleet checks every claude version with:
    /// both read `src/typescript.version`, and nothing writes the number a second time**
    /// (SKEIN-1396).
    ///
    /// What would make it fail: the `plugin` job's `TYPESCRIPT_VERSION:` literal coming back, or the
    /// pin's number anywhere in the workflow; the job no longer reading the file into
    /// `TYPESCRIPT_VERSION` through `GITHUB_ENV`; a pin file that is not one plain version.
    #[test]
    fn the_ci_typescript_pin_is_the_fleets_pin() {
        let pin = TYPESCRIPT_PIN.trim();
        assert!(
            version_is_nameable(pin),
            "src/typescript.version is {pin:?}"
        );
        assert_eq!(typescript_entry(), format!("typescript@{pin}"));
        let ci = std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR")).join(".github/workflows/ci.yml"),
        )
        .expect("the CI workflow");
        let job = ci
            .split("\n  plugin:\n")
            .nth(1)
            .expect("the CI workflow has no `plugin` job");
        let job = job.split("\n\n").next().unwrap_or(job);
        assert!(
            job.lines().any(|l| l.contains("src/typescript.version")
                && l.contains("TYPESCRIPT_VERSION=")
                && l.contains("GITHUB_ENV")),
            "the plugin job does not read TYPESCRIPT_VERSION from src/typescript.version:\n{job}"
        );
        assert!(
            job.contains("typescript@$TYPESCRIPT_VERSION"),
            "the plugin job does not install the TypeScript it read:\n{job}"
        );
        assert!(
            !ci.contains("TYPESCRIPT_VERSION:"),
            "the workflow sets TYPESCRIPT_VERSION to a value of its own"
        );
        assert!(
            !ci.contains(pin),
            "the workflow writes the TypeScript pin's number {pin} itself, beside the file it \
             should read"
        );
    }

    /// **Only `typescript@` and a plain version is a TypeScript entry.**
    ///
    /// What would make it fail: `TYPESCRIPT` naming its entries with `cli_entry_is_nameable` (the
    /// TypeScript entry is refused and `claude@2.1.314` accepted), or a check that takes any
    /// version (`latest` and a pre-release accepted).
    #[test]
    fn only_typescript_and_a_plain_version_is_a_typescript_entry() {
        let named = TYPESCRIPT.entry_is_nameable;
        assert!(named("typescript@5.9.3"), "typescript@5.9.3");
        assert!(named(&typescript_entry()), "the pin's own entry");
        for bad in [
            "typescript@latest",
            concat!("typescript@5.9.3-", "beta"),
            "typescript@5.9",
            "typescript@",
            "typescript",
            "claude@2.1.314",
            "codex@0.170.0",
            "typescript@5.9.3/..",
        ] {
            assert!(!named(bad), "{bad:?}");
        }
        assert!(typescript_fill("typescript@latest").is_err());
        let fill = typescript_fill("typescript@5.9.3").expect("a TypeScript fill");
        assert!(
            fill.fetch.iter().any(|w| w == "--ignore-scripts")
                && fill.fetch.last().map(String::as_str) == Some("typescript@5.9.3"),
            "{:?}",
            fill.fetch
        );
    }

    /// **The mod's check is given the pinned `tsc`, read-only at `/tsc` and named by `TSC`, and
    /// no other check is.**
    ///
    /// What would make it fail: dropping the `TSC` variable or the `/tsc` bind from the
    /// plugin-check row; giving them to codex's or the version check; `check_script` not emitting
    /// a check's binds as `--ro-bind`.
    #[test]
    fn the_mod_check_is_given_the_pinned_tsc() {
        let ts = Path::new("/depot/typescript").join(typescript_entry());
        let claude = cli("claude").unwrap();
        let checks = checks_for(claude, "2.1.314", &ts);
        let plugin = checks
            .iter()
            .find(|c| c.name == "plugin-check")
            .expect("claude's checks have the mod's check");
        assert_eq!(
            plugin.binds,
            [(ts.display().to_string(), "/tsc".to_string())]
        );
        assert!(
            plugin
                .env
                .contains(&("TSC".to_string(), "/tsc/bin/tsc".to_string())),
            "{:?}",
            plugin.env
        );
        let script = check_script("/depot/agent-cli/claude@2.1.314", "/src", plugin);
        assert!(
            script.contains(&format!(
                " --ro-bind {} {} ",
                sh_quote(&ts.display().to_string()),
                sh_quote("/tsc")
            )),
            "{script}"
        );
        let codex = checks_for(cli("codex").unwrap(), "0.170.0", &ts);
        let others: Vec<&CliCheck> = checks
            .iter()
            .filter(|c| c.name != "plugin-check")
            .chain(codex.iter())
            .collect();
        for c in others {
            assert!(
                c.binds.is_empty() && !c.env.iter().any(|(k, _)| k == "TSC"),
                "{c:?}"
            );
            assert!(!check_script("/e", "/src", c).contains("/tsc"), "{c:?}");
        }
    }

    /// **The restart rule: an idle box that is behind is restarted; a working one, one waiting on a
    /// person, one whose state is unknown, or one behind on nothing, is not; and without an ask the
    /// grace must pass first.**
    ///
    /// What would make it fail: dropping the idle check (the working box restarts); dropping the
    /// re-read at the press (`busy-at-press` restarts); restarting without the grace when nothing
    /// was asked (`idle-not-asked` restarts on the first pass); counting a non-version key.
    #[test]
    fn only_an_idle_box_behind_on_a_version_is_restarted() {
        let _g = env_lock();
        let dir = tempdir();
        let mut pins = env_pins();
        pins.set("SKEIN_FLEET_ROOT", dir.join("fleet"))
            .set("SKEIN_HOME", dir.join("home"));
        let asked = restart_request_path("idle-asked");
        std::fs::create_dir_all(asked.parent().unwrap()).unwrap();
        std::fs::write(&asked, "").unwrap();
        let boxes: Vec<BoxNow> = [
            ("idle-asked", "waiting"),
            ("idle-not-asked", "waiting"),
            ("working", "working"),
            ("needs-person", "needs-input"),
            ("unknown", ""),
            ("busy-at-press", "waiting"),
            ("launcher-only", "waiting"),
        ]
        .into_iter()
        .map(|(n, s)| BoxNow {
            name: n.into(),
            state: s.into(),
        })
        .collect();
        std::fs::create_dir_all(restart_request_path("busy-at-press").parent().unwrap()).unwrap();
        std::fs::write(restart_request_path("busy-at-press"), "").unwrap();
        let memory = IdleMemory::default();
        let restarted = std::cell::RefCell::new(Vec::<String>::new());
        let stale = |n: &str| {
            if n == "launcher-only" {
                vec!["launcher"]
            } else {
                vec!["agent-cli"]
            }
        };
        let idle_now = |n: &str| {
            if n == "busy-at-press" {
                super::super::idle::Idle::Busy("working".into())
            } else {
                super::super::idle::Idle::Idle
            }
        };
        let t0 = std::time::Instant::now();
        let pass = |at| {
            restart_onto_rollouts(&boxes, at, &memory, stale, idle_now, |n| {
                restarted.borrow_mut().push(n.to_string());
                Ok(())
            })
        };
        pass(t0);
        assert_eq!(*restarted.borrow(), ["idle-asked"]);
        assert!(!asked.exists(), "the ask was not consumed by the restart");
        pass(t0 + IDLE_GRACE - Duration::from_secs(1));
        assert_eq!(
            *restarted.borrow(),
            ["idle-asked"],
            "the grace was not waited out"
        );
        pass(t0 + IDLE_GRACE);
        assert_eq!(*restarted.borrow(), ["idle-asked", "idle-not-asked"]);
    }
}
