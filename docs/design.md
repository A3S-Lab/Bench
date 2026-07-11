# a3s-bench Design

Status: canonical

This document defines the only a3s-bench architecture and user contract. Schema
versions are compatibility guards; they do not select an alternative product
mode.

## Executive Summary

a3s-bench is a benchmark control component, not an Agent runtime. It owns the
meaning of an evaluation: Task compilation, immutable identity, Trial planning,
Judge selection, score validation, persistence, and reporting. A3S OS Runtime
owns all Candidate-adapter and Judge execution.

The design has eight foundational rules:

1. `a3s bench` is the only public entrypoint. There is no public `a3s-bench`
   executable, second account, or second configuration tree.
2. Candidate is the product-neutral system being evaluated: a coding agent,
   another automated system, or a deterministic tool. A Candidate adapter and
   the Judge are packaged through the standard immutable Asset contract. The
   task owns its Judge; an entrant cannot replace it, and the Candidate need
   not be implemented with A3S.
3. Bench imports one shared A3S OS execution API: `A3sRuntimeClient`. Candidate
   adapters and Judges use the same `RuntimeExecutionSpec` and
   `RuntimeExecutionResult`, distinguished only by a locked role and capability
   policy.
4. Runtime provider selection belongs to the shared A3S OS Runtime. An explicit
   operator selection in `.a3s/config.acl` wins and may name any conforming
   provider, including `a3s-box`. Otherwise authenticated OS policy applies
   when signed in; with no session, the local Docker provider is the default.
   Bench has no provider-specific adapters and calls neither Docker nor Box.
5. A Judge is a standard Agent Asset that declares the generic
   `bench.judge.v1` capability. Its normal entrypoint, model, tools, and runtime
   remain meaningful; Bench does not extract a private handler or invent a
   second Judge runtime format.
6. Runtime captures a Candidate-private terminal checkpoint, then derives a
   separate immutable SubmissionSnapshot using the locked submission policy.
   Judge-private bytes and the SubmissionSnapshot enter Judge only through
   distinct protected read-only mounts. A typed protected result channel,
   never stdout, returns `bench.judge.result.v1`.
7. Bench links the `a3s-flow` engine as a private implementation library for
   durable orchestration. It does not require, accept, create, resolve, or run a
   Flow Asset. A3S OS Runtime owns execution resources, and BenchStore owns
   benchmark records. Bench does not duplicate queues, leases, fencing,
   retries, reapers, or workspace lifecycle.
8. Each P1 run is deliberately one Task, one Candidate execution, one private
   Runtime-owned terminal checkpoint, one derived SubmissionSnapshot, one Judge
   execution, and one result. Every built-in marked locally available in a
   release must be runnable and out of the box. Official admission is an
   independent governance property. The catalog is revisioned and may grow or
   change independently of the current provisional import snapshot. Suites,
   trajectory evaluation, campaigns, statistics, and
   leaderboards come only after this per-Task vertical slice is proven.

The common path is:

~~~bash
a3s bench list
a3s bench run ./examples/smoke --agent a3s-code --model openai/example
~~~

An admitted built-in uses its bare Task ID. A local path is always explicit:

~~~bash
a3s bench run ./tasks/smoke --agent ./agents/reviewer
~~~

The completed `run` prints the verdict, score, closed-code public diagnostics,
and report location. `a3s bench result` reopens the latest result; users do not
need a second command to finish a normal evaluation.

## 1. Scope and Ownership

### 1.1 Bench control component

The Bench control component owns:

- strict Task parsing and compilation;
- built-in Task discovery and admission state;
- Candidate and Judge Asset resolution to immutable snapshots;
- TaskLock and ExperimentPlan identity;
- the single-Trial evaluation workflow;
- Judge result validation and deterministic score computation;
- immutable benchmark records and public/private projections;
- terminal output and reports.

It may be installed lazily by the main CLI. The downloaded payload is a private
control component under `~/.a3s/components/bench/`; it is not added to `PATH` and
must not be described as a Bench Runtime.

The top-level `a3s` CLI activates a component only after verifying a detached
release statement signed by an A3S component trust root embedded in that CLI.
The statement binds component, stable version, target, CLI protocol, archive
digest, canonical payload-tree digest, and validity interval. HTTPS and an
adjacent checksum provide transport integrity only and are not release
authority. The private component cannot update its own trust root, install a
different component, or treat a successful probe as a substitute for signature
verification. The validity interval governs new activation. On every launch the
CLI rechecks the stored signed statement, active payload-tree digest, and its
own signed component-revocation policy; an already activated payload does not
become unauthenticated merely because its release statement later expires, but
a top-level revocation prevents execution.

### 1.2 A3S OS Runtime

A3S OS Runtime owns:

- materializing and running an immutable Agent Asset snapshot;
- workspace creation, protected mounts, scratch space, and atomic checkpoints;
- deriving an immutable SubmissionSnapshot from a terminal checkpoint under a
  locked submission policy;
- isolation, resources, processes, network, and credential boundaries;
- scoped ModelGateway capability issuance and usage accounting;
- execution idempotency, resource leases, fencing, termination, and cleanup;
- provider selection and provider-specific lifecycle;
- protected typed result collection;
- runtime evidence and attestations.

Docker and A3S Box implement local Runtime providers; Kubernetes or a remote
provider implements the same Runtime contract. Bench neither imports those
providers nor branches on them. The shared Runtime client resolves explicit
operator configuration first, authenticated OS policy second, and the no-login
Docker default last. Once selected, provider unavailability is a preflight
failure rather than permission to fall back to another provider.

The execution client and its request/result types are owned by the shared A3S
OS platform API. Bench imports that public client; it does not define a
Bench-local Runtime port and does not wrap the CLI-private `runtime` tool.

### 1.3 Embedded a3s-flow engine, not a Flow Asset

Bench uses the `a3s-flow` engine as internal orchestration infrastructure. A
benchmark run is not an A3S Flow Asset or an authored workflow package. Bench
MUST NOT resolve, publish, import, or execute Flow Assets; TaskBundle and
ExperimentPlan MUST NOT reference one; and the CLI MUST NOT expose `--flow` or
any other Flow Asset selector.

The dependency is code-only: the Bench release contains or links the exact
compatible engine implementation. Installation and execution MUST NOT contact a
Flow registry, inspect the user's Flow catalog, require a Flow login, load a
Flow manifest, or read `.a3s/flow/`. No Flow identifier, revision, digest,
credential, environment variable, project setting, or user-authored workflow is
an input to a Bench command. Absence of the Flow product, Flow Assets, or Flow
account state cannot make an otherwise ready Bench run unavailable.

The workflow definition that coordinates a run is versioned implementation
code shipped with the Bench control component. It is not user input, does not
enter TaskLock or ExperimentPlan identity, and cannot change the locked
evaluation semantics. Its durable history records orchestration progress only.

The control component records its own build/protocol version as operational
evidence so an interrupted run can reject an incompatible implementation and
remain reconciling. That version is not a selectable workflow revision and does
not become a Flow Asset identity. An upgrade may change scheduling mechanics
only; it MUST NOT re-resolve locks, alter grants or budgets, create another
logical execution, reinterpret a terminal fact, or change scoring. If durable
state cannot be resumed with those guarantees, activation fails closed for that
run instead of migrating semantics or starting over.

Within this internal integration, the a3s-flow engine owns:

- durable workflow state;
- step scheduling, timers, signals, and cancellation;
- workflow retry policy and attempt accounting;
- worker queues, worker leases, and stale-worker recovery;
- resuming the workflow after a process restart.

If the engine lacks a generic orchestration primitive, that primitive is added
to a3s-flow. It is not copied into Bench.

P0 specifically requires a generic external-operation activity that persists
an operation ID and Runtime handle, suspends without holding a worker lease,
reattaches after restart, and runs an idempotent cancellation finalizer. The
current a3s-flow cancel event and synchronous task lease alone are not
sufficient for that contract.

The engine is never an authority for benchmark-domain data. Its retry counters,
task payloads, events, timers, and completion values cannot establish Asset or
Task identity, Runtime completion, Trial disposition, JudgeResult validity, or
score. BenchStore and Runtime facts remain authoritative as specified below.

### 1.4 BenchStore

BenchStore owns transactional benchmark domain records:

- compiled locks and plans;
- Experiment and Trial disposition;
- references to Runtime executions and immutable artifacts;
- validated JudgeResult, score, and report projections;
- privacy labels and provenance.

BenchStore does not own worker queues, execution leases, resource fencing,
provider cleanup, or a general BlobStore. Artifact bytes belong to the shared
A3S artifact service; BenchStore commits immutable, authorization-scoped
ArtifactRefs.

### 1.5 Closed ownership matrix

The following ownership is normative. An implementation MUST NOT move a
responsibility across these boundaries for convenience:

| Concern | Sole authority | Explicitly not authoritative |
| --- | --- | --- |
| Task/Judge/Candidate semantic identity and score | Bench domain and BenchStore | CLI, Runtime provider, report renderer |
| Asset package parsing and AssetSnapshot identity | shared A3S Asset schema and resolver | Bench-local parser, Asset Center locator metadata |
| execution acceptance, isolation, checkpoint, submission projection, usage, cleanup | A3S OS Runtime | Bench, Judge, Candidate, a3s-flow |
| durable step intent and reattachment | Bench's embedded a3s-flow engine | Flow Asset, Bench-owned queue, Runtime provider |
| immutable artifact bytes, authorization, pins, retention, garbage collection | shared A3S ArtifactStore | BenchStore, Task ACL, report output |
| admission of official built-ins | signed A3S admission records | Task author, local CLI flag, Judge manifest |
| placement and denial policy | operator policy through Runtime | Task author, Candidate, Judge |
| public CLI parsing and component activation | top-level `a3s` CLI | private Bench entrypoint as a second product |

### 1.6 Closed dependency boundary

P1 has exactly these dependency directions:

| Caller | Allowed dependency | Forbidden interpretation |
| --- | --- | --- |
| top-level `a3s` CLI | activate the signed private Bench component | public `a3s-bench` product or remotely callable private protocol |
| Bench domain | shared Asset resolver, `A3sRuntimeClient`, ArtifactStore, BenchStore, embedded `a3s-flow` engine | provider adapter, Agent executor, blob store, Flow Asset client |
| embedded `a3s-flow` engine | schedule Bench-supplied internal activities and persist opaque orchestration state | choose Tasks, Assets, models, providers, retries of logical Trials, dispositions, or scores |
| `A3sRuntimeClient` | execute sealed role specs and return durable Runtime facts | parse Task ACL, choose Judge, score, or promote admission |
| local Runtime provider | Docker, A3S Box, or another configured provider behind the shared Runtime contract | direct provider API use or provider-specific execution logic in Bench |

No dependency may be reached by fallback through another row. In particular,
Bench MUST NOT use Flow Asset APIs because the engine is present, call Docker or
Box directly because Runtime selected that provider, or expose its private
component protocol because Runtime may be remote. A missing required shared
contract is a preflight failure, not permission to cross an ownership boundary.

Candidate and Judge code are containment-untrusted. A locked Judge is
measurement-authoritative only because the Task owner selected it; an official
built-in additionally requires A3S admission. Bench proves identity, protocol,
isolation, and deterministic score projection. It does not prove that Judge
logic is scientifically or semantically correct.

## 2. Non-Goals

- Do not build a second Agent runtime, container scheduler, workspace backend,
  model gateway, object store, or credential system.
- Do not execute an authored Dockerfile, image build, package installation,
  Task hook, or dependency hook while compiling or locking. P1 work images are
  OCI inputs resolved to immutable manifests, not Bench build programs.
- Do not define `AgentRuntimeBackend`, `WorkspaceBackend`, `JudgeExecutor`,
  `ModelGatewayAdapter`, `BoxAdapter`, or `KubernetesAdapter` ports in Bench.
- Do not interpret a Judge Asset as a Bench-specific Python handler, shell
  command, image entrypoint override, or private runtime index. A generic
  function entrypoint is valid only when it is part of the shared Agent Asset
  and A3S OS Runtime contract.
- Do not expose Judge-private data, protected results, raw credentials, or
  provider handles to the Candidate.
- Do not mount a full TerminalCheckpoint into Judge. Judge receives only the
  Runtime-derived SubmissionSnapshot selected by the locked submission policy.
- Do not let a task author weaken operator or Runtime security policy.
- Do not treat a mutable tag, selector, Task ID, model alias, directory path, or
  provider name as reproducible identity.
- Do not silently fall back to host execution or a weaker provider.
- Do not model a benchmark run as a Flow Asset or add Flow Asset discovery,
  authoring, publishing, selection, or execution to Bench.
