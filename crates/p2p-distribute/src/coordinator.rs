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
use std::sync::{Arc, Mutex};
use std::time::Instant;

use thiserror::Error;

use crate::corruption_ledger::{Attribution, CorruptionLedger};
use crate::network_id::NetworkId;
use crate::peer::{Peer, PeerError, PeerKind};
use crate::peer_id::PeerId;
use crate::peer_stats::PeerTracker;
use crate::pex::{PexEntry, PexFlags, PexMessage, MAX_PEX_ADDED};
use crate::phi_detector::{PhiDetector, SUSPECT_PHI_THRESHOLD};
use crate::piece_data_cache::PieceDataCache;
use crate::piece_map::{PieceState, SharedPieceMap};
use crate::priority::PiecePriorityMap;
use crate::reader::StreamNotifier;
use crate::resume::ResumeState;
use crate::selection::select_next_piece;
use crate::storage::{FileStorageFactory, StorageError, StorageFactory};
use crate::streaming::ByteRange;
use crate::torrent_info::TorrentInfo;

// ── Download mode ───────────────────────────────────────────────────

/// Download strategy: bulk (maximum throughput) or streaming (playback-optimised).
///
/// ## Design rationale
///
/// Bulk and streaming downloads have fundamentally different performance
/// profiles. Forcing both through the same code path means one must
/// compromise:
///
/// - **Bulk downloads** want maximum aggregate throughput. Piece order
///   doesn't matter — sequential scan is fastest for web seeds, and
///   rarest-first is optimal for swarm health with BT peers. No
///   per-piece callback overhead, no priority map computation.
///
/// - **Streaming downloads** want the *right* pieces *now*. The playhead
///   piece is life-or-death for stall-free playback. Container metadata
///   pieces (moov, idx1, SeekHead) must arrive early for seeking. All
///   other pieces are background fill. Priority-weighted rarest-first
///   selection is mandatory, and a `StreamNotifier` must wake the reader
///   after each piece.
///
/// Separating these into explicit modes eliminates conditional branches
/// in the hot path and makes the performance contract clear to callers.
#[derive(Default)]
pub enum DownloadMode {
    /// Maximum throughput, no streaming overhead.
    ///
    /// Piece selection: sequential scan (optimal for web-seed-only swarms
    /// where all peers have all pieces, resulting in sequential disk I/O).
    /// No priority map, no stream notifications, no per-piece callbacks
    /// beyond progress reporting.
    #[default]
    Bulk,

    /// Streaming playback: priority-weighted piece selection + reader notification.
    ///
    /// Piece selection: rarest-first weighted by [`PiecePriorityMap`] so
    /// playhead and metadata pieces always win. After each verified piece,
    /// the [`StreamNotifier`] wakes the blocking reader.
    ///
    /// The caller is responsible for periodically calling
    /// [`PiecePriorityMap::update()`] as the playhead advances.
    Streaming {
        /// Per-piece priority map (playhead, prebuffer, metadata, background).
        /// Must be behind `Arc<Mutex<_>>` so the streaming reader can update
        /// priorities while the coordinator reads them.
        priority_map: Arc<Mutex<PiecePriorityMap>>,
        /// Notifier that wakes the [`StreamingReader`](crate::reader::StreamingReader)
        /// when pieces complete.
        notifier: StreamNotifier,
    },
}

impl std::fmt::Debug for DownloadMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Bulk => f.write_str("Bulk"),
            Self::Streaming { .. } => f.write_str("Streaming { .. }"),
        }
    }
}

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
    #[error("storage error for piece {piece_index}: {source}")]
    Storage {
        piece_index: u32,
        source: StorageError,
    },
    #[error("download cancelled")]
    Cancelled,
}

// ── Coordinator configuration ───────────────────────────────────────

/// Configuration for the piece coordinator.
///
/// All fields have sensible defaults. The design values are informed by D049
/// transport strategy and production experience from aria2, Blizzard Agent,
/// and Resilio Sync.
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
    /// Path for persistent resume state (FlashGet `.jcd` pattern).
    ///
    /// When set, the coordinator saves a checkpoint file after each verified
    /// piece and loads it on startup. This enables crash recovery without
    /// re-downloading already-verified pieces. The resume file is deleted
    /// on successful completion.
    pub resume_path: Option<std::path::PathBuf>,
    /// Custom storage factory for piece data.
    ///
    /// When `None`, the coordinator uses [`FileStorageFactory`] (direct file
    /// I/O with pre-allocation). Consumers can provide custom implementations
    /// for in-memory storage, write coalescing, or database backends.
    pub storage_factory: Option<Box<dyn StorageFactory>>,
    /// Download strategy: bulk throughput or streaming playback.
    ///
    /// - [`DownloadMode::Bulk`] (default) — sequential piece selection,
    ///   no priority computation, no stream notification overhead.
    /// - [`DownloadMode::Streaming`] — priority-weighted rarest-first
    ///   selection, notifies the streaming reader after each piece.
    pub download_mode: DownloadMode,
    /// Optional LRU piece data cache for reducing disk I/O during seeding.
    ///
    /// When set, the coordinator inserts verified piece bytes immediately
    /// after writing to storage (hot-on-arrival pattern). The future upload
    /// handler checks this cache before falling back to disk reads, avoiding
    /// redundant I/O for hot pieces requested by multiple peers.
    ///
    /// libtorrent uses the same pattern (default 16 MB). For C&C disc ISOs
    /// (500–700 MB), 32 MB covers the rarest-first convergence window.
    /// Set to `None` to disable caching (download-only, no seeding).
    pub piece_data_cache: Option<Arc<PieceDataCache>>,
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
            resume_path: None,
            storage_factory: None,
            download_mode: DownloadMode::Bulk,
            piece_data_cache: None,
        }
    }
}

