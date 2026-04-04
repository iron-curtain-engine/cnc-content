// SPDX-License-Identifier: MIT OR Apache-2.0

//! Peer trait and supporting types — unified interface for any piece source.
//!
//! Both HTTP web seeds and BitTorrent swarm peers implement [`Peer`]. The
//! coordinator treats all peers equally: it assigns pieces to the fastest
//! available peer that has the requested piece.

use std::fmt;
use std::io;

use thiserror::Error;

// ── Rejection reasons ───────────────────────────────────────────────

/// Structured rejection reason from a peer.
///
/// When a peer actively refuses a piece request (as opposed to silently failing
/// or timing out), the reason tells the coordinator whether and when to retry.
///
/// ## Design
///
/// Maps to IRC's numeric error codes (e.g. `ERR_CHANNELISFULL`) and the IC wire
/// protocol's planned `ic_reject` extension message (D049). Each variant carries
/// enough semantics for the coordinator to make an intelligent retry decision
/// without understanding the underlying transport.
///
/// - `RateLimited`, `SwarmFull` — transient, retry after backoff.
/// - `InsufficientAuth`, `PolicyViolation` — semi-permanent, retry only after
///   re-authentication or policy change.
/// - `Maintenance` — permanent for the session, do not retry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RejectionReason {
    /// Peer is rate-limiting requests — try again after a delay.
    RateLimited,
    /// Peer's connection slots are full — try again later.
    SwarmFull,
    /// Authentication required or insufficient — cannot proceed without
    /// valid credentials.
    InsufficientAuth,
    /// Request violates peer's policy (e.g., banned content hash, blocked
    /// IP range). Retrying won't help.
    PolicyViolation,
    /// Peer is going offline for maintenance — do not retry this session.
    Maintenance,
    /// Catch-all for peer-specific reasons not in the above categories.
    Other(String),
}

impl fmt::Display for RejectionReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::RateLimited => write!(f, "rate limited"),
            Self::SwarmFull => write!(f, "swarm full"),
            Self::InsufficientAuth => write!(f, "insufficient authentication"),
            Self::PolicyViolation => write!(f, "policy violation"),
            Self::Maintenance => write!(f, "peer maintenance"),
            Self::Other(detail) => write!(f, "{detail}"),
        }
    }
}

// ── Peer capabilities ───────────────────────────────────────────────

/// Operational capabilities advertised by a peer.
///
/// Peers can declare their limits and features so the coordinator can make
/// informed scheduling decisions. Maps to IRC's `RPL_ISUPPORT (005)` feature
/// advertisement and the IC wire protocol's planned extension handshake
/// fields (D049).
///
/// All fields are optional — a peer that doesn't advertise capabilities
/// uses the defaults (no limits, all features assumed available).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PeerCapabilities {
    /// Maximum upload rate this peer will provide, in bytes/sec.
    /// `None` means unknown / no limit.
    pub max_upload_rate: Option<u64>,
    /// Maximum number of concurrent piece requests this peer accepts.
    /// `None` means unknown / use protocol default (BEP 3: reqq=128).
    pub max_concurrent_requests: Option<u32>,
    /// Number of pieces this peer has available (from bitfield exchange).
    /// `None` means unknown (web seeds implicitly have all pieces).
    pub announced_piece_count: Option<u32>,
    /// Whether this peer supports priority piece requests (ic_priority).
    /// `false` means the peer only supports standard BEP 3 requests.
    pub supports_priority: bool,
}

// ── Peer kind ───────────────────────────────────────────────────────

/// Identifies the kind of peer for logging, metrics, and strategy selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PeerKind {
    /// HTTP web seed (BEP 19) — always has all pieces, never choked.
    /// Each URL serves the complete file; the coordinator uses HTTP Range
    /// requests to fetch individual pieces.
    WebSeed,
    /// BitTorrent swarm — wraps a BT client's peer set as one logical
    /// peer. Piece availability and choke state depend on connected
    /// swarm members.
    BtSwarm,
}

