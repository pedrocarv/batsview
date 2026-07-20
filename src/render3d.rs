use std::sync::{Arc, Mutex};

use bytemuck::{Pod, Zeroable};
use eframe::{
    egui::{self, Color32, Pos2, Stroke},
    egui_wgpu::{self, wgpu},
};
use wgpu::util::DeviceExt;

use crate::{
    camera3d::{Camera3d, project_point},
    protocol::{FieldLines3dData, Surface3dData, SurfaceLayerHeader, SurfaceLayerKind},
    scene::{AppearanceSettings, ColorMode, Colormap, FieldLine3dSettings, RgbaColor, Scale},
};

const UNIFORM_STRIDE: u64 = 256;
const MAX_RENDER_LAYERS: usize = 16;

#[derive(Clone, Debug)]
pub struct Display3d {
    pub scale: Scale,
    pub limits: [f32; 2],
    pub positive_range: [f32; 2],
    pub colormap: Colormap,
    pub reversed: bool,
    pub color_mode: ColorMode,
    pub opacity: f32,
    pub show_axes: bool,
    pub show_box: bool,
    pub show_reference_sphere: bool,
    pub reference_sphere_radius: f32,
}

impl Default for Display3d {
    fn default() -> Self {
        Self {
            scale: Scale::Linear,
            limits: [0.0, 1.0],
            positive_range: [f32::NAN; 2],
            colormap: Colormap::Viridis,
            reversed: false,
            color_mode: ColorMode::Continuous,
            opacity: 0.94,
            show_axes: true,
            show_box: true,
            show_reference_sphere: true,
            reference_sphere_radius: 2.5,
        }
    }
}

#[derive(Default)]
pub struct SharedScene3d {
    pub generation: u64,
    pub data: Option<Arc<Surface3dData>>,
    pub display: Display3d,
    pub layer_styles: Vec<LayerDisplay3d>,
    pub camera: Camera3d,
}

#[derive(Clone, Debug)]
pub struct LayerDisplay3d {
    pub layer_id: u64,
    pub visible: bool,
    pub opacity: f32,
    pub solid_color: Option<RgbaColor>,
    pub appearance: AppearanceSettings,
    pub order: u32,
}

impl SharedScene3d {
    pub fn clear_data(&mut self) {
        self.data = None;
        self.generation = self.generation.wrapping_add(1);
    }

    pub fn set_data(&mut self, data: Arc<Surface3dData>) {
        let preserve_camera = self.data.as_ref().is_some_and(|current| {
            current
                .header
                .active_bounds()
                .into_iter()
                .zip(data.header.active_bounds())
                .all(|(left, right)| {
                    let scale = left.abs().max(right.abs()).max(1.0);
                    (left - right).abs() <= scale * 1.0e-5
                })
        });
        self.display.limits = data.header.value_range;
        let mut positive_low = f32::INFINITY;
        let mut positive_high = f32::NEG_INFINITY;
        for &value in &data.values {
            if value.is_finite() && value > 0.0 {
                positive_low = positive_low.min(value);
                positive_high = positive_high.max(value);
            }
        }
        self.display.positive_range = [positive_low, positive_high];
        if !preserve_camera {
            self.camera.fit(data.header.active_bounds());
        }
        self.data = Some(data);
        self.generation = self.generation.wrapping_add(1);
    }

    pub fn set_appearance(&mut self, appearance: &AppearanceSettings) {
        self.display.scale = appearance.scale;
        self.display.colormap = appearance.colormap;
        self.display.reversed = appearance.reversed;
        self.display.color_mode = appearance.color_mode;
        let automatic = if appearance.scale == Scale::Logarithmic
            && self.display.positive_range[0].is_finite()
        {
            self.display.positive_range
        } else {
            self.data
                .as_ref()
                .map_or(self.display.limits, |data| data.header.value_range)
        };
        let requested = appearance.color_limits.unwrap_or(automatic);
        if requested.into_iter().all(f32::is_finite)
            && requested[1] > requested[0]
            && (appearance.scale == Scale::Linear || requested[0] > 0.0)
        {
            self.display.limits = requested;
        }
    }