impl std::fmt::Debug for CoordinatorConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CoordinatorConfig")
            .field("max_concurrent_pieces", &self.max_concurrent_pieces)
            .field("max_retries_per_piece", &self.max_retries_per_piece)
            .field("cancel_flag", &self.cancel_flag)
            .field("min_peer_speed", &self.min_peer_speed)
            .field("endgame_threshold", &self.endgame_threshold)
            .field("max_peer_failures", &self.max_peer_failures)
            .field("resume_path", &self.resume_path)
            .field(
                "storage_factory",
                &self.storage_factory.as_ref().map(|_| ".."),
            )
            .field("download_mode", &self.download_mode)
            .field(
                "piece_data_cache",
                &self.piece_data_cache.as_ref().map(|c| c.stats()),
            )
            .finish()
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
    /// Download resumed from a checkpoint file — some pieces were already done.
    ///
    /// Fired after `Starting` when a valid resume file is found. The
    /// coordinator has marked the resumed pieces as `Done` in the piece map
    /// and will skip their download.
    Resumed {
        pieces_resumed: u32,
        pieces_total: u32,
        /// Approximate bytes already on disk from the previous session.
        bytes_resumed: u64,
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
    /// Periodic availability snapshot for cooperative downloading (Syncthing
    /// `DownloadProgress` pattern).
    ///
    /// Emitted every N pieces (or at the caller's request) so that external
    /// systems (swarm peers, UI) know which pieces are available locally.
    /// This enables cooperative downloading: other peers can avoid requesting
    /// pieces we already have.
    AvailabilityUpdate {
        /// Bitset of completed pieces (index = piece number).
        completed_pieces: Vec<bool>,
        /// Total pieces in the torrent.
        piece_count: u32,
        /// Current download speed in bytes/sec (EWMA).
        download_speed: u64,
    },
}

// ── Coordinator shared mutable state ────────────────────────────────

/// Mutable per-peer and per-piece tracking state for the coordinator.
///
/// Wrapped in a single `Mutex` during concurrent piece fetching. The lock
/// is held only for O(peers) bookkeeping operations (microseconds), never
/// during network I/O (the expensive part). A single Mutex avoids the
/// deadlock risk of multiple fine-grained locks while keeping contention
/// negligible relative to network latency.
struct CoordinatorMutableState {
    /// SHA-1 mismatch count per peer (for blacklisting).
    peer_failures: Vec<u32>,
    /// Whether each peer is permanently blacklisted.
    peer_blacklisted: Vec<bool>,
    /// Number of retry attempts per piece.
    retry_counts: Vec<u32>,
    /// Which peer last failed each piece (for retry rotation).
    last_failed_peer: Vec<Option<usize>>,
    /// Cumulative bytes downloaded across all peers.
    bytes_downloaded: u64,
    /// Per-peer composite scoring stats (D049 formula).
    tracker: PeerTracker,
    /// Per-peer phi accrual failure detectors (Cassandra pattern).
    phi_detectors: Vec<PhiDetector>,
    /// Per-piece byte-range attribution (aMule CorruptionBlackBox).
    corruption_ledger: CorruptionLedger,
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

    /// Builds a PEX (Peer Exchange) message advertising the coordinator's
    /// known peers.
    ///
    /// The message includes only peers that have a stable identity
    /// ([`Peer::peer_id`] returns `Some`). Anonymous peers are excluded
    /// because PEX entries require a [`PeerId`] for meaningful gossip.
    ///
    /// Returns `None` if no identifiable peers exist. The caller is
    /// responsible for sending the message at the BEP-11 recommended
    /// interval ([`pex::PEX_INTERVAL_SECS`]).
    pub fn build_pex_message(&self, network_id: NetworkId) -> Option<PexMessage> {
        if self.peers.is_empty() {
            return None;
        }

        let mut msg = PexMessage::new(network_id);

        for peer in &self.peers {
            if msg.added.len() >= MAX_PEX_ADDED {
                break;
            }

            // Only include peers with a stable identity.
            let Some(id_str) = peer.peer_id() else {
                continue;
            };

            let flags = PexFlags {
                seed: peer.has_piece(0),
                encryption: false,
                utp: false,
                utp_holepunch: false,
                connectable: peer.kind() == PeerKind::WebSeed,
            };

            msg.added.push(PexEntry {
                peer_id: PeerId::from_key_material(id_str.as_bytes()),
                addr: id_str.to_string(),
                flags,
            });
        }

        if msg.is_empty() {
            None
        } else {
            Some(msg)
        }
    }

