//! Which of a box's clean items belong to finished work, and which speed up its next run
//! (SKEIN-1477, the owner's refinement of 2026-11-06).
//!
//! Every item on [`box_cleanables`]' list gets a class:
//!
//!   * **dead**: it belongs to something finished. The build output of a linked worktree whose
//!     branch is merged into the default branch's remote head or into the box's own checkout of
//!     the default branch, whose pull request is merged or closed, or whose upstream git marks
//!     gone; build output under a checkout's `.worktrees/<name>`
//!     when that worktree is no longer there; the scratchpads of ended sessions; and a download
//!     cache the box no longer uses because its repo's tools area holds that cache now.
//!   * **warm**: everything else. It speeds up the next run, so it is kept until the disk is short.
//!
//! **Never merged by age.** A squash merge leaves no ancestry, so a branch that is not an ancestor
//! is asked about through `gh pr list --head`, once a day per branch, and when `gh` cannot answer
//! that check is skipped rather than guessed. Ancestry alone is not enough either: a branch made a
//! moment ago from the remote head is its ancestor too, so ancestry counts only for a branch whose
//! reflog shows a commit made on it.
//!
//! **Merged here counts.** A coordinator merges its lanes into its own checkout of the default
//! branch and pushes later, or never (2026-11-13: two merged lanes' build folders, 15G, were offered
//! as plain build output on a box at 23.9G of its 10G). A branch whose every commit is in the main
//! checkout's branch, when that branch is the default one, is finished for its worktree and its
//! build output: nothing in them is not also in that branch. It is not finished for the branch
//! itself, which may still need pushing, and skein never offers the branch.
//!
//! **Source is never dead.** A worktree's own files are not on the list and never will be. Its
//! regenerable output is dead only when its branch is finished and nothing in it is uncommitted or
//! unpushed, since work that is not committed and pushed is not finished; the worktree is then
//! named for `git worktree remove`, which git refuses while anything in it is uncommitted.
//!
//! **git runs inside the box.** The checkout and its `.git` are the box's to write, and git run on
//! them from outside runs the box's config with skein's reach: a `filter.<x>.clean` on anything
//! that refreshes the index, which `git status` does (SKEIN-1252, `diff::in_box_lines`). So the
//! classifier reads only plain files on skein's side and asks git and `gh` through a script run in
//! the box, as the box. A box that is not running is not asked, and its worktrees' output is warm.
//! The cost is bounded: one `git for-each-ref` per repository, `git status` only for a worktree
//! whose branch is already finished or about to be asked about, at most [`GH_PER_LIST`] `gh` calls
//! a listing, and no fetch.
//!
//! **Off the resource tick.** A box's answers can take tens of seconds, and the tick that writes
//! every box's signal file must not wait on them, or a disk ask waits too. So the tick classifies
//! only what plain files tell it, and records what it listed ([`want`]); [`watch_classes`] asks the
//! running boxes on its own timer, and the tick reads the last answers ([`found`]).

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

/// The class of an item that belongs to finished work.
pub const DEAD: &str = "dead";
/// The class of an item that speeds up the next run.
pub const WARM: &str = "warm";

/// One item's class, and for a dead one, why, in words for the agent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Classed {
    pub class: &'static str,
    /// `lane/x is merged`, `its worktree is gone`. Empty for a warm item.
    pub because: String,
    /// The linked worktree a dead item is in, as the box spells it: it can go too, with
    /// `git worktree remove`. Empty for anything else.
    pub worktree: String,
}

impl Classed {
    fn warm() -> Classed {
        Classed {
            class: WARM,
            because: String::new(),
            worktree: String::new(),
        }
    }

    fn dead(because: impl Into<String>) -> Classed {
        Classed {
            class: DEAD,
            because: because.into(),
            worktree: String::new(),
        }
    }
}

/// The download caches under a box's home that its repo's tools area takes over when there is
/// one (`box-session.sh`, SKEIN-1431): `CARGO_HOME`, `npm_config_cache`, `PIP_CACHE_DIR` and
/// `UV_CACHE_DIR` all point into the area then, so these are stray copies nothing reads.
const MOVED_TO_REPO_TOOLS: &[&str] = &[
    ".cargo/registry",
    ".cargo/git",
    ".npm/_cacache",
    ".cache/pip",
    ".cache/uv",
];

/// How long a `gh` answer about a branch is kept, and how long a failure to get one is.
const GH_KEPT_SECS: i64 = 24 * 60 * 60;
const GH_FAILED_SECS: i64 = 60 * 60;

/// At most this many branches are asked about with `gh` in one listing, so a box with many lanes
/// spreads its questions over several listings rather than holding up one tick.
pub const GH_PER_LIST: usize = 4;

/// What `gh pr list --head <branch>` said: each pull request's state and head commit.
type PullRequests = Vec<(String, String)>;

/// `gh`'s answers, by (box, repository, branch), with when each was got. `None` is a failure.
#[derive(Debug, Default)]
pub struct GhCache {
    answers: HashMap<(String, String, String), (i64, Option<PullRequests>)>,
}

impl GhCache {
    fn fresh(&self, key: &(String, String, String), now: i64) -> Option<&Option<PullRequests>> {
        let (at, answer) = self.answers.get(key)?;
        let kept = match answer {
            Some(_) => GH_KEPT_SECS,
            None => GH_FAILED_SECS,
        };
        (now - at < kept).then_some(answer)
    }
}

/// The one cache skein-server keeps across ticks.
pub fn gh_cache() -> &'static std::sync::Mutex<GhCache> {
    static CACHE: std::sync::OnceLock<std::sync::Mutex<GhCache>> = std::sync::OnceLock::new();
    CACHE.get_or_init(Default::default)
}

