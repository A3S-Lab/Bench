#!/usr/bin/env python3
"""Offline structural and provenance checks for the global builtin catalog."""

from __future__ import annotations

import hashlib
import json
import re
from collections import Counter
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1] / "builtin"
PROVENANCE_PATH = ROOT / "provenance" / "edgebench.json"
EXPECTED_DATASET_COMMIT = "47846a4c3669ad447e0ea984833b0d352460c5f9"
EXPECTED_HARNESS_COMMIT = "f59bcb0f024d4bc8baedeac271306050e4bb0d33"
EXPECTED_TASKS = 51
EXPECTED_MODES = {"batch": 48, "game_server": 3}


def sha256(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def require(condition: bool, message: str) -> None:
    if not condition:
        raise AssertionError(message)


def main() -> None:
    catalog = json.loads((ROOT / "catalog.json").read_text(encoding="utf-8"))
    provenance = json.loads(PROVENANCE_PATH.read_text(encoding="utf-8"))
    require(catalog["schema"] == "a3s-bench/builtin-catalog/v1", "catalog schema")
    require(set(catalog) == {"schema", "tasks"}, "catalog fields")
    require(provenance["dataset"]["commit"] == EXPECTED_DATASET_COMMIT, "dataset commit")
    require(provenance["harness"]["commit"] == EXPECTED_HARNESS_COMMIT, "harness commit")
    require(provenance["task_count"] == EXPECTED_TASKS, "provenance task count")
    require((ROOT / "licenses" / "CC-BY-4.0.txt").is_file(), "CC BY license")
    require((ROOT / "licenses" / "Apache-2.0.txt").is_file(), "Apache license")
    require((ROOT / "README.md").is_file(), "builtin README")
    require((ROOT / "THIRD_PARTY_NOTICES.md").is_file(), "third-party notices")

    records = {record["task_id"]: record for record in provenance["records"]}
    entries = [
        entry
        for entry in catalog["tasks"]
        if entry["provenance_ref"].startswith("provenance/edgebench.json#")
    ]
    require(len(entries) == EXPECTED_TASKS, "imported catalog task count")
    ids = [entry["id"] for entry in entries]
    require(ids == sorted(ids), "catalog ordering")
    require(len(ids) == len(set(ids)), "duplicate task id")
    require(set(records) == set(ids), "provenance records")

    modes: Counter[str] = Counter()
    model_gateway_count = 0
    for entry in entries:
        task_id = entry["id"]
        require(
            set(entry)
            == {
                "id",
                "path",
                "name",
                "category",
                "execution_class",
                "availability",
                "availability_reason",
                "admission",
                "admission_reason",
                "provenance_ref",
            },
            f"discovery-only catalog entry: {task_id}",
        )
        require(re.fullmatch(r"[a-z][a-z0-9_]{0,63}", task_id) is not None, f"task id: {task_id}")
        task_root = ROOT / "tasks" / task_id
        task_acl_path = task_root / "task.acl"
        prompt_path = task_root / "public" / "prompt.md"
        private_root = task_root / "private"
        asset_root = private_root / "judge"
        bundle_root = private_root / "bundle"
        asset_acl_path = asset_root / ".a3s" / "asset.acl"
        agent_path = asset_root / "agent.md"
        descriptor_path = asset_root / "judge.source.json"
        generated = {
            "task.acl": task_acl_path,
            "public/prompt.md": prompt_path,
            "private/judge/.a3s/asset.acl": asset_acl_path,
            "private/judge/agent.md": agent_path,
            "private/judge/judge.source.json": descriptor_path,
        }
        for path in generated.values():
            require(path.is_file(), f"missing {path.relative_to(ROOT)}")
            require(not path.is_symlink(), f"symlink forbidden: {path.relative_to(ROOT)}")
        require(not bundle_root.exists(), f"unavailable hidden bundle must be absent: {task_id}")
        require(
            {path.name for path in private_root.iterdir()} == {"judge"},
            f"canonical private layout: {task_id}",
        )

        record = records[task_id]
        require(re.fullmatch(r"sha256:[0-9a-f]{64}", record["source_sha256"]) is not None, f"source digest: {task_id}")
        require(record["modified"] is True, f"adaptation flag: {task_id}")
        for relative, path in generated.items():
            require(
                record["generated_sha256"][relative] == f"sha256:{sha256(path)}",
                f"generated digest: {task_id}/{relative}",
            )

        task_acl = task_acl_path.read_text(encoding="utf-8")
        asset_acl = asset_acl_path.read_text(encoding="utf-8")
        agent_md = agent_path.read_text(encoding="utf-8")
        descriptor = json.loads(descriptor_path.read_text(encoding="utf-8"))
        require(entry["path"] == f"tasks/{task_id}", f"path: {task_id}")
        require(entry["execution_class"] == "long_horizon", f"execution class: {task_id}")
        expected_availability = "ready"
        require(entry["availability"] == expected_availability, f"availability: {task_id}")
        expected_availability_reason = (
            "requires_configured_judge_model"
            if task_id == "college_english_exam_bank"
            else "bundled_oci_task"
        )
        require(
            entry["availability_reason"] == expected_availability_reason,
            f"availability reason: {task_id}",
        )
        require(entry["admission"] == "quarantined", f"catalog admission: {task_id}")
        require(
            entry["admission_reason"] == "official_evidence_incomplete",
            f"catalog admission reason: {task_id}",
        )
        require(task_acl.count("{") == task_acl.count("}"), f"ACL braces: {task_id}")
        require(task_acl.count("[") == task_acl.count("]"), f"ACL arrays: {task_id}")
        require(asset_acl.count("{") == asset_acl.count("}"), f"asset braces: {task_id}")
        require("dev_visible" not in task_acl, f"legacy dev visibility field: {task_id}")
        require(re.search(rf'^bench "{re.escape(task_id)}" \{{$', task_acl, re.MULTILINE) is not None, f"ACL id: {task_id}")
        require('metric "score"' in task_acl, f"native metric: {task_id}")
        require(re.search(r'^\s*asset\s*=\s*"private/judge"$', task_acl, re.MULTILINE) is not None, f"task Judge ref: {task_id}")
        require('version = "a3s.asset.v1"' in asset_acl, f"asset version: {task_id}")
        require('category = "agent"' in asset_acl, f"asset category: {task_id}")
        require('capability "bench.judge.v1"' in asset_acl, f"Judge capability: {task_id}")
        require('input_schema  = "bench.judge.request.v1"' in asset_acl, f"Judge input: {task_id}")
        require('output_schema = "bench.judge.result.v1"' in asset_acl, f"Judge output: {task_id}")
        require("benchmark {" not in asset_acl, f"private Judge dialect: {task_id}")
        expected_gateway = "scoped" if "model_gateway" in descriptor["requirements"] else "none"
        require(
            f'model_gateway = "{expected_gateway}"' in asset_acl,
            f"Judge ModelGateway capability: {task_id}",
        )
        require("EdgeBench" not in asset_acl, f"source-specific asset name: {task_id}")
        require("EdgeBench" not in agent_md, f"source-specific agent content: {task_id}")
        require("Bench runner" not in agent_md, f"private Bench runner wording: {task_id}")
        require(descriptor["schema"] == "a3s-bench/judge-source/v1", f"descriptor schema: {task_id}")
        require(descriptor["admission"] == "quarantined", f"descriptor admission: {task_id}")
        require(descriptor["kind"] == "oci", f"descriptor kind: {task_id}")
        require(descriptor["image"]["platform"] == "linux/amd64", f"platform: {task_id}")
        require("upstream" not in descriptor, f"duplicated provenance: {task_id}")
        modes[descriptor["evaluation"]["mode"]] += 1
        model_gateway_count += int("model_gateway" in descriptor["requirements"])

    require(dict(modes) == EXPECTED_MODES, "Judge modes")
    require(model_gateway_count == 1, "model-gateway Judge count")
    forbidden = [path for path in ROOT.rglob("*") if ".a3s-bench" in path.parts]
    require(not forbidden, "forbidden .a3s-bench path")
    require(not (ROOT / "edgebench").exists(), "source-specific builtin root")
    require(not (ROOT / "upstream").exists(), "upstream mirror directory")
    require(not list((ROOT / "tasks").glob("*/upstream.json")), "copied source records")
    legacy_layouts = [
        path
        for task_root in (ROOT / "tasks").iterdir()
        for path in (
            task_root / "private" / "judge" / "asset",
            task_root / "private" / "judge-dev",
            task_root / "private" / "judge-final",
        )
        if path.exists() or path.is_symlink()
    ]
    require(not legacy_layouts, "legacy Judge or phase bundle layout")
    print(f"checked {len(entries)} native long-horizon Task/Judge adapters")


if __name__ == "__main__":
    main()
