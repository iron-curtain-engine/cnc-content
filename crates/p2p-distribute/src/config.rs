// SPDX-License-Identifier: MIT OR Apache-2.0

//! Unified configuration for the p2p-distribute crate.
//!
//! ## What
//!
//! A single top-level [`Config`] struct that composes every subsystem's
//! configuration and exposes feature toggles for each capability. Advanced
//! users get full control; casual users get sensible defaults.
//!
//! ## Why
//!
//! Individual subsystem configs (`CoordinatorConfig`, `SessionConfig`,
//! `PeerPoolConfig`, etc.) exist in their respective modules. This module
//! ties them together into a coherent whole and adds feature toggles that
//! let consumers disable capabilities they don't need — e.g. disable DHT
//! on a LAN, disable obfuscation when debugging, or disable PEX in a
//! closed swarm.
//!
//! ## How
//!
//! [`Config`] is constructed via [`ConfigBuilder`] (builder pattern).
//! Every field has a sensible default. Subsystem configs are nested —
//! the builder provides both shortcut methods for common tweaks and
//! direct access to subsystem config structs for deep customisation.
//!
//! ```
//! use p2p_distribute::config::{Config, ConfigBuilder};
//!
//! let config = Config::builder()
//!     .enable_dht(false)
//!     .enable_pex(false)
//!     .max_peers(100)
//!     .build();
//!
//! assert!(!config.features.dht);
//! assert!(!config.features.pex);
//! assert_eq!(config.peer_pool.max_peers, 100);
//! ```

use std::time::Duration;

use crate::bridge::BridgeConfig;
use crate::peer_affinity::AffinityConfig;
use crate::peer_pool::PeerPoolConfig;
use crate::piece_validator::ValidatorConfig;
use crate::session_manager::SessionConfig;
use crate::streaming::BufferPolicy;

// ── Feature toggles ─────────────────────────────────────────────────

/// Feature toggles — enable or disable individual protocol capabilities.
///
/// Every capability defaults to `true` (enabled). Consumers can disable
/// capabilities they don't need or that are inappropriate for their
/// deployment environment.
///
/// These toggles control whether the local node **advertises and uses**
/// the capability. They do not affect whether the node can *interoperate*
/// with peers that advertise the capability — that is determined by the
/// protocol handshake.
///
/// ```
/// use p2p_distribute::config::FeatureToggles;
///
/// let ft = FeatureToggles::default();
/// assert!(ft.dht);
/// assert!(ft.pex);
/// assert!(ft.obfuscation);
///
/// let restricted = FeatureToggles::all_disabled();
/// assert!(!restricted.dht);
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FeatureToggles {
    /// Kademlia DHT for peer discovery (BEP 5).
    pub dht: bool,

    /// Peer Exchange gossip (BEP 11).
    pub pex: bool,

    /// Local Peer Discovery via multicast (BEP 14).
    pub local_discovery: bool,

    /// Protocol obfuscation (XOR stream masking + random ports).
    /// Helps bypass naive DPI but is not cryptographic.
    pub obfuscation: bool,

    /// Merkle sub-piece verification for large pieces.
    pub merkle_verification: bool,

    /// HTTP web seed support (BEP 19) — fetch pieces via Range requests.
    pub web_seeds: bool,

    /// Byte-range streaming (StreamingReader support).
    pub streaming: bool,

    /// Super-seeding mode (BEP 16) — optimises initial seeding by
    /// offering each piece to only one peer until it propagates.
    pub super_seeding: bool,

    /// Fast extension (BEP 6) — Allowed Fast / Suggest Piece messages.
    pub fast_extension: bool,

    /// Metadata exchange (BEP 9) — fetch torrent info from peers.
    pub metadata_exchange: bool,

    /// Tracker announce support (BEP 15 UDP, BEP 3 HTTP).
    pub tracker_announce: bool,

    /// Upload / seeding capability. When disabled, the node is
    /// download-only (leech mode).
    pub upload: bool,

    /// Endgame mode — aggressive duplicate requesting for the last
    /// few pieces to minimise tail latency.
    pub endgame: bool,

    /// Rate limiting — per-peer and global bandwidth throttling.
    pub rate_limiting: bool,

    /// Relay/NAT traversal support (relay circuits + hole punching).
    pub relay: bool,

    /// P2P-to-HTTP bridge mode — source pieces from HTTP mirrors
    /// and serve them to the swarm.
    pub bridge: bool,

    /// Resume/checkpoint support — save download progress to disk
    /// and recover after crashes.
    pub resume: bool,

    /// Choking algorithm — tit-for-tat upload slot management.
    /// Disable to unchoke all peers unconditionally.
    pub choking: bool,

    /// Credit-based incentive system — reward generous peers with
    /// preferential unchoking.
    pub credit_system: bool,

    /// Corruption ledger — track and blame peers that send bad data.
    pub corruption_tracking: bool,
}

impl Default for FeatureToggles {
    /// All features enabled by default.
    fn default() -> Self {
        Self {
            dht: true,
            pex: true,
            local_discovery: true,
            obfuscation: true,
            merkle_verification: true,
            web_seeds: true,
            streaming: true,
            super_seeding: true,
            fast_extension: true,
            metadata_exchange: true,
            tracker_announce: true,
            upload: true,
            endgame: true,
            rate_limiting: true,
            relay: true,
            bridge: true,
            resume: true,
            choking: true,
            credit_system: true,
            corruption_tracking: true,
        }
    }
}

