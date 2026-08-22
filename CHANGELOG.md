# Changelog

All notable changes to a3s-bench are documented in this file.

## [Unreleased]

## [0.1.3] - 2026-08-22

### Added

- Added a native `a3s-code` product Candidate that runs the installed A3S CLI
  through `code exec --mode auto --tool-policy local-workspace`, binds the
  reported CLI version into CandidateLock v2, verifies the retained policy,
  and records structured token and tool usage.
- Added `a3s-code-core` as the explicit embedded-controller Candidate for
  controlled provider/model comparisons.

### Changed

- Updated the embedded A3S Code Core controller from 5.3.4 to 7.0.2. The
  `a3s-code` selector now means the complete CLI product; use
  `a3s-code-core` when the embedded controller is the intended experimental
  constant.
- Removed disposable per-Task long-term memory from the embedded controller.
  Isolated benchmark sessions still persist their conversation and trajectory
  evidence, but no longer add a hidden post-turn extraction model call or
  workspace-local memory artifacts.

## [0.1.2] - 2026-08-05

### Changed

- Candidate deadline expiry now preserves and judges the final workspace
  instead of discarding partial work. Result schema v5 records a typed
  `candidate_execution` status and timeout while remaining able to load v4
  results.
- Redesigned the project README around the immutable evaluation contract,
  complete smoke run, Task-owned Judge boundary, supported providers, and
  current release limitations.

### Fixed

- Accepted workspace-internal relative symlinks from OCI seeds while rejecting
  absolute, escaping, cyclic, and unresolvable links.
- Extracted OCI workspace seeds without preserving container ownership or
  interpolating paths through a shell.
- Retried transient Docker image-pull failures with exponential backoff.
- Implemented every imported score-rescale kind and made invalid domains or
  degenerate parameters resolve to finite scores.
- Let legacy Judges access their workspace with only `CAP_DAC_OVERRIDE`, raised
  their memory limit to 16 GiB, and removed the recursive permission rewrite.
- Recorded zero for ordinary Judge rejections and unbuildable submissions while
  retaining infrastructure errors for timeouts, signal kills, and malformed
  structured output.
- Applied submission filters before limits, truncated oversized projections
  deterministically, and ignored non-projectable sockets and other special
  files.
- Preserved case-distinct Linux paths and copied hard-linked files as
  independent regular submission files.
- Replaced candidate pipe capture with anonymous temporary files so descendant
  processes cannot hold the runner open indefinitely.

## [0.1.1] - 2026-07-19

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

[0.1.3]: https://github.com/A3S-Lab/Bench/releases/tag/v0.1.3
[0.1.2]: https://github.com/A3S-Lab/Bench/releases/tag/v0.1.2
[0.1.1]: https://github.com/A3S-Lab/Bench/releases/tag/v0.1.1
[0.1.0]: https://github.com/A3S-Lab/Bench/releases/tag/v0.1.0
[0.1.0-preview.1]: https://github.com/A3S-Lab/Bench/releases/tag/v0.1.0-preview.1
[Unreleased]: https://github.com/A3S-Lab/Bench/compare/v0.1.3...HEAD
