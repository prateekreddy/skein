//! The mount cover: what a box on a mounted volume, an ordinary box, the workshop box, a box
//! with no mount manifest and an unmatched box can and cannot read, and what each announces.

use super::*;

/// A fleet whose state lives on a mounted VOLUME still covers it — and still leaves the box the
/// two directories it owns (SKEIN-219).
///
/// The 4a cover skipped any mount that was an ancestor of what the box owns, because a tmpfs over
/// `~/.skein` written after the binds of `~/.skein/boxes/<box>` would throw them away. The volume
/// skein-server-in-fleet is given is exactly that ancestor, so from every box on such a fleet
/// `credentials/`, `api-token` and `github-pats/` were a `cat` away. Ordering, not enumeration, is
/// the fix: the ancestor is covered BEFORE the entitlements are bound back.
#[test]
fn a_box_on_a_mounted_volume_cannot_read_the_fleets_credentials() {
    if !bwrap_works() {
        return skip(
            "bwrap cannot \
             create a user namespace here, so the volume cover was NOT exercised against a real \
             namespace on this machine",
        );
    }
    let fleet = Fleet::make_on_volume("volume");
    let volume = fleet.volume.clone().expect("this fleet is on a volume");
    let report = fleet.seen_by_box(Born::Covered);

    for secret in [
        volume.join("credentials/claude.json"),
        volume.join("github-pats/acme"),
        volume.join("api-token"),
        // The warden's shared secret, which is what will authenticate skein once the bind widens.
        volume.join("warden/secret"),
    ] {
        assert_eq!(
            verdict(&report, &secret),
            "gone",
            "a box on a volume-mounted fleet can read {} — that credential IS the fleet:\n{report}",
            secret.display()
        );
    }
    // **The "gone" assertions above are only worth having if the files were there to hide.** A
    // fixture that failed to write one would report "gone" for a path that never existed, and the
    // loop would pass while proving nothing — the exact shape of a test that cannot fail. So the
    // same volume is read again by a WORKSHOP box, which `box-session.sh:1109` deliberately exempts
    // from the cover: it must see the secret the ordinary box could not.
    let workshop = fleet.seen_by_box(Born::Workshop);
    assert_ne!(
        verdict(&workshop, &volume.join("warden/secret")),
        "gone",
        "the workshop box cannot see the warden secret either, so the cover is not what hid it \
         from the ordinary box and the assertion above proves nothing:\n{workshop}"
    );

    // …and the box still has everything it is entitled to, which is the half a blunt tmpfs breaks.
    assert_eq!(
        verdict(&report, &fleet.store()),
        "write",
        "covering the volume took the box's own store with it:\n{report}"
    );
    assert_eq!(
        verdict(&report, &fleet.state_parent.join("web-main")),
        "see",
        "covering the volume took the box's own state with it:\n{report}"
    );
    assert_eq!(
        verdict(
            &report,
            &fleet.state_parent.join("web-main/git-tokens/owner%2Frepo")
        ),
        "see",
        "covering the volume took the git token the host placed for this box:\n{report}"
    );
    assert_eq!(
        verdict(&report, &fleet.fleet_root.join("web-main")),
        "write",
        "covering the volume took the box's own checkout with it:\n{report}"
    );
    // The neighbour is still gone, so this is a cover rather than an accident of layout.
    assert_eq!(
        verdict(&report, &fleet.state_parent.join("other-main")),
        "gone",
        "another box's state is reachable on a volume fleet:\n{report}"
    );
}

/// A box can read what skein is built from and cannot write any of it (SKEIN-448).
///
/// The sandbox builds its own server now, which means the compiler and the source live inside the
/// fleet — and a compiler every box can overwrite is a worse position than the host build it
/// replaces, because the thing it compiles holds `credentials/`, `github-pats/` and the API token.
/// Architecture §9.2 states the rule: **no shared writable path may contain anything another box
/// executes.**
///
/// Asserted against real bwrap rather than against the placement, because "it is under `.skein`"
/// is a claim about a string and this is a claim about a namespace. The unit test
/// `fleet::nothing_the_sandbox_builds_skein_with_is_writable_by_a_box` covers the placement; this
/// covers whether the launcher actually delivers it.
///
/// `see` and not `write` is the whole assertion. `gone` would be a failure too, and a different
/// one: the sandbox has to be able to run what it built.
#[test]
fn a_box_can_read_what_skein_was_built_from_and_cannot_write_it() {
    if !bwrap_works() {
        return skip(
            "bwrap cannot \
             create a user namespace here, so the toolchain cover was NOT exercised against a real \
             namespace on this machine",
        );
    }
    let fleet = Fleet::make("toolchain");
    let report = fleet.seen_by_box(Born::Covered);

    for path in [
        fleet.fleet_root.join(".skein/src"),
        fleet.fleet_root.join(".skein/toolchain/cargo/bin"),
    ] {
        assert_eq!(
            verdict(&report, &path),
            "see",
            "a box can WRITE {} — it can choose what the fleet's own server is built from, and \
             the server holds every credential the fleet has (architecture §9.2):\n{report}",
            path.display()
        );
    }
}

