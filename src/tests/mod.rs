//! Cross-module integration tests.
//!
//! Grouped into sub-modules by concern:
//! - [`core`] — GameId, package, and source invariants
//! - [`downloads`] — download resolution, mirror lists, seeding policy
//! - [`post_download`] — torrent hash validation, post-extraction manifests
mod core;
mod downloads;
mod post_download;
