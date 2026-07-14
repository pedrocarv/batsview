use std::{
    collections::hash_map::DefaultHasher,
    fs,
    hash::{Hash, Hasher},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, mpsc},
    thread,
    time::Duration,
};

use anyhow::{Context, Result};
use eframe::egui::{
    self, Color32, FontId, KeyboardShortcut, Modifiers, RichText, Sense, Stroke, StrokeKind,
};
use serde::{Deserialize, Serialize};

use crate::{
    annotations::{AnnotationEditor, DrawingTool},
    bridge::Bridge,
    catalog::scan_directory,
    export::{ExportBackground, ExportFrame, ExportSettings, render_plot_png},
    plot_ui::{PlotChrome, PlotColors, fit_plot_rect, paint_plot_chrome, sample_appearance},
    protocol::{FileInfo, PlotData, PlotFile, ScanResult, read_plot},
    render::{PlotCallback, PlotHandle, PlotResources, SharedPlot},
    scene::{
        AnnotationGeometry, AnnotationScope, AppearanceSettings, ColorMode, ColorbarTick, Colormap,
        DashStyle, NumberFormat, RgbaColor, Scale, SceneDocument, ScopeContext, TickMode,
        TitleConfig, TitleContext, render_title, validate_custom_ticks,
    },
};

const APP_STORAGE_KEY: &str = "batsview-app-state-v1";
const ACCENT: Color32 = Color32::from_rgb(70, 160, 235);
const PANEL_BG: Color32 = Color32::from_rgb(15, 22, 31);
const DEEP_BG: Color32 = Color32::from_rgb(9, 14, 21);
const MUTED: Color32 = Color32::from_rgb(145, 158, 173);

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
    SceneSaved(Result<Option<PathBuf>>),
    SceneLoaded(Result<Option<(PathBuf, SceneDocument)>>),
    ExportPathChosen {
        path: Option<PathBuf>,
        settings: ExportSettings,
    },
    ImageSaved(Result<PathBuf>),
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum InspectorTab {
    #[default]
    Data,
    Appearance,
    Annotations,
    Metadata,
}

impl InspectorTab {
    const ALL: [Self; 4] = [
        Self::Data,
        Self::Appearance,
        Self::Annotations,
        Self::Metadata,
    ];

    fn name(self) -> &'static str {
        match self {
            Self::Data => "Data",
            Self::Appearance => "Appearance",
            Self::Annotations => "Annotations",
            Self::Metadata => "Metadata",
        }
    }
}

#[derive(Clone, Copy)]
enum ToolbarIcon {
    Drawing(DrawingTool),
    FitView,
    Undo,
    Redo,
}

#[derive(Clone, Serialize, Deserialize)]
struct StoredRunScene {
    key: String,
    directory: String,
    scene: SceneDocument,
}

#[derive(Default, Serialize, Deserialize)]
struct PersistedAppState {
    #[serde(default)]
    recursive: bool,
    #[serde(default)]
    recent_runs: Vec<StoredRunScene>,
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
    io_busy: bool,
    status: String,
    scene: SceneDocument,
    stored_runs: Vec<StoredRunScene>,
    current_run_key: Option<String>,
    editor: AnnotationEditor,
    inspector_tab: InspectorTab,
    show_export_dialog: bool,
    export_settings: ExportSettings,
    render_state: Option<eframe::egui_wgpu::RenderState>,
    last_export_rect: Option<egui::Rect>,
}

