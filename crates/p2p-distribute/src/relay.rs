// SPDX-License-Identifier: MIT OR Apache-2.0

//! NAT traversal relay — connection relaying for peers behind restrictive
//! NATs and firewalls.
//!
//! ## What
//!
//! Provides relay node discovery, relay circuit management, and hole-
//! punching coordination so that peers behind symmetric NATs or strict
//! firewalls can still participate in the P2P swarm.
//!
//! ## Why — lessons from CnCNet tunnel infrastructure
//!
//! CnCNet has operated community-funded relay tunnels since 2009, keeping
//! classic C&C multiplayer alive for players behind restrictive networks.
//! Key lessons from their architecture:
//!
//! - **Most home players need relay help.** Carrier-grade NAT (CG-NAT),
//!   university firewalls, and mobile networks prevent direct peer
//!   connections. CnCNet reports that the majority of their game sessions
//!   route through tunnel servers.
//! - **Relay is a fallback, not primary path.** Direct connections are
//!   always preferred for latency and throughput. Relay adds ~50-100ms
//!   RTT plus bandwidth bottleneck at the relay. CnCNet uses relay only
//!   when direct connection fails.
//! - **Community-funded relays work.** CnCNet's relay infrastructure runs
//!   on DigitalOcean with community Patreon funding. The lesson: relay
//!   nodes don't need centralised infrastructure — any well-connected
//!   node can volunteer as a relay.
//! - **Geographic relay selection matters.** CnCNet runs tunnels in
//!   multiple regions. Routing US↔US traffic through a EU relay doubles
//!   latency unnecessarily. Relay selection should prefer topologically
//!   close relays.
//! - **Relay abuse prevention is essential.** Without rate limits, a
//!   single user can monopolise relay bandwidth. CnCNet applies per-user
//!   bandwidth caps on their tunnels.
//!
//! ## How
//!
//! - [`RelayNode`]: Represents a known relay node with capacity and
//!   location metadata.
//! - [`RelayCircuit`]: An active relayed connection between two peers
//!   through a relay node.
//! - [`RelayRegistry`]: Tracks available relay nodes, selects the best
//!   relay for a given connection, and manages circuit lifecycle.
//! - [`HolePunchAttempt`]: Coordinates simultaneous TCP/UDP open between
//!   two NATed peers, using a relay as the signalling channel.
//!
//! The relay system integrates with the existing `PhiDetector` for relay
//! node health monitoring and `BandwidthThrottle` for per-circuit rate
//! limiting.

use std::time::{Duration, Instant};

// ── Constants ───────────────────────────────────────────────────────

/// Maximum time to wait for a hole-punch attempt before falling back to
/// relay.
///
/// Most NATs will respond within 2 seconds if they're going to. Beyond
/// this, relay fallback is faster than waiting.
const HOLE_PUNCH_TIMEOUT: Duration = Duration::from_secs(3);

/// Maximum concurrent circuits a single relay node should carry.
///
/// Prevents a single relay from being overloaded. CnCNet experience
/// shows that 100-200 concurrent sessions per tunnel is a practical
/// limit for commodity VPS hardware.
const DEFAULT_MAX_CIRCUITS_PER_RELAY: u32 = 128;

/// Maximum bandwidth (bytes/sec) allocated per relay circuit.
///
/// Content downloads through relay should be rate-limited to prevent
/// relay abuse. 256 KiB/s is sufficient for piece delivery while
/// protecting relay bandwidth.
const DEFAULT_CIRCUIT_BANDWIDTH_LIMIT: u64 = 256 * 1024;

/// Relay circuit idle timeout — circuits with no traffic are torn down.
const CIRCUIT_IDLE_TIMEOUT: Duration = Duration::from_secs(120);

/// Maximum number of relay nodes tracked in the registry.
const MAX_RELAY_NODES: usize = 64;

// ── NAT type ────────────────────────────────────────────────────────