/// How skein asks a box: run a shell script there, as the box, and get its stdout, or `None` when
/// it could not. `home` and `vartmp` are shell words for where the box sees its `home` and
/// `vartmp` directories: `"$HOME"` and `/var/tmp` in a real box.
pub struct InBox<'a> {
    pub run: &'a dyn Fn(&str) -> Option<String>,
    pub home: String,
    pub vartmp: String,
}

/// [`InBox`] for box `name` through its place, the way `prune_tool_caches` reaches a box.
pub fn in_box_runner(name: &str) -> impl Fn(&str) -> Option<String> + '_ {
    move |script: &str| {
        let wrapped = format!("bash -c {}", crate::util::sh_quote(script));
        crate::place::located(name)
            .and_then(|p| p.exec(&wrapped, std::time::Duration::from_secs(60)))
            .ok()
    }
}

/// Where an item sits in git terms, read from plain files on skein's side.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Place {
    /// Inside the linked worktree at this path.
    Linked(PathBuf),
    /// Under `<checkout>/.worktrees/<name>`, which is no longer a worktree.
    Orphan,
    /// Anywhere else: the main checkout, a kept target, outside every checkout.
    Other,
}

fn place_of(box_dir: &Path, item: &Path) -> Place {
    let mut at = item.parent();
    while let Some(dir) = at {
        if !dir.starts_with(box_dir) || dir == box_dir {
            return Place::Other;
        }
        let Ok(meta) = std::fs::symlink_metadata(dir.join(".git")) else {
            at = dir.parent();
            continue;
        };
        if meta.is_file() {
            return Place::Linked(dir.to_path_buf());
        }
        // The main checkout: an item under its `.worktrees/<name>` with no `.git` there is the
        // output of a worktree that was removed around it.
        let mut rel = item
            .strip_prefix(dir)
            .ok()
            .into_iter()
            .flat_map(|r| r.components());
        if let (Some(first), Some(name), Some(_)) = (rel.next(), rel.next(), rel.next()) {
            if first.as_os_str() == ".worktrees"
                && std::fs::symlink_metadata(dir.join(".worktrees").join(name).join(".git"))
                    .is_err()
            {
                return Place::Orphan;
            }
        }
        return Place::Other;
    }
    Place::Other
}

/// One branch as `git for-each-ref` reported it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Ref {
    tip: String,
    upstream: String,
    /// `gone`, `ahead 1`, `behind 2`, `ahead 1, behind 2`, or empty.
    track: String,
    /// Commits on the branch the default remote head does not have, when there is one.
    ahead: Option<u64>,
    /// Commits on the branch the main checkout's default branch does not have, when the main
    /// checkout has the default branch checked out.
    ahead_here: Option<u64>,
}

/// One repository: its default remote head and its branches.
#[derive(Debug, Clone, Default)]
struct Repo {
    /// `refs/remotes/origin/HEAD`'s target, or `origin/main` or `origin/master`. Empty when none.
    base: String,
    /// The branch the main checkout has checked out, when it is the default one: the remote
    /// head's name, or `main` or `master` with no remote head. Empty otherwise, or when bare.
    here: String,
    refs: HashMap<String, Ref>,
}

/// One linked worktree: its repository (the common git directory, as the box spells it), its
/// branch (empty when detached), and whether its reflog shows a commit made on it.
#[derive(Debug, Clone, Default)]
struct Worktree {
    repo: String,
    branch: String,
    own: bool,
}

/// The shell that maps a fleet path to the box's own spelling of it.
fn prelude(box_dir: &Path, inbox: &InBox) -> String {
    let mut s = String::new();
    s.push_str(&format!(
        "R={}; H={}; V={}; t=$(printf '\\t')\n",
        crate::util::sh_quote(&box_dir.display().to_string()),
        inbox.home,
        inbox.vartmp
    ));
    s.push_str(
        r#"seen() { case "$1" in "$R/home"|"$R/home/"*) printf '%s' "$H${1#"$R/home"}" ;; "$R/vartmp"|"$R/vartmp/"*) printf '%s' "$V${1#"$R/vartmp"}" ;; *) printf '%s' "$1" ;; esac; }
"#,
    );
    s
}

