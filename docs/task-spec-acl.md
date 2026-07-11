# Task ACL Quick Reference

This is the task-author reference for the single canonical a3s-bench design.
The normative identity, evaluation, and security rules are in
[design.md](design.md).

Benchmarking is a top-level domain of the installed A3S CLI. It reuses the
existing A3S OS login, Agent Asset resolver, and Runtime rather than creating a
separate executable, account state, or Agent Runtime.

Run an authored task directly with the normal four-command interface:

~~~bash
a3s bench run ./my_task --agent codex
~~~

`run` performs portable validation and Runtime capability preflight
automatically, then prints the final score and report path. Author-only
operations live under `a3s bench advanced`; for example,
`a3s bench advanced init my_task` scaffolds a bundle and
`a3s bench advanced check ./my_task` validates its captured local authoring
closure without network, credential lookup, external resolution, project-state
write, or execution. Remote references are syntax-checked and resolved only by
`advanced task lock` or `run`. Generated locks, plans, results, and evidence
always live under the current A3S project root's `.a3s/bench/`; authored files
are not rewritten.

See the [minimal smoke task](../examples/smoke/README.md) for a complete public
workspace, one local A3S Judge Agent Asset, one terminal hidden bundle, and the
protected result contract.

The [global builtin catalog](../builtin/README.md) shows how third-party Task
and Judge sources are discovered without creating another benchmark product or
granting unreviewed code execution authority.

## Task References

The canonical task reference forms are:

| Form | Meaning |
| --- | --- |
| `<task-id>` | Exact admitted task in the global built-in catalog. |
| `./path/to/task` | Local TaskBundle directory containing `task.acl`. |
| `./path/to/task/task.acl` | Local Task ACL. |
| `./path/to/task/task.lock.json` | Exported immutable TaskLock. |
| `oci://registry.example/tasks/name@sha256:<digest>` | Advanced immutable published TaskBundle. |

A bare ID searches admitted built-ins only. It never resolves a local path, an
A3S OS record, a source-specific namespace, or a quarantined catalog entry.
Local references must begin with `./` or `../`, so a local directory cannot
shadow a built-in. Published TaskBundles require the explicit `oci://` form and
an immutable digest.

`a3s bench list` shows only admitted built-ins. `a3s bench list --all` includes
quarantined discovery records, and `a3s bench info <task-id> --all` can inspect
one without making it runnable. Catalog inspection is read-only and creates no
project state.

TaskRevision remains the digest of compiled TaskLock content. A task always
owns its Judge, so there is no `--judge` override. Any generated state from a
later operation remains under the current project `.a3s/bench/` root,
regardless of where the local TaskBundle resides.

With `--locked`, Bench performs no selector or Task source resolution and never
chooses a cached lock heuristically. Task must be an explicit exported TaskLock,
Candidate must be an explicit exported CandidateLock, and all referenced
artifact bytes must already be locally available by digest. Bare built-ins,
embedded aliases, local TaskBundle directories, OCI references, Asset
selectors, and mutable published selectors are rejected under `--locked`.
An offline TaskLock envelope declares exactly one governance status. An
`official` lock must carry a valid admission chain and signed revocation
snapshot; a `local_unofficial` lock must not claim admission. Missing, invalid,
or contradictory material fails without catalog fallback or status downgrade.

Offline begins before Bench activation. A compatible verified Bench component,
selected Runtime provider, signed trust/revocation material, matching artifact
bytes, and current tenant/privacy-class authorization must already exist
locally. `--locked` never installs or updates Bench/Box, refreshes credentials,
or follows lock locators. A lock is not bearer authority. The official
revocation snapshot must be signed and within its admission-defined freshness
window at plan commit; an expired or stale snapshot fails without refresh.

## Conventional Layout

~~~text
my_task/
  task.acl
  public/
    prompt.md
    workspace/
  private/
    judge/
      .a3s/
        asset.acl
      agent.md
      judge.py
    bundle/                 optional terminal hidden data
~~~

The candidate execution receives only `public/`. The Judge Asset is resolved
separately, and the A3S OS Runtime exposes the terminal hidden bundle only as a
protected read-only mount in the Judge execution. An authored or admitted Task
that needs no hidden data may omit `private/bundle/`; the compiler records the
canonical empty-tree digest, so source control need not preserve an empty
directory. A quarantined import whose bundle source is missing is unavailable,
not empty, and cannot be admitted until provenance resolves that absence. Do
not put hidden tests, expected answers, or evaluator secrets inside the Agent
Asset: an OS-hosted asset is an authoring and distribution source, not the
private data plane.