impl FeatureToggles {
    /// Returns a toggle set with every feature disabled.
    ///
    /// Useful as a starting point when you want to enable only specific
    /// capabilities.
    ///
    /// ```
    /// use p2p_distribute::config::FeatureToggles;
    ///
    /// let mut ft = FeatureToggles::all_disabled();
    /// ft.web_seeds = true;
    /// ft.streaming = true;
    /// assert!(!ft.dht);
    /// assert!(ft.web_seeds);
    /// ```
    pub fn all_disabled() -> Self {
        Self {
            dht: false,
            pex: false,
            local_discovery: false,
            obfuscation: false,
            merkle_verification: false,
            web_seeds: false,
            streaming: false,
            super_seeding: false,
            fast_extension: false,
            metadata_exchange: false,
            tracker_announce: false,
            upload: false,
            endgame: false,
            rate_limiting: false,
            relay: false,
            bridge: false,
            resume: false,
            choking: false,
            credit_system: false,
            corruption_tracking: false,
        }
    }

    /// Returns the [`Capabilities`](crate::handshake::Capabilities) bitmap
    /// that reflects these toggles — for use in the peer handshake.
    pub fn to_capabilities(&self) -> crate::handshake::Capabilities {
        let mut bits: u32 = 0;
        if self.obfuscation {
            bits |= crate::handshake::Capabilities::ENCRYPTION.to_u32();
        }
        if self.merkle_verification {
            bits |= crate::handshake::Capabilities::MERKLE_VERIFY.to_u32();
        }
        if self.pex {
            bits |= crate::handshake::Capabilities::PEX.to_u32();
        }
        if self.dht {
            bits |= crate::handshake::Capabilities::DHT.to_u32();
        }
        if self.web_seeds {
            bits |= crate::handshake::Capabilities::WEB_SEED.to_u32();
        }
        if self.streaming {
            bits |= crate::handshake::Capabilities::STREAMING.to_u32();
        }
        if self.upload {
            bits |= crate::handshake::Capabilities::UPLOAD_SLOTS.to_u32();
        }
        if self.rate_limiting {
            bits |= crate::handshake::Capabilities::RATE_LIMIT.to_u32();
        }
        crate::handshake::Capabilities::from_u32(bits)
    }
}

// ── Obfuscation config ──────────────────────────────────────────────

/// Configuration for protocol obfuscation (XOR stream masking).
///
/// Controls port selection range and obfuscation behaviour. Only
/// effective when `FeatureToggles::obfuscation` is `true`.
///
/// ```
/// use p2p_distribute::config::ObfuscationConfig;
///
/// let cfg = ObfuscationConfig::default();
/// assert_eq!(cfg.port_range, (49152, 65535));
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObfuscationConfig {
    /// Ephemeral port range (min, max) for random port selection.
    /// Default: IANA ephemeral range (49152–65535).
    pub port_range: (u16, u16),

    /// Whether to require obfuscation from all peers (reject plaintext).
    /// Default: false (accept both obfuscated and plaintext connections).
    pub require_obfuscation: bool,

    /// Whether to prefer obfuscated connections when both are available.
    /// Default: true (prefer obfuscated, but fall back to plaintext).
    pub prefer_obfuscated: bool,
}

impl Default for ObfuscationConfig {
    fn default() -> Self {
        Self {
            port_range: (49152, 65535),
            require_obfuscation: false,
            prefer_obfuscated: true,
        }
    }
}

// ── Connection config ───────────────────────────────────────────────

/// Configuration for peer connection lifecycle.
///
/// Controls timeouts and keep-alive behaviour for individual connections.
///
/// ```
/// use p2p_distribute::config::ConnectionConfig;
///
/// let cfg = ConnectionConfig::default();
/// assert_eq!(cfg.handshake_timeout.as_secs(), 30);
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectionConfig {
    /// Maximum time to wait for handshake completion.
    pub handshake_timeout: Duration,

    /// Interval between keep-alive messages on idle connections.
    pub keepalive_interval: Duration,
}

impl Default for ConnectionConfig {
    fn default() -> Self {
        Self {
            handshake_timeout: Duration::from_secs(30),
            keepalive_interval: Duration::from_secs(120),
        }
    }
}

// ── Tracker config ──────────────────────────────────────────────────

/// Configuration for tracker announce behaviour.
///
/// Controls announce intervals, failure tolerance, and request parameters.
///
/// ```
/// use p2p_distribute::config::TrackerConfig;
///
/// let cfg = TrackerConfig::default();
/// assert_eq!(cfg.numwant, 50);
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackerConfig {
    /// Default announce interval when the tracker doesn't specify one.
    pub default_announce_interval: Duration,

    /// Minimum announce interval (floor for tracker-specified values).
    pub min_announce_interval: Duration,

    /// Maximum consecutive failures before a tracker tier is abandoned.
    pub max_failures: u32,

    /// Maximum backoff duration for failed trackers.
    pub max_backoff: Duration,

    /// Number of peers to request from the tracker per announce.
    pub numwant: u32,
}

impl Default for TrackerConfig {
    fn default() -> Self {
        Self {
            default_announce_interval: Duration::from_secs(1800),
            min_announce_interval: Duration::from_secs(60),
            max_failures: 5,
            max_backoff: Duration::from_secs(3600),
            numwant: 50,
        }
    }
}

// ── Relay config ────────────────────────────────────────────────────

