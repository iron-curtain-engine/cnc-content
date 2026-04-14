// SPDX-License-Identifier: MIT OR Apache-2.0

//! BitTorrent peer wire protocol message codec (BEP 3).
//!
//! ## What
//!
//! Defines the complete set of BitTorrent peer wire protocol messages as a
//! strongly-typed enum, with serialization to/from the wire format. Every
//! message exchanged between peers after the handshake flows through these
//! types.
//!
//! ## Why — the wire protocol is the lingua franca
//!
//! All higher-level modules (choking, piece selection, PEX, metadata
//! exchange, fast extension) ultimately produce or consume wire messages.
//! Without a shared message type, each module invents its own byte
//! serialization, leading to duplicated parsing, inconsistent error
//! handling, and no unified message dispatch.
//!
//! The BEP 3 wire format is simple:
//! - 4-byte big-endian length prefix (excluding the length itself)
//! - 1-byte message ID
//! - Payload (variable, message-dependent)
//!
//! Keep-alive is the special case: length 0, no ID, no payload.
//!
//! ## How
//!
//! - [`PeerMessage`]: Enum covering all standard BEP 3 messages plus the
//!   extended message container (BEP 10).
//! - [`encode_message`]: Serializes a `PeerMessage` into a `Vec<u8>` with
//!   length prefix.
//! - [`decode_message`]: Parses a length-prefixed byte buffer into a
//!   `PeerMessage`.
//! - [`MessageError`]: Structured error for malformed messages.
//!
//! The codec is **zero-copy for piece data**: the `Piece` variant borrows
//! its payload from the input buffer when decoding, but this module uses
//! owned `Vec<u8>` for simplicity and safety. A future optimisation could
//! add a lifetime-parameterised variant.

use thiserror::Error;

// ── Constants ───────────────────────────────────────────────────────

/// Length prefix size in bytes (big-endian u32).
const LENGTH_PREFIX_SIZE: usize = 4;

/// Message ID for `Choke`.
const MSG_CHOKE: u8 = 0;

/// Message ID for `Unchoke`.
const MSG_UNCHOKE: u8 = 1;

/// Message ID for `Interested`.
const MSG_INTERESTED: u8 = 2;

/// Message ID for `NotInterested`.
const MSG_NOT_INTERESTED: u8 = 3;

/// Message ID for `Have`.
const MSG_HAVE: u8 = 4;

/// Message ID for `Bitfield`.
const MSG_BITFIELD: u8 = 5;

/// Message ID for `Request`.
const MSG_REQUEST: u8 = 6;

/// Message ID for `Piece` (data response).
const MSG_PIECE: u8 = 7;

/// Message ID for `Cancel`.
const MSG_CANCEL: u8 = 8;

/// Message ID for `Port` (DHT port advertisement, BEP 5).
const MSG_PORT: u8 = 9;

/// Message ID for `Extended` (BEP 10 extension protocol container).
const MSG_EXTENDED: u8 = 20;

/// Standard request/piece block size (16 KiB). Most BT clients use this
/// as the default sub-piece block size.
pub const BLOCK_SIZE: u32 = 16_384;

/// Maximum message length accepted (16 MiB). Prevents memory exhaustion
/// from malicious length prefixes. A standard piece message with 16 KiB
/// block is only ~16.4 KiB; even with a 4 MiB piece, 16 MiB is generous.
pub const MAX_MESSAGE_LENGTH: u32 = 16 * 1024 * 1024;

// ── Message types ───────────────────────────────────────────────────

