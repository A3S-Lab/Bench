# a3s-bench

`a3s-bench` evaluates an A3S Agent against a reproducible Task and its
task-owned Judge. It freezes every mutable input before execution, runs the
Candidate in an isolated workspace, gives the Judge only the projected
submission, and stores an identity-bound result.

The intended command is:

```bash
a3s bench run <task> --agent <agent>
```

This repository contains the Bench control component, its protocol design,
authoring examples, tests, and a provisional imported benchmark catalog. The
current snapshot contains 51 sources; that number is not a product boundary or
permanent task set.

## Current status

The project is at preview maturity. Preview artifacts are intended for local
development and contract validation; they are not signed production Bench
components and cannot make an evaluation official.

| Area | Current state |
| --- | --- |
| Local Task and Agent Assets | Runnable without an A3S OS login |
| Docker Runtime | Default for signed-out local runs and covered by smoke tests |
| Local `.a3s/config.acl` models | Supported without an A3S OS login |
| OCI Agent and Judge Assets | Docker-compatible images and generic ORAS artifacts supported |
| `a3s-box` selection | Parsed and preflighted; execution is not implemented yet |
| Shared Runtime lifecycle | Contract, registry, and durable operation primitives exist; Bench migration is incomplete |
| Built-in catalog | Current 51-source snapshot validates structurally; entries remain provisional and quarantined pending per-revision evidence |
| Published component | `v0.1.0-preview.1` prerelease through GitHub Actions |

Quarantined built-ins are not presented as runnable or official. A local Task
may run as `local_unofficial`.

## Preview releases

Git tags matching the Cargo package version trigger the release workflow. It
validates formatting, the current built-in snapshot, unit and Docker smoke
tests, and Clippy before producing native component archives for Linux and
macOS. Each archive contains the binary, component manifest, and provisional
built-in catalog together with a SHA-256 checksum.

`v0.1.0-preview.1` is deliberately a GitHub prerelease. It is not accompanied
by the A3S-signed component release statement or signed Task admissions required
by the production design. All results remain `local_unofficial`.

## Quick start

Prerequisites:

- Rust toolchain
- a running Docker Engine
- this repository checkout

From the repository root:

```bash
docker build -q -t a3s-bench-smoke-agent:test ./examples/smoke-candidate
docker build -q -t a3s-bench-smoke-judge:test ./examples/smoke/private/judge

cargo run -- run ./examples/smoke \
  --agent ./examples/smoke-candidate
```

Expected result:

```text
COMPLETED  score=1  task=smoke_answer
```

Show the most recent result again:

```bash
cargo run -- result
```

Use `--json` with `list`, `info`, `run`, `result`, or `advanced doctor` for
machine-readable output.

## What a run does

```text
Task source       -> stable TaskSourceSnapshot -> TaskLock
Candidate source  -> immutable Agent snapshot  -> CandidateLock
TaskLock + CandidateLock                        -> one run
Candidate workspace -> SubmissionSnapshot       -> task-owned Judge
JudgeResult + input locks                       -> durable result
```

Before execution, every ordinary run creates canonical Task and Candidate
locks. An explicit `--locked` run loads those locks instead. Both paths then use
the same verified inputs.

The lock and result chain provides:

- stable source capture with `source_changed` detection;
- immutable Task, Candidate, Judge, and container-image identities;
- canonical lock digests covering semantic fields;
- an offline locked path that does not re-resolve an OCI Judge;
- a durable run journal binding both input locks;
- a result digest binding the complete Judge result and execution evidence;
- cross-validation of result digest, journal, path, and input locks on reload.

These local digests provide integrity, not official admission authority.

## Core concepts

| Concept | Meaning |
| --- | --- |
| TaskBundle | Task ACL, public prompt/workspace, hidden Judge inputs, and the Judge selector |
| Candidate | The Agent Asset being evaluated |
| Judge | An Agent Asset selected by the Task, never by the benchmark user |
| TaskLock | Immutable Task snapshot, Judge snapshot, and resolved work images |
| CandidateLock | Immutable Candidate snapshot and optional model binding |
| SubmissionSnapshot | Locked-policy projection of the terminal Candidate workspace |
| JudgeResult | Typed metrics returned by the Judge |
| Local result | Identity-bound `local_unofficial` record stored under `.a3s/bench` |

There is no `--judge` option. Allowing the user to replace the evaluator would
make scores incomparable.

## Commands

The implemented development CLI is:

```text
a3s bench list [--all] [--json]
a3s bench info <task> [--all] [--json]
a3s bench run <task> --agent <asset> [--model <provider/model>] [--locked] [--json]
a3s bench result [run-id] [--json]

a3s bench advanced check <./task>
a3s bench advanced doctor [--json]
a3s bench advanced task lock <source> --out <file>
a3s bench advanced candidate lock <asset> [--model <provider/model>] --out <file>
```

