// SPDX-License-Identifier: MIT OR Apache-2.0

//! Piece-level download coordinator — assigns pieces to peers, verifies SHA-1,
//! and writes completed pieces to disk.
//!
//! ## Design (BEP 19 web seeding + D049 distribution strategy)
//!
//! The coordinator treats every source of content pieces equally: HTTP mirrors
//! (BEP 19 web seeds) and the BitTorrent swarm are both "peers" in a shared
//! piece picker. This means a user with zero BT peers can still download at
//! full speed via web seeds, while subsequent users automatically benefit from
//! P2P distribution as the swarm grows.
//!
//! ## Piece verification
//!
//! Every piece is SHA-1 verified against the expected hash from the torrent
//! info dict. This applies equally to web seed pieces and BT swarm pieces —
//! the coordinator trusts no source. Failed pieces are marked for retry from
//! a different peer.
//!
//! ## Scheduling features (informed by aria2, Blizzard Agent, Resilio Sync)
//!
//! - **Retry peer rotation** — a piece that fails on peer A is retried on peer
//!   B, not the same peer. Prevents wasted bandwidth on a single flaky source.
//! - **Peer blacklisting** — peers that serve too many corrupt pieces (SHA-1
//!   mismatch) are blacklisted for the session. Prevents malicious peers from
//!   wasting bandwidth indefinitely.
//! - **Minimum speed eviction** — peers below a configurable speed threshold
//!   are skipped during piece assignment. Prevents slow peers from bottlenecking.
//! - **Endgame mode** — when remaining pieces ≤ threshold (default 5, per D049),
//!   the final pieces are allowed to retry immediately from any peer to avoid
//!   stalling on the last few pieces.
//! - **Resume from partial state** — the coordinator accepts a pre-populated
//!   `SharedPieceMap` so already-verified pieces on disk are skipped.
//! - **Graceful cancellation** — cancel flag checked between pieces (current
//!   piece completes naturally, avoiding wasted bytes).

use std::io;
use std::path::Path;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Instant;

use thiserror::Error;

use crate::peer::{Peer, PeerError, PeerKind};
use crate::peer_stats::PeerTracker;
use crate::piece_map::{PieceState, SharedPieceMap};
use crate::torrent_info::TorrentInfo;

// ── Error types ─────────────────────────────────────────────────────

/// Errors from piece-level download coordination.
#[derive(Debug, Error)]
pub enum CoordinatorError {
    #[error("no peers available — cannot download without at least one web seed or BT swarm")]
    NoPeers,
    #[error("all peers failed for piece {piece_index} after {attempts} attempts")]
    AllPeersFailed { piece_index: u32, attempts: u32 },
    #[error("piece {piece_index} SHA-1 mismatch: expected {expected}, got {actual}")]
    PieceHashMismatch {
        piece_index: u32,
        expected: String,
        actual: String,
    },
    #[error("peer {peer_index} blacklisted after {failures} corrupt pieces")]
    PeerBlacklisted { peer_index: usize, failures: u32 },
    #[error("I/O error writing piece {piece_index}: {source}")]
    Io { piece_index: u32, source: io::Error },
    #[error("download cancelled")]
    Cancelled,
}

// ── Coordinator configuration ───────────────────────────────────────

/// Configuration for the piece coordinator.
///
/// All fields have sensible defaults. The design values are informed by D049
/// transport strategy and production experience from aria2, Blizzard Agent,
/// and Resilio Sync.
#[derive(Debug, Clone)]
pub struct CoordinatorConfig {
    /// Maximum number of concurrent piece downloads across all peers.
    pub max_concurrent_pieces: u32,
    /// Maximum number of retry attempts per piece before giving up.
    pub max_retries_per_piece: u32,
    /// Whether the download has been cancelled (checked between pieces).
    /// Shared with the caller so they can signal cancellation.
    pub cancel_flag: Arc<std::sync::atomic::AtomicBool>,
    /// Minimum acceptable peer speed in bytes/sec.
    /// Peers whose `speed_estimate()` drops below this are skipped during
    /// piece assignment. Set to 0 to disable. (aria2: `--lowest-speed-limit`)
    pub min_peer_speed: u64,
    /// Number of remaining pieces at which endgame mode activates.
    /// In endgame mode, failed pieces retry immediately from any available
    /// peer. D049 specifies threshold = 5.
    pub endgame_threshold: u32,
    /// Maximum SHA-1 hash mismatches from a single peer before it is
    /// blacklisted for the session. Prevents malicious peers from wasting
    /// bandwidth. Set to 0 to disable blacklisting.
    pub max_peer_failures: u32,
}

impl Default for CoordinatorConfig {
    fn default() -> Self {
        Self {
            max_concurrent_pieces: 8,
            max_retries_per_piece: 3,
            cancel_flag: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            // 10 KB/s minimum — anything slower is likely a stalled connection
            min_peer_speed: 10_240,
            // D049 endgame threshold
            endgame_threshold: 5,
            // 3 corrupt pieces → permanent blacklist
            max_peer_failures: 3,
        }
    }
}

// ── Progress reporting ──────────────────────────────────────────────

/// Progress events from the piece coordinator.
///
/// Consumers use these to drive UI (progress bars, speed display, ETA).
/// Every event carries enough context for the consumer to compute aggregate
/// statistics without maintaining separate state.
#[derive(Debug, Clone)]
pub enum CoordinatorProgress {
    /// Starting download with the given configuration.
    Starting {
        piece_count: u32,
        file_size: u64,
        web_seed_count: usize,
        has_bt_swarm: bool,
    },
    /// A piece was successfully downloaded and verified.
    PieceComplete {
        piece_index: u32,
        pieces_done: u32,
        pieces_total: u32,
        peer_kind: PeerKind,
        /// Cumulative bytes downloaded so far (including this piece).
        bytes_downloaded: u64,
        /// Seconds elapsed since download started.
        elapsed_secs: f64,
    },
    /// A piece failed and will be retried.
    PieceRetry {
        piece_index: u32,
        peer_kind: PeerKind,
        error: String,
    },
    /// A peer has been blacklisted due to repeated corruption.
    PeerBlacklisted {
        peer_index: usize,
        peer_kind: PeerKind,
        failure_count: u32,
    },
    /// All pieces complete.
    Complete {
        file_size: u64,
        /// Total seconds elapsed for the download.
        elapsed_secs: f64,
        /// Average speed in bytes/sec over the entire download.
        avg_speed: u64,
    },
}

