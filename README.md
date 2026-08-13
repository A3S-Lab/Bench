<p align="center">
  <img src="assets/readme/hero.svg" width="100%" alt="A3S Bench locks a Task and Candidate, runs them in isolation, and records a Task-owned Judge result">
</p>

<p align="center">
  <strong>Reproducible evaluation for coding agents and automated systems.</strong>
</p>

<p align="center">
  <a href="https://github.com/A3S-Lab/Bench/actions/workflows/ci.yml"><img alt="CI status" src="https://img.shields.io/github/actions/workflow/status/A3S-Lab/Bench/ci.yml?branch=main&amp;style=flat-square&amp;label=CI"></a>
  <a href="https://github.com/A3S-Lab/Bench/releases/latest"><img alt="Latest Bench release" src="https://img.shields.io/github/v/release/A3S-Lab/Bench?display_name=tag&amp;sort=semver&amp;style=flat-square&amp;color=171717"></a>
  <a href="https://www.rust-lang.org/"><img alt="Rust native" src="https://img.shields.io/badge/Rust-native-60646c?style=flat-square"></a>
  <a href="https://opensource.org/license/mit"><img alt="MIT License" src="https://img.shields.io/badge/license-MIT-171717?style=flat-square"></a>
</p>

<p align="center">
  <a href="#run-one-complete-evaluation">Quick start</a> ·
  <a href="#the-evaluation-contract">Evaluation contract</a> ·
  <a href="#candidates">Candidates</a> ·
  <a href="#runtime-providers">Runtimes</a> ·
  <a href="#authoring">Authoring</a> ·
  <a href="#development">Development</a>
</p>

---

A3S Bench is the evaluation control component for A3S. It captures a Task and
Candidate as immutable locks, executes the Candidate in an isolated Runtime,
projects a bounded read-only submission, invokes the Task-owned Judge, validates
its metrics, and stores an identity-bound result.

A Candidate can be an agent, another automated system, or a deterministic tool.
Bench is deliberately **not** an Agent Runtime or a leaderboard: Runtime owns
execution, the Task owns its Judge, and local results remain
`local_unofficial`.

## Run one complete evaluation

The `quick_file_edit` conformance Task exercises locking, Candidate execution,
submission projection, judging, and result persistence in a few seconds.

Requirements:

