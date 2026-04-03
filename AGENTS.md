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
The design repo has broader context and understanding of the overall
architecture.  This file is a local implementation guide, not a design
authority.  When in doubt, check the design docs.  If you have questions,
raise them by opening an issue in the design-docs repo.

Primary canonical references:

- `src/decisions/09a/D076-standalone-crates.md` — standalone crate extraction strategy
- `src/18-PROJECT-TRACKER.md` — execution overlay, milestone ordering
- `src/16-CODING-STANDARDS.md` + `src/coding-standards/quality-review.md` — code style and review checklist
- `src/decisions/09a/D049-*` — content distribution, P2P, web seeding design

## Non-Negotiable Rule: No Silent Design Divergence

If implementation reveals a missing detail, contradiction, or infeasible design path:

- do **not** silently invent a new canonical behavior
- open a design-gap/design-change request in the design-doc repo
- mark local work as `implementation placeholder` or `blocked on Pxxx`

### Before Proposing Any Removal — Check Design Docs First

**Never propose removing a module, public function, feature flag, or
architectural element without first reading the relevant design doc
(especially `D076-standalone-crates.md`).**

This crate serves a broader audience than the Iron Curtain engine alone.
Features that seem unnecessary from an engine perspective may exist because
D076 explicitly mandates them for the crate's standalone community utility.

**Workflow before questioning any existing feature:**

1. Search D076 for the feature name or related keywords.
2. If D076 mandates it, the feature stays — end of discussion.
3. If D076 is silent, check `18-PROJECT-TRACKER.md`.
4. Only if *no* design doc mentions or implies the feature may you raise
   the question with the maintainer — and even then, do not propose removal
   without explicit approval.

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
- Define what each game needs (RA, TD, Dune 2, Dune 2000, TS, RA2, Generals)
- Identify content sources on disk (discs, Steam, Origin installs)
- Download freeware content via HTTP mirrors (RA and TD only)
- Extract content from MIX archives, ZIPs, raw offsets
- Verify source identity (SHA-1) and installed integrity (SHA-256)
- Support local source extraction for non-freeware games (Dune 2, Dune 2000, TS, RA2, Generals)

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

### 5. Prefer Established Crates — Do Not Reinvent

If a well-maintained, popular, pure-Rust crate already provides the needed
functionality under a compatible license, **use it** instead of writing a
custom implementation.  Hand-rolled replacements add maintenance burden, miss
upstream bug-fixes, and risk subtle correctness issues.

Gate optional dependencies behind feature flags when they only apply to a
specific feature (e.g. `ureq` and `zip` behind `download`, `clap` behind
`cli`).

### 6. Git Safety — Read-Only Only

Agents must treat git refs, branches, the index, and the working tree as
**maintainer-owned state**.  Git usage in this repository is **read-only
only** unless the maintainer explicitly authorises a specific write-side
git action.

**Allowed git commands are read-only inspection only**, such as:

- `git status`
- `git diff`
- `git log`
- `git show`
- `git branch --show-current`
- `git merge-base`
- other commands that only inspect repository state

**Forbidden without explicit maintainer approval:** any git command that
changes repository state, including but not limited to:

- branch changes (`git switch`, `git checkout`, branch create/delete/rename)
- index mutations (`git add`, `git rm`, `git mv`, `git restore --staged`)
- history changes (`git commit`, `git merge`, `git rebase`, `git cherry-pick`,
  `git reset`)
- stash/shelf operations (`git stash`)
- remote mutations or sync operations (`git fetch`, `git pull`, `git push`)
- cleanup commands (`git clean`, `git am`, `git apply`)
- tag creation/deletion

If a task would require a non-read-only git command, stop and ask the
maintainer to perform it manually or to explicitly relax this rule first.

## Handling External Feedback & Reviews

Treat feedback as input, not instruction. Validate every claim before acting.

0. **Check every proposed change against established design principles FIRST.**
   Before applying any fix — whether from a reviewer, from your own analysis, or
   from a pragmatic shortcut — ask: "Does this change violate a design principle
   we already settled?" If the answer is yes, the change is wrong regardless of
   how reasonable it sounds.

