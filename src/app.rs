use std::{
    collections::hash_map::DefaultHasher,
    hash::{Hash, Hasher},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, mpsc},
    thread,
};

use anyhow::Result;
use eframe::egui::{self, Color32, RichText, Sense, Stroke, StrokeKind};

use crate::{
    bridge::Bridge,
    catalog::scan_directory,
    protocol::{FileInfo, PlotData, PlotFile, ScanResult, read_plot},
    render::{PlotCallback, PlotHandle, PlotResources, Scale, SharedPlot},
};

enum Event {
    DirectoryChosen(Option<PathBuf>),
    Scan(Result<ScanResult>),
    Inspect {
        path: String,
        result: Result<FileInfo>,
    },
    Plot {
        path: String,
        variable: String,
        result: Box<Result<PlotData>>,
    },
}

pub struct ViewerApp {
    bridge: Bridge,
    sender: mpsc::Sender<Event>,
    receiver: mpsc::Receiver<Event>,
    plot: PlotHandle,
    directory: Option<PathBuf>,
    recursive: bool,
    files: Vec<PlotFile>,
    selected_path: Option<String>,
    info: Option<FileInfo>,
    selected_variable: Option<String>,
    file_filter: String,
    variable_filter: String,
    choosing_run: bool,
    loading: bool,
    status: String,
}

impl ViewerApp {
    pub fn new(context: &eframe::CreationContext<'_>, initial_path: Option<PathBuf>) -> Self {
        let plot = Arc::new(Mutex::new(SharedPlot::default()));
        if let Some(render_state) = &context.wgpu_render_state {
            let resources = PlotResources::new(&render_state.device, render_state.target_format);
            render_state
                .renderer
                .write()
                .callback_resources
                .insert(resources);
        }
        let (sender, receiver) = mpsc::channel();
        let mut app = Self {
            bridge: Bridge::discover(),
            sender,
            receiver,
            plot,
            directory: None,
            recursive: false,
            files: Vec::new(),
            selected_path: None,
            info: None,
            selected_variable: None,
            file_filter: String::new(),
            variable_filter: String::new(),
            choosing_run: false,
            loading: false,
            status: "Choose a BATS-R-US output directory to begin".to_owned(),
        };
        if let Some(path) = initial_path {
            if path.is_dir() {
                app.scan(path);
            } else if is_plt_file(&path) {
                app.directory = path.parent().map(Path::to_path_buf);
                app.inspect(path.to_string_lossy().into_owned());
            }
        }
        app
    }

    fn choose_directory(&mut self) {
        self.choosing_run = true;
        self.status = "Choosing a run directory…".to_owned();
        let sender = self.sender.clone();
        let initial_directory = self.directory.clone();
        thread::spawn(move || {
            let mut dialog = rfd::AsyncFileDialog::new().set_title("Open BATS-R-US run");
            if let Some(directory) = initial_directory {
                dialog = dialog.set_directory(directory);
            }
            let selected =
                pollster::block_on(dialog.pick_folder()).map(|handle| handle.path().to_owned());
            let _ = sender.send(Event::DirectoryChosen(selected));
        });
    }

    fn scan(&mut self, directory: PathBuf) {
        self.directory = Some(directory.clone());
        self.files.clear();
        self.info = None;
        self.selected_path = None;
        self.selected_variable = None;
        self.loading = true;
        self.status = format!("Scanning {}…", directory.display());
        let sender = self.sender.clone();
        let recursive = self.recursive;
        thread::spawn(move || {
            let _ = sender.send(Event::Scan(scan_directory(&directory, recursive)));
        });
    }

    fn inspect(&mut self, path: String) {
        self.selected_path = Some(path.clone());
        self.info = None;
        self.selected_variable = None;
        self.loading = true;
        self.status = "Inspecting metadata…".to_owned();
        let bridge = self.bridge.clone();
        let sender = self.sender.clone();
        let request_path = path.clone();
        thread::spawn(move || {
            let result = bridge.inspect(Path::new(&request_path));
            let _ = sender.send(Event::Inspect { path, result });
        });
    }

