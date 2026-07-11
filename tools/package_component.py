#!/usr/bin/env python3
"""Build the private Bench component payload consumed by the top-level a3s CLI."""

from __future__ import annotations

import hashlib
import json
import platform
import shutil
import subprocess
import tarfile
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
VERSION = "0.1.0"


def release_target() -> str:
    os_name = {"Darwin": "darwin", "Linux": "linux"}.get(platform.system())
    machine = {"arm64": "arm64", "aarch64": "arm64", "x86_64": "x86_64"}.get(
        platform.machine()
    )
    if not os_name or not machine:
        raise SystemExit(f"unsupported release target: {platform.system()}-{platform.machine()}")
    return f"{os_name}-{machine}"


def main() -> None:
    target = release_target()
    subprocess.run(["cargo", "build", "--release", "--locked"], cwd=ROOT, check=True)
    binary = ROOT / "target" / "release" / "a3s-bench"
    package_name = f"a3s-bench-{VERSION}-{target}"
    package_root = ROOT / "dist" / package_name
    if package_root.exists():
        shutil.rmtree(package_root)
    (package_root / "bin").mkdir(parents=True)
    shutil.copy2(binary, package_root / "bin" / "a3s-bench")
    manifest = {
        "schema": "a3s.component.v1",
        "component": "bench",
        "version": VERSION,
        "target": target,
        "cli_protocol": "a3s-bench-cli/v1",
        "entrypoint": "bin/a3s-bench",
        "required_files": [],
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
