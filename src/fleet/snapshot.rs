//! A box's snapshot: the unpushed work and the ignored files worth carrying, packed so a
//! rebuilt box comes back with them.

use super::*;

/// Everything in a box that is not already on a remote, written into the repo's host-mounted store.
///
/// The fleet sandbox's memory and CPUs are fixed when it is created, so changing them means
/// destroying it — and every box's checkout is VM-local, which is exactly what makes builds fast and
/// makes this necessary. Committed-but-unpushed work, staged changes, unstaged changes and untracked
/// files each need their own artifact: a bundle preserves history a patch cannot, and `git diff`
/// covers neither untracked files nor the index/worktree distinction.
///
/// Returns the path *relative to the store*, which is the form the launch spec carries and the
/// provisioning script validates — it refuses anything not under `skein/handoff-snapshots/`.
pub fn snapshot_box(name: &str, store: &str, run: &str) -> Result<String, String> {
    let relative = format!("skein/handoff-snapshots/{name}/{run}");
    let snapshot = format!("{store}/{relative}");
    // Addressed from the SANDBOX, not through the box's namespace.
    //
    // A snapshot exists to rescue work, so requiring the box's *session* to be alive to take one is
    // backwards — and it fails exactly when it is needed most: a fleet box loses its tmux server
    // whenever the sandbox cycles, and the first thing resize did was refuse with `nsenter: cannot
    // open /proc/<pid>/ns/user`, leaving the work it was trying to save unreachable.
    //
    // Nothing here needs the namespace anyway. A box's tree and its private HOME are ordinary
    // directories in the sandbox (`/boxes/<name>/{tree,home}`), so naming them directly reads the
    // same bytes without entering anything. A legacy box keeps the old path: its sandbox IS the box.
    let placed = shared_record(name);
    let (boxed, enter, home) = match &placed {
        Some(record) => (
            own_sandbox(&record.sandbox),
            format!("cd {}; ", sh_quote(&format!("{}/tree", box_root(name)))),
            format!("{}/home", box_root(name)),
        ),
        None => (
            place_of(name).ok_or_else(|| format!("box {name} is not placed"))?,
            String::new(),
            "$HOME".to_string(),
        ),
    };
    let build = format!("{enter}{}", snapshot_script(&snapshot, name, &home));
    boxed.exec(&build, Duration::from_secs(600))?;

    // What the sweep refused to carry, said out loud. A snapshot that quietly leaves things behind
    // is worse than one that carries less: the box comes back looking complete.
    let skipped = std::fs::read_to_string(format!("{snapshot}/{SKIPPED_FILE}")).unwrap_or_default();
    let lines: Vec<&str> = skipped.lines().filter(|l| !l.trim().is_empty()).collect();
    if !lines.is_empty() {
        eprintln!(
            "skein: {name}'s snapshot leaves {} ignored path(s) behind — {}{}. \
             They are build output or dependencies by size; rebuild them in the box.",
            lines.len(),
            lines.iter().take(3).copied().collect::<Vec<_>>().join(", "),
            if lines.len() > 3 { ", …" } else { "" }
        );
    }
    Ok(relative)
}

/// Where the snapshot records the ignored paths it decided not to carry.
const SKIPPED_FILE: &str = "skipped-ignored.txt";

