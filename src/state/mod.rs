//! What skein knows about a box, one noun at a time: where each fact lives, its file format, and
//! the one answer people are shown. Paths are passed in, so nothing here reads the environment.
//!
//! [`access`] is the first noun, a box's git write access.

pub mod access;
