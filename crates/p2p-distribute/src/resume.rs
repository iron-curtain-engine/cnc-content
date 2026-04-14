// SPDX-License-Identifier: MIT OR Apache-2.0

//! Persistent download resume state — checkpoint file for crash recovery.
//!
//! Saves a compact binary representation of which pieces have been downloaded
//! and verified. After a crash or interrupt, the coordinator loads the resume
//! state and skips already-completed pieces instead of restarting from zero.
//!
//! ## File format
//!
//! ```text
//! [0..4]   Magic bytes "P2PR"
//! [4]      Version (0x01)
//! [5..9]   piece_count: u32 LE
//! [9..17]  file_size: u64 LE
//! [17..]   Packed bitfield: ceil(piece_count / 8) bytes
//!          Bit N of byte (N/8) = piece N is done (LSB-first)
//! ```
//!
//! Total overhead: 17 bytes header + ceil(piece_count / 8) bitfield.
//! For a typical 146-piece torrent: 17 + 19 = 36 bytes.
//!
//! ## Design rationale (FlashGet .jcd pattern)
//!
//! Every production download manager since FlashGet (1999) persists resume
//! state to a sidecar file. Without this, an interrupted multi-hundred-MB
//! download restarts from zero — unacceptable for users on slow connections.
//!
//! The format is intentionally simple: no external dependencies, no
//! compression, no encryption. The piece bitmap is small enough that
//! writing it on every piece completion has negligible I/O cost (typically
//! < 100 bytes for our torrents).

use std::path::{Path, PathBuf};

use thiserror::Error;

use crate::piece_map::{PieceState, SharedPieceMap};

// ── Constants ───────────────────────────────────────────────────────

/// Magic bytes identifying a resume state file.
const MAGIC: &[u8; 4] = b"P2PR";

/// Current file format version.
const VERSION: u8 = 0x01;

/// Fixed header size: magic (4) + version (1) + piece_count (4) + file_size (8).
const HEADER_SIZE: usize = 17;

// ── Errors ──────────────────────────────────────────────────────────

/// Errors from resume state operations.
#[derive(Debug, Error)]
pub enum ResumeError {
    /// File system I/O failure during save or load.
    #[error("resume I/O error: {source}")]
    Io {
        #[from]
        source: std::io::Error,
    },
    /// File does not start with the expected "P2PR" magic bytes.
    #[error("invalid resume file magic: expected P2PR, got {found:02x?}")]
    InvalidMagic { found: [u8; 4] },
    /// Resume file was written by a newer, incompatible version.
    #[error("unsupported resume file version {version} (expected {VERSION})")]
    UnsupportedVersion { version: u8 },
    /// Resume file is shorter than the header or bitfield requires.
    #[error("resume file truncated: expected {expected_bytes} bytes, got {actual_bytes}")]
    Truncated {
        expected_bytes: usize,
        actual_bytes: usize,
    },
    /// Resume file piece count does not match the torrent being downloaded.
    #[error("piece count mismatch: resume has {found}, torrent has {expected}")]
    PieceCountMismatch { expected: u32, found: u32 },
    /// Resume file size does not match the torrent being downloaded.
    #[error("file size mismatch: resume has {found}, torrent has {expected}")]
    FileSizeMismatch { expected: u64, found: u64 },
}

// ── ResumeState ─────────────────────────────────────────────────────

/// Persistent resume state for crash recovery.
///
/// Tracks which pieces have been downloaded and verified using a packed
/// bitfield. Serializes to a compact binary file (< 100 bytes for typical
/// torrents) that can be written after each piece completion without
/// measurable I/O overhead.
///
/// ## Usage
///
/// ```rust
/// use p2p_distribute::resume::ResumeState;
///
/// // Create empty state for a 100-piece, 256 MB download
/// let mut state = ResumeState::new(100, 256_000_000);
/// state.mark_done(0);
/// state.mark_done(42);
/// assert_eq!(state.done_count(), 2);
/// assert!(state.is_done(0));
/// assert!(state.is_done(42));
/// assert!(!state.is_done(1));
///
/// // Save to disk and reload
/// let path = std::env::temp_dir().join("p2p-resume-doctest.resume");
/// state.save(&path).unwrap();
/// let loaded = ResumeState::load(&path).unwrap();
/// assert_eq!(loaded.done_count(), 2);
/// assert!(loaded.is_done(42));
/// let _ = std::fs::remove_file(&path);
/// ```
#[derive(Debug)]
pub struct ResumeState {
    /// Number of pieces in the torrent.
    piece_count: u32,
    /// Total expected file size in bytes (for validation on reload).
    file_size: u64,
    /// Packed bitfield: bit N of byte (N/8) = piece N is done (LSB-first).
    bitfield: Vec<u8>,
}

impl ResumeState {
    /// Creates an empty resume state (no pieces completed).
    pub fn new(piece_count: u32, file_size: u64) -> Self {
        let byte_count = packed_byte_count(piece_count);
        Self {
            piece_count,
            file_size,
            bitfield: vec![0u8; byte_count],
        }
    }

    /// Snapshots the current state of a [`SharedPieceMap`].
    ///
    /// Pieces in [`PieceState::Done`] are marked as completed. All other
    /// states (Needed, InFlight, Failed) are treated as incomplete —
    /// the coordinator will re-download them on resume.
    pub fn from_piece_map(piece_map: &SharedPieceMap, file_size: u64) -> Self {
        let piece_count = piece_map.piece_count();
        let byte_count = packed_byte_count(piece_count);
        let mut bitfield = vec![0u8; byte_count];

        for i in 0..piece_count {
            if piece_map.get(i) == PieceState::Done {
                set_bit(&mut bitfield, i);
            }
        }

        Self {
            piece_count,
            file_size,
            bitfield,
        }
    }

