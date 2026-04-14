// SPDX-License-Identifier: MIT OR Apache-2.0

//! AICH-inspired sub-piece Merkle tree — identifies corrupt sub-piece regions
//! without re-downloading the entire piece.
//!
//! ## What
//!
//! When a large piece (> 1 MiB) fails SHA-1 verification, the standard
//! approach is to discard the entire piece and re-download it. This module
//! builds a SHA-256 Merkle tree over 256 KiB leaves within each piece,
//! enabling the coordinator to identify which specific leaf (sub-piece
//! region) is corrupt and re-download only that portion.
//!
//! ## Why — aMule AICH (Advanced Intelligent Corruption Handling)
//!
//! aMule's `SHAHashSet` builds a Merkle tree of SHA-1 hashes at 180 KiB
//! leaf granularity inside each eD2k 9.28 MiB part. When part hash
//! verification fails, the tree pinpoints the corrupt leaf — typically
//! saving 95%+ of re-download bandwidth for large parts.
//!
//! This module adapts the concept using SHA-256 and 256 KiB leaves (a
//! power-of-two size for alignment). The tree is useful only for pieces
//! > 1 MiB; smaller pieces are cheaper to re-download entirely.
//!
//! ## How
//!
//! 1. **Build**: [`MerkleTree::build()`] hashes each 256 KiB leaf with
//!    SHA-256, then recursively combines pairs of hashes up to a root.
//! 2. **Verify leaf**: [`MerkleTree::verify_leaf()`] checks a single leaf
//!    against its expected hash. Returns `true` if the leaf is intact.
//! 3. **Find corrupt leaves**: [`MerkleTree::find_corrupt_leaves()`]
//!    returns indices of leaves whose hash doesn't match the expected tree.
//! 4. **Proof generation**: [`MerkleTree::proof()`] generates a Merkle
//!    inclusion proof for a leaf (for future P2P sub-piece verification).

use sha2::{Digest, Sha256};

// ── Constants ───────────────────────────────────────────────────────

/// Leaf size for the sub-piece Merkle tree: 256 KiB.
///
/// Matches common disk sector alignment and provides ~4:1 ratio for
/// typical 1 MiB BT piece sizes. aMule uses 180 KiB (PARTSIZE/53);
/// we use a power-of-two for simplicity.
pub const LEAF_SIZE: usize = 256 * 1024;

/// Minimum piece size (in bytes) for which Merkle sub-piece verification
/// is worthwhile. Below this, re-downloading the piece entirely is cheaper
/// than the tree overhead.
pub const MIN_PIECE_SIZE_FOR_MERKLE: usize = 1024 * 1024;

/// SHA-256 digest length in bytes.
const HASH_LEN: usize = 32;

// ── MerkleTree ──────────────────────────────────────────────────────

/// Sub-piece Merkle tree for AICH-style corruption localisation.
///
/// Built from piece data, the tree stores SHA-256 hashes at each level.
/// Level 0 = leaf hashes. The last level contains the root hash.
///
/// ```
/// use p2p_distribute::merkle::{MerkleTree, LEAF_SIZE};
///
/// // Build a tree from 512 KiB of data (2 leaves).
/// let data = vec![0xABu8; LEAF_SIZE * 2];
/// let tree = MerkleTree::build(&data);
///
/// assert_eq!(tree.leaf_count(), 2);
/// assert_eq!(tree.root_hash().len(), 32);
///
/// // Verify the first leaf is intact.
/// assert!(tree.verify_leaf(0, &data[..LEAF_SIZE]));
///
/// // Corrupt the first leaf — verification fails.
/// let mut corrupted = data[..LEAF_SIZE].to_vec();
/// corrupted[0] = 0xFF;
/// assert!(!tree.verify_leaf(0, &corrupted));
/// ```
#[derive(Debug, Clone)]
pub struct MerkleTree {
    /// All nodes stored level by level. Level 0 = leaf hashes.
    /// `levels[i]` contains `ceil(levels[i-1].len() / 2)` hashes (or leaf
    /// count for level 0).
    levels: Vec<Vec<[u8; HASH_LEN]>>,
    /// Number of data leaves (may be < number of level-0 entries if the
    /// last leaf is padded).
    leaf_count: usize,
}