The canonical design also specifies `advanced init` and `advanced cancel`.
They are not implemented in the current development binary. Cancellation will
not be exposed until it can cancel a real shared-Runtime operation and reach a
durable terminal state.

### List and inspect tasks

```bash
# Admitted runnable built-ins only; currently empty.
cargo run -- list

# Include quarantined imported sources.
cargo run -- list --all
cargo run -- info juliet_vulnerability_analyzer --all

# Inspect a local TaskBundle.
cargo run -- info ./examples/smoke
```

Bare task IDs resolve only admitted built-ins. Local references must begin with
`./` or `../` so a directory cannot silently shadow a built-in.

### Export and use locks

```bash
tmp=$(mktemp -d)

cargo run -- advanced task lock ./examples/smoke \
  --out "$tmp/task.lock.json"

cargo run -- advanced candidate lock ./examples/smoke-candidate \
  --out "$tmp/candidate.lock.json"

cargo run -- run "$tmp/task.lock.json" \
  --agent "$tmp/candidate.lock.json" \
  --locked
```

`--locked` accepts only explicit TaskLock and CandidateLock files. It rejects
paths to mutable Task/Agent sources, aliases, and OCI selectors. Every required
artifact must already exist locally.

## Agent and Judge sources

The development resolver supports:

| Source | Example |
| --- | --- |
| Local Agent Asset directory | `./examples/smoke-candidate` |
| OCI Agent Asset | `oci://registry.example.com/team/agent:tag` |
| Exported CandidateLock | `./candidate.lock.json` with `--locked` |

A local Agent Asset must contain `.a3s/asset.acl`. An OCI artifact must contain
the same closed A3S Agent Asset package; Bench does not infer an Agent from an
arbitrary image.

For OCI sources:

- Docker-compatible images are inspected and extracted through Docker;
- other OCI artifact/media types are resolved and pulled with ORAS;
- Docker Hub, GHCR, Harbor, ECR, ACR, and other OCI Distribution-compatible
  registries are not special-cased;
- mutable selectors are resolved once and converted to content-addressed
  snapshots;
- generic artifacts require the `oras` executable only when that path is used;
- registry credentials remain owned by Docker or ORAS and never enter a lock,
  workspace, or result.

Candidate and task-owned Judge Assets use the same resolver.

To add another Candidate, package its entrypoint and optional model-controller
definition as an Agent Asset. See [Agent Asset authoring](docs/agent-assets.md)
for the complete local, OCI, and CandidateLock workflow and the current boundary
between generic model-backed Candidates and native Codex/Claude Code adapters.

## Running with a custom model

Bench can use provider/model definitions from the normal local
`.a3s/config.acl`. This does not require an A3S OS login.

Example:

```acl
providers "openai" {
  api_key  = "..."
  base_url = "https://api.example.com/v1"

  models "my-model" {
    name = "My model"
  }
}
```

Run with an explicit model:

```bash
cargo run -- run ./examples/smoke \
  --agent ./path/to/model-agent \
  --model openai/my-model
```

Rules:

- the model must be bound by the CandidateLock or supplied with `--model`;
- Bench never silently inherits `default_model` as benchmark input;
- `--model` cannot alter an exported CandidateLock;
- provider credentials and base URLs are not copied into locks or containers;
- results store model identity and usage, not credentials.

## Comparing Codex and Claude Code

Bench compares immutable Agent Assets, not brand names or bare executables. A
Codex or Claude Code integration should therefore be a small standard Agent
Asset adapter that freezes:

- the coding-agent product and adapter version;
- its non-interactive entrypoint and controller instructions;
- allowed tools and workspace contract;
- the exact model binding or fill-only model slot;
- Runtime and network requirements.

The target user experience is deliberately simple:

```bash
a3s bench run ./task.lock.json --agent ./codex.candidate.lock.json --locked
a3s bench run ./task.lock.json --agent ./claude.candidate.lock.json --locked
```

Both runs use the same TaskLock and Judge, while each CandidateLock records the
agent adapter and model. This produces two independent result IDs whose scores
and evidence can be compared without introducing a special benchmark mode.

Today, `codex` and `claude` bare aliases are not implemented by this development
binary. Use local or OCI Agent Asset adapters and lock them explicitly:

```bash
cargo run -- advanced task lock ./my_task --out ./task.lock.json
cargo run -- advanced candidate lock ./agents/codex \
  --model openai/my-codex-model --out ./codex.candidate.lock.json
cargo run -- advanced candidate lock oci://registry.example.com/agents/claude-code:1 \
  --model anthropic/my-claude-model --out ./claude.candidate.lock.json
```

