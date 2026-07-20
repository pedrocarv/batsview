use std::{
    sync::{
        Arc,
        mpsc::{self, Receiver, Sender},
    },
    thread,
};

use eframe::egui::{Pos2, Rect};

use crate::{
    camera3d::{Camera3d, Projection3d},
    protocol::{PlotData, Position3, Surface3dData, SurfaceLayerKind},
};

#[derive(Clone, Debug, PartialEq)]
pub struct ProbeHit {
    pub position: [f32; 3],
    pub value: f32,
    pub variable: String,
    pub unit: Option<String>,
    pub layer_id: Option<u64>,
    pub layer_name: String,
}

pub enum ProbeIndex {
    TwoD(ProbeIndex2d),
    ThreeD(ProbeIndex3d),
}

impl ProbeIndex {
    pub fn query_2d(&self, point: [f32; 2]) -> Option<ProbeHit> {
        match self {
            Self::TwoD(index) => index.query(point),
            Self::ThreeD(_) => None,
        }
    }

    pub fn query_3d(
        &self,
        ray: Ray3d,
        mut layer_visible: impl FnMut(Option<u64>) -> bool,
    ) -> Option<ProbeHit> {
        match self {
            Self::ThreeD(index) => index.query(ray, &mut layer_visible),
            Self::TwoD(_) => None,
        }
    }
}

enum BuildRequest {
    TwoD(u64, Arc<PlotData>),
    ThreeD(u64, Arc<Surface3dData>),
}

pub struct ProbeIndexResult {
    pub generation: u64,
    pub index: Arc<ProbeIndex>,
}

pub struct ProbeIndexer {
    request: Sender<BuildRequest>,
    result: Receiver<ProbeIndexResult>,
    next_generation: u64,
}

impl ProbeIndexer {
    pub fn new() -> Self {
        let (request_tx, request_rx) = mpsc::channel();
        let (result_tx, result_rx) = mpsc::channel();
        thread::Builder::new()
            .name("batsview-probe-index".to_owned())
            .spawn(move || probe_worker(request_rx, result_tx))
            .expect("starting probe index worker");
        Self {
            request: request_tx,
            result: result_rx,
            next_generation: 0,
        }
    }

    pub fn schedule_2d(&mut self, data: Arc<PlotData>) -> u64 {
        self.next_generation = self.next_generation.wrapping_add(1);
        let generation = self.next_generation;
        let _ = self.request.send(BuildRequest::TwoD(generation, data));
        generation
    }

    pub fn schedule_3d(&mut self, data: Arc<Surface3dData>) -> u64 {
        self.next_generation = self.next_generation.wrapping_add(1);
        let generation = self.next_generation;
        let _ = self.request.send(BuildRequest::ThreeD(generation, data));
        generation
    }

    pub fn latest(&mut self) -> Option<ProbeIndexResult> {
        let mut latest = None;
        while let Ok(result) = self.result.try_recv() {
            latest = Some(result);
        }
        latest
    }
}

fn probe_worker(request: Receiver<BuildRequest>, result: Sender<ProbeIndexResult>) {
    while let Ok(mut pending) = request.recv() {
        while let Ok(newer) = request.try_recv() {
            pending = newer;
        }
        let (generation, index) = match pending {
            BuildRequest::TwoD(generation, data) => {
                (generation, ProbeIndex::TwoD(ProbeIndex2d::build(data)))
            }
            BuildRequest::ThreeD(generation, data) => {
                (generation, ProbeIndex::ThreeD(ProbeIndex3d::build(data)))
            }
        };
        let _ = result.send(ProbeIndexResult {
            generation,
            index: Arc::new(index),
        });
    }
}

pub struct ProbeIndex2d {
    data: Arc<PlotData>,
    bounds: [f32; 4],
    columns: usize,
    rows: usize,
    cells: Vec<Vec<u32>>,
}

