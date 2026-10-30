//! A box's agent removes what skein listed for it, inside the box and as the box (SKEIN-1356).

use super::*;

/// The server's tool calls, one JSON-RPC line each.
fn calls(paths: &[String]) -> String {
    let call = serde_json::json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
        "params": {"name": "skein_clean", "arguments": {"paths": paths}}});
    format!("{call}\n")
}

/// Start covered box `web-main` of repo `web` with the launcher's isolation block and repo-cache
/// block, its `$HOME` bound from `home_from` as the launcher binds `<box>/home`, and run the
/// shipped `skein-mcp` in it with `input` on stdin. Returns what the server printed.
fn clean_in_box(f: &Fleet, server: &Path, home_from: &Path, input: &str) -> String {
    clean_in_box_with(f, server, home_from, input, "", "exec python3 \"$1\"")
}

/// [`clean_in_box`], with `extra` appended to the bwrap arguments after the launcher's, and `shell`
/// run by `/bin/sh` in the box in place of the server alone, with the server's path as `$1`.
fn clean_in_box_with(
    f: &Fleet,
    server: &Path,
    home_from: &Path,
    input: &str,
    extra: &str,
    shell: &str,
) -> String {
    let root = f.fleet_root.join("web-main");
    let state = f.state_parent.join("web-main");
    let boxhome = f.dir.path().join("boxhome");
    fs::create_dir_all(&boxhome).unwrap();
    let q = |p: &Path| skein::util::sh_quote(p.to_string_lossy().as_ref());
    let runner = format!(
        "set -uo pipefail\n\
         binds=()\n\
         box=web-main\n\
         root={root}\n\
         state={state}\n\
         export SKEIN_FLEET_ROOT={fleet} SKEIN_BOX_PRIVILEGED=0 \
         SKEIN_FLEET_MOUNTS={mounts} SKEIN_BOX_STORE={store}\n\
         {iso}\n\
         {cache}\n\
         binds+=(--bind {home_from} {boxhome} {extra})\n\
         exec bwrap --dev-bind / / ${{binds[@]+\"${{binds[@]}}\"}} -- \
           /usr/bin/env HOME={boxhome} SKEIN_STATE=\"$state\" SKEIN_BOX=web-main \
           /bin/sh -c {shell} skein-probe {server}\n",
        root = q(&root),
        state = q(&state),
        fleet = q(&f.fleet_root),
        mounts = skein::util::sh_quote(&f.mounts(Born::Covered)),
        store = q(&f.store()),
        iso = isolation_block(),
        cache = super::repocache::repo_cache_block(),
        home_from = q(home_from),
        boxhome = q(&boxhome),
        server = q(server),
        shell = skein::util::sh_quote(shell),
    );
    let mut child = Command::new("bash")
        .arg("-c")
        .arg(&runner)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("bash");
    use std::io::Write;
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(
        out.status.success(),
        "the box did not start: {}\n--- script ---\n{runner}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).to_string()
}

/// **In a real covered box, `skein_clean` removes the box's own cache and the repo's shared cache
/// that skein listed, and nothing a link in them leads to** (SKEIN-1356).
///
/// The link's target is the repo's `store/data`, which the box can reach and write: the case where
/// only the removal's own refusal to follow the link keeps it. And the tool believes skein's list
/// only because `<box>/home` and `$HOME` are one directory through the launcher's bind; with a
/// different directory bound at `$HOME`, the same list is refused.
///
/// What would make it fail: the walk following links (`store/data/keep/precious` is removed);
/// the repo cache not bound back into the covered box (its removal fails); the plugin trusting
/// the list without the inode check (the control's removal goes ahead). Watched: with links
/// followed in `remove_tree_at`, `precious` was removed through the link and this failed.
#[test]
fn a_box_removes_what_skein_listed_and_nothing_a_link_leads_to() {
    if !bwrap_works() {
        return skip(
            "bwrap cannot create a user namespace here, so skein_clean was NOT exercised in a box",
        );
    }
    let f = Fleet::make("agentclean");
    let root = f.fleet_root.join("web-main");
    let cache = f.repos.join("web/store/cache");
    let keep = f.store().join("keep");
    for p in [
        root.join("home/.cache/pip/wheels/x.whl"),
        root.join("tree/README"),
        keep.join("precious"),
        cache.join("pip/http/x"),
        cache.join("cargo/credentials.toml"),
    ] {
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(&p, "data").unwrap();
    }
    std::os::unix::fs::symlink(&keep, root.join("home/.cache/pip/evil")).unwrap();
    let server = f.fleet_root.join(".skein/plugin/bin/skein-mcp");
    fs::create_dir_all(server.parent().unwrap()).unwrap();
    fs::write(&server, include_str!("../../src/plugin/bin/skein-mcp")).unwrap();

    let now = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
    let item = |p: &Path| {
        serde_json::json!({"path": p.display().to_string(), "shown": "x",
                           "kind": "download cache", "bytes": 4096})
    };
    let signals = f.state_parent.join("web-main/signals");
    fs::create_dir_all(&signals).unwrap();
    fs::write(
        signals.join("resources.json"),
        serde_json::json!({
            "at": now, "asks": [],
            "clean": {
                "at": now,
                "box_root": root.display().to_string(),
                "items": [item(&root.join("home/.cache/pip"))],
                "repo_cache": {
                    "path": cache.display().to_string(), "bytes": 4096,
                    "items": [item(&cache.join("pip"))],
                    "others_running": 0, "others_unknown": 0, "none_running_at": now,
                },
            },
        })
        .to_string(),
    )
    .unwrap();
    let asked = vec![
        root.join("home/.cache/pip").display().to_string(),
        cache.join("pip").display().to_string(),
    ];

    // The control: some other directory at `$HOME`, so the list is not this box's.
    let elsewhere = f.dir.path().join("not-the-box-home");
    fs::create_dir_all(&elsewhere).unwrap();
    let refused = clean_in_box(&f, &server, &elsewhere, &calls(&asked));
    assert!(
        refused.contains("not-safe-now") && refused.contains("not this box's"),
        "a list for a directory that is not the box's home was acted on:\n{refused}"
    );
    assert!(root.join("home/.cache/pip/wheels/x.whl").exists());
    assert!(cache.join("pip/http/x").exists());

    let said = clean_in_box(&f, &server, &root.join("home"), &calls(&asked));
    // First, so a removal that followed the link is named as that, whatever else it did.
    assert!(
        keep.join("precious").exists(),
        "the removal followed a link into the repo's store:\n{said}"
    );
    assert!(
        said.contains("removed 2"),
        "the box could not clean:\n{said}"
    );
    assert!(!root.join("home/.cache/pip").exists(), "{said}");
    assert!(!cache.join("pip").exists(), "{said}");
    assert!(cache.join("cargo/credentials.toml").exists());
    assert!(root.join("tree/README").exists());
}

/// **A mount inside a listed cache is left alone, and the rest of the cache is removed**
/// (SKEIN-1356). A removal never leaves the filesystem it started on.
///
/// The choice: remove what is on the box's own filesystem and keep the mount and the directories
/// above it, reporting the item as not removed and naming what was left. Refusing the whole item
/// would leave a cache the agent cannot free at all because something is mounted somewhere inside
/// it; entering the mount would remove what belongs to some other filesystem, which in a box can be
/// a bind of something shared.
///
/// A tmpfs is mounted inside `~/.cache/pip`, as the box's directory is seen through the fleet path
/// the removal walks, and a file is written into it in the box before the clean. The file is read
/// back in the same box afterwards: a tmpfs lives only in the namespace.
///
/// What would make it fail: dropping both device checks in the plugin's `open_dir_at` (the walk
/// then enters the tmpfs and removes the file in it). Watched: with exactly that, the file in the
/// mount was gone and this failed.
#[test]
fn a_mount_inside_a_listed_cache_is_left_alone_and_the_rest_removed() {
    if !bwrap_works() {
        return skip(
            "bwrap cannot create a user namespace here, so skein_clean's filesystem boundary was \
             NOT exercised in a box",
        );
    }
    let f = Fleet::make("agentcleanmount");
    let root = f.fleet_root.join("web-main");
    let pip = root.join("home/.cache/pip");
    let mnt = pip.join("mnt");
    for p in [pip.join("wheels/x.whl"), root.join("tree/README")] {
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(&p, "data").unwrap();
    }
    fs::create_dir_all(&mnt).unwrap();
    let server = f.fleet_root.join(".skein/plugin/bin/skein-mcp");
    fs::create_dir_all(server.parent().unwrap()).unwrap();
    fs::write(&server, include_str!("../../src/plugin/bin/skein-mcp")).unwrap();
    let now = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
    let signals = f.state_parent.join("web-main/signals");
    fs::create_dir_all(&signals).unwrap();
    fs::write(
        signals.join("resources.json"),
        serde_json::json!({
            "at": now, "asks": [],
            "clean": {
                "at": now,
                "box_root": root.display().to_string(),
                "items": [{"path": pip.display().to_string(), "shown": "~/.cache/pip",
                           "kind": "download cache", "bytes": 4096}],
            },
        })
        .to_string(),
    )
    .unwrap();

    let m = skein::util::sh_quote(mnt.to_string_lossy().as_ref());
    let said = clean_in_box_with(
        &f,
        &server,
        &root.join("home"),
        &calls(&[pip.display().to_string()]),
        &format!("--tmpfs {m}"),
        &format!(
            "mkdir -p {m}/deep && echo kept > {m}/deep/inmount && python3 \"$1\"; \
             echo; echo \"inmount: $(cat {m}/deep/inmount 2>/dev/null || echo gone)\""
        ),
    );
    // First, so a removal that entered the mount is named as that, whatever else it did.
    assert!(
        said.contains("inmount: kept"),
        "the removal crossed into a filesystem mounted inside the cache:\n{said}"
    );
    assert!(
        !pip.join("wheels").exists(),
        "the part of the cache on the box's own filesystem was not removed:\n{said}"
    );
    assert!(
        said.contains("another filesystem mounted inside it") && said.contains("left_alone"),
        "the clean did not say what it left:\n{said}"
    );
    assert!(mnt.is_dir(), "the mount point itself was removed:\n{said}");
    assert!(root.join("tree/README").exists());
}
