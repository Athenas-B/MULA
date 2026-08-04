#!/usr/bin/env python3
"""Setup script for VSD module dependencies.

Run this once to install the required Python packages:
    python setup.py

Or manually:
    pip install -r requirements.txt
"""

import subprocess
import sys
from pathlib import Path

HERE = Path(__file__).parent


def main():
    requirements = HERE / "requirements.txt"
    print(f"Installing VSD dependencies from {requirements}...")
    subprocess.check_call([
        sys.executable, "-m", "pip", "install",
        "-r", str(requirements),
        "--quiet",
    ])
    print("Done! VSD module dependencies installed.")


if __name__ == "__main__":
    main()