Before parsing a local bundle, Bench atomically captures one
TaskSourceSnapshot. Task ACL, prompt, workspace, local Judge package, and hidden
bundle resolution consume that one captured generation. If any included file
changes during capture, validation fails with `source_changed`; Bench never
combines generations or silently retries into a different TaskRevision.

The captured semantic closure is exactly root `task.acl`, declared or
conventional prompt/workspace, the complete selected local Judge package, the
declared or conventional hidden bundle. Unreferenced author notes and
repository files are not hashed,
mounted, or visible to either Agent and do not change TaskRevision. Every
captured entry must be a regular file or directory inside the bundle; P1 rejects
symlinks, hardlinks, special files, mount crossings, case collisions, and path
replacement during capture even when a link currently resolves inward.

## Minimal Canonical ACL

~~~acl
bench "example_task" {
  schema  = "a3s-bench/task/v1"
  version = "0.1.0"

  name        = "Example task"
  category    = "systems"
  description = "Improve the solution while preserving correctness."

  work {
    image {
      ref = "docker.io/library/node:22-bookworm-slim"
    }
  }

  judge {
    asset = "private/judge"
  }

  metric "correctness" {
    type      = "ratio"
    role      = "gate"
    direction = "maximize"
    min       = 0
    max       = 1
    gate      = "eq:1"
    gate_failure_score_basis_points = 0
    solution_failure_value = "0"
    public_report = true
  }

  metric "latency_p95_ms" {
    type          = "duration_ms"
    role          = "primary"
    direction     = "minimize"
    min           = 0
    max           = 600000
    normalization = "linear_range_v1"
    solution_failure_value = "600000"
    public_report = true

    measurement {
      warmup_repeats   = 3
      measured_repeats = 10
      estimator        = "p95"
      outlier_policy   = "none"
      tolerance        = "1"
    }
  }
}
~~~

Use `a3s bench info ./my_task` to inspect every expanded value and its source
before locking.

## Judge Asset

`judge.asset` accepts the same `AgentAssetRef` family as `--agent`:

| Form | Meaning |
| --- | --- |
| `private/judge` | Local package containing `.a3s/asset.acl`. |
| `asset:judge-name` | Exact asset name in the signed-in user's OS scope. |
| `asset:owner/judge-name` | Exact OS owner and asset name. |
| `asset://<uuid>/<ref>` | Explicit OS asset plus branch, tag, or commit. |
| `https://<configured-os>/.../assets/<uuid>` | Pasteable Asset URL on the configured A3S OS origin. |
| `oci://<registry>/<repository>:<tag>` or `@sha256:<digest>` | Agent Asset package from any OCI-compatible registry. |

An HTTPS reference is accepted only when its origin exactly matches the
configured A3S OS origin. Authority-changing redirects are rejected, and
credentials are never forwarded to a Task-controlled or pasted foreign origin.
OCI references are registry-neutral but must resolve to a complete
`a3s.asset.v1`, `category = "agent"` package, never merely a container image.
Registry credentials are authority-scoped and never forwarded when an OCI
redirect changes authority. Mutable authoring tags are resolved into exact
manifest and package-content digests when TaskLock is created.
Under `--locked`, no remote Asset selector is resolved; TaskLock already fixes
the Judge AssetSnapshot and required bytes must be locally available by digest.

Ambiguous names are errors. A local or remote package must use
`version = "a3s.asset.v1"`, `category = "agent"`, and explicitly declare the
typed Judge capability. A minimal deterministic Judge manifest is:

~~~acl
version = "a3s.asset.v1"
category = "agent"
kind = "tool"
name = "example-judge"
description = "Offline evaluator for example_task."
service = "Function as a Service"

source {
  package_path = "."
  entrypoint = "judge.py:evaluate"
  definition_path = "agent.md"
}

metadata {
  asset_acl_path = ".a3s/asset.acl"
}

runtime {
  kind = "tool"
  isolation = "serving"
  runtime_kind = "a3s-function-service"
  protocol = "agent-tool"
  agent_kind = "tool"
}

capability "bench.judge.v1" {
  input_schema = "bench.judge.request.v1"
  output_schema = "bench.judge.result.v1"
  network = "none"
  model_gateway = "none"
}
~~~

