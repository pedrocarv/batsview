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
