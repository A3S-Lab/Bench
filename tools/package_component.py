#!/usr/bin/env python3
"""Build the private Bench component payload consumed by the top-level a3s CLI."""

from __future__ import annotations

import hashlib
import json
import os
import platform
import re
import shutil
import subprocess
import tarfile
import tempfile
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]

def package_version() -> str:
    manifest = (ROOT / "Cargo.toml").read_text(encoding="utf-8")
    package = manifest.split("[dependencies]", 1)[0]
    match = re.search(r'^version\s*=\s*"([^"]+)"\s*$', package, re.MULTILINE)
    if not match:
        raise SystemExit("could not read package version from Cargo.toml")
    return match.group(1)


def release_target() -> str:
    os_name = {"Darwin": "darwin", "Linux": "linux"}.get(platform.system())
    machine = {"arm64": "arm64", "aarch64": "arm64", "x86_64": "x86_64"}.get(
        platform.machine()
    )
    if not os_name or not machine:
        raise SystemExit(f"unsupported release target: {platform.system()}-{platform.machine()}")
    return f"{os_name}-{machine}"


def main() -> None:
    version = package_version()
    target = release_target()
    subprocess.run(["cargo", "build", "--release", "--locked"], cwd=ROOT, check=True)
    binary = ROOT / "target" / "release" / "a3s-bench"
    package_name = f"a3s-bench-{version}-{target}"
    package_root = ROOT / "dist" / package_name
    if package_root.exists():
        shutil.rmtree(package_root)
    (package_root / "bin").mkdir(parents=True)
    shutil.copy2(binary, package_root / "bin" / "a3s-bench")
    shutil.copytree(ROOT / "builtin", package_root / "builtin")
    manifest = {
        "schema": "a3s.component.v1",
        "component": "bench",
        "version": version,
        "target": target,
        "cli_protocol": "a3s-bench-cli/v1",
        "entrypoint": "bin/a3s-bench",
        "required_files": ["builtin/catalog.json", "builtin/tasks"],
    }
    (package_root / "component.json").write_text(
        json.dumps(manifest, indent=2, sort_keys=True) + "\n", encoding="utf-8"
    )
    probe = subprocess.run(
        [package_root / "bin" / "a3s-bench", "--component-info", "--json"],
        check=True,
        capture_output=True,
        text=True,
    )
    identity = json.loads(probe.stdout)
    for key in ("component", "version", "target", "cli_protocol"):
        if identity[key] != manifest[key]:
            raise SystemExit(f"component probe mismatch for {key}")

    with tempfile.TemporaryDirectory() as working_directory:
        listing = subprocess.run(
            [package_root / "bin" / "a3s-bench", "list", "--all", "--json"],
            cwd=working_directory,
            check=True,
            capture_output=True,
            text=True,
        )
        core_candidate_lock = (
            Path(working_directory) / "a3s-code-core.candidate.lock.json"
        )
        subprocess.run(
            [
                package_root / "bin" / "a3s-bench",
                "advanced",
                "candidate",
                "lock",
                "a3s-code-core",
                "--model",
                "test/model",
                "--out",
                core_candidate_lock,
            ],
            cwd=working_directory,
            check=True,
            capture_output=True,
            text=True,
        )
        locked_core_candidate = json.loads(
            core_candidate_lock.read_text(encoding="utf-8")
        )
        if (
            locked_core_candidate["schema"] != "a3s.bench.candidate-lock.v1"
            or locked_core_candidate["model"] != "test/model"
        ):
            raise SystemExit("packaged A3S Code Core Candidate binding is invalid")

        fake_bin = Path(working_directory) / "fake-bin"
        fake_bin.mkdir()
        fake_a3s = fake_bin / "a3s"
        fake_a3s.write_text(
            "#!/bin/sh\n"
            "if [ \"$1\" = \"--version\" ]; then\n"
            "  echo 'a3s 0.12.5'\n"
            "else\n"
            "  echo '  --tool-policy <standard|local-workspace>'\n"
            "fi\n",
            encoding="utf-8",
        )
        fake_a3s.chmod(0o700)
        product_candidate_lock = (
            Path(working_directory) / "a3s-code.candidate.lock.json"
        )
        product_env = os.environ.copy()
        product_env["PATH"] = f"{fake_bin}{os.pathsep}{product_env['PATH']}"
        subprocess.run(
            [
                package_root / "bin" / "a3s-bench",
                "advanced",
                "candidate",
                "lock",
                "a3s-code",
                "--model",
                "test/model",
                "--out",
                product_candidate_lock,
            ],
            cwd=working_directory,
            env=product_env,
            check=True,
            capture_output=True,
            text=True,
        )
        locked_product_candidate = json.loads(
            product_candidate_lock.read_text(encoding="utf-8")
        )
        if (
            locked_product_candidate["schema"] != "a3s.bench.candidate-lock.v2"
            or locked_product_candidate["model"] != "test/model"
            or locked_product_candidate["product"]
            != {"name": "a3s-cli", "version": "a3s 0.12.5"}
        ):
            raise SystemExit("packaged A3S Code product Candidate binding is invalid")
    packaged_catalog = json.loads(listing.stdout)
    source_catalog = json.loads((ROOT / "builtin" / "catalog.json").read_text())
    if len(packaged_catalog["data"]["tasks"]) != len(source_catalog["tasks"]):
        raise SystemExit("packaged built-in catalog is incomplete")

    archive = ROOT / "dist" / f"{package_name}.tar.gz"
    with tarfile.open(archive, "w:gz") as output:
        output.add(package_root, arcname=package_name)
    digest = hashlib.sha256(archive.read_bytes()).hexdigest()
    archive.with_suffix(archive.suffix + ".sha256").write_text(
        f"{digest}  {archive.name}\n", encoding="ascii"
    )
    print(archive)


if __name__ == "__main__":
    main()
