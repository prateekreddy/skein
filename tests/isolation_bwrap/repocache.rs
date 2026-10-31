//! A repo's tools and caches area (SKEIN-1431, and the download cache before it, SKEIN-1350): the
//! boxes of one repo share it, and a box of another repo can neither see nor write it.

use super::*;

/// Lines of the launcher from the first one equal to `from` (trimmed) to the next one equal to
/// `to`, joined.
fn lift(from: &str, to: &str, what: &str) -> String {
    let src = fs::read_to_string(script("box-session.sh")).unwrap();
    let lines: Vec<&str> = src.lines().collect();
    let start = lines
        .iter()
        .position(|l| l.trim() == from)
        .unwrap_or_else(|| panic!("the launcher's {what} moved"));
    let end = start
        + lines[start..]
            .iter()
            .position(|l| l.trim() == to)
            .unwrap_or_else(|| panic!("the launcher's {what} has no end"));
    lines[start..=end].join("\n")
}

/// The launcher's old repo-cache block, lifted: from `repo_cache=""` to its `unset`. It binds the
/// host's `store/cache` back through the cover, so that a box can still clean it (SKEIN-1356).
pub(super) fn repo_cache_block() -> String {
    lift(
        r#"repo_cache="""#,
        "unset repo_cache_at repo_cache",
        "repo-cache block",
    )
}

/// The launcher's repo-tools block, lifted: it makes the repo's area under the fleet root, holds
/// it open, and binds it at `~/.repo-tools`. Needs `$box`, `$home_at` and `$HOME`.
fn repo_tools_block() -> String {
    lift(
        r#"repo_tools="" repo_tools_id="" repo_tools_fd="""#,
        "unset repo_tools_id repo_tools_at repo_tools_dir repo_tools_fd",
        "repo-tools block",
    )
}

/// The session block's exports into the area, lifted: the `if` on `$repo_tools` and its `fi`.
/// Run inside the box, as production runs them, with `$repo_tools` and `$tree` set as the
/// launcher hands them across.
fn repo_tools_exports() -> String {
    let block = lift(
        r#"if [ -n "$repo_tools" ] && [ -d "$repo_tools" ]; then"#,
        "fi",
        "session block's repo-tools exports",
    );
    for name in [
        "SKEIN_REPO_TOOLS",
        "CARGO_HOME",
        "CARGO_INSTALL_ROOT",
        "npm_config_cache",
        "PIP_CACHE_DIR",
        "UV_CACHE_DIR",
        "npm_config_store_dir",
        "NPM_CONFIG_PREFIX",
    ] {
        assert!(
            block.contains(&format!("{name}=")),
            "the lifted exports no longer set {name}"
        );
    }
    block
}

/// The launcher's `box_path=` line, lifted: the PATH the box's session runs on.
fn box_path_line() -> String {
    let src = fs::read_to_string(script("box-session.sh")).unwrap();
    src.lines()
        .find(|l| l.starts_with("box_path="))
        .expect("the launcher has no box_path line")
        .to_string()
}

/// One write per place the area holds, each where the session pointed that tool, and a command
/// installed where `cargo install` puts one. `wrote` or `no`.
const PLANT: &str = r#"w() { mkdir -p "$(dirname "$2")" 2>/dev/null && printf planted > "$2" 2>/dev/null && echo "$1 wrote" || echo "$1 no"; }
w npx "$npm_config_cache/_npx/0123/node_modules/skein-test/cli.js"
w cargo "$CARGO_HOME/registry/src/skein-test/build.rs"
w pip "$PIP_CACHE_DIR/http/skein-test"
w uv "$UV_CACHE_DIR/wheels/skein-test"
w rustup "$SKEIN_REPO_TOOLS/rustup/toolchains/skein-test/marker"
w npmglobal "$NPM_CONFIG_PREFIX/lib/node_modules/skein-test/index.js"
{ mkdir -p "$CARGO_INSTALL_ROOT/bin" && printf '#!/bin/sh\necho tool-ran\n' > "$CARGO_INSTALL_ROOT/bin/skein-test-tool" && chmod +x "$CARGO_INSTALL_ROOT/bin/skein-test-tool"; } 2>/dev/null && echo "tool wrote" || echo "tool no"
"#;