impl MerkleTree {
    /// Builds a Merkle tree from piece data.
    ///
    /// Divides the data into [`LEAF_SIZE`]-byte chunks, hashes each with
    /// SHA-256, and builds the tree bottom-up. The last chunk may be
    /// smaller than `LEAF_SIZE`.
    pub fn build(data: &[u8]) -> Self {
        // ── Level 0: leaf hashes ────────────────────────────────────
        let leaf_count = if data.is_empty() {
            0
        } else {
            (data.len().saturating_add(LEAF_SIZE - 1)) / LEAF_SIZE
        };

        let mut leaf_hashes = Vec::with_capacity(leaf_count);
        let mut offset = 0usize;
        while offset < data.len() {
            let end = data.len().min(offset.saturating_add(LEAF_SIZE));
            let chunk = data.get(offset..end).unwrap_or(&[]);
            leaf_hashes.push(hash_sha256(chunk));
            offset = end;
        }

        // ── Build tree bottom-up ────────────────────────────────────
        let mut levels = vec![leaf_hashes];
        loop {
            let prev = levels.last().expect("levels is non-empty");
            if prev.len() <= 1 {
                break;
            }
            let next = combine_level(prev);
            levels.push(next);
        }

        Self { levels, leaf_count }
    }

    /// Returns the root hash of the Merkle tree.
    ///
    /// For a single-leaf tree, the root is the leaf hash. For an empty
    /// tree, returns the SHA-256 of an empty slice.
    pub fn root_hash(&self) -> &[u8; HASH_LEN] {
        self.levels
            .last()
            .and_then(|level| level.first())
            .unwrap_or(&EMPTY_HASH)
    }

    /// Number of data leaves in the tree.
    pub fn leaf_count(&self) -> usize {
        self.leaf_count
    }

    /// Number of levels in the tree (including leaves and root).
    pub fn depth(&self) -> usize {
        self.levels.len()
    }

    /// Returns the expected hash for a leaf at the given index.
    pub fn leaf_hash(&self, leaf_index: usize) -> Option<&[u8; HASH_LEN]> {
        self.levels
            .first()
            .and_then(|leaves| leaves.get(leaf_index))
    }

    /// Verifies a single leaf's data against its expected hash.
    ///
    /// Returns `true` if the SHA-256 of `leaf_data` matches the stored
    /// leaf hash. Returns `false` if the hash differs or the leaf index
    /// is out of bounds.
    pub fn verify_leaf(&self, leaf_index: usize, leaf_data: &[u8]) -> bool {
        let Some(expected) = self.leaf_hash(leaf_index) else {
            return false;
        };
        let actual = hash_sha256(leaf_data);
        actual == *expected
    }

    /// Finds all corrupt leaf indices by comparing actual data against
    /// the stored tree.
    ///
    /// Walks the data in [`LEAF_SIZE`] chunks and returns the indices of
    /// leaves whose SHA-256 doesn't match the expected hash.
    pub fn find_corrupt_leaves(&self, data: &[u8]) -> Vec<usize> {
        let mut corrupt = Vec::new();
        let mut offset = 0usize;
        let mut idx = 0usize;

        while offset < data.len() && idx < self.leaf_count {
            let end = data.len().min(offset.saturating_add(LEAF_SIZE));
            let chunk = data.get(offset..end).unwrap_or(&[]);
            if !self.verify_leaf(idx, chunk) {
                corrupt.push(idx);
            }
            offset = end;
            idx = idx.saturating_add(1);
        }

        corrupt
    }

    /// Generates a Merkle inclusion proof for a leaf.
    ///
    /// The proof is a sequence of sibling hashes from leaf to root. A
    /// verifier can recompute the root hash using the leaf data and the
    /// proof to confirm inclusion without the full tree.
    ///
    /// Returns `None` if the leaf index is out of bounds.
    pub fn proof(&self, leaf_index: usize) -> Option<Vec<ProofNode>> {
        if leaf_index >= self.leaf_count {
            return None;
        }

        let mut proof = Vec::with_capacity(self.levels.len().saturating_sub(1));
        let mut idx = leaf_index;

        for level in &self.levels {
            if level.len() <= 1 {
                break;
            }
            // Sibling is the other half of the pair.
            let sibling_idx = idx ^ 1;
            if let Some(sibling_hash) = level.get(sibling_idx) {
                proof.push(ProofNode {
                    hash: *sibling_hash,
                    is_left: sibling_idx < idx,
                });
            }
            // Move to parent index.
            idx /= 2;
        }

        Some(proof)
    }

    /// Verifies a Merkle proof against the root hash.
    ///
    /// Recomputes the root from `leaf_data` and the proof nodes, then
    /// compares against the stored root. Returns `true` if the proof is
    /// valid.
    pub fn verify_proof(&self, leaf_data: &[u8], proof: &[ProofNode]) -> bool {
        let mut current = hash_sha256(leaf_data);

        for node in proof {
            current = if node.is_left {
                combine_hashes(&node.hash, &current)
            } else {
                combine_hashes(&current, &node.hash)
            };
        }

        current == *self.root_hash()
    }
}

// ── ProofNode ───────────────────────────────────────────────────────

