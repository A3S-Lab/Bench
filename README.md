# a3s-bench

`a3s-bench` is the benchmark control plane for A3S agents. A user selects a
task and a candidate A3S Agent Asset; Bench locks the inputs, asks the A3S OS
Runtime to run the candidate and the task-owned Judge Agent Asset, then stores
and prints a reproducible result.

> Current status: this repository contains the canonical contract, authoring
> fixtures, and 51 pinned third-party Task/Judge sources. A runnable Bench
> control-component release has not been published yet. All imported tasks are
> quarantined discovery records, not executable claims. Shared OS Runtime
> support for immutable AssetSnapshot execution, atomic terminal checkpoints,
> protected Judge mounts/results, and durable operation reattachment is a P0
> platform prerequisite; these fixtures are not evidence of an end-to-end run.
> The first usable release is blocked until all 51 are admitted and work out of
> the box; quarantine-only publication is a source snapshot, not a release.

The development component validates the complete 51-entry catalog on load:
every catalog ID/path/metadata tuple must match a parseable Task descriptor,
every Task must provide a supported executable Judge source, Judge platforms
must be pinned to `linux/amd64`, and the catalog must exactly cover the packaged
task directories. This is a structural and execution-protocol gate; it does not
replace signed admission or claim that all 51 task images have completed a full
evaluation run.

Local development runs also create an owner-only
`a3s.bench.run-journal.v1` record before input resolution. Runtime readiness,
input resolution, Candidate execution, judging, and terminal completion/failure
are persisted by atomic rename. Completed journals bind the same run ID and
result path emitted by the CLI. This is the filesystem lifecycle foundation;
idempotent shared-Runtime operation reattachment and cancellation remain P1
work rather than being simulated by a Bench-local Docker/Box abstraction.
An exact local run ID can already be passed to `result` to inspect a persisted
nonterminal or failed stage. That projection deliberately omits the journal's
private error text; only a committed result can expose a score.
Completed development runs use the closed `a3s.bench.local-result.v2` record,
which binds `primary_metric`, `score`, and the matching Judge metric and rejects
unknown fields or inconsistent model token totals when reloaded.
Imported `a3s-bench/judge-source/v1` descriptors are also decoded through a
closed typed schema, including their nested evaluation, image, workspace, and
tagged rescale-hint variants; all 51 packaged descriptors are covered by one
catalog-wide test.

Until the shared Runtime checkpoint API lands, the local development path
projects the Candidate terminal directory into a separate owner-only
SubmissionSnapshot using the exact include, exclude, file-count, depth, and
byte limits sealed in TaskLock. Projection walks the complete terminal tree
before filtering and rejects non-UTF-8 or non-normalized names, case-colliding
paths, symlinks, hard links, and special files, including unsafe entries that
an exclude rule would otherwise hide. `.a3s/bench` is always reserved. The
legacy imported Judge adapter mounts only this sealed snapshot read-only at
`/a3s/submission`, copies its contents into the Judge image's disposable
baseline workspace, restores owner write permission there, and runs the pinned
upstream command; it never mounts the Candidate-private terminal directory.
This is development conformance coverage, not a change to the normative
ownership boundary: the shared Runtime must ultimately capture the terminal
checkpoint, derive and attest the SubmissionSnapshot, and supply its protected
Judge mount.

There is one user-facing design and one command prefix: `a3s bench`.

The [canonical design](docs/design.md) is the normative P1 protocol. This
README, examples, catalog records, generated help, and the ACL quick reference
cannot add syntax or weaken it. Any contradiction is a defect and fails closed.
All input schemas are closed: unknown fields, options, enum values, capability
names, schema IDs, and versions are rejected rather than ignored or forwarded.
Bench never repairs, coerces, trims, case-folds, or guesses malformed input.

## Four Commands

The normal interface consists of four commands:

| Command | Purpose |
| --- | --- |
| `a3s bench list` | List admitted built-in tasks that can be run. |
| `a3s bench info <task>` | Explain a task, its admission, and its locked Judge. |
| `a3s bench run <task> --agent <agent>` | Run one evaluation and print its result. |
| `a3s bench result [run-id]` | Show the latest or a selected result again. |

The shortest complete workflow is one command:

~~~bash
a3s bench run ./examples/smoke --agent codex
~~~

`run` performs validation, asset resolution, readiness checks, execution,
judging, scoring, persistence, and report generation. On completion it prints
the score, verdict, run ID, closed-code public diagnostics, and report path. A
second `result` command is optional, not a required final step.

Use `--json` on `list`, `info`, `run`, or `result` for automation. Machine
output goes to stdout; progress and diagnostics go to stderr.

P1 has no implicit or abbreviated Bench options. `list` accepts `--all` and
`--json`; `info` accepts `--all` (bare catalog IDs only) and `--json`; `run`
requires one `--agent` and accepts only `--model`, `--locked`, and `--json`;
`result` accepts only `--out` and `--json`. A duplicate singleton, wrong-command
option, missing value, unknown alias, or environment-provided option is an
error.