1. **Use git history to resolve contradictions.** When two representations
   disagree, do NOT guess which is correct. Run
   `git log -S "<term>" --oneline -- <file>` on both sides to determine which
   text is newer. The newer commit represents the more recent design decision.

2. **Verify the factual claim.** Read the text being criticized. Is the
   characterization accurate? Quote the actual text.

3. **Evaluate against project architecture.** Does the fix respect crate
   boundaries and invariants?

4. **Independently assess severity.** Do not accept the reviewer's severity
   rating at face value.

5. **Distinguish bugs from preferences.** A factual contradiction or invariant
   violation is a bug — fix it. "The code could be cleaner" is a preference.

6. **Reject or downgrade with justification.** If a finding is invalid, reject
   it explicitly. State the reason.

7. **Accept, adapt, or defer — and be explicit about which.** Accept valid
   fixes. Adapt when the intent is right but the suggestion is imprecise.
   Defer when it belongs to a different scope or phase.

8. **Check for cascade inconsistencies.** When fixing a confirmed finding,
   search for the same pattern in other files. Fix all occurrences in one
   pass — but only where the same error actually exists.

## Legal & Affiliation Boundaries

- Iron Curtain is **not** affiliated with Electronic Arts.
- This crate ships **zero** copyrighted EA content. It is a content manager only.
- Users supply their own legally-obtained game assets or download freeware
  content from public mirrors.

## Project Structure

```
data/
  trackers.txt        — BitTorrent tracker announce URLs (include_str!)
src/
  lib.rs              — crate root, core types (GameId, PackageId, SeedingPolicy, etc.)
  query.rs            — convenience lookup/filter functions (re-exported from lib.rs)
  actions.rs          — InstallAction enum (Copy, ExtractMix, ExtractZip, ExtractBig, ExtractMeg, ExtractBagIdx, etc.)
  config.rs           — Config persistence (TOML), SeedingPolicy, speed limits
  downloads.rs        — HTTP download definitions (16 packages, RA + TD only)
  packages.rs         — Content package definitions (25 packages, 7 games)
  sources.rs          — Content source definitions (36 sources with SHA-1 IDFiles)
  torrent_create.rs   — Deterministic .torrent file generation (bencode, info hash)
  downloader/
    mod.rs            — HTTP download + parallel mirror racing (feature-gated: download)
    tests.rs          — downloader unit and security tests
  executor/
    mod.rs            — Install recipe executor (MIX, ISCAB, ZIP, raw, copy, delete)
    tests.rs          — executor unit and path-traversal security tests
  iscab/
    mod.rs            — InstallShield CAB v5/v6 archive reader (zlib decompression)
    tests.rs          — iscab unit tests
  recipes/
    mod.rs            — Install recipes (source × package action sequences); all RA
  session/
    mod.rs            — ContentSession: high-level game content lifecycle API (feature-gated: download)
    tests.rs          — ContentSession unit and security tests
  streaming/
    mod.rs            — Byte-range tracking, piece mapping, prebuffer policy, StreamingReader
    tests.rs          — streaming unit tests
  torrent.rs          — P2P BitTorrent transport via librqbit (feature-gated: torrent)
  verify/
    mod.rs            — Source identification + installed content verification (SHA-1/SHA-256)
    tests.rs          — verify unit tests
  tests/
    mod.rs            — Cross-module integration test root
    core.rs           — GameId, package, and source invariant tests
    downloads.rs      — Download resolution, mirror list, and seeding policy tests
    post_download.rs  — Torrent hash validation and post-extraction manifest tests
  source/
    mod.rs            — detect_all() orchestrator
    steam.rs          — Steam library probe (VDF parsing, app ID matching)
    origin.rs         — Origin / EA App probe (registry + filesystem)
    gog.rs            — GOG.com / GOG Galaxy probe (registry + filesystem)
    registry.rs       — Legacy Windows registry probe (Westwood/EA keys)
    openra.rs         — OpenRA content directory probe
    disc.rs           — Mounted disc / ISO volume probe
    vdf.rs            — Valve Data Format (VDF/KeyValues) parser
  bin/cnc_content/
    main.rs           — CLI entry point: Parser structs + main() dispatch (~240 lines)
    progress.rs       — Download progress display (indicatif progress bars, spinners)
    commands/
      mod.rs          — command submodule declarations
      status.rs       — cmd_status, cmd_verify, cmd_detect, cmd_identify, cmd_games, cmd_seed_config
      install.rs      — cmd_download, cmd_install, cmd_clean, cmd_torrent_create
```