/// A single node in a Merkle inclusion proof.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProofNode {
    /// SHA-256 hash of the sibling node.
    pub hash: [u8; HASH_LEN],
    /// Whether this sibling is on the left side of the pair.
    pub is_left: bool,
}

// ── Internal helpers ────────────────────────────────────────────────

/// SHA-256 hash of the empty byte slice (used for empty trees).
const EMPTY_HASH: [u8; HASH_LEN] = {
    // SHA-256("") = e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855
    [
        0xe3, 0xb0, 0xc4, 0x42, 0x98, 0xfc, 0x1c, 0x14, 0x9a, 0xfb, 0xf4, 0xc8, 0x99, 0x6f, 0xb9,
        0x24, 0x27, 0xae, 0x41, 0xe4, 0x64, 0x9b, 0x93, 0x4c, 0xa4, 0x95, 0x99, 0x1b, 0x78, 0x52,
        0xb8, 0x55,
    ]
};

/// Computes SHA-256 of a byte slice, returning a fixed-size array.
fn hash_sha256(data: &[u8]) -> [u8; HASH_LEN] {
    let mut hasher = Sha256::new();
    hasher.update(data);
    let result = hasher.finalize();
    let mut out = [0u8; HASH_LEN];
    out.copy_from_slice(&result);
    out
}

/// Combines two hashes into a parent hash: SHA-256(left || right).
fn combine_hashes(left: &[u8; HASH_LEN], right: &[u8; HASH_LEN]) -> [u8; HASH_LEN] {
    let mut hasher = Sha256::new();
    hasher.update(left);
    hasher.update(right);
    let result = hasher.finalize();
    let mut out = [0u8; HASH_LEN];
    out.copy_from_slice(&result);
    out
}

