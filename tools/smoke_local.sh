#!/bin/sh
set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$root"

docker version --format '{{.Server.Version}}' >/dev/null
docker build -q -t a3s-bench-smoke-agent:test ./examples/smoke-candidate >/dev/null
docker build -q -t a3s-bench-smoke-judge:test ./examples/smoke/private/judge >/dev/null

local_output=$(cargo run --quiet -- run ./examples/smoke \
  --agent ./examples/smoke-candidate --json)
oci_output=$(cargo run --quiet -- run ./examples/smoke-oci-judge \
  --agent oci://a3s-bench-smoke-agent:test --json)

lock_dir=$(mktemp -d)
trap 'rm -rf "$lock_dir"' EXIT HUP INT TERM
cargo run --quiet -- advanced task lock ./examples/smoke \
  --out "$lock_dir/task.lock.json" >/dev/null
cargo run --quiet -- advanced candidate lock ./examples/smoke-candidate \
  --out "$lock_dir/candidate.lock.json" >/dev/null
locked_output=$(cargo run --quiet -- run "$lock_dir/task.lock.json" \
  --agent "$lock_dir/candidate.lock.json" --locked --json)

cargo run --quiet -- advanced task lock ./examples/smoke-oci-judge \
  --out "$lock_dir/oci-task.lock.json" >/dev/null
docker image rm --force a3s-bench-smoke-judge:test >/dev/null
locked_oci_judge_output=$(cargo run --quiet -- run "$lock_dir/oci-task.lock.json" \
  --agent "$lock_dir/candidate.lock.json" --locked --json)

if failure=$(cargo run --quiet -- run ./examples/smoke \
  --agent ./examples/does-not-exist --json 2>&1); then
  echo "expected missing Candidate run to fail" >&2
  exit 1
fi
failed_run_id=$(printf '%s\n' "$failure" | sed -n \
  's/^a3s bench: run \(local-[A-Za-z0-9-]*\) failed:.*/\1/p')
test -n "$failed_run_id"
failed_output=$(cargo run --quiet -- result "$failed_run_id" --json)

python3 - "$root" "$local_output" "$oci_output" "$locked_output" \
  "$locked_oci_judge_output" "$failed_output" <<'PY'
import json
from pathlib import Path
import sys

root = Path(sys.argv[1])
for raw, expected_task in zip(
    sys.argv[2:6],
    ("smoke_answer", "smoke_oci_judge", "smoke_answer", "smoke_oci_judge"),
):
    value = json.loads(raw)
    assert value["schema"] == "a3s.bench.output.v1", value
    assert value["status"] == "completed", value
    assert value["task_id"] == expected_task, value
    assert value["score"] == "1", value
    journal_path = root / ".a3s" / "bench" / "runs" / f'{value["run_id"]}.json'
    journal = json.loads(journal_path.read_text())
    assert journal["schema"] == "a3s.bench.run-journal.v3", journal
    assert journal["task_lock_digest"].startswith("sha256:"), journal
    assert journal["candidate_lock_digest"].startswith("sha256:"), journal
    assert journal["stage"] == "completed", journal
    assert journal["result_path"] == value["result_path"], (journal, value)
    result = json.loads(Path(value["result_path"]).read_text())
    assert result["schema"] == "a3s.bench.local-result.v4", result
    assert result["result_digest"] == journal["result_digest"], (result, journal)
    assert result["task_lock_digest"] == journal["task_lock_digest"], (result, journal)
    assert result["candidate_lock_digest"] == journal["candidate_lock_digest"], (result, journal)

failed = json.loads(sys.argv[6])
assert failed["schema"] == "a3s.bench.output.v1", failed
assert failed["status"] == "failed", failed
assert "error" not in failed, failed
PY

cargo run --quiet -- result --json >/dev/null
echo "local Docker Runtime smoke passed (local, OCI, locked Agent Assets, and offline locked OCI Judge)"
