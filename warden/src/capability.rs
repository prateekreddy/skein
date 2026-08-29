//! What this warden can do, derived from what is linked (§8.3).
//!
//! **Not read from config, and not a runtime switch.** Each doer is its own module behind its own
//! Cargo feature, so a warden built without one does not contain it: a runtime check falls to a bug
//! in the check, and absent code falls to nothing. That is the whole of §8.3's argument and it only
//! holds if the list below is computed from `cfg!` rather than from anything a file could say.
//!
//! **Two of the four are not here**, and their absence is the point. The audit sink and fleet
//! observation have no feature and cannot be removed — §8.3 states this as the deliberate exception
//! to §12.10: a capability that *performs* something may be left unbuilt; one that only *reports*
//! may not, or the design loses the ability to see and to account for itself.
//!
//! **Both doers ship by default.** An earlier draft of the design shipped `create` only, and resize
//! is destroy + create (§7.3) — so a create-only warden cannot resize, which is the commonest
//! lifecycle operation after create, nor retire a tombstone (§2.6). Making `destroy` optional made
//! `resize` optional by accident. What compile-time removal is *for* is a machine that should never
//! destroy a fleet — a shared or long-lived host — which is a choice someone makes.
//!
//! And the rule that matters at the other end: **advertisement decides what skein OFFERS; it never
//! decides what skein BELIEVES.** A malicious endpoint advertises whatever makes skein show a
//! button. This list is a courtesy to the client's UI, not evidence about anything.

use serde::Serialize;

/// A thing this warden may be asked to perform.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Capability {
    /// Make the fleet sandbox. Removable: `--no-default-features`.
    Create,
    /// Destroy it. Removable, and removing it removes resize with it.
    Destroy,
    /// Withdraw a host port mapping — `sbx ports <sandbox> --unpublish HOST:SANDBOX`. Removable.
    ///
    /// **There is no `Publish`, and that is the whole of why this one is safe to have.** Skein
    /// publishes host ports and cannot take them back, so every mapping it makes by mistake was a
    /// line a person had to run. Withdrawing one only ever closes an opening; publishing opens a
    /// host port into the network namespace every box shares, which is exactly the act §9.4 makes
    /// prompted. A warden that could publish would remove the person from the decision that needs
    /// them most, so this half is here and that half is not.
    Unpublish,
}

impl Capability {
    pub fn name(self) -> &'static str {
        match self {
            Capability::Create => "create",
            Capability::Destroy => "destroy",
            Capability::Unpublish => "unpublish",
        }
    }
}

/// The doers this binary was built with, in a stable order.
///
/// Computed from `cfg!`, which is the only way the claim "derived from what is linked" can be true.
/// A `Vec` rather than a const array because the length is a build-time fact and Rust cannot size an
/// array by `cfg!` without repeating the whole expression.
pub fn linked() -> Vec<Capability> {
    let mut all = Vec::new();
    if cfg!(feature = "create") {
        all.push(Capability::Create);
    }
    if cfg!(feature = "destroy") {
        all.push(Capability::Destroy);
    }
    if cfg!(feature = "unpublish") {
        all.push(Capability::Unpublish);
    }
    all
}

/// Is this doer in this binary?
pub fn is_linked(capability: Capability) -> bool {
    linked().contains(&capability)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The default build has both, and the reason is `resize`.
    ///
    /// Resize is destroy + create (§7.3). A default that shipped `create` alone would make the
    /// commonest lifecycle operation after create unavailable — by accident, which is how the
    /// earlier draft did it.
    ///
    /// Gated, because §13 requires this crate be built and tested **four** ways and this assertion
    /// is about exactly one of them. Compiled into all four it fails the other three — which it did,
    /// unseen, because `cargo test` at the root runs the default build and nothing ran the rest.
    /// `the_advertised_set_is_the_compiled_set` below is the one that has to hold in every build,
    /// and it does: it asks `cfg!` the same question the code asks.
    #[cfg(all(feature = "create", feature = "destroy"))]
    #[test]
    fn the_default_build_can_destroy_because_resize_is_destroy_and_create() {
        assert!(
            is_linked(Capability::Create) && is_linked(Capability::Destroy),
            "the default build must ship both doers, or a fleet cannot be resized: {:?}",
            linked()
        );
    }

    /// The list is what is linked, not a list of everything the type can name.
    ///
    /// If these two ever disagree in the default build, `linked()` has stopped being computed and
    /// started being written down — which is the failure §8.3 is about.
    #[test]
    fn the_advertised_set_is_the_compiled_set() {
        for capability in [
            Capability::Create,
            Capability::Destroy,
            Capability::Unpublish,
        ] {
            assert_eq!(
                is_linked(capability),
                match capability {
                    Capability::Create => cfg!(feature = "create"),
                    Capability::Destroy => cfg!(feature = "destroy"),
                    Capability::Unpublish => cfg!(feature = "unpublish"),
                },
                "{} is advertised on evidence other than being compiled",
                capability.name()
            );
        }
    }
}
