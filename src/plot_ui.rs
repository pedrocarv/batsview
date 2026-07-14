use eframe::egui::{self, Color32, FontId, Stroke, StrokeKind};

use crate::{
    render::DisplayState,
    scene::{AppearanceSettings, colorbar_ticks, normalized_value},
};

#[derive(Clone, Copy)]
pub struct PlotColors {
    pub foreground: Color32,
    pub muted: Color32,
}

#[derive(Clone, Debug)]
pub struct PlotChrome {
    pub title: String,
    pub subtitle: String,
    pub x_label: String,
    pub y_label: String,
    pub unit: Option<String>,
    pub filename: String,
}

pub fn paint_plot_chrome(
    ui: &egui::Ui,
    export_rect: egui::Rect,
    plot_rect: egui::Rect,
    chrome: &PlotChrome,
    display: &DisplayState,
    appearance: &AppearanceSettings,
    colors: PlotColors,
) {
    let PlotColors { foreground, muted } = colors;
    ui.painter().text(
        export_rect.left_top() + egui::vec2(18.0, 14.0),
        egui::Align2::LEFT_TOP,
        &chrome.title,
        FontId::proportional(20.0),
        foreground,
    );
    ui.painter().text(
        export_rect.left_top() + egui::vec2(18.0, 41.0),
        egui::Align2::LEFT_TOP,
        &chrome.subtitle,
        FontId::proportional(11.5),
        muted,
    );
    ui.painter().text(
        plot_rect.center_bottom() + egui::vec2(0.0, 31.0),
        egui::Align2::CENTER_CENTER,
        &chrome.x_label,
        FontId::proportional(13.0),
        foreground,
    );
    ui.painter().text(
        plot_rect.left_center() - egui::vec2(42.0, 0.0),
        egui::Align2::CENTER_CENTER,
        &chrome.y_label,
        FontId::proportional(13.0),
        foreground,
    );
    for (position, align, value) in [
        (
            plot_rect.left_bottom() + egui::vec2(0.0, 7.0),
            egui::Align2::LEFT_TOP,
            display.view_bounds[0],
        ),
        (
            plot_rect.right_bottom() + egui::vec2(0.0, 7.0),
            egui::Align2::RIGHT_TOP,
            display.view_bounds[1],
        ),
        (
            plot_rect.left_bottom() - egui::vec2(7.0, 0.0),
            egui::Align2::RIGHT_BOTTOM,
            display.view_bounds[2],
        ),
        (
            plot_rect.left_top() - egui::vec2(7.0, 0.0),
            egui::Align2::RIGHT_TOP,
            display.view_bounds[3],
        ),
    ] {
        ui.painter().text(
            position,
            align,
            format_value(value),
            FontId::monospace(10.0),
            muted,
        );
    }

    let bar = egui::Rect::from_min_max(
        egui::pos2(plot_rect.right() + 20.0, plot_rect.top()),
        egui::pos2(plot_rect.right() + 40.0, plot_rect.bottom()),
    );
    for step in 0..96 {
        let top = bar.top() + bar.height() * step as f32 / 96.0;
        let bottom = bar.top() + bar.height() * (step + 1) as f32 / 96.0;
        let normalized = 1.0 - step as f32 / 95.0;
        ui.painter().rect_filled(
            egui::Rect::from_min_max(egui::pos2(bar.left(), top), egui::pos2(bar.right(), bottom)),
            0.0,
            sample_appearance(appearance, normalized),
        );
    }
    ui.painter()
        .rect_stroke(bar, 0.0, Stroke::new(1.0, muted), StrokeKind::Inside);
    for tick in colorbar_ticks(&appearance.ticks, display.limits, appearance.scale) {
        if let Some(normalized) = normalized_value(tick.value, display.limits, appearance.scale) {
            let y = bar.bottom() - normalized * bar.height();
            ui.painter().line_segment(
                [egui::pos2(bar.right(), y), egui::pos2(bar.right() + 5.0, y)],
                Stroke::new(1.0, muted),
            );
            ui.painter().text(
                egui::pos2(bar.right() + 8.0, y),
                egui::Align2::LEFT_CENTER,
                tick.label,
                FontId::monospace(10.0),
                foreground,
            );
        }
    }
    if let Some(unit) = &chrome.unit {
        ui.painter().text(
            bar.center_top() - egui::vec2(0.0, 8.0),
            egui::Align2::CENTER_BOTTOM,
            unit,
            FontId::proportional(10.0),
            muted,
        );
    }
    ui.painter().text(
        export_rect.right_bottom() - egui::vec2(12.0, 10.0),
        egui::Align2::RIGHT_BOTTOM,
        &chrome.filename,
        FontId::monospace(9.0),
        muted,
    );
}

pub fn fit_plot_rect(outer: egui::Rect, bounds: [f32; 4]) -> egui::Rect {
    let x_span = (bounds[1] - bounds[0]).abs();
    let y_span = (bounds[3] - bounds[2]).abs();
    if !x_span.is_finite() || !y_span.is_finite() || x_span <= 0.0 || y_span <= 0.0 {
        return outer;
    }
    let plot_aspect = x_span / y_span;
    let outer_aspect = outer.width() / outer.height().max(1.0e-6);
    let size = if plot_aspect > outer_aspect {
        egui::vec2(outer.width(), outer.width() / plot_aspect)
    } else {
        egui::vec2(outer.height() * plot_aspect, outer.height())
    };
    egui::Rect::from_center_size(outer.center(), size)
}

pub fn sample_appearance(appearance: &AppearanceSettings, normalized: f32) -> Color32 {
    let mut value = normalized.clamp(0.0, 1.0);
    if appearance.reversed {
        value = 1.0 - value;
    }
    if let Some(bins) = appearance.color_mode.bins() {
        let bins = f32::from(bins);
        value = (value * bins).floor().min(bins - 1.0) / (bins - 1.0).max(1.0);
    }
    appearance.colormap.sample(value).to_egui()
}

fn format_value(value: f32) -> String {
    if value.abs() >= 10_000.0 || (value != 0.0 && value.abs() < 0.001) {
        format!("{value:.2e}")
    } else {
        format!("{value:.3}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plot_rect_preserves_coordinate_aspect_ratio() {
        let outer = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(400.0, 300.0));
        let square = fit_plot_rect(outer, [-1.0, 1.0, -1.0, 1.0]);
        assert!((square.width() - square.height()).abs() < 0.01);
        let wide = fit_plot_rect(outer, [-2.0, 2.0, -0.5, 0.5]);
        assert!((wide.width() / wide.height() - 4.0).abs() < 0.01);
    }
}
