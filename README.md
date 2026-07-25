# A3S Bench

<p align="center">
  <strong>Reproducible Evaluation for Coding Agents and Automated Systems</strong>
</p>

<p align="center">
  <em>Lock every input, isolate every run, and let the Task own how its result is judged</em>
</p>

<p align="center">
  <a href="#overview">Overview</a> •
  <a href="#features">Features</a> •
  <a href="#quick-start">Quick Start</a> •
  <a href="#evaluation-model">Evaluation Model</a> •
  <a href="#candidates">Candidates</a> •
  <a href="#runtimes">Runtimes</a> •
  <a href="#development">Development</a>
</p>

---

## Overview

**A3S Bench** is the benchmark control component for A3S. It snapshots a Task
and Candidate into immutable locks, executes the Candidate in an isolated
Runtime, projects the resulting workspace into a read-only submission, invokes
the Task-owned Judge, validates its metrics, and stores an identity-bound
result.

Bench evaluates more than A3S agents. A Candidate can be a coding agent, another
automated system, or a deterministic tool, provided that it is packaged through
the Candidate adapter contract.

Bench is not an Agent Runtime or a leaderboard. The selected Runtime provider
executes the workload, the Task owns its Judge, and the current CLI records
local evaluations as `local_unofficial`.

### Basic usage

```bash
a3s install bench
a3s bench list
a3s bench run quick_file_edit --agent ./my-candidate
a3s bench result
```

Local Docker runs do not require an A3S OS login.

## Features

- **Immutable Inputs**: Snapshot Tasks, Candidates, Judges, work images, and
  model bindings into digest-verified locks
- **Task-Owned Judging**: Keep the Judge fixed by the Task instead of allowing
  entrants to choose a more favorable scorer
- **Product-Neutral Candidates**: Evaluate local adapters, OCI packages,
  deterministic tools, or the bundled `a3s-code` model controller
- **Isolated Execution**: Run the complete local path with Docker and support a
  bounded deterministic subset through A3S OS Runtime
- **Submission Projection**: Give the Judge a policy-filtered, read-only
  snapshot rather than the Candidate's live workspace
- **Validated Results**: Require canonical metric values and bind the result to
  both input locks and the run journal
- **Repeatable Comparisons**: Export `TaskLock` and `CandidateLock` files, then
  rerun without resolving mutable sources
- **Automation Output**: Return a stable `a3s.bench.output.v1` JSON envelope
  from commands that support `--json`

### Capability matrix

| Area | Current capability |
| --- | --- |
| Tasks | 52 locally runnable built-ins, local TaskBundle directories, and exported TaskLocks |
| Candidates | Bundled `a3s-code`, local adapter directories, Docker-compatible OCI images, generic ORAS artifacts, and CandidateLocks |
| Judges | Task-owned local or OCI Asset Judges plus the packaged legacy, game, and model-backed adapters used by built-ins |
| Runtime | Docker by default; limited `os-runtime`; `a3s-box` discovery and preflight only |
| Results | Digest-bound local result, run journal, primary score, public projection, and Candidate timeout status |
| Governance | Local runs are `local_unofficial`; catalog admission metadata does not promote a local result |

## Quick Start

### Requirements

- A current `a3s` CLI
- Docker for the default local Runtime
- Rust only when running Bench directly from a repository checkout
- ORAS only when resolving a generic, non-Docker OCI artifact
- A configured provider only for a model-backed Candidate or Judge

Install the managed Bench component:

```bash
a3s install bench
a3s bench advanced doctor
```

### Run the smoke Task

The built-in `quick_file_edit` Task exercises Task locking, Candidate execution,
submission projection, judging, and result persistence in a few seconds.

```bash
git clone git@github.com:A3S-Lab/Bench.git
cd Bench

docker build -q -t a3s-bench-smoke-agent:test ./examples/smoke-candidate
a3s bench run quick_file_edit --agent ./examples/smoke-candidate
```

Expected output:

```text
COMPLETED  score=1  task=quick_file_edit
```

From a development checkout, the equivalent command is:

```bash
cargo run -- run quick_file_edit --agent ./examples/smoke-candidate
```

Use `a3s bench result` to reopen the latest result or
`a3s bench result <run-id>` to inspect a specific run.

## Evaluation Model

A normal run resolves mutable inputs once and records their immutable identity:

```text
Task source      → Task snapshot      → TaskLock + task-owned Judge
Candidate source → Candidate snapshot → CandidateLock
TaskLock + CandidateLock
        ↓
isolated Candidate execution
        ↓
read-only SubmissionSnapshot
        ↓
task-owned Judge → validated metrics → identity-bound local result
```