/// Configuration for relay / NAT traversal support.
///
/// ```
/// use p2p_distribute::config::RelayConfig;
///
/// let cfg = RelayConfig::default();
/// assert_eq!(cfg.max_circuits_per_relay, 128);
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelayConfig {
    /// Maximum relay circuits this node will maintain simultaneously.
    pub max_circuits_per_relay: u32,

    /// Per-circuit bandwidth limit in bytes/sec.
    pub circuit_bandwidth_limit: u64,

    /// Maximum relay nodes tracked in the registry.
    pub max_relay_nodes: usize,
}

impl Default for RelayConfig {
    fn default() -> Self {
        Self {
            max_circuits_per_relay: 128,
            circuit_bandwidth_limit: 256 * 1024,
            max_relay_nodes: 64,
        }
    }
}

// ── Endgame config ──────────────────────────────────────────────────

/// Configuration for endgame mode behaviour.
///
/// ```
/// use p2p_distribute::config::EndgameConfig;
///
/// let cfg = EndgameConfig::default();
/// assert_eq!(cfg.peer_multiplier, 2);
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EndgameConfig {
    /// Number of additional peers to request each remaining piece from
    /// during endgame mode.
    pub peer_multiplier: u32,

    /// Minimum remaining pieces before endgame activates (safety floor).
    pub min_remaining: u32,
}

impl Default for EndgameConfig {
    fn default() -> Self {
        Self {
            peer_multiplier: 2,
            min_remaining: 1,
        }
    }
}

// ── Choking config ──────────────────────────────────────────────────

/// Configuration for the choking algorithm.
///
/// ```
/// use p2p_distribute::config::ChokingConfig;
///
/// let cfg = ChokingConfig::default();
/// assert_eq!(cfg.unchoke_slots, 4);
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChokingConfig {
    /// Number of regular (performance-based) unchoke slots.
    pub unchoke_slots: usize,

    /// Whether to auto-scale slots based on upload utilisation.
    pub auto_scale_slots: bool,

    /// Evaluation rounds between optimistic unchoke rotations.
    pub optimistic_interval: u32,
}

impl Default for ChokingConfig {
    fn default() -> Self {
        Self {
            unchoke_slots: 4,
            auto_scale_slots: false,
            optimistic_interval: 3,
        }
    }
}

// ── Upload config ───────────────────────────────────────────────────

/// Configuration for upload / seeding behaviour.
///
/// ```
/// use p2p_distribute::config::UploadConfig;
///
/// let cfg = UploadConfig::default();
/// assert_eq!(cfg.max_slots, 4);
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UploadConfig {
    /// Maximum concurrent upload slots.
    pub max_slots: usize,

    /// Maximum queued upload requests.
    pub max_queue: usize,
}

impl Default for UploadConfig {
    fn default() -> Self {
        Self {
            max_slots: 4,
            max_queue: 20,
        }
    }
}

// ── Fast extension config ────────────────────────────────────────────

/// Configuration for BEP 6 fast extension behaviour.
///
/// ```
/// use p2p_distribute::config::FastExtensionConfig;
///
/// let cfg = FastExtensionConfig::default();
/// assert_eq!(cfg.allowed_fast_count, 10);
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FastExtensionConfig {
    /// Number of pieces to include in the Allowed Fast set.
    pub allowed_fast_count: usize,

    /// Maximum Allowed Fast pieces a peer may advertise (safety cap).
    pub max_allowed_fast_count: usize,

    /// Maximum number of Suggest Piece entries to cache per peer.
    pub max_suggestions: usize,
}

impl Default for FastExtensionConfig {
    fn default() -> Self {
        Self {
            allowed_fast_count: 10,
            max_allowed_fast_count: 100,
            max_suggestions: 50,
        }
    }
}

// ── PEX config ──────────────────────────────────────────────────────

/// Configuration for Peer Exchange (BEP 11) gossip.
///
/// ```
/// use p2p_distribute::config::PexConfig;
///
/// let cfg = PexConfig::default();
/// assert_eq!(cfg.max_added, 50);
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PexConfig {
    /// Maximum added entries per PEX message.
    pub max_added: usize,

    /// Maximum dropped entries per PEX message.
    pub max_dropped: usize,

    /// Interval between PEX messages.
    pub interval: Duration,

    /// Maximum total tracked peers in the PEX delta tracker.
    pub max_tracked_peers: usize,
}

impl Default for PexConfig {
    fn default() -> Self {
        Self {
            max_added: 50,
            max_dropped: 50,
            interval: Duration::from_secs(60),
            max_tracked_peers: 200,
        }
    }
}

// ── Web seed config ─────────────────────────────────────────────────

/// Configuration for HTTP web seed (BEP 19) behaviour.
///
/// ```
/// use p2p_distribute::config::WebSeedConfig;
///
/// let cfg = WebSeedConfig::default();
/// assert_eq!(cfg.timeout.as_secs(), 300);
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebSeedConfig {
    /// HTTP request timeout for piece fetches.
    pub timeout: Duration,
}

impl Default for WebSeedConfig {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(300),
        }
    }
}

// ── Local discovery config ──────────────────────────────────────────

/// Configuration for BEP 14 Local Peer Discovery.
///
/// ```
/// use p2p_distribute::config::LocalDiscoveryConfig;
///
/// let cfg = LocalDiscoveryConfig::default();
/// assert_eq!(cfg.announce_interval.as_secs(), 300);
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalDiscoveryConfig {
    /// Interval between multicast announces.
    pub announce_interval: Duration,

    /// Maximum LPD peers to track.
    pub max_peers: usize,

    /// Time before a peer without re-announce is considered stale.
    pub stale_timeout: Duration,

    /// IPv4 multicast address for LPD announces.
    pub multicast_addr: String,

    /// UDP port for LPD announces.
    pub port: u16,
}