### Rules

1. **Each module is a flat file** — the crate is small enough that directory
   modules are not needed yet. When any file exceeds ~600 lines, split it
   into a directory module (`foo/mod.rs` + `foo/tests.rs`).
2. **Feature gates** — networking code (`downloader.rs`) is behind the
   `download` feature. CLI code is behind the `cli` feature.
3. **Const data** — package, source, and download definitions are `const`
   static data, not deserialized from config files. This ensures compile-time
   correctness.
4. **`data/` directory and `include_str!`** — external data that may change
   independently of code logic (URLs, tracker lists) lives in plain-text
   files under `data/` and is embedded at compile time via `include_str!`.
   See the [Data Externalisation](#data-externalisation-include_str) section.
5. **RAG / LLM-friendly** — keep files small and focused. Split before ~600
   lines. Prefer stable top-to-bottom layout: module docs → imports →
   constants → types → impl blocks → functions → tests.
6. **Modules are independently understandable.** Each module must be clear,
   maintainable, and testable in isolation. A developer reading a single
   module should understand its purpose, invariants, and failure modes
   without reading the rest of the crate. This means:
   - Module-level `//!` doc comments explain the module's role and how it
     fits into the crate.
   - Public types are self-explanatory through their names, field types,
     and doc comments — not through comments that say "see module X".
   - Dependencies between modules flow through explicit public APIs, not
     shared mutable state or implicit ordering.
   - Side effects (I/O, network, filesystem) are isolated behind
     parameters or trait boundaries so that pure logic can be tested
     without the side effect. Extract testable logic into pure functions
     that take inputs and return outputs.
7. **Extract testable logic from side-effectful functions.** When a function
   mixes pure computation with I/O (network, filesystem), extract the pure
   computation into a separate function that can be unit-tested without
   mocking. The I/O function becomes a thin wrapper that fetches data and
   delegates to the pure function. This is preferred over mocking
   frameworks.

## Data Externalisation (`include_str!`)

Use `include_str!` **only** for data that benefits from being externally
editable — things that change over time, may become stale, or that community
contributors should be able to update without touching Rust syntax.

### When to use `include_str!`

- **URLs and tracker lists** — tracker announce URLs, mirror bootstrap URLs.
  These can become invalid or need extension over time.
- **Configuration that benefits from community edits** — data where a
  contributor might submit a PR adding a new tracker or mirror without
  needing to understand Rust.

Current external files:

```
data/
  trackers.txt — BitTorrent tracker announce URLs (5 trackers)
```

### When NOT to use `include_str!`

- **Immutable game data** — file names of game resources (MIX files, VQA
  movies, PAK archives) are fixed properties of games released in the 1990s.
  They will never change. Keep them as inline `&[&str]` slices in Rust.
- **Structured data** — anything that benefits from compile-time type
  checking (enums, structs). Keep as Rust `static` definitions.
- **Tiny lists** — 1–2 items where inline is obviously clearer.
- **Single-line files** — if your external file would contain one line,
  that is a sign it should be inline.

## Environment Variable Overrides

Certain runtime behaviours should be overridable via environment variables
so that developers, packagers, and CI can adjust without recompilation.

| Variable               | Purpose                                         | Default                                       |
| ---------------------- | ----------------------------------------------- | --------------------------------------------- |
| `CNC_CONTENT_ROOT`     | Override the default content root directory     | Platform-specific app data path               |
| `CNC_MIRROR_LIST_URL`  | Override the mirror list URL for all downloads  | Per-package URL from download definitions     |
| `CNC_DOWNLOAD_TIMEOUT` | HTTP download timeout in seconds                | `300`                                         |
| `CNC_NO_P2P`           | Disable P2P transport entirely (`1` = disabled) | `0` (P2P enabled when feature is compiled in) |

### Guidelines for adding new overrides

1. Prefix all env vars with `CNC_` to avoid collisions.
2. Document each override in this table.
3. Use `std::env::var("CNC_...")` at the call site — do not cache globally.
4. Env vars override compiled-in defaults but never override explicit
   user CLI flags (CLI flags win over env vars).

## Coding Session Discipline

These rules govern how implementation work is carried out. They are not
optional style preferences.

### 1. Test-First / Proof-First

- For every non-trivial behavior change, bug fix, or new feature: **write
  or update the tests first** so the expected behavior is explicit before
  implementation changes begin.
- Tests are not cleanup. They are the primary proof artifact that the design
  was understood correctly and implemented correctly.
- The intended workflow is **red → green → refactor**.
- Every problem or bug fixed must include a regression test as part of the
  same change set.
- When closing work, call out the exact tests that serve as evidence.
  "Implemented" without proof is not acceptable.

### 2. Evidence Rule

Do not claim a feature is complete without evidence:

- tests (unit, integration, or conformance)
- CI output showing clean build + test pass
- manual verification notes (if no automation exists yet)

## Coding Principles

### Module Documentation

Every source file must open with a `//!` (inner) doc comment.  The comment
must state, at minimum:

- **What** the file provides: formats handled, subcommand implemented, or
  helpers exposed.
- **Why** the file exists as a separate file (when it is a split-off).
- **How** the module works at a high level: key invariants, data flow, or
  algorithm strategy.

### Implementation Comments (What / Why / How)

A reviewer should be able to learn and understand the entire design by
reading the source alone — without consulting external documentation, git
history, or the original author.

Every non-trivial block of implementation code must carry comments that
answer up to three questions:

1. **What** — what this code does (one-line summary above the block).
2. **Why** — the design decision or domain rationale that motivated this
   approach over alternatives.
3. **How** (when non-obvious) — algorithm steps or domain specifics.

Specific guidance:

- **Constants and magic numbers:** document the origin and meaning.
- **Section headers:** use `// ── Section name ───…` comment bars to
  visually separate logical phases within a long function.
- **Safety-critical paths:** every security guard (Zip Slip check, SHA-1
  verification, boundary validation) must have an inline comment explaining
  *what* it prevents.
- Avoid obvious comments like "increment counter". Spend comments on
  context, rationale, and constraints.

### Error Design

- Use `thiserror` for structured error types.
- Every variant must carry **structured fields** (named, not positional)
  that provide enough context for callers to produce diagnostics.
- Implement `Display` so the human-readable message embeds key context
  (file paths, byte counts, expected vs. actual values).
- Every error must answer: **what** failed, **where** it failed, **why** it
  failed, and (when knowable) **how to fix it**.

### No `.unwrap()` in Production Code

Production code must **never** call `.unwrap()`, `.expect()`, or any method
that panics on `None`/`Err`. Use `?`, `.ok_or()`, `.map_err()`, or
`.unwrap_or()` instead.

**Test code** may use `.unwrap()` freely.

### Integer Overflow Safety

Use `saturating_add` (or `checked_add` where recovery is needed) at every
arithmetic boundary where untrusted input influences the operands.
Never rely on Rust's debug-mode overflow panics as the safety mechanism;
the code must be correct in release mode.

### No Dead Code, No `#[allow(dead_code)]`

This crate must never contain dead code, and `#[allow(dead_code)]` is
**unconditionally banned**. The compiler's dead-code warning exists to
catch unused code that should be removed. Suppressing it masks real
problems.

If a function, method, or type triggers a dead-code warning:

1. **Use it** — if it's part of the intended API, wire it into
   production code so it is called.
2. **Remove it** — if it is genuinely unused, delete it.

`#[cfg(test)]` helper methods on production types are acceptable only if
they are actually called from tests. Verify this — do not assume.

The same principle applies to `#[allow(unused_imports)]`,
`#[allow(unused_variables)]`, and all other lint-suppression attributes.
If the compiler says it's unused, fix the root cause instead of
silencing the warning.

### Path Security with `strict-path`

All file I/O that involves **untrusted or external paths** must be
boundary-enforced using the [`strict-path`] crate. This prevents path
traversal attacks (Zip Slip, `../` escapes, symlink attacks, Windows
8.3/ADS tricks) with mathematical proof — not string heuristics.

**When to use `strict-path`:**

- Archive extraction (ZIP, MIX, ISCAB) — archive entry names are
  untrusted input.
- Downloaded content extraction — file names originate from the network.
- Any code that joins a user-supplied or archive-supplied subpath onto a
  base directory.

**How to use it correctly:**

1. Create a `PathBoundary` for each security boundary (content root,
   source root) at the entry point of the operation.
2. Use `boundary.strict_join(subpath)` to validate every subpath.
3. Use `StrictPath`'s built-in I/O methods (`.read()`, `.write()`,
   `.open_file()`, `.create_file()`, `.create_parent_dir_all()`) instead
   of `std::fs` functions where possible.