The Task defines the prompt, workspace, execution class, resource limits,
submission policy, metrics, and Judge. There is intentionally no `--judge`
option.

The current built-in catalog contains:

| Class | Count | Purpose |
| --- | ---: | --- |
| Conformance | 1 | Fast end-to-end installation and Runtime check |
| Long horizon | 51 | Provisional imported software, data, optimization, simulation, and game Tasks |

All 52 entries are locally available by bare ID. The 51 imported Tasks are
quarantined for official admission, but can still produce local unofficial
results. Inspect a Task before running it:

```bash
a3s bench list
a3s bench info quick_file_edit
a3s bench info juliet_vulnerability_analyzer
a3s bench info college_english_exam_bank
```

`list --all` and `info <id> --all` also expose catalog entries that a future
release may ship as locally blocked.

### Local Tasks

A local Task reference must begin with `./` or `../`:

```bash
a3s bench advanced check ./my-task
a3s bench info ./my-task
a3s bench run ./my-task --agent ./my-candidate
```

A minimal TaskBundle follows this shape:

```text
my-task/
├── task.acl
├── public/
│   ├── prompt.md
│   └── workspace/
└── private/
    ├── bundle/
    └── judge/
        ├── .a3s/asset.acl
        ├── agent.md
        └── judge.py
```

Candidates receive public inputs only. The Judge receives the projected
submission and its protected private bundle through separate read-only paths.
See [Task Spec ACL](docs/task-spec-acl.md) for the complete schema.

## Candidates

A Candidate adapter is a closed A3S Asset package. Bench does not guess how to
run an arbitrary directory, host executable, or container image.

| Source | Reference |
| --- | --- |
| Bundled model controller | `a3s-code` |
| Local adapter | `./agents/my-agent` |
| Docker-compatible OCI package | `oci://ghcr.io/acme/my-agent@sha256:<digest>` |
| Generic OCI artifact | `oci://registry.example.com/acme/my-agent@sha256:<digest>` |
| Exported lock | `./candidate.lock.json` with `--locked` |

A minimal executable adapter contains:

```text
my-agent/
├── .a3s/
│   └── asset.acl
└── run.sh
```

```acl
version = "a3s.asset.v1"
category = "agent"
kind = "tool"
name = "my-agent"

source {
  package_path = "."
  entrypoint   = "run.sh"
}
```

An executable Candidate entrypoint receives the private workspace path as its
first argument. Asset paths must be package-relative, and local packages reject
symlinks, hard links, and special files during snapshotting.

See [Candidate adapter authoring](docs/candidate-adapters.md) for executable,
model-backed, local, and OCI examples.

### Model-backed Candidates

`--model` binds a configured `provider/model` route to the CandidateLock.
Credentials stay in `.a3s/config.acl`; locks and results contain model identity
and usage, not provider secrets.

```acl
providers "openai" {
  api_key  = "..."
  base_url = "https://api.openai.com/v1"

  models "gpt-5.2-codex" {
    name = "GPT-5.2 Codex"
  }
}
```

```bash
a3s bench run quick_file_edit \
  --agent a3s-code \
  --model openai/gpt-5.2-codex
```

The bundled `a3s-code` adapter uses the versioned A3S Code Core 5.3.4
controller with automatic planning, continuation, and manual delegation. It is
not the interactive A3S Code CLI or TUI.

Using one `a3s-code` adapter with different model bindings compares models under
the same controller. Comparing complete Codex, Claude Code, and A3S Code
products requires one separately packaged native Candidate adapter per product;
Bench does not currently bundle native `codex` or `claude` aliases.

### Model-backed Judges

The small number of Tasks that require a model Judge read a separate route:

```acl
bench {
  judge_model = "openai/my-judge-model"
}
```

The route is bound into the TaskLock. It does not change which Judge the Task
owns.

## Runtimes

Docker is the signed-out default. Select another implemented provider explicitly
in `.a3s/config.acl`:

```acl
runtime {
  provider = "os-runtime"
}
```

Bench never silently falls back to Docker when an explicit provider is missing
or unsupported.

| Provider | Status | Current scope |
| --- | --- | --- |
| `docker` | Implemented, default | Executable and model Candidates; embedded or OCI workspaces; Asset, legacy, game, and model-backed Judges |
| `os-runtime` | Implemented subset | Deterministic Candidates and Python Asset Judges with embedded `public/workspace` |
| `a3s-box` | Preflight only | Installation can be detected, but benchmark execution is not implemented |