/// A BitTorrent peer wire protocol message.
///
/// Covers all BEP 3 message types plus the BEP 10 extended message
/// container. Keep-alive is represented as its own variant rather than
/// being implicit.
///
/// ```
/// use p2p_distribute::message::{PeerMessage, encode_message, decode_message};
///
/// let msg = PeerMessage::Have { piece_index: 42 };
/// let bytes = encode_message(&msg);
///
/// // Length prefix (4) + message ID (1) + piece index (4) = 9 bytes.
/// assert_eq!(bytes.len(), 9);
///
/// let decoded = decode_message(&bytes).unwrap();
/// assert_eq!(decoded, msg);
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PeerMessage {
    /// Keep-alive: length 0, no ID, no payload. Prevents connection timeout.
    KeepAlive,

    /// Choke: the sender will not serve requests from the receiver.
    Choke,

    /// Unchoke: the sender is willing to serve requests.
    Unchoke,

    /// Interested: the sender wants pieces the receiver has.
    Interested,

    /// Not interested: the sender does not want anything from the receiver.
    NotInterested,

    /// Have: the sender has completed the given piece.
    Have {
        /// Zero-based piece index.
        piece_index: u32,
    },

    /// Bitfield: compact representation of all pieces the sender has.
    /// Sent immediately after handshake, before any other messages.
    Bitfield {
        /// Raw bitfield bytes (BEP 3 big-endian bit order).
        data: Vec<u8>,
    },

    /// Request: ask the receiver for a block of a piece.
    Request {
        /// Piece index.
        index: u32,
        /// Byte offset within the piece.
        begin: u32,
        /// Block length in bytes (typically [`BLOCK_SIZE`]).
        length: u32,
    },

    /// Piece: response carrying a block of data.
    Piece {
        /// Piece index.
        index: u32,
        /// Byte offset within the piece.
        begin: u32,
        /// Block data.
        block: Vec<u8>,
    },

    /// Cancel: retract a previously-sent Request.
    Cancel {
        /// Piece index.
        index: u32,
        /// Byte offset within the piece.
        begin: u32,
        /// Block length.
        length: u32,
    },

    /// Port: advertise the sender's DHT listen port (BEP 5).
    Port {
        /// DHT listen port.
        port: u16,
    },

    /// Extended: BEP 10 extension protocol message container.
    Extended {
        /// Extension message ID (0 = handshake, others are negotiated).
        ext_id: u8,
        /// Extension payload (typically bencoded).
        payload: Vec<u8>,
    },
}

impl PeerMessage {
    /// Returns the BEP 3 message ID, or `None` for keep-alive.
    pub fn message_id(&self) -> Option<u8> {
        match self {
            Self::KeepAlive => None,
            Self::Choke => Some(MSG_CHOKE),
            Self::Unchoke => Some(MSG_UNCHOKE),
            Self::Interested => Some(MSG_INTERESTED),
            Self::NotInterested => Some(MSG_NOT_INTERESTED),
            Self::Have { .. } => Some(MSG_HAVE),
            Self::Bitfield { .. } => Some(MSG_BITFIELD),
            Self::Request { .. } => Some(MSG_REQUEST),
            Self::Piece { .. } => Some(MSG_PIECE),
            Self::Cancel { .. } => Some(MSG_CANCEL),
            Self::Port { .. } => Some(MSG_PORT),
            Self::Extended { .. } => Some(MSG_EXTENDED),
        }
    }

    /// Returns whether this is a data-carrying message (Piece).
    pub fn is_data(&self) -> bool {
        matches!(self, Self::Piece { .. })
    }

    /// Returns whether this is a control message (non-data, non-keepalive).
    pub fn is_control(&self) -> bool {
        !matches!(self, Self::KeepAlive | Self::Piece { .. })
    }
}

// ── Encoding ────────────────────────────────────────────────────────