The capability is the explicit grant that lets a Task use the Agent Asset as a
Judge. The asset's ordinary entrypoint, model, tools, runtime, and dependency
closure remain authoritative: Bench does not substitute a private execution
path. The compiler locks the entire AssetSnapshot and the exact capability
declaration into the TaskLock.

The function entrypoint above is a generic tool-Asset contract owned by the
shared A3S Asset schema and OS Runtime, not a Bench handler ABI. Current platform
tooling must gain this standard deterministic function form before the fixture
is executable; Bench must not load the Python symbol itself.

Candidate and Judge executions use the same A3S OS Runtime API. Bench submits a
`RuntimeExecutionSpec` with `role = "candidate"` or `role = "judge"`; it does
not select a separate Judge executor. For the Judge role, the Runtime:

- starts the locked Agent AssetSnapshot using its normal runtime contract;
- exposes the Runtime-derived SubmissionSnapshot and locked hidden bundle
  through distinct protected read-only mounts;
- never exposes the Candidate-private TerminalCheckpoint, Candidate adapter,
  controller state, credentials, or private logs;
- provides a typed `bench.judge.request.v1` request;
- accepts only `bench.judge.result.v1` from the protected result channel;
- returns a standard `RuntimeExecutionResult` with resource and identity
  evidence.

Stdout, stderr, and arbitrary workspace files are never authoritative Judge
results. Bench validates the protected typed result against the locked metric
schema. An execution error, unknown or out-of-range field, identity mismatch,
or oversized result is a Judge contract failure, never a candidate score.

The deterministic manifest above receives neither network nor ModelGateway
access and is the P1 admission target. A model-based Judge declares
`model_gateway = "scoped"` and a concrete model in its ordinary Agent Asset
contract; it becomes runnable only after the shared Runtime capability and
locked operator policy admit that route and budget. This uses the same product
and execution path. Judge capabilities never inherit candidate credentials.
The hidden bundle is not uploaded as part of an OS Agent Asset.

Runtime treats every Judge as containment-untrusted. Its result is
measurement-authoritative because the local Task author selected the exact
locked snapshot, or because an official built-in admission record authorizes
it. Bench validates identity, protocol, isolation, bounds, and deterministic
score projection; it does not certify evaluator correctness, scientific
validity, or absence of bias.

### Quarantined Judge sources

Discovery is not execution admission. A built-in A3S `category = "agent"`
package may record an inert `judge.source.json` while its catalog entry has
`admission = "quarantined"`. This permits catalog and provenance inspection
without interpreting a source image command, stdout parser, interactive
service, model route, or credential request as an admitted Judge execution.

Such a package is not a second Judge API. Advanced validation, lock compilation,
and `run` must fail before image pull or billable work until a signed admission
record binds the exact AssetSnapshot, immutable dependencies, typed
request/result schemas, A3S OS Runtime behavior, capabilities, licenses,
resources, timeouts, and evidence requirements. An admission record cannot
authorize an unknown runtime contract or cause Bench to execute commands stored
in task metadata. The task still owns the Judge and entrants still cannot
replace it.

## Canonical Defaults

`docs/design.md` is normative when this quick reference or an example is
incomplete. Neither this file, generated help, catalog metadata, nor examples
add accepted fields or syntax. A contradiction is a defect and fails closed.

The ACL grammar and schemas are closed. Unknown or duplicate attributes and
blocks, duplicate map keys, unknown enum values, and unsupported schema IDs are
errors. Bench performs no coercion, trimming, environment interpolation,
case-folded lookup, unknown-field preservation, or malformed-input repair.

All strings are valid NFC UTF-8. Semantic strings reject NUL and Unicode control
characters other than grammar whitespace. Identifiers are ASCII and
case-sensitive. Relative paths use `/` and reject absolute paths, empty, `.`,
or `..` segments, trailing `/`, backslash, drive/URI prefixes, symlinks, hard
links, and special files. Exact on-disk spelling must match even on a
case-insensitive host. Integers are canonical base-10 ASCII; `_sec` means
integer seconds, sizes mean integer bytes, limits are inclusive, and overflow
or coercion is an error. P1 digests are exactly lowercase `sha256:` plus 64
lowercase hexadecimal digits.