    /// Marks a piece as completed.
    pub fn mark_done(&mut self, piece_index: u32) {
        set_bit(&mut self.bitfield, piece_index);
    }

    /// Returns whether a piece is marked as completed.
    pub fn is_done(&self, piece_index: u32) -> bool {
        get_bit(&self.bitfield, piece_index)
    }

    /// Returns the number of completed pieces.
    pub fn done_count(&self) -> u32 {
        let mut count = 0u32;
        for i in 0..self.piece_count {
            if self.is_done(i) {
                count = count.saturating_add(1);
            }
        }
        count
    }

    /// Returns the piece count stored in the resume state.
    pub fn piece_count(&self) -> u32 {
        self.piece_count
    }

    /// Returns the file size stored in the resume state.
    pub fn file_size(&self) -> u64 {
        self.file_size
    }

    /// Converts to a `Vec<bool>` for use with
    /// [`PieceCoordinator::new_resume`](crate::PieceCoordinator::new_resume).
    pub fn to_verified_pieces(&self) -> Vec<bool> {
        (0..self.piece_count).map(|i| self.is_done(i)).collect()
    }

    /// Validates that this resume state matches the expected torrent parameters.
    ///
    /// Returns `Ok(())` if piece count and file size match. Use this before
    /// trusting pieces from a resume file to guard against mismatched torrents.
    pub fn validate(
        &self,
        expected_piece_count: u32,
        expected_file_size: u64,
    ) -> Result<(), ResumeError> {
        if self.piece_count != expected_piece_count {
            return Err(ResumeError::PieceCountMismatch {
                expected: expected_piece_count,
                found: self.piece_count,
            });
        }
        if self.file_size != expected_file_size {
            return Err(ResumeError::FileSizeMismatch {
                expected: expected_file_size,
                found: self.file_size,
            });
        }
        Ok(())
    }

    /// Saves the resume state to a binary file.
    ///
    /// The file is written atomically: data goes to a temporary file first,
    /// then renamed to the target path. This prevents corrupted resume files
    /// if the process crashes during the write.
    pub fn save(&self, path: &Path) -> Result<(), ResumeError> {
        let mut buf = Vec::with_capacity(HEADER_SIZE.saturating_add(self.bitfield.len()));

        // ── Header ──────────────────────────────────────────────────
        buf.extend_from_slice(MAGIC);
        buf.push(VERSION);
        buf.extend_from_slice(&self.piece_count.to_le_bytes());
        buf.extend_from_slice(&self.file_size.to_le_bytes());

        // ── Bitfield ────────────────────────────────────────────────
        buf.extend_from_slice(&self.bitfield);

        // ── Atomic write via temp file + rename ─────────────────────
        //
        // Write to a sibling .tmp file, then rename. If the process
        // crashes between write and rename, the old resume file (if any)
        // is preserved intact. rename() is atomic on all major OSes
        // (POSIX: rename(2); Windows: MoveFileExW with MOVEFILE_REPLACE_EXISTING).
        let tmp_path = tmp_sibling(path);
        std::fs::write(&tmp_path, &buf)?;
        std::fs::rename(&tmp_path, path)?;

        Ok(())
    }

    /// Loads a resume state from a binary file.
    ///
    /// Validates the magic bytes, version, and bitfield length. Returns a
    /// structured error for each failure mode so the caller can distinguish
    /// "corrupt file" from "wrong torrent" from "I/O error".
    pub fn load(path: &Path) -> Result<Self, ResumeError> {
        let data = std::fs::read(path)?;

        // ── Validate minimum size ───────────────────────────────────
        if data.len() < HEADER_SIZE {
            return Err(ResumeError::Truncated {
                expected_bytes: HEADER_SIZE,
                actual_bytes: data.len(),
            });
        }

        // ── Magic bytes [0..4] ──────────────────────────────────────
        let magic_slice = data.get(0..4).ok_or(ResumeError::Truncated {
            expected_bytes: HEADER_SIZE,
            actual_bytes: data.len(),
        })?;
        let mut magic = [0u8; 4];
        magic.copy_from_slice(magic_slice);
        if &magic != MAGIC {
            return Err(ResumeError::InvalidMagic { found: magic });
        }

        // ── Version [4] ────────────────────────────────────────────
        let version = data.get(4).copied().ok_or(ResumeError::Truncated {
            expected_bytes: HEADER_SIZE,
            actual_bytes: data.len(),
        })?;
        if version != VERSION {
            return Err(ResumeError::UnsupportedVersion { version });
        }

        // ── piece_count [5..9] ─────────────────────────────────────
        let pc_bytes = data.get(5..9).ok_or(ResumeError::Truncated {
            expected_bytes: HEADER_SIZE,
            actual_bytes: data.len(),
        })?;
        let piece_count =
            u32::from_le_bytes(pc_bytes.try_into().map_err(|_| ResumeError::Truncated {
                expected_bytes: HEADER_SIZE,
                actual_bytes: data.len(),
            })?);

        // ── file_size [9..17] ──────────────────────────────────────
        let fs_bytes = data.get(9..17).ok_or(ResumeError::Truncated {
            expected_bytes: HEADER_SIZE,
            actual_bytes: data.len(),
        })?;
        let file_size =
            u64::from_le_bytes(fs_bytes.try_into().map_err(|_| ResumeError::Truncated {
                expected_bytes: HEADER_SIZE,
                actual_bytes: data.len(),
            })?);

        // ── Bitfield [17..] ────────────────────────────────────────
        let expected_bytes = packed_byte_count(piece_count);
        let bitfield_slice = data.get(HEADER_SIZE..).unwrap_or(&[]);
        if bitfield_slice.len() < expected_bytes {
            return Err(ResumeError::Truncated {
                expected_bytes: HEADER_SIZE.saturating_add(expected_bytes),
                actual_bytes: data.len(),
            });
        }
        let bitfield = bitfield_slice
            .get(..expected_bytes)
            .unwrap_or(bitfield_slice)
            .to_vec();

        Ok(Self {
            piece_count,
            file_size,
            bitfield,
        })
    }
}

