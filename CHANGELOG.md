# Changelog

All notable changes to a3s-bench are documented in this file.

## [Unreleased]

### Added

- Added an `os-runtime` execution provider for deterministic Candidates and
  Python asset Judges using authenticated, digest-pinned A3S OS OCI steps.
- Added bounded workspace envelopes, remote result recovery, and explicit
  errors for execution classes not yet supported by the OS lifecycle.

### Changed

- Updated the bundled `a3s-code` model Candidate from A3S Code Core 4.3.3 to
  5.3.4. Its immutable package now records the implementation version and keeps
  automatic planning, continuation, and manual delegation enabled.
- Runtime-aware TaskLock creation no longer requires local Docker image
  resolution when the selected provider is `os-runtime`; it instead binds the
  digest-pinned managed Candidate and Judge runner images.
- A3S OS envelopes are capped at the production inline JSON limit of 64 KiB,
  including bounded remote result parsing.
- Stored A3S OS sessions refresh only near expiry and are reused for the
  process; partial environment credential configuration now fails closed.

### Fixed

- Initialized A3S Code model Candidates with the explicitly locked model route,
  so `--model` no longer depends on an ambient `default_model` being valid.
- Described flattened OCI source directories to model Candidates and allowed
  read-only inspection of Task-provided work-image fixtures, preventing source
  directory prefixes from being mistaken for nested workspace paths.
- Translated host workspace paths in model-generated shell commands to the
  Docker `/workspace` mount, preventing complex model Candidates from trying to
  access host-only paths inside the work container.

## [0.1.0] - 2026-07-12

First stable release of the local Bench component and its v1 CLI, TaskLock,
CandidateLock, and result contracts.

### Added

- One short, offline `quick_file_edit` conformance Task and 51 locally runnable
  long-horizon Task/Judge adapters.
- Product-neutral Candidate adapters loaded from local directories, arbitrary
  OCI artifacts, or immutable CandidateLocks.
- Bundled `a3s-code` model controller for reproducible provider/model
  comparisons using local `.a3s/config.acl` routes without A3S OS login.
- Task-owned Judges loaded from local packages or arbitrary OCI artifacts,
  including model-backed Judge routes bound into TaskLock and result identity.
- Signed-out Docker Runtime selection by default, with explicit Runtime
  provider configuration and `a3s-box` preflight support.
- GitHub Actions release packaging for Linux and macOS with component manifests
  and SHA-256 checksums.

### Changed

- Defined Candidate as a product-neutral coding agent, automated system, or
  deterministic tool, with the A3S Asset schema serving only as the current
  adapter wire format.
- Renamed user-facing Agent Asset authoring language to Candidate adapter
  authoring while preserving the existing CLI and package protocol.

## [0.1.0-preview.1] - 2026-07-11

Initial development preview.

### Added

- Local Task, Candidate, and task-owned Judge execution with a signed-out
  Docker default.
- Local `.a3s/config.acl` provider/model resolution without A3S OS login.
- Local and arbitrary OCI Agent Asset resolution, including generic ORAS
  artifacts.
- Immutable TaskLock and CandidateLock inputs, offline locked Judge execution,
  durable run journals, and identity-bound result records.
- Provisional imported Task/Judge snapshot with per-revision quarantine and
  structural validation.
- Agent Asset authoring examples for executable and model-backed Candidates.
- GitHub Actions CI, native component packaging, and prerelease publication.

### Limitations

- Built-in imports are quarantined and are not official runnable Tasks.
- Shared Runtime execution migration, signed component admission, `a3s-box`
  execution, and native Codex/Claude Code adapters remain incomplete.
- Preview artifacts produce only `local_unofficial` results.

[0.1.0]: https://github.com/A3S-Lab/Bench/releases/tag/v0.1.0
[0.1.0-preview.1]: https://github.com/A3S-Lab/Bench/releases/tag/v0.1.0-preview.1
[Unreleased]: https://github.com/A3S-Lab/Bench/compare/v0.1.0...HEAD