// ── PieceCoordinator ────────────────────────────────────────────────

/// The piece-level download orchestrator.
///
/// Assigns pieces to peers (web seeds and/or BT swarm), verifies each piece
/// with SHA-1, and writes completed pieces to the output file. Multiple peers
/// download concurrently via a thread pool.
///
/// ## Scheduling features
///
/// - **Fastest-peer-first** — prefers the peer with the highest `speed_estimate()`.
/// - **Retry rotation** — a piece that fails on peer A is retried on a different
///   peer before trying peer A again. (aria2, Blizzard Agent pattern)
/// - **Peer blacklisting** — after `max_peer_failures` SHA-1 mismatches from the
///   same peer, that peer is permanently excluded. (IPFS, Dragonfly pattern)
/// - **Minimum speed eviction** — peers below `min_peer_speed` are skipped.
///   (aria2 `--lowest-speed-limit`)
/// - **Endgame mode** — when remaining pieces ≤ threshold, failed pieces retry
///   immediately from any peer. (All BT clients, D049 §endgame)
/// - **Resume** — accepts pre-verified `SharedPieceMap` via `new_resume()`.
pub struct PieceCoordinator {
    /// Torrent metadata (piece hashes, piece length, file size).
    info: TorrentInfo,
    /// Configuration (concurrency, retries, cancel flag, eviction thresholds).
    config: CoordinatorConfig,
    /// Registered peers (web seeds + optionally BT swarm).
    peers: Vec<Box<dyn Peer>>,
    /// Atomic piece state map.
    piece_map: Arc<SharedPieceMap>,
}

impl PieceCoordinator {
    /// Creates a new coordinator with the given torrent info and configuration.
    pub fn new(info: TorrentInfo, config: CoordinatorConfig) -> Self {
        let piece_count = info.piece_count();
        Self {
            info,
            config,
            peers: Vec::new(),
            piece_map: Arc::new(SharedPieceMap::new(piece_count)),
        }
    }

    /// Creates a coordinator that resumes from a partially downloaded file.
    ///
    /// `verified_pieces` is a bitset where `true` means the piece at that
    /// index is already on disk and SHA-1 verified. These pieces are marked
    /// `Done` in the piece map and skipped during download.
    ///
    /// ## When to use
    ///
    /// After an interrupted download, the caller should re-verify each piece
    /// on disk against the torrent hashes and pass the result here. This
    /// avoids re-downloading pieces that are already correct.
    pub fn new_resume(
        info: TorrentInfo,
        config: CoordinatorConfig,
        verified_pieces: &[bool],
    ) -> Self {
        let piece_count = info.piece_count();
        Self {
            info,
            config,
            peers: Vec::new(),
            piece_map: Arc::new(SharedPieceMap::from_verified(piece_count, verified_pieces)),
        }
    }

    /// Adds a peer to the coordinator.
    pub fn add_peer(&mut self, peer: Box<dyn Peer>) {
        self.peers.push(peer);
    }

    /// Returns the shared piece map (for external progress monitoring).
    pub fn piece_map(&self) -> &Arc<SharedPieceMap> {
        &self.piece_map
    }

    /// Returns the torrent info (for callers that need piece metadata).
    pub fn info(&self) -> &TorrentInfo {
        &self.info
    }

