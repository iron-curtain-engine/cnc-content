# p2p-distribute — Implementation Gap Analysis & Plan

> **Generated:** 2026-04-04
> **Design authority:** `C:\git\games\iron-curtain-design-docs`
> **Key design docs:**
> - `research/p2p-distribute-crate-design.md` (2819 lines — full crate design)
> - `research/p2p-engine-protocol-design.md` (1569 lines — wire protocol)
> - `research/p2p-distribute-implementation-plan.md` (270 lines — milestone sequence)
> - `src/decisions/09e/D049/D049-p2p-distribution.md` — P2P distribution design
> - `src/decisions/09e/D049/D049-web-seeding.md` — web seeding design (3-gate scheduler)
> - `src/decisions/09e/D049/D049-content-channels-integration.md` — content channels
> - `src/decisions/09e/D049/D049-package-profiles.md` — .icpkg format
> - `src/decisions/09e/D049/D049-replay-sharing.md` — replay P2P distribution
> - `src/decisions/09a/D076-standalone-crates.md` — crate extraction (Tier 3)

---

## Current State (304 lib tests + 13 doc-tests, all passing)

The crate has **19 modules** (~4,000 lines of logic). It implements a working MVP
download coordinator with HTTP web seeds, streaming reader, and rich supporting
infrastructure:

| Module | What it provides |
|--------|-----------------|
| `coordinator.rs` | `PieceCoordinator` — piece-level download orchestration, SHA-1 verification, peer blacklisting, endgame mode, retry rotation |
| `webseed.rs` | `WebSeedPeer` — HTTP BEP 19 Range requests, EMA speed tracking |
| `peer.rs` | `Peer` trait, `PeerError`, `PeerKind`, `RejectionReason`, `PeerCapabilities` |
| `piece_map.rs` | `SharedPieceMap` — lock-free atomic per-piece state (Needed→InFlight→Done/Failed) |
| `selection.rs` | Rarest-first piece selection with streaming priority weighting |
| `priority.rs` | `PiecePriority` — 3-tier priority (Critical/High/Normal) with weighted scoring |
| `streaming.rs` | `ByteRangeMap`, `BufferPolicy`, `PieceMapping`, `PeerPriority`, `StreamProgress` |
| `reader.rs` | `StreamingReader` — `Read + Seek` over partially-downloaded files |
| `bandwidth.rs` | Dual-EWMA bandwidth estimator (fast α=0.5, slow α=0.1) |
| `peer_stats.rs` | Comprehensive per-peer session tracking: EWMA speed, trust levels, composite scoring, anti-snubbing, reputation snapshots |
| `phi_detector.rs` | Phi accrual failure detector (Cassandra/Akka pattern) |
| `budget.rs` | `ConnectionBudget` — resource limits (max established, pending, per-peer) |
| `bitfield.rs` | `PeerBitfield` — BEP 3 wire format, per-peer piece availability |
| `pex.rs` | Peer Exchange message types (BEP 11 type defs — no wire protocol yet) |
| `peer_id.rs` | `PeerId` — 32-byte identity, SHA-256/Ed25519, callsign generation |
| `network_id.rs` | `NetworkId` — 32-byte swarm isolation tag |
| `torrent_info.rs` | `TorrentInfo` — piece hashes, piece length, file metadata |
| `torrent_create.rs` | Deterministic `.torrent` file creation, bencode encoding, info hash, BEP 19 web seed URLs |
| `lib.rs` | Module declarations, re-exports, crate docs with roadmap |

**Key design decisions already made:**
- Synchronous I/O (design says tokio — see architecture gap below)
- `Peer` trait unifies all piece sources (HTTP, BT, custom)
- Lock-free coordinator (atomic CAS for piece claims)
- No `unsafe`, no `.unwrap()` in library code
- MIT OR Apache-2.0

---

## Architecture-Level Gaps

These span multiple milestones and represent fundamental missing layers.

### A1. Async Runtime (tokio)

**Design requirement:** The implementation plan specifies `tokio` as the async
runtime. The crate design's `Session` API is fully async (`async fn`, returns
`impl Future`, uses `impl Stream`).

