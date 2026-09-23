//! **Where the cockpit draws the notice a lost box leaves** (SKEIN-799), and where it must not.
//!
//! `src/web/index.html` is 6,000 lines of inline script with no module boundary, so this asserts
//! against the page's source the way `ai::call::tests::the_ceiling_sits_in_front_of_both_
//! destinations_and_not_inside_one` asserts against `src/ai/call.rs`'s: by finding the two
//! functions' spans and asking which one the drawing is inside. That is weaker than driving the
//! real page, and it is the part of this the browser tier does not currently cover — said plainly
//! rather than left to be inferred from what is here.
//!
//! Through the same `include_str!` the server serves (`cockpit::INDEX` in `src/cockpit.rs`),
//! so there is no second copy of the page for this to be right about while the shipped one is
//! wrong.

/// Where `function <name>(` begins and ends, as byte offsets into the page.
fn span_of(page: &str, name: &str) -> std::ops::Range<usize> {
    let head = format!("\nfunction {name}(");
    let begins = page
        .find(&head)
        .unwrap_or_else(|| panic!("the page has no `function {name}(` — it was renamed"))
        + 1;
    // To the next thing that starts at column zero, which ends the function. Never a bare `}`
    // scan: every template literal in here contains braces.
    let ends = page[begins + 1..]
        .find("\nfunction ")
        .map(|at| begins + 1 + at)
        .unwrap_or(page.len());
    begins..ends
}

/// **One surface, and it is the open row** — SKEIN-400's rule, applied to the new sentence.
///
/// That rule was bought by `unread_because`, which an open row drew three times over: cut to a
/// 256px column in the gist, in full again thirty pixels below, and a third time as the gist's own
/// `title`. This sentence is longer than that one and ends with the part that matters most — an
/// approval being out of reach — so a copy in the gist would keep the complaint and lose the
/// consequence.
///
/// **What makes it fail:** drawing `s.read_outside_box` inside `revGist` as well, or removing it
/// from `revDetail`. Both were done and both failed here before this was believed.
#[test]
fn the_lost_box_notice_is_drawn_in_the_open_row_and_nowhere_else() {
    let page = skein::cockpit::INDEX;
    let sites: Vec<usize> = page
        .match_indices("s.read_outside_box")
        .map(|(at, _)| at)
        .collect();
    assert!(
        !sites.is_empty(),
        "the page draws the lost-box notice nowhere at all, so a reader is back to the server's \
         stderr — which is the whole of SKEIN-799"
    );

    let detail = span_of(page, "revDetail");
    let gist = span_of(page, "revGist");
    for site in sites {
        assert!(
            !gist.contains(&site),
            "the notice is drawn in the collapsed gist as well as the open row — SKEIN-400's \
             rule, and this sentence ends with the part a reader most needs, so a cut copy loses \
             it"
        );
        assert!(
            detail.contains(&site),
            "the notice is drawn outside `revDetail`, so the one surface is now two: {}",
            &page[site.saturating_sub(120)..(site + 80).min(page.len())]
        );
    }
}

/// **The sentence is the server's, and the page does not write one of its own** (SKEIN-799).
///
/// The composition lives in `review::summary::outside_box_notice` because its second half turns on
/// `Summary::swept`, which reaches this page only when it is TRUE (`skip_serializing_if`) — so a
/// page that composed the sentence itself could not tell "no sweep accounted for it" from "this
/// skein is too old to say", and would have to guess. Guessing in that direction is what
/// `swept`'s own doc forbids.
///
/// **What makes it fail:** writing any part of the wording into the page — which is the natural
/// thing to do the next time somebody wants to tweak it while looking at the renderer.
#[test]
fn the_page_does_not_compose_the_notice_itself() {
    let page = skein::cockpit::INDEX;
    for phrase in [
        "Read outside its box",
        "was not checked out",
        "No sweep accounted for it",
        "an approval stays out of reach",
    ] {
        assert!(
            !page.contains(phrase),
            "the page writes `{phrase}` itself, so there are now two authors of one sentence and \
             the page cannot see the `swept` it would need to get the second half right"
        );
    }
}
