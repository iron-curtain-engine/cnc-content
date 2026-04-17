// SPDX-License-Identifier: MIT OR Apache-2.0

//! Kademlia DHT for decentralised peer discovery — O(log N) lookup with
//! XOR distance metric.
//!
//! ## What
//!
//! A Kademlia-style distributed hash table for discovering peers without
//! relying on centralised trackers. Each node maintains a routing table of
//! k-buckets organised by XOR distance from its own [`NodeId`]. Lookups
//! converge in O(log N) hops by iteratively querying closer nodes.
//!
//! ## Why — Kademlia / Kad network lesson
//!
//! eMule's Kad network (implementing Kademlia) enables tracker-free peer
//! discovery for millions of nodes. Key lessons:
//!
//! - **XOR distance metric** — symmetric, satisfies triangle inequality,
//!   and produces uniformly distributed bucket occupancies. Simpler than
//!   Chord's ring structure.
//! - **k-buckets** — each bucket holds up to `K` peers at a given distance
//!   range. Older peers are preferred (long-lived nodes are more reliable).
//! - **Iterative lookup** — query α closest nodes in parallel, converge
//!   toward the target in O(log N) rounds.
//! - **NetworkId scoping** — our DHT operates per-group, preventing
//!   cross-contamination between test/prod networks.
//!
//! ## How
//!
//! - [`NodeId`]: 256-bit (32-byte) node identity, same size as [`PeerId`].
//! - [`RoutingTable`]: k-bucket array (256 buckets for 256-bit IDs).
//! - [`DhtMessage`]: PING, FIND_NODE, FIND_VALUE, STORE request/response.
//! - [`DhtNode`]: Local node state with routing table and message handling.
//!
//! The transport layer is not implemented here — this module provides the
//! data structures and algorithms. Consumers wire it to their own transport
//! (UDP, IC wire protocol, etc.).

use crate::network_id::NetworkId;
use crate::peer_id::PeerId;

// ── Constants ───────────────────────────────────────────────────────

/// Maximum entries per k-bucket (Kademlia default).
pub const K: usize = 20;

/// Number of parallel lookups per iteration (Kademlia α parameter).
pub const ALPHA: usize = 3;

/// Node ID size in bytes (matches PeerId for compatibility).
const NODE_ID_LEN: usize = 32;

/// Total number of k-buckets (one per bit of the ID space).
const BUCKET_COUNT: usize = NODE_ID_LEN * 8;

// ── NodeId ──────────────────────────────────────────────────────────

/// 256-bit node identity in the Kademlia DHT.
///
/// Derived from a [`PeerId`] by copying the raw bytes. The XOR distance
/// between two `NodeId`s determines their relative position in the DHT.
///
/// ```
/// use p2p_distribute::dht::NodeId;
/// use p2p_distribute::PeerId;
///
/// let peer = PeerId::from_key_material(b"alice");
/// let node = NodeId::from_peer_id(&peer);
///
/// // Distance to self is zero.
/// let dist = node.xor_distance(&node);
/// assert!(dist.iter().all(|&b| b == 0));
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct NodeId {
    bytes: [u8; NODE_ID_LEN],
}

impl NodeId {
    /// Creates a NodeId from raw bytes.
    pub fn from_bytes(bytes: [u8; NODE_ID_LEN]) -> Self {
        Self { bytes }
    }

    /// Creates a NodeId from a PeerId.
    pub fn from_peer_id(peer_id: &PeerId) -> Self {
        Self {
            bytes: *peer_id.as_bytes(),
        }
    }

    /// Returns the raw bytes.
    pub fn as_bytes(&self) -> &[u8; NODE_ID_LEN] {
        &self.bytes
    }

    /// Computes the XOR distance between two node IDs.
    ///
    /// The XOR metric is symmetric and satisfies the triangle inequality.
    pub fn xor_distance(&self, other: &NodeId) -> [u8; NODE_ID_LEN] {
        let mut result = [0u8; NODE_ID_LEN];
        for (r, (a, b)) in result
            .iter_mut()
            .zip(self.bytes.iter().zip(other.bytes.iter()))
        {
            *r = a ^ b;
        }
        result
    }