    fn load_variable(&mut self, variable: String) {
        let Some(path) = self.selected_path.clone() else {
            return;
        };
        self.selected_variable = Some(variable.clone());
        self.loading = true;
        self.status = format!("Loading {variable}…");
        let bridge = self.bridge.clone();
        let sender = self.sender.clone();
        let request_path = path.clone();
        let request_variable = variable.clone();
        thread::spawn(move || {
            let output = exchange_path(&request_path, &request_variable);
            let result = bridge
                .export(Path::new(&request_path), &request_variable, &output)
                .and_then(|()| read_plot(&output));
            let _ = sender.send(Event::Plot {
                path,
                variable,
                result: Box::new(result),
            });
        });
    }

    fn poll_events(&mut self) {
        while let Ok(event) = self.receiver.try_recv() {
            match event {
                Event::DirectoryChosen(Some(directory)) => {
                    self.choosing_run = false;
                    self.scan(directory);
                }
                Event::DirectoryChosen(None) => {
                    self.choosing_run = false;
                    self.status = "Run selection canceled".to_owned();
                }
                Event::Scan(result) => match result {
                    Ok(scan) if scan.protocol == 1 => {
                        self.files = scan.files;
                        self.directory = Some(scan.directory.into());
                        self.loading = false;
                        self.status = format!("{} plot files", self.files.len());
                        if let Some(first) = self.files.first() {
                            self.inspect(first.path.clone());
                        }
                    }
                    Ok(scan) => self.fail(format!("Unsupported bridge protocol {}", scan.protocol)),
                    Err(error) => self.fail(error.to_string()),
                },
                Event::Inspect { path, result } if self.selected_path.as_deref() == Some(&path) => {
                    match result {
                        Ok(info) if info.protocol == 1 => {
                            let first_scalar = info
                                .variables
                                .iter()
                                .find(|variable| {
                                    !is_coordinate(&variable.source)
                                        && !is_coordinate(&variable.canonical)
                                })
                                .map(|variable| variable.canonical.clone());
                            self.info = Some(info);
                            self.loading = false;
                            self.status = "Metadata ready".to_owned();
                            if let Some(variable) = first_scalar {
                                self.load_variable(variable);
                            }
                        }
                        Ok(info) => {
                            self.fail(format!("Unsupported bridge protocol {}", info.protocol))
                        }
                        Err(error) => self.fail(error.to_string()),
                    }
                }
                Event::Plot {
                    path,
                    variable,
                    result,
                } if self.selected_path.as_deref() == Some(&path)
                    && self.selected_variable.as_deref() == Some(&variable) =>
                {
                    match *result {
                        Ok(data) => {
                            let points = data.header.point_count;
                            let triangles = data.header.triangle_count;
                            self.plot.lock().unwrap().set_data(data);
                            self.loading = false;
                            self.status = format!("{points} points · {triangles} triangles");
                        }
                        Err(error) => self.fail(error.to_string()),
                    }
                }
                _ => {}
            }
        }
    }

    fn fail(&mut self, message: String) {
        self.loading = false;
        self.status = format!("Error: {message}");
    }

    fn adjacent_file(&mut self, offset: isize) {
        let Some(selected) = self.selected_path.as_deref() else {
            return;
        };
        let Some(index) = self.files.iter().position(|file| file.path == selected) else {
            return;
        };
        let current = &self.files[index];
        let candidates: Vec<&PlotFile> = self
            .files
            .iter()
            .filter(|file| file.section == current.section && file.var_id == current.var_id)
            .collect();
        let Some(position) = candidates.iter().position(|file| file.path == selected) else {
            return;
        };
        let next = (position as isize + offset).clamp(0, candidates.len() as isize - 1) as usize;
        if next != position {
            self.inspect(candidates[next].path.clone());
        }
    }

