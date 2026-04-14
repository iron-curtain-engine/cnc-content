# p2p-distribute

A pure-Rust, BitTorrent-compatible content distribution engine. Download
files from HTTP mirrors and P2P swarms simultaneously, stream media before
the download completes, and seed content back to the network — all with
pluggable transports, zero `unsafe`, and only 4 required dependencies.

[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue)](LICENSE-MIT)
[![Rust: 1.89+](https://img.shields.io/badge/rust-1.89%2B-orange)]()

```
                                ┌─────────────────┐
                                │  .torrent file   │
                                │  or magnet URI   │
                                └────────┬────────┘
                                         │
                    ┌────────────────────┼────────────────────┐
                    ▼                    ▼                    ▼
            ┌──────────────┐   ┌──────────────┐   ┌──────────────┐
            │  HTTP Mirror │   │  BT Swarm    │   │  Bridge Node │
            │  (BEP 19)    │   │  Peer        │   │  (HTTP→P2P)  │
            └──────┬───────┘   └──────┬───────┘   └──────┬───────┘
                   │                  │                   │
                   └──────────────────┼───────────────────┘
                                      ▼
                          ┌───────────────────────┐
                          │   PieceCoordinator    │
                          │  ─────────────────    │
                          │  SHA-1 verify each    │
                          │  piece · rarest-first │
                          │  selection · endgame  │
                          │  mode · retry/rotate  │
                          └───────────┬───────────┘
                                      │
                        ┌─────────────┼─────────────┐
                        ▼                           ▼
               ┌──────────────┐           ┌──────────────────┐
               │ FileStorage  │           │ StreamingReader   │
               │ (piece→disk) │           │ (Read+Seek over   │
               └──────────────┘           │  partial download)│
                                          └──────────────────┘
```

## Why this crate exists

Distributing large files from a central server is expensive and fragile.
A popular 500 MB mod downloaded 10,000 times generates 5 TB of egress per
month. At CDN rates, that is $50–450/month — per file. BitTorrent inverts
that equation: the more popular a file, the more peers seed it, and the
faster it downloads.

`p2p-distribute` provides the low-level engine for this. It handles piece
scheduling, hash verification, peer management, streaming playback, and
all the protocol machinery needed to participate in a BitTorrent swarm —
without pulling in a full BitTorrent client as a dependency. You bring
your own BT backend (or use the built-in HTTP web seeds) by implementing
the `Peer` trait.

**Built with interplanetary ambitions.** While its immediate use case is
game content distribution (the [Iron Curtain](https://github.com/iron-curtain-engine)
engine uses it to distribute Command & Conquer game assets), the
architecture is designed for environments where centralized infrastructure
is impossible. Consider a future Mars colony receiving high-definition video
from Earth: a constellation of satellite relay nodes, each caching and
sharing pieces with its neighbours, could bridge the 3–22 minute
light-delay gap far more effectively than point-to-point links. When a
node fails, the swarm self-heals — new nodes replicate from survivors.
P2P is the natural topology for resilient, delay-tolerant, high-throughput
content distribution across any distance, from LAN to interplanetary.

## Quick start

```rust
use p2p_distribute::{PieceCoordinator, CoordinatorConfig, TorrentInfo, WebSeedPeer};

// Describe the content — piece hashes from a .torrent file
let info = TorrentInfo {
    piece_length: 262_144,     // 256 KiB per piece
    piece_hashes: vec![],      // SHA-1 hashes from torrent metadata
    file_size: 10_000_000,     // 10 MB file
    file_name: "soundtrack.zip".into(),
};

let config = CoordinatorConfig::default();
let mut coord = PieceCoordinator::new(info, config);

// Add HTTP mirrors as BEP 19 web seeds
coord.add_peer(Box::new(WebSeedPeer::new(
    "https://mirror1.example.com/soundtrack.zip".into(),
)));
coord.add_peer(Box::new(WebSeedPeer::new(
    "https://mirror2.example.com/soundtrack.zip".into(),
)));

// Download — pieces are fetched from all peers concurrently,
// SHA-1 verified, and written to disk
coord.run(std::path::Path::new("soundtrack.zip"), &mut |progress| {
    match progress {
        p2p_distribute::CoordinatorProgress::PieceComplete { piece_index, .. } => {
            println!("piece {piece_index} verified ✓");
        }
        p2p_distribute::CoordinatorProgress::Complete { elapsed, .. } => {
            println!("download complete in {elapsed:?}");
        }
        _ => {}
    }
}).expect("download failed");
```

### Stream media during download

```rust
use p2p_distribute::{StreamingReader, ByteRangeMap, BufferPolicy};
use std::io::Read;

// Create a streaming reader over a partially-downloaded file
let file_size = 50_000_000; // 50 MB video file
let available = ByteRangeMap::new(file_size); // nothing available yet
let policy = BufferPolicy::default();

let (mut reader, notifier) = StreamingReader::new_streaming(
    std::path::Path::new("cutscene.vqa"),
    available,
    policy,
).unwrap();

// The reader blocks transparently when you read bytes that
// haven't arrived yet. As pieces complete, call
// notifier.notify_piece() to unblock waiting reads.
let mut buf = [0u8; 4096];
let n = reader.read(&mut buf).unwrap(); // blocks until bytes available
```

### Create deterministic .torrent files

```rust
use p2p_distribute::{create_torrent, recommended_piece_length};

let file_path = std::path::Path::new("content.zip");
let piece_length = recommended_piece_length(file_path).unwrap();
let metadata = create_torrent(file_path, piece_length).unwrap();

println!("info_hash: {}", metadata.info_hash);
println!("pieces: {}", metadata.piece_count);
// Same input + same piece_length = identical info_hash every time
```

### Manage multi-download queues

```rust
use p2p_distribute::{BasicSession, SessionConfig, DownloadSession};

let config = SessionConfig::default(); // max 3 concurrent downloads
let mut session = BasicSession::new(config);

// Queue multiple downloads — they execute with FIFO priority,
// bounded by max_concurrent. Pause, resume, remove at any time.
```

### Build group catalogs for managed distribution

```rust
use p2p_distribute::{
    ManifestBuilder, GroupRoster, GroupRole, GroupMember, PeerId,
    HmacSha256Signer, sign_manifest, verify_manifest, HmacSha256Verifier,
};

// A group master builds a signed content catalog
let manifest = ManifestBuilder::new()
    .version(1)
    .add_entry("maps/alpine.map", &[0xAA; 32], 1024)
    .add_entry("music/intro.ogg", &[0xBB; 32], 8192)
    .build()
    .unwrap();

// Sign the catalog so mirrors can verify authenticity
let key = b"shared-group-secret";
let signer = HmacSha256Signer::new(key);
let manifest_bytes = b"serialized manifest content";
let signature = sign_manifest(&signer, manifest_bytes);

// Mirrors verify before replicating
let verifier = HmacSha256Verifier::new(key);
verify_manifest(&verifier, manifest_bytes, &signature).unwrap();
```

## Feature flags

| Flag   | Default | Dependency | What it enables |
| ------ | ------- | ---------- | --------------- |
| `http` | Yes     | `ureq`     | `WebSeedPeer` — fetch pieces via HTTP Range requests (BEP 19) |

With `default-features = false`, only the core coordinator, wire protocol,
streaming, storage, and torrent creation are available — zero network
dependencies.

## Architecture

p2p-distribute is organised into subsystems that compose through traits.
Each subsystem is a self-contained module you can use independently.

### Core: Piece Coordinator

The `PieceCoordinator` is the central download scheduler. It assigns pieces
to peers using rarest-first selection, rotates to alternate peers on failure,
enters endgame mode for the final pieces, and drives the download to
completion.

| Type | Role |
| ---- | ---- |
| `PieceCoordinator` | Piece scheduling, retry rotation, peer blacklisting, min-speed eviction, endgame mode, resume, cancellation |
| `CoordinatorConfig` | Tuning: max concurrent pieces (default 8), max retries (3), endgame threshold, cancel flag |
| `SharedPieceMap` | Lock-free per-piece state via `AtomicU8` — `Needed`, `InFlight`, `Done`, `Failed` |
| `PieceSelection` | Rarest-first selection with streaming priority weighting |
| `EndgameMode` | BEP 3 endgame: broadcast remaining blocks to all peers, cancel duplicates on first completion |
| `DownloadStateMachine` | Formal lifecycle: Queued → Checking → Downloading → Seeding → Completed |
| `BasicSession` | Multi-download queue — schedule N downloads with configurable concurrency (default 3 active) |

### Peer System

The `Peer` trait abstracts any data source into a uniform interface. The
crate provides `WebSeedPeer` (HTTP Range) and `BridgePeer` (HTTP-backed
P2P node). Consumers implement `Peer` for their BitTorrent backend.

| Type | Role |
| ---- | ---- |
| `Peer` trait | `fetch_piece()`, `has_piece()`, `speed_estimate()`, `capabilities()`, `peer_id()` |
| `WebSeedPeer` | BEP 19 HTTP web seed — fetches pieces via Range requests (feature-gated: `http`) |
| `BridgePeer<F>` | Generic peer backed by a closure — use for custom transports |
| `PeerId` | 32-byte cryptographic identity with military callsign display (e.g. `Bravo-Echo-7F`) |
| `PeerPool` | Bounded peer set (default 55) with exponential backoff and composite eviction scoring |
| `PeerTracker` | Per-peer scoring: Speed (0.4) + Reliability (0.3) + Availability (0.2) + Recency (0.1) |
| `PeerStats` | Detailed per-peer metrics: bytes up/down, latency, failure count, exclusion history |
| `TrustLevel` | Progressive trust: Unknown → Provisional → Established → Trusted → Verified |
| `ExclusionScope` | IRC-inspired ban scopes: Local (single peer), Shared (broadcast), Subnet (IP range) |
| `AffinityScorer` | Geographic/topological peer preference: Region (0.25) + Latency (0.40) + Speed (0.35) |
| `ConnectionBudget` | libp2p-style limits: max total (50), per-peer (1), pending (10) |

### Piece Verification & Trust

Every piece is SHA-1 verified against the torrent metadata hash. When
corruption is detected, Merkle sub-piece localisation identifies the exact
bad bytes and which peer sent them.

| Type | Role |
| ---- | ---- |
| `PieceValidator` | Extended validation: Merkle sub-piece localisation, quarantine, retry budget per piece |
| `MerkleTree` | SHA-256 Merkle tree (256 KiB leaves) for byte-range corruption localisation (aMule AICH pattern) |
| `CorruptionLedger` | Blame attribution — records `(byte_offset, peer_id)` to identify which peer sent bad data |
| `CreditLedger` | eMule-style bilateral credit: `min(2 × uploaded / max(1 MB, downloaded), 10.0)` |

### Choking & Upload

Control who gets upload bandwidth and how seeding works.

| Type | Role |
| ---- | ---- |
| `TitForTatChoking` | BEP 3 tit-for-tat with optimistic unchoking, credit-weighted evaluation, auto-scaling slots |
| `AlwaysUnchoke` | Unconditional unchoke — ideal for content distribution where maximizing spread matters more than reciprocity |
| `ChokingStrategy` trait | Pluggable: implement your own upload bandwidth allocation logic |
| `UploadQueue` | XDCC-style bounded upload slots (default 4) with FIFO queue (default 20 pending) |
| `SuperSeedState` | BEP 16 super-seeding — maximize piece diversity from the initial seeder |

### Network & Discovery

| Type | Role |
| ---- | ---- |
| `DhtNode` / `RoutingTable` | Kademlia DHT — O(log N) lookup, K=20 buckets, α=3 parallel queries, 256-bit node IDs |
| `PexMessage` / `PexDeltaTracker` | BEP 11 Peer Exchange gossip (max 50 peers added per round, 60s interval) |
| `LpdService` | BEP 14 LAN multicast peer discovery (239.192.152.143:6771) |
| `TrackerState` | BEP 3 HTTP + BEP 15 UDP tracker announce/scrape with compact peer encoding |
| `SourceRegistry` | Pluggable multi-method discovery with trust scoring and freshness tracking |
| `RelayNode` | CnCNet-inspired NAT traversal — relay circuits, UDP hole punching, NAT type detection |
| `NetworkId` | 32-byte network isolation tag (SSB Secret Handshake pattern) — `PRODUCTION`, `TEST`, or custom |

### Wire Protocol

Full BEP 3 message codec with extensions:

| Type | Role |
| ---- | ---- |
| `PeerMessage` | Complete BEP 3: KeepAlive, Choke, Unchoke, Interested, NotInterested, Have, Bitfield, Request, Piece, Cancel, Port, Extended |
| `FastMessage` | BEP 6 Fast Extension: SuggestPiece, HaveAll, HaveNone, RejectRequest, AllowedFast |
| `MetadataExchange` | BEP 9 magnet URI metadata download (16 KiB chunks, SHA-1 verified) |
| `HandshakeMessage` / `Capabilities` | Wire handshake with IRC ISUPPORT-style capability bitmap |
| `BencodeValue` | Zero-copy bencode encoder/decoder with depth limit (64) for DoS protection |
| `ConnectionState` | Connection lifecycle FSM: Connecting → Handshaking → BitfieldExchange → Ready → Closed |

### Streaming & HTTP Gateway

Play media content during download. The `StreamingReader` provides standard
`Read + Seek` — wrap it in any media player that reads from a file handle.
The gateway adapter bridges P2P-backed content to HTTP clients transparently.

| Type | Role |
| ---- | ---- |
| `StreamingReader` | `Read + Seek` over partial downloads — blocks transparently until needed bytes arrive via condvar |
| `ByteRangeMap` | Tracks which byte ranges are available, enabling fine-grained readiness checks |
| `BufferPolicy` | Controls prefetch distance for smooth playback (configurable prebuffer thresholds) |
| `BandwidthEstimator` | Dual EWMA (fast α=0.5, slow α=0.1) for DASH-style adaptive prebuffering decisions |
| `PieceMapping` | Maps byte offsets ↔ piece indices for streaming priority assignment |
| `GatewayAdapter` | Bridges HTTP Range requests to `StreamingReader` — serve P2P content over plain HTTP |
| `RangeRequest` | RFC 7233 parser for `Range: bytes=start-end` headers |
| `PiecePriority` | DASH/HLS-style piece priority: Critical (1000), High (500), Normal (200), Low (100) |

### P2P ↔ HTTP Bridge

A bridge node participates in the P2P swarm as a normal peer but sources
data from HTTP mirrors. The swarm sees a high-availability super-seed; the
HTTP infrastructure is an invisible backend.

| Type | Role |
| ---- | ---- |
| `BridgeNode` | Orchestrates mirror pool, demand tracking, piece cache, and prefetch planning |
| `BridgePeer` | Implements `Peer` backed by the healthiest mirror — with proactive piece prefetch |
| `DemandTracker` | Exponential-decay heat map — prefetches pieces before peers request them |
| `MirrorRegistry` | Mirror health tracking via phi detector: Healthy → Suspect → Degraded → Dead |
| `PieceCache` | Bounded LRU piece cache for bridge nodes |
| `PieceDataCache` | ARC (Adaptive Replacement Cache) for seeding hot pieces — scan-resistant, default 32 MiB |

### Group Replication

Closed share groups for managed content distribution. A group master
publishes a signed content catalog; mirror nodes join the group and
replicate automatically. Only the master (or admins) can mutate the catalog.

| Type | Role |
| ---- | ---- |
| `GroupRoster` | Role-based access: Master, Admin, Mirror, Reader (max 500 members) |
| `GroupManifest` | Versioned, signed content catalog — SHA-256 hashes, sorted entries, diffing |
| `ManifestBuilder` | Fluent API for constructing manifests with guaranteed sort order |
| `CatalogSigner` / `CatalogVerifier` | Pluggable manifest signing — HMAC-SHA-256 built in |
| `SyncPlan` | Manifest diff → Download/Delete action plan for mirrors |
| `ReplicationPolicy` | Full mirror or prefix-filtered partial replication |

### Rate Limiting & Bandwidth

| Type | Role |
| ---- | ---- |
| `BandwidthThrottle` / `ThrottlePair` | Global byte-volume token bucket (aria2 pattern) — separate download + upload limits |
| `RateLimiterMap` | Per-entity request-count rate limiting (IRC flood control, RFC 1459) |
| `AdaptiveConcurrency` | AIMD: ramp up on +10% throughput gain, back off -20% on degradation |
| `WorkStealingScheduler` | Lock-free atomic byte-range bisection for parallel segment downloads (FlashGet pattern) |

### Storage & Resume

| Type | Role |
| ---- | ---- |
| `PieceStorage` trait | Pluggable piece storage — implement for your backing store |
| `FileStorage` | Write verified pieces to disk at correct offsets |
| `MemoryStorage` | In-memory storage for testing and small payloads |
| `CoalescingStorage` | Write-coalescing wrapper for sequential I/O efficiency |
| `StorageFactory` trait | Create storage backends per-download |
| `ResumeState` | Crash recovery — compact binary bitfield format (magic `P2PR`, 17-byte header) |

### Torrent Creation

| Type | Role |
| ---- | ---- |
| `create_torrent()` | Deterministic `.torrent` generation — same file + same piece length = identical `info_hash` |
| `recommended_piece_length()` | Auto-select optimal piece size based on file size (64 KiB–4 MiB range) |
| `TorrentInfo` | Torrent metadata: piece hashes, file size, piece count, byte offsets |
| `TorrentMetadata` | Created torrent: bencoded data, info_hash, piece count, file size |

### Security & Obfuscation

| Type | Role |
| ---- | ---- |
| `ObfuscationKey` | eMule-style XOR stream obfuscation with random port selection (anti-DPI) |
| `PhiDetector` | Phi accrual failure detector (Cassandra/Akka) — probabilistic peer liveness |
| `NetworkId` | Network isolation prevents cross-environment pollution (prod peers never see test traffic) |

### Configuration

`Config` provides unified configuration with per-subsystem nesting. The
`ConfigBuilder` offers a fluent API with sensible defaults.

**20 feature toggles:**

```
dht · pex · local_discovery · obfuscation · merkle_verification
web_seeds · streaming · super_seeding · fast_extension
metadata_exchange · tracker_announce · upload · endgame
rate_limiting · relay · content_discovery · groups · affinity
gateway · work_stealing
```

**Sub-configs:** `ObfuscationConfig`, `ConnectionConfig`, `TrackerConfig`,
`RelayConfig`, `EndgameConfig`, `ChokingConfig`, `UploadConfig`,
`FastExtensionConfig`, `PexConfig`, `WebSeedConfig`, `LocalDiscoveryConfig`,
`MetadataExchangeConfig`, `SuperSeedingConfig`, `ContentDiscoveryConfig`,
`SessionConfig`, `PeerPoolConfig`, `AffinityConfig`, `ValidatorConfig`,
`BridgeConfig`, `BufferPolicy`

## BitTorrent BEP support

| BEP | Name | Module |
| --- | ---- | ------ |
| [BEP 3](https://www.bittorrent.org/beps/bep_0003.html) | The BitTorrent Protocol | `message`, `coordinator`, `choking` |
| [BEP 5](https://www.bittorrent.org/beps/bep_0005.html) | DHT Protocol | `dht` |
| [BEP 6](https://www.bittorrent.org/beps/bep_0006.html) | Fast Extension | `fast_extension` |
| [BEP 9](https://www.bittorrent.org/beps/bep_0009.html) | Extension for Peers to Send Metadata Files | `metadata_exchange` |
| [BEP 11](https://www.bittorrent.org/beps/bep_0011.html) | Peer Exchange (PEX) | `pex` |
| [BEP 14](https://www.bittorrent.org/beps/bep_0014.html) | Local Service Discovery | `local_discovery` |
| [BEP 15](https://www.bittorrent.org/beps/bep_0015.html) | UDP Tracker Protocol | `tracker` |
| [BEP 16](https://www.bittorrent.org/beps/bep_0016.html) | Superseeding | `superseeding` |
| [BEP 19](https://www.bittorrent.org/beps/bep_0019.html) | WebSeed — HTTP/FTP Seeding | `webseed` |

## Design inspiration

The architecture draws from battle-tested distributed systems and classic
P2P protocols:

| Pattern | Origin | Module |
| ------- | ------ | ------ |
| Tit-for-tat + optimistic unchoke | BitTorrent BEP 3 | `choking` |
| AICH corruption localisation | aMule / eMule | `merkle`, `corruption_ledger` |
| Bilateral credit system | eMule | `credit` |
| Phi accrual failure detector | Cassandra / Akka | `phi_detector` |
| Adaptive Replacement Cache (ARC) | Megiddo & Modha 2003 | `piece_data_cache` |
| Adaptive concurrency (AIMD) | FlashGet / TCP congestion | `adaptive` |
| Segmented parallel download | FlashGet / IDM | `work_stealing` |
| NAT traversal relay circuits | CnCNet tunnels | `relay` |
| Key-as-identity | Hamachi VPN / SSB | `peer_id` |
| Network isolation tags | SSB Secret Handshake | `network_id` |
| XDCC slot queuing | IRC XDCC bots | `upload_queue` |
| Connection budgets | libp2p swarm limits | `budget` |
| DASH/HLS adaptive bitrate | Media streaming | `priority`, `bandwidth` |
| IRC ISUPPORT capability bits | IRC v3 | `handshake` |
| IRC flood control | RFC 1459 | `rate_limiter` |
| IRC K/G/Z-line ban scopes | IRC networks (UnrealIRCd) | `peer_stats` |

## Design principles

- **Zero-copy piece verification.** Every piece is SHA-1 verified against
  the torrent metadata hash. No piece reaches storage without passing
  verification, regardless of source.

- **Streaming-first.** `StreamingReader` provides `Read + Seek` over
  partially-downloaded content. Downstream can play video, decompress
  archives, or serve HTTP responses while pieces are still arriving.

- **Pluggable everything.** Peer transports, piece storage, catalog signing,
  choking strategies, and discovery backends are all trait-based. Use the
  built-in defaults or bring your own implementations.

- **No `unsafe` code.** The entire crate — all 60+ modules, 1,094 tests —
  is written in safe Rust. No `unsafe` blocks, no `unsafe fn`, no
  `unsafe impl`.

- **Minimal dependencies.** Only 4 required crates (`sha1`, `sha2`,
  `getrandom`, `thiserror`). The HTTP transport adds `ureq` behind the
  `http` feature flag.

- **Deterministic torrent creation.** `create_torrent()` is fully
  deterministic: same file content + same piece length = identical
  `info_hash` on every run. No timestamps, no random data.

## Module index

Every module is a flat `.rs` file in `src/`. The crate contains 62 modules
with 1,094 tests.

| Module | Purpose |
| ------ | ------- |
| `adaptive` | AIMD adaptive concurrency controller |
| `bandwidth` | Dual-EWMA bandwidth estimator (fast + slow) |
| `bencode` | BEP 3 bencode encoder/decoder (depth-limited) |
| `bitfield` | BEP 3 peer bitfield + rarity scoring |
| `bridge` | P2P ↔ HTTP bridge node with mirror pool and demand tracking |
| `bridge_peer` | `Peer` impl backed by closure for custom transports |
| `budget` | libp2p-style connection budget enforcement |
| `cache` | LRU piece metadata cache |
| `catalog` | Group catalog sync planning (diff → download/delete) |
| `catalog_sign` | HMAC-SHA-256 catalog signing and verification |
| `choking` | BEP 3 tit-for-tat + AlwaysUnchoke strategies |
| `config` | Unified `Config` with 20 sub-configs + `ConfigBuilder` |
| `connection` | Connection lifecycle FSM with close reasons |
| `content_discovery` | Multi-method content source discovery |
| `coordinator` | `PieceCoordinator` — the core download orchestrator |
| `corruption_ledger` | Per-piece corruption blame tracking |
| `credit` | eMule-style bilateral credit ledger |
| `demand` | Exponential-decay demand heat map |
| `dht` | Kademlia DHT (K=20, α=3, 256-bit) |
| `endgame` | BEP 3 endgame mode (duplicate block requests) |
| `fast_extension` | BEP 6 fast extension messages |
| `gateway` | HTTP Range-request types for P2P gateway |
| `gateway_adapter` | P2P storage → HTTP range response adapter |
| `group` | Group replication roles and roster (RBAC) |
| `handshake` | Wire handshake with capability bitmap |
| `local_discovery` | BEP 14 LAN multicast peer discovery |
| `manifest` | Versioned group manifest with content diffing |
| `merkle` | SHA-256 Merkle tree for sub-piece verification |
| `message` | BEP 3 wire message encode/decode |
| `metadata_exchange` | BEP 9 magnet URI metadata exchange |
| `mirror_health` | Phi-detector-based mirror health registry |
| `network_id` | 32-byte network isolation tag |
| `obfuscation` | XOR stream obfuscation + ephemeral ports |
| `peer` | `Peer` trait + error types + capabilities |
| `peer_affinity` | Geographic/topological peer scoring |
| `peer_id` | 32-byte `PeerId` with military callsign generation |
| `peer_pool` | Bounded peer pool: eviction, backoff, scoring |
| `peer_stats` | Per-peer stats, reputation, trust levels, exclusion scopes |
| `pex` | BEP 11 Peer Exchange gossip protocol |
| `phi_detector` | Φ accrual failure detector |
| `piece_data_cache` | ARC piece data cache (scan-resistant, 32 MiB default) |
| `piece_map` | Atomic per-piece state tracking (`SharedPieceMap`) |
| `piece_validator` | Merkle sub-piece verification + quarantine + retry |
| `priority` | Piece priority levels (Low / Normal / High / Critical) |
| `rate_limiter` | Token-bucket per-entity rate limiting |
| `reader` | `StreamingReader` — `Read + Seek` over partial downloads |
| `relay` | NAT traversal relay circuits + hole punching |
| `resume` | Binary resume state: save/load verified bitfield |
| `selection` | Rarest-first piece selector with speed categories |
| `session_manager` | Multi-download session with FIFO queuing |
| `state` | Download lifecycle state machine |
| `storage` | `PieceStorage` trait + File / Coalescing / Memory impls |
| `streaming` | Byte-range map, buffer policy, piece mapping |
| `superseeding` | BEP 16 super-seeding (piece diversity maximisation) |
| `throttle` | Global bandwidth throttle (byte-granularity token bucket) |
| `torrent_create` | Deterministic `.torrent` file creation |
| `torrent_info` | `TorrentInfo` metadata struct |
| `tracker` | BEP 3/15 HTTP + UDP tracker announce/scrape |
| `upload_queue` | XDCC-style bounded upload slot queue |
| `webseed` | BEP 19 HTTP web seed peer (feature-gated: `http`) |
| `work_stealing` | Lock-free byte-range work-stealing scheduler |

## Dependencies

Minimal by design:

| Crate | Purpose |
| ----- | ------- |
| `sha1` | Piece hash verification (BitTorrent standard) |
| `sha2` | Merkle tree leaves, manifest signing (SHA-256) |
| `getrandom` | Peer ID generation, random port selection |
| `thiserror` | Structured error types with `Display` |
| `ureq` *(optional, `http` feature)* | HTTP Range requests for BEP 19 web seeds |

## Testing

1,094 tests across 62 modules:

```sh
cargo test                       # run all tests (default features)
cargo test --no-default-features # core-only (no HTTP)
```

Tests cover:
- **Happy paths** — normal download, streaming, resume, torrent creation
- **Error paths** — every error variant, structured field validation
- **Adversarial inputs** — corrupt pieces, malicious peers, oversized
  messages, invalid bencode nesting, path traversal
- **Determinism** — same input → same output (torrent hashes, peer IDs)
- **Boundary conditions** — empty files, single-piece files, max piece
  counts, zero-byte ranges

## License

Licensed under either of:

- Apache License, Version 2.0
  ([LICENSE-APACHE](LICENSE-APACHE) or <https://www.apache.org/licenses/LICENSE-2.0>)
- MIT License
  ([LICENSE-MIT](LICENSE-MIT) or <https://opensource.org/licenses/MIT>)

at your option.

## Contributing

Unless you explicitly state otherwise, any contribution intentionally
submitted for inclusion in this crate by you, as defined in the Apache-2.0
license, shall be dual licensed as above, without any additional terms or
conditions.