- Do not inherit ambient user models, prompts, memory, MCP servers, tools,
  sessions, shell configuration, environment variables, or current-directory
  files into a sealed evaluation.
- Do not allow Advanced commands, local metadata, or operator overrides to
  promote a quarantined source or manufacture an official result.
- Do not implement Suite, Campaign, leaderboard, periodic checkpoint, dev Judge,
  or distributed scheduling semantics in the first vertical slice.

## 3. Normative Security and Reproducibility Invariants

Every conforming execution MUST satisfy all of the following:

1. Runtime consumes a sealed `RuntimeExecutionSpec`, never authored ACL.
2. Local TaskBundle input is atomically captured as one TaskSourceSnapshot
   before compilation. A source change during capture fails the run; compilation
   never accepts a torn mix of file generations.
3. Candidate adapters and Judges execute from immutable, content-addressed
   Asset snapshots.
4. A TaskLock fixes the Judge snapshot and its `bench.judge.v1` capability. No
   CLI option can override the Judge.
5. Candidate receives only public Task inputs. It never receives the Judge
   Asset, hidden bundle, Judge request, result channel, or Judge logs.
6. Candidate cannot upload an authoritative checkpoint or submission. Runtime
   captures the terminal workspace atomically as a Candidate-private
   TerminalCheckpoint and derives a separate SubmissionSnapshot using the locked
   include/exclude and size policy.
7. Judge receives only that SubmissionSnapshot read-only, the hidden bundle
   through a separate protected read-only mount, bounded scratch, and no
   Candidate capability. It never receives the full TerminalCheckpoint,
   Candidate adapter package, controller state, credentials, or private logs.
8. Judge output is accepted only from the protected typed result channel.
   stdout, stderr, exit text, and files in ordinary workspace are never result
   protocols.
9. Candidate and Judge code are containment-untrusted. A Judge result is
   measurement-authoritative only under the Task's locked trust decision and,
   for an official built-in, a valid admission record.
10. No A3S OS token, registry credential, artifact credential, raw model-provider
   credential, or operator secret enters either Agent sandbox.
11. General network is denied for the P1 Candidate and Judge profiles. Model
    access, when allowed, is a distinct trial-scoped ModelGateway capability.
12. A deterministic Judge receives no ModelGateway capability. A model Judge
    receives only the model, route, budget, and operation scope locked by the
    plan.
13. Candidate capabilities are exactly the intersection of Asset requests,
    Task limits, and operator allowance. `--model` may fill an unbound Candidate
    model but may not replace an Asset-bound model. Missing intersections reject
    the plan; there is no substitution or ambient default.
14. Every outcome-affecting input is canonical data or is identified by an
    immutable digest in TaskLock or ExperimentPlan.
15. Infrastructure failure, Candidate failure, Judge contract failure, and a
    valid low score remain distinct outcomes.
16. Operator policy may reject a plan but cannot silently change its benchmark
    semantics.
17. Missing Runtime capabilities fail preflight before image pulls, credential
    issuance, or billable model work whenever discovery can prove the failure.
18. Reports are projections of committed immutable records; opening a report
    never re-runs a Judge.
19. Every Bench-owned implicit project record and artifact pin is under
    `<project>/.a3s/bench/`. Runtime and ArtifactStore may keep their own
    operator-configured global state, which is never a Bench selector or public
    project layout. No command creates `.a3s-bench`, and there is no
    `--state-dir` escape hatch.
20. A public projection means safe to export; it does not publish data or grant
    access. Private artifacts never become public through `result`, `--out`,
    equal content digests, or local filesystem convenience.
21. The P1 command and Bench-specific option sets are closed. Unknown,
    abbreviated, duplicated, or command-inapplicable options are errors; no
    environment variable or config key creates a hidden Bench option.
22. Host environment variables are never copied into Candidate or Judge specs.
    Only the already-selected top-level A3S account/configuration, project root,
    component root, terminal presentation, and explicit command arguments may
    affect control-plane behavior; every outcome-affecting resolved value is
    then locked.
23. `network = "none"` denies routed egress, ingress, DNS, loopback, link-local
    and metadata services, host services, and inherited or newly created IP or
    Unix-domain sockets. A sealed Runtime capability such as ModelGateway or the
    protected result channel is not general network and grants no raw socket.
24. `--locked` is offline from top-level CLI activation through terminal result.
    It never lazily installs or updates Bench, Box, another Runtime provider, or
    an artifact. All compatible component, provider, trust, lock, and artifact
    material must already be installed and locally authorized.
25. An exported lock is identity and governance data, not bearer authority.
    Copying it grants no artifact, tenant, registry, OS, or model access, and
    `--locked` never follows a locator embedded in it to obtain missing bytes.
26. Workflow retries may repeat pure local steps and transport calls only. They
    reuse the same Runtime operation ID and cannot create another logical
    Candidate or Judge execution, choose a better attempt, or re-resolve input.
27. Bench-created state directories and private records are owner-only, are
    created without following symbolic links, and fail closed when any path
    component below `.a3s/bench/` changes identity during an operation.
28. P1 machine-output and public-export schemas are closed allowlists. A new
    field, enum meaning, or privacy projection requires a new schema identifier;
    an implementation cannot add a field to v1 merely because a reader might
    ignore it.
29. This document is the normative P1 protocol. `README.md`, examples, catalog
    metadata, generated help, and the ACL quick reference are explanatory and
    cannot add syntax or relax a rule. A contradiction is a defect;
    implementations fail closed instead of choosing a permissive reading.
30. Acceptance is closed-world at every boundary. Unknown fields, blocks,
    options, enum values, capability names, schema identifiers, media types,
    artifact kinds, result states, and disposition codes are errors, not
    warnings, extensions, or values to preserve and forward.
31. Validation never repairs input. Bench does not trim semantic strings,
    normalize a path supplied in another Unicode form, use a case-folded match,
    coerce a number or Boolean from a string, discard an unknown field, pick one
    duplicate, or replace malformed UTF-8.
32. A failed command commits no success-shaped output. It may durably record an
    already-created Experiment or an externally established Runtime fact, but
    it never publishes a score, advances an immutable lock, moves `latest` to a
    nonexistent run, or leaves a partial `--out` file.
33. The embedded `a3s-flow` dependency is never resolved as data. Bench startup,
    planning, execution, recovery, and result lookup perform no Flow Asset,
    registry, catalog, account, manifest, or `.a3s/flow/` access. Flow state can
    neither enable nor deny a run.
34. Internal workflow state is non-authoritative. It may request or reattach an
    operation using already committed IDs, but only Runtime facts and
    BenchStore's compare-and-commit rules can establish terminal disposition;
    orchestration payloads cannot manufacture or rewrite one.

### 3.1 Closed interpretation rules

All P1 protocol text and data use UTF-8. Input containing invalid UTF-8, NUL,
Unicode control characters other than ACL/JSON whitespace, or a string not in
NFC is invalid. Identifiers are ASCII and case-sensitive. Task IDs, schema IDs,
capability names, metric names, and run IDs are compared byte-for-byte; locale,
Unicode case folding, and filesystem case folding never participate.

Canonical relative paths use `/`, contain NFC UTF-8 segments, and contain no
empty, `.`, or `..` segment, leading `/`, trailing `/`, backslash, NUL, drive
prefix, or URI form. Resolution stays beneath the already-opened bundle or
snapshot root, component by component, without following symlinks. Resolved
entry spelling must exactly equal manifest spelling. Hard links, devices,
FIFOs, sockets, and other special files are rejected unless a schema explicitly
admits that exact type; P1 Task and submission schemas do not.

ACL integers are base-10 ASCII with no sign unless a field explicitly admits a
negative value, no leading zero except `0`, and no exponent or unit suffix.
JSON identity integers must be integral and within the declared field range;
floating-point input is never coerced. Durations use integer seconds where
named `_sec`, byte limits use integer bytes, and scores use integer basis
points. Limits are inclusive unless stated otherwise. Overflow, underflow,
precision loss, NaN, infinity, and out-of-range values are errors.

Digests use lowercase `sha256:` followed by exactly 64 lowercase hexadecimal
digits and identify the canonical bytes named by the containing field.
Uppercase hex, omitted algorithms, alternate encodings, and algorithm
negotiation are invalid in P1. Digest equality never grants trust or authority.

Protocol timestamps are UTC RFC 3339 with exactly `YYYY-MM-DDTHH:MM:SSZ`;
fractional seconds, offsets, leap seconds, and local time are invalid. Durations
and Runtime deadlines use monotonic elapsed time. Wall-clock time is used only
for signed admission/revocation validity and display metadata, never for
ordering, retries, scoring, or timeout measurement. A missing trustworthy clock
makes an official validity check unavailable; it does not assume validity.

Schema support is exact. P1 accepts only schema identifiers and a
RuntimeSemanticsProfile explicitly supported by the installed compatible
control component. There is no range negotiation, unknown-field forwarding,
downgrade, or closest-profile selection. The signed top-level CLI and Bench
component establish protocol compatibility before parsing benchmark input or
creating project state.

## 4. User Contract

### 4.1 Task references

Task references have one unambiguous grammar:

| Form | Meaning |
| --- | --- |
| `<task-id>` | Exact globally unique built-in Task ID. |
| `./path` or `../path` | Local TaskBundle directory, `task.acl`, or exported TaskLock. |
| `oci://...@sha256:<digest>` | Advanced, immutable published TaskBundle. |

A bare name never searches the filesystem. A path without `./` or `../` is
rejected with a correction. Built-ins have no `builtin:` prefix and no
source-specific namespace. Upstream names such as EdgeBench appear only in
provenance and third-party notices.

Local reference classification uses `lstat` plus content, never a filename
guess: a directory must contain exactly one root `task.acl`; a regular file
named `task.acl` denotes its containing TaskBundle; any other regular file must
be a complete `a3s.bench.task-lock.v1` envelope. Symlinks, special files,
schema-mismatched JSON, and a directory that also attempts to act as a lock are
rejected. A `.json` suffix alone never gives a file lock authority.

Built-in listing is read-only and offline:

~~~bash
a3s bench list
a3s bench info ad_placement_optimization --all
a3s bench list --all   # also show quarantined or unavailable entries
~~~

The default list contains locally available Tasks that the installed control
component schema supports. A locally available Task may run only as
`local_unofficial` unless it also has valid signed admission. `--all` includes
locally blocked records; `run` rejects those before external or billable work.

### 4.2 Candidate references

`--agent` names the Candidate for CLI compatibility. It accepts the standard
Candidate adapter reference family:

~~~bash
a3s bench run ./examples/smoke --agent a3s-code --model openai/example
a3s bench run ./examples/smoke --agent ./agents/reviewer
a3s bench run ./examples/smoke --agent asset:acme/reviewer
a3s bench run ./examples/smoke --agent asset://<asset-id>/<immutable-ref>
a3s bench run ./examples/smoke --agent oci://registry.example/agents/reviewer@sha256:<digest>
a3s bench run ./task.lock.json --agent ./reviewer.candidate.lock.json --locked
~~~

`a3s-code` is an embedded selector that the installed component maps to one
exact immutable Candidate snapshot. The word itself is not identity and a
component update may map a new unlocked run to a new CandidateRevision; every
run records the resolved revision, and `--locked` rejects the alias. Local, OCI, and A3S OS
packages pass through the same shared Asset resolver and produce the same
AssetSnapshot identity when their canonical package trees and semantic
configuration are identical. The adapter currently uses the `a3s.asset.v1`,
`category = "agent"` wire format; this is a packaging and execution contract,
not a restriction on the Candidate implementation.

An installed component may later provide `codex` or `claude` under exactly the
same embedded selector contract once native product adapters exist. These names
are convenience selectors, not product-specific execution modes. Bench resolves
every selector to a normal Candidate adapter and then uses the same Candidate,
Runtime, locking, and result pipeline.

A Codex-versus-Claude Code comparison is two ordinary runs over the same
TaskLock, with one CandidateLock for each exact adapter and model combination.
Distinct adapters compare the complete coding-agent systems. To isolate model
behavior, use the same adapter revision and create CandidateLocks that differ
only in their configured model route. Reports compare the resulting locked run
identities; Bench does not infer experimental equivalence from product names.
Bare selectors are convenient for unlocked exploration, but reproducible
comparisons resolve and save their CandidateLocks first, and `--locked` always
rejects selectors.

A local Candidate reference beginning `./` or `../` is classified by `lstat`
and schema content. A directory must contain the standard root
`.a3s/asset.acl`; a regular file must be a complete
`a3s.bench.candidate-lock.v1` envelope. Symlinks, special files, direct paths to
an internal asset file, and extension-based guessing are rejected. Candidate
lock commands accept an Asset source, while `run --locked` accepts only the
lock; neither command wraps an existing lock into another lock.

