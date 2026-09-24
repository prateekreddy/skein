//! Rationed, lazy AI enrichment over the Claude subscription — no API key.
//!
//! skein runs *inside* an `sbx run` box where `claude` is logged in, so every call here rides the
//! SAME rate-limit window as the fleet doing the real work. That is the whole reason this is
//! opt-in, on demand only, cached per turn-end, and never a per-tick fleet sweep.
//!
//! The governing rule, which every function here obeys: **AI may only add scrutiny, never remove
//! it.** The batch-resume gate can hold a box back; it can never clear one the free heuristic
//! wouldn't already have cleared. A flaky, garbled or absent answer therefore fails toward asking
//! you.
//!
//! ## Layout
//!
//! One file per question, and this one is wiring: every name that resolved as `crate::ai::X`
//! before the split still does, through the `pub use` of each file below, at the visibility it
//! had. Tests sit in a `mod tests` beside the code they test.

use crate::config::load_config;
use crate::signals::{session_signal, SessionSignal};
use crate::util::valid_name;
use std::env;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

mod binary;
mod call;
mod models;
mod narrate;
mod refusal;
mod site;
mod switches;
mod turn;
mod unread;

pub use binary::*;
pub use call::*;
pub use models::*;
pub use narrate::*;
pub use refusal::*;
pub use site::*;
pub use switches::*;
pub(crate) use turn::*;
pub use unread::*;