// ── Bitfield helpers ────────────────────────────────────────────────

/// Returns the number of bytes needed to pack `piece_count` bits.
fn packed_byte_count(piece_count: u32) -> usize {
    ((piece_count as usize).saturating_add(7)) / 8
}

/// Sets bit `index` in the packed bitfield (LSB-first within each byte).
fn set_bit(bitfield: &mut [u8], index: u32) {
    let byte_idx = (index / 8) as usize;
    let bit_idx = index % 8;
    if let Some(byte) = bitfield.get_mut(byte_idx) {
        *byte |= 1 << bit_idx;
    }
}

/// Gets bit `index` from the packed bitfield (LSB-first within each byte).
fn get_bit(bitfield: &[u8], index: u32) -> bool {
    let byte_idx = (index / 8) as usize;
    let bit_idx = index % 8;
    bitfield
        .get(byte_idx)
        .is_some_and(|byte| (byte >> bit_idx) & 1 == 1)
}

/// Returns a sibling path with `.tmp` appended (e.g. `foo.resume` → `foo.resume.tmp`).
fn tmp_sibling(path: &Path) -> PathBuf {
    let mut os = path.as_os_str().to_owned();
    os.push(".tmp");
    PathBuf::from(os)
}

// ── PeerState ───────────────────────────────────────────────────────

/// Magic bytes for peer state companion file.
const PEER_MAGIC: &[u8; 4] = b"P2PP";

/// Peer state file format version.
const PEER_VERSION: u8 = 0x01;

/// Persistent peer state — companion file for crash recovery.
///
/// Stores known-good peer reputations and timed exclusion state so that
/// session restarts don't lose peer trust history. Saved alongside the
/// piece resume file (`.resume` → `.peers`).
///
/// ## File format
///
/// ```text
/// [0..4]   Magic bytes "P2PP"
/// [4]      Version (0x01)
/// [5..9]   entry_count: u32 LE
/// [9..]    Entries, each:
///            [0..32]   peer_id bytes
///            [32]      trust_level: u8 (0=Untested, 1=Probationary, 2=Established, 3=Trusted)
///            [33..41]  lifetime_pieces_served: u64 LE
///            [41..45]  lifetime_corruption_count: u32 LE
///            [45..53]  avg_speed_bytes_per_sec: u64 LE
///            [53..61]  last_seen_unix_secs: u64 LE
///            [61]      banned: u8 (0=false, 1=true)
///            [62..66]  exclusion_count: u32 LE
///          Total per entry: 66 bytes
/// ```
///
/// ## Usage
///
/// ```rust
/// use p2p_distribute::resume::PeerState;
/// use p2p_distribute::peer_stats::{PeerReputation, TrustLevel};
/// use p2p_distribute::PeerId;
///
/// let reps = vec![PeerReputation {
///     peer_id: PeerId::from_key_material(b"alice"),
///     trust_level: TrustLevel::Trusted,
///     lifetime_pieces_served: 500,
///     lifetime_corruption_count: 0,
///     avg_speed_bytes_per_sec: 50_000,
///     last_seen_unix_secs: 1_700_000_000,
///     banned: false,
/// }];
///
/// let state = PeerState::new(reps, vec![0]);
/// let path = std::env::temp_dir().join("p2p-peers-doctest.peers");
/// state.save(&path).unwrap();
///
/// let loaded = PeerState::load(&path).unwrap();
/// assert_eq!(loaded.reputations().len(), 1);
/// assert_eq!(loaded.reputations()[0].trust_level, TrustLevel::Trusted);
/// let _ = std::fs::remove_file(&path);
/// ```
#[derive(Debug)]
pub struct PeerState {
    /// Peer reputations from the last session.
    reputations: Vec<crate::peer_stats::PeerReputation>,
    /// Per-peer exclusion counts (indexed parallel to `reputations`).
    exclusion_counts: Vec<u32>,
}

/// Size of each peer entry in the binary format.
const PEER_ENTRY_SIZE: usize = 66;

/// Peer state header size: magic(4) + version(1) + entry_count(4) = 9.
const PEER_HEADER_SIZE: usize = 9;

impl PeerState {
    /// Creates a new peer state from reputations and exclusion counts.
    pub fn new(
        reputations: Vec<crate::peer_stats::PeerReputation>,
        exclusion_counts: Vec<u32>,
    ) -> Self {
        Self {
            reputations,
            exclusion_counts,
        }
    }

    /// Returns the stored reputations.
    pub fn reputations(&self) -> &[crate::peer_stats::PeerReputation] {
        &self.reputations
    }

    /// Returns the stored exclusion counts.
    pub fn exclusion_counts(&self) -> &[u32] {
        &self.exclusion_counts
    }