## Task References

Task references are deliberately explicit:

| Form | Meaning |
| --- | --- |
| `<task-id>` | Exact ID of an admitted built-in task. |
| `./tasks/smoke` | Local TaskBundle containing `task.acl`. |
| `./tasks/smoke/task.acl` | Local Task ACL. |
| `./tasks/smoke/task.lock.json` | Exported immutable TaskLock. |

A bare task ID searches only admitted built-ins. It never searches the current
directory, A3S OS, an upstream source catalog, or quarantined entries. A local
reference must start with `./` or `../`; this prevents a local directory from
silently shadowing a built-in task.

Local types are determined from `lstat` and schema content, not suffixes. A
directory must have one root `task.acl`; a regular file named `task.acl` selects
its containing bundle; another regular file must be a complete TaskLock v1.
Symlinks, special files, and `.json` files with the wrong schema are rejected.

Digest-pinned published TaskBundles are accepted as the Advanced input
`oci://registry.example/tasks/name@sha256:<digest>`. Mutable OCI tags are
authoring inputs only and are resolved into a TaskLock before execution.

There is no source-specific selector namespace. Upstream project names and
revisions appear only in provenance and third-party notices.

### Listing and inspecting quarantined sources

`list` shows only admitted, runnable tasks:

~~~bash
a3s bench list
~~~

Use `--all` to include quarantined catalog records:

~~~bash
a3s bench list --all
a3s bench info ad_placement_optimization --all
~~~

`info <id> --all` is catalog inspection, not runnable task resolution. It shows
the admission state and reasons without pulling an image, resolving credentials,
or creating project state. `run ad_placement_optimization` still fails unless
that exact built-in revision has been admitted.

The 51 current third-party imports are all quarantined. See
[Built-in Tasks](builtin/README.md) for their provenance and admission rules.
Until P1 admits all 51 conformant built-ins, the default `list` is therefore
empty; `list --all` is the useful catalog view in this incomplete source-tree
snapshot. A release cannot preserve this quarantine-only state.

A validated local TaskBundle may run as `local_unofficial`; it does not need an
A3S admission signature and its report can never claim built-in or official
status. Official built-ins require a signed, unexpired, unrevoked admission
record rooted in A3S trust material shipped by the signed top-level CLI. Task
files, Judge manifests, environment variables, operator settings, and Advanced
commands may deny an admitted task but cannot promote quarantine or manufacture
official status.

## Agent Assets

Both participants in an evaluation are ordinary A3S Agent Assets:

- `--agent` selects the candidate Agent Asset;
- `task.acl` selects the Judge Agent Asset;
- both are resolved to immutable AssetSnapshots;
- both are executed by the A3S OS Runtime under different role policies.

The candidate selector forms are:

| Form | Resolution |
| --- | --- |
| `codex` | Embedded selector resolving to the installed component's pinned Candidate snapshot. |
| `./agents/reviewer` | Local A3S Agent Asset package. |
| `asset:reviewer` | Exact name in the signed-in user's A3S OS scope. |
| `asset:acme/reviewer` | Exact A3S OS owner and asset name. |
| `asset://<uuid>/<ref>` | Explicit OS asset and branch, tag, or commit. |
| `https://<configured-os>/.../assets/<uuid>` | Pasteable Asset URL on the configured A3S OS origin. |
| `oci://<registry>/<repository>:<tag>` or `@sha256:<digest>` | A3S Agent Asset package from any OCI-compatible registry. |
| `./agents/reviewer.candidate.lock.json` | Exported immutable CandidateLock. |

Bare agent words search embedded aliases only. Ambiguous OS names are errors,
and the current account's default model is never silently selected. If an
asset does not lock a concrete model, provide an allowed model explicitly:

~~~bash
a3s bench run ./examples/smoke \
  --agent asset:acme/reviewer \
  --model openai/gpt-5
~~~

A local Candidate path is classified by `lstat` and schema content. A directory
must contain root `.a3s/asset.acl`; a regular file must be a complete
CandidateLock v1. Symlinks, special files, direct internal-manifest paths, and
extension-based guessing are rejected.

OCI Candidate sources are registry-neutral: Docker Hub, a private registry, or
any other OCI Distribution-compatible registry may be used. The referenced
artifact must contain a valid `a3s.asset.v1`, `category = "agent"` package;
Bench never guesses an Agent definition from an arbitrary container image.
Mutable tags are accepted only as unlocked source selectors and are resolved
once to an exact manifest and package-content digest before planning. Exported
CandidateLock and `--locked` execution contain no mutable OCI reference.
Registry credentials remain resolver-owned, are scoped to the requested
registry authority, and never enter the Asset snapshot or Agent sandbox.
The development resolver first accepts Docker-compatible OCI images with an
embedded `/.a3s/asset.acl`. If the reference is not an image, it uses the
standard `oras resolve` and `oras pull` commands and accepts any artifact/media
type whose extracted file set is a valid closed A3S Agent Asset package. ORAS
owns registry authentication; a missing ORAS installation is reported only
when this generic artifact path is required. Both paths cache by the resolved
`sha256` identity, reject symlinks and special files, and publish the cache
atomically. Candidate and task-owned Judge references use the same resolver.

