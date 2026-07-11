# Candidate adapter authoring

Bench evaluates Candidates, not one vendor's Agent type. A Candidate may be a
coding agent, another automated system, or a deterministic tool. It joins Bench
through a Candidate adapter: create a local package, publish the same package as
an OCI artifact, or export a CandidateLock from either source. Adding a
Candidate does not require a product-specific branch in Bench.

The current adapter wire format reuses `a3s.asset.v1`, `category = "agent"` so
Candidate and Judge packages can share resolution, snapshotting, and Runtime
machinery. This is a packaging contract, not a requirement that the underlying
Candidate was built with A3S.

## Package contract

A minimal local package has this shape:

```text
my-agent/
├── .a3s/
│   └── asset.acl
├── run.sh
└── prompts/
    └── controller.md
```

```acl
version = "a3s.asset.v1"
category = "agent"
kind = "tool"
name = "my-agent"

source {
  package_path   = "."
  entrypoint     = "run.sh"
  definition_path = "prompts/controller.md"
}
```

Paths are normalized package-relative paths. Absolute paths, `..`, symlinks,
hard links, and special files are rejected during validation or snapshotting.
The entrypoint and definition must be part of the immutable package.

## Two execution forms

An executable Candidate omits `--model`. Its entrypoint receives the private
workspace path as its first argument:

```sh
#!/bin/sh
set -eu
workspace=$1
# Read and modify only "$workspace".
```

The current development Docker path runs this entrypoint without network access
or model-provider credentials. It is suitable for deterministic tools and for
adapters whose complete dependencies are already in the locked work image. It
is not a safe way to run a host-installed Codex or Claude Code CLI.

A model-backed Candidate supplies `--model`. Bench reads the controller
instructions from `source.definition_path`, obtains the named provider/model
route from `.a3s/config.acl`, and keeps provider credentials on the host-owned
model client. The ambient `default_model` is never used as benchmark identity.

```bash
a3s bench run ./task \
  --agent ./my-agent \
  --model openai/my-model
```

The current model-backed implementation is Bench's generic workspace-tool
controller. A controller prompt named after Codex or Claude does not make it
the Codex or Claude Code product. Native product adapters require the shared
Runtime to expose their declared network, ModelGateway, tool, and credential
capabilities without placing secrets in the Candidate sandbox.

## Lock before comparing

Create one TaskLock and one CandidateLock per exact adapter/model combination:

```bash
a3s bench advanced task lock ./task --out ./task.lock.json
a3s bench advanced candidate lock ./my-agent \
  --model openai/my-model \
  --out ./my-agent.candidate.lock.json

a3s bench run ./task.lock.json \
  --agent ./my-agent.candidate.lock.json \
  --locked
```

Use distinct Candidate adapter revisions to compare complete coding-agent
products. Use one adapter revision with different model bindings to isolate
model behavior. In both cases, run the CandidateLocks against the same TaskLock.

## OCI publication

The package can be stored in any OCI Distribution-compatible registry. Bench
accepts Docker-compatible images containing `/.a3s/asset.acl` and generic OCI
artifacts pulled with ORAS:

```bash
a3s bench advanced candidate lock \
  oci://registry.example.com/agents/my-agent:1 \
  --model openai/my-model \
  --out ./my-agent.candidate.lock.json
```

A mutable tag is only a source selector. Lock creation records the resolved OCI
manifest and canonical package content; locked execution never follows the tag.