An OCI Candidate adapter may name any OCI Distribution-compatible registry;
it is not restricted to Docker Hub or an A3S-operated registry. Its artifact
must be a complete standard `a3s.asset.v1`, `category = "agent"` package. A
plain container image without that package contract is not inferred or wrapped
as a Candidate adapter. An unlocked mutable OCI tag is a source selector only:
the resolver records the exact manifest digest and canonical package-content digest
before CandidateLock construction. Registry authentication is scoped to the
original authority, credentials are never forwarded across an authority change,
and no credential bytes enter AssetSnapshot, CandidateLock, or an Agent
sandbox. `--locked` accepts the exported CandidateLock rather than an OCI
reference, even when that reference is already digest-pinned.

An HTTPS Asset URL is accepted only when its origin exactly matches the
currently configured A3S OS origin. Redirects may not change authority, and an
OS bearer credential is never sent to an origin learned from task content or a
pasted foreign URL. Selecting another OS origin is an explicit top-level A3S
account/configuration operation performed before Bench starts.

`--model` is fill-only. It is accepted only when the Candidate adapter leaves its
model unbound, and the selected model must be allowed by the Asset, Task limits,
and operator policy. It cannot override an Asset-bound model. CandidateLock
also freezes model parameters, prompt/controller configuration, tools, memory
policy, and capability requests. Evaluation never inherits the current
account's default model or ambient Code session, Memory, MCP, tool, shell, or
environment configuration. A3S OS login is control-plane authority for remote
resolution; it is never sandbox authority. Local execution must work without an
A3S OS login. For an explicitly selected or Asset-bound model, the local
Runtime may resolve its provider/model route and credential from the standard
project-local or user-local `.a3s/config.acl`. It must not use that file's
`default_model` as an implicit Candidate choice.

There is no `--judge`, `--flow`, `--runtime`, `--box`, `--kubernetes`,
`--backend`, or `--state-dir` option. Provider configuration belongs to A3S OS
Runtime, the Judge belongs to the Task, and internal orchestration belongs to
Bench rather than a user-selected Flow Asset.

The shared platform configuration schema must expose a typed Runtime provider
selection in `.a3s/config.acl`; this is operator infrastructure configuration,
not a Bench option or Task field. The selector resolves to a provider object and
its declared capabilities, not an executable path or arbitrary shell command.
Its canonical provider identity and RuntimeSemanticsProfile enter preflight and
the ExperimentPlan, while endpoints and credentials remain Runtime-owned.
P1 begins with the closed shape `runtime { provider = "<registered-id>" }` and
built-in IDs `docker` and `a3s-box`; additional implementations extend the
shared typed provider registry, not Bench's command grammar.

Bench reads no `A3S_BENCH_*`, `BENCH_*`, model, prompt, tool, MCP, Memory,
session, shell, proxy, or credential environment variable as benchmark input.
Platform-owned configuration and credentials may be used by the top-level A3S
clients for authorized resolution, but their resolved semantic choices are
sealed in locks and their secret bytes never enter a lock or Agent sandbox.
For the local Runtime, `.a3s/config.acl` is platform/operator configuration,
not Task or Candidate input. The Runtime may use it to map an exact locked
`provider/model` reference to a local or custom inference endpoint. Model
identity, route identity, and outcome-affecting parameters are captured before
execution; API keys, bearer tokens, and equivalent secret material remain
Runtime-only. A changed configuration must produce a new unlocked plan or make
a locked run fail preflight; it must never silently alter a locked run.
`A3S_COMPONENTS_DIR` changes only where the top-level CLI finds private
components. Locale, color, and TTY state change presentation only.

### 4.3 Project root and state

The shared A3S project-root resolver determines `<project>` once at command
start. If no project marker exists, the current working directory is the
project root. A mutating command creates Bench-owned project records and pins
only below:

~~~text
<project>/.a3s/bench/
~~~

Read-only `list`, `info`, and help commands do not create project state. An
explicit command-specific `--out` may export a safe projection or immutable
lock elsewhere; it never changes the state root. P1 has no generic artifact or
evidence export. Export creation is atomic, uses no-follow/exclusive-create
semantics, fails if the destination exists, creates no missing parent
directories, and defaults to owner-only permissions. Exported locks contain
identities and authorization-scoped ArtifactRefs, never hidden bytes,
credentials, temporary URLs, private logs, or provider handles.

P1 permits `--out` only for `result`, `advanced task lock`, and
`advanced candidate lock`. `result --out` writes exactly one canonical UTF-8
`a3s.bench.public-result.v1` JSON object containing the redacted public result
projection; it does not infer format from the filename. `public` means safe to
export, not published or granted to another principal. Lock exports contain
immutable semantic identities and scoped ArtifactRefs but no referenced private
bytes. An active/reconciling run has no exportable result; `result --out` fails
until a terminal public projection is committed.

`latest` is a convenience pointer inside `.a3s/bench/`, while all durable
records use explicit immutable IDs. Subdirectory names are implementation-owned
and are not selectors or stable external APIs.

### 4.4 Primary commands

The normal interface has four commands:

~~~bash
a3s bench list
a3s bench info <task-id-or-./path>
a3s bench run <task-id-or-./path> --agent <candidate-ref>
a3s bench result [run-id] [--out <public-result.json>]
~~~

The Bench-specific P1 grammar is closed:

| Command | Positionals | Bench-specific options |
| --- | --- | --- |
| `list` | none | `--all`, `--json` |
| `info` | exactly one Task reference | `--all` only for a bare catalog ID; `--json` |
| `run` | exactly one Task reference | exactly one `--agent`; optional `--model`, `--locked`, `--json` |
| `result` | zero or one run ID | optional `--out`, `--json` |
| `advanced init` | exactly one creation path | none |
| `advanced check` | exactly one local authored TaskBundle directory or root `task.acl` | none |
| `advanced doctor` | none | none |
| `advanced task lock` | exactly one non-lock Task source | exactly one `--out` |
| `advanced candidate lock` | exactly one non-lock Candidate adapter reference | optional `--model`; exactly one `--out` |
| `advanced cancel` | exactly one run ID | none |

Each singleton option may occur once. Options have only the spellings above;
there are no prefix abbreviations, aliases, positional model/Judge values,
implicit current-task/current-agent values, or option values read from the
environment. Top-level `a3s` presentation and confirmation options remain
top-level and cannot change ExperimentPlan semantics.

Bench parses arguments as an exact token grammar. An option value is the next
token and is never split on `=`, commas, or whitespace; `--option=value`, short
options, combined options, prefix matches, and a Bench-local `--` terminator are
not P1 syntax. A value beginning with `-` is invalid. Local paths that might be
mistaken for options must use an explicit `./` or `../` spelling. Run IDs use
the canonical ASCII grammar emitted by Bench; `latest`, partial IDs,
case-insensitive IDs, and path-shaped IDs are not aliases. Omitting the
`result` positional is the only way to select the project-local latest run.

`run` performs validation, capability preflight, locking, planning, execution,
judging, scoring, persistence, and report generation. It prints resolved Task,
Candidate, Judge, model scopes, hard limits, and maximum billable exposure
before work starts. Interactive confirmation follows shared A3S policy;
non-interactive billable work requires the shared confirmation mechanism.

`result` defaults to the project-local latest run. `run` already prints the
same summary, so `result` is for revisiting a run, not a required second step.
The public `run-id` names the internal Experiment record; `experiment-id` is
never a second CLI term.

`latest` means the run whose Experiment record committed most recently in this
project, ordered by the BenchStore commit sequence rather than wall-clock time
or completion time. Creating a run atomically moves the pointer; completing an
older run does not steal it back. `result` may drive reconciliation of that
same run and commit a Runtime-established terminal fact, but it cannot create a
new Experiment, submit a new logical Runtime operation, re-resolve a selector,
or re-run Judge.

Authoring and interrupted-run operations stay under one Advanced namespace:

~~~bash
a3s bench advanced init my_task
a3s bench advanced check ./my_task
a3s bench advanced doctor
a3s bench advanced task lock ./my_task --out ./task.lock.json
a3s bench advanced candidate lock ./agents/reviewer --out ./candidate.lock.json
a3s bench advanced cancel <run-id>
~~~

| Advanced command | Maximum authority in P1 |
| --- | --- |
| `init` | Exclusively creates a new authored TaskBundle at the explicit path; never overwrites or follows a symlink. |
| `check` | Local source-only validation; no external or control-plane side effects. |
| `doctor` | After normal Bench component activation, probes configured platform readiness without installing Box or another component, resolving Task/Asset selectors, starting Runtime work, or creating project state. |
| `task lock` | Resolves one Task and its owned Judge into TaskLock; no Candidate or Runtime execution. |
| `candidate lock` | Resolves one Candidate and fill-only model binding into CandidateLock; no Task, Judge, or Runtime execution. |
| `cancel` | Signals one existing run ID; cannot create, resume under a new ID, re-plan, re-resolve, or mutate disposition directly. |

This is the closed P1 Advanced command set. Advanced commands use the same
parser, resolver, admission rules, lock compiler, Runtime policy, privacy
classes, and project root as the normal path. They cannot select a provider,
replace a Judge, execute authored commands, weaken validation, expose private
artifacts, promote quarantine, or mark a result official. Suite, Campaign,
leaderboard, raw artifact export, manual resume, and arbitrary status mutation
are absent until a later version defines them explicitly.

`check` success means the captured local authoring closure is portable-valid;
it does not claim that a remote Judge/image/workspace selector is reachable or
admitted. It performs no network, credential lookup or refresh, external
selector resolution, artifact pull, component/provider install, project-state
creation, lock persistence, Runtime work, or model reservation. Remote
references are syntax-checked and reported as unresolved inputs. `advanced task
lock` is the only authoring command that resolves those Task-owned external
inputs and emits a TaskLock. `info` is likewise offline inspection and never
substitutes for either operation.

P1 `--locked` performs no selector resolution, source resolution, registry
access, OS lookup, component-alias expansion, catalog lookup, or heuristic cache
selection. Task must be an explicit exported TaskLock, Candidate must be an
explicit exported CandidateLock, and every referenced artifact must already be
available locally by the digests recorded in those locks.

Offline begins before Bench component activation. If a compatible verified
Bench component, selected Runtime provider, signed trust/revocation material,
or locally authorized artifact is absent, `--locked` fails with
offline-unavailable. It does not invoke lazy component installation, provider
installation, update, registry authentication, credential refresh, or network
recovery. Artifact availability requires both matching bytes and authorization
for the current tenant/privacy class; digest equality alone is insufficient.

A bare built-in ID, embedded alias such as `a3s-code`, local TaskBundle directory,
OCI reference, `asset:name`, branch, tag, pasteable Asset URL, revision selector,
or cached "only match" is rejected under `--locked`. Missing content is an
offline-unavailable error; Bench never resolves, refreshes, or falls back. An
unlocked run or the explicit Advanced lock commands are the only ways to create
new locks.

The explicit TaskLock envelope declares `governance_status`. An `official` lock
must carry the complete valid admission signer chain and signed revocation
snapshot needed for offline verification; a `local_unofficial` lock must not
claim admission. Missing, invalid, or contradictory governance material fails
rather than downgrading or promoting the run. Bench never consults the installed
catalog to change an explicit lock's governance status under `--locked`.
The revocation snapshot must satisfy the admission profile's signed freshness
and validity window at ExperimentPlan commit time. An expired snapshot fails
offline; Bench neither ignores it nor refreshes it under `--locked`.

## 5. Minimal Domain Model

| Term | Meaning |
| --- | --- |
| Candidate | Product-neutral coding agent, automated system, or deterministic tool being evaluated. |
| Candidate adapter | Immutable package that exposes a Candidate through Bench's execution contract; currently encoded with the shared `a3s.asset.v1`, `category = "agent"` wire format. |
| TaskSourceSnapshot | Atomic immutable capture of one authored TaskBundle generation before parsing or external resolution. |
| TaskBundle | Authored Task content split into public inputs, hidden Judge inputs, and a task-owned Judge Asset reference. |
| AssetSnapshot | Immutable canonical snapshot of a shared Asset package used by a Candidate adapter or Judge. |
| TaskLock | Immutable compiled Task semantics, public input digests, hidden input digest, metrics, and locked Judge snapshot/capability. |
| CandidateLock | Immutable Candidate snapshot plus admitted model and parameter bindings. |
| ExperimentPlan | Root commitment to one TaskLock, one CandidateLock, limits, Runtime requirements, and scoring contract. |
| Experiment | User-visible evaluation instance of one ExperimentPlan. |
| Trial | The one logical Candidate-to-Judge evaluation in the P1 Experiment. |
| RuntimeExecutionSpec | Provider-neutral sealed request for one Candidate-adapter or Judge execution. |
| RuntimeExecutionResult | Provider-neutral, identity-bound completion record returned by A3S OS Runtime. |
| TerminalCheckpoint | Candidate-private Runtime-owned immutable snapshot of the complete terminal workspace; never mounted into Judge. |
| SubmissionSnapshot | Runtime-derived immutable projection of TerminalCheckpoint under the locked submission policy; the only Candidate output mounted into Judge. |
| JudgeResult | Typed `bench.judge.result.v1` payload returned through the protected Runtime channel. |
| Score | Deterministic projection of a validated JudgeResult and locked metric contract. |
| Report | Public/private projection of committed Experiment records. |