For a model-backed Candidate in local no-login mode, Bench reuses A3S Code's
provider/model definitions from the discovered `.a3s/config.acl` and the exact
model selected by `--model` or CandidateLock. Model API calls remain in the
host-owned Runtime client, while workspace file tools are confined to the
private Bench workspace and bash tools execute through the selected Docker
sandbox. Provider keys and base URLs are never copied into the container, lock,
result, or report. Results record only the exact model ID, token usage, and tool
call count. This path does not contact A3S OS or require an OS session.

An embedded alias is a selector, not reproducible identity. A component update
may move `codex` to a new CandidateRevision for a new unlocked run; the resolved
revision is always locked into that run, and `--locked` rejects the alias.

`--model` is fill-only: it cannot replace a model already bound by the Asset.
The resulting model, parameters, tools, memory policy, prompt/controller
configuration, and capability requests are sealed in CandidateLock. Runtime
grants only the intersection of Asset requests, Task limits, and operator
allowance. P1 treats every discrete Asset request as required; if the complete
set is not allowed, planning is `policy_rejected`. Scalar resources and budgets
must resolve to one exact value satisfying Asset minima, Task ceilings, and
operator maxima. There is never a silent model, route, tool, network, resource,
or isolation substitution. Bench does not inherit the current user's default
model, Code session, Memory, MCP servers, shell environment, or other ambient
configuration.

Bench also ignores `A3S_BENCH_*`, `BENCH_*`, proxy, model, prompt, tool, MCP,
Memory, session, shell, and credential environment variables as benchmark
input. This does not prevent the local Runtime from using the standard local
`.a3s/config.acl` as operator configuration. For a model explicitly selected by
the Candidate Asset or `--model`, the local Runtime may resolve the matching
provider/model entry, endpoint, and credential from project-local or user-local
configuration. Outcome-affecting model identity and parameters are sealed in
the plan; provider credentials and other secret bytes never enter a lock,
report, or Agent sandbox. `A3S_COMPONENTS_DIR` changes only the private
component location; locale, color, and TTY state affect presentation only.

A pasted HTTPS Asset URL is accepted only when its origin exactly matches the
configured A3S OS origin. Authority-changing redirects are rejected, and Bench
never sends an OS credential to an origin learned from Task content or a pasted
foreign URL.

The task owns its Judge. Users cannot replace it and there is no `--judge`
option. Local, OCI, and A3S OS Judge references use the same asset resolver as
the candidate; the resulting Judge AssetSnapshot is part of the TaskLock.

Embedded and local assets with the local Runtime provider do not require an A3S
OS login, including when the local Runtime serves model inference from a
provider/model configured in `.a3s/config.acl`. Signing in is required only to
resolve an OS-hosted asset or use a Runtime/provider that explicitly requires
A3S OS authority. The model must still be selected explicitly or bound by the
Asset; Bench never inherits `default_model`. Provider authentication may still
be required, but a local provider credential is not an A3S OS login.

## What Happens During `run`

~~~text
authored TaskBundle             -> atomic TaskSourceSnapshot
task snapshot + Judge Asset     -> immutable TaskLock
candidate Agent Asset           -> immutable CandidateLock
TaskLock + CandidateLock        -> run plan
run plan                        -> A3S OS Runtime executions
SubmissionSnapshot + JudgeResult -> persisted result and report
~~~

Before billable work, `run` prints the resolved task revision, candidate and
Judge snapshots, models, trial count, hard budgets, and worst-case retry
exposure. It then:

1. validates the task and its admission;
2. resolves mutable asset references exactly once and writes immutable locks;
3. asks the A3S OS Runtime to start an isolated candidate execution;
4. captures the complete terminal workspace as a Candidate-private checkpoint;
5. asks Runtime to derive a separate immutable SubmissionSnapshot using the
   locked include/exclude and size policy;
6. asks the same Runtime to start the locked Judge Asset with only that
   SubmissionSnapshot and the protected hidden bundle;
7. validates the typed JudgeResult, computes the score, and commits evidence;
8. prints the result and report location.

Candidate and Judge isolation, lifecycle, mounts, ModelGateway capabilities,
resource enforcement, checkpointing, submission projection, cancellation, and
evidence collection belong to the A3S OS Runtime. If the user has neither an
authenticated A3S OS session nor an explicitly configured Runtime provider, the
shared Runtime selects its local Docker provider. Users may instead select any
conforming provider, such as `a3s-box`, through the standard
`.a3s/config.acl`. Bench does not implement a second sandbox, file projector,
container executor, or Agent Runtime and never calls Docker or Box directly.