    /// Returns the index of the highest bit set in the XOR distance,
    /// which determines which k-bucket a peer belongs in.
    ///
    /// Returns `None` if the distance is zero (same node).
    pub fn bucket_index(&self, other: &NodeId) -> Option<usize> {
        let distance = self.xor_distance(other);
        // Find the highest set bit.
        for (i, &byte) in distance.iter().enumerate() {
            if byte != 0 {
                let bit_pos = 7u32.saturating_sub(byte.leading_zeros());
                let bucket = (NODE_ID_LEN.saturating_sub(1).saturating_sub(i))
                    .saturating_mul(8)
                    .saturating_add(bit_pos as usize);
                return Some(bucket);
            }
        }
        None // Same node
    }
}

// ── KBucketEntry ────────────────────────────────────────────────────

/// An entry in a k-bucket: a node we know about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KBucketEntry {
    /// The node's identity.
    pub node_id: NodeId,
    /// Network address (IP:port or other transport-specific address).
    pub addr: String,
    /// Last time this node responded to a query (Unix epoch seconds).
    pub last_seen_unix_secs: u64,
}

// ── RoutingTable ────────────────────────────────────────────────────

/// Kademlia routing table — 256 k-buckets organised by XOR distance.
///
/// Each bucket holds up to [`K`] entries at a specific distance range
/// from the local node. Buckets closer to the local node tend to have
/// fewer entries (there are fewer nodes at closer XOR distances).
///
/// ## Eviction policy (Kademlia)
///
/// When a bucket is full, new entries are only added if the oldest
/// entry fails a liveness check (PING). This "prefer older nodes"
/// policy exploits the observation that long-lived nodes tend to stay
/// alive longer (Pareto distribution of node lifetimes).
#[derive(Debug, Clone)]
pub struct RoutingTable {
    /// Local node identity.
    local_id: NodeId,
    /// K-buckets indexed by distance bit position.
    buckets: Vec<Vec<KBucketEntry>>,
}

impl RoutingTable {
    /// Creates an empty routing table for the given local node.
    pub fn new(local_id: NodeId) -> Self {
        Self {
            local_id,
            buckets: (0..BUCKET_COUNT).map(|_| Vec::new()).collect(),
        }
    }

    /// Returns the local node's identity.
    pub fn local_id(&self) -> &NodeId {
        &self.local_id
    }

    /// Attempts to insert or update a node in the appropriate k-bucket.
    ///
    /// Returns `true` if the node was inserted or updated. Returns `false`
    /// if the bucket is full (caller should PING the oldest entry and evict
    /// if it doesn't respond).
    pub fn insert(&mut self, entry: KBucketEntry) -> bool {
        let Some(bucket_idx) = self.local_id.bucket_index(&entry.node_id) else {
            return false; // Same as local node
        };

        let bucket = match self.buckets.get_mut(bucket_idx) {
            Some(b) => b,
            None => return false,
        };

        // If already in bucket, move to tail (most recently seen).
        if let Some(pos) = bucket.iter().position(|e| e.node_id == entry.node_id) {
            bucket.remove(pos);
            bucket.push(entry);
            return true;
        }

        // If bucket has room, add to tail.
        if bucket.len() < K {
            bucket.push(entry);
            return true;
        }

        // Bucket full — caller should PING oldest and evict if dead.
        false
    }

    /// Evicts the oldest entry in the bucket and inserts the new entry.
    ///
    /// Called after a PING to the oldest entry times out.
    pub fn evict_and_insert(&mut self, entry: KBucketEntry) -> bool {
        let Some(bucket_idx) = self.local_id.bucket_index(&entry.node_id) else {
            return false;
        };

        let bucket = match self.buckets.get_mut(bucket_idx) {
            Some(b) => b,
            None => return false,
        };

        if !bucket.is_empty() {
            bucket.remove(0); // Remove oldest (head)
        }
        bucket.push(entry);
        true
    }

    /// Finds the `count` closest nodes to a target ID.
    ///
    /// Searches all buckets and returns nodes sorted by XOR distance
    /// to the target.
    pub fn closest_nodes(&self, target: &NodeId, count: usize) -> Vec<&KBucketEntry> {
        let mut all_entries: Vec<(&KBucketEntry, [u8; NODE_ID_LEN])> = self
            .buckets
            .iter()
            .flat_map(|bucket| bucket.iter())
            .map(|entry| (entry, target.xor_distance(&entry.node_id)))
            .collect();

        // Sort by XOR distance (lexicographic comparison of byte arrays).
        all_entries.sort_by_key(|a| a.1);

        all_entries
            .into_iter()
            .take(count)
            .map(|(entry, _)| entry)
            .collect()
    }

    /// Total number of entries across all buckets.
    pub fn node_count(&self) -> usize {
        self.buckets.iter().map(|b| b.len()).sum()
    }