- a current [`a3s` CLI](https://github.com/A3S-Lab/a3s/releases/latest);
- Docker for the default local Runtime; and
- Rust only when running Bench directly from a source checkout.

```bash
a3s install bench
a3s bench advanced doctor

git clone git@github.com:A3S-Lab/Bench.git
cd Bench
docker build -q -t a3s-bench-smoke-agent:test ./examples/smoke-candidate
a3s bench run quick_file_edit --agent ./examples/smoke-candidate
```

Expected result:

```text
COMPLETED  score=1  task=quick_file_edit
run:    <run-id>
```

Reopen it without reading private state directly:

```bash
a3s bench result
a3s bench result <run-id> --json
```

Compare completed runs in Task-lock-matched pairs. Every baseline result must
identify the same Candidate and model, as must every candidate result; Bench
rejects mixed identities and pairs produced from different Task locks.

```bash
a3s bench compare \
  <baseline-task-a-run> <candidate-task-a-run> \
  <baseline-task-b-run> <candidate-task-b-run>
a3s bench compare <baseline-run> <candidate-run> --json
```

The `a3s.bench.comparison.v1` report records per-Task scores and outcomes plus
aggregate wins, ties, timeouts, and complete-model token totals when available.
Like its source results, the report remains `local_unofficial`.

Run a reproducible two-Candidate suite when the same comparison should cover
multiple Tasks. Bench resolves every Task lock and both Candidate locks before
the first execution, then persists each completed member. A failed or
interrupted suite can continue without rerunning recorded members:

```bash
a3s bench suite run ./examples/coding-suite.acl
a3s bench suite run ./examples/coding-suite.acl --resume <suite-run-id>
```

```acl
bench_suite "coding-core" {
  schema = "a3s-bench/suite/v1"
  tasks  = ["quick_file_edit"]

  candidate "baseline" {
    agent = "a3s-code"
    model = "openai/baseline-model"
  }

  candidate "candidate" {
    agent = "a3s-code"
    model = "openai/candidate-model"
  }
}
```

Resume is bound to the exact suite digest and persisted locks. Editing the
suite, changing either model, reordering Tasks, or replacing a lock fails
closed instead of silently changing the comparison.

Local Docker runs do not require an A3S OS login. From a development checkout,
replace `a3s bench` with `cargo run --`.

## The evaluation contract

<p align="center">
  <img src="assets/readme/evaluation-flow.svg" width="100%" alt="A3S Bench evaluation flow from immutable Task and Candidate locks through isolated execution and submission projection to a Task-owned Judge and validated result">
</p>

Each normal run resolves mutable sources exactly once:

| Owner | What it controls |
| --- | --- |
| **Task** | Prompt, public workspace, hidden bundle, limits, submission policy, metrics, and Judge |
| **Candidate** | One immutable adapter snapshot and, when applicable, one locked model route |
| **Runtime** | Candidate and Judge execution, resource limits, workspace lifecycle, and protected mounts |
| **Judge** | Measurements produced from the read-only SubmissionSnapshot and separate hidden bundle |
| **Bench** | Lock identity, result validation, deterministic score, journal, persistence, and reporting |

The Candidate receives only public inputs. It never receives Judge bytes,
hidden expected data, credentials, or the protected result channel. The Judge
never sees the live Candidate workspace; it receives the policy-filtered
submission read-only.

There is intentionally no `--judge` option. Letting an entrant replace the
Task-owned Judge would change the evaluation identity.

### What ships

| Area | Current capability |
| --- | --- |
| Tasks | 52 locally runnable built-ins: one admitted conformance Task and 51 provisional long-horizon Tasks |
| Candidates | Bundled `a3s-code`, local adapters, Docker-compatible OCI images, generic ORAS artifacts, and CandidateLocks |
| Judges | Task-owned local or OCI Asset Judges plus packaged legacy, game, and model-backed adapters |
| Results | Digest-bound local result, run journal, primary score, public projection, and typed Candidate timeout status |
| Automation | Stable `a3s.bench.output.v1` JSON envelopes for commands that support `--json` |

The imported long-horizon catalog is locally runnable but quarantined for
official admission. Catalog metadata never promotes a local result.

```bash
a3s bench list
a3s bench info quick_file_edit
a3s bench info juliet_vulnerability_analyzer
```

## Candidates

A Candidate adapter is a closed A3S Asset package. Bench does not guess how to
run an arbitrary directory, host executable, or container image.

| Source | Reference |
| --- | --- |
| Bundled model controller | `a3s-code` |
| Native Codex product | `codex` (authenticated Codex CLI required) |
| Local adapter | `./agents/my-agent` |
| Docker-compatible OCI package | `oci://ghcr.io/acme/my-agent@sha256:<digest>` |
| Generic OCI artifact | `oci://registry.example.com/acme/my-agent@sha256:<digest>` |
| Exported lock | `./candidate.lock.json` with `--locked` |

A minimal executable adapter contains an Asset manifest and entrypoint:

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

The entrypoint receives the private workspace path as its first argument.
Local packages reject escaping paths, unsafe links, and special files during
snapshotting. See [Candidate adapter authoring](docs/candidate-adapters.md) for
the executable, model-backed, local, and OCI contracts.

### Model-backed comparisons

`--model` binds an exact configured `provider/model` route into the
CandidateLock. Credentials remain in `.a3s/config.acl`; locks and results record
identity and usage, not provider secrets.

```bash
a3s bench run quick_file_edit \
  --agent a3s-code \
  --model openai/gpt-5.2-codex
```

The `a3s-code` adapter uses A3S Code Core 5.3.4 as a versioned controller.
Varying the model compares models under that same controller. The separate
`codex` adapter runs the native Codex CLI and binds its reported version into
the CandidateLock, enabling complete-product comparisons without presenting a
prompt template as the Codex product.

## Runtime providers

Docker is the signed-out default. An explicit provider in `.a3s/config.acl`
wins, and Bench never silently falls back to a weaker provider.

| Provider | Status | Current scope |
| --- | --- | --- |
| `docker` | Implemented, default | Executable and model Candidates; embedded or OCI workspaces; Asset, legacy, game, and model-backed Judges |
| `os-runtime` | Implemented subset | Deterministic Candidates and Python Asset Judges with embedded public workspaces |
| `a3s-box` | Preflight only | Installation can be detected; benchmark execution is not implemented |

```acl
runtime {
  provider = "os-runtime"
}
```

The current `os-runtime` slice is fail-closed outside its documented subset. It
rejects model-backed Candidates, legacy or game Judges, OCI workspace seeds,
payloads above 64 KiB, and step timeouts above 600 seconds.

Inspect the effective provider without starting a run:

```bash
a3s bench advanced doctor
a3s bench advanced doctor --json
```

## Reproducible runs

Ordinary runs create locks automatically below the current project's private
`.a3s/bench/` state. Export them when a comparison must reuse the exact inputs:

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

A locked run accepts only explicit TaskLock and CandidateLock files, verifies
their semantic digests and captured artifacts, and never re-resolves aliases,
directories, tags, or model choices.

When a Candidate reaches `solution_timeout_sec`, Bench terminates it, preserves
the final projected workspace, and still lets the Judge score it. Ordinary
Judge rejection or an unbuildable submission records score zero. Infrastructure
timeouts, signal kills, malformed structured Judge output, and projection
failures remain errors rather than synthetic scores.

## Authoring

A local Task reference must begin with `./` or `../`. A minimal TaskBundle keeps
public Candidate inputs separate from protected Judge data:

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

```bash
a3s bench advanced check ./my-task
a3s bench info ./my-task
a3s bench run ./my-task --agent ./my-candidate
```

Read the [Task Spec ACL](docs/task-spec-acl.md) for the complete schema and the
[smoke fixture](examples/smoke/README.md) for the smallest end-to-end example.

## CLI reference

```text
a3s bench list [--all] [--json]
a3s bench info <task> [--all] [--json]
a3s bench run <task> --agent <candidate> [--model <provider/model>] [--locked] [--json]
a3s bench result [run-id] [--json]
a3s bench compare <baseline-run> <candidate-run> [<baseline-run> <candidate-run> ...] [--json]
a3s bench suite run <suite.acl> [--resume <suite-run-id>] [--json]

a3s bench advanced check <./task>
a3s bench advanced doctor [--json]
a3s bench advanced task lock <source> --out <file>
a3s bench advanced candidate lock <candidate> [--model <provider/model>] --out <file>
```

The public entrypoint is `a3s bench`. The managed `a3s-bench` executable is a
private component invoked by the top-level CLI.

## Current boundaries

- One run contains one Task, one Candidate execution, one projected submission,
  one Judge execution, and one result.
- Parallel/distributed suites, campaigns, leaderboards, `advanced init`, and
  `advanced cancel` are not implemented.
- All local results are `local_unofficial`; official admission and publication
  remain separate governance work.
- Managed release artifacts currently cover Linux x86_64 and macOS arm64.
  Other targets require a source build.
- Local execution currently requires Docker. `a3s-box` execution and the
  remaining shared Runtime lifecycle are still pending.

## Development

Run checks from the Bench repository, not the A3S monorepo root:

```bash
cargo fmt --all -- --check
cargo test --locked
cargo clippy --locked --all-targets -- -D warnings
python3 tools/check_builtins.py
```

Docker-backed validation is explicit:

```bash
cargo test --locked -- --ignored --nocapture --test-threads=1
./tools/smoke_local.sh
./tools/smoke_imported.sh
```

The test suite covers strict ACL parsing, immutable snapshots, lock and result
identity, Docker and OS Runtime boundaries, timeout recovery, OCI resolution,
submission projection, Judge validation, and the complete built-in catalog.

## Documentation

- [Canonical design](docs/design.md) — architecture, trust model, lifecycle,
  schemas, and roadmap
- [Task Spec ACL](docs/task-spec-acl.md) — Task authoring reference
- [Candidate adapter authoring](docs/candidate-adapters.md) — local and OCI
  Candidate packages
- [Built-in catalog](builtin/README.md) — source provenance and admission state
- [Smoke example](examples/smoke/README.md) — smallest runnable fixture

## License

Licensed under the [MIT License](https://opensource.org/license/mit). Imported sources retain their
upstream licenses; see [third-party notices](builtin/THIRD_PARTY_NOTICES.md).