fn ignored_sweep(dir: &str, list: &str, skipped: &str) -> String {
    const FILE_KB: u64 = 10 * 1024;
    const DIR_KB: u64 = 20 * 1024;
    const DIR_FILES: u64 = 2000;
    let d = sh_quote(dir);
    format!(
        "git ls-files --others --ignored --exclude-standard --directory -z \
           -- . ':(exclude).claude' ':(exclude).claude/**' > {d}/ignored.list; \
         : > {skipped_q}; \
         while IFS= read -r -d '' p; do \
           case \"$p\" in \
             */) \
               n=$(find \"$p\" -type f 2>/dev/null | head -n {over} | wc -l); \
               if [ \"$n\" -ge {over} ]; then \
                 printf '%s (over {DIR_FILES} files)\\n' \"$p\" >> {skipped_q}; continue; \
               fi; \
               kb=$(du -sk \"$p\" 2>/dev/null | cut -f1); \
               case \"$kb\" in ''|*[!0-9]*) kb=0 ;; esac; \
               if [ \"$kb\" -gt {DIR_KB} ]; then \
                 printf '%s (%s MB)\\n' \"$p\" \"$((kb/1024))\" >> {skipped_q}; continue; \
               fi ;; \
             *) \
               kb=$(( $(wc -c < \"$p\" 2>/dev/null || echo 0) / 1024 )); \
               if [ \"$kb\" -gt {FILE_KB} ]; then \
                 printf '%s (%s MB)\\n' \"$p\" \"$((kb/1024))\" >> {skipped_q}; continue; \
               fi ;; \
           esac; \
           printf '%s\\0' \"$p\" >> {d}/{list}; \
         done < {d}/ignored.list; \
         rm -f {d}/ignored.list; ",
        over = DIR_FILES + 1,
        skipped_q = sh_quote(skipped),
    )
}

/// Everything a box's work is, written into the store: its commits, its index, its worktree, the
/// files git is not tracking, and the agent's own state.
///
/// **Ignored files are work too.** The sweep used to be `--others --exclude-standard`, which lists
/// untracked files and deliberately omits ignored ones — so `.env`, `.envrc`, local dev config and
/// the box's own `.skein/journal.md` were silently left behind on every migration and every resize.
/// The box came back looking complete and failed at runtime, or came back having forgotten what it
/// had been doing, which is worse than an error because nothing announces it.
///
/// The reason it cannot simply carry everything ignored is `node_modules/` and `target/`: this runs
/// for every box, into a host directory, and a resize does the whole fleet at once. So the rule is
/// **size, not names** — a hand-written list of build directories is a list that is wrong for the
/// next language. An ignored *file* is carried unless it is very large; an ignored *directory* is
/// carried when it is small enough to be config rather than artefacts. `.skein/` and `.env` pass;
/// a dependency tree does not. The file-count probe short-circuits at its threshold, so a directory
/// with 200k files costs one bounded `find` rather than a walk of the whole thing.
///
/// Whatever is refused is written to [`SKIPPED_FILE`] and reported by the caller.
///
/// **The bundle carries what the remote does not have, not the whole history.** `--all` on its own
/// wrote every object the repository has ever held into the store, over a virtiofs mount that is
/// several times slower to write than local disk: one box with a 1.8 GB `.git` took a migration past
/// its ten-minute budget doing nothing but copying history that already exists on the remote. A
/// resize does that for every box at once.
///
/// `--not --remotes` leaves a bundle of exactly the commits that would otherwise be lost, with the
/// rest recorded as prerequisites. That is safe *because of how the box is rebuilt*: the replacement
/// is cloned from the same source this box was, so every prerequisite is already in it before the
/// bundle is opened. Unpushed local commits are by definition not reachable from a remote ref, so
/// they are all still in there — which is the entire job.
///
/// The trimmed bundle is then **checked for the box's own branch**, and that check is the whole
/// safety of this. `--not --remotes` drops any ref whose tip the remote already has, so a box whose
/// checked-out branch is fully pushed gets a bundle without it — and if some *other* local ref is
/// unpushed the bundle is still non-empty, so it looks perfectly healthy. Measured: a 127 KB bundle
/// with no `refs/heads/<branch>` and no HEAD, and a restore that died on `couldn't find remote ref
/// HEAD` after the old sandbox had already been stopped.
///
/// When the branch is missing — or the bundle would be empty, which git refuses to write — the
/// fallback carries the tip commit alone, its parent recorded as a prerequisite. That is a ref the
/// restore can find, and still nothing like the full history. `--all` remains the last resort, for a
/// repository too young to have a parent commit.
fn snapshot_script(snapshot: &str, name: &str, home: &str) -> String {
    let s = sh_quote(snapshot);
    let skipped = format!("{snapshot}/{SKIPPED_FILE}");
    let sweep_ignored = ignored_sweep(snapshot, "untracked.list", &skipped);
    format!(
        "set -e; mkdir -p {s}; \
         b=\"$(git rev-parse --abbrev-ref HEAD)\"; \
         if ! {{ git bundle create {s}/repo.bundle --all --not --remotes 2>/dev/null \
                && git bundle list-heads {s}/repo.bundle 2>/dev/null \
                   | awk -v r=\"refs/heads/$b\" '$2==r{{f=1}} END{{exit !f}}'; }}; then \
           git bundle create {s}/repo.bundle 'HEAD~1..HEAD' 2>/dev/null \
             || git bundle create {s}/repo.bundle --all; \
         fi; \
         git diff --cached --binary HEAD > {s}/index.patch; \
         git diff --binary > {s}/worktree.patch; \
         git ls-files --others --exclude-standard -z -- . ':(exclude).claude' ':(exclude).claude/**' > {s}/untracked.list; \
         {sweep_ignored}\
         {pack}\
         printf '{{\"box\":\"%s\",\"branch\":\"%s\",\"head\":\"%s\"}}\\n' {n} \
           \"$(git rev-parse --abbrev-ref HEAD)\" \"$(git rev-parse HEAD)\" > {s}/manifest.json; \
         {agent_state}",
        n = sh_quote(name),
        pack = pack_carried(snapshot, "untracked.list", "untracked.tgz", &skipped),
        // A box already in the fleet host-binds its transcript; one being migrated in does not.
        agent_state = agent_state_tar(snapshot, home),
    )
}