/// The first script: for each linked worktree, its repository, branch and whether it has commits
/// of its own, read from its git files; then, once per repository, the default remote head, the
/// main checkout's branch when it is the default one, and one `git for-each-ref` over its branches.
fn refs_script(box_dir: &Path, inbox: &InBox, worktrees: &[&Path]) -> String {
    let mut s = prelude(box_dir, inbox);
    s.push_str("cs=\n");
    for w in worktrees {
        s.push_str(&format!(
            "W={}\n",
            crate::util::sh_quote(&w.display().to_string())
        ));
        s.push_str(
            r#"w=$(seen "$W"); g=$(sed -n '1s/^gitdir: //p' "$w/.git" 2>/dev/null)
if [ -n "$g" ]; then
  case "$g" in /*) ;; *) g="$w/$g" ;; esac
  if [ -f "$g/commondir" ]; then
    c=$(cat "$g/commondir"); case "$c" in /*) ;; *) c="$g/$c" ;; esac
    c=$(cd "$c" 2>/dev/null && pwd -P) || c=
    h=$(cat "$g/HEAD" 2>/dev/null); b=; o=0
    case "$h" in "ref: refs/heads/"*) b=${h#ref: refs/heads/}; grep -qE "${t}(commit|cherry-pick)" "$c/logs/refs/heads/$b" 2>/dev/null && o=1 ;; esac
    if [ -n "$c" ]; then printf 'wt\t%s\t%s\t%s\t%s\n' "$W" "$c" "$b" "$o"; cs="$cs$c
"; fi
  fi
fi
"#,
        );
    }
    s.push_str(
        r#"printf '%s' "$cs" | sort -u | while IFS= read -r c; do
  [ -n "$c" ] || continue
  d=$(git --git-dir="$c" symbolic-ref -q refs/remotes/origin/HEAD 2>/dev/null) || d=
  if [ -z "$d" ]; then for x in main master; do if git --git-dir="$c" rev-parse -q --verify "refs/remotes/origin/$x" >/dev/null 2>&1; then d="refs/remotes/origin/$x"; break; fi; done; fi
  m=; if [ "$(git --git-dir="$c" config --bool core.bare 2>/dev/null)" != true ]; then m=$(git --git-dir="$c" symbolic-ref -q --short HEAD 2>/dev/null) || m=; fi
  case "$d:$m" in ?*:*) [ "$m" = "${d#refs/remotes/origin/}" ] || m= ;; :main|:master) ;; *) m= ;; esac
  printf 'repo\t%s\t%s\t%s\n' "$c" "$d" "$m"
  f='%(refname:lstrip=2)%09%(objectname)%09%(upstream)%09%(upstream:track,nobracket)'
  if [ -n "$d" ]; then a="$f%09%(ahead-behind:$d)"; else a="$f%09-"; fi
  if [ -n "$m" ]; then a="$a%09%(ahead-behind:refs/heads/$m)"; fi
  { git -c core.fsmonitor=false --git-dir="$c" for-each-ref --format="$a" refs/heads 2>/dev/null || git -c core.fsmonitor=false --git-dir="$c" for-each-ref --format="$f" refs/heads 2>/dev/null; } | while IFS= read -r l; do printf 'ref\t%s\n' "$l"; done
done
"#,
    );
    s
}

fn parse_refs(out: &str) -> (HashMap<String, Worktree>, HashMap<String, Repo>) {
    let (mut wts, mut repos) = (HashMap::new(), HashMap::<String, Repo>::new());
    let mut current: Option<String> = None;
    for line in out.lines() {
        let f: Vec<&str> = line.split('\t').collect();
        match f.as_slice() {
            ["wt", w, c, b, o] => {
                wts.insert(
                    w.to_string(),
                    Worktree {
                        repo: c.to_string(),
                        branch: b.to_string(),
                        own: *o == "1",
                    },
                );
            }
            ["repo", c, d, here @ ..] => {
                let repo = repos.entry(c.to_string()).or_default();
                repo.base = d.to_string();
                repo.here = here.first().map(|h| h.to_string()).unwrap_or_default();
                current = Some(c.to_string());
            }
            ["ref", name, tip, upstream, track, rest @ ..] => {
                let Some(repo) = current.as_ref().and_then(|c| repos.get_mut(c)) else {
                    continue;
                };
                let ahead_of = |at: usize| {
                    rest.get(at)
                        .and_then(|ab| ab.split_whitespace().next())
                        .and_then(|a| a.parse().ok())
                };
                repo.refs.insert(
                    name.to_string(),
                    Ref {
                        tip: tip.to_string(),
                        upstream: upstream.to_string(),
                        track: track.to_string(),
                        ahead: ahead_of(0),
                        ahead_here: ahead_of(1),
                    },
                );
            }
            _ => {}
        }
    }
    (wts, repos)
}

/// The second script: `git status` for each worktree in `status`, and `gh pr list` for each
/// (worktree, branch) in `ask`, stopping at the first `gh` that fails.
fn state_script(box_dir: &Path, inbox: &InBox, status: &[&Path], ask: &[(&Path, &str)]) -> String {
    let mut s = prelude(box_dir, inbox);
    for w in status {
        s.push_str(&format!(
            "W={}\n",
            crate::util::sh_quote(&w.display().to_string())
        ));
        s.push_str(
            r#"w=$(seen "$W")
if o=$(git -C "$w" -c core.fsmonitor=false --no-optional-locks status --porcelain --untracked-files=normal 2>/dev/null); then if [ -n "$o" ]; then n=1; else n=0; fi; else n=x; fi
printf 'dirty\t%s\t%s\n' "$W" "$n"
"#,
        );
    }
    s.push_str("k=1; command -v gh >/dev/null 2>&1 || k=0\n");
    for (w, b) in ask {
        s.push_str(&format!(
            "W={}; B={}\n",
            crate::util::sh_quote(&w.display().to_string()),
            crate::util::sh_quote(b)
        ));
        s.push_str(
            r#"if [ "$k" = 1 ]; then
  w=$(seen "$W")
  if o=$(cd "$w" && timeout 8 gh pr list --head "$B" --state all --json state,headRefOid --limit 20 2>/dev/null); then printf 'gh\t%s\t%s\t%s\n' "$W" "$B" "$(printf '%s' "$o" | tr -d '\n\t')"; else k=0; fi
fi
"#,
        );
    }
    s
}

/// `gh pr list --json state,headRefOid`, as (state, head) pairs.
fn parse_prs(json: &str) -> Option<PullRequests> {
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    Some(
        v.as_array()?
            .iter()
            .filter_map(|p| {
                Some((
                    p.get("state")?.as_str()?.to_string(),
                    p.get("headRefOid")?.as_str()?.to_string(),
                ))
            })
            .collect(),
    )
}

/// Whether a branch is finished by its pull requests: none is open, and one whose head is the
/// branch's tip is merged or closed. `Some(true)` for merged, `Some(false)` for closed.
fn finished_by_prs(prs: &PullRequests, tip: &str) -> Option<bool> {
    if prs.iter().any(|(state, _)| state == "OPEN") {
        return None;
    }
    let at_tip: Vec<&str> = prs
        .iter()
        .filter(|(_, head)| head == tip)
        .map(|(state, _)| state.as_str())
        .collect();
    if at_tip.contains(&"MERGED") {
        Some(true)
    } else if at_tip.contains(&"CLOSED") {
        Some(false)
    } else {
        None
    }
}

/// A branch name as the words may say it: the characters a branch is usually made of, or
/// "its branch" for anything else, since a box chooses its own branch names.
fn said(branch: &str) -> String {
    let plain = !branch.is_empty()
        && branch.len() <= 80
        && branch
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "/._+-".contains(c));
    match plain {
        true => branch.to_string(),
        false => "its branch".to_string(),
    }
}

/// `path` as the box spells it: `~/…` under its home, `/var/tmp/…` under its `vartmp`, and its
/// own name elsewhere, where the launcher binds the box directory at the same path.
pub fn box_spelling(box_dir: &Path, path: &Path) -> String {
    if let Ok(rel) = path.strip_prefix(box_dir.join("home")) {
        return format!("~/{}", rel.display());
    }
    if let Ok(rel) = path.strip_prefix(box_dir.join("vartmp")) {
        return format!("/var/tmp/{}", rel.display());
    }
    path.display().to_string()
}

/// The class of every item in `items`, (path, kind) as [`box_cleanables`] lists them, in box
/// `box_dir`. `moved`: the box's repo has a tools area, so [`MOVED_TO_REPO_TOOLS`] are strays.
/// `inbox`: how to ask the box, or `None` when it is not running. `now`: seconds, for `gh`'s
/// answers in `gh`.
pub fn classify(
    box_dir: &Path,
    items: &[(&str, &str)],
    moved: bool,
    inbox: Option<&InBox>,
    gh: &mut GhCache,
    now: i64,
) -> BTreeMap<String, Classed> {
    let home = box_dir.join("home");
    let mut out = BTreeMap::new();
    let mut linked: BTreeMap<PathBuf, Vec<String>> = BTreeMap::new();
    for (path, kind) in items {
        let p = Path::new(path);
        let class = match *kind {
            "scratchpad" => Classed::dead("its session has ended"),
            "download cache"
                if moved
                    && MOVED_TO_REPO_TOOLS
                        .iter()
                        .any(|rel| p == home.join(rel).as_path()) =>
            {
                Classed::dead("this box now uses the copy in its repo's tools area")
            }
            "build output" => match place_of(box_dir, p) {
                Place::Orphan => Classed::dead("its worktree is gone"),
                Place::Linked(w) => {
                    let spelled = w.display().to_string();
                    if !spelled.contains(['\n', '\t']) {
                        linked.entry(w).or_default().push(path.to_string());
                    }
                    Classed::warm()
                }
                Place::Other => Classed::warm(),
            },
            _ => Classed::warm(),
        };
        out.insert(path.to_string(), class);
    }
    let Some(inbox) = inbox else {
        return out;
    };
    if linked.is_empty() {
        return out;
    }
    let worktrees: Vec<&Path> = linked.keys().map(|p| p.as_path()).collect();
    let Some(text) = (inbox.run)(&refs_script(box_dir, inbox, &worktrees)) else {
        return out;
    };
    let (wts, repos) = parse_refs(&text);
    let name = box_dir
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();

    // What each worktree's branch is, so far: finished and why, or to be asked about.
    struct Branch<'a> {
        w: &'a Path,
        wt: &'a Worktree,
        r: &'a Ref,
        base: &'a str,
        finished: Option<String>,
        by_prs: bool,
        /// Finished by being merged into the main checkout's default branch: its commits are
        /// there, so it is not unfinished for want of a push.
        here: bool,
    }
    let mut branches: Vec<Branch> = Vec::new();
    for w in &worktrees {
        let Some(wt) = wts.get(&w.display().to_string()) else {
            continue;
        };
        let Some(repo) = repos.get(&wt.repo) else {
            continue;
        };
        let Some(r) = repo.refs.get(&wt.branch) else {
            continue;
        };
        let default = repo.base.rsplit_once("origin/").map_or("", |(_, b)| b);
        if wt.branch.is_empty() || wt.branch == default || wt.branch == repo.here {
            continue;
        }
        let b = said(&wt.branch);
        let mut here = false;
        let finished = if wt.own && !repo.base.is_empty() && r.ahead == Some(0) {
            Some(format!("{b} is merged"))
        } else if r.track == "gone" {
            Some(format!("{b}'s upstream is gone"))
        } else if wt.own && !repo.here.is_empty() && r.ahead_here == Some(0) {
            here = true;
            Some(format!("{b} is merged into {}", said(&repo.here)))
        } else {
            None
        };
        branches.push(Branch {
            w,
            wt,
            r,
            base: &repo.base,
            finished,
            by_prs: false,
            here,
        });
    }
    let key = |b: &Branch| (name.clone(), b.wt.repo.clone(), b.wt.branch.clone());
    let mut ask: Vec<(&Path, &str)> = Vec::new();
    for b in branches.iter_mut().filter(|b| b.finished.is_none()) {
        match gh.fresh(&key(b), now) {
            Some(Some(prs)) => {
                if let Some(merged) = finished_by_prs(prs, &b.r.tip) {
                    b.finished = Some(pr_words(&b.wt.branch, merged));
                    b.by_prs = true;
                }
            }
            Some(None) => {}
            None if ask.len() < GH_PER_LIST => ask.push((b.w, b.wt.branch.as_str())),
            None => {}
        }
    }
    let status: Vec<&Path> = branches
        .iter()
        .filter(|b| b.finished.is_some() || ask.iter().any(|(w, _)| *w == b.w))
        .map(|b| b.w)
        .collect();
    if status.is_empty() {
        return out;
    }
    let state = (inbox.run)(&state_script(box_dir, inbox, &status, &ask)).unwrap_or_default();
    let mut dirty: HashMap<String, bool> = HashMap::new();
    let mut answered: HashMap<String, PullRequests> = HashMap::new();
    for line in state.lines() {
        match line.split('\t').collect::<Vec<_>>().as_slice() {
            ["dirty", w, "0"] => {
                dirty.insert(w.to_string(), false);
            }
            ["dirty", w, "1"] => {
                dirty.insert(w.to_string(), true);
            }
            ["gh", w, _, json] => {
                if let Some(prs) = parse_prs(json) {
                    answered.insert(w.to_string(), prs);
                }
            }
            _ => {}
        }
    }
    for b in branches.iter_mut() {
        if !ask.iter().any(|(w, _)| *w == b.w) {
            continue;
        }
        let prs = answered.get(&b.w.display().to_string()).cloned();
        if let Some(merged) = prs.as_ref().and_then(|p| finished_by_prs(p, &b.r.tip)) {
            b.finished = Some(pr_words(&b.wt.branch, merged));
            b.by_prs = true;
        }
        gh.answers.insert(key(b), (now, prs));
    }
    for b in &branches {
        let Some(because) = &b.finished else {
            continue;
        };
        // Unpushed: ahead of its upstream while it still has one, or, with none, commits the
        // default remote head does not have, unless its merged or closed pull request is at its
        // tip, which says they were pushed. A branch merged into the main checkout's default
        // branch has every commit there, which is what pushing would keep.
        let unpushed = match (b.r.upstream.is_empty(), b.r.track.as_str()) {
            _ if b.here => false,
            (false, "gone") => false,
            (false, track) => track.contains("ahead"),
            (true, _) => !b.by_prs && !b.base.is_empty() && b.r.ahead.is_none_or(|a| a > 0),
        };
        // Uncommitted or unpushed work means the work is not finished, whatever the branch says,
        // and the agent may be following it up there: its output stays warm (coordinator,
        // 2026-11-06). A worktree whose state could not be read is not known to be finished.
        let clean = dirty.get(&b.w.display().to_string()) == Some(&false);
        if !clean || unpushed {
            continue;
        }
        let worktree = box_spelling(box_dir, b.w);
        for path in linked.get(b.w).into_iter().flatten() {
            out.insert(
                path.clone(),
                Classed {
                    class: DEAD,
                    because: because.clone(),
                    worktree: worktree.clone(),
                },
            );
        }
    }
    out
}

fn pr_words(branch: &str, merged: bool) -> String {
    match merged {
        true => format!("{}'s pull request is merged", said(branch)),
        false => format!("{}'s pull request is closed", said(branch)),
    }
}

/// How often [`watch_classes`] asks the running boxes about their worktrees: the clean list's own
/// period, `resource_signal::CLEAN_LIST_EVERY`.
pub const CLASSIFY_EVERY: std::time::Duration = std::time::Duration::from_secs(300);

/// What the tick last listed in one box, for [`classify_wanted`] to ask the box about.
#[derive(Debug, Clone)]
struct Wanted {
    box_dir: PathBuf,
    items: Vec<(String, String)>,
    moved: bool,
}

fn wanted() -> &'static std::sync::Mutex<BTreeMap<String, Wanted>> {
    static WANTED: std::sync::OnceLock<std::sync::Mutex<BTreeMap<String, Wanted>>> =
        std::sync::OnceLock::new();
    WANTED.get_or_init(Default::default)
}

fn answers() -> &'static std::sync::Mutex<BTreeMap<String, BTreeMap<String, Classed>>> {
    static FOUND: std::sync::OnceLock<
        std::sync::Mutex<BTreeMap<String, BTreeMap<String, Classed>>>,
    > = std::sync::OnceLock::new();
    FOUND.get_or_init(Default::default)
}

/// Record what the tick listed in box `name`, (path, kind), for the next [`classify_wanted`].
pub fn want(name: &str, box_dir: &Path, items: &[(&str, &str)], moved: bool) {
    let entry = Wanted {
        box_dir: box_dir.to_path_buf(),
        items: items
            .iter()
            .map(|(p, k)| (p.to_string(), k.to_string()))
            .collect(),
        moved,
    };
    wanted()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(name.to_string(), entry);
}

/// The classes [`classify_wanted`] last found for box `name`, by path. Empty when it has not
/// asked, or the box was not running when it last looked.
pub fn found(name: &str) -> BTreeMap<String, Classed> {
    answers()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(name)
        .cloned()
        .unwrap_or_default()
}

/// Ask every running box the tick has listed about its worktrees, and keep the answers for
/// [`found`]. A box that is not running has its answers dropped: nothing it says can be fresh.
pub fn classify_wanted(liveness: &HashMap<String, bool>) {
    let boxes = wanted().lock().unwrap_or_else(|e| e.into_inner()).clone();
    let now = chrono::Utc::now().timestamp();
    for (name, w) in boxes {
        if liveness.get(&name) != Some(&true) {
            answers()
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&name);
            continue;
        }
        let runner = in_box_runner(&name);
        let inbox = InBox {
            run: &runner,
            home: "\"$HOME\"".into(),
            vartmp: "/var/tmp".into(),
        };
        let pairs: Vec<(&str, &str)> = w
            .items
            .iter()
            .map(|(p, k)| (p.as_str(), k.as_str()))
            .collect();
        let classes = {
            let mut gh = gh_cache().lock().unwrap_or_else(|e| e.into_inner());
            classify(&w.box_dir, &pairs, w.moved, Some(&inbox), &mut gh, now)
        };
        answers()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(name.clone(), classes);
    }
}

/// The loop skein-server runs: [`classify_wanted`] every [`CLASSIFY_EVERY`], off the resource
/// tick. Never returns.
pub async fn watch_classes() {
    let mut tick = tokio::time::interval(CLASSIFY_EVERY);
    loop {
        tick.tick().await;
        let ran = tokio::task::spawn_blocking(|| {
            classify_wanted(&crate::fleet::fleet_liveness());
        })
        .await;
        if let Err(e) = ran {
            eprintln!("skein: classifying the boxes' clean items did not run: {e}");
        }
    }
}

/// How many entries [`last_used`] looks at, and how deep.
const USED_ENTRIES: usize = 1024;
const USED_DEPTH: usize = 2;

/// When the directory `dir` was last used, as the newest modification time, in seconds since the
/// epoch, among it and what is at most [`USED_DEPTH`] levels below it, looking at no more than
/// [`USED_ENTRIES`] entries and never through a link. A sample, not a walk: a build or an install
/// writes into the top levels of what it uses (`target/debug/deps`, `node_modules/.package-lock.json`)
/// each time it runs. `None` when nothing could be read.
pub fn last_used(dir: &Path) -> Option<i64> {
    use std::os::unix::fs::MetadataExt;
    let mut newest = std::fs::symlink_metadata(dir).ok()?.mtime();
    let mut seen = 0usize;
    let mut level: Vec<PathBuf> = vec![dir.to_path_buf()];
    for _ in 0..USED_DEPTH {
        let mut below = Vec::new();
        for d in &level {
            let Ok(rd) = std::fs::read_dir(d) else {
                continue;
            };
            for entry in rd.flatten() {
                if seen >= USED_ENTRIES {
                    return Some(newest);
                }
                seen += 1;
                let Ok(m) = std::fs::symlink_metadata(entry.path()) else {
                    continue;
                };
                newest = newest.max(m.mtime());
                if m.is_dir() {
                    below.push(entry.path());
                }
            }
        }
        level = below;
    }
    Some(newest)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// git with nothing of this machine's config, as a fixed author.
    fn git(dir: &Path, args: &[&str]) -> String {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["-c", "init.defaultBranch=master"])
            .args(args)
            .envs(git_env())
            .output()
            .expect("git");
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    fn git_env() -> Vec<(&'static str, &'static str)> {
        vec![
            ("GIT_CONFIG_GLOBAL", "/dev/null"),
            ("GIT_CONFIG_NOSYSTEM", "1"),
            ("GIT_AUTHOR_NAME", "t"),
            ("GIT_AUTHOR_EMAIL", "t@example.com"),
            ("GIT_COMMITTER_NAME", "t"),
            ("GIT_COMMITTER_EMAIL", "t@example.com"),
        ]
    }

    /// A box `web-main` in a fleet under a tempdir, its checkout `tree` cloned from a bare
    /// `origin`, with SKEIN_HOME and SKEIN_FLEET_ROOT pinned into the tempdir, and a `gh` stub
    /// that answers from `gh/<branch with / as ->` and logs each call.
    struct Box_ {
        _lock: crate::testutil::EnvGuard,
        _pins: crate::testutil::EnvPins,
        dir: crate::testutil::TempDir,
        box_dir: PathBuf,
        tree: PathBuf,
    }

    impl Box_ {
        fn new() -> Box_ {
            let lock = crate::testutil::env_lock();
            let dir = crate::testutil::tempdir();
            let root: &Path = dir.as_ref();
            let mut pins = crate::testutil::env_pins();
            pins.set("SKEIN_FLEET_ROOT", root.join("fleet"));
            pins.set("SKEIN_HOME", root.join("skein"));
            let box_dir = root.join("fleet/web-main");
            for d in ["home", "vartmp"] {
                std::fs::create_dir_all(box_dir.join(d)).unwrap();
            }
            let origin = root.join("origin.git");
            let seed = root.join("seed");
            std::fs::create_dir_all(&seed).unwrap();
            git(root, &["init", "-q", "--bare", origin.to_str().unwrap()]);
            git(&seed, &["init", "-q"]);
            std::fs::write(seed.join(".gitignore"), "target/\n").unwrap();
            std::fs::write(seed.join("README"), "one\n").unwrap();
            git(&seed, &["add", "."]);
            git(&seed, &["commit", "-qm", "one"]);
            git(&seed, &["push", "-q", origin.to_str().unwrap(), "master"]);
            let tree = box_dir.join("tree");
            git(
                root,
                &[
                    "clone",
                    "-q",
                    origin.to_str().unwrap(),
                    tree.to_str().unwrap(),
                ],
            );
            std::fs::create_dir_all(root.join("gh")).unwrap();
            let stub = root.join("bin/gh");
            std::fs::create_dir_all(stub.parent().unwrap()).unwrap();
            std::fs::write(
                &stub,
                format!(
                    "#!/bin/sh\nd={}\necho \"$*\" >> \"$d/calls\"\n[ -f \"$d/fail\" ] && exit 1\n\
                     while [ $# -gt 0 ]; do [ \"$1\" = --head ] && b=$2; shift; done\n\
                     f=\"$d/$(printf '%s' \"$b\" | tr / -)\"\n\
                     if [ -f \"$f\" ]; then cat \"$f\"; else echo '[]'; fi\n",
                    crate::util::sh_quote(&root.join("gh").display().to_string())
                ),
            )
            .unwrap();
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();
            Box_ {
                _lock: lock,
                _pins: pins,
                dir,
                box_dir,
                tree,
            }
        }

        fn root(&self) -> &Path {
            self.dir.as_ref()
        }

        /// A worktree `.worktrees/<n>` on a new branch `lane/<n>`, with one commit of its own
        /// unless `commit` is false, and a `target` holding a file. Returns the target's path.
        fn lane(&self, n: &str, commit: bool) -> String {
            let w = self.tree.join(".worktrees").join(n);
            let branch = format!("lane/{n}");
            git(
                &self.tree,
                &["worktree", "add", "-q", w.to_str().unwrap(), "-b", &branch],
            );
            if commit {
                std::fs::write(w.join(n), "work\n").unwrap();
                git(&w, &["add", n]);
                git(&w, &["commit", "-qm", n]);
            }
            std::fs::create_dir_all(w.join("target/debug")).unwrap();
            std::fs::write(w.join("target/debug/x"), "built").unwrap();
            w.join("target").display().to_string()
        }

        fn worktree(&self, n: &str) -> PathBuf {
            self.tree.join(".worktrees").join(n)
        }

        /// Merge `lane/<n>` into master and push master.
        fn merge(&self, n: &str) {
            git(
                &self.tree,
                &[
                    "merge",
                    "-q",
                    "--no-ff",
                    "-m",
                    "merge",
                    &format!("lane/{n}"),
                ],
            );
            git(&self.tree, &["push", "-q", "origin", "master"]);
        }

        /// Push `lane/<n>` with an upstream, squash it into master, and push master.
        fn squash(&self, n: &str) {
            git(
                &self.worktree(n),
                &["push", "-q", "-u", "origin", &format!("lane/{n}")],
            );
            git(
                &self.tree,
                &["merge", "-q", "--squash", &format!("lane/{n}")],
            );
            git(&self.tree, &["commit", "-qm", "squashed"]);
            git(&self.tree, &["push", "-q", "origin", "master"]);
        }

        fn gh_says(&self, branch: &str, json: &str) {
            std::fs::write(self.root().join("gh").join(branch.replace('/', "-")), json).unwrap();
        }

        fn gh_calls(&self) -> usize {
            std::fs::read_to_string(self.root().join("gh/calls"))
                .map(|s| s.lines().count())
                .unwrap_or(0)
        }

        /// Classify `items` with this machine's bash standing in for the box, the box seeing its
        /// own paths at the fleet's names.
        fn classify(
            &self,
            items: &[(&str, &str)],
            moved: bool,
            gh: &mut GhCache,
            now: i64,
        ) -> BTreeMap<String, Classed> {
            let path = format!(
                "{}:{}",
                self.root().join("bin").display(),
                std::env::var("PATH").unwrap_or_default()
            );
            let run = |script: &str| -> Option<String> {
                let out = std::process::Command::new("bash")
                    .arg("-c")
                    .arg(script)
                    .envs(git_env())
                    .env("PATH", &path)
                    .output()
                    .ok()?;
                out.status
                    .success()
                    .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
            };
            let inbox = InBox {
                run: &run,
                home: crate::util::sh_quote(&self.box_dir.join("home").display().to_string()),
                vartmp: crate::util::sh_quote(&self.box_dir.join("vartmp").display().to_string()),
            };
            classify(&self.box_dir, items, moved, Some(&inbox), gh, now)
        }
    }

    /// **A merged branch's worktree output is dead, and a live branch's is warm**; so is a branch
    /// made a moment ago from the remote head, which is its ancestor too.
    ///
    /// What would make it fail: the ancestry rule not read (`lane/a` warm); ancestry counted
    /// without a commit of the branch's own (`lane/fresh` dead); the worktree not offered for
    /// `git worktree remove` when it is clean and merged. Watched: with `r.ahead == Some(0)` made
    /// `r.ahead == Some(u64::MAX)`, `lane/a` came back warm and this failed; with `wt.own &&`
    /// dropped, `lane/fresh` came back dead and this failed.
    #[test]
    fn a_merged_branchs_output_is_dead_and_a_live_ones_is_warm() {
        let b = Box_::new();
        let merged = b.lane("a", true);
        let live = b.lane("b", true);
        let fresh = b.lane("fresh", false);
        b.merge("a");
        let items = [
            (merged.as_str(), "build output"),
            (live.as_str(), "build output"),
            (fresh.as_str(), "build output"),
        ];
        let got = b.classify(&items, false, &mut GhCache::default(), 0);
        assert_eq!(
            got[&merged],
            Classed {
                class: DEAD,
                because: "lane/a is merged".into(),
                worktree: b.worktree("a").display().to_string(),
            }
        );
        assert_eq!(got[&live].class, WARM, "{:?}", got[&live]);
        assert_eq!(got[&fresh].class, WARM, "{:?}", got[&fresh]);
    }

    /// **A branch merged into the box's own master and not pushed is finished for its worktree's
    /// output**, which is dead and names the branch it is merged into, with the worktree offered;
    /// while a branch with a commit made since the merge, one never merged, and one made a moment
    /// ago from master stay warm, and so does a merged one while the main checkout is on another
    /// branch (2026-11-13: a coordinator's two merged lanes, 15G, were offered as plain build
    /// output).
    ///
    /// What would make it fail: ancestry read only against `origin/HEAD` (`lane/a` warm, the
    /// incident); the unpushed rule applied to it (warm again, since `origin` has none of its
    /// commits); ancestry counted without a commit of the branch's own (`lane/fresh` dead); the
    /// main checkout's branch not compared with the default one (`lane/a` dead off `side`).
    #[test]
    fn a_branch_merged_into_the_boxs_own_master_has_dead_output() {
        let b = Box_::new();
        let merged = b.lane("a", true);
        let after = b.lane("later", true);
        let live = b.lane("b", true);
        let fresh = b.lane("fresh", false);
        let local_merge = |n: &str| {
            git(
                &b.tree,
                &[
                    "merge",
                    "-q",
                    "--no-ff",
                    "-m",
                    "merge",
                    &format!("lane/{n}"),
                ],
            )
        };
        local_merge("a");
        local_merge("later");
        std::fs::write(b.worktree("later").join("more"), "more\n").unwrap();
        git(&b.worktree("later"), &["add", "more"]);
        git(&b.worktree("later"), &["commit", "-qm", "more"]);
        let items = [
            (merged.as_str(), "build output"),
            (after.as_str(), "build output"),
            (live.as_str(), "build output"),
            (fresh.as_str(), "build output"),
        ];
        let got = b.classify(&items, false, &mut GhCache::default(), 0);
        assert_eq!(
            got[&merged],
            Classed {
                class: DEAD,
                because: "lane/a is merged into master".into(),
                worktree: b.worktree("a").display().to_string(),
            }
        );
        for (path, what) in [
            (&after, "a commit since"),
            (&live, "never merged"),
            (&fresh, "fresh"),
        ] {
            assert_eq!(got[path].class, WARM, "{what}: {:?}", got[path]);
        }

        git(&b.tree, &["checkout", "-q", "-b", "side"]);
        let off = b.classify(&items[..1], false, &mut GhCache::default(), 0);
        assert_eq!(
            off[&merged].class, WARM,
            "merged into a branch that is not the default one"
        );
    }

    /// **A squash-merged branch, which is no ancestor of the remote head, is dead through its pull
    /// request's state**, asked of `gh` once and then answered from the cache for the day; and
    /// when `gh` fails, the check is skipped, not guessed.
    ///
    /// What would make it fail: `gh` not asked (warm); the pull request's head not compared with
    /// the branch's tip, or its state not read; the answer not cached (a second call); a failing
    /// `gh` read as merged. Watched: with `parse_prs` made to return `None`, `lane/c` came back
    /// warm and this failed.
    #[test]
    fn a_squash_merged_branch_is_dead_through_its_pull_request() {
        let b = Box_::new();
        let squashed = b.lane("c", true);
        let failing = b.lane("f", true);
        b.squash("c");
        b.squash("f");
        let tip = git(&b.worktree("c"), &["rev-parse", "HEAD"]);
        b.gh_says(
            "lane/c",
            &format!(r#"[{{"state":"MERGED","headRefOid":"{tip}"}}]"#),
        );
        let mut gh = GhCache::default();
        let items = [(squashed.as_str(), "build output")];
        let got = b.classify(&items, false, &mut gh, 1000);
        assert_eq!(got[&squashed].class, DEAD, "{:?}", got[&squashed]);
        assert_eq!(got[&squashed].because, "lane/c's pull request is merged");
        assert_eq!(b.gh_calls(), 1);
        let again = b.classify(&items, false, &mut gh, 1000 + 3600);
        assert_eq!(again[&squashed].class, DEAD);
        assert_eq!(b.gh_calls(), 1, "gh was asked again inside the day");

        std::fs::write(b.root().join("gh/fail"), "").unwrap();
        let items = [(failing.as_str(), "build output")];
        let got = b.classify(&items, false, &mut gh, 1000);
        assert_eq!(got[&failing].class, WARM, "a failing gh was guessed at");
    }

    /// **A worktree with uncommitted changes is never dead**: its branch is merged, and its output
    /// stays warm and the worktree is not offered for removal, while a clean one beside it is
    /// dead and offered (coordinator, 2026-11-06: uncommitted work is unfinished work).
    ///
    /// What would make it fail: `git status` not asked, or its answer not read, or a dirty
    /// worktree's output still classed dead. Watched: with `if !clean || unpushed` made
    /// `if unpushed`, `.worktrees/d/target` came back dead and this failed.
    #[test]
    fn a_worktree_with_uncommitted_changes_is_never_dead() {
        let b = Box_::new();
        let clean = b.lane("a", true);
        let dirty = b.lane("d", true);
        b.merge("a");
        b.merge("d");
        std::fs::write(b.worktree("d").join("notes.md"), "half-written\n").unwrap();
        let items = [
            (clean.as_str(), "build output"),
            (dirty.as_str(), "build output"),
        ];
        let got = b.classify(&items, false, &mut GhCache::default(), 0);
        assert_eq!(got[&clean].worktree, b.worktree("a").display().to_string());
        assert_eq!(got[&clean].class, DEAD, "{:?}", got[&clean]);
        assert_eq!(
            got[&dirty],
            Classed::warm(),
            "a worktree with uncommitted work was classed dead"
        );
    }

    /// **A stray `~/.cargo/registry` is dead once `CARGO_HOME` is the repo's tools area, and warm
    /// while it is the box's own**; a cache the area does not take over stays warm either way.
    ///
    /// What would make it fail: `.cargo/registry` missing from [`MOVED_TO_REPO_TOOLS`], or `moved`
    /// not read. Watched: with `.cargo/registry` removed from the list, this failed.
    #[test]
    fn a_stray_cargo_registry_is_dead_when_cargo_home_is_elsewhere() {
        let b = Box_::new();
        let registry = b.box_dir.join("home/.cargo/registry").display().to_string();
        let browsers = b
            .box_dir
            .join("home/.cache/ms-playwright")
            .display()
            .to_string();
        let items = [
            (registry.as_str(), "download cache"),
            (browsers.as_str(), "download cache"),
        ];
        let moved = b.classify(&items, true, &mut GhCache::default(), 0);
        assert_eq!(moved[&registry].class, DEAD);
        assert_eq!(moved[&browsers].class, WARM);
        let own = b.classify(&items, false, &mut GhCache::default(), 0);
        assert_eq!(own[&registry].class, WARM);
    }

    /// **The output of a worktree removed around it is dead**: `.worktrees/<n>/target` with no
    /// `.git` left in `.worktrees/<n>`.
    ///
    /// What would make it fail: [`place_of`] not checking for the worktree's `.git`.
    #[test]
    fn output_whose_worktree_is_gone_is_dead() {
        let b = Box_::new();
        let target = b.tree.join(".worktrees/gone/target");
        std::fs::create_dir_all(&target).unwrap();
        let path = target.display().to_string();
        let got = b.classify(
            &[(&path, "build output")],
            false,
            &mut GhCache::default(),
            0,
        );
        assert_eq!(got[&path].because, "its worktree is gone");
    }
}