**Current state:** Entirely synchronous. `PieceCoordinator::run()` is blocking.
`WebSeedPeer::fetch_piece()` uses `ureq` (blocking HTTP).

**Impact:** Switching to async is a cross-cutting change that touches every I/O
path. This should be done early (M1–M2 timeframe) or all subsequent milestones
will need rework.

**Options:**
1. Migrate to tokio + reqwest now (breaking change)
2. Keep sync core, add async wrappers via `spawn_blocking()` later
3. Keep sync for the coordinator's hot loop, use async only for network I/O

### A2. Session API

**Design requirement:** Top-level `Session` → `TorrentHandle` hierarchy with
event streaming, config mutation, resume data, graceful shutdown.

**Current state:** Only `PieceCoordinator` exists (single-torrent, single-run).

**Depends on:** M1 (wire protocol), M2 (storage backend), A1 (async)

### A3. Extensibility Traits

**Design specifies 8 pluggable traits:**

| Trait | Purpose | Status |
|-------|---------|--------|
| `StorageBackend` | Pluggable content storage (fs, memory, IndexedDB) | ❌ Missing |
| `StorageBackendFactory` | Per-torrent storage creation | ❌ Missing |
| `AuthPolicy` | Peer authentication (Ed25519, tokens) | ❌ Missing |
| `PeerFilter` | IP blocklists, ASN rules | ❌ Missing |
| `RatePolicy` | Token bucket, time-of-day schedules | ❌ Missing |
| `RevocationPolicy` | Content takedown, block lists | ❌ Missing |
| `DiscoveryBackend` | Custom peer discovery sources | ❌ Missing |
| `MetricsSink` | Prometheus, OTEL, StatsD | ❌ Missing |
| `LogSink` | Structured logging integration | ❌ Missing |

Only `Peer` (piece source abstraction) exists today.

### A4. Storage Middleware (Tower-Style Layers)

**Design requirement:** Composable `StorageLayer<S>` trait for
`IntegrityCheckLayer`, `ReadCacheLayer`, `MetricsLayer`, `WriteBatchLayer`.

**Current state:** No storage abstraction at all.

### A5. Upload / Seeding

**Current state:** The crate is download-only. No upload path exists.

**Design requirement:** Full seeding support — serve pieces to requesting peers,
choking algorithm, upload slot allocation, tit-for-tat reciprocity.

### A6. .torrent File Parsing

**Current state:** `torrent_create.rs` can *create* torrents. `TorrentInfo` must
be manually constructed — cannot parse `.torrent` files.

**Design requirement:** Full metainfo parsing, InfoHash computation from
bencode, magnet URI resolution (BEP 9).

---

## Milestone Gap Details

### M1 — Bencode & Wire Protocol ❌ NOT STARTED

**Goal:** Parse and generate all BitTorrent protocol messages.

| Deliverable | Status | Notes |
|-------------|--------|-------|
| `bencode.rs` — full encoder/decoder (BEP 3) | ~15% | `torrent_create.rs` has a minimal hand-rolled encoder (strings, ints, lists, dicts). No decoder/parser exists. |
| `metainfo.rs` — `.torrent` file parsing | ❌ | `TorrentInfo` is manually constructed. No `.torrent` parser. No `InfoHash` computation from bencode. |
| `wire.rs` — peer wire protocol messages | ❌ | No handshake, bitfield, have, request, piece, choke/unchoke, interested/not-interested, cancel message types. |
| BEP 10 extension protocol framework | ❌ | No extension message ID negotiation. |

**This is the natural next milestone.** Everything else builds on wire protocol.

### M2 — Storage Backend ❌ NOT STARTED

**Goal:** Pluggable content storage with filesystem default.

| Deliverable | Status | Notes |
|-------------|--------|-------|
| `StorageBackend` trait | ❌ | Coordinator writes directly via `std::fs`. |
| `FsStorage` (filesystem backend) | ❌ | |
| `MemoryStorage` (testing backend) | ❌ | |
| Piece hash verification | ~50% | SHA-1 verification exists in coordinator. SHA-256 for v2 missing. |

### M3 — Peer Connections & Choking ❌ NOT STARTED

**Goal:** TCP connections with peers, BT choking algorithm.

