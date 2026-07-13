from __future__ import annotations

import argparse
import importlib.util
import json
from pathlib import Path
import struct

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
        )
    )

    response = json.loads(capsys.readouterr().out)
    assert response["point_count"] == 4
    assert response["triangle_count"] == 2
    payload = output.read_bytes()
    assert payload[:4] == b"BPV1"
    header_size = struct.unpack_from("<I", payload, 4)[0]
    header = json.loads(payload[8 : 8 + header_size])
    assert header["variable"] == "density"
    assert header["positive_range"] == [1.0, 4.0]
    assert len(payload) == 8 + header_size + 4 * 3 * 4 + 2 * 3 * 4
