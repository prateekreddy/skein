//! skein core — the library both binaries drive.
//!
//! `skein` (the CLI) and `skein-server` (the cockpit) are two front ends over one crate; neither
//! owns behaviour the other cannot reach, which is why they cannot disagree about what a box is
//! doing. See `docs/architecture.md` for where this is going, and `docs/inventory.md` for what it
//! does today. (`ARCHITECTURE.md` at the root describes the per-VM system this replaced and is
//! **stale** — it still says ratatui and Svelte.)
//!
//! **This file declares modules and nothing else.** It held ~2,570 lines of implementation until
//! SKEIN-25, and the cost was that a reference into any of it was spelled `crate::X` — a root path
//! naming no module, which no dependency rule can be checked against. If something here would be
//! reached as `crate::X` again, it wants a module.
// Modules are public and there are no re-exports at the root. There used to be sixteen
// `pub use <mod>::*` lines here, and they cost more than they looked: every cross-module reference
// resolved through a flat namespace, so `crate::signals::` matched nothing from anywhere and the
// declared `mod` graph carried no information at all — none of `docs/architecture.md` §14's
// dependency rules could be checked against it. A reference now says where it comes from, and the
// price is paid at the import rather than hidden in the façade.
//
// What is below the modules is `use`, not `pub use`, and the distinction is the whole point: those
// are what THIS file's own body needs, not a surface anyone else reaches through.
pub mod ai;
pub mod answer;
pub mod apiauth;
pub mod attempt;
pub mod board;
pub mod cockpit;
pub mod codeowners;
pub mod config;
pub mod contracts;
pub mod diff;
pub mod digest;
pub mod files;
pub mod fleet;
pub mod gitgate;
pub mod github;
pub mod handoff;
pub mod health;
pub mod kit;
pub mod mailbox;
pub mod moduledocs;
pub mod place;
pub mod probes;
pub mod prq;
pub mod registry;
pub mod repos;
pub mod review;
pub mod runtime;
pub mod sandbox;
pub mod sbx;
pub mod sharedhome;
pub mod signal;
pub mod signals;
pub mod source;
pub mod substrate;
pub mod takeover;
#[cfg(test)]
mod testutil;
pub mod tracking;
pub mod transcript;
pub mod util;
pub mod volume;
pub mod warden_client;
