//! What one box's home can do to another's: a binary one box plants is not what another runs,
//! the shared toolchain is still seen under the private overlay, and the npm prefix the agent
//! runs from is read-only inside a box.

use super::*;

// --- The home binds: what one box can decide about another box's PATH (SKEIN-963, SKEIN-968) -----

/// The launcher's three `$HOME`-relative path lists, lifted as written.
///
/// `seed_paths`, `share_paths` and `overlay_paths` — exactly one assignment of each at column 0,
/// and it **refuses to run** rather than returning a short list, for the reason
/// `unsets_between_blocks` gives: a landmark that moved and a launcher that stopped classifying a
/// path are indistinguishable by a missing line alone.
fn path_lists_block() -> String {
    let src = fs::read_to_string(script("box-session.sh")).unwrap();
    let lines: Vec<&str> = src.lines().collect();
    let mut picked: Vec<&str> = Vec::new();
    for want in ["seed_paths=(", "share_paths=(", "overlay_paths=("] {
        let found: Vec<&str> = lines
            .iter()
            .filter(|l| l.starts_with(want))
            .copied()
            .collect();
        assert_eq!(
            found.len(),
            1,
            "box-session.sh has {} assignments beginning `{want}` at column 0 and this harness \
             needs exactly one: none means the launcher no longer classifies those paths here, \
             and two would make the second silently win. Found: {found:?}",
            found.len()
        );
        picked.push(found[0]);
    }
    picked.join("\n")
}

