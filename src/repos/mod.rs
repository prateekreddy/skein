//! The repos skein manages (`~/.skein/repos.json`) and everything derived from one.
//!
//! skein is not single-repo: each box is `<repo-id>-<branch>` and maps back to its repo by
//! id-prefix, longest id winning. This is skein's OWN registration of a repo — distinct from the
//! sbx registry of sandboxes — and it is what makes "add a repo URL and go" work without the repo
//! shipping anything for skein.
//!
//! ## Layout
//!
//! One file per question, and this one is wiring: every name that resolved as `crate::repos::X`
//! before the split still does, through the `pub use` of each file below, at the visibility it
//! had. Tests sit in a `mod tests` beside the code they test; helpers two files share are in
//! `testkit.rs`.

use crate::config::*;
use crate::kit::{ensure_kit, ensure_store};
use crate::registry::registry_entry_for_box;
use crate::runtime::*;
use crate::sbx::lookup_dir;
use crate::sbx::{fleet_boxes, git_branch_for};
use crate::tracking::{load_connections, plane_project_id};
use crate::util::valid_name;
use crate::util::*;
use serde::{Deserialize, Serialize};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

mod add;
mod boxes;
mod fleetgit;
mod list;
mod mirror;
mod record;
mod source;
#[cfg(test)]
mod testkit;

pub use add::*;
pub use boxes::*;
use fleetgit::*;
pub use list::*;
pub use mirror::*;
pub use record::*;
pub use source::*;