Task ID, asset name, selector, path, provider, and Experiment display name are
not immutable identity. Canonical content digests are identity.

P1 freezes these machine schema identifiers:

| Object | Schema |
| --- | --- |
| exported TaskLock | `a3s.bench.task-lock.v1` |
| exported CandidateLock | `a3s.bench.candidate-lock.v1` |
| ExperimentPlan | `a3s.bench.experiment-plan.v1` |
| Judge request | `bench.judge.request.v1` |
| Judge result | `bench.judge.result.v1` |
| public result export | `a3s.bench.public-result.v1` |
| built-in admission record | `a3s.bench.admission.v1` |
| component release statement | `a3s.component.release.v1` |

Locks and plans are generated artifacts, never hand-authored ACL. Each file has
one strict schema version, canonical semantic content, its content digest, and a
separate envelope for signatures, provenance, locators, and transport metadata.
Unknown semantic fields are rejected for v1; a new meaning requires a new
schema identifier. Envelope extensions cannot alter identity or grant
authority. A file claiming a digest that does not match its canonical semantic
content is invalid before any external work.

Every exported TaskLock envelope declares exactly one governance status. A
local source produces `local_unofficial`; an admitted built-in may produce
`official` only with the complete verified admission material. A run never
silently downgrades an official input or promotes an unofficial one.

## 6. TaskBundle and TaskLock

### 6.1 Canonical authoring layout

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
      ...ordinary Agent Asset files...
    bundle/
      ...hidden tests and expected data...
~~~

`judge.asset` may instead reference an A3S OS Agent Asset. The hidden bundle
always remains separate from the Agent Asset so publishing a Judge cannot
publish hidden tests. A Task that intentionally needs no hidden bytes may omit
`private/bundle/`; compilation then commits the schema-defined canonical empty
tree digest. A quarantined import with unavailable private inputs is not treated
as an intentionally empty admitted bundle: its admission record must preserve
that incompleteness explicitly.

The public workspace and environment image are separate inputs. The Candidate
receives only the prompt and public workspace. Task authors cannot choose mount
locations, provider names, raw secrets, or host paths.

For a local TaskBundle, Bench first captures one TaskSourceSnapshot. The capture
uses normalized relative paths and stable file handles, rejects unsafe file
types and path collisions, and verifies that no included file changed while the
manifest was read. Any change produces `source_changed`; Bench does not silently
retry into a different revision. Parsing, local Judge Asset resolution, public
workspace ingestion, and hidden-bundle ingestion all consume that captured
generation rather than reopening mutable authored paths independently.

The semantic capture closure is exact: root `task.acl`, the declared or
conventional prompt/workspace, the complete local Judge package when selected,
and the declared or conventional hidden bundle. Other author notes and
repository files are neither captured,
hashed, mounted, nor visible to either Agent; changing only those files does not
create a TaskRevision. Every path reachable from the closure must be a regular
file or directory inside the bundle. P1 rejects all symlinks, hardlinks, special
files, mount crossings, case-colliding names, and path replacement during
capture, even when a link would currently resolve inside the bundle.

### 6.2 Minimal Task ACL

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
    role      = "primary"
    direction = "maximize"
    min       = 0
    max       = 1
  }
}
~~~

Schema defaults define the conventional prompt, public workspace, hidden
bundle, resource limits, terminal timeout, network policy, checkpoint limits,
and metric quantization. Compilation expands all defaults into TaskLock, so a
default cannot change beneath an existing TaskRevision.

### 6.3 Strict compilation

Compilation is a security boundary. It MUST:

1. consume one atomic TaskSourceSnapshot for local authored input;
2. reject unknown, duplicate, dynamic, non-normalized, and out-of-range fields;
3. retain source spans for actionable diagnostics;
4. reject absolute paths, parent traversal, symlink escape, path collisions,
   unsafe file types, and archive expansion abuse;
5. resolve the Judge selector atomically to one immutable AssetSnapshot;
6. require `version = "a3s.asset.v1"`, `category = "agent"`, and exactly one
   `bench.judge.v1` capability;
7. resolve authored image tags and external content to immutable digests;
8. digest public, hidden, and submission-policy inputs separately;
9. validate metrics, score direction, range, and failure mappings;
10. emit canonical TaskLock content independently from provenance and transport
   envelope data;
11. execute no Task hook, Asset hook, package command, or Judge code.

TaskLock includes the Judge AssetSnapshot digest, the canonical
`bench.judge.v1` capability declaration, hidden bundle digest, expected input
and output schemas, submission projection policy, model-access mode, Runtime
requirements, and metric contract. It does not contain hidden bytes or provider
credentials.

Mutable selectors are resolved once for each new non-locked run while creating
its locks. Existing run plans, retry, resume, `--locked`, and result rendering
never re-resolve them. Provenance such as
source URL, owner, upstream commit, license, signatures, and transport digest is
reportable but is not semantic identity unless explicitly required by
admission policy.

### 6.4 Asset snapshot rules

Candidate adapters and Judges use one shared A3S AssetSnapshot implementation.
Snapshot creation:

- reads `.a3s/asset.acl` and the complete declared package atomically;
- normalizes UTF-8 paths, modes, ordering, and file manifests;
- rejects symlinks, hardlinks, devices, sockets, FIFOs, setuid/setgid, unsafe
  xattrs, case collisions, excess depth, excess files, and compression bombs;
- never executes install or build hooks during resolution;
- separates semantic content from locator, authentication, and download data;
- produces the same digest for equivalent local and OS package snapshots.

Remote resolution follows the configured-origin rule in section 4.2. The
resolver rejects authority-changing redirects and never forwards credentials
to a locator-controlled origin. Under `--locked`, the resolver is not called.

The shared Asset schema and resolver belong to reusable A3S platform code.
Bench must not copy CLI/TUI-private JSON parsing or create a Judge-only package
format.

## 7. Judge as a Standard A3S Agent Asset

### 7.1 The only Judge declaration

A Judge is an ordinary `a3s.asset.v1`, `category = "agent"` Asset. It opts into
evaluation with the generic capability block below:

~~~acl
version = "a3s.asset.v1"
category = "agent"
kind = "tool"
name = "example-judge"
description = "Judge for example_task."
service = "Function as a Service"

source {
  package_path    = "."
  entrypoint      = "judge.py:evaluate"
  definition_path = "agent.md"
}

metadata {
  asset_acl_path = ".a3s/asset.acl"
}

runtime {
  kind         = "tool"
  isolation    = "serving"
  runtime_kind = "a3s-function-service"
  protocol     = "agent-tool"
  agent_kind   = "tool"
}

capability "bench.judge.v1" {
  input_schema  = "bench.judge.request.v1"
  output_schema = "bench.judge.result.v1"
  network       = "none"
  model_gateway = "none"
}
~~~

The capability block is part of the common A3S Agent Asset schema. If current
platform tooling cannot parse it, the common schema is extended; Bench does not
create a parallel manifest parser.

The ordinary Agent entrypoint, instructions, model, tools, dependencies, and
runtime are honored by A3S OS Runtime. `judge.py:evaluate` above is a generic
tool-Asset function entrypoint, not a Judge-only ABI. P0 must add this standard
function-entrypoint form to the shared Asset schema and Runtime before the
fixture is executable. There is no `benchmark {}` block, Judge role field,
Bench-owned handler field, SDK ABI, runtime index, fixed Python runner, or
Bench-specific command override.

### 7.2 Deterministic and model Judges

The shared capability schema defines two values for `model_gateway`:

- `none`: deterministic Judge. Runtime refuses all model capability and general
  network access.
- `scoped`: model Judge. The normal Agent Asset declares its model intent; plan
  resolution locks the exact allowed model, route policy, token/cost limits, and
  capability scope. Runtime supplies a trial-scoped ModelGateway capability.

`scoped` never means general egress and never places a provider API key in the
Agent environment. A Judge cannot request a broader scope at runtime. An
operator may reject an unavailable or disallowed route but cannot substitute a
different outcome-affecting model without producing a different plan.

For both roles, `network = "none"` is a deny-all socket policy, not merely an
empty outbound allowlist. Runtime denies DNS, loopback, link-local and metadata
addresses, host/guest bridge services, inherited descriptors, listening
sockets, raw sockets, and Unix-domain sockets outside sealed Runtime-owned
capabilities. ModelGateway, protected mounts, and the protected result channel
are typed capabilities with fixed peers and protocols; they do not expose an
IP address, hostname, arbitrary path, proxy setting, or reusable credential.
Proxy variables and host resolver configuration are cleared rather than
inherited.

P1 admits `none` first. `scoped` uses the same Asset, plan, and Runtime contract
and may be enabled after its shared ModelGateway capability passes conformance;
it does not create another product mode or Judge path. Other network modes,
ordinary runtime secrets, arbitrary external tools, mutable model routing, and
user-provided credentials are not admitted.

Judge trust has two independent dimensions. Runtime treats all Judge code as
containment-untrusted and grants only the sealed Judge-role capabilities. Bench
treats the locked Judge result as measurement-authoritative because the local
Task author selected that exact snapshot, or because an official built-in has a
valid A3S admission record for it. Contract validation cannot certify evaluator
correctness, absence of bias, or scientific validity; those are admission and
Task-governance responsibilities and must not be implied by a successful run.

### 7.3 Judge input

`bench.judge.request.v1` contains only locked, bounded evaluation data:

- ExperimentPlan and Trial identity bindings;
- SubmissionSnapshot digest and read-only submission mount name;
- no TerminalCheckpoint identity, mount, digest, or ArtifactRef; Runtime binds
  checkpoint-to-submission evidence outside the Candidate-visible request;
- hidden bundle digest and protected mount name;
- declared metric schema and scoring-relevant constraints;
- time, resource, output, and ModelGateway budgets;
- deterministic randomness commitment when required;
- the protected result contract identifier.

Runtime supplies the actual mount capabilities. The hidden bundle is fetched
and digest-verified outside the Agent, mounted read-only at a protocol-owned
location, and never represented by a storage URL or bearer credential. The
SubmissionSnapshot is a separate read-only mount derived from the private
TerminalCheckpoint according to TaskLock; the full checkpoint is not mounted.
Scratch is bounded and writable.

The generic function context exposes only protocol-owned
`submission_root`, `hidden_bundle_root`, and `scratch_root` locations inside the
Judge sandbox; Task authors cannot choose those paths.

### 7.4 Judge result

The Judge returns one bounded typed payload through the Runtime protected
result channel:
~~~json
{
  "schema": "bench.judge.result.v1",
  "solution_verdict": "valid",
  "metrics": {
    "correctness": "1"
  },
  "diagnostics": {}
}
~~~

Metric values use canonical integer or decimal strings. Keys must be declared
by TaskLock; values must satisfy locked type and range rules. Size, field count,
string length, nesting, and diagnostic bounds are part of the protocol.

Runtime binds the protected payload to execution ID, spec digest, Judge
AssetSnapshot, input artifact digests, model-usage evidence, and completion
state in `RuntimeExecutionResult`. Bench verifies both the Runtime envelope and
typed payload before scoring.

For the P1 deterministic tool Asset, the ordinary function return value is
captured by the OS function host and written to the protected ResultSink; the
Judge receives no sink credential or writable result path. When structured
model-Agent results are later admitted, they use the shared OS structured-result
API and the same ResultSink envelope. Neither path is implemented or parsed by
Bench.

Malformed output, schema mismatch, missing output, an Agent exception, or an
identity mismatch is `judge_contract_failed`; it is never converted to a zero
Candidate score. stdout and stderr are bounded private diagnostics only.

## 8. The Single Runtime Port

### 8.1 A3sRuntimeClient