impl Default for LocalDiscoveryConfig {
    fn default() -> Self {
        Self {
            announce_interval: Duration::from_secs(300),
            max_peers: 50,
            stale_timeout: Duration::from_secs(600),
            multicast_addr: String::from("239.192.152.143"),
            port: 6771,
        }
    }
}

// ── Metadata exchange config ────────────────────────────────────────

/// Configuration for BEP 9 metadata exchange.
///
/// ```
/// use p2p_distribute::config::MetadataExchangeConfig;
///
/// let cfg = MetadataExchangeConfig::default();
/// assert_eq!(cfg.max_metadata_size, 10 * 1024 * 1024);
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MetadataExchangeConfig {
    /// Maximum metadata size accepted (bytes). Rejects info dicts larger
    /// than this to prevent memory exhaustion.
    pub max_metadata_size: usize,

    /// Block size for metadata exchange chunks.
    pub block_size: usize,
}

impl Default for MetadataExchangeConfig {
    fn default() -> Self {
        Self {
            max_metadata_size: 10 * 1024 * 1024,
            block_size: 16384,
        }
    }
}

// ── Super-seeding config ────────────────────────────────────────────

/// Configuration for BEP 16 super-seeding mode.
///
/// ```
/// use p2p_distribute::config::SuperSeedingConfig;
///
/// let cfg = SuperSeedingConfig::default();
/// assert_eq!(cfg.max_tracked_peers, 200);
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SuperSeedingConfig {
    /// Maximum peers tracked in super-seed state.
    pub max_tracked_peers: usize,

    /// Timeout before an offered piece is considered stuck.
    pub offer_timeout: Duration,

    /// Number of peers that must have a piece before it's considered
    /// "propagated" and a new piece can be offered.
    pub propagation_threshold: u32,
}

impl Default for SuperSeedingConfig {
    fn default() -> Self {
        Self {
            max_tracked_peers: 200,
            offer_timeout: Duration::from_secs(120),
            propagation_threshold: 2,
        }
    }
}

// ── Content discovery config ────────────────────────────────────────

/// Configuration for content source discovery.
///
/// ```
/// use p2p_distribute::config::ContentDiscoveryConfig;
///
/// let cfg = ContentDiscoveryConfig::default();
/// assert_eq!(cfg.max_sources, 128);
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContentDiscoveryConfig {
    /// Maximum content sources tracked in the registry.
    pub max_sources: usize,

    /// Time before a source is considered stale.
    pub stale_threshold: Duration,

    /// Consecutive failures before a source is removed.
    pub failure_threshold: u32,
}

impl Default for ContentDiscoveryConfig {
    fn default() -> Self {
        Self {
            max_sources: 128,
            stale_threshold: Duration::from_secs(3600),
            failure_threshold: 5,
        }
    }
}

// ── Top-level Config ────────────────────────────────────────────────

/// Unified configuration for the p2p-distribute crate.
///
/// Composes all subsystem configs and feature toggles into a single
/// struct. Every field has a sensible default. Use [`Config::builder()`]
/// for ergonomic construction with selective overrides.
///
/// ```
/// use p2p_distribute::config::Config;
///
/// // All defaults — everything enabled with production-ready values.
/// let config = Config::default();
/// assert!(config.features.dht);
/// assert!(config.features.pex);
/// assert_eq!(config.session.max_active_downloads, 3);
/// ```
#[derive(Debug, Clone, Default)]
pub struct Config {
    /// Feature toggles — enable/disable individual capabilities.
    pub features: FeatureToggles,

    /// Session-level resource limits (concurrent downloads, speed caps).
    pub session: SessionConfig,

    /// Peer pool management (max peers, eviction, backoff).
    pub peer_pool: PeerPoolConfig,

    /// Peer affinity scoring weights (region, latency, speed).
    pub affinity: AffinityConfig,

    /// Piece validation behaviour (retries, Merkle sub-piece).
    pub validator: ValidatorConfig,

    /// Bridge node configuration (cache, prefetch, mirror health).
    pub bridge: BridgeConfig,

    /// Streaming buffer policy (prebuffer, timeouts, head/tail).
    pub streaming: BufferPolicy,

    /// Protocol obfuscation (port range, require/prefer flags).
    pub obfuscation: ObfuscationConfig,

    /// Peer connection lifecycle (handshake timeout, keep-alive).
    pub connection: ConnectionConfig,

    /// Tracker announce behaviour (intervals, retries, numwant).
    pub tracker: TrackerConfig,

    /// Relay / NAT traversal (circuit limits, bandwidth caps).
    pub relay: RelayConfig,

    /// Endgame mode (peer multiplier, activation threshold).
    pub endgame: EndgameConfig,

    /// Choking algorithm (unchoke slots, optimistic interval).
    pub choking: ChokingConfig,

    /// Upload / seeding (slot count, queue depth).
    pub upload: UploadConfig,

    /// Fast extension (BEP 6) — Allowed Fast set size, suggestions.
    pub fast_extension: FastExtensionConfig,

    /// Peer Exchange (BEP 11) — message limits, interval.
    pub pex: PexConfig,

    /// HTTP web seed (BEP 19) — timeout.
    pub web_seed: WebSeedConfig,