Both roles use the same OS-owned `A3sRuntimeClient` entry point. Bench imports
that shared platform client rather than defining a Runtime port. It submits a
`RuntimeExecutionSpec` with `role = "candidate"` or `role = "judge"` and
receives the same `RuntimeExecutionResult` shape; role policy changes
capabilities, not the execution architecture.

Bench owns only evaluation-domain behavior: task compilation, immutable locks,
run planning, JudgeResult validation, scoring, result storage, and reporting.

### No Flow Asset is required

Bench links the `a3s-flow` engine as a private implementation library to persist
and resume run steps. This does **not** make Flow an evaluation asset or user
dependency. A benchmark run is not an A3S Flow Asset: Bench does not resolve,
publish, import, create, or execute Flow Assets, and users cannot select one.
There is no `--flow` option and no Flow Asset reference in `task.acl`, a lock,
or a plan.

The internal workflow is implementation state owned by Bench. It coordinates
the two Runtime operations for Candidate and Judge; it is not benchmark input,
does not enter TaskLock or ExperimentPlan identity, and is never exposed as a
second authoring or execution surface. Bench requires no Flow login, registry,
catalog, installed Flow Asset, manifest, credential, or `.a3s/flow/` state. It
must behave the same whether user Flow Assets exist or not.

The boundary is literal: `a3s-flow` may schedule and recover already-locked
steps, but it cannot choose or alter the Task, Candidate, Judge, model, Runtime
provider, budgets, disposition, or score. Only the shared Runtime can establish
execution facts, and only BenchStore can commit benchmark-domain facts.

## Installation and Lazy Loading

Installing `a3s` includes A3S Code. Bench is a private, lazily installed
control component:

~~~bash
a3s install code    # verify/repair the Code component included with a3s
a3s install box     # optional when config.acl selects the a3s-box provider
a3s install bench   # optional preparation
a3s list            # inspect component state; never installs
a3s update bench    # update an already installed component
~~~

`a3s install code` does not create or download a second Code installation;
Code ships with `a3s`. Box and Bench remain optional delayed components. The
default no-login Docker provider requires a compatible user-supplied Docker
Engine; Bench does not install or reconfigure Docker.

`a3s bench --help` and `a3s bench --version` work without installing Bench.
The first command that needs Bench functionality installs the compatible
control component on demand. The component is private to `a3s bench`, is never
added to `PATH`, and does not provide an alternative CLI.

Consequently, `a3s bench list` and `a3s bench info` are offline after the
control component is installed, but their very first use may need network
access to install that component. They never install Box, start an Agent,
resolve credentials, or create project state.

Installing Bench does not install another Agent Runtime. At execution time the
control component calls the shared A3S OS Runtime. Provider selection order is:
an explicit operator choice in `.a3s/config.acl`; otherwise the authenticated
A3S OS Runtime policy when signed in; otherwise the local Docker provider. An
explicit `a3s-box` choice follows normal Box installation and readiness rules.
The selected provider is preflighted and sealed into the plan; an unavailable
provider fails explicitly and never falls back to Docker or direct host
execution.

The initial typed local selector is:

~~~acl
runtime {
  provider = "a3s-box"
}
~~~

`provider = "docker"` explicitly selects Docker; omitting the block has the
same provider result only in the no-login/no-policy case. Provider-specific
options will use typed nested blocks when their shared Runtime contracts are
implemented; executable paths and shell commands are not accepted.

The development component already delegates this precedence and defaulting
rule to `a3s-runtime`: Bench parses the operator's ACL block into a typed
`ProviderId`, while the shared resolver chooses operator config, authenticated
session policy, or the signed-out Docker default in that order. This removes a
Bench-owned copy of provider-selection semantics. Execution preflight and the
current Docker development path still need migration behind
`A3sRuntimeClient`; selecting another provider continues to fail explicitly
until that provider implements the complete shared lifecycle contract.

P1 always runs the Bench control component as a local child of the top-level
CLI. A remote Runtime may sit behind `A3sRuntimeClient`, but there is no remote
Bench control plane, Bench service login, or private-component protocol exposed
over the network. Any future remote control plane requires a new authenticated,
tenant-aware, privacy-versioned protocol; CLI syntax alone is not that contract.

The component payload and project state are separate:

~~~text
~/.a3s/components/bench/   user-wide, validated control-component payloads
<project>/.a3s/bench/      locks, runs, evidence, results, and reports
~~~

`A3S_COMPONENTS_DIR` can relocate the user-wide component root. It does not
relocate project evaluation state.

For local component development, build the exact payload layout and verify its
`--component-info` compatibility with the top-level CLI using:

~~~bash
python3 tools/package_component.py
~~~

This produces an unsigned archive and SHA-256 transport checksum under `dist/`.
It is suitable for packaging tests only; publication still requires the signed
release statement and activation checks below.

Local Runtime integration checks are split by cost:

~~~bash
./tools/smoke_local.sh       # fast local/OCI/locked fixtures
./tools/smoke_imported.sh    # real imported work + hidden Judge OCI images
~~~

The imported smoke runs explicitly from its source path and is reported as
`local_unofficial`; it is execution evidence, not official admission.

### Control-component release contract

Each release provides one archive and one signed release statement per
supported target:

~~~text
a3s-bench-<stable-semver>-<darwin-arm64|darwin-x86_64|linux-arm64|linux-x86_64>.tar.gz
a3s-bench-<stable-semver>-<target>.release.json
a3s-bench-<stable-semver>-<target>.release.json.sig
~~~

The detached statement is signed by an A3S component key whose trust root ships
in the top-level `a3s` CLI. It binds the exact component, stable version, target,
CLI protocol, archive SHA-256, canonical extracted payload-tree SHA-256, issue
time, and expiry. HTTPS, a GitHub asset digest, or an adjacent `.sha256` may be
used as an additional transport check but is never sufficient release
authority. Redirects outside the trusted A3S-Lab release repository are
rejected.

The archive contains regular files and directories only, exactly one
`component.json`, and no symbolic links. Its manifest identity must match the
signed release statement:

~~~json
{
  "schema": "a3s.component.v1",
  "component": "bench",
  "version": "1.0.0",
  "target": "linux-x86_64",
  "cli_protocol": "a3s-bench-cli/v1",
  "entrypoint": "bin/a3s-bench",
  "required_files": []
}
~~~

Before extraction, `a3s` verifies the detached signature, trust root, validity
interval, target, protocol, and archive digest. Before activation it checks the
canonical payload-tree digest, safe paths and file types, manifest identity,
host target, executable permissions, and the private component probe:

~~~bash
bin/a3s-bench --component-info --json
~~~

The probe must return the same component, version, target, and CLI protocol as
the signed statement and manifest. A successful probe is compatibility
evidence, not authenticity. Activation is version-isolated and atomic; the
private component cannot replace or extend the CLI's trust roots. Statement
expiry prevents new activation. On every launch the top-level CLI rechecks the
stored signed statement, active payload-tree digest, and its signed revocation
policy; a top-level revocation prevents execution of an already installed
component.

## Project State

Every implicit Bench-owned project record and ArtifactStore pin lives under the
A3S project root:

~~~text
<project>/.a3s/bench/
  locks/       immutable TaskLock and CandidateLock indexes
  runs/        durable run state and operation journals
  results/     validated typed results and score projections
  reports/     human-readable report projections
  cache/       rebuildable public catalog metadata and derived indexes
  latest.json  project-local latest-run pointer
~~~

The shared A3S project discovery determines `<project>` before resolving the
task. Passing `./some/task` never changes the state root to that task directory.
`list` and `info` are read-only; `run` creates `.a3s/bench/` when needed.

There is no public option for selecting another implicit state root. P1 permits
`--out` only on `result`, `advanced task lock`, and `advanced candidate lock`;
there is no generic artifact or evidence export. Exports never change the state
root, and locks, attempts, evidence, results, and reports remain indexed under
the same project `.a3s/bench/` directory.

Only `.a3s/bench/` is the stable storage contract. The tree above illustrates
the current roles; subdirectories, filenames, database schemas, and blob
layouts are implementation-owned and may migrate atomically. `result` is the
stable way to locate a run; scripts should not infer run status by traversing
internal files.

This contract covers Bench-owned project records and ArtifactStore pins. The
Runtime and shared ArtifactStore may keep operator-configured VM, image, and
encrypted content state under their own global roots; those locations are not
Bench state, selectors, or public APIs. Bench stores immutable,
authorization-scoped ArtifactRefs and pin intent, while ArtifactStore alone
owns bytes, authorization, retention, and garbage collection. Task ACL cannot
choose storage, privacy class, retention, or deletion.

Bench points its embedded `a3s-flow` engine stores below `.a3s/bench/`; it never
accepts a3s-flow's standalone `.a3s/flow/` default for benchmark work. This
storage is internal run-orchestration state, not a stored or published Flow
Asset.

The state directory is excluded from candidate submissions, TaskBundles, and
Agent Asset snapshots. Bench never copies built-in payloads or private Artifact
bytes into its cache.

Bench-created state directories and private records are owner-only. Mutations
use no-follow, identity-checked, exclusive and atomic filesystem operations. A
symlink, hardlinked mutable record, ownership/mode mismatch, or swapped path
component fails closed; Bench does not silently repair permissive existing
state.

## Result Behavior