The current `os-runtime` slice rejects model-backed Candidates, legacy or game
Judges, OCI workspace seeds, payload envelopes larger than 64 KiB, and step
timeouts over 600 seconds. It reads the active session from
`~/.a3s/os-auth.json`; automation can set `A3S_OS_ADDRESS` and
`A3S_OS_ACCESS_TOKEN` together. Managed runner overrides must remain
digest-pinned; the supported variables are `A3S_BENCH_OS_NODE_IMAGE` and
`A3S_BENCH_OS_PYTHON_IMAGE`.

Check the selected provider without starting a run:

```bash
a3s bench advanced doctor
a3s bench advanced doctor --json
```

## Reproducible Runs

Ordinary runs create Task and Candidate locks automatically under the current
project's `.a3s/bench/` state. Export locks when a comparison must reuse the
exact same inputs:

```bash
a3s bench advanced task lock quick_file_edit \
  --out ./task.lock.json

a3s bench advanced candidate lock a3s-code \
  --model openai/gpt-5.2-codex \
  --out ./candidate.lock.json

a3s bench run ./task.lock.json \
  --agent ./candidate.lock.json \
  --locked
```

A locked run:

- accepts explicit TaskLock and CandidateLock files only;
- does not re-resolve aliases, directories, tags, or model choices;
- verifies semantic digests and captured artifacts;
- requires referenced artifacts to remain available in local Bench state.

Mutable OCI tags can be used while creating a lock. Lock creation records the
resolved manifest and canonical package snapshot; locked execution does not
follow the tag again.

### Results and timeouts

Project state is private implementation data:

```text
<project>/.a3s/bench/
├── artifacts/
├── assets/
├── locks/
├── runs/
├── workspaces/
├── submissions/
├── runtime-assets/
└── results/
```

Use the CLI rather than reading this layout directly:

```bash
a3s bench result
a3s bench result <run-id>
a3s bench result <run-id> --json
```

When a Candidate reaches `solution_timeout_sec`, Bench terminates it, preserves
the final projected workspace, and still runs the Judge. The result remains
scoreable and records:

```json
{
  "candidate_execution": {
    "status": "timed_out",
    "timeout_sec": 600
  }
}
```

Initialization, configuration, process, and workspace-projection errors remain
run failures. They are not converted into Candidate timeouts or scores.

## CLI Reference

```text
a3s bench list [--all] [--json]
a3s bench info <task> [--all] [--json]
a3s bench run <task> --agent <candidate> [--model <provider/model>] [--locked] [--json]
a3s bench result [run-id] [--json]

a3s bench advanced check <./task>
a3s bench advanced doctor [--json]
a3s bench advanced task lock <source> --out <file>
a3s bench advanced candidate lock <candidate> [--model <provider/model>] --out <file>
```

The public entrypoint is `a3s bench`. The managed `a3s-bench` executable is a
private component invoked by the top-level CLI.

Commands with `--json` emit one closed envelope:

```json
{
  "schema": "a3s.bench.output.v1",
  "command": "list",
  "ok": true,
  "data": {}
}
```

An error replaces `data` with `error`.

## Current Boundaries

Version 0.1 implements one Task and one Candidate per run. It does not yet
provide suites, campaigns, leaderboards, distributed scheduling,
`advanced init`, or `advanced cancel`.

`a3s-box` execution and the remaining shared Runtime lifecycle are still
pending. The limited `os-runtime` path is intentionally fail-closed outside its
supported subset. The 51 imported long-horizon Tasks are useful for local
evaluation but remain provisional and quarantined for official admission.

## Development

Run checks from the `a3s-bench` repository, not the monorepo root:

```bash
cargo fmt --all -- --check
cargo test --locked
cargo clippy --locked --all-targets -- -D warnings
python3 tools/check_builtins.py

./tools/smoke_local.sh
./tools/smoke_imported.sh
```

The test suite covers ACL validation, immutable snapshotting, lock and result
identity, Docker and OS Runtime boundaries, timeout recovery, OCI resolution,
submission projection, Judge validation, and the complete built-in catalog.

## Documentation

- [Canonical design](docs/design.md) — architecture, trust model, lifecycle,
  schemas, and roadmap
- [Task Spec ACL](docs/task-spec-acl.md) — Task authoring reference
- [Candidate adapter authoring](docs/candidate-adapters.md) — local and OCI
  Candidate packages
- [Built-in catalog](builtin/README.md) — sources, provenance, and admission
  state
- [Smoke example](examples/smoke/README.md) — smallest runnable fixture

## License

MIT. Imported sources retain their upstream licenses; see
[Third-party notices](builtin/THIRD_PARTY_NOTICES.md).