    /// Returns the number of non-empty buckets.
    pub fn active_bucket_count(&self) -> usize {
        self.buckets.iter().filter(|b| !b.is_empty()).count()
    }
}

// ── DhtMessage ──────────────────────────────────────────────────────

/// Kademlia DHT message types.
///
/// Each message includes a [`NetworkId`] for cross-network isolation.
/// Nodes must discard messages with non-matching network IDs.
#[derive(Debug, Clone)]
pub enum DhtMessage {
    /// Liveness check — "are you there?"
    Ping {
        network_id: NetworkId,
        sender: NodeId,
    },
    /// Liveness response — "yes, I'm here."
    Pong {
        network_id: NetworkId,
        sender: NodeId,
    },
    /// Find the K closest nodes to a target ID.
    FindNode {
        network_id: NetworkId,
        sender: NodeId,
        target: NodeId,
    },
    /// Response to FindNode — K closest known nodes.
    FoundNode {
        network_id: NetworkId,
        sender: NodeId,
        nodes: Vec<KBucketEntry>,
    },
    /// Store a value at a key (content hash → peer address).
    Store {
        network_id: NetworkId,
        sender: NodeId,
        key: [u8; NODE_ID_LEN],
        value: String,
    },
    /// Find the value associated with a key.
    FindValue {
        network_id: NetworkId,
        sender: NodeId,
        key: [u8; NODE_ID_LEN],
    },
    /// Response to FindValue — either the value or K closest nodes.
    FoundValue {
        network_id: NetworkId,
        sender: NodeId,
        /// `Some` if the queried node has the value; `None` if it returned
        /// closer nodes instead.
        value: Option<String>,
        /// Closer nodes (populated when value is `None`).
        nodes: Vec<KBucketEntry>,
    },
    /// Unknown message type — forward compatibility (IRC CTCP pattern).
    ///
    /// ## Why
    ///
    /// IRC's CTCP (Client-To-Client Protocol) embeds extensible commands
    /// inside regular PRIVMSG: `\x01ACTION dances\x01`. Clients that don't
    /// understand a CTCP type simply ignore it — no disconnection, no error.
    ///
    /// The same principle here: when a node receives a message with an
    /// unknown `type_id`, it is captured as `Unknown` rather than causing
    /// a parse error. The sender's routing table entry is still updated
    /// (the node is alive, even if it speaks a newer protocol). This
    /// allows gradual protocol upgrades without flag-day deployments.
    Unknown {
        network_id: NetworkId,
        sender: NodeId,
        /// Opaque message type identifier from the wire format.
        type_id: u8,
        /// Raw message payload (unparsed).
        payload: Vec<u8>,
    },
}

// ── DhtNode ─────────────────────────────────────────────────────────

/// Local DHT node state — routing table + simple key-value store.
///
/// ```
/// use p2p_distribute::dht::{DhtNode, NodeId, KBucketEntry};
/// use p2p_distribute::NetworkId;
/// use p2p_distribute::PeerId;
///
/// let peer = PeerId::from_key_material(b"local-node");
/// let local_id = NodeId::from_peer_id(&peer);
/// let mut node = DhtNode::new(local_id, NetworkId::TEST);
///
/// // Add a known node.
/// let remote = NodeId::from_peer_id(&PeerId::from_key_material(b"remote-1"));
/// node.routing_table_mut().insert(KBucketEntry {
///     node_id: remote,
///     addr: "192.168.1.1:6881".into(),
///     last_seen_unix_secs: 0,
/// });
///
/// assert_eq!(node.routing_table().node_count(), 1);
/// ```
pub struct DhtNode {
    /// Local node identity.
    local_id: NodeId,
    /// Network scope (test/prod/custom).
    network_id: NetworkId,
    /// Routing table for peer discovery.
    routing_table: RoutingTable,
    /// Local key-value store for STORE/FIND_VALUE.
    store: std::collections::HashMap<[u8; NODE_ID_LEN], String>,
}

impl DhtNode {
    /// Creates a new DHT node.
    pub fn new(local_id: NodeId, network_id: NetworkId) -> Self {
        Self {
            local_id,
            network_id,
            routing_table: RoutingTable::new(local_id),
            store: std::collections::HashMap::new(),
        }
    }

    /// Returns the local node ID.
    pub fn local_id(&self) -> &NodeId {
        &self.local_id
    }

    /// Returns the network ID.
    pub fn network_id(&self) -> &NetworkId {
        &self.network_id
    }