A successful foreground run ends with a compact result:

~~~text
COMPLETED  score=10000/10000  task=smoke_answer
run:    01J...
report: .a3s/bench/reports/01J.../index.html
~~~

`COMPLETED` means a valid score was committed and the Task defines no pass
gate. Tasks with locked gates print `PASS` or `FAIL`; infrastructure,
Candidate, and Judge contract failures print `ERROR` and never invent a score.

Show it again with:

~~~bash
a3s bench result            # latest run in this project
a3s bench result 01J...     # exact run ID
a3s bench result 01J... --json
~~~

`a3s bench result 01J... --out ./public-result.json` is the only P1 result export
and writes exactly one canonical UTF-8 `a3s.bench.public-result.v1` JSON object;
format is not inferred from the filename. Here `public` means safe to export,
not published or globally readable. Export uses atomic exclusive creation,
owner-only permissions, rejects symlinks and existing destinations, and never
contains Judge diagnostics, hidden data, private logs, credentials, temporary
URLs, TerminalCheckpoint bytes, or raw Runtime evidence. Active or reconciling
runs have no exportable result; `--out` fails until a terminal projection is
committed.

If a run is still active, `result` shows its current stage and progress rather
than inventing a partial score. Failed infrastructure or Judge executions are
reported as typed failures and are never converted into a candidate score.
`result` may reconcile the same durable Runtime operation and commit its
terminal fact; it never creates a run or Runtime operation, resolves a selector,
or re-runs Judge. `latest` is the most recently committed run creation, not the
run that finished last.

For a foreground run, the first `Ctrl-C` requests durable cancellation, prints
the run ID, and waits while Bench's embedded orchestrator and Runtime drive the
execution to a terminal state. A second `Ctrl-C` detaches the client without
claiming cancellation has finished. If the client disappears abruptly, the run
is not duplicated: reopen it with `result`, or use
`advanced cancel <run-id>` when cancellation is still required.

Reports identify the task, candidate and Judge snapshots, effective models,
budgets, resource cohort, admission/conformance state, metrics, verdict, and
evidence availability. They never expose private evidence/artifact digests,
private bundles, hidden test names, raw Judge output, credentials, temporary
URLs, or private asset bytes.
P1 report HTML is inert and local: no scripts, forms, remote URLs, external
assets, active content, or private-diagnostic retrieval surface.

### JSON and exit codes

