# Smoke Task

This is the canonical minimal authoring fixture. The candidate changes
`answer.txt` to `42`; one standard A3S Judge Agent Asset evaluates the terminal
SubmissionSnapshot against the task's single hidden `expected.txt` bundle. The
complete TerminalCheckpoint remains Candidate-private.

~~~text
examples/smoke/
  task.acl
  public/
    prompt.md
    workspace/answer.txt
  private/
    judge/
      .a3s/asset.acl
      agent.md
      judge.py
    bundle/expected.txt
~~~

The Judge is an ordinary `a3s.asset.v1`, `category = "agent"` package. It uses
its normal `judge.py:evaluate` entrypoint and declares the standard
`bench.judge.v1` capability. A3S OS Runtime launches the locked AssetSnapshot
with `role = judge`, supplies the submission and hidden bundle through protected
read-only mounts, validates the bounded `bench.judge.result.v1` output, and owns
the protected result channel. The Judge reads candidate and hidden files only
through the request's `submission_root` and `hidden_bundle_root`; it never
receives the result capability itself. Runtime derives the immutable submission
from the private checkpoint using TaskLock; neither Bench nor Candidate supplies
its authoritative bytes, path, digest, manifest, or ArtifactRef.

This is the target shared Agent Asset contract. The development Docker Runtime
adapter executes the function entrypoint inside a read-only Judge container and
parses its bounded typed result. That adapter must move behind the shared
`A3sRuntimeClient` contract before release; Bench domain code must never execute
or interpret `judge.py:evaluate` itself.

The expected answer deliberately remains outside the asset. A3S OS Runtime
mounts `private/bundle/` read-only only for the terminal Judge execution, so
publishing this asset to A3S OS would not publish hidden task data.

With a running Docker Engine, exercise the current no-login development path
from the crate root:

~~~bash
cargo run -- run ./examples/smoke --agent ./examples/smoke-candidate
~~~

The repository integration smoke builds both Candidate and Judge OCI fixtures,
runs the local-directory path and the all-OCI Candidate/Judge adapter path,
validates their machine output, and reopens the latest persisted result:

~~~bash
./tools/smoke_local.sh
~~~

`run` validates the task, resolves and locks the Candidate and Judge adapters,
asks A3S OS Runtime to execute their role-specific sandboxes, and prints
the final result and report location. Retries and resume reuse the sealed plan;
they may recompute a pure step or reattach using the same Runtime operation ID,
but never create another logical Candidate/Judge execution or re-resolve input.
Changing authored source creates a new immutable revision for a new run.

The component supports the deterministic local Candidate above, generic
model-backed Candidates, and local or OCI adapter packages. Migration to the
complete shared Runtime lifecycle remains release work.
