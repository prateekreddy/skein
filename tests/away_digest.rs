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

use common::Scratch;

#[test]
fn the_mark_outlives_the_reader_and_is_the_same_for_everyone() {
    let home = Scratch::boxes("skein-away");
    std::env::set_var("SKEIN_HOME", home.path());

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

    // And the digest is answered against that one mark: everything after it, nothing before.
    let nothing = skein::stream::since(&later);
    assert!(
        nothing.is_empty(),
        "moments from before the acknowledgement were shown again: {nothing:?}"
    );

    std::env::remove_var("SKEIN_HOME");
}