    fn top_bar(&mut self, root: &mut egui::Ui) {
        egui::Panel::top("top_bar").show(root, |ui| {
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(!self.choosing_run, egui::Button::new("Open run…"))
                    .clicked()
                {
                    self.choose_directory();
                }
                let changed = ui.checkbox(&mut self.recursive, "Recursive").changed();
                if changed && let Some(directory) = self.directory.clone() {
                    self.scan(directory);
                }
                ui.separator();
                if ui.button("◀").on_hover_text("Previous timestep").clicked() {
                    self.adjacent_file(-1);
                }
                if ui.button("▶").on_hover_text("Next timestep").clicked() {
                    self.adjacent_file(1);
                }
                if let Some(path) = &self.selected_path
                    && let Some(file) = self.files.iter().find(|file| &file.path == path)
                {
                    ui.label(
                        RichText::new(format!(
                            "{}  ·  t={}  ·  n={}",
                            file.section.as_deref().unwrap_or("unclassified"),
                            file.time_step.map_or_else(|| "—".into(), |v| v.to_string()),
                            file.dump_index
                                .map_or_else(|| "—".into(), |v| v.to_string()),
                        ))
                        .strong(),
                    );
                }
                if self.loading || self.choosing_run {
                    ui.spinner();
                }
            });
        });
    }

    fn file_panel(&mut self, root: &mut egui::Ui) {
        egui::Panel::left("files")
            .default_size(285.0)
            .resizable(true)
            .show(root, |ui| {
                ui.heading("Files");
                ui.add(
                    egui::TextEdit::singleline(&mut self.file_filter).hint_text("Filter files…"),
                );
                ui.separator();
                let filter = self.file_filter.to_lowercase();
                let visible: Vec<usize> = self
                    .files
                    .iter()
                    .enumerate()
                    .filter_map(|(index, file)| {
                        (filter.is_empty() || file.name.to_lowercase().contains(&filter))
                            .then_some(index)
                    })
                    .collect();
                let row_height = 42.0;
                egui::ScrollArea::vertical().show_rows(
                    ui,
                    row_height,
                    visible.len(),
                    |ui, range| {
                        for row in range {
                            let file = &self.files[visible[row]];
                            let selected = self.selected_path.as_deref() == Some(&file.path);
                            let label = if let Some(time) = file.time_step {
                                format!(
                                    "{}\nt={time}  ·  {:.1} MB",
                                    file.section.as_deref().unwrap_or("plot"),
                                    file.size as f64 / 1_048_576.0
                                )
                            } else {
                                format!("{}\n{:.1} MB", file.name, file.size as f64 / 1_048_576.0)
                            };
                            if ui.selectable_label(selected, label).clicked() {
                                self.inspect(file.path.clone());
                            }
                        }
                    },
                );
            });
    }

    fn controls_panel(&mut self, root: &mut egui::Ui) {
        egui::Panel::right("controls")
            .default_size(270.0)
            .resizable(true)
            .show(root, |ui| {
                ui.heading("Variables");
                ui.add(
                    egui::TextEdit::singleline(&mut self.variable_filter)
                        .hint_text("Search source or alias…"),
                );
                let mut requested = None;
                if let Some(info) = &self.info {
                    let filter = self.variable_filter.to_lowercase();
                    egui::ScrollArea::vertical()
                        .max_height(280.0)
                        .show(ui, |ui| {
                            for variable in &info.variables {
                                if is_coordinate(&variable.source) {
                                    continue;
                                }
                                let searchable =
                                    format!("{} {}", variable.source, variable.canonical)
                                        .to_lowercase();
                                if !filter.is_empty() && !searchable.contains(&filter) {
                                    continue;
                                }
                                let selected =
                                    self.selected_variable.as_deref() == Some(&variable.canonical);
                                let mut text = if variable.canonical == variable.source {
                                    variable.source.clone()
                                } else {
                                    format!("{}\n{}", variable.canonical, variable.source)
                                };
                                if let Some(unit) = &variable.unit {
                                    text.push_str(&format!("  [{unit}]"));
                                }
                                if ui.selectable_label(selected, text).clicked() {
                                    requested = Some(variable.canonical.clone());
                                }
                            }
                        });
                    ui.separator();
                    ui.label(RichText::new(&info.title).strong())
                        .on_hover_text(&info.path);
                    if let Some(section) = &info.section {
                        ui.small(format!("Section: {section}"));
                    }
                    if let Some(zone) = info.zones.first() {
                        ui.small(format!(
                            "Zone {}: {} · {}",
                            zone.index, zone.name, zone.zone_type
                        ));
                        ui.small(format!(
                            "{} points · {} elements",
                            zone.num_points, zone.num_elements
                        ));
                    }
                } else {
                    ui.label("Select a file to inspect its variables.");
                }
                if let Some(variable) = requested {
                    self.load_variable(variable);
                }

                ui.add_space(12.0);
                ui.heading("Display");
                let mut shared = self.plot.lock().unwrap();
                ui.horizontal(|ui| {
                    ui.selectable_value(&mut shared.display.scale, Scale::Linear, "Linear");
                    ui.selectable_value(&mut shared.display.scale, Scale::Logarithmic, "Log₁₀");
                });
                ui.label("Color limits");
                ui.horizontal(|ui| {
                    ui.add(egui::DragValue::new(&mut shared.display.limits[0]).speed(0.1));
                    ui.label("to");
                    ui.add(egui::DragValue::new(&mut shared.display.limits[1]).speed(0.1));
                });
                ui.add_space(8.0);
                ui.label("Axis limits");
                let mut view_bounds = shared.display.view_bounds;
                let bounds_changed = axis_limit_row(ui, "X", &mut view_bounds, 0, 1)
                    | axis_limit_row(ui, "Y", &mut view_bounds, 2, 3);
                if bounds_changed {
                    shared.set_view_bounds(view_bounds);
                }
                ui.horizontal(|ui| {
                    if ui.button("Zoom out").clicked() {
                        shared.zoom_view(1.25);
                    }
                    if ui.button("Zoom in").clicked() {
                        shared.zoom_view(0.8);
                    }
                    if ui.button("Fit").clicked() {
                        shared.reset_view();
                    }
                });
                ui.add_space(12.0);
                ui.small("Edit X/Y limits · drag to pan · wheel to zoom · double-click to fit");
            });
    }

    fn plot_panel(&mut self, root: &mut egui::Ui) {
        egui::CentralPanel::default().show(root, |ui| {
            let context = ui.ctx().clone();
            let available = ui.available_size();
            let plot_size = egui::vec2(
                (available.x - 72.0).max(100.0),
                (available.y - 38.0).max(100.0),
            );
            let (rect, response) = ui.allocate_exact_size(plot_size, Sense::click_and_drag());
            ui.painter()
                .rect_filled(rect, 2.0, Color32::from_rgb(15, 18, 24));
            ui.painter().rect_stroke(
                rect,
                2.0,
                Stroke::new(1.0, Color32::from_gray(70)),
                StrokeKind::Inside,
            );

            {
                let mut shared = self.plot.lock().unwrap();
                if response.dragged() {
                    let delta = ui.input(|input| input.pointer.delta());
                    shared.pan_view(-delta.x / rect.width(), delta.y / rect.height());
                    context.request_repaint();
                }
                if response.double_clicked() {
                    shared.reset_view();
                }
                if response.hovered() {
                    let scroll = context.input(|input| input.smooth_scroll_delta.y);
                    if scroll != 0.0 {
                        shared.zoom_view((-scroll * 0.002).exp());
                        context.request_repaint();
                    }
                }
            }

            let has_data = self.plot.lock().unwrap().data.is_some();
            if has_data {
                ui.painter()
                    .add(PlotCallback::paint_callback(rect, self.plot.clone()));
                self.paint_labels(ui, rect);
            } else {
                ui.painter().text(
                    rect.center(),
                    egui::Align2::CENTER_CENTER,
                    "Open a run and select a variable",
                    egui::FontId::proportional(18.0),
                    Color32::from_gray(160),
                );
            }
        });
    }

    fn paint_labels(&self, ui: &egui::Ui, rect: egui::Rect) {
        let shared = self.plot.lock().unwrap();
        let Some(data) = &shared.data else { return };
        let header = &data.header;
        ui.painter().text(
            rect.left_top() + egui::vec2(10.0, 10.0),
            egui::Align2::LEFT_TOP,
            &header.variable,
            egui::FontId::proportional(17.0),
            Color32::WHITE,
        );
        let source = if header.source_variable == header.variable {
            header.title.clone()
        } else {
            format!("{} · {}", header.source_variable, header.title)
        };
        let detail = format!(
            "{} · {}{}{}",
            source,
            header.zone,
            header
                .section
                .as_ref()
                .map_or(String::new(), |section| format!(" · {section}")),
            header
                .unit
                .as_ref()
                .map_or(String::new(), |unit| format!(" · {unit}")),
        );
        ui.painter().text(
            rect.left_top() + egui::vec2(10.0, 34.0),
            egui::Align2::LEFT_TOP,
            detail,
            egui::FontId::proportional(11.0),
            Color32::LIGHT_GRAY,
        );
        ui.painter().text(
            rect.right_bottom() - egui::vec2(8.0, 8.0),
            egui::Align2::RIGHT_BOTTOM,
            Path::new(&header.path)
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or(&header.path),
            egui::FontId::monospace(10.0),
            Color32::from_gray(120),
        );
        ui.painter().text(
            rect.center_bottom() + egui::vec2(0.0, 24.0),
            egui::Align2::CENTER_CENTER,
            &header.x_label,
            egui::FontId::proportional(13.0),
            Color32::LIGHT_GRAY,
        );
        ui.painter().text(
            rect.left_bottom() + egui::vec2(0.0, 4.0),
            egui::Align2::LEFT_TOP,
            format_value(shared.display.view_bounds[0]),
            egui::FontId::monospace(10.0),
            Color32::from_gray(150),
        );
        ui.painter().text(
            rect.right_bottom() + egui::vec2(0.0, 4.0),
            egui::Align2::RIGHT_TOP,
            format_value(shared.display.view_bounds[1]),
            egui::FontId::monospace(10.0),
            Color32::from_gray(150),
        );
        ui.painter().text(
            rect.left_center() + egui::vec2(-8.0, 0.0),
            egui::Align2::RIGHT_CENTER,
            &header.y_label,
            egui::FontId::proportional(13.0),
            Color32::LIGHT_GRAY,
        );
        ui.painter().text(
            rect.left_top() - egui::vec2(6.0, 0.0),
            egui::Align2::RIGHT_TOP,
            format_value(shared.display.view_bounds[3]),
            egui::FontId::monospace(10.0),
            Color32::from_gray(150),
        );
        ui.painter().text(
            rect.left_bottom() - egui::vec2(6.0, 0.0),
            egui::Align2::RIGHT_BOTTOM,
            format_value(shared.display.view_bounds[2]),
            egui::FontId::monospace(10.0),
            Color32::from_gray(150),
        );
        let bar = egui::Rect::from_min_max(
            egui::pos2(rect.right() + 14.0, rect.top()),
            egui::pos2(rect.right() + 32.0, rect.bottom()),
        );
        for step in 0..64 {
            let top = bar.top() + bar.height() * step as f32 / 64.0;
            let bottom = bar.top() + bar.height() * (step + 1) as f32 / 64.0;
            ui.painter().rect_filled(
                egui::Rect::from_min_max(
                    egui::pos2(bar.left(), top),
                    egui::pos2(bar.right(), bottom),
                ),
                0.0,
                turbo_color(1.0 - step as f32 / 63.0),
            );
        }
        ui.painter().text(
            bar.right_top() + egui::vec2(5.0, 0.0),
            egui::Align2::LEFT_TOP,
            format_value(shared.display.limits[1]),
            egui::FontId::monospace(11.0),
            Color32::LIGHT_GRAY,
        );
        ui.painter().text(
            bar.right_bottom() + egui::vec2(5.0, 0.0),
            egui::Align2::LEFT_BOTTOM,
            format_value(shared.display.limits[0]),
            egui::FontId::monospace(11.0),
            Color32::LIGHT_GRAY,
        );
    }
}