    pub fn set_layer_styles(&mut self, styles: Vec<LayerDisplay3d>) {
        self.layer_styles = styles;
    }

    pub fn fit(&mut self) {
        if let Some(data) = &self.data {
            self.camera.fit(data.header.active_bounds());
        }
    }
}

pub type Scene3dHandle = Arc<Mutex<SharedScene3d>>;

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Uniforms3d {
    view_projection: [f32; 16],
    limits: [f32; 4],
    shape: [f32; 4],
    style: [f32; 4],
    solid_color: [f32; 4],
    model: [f32; 4],
}

#[derive(Clone, Copy)]
struct DrawRange3d {
    start: u32,
    end: u32,
    uniform_offset: u32,
    opaque: bool,
    sphere: bool,
}

pub struct Scene3dResources {
    opaque_pipeline: wgpu::RenderPipeline,
    transparent_pipeline: wgpu::RenderPipeline,
    bind_group: wgpu::BindGroup,
    uniform_buffer: wgpu::Buffer,
    position_buffer: Option<wgpu::Buffer>,
    scalar_buffer: Option<wgpu::Buffer>,
    index_buffer: Option<wgpu::Buffer>,
    sphere_position_buffer: wgpu::Buffer,
    sphere_scalar_buffer: wgpu::Buffer,
    sphere_index_buffer: wgpu::Buffer,
    sphere_index_count: u32,
    draw_ranges: Vec<DrawRange3d>,
    mesh_id: Option<String>,
    generation: u64,
}

impl Scene3dResources {
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        target_format: wgpu::TextureFormat,
    ) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("BATSView 3D slice shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("scene3d.wgsl").into()),
        });
        let uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("BATSView 3D uniforms"),
            size: UNIFORM_STRIDE * MAX_RENDER_LAYERS as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let colormap_texture = device.create_texture_with_data(
            queue,
            &wgpu::TextureDescriptor {
                label: Some("BATSView 3D colormap lookup table"),
                size: wgpu::Extent3d {
                    width: 256,
                    height: Colormap::ALL.len() as u32,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8UnormSrgb,
                usage: wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            },
            wgpu::util::TextureDataOrder::LayerMajor,
            &Colormap::lookup_texture(),
        );
        let colormap_view = colormap_texture.create_view(&Default::default());
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("BATSView 3D bind-group layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: true,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
            ],
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("BATSView 3D bind group"),
            layout: &layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                        buffer: &uniform_buffer,
                        offset: 0,
                        size: std::num::NonZeroU64::new(size_of::<Uniforms3d>() as u64),
                    }),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&colormap_view),
                },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("BATSView 3D pipeline layout"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let create_pipeline = |label: &'static str, blend, depth_write_enabled| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(&pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some("vs_main"),
                    compilation_options: Default::default(),
                    buffers: &[
                        wgpu::VertexBufferLayout {
                            array_stride: size_of::<crate::protocol::Position3>() as u64,
                            step_mode: wgpu::VertexStepMode::Vertex,
                            attributes: &wgpu::vertex_attr_array![0 => Float32x3],
                        },
                        wgpu::VertexBufferLayout {
                            array_stride: size_of::<f32>() as u64,
                            step_mode: wgpu::VertexStepMode::Vertex,
                            attributes: &wgpu::vertex_attr_array![1 => Float32],
                        },
                    ],
                },
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some("fs_main"),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: target_format,
                        blend,
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                primitive: wgpu::PrimitiveState {
                    topology: wgpu::PrimitiveTopology::TriangleList,
                    cull_mode: None,
                    ..Default::default()
                },
                depth_stencil: Some(wgpu::DepthStencilState {
                    format: wgpu::TextureFormat::Depth24Plus,
                    depth_write_enabled: Some(depth_write_enabled),
                    depth_compare: Some(wgpu::CompareFunction::LessEqual),
                    stencil: Default::default(),
                    bias: Default::default(),
                }),
                multisample: Default::default(),
                multiview_mask: None,
                cache: None,
            })
        };
        let opaque_pipeline = create_pipeline("BATSView opaque 3D surface pipeline", None, true);
        let transparent_pipeline = create_pipeline(
            "BATSView transparent 3D surface pipeline",
            Some(wgpu::BlendState::ALPHA_BLENDING),
            false,
        );
        let (sphere_positions, sphere_indices) = sphere_mesh(28, 48);
        let sphere_values = vec![0.0_f32; sphere_positions.len()];
        let sphere_position_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("BATSView reference planet positions"),
            contents: bytemuck::cast_slice(&sphere_positions),
            usage: wgpu::BufferUsages::VERTEX,
        });
        let sphere_scalar_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("BATSView reference planet scalar values"),
            contents: bytemuck::cast_slice(&sphere_values),
            usage: wgpu::BufferUsages::VERTEX,
        });
        let sphere_index_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("BATSView reference planet indices"),
            contents: bytemuck::cast_slice(&sphere_indices),
            usage: wgpu::BufferUsages::INDEX,
        });
        Self {
            opaque_pipeline,
            transparent_pipeline,
            bind_group,
            uniform_buffer,
            position_buffer: None,
            scalar_buffer: None,
            index_buffer: None,
            sphere_position_buffer,
            sphere_scalar_buffer,
            sphere_index_buffer,
            sphere_index_count: sphere_indices.len() as u32,
            draw_ranges: Vec::new(),
            mesh_id: None,
            generation: u64::MAX,
        }
    }
}