/// Tar the swept paths, resolving the symlinks that would not survive the move.
///
/// A `--clone` sandbox bind-mounts the host repository at `/run/sandbox/source`, and a box's `.env`
/// is often a symlink into it — that is how the box reads the host's environment file without a
/// copy. tar preserves a symlink *as a symlink*, so the rescue faithfully carried a pointer to a
/// mount that does not exist in the fleet, and the box got a dangling link where its config should
/// be. It looked like the file was there, which is the worst of both outcomes.
///
/// So a symlink is carried as a symlink only while it still points inside the tree, where it will
/// mean the same thing after the move. One that points outside is carried as its *content*, since
/// the thing worth keeping is what it resolves to. One that already resolves to nothing is reported
/// rather than carried — there is nothing behind it to take.
///
/// Two passes into one archive rather than two archives: `-r` appends, `-h` dereferences, and each
/// path appears exactly once, so an extraction that refuses to overwrite still lands the right thing.
fn pack_carried(dir: &str, list: &str, archive: &str, skipped: &str) -> String {
    let d = sh_quote(dir);
    format!(
        "root=\"$(git rev-parse --show-toplevel 2>/dev/null || pwd)\"; \
         : > {d}/carry.list; : > {d}/deref.list; \
         while IFS= read -r -d '' p; do \
           if [ -L \"$p\" ]; then \
             if [ ! -e \"$p\" ]; then \
               printf '%s (dangling symlink -> %s)\\n' \"$p\" \"$(readlink \"$p\")\" >> {skipped_q}; \
               continue; \
             fi; \
             case \"$(readlink -f \"$p\" 2>/dev/null)\" in \
               \"$root\"/*) ;; \
               *) printf '%s\\0' \"$p\" >> {d}/deref.list; continue ;; \
             esac; \
           fi; \
           printf '%s\\0' \"$p\" >> {d}/carry.list; \
         done < {d}/{list}; \
         if [ -s {d}/carry.list ]; then tar --null -T {d}/carry.list -cf {d}/carry.tar; \
         else tar -cf {d}/carry.tar --files-from /dev/null; fi; \
         if [ -s {d}/deref.list ]; then tar --null -T {d}/deref.list -rhf {d}/carry.tar; fi; \
         gzip -c {d}/carry.tar > {archive_q}; \
         rm -f {d}/carry.tar {d}/carry.list {d}/deref.list {d}/{list}; ",
        skipped_q = sh_quote(skipped),
        archive_q = sh_quote(&format!("{dir}/{archive}")),
    )
}