4. **Never expose `strict-path` types in public APIs.** Public functions
   accept standard `&Path` / `PathBuf`. Create boundaries internally and
   keep all `PathBoundary` / `StrictPath` usage private.
5. **Never wrap `.interop_path()` in `Path::new()` or `PathBuf::from()`**
   — this defeats all security guarantees.

**When NOT to use `strict-path`:**

- Paths that are entirely within the crate's control (compile-time
  constants, game slug strings). These are trusted by definition.
- However, even trusted paths benefit from boundary enforcement as a
  defense-in-depth measure.

[`strict-path`]: https://docs.rs/strict-path

### Type Safety

- **Newtypes for domain identifiers.** Use newtype wrappers for
  domain-specific identifiers to prevent accidental mixing of semantically
  different values (`PackageId`, `SourceId`, `DownloadId`, `GameId`).
- **`Option` / `Result` over sentinel values.** Never use `-1`, `0`,
  `""`, or null-equivalent magic values to signal absence. Use `Option`
  or `Result` so the compiler forces callers to handle the missing case.
- **Exhaustive matching.** Prefer `match` over `if let` when handling enums
  so that adding a new variant produces a compile error at every site that
  must handle it.
- **Visibility and constructor control.** Keep struct fields private when
  invariants must be enforced. Expose transition methods that enforce them.

