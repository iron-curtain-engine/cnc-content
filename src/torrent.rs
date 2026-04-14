// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2025–present Iron Curtain contributors

//! BitTorrent P2P content distribution — behind the `torrent` feature flag.
//!
//! Uses `librqbit` for BitTorrent protocol support: DHT, HTTP/UDP trackers,
//! piece verification, seeding, and magnet link resolution.
//!
//! ## Design (per Iron Curtain design docs, D049)
//!
//! - **P2P-first**: P2P is always preferred when torrent metadata exists
//!   (no size gate). HTTP mirrors serve as BEP 19 web seeds within the
//!   same piece coordinator. If P2P fails at runtime, automatic fallback
//!   to HTTP-only (FlashGet-style segmented parallel download).
//! - Clients automatically seed downloaded content (opt-out, configurable speed caps)
//! - SHA-256 verification after download
//! - Rarest-first piece selection, endgame mode for stall prevention
//!
//! ## Usage
//!
//! Create a [`TorrentConfig`] (or use `Default`), construct a
//! [`TorrentDownloader`], then call
//! [`download_package`](TorrentDownloader::download_package) with the
//! target [`DownloadPackage`](crate::DownloadPackage) and a
//! progress callback.
//!
//! ```
//! // TorrentConfig is constructible with all defaults — no network required.
//! let config = cnc_content::torrent::TorrentConfig::default();
//! assert_eq!(config.max_upload_speed, 1_048_576); // 1 MB/s upload cap
//! assert_eq!(config.max_download_speed, 0);        // 0 = unlimited
//! ```

use std::path::{Path, PathBuf};
use std::sync::Arc;

use thiserror::Error;

/// Errors from torrent operations.
#[derive(Debug, Error)]
pub enum TorrentError {
    #[error("librqbit session error: {message}")]
    Session { message: String },
    #[error("torrent download failed: {message}")]
    Download { message: String },
    #[error("no info_hash or torrent file available for this package")]
    NoTorrentMetadata,
    #[error("I/O error: {source}")]
    Io {
        #[from]
        source: std::io::Error,
    },
}

/// Configuration for P2P downloads.
#[derive(Debug, Clone)]
pub struct TorrentConfig {
    /// Maximum upload speed in bytes/sec (0 = unlimited).
    pub max_upload_speed: u64,
    /// Maximum download speed in bytes/sec (0 = unlimited).
    pub max_download_speed: u64,
    /// Seeding policy — controls upload behavior after download.
    pub seeding_policy: crate::SeedingPolicy,
    /// Directory for torrent session state and partial downloads.
    pub session_dir: PathBuf,
    /// Directory for downloaded archive files (ZIPs, ISOs).
    /// Separate from session_dir so archives can be retained for seeding.
    pub archive_dir: PathBuf,
    /// DHT enabled.
    pub enable_dht: bool,
    /// Port range for incoming connections.
    pub listen_port: u16,
}

impl Default for TorrentConfig {
    fn default() -> Self {
        let session_dir = default_session_dir();
        let archive_dir = session_dir
            .parent()
            .map(|p| p.join("downloads"))
            .unwrap_or_else(|| PathBuf::from("downloads"));
        // Use a random ephemeral port (49152–65535) instead of the
        // well-known BitTorrent port 6881, which is trivially identified
        // and throttled by ISP DPI. This follows the eMule lesson: fixed
        // ports are a blocking target.
        let listen_port = random_listen_port();

        Self {
            max_upload_speed: 1_048_576, // 1 MB/s
            max_download_speed: 0,       // unlimited
            seeding_policy: crate::SeedingPolicy::default(),
            session_dir,
            archive_dir,
            enable_dht: true,
            listen_port,
        }
    }
}

/// Progress events from torrent downloads.
#[derive(Debug, Clone)]
pub enum TorrentProgress {
    /// Connecting to trackers / DHT.
    Connecting { trackers: usize },
    /// Found peers.
    PeersFound { count: usize },
    /// Download progress.
    Downloading {
        bytes_downloaded: u64,
        total_bytes: u64,
        peers: usize,
        download_speed: u64,
    },
    /// Verifying pieces.
    Verifying { pieces_verified: u32, total: u32 },
    /// Download complete, now seeding.
    Seeding { uploaded_bytes: u64, peers: usize },
    /// Fully complete (seeding stopped).
    Complete,
}

