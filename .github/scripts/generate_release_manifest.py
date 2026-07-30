#!/usr/bin/env python3
"""Generates manifest.json for a Tanuki GitHub Release.

Pulls this release's own section out of CHANGELOG.md (from its
"## [X.Y.Z] - ..." heading to the next "## [" heading or EOF) so the
in-app "What's New" (notes_body in src/update.rs, rendered by
src/ui/release_notes.rs) shows the real changelog instead of a
generic placeholder string.

Usage: generate_release_manifest.py <version> <repo> <tag> <artifacts_dir>
"""

import hashlib
import json
import pathlib
import re
import sys

ASSET_TARGETS = [
    "tanuki-windows-x86_64.exe",
    "tanuki-linux-x86_64",
    "tanuki-linux-aarch64",
    "tanuki-macos-x86_64",
    "tanuki-macos-aarch64",
]

PROTOCOL_SOURCE_PATH = pathlib.Path("src/protocol/wire.rs")


def read_protocol_version() -> int:
    content = PROTOCOL_SOURCE_PATH.read_text(encoding="utf-8")
    match = re.search(r"pub const PROTOCOL_VERSION: u32 = (\d+);", content)
    if not match:
        raise SystemExit(f"could not read PROTOCOL_VERSION from {PROTOCOL_SOURCE_PATH}")
    return int(match.group(1))


def changelog_notes_for(version: str) -> str:
    text = pathlib.Path("CHANGELOG.md").read_text(encoding="utf-8")
    pattern = re.compile(
        r"^## \[" + re.escape(version) + r"\].*?\n(.*?)(?=^## \[|\Z)",
        re.MULTILINE | re.DOTALL,
    )
    match = pattern.search(text)
    return match.group(1).strip() if match else ""


def main() -> None:
    version, repo, tag, artifacts_dir = sys.argv[1:5]
    artifacts = pathlib.Path(artifacts_dir)

    notes = changelog_notes_for(version)
    if not notes:
        print(
            f"::warning::no CHANGELOG.md section found for {tag}; "
            "falling back to a generic notes string",
            file=sys.stderr,
        )
        notes = f"Tanuki {tag}"

    assets = {}
    for target in ASSET_TARGETS:
        key = target.removeprefix("tanuki-").removesuffix(".exe")
        file = artifacts / target
        sha256 = hashlib.sha256(file.read_bytes()).hexdigest()
        url = f"https://github.com/{repo}/releases/download/{tag}/{target}"
        assets[key] = {"url": url, "sha256": sha256}

    manifest = {
        "version": version,
        "protocol": read_protocol_version(),
        "notes": notes,
        "announcement": None,
        "assets": assets,
    }
    print(json.dumps(manifest, indent=2))


if __name__ == "__main__":
    main()