    /// Local Peer Discovery (BEP 14) — multicast, interval, limits.
    pub local_discovery: LocalDiscoveryConfig,

    /// Metadata exchange (BEP 9) — size limits, block size.
    pub metadata_exchange: MetadataExchangeConfig,

    /// Super-seeding (BEP 16) — peer tracking, propagation policy.
    pub super_seeding: SuperSeedingConfig,

    /// Content source discovery — registry limits, staleness, failures.
    pub content_discovery: ContentDiscoveryConfig,
}

impl Config {
    /// Returns a [`ConfigBuilder`] for ergonomic construction.
    pub fn builder() -> ConfigBuilder {
        ConfigBuilder::new()
    }
}

// ── ConfigBuilder ───────────────────────────────────────────────────

/// Builder for [`Config`] — provides shortcut methods for common tweaks
/// and direct access to subsystem configs for deep customisation.
///
/// ```
/// use p2p_distribute::config::{Config, ConfigBuilder};
///
/// let config = Config::builder()
///     .enable_dht(false)
///     .enable_pex(false)
///     .enable_obfuscation(false)
///     .max_peers(100)
///     .download_rate_limit(1_048_576) // 1 MB/s
///     .upload_rate_limit(524_288)     // 512 KB/s
///     .build();
///
/// assert!(!config.features.dht);
/// assert_eq!(config.peer_pool.max_peers, 100);
/// assert_eq!(config.session.download_rate_limit, 1_048_576);
/// ```
#[derive(Debug, Clone)]
pub struct ConfigBuilder {
    config: Config,
}

impl ConfigBuilder {
    /// Creates a builder with all defaults.
    pub fn new() -> Self {
        Self {
            config: Config::default(),
        }
    }

    /// Builds the final [`Config`].
    pub fn build(self) -> Config {
        self.config
    }

    // ── Feature toggles ─────────────────────────────────────────────

    /// Replaces the entire feature toggles struct.
    pub fn features(mut self, features: FeatureToggles) -> Self {
        self.config.features = features;
        self
    }

    /// Enable/disable DHT peer discovery.
    pub fn enable_dht(mut self, enabled: bool) -> Self {
        self.config.features.dht = enabled;
        self
    }

    /// Enable/disable Peer Exchange.
    pub fn enable_pex(mut self, enabled: bool) -> Self {
        self.config.features.pex = enabled;
        self
    }

    /// Enable/disable Local Peer Discovery.
    pub fn enable_local_discovery(mut self, enabled: bool) -> Self {
        self.config.features.local_discovery = enabled;
        self
    }

    /// Enable/disable protocol obfuscation.
    pub fn enable_obfuscation(mut self, enabled: bool) -> Self {
        self.config.features.obfuscation = enabled;
        self
    }

    /// Enable/disable Merkle sub-piece verification.
    pub fn enable_merkle_verification(mut self, enabled: bool) -> Self {
        self.config.features.merkle_verification = enabled;
        self
    }

    /// Enable/disable HTTP web seed support.
    pub fn enable_web_seeds(mut self, enabled: bool) -> Self {
        self.config.features.web_seeds = enabled;
        self
    }

    /// Enable/disable streaming reader support.
    pub fn enable_streaming(mut self, enabled: bool) -> Self {
        self.config.features.streaming = enabled;
        self
    }

    /// Enable/disable super-seeding mode.
    pub fn enable_super_seeding(mut self, enabled: bool) -> Self {
        self.config.features.super_seeding = enabled;
        self
    }

    /// Enable/disable fast extension (BEP 6).
    pub fn enable_fast_extension(mut self, enabled: bool) -> Self {
        self.config.features.fast_extension = enabled;
        self
    }

    /// Enable/disable metadata exchange (BEP 9).
    pub fn enable_metadata_exchange(mut self, enabled: bool) -> Self {
        self.config.features.metadata_exchange = enabled;
        self
    }

    /// Enable/disable tracker announces.
    pub fn enable_tracker_announce(mut self, enabled: bool) -> Self {
        self.config.features.tracker_announce = enabled;
        self
    }

    /// Enable/disable uploading (seeding). When disabled, the node
    /// operates in leech mode.
    pub fn enable_upload(mut self, enabled: bool) -> Self {
        self.config.features.upload = enabled;
        self
    }

    /// Enable/disable endgame mode.
    pub fn enable_endgame(mut self, enabled: bool) -> Self {
        self.config.features.endgame = enabled;
        self
    }

    /// Enable/disable per-peer rate limiting.
    pub fn enable_rate_limiting(mut self, enabled: bool) -> Self {
        self.config.features.rate_limiting = enabled;
        self
    }

    /// Enable/disable relay / NAT traversal.
    pub fn enable_relay(mut self, enabled: bool) -> Self {
        self.config.features.relay = enabled;
        self
    }

    /// Enable/disable P2P-to-HTTP bridge mode.
    pub fn enable_bridge(mut self, enabled: bool) -> Self {
        self.config.features.bridge = enabled;
        self
    }

    /// Enable/disable resume/checkpoint support.
    pub fn enable_resume(mut self, enabled: bool) -> Self {
        self.config.features.resume = enabled;
        self
    }

    /// Enable/disable choking algorithm.
    pub fn enable_choking(mut self, enabled: bool) -> Self {
        self.config.features.choking = enabled;
        self
    }

    /// Enable/disable credit-based incentive system.
    pub fn enable_credit_system(mut self, enabled: bool) -> Self {
        self.config.features.credit_system = enabled;
        self
    }