    /// Returns the routing table (read-only).
    pub fn routing_table(&self) -> &RoutingTable {
        &self.routing_table
    }

    /// Returns the routing table (mutable).
    pub fn routing_table_mut(&mut self) -> &mut RoutingTable {
        &mut self.routing_table
    }

    /// Handles an incoming DHT message and produces a response.
    ///
    /// Returns `None` if the message's network ID doesn't match (silently
    /// discarded for cross-network isolation).
    pub fn handle_message(&mut self, msg: DhtMessage) -> Option<DhtMessage> {
        match msg {
            DhtMessage::Ping { network_id, sender } => {
                if network_id != self.network_id {
                    return None;
                }
                // Update routing table with sender.
                self.routing_table.insert(KBucketEntry {
                    node_id: sender,
                    addr: String::new(),
                    last_seen_unix_secs: 0,
                });
                Some(DhtMessage::Pong {
                    network_id: self.network_id,
                    sender: self.local_id,
                })
            }
            DhtMessage::Pong { network_id, sender } => {
                if network_id != self.network_id {
                    return None;
                }
                self.routing_table.insert(KBucketEntry {
                    node_id: sender,
                    addr: String::new(),
                    last_seen_unix_secs: 0,
                });
                None // Pong is a response, not a request
            }
            DhtMessage::FindNode {
                network_id,
                sender,
                target,
            } => {
                if network_id != self.network_id {
                    return None;
                }
                self.routing_table.insert(KBucketEntry {
                    node_id: sender,
                    addr: String::new(),
                    last_seen_unix_secs: 0,
                });
                let closest = self
                    .routing_table
                    .closest_nodes(&target, K)
                    .into_iter()
                    .cloned()
                    .collect();
                Some(DhtMessage::FoundNode {
                    network_id: self.network_id,
                    sender: self.local_id,
                    nodes: closest,
                })
            }
            DhtMessage::Store {
                network_id,
                sender,
                key,
                value,
            } => {
                if network_id != self.network_id {
                    return None;
                }
                self.routing_table.insert(KBucketEntry {
                    node_id: sender,
                    addr: String::new(),
                    last_seen_unix_secs: 0,
                });
                self.store.insert(key, value);
                None // STORE is fire-and-forget
            }
            DhtMessage::FindValue {
                network_id,
                sender,
                key,
            } => {
                if network_id != self.network_id {
                    return None;
                }
                self.routing_table.insert(KBucketEntry {
                    node_id: sender,
                    addr: String::new(),
                    last_seen_unix_secs: 0,
                });
                let value = self.store.get(&key).cloned();
                let nodes = if value.is_none() {
                    let target = NodeId::from_bytes(key);
                    self.routing_table
                        .closest_nodes(&target, K)
                        .into_iter()
                        .cloned()
                        .collect()
                } else {
                    Vec::new()
                };
                Some(DhtMessage::FoundValue {
                    network_id: self.network_id,
                    sender: self.local_id,
                    value,
                    nodes,
                })
            }
            DhtMessage::FoundNode { network_id, .. }
            | DhtMessage::FoundValue { network_id, .. } => {
                if network_id != self.network_id {
                    return None;
                }
                None // Responses don't need replies
            }
            // Forward compatibility: unknown message types are silently
            // ignored (IRC CTCP pattern — don't disconnect on unknown
            // commands, just skip them). The sender's existence is still
            // recorded in the routing table.
            DhtMessage::Unknown {
                network_id, sender, ..
            } => {
                if network_id != self.network_id {
                    return None;
                }
                self.routing_table.insert(KBucketEntry {
                    node_id: sender,
                    addr: String::new(),
                    last_seen_unix_secs: 0,
                });
                None
            }
        }
    }

    /// Stores a value in the local store.
    pub fn store_local(&mut self, key: [u8; NODE_ID_LEN], value: String) {
        self.store.insert(key, value);
    }

