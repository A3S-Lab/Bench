## fix: harden judge container and submission pipeline for large workspaces

All changes are in `src/`. See linked issues for problem descriptions and reproduction steps.

---

### 1. Remove `--cap-drop ALL` and the compensating `chmod -R`

**Files:** `src/legacy_judge.rs`, `src/runtime_profile.rs`

Dropped `--cap-drop ALL` from the judge container, restoring the Docker default capability set (which includes `CAP_DAC_OVERRIDE`). This makes root (`--user 0:0`) able to bypass file permission bits as expected, so the `chmod -R u+rwX` workaround is no longer needed and has also been removed. The remaining security layers (`no-new-privileges`, seccomp, namespace isolation) are untouched.

> **Alternative considered:** Keep `--cap-drop ALL` and selectively `--cap-add DAC_OVERRIDE`. Not adopted because judge images are trusted infrastructure and `no-new-privileges` + seccomp already mitigate escalation.

Closes #14, Closes #18.

---

### 2. Raise judge memory limit from 4 GB to 16 GB (temporary)

**File:** `src/runtime_profile.rs`

The judge container had half the memory of the candidate container (4 GB vs 8 GB), yet must compile the same codebase. Raised to 16 GB.

This is a stopgap. The proper solution is per-task memory limits configured in each task's judge spec, but that requires a larger change to the task schema and is deferred. Issue #15 remains open until that is implemented.

---

### 3. Fix root ownership in workspace seed extraction

**File:** `src/workspace.rs`

Piped `docker cp` output through `tar -x --no-same-owner` so files extracted from OCI images land with the bench process's own uid instead of root, making them accessible to host-side operations.

Closes #16.

---

### 4. Truncate oversized submissions instead of aborting before judge

**File:** `src/submission.rs`

Moved include/exclude filtering into `collect_terminal_files` so irrelevant files are removed before size checking. If the filtered set still exceeds the limit, the file list is truncated rather than aborting, allowing the judge to run and produce a score.

Closes #17.

---

### 5. Distinguish judge crashes from candidate quality failures

**File:** `src/legacy_judge.rs`

Added an exit-code check before score parsing. If the judge process was killed by a signal (`exit_code` is `None`, e.g. OOM) or timed out (exit code 124), the run fails with a diagnostic message containing the exit code and an output snippet. Only judges that exit normally proceed to score parsing; if no structured result is found, `0.0` is recorded instead of failing the run.

Issue #19 remains open until per-task memory limits are implemented (see section 2); once a judge OOM can be attributed to the candidate rather than infrastructure, it should also score 0.0 instead of failing.

---

### 6. Score 0.0 when judge marks result invalid

**File:** `src/legacy_judge.rs`

When the judge emits a structured JSON result with `"valid": false`, the run previously failed with a hard error. Now `parse_score()` records `0.0` and lets the benchmark continue with remaining tasks, treating an explicit invalid verdict as a candidate quality issue rather than an infrastructure failure.

Closes #21.

---

### 7. Retry Docker image pulls with exponential backoff

**Files:** `src/runtime.rs`, `src/workspace.rs`

Extracted a shared `pull_image_with_retry()` helper that retries `docker pull` up to 3 times with exponential backoff (5 s → 10 s → 20 s). Both `resolve_image()` and `materialize_seed()` now call this helper instead of inlining single-shot pull commands that bail on the first transient network failure.

Closes #22.

---

### 8. Skip special files in terminal workspace

**File:** `src/submission.rs`

`collect_terminal_files()` now skips sockets, FIFOs, and device nodes left behind by the candidate's runtime instead of aborting with `"terminal workspace contains a special file"`. Each skipped file is logged to stderr. These entries carry no submission content and are safely ignored.

Closes #23.

---

### Verification

| Check | Result |
|-------|--------|
| `cargo build` | Clean |
| `cargo test` | 92 passed, 0 failed |
| `cargo clippy -- -D warnings` | Clean |
| `cargo fmt --check` | Clean |
| End-to-end on `carleson_formalization` | Completed with score=0.005 (previously crashed at multiple stages) |
| End-to-end on `dcss_dungeon_ai` | Completed with score=0 (previously crashed on special file) |
| Judge phase wall time | ~5–8 min (was 60+ min due to `chmod -R`) |
