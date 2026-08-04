#!/usr/bin/env python3
"""Build VSD browser extensions.

Combines shared source files with browser-specific manifests into
ready-to-load extension directories.

Usage:
    python build.py          # Build both
    python build.py chrome   # Chrome only
    python build.py firefox  # Firefox only
"""

import shutil
import sys
from pathlib import Path

HERE = Path(__file__).parent
SRC = HERE / "src"
DIST = HERE / "dist"


def build(browser: str):
    manifest_dir = HERE / browser
    if not manifest_dir.exists():
        print(f"Error: {manifest_dir} not found")
        return False

    out = DIST / browser
    if out.exists():
        shutil.rmtree(out)
    out.mkdir(parents=True)

    # Copy all shared source files
    for item in SRC.rglob("*"):
        if item.is_file():
            dest = out / item.relative_to(SRC)
            dest.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(item, dest)

    # Copy browser-specific manifest (overrides any in src/)
    shutil.copy2(manifest_dir / "manifest.json", out / "manifest.json")

    print(f"Built: {out}")
    return True


def main():
    targets = sys.argv[1:] or ["chrome", "firefox"]
    for target in targets:
        if not build(target):
            sys.exit(1)
    print("\nDone! Load extensions from:")
    for target in targets:
        print(f"  {DIST / target}")


if __name__ == "__main__":
    main()