    /// Enable/disable corruption tracking ledger.
    pub fn enable_corruption_tracking(mut self, enabled: bool) -> Self {
        self.config.features.corruption_tracking = enabled;
        self
    }

    // ── Shortcut methods for common parameters ──────────────────────

    /// Maximum peers in the pool.
    pub fn max_peers(mut self, n: usize) -> Self {
        self.config.peer_pool.max_peers = n;
        self
    }

    /// Maximum concurrent active downloads.
    pub fn max_active_downloads(mut self, n: usize) -> Self {
        self.config.session.max_active_downloads = n;
        self
    }

    /// Global download speed limit (bytes/sec). 0 = unlimited.
    pub fn download_rate_limit(mut self, bytes_per_sec: u64) -> Self {
        self.config.session.download_rate_limit = bytes_per_sec;
        self
    }

    /// Global upload speed limit (bytes/sec). 0 = unlimited.
    pub fn upload_rate_limit(mut self, bytes_per_sec: u64) -> Self {
        self.config.session.upload_rate_limit = bytes_per_sec;
        self
    }

    /// Maximum concurrent piece downloads per torrent.
    pub fn max_active_seeding(mut self, n: usize) -> Self {
        self.config.session.max_active_seeding = n;
        self
    }

    /// Port range for random listen port selection.
    pub fn port_range(mut self, min: u16, max: u16) -> Self {
        self.config.obfuscation.port_range = (min, max);
        self
    }

    /// Connection handshake timeout.
    pub fn handshake_timeout(mut self, duration: Duration) -> Self {
        self.config.connection.handshake_timeout = duration;
        self
    }

    /// Number of tracker peers to request per announce.
    pub fn tracker_numwant(mut self, n: u32) -> Self {
        self.config.tracker.numwant = n;
        self
    }

    // ── Subsystem config replacement ────────────────────────────────

    /// Replace the entire session config.
    pub fn session_config(mut self, cfg: SessionConfig) -> Self {
        self.config.session = cfg;
        self
    }

    /// Replace the entire peer pool config.
    pub fn peer_pool_config(mut self, cfg: PeerPoolConfig) -> Self {
        self.config.peer_pool = cfg;
        self
    }

    /// Replace the entire affinity config.
    pub fn affinity_config(mut self, cfg: AffinityConfig) -> Self {
        self.config.affinity = cfg;
        self
    }

    /// Replace the entire validator config.
    pub fn validator_config(mut self, cfg: ValidatorConfig) -> Self {
        self.config.validator = cfg;
        self
    }

    /// Replace the entire bridge config.
    pub fn bridge_config(mut self, cfg: BridgeConfig) -> Self {
        self.config.bridge = cfg;
        self
    }

    /// Replace the entire streaming buffer policy.
    pub fn streaming_policy(mut self, policy: BufferPolicy) -> Self {
        self.config.streaming = policy;
        self
    }

    /// Replace the entire obfuscation config.
    pub fn obfuscation_config(mut self, cfg: ObfuscationConfig) -> Self {
        self.config.obfuscation = cfg;
        self
    }

    /// Replace the entire connection config.
    pub fn connection_config(mut self, cfg: ConnectionConfig) -> Self {
        self.config.connection = cfg;
        self
    }

    /// Replace the entire tracker config.
    pub fn tracker_config(mut self, cfg: TrackerConfig) -> Self {
        self.config.tracker = cfg;
        self
    }

    /// Replace the entire relay config.
    pub fn relay_config(mut self, cfg: RelayConfig) -> Self {
        self.config.relay = cfg;
        self
    }

    /// Replace the entire endgame config.
    pub fn endgame_config(mut self, cfg: EndgameConfig) -> Self {
        self.config.endgame = cfg;
        self
    }

    /// Replace the entire choking config.
    pub fn choking_config(mut self, cfg: ChokingConfig) -> Self {
        self.config.choking = cfg;
        self
    }

    /// Replace the entire upload config.
    pub fn upload_config(mut self, cfg: UploadConfig) -> Self {
        self.config.upload = cfg;
        self
    }

    /// Replace the entire fast extension config.
    pub fn fast_extension_config(mut self, cfg: FastExtensionConfig) -> Self {
        self.config.fast_extension = cfg;
        self
    }

    /// Replace the entire PEX config.
    pub fn pex_config(mut self, cfg: PexConfig) -> Self {
        self.config.pex = cfg;
        self
    }

    /// Replace the entire web seed config.
    pub fn web_seed_config(mut self, cfg: WebSeedConfig) -> Self {
        self.config.web_seed = cfg;
        self
    }

    /// Replace the entire local discovery config.
    pub fn local_discovery_config(mut self, cfg: LocalDiscoveryConfig) -> Self {
        self.config.local_discovery = cfg;
        self
    }

    /// Replace the entire metadata exchange config.
    pub fn metadata_exchange_config(mut self, cfg: MetadataExchangeConfig) -> Self {
        self.config.metadata_exchange = cfg;
        self
    }

    /// Replace the entire super-seeding config.
    pub fn super_seeding_config(mut self, cfg: SuperSeedingConfig) -> Self {
        self.config.super_seeding = cfg;
        self
    }

    /// Replace the entire content discovery config.
    pub fn content_discovery_config(mut self, cfg: ContentDiscoveryConfig) -> Self {
        self.config.content_discovery = cfg;
        self
    }
}

