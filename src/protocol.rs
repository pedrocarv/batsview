use std::{fs::File, io::Read, path::Path, sync::Arc};

use anyhow::{Context, Result, bail, ensure};
use bytemuck::{Pod, Zeroable};
use serde::{Deserialize, Serialize};

pub const BRIDGE_PROTOCOL: u32 = 2;

#[derive(Clone, Debug, Deserialize)]
pub struct PlotFile {
    pub path: String,
    pub name: String,
    pub size: u64,
    pub section: Option<String>,
    pub var_id: Option<u64>,
    pub time_step: Option<u64>,
    pub dump_index: Option<u64>,
}

#[derive(Debug, Deserialize)]
pub struct ScanResult {
    pub protocol: u32,
    pub directory: String,
    pub files: Vec<PlotFile>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct VariableInfo {
    pub source: String,
    pub canonical: String,
    pub unit: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ZoneInfo {
    pub index: usize,
    pub name: String,
    pub num_points: usize,
    pub num_elements: usize,
    pub zone_type: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct FileInfo {
    pub protocol: u32,
    pub path: String,
    pub title: String,
    pub section: Option<String>,
    pub variables: Vec<VariableInfo>,
    pub zones: Vec<ZoneInfo>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct PlotHeader {
    pub protocol: u32,
    pub path: String,
    pub title: String,
    pub section: Option<String>,
    pub zone: String,
    pub variable: String,
    pub source_variable: String,
    pub unit: Option<String>,
    pub x_label: String,
    pub y_label: String,
    pub point_count: usize,
    pub triangle_count: usize,
    pub mesh_id: String,
    pub mesh_included: bool,
    pub bounds: [f32; 4],
    pub value_range: [f32; 2],
    pub positive_range: Option<[f32; 2]>,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct Position {
    pub x: f32,
    pub y: f32,
}

#[derive(Clone, Debug)]
pub struct MeshData {
    pub id: String,
    pub positions: Vec<Position>,
    pub indices: Vec<u32>,
}

impl MeshData {
    pub fn numeric_bytes(&self) -> usize {
        self.positions.len() * size_of::<Position>() + self.indices.len() * size_of::<u32>()
    }
}

#[derive(Clone, Debug)]
pub struct PlotData {
    pub header: PlotHeader,
    pub mesh: Arc<MeshData>,
    pub values: Vec<f32>,
}

impl PlotData {
    pub fn scalar_bytes(&self) -> usize {
        self.values.len() * size_of::<f32>()
    }
}

pub fn read_plot(path: &Path, reused_mesh: Option<Arc<MeshData>>) -> Result<PlotData> {
    let mut file = File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let mut prefix = [0_u8; 8];
    file.read_exact(&mut prefix)?;
    if &prefix[..4] != b"BPV2" {
        bail!("unsupported plot exchange format (expected BPV2)");
    }
    let header_size = u32::from_le_bytes(prefix[4..8].try_into().unwrap()) as usize;
    ensure!(
        header_size <= 16 * 1024 * 1024,
        "invalid BPV2 header size: {header_size}"
    );
    let mut header_bytes = vec![0; header_size];
    file.read_exact(&mut header_bytes)?;
    let header: PlotHeader = serde_json::from_slice(&header_bytes)?;
    ensure!(
        header.protocol == BRIDGE_PROTOCOL,
        "unsupported bridge protocol {}",
        header.protocol
    );
    ensure!(
        header.mesh_id.len() == 32 && header.mesh_id.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "invalid BPV2 mesh identifier"
    );

    let position_bytes = header
        .point_count
        .checked_mul(size_of::<Position>())
        .context("position buffer size overflow")?;
    let value_bytes = header
        .point_count
        .checked_mul(size_of::<f32>())
        .context("scalar buffer size overflow")?;
    let index_count = header
        .triangle_count
        .checked_mul(3)
        .context("index count overflow")?;
    let index_bytes = index_count
        .checked_mul(size_of::<u32>())
        .context("index buffer size overflow")?;
    let mesh_bytes = if header.mesh_included {
        position_bytes
            .checked_add(index_bytes)
            .context("mesh payload size overflow")?
    } else {
        0
    };
    let expected = 8_u64 + header_size as u64 + value_bytes as u64 + mesh_bytes as u64;
    ensure!(
        file.metadata()?.len() == expected,
        "BPV2 payload size does not match its header"
    );

    let mesh = if header.mesh_included {
        let mut positions = vec![Position::zeroed(); header.point_count];
        file.read_exact(bytemuck::cast_slice_mut(&mut positions))?;
        let mut values = vec![0.0_f32; header.point_count];
        file.read_exact(bytemuck::cast_slice_mut(&mut values))?;
        let mut indices = vec![0_u32; index_count];
        file.read_exact(bytemuck::cast_slice_mut(&mut indices))?;
        ensure!(
            !indices
                .iter()
                .any(|&index| index as usize >= positions.len()),
            "BPV2 mesh contains an out-of-range index"
        );
        return Ok(PlotData {
            mesh: Arc::new(MeshData {
                id: header.mesh_id.clone(),
                positions,
                indices,
            }),
            header,
            values,
        });
    } else {
        let mesh = reused_mesh.context("BPV2 payload references a mesh that is not available")?;
        ensure!(
            mesh.id == header.mesh_id,
            "BPV2 reused mesh identifier does not match"
        );
        ensure!(
            mesh.positions.len() == header.point_count && mesh.indices.len() == index_count,
            "BPV2 reused mesh dimensions do not match"
        );
        mesh
    };
    let mut values = vec![0.0_f32; header.point_count];
    file.read_exact(bytemuck::cast_slice_mut(&mut values))?;
    Ok(PlotData {
        header,
        mesh,
        values,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn header(mesh_included: bool) -> PlotHeader {
        PlotHeader {
            protocol: BRIDGE_PROTOCOL,
            path: "test.plt".into(),
            title: "fixture".into(),
            section: Some("z=0".into()),
            zone: "cut".into(),
            variable: "density".into(),
            source_variable: "Rho".into(),
            unit: None,
            x_label: "X".into(),
            y_label: "Y".into(),
            point_count: 3,
            triangle_count: 1,
            mesh_id: "0123456789abcdef0123456789abcdef".into(),
            mesh_included,
            bounds: [0.0, 1.0, 0.0, 1.0],
            value_range: [1.0, 3.0],
            positive_range: Some([1.0, 3.0]),
        }
    }

    fn write_payload(
        file: &mut tempfile::NamedTempFile,
        header: &PlotHeader,
        positions: &[Position],
        values: &[f32],
        indices: &[u32],
    ) {
        let encoded = serde_json::to_vec(header).unwrap();
        file.write_all(b"BPV2").unwrap();
        file.write_all(&(encoded.len() as u32).to_le_bytes())
            .unwrap();
        file.write_all(&encoded).unwrap();
        if header.mesh_included {
            file.write_all(bytemuck::cast_slice(positions)).unwrap();
        }
        file.write_all(bytemuck::cast_slice(values)).unwrap();
        if header.mesh_included {
            file.write_all(bytemuck::cast_slice(indices)).unwrap();
        }
        file.flush().unwrap();
    }

    #[test]
    fn rejects_wrong_magic() {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        file.write_all(b"NOPE\0\0\0\0").unwrap();
        assert!(
            read_plot(file.path(), None)
                .unwrap_err()
                .to_string()
                .contains("BPV2")
        );
    }

    #[test]
    fn reads_full_mesh_and_scalar_only_payloads() {
        let positions = [
            Position { x: 0.0, y: 0.0 },
            Position { x: 1.0, y: 0.0 },
            Position { x: 0.0, y: 1.0 },
        ];
        let values = [1.0, 2.0, 3.0];
        let indices = [0_u32, 1, 2];
        let mut full = tempfile::NamedTempFile::new().unwrap();
        write_payload(&mut full, &header(true), &positions, &values, &indices);
        let loaded = read_plot(full.path(), None).unwrap();
        assert_eq!(loaded.mesh.positions.len(), 3);
        assert_eq!(loaded.mesh.indices, indices);
        assert_eq!(loaded.values, values);

        let mut scalar = tempfile::NamedTempFile::new().unwrap();
        write_payload(&mut scalar, &header(false), &[], &values, &[]);
        let reused = read_plot(scalar.path(), Some(loaded.mesh.clone())).unwrap();
        assert!(Arc::ptr_eq(&loaded.mesh, &reused.mesh));
        assert_eq!(reused.values, values);
    }

    #[test]
    fn scalar_only_payload_requires_the_matching_mesh() {
        let mut scalar = tempfile::NamedTempFile::new().unwrap();
        write_payload(&mut scalar, &header(false), &[], &[1.0, 2.0, 3.0], &[]);
        assert!(
            read_plot(scalar.path(), None)
                .unwrap_err()
                .to_string()
                .contains("not available")
        );
    }

    #[test]
    fn rejects_out_of_range_mesh_indices() {
        let positions = [Position { x: 0.0, y: 0.0 }; 3];
        let mut full = tempfile::NamedTempFile::new().unwrap();
        write_payload(
            &mut full,
            &header(true),
            &positions,
            &[1.0, 2.0, 3.0],
            &[0, 1, 3],
        );
        assert!(
            read_plot(full.path(), None)
                .unwrap_err()
                .to_string()
                .contains("out-of-range")
        );
    }
}
