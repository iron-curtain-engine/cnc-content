// SPDX-License-Identifier: MIT OR Apache-2.0

//! BEP 6 Fast Extension — reduced round-trip peer messaging.
//!
//! ## What
//!
//! Implements the five additional message types from BEP 6 (Fast Extension):
//!
//! - **Suggest Piece** (0x0D): seeder hints that a piece is available in
//!   its disk cache and would be cheap to serve.
//! - **Have All** (0x0E): shorthand for "I have every piece" — replaces
//!   sending a full bitfield of all 1s.
//! - **Have None** (0x0F): shorthand for "I have nothing" — replaces
//!   sending a full bitfield of all 0s.
//! - **Reject Request** (0x10): explicit rejection of a request, allowing
//!   the requester to immediately retry elsewhere instead of waiting for
//!   a timeout.
//! - **Allowed Fast** (0x11): declares pieces that a peer will serve
//!   regardless of choke state, giving new leechers immediate data.
//!
//! ## Why — reducing round trips for seeders and new leechers
//!
//! Standard BEP 3 has two inefficiencies that BEP 6 addresses:
//!
//! 1. **Seeders waste bandwidth sending full bitfields.** A seeder with
//!    all pieces must transmit a `ceil(piece_count/8)` byte bitfield of
//!    all 1s. For a 4 GiB torrent with 256 KiB pieces, that's ~2 KiB per
//!    connection. `HaveAll` replaces this with 5 bytes.
//!
//! 2. **New leechers are stalled by choke.** In BEP 3, a newly-connected
//!    leecher must wait to be unchoked before receiving any data. With
//!    Allowed Fast, the seeder declares a set of pieces it will serve
//!    immediately, so the leecher can start downloading within the first
//!    round trip.
//!
//! 3. **Implicit rejections waste time.** When BEP 3 peers are choked,
//!    requests are silently ignored. The requester must wait for a timeout
//!    to detect this. `RejectRequest` makes rejection explicit.
//!
//! ## How
//!
//! - [`FastMessage`]: Enum for all five BEP 6 message types.
//! - [`AllowedFastSet`]: Manages the set of pieces a peer will serve
//!   while choking, with the standard BEP 6 generation algorithm.
//! - [`SuggestCache`]: Tracks suggested pieces from peers.

// ── Constants ───────────────────────────────────────────────────────

/// BEP 6 message ID for Suggest Piece.
pub const MSG_SUGGEST_PIECE: u8 = 0x0D;

/// BEP 6 message ID for Have All.
pub const MSG_HAVE_ALL: u8 = 0x0E;

/// BEP 6 message ID for Have None.
pub const MSG_HAVE_NONE: u8 = 0x0F;

/// BEP 6 message ID for Reject Request.
pub const MSG_REJECT_REQUEST: u8 = 0x10;

/// BEP 6 message ID for Allowed Fast.
pub const MSG_ALLOWED_FAST: u8 = 0x11;

/// Default number of allowed-fast pieces per peer.
const DEFAULT_ALLOWED_FAST_COUNT: usize = 10;

/// Maximum allowed-fast set size to prevent abuse.
const MAX_ALLOWED_FAST_COUNT: usize = 100;

/// Maximum number of suggestions tracked per peer.
const MAX_SUGGESTIONS: usize = 50;

// ── Fast messages ───────────────────────────────────────────────────

/// A BEP 6 Fast Extension message.
///
/// These are additional message types layered on top of the standard
/// BEP 3 wire protocol. Peers advertise support for the fast extension
/// via a reserved bit in the handshake.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FastMessage {
    /// Suggest Piece: the sender recommends downloading this piece.
    ///
    /// Typically sent by seeders for pieces in their disk/OS cache.
    SuggestPiece {
        /// Piece index being suggested.
        piece_index: u32,
    },

    /// Have All: the sender has every piece. Replaces a full bitfield.
    HaveAll,

    /// Have None: the sender has no pieces. Replaces an empty bitfield.
    HaveNone,

    /// Reject Request: explicitly rejects a previously-received request.
    ///
    /// Sent instead of silently dropping requests when choked.
    RejectRequest {
        /// Piece index of the rejected request.
        index: u32,
        /// Byte offset of the rejected block.
        begin: u32,
        /// Length of the rejected block.
        length: u32,
    },

    /// Allowed Fast: declares a piece that will be served regardless of
    /// choke state.
    AllowedFast {
        /// Piece index that is allowed-fast.
        piece_index: u32,
    },
}

