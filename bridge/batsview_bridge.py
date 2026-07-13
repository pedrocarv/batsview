#!/usr/bin/env python3
"""Small process boundary between BATSView and the batsplot library."""

from __future__ import annotations

import argparse
import json
import math
import os
from pathlib import Path
import struct
import sys
from typing import Any

import numpy as np

import batsplot as bp


PROTOCOL = 1
MAGIC = b"BPV1"


def _default_cache_dir() -> Path:
    override = os.environ.get("BATSVIEW_CACHE_DIR")
    if override:
        return Path(override).expanduser()
    if sys.platform == "win32":
        root = Path(os.environ.get("LOCALAPPDATA", Path.home() / "AppData" / "Local"))
        return root / "BATSView" / "cache"
    if sys.platform == "darwin":
        return Path.home() / "Library" / "Caches" / "BATSView"
    root = Path(os.environ.get("XDG_CACHE_HOME", Path.home() / ".cache"))
    return root / "batsview"


def _json(value: object) -> None:
    json.dump(value, sys.stdout, ensure_ascii=False, separators=(",", ":"))
    sys.stdout.write("\n")


def _record(path: Path) -> dict[str, Any]:
    result: dict[str, Any] = {
        "path": str(path.resolve()),
        "name": path.name,
        "size": path.stat().st_size,
        "section": None,
        "var_id": None,
        "time_step": None,
        "dump_index": None,
    }
    try:
        parsed = bp.parse_filename(path)
    except ValueError:
        return result
    result.update(
        section=parsed.section,
        var_id=parsed.var_id,
        time_step=parsed.time_step,
        dump_index=parsed.dump_index,
    )
    return result


def command_scan(arguments: argparse.Namespace) -> None:
    directory = Path(arguments.directory).expanduser()
    if not directory.is_dir():
        raise FileNotFoundError(f"Directory not found: {directory}")
    paths = directory.rglob("*.plt") if arguments.recursive else directory.glob("*.plt")
    records = [_record(path) for path in paths if path.is_file()]
    records.sort(
        key=lambda item: (
            item["section"] or "",
            item["var_id"] if item["var_id"] is not None else -1,
            item["time_step"] if item["time_step"] is not None else -1,
            item["dump_index"] if item["dump_index"] is not None else -1,
            item["name"],
        )
    )
    _json({"protocol": PROTOCOL, "directory": str(directory.resolve()), "files": records})


def _section(path: Path) -> str | None:
    try:
        return bp.parse_filename(path).section
    except ValueError:
        return None


def command_inspect(arguments: argparse.Namespace) -> None:
    path = Path(arguments.input).expanduser()
    info = bp.inspect(path)
    config = bp.Config.default()
    aliases = config.aliases()
    reverse = {source: canonical for canonical, source in aliases.items()}
    variables = [
        {
            "source": source,
            "canonical": reverse.get(source, source),
            "unit": info.units.get(source),
        }
        for source in info.variables
    ]
    _json(
        {
            "protocol": PROTOCOL,
            "path": str(path.resolve()),
            "title": info.title,
            "section": _section(path),
            "variables": variables,
            "zones": [
                {
                    "index": zone.index,
                    "name": zone.name,
                    "num_points": zone.num_points,
                    "num_elements": zone.num_elements,
                    "zone_type": zone.zone_type,
                    "value_locations": list(zone.value_locations),
                }
                for zone in info.zones
            ],
        }
    )


def _coordinate_names(section: str | None, zone: bp.Zone) -> tuple[str, str]:
    config = bp.Config.default()
    if section in config.sections:
        coordinates = config.section(section).coordinates
        if len(coordinates) == 2:
            return tuple(config.canonical_name(name) for name in coordinates)  # type: ignore[return-value]

    candidates = []
    for name in zone.variables:
        compact = name.lower().replace(" ", "")
        if compact.startswith(("x[", "y[", "z[")) or compact in {"x", "y", "z"}:
            candidates.append(name)
    if len(candidates) >= 2:
        return candidates[0], candidates[1]
    raise ValueError(
        "Could not determine two plot coordinates. Use a standard y=0/z=0 filename "
        "or configure the section in batsplot."
    )


def _triangles(zone: bp.Zone, point_count: int) -> np.ndarray:
    if zone.connectivity is not None:
        cells = np.asarray(zone.connectivity, dtype=np.uint32)
        if cells.ndim != 2:
            raise ValueError("Connectivity must be a two-dimensional array")
        if cells.shape[1] == 3:
            return np.ascontiguousarray(cells)
        if cells.shape[1] == 4:
            return np.ascontiguousarray(
                np.concatenate((cells[:, [0, 1, 2]], cells[:, [0, 2, 3]]), axis=0)
            )
        raise ValueError(f"2-D viewer does not support {cells.shape[1]}-node cells")

    dimensions = zone.dimensions
    if dimensions is None:
        raise ValueError("The selected zone has neither connectivity nor ordered dimensions")
    active = [size for size in dimensions if size > 1]
    if len(active) != 2 or math.prod(active) != point_count:
        raise ValueError(f"Expected a 2-D ordered zone, got dimensions={dimensions}")
    rows, columns = active
    indices = np.arange(rows * columns, dtype=np.uint32).reshape(rows, columns)
    a = indices[:-1, :-1].ravel()
    b = indices[:-1, 1:].ravel()
    c = indices[1:, 1:].ravel()
    d = indices[1:, :-1].ravel()
    return np.ascontiguousarray(np.stack((a, b, c, a, c, d), axis=1).reshape(-1, 3))