impl ViewerApp {
    pub fn new(context: &eframe::CreationContext<'_>, initial_path: Option<PathBuf>) -> Self {
        configure_style(&context.egui_ctx);
        let plot = Arc::new(Mutex::new(SharedPlot::default()));
        if let Some(render_state) = &context.wgpu_render_state {
            let resources = PlotResources::new(
                &render_state.device,
                &render_state.queue,
                render_state.target_format,
            );
            render_state
                .renderer
                .write()
                .callback_resources
                .insert(resources);
        }
        let persisted: PersistedAppState = context
            .storage
            .and_then(|storage| eframe::get_value(storage, APP_STORAGE_KEY))
            .unwrap_or_default();
        let (sender, receiver) = mpsc::channel();
        let mut app = Self {
            bridge: Bridge::discover(),
            sender,
            receiver,
            plot,
            directory: None,
            recursive: persisted.recursive,
            files: Vec::new(),
            selected_path: None,
            info: None,
            selected_variable: None,
            file_filter: String::new(),
            variable_filter: String::new(),
            choosing_run: false,
            loading: false,
            io_busy: false,
            status: "Choose a BATS-R-US output directory to begin".to_owned(),
            scene: SceneDocument::default(),
            stored_runs: persisted.recent_runs,
            current_run_key: None,
            editor: AnnotationEditor::default(),
            inspector_tab: InspectorTab::Data,
            show_export_dialog: false,
            export_settings: ExportSettings::default(),
            render_state: context.wgpu_render_state.clone(),
            last_export_rect: None,
        };
        if let Some(path) = initial_path {
            if path.is_dir() {
                app.scan(path);
            } else if is_plt_file(&path) {
                if let Some(parent) = path.parent() {
                    let directory = parent.canonicalize().unwrap_or_else(|_| parent.to_owned());
                    app.activate_scene(directory.to_string_lossy().into_owned());
                    app.directory = Some(directory);
                }
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

    fn poll_events(&mut self, context: &egui::Context) {
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
                        self.activate_scene(scan.directory.clone());
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
                            self.sync_plot_appearance();
                            self.loading = false;
                            self.status = format!("{points} points · {triangles} triangles");
                        }
                        Err(error) => self.fail(error.to_string()),
                    }
                }
                Event::SceneSaved(result) => {
                    self.io_busy = false;
                    match result {
                        Ok(Some(path)) => self.status = format!("Scene saved · {}", path.display()),
                        Ok(None) => self.status = "Scene save canceled".to_owned(),
                        Err(error) => self.fail(error.to_string()),
                    }
                }
                Event::SceneLoaded(result) => {
                    self.io_busy = false;
                    match result {
                        Ok(Some((path, scene))) => {
                            self.editor.checkpoint(&self.scene);
                            self.scene = scene;
                            self.editor.selected = None;
                            self.sync_plot_appearance();
                            self.status = format!("Scene loaded · {}", path.display());
                        }
                        Ok(None) => self.status = "Scene load canceled".to_owned(),
                        Err(error) => self.fail(error.to_string()),
                    }
                }
                Event::ExportPathChosen { path, settings } => {
                    self.io_busy = false;
                    if let Some(path) = path {
                        if let Some(frame) =
                            self.export_frame(path, settings, context.pixels_per_point())
                        {
                            self.io_busy = true;
                            self.status = "Rendering PNG…".to_owned();
                            let sender = self.sender.clone();
                            thread::spawn(move || {
                                let _ = sender.send(Event::ImageSaved(render_plot_png(frame)));
                            });
                        } else {
                            self.fail(
                                "No GPU plot is available to export; load a variable first"
                                    .to_owned(),
                            );
                        }
                    } else {
                        self.status = "Image export canceled".to_owned();
                    }
                }
                Event::ImageSaved(result) => {
                    self.io_busy = false;
                    match result {
                        Ok(path) => self.status = format!("Image saved · {}", path.display()),
                        Err(error) => self.fail(error.to_string()),
                    }
                }
                _ => {}
            }
        }
    }

    fn fail(&mut self, message: String) {
        self.loading = false;
        self.io_busy = false;
        self.status = format!("Error: {message}");
    }

    fn sync_plot_appearance(&mut self) {
        let appearance = self.scene.appearance_for(self.selected_variable.as_deref());
        self.plot.lock().unwrap().set_appearance(&appearance);
    }

    fn stash_current_scene(&mut self) {
        let Some(key) = self.current_run_key.clone() else {
            return;
        };
        self.stored_runs.retain(|stored| stored.key != key);
        self.stored_runs.insert(
            0,
            StoredRunScene {
                key,
                directory: self
                    .directory
                    .as_ref()
                    .map_or_else(String::new, |path| path.to_string_lossy().into_owned()),
                scene: self.scene.clone(),
            },
        );
        self.stored_runs.truncate(20);
    }

    fn activate_scene(&mut self, directory: String) {
        let key = run_key(&directory);
        if self.current_run_key.as_deref() == Some(&key) {
            return;
        }
        self.stash_current_scene();
        let restored = self
            .stored_runs
            .iter()
            .position(|stored| stored.key == key)
            .map(|index| self.stored_runs.remove(index).scene);
        self.scene = restored.unwrap_or_default();
        if self.scene.source_run.is_none() {
            self.scene.source_run = Path::new(&directory)
                .file_name()
                .and_then(|name| name.to_str())
                .map(str::to_owned);
        }
        self.current_run_key = Some(key);
        self.editor = AnnotationEditor::default();
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

    fn save_scene_dialog(&mut self) {
        self.io_busy = true;
        let scene = self.scene.clone();
        let sender = self.sender.clone();
        let directory = self.directory.clone();
        thread::spawn(move || {
            let mut dialog = rfd::AsyncFileDialog::new()
                .set_title("Save BATSView scene")
                .add_filter("BATSView scene", &["json"])
                .set_file_name("batsview-scene.json");
            if let Some(directory) = directory {
                dialog = dialog.set_directory(directory);
            }
            let handle = pollster::block_on(dialog.save_file());
            let result = handle
                .map(|handle| {
                    let path = handle.path().to_owned();
                    let json = serde_json::to_vec_pretty(&scene).context("serializing scene")?;
                    fs::write(&path, json)
                        .with_context(|| format!("saving scene to {}", path.display()))?;
                    Ok(path)
                })
                .transpose();
            let _ = sender.send(Event::SceneSaved(result));
        });
    }

    fn load_scene_dialog(&mut self) {
        self.io_busy = true;
        let sender = self.sender.clone();
        let directory = self.directory.clone();
        thread::spawn(move || {
            let mut dialog = rfd::AsyncFileDialog::new()
                .set_title("Load BATSView scene")
                .add_filter("BATSView scene", &["json"]);
            if let Some(directory) = directory {
                dialog = dialog.set_directory(directory);
            }
            let handle = pollster::block_on(dialog.pick_file());
            let result = handle
                .map(|handle| {
                    let path = handle.path().to_owned();
                    let bytes = fs::read(&path)
                        .with_context(|| format!("reading scene from {}", path.display()))?;
                    let scene: SceneDocument =
                        serde_json::from_slice(&bytes).context("parsing BATSView scene")?;
                    scene.validate()?;
                    Ok((path, scene))
                })
                .transpose();
            let _ = sender.send(Event::SceneLoaded(result));
        });
    }

    fn export_dialog(&mut self) {
        self.io_busy = true;
        let sender = self.sender.clone();
        let directory = self.directory.clone();
        let settings = self.export_settings;
        let variable = self
            .selected_variable
            .clone()
            .unwrap_or_else(|| "plot".into());
        thread::spawn(move || {
            let mut dialog = rfd::AsyncFileDialog::new()
                .set_title("Save BATSView plot")
                .add_filter("PNG image", &["png"])
                .set_file_name(format!("{}.png", safe_filename(&variable)));
            if let Some(directory) = directory {
                dialog = dialog.set_directory(directory);
            }
            let path =
                pollster::block_on(dialog.save_file()).map(|handle| handle.path().to_owned());
            let _ = sender.send(Event::ExportPathChosen { path, settings });
        });
    }

    fn shortcuts(&mut self, context: &egui::Context) {
        let command = Modifiers::COMMAND;
        if context.input_mut(|input| {
            input.consume_shortcut(&KeyboardShortcut::new(command, egui::Key::O))
        }) {
            self.choose_directory();
        }
        if context.input_mut(|input| {
            input.consume_shortcut(&KeyboardShortcut::new(command, egui::Key::S))
        }) {
            self.save_scene_dialog();
        }
        if context.input_mut(|input| {
            input.consume_shortcut(&KeyboardShortcut::new(command, egui::Key::E))
        }) {
            self.show_export_dialog = self.plot.lock().unwrap().data.is_some();
        }
        let undo = context.input_mut(|input| {
            input.consume_shortcut(&KeyboardShortcut::new(command, egui::Key::Z))
        });
        let redo = context.input_mut(|input| {
            input.consume_shortcut(&KeyboardShortcut::new(
                command | Modifiers::SHIFT,
                egui::Key::Z,
            )) || input.consume_shortcut(&KeyboardShortcut::new(command, egui::Key::Y))
        });
        if undo {
            self.editor.undo(&mut self.scene);
            self.sync_plot_appearance();
        }
        if redo {
            self.editor.redo(&mut self.scene);
            self.sync_plot_appearance();
        }
        if !context.egui_wants_keyboard_input() {
            if context.input(|input| {
                input.key_pressed(egui::Key::Delete) || input.key_pressed(egui::Key::Backspace)
            }) {
                self.editor.delete_selected(&mut self.scene);
            }
            if context.input(|input| input.key_pressed(egui::Key::F)) {
                self.plot.lock().unwrap().reset_view();
            }
        }
    }

    fn top_bar(&mut self, root: &mut egui::Ui) {
        egui::Panel::top("command_bar")
            .frame(
                egui::Frame::new()
                    .fill(PANEL_BG)
                    .inner_margin(egui::Margin::symmetric(14, 8))
                    .stroke(Stroke::new(1.0, Color32::from_rgb(29, 40, 53))),
            )
            .show(root, |ui| {
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        ui.spacing_mut().item_spacing.y = 0.0;
                        ui.label(
                            RichText::new("BATSView")
                                .size(20.0)
                                .strong()
                                .color(Color32::WHITE),
                        );
                        ui.label(RichText::new("SCIENTIFIC 2D VIEWER").size(9.0).color(MUTED));
                    });
                    ui.add_space(10.0);
                    ui.separator();
                    if ui
                        .add_enabled(
                            !self.choosing_run,
                            egui::Button::new(RichText::new("Open run").strong())
                                .fill(ACCENT)
                                .stroke(Stroke::NONE),
                        )
                        .on_hover_text("Open a BATS-R-US output directory  Ctrl/Cmd+O")
                        .clicked()
                    {
                        self.choose_directory();
                    }
                    ui.add_enabled_ui(self.directory.is_some(), |ui| {
                        ui.menu_button("Scene", |ui| {
                            if ui.button("Save scene...     Ctrl/Cmd+S").clicked() {
                                self.save_scene_dialog();
                                ui.close();
                            }
                            if ui.button("Load scene...").clicked() {
                                self.load_scene_dialog();
                                ui.close();
                            }
                        });
                    });
                    let has_plot = self.plot.lock().unwrap().data.is_some();
                    if ui
                        .add_enabled(has_plot, egui::Button::new("Export PNG"))
                        .on_hover_text("Export the plot as PNG  Ctrl/Cmd+E")
                        .clicked()
                    {
                        self.show_export_dialog = true;
                    }
                    ui.add_space(6.0);
                    ui.separator();
                    ui.label(RichText::new("TIMESTEP").size(9.5).strong().color(MUTED));
                    if ui
                        .button("Prev")
                        .on_hover_text("Previous timestep")
                        .clicked()
                    {
                        self.adjacent_file(-1);
                    }
                    if ui.button("Next").on_hover_text("Next timestep").clicked() {
                        self.adjacent_file(1);
                    }
                    if let Some(file) = self.selected_file() {
                        egui::Frame::new()
                            .fill(Color32::from_rgb(20, 30, 42))
                            .corner_radius(5)
                            .inner_margin(egui::Margin::symmetric(9, 4))
                            .show(ui, |ui| {
                                ui.label(
                                    RichText::new(format!(
                                        "{}  ·  t={}  ·  n={}",
                                        file.section.as_deref().unwrap_or("unclassified"),
                                        file.time_step
                                            .map_or_else(|| "-".into(), |value| value.to_string()),
                                        file.dump_index
                                            .map_or_else(|| "-".into(), |value| value.to_string()),
                                    ))
                                    .color(Color32::from_rgb(201, 211, 222)),
                                );
                            });
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if self.loading || self.choosing_run || self.io_busy {
                            ui.spinner();
                        }
                    });
                });
            });
    }

    fn file_panel(&mut self, root: &mut egui::Ui) {
        egui::Panel::left("run_explorer")
            .default_size(290.0)
            .min_size(240.0)
            .resizable(true)
            .frame(
                egui::Frame::new()
                    .fill(PANEL_BG)
                    .inner_margin(egui::Margin::symmetric(12, 12)),
            )
            .show(root, |ui| {
                section_heading(ui, "Run explorer");
                if let Some(directory) = &self.directory {
                    let display_name = directory
                        .file_name()
                        .and_then(|name| name.to_str())
                        .map(str::to_owned)
                        .unwrap_or_else(|| directory.to_string_lossy().into_owned());
                    ui.label(RichText::new(display_name).strong())
                        .on_hover_text(directory.display().to_string());
                } else {
                    ui.label(RichText::new("No run open").color(MUTED));
                }
                ui.horizontal(|ui| {
                    let changed = ui
                        .checkbox(&mut self.recursive, "Include subfolders")
                        .changed();
                    if changed && let Some(directory) = self.directory.clone() {
                        self.scan(directory);
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.small(format!("{} files", self.files.len()));
                    });
                });
                ui.add(
                    egui::TextEdit::singleline(&mut self.file_filter)
                        .hint_text("Filter filename or section…"),
                );
                ui.separator();
                let filter = self.file_filter.to_lowercase();
                let visible: Vec<usize> = self
                    .files
                    .iter()
                    .enumerate()
                    .filter_map(|(index, file)| {
                        let searchable =
                            format!("{} {}", file.name, file.section.as_deref().unwrap_or(""))
                                .to_lowercase();
                        (filter.is_empty() || searchable.contains(&filter)).then_some(index)
                    })
                    .collect();
                if visible.is_empty() {
                    ui.add_space(20.0);
                    ui.vertical_centered(|ui| {
                        ui.label(
                            RichText::new(if self.files.is_empty() {
                                "Open a run to browse .plt files"
                            } else {
                                "No files match the filter"
                            })
                            .color(MUTED),
                        );
                    });
                } else {
                    egui::ScrollArea::vertical().show_rows(ui, 54.0, visible.len(), |ui, range| {
                        for row in range {
                            let file = &self.files[visible[row]];
                            let selected = self.selected_path.as_deref() == Some(&file.path);
                            let primary = file.section.as_deref().unwrap_or(&file.name);
                            let secondary = if let Some(time) = file.time_step {
                                format!("t={time}  ·  {:.1} MB", file.size as f64 / 1_048_576.0)
                            } else {
                                format!("{:.1} MB", file.size as f64 / 1_048_576.0)
                            };
                            let label = format!("{primary}\n{secondary}");
                            if ui
                                .add_sized(
                                    [ui.available_width(), 48.0],
                                    egui::Button::selectable(selected, label),
                                )
                                .on_hover_text(&file.name)
                                .clicked()
                            {
                                self.inspect(file.path.clone());
                            }
                        }
                    });
                }
            });
    }

    fn inspector_panel(&mut self, root: &mut egui::Ui) {
        egui::Panel::right("inspector")
            .default_size(360.0)
            .min_size(320.0)
            .resizable(true)
            .frame(
                egui::Frame::new()
                    .fill(PANEL_BG)
                    .inner_margin(egui::Margin::symmetric(12, 12)),
            )
            .show(root, |ui| {
                ui.horizontal(|ui| {
                    let width = (ui.available_width() - 3.0 * ui.spacing().item_spacing.x) / 4.0;
                    for tab in InspectorTab::ALL {
                        if ui
                            .add_sized(
                                [width, 32.0],
                                egui::Button::selectable(self.inspector_tab == tab, tab.name()),
                            )
                            .clicked()
                        {
                            self.inspector_tab = tab;
                        }
                    }
                });
                ui.add_space(5.0);
                ui.separator();
                ui.add_space(4.0);
                egui::ScrollArea::vertical().show(ui, |ui| match self.inspector_tab {
                    InspectorTab::Data => self.data_inspector(ui),
                    InspectorTab::Appearance => self.appearance_inspector(ui),
                    InspectorTab::Annotations => self.annotation_inspector(ui),
                    InspectorTab::Metadata => self.metadata_inspector(ui),
                });
            });
    }

    fn data_inspector(&mut self, ui: &mut egui::Ui) {
        section_heading(ui, "Variables");
        ui.add(
            egui::TextEdit::singleline(&mut self.variable_filter)
                .hint_text("Search source name or alias…"),
        );
        let mut requested = None;
        if let Some(info) = &self.info {
            let filter = self.variable_filter.to_lowercase();
            egui::ScrollArea::vertical()
                .max_height(330.0)
                .show(ui, |ui| {
                    for variable in &info.variables {
                        if is_coordinate(&variable.source) {
                            continue;
                        }
                        let searchable =
                            format!("{} {}", variable.source, variable.canonical).to_lowercase();
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
                        if ui
                            .add_sized(
                                [
                                    ui.available_width(),
                                    if text.contains('\n') { 40.0 } else { 28.0 },
                                ],
                                egui::Button::selectable(selected, text),
                            )
                            .clicked()
                        {
                            requested = Some(variable.canonical.clone());
                        }
                    }
                });
        } else {
            ui.label(RichText::new("Select a file to inspect its variables.").color(MUTED));
        }
        if let Some(variable) = requested {
            self.load_variable(variable);
        }

        ui.add_space(14.0);
        section_heading(ui, "View limits");
        let mut shared = self.plot.lock().unwrap();
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
            if ui.button("+ Zoom").on_hover_text("Zoom in").clicked() {
                shared.zoom_view(0.8);
            }
            if ui.button("Fit").on_hover_text("Fit data  F").clicked() {
                shared.reset_view();
            }
        });
        ui.add_space(8.0);
        ui.small(
            RichText::new("Drag to pan · wheel to zoom · double-click or F to fit").color(MUTED),
        );
    }

    fn appearance_inspector(&mut self, ui: &mut egui::Ui) {
        let variable = self.selected_variable.clone();
        let mut override_enabled = variable
            .as_deref()
            .is_some_and(|name| self.scene.variable_overrides.contains_key(name));
        if let Some(variable) = variable.as_deref() {
            if ui
                .checkbox(
                    &mut override_enabled,
                    format!("Override appearance for {variable}"),
                )
                .changed()
            {
                self.editor.checkpoint(&self.scene);
                self.scene.set_variable_override(variable, override_enabled);
                self.sync_plot_appearance();
            }
        } else {
            ui.label(RichText::new("Run-wide defaults").color(MUTED));
        }

        let before = self.scene.appearance_for(variable.as_deref());
        let mut edited = before.clone();
        let effective_limits = self.plot.lock().unwrap().display.limits;

        ui.add_space(10.0);
        section_heading(ui, "Color mapping");
        egui::ComboBox::from_label("Colormap")
            .selected_text(edited.colormap.name())
            .show_ui(ui, |ui| {
                for map in Colormap::ALL {
                    ui.selectable_value(&mut edited.colormap, map, map.name());
                }
            });
        paint_colormap_preview(ui, &edited);
        ui.checkbox(&mut edited.reversed, "Reverse colormap");
        ui.horizontal(|ui| {
            ui.label("Rendering");
            if ui
                .selectable_label(edited.color_mode == ColorMode::Continuous, "Continuous")
                .clicked()
            {
                edited.color_mode = ColorMode::Continuous;
            }
            let discrete = matches!(edited.color_mode, ColorMode::Discrete { .. });
            if ui.selectable_label(discrete, "Discrete").clicked() && !discrete {
                edited.color_mode = ColorMode::Discrete { bins: 10 };
            }
        });
        if let ColorMode::Discrete { bins } = &mut edited.color_mode {
            ui.add(egui::Slider::new(bins, 2..=32).text("Bins"));
        }
        ui.horizontal(|ui| {
            ui.label("Scale");
            ui.selectable_value(&mut edited.scale, Scale::Linear, "Linear");
            ui.selectable_value(&mut edited.scale, Scale::Logarithmic, "Log10");
        });

        ui.add_space(10.0);
        section_heading(ui, "Color limits");
        let mut automatic = edited.color_limits.is_none();
        if ui
            .checkbox(&mut automatic, "Automatic data range")
            .changed()
        {
            edited.color_limits = if automatic {
                None
            } else {
                Some(effective_limits)
            };
        }
        if let Some(limits) = &mut edited.color_limits {
            let speed = color_limit_speed(*limits);
            ui.horizontal(|ui| {
                ui.add(egui::DragValue::new(&mut limits[0]).speed(speed));
                ui.label("to");
                ui.add(egui::DragValue::new(&mut limits[1]).speed(speed));
            });
            if !limits.iter().all(|value| value.is_finite()) || limits[1] <= limits[0] {
                ui.colored_label(
                    Color32::from_rgb(241, 126, 126),
                    "Upper limit must exceed the lower limit.",
                );
            }
            if edited.scale == Scale::Logarithmic && limits[0] <= 0.0 {
                ui.colored_label(
                    Color32::from_rgb(241, 126, 126),
                    "Logarithmic limits must be positive.",
                );
            }
        }

        ui.add_space(10.0);
        section_heading(ui, "Colorbar ticks");
        tick_controls(ui, &mut edited, effective_limits);

        ui.add_space(10.0);
        section_heading(ui, "Plot title");
        let rendered_title = self.preview_title(&edited.title).ok();
        title_controls(ui, &mut edited.title, rendered_title.as_deref());
        match self.preview_title(&edited.title) {
            Ok(title) => {
                ui.label(RichText::new("Preview").small().color(MUTED));
                ui.label(RichText::new(title).strong());
            }
            Err(error) => {
                ui.colored_label(Color32::from_rgb(241, 126, 126), error);
            }
        }
        ui.collapsing("Available title tokens", |ui| {
            ui.small("{variable}  {source}  {unit}  {section}  {time}  {dump}  {zone}  {file}  {run}  {dataset_title}");
        });

        if edited != before {
            self.editor.checkpoint(&self.scene);
            if let Some(variable) = variable.as_deref()
                && override_enabled
            {
                self.scene
                    .variable_overrides
                    .insert(variable.to_owned(), edited);
            } else {
                self.scene.run_defaults = edited;
            }
            self.sync_plot_appearance();
        }
    }

    fn annotation_inspector(&mut self, ui: &mut egui::Ui) {
        section_heading(ui, "Layers");
        let (section, variable, relative_path) = self.scope_values();
        let scope_context = ScopeContext {
            section: section.as_deref(),
            variable: variable.as_deref(),
            relative_path: relative_path.as_deref(),
        };
        if self.scene.annotations.is_empty() {
            ui.label(
                RichText::new("Use the drawing toolbar above the plot to add figures.")
                    .color(MUTED),
            );
        }
        for index in (0..self.scene.annotations.len()).rev() {
            let before = self.scene.annotations[index].clone();
            let mut edited = before.clone();
            let active = edited.scope.matches(&scope_context);
            ui.horizontal(|ui| {
                ui.checkbox(&mut edited.visible, "")
                    .on_hover_text("Visible");
                let selected = self.editor.selected == Some(edited.id);
                let label = if active {
                    edited.name.clone()
                } else {
                    format!("{}  · inactive", edited.name)
                };
                if ui
                    .selectable_label(
                        selected,
                        RichText::new(label).color(if active { Color32::WHITE } else { MUTED }),
                    )
                    .clicked()
                {
                    self.editor.selected = Some(edited.id);
                }
                ui.checkbox(&mut edited.locked, "Lock")
                    .on_hover_text("Lock layer");
            });
            if edited != before {
                self.editor.checkpoint(&self.scene);
                self.scene.annotations[index] = edited;
            }
        }

        ui.horizontal(|ui| {
            if ui
                .add_enabled(
                    self.editor.selected.is_some(),
                    egui::Button::new("Duplicate"),
                )
                .clicked()
            {
                self.editor.duplicate_selected(&mut self.scene);
            }
            if ui
                .add_enabled(self.editor.selected.is_some(), egui::Button::new("Delete"))
                .clicked()
            {
                self.editor.delete_selected(&mut self.scene);
            }
        });
        ui.horizontal(|ui| {
            if ui
                .add_enabled(
                    self.editor.selected.is_some(),
                    egui::Button::new("Backward"),
                )
                .clicked()
            {
                self.editor.send_backward(&mut self.scene);
            }
            if ui
                .add_enabled(self.editor.selected.is_some(), egui::Button::new("Forward"))
                .clicked()
            {
                self.editor.bring_forward(&mut self.scene);
            }
        });

        let Some(id) = self.editor.selected else {
            return;
        };
        let Some(before) = self
            .scene
            .annotations
            .iter()
            .find(|item| item.id == id)
            .cloned()
        else {
            return;
        };
        let mut edited = before.clone();
        ui.add_space(14.0);
        section_heading(ui, "Selected figure");
        ui.add(egui::TextEdit::singleline(&mut edited.name).hint_text("Layer name"));
        ui.label(RichText::new(edited.geometry.display_name()).color(MUTED));
        geometry_controls(ui, &mut edited.geometry);

        ui.add_space(8.0);
        ui.label("Stroke");
        ui.horizontal(|ui| {
            color_control(ui, &mut edited.style.stroke);
            ui.add(egui::Slider::new(&mut edited.style.stroke_width, 0.5..=20.0).text("Width"));
        });
        egui::ComboBox::from_label("Line style")
            .selected_text(match edited.style.dash {
                DashStyle::Solid => "Solid",
                DashStyle::Dashed => "Dashed",
                DashStyle::Dotted => "Dotted",
            })
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut edited.style.dash, DashStyle::Solid, "Solid");
                ui.selectable_value(&mut edited.style.dash, DashStyle::Dashed, "Dashed");
                ui.selectable_value(&mut edited.style.dash, DashStyle::Dotted, "Dotted");
            });
        let supports_fill = matches!(
            edited.geometry,
            AnnotationGeometry::Rectangle { .. }
                | AnnotationGeometry::Ellipse { .. }
                | AnnotationGeometry::Polygon { .. }
        );
        if supports_fill {
            let mut enabled = edited.style.fill.is_some();
            if ui.checkbox(&mut enabled, "Fill").changed() {
                edited.style.fill = enabled.then_some(RgbaColor([70, 160, 235, 64]));
            }
            if let Some(fill) = &mut edited.style.fill {
                color_control(ui, fill);
            }
        }
        if matches!(edited.geometry, AnnotationGeometry::Arrow { .. }) {
            ui.add(
                egui::Slider::new(&mut edited.style.arrowhead_size, 4.0..=40.0).text("Arrowhead"),
            );
        }
        if matches!(edited.geometry, AnnotationGeometry::Text { .. }) {
            ui.add(egui::Slider::new(&mut edited.style.text_size, 8.0..=72.0).text("Text size"));
        }

        ui.add_space(8.0);
        egui::ComboBox::from_label("Scope")
            .selected_text(edited.scope.label())
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut edited.scope, AnnotationScope::Run, "Whole run");
                if let Some(section) = &section {
                    ui.selectable_value(
                        &mut edited.scope,
                        AnnotationScope::Section {
                            section: section.clone(),
                        },
                        format!("Section · {section}"),
                    );
                }
                if let Some(variable) = &variable {
                    ui.selectable_value(
                        &mut edited.scope,
                        AnnotationScope::Variable {
                            variable: variable.clone(),
                        },
                        format!("Variable · {variable}"),
                    );
                }
                if let (Some(relative_path), Some(variable)) = (&relative_path, &variable) {
                    ui.selectable_value(
                        &mut edited.scope,
                        AnnotationScope::Plot {
                            relative_path: relative_path.clone(),
                            variable: variable.clone(),
                        },
                        "Selected plot only",
                    );
                }
            });

        if edited != before {
            self.editor.checkpoint(&self.scene);
            if let Some(annotation) = self.scene.annotations.iter_mut().find(|item| item.id == id) {
                *annotation = edited;
            }
        }
    }

    fn metadata_inspector(&mut self, ui: &mut egui::Ui) {
        section_heading(ui, "Dataset");
        if let Some(info) = &self.info {
            ui.label(RichText::new(&info.title).strong());
            ui.small(&info.path);
            if let Some(section) = &info.section {
                metadata_row(ui, "Section", section);
            }
            metadata_row(ui, "Variables", &info.variables.len().to_string());
            for zone in &info.zones {
                ui.add_space(8.0);
                ui.label(RichText::new(format!("Zone {} · {}", zone.index, zone.name)).strong());
                metadata_row(ui, "Type", &zone.zone_type);
                metadata_row(ui, "Points", &zone.num_points.to_string());
                metadata_row(ui, "Elements", &zone.num_elements.to_string());
            }
        } else {
            ui.label(RichText::new("Select a file to inspect its metadata.").color(MUTED));
        }
        let shared = self.plot.lock().unwrap();
        if let Some(data) = &shared.data {
            ui.add_space(14.0);
            section_heading(ui, "Loaded plot");
            metadata_row(ui, "Variable", &data.header.variable);
            metadata_row(ui, "Source", &data.header.source_variable);
            metadata_row(ui, "X axis", &data.header.x_label);
            metadata_row(ui, "Y axis", &data.header.y_label);
            metadata_row(ui, "Triangles", &data.header.triangle_count.to_string());
            metadata_row(ui, "Points", &data.header.point_count.to_string());
        }
    }

    fn plot_panel(&mut self, root: &mut egui::Ui) {
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(DEEP_BG))
            .show(root, |ui| {
                egui::Frame::new()
                    .fill(PANEL_BG)
                    .corner_radius(6)
                    .stroke(Stroke::new(1.0, Color32::from_rgb(31, 43, 57)))
                    .inner_margin(egui::Margin::symmetric(10, 6))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(RichText::new("TOOLS").size(9.5).strong().color(MUTED));
                            for tool in DrawingTool::ALL {
                                let tooltip = match tool {
                                    DrawingTool::Select => {
                                        "Select and move figures; drag empty space to pan"
                                    }
                                    DrawingTool::Polyline | DrawingTool::Polygon => {
                                        "Click vertices; double-click or Enter to finish; Escape to cancel"
                                    }
                                    DrawingTool::Text => "Click the plot to place text",
                                    DrawingTool::Ellipse => {
                                        "Draw an ellipse; hold Shift for a circle"
                                    }
                                    _ => "Drag on the plot; hold Shift to constrain",
                                };
                                if toolbar_icon_button(
                                    ui,
                                    ToolbarIcon::Drawing(tool),
                                    self.editor.tool == tool,
                                    true,
                                    &format!("{}: {tooltip}", tool.name()),
                                ) {
                                    self.editor.cancel_drawing();
                                    self.editor.tool = tool;
                                }
                            }
                            ui.separator();
                            if toolbar_icon_button(
                                ui,
                                ToolbarIcon::FitView,
                                false,
                                true,
                                "Fit data to view  F",
                            ) {
                                self.plot.lock().unwrap().reset_view();
                            }
                            if toolbar_icon_button(
                                ui,
                                ToolbarIcon::Undo,
                                false,
                                self.editor.can_undo(),
                                "Undo  Ctrl/Cmd+Z",
                            ) {
                                self.editor.undo(&mut self.scene);
                                self.sync_plot_appearance();
                            }
                            if toolbar_icon_button(
                                ui,
                                ToolbarIcon::Redo,
                                false,
                                self.editor.can_redo(),
                                "Redo  Ctrl/Cmd+Shift+Z",
                            ) {
                                self.editor.redo(&mut self.scene);
                                self.sync_plot_appearance();
                            }
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    ui.label(
                                        RichText::new("Wheel: zoom  ·  Drag: pan")
                                            .small()
                                            .color(MUTED),
                                    );
                                },
                            );
                        });
                    });
                ui.add_space(7.0);

                let available = ui.available_size();
                let (export_rect, _) = ui.allocate_exact_size(available, Sense::hover());
                self.last_export_rect = Some(export_rect);
                let export_background = ExportBackground::Dark;
                let canvas = export_background.canvas_color();
                let foreground = export_background.foreground();
                let muted = export_background.muted_foreground();
                ui.painter().rect_filled(export_rect, 6.0, canvas);

                let (data, display) = {
                    let shared = self.plot.lock().unwrap();
                    (shared.data.clone(), shared.display.clone())
                };
                let Some(data) = data else {
                    ui.painter().text(
                        export_rect.center(),
                        egui::Align2::CENTER_CENTER,
                        "Open a run and select a variable",
                        FontId::proportional(18.0),
                        muted,
                    );
                    return;
                };

                let chart_outer = egui::Rect::from_min_max(
                    export_rect.min + egui::vec2(64.0, 62.0),
                    export_rect.max - egui::vec2(112.0, 52.0),
                );
                let plot_rect = fit_plot_rect(chart_outer, display.view_bounds);
                let response = ui.interact(
                    plot_rect,
                    ui.id().with("plot_interaction"),
                    Sense::click_and_drag(),
                );
                ui.painter().rect_filled(plot_rect, 2.0, canvas);
                ui.painter().rect_stroke(
                    plot_rect,
                    2.0,
                    Stroke::new(1.0, muted.gamma_multiply(0.55)),
                    StrokeKind::Inside,
                );
                ui.painter().add(PlotCallback::paint_callback(plot_rect, self.plot.clone()));

                let (section, variable, relative_path) = self.scope_values();
                let scope = ScopeContext {
                    section: section.as_deref(),
                    variable: variable.as_deref(),
                    relative_path: relative_path.as_deref(),
                };
                let consumed = self.editor.interact(
                    ui,
                    &response,
                    plot_rect,
                    display.view_bounds,
                    &mut self.scene,
                    &scope,
                );
                if self.editor.tool == DrawingTool::Select && !consumed {
                    let mut shared = self.plot.lock().unwrap();
                    if response.dragged() {
                        let delta = ui.input(|input| input.pointer.delta());
                        shared.pan_view(-delta.x / plot_rect.width(), delta.y / plot_rect.height());
                    }
                    if response.double_clicked() {
                        shared.reset_view();
                    }
                }
                if response.hovered() {
                    let scroll = ui.input(|input| input.smooth_scroll_delta.y);
                    if scroll != 0.0 {
                        self.plot.lock().unwrap().zoom_view((-scroll * 0.002).exp());
                    }
                }
                self.editor.paint(
                    ui,
                    plot_rect,
                    display.view_bounds,
                    &self.scene,
                    &scope,
                    true,
                );
                let appearance = self.scene.appearance_for(self.selected_variable.as_deref());
                paint_plot_chrome(
                    ui,
                    export_rect,
                    plot_rect,
                    &self.plot_chrome(&data.header),
                    &display,
                    &appearance,
                    PlotColors { foreground, muted },
                );
            });
    }

    fn preview_title(&self, config: &TitleConfig) -> Result<String, String> {
        let shared = self.plot.lock().unwrap();
        let data = shared
            .data
            .as_ref()
            .ok_or_else(|| "Load a variable to preview the title".to_owned())?;
        self.title_with_header(config, &data.header)
    }

    fn plot_chrome(&self, header: &crate::protocol::PlotHeader) -> PlotChrome {
        let appearance = self.scene.appearance_for(self.selected_variable.as_deref());
        let title = self
            .title_with_header(&appearance.title, header)
            .unwrap_or_else(|_| header.variable.clone());
        let source = if header.source_variable == header.variable {
            header.title.clone()
        } else {
            format!("{} · {}", header.source_variable, header.title)
        };
        let subtitle = format!(
            "{} · {}{}{}",
            source,
            header.zone,
            header
                .section
                .as_ref()
                .map_or(String::new(), |value| format!(" · {value}")),
            header
                .unit
                .as_ref()
                .map_or(String::new(), |value| format!(" · {value}")),
        );
        PlotChrome {
            title,
            subtitle,
            x_label: header.x_label.clone(),
            y_label: header.y_label.clone(),
            unit: header.unit.clone(),
            filename: Path::new(&header.path)
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or(&header.path)
                .to_owned(),
        }
    }

    fn export_frame(
        &self,
        destination: PathBuf,
        settings: ExportSettings,
        pixels_per_point: f32,
    ) -> Option<ExportFrame> {
        let render_state = self.render_state.clone()?;
        let logical_size = self.last_export_rect?.size();
        let header = self
            .plot
            .lock()
            .unwrap()
            .data
            .as_ref()
            .map(|data| data.header.clone())?;
        let (scope_section, scope_variable, scope_relative_path) = self.scope_values();
        let appearance = self.scene.appearance_for(self.selected_variable.as_deref());
        Some(ExportFrame {
            render_state,
            plot: self.plot.clone(),
            scene: self.scene.clone(),
            scope_section,
            scope_variable,
            scope_relative_path,
            appearance,
            chrome: self.plot_chrome(&header),
            logical_size,
            pixels_per_point,
            settings,
            destination,
        })
    }

    fn title_with_header(
        &self,
        config: &TitleConfig,
        header: &crate::protocol::PlotHeader,
    ) -> Result<String, String> {
        let selected_file = self.selected_file();
        let file = Path::new(&header.path)
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or(&header.path);
        let run = self
            .directory
            .as_ref()
            .and_then(|path| path.file_name())
            .and_then(|name| name.to_str())
            .unwrap_or("");
        render_title(
            config,
            &TitleContext {
                variable: &header.variable,
                source: &header.source_variable,
                unit: header.unit.as_deref(),
                section: header.section.as_deref(),
                time: selected_file.and_then(|item| item.time_step),
                dump: selected_file.and_then(|item| item.dump_index),
                zone: &header.zone,
                file,
                run,
                dataset_title: &header.title,
            },
        )
    }

    fn selected_file(&self) -> Option<&PlotFile> {
        let path = self.selected_path.as_deref()?;
        self.files.iter().find(|file| file.path == path)
    }

    fn scope_values(&self) -> (Option<String>, Option<String>, Option<String>) {
        let section = self.selected_file().and_then(|file| file.section.clone());
        let variable = self.selected_variable.clone();
        let relative = self.selected_path.as_ref().map(|path| {
            let path = Path::new(path);
            self.directory
                .as_ref()
                .and_then(|directory| path.strip_prefix(directory).ok())
                .unwrap_or(path)
                .to_string_lossy()
                .replace('\\', "/")
        });
        (section, variable, relative)
    }

    fn export_options_window(&mut self, context: &egui::Context) {
        if !self.show_export_dialog {
            return;
        }
        let mut close = false;
        egui::Window::new("Save image")
            .collapsible(false)
            .resizable(false)
            .show(context, |ui| {
                ui.label("Export the plot, title, axes, colorbar, and annotations.");
                ui.add_space(6.0);
                egui::ComboBox::from_label("Resolution")
                    .selected_text(format!("{}x", self.export_settings.scale))
                    .show_ui(ui, |ui| {
                        for scale in [1, 2, 4] {
                            ui.selectable_value(
                                &mut self.export_settings.scale,
                                scale,
                                format!("{scale}x"),
                            );
                        }
                    });
                egui::ComboBox::from_label("Background")
                    .selected_text(self.export_settings.background.name())
                    .show_ui(ui, |ui| {
                        for background in ExportBackground::ALL {
                            ui.selectable_value(
                                &mut self.export_settings.background,
                                background,
                                background.name(),
                            );
                        }
                    });
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button("Cancel").clicked() {
                        close = true;
                    }
                    if ui.button("Choose file…").clicked() {
                        self.export_dialog();
                        close = true;
                    }
                });
            });
        if close {
            self.show_export_dialog = false;
        }
    }
}