Defaults are schema-owned, versioned content. They are expanded into the
`TaskLock` and therefore cannot change beneath an existing `TaskRevision`.

| Area | Default |
| --- | --- |
| prompt | `public/prompt.md` |
| workspace | `public/workspace` |
| work | `work-medium-v1`: linux/amd64 microVM, 4 CPU, 8,589,934,592 memory bytes, 21,474,836,480 disk bytes, 512 PIDs |
| task egress | none |
| submission | include `["**"]`; exclude `.git`, `.a3s/bench`, `node_modules`, and target trees; 50,000 files; 536,870,912 total bytes; 67,108,864 per file |
| judge asset | `private/judge`; standard `a3s.asset.v1`, `category = "agent"`; capability `bench.judge.v1` |
| hidden bundle | `private/bundle`; when intentionally omitted by an authored or admitted Task, lock the canonical empty-tree digest |
| judge role policy | `judge-small-v1`: linux/amd64 microVM, 2 CPU, 4,294,967,296 memory bytes, 10,737,418,240 disk bytes, 4,294,967,296 scratch bytes, 256 PIDs, cold reset, functional cohort |
| judge protocol | typed `bench.judge.request.v1` input and protected `bench.judge.result.v1` output channel |
| timeout | 480 seconds candidate plus 60 seconds harness grace |
| direct metric | direct measurement with type-specific quantization |

An author can override a documented field. The resolved value and profile
digest enter the lock. Performance tasks must declare the required measurement
cohort when the unqualified local default is not sufficient.

## Lock and Refresh Semantics

The first `run`, `advanced check`, or `advanced task lock` resolves the Judge
selector exactly once. The resulting `TaskLock` includes:

- the canonical Judge `AssetSnapshot` and dependency closure;
- the locked `bench.judge.v1` capability declaration;
- the terminal hidden-bundle digest and typed request/result schemas;
- the complete submission projection policy and bounds;
- the Judge role policy and required Runtime capabilities;
- immutable work-environment identity and all expanded task defaults.

Each new `run` resolves its current selectors exactly once and creates or reuses
locks by content digest. Changed local bytes or a moved mutable OS reference
therefore create a new revision for that new run, while retries and resume stay
on the run's sealed revision.

`--locked` disables all selector/source resolution, registry access, OS lookup,
catalog lookup, embedded-alias expansion, and heuristic cache selection. Task
must be an explicit exported TaskLock and Candidate must be an explicit exported
CandidateLock. Every referenced artifact must already be locally available by
digest. Bare built-ins, embedded aliases, local TaskBundle directories, OCI
references, Asset names, revisions, branches, tags, pasted URLs, and a cached
"only match" are rejected. Missing content is an offline-unavailable error;
there is no refresh or fallback mode. The explicit TaskLock envelope declares
exactly one governance status: `official` requires a valid admission chain and
signed revocation snapshot, while `local_unofficial` must carry no admission
claim. Bench never consults the catalog or silently changes either status
offline.

This also forbids component/provider lazy installation, update, credential
refresh, and locator following. Local availability means both exact bytes and
current-principal authorization; digest equality alone is not access.

## Field Guide

### bench

The block label is the task ID: lowercase ASCII snake_case, beginning with a
letter, at most 64 characters. It is a display/path key, not `TaskRevision`.
`schema` is generated by `a3s bench advanced init`; `version` is the task's
SemVer release. `name`, `category`, and `description` are required. `tags` is
optional.

### prompt, workspace, and work

The conventional public paths need no ACL blocks. Override them only when
necessary. A workspace selects one bundle-relative public directory,
digest-pinned artifact, digest-pinned OCI artifact, or explicit empty seed. It
is materialized at `/workspace`; paths cannot escape the bundle or use reserved
judge, result, scratch, or secret roots.

For an OCI workspace seed, `source_path` selects the directory to extract from
the immutable image filesystem. It never changes the candidate mount point:

~~~acl
workspace {
  oci {
    ref         = "registry.example/task-workspace:source-revision"
    platform    = "linux/amd64"
    source_path = "/home/workspace/project"
  }
}
~~~

An authored tag is resolved once while creating the new run's TaskLock. The
lock records the manifest digest and normalized extracted workspace manifest.
Official admission rejects content that cannot be licensed, scanned, resolved
immutably, or materialized without unsafe file types and reserved-path escapes.