    /// Runs the download to completion, writing pieces to the output file.
    ///
    /// Downloads all pieces by assigning them to available peers, SHA-1
    /// verifies each piece, and writes verified data to the correct offset.
    /// When `max_concurrent_pieces > 1`, multiple pieces are fetched in
    /// parallel using scoped threads — each thread claims pieces via the
    /// atomic [`SharedPieceMap`], fetches from the selected peer (network
    /// I/O runs outside any lock), then briefly locks shared state for
    /// bookkeeping.
    ///
    /// Calls `on_progress` after each piece completes or fails.
    ///
    /// ## Cancellation
    ///
    /// The cancel flag is checked between pieces (graceful cancellation).
    /// In-flight pieces finish naturally before returning `Cancelled`.
    ///
    /// ## Errors
    ///
    /// - `NoPeers` if no peers have been added
    /// - `AllPeersFailed` if a piece exhausts all retry attempts
    /// - `Cancelled` if the cancel flag is set
    /// - `Io` / `Storage` for file system errors
    pub fn run(
        &self,
        output_path: &Path,
        on_progress: &mut (dyn FnMut(CoordinatorProgress) + Send),
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

        // ── Resume from checkpoint (FlashGet .jcd pattern) ──────────
        let resuming = self.try_load_resume(on_progress);

        // ── Storage ─────────────────────────────────────────────────
        let factory: &dyn StorageFactory = match self.config.storage_factory {
            Some(ref f) => f.as_ref(),
            None => &FileStorageFactory,
        };
        let storage = factory
            .create_storage(output_path, self.info.file_size, resuming)
            .map_err(|source| CoordinatorError::Storage {
                piece_index: 0,
                source,
            })?;

        let start_time = Instant::now();

        // ── Shared mutable state (Mutex-protected) ──────────────────
        //
        // All per-peer and per-piece tracking is bundled into a single
        // struct behind one Mutex. The lock is held only for bookkeeping
        // (microseconds), never during network I/O (the slow part).
        // This enables concurrent piece fetching with minimal contention.
        let shared = Mutex::new(CoordinatorMutableState {
            peer_failures: vec![0; self.peers.len()],
            peer_blacklisted: vec![false; self.peers.len()],
            retry_counts: vec![0; self.info.piece_count() as usize],
            last_failed_peer: vec![None; self.info.piece_count() as usize],
            bytes_downloaded: 0,
            tracker: PeerTracker::new(self.peers.len(), start_time),
            phi_detectors: (0..self.peers.len())
                .map(|_| PhiDetector::new(start_time))
                .collect(),
            corruption_ledger: CorruptionLedger::new(),
        });

        // Progress callback must also be serialized across threads.
        let on_progress = Mutex::new(on_progress);

        // ── Determine concurrency level ─────────────────────────────
        //
        // Use min(max_concurrent_pieces, peer_count) worker threads.
        // A single peer can only serve one piece at a time, so there's no
        // benefit to more threads than peers.
        let concurrency = (self.config.max_concurrent_pieces as usize)
            .min(self.peers.len())
            .max(1);

        // ── Worker loop: claim → select → fetch → verify → write ────
        //
        // Each worker thread independently finds pieces to download. The
        // atomic SharedPieceMap ensures no two threads claim the same piece.
        // Peer selection happens under the shared Mutex (brief), then the
        // network fetch runs without any lock held (the expensive part).
        //
        // When a global error is detected (storage failure, permanent piece
        // failure), it's stored in this Arc and the cancel flag is set so
        // all workers drain gracefully.
        let fatal_error: Arc<Mutex<Option<CoordinatorError>>> = Arc::new(Mutex::new(None));

        std::thread::scope(|scope| {
            for _worker_id in 0..concurrency {
                let shared = &shared;
                let storage = &storage;
                let on_progress = &on_progress;
                let fatal_error = Arc::clone(&fatal_error);

                scope.spawn(move || {
                    loop {
                        // ── Check termination conditions ────────────
                        if self.config.cancel_flag.load(Ordering::Acquire) {
                            break;
                        }
                        if self.piece_map.is_complete() {
                            break;
                        }
                        if fatal_error.lock().ok().is_some_and(|e| e.is_some()) {
                            break;
                        }

                        // ── Endgame: reset failed pieces for retry ──
                        // Brief lock to read retry_counts for endgame reset.
                        {
                            let state = shared.lock().unwrap_or_else(|e| e.into_inner());
                            let remaining = self
                                .info
                                .piece_count()
                                .saturating_sub(self.piece_map.done_count());
                            let in_endgame = remaining <= self.config.endgame_threshold;

                            for i in 0..self.info.piece_count() {
                                if self.piece_map.get(i) == PieceState::Failed {
                                    let retries =
                                        state.retry_counts.get(i as usize).copied().unwrap_or(0);
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
                        }

                        // ── Claim the next needed piece (lock-free) ─
                        let Some(piece_index) = self.claim_next_piece() else {
                            // No pieces available right now. If all are Done,
                            // or InFlight by other threads, break or yield.
                            if self.piece_map.is_complete() {
                                break;
                            }
                            // Check if any pieces are still InFlight (other
                            // threads working) or Needed. If everything is
                            // either Done or permanently Failed (not Needed,
                            // not InFlight), we're stuck — break and let the
                            // post-loop error check report AllPeersFailed.
                            let any_in_progress = (0..self.info.piece_count()).any(|i| {
                                let s = self.piece_map.get(i);
                                s == PieceState::Needed || s == PieceState::InFlight
                            });
                            if !any_in_progress {
                                break;
                            }
                            // Other threads are working on the remaining
                            // pieces. Briefly yield and retry.
                            std::thread::yield_now();
                            continue;
                        };

                        // ── Select peer (brief lock) ────────────────
                        let (peer_idx, peer_kind) = {
                            let state = shared.lock().unwrap_or_else(|e| e.into_inner());
                            let last_fail = state
                                .last_failed_peer
                                .get(piece_index as usize)
                                .copied()
                                .flatten();
                            match self.select_peer_with_exclusion(
                                piece_index,
                                last_fail,
                                &state.peer_blacklisted,
                                &state.tracker,
                                &state.phi_detectors,
                                Instant::now(),
                            ) {
                                Some((idx, peer)) => (idx, peer.kind()),
                                None => {
                                    // No eligible peer — mark piece failed.
                                    self.piece_map.mark_failed(piece_index);
                                    drop(state);
                                    let mut s = shared.lock().unwrap_or_else(|e| e.into_inner());
                                    if let Some(r) = s.retry_counts.get_mut(piece_index as usize) {
                                        *r = r.saturating_add(1);
                                    }
                                    continue;
                                }
                            }
                        };

                        // ── Fetch piece (NO lock held — network I/O) ─
                        let offset = self.info.piece_offset(piece_index);
                        let length = self.info.piece_size(piece_index);
                        let peer = self
                            .peers
                            .get(peer_idx)
                            .map(|p| p.as_ref())
                            .expect("peer_idx from select must be valid");
                        let fetch_start = Instant::now();
                        let fetch_result = peer.fetch_piece(piece_index, offset, length);

                        // ── Process result (brief lock) ─────────────
                        match fetch_result {
                            Ok(data) => {
                                // SHA-1 verify (no lock needed — pure computation).
                                if let Err(e) = self.verify_piece(piece_index, &data) {
                                    self.piece_map.mark_failed(piece_index);
                                    let mut state =
                                        shared.lock().unwrap_or_else(|e| e.into_inner());
                                    self.handle_hash_failure(
                                        piece_index,
                                        peer_idx,
                                        peer_kind,
                                        length,
                                        &e,
                                        &mut state,
                                        on_progress,
                                    );
                                    continue;
                                }

                                // Write verified piece (PieceStorage is Send+Sync).
                                if let Err(source) = storage.write_piece(piece_index, offset, &data)
                                {
                                    // Storage error is fatal — signal all workers.
                                    self.config.cancel_flag.store(true, Ordering::Release);
                                    if let Ok(mut fe) = fatal_error.lock() {
                                        *fe = Some(CoordinatorError::Storage {
                                            piece_index,
                                            source,
                                        });
                                    }
                                    break;
                                }

                                self.piece_map.mark_done(piece_index);

                                // Hot-on-arrival: cache verified piece bytes
                                // so the upload path can serve them from RAM.
                                // Freshly downloaded pieces are the hottest in
                                // the swarm (rarest-first causes many peers to
                                // request the same piece). Inserting here avoids
                                // a wasteful read-back when seeding starts.
                                // The cache is Arc<PieceDataCache> (Send+Sync),
                                // so this is safe from any worker thread.
                                if let Some(ref cache) = self.config.piece_data_cache {
                                    cache.insert(piece_index, data.clone());
                                }

                                // Streaming mode: notify the reader that a
                                // new byte range is available. This wakes any
                                // blocking StreamingReader::read() call that
                                // is waiting on this piece. Done outside the
                                // Mutex — StreamNotifier uses its own condvar.
                                if let DownloadMode::Streaming { ref notifier, .. } =
                                    self.config.download_mode
                                {
                                    notifier.piece_completed(ByteRange {
                                        start: offset,
                                        end: offset.saturating_add(data.len() as u64),
                                    });
                                }

                                // Update tracking state (brief lock).
                                let mut state = shared.lock().unwrap_or_else(|e| e.into_inner());
                                self.handle_piece_success(
                                    piece_index,
                                    peer_idx,
                                    peer_kind,
                                    &data,
                                    fetch_start,
                                    start_time,
                                    &mut state,
                                    on_progress,
                                );
                            }
                            Err(e) => {
                                self.piece_map.mark_failed(piece_index);
                                let mut state = shared.lock().unwrap_or_else(|e| e.into_inner());
                                self.handle_fetch_failure(
                                    piece_index,
                                    peer_idx,
                                    peer_kind,
                                    &e,
                                    &mut state,
                                    on_progress,
                                );
                            }
                        }
                    }
                });
            }
        });

        // ── Check for fatal errors from workers ─────────────────────
        if let Some(err) = fatal_error.lock().ok().and_then(|mut e| e.take()) {
            return Err(err);
        }

        // Cancellation check after all workers have exited.
        if self.config.cancel_flag.load(Ordering::Acquire) {
            return Err(CoordinatorError::Cancelled);
        }

        // Check if we actually completed or if all pieces failed.
        if !self.piece_map.is_complete() {
            let state = shared.lock().unwrap_or_else(|e| e.into_inner());
            let failed_piece =
                (0..self.info.piece_count()).find(|&i| self.piece_map.get(i) == PieceState::Failed);
            if let Some(pi) = failed_piece {
                return Err(CoordinatorError::AllPeersFailed {
                    piece_index: pi,
                    attempts: state.retry_counts.get(pi as usize).copied().unwrap_or(0),
                });
            }
        }

        let state = shared.lock().unwrap_or_else(|e| e.into_inner());
        let elapsed = start_time.elapsed().as_secs_f64();
        let avg_speed = if elapsed > 0.0 {
            (state.bytes_downloaded as f64 / elapsed) as u64
        } else {
            0
        };
        drop(state);

        // Flush storage to ensure all piece data is durable.
        storage
            .flush()
            .map_err(|source| CoordinatorError::Storage {
                piece_index: self.info.piece_count().saturating_sub(1),
                source,
            })?;

        if let Ok(mut cb) = on_progress.lock() {
            cb(CoordinatorProgress::Complete {
                file_size: self.info.file_size,
                elapsed_secs: elapsed,
                avg_speed,
            });
        }

        // Delete resume file — download is done.
        if let Some(ref resume_path) = self.config.resume_path {
            let _ = std::fs::remove_file(resume_path);
        }

        Ok(())
    }

    /// Claims the next needed piece by scanning the piece map and atomically
    /// transitioning it from `Needed` to `InFlight`.
    ///
    /// Returns `None` if no `Needed` piece exists (all are `Done`, `InFlight`,
    /// or `Failed`).
    ///
    /// ## Mode-dependent selection
    ///
    /// - **Bulk** — sequential scan via [`SharedPieceMap::next_needed`].
    ///   Zero allocation, lock-free CAS. Optimal for web-seed-only swarms
    ///   where all peers have all pieces, producing sequential disk I/O.
    ///
    /// - **Streaming** — priority-weighted rarest-first via
    ///   [`select_next_piece`]. Builds a transient needed list and peer
    ///   bitfield snapshot each call (allocation cost is acceptable since
    ///   piece fetch is orders of magnitude slower). Ensures playhead and
    ///   metadata pieces are fetched first.
    fn claim_next_piece(&self) -> Option<u32> {
        match self.config.download_mode {
            DownloadMode::Bulk => self.claim_next_piece_bulk(),
            DownloadMode::Streaming {
                ref priority_map, ..
            } => self.claim_next_piece_streaming(priority_map),
        }
    }

    /// Bulk mode: sequential scan + CAS. Lock-free, zero-allocation.
    fn claim_next_piece_bulk(&self) -> Option<u32> {
        // Scan for Needed pieces and try to claim one. Another thread may
        // claim the same piece between next_needed() and try_claim(), so
        // we retry on CAS failure.
        for _ in 0..self.info.piece_count() {
            if let Some(idx) = self.piece_map.next_needed() {
                if self.piece_map.try_claim(idx) {
                    return Some(idx);
                }
            } else {
                return None;
            }
        }
        None
    }

    /// Streaming mode: priority-weighted rarest-first piece selection.
    ///
    /// Builds a snapshot of needed pieces and peer availability, then
    /// delegates to [`select_next_piece`] which scores each candidate
    /// by `rarity × priority_weight`. The highest-scored piece is
    /// claimed via CAS; on CAS failure (another thread claimed it),
    /// the next-best candidate is tried.
    fn claim_next_piece_streaming(
        &self,
        priority_map: &Arc<Mutex<PiecePriorityMap>>,
    ) -> Option<u32> {
        use crate::bitfield::PeerBitfield;

        // Build needed list — all pieces still in Needed state.
        let needed: Vec<u32> = (0..self.info.piece_count())
            .filter(|&i| self.piece_map.get(i) == PieceState::Needed)
            .collect();
        if needed.is_empty() {
            return None;
        }

        // Build per-peer bitfields. Web seeds report `new_full()` since
        // they can serve any piece; BT peers would provide real bitfields
        // (plumbed through the Peer trait's `has_piece` for now).
        let peer_bitfields: Vec<PeerBitfield> = self
            .peers
            .iter()
            .map(|peer| {
                if peer.kind() == PeerKind::WebSeed {
                    PeerBitfield::new_full(self.info.piece_count())
                } else {
                    // Build bitfield from the Peer trait's has_piece() queries.
                    let mut bf = PeerBitfield::new_empty(self.info.piece_count());
                    for i in 0..self.info.piece_count() {
                        if peer.has_piece(i) {
                            bf.set_piece(i);
                        }
                    }
                    bf
                }
            })
            .collect();

        let bf_refs: Vec<&PeerBitfield> = peer_bitfields.iter().collect();

        // Lock the priority map briefly to score pieces.
        let pm = priority_map.lock().unwrap_or_else(|e| e.into_inner());

        // Use `select_next_piece` which computes rarity scores and weights
        // by priority, returning the highest-scored candidate.
        let selection = select_next_piece(&needed, &bf_refs, &pm, self.info.piece_count());
        drop(pm);

        // Try to claim the selected piece. On CAS failure (another thread
        // got it), fall back to sequential scan as a simple recovery path.
        if let Some(sel) = selection {
            if self.piece_map.try_claim(sel.piece_index) {
                return Some(sel.piece_index);
            }
        }

        // CAS contention fallback: try the remaining needed pieces in order.
        needed
            .iter()
            .find(|&&idx| self.piece_map.try_claim(idx))
            .copied()
    }

    /// Bookkeeping for a successful piece download (called under lock).
    #[allow(clippy::too_many_arguments)]
    fn handle_piece_success(
        &self,
        piece_index: u32,
        peer_idx: usize,
        peer_kind: PeerKind,
        data: &[u8],
        fetch_start: Instant,
        start_time: Instant,
        state: &mut CoordinatorMutableState,
        on_progress: &Mutex<&mut (dyn FnMut(CoordinatorProgress) + Send)>,
    ) {
        // Record successful fetch in peer stats.
        if let Some(s) = state.tracker.get_mut(peer_idx) {
            s.record_success(data.len() as u64, fetch_start.elapsed(), Instant::now());
        }
        // Record heartbeat in phi detector.
        if let Some(phi) = state.phi_detectors.get_mut(peer_idx) {
            phi.record_heartbeat(Instant::now());
        }
        // Clear corruption ledger for this piece.
        state.corruption_ledger.clear_piece(piece_index);

        state.bytes_downloaded = state.bytes_downloaded.saturating_add(data.len() as u64);
        let elapsed = start_time.elapsed().as_secs_f64();

        if let Ok(mut cb) = on_progress.lock() {
            cb(CoordinatorProgress::PieceComplete {
                piece_index,
                pieces_done: self.piece_map.done_count(),
                pieces_total: self.info.piece_count(),
                peer_kind,
                bytes_downloaded: state.bytes_downloaded,
                elapsed_secs: elapsed,
            });
        }

        // Auto-save resume checkpoint.
        if let Some(ref resume_path) = self.config.resume_path {
            let resume_state = ResumeState::from_piece_map(&self.piece_map, self.info.file_size);
            let _ = resume_state.save(resume_path);
        }

        // Periodic availability broadcast (every 10 pieces).
        let done_count = self.piece_map.done_count();
        if done_count.is_multiple_of(10) || done_count == self.info.piece_count() {
            let completed: Vec<bool> = (0..self.info.piece_count())
                .map(|i| self.piece_map.get(i) == PieceState::Done)
                .collect();
            let speed = if elapsed > 0.0 {
                (state.bytes_downloaded as f64 / elapsed) as u64
            } else {
                0
            };
            if let Ok(mut cb) = on_progress.lock() {
                cb(CoordinatorProgress::AvailabilityUpdate {
                    completed_pieces: completed,
                    piece_count: self.info.piece_count(),
                    download_speed: speed,
                });
            }
        }
    }

    /// Bookkeeping for a SHA-1 hash failure (called under lock).
    #[allow(clippy::too_many_arguments)]
    fn handle_hash_failure(
        &self,
        piece_index: u32,
        peer_idx: usize,
        peer_kind: PeerKind,
        piece_length: u32,
        error: &CoordinatorError,
        state: &mut CoordinatorMutableState,
        on_progress: &Mutex<&mut (dyn FnMut(CoordinatorProgress) + Send)>,
    ) {
        if let Some(r) = state.retry_counts.get_mut(piece_index as usize) {
            *r = r.saturating_add(1);
        }
        if let Some(slot) = state.last_failed_peer.get_mut(piece_index as usize) {
            *slot = Some(peer_idx);
        }

        // Record corruption in peer stats tracker.
        if let Some(s) = state.tracker.get_mut(peer_idx) {
            s.record_corruption(Instant::now());
        }

        // Blame analysis (aMule CorruptionBlackBox).
        state.corruption_ledger.record(
            piece_index,
            Attribution {
                start: 0,
                end: piece_length,
                peer_index: peer_idx,
            },
        );
        let blame = state
            .corruption_ledger
            .blame_analysis(piece_index, piece_length);
        for entry in &blame {
            if entry.should_escalate {
                if let Some(s) = state.tracker.get_mut(entry.peer_index) {
                    s.apply_exclusion(Instant::now());
                }
            }
        }
        state.corruption_ledger.clear_piece(piece_index);

        // Peer blacklisting.
        if let Some(count) = state.peer_failures.get_mut(peer_idx) {
            *count = count.saturating_add(1);
            if self.config.max_peer_failures > 0
                && *count >= self.config.max_peer_failures
                && !state
                    .peer_blacklisted
                    .get(peer_idx)
                    .copied()
                    .unwrap_or(false)
            {
                if let Some(bl) = state.peer_blacklisted.get_mut(peer_idx) {
                    *bl = true;
                }
                if let Ok(mut cb) = on_progress.lock() {
                    cb(CoordinatorProgress::PeerBlacklisted {
                        peer_index: peer_idx,
                        peer_kind,
                        failure_count: *count,
                    });
                }
            }
        }

        if let Ok(mut cb) = on_progress.lock() {
            cb(CoordinatorProgress::PieceRetry {
                piece_index,
                peer_kind,
                error: error.to_string(),
            });
        }
    }

    /// Bookkeeping for a fetch failure (network error, timeout, etc.).
    fn handle_fetch_failure(
        &self,
        piece_index: u32,
        peer_idx: usize,
        peer_kind: PeerKind,
        error: &PeerError,
        state: &mut CoordinatorMutableState,
        on_progress: &Mutex<&mut (dyn FnMut(CoordinatorProgress) + Send)>,
    ) {
        // Record failure type in peer stats tracker.
        if let Some(s) = state.tracker.get_mut(peer_idx) {
            let now = Instant::now();
            match error {
                PeerError::Timeout { .. } => s.record_timeout(now),
                PeerError::Rejected { reason, .. } => {
                    s.record_rejection(reason.clone(), now);
                }
                _ => s.record_failure(now),
            }
        }

        if let Some(r) = state.retry_counts.get_mut(piece_index as usize) {
            *r = r.saturating_add(1);
        }
        if let Some(slot) = state.last_failed_peer.get_mut(piece_index as usize) {
            *slot = Some(peer_idx);
        }

        if let Ok(mut cb) = on_progress.lock() {
            cb(CoordinatorProgress::PieceRetry {
                piece_index,
                peer_kind,
                error: error.to_string(),
            });
        }
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
        phi_detectors: &[PhiDetector],
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
                    // Skip peers in timed exclusion (aMule DeadSourceList).
                    && !tracker.get(*i).is_some_and(|s| s.is_excluded(now))
            })
            .max_by_key(|(i, p)| {
                // D049 composite score as primary key, speed estimate as tiebreaker.
                let score = tracker.composite_score(*i, now);
                // PhiDetector penalty: suspect peers (phi ≥ 3.0) get their
                // score halved. This deprioritises "zombie" peers without
                // fully banning them. Cassandra/Akka pattern — gradual
                // degradation rather than binary alive/dead.
                let phi_penalty = phi_detectors
                    .get(*i)
                    .is_some_and(|d| d.phi(now) >= SUSPECT_PHI_THRESHOLD);
                let adjusted_score = if phi_penalty { score / 2 } else { score };
                (adjusted_score, p.speed_estimate())
            });

        if let Some((idx, peer)) = primary {
            return Some((idx, peer.as_ref()));
        }

        // Fallback: allow the excluded peer (retry rotation is best-effort).
        // Also relax min_peer_speed — a slow peer is better than no peer.
        // Still exclude permanently rejected peers — they will never serve us.
        // Still exclude peers in timed exclusion — they need cool-off time.
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
                    && !tracker.get(*i).is_some_and(|s| s.is_excluded(now))
            })
            .max_by_key(|(i, p)| {
                let score = tracker.composite_score(*i, now);
                (score, p.speed_estimate())
            })
            .map(|(idx, peer)| (idx, peer.as_ref()))
    }

    /// Attempts to load resume state from disk and restore the piece map.
    ///
    /// Returns `true` if pieces were successfully restored (the output file
    /// should be opened without truncation). Returns `false` if no resume
    /// path is configured, no file exists, the file is corrupt, or the
    /// parameters don't match (fresh start with truncation).
    fn try_load_resume(&self, on_progress: &mut (dyn FnMut(CoordinatorProgress) + Send)) -> bool {
        let resume_path = match self.config.resume_path {
            Some(ref p) => p,
            None => return false,
        };

        let state = match ResumeState::load(resume_path) {
            Ok(s) => s,
            Err(_) => return false,
        };

        // Reject resume state from a different torrent.
        if state
            .validate(self.info.piece_count(), self.info.file_size)
            .is_err()
        {
            return false;
        }

        // Restore completed pieces into the piece map.
        let mut pieces_resumed = 0u32;
        let mut bytes_resumed = 0u64;
        for i in 0..state.piece_count() {
            if state.is_done(i) && self.piece_map.get(i) != PieceState::Done {
                self.piece_map.mark_done(i);
                pieces_resumed = pieces_resumed.saturating_add(1);
                bytes_resumed = bytes_resumed.saturating_add(self.info.piece_size(i) as u64);
            }
        }

        if pieces_resumed > 0 {
            on_progress(CoordinatorProgress::Resumed {
                pieces_resumed,
                pieces_total: self.info.piece_count(),
                bytes_resumed,
            });
            true
        } else {
            false
        }
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

// ── Partition recovery (IRC netsplit pattern) ───────────────────────

/// Tracks per-peer bitfield snapshots for netsplit-aware reconnection.
///
/// ## Why (IRC netsplit lesson)
///
/// When a peer disconnects and reconnects (analogous to an IRC netsplit/
/// netburst), its piece availability may have changed. Blindly trusting
/// cached state risks requesting pieces the peer no longer has (stale
/// entries) or missing pieces the peer acquired while partitioned.
///
/// ## How
///
/// On disconnect, the `PartitionRecovery` saves the peer's last-known
/// bitfield snapshot. On reconnect, the coordinator requests a fresh
/// bitfield and calls [`reconcile`] to diff the two. The diff reveals:
/// - **Gained pieces** — the peer acquired new pieces during the partition.
/// - **Lost pieces** — pieces the peer no longer has (storage failure,
///   pruning, or data corruption during the partition).
///
/// The coordinator uses this diff to update its piece availability map
/// without trusting stale cached state.
pub struct PartitionRecovery {
    /// Saved bitfield snapshots, keyed by peer index.
    ///
    /// Only populated for peers that have disconnected. Removed after
    /// successful reconciliation.
    snapshots: std::collections::HashMap<usize, Vec<bool>>,
}

/// Result of reconciling a peer's pre-partition and post-reconnect bitfields.
///
/// The coordinator uses this to update piece availability without trusting
/// stale cached state from before the partition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReconciliationDiff {
    /// Piece indices the peer gained during the partition (not in old, in new).
    pub gained: Vec<u32>,
    /// Piece indices the peer lost during the partition (in old, not in new).
    pub lost: Vec<u32>,
}