Bench depends on the one provider-neutral client exported by A3S OS platform
code. `A3sRuntimeClient` is a name for that shared service contract, not a trait
owned under Bench `ports/`. The following interface is illustrative; wire
encoding and language bindings may differ without changing its semantics:

~~~rust
trait A3sRuntimeClient {
    async fn capabilities(&self) -> RuntimeCapabilities;
    async fn submit(
        &self,
        spec: RuntimeExecutionSpec,
    ) -> Result<RuntimeExecutionHandle, RuntimeError>;
    async fn inspect(
        &self,
        handle: &RuntimeExecutionHandle,
    ) -> Result<RuntimeExecutionState, RuntimeError>;
    async fn result(
        &self,
        handle: &RuntimeExecutionHandle,
    ) -> Result<Option<RuntimeExecutionResult>, RuntimeError>;
    async fn cancel(
        &self,
        handle: &RuntimeExecutionHandle,
    ) -> Result<(), RuntimeError>;
}
~~~

Stable operation IDs make `submit` and `cancel` idempotent. Runtime, not Bench,
maps handles to Box, remote Runtime, Kubernetes, or another provider and owns
reattachment after transport uncertainty.

Bench must not downcast a handle, inspect provider internals, mount a host path,
call Box directly, issue a ModelGateway token, or clean a provider resource.

### 8.2 RuntimeExecutionSpec

Candidate and Judge use the same sealed object:

~~~text
RuntimeExecutionSpec
  schema
  operation_id
  role: candidate | judge
  asset_snapshot
  typed_input
  input_artifacts[]
  workspace_policy
  protected_mounts[]
  result_contract
  capability_grants
  resource_limits
  deadline
  evidence_requirements
  plan_bindings
~~~

Every path and mount kind is selected from a Runtime-owned protocol profile;
Task authors do not provide host paths. The spec contains logical capability
grants, never raw credentials.

For `role = candidate`:

- typed input contains the public prompt and plan bindings;
- public workspace seed is the only Task artifact;
- workspace is writable;
- Judge Asset and hidden bundle are absent;
- CandidateLock fixes the ModelGateway request; ExperimentPlan fixes the final
  exact scope after policy resolution;
- submission projection policy is copied exactly from TaskLock;
- result contract requires a Runtime-owned private TerminalCheckpoint, its
  derived SubmissionSnapshot, and usage evidence.

The Candidate adapter defines the controller entrypoint and controller runtime.
The Task's `work.image` defines the writable tool/workspace sandbox attached to that
controller; it never replaces or merges with the Asset runtime image. P1 admits
only Candidate adapters that implement this standard external-workspace
capability. Provider-specific image-composition rules are forbidden.

For `role = judge`:

- typed input is `bench.judge.request.v1`;
- SubmissionSnapshot and hidden bundle are distinct read-only inputs;
- TerminalCheckpoint is absent from Judge mounts and capabilities;
- the hidden bundle uses a protected mount inaccessible to Candidate roles;
- only bounded scratch is writable;
- general network is none;
- ModelGateway grant is none or the exact scoped grant in TaskLock and plan;
- result contract is the protected `bench.judge.result.v1` channel.

Role policy is control-plane data locked into ExperimentPlan. Passing the same
Asset selector in a different position cannot grant Judge authority because a
Judge role additionally requires the locked Task ownership and capability.

### 8.3 RuntimeExecutionResult

One result envelope covers both roles:

~~~text
RuntimeExecutionResult
  schema
  execution_id
  operation_id
  spec_digest
  role
  state
  started_at / finished_at
  typed_result_artifact?
  terminal_checkpoint?
  submission_snapshot?
  usage
  evidence
  provider_attestation
  failure?
~~~

Candidate completion requires both `terminal_checkpoint` and
`submission_snapshot`; Judge completion requires `typed_result_artifact`.
TerminalCheckpoint is classified `candidate_private`, while SubmissionSnapshot
has a distinct Trial-scoped read grant usable only by the locked Judge
operation. A field invalid for the role is rejected. Runtime evidence binds the
submission to its source checkpoint and locked projection policy, and binds all
input digests, the AssetSnapshot, granted capabilities, resource policy, and
provider conformance.

Provider name and hardware evidence may be reportable, but they do not give
Bench provider control. If a locked performance cohort requires particular
hardware, Bench declares the abstract requirement and verifies returned
attestation; Runtime performs placement.

RuntimeCapabilities is discovery data, not trust evidence. For an official
result, provider build conformance to the locked RuntimeSemanticsProfile and any
hardware cohort must chain to an A3S Runtime trust root, and the per-execution
attestation must bind that build, placement, spec digest, and result. A provider
cannot authorize itself by echoing capability fields. Operator-trusted
development providers may be used for `local_unofficial` runs, which remain
visibly unofficial regardless of successful capability preflight. An
`official` plan cannot downgrade to a development provider; it fails preflight.

### 8.4 Capability preflight

Before submission, Bench compares plan requirements with
`RuntimeCapabilities`. P1 requires:

- immutable Agent AssetSnapshot execution;
- Candidate/Judge role isolation;
- protected read-only mounts;
- protected typed results;
- atomic terminal checkpoint capture;
- atomic, policy-bound SubmissionSnapshot derivation without exposing the full
  checkpoint to Judge;
- network-none enforcement;
- no ModelGateway grant for the deterministic P1 Judge;
- hard resource and time limits;
- idempotent submit, inspect, result, and cancel;
- usage and isolation evidence.

Missing capability rejects the Experiment. There is no host-process fallback,
Judge stdout fallback, approximate copy fallback, or provider-specific bypass.

Capability resolution is a pure plan-time intersection:

~~~text
sealed grant = Asset request ∩ Task limit ∩ operator allowance
~~~

Task limits and operator policy may deny requests but cannot add an undeclared
Asset capability or silently reduce a discrete request. P1 has no optional
discrete capabilities: every discrete Asset request is required, so the allowed
intersection must contain
the complete requested set or planning fails with `policy_rejected`. Scalar
resources and budgets resolve to one exact value that satisfies Asset minima,
Task ceilings/defaults, and operator maxima; if no value exists, planning fails.
Runtime never substitutes a model, route, tool, network mode, resource cohort,
or weaker isolation profile. The sealed exact grant and the digests of all
outcome-affecting inputs enter ExperimentPlan before billable work.

## 9. Architecture

~~~text
                         a3s CLI
                            |
                  Bench control component
          +-----------------+------------------+
          |                 |                  |
   shared Asset     embedded a3s-flow     BenchStore
      resolver              |                  |
          |                 |                  |
          +---- A3sRuntimeClient + ArtifactStore ----+
                            |
                     A3S OS Runtime
          +-----------------+------------------+
          |                 |                  |
 local providers       remote provider   shared platform
 (Docker default,
  optional A3S Box)
                                            services
                                        (artifact store,
                                         ModelGateway,
                                         evidence)
~~~

The line between Bench and Runtime is exactly `A3sRuntimeClient` plus immutable
ArtifactRefs. There is no Candidate backend interface and separate Judge
backend interface.

### 9.1 Dependency direction

~~~text
a3s CLI presentation
        -> Bench application API
        -> Bench domain
        -> shared Asset resolver + embedded a3s-flow engine
           + A3sRuntimeClient + ArtifactStore + BenchStore
~~~

Bench never imports the CLI/TUI, Box SDK, Kubernetes SDK, ModelGateway SDK, or a
provider workspace implementation. Optional OS session/token refresh, project
root discovery, Asset Center APIs, and Runtime transport live in reusable A3S
platform code. Embedded/local Asset resolution and the local provider do not
require an A3S OS session. The local provider must be able to construct an
operator-owned ModelGateway route from the standard `.a3s/config.acl` for an
exact model selected by the Asset or `--model`, including custom and local
OpenAI-compatible providers. This route construction requires no A3S OS
session. Remote Asset resolution and remote providers may require one.

### 9.2 No duplicated control loops

Bench stores logical workflow step IDs and Runtime execution handles, but it
does not implement:

- an in-process or database work queue;
- worker leasing or lease renewal;
- controller epochs or fencing tokens;
- retry polling loops independent of the embedded orchestration engine;
- orphan scanning or a reaper;
- provider resource reconciliation;
- checkpoint mutexes or workspace writer barriers;
- ModelGateway budget enforcement;
- artifact upload credentials.

The embedded orchestration engine decides when a durable step runs or retries.
Runtime makes each execution operation idempotent, fences provider resources,
captures workspaces, and cleans up. Bench decides whether the resulting domain
outcome is valid and scoreable.

### 9.3 State authority and recovery

The three durable systems have non-overlapping authority:

- the internal a3s-flow history is authoritative for orchestration intent and
  the next durable step; it is not an Asset record;
- A3S OS Runtime is authoritative for execution acceptance, lifecycle,
  checkpoint, usage, and cleanup facts;
- BenchStore is authoritative for locks, accepted Judge results, scores, and
  reportable benchmark disposition.

P1 local mode places BenchStore, the embedded engine's event store, and an
outbox in one SQLite database below `.a3s/bench/`. Creating a run record and its
initial internal work item is one transaction. Runtime submission is outside
that transaction, so stable run/operation IDs and an outbox reconciler make it
idempotent. A BenchStore result is accepted only after its referenced terminal
Runtime fact has been verified; disagreement remains `reconciling`, never
guessed. None of these records is a Flow Asset or an Asset Center resource.

A future P3 remote Bench control plane may use separate services only under the
new remote protocol required by section 15; it must preserve the same authority
and reconciliation rules and must not add a Bench queue. P1 has no remote Bench
implementation.

## 10. P1 Single-Trial Workflow

The first usable workflow is intentionally linear:

~~~text
resolve and compile
        |
        v
Candidate RuntimeExecutionSpec
        |
        v
private TerminalCheckpoint
        |
        v
Runtime-derived SubmissionSnapshot
        |
        v
Judge RuntimeExecutionSpec
        |
        v
validated JudgeResult -> deterministic Score -> Report
~~~

Bench's internal durable workflow performs these steps:

1. Resolve the Task reference and Candidate reference to immutable inputs.
2. Compile or reuse TaskLock and CandidateLock.
3. Query Runtime capabilities and create one sealed ExperimentPlan.
4. Commit Experiment and Trial records in BenchStore.
5. Submit the Candidate `RuntimeExecutionSpec` with a stable operation ID.
6. Await or reattach to its `RuntimeExecutionResult` through the embedded
   orchestration engine.
7. Verify Candidate evidence, the private Runtime-owned TerminalCheckpoint,
   and the policy-bound SubmissionSnapshot derived from it.
8. Submit the Judge `RuntimeExecutionSpec` with a different stable operation ID.
9. Await or reattach to its `RuntimeExecutionResult` through the embedded
   orchestration engine.
10. Validate `bench.judge.result.v1`, compute the Score, commit the disposition,
    generate the report, and print the summary.

A restart resumes the internal workflow and inspects the same Runtime handles.
It does not submit duplicate work under a new operation ID. `cancel` signals
the embedded engine's generic external-operation activity; its finalizer calls
Runtime cancel and waits for a terminal Runtime record before finalizing the
Trial.

P1 does not promise a local background daemon. The foreground control component
drives its embedded orchestration engine while attached. If the client
disappears, durable state and the Runtime handle remain; the next `run`,
`result`, or `advanced cancel` invocation starts the embedded driver,
reconciles the same operation, and continues or cancels it without duplication.
A remote Runtime execution may continue while the client is absent because
Runtime, not Bench, owns its lifecycle.

P1 has exactly one terminal checkpoint and one derived SubmissionSnapshot.
There are no baseline, scheduled, trajectory, dev, oracle, anytime, or
user-selected checkpoints. There is no Suite wrapper hidden behind the command.
The ExperimentPlan directly commits to one Task and one Trial.

### 10.1 Terminal checkpoint

At Candidate termination, Runtime stops inference admission and tool/workspace
writers, drains admitted operations, captures one atomic generation, and then
releases or destroys the workspace according to retention policy. Bench never
walks the live directory.

The terminal manifest permits normalized directories and regular files only.
It rejects absolute and parent paths, duplicate or case-colliding paths,
symlinks, hardlinks, devices, sockets, FIFOs, setuid/setgid, unsafe xattrs,
sparse-file abuse, secret mount paths, excess depth, excess files, excess file
size, and excess total bytes. Runtime returns a content digest distinct from any
transport encoding digest.

The terminal checkpoint remains Candidate-private. In the same fenced terminal
generation, Runtime applies the locked submission include/exclude rules and
bounds to derive a normalized SubmissionSnapshot. Projection rejects unsafe
types and path collisions again, records the projection-policy digest, and
returns evidence binding submission digest to checkpoint digest. Judge receives
only the projected snapshot. Bench never performs projection by copying files,
and a Candidate-provided manifest, digest, path, or ArtifactRef is never
authoritative.