impl ProbeIndex2d {
    fn build(data: Arc<PlotData>) -> Self {
        let bounds = data.header.bounds;
        let target = ((data.mesh.indices.len() / 3).max(1) as f64).sqrt() as usize;
        let columns = target.clamp(8, 160);
        let rows = columns;
        let mut cells = vec![Vec::new(); columns * rows];
        for (triangle, indices) in data.mesh.indices.chunks_exact(3).enumerate() {
            let points = [
                data.mesh.positions[indices[0] as usize],
                data.mesh.positions[indices[1] as usize],
                data.mesh.positions[indices[2] as usize],
            ];
            let min_x = points
                .iter()
                .map(|point| point.x)
                .fold(f32::INFINITY, f32::min);
            let max_x = points
                .iter()
                .map(|point| point.x)
                .fold(f32::NEG_INFINITY, f32::max);
            let min_y = points
                .iter()
                .map(|point| point.y)
                .fold(f32::INFINITY, f32::min);
            let max_y = points
                .iter()
                .map(|point| point.y)
                .fold(f32::NEG_INFINITY, f32::max);
            let [x0, x1] = grid_range(min_x, max_x, bounds[0], bounds[1], columns);
            let [y0, y1] = grid_range(min_y, max_y, bounds[2], bounds[3], rows);
            for y in y0..=y1 {
                for x in x0..=x1 {
                    cells[y * columns + x].push(triangle as u32);
                }
            }
        }
        Self {
            data,
            bounds,
            columns,
            rows,
            cells,
        }
    }

    fn query(&self, point: [f32; 2]) -> Option<ProbeHit> {
        if point[0] < self.bounds[0]
            || point[0] > self.bounds[1]
            || point[1] < self.bounds[2]
            || point[1] > self.bounds[3]
        {
            return None;
        }
        let x = grid_coordinate(point[0], self.bounds[0], self.bounds[1], self.columns);
        let y = grid_coordinate(point[1], self.bounds[2], self.bounds[3], self.rows);
        for &triangle in &self.cells[y * self.columns + x] {
            let offset = triangle as usize * 3;
            let indices = &self.data.mesh.indices[offset..offset + 3];
            let a = self.data.mesh.positions[indices[0] as usize];
            let b = self.data.mesh.positions[indices[1] as usize];
            let c = self.data.mesh.positions[indices[2] as usize];
            let Some(weights) = barycentric_2d(point, [a.x, a.y], [b.x, b.y], [c.x, c.y]) else {
                continue;
            };
            if weights.iter().any(|weight| *weight < -1.0e-5) {
                continue;
            }
            let values = [
                self.data.values[indices[0] as usize],
                self.data.values[indices[1] as usize],
                self.data.values[indices[2] as usize],
            ];
            let value = weights[0] * values[0] + weights[1] * values[1] + weights[2] * values[2];
            if !value.is_finite() {
                continue;
            }
            return Some(ProbeHit {
                position: [point[0], point[1], 0.0],
                value,
                variable: self.data.header.variable.clone(),
                unit: self.data.header.unit.clone(),
                layer_id: None,
                layer_name: "2D plot".to_owned(),
            });
        }
        None
    }
}

fn grid_range(low: f32, high: f32, bound_low: f32, bound_high: f32, count: usize) -> [usize; 2] {
    [
        grid_coordinate(low, bound_low, bound_high, count),
        grid_coordinate(high, bound_low, bound_high, count),
    ]
}

fn grid_coordinate(value: f32, low: f32, high: f32, count: usize) -> usize {
    (((value - low) / (high - low).max(f32::EPSILON) * count as f32).floor() as isize)
        .clamp(0, count as isize - 1) as usize
}

fn barycentric_2d(point: [f32; 2], a: [f32; 2], b: [f32; 2], c: [f32; 2]) -> Option<[f32; 3]> {
    let denominator = (b[1] - c[1]) * (a[0] - c[0]) + (c[0] - b[0]) * (a[1] - c[1]);
    if denominator.abs() <= 1.0e-20 {
        return None;
    }
    let u = ((b[1] - c[1]) * (point[0] - c[0]) + (c[0] - b[0]) * (point[1] - c[1])) / denominator;
    let v = ((c[1] - a[1]) * (point[0] - c[0]) + (a[0] - c[0]) * (point[1] - c[1])) / denominator;
    Some([u, v, 1.0 - u - v])
}