impl eframe::App for ViewerApp {
    fn ui(&mut self, root: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let context = root.ctx().clone();
        self.poll_events(&context);
        self.shortcuts(&context);
        let dropped: Vec<PathBuf> = context.input(|input| {
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
                    let directory = parent.canonicalize().unwrap_or_else(|_| parent.to_owned());
                    self.activate_scene(directory.to_string_lossy().into_owned());
                    self.directory = Some(directory);
                }
                self.inspect(path.to_string_lossy().into_owned());
            }
        }
        self.top_bar(root);
        self.file_panel(root);
        self.inspector_panel(root);
        egui::Panel::bottom("status_bar")
            .frame(
                egui::Frame::new()
                    .fill(PANEL_BG)
                    .inner_margin(egui::Margin::symmetric(12, 5))
                    .stroke(Stroke::new(1.0, Color32::from_rgb(29, 40, 53))),
            )
            .show(root, |ui| {
                ui.horizontal(|ui| {
                    ui.small(&self.status);
                    if let Some(directory) = &self.directory {
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.small(RichText::new(directory.display().to_string()).color(MUTED));
                        });
                    }
                });
            });
        self.plot_panel(root);
        self.export_options_window(&context);
        if self.loading || self.choosing_run || self.io_busy {
            context.request_repaint_after(Duration::from_millis(40));
        }
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        self.stash_current_scene();
        let state = PersistedAppState {
            recursive: self.recursive,
            recent_runs: self.stored_runs.clone(),
        };
        eframe::set_value(storage, APP_STORAGE_KEY, &state);
    }
}

