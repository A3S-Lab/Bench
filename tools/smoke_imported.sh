#!/bin/sh
set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$root"

docker version --format '{{.Server.Version}}' >/dev/null
output=$(cargo run --quiet -- run \
  juliet_vulnerability_analyzer \
  --agent ./examples/smoke-candidate --json)

python3 - "$output" <<'PY'
import json
import sys

value = json.loads(sys.argv[1])
assert value["schema"] == "a3s.bench.output.v1", value
assert value["command"] == "run" and value["ok"] is True, value
data = value["data"]
assert data["status"] == "completed", value
assert data["governance_status"] == "local_unofficial", value
assert data["task_id"] == "juliet_vulnerability_analyzer", value
score = float(data["score"])
assert 0.0 <= score <= 1.0, value
PY

echo "imported OCI Task/Judge smoke passed (juliet_vulnerability_analyzer)"
