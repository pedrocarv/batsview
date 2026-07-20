use eframe::egui::{self, Color32, FontId, Stroke, StrokeKind};

use crate::{
    render::DisplayState,
    scene::{
        AppearanceSettings, DaysideDirection2d, View2dSettings, colorbar_ticks, normalized_value,
    },
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
        FontId::proportional(24.0),
        foreground,
    );
    ui.painter().text(
        export_rect.left_top() + egui::vec2(18.0, 46.0),
        egui::Align2::LEFT_TOP,
        &chrome.subtitle,
        FontId::proportional(13.5),
        muted,
    );
    ui.painter().text(
        plot_rect.center_bottom() + egui::vec2(0.0, 31.0),
        egui::Align2::CENTER_CENTER,
        &chrome.x_label,
        FontId::proportional(14.5),
        foreground,
    );
    ui.painter().text(
        plot_rect.left_center() - egui::vec2(42.0, 0.0),
        egui::Align2::CENTER_CENTER,
        &chrome.y_label,
        FontId::proportional(14.5),
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
            FontId::monospace(12.0),
            muted,
        );
    }

    let bar = colorbar_rect_2d(plot_rect);
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
                FontId::monospace(12.5),
                foreground,
            );
        }
    }
    if let Some(unit) = &chrome.unit {
        ui.painter().text(
            bar.center_top() - egui::vec2(0.0, 8.0),
            egui::Align2::CENTER_BOTTOM,
            unit,
            FontId::proportional(12.5),
            muted,
        );
    }
    ui.painter().text(
        export_rect.right_bottom() - egui::vec2(12.0, 10.0),
        egui::Align2::RIGHT_BOTTOM,
        &chrome.filename,
        FontId::monospace(10.5),
        muted,
    );
}

pub fn colorbar_rect_2d(plot_rect: egui::Rect) -> egui::Rect {
    let height = (plot_rect.height() * 0.68)
        .clamp(60.0, 360.0)
        .min(plot_rect.height());
    egui::Rect::from_center_size(
        egui::pos2(plot_rect.right() + 27.0, plot_rect.center().y),
        egui::vec2(14.0, height),
    )
}

pub fn colorbar_rect_3d(frame_rect: egui::Rect) -> egui::Rect {
    let height = (frame_rect.height() * 0.56)
        .clamp(60.0, 340.0)
        .min(frame_rect.height());
    egui::Rect::from_center_size(
        egui::pos2(frame_rect.right() - 82.0, frame_rect.center().y + 10.0),
        egui::vec2(13.0, height),
    )
}

pub fn paint_reference_bodies_2d(
    ui: &egui::Ui,
    plot_rect: egui::Rect,
    bounds: [f32; 4],
    x_label: &str,
    y_label: &str,
    settings: &View2dSettings,
) {
    let spans = [bounds[1] - bounds[0], bounds[3] - bounds[2]];
    if spans
        .into_iter()
        .any(|span| !span.is_finite() || span <= 0.0)
    {
        return;
    }
    let center = egui::pos2(
        plot_rect.left() + (-bounds[0] / spans[0]) * plot_rect.width(),
        plot_rect.bottom() - (-bounds[2] / spans[1]) * plot_rect.height(),
    );
    let pixels_per_re = (plot_rect.width() / spans[0])
        .min(plot_rect.height() / spans[1])
        .abs();
    let painter = ui.painter().with_clip_rect(plot_rect);

    if settings.show_inner_boundary
        && settings.inner_boundary_radius.is_finite()
        && settings.inner_boundary_radius > 0.0
    {
        let radius = settings.inner_boundary_radius * pixels_per_re;
        if radius >= 0.5 {
            painter.circle_filled(center, radius, Color32::from_rgb(91, 101, 113));
            painter.circle_stroke(
                center,
                radius,
                Stroke::new(1.2, Color32::from_rgb(176, 187, 199)),
            );
        }
    }

    if !settings.show_earth || !settings.earth_radius.is_finite() || settings.earth_radius <= 0.0 {
        return;
    }
    let radius = settings.earth_radius * pixels_per_re;
    if radius < 0.5 {
        return;
    }
    painter.circle_filled(center, radius, Color32::BLACK);

    let dayside = dayside_screen_direction(x_label, y_label, settings.dayside_direction);
    let perpendicular = egui::vec2(-dayside.y, dayside.x);
    let mut white_half = Vec::with_capacity(27);
    white_half.push(center);
    for index in 0..=24 {
        let angle = -std::f32::consts::FRAC_PI_2 + std::f32::consts::PI * index as f32 / 24.0;
        white_half.push(center + (dayside * angle.cos() + perpendicular * angle.sin()) * radius);
    }
    painter.add(egui::Shape::convex_polygon(
        white_half,
        Color32::WHITE,
        Stroke::new(0.0, Color32::TRANSPARENT),
    ));
    painter.line_segment(
        [
            center - perpendicular * radius,
            center + perpendicular * radius,
        ],
        Stroke::new(1.0, Color32::from_gray(125)),
    );
    painter.circle_stroke(center, radius, Stroke::new(1.2, Color32::from_gray(205)));
}

fn coordinate_axis(label: &str) -> Option<char> {
    label
        .trim_start()
        .chars()
        .next()
        .map(|axis| axis.to_ascii_lowercase())
        .filter(|axis| matches!(axis, 'x' | 'y' | 'z'))
}

fn dayside_screen_direction(
    x_label: &str,
    y_label: &str,
    direction: DaysideDirection2d,
) -> egui::Vec2 {
    let positive_x = if coordinate_axis(x_label) == Some('x') {
        egui::vec2(1.0, 0.0)
    } else if coordinate_axis(y_label) == Some('x') {
        egui::vec2(0.0, -1.0)
    } else {
        egui::vec2(1.0, 0.0)
    };
    match direction {
        DaysideDirection2d::PositiveX => positive_x,
        DaysideDirection2d::NegativeX => -positive_x,
    }
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

    #[test]
    fn colorbars_are_compact_and_centered() {
        let plot = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(600.0, 500.0));
        let bar_2d = colorbar_rect_2d(plot);
        assert_eq!(bar_2d.width(), 14.0);
        assert!(bar_2d.height() < plot.height());
        assert!((bar_2d.center().y - plot.center().y).abs() < 0.01);

        let bar_3d = colorbar_rect_3d(plot);
        assert_eq!(bar_3d.width(), 13.0);
        assert!(bar_3d.height() < plot.height());
    }

    #[test]
    fn earth_dayside_follows_configured_x_direction() {
        assert_eq!(
            dayside_screen_direction("X [Re]", "Y [Re]", DaysideDirection2d::PositiveX),
            egui::vec2(1.0, 0.0)
        );
        assert_eq!(
            dayside_screen_direction("Y [Re]", "X [Re]", DaysideDirection2d::PositiveX),
            egui::vec2(0.0, -1.0)
        );
        assert_eq!(
            dayside_screen_direction("X [Re]", "Y [Re]", DaysideDirection2d::NegativeX),
            egui::vec2(-1.0, 0.0)
        );
    }
}
