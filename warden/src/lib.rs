//! The host warden: the small service that owns fleet create and destroy.
//!
//! It exists because **create and destroy both terminate skein** — create because skein does not
//! exist yet, destroy because it will not afterwards — so fleet lifecycle cannot live inside the
//! fleet, permanently (architecture §8). That is a boundary rather than a limitation.
//!
//! A separate crate, with no dependency on `skein`. §14 gives this module an empty depends-on column
//! and the note "(separate binary)", and the reason is the whole point of the component: the warden
//! is what a compromised skein has to get past. A shared library is a shared blast radius.

pub mod outcome;