`work.image` selects the Candidate-visible workspace/tool sandbox. P1 accepts an
immutable OCI reference or an authoring OCI reference resolved to an exact
manifest digest at lock time. It does not execute an inline Dockerfile, build
hook, package install, or Task command. Authors must produce an image through a
separate trusted build workflow before locking; adding a future hermetic builder
requires a shared platform contract and does not make Bench an image builder.
The Candidate adapter still defines the controller entrypoint and controller
runtime; A3S OS Runtime attaches that controller to the sandbox via the standard
workspace capability instead of merging or replacing images. The lock records
the resolved content and provenance. `network_need` describes task egress only;
model inference uses the separately scoped ModelGateway selected from the
Candidate adapter and operator policy.

P1 admits only `network_need = "none"`. A Task that requires general egress is
portable-valid only under a future schema/profile and cannot run in P1. Model
inference is not general egress: CandidateLock requests a concrete
ModelGateway scope, and the sealed grant is the intersection of Asset requests,
Task limits, and operator allowance. P1 treats all discrete Asset requests as
required, so the allowed set must contain them all. Scalar resources and
budgets resolve to one exact value satisfying Asset minima, Task
ceilings/defaults, and operator maxima. Any unsatisfied constraint is
`policy_rejected`; Runtime never substitutes a model, route, tool, resource, or
weaker isolation mode.

`none` denies more than routed internet access: no DNS, loopback,
link-local/metadata, host bridge service, inherited or new IP socket, listening
or raw socket, proxy inheritance, or unsealed Unix-domain socket is available.
ModelGateway and protected result/mount channels are typed Runtime capabilities
with fixed peers and protocols, not general sockets or reusable credentials.

### submission

`submission` defines a deterministic projection policy, not a Candidate mount
or a host copy command. Runtime first captures the complete terminal workspace
as one Candidate-private TerminalCheckpoint. In the same fenced terminal
generation it applies the locked include/exclude patterns, normalized path
rules, file-type rules, and size/count bounds to derive one immutable
SubmissionSnapshot.

The Judge receives only SubmissionSnapshot. It never receives the full
TerminalCheckpoint, Candidate adapter/controller source, credentials,
private logs, or live workspace. Bench and Candidate cannot provide an
authoritative submission path, manifest, digest, bytes, or ArtifactRef. Runtime
evidence binds the SubmissionSnapshot digest to the source checkpoint and
projection-policy digest. Projection failure is a typed Candidate failure, not
a Judge score.

### judge

The task declares one Judge Agent Asset. Its `bench.judge.v1` capability and
ordinary A3S runtime contract apply to the one terminal Judge execution. The
Runtime mounts SubmissionSnapshot and `private/bundle/` separately and
read-only, never mounts the full TerminalCheckpoint, and returns one typed
JudgeResult through the protected result channel.

`solution_timeout_sec` limits candidate execution and
`harness_grace_sec` reserves protected cleanup and result collection. Judge
resource requirements are exact scoring inputs for performance tasks. Task ACL
cannot provide a shell command, replace the Agent Asset runtime, relax Judge
isolation, grant capabilities, or bind secrets. Those requests belong to the
Judge Asset and must also pass operator policy.

### metric

Every task declares exactly one primary metric. Other roles are `gate`,
`tie_break`, and `diagnostic`. Gates and tie-breaks may affect ranking;
diagnostics do not. Correctness gates explicitly define their normalized
failure score.

`public_report = true` permits only the declared canonical metric value and
Bench-generated normalization/gate projection in a redacted export. It cannot
publish Judge free-form diagnostics, hidden expected values, test names, model
internals, protected metadata, private logs, or raw Runtime evidence. All
Judge-provided diagnostics remain private in P1.

Built-in normalization names such as `linear_range_v1` are immutable schema
definitions. Repeated performance metrics declare warmup, repeats, estimator,
outlier policy, tolerance, and any non-default quantization. Binary floating
point is not used for official score normalization.

### governance

`governance` is optional for local authoring. A future admitted publication
operation will require license, immutable source revision, first-public date,
contamination review, and campaign-required reference metadata before signing
or upload; P1 exposes no publish command.

A portable-valid local Task runs only as `local_unofficial`; its report cannot
claim built-in or official status. Official admission is a separate signed A3S
record binding the exact TaskLock, Judge snapshot, dependency and image closure,
RuntimeSemanticsProfile, cohort, provenance, privacy, evidence requirements,
validity interval, and signer role. Task ACL, Judge metadata, environment
variables, operator configuration, and Advanced commands may cause denial but
cannot create, extend, replace, or promote admission.

