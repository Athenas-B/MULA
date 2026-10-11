#!/usr/bin/env python3
"""Build VSD browser extensions.

Combines shared source files with browser-specific manifests into
ready-to-load extension directories.

Usage:
    python build.py          # Build both
    python build.py chrome   # Chrome only
    python build.py firefox  # Firefox only
    python build.py chrome --server-url http://127.0.0.1:9000   # bake in another server URL
"""

import re
import shutil
import sys
from pathlib import Path

HERE = Path(__file__).parent
SRC = HERE / "src"
DIST = HERE / "dist"


DEFAULT_SERVER_LINE = re.compile(r"^const VSD_DEFAULT_SERVER = '[^']*';$", re.MULTILINE)


def set_default_server(config_js: Path, server_url: str):
    """Point the built extension's default server at `server_url` (MULA's configured port)."""
    text = config_js.read_text(encoding="utf-8")
    updated, count = DEFAULT_SERVER_LINE.subn(f"const VSD_DEFAULT_SERVER = '{server_url}';", text)
    if count != 1:
        raise ValueError(f"VSD_DEFAULT_SERVER not found in {config_js}")
    config_js.write_text(updated, encoding="utf-8")


def build(browser: str, server_url: str | None = None):
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

    if server_url:
        set_default_server(out / "config.js", server_url)

    print(f"Built: {out}")
    return True


def main():
    args = sys.argv[1:]
    server_url = None
    if "--server-url" in args:
        i = args.index("--server-url")
        if i + 1 >= len(args):
            print("Error: --server-url needs a value")
            sys.exit(1)
        server_url = args[i + 1]
        del args[i:i + 2]
    targets = args or ["chrome", "firefox"]
    for target in targets:
        if not build(target, server_url):
            sys.exit(1)
    print("\nDone! Load extensions from:")
    for target in targets:
        print(f"  {DIST / target}")


if __name__ == "__main__":
    main()