impl FastMessage {
    /// Returns the BEP 6 message ID.
    pub fn message_id(&self) -> u8 {
        match self {
            Self::SuggestPiece { .. } => MSG_SUGGEST_PIECE,
            Self::HaveAll => MSG_HAVE_ALL,
            Self::HaveNone => MSG_HAVE_NONE,
            Self::RejectRequest { .. } => MSG_REJECT_REQUEST,
            Self::AllowedFast { .. } => MSG_ALLOWED_FAST,
        }
    }

    /// Encodes this message to its wire format (length-prefixed).
    pub fn encode(&self) -> Vec<u8> {
        match self {
            Self::SuggestPiece { piece_index } | Self::AllowedFast { piece_index } => {
                // Length: 1 (ID) + 4 (index) = 5.
                let mut buf = Vec::with_capacity(9);
                buf.extend_from_slice(&5u32.to_be_bytes());
                buf.push(self.message_id());
                buf.extend_from_slice(&piece_index.to_be_bytes());
                buf
            }
            Self::HaveAll | Self::HaveNone => {
                // Length: 1 (ID only).
                let mut buf = Vec::with_capacity(5);
                buf.extend_from_slice(&1u32.to_be_bytes());
                buf.push(self.message_id());
                buf
            }
            Self::RejectRequest {
                index,
                begin,
                length,
            } => {
                // Length: 1 (ID) + 12 (index + begin + length) = 13.
                let mut buf = Vec::with_capacity(17);
                buf.extend_from_slice(&13u32.to_be_bytes());
                buf.push(self.message_id());
                buf.extend_from_slice(&index.to_be_bytes());
                buf.extend_from_slice(&begin.to_be_bytes());
                buf.extend_from_slice(&length.to_be_bytes());
                buf
            }
        }
    }

    /// Decodes a BEP 6 message from a message ID and payload.
    ///
    /// The caller has already parsed the length prefix and extracted
    /// the message ID. This function handles the payload parsing.
    pub fn decode(msg_id: u8, payload: &[u8]) -> Result<Self, FastDecodeError> {
        match msg_id {
            MSG_SUGGEST_PIECE => {
                let index = read_piece_index(payload, msg_id)?;
                Ok(Self::SuggestPiece { piece_index: index })
            }
            MSG_HAVE_ALL => Ok(Self::HaveAll),
            MSG_HAVE_NONE => Ok(Self::HaveNone),
            MSG_REJECT_REQUEST => {
                if payload.len() < 12 {
                    return Err(FastDecodeError::PayloadTooShort {
                        msg_id,
                        needed: 12,
                        actual: payload.len() as u32,
                    });
                }
                let index = read_u32_at(payload, 0)?;
                let begin = read_u32_at(payload, 4)?;
                let length = read_u32_at(payload, 8)?;
                Ok(Self::RejectRequest {
                    index,
                    begin,
                    length,
                })
            }
            MSG_ALLOWED_FAST => {
                let index = read_piece_index(payload, msg_id)?;
                Ok(Self::AllowedFast { piece_index: index })
            }
            _ => Err(FastDecodeError::NotFastMessage { msg_id }),
        }
    }
}

/// Reads a 4-byte big-endian u32 from a payload, used for piece index.
fn read_piece_index(payload: &[u8], msg_id: u8) -> Result<u32, FastDecodeError> {
    if payload.len() < 4 {
        return Err(FastDecodeError::PayloadTooShort {
            msg_id,
            needed: 4,
            actual: payload.len() as u32,
        });
    }
    read_u32_at(payload, 0)
}