| Deliverable | Status | Notes |
|-------------|--------|-------|
| TCP listener + connector | ❌ | |
| Peer state machine (connecting → handshaking → active → closing) | ❌ | |
| Traditional choking algorithm (4 upload slots, optimistic unchoke 30s) | ❌ | |
| Connection limits (50/torrent, 200 global) | ~30% | `ConnectionBudget` type exists with counting logic, not wired to real connections. |
| BEP 10 extension handshake | ❌ | |

### M4 — Piece Scheduler ✅ MOSTLY DONE (~85%)

**Goal:** Rarest-first piece selection with priority hints.

| Deliverable | Status | Notes |
|-------------|--------|-------|
| Rarity tracking across peer bitfields | ✅ | `selection.rs` |
| Rarest-first selection | ✅ | Tie-breaks by piece index (design says random — minor divergence) |
| Priority override | ✅ | `priority.rs` — 3-tier `PiecePriority` enum |
| Endgame mode | ✅ | `coordinator.rs` — configurable threshold |

**Remaining:** Random tie-breaking (currently deterministic by piece index).

### M5 — HTTP Tracker & Single Complete Transfer ❌ NOT STARTED

**Goal:** Announce to HTTP tracker, complete a full download.

| Deliverable | Status | Notes |
|-------------|--------|-------|
| HTTP tracker client (`/announce`, `/scrape`) | ❌ | |
| Compact peer format (BEP 23) | ❌ | |
| Integration: tracker → discovery → download → verify → complete | ❌ | Current flow requires manual `add_peer()` |
| CLI smoke test | ❌ | |

### M6 — HTTP Web Seeding ✅ SIMPLIFIED (~40%)

**Goal:** BEP 17/19 web seed support.

| Deliverable | Status | Notes |
|-------------|--------|-------|
| BEP 19 (Hoffman-style) Range requests | ✅ | `webseed.rs` — working with EMA speed tracking |
| BEP 17 (GetRight-style) whole-torrent ranges | ❌ | Design says "use both" |
| Hybrid BT + HTTP swarm | ~30% | Architecture exists (`Peer` trait) but no BT peer impl |
| Three-gate scheduler | ❌ | No eligibility filter, bandwidth fraction cap, or `prefer_bt_peers` |
| `supports_range` detection (200→mark unusable) | ~50% | Rejects non-206, but doesn't mark seed as unusable and fall back |
| Retry/backoff on HTTP errors | ❌ | No `consecutive_failures` tracking or backoff |
| Multi-file BEP 19 (piece spanning) | ❌ | Only single-file torrents |

**Three-gate scheduler from D049-web-seeding.md:**
```
Gate 1: Eligibility — supports_range, active_requests < max, failures < threshold, global < max
Gate 2: Bandwidth fraction cap — http_fraction >= max → BT only this round
Gate 3: prefer_bt_peers — healthy swarm (≥2 BT peers above rate threshold) → BT first
```

**Configuration from design (not yet implemented):**
| Key | Default | Description |
|-----|---------|-------------|
| `max_requests_per_seed` | 4 | Max concurrent HTTP requests per seed |
| `max_requests_global` | 16 | Max concurrent HTTP requests total |
| `connect_timeout` | 10s | |
| `request_timeout` | 60s | |
| `failure_backoff_threshold` | 5 | Consecutive failures → disable seed |
| `failure_backoff_duration` | 300s | Disable duration |
| `max_bandwidth_fraction` | 0.8 | Max HTTP share of total bandwidth |
| `prefer_bt_peers` | true | Deprioritize HTTP when swarm healthy |
| `bt_peer_rate_threshold` | 51200 | Min BT peer rate for "healthy" |

### M7 — Peer Scoring & Bandwidth ✅ PARTIAL (~50%)

**Goal:** Adaptive peer quality scoring and bandwidth management.

| Deliverable | Status | Notes |
|-------------|--------|-------|
| Per-peer scoring | ✅ | `peer_stats.rs` — composite formula: Speed(0.4) + Reliability(0.3) + Availability(0.2) + Recency(0.1) |
| Bandwidth limiter (global caps) | ❌ | `BandwidthEstimator` measures but doesn't enforce limits |
| Peer rotation (drop low-scoring, reconnect better) | ❌ | No connection lifecycle management |
| Upload slot allocation (tit-for-tat) | ❌ | No upload path |

