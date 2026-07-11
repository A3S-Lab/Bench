# a3s-bench

`a3s-bench` runs reproducible evaluations of coding agents, automated systems,
and deterministic tools. Each run combines an immutable Task, a packaged
Candidate adapter, and the task-owned Judge, then stores an identity-bound
result.

```bash
a3s bench run <task> --agent <candidate>
```

Bench works locally without signing in to A3S OS. Local provider/model routes
come from `.a3s/config.acl`, signed-out execution defaults to Docker, and both
Candidates and Judges can be loaded from local directories or any
OCI Distribution-compatible registry.

> [!NOTE]
> A stable Bench component does not make every evaluation official. Local runs
> produce `local_unofficial` results; official evaluation additionally requires
> signed component and Task admission.

## Quick start

The smallest built-in Task, `quick_file_edit`, verifies the complete execution
and judging path in seconds:

```bash
git clone git@github.com:A3S-Lab/Bench.git
cd Bench

docker build -q -t a3s-bench-smoke-agent:test ./examples/smoke-candidate
cargo run -- run quick_file_edit --agent ./examples/smoke-candidate
```

Expected output:

```text
COMPLETED  score=1  task=quick_file_edit
```

With an installed component, use the same workflow as:

```bash
a3s bench run quick_file_edit --agent <candidate>
a3s bench result
```

Docker is required for the current local execution path. An A3S OS account is
not.

## Choose a Task

```bash
# List locally runnable built-ins.
a3s bench list

# Inspect execution class, requirements, and metrics.
a3s bench info quick_file_edit
a3s bench info juliet_vulnerability_analyzer

# Inspect or validate a local TaskBundle.
a3s bench info ./my-task
a3s bench advanced check ./my-task
```

Bench includes one short conformance Task and 51 provisional, imported
long-horizon Tasks. The long-horizon set is not a fixed catalog or product
boundary: Tasks can be added, revised, replaced, or removed. All currently
packaged Tasks are locally available by bare ID.

Use `quick_file_edit` for installation checks. Before running a long-horizon
Task, inspect its `execution_class`, resource needs, and Judge requirements.
`college_english_exam_bank`, for example, requires a configured model Judge.

## Choose or add a Candidate

A Candidate is accessed through a packaged adapter. It can represent any
coding agent, automated system, or deterministic tool; it does not need to be
an A3S-native agent.

| Source | Example |
| --- | --- |
| Bundled adapter | `a3s-code` |
| Local adapter directory | `./agents/my-agent` |
| OCI adapter | `oci://ghcr.io/example/my-agent@sha256:<digest>` |
| Exported immutable lock | `./candidate.lock.json` with `--locked` |

Local and OCI adapters use the same closed package contract and contain
`.a3s/asset.acl`. Bench does not guess how to run an arbitrary directory,
container image, or host executable.

To add an agent:

1. Create an adapter directory with `.a3s/asset.acl`, its entrypoint, and any
   controller instructions.
2. Validate it by running `quick_file_edit`.
3. Publish the directory as an OCI artifact if it should be shared.
4. Use a digest-pinned OCI reference for repeatable comparisons.

See [Candidate adapter authoring](docs/candidate-adapters.md) for the package
schema and complete local/OCI workflow.

## Configure models without A3S OS login

Bench resolves custom providers and models from the standard project-local or
user-local `.a3s/config.acl`:

```acl
providers "openai" {
  api_key  = "..."
  base_url = "https://api.openai.com/v1"

  models "gpt-5.2-codex" {
    name = "GPT-5.2 Codex"
  }
}

providers "anthropic" {
  api_key  = "..."
  base_url = "https://api.anthropic.com"

  models "claude-opus-4-6" {
    name = "Claude Opus 4.6"
  }
}
```

Use any supported provider name, model name, or custom compatible endpoint.
Credentials remain in local configuration: locks and results contain model
identity and usage, never API keys or provider credentials. Bench deliberately
does not inherit an ambient `default_model`; the benchmark input must bind the
model explicitly.

### Compare models with the same controller

`a3s-code` is the bundled model-backed Candidate adapter. Freeze one Task and
bind the same controller to each model:

```bash
a3s bench advanced task lock quick_file_edit --out ./task.lock.json

a3s bench advanced candidate lock a3s-code \
  --model openai/gpt-5.2-codex \
  --out ./a3s-code-openai.candidate.lock.json

a3s bench advanced candidate lock a3s-code \
  --model anthropic/claude-opus-4-6 \
  --out ./a3s-code-claude.candidate.lock.json

a3s bench run ./task.lock.json \
  --agent ./a3s-code-openai.candidate.lock.json --locked

a3s bench run ./task.lock.json \
  --agent ./a3s-code-claude.candidate.lock.json --locked
```

This compares models under the same A3S Code controller. It does **not** run or
compare the native Codex and Claude Code products.

### Compare Codex, Claude Code, and A3S Code

A product comparison requires one native Candidate adapter per product and
model combination. Freeze product version, controller behavior, tools, and
model in that adapter's identity, then run every CandidateLock against the same
TaskLock:

