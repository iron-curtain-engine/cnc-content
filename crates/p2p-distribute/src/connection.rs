// SPDX-License-Identifier: MIT OR Apache-2.0

//! Connection state machine — tracks peer connection lifecycle phases.
//!
//! ## What
//!
//! Models the lifecycle of a single BitTorrent peer connection from TCP
//! establishment through handshake exchange, bitfield transfer, and into
//! the steady-state data transfer phase. Each phase has specific valid
//! transitions enforced at the type level.
//!
//! ## Why — explicit state prevents protocol violations
//!
//! Without a formal connection state machine, the coordinator must rely on
//! implicit state: "did we receive a handshake yet?" is a boolean flag,
//! "are we choked?" is another flag, "have we sent our bitfield?" is yet
//! another. These flags form a latent state machine that's easy to
//! mishandle — sending a piece request before the handshake is complete,
//! or sending interest before the bitfield exchange, violates the protocol
//! and causes peer disconnection.
//!
//! By encoding each phase as a distinct enum variant, the connection state
//! machine makes invalid transitions impossible: you cannot access the
//! `Ready` state's choke/interest methods until handshake and bitfield
//! exchange are complete.
//!
//! ## How
//!
//! - [`ConnectionPhase`]: Enum of lifecycle phases (Connecting, Handshaking,
//!   BitfieldExchange, Ready, Closing, Closed).
//! - [`ConnectionState`]: Manages the current phase plus choke/interest
//!   state for the Ready phase.
//! - Transitions are methods that return `Result<(), ConnectionError>`,
//!   failing if the current phase doesn't allow the transition.

use std::fmt;
use std::time::Instant;

use thiserror::Error;

// ── Constants ───────────────────────────────────────────────────────

/// Default connection timeout before handshake completes (30 seconds).
const DEFAULT_HANDSHAKE_TIMEOUT_SECS: u64 = 30;

/// Default keep-alive interval (2 minutes, per BEP 3 recommendation).
const DEFAULT_KEEPALIVE_INTERVAL_SECS: u64 = 120;

// ── Connection phase ────────────────────────────────────────────────

/// Lifecycle phase of a peer connection.
///
/// ```text
/// Connecting ──→ Handshaking ──→ BitfieldExchange ──→ Ready ──→ Closing ──→ Closed
///      │              │                  │                │
///      └──────────────┴──────────────────┴────────────────┘──→ Closing ──→ Closed
/// ```
///
/// Any active phase can transition to `Closing` on error or disconnect.
/// `Closed` is the terminal state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectionPhase {
    /// TCP connection is being established (outgoing) or just accepted
    /// (incoming). No protocol data exchanged yet.
    Connecting,

    /// The BEP 3 handshake is in progress. Both peers exchange:
    /// protocol string, reserved bytes, info hash, and peer ID.
    Handshaking,

    /// Handshake is complete. Peers exchange bitfield messages.
    /// In BEP 3, the bitfield is the first message after handshake.
    BitfieldExchange,

    /// Connection is fully established. Data transfer (request/piece),
    /// choking, interest, and extension messages are allowed.
    Ready,

    /// Connection is being torn down. A reason is preserved for diagnostics.
    Closing {
        /// Why the connection is closing.
        reason: CloseReason,
    },

    /// Terminal state: connection is fully closed.
    Closed,
}

impl fmt::Display for ConnectionPhase {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Connecting => write!(f, "connecting"),
            Self::Handshaking => write!(f, "handshaking"),
            Self::BitfieldExchange => write!(f, "bitfield exchange"),
            Self::Ready => write!(f, "ready"),
            Self::Closing { reason } => write!(f, "closing ({reason})"),
            Self::Closed => write!(f, "closed"),
        }
    }
}

// ── Close reason ────────────────────────────────────────────────────

/// Reason for connection closure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CloseReason {
    /// Normal shutdown (local decision to disconnect).
    Normal,
    /// Handshake timeout.
    HandshakeTimeout,
    /// Info hash mismatch during handshake.
    InfoHashMismatch,
    /// Protocol error (malformed message, unexpected message type).
    ProtocolError,
    /// Remote peer closed the connection.
    RemoteClosed,
    /// I/O error.
    IoError,
    /// Keep-alive timeout (no messages received in time).
    KeepAliveTimeout,
}