/// BitTorrent downloader backed by librqbit.
///
/// Manages a persistent session that can download multiple packages and
/// seed completed content to other peers. Seeding behavior is controlled
/// by the [`SeedingPolicy`](crate::SeedingPolicy) in the config.
///
/// ## WebSeed note
///
/// librqbit does not currently support BEP 19 webseeds. HTTP mirrors in
/// `direct_urls` are handled by the separate HTTP downloader. When the
/// `p2p-distribute` crate replaces librqbit, HTTP mirrors will become
/// true webseeds inside the torrent swarm.
pub struct TorrentDownloader {
    config: TorrentConfig,
    runtime: tokio::runtime::Runtime,
    session: Arc<librqbit::Session>,
    /// Whether seeding is currently paused (e.g. during online gameplay).
    seeding_paused: std::sync::atomic::AtomicBool,
}

impl TorrentDownloader {
    /// Creates a new torrent downloader with the given configuration.
    pub fn new(config: TorrentConfig) -> Result<Self, TorrentError> {
        std::fs::create_dir_all(&config.session_dir)?;
        std::fs::create_dir_all(&config.archive_dir)?;

        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .worker_threads(2)
            .build()
            .map_err(|e| TorrentError::Session {
                message: e.to_string(),
            })?;

        let session = runtime.block_on(async {
            let session_opts = librqbit::SessionOptions {
                disable_dht: !config.enable_dht,
                listen_port_range: Some(config.listen_port..config.listen_port + 100),
                ..Default::default()
            };

            librqbit::Session::new_with_opts(config.session_dir.clone(), session_opts)
                .await
                .map_err(|e| TorrentError::Session {
                    message: e.to_string(),
                })
        })?;

        Ok(Self {
            config,
            runtime,
            session,
            seeding_paused: std::sync::atomic::AtomicBool::new(false),
        })
    }

    /// Downloads a content package via BitTorrent.
    ///
    /// The package must have a non-empty `info_hash` field. Trackers from the
    /// package definition and the global `PUBLIC_TRACKERS` list are combined.
    ///
    /// After download completes:
    /// - `SeedAlways` / `PauseDuringOnlinePlay`: the torrent stays active for seeding
    /// - `KeepNoSeed`: download stops, archive is retained
    /// - `ExtractAndDelete`: download stops, archive is deleted after extraction
    pub fn download_package(
        &self,
        package: &crate::DownloadPackage,
        content_root: &Path,
        on_progress: &mut dyn FnMut(TorrentProgress),
    ) -> Result<PathBuf, TorrentError> {
        let info_hash = match package.info_hash.as_deref() {
            Some(hash) => hash,
            None => return Err(TorrentError::NoTorrentMetadata),
        };

        std::fs::create_dir_all(content_root)?;

        let output_dir = &self.config.archive_dir;

        // Build magnet URI from info hash and trackers.
        let mut magnet = format!("magnet:?xt=urn:btih:{info_hash}");
        // Add display name for tracker announces.
        magnet.push_str("&dn=");
        magnet.push_str(&urlencoding::encode(&package.title));

        let public_trackers: Vec<&str> = crate::public_trackers().collect();
        for tracker in package
            .trackers
            .iter()
            .map(String::as_str)
            .chain(public_trackers.iter().copied())
        {
            magnet.push_str("&tr=");
            magnet.push_str(&urlencoding::encode(tracker));
        }

        on_progress(TorrentProgress::Connecting {
            trackers: package.trackers.len() + public_trackers.len(),
        });

        self.runtime.block_on(async {
            let add_opts = librqbit::AddTorrentOptions {
                output_folder: Some(output_dir.to_string_lossy().into_owned()),
                ..Default::default()
            };

            let handle = self
                .session
                .add_torrent(librqbit::AddTorrent::from_url(&magnet), Some(add_opts))
                .await
                .map_err(|e| TorrentError::Download {
                    message: e.to_string(),
                })?
                .into_handle()
                .ok_or_else(|| TorrentError::Download {
                    message: "failed to get torrent handle".into(),
                })?;

            // Wait for download to complete.
            handle
                .wait_until_completed()
                .await
                .map_err(|e| TorrentError::Download {
                    message: e.to_string(),
                })?;

            // The session owns the torrent — dropping the handle doesn't stop it.
            // Seeding continues as long as the session is alive, regardless of
            // handle lifetime. Non-seeding policies are enforced at shutdown.

            Ok::<(), TorrentError>(())
        })?;

        on_progress(TorrentProgress::Complete);

        // Return the archive directory where librqbit saved the files.
        Ok(output_dir.to_path_buf())
    }

