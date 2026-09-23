//! Is skein's own environment sound? One report the cockpit and `skein doctor` both render, so a
//! misconfiguration is diagnosed in one place rather than guessed at from a failure downstream.
//!
//! ## Layout
//!
//! One file per question, and this one is wiring: every name that resolved as `crate::health::X`
//! before the split still does, through the `pub use` of each file below, at the visibility it
//! had. Tests sit in a `mod tests` beside the code they test; helpers two files share are in
//! `testkit.rs`.

use crate::board::load_views;
use crate::registry::load_registry;
use crate::repos::load_repos;
use crate::runtime::*;
use crate::sbx::{fleet_boxes, fleet_degraded};
use crate::util::*;
use serde::Serialize;
use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

mod check;
mod disk;
mod reach;
mod report;
mod sandbox;
mod scratch;
#[cfg(test)]
mod testkit;
mod token;

pub use check::*;
pub use disk::*;
pub(crate) use reach::*;
pub use report::*;
pub use sandbox::*;
pub use scratch::*;
pub use token::*;