fn axis_limit_row(
    ui: &mut egui::Ui,
    label: &str,
    bounds: &mut [f32; 4],
    low: usize,
    high: usize,
) -> bool {
    let speed = ((bounds[high] - bounds[low]).abs() / 200.0).max(1.0e-6);
    ui.horizontal(|ui| {
        ui.label(label);
        let low_changed = ui
            .add(egui::DragValue::new(&mut bounds[low]).speed(speed))
            .changed();
        ui.label("to");
        let high_changed = ui
            .add(egui::DragValue::new(&mut bounds[high]).speed(speed))
            .changed();
        low_changed || high_changed
    })
    .inner
}

impl eframe::App for ViewerApp {
    fn ui(&mut self, root: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.poll_events();
        let dropped: Vec<PathBuf> = root.ctx().input(|input| {
            input
                .raw
                .dropped_files
                .iter()
                .filter_map(|file| file.path.clone())
                .collect()
        });
        if let Some(path) = dropped.first() {
            if path.is_dir() {
                self.scan(path.clone());
            } else if is_plt_file(path) {
                if let Some(parent) = path.parent() {
                    self.directory = Some(parent.to_owned());
                }
                self.inspect(path.to_string_lossy().into_owned());
            }
        }
        self.top_bar(root);
        self.file_panel(root);
        self.controls_panel(root);
        egui::Panel::bottom("status").show(root, |ui| {
            ui.horizontal(|ui| {
                ui.small(&self.status);
                if let Some(directory) = &self.directory {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.small(directory.display().to_string());
                    });
                }
            });
        });
        self.plot_panel(root);
    }
}

