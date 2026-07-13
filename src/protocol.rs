use std::{fs::File, io::Read, path::Path};

use anyhow::{Context, Result, bail};
use bytemuck::{Pod, Zeroable};
use serde::Deserialize;

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

#[derive(Clone, Debug, Deserialize)]
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
    pub bounds: [f32; 4],
    pub value_range: [f32; 2],
    pub positive_range: Option<[f32; 2]>,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct Vertex {
    pub x: f32,
    pub y: f32,
    pub value: f32,
}

#[derive(Clone, Debug)]
pub struct PlotData {
    pub header: PlotHeader,
    pub vertices: Vec<Vertex>,
    pub indices: Vec<u32>,
}

pub fn read_plot(path: &Path) -> Result<PlotData> {
    let mut file = File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let mut prefix = [0_u8; 8];
    file.read_exact(&mut prefix)?;
    if &prefix[..4] != b"BPV1" {
        bail!("unsupported plot exchange format (expected BPV1)");
    }
    let header_size = u32::from_le_bytes(prefix[4..8].try_into().unwrap()) as usize;
    if header_size > 16 * 1024 * 1024 {
        bail!("invalid BPV1 header size: {header_size}");
    }
    let mut header_bytes = vec![0; header_size];
    file.read_exact(&mut header_bytes)?;
    let header: PlotHeader = serde_json::from_slice(&header_bytes)?;
    if header.protocol != 1 {
        bail!("unsupported bridge protocol {}", header.protocol);
    }

    let vertex_bytes = header
        .point_count
        .checked_mul(size_of::<Vertex>())
        .context("vertex buffer size overflow")?;
    let index_count = header
        .triangle_count
        .checked_mul(3)
        .context("index count overflow")?;
    let index_bytes = index_count
        .checked_mul(size_of::<u32>())
        .context("index buffer size overflow")?;
    let mut vertices = vec![Vertex::zeroed(); header.point_count];
    let mut indices = vec![0_u32; index_count];
    file.read_exact(bytemuck::cast_slice_mut(&mut vertices))?;
    file.read_exact(bytemuck::cast_slice_mut(&mut indices))?;

    let expected = 8_u64 + header_size as u64 + vertex_bytes as u64 + index_bytes as u64;
    if file.metadata()?.len() != expected {
        bail!("BPV1 payload size does not match its header");
    }
    if indices
        .iter()
        .any(|&index| index as usize >= vertices.len())
    {
        bail!("BPV1 mesh contains an out-of-range index");
    }
    Ok(PlotData {
        header,
        vertices,
        indices,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn rejects_wrong_magic() {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        file.write_all(b"NOPE\0\0\0\0").unwrap();
        assert!(
            read_plot(file.path())
                .unwrap_err()
                .to_string()
                .contains("BPV1")
        );
    }
}