### Rust Zero-Cost Design Patterns

Rust's type system enables compile-time guarantees that other languages
enforce at runtime. This crate must exploit these patterns — not as
optional style, but as load-bearing correctness infrastructure.

#### Typestate pattern — encode state transitions in types

When an object has distinct lifecycle phases (e.g. "unverified" →
"verified", "open" → "sealed"), represent each phase as a separate type
or generic parameter. Invalid transitions become compile errors, not
runtime panics.

```rust
struct Unverified;
struct Verified;
struct Download<State> { /* … */ state: PhantomData<State> }

impl Download<Unverified> {
    fn verify(self) -> Result<Download<Verified>, VerifyError> { /* … */ }
}
impl Download<Verified> {
    fn install(self) -> Result<(), InstallError> { /* … */ }
}
// install() is impossible to call on Unverified — no impl exists.
```

Use this when incorrect ordering would silently corrupt data or violate
a security invariant. Do not use it for trivial state that is adequately
guarded by a boolean flag.

#### Marker types — distinguish values at the type level

When two values share the same representation but different semantics,
use zero-sized marker types (or `PhantomData<T>`) to prevent mixing.
`strict-path` already does this with `PathBoundary<T>` — the same
principle applies elsewhere. A `Sha1Digest` and a `Sha256Digest` should
not be interchangeable even though both are `[u8; N]`.

#### Builder pattern — enforce required fields at compile time

When construction requires multiple steps or mandatory fields, use a
builder whose type signature changes as fields are set. The final
`.build()` method is only available when all required fields are present.
Prefer this over runtime `Option::unwrap()` inside builders.

#### Sealed traits — restrict who can implement a trait

When a trait is part of this crate's internal contract and external
implementors would break invariants, seal the trait using a private
supertrait. This preserves the ability to add methods in future without
a breaking change.

```rust
mod private { pub trait Sealed {} }
pub trait Strategy: private::Sealed { /* … */ }
```

#### Const generics and const evaluation

Where array sizes or limits are known at compile time, use const generics
or `const fn` rather than runtime checks. For example, hash digest
lengths, piece sizes, and fixed-format header sizes should be
`[u8; N]` arrays, not `Vec<u8>`.