/// An ordinary box reaches its own repo and its own state, and nothing else the sandbox mounts.
#[test]
fn a_box_run_under_bwrap_can_reach_its_own_repo_and_no_one_elses() {
    if !bwrap_works() {
        return skip(
            "bwrap cannot \
             create a user namespace here, so the isolation cover was NOT exercised against a real \
             namespace on this machine",
        );
    }
    let fleet = Fleet::make("ordinary");
    let report = fleet.seen_by_box(Born::Covered);

    // Its own, and read-write: the store is where its memory and mailbox are written.
    assert_eq!(
        verdict(&report, &fleet.store()),
        "write",
        "a box lost its own repo's store:\n{report}"
    );
    // Its own state, READ-ONLY. §5 divides this directory by who writes what: the conversation is
    // the box's and comes back writable through a separate bind at `$HOME`; the git token is the
    // host's, minted and placed for this box, and read here.
    assert_eq!(
        verdict(&report, &fleet.state_parent.join("web-main")),
        "see",
        "the box's own state directory is writable, so it can rewrite what the host placed there:\n{report}"
    );
    // The trap this item exists to avoid: bind only the conversation and every box silently loses
    // git push, because the credential helper reads its token out of this path.
    assert_eq!(
        verdict(
            &report,
            &fleet.state_parent.join("web-main/git-tokens/owner%2Frepo")
        ),
        "see",
        "a box cannot read the git token the host placed for it, so it cannot push:\n{report}"
    );
    // And the other half of the same rule: the conversation IS writable, through the bind the
    // launcher makes at `$HOME`. Read-only state that also took this away would be a bug wearing a
    // security argument.
    assert_eq!(
        verdict(&report, &fleet.dir.join("boxhome/.claude/projects")),
        "write",
        "a box cannot write its own conversation:\n{report}"
    );
    assert_eq!(
        verdict(&report, &fleet.fleet_root.join("web-main")),
        "write",
        "a box lost its own checkout:\n{report}"
    );

    // The launcher and the credential helper, readable and not writable — a box that could rewrite
    // box-session.sh would choose its own next namespace.
    assert_eq!(
        verdict(&report, &fleet.fleet_root.join(".skein")),
        "see",
        "the fleet's own scripts are writable by a box, or unreachable:\n{report}"
    );

    // And everything that is somebody else's.
    for (path, what) in [
        (
            fleet.repos.join("other/store/.claude"),
            "another repo's store",
        ),
        (
            fleet.elsewhere.join("store/.claude"),
            "a store outside the workspace",
        ),
        (
            fleet.fleet_root.join("other-main"),
            "another box's checkout",
        ),
        (
            fleet.state_parent.join("other-main"),
            "another box's conversation",
        ),
    ] {
        assert!(
            matches!(verdict(&report, &path), "gone" | "empty"),
            "{what} is reachable from a box:\n{report}"
        );
    }
}

/// The workshop box sees the fleet, which is the whole of what the flag is for.
///
/// Worth a test of its own: a cover that never lifts and a cover that never applies both pass a
/// one-sided check, and they are opposite bugs.
#[test]
fn the_workshop_box_sees_what_an_ordinary_box_cannot() {
    if !bwrap_works() {
        return skip(
            "bwrap cannot create a user \
             namespace here",
        );
    }
    let fleet = Fleet::make("workshop");
    let report = fleet.seen_by_box(Born::Workshop);
    for path in [
        fleet.repos.join("other/store/.claude"),
        fleet.elsewhere.join("store/.claude"),
        fleet.fleet_root.join("other-main"),
        fleet.state_parent.join("other-main"),
    ] {
        assert_eq!(
            verdict(&report, &path),
            "write",
            "the workshop box cannot reach {}, so the flag does nothing:\n{report}",
            path.display()
        );
    }
}