#[derive(Clone, Copy, Debug)]
pub struct Ray3d {
    pub origin: [f32; 3],
    pub direction: [f32; 3],
}

pub fn camera_ray(camera: Camera3d, rect: Rect, pointer: Pos2) -> Ray3d {
    let x = (2.0 * (pointer.x - rect.left()) / rect.width().max(1.0) - 1.0).clamp(-1.0, 1.0);
    let y = (1.0 - 2.0 * (pointer.y - rect.top()) / rect.height().max(1.0)).clamp(-1.0, 1.0);
    let direction = normalize(camera.direction);
    let up = normalize(camera.up);
    let right = normalize(cross(up, direction));
    let aspect = rect.width() / rect.height().max(1.0);
    let eye = add(camera.target, mul(direction, camera.distance));
    match camera.projection {
        Projection3d::Perspective => {
            let half_height = camera.distance
                * (0.5 * camera.field_of_view_degrees.clamp(10.0, 100.0).to_radians()).tan();
            let target = add(
                camera.target,
                add(
                    mul(right, x * half_height * aspect),
                    mul(up, y * half_height),
                ),
            );
            Ray3d {
                origin: eye,
                direction: normalize(sub(target, eye)),
            }
        }
        Projection3d::Orthographic => Ray3d {
            origin: add(
                eye,
                add(
                    mul(right, x * camera.orthographic_scale * aspect),
                    mul(up, y * camera.orthographic_scale),
                ),
            ),
            direction: mul(direction, -1.0),
        },
    }
}

#[derive(Clone)]
struct LayerMeta {
    id: Option<u64>,
    name: String,
    variable: String,
    unit: Option<String>,
}

#[derive(Clone)]
struct Triangle3d {
    indices: [u32; 3],
    layer: usize,
}

#[derive(Clone, Copy, Default)]
struct BvhNode {
    bounds: [f32; 6],
    left: Option<u32>,
    right: Option<u32>,
    start: u32,
    count: u32,
}

pub struct ProbeIndex3d {
    data: Arc<Surface3dData>,
    layers: Vec<LayerMeta>,
    triangles: Vec<Triangle3d>,
    nodes: Vec<BvhNode>,
}

impl ProbeIndex3d {
    fn build(data: Arc<Surface3dData>) -> Self {
        let mut layers = Vec::new();
        let mut triangles = Vec::new();
        for header in &data.header.layers {
            if header.index_count == 0 || header.inactive_reason.is_some() {
                continue;
            }
            let layer = layers.len();
            layers.push(LayerMeta {
                id: header.layer_id,
                name: if header.name.is_empty() {
                    match header.kind {
                        SurfaceLayerKind::Slice => format!(
                            "{} slice",
                            header.axis.as_deref().unwrap_or("3D").to_ascii_uppercase()
                        ),
                        SurfaceLayerKind::Isosurface => "Isosurface".to_owned(),
                    }
                } else {
                    header.name.clone()
                },
                variable: header
                    .color_variable
                    .clone()
                    .unwrap_or_else(|| header.variable.clone()),
                unit: (!header.unit.is_empty()).then(|| header.unit.clone()),
            });
            let start = header.index_start as usize;
            let end = start + header.index_count as usize;
            for indices in data.mesh.indices[start..end].chunks_exact(3) {
                triangles.push(Triangle3d {
                    indices: [indices[0], indices[1], indices[2]],
                    layer,
                });
            }
        }
        let mut nodes = Vec::new();
        if !triangles.is_empty() {
            let length = triangles.len();
            build_bvh(&mut triangles, 0, length, &data.mesh.positions, &mut nodes);
        }
        Self {
            data,
            layers,
            triangles,
            nodes,
        }
    }

