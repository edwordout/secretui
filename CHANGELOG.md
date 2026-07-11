# Changelog

All notable user-visible changes are recorded here. This project follows
[Semantic Versioning](https://semver.org/) while the `0.1.x` interface is still
allowed to evolve.

## [Unreleased]

## [0.1.2] - 2026-07-11

**SecretUI v0.1.2 focuses on safe mutation, predictable lock handling,
hardened terminal rendering, accurate diagnostics, and trustworthy release
distribution.**

### Added

- Added deterministic, conflict-aware `ImportPlan`, `PlannedChange`,
  `ImportConflict`, and `ApplyReport` metadata APIs.
- Added a second full import preflight, exact-plan consumption, no-clobber
  recovery documents, and durable per-operation apply reports.
- Added strict metadata version parsing, D-Bus path validation, global duplicate
  detection, and bounded document/field/count limits.
- Added structured store warnings, temporary-unlock cleanup tracking, relock
  verification, and shutdown cleanup.
- Added deterministic in-memory store recording and failure/concurrency controls
  for unit and TUI tests.
- Added this changelog, security policy and threat model, metadata-format
  reference, and reproducible release procedure.

### Changed

- Reframed the old import workflow as **same-store metadata repair**. Apply
  matches exact collection and item paths only; it does not move data between
  providers.
- Split backend mutation operations by collection label, item label, item
  attributes, secret replacement, and deletion. Item operations now identify
  both collection and item paths.
- Metadata apply now blocks all writes on any preflight conflict, stops on the
  first provider/journal failure, records partial completion, and exits nonzero
  unless complete.
- Metadata export is no-clobber and symlink-safe by default, supports explicit
  `--force`, creates mode-`0600` files, and syncs both file and parent directory.
- TUI metadata edits apply only real differences in label-then-attributes order;
  secret replacement is a distinct operation and is never automatically rolled
  back.
- Details initially selects Back and adds `r` for explicit reveal. Delete
  confirmation initially selects Cancel, displays a sanitized snapshot, warns
  that metadata cannot recover the secret, and detects last-second drift.
- TUI and CLI output now share bounded terminal-safe rendering for control,
  escape, bidirectional, long, and unusual Unicode text.
- Provider read failures no longer invent labels, lock states, timestamps, or
  creation results. Errors name the operation and sanitized target; creation
  verification failures warn that creation may already have occurred.

### Security

- Temporary unlocks now restore and verify original collection/item lock state
  after ordinary success and failure. Relock failures remain visible warnings
  and pending targets are retried during normal shutdown.
- Secret reveal state is cleared centrally on identity, filter, refresh,
  navigation, failure, expiry, and shutdown transitions.
- Exports, plans, reports, errors, and operation logs never include secret bytes.
  Secret previews remain MIME-aware, escaped, limited to 256 bytes, zeroized,
  and expired after 30 seconds.

### Compatibility

- The release artifact is `x86_64-unknown-linux-gnu`, dynamically linked, built
  on Ubuntu 22.04, and requires glibc 2.34 or newer.
- The required live release gate targets Ubuntu 26.04/KDE and KWallet 6.24.0.
  Its latest unattended run was cancelled at the provider prompt, so KWallet,
  GNOME Keyring, and KeePassXC remain unverified until an authorized run passes.

## [0.1.1] - 2026-07-11

- Added safe binary-secret inspection with MIME-aware escaped text and hex
  previews, explicit Base64/hex clipboard encodings, preview bounds, expiry, and
  zeroization.
- Clarified the terminal administration/troubleshooting positioning and added
  the TUI screenshot and prebuilt-release installation instructions.

## [0.1.0] - 2026-07-11

- Initial public release of the keyboard-driven Secret Service TUI.
- Added collection/item browsing, create/edit/delete workflows, metadata-only
  deterministic export/import preview, clipboard actions, man generation, Bash
  completion, CI, and release packaging.

[Unreleased]: https://github.com/edwordout/secretui/compare/v0.1.2...HEAD
[0.1.2]: https://github.com/edwordout/secretui/compare/v0.1.1...v0.1.2
[0.1.1]: https://github.com/edwordout/secretui/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/edwordout/secretui/releases/tag/v0.1.0