**Design also specifies a different peer scoring formula (tracker-side, Phase 5+):**
```
PeerScore = Capacity(0.4) + Locality(0.3) + SeedStatus(0.2) + LobbyContext(0.1)
```
The current formula is client-side session scoring — complementary, not conflicting.

### M8 — DHT, UDP Tracker, PEX, LSD ❌ NOT STARTED

| Deliverable | Status | Notes |
|-------------|--------|-------|
| UDP tracker protocol (BEP 15) | ❌ | |
| Mainline DHT (BEP 5) | ❌ | Routing table, `get_peers`, `announce_peer` |
| Peer Exchange (BEP 11) wire messages | ~10% | `pex.rs` has **type definitions only** — `PexMessage`, `PexFlags`. No wire serialization. |
| Local Service Discovery (BEP 14) | ❌ | Multicast LAN announce |
| DHT bootstrap from well-known nodes | ❌ | |

### M9 — NAT Traversal & uTP ❌ NOT STARTED

| Deliverable | Status | Notes |
|-------------|--------|-------|
| uTP transport (BEP 29, LEDBAT congestion) | ❌ | |
| NAT-PMP / PCP port mapping | ❌ | |
| UPnP IGD port mapping | ❌ | |
| Hole punching (BEP 55) | ❌ | |

### M10 — IC Extensions & Embedded Tracker ❌ NOT STARTED

| Deliverable | Status | Notes |
|-------------|--------|-------|
| `ic_auth` extension (Ed25519 peer auth) | ❌ | `PeerId` supports Ed25519 raw bytes but no auth handshake protocol |
| `ic_priority` extension messages | ❌ | Priority types exist, no wire encoding |
| `AuthPolicy` trait | ❌ | |
| `RevocationPolicy` trait | ❌ | |
| Embedded HTTP tracker | ❌ | In-process announce/scrape, peer bucketing |

### M11 — Content Channels ❌ NOT STARTED

| Deliverable | Status | Notes |
|-------------|--------|-------|
| `SnapshotInfo` (sequence, hash chain, signature) | ❌ | |
| Channel subscription (background-priority download) | ❌ | |
| Manual activation (no auto-apply safety) | ❌ | |
| Retention enforcement (GC old snapshots) | ❌ | |
| Lobby fingerprint integration | ❌ | |

**D049-content-channels-integration.md specifies:**
- Balance patches, server config, live feeds, mod update notifications
- Ed25519 signed snapshot sequence
- Integration with D062 mod profiles (namespace resolution overlay)
- `auto_subscribe` for community servers

### M12 — WebRTC Transport & Browser Support ❌ NOT STARTED

| Deliverable | Status | Notes |
|-------------|--------|-------|
| WebRTC data channel transport (`str0m`) | ❌ | |
| WebSocket signaling to workshop server | ❌ | |
| ICE/STUN/TURN support | ❌ | |
| Hybrid BT + WebRTC swarm | ❌ | |
| WASM build target (`wasm32-unknown-unknown`) | ❌ | |
| `BrowserWasm` config profile | ❌ | |

### M13 — Config System & Profiles ❌ NOT STARTED

| Deliverable | Status | Notes |
|-------------|--------|-------|
| 10-group config system | ~10% | `CoordinatorConfig` is a flat struct with 6 fields |
| 4 built-in profiles (`embedded_minimal`, `desktop_balanced`, `server_seedbox`, `lan_party`) | ❌ | |
| Config builder with serde (TOML/JSON) | ❌ | |
| Runtime config override API | ❌ | |

**Design specifies 10 config groups:** session, torrent_defaults, storage,
network, tracker, peer_selection, bandwidth, cache, security, platform.

### M14 — Hardening & Publication ❌ NOT STARTED

| Deliverable | Status | Notes |
|-------------|--------|-------|
| Fuzz testing (bencode, wire protocol, metainfo) | ❌ | |
| Property-based testing (piece selection fairness) | ❌ | |
| API documentation (every public type) | ~40% | Module-level docs exist, many types documented |
| `crates.io` publication | ❌ | |
| CI: Linux/macOS/Windows/WASM | ❌ | Only Windows tested |