impl fmt::Display for CloseReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Normal => write!(f, "normal shutdown"),
            Self::HandshakeTimeout => write!(f, "handshake timeout"),
            Self::InfoHashMismatch => write!(f, "info hash mismatch"),
            Self::ProtocolError => write!(f, "protocol error"),
            Self::RemoteClosed => write!(f, "remote closed"),
            Self::IoError => write!(f, "I/O error"),
            Self::KeepAliveTimeout => write!(f, "keep-alive timeout"),
        }
    }
}

// ── Connection state ────────────────────────────────────────────────

/// Manages the full state of a peer connection.
///
/// Tracks the current phase, choke/interest state (for the Ready phase),
/// and timing information for timeouts.
///
/// ```
/// use p2p_distribute::connection::{ConnectionState, ConnectionPhase};
/// use std::time::Instant;
///
/// let now = Instant::now();
/// let mut conn = ConnectionState::new(now);
/// assert_eq!(conn.phase(), &ConnectionPhase::Connecting);
///
/// conn.transition_to_handshaking(now).unwrap();
/// assert_eq!(conn.phase(), &ConnectionPhase::Handshaking);
///
/// conn.transition_to_bitfield_exchange(now).unwrap();
/// conn.transition_to_ready(now).unwrap();
/// assert_eq!(conn.phase(), &ConnectionPhase::Ready);
/// ```
pub struct ConnectionState {
    /// Current connection phase.
    phase: ConnectionPhase,
    /// When this connection was created.
    created_at: Instant,
    /// When the phase last transitioned.
    last_transition: Instant,
    /// When we last received any message from the peer.
    last_message_received: Instant,
    /// Whether the local side is choking the remote peer.
    am_choking: bool,
    /// Whether the local side is interested in the remote peer's pieces.
    am_interested: bool,
    /// Whether the remote peer is choking us.
    peer_choking: bool,
    /// Whether the remote peer is interested in our pieces.
    peer_interested: bool,
    /// Whether the remote peer supports the BEP 6 Fast Extension.
    fast_extension: bool,
}

impl ConnectionState {
    /// Creates a new connection in the `Connecting` phase.
    pub fn new(now: Instant) -> Self {
        Self {
            phase: ConnectionPhase::Connecting,
            created_at: now,
            last_transition: now,
            last_message_received: now,
            // BEP 3: connections start choked and not interested.
            am_choking: true,
            am_interested: false,
            peer_choking: true,
            peer_interested: false,
            fast_extension: false,
        }
    }

    /// Returns the current connection phase.
    pub fn phase(&self) -> &ConnectionPhase {
        &self.phase
    }

    /// Returns when the connection was created.
    pub fn created_at(&self) -> Instant {
        self.created_at
    }

    /// Returns when the last phase transition occurred.
    pub fn last_transition(&self) -> Instant {
        self.last_transition
    }

    /// Returns whether the connection is in the Ready phase.
    pub fn is_ready(&self) -> bool {
        self.phase == ConnectionPhase::Ready
    }

    /// Returns whether the connection is closed or closing.
    pub fn is_terminal(&self) -> bool {
        matches!(
            self.phase,
            ConnectionPhase::Closing { .. } | ConnectionPhase::Closed
        )
    }

    // ── Phase transitions ───────────────────────────────────────────

    /// Transitions from Connecting to Handshaking.
    pub fn transition_to_handshaking(&mut self, now: Instant) -> Result<(), ConnectionError> {
        if self.phase != ConnectionPhase::Connecting {
            return Err(ConnectionError::InvalidTransition {
                from: self.phase.to_string(),
                to: "handshaking".into(),
            });
        }
        self.phase = ConnectionPhase::Handshaking;
        self.last_transition = now;
        Ok(())
    }

    /// Transitions from Handshaking to BitfieldExchange.
    pub fn transition_to_bitfield_exchange(&mut self, now: Instant) -> Result<(), ConnectionError> {
        if self.phase != ConnectionPhase::Handshaking {
            return Err(ConnectionError::InvalidTransition {
                from: self.phase.to_string(),
                to: "bitfield exchange".into(),
            });
        }
        self.phase = ConnectionPhase::BitfieldExchange;
        self.last_transition = now;
        Ok(())
    }