For a model-only comparison, use the same Agent Asset adapter for both
CandidateLocks and change only `--model`. For a full coding-agent comparison,
use distinct Codex and Claude Code adapters; otherwise the experiment compares
models under one controller rather than the two products.

Future signed Bench components may provide `codex` and `claude` as embedded
selectors resolving to pinned adapter snapshots. The aliases themselves will
never be identity, and `--locked` will continue to require explicit locks.

## Runtime selection

Without an explicit provider or authenticated Runtime policy, a signed-out run
selects Docker.

Override the provider in `.a3s/config.acl`:

```acl
runtime {
  provider = "a3s-box"
}
```

Selection precedence is:

1. explicit operator configuration;
2. authenticated session policy;
3. signed-out Docker default.

An unavailable explicit provider fails. It never silently falls back to
Docker. The current Bench development execution path supports Docker only;
`a3s-box` currently passes selection/preflight and then reports that execution
is not implemented.

Check readiness:

```bash
cargo run -- advanced doctor
cargo run -- advanced doctor --json
```

## Authoring a Task

Start from [examples/smoke](examples/smoke/). Its structure is:

```text
task.acl
public/
  prompt.md
  workspace/
private/
  bundle/
  judge/
    .a3s/asset.acl
    agent.md
    judge.py
```

Validate a local descriptor without running it:

```bash
cargo run -- advanced check ./examples/smoke
```

Task ACL syntax, defaults, bounds, Judge contracts, submission projection, and
import rules are documented in [Task Spec ACL](docs/task-spec-acl.md).

Important invariants:

- public inputs are visible to the Candidate;
- the hidden bundle and Judge package are not;
- the Judge receives the SubmissionSnapshot, not the full Candidate workspace;
- `.a3s/bench` is always excluded from submissions;
- symlinks, hard links, special files, unsafe paths, and case collisions are
  rejected during source/snapshot processing.

## Built-in tasks

The current repository snapshot contains 51 provisional imported Task/Judge
descriptors under [`builtin/tasks`](builtin/tasks). That number is not a product
boundary: tasks may be added, removed, replaced, or revised. The catalog-wide
test proves only that the current snapshot is internally consistent:

- catalog IDs, paths, and metadata agree;
- every Task ACL parses under the closed schema;
- every Judge descriptor uses a supported protocol;
- required Judge platforms are pinned;
- the catalog exactly covers the packaged task directories.

This is not admission or full execution evidence. Admission is per Task
revision, so an unavailable provisional entry does not block unrelated admitted
tasks. In the current snapshot every imported entry remains quarantined, and
`list` intentionally returns no runnable built-in until signed admission and
end-to-end evidence exist. Provenance and licensing information are in
[builtin/README.md](builtin/README.md) and
[THIRD_PARTY_NOTICES.md](builtin/THIRD_PARTY_NOTICES.md).

## Project state

Runs create owner-only state under the current project:

```text
.a3s/bench/
  artifacts/       content-addressed Task, Candidate, and Judge snapshots
  assets/          OCI Agent Asset cache
  locks/           internal TaskLock and CandidateLock files
  runs/            durable run journals
  workspaces/      private Candidate workspaces
  submissions/     read-only projected submissions
  results/         completed local result records
```

Do not treat these paths as a public API. Use `result [run-id]` to inspect a
run.

## Development

Run formatting, all tests, and linting from this crate:

```bash
cargo fmt --all -- --check
CARGO_INCREMENTAL=0 cargo test -- --include-ignored
CARGO_INCREMENTAL=0 cargo clippy --all-targets -- -D warnings
```

Docker integration smoke tests:

```bash
./tools/smoke_local.sh
./tools/smoke_imported.sh
```

Other useful checks:

```bash
python3 tools/check_builtins.py
python3 tools/package_component.py
git diff --check
```

`smoke_local.sh` covers local, OCI, locked Agent Assets, and an offline locked
OCI Judge. `smoke_imported.sh` executes the imported Juliet work/Judge path as
`local_unofficial`.

## Documentation

- [Canonical design](docs/design.md) — normative P1 architecture, trust model,
  lifecycle, schemas, and roadmap
- [Task Spec ACL](docs/task-spec-acl.md) — Task authoring reference
- [Agent Asset authoring](docs/agent-assets.md) — add local or OCI Candidates
- [Built-in catalog](builtin/README.md) — imported sources, quarantine, and
  admission requirements
- [Smoke example](examples/smoke/README.md) — smallest runnable fixture

When this README and the canonical design disagree, the canonical design wins.

## License

MIT. Imported sources retain their upstream licenses; see
[THIRD_PARTY_NOTICES.md](builtin/THIRD_PARTY_NOTICES.md).
