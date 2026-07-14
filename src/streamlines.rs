use std::sync::Arc;

use anyhow::{Result, bail, ensure};
use eframe::egui::{self, Stroke};

use crate::{
    protocol::PlotData,
    scene::{DataPoint, StreamlineDirection, StreamlineSettings},
};

#[derive(Clone, Debug)]
pub struct StreamlineOverlay {
    pub path: String,
    pub section: Option<String>,
    pub horizontal_component: String,
    pub vertical_component: String,
    pub lines: Vec<Vec<DataPoint>>,
    pub settings: StreamlineSettings,
}

#[derive(Debug)]
pub struct VectorField {
    horizontal: Arc<PlotData>,
    vertical: Arc<PlotData>,
    locator: TriangleLocator,
}

impl VectorField {
    pub fn new(horizontal: Arc<PlotData>, vertical: Arc<PlotData>) -> Result<Self> {
        ensure!(
            horizontal.mesh.id == vertical.mesh.id,
            "vector components use different meshes"
        );
        ensure!(
            horizontal.values.len() == horizontal.mesh.positions.len()
                && vertical.values.len() == horizontal.mesh.positions.len(),
            "vector component length does not match the mesh"
        );
        let locator = TriangleLocator::new(&horizontal)?;
        Ok(Self {
            horizontal,
            vertical,
            locator,
        })
    }

    pub fn integrate(&self, settings: &StreamlineSettings) -> Vec<Vec<DataPoint>> {
        if !settings.enabled || settings.seeds.is_empty() {
            return Vec::new();
        }
        let diagonal = ((self.locator.bounds[1] - self.locator.bounds[0]).powi(2)
            + (self.locator.bounds[3] - self.locator.bounds[2]).powi(2))
        .sqrt();
        let step = diagonal * f64::from(settings.step_fraction.clamp(0.000_01, 0.05));
        if !step.is_finite() || step <= 0.0 {
            return Vec::new();
        }
        let max_steps = settings.max_steps.clamp(10, 5_000) as usize;
        settings
            .seeds
            .iter()
            .filter_map(|seed| {
                let line = match settings.direction {
                    StreamlineDirection::Forward => self.trace(*seed, 1.0, step, max_steps),
                    StreamlineDirection::Backward => {
                        let mut line = self.trace(*seed, -1.0, step, max_steps);
                        line.reverse();
                        line
                    }
                    StreamlineDirection::Both => {
                        let mut backward = self.trace(*seed, -1.0, step, max_steps);
                        backward.reverse();
                        if !is_closed(&backward, *seed, step) {
                            let forward = self.trace(*seed, 1.0, step, max_steps);
                            backward.extend(forward.into_iter().skip(1));
                        }
                        backward
                    }
                };
                (line.len() >= 2).then_some(line)
            })
            .collect()
    }

    fn trace(&self, seed: DataPoint, sign: f64, step: f64, max_steps: usize) -> Vec<DataPoint> {
        if self.sample(seed).is_none() {
            return Vec::new();
        }
        let mut points = Vec::with_capacity(max_steps.min(2_048) + 1);
        points.push(seed);
        let mut current = seed;
        for index in 0..max_steps {
            let Some(next) = self.advance(current, sign, step) else {
                break;
            };
            let distance = ((next.x - current.x).powi(2) + (next.y - current.y).powi(2)).sqrt();
            if !distance.is_finite() || distance < step * 1.0e-5 {
                break;
            }
            points.push(next);
            current = next;
            if index > 24 {
                let seed_distance =
                    ((current.x - seed.x).powi(2) + (current.y - seed.y).powi(2)).sqrt();
                if seed_distance < step * 0.7 {
                    break;
                }
            }
        }
        points
    }

