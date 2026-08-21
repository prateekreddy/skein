//! A value, and where it came from.
//!
//! skein can read the same fact from three different places, and they disagree: the box itself,
//! the shared store the box wrote at its last turn end, and the host's own copy of the repo. For a
//! clone-mode box those are different checkouts on different branches, so "the diff" and "the file
//! listing" have three possible answers and only one of them is the one you meant.
//!
//! Twice this has shipped as a bug — Files rendered the host clone as if it were the box's tree,
//! and diff measured the host clone as if it were the branch. Neither was a wrong *computation*:
//! each value was correct for where it came from and wrong for what it claimed to be. Before this
//! type, four features had each grown their own `source` / `note` / `stale` field, which is the
//! shape of a primitive nobody had named yet.
//!
//! So provenance travels with the value, in one type, rendered by one component. A feature can
//! still be wrong — but it can no longer be silently wrong about *which* tree it read.
//!
//! **Two axes, and they were once one word.** *Which copy* was read is [`Vantage`]; *how it was
//! reached* is [`crate::source::Source`], §2.3's primitive. Both were called `Source`, and neither
//! determines the other — `Store` and `Host` are both reached by `file`, and `Box` is reached by
//! `enter` or by `socket` depending on what was asked. §2.2 says a signal carries "which Source
//! produced it (§2.3)"; `Answer` now carries that as [`Answer::reach`], beside the vantage it
//! always had. The rename is the load-bearing half: a reader who knows §2.3 read the old field as
//! the wrong thing, and nothing about the code told them otherwise.

use serde::Serialize;

/// Which copy of a fact was read. Ordered by how much it can be trusted to describe *now*.
///
/// **Called `Source` until it wasn't.** [`crate::source::Source`] is §2.3's primitive — *how* a
/// subject is reached — and this is *which copy* was read. Different axes, many-to-one in both
/// directions, and one word for both meant a reader who knew §2.3 read this as the wrong thing.
/// The wire name changed with it: the field is `vantage`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Vantage {
    /// Asked the box just now. The only source that is definitionally current.
    Box,
    /// The box wrote it into the shared store at a turn boundary — true when it was written,
    /// which is not the same as true now.
    Store,
    /// The host's own copy. For a clone-mode box this is a *different checkout on a different
    /// branch*, so it is always a fallback and never a substitute.
    Host,
}

impl Vantage {
    /// Does this vantage describe the box as it is right now?
    pub fn is_current(self) -> bool {
        self == Vantage::Box
    }
}

/// `T` plus its provenance. Serialises as `T`'s own fields alongside `vantage`/`reach`/`note`/
/// `as_of`, so adding provenance to an endpoint doesn't reshape the payload its client already
/// reads.
#[derive(Debug, Clone, Serialize)]
pub struct Answer<T> {
    #[serde(flatten)]
    pub value: T,
    /// Which copy of the fact this is.
    pub vantage: Vantage,
    /// How it was reached — §2.3's Source, which §2.2 says every signal carries.
    ///
    /// It is not derivable from `vantage` and that is the point: a listing read by entering a box's
    /// namespace and one read off its tmux screen are both `Box`, and they do not cost the same,
    /// cannot fail the same way, and are not stale in the same way.
    pub reach: crate::source::Source,
    /// Why it isn't fresher, in words meant for the person reading it. Empty when `source` already
    /// says everything there is to say.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub note: String,
    /// When the value was produced, when that is knowable and isn't "just now". A string, like
    /// every other timestamp skein hands a client — the box writes them, and skein does not
    /// re-interpret what the box said.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub as_of: String,
}

impl<T> Answer<T> {
    /// Straight from the box, just now — and say how it was reached.
    ///
    /// The reach is a parameter here and hardcoded in the other two constructors, which is the
    /// asymmetry §2.3 predicts: the store and the host copy are files by definition, and a box is
    /// the one subject with more than one way in.
    pub fn from_box(value: T, reach: crate::source::Source) -> Self {
        Answer {
            value,
            vantage: Vantage::Box,
            reach,
            note: String::new(),
            as_of: String::new(),
        }
    }

    /// From the shared store — say when it was written, because that is the whole question.
    pub fn from_store(value: T, note: impl Into<String>) -> Self {
        Answer {
            value,
            vantage: Vantage::Store,
            // The store is a directory on the volume. There is no other way to read it, so this is
            // a fact about the vantage rather than a choice the caller makes.
            reach: crate::source::Source::File,
            note: note.into(),
            as_of: String::new(),
        }
    }

    /// From the host's copy. A note is required rather than optional: this source is the one that
    /// has twice been mistaken for the box, and an unexplained fallback is how that happened.
    pub fn from_host(value: T, note: impl Into<String>) -> Self {
        Answer {
            value,
            vantage: Vantage::Host,
            reach: crate::source::Source::File,
            note: note.into(),
            as_of: String::new(),
        }
    }

