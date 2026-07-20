<p align="center">
  <img src="packaging/icons/batsview.png" alt="BATSView magnetosphere logo" width="190">
</p>

<h1 align="center">BATSView</h1>

<p align="center">
  A fast, focused desktop viewer for two- and three-dimensional BATS-R-US output.
</p>

<p align="center">
  <a href="https://github.com/pedrocarv/batsview/releases/latest"><img src="https://img.shields.io/github/v/release/pedrocarv/batsview?display_name=tag&sort=semver" alt="Latest release"></a>
  <a href="https://github.com/pedrocarv/batsview/actions/workflows/ci.yml"><img src="https://github.com/pedrocarv/batsview/actions/workflows/ci.yml/badge.svg" alt="CI status"></a>
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-MIT-blue.svg" alt="MIT license"></a>
</p>

BATSView combines run browsing, publication-ready plot styling, interactive 3D
exploration, field-line visualization, isosurfaces, scientific probes, and
timestep playback in one application for Windows, Linux, and macOS.

Packaged builds include everything required to read `.plt` files. End users do
not need to install Python, Rust, or use a terminal.

## Highlights

- Browse a complete BATS-R-US run and search files or variables by source and
  canonical name.
- Pan, zoom, fit, and set exact plot-axis limits on a GPU-accelerated canvas.
- Orient 2D cuts with default origin-centered planetary references: a gray
  2.5 Re inner boundary and a 1 Re black/white night/day Earth disk.
- Explore 3D Brick and Tetra zones through configurable orthogonal scalar
  slices with free rotation, pan, zoom, camera presets, and perspective or
  orthographic projection.
- Add up to eight optional 3D isosurfaces with exact values, solid or
  secondary-scalar coloring, opacity, cropping, and triangle budgets.
- Choose from nine scientific colormaps, reverse or discretize them, and use
  linear or logarithmic scaling.
- Configure automatic or exact colorbar ticks, labels, number formatting, and
  plot titles.
- Add editable lines, arrows, rectangles, circles, ellipses, polylines,
  polygons, and text in plot coordinates.
- Draw optional two- or three-dimensional field lines from arbitrary vector
  components, with one-click magnetic-field setup, planetary footpoints,
  custom seed regions, and direction arrows.
- Step through or play a timestep series with adjustable frame rate and
  looping.
- Save reusable scenes and export clean, plot-only PNG images with dark, white,
  or transparent backgrounds.
- Hover to inspect interpolated coordinates and values, or press `P` to save
  persistent measurement pins in 2D or 3D.

BATSView never modifies source `.plt` files.

## Download and install