    /// Saves peer state to a binary file (atomic write).
    pub fn save(&self, path: &Path) -> Result<(), ResumeError> {
        let entry_count = self.reputations.len() as u32;
        let total_size =
            PEER_HEADER_SIZE.saturating_add((entry_count as usize).saturating_mul(PEER_ENTRY_SIZE));
        let mut buf = Vec::with_capacity(total_size);

        // ── Header ──────────────────────────────────────────────────
        buf.extend_from_slice(PEER_MAGIC);
        buf.push(PEER_VERSION);
        buf.extend_from_slice(&entry_count.to_le_bytes());

        // ── Entries ─────────────────────────────────────────────────
        for (i, rep) in self.reputations.iter().enumerate() {
            buf.extend_from_slice(rep.peer_id.as_bytes());
            buf.push(trust_level_to_u8(rep.trust_level));
            buf.extend_from_slice(&rep.lifetime_pieces_served.to_le_bytes());
            buf.extend_from_slice(&rep.lifetime_corruption_count.to_le_bytes());
            buf.extend_from_slice(&rep.avg_speed_bytes_per_sec.to_le_bytes());
            buf.extend_from_slice(&rep.last_seen_unix_secs.to_le_bytes());
            buf.push(if rep.banned { 1 } else { 0 });
            let exc = self.exclusion_counts.get(i).copied().unwrap_or(0);
            buf.extend_from_slice(&exc.to_le_bytes());
        }

        let tmp_path = tmp_sibling(path);
        std::fs::write(&tmp_path, &buf)?;
        std::fs::rename(&tmp_path, path)?;
        Ok(())
    }

    /// Loads peer state from a binary file.
    pub fn load(path: &Path) -> Result<Self, ResumeError> {
        let data = std::fs::read(path)?;

        if data.len() < PEER_HEADER_SIZE {
            return Err(ResumeError::Truncated {
                expected_bytes: PEER_HEADER_SIZE,
                actual_bytes: data.len(),
            });
        }

        // ── Magic ───────────────────────────────────────────────────
        let magic_slice = data.get(0..4).ok_or(ResumeError::Truncated {
            expected_bytes: PEER_HEADER_SIZE,
            actual_bytes: data.len(),
        })?;
        let mut magic = [0u8; 4];
        magic.copy_from_slice(magic_slice);
        if &magic != PEER_MAGIC {
            return Err(ResumeError::InvalidMagic { found: magic });
        }

        // ── Version ─────────────────────────────────────────────────
        let version = data.get(4).copied().ok_or(ResumeError::Truncated {
            expected_bytes: PEER_HEADER_SIZE,
            actual_bytes: data.len(),
        })?;
        if version != PEER_VERSION {
            return Err(ResumeError::UnsupportedVersion { version });
        }

        // ── Entry count ─────────────────────────────────────────────
        let ec_bytes = data.get(5..9).ok_or(ResumeError::Truncated {
            expected_bytes: PEER_HEADER_SIZE,
            actual_bytes: data.len(),
        })?;
        let entry_count =
            u32::from_le_bytes(ec_bytes.try_into().map_err(|_| ResumeError::Truncated {
                expected_bytes: PEER_HEADER_SIZE,
                actual_bytes: data.len(),
            })?);

        let expected_size =
            PEER_HEADER_SIZE.saturating_add((entry_count as usize).saturating_mul(PEER_ENTRY_SIZE));
        if data.len() < expected_size {
            return Err(ResumeError::Truncated {
                expected_bytes: expected_size,
                actual_bytes: data.len(),
            });
        }

        // ── Parse entries ───────────────────────────────────────────
        let mut reputations = Vec::with_capacity(entry_count as usize);
        let mut exclusion_counts = Vec::with_capacity(entry_count as usize);
        let mut offset = PEER_HEADER_SIZE;

        for _ in 0..entry_count {
            let entry = data
                .get(offset..offset.saturating_add(PEER_ENTRY_SIZE))
                .ok_or(ResumeError::Truncated {
                    expected_bytes: expected_size,
                    actual_bytes: data.len(),
                })?;

            let mut peer_id_bytes = [0u8; 32];
            peer_id_bytes.copy_from_slice(entry.get(0..32).ok_or(ResumeError::Truncated {
                expected_bytes: expected_size,
                actual_bytes: data.len(),
            })?);
            let peer_id = crate::peer_id::PeerId::from_bytes(peer_id_bytes);

            let trust_byte = entry.get(32).copied().ok_or(ResumeError::Truncated {
                expected_bytes: expected_size,
                actual_bytes: data.len(),
            })?;
            let trust_level = trust_level_from_u8(trust_byte);

            let lps = u64::from_le_bytes(
                entry
                    .get(33..41)
                    .ok_or(ResumeError::Truncated {
                        expected_bytes: expected_size,
                        actual_bytes: data.len(),
                    })?
                    .try_into()
                    .map_err(|_| ResumeError::Truncated {
                        expected_bytes: expected_size,
                        actual_bytes: data.len(),
                    })?,
            );

            let lcc = u32::from_le_bytes(
                entry
                    .get(41..45)
                    .ok_or(ResumeError::Truncated {
                        expected_bytes: expected_size,
                        actual_bytes: data.len(),
                    })?
                    .try_into()
                    .map_err(|_| ResumeError::Truncated {
                        expected_bytes: expected_size,
                        actual_bytes: data.len(),
                    })?,
            );

            let avg_speed = u64::from_le_bytes(
                entry
                    .get(45..53)
                    .ok_or(ResumeError::Truncated {
                        expected_bytes: expected_size,
                        actual_bytes: data.len(),
                    })?
                    .try_into()
                    .map_err(|_| ResumeError::Truncated {
                        expected_bytes: expected_size,
                        actual_bytes: data.len(),
                    })?,
            );

            let last_seen = u64::from_le_bytes(
                entry
                    .get(53..61)
                    .ok_or(ResumeError::Truncated {
                        expected_bytes: expected_size,
                        actual_bytes: data.len(),
                    })?
                    .try_into()
                    .map_err(|_| ResumeError::Truncated {
                        expected_bytes: expected_size,
                        actual_bytes: data.len(),
                    })?,
            );

            let banned = entry.get(61).copied().unwrap_or(0) != 0;

            let exc_count = u32::from_le_bytes(
                entry
                    .get(62..66)
                    .ok_or(ResumeError::Truncated {
                        expected_bytes: expected_size,
                        actual_bytes: data.len(),
                    })?
                    .try_into()
                    .map_err(|_| ResumeError::Truncated {
                        expected_bytes: expected_size,
                        actual_bytes: data.len(),
                    })?,
            );

            reputations.push(crate::peer_stats::PeerReputation {
                peer_id,
                trust_level,
                lifetime_pieces_served: lps,
                lifetime_corruption_count: lcc,
                avg_speed_bytes_per_sec: avg_speed,
                last_seen_unix_secs: last_seen,
                banned,
            });
            exclusion_counts.push(exc_count);

            offset = offset.saturating_add(PEER_ENTRY_SIZE);
        }

        Ok(Self {
            reputations,
            exclusion_counts,
        })
    }