```bash
a3s bench advanced task lock <task-id> --out ./task.lock.json

a3s bench advanced candidate lock ./agents/codex-gpt-5.2-codex \
  --out ./codex.candidate.lock.json

a3s bench advanced candidate lock \
  oci://registry.example.com/agents/claude-code-opus-4-6@sha256:<digest> \
  --out ./claude-code.candidate.lock.json

a3s bench advanced candidate lock a3s-code \
  --model openai/gpt-5.2-codex \
  --out ./a3s-code.candidate.lock.json

for candidate in codex claude-code a3s-code; do
  a3s bench run ./task.lock.json \
    --agent "./${candidate}.candidate.lock.json" --locked
done
```

The Codex and Claude Code paths above are examples of adapters you supply; the
current release does not bundle native adapters or bare `codex` and `claude`
aliases. Do not add `--model` to those native locks unless their adapter
contract explicitly supports it. In the current release, `--model` selects the
generic A3S Code model controller.

## Task-owned Judges

The Task selects its Judge, so there is intentionally no `--judge` option. A
replaceable Judge would make results for the same Task incomparable.

Judge adapters use the same source forms as Candidate adapters:

- a local adapter directory inside a TaskBundle;
- a Docker-compatible OCI image;
- a generic OCI artifact from Docker Hub, GHCR, Harbor, ECR, ACR, or another
  OCI Distribution-compatible registry.

Docker-compatible images are inspected and extracted through Docker. Other OCI
media types are pulled with [ORAS](https://oras.land/), which treats arbitrary
files as content-addressed OCI artifacts. The `oras` executable is needed only
for that generic-artifact path. Registry credentials stay with Docker or ORAS
and are never copied into a TaskLock or result.

For a model-backed Judge, configure its route separately:

```acl
bench {
  judge_model = "openai/my-judge-model"
}
```

Only Tasks whose Judge declares a model gateway use this setting. The resolved
Judge model identity is sealed into the TaskLock and result.

## Runtime selection

With no authenticated A3S OS policy or explicit operator setting, Bench chooses
Docker. Override the Runtime provider in `.a3s/config.acl`:

```acl
runtime {
  provider = "a3s-box"
}
```

Selection order is explicit operator configuration, authenticated session
policy, then the signed-out Docker default. If an explicitly selected provider
is unavailable, Bench fails instead of silently changing the execution
environment.

```bash
a3s bench advanced doctor
a3s bench advanced doctor --json
```

The current release executes with Docker. `a3s-box` selection and preflight are
implemented, but execution awaits completion of the shared Runtime lifecycle
migration. The architecture accepts other conforming Runtime providers through
the same platform registry rather than adding provider-specific Bench flags.

## Reproducibility model

Every ordinary run snapshots mutable inputs and creates canonical Task and
Candidate locks. A `--locked` run consumes exported lock files and performs no
source re-resolution.

```text
Task source      -> TaskLock (Task + task-owned Judge + work images)
Candidate source -> CandidateLock (adapter + optional model binding)
TaskLock + CandidateLock -> isolated run -> SubmissionSnapshot
SubmissionSnapshot -> JudgeResult -> identity-bound durable result
```

The lock/result chain detects changed sources, preserves executable file
semantics, resolves mutable OCI selectors to content-addressed snapshots, and
binds the complete Judge result and execution evidence to both input locks.
Local digests establish integrity; they do not grant official admission.

`--locked` accepts only explicit TaskLock and CandidateLock files. It rejects
mutable sources, aliases, and OCI selectors, and requires every artifact to
already exist locally.

## Author a Task

Start from [examples/smoke](examples/smoke/):

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

```bash
a3s bench advanced check ./my-task
a3s bench run ./my-task --agent ./my-candidate
```

Candidates see public inputs only. The Judge receives a policy-projected,
read-only `SubmissionSnapshot`, not the Candidate workspace or hidden Task
data. See [Task Spec ACL](docs/task-spec-acl.md) for schemas, defaults, metric
contracts, limits, and import rules.

## CLI reference

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

`--json` returns one `a3s.bench.output.v1` object with `command`, `ok`, and
exactly one of `data` or `error`.

Runs store private state below the current project's `.a3s/bench/`. That layout
is not a public API; use `a3s bench result [run-id]` to inspect results.

## Development

Run checks from this crate, not the monorepo root:

```bash
cargo fmt --all -- --check
cargo test --locked -p a3s-bench
cargo clippy --locked -p a3s-bench --all-targets -- -D warnings
python3 tools/check_builtins.py

./tools/smoke_local.sh
./tools/smoke_imported.sh
```

Version 0.1 establishes the stable local CLI and lock/result formats implemented
by this release. `advanced init` and `advanced cancel` are specified but not
implemented, `a3s-box` execution is pending, and the shared Runtime lifecycle
migration is incomplete. The packaged 51-task catalog is locally runnable, but
catalog consistency is not full execution evidence or official admission.

## Documentation

- [Canonical design](docs/design.md) — architecture, trust model, lifecycle,
  schemas, and roadmap
- [Task Spec ACL](docs/task-spec-acl.md) — Task authoring reference
- [Candidate adapter authoring](docs/candidate-adapters.md) — local and OCI
  Candidate packages
- [Built-in catalog](builtin/README.md) — sources, provenance, and admission
  requirements
- [Smoke example](examples/smoke/README.md) — smallest runnable fixture

When this README and the canonical design disagree, the canonical design wins.

## License

MIT. Imported sources retain their upstream licenses; see
[THIRD_PARTY_NOTICES.md](builtin/THIRD_PARTY_NOTICES.md).
