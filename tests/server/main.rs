//! Black-box smoke test: launch the real `skein-server` binary and exercise the HTTP surface.
//! Catches route-wiring, the include_str! UI, vendored assets, and the :name path-traversal guard
//! — the layers a pure unit test can't see.
//!
//! ## Layout
//!
//! This file is the crate root cargo builds as the `server` test binary: the shared imports and
//! the `mod` of each file beside it. `fixture.rs` holds the server this suite starts and is
//! imported whole below; each other file holds one question and its tests.

#[path = "../common/mod.rs"]
mod common;

use common::{fake_github, have, skein_server, skein_server_behind, skip, Scratch};
use skein::doorway::{FIRST, INHERITED_ONLY};
use std::ffi::OsStr;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

mod asks;
mod door;
mod fixture;
mod fleet;
mod requests;
mod routes;
mod usage;

use fixture::*;