/// **A box skein could not match to a repository is uncovered, and it is uncovered exactly this
/// far** (SKEIN-836).
///
/// `fleet::mount_manifest` returns an empty manifest for such a box, so `SKEIN_FLEET_MOUNTS`
/// arrives empty and the two covers written *over the manifest* — the SKEIN-219 ancestor cover and
/// the per-mount cover — iterate nothing. Everything the launcher spells from paths skein chose
/// still runs, because none of it is inside that guard.
///
/// Both halves are asserted because only one of them is believable on its own:
///
///   * **the exposure**, and the same volume is read by a COVERED box in the same test — otherwise
///     a fixture that failed to write a secret would report it reachable-because-absent, or
///     hidden-because-absent, and neither direction proves anything (the rule the
///     `a_box_on_a_mounted_volume…` test states);
///   * **the limit**, which is the half this line used to get wrong. `mount_manifest` said such a
///     box "starts with the sandbox's whole view, as boxes did before covers", and it does not:
///     the other boxes' checkouts and state are still gone, and `private/` is still covered. A
///     warning that overstates is one a reader learns to discount.
///
/// **What would make this fail.** Moving `binds+=(--tmpfs "$fleet_root_dir")` or the state-parent
/// cover inside the `[ -n "${SKEIN_FLEET_MOUNTS-}" ]` guard breaks the limit half — the other box's
/// checkout comes back. Hoisting the ancestor cover out of that guard breaks the exposure half —
/// the credentials go, and the assertions that the covered box cannot see them stop proving that
/// the cover is what hid them.
#[test]
fn a_box_with_no_mount_manifest_is_uncovered_and_a_matched_box_is_not() {
    if !bwrap_works() {
        return skip(
            "bwrap cannot create a user namespace here, so the unmatched-repo path was NOT \
             exercised against a real namespace on this machine",
        );
    }
    let fleet = Fleet::make_on_volume("unmatched");
    let volume = fleet.volume.clone().expect("this fleet is on a volume");
    let loose = fleet.seen_by_box(Born::Unmatched);
    let covered = fleet.seen_by_box(Born::Covered);

    // The fleet's own credentials. Reachable from the unmatched box, gone from the matched one —
    // the second assertion is what makes the first a statement about the cover.
    for secret in [
        volume.join("credentials/claude.json"),
        volume.join("github-pats/acme"),
        volume.join("api-token"),
        volume.join("warden/secret"),
    ] {
        assert_ne!(
            verdict(&loose, &secret),
            "gone",
            "a box with no manifest cannot see {} — then the ancestor cover is not what the \
             manifest gates, and the exposure this test names is somewhere else:\n{loose}",
            secret.display()
        );
        assert_eq!(
            verdict(&covered, &secret),
            "gone",
            "a box WITH a manifest can read {} — that credential IS the fleet:\n{covered}",
            secret.display()
        );
    }

    // Every other repo's store and work tree, same shape and the commoner case: most fleets are not
    // on a volume, and this half is the one they still have.
    for (path, what) in [
        (
            fleet.repos.join("other/store/.claude"),
            "another repo's store",
        ),
        (
            fleet.elsewhere.join("store/.claude"),
            "a store outside the workspace",
        ),
    ] {
        assert_eq!(
            verdict(&loose, &path),
            "write",
            "{what} is not reachable from a box with no manifest, so the empty manifest is not \
             what this test thinks it is:\n{loose}"
        );
        assert!(
            matches!(verdict(&covered, &path), "gone" | "empty"),
            "{what} is reachable from a box WITH a manifest:\n{covered}"
        );
    }

    // And the limit. These covers are spelled from paths skein chose, so they do not need the
    // manifest and they still apply — which is why "the sandbox's whole view" was the wrong words.
    for (path, what) in [
        (
            fleet.fleet_root.join("other-main"),
            "another box's checkout",
        ),
        (
            fleet.state_parent.join("other-main"),
            "another box's conversation",
        ),
        (
            fleet.fleet_root.join(".skein/private/fleet-agent.token"),
            "the fleet agent's token",
        ),
    ] {
        assert!(
            matches!(verdict(&loose, &path), "gone" | "empty"),
            "{what} is reachable from a box with no manifest — either a cover moved inside the \
             manifest guard, or the launcher's banner is now overstating what is exposed:\n{loose}"
        );
    }

    // …and the box still has its own, which is the half a blunt "cover everything" would break.
    assert_eq!(
        verdict(&loose, &fleet.fleet_root.join("web-main")),
        "write",
        "a box with no manifest lost its own checkout:\n{loose}"
    );
    assert_eq!(
        verdict(&loose, &fleet.state_parent.join("web-main")),
        "see",
        "a box with no manifest lost its own state:\n{loose}"
    );
}

