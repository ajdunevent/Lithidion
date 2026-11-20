#!/usr/bin/env python3
"""Writes build-aux/cargo-sources.json, the crate list for the offline Flatpak build, from
Cargo.lock. Standard library only (Python 3.11+). Handles crates.io crates, which is all
Lithidion uses; a git or other source stops it with an error.

Usage, from the project root: python3 build-aux/cargo-sources.py
"""

import json
import sys
import tomllib
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
CRATES_IO = "registry+https://github.com/rust-lang/crates.io-index"
VENDOR = "cargo/vendor"


def main() -> int:
    lock = tomllib.loads((ROOT / "Cargo.lock").read_text())
    sources = []
    for package in sorted(lock.get("package", []), key=lambda p: (p["name"], p["version"])):
        source = package.get("source")
        if source is None:
            continue  # a workspace crate
        if source != CRATES_IO:
            print(f"{package['name']} {package['version']}: unsupported source {source}", file=sys.stderr)
            return 1
        name, version, checksum = package["name"], package["version"], package["checksum"]
        dest = f"{VENDOR}/{name}-{version}"
        sources.append({
            "type": "archive",
            "archive-type": "tar-gzip",
            "url": f"https://static.crates.io/crates/{name}/{name}-{version}.crate",
            "sha256": checksum,
            "dest": dest,
        })
        sources.append({
            "type": "inline",
            "contents": json.dumps({"package": checksum, "files": {}}),
            "dest": dest,
            "dest-filename": ".cargo-checksum.json",
        })
    # Relative to the parent of CARGO_HOME, which the manifest sets to the module's cargo/.
    config = (
        "[source.vendored-sources]\n"
        f'directory = "{VENDOR}"\n'
        "\n"
        "[source.crates-io]\n"
        'replace-with = "vendored-sources"\n'
    )
    sources.append({"type": "inline", "contents": config, "dest": "cargo", "dest-filename": "config"})
    out = ROOT / "build-aux" / "cargo-sources.json"
    out.write_text(json.dumps(sources, indent=4) + "\n")
    print(f"Wrote {out.relative_to(ROOT)}: {(len(sources) - 1) // 2} crates")
    return 0


if __name__ == "__main__":
    sys.exit(main())