The Candidate may ask to finalize, but cannot provide checkpoint bytes, a host
path, an ArtifactRef, or a digest to be trusted. Timeout and cancellation use
the same Runtime-owned capture/failure contract.

### 10.2 Idempotency

Stable identities are derived from canonical inputs:

- `task_revision_id`: TaskLock content digest;
- `candidate_revision_id`: CandidateLock content digest;
- `experiment_plan_id`: ExperimentPlan content digest;
- `experiment_id`: plan plus creation nonce, for the user-visible attempt;
- Candidate operation ID: Experiment plus `candidate`;
- Judge operation ID: Experiment plus `judge`, SubmissionSnapshot digest, and
  hidden-bundle digest;
- `score_id`: plan, Judge result digest, and scoring contract digest.

The embedded orchestration engine retries steps with the same operation ID.
Runtime guarantees that repeated submit returns or reattaches to the same
logical execution. BenchStore uniqueness constraints prevent two accepted
Candidate results, Judge results, or Scores for one Trial.

This is transport/step retry, not evaluation retry. After Runtime accepts an
operation ID, no timeout, disconnect, process crash, or ambiguous response may
cause Bench to submit equivalent work under another ID. Pure compilation or
validation steps may be recomputed from the same locked bytes; an external
operation may only be queried, reattached, or cancelled. A second user-issued
`run` is a distinct Experiment with a new creation nonce and is never silently
deduplicated with an earlier run.

P1 does not add Bench-level speculative retries or choose the best of repeated
Judge attempts. Later retry policy must preserve logical identity and may retry
only classified infrastructure failures on identical inputs.

## 11. Plan and Identity

ExperimentPlan contains everything needed to reproduce the single Trial:

- TaskLock digest and canonical scoring contract;
- Candidate AssetSnapshot and CandidateLock digests;
- Judge AssetSnapshot and `bench.judge.v1` capability digest;
- public workspace, prompt, image, and hidden bundle digests;
- exact model identities, route policies, and budgets for Candidate and Judge;
- Candidate and Judge Runtime requirements and resource limits;
- terminal checkpoint manifest policy;
- SubmissionSnapshot projection policy and privacy class;
- typed Judge request/result schemas and bounds;
- failure mapping, evidence requirements, and privacy policy;
- governance status and, for official runs, admission record, signer-chain, and
  revocation-snapshot digests;
- required Runtime capability-profile and outcome-affecting policy-version
  digests.

The plan locks a `RuntimeSemanticsProfile` digest covering every
outcome-affecting Runtime rule: request/result protocol versions, Asset and
workspace materialization, protected mount and result behavior, checkpoint and
submission projection semantics, resource/time/usage accounting, network and
ModelGateway enforcement, cancellation terminality, and normalization rules.
An implementation or policy change that can alter any of those rules requires
a new profile digest and therefore a new ExperimentPlan.

The current capability advertisement, conforming provider build identifier,
and actual placement are evidence facts rather than semantic identity. A
Runtime upgrade may reuse a plan only when signed conformance evidence proves
the same locked RuntimeSemanticsProfile. For a performance metric, the plan
also locks the measurement cohort and all outcome-affecting placement
properties; actual hardware evidence must match them exactly. Unqualified or
best-effort placement cannot produce an official performance score.

Canonical identity JSON uses UTF-8 NFC strings, forward-slash case-sensitive
paths, sorted fields/entries, integer units, canonical decimal strings, expanded
defaults, and no timestamps, temporary URLs, credentials, mirrors, or provider
handles. An envelope stores provenance, signatures, and transport metadata
without changing semantic identity.

Task authors declare semantic needs. Runtime/operator policy chooses placement
and may impose stronger containment only when the locked
RuntimeSemanticsProfile explicitly classifies that containment as monotonic and
non-observable for the metric class. If that cannot be proven, planning rejects
the execution or creates a new plan. Any outcome-affecting substitution always
requires a new plan.

## 12. Result, Scoring, and Failures

### 12.1 Result validation

Bench accepts a Judge result only when:

- Runtime execution and evidence are conformant;
- execution ID, operation ID, role, and spec digest match the plan;
- Judge AssetSnapshot and every input artifact digest match;
- typed result came from the protected channel;
- schema, size, depth, keys, value types, and metric ranges are valid;
- model usage, if any, is within the scoped locked budget;
- no forbidden capability or general network use is reported.

All Judge-provided diagnostics are private in P1. A Task may mark declared
metric values as reportable, but it cannot promote Judge free-form text, hidden
expected values, test names, private logs, model chain-of-thought, protected
mount metadata, or raw Runtime diagnostics. Public messages are generated by
Bench from a closed set of typed disposition and validation codes. A later
schema may add audited parameterized templates; raw Judge strings never become
public through Task metadata alone.

### 12.2 Scoring

Scoring is a pure deterministic Bench function over validated canonical metric
values and the locked metric contract. P1 supports:

- integer and canonical decimal metrics;
- maximize or minimize direction;
- optional gate metrics;
- deterministic normalization to integer basis points;
- one primary aggregate score.

Binary floating point, Judge-supplied weights, dynamic code, report-time
recalculation from mutable inputs, and provider-dependent rounding are
forbidden. The report includes raw validated metrics, normalization, gates,
aggregate score, and scoring contract digest.

`solution_verdict = "valid"` means the Judge produced an authoritative,
scoreable measurement; it does not mean the candidate passed. A valid result
with no locked gates is displayed as `COMPLETED`. If gates exist, all passing is
`PASS` and any failing is `FAIL`. Typed execution/contract failures are
`ERROR` and have no score. A Judge must not encode ordinary test failure as a
second result status when the locked metric/gate contract already represents
it.

### 12.3 Failure taxonomy

| Class | Examples | Scoreable? |
| --- | --- | --- |
| `task_invalid` | bad TaskLock, unavailable hidden input, invalid Judge capability | No |
| `candidate_failed` | Agent exits, exceeds a locked Candidate limit, or produces invalid workspace content | Only if TaskLock explicitly defines a deterministic solution-failure mapping |
| `infrastructure_failed` | Runtime unavailable, lost provider resource, failed protected collection, unverifiable checkpoint | No |
| `judge_contract_failed` | Judge exception, malformed or missing typed result, identity mismatch | No |
| `policy_rejected` | missing capability, disallowed asset/model/route, unsupported isolation | No |
| `cancelled` | user cancellation completed by Bench's embedded orchestrator and Runtime | No |
| `valid` | validated Judge result and deterministic score | Yes |

Unknown execution state remains pending/reconciling until Runtime establishes a
terminal record. P1 exposes no Bench command or mutable database flag for an
operator to force a disposition. A Runtime-owned terminal `resource_lost` or
equivalent evidence may establish `infrastructure_failed`; it never establishes
a Candidate score. A future administrative override requires a signed,
append-only audit event, remains non-scoreable, and cannot rewrite an accepted
result. Infrastructure and Judge failures are visible in the report without
leaking private details.

Terminal facts are monotonic. Exactly one terminal disposition can commit for a
run through the BenchStore compare-and-commit sequence. A cancellation request
is intent, not a terminal fact: if Runtime had already durably completed, that
completion wins; if Runtime durably acknowledges cancellation first,
`cancelled` wins. Later, duplicate, or contradictory callbacks remain private
audit evidence and cannot rewrite the committed disposition. Client disconnect,
reconciliation timeout, and repeated `cancel` never provide another tie-break
or create a second operation.

### 12.4 Machine output and exit status

`--json` emits exactly one UTF-8 JSON object on stdout and no ANSI. Progress is
stderr. Every object has `schema = "a3s.bench.output.v1"`, `command`, `ok`, and
exactly one of typed command data or a typed `error`. Run/result data includes
`run_id`, `state`, `disposition`, Task/Candidate/Judge revision IDs,
`governance_status = local_unofficial | official`, admission digest when
official, metrics, score basis points when scoreable, gate verdict when defined,
`evidence_availability`, and report ref. Public output omits private artifact
digests and Judge diagnostics. The v1 object is a closed allowlist: emitters
must not add unregistered fields or enum values, and consumers must reject them.
Any additive field, changed optionality, new enum value, or altered redaction
requires a new schema identifier and explicit negotiation by the top-level CLI.

The JSON object is one line terminated by `\n`, uses canonical JSON escaping,
and contains no duplicate keys. Human output and JSON are mutually exclusive.
If the process cannot emit the complete object, stdout is not a protocol
response; callers use the exit status and durable `result` lookup. A truncated
object must never be interpreted as partial success.

Process exits are intentionally coarse:

| Code | Meaning |
| --- | --- |
| `0` | Command succeeded; for `run`, a valid score with no gate or a `PASS` was committed. |
| `2` | CLI, source, or schema validation error. |
| `3` | Task, asset, policy, admission, or Runtime capability unavailable. |
| `4` | Infrastructure failed or remained unreconcilable. |
| `5` | Candidate failed without a scoreable locked failure mapping. |
| `6` | Judge contract failed; no score was committed. |
| `10` | A valid score was committed but a locked gate produced `FAIL`. |
| `130` | User interruption; JSON/state distinguishes terminal `cancelled` from a second-`Ctrl-C` detach still reconciling. |

Exit `10` lets simple CI fail on an explicit Task gate while remaining distinct
from an execution error. Rich automation also reads the locked gate verdict
from JSON; a low score without a gate is still a successful evaluation.

## 13. Storage, Privacy, and Evidence

All Bench-owned implicit project records and artifact pins are rooted at
`.a3s/bench/`. A possible local layout is:

~~~text
.a3s/bench/
  locks/
  runs/
  results/
  reports/
  cache/       # public catalog metadata and rebuildable derived indexes only
  latest.json
~~~

Only the root is normative; subdirectories may migrate atomically between
component versions and are not public reference syntax. Files are immutable
after commit except atomic convenience pointers and bounded cache metadata.
Secret bytes, Runtime tokens, temporary URLs, provider credentials, and hidden
bundle plaintext are not stored in public records. Bench cache contains no
hidden bundle, private Agent package, TerminalCheckpoint, SubmissionSnapshot,
JudgeResult, credential, or private Runtime evidence bytes; those belong to
ArtifactStore under their authorization classes.

When Bench creates the root it uses owner-only directory permissions; private
records and journals are owner-readable/writable only. Creation and every
mutation use descriptor-relative no-follow operations, exclusive temporary
names, file/directory identity checks, durable flush, and atomic rename within
the same filesystem. Bench rejects a symlink, hardlinked mutable record,
ownership mismatch, permissive replacement, or path-component swap below the
root instead of following it. Existing projects with weaker permissions fail
with a remediation diagnostic; Bench does not silently chmod user-owned state
or continue insecurely.

This project-root guarantee does not relocate or expose Runtime or ArtifactStore
operator state. A local Runtime may keep VM, image, and content caches under its
own configured global root, and the shared local ArtifactStore may keep
encrypted/deduplicated bytes under its platform root. Those locations are not
Bench state, selectors, public APIs, or report inputs. Bench must never inspect
their layout directly.

Bench configures its embedded `a3s-flow` event store and task queue below this
same root; it must not accept a3s-flow's standalone `.a3s/flow/` defaults. The
engine continues to own workflow durability and leases, while Bench chooses
`.a3s/bench/` as the storage location required by the user contract. These are
internal orchestration records, never Flow Assets.

ArtifactRefs carry tenant and privacy class. At minimum:

- `public_task`: prompt and publishable Task metadata;
- `candidate_private`: terminal checkpoint and Candidate logs;
- `submission_trial`: SubmissionSnapshot with a read grant restricted to the
  one locked Judge operation;
- `judge_private`: hidden bundle reference, Judge logs, typed private result,
  and private diagnostics;
- `report_public`: explicitly redacted result projection;
- `operator_private`: Runtime evidence and security diagnostics.

`public` in these names means eligible for Bench's redacted export projection;
it does not mean uploaded, globally readable, anonymously accessible, or
published. P1 writes no public network resource. All local exports use the
command-specific atomic rules in section 4.3.

Equal content digests do not grant cross-tenant or cross-class access. Runtime
and artifact-service authorization is checked independently of digest equality.

ArtifactStore is the sole owner of bytes, encryption, authorization, pinning,
retention, and garbage collection. BenchStore owns only ArtifactRefs and
transactional pin intent. Committing a run atomically records pins for every
artifact required by its locked retention class; rollback or explicit
operator-owned retention expiry releases pins. Task ACL cannot set retention,
force deletion, or change privacy class. Runtime owns bounded temporary
materialization and guaranteed cleanup of plaintext hidden or submission data.

