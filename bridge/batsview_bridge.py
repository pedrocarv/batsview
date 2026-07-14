#!/usr/bin/env python3
"""Small process boundary between BATSView and the batsplot library."""

from __future__ import annotations

import argparse
import hashlib
import json
import math
import os
from pathlib import Path
import struct
import sys
from typing import Any

import numpy as np

import batsplot as bp


PROTOCOL = 2
MAGIC = b"BPV2"


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
    sys.stdout.flush()


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


def _inspect(path: Path) -> dict[str, Any]:
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
    return {
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


def command_inspect(arguments: argparse.Namespace) -> None:
    _json(_inspect(Path(arguments.input).expanduser()))


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


def _mesh_id(positions: np.ndarray, indices: np.ndarray) -> str:
    digest = hashlib.blake2b(digest_size=16)
    digest.update(struct.pack("<QQ", positions.shape[0], indices.shape[0]))
    digest.update(positions.tobytes(order="C"))
    digest.update(indices.tobytes(order="C"))
    return digest.hexdigest()


def _load_plot(
    *,
    input_path: str,
    variable_name: str,
    output_path: str,
    zone_index: int = 0,
    cache: bool = True,
    cache_dir: str | Path | None = None,
    reuse_mesh_id: str | None = None,
) -> dict[str, Any]:
    path = Path(input_path).expanduser()
    output = Path(output_path).expanduser()
    section = _section(path)
    data = bp.read(
        path,
        zone=zone_index,
        variables=[variable_name],
        include_connectivity=True,
        cache=cache,
        cache_dir=cache_dir or _default_cache_dir(),
    )
    zone = data.zone()
    config = bp.Config.default()
    variable = config.canonical_name(config.source_name(variable_name))
    if variable not in zone.variables:
        variable = variable_name
    x_name, y_name = _coordinate_names(section, zone)
    x = np.asarray(zone[x_name], dtype=np.float64).ravel()
    y = np.asarray(zone[y_name], dtype=np.float64).ravel()
    if x.size != y.size:
        raise ValueError("Coordinate arrays have different lengths")
    triangles = _triangles(zone, x.size)
    values = _nodal_values(zone, variable, triangles, x.size)

    finite = values[np.isfinite(values)]
    if finite.size == 0:
        raise ValueError(f"Variable {variable_name!r} contains no finite values")
    positive = finite[finite > 0]
    positions = np.column_stack((x, y)).astype("<f4", copy=False)
    scalar_values = values.astype("<f4", copy=False)
    indices = triangles.astype("<u4", copy=False)
    mesh_id = _mesh_id(positions, indices)
    mesh_included = reuse_mesh_id != mesh_id
    header = {
        "protocol": PROTOCOL,
        "path": str(path.resolve()),
        "title": data.title,
        "section": section,
        "zone": zone.name,
        "variable": variable,
        "source_variable": zone.original_names.get(variable, variable_name),
        "unit": zone.units.get(variable),
        "x_label": x_name,
        "y_label": y_name,
        "point_count": int(positions.shape[0]),
        "triangle_count": int(indices.shape[0]),
        "mesh_id": mesh_id,
        "mesh_included": mesh_included,
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
        if mesh_included:
            stream.write(positions.tobytes(order="C"))
        stream.write(scalar_values.tobytes(order="C"))
        if mesh_included:
            stream.write(indices.tobytes(order="C"))
    temporary.replace(output)
    return {"protocol": PROTOCOL, "output": str(output.resolve()), **header}


def command_export(arguments: argparse.Namespace) -> None:
    _json(
        _load_plot(
            input_path=arguments.input,
            variable_name=arguments.variable,
            output_path=arguments.output,
            zone_index=arguments.zone,
            cache=arguments.cache,
            cache_dir=arguments.cache_dir,
            reuse_mesh_id=arguments.reuse_mesh_id,
        )
    )


def command_serve(_arguments: argparse.Namespace) -> None:
    for line in sys.stdin:
        request_id: object = None
        try:
            request = json.loads(line)
            request_id = request.get("id")
            if request.get("protocol") != PROTOCOL:
                raise ValueError(f"Unsupported bridge protocol {request.get('protocol')!r}")
            method = request.get("method")
            parameters = request.get("params") or {}
            if method == "inspect":
                result = _inspect(Path(parameters["path"]).expanduser())
            elif method == "load":
                result = _load_plot(
                    input_path=parameters["path"],
                    variable_name=parameters["variable"],
                    output_path=parameters["output"],
                    zone_index=int(parameters.get("zone", 0)),
                    cache=bool(parameters.get("cache", True)),
                    cache_dir=parameters.get("cache_dir"),
                    reuse_mesh_id=parameters.get("reuse_mesh_id"),
                )
            elif method == "shutdown":
                _json({"protocol": PROTOCOL, "id": request_id, "ok": True, "result": {}})
                break
            else:
                raise ValueError(f"Unknown bridge method {method!r}")
            _json({"protocol": PROTOCOL, "id": request_id, "ok": True, "result": result})
        except Exception as error:
            _json(
                {
                    "protocol": PROTOCOL,
                    "id": request_id,
                    "ok": False,
                    "error": {"type": type(error).__name__, "message": str(error)},
                }
            )


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
    export.add_argument("--reuse-mesh-id")
    export.set_defaults(handler=command_export)

    serve = commands.add_parser("serve")
    serve.set_defaults(handler=command_serve)
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
