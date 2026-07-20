use std::{collections::HashSet, fs::File, io::Read, path::Path, sync::Arc};

use anyhow::{Context, Result, bail, ensure};
use bytemuck::{Pod, Zeroable};
use serde::{Deserialize, Serialize};

pub const BRIDGE_PROTOCOL: u32 = 4;

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
    #[serde(default)]
    pub spatial_dimension: u8,
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

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SurfaceLayerKind {
    #[default]
    Slice,
    Isosurface,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct SurfaceLayerHeader {
    #[serde(default)]
    pub kind: SurfaceLayerKind,
    #[serde(default)]
    pub layer_id: Option<u64>,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub axis: Option<String>,
    #[serde(default)]
    pub position: Option<f32>,
    #[serde(default)]
    pub variable: String,
    #[serde(default)]
    pub color_variable: Option<String>,
    #[serde(default)]
    pub isovalue: Option<f64>,
    #[serde(default)]
    pub unit: String,
    #[serde(default)]
    pub value_range: Option<[f32; 2]>,
    #[serde(default)]
    pub volume_range: Option<[f32; 2]>,
    pub index_start: u32,
    pub index_count: u32,
    #[serde(default)]
    pub source_triangles: u32,
    #[serde(default)]
    pub rendered_triangles: u32,
    #[serde(default)]
    pub inactive_reason: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Surface3dHeader {
    pub protocol: u32,
    pub source: String,
    pub title: String,
    pub dataset_title: String,
    pub section: String,
    pub zone_name: String,
    pub variable: String,
    pub canonical_name: String,
    pub unit: String,
    pub axis_labels: [String; 3],
    pub vertex_count: usize,
    pub triangle_count: usize,
    pub mesh_id: String,
    pub mesh_included: bool,
    pub bounds: [f32; 6],
    #[serde(default)]
    pub crop_bounds: Option<[f32; 6]>,
    pub value_range: [f32; 2],
    #[serde(default)]
    pub volume_value_range: Option<[f32; 2]>,
    #[serde(default)]
    pub layers: Vec<SurfaceLayerHeader>,
    pub time: Option<f64>,
    pub dump: Option<i64>,
}

impl Surface3dHeader {
    pub fn active_bounds(&self) -> [f32; 6] {
        self.crop_bounds.unwrap_or(self.bounds)
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Pod, Zeroable)]
pub struct Position3 {
    pub x: f32,
    pub y: f32,
    pub z: f32,
}

#[derive(Debug)]
pub struct SurfaceMesh3d {
    pub id: String,
    pub positions: Vec<Position3>,
    pub indices: Vec<u32>,
}

impl SurfaceMesh3d {
    pub fn numeric_bytes(&self) -> usize {
        self.positions.len() * std::mem::size_of::<Position3>()
            + self.indices.len() * std::mem::size_of::<u32>()
    }
}

#[derive(Debug)]
pub struct Surface3dData {
    pub header: Surface3dHeader,
    pub mesh: Arc<SurfaceMesh3d>,
    pub values: Vec<f32>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct FieldLines3dHeader {
    pub protocol: u32,
    pub source: String,
    pub section: String,
    pub zone_name: String,
    pub components: [String; 3],
    pub line_count: usize,
    pub point_count: usize,
    pub seed_count: usize,
    pub bounds: [f32; 6],
    pub planet_radius: f32,
}

#[derive(Debug)]
pub struct FieldLines3dData {
    pub header: FieldLines3dHeader,
    pub offsets: Vec<u32>,
    pub positions: Vec<Position3>,
}

impl FieldLines3dData {
    pub fn numeric_bytes(&self) -> usize {
        self.offsets.len() * size_of::<u32>() + self.positions.len() * size_of::<Position3>()
    }

    pub fn lines(&self) -> impl Iterator<Item = &[Position3]> {
        self.offsets
            .windows(2)
            .map(|range| &self.positions[range[0] as usize..range[1] as usize])
    }
}

impl Surface3dData {
    pub fn numeric_bytes_without_mesh(&self) -> usize {
        self.values.len() * std::mem::size_of::<f32>()
    }
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

pub fn read_surface3d(
    path: &Path,
    reused_mesh: Option<Arc<SurfaceMesh3d>>,
) -> Result<Surface3dData> {
    let mut file = File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let mut prefix = [0_u8; 8];
    file.read_exact(&mut prefix)?;
    if &prefix[..4] != b"B3S2" {
        bail!("unsupported 3D surface exchange format (expected B3S2)");
    }
    let header_size = u32::from_le_bytes(prefix[4..8].try_into().unwrap()) as usize;
    ensure!(
        header_size <= 16 * 1024 * 1024,
        "invalid B3S2 header size: {header_size}"
    );
    let mut header_bytes = vec![0; header_size];
    file.read_exact(&mut header_bytes)?;
    let header: Surface3dHeader = serde_json::from_slice(&header_bytes)?;
    ensure!(
        header.protocol == BRIDGE_PROTOCOL,
        "unsupported bridge protocol {}",
        header.protocol
    );
    ensure!(
        header.mesh_id.len() == 32 && header.mesh_id.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "invalid B3S2 mesh identifier"
    );
    ensure!(
        header.bounds.into_iter().all(f32::is_finite)
            && header.bounds[1] > header.bounds[0]
            && header.bounds[3] > header.bounds[2]
            && header.bounds[5] > header.bounds[4],
        "B3S2 contains invalid volume bounds"
    );
    ensure!(
        header.value_range.into_iter().all(f32::is_finite)
            && header.value_range[1] >= header.value_range[0],
        "B3S2 contains an invalid value range"
    );
    if let Some(crop) = header.crop_bounds {
        ensure!(
            crop.into_iter().all(f32::is_finite)
                && crop[1] > crop[0]
                && crop[3] > crop[2]
                && crop[5] > crop[4]
                && crop[0] >= header.bounds[0]
                && crop[1] <= header.bounds[1]
                && crop[2] >= header.bounds[2]
                && crop[3] <= header.bounds[3]
                && crop[4] >= header.bounds[4]
                && crop[5] <= header.bounds[5],
            "B3S2 contains invalid crop bounds"
        );
    }
    let mut layer_ids = HashSet::new();
    let mut range_cursor = 0_u32;
    let isosurface_count = header
        .layers
        .iter()
        .filter(|layer| layer.kind == SurfaceLayerKind::Isosurface)
        .count();
    ensure!(
        header.layers.len() <= 11
            && isosurface_count <= 8
            && header.layers.iter().all(|layer| {
                let range_valid = layer.index_start as usize <= header.triangle_count * 3
                    && layer.index_count as usize <= header.triangle_count * 3
                    && layer.index_start as usize + layer.index_count as usize
                        <= header.triangle_count * 3
                    && layer.index_count.is_multiple_of(3);
                let metadata_valid = match layer.kind {
                    SurfaceLayerKind::Slice => {
                        layer
                            .axis
                            .as_deref()
                            .is_some_and(|axis| matches!(axis, "x" | "y" | "z"))
                            && layer.position.is_some_and(f32::is_finite)
                    }
                    SurfaceLayerKind::Isosurface => {
                        layer.layer_id.is_some_and(|id| layer_ids.insert(id))
                            && layer.isovalue.is_some_and(f64::is_finite)
                            && !layer.variable.trim().is_empty()
                    }
                };
                let values_valid = layer.value_range.is_none_or(|range| {
                    range.into_iter().all(f32::is_finite) && range[1] >= range[0]
                });
                let contiguous = layer.index_count == 0 || {
                    let valid = layer.index_start == range_cursor;
                    range_cursor = layer.index_start.saturating_add(layer.index_count);
                    valid
                };
                range_valid && metadata_valid && values_valid && contiguous
            }),
        "B3S2 layer metadata or index range is invalid"
    );
    ensure!(
        range_cursor as usize == header.triangle_count * 3,
        "B3S2 layer ranges do not cover the index payload"
    );

    let position_bytes = header
        .vertex_count
        .checked_mul(size_of::<Position3>())
        .context("3D position buffer size overflow")?;
    let value_bytes = header
        .vertex_count
        .checked_mul(size_of::<f32>())
        .context("3D scalar buffer size overflow")?;
    let index_count = header
        .triangle_count
        .checked_mul(3)
        .context("3D index count overflow")?;
    let index_bytes = index_count
        .checked_mul(size_of::<u32>())
        .context("3D index buffer size overflow")?;
    let mesh_bytes = if header.mesh_included {
        position_bytes
            .checked_add(index_bytes)
            .context("3D mesh payload size overflow")?
    } else {
        0
    };
    let expected = 8_u64 + header_size as u64 + value_bytes as u64 + mesh_bytes as u64;
    ensure!(
        file.metadata()?.len() == expected,
        "B3S2 payload size does not match its header"
    );

    if header.mesh_included {
        let mut positions = vec![Position3::zeroed(); header.vertex_count];
        file.read_exact(bytemuck::cast_slice_mut(&mut positions))?;
        let mut values = vec![0.0_f32; header.vertex_count];
        file.read_exact(bytemuck::cast_slice_mut(&mut values))?;
        let mut indices = vec![0_u32; index_count];
        file.read_exact(bytemuck::cast_slice_mut(&mut indices))?;
        ensure!(
            !indices
                .iter()
                .any(|&index| index as usize >= positions.len()),
            "B3S2 mesh contains an out-of-range index"
        );
        return Ok(Surface3dData {
            mesh: Arc::new(SurfaceMesh3d {
                id: header.mesh_id.clone(),
                positions,
                indices,
            }),
            header,
            values,
        });
    }

    let mesh = reused_mesh.context("B3S2 payload references a mesh that is not available")?;
    ensure!(
        mesh.id == header.mesh_id,
        "B3S2 reused mesh identifier does not match"
    );
    ensure!(
        mesh.positions.len() == header.vertex_count && mesh.indices.len() == index_count,
        "B3S2 reused mesh dimensions do not match"
    );
    let mut values = vec![0.0_f32; header.vertex_count];
    file.read_exact(bytemuck::cast_slice_mut(&mut values))?;
    Ok(Surface3dData {
        header,
        mesh,
        values,
    })
}

pub fn read_fieldlines3d(path: &Path) -> Result<FieldLines3dData> {
    let mut file = File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let mut prefix = [0_u8; 8];
    file.read_exact(&mut prefix)?;
    if &prefix[..4] != b"B3L1" {
        bail!("unsupported 3D field-line exchange format (expected B3L1)");
    }
    let header_size = u32::from_le_bytes(prefix[4..8].try_into().unwrap()) as usize;
    ensure!(
        header_size <= 16 * 1024 * 1024,
        "invalid B3L1 header size: {header_size}"
    );
    let mut header_bytes = vec![0; header_size];
    file.read_exact(&mut header_bytes)?;
    let header: FieldLines3dHeader = serde_json::from_slice(&header_bytes)?;
    ensure!(
        header.protocol == BRIDGE_PROTOCOL,
        "unsupported bridge protocol {}",
        header.protocol
    );
    ensure!(header.line_count > 0, "B3L1 contains no field lines");
    ensure!(header.point_count >= 2, "B3L1 contains too few points");
    ensure!(
        header.bounds.into_iter().all(f32::is_finite)
            && header.bounds[1] > header.bounds[0]
            && header.bounds[3] > header.bounds[2]
            && header.bounds[5] > header.bounds[4],
        "B3L1 contains invalid bounds"
    );
    let offset_count = header
        .line_count
        .checked_add(1)
        .context("B3L1 offset count overflow")?;
    let offset_bytes = offset_count
        .checked_mul(size_of::<u32>())
        .context("B3L1 offset buffer size overflow")?;
    let position_bytes = header
        .point_count
        .checked_mul(size_of::<Position3>())
        .context("B3L1 position buffer size overflow")?;
    let expected = 8_u64 + header_size as u64 + offset_bytes as u64 + position_bytes as u64;
    ensure!(
        file.metadata()?.len() == expected,
        "B3L1 payload size does not match its header"
    );
    let mut offsets = vec![0_u32; offset_count];
    file.read_exact(bytemuck::cast_slice_mut(&mut offsets))?;
    ensure!(
        offsets.first() == Some(&0)
            && offsets.last().copied() == Some(header.point_count as u32)
            && offsets.windows(2).all(|range| range[1] > range[0]),
        "B3L1 line offsets are invalid"
    );
    let mut positions = vec![Position3::zeroed(); header.point_count];
    file.read_exact(bytemuck::cast_slice_mut(&mut positions))?;
    ensure!(
        positions
            .iter()
            .all(|point| point.x.is_finite() && point.y.is_finite() && point.z.is_finite()),
        "B3L1 contains non-finite positions"
    );
    Ok(FieldLines3dData {
        header,
        offsets,
        positions,
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

    fn surface_header(mesh_included: bool) -> Surface3dHeader {
        Surface3dHeader {
            protocol: BRIDGE_PROTOCOL,
            source: "volume.plt".into(),
            title: "fixture".into(),
            dataset_title: "fixture".into(),
            section: "3d".into(),
            zone_name: "volume".into(),
            variable: "Rho".into(),
            canonical_name: "density".into(),
            unit: "amu/cm^3".into(),
            axis_labels: ["X".into(), "Y".into(), "Z".into()],
            vertex_count: 3,
            triangle_count: 1,
            mesh_id: "fedcba9876543210fedcba9876543210".into(),
            mesh_included,
            bounds: [0.0, 1.0, 0.0, 1.0, 0.0, 1.0],
            crop_bounds: None,
            value_range: [1.0, 3.0],
            volume_value_range: Some([1.0, 3.0]),
            layers: vec![SurfaceLayerHeader {
                kind: SurfaceLayerKind::Slice,
                layer_id: None,
                name: "X slice".into(),
                axis: Some("x".into()),
                position: Some(0.5),
                variable: "rho".into(),
                color_variable: None,
                isovalue: None,
                unit: String::new(),
                value_range: Some([1.0, 3.0]),
                volume_range: Some([1.0, 3.0]),
                index_start: 0,
                index_count: 3,
                source_triangles: 1,
                rendered_triangles: 1,
                inactive_reason: None,
            }],
            time: Some(1.0),
            dump: Some(1),
        }
    }

    fn write_surface_payload(
        file: &mut tempfile::NamedTempFile,
        header: &Surface3dHeader,
        positions: &[Position3],
        values: &[f32],
        indices: &[u32],
    ) {
        let encoded = serde_json::to_vec(header).unwrap();
        file.write_all(b"B3S2").unwrap();
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

    #[test]
    fn reads_3d_surface_and_reuses_matching_mesh() {
        let positions = [
            Position3 {
                x: 0.5,
                y: 0.0,
                z: 0.0,
            },
            Position3 {
                x: 0.5,
                y: 1.0,
                z: 0.0,
            },
            Position3 {
                x: 0.5,
                y: 0.0,
                z: 1.0,
            },
        ];
        let values = [1.0, 2.0, 3.0];
        let indices = [0_u32, 1, 2];
        let mut full = tempfile::NamedTempFile::new().unwrap();
        write_surface_payload(
            &mut full,
            &surface_header(true),
            &positions,
            &values,
            &indices,
        );
        let loaded = read_surface3d(full.path(), None).unwrap();
        assert_eq!(loaded.mesh.positions, positions);
        assert_eq!(loaded.values, values);

        let mut scalar = tempfile::NamedTempFile::new().unwrap();
        write_surface_payload(&mut scalar, &surface_header(false), &[], &values, &[]);
        let reused = read_surface3d(scalar.path(), Some(loaded.mesh.clone())).unwrap();
        assert!(Arc::ptr_eq(&loaded.mesh, &reused.mesh));
        assert_eq!(reused.values, values);
    }

    #[test]
    fn reads_inactive_empty_isosurface_layer() {
        let mut header = surface_header(true);
        header.vertex_count = 0;
        header.triangle_count = 0;
        header.mesh_id = "00000000000000000000000000000000".into();
        header.layers = vec![SurfaceLayerHeader {
            kind: SurfaceLayerKind::Isosurface,
            layer_id: Some(17),
            name: "Missing shell".into(),
            axis: None,
            position: None,
            variable: "density".into(),
            color_variable: None,
            isovalue: Some(99.0),
            unit: String::new(),
            value_range: None,
            volume_range: Some([1.0, 3.0]),
            index_start: 0,
            index_count: 0,
            source_triangles: 0,
            rendered_triangles: 0,
            inactive_reason: Some("outside range".into()),
        }];
        let mut file = tempfile::NamedTempFile::new().unwrap();
        write_surface_payload(&mut file, &header, &[], &[], &[]);
        let loaded = read_surface3d(file.path(), None).unwrap();
        assert!(loaded.mesh.positions.is_empty());
        assert_eq!(
            loaded.header.layers[0].inactive_reason.as_deref(),
            Some("outside range")
        );
    }

    #[test]
    fn rejects_overlapping_3d_layer_ranges() {
        let mut header = surface_header(true);
        header.layers.push(SurfaceLayerHeader {
            kind: SurfaceLayerKind::Isosurface,
            layer_id: Some(3),
            name: "Overlap".into(),
            axis: None,
            position: None,
            variable: "density".into(),
            color_variable: None,
            isovalue: Some(2.0),
            unit: String::new(),
            value_range: Some([1.0, 3.0]),
            volume_range: Some([1.0, 3.0]),
            index_start: 0,
            index_count: 3,
            source_triangles: 1,
            rendered_triangles: 1,
            inactive_reason: None,
        });
        let positions = [Position3::default(); 3];
        let mut file = tempfile::NamedTempFile::new().unwrap();
        write_surface_payload(&mut file, &header, &positions, &[1.0, 2.0, 3.0], &[0, 1, 2]);
        assert!(
            read_surface3d(file.path(), None)
                .unwrap_err()
                .to_string()
                .contains("layer metadata")
        );
    }

    #[test]
    fn reads_3d_field_line_payload_and_splits_lines() {
        let header = FieldLines3dHeader {
            protocol: BRIDGE_PROTOCOL,
            source: "volume.plt".into(),
            section: "3d".into(),
            zone_name: "volume".into(),
            components: ["Bx".into(), "By".into(), "Bz".into()],
            line_count: 2,
            point_count: 5,
            seed_count: 2,
            bounds: [-2.0, 2.0, -2.0, 2.0, -2.0, 2.0],
            planet_radius: 2.5,
        };
        let offsets = [0_u32, 2, 5];
        let positions = [
            Position3 {
                x: -1.0,
                y: 0.0,
                z: 0.0,
            },
            Position3 {
                x: 0.0,
                y: 0.0,
                z: 0.0,
            },
            Position3 {
                x: 0.0,
                y: -1.0,
                z: 0.0,
            },
            Position3 {
                x: 0.0,
                y: 0.0,
                z: 0.0,
            },
            Position3 {
                x: 0.0,
                y: 1.0,
                z: 0.0,
            },
        ];
        let encoded = serde_json::to_vec(&header).unwrap();
        let mut file = tempfile::NamedTempFile::new().unwrap();
        file.write_all(b"B3L1").unwrap();
        file.write_all(&(encoded.len() as u32).to_le_bytes())
            .unwrap();
        file.write_all(&encoded).unwrap();
        file.write_all(bytemuck::cast_slice(&offsets)).unwrap();
        file.write_all(bytemuck::cast_slice(&positions)).unwrap();
        file.flush().unwrap();

        let loaded = read_fieldlines3d(file.path()).unwrap();
        let lines: Vec<_> = loaded.lines().collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].len(), 2);
        assert_eq!(lines[1].len(), 3);
        assert_eq!(
            loaded.numeric_bytes(),
            offsets.len() * 4 + positions.len() * 12
        );
    }
}