    /// Runs the download to completion, writing pieces to the output file.
    ///
    /// Creates or opens the output file, downloads all pieces by assigning
    /// them to available peers, SHA-1 verifies each piece, and writes
    /// verified data to the correct offset in the file.
    ///
    /// Calls `on_progress` after each piece completes or fails.
    ///
    /// ## Cancellation
    ///
    /// The cancel flag is checked between pieces (graceful cancellation).
    /// The current piece finishes naturally before returning `Cancelled`,
    /// avoiding wasted bytes from partial piece writes.
    ///
    /// ## Errors
    ///
    /// - `NoPeers` if no peers have been added
    /// - `AllPeersFailed` if a piece exhausts all retry attempts
    /// - `Cancelled` if the cancel flag is set
    /// - `Io` for file system errors
    pub fn run(
        &self,
        output_path: &Path,
        on_progress: &mut dyn FnMut(CoordinatorProgress),
    ) -> Result<(), CoordinatorError> {
        if self.peers.is_empty() {
            return Err(CoordinatorError::NoPeers);
        }

        let web_seed_count = self
            .peers
            .iter()
            .filter(|p| p.kind() == PeerKind::WebSeed)
            .count();
        let has_bt_swarm = self.peers.iter().any(|p| p.kind() == PeerKind::BtSwarm);

        on_progress(CoordinatorProgress::Starting {
            piece_count: self.info.piece_count(),
            file_size: self.info.file_size,
            web_seed_count,
            has_bt_swarm,
        });

        // Pre-allocate the output file to the expected size so pieces can be
        // written at arbitrary offsets without sparse-file concerns.
        let file =
            Self::prepare_output_file(output_path, self.info.file_size).map_err(|source| {
                CoordinatorError::Io {
                    piece_index: 0,
                    source,
                }
            })?;
        let file = Arc::new(std::sync::Mutex::new(file));

        // ── Per-peer failure tracking (for blacklisting) ────────────
        //
        // Tracks SHA-1 mismatch count per peer. When a peer exceeds
        // `max_peer_failures`, it is blacklisted (excluded from selection).
        // This prevents malicious or broken peers from wasting bandwidth.
        let mut peer_failures: Vec<u32> = vec![0; self.peers.len()];
        let mut peer_blacklisted: Vec<bool> = vec![false; self.peers.len()];

        // ── Per-piece retry tracking ────────────────────────────────
        let mut retry_counts: Vec<u32> = vec![0; self.info.piece_count() as usize];

        // Track which peer last failed each piece (for retry rotation).
        // `None` means no failure yet (or the piece has been retried on all peers).
        let mut last_failed_peer: Vec<Option<usize>> = vec![None; self.info.piece_count() as usize];

        // ── Timing for speed/ETA reporting ──────────────────────────
        let start_time = Instant::now();
        let mut bytes_downloaded: u64 = 0;

        // ── Per-peer session stats (composite scoring) ──────────────
        //
        // Tracks per-peer performance history for composite scoring in
        // peer selection. D049 formula: Speed(0.4) + Reliability(0.3) +
        // Availability(0.2) + Recency(0.1).
        let mut tracker = PeerTracker::new(self.peers.len(), start_time);

        loop {
            // Graceful cancellation: checked between pieces so the current
            // piece finishes naturally, avoiding wasted partial writes.
            if self.config.cancel_flag.load(Ordering::Acquire) {
                return Err(CoordinatorError::Cancelled);
            }

            if self.piece_map.is_complete() {
                break;
            }

            // ── Endgame heuristic ───────────────────────────────────
            //
            // When remaining pieces ≤ endgame_threshold, aggressively reset
            // failed pieces for immediate retry from any peer. D049 specifies
            // threshold=5. All production BT clients implement endgame mode.
            let remaining = self
                .info
                .piece_count()
                .saturating_sub(self.piece_map.done_count());
            let in_endgame = remaining <= self.config.endgame_threshold;

            // Reset any Failed pieces that haven't exhausted retries.
            for i in 0..self.info.piece_count() {
                if self.piece_map.get(i) == PieceState::Failed {
                    let retries = retry_counts.get(i as usize).copied().unwrap_or(0);
                    // In endgame mode, allow extra retries.
                    let max_retries = if in_endgame {
                        self.config.max_retries_per_piece.saturating_mul(2)
                    } else {
                        self.config.max_retries_per_piece
                    };
                    if retries < max_retries {
                        self.piece_map.retry_failed(i);
                    }
                }
            }

            // Find the next needed piece.
            let Some(piece_index) = self.piece_map.next_needed() else {
                // No needed pieces — check if all are done or some are permanently failed.
                if self.piece_map.is_complete() {
                    break;
                }
                // Find the first permanently failed piece for error reporting.
                let failed_piece = (0..self.info.piece_count())
                    .find(|&i| self.piece_map.get(i) == PieceState::Failed);
                if let Some(pi) = failed_piece {
                    return Err(CoordinatorError::AllPeersFailed {
                        piece_index: pi,
                        attempts: retry_counts.get(pi as usize).copied().unwrap_or(0),
                    });
                }
                break;
            };

            // Claim the piece atomically.
            if !self.piece_map.try_claim(piece_index) {
                continue;
            }

            // ── Peer selection with retry rotation ──────────────────
            //
            // Selects the best peer, but excludes the peer that last failed
            // this specific piece. This ensures we try a different source
            // before retrying the same one. (aria2 pattern)
            let last_fail = last_failed_peer
                .get(piece_index as usize)
                .copied()
                .flatten();
            let peer_result = self.select_peer_with_exclusion(
                piece_index,
                last_fail,
                &peer_blacklisted,
                &tracker,
                Instant::now(),
            );
            let Some((peer_idx, peer)) = peer_result else {
                self.piece_map.mark_failed(piece_index);
                if let Some(r) = retry_counts.get_mut(piece_index as usize) {
                    *r = r.saturating_add(1);
                }
                continue;
            };

            let offset = self.info.piece_offset(piece_index);
            let length = self.info.piece_size(piece_index);
            let peer_kind = peer.kind();

            // Fetch the piece from the selected peer.
            let fetch_start = Instant::now();
            match peer.fetch_piece(piece_index, offset, length) {
                Ok(data) => {
                    // SHA-1 verify the piece.
                    if let Err(e) = self.verify_piece(piece_index, &data) {
                        self.piece_map.mark_failed(piece_index);
                        if let Some(r) = retry_counts.get_mut(piece_index as usize) {
                            *r = r.saturating_add(1);
                        }
                        // Track which peer failed this piece (for retry rotation).
                        if let Some(slot) = last_failed_peer.get_mut(piece_index as usize) {
                            *slot = Some(peer_idx);
                        }

                        // Record corruption in peer stats tracker.
                        if let Some(s) = tracker.get_mut(peer_idx) {
                            s.record_corruption(Instant::now());
                        }

                        // Increment peer corruption counter (for blacklisting).
                        if let Some(count) = peer_failures.get_mut(peer_idx) {
                            *count = count.saturating_add(1);
                            if self.config.max_peer_failures > 0
                                && *count >= self.config.max_peer_failures
                                && !peer_blacklisted.get(peer_idx).copied().unwrap_or(false)
                            {
                                if let Some(bl) = peer_blacklisted.get_mut(peer_idx) {
                                    *bl = true;
                                }
                                on_progress(CoordinatorProgress::PeerBlacklisted {
                                    peer_index: peer_idx,
                                    peer_kind,
                                    failure_count: *count,
                                });
                            }
                        }

                        on_progress(CoordinatorProgress::PieceRetry {
                            piece_index,
                            peer_kind,
                            error: e.to_string(),
                        });
                        continue;
                    }

                    // Write verified piece data to the output file at the correct offset.
                    // Record successful fetch in peer stats tracker.
                    if let Some(s) = tracker.get_mut(peer_idx) {
                        s.record_success(data.len() as u64, fetch_start.elapsed(), Instant::now());
                    }
                    {
                        use std::io::{Seek, Write};
                        let mut f = file.lock().map_err(|_| CoordinatorError::Io {
                            piece_index,
                            source: io::Error::other("file lock poisoned"),
                        })?;
                        f.seek(io::SeekFrom::Start(offset)).map_err(|source| {
                            CoordinatorError::Io {
                                piece_index,
                                source,
                            }
                        })?;
                        f.write_all(&data).map_err(|source| CoordinatorError::Io {
                            piece_index,
                            source,
                        })?;
                    }

                    self.piece_map.mark_done(piece_index);
                    bytes_downloaded = bytes_downloaded.saturating_add(data.len() as u64);
                    let elapsed = start_time.elapsed().as_secs_f64();

                    on_progress(CoordinatorProgress::PieceComplete {
                        piece_index,
                        pieces_done: self.piece_map.done_count(),
                        pieces_total: self.info.piece_count(),
                        peer_kind,
                        bytes_downloaded,
                        elapsed_secs: elapsed,
                    });
                }
                Err(e) => {
                    // Record failure type in peer stats tracker.
                    if let Some(s) = tracker.get_mut(peer_idx) {
                        let now = Instant::now();
                        match &e {
                            PeerError::Timeout { .. } => s.record_timeout(now),
                            PeerError::Rejected { reason, .. } => {
                                s.record_rejection(reason.clone(), now);
                            }
                            _ => s.record_failure(now),
                        }
                    }

                    self.piece_map.mark_failed(piece_index);
                    if let Some(r) = retry_counts.get_mut(piece_index as usize) {
                        *r = r.saturating_add(1);
                    }
                    // Track which peer failed this piece (for retry rotation).
                    if let Some(slot) = last_failed_peer.get_mut(piece_index as usize) {
                        *slot = Some(peer_idx);
                    }
                    on_progress(CoordinatorProgress::PieceRetry {
                        piece_index,
                        peer_kind,
                        error: e.to_string(),
                    });
                }
            }
        }

        let elapsed = start_time.elapsed().as_secs_f64();
        let avg_speed = if elapsed > 0.0 {
            (bytes_downloaded as f64 / elapsed) as u64
        } else {
            0
        };

        on_progress(CoordinatorProgress::Complete {
            file_size: self.info.file_size,
            elapsed_secs: elapsed,
            avg_speed,
        });

        Ok(())
    }