#### General principles

- **If it can be checked at compile time, it must be.** Runtime checks
  are fallback, not primary defense. Prefer type constraints, trait
  bounds, visibility rules, and const evaluation over assertions, debug
  panics, or sentinel checks.
- **Zero-cost means zero runtime overhead, not zero design effort.**
  These patterns cost compile time and API design thought. That cost is
  always justified when the alternative is a runtime failure mode.
- **Make invalid states unrepresentable.** If a combination of field
  values is nonsensical, restructure the types so that combination cannot
  be constructed. Prefer enum variants with associated data over
  `struct { kind: Kind, data: Option<T> }` where `data` is `None` for
  some `kind` values.
- **No `unsafe` code.** This crate must never contain `unsafe` blocks,
  `unsafe fn`, or `unsafe impl`. There is no legitimate reason for
  `unsafe` in a content manager — all operations (hashing, I/O, parsing,
  extraction) are expressible in safe Rust. If a dependency requires an
  unsafe interface, find a safe alternative or wrap it in a dedicated
  dependency crate — never bring `unsafe` into this crate's source.

### Lifetime Naming

Lifetime parameter names must be meaningful: name the lifetime after the
item whose lifetime it represents (e.g. `'input` for an input slice, `'buf`
for a buffer). Avoid vague single-letter names like `'a` in public APIs.

### Safe Indexing — No Direct Indexing in Production Code

Production code must **never** use direct indexing on **any type** —
`&[u8]`, `&str`, `Vec<T>`, or any other indexable container.  This applies
regardless of whether the index "feels safe" (e.g. derived from `.find()`
or bounded by a loop guard).  Direct indexing panics on out-of-bounds
access, which is a denial-of-service vector.

For **sequential processing**, use iterators, combinators, and transformers
(`.iter()`, `.map()`, `.filter()`, `.enumerate()`, `.zip()`, `.flat_map()`,
`.fold()`, etc.) instead of index-based loops. Prefer `.windows()`,
`.chunks()`, `.split()`, and similar slice iterators over manual index range
loops. When iterating with an index for bookkeeping, use `.enumerate()`
rather than a manual counter.

**Banned patterns (all of these panic on OOB):**

```rust
data[offset]           // byte slice indexing
data[start..end]       // byte slice range
line[pos..]            // string slicing
content[..colon_pos]   // string slicing with find()-derived index
entries[i].0           // vec/slice element access
bytes[i]               // byte array indexing
value.as_bytes()[0]    // first-byte access
```

**Required replacements:**

| Banned                | Replacement                                            |
| --------------------- | ------------------------------------------------------ |
| `data[offset]`        | `data.get(offset).ok_or(Error::…)?`                    |
| `data[start..end]`    | `data.get(start..end).ok_or(Error::…)?`                |
| `line[pos..]`         | `line.get(pos..).unwrap_or("")`                        |
| `&line[..pos]`        | `line.get(..pos).unwrap_or(line)`                      |
| `entries[i]`          | `entries.get(i).map(…)` or `entries.get_mut(i).map(…)` |
| `bytes[i]`            | `bytes.get(i) == Some(&val)`                           |
| `value.as_bytes()[0]` | `value.as_bytes().first()`                             |

**Text parsers** should use `.get()` with `.unwrap_or("")` (or
`.unwrap_or(original)` when the fallback is the unsliced source).
Even though `str::find()` returns valid UTF-8-aligned indices, the rule
is absolute — no reviewer should ever need to *reason* about whether an
index is safe.  If it compiles without `.get()`, it's wrong.

**Test code** (`#[cfg(test)]` blocks) may use direct indexing when the test
controls the input and panic-on-bug is acceptable.

### Heap Allocation Policy

Minimise heap allocation in library code to reduce allocator overhead and
memory fragmentation for downstream consumers.

**Rules (in priority order):**

1. **Hot paths must not heap-allocate.** Any function called per-file,
   per-entry, or per-byte (e.g. CRC computation, hash loops, VDF token
   scanning) must be zero-allocation. Use stack buffers, byte-by-byte
   processing, or iterator patterns instead of `String`, `Vec`, or `Box`.

