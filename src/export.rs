use std::{
    path::{Path, PathBuf},
    sync::mpsc,
};

use anyhow::{Context, Result, bail};
use eframe::{
    egui::{self, Color32, Rect, Stroke, StrokeKind},
    egui_wgpu::{self, RenderState, RendererOptions, ScreenDescriptor, wgpu},
};
use image::RgbaImage;

use crate::{
    annotations::AnnotationEditor,
    plot_ui::{PlotChrome, PlotColors, fit_plot_rect, paint_plot_chrome},
    render::{PlotCallback, PlotHandle, PlotResources},
    scene::{AppearanceSettings, SceneDocument, ScopeContext},
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ExportBackground {
    #[default]
    Dark,
    White,
    Transparent,
}

impl ExportBackground {
    pub const ALL: [Self; 3] = [Self::Dark, Self::White, Self::Transparent];

    pub fn name(self) -> &'static str {
        match self {
            Self::Dark => "Dark canvas",
            Self::White => "White",
            Self::Transparent => "Transparent",
        }
    }

    pub fn canvas_color(self) -> Color32 {
        match self {
            Self::Dark => Color32::from_rgb(9, 14, 21),
            Self::White => Color32::WHITE,
            Self::Transparent => Color32::TRANSPARENT,
        }
    }

    pub fn foreground(self) -> Color32 {
        match self {
            Self::White | Self::Transparent => Color32::from_rgb(22, 29, 37),
            Self::Dark => Color32::from_rgb(226, 232, 240),
        }
    }

    pub fn muted_foreground(self) -> Color32 {
        match self {
            Self::White | Self::Transparent => Color32::from_rgb(70, 82, 96),
            Self::Dark => Color32::from_rgb(145, 158, 173),
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct ExportSettings {
    pub scale: u32,
    pub background: ExportBackground,
}

impl Default for ExportSettings {
    fn default() -> Self {
        Self {
            scale: 2,
            background: ExportBackground::Dark,
        }
    }
}

pub struct ExportFrame {
    pub render_state: RenderState,
    pub plot: PlotHandle,
    pub scene: SceneDocument,
    pub scope_section: Option<String>,
    pub scope_variable: Option<String>,
    pub scope_relative_path: Option<String>,
    pub appearance: AppearanceSettings,
    pub chrome: PlotChrome,
    pub logical_size: egui::Vec2,
    pub pixels_per_point: f32,
    pub settings: ExportSettings,
    pub destination: PathBuf,
}

pub fn normalized_png_path(path: &Path) -> PathBuf {
    if path
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("png"))
    {
        path.to_owned()
    } else {
        path.with_extension("png")
    }
}

pub fn render_plot_png(frame: ExportFrame) -> Result<PathBuf> {
    let scale = frame.settings.scale.clamp(1, 4) as f32;
    let native_pixels_per_point = frame.pixels_per_point.max(1.0);
    let requested_pixels_per_point = native_pixels_per_point * scale;
    let requested_width = (frame.logical_size.x * requested_pixels_per_point)
        .round()
        .max(1.0) as u32;
    let requested_height = (frame.logical_size.y * requested_pixels_per_point)
        .round()
        .max(1.0) as u32;
    let maximum = frame.render_state.device.limits().max_texture_dimension_2d;
    if requested_width > maximum || requested_height > maximum {
        bail!(
            "requested image is {requested_width}×{requested_height}, but this GPU supports at most {maximum} pixels per side"
        );
    }

    let device = &frame.render_state.device;
    let queue = &frame.render_state.queue;
    let format = wgpu::TextureFormat::Rgba8UnormSrgb;
    let mut renderer = egui_wgpu::Renderer::new(device, format, RendererOptions::default());
    renderer
        .callback_resources
        .insert(PlotResources::new(device, queue, format));

    let context = egui::Context::default();
    context.set_zoom_factor(scale);
    let logical_rect = Rect::from_min_size(egui::Pos2::ZERO, frame.logical_size);
    let mut raw_input = egui::RawInput {
        screen_rect: Some(logical_rect),
        max_texture_side: Some(maximum as usize),
        ..Default::default()
    };
    if let Some(viewport) = raw_input.viewports.get_mut(&egui::ViewportId::ROOT) {
        viewport.native_pixels_per_point = Some(native_pixels_per_point);
        viewport.inner_rect = Some(logical_rect);
    }
    let background = frame.settings.background;
    let full_output = context.run_ui(raw_input, |ui| {
        let export_rect = logical_rect;
        if background != ExportBackground::Transparent {
            ui.painter()
                .rect_filled(export_rect, 0.0, background.canvas_color());
        }
        let display = frame.plot.lock().unwrap().display.clone();
        let chart_outer = Rect::from_min_max(
            export_rect.min + egui::vec2(66.0, 64.0),
            export_rect.max - egui::vec2(116.0, 54.0),
        );
        let plot_rect = fit_plot_rect(chart_outer, display.view_bounds);
        if background != ExportBackground::Transparent {
            ui.painter()
                .rect_filled(plot_rect, 2.0, background.canvas_color());
        }
        ui.painter().rect_stroke(
            plot_rect,
            2.0,
            Stroke::new(1.0, background.muted_foreground().gamma_multiply(0.55)),
            StrokeKind::Inside,
        );
        ui.painter()
            .add(PlotCallback::paint_callback(plot_rect, frame.plot.clone()));
        let scope = ScopeContext {
            section: frame.scope_section.as_deref(),
            variable: frame.scope_variable.as_deref(),
            relative_path: frame.scope_relative_path.as_deref(),
        };
        AnnotationEditor::default().paint(
            ui,
            plot_rect,
            display.view_bounds,
            &frame.scene,
            &scope,
            false,
        );
        paint_plot_chrome(
            ui,
            export_rect,
            plot_rect,
            &frame.chrome,
            &display,
            &frame.appearance,
            PlotColors {
                foreground: background.foreground(),
                muted: background.muted_foreground(),
            },
        );
    });
    let pixels_per_point = full_output.pixels_per_point;
    let width = (frame.logical_size.x * pixels_per_point).round().max(1.0) as u32;
    let height = (frame.logical_size.y * pixels_per_point).round().max(1.0) as u32;
    if width > maximum || height > maximum {
        bail!(
            "rendered image is {width}×{height}, but this GPU supports at most {maximum} pixels per side"
        );
    }
    let paint_jobs = context.tessellate(full_output.shapes, pixels_per_point);
    for (id, delta) in &full_output.textures_delta.set {
        renderer.update_texture(device, queue, *id, delta);
    }

    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("BATSView PNG export"),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    let unpadded_bytes_per_row = width * 4;
    let padded_bytes_per_row = unpadded_bytes_per_row.div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT)
        * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("BATSView PNG readback"),
        size: u64::from(padded_bytes_per_row) * u64::from(height),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let screen = ScreenDescriptor {
        size_in_pixels: [width, height],
        pixels_per_point,
    };
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("BATSView PNG encoder"),
    });
    let callback_buffers =
        renderer.update_buffers(device, queue, &mut encoder, &paint_jobs, &screen);
    {
        let mut render_pass = encoder
            .begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("BATSView PNG render pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            })
            .forget_lifetime();
        renderer.render(&mut render_pass, &paint_jobs, &screen);
    }
    encoder.copy_texture_to_buffer(
        texture.as_image_copy(),
        wgpu::TexelCopyBufferInfo {
            buffer: &readback,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(padded_bytes_per_row),
                rows_per_image: Some(height),
            },
        },
        texture.size(),
    );
    queue.submit(callback_buffers.into_iter().chain([encoder.finish()]));

    let slice = readback.slice(..);
    let (sender, receiver) = mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |result| {
        let _ = sender.send(result);
    });
    device
        .poll(wgpu::PollType::wait_indefinitely())
        .context("waiting for the exported image to render")?;
    receiver
        .recv()
        .context("waiting for PNG readback")?
        .context("reading rendered PNG pixels")?;
    let mapped = slice.get_mapped_range();
    let mut rgba = Vec::with_capacity(width as usize * height as usize * 4);
    for row in mapped.chunks_exact(padded_bytes_per_row as usize) {
        rgba.extend_from_slice(&row[..unpadded_bytes_per_row as usize]);
    }
    drop(mapped);
    readback.unmap();
    if background == ExportBackground::Transparent {
        unpremultiply_alpha(&mut rgba);
    }
    let image = RgbaImage::from_raw(width, height, rgba)
        .context("the rendered image buffer had an unexpected size")?;
    let destination = normalized_png_path(&frame.destination);
    image
        .save(&destination)
        .with_context(|| format!("saving {}", destination.display()))?;
    Ok(destination)
}