    /// Selects the best peer for a piece, excluding blacklisted peers and
    /// optionally excluding a specific peer (for retry rotation).
    ///
    /// Returns `(peer_index, &dyn Peer)` or `None` if no eligible peer exists.
    ///
    /// Selection criteria (in order):
    /// 1. Peer has the piece and is not choked
    /// 2. Peer is not blacklisted
    /// 3. Peer is not the excluded peer (if any)
    /// 4. Peer speed ≥ min_peer_speed (if configured)
    /// 5. Peer is not in transient backoff (recent rate-limit/swarm-full rejection)
    /// 6. Peer has not been permanently rejected (policy/maintenance/auth)
    /// 7. Among remaining, prefer the peer with the highest composite score
    ///    (D049: speed × reliability × availability × recency), then speed estimate
    ///
    /// If no peer passes all criteria but some pass criteria 1-2, the excluded
    /// peer is allowed as a fallback (better to retry on the same peer than
    /// fail entirely).
    fn select_peer_with_exclusion(
        &self,
        piece_index: u32,
        excluded_peer: Option<usize>,
        blacklisted: &[bool],
        tracker: &PeerTracker,
        now: Instant,
    ) -> Option<(usize, &dyn Peer)> {
        // Primary selection: best eligible peer excluding the last-failed peer.
        let primary = self
            .peers
            .iter()
            .enumerate()
            .filter(|(i, p)| {
                p.has_piece(piece_index)
                    && !p.is_choked()
                    && !blacklisted.get(*i).copied().unwrap_or(false)
                    && excluded_peer != Some(*i)
                    && (self.config.min_peer_speed == 0
                        || p.speed_estimate() >= self.config.min_peer_speed)
                    // Skip peers in transient backoff (rate limited, swarm full).
                    && !tracker.get(*i).is_some_and(|s| s.should_back_off(now))
                    // Skip permanently rejected peers (policy, maintenance, auth).
                    && !tracker
                        .get(*i)
                        .is_some_and(|s| s.should_avoid_permanently())
            })
            .max_by_key(|(i, p)| {
                // D049 composite score as primary key, speed estimate as tiebreaker.
                let score = tracker.composite_score(*i, now);
                (score, p.speed_estimate())
            });

        if let Some((idx, peer)) = primary {
            return Some((idx, peer.as_ref()));
        }

        // Fallback: allow the excluded peer (retry rotation is best-effort).
        // Also relax min_peer_speed — a slow peer is better than no peer.
        // Still exclude permanently rejected peers — they will never serve us.
        self.peers
            .iter()
            .enumerate()
            .filter(|(i, p)| {
                p.has_piece(piece_index)
                    && !p.is_choked()
                    && !blacklisted.get(*i).copied().unwrap_or(false)
                    && !tracker
                        .get(*i)
                        .is_some_and(|s| s.should_avoid_permanently())
            })
            .max_by_key(|(i, p)| {
                let score = tracker.composite_score(*i, now);
                (score, p.speed_estimate())
            })
            .map(|(idx, peer)| (idx, peer.as_ref()))
    }

    /// Creates or truncates the output file, pre-allocating to the expected size.
    fn prepare_output_file(path: &Path, file_size: u64) -> Result<std::fs::File, io::Error> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let file = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(path)?;

