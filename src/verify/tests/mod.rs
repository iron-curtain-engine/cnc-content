//! Unit tests for source identification and installed-content verification.
//!
//! Covers SHA-1 source ID checks, BLAKE3 manifest generation and
//! verification, hex encoding, and adversarial inputs.

pub(super) use super::*;
use std::collections::BTreeMap;

mod bitfield;
mod hash;
mod id_and_content;
mod manifest;