    fn advance(&self, point: DataPoint, sign: f64, step: f64) -> Option<DataPoint> {
        let k1 = self.unit_direction(point, sign)?;
        let k2 = self.unit_direction(offset(point, k1, 0.5 * step), sign)?;
        let k3 = self.unit_direction(offset(point, k2, 0.5 * step), sign)?;
        let k4 = self.unit_direction(offset(point, k3, step), sign)?;
        let dx = step * (k1.0 + 2.0 * k2.0 + 2.0 * k3.0 + k4.0) / 6.0;
        let dy = step * (k1.1 + 2.0 * k2.1 + 2.0 * k3.1 + k4.1) / 6.0;
        let next = DataPoint::new(point.x + dx, point.y + dy);
        self.sample(next).map(|_| next)
    }

    fn unit_direction(&self, point: DataPoint, sign: f64) -> Option<(f64, f64)> {
        let (horizontal, vertical) = self.sample(point)?;
        let magnitude = horizontal.hypot(vertical);
        if !magnitude.is_finite() || magnitude <= 1.0e-30 {
            return None;
        }
        Some((sign * horizontal / magnitude, sign * vertical / magnitude))
    }

    fn sample(&self, point: DataPoint) -> Option<(f64, f64)> {
        let candidates = self.locator.candidates(point)?;
        let positions = &self.horizontal.mesh.positions;
        let indices = &self.horizontal.mesh.indices;
        for &triangle in candidates {
            let offset = triangle as usize * 3;
            let [a, b, c] = [
                indices[offset] as usize,
                indices[offset + 1] as usize,
                indices[offset + 2] as usize,
            ];
            let pa = positions[a];
            let pb = positions[b];
            let pc = positions[c];
            let denominator = f64::from(pb.y - pc.y) * f64::from(pa.x - pc.x)
                + f64::from(pc.x - pb.x) * f64::from(pa.y - pc.y);
            if denominator.abs() <= 1.0e-30 {
                continue;
            }
            let alpha = (f64::from(pb.y - pc.y) * (point.x - f64::from(pc.x))
                + f64::from(pc.x - pb.x) * (point.y - f64::from(pc.y)))
                / denominator;
            let beta = (f64::from(pc.y - pa.y) * (point.x - f64::from(pc.x))
                + f64::from(pa.x - pc.x) * (point.y - f64::from(pc.y)))
                / denominator;
            let gamma = 1.0 - alpha - beta;
            if alpha >= -1.0e-7 && beta >= -1.0e-7 && gamma >= -1.0e-7 {
                let horizontal = alpha * f64::from(self.horizontal.values[a])
                    + beta * f64::from(self.horizontal.values[b])
                    + gamma * f64::from(self.horizontal.values[c]);
                let vertical = alpha * f64::from(self.vertical.values[a])
                    + beta * f64::from(self.vertical.values[b])
                    + gamma * f64::from(self.vertical.values[c]);
                return (horizontal.is_finite() && vertical.is_finite())
                    .then_some((horizontal, vertical));
            }
        }
        None
    }
}

#[derive(Debug)]
struct TriangleLocator {
    bounds: [f64; 4],
    columns: usize,
    rows: usize,
    bins: Vec<Vec<u32>>,
}