/// Reads a big-endian u32 at the given byte offset.
fn read_u32_at(data: &[u8], offset: usize) -> Result<u32, FastDecodeError> {
    let slice =
        data.get(offset..offset.saturating_add(4))
            .ok_or(FastDecodeError::PayloadTooShort {
                msg_id: 0,
                needed: offset.saturating_add(4) as u32,
                actual: data.len() as u32,
            })?;
    Ok(u32::from_be_bytes([
        *slice.first().unwrap_or(&0),
        *slice.get(1).unwrap_or(&0),
        *slice.get(2).unwrap_or(&0),
        *slice.get(3).unwrap_or(&0),
    ]))
}

// ── Allowed Fast set ────────────────────────────────────────────────

/// Manages the set of pieces a peer will serve regardless of choke state.
///
/// BEP 6 defines an algorithm for deterministic allowed-fast generation
/// based on the peer's IP and the torrent's info hash. This ensures both
/// sides agree on which pieces are allowed-fast without explicit
/// negotiation.
///
/// ```
/// use p2p_distribute::fast_extension::AllowedFastSet;
///
/// let mut afs = AllowedFastSet::new();
/// afs.add(42);
/// afs.add(99);
/// assert!(afs.contains(42));
/// assert!(!afs.contains(0));
/// assert_eq!(afs.count(), 2);
/// ```
pub struct AllowedFastSet {
    /// Piece indices in the allowed-fast set.
    pieces: Vec<u32>,
}

impl AllowedFastSet {
    /// Creates an empty allowed-fast set.
    pub fn new() -> Self {
        Self { pieces: Vec::new() }
    }

    /// Creates an allowed-fast set with a specific capacity.
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            pieces: Vec::with_capacity(capacity.min(MAX_ALLOWED_FAST_COUNT)),
        }
    }

    /// Adds a piece to the allowed-fast set.
    ///
    /// Returns `true` if the piece was added, `false` if it was already
    /// present or the set is at maximum capacity.
    pub fn add(&mut self, piece_index: u32) -> bool {
        if self.pieces.len() >= MAX_ALLOWED_FAST_COUNT {
            return false;
        }
        if self.pieces.contains(&piece_index) {
            return false;
        }
        self.pieces.push(piece_index);
        true
    }

    /// Returns whether a piece is in the allowed-fast set.
    pub fn contains(&self, piece_index: u32) -> bool {
        self.pieces.contains(&piece_index)
    }

    /// Returns the number of allowed-fast pieces.
    pub fn count(&self) -> usize {
        self.pieces.len()
    }

    /// Returns the default number of allowed-fast pieces.
    pub fn default_count() -> usize {
        DEFAULT_ALLOWED_FAST_COUNT
    }

    /// Returns the allowed-fast piece indices as a slice.
    pub fn pieces(&self) -> &[u32] {
        &self.pieces
    }

    /// Returns whether the set is empty.
    pub fn is_empty(&self) -> bool {
        self.pieces.is_empty()
    }

    /// Clears all pieces from the set.
    pub fn clear(&mut self) {
        self.pieces.clear();
    }
}

impl Default for AllowedFastSet {
    fn default() -> Self {
        Self::new()
    }
}

// ── Suggest cache ───────────────────────────────────────────────────

/// Tracks piece suggestions received from peers.
///
/// Peers (especially seeders) send Suggest Piece messages for pieces
/// that are cheap to serve (in their disk cache). The coordinator can
/// prefer these pieces for slightly better performance.
pub struct SuggestCache {
    /// Suggested piece indices, most recent last.
    suggestions: Vec<u32>,
}

impl SuggestCache {
    /// Creates an empty suggestion cache.
    pub fn new() -> Self {
        Self {
            suggestions: Vec::new(),
        }
    }