pub struct Scene3dCallback {
    scene: Scene3dHandle,
    aspect: f32,
}

impl Scene3dCallback {
    pub fn paint_callback(rect: egui::Rect, scene: Scene3dHandle) -> egui::PaintCallback {
        egui_wgpu::Callback::new_paint_callback(
            rect,
            Self {
                scene,
                aspect: rect.width() / rect.height().max(1.0),
            },
        )
    }
}

impl egui_wgpu::CallbackTrait for Scene3dCallback {
    fn prepare(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        _screen: &egui_wgpu::ScreenDescriptor,
        _encoder: &mut wgpu::CommandEncoder,
        resources: &mut egui_wgpu::CallbackResources,
    ) -> Vec<wgpu::CommandBuffer> {
        let shared = self.scene.lock().unwrap();
        let gpu: &mut Scene3dResources = resources.get_mut().expect("3D resources registered");
        let Some(data) = &shared.data else {
            return Vec::new();
        };
        if data.mesh.positions.is_empty() || data.mesh.indices.is_empty() {
            gpu.draw_ranges.clear();
            gpu.generation = shared.generation;
            return Vec::new();
        }
        if gpu.generation != shared.generation {
            if gpu.mesh_id.as_deref() != Some(&data.mesh.id) {
                gpu.position_buffer = Some(device.create_buffer_init(
                    &wgpu::util::BufferInitDescriptor {
                        label: Some("BATSView 3D positions"),
                        contents: bytemuck::cast_slice(&data.mesh.positions),
                        usage: wgpu::BufferUsages::VERTEX,
                    },
                ));
                gpu.index_buffer = Some(device.create_buffer_init(
                    &wgpu::util::BufferInitDescriptor {
                        label: Some("BATSView 3D indices"),
                        contents: bytemuck::cast_slice(&data.mesh.indices),
                        usage: wgpu::BufferUsages::INDEX,
                    },
                ));
                gpu.mesh_id = Some(data.mesh.id.clone());
            }
            gpu.scalar_buffer = Some(device.create_buffer_init(
                &wgpu::util::BufferInitDescriptor {
                    label: Some("BATSView 3D scalar values"),
                    contents: bytemuck::cast_slice(&data.values),
                    usage: wgpu::BufferUsages::VERTEX,
                },
            ));
            gpu.generation = shared.generation;
        }
        let direction = shared.camera.direction;
        let bounds = data.header.active_bounds();
        let center = [
            0.5 * (bounds[0] + bounds[1]),
            0.5 * (bounds[2] + bounds[3]),
            0.5 * (bounds[4] + bounds[5]),
        ];
        let mut ranges: Vec<(bool, f32, u32, u32, Uniforms3d, bool, u32)> = data
            .header
            .layers
            .iter()
            .filter_map(|layer| {
                let style = layer_style(&shared, layer);
                if !style.visible || layer.index_count == 0 || layer.inactive_reason.is_some() {
                    return None;
                }
                let mut point = center;
                if let (Some(axis), Some(position)) = (layer.axis.as_deref(), layer.position) {
                    let axis = match axis {
                        "x" => 0,
                        "y" => 1,
                        _ => 2,
                    };
                    point[axis] = position;
                }
                let depth =
                    point[0] * direction[0] + point[1] * direction[1] + point[2] * direction[2];
                let automatic = layer.value_range.unwrap_or(data.header.value_range);
                let requested = style.appearance.color_limits.unwrap_or(automatic);
                let limits = if requested.into_iter().all(f32::is_finite)
                    && requested[1] > requested[0]
                    && (style.appearance.scale == Scale::Linear || requested[0] > 0.0)
                {
                    requested
                } else {
                    automatic
                };
                let solid = style.solid_color.map_or([1.0; 4], |color| {
                    color.0.map(|channel| f32::from(channel) / 255.0)
                });
                let opacity = (style.opacity * solid[3]).clamp(0.0, 1.0);
                let uniforms = Uniforms3d {
                    view_projection: shared.camera.view_projection(bounds, self.aspect),
                    limits: [
                        limits[0],
                        limits[1],
                        shared.display.positive_range[0],
                        shared.display.positive_range[1],
                    ],
                    shape: [
                        style.appearance.colormap.index() as f32,
                        if style.appearance.scale == Scale::Logarithmic {
                            1.0
                        } else {
                            0.0
                        },
                        if style.appearance.reversed { 1.0 } else { 0.0 },
                        style.appearance.color_mode.bins().map_or(0.0, f32::from),
                    ],
                    style: [
                        opacity,
                        if style.solid_color.is_some() {
                            1.0
                        } else {
                            0.0
                        },
                        if layer.kind == SurfaceLayerKind::Isosurface {
                            1.0
                        } else {
                            0.0
                        },
                        0.0,
                    ],
                    solid_color: [solid[0], solid[1], solid[2], 1.0],
                    model: [1.0, 0.0, 0.0, 0.0],
                };
                Some((
                    opacity >= 0.995,
                    depth,
                    layer.index_start,
                    layer.index_start + layer.index_count,
                    uniforms,
                    false,
                    style.order,
                ))
            })
            .collect();
        if shared.display.show_reference_sphere
            && sphere_intersects_bounds(shared.display.reference_sphere_radius, bounds)
        {
            ranges.push((
                true,
                0.0,
                0,
                gpu.sphere_index_count,
                Uniforms3d {
                    view_projection: shared.camera.view_projection(bounds, self.aspect),
                    limits: [0.0, 1.0, 0.0, 1.0],
                    shape: [0.0; 4],
                    style: [1.0, 1.0, 1.0, 0.0],
                    solid_color: [0.32, 0.39, 0.48, 1.0],
                    model: [
                        shared.display.reference_sphere_radius.max(1.0e-6),
                        0.0,
                        0.0,
                        0.0,
                    ],
                },
                true,
                0,
            ));
        }
        ranges.sort_by(|left, right| {
            // Opaque geometry populates depth first. Translucent layers then draw back-to-front.
            right
                .0
                .cmp(&left.0)
                .then_with(|| left.1.total_cmp(&right.1))
                .then_with(|| left.6.cmp(&right.6))
        });
        gpu.draw_ranges.clear();
        for (slot, (opaque, _, start, end, uniforms, sphere, _)) in
            ranges.into_iter().take(MAX_RENDER_LAYERS).enumerate()
        {
            let offset = UNIFORM_STRIDE * slot as u64;
            queue.write_buffer(&gpu.uniform_buffer, offset, bytemuck::bytes_of(&uniforms));
            gpu.draw_ranges.push(DrawRange3d {
                start,
                end,
                uniform_offset: offset as u32,
                opaque,
                sphere,
            });
        }
        Vec::new()
    }