    pub fn at(mut self, ts: impl Into<String>) -> Self {
        self.as_of = ts.into();
        self
    }

    /// Change the value, keep the provenance. Used when a later stage annotates or trims what an
    /// earlier one read — losing the source there is exactly the mistake this type exists to stop.
    pub fn map<U>(self, f: impl FnOnce(T) -> U) -> Answer<U> {
        Answer {
            value: f(self.value),
            vantage: self.vantage,
            reach: self.reach,
            note: self.note,
            as_of: self.as_of,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::Source as Reach;

    #[derive(Debug, Clone, Serialize, PartialEq)]
    struct Patch {
        patch: String,
        base: String,
    }

    fn patch() -> Patch {
        Patch {
            patch: "diff --git".into(),
            base: "origin/main".into(),
        }
    }

    // The payload shape is a compatibility promise: a client that already reads `patch` and `base`
    // must not have to learn a wrapper object just because skein started saying where they came from.
    #[test]
    fn provenance_rides_alongside_the_value_not_around_it() {
        let json = serde_json::to_value(Answer::from_box(patch(), Reach::Enter)).unwrap();
        assert_eq!(json["patch"], "diff --git");
        assert_eq!(json["base"], "origin/main");
        assert_eq!(json["vantage"], "box");
        assert!(
            json.get("note").is_none(),
            "a current answer has nothing to explain: {json}"
        );
        assert!(json.get("value").is_none(), "the value must not be nested");
    }

    /// The two axes are two fields, because neither can be computed from the other.
    ///
    /// Both were once called `source`, and the collapse was not visible in any single value — it is
    /// visible here, where two answers agree on one axis and differ on the other.
    #[test]
    fn how_it_was_reached_is_not_which_copy_was_read() {
        let entered = Answer::from_box(patch(), Reach::Enter);
        let watched = Answer::from_box(patch(), Reach::Socket);
        assert_eq!(entered.vantage, watched.vantage, "same copy of the fact");
        assert_ne!(entered.reach, watched.reach, "reached two different ways");

        let store = Answer::from_store(patch(), "last turn end");
        let host = Answer::from_host(patch(), "the box isn't running");
        assert_ne!(store.vantage, host.vantage, "different copies of the fact");
        assert_eq!(
            store.reach, host.reach,
            "and both of them are a file read — which is why one word for both was wrong"
        );

        // Both travel to the client, under names that cannot be mistaken for each other.
        let json = serde_json::to_value(&watched).unwrap();
        assert_eq!(json["vantage"], "box");
        assert_eq!(json["reach"], "socket");
        assert!(
            json.get("source").is_none(),
            "the old ambiguous name must not still be on the wire: {json}"
        );
    }

    // A fallback that doesn't say it is one is the bug this whole type exists to prevent, so the
    // constructor for the dangerous source cannot be called without an explanation.
    #[test]
    fn a_fallback_always_carries_its_reason() {
        let host = Answer::from_host(patch(), "read from the host clone — the box isn't running");
        let json = serde_json::to_value(&host).unwrap();
        assert_eq!(json["vantage"], "host");
        assert!(json["note"].as_str().unwrap().contains("host clone"));
        assert!(!host.vantage.is_current());
        assert!(Answer::from_box(patch(), Reach::Enter).vantage.is_current());
        assert!(!Answer::from_store(patch(), "written at the last turn end")
            .vantage
            .is_current());
    }

    /// The wire change reached the only client there is.
    ///
    /// Renaming a serialised field is not a rename, it is a protocol change: the server stops
    /// sending `source` and every reader still asking for it silently gets `undefined`, which in
    /// this code reads as "current" — the fallback would stop announcing itself, which is the exact
    /// bug the whole type exists to prevent. So the cockpit is asserted against, not assumed.
    #[test]
    fn the_cockpit_reads_the_axis_it_is_sent() {
        let page = include_str!("web/index.html");
        assert!(
            page.contains("a.vantage") && page.contains("d.vantage"),
            "the cockpit still reads the old field name, so every fallback renders as current"
        );
        assert!(
            !page.contains("a.source") && !page.contains("d.source"),
            "a reader of the removed field survives, and `undefined !== \"box\"` is false"
        );
        // And the second axis is there to be inspected, which is the claim `source.rs` makes for
        // being serialisable at all.
        assert!(
            page.contains("a.reach"),
            "the reach is sent and nothing reads it"
        );
    }

    // Trimming a huge patch must not quietly turn a store answer into a box one.
    #[test]
    fn mapping_the_value_keeps_the_provenance() {
        let a = Answer::from_store(patch(), "last turn end").map(|p| p.patch.len());
        assert_eq!(a.value, 10);
        assert_eq!(a.vantage, Vantage::Store);
        assert_eq!(a.note, "last turn end");
    }
}