/// Combines a level of hashes into the next level by pairing adjacent nodes.
///
/// Odd-count levels: the last node is promoted unpaired (like aMule's
/// AICH tree handling for non-power-of-two leaf counts).
fn combine_level(level: &[[u8; HASH_LEN]]) -> Vec<[u8; HASH_LEN]> {
    let parent_count = (level.len().saturating_add(1)) / 2;
    let mut parents = Vec::with_capacity(parent_count);

    let mut i = 0;
    while i < level.len() {
        if i.saturating_add(1) < level.len() {
            let left = level.get(i).expect("bounds checked");
            let right = level.get(i.saturating_add(1)).expect("bounds checked");
            parents.push(combine_hashes(left, right));
        } else {
            // Odd node — promote unpaired.
            parents.push(*level.get(i).expect("bounds checked"));
        }
        i = i.saturating_add(2);
    }

    parents
}

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── Construction ────────────────────────────────────────────────

    /// Empty data produces a tree with zero leaves.
    ///
    /// The root hash should be the SHA-256 of empty input.
    #[test]
    fn empty_data_tree() {
        let tree = MerkleTree::build(&[]);
        assert_eq!(tree.leaf_count(), 0);
        assert_eq!(*tree.root_hash(), EMPTY_HASH);
    }

    /// Single leaf tree: root hash equals the leaf hash.
    ///
    /// With only one leaf, there are no internal nodes to combine.
    #[test]
    fn single_leaf_root_equals_leaf() {
        let data = vec![0xAAu8; LEAF_SIZE];
        let tree = MerkleTree::build(&data);
        assert_eq!(tree.leaf_count(), 1);
        assert_eq!(tree.depth(), 1);
        assert_eq!(*tree.root_hash(), hash_sha256(&data));
    }

    /// Two-leaf tree has correct structure.
    ///
    /// Level 0: 2 leaf hashes. Level 1: 1 root hash = SHA-256(leaf0 || leaf1).
    #[test]
    fn two_leaf_tree_structure() {
        let data = vec![0xBBu8; LEAF_SIZE * 2];
        let tree = MerkleTree::build(&data);
        assert_eq!(tree.leaf_count(), 2);
        assert_eq!(tree.depth(), 2);

        let leaf0 = hash_sha256(&data[..LEAF_SIZE]);
        let leaf1 = hash_sha256(&data[LEAF_SIZE..]);
        let expected_root = combine_hashes(&leaf0, &leaf1);
        assert_eq!(*tree.root_hash(), expected_root);
    }

    /// Three-leaf tree: odd count promotes the last leaf unpaired.
    ///
    /// Level 0: 3 leaves. Level 1: pair(0,1) + promote(2) = 2 nodes.
    /// Level 2: pair of those = 1 root.
    #[test]
    fn three_leaf_odd_promotion() {
        let data = vec![0xCCu8; LEAF_SIZE * 3];
        let tree = MerkleTree::build(&data);
        assert_eq!(tree.leaf_count(), 3);
        assert!(tree.depth() >= 2);
    }

    /// Partial last leaf (data not a multiple of LEAF_SIZE).
    ///
    /// The last leaf is smaller than LEAF_SIZE but still hashed correctly.
    #[test]
    fn partial_last_leaf() {
        let data = vec![0xDDu8; LEAF_SIZE + 100];
        let tree = MerkleTree::build(&data);
        assert_eq!(tree.leaf_count(), 2);

        // Verify the partial leaf.
        assert!(tree.verify_leaf(1, &data[LEAF_SIZE..]));
    }

    // ── Leaf verification ───────────────────────────────────────────

    /// Intact leaf passes verification.
    ///
    /// Verifying original data against its stored hash must succeed.
    #[test]
    fn verify_intact_leaf() {
        let data = vec![0xEEu8; LEAF_SIZE * 2];
        let tree = MerkleTree::build(&data);
        assert!(tree.verify_leaf(0, &data[..LEAF_SIZE]));
        assert!(tree.verify_leaf(1, &data[LEAF_SIZE..]));
    }

    /// Corrupted leaf fails verification.
    ///
    /// Flipping a single bit must cause the leaf hash to differ.
    #[test]
    fn verify_corrupt_leaf_fails() {
        let data = vec![0xFFu8; LEAF_SIZE * 2];
        let tree = MerkleTree::build(&data);

        let mut corrupted = data[..LEAF_SIZE].to_vec();
        corrupted[0] = 0x00;
        assert!(!tree.verify_leaf(0, &corrupted));
    }

    /// Out-of-bounds leaf index returns false.
    ///
    /// Prevents panics when querying beyond the tree.
    #[test]
    fn verify_out_of_bounds_leaf() {
        let data = vec![0x11u8; LEAF_SIZE];
        let tree = MerkleTree::build(&data);
        assert!(!tree.verify_leaf(99, &[0x11u8; LEAF_SIZE]));
    }

    // ── Corrupt leaf detection ──────────────────────────────────────

    /// find_corrupt_leaves identifies the corrupted leaf.
    ///
    /// Only the modified leaf should appear in the result, not the intact one.
    #[test]
    fn find_corrupt_leaves_identifies_bad_leaf() {
        let mut data = vec![0xAAu8; LEAF_SIZE * 3];
        let tree = MerkleTree::build(&data);

        // Corrupt the second leaf.
        data[LEAF_SIZE] = 0x00;

        let corrupt = tree.find_corrupt_leaves(&data);
        assert_eq!(corrupt, vec![1]);
    }

    /// All intact data returns no corrupt leaves.
    #[test]
    fn find_corrupt_leaves_all_intact() {
        let data = vec![0xBBu8; LEAF_SIZE * 2];
        let tree = MerkleTree::build(&data);
        let corrupt = tree.find_corrupt_leaves(&data);
        assert!(corrupt.is_empty());
    }

    // ── Merkle proofs ───────────────────────────────────────────────

    /// Valid proof verifies against the root.
    ///
    /// The inclusion proof for a leaf should allow root recomputation.
    #[test]
    fn valid_proof_verifies() {
        let data = vec![0xCCu8; LEAF_SIZE * 4];
        let tree = MerkleTree::build(&data);

        for i in 0..4 {
            let start = i * LEAF_SIZE;
            let end = start + LEAF_SIZE;
            let leaf_data = &data[start..end];
            let proof = tree.proof(i).expect("valid index");
            assert!(
                tree.verify_proof(leaf_data, &proof),
                "proof for leaf {i} should verify"
            );
        }
    }

    /// Proof for corrupted data fails verification.
    ///
    /// Modifying the leaf data should make the proof invalid.
    #[test]
    fn corrupt_data_fails_proof() {
        let data = vec![0xDDu8; LEAF_SIZE * 2];
        let tree = MerkleTree::build(&data);

        let proof = tree.proof(0).expect("valid index");
        let mut bad_data = data[..LEAF_SIZE].to_vec();
        bad_data[0] = 0xFF;
        assert!(!tree.verify_proof(&bad_data, &proof));
    }

    /// Proof for out-of-bounds index returns None.
    #[test]
    fn proof_out_of_bounds_returns_none() {
        let data = vec![0xEEu8; LEAF_SIZE];
        let tree = MerkleTree::build(&data);
        assert!(tree.proof(5).is_none());
    }

    // ── Determinism ─────────────────────────────────────────────────

    /// Building the same data twice produces identical trees.
    ///
    /// SHA-256 is deterministic; the tree structure must be too.
    #[test]
    fn tree_build_deterministic() {
        let data = vec![0x42u8; LEAF_SIZE * 3];
        let tree1 = MerkleTree::build(&data);
        let tree2 = MerkleTree::build(&data);
        assert_eq!(tree1.root_hash(), tree2.root_hash());
        assert_eq!(tree1.leaf_count(), tree2.leaf_count());
    }
}