/// The same places, read from where THIS box's tools are pointed, the command run off this box's
/// PATH, and the first repo's area read and written by its path on the fleet's disk, `$2`.
const READ: &str = r#"r() { printf '%s %s\n' "$1" "$(cat "$2" 2>/dev/null || echo none)"; }
r npx "$npm_config_cache/_npx/0123/node_modules/skein-test/cli.js"
r cargo "$CARGO_HOME/registry/src/skein-test/build.rs"
r pip "$PIP_CACHE_DIR/http/skein-test"
r uv "$UV_CACHE_DIR/wheels/skein-test"
r rustup "$SKEIN_REPO_TOOLS/rustup/toolchains/skein-test/marker"
r npmglobal "$NPM_CONFIG_PREFIX/lib/node_modules/skein-test/index.js"
printf 'tool %s\n' "$(skein-test-tool 2>/dev/null || echo none)"
r direct "$2/cache/npm/_npx/0123/node_modules/skein-test/cli.js"
{ mkdir -p "$2/tools/bin" && printf x > "$2/tools/bin/skein-test-planted"; } 2>/dev/null
"#;

const PLACES: [&str; 6] = ["npx", "cargo", "pip", "uv", "rustup", "npmglobal"];

/// Start covered box `name` of repo `repo` and run `probe` in it, with the launcher's isolation
/// block, its repo-cache and repo-tools blocks, the session's exports and its `box_path`, in
/// production's order. `$2` is `arg`. The box's `$HOME` is its own `home`, which is where the
/// launcher binds its home from.
fn in_repo_box(f: &Fleet, name: &str, repo: &str, probe: &str, arg: &Path) -> String {
    let root = f.fleet_root.join(name);
    for p in [
        root.join("tree"),
        root.join("home"),
        f.state_parent.join(name),
    ] {
        fs::create_dir_all(p).unwrap();
    }
    let store = f.repos.join(repo).join("store/data");
    let q = |p: &Path| skein::util::sh_quote(p.to_string_lossy().as_ref());
    let inside = format!(
        "repo_tools=\"$1\"\ntree={tree}\n{exports}\n{path}\nexport PATH=\"$box_path\"\n{probe}",
        tree = q(&root.join("tree")),
        exports = repo_tools_exports(),
        path = box_path_line(),
    );
    let runner = format!(
        "set -uo pipefail\n\
         binds=()\n\
         box={name}\n\
         root={root}\n\
         state={state}\n\
         export HOME={home}\n\
         home_at=\"$HOME\"\n\
         export SKEIN_FLEET_ROOT={fleet} SKEIN_BOX_PRIVILEGED=0 \
         SKEIN_FLEET_MOUNTS={mounts} SKEIN_BOX_STORE={store}\n\
         {iso}\n\
         {cache}\n\
         {tools}\n\
         exec bwrap --dev-bind / / ${{binds[@]+\"${{binds[@]}}\"}} -- \
           /bin/bash -c {inside} skein-probe \"$repo_tools\" {arg}\n",
        name = skein::util::sh_quote(name),
        root = q(&root),
        state = q(&f.state_parent.join(name)),
        home = q(&root.join("home")),
        fleet = q(&f.fleet_root),
        mounts = skein::util::sh_quote(&f.mounts(Born::Covered)),
        store = q(&store),
        iso = isolation_block(),
        cache = repo_cache_block(),
        tools = repo_tools_block(),
        inside = skein::util::sh_quote(&inside),
        arg = q(arg),
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

/// **Two boxes of one repo share its tools area; a box of another repo can neither see nor write
/// it** (SKEIN-1431, the owner's decisions of 2026-10-29 and 2026-10-31: within a repo share
/// freely, between repos a wall). Each download cache, rustup's home, npm's global prefix and the
/// area's `bin` are written by one box of the repo and read by its sibling, which runs the command
/// off its own PATH. The other repo's box reads none of it where its own tools point, and cannot
/// read or write the first repo's area by its path on the fleet's disk.
///
/// The npm write is the one SKEIN-1350 named: `~/.npm/_npx` is where `npx` keeps a package it runs
/// on later calls, so it is code one box can leave for another.
///
/// # What would make this fail
///
///   * One area for the whole fleet rather than one per repo: the other repo's box reads every
///     planted file ("a box of another repo read").
///   * Binding the whole `.repo-tools` directory rather than the one repo's: the other repo's box
///     reaches the first repo's area by its path ("direct", "directwrite").
///   * Dropping the bind: under the tmpfs over the fleet root a covered box finds no area, and its
///     writes are `no`.
///   * The area's `bin` left off `box_path`: the sibling cannot run the command ("tool").
///   * Dropping an export: that tool's write is `no`.
#[test]
fn a_repos_boxes_share_its_tools_area_and_another_repos_box_sees_none_of_it() {
    if !bwrap_works() {
        skip(
            "bwrap cannot make a user namespace here, so who sees a repo's tools area is unchecked",
        );
        return;
    }
    let f = Fleet::make("repotools1431");
    let web_area = f.fleet_root.join(".repo-tools/web");
    let wrote = in_repo_box(&f, "web-main", "web", PLANT, &web_area);
    let sibling = in_repo_box(&f, "web-two", "web", READ, &web_area);
    let other = in_repo_box(&f, "other-main", "other", READ, &web_area);
    for place in PLACES.iter().copied().chain(["tool"]) {
        assert_eq!(
            answer(&wrote, place),
            "wrote",
            "a box could not write its repo's {place} in the tools area:\n{wrote}"
        );
    }
    for place in PLACES {
        assert_eq!(
            answer(&sibling, place),
            "planted",
            "a second box of the same repo did not see the {place} the first one wrote, so the \
             repo's boxes are not sharing one area:\n{wrote}\n{sibling}"
        );
        assert_eq!(
            answer(&other, place),
            "none",
            "a box of another repo read the first repo's {place}:\n{other}"
        );
    }
    assert_eq!(
        answer(&sibling, "tool"),
        "tool-ran",
        "the second box of the repo could not run the command the first installed into the \
         area's bin off its own PATH:\n{sibling}"
    );
    assert_eq!(
        answer(&other, "tool"),
        "none",
        "a box of another repo ran the first repo's installed command:\n{other}"
    );
    assert_eq!(
        answer(&other, "direct"),
        "none",
        "a box of another repo can read the first repo's area by its path:\n{other}"
    );
    // Its write by that path is looked for on disk, not asked about: in a covered box the path is
    // under the tmpfs over the fleet root, so the write lands there and is gone with the box.
    assert!(
        !web_area.join("tools/bin/skein-test-planted").exists(),
        "a box of another repo wrote into the first repo's area by its path:\n{other}"
    );
    assert_eq!(
        fs::read_to_string(web_area.join("cache/npm/_npx/0123/node_modules/skein-test/cli.js"))
            .unwrap_or_default(),
        "planted",
        "the write did not land in the repo's area on the fleet's disk"
    );
    assert!(
        !f.fleet_root
            .join(".repo-tools/other/cache/npm/_npx/0123/node_modules/skein-test")
            .exists(),
        "the first repo's npx package is in the other repo's area"
    );
}

/// **A command installed into the area is still there, on the box's PATH, after the box
/// restarts** (SKEIN-1431). `~/.local` is the box's own RAM-backed overlay and loses what is
/// installed there at a stop (SKEIN-1414); the area is on the fleet's disk, outside every box root.
/// Two namespaces one after the other, with nothing of the first left running, stand for a stop
/// and a start.
///
/// What would make it fail: the area made on a tmpfs or anywhere a stop discards (the second
/// start finds nothing); the area's `bin` left off `box_path` (the command is not found).
#[test]
fn a_tool_installed_into_the_area_survives_a_box_restart() {
    if !bwrap_works() {
        skip(
            "bwrap cannot make a user namespace here, so whether a repo's tools last is unchecked",
        );
        return;
    }
    let f = Fleet::make("repotoolsrestart");
    let web_area = f.fleet_root.join(".repo-tools/web");
    let install = "mkdir -p \"$CARGO_INSTALL_ROOT/bin\" && \
                   printf '#!/bin/sh\\necho kept\\n' > \"$CARGO_INSTALL_ROOT/bin/skein-test-kept\" && \
                   chmod +x \"$CARGO_INSTALL_ROOT/bin/skein-test-kept\" && echo installed";
    let first = in_repo_box(&f, "web-main", "web", install, &web_area);
    assert_eq!(first.trim(), "installed", "{first}");
    let again = in_repo_box(
        &f,
        "web-main",
        "web",
        "skein-test-kept 2>/dev/null || echo missing",
        &web_area,
    );
    assert_eq!(
        again.trim(),
        "kept",
        "a command installed into the repo's area was not on the box's PATH after a restart"
    );
    assert!(
        web_area.join("tools/bin/skein-test-kept").is_file(),
        "the installed command is not in the repo's area on the fleet's disk"
    );
}

/// **Every cache is on the file system the box's checkout is on, and pnpm's store is in the same
/// mount as its worktrees, so pnpm hard links rather than copying** (SKEIN-1431, SKEIN-1429).
///
/// The host's `store/cache` is virtiofs on a real fleet; the area is under the fleet root, where
/// the box roots are. In this fixture every directory is on one file system, so the device numbers
/// alone cannot tell the host store from the fleet's disk: the files the box wrote are also looked
/// for on this side, under the fleet root and not in the host's `store/cache`.
///
/// A hard link is what pnpm makes from its store into `node_modules`, and it cannot cross from one
/// mount to another even on one file system: link(2) answers EXDEV. The area and the checkout are
/// two binds, so pnpm's store is in the box root, the bind every worktree under `tree` is in. The
/// link is made here as pnpm makes it. The reason uv still copies is held too: a link from uv's
/// cache in the area into the checkout fails, and if it ever stops failing UV_LINK_MODE=copy can go.
///
/// What would make it fail: a cache pointed back at the host's `store/cache` (its file turns up
/// there, not in the area); pnpm's store put in the area with the other caches ("pnpm copied"),
/// which is SKEIN-1429.
#[test]
fn the_caches_are_on_the_trees_file_system_and_pnpm_hard_links_into_a_worktree() {
    if !bwrap_works() {
        skip("bwrap cannot make a user namespace here, so where a repo's caches live is unchecked");
        return;
    }
    let f = Fleet::make("repotoolsfs");
    fs::create_dir_all(f.repos.join("web/store/cache")).unwrap();
    let web_area = f.fleet_root.join(".repo-tools/web");
    let probe = r#"t="$tree/.worktrees/w/node_modules/.pnpm/p"; mkdir -p "$t"
for c in CARGO_HOME npm_config_cache PIP_CACHE_DIR UV_CACHE_DIR npm_config_store_dir; do
  d="${!c}"; mkdir -p "$d" && printf x > "$d/skein-test-$c"
  [ "$(stat -c %d "$d")" = "$(stat -c %d "$tree")" ] && echo "$c same" || echo "$c other"
done
ln "$npm_config_store_dir/skein-test-npm_config_store_dir" "$t/f" 2>/dev/null && echo "pnpm linked" || echo "pnpm copied"
ln "$UV_CACHE_DIR/skein-test-UV_CACHE_DIR" "$tree/uvf" 2>/dev/null && echo "uv linked" || echo "uv copied"
"#;
    let said = in_repo_box(&f, "web-main", "web", probe, &web_area);
    for c in [
        "CARGO_HOME",
        "npm_config_cache",
        "PIP_CACHE_DIR",
        "UV_CACHE_DIR",
        "npm_config_store_dir",
    ] {
        assert_eq!(
            answer(&said, c),
            "same",
            "{c} is not on the file system of the box's checkout:\n{said}"
        );
    }
    for (c, rel) in [
        ("CARGO_HOME", "cargo"),
        ("npm_config_cache", "npm"),
        ("PIP_CACHE_DIR", "pip"),
        ("UV_CACHE_DIR", "uv"),
    ] {
        assert!(
            web_area
                .join(format!("cache/{rel}/skein-test-{c}"))
                .is_file(),
            "{c} is not in the repo's area under the fleet root:\n{said}"
        );
        assert!(
            !f.repos
                .join(format!("web/store/cache/{rel}/skein-test-{c}"))
                .exists(),
            "{c} still writes into the host's store/cache"
        );
    }
    assert_eq!(
        answer(&said, "pnpm"),
        "linked",
        "pnpm's store cannot hard link into a worktree of the box, so pnpm copies every file \
         (SKEIN-1429):\n{said}"
    );
    assert_eq!(
        answer(&said, "uv"),
        "copied",
        "uv's cache in the area now hard links into the checkout, so UV_LINK_MODE=copy in the \
         session block can go:\n{said}"
    );
}

/// **A link where the area should be is refused, and the box starts without one** (SKEIN-1431).
/// The launcher makes and binds the area at fleet scope, so a link planted there would have it
/// bind whatever the link leads to as this repo's writable area. Here it leads to another repo's.
///
/// What would make it fail: binding the area by name, or without asking where the held descriptor
/// really leads (the box writes through the link into the other repo's area).
#[test]
fn a_link_where_the_area_should_be_is_refused() {
    if !bwrap_works() {
        skip("bwrap cannot make a user namespace here, so a link at a repo's area is unchecked");
        return;
    }
    let f = Fleet::make("repotoolslink");
    let other_area = f.fleet_root.join(".repo-tools/other");
    fs::create_dir_all(&other_area).unwrap();
    std::os::unix::fs::symlink(&other_area, f.fleet_root.join(".repo-tools/web")).unwrap();
    let said = in_repo_box(
        &f,
        "web-main",
        "web",
        "printf 'area %s\\n' \"${SKEIN_REPO_TOOLS:-none}\"; \
         { mkdir -p \"$HOME/.repo-tools/tools\" && printf x > \"$HOME/.repo-tools/tools/x\"; } 2>/dev/null; true",
        &other_area,
    );
    assert_eq!(answer(&said, "area"), "none", "{said}");
    assert!(
        fs::read_dir(&other_area).unwrap().next().is_none(),
        "a box wrote into another repo's area through a link at its own"
    );
}
