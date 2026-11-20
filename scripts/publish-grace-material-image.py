#!/usr/bin/env python3
"""Validate Grace boot files and publish them as a NICo PXE init image."""

from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path
import re
import shutil
import subprocess
import tempfile


EXPECTED = (
    "BUILD-INFO.json",
    "ROOTFS-PACKAGES.json",
    "golan.efi",
    "ipxe.efi",
    "scout.efi",
    "scout.squashfs",
)
SHA256 = re.compile(r"^[0-9a-f]{64}$")
DEFAULT_BASE_IMAGE = (
    "docker.io/library/alpine:3.20.10@"
    "sha256:d9e853e87e55526f6b2917df91a2115c36dd7c696a35be12163d44e6e2a4b6bc"
)


def digest(path: Path) -> str:
    """Return the SHA-256 digest of one material file."""
    value = hashlib.sha256()
    with path.open("rb") as stream:
        for block in iter(lambda: stream.read(8 * 1024**2), b""):
            value.update(block)
    return value.hexdigest()


def load_materials(directory: Path, revision: str) -> list[dict]:
    """Validate the generated recipe and return its fixed payload rows."""
    root = directory.resolve()
    recipe_path = root / "MATERIAL-RECIPE.json"
    recipe = json.loads(recipe_path.read_text())
    source = recipe.get("source", {})
    if source.get("core_commit") != revision:
        raise ValueError("Grace material Core revision differs from the publication revision")
    rows = recipe.get("materials")
    if not isinstance(rows, list) or tuple(row.get("target") for row in rows) != EXPECTED:
        raise ValueError("Grace material recipe does not contain the supported six-file layout")
    for row in rows:
        name = row["target"]
        path = root / name
        if path.is_symlink() or not path.is_file() or path.resolve().parent != root:
            raise ValueError("Missing or unsafe Grace material: " + name)
        if not isinstance(row.get("bytes"), int) or row["bytes"] <= 0:
            raise ValueError("Invalid Grace material size: " + name)
        if path.stat().st_size != row["bytes"]:
            raise ValueError("Grace material size differs: " + name)
        expected = row.get("sha256")
        if not isinstance(expected, str) or SHA256.fullmatch(expected) is None:
            raise ValueError("Invalid Grace material digest: " + name)
        if digest(path) != expected:
            raise ValueError("Grace material digest differs: " + name)
    return rows


def main() -> int:
    """Build and push the validated linux/amd64 material carrier image."""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--materials", required=True, type=Path)
    parser.add_argument("--image", required=True)
    parser.add_argument("--revision", required=True)
    parser.add_argument("--version", required=True)
    parser.add_argument("--source", default="https://github.com/SSmallMonster/infra-controller")
    parser.add_argument("--base-image", default=DEFAULT_BASE_IMAGE)
    args = parser.parse_args()

    if re.fullmatch(r"[0-9a-f]{40}", args.revision) is None:
        parser.error("--revision must be a full Git commit SHA")
    rows = load_materials(args.materials, args.revision)
    initializer = Path(__file__).with_name("grace-material-init.sh")
    if initializer.is_symlink() or not initializer.is_file():
        raise ValueError("Missing Grace material initializer")

    with tempfile.TemporaryDirectory(prefix="grace-material-image-") as temporary:
        context = Path(temporary)
        payload = context / "payload/aarch64"
        payload.mkdir(parents=True)
        for row in rows:
            shutil.copyfile(args.materials / row["target"], payload / row["target"])
        manifest = "".join(
            f'{row["sha256"]}\t{row["bytes"]}\t{row["target"]}\n' for row in rows
        )
        (context / "payload/manifest.tsv").write_text(manifest)
        shutil.copyfile(initializer, context / "grace-material-init.sh")
        (context / "Dockerfile").write_text(
            "ARG BASE_IMAGE\n"
            "FROM ${BASE_IMAGE}\n"
            "ARG OCI_SOURCE\n"
            "ARG VCS_REF\n"
            "ARG VERSION\n"
            'LABEL org.opencontainers.image.title="NICo Grace ARM64 boot artifacts" \\\n'
            '      org.opencontainers.image.source="${OCI_SOURCE}" \\\n'
            '      org.opencontainers.image.revision="${VCS_REF}" \\\n'
            '      org.opencontainers.image.version="${VERSION}" \\\n'
            '      org.opencontainers.image.licenses="Apache-2.0"\n'
            "COPY payload/ /payload/\n"
            "COPY grace-material-init.sh /usr/local/bin/grace-material-init\n"
            "RUN chmod 0755 /usr/local/bin/grace-material-init\n"
            'ENTRYPOINT ["/usr/local/bin/grace-material-init"]\n'
        )
        command = [
            "docker", "buildx", "build", "--platform", "linux/amd64", "--push",
            "--build-arg", "BASE_IMAGE=" + args.base_image,
            "--build-arg", "OCI_SOURCE=" + args.source,
            "--build-arg", "VCS_REF=" + args.revision,
            "--build-arg", "VERSION=" + args.version,
            "--tag", args.image, str(context),
        ]
        subprocess.run(command, check=True)
    print("PUBLISHED_GRACE_BOOT_IMAGE=" + args.image, flush=True)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