    fn paint(
        &self,
        _info: egui::PaintCallbackInfo,
        render_pass: &mut wgpu::RenderPass<'static>,
        resources: &egui_wgpu::CallbackResources,
    ) {
        let gpu: &Scene3dResources = resources.get().expect("3D resources registered");
        let (Some(positions), Some(values), Some(indices)) =
            (&gpu.position_buffer, &gpu.scalar_buffer, &gpu.index_buffer)
        else {
            return;
        };
        for range in &gpu.draw_ranges {
            render_pass.set_pipeline(if range.opaque {
                &gpu.opaque_pipeline
            } else {
                &gpu.transparent_pipeline
            });
            render_pass.set_bind_group(0, &gpu.bind_group, &[range.uniform_offset]);
            if range.sphere {
                render_pass.set_vertex_buffer(0, gpu.sphere_position_buffer.slice(..));
                render_pass.set_vertex_buffer(1, gpu.sphere_scalar_buffer.slice(..));
                render_pass
                    .set_index_buffer(gpu.sphere_index_buffer.slice(..), wgpu::IndexFormat::Uint32);
            } else {
                render_pass.set_vertex_buffer(0, positions.slice(..));
                render_pass.set_vertex_buffer(1, values.slice(..));
                render_pass.set_index_buffer(indices.slice(..), wgpu::IndexFormat::Uint32);
            }
            render_pass.draw_indexed(range.start..range.end, 0, 0..1);
        }
    }
}