impl PartitionRecovery {
    /// Creates an empty partition recovery tracker.
    pub fn new() -> Self {
        Self {
            snapshots: std::collections::HashMap::new(),
        }
    }

    /// Saves a peer's bitfield snapshot on disconnect.
    ///
    /// Call this when a peer disconnects so that on reconnect the coordinator
    /// can diff the old and new bitfields.
    pub fn save_snapshot(&mut self, peer_index: usize, bitfield: Vec<bool>) {
        self.snapshots.insert(peer_index, bitfield);
    }

    /// Removes a saved snapshot without reconciling (e.g. peer permanently gone).
    pub fn discard(&mut self, peer_index: usize) {
        self.snapshots.remove(&peer_index);
    }

    /// Returns `true` if a snapshot exists for the given peer (peer was partitioned).
    pub fn has_snapshot(&self, peer_index: usize) -> bool {
        self.snapshots.contains_key(&peer_index)
    }

    /// Reconciles a peer's pre-partition snapshot against a fresh bitfield
    /// received on reconnect.
    ///
    /// Returns the diff (gained/lost pieces) and removes the saved snapshot.
    /// Returns `None` if no snapshot was saved for this peer (peer was not
    /// partitioned — treat as a fresh join).
    pub fn reconcile(
        &mut self,
        peer_index: usize,
        fresh_bitfield: &[bool],
    ) -> Option<ReconciliationDiff> {
        let old = self.snapshots.remove(&peer_index)?;

        let max_len = old.len().max(fresh_bitfield.len());
        let mut gained = Vec::new();
        let mut lost = Vec::new();

        for i in 0..max_len {
            let was_available = old.get(i).copied().unwrap_or(false);
            let now_available = fresh_bitfield.get(i).copied().unwrap_or(false);

            if !was_available && now_available {
                gained.push(i as u32);
            } else if was_available && !now_available {
                lost.push(i as u32);
            }
        }

        Some(ReconciliationDiff { gained, lost })
    }

