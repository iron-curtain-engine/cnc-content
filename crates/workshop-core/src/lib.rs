// SPDX-License-Identifier: MIT OR Apache-2.0

//! Engine-agnostic Workshop package registry and content distribution.
//!
//! `workshop-core` is the middle layer in a three-tier architecture:
//!
//! ```text
//! ┌─────────────────────────────────────────────────────┐
//! │  Game Integration Layer (per-project, engine-specific)
//! │    IC: .icpkg, Bevy plugin, lobby auto-download
//! │    Other projects: their own formats and triggers
//! ├─────────────────────────────────────────────────────┤
//! │  workshop-core (this crate — game-agnostic)
//! │    Registry, manifests, identity, integrity,
//! │    dependency resolution, index backends, CAS store
//! ├─────────────────────────────────────────────────────┤
//! │  p2p-distribute (domain-agnostic)
//! │    BitTorrent wire protocol, web seeds, streaming,
//! │    peer scoring, content channels, embedded tracker
//! └─────────────────────────────────────────────────────┘
//! ```
//!
//! The crate applies three architectural lessons from git:
//!
//! 1. **Content addressing is the foundation.** Every package file is
//!    identified by its SHA-256 hash. Two versions sharing the same file
//!    share the same blob — deduplication is automatic and free.
//!
//! 2. **Mutable pointers over immutable content.** Packages are immutable
//!    once published (the hash IS the identity). Version discovery comes
//!    from the index layer — a lightweight manifest index (hostable on
//!    any git platform or HTTP server) that maps `publisher/name@version`
//!    to content hashes and download metadata.
//!
//! 3. **Efficient incremental sync.** `git fetch --depth=1` on the index
//!    repo transfers only changed manifests (~KB). The content layer
//!    (multi-GB mods) flows through `p2p-distribute` with web seeds
//!    bootstrapping the P2P swarm.
//!
//! 4. **Platform independence by design.** No component assumes a specific
//!    hosting platform. Content is identified by SHA-256 hash — permanent
//!    and universal. URLs are ephemeral delivery hints, replaceable
//!    without changing content identity. The index layer supports multiple
//!    backends with automatic failover ([`FailoverIndex`]): if the
//!    primary mirror goes down, clients seamlessly switch to alternatives.
//!    Every piece of infrastructure (git index, file hosting, BitTorrent
//!    trackers) is self-hostable and replaceable. The entire project can
//!    migrate between platforms in hours, not months.
//!
//! # Design authority
//!
//! This crate implements requirements from the Iron Curtain design docs:
//! - D030 — Workshop registry model, phased delivery, resource identity
//! - D049 — Content distribution, P2P transport strategy, web seeding
//! - D050 — Three-layer architecture, core library boundary
//! - D076 — Standalone crate extraction (MIT/Apache-2.0 license)

pub mod blob;
pub mod error;
pub mod index;
pub mod manifest;
pub mod resource;
pub mod update;

pub use blob::{BlobId, BlobStore};
pub use error::WorkshopError;
pub use index::{FailoverIndex, IndexBackend, UpdateResult};
pub use manifest::PackageManifest;
pub use resource::{Channel, Dependency, ResourceCategory, ResourceId, ResourceVersion};
pub use update::{FailoverDiscovery, PublisherId, UpdateDiscovery, VersionAnnouncement};