    /// Returns the path for a peer state file given the resume path.
    ///
    /// Convention: `foo.resume` → `foo.peers`.
    pub fn peer_path_from_resume(resume_path: &Path) -> PathBuf {
        let mut p = resume_path.as_os_str().to_owned();
        // Replace .resume extension with .peers, or append .peers.
        let path_str = resume_path.to_string_lossy();
        if path_str.ends_with(".resume") {
            let base = &path_str[..path_str.len().saturating_sub(7)];
            PathBuf::from(format!("{base}.peers"))
        } else {
            p.push(".peers");
            PathBuf::from(p)
        }
    }
}

/// Converts a TrustLevel to a u8 discriminant.
fn trust_level_to_u8(level: crate::peer_stats::TrustLevel) -> u8 {
    match level {
        crate::peer_stats::TrustLevel::Untested => 0,
        crate::peer_stats::TrustLevel::Probationary => 1,
        crate::peer_stats::TrustLevel::Established => 2,
        crate::peer_stats::TrustLevel::Trusted => 3,
    }
}

/// Converts a u8 discriminant back to a TrustLevel.
fn trust_level_from_u8(byte: u8) -> crate::peer_stats::TrustLevel {
    match byte {
        0 => crate::peer_stats::TrustLevel::Untested,
        1 => crate::peer_stats::TrustLevel::Probationary,
        2 => crate::peer_stats::TrustLevel::Established,
        3 => crate::peer_stats::TrustLevel::Trusted,
        _ => crate::peer_stats::TrustLevel::Untested,
    }
}

// ── SubPieceProgress ────────────────────────────────────────────────

/// Sub-piece byte offset tracking — DCC RESUME pattern.
///
/// ## Why — IRC DCC RESUME lesson
///
/// IRC's DCC SEND protocol transfers files one-to-one over TCP. If the
/// connection drops mid-transfer, DCC RESUME lets the receiver propose
/// a byte offset to restart from, avoiding re-transferring the beginning
/// of the file. The sender confirms with DCC ACCEPT.
///
/// Applied to P2P: pieces in BitTorrent are typically 256 KiB–4 MiB.
/// On slow connections, downloading a single piece can take seconds to
/// minutes. If the connection drops mid-piece, re-downloading from byte 0
/// wastes whatever partial data was already received.
///
/// `SubPieceProgress` tracks how many bytes of each in-progress piece
/// have been received and verified. On reconnection, the coordinator can
/// send a "resume from offset X" request instead of restarting the piece.
///
/// ## How
///
/// A simple `HashMap<u32, u64>` mapping piece_index → bytes_received.
/// Only in-progress pieces are tracked. Completed pieces are removed
/// (they're in the ResumeState bitfield). Failed pieces are reset to 0.
///
/// ```
/// use p2p_distribute::resume::SubPieceProgress;
///
/// let mut progress = SubPieceProgress::new();
///
/// // Peer is downloading piece 7, has received 128 KiB so far.
/// progress.update(7, 131_072);
/// assert_eq!(progress.bytes_received(7), 131_072);
///
/// // Piece completed — clear the sub-piece tracking.
/// progress.complete(7);
/// assert_eq!(progress.bytes_received(7), 0);
/// ```
#[derive(Debug, Clone, Default)]
pub struct SubPieceProgress {
    /// In-progress piece byte offsets: piece_index → bytes_received.
    offsets: std::collections::HashMap<u32, u64>,
}

impl SubPieceProgress {
    /// Creates empty sub-piece progress (no in-progress pieces).
    pub fn new() -> Self {
        Self::default()
    }

    /// Updates the byte offset for an in-progress piece.
    ///
    /// Call this after each successful sub-piece block reception.
    pub fn update(&mut self, piece_index: u32, bytes_received: u64) {
        self.offsets.insert(piece_index, bytes_received);
    }

    /// Returns the byte offset (bytes already received) for a piece.
    ///
    /// Returns 0 if the piece is not being tracked (not in progress).
    pub fn bytes_received(&self, piece_index: u32) -> u64 {
        self.offsets.get(&piece_index).copied().unwrap_or(0)
    }