    /// Transitions from BitfieldExchange to Ready.
    pub fn transition_to_ready(&mut self, now: Instant) -> Result<(), ConnectionError> {
        if self.phase != ConnectionPhase::BitfieldExchange {
            return Err(ConnectionError::InvalidTransition {
                from: self.phase.to_string(),
                to: "ready".into(),
            });
        }
        self.phase = ConnectionPhase::Ready;
        self.last_transition = now;
        Ok(())
    }

    /// Transitions to Closing from any active phase.
    pub fn transition_to_closing(
        &mut self,
        reason: CloseReason,
        now: Instant,
    ) -> Result<(), ConnectionError> {
        if self.is_terminal() {
            return Err(ConnectionError::InvalidTransition {
                from: self.phase.to_string(),
                to: "closing".into(),
            });
        }
        self.phase = ConnectionPhase::Closing { reason };
        self.last_transition = now;
        Ok(())
    }

    /// Transitions from Closing to Closed.
    pub fn transition_to_closed(&mut self, now: Instant) -> Result<(), ConnectionError> {
        if !matches!(self.phase, ConnectionPhase::Closing { .. }) {
            return Err(ConnectionError::InvalidTransition {
                from: self.phase.to_string(),
                to: "closed".into(),
            });
        }
        self.phase = ConnectionPhase::Closed;
        self.last_transition = now;
        Ok(())
    }

    // ── Choke / interest ────────────────────────────────────────────

    /// Sets the local choking state.
    pub fn set_am_choking(&mut self, choking: bool) {
        self.am_choking = choking;
    }

    /// Returns whether the local side is choking.
    pub fn am_choking(&self) -> bool {
        self.am_choking
    }

    /// Sets the local interest state.
    pub fn set_am_interested(&mut self, interested: bool) {
        self.am_interested = interested;
    }

    /// Returns whether the local side is interested.
    pub fn am_interested(&self) -> bool {
        self.am_interested
    }

    /// Records that the remote peer changed its choking state.
    pub fn set_peer_choking(&mut self, choking: bool) {
        self.peer_choking = choking;
    }

    /// Returns whether the remote peer is choking us.
    pub fn peer_choking(&self) -> bool {
        self.peer_choking
    }

    /// Records that the remote peer changed its interest state.
    pub fn set_peer_interested(&mut self, interested: bool) {
        self.peer_interested = interested;
    }

    /// Returns whether the remote peer is interested.
    pub fn peer_interested(&self) -> bool {
        self.peer_interested
    }

    /// Returns whether we can send requests (peer is not choking us
    /// and we are interested).
    pub fn can_request(&self) -> bool {
        self.is_ready() && !self.peer_choking && self.am_interested
    }

    /// Returns whether the peer can request from us (we are not choking
    /// them and they are interested).
    pub fn can_serve(&self) -> bool {
        self.is_ready() && !self.am_choking && self.peer_interested
    }

    // ── Fast extension ──────────────────────────────────────────────

    /// Records that the peer supports the BEP 6 Fast Extension.
    pub fn set_fast_extension(&mut self, supported: bool) {
        self.fast_extension = supported;
    }

    /// Returns whether the peer supports the BEP 6 Fast Extension.
    pub fn supports_fast_extension(&self) -> bool {
        self.fast_extension
    }

    // ── Timing ──────────────────────────────────────────────────────

    /// Records that a message was received from the peer.
    pub fn record_message_received(&mut self, now: Instant) {
        self.last_message_received = now;
    }

    /// Returns whether the handshake has timed out.
    pub fn is_handshake_timed_out(&self, now: Instant) -> bool {
        if !matches!(
            self.phase,
            ConnectionPhase::Connecting | ConnectionPhase::Handshaking
        ) {
            return false;
        }
        now.duration_since(self.created_at).as_secs() > DEFAULT_HANDSHAKE_TIMEOUT_SECS
    }

    /// Returns whether the keep-alive has timed out.
    pub fn is_keepalive_timed_out(&self, now: Instant) -> bool {
        if !self.is_ready() {
            return false;
        }
        now.duration_since(self.last_message_received).as_secs()
            > DEFAULT_KEEPALIVE_INTERVAL_SECS.saturating_mul(2)
    }