~~~acl
governance {
  license                   = "MIT"
  source_revision           = "sha256:<source-digest>"
  first_public_date         = "2026-07-10"
  contamination_status      = "reviewed"
  reference_solution_digest = "sha256:<solution-digest>"
}
~~~

## Fields That Do Not Belong Here

Task ACL intentionally excludes:

- registry mirrors, signatures, credentials, and retention;
- trial seeds, budgets, checkpoint times, feedback quota, and aggregation;
- Candidate implementation, model, adapter instructions, tools, and memory
  policy;
- Flow Asset references, authored workflow definitions, and orchestration
  selectors;
- executor selection, host fallback, and concrete secret bindings;
- artifact backend, privacy-class changes, retention, deletion, and operational
  observability;
- admission status, signer, official-result label, or trust-root selection;
- public promotion of Judge free-form diagnostics or raw evidence.

Task ACL also excludes host environment interpolation. `env()`, `${NAME}`,
proxy variables, shell expansion, current-session values, and
`A3S_BENCH_*`/`BENCH_*` overrides are invalid Task semantics. Platform
credentials may authorize the configured resolver. The local Runtime may also
use project-local or user-local `.a3s/config.acl` to resolve the provider route
and credential for an exact model selected by the Candidate adapter or
`--model`, without an A3S OS login. It never inherits `default_model` for a
Bench run. Resolved outcome-affecting choices are locked, while secret bytes
never enter the ACL, lock, report, evidence, or Agent environment.

These belong to the selected Candidate adapter, `CandidateLock`, operator
policy, `ExperimentPlan`, or generated `RuntimeExecutionSpec`. Bench's internal
run workflow is implementation code executed by its embedded `a3s-flow` engine;
it is not a Flow Asset, is not benchmark input, and cannot be selected from
`task.acl` or the CLI. The engine is a code dependency shipped with Bench, not
an asset dependency: Task checking, locking, running, recovery, cancellation,
and result lookup never read a Flow catalog, registry, account, manifest,
credential, environment variable, or `.a3s/flow/` directory. User Flow Assets
cannot enable, disable, or alter an evaluation. The Bench compiler expands the
remaining fields from content-addressed safe defaults.

## What `run` Validates

`run` performs the same portable validation as
`a3s bench advanced check <task>` before it reserves Runtime capacity. It fails
on:

- unknown or duplicate task fields and blocks;
- a local TaskBundle that cannot be captured as one stable TaskSourceSnapshot;
- unsupported schema or dynamic functions such as `env()`;
- inline image builds, Dockerfiles, install hooks, package hooks, or Task
  commands in P1;
- a missing or malformed `.a3s/asset.acl`;
- a Judge package that is not `category = "agent"` or lacks the explicit
  `bench.judge.v1` capability;
- a quarantined Judge source, unsupported Agent Runtime contract, or a built-in
  whose official execution lacks a valid signed admission binding;
- an incompatible request/result schema, public input, or terminal hidden bundle;
- a quarantined import that presents unavailable hidden data as an intentional
  canonical empty bundle;
- a hidden bundle placed inside the judge asset;
- a submission policy that can expose reserved/private paths, unsafe file types,
  or unbounded content;
- non-integer resource fields or invalid metric contracts;
- path, symlink, reserved-root, or build-context escape;
- an HTTPS Asset origin that differs from the configured A3S OS origin or
  redirects to another authority;
- P1 general egress or an internally inconsistent Asset-request/Task-limit
  contract;
- a mutable selector that cannot be resolved for a new non-locked run;
- any mutable selector, source directory, remote lookup, or heuristic cached
  match under `--locked`;
- task fields that attempt to control operator policy.

Advanced portable validation does not require a local Runtime provider.
`a3s bench advanced doctor` reports A3S OS Runtime, local Box provider, Asset
Center, ModelGateway, registry, and credential readiness; `run` invokes both
validation and readiness automatically. Run preflight additionally rejects a
missing RuntimeSemanticsProfile, protected checkpoint/submission/result
capability, ArtifactStore privacy/pin capability, exact model route, resource
range, operator allowance, or required signed conformance evidence. Every
diagnostic includes a source span when applicable, the violated rule, and a
concrete remediation.