    fn query(
        &self,
        ray: Ray3d,
        layer_visible: &mut impl FnMut(Option<u64>) -> bool,
    ) -> Option<ProbeHit> {
        if self.nodes.is_empty() {
            return None;
        }
        let mut nearest = f32::INFINITY;
        let mut result = None;
        let mut stack = vec![0_u32];
        while let Some(index) = stack.pop() {
            let node = self.nodes[index as usize];
            if !ray_aabb(ray, node.bounds, nearest) {
                continue;
            }
            if let (Some(left), Some(right)) = (node.left, node.right) {
                stack.push(right);
                stack.push(left);
                continue;
            }
            for triangle in &self.triangles[node.start as usize..(node.start + node.count) as usize]
            {
                let layer = &self.layers[triangle.layer];
                if !layer_visible(layer.id) {
                    continue;
                }
                let [a, b, c] = triangle
                    .indices
                    .map(|vertex| position_array(self.data.mesh.positions[vertex as usize]));
                let Some((distance, u, v)) = ray_triangle(ray, a, b, c) else {
                    continue;
                };
                if distance >= nearest {
                    continue;
                }
                let values = triangle
                    .indices
                    .map(|vertex| self.data.values[vertex as usize]);
                let w = 1.0 - u - v;
                let value = values[0] * w + values[1] * u + values[2] * v;
                if !value.is_finite() {
                    continue;
                }
                nearest = distance;
                result = Some(ProbeHit {
                    position: add(ray.origin, mul(ray.direction, distance)),
                    value,
                    variable: layer.variable.clone(),
                    unit: layer.unit.clone(),
                    layer_id: layer.id,
                    layer_name: layer.name.clone(),
                });
            }
        }
        result
    }
}

fn build_bvh(
    triangles: &mut [Triangle3d],
    start: usize,
    end: usize,
    positions: &[Position3],
    nodes: &mut Vec<BvhNode>,
) -> u32 {
    let index = nodes.len() as u32;
    nodes.push(BvhNode::default());
    let bounds = triangle_bounds(&triangles[start..end], positions);
    let count = end - start;
    if count <= 8 {
        nodes[index as usize] = BvhNode {
            bounds,
            start: start as u32,
            count: count as u32,
            ..BvhNode::default()
        };
        return index;
    }
    let spans = [
        bounds[1] - bounds[0],
        bounds[3] - bounds[2],
        bounds[5] - bounds[4],
    ];
    let axis = if spans[1] > spans[0] && spans[1] >= spans[2] {
        1
    } else if spans[2] > spans[0] {
        2
    } else {
        0
    };
    triangles[start..end].sort_unstable_by(|left, right| {
        triangle_centroid(left, positions)[axis]
            .total_cmp(&triangle_centroid(right, positions)[axis])
    });
    let middle = start + count / 2;
    let left = build_bvh(triangles, start, middle, positions, nodes);
    let right = build_bvh(triangles, middle, end, positions, nodes);
    nodes[index as usize] = BvhNode {
        bounds,
        left: Some(left),
        right: Some(right),
        ..BvhNode::default()
    };
    index
}

fn triangle_bounds(triangles: &[Triangle3d], positions: &[Position3]) -> [f32; 6] {
    let mut bounds = [
        f32::INFINITY,
        f32::NEG_INFINITY,
        f32::INFINITY,
        f32::NEG_INFINITY,
        f32::INFINITY,
        f32::NEG_INFINITY,
    ];
    for triangle in triangles {
        for index in triangle.indices {
            let point = position_array(positions[index as usize]);
            for axis in 0..3 {
                bounds[axis * 2] = bounds[axis * 2].min(point[axis]);
                bounds[axis * 2 + 1] = bounds[axis * 2 + 1].max(point[axis]);
            }
        }
    }
    bounds
}

fn triangle_centroid(triangle: &Triangle3d, positions: &[Position3]) -> [f32; 3] {
    let points = triangle
        .indices
        .map(|index| position_array(positions[index as usize]));
    mul(add(add(points[0], points[1]), points[2]), 1.0 / 3.0)
}

