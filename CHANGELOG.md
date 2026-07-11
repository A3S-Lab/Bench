# Changelog

All notable changes to a3s-bench are documented in this file.

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

[0.1.0-preview.1]: https://github.com/A3S-Lab/Bench/releases/tag/v0.1.0-preview.1