fn configure_style(context: &egui::Context) {
    context.set_theme(egui::Theme::Dark);
    let mut style = (*context.style_of(egui::Theme::Dark)).clone();
    style.text_styles.insert(
        egui::TextStyle::Heading,
        FontId::new(19.0, egui::FontFamily::Proportional),
    );
    style.text_styles.insert(
        egui::TextStyle::Body,
        FontId::new(13.5, egui::FontFamily::Proportional),
    );
    style.text_styles.insert(
        egui::TextStyle::Button,
        FontId::new(13.0, egui::FontFamily::Proportional),
    );
    style.text_styles.insert(
        egui::TextStyle::Small,
        FontId::new(11.5, egui::FontFamily::Proportional),
    );
    style.text_styles.insert(
        egui::TextStyle::Monospace,
        FontId::new(12.0, egui::FontFamily::Monospace),
    );
    style.spacing.item_spacing = egui::vec2(9.0, 8.0);
    style.spacing.button_padding = egui::vec2(11.0, 6.0);
    style.spacing.interact_size = egui::vec2(40.0, 30.0);
    style.spacing.indent = 20.0;
    style.spacing.slider_width = 140.0;
    style.spacing.combo_width = 130.0;
    let mut visuals = egui::Visuals::dark();
    visuals.panel_fill = PANEL_BG;
    visuals.window_fill = Color32::from_rgb(18, 26, 36);
    visuals.extreme_bg_color = Color32::from_rgb(7, 11, 17);
    visuals.faint_bg_color = Color32::from_rgb(22, 31, 42);
    visuals.selection.bg_fill = Color32::from_rgb(36, 93, 142);
    visuals.selection.stroke = Stroke::new(1.0, Color32::from_rgb(117, 201, 255));
    visuals.hyperlink_color = Color32::from_rgb(92, 200, 255);
    visuals.widgets.inactive.bg_fill = Color32::from_rgb(20, 29, 40);
    visuals.widgets.inactive.weak_bg_fill = Color32::from_rgb(20, 29, 40);
    visuals.widgets.inactive.bg_stroke = Stroke::new(1.0, Color32::from_rgb(42, 54, 69));
    visuals.widgets.hovered.bg_fill = Color32::from_rgb(31, 43, 57);
    visuals.widgets.hovered.bg_stroke = Stroke::new(1.0, Color32::from_rgb(65, 116, 155));
    visuals.widgets.active.bg_fill = Color32::from_rgb(39, 91, 132);
    visuals.window_corner_radius = egui::CornerRadius::same(8);
    style.visuals = visuals;
    context.set_style_of(egui::Theme::Dark, style);
}