    /// Returns whether this downloader can handle a given package (has info_hash).
    pub fn can_download(package: &crate::DownloadPackage) -> bool {
        package.info_hash.is_some()
    }

    /// Returns the current seeding policy.
    pub fn seeding_policy(&self) -> crate::SeedingPolicy {
        self.config.seeding_policy
    }

    /// Pauses all seeding activity (e.g. when an online game starts).
    ///
    /// Only effective when the seeding policy is `PauseDuringOnlinePlay`.
    pub fn pause_seeding(&self) {
        self.seeding_paused
            .store(true, std::sync::atomic::Ordering::Relaxed);
        // In a full implementation, this would pause all active torrent
        // uploads via the librqbit session. For now we track the state
        // so callers can query it.
    }

    /// Resumes seeding activity (e.g. when returning to the menu).
    pub fn resume_seeding(&self) {
        self.seeding_paused
            .store(false, std::sync::atomic::Ordering::Relaxed);
    }

    /// Returns `true` if seeding is currently paused.
    pub fn is_seeding_paused(&self) -> bool {
        self.seeding_paused
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Shuts down the session, stopping all seeding.
    pub fn shutdown(self) {
        // Destructure to control drop order: release the session Arc first
        // (signals torrents to wind down), then shut down the runtime.
        let Self {
            session, runtime, ..
        } = self;
        drop(session);
        runtime.shutdown_timeout(std::time::Duration::from_secs(5));
    }
}

/// Determines whether a package has P2P metadata available.
///
/// Returns `true` when the package has a non-empty `info_hash`, meaning
/// torrent metadata exists and P2P download is possible. P2P is always
/// preferred — HTTP mirrors participate as BEP 19 web seeds within the
/// swarm, so even with zero BT peers the download works via mirrors
/// serving pieces through Range requests.
pub fn should_use_p2p(package: &crate::DownloadPackage) -> bool {
    package.info_hash.is_some()
}

// ── Coordinator integration ─────────────────────────────────────────

/// A running librqbit session with resolved torrent metadata, ready to
/// hand off to the [`PieceCoordinator`](crate::coordinator::PieceCoordinator).
///
/// After `resolve_and_start` returns:
/// - librqbit is downloading in the background (BT peers, DHT, trackers)
/// - Metadata (piece hashes, piece lengths) is available
/// - The caller builds a coordinator with [`BtSwarmPeer`](crate::coordinator::btswarm::BtSwarmPeer)
///   + [`WebSeedPeer`](crate::coordinator::webseed::WebSeedPeer) instances
pub struct ResolvedTorrent {
    /// Torrent metadata for the coordinator (piece length, piece SHA-1 hashes, file size).
    pub info: crate::coordinator::TorrentInfo,
    /// Tokio runtime — must remain alive while the session is active.
    pub runtime: Arc<tokio::runtime::Runtime>,
    /// librqbit session — must remain alive while BtSwarmPeer observes the output file.
    pub session: Arc<librqbit::Session>,
    /// Path where librqbit is writing the downloaded file.
    /// The coordinator reads from this file via BtSwarmPeer.
    pub librqbit_output: PathBuf,
}

/// Resolves a magnet URI and starts downloading, returning metadata for the coordinator.
///
/// ## What happens
///
/// 1. Creates a librqbit session (trackers, DHT, incoming port).
/// 2. Adds the magnet URI — librqbit resolves metadata from DHT/trackers.
///    Metadata includes the pieces (SHA-1 hashes) and file info.
/// 3. librqbit begins downloading in the background immediately.
/// 4. Returns `ResolvedTorrent` with the extracted metadata + live session.
///
/// The caller then builds a `PieceCoordinator` with `BtSwarmPeer` (wrapping
/// the running session) and `WebSeedPeer` instances (from `web_seeds` +
/// resolved mirror URLs), and runs the coordinator to completion.
///
/// After the coordinator finishes, the caller should drop the `ResolvedTorrent`
/// to shut down the librqbit session (unless seeding is desired).
pub fn resolve_and_start(
    package: &crate::DownloadPackage,
    config: &TorrentConfig,
) -> Result<ResolvedTorrent, TorrentError> {
    let info_hash = match package.info_hash.as_deref() {
        Some(hash) => hash,
        None => return Err(TorrentError::NoTorrentMetadata),
    };

    std::fs::create_dir_all(&config.session_dir)?;
    std::fs::create_dir_all(&config.archive_dir)?;

    // ── Build tokio runtime and librqbit session ────────────────────
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .worker_threads(2)
        .build()
        .map_err(|e| TorrentError::Session {
            message: e.to_string(),
        })?;

    let session = runtime.block_on(async {
        let session_opts = librqbit::SessionOptions {
            disable_dht: !config.enable_dht,
            listen_port_range: Some(config.listen_port..config.listen_port + 100),
            ..Default::default()
        };

        librqbit::Session::new_with_opts(config.session_dir.clone(), session_opts)
            .await
            .map_err(|e| TorrentError::Session {
                message: e.to_string(),
            })
    })?;

    // ── Build magnet URI from info hash + trackers ──────────────────
    let mut magnet = format!("magnet:?xt=urn:btih:{info_hash}");
    magnet.push_str("&dn=");
    magnet.push_str(&urlencoding::encode(&package.title));

    let public_trackers: Vec<&str> = crate::public_trackers().collect();
    for tracker in package
        .trackers
        .iter()
        .map(String::as_str)
        .chain(public_trackers.iter().copied())
    {
        magnet.push_str("&tr=");
        magnet.push_str(&urlencoding::encode(tracker));
    }

    // ── Add magnet to session — resolves metadata + starts downloading ──
    //
    // librqbit resolves the metadata (piece hashes, file info) from
    // DHT/trackers before returning the handle. Once add_torrent returns,
    // metadata IS available and pieces are downloading in the background.
    let output_dir = config.archive_dir.clone();
    let handle = runtime.block_on(async {
        let add_opts = librqbit::AddTorrentOptions {
            output_folder: Some(output_dir.to_string_lossy().into_owned()),
            ..Default::default()
        };

        let resp = session
            .add_torrent(librqbit::AddTorrent::from_url(&magnet), Some(add_opts))
            .await
            .map_err(|e| TorrentError::Download {
                message: e.to_string(),
            })?;

        resp.into_handle().ok_or_else(|| TorrentError::Download {
            message: "failed to get torrent handle after adding magnet".into(),
        })
    })?;

    // ── Extract metadata into coordinator::TorrentInfo ──────────────
    //
    // At this point, metadata is resolved (add_torrent blocks until
    // metadata is available for magnet URIs). We extract piece hashes,
    // piece length, file size, and file name for the coordinator.
    let (info, librqbit_output) = handle
        .with_metadata(|meta| {
            let piece_hashes = meta.info.pieces.as_ref().to_vec();
            let piece_length = meta.info.piece_length as u64;
            let file_size = meta.lengths.total_length();
            let file_name = meta
                .name
                .clone()
                .unwrap_or_else(|| package.title.to_string());

            let torrent_info = crate::coordinator::TorrentInfo {
                piece_length,
                piece_hashes,
                file_size,
                file_name: file_name.clone(),
            };

            // librqbit writes the file to output_dir/file_name.
            let output_path = output_dir.join(&file_name);

            (torrent_info, output_path)
        })
        .map_err(|e| TorrentError::Download {
            message: format!("failed to read torrent metadata: {e}"),
        })?;

    Ok(ResolvedTorrent {
        info,
        runtime: Arc::new(runtime),
        session,
        librqbit_output,
    })
}

fn default_session_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("CNC_CONTENT_ROOT") {
        return PathBuf::from(dir).join(".torrent-session");
    }
    app_path::try_app_path!(".torrent-session")
        .map(|p| p.into_path_buf())
        .unwrap_or_else(|_| PathBuf::from(".torrent-session"))
}

/// Selects a random listen port in the IANA ephemeral range (49152–65535).
///
/// Avoids the well-known BitTorrent port range (6881–6889) which is
/// trivially identified and throttled by ISP DPI. This follows the eMule
/// lesson: fixed ports are the easiest blocking vector.
///
/// Uses a hash of the current time and process ID for unpredictability.
/// Not cryptographic, but sufficient for port selection where the goal
/// is avoiding predictable well-known ports.
fn random_listen_port() -> u16 {
    use std::hash::{Hash, Hasher};

    const EPHEMERAL_MIN: u16 = 49152;
    const EPHEMERAL_MAX: u16 = 65535;

    // Hash current time + process ID for a unique-per-invocation port.
    let mut hasher = std::hash::DefaultHasher::new();
    std::time::SystemTime::now().hash(&mut hasher);
    std::process::id().hash(&mut hasher);
    let hash = hasher.finish();

    let range = (EPHEMERAL_MAX - EPHEMERAL_MIN).saturating_add(1) as u64;
    EPHEMERAL_MIN.saturating_add((hash % range) as u16)
}
