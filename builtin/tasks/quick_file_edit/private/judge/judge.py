"""Offline Judge for the quick file-edit conformance task."""

from pathlib import Path


def evaluate(context):
    """Return the locked correctness metric."""
    answer_path = Path(context["submission_root"]) / "answer.txt"
    expected_path = Path(context["hidden_bundle_root"]) / "expected.txt"
    actual = answer_path.read_text(encoding="utf-8").strip() if answer_path.exists() else ""
    expected = expected_path.read_text(encoding="utf-8").strip()
    return {
        "schema": "bench.judge.result.v1",
        "solution_verdict": "valid",
        "metrics": {"correctness": "1" if actual == expected else "0"},
        "diagnostics": {"answer_present": answer_path.exists()},
    }