// ── Peer errors ─────────────────────────────────────────────────────

/// Errors from individual peer operations.
#[derive(Debug, Error)]
pub enum PeerError {
    #[error("HTTP error fetching piece {piece_index} from {url}: {detail}")]
    Http {
        piece_index: u32,
        url: String,
        detail: String,
    },
    #[error("BT swarm error for piece {piece_index}: {detail}")]
    BtSwarm { piece_index: u32, detail: String },
    #[error("peer timeout for piece {piece_index}")]
    Timeout { piece_index: u32 },
    #[error("peer rejected request for piece {piece_index}: {reason}")]
    Rejected {
        piece_index: u32,
        reason: RejectionReason,
    },
    #[error("I/O error: {source}")]
    Io {
        #[from]
        source: io::Error,
    },
}

// ── Peer trait ──────────────────────────────────────────────────────

/// Unified interface for any piece source in the coordinator.
///
/// Both HTTP web seeds and the BitTorrent swarm implement this trait.
/// The coordinator treats all peers equally — it assigns pieces to the
/// fastest available peer that has the requested piece.
///
/// ## Contract
///
/// - `has_piece()` must be accurate. Web seeds always return `true` (they
///   serve the complete file). BT swarm peers return the current bitfield.
/// - `fetch_piece()` must return exactly `length` bytes (or fewer for the
///   last piece of the file). The data must correspond to the piece at the
///   given file offset.
/// - Implementations must be safe to call from multiple threads.
pub trait Peer: Send + Sync {
    /// Returns what kind of peer this is (for logging and metrics).
    fn kind(&self) -> PeerKind;

    /// Whether this peer has a specific piece available for download.
    ///
    /// Web seeds always return `true`. BT swarm peers check the bitfield
    /// received from the swarm.
    fn has_piece(&self, piece_index: u32) -> bool;

    /// Whether this peer is currently choked (cannot accept requests).
    ///
    /// Web seeds are never choked. BT swarm peers may be choked by the
    /// remote peer.
    fn is_choked(&self) -> bool;

    /// Fetches piece data from this peer.
    ///
    /// - `piece_index`: which piece number (0-based)
    /// - `offset`: byte offset within the file where this piece starts
    /// - `length`: expected piece length in bytes
    ///
    /// Returns the raw piece bytes. The coordinator will SHA-1 verify them.
    fn fetch_piece(&self, piece_index: u32, offset: u64, length: u32)
        -> Result<Vec<u8>, PeerError>;

    /// Estimated download speed from this peer in bytes/sec.
    ///
    /// Used by the coordinator to prefer faster peers when multiple peers
    /// have the same piece. Returns 0 if unknown.
    fn speed_estimate(&self) -> u64;

    /// Returns this peer's advertised operational capabilities.
    ///
    /// Used by the coordinator to respect peer limits (upload cap, max
    /// concurrent requests) and to prefer peers that support priority
    /// requests. The default returns `PeerCapabilities::default()` (no
    /// limits, no special features).
    fn capabilities(&self) -> PeerCapabilities {
        PeerCapabilities::default()
    }

    /// Returns a stable identifier for this peer across reconnections.
    ///
    /// Used to track peers across connection drops. For BT peers this
    /// is typically the 20-byte peer ID; for web seeds it could be the URL.
    /// Returns `None` if the peer has no stable identity.
    fn peer_id(&self) -> Option<&str> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── RejectionReason Display ─────────────────────────────────────

    /// Every `RejectionReason` variant produces a non-empty, meaningful message.
    ///
    /// Rejection reasons are shown to users in error diagnostics. Each must
    /// be human-readable and distinguish itself from other variants.
    #[test]
    fn rejection_reason_display_all_variants() {
        let cases = [
            (RejectionReason::RateLimited, "rate limited"),
            (RejectionReason::SwarmFull, "swarm full"),
            (
                RejectionReason::InsufficientAuth,
                "insufficient authentication",
            ),
            (RejectionReason::PolicyViolation, "policy violation"),
            (RejectionReason::Maintenance, "peer maintenance"),
            (
                RejectionReason::Other("custom reason".into()),
                "custom reason",
            ),
        ];
        for (reason, expected_substr) in &cases {
            let msg = reason.to_string();
            assert!(
                msg.contains(expected_substr),
                "{reason:?} display should contain '{expected_substr}': got '{msg}'"
            );
        }
    }