---

## Feature Flags (Design vs. Current)

| Flag (design) | Current | Notes |
|---------------|---------|-------|
| `default = []` | `default = ["http"]` | Design says empty default; we default to `http` |
| `http-seeds` (reqwest) | `http` (ureq) | Different name, different HTTP crate |
| `dht` | ❌ | |
| `pex` | ❌ | |
| `lsd` | ❌ | |
| `udp-tracker` | ❌ | |
| `utp` | ❌ | |
| `channels` | ❌ | |
| `embedded-tracker` | ❌ | |
| `webrtc` (str0m) | ❌ | |
| `v2` (BEP 52 merkle) | ❌ | |

---

## D049 Protocol Parameters (Reference)

Key numeric parameters from the design docs that implementations must match:

| Parameter | Value | Source |
|-----------|-------|--------|
| Announce interval | 30s (10s during download, 60s seeding, 120s cap) | D049 |
| Piece length <5MB | N/A (HTTP only) | D049 |
| Piece length 5–50MB | 256 KB | D049 |
| Piece length 50–500MB | 1 MB | D049 |
| Piece length >500MB | 4 MB | D049 |
| Max connections per package | 8 | D049 |
| Pipeline limit per peer | 3 concurrent requests | D049 |
| Piece request timeout | 8s base + 6s/MB | D049 |
| Endgame threshold | 5 remaining pieces | D049 |
| Blacklist trigger | 0 throughput for 30s | D049 |
| Blacklist cooldown | 5 min | D049 |
| Sybil limit | 3 peers per /24 subnet | D049 |
| Connection TTL (idle) | 60s | D049 |
| Peer handout limit | 30 peers per announce | D049 |
| Choking interval | 10s regular | D049 |
| Optimistic unchoke interval | 30s | D049 |
| Default upload cap | 1 MB/s | D049 |
| Seed duration after exit | 30 min | D049 |
| Cache size limit | 2 GB (LRU eviction) | D049 |

---

## Suggested Implementation Order

Based on dependency graph and incremental value:

1. **M1 — Bencode & Wire Protocol** — foundation for everything
2. **M2 — Storage Backend** — `StorageBackend` trait, `FsStorage`, `MemoryStorage`
3. **A1 — Async decision** — resolve sync vs async before M3
4. **M3 — Peer Connections & Choking** — TCP, peer state machine, choking
5. **M5 — HTTP Tracker** — tracker announce/scrape, full download flow
6. **M6 completion — Web Seeding** — BEP 17, 3-gate scheduler, multi-file
7. **M7 completion — Bandwidth** — rate limiter, peer rotation
8. **M4 polish** — random tie-breaking
9. **M8 — Discovery** — DHT, UDP tracker, PEX wire, LSD
10. **M9 — NAT/uTP** — connectivity
11. **M10 — IC Extensions** — auth, priority, embedded tracker
12. **M11 — Content Channels** — snapshots, subscriptions
13. **M12 — WebRTC** — browser support
14. **M13 — Config** — profiles, serde config
15. **M14 — Hardening** — fuzz, proptest, publish

---

## Summary Table

| Milestone | Status | Coverage |
|-----------|--------|----------|
| **M1** Bencode & Wire Protocol | ❌ Not started | ~5% |
| **M2** Storage Backend | ❌ Not started | ~10% |
| **M3** Peer Connections & Choking | ❌ Not started | ~15% |
| **M4** Piece Scheduler | ✅ Mostly done | ~85% |
| **M5** HTTP Tracker | ❌ Not started | 0% |
| **M6** Web Seeding | ✅ Simplified | ~40% |
| **M7** Peer Scoring & Bandwidth | ✅ Partial | ~50% |
| **M8** DHT, UDP Tracker, PEX, LSD | ❌ Not started | ~5% |
| **M9** NAT Traversal & uTP | ❌ Not started | 0% |
| **M10** IC Extensions & Tracker | ❌ Not started | ~5% |
| **M11** Content Channels | ❌ Not started | 0% |
| **M12** WebRTC Transport | ❌ Not started | 0% |
| **M13** Config & Profiles | ❌ Not started | ~10% |
| **M14** Hardening & Publication | ❌ Not started | ~20% |
