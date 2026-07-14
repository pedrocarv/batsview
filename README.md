# BATSView

[![CI](https://github.com/pedrocarv/batsview/actions/workflows/ci.yml/badge.svg)](https://github.com/pedrocarv/batsview/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

BATSView is a fast, standalone desktop viewer for two-dimensional BATS-R-US
Tecplot output. It combines run browsing, scientific plot styling, annotations,
field-line visualization, and timestep playback in one focused application for
Windows, Linux, and macOS.

Packaged builds include everything required to read `.plt` files. End users do
not need to install Python, Rust, or use a terminal.

## Highlights

- Browse a complete BATS-R-US run and search files or variables by source and
  canonical name.
- Pan, zoom, fit, and set exact plot-axis limits on a GPU-accelerated canvas.
- Choose from nine scientific colormaps, reverse or discretize them, and use
  linear or logarithmic scaling.
- Configure automatic or exact colorbar ticks, labels, number formatting, and
  plot titles.
- Add editable lines, arrows, rectangles, circles, ellipses, polylines,
  polygons, and text in plot coordinates.
- Draw optional two-dimensional streamtraces from any pair of nodal variables,
  with a one-click magnetic-field setup, configurable seeds, and direction
  arrows.
- Step through or play a timestep series with adjustable frame rate and
  looping.
- Save reusable scenes and export clean, plot-only PNG images with dark, white,
  or transparent backgrounds.

BATSView never modifies source `.plt` files.

## Download and install

Download the build for your platform from the
[BATSView releases page](https://github.com/pedrocarv/batsview/releases).

| Platform | Package | Installation |
| --- | --- | --- |
| Windows 10/11, x86-64 | Setup `.exe` | Run the installer, then open BATSView from the Start menu. |
| Linux, x86-64 | `.AppImage` | In the file properties, allow the file to run as a program, then double-click it. |
| macOS, Apple silicon | `BATSView.app` in a `.zip` | Extract the archive and move BATSView to Applications. |

Current packages are unsigned. Windows SmartScreen or macOS Gatekeeper may ask
you to confirm the first launch. On macOS, control-click BATSView, choose
**Open**, and confirm when prompted.

## Quick start

1. Open BATSView and select **Open run**.
2. Choose the directory containing the BATS-R-US `.plt` files. Enable
   **Include subfolders** when the run is organized into nested directories.
3. Select a file in the run explorer.
4. Choose a variable from the **Data** inspector. The plot updates while the
   selected mesh is reused whenever possible.
5. Drag empty plot space to pan, use the mouse wheel to zoom, and double-click
   the plot or press `F` to fit the full domain.

The transport strip beneath the plot becomes available when matching timesteps
are found for the selected section and variable.

## Workspace

### Data

Search and select variables, enter exact axis limits, fit or expand the view,
and configure the in-memory plot cache. The default cache limit is 512 MiB and
can be adjusted from 64 MiB to 8 GiB. **Clear cache** releases cached frames;
it does not delete source data.

### Style

Control the scalar range, linear or logarithmic normalization, colormap,
reversal, and discrete color bins. Colorbar ticks can be generated
automatically or supplied as exact values with custom labels and fixed-decimal
or scientific formatting.

Plot titles may be fixed text or templates using metadata such as variable,
unit, section, timestep, filename, run, and dataset title.

### Shapes

Use the toolbar above the plot to draw scientific annotations. Figures remain
anchored in data coordinates while the view is panned or zoomed. Select a
figure to move or resize it directly, or enter exact coordinates and dimensions
in the inspector.

Annotations support stroke and fill styling, dash patterns, arrowheads,
visibility, locking, duplication, ordering, and run-, section-, variable-, or
plot-specific scope. Scene edits support up to 100 undo/redo actions.

### Fields

Field lines are optional and are never added to a new plot automatically.

- Select **Add magnetic field lines** to use the magnetic components aligned
  with the current plot axes.
- To visualize another vector field, select its horizontal and vertical
  components and choose **Add custom field lines**.
- Generate a uniform seed grid or activate **Place on plot** and click the
  canvas to add individual seeds.
- Configure integration direction, step size, maximum steps, line color and
  width, and direction arrows.
- Select **Hide** to remove the overlay while preserving its configuration.

Field-line settings are stored per section. A previously saved scene restores
field lines only when they were explicitly enabled in that scene.

### Info

Review source metadata, zone information, dimensions, variable aliases, units,
and other details reported by the BATS-R-US file reader.

## Timeline playback

BATSView groups files with the same section and variable identifier into an
ordered timeline. Use the controls below the canvas to move to the previous or
next frame, scrub to a timestep, play or pause, select a rate from 0.5 to 30
frames per second, and enable looping.

Playback visits frames in order. If the next frame is not ready, the current
frame remains visible and BATSView displays **Buffering** rather than skipping
ahead. Neighboring frames are prefetched in the background.

## Scenes and image export

BATSView automatically remembers recent per-run scenes. Use **Save Scene** to
write a portable JSON scene file and **Load Scene** to apply one to the current
run. Scene paths are stored relatively so files can move between Windows and
Linux systems with the same run layout.

**Export PNG** writes only the scientific figure—not the surrounding
application panels—and includes the title, axes, scalar field, colorbar,
annotations, and visible field lines. Available output sizes are 1x, 2x, and 4x
the current plot dimensions, with dark, publication-white, or transparent
backgrounds.

## Keyboard and mouse controls

| Action | Control |
| --- | --- |
| Open a run | `Ctrl+O` / `Cmd+O` |
| Save a scene | `Ctrl+S` / `Cmd+S` |
| Export a PNG | `Ctrl+E` / `Cmd+E` |
| Undo / redo | `Ctrl+Z` / `Ctrl+Shift+Z` or `Cmd+Z` / `Cmd+Shift+Z` |
| Fit the plot | `F` or double-click the canvas |
| Pan | Drag empty plot space |
| Zoom | Mouse wheel |
| Play / pause | `Space` when a text field is not active |
| Delete a selected annotation | `Delete` / `Backspace` |
| Cancel drawing or seed placement | `Escape` |
| Constrain a line, square, or circle | Hold `Shift` while drawing |
| Finish a polyline or polygon | `Enter` or double-click |

## Performance

BATSView is designed for large simulation outputs:

- one persistent reader process serves the entire application session;
- a byte-limited least-recently-used cache retains recently viewed frames;
- identical meshes and connectivity are shared across variables and timesteps;
- matching GPU meshes update only their scalar buffer;
- neighboring timeline frames load in the background; and
- superseded requests are cancelled so rapid selections cannot display stale
  results.

The first access to a large mesh may still take time. Subsequent variables and
timesteps on the same grid are normally much faster.

## Troubleshooting

### No files appear after opening a run

Confirm that the selected directory contains files ending in `.plt`. If the
files are below the selected directory, enable **Include subfolders**. Extension
matching is case-insensitive.

### Field lines are not visible

Open **Fields** and explicitly add a magnetic or custom vector field. Confirm
that two different nodal components are selected and that the seed list is not
empty. Field lines remain off until an overlay is added.

### Playback is buffering

Large frames may need additional time on first access. Allow the next frame to
finish loading, reduce the playback rate, or increase the memory-cache limit if
the computer has sufficient RAM.

### The operating system blocks the application

The current packages are unsigned. Use the platform-specific first-launch
confirmation described in [Download and install](#download-and-install). Only
install builds obtained from the official BATSView repository.

## For contributors

BATSView's interface and renderer are written in Rust using `egui` and `wgpu`.
A supervised Python bridge uses
[`batsplot`](https://github.com/pedrocarv/batsplot) as the source of truth for
TDV112 parsing and variable aliases. Packaged applications bundle this bridge;
the following dependencies are required only for development.

### Requirements

- Rust stable 1.85 or newer
- Python 3.10 or newer
- A local or installed copy of `batsplot`

### Run from source

```bash
# When batsplot is installed in the active Python environment
cargo run --release

# Or point BATSView at a batsplot checkout
BATSPLOT_SOURCE=/path/to/batsplot/src cargo run --release

# Open a run directory or one .plt file immediately
cargo run --release -- /path/to/run
```

Development environment variables:

- `BATSVIEW_BRIDGE`: packaged bridge executable or directory.
- `BATSVIEW_PYTHON`: Python executable used in development; defaults to
  `python3`, with `python` as the Windows fallback.
- `BATSPLOT_SOURCE`: directory prepended to `PYTHONPATH`, normally the
  `batsplot` checkout's `src` directory.
- `BATSVIEW_CACHE_DIR`: optional persistent reader-cache location.

### Test

```bash
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets
python -m pytest -q
```

CI runs the Rust suite on Windows, Linux, and macOS and runs the bridge tests on
Python 3.12.

### Build a desktop package

```bash
python -m pip install -r bridge/requirements-build.txt
cargo install cargo-packager --locked --version 0.11.8
python scripts/package.py
```

The default output is a Windows NSIS installer, Linux AppImage, or macOS
application bundle under `target/release`. Use `python scripts/package.py
--help` to list additional package formats.

Building on Debian or Ubuntu requires the native window-system packages:

```bash
sudo apt-get install build-essential pkg-config libx11-dev libxkbcommon-dev \
  libwayland-dev libgl1-mesa-dev
```

<details>
<summary>Bridge protocol</summary>

The persistent bridge control plane uses newline-delimited JSON over standard
input and output. Requests include protocol version 2, a numeric request ID, a
method (`inspect`, `load`, or `shutdown`), and parameters. Responses echo the
request ID and contain either a result or a structured error. One-shot commands
remain available for diagnostics.

Plot data uses the little-endian `BPV2` exchange format:

1. four-byte ASCII magic `BPV2`;
2. unsigned 32-bit JSON-header length;
3. UTF-8 JSON header;
4. optional float32 positions;
5. float32 scalar values; and
6. optional uint32 triangle indices.

The header records a stable mesh ID, mesh-presence flag, counts, labels, value
ranges, and bounds. When a request supplies a matching mesh ID, the bridge
writes only the scalar values.

</details>

## License

BATSView is available under the [MIT License](LICENSE).
