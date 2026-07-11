# Built-in Tasks

Built-ins are ordinary a3s-bench TaskBundles with task-owned A3S Judge Agent
Assets. They are entries in one global catalog, not separate benchmark
products, runtimes, or command namespaces.

## User Interface

An admitted built-in is referenced by its bare task ID:

~~~bash
a3s bench run TASK_ID --agent codex
~~~

A bare ID searches admitted built-ins only. It never searches local paths, A3S
OS, upstream source names, or quarantined entries. Local TaskBundles are
explicit and begin with `./` or `../`:

~~~bash
a3s bench run ./tasks/smoke --agent codex
~~~

The catalog commands are:

~~~bash
a3s bench list                              # admitted and runnable only
a3s bench list --all                        # include quarantined records
a3s bench info TASK_ID                      # runnable task details
a3s bench info ad_placement_optimization --all  # quarantine inspection
~~~

`info <id> --all` is read-only catalog inspection. It does not turn a
quarantined record into a runnable task reference, pull its image, resolve
credentials, or create project state.

There are no source-specific selectors, CLIs, protocols, state directories,
or runtime versions. Immutable identity comes from the compiled TaskLock and
the task-owned Judge AssetSnapshot, not from the readable task ID.

## Admission

Catalog discovery and execution admission are separate. A task with
`admission = "quarantined"` appears only with `list --all` and can be inspected
with `info --all`, but `run` and lock compilation fail before an OCI pull,
credential lookup, model reservation, or billable work.

Admission requires a signed record binding:

- the exact TaskBundle and Judge Agent AssetSnapshot;
- immutable dependency and OCI manifest digests;
- the `bench.judge.v1` capability and typed request/result schemas;
- A3S OS Runtime isolation, protected mounts, and result-channel behavior;
- network, ModelGateway, secret, resource, and timeout capabilities;
- licenses, provenance, and required execution evidence.

The signer must chain to an A3S admission trust root distributed by the signed
top-level `a3s` release. The record also binds the RuntimeSemanticsProfile,
measurement cohort, privacy policy, validity interval, admission schema, and
signer role. The installed Bench component carries a signed revocation
snapshot; an expired, revoked, unknown-key, mismatched, or malformed record is
not admitted.

Task authors, Judge manifests, local catalog edits, environment variables,
operator configuration, and Advanced commands may deny a task but cannot
create, extend, replace, or promote official admission. A portable-valid local
Task may run only as `local_unofficial`, and its result cannot claim built-in or
official status. Updating signed A3S trust/component material is the only P1
path to a new official admission.

Runtime capability advertisement alone is not execution admission. An official
run also requires signed provider-build conformance to the admitted
RuntimeSemanticsProfile and a per-execution attestation binding build,
placement, spec, and result. An operator-trusted development provider may run a
local Task, but the result remains `local_unofficial`.

The 51 third-party task sources currently in this catalog are all quarantined.
Their source records publish OCI tags and legacy evaluator commands, but do not
provide enough evidence to admit those commands as A3S Judge Agent Assets. The
image layers are referenced, not included or republished by this repository.
Consequently, the default `a3s bench list` is empty in the current fixture
snapshot; `--all` exposes the quarantined records for audit.

This is an incomplete development snapshot, not an acceptable release state.
The first usable a3s-bench release must admit and ship all 51 as out-of-the-box
built-ins. For every entry, release preparation must resolve immutable work and
Judge OCI manifests, provide any required protected hidden input, adapt legacy
evaluator output to `bench.judge.result.v1`, validate the complete Task/Judge
supply chain, and attach the required admission and Runtime-conformance
evidence. Normal first use may pull those locked OCI artifacts, but users must
not clone an upstream dataset, run the import tool, construct a Judge, place a
hidden bundle, edit the catalog, or log in to A3S OS when using the local
Runtime. Any one of the 51 remaining quarantined or unavailable blocks release.

Once a revision is admitted, it becomes visible in the default `list` and its
bare ID works with the normal command:

~~~bash
a3s bench run TASK_ID --agent asset:acme/reviewer
~~~

The task still owns the Judge and users cannot provide a replacement. Candidate
and Judge are both standard `a3s.asset.v1`, `category = "agent"` assets. The
A3S OS Runtime executes their locked snapshots with candidate and Judge role
policies; Bench does not provide another Agent Runtime.

## Repository Layout

~~~text
builtin/
  catalog.json
  README.md
  THIRD_PARTY_NOTICES.md
  licenses/
  provenance/
  tasks/<task-id>/
    task.acl
    public/prompt.md
    private/judge/
      .a3s/asset.acl
      agent.md
      judge.source.json
~~~

An admitted runnable TaskBundle that needs hidden data supplies it at
`private/bundle/`. If an authored or admitted Task intentionally needs none, it
may omit that directory and its TaskLock records the canonical empty-tree
digest; Git does not need to preserve an empty directory. These 51 quarantined
source records also omit the directory, but for a different reason: the
upstream hidden bytes are unavailable. That absence must not be represented as
an audited empty bundle, so their catalog entries use
`admission_reason = "hidden_bundle_unavailable"`.

`catalog.json` contains discovery fields only. Source repositories, revisions,
source-file digests, adaptations, and generated-file digests live under
`provenance/`. Source-specific importers are maintainer tools; source names do
not become public task selectors or runtime concepts.

Built-in payloads remain in the private lazy Bench control component and are
not copied into the current project. `list` and `info` are read-only. Any locks,
runs, evidence, results, and reports created by `run` use the one project state
root:

~~~text
<project>/.a3s/bench/
~~~

The location of a built-in or local TaskBundle never changes that root.

## Maintenance

To reproduce the current third-party conversion from pinned local source
repositories:

~~~bash
python3 tools/import_edgebench.py \
  --dataset-dir /path/to/task-dataset \
  --harness-dir /path/to/evaluation-harness
python3 tools/check_builtins.py
~~~

The importer refuses unexpected source revisions, merges into the global
catalog without overwriting tasks owned by another source, and never contacts
an OCI registry. The checker verifies catalog identity, generated digests,
standard A3S Agent Asset fields, quarantine state, generic naming, provenance,
licenses, and forbidden paths.

See [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md) for attribution and reuse
terms.