2. **Parsers should borrow, not copy.** When the parsed result can reference
   the input slice (via `&'a [u8]` or `&'a str`), prefer borrowing over
   `.to_vec()` or `.to_string()`. This eliminates per-entry allocations
   during bulk parsing.

3. **Fixed-size scratch buffers belong on the stack.** When the maximum size
   is bounded and small (≤ ~4 KB), use a `[T; N]` array instead of
   `Vec<T>`.

4. **`Vec::with_capacity` for necessary allocations.** When a heap
   allocation is unavoidable (variable-length output like decompressed
   data), always use `Vec::with_capacity(known_size)` to avoid
   reallocation.

5. **Prefer bulk operations over per-element loops.**
   - `Vec::extend_from_slice` over N × `push` for literal copies (memcpy).
   - `Vec::extend_from_within` over N × indexed-push for non-overlapping
     back-references (memcpy from self).
   - `Vec::resize(len + n, value)` over N × `push(value)` for fills
     (memset).
   These let the compiler emit SIMD/vectorised memory operations.

### CLI Global Allocator (`mimalloc`)

The CLI binary (`src/bin/cnc_content/main.rs`) must use `mimalloc` as the global
allocator on native targets, consistent with the Iron Curtain engine's
allocator strategy. This reduces fragmentation and improves throughput for
the download/extraction workloads.

```rust
#[cfg(not(target_arch = "wasm32"))]
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;
```

The `mimalloc` dependency is gated behind the `cli` feature flag so that
library-only consumers are not forced to link it.

## Testing Standards

### Test Documentation

Every `#[test]` function must have a `///` doc comment with up to three
paragraphs:

1. **What** (first line) — the scenario being tested.
2. **Why** (second paragraph) — the invariant, correctness guarantee, or
   edge-case rationale that motivates the test.
3. **How** (optional third paragraph) — non-obvious test construction details.

Omit the "How" paragraph when the test body is self-explanatory.

### Test Organisation

Tests within each module are grouped under section-comment headers:

```rust
// ── Category name ────────────────────────────────────────────────────
```

### Doc Examples Must Compile and Pass

All `///` and `//!` code examples (doctests) must compile, run, and pass.
Never use `no_run`, `ignore`, or `compile_fail` annotations to skip
execution. If a code example requires filesystem or network access, rewrite
it to use in-memory data.

### Required Test Categories

- **Happy path:** verify correct operation with well-formed input.
- **Error paths:** each error variant must be tested, including verification
  that structured fields carry correct values.
- **Display messages:** at least one test asserting `Error::Display` output
  contains key context values.
- **Determinism:** call the same operation twice, assert equality.
- **Boundary:** test both sides of limits (at cap succeeds, past cap fails).

### Security Testing

Security-critical modules (`downloader.rs`, `executor.rs`) must include
adversarial tests:

- **Zip Slip traversal:** `../`, absolute paths, backslash evasion —
  verified via `strict_path::PathBoundary` rejection.
- **SHA-1 verification:** mismatched hashes, placeholder detection.
- **Path boundary:** all extracted paths validated against content root
  via `strict-path` boundaries (see "Path Security with `strict-path`"
  above).

### Test Fixture Legality

- Never commit proprietary or copyrighted game assets to this repository.
- CI-required tests must be self-contained and legally redistributable.
  Generate synthetic fixtures inline or build minimal valid payloads with
  test helpers.
- When real installed assets are useful for extra validation, gate them
  behind environment variables — never make CI depend on proprietary files.

### Verification Workflow

After any code change, always run the full verification before considering
the task complete:

```
cargo test
cargo clippy --tests -- -D warnings
cargo fmt --check
```

All three must pass cleanly (zero warnings, zero format diffs).

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

## Multi-Source Download Architecture

The design docs (D049) specify concurrent multi-transport downloads where
HTTP mirrors and BitTorrent peers aggregate bandwidth simultaneously.

**Current implementation:**

1. **Parallel mirror racing** — all mirrors start concurrently, first
   successful download wins, losers are cancelled via `AtomicBool`.
   Single-URL downloads skip the thread pool overhead. Implemented in
   `downloader.rs::parallel_download()`.