def _nodal_values(zone: bp.Zone, variable: str, triangles: np.ndarray, count: int) -> np.ndarray:
    values = np.asarray(zone[variable], dtype=np.float64).ravel()
    if zone.location(variable) == "nodal":
        if values.size != count:
            raise ValueError(f"Nodal variable contains {values.size} values for {count} points")
        return values

    cells = np.asarray(zone.connectivity) if zone.connectivity is not None else None
    if cells is None or values.size != cells.shape[0]:
        raise ValueError("Cell-centered ordered data is not supported yet")
    sums = np.zeros(count, dtype=np.float64)
    weights = np.zeros(count, dtype=np.uint32)
    for column in range(cells.shape[1]):
        np.add.at(sums, cells[:, column], values)
        np.add.at(weights, cells[:, column], 1)
    return np.divide(sums, weights, out=np.full(count, np.nan), where=weights > 0)


def command_export(arguments: argparse.Namespace) -> None:
    path = Path(arguments.input).expanduser()
    output = Path(arguments.output).expanduser()
    section = _section(path)
    data = bp.read(
        path,
        zone=arguments.zone,
        variables=[arguments.variable],
        include_connectivity=True,
        cache=arguments.cache,
        cache_dir=arguments.cache_dir,
    )
    zone = data.zone()
    variable = bp.Config.default().canonical_name(bp.Config.default().source_name(arguments.variable))
    if variable not in zone.variables:
        variable = arguments.variable
    x_name, y_name = _coordinate_names(section, zone)
    x = np.asarray(zone[x_name], dtype=np.float64).ravel()
    y = np.asarray(zone[y_name], dtype=np.float64).ravel()
    if x.size != y.size:
        raise ValueError("Coordinate arrays have different lengths")
    triangles = _triangles(zone, x.size)
    values = _nodal_values(zone, variable, triangles, x.size)

    finite = values[np.isfinite(values)]
    if finite.size == 0:
        raise ValueError(f"Variable {arguments.variable!r} contains no finite values")
    positive = finite[finite > 0]
    vertices = np.column_stack((x, y, values)).astype("<f4", copy=False)
    indices = triangles.astype("<u4", copy=False)
    header = {
        "protocol": PROTOCOL,
        "path": str(path.resolve()),
        "title": data.title,
        "section": section,
        "zone": zone.name,
        "variable": variable,
        "source_variable": zone.original_names.get(variable, arguments.variable),
        "unit": zone.units.get(variable),
        "x_label": x_name,
        "y_label": y_name,
        "point_count": int(vertices.shape[0]),
        "triangle_count": int(indices.shape[0]),
        "bounds": [float(np.nanmin(x)), float(np.nanmax(x)), float(np.nanmin(y)), float(np.nanmax(y))],
        "value_range": [float(finite.min()), float(finite.max())],
        "positive_range": [float(positive.min()), float(positive.max())] if positive.size else None,
    }
    encoded = json.dumps(header, ensure_ascii=False, separators=(",", ":")).encode("utf-8")
    output.parent.mkdir(parents=True, exist_ok=True)
    temporary = output.with_suffix(output.suffix + ".tmp")
    with temporary.open("wb") as stream:
        stream.write(MAGIC)
        stream.write(struct.pack("<I", len(encoded)))
        stream.write(encoded)
        stream.write(vertices.tobytes(order="C"))
        stream.write(indices.tobytes(order="C"))
    temporary.replace(output)
    _json({"protocol": PROTOCOL, "output": str(output.resolve()), **header})


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(prog="batsview-bridge")
    parser.add_argument("--version", action="version", version=f"batsview-bridge {PROTOCOL}")
    commands = parser.add_subparsers(dest="command", required=True)

    scan = commands.add_parser("scan")
    scan.add_argument("directory")
    scan.add_argument("--recursive", action="store_true")
    scan.set_defaults(handler=command_scan)

    inspect = commands.add_parser("inspect")
    inspect.add_argument("input")
    inspect.set_defaults(handler=command_inspect)

    export = commands.add_parser("export")
    export.add_argument("input")
    export.add_argument("variable")
    export.add_argument("output")
    export.add_argument("--zone", type=int, default=0)
    export.add_argument("--cache", action=argparse.BooleanOptionalAction, default=True)
    export.add_argument("--cache-dir", default=str(_default_cache_dir()))
    export.set_defaults(handler=command_export)
    return parser


def main() -> int:
    arguments = build_parser().parse_args()
    try:
        arguments.handler(arguments)
        return 0
    except Exception as error:
        _json({"protocol": PROTOCOL, "error": str(error), "type": type(error).__name__})
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