/// Detected NAT type affecting connectivity strategy.
///
/// NAT classification determines which connection strategies are viable.
/// This follows RFC 3489 / STUN classification with practical additions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NatType {
    /// No NAT — public IP, all ports reachable.
    Open,
    /// Full-cone NAT — any external host can reach mapped port after
    /// first outbound packet. Direct connection usually works.
    FullCone,
    /// Restricted-cone — only hosts the NAT has sent to can reply.
    /// Hole-punching usually succeeds.
    RestrictedCone,
    /// Port-restricted cone — both IP and port must match. Hole-punching
    /// sometimes works.
    PortRestricted,
    /// Symmetric NAT — unique mapping per destination. Hole-punching
    /// rarely works; relay is usually required. Common on mobile/CG-NAT.
    Symmetric,
    /// Unknown — detection hasn't been performed yet.
    Unknown,
}

impl NatType {
    /// Returns whether direct connection (without relay) is likely to
    /// succeed between two peers with given NAT types.
    pub fn can_direct_connect(self, remote: Self) -> bool {
        match (self, remote) {
            // At least one side is open — always works.
            (NatType::Open, _) | (_, NatType::Open) => true,
            // Full cone on either side — works.
            (NatType::FullCone, _) | (_, NatType::FullCone) => true,
            // Restricted cone on both — hole-punch usually works.
            (NatType::RestrictedCone, NatType::RestrictedCone) => true,
            // Port-restricted + restricted — might work.
            (NatType::PortRestricted, NatType::RestrictedCone) => true,
            (NatType::RestrictedCone, NatType::PortRestricted) => true,
            // Symmetric on either side — unlikely without relay.
            (NatType::Symmetric, _) | (_, NatType::Symmetric) => false,
            // Both port-restricted — unlikely.
            (NatType::PortRestricted, NatType::PortRestricted) => false,
            // Unknown — be optimistic, try direct first.
            (NatType::Unknown, _) | (_, NatType::Unknown) => true,
        }
    }

    /// Returns whether hole-punching has a reasonable chance of success.
    pub fn supports_hole_punch(self, remote: Self) -> bool {
        !matches!(
            (self, remote),
            (NatType::Symmetric, NatType::Symmetric)
                | (NatType::Symmetric, NatType::PortRestricted)
                | (NatType::PortRestricted, NatType::Symmetric)
        )
    }
}

// ── Relay node ──────────────────────────────────────────────────────

/// A known relay node that can bridge connections between NATed peers.
///
/// Relay nodes are discovered via DHT, PEX, or static configuration.
/// Each node has capacity limits and health monitoring via the phi
/// accrual failure detector pattern.
#[derive(Debug, Clone)]
pub struct RelayNode {
    /// Unique identifier (typically derived from the relay's peer ID).
    id: [u8; 20],
    /// Network address of the relay (host:port as string).
    address: String,
    /// Maximum circuits this relay advertises it can carry.
    max_circuits: u32,
    /// Currently active circuits through this relay (as observed).
    active_circuits: u32,
    /// Region hint (e.g. "eu-west", "us-east") for geographic selection.
    region: Option<String>,
    /// When we last heard from this relay (heartbeat or response).
    last_seen: Instant,
    /// Measured round-trip latency to this relay.
    latency: Option<Duration>,
    /// Whether this relay has been verified as operational.
    verified: bool,
}

impl RelayNode {
    /// Creates a new relay node record.
    pub fn new(id: [u8; 20], address: String, now: Instant) -> Self {
        Self {
            id,
            address,
            max_circuits: DEFAULT_MAX_CIRCUITS_PER_RELAY,
            active_circuits: 0,
            region: None,
            last_seen: now,
            latency: None,
            verified: false,
        }
    }

    /// Sets the maximum circuit capacity for this relay.
    pub fn with_max_circuits(mut self, max: u32) -> Self {
        self.max_circuits = max;
        self
    }

    /// Sets the region hint for geographic selection.
    pub fn with_region(mut self, region: String) -> Self {
        self.region = Some(region);
        self
    }

    /// Returns the relay's unique identifier.
    pub fn id(&self) -> &[u8; 20] {
        &self.id
    }

    /// Returns the relay's network address.
    pub fn address(&self) -> &str {
        &self.address
    }

    /// Returns the region hint, if known.
    pub fn region(&self) -> Option<&str> {
        self.region.as_deref()
    }

