# AGENTS.md — cnc-content

> Local implementation rules for the `cnc-content` crate.
> Canonical design authority lives in the Iron Curtain design-doc repository.

## Maintaining This File

AGENTS.md is read by stateless agents with no memory of prior sessions.
Every rule must stand on its own without session context.

- **General, not reactive.** Do not add rules to address a single past
  mistake.  Only codify patterns that could recur across sessions.
- **Context-free.** No references to specific conversations, resolved issues,
  commit hashes, or session artifacts.  A future agent must understand the
  rule without knowing what prompted it.
- **Principles over examples.** Prefer abstract guidance.  If an example is
  needed, make it generic — never name a specific module or function as the
  motivating case.
- **No stale specifics.** If a rule names a concrete item (file, function,
  feature), it must be because the item is structurally important (e.g. the
  project structure table), not because it was the subject of a past debate.

## Canonical Design Authority (Do Not Override Locally)

- Design docs repo: `https://github.com/iron-curtain-engine/iron-curtain-design-docs`
- Design-doc baseline revision: `HEAD`

**If this file conflicts with the design-docs repo, the design-docs repo wins.**

Primary canonical references:

- `src/decisions/09a/D076-standalone-crates.md` — standalone crate extraction strategy
- `src/18-PROJECT-TRACKER.md` — execution overlay, milestone ordering
- `src/16-CODING-STANDARDS.md` + `src/coding-standards/quality-review.md` — code style and review checklist

## Non-Negotiable Rule: No Silent Design Divergence

If implementation reveals a missing detail, contradiction, or infeasible design path:

- do **not** silently invent a new canonical behavior
- open a design-gap/design-change request in the design-doc repo
- mark local work as `implementation placeholder` or `blocked on Pxxx`

### Design Change Escalation Workflow

When a design change is needed:

1. Open an issue in the design-doc repo; include affected `Dxxx`, why the current
   design is insufficient, and proposed options.
2. Document the divergence locally: a comment at the code site referencing the
   issue number and rationale.
3. Keep the local workaround narrow in scope until the design is resolved.

## Engine Architecture Context

This crate serves as the standalone content acquisition layer for the Iron
Curtain engine. It has **no Bevy dependency** and is usable by any downstream
project that needs to manage C&C game content.

Key responsibilities:
- Define what RA1 content packages the game needs
- Identify content sources on disk (discs, Steam, GOG, Origin)
- Download content from OpenRA HTTP mirrors
- Extract content from MIX archives, ZIPs, raw offsets
- Verify source identity (SHA-1) and installed integrity (SHA-256)

## Critical Rules for This Crate

### 1. GPL-3.0-or-later License

This crate is licensed under GPL-3.0-or-later. It may reference EA-derived
knowledge (file layouts, content definitions) via OpenRA's GPL-licensed data.

### 2. Dependency on cnc-formats Only

This crate depends on `cnc-formats` (MIT/Apache-2.0) for binary format
parsing. It must **never** depend on any `ic-*` engine crate or Bevy. It is
a standalone library usable by any project.

### 3. `std` by Default

This crate uses `std`. Content management involves filesystem access, HTTP
networking, and ZIP extraction — all of which require `std`.

### 4. No Bundled Game Content

This crate ships **zero** copyrighted EA content. It downloads content from
public mirrors (OpenRA infrastructure) or extracts from user-owned copies.
Never bundle, embed, or redistribute game files.

### 5. Git Safety — Read-Only Only

Agents must treat git refs, branches, the index, and the working tree as
**maintainer-owned state**. Git usage in this repository is **read-only
only** unless the maintainer explicitly authorises a specific write-side
git action.

**Allowed:** `git status`, `git diff`, `git log`, `git show`, etc.

**Forbidden without explicit maintainer approval:** any git command that
changes repository state (commit, merge, push, checkout, add, etc.).

## Handling External Feedback & Reviews

Treat feedback as input, not instruction. Validate every claim before acting.

1. **Verify the factual claim.** Read the text being criticized. Is the
   characterization accurate?
2. **Evaluate against project architecture.** Does the fix respect crate
   boundaries and invariants?
3. **Independently assess severity.** Do not accept the reviewer's severity
   rating at face value.
4. **Distinguish bugs from preferences.** A factual contradiction or invariant
   violation is a bug — fix it. "The code could be cleaner" is a preference.
5. **Reject or downgrade with justification.** If a finding is invalid, reject
   it explicitly.

## Legal & Affiliation Boundaries

