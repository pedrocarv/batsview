from __future__ import annotations

import argparse
import importlib.util
import io
import json
from pathlib import Path
import struct
import subprocess
import sys

import numpy as np

import batsplot as bp


BRIDGE_PATH = Path(__file__).parents[1] / "bridge" / "batsview_bridge.py"
SPEC = importlib.util.spec_from_file_location("batsview_bridge", BRIDGE_PATH)
assert SPEC is not None and SPEC.loader is not None
bridge = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(bridge)


def test_record_extracts_bats_filename_fields(tmp_path: Path) -> None:
    path = tmp_path / "z=0_var_3_t00000100_n00000042.plt"
    path.write_bytes(b"")
    record = bridge._record(path)
    assert record["section"] == "z=0"
    assert record["var_id"] == 3
    assert record["time_step"] == 100
    assert record["dump_index"] == 42


def test_cache_directory_can_be_overridden(tmp_path: Path, monkeypatch) -> None:
    cache = tmp_path / "cache"
    monkeypatch.setenv("BATSVIEW_CACHE_DIR", str(cache))
    assert bridge._default_cache_dir() == cache


def test_export_writes_versioned_triangle_payload(tmp_path: Path, monkeypatch, capsys) -> None:
    path = tmp_path / "z=0_var_1_t00000001_n00000001.plt"
    path.write_bytes(b"fixture")
    zone = bp.Zone(
        name="cut",
        arrays={
            "X [R]": np.array([0.0, 1.0, 1.0, 0.0]),
            "Y [R]": np.array([0.0, 0.0, 1.0, 1.0]),
            "density": np.array([1.0, 2.0, 3.0, 4.0]),
        },
        connectivity=np.array([[0, 1, 2, 3]], dtype=np.uint32),
        zone_type="Quad",
        original_names={"density": "Rho [amu/cm^3]"},
        units={"density": "amu/cm^3"},
    )
    dataset = bp.Dataset(path=path, title="fixture", zones=(zone,), section="z=0")
    monkeypatch.setattr(bp, "read", lambda *args, **kwargs: dataset)
    output = tmp_path / "plot.bpv"
    bridge.command_export(
        argparse.Namespace(
            input=str(path),
            variable="density",
            output=str(output),
            zone=0,
            cache=False,
            cache_dir=str(tmp_path / "cache"),
            reuse_mesh_id=None,
        )
    )

    response = json.loads(capsys.readouterr().out)
    assert response["point_count"] == 4
    assert response["triangle_count"] == 2
    payload = output.read_bytes()
    assert payload[:4] == b"BPV2"
    header_size = struct.unpack_from("<I", payload, 4)[0]
    header = json.loads(payload[8 : 8 + header_size])
    assert header["variable"] == "density"
    assert header["positive_range"] == [1.0, 4.0]
    assert header["mesh_included"] is True
    assert len(header["mesh_id"]) == 32
    assert len(payload) == 8 + header_size + 4 * 2 * 4 + 4 * 4 + 2 * 3 * 4


def test_reused_mesh_writes_only_scalar_values(tmp_path: Path, monkeypatch, capsys) -> None:
    path = tmp_path / "z=0_var_1_t00000001_n00000001.plt"
    path.write_bytes(b"fixture")
    zone = bp.Zone(
        name="cut",
        arrays={
            "X [R]": np.array([0.0, 1.0, 1.0]),
            "Y [R]": np.array([0.0, 0.0, 1.0]),
            "density": np.array([1.0, 2.0, 3.0]),
        },
        connectivity=np.array([[0, 1, 2]], dtype=np.uint32),
        zone_type="Triangle",
    )
    dataset = bp.Dataset(path=path, title="fixture", zones=(zone,), section="z=0")
    monkeypatch.setattr(bp, "read", lambda *args, **kwargs: dataset)
    full = tmp_path / "full.bpv"
    response = bridge._load_plot(
        input_path=str(path),
        variable_name="density",
        output_path=str(full),
        cache=False,
    )
    scalar = tmp_path / "scalar.bpv"
    second = bridge._load_plot(
        input_path=str(path),
        variable_name="density",
        output_path=str(scalar),
        cache=False,
        reuse_mesh_id=response["mesh_id"],
    )
    assert second["mesh_included"] is False
    payload = scalar.read_bytes()
    header_size = struct.unpack_from("<I", payload, 4)[0]
    assert len(payload) == 8 + header_size + 3 * 4
    capsys.readouterr()


