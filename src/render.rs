use std::sync::{Arc, Mutex};

use bytemuck::{Pod, Zeroable};
use eframe::{
    egui,
    egui_wgpu::{self, wgpu},
};
use wgpu::util::DeviceExt;

use crate::protocol::{PlotData, Vertex};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scale {
    Linear,
    Logarithmic,
}

#[derive(Clone, Debug)]
pub struct DisplayState {
    pub scale: Scale,
    pub limits: [f32; 2],
    pub pan: [f32; 2],
    pub zoom: f32,
    pub viewport_aspect: f32,
}

impl Default for DisplayState {
    fn default() -> Self {
        Self {
            scale: Scale::Linear,
            limits: [0.0, 1.0],
            pan: [0.0, 0.0],
            zoom: 1.0,
            viewport_aspect: 1.0,
        }
    }
}

#[derive(Default)]
pub struct SharedPlot {
    pub generation: u64,
    pub data: Option<Arc<PlotData>>,
    pub display: DisplayState,
}

impl SharedPlot {
    pub fn set_data(&mut self, data: PlotData) {
        self.display.limits = data.header.value_range;
        self.display.pan = [0.0, 0.0];
        self.display.zoom = 1.0;
        self.data = Some(Arc::new(data));
        self.generation = self.generation.wrapping_add(1);
    }

    pub fn reset_view(&mut self) {
        self.display.pan = [0.0, 0.0];
        self.display.zoom = 1.0;
    }
}

pub type PlotHandle = Arc<Mutex<SharedPlot>>;

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Uniforms {
    bounds: [f32; 4],
    limits: [f32; 4],
    view: [f32; 4],
    shape: [f32; 4],
}

pub struct PlotResources {
    pipeline: wgpu::RenderPipeline,
    bind_group: wgpu::BindGroup,
    uniform_buffer: wgpu::Buffer,
    vertex_buffer: Option<wgpu::Buffer>,
    index_buffer: Option<wgpu::Buffer>,
    index_count: u32,
    generation: u64,
}

impl PlotResources {
    pub fn new(device: &wgpu::Device, target_format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("BATSView scalar shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("plot.wgsl").into()),
        });
        let uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("BATSView uniforms"),
            size: size_of::<Uniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("BATSView uniform layout"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("BATSView uniform bind group"),
            layout: &bind_group_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: uniform_buffer.as_entire_binding(),
            }],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("BATSView pipeline layout"),
            bind_group_layouts: &[Some(&bind_group_layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("BATSView scalar pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: size_of::<Vertex>() as u64,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &wgpu::vertex_attr_array![0 => Float32x2, 1 => Float32],
                }],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: target_format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                cull_mode: None,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });
        Self {
            pipeline,
            bind_group,
            uniform_buffer,
            vertex_buffer: None,
            index_buffer: None,
            index_count: 0,
            generation: u64::MAX,
        }
    }
}

pub struct PlotCallback {
    plot: PlotHandle,
}

impl PlotCallback {
    pub fn paint_callback(rect: egui::Rect, plot: PlotHandle) -> egui::PaintCallback {
        egui_wgpu::Callback::new_paint_callback(rect, Self { plot })
    }
}

impl egui_wgpu::CallbackTrait for PlotCallback {
    fn prepare(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        _screen: &egui_wgpu::ScreenDescriptor,
        _encoder: &mut wgpu::CommandEncoder,
        resources: &mut egui_wgpu::CallbackResources,
    ) -> Vec<wgpu::CommandBuffer> {
        let shared = self.plot.lock().unwrap();
        let gpu: &mut PlotResources = resources.get_mut().expect("plot resources registered");
        let Some(data) = &shared.data else {
            return Vec::new();
        };
        if gpu.generation != shared.generation {
            gpu.vertex_buffer = Some(device.create_buffer_init(
                &wgpu::util::BufferInitDescriptor {
                    label: Some("BATSView vertices"),
                    contents: bytemuck::cast_slice(&data.vertices),
                    usage: wgpu::BufferUsages::VERTEX,
                },
            ));
            gpu.index_buffer = Some(
                device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("BATSView indices"),
                    contents: bytemuck::cast_slice(&data.indices),
                    usage: wgpu::BufferUsages::INDEX,
                }),
            );
            gpu.index_count = data.indices.len().try_into().unwrap_or(u32::MAX);
            gpu.generation = shared.generation;
        }
        let bounds = data.header.bounds;
        let data_aspect = ((bounds[1] - bounds[0]) / (bounds[3] - bounds[2]))
            .abs()
            .max(1.0e-12);
        let positive = data.header.positive_range.unwrap_or([f32::NAN, f32::NAN]);
        let uniforms = Uniforms {
            bounds,
            limits: [
                shared.display.limits[0],
                shared.display.limits[1],
                positive[0],
                positive[1],
            ],
            view: [
                shared.display.pan[0],
                shared.display.pan[1],
                shared.display.zoom,
                shared.display.viewport_aspect,
            ],
            shape: [
                data_aspect,
                if shared.display.scale == Scale::Logarithmic {
                    1.0
                } else {
                    0.0
                },
                0.0,
                0.0,
            ],
        };
        queue.write_buffer(&gpu.uniform_buffer, 0, bytemuck::bytes_of(&uniforms));
        Vec::new()
    }

    fn paint(
        &self,
        _info: egui::PaintCallbackInfo,
        render_pass: &mut wgpu::RenderPass<'static>,
        resources: &egui_wgpu::CallbackResources,
    ) {
        let gpu: &PlotResources = resources.get().expect("plot resources registered");
        let (Some(vertices), Some(indices)) = (&gpu.vertex_buffer, &gpu.index_buffer) else {
            return;
        };
        render_pass.set_pipeline(&gpu.pipeline);
        render_pass.set_bind_group(0, &gpu.bind_group, &[]);
        render_pass.set_vertex_buffer(0, vertices.slice(..));
        render_pass.set_index_buffer(indices.slice(..), wgpu::IndexFormat::Uint32);
        render_pass.draw_indexed(0..gpu.index_count, 0, 0..1);
    }
}