/// The parts of a box's private `$HOME` that a rebuilt box needs and cannot get any other way.
///
/// An **allowlist**, and that is the whole design. The same argument that made `box-session.sh`
/// private-by-default applies in reverse here: an agent harness keeps state wherever it likes, and
/// with an exclude list anything unanticipated — a new token cache, a new auth file — would be
/// copied into the repo's store, which is host-side shared data. Named paths mean a harness change
/// costs a lost todo list, never a leaked credential.
///
/// `.credentials.json` is therefore not here, and does not need to be: `box-session.sh` seeds the
/// box's `~/.claude` from the sandbox's on first start, so the rebuilt box is already logged in.
///
/// The **transcript is deliberately not here.** A box host-binds `~/.claude/projects`, so the
/// conversation is already durable and already exactly where the rebuilt box will look — tarring it
/// would copy a virtiofs directory out to the store and straight back, twice over the slow path, to
/// arrive at the file that never left.
///
/// This used to be conditional, because a box migrating in from its own VM kept the transcript on
/// VM-local disk and would have lost it. There is no such box any more: every box lives in the shared
/// sandbox with a host-bound home, so the condition had exactly one reachable value.
fn agent_state_tar(snapshot: &str, home: &str) -> String {
    let carried: Vec<&str> = vec![
        ".claude/history.jsonl", // the prompt history
        ".claude/todos",         // in-flight task list
        ".claude.json",          // per-box MCP registration + project state
        ".codex/history.jsonl",
    ];
    let list = carried
        .iter()
        .map(|p| sh_quote(p))
        .collect::<Vec<_>>()
        .join(" ");
    // Only the paths that exist: tar fails the whole archive on a missing member, and which of these
    // a box has depends on which runtime it ran.
    // `home` is the box's private HOME as seen from wherever this runs: an absolute path in the
    // sandbox for a fleet box, and literally `$HOME` for a legacy one entered through its own place.
    // `home` is quoted like everything else here. It was not — `"{h}/$p"` and `tar -C "{h}"` took
    // it raw inside double quotes — and `home` is `<fleet root>/<box>/home`, so the box name was in
    // it and a `$(` in one would have been substituted. `valid_name` now refuses such a name, and
    // this quotes it anyway: the guard and the escaping are two mechanisms and neither should be
    // load-bearing alone. A `$HOME` for a legacy box is passed as the literal string `$HOME`, which
    // is why the shell variable is expanded into `h` by the caller rather than quoted here.
    format!(
        "h={h}; have=''; for p in {list}; do [ -e \"$h/$p\" ] && have=\"$have $p\"; done; \
         if [ -n \"$have\" ]; then tar -C \"$h\" -czf {s}/agent-state.tgz $have; \
         else tar -czf {s}/agent-state.tgz --files-from /dev/null; fi",
        h = match home {
            "$HOME" => "\"$HOME\"".to_string(),
            path => sh_quote(path),
        },
        s = sh_quote(snapshot),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::*;

    // The transcript must NOT be carried, and the HOME must be named absolutely. Both used to depend
    // on a `transcript_is_vm_local` flag that only a migration ever set true; with the per-VM model
    // gone there is one shape left, and this pins it so a rebuild does not start copying a host-bound
    // directory out to the store and back.
    #[test]
    fn a_rebuilt_box_leaves_its_host_bound_conversation_where_it_is() {
        let already_in = agent_state_tar("/snap", "/boxes/web-main/home");
        assert!(
            !already_in.contains(".claude/projects") && !already_in.contains(".codex/sessions"),
            "the transcript is host-bound already; copying it is pure virtiofs waste: {already_in}"
        );
        assert!(
            already_in.contains(".claude/todos") && already_in.contains(".claude.json"),
            "the state that is NOT host-bound still has to travel: {already_in}"
        );

        // A fleet box's HOME is named absolutely, because the tar runs in the SANDBOX rather than
        // inside the box — that is what lets a box whose session has died still have its work saved.
        assert!(
            already_in.contains("/boxes/web-main/home") && !already_in.contains("$HOME"),
            "a dead box's private HOME must still be addressable: {already_in}"
        );

        // The allowlist rule — this lands in host-side shared data, so a credential must never be in it.
        assert!(
            !already_in.contains("credentials"),
            "a credential would be copied into the repo store: {already_in}"
        );
    }

    // The snapshot carries what the remote does not have. `--all` copied every object the repo had
    // ever held into the store, over a mount several times slower than local disk: a box with a
    // 1.8 GB .git took a migration past its ten-minute budget copying history the remote already
    // had, and a resize would do that for every box at once.
    #[test]
    fn a_snapshot_bundles_the_unpushed_work_not_the_whole_history() {
        use std::fs;
        let dir = tempdir();
        let origin = dir.join("origin");
        let tree = dir.join("tree");
        let home = dir.join("home");
        fs::create_dir_all(&origin).unwrap();
        fs::create_dir_all(&home).unwrap();
        let sh = |cwd: &std::path::Path, script: &str| -> std::process::Output {
            std::process::Command::new("bash")
                .current_dir(cwd)
                .arg("-c")
                .arg(script)
                .env("HOME", &home)
                .env("GIT_AUTHOR_NAME", "t")
                .env("GIT_AUTHOR_EMAIL", "t@e")
                .env("GIT_COMMITTER_NAME", "t")
                .env("GIT_COMMITTER_EMAIL", "t@e")
                .output()
                .unwrap()
        };
        // History the remote already has: one large blob, the stand-in for a 1.8 GB .git.
        let out = sh(&origin, "git init -q -b main .");
        assert!(out.status.success(), "{out:?}");
        fs::write(origin.join("big.bin"), vec![7u8; 6 * 1024 * 1024]).unwrap();
        let out = sh(&origin, "git add -A && git commit -qm history");
        assert!(out.status.success(), "{out:?}");

        let out = sh(
            &dir,
            &format!("git clone -q {} {}", origin.display(), tree.display()),
        );
        assert!(out.status.success(), "{out:?}");
        // The only thing that would actually be lost: one small unpushed commit.
        fs::write(tree.join("work.txt"), "the unpushed work\n").unwrap();
        let out = sh(&tree, "git add -A && git commit -qm unpushed");
        assert!(out.status.success(), "{out:?}");

        let snapshot = dir.join("snap");
        let script = snapshot_script(
            &snapshot.to_string_lossy(),
            "demo-main",
            &home.to_string_lossy(),
        );
        let out = sh(&tree, &script);
        assert!(out.status.success(), "snapshot failed: {out:?}");

        let bundle = snapshot.join("repo.bundle");
        let size = fs::metadata(&bundle).unwrap().len();
        assert!(
            size < 1024 * 1024,
            "the bundle re-copied history the remote already has: {size} bytes"
        );
        // Small, and still complete: the unpushed commit is in there, and a fresh clone of the same
        // origin — which is exactly what the box is rebuilt from — can open it.
        let restored = dir.join("restored");
        let out = sh(
            &dir,
            &format!("git clone -q {} {}", origin.display(), restored.display()),
        );
        assert!(out.status.success(), "{out:?}");
        let out = sh(
            &restored,
            &format!(
                "git fetch -q {} 'refs/heads/*:refs/remotes/snapshot/*' && \
                 git checkout -q -B main refs/remotes/snapshot/main && cat work.txt",
                bundle.display()
            ),
        );
        assert!(
            out.status.success() && String::from_utf8_lossy(&out.stdout).contains("unpushed work"),
            "a thin bundle must still restore the work it was taken for: {out:?}"
        );

        // A box with nothing unpushed asks git for an empty bundle, which git refuses to write. The
        // fallback carries the tip alone — the restore needs *a* bundle, and the kit treats a
        // missing one as a failed snapshot.
        let clean = dir.join("clean");
        let out = sh(
            &dir,
            &format!("git clone -q {} {}", origin.display(), clean.display()),
        );
        assert!(out.status.success(), "{out:?}");
        let snapshot2 = dir.join("snap2");
        let out = sh(
            &clean,
            &snapshot_script(
                &snapshot2.to_string_lossy(),
                "demo-main",
                &home.to_string_lossy(),
            ),
        );
        assert!(
            out.status.success(),
            "snapshot of a clean box failed: {out:?}"
        );
        let size2 = fs::metadata(snapshot2.join("repo.bundle")).unwrap().len();
        assert!(
            size2 > 0,
            "the kit reads a missing bundle as a failed snapshot"
        );
        assert!(
            size2 < 1024 * 1024,
            "a clean box must not fall back to the whole history: {size2} bytes"
        );

        // The shape that actually broke a migration: the checked-out branch is fully pushed, but
        // ANOTHER local ref is not. `--not --remotes` drops the pushed branch's ref while the other
        // keeps the bundle non-empty — so it looked healthy and carried nothing the restore could
        // find, and the box's old sandbox had already been stopped by the time that surfaced.
        let out = sh(
            &clean,
            "git checkout -q -b side && git commit -q --allow-empty -m side && git checkout -q main",
        );
        assert!(out.status.success(), "{out:?}");
        let snapshot3 = dir.join("snap3");
        let out = sh(
            &clean,
            &snapshot_script(
                &snapshot3.to_string_lossy(),
                "demo-main",
                &home.to_string_lossy(),
            ),
        );
        assert!(out.status.success(), "{out:?}");
        let restored2 = dir.join("restored2");
        let out = sh(
            &dir,
            &format!("git clone -q {} {}", origin.display(), restored2.display()),
        );
        assert!(out.status.success(), "{out:?}");
        // The invariant the restore depends on: a ref it can actually check the branch out from.
        let bundle3 = snapshot3.join("repo.bundle");
        let out = sh(
            &restored2,
            &format!(
                "git fetch -q {b} 'refs/heads/*:refs/remotes/snapshot/*' 2>/dev/null && \
                 git rev-parse --verify -q refs/remotes/snapshot/main >/dev/null || \
                 git fetch -q {b} HEAD",
                b = bundle3.display()
            ),
        );
        assert!(
            out.status.success(),
            "the restore must find a ref for the box's branch: {out:?}"
        );
    }

    // Ignored files are work too — `.env`, `.envrc`, local dev config, and the box's own
    // `.skein/journal.md`. The sweep omitted every one of them, so a migrated box came back looking
    // complete and either failed at runtime or had forgotten what it was doing. What it must still
    // refuse is `node_modules/` and `target/`, which is why the rule is size rather than a list of
    // names that would be wrong for the next language.
    //
    // Runs the real script against a real repository: it is shell, and shell is where the bug was.
    #[test]
    fn a_snapshot_carries_ignored_config_but_not_the_build_output() {
        use std::fs;
        let dir = tempdir();
        let tree = dir.join("tree");
        let snapshot = dir.join("snap");
        let home = dir.join("home");
        fs::create_dir_all(&tree).unwrap();
        fs::create_dir_all(&home).unwrap();
        let sh = |cwd: &std::path::Path, script: &str| -> std::process::Output {
            std::process::Command::new("bash")
                .current_dir(cwd)
                .arg("-c")
                .arg(script)
                .env("HOME", &home)
                .env("GIT_AUTHOR_NAME", "t")
                .env("GIT_AUTHOR_EMAIL", "t@e")
                .env("GIT_COMMITTER_NAME", "t")
                .env("GIT_COMMITTER_EMAIL", "t@e")
                .output()
                .unwrap()
        };
        fs::write(
            tree.join(".gitignore"),
            ".env\n.skein/\nnode_modules/\nbig.bin\n",
        )
        .unwrap();
        fs::write(tree.join("src.rs"), "fn main() {}").unwrap();
        let out = sh(
            &tree,
            "git init -q -b main . && git add -A && git commit -qm one",
        );
        assert!(out.status.success(), "{out:?}");

        // Ignored, and all of it work: local config and the box's own journal.
        fs::write(tree.join(".env"), "SECRET=1\n").unwrap();
        fs::create_dir_all(tree.join(".skein")).unwrap();
        fs::write(tree.join(".skein/journal.md"), "what I did\n").unwrap();
        // Ignored, and none of it work: reproducible output, too big to keep copying.
        fs::create_dir_all(tree.join("node_modules/pkg")).unwrap();
        for i in 0..2100 {
            fs::write(tree.join(format!("node_modules/pkg/f{i}")), "x").unwrap();
        }
        fs::write(tree.join("big.bin"), vec![0u8; 11 * 1024 * 1024]).unwrap();
        // Plain untracked files must still be carried, exactly as before.
        fs::write(tree.join("notes.txt"), "scratch\n").unwrap();

        // The three symlink shapes, which is where the rescue went wrong. A `--clone` box's `.env`
        // is a link into `/run/sandbox/source` — the host repo's bind mount — and carrying the LINK
        // hands the fleet box a pointer to a mount that does not exist there.
        use std::os::unix::fs::symlink;
        let outside = dir.join("host-repo");
        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join("env.real"), "FROM_HOST=1\n").unwrap();
        symlink(outside.join("env.real"), tree.join(".env.host")).unwrap();
        symlink("notes.txt", tree.join("notes.link")).unwrap();
        symlink("/run/sandbox/source/.env.gone", tree.join(".env.dangling")).unwrap();
        fs::write(
            tree.join(".gitignore"),
            ".env\n.env.host\n.env.dangling\n.skein/\nnode_modules/\nbig.bin\n",
        )
        .unwrap();

        let script = snapshot_script(
            &snapshot.to_string_lossy(),
            "demo-main",
            &home.to_string_lossy(),
        );
        let out = sh(&tree, &script);
        assert!(out.status.success(), "snapshot failed: {out:?}");

        let listing = sh(&snapshot, "tar -tzf untracked.tgz");
        let carried = String::from_utf8_lossy(&listing.stdout);
        for want in [".env", ".skein/journal.md", "notes.txt"] {
            assert!(
                carried.lines().any(|l| l.trim_end_matches('/') == want),
                "{want} was left behind: {carried}"
            );
        }
        assert!(
            !carried.contains("node_modules"),
            "a dependency tree does not belong in the store: {carried}"
        );
        assert!(!carried.contains("big.bin"), "{carried}");

        // And it says so, rather than leaving them behind quietly.
        let skipped = fs::read_to_string(snapshot.join(SKIPPED_FILE)).unwrap();
        assert!(skipped.contains("node_modules/"), "{skipped}");
        assert!(skipped.contains("big.bin"), "{skipped}");

        // A link out of the tree is carried as its CONTENT: what it points at will not be there
        // after the move, and the content is the thing worth keeping.
        let unpacked = dir.join("unpacked");
        fs::create_dir_all(&unpacked).unwrap();
        let out = sh(
            &unpacked,
            &format!("tar -xzf {}/untracked.tgz", snapshot.display()),
        );
        assert!(out.status.success(), "{out:?}");
        assert!(
            !unpacked.join(".env.host").is_symlink(),
            "a link into a mount the fleet does not have is a dangling link there"
        );
        assert_eq!(
            fs::read_to_string(unpacked.join(".env.host")).unwrap(),
            "FROM_HOST=1\n"
        );
        // A link INSIDE the tree still means the same thing after the move, so it stays a link.
        assert!(
            unpacked.join("notes.link").is_symlink(),
            "an in-tree symlink must not be flattened into a copy"
        );
        // And one that already resolves to nothing is reported, not carried: there is nothing there.
        assert!(
            skipped.contains(".env.dangling"),
            "a broken link must be named, not silently dropped: {skipped}"
        );
        assert!(!unpacked.join(".env.dangling").exists());
    }

    // What counts as work worth carrying is one rule, used by the snapshot and by the sweep it calls.
    // Two copies of it would drift, and the drift would be silent — a box would come back missing a
    // file nobody noticed it had.
    #[test]
    fn the_snapshot_carries_exactly_what_the_sweep_says_is_worth_carrying() {
        let sweep = ignored_sweep("/snap", "list", "/snap/skipped");
        let snapshot = snapshot_script("/snap", "demo-main", "/home/agent");
        assert!(
            snapshot.contains("--others --ignored --exclude-standard --directory"),
            "the snapshot must sweep ignored files at all"
        );
        for rule in ["over 2000 files", "-gt 20480", "-gt 10240"] {
            assert!(sweep.contains(rule), "the sweep lost a rule: {rule}");
            assert!(snapshot.contains(rule), "the snapshot lost a rule: {rule}");
        }
    }
}