/// Encodes a `PeerMessage` into its wire format (length-prefixed).
///
/// The returned `Vec<u8>` contains the 4-byte big-endian length prefix
/// followed by the message ID and payload.
pub fn encode_message(msg: &PeerMessage) -> Vec<u8> {
    match msg {
        PeerMessage::KeepAlive => {
            // Length 0, no ID, no payload.
            vec![0, 0, 0, 0]
        }
        PeerMessage::Choke => encode_id_only(MSG_CHOKE),
        PeerMessage::Unchoke => encode_id_only(MSG_UNCHOKE),
        PeerMessage::Interested => encode_id_only(MSG_INTERESTED),
        PeerMessage::NotInterested => encode_id_only(MSG_NOT_INTERESTED),

        PeerMessage::Have { piece_index } => {
            // Length: 1 (ID) + 4 (index) = 5.
            let mut buf = Vec::with_capacity(LENGTH_PREFIX_SIZE + 5);
            buf.extend_from_slice(&5u32.to_be_bytes());
            buf.push(MSG_HAVE);
            buf.extend_from_slice(&piece_index.to_be_bytes());
            buf
        }

        PeerMessage::Bitfield { data } => {
            // Length: 1 (ID) + data.len().
            let len = 1u32.saturating_add(data.len() as u32);
            let mut buf = Vec::with_capacity(LENGTH_PREFIX_SIZE + len as usize);
            buf.extend_from_slice(&len.to_be_bytes());
            buf.push(MSG_BITFIELD);
            buf.extend_from_slice(data);
            buf
        }

        PeerMessage::Request {
            index,
            begin,
            length,
        } => encode_index_begin_length(MSG_REQUEST, *index, *begin, *length),

        PeerMessage::Piece {
            index,
            begin,
            block,
        } => {
            // Length: 1 (ID) + 4 (index) + 4 (begin) + block.len().
            let len = 9u32.saturating_add(block.len() as u32);
            let mut buf = Vec::with_capacity(LENGTH_PREFIX_SIZE + len as usize);
            buf.extend_from_slice(&len.to_be_bytes());
            buf.push(MSG_PIECE);
            buf.extend_from_slice(&index.to_be_bytes());
            buf.extend_from_slice(&begin.to_be_bytes());
            buf.extend_from_slice(block);
            buf
        }

        PeerMessage::Cancel {
            index,
            begin,
            length,
        } => encode_index_begin_length(MSG_CANCEL, *index, *begin, *length),

        PeerMessage::Port { port } => {
            // Length: 1 (ID) + 2 (port) = 3.
            let mut buf = Vec::with_capacity(LENGTH_PREFIX_SIZE + 3);
            buf.extend_from_slice(&3u32.to_be_bytes());
            buf.push(MSG_PORT);
            buf.extend_from_slice(&port.to_be_bytes());
            buf
        }

        PeerMessage::Extended { ext_id, payload } => {
            // Length: 1 (ID) + 1 (ext_id) + payload.len().
            let len = 2u32.saturating_add(payload.len() as u32);
            let mut buf = Vec::with_capacity(LENGTH_PREFIX_SIZE + len as usize);
            buf.extend_from_slice(&len.to_be_bytes());
            buf.push(MSG_EXTENDED);
            buf.push(*ext_id);
            buf.extend_from_slice(payload);
            buf
        }
    }
}

/// Encodes a message with only an ID (no payload). Length = 1.
fn encode_id_only(id: u8) -> Vec<u8> {
    let mut buf = Vec::with_capacity(LENGTH_PREFIX_SIZE + 1);
    buf.extend_from_slice(&1u32.to_be_bytes());
    buf.push(id);
    buf
}

/// Encodes a message with index + begin + length payload (Request, Cancel).
fn encode_index_begin_length(id: u8, index: u32, begin: u32, length: u32) -> Vec<u8> {
    // Length: 1 (ID) + 4 + 4 + 4 = 13.
    let mut buf = Vec::with_capacity(LENGTH_PREFIX_SIZE + 13);
    buf.extend_from_slice(&13u32.to_be_bytes());
    buf.push(id);
    buf.extend_from_slice(&index.to_be_bytes());
    buf.extend_from_slice(&begin.to_be_bytes());
    buf.extend_from_slice(&length.to_be_bytes());
    buf
}

// ── Decoding ────────────────────────────────────────────────────────