fn is_plt_file(path: &Path) -> bool {
    path.extension()
        .and_then(|value| value.to_str())
        .is_some_and(|value| value.eq_ignore_ascii_case("plt"))
}

fn exchange_path(path: &str, variable: &str) -> PathBuf {
    let mut hasher = DefaultHasher::new();
    path.hash(&mut hasher);
    variable.hash(&mut hasher);
    std::env::temp_dir().join(format!("batsview-{:016x}.bpv", hasher.finish()))
}

fn is_coordinate(name: &str) -> bool {
    let compact = name.to_lowercase().replace(' ', "");
    matches!(compact.as_str(), "x" | "y" | "z")
        || compact.starts_with("x[")
        || compact.starts_with("y[")
        || compact.starts_with("z[")
}

fn format_value(value: f32) -> String {
    if value.abs() >= 10_000.0 || (value != 0.0 && value.abs() < 0.001) {
        format!("{value:.2e}")
    } else {
        format!("{value:.3}")
    }
}

fn turbo_color(value: f32) -> Color32 {
    let x = value.clamp(0.0, 1.0);
    let channel = |coefficients: [f32; 6]| {
        let result = coefficients
            .iter()
            .rev()
            .fold(0.0, |sum, &coefficient| sum * x + coefficient);
        (result.clamp(0.0, 1.0) * 255.0) as u8
    };
    Color32::from_rgb(
        channel([
            0.13572138, 4.6153926, -42.660324, 132.13109, -152.9424, 59.28638,
        ]),
        channel([
            0.09140261, 2.1941884, 4.8429666, -14.185033, 4.2772985, 2.829566,
        ]),
        channel([
            0.1066733, 12.641946, -60.582047, 110.36277, -89.90311, 27.34825,
        ]),
    )
}
