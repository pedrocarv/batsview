#!/usr/bin/env python3
"""Build the self-contained desktop package for the current platform."""

from __future__ import annotations

import argparse
import platform
import shutil
import subprocess
import sys
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
DEFAULT_FORMAT = {
    "Darwin": "app",
    "Windows": "nsis",
    "Linux": "appimage",
}


def run(command: list[str]) -> None:
    print("+", " ".join(command), flush=True)
    subprocess.run(command, cwd=ROOT, check=True)


def main() -> int:
    parser = argparse.ArgumentParser(
        description="Build a double-clickable BATSView desktop package."
    )
    parser.add_argument(
        "--format",
        choices=("app", "dmg", "nsis", "wix", "appimage", "deb"),
        default=DEFAULT_FORMAT.get(platform.system()),
        help="package format (defaults to the native format for this platform)",
    )
    args = parser.parse_args()
    if args.format is None:
        parser.error("unsupported platform; pass --format explicitly")
    if shutil.which("cargo-packager") is None:
        parser.error(
            "cargo-packager is required; install it with "
            "`cargo install cargo-packager --locked --version 0.11.8`"
        )

    run(
        [
            sys.executable,
            "-m",
            "PyInstaller",
            "--noconfirm",
            "--clean",
            "--onedir",
            "--name",
            "batsview-bridge",
            "--exclude-module",
            "matplotlib",
            "--exclude-module",
            "pyvista",
            "--exclude-module",
            "pygame",
            "--exclude-module",
            "PIL",
            "bridge/batsview_bridge.py",
        ]
    )
    run(["cargo", "packager", "--release", "--formats", args.format])
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