/// Decodes a length-prefixed wire message into a `PeerMessage`.
///
/// The input must contain at least the 4-byte length prefix and the
/// full message body indicated by that prefix.
pub fn decode_message(data: &[u8]) -> Result<PeerMessage, MessageError> {
    // Read the 4-byte length prefix.
    let len_bytes = data
        .get(..LENGTH_PREFIX_SIZE)
        .ok_or(MessageError::TooShort {
            needed: LENGTH_PREFIX_SIZE as u32,
            actual: data.len() as u32,
        })?;
    let length = u32::from_be_bytes([
        *len_bytes.first().unwrap_or(&0),
        *len_bytes.get(1).unwrap_or(&0),
        *len_bytes.get(2).unwrap_or(&0),
        *len_bytes.get(3).unwrap_or(&0),
    ]);

    // Keep-alive: length 0.
    if length == 0 {
        return Ok(PeerMessage::KeepAlive);
    }

    // Guard against oversized messages.
    if length > MAX_MESSAGE_LENGTH {
        return Err(MessageError::TooLarge {
            length,
            max: MAX_MESSAGE_LENGTH,
        });
    }

    let total_len = LENGTH_PREFIX_SIZE.saturating_add(length as usize);
    if data.len() < total_len {
        return Err(MessageError::TooShort {
            needed: total_len as u32,
            actual: data.len() as u32,
        });
    }

    // The message body starts after the length prefix.
    let body = data
        .get(LENGTH_PREFIX_SIZE..total_len)
        .ok_or(MessageError::TooShort {
            needed: total_len as u32,
            actual: data.len() as u32,
        })?;

    // First byte of body is the message ID.
    let msg_id = *body.first().ok_or(MessageError::TooShort {
        needed: 1,
        actual: 0,
    })?;
    let payload = body.get(1..).unwrap_or(&[]);

    match msg_id {
        MSG_CHOKE => Ok(PeerMessage::Choke),
        MSG_UNCHOKE => Ok(PeerMessage::Unchoke),
        MSG_INTERESTED => Ok(PeerMessage::Interested),
        MSG_NOT_INTERESTED => Ok(PeerMessage::NotInterested),

        MSG_HAVE => {
            let idx_bytes = payload.get(..4).ok_or(MessageError::InvalidPayload {
                msg_id,
                reason: "have message requires 4-byte piece index".into(),
            })?;
            let piece_index = u32::from_be_bytes([
                *idx_bytes.first().unwrap_or(&0),
                *idx_bytes.get(1).unwrap_or(&0),
                *idx_bytes.get(2).unwrap_or(&0),
                *idx_bytes.get(3).unwrap_or(&0),
            ]);
            Ok(PeerMessage::Have { piece_index })
        }

        MSG_BITFIELD => Ok(PeerMessage::Bitfield {
            data: payload.to_vec(),
        }),

        MSG_REQUEST => {
            decode_index_begin_length(msg_id, payload).map(|(i, b, l)| PeerMessage::Request {
                index: i,
                begin: b,
                length: l,
            })
        }

        MSG_PIECE => {
            if payload.len() < 8 {
                return Err(MessageError::InvalidPayload {
                    msg_id,
                    reason: "piece message requires at least 8 bytes (index + begin)".into(),
                });
            }
            let index = read_u32(payload, 0)?;
            let begin = read_u32(payload, 4)?;
            let block = payload.get(8..).unwrap_or(&[]).to_vec();
            Ok(PeerMessage::Piece {
                index,
                begin,
                block,
            })
        }

        MSG_CANCEL => {
            decode_index_begin_length(msg_id, payload).map(|(i, b, l)| PeerMessage::Cancel {
                index: i,
                begin: b,
                length: l,
            })
        }

        MSG_PORT => {
            if payload.len() < 2 {
                return Err(MessageError::InvalidPayload {
                    msg_id,
                    reason: "port message requires 2-byte port".into(),
                });
            }
            let port = u16::from_be_bytes([
                *payload.first().unwrap_or(&0),
                *payload.get(1).unwrap_or(&0),
            ]);
            Ok(PeerMessage::Port { port })
        }

        MSG_EXTENDED => {
            let ext_id = *payload.first().ok_or(MessageError::InvalidPayload {
                msg_id,
                reason: "extended message requires at least 1-byte extension ID".into(),
            })?;
            let ext_payload = payload.get(1..).unwrap_or(&[]).to_vec();
            Ok(PeerMessage::Extended {
                ext_id,
                payload: ext_payload,
            })
        }

        _ => Err(MessageError::UnknownId { msg_id }),
    }
}

