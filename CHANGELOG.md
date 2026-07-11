# Changelog

All notable changes to a3s-bench are documented in this file.

## [Unreleased]

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