`--json` emits exactly one closed-schema `a3s.bench.output.v1` object on stdout;
unregistered fields and enum values require a new schema ID and are invalid in
v1. Progress stays on stderr. A valid score with no gate or a `PASS` exits `0`;
a valid gate-level `FAIL` exits `10`, so simple CI works without confusing
failure with an infrastructure error. Other stable nonzero classes are `2` validation, `3`
unavailable/admission, `4` infrastructure, `5` unscored Candidate failure, `6`
Judge contract failure, and `130` user interruption (terminal cancellation or
forced detach is distinguished in the result state). See the
[canonical design](docs/design.md#124-machine-output-and-exit-status) for the
required fields. Run/result JSON includes `governance_status`, the admission
digest only for official runs, and `evidence_availability`; public output omits
private artifact digests and all Judge diagnostics.

## Authoring a Task

Task authors use the same `run` path as everyone else; local references simply
start with `./`:

~~~bash
a3s bench run ./my_task --agent codex
~~~

The conventional TaskBundle layout is:

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

The candidate receives only `public/`. The Judge is resolved independently and
the terminal hidden bundle is mounted read-only only into its protected Runtime
execution. An authored or admitted Task that needs no hidden data may omit
`private/bundle/`; compilation locks the canonical empty-tree digest, so Git
does not need to preserve an empty directory. A quarantined import with a
missing bundle is different: provenance must mark it unavailable, and admission
must never reinterpret it as an intentional empty bundle.

Before parsing a local bundle, Bench atomically captures one
TaskSourceSnapshot covering Task ACL, public inputs, local Judge package, and
hidden bundle. If included content changes during capture, the command fails
with `source_changed`; it never compiles a torn mix or silently retries into a
different TaskRevision.

Do not place hidden tests, expected answers, or evaluator secrets inside an
OS-hosted Judge Asset; assets are distribution units, not the hidden-data plane.

A small task can be written as:

~~~acl
bench "smoke_answer" {
  schema  = "a3s-bench/task/v1"
  version = "0.1.0"

  name        = "Smoke answer task"
  category    = "correctness"
  description = "Change answer.txt to the requested value."

  work {
    image {
      ref = "docker.io/library/alpine:3.20"
    }
  }

  judge {
    asset = "private/judge"
  }

  metric "correctness" {
    type                   = "ratio"
    role                   = "primary"
    direction              = "maximize"
    min                    = 0
    max                    = 1
    normalization          = "linear_range_v1"
    solution_failure_value = "0"
    public_report          = true
  }
}
~~~

See the [Task ACL Quick Reference](docs/task-spec-acl.md), the
[minimal smoke fixture](examples/smoke/README.md), and the
[performance fixture](examples/a3s-box-pnpm/README.md) for complete fields and
examples.

## Judge Agent Asset Contract

`judge.asset` accepts local, OCI, and A3S OS Agent Asset references:

| Form | Meaning |
| --- | --- |
| `private/judge` | Package-local A3S Agent Asset. |
| `asset:judge-name` | Exact name in the signed-in user's OS scope. |
| `asset:owner/judge-name` | Exact OS owner and asset name. |
| `asset://<uuid>/<ref>` | Explicit OS asset and branch, tag, or commit. |
| `https://<configured-os>/.../assets/<uuid>` | Pasteable Asset URL on the configured A3S OS origin. |
| `oci://<registry>/<repository>:<tag>` or `@sha256:<digest>` | Agent Asset package from any OCI-compatible registry. |

The package uses `version = "a3s.asset.v1"` and `category = "agent"`. Its
standard capability block declares the typed Judge contract:

~~~acl
capability "bench.judge.v1" {
  input_schema = "bench.judge.request.v1"
  output_schema = "bench.judge.result.v1"
  network = "none"
  model_gateway = "none"
}
~~~

The P1 Judge is deterministic. A model-based Judge declares
`model_gateway = "scoped"` and a concrete model through its ordinary Agent
Asset contract, and becomes runnable when that shared Runtime capability is
admitted. It still uses the same product and execution path. Entrypoint, model,
tools, runtime, dependency closure, network, secrets, and resources are handled
and policy-checked as standard Agent Asset content and locked into the TaskLock;
Bench does not replace the Asset's normal execution contract with a private
execution path.

Runtime always treats Judge code as containment-untrusted. Its measurement is
authoritative only because the local Task author selected that exact locked
snapshot, or because an official built-in carries a valid A3S admission record.
Bench verifies identity, protocol, isolation, and score projection; it does not
certify that Judge logic is unbiased, scientifically valid, or semantically
correct.

The A3S OS Runtime starts the locked Judge snapshot. It provides the
SubmissionSnapshot derived from the Candidate-private terminal checkpoint and
the locked hidden bundle through distinct protected read-only mounts. The full
checkpoint is never mounted. Runtime accepts the verdict only through the
protected typed JudgeResult channel. Bench validates that result against the
locked metric schema before scoring.

A Judge Asset cannot obtain candidate credentials or arbitrary control-plane
state. The candidate cannot see the Judge package, hidden mount, result
capability, or Judge output. A quarantined Judge is rejected before
image pull, credential resolution, model reservation, or other billable work.

## Locks

Every new `run` resolves its task, work image, candidate, and Judge references
once and immediately converts them to content-addressed locks. Equal content
reuses the same immutable records; changed local content or a moved mutable OS
reference creates a new revision. Retries and resumed operations always use the
run's existing locks and never follow the original selectors again.

Use `--locked` when resolution must not contact a registry or A3S OS. It accepts
only explicitly immutable inputs whose referenced bytes already exist locally:

~~~bash
a3s bench run ./my_task/task.lock.json \
  --agent ./reviewer.candidate.lock.json \
  --locked
~~~

Bare built-in IDs, embedded aliases such as `codex`, local TaskBundle
directories, OCI references, `asset:name`, revision/branch/tag references,
pasted Asset URLs, and heuristic "only cached match" selection are rejected.
Missing digest-pinned content is an offline-unavailable error. There is no
refresh mode, implicit cache fallback, or network recovery under `--locked`.
Offline starts before control-component activation: Bench, the selected Runtime
provider, trust/revocation material, and every authorized artifact must already
be installed locally. `--locked` never lazily installs or updates Bench/Box,
refreshes a credential, or follows a locator from a lock. A copied lock grants
no artifact access, and matching bytes without current tenant/privacy-class
authorization are unavailable.
The explicit TaskLock envelope declares exactly one governance status. An
`official` lock must carry a valid admission signer chain and signed revocation
snapshot; a `local_unofficial` lock must carry no admission claim. Missing,
invalid, expired, stale, or contradictory material fails. Bench never consults
the catalog, refreshes revocation state, or silently downgrades or promotes
status under `--locked`.

## Safe Local Defaults

The common `run` path compiles one reportable, content-addressed protocol:

| Setting | P1 behavior |
| --- | --- |
| Trials | One. |
| Candidate capture | One private Runtime-owned terminal checkpoint plus one locked-policy SubmissionSnapshot. |
| Judge | One logical terminal Judge execution. |
| Score | One direct endpoint projection. |
| Retries | No speculative or best-of retry; Bench's internal orchestrator may reattach the same idempotent Runtime operation. |
| Feedback and trajectory | None. |
| Hidden seed | None. |
| Limits | Hard time, resource, token, tool, output, and model-cost grants locked before execution. |

Before starting, Bench prints every effective hard limit and the maximum
billable exposure. The exact limits come from the locked task, Agent Assets,
and operator policy; they are protocol ceilings, not estimates.

An internal retry may recompute a pure step or repeat a transport call with the
same operation ID. It cannot create another logical Candidate/Judge execution,
change an input, or choose a better attempt. A second explicit user `run` is a
new Experiment, never an automatic retry of the first.

## Advanced

Most users never need the Advanced namespace. P1 keeps only authoring,
readiness, lock export, and exceptional cancellation there:

~~~bash
# scaffold and validate authoring sources
a3s bench advanced init my_task
a3s bench advanced check ./my_task
a3s bench advanced doctor

# export an immutable task input
a3s bench advanced task lock ./my_task --out ./my_task/task.lock.json

# export an immutable candidate input
a3s bench advanced candidate lock ./agents/reviewer \
  --out ./reviewer.candidate.lock.json

# cancel a detached or interrupted run
a3s bench advanced cancel <run-id>
~~~

The forms above are exact. `init`, `check`, `doctor`, and `cancel` accept no
Bench-specific options; both lock commands require one `--out`, and only
`candidate lock` additionally accepts fill-only `--model`. `check` accepts only
a local authored bundle/root ACL and performs no network, credential lookup,
external resolution, pull, installation, project-state write, or lock write;
remote references are syntax-checked and left for `task lock`. Lock commands
reject an already exported lock as input; they do not wrap locks inside locks.

Advanced commands use the same compiler, locks, A3S OS Runtime, result shape,
and project `.a3s/bench/` root. They do not enable an alternative execution
architecture or state layout. `result` already reports active status, and the
embedded orchestrator resumes durable work automatically, so there are no
duplicate `status` or manual `resume` commands. This mechanism does not accept
or create Flow Assets. It also cannot replace a Judge, select a provider, weaken
validation, expose private artifacts, promote quarantine, or mark a result
official. The displayed commands are the closed P1 Advanced set; suites,
campaigns, leaderboards, raw artifact export, and arbitrary status mutation are
absent until their schemas and semantics are admitted.

## Security and Reproducibility

- Candidate and Judge selectors resolve to canonical immutable AssetSnapshots.
- The A3S OS Runtime isolates every role and attempt.
- The candidate receives public task input and scoped capabilities, never raw
  provider, OS, registry, or storage credentials.
- Judge assets and hidden bundles never enter the candidate execution.
- Candidate Agent Asset/controller source, credentials, private logs, and full
  TerminalCheckpoint never enter Judge. Only the Runtime-derived
  SubmissionSnapshot selected by TaskLock does.
- Hidden bundles remain separate from OS-hosted Judge assets.
- The protected JudgeResult channel, not stdout or an arbitrary file, is
  authoritative.
- Checkpoints are captured by the Runtime, not uploaded authoritatively by the
  candidate.
- SubmissionSnapshot is derived by Runtime from the private checkpoint; Bench
  and Candidate cannot supply its bytes, path, digest, or ArtifactRef.
- Judge output is never returned to the candidate execution.
- Quarantined sources fail before pulls, credential lookup, model reservation,
  or billable work.
- Official execution never silently falls back to a weaker host executor.
- `network = "none"` denies DNS, loopback, link-local/metadata, host bridge
  services, inherited/new IP sockets, raw sockets, proxy inheritance, and
  unsealed Unix sockets. Typed Runtime capabilities expose no general socket.

## Common Diagnostics

| Diagnostic | What to do |
| --- | --- |
| Unknown bare task ID | Run `a3s bench list`; use `./path` for a local task. |
| Task appears only with `--all` | It is quarantined and cannot run until admitted. |
| Ambiguous `asset:name` | Use `asset:owner/name` or `asset://<uuid>/<commit>`. |
| Asset has no concrete model | Add an allowed `--model <provider/model>`. |
| Selector used under `--locked` | Supply an explicit exported TaskLock and CandidateLock; built-ins, embedded aliases, paths, OCI/Asset references, and cached selector matching are forbidden. |
| Locked artifact is unavailable | Materialize the exact digest before offline execution or run unlocked while its source is reachable; `--locked` never fetches. |
| A3S OS is unavailable during resolution | Sign in, or use explicit exported locks whose bytes are already local; there is no cached-selector fallback. |
| Runtime capability is missing | Configure the named A3S OS Runtime capability; there is no silent fallback. |
| JudgeResult is invalid | Fix or re-admit the Judge Asset; it is not converted into a candidate score. |

Related references:

- [Canonical Design](docs/design.md)
- [Task ACL Quick Reference](docs/task-spec-acl.md)
- [Built-in Tasks](builtin/README.md)
- [Minimal Smoke Task](examples/smoke/README.md)