    /// Returns the connection age.
    pub fn age(&self, now: Instant) -> std::time::Duration {
        now.duration_since(self.created_at)
    }
}

// ── Errors ──────────────────────────────────────────────────────────

/// Error from an invalid connection state transition.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ConnectionError {
    /// The requested transition is not valid from the current phase.
    #[error("invalid transition from '{from}' to '{to}'")]
    InvalidTransition {
        /// Current phase name.
        from: String,
        /// Attempted target phase name.
        to: String,
    },
}

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    // ── Happy path ──────────────────────────────────────────────────

    /// Full lifecycle: Connecting → Handshaking → Bitfield → Ready → Closing → Closed.
    ///
    /// This is the normal connection lifecycle for a successful peer session.
    #[test]
    fn full_lifecycle() {
        let now = Instant::now();
        let mut conn = ConnectionState::new(now);

        assert_eq!(conn.phase(), &ConnectionPhase::Connecting);
        assert!(!conn.is_ready());
        assert!(!conn.is_terminal());

        conn.transition_to_handshaking(now).unwrap();
        assert_eq!(conn.phase(), &ConnectionPhase::Handshaking);

        conn.transition_to_bitfield_exchange(now).unwrap();
        assert_eq!(conn.phase(), &ConnectionPhase::BitfieldExchange);

        conn.transition_to_ready(now).unwrap();
        assert!(conn.is_ready());

        conn.transition_to_closing(CloseReason::Normal, now)
            .unwrap();
        assert!(conn.is_terminal());

        conn.transition_to_closed(now).unwrap();
        assert_eq!(conn.phase(), &ConnectionPhase::Closed);
    }

    // ── Invalid transitions ─────────────────────────────────────────

    /// Cannot skip handshake phase.
    #[test]
    fn cannot_skip_handshake() {
        let now = Instant::now();
        let mut conn = ConnectionState::new(now);
        let result = conn.transition_to_bitfield_exchange(now);
        assert!(matches!(
            result,
            Err(ConnectionError::InvalidTransition { .. })
        ));
    }

    /// Cannot skip bitfield exchange.
    #[test]
    fn cannot_skip_bitfield() {
        let now = Instant::now();
        let mut conn = ConnectionState::new(now);
        conn.transition_to_handshaking(now).unwrap();
        let result = conn.transition_to_ready(now);
        assert!(matches!(
            result,
            Err(ConnectionError::InvalidTransition { .. })
        ));
    }

    /// Cannot transition to Closing from Closed.
    #[test]
    fn cannot_close_when_closed() {
        let now = Instant::now();
        let mut conn = ConnectionState::new(now);
        conn.transition_to_closing(CloseReason::Normal, now)
            .unwrap();
        conn.transition_to_closed(now).unwrap();

        let result = conn.transition_to_closing(CloseReason::Normal, now);
        assert!(matches!(
            result,
            Err(ConnectionError::InvalidTransition { .. })
        ));
    }

    /// Cannot go to Closed without going through Closing.
    #[test]
    fn must_close_through_closing() {
        let now = Instant::now();
        let mut conn = ConnectionState::new(now);
        conn.transition_to_handshaking(now).unwrap();
        let result = conn.transition_to_closed(now);
        assert!(matches!(
            result,
            Err(ConnectionError::InvalidTransition { .. })
        ));
    }

    // ── Choke / interest ────────────────────────────────────────────

    /// Initial choke/interest state matches BEP 3 defaults.
    ///
    /// Connections start choked and not interested on both sides.
    #[test]
    fn initial_choke_interest() {
        let conn = ConnectionState::new(Instant::now());
        assert!(conn.am_choking());
        assert!(!conn.am_interested());
        assert!(conn.peer_choking());
        assert!(!conn.peer_interested());
    }

    /// can_request requires Ready + peer not choking + we are interested.
    #[test]
    fn can_request_conditions() {
        let now = Instant::now();
        let mut conn = ConnectionState::new(now);

        // Not ready — can't request.
        assert!(!conn.can_request());

        // Get to Ready.
        conn.transition_to_handshaking(now).unwrap();
        conn.transition_to_bitfield_exchange(now).unwrap();
        conn.transition_to_ready(now).unwrap();

        // Ready but peer is choking and we're not interested.
        assert!(!conn.can_request());

        // Set interested but still choked.
        conn.set_am_interested(true);
        assert!(!conn.can_request());

        // Peer unchokes us.
        conn.set_peer_choking(false);
        assert!(conn.can_request());
    }

    /// can_serve requires Ready + we are not choking + peer is interested.
    #[test]
    fn can_serve_conditions() {
        let now = Instant::now();
        let mut conn = ConnectionState::new(now);
        conn.transition_to_handshaking(now).unwrap();
        conn.transition_to_bitfield_exchange(now).unwrap();
        conn.transition_to_ready(now).unwrap();

        assert!(!conn.can_serve()); // We're choking, peer not interested.

        conn.set_am_choking(false);
        assert!(!conn.can_serve()); // Peer still not interested.

        conn.set_peer_interested(true);
        assert!(conn.can_serve());
    }

    // ── Closing from any phase ──────────────────────────────────────

    /// Can close from Connecting.
    #[test]
    fn close_from_connecting() {
        let now = Instant::now();
        let mut conn = ConnectionState::new(now);
        conn.transition_to_closing(CloseReason::IoError, now)
            .unwrap();
        assert!(conn.is_terminal());
    }

    /// Can close from Handshaking.
    #[test]
    fn close_from_handshaking() {
        let now = Instant::now();
        let mut conn = ConnectionState::new(now);
        conn.transition_to_handshaking(now).unwrap();
        conn.transition_to_closing(CloseReason::InfoHashMismatch, now)
            .unwrap();
        assert!(conn.is_terminal());
    }

    // ── Timeout detection ───────────────────────────────────────────

    /// Handshake timeout detection.
    #[test]
    fn handshake_timeout() {
        let now = Instant::now();
        let conn = ConnectionState::new(now);

        assert!(!conn.is_handshake_timed_out(now));
        let later = now + Duration::from_secs(DEFAULT_HANDSHAKE_TIMEOUT_SECS + 1);
        assert!(conn.is_handshake_timed_out(later));
    }

    /// Keep-alive timeout detection (only in Ready phase).
    #[test]
    fn keepalive_timeout() {
        let now = Instant::now();
        let mut conn = ConnectionState::new(now);
        conn.transition_to_handshaking(now).unwrap();
        conn.transition_to_bitfield_exchange(now).unwrap();
        conn.transition_to_ready(now).unwrap();

        assert!(!conn.is_keepalive_timed_out(now));
        let later = now + Duration::from_secs(DEFAULT_KEEPALIVE_INTERVAL_SECS * 2 + 1);
        assert!(conn.is_keepalive_timed_out(later));
    }

    /// Keep-alive timeout is not triggered before Ready.
    #[test]
    fn keepalive_not_in_handshake() {
        let now = Instant::now();
        let conn = ConnectionState::new(now);
        let later = now + Duration::from_secs(9999);
        assert!(!conn.is_keepalive_timed_out(later));
    }

    // ── Fast extension ──────────────────────────────────────────────

    /// Fast extension flag tracking.
    #[test]
    fn fast_extension_tracking() {
        let mut conn = ConnectionState::new(Instant::now());
        assert!(!conn.supports_fast_extension());

        conn.set_fast_extension(true);
        assert!(conn.supports_fast_extension());
    }

    // ── Display ─────────────────────────────────────────────────────

    /// Phase display strings are human-readable.
    #[test]
    fn phase_display() {
        assert_eq!(ConnectionPhase::Connecting.to_string(), "connecting");
        assert_eq!(ConnectionPhase::Ready.to_string(), "ready");
        assert_eq!(
            ConnectionPhase::Closing {
                reason: CloseReason::Normal
            }
            .to_string(),
            "closing (normal shutdown)"
        );
    }

    /// Connection age tracks correctly.
    #[test]
    fn connection_age() {
        let now = Instant::now();
        let conn = ConnectionState::new(now);
        let later = now + Duration::from_secs(60);
        assert_eq!(conn.age(later), Duration::from_secs(60));
    }
}
