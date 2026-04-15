# TODO — Cloud Integration & Deferred Features

Items that require cloud infrastructure, protocol wire format changes,
or async runtime integration. Only items that belong in `p2p-distribute`
are listed here — storage abstraction layers belong elsewhere.

## 1. Pre-Signed URL Wire Format (Protocol Extension)

**What:** Define bencode encoding for `UrlPexMessage` so it can be
transmitted over the P2P protocol.

**Why:** `url_pex.rs` defines the Rust types (`UrlPexEntry`,
`UrlPexMessage`, `UrlPexCache`) but no serialization. The swarm needs a
message type ID and encoding to actually gossip URLs.

**Steps:**
1. Assign a protocol message type ID for URL PEX messages.
2. Define bencode encoding: `{network_id, urls: [{piece, url, ttl, sha1}]}`.
3. Add `encode()`/`decode()` functions to `url_pex.rs`.
4. Integrate with `message.rs` (`PeerMessage` enum) and handshake
   capability negotiation.
5. Update D049 design doc with the protocol extension spec.

## 2. Pre-Signed URL Rotation & Expiry Policy

**What:** Automatic URL regeneration for long-running seeds.

**Why:** Pre-signed URLs expire. A seeder that runs for days needs to
periodically regenerate URLs and re-announce via the PEX gossip layer.

**Steps:**
1. Add a `UrlRefresher` that watches `UrlPexCache` for entries approaching
   expiry (< 10 min remaining).
2. Callback to a user-provided URL generator to produce fresh URLs.
3. Re-inject refreshed entries into the outgoing `UrlPexMessage` stream.
4. Rate-limit re-announcements to avoid gossip flooding.

## 3. DemandTracker → TieringStorage Policy Loop

**What:** Connect the existing `DemandTracker` heat map to
`TieringStorage` demotion/promotion decisions.

**Why:** `DemandTracker` already tracks piece demand with exponential
decay. `TieringStorage` has `demote()` and `coldest_pieces()`. The
missing piece is a periodic policy loop that checks heat scores and
demotes cold pieces.

**Steps:**
1. Create a `TieringPolicy` struct: threshold heat score, check interval,
   max demotions per cycle.
2. Periodic `tick()` method: get `coldest_pieces(n)`, check
   `demand.is_hot(piece)`, demote those that aren't.
3. Optionally: log tier transitions for observability.

## 4. Cloudflare R2 Permanent Seeds (cnc-content scope)

**What:** Deploy freeware packages to R2 buckets as BEP 19 web seeds.

**Why:** R2 has zero egress fees. A single R2 bucket can serve as a
permanent seed for all freeware content (RA, TD, TS).

**Note:** This is deployment/infrastructure work in `cnc-content`, not
a `p2p-distribute` code change. R2 URLs go into `data/downloads.toml`
as web seeds and into `.torrent` files.

## 5. Serverless Seedbox (Lambda / Cloudflare Worker)

**What:** A stateless function that serves piece requests from S3/R2.

**Why:** Eliminates the need for always-on seed infrastructure. Combined
with pre-signed URL PEX, this makes "serverless P2P seeding" possible.

**Note:** This is a standalone deployment project. The Worker/Lambda
receives HTTP requests, reads pieces from R2/S3, and returns them. It
registers as a web seed URL in `.torrent` files. No changes to
`p2p-distribute` needed — it's already a valid `WebSeedPeer` URL.
