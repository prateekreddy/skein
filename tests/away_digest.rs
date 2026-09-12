//! "Since you were away" survives a reload and agrees between two tabs.
//!
//! Those two are the whole reason it is server-side, and they are exactly what a client-side delta
//! cannot do. A mark in a tab's memory dies on reload; a mark per tab makes two tabs disagree; and a
//! mark computed from "when this tab gained focus" cannot tell a box that finished while you were
//! out from one that finished before you opened it. Three failures that all look like the feature
//! working, which is why they are asserted rather than reasoned about.
//!
//! Its own binary because the mark is a file under `$SKEIN_HOME`, and the point is that it outlives
//! the thing that read it.

mod common;

use common::{env_pins, Scratch};
use skein::board::BoxView;

/// One box's view, for [`stream::publish`] — only `name` and `state` matter to `remember`'s
/// transition check, so everything else is left at its default.
fn view(name: &str, state: &str) -> BoxView {
    BoxView {
        name: name.into(),
        state: state.into(),
        ..Default::default()
    }
}

#[test]
fn the_mark_outlives_the_reader_and_is_the_same_for_everyone() {
    let home = Scratch::boxes("skein-away");
    // Bound after the `Scratch`, so the pins go back before the directory they name is removed —
    // and through `EnvPins` rather than a trailing `remove_var`, which a failing assertion unwinds
    // straight past.
    //
    // `$SKEIN_FLEET_ROOT` is pinned although nothing this test calls resolves a fleet path today:
    // `stream::acknowledge` reaches `config::skein_home` and stops there. Unpinned it would mean
    // `/boxes`, the owner's live fleet, and the pin's absence is invisible until the day something
    // inside `stream` resolves a fleet path — at which point the failure lands in a test nobody
    // edited. `tools/fleet-pin-check.py` is what keeps the pair together.
    let mut pins = env_pins();
    pins.set("SKEIN_HOME", home.path())
        .set("SKEIN_FLEET_ROOT", home.join("boxes"));

    // Nobody has looked yet, and that is a state rather than a time. An empty mark means "we do not
    // know when you last looked", which shows the night rather than hiding it.
    assert_eq!(
        skein::stream::last_seen(),
        "",
        "a fresh skein claims to have been seen"
    );

    let at = skein::stream::acknowledge().expect("acknowledge");
    assert!(!at.is_empty());

    // **Survives a reload.** Nothing in this process is consulted — the answer is read back off
    // disk, which is what a tab reloading does.
    assert_eq!(
        skein::stream::last_seen(),
        at,
        "the mark did not survive being read by somebody else"
    );
    assert!(
        home.join("seen.json").is_file(),
        "the mark is in memory, so a reload loses it"
    );

    // **Two tabs agree**, because there is one mark rather than one per reader. A second
    // acknowledgement moves it for everybody, which is the behaviour that makes the digest mean the
    // same thing in both windows.
    let later = skein::stream::acknowledge().expect("acknowledge again");
    assert!(later >= at);
    assert_eq!(skein::stream::last_seen(), later);

    // And the digest is answered against that one mark: everything after it, nothing before. Put
    // one transition on each side of `later` — an absence that was never a presence proves nothing
    // (RT-9), so first prove the journal actually holds the earlier one, then ask `since`.
    //
    // `remember` only records a transition ("box moved"), never a box's first-seen state, so the
    // first `publish` establishes "before" without writing to the journal (read `remember`'s doc in
    // `src/stream.rs`), and the state change on the second call is what lands as a `Moment`. A tick
    // between each publish, so the RFC3339 stamps `remember`/`acknowledge` both take from the clock
    // are strictly ordered rather than tied.
    std::thread::sleep(std::time::Duration::from_millis(5));
    skein::stream::publish(vec![view("digest-test", "working")]);
    std::thread::sleep(std::time::Duration::from_millis(5));
    skein::stream::publish(vec![view("digest-test", "done")]);
    // Control: prove the journal actually holds this transition before asking whether a LATER mark
    // excludes it — an absence that was never a presence proves nothing.
    let since_first_ack = skein::stream::since(&at);
    assert_eq!(
        since_first_ack.len(),
        1,
        "the earlier transition never reached the journal, so the assertion below would pass \
         with an empty journal too: {since_first_ack:?}"
    );
    assert_eq!(since_first_ack[0].to, "done");

    std::thread::sleep(std::time::Duration::from_millis(5));
    let third_ack = skein::stream::acknowledge().expect("acknowledge a third time");
    std::thread::sleep(std::time::Duration::from_millis(5));
    skein::stream::publish(vec![view("digest-test", "waiting")]);

    let since_third_ack = skein::stream::since(&third_ack);
    assert_eq!(
        since_third_ack.len(),
        1,
        "expected exactly the one transition after the mark: {since_third_ack:?}"
    );
    assert_eq!(since_third_ack[0].name, "digest-test");
    assert_eq!(since_third_ack[0].from, "done");
    assert_eq!(since_third_ack[0].to, "waiting");
    assert!(
        !since_third_ack.iter().any(|m| m.to == "done"),
        "a moment from before the acknowledgement was shown again: {since_third_ack:?}"
    );
}
