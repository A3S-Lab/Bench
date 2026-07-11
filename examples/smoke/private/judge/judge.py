"""Standard entrypoint for the smoke A3S Judge agent asset."""

from pathlib import Path


def evaluate(context):
    """Return a bounded result through the Runtime-protected result channel."""
    answer_path = Path(context["submission_root"]) / "answer.txt"
    expected_path = Path(context["hidden_bundle_root"]) / "expected.txt"

    expected = expected_path.read_text(encoding="utf-8").strip()
    actual = (
        answer_path.read_text(encoding="utf-8").strip()
        if answer_path.exists()
        else ""
    )
    correct = actual == expected

    return {
        "schema": "bench.judge.result.v1",
        # `valid` means the protected measurement is authoritative. Correctness
        # itself is represented by the locked metric, not a second status enum.
        "solution_verdict": "valid",
        "metrics": {"correctness": "1" if correct else "0"},
        "diagnostics": {"answer_present": answer_path.exists()},
    }