fn sphere_mesh(
    latitude_segments: u32,
    longitude_segments: u32,
) -> (Vec<crate::protocol::Position3>, Vec<u32>) {
    let mut positions = Vec::new();
    for latitude in 0..=latitude_segments {
        let polar = std::f32::consts::PI * latitude as f32 / latitude_segments as f32;
        let (sin_polar, cos_polar) = polar.sin_cos();
        for longitude in 0..=longitude_segments {
            let azimuth = std::f32::consts::TAU * longitude as f32 / longitude_segments as f32;
            let (sin_azimuth, cos_azimuth) = azimuth.sin_cos();
            positions.push(crate::protocol::Position3 {
                x: sin_polar * cos_azimuth,
                y: sin_polar * sin_azimuth,
                z: cos_polar,
            });
        }
    }
    let stride = longitude_segments + 1;
    let mut indices = Vec::new();
    for latitude in 0..latitude_segments {
        for longitude in 0..longitude_segments {
            let a = latitude * stride + longitude;
            let b = a + stride;
            indices.extend_from_slice(&[a, b, a + 1, a + 1, b, b + 1]);
        }
    }
    (positions, indices)
}

fn sphere_intersects_bounds(radius: f32, bounds: [f32; 6]) -> bool {
    let mut distance_squared = 0.0;
    for axis in 0..3 {
        let low = bounds[axis * 2];
        let high = bounds[axis * 2 + 1];
        let distance = if 0.0 < low {
            low
        } else if 0.0 > high {
            -high
        } else {
            0.0
        };
        distance_squared += distance * distance;
    }
    distance_squared <= radius * radius
}

fn layer_style(shared: &SharedScene3d, layer: &SurfaceLayerHeader) -> LayerDisplay3d {
    if layer.kind == SurfaceLayerKind::Isosurface
        && let Some(style) = layer.layer_id.and_then(|id| {
            shared
                .layer_styles
                .iter()
                .find(|style| style.layer_id == id)
        })
    {
        return style.clone();
    }
    LayerDisplay3d {
        layer_id: layer.layer_id.unwrap_or_default(),
        visible: true,
        opacity: shared.display.opacity,
        solid_color: None,
        appearance: AppearanceSettings {
            scale: shared.display.scale,
            colormap: shared.display.colormap,
            reversed: shared.display.reversed,
            color_mode: shared.display.color_mode,
            color_limits: Some(shared.display.limits),
            ..AppearanceSettings::default()
        },
        order: 0,
    }
}