2. **Sequential fallback** — single-URL paths use simple sequential I/O.
3. **P2P distribution** — BitTorrent via `librqbit` behind the `torrent`
   feature flag. Uses magnet URIs with public trackers from
   `data/trackers.txt`. Active when packages have populated `info_hash`
   fields.

The sequential fallback is always available as the reliability baseline.

## URL and Domain Integrity

Every URL compiled into the crate must point to an actually-reachable
resource. Do not invent domains or URLs that do not exist.

**Known-live domains:**
- `www.openra.net` — OpenRA mirror list infrastructure (verified)
- `files.cncnz.com` — CNCNZ community file archive (RA + TD freeware ISOs)
- `raw.githubusercontent.com` — GitHub raw content (bootstrap repo)
- `archive.org` — Internet Archive, non-profit digital library (freeware C&C ISOs)
- `downloads.cncnet.org` — CNCNet community hub (freeware game installers)
- `cdn.mailaender.name` — Community-hosted OpenRA content mirror (direct fallback)
- `openra.0x47.net` — Community-hosted OpenRA content mirror (direct fallback)

**Bootstrap repo:** `iron-curtain-engine/content-bootstrap` on GitHub hosts
mirror list files for IC-hosted freeware content. Mirror lists are served
via `raw.githubusercontent.com`. All IC-hosted download packages point to
mirror list files in this repo. Mirror lists may be empty (no mirrors
populated yet) but the files and URLs must exist.

## Current Implementation Status

- Content manifest data: **complete** (25 packages, 36 sources, 16 downloads across 7 games)
- Install actions: **complete** (Copy, ExtractMix, ExtractMixFromContent, ExtractIscab, ExtractZip, ExtractRaw, Delete, ExtractBig, ExtractMeg, ExtractBagIdx)
- HTTP downloader: **complete** (parallel mirror racing + SHA-1 verification)
- Content verification: **complete** (SHA-256 manifest generation + verification)
- Source probes: **complete** (Steam VDF, Origin/EA App, GOG Galaxy/classic, Windows registry, OpenRA, disc/ISO)
- P2P distribution: **code complete, content pending** (torrent.rs with librqbit behind `torrent` feature flag; all `info_hash` fields are empty — see "P2P Content Pipeline Gap" below)
- Parallel downloads: **complete** (multi-mirror racing via thread pool; single-URL fast path)
- InstallShield CAB: **complete** (iscab.rs reader for v5/v6 archives, zlib decompression)
- Setup wizard UI: lives in ic-game, not this crate
- Freeware-only downloads: **enforced** (Dune 2 and Dune 2000 are local-source-only)

## P2P Content Pipeline Gap

The torrent code (`torrent.rs`) is feature-complete: magnet URI construction,
`librqbit` session management, public tracker list from `data/trackers.txt`,
size-based transport selection (D049 strategy). **However, no packages have
`info_hash` values populated** — all are empty strings.

Closing this gap requires an **operations pipeline**, not more Rust code:

1. **Build content ZIPs** — for each `DownloadPackage`, assemble the
   exact files it provides into a deterministic ZIP archive.
2. **Generate `.torrent` files** — hash each ZIP into a `.torrent` with
   piece length, info hash, and the public trackers from
   `data/trackers.txt`.
3. **Record info hashes** — populate the `info_hash: ""` fields in
   `downloads.rs` with the hex-encoded BitTorrent v1 info hashes.
4. **Seed** — upload the `.torrent` files and start seeding from at
   least one reliable host.
5. **Verify** — confirm that `TorrentDownloader::download_package()`
   resolves the magnet URI, finds peers, downloads, and passes SHA-256
   verification.

Until this pipeline runs, all downloads use HTTP mirrors only. The
`should_use_p2p()` function returns `false` for every package because
`info_hash` is empty.

**IC-hosted content** (RA music, movies, expansion music) additionally
requires the `content-bootstrap` repo's mirror list files to be populated
with actual mirror URLs. Currently these mirror lists are empty files.

## Execution Overlay Mapping

- **Milestone:** `M1` (Resource Fidelity)
- **Priority:** `P-Core`
- **Feature Cluster:** D076 Tier 1
- **Depends on:** `cnc-formats` (Tier 1 peer)