    /// Returns the number of peers with saved snapshots.
    pub fn partitioned_count(&self) -> usize {
        self.snapshots.len()
    }
}

impl Default for PartitionRecovery {
    fn default() -> Self {
        Self::new()
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

    // ── PartitionRecovery (IRC netsplit pattern) ────────────────────

    /// New PartitionRecovery starts with no snapshots.
    #[test]
    fn partition_recovery_starts_empty() {
        let pr = PartitionRecovery::new();
        assert_eq!(pr.partitioned_count(), 0);
        assert!(!pr.has_snapshot(0));
    }

    /// Saving and reconciling a snapshot detects gained and lost pieces.
    ///
    /// When a peer disconnects with [true, false, true] and reconnects
    /// with [true, true, false], piece 1 was gained and piece 2 was lost.
    #[test]
    fn partition_recovery_detects_gained_and_lost() {
        let mut pr = PartitionRecovery::new();
        pr.save_snapshot(0, vec![true, false, true, false]);
        assert!(pr.has_snapshot(0));

        let fresh = vec![true, true, false, false];
        let diff = pr.reconcile(0, &fresh).unwrap();

        assert_eq!(diff.gained, vec![1]);
        assert_eq!(diff.lost, vec![2]);
        // Snapshot is consumed after reconciliation.
        assert!(!pr.has_snapshot(0));
    }

    /// Reconciling without a saved snapshot returns None (fresh join).
    ///
    /// If a peer connects for the first time (never disconnected), there
    /// is no pre-partition state to diff against.
    #[test]
    fn partition_recovery_no_snapshot_returns_none() {
        let mut pr = PartitionRecovery::new();
        let result = pr.reconcile(5, &[true, false]);
        assert!(result.is_none());
    }

    /// Discard removes a saved snapshot without reconciling.
    #[test]
    fn partition_recovery_discard() {
        let mut pr = PartitionRecovery::new();
        pr.save_snapshot(2, vec![true, true]);
        assert_eq!(pr.partitioned_count(), 1);
        pr.discard(2);
        assert_eq!(pr.partitioned_count(), 0);
        assert!(!pr.has_snapshot(2));
    }

    /// Reconciliation handles different-length bitfields.
    ///
    /// The fresh bitfield may be longer (peer now tracks more pieces) or
    /// shorter (truncated). Missing entries are treated as `false`.
    #[test]
    fn partition_recovery_different_lengths() {
        let mut pr = PartitionRecovery::new();
        // Old: 3 pieces, New: 5 pieces (peer now has 2 extra).
        pr.save_snapshot(0, vec![true, false, true]);
        let fresh = vec![true, false, true, true, true];
        let diff = pr.reconcile(0, &fresh).unwrap();
        assert_eq!(diff.gained, vec![3, 4]);
        assert!(diff.lost.is_empty());
    }

    /// Identical bitfields produce an empty diff.
    #[test]
    fn partition_recovery_no_change() {
        let mut pr = PartitionRecovery::new();
        pr.save_snapshot(0, vec![true, false, true]);
        let diff = pr.reconcile(0, &[true, false, true]).unwrap();
        assert!(diff.gained.is_empty());
        assert!(diff.lost.is_empty());
    }

    /// Default trait implementation works.
    #[test]
    fn partition_recovery_default() {
        let pr = PartitionRecovery::default();
        assert_eq!(pr.partitioned_count(), 0);
    }

    // ── DownloadMode ────────────────────────────────────────────────

    /// Bulk mode downloads all pieces using sequential piece selection.
    ///
    /// Validates that the default `DownloadMode::Bulk` path produces a
    /// correct output file. This is the zero-overhead fast path for web
    /// seeds.
    #[test]
    fn bulk_mode_downloads_all_pieces() {
        let piece_a = vec![0xAA; 256];
        let piece_b = vec![0xBB; 128];
        let info = make_torrent_info(&[&piece_a, &piece_b]);

        let config = CoordinatorConfig {
            download_mode: DownloadMode::Bulk,
            ..CoordinatorConfig::default()
        };

        let mut coord = PieceCoordinator::new(info, config);
        coord.add_peer(Box::new(MockPeer::web_seed(vec![
            piece_a.clone(),
            piece_b.clone(),
        ])));

        let tmp = std::env::temp_dir().join("p2p-bulk-mode-test");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let out = tmp.join("bulk.bin");

        let result = coord.run(&out, &mut |_| {});
        assert!(result.is_ok(), "bulk mode failed: {:?}", result);

        let data = std::fs::read(&out).unwrap();
        let mut expected = piece_a;
        expected.extend_from_slice(&piece_b);
        assert_eq!(data, expected);

        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// Streaming mode uses priority-weighted piece selection and notifies
    /// the `StreamNotifier` after each completed piece.
    ///
    /// Validates that `DownloadMode::Streaming` correctly integrates the
    /// `PiecePriorityMap` and `StreamNotifier` from `reader.rs`, and that
    /// completed pieces produce byte-range notifications.
    #[test]
    fn streaming_mode_downloads_and_notifies() {
        use crate::priority::PiecePriorityMap;
        use crate::reader::StreamingReader;
        use crate::streaming::{BufferPolicy, ByteRangeMap};

        let piece_a = vec![0xAA; 256];
        let piece_b = vec![0xBB; 128];
        let info = make_torrent_info(&[&piece_a, &piece_b]);

        let tmp = std::env::temp_dir().join("p2p-streaming-mode-test");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();

        // Create the output file for the coordinator.
        let out = tmp.join("streaming.bin");

        // Create a backing file for StreamingReader (needs to exist for open).
        let reader_file = tmp.join("reader.bin");
        std::fs::write(&reader_file, [0u8; 384]).unwrap();

        let (_reader, notifier) = StreamingReader::new_streaming(
            &reader_file,
            ByteRangeMap::new(384),
            BufferPolicy::default(),
        )
        .unwrap();

        // Priority map: piece 1 is Critical, piece 0 stays Normal.
        let now = Instant::now();
        let mut pm = PiecePriorityMap::new(2, now);
        pm.update(&[1], &[], &[], now);
        let priority_map = Arc::new(Mutex::new(pm));

        let config = CoordinatorConfig {
            download_mode: DownloadMode::Streaming {
                priority_map: Arc::clone(&priority_map),
                notifier,
            },
            ..CoordinatorConfig::default()
        };

        let mut coord = PieceCoordinator::new(info, config);
        coord.add_peer(Box::new(MockPeer::web_seed(vec![
            piece_a.clone(),
            piece_b.clone(),
        ])));

        let mut completed_pieces = Vec::new();
        let result = coord.run(&out, &mut |ev| {
            if let CoordinatorProgress::PieceComplete { piece_index, .. } = ev {
                completed_pieces.push(piece_index);
            }
        });
        assert!(result.is_ok(), "streaming mode failed: {:?}", result);

        // Verify all pieces downloaded.
        let data = std::fs::read(&out).unwrap();
        let mut expected = piece_a;
        expected.extend_from_slice(&piece_b);
        assert_eq!(data, expected);

        // Both pieces must have been completed.
        assert_eq!(completed_pieces.len(), 2);
        assert!(completed_pieces.contains(&0));
        assert!(completed_pieces.contains(&1));

        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// Streaming mode with a single peer prioritises the Critical piece.
    ///
    /// With concurrency=1, piece selection is deterministic: the Critical
    /// piece (index 1) must be selected before the Normal piece (index 0).
    /// This validates that `select_next_piece()` is actually wired into
    /// the coordinator for Streaming mode.
    #[test]
    fn streaming_mode_respects_priority_order() {
        use crate::priority::PiecePriorityMap;
        use crate::reader::StreamingReader;
        use crate::streaming::{BufferPolicy, ByteRangeMap};

        let piece_a = vec![0xAA; 256];
        let piece_b = vec![0xBB; 256];
        let info = make_torrent_info(&[&piece_a, &piece_b]);

        let tmp = std::env::temp_dir().join("p2p-streaming-priority-test");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();

        let out = tmp.join("priority.bin");

        let reader_file = tmp.join("reader.bin");
        std::fs::write(&reader_file, [0u8; 512]).unwrap();

        let (_reader, notifier) = StreamingReader::new_streaming(
            &reader_file,
            ByteRangeMap::new(512),
            BufferPolicy::default(),
        )
        .unwrap();

        // Priority map: piece 1 is Critical, piece 0 stays Normal.
        // Critical weight (1000) always beats Normal (200) in the
        // additive two-tier scoring formula.
        let now = Instant::now();
        let mut pm = PiecePriorityMap::new(2, now);
        pm.update(&[1], &[], &[], now);
        let priority_map = Arc::new(Mutex::new(pm));

        let config = CoordinatorConfig {
            // Single worker ensures deterministic ordering.
            max_concurrent_pieces: 1,
            download_mode: DownloadMode::Streaming {
                priority_map: Arc::clone(&priority_map),
                notifier,
            },
            ..CoordinatorConfig::default()
        };

        let mut coord = PieceCoordinator::new(info, config);
        coord.add_peer(Box::new(MockPeer::web_seed(vec![
            piece_a.clone(),
            piece_b.clone(),
        ])));

        let mut order = Vec::new();
        let result = coord.run(&out, &mut |ev| {
            if let CoordinatorProgress::PieceComplete { piece_index, .. } = ev {
                order.push(piece_index);
            }
        });
        assert!(result.is_ok(), "streaming priority failed: {:?}", result);

        // Critical piece 1 must be downloaded before Low piece 0.
        assert_eq!(order, vec![1, 0], "expected Critical piece first");

        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// DownloadMode default is Bulk.
    ///
    /// Ensures the zero-overhead path is the default for callers that don't
    /// specify a mode.
    #[test]
    fn download_mode_default_is_bulk() {
        assert!(matches!(DownloadMode::default(), DownloadMode::Bulk));
    }

    /// DownloadMode Debug formatting.
    ///
    /// Streaming variant must not attempt to format the inner Mutex/Arc
    /// fields — it should print a placeholder.
    #[test]
    fn download_mode_debug_formatting() {
        let bulk_debug = format!("{:?}", DownloadMode::Bulk);
        assert_eq!(bulk_debug, "Bulk");
    }

    /// CoordinatorConfig Debug includes download_mode field.
    #[test]
    fn coordinator_config_debug_includes_download_mode() {
        let config = CoordinatorConfig::default();
        let debug = format!("{:?}", config);
        assert!(
            debug.contains("download_mode"),
            "Debug output must include download_mode field"
        );
        assert!(
            debug.contains("Bulk"),
            "Default mode should be Bulk in debug output"
        );
    }
}
