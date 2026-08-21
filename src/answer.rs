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

use serde::Serialize;

/// Where a value came from. Ordered by how much it can be trusted to describe *now*.
///
/// **Not [`crate::source::Source`], and the two must not be conflated.** This one is *which copy of
/// the fact* was read; that one is *how the thing was reached*. They are different axes, and the
/// mapping is many-to-one in both directions: `Store` and `Host` are both reached by `file`, and
/// `Box` is reached by `enter` or by `socket` depending on what was asked. Architecture §2.2 says a
/// signal carries "which Source produced it (§2.3)", which this field does **not** answer — it
/// answers a coarser question that happens to share the word.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    /// Asked the box just now. The only source that is definitionally current.
    Box,
    /// The box wrote it into the shared store at a turn boundary — true when it was written,
    /// which is not the same as true now.
    Store,
    /// The host's own copy. For a clone-mode box this is a *different checkout on a different
    /// branch*, so it is always a fallback and never a substitute.
    Host,
}

impl Source {
    /// Does this source describe the box as it is right now?
    pub fn is_current(self) -> bool {
        self == Source::Box
    }
}

/// `T` plus its provenance. Serialises as `T`'s own fields alongside `source`/`note`/`as_of`, so
/// adding provenance to an endpoint doesn't reshape the payload its client already reads.
#[derive(Debug, Clone, Serialize)]
pub struct Answer<T> {
    #[serde(flatten)]
    pub value: T,
    pub source: Source,
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
    /// Straight from the box, just now.
    pub fn from_box(value: T) -> Self {
        Answer {
            value,
            source: Source::Box,
            note: String::new(),
            as_of: String::new(),
        }
    }

    /// From the shared store — say when it was written, because that is the whole question.
    pub fn from_store(value: T, note: impl Into<String>) -> Self {
        Answer {
            value,
            source: Source::Store,
            note: note.into(),
            as_of: String::new(),
        }
    }

    /// From the host's copy. A note is required rather than optional: this source is the one that
    /// has twice been mistaken for the box, and an unexplained fallback is how that happened.
    pub fn from_host(value: T, note: impl Into<String>) -> Self {
        Answer {
            value,
            source: Source::Host,
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
            source: self.source,
            note: self.note,
            as_of: self.as_of,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let json = serde_json::to_value(Answer::from_box(patch())).unwrap();
        assert_eq!(json["patch"], "diff --git");
        assert_eq!(json["base"], "origin/main");
        assert_eq!(json["source"], "box");
        assert!(
            json.get("note").is_none(),
            "a current answer has nothing to explain: {json}"
        );
        assert!(json.get("value").is_none(), "the value must not be nested");
    }

    // A fallback that doesn't say it is one is the bug this whole type exists to prevent, so the
    // constructor for the dangerous source cannot be called without an explanation.
    #[test]
    fn a_fallback_always_carries_its_reason() {
        let host = Answer::from_host(patch(), "read from the host clone — the box isn't running");
        let json = serde_json::to_value(&host).unwrap();
        assert_eq!(json["source"], "host");
        assert!(json["note"].as_str().unwrap().contains("host clone"));
        assert!(!host.source.is_current());
        assert!(Answer::from_box(patch()).source.is_current());
        assert!(!Answer::from_store(patch(), "written at the last turn end")
            .source
            .is_current());
    }

    // Trimming a huge patch must not quietly turn a store answer into a box one.
    #[test]
    fn mapping_the_value_keeps_the_provenance() {
        let a = Answer::from_store(patch(), "last turn end").map(|p| p.patch.len());
        assert_eq!(a.value, 10);
        assert_eq!(a.source, Source::Store);
        assert_eq!(a.note, "last turn end");
    }
}