The validated JudgeResult fields required for scoring, the Score, disposition,
revision IDs, and redacted report projection are committed directly as small
immutable BenchStore records, not recoverable only from an expiring blob. If a
permitted evidence artifact later expires or is administratively unavailable,
`result` renders the committed score unchanged and reports
`evidence_availability = "unavailable"`; it never re-runs Judge or silently
reconstructs evidence.

P1 reports are inert local projections: UTF-8 HTML plus same-directory static
assets, with no script execution, remote URL, external font/image, form,
navigation request, embedded credential, or active content. Opening a report
performs no Runtime, registry, OS, ModelGateway, ArtifactStore, or Judge call.
Only the closed public projection is rendered. Private Judge diagnostics and
operator evidence remain authorization-scoped ArtifactRefs and have no P1
report or export surface.

The Runtime result supplies evidence for immutable Asset materialization,
capability grants, input digests, resource limits, network enforcement,
ModelGateway usage, terminal checkpoint, result collection, termination, and
provider conformance. Bench stores evidence references and verified digests,
not provider secrets.

## 14. Built-in Tasks and Third-Party Sources

Built-ins are native TaskBundles in one global catalog. Their public reference
is the bare Task ID. Source projects are importer concerns only.

Local TaskBundles and official built-ins have one execution protocol but
different governance status. A portable-valid local Task may run as
`local_unofficial` under the current operator policy without an A3S admission
signature. Its result and report must carry that label and can never be
represented as an official built-in result. A bare built-in ID is runnable only
when the installed catalog binds it to a valid signed admission record.

Official admission signatures chain to an A3S admission trust root distributed
by the signed top-level `a3s` release. The signed record binds at least the exact
TaskLock, Judge AssetSnapshot and capability, dependency closure, OCI manifests,
RuntimeSemanticsProfile, resource/measurement cohort, license/provenance,
privacy policy, evidence requirements, validity interval, signer role, and
admission schema. The installed component also carries a signed revocation
snapshot. Plan creation rejects an expired, revoked, unknown-key, mismatched, or
malformed admission before pull, credential issuance, or billable work.

Task authors, Judge manifests, local files, environment variables, operator
configuration, and Advanced commands may deny an admitted task but cannot
create, extend, replace, or promote an admission. Updating the signed component
or top-level trust material is the only P1 path to a new official admission.
Already committed reports retain the exact admission and revocation-snapshot
digests used at run creation; later revocation does not rewrite history.

For imported Tasks:

- converted Task and Judge metadata must conform to the same canonical schemas;
- every upstream source file, commit, digest, transformation, and license is
  recorded in provenance;
- upstream CLI, state layout, scheduler, runtime assumptions, and selector
  namespace are not imported;
- hidden or licensed bytes that cannot be redistributed remain immutable
  external artifacts and require admission before use;
- a source descriptor is not executable authority;
- catalog admission is separate from the Judge Asset manifest.

`a3s bench list` shows locally available Tasks. `--all` may show blocked entries
with the exact availability reason. Official admission and local availability
are independent, and neither creates a second Task kind, Judge protocol, or
Runtime path.

Built-in payloads remain in the installed Bench control component. Project
locks, Experiment records, and reports still go only to `.a3s/bench/`.

## 15. Code Organization

The initial implementation should remain small and enforce dependency
direction:

~~~text
src/
  domain/
    asset.rs
    task_source.rs
    task.rs
    plan.rs
    trial.rs
    submission.rs
    judge_result.rs
    scoring.rs
    report.rs
  application/
    compile_task.rs
    plan_experiment.rs
    run_experiment.rs
    read_result.rs
  ports/
    bench_store.rs
  orchestration/
    run_workflow.rs
  interfaces/
    a3s_cli.rs
  projections/
    report.rs
~~~

There are intentionally no Bench `runtime`, `executors`, `workspace_backends`,
`judge_runner`, `box`, `kubernetes`, `model_gateway`, `queue`, `leases`, or
`reaper` modules. The OS-owned Runtime client and shared Asset resolver are
platform dependencies, not Bench ports. The shared ArtifactStore client is also
a platform dependency; Bench defines privacy and pin intent but no blob backend.

The top-level CLI parses presentation options and invokes the Bench application
API. It does not implement digests, planning, privacy, scoring, or retry
semantics. P1 runs the Bench control component only as a local child process of
the top-level CLI. A remote A3S OS Runtime is allowed behind
`A3sRuntimeClient`; a remotely hosted Bench control plane is not.

Any future remote Bench control plane is a P3 product capability requiring a
new versioned transport, authentication, authorization, tenant, confirmation,
storage, privacy, cancellation, and availability contract. It may reuse the
domain schemas but cannot claim compatibility merely by forwarding the private
component protocol or preserving the current CLI syntax.

## 16. Implementation Phases

### P0: Freeze the minimum cross-component contracts

P0 exists to prove that the vertical slice is implementable, not to design the
eventual leaderboard.

This is a real platform gate, not implementation already present in Bench. At
the time of this design, the CLI `runtime` tool is a private FaaS batch client,
Agent publishing requires an `agent.md` project entrypoint, Box checkpointing
is explicitly unsupported, Box command results contain only
stdout/stderr/exit-code, and a3s-flow cancellation records an event without a
Runtime finalizer. P0 extends the shared platform components; Bench must not
paper over any of these gaps.

- Add generic named capabilities to the shared `a3s.asset.v1` schema and freeze
  the exact `bench.judge.v1` fields shown in this document.
- Freeze shared AssetSnapshot resolution for embedded, local, OCI, and A3S
  OS-hosted Candidate/Judge packages.
- Freeze atomic TaskSourceSnapshot capture and `source_changed` behavior for
  local TaskBundles.
- Add/freeze the shared A3S OS execution client,
  `RuntimeExecutionSpec`, `RuntimeExecutionResult`, protected mounts, and
  protected typed results in platform code rather than Bench.
- Add the generic deterministic function-entrypoint and named-capability forms
  to the shared `a3s.asset.v1` tool-Asset contract.
- Freeze the shared external-workspace capability that attaches an OS-hosted
  Agent controller to the Task `work.image` sandbox without image merging.
- Add the a3s-flow engine's generic external-operation activity with durable
  handle, suspend/reattach, cancellation finalizer, and non-blocking lease
  semantics.
- Add the generic a3s-flow/SQLite outbox integration needed to atomically create
  a Bench run and its initial internal workflow intent below `.a3s/bench/`.
- Prove that no-login/no-override execution selects Docker, that an explicit
  `.a3s/config.acl` provider such as `a3s-box` takes precedence, and that both
  roles run without provider SDK dependencies in Bench.
- Prove Runtime-owned atomic terminal checkpoint capture, idempotent reattach,
  cancellation, network none, and evidence collection.
- Prove Runtime-owned SubmissionSnapshot derivation from the private terminal
  checkpoint under the locked projection policy; Judge must not receive the
  full checkpoint.
- Freeze capability resolution as Asset request intersected with Task limit and
  operator allowance, including fill-only `--model` and zero ambient session or
  user configuration.
- Freeze the local shared ArtifactStore interface, privacy-class authorization,
  transactional pin intent, retention ownership, and bounded Runtime
  materialization cleanup.
- Add signed component release statements and signed built-in admission records
  rooted in top-level A3S trust material; checksums and local metadata are not
  authority.
- Prove the deterministic-none Judge execution through the shared Runtime API;
  keep scoped-ModelGateway Judge conformance on the same contract without
  blocking P1.
- Freeze minimal TaskLock, CandidateLock, ExperimentPlan,
  `bench.judge.request.v1`, `bench.judge.result.v1`, Score, and failure schemas.
- Implement strict Task and Asset validation and canonical digest fixtures.
- Verify a malicious Candidate cannot reach protected mounts or results, and a
  malicious Judge cannot escape its capability scope.
- Check in one smoke Task with known-good, known-bad, timeout,
  malformed-Judge, and deterministic-Judge fixtures.

P0 does not implement Suite, Campaign, checkpoint schedules, dev feedback,
statistics, distributed Bench workers, or leaderboards.

### P1: Usable single-Task terminal evaluation

- Wire the four normal commands through the main A3S CLI and lazy Bench control
  component; implement only the closed P1 Advanced set from section 4.4.
- Resolve bare built-in Task IDs, explicit local paths, embedded Candidate
  aliases, local Candidate adapters, arbitrary OCI Candidate adapters, and A3S
  OS-hosted adapter packages.
- Run embedded/local Candidate and Judge adapters through the local OS Runtime
  provider without requiring a cloud login; only remote Asset resolution or a
  remote provider requires an OS account.
- Compile one TaskLock, CandidateLock, and direct one-Trial ExperimentPlan.
- Execute Candidate and task-owned Judge through `A3sRuntimeClient` only.
- Persist internal orchestration state, Runtime references, artifact pins,
  terminal checkpoint, SubmissionSnapshot, validated Judge result, Score, and
  report under the ownership rules in section 13.
- Recover after CLI/control-component restart by resuming the internal workflow
  and reattaching Runtime handles.
- Enforce hard resource, time, token, tool, output, and model-cost limits through
  locked Runtime grants.
- Materialize, adapt, validate, and admit each Task revision selected for the
  P1 released built-in set together with its Judge supply chain. A selected
  revision with a missing hidden bundle, mutable Judge image, legacy stdout
  parser, absent typed-result adapter, or missing Runtime conformance evidence
  is excluded from that released set. It does not block unrelated admitted
  revisions, and it must never appear as runnable in the default catalog.

P1 is the first usable release. It is complete before any advanced evaluation
surface is added.

### P2: Repetition, Suites, and trajectory protocol

- Add repeated Trials with explicit seeds and deterministic retry semantics.
- Add Suite compilation and aggregation.
- Add controller-requested scheduled checkpoints and bounded dev feedback.
- Add terminal, trajectory, anytime, and oracle projections over predeclared
  targets without re-running accepted Judges.
- Add confidence intervals, cohort validation, and multi-Trial reports.

All Agent execution continues through the same Runtime port. P2 must not add a
Bench scheduler, provider adapter, or Judge executor.

### P3: Official and distributed evaluation

- Add signed plans, campaign admission, hidden-seed commitments, embargo, and
  release policy.
- Define a remote Bench control-plane protocol only if required, with explicit
  authentication, tenant isolation, authorization, privacy, confirmation,
  cancellation, and compatibility semantics; do not expose the private local
  component protocol directly.
- Add remote Runtime conformance, tenant quotas, storage retention, and
  operational views through shared platform services.
- Add immutable leaderboard releases, provenance, quarantine, re-score policy,
  and audit evidence.

Distribution scales Runtime and internal orchestration capacity; it does not
create Bench-owned worker leases or provider control loops.

## 17. P1 Acceptance Criteria

The first usable release is complete only when evidence proves every item:

- `a3s --help` exposes one `a3s bench` command family and no public secondary
  executable is required;
- component activation requires a valid A3S-signed release statement; HTTPS,
  checksum, payload probe, and local manifest alone cannot authorize code;
- no command or Task field resolves, publishes, imports, executes, or selects a
  Flow Asset, and no `--flow` option exists;
- Bench works with no Flow account, catalog, registry, installed Flow Asset, or
  `.a3s/flow/` tree; network traces and resolver mocks prove that no Flow lookup
  occurs during install, run, recovery, cancellation, or result lookup;
- the embedded engine accepts only Bench-compiled internal activities and
  cannot supply Task, Candidate, Judge, provider, disposition, or score values;
- help and version probes, plus top-level `a3s list`, do not trigger component
  installation or create project state;
- the first real Bench command, including `bench list` or `bench info`, can
  lazily install only the private control component; read-only commands never
  install Box, start Runtime work, or create project state;
- a bare built-in ID and an explicit `./` local path resolve unambiguously;
- every command accepts only the exact positional/option matrix in section 4.4;
  aliases, abbreviations, duplicate singletons, hidden environment options, and
  command-inapplicable flags fail validation;
- `--locked` accepts only the explicit immutable inputs in section 4.4, performs
  no component/provider lazy install, update, network/source resolution,
  credential refresh, or heuristic cache lookup, and fails when digest-pinned
  bytes or current-principal authorization are unavailable locally;
- every Task revision in the released built-in set is admitted, appears in the
  default list, and completes end to end with its locked task-owned Judge; a
  provisional, quarantined, or unavailable import is excluded from that set and
  does not block unrelated admitted revisions;