    /// Returns whether this relay has available capacity.
    pub fn has_capacity(&self) -> bool {
        self.active_circuits < self.max_circuits
    }

    /// Returns the fraction of capacity used (0.0 = empty, 1.0 = full).
    pub fn load_factor(&self) -> f64 {
        if self.max_circuits == 0 {
            return 1.0;
        }
        self.active_circuits as f64 / self.max_circuits as f64
    }

    /// Returns the measured latency, if known.
    pub fn latency(&self) -> Option<Duration> {
        self.latency
    }

    /// Records a heartbeat from this relay.
    pub fn record_heartbeat(&mut self, latency: Duration, now: Instant) {
        self.last_seen = now;
        self.latency = Some(latency);
        self.verified = true;
    }

    /// Returns seconds since last contact.
    pub fn age(&self, now: Instant) -> Duration {
        now.duration_since(self.last_seen)
    }

    /// Returns whether this relay has been verified operational.
    pub fn is_verified(&self) -> bool {
        self.verified
    }

    /// Updates the observed active circuit count.
    pub fn set_active_circuits(&mut self, count: u32) {
        self.active_circuits = count;
    }

    /// Selection score: lower latency + more capacity = better.
    ///
    /// Score is in `[0.0, 1.0]` where higher is better.
    pub fn selection_score(&self) -> f64 {
        // Capacity component: prefer relays with headroom.
        let capacity_score = 1.0 - self.load_factor();

        // Latency component: prefer low-latency relays.
        let latency_score = match self.latency {
            Some(d) => {
                let ms = d.as_millis() as f64;
                // 0ms → 1.0, 500ms → 0.0, clamped.
                1.0 - (ms / 500.0).min(1.0)
            }
            None => 0.5, // Unknown latency → middle ground.
        };

        // Verification bonus: prefer verified relays.
        let verified_bonus = if self.verified { 0.1 } else { 0.0 };

        (capacity_score * 0.4 + latency_score * 0.5 + verified_bonus).min(1.0)
    }
}

// ── Relay circuit ───────────────────────────────────────────────────

/// An active relayed connection between two peers through a relay node.
///
/// Circuits have bandwidth limits and idle timeouts. They are the
/// fallback path when direct connection and hole-punching both fail.
#[derive(Debug, Clone)]
pub struct RelayCircuit {
    /// Unique circuit identifier.
    circuit_id: u64,
    /// Relay node carrying this circuit.
    relay_id: [u8; 20],
    /// Our local peer ID.
    local_peer: [u8; 20],
    /// Remote peer ID.
    remote_peer: [u8; 20],
    /// When this circuit was established.
    established_at: Instant,
    /// When data last flowed through this circuit.
    last_activity: Instant,
    /// Total bytes relayed through this circuit.
    bytes_relayed: u64,
    /// Bandwidth limit in bytes per second.
    bandwidth_limit: u64,
}

impl RelayCircuit {
    /// Creates a new relay circuit record.
    pub fn new(
        circuit_id: u64,
        relay_id: [u8; 20],
        local_peer: [u8; 20],
        remote_peer: [u8; 20],
        now: Instant,
    ) -> Self {
        Self {
            circuit_id,
            relay_id,
            local_peer,
            remote_peer,
            established_at: now,
            last_activity: now,
            bytes_relayed: 0,
            bandwidth_limit: DEFAULT_CIRCUIT_BANDWIDTH_LIMIT,
        }
    }

    /// Returns the circuit's unique identifier.
    pub fn circuit_id(&self) -> u64 {
        self.circuit_id
    }

    /// Returns the relay node's identifier.
    pub fn relay_id(&self) -> &[u8; 20] {
        &self.relay_id
    }

    /// Returns the remote peer's identifier.
    pub fn remote_peer(&self) -> &[u8; 20] {
        &self.remote_peer
    }

    /// Records data transfer activity on this circuit.
    pub fn record_activity(&mut self, bytes: u64, now: Instant) {
        self.bytes_relayed = self.bytes_relayed.saturating_add(bytes);
        self.last_activity = now;
    }

    /// Returns whether this circuit has been idle beyond the timeout.
    pub fn is_idle(&self, now: Instant) -> bool {
        now.duration_since(self.last_activity) >= CIRCUIT_IDLE_TIMEOUT
    }