def test_mesh_id_is_stable_and_changes_with_connectivity() -> None:
    positions = np.array([[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]], dtype="<f4")
    first = np.array([[0, 1, 2]], dtype="<u4")
    second = np.array([[0, 2, 1]], dtype="<u4")
    assert bridge._mesh_id(positions, first) == bridge._mesh_id(positions.copy(), first.copy())
    assert bridge._mesh_id(positions, first) != bridge._mesh_id(positions, second)


def test_3d_brick_slice_writes_compact_surface_payload(tmp_path: Path, monkeypatch) -> None:
    path = tmp_path / "3d__var_1_t00000001_n00000001.plt"
    path.write_bytes(b"fixture")
    coordinates = np.array(
        [
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [1.0, 1.0, 0.0],
            [0.0, 1.0, 0.0],
            [0.0, 0.0, 1.0],
            [1.0, 0.0, 1.0],
            [1.0, 1.0, 1.0],
            [0.0, 1.0, 1.0],
        ]
    )
    zone = bp.Zone(
        name="volume",
        arrays={
            "X [R]": coordinates[:, 0],
            "Y [R]": coordinates[:, 1],
            "Z [R]": coordinates[:, 2],
            "density": coordinates.sum(axis=1) + 1.0,
        },
        connectivity=np.arange(8, dtype=np.uint32).reshape(1, 8),
        zone_type="Brick",
        original_names={"density": "Rho [amu/cm^3]"},
        units={"density": "amu/cm^3"},
    )
    dataset = bp.Dataset(path=path, title="3D fixture", zones=(zone,), section="3d")
    monkeypatch.setattr(bp, "read", lambda *args, **kwargs: dataset)
    output = tmp_path / "slice.b3s"
    response = bridge._load_surface3d(
        input_path=str(path),
        variable_name="density",
        output_path=str(output),
        planes=[{"axis": "x", "position": 0.5, "enabled": True, "normalized": True}],
        cache=False,
    )

    assert response["triangle_count"] > 0
    assert response["vertex_count"] <= response["triangle_count"] * 3
    assert len(response["layers"]) == 1
    layer = response["layers"][0]
    assert layer["kind"] == "slice"
    assert layer["axis"] == "x"
    assert layer["position"] == 0.5
    assert layer["index_start"] == 0
    assert layer["index_count"] == response["triangle_count"] * 3
    payload = output.read_bytes()
    assert payload[:4] == b"B3S2"
    header_size = struct.unpack_from("<I", payload, 4)[0]
    header = json.loads(payload[8 : 8 + header_size])
    offset = 8 + header_size
    positions = np.frombuffer(
        payload, dtype="<f4", count=header["vertex_count"] * 3, offset=offset
    ).reshape(-1, 3)
    assert np.allclose(positions[:, 0], 0.5)
    assert len(payload) == (
        8
        + header_size
        + header["vertex_count"] * 3 * 4
        + header["vertex_count"] * 4
        + header["triangle_count"] * 3 * 4
    )

    scalar_only = tmp_path / "slice-scalar.b3s"
    reused = bridge._load_surface3d(
        input_path=str(path),
        variable_name="density",
        output_path=str(scalar_only),
        planes=[{"axis": "x", "position": 0.5, "enabled": True, "normalized": True}],
        cache=False,
        reuse_mesh_id=response["mesh_id"],
    )
    assert reused["mesh_included"] is False
    scalar_payload = scalar_only.read_bytes()
    scalar_header_size = struct.unpack_from("<I", scalar_payload, 4)[0]
    assert len(scalar_payload) == 8 + scalar_header_size + reused["vertex_count"] * 4


