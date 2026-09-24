//! The isolation cover, proved by running bwrap rather than by reading its arguments.
//!
//! Every other test of `box-session.sh`'s isolation block inspects the argv it builds: does
//! `--tmpfs <path>` appear, does `--bind <store>` appear, does this tmpfs land after that bind.
//! That checks *skein asked for the right mounts*. What matters is *a box cannot reach the other
//! things*, and the gap between the two is exactly where a wrong flag, a wrong order, or a bwrap
//! behaviour nobody predicted lives. The ordering rule especially: it is a hand-written comparison
//! of positions in a list, and bwrap's own resolution is the authority on what that list means.
//!
//! So this one builds a fleet-shaped directory tree, generates the binds the way the launcher does,
//! runs **bwrap** with them, and asks the process inside what it can see.
//!
//! It **skips** rather than fails where bwrap cannot make a user namespace — an unprivileged
//! container without `CAP_SYS_ADMIN`, a kernel with `unprivileged_userns_clone` off. A test that
//! cannot run must not be a red build, and it must not be a silently green one either: the skip
//! says which check did not happen.
//!
//! ## Layout
//!
//! This file is the crate root cargo builds as the `isolation_bwrap` test binary: the path to the
//! launcher and the `mod` of each file beside it. `harness.rs` builds the fleet the others run
//! bwrap over; each other file holds one axis of the cover and its tests; `use <file>::*` below
//! lets a helper one file defines reach another.

#[path = "../common/mod.rs"]
mod common;

use common::{bwrap_works, skip, Scratch};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn script(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("src")
        .join(name)
}

mod cover;
mod harness;
mod homes;
mod network;
mod path;
mod private;
mod requests;
mod signals;

use harness::*;
use network::*;