    /// Marks a piece as completed — removes it from tracking.
    pub fn complete(&mut self, piece_index: u32) {
        self.offsets.remove(&piece_index);
    }

    /// Resets a piece to offset 0 (re-download from start).
    pub fn reset(&mut self, piece_index: u32) {
        self.offsets.remove(&piece_index);
    }

    /// Returns the number of pieces currently being tracked.
    pub fn in_progress_count(&self) -> usize {
        self.offsets.len()
    }

    /// Returns `true` if no pieces are being tracked.
    pub fn is_empty(&self) -> bool {
        self.offsets.is_empty()
    }

    /// Returns an iterator over (piece_index, bytes_received) pairs.
    pub fn iter(&self) -> impl Iterator<Item = (u32, u64)> + '_ {
        self.offsets.iter().map(|(&k, &v)| (k, v))
    }

    /// Clears all sub-piece progress (e.g. on download completion).
    pub fn clear(&mut self) {
        self.offsets.clear();
    }
}

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── Construction ────────────────────────────────────────────────

    /// Empty resume state starts with zero pieces done.
    ///
    /// All bits must be clear so that no pieces are skipped on first download.
    #[test]
    fn new_starts_empty() {
        let state = ResumeState::new(100, 1_000_000);
        assert_eq!(state.piece_count(), 100);
        assert_eq!(state.file_size(), 1_000_000);
        assert_eq!(state.done_count(), 0);
        for i in 0..100 {
            assert!(!state.is_done(i));
        }
    }

    /// `from_piece_map` captures only Done pieces.
    ///
    /// InFlight and Failed pieces must not be marked as done — they need
    /// to be re-downloaded on resume.
    #[test]
    fn from_piece_map_captures_done_only() {
        let map = SharedPieceMap::new(4);
        map.try_claim(0);
        map.mark_done(0); // Done
        map.try_claim(1);
        map.mark_failed(1); // Failed — should NOT be in resume
                            // Pieces 2, 3: Needed — should NOT be in resume

        let state = ResumeState::from_piece_map(&map, 1024);
        assert_eq!(state.done_count(), 1);
        assert!(state.is_done(0));
        assert!(!state.is_done(1));
        assert!(!state.is_done(2));
        assert!(!state.is_done(3));
    }

    // ── Mark and query ──────────────────────────────────────────────

    /// `mark_done` and `is_done` round-trip correctly.
    ///
    /// Marked pieces must read back as done, unmarked pieces must not.
    #[test]
    fn mark_done_and_is_done_roundtrip() {
        let mut state = ResumeState::new(16, 4096);
        state.mark_done(0);
        state.mark_done(7); // last bit of first byte
        state.mark_done(8); // first bit of second byte
        state.mark_done(15);

        assert!(state.is_done(0));
        assert!(state.is_done(7));
        assert!(state.is_done(8));
        assert!(state.is_done(15));
        assert!(!state.is_done(1));
        assert!(!state.is_done(9));
        assert_eq!(state.done_count(), 4);
    }

    /// `to_verified_pieces` produces correctly-sized vec with correct values.
    #[test]
    fn to_verified_pieces_matches_done_state() {
        let mut state = ResumeState::new(5, 500);
        state.mark_done(1);
        state.mark_done(3);
        let verified = state.to_verified_pieces();
        assert_eq!(verified, vec![false, true, false, true, false]);
    }

    /// Out-of-bounds `mark_done` and `is_done` silently do nothing.
    ///
    /// Prevents panics if a corrupt resume file claims more pieces than exist.
    #[test]
    fn out_of_bounds_access_is_safe() {
        let mut state = ResumeState::new(4, 100);
        state.mark_done(100); // beyond range — no panic
        assert!(!state.is_done(100)); // returns false
        assert_eq!(state.done_count(), 0);
    }

    // ── Save and load ───────────────────────────────────────────────

    /// Save/load round-trip preserves all state.
    ///
    /// The fundamental contract: what you save is what you get back.
    #[test]
    fn save_load_roundtrip() {
        let tmp = std::env::temp_dir().join("p2p-resume-test-roundtrip.resume");
        let _ = std::fs::remove_file(&tmp);

        let mut state = ResumeState::new(20, 5_000_000);
        state.mark_done(0);
        state.mark_done(5);
        state.mark_done(19);
        state.save(&tmp).unwrap();

        let loaded = ResumeState::load(&tmp).unwrap();
        assert_eq!(loaded.piece_count(), 20);
        assert_eq!(loaded.file_size(), 5_000_000);
        assert_eq!(loaded.done_count(), 3);
        assert!(loaded.is_done(0));
        assert!(loaded.is_done(5));
        assert!(loaded.is_done(19));
        assert!(!loaded.is_done(1));

        let _ = std::fs::remove_file(&tmp);
    }

    /// Saving twice produces identical bytes (determinism).
    ///
    /// Resume files must be byte-for-byte reproducible given the same state.
    /// Non-deterministic output would cause spurious diffs in version control.
    #[test]
    fn save_is_deterministic() {
        let path1 = std::env::temp_dir().join("p2p-resume-test-det1.resume");
        let path2 = std::env::temp_dir().join("p2p-resume-test-det2.resume");
        let _ = std::fs::remove_file(&path1);
        let _ = std::fs::remove_file(&path2);

        let mut state = ResumeState::new(10, 1024);
        state.mark_done(3);
        state.mark_done(7);

        state.save(&path1).unwrap();
        state.save(&path2).unwrap();

        let bytes1 = std::fs::read(&path1).unwrap();
        let bytes2 = std::fs::read(&path2).unwrap();
        assert_eq!(bytes1, bytes2);

        let _ = std::fs::remove_file(&path1);
        let _ = std::fs::remove_file(&path2);
    }

    // ── Boundary conditions ─────────────────────────────────────────

    /// Zero-piece torrent produces a valid (header-only) resume file.
    ///
    /// Degenerate case that must not panic or produce invalid output.
    #[test]
    fn zero_pieces_roundtrip() {
        let tmp = std::env::temp_dir().join("p2p-resume-test-zero.resume");
        let _ = std::fs::remove_file(&tmp);

        let state = ResumeState::new(0, 0);
        assert_eq!(state.done_count(), 0);
        state.save(&tmp).unwrap();

        let loaded = ResumeState::load(&tmp).unwrap();
        assert_eq!(loaded.piece_count(), 0);
        assert_eq!(loaded.done_count(), 0);

        let _ = std::fs::remove_file(&tmp);
    }

    /// Single-piece torrent uses one bitfield byte.
    #[test]
    fn single_piece_roundtrip() {
        let tmp = std::env::temp_dir().join("p2p-resume-test-single.resume");
        let _ = std::fs::remove_file(&tmp);

        let mut state = ResumeState::new(1, 256);
        state.mark_done(0);
        state.save(&tmp).unwrap();

        let loaded = ResumeState::load(&tmp).unwrap();
        assert_eq!(loaded.piece_count(), 1);
        assert!(loaded.is_done(0));
        assert_eq!(loaded.done_count(), 1);

        let _ = std::fs::remove_file(&tmp);
    }

    /// Eight pieces (exact byte boundary) round-trips correctly.
    #[test]
    fn eight_pieces_byte_boundary() {
        let tmp = std::env::temp_dir().join("p2p-resume-test-eight.resume");
        let _ = std::fs::remove_file(&tmp);

        let mut state = ResumeState::new(8, 2048);
        for i in 0..8 {
            state.mark_done(i);
        }
        state.save(&tmp).unwrap();

        let loaded = ResumeState::load(&tmp).unwrap();
        assert_eq!(loaded.done_count(), 8);
        for i in 0..8 {
            assert!(loaded.is_done(i));
        }

        let _ = std::fs::remove_file(&tmp);
    }

    /// Nine pieces (one past byte boundary) needs 2 bitfield bytes.
    #[test]
    fn nine_pieces_past_boundary() {
        let tmp = std::env::temp_dir().join("p2p-resume-test-nine.resume");
        let _ = std::fs::remove_file(&tmp);

        let mut state = ResumeState::new(9, 2304);
        state.mark_done(8); // bit 0 of second byte
        state.save(&tmp).unwrap();

        let loaded = ResumeState::load(&tmp).unwrap();
        assert_eq!(loaded.done_count(), 1);
        assert!(!loaded.is_done(0));
        assert!(loaded.is_done(8));

        let _ = std::fs::remove_file(&tmp);
    }

    // ── Error paths ─────────────────────────────────────────────────

    /// Loading a file with wrong magic bytes produces `InvalidMagic`.
    #[test]
    fn load_wrong_magic() {
        let tmp = std::env::temp_dir().join("p2p-resume-test-bad-magic.resume");
        std::fs::write(
            &tmp,
            b"NOPE\x01\x04\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00",
        )
        .unwrap();

        let err = ResumeState::load(&tmp).unwrap_err();
        assert!(
            err.to_string().contains("invalid resume file magic"),
            "unexpected error: {err}"
        );

        let _ = std::fs::remove_file(&tmp);
    }

    /// Loading a file with unsupported version produces `UnsupportedVersion`.
    #[test]
    fn load_wrong_version() {
        let tmp = std::env::temp_dir().join("p2p-resume-test-bad-version.resume");
        let mut data = Vec::new();
        data.extend_from_slice(b"P2PR");
        data.push(0x99); // bad version
        data.extend_from_slice(&4u32.to_le_bytes());
        data.extend_from_slice(&1024u64.to_le_bytes());
        data.push(0x00); // 1 bitfield byte
        std::fs::write(&tmp, &data).unwrap();

        let err = ResumeState::load(&tmp).unwrap_err();
        assert!(
            err.to_string().contains("unsupported resume file version"),
            "unexpected error: {err}"
        );

        let _ = std::fs::remove_file(&tmp);
    }

    /// Loading a truncated file (shorter than header) produces `Truncated`.
    #[test]
    fn load_truncated_header() {
        let tmp = std::env::temp_dir().join("p2p-resume-test-truncated.resume");
        std::fs::write(&tmp, b"P2PR\x01").unwrap(); // only 5 bytes, header needs 17

        let err = ResumeState::load(&tmp).unwrap_err();
        assert!(
            err.to_string().contains("truncated"),
            "unexpected error: {err}"
        );

        let _ = std::fs::remove_file(&tmp);
    }

    /// Loading a file with truncated bitfield produces `Truncated`.
    #[test]
    fn load_truncated_bitfield() {
        let tmp = std::env::temp_dir().join("p2p-resume-test-truncbf.resume");
        let mut data = Vec::new();
        data.extend_from_slice(b"P2PR");
        data.push(VERSION);
        data.extend_from_slice(&16u32.to_le_bytes()); // 16 pieces → 2 bitfield bytes
        data.extend_from_slice(&4096u64.to_le_bytes());
        data.push(0xFF); // only 1 bitfield byte, need 2
        std::fs::write(&tmp, &data).unwrap();

        let err = ResumeState::load(&tmp).unwrap_err();
        assert!(
            err.to_string().contains("truncated"),
            "unexpected error: {err}"
        );

        let _ = std::fs::remove_file(&tmp);
    }

    /// Loading a nonexistent file produces an `Io` error.
    #[test]
    fn load_missing_file() {
        let err = ResumeState::load(Path::new("/nonexistent/path/resume.bin")).unwrap_err();
        assert!(err.to_string().contains("I/O"), "unexpected error: {err}");
    }

    // ── Validation ──────────────────────────────────────────────────

    /// Validate succeeds when parameters match.
    #[test]
    fn validate_matching_params() {
        let state = ResumeState::new(100, 5_000_000);
        assert!(state.validate(100, 5_000_000).is_ok());
    }

    /// Validate rejects mismatched piece count.
    #[test]
    fn validate_piece_count_mismatch() {
        let state = ResumeState::new(100, 5_000_000);
        let err = state.validate(200, 5_000_000).unwrap_err();
        assert!(
            err.to_string().contains("piece count mismatch"),
            "unexpected error: {err}"
        );
    }

    /// Validate rejects mismatched file size.
    #[test]
    fn validate_file_size_mismatch() {
        let state = ResumeState::new(100, 5_000_000);
        let err = state.validate(100, 9_999_999).unwrap_err();
        assert!(
            err.to_string().contains("file size mismatch"),
            "unexpected error: {err}"
        );
    }

    // ── Display messages ────────────────────────────────────────────

    /// Error Display messages include key context values.
    #[test]
    fn error_display_messages_have_context() {
        let e = ResumeError::InvalidMagic {
            found: [0x00, 0x01, 0x02, 0x03],
        };
        assert!(e.to_string().contains("P2PR"));

        let e = ResumeError::UnsupportedVersion { version: 42 };
        assert!(e.to_string().contains("42"));

        let e = ResumeError::PieceCountMismatch {
            expected: 100,
            found: 200,
        };
        let msg = e.to_string();
        assert!(msg.contains("100") && msg.contains("200"));

        let e = ResumeError::FileSizeMismatch {
            expected: 5_000_000,
            found: 9_999_999,
        };
        let msg = e.to_string();
        assert!(msg.contains("5000000") && msg.contains("9999999"));
    }

    // ── Bitfield helpers ────────────────────────────────────────────

    /// `packed_byte_count` computes correct sizes at boundaries.
    #[test]
    fn packed_byte_count_boundaries() {
        assert_eq!(packed_byte_count(0), 0);
        assert_eq!(packed_byte_count(1), 1);
        assert_eq!(packed_byte_count(7), 1);
        assert_eq!(packed_byte_count(8), 1);
        assert_eq!(packed_byte_count(9), 2);
        assert_eq!(packed_byte_count(16), 2);
        assert_eq!(packed_byte_count(17), 3);
    }

    // ── SubPieceProgress (DCC RESUME pattern) ───────────────────────

    /// New sub-piece progress starts empty.
    #[test]
    fn sub_piece_progress_starts_empty() {
        let progress = SubPieceProgress::new();
        assert!(progress.is_empty());
        assert_eq!(progress.in_progress_count(), 0);
        assert_eq!(progress.bytes_received(0), 0);
    }

    /// Update and query byte offset for a piece.
    ///
    /// After receiving partial data, the offset should reflect how much
    /// was downloaded so the coordinator can resume from that point.
    #[test]
    fn sub_piece_update_and_query() {
        let mut progress = SubPieceProgress::new();
        progress.update(5, 131_072); // 128 KiB received
        assert_eq!(progress.bytes_received(5), 131_072);
        assert_eq!(progress.in_progress_count(), 1);
    }

    /// Completing a piece removes its sub-piece tracking.
    ///
    /// Once a piece is fully downloaded and verified, there's no need
    /// to track partial progress — it's in the bitfield.
    #[test]
    fn sub_piece_complete_removes_tracking() {
        let mut progress = SubPieceProgress::new();
        progress.update(3, 50_000);
        progress.complete(3);
        assert_eq!(progress.bytes_received(3), 0);
        assert!(progress.is_empty());
    }

    /// Reset sets a piece back to offset 0.
    ///
    /// Used when a piece fails verification and must be re-downloaded
    /// from scratch.
    #[test]
    fn sub_piece_reset_clears_offset() {
        let mut progress = SubPieceProgress::new();
        progress.update(7, 200_000);
        progress.reset(7);
        assert_eq!(progress.bytes_received(7), 0);
    }

    /// Multiple pieces are tracked independently.
    #[test]
    fn sub_piece_multiple_pieces() {
        let mut progress = SubPieceProgress::new();
        progress.update(0, 100);
        progress.update(1, 200);
        progress.update(2, 300);
        assert_eq!(progress.in_progress_count(), 3);
        assert_eq!(progress.bytes_received(1), 200);

        progress.complete(1);
        assert_eq!(progress.in_progress_count(), 2);
    }

    /// Clear removes all tracking.
    #[test]
    fn sub_piece_clear() {
        let mut progress = SubPieceProgress::new();
        progress.update(0, 100);
        progress.update(1, 200);
        progress.clear();
        assert!(progress.is_empty());
    }

    /// Iterator yields all tracked pieces.
    #[test]
    fn sub_piece_iterator() {
        let mut progress = SubPieceProgress::new();
        progress.update(10, 1000);
        progress.update(20, 2000);
        let mut items: Vec<_> = progress.iter().collect();
        items.sort_by_key(|&(idx, _)| idx);
        assert_eq!(items, vec![(10, 1000), (20, 2000)]);
    }
}