def test_tetra_isosurface_interpolates_secondary_scalar() -> None:
    coordinates = np.asarray(
        [[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
        dtype=np.float64,
    )
    tetrahedra = np.asarray([[0, 1, 2, 3]], dtype=np.intp)
    contour = coordinates.sum(axis=1)
    secondary = 2.0 * coordinates[:, 0] + 3.0 * coordinates[:, 1] + 4.0 * coordinates[:, 2]
    positions, values = bridge._contour_tetrahedra(
        tetrahedra, coordinates, contour, secondary, 0.5
    )
    assert positions.shape == (3, 3)
    assert np.allclose(positions.sum(axis=1), 0.5)
    expected = 2.0 * positions[:, 0] + 3.0 * positions[:, 1] + 4.0 * positions[:, 2]
    assert np.allclose(values, expected)


def test_crop_clips_triangles_and_interpolates_values_exactly() -> None:
    positions = np.asarray(
        [[-1.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]], dtype=np.float64
    )
    values = positions[:, 0].copy()
    crop = np.asarray([[-0.25, 0.5], [-1.0, 2.0], [-1.0, 1.0]], dtype=np.float64)
    clipped, interpolated = bridge._clip_triangles_to_box(positions, values, crop)
    assert clipped.shape[0] % 3 == 0
    assert clipped.shape[0] >= 3
    assert np.all(clipped[:, 0] >= -0.25 - 1.0e-12)
    assert np.all(clipped[:, 0] <= 0.5 + 1.0e-12)
    assert np.any(np.isclose(clipped[:, 0], -0.25))
    assert np.any(np.isclose(clipped[:, 0], 0.5))
    assert np.allclose(interpolated, clipped[:, 0])


def test_crop_clips_fieldline_segments_at_exact_boundaries() -> None:
    line = np.asarray(
        [[-2.0, 0.0, 0.0], [0.0, 0.0, 0.0], [2.0, 0.0, 0.0]], dtype=np.float64
    )
    bounds = np.asarray([[-0.5, 0.75], [-1.0, 1.0], [-1.0, 1.0]], dtype=np.float64)
    clipped = bridge._clip_polyline_to_box(line, bounds)
    assert len(clipped) == 1
    assert np.allclose(clipped[0][0], [-0.5, 0.0, 0.0])
    assert np.allclose(clipped[0][-1], [0.75, 0.0, 0.0])


def test_3d_isosurface_crop_and_inactive_layers_are_preserved(
    tmp_path: Path, monkeypatch
) -> None:
    path = tmp_path / "3d__var_1_t00000001_n00000001.plt"
    path.write_bytes(b"fixture")
    coordinates = np.asarray(
        [
            [0.0, 0.0, 0.0], [1.0, 0.0, 0.0],
            [1.0, 1.0, 0.0], [0.0, 1.0, 0.0],
            [0.0, 0.0, 1.0], [1.0, 0.0, 1.0],
            [1.0, 1.0, 1.0], [0.0, 1.0, 1.0],
        ],
        dtype=np.float64,
    )
    zone = bp.Zone(
        name="volume",
        arrays={
            "X [R]": coordinates[:, 0],
            "Y [R]": coordinates[:, 1],
            "Z [R]": coordinates[:, 2],
            "density": coordinates.sum(axis=1),
            "temperature": 10.0 + 2.0 * coordinates[:, 0],
        },
        connectivity=np.arange(8, dtype=np.uint32).reshape(1, 8),
        zone_type="Brick",
        units={"temperature": "K"},
    )
    dataset = bp.Dataset(path=path, title="3D fixture", zones=(zone,), section="3d")
    monkeypatch.setattr(bp, "read", lambda *args, **kwargs: dataset)
    monkeypatch.setattr(
        bp,
        "inspect",
        lambda *args, **kwargs: argparse.Namespace(
            variables=["density", "temperature", "missing"]
        ),
    )
    output = tmp_path / "isosurfaces.b3s"
    response = bridge._load_surface3d(
        input_path=str(path),
        variable_name="density",
        output_path=str(output),
        planes=[],
        isosurfaces=[
            {
                "id": 11,
                "name": "Density shell",
                "variable": "density",
                "color_variable": "temperature",
                "isovalue": 1.0,
                "triangle_limit": 500_000,
            },
            {
                "id": 12,
                "name": "Unavailable color",
                "variable": "density",
                "color_variable": "missing",
                "isovalue": 1.0,
            },
        ],
        crop={"enabled": True, "fractions": [0.25, 0.75, 0.0, 1.0, 0.0, 1.0]},
        cache=False,
    )
    assert len(response["layers"]) == 2
    active, inactive = response["layers"]
    assert active["kind"] == "isosurface"
    assert active["layer_id"] == 11
    assert active["color_variable"] == "temperature"
    assert active["unit"] == "K"
    assert active["rendered_triangles"] > 0
    assert inactive["layer_id"] == 12
    assert "not available" in inactive["inactive_reason"]
    payload = output.read_bytes()
    header_size = struct.unpack_from("<I", payload, 4)[0]
    header = json.loads(payload[8 : 8 + header_size])
    positions = np.frombuffer(
        payload,
        dtype="<f4",
        count=header["vertex_count"] * 3,
        offset=8 + header_size,
    ).reshape(-1, 3)
    assert np.all(positions[:, 0] >= 0.25 - 1.0e-6)
    assert np.all(positions[:, 0] <= 0.75 + 1.0e-6)


def test_empty_out_of_range_isosurface_returns_inactive_b3s2(
    tmp_path: Path, monkeypatch
) -> None:
    path = tmp_path / "3d__var_1_t00000001_n00000001.plt"
    path.write_bytes(b"fixture")
    coordinates = np.asarray(
        [[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]]
    )
    zone = bp.Zone(
        name="volume",
        arrays={
            "X [R]": coordinates[:, 0],
            "Y [R]": coordinates[:, 1],
            "Z [R]": coordinates[:, 2],
            "density": coordinates.sum(axis=1),
        },
        connectivity=np.arange(4, dtype=np.uint32).reshape(1, 4),
        zone_type="Tetra",
    )
    monkeypatch.setattr(
        bp,
        "read",
        lambda *args, **kwargs: bp.Dataset(
            path=path, title="fixture", zones=(zone,), section="3d"
        ),
    )
    monkeypatch.setattr(
        bp, "inspect", lambda *args, **kwargs: argparse.Namespace(variables=["density"])
    )
    output = tmp_path / "empty.b3s"
    response = bridge._load_surface3d(
        input_path=str(path),
        variable_name="density",
        output_path=str(output),
        planes=[],
        isosurfaces=[{"id": 1, "variable": "density", "isovalue": 99.0}],
        cache=False,
    )
    assert response["triangle_count"] == 0
    assert response["vertex_count"] == 0
    assert response["layers"][0]["inactive_reason"]
    assert output.read_bytes()[:4] == b"B3S2"


def test_3d_field_tracer_integrates_constant_brick_field(tmp_path: Path, monkeypatch) -> None:
    path = tmp_path / "3d__var_1_t00000001_n00000001.plt"
    path.write_bytes(b"fixture")
    coordinates = np.array(
        [
            [0.0, 0.0, 0.0], [1.0, 0.0, 0.0],
            [1.0, 1.0, 0.0], [0.0, 1.0, 0.0],
            [0.0, 0.0, 1.0], [1.0, 0.0, 1.0],
            [1.0, 1.0, 1.0], [0.0, 1.0, 1.0],
        ],
        dtype=np.float32,
    )
    zone = bp.Zone(
        name="volume",
        arrays={
            "X [R]": coordinates[:, 0],
            "Y [R]": coordinates[:, 1],
            "Z [R]": coordinates[:, 2],
            "magnetic_field.x": np.ones(8, dtype=np.float32),
            "magnetic_field.y": np.zeros(8, dtype=np.float32),
            "magnetic_field.z": np.zeros(8, dtype=np.float32),
        },
        connectivity=np.arange(8, dtype=np.uint32).reshape(1, 8),
        zone_type="Brick",
    )
    dataset = bp.Dataset(path=path, title="3D fixture", zones=(zone,), section="3d")
    monkeypatch.setattr(bp, "read", lambda *args, **kwargs: dataset)
    output = tmp_path / "lines.b3l"
    response = bridge._trace_fieldlines3d(
        input_path=str(path),
        components=["magnetic_field.x", "magnetic_field.y", "magnetic_field.z"],
        seeds=[[0.5, 0.5, 0.5]],
        output_path=str(output),
        step=0.05,
        max_steps=100,
        max_length=2.0,
        planet_radius=0.01,
        cache=False,
    )
    assert response["line_count"] == 1
    payload = output.read_bytes()
    assert payload[:4] == b"B3L1"
    header_size = struct.unpack_from("<I", payload, 4)[0]
    header = json.loads(payload[8 : 8 + header_size])
    offset = 8 + header_size + (header["line_count"] + 1) * 4
    points = np.frombuffer(
        payload, dtype="<f4", count=header["point_count"] * 3, offset=offset
    ).reshape(-1, 3)
    assert points[0, 0] < 0.1
    assert points[-1, 0] > 0.9
    assert np.allclose(points[:, 1:], 0.5, atol=1.0e-5)


def test_fieldline_locator_reuses_only_an_identical_grid() -> None:
    connectivity = np.arange(8, dtype=np.uint32).reshape(1, 8)
    coordinates = np.asarray(
        [
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [1.0, 1.0, 0.0],
            [0.0, 1.0, 0.0],
            [0.0, 0.0, 1.0],
            [1.0, 0.0, 1.0],
            [1.0, 1.0, 1.0],
            [0.0, 1.0, 1.0],
        ],
        dtype=np.float32,
    )
    bridge._FIELDLINE_LOCATOR_CACHE = None
    mesh_id, first = bridge._fieldline_locator(connectivity, coordinates)
    repeated_id, repeated = bridge._fieldline_locator(connectivity.copy(), coordinates.copy())
    assert repeated_id == mesh_id
    assert repeated is first

    changed = coordinates.copy()
    changed[6, 2] = 1.5
    changed_id, replacement = bridge._fieldline_locator(connectivity, changed)
    assert changed_id != mesh_id
    assert replacement is not first
    bridge._FIELDLINE_LOCATOR_CACHE = None


def test_serve_handles_multiple_requests_and_structured_errors(monkeypatch, capsys) -> None:
    monkeypatch.setattr(
        bridge,
        "_inspect",
        lambda path: {"protocol": bridge.PROTOCOL, "path": str(path)},
    )
    requests = [
        {"protocol": bridge.PROTOCOL, "id": 1, "method": "inspect", "params": {"path": "a.plt"}},
        {"protocol": bridge.PROTOCOL, "id": 2, "method": "unknown", "params": {}},
        {"protocol": bridge.PROTOCOL, "id": 3, "method": "shutdown", "params": {}},
    ]
    monkeypatch.setattr(
        bridge.sys,
        "stdin",
        io.StringIO("".join(json.dumps(request) + "\n" for request in requests)),
    )
    bridge.command_serve(argparse.Namespace())
    responses = [json.loads(line) for line in capsys.readouterr().out.splitlines()]
    assert [response["id"] for response in responses] == [1, 2, 3]
    assert responses[0]["ok"] is True
    assert responses[1]["ok"] is False
    assert responses[1]["error"]["type"] == "ValueError"
    assert responses[2]["ok"] is True


def test_serve_mode_keeps_one_subprocess_for_multiple_requests() -> None:
    requests = [
        {"protocol": bridge.PROTOCOL, "id": 10, "method": "unknown", "params": {}},
        {"protocol": bridge.PROTOCOL, "id": 11, "method": "shutdown", "params": {}},
    ]
    process = subprocess.run(
        [sys.executable, str(BRIDGE_PATH), "serve"],
        input="".join(json.dumps(request) + "\n" for request in requests),
        text=True,
        capture_output=True,
        check=False,
        timeout=10,
    )
    assert process.returncode == 0, process.stderr
    responses = [json.loads(line) for line in process.stdout.splitlines()]
    assert [response["id"] for response in responses] == [10, 11]
    assert responses[0]["ok"] is False
    assert responses[1]["ok"] is True