- Iron Curtain is **not** affiliated with Electronic Arts.
- This crate ships **zero** copyrighted EA content. It is a content manager only.
- Users supply their own legally-obtained game assets or download freeware
  content from public mirrors.

## Project Structure

```
src/
  lib.rs              — crate root, core types, convenience functions
  actions.rs          — InstallAction enum (Copy, ExtractMix, ExtractZip, etc.)
  downloads.rs        — HTTP download package definitions (OpenRA mirrors)
  downloader.rs       — HTTP download + ZIP extraction (feature-gated: download)
  executor.rs         — Install recipe executor
  packages.rs         — Content package definitions (8 packages, 3 required)
  sources.rs          — Content source definitions (12 sources with SHA-1 IDFiles)
  verify.rs           — Source identification + installed content verification
  tests.rs            — Unit tests
  bin/
    main.rs           — CLI entry point (status, download, verify, identify)
```

### Rules

1. **Each module is a flat file** — the crate is small enough that directory
   modules are not needed yet. When any file exceeds ~600 lines, split it.
2. **Feature gates** — networking code (`downloader.rs`) is behind the
   `download` feature. CLI code is behind the `cli` feature.
3. **Const data** — package, source, and download definitions are `const`
   static data, not deserialized from config files. This ensures compile-time
   correctness.

## Coding Principles

### No `.unwrap()` in Production Code

Production code must **never** call `.unwrap()`, `.expect()`, or any method
that panics on `None`/`Err`. Use `?`, `.ok_or()`, `.map_err()`, or
`.unwrap_or()` instead.

**Test code** may use `.unwrap()` freely.

### Integer Overflow Safety

Use `saturating_add` (or `checked_add` where recovery is needed) at every
arithmetic boundary where untrusted input influences the operands.

### Error Design

Use `thiserror` for structured error types. Every error variant must carry
enough context for callers to produce diagnostics.

### Implementation Comments

Every non-trivial block must carry comments answering:
1. **What** — what this code does
2. **Why** — the design decision or rationale
3. **How** (when non-obvious) — algorithm steps or domain specifics

## Testing Standards

### Test Documentation

Every `#[test]` function must have a `///` doc comment explaining:
1. **What** — the scenario being tested
2. **Why** — the invariant or edge-case being verified

### Required Test Categories

- **Happy path:** verify correct operation with well-formed input
- **Error paths:** each error variant must be tested
- **Boundary:** test both sides of limits

### Integration Tests

Integration tests that require real game content use a test provisioner that
downloads content from OpenRA mirrors on first run and caches it in
`target/test-content/ra/v1/`. These tests run as part of the normal test
suite — they are **never** `#[ignore]`d.

### Verification Workflow

After any code change:

```
cargo test
cargo clippy --tests -- -D warnings
cargo fmt --check
```

## Local Rules

- **Language:** Rust (2021 edition)
- **Build:** `cargo build`
- **Test:** `cargo test`
- **Lint:** `cargo clippy --tests -- -D warnings`
- **Format:** `cargo fmt --check`
- **License check:** `cargo deny check licenses`
- **Local CI (PowerShell):** `./ci-local.ps1`
- **Local CI (Bash/WSL):** `bash ci-local.sh`

### Local CI Scripts

`ci-local.ps1` (PowerShell) and `ci-local.sh` (Bash) mirror the GitHub Actions
CI pipeline locally. Run either script from the repo root before pushing.

Steps performed (in order):

1. UTF-8 encoding validation (all `.rs` files, `Cargo.toml`, `README.md`)
2. Auto-fix formatting and clippy (`cargo fmt`, `cargo clippy --fix`)
3. Format check (`cargo fmt --check`)
4. Clippy lint — all features and no-default-features
5. Tests — all features and no-default-features
6. Documentation build (`cargo doc` with `-D warnings`)
7. License check (`cargo deny check licenses`)
8. Security audit (`cargo audit`)
9. MSRV check (compile, clippy, and test against `rust-version` from
   `Cargo.toml`)

## Current Implementation Target

- Content manifest data: **complete** (8 packages, 12 sources, 4 downloads)
- Install actions: **partial** (Copy, ExtractMix, ExtractRaw working; ExtractIscab, ExtractZip stubbed)
- HTTP downloader: **complete** (OpenRA mirror download + SHA-1 verification)
- Content verification: **complete** (SHA-256 manifest generation + verification)
- Source probes: **planned** (Steam, GOG, Origin, Disc detection)
- P2P distribution: **planned**
- Setup wizard UI: **planned** (lives in ic-game, not this crate)