    /// `RejectionReason::Other` carries the detail string through Display.
    #[test]
    fn rejection_reason_other_carries_detail() {
        let reason = RejectionReason::Other("peer is on fire".into());
        assert_eq!(reason.to_string(), "peer is on fire");
    }

    // ── PeerError Display ───────────────────────────────────────────

    /// `PeerError::Http` includes piece index, URL, and detail.
    #[test]
    fn peer_error_http_display() {
        let err = PeerError::Http {
            piece_index: 42,
            url: "https://mirror.example.com/data.zip".into(),
            detail: "HTTP 503 Service Unavailable".into(),
        };
        let msg = err.to_string();
        assert!(msg.contains("42"), "should contain piece index: {msg}");
        assert!(
            msg.contains("mirror.example.com"),
            "should contain URL: {msg}"
        );
        assert!(msg.contains("503"), "should contain HTTP status: {msg}");
    }

    /// `PeerError::BtSwarm` includes piece index and detail.
    #[test]
    fn peer_error_bt_swarm_display() {
        let err = PeerError::BtSwarm {
            piece_index: 7,
            detail: "peer disconnected mid-transfer".into(),
        };
        let msg = err.to_string();
        assert!(msg.contains("7"), "should contain piece index: {msg}");
        assert!(msg.contains("disconnected"), "should contain detail: {msg}");
    }

    /// `PeerError::Timeout` includes piece index.
    #[test]
    fn peer_error_timeout_display() {
        let err = PeerError::Timeout { piece_index: 99 };
        let msg = err.to_string();
        assert!(msg.contains("99"), "should contain piece index: {msg}");
        assert!(msg.contains("timeout"), "should mention timeout: {msg}");
    }

    /// `PeerError::Rejected` includes piece index and rejection reason.
    #[test]
    fn peer_error_rejected_display() {
        let err = PeerError::Rejected {
            piece_index: 3,
            reason: RejectionReason::SwarmFull,
        };
        let msg = err.to_string();
        assert!(msg.contains("3"), "should contain piece index: {msg}");
        assert!(msg.contains("swarm full"), "should contain reason: {msg}");
    }

    /// `PeerError::Io` wraps the underlying I/O error.
    #[test]
    fn peer_error_io_display() {
        let err = PeerError::Io {
            source: io::Error::new(io::ErrorKind::ConnectionReset, "connection reset by peer"),
        };
        let msg = err.to_string();
        assert!(
            msg.contains("connection reset"),
            "should contain source: {msg}"
        );
    }

    // ── PeerCapabilities ────────────────────────────────────────────

    /// Default `PeerCapabilities` has no limits and no special features.
    #[test]
    fn capabilities_default_no_limits() {
        let caps = PeerCapabilities::default();
        assert!(caps.max_upload_rate.is_none());
        assert!(caps.max_concurrent_requests.is_none());
        assert!(caps.announced_piece_count.is_none());
        assert!(!caps.supports_priority);
    }

    // ── RejectionReason equality ────────────────────────────────────

    /// `RejectionReason` variants are distinguishable via PartialEq.
    #[test]
    fn rejection_reason_equality() {
        assert_eq!(RejectionReason::RateLimited, RejectionReason::RateLimited);
        assert_ne!(RejectionReason::RateLimited, RejectionReason::SwarmFull);
        assert_ne!(
            RejectionReason::Other("a".into()),
            RejectionReason::Other("b".into())
        );
        assert_eq!(
            RejectionReason::Other("same".into()),
            RejectionReason::Other("same".into())
        );
    }
}
