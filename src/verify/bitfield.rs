// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2025–present Iron Curtain contributors

//! SIMD verification bitfield — tracks file verification status using `wide::u64x4` lanes.
//!
//! Split from `verify/mod.rs` because the bitfield logic is large, self-contained,
//! and only compiled under the `fast-verify` feature.
//!
//! ## Design (IC performance doc §2.5)
//!
//! Each bit position maps to a file index in the installed-content manifest.
//! Set operations (AND, OR, AND NOT) are single-instruction on AVX2/NEON via
//! the `wide` crate. Popcount gives pass/fail counts without a loop.
//!
//! The same pattern is recommended for P2P piece have/need bitmaps in
//! `p2p-distribute` — this bitfield is a natural precursor that exercises the
//! same SIMD codepath.

/// SIMD-width bitfield for tracking file verification status.
///
/// Uses `wide::u64x4` (256 bits per SIMD lane) for set operations:
/// - **AND** (intersection): "which files are both installed and verified"
/// - **OR** (union): "which files have been checked at all"
/// - **AND NOT** (difference): "which files still need checking"
/// - **popcount**: "how many files passed/failed"
///
/// Each bit position corresponds to a file index in the manifest.
/// Supports up to 4096 files (16 × u64x4 = 16 × 256 bits). Game content
/// manifests are typically 20–200 files, well within this limit.
///
/// This is the same pattern recommended for P2P piece have/need bitmaps
/// in `p2p-distribute` — the verification bitfield is a natural precursor
/// that exercises the same SIMD codepath.
#[cfg(feature = "fast-verify")]
pub struct VerifyBitfield {
    /// Each `u64x4` holds 256 bits. 16 lanes = 4096 file capacity.
    lanes: [wide::u64x4; 16],
    /// Number of files tracked.
    len: usize,
}

#[cfg(feature = "fast-verify")]
impl VerifyBitfield {
    /// Maximum number of files supported.
    pub const MAX_FILES: usize = 16 * 256;

    /// Creates a new bitfield with all bits cleared (all files unverified).
    pub fn new(file_count: usize) -> Self {
        assert!(
            file_count <= Self::MAX_FILES,
            "VerifyBitfield supports up to {} files, got {file_count}",
            Self::MAX_FILES
        );
        Self {
            lanes: [wide::u64x4::ZERO; 16],
            len: file_count,
        }
    }

    /// Marks a file index as set (verified/passed).
    ///
    /// Out-of-bounds indices are silently ignored (defensive guard).
    pub fn set(&mut self, index: usize) {
        if index >= self.len {
            return;
        }
        let lane = index / 256;
        let bit_in_lane = index % 256;
        let word = bit_in_lane / 64;
        let bit = bit_in_lane % 64;

        let Some(lane_ref) = self.lanes.get(lane) else {
            return;
        };
        let mut arr = lane_ref.to_array();
        let Some(w) = arr.get_mut(word) else {
            return;
        };
        *w |= 1u64 << bit;
        if let Some(slot) = self.lanes.get_mut(lane) {
            *slot = wide::u64x4::from(arr);
        }
    }

    /// Returns `true` if the given file index is set.
    ///
    /// Returns `false` for out-of-bounds indices (defensive guard).
    pub fn get(&self, index: usize) -> bool {
        if index >= self.len {
            return false;
        }
        let lane = index / 256;
        let bit_in_lane = index % 256;
        let word = bit_in_lane / 64;
        let bit = bit_in_lane % 64;

        let Some(lane_ref) = self.lanes.get(lane) else {
            return false;
        };
        let arr = lane_ref.to_array();
        arr.get(word).is_some_and(|w| w & (1u64 << bit) != 0)
    }

    /// Returns the number of set bits (files that passed verification).
    pub fn count_ones(&self) -> usize {
        let mut total = 0usize;
        for lane in &self.lanes {
            for word in lane.to_array() {
                total += word.count_ones() as usize;
            }
        }
        total
    }

    /// Returns the number of files tracked.
    pub fn len(&self) -> usize {
        self.len
    }

    /// Returns `true` if no files are tracked.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Returns the number of files that failed (not set).
    pub fn count_failures(&self) -> usize {
        self.len - self.count_ones()
    }

    /// SIMD AND — intersection of two bitfields.
    ///
    /// Returns a new bitfield where only bits set in *both* inputs are set.
    /// Useful for "which files are both installed AND verified?"
    pub fn and(&self, other: &Self) -> Self {
        let mut result = Self::new(self.len.max(other.len));
        for (r, (a, b)) in result
            .lanes
            .iter_mut()
            .zip(self.lanes.iter().zip(other.lanes.iter()))
        {
            *r = *a & *b;
        }
        result
    }

    /// SIMD OR — union of two bitfields.
    ///
    /// Returns a new bitfield where bits set in *either* input are set.
    /// Useful for "which files have been checked at all?"
    pub fn or(&self, other: &Self) -> Self {
        let mut result = Self::new(self.len.max(other.len));
        for (r, (a, b)) in result
            .lanes
            .iter_mut()
            .zip(self.lanes.iter().zip(other.lanes.iter()))
        {
            *r = *a | *b;
        }
        result
    }

    /// SIMD AND NOT — difference: bits set in `self` but not in `other`.
    ///
    /// Useful for "which files still need checking?" (all AND NOT checked).
    pub fn and_not(&self, other: &Self) -> Self {
        let mut result = Self::new(self.len.max(other.len));
        for (r, (a, b)) in result
            .lanes
            .iter_mut()
            .zip(self.lanes.iter().zip(other.lanes.iter()))
        {
            *r = *a & !*b;
        }
        result
    }

    /// Returns indices of all set bits.
    pub fn set_indices(&self) -> Vec<usize> {
        let mut indices = Vec::new();
        for (lane_idx, lane) in self.lanes.iter().enumerate() {
            for (word_idx, word) in lane.to_array().iter().enumerate() {
                let mut w = *word;
                while w != 0 {
                    let bit = w.trailing_zeros() as usize;
                    let index = lane_idx * 256 + word_idx * 64 + bit;
                    if index < self.len {
                        indices.push(index);
                    }
                    w &= w - 1; // clear lowest set bit
                }
            }
        }
        indices
    }
}