    /// Looks up a value in the local store.
    pub fn get_local(&self, key: &[u8; NODE_ID_LEN]) -> Option<&String> {
        self.store.get(key)
    }
}

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_node_id(seed: u8) -> NodeId {
        let peer = PeerId::from_key_material(&[seed]);
        NodeId::from_peer_id(&peer)
    }

    // ── NodeId ──────────────────────────────────────────────────────

    /// XOR distance to self is zero.
    ///
    /// A node is zero distance from itself by the XOR metric.
    #[test]
    fn xor_distance_self_is_zero() {
        let id = make_node_id(1);
        let dist = id.xor_distance(&id);
        assert!(dist.iter().all(|&b| b == 0));
    }

    /// XOR distance is symmetric.
    ///
    /// d(A, B) == d(B, A) — core Kademlia property.
    #[test]
    fn xor_distance_symmetric() {
        let a = make_node_id(1);
        let b = make_node_id(2);
        assert_eq!(a.xor_distance(&b), b.xor_distance(&a));
    }

    /// Bucket index for self is None.
    ///
    /// A node should not be in its own routing table.
    #[test]
    fn bucket_index_self_is_none() {
        let id = make_node_id(1);
        assert!(id.bucket_index(&id).is_none());
    }

    /// Different nodes produce a valid bucket index.
    #[test]
    fn bucket_index_different_nodes() {
        let a = make_node_id(1);
        let b = make_node_id(2);
        assert!(a.bucket_index(&b).is_some());
    }

    // ── RoutingTable ────────────────────────────────────────────────

    /// Insert a node into an empty routing table succeeds.
    ///
    /// First entry in any bucket should always succeed.
    #[test]
    fn insert_first_node() {
        let local = make_node_id(0);
        let mut table = RoutingTable::new(local);
        let remote = make_node_id(1);
        assert!(table.insert(KBucketEntry {
            node_id: remote,
            addr: "1.2.3.4:6881".into(),
            last_seen_unix_secs: 100,
        }));
        assert_eq!(table.node_count(), 1);
    }

    /// Inserting the local node ID is rejected.
    ///
    /// A node should never appear in its own routing table.
    #[test]
    fn insert_self_rejected() {
        let local = make_node_id(0);
        let mut table = RoutingTable::new(local);
        assert!(!table.insert(KBucketEntry {
            node_id: local,
            addr: "self".into(),
            last_seen_unix_secs: 0,
        }));
    }

    /// Updating an existing node moves it to the tail.
    ///
    /// Kademlia: most recently seen nodes go to the tail of the bucket.
    #[test]
    fn update_moves_to_tail() {
        let local = make_node_id(0);
        let mut table = RoutingTable::new(local);
        let remote = make_node_id(1);
        table.insert(KBucketEntry {
            node_id: remote,
            addr: "first".into(),
            last_seen_unix_secs: 100,
        });
        table.insert(KBucketEntry {
            node_id: remote,
            addr: "updated".into(),
            last_seen_unix_secs: 200,
        });
        assert_eq!(table.node_count(), 1);
    }

    /// closest_nodes returns nodes sorted by XOR distance.
    ///
    /// The closest node to the target should be first in the result.
    #[test]
    fn closest_nodes_sorted() {
        let local = make_node_id(0);
        let mut table = RoutingTable::new(local);

        // Insert several nodes.
        for seed in 1..10u8 {
            let id = make_node_id(seed);
            table.insert(KBucketEntry {
                node_id: id,
                addr: format!("node-{seed}"),
                last_seen_unix_secs: 0,
            });
        }

        let target = make_node_id(5);
        let closest = table.closest_nodes(&target, 3);
        assert!(closest.len() <= 3);

        // Verify sorted order — each successive node should be farther.
        for pair in closest.windows(2) {
            let d0 = target.xor_distance(&pair[0].node_id);
            let d1 = target.xor_distance(&pair[1].node_id);
            assert!(d0 <= d1, "closest_nodes not sorted by distance");
        }
    }

    // ── DhtNode message handling ────────────────────────────────────

    /// PING produces a PONG response.
    ///
    /// The basic liveness check must always be answered.
    #[test]
    fn ping_produces_pong() {
        let local = make_node_id(0);
        let mut node = DhtNode::new(local, NetworkId::TEST);

        let sender = make_node_id(1);
        let response = node.handle_message(DhtMessage::Ping {
            network_id: NetworkId::TEST,
            sender,
        });

        assert!(matches!(response, Some(DhtMessage::Pong { .. })));
    }

    /// Wrong network ID silently discards the message.
    ///
    /// Cross-network isolation: test network messages must not affect
    /// production nodes.
    #[test]
    fn wrong_network_id_discarded() {
        let local = make_node_id(0);
        let mut node = DhtNode::new(local, NetworkId::TEST);

        let response = node.handle_message(DhtMessage::Ping {
            network_id: NetworkId::PRODUCTION,
            sender: make_node_id(1),
        });

        assert!(response.is_none());
        assert_eq!(node.routing_table().node_count(), 0);
    }

    /// STORE + FIND_VALUE round-trip.
    ///
    /// Storing a value and finding it should return the same value.
    #[test]
    fn store_and_find_value() {
        let local = make_node_id(0);
        let mut node = DhtNode::new(local, NetworkId::TEST);
        let sender = make_node_id(1);

        let key = [42u8; NODE_ID_LEN];
        node.handle_message(DhtMessage::Store {
            network_id: NetworkId::TEST,
            sender,
            key,
            value: "192.168.1.1:6881".into(),
        });

        let response = node.handle_message(DhtMessage::FindValue {
            network_id: NetworkId::TEST,
            sender,
            key,
        });

        match response {
            Some(DhtMessage::FoundValue { value, .. }) => {
                assert_eq!(value, Some("192.168.1.1:6881".into()));
            }
            other => panic!("expected FoundValue, got {other:?}"),
        }
    }

    /// FIND_VALUE for missing key returns closest nodes.
    ///
    /// When the value is not stored locally, the response should contain
    /// the K closest nodes for iterative lookup.
    #[test]
    fn find_value_missing_returns_nodes() {
        let local = make_node_id(0);
        let mut node = DhtNode::new(local, NetworkId::TEST);

        // Populate routing table.
        for seed in 1..5u8 {
            let id = make_node_id(seed);
            node.routing_table_mut().insert(KBucketEntry {
                node_id: id,
                addr: format!("node-{seed}"),
                last_seen_unix_secs: 0,
            });
        }

        let key = [99u8; NODE_ID_LEN];
        let response = node.handle_message(DhtMessage::FindValue {
            network_id: NetworkId::TEST,
            sender: make_node_id(10),
            key,
        });

        match response {
            Some(DhtMessage::FoundValue { value, nodes, .. }) => {
                assert!(value.is_none());
                assert!(!nodes.is_empty());
            }
            other => panic!("expected FoundValue with nodes, got {other:?}"),
        }
    }

    /// PING updates the routing table with the sender.
    ///
    /// Every message should cause the sender to be added/updated in the
    /// routing table (Kademlia protocol requirement).
    #[test]
    fn ping_updates_routing_table() {
        let local = make_node_id(0);
        let mut node = DhtNode::new(local, NetworkId::TEST);

        node.handle_message(DhtMessage::Ping {
            network_id: NetworkId::TEST,
            sender: make_node_id(1),
        });

        assert_eq!(node.routing_table().node_count(), 1);
    }

    // ── Unknown message forward compatibility (IRC CTCP) ────────────

    /// Unknown message type is silently ignored.
    ///
    /// IRC CTCP: unknown commands don't cause disconnection. Similarly,
    /// a DHT node that receives a message with an unknown type_id should
    /// not panic or return an error — it silently ignores the message
    /// while still recording the sender as alive.
    #[test]
    fn unknown_message_silently_ignored() {
        let local = make_node_id(0);
        let mut node = DhtNode::new(local, NetworkId::TEST);
        let sender = make_node_id(1);

        let response = node.handle_message(DhtMessage::Unknown {
            network_id: NetworkId::TEST,
            sender,
            type_id: 0xFF,
            payload: vec![0xDE, 0xAD],
        });

        assert!(response.is_none());
    }

    /// Unknown message still updates the routing table.
    ///
    /// The sender is alive (we received a message from them) even if we
    /// don't understand the message type. Their routing table entry
    /// should be created/updated.
    #[test]
    fn unknown_message_updates_routing_table() {
        let local = make_node_id(0);
        let mut node = DhtNode::new(local, NetworkId::TEST);
        let sender = make_node_id(1);

        assert_eq!(node.routing_table().node_count(), 0);

        node.handle_message(DhtMessage::Unknown {
            network_id: NetworkId::TEST,
            sender,
            type_id: 42,
            payload: vec![],
        });

        assert_eq!(node.routing_table().node_count(), 1);
    }

    /// Unknown message with wrong network is discarded.
    ///
    /// Cross-network isolation applies to unknown messages too.
    #[test]
    fn unknown_message_wrong_network_discarded() {
        let local = make_node_id(0);
        let mut node = DhtNode::new(local, NetworkId::TEST);

        node.handle_message(DhtMessage::Unknown {
            network_id: NetworkId::PRODUCTION,
            sender: make_node_id(1),
            type_id: 42,
            payload: vec![1, 2, 3],
        });

        assert_eq!(node.routing_table().node_count(), 0);
    }
}