fn ray_aabb(ray: Ray3d, bounds: [f32; 6], maximum: f32) -> bool {
    let mut low: f32 = 0.0;
    let mut high: f32 = maximum;
    for axis in 0..3 {
        if ray.direction[axis].abs() < 1.0e-12 {
            if ray.origin[axis] < bounds[axis * 2] || ray.origin[axis] > bounds[axis * 2 + 1] {
                return false;
            }
            continue;
        }
        let inverse = 1.0 / ray.direction[axis];
        let mut a = (bounds[axis * 2] - ray.origin[axis]) * inverse;
        let mut b = (bounds[axis * 2 + 1] - ray.origin[axis]) * inverse;
        if a > b {
            std::mem::swap(&mut a, &mut b);
        }
        low = low.max(a);
        high = high.min(b);
        if high < low {
            return false;
        }
    }
    true
}

fn ray_triangle(ray: Ray3d, a: [f32; 3], b: [f32; 3], c: [f32; 3]) -> Option<(f32, f32, f32)> {
    let edge1 = sub(b, a);
    let edge2 = sub(c, a);
    let p = cross(ray.direction, edge2);
    let determinant = dot(edge1, p);
    if determinant.abs() < 1.0e-9 {
        return None;
    }
    let inverse = 1.0 / determinant;
    let offset = sub(ray.origin, a);
    let u = dot(offset, p) * inverse;
    if !(0.0..=1.0).contains(&u) {
        return None;
    }
    let q = cross(offset, edge1);
    let v = dot(ray.direction, q) * inverse;
    if v < 0.0 || u + v > 1.0 {
        return None;
    }
    let distance = dot(edge2, q) * inverse;
    (distance > 1.0e-7).then_some((distance, u, v))
}

fn position_array(position: Position3) -> [f32; 3] {
    [position.x, position.y, position.z]
}

fn add(left: [f32; 3], right: [f32; 3]) -> [f32; 3] {
    [left[0] + right[0], left[1] + right[1], left[2] + right[2]]
}

fn sub(left: [f32; 3], right: [f32; 3]) -> [f32; 3] {
    [left[0] - right[0], left[1] - right[1], left[2] - right[2]]
}

fn mul(value: [f32; 3], scale: f32) -> [f32; 3] {
    [value[0] * scale, value[1] * scale, value[2] * scale]
}

fn dot(left: [f32; 3], right: [f32; 3]) -> f32 {
    left[0] * right[0] + left[1] * right[1] + left[2] * right[2]
}

fn cross(left: [f32; 3], right: [f32; 3]) -> [f32; 3] {
    [
        left[1] * right[2] - left[2] * right[1],
        left[2] * right[0] - left[0] * right[2],
        left[0] * right[1] - left[1] * right[0],
    ]
}

fn normalize(value: [f32; 3]) -> [f32; 3] {
    let length = dot(value, value).sqrt().max(1.0e-20);
    mul(value, 1.0 / length)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn barycentric_interpolation_weights_sum_to_one() {
        let weights = barycentric_2d([0.25, 0.25], [0.0, 0.0], [1.0, 0.0], [0.0, 1.0]).unwrap();
        assert!((weights.iter().sum::<f32>() - 1.0).abs() < 1.0e-6);
        assert!(weights.into_iter().all(|weight| weight >= 0.0));
    }

    #[test]
    fn perspective_center_ray_points_toward_target() {
        let camera = Camera3d::default();
        let rect = Rect::from_min_size(Pos2::ZERO, eframe::egui::vec2(400.0, 200.0));
        let ray = camera_ray(camera, rect, rect.center());
        assert!(dot(ray.direction, camera.direction) < -0.999);
    }

    #[test]
    fn ray_selects_triangle_in_front_of_camera() {
        let ray = Ray3d {
            origin: [0.2, 0.2, 1.0],
            direction: [0.0, 0.0, -1.0],
        };
        let hit = ray_triangle(ray, [0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]).unwrap();
        assert!((hit.0 - 1.0).abs() < 1.0e-6);
    }
}