    /// Returns total bytes relayed.
    pub fn bytes_relayed(&self) -> u64 {
        self.bytes_relayed
    }

    /// Returns the local peer ID.
    pub fn local_peer(&self) -> &[u8; 20] {
        &self.local_peer
    }

    /// Returns how long this circuit has been active.
    pub fn duration(&self, now: Instant) -> Duration {
        now.duration_since(self.established_at)
    }

    /// Returns the bandwidth limit for this circuit.
    pub fn bandwidth_limit(&self) -> u64 {
        self.bandwidth_limit
    }
}

// ── Hole-punch attempt ──────────────────────────────────────────────

/// State of a coordinated hole-punch attempt between two NATed peers.
///
/// The relay acts as a signalling channel: both peers exchange their
/// external addresses via the relay, then simultaneously attempt to
/// connect to each other's external endpoint. If both NATs create
/// mappings in time, the connection succeeds without needing the relay
/// for data transfer.
#[derive(Debug, Clone)]
pub struct HolePunchAttempt {
    /// Remote peer we're trying to hole-punch to.
    remote_peer: [u8; 20],
    /// Remote peer's external address (from relay signalling).
    remote_address: Option<String>,
    /// When the attempt started.
    started_at: Instant,
    /// Current state of the attempt.
    state: HolePunchState,
}

/// State machine for hole-punch progression.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HolePunchState {
    /// Waiting for the remote peer's address from the relay.
    WaitingForAddress,
    /// Address received, attempting simultaneous connect.
    Punching,
    /// Connection established — relay no longer needed for data.
    Succeeded,
    /// Timed out or failed — fall back to relay circuit.
    Failed,
}

impl HolePunchAttempt {
    /// Creates a new hole-punch attempt.
    pub fn new(remote_peer: [u8; 20], now: Instant) -> Self {
        Self {
            remote_peer,
            remote_address: None,
            started_at: now,
            state: HolePunchState::WaitingForAddress,
        }
    }

    /// Records that we received the remote peer's external address.
    pub fn set_remote_address(&mut self, address: String) {
        self.remote_address = Some(address);
        self.state = HolePunchState::Punching;
    }

    /// Marks the attempt as succeeded.
    pub fn mark_succeeded(&mut self) {
        self.state = HolePunchState::Succeeded;
    }

    /// Marks the attempt as failed.
    pub fn mark_failed(&mut self) {
        self.state = HolePunchState::Failed;
    }

    /// Returns whether this attempt has timed out.
    pub fn is_timed_out(&self, now: Instant) -> bool {
        now.duration_since(self.started_at) >= HOLE_PUNCH_TIMEOUT
            && self.state != HolePunchState::Succeeded
    }

    /// Returns the current state.
    pub fn state(&self) -> HolePunchState {
        self.state
    }

    /// Returns the remote peer's address if known.
    pub fn remote_address(&self) -> Option<&str> {
        self.remote_address.as_deref()
    }

    /// Returns the remote peer's identity.
    pub fn remote_peer(&self) -> &[u8; 20] {
        &self.remote_peer
    }
}

// ── Relay registry ──────────────────────────────────────────────────

/// Registry of known relay nodes with selection and circuit management.
///
/// ```
/// use p2p_distribute::relay::{RelayRegistry, RelayNode, NatType};
/// use std::time::Instant;
///
/// let mut registry = RelayRegistry::new();
/// let now = Instant::now();
///
/// let relay = RelayNode::new([1u8; 20], "relay.example.com:6881".into(), now);
/// registry.add_relay(relay).unwrap();
///
/// assert_eq!(registry.relay_count(), 1);
/// ```
pub struct RelayRegistry {
    /// Known relay nodes.
    relays: Vec<RelayNode>,
    /// Active circuits through various relays.
    circuits: Vec<RelayCircuit>,
    /// Pending hole-punch attempts.
    punch_attempts: Vec<HolePunchAttempt>,
    /// Next circuit ID to assign.
    next_circuit_id: u64,
}

/// Errors from relay registry operations.
#[derive(Debug, thiserror::Error)]
pub enum RelayError {
    /// Registry is full.
    #[error("relay registry full: {max} relays maximum")]
    RegistryFull {
        /// Maximum capacity.
        max: usize,
    },

