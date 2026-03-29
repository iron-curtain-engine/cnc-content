# Contributing to cnc-content

Thank you for your interest in contributing!

## Developer Certificate of Origin (DCO)

All contributions require a
[Developer Certificate of Origin](https://developercertificate.org/) sign-off.
Add `Signed-off-by` to your commit messages:

```
git commit -s -m "your commit message"
```

## License

Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in this crate by you shall be licensed under GPL-3.0-or-later,
without any additional terms or conditions.

## Dependencies

This crate is GPL-3.0-or-later. Dependencies may be MIT, Apache-2.0, or any
GPL-compatible license. The `cnc-formats` dependency is MIT/Apache-2.0
(clean-room parsers); this crate adds GPL-licensed game-specific logic on top.

## Code Style

Read [AGENTS.md](AGENTS.md) for the full coding standards. Key rules:

- No `.unwrap()` in production code — use `?`, `.ok_or()`, or `.unwrap_or()`
- Use `saturating_add` / `checked_add` for arithmetic on untrusted input
- Every module needs unit tests

## Running Tests

```
cargo test --all-features
cargo test --no-default-features
cargo clippy --all-features --tests -- -D warnings
cargo clippy --no-default-features --tests -- -D warnings
cargo fmt --check
```

Or run the full local CI:

```powershell
./ci-local.ps1      # PowerShell
bash ci-local.sh     # Bash / WSL
```
