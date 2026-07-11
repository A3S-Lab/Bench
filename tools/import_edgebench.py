#!/usr/bin/env python3
"""Convert the pinned public EdgeBench records into native builtin sources.

EdgeBench is an upstream provenance source, not an a3s-bench subsystem. The
generated output joins the single global catalog; admitted built-ins resolve by
bare task ID. OCI images are referenced but never pulled or redistributed.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import shutil
import subprocess
from pathlib import Path
from typing import Any


DATASET_REPOSITORY = "https://huggingface.co/datasets/ByteDance-Seed/EdgeBench"
DATASET_COMMIT = "47846a4c3669ad447e0ea984833b0d352460c5f9"
HARNESS_REPOSITORY = "https://github.com/ByteDance-Seed/EdgeBench"
HARNESS_COMMIT = "f59bcb0f024d4bc8baedeac271306050e4bb0d33"
TASK_COUNT = 51
MODEL_GATEWAY_TASKS = {"college_english_exam_bank"}
PROVENANCE_REF = "provenance/edgebench.json"


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--dataset-dir", type=Path, required=True)
    parser.add_argument("--harness-dir", type=Path, required=True)
    parser.add_argument(
        "--output",
        type=Path,
        default=Path(__file__).resolve().parents[1] / "builtin",
    )
    return parser.parse_args()


def git_head(path: Path) -> str:
    return subprocess.run(
        ["git", "rev-parse", "HEAD"],
        cwd=path,
        check=True,
        capture_output=True,
        text=True,
    ).stdout.strip()


def sha256_bytes(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def sha256_file(path: Path) -> str:
    return sha256_bytes(path.read_bytes())


def json_text(value: Any) -> str:
    return json.dumps(value, ensure_ascii=False, indent=2, sort_keys=True) + "\n"


def write_text(path: Path, value: str) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(value, encoding="utf-8")


def acl_string(value: str) -> str:
    return json.dumps(value, ensure_ascii=False)


def acl_list(values: list[str], indent: str = "  ") -> str:
    if not values:
        return "[]"
    rows = ["["]
    rows.extend(f"{indent}{acl_string(value)}," for value in values)
    rows.append(indent[:-2] + "]" if len(indent) >= 2 else "]")
    return "\n".join(rows)


def image_ref(task_id: str, role: str, tag: str) -> str:
    return f"docker.io/seededge/edgebench.{role}.{task_id}:{tag}"


def normalized_excludes(task: dict[str, Any]) -> list[str]:
    return [value.rstrip("/") for value in task.get("submit_exclude", ["tests/"])]


def judge_mode(task: dict[str, Any]) -> str:
    return "game_server" if task.get("game_mode", False) else "batch"


def judge_requirements(task: dict[str, Any]) -> list[str]:
    requirements = ["oci_judge_admission"]
    if task.get("game_mode", False):
        requirements.append("interactive_candidate_channel")
    if task["task_id"] in MODEL_GATEWAY_TASKS:
        requirements.append("model_gateway")
    return requirements


def render_task_acl(task: dict[str, Any]) -> str:
    task_id = task["task_id"]
    work_ref = image_ref(task_id, "work", task["work"]["image_tag"])
    network = "public_internet" if task.get("internet", True) else "none"
    include = acl_list(task["submit_paths"], indent="      ")
    exclude = acl_list(normalized_excludes(task), indent="      ")
    timeout = int(task["judge"].get("eval_timeout", 600))
    return f'''# Third-party source details: ../../{PROVENANCE_REF}
bench {acl_string(task_id)} {{
  schema  = "a3s-bench/task/v1"
  version = "0.1.0"

  name        = {acl_string(task["name"])}
  category    = {acl_string(task["category"])}
  description = "Complete the task described in public/prompt.md."

  workspace {{
    oci {{
      ref         = {acl_string(work_ref)}
      platform    = {acl_string(task["platform"])}
      source_path = {acl_string(task["cwd"])}
    }}
  }}

  work {{
    image {{
      ref = {acl_string(work_ref)}
    }}
    network_need = {acl_string(network)}
  }}

  submission {{
    include = {include}
    exclude = {exclude}
  }}

  judge {{
    asset                = "private/judge"
    solution_timeout_sec = {timeout}
  }}

  metric "score" {{
    type                   = "ratio"
    role                   = "primary"
    direction              = "maximize"
    min                    = 0
    max                    = 1
    normalization          = "linear_range_v1"
    solution_failure_value = "0"
    public_report          = true
  }}
}}
'''


def render_asset_acl(task: dict[str, Any]) -> str:
    task_id = task["task_id"]
    asset_name = f"{task_id.replace('_', '-')}-judge"
    model_gateway = "scoped" if task_id in MODEL_GATEWAY_TASKS else "none"
    return f'''version = "a3s.asset.v1"
category = "agent"
kind = "tool"
name = {acl_string(asset_name)}
description = {acl_string(f"Quarantined Judge source for {task_id}.")}
service = "Function as a Service"
created_by = "a3s-bench"

source {{
  package_path = "."
  entrypoint = "agent.md"
  definition_path = "agent.md"
}}

metadata {{
  asset_acl_path = ".a3s/asset.acl"
}}

runtime {{
  kind = "tool"
  isolation = "serving"
  runtime_kind = "a3s-function-service"
  protocol = "agent-tool"
  agent_kind = "tool"
}}

capability "bench.judge.v1" {{
  input_schema  = "bench.judge.request.v1"
  output_schema = "bench.judge.result.v1"
  network       = "none"
  model_gateway = {acl_string(model_gateway)}
}}
'''


def render_agent_md(task: dict[str, Any]) -> str:
    task_id = task["task_id"]
    asset_name = f"{task_id.replace('_', '-')}-judge"
    return f'''---
name: {asset_name}
description: Quarantined Judge source for {task_id}.
tools: []
max_steps: 1
---

# Judge source

This A3S agent asset records an upstream evaluator source and its requirements.
It is not admitted for execution. Bench compilation must reject it until a
signed admission binds an audited standard Agent Asset implementation,
immutable image digests, typed result handling, third-party licenses, and A3S
OS Runtime behavior.
'''


def judge_descriptor(task: dict[str, Any]) -> dict[str, Any]:
    task_id = task["task_id"]
    judge = task["judge"]
    return {
        "schema": "a3s-bench/judge-source/v1",
        "admission": "quarantined",
        "kind": "oci",
        "image": {
            "ref": image_ref(task_id, "judge", judge["image_tag"]),
            "platform": task["platform"],
            "digest_resolution": "required_at_task_lock",
        },
        "workspace": {
            "source_path": task["cwd"],
            "submission_paths": task["submit_paths"],
            "submission_exclude": normalized_excludes(task),
        },
        "evaluation": {
            "mode": judge_mode(task),
            "source_command": judge.get("eval_cmd", ""),
            "source_game_server_command": judge.get("game_server_cmd"),
            "timeout_sec": int(judge.get("eval_timeout", 600)),
        },
        "source_result": {
            "kind": "legacy_stdout",
            "parser": judge.get("parser", ""),
            "selection_hint": judge.get("selection", "pass_rate_first"),
            "score_direction": judge.get("score_direction", "maximize"),
            "rescale_hint": judge.get("rescale"),
            "target_metric": "score",
        },
        "requirements": judge_requirements(task),
    }


def catalog_entry(task: dict[str, Any]) -> dict[str, Any]:
    task_id = task["task_id"]
    return {
        "id": task_id,
        "path": f"tasks/{task_id}",
        "name": task["name"],
        "category": task["category"],
        "execution_class": "long_horizon",
        "availability": "ready",
        "availability_reason": (
            "requires_configured_judge_model"
            if task_id == "college_english_exam_bank"
            else "bundled_oci_task"
        ),
        "admission": "quarantined",
        "admission_reason": "official_evidence_incomplete",
        "provenance_ref": f"{PROVENANCE_REF}#{task_id}",
    }


def assert_source_revisions(dataset_dir: Path, harness_dir: Path) -> None:
    dataset_head = git_head(dataset_dir)
    harness_head = git_head(harness_dir)
    if dataset_head != DATASET_COMMIT:
        raise SystemExit(
            f"dataset revision mismatch: expected {DATASET_COMMIT}, got {dataset_head}"
        )
    if harness_head != HARNESS_COMMIT:
        raise SystemExit(
            f"harness revision mismatch: expected {HARNESS_COMMIT}, got {harness_head}"
        )


def load_existing_catalog(output: Path) -> list[dict[str, Any]]:
    path = output / "catalog.json"
    if not path.exists():
        return []
    catalog = json.loads(path.read_text(encoding="utf-8"))
    if catalog.get("schema") != "a3s-bench/builtin-catalog/v1":
        raise SystemExit(f"unsupported existing builtin catalog: {path}")
    return list(catalog.get("tasks", []))


def load_managed_ids(output: Path) -> set[str]:
    path = output / PROVENANCE_REF
    if not path.exists():
        return set()
    provenance = json.loads(path.read_text(encoding="utf-8"))
    return {record["task_id"] for record in provenance.get("records", [])}


def remove_legacy_layout(task_root: Path) -> None:
    """Remove only source-managed layouts superseded by the canonical P1 tree."""
    legacy_paths = (
        task_root / "private" / "judge" / "asset",
        task_root / "private" / "judge-dev",
        task_root / "private" / "judge-final",
    )
    for path in legacy_paths:
        if path.is_symlink() or path.is_file():
            path.unlink()
        elif path.is_dir():
            shutil.rmtree(path)


def remove_unavailable_bundle(task_root: Path) -> None:
    """Do not turn unavailable upstream hidden bytes into a fake empty bundle."""
    bundle_root = task_root / "private" / "bundle"
    if bundle_root.is_symlink() or bundle_root.is_file():
        raise SystemExit(f"unexpected hidden bundle path: {bundle_root}")
    if bundle_root.is_dir():
        if any(bundle_root.iterdir()):
            raise SystemExit(
                f"refusing non-empty hidden bundle for quarantined source: {task_root.name}"
            )
        bundle_root.rmdir()


def main() -> None:
    args = parse_args()
    dataset_dir = args.dataset_dir.resolve()
    harness_dir = args.harness_dir.resolve()
    output = args.output.resolve()
    assert_source_revisions(dataset_dir, harness_dir)

    task_paths = sorted(dataset_dir.glob("*.json"))
    if len(task_paths) != TASK_COUNT:
        raise SystemExit(f"expected {TASK_COUNT} task JSON files, found {len(task_paths)}")

    tasks: list[tuple[dict[str, Any], Path]] = []
    ids: set[str] = set()
    for task_path in task_paths:
        task = json.loads(task_path.read_text(encoding="utf-8"))
        task_id = task["task_id"]
        if task_path.stem != task_id:
            raise SystemExit(f"task filename/id mismatch: {task_path.name} / {task_id}")
        if task_id in ids:
            raise SystemExit(f"duplicate task id: {task_id}")
        ids.add(task_id)
        tasks.append((task, task_path))

    output.mkdir(parents=True, exist_ok=True)
    existing_entries = load_existing_catalog(output)
    managed_ids = load_managed_ids(output)
    for entry in existing_entries:
        if entry["id"] in ids and entry["id"] not in managed_ids:
            raise SystemExit(f"builtin task id already owned by another source: {entry['id']}")
    for task_id in ids:
        task_root = output / "tasks" / task_id
        if task_root.exists() and task_id not in managed_ids:
            raise SystemExit(f"refusing to overwrite unmanaged builtin task: {task_id}")

    (output / "licenses").mkdir(parents=True, exist_ok=True)
    shutil.copyfile(dataset_dir / "LICENSE", output / "licenses" / "CC-BY-4.0.txt")
    shutil.copyfile(harness_dir / "LICENSE", output / "licenses" / "Apache-2.0.txt")

    new_entries: list[dict[str, Any]] = []
    source_records: list[dict[str, Any]] = []
    for task, task_path in tasks:
        task_id = task["task_id"]
        task_root = output / "tasks" / task_id
        remove_legacy_layout(task_root)
        remove_unavailable_bundle(task_root)
        task_acl_path = task_root / "task.acl"
        prompt_path = task_root / "public" / "prompt.md"
        asset_root = task_root / "private" / "judge"
        asset_acl_path = asset_root / ".a3s" / "asset.acl"
        agent_path = asset_root / "agent.md"
        descriptor_path = asset_root / "judge.source.json"

        write_text(task_acl_path, render_task_acl(task))
        write_text(prompt_path, task["work"]["agent_query"])
        write_text(asset_acl_path, render_asset_acl(task))
        write_text(agent_path, render_agent_md(task))
        write_text(descriptor_path, json_text(judge_descriptor(task)))

        new_entries.append(catalog_entry(task))
        source_records.append(
            {
                "task_id": task_id,
                "source_path": task_path.name,
                "source_sha256": f"sha256:{sha256_file(task_path)}",
                "license": "CC-BY-4.0",
                "modified": True,
                "generated_sha256": {
                    "task.acl": f"sha256:{sha256_file(task_acl_path)}",
                    "public/prompt.md": f"sha256:{sha256_file(prompt_path)}",
                    "private/judge/.a3s/asset.acl": f"sha256:{sha256_file(asset_acl_path)}",
                    "private/judge/agent.md": f"sha256:{sha256_file(agent_path)}",
                    "private/judge/judge.source.json": f"sha256:{sha256_file(descriptor_path)}",
                },
            }
        )

    other_entries = [entry for entry in existing_entries if entry["id"] not in managed_ids]
    catalog = {
        "schema": "a3s-bench/builtin-catalog/v1",
        "tasks": sorted(other_entries + new_entries, key=lambda entry: entry["id"]),
    }
    write_text(output / "catalog.json", json_text(catalog))

    provenance = {
        "schema": "a3s-bench/builtin-provenance/v1",
        "source": "EdgeBench",
        "dataset": {
            "repository": DATASET_REPOSITORY,
            "commit": DATASET_COMMIT,
            "license": "CC-BY-4.0",
        },
        "harness": {
            "repository": HARNESS_REPOSITORY,
            "commit": HARNESS_COMMIT,
            "license": "Apache-2.0",
        },
        "task_count": len(source_records),
        "records": source_records,
        "adaptation": [
            "Converted source task fields into native a3s-bench TaskBundle files.",
            "Copied each source agent query verbatim to public/prompt.md.",
            "Generated one quarantined A3S Judge Agent Asset at private/judge per task.",
            "Marked the terminal hidden bundle unavailable; no upstream hidden bytes were copied or represented as an empty bundle.",
            "Renamed the normalized primary metric to the native name score.",
        ],
        "redistribution": (
            "Upstream OCI images are referenced, not copied or redistributed."
        ),
    }
    write_text(output / PROVENANCE_REF, json_text(provenance))

    modes: dict[str, int] = {}
    for task, _ in tasks:
        mode = judge_mode(task)
        if task["task_id"] in MODEL_GATEWAY_TASKS:
            mode = "model_gateway"
        modes[mode] = modes.get(mode, 0) + 1
    print(json.dumps({"tasks": len(new_entries), "requirements": modes}, sort_keys=True))


if __name__ == "__main__":
    main()
