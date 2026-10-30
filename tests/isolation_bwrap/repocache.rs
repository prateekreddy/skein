//! A repo's shared download cache (SKEIN-1350): the boxes of one repo share it, and a box of
//! another repo cannot see it at all.

use super::*;

/// The launcher's repo-cache block, lifted: from `repo_cache=""` to `unset repo_cache_at`. It
/// works out the cache from `$SKEIN_BOX_STORE` and binds it back through the cover.
pub(super) fn repo_cache_block() -> String {
    let src = fs::read_to_string(script("box-session.sh")).unwrap();
    let lines: Vec<&str> = src.lines().collect();
    let from = lines
        .iter()
        .position(|l| *l == r#"repo_cache="""#)
        .expect("the launcher's repo-cache block moved");
    let to = from
        + lines[from..]
            .iter()
            .position(|l| *l == "unset repo_cache_at")
            .expect("the repo-cache block has no end");
    lines[from..=to].join("\n")
}

/// The session block's exports that point each tool at the repo cache, lifted: the `if` on
/// `$repo_cache` and its `fi`. Inside the box, as production runs them, with `$repo_cache` the
/// positional the launcher hands across.
fn repo_cache_exports() -> String {
    let src = fs::read_to_string(script("box-session.sh")).unwrap();
    let lines: Vec<&str> = src.lines().collect();
    let from = lines
        .iter()
        .position(|l| l.trim() == r#"if [ -n "$repo_cache" ] && [ -d "$repo_cache" ]; then"#)
        .expect("the session block's repo-cache exports moved");
    let to = from
        + lines[from..]
            .iter()
            .position(|l| l.trim() == "fi")
            .expect("the repo-cache exports have no end");
    let block = lines[from..=to].join("\n");
    for name in [
        "CARGO_HOME",
        "npm_config_cache",
        "PIP_CACHE_DIR",
        "UV_CACHE_DIR",
        "npm_config_store_dir",
    ] {
        assert!(
            block.contains(&format!("{name}=")),
            "the lifted exports no longer set {name}"
        );
    }
    block
}

/// One write per tool, each into the place that tool was pointed at. `wrote` or `no`.
const PLANT: &str = r#"w() { mkdir -p "$(dirname "$2")" 2>/dev/null && printf planted > "$2" 2>/dev/null && echo "$1 wrote" || echo "$1 no"; }
w npx "$npm_config_cache/_npx/0123/node_modules/skein-test/cli.js"
w cargo "$CARGO_HOME/registry/src/skein-test/build.rs"
w pip "$PIP_CACHE_DIR/http/skein-test"
w uv "$UV_CACHE_DIR/wheels/skein-test"
w pnpm "$npm_config_store_dir/v10/skein-test"
"#;

/// The same five, read from where THIS box's tools are pointed, and the npx one read again at
/// `$2`, the first repo's cache by its absolute path. `planted` or `none`.
const READ: &str = r#"r() { printf '%s %s\n' "$1" "$(cat "$2" 2>/dev/null || echo none)"; }
r npx "$npm_config_cache/_npx/0123/node_modules/skein-test/cli.js"
r cargo "$CARGO_HOME/registry/src/skein-test/build.rs"
r pip "$PIP_CACHE_DIR/http/skein-test"
r uv "$UV_CACHE_DIR/wheels/skein-test"
r pnpm "$npm_config_store_dir/v10/skein-test"
r direct "$2/npm/_npx/0123/node_modules/skein-test/cli.js"
"#;

const TOOLS: [&str; 5] = ["npx", "cargo", "pip", "uv", "pnpm"];

/// Start covered box `name` of repo `repo` and run `probe` in it, with the launcher's isolation
/// block, its repo-cache block, and the session's exports, in production's order. `$2` is
/// `arg`. `rewrite` is applied to the repo-cache block, the seam the sabotage uses.
fn in_repo_box(
    f: &Fleet,
    name: &str,
    repo: &str,
    probe: &str,
    arg: &Path,
    rewrite: &dyn Fn(String) -> String,
) -> String {
    for p in [
        f.fleet_root.join(name).join("tree"),
        f.state_parent.join(name),
    ] {
        fs::create_dir_all(p).unwrap();
    }
    let store = f.repos.join(repo).join("store/data");
    let inside = format!("repo_cache=\"$1\"\n{}\n{probe}", repo_cache_exports());
    let runner = format!(
        "set -uo pipefail\n\
         binds=()\n\
         box={name}\n\
         root={root}\n\
         state={state}\n\
         export SKEIN_FLEET_ROOT={fleet} SKEIN_BOX_PRIVILEGED=0 \
         SKEIN_FLEET_MOUNTS={mounts} SKEIN_BOX_STORE={store}\n\
         {iso}\n\
         {cache}\n\
         exec bwrap --dev-bind / / ${{binds[@]+\"${{binds[@]}}\"}} -- \
           /bin/bash -c {inside} skein-probe \"$repo_cache\" {arg}\n",
        name = skein::util::sh_quote(name),
        root = skein::util::sh_quote(f.fleet_root.join(name).to_string_lossy().as_ref()),
        state = skein::util::sh_quote(f.state_parent.join(name).to_string_lossy().as_ref()),
        fleet = skein::util::sh_quote(f.fleet_root.to_string_lossy().as_ref()),
        mounts = skein::util::sh_quote(&f.mounts(Born::Covered)),
        store = skein::util::sh_quote(store.to_string_lossy().as_ref()),
        iso = isolation_block(),
        cache = rewrite(repo_cache_block()),
        inside = skein::util::sh_quote(&inside),
        arg = skein::util::sh_quote(arg.to_string_lossy().as_ref()),
    );
    let out = Command::new("bash")
        .arg("-c")
        .arg(&runner)
        .output()
        .expect("bash");
    assert!(
        out.status.success(),
        "the box did not start: {}\n--- script ---\n{runner}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).to_string()
}

/// **Two boxes of one repo share its download cache; a box of another repo sees none of it**
/// (SKEIN-1350, the owner's decision of 2026-10-29: within a repo share freely, between repos a
/// wall). Each of the five tools is pointed at the repo's `store/cache` by the session's exports,
/// and the cache is bound back through the cover that hides every other repo's store.
///
/// The npm write is the one the item names: `~/.npm/_npx` is where `npx` keeps a package it runs
/// on later calls, so it is code one box can leave for another. It must reach a sibling of the
/// same repo, and nothing of another repo's.
///
/// # What would make this fail
///
///   * Dropping the bind of the cache back through the cover: under the tmpfs over the repos
///     mount a covered box finds no cache, its tools are pointed at nothing, and its write is `no`.
///   * Binding the whole `repos` directory back to reach the cache: the other repo's box reads
///     the first repo's npx package by its path (the "direct" assertion).
///   * Dropping an export: that tool's write is `no`.
///
/// Watched, the first two, made in `box-session.sh` itself: the first failed on "a box could not
/// write its repo's npx cache", the second on "a box of another repo can read the first repo's
/// npx packages by their path".
#[test]
fn a_repos_boxes_share_its_cache_and_another_repos_box_sees_none_of_it() {
    if !bwrap_works() {
        skip("bwrap cannot make a user namespace here, so who sees a repo's download cache is unchecked");
        return;
    }
    let f = Fleet::make("repocache1350");
    for repo in ["web", "other"] {
        fs::create_dir_all(f.repos.join(repo).join("store/cache")).unwrap();
    }
    let web_cache = f.repos.join("web/store/cache");
    let check = |rewrite: &dyn Fn(String) -> String| {
        let wrote = in_repo_box(&f, "web-main", "web", PLANT, &web_cache, rewrite);
        let sibling = in_repo_box(&f, "web-two", "web", READ, &web_cache, rewrite);
        let other = in_repo_box(&f, "other-main", "other", READ, &web_cache, rewrite);
        (wrote, sibling, other)
    };
    let (wrote, sibling, other) = check(&|b| b);
    for tool in TOOLS {
        assert_eq!(
            answer(&wrote, tool),
            "wrote",
            "a box could not write its repo's {tool} cache:\n{wrote}"
        );
        assert_eq!(
            answer(&sibling, tool),
            "planted",
            "a second box of the same repo did not see the {tool} download the first one made, \
             so the repo's boxes are not sharing one cache:\n{wrote}\n{sibling}"
        );
        assert_eq!(
            answer(&other, tool),
            "none",
            "a box of another repo read the first repo's {tool} cache:\n{other}"
        );
    }
    assert_eq!(
        answer(&other, "direct"),
        "none",
        "a box of another repo can read the first repo's npx packages by their path:\n{other}"
    );
    assert_eq!(
        fs::read_to_string(web_cache.join("npm/_npx/0123/node_modules/skein-test/cli.js"))
            .unwrap_or_default(),
        "planted",
        "the write did not land in the repo's cache on disk, so it would be gone at the next stop"
    );
    assert!(
        !f.repos
            .join("other/store/cache/npm/_npx/0123/node_modules/skein-test")
            .exists(),
        "the first repo's npx package is in the other repo's cache"
    );
}