    /// Relay with this ID already exists.
    #[error("duplicate relay: {id:?}")]
    DuplicateRelay {
        /// The relay ID that already exists.
        id: [u8; 20],
    },

    /// No relay nodes available for circuit creation.
    #[error("no relays available with capacity")]
    NoRelaysAvailable,

    /// Circuit not found.
    #[error("circuit {circuit_id} not found")]
    CircuitNotFound {
        /// The missing circuit ID.
        circuit_id: u64,
    },
}

impl RelayRegistry {
    /// Creates a new empty relay registry.
    pub fn new() -> Self {
        Self {
            relays: Vec::with_capacity(16),
            circuits: Vec::new(),
            punch_attempts: Vec::new(),
            next_circuit_id: 1,
        }
    }

    /// Adds a relay node to the registry.
    pub fn add_relay(&mut self, relay: RelayNode) -> Result<(), RelayError> {
        if self.relays.len() >= MAX_RELAY_NODES {
            return Err(RelayError::RegistryFull {
                max: MAX_RELAY_NODES,
            });
        }
        if self.relays.iter().any(|r| r.id == relay.id) {
            return Err(RelayError::DuplicateRelay { id: relay.id });
        }
        self.relays.push(relay);
        Ok(())
    }

    /// Selects the best relay for a new circuit, preferring low-latency
    /// relays with available capacity.
    ///
    /// Optionally filters by region if specified.
    pub fn select_relay(&self, preferred_region: Option<&str>) -> Option<&RelayNode> {
        let mut candidates: Vec<&RelayNode> = self
            .relays
            .iter()
            .filter(|r| r.has_capacity() && r.is_verified())
            .collect();

        if candidates.is_empty() {
            // Fall back to unverified relays with capacity.
            candidates = self.relays.iter().filter(|r| r.has_capacity()).collect();
        }

        if candidates.is_empty() {
            return None;
        }

        // Prefer matching region, then sort by selection score.
        candidates.sort_by(|a, b| {
            let a_region_match = preferred_region
                .map(|pr| a.region() == Some(pr))
                .unwrap_or(false);
            let b_region_match = preferred_region
                .map(|pr| b.region() == Some(pr))
                .unwrap_or(false);

            b_region_match.cmp(&a_region_match).then_with(|| {
                b.selection_score()
                    .partial_cmp(&a.selection_score())
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
        });

        candidates.first().copied()
    }

    /// Creates a relay circuit through the best available relay.
    pub fn create_circuit(
        &mut self,
        local_peer: [u8; 20],
        remote_peer: [u8; 20],
        preferred_region: Option<&str>,
        now: Instant,
    ) -> Result<u64, RelayError> {
        let relay_id = self
            .select_relay(preferred_region)
            .map(|r| r.id)
            .ok_or(RelayError::NoRelaysAvailable)?;

        let circuit_id = self.next_circuit_id;
        self.next_circuit_id = self.next_circuit_id.saturating_add(1);

        let circuit = RelayCircuit::new(circuit_id, relay_id, local_peer, remote_peer, now);
        self.circuits.push(circuit);

        // Update relay's active circuit count.
        if let Some(relay) = self.relays.iter_mut().find(|r| r.id == relay_id) {
            relay.active_circuits = relay.active_circuits.saturating_add(1);
        }

        Ok(circuit_id)
    }

    /// Closes a relay circuit and frees capacity on the relay.
    pub fn close_circuit(&mut self, circuit_id: u64) -> Result<(), RelayError> {
        let idx = self
            .circuits
            .iter()
            .position(|c| c.circuit_id == circuit_id)
            .ok_or(RelayError::CircuitNotFound { circuit_id })?;

        let circuit = self.circuits.remove(idx);

        // Decrement relay's active circuit count.
        if let Some(relay) = self.relays.iter_mut().find(|r| r.id == *circuit.relay_id()) {
            relay.active_circuits = relay.active_circuits.saturating_sub(1);
        }

        Ok(())
    }

    /// Initiates a hole-punch attempt to the given remote peer.
    pub fn start_hole_punch(&mut self, remote_peer: [u8; 20], now: Instant) {
        // Remove any existing attempt for this peer.
        self.punch_attempts.retain(|a| a.remote_peer != remote_peer);
        self.punch_attempts
            .push(HolePunchAttempt::new(remote_peer, now));
    }

    /// Returns a mutable reference to a pending hole-punch attempt.
    pub fn get_punch_attempt(&mut self, remote_peer: &[u8; 20]) -> Option<&mut HolePunchAttempt> {
        self.punch_attempts
            .iter_mut()
            .find(|a| &a.remote_peer == remote_peer)
    }

    /// Cleans up timed-out hole-punch attempts and idle circuits.
    pub fn cleanup(&mut self, now: Instant) -> CleanupResult {
        let mut result = CleanupResult::default();

        // Expire hole-punch attempts.
        let before_punches = self.punch_attempts.len();
        self.punch_attempts.retain(|a| {
            !(a.is_timed_out(now)
                || a.state() == HolePunchState::Succeeded
                || a.state() == HolePunchState::Failed)
        });
        result.expired_punches = before_punches.saturating_sub(self.punch_attempts.len());

        // Close idle circuits.
        let idle_ids: Vec<u64> = self
            .circuits
            .iter()
            .filter(|c| c.is_idle(now))
            .map(|c| c.circuit_id())
            .collect();

        for id in &idle_ids {
            let _ = self.close_circuit(*id);
        }
        result.closed_idle_circuits = idle_ids.len();

        result
    }

    /// Returns the number of known relay nodes.
    pub fn relay_count(&self) -> usize {
        self.relays.len()
    }

    /// Returns the number of active circuits.
    pub fn circuit_count(&self) -> usize {
        self.circuits.len()
    }

    /// Returns a relay node by ID.
    pub fn get_relay(&self, id: &[u8; 20]) -> Option<&RelayNode> {
        self.relays.iter().find(|r| &r.id == id)
    }

    /// Returns a mutable reference to a relay by ID.
    pub fn get_relay_mut(&mut self, id: &[u8; 20]) -> Option<&mut RelayNode> {
        self.relays.iter_mut().find(|r| &r.id == id)
    }
}

impl Default for RelayRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// Summary of cleanup operations performed.
#[derive(Debug, Clone, Default)]
pub struct CleanupResult {
    /// Hole-punch attempts that expired.
    pub expired_punches: usize,
    /// Idle circuits that were closed.
    pub closed_idle_circuits: usize,
}

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── NatType ─────────────────────────────────────────────────────

    /// Open NAT can connect to anything.
    ///
    /// A peer with a public IP should always succeed with direct
    /// connections regardless of remote NAT type.
    #[test]
    fn open_nat_connects_to_all() {
        for remote in &[
            NatType::Open,
            NatType::FullCone,
            NatType::RestrictedCone,
            NatType::PortRestricted,
            NatType::Symmetric,
            NatType::Unknown,
        ] {
            assert!(
                NatType::Open.can_direct_connect(*remote),
                "Open should connect to {remote:?}"
            );
        }
    }

    /// Symmetric NAT cannot direct-connect to most types.
    ///
    /// Symmetric NATs create unique mappings per destination, making
    /// hole-punching unreliable. Only Open/FullCone peers can reach
    /// symmetric peers.
    #[test]
    fn symmetric_nat_needs_relay() {
        assert!(!NatType::Symmetric.can_direct_connect(NatType::Symmetric));
        assert!(!NatType::Symmetric.can_direct_connect(NatType::PortRestricted));
        assert!(!NatType::Symmetric.can_direct_connect(NatType::RestrictedCone));
        // But open/full-cone can reach symmetric.
        assert!(NatType::Open.can_direct_connect(NatType::Symmetric));
        assert!(NatType::FullCone.can_direct_connect(NatType::Symmetric));
    }

    /// Hole-punching fails for symmetric-symmetric pairs.
    ///
    /// Both sides create different mappings, making simultaneous connect
    /// impossible.
    #[test]
    fn hole_punch_fails_symmetric_pair() {
        assert!(!NatType::Symmetric.supports_hole_punch(NatType::Symmetric));
    }

    /// Restricted-cone peers can hole-punch each other.
    ///
    /// Both sides create predictable mappings, so simultaneous connect
    /// works reliably.
    #[test]
    fn restricted_cone_can_hole_punch() {
        assert!(NatType::RestrictedCone.supports_hole_punch(NatType::RestrictedCone));
    }

    // ── RelayNode ───────────────────────────────────────────────────

    /// New relay has full capacity.
    ///
    /// Fresh relay nodes should report available capacity.
    #[test]
    fn new_relay_has_capacity() {
        let now = Instant::now();
        let relay = RelayNode::new([1u8; 20], "relay:6881".into(), now);
        assert!(relay.has_capacity());
        assert!(relay.load_factor() < 0.01);
    }

    /// Selection score reflects latency and capacity.
    ///
    /// A verified relay with low latency and empty load should score
    /// higher than an unverified one.
    #[test]
    fn selection_score_verified_better() {
        let now = Instant::now();
        let mut verified = RelayNode::new([1u8; 20], "fast:6881".into(), now);
        verified.record_heartbeat(Duration::from_millis(20), now);

        let unverified = RelayNode::new([2u8; 20], "slow:6881".into(), now);

        assert!(
            verified.selection_score() > unverified.selection_score(),
            "verified={}, unverified={}",
            verified.selection_score(),
            unverified.selection_score()
        );
    }

    /// Region can be set on relay node.
    ///
    /// Geographic metadata aids relay selection.
    #[test]
    fn relay_with_region() {
        let now = Instant::now();
        let relay =
            RelayNode::new([1u8; 20], "relay:6881".into(), now).with_region("eu-west".into());
        assert_eq!(relay.region(), Some("eu-west"));
    }

    // ── RelayCircuit ────────────────────────────────────────────────

    /// New circuit starts active with zero bytes.
    ///
    /// Freshly created circuits have no data relayed yet.
    #[test]
    fn new_circuit_zero_bytes() {
        let now = Instant::now();
        let circuit = RelayCircuit::new(1, [1u8; 20], [2u8; 20], [3u8; 20], now);
        assert_eq!(circuit.bytes_relayed(), 0);
        assert!(!circuit.is_idle(now));
    }

    /// Idle circuit detection respects timeout.
    ///
    /// Circuits with no activity beyond the idle timeout should be
    /// flagged for cleanup.
    #[test]
    fn circuit_idle_detection() {
        let now = Instant::now();
        let circuit = RelayCircuit::new(1, [1u8; 20], [2u8; 20], [3u8; 20], now);

        assert!(!circuit.is_idle(now));
        let later = now + CIRCUIT_IDLE_TIMEOUT + Duration::from_secs(1);
        assert!(circuit.is_idle(later));
    }

    /// Activity resets idle timer.
    ///
    /// Data transfer should prevent circuit from being idle-closed.
    #[test]
    fn activity_resets_idle() {
        let now = Instant::now();
        let mut circuit = RelayCircuit::new(1, [1u8; 20], [2u8; 20], [3u8; 20], now);

        let mid = now + Duration::from_secs(60);
        circuit.record_activity(1024, mid);

        // Should not be idle 60s after activity.
        let check = mid + Duration::from_secs(60);
        assert!(!circuit.is_idle(check));
    }

    // ── HolePunchAttempt ────────────────────────────────────────────

    /// Hole-punch times out after deadline.
    ///
    /// Failed hole-punches should trigger relay fallback.
    #[test]
    fn hole_punch_timeout() {
        let now = Instant::now();
        let attempt = HolePunchAttempt::new([1u8; 20], now);

        assert!(!attempt.is_timed_out(now));
        let later = now + HOLE_PUNCH_TIMEOUT + Duration::from_secs(1);
        assert!(attempt.is_timed_out(later));
    }

    /// Successful hole-punch is not timed out.
    ///
    /// Once succeeded, the attempt should not be flagged as timed out
    /// even if the deadline passes.
    #[test]
    fn successful_punch_not_timed_out() {
        let now = Instant::now();
        let mut attempt = HolePunchAttempt::new([1u8; 20], now);
        attempt.mark_succeeded();

        let later = now + HOLE_PUNCH_TIMEOUT + Duration::from_secs(10);
        assert!(!attempt.is_timed_out(later));
    }

    /// Address progresses state to Punching.
    ///
    /// Receiving the remote peer's address advances the state machine.
    #[test]
    fn address_progresses_state() {
        let now = Instant::now();
        let mut attempt = HolePunchAttempt::new([1u8; 20], now);
        assert_eq!(attempt.state(), HolePunchState::WaitingForAddress);

        attempt.set_remote_address("1.2.3.4:6881".into());
        assert_eq!(attempt.state(), HolePunchState::Punching);
        assert_eq!(attempt.remote_address(), Some("1.2.3.4:6881"));
    }

    // ── RelayRegistry ───────────────────────────────────────────────

    /// Registry rejects duplicate relay IDs.
    ///
    /// Prevents double-tracking the same relay node.
    #[test]
    fn registry_rejects_duplicate() {
        let mut reg = RelayRegistry::new();
        let now = Instant::now();

        reg.add_relay(RelayNode::new([1u8; 20], "a:6881".into(), now))
            .unwrap();
        let err = reg
            .add_relay(RelayNode::new([1u8; 20], "b:6881".into(), now))
            .unwrap_err();
        assert!(err.to_string().contains("duplicate"), "err = {err}");
    }

    /// Circuit creation and closure adjusts relay capacity.
    ///
    /// Active circuit counts must track correctly to prevent over-
    /// allocation.
    #[test]
    fn circuit_lifecycle() {
        let mut reg = RelayRegistry::new();
        let now = Instant::now();

        let mut relay = RelayNode::new([1u8; 20], "relay:6881".into(), now);
        relay.record_heartbeat(Duration::from_millis(10), now);
        reg.add_relay(relay).unwrap();

        let cid = reg.create_circuit([2u8; 20], [3u8; 20], None, now).unwrap();
        assert_eq!(reg.circuit_count(), 1);

        reg.close_circuit(cid).unwrap();
        assert_eq!(reg.circuit_count(), 0);
    }

    /// Select relay prefers matching region.
    ///
    /// Geographic affinity reduces relay-induced latency.
    #[test]
    fn select_prefers_region() {
        let mut reg = RelayRegistry::new();
        let now = Instant::now();

        let mut eu = RelayNode::new([1u8; 20], "eu:6881".into(), now).with_region("eu-west".into());
        eu.record_heartbeat(Duration::from_millis(100), now);

        let mut us = RelayNode::new([2u8; 20], "us:6881".into(), now).with_region("us-east".into());
        us.record_heartbeat(Duration::from_millis(50), now);

        reg.add_relay(eu).unwrap();
        reg.add_relay(us).unwrap();

        // Prefer EU relay for EU request.
        let selected = reg.select_relay(Some("eu-west")).unwrap();
        assert_eq!(selected.region(), Some("eu-west"));
    }

    /// Cleanup removes idle circuits and expired punches.
    ///
    /// Prevents resource leaks from forgotten circuits.
    #[test]
    fn cleanup_removes_idle() {
        let mut reg = RelayRegistry::new();
        let now = Instant::now();

        let mut relay = RelayNode::new([1u8; 20], "relay:6881".into(), now);
        relay.record_heartbeat(Duration::from_millis(10), now);
        reg.add_relay(relay).unwrap();

        reg.create_circuit([2u8; 20], [3u8; 20], None, now).unwrap();
        reg.start_hole_punch([4u8; 20], now);

        let later = now + CIRCUIT_IDLE_TIMEOUT + Duration::from_secs(10);
        let result = reg.cleanup(later);

        assert_eq!(result.closed_idle_circuits, 1);
        assert_eq!(result.expired_punches, 1);
    }

    /// No relays available returns error.
    ///
    /// Attempting to create a circuit with no relays should fail
    /// gracefully, not panic.
    #[test]
    fn no_relays_error() {
        let mut reg = RelayRegistry::new();
        let now = Instant::now();

        let err = reg
            .create_circuit([1u8; 20], [2u8; 20], None, now)
            .unwrap_err();
        assert!(err.to_string().contains("no relays"), "err = {err}");
    }
}