- every built-in works after normal Bench installation without an A3S OS login
  when using local assets and the local Runtime; first use may pull its locked
  work/Judge OCI artifacts from their declared registries, including with
  operator-owned registry credentials, but requires no manual import, hidden
  bundle placement, Judge construction, or catalog repair;
- official built-ins require a valid signed admission and local Task results are
  labeled `local_unofficial`; no local or Advanced input can promote them, and
  an invalid official plan never silently downgrades;
- every Bench-owned implicit project record and ArtifactStore pin is under
  `.a3s/bench/`, no `.a3s-bench` path exists, and no `--state-dir` option exists;
  Runtime and ArtifactStore global roots remain opaque platform state; created
  state is owner-only and no-follow/path-identity checks fail closed;
- Candidate adapters and Judges both resolve through the ordinary A3S
  AssetSnapshot contract without restricting the Candidate implementation;
- local Task, Asset, Runtime, and `.a3s/config.acl` provider/model resolution
  run without an A3S OS login; only a selected remote Asset or provider may
  require OS authority;
- the local Runtime resolves only an explicitly selected or Asset-bound model
  from `.a3s/config.acl`, never its ambient `default_model`, and keeps provider
  credentials out of locks, reports, evidence, and Agent sandboxes;
- Candidate model and grants follow fill-only/intersection rules and inherit no
  ambient account, session, Memory, MCP, tool, shell, or environment state;
- the Judge has exactly one valid `bench.judge.v1` capability and cannot be
  replaced from the CLI;
- the deterministic Judge runs through the shared `A3sRuntimeClient` contract;
- Bench contains no Box, Kubernetes, workspace, Judge executor, or ModelGateway
  adapter;
- Candidate receives only public inputs and cannot discover or access the Judge
  Asset, hidden bundle, result channel, or provider credentials;
- Runtime atomically captures one Candidate-private terminal checkpoint and
  derives one SubmissionSnapshot under the locked policy without trusting
  Candidate or Bench bytes, paths, digests, manifests, or ArtifactRefs;
- Judge receives only separate read-only SubmissionSnapshot and protected hidden
  mounts plus bounded scratch, never the full checkpoint or Candidate controller
  state;
- only the protected typed channel can produce an accepted Judge result;
- a crash resumes the same internal workflow and reattaches to the same Runtime
  operations without duplicate Candidate, Judge, or Score records;
- cancellation reaches a terminal Runtime and Trial state without a Bench
  reaper;
- infrastructure and Judge failures are never reported as Candidate scores;
- the completed run prints the score and report location, and `result` opens the
  committed result without re-evaluation; reconciliation can commit only facts
  from the same Runtime operation IDs;
- P1 exports are closed, atomic, owner-only redacted projections or locks and
  cannot expose private artifacts or overwrite an existing destination;
- P1 exposes no manual disposition override; only a Runtime-owned terminal fact
  may establish infrastructure failure;
- `--json`, terminal labels, and exit codes conform to section 12.4;
- v1 JSON and public report/export projections contain only their closed
  allowlists, and generated HTML is inert and network-free;
- all security, identity, privacy, and failure tests below pass against the
  default Docker provider and an explicitly configured A3S Box provider,
  including no-login selection, provider identity sealing, and no fallback when
  the selected provider is unavailable.

## 18. Required Tests

### 18.1 CLI and state

- help/version before Bench installation without network or filesystem writes;
- first-use `bench list` and `bench info` install only the control component,
  then remain read-only with respect to project state and Runtime providers;
- lazy installation on the first subcommand that needs the control component;
- signed release-statement trust root, signature, expiry, target, archive digest,
  payload-tree digest, manifest, and probe mismatch rejection; checksum/probe
  alone must fail authentication;
- bare built-in, `./` local, `../` local, immutable OCI, and invalid ambiguous
  reference cases;
- no `builtin:` compatibility alias, implicit local lookup, `--judge`, `--flow`,
  provider selector, or `--state-dir`;
- exact command grammar rejection for unknown, abbreviated, duplicate,
  misplaced, and command-inapplicable options, plus all attempted hidden
  `A3S_BENCH_*`/`BENCH_*` overrides;
- rejection of `--option=value`, short/combined options, Bench-local `--`,
  dash-prefixed values, `latest`/partial/case-folded run-ID aliases, and
  acceptance only of the documented next-token value forms;
- project-root discovery, local no-login execution, and remote login only when
  a selected asset/provider needs it;
- project-local and user-local `.a3s/config.acl` provider/model resolution by
  the local Runtime without an OS session, including custom endpoints, explicit
  model selection, secret non-disclosure, and rejection of implicit
  `default_model` inheritance;
- Candidate adapter and Judge Asset resolution from arbitrary OCI-compatible
  registries, exact manifest/package digest locking, mutable-tag elimination,
  rejection of plain non-Asset images, and authority-scoped registry
  credentials;
- pasted foreign Asset origin, authority-changing redirect, and credential
  forwarding rejection;
- all mutating commands create Bench-owned project records and pins only below
  `.a3s/bench/` unless a permitted `--out` is explicitly supplied; Runtime and
  ArtifactStore internal roots remain opaque;
- the exact closed Advanced command set and rejection of Bench CLI/Task-level
  provider selection, Judge replacement, quarantine promotion, official
  labeling, raw export, manual resume, and status mutation; operator selection
  remains available only through the shared `.a3s/config.acl` Runtime schema;
- `--locked` explicit TaskLock/CandidateLock, missing local artifact, bare
  built-in, embedded alias, mutable selector, source directory, remote lookup,
  heuristic cache-match, absent component/provider, attempted lazy install,
  credential refresh, expired revocation snapshot, and digest-present but
  unauthorized artifact cases;
- export exclusive-create, no-follow, owner-only mode, existing destination,
  missing parent, and private-data redaction cases;
- project-state owner/mode, symlink, hardlink, path-component replacement,
  permissive pre-existing root, and concurrent writer cases;
- Bench's embedded a3s-flow integration never creates a sibling `.a3s/flow/`
  tree, never creates a Flow Asset, never calls a Flow registry/catalog API,
  and behaves identically when arbitrary user Flow Assets and Flow credentials
  are present or absent;
- an incompatible internal-workflow version fails recovery without re-resolving
  input, starting a replacement Runtime operation, or mutating a terminal fact;
- `run` terminal summary and `result` latest/explicit-ID behavior;
- stable `a3s.bench.output.v1`, stdout/stderr separation, and every exit-code
  class in section 12.4, including rejection of unregistered output fields and
  enum values;
- one-line complete JSON, duplicate-key rejection, truncated-output failure,
  and no partial or pre-existing `--out` mutation on every error boundary;
- first `Ctrl-C`, abrupt client loss, next-command reconciliation, and advanced
  cancellation behavior, including second-`Ctrl-C` detach and both orderings
  of the Runtime-completion/cancellation race;

### 18.2 Compiler and identity

- duplicate, unknown, dynamic, range, path, and cross-field rejection;
- invalid UTF-8, non-NFC, control, case-folded path, noncanonical integer,
  overflow, digest spelling, timestamp, unknown schema, and attempted schema
  downgrade/closest-version rejection;
- TaskSourceSnapshot atomic capture and mutation-at-every-read-boundary tests;
- exact semantic capture closure; unreferenced author files do not affect
  identity or enter a sandbox, while symlink, hardlink, mount crossing, special
  file, case collision, and path-replacement cases fail;
- canonicalization and digest golden tests;
- local and OS Asset packages with equal semantic content produce equal
  AssetSnapshot digests;
- archive re-encoding and provenance changes do not change semantic identity;
- mutable source movement does not change an existing TaskLock or plan;
- Candidate `--model` is fill-only, an adapter-bound model cannot be overridden,
  ambient account/session/Memory/MCP/tool/shell/environment state is ignored,
  and every capability grant equals the locked three-way intersection;
- Candidate adapter lacking normal execution requirements and Judge Asset lacking or
  duplicating `bench.judge.v1` are rejected;
- old `benchmark {}`, handler, runner, SDK ABI, and Judge-runtime-index fields
  are rejected rather than silently translated;
- TaskLock binds Judge snapshot, capability, hidden digest, model scope, schemas,
  and metrics;
- quarantined built-ins fail before external or billable work.
- local portable-valid results are `local_unofficial`; admission unknown-key,
  mismatch, expiry, revocation, validity, contradictory envelope, silent
  downgrade, and local-promotion cases fail before external or billable work;

### 18.3 Unified Runtime contract

- Candidate and Judge requests serialize through the same
  `RuntimeExecutionSpec` type;
- role-incompatible mounts, grants, and result contracts are rejected;
- repeated submit with one operation ID reattaches to one execution;
- inspect/result after client restart returns the same completion;
- cancel is idempotent and Runtime performs descendant/resource cleanup;
- capability preflight rejects missing atomic checkpoint, protected mount,
  protected result, network-none, or evidence support; it also rejects missing
  ModelGateway scope whenever a scoped model Asset is admitted;
- Candidate completion requires both private TerminalCheckpoint and
  policy-bound SubmissionSnapshot with evidence binding their digests;
- RuntimeSemanticsProfile mismatch, outcome-affecting provider upgrade,
  unqualified performance placement, and hardware cohort mismatch rejection;
- official execution rejects unsigned, unknown-key, self-asserted, expired, or
  spec/build/placement-mismatched Runtime conformance evidence; operator-trusted
  development providers remain `local_unofficial`;
- Box is exercised through the Runtime provider without a direct Bench call;
- the shared Runtime client types are owned outside Bench and the CLI-private
  FaaS batch tool is not imported;
- provider identity cannot alter plan semantics or score.

### 18.4 Candidate and checkpoint security

- Candidate cannot enumerate or read Judge Asset, hidden bundle, Judge request,
  Judge output, protected result metadata, or provider handles;
- absolute, parent, duplicate, Unicode, symlink, hardlink, device, FIFO, sparse,
  xattr, setuid, file-count, size, and compression-bomb checkpoint cases;
- secret and `.a3s/bench/` path exclusion;
- atomic terminal capture under concurrent workspace writes;
- deterministic submission include/exclude projection, reserved path, unsafe
  type, count/size bound, checkpoint-binding, and concurrent-write cases;
- timeout, cancellation, orphan process, disk, process, output, token, tool, and
  cost limit enforcement;
- Candidate cannot supply an authoritative digest, ArtifactRef, or host path.
- Candidate and Bench cannot supply an authoritative SubmissionSnapshot digest,
  manifest, ArtifactRef, bytes, or path;

### 18.5 Judge security and result validation

- deterministic Judge receives no ModelGateway or general network capability;
- network-none denies DNS, loopback, link-local/metadata, bridge host services,
  inherited and new IP sockets, raw sockets, proxy inheritance, and unsealed
  Unix-domain sockets for both Candidate and Judge;
- when scoped-model admission is enabled, its Judge receives only the locked
  model/route/budget scope and no raw credential;
- Judge cannot widen capabilities, access Candidate runtime state, or write the
  read-only submission and hidden mounts;
- Judge cannot access the full TerminalCheckpoint, Candidate adapter/controller
  source, private logs, credentials, or live workspace;
- Candidate cannot write, inherit, or spoof the protected result capability;
- stdout/stderr and ordinary workspace files cannot substitute for typed result;
- result schema, field, range, identity, size, nesting, and binding mismatch;
- Judge exception and missing output map to `judge_contract_failed`;
- all Judge free-form diagnostics, hidden data, test names, model internals,
  protected metadata, and raw Runtime evidence are absent from public report;
- provider collection failure maps to `infrastructure_failed`;
- malicious Judge output cannot change weights, scoring code, plan identity, or
  Runtime evidence.

### 18.6 Durability and scoring

- embedded orchestration-engine restart at every step boundary;
- transport uncertainty after Runtime acceptance;
- pure-step retries and repeated submit preserve operation IDs; disconnect,
  restart, and timeout never create a second logical execution, while a second
  explicit user `run` creates a distinct Experiment;
- exactly one accepted Candidate result, terminal checkpoint,
  SubmissionSnapshot, Judge result, and Score per Trial;
- cancellation during Candidate and Judge execution;
- deterministic metric validation, gates, normalization, quantization, and
  aggregate score;
- candidate failure mapping only when explicitly locked;
- result/report rendering performs no Runtime or Judge call;
- report HTML has no script, active content, external URL, or network-capable
  asset, and exposes no private-diagnostic retrieval surface;
- equal artifact digests do not bypass tenant or privacy-class authorization;
- ArtifactStore transactional pin/rollback/expiry/GC and Runtime temporary
  plaintext cleanup;
- missing retained evidence does not change committed score and renders explicit
  evidence availability without re-execution;
- no P1 mutable operator disposition override or rewrite of an accepted result.