impl Default for ConfigBuilder {
    fn default() -> Self {
        Self::new()
    }
}

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── Default construction ────────────────────────────────────────

    /// Default config has all features enabled.
    ///
    /// Users who don't customise should get the full-featured experience.
    #[test]
    fn default_config_has_all_features_enabled() {
        let config = Config::default();
        let ft = &config.features;
        assert!(ft.dht);
        assert!(ft.pex);
        assert!(ft.local_discovery);
        assert!(ft.obfuscation);
        assert!(ft.merkle_verification);
        assert!(ft.web_seeds);
        assert!(ft.streaming);
        assert!(ft.super_seeding);
        assert!(ft.fast_extension);
        assert!(ft.metadata_exchange);
        assert!(ft.tracker_announce);
        assert!(ft.upload);
        assert!(ft.endgame);
        assert!(ft.rate_limiting);
        assert!(ft.relay);
        assert!(ft.bridge);
        assert!(ft.resume);
        assert!(ft.choking);
        assert!(ft.credit_system);
        assert!(ft.corruption_tracking);
    }

    /// Default config has sensible subsystem defaults.
    ///
    /// Spot-check key subsystem parameters against their documented defaults.
    #[test]
    fn default_config_has_sensible_subsystem_defaults() {
        let config = Config::default();
        assert_eq!(config.session.max_active_downloads, 3);
        assert_eq!(config.session.download_rate_limit, 0);
        assert_eq!(config.peer_pool.max_peers, 55);
        assert_eq!(config.validator.max_retries, 3);
        assert!(config.validator.use_merkle);
        assert_eq!(config.obfuscation.port_range, (49152, 65535));
        assert_eq!(config.connection.handshake_timeout.as_secs(), 30);
        assert_eq!(config.tracker.numwant, 50);
        assert_eq!(config.relay.max_circuits_per_relay, 128);
        assert_eq!(config.endgame.peer_multiplier, 2);
        assert_eq!(config.choking.unchoke_slots, 4);
        assert_eq!(config.upload.max_slots, 4);
    }

    // ── Feature toggles ─────────────────────────────────────────────

    /// all_disabled() disables every feature.
    ///
    /// Important for security-conscious deployments that enable features
    /// explicitly.
    #[test]
    fn all_disabled_turns_everything_off() {
        let ft = FeatureToggles::all_disabled();
        assert!(!ft.dht);
        assert!(!ft.pex);
        assert!(!ft.local_discovery);
        assert!(!ft.obfuscation);
        assert!(!ft.merkle_verification);
        assert!(!ft.web_seeds);
        assert!(!ft.streaming);
        assert!(!ft.super_seeding);
        assert!(!ft.fast_extension);
        assert!(!ft.metadata_exchange);
        assert!(!ft.tracker_announce);
        assert!(!ft.upload);
        assert!(!ft.endgame);
        assert!(!ft.rate_limiting);
        assert!(!ft.relay);
        assert!(!ft.bridge);
        assert!(!ft.resume);
        assert!(!ft.choking);
        assert!(!ft.credit_system);
        assert!(!ft.corruption_tracking);
    }

    /// to_capabilities() maps toggles to the handshake bitmap correctly.
    ///
    /// The handshake capabilities must reflect the feature toggles so peers
    /// know what we support.
    #[test]
    fn to_capabilities_maps_toggles() {
        let all = FeatureToggles::default();
        let caps = all.to_capabilities();
        assert!(caps.supports(crate::handshake::Capabilities::DHT));
        assert!(caps.supports(crate::handshake::Capabilities::PEX));
        assert!(caps.supports(crate::handshake::Capabilities::ENCRYPTION));

        let none = FeatureToggles::all_disabled();
        let caps = none.to_capabilities();
        assert_eq!(caps.to_u32(), 0);
    }

    // ── Builder ─────────────────────────────────────────────────────

    /// Builder applies feature toggles correctly.
    ///
    /// Each shortcut method must modify exactly the intended field.
    #[test]
    fn builder_applies_feature_toggles() {
        let config = Config::builder()
            .enable_dht(false)
            .enable_pex(false)
            .enable_obfuscation(false)
            .enable_upload(false)
            .build();

        assert!(!config.features.dht);
        assert!(!config.features.pex);
        assert!(!config.features.obfuscation);
        assert!(!config.features.upload);
        // Unmodified features remain enabled.
        assert!(config.features.web_seeds);
        assert!(config.features.streaming);
        assert!(config.features.endgame);
    }

    /// Builder shortcut methods modify subsystem configs.
    ///
    /// Common parameters (max_peers, rate limits) should be accessible
    /// without constructing subsystem config structs.
    #[test]
    fn builder_shortcut_methods() {
        let config = Config::builder()
            .max_peers(100)
            .max_active_downloads(5)
            .download_rate_limit(1_048_576)
            .upload_rate_limit(524_288)
            .port_range(10000, 20000)
            .handshake_timeout(Duration::from_secs(10))
            .tracker_numwant(200)
            .build();

        assert_eq!(config.peer_pool.max_peers, 100);
        assert_eq!(config.session.max_active_downloads, 5);
        assert_eq!(config.session.download_rate_limit, 1_048_576);
        assert_eq!(config.session.upload_rate_limit, 524_288);
        assert_eq!(config.obfuscation.port_range, (10000, 20000));
        assert_eq!(config.connection.handshake_timeout.as_secs(), 10);
        assert_eq!(config.tracker.numwant, 200);
    }

    /// Builder subsystem replacement overwrites the entire config.
    ///
    /// When a user provides a custom subsystem config, it must completely
    /// replace the default — no merging.
    #[test]
    fn builder_subsystem_replacement() {
        let custom_pool = PeerPoolConfig {
            max_peers: 200,
            ..PeerPoolConfig::default()
        };
        let config = Config::builder().peer_pool_config(custom_pool).build();

        assert_eq!(config.peer_pool.max_peers, 200);
    }

    // ── Obfuscation config ──────────────────────────────────────────

    /// Default obfuscation config matches IANA ephemeral range.
    ///
    /// The port range must default to IANA (49152–65535) to avoid
    /// conflicting with well-known or registered ports.
    #[test]
    fn obfuscation_config_default_port_range() {
        let cfg = ObfuscationConfig::default();
        assert_eq!(cfg.port_range, (49152, 65535));
        assert!(!cfg.require_obfuscation);
        assert!(cfg.prefer_obfuscated);
    }

    // ── Connection config ───────────────────────────────────────────

    /// Default connection config matches BT spec recommendations.
    ///
    /// Handshake timeout of 30s and keep-alive interval of 120s are
    /// standard across most BT implementations.
    #[test]
    fn connection_config_defaults() {
        let cfg = ConnectionConfig::default();
        assert_eq!(cfg.handshake_timeout.as_secs(), 30);
        assert_eq!(cfg.keepalive_interval.as_secs(), 120);
    }

    // ── Tracker config ──────────────────────────────────────────────

    /// Default tracker config matches standard BEP values.
    ///
    /// 1800s announce interval, 50 peers per request, 5-failure tolerance.
    #[test]
    fn tracker_config_defaults() {
        let cfg = TrackerConfig::default();
        assert_eq!(cfg.default_announce_interval.as_secs(), 1800);
        assert_eq!(cfg.min_announce_interval.as_secs(), 60);
        assert_eq!(cfg.max_failures, 5);
        assert_eq!(cfg.numwant, 50);
    }

    // ── Relay config ────────────────────────────────────────────────

    /// Default relay config provides reasonable circuit limits.
    ///
    /// 128 circuits per relay with 256 KB/s per-circuit cap prevents
    /// a relay from being overwhelmed.
    #[test]
    fn relay_config_defaults() {
        let cfg = RelayConfig::default();
        assert_eq!(cfg.max_circuits_per_relay, 128);
        assert_eq!(cfg.circuit_bandwidth_limit, 256 * 1024);
        assert_eq!(cfg.max_relay_nodes, 64);
    }

    // ── Endgame config ──────────────────────────────────────────────

    /// Default endgame config uses moderate aggressiveness.
    ///
    /// 2x peer multiplier means each remaining piece is requested
    /// from 2 additional peers, balancing speed against waste.
    #[test]
    fn endgame_config_defaults() {
        let cfg = EndgameConfig::default();
        assert_eq!(cfg.peer_multiplier, 2);
        assert_eq!(cfg.min_remaining, 1);
    }

    // ── Choking config ──────────────────────────────────────────────

    /// Default choking config matches BT standard (4 unchoke slots).
    ///
    /// 4 regular slots + 1 optimistic = 5 peers receiving data, rotating
    /// every 3 rounds (30 seconds at standard 10s evaluation interval).
    #[test]
    fn choking_config_defaults() {
        let cfg = ChokingConfig::default();
        assert_eq!(cfg.unchoke_slots, 4);
        assert!(!cfg.auto_scale_slots);
        assert_eq!(cfg.optimistic_interval, 3);
    }

    // ── Upload config ───────────────────────────────────────────────

    /// Default upload config provides 4 concurrent slots.
    ///
    /// 4 slots with 20 queue depth matches XDCC-inspired upload queue
    /// dimensions.
    #[test]
    fn upload_config_defaults() {
        let cfg = UploadConfig::default();
        assert_eq!(cfg.max_slots, 4);
        assert_eq!(cfg.max_queue, 20);
    }

    // ── Config composition ──────────────────────────────────────────

    /// Download-only config disables all upload-related features.
    ///
    /// A consumer that only wants to download should be able to cleanly
    /// disable all seeding/upload features.
    #[test]
    fn download_only_config() {
        let config = Config::builder()
            .enable_upload(false)
            .enable_super_seeding(false)
            .enable_choking(false)
            .max_active_seeding(0)
            .build();

        assert!(!config.features.upload);
        assert!(!config.features.super_seeding);
        assert!(!config.features.choking);
        assert_eq!(config.session.max_active_seeding, 0);
    }

    /// Minimal config disables everything except HTTP web seeds.
    ///
    /// The simplest possible configuration: HTTP download only, no P2P.
    #[test]
    fn minimal_http_only_config() {
        let config = Config::builder()
            .features(FeatureToggles::all_disabled())
            .enable_web_seeds(true)
            .build();

        assert!(config.features.web_seeds);
        assert!(!config.features.dht);
        assert!(!config.features.pex);
        assert!(!config.features.obfuscation);
        assert!(!config.features.upload);
    }

    /// LAN config disables internet-facing features.
    ///
    /// For local network deployments: enable LPD, disable DHT/trackers/relay.
    #[test]
    fn lan_config() {
        let config = Config::builder()
            .enable_dht(false)
            .enable_tracker_announce(false)
            .enable_relay(false)
            .enable_local_discovery(true)
            .build();

        assert!(!config.features.dht);
        assert!(!config.features.tracker_announce);
        assert!(!config.features.relay);
        assert!(config.features.local_discovery);
        assert!(config.features.pex); // PEX still useful on LAN
    }
}