/// **The box that is uncovered by accident says so, and says something different from the box that
/// is uncovered on purpose** (SKEIN-836).
///
/// Two boxes can run without the mount cover and only one of them was chosen. The workshop box has
/// announced itself at every start for as long as the switch has existed; the box no manifest
/// reached announced nothing, and the one line that mentioned it went to skein-server's own stderr,
/// where SKEIN-799 established nobody reads it. So from inside, the deliberate state and the
/// accidental one were the same state.
///
/// **This is a status display, which in this repo is the thing that reports on something other than
/// what it names**, so all four cases are asserted rather than the one that motivated the change:
/// it must fire on an empty manifest, stay silent on a full one, and lose to the workshop banner
/// when both conditions hold at once — because what that box is, is chosen.
///
/// **What would make this fail.** Deleting `[ -n "${SKEIN_FLEET_MOUNTS-}" ] || uncovered=1` from
/// the top of the isolation block, or reading `$SKEIN_FLEET_MOUNTS` at the banner instead of the
/// flag — the `unset` between them means the banner would then never fire, and the empty-manifest
/// case goes silent. Dropping the `elif`'s condition fires it on a covered box. Making the two
/// banners one sentence collapses the distinction the whole item is about.
/// And for the delivery half at the end: deleting the `printf 'SKEIN_NOTICE %s\n'` line from the
/// launcher, which leaves both banners on a stderr that `Place::bytes` throws away on every
/// successful launch — the state SKEIN-846 found and the reason this test grew a second half.
#[test]
fn an_unmatched_box_announces_that_it_is_uncovered_and_a_covered_box_says_nothing() {
    let fleet = Fleet::make_on_volume("announce");

    // Covered: nothing to report, and reporting anyway is the failure mode that makes a banner
    // worth ignoring.
    let covered = fleet.announced(Born::Covered);
    assert!(
        !covered.contains("UNCOVERED"),
        "a box with a manifest is told it came up uncovered:\n{covered}"
    );

    // Unmatched: it says so, it names itself, and it names what is reachable rather than just
    // sounding alarmed.
    let loose = fleet.announced(Born::Unmatched);
    assert!(
        loose.contains("UNCOVERED"),
        "a box with no manifest is told nothing, which is the state this item found:\n{loose}"
    );
    assert!(
        loose.contains("web-main"),
        "the banner does not say WHICH box, which is the one thing a reader cannot recover from \
         it:\n{loose}"
    );
    for term in ["store", "work tree", "still separate"] {
        assert!(
            loose.contains(term),
            "the banner no longer names `{term}` — it has to say what is exposed AND what is not, \
             or the next reader re-derives it from the launcher:\n{loose}"
        );
    }

    // The workshop box says its own thing, and it is not this one.
    let workshop = fleet.announced(Born::Workshop);
    assert!(
        workshop.contains("WORKSHOP box"),
        "the privileged box stopped announcing itself:\n{workshop}"
    );
    assert!(
        !workshop.contains("UNCOVERED"),
        "the workshop box is told it came up uncovered by accident, which is the opposite of what \
         its switch means:\n{workshop}"
    );
    assert_ne!(
        loose.trim(),
        workshop.trim(),
        "the deliberate and the accidental uncovered box now say the same thing, so a reader still \
         cannot tell which one they are in"
    );

    // Both conditions at once: privileged wins, because that state was chosen.
    let both = fleet.announced(Born::WorkshopUnmatched);
    assert!(
        both.contains("WORKSHOP box") && !both.contains("UNCOVERED"),
        "a privileged box with no manifest is reported as an accident — the banner is firing on \
         the empty manifest rather than on the condition it names:\n{both}"
    );

    // ---- and it leaves by the door skein is standing at (SKEIN-846) ----
    //
    // Everything above is about WHICH sentence. This is about whether anyone gets it. The launcher
    // runs under `Place::exec`, which keeps stdout on success and reads stderr only on failure, so
    // for as long as these banners were `echo … >&2` alone they were written on every start and
    // seen on none. Read back through `fleet::notices_from_launch` — the production parser, not a
    // `contains` of my own — so that a marker the launcher writes in a shape skein cannot lift out
    // again fails here rather than in a fleet.
    let recovered = skein::fleet::notices_from_launch(&fleet.announced_to_skein(Born::Unmatched));
    assert_eq!(
        recovered.len(),
        1,
        "an uncovered box wrote {} notices to the stream skein reads; the banner is the one thing \
         on that stream that has to survive a successful launch",
        recovered.len()
    );
    assert!(
        recovered[0].contains("came up UNCOVERED") && recovered[0].contains("web-main"),
        "the sentence skein recovers from the launcher is not the sentence the launcher decided: \
         {:?}",
        recovered[0]
    );
    assert!(
        skein::fleet::notices_from_launch(&fleet.announced_to_skein(Born::Workshop))
            .iter()
            .any(|said| said.contains("WORKSHOP box")),
        "the workshop box announces itself only where nobody reads, which is where both banners \
         were for months"
    );
    assert!(
        skein::fleet::notices_from_launch(&fleet.announced_to_skein(Born::Covered)).is_empty(),
        "a covered box puts a notice on skein's stream, so every launch would print one and the \
         two that matter would be lost in them"
    );
}