fn toolbar_icon_button(
    ui: &mut egui::Ui,
    icon: ToolbarIcon,
    selected: bool,
    enabled: bool,
    tooltip: &str,
) -> bool {
    let sense = if enabled {
        Sense::click()
    } else {
        Sense::hover()
    };
    let (rect, response) = ui.allocate_exact_size(egui::vec2(32.0, 32.0), sense);
    let visuals = if enabled {
        ui.style().interact_selectable(&response, selected)
    } else {
        *ui.style().noninteractive()
    };
    let button_rect = rect.expand(visuals.expansion);
    ui.painter()
        .rect_filled(button_rect, visuals.corner_radius, visuals.weak_bg_fill);
    ui.painter().rect_stroke(
        button_rect,
        visuals.corner_radius,
        visuals.bg_stroke,
        StrokeKind::Inside,
    );
    paint_toolbar_icon(
        ui.painter(),
        rect.shrink(7.0),
        icon,
        visuals.fg_stroke.color,
    );
    let response = response.on_hover_text(tooltip);
    enabled && response.clicked()
}

fn paint_toolbar_icon(
    painter: &egui::Painter,
    rect: egui::Rect,
    icon: ToolbarIcon,
    color: Color32,
) {
    let stroke = Stroke::new(1.7, color);
    let left = rect.left();
    let right = rect.right();
    let top = rect.top();
    let bottom = rect.bottom();
    let center = rect.center();
    match icon {
        ToolbarIcon::Drawing(DrawingTool::Select) => {
            let mut points = vec![
                egui::pos2(left + 1.0, top),
                egui::pos2(left + 1.0, bottom - 1.0),
                egui::pos2(left + 5.0, bottom - 5.0),
                egui::pos2(left + 8.5, bottom),
                egui::pos2(left + 11.5, bottom - 2.0),
                egui::pos2(left + 8.0, bottom - 7.0),
                egui::pos2(right, bottom - 7.0),
            ];
            points.push(points[0]);
            painter.add(egui::Shape::line(points, stroke));
        }
        ToolbarIcon::Drawing(DrawingTool::Line) => {
            painter.line_segment(
                [
                    egui::pos2(left + 1.0, bottom - 1.0),
                    egui::pos2(right - 1.0, top + 1.0),
                ],
                stroke,
            );
        }
        ToolbarIcon::Drawing(DrawingTool::Arrow) => {
            let origin = egui::pos2(left + 1.0, bottom - 1.0);
            let tip = egui::pos2(right - 1.0, top + 1.0);
            painter.arrow(origin, tip - origin, stroke);
        }
        ToolbarIcon::Drawing(DrawingTool::Rectangle) => {
            painter.rect_stroke(rect.shrink(1.0), 0.5, stroke, StrokeKind::Inside);
        }
        ToolbarIcon::Drawing(DrawingTool::Ellipse) => {
            painter.add(egui::epaint::EllipseShape::stroke(
                center,
                egui::vec2(rect.width() * 0.45, rect.height() * 0.36),
                stroke,
            ));
        }
        ToolbarIcon::Drawing(DrawingTool::Polyline) => {
            let points = vec![
                egui::pos2(left, bottom - 2.0),
                egui::pos2(left + rect.width() * 0.33, top + 3.0),
                egui::pos2(left + rect.width() * 0.66, bottom - 4.0),
                egui::pos2(right, top + 1.0),
            ];
            painter.add(egui::Shape::line(points.clone(), stroke));
            for point in points {
                painter.circle_filled(point, 1.7, color);
            }
        }
        ToolbarIcon::Drawing(DrawingTool::Polygon) => {
            let radius = 0.46 * rect.width().min(rect.height());
            let mut points: Vec<_> = (0..5)
                .map(|index| {
                    let angle =
                        -std::f32::consts::FRAC_PI_2 + index as f32 * std::f32::consts::TAU / 5.0;
                    center + egui::vec2(angle.cos(), angle.sin()) * radius
                })
                .collect();
            points.push(points[0]);
            painter.add(egui::Shape::line(points, stroke));
        }
        ToolbarIcon::Drawing(DrawingTool::Text) => {
            painter.line_segment(
                [
                    egui::pos2(left + 2.0, top + 1.0),
                    egui::pos2(right - 2.0, top + 1.0),
                ],
                stroke,
            );
            painter.line_segment(
                [
                    egui::pos2(center.x, top + 1.0),
                    egui::pos2(center.x, bottom),
                ],
                stroke,
            );
        }
        ToolbarIcon::FitView => {
            let length = 5.0;
            for (corner, x_direction, y_direction) in [
                (rect.left_top(), 1.0, 1.0),
                (rect.right_top(), -1.0, 1.0),
                (rect.left_bottom(), 1.0, -1.0),
                (rect.right_bottom(), -1.0, -1.0),
            ] {
                painter.line_segment(
                    [corner, corner + egui::vec2(length * x_direction, 0.0)],
                    stroke,
                );
                painter.line_segment(
                    [corner, corner + egui::vec2(0.0, length * y_direction)],
                    stroke,
                );
            }
        }
        ToolbarIcon::Undo | ToolbarIcon::Redo => {
            let radius = rect.width().min(rect.height()) * 0.39;
            let redo = matches!(icon, ToolbarIcon::Redo);
            let mut points: Vec<_> = (0..=14)
                .map(|index| {
                    let fraction = index as f32 / 14.0;
                    let angle = 0.25 + (-std::f32::consts::PI - 0.25) * fraction;
                    let mut point = center + egui::vec2(angle.cos(), angle.sin()) * radius;
                    if redo {
                        point.x = 2.0 * center.x - point.x;
                    }
                    point
                })
                .collect();
            let tip = *points.last().expect("undo path has points");
            painter.add(egui::Shape::line(std::mem::take(&mut points), stroke));
            let direction = if redo { -1.0 } else { 1.0 };
            painter.line_segment([tip, tip + egui::vec2(4.0 * direction, -3.0)], stroke);
            painter.line_segment([tip, tip + egui::vec2(4.0 * direction, 3.0)], stroke);
        }
    }
}

