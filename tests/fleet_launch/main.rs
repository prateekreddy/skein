//! The whole shared-sandbox launch, end to end, against a fake `sbx`.
//!
//! Everything the fleet path needs from the host is `sbx create` and `sbx exec`. But a sandbox is a
//! Linux machine with `bwrap`, `tmux` and `git` — and so is the machine running this test — so
//! `sbx exec <fleet> …` can simply mean "run it here" and the rest is genuinely exercised: a real
//! clone from a real remote, a real bwrap namespace, a real tmux server, real `nsenter` re-entry.
//!
//! What this deliberately does NOT cover is sbx's own behaviour — whether the flags are spelled
//! right, and where a workspace mount lands. Both were verified by hand against a real sandbox
//! instead (see `fleet::create_argv` and `fleet::fleet_workspace`), because no fake can answer them.
//!
//! Skipped rather than failed where the substrate is absent: this suite is about skein's logic, and
//! a machine without `bwrap` cannot host a box at all.
//!
//! ## Layout
//!
//! This file is the crate root cargo builds as the `fleet_launch` test binary: the shared fake
//! `sbx`, remote and fleet names, and the `mod` of each file beside it. Each of those holds one
//! question and its tests; `use <file>::*` below lets a helper one file defines reach another.

#[path = "../common/mod.rs"]
mod common;

use common::{bwrap_works, env_lock, env_pins, have, skip, Scratch};
use skein::config::{load_config, save_config, Config};
use skein::fleet::{
    anchor_from_launch, box_root, box_session_path, box_sock, box_state, clone_script,
    ensure_box_session, fleet_liveness, forget_fleet_liveness, heal_fleet, install_launcher,
    provision_script, resize_fleet, server_tmux_sock_in, session_script, snapshot_box, start_box,
};
use skein::kit::ensure_store;
use skein::place::{forget_place, own_sandbox, place_of, record_place, shared_record, PlaceRecord};
use skein::probes::ensure_probe_in;
use skein::repos::{branch_of, save_repos, Repo};
use skein::sandbox::{destroy_box, stop_box};
use skein::sbx::{fleet_boxes, Liveness};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

const FLEET: &str = "test-fleet";
const BOX: &str = "web-main";

fn sh(script: &str) -> String {
    let out = Command::new("bash")
        .arg("-lc")
        .arg(script)
        .output()
        .expect("bash");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// A stand-in for `sbx` that runs the guest command locally.
///
/// `exec` drops its flags and the sandbox name and execs the rest, so an `nsenter` hop reaches the
/// same namespace it would in a real sandbox. `create` only has to succeed — the sandbox in this
/// test is the machine itself.
fn write_fake_sbx(dir: &Path) {
    use std::os::unix::fs::PermissionsExt;
    fs::create_dir_all(dir).unwrap();
    let p = dir.join("sbx");
    fs::write(
        &p,
        r#"#!/usr/bin/env bash
verb="$1"; shift
case "$verb" in
  create) exit 0 ;;
  # Destroying the sandbox is the one irreversible step, so the harness records that it happened
  # rather than trusting resize's own report of whether it got that far.
  rm) : > "$SBX_RM_MARKER"; exit 0 ;;
  exec)
    while [ $# -gt 0 ]; do case "$1" in -*) shift ;; *) break ;; esac; done
    shift          # the sandbox name
    exec "$@" ;;
  *) echo "fake sbx: unsupported verb $verb" >&2; exit 2 ;;
esac
"#,
    )
    .unwrap();
    fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).unwrap();
}

/// A bare repo with one commit on `main`, standing in for the remote a box clones from.
fn write_remote(root: &Path) -> String {
    let remote = root.join("remote.git");
    let seed = root.join("seed");
    let git = "git -c user.email=t@example.com -c user.name=test -c init.defaultBranch=main";
    sh(&format!(
        "set -e; git init --bare -q -b main {r}; {git} init -q {s}; \
         cd {s}; echo hello > README.md; {git} add -A; {git} commit -qm seed; \
         {git} remote add origin {r}; {git} push -q origin main",
        r = remote.display(),
        s = seed.display(),
        git = git
    ));
    remote.to_string_lossy().into_owned()
}

mod environment;
mod fixture;
mod lifecycle;
mod onboarding;
mod start;
mod sudo;
mod telemetry;
mod trust;

use fixture::*;
use lifecycle::*;
use onboarding::*;
use sudo::*;
use trust::*;