    /// Records a suggestion from a peer.
    pub fn add(&mut self, piece_index: u32) {
        // Remove if already present (will re-add at end for recency).
        self.suggestions.retain(|&p| p != piece_index);
        if self.suggestions.len() >= MAX_SUGGESTIONS {
            self.suggestions.remove(0);
        }
        self.suggestions.push(piece_index);
    }

    /// Returns whether a piece has been suggested.
    pub fn is_suggested(&self, piece_index: u32) -> bool {
        self.suggestions.contains(&piece_index)
    }

    /// Returns all suggested pieces (most recent last).
    pub fn suggestions(&self) -> &[u32] {
        &self.suggestions
    }

    /// Returns the number of suggestions.
    pub fn count(&self) -> usize {
        self.suggestions.len()
    }

    /// Clears all suggestions.
    pub fn clear(&mut self) {
        self.suggestions.clear();
    }
}

impl Default for SuggestCache {
    fn default() -> Self {
        Self::new()
    }
}

// ── Errors ──────────────────────────────────────────────────────────

/// Error decoding a BEP 6 Fast Extension message.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FastDecodeError {
    /// Payload is too short for the given message type.
    #[error("fast message {msg_id:#04x}: need {needed} bytes, got {actual}")]
    PayloadTooShort {
        /// The message ID being decoded.
        msg_id: u8,
        /// Minimum bytes needed.
        needed: u32,
        /// Actual bytes available.
        actual: u32,
    },

    /// The message ID is not a BEP 6 Fast Extension type.
    #[error("message ID {msg_id:#04x} is not a fast extension message")]
    NotFastMessage {
        /// The unrecognised message ID.
        msg_id: u8,
    },
}

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── Message round-trips ─────────────────────────────────────────

    /// SuggestPiece encodes and decodes correctly.
    #[test]
    fn suggest_piece_round_trip() {
        let msg = FastMessage::SuggestPiece { piece_index: 42 };
        let bytes = msg.encode();
        assert_eq!(bytes.len(), 9); // 4 + 1 + 4
                                    // Decode uses payload (after length prefix + ID).
        let payload = &bytes[5..];
        let decoded = FastMessage::decode(MSG_SUGGEST_PIECE, payload).unwrap();
        assert_eq!(decoded, msg);
    }

    /// HaveAll encodes and decodes correctly.
    #[test]
    fn have_all_round_trip() {
        let msg = FastMessage::HaveAll;
        let bytes = msg.encode();
        assert_eq!(bytes.len(), 5); // 4 + 1
        let decoded = FastMessage::decode(MSG_HAVE_ALL, &[]).unwrap();
        assert_eq!(decoded, msg);
    }

    /// HaveNone encodes and decodes correctly.
    #[test]
    fn have_none_round_trip() {
        let msg = FastMessage::HaveNone;
        let bytes = msg.encode();
        assert_eq!(bytes.len(), 5);
        let decoded = FastMessage::decode(MSG_HAVE_NONE, &[]).unwrap();
        assert_eq!(decoded, msg);
    }

    /// RejectRequest encodes and decodes correctly.
    #[test]
    fn reject_request_round_trip() {
        let msg = FastMessage::RejectRequest {
            index: 10,
            begin: 0,
            length: 16384,
        };
        let bytes = msg.encode();
        assert_eq!(bytes.len(), 17); // 4 + 1 + 12
        let payload = &bytes[5..];
        let decoded = FastMessage::decode(MSG_REJECT_REQUEST, payload).unwrap();
        assert_eq!(decoded, msg);
    }

    /// AllowedFast encodes and decodes correctly.
    #[test]
    fn allowed_fast_round_trip() {
        let msg = FastMessage::AllowedFast { piece_index: 99 };
        let bytes = msg.encode();
        assert_eq!(bytes.len(), 9);
        let payload = &bytes[5..];
        let decoded = FastMessage::decode(MSG_ALLOWED_FAST, payload).unwrap();
        assert_eq!(decoded, msg);
    }

    // ── Message IDs ─────────────────────────────────────────────────

    /// Message IDs match BEP 6 specification.
    #[test]
    fn message_ids_match_spec() {
        assert_eq!(
            FastMessage::SuggestPiece { piece_index: 0 }.message_id(),
            0x0D
        );
        assert_eq!(FastMessage::HaveAll.message_id(), 0x0E);
        assert_eq!(FastMessage::HaveNone.message_id(), 0x0F);
        assert_eq!(
            FastMessage::RejectRequest {
                index: 0,
                begin: 0,
                length: 0
            }
            .message_id(),
            0x10
        );
        assert_eq!(
            FastMessage::AllowedFast { piece_index: 0 }.message_id(),
            0x11
        );
    }

    // ── AllowedFastSet ──────────────────────────────────────────────

    /// Add and contains work correctly.
    #[test]
    fn allowed_fast_add_contains() {
        let mut afs = AllowedFastSet::new();
        assert!(afs.is_empty());

        assert!(afs.add(42));
        assert!(afs.contains(42));
        assert!(!afs.contains(43));
        assert_eq!(afs.count(), 1);
    }

    /// Duplicate adds are rejected.
    #[test]
    fn allowed_fast_no_duplicates() {
        let mut afs = AllowedFastSet::new();
        assert!(afs.add(42));
        assert!(!afs.add(42));
        assert_eq!(afs.count(), 1);
    }

    /// Set respects maximum capacity.
    #[test]
    fn allowed_fast_cap() {
        let mut afs = AllowedFastSet::new();
        for i in 0..MAX_ALLOWED_FAST_COUNT as u32 {
            assert!(afs.add(i));
        }
        assert!(!afs.add(999));
        assert_eq!(afs.count(), MAX_ALLOWED_FAST_COUNT);
    }

    /// Clear empties the set.
    #[test]
    fn allowed_fast_clear() {
        let mut afs = AllowedFastSet::new();
        afs.add(1);
        afs.add(2);
        afs.clear();
        assert!(afs.is_empty());
    }

    // ── SuggestCache ────────────────────────────────────────────────

    /// Suggestion tracking works.
    #[test]
    fn suggest_cache_basic() {
        let mut cache = SuggestCache::new();
        cache.add(10);
        assert!(cache.is_suggested(10));
        assert!(!cache.is_suggested(11));
        assert_eq!(cache.count(), 1);
    }

    /// Duplicate suggestions promote to most-recent.
    #[test]
    fn suggest_cache_dedup() {
        let mut cache = SuggestCache::new();
        cache.add(10);
        cache.add(20);
        cache.add(10); // Re-add: should move to end.

        assert_eq!(cache.suggestions(), &[20, 10]);
    }

    /// Suggest cache respects maximum capacity.
    #[test]
    fn suggest_cache_cap() {
        let mut cache = SuggestCache::new();
        for i in 0..MAX_SUGGESTIONS as u32 + 5 {
            cache.add(i);
        }
        assert_eq!(cache.count(), MAX_SUGGESTIONS);
    }

    // ── Decode error tests ──────────────────────────────────────────

    /// Truncated SuggestPiece payload returns error.
    #[test]
    fn decode_truncated_suggest() {
        let result = FastMessage::decode(MSG_SUGGEST_PIECE, &[0, 0]);
        assert!(matches!(
            result,
            Err(FastDecodeError::PayloadTooShort { .. })
        ));
    }

    /// Non-fast message ID returns error.
    #[test]
    fn decode_non_fast_id() {
        let result = FastMessage::decode(0x00, &[]); // Choke is not fast.
        assert!(matches!(
            result,
            Err(FastDecodeError::NotFastMessage { .. })
        ));
    }

    /// Default count is accessible.
    #[test]
    fn default_allowed_fast_count() {
        assert_eq!(AllowedFastSet::default_count(), DEFAULT_ALLOWED_FAST_COUNT);
    }
}