fn section_heading(ui: &mut egui::Ui, text: &str) {
    ui.label(
        RichText::new(text.to_uppercase())
            .size(10.0)
            .strong()
            .color(ACCENT),
    );
    ui.add_space(3.0);
}

fn metadata_row(ui: &mut egui::Ui, label: &str, value: &str) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(label).color(MUTED));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(value);
        });
    });
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

fn color_limit_speed(limits: [f32; 2]) -> f64 {
    ((limits[1] - limits[0]).abs() / 200.0).max(1.0e-9) as f64
}

fn paint_colormap_preview(ui: &mut egui::Ui, appearance: &AppearanceSettings) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 18.0), Sense::hover());
    for index in 0..64 {
        let left = rect.left() + rect.width() * index as f32 / 64.0;
        let right = rect.left() + rect.width() * (index + 1) as f32 / 64.0;
        ui.painter().rect_filled(
            egui::Rect::from_min_max(
                egui::pos2(left, rect.top()),
                egui::pos2(right, rect.bottom()),
            ),
            0.0,
            sample_appearance(appearance, index as f32 / 63.0),
        );
    }
    ui.painter()
        .rect_stroke(rect, 2.0, Stroke::new(1.0, MUTED), StrokeKind::Inside);
}

fn tick_controls(ui: &mut egui::Ui, appearance: &mut AppearanceSettings, limits: [f32; 2]) {
    let automatic = matches!(appearance.ticks.mode, TickMode::Automatic { .. });
    ui.horizontal(|ui| {
        if ui.selectable_label(automatic, "Automatic").clicked() && !automatic {
            appearance.ticks.mode = TickMode::Automatic { count: 7 };
        }
        if ui.selectable_label(!automatic, "Custom").clicked() && automatic {
            appearance.ticks.mode = TickMode::Custom {
                ticks: vec![
                    ColorbarTick {
                        value: limits[0] as f64,
                        label: None,
                    },
                    ColorbarTick {
                        value: limits[1] as f64,
                        label: None,
                    },
                ],
            };
        }
    });
    match &mut appearance.ticks.mode {
        TickMode::Automatic { count } => {
            ui.add(egui::Slider::new(count, 2..=12).text("Target count"));
        }
        TickMode::Custom { ticks } => {
            let errors = validate_custom_ticks(ticks, limits, appearance.scale);
            let mut remove = None;
            for (index, tick) in ticks.iter_mut().enumerate() {
                ui.horizontal(|ui| {
                    ui.add(egui::DragValue::new(&mut tick.value).speed(color_limit_speed(limits)));
                    let mut label = tick.label.clone().unwrap_or_default();
                    if ui
                        .add(egui::TextEdit::singleline(&mut label).hint_text("Optional label"))
                        .changed()
                    {
                        tick.label = (!label.is_empty()).then_some(label);
                    }
                    if ui
                        .small_button("Remove")
                        .on_hover_text("Remove tick")
                        .clicked()
                    {
                        remove = Some(index);
                    }
                });
                if let Some(error) = &errors[index] {
                    ui.colored_label(Color32::from_rgb(241, 126, 126), error);
                }
            }
            if let Some(index) = remove {
                ticks.remove(index);
            }
            if ui.button("+ Add tick").clicked() {
                ticks.push(ColorbarTick {
                    value: 0.5 * (limits[0] + limits[1]) as f64,
                    label: None,
                });
            }
        }
    }

    let current_name = match appearance.ticks.format {
        NumberFormat::Automatic => "Automatic",
        NumberFormat::Fixed(_) => "Fixed decimal",
        NumberFormat::Scientific(_) => "Scientific",
    };
    egui::ComboBox::from_label("Number format")
        .selected_text(current_name)
        .show_ui(ui, |ui| {
            if ui
                .selectable_label(
                    matches!(appearance.ticks.format, NumberFormat::Automatic),
                    "Automatic",
                )
                .clicked()
            {
                appearance.ticks.format = NumberFormat::Automatic;
            }
            if ui
                .selectable_label(
                    matches!(appearance.ticks.format, NumberFormat::Fixed(_)),
                    "Fixed decimal",
                )
                .clicked()
            {
                appearance.ticks.format = NumberFormat::Fixed(3);
            }
            if ui
                .selectable_label(
                    matches!(appearance.ticks.format, NumberFormat::Scientific(_)),
                    "Scientific",
                )
                .clicked()
            {
                appearance.ticks.format = NumberFormat::Scientific(3);
            }
        });
    match &mut appearance.ticks.format {
        NumberFormat::Fixed(precision) | NumberFormat::Scientific(precision) => {
            ui.add(egui::Slider::new(precision, 0..=9).text("Precision"));
        }
        NumberFormat::Automatic => {}
    }
}

