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


PROTOCOL = 4
MAGIC = b"BPV2"
MAGIC_3D = b"B3S2"
MAGIC_3D_LINES = b"B3L1"


def _spatial_dimension(zone_type: str) -> int:
    compact = zone_type.lower().replace("_", "").replace(" ", "")
    if any(name in compact for name in ("brick", "hexa", "tetra", "3d")):
        return 3
    return 2


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
                "spatial_dimension": _spatial_dimension(zone.zone_type),
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


def _coordinate_names_3d(section: str | None, zone: bp.Zone) -> tuple[str, str, str]:
    config = bp.Config.default()
    if section in config.sections:
        coordinates = config.section(section).coordinates
        if len(coordinates) == 3:
            return tuple(config.canonical_name(name) for name in coordinates)  # type: ignore[return-value]

    candidates: dict[str, str] = {}
    for name in zone.variables:
        compact = name.lower().replace(" ", "")
        for axis in "xyz":
            if compact == axis or compact.startswith(f"{axis}["):
                candidates.setdefault(axis, name)
    if len(candidates) == 3:
        return candidates["x"], candidates["y"], candidates["z"]
    raise ValueError(
        "Could not determine three plot coordinates. Use a standard 3d filename "
        "or configure the 3d section in batsplot."
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


_TETRA_EDGES = ((0, 1), (1, 2), (2, 0), (0, 3), (1, 3), (2, 3))
_TETRA_EDGE_INDEX = {tuple(sorted(edge)): index for index, edge in enumerate(_TETRA_EDGES)}


def _tetra_case_table() -> dict[int, tuple[tuple[int, int, int], ...]]:
    table: dict[int, tuple[tuple[int, int, int], ...]] = {}
    edge = lambda a, b: _TETRA_EDGE_INDEX[tuple(sorted((a, b)))]
    for case in range(1, 15):
        inside = [index for index in range(4) if case & (1 << index)]
        outside = [index for index in range(4) if not case & (1 << index)]
        if len(inside) == 1:
            a = inside[0]
            table[case] = ((edge(a, outside[0]), edge(a, outside[1]), edge(a, outside[2])),)
        elif len(inside) == 3:
            a = outside[0]
            table[case] = ((edge(a, inside[2]), edge(a, inside[1]), edge(a, inside[0])),)
        else:
            a, b = inside
            c, d = outside
            ac, ad, bc, bd = edge(a, c), edge(a, d), edge(b, c), edge(b, d)
            table[case] = ((ac, ad, bd), (ac, bd, bc))
    return table


_TETRA_CASES = _tetra_case_table()
_BRICK_TETRAHEDRA = np.asarray(
    (
        (0, 1, 2, 6),
        (0, 2, 3, 6),
        (0, 3, 7, 6),
        (0, 7, 4, 6),
        (0, 4, 5, 6),
        (0, 5, 1, 6),
    ),
    dtype=np.intp,
)


def _candidate_cells(
    connectivity: np.ndarray,
    coordinates: np.ndarray,
    axis: int,
    position: float,
) -> np.ndarray:
    candidates: list[np.ndarray] = []
    for start in range(0, connectivity.shape[0], 131_072):
        cells = connectivity[start : start + 131_072]
        axis_values = coordinates[cells, axis]
        low = np.nanmin(axis_values, axis=1)
        high = np.nanmax(axis_values, axis=1)
        tolerance = np.maximum(np.maximum(np.abs(low), np.abs(high)), 1.0) * 1.0e-7
        mask = (low - tolerance <= position) & (high + tolerance >= position) & (high > low)
        if np.any(mask):
            candidates.append(np.ascontiguousarray(cells[mask]))
    if not candidates:
        return np.empty((0, connectivity.shape[1]), dtype=np.intp)
    return np.concatenate(candidates, axis=0)


def _slice_tetrahedra(
    nodes: np.ndarray,
    coordinates: np.ndarray,
    scalar_values: np.ndarray,
    axis: int,
    position: float,
) -> tuple[np.ndarray, np.ndarray]:
    if nodes.size == 0:
        return np.empty((0, 3), dtype="<f4"), np.empty(0, dtype="<f4")

    vertex_coordinates = coordinates[nodes]
    axis_values = vertex_coordinates[:, :, axis]
    cases = np.sum((axis_values >= position).astype(np.uint8) << np.arange(4, dtype=np.uint8), axis=1)
    vertex_scalars = scalar_values[nodes]
    output_positions: list[np.ndarray] = []
    output_values: list[np.ndarray] = []

    for case, triangles in _TETRA_CASES.items():
        selected = np.flatnonzero(cases == case)
        if selected.size == 0:
            continue
        case_coordinates = vertex_coordinates[selected]
        case_axis = axis_values[selected]
        case_scalars = vertex_scalars[selected]
        edge_positions: list[np.ndarray] = []
        edge_values: list[np.ndarray] = []
        for a, b in _TETRA_EDGES:
            denominator = case_axis[:, b] - case_axis[:, a]
            t = np.divide(
                position - case_axis[:, a],
                denominator,
                out=np.zeros_like(denominator, dtype=np.float64),
                where=denominator != 0,
            )
            t = np.clip(t, 0.0, 1.0)
            edge_positions.append(
                case_coordinates[:, a] + t[:, None] * (case_coordinates[:, b] - case_coordinates[:, a])
            )
            edge_values.append(case_scalars[:, a] + t * (case_scalars[:, b] - case_scalars[:, a]))

        for triangle in triangles:
            points = np.stack([edge_positions[index] for index in triangle], axis=1)
            values = np.stack([edge_values[index] for index in triangle], axis=1)
            area_vector = np.cross(points[:, 1] - points[:, 0], points[:, 2] - points[:, 0])
            valid = np.isfinite(points).all(axis=(1, 2)) & (np.linalg.norm(area_vector, axis=1) > 1.0e-12)
            if np.any(valid):
                output_positions.append(points[valid].reshape(-1, 3).astype("<f4", copy=False))
                output_values.append(values[valid].reshape(-1).astype("<f4", copy=False))

    if not output_positions:
        return np.empty((0, 3), dtype="<f4"), np.empty(0, dtype="<f4")
    return np.concatenate(output_positions), np.concatenate(output_values)


def _extract_slice(
    connectivity: np.ndarray,
    coordinates: np.ndarray,
    scalar_values: np.ndarray,
    axis: int,
    position: float,
) -> tuple[np.ndarray, np.ndarray]:
    cells = _candidate_cells(connectivity, coordinates, axis, position)
    if cells.shape[1] == 4:
        tetrahedra = cells
    elif cells.shape[1] == 8:
        tetrahedra = cells[:, _BRICK_TETRAHEDRA].reshape(-1, 4)
    else:
        raise ValueError(f"3-D viewer does not support {cells.shape[1]}-node cells")
    return _slice_tetrahedra(tetrahedra, coordinates, scalar_values, axis, position)


def _candidate_isosurface_cells(
    connectivity: np.ndarray,
    coordinates: np.ndarray,
    scalar_values: np.ndarray,
    isovalue: float,
    crop_bounds: np.ndarray | None,
) -> np.ndarray:
    candidates: list[np.ndarray] = []
    for start in range(0, connectivity.shape[0], 131_072):
        cells = connectivity[start : start + 131_072]
        cell_values = scalar_values[cells]
        low = np.nanmin(cell_values, axis=1)
        high = np.nanmax(cell_values, axis=1)
        tolerance = np.maximum(np.maximum(np.abs(low), np.abs(high)), 1.0) * 1.0e-7
        mask = (
            np.isfinite(cell_values).all(axis=1)
            & (low - tolerance <= isovalue)
            & (high + tolerance >= isovalue)
            & (high > low)
        )
        if crop_bounds is not None and np.any(mask):
            points = coordinates[cells]
            cell_low = np.nanmin(points, axis=1)
            cell_high = np.nanmax(points, axis=1)
            mask &= np.all(
                (cell_high >= crop_bounds[:, 0]) & (cell_low <= crop_bounds[:, 1]), axis=1
            )
        if np.any(mask):
            candidates.append(np.ascontiguousarray(cells[mask]))
    if not candidates:
        return np.empty((0, connectivity.shape[1]), dtype=np.intp)
    return np.concatenate(candidates, axis=0)


def _contour_tetrahedra(
    nodes: np.ndarray,
    coordinates: np.ndarray,
    contour_values: np.ndarray,
    output_values: np.ndarray,
    level: float,
) -> tuple[np.ndarray, np.ndarray]:
    if nodes.size == 0:
        return np.empty((0, 3), dtype="<f4"), np.empty(0, dtype="<f4")
    vertex_coordinates = coordinates[nodes]
    vertex_contours = contour_values[nodes]
    vertex_outputs = output_values[nodes]
    cases = np.sum(
        (vertex_contours >= level).astype(np.uint8) << np.arange(4, dtype=np.uint8), axis=1
    )
    output_positions: list[np.ndarray] = []
    output_scalars: list[np.ndarray] = []
    for case, triangles in _TETRA_CASES.items():
        selected = np.flatnonzero(cases == case)
        if selected.size == 0:
            continue
        case_coordinates = vertex_coordinates[selected]
        case_contours = vertex_contours[selected]
        case_outputs = vertex_outputs[selected]
        edge_positions: list[np.ndarray] = []
        edge_outputs: list[np.ndarray] = []
        for a, b in _TETRA_EDGES:
            denominator = case_contours[:, b] - case_contours[:, a]
            t = np.divide(
                level - case_contours[:, a],
                denominator,
                out=np.zeros_like(denominator, dtype=np.float64),
                where=denominator != 0,
            )
            t = np.clip(t, 0.0, 1.0)
            edge_positions.append(
                case_coordinates[:, a]
                + t[:, None] * (case_coordinates[:, b] - case_coordinates[:, a])
            )
            edge_outputs.append(case_outputs[:, a] + t * (case_outputs[:, b] - case_outputs[:, a]))
        for triangle in triangles:
            points = np.stack([edge_positions[index] for index in triangle], axis=1)
            values = np.stack([edge_outputs[index] for index in triangle], axis=1)
            area = np.cross(points[:, 1] - points[:, 0], points[:, 2] - points[:, 0])
            valid = (
                np.isfinite(points).all(axis=(1, 2))
                & np.isfinite(values).all(axis=1)
                & (np.linalg.norm(area, axis=1) > 1.0e-12)
            )
            if np.any(valid):
                output_positions.append(points[valid].reshape(-1, 3).astype("<f4", copy=False))
                output_scalars.append(values[valid].reshape(-1).astype("<f4", copy=False))
    if not output_positions:
        return np.empty((0, 3), dtype="<f4"), np.empty(0, dtype="<f4")
    return np.concatenate(output_positions), np.concatenate(output_scalars)


def _extract_isosurface(
    connectivity: np.ndarray,
    coordinates: np.ndarray,
    contour_values: np.ndarray,
    output_values: np.ndarray,
    isovalue: float,
    crop_bounds: np.ndarray | None,
) -> tuple[np.ndarray, np.ndarray]:
    cells = _candidate_isosurface_cells(
        connectivity, coordinates, contour_values, isovalue, crop_bounds
    )
    if cells.shape[1] == 4:
        tetrahedra = cells
    elif cells.shape[1] == 8:
        tetrahedra = cells[:, _BRICK_TETRAHEDRA].reshape(-1, 4)
    else:
        raise ValueError(f"3-D viewer does not support {cells.shape[1]}-node cells")
    return _contour_tetrahedra(
        tetrahedra, coordinates, contour_values, output_values, isovalue
    )


def _clean_indexed_mesh(
    positions: np.ndarray, values: np.ndarray, indices: np.ndarray
) -> tuple[np.ndarray, np.ndarray, np.ndarray]:
    if indices.size == 0:
        return (
            np.empty((0, 3), dtype="<f4"),
            np.empty(0, dtype="<f4"),
            np.empty((0, 3), dtype="<u4"),
        )
    distinct = (
        (indices[:, 0] != indices[:, 1])
        & (indices[:, 1] != indices[:, 2])
        & (indices[:, 2] != indices[:, 0])
    )
    indices = indices[distinct]
    if indices.size == 0:
        return _clean_indexed_mesh(positions, values, np.empty((0, 3), dtype=np.uint32))
    points = positions[indices]
    area = np.linalg.norm(
        np.cross(points[:, 1] - points[:, 0], points[:, 2] - points[:, 0]), axis=1
    )
    indices = indices[np.isfinite(area) & (area > 1.0e-10)]
    if indices.size == 0:
        return _clean_indexed_mesh(positions, values, np.empty((0, 3), dtype=np.uint32))
    canonical = np.sort(indices, axis=1)
    _, unique = np.unique(canonical, axis=0, return_index=True)
    indices = indices[np.sort(unique)]
    used, inverse = np.unique(indices.ravel(), return_inverse=True)
    return (
        np.ascontiguousarray(positions[used], dtype="<f4"),
        np.ascontiguousarray(values[used], dtype="<f4"),
        np.ascontiguousarray(inverse.reshape(-1, 3), dtype="<u4"),
    )


def _weld_surface(
    positions: np.ndarray,
    values: np.ndarray,
    crop_bounds: np.ndarray | None,
) -> tuple[np.ndarray, np.ndarray, np.ndarray]:
    if positions.size == 0:
        return _clean_indexed_mesh(
            np.empty((0, 3), dtype="<f4"),
            np.empty(0, dtype="<f4"),
            np.empty((0, 3), dtype="<u4"),
        )
    positions = np.asarray(positions, dtype=np.float64)
    values = np.asarray(values, dtype=np.float64)
    if crop_bounds is not None:
        positions, values = _clip_triangles_to_box(positions, values, crop_bounds)
        if positions.size == 0:
            return _clean_indexed_mesh(
                np.empty((0, 3), dtype="<f4"),
                np.empty(0, dtype="<f4"),
                np.empty((0, 3), dtype="<u4"),
            )
    span = np.ptp(positions, axis=0)
    tolerance = max(float(np.nanmax(span)) * 1.0e-7, 1.0e-7)
    origin = np.nanmin(positions, axis=0)
    keys = np.rint((positions - origin) / tolerance).astype(np.int64)
    _, inverse = np.unique(keys, axis=0, return_inverse=True)
    count = int(inverse.max()) + 1
    weights = np.bincount(inverse, minlength=count).astype(np.float64)
    welded_positions = np.column_stack(
        [np.bincount(inverse, weights=positions[:, axis], minlength=count) for axis in range(3)]
    ) / weights[:, None]
    welded_values = np.bincount(inverse, weights=values, minlength=count) / weights
    indices = inverse.reshape(-1, 3).astype(np.uint32, copy=False)
    return _clean_indexed_mesh(welded_positions, welded_values, indices)


def _clip_triangles_to_box(
    positions: np.ndarray, values: np.ndarray, crop_bounds: np.ndarray
) -> tuple[np.ndarray, np.ndarray]:
    """Clip unindexed triangles exactly against an axis-aligned box."""
    output_positions: list[np.ndarray] = []
    output_values: list[float] = []
    for triangle_points, triangle_values in zip(
        positions.reshape(-1, 3, 3), values.reshape(-1, 3), strict=True
    ):
        polygon = [
            (np.asarray(point, dtype=np.float64), float(value))
            for point, value in zip(triangle_points, triangle_values, strict=True)
        ]
        for axis in range(3):
            for boundary, keep_above in (
                (float(crop_bounds[axis, 0]), True),
                (float(crop_bounds[axis, 1]), False),
            ):
                if not polygon:
                    break
                clipped: list[tuple[np.ndarray, float]] = []
                previous_point, previous_value = polygon[-1]
                previous_inside = (
                    previous_point[axis] >= boundary
                    if keep_above
                    else previous_point[axis] <= boundary
                )
                for current_point, current_value in polygon:
                    current_inside = (
                        current_point[axis] >= boundary
                        if keep_above
                        else current_point[axis] <= boundary
                    )
                    if current_inside != previous_inside:
                        denominator = current_point[axis] - previous_point[axis]
                        fraction = 0.0 if denominator == 0.0 else (
                            boundary - previous_point[axis]
                        ) / denominator
                        fraction = min(max(fraction, 0.0), 1.0)
                        intersection = previous_point + fraction * (
                            current_point - previous_point
                        )
                        intersection[axis] = boundary
                        interpolated = previous_value + fraction * (
                            current_value - previous_value
                        )
                        clipped.append((intersection, interpolated))
                    if current_inside:
                        clipped.append((current_point, current_value))
                    previous_point, previous_value = current_point, current_value
                    previous_inside = current_inside
                polygon = clipped
            if not polygon:
                break
        for index in range(1, len(polygon) - 1):
            for point, value in (polygon[0], polygon[index], polygon[index + 1]):
                output_positions.append(point)
                output_values.append(value)
    if not output_positions:
        return np.empty((0, 3), dtype=np.float64), np.empty(0, dtype=np.float64)
    return np.asarray(output_positions, dtype=np.float64), np.asarray(output_values, dtype=np.float64)


def _cluster_surface(
    positions: np.ndarray,
    values: np.ndarray,
    indices: np.ndarray,
    resolution: int,
) -> tuple[np.ndarray, np.ndarray, np.ndarray]:
    low = np.nanmin(positions, axis=0)
    span = np.maximum(np.nanmax(positions, axis=0) - low, 1.0e-20)
    keys = np.floor((positions - low) / span * max(resolution - 1, 1)).astype(np.int64)
    _, inverse = np.unique(keys, axis=0, return_inverse=True)
    count = int(inverse.max()) + 1
    weights = np.bincount(inverse, minlength=count).astype(np.float64)
    clustered_positions = np.column_stack(
        [np.bincount(inverse, weights=positions[:, axis], minlength=count) for axis in range(3)]
    ) / weights[:, None]
    clustered_values = np.bincount(inverse, weights=values, minlength=count) / weights
    clustered_indices = inverse[indices].astype(np.uint32, copy=False)
    return _clean_indexed_mesh(clustered_positions, clustered_values, clustered_indices)


def _reduce_surface(
    positions: np.ndarray,
    values: np.ndarray,
    indices: np.ndarray,
    triangle_limit: int | None,
) -> tuple[np.ndarray, np.ndarray, np.ndarray]:
    if triangle_limit is None or indices.shape[0] <= triangle_limit:
        return positions, values, indices
    resolution = max(4, int(math.sqrt(max(triangle_limit, 1) / 6.0)))
    reduced = (positions, values, indices)
    while resolution >= 2:
        candidate = _cluster_surface(positions, values, indices, resolution)
        reduced = candidate
        if candidate[2].shape[0] <= triangle_limit:
            return candidate
        resolution = max(2, int(resolution * 0.72))
        if resolution == 2:
            return _cluster_surface(positions, values, indices, resolution)
    return reduced


def _finite_range(values: np.ndarray) -> list[float] | None:
    finite = np.asarray(values)[np.isfinite(values)]
    if finite.size == 0:
        return None
    return [float(finite.min()), float(finite.max())]


def _load_surface3d(
    *,
    input_path: str,
    variable_name: str,
    output_path: str,
    planes: list[dict[str, Any]],
    isosurfaces: list[dict[str, Any]] | None = None,
    crop: dict[str, Any] | None = None,
    zone_index: int = 0,
    cache: bool = True,
    cache_dir: str | Path | None = None,
    reuse_mesh_id: str | None = None,
) -> dict[str, Any]:
    path = Path(input_path).expanduser()
    output = Path(output_path).expanduser()
    section = _section(path)
    isosurfaces = list(isosurfaces or [])
    if len(isosurfaces) > 8:
        raise ValueError("A scene may contain at most eight isosurfaces")
    config = bp.Config.default()
    available_sources: set[str] | None = None
    if isosurfaces:
        try:
            available_sources = set(bp.inspect(path).variables)
        except Exception:
            available_sources = None
    requested = [variable_name]
    for layer in isosurfaces:
        for requested_name in (layer.get("variable"), layer.get("color_variable")):
            if not requested_name:
                continue
            source = config.source_name(str(requested_name))
            if (available_sources is None or source in available_sources) and requested_name not in requested:
                requested.append(str(requested_name))
    data = bp.read(
        path,
        zone=zone_index,
        variables=requested,
        include_connectivity=True,
        cache=cache,
        cache_dir=cache_dir or _default_cache_dir(),
    )
    zone = data.zone()
    if zone.connectivity is None:
        raise ValueError("The 3-D viewer currently requires finite-element connectivity")
    connectivity = np.asarray(zone.connectivity, dtype=np.intp)
    if connectivity.ndim != 2:
        raise ValueError("Connectivity must be a two-dimensional array")

    variable = config.canonical_name(config.source_name(variable_name))
    if variable not in zone.variables:
        variable = variable_name
    coordinate_names = _coordinate_names_3d(section, zone)
    coordinate_arrays = [np.asarray(zone[name], dtype=np.float64).ravel() for name in coordinate_names]
    if len({values.size for values in coordinate_arrays}) != 1:
        raise ValueError("3-D coordinate arrays have different lengths")
    coordinates = np.ascontiguousarray(np.column_stack(coordinate_arrays), dtype=np.float64)
    values = _nodal_values(zone, variable, connectivity, coordinates.shape[0])

    bounds = np.asarray(
        [[np.nanmin(coordinates[:, axis]), np.nanmax(coordinates[:, axis])] for axis in range(3)],
        dtype=np.float64,
    )
    crop_bounds: np.ndarray | None = None
    if crop and bool(crop.get("enabled", False)):
        fractions = np.asarray(crop.get("fractions"), dtype=np.float64).reshape(3, 2)
        if (
            not np.isfinite(fractions).all()
            or np.any(fractions[:, 0] < 0.0)
            or np.any(fractions[:, 1] > 1.0)
            or np.any(fractions[:, 0] >= fractions[:, 1])
        ):
            raise ValueError("Crop fractions must be ordered values between 0 and 1")
        crop_bounds = bounds[:, :1] + fractions * (bounds[:, 1:] - bounds[:, :1])

    def resolve(name: str | None) -> str | None:
        if not name:
            return None
        canonical = config.canonical_name(config.source_name(str(name)))
        if canonical in zone.variables:
            return canonical
        return str(name) if str(name) in zone.variables else None

    positions_parts: list[np.ndarray] = []
    values_parts: list[np.ndarray] = []
    indices_parts: list[np.ndarray] = []
    layers: list[dict[str, Any]] = []
    index_start = 0

    def append_layer(
        metadata: dict[str, Any],
        layer_positions: np.ndarray,
        layer_values: np.ndarray,
        layer_indices: np.ndarray,
        source_triangles: int,
    ) -> None:
        nonlocal index_start
        offset = sum(part.shape[0] for part in positions_parts)
        if layer_positions.size:
            positions_parts.append(np.ascontiguousarray(layer_positions, dtype="<f4"))
            values_parts.append(np.ascontiguousarray(layer_values, dtype="<f4"))
            indices_parts.append(np.ascontiguousarray(layer_indices + offset, dtype="<u4"))
        index_count = int(layer_indices.size)
        metadata.update(
            {
                "index_start": index_start,
                "index_count": index_count,
                "source_triangles": int(source_triangles),
                "rendered_triangles": int(layer_indices.shape[0]),
            }
        )
        layers.append(metadata)
        index_start += index_count

    for plane in planes:
        if not bool(plane.get("enabled", True)):
            continue
        axis_name = str(plane["axis"]).lower()
        if axis_name not in {"x", "y", "z"}:
            raise ValueError(f"Unknown slice axis {axis_name!r}")
        axis = "xyz".index(axis_name)
        position = float(plane["position"])
        if bool(plane.get("origin_if_available", False)) and bounds[axis, 0] <= 0.0 <= bounds[axis, 1]:
            position = 0.0
        elif bool(plane.get("normalized", False)):
            if not 0.0 <= position <= 1.0:
                raise ValueError(f"Normalized {axis_name.upper()} slice must be between 0 and 1")
            position = float(bounds[axis, 0] + position * (bounds[axis, 1] - bounds[axis, 0]))
        if not math.isfinite(position) or position < bounds[axis, 0] or position > bounds[axis, 1]:
            raise ValueError(
                f"{axis_name.upper()} slice {position:g} is outside "
                f"[{bounds[axis, 0]:g}, {bounds[axis, 1]:g}]"
            )
        slice_positions, slice_values = _extract_slice(
            connectivity, coordinates, values, axis, position
        )
        source_triangles = int(slice_positions.shape[0] // 3)
        layer_positions, layer_values, layer_indices = _weld_surface(
            slice_positions, slice_values, crop_bounds
        )
        metadata = {
            "kind": "slice",
            "name": f"{axis_name.upper()} slice",
            "axis": axis_name,
            "position": position,
            "variable": variable,
            "unit": zone.units.get(variable) or "",
            "value_range": _finite_range(layer_values),
            "volume_range": _finite_range(values),
            "inactive_reason": None if layer_indices.size else "Slice does not intersect the crop box",
        }
        append_layer(metadata, layer_positions, layer_values, layer_indices, source_triangles)

    for request in isosurfaces:
        layer_id = int(request.get("id", 0))
        requested_variable = str(request.get("variable") or "")
        isovalue = float(request.get("isovalue", math.nan))
        metadata: dict[str, Any] = {
            "kind": "isosurface",
            "layer_id": layer_id,
            "name": str(request.get("name") or f"Isosurface {layer_id}"),
            "variable": requested_variable,
            "color_variable": request.get("color_variable"),
            "isovalue": isovalue,
            "unit": "",
            "value_range": None,
            "volume_range": None,
            "inactive_reason": None,
        }
        contour_name = resolve(requested_variable)
        requested_color = request.get("color_variable")
        color_name = resolve(requested_color) if requested_color else contour_name
        if not math.isfinite(isovalue):
            metadata["inactive_reason"] = "Isovalue must be finite"
            append_layer(metadata, np.empty((0, 3)), np.empty(0), np.empty((0, 3), dtype=np.uint32), 0)
            continue
        if contour_name is None:
            metadata["inactive_reason"] = f"Variable {requested_variable!r} is not available"
            append_layer(metadata, np.empty((0, 3)), np.empty(0), np.empty((0, 3), dtype=np.uint32), 0)
            continue
        if color_name is None:
            metadata["inactive_reason"] = f"Color variable {request.get('color_variable')!r} is not available"
            append_layer(metadata, np.empty((0, 3)), np.empty(0), np.empty((0, 3), dtype=np.uint32), 0)
            continue
        contour_values = _nodal_values(zone, contour_name, connectivity, coordinates.shape[0])
        color_values = _nodal_values(zone, color_name, connectivity, coordinates.shape[0])
        volume_range = _finite_range(contour_values)
        metadata["variable"] = contour_name
        metadata["color_variable"] = color_name if request.get("color_variable") else None
        metadata["unit"] = zone.units.get(color_name) or ""
        metadata["volume_range"] = volume_range
        if volume_range is None or not volume_range[0] <= isovalue <= volume_range[1]:
            metadata["inactive_reason"] = "Isovalue is outside the finite volume range"
            append_layer(metadata, np.empty((0, 3)), np.empty(0), np.empty((0, 3), dtype=np.uint32), 0)
            continue
        raw_positions, raw_values = _extract_isosurface(
            connectivity,
            coordinates,
            contour_values,
            color_values,
            isovalue,
            crop_bounds,
        )
        source_triangles = int(raw_positions.shape[0] // 3)
        layer_positions, layer_values, layer_indices = _weld_surface(
            raw_positions, raw_values, crop_bounds
        )
        budget = request.get("triangle_limit")
        triangle_limit = None if budget is None else max(100_000, min(int(budget), 2_000_000))
        layer_positions, layer_values, layer_indices = _reduce_surface(
            layer_positions, layer_values, layer_indices, triangle_limit
        )
        metadata["value_range"] = _finite_range(layer_values)
        if layer_indices.size == 0:
            metadata["inactive_reason"] = "No surface intersects this value and crop box"
        append_layer(metadata, layer_positions, layer_values, layer_indices, source_triangles)

    if not layers:
        raise ValueError("No slice or isosurface layer was requested")
    positions = (
        np.ascontiguousarray(np.concatenate(positions_parts), dtype="<f4")
        if positions_parts
        else np.empty((0, 3), dtype="<f4")
    )
    scalar_values = (
        np.ascontiguousarray(np.concatenate(values_parts), dtype="<f4")
        if values_parts
        else np.empty(0, dtype="<f4")
    )
    indices = (
        np.ascontiguousarray(np.concatenate(indices_parts), dtype="<u4")
        if indices_parts
        else np.empty((0, 3), dtype="<u4")
    )
    finite = scalar_values[np.isfinite(scalar_values)]
    mesh_id = _mesh_id(positions, indices)
    mesh_included = reuse_mesh_id != mesh_id
    try:
        parsed = bp.parse_filename(path)
        time_value = float(parsed.time_step) if parsed.time_step is not None else None
        dump_value = int(parsed.dump_index) if parsed.dump_index is not None else None
    except ValueError:
        time_value = None
        dump_value = None
    header = {
        "protocol": PROTOCOL,
        "source": str(path.resolve()),
        "title": data.title,
        "dataset_title": data.title,
        "section": section or "3d",
        "zone_name": zone.name,
        "variable": zone.original_names.get(variable, variable_name),
        "canonical_name": variable,
        "unit": zone.units.get(variable) or "",
        "axis_labels": list(coordinate_names),
        "vertex_count": int(positions.shape[0]),
        "triangle_count": int(indices.shape[0]),
        "mesh_id": mesh_id,
        "mesh_included": mesh_included,
        "bounds": [
            float(bounds[0, 0]), float(bounds[0, 1]),
            float(bounds[1, 0]), float(bounds[1, 1]),
            float(bounds[2, 0]), float(bounds[2, 1]),
        ],
        "crop_bounds": None if crop_bounds is None else [float(value) for value in crop_bounds.ravel()],
        "value_range": (
            [float(finite.min()), float(finite.max())]
            if finite.size
            else (_finite_range(values) or [0.0, 1.0])
        ),
        "volume_value_range": _finite_range(values),
        "layers": layers,
        "time": time_value,
        "dump": dump_value,
    }
    encoded = json.dumps(header, ensure_ascii=False, separators=(",", ":")).encode("utf-8")
    output.parent.mkdir(parents=True, exist_ok=True)
    temporary = output.with_suffix(output.suffix + ".tmp")
    with temporary.open("wb") as stream:
        stream.write(MAGIC_3D)
        stream.write(struct.pack("<I", len(encoded)))
        stream.write(encoded)
        if mesh_included:
            stream.write(positions.tobytes(order="C"))
        stream.write(scalar_values.tobytes(order="C"))
        if mesh_included:
            stream.write(indices.tobytes(order="C"))
    temporary.replace(output)
    return {"protocol": PROTOCOL, "output": str(output.resolve()), **header}


class _CellLocator3D:
    """Compact multilevel locator for axis-aligned AMR Brick cells."""

    def __init__(self, connectivity: np.ndarray, coordinates: np.ndarray) -> None:
        if connectivity.ndim != 2 or connectivity.shape[1] not in {4, 8}:
            raise ValueError("3-D field tracing supports 4-node Tetra and 8-node Brick cells")
        self.connectivity = np.ascontiguousarray(connectivity, dtype=np.uint32)
        self.coordinates = np.asarray(coordinates, dtype=np.float32)
        self.is_brick = connectivity.shape[1] == 8
        self.minimum = np.nanmin(self.coordinates, axis=0).astype(np.float64)
        self.maximum = np.nanmax(self.coordinates, axis=0).astype(np.float64)
        parts: dict[float, list[tuple[np.ndarray, np.ndarray]]] = {}
        dimensions: dict[float, np.ndarray] = {}

        for start in range(0, self.connectivity.shape[0], 131_072):
            cells = self.connectivity[start : start + 131_072]
            if self.is_brick:
                first = self.coordinates[cells[:, 0]].astype(np.float64)
                opposite = self.coordinates[cells[:, 6]].astype(np.float64)
                low = np.minimum(first, opposite)
                high = np.maximum(first, opposite)
            else:
                points = self.coordinates[cells].astype(np.float64)
                low = np.nanmin(points, axis=1)
                high = np.nanmax(points, axis=1)
            widths = np.nanmax(high - low, axis=1)
            rounded = np.round(widths, decimals=6)
            for level in np.unique(rounded):
                if not np.isfinite(level) or level <= 0:
                    continue
                mask = rounded == level
                cell_indices = np.flatnonzero(mask).astype(np.uint32) + np.uint32(start)
                centers = 0.5 * (low[mask] + high[mask])
                size = float(level)
                dims = dimensions.setdefault(
                    size,
                    np.ceil((self.maximum - self.minimum) / size).astype(np.int64) + 3,
                )
                bins = np.floor((centers - self.minimum) / size + 1.0e-7).astype(np.int64)
                keys = ((bins[:, 0] * dims[1] + bins[:, 1]) * dims[2] + bins[:, 2]).astype(
                    np.int64
                )
                parts.setdefault(size, []).append((keys, cell_indices))

        self.levels: list[tuple[float, np.ndarray, np.ndarray, np.ndarray]] = []
        for size in sorted(parts):
            keys = np.concatenate([part[0] for part in parts[size]])
            indices = np.concatenate([part[1] for part in parts[size]])
            order = np.argsort(keys, kind="stable")
            self.levels.append((size, dimensions[size], keys[order], indices[order]))
        if not self.levels:
            raise ValueError("Could not construct a spatial index for the 3-D mesh")

    def contains(self, point: np.ndarray) -> bool:
        return bool(np.all(point >= self.minimum) and np.all(point <= self.maximum))

    @staticmethod
    def _key(cell: np.ndarray, dims: np.ndarray) -> int:
        return int((cell[0] * dims[1] + cell[1]) * dims[2] + cell[2])

    def _candidate_indices(
        self, point: np.ndarray, size: float, dims: np.ndarray, keys: np.ndarray, indices: np.ndarray
    ):
        base = np.floor((point - self.minimum) / size + 1.0e-7).astype(np.int64)
        search_cells = [base]
        for dx in (-1, 0, 1):
            for dy in (-1, 0, 1):
                for dz in (-1, 0, 1):
                    if dx == dy == dz == 0:
                        continue
                    search_cells.append(base + np.array([dx, dy, dz], dtype=np.int64))
        for cell in search_cells:
            if np.any(cell < 0) or np.any(cell >= dims):
                continue
            key = self._key(cell, dims)
            left = int(np.searchsorted(keys, key, side="left"))
            right = int(np.searchsorted(keys, key, side="right"))
            yield from indices[left:right]

    def sample(self, point: np.ndarray, vectors: np.ndarray) -> np.ndarray | None:
        point = np.asarray(point, dtype=np.float64)
        if point.shape != (3,) or not np.isfinite(point).all() or not self.contains(point):
            return None
        for size, dims, keys, indices in self.levels:
            for cell_index in self._candidate_indices(point, size, dims, keys, indices):
                nodes = self.connectivity[int(cell_index)]
                vertices = self.coordinates[nodes].astype(np.float64)
                low = np.min(vertices, axis=0)
                high = np.max(vertices, axis=0)
                tolerance = max(size * 1.0e-6, 1.0e-8)
                if np.any(point < low - tolerance) or np.any(point > high + tolerance):
                    continue
                if self.is_brick:
                    span = high - low
                    if np.any(span <= 0):
                        continue
                    u, v, w = np.clip((point - low) / span, 0.0, 1.0)
                    weights = np.asarray(
                        [
                            (1 - u) * (1 - v) * (1 - w),
                            u * (1 - v) * (1 - w),
                            u * v * (1 - w),
                            (1 - u) * v * (1 - w),
                            (1 - u) * (1 - v) * w,
                            u * (1 - v) * w,
                            u * v * w,
                            (1 - u) * v * w,
                        ],
                        dtype=np.float64,
                    )
                else:
                    matrix = np.column_stack(
                        (vertices[1] - vertices[0], vertices[2] - vertices[0], vertices[3] - vertices[0])
                    )
                    try:
                        tail = np.linalg.solve(matrix, point - vertices[0])
                    except np.linalg.LinAlgError:
                        continue
                    weights = np.asarray([1.0 - tail.sum(), *tail], dtype=np.float64)
                    if np.any(weights < -1.0e-6) or np.any(weights > 1.0 + 1.0e-6):
                        continue
                sampled = weights @ vectors[nodes]
                if np.isfinite(sampled).all():
                    return sampled
        return None


_FIELDLINE_LOCATOR_CACHE: tuple[str, _CellLocator3D] | None = None


def _fieldline_locator(
    connectivity: np.ndarray, coordinates: np.ndarray
) -> tuple[str, _CellLocator3D]:
    """Reuse the last exact 3-D grid in persistent bridge sessions."""
    global _FIELDLINE_LOCATOR_CACHE
    canonical_connectivity = np.ascontiguousarray(connectivity, dtype="<u4")
    canonical_coordinates = np.ascontiguousarray(coordinates, dtype="<f4")
    mesh_id = _mesh_id(canonical_coordinates, canonical_connectivity)
    if _FIELDLINE_LOCATOR_CACHE is not None and _FIELDLINE_LOCATOR_CACHE[0] == mesh_id:
        return _FIELDLINE_LOCATOR_CACHE
    locator = _CellLocator3D(canonical_connectivity, canonical_coordinates)
    _FIELDLINE_LOCATOR_CACHE = (mesh_id, locator)
    return mesh_id, locator


def _trace_direction_3d(
    locator: _CellLocator3D,
    vectors: np.ndarray,
    seed: np.ndarray,
    sign: float,
    step: float,
    max_steps: int,
    max_length: float,
    planet_radius: float,
) -> np.ndarray:
    def direction(point: np.ndarray) -> np.ndarray | None:
        sampled = locator.sample(point, vectors)
        if sampled is None:
            return None
        magnitude = float(np.linalg.norm(sampled))
        if not math.isfinite(magnitude) or magnitude <= 1.0e-30:
            return None
        return sampled * (sign / magnitude)

    if direction(seed) is None:
        return np.empty((0, 3), dtype=np.float32)
    points = [np.asarray(seed, dtype=np.float64)]
    current = points[0]
    length = 0.0
    for index in range(max_steps):
        k1 = direction(current)
        if k1 is None:
            break
        k2 = direction(current + 0.5 * step * k1)
        if k2 is None:
            break
        k3 = direction(current + 0.5 * step * k2)
        if k3 is None:
            break
        k4 = direction(current + step * k3)
        if k4 is None:
            break
        next_point = current + step * (k1 + 2.0 * k2 + 2.0 * k3 + k4) / 6.0
        distance = float(np.linalg.norm(next_point - current))
        if not math.isfinite(distance) or distance <= step * 1.0e-5:
            break
        length += distance
        points.append(next_point)
        current = next_point
        if length >= max_length:
            break
        if index >= 2 and float(np.linalg.norm(current)) <= planet_radius:
            break
        if index >= 24 and float(np.linalg.norm(current - seed)) <= step * 0.7:
            break
    return np.asarray(points, dtype=np.float32)


def _trace_fieldlines3d(
    *,
    input_path: str,
    components: list[str],
    seeds: list[list[float]],
    output_path: str,
    zone_index: int = 0,
    step: float = 0.1,
    max_steps: int = 4000,
    max_length: float = 500.0,
    planet_radius: float = 2.5,
    crop: dict[str, Any] | None = None,
    cache: bool = True,
    cache_dir: str | Path | None = None,
) -> dict[str, Any]:
    if len(components) != 3 or len(set(components)) != 3:
        raise ValueError("3-D field tracing requires three different vector components")
    if not seeds:
        raise ValueError("3-D field tracing requires at least one seed")
    if not math.isfinite(step) or step <= 0:
        raise ValueError("Field-line step must be positive")
    if max_steps < 10 or max_steps > 20_000:
        raise ValueError("Field-line max_steps must be between 10 and 20000")
    if not math.isfinite(max_length) or max_length <= 0:
        raise ValueError("Field-line max_length must be positive")
    if not math.isfinite(planet_radius) or planet_radius <= 0:
        raise ValueError("Planet radius must be positive")

    path = Path(input_path).expanduser()
    output = Path(output_path).expanduser()
    data = bp.read(
        path,
        zone=zone_index,
        variables=components,
        include_connectivity=True,
        cache=cache,
        cache_dir=cache_dir or _default_cache_dir(),
    )
    zone = data.zone()
    if zone.connectivity is None:
        raise ValueError("3-D field tracing requires finite-element connectivity")
    coordinate_names = _coordinate_names_3d(_section(path), zone)
    coordinates = np.ascontiguousarray(
        np.column_stack([np.asarray(zone[name], dtype=np.float32).ravel() for name in coordinate_names])
    )
    connectivity = np.ascontiguousarray(zone.connectivity, dtype=np.uint32)
    config = bp.Config.default()
    canonical_components: list[str] = []
    component_values: list[np.ndarray] = []
    for requested in components:
        canonical = config.canonical_name(config.source_name(requested))
        if canonical not in zone.variables:
            canonical = requested
        canonical_components.append(canonical)
        component_values.append(
            _nodal_values(zone, canonical, connectivity, coordinates.shape[0]).astype(np.float32)
        )
    vectors = np.ascontiguousarray(np.column_stack(component_values), dtype=np.float32)
    mesh_id, locator = _fieldline_locator(connectivity, coordinates)
    crop_bounds: np.ndarray | None = None
    if crop and bool(crop.get("enabled", False)):
        fractions = np.asarray(crop.get("fractions"), dtype=np.float64).reshape(3, 2)
        if (
            not np.isfinite(fractions).all()
            or np.any(fractions[:, 0] < 0.0)
            or np.any(fractions[:, 1] > 1.0)
            or np.any(fractions[:, 0] >= fractions[:, 1])
        ):
            raise ValueError("Crop fractions must be ordered values between 0 and 1")
        crop_bounds = locator.minimum[:, None] + fractions * (
            locator.maximum - locator.minimum
        )[:, None]
    seed_array = np.asarray(seeds, dtype=np.float64)
    if seed_array.ndim != 2 or seed_array.shape[1] != 3 or not np.isfinite(seed_array).all():
        raise ValueError("Every 3-D seed must contain three finite coordinates")

    lines: list[np.ndarray] = []
    for seed in seed_array:
        backward = _trace_direction_3d(
            locator, vectors, seed, -1.0, step, max_steps, max_length, planet_radius
        )
        forward = _trace_direction_3d(
            locator, vectors, seed, 1.0, step, max_steps, max_length, planet_radius
        )
        if backward.shape[0] >= 2:
            backward = backward[::-1]
        if backward.shape[0] >= 2 and forward.shape[0] >= 2:
            line = np.concatenate((backward, forward[1:]), axis=0)
        elif backward.shape[0] >= 2:
            line = backward
        else:
            line = forward
        if line.shape[0] >= 2 and crop_bounds is not None:
            lines.extend(_clip_polyline_to_box(line, crop_bounds))
        elif line.shape[0] >= 2:
            lines.append(np.ascontiguousarray(line, dtype="<f4"))

    if not lines:
        raise ValueError("No 3-D field lines could be traced from the requested seeds")
    offsets = np.zeros(len(lines) + 1, dtype="<u4")
    offsets[1:] = np.cumsum([line.shape[0] for line in lines], dtype=np.uint32)
    positions = np.ascontiguousarray(np.concatenate(lines, axis=0), dtype="<f4")
    header = {
        "protocol": PROTOCOL,
        "source": str(path.resolve()),
        "section": _section(path) or "3d",
        "zone_name": zone.name,
        "components": canonical_components,
        "mesh_id": mesh_id,
        "line_count": len(lines),
        "point_count": int(positions.shape[0]),
        "seed_count": int(seed_array.shape[0]),
        "bounds": [
            float((crop_bounds[:, 0] if crop_bounds is not None else locator.minimum)[0]),
            float((crop_bounds[:, 1] if crop_bounds is not None else locator.maximum)[0]),
            float((crop_bounds[:, 0] if crop_bounds is not None else locator.minimum)[1]),
            float((crop_bounds[:, 1] if crop_bounds is not None else locator.maximum)[1]),
            float((crop_bounds[:, 0] if crop_bounds is not None else locator.minimum)[2]),
            float((crop_bounds[:, 1] if crop_bounds is not None else locator.maximum)[2]),
        ],
        "planet_radius": planet_radius,
    }
    encoded = json.dumps(header, ensure_ascii=False, separators=(",", ":")).encode("utf-8")
    output.parent.mkdir(parents=True, exist_ok=True)
    temporary = output.with_suffix(output.suffix + ".tmp")
    with temporary.open("wb") as stream:
        stream.write(MAGIC_3D_LINES)
        stream.write(struct.pack("<I", len(encoded)))
        stream.write(encoded)
        stream.write(offsets.tobytes(order="C"))
        stream.write(positions.tobytes(order="C"))
    temporary.replace(output)
    return {"protocol": PROTOCOL, "output": str(output.resolve()), **header}


def _clip_polyline_to_box(line: np.ndarray, bounds: np.ndarray) -> list[np.ndarray]:
    """Clip a polyline into one or more exact in-box segments."""
    segments: list[list[np.ndarray]] = []
    current: list[np.ndarray] = []
    for first, second in zip(line[:-1], line[1:], strict=True):
        first = np.asarray(first, dtype=np.float64)
        second = np.asarray(second, dtype=np.float64)
        delta = second - first
        low = 0.0
        high = 1.0
        valid = True
        for axis in range(3):
            if abs(float(delta[axis])) <= 1.0e-20:
                if first[axis] < bounds[axis, 0] or first[axis] > bounds[axis, 1]:
                    valid = False
                    break
                continue
            a = float((bounds[axis, 0] - first[axis]) / delta[axis])
            b = float((bounds[axis, 1] - first[axis]) / delta[axis])
            if a > b:
                a, b = b, a
            low = max(low, a)
            high = min(high, b)
            if high < low:
                valid = False
                break
        if not valid:
            if len(current) >= 2:
                segments.append(current)
            current = []
            continue
        clipped_first = first + low * delta
        clipped_second = first + high * delta
        if current and np.linalg.norm(current[-1] - clipped_first) <= 1.0e-7:
            current.append(clipped_second)
        else:
            if len(current) >= 2:
                segments.append(current)
            current = [clipped_first, clipped_second]
    if len(current) >= 2:
        segments.append(current)
    return [np.ascontiguousarray(segment, dtype="<f4") for segment in segments]


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


def command_export_3d(arguments: argparse.Namespace) -> None:
    planes = [
        {
            "axis": axis,
            "position": position,
            "enabled": True,
            "normalized": False,
            "origin_if_available": False,
        }
        for axis, position in zip(arguments.axis, arguments.position, strict=True)
    ]
    isosurfaces = []
    for specification in arguments.iso or []:
        variable, separator, value = specification.partition("=")
        if not separator:
            raise ValueError("--iso must use VARIABLE=VALUE")
        isosurfaces.append(
            {
                "id": len(isosurfaces) + 1,
                "name": f"{variable} = {value}",
                "variable": variable,
                "isovalue": float(value),
                "triangle_limit": arguments.triangle_limit,
            }
        )
    crop = None
    if arguments.crop is not None:
        crop = {"enabled": True, "fractions": arguments.crop}
    _json(
        _load_surface3d(
            input_path=arguments.input,
            variable_name=arguments.variable,
            output_path=arguments.output,
            planes=planes,
            isosurfaces=isosurfaces,
            crop=crop,
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
            elif method == "load_surface3d":
                result = _load_surface3d(
                    input_path=parameters["path"],
                    variable_name=parameters["variable"],
                    output_path=parameters["output"],
                    planes=list(parameters.get("planes") or []),
                    isosurfaces=list(parameters.get("isosurfaces") or []),
                    crop=parameters.get("crop"),
                    zone_index=int(parameters.get("zone", 0)),
                    cache=bool(parameters.get("cache", True)),
                    cache_dir=parameters.get("cache_dir"),
                    reuse_mesh_id=parameters.get("reuse_mesh_id"),
                )
            elif method == "trace_fieldlines3d":
                result = _trace_fieldlines3d(
                    input_path=parameters["path"],
                    components=list(parameters["components"]),
                    seeds=list(parameters["seeds"]),
                    output_path=parameters["output"],
                    zone_index=int(parameters.get("zone", 0)),
                    step=float(parameters.get("step", 0.1)),
                    max_steps=int(parameters.get("max_steps", 4000)),
                    max_length=float(parameters.get("max_length", 500.0)),
                    planet_radius=float(parameters.get("planet_radius", 2.5)),
                    crop=parameters.get("crop"),
                    cache=bool(parameters.get("cache", True)),
                    cache_dir=parameters.get("cache_dir"),
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

    export_3d = commands.add_parser("export-3d")
    export_3d.add_argument("input")
    export_3d.add_argument("variable")
    export_3d.add_argument("output")
    export_3d.add_argument("--axis", action="append", choices=("x", "y", "z"), default=[])
    export_3d.add_argument("--position", action="append", type=float, default=[])
    export_3d.add_argument("--iso", action="append", metavar="VARIABLE=VALUE")
    export_3d.add_argument("--triangle-limit", type=int, default=500_000)
    export_3d.add_argument(
        "--crop",
        nargs=6,
        type=float,
        metavar=("X0", "X1", "Y0", "Y1", "Z0", "Z1"),
    )
    export_3d.add_argument("--zone", type=int, default=0)
    export_3d.add_argument("--cache", action=argparse.BooleanOptionalAction, default=True)
    export_3d.add_argument("--cache-dir", default=str(_default_cache_dir()))
    export_3d.add_argument("--reuse-mesh-id")
    export_3d.set_defaults(handler=command_export_3d)

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