/// The launcher's `$HOME` bind construction, lifted out of the script it lives in.
///
/// From `binds=(--bind "$home" "$HOME")` — the first thing in the box's mount list — down to the
/// read-only bind of the npm prefix, which is the last of the binds decided from the path lists
/// above. Everything after it needs `$state` and `$root`, which belong to other tests.
///
/// Read rather than copied, so a change to the launcher is a change to what these tests run: the
/// whole point of this file is that skein's own idea of its argv is not the authority on what
/// those mounts mean.
fn home_binds_block() -> String {
    let src = fs::read_to_string(script("box-session.sh")).unwrap();
    let lines: Vec<&str> = src.lines().collect();
    let from = lines
        .iter()
        .position(|l| *l == r#"binds=(--bind "$home" "$HOME")"#)
        .expect("the launcher's `$HOME` bind construction moved");
    let to = lines[from..]
        .iter()
        .position(|l| l.contains("--ro-bind /usr/local/share/npm-global"))
        .map(|i| from + i)
        .expect(
            "box-session.sh no longer read-only-binds /usr/local/share/npm-global, which is the \
             npm prefix `box_path` puts second on every box's PATH (SKEIN-968)",
        );
    let block = lines[from..=to].join("\n");
    for landmark in [
        r#"for rel in "${share_paths[@]}"; do"#,
        r#"for rel in "${overlay_paths[@]}"; do"#,
        "--tmp-overlay",
        r#"binds+=(--bind "$home/.local/state" "$HOME/.local/state")"#,
    ] {
        assert!(
            block.contains(landmark),
            "the lifted bind block no longer holds `{landmark}`, so this harness is running \
             something other than the thing that decides what a box shares"
        );
    }
    // **A range lift can only see what is inside the range**, and the launcher goes on adding to
    // `binds` for another eight hundred lines. A second `binds+=` naming either of these two paths,
    // anywhere after this block, would win over what is in it — bwrap applies mounts in order —
    // and every assertion built on this lift would go on passing while a box got the path back
    // read-write. Caught, not assumed: this was written because a rehearsal that added
    // `binds+=(--bind /usr/local/share/npm-global …)` one line below the block left all three of
    // these tests green.
    //
    // So the harness refuses rather than testing a mount list the launcher does not have. It is a
    // check on the LIFT, not on the subject: what the mounts mean is still decided by running them.
    for path in ["/usr/local/share/npm-global", "$HOME/.local"] {
        let outside: Vec<&str> = lines
            .iter()
            .enumerate()
            .filter(|(i, l)| (*i < from || *i > to) && l.contains("binds+=(") && l.contains(path))
            .map(|(_, l)| l.trim())
            .collect();
        assert!(
            outside.is_empty(),
            "box-session.sh binds {path} into a box from outside the block this harness lifts, so \
             what it runs is not the mount list a box gets — bwrap applies these in order and a \
             later bind wins. Move it inside the block or widen the lift. Found: {outside:?}"
        );
    }
    block
}

/// A sandbox home with a shared toolchain in it, and two boxes that get their own homes over it.
struct Homes {
    dir: Scratch,
    /// The SANDBOX's `$HOME` — the lower layer every box reads `~/.local` from, and the directory
    /// an install made outside any box lands in.
    shared: PathBuf,
}

/// Named `skein-test-` so it cannot collide with anything real and says what it is in a listing.
const SHARED_BIN: &str = "skein-test-shared-agent";

impl Homes {
    fn make(tag: &str) -> Homes {
        use std::os::unix::fs::PermissionsExt;
        // `Scratch::boxes`, not `Scratch::temp`, for the reason `Started::make` gives: a fixture
        // under `/tmp` is unreadable from outside a box that binds its own over it.
        let dir = Scratch::boxes(tag);
        let shared = dir.join("sandbox-home");
        for p in [
            &shared.join(".local/bin"),
            &shared.join(".local/lib/skein-test-pkg"),
            &shared.join(".local/state/skein"),
            &dir.join("box-a"),
            &dir.join("box-b"),
        ] {
            fs::create_dir_all(p).unwrap();
        }
        let f = shared.join(".local/bin").join(SHARED_BIN);
        fs::write(&f, "#!/bin/sh\nprintf %s shared\n").unwrap();
        fs::set_permissions(&f, fs::Permissions::from_mode(0o755)).unwrap();
        // A library rather than a binary, because "the tools are not lost" is about the whole 1.1 GB
        // under `~/.local` and not only about what is on PATH.
        fs::write(shared.join(".local/lib/skein-test-pkg/VERSION"), "1.0\n").unwrap();
        Homes { dir, shared }
    }

    /// Start one box over this sandbox home and run `probe` inside it, with `$HOME` bound to
    /// `box`'s own private home exactly as the launcher does it.
    ///
    /// `rewrite` is applied to the lifted bind block before it runs — the seam the
    /// presence-before-absence halves use, and nothing else.
    fn inside(&self, box_name: &str, probe: &str, rewrite: &dyn Fn(String) -> String) -> String {
        // `box=` because the launcher names the box in the sentence it prints when this bwrap
        // cannot make an overlay, and `set -u` is on in the lifted block exactly as it is in
        // production. A harness that left it undefined would fail every box start on that path and
        // report the subject as broken.
        let runner = format!(
            "set -uo pipefail\n\
             export HOME={home}\n\
             home={boxhome}\n\
             box={boxname}\n\
             {lists}\n\
             {block}\n\
             exec bwrap --dev-bind / / \"${{binds[@]}}\" -- /bin/sh -c {probe} skein-probe\n",
            home = skein::util::sh_quote(self.shared.to_string_lossy().as_ref()),
            boxhome = skein::util::sh_quote(self.dir.join(box_name).to_string_lossy().as_ref()),
            boxname = skein::util::sh_quote(box_name),
            lists = path_lists_block(),
            block = rewrite(home_binds_block()),
            probe = skein::util::sh_quote(probe),
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
}

/// The bind block with the overlay turned back into the plain read-write share it used to be.
///
/// **Derived from the real block rather than written out**, for the reason
/// `a_planted_nsenter_is_not_what_a_crossing_runs` gives: if the fix is ever removed, this
/// substitution finds nothing to do, the two halves become the same command, and it is the
/// ABSENCE assertion that fires — naming the binary one box planted in another — rather than a
/// harness complaining that a shape it expected is missing.
/// **Both branches**, because the launcher has two: the overlay when this bwrap can make one, and a
/// read-only bind when it cannot. Substituting only the first left the `else` in place, so the
/// "before" half ran a READ-ONLY share and reported that the first box could not plant at all —
/// green, for a reason with nothing to do with the fix. Caught by that assertion, which is what it
/// is for.
fn shared_read_write_again(block: String) -> String {
    let plain = r#"binds+=(--bind "$HOME/$rel" "$HOME/$rel")"#;
    block
        .replace(
            r#"binds+=(--overlay-src "$HOME/$rel" --tmp-overlay "$HOME/$rel")"#,
            plain,
        )
        .replace(r#"binds+=(--ro-bind "$HOME/$rel" "$HOME/$rel")"#, plain)
}

/// **A binary one box writes onto another box's PATH is not what that box runs** (SKEIN-963).
///
/// `box_path` is `$HOME/.local/bin:/usr/local/share/npm-global/bin:$PATH`, and `~/.local` was in
/// `share_paths` — bound read-write from the sandbox's real `$HOME` into every box. So
/// `~/.local/bin/claude` written by one box was what every other box executed on its next start:
/// persistent cross-box code execution needing no live target and no exploit, just a file copy
/// into a directory the box already wrote. architecture §9.2 path 1 states the rule this broke in
/// as many words — *no shared writable path may contain anything another box executes*.
///
/// # The change that would make this fail, named before it was written
///
/// Putting `.local` back in `share_paths` — or, equivalently, replacing the launcher's
/// `--overlay-src`/`--tmp-overlay` pair with the `--bind` it used to be. That is not a thought
/// experiment here: [`shared_read_write_again`] performs exactly that substitution, and the FIRST
/// half of this test runs it and asserts the planted binary wins. An absence that was never a
/// presence proves nothing.
#[test]
fn a_binary_one_box_plants_is_not_what_another_box_runs() {
    if !bwrap_works() {
        skip("bwrap cannot make a user namespace here, so what one box can plant in another is unchecked");
        return;
    }
    let f = Homes::make("skein-plant963");
    // One box plants; the other runs. The planting probe reports whether its own write was allowed,
    // because a write that simply failed would make the second half green for the wrong reason.
    let plant = format!(
        "printf '#!/bin/sh\\nprintf %%s planted\\n' > \"$HOME/.local/bin/{SHARED_BIN}\" \
           && chmod 755 \"$HOME/.local/bin/{SHARED_BIN}\" \
           && printf 'wrote yes\\n' || printf 'wrote no\\n'\n\
         printf 'ran %s\\n' \"$(\"$HOME/.local/bin/{SHARED_BIN}\")\"\n"
    );
    let run = format!(
        "printf 'ran %s\\n' \"$(\"$HOME/.local/bin/{SHARED_BIN}\" 2>/dev/null || echo none)\"\n"
    );

    // --- presence: the launcher as it was, with `.local` shared read-write ---
    let planted = f.inside("box-a", &plant, &shared_read_write_again);
    assert_eq!(
        answer(&planted, "wrote"),
        "yes",
        "the first box could not write the shared toolchain at all, so the run below would be \
         green for a reason that has nothing to do with this fix:\n{planted}"
    );
    let victim = f.inside("box-b", &run, &shared_read_write_again);
    assert_eq!(
        answer(&victim, "ran"),
        "planted",
        "with `.local` bound read-write — the shape this fix replaces — a second box did NOT run \
         the binary the first one planted, so this harness is not reproducing the defect and the \
         absence below would prove nothing:\n{victim}"
    );
    // The sandbox's own copy is what the second box read, so it really was replaced on disk.
    assert_eq!(
        fs::read_to_string(f.shared.join(".local/bin").join(SHARED_BIN)).unwrap(),
        "#!/bin/sh\nprintf %s planted\n",
        "the read-write half did not reach the sandbox's own file, so the defect it is reproducing \
         is not the one SKEIN-963 describes"
    );
    // Put it back, so the absence half starts from the same fixture the presence half did.
    let good = f.shared.join(".local/bin").join(SHARED_BIN);
    fs::write(&good, "#!/bin/sh\nprintf %s shared\n").unwrap();

    // --- absence: the launcher as it is ---
    let planted = f.inside("box-a", &plant, &|b| b);
    assert_eq!(
        answer(&planted, "wrote"),
        "yes",
        "a box can no longer write its OWN `~/.local/bin`, which is a different bug: the overlay \
         is meant to redirect that write, not refuse it:\n{planted}"
    );
    assert_eq!(
        answer(&planted, "ran"),
        "planted",
        "the box that planted does not see its own write, so its upper layer is not where its \
         writes go:\n{planted}"
    );
    // The property first, then the mechanism behind it. Both fire under the same sabotage — putting
    // `.local` back in `share_paths` — and this is the order that says which failure it is: "the
    // second box ran the first one's binary" is the defect, and "the sandbox's own copy changed" is
    // the reason. A run that reported only the second would leave a reader working out whether it
    // mattered.
    let victim = f.inside("box-b", &run, &|b| b);
    assert_eq!(
        answer(&victim, "ran"),
        "shared",
        "a second box ran the binary the first one planted on its PATH — this is SKEIN-963, \
         persistent cross-box code execution through `~/.local/bin`:\n{victim}"
    );
    assert_eq!(
        fs::read_to_string(&good).unwrap(),
        "#!/bin/sh\nprintf %s shared\n",
        "a box's write reached the SANDBOX's copy of the shared toolchain — the lower layer is \
         writable, so every box gets it at its next start"
    );
}

/// **A box still sees everything the shared lower layer holds**, so making `~/.local` private per
/// box does not take a fleet's tools away from it (SKEIN-963).
///
/// This is the other half of the decision and the half that is easy to lose: `~/.local` is about
/// 1.1 GB of `pip --user` libraries and agent tooling on the live fleet, every box wants it
/// identical, and an isolation fix that emptied it would be a worse outcome than the defect. So
/// the overlay's LOWER layer is asserted here — on PATH and off it — beside the private state
/// directory that must survive a restart.
///
/// # The change that would make this fail, named before it was written
///
/// Turning the `--overlay-src`/`--tmp-overlay` pair into a `--tmpfs`, or dropping the overlay entry
/// and leaving the box's private `$HOME` bind to shadow `~/.local`. Either is the "give every box
/// an empty `~/.local`" option the tracker comment on SKEIN-963 lists first; both leave every
/// assertion below unable to find a thing that is there today.
#[test]
fn a_box_still_sees_the_shared_toolchain_under_its_private_overlay() {
    if !bwrap_works() {
        skip("bwrap cannot make a user namespace here, so what a box still sees is unchecked");
        return;
    }
    let f = Homes::make("skein-lower963");
    let probe = format!(
        "printf 'onpath %s\\n' \"$(command -v {SHARED_BIN} || echo none)\"\n\
         printf 'ran %s\\n' \"$({SHARED_BIN} 2>/dev/null || echo none)\"\n\
         printf 'lib %s\\n' \"$(cat \"$HOME/.local/lib/skein-test-pkg/VERSION\" 2>/dev/null || echo none)\"\n\
         printf 'state %s\\n' \"$(echo mine > \"$HOME/.local/state/skein-test-stamp\" \
           && echo wrote || echo no)\"\n"
    );
    // PATH is set here rather than taken from the launcher's `box_path`, which is decided in the
    // session block this harness does not run — what is under test is the MOUNT, and the mount is
    // only interesting at the place `box_path` points.
    let seen = f.inside(
        "box-a",
        &format!("PATH=\"$HOME/.local/bin:$PATH\"\n{probe}"),
        &|b| b,
    );
    assert_eq!(
        answer(&seen, "onpath"),
        f.shared
            .join(".local/bin")
            .join(SHARED_BIN)
            .display()
            .to_string(),
        "the shared toolchain is not on the box's own PATH any more, so a box has lost the agent \
         CLI and everything else installed outside it:\n{seen}"
    );
    assert_eq!(
        answer(&seen, "ran"),
        "shared",
        "the file resolved but did not run, so the lower layer is visible and not usable:\n{seen}"
    );
    assert_eq!(
        answer(&seen, "lib"),
        "1.0",
        "a library under `~/.local/lib` is gone — the overlay is showing only what is on PATH, \
         and the 1.1 GB of `pip --user` packages every box shares would be lost:\n{seen}"
    );
    assert_eq!(
        answer(&seen, "state"),
        "wrote",
        "the box cannot write `~/.local/state`, where sync-install.sh keeps the stamp that stops \
         it re-asserting over a box's own CLAUDE.md and memory at every start:\n{seen}"
    );
    // **Where the stamp landed, not merely that the write worked.** A write into the overlay's
    // tmpfs upper layer succeeds too and is gone at the next restart — so `sync-install.sh` would
    // find no stamp on the next start and rewrite a box's own CLAUDE.md, memory and skill, every
    // start, silently. `[ -w ]` cannot tell those two apart; reading the box's own directory from
    // outside the namespace can, and that is the whole reason `.local/state` is bound back on top
    // of the overlay rather than left to ride in it.
    assert_eq!(
        fs::read_to_string(f.dir.join("box-a/.local/state/skein-test-stamp"))
            .unwrap_or_default()
            .trim(),
        "mine",
        "the box's `~/.local/state` write did not reach its own private home, so it went to the \
         overlay's tmpfs upper layer and will be gone at the next restart"
    );
    assert!(
        !f.shared.join(".local/state/skein-test-stamp").exists(),
        "one box's work-tracker stamp landed in the SANDBOX's `~/.local/state`, where it answers \
         for every other box — which is how a box that never ran sync-install.sh comes up as \
         though it had"
    );
}

/// Whether a write can actually land under `dir`, proved by attempting one rather than by asking
/// permission bits.
///
/// `access(2)` — what `/usr/bin/test -w` calls — can answer "writable" from mode bits alone while
/// the mount itself is read-only; it is not required to know about `EROFS` at all. Measured on this
/// box after the host started mounting `/usr/local/share/npm-global` read-only: `test -w
/// /usr/local/share/npm-global` exits 0 ("writable"), while `touch
/// /usr/local/share/npm-global/.probe` answers "Read-only file system". Only a real write, and a
/// real removal, tells a working cover from a machine that never had the hole.
fn write_lands_under(dir: &Path) -> bool {
    let probe = dir.join(format!(".skein-write-probe-{}", std::process::id()));
    let landed = fs::write(&probe, b"probe").is_ok();
    let _ = fs::remove_file(&probe);
    landed
}

/// **The npm prefix a box actually runs `claude` from is read-only inside a box** (SKEIN-968).
///
/// A private `~/.local` does not close this and never could: `/usr/local/share/npm-global` is not
/// under `$HOME`. It is owned by uid 1000 — the uid every box runs as — it is second on `box_path`,
/// and `which -a claude` inside a real box answers it FIRST, because nothing installs an agent into
/// `~/.local/bin` on this substrate. Measured on the live fleet 2026-09-19: the copy there was
/// 2.1.278, written from inside a box by Claude Code's own background auto-updater, while the
/// root-owned `/usr/local/bin/claude` that skein's update path installs was 2.1.272.
///
/// # The change that would make this fail, named before it was written
///
/// Deleting the `--ro-bind` line from the launcher. The first half runs precisely that — the same
/// probe under a namespace built without it — and asserts the directory is writable, so the
/// read-only verdict below is an absence that was first a presence.
///
/// Nothing here writes to that directory; both halves ask `test -w`. A test that proved the point
/// by planting a file would be planting it in the fleet's real agent.
#[test]
fn the_npm_prefix_a_box_runs_the_agent_from_is_read_only_inside_it() {
    if !bwrap_works() {
        skip("bwrap cannot make a user namespace here, so the npm prefix's cover is unchecked");
        return;
    }
    const PREFIX: &str = "/usr/local/share/npm-global";
    if !Path::new(PREFIX).is_dir() {
        skip("this machine has no /usr/local/share/npm-global, so there is no npm prefix to cover");
        return;
    }
    if !write_lands_under(Path::new(PREFIX)) {
        skip(
            "/usr/local/share/npm-global is already unwritable by this user, so nothing here can \
             tell a working cover from a machine that never had the hole",
        );
        return;
    }
    let f = Homes::make("skein-npmglobal968");
    let probe = format!(
        "printf 'prefix %s\\n' \"$([ -w {PREFIX} ] && echo writable || echo read-only)\"\n"
    );

    let uncovered = f.inside("box-a", &probe, &|b| {
        b.lines()
            .filter(|l| !l.contains("--ro-bind /usr/local/share/npm-global"))
            .collect::<Vec<&str>>()
            .join("\n")
            // The `&&` continuation the removed line completed would otherwise dangle.
            .replace("[ -d /usr/local/share/npm-global ] \\", ":")
    });
    assert_eq!(
        answer(&uncovered, "prefix"),
        "writable",
        "without the launcher's read-only bind the npm prefix was ALREADY unwritable from inside \
         a box, so the cover below is not what is making the difference and this test proves \
         nothing:\n{uncovered}"
    );

    let covered = f.inside("box-a", &probe, &|b| b);
    assert_eq!(
        answer(&covered, "prefix"),
        "read-only",
        "a box can write {PREFIX} — the directory `box_path` puts second on every box's PATH and \
         the one `which -a claude` answers with. One box's write is the agent every box runs:\n{covered}"
    );
}

/// **A bwrap too old for an overlay leaves a box with its tools and without the hole** — it does
/// not leave the fleet unable to start (SKEIN-963).
///
/// `--tmp-overlay` arrived in bubblewrap 0.9. An unconditional overlay on a sandbox image carrying
/// an older one is `bwrap: Unknown option` at every box start, on a fleet whose only way in is a
/// box — which is the one failure `box-session.sh` must never have, and the reason it probes the
/// capability by running it instead of assuming it. This asserts what the fallback is worth: the
/// shared toolchain is still all there, and it is still not something a box can change.
///
/// # The change that would make this fail, named before it was written
///
/// Making the fallback a `--tmpfs` (an empty `~/.local`, so the box is stranded without its tools)
/// or a plain `--bind` (the SKEIN-963 hole back, on exactly the substrates least able to notice).
/// The two assertions below are one for each.
///
/// The forced-fallback rewrite is derived: it replaces the probe's own command with `false`. Delete
/// the probe and the substitution finds nothing, the real overlay runs, and it is the READ-ONLY
/// assertion that fires rather than a harness complaining about a shape it expected.
#[test]
fn a_bwrap_with_no_overlay_falls_back_to_read_only_rather_than_to_empty_or_to_shared() {
    if !bwrap_works() {
        skip("bwrap cannot make a user namespace here, so the no-overlay fallback is unchecked");
        return;
    }
    let f = Homes::make("skein-fallback963");
    let no_overlay = |b: String| {
        b.replace(
            r#"if bwrap --dev-bind / / --overlay-src "$HOME/$rel" --tmp-overlay "$HOME/$rel" -- /bin/true >/dev/null 2>&1; then"#,
            "if false; then",
        )
    };
    let probe = format!(
        "printf 'ran %s\\n' \"$(\"$HOME/.local/bin/{SHARED_BIN}\" 2>/dev/null || echo none)\"\n\
         printf 'wrote %s\\n' \"$(echo x > \"$HOME/.local/bin/{SHARED_BIN}\" 2>/dev/null \
           && echo yes || echo no)\"\n\
         printf 'state %s\\n' \"$(echo mine > \"$HOME/.local/state/skein-test-stamp\" 2>/dev/null \
           && echo wrote || echo no)\"\n"
    );
    let seen = f.inside("box-a", &probe, &no_overlay);
    assert_eq!(
        answer(&seen, "ran"),
        "shared",
        "the fallback left the box without the shared toolchain, so a fleet on an older bwrap \
         comes up with no agent CLI at all — which is worse than the defect being fixed:\n{seen}"
    );
    assert_eq!(
        answer(&seen, "wrote"),
        "no",
        "the fallback is a read-WRITE bind, so on exactly the substrates that cannot have the \
         overlay, one box still decides the binary every other box runs — SKEIN-963, unfixed and \
         now invisible:\n{seen}"
    );
    assert_eq!(
        fs::read_to_string(f.shared.join(".local/bin").join(SHARED_BIN)).unwrap(),
        "#!/bin/sh\nprintf %s shared\n",
        "the sandbox's own copy changed under the fallback, so the read-only verdict above is \
         about something other than the file every box runs"
    );
    // The stamp still has to land, because the private bind is what carries it and it is applied
    // after the fallback rather than after the overlay. A fallback that also took `~/.local/state`
    // away would make sync-install.sh rewrite a box's own memory at every start.
    assert_eq!(
        answer(&seen, "state"),
        "wrote",
        "under the fallback a box cannot write `~/.local/state`, so its work-tracker stamp never \
         lands and sync-install.sh re-asserts over its CLAUDE.md and memory every start:\n{seen}"
    );
}