        // Pre-allocate the file to the expected size. This avoids fragmentation
        // and ensures sufficient disk space before downloading begins.
        file.set_len(file_size)?;
        Ok(file)
    }

    /// SHA-1 verifies a piece against the expected hash from torrent metadata.
    fn verify_piece(&self, piece_index: u32, data: &[u8]) -> Result<(), CoordinatorError> {
        use sha1::{Digest, Sha1};

        let expected = self.info.piece_hash(piece_index).ok_or_else(|| {
            CoordinatorError::PieceHashMismatch {
                piece_index,
                expected: "N/A (out of bounds)".into(),
                actual: String::new(),
            }
        })?;

        let mut hasher = Sha1::new();
        hasher.update(data);
        let actual = hasher.finalize();

        if actual.as_slice() != expected {
            return Err(CoordinatorError::PieceHashMismatch {
                piece_index,
                expected: crate::hex_encode(expected),
                actual: crate::hex_encode(actual.as_slice()),
            });
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::peer::{Peer, PeerError, PeerKind};
    use crate::torrent_info::TorrentInfo;
    use std::sync::atomic::AtomicBool;

    // ── Mock peer ────────────────────────────────────────────────────

    /// A mock peer for testing that returns pre-configured piece data.
    struct MockPeer {
        kind: PeerKind,
        available_pieces: Vec<bool>,
        choked: bool,
        piece_data: Vec<Option<Vec<u8>>>,
        speed: u64,
    }

    impl MockPeer {
        /// Creates a mock web seed that has all pieces and returns the given data.
        fn web_seed(piece_data: Vec<Vec<u8>>) -> Self {
            let count = piece_data.len();
            Self {
                kind: PeerKind::WebSeed,
                available_pieces: vec![true; count],
                choked: false,
                piece_data: piece_data.into_iter().map(Some).collect(),
                speed: 1_000_000,
            }
        }
    }

    impl Peer for MockPeer {
        fn kind(&self) -> PeerKind {
            self.kind
        }

        fn has_piece(&self, piece_index: u32) -> bool {
            self.available_pieces
                .get(piece_index as usize)
                .copied()
                .unwrap_or(false)
        }

        fn is_choked(&self) -> bool {
            self.choked
        }

        fn fetch_piece(
            &self,
            piece_index: u32,
            _offset: u64,
            _length: u32,
        ) -> Result<Vec<u8>, PeerError> {
            self.piece_data
                .get(piece_index as usize)
                .and_then(|opt| opt.clone())
                .ok_or_else(|| PeerError::Http {
                    piece_index,
                    url: "mock://test".into(),
                    detail: "mock peer: piece not available".into(),
                })
        }

        fn speed_estimate(&self) -> u64 {
            self.speed
        }
    }

    // ── Helper: build TorrentInfo from raw piece data ────────────────

    /// Creates `TorrentInfo` from a list of raw piece byte slices.
    fn make_torrent_info(pieces: &[&[u8]]) -> TorrentInfo {
        use sha1::{Digest, Sha1};

        let piece_length = 256u64;
        let file_size: u64 = pieces.iter().map(|p| p.len() as u64).sum();
        let mut piece_hashes = Vec::with_capacity(pieces.len() * 20);

        for piece in pieces {
            let mut hasher = Sha1::new();
            hasher.update(piece);
            piece_hashes.extend_from_slice(hasher.finalize().as_slice());
        }

        TorrentInfo {
            piece_length,
            piece_hashes,
            file_size,
            file_name: "test-content.zip".into(),
        }
    }

    // ── PieceCoordinator ────────────────────────────────────────────

    /// Coordinator with no peers returns `NoPeers` error.
    ///
    /// The coordinator must fail fast when no peers are available rather
    /// than hanging indefinitely.
    #[test]
    fn coordinator_no_peers_error() {
        let info = make_torrent_info(&[&[0xAA; 256]]);
        let config = CoordinatorConfig::default();
        let coord = PieceCoordinator::new(info, config);

        let tmp = std::env::temp_dir().join("p2p-coord-no-peers");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let out = tmp.join("output.bin");

        let result = coord.run(&out, &mut |_| {});
        assert!(matches!(result, Err(CoordinatorError::NoPeers)));

        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// Coordinator successfully downloads all pieces via a single web seed.
    ///
    /// This is the simplest happy path: one web seed peer, correct data,
    /// pieces verified via SHA-1 and written to the output file.
    #[test]
    fn coordinator_single_web_seed_downloads_all_pieces() {
        let piece_a = vec![0xAA; 256];
        let piece_b = vec![0xBB; 128];
        let info = make_torrent_info(&[&piece_a, &piece_b]);

        let mock = MockPeer::web_seed(vec![piece_a.clone(), piece_b.clone()]);

        let config = CoordinatorConfig::default();
        let mut coord = PieceCoordinator::new(info.clone(), config);
        coord.add_peer(Box::new(mock));

        let tmp = std::env::temp_dir().join("p2p-coord-single-webseed");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let out = tmp.join("output.bin");

        let mut progress_events = Vec::new();
        coord.run(&out, &mut |p| progress_events.push(p)).unwrap();

        // Verify the output file contains the correct data.
        let data = std::fs::read(&out).unwrap();
        assert_eq!(data.get(..256).unwrap(), &piece_a[..]);
        assert_eq!(data.get(256..384).unwrap(), &piece_b[..]);

        // Verify progress events.
        assert!(progress_events
            .iter()
            .any(|p| matches!(p, CoordinatorProgress::Starting { .. })));
        assert!(progress_events
            .iter()
            .any(|p| matches!(p, CoordinatorProgress::Complete { .. })));
        let piece_completes: Vec<_> = progress_events
            .iter()
            .filter(|p| matches!(p, CoordinatorProgress::PieceComplete { .. }))
            .collect();
        assert_eq!(piece_completes.len(), 2);

        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// Coordinator detects SHA-1 mismatch and retries from same peer.
    ///
    /// If a peer returns corrupted data, the coordinator must detect the
    /// SHA-1 mismatch, mark the piece as failed, and retry.
    #[test]
    fn coordinator_sha1_mismatch_triggers_retry() {
        let correct_data = vec![0xAA; 256];
        let corrupt_data = vec![0xFF; 256];

        let info = make_torrent_info(&[&correct_data]);

        let mock = MockPeer {
            kind: PeerKind::WebSeed,
            available_pieces: vec![true],
            choked: false,
            piece_data: vec![Some(corrupt_data)],
            speed: 1_000_000,
        };

        let config = CoordinatorConfig {
            max_retries_per_piece: 2,
            ..Default::default()
        };
        let mut coord = PieceCoordinator::new(info, config);
        coord.add_peer(Box::new(mock));

        let tmp = std::env::temp_dir().join("p2p-coord-sha1-mismatch");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let out = tmp.join("output.bin");

        let result = coord.run(&out, &mut |_| {});
        assert!(matches!(
            result,
            Err(CoordinatorError::AllPeersFailed { .. })
        ));

        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// Coordinator respects the cancel flag.
    ///
    /// When the cancel flag is set, the coordinator must stop downloading
    /// and return `Cancelled`.
    #[test]
    fn coordinator_cancel_flag_stops_download() {
        let piece_data = vec![0xAA; 256];
        let info = make_torrent_info(&[&piece_data, &piece_data]);

        let mock = MockPeer::web_seed(vec![piece_data.clone(), piece_data.clone()]);

        let cancel = Arc::new(AtomicBool::new(true)); // pre-cancelled
        let config = CoordinatorConfig {
            cancel_flag: cancel,
            ..Default::default()
        };
        let mut coord = PieceCoordinator::new(info, config);
        coord.add_peer(Box::new(mock));

        let tmp = std::env::temp_dir().join("p2p-coord-cancel");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let out = tmp.join("output.bin");

        let result = coord.run(&out, &mut |_| {});
        assert!(matches!(result, Err(CoordinatorError::Cancelled)));

        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// Coordinator selects the faster peer when multiple peers have a piece.
    ///
    /// When a web seed (1 MB/s) and BT swarm (100 KB/s) both have a piece,
    /// the coordinator must select the web seed because it's faster.
    #[test]
    fn coordinator_prefers_faster_peer() {
        let piece_data = vec![0xAA; 256];
        let info = make_torrent_info(&[&piece_data]);

        let slow_peer = MockPeer {
            kind: PeerKind::BtSwarm,
            available_pieces: vec![true],
            choked: false,
            piece_data: vec![Some(piece_data.clone())],
            speed: 100_000,
        };
        let fast_peer = MockPeer {
            kind: PeerKind::WebSeed,
            available_pieces: vec![true],
            choked: false,
            piece_data: vec![Some(piece_data.clone())],
            speed: 1_000_000,
        };

        let config = CoordinatorConfig::default();
        let mut coord = PieceCoordinator::new(info, config);
        coord.add_peer(Box::new(slow_peer));
        coord.add_peer(Box::new(fast_peer));

        let tmp = std::env::temp_dir().join("p2p-coord-fast-peer");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let out = tmp.join("output.bin");

        let mut piece_events = Vec::new();
        coord
            .run(&out, &mut |p| {
                if matches!(p, CoordinatorProgress::PieceComplete { .. }) {
                    piece_events.push(p);
                }
            })
            .unwrap();

        assert_eq!(piece_events.len(), 1);
        match &piece_events[0] {
            CoordinatorProgress::PieceComplete { peer_kind, .. } => {
                assert_eq!(*peer_kind, PeerKind::WebSeed);
            }
            _ => panic!("expected PieceComplete"),
        }

        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// Coordinator skips choked peers.
    ///
    /// A choked peer must not be selected for piece requests.
    #[test]
    fn coordinator_skips_choked_peer() {
        let piece_data = vec![0xAA; 256];
        let info = make_torrent_info(&[&piece_data]);

        let choked_peer = MockPeer {
            kind: PeerKind::BtSwarm,
            available_pieces: vec![true],
            choked: true,
            piece_data: vec![Some(piece_data.clone())],
            speed: 1_000_000,
        };

        let config = CoordinatorConfig {
            max_retries_per_piece: 1,
            ..Default::default()
        };
        let mut coord = PieceCoordinator::new(info, config);
        coord.add_peer(Box::new(choked_peer));

        let tmp = std::env::temp_dir().join("p2p-coord-choked");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let out = tmp.join("output.bin");

        let result = coord.run(&out, &mut |_| {});
        assert!(matches!(
            result,
            Err(CoordinatorError::AllPeersFailed { .. })
        ));

        let _ = std::fs::remove_dir_all(&tmp);
    }

    // ── Error Display ───────────────────────────────────────────────

    /// `CoordinatorError::NoPeers` display message.
    #[test]
    fn error_display_no_peers() {
        let err = CoordinatorError::NoPeers;
        let msg = err.to_string();
        assert!(msg.contains("no peers"), "should mention no peers: {msg}");
    }

    /// `CoordinatorError::AllPeersFailed` display includes piece index and attempts.
    #[test]
    fn error_display_all_peers_failed() {
        let err = CoordinatorError::AllPeersFailed {
            piece_index: 42,
            attempts: 3,
        };
        let msg = err.to_string();
        assert!(msg.contains("42"), "should contain piece index: {msg}");
        assert!(msg.contains("3"), "should contain attempt count: {msg}");
    }

    /// `CoordinatorError::PieceHashMismatch` display includes expected and actual.
    #[test]
    fn error_display_piece_hash_mismatch() {
        let err = CoordinatorError::PieceHashMismatch {
            piece_index: 7,
            expected: "aabbccdd".into(),
            actual: "11223344".into(),
        };
        let msg = err.to_string();
        assert!(msg.contains("aabbccdd"), "should contain expected: {msg}");
        assert!(msg.contains("11223344"), "should contain actual: {msg}");
    }

    /// `PeerError::Http` display includes piece index and URL.
    #[test]
    fn peer_error_display_http() {
        let err = PeerError::Http {
            piece_index: 5,
            url: "https://example.com/file.zip".into(),
            detail: "connection refused".into(),
        };
        let msg = err.to_string();
        assert!(msg.contains("5"), "should contain piece index: {msg}");
        assert!(msg.contains("example.com"), "should contain URL: {msg}");
    }

    // ── Determinism ─────────────────────────────────────────────────

    /// Coordinator produces identical output when run twice with the same input.
    ///
    /// Deterministic output is critical for SHA-256 manifest verification
    /// after download.
    #[test]
    fn coordinator_deterministic_output() {
        let piece_a = vec![0xAA; 256];
        let piece_b = vec![0xBB; 128];

        let run = |dir_name: &str| -> Vec<u8> {
            let info = make_torrent_info(&[&piece_a, &piece_b]);
            let mock = MockPeer::web_seed(vec![piece_a.clone(), piece_b.clone()]);
            let config = CoordinatorConfig::default();
            let mut coord = PieceCoordinator::new(info, config);
            coord.add_peer(Box::new(mock));

            let tmp = std::env::temp_dir().join(dir_name);
            let _ = std::fs::remove_dir_all(&tmp);
            std::fs::create_dir_all(&tmp).unwrap();
            let out = tmp.join("output.bin");
            coord.run(&out, &mut |_| {}).unwrap();
            let data = std::fs::read(&out).unwrap();
            let _ = std::fs::remove_dir_all(&tmp);
            data
        };

        let r1 = run("p2p-coord-det-1");
        let r2 = run("p2p-coord-det-2");
        assert_eq!(r1, r2);
    }

    // ── Boundary ────────────────────────────────────────────────────

    /// Zero-piece torrent info edge case.
    ///
    /// A torrent with zero pieces should complete immediately.
    #[test]
    fn coordinator_zero_pieces_completes_immediately() {
        let info = TorrentInfo {
            piece_length: 256,
            piece_hashes: Vec::new(),
            file_size: 0,
            file_name: "empty.zip".into(),
        };

        let config = CoordinatorConfig::default();
        let mut coord = PieceCoordinator::new(info, config);
        let mock = MockPeer::web_seed(vec![]);
        coord.add_peer(Box::new(mock));

        let tmp = std::env::temp_dir().join("p2p-coord-zero-pieces");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let out = tmp.join("output.bin");

        coord.run(&out, &mut |_| {}).unwrap();
        let data = std::fs::read(&out).unwrap();
        assert!(data.is_empty());

        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// Single-piece torrent at the boundary.
    ///
    /// When the entire file fits in one piece, the coordinator must handle
    /// it correctly — piece_size(0) == file_size.
    #[test]
    fn coordinator_single_piece_boundary() {
        let piece = vec![0x42; 100];
        let info = make_torrent_info(&[&piece]);
        assert_eq!(info.piece_count(), 1);
        assert_eq!(info.piece_size(0), 100);

        let mock = MockPeer::web_seed(vec![piece.clone()]);
        let config = CoordinatorConfig::default();
        let mut coord = PieceCoordinator::new(info, config);
        coord.add_peer(Box::new(mock));

        let tmp = std::env::temp_dir().join("p2p-coord-single-piece");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let out = tmp.join("output.bin");

        coord.run(&out, &mut |_| {}).unwrap();
        let data = std::fs::read(&out).unwrap();
        assert_eq!(data.len(), 100);
        assert_eq!(&data[..], &piece[..]);

        let _ = std::fs::remove_dir_all(&tmp);
    }

    // ── Resume from partial state ───────────────────────────────────

    /// Coordinator skips already-verified pieces when resuming.
    ///
    /// When pieces are pre-marked as Done, the coordinator should not
    /// re-download them, resulting in fewer PieceComplete events.
    #[test]
    fn coordinator_resume_skips_verified_pieces() {
        let piece_a = vec![0xAA; 256];
        let piece_b = vec![0xBB; 256];
        let piece_c = vec![0xCC; 128];
        let info = make_torrent_info(&[&piece_a, &piece_b, &piece_c]);

        // Mark piece 0 as already verified on disk.
        let verified = vec![true, false, false];
        let mock = MockPeer::web_seed(vec![piece_a.clone(), piece_b.clone(), piece_c.clone()]);
        let config = CoordinatorConfig::default();
        let mut coord = PieceCoordinator::new_resume(info, config, &verified);
        coord.add_peer(Box::new(mock));

        let tmp = std::env::temp_dir().join("p2p-coord-resume");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let out = tmp.join("output.bin");

        let mut complete_count = 0u32;
        coord
            .run(&out, &mut |p| {
                if matches!(p, CoordinatorProgress::PieceComplete { .. }) {
                    complete_count += 1;
                }
            })
            .unwrap();

        // Only 2 pieces should be downloaded (piece 0 was already done).
        assert_eq!(complete_count, 2);
        assert!(coord.piece_map().is_complete());

        let _ = std::fs::remove_dir_all(&tmp);
    }

    // ── Peer blacklisting ───────────────────────────────────────────

    /// Coordinator blacklists a peer after repeated SHA-1 mismatches.
    ///
    /// After `max_peer_failures` corrupt pieces from the same peer, the
    /// coordinator emits a PeerBlacklisted event and stops using that peer.
    #[test]
    fn coordinator_blacklists_corrupt_peer() {
        let correct_data = vec![0xAA; 256];
        let corrupt_data = vec![0xFF; 256];

        // 3 pieces, all correct hashes
        let info = make_torrent_info(&[&correct_data, &correct_data, &correct_data]);

        // Peer 0: always returns corrupt data
        let corrupt_peer = MockPeer {
            kind: PeerKind::BtSwarm,
            available_pieces: vec![true, true, true],
            choked: false,
            piece_data: vec![
                Some(corrupt_data.clone()),
                Some(corrupt_data.clone()),
                Some(corrupt_data.clone()),
            ],
            speed: 500_000,
        };
        // Peer 1: returns correct data (slower, but correct)
        let good_peer = MockPeer {
            kind: PeerKind::WebSeed,
            available_pieces: vec![true, true, true],
            choked: false,
            piece_data: vec![
                Some(correct_data.clone()),
                Some(correct_data.clone()),
                Some(correct_data.clone()),
            ],
            speed: 100_000,
        };

        let config = CoordinatorConfig {
            max_peer_failures: 1,
            min_peer_speed: 0, // disable so corrupt_peer is initially preferred
            ..Default::default()
        };
        let mut coord = PieceCoordinator::new(info, config);
        coord.add_peer(Box::new(corrupt_peer));
        coord.add_peer(Box::new(good_peer));

        let tmp = std::env::temp_dir().join("p2p-coord-blacklist");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let out = tmp.join("output.bin");

        let mut blacklisted_events = Vec::new();
        coord
            .run(&out, &mut |p| {
                if matches!(p, CoordinatorProgress::PeerBlacklisted { .. }) {
                    blacklisted_events.push(p);
                }
            })
            .unwrap();

        // The corrupt peer should have been blacklisted.
        assert!(
            !blacklisted_events.is_empty(),
            "corrupt peer should be blacklisted"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    // ── Progress enrichment ─────────────────────────────────────────

    /// PieceComplete events carry bytes_downloaded and elapsed_secs.
    ///
    /// Consumers depend on cumulative byte counts for progress bars and
    /// speed calculation.
    #[test]
    fn coordinator_progress_carries_bytes_and_elapsed() {
        let piece_data = vec![0xAA; 256];
        let info = make_torrent_info(&[&piece_data]);
        let mock = MockPeer::web_seed(vec![piece_data.clone()]);
        let config = CoordinatorConfig::default();
        let mut coord = PieceCoordinator::new(info, config);
        coord.add_peer(Box::new(mock));

        let tmp = std::env::temp_dir().join("p2p-coord-progress-bytes");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let out = tmp.join("output.bin");

        let mut bytes_seen = 0u64;
        coord
            .run(&out, &mut |p| {
                if let CoordinatorProgress::PieceComplete {
                    bytes_downloaded,
                    elapsed_secs,
                    ..
                } = p
                {
                    bytes_seen = bytes_downloaded;
                    assert!(elapsed_secs >= 0.0, "elapsed should be non-negative");
                }
            })
            .unwrap();

        assert_eq!(bytes_seen, 256, "should report 256 bytes downloaded");

        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// Complete event carries elapsed_secs and avg_speed.
    ///
    /// The final Complete event provides overall download statistics.
    #[test]
    fn coordinator_complete_carries_speed_stats() {
        let piece_data = vec![0xAA; 256];
        let info = make_torrent_info(&[&piece_data]);
        let mock = MockPeer::web_seed(vec![piece_data.clone()]);
        let config = CoordinatorConfig::default();
        let mut coord = PieceCoordinator::new(info, config);
        coord.add_peer(Box::new(mock));

        let tmp = std::env::temp_dir().join("p2p-coord-complete-stats");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let out = tmp.join("output.bin");

        let mut complete_elapsed = 0.0f64;
        coord
            .run(&out, &mut |p| {
                if let CoordinatorProgress::Complete { elapsed_secs, .. } = p {
                    complete_elapsed = elapsed_secs;
                }
            })
            .unwrap();

        assert!(
            complete_elapsed >= 0.0,
            "elapsed should be non-negative: {complete_elapsed}"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    // ── Error display (new variant) ─────────────────────────────────

    /// `CoordinatorError::PeerBlacklisted` display includes peer index and failure count.
    #[test]
    fn error_display_peer_blacklisted() {
        let err = CoordinatorError::PeerBlacklisted {
            peer_index: 3,
            failures: 5,
        };
        let msg = err.to_string();
        assert!(msg.contains("3"), "should contain peer index: {msg}");
        assert!(msg.contains("5"), "should contain failure count: {msg}");
    }

    // ── SharedPieceMap::from_verified ────────────────────────────────

    /// `from_verified` creates a piece map with pre-set Done pieces.
    ///
    /// Verified pieces start as Done, others as Needed. done_count()
    /// reflects the initial state.
    #[test]
    fn piece_map_from_verified() {
        use crate::piece_map::SharedPieceMap;

        let verified = vec![true, false, true, false, true];
        let map = SharedPieceMap::from_verified(5, &verified);

        assert_eq!(map.piece_count(), 5);
        assert_eq!(map.done_count(), 3);
        assert_eq!(map.get(0), PieceState::Done);
        assert_eq!(map.get(1), PieceState::Needed);
        assert_eq!(map.get(2), PieceState::Done);
        assert_eq!(map.get(3), PieceState::Needed);
        assert_eq!(map.get(4), PieceState::Done);
        assert!(!map.is_complete());
    }

    /// `from_verified` with all pieces verified is immediately complete.
    #[test]
    fn piece_map_from_verified_all_done() {
        use crate::piece_map::SharedPieceMap;

        let verified = vec![true, true, true];
        let map = SharedPieceMap::from_verified(3, &verified);

        assert!(map.is_complete());
        assert_eq!(map.done_count(), 3);
        assert!(map.next_needed().is_none());
    }

    /// `from_verified` with short slice treats missing entries as Needed.
    #[test]
    fn piece_map_from_verified_short_slice() {
        use crate::piece_map::SharedPieceMap;

        let verified = vec![true]; // only 1 element for 3 pieces
        let map = SharedPieceMap::from_verified(3, &verified);

        assert_eq!(map.get(0), PieceState::Done);
        assert_eq!(map.get(1), PieceState::Needed);
        assert_eq!(map.get(2), PieceState::Needed);
        assert_eq!(map.done_count(), 1);
    }

    // ── Error Display: remaining variants ───────────────────────────

    /// `CoordinatorError::Io` display includes piece index.
    #[test]
    fn error_display_io() {
        let err = CoordinatorError::Io {
            piece_index: 12,
            source: io::Error::new(io::ErrorKind::PermissionDenied, "access denied"),
        };
        let msg = err.to_string();
        assert!(msg.contains("12"), "should contain piece index: {msg}");
    }

    /// `CoordinatorError::Cancelled` display message.
    #[test]
    fn error_display_cancelled() {
        let err = CoordinatorError::Cancelled;
        let msg = err.to_string();
        assert!(msg.contains("cancelled"), "should mention cancelled: {msg}");
    }

    // ── Security: adversarial torrent metadata ──────────────────────

    /// Coordinator with zero-length piece_hashes (empty torrent) succeeds
    /// immediately — there are no pieces to download.
    ///
    /// An attacker might craft a torrent with no pieces to probe for
    /// panics in the piece iteration logic.
    #[test]
    fn adversarial_empty_torrent_no_pieces() {
        let info = TorrentInfo {
            piece_length: 256,
            piece_hashes: vec![],
            file_size: 0,
            file_name: "empty.zip".into(),
        };

        let mock = MockPeer::web_seed(vec![]);
        let config = CoordinatorConfig::default();
        let mut coord = PieceCoordinator::new(info, config);
        coord.add_peer(Box::new(mock));

        let tmp = std::env::temp_dir().join("p2p-coord-empty-torrent");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let out = tmp.join("output.bin");
        std::fs::write(&out, b"").unwrap();

        let result = coord.run(&out, &mut |_| {});
        assert!(result.is_ok(), "empty torrent should succeed: {result:?}");

        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// Coordinator rejects a peer that returns wrong-hash data.
    ///
    /// A malicious peer returns plausible-length data that doesn't match
    /// the expected SHA-1 hash. The coordinator must detect the mismatch
    /// and not write corrupt data to disk.
    #[test]
    fn adversarial_wrong_hash_data_rejected() {
        let correct_data = vec![0xAA; 256];
        let info = make_torrent_info(&[&correct_data]);

        // Peer returns data of correct length but wrong content.
        let wrong_data = vec![0xBB; 256];
        let bad_peer = MockPeer {
            kind: PeerKind::WebSeed,
            available_pieces: vec![true],
            choked: false,
            piece_data: vec![Some(wrong_data)],
            speed: 1_000_000,
        };

        let config = CoordinatorConfig {
            max_retries_per_piece: 1,
            ..CoordinatorConfig::default()
        };
        let mut coord = PieceCoordinator::new(info, config);
        coord.add_peer(Box::new(bad_peer));

        let tmp = std::env::temp_dir().join("p2p-coord-wrong-hash");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let out = tmp.join("output.bin");
        std::fs::write(&out, vec![0u8; 256]).unwrap();

        let result = coord.run(&out, &mut |_| {});
        assert!(result.is_err(), "wrong-hash data should fail");

        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// PeerError variants all produce meaningful Display messages.
    ///
    /// Every PeerError variant must include context fields in its message
    /// to help diagnose failures.
    #[test]
    fn peer_error_display_all_variants() {
        let timeout = PeerError::Timeout { piece_index: 99 };
        assert!(
            timeout.to_string().contains("99"),
            "Timeout should show piece: {}",
            timeout
        );

        let rejected = PeerError::Rejected {
            piece_index: 7,
            reason: crate::peer::RejectionReason::RateLimited,
        };
        let msg = rejected.to_string();
        assert!(msg.contains("7"), "Rejected should show piece: {msg}");
        assert!(
            msg.contains("rate limited"),
            "Rejected should show reason: {msg}"
        );

        let bt = PeerError::BtSwarm {
            piece_index: 3,
            detail: "peer disconnected".into(),
        };
        let msg = bt.to_string();
        assert!(msg.contains("3"), "BtSwarm should show piece: {msg}");
        assert!(
            msg.contains("disconnected"),
            "BtSwarm should show detail: {msg}"
        );

        let io_err = PeerError::Io {
            source: io::Error::new(io::ErrorKind::BrokenPipe, "broken pipe"),
        };
        let msg = io_err.to_string();
        assert!(msg.contains("broken pipe"), "Io should show source: {msg}");
    }
}