fn unpremultiply_alpha(rgba: &mut [u8]) {
    for pixel in rgba.chunks_exact_mut(4) {
        let alpha = u32::from(pixel[3]);
        if alpha == 0 {
            pixel[0] = 0;
            pixel[1] = 0;
            pixel[2] = 0;
        } else if alpha < 255 {
            for channel in &mut pixel[..3] {
                *channel = ((u32::from(*channel) * 255 + alpha / 2) / alpha).min(255) as u8;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;
    use crate::render::SharedPlot;

    #[test]
    fn png_path_is_normalized_without_replacing_png_case() {
        assert_eq!(
            normalized_png_path(Path::new("plot")),
            PathBuf::from("plot.png")
        );
        assert_eq!(
            normalized_png_path(Path::new("plot.PNG")),
            PathBuf::from("plot.PNG")
        );
    }

    #[test]
    fn transparent_pixels_are_unpremultiplied() {
        let mut pixels = vec![50, 25, 0, 128, 20, 30, 40, 0, 1, 2, 3, 255];
        unpremultiply_alpha(&mut pixels);
        assert_eq!(&pixels[0..4], &[100, 50, 0, 128]);
        assert_eq!(&pixels[4..8], &[0, 0, 0, 0]);
        assert_eq!(&pixels[8..12], &[1, 2, 3, 255]);
    }

    #[test]
    fn hidpi_export_callback_stays_inside_the_requested_frame() {
        let context = egui::Context::default();
        context.set_zoom_factor(2.0);
        let logical_rect = Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1094.0, 942.0));
        let mut input = egui::RawInput {
            screen_rect: Some(logical_rect),
            ..Default::default()
        };
        let viewport = input.viewports.get_mut(&egui::ViewportId::ROOT).unwrap();
        viewport.native_pixels_per_point = Some(2.0);
        viewport.inner_rect = Some(logical_rect);
        let plot = Arc::new(Mutex::new(SharedPlot::default()));
        let output = context.run_ui(input, |ui| {
            let chart_outer = Rect::from_min_max(
                logical_rect.min + egui::vec2(66.0, 64.0),
                logical_rect.max - egui::vec2(116.0, 54.0),
            );
            let plot_rect = fit_plot_rect(chart_outer, [-32.0, 224.0, -128.0, 128.0]);
            ui.painter()
                .add(PlotCallback::paint_callback(plot_rect, plot.clone()));
        });
        assert_eq!(output.pixels_per_point, 4.0);
        let jobs = context.tessellate(output.shapes, output.pixels_per_point);
        let callback = jobs
            .iter()
            .find_map(|job| match &job.primitive {
                egui::epaint::Primitive::Callback(callback) => Some(callback.rect),
                _ => None,
            })
            .unwrap();
        assert!(callback.left() >= 0.0 && callback.top() >= 0.0);
        assert!(
            callback.right() <= logical_rect.right(),
            "callback {callback:?} exceeds frame {logical_rect:?}"
        );
        assert!(
            callback.bottom() <= logical_rect.bottom(),
            "callback {callback:?} exceeds frame {logical_rect:?}"
        );
        assert!((callback.width() - callback.height()).abs() < 0.01);
    }
}