impl TriangleLocator {
    fn new(plot: &PlotData) -> Result<Self> {
        let bounds = plot.header.bounds.map(f64::from);
        ensure!(
            bounds.into_iter().all(f64::is_finite)
                && bounds[1] > bounds[0]
                && bounds[3] > bounds[2],
            "vector mesh has invalid bounds"
        );
        let triangle_count = plot.mesh.indices.len() / 3;
        if triangle_count == 0 {
            bail!("vector mesh contains no triangles");
        }
        let resolution = ((triangle_count as f64).sqrt() * 0.65).round() as usize;
        let columns = resolution.clamp(16, 192);
        let rows = resolution.clamp(16, 192);
        let mut locator = Self {
            bounds,
            columns,
            rows,
            bins: vec![Vec::new(); columns * rows],
        };
        for (triangle, indices) in plot.mesh.indices.chunks_exact(3).enumerate() {
            let points = [
                plot.mesh.positions[indices[0] as usize],
                plot.mesh.positions[indices[1] as usize],
                plot.mesh.positions[indices[2] as usize],
            ];
            let minimum = DataPoint::new(
                points
                    .iter()
                    .map(|point| f64::from(point.x))
                    .fold(f64::INFINITY, f64::min),
                points
                    .iter()
                    .map(|point| f64::from(point.y))
                    .fold(f64::INFINITY, f64::min),
            );
            let maximum = DataPoint::new(
                points
                    .iter()
                    .map(|point| f64::from(point.x))
                    .fold(f64::NEG_INFINITY, f64::max),
                points
                    .iter()
                    .map(|point| f64::from(point.y))
                    .fold(f64::NEG_INFINITY, f64::max),
            );
            let Some((left, bottom)) = locator.cell(minimum) else {
                continue;
            };
            let Some((right, top)) = locator.cell(maximum) else {
                continue;
            };
            for row in bottom.min(top)..=bottom.max(top) {
                for column in left.min(right)..=left.max(right) {
                    locator.bins[row * columns + column].push(triangle as u32);
                }
            }
        }
        Ok(locator)
    }

    fn candidates(&self, point: DataPoint) -> Option<&[u32]> {
        let (column, row) = self.cell(point)?;
        Some(&self.bins[row * self.columns + column])
    }

    fn cell(&self, point: DataPoint) -> Option<(usize, usize)> {
        if point.x < self.bounds[0]
            || point.x > self.bounds[1]
            || point.y < self.bounds[2]
            || point.y > self.bounds[3]
        {
            return None;
        }
        let x = ((point.x - self.bounds[0]) / (self.bounds[1] - self.bounds[0])
            * self.columns as f64)
            .floor() as usize;
        let y = ((point.y - self.bounds[2]) / (self.bounds[3] - self.bounds[2]) * self.rows as f64)
            .floor() as usize;
        Some((x.min(self.columns - 1), y.min(self.rows - 1)))
    }
}

fn offset(point: DataPoint, vector: (f64, f64), scale: f64) -> DataPoint {
    DataPoint::new(point.x + vector.0 * scale, point.y + vector.1 * scale)
}

fn is_closed(points: &[DataPoint], seed: DataPoint, step: f64) -> bool {
    points.len() > 25
        && points.first().is_some_and(|point| {
            ((point.x - seed.x).powi(2) + (point.y - seed.y).powi(2)).sqrt() < step * 0.8
        })
}

pub fn screen_to_data(point: egui::Pos2, rect: egui::Rect, bounds: [f32; 4]) -> DataPoint {
    let x = bounds[0] + (point.x - rect.left()) / rect.width() * (bounds[1] - bounds[0]);
    let y = bounds[3] - (point.y - rect.top()) / rect.height() * (bounds[3] - bounds[2]);
    DataPoint::new(f64::from(x), f64::from(y))
}

fn data_to_screen(point: DataPoint, rect: egui::Rect, bounds: [f32; 4]) -> egui::Pos2 {
    let x = (point.x as f32 - bounds[0]) / (bounds[1] - bounds[0]);
    let y = (point.y as f32 - bounds[2]) / (bounds[3] - bounds[2]);
    egui::pos2(
        rect.left() + x * rect.width(),
        rect.bottom() - y * rect.height(),
    )
}

pub fn paint_streamlines(
    ui: &egui::Ui,
    plot_rect: egui::Rect,
    bounds: [f32; 4],
    overlay: &StreamlineOverlay,
) {
    let painter = ui.painter().with_clip_rect(plot_rect);
    let color = overlay.settings.color.to_egui();
    let stroke = Stroke::new(overlay.settings.width.clamp(0.25, 12.0), color);
    for line in &overlay.lines {
        let points: Vec<_> = line
            .iter()
            .map(|point| data_to_screen(*point, plot_rect, bounds))
            .collect();
        if points.len() < 2 {
            continue;
        }
        painter.add(egui::Shape::line(points.clone(), stroke));
        if overlay.settings.arrows {
            paint_arrow(&painter, &points, stroke, overlay.settings.arrow_size);
        }
    }
}