Open the [latest BATSView release](https://github.com/pedrocarv/batsview/releases/latest)
and download the file for your computer. The application is self-contained:
Python, Rust, and terminal commands are not required.

| System | Download | Install and launch |
| --- | --- | --- |
| Windows 10/11, 64-bit | `BATSView-windows-x86_64-setup.exe` | Run the installer, then open **BATSView** from the Start menu. |
| Linux, 64-bit Intel/AMD | `BATSView-linux-x86_64.AppImage` | In the file properties, enable **Allow executing file as program**, then double-click it. |
| macOS, Apple silicon | `BATSView-macos-arm64.zip` | Extract the archive, drag **BATSView** to Applications, and open it. |
| macOS, Intel | `BATSView-macos-x86_64.zip` | Extract the archive, drag **BATSView** to Applications, and open it. |

The initial release is not code-signed. If the operating system asks for
confirmation, verify that the file came from the official release page, then:

- On Windows, select **More info** and **Run anyway** in SmartScreen.
- On macOS, control-click **BATSView**, choose **Open**, and confirm.

Every release also includes `SHA256SUMS.txt` so downloaded packages can be
checked against the hashes published with that release.

## Quick start

1. Open BATSView and select **Open run**.
2. Choose the directory containing the BATS-R-US `.plt` files. Enable
   **Include subfolders** when the run is organized into nested directories.
3. Select a file in the run explorer.
4. Choose a variable from the **Data** inspector. The plot updates while the
   selected mesh is reused whenever possible.
5. BATSView selects the 2D or 3D workspace automatically from the active zone.
   In 2D, drag empty plot space to pan. In 3D, left-drag to rotate and
   Shift+left-drag or right-drag to pan. Use the mouse wheel or trackpad pinch
   to zoom and press `F` to fit.

The transport strip beneath the plot becomes available when matching timesteps
are found for the selected section and variable.

## Workspace

### Data

Search and select variables, enter exact axis limits, fit or expand the view,
and configure the in-memory plot cache. The default cache limit is 512 MiB and
can be adjusted from 64 MiB to 8 GiB. **Clear cache** releases cached frames;
it does not delete source data.

The Data inspector also lists measurements pinned with the **Probe** tool.
Pins can be renamed, hidden, deleted, or cleared for the current plot. They are
stored in scene files and remain attached to their file, variable, and 3D
surface while the camera or axis limits change.

### Style

Control the scalar range, linear or logarithmic normalization, colormap,
reversal, and discrete color bins. Colorbar ticks can be generated
automatically or supplied as exact values with custom labels and fixed-decimal
or scientific formatting.

Plot titles may be fixed text or templates using metadata such as variable,
unit, section, timestep, filename, run, and dataset title.

For 2D plots, the default planetary overlays can also be shown or hidden here,
and both radii can be edited. The Earth disk is centered at the origin with the
sunward `+X` hemisphere white and the nightside black. A `-X` display option is
available for datasets or figures that intentionally use a reversed convention.

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
- By default, trace from invisible footpoints distributed along configurable
  planetary latitudes. You can also generate a grid inside an exact region or
  activate **Place on plot** and click the canvas to add individual 2D seeds.
  Seed locations are never drawn over the scientific figure.
- Configure integration direction, step size, maximum steps, line color and
  width, and direction arrows.
- Select **Hide** to remove the overlay while preserving its configuration.

Field-line settings are stored per section. A previously saved scene restores
field lines only when they were explicitly enabled in that scene.

### 3D scene

Three-dimensional Brick and Tetra files open automatically in an isometric 3D
workspace. A shaded planet / inner-boundary sphere is visible by default at
the origin with radius 2.5 Re. The **Shapes** inspector becomes a scene
inspector with independent X, Y, and Z slice controls, surface opacity,
domain-box, axes, planet radius, camera presets, and projection controls.
Slice positions and the camera are stored in scene files and in the
automatically restored per-run state.

The toolbar provides explicit **Zoom in**, **Zoom out**, **Fit all**, and
**Reset view** actions. The scene inspector also provides directional movement
buttons and exact XYZ camera-target coordinates. New or invalid legacy camera
states are fitted to the complete domain before the first frame is shown.

Open **Fields** to add optional 3D magnetic field lines or choose any three
nodal variables as a custom vector. The initial seeds are invisible planetary
footpoints distributed across selected latitudes and longitudes. Exact XYZ
seeds and regular grids inside a custom 3D region can be added independently.
Integration step, maximum length, maximum steps, color, width, and direction
arrows are configurable.

### Surfaces

Isosurfaces are optional and are never created automatically. In a 3D file,
select a scalar variable and choose **+ Isosurface** in the command bar or
**Add isosurface** in the Surfaces inspector. Enter an exact isovalue, then
choose **Apply**. The current scene remains visible while extraction runs.

Each section can contain up to eight ordered surfaces. A layer may use a solid
color or an independently styled secondary scalar, including its own colormap,
scale, limits, discrete bins, and colorbar ticks. Visibility, opacity, color,
and ordering update immediately; extraction-variable, isovalue, crop,
secondary-variable, and triangle-budget changes require **Apply**. Select the
slice group to show the normal plot colorbar, a scalar surface to show its
colorbar, or a solid surface to hide the colorbar.

The shared crop box uses normalized scene coordinates for portability but
shows actual domain coordinates in the inspector. It clips slices,
isosurfaces, field lines, probing, the domain box, and camera fitting. Mesh
budgets range from 100k to 2M triangles or Full; Auto targets 500k triangles.
Empty, out-of-range, or temporarily unmatched layers remain in the scene as
inactive cards with an explanatory message.

### Scientific probe

Hover either canvas for an interpolated coordinate and scalar readout. In 2D,
BATSView interpolates within the underlying triangle. In 3D, it selects the
nearest visible slice or isosurface under the cursor. Choose the crosshair
toolbar button or press `P`, then click to pin a measurement. Probe indexes are
built in the background and replaced safely when the mesh changes.

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
ahead. Neighboring frames are prefetched in the background. When field lines
are enabled, the previous completed overlay remains visible until the next one
is ready, then the two are exchanged without an empty intermediate frame.

## Scenes and image export

BATSView automatically remembers recent per-run scenes. Use **Save Scene** to
write a portable JSON scene file and **Load Scene** to apply one to the current
run. Scene paths are stored relatively so files can move between Windows and
Linux systems with the same run layout.

**Export PNG** writes only the scientific figure—not the surrounding
application panels—and includes the title, axes, scalar field, active colorbar,
annotations, field lines, and visible measurement pins. The active 3D camera,
depth-tested slices and isosurfaces, cropped domain box, planet, and field lines
are exported as shown. Available output sizes are 1x, 2x, and 4x the current
plot dimensions, with dark, publication-white, or transparent backgrounds.

## Keyboard and mouse controls

| Action | Control |
| --- | --- |
| Open a run | `Ctrl+O` / `Cmd+O` |
| Save a scene | `Ctrl+S` / `Cmd+S` |
| Export a PNG | `Ctrl+E` / `Cmd+E` |
| Undo / redo | `Ctrl+Z` / `Ctrl+Shift+Z` or `Cmd+Z` / `Cmd+Shift+Z` |
| Fit the plot | `F` or double-click the canvas |
| Pan in 2D | Drag empty plot space |
| Rotate in 3D | Left-drag the scene |
| Pan in 3D | Shift+left-drag, right-drag, or middle-drag the scene |
| Zoom | Mouse wheel, trackpad pinch, or 3D toolbar buttons |
| Play / pause | `Space` when a text field is not active |
| Toggle measurement pinning | `P` |
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

For 3D volumes, the bridge sends only enabled slices and extracted isosurface
meshes; the full multi-million-cell volume is not copied into the GUI. Surface
intersections are welded into indexed meshes and can be reduced to a requested
triangle budget. Camera movement and style-only changes are entirely GPU-side
and do not invoke the bridge. Geometry changes start a cancellable extraction
while the previous completed scene remains visible.

## Troubleshooting

### No files appear after opening a run

Confirm that the selected directory contains files ending in `.plt`. If the
files are below the selected directory, enable **Include subfolders**. Extension
matching is case-insensitive.

### Field lines are not visible

Open **Fields** and explicitly add a magnetic or custom vector field. Confirm
that two different nodal components are selected in 2D, or three different
components in 3D. Enable planetary footpoints or add individual / regional
seeds. Field lines remain off until an overlay is added.

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
input and output. Requests include protocol version 4, a numeric request ID, a
method (`inspect`, `load`, `load_surface3d`, `trace_fieldlines3d`, or
`shutdown`), and parameters. Responses echo the request ID and contain either a
result or a structured error. One-shot commands remain available for
diagnostics.

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

Three-dimensional layered data uses the little-endian `B3S2` format. It stores
separate float32 XYZ positions and scalar values, uint32 triangle indices,
full-volume and crop bounds, and tagged slice/isosurface ranges. Layer metadata
records stable IDs, variables, isovalues, units, full and rendered ranges,
triangle counts, and inactive errors. Matching complete geometry may reuse the
same mesh and transfer scalar values only.

Three-dimensional field lines use the additive `B3L1` format: a JSON header,
uint32 polyline offsets, and float32 XYZ points. The persistent bridge retains
the most recently used exact 3D grid locator so consecutive variables and
timesteps on the same mesh do not rebuild the multi-million-cell spatial index.

</details>

## License

BATSView is available under the [MIT License](LICENSE).