fn title_controls(ui: &mut egui::Ui, title: &mut TitleConfig, rendered_title: Option<&str>) {
    let mut fixed = title.override_text.is_some();
    if ui.checkbox(&mut fixed, "Use fixed title").changed() {
        title.override_text = fixed.then(|| {
            rendered_title
                .map(str::to_owned)
                .unwrap_or_else(|| title.template.clone())
        });
    }
    if let Some(override_text) = &mut title.override_text {
        ui.add(egui::TextEdit::singleline(override_text).hint_text("Plot title"));
    } else {
        ui.add(egui::TextEdit::singleline(&mut title.template).hint_text("Title template"));
    }
    if ui.button("Reset to automatic").clicked() {
        *title = TitleConfig::default();
    }
}

fn geometry_controls(ui: &mut egui::Ui, geometry: &mut AnnotationGeometry) {
    match geometry {
        AnnotationGeometry::Line { start, end } | AnnotationGeometry::Arrow { start, end } => {
            point_controls(ui, "Start", start);
            point_controls(ui, "End", end);
        }
        AnnotationGeometry::Rectangle { start, end } => {
            let mut center =
                crate::scene::DataPoint::new(0.5 * (start.x + end.x), 0.5 * (start.y + end.y));
            let mut width = (end.x - start.x).abs();
            let mut height = (end.y - start.y).abs();
            ui.label(RichText::new("Position and size").small().color(MUTED));
            egui::Grid::new("rectangle_geometry")
                .num_columns(2)
                .spacing(egui::vec2(10.0, 7.0))
                .show(ui, |ui| {
                    ui.label("Center X");
                    ui.add(egui::DragValue::new(&mut center.x).speed(0.01));
                    ui.end_row();
                    ui.label("Center Y");
                    ui.add(egui::DragValue::new(&mut center.y).speed(0.01));
                    ui.end_row();
                    ui.label("Width");
                    ui.add(
                        egui::DragValue::new(&mut width)
                            .speed(0.01)
                            .range(0.0..=f64::INFINITY),
                    );
                    ui.end_row();
                    ui.label("Height");
                    ui.add(
                        egui::DragValue::new(&mut height)
                            .speed(0.01)
                            .range(0.0..=f64::INFINITY),
                    );
                    ui.end_row();
                });
            *start = crate::scene::DataPoint::new(center.x - 0.5 * width, center.y - 0.5 * height);
            *end = crate::scene::DataPoint::new(center.x + 0.5 * width, center.y + 0.5 * height);
        }
        AnnotationGeometry::Ellipse {
            start,
            end,
            lock_aspect,
        } => {
            let mut center =
                crate::scene::DataPoint::new(0.5 * (start.x + end.x), 0.5 * (start.y + end.y));
            let mut radius_x = 0.5 * (end.x - start.x).abs();
            let mut radius_y = 0.5 * (end.y - start.y).abs();
            ui.checkbox(lock_aspect, "Circle (equal radii)")
                .on_hover_text(
                    "Turn this off to edit horizontal and vertical ellipse radii independently",
                );
            if *lock_aspect {
                radius_y = radius_x;
            }
            ui.label(RichText::new("Center and radius").small().color(MUTED));
            egui::Grid::new("ellipse_geometry")
                .num_columns(2)
                .spacing(egui::vec2(10.0, 7.0))
                .show(ui, |ui| {
                    ui.label("Center X");
                    ui.add(egui::DragValue::new(&mut center.x).speed(0.01));
                    ui.end_row();
                    ui.label("Center Y");
                    ui.add(egui::DragValue::new(&mut center.y).speed(0.01));
                    ui.end_row();
                    if *lock_aspect {
                        ui.label("Radius");
                        ui.add(
                            egui::DragValue::new(&mut radius_x)
                                .speed(0.01)
                                .range(0.0..=f64::INFINITY),
                        );
                        radius_y = radius_x;
                        ui.end_row();
                    } else {
                        ui.label("Radius X");
                        ui.add(
                            egui::DragValue::new(&mut radius_x)
                                .speed(0.01)
                                .range(0.0..=f64::INFINITY),
                        );
                        ui.end_row();
                        ui.label("Radius Y");
                        ui.add(
                            egui::DragValue::new(&mut radius_y)
                                .speed(0.01)
                                .range(0.0..=f64::INFINITY),
                        );
                        ui.end_row();
                    }
                });
            *start = crate::scene::DataPoint::new(center.x - radius_x, center.y - radius_y);
            *end = crate::scene::DataPoint::new(center.x + radius_x, center.y + radius_y);
            ui.small(
                RichText::new("Drag the blue center handle to move; drag edge handles to resize.")
                    .color(MUTED),
            );
        }
        AnnotationGeometry::Polyline { points } | AnnotationGeometry::Polygon { points } => {
            for (index, point) in points.iter_mut().enumerate() {
                point_controls(ui, &format!("Point {}", index + 1), point);
            }
        }
        AnnotationGeometry::Text { position, text } => {
            point_controls(ui, "Position", position);
            ui.add(egui::TextEdit::multiline(text).desired_rows(3));
        }
    }
}