fn paint_arrow(painter: &egui::Painter, points: &[egui::Pos2], stroke: Stroke, size: f32) {
    let total: f32 = points
        .windows(2)
        .map(|pair| pair[0].distance(pair[1]))
        .sum();
    if total < 12.0 {
        return;
    }
    let target = total * 0.55;
    let mut traversed = 0.0;
    for pair in points.windows(2) {
        let segment = pair[0].distance(pair[1]);
        if segment > 0.0 && traversed + segment >= target {
            let direction = (pair[1] - pair[0]).normalized();
            let tip = pair[0] + direction * (target - traversed);
            let normal = egui::vec2(-direction.y, direction.x);
            let size = size.clamp(3.0, 24.0);
            painter.line_segment([tip, tip - direction * size + normal * size * 0.45], stroke);
            painter.line_segment([tip, tip - direction * size - normal * size * 0.45], stroke);
            break;
        }
        traversed += segment;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{BRIDGE_PROTOCOL, MeshData, PlotHeader, Position};

    fn component(mesh_id: &str, values: [f32; 4]) -> Arc<PlotData> {
        Arc::new(PlotData {
            header: PlotHeader {
                protocol: BRIDGE_PROTOCOL,
                path: "test.plt".into(),
                title: "test".into(),
                section: Some("z=0".into()),
                zone: "cut".into(),
                variable: "component".into(),
                source_variable: "component".into(),
                unit: None,
                x_label: "X".into(),
                y_label: "Y".into(),
                point_count: 4,
                triangle_count: 2,
                mesh_id: mesh_id.into(),
                mesh_included: true,
                bounds: [0.0, 1.0, 0.0, 1.0],
                value_range: [0.0, 1.0],
                positive_range: Some([1.0, 1.0]),
            },
            mesh: Arc::new(MeshData {
                id: mesh_id.into(),
                positions: vec![
                    Position { x: 0.0, y: 0.0 },
                    Position { x: 1.0, y: 0.0 },
                    Position { x: 1.0, y: 1.0 },
                    Position { x: 0.0, y: 1.0 },
                ],
                indices: vec![0, 1, 2, 0, 2, 3],
            }),
            values: values.to_vec(),
        })
    }

    #[test]
    fn constant_horizontal_field_integrates_from_seed() {
        let horizontal = component("00000000000000000000000000000001", [1.0; 4]);
        let mut vertical = component("00000000000000000000000000000001", [0.0; 4]);
        Arc::get_mut(&mut vertical).unwrap().mesh = horizontal.mesh.clone();
        let field = VectorField::new(horizontal, vertical).unwrap();
        let settings = StreamlineSettings {
            enabled: true,
            seeds: vec![DataPoint::new(0.5, 0.5)],
            step_fraction: 0.02,
            max_steps: 200,
            ..StreamlineSettings::default()
        };
        let lines = field.integrate(&settings);
        assert_eq!(lines.len(), 1);
        let line = &lines[0];
        assert!(line.first().unwrap().x < 0.05);
        assert!(line.last().unwrap().x > 0.95);
        assert!(line.iter().all(|point| (point.y - 0.5).abs() < 1.0e-6));
    }

    #[test]
    fn mismatched_component_meshes_are_rejected() {
        let first = component("00000000000000000000000000000001", [1.0; 4]);
        let second = component("00000000000000000000000000000002", [0.0; 4]);
        assert!(VectorField::new(first, second).is_err());
    }

    #[test]
    fn coordinate_transform_respects_plot_bounds() {
        let rect = egui::Rect::from_min_size(egui::pos2(10.0, 20.0), egui::vec2(200.0, 100.0));
        let point = screen_to_data(rect.center(), rect, [-2.0, 2.0, -1.0, 3.0]);
        assert!((point.x - 0.0).abs() < 1.0e-6);
        assert!((point.y - 1.0).abs() < 1.0e-6);
    }
}
