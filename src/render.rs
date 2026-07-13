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
    pub view_bounds: [f32; 4],
}

impl Default for DisplayState {
    fn default() -> Self {
        Self {
            scale: Scale::Linear,
            limits: [0.0, 1.0],
            view_bounds: [0.0, 1.0, 0.0, 1.0],
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
        let preserve_view = self.data.as_ref().is_some_and(|current| {
            current.header.x_label == data.header.x_label
                && current.header.y_label == data.header.y_label
                && similar_bounds(current.header.bounds, data.header.bounds)
        });
        self.display.limits = data.header.value_range;
        if !preserve_view {
            self.display.view_bounds = usable_bounds(data.header.bounds);
        }
        self.data = Some(Arc::new(data));
        self.generation = self.generation.wrapping_add(1);
    }

    pub fn reset_view(&mut self) {
        if let Some(data) = &self.data {
            self.display.view_bounds = usable_bounds(data.header.bounds);
        }
    }

    pub fn set_view_bounds(&mut self, bounds: [f32; 4]) -> bool {
        if valid_bounds(bounds) {
            self.display.view_bounds = bounds;
            true
        } else {
            false
        }
    }

    pub fn pan_view(&mut self, x_fraction: f32, y_fraction: f32) {
        let bounds = &mut self.display.view_bounds;
        let x_offset = (bounds[1] - bounds[0]) * x_fraction;
        let y_offset = (bounds[3] - bounds[2]) * y_fraction;
        bounds[0] += x_offset;
        bounds[1] += x_offset;
        bounds[2] += y_offset;
        bounds[3] += y_offset;
    }

    pub fn zoom_view(&mut self, factor: f32) {
        let Some(data) = &self.data else { return };
        let factor = factor.clamp(0.01, 100.0);
        let bounds = &mut self.display.view_bounds;
        for (low, high, data_low, data_high) in [(0, 1, 0, 1), (2, 3, 2, 3)] {
            let center = 0.5 * (bounds[low] + bounds[high]);
            let data_span = (data.header.bounds[data_high] - data.header.bounds[data_low])
                .abs()
                .max(1.0e-20);
            let span = ((bounds[high] - bounds[low]).abs() * factor)
                .clamp(data_span * 1.0e-6, data_span * 1.0e6);
            bounds[low] = center - 0.5 * span;
            bounds[high] = center + 0.5 * span;
        }
    }
}

fn similar_bounds(left: [f32; 4], right: [f32; 4]) -> bool {
    left.into_iter().zip(right).all(|(left, right)| {
        let scale = left.abs().max(right.abs()).max(1.0);
        (left - right).abs() <= 1.0e-5 * scale
    })
}

fn valid_bounds(bounds: [f32; 4]) -> bool {
    bounds.into_iter().all(f32::is_finite) && bounds[1] > bounds[0] && bounds[3] > bounds[2]
}

fn usable_bounds(mut bounds: [f32; 4]) -> [f32; 4] {
    for (low, high) in [(0, 1), (2, 3)] {
        if !bounds[low].is_finite() || !bounds[high].is_finite() {
            bounds[low] = 0.0;
            bounds[high] = 1.0;
        } else if bounds[high] <= bounds[low] {
            let padding = bounds[low].abs().max(1.0) * 1.0e-3;
            bounds[low] -= padding;
            bounds[high] += padding;
        }
    }
    bounds
}

pub type PlotHandle = Arc<Mutex<SharedPlot>>;

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Uniforms {
    bounds: [f32; 4],
    limits: [f32; 4],
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
        let bounds = shared.display.view_bounds;
        let positive = data.header.positive_range.unwrap_or([f32::NAN, f32::NAN]);
        let uniforms = Uniforms {
            bounds,
            limits: [
                shared.display.limits[0],
                shared.display.limits[1],
                positive[0],
                positive[1],
            ],
            shape: [
                0.0,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::PlotHeader;

    fn plot_data() -> PlotData {
        PlotData {
            header: PlotHeader {
                protocol: 1,
                path: "test.plt".into(),
                title: "test".into(),
                section: Some("z=0".into()),
                zone: "cut".into(),
                variable: "density".into(),
                source_variable: "Rho".into(),
                unit: None,
                x_label: "X".into(),
                y_label: "Y".into(),
                point_count: 0,
                triangle_count: 0,
                bounds: [-10.0, 10.0, -5.0, 5.0],
                value_range: [1.0, 2.0],
                positive_range: Some([1.0, 2.0]),
            },
            vertices: Vec::new(),
            indices: Vec::new(),
        }
    }

    #[test]
    fn view_uses_data_coordinates_for_pan_zoom_and_reset() {
        let mut plot = SharedPlot::default();
        plot.set_data(plot_data());
        plot.zoom_view(0.5);
        assert_eq!(plot.display.view_bounds, [-5.0, 5.0, -2.5, 2.5]);
        plot.pan_view(0.1, -0.2);
        assert_eq!(plot.display.view_bounds, [-4.0, 6.0, -3.5, 1.5]);
        assert!(!plot.set_view_bounds([2.0, 1.0, -1.0, 1.0]));
        assert_eq!(plot.display.view_bounds, [-4.0, 6.0, -3.5, 1.5]);
        assert!(plot.set_view_bounds([-2.0, 2.0, -1.0, 1.0]));
        plot.set_data(plot_data());
        assert_eq!(plot.display.view_bounds, [-2.0, 2.0, -1.0, 1.0]);
        plot.reset_view();
        assert_eq!(plot.display.view_bounds, [-10.0, 10.0, -5.0, 5.0]);
    }
}
