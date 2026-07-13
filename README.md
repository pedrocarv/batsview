# BATSView

A fast, standalone desktop browser and 2-D viewer for BATS-R-US Tecplot files.
The interface and renderer are native Rust. The existing Python `batsplot`
package remains the single source of truth for TDV112 parsing and variable
aliases through a small process boundary.

## Why this architecture

- `egui` provides one native UI codebase for Windows, Linux, and macOS.
- `wgpu` uses DirectX 12, Vulkan, or Metal and keeps the finite-element mesh on
  the GPU while the user pans, zooms, and changes display controls.
- The bridge asks `batsplot` to read only the selected scalar and coordinates.
- A compact, versioned `.bpv` exchange file avoids JSON encoding large arrays.
- Bridge work runs outside the UI process, so a malformed or very large file
  cannot freeze the interface.

This first milestone supports directory browsing, timestep navigation,
metadata inspection, searchable source/canonical variable names, selective
variable loading, linear/log color scaling, limits, and GPU pan/zoom/reset.

## Development

Requirements:

- Rust stable (1.85 or newer)
- Python 3.10 or newer
- A local or installed copy of `batsplot`

From this directory:

```bash
# When batsplot is installed in the current Python environment:
cargo run --release

# Or point the bridge at a batsplot checkout:
BATSPLOT_SOURCE=/path/to/batsplot/src cargo run --release

# Open a run directory or one .plt file immediately:
cargo run --release -- /path/to/run
```

The viewer honors these environment variables:

- `BATSVIEW_BRIDGE`: path to a packaged bridge executable.
- `BATSVIEW_PYTHON`: Python executable used in development (default: `python3`,
  with `python` as the Windows fallback).
- `BATSPLOT_SOURCE`: directory prepended to `PYTHONPATH`, normally the
  `batsplot` repository's `src` directory.
- `BATSVIEW_CACHE_DIR`: optional override for the persistent reader cache.

## End-user installation

GitHub Actions artifacts are native desktop packages. Users do not need a
terminal, Rust, or Python:

- macOS: open `BATSView.app` from Finder (normally move it to Applications);
- Windows: run the BATSView setup `.exe`, then launch it from the Start menu;
- Linux: mark the `.AppImage` executable once, then open it from the desktop or
  file manager.

The installed app includes the platform-specific Python bridge and `batsplot`.
The directory-form bridge avoids the extraction delay paid by one-file Python
bundles on every selective read. BATSView also registers `.plt` files so an
installed copy can open them directly.

The initial packages are unsigned. macOS Gatekeeper and Windows SmartScreen may
therefore ask the user to confirm the first launch. Production releases should
be code-signed (and notarized on macOS).

## Building a desktop package

Install the build requirements, then run the cross-platform packaging helper:

```bash
python -m pip install -r bridge/requirements-build.txt
cargo install cargo-packager --locked --version 0.11.8
python scripts/package.py
```

The default output is a macOS `.app`, Windows NSIS installer `.exe`, or Linux
`.AppImage`. Pass `--format dmg`, `--format wix`, or `--format deb` to request
one of the other supported native formats. Outputs are written under
`target/release/`.

## Bridge protocol

The bridge's control plane is JSON on stdout. Plot data uses `BPV1`, a compact
little-endian file:

1. four-byte ASCII magic `BPV1`;
2. unsigned 32-bit JSON header length;
3. UTF-8 JSON header;
4. interleaved point vertices (`x`, `y`, `value`) as `float32`;
5. triangle indices as `uint32`.

The header records counts, labels, value ranges, and bounds. New protocol
versions must use a new magic value so incompatible readers fail clearly.

## Linux packages

Building `eframe` on Debian/Ubuntu typically needs:

```bash
sudo apt-get install build-essential pkg-config libx11-dev libxkbcommon-dev \
  libwayland-dev libgl1-mesa-dev
```