fn point_controls(ui: &mut egui::Ui, label: &str, point: &mut crate::scene::DataPoint) {
    ui.label(RichText::new(label).small().color(MUTED));
    ui.horizontal(|ui| {
        ui.label("x");
        ui.add(egui::DragValue::new(&mut point.x).speed(0.01));
        ui.label("y");
        ui.add(egui::DragValue::new(&mut point.y).speed(0.01));
    });
}

fn color_control(ui: &mut egui::Ui, color: &mut RgbaColor) {
    let mut value = color.to_egui();
    if ui.color_edit_button_srgba(&mut value).changed() {
        *color = RgbaColor::from_egui(value);
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

fn run_key(directory: &str) -> String {
    #[cfg(target_os = "windows")]
    {
        directory.replace('\\', "/").to_lowercase()
    }
    #[cfg(not(target_os = "windows"))]
    {
        directory.to_owned()
    }
}

fn safe_filename(value: &str) -> String {
    let sanitized: String = value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_') {
                character
            } else {
                '_'
            }
        })
        .collect();
    if sanitized.is_empty() {
        "plot".into()
    } else {
        sanitized
    }
}

fn is_coordinate(name: &str) -> bool {
    let compact = name.to_lowercase().replace(' ', "");
    matches!(compact.as_str(), "x" | "y" | "z")
        || compact.starts_with("x[")
        || compact.starts_with("y[")
        || compact.starts_with("z[")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plot_rect_preserves_coordinate_aspect_ratio() {
        let outer = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(200.0, 100.0));
        let square = fit_plot_rect(outer, [-1.0, 1.0, -1.0, 1.0]);
        assert_eq!(square.size(), egui::vec2(100.0, 100.0));
        let wide = fit_plot_rect(outer, [-2.0, 2.0, -0.5, 0.5]);
        assert_eq!(wide.size(), egui::vec2(200.0, 50.0));
    }

    #[test]
    fn export_filenames_are_portable() {
        assert_eq!(safe_filename("rho [amu/cm3]"), "rho__amu_cm3_");
        assert_eq!(safe_filename(""), "plot");
    }
}