/// Reads a big-endian u32 from a slice at the given offset.
fn read_u32(data: &[u8], offset: usize) -> Result<u32, MessageError> {
    let slice = data
        .get(offset..offset.saturating_add(4))
        .ok_or(MessageError::TooShort {
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

/// Decodes index + begin + length from a standard 12-byte payload.
fn decode_index_begin_length(msg_id: u8, payload: &[u8]) -> Result<(u32, u32, u32), MessageError> {
    if payload.len() < 12 {
        return Err(MessageError::InvalidPayload {
            msg_id,
            reason: "requires 12-byte payload (index + begin + length)".into(),
        });
    }
    let index = read_u32(payload, 0)?;
    let begin = read_u32(payload, 4)?;
    let length = read_u32(payload, 8)?;
    Ok((index, begin, length))
}

// ── Errors ──────────────────────────────────────────────────────────

/// Error during message encoding or decoding.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum MessageError {
    /// Input buffer too short to contain the expected data.
    #[error("message too short: need {needed} bytes, got {actual}")]
    TooShort {
        /// Minimum bytes needed.
        needed: u32,
        /// Bytes actually available.
        actual: u32,
    },

    /// Message exceeds the maximum allowed length.
    #[error("message too large: {length} bytes exceeds max {max}")]
    TooLarge {
        /// Declared message length.
        length: u32,
        /// Maximum allowed length.
        max: u32,
    },

    /// Unknown message ID (not a standard BEP 3 or BEP 10 message).
    #[error("unknown message ID: {msg_id}")]
    UnknownId {
        /// The unrecognised message ID byte.
        msg_id: u8,
    },

    /// Message payload is malformed for the given message type.
    #[error("invalid payload for message ID {msg_id}: {reason}")]
    InvalidPayload {
        /// The message ID.
        msg_id: u8,
        /// Human-readable description of what's wrong.
        reason: String,
    },
}

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── Round-trip tests ────────────────────────────────────────────

    /// Keep-alive round-trips correctly.
    ///
    /// Keep-alive is the zero-length sentinel that prevents TCP timeout.
    #[test]
    fn keepalive_round_trip() {
        let bytes = encode_message(&PeerMessage::KeepAlive);
        assert_eq!(bytes, [0, 0, 0, 0]);
        let decoded = decode_message(&bytes).unwrap();
        assert_eq!(decoded, PeerMessage::KeepAlive);
    }

    /// Choke round-trips correctly.
    ///
    /// ID-only messages must encode as length=1 + ID byte.
    #[test]
    fn choke_round_trip() {
        let msg = PeerMessage::Choke;
        let bytes = encode_message(&msg);
        assert_eq!(bytes.len(), 5);
        assert_eq!(decode_message(&bytes).unwrap(), msg);
    }

    /// Unchoke round-trips correctly.
    #[test]
    fn unchoke_round_trip() {
        let msg = PeerMessage::Unchoke;
        assert_eq!(decode_message(&encode_message(&msg)).unwrap(), msg);
    }

    /// Interested round-trips correctly.
    #[test]
    fn interested_round_trip() {
        let msg = PeerMessage::Interested;
        assert_eq!(decode_message(&encode_message(&msg)).unwrap(), msg);
    }

    /// NotInterested round-trips correctly.
    #[test]
    fn not_interested_round_trip() {
        let msg = PeerMessage::NotInterested;
        assert_eq!(decode_message(&encode_message(&msg)).unwrap(), msg);
    }

    /// Have round-trips with correct piece index.
    ///
    /// The 4-byte big-endian piece index must survive encoding.
    #[test]
    fn have_round_trip() {
        let msg = PeerMessage::Have {
            piece_index: 0x1234_5678,
        };
        let bytes = encode_message(&msg);
        assert_eq!(bytes.len(), 9); // 4 + 1 + 4
        assert_eq!(decode_message(&bytes).unwrap(), msg);
    }

    /// Bitfield round-trips with arbitrary data.
    ///
    /// The bitfield payload should be preserved exactly.
    #[test]
    fn bitfield_round_trip() {
        let msg = PeerMessage::Bitfield {
            data: vec![0xFF, 0x80, 0x00],
        };
        assert_eq!(decode_message(&encode_message(&msg)).unwrap(), msg);
    }

    /// Request round-trips with all three fields.
    #[test]
    fn request_round_trip() {
        let msg = PeerMessage::Request {
            index: 10,
            begin: 0,
            length: BLOCK_SIZE,
        };
        let bytes = encode_message(&msg);
        assert_eq!(bytes.len(), 17); // 4 + 1 + 12
        assert_eq!(decode_message(&bytes).unwrap(), msg);
    }

    /// Piece round-trips with block data.
    ///
    /// The block payload must be preserved byte-for-byte.
    #[test]
    fn piece_round_trip() {
        let block = vec![0xDE, 0xAD, 0xBE, 0xEF];
        let msg = PeerMessage::Piece {
            index: 5,
            begin: 16384,
            block: block.clone(),
        };
        let bytes = encode_message(&msg);
        // 4 (prefix) + 1 (id) + 4 (index) + 4 (begin) + 4 (block) = 17
        assert_eq!(bytes.len(), 17);
        assert_eq!(decode_message(&bytes).unwrap(), msg);
    }

    /// Cancel round-trips correctly.
    #[test]
    fn cancel_round_trip() {
        let msg = PeerMessage::Cancel {
            index: 7,
            begin: 0,
            length: BLOCK_SIZE,
        };
        assert_eq!(decode_message(&encode_message(&msg)).unwrap(), msg);
    }

    /// Port round-trips with correct value.
    #[test]
    fn port_round_trip() {
        let msg = PeerMessage::Port { port: 6881 };
        let bytes = encode_message(&msg);
        assert_eq!(bytes.len(), 7); // 4 + 1 + 2
        assert_eq!(decode_message(&bytes).unwrap(), msg);
    }

    /// Extended round-trips with ext_id and payload.
    #[test]
    fn extended_round_trip() {
        let msg = PeerMessage::Extended {
            ext_id: 1,
            payload: b"d1:v5:test1e".to_vec(),
        };
        assert_eq!(decode_message(&encode_message(&msg)).unwrap(), msg);
    }

    // ── Message ID tests ────────────────────────────────────────────

    /// Message IDs match BEP 3 specification.
    ///
    /// These values are protocol constants and must never change.
    #[test]
    fn message_ids_match_spec() {
        assert_eq!(PeerMessage::Choke.message_id(), Some(0));
        assert_eq!(PeerMessage::Unchoke.message_id(), Some(1));
        assert_eq!(PeerMessage::Interested.message_id(), Some(2));
        assert_eq!(PeerMessage::NotInterested.message_id(), Some(3));
        assert_eq!(PeerMessage::Have { piece_index: 0 }.message_id(), Some(4));
        assert_eq!(PeerMessage::Bitfield { data: vec![] }.message_id(), Some(5));
        assert_eq!(
            PeerMessage::Request {
                index: 0,
                begin: 0,
                length: 0
            }
            .message_id(),
            Some(6)
        );
        assert_eq!(
            PeerMessage::Piece {
                index: 0,
                begin: 0,
                block: vec![]
            }
            .message_id(),
            Some(7)
        );
        assert_eq!(
            PeerMessage::Cancel {
                index: 0,
                begin: 0,
                length: 0
            }
            .message_id(),
            Some(8)
        );
        assert_eq!(PeerMessage::Port { port: 0 }.message_id(), Some(9));
        assert_eq!(
            PeerMessage::Extended {
                ext_id: 0,
                payload: vec![]
            }
            .message_id(),
            Some(20)
        );
        assert_eq!(PeerMessage::KeepAlive.message_id(), None);
    }

    // ── Classification tests ────────────────────────────────────────

    /// Piece messages are classified as data.
    #[test]
    fn piece_is_data() {
        let msg = PeerMessage::Piece {
            index: 0,
            begin: 0,
            block: vec![1],
        };
        assert!(msg.is_data());
        assert!(!msg.is_control());
    }

    /// Control messages are classified correctly.
    #[test]
    fn choke_is_control() {
        assert!(PeerMessage::Choke.is_control());
        assert!(!PeerMessage::Choke.is_data());
    }

    /// Keep-alive is neither data nor control.
    #[test]
    fn keepalive_classification() {
        assert!(!PeerMessage::KeepAlive.is_data());
        assert!(!PeerMessage::KeepAlive.is_control());
    }

    // ── Error tests ─────────────────────────────────────────────────

    /// Decoding too-short input returns TooShort error.
    #[test]
    fn decode_too_short() {
        let result = decode_message(&[0, 0]);
        assert!(matches!(result, Err(MessageError::TooShort { .. })));
    }

    /// Decoding oversized length prefix returns TooLarge error.
    #[test]
    fn decode_too_large() {
        // Encode a length of MAX_MESSAGE_LENGTH + 1.
        let bad_len = (MAX_MESSAGE_LENGTH + 1).to_be_bytes();
        let result = decode_message(&bad_len);
        assert!(matches!(result, Err(MessageError::TooLarge { .. })));
    }

    /// Unknown message ID returns UnknownId error.
    #[test]
    fn decode_unknown_id() {
        // Length=1, ID=99 (not defined).
        let data = [0, 0, 0, 1, 99];
        let result = decode_message(&data);
        assert!(matches!(
            result,
            Err(MessageError::UnknownId { msg_id: 99 })
        ));
    }

    /// Truncated Have payload returns InvalidPayload error.
    #[test]
    fn decode_truncated_have() {
        // Length=3, ID=4 (have), but only 2 bytes of index.
        let data = [0, 0, 0, 3, MSG_HAVE, 0, 0];
        let result = decode_message(&data);
        assert!(matches!(result, Err(MessageError::InvalidPayload { .. })));
    }

    /// Truncated Request payload returns InvalidPayload error.
    #[test]
    fn decode_truncated_request() {
        // Length=5, ID=6 (request), only 4 bytes instead of 12.
        let data = [0, 0, 0, 5, MSG_REQUEST, 0, 0, 0, 0];
        let result = decode_message(&data);
        assert!(matches!(result, Err(MessageError::InvalidPayload { .. })));
    }
}