pub fn paint_scene_overlays(ui: &egui::Ui, rect: egui::Rect, scene: &Scene3dHandle) {
    let shared = scene.lock().unwrap();
    let Some(data) = &shared.data else { return };
    let bounds = data.header.active_bounds();
    let aspect = rect.width() / rect.height().max(1.0);
    let project = |point: [f32; 3]| -> Option<Pos2> {
        let ndc = shared.camera.project(point, bounds, aspect)?;
        Some(Pos2::new(
            rect.left() + (ndc[0] + 1.0) * 0.5 * rect.width(),
            rect.bottom() - (ndc[1] + 1.0) * 0.5 * rect.height(),
        ))
    };
    let corners = [
        [bounds[0], bounds[2], bounds[4]],
        [bounds[1], bounds[2], bounds[4]],
        [bounds[1], bounds[3], bounds[4]],
        [bounds[0], bounds[3], bounds[4]],
        [bounds[0], bounds[2], bounds[5]],
        [bounds[1], bounds[2], bounds[5]],
        [bounds[1], bounds[3], bounds[5]],
        [bounds[0], bounds[3], bounds[5]],
    ];
    if shared.display.show_box {
        for (a, b) in [
            (0, 1),
            (1, 2),
            (2, 3),
            (3, 0),
            (4, 5),
            (5, 6),
            (6, 7),
            (7, 4),
            (0, 4),
            (1, 5),
            (2, 6),
            (3, 7),
        ] {
            if let (Some(a), Some(b)) = (project(corners[a]), project(corners[b])) {
                ui.painter()
                    .line_segment([a, b], Stroke::new(1.0, Color32::from_gray(105)));
            }
        }
    }
    if shared.display.show_axes {
        let origin = [bounds[0], bounds[2], bounds[4]];
        for (end, color, label) in [
            (
                [bounds[1], bounds[2], bounds[4]],
                Color32::from_rgb(235, 92, 92),
                "X",
            ),
            (
                [bounds[0], bounds[3], bounds[4]],
                Color32::from_rgb(95, 210, 135),
                "Y",
            ),
            (
                [bounds[0], bounds[2], bounds[5]],
                Color32::from_rgb(90, 155, 245),
                "Z",
            ),
        ] {
            if let (Some(start), Some(end)) = (project(origin), project(end)) {
                ui.painter()
                    .line_segment([start, end], Stroke::new(2.0, color));
                ui.painter().text(
                    end,
                    egui::Align2::CENTER_CENTER,
                    label,
                    egui::FontId::proportional(12.0),
                    color,
                );
            }
        }
    }
}

pub fn paint_fieldlines3d(
    ui: &egui::Ui,
    rect: egui::Rect,
    scene: &Scene3dHandle,
    lines: &FieldLines3dData,
    settings: &FieldLine3dSettings,
) {
    if !settings.enabled {
        return;
    }
    let shared = scene.lock().unwrap();
    let Some(data) = &shared.data else { return };
    let aspect = rect.width() / rect.height().max(1.0);
    let matrix = shared
        .camera
        .view_projection(data.header.active_bounds(), aspect);
    drop(shared);
    let painter = ui.painter().with_clip_rect(rect);
    let stroke = Stroke::new(settings.width.clamp(0.25, 12.0), settings.color.to_egui());
    for line in lines.lines() {
        let mut projected = Vec::with_capacity(line.len());
        for point in line {
            let Some(ndc) = project_point(matrix, [point.x, point.y, point.z]) else {
                continue;
            };
            if !(0.0..=1.0).contains(&ndc[2]) {
                continue;
            }
            projected.push(Pos2::new(
                rect.left() + (ndc[0] + 1.0) * 0.5 * rect.width(),
                rect.bottom() - (ndc[1] + 1.0) * 0.5 * rect.height(),
            ));
        }
        if projected.len() < 2 {
            continue;
        }
        painter.add(egui::Shape::line(projected.clone(), stroke));
        if settings.arrows {
            paint_projected_arrow(&painter, &projected, stroke, settings.arrow_size);
        }
    }
}

fn paint_projected_arrow(painter: &egui::Painter, points: &[Pos2], stroke: Stroke, size: f32) {
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
