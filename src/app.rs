use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, mpsc},
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use eframe::egui::{
    self, Color32, FontId, KeyboardShortcut, Modifiers, RichText, Sense, Stroke, StrokeKind,
};
use serde::{Deserialize, Serialize};

use crate::{
    annotations::{AnnotationEditor, DrawingTool},
    bridge::Bridge,
    camera3d::Projection3d,
    catalog::{scan_directory, timeline_indices},
    export::{
        ExportBackground, ExportFrame, ExportFrame3d, ExportSettings, render_plot_png,
        render_scene3d_png,
    },
    loader::{
        CacheStats, Crop3dRequest, FieldLineTrace3dRequest, IsosurfaceRequest, LoaderEvent,
        PlotKey, PlotLoader, RequestPriority, SliceAxis, SlicePlaneRequest, Surface3dKey,
    },
    plot_ui::{
        PlotChrome, PlotColors, colorbar_rect_3d, fit_plot_rect, paint_plot_chrome,
        paint_reference_bodies_2d, sample_appearance,
    },
    probe::{ProbeHit, ProbeIndex, ProbeIndexer, camera_ray},
    protocol::{BRIDGE_PROTOCOL, FieldLines3dData, FileInfo, PlotData, PlotFile, ScanResult},
    render::{PlotCallback, PlotHandle, PlotResources, SharedPlot, VIEW_DEPTH_FORMAT},
    render3d::{
        LayerDisplay3d, Scene3dCallback, Scene3dHandle, Scene3dResources, SharedScene3d,
        paint_fieldlines3d, paint_scene_overlays,
    },
    scene::{
        AnnotationGeometry, AnnotationScope, AppearanceSettings, ColorMode, ColorbarTick, Colormap,
        CropBox3d, DashStyle, DataPoint, DataPoint3, DaysideDirection2d, IsosurfaceColoring,
        IsosurfaceLayer, LatitudeSeedSettings, MAX_ISOSURFACE_LAYERS, MAX_PROBE_MEASUREMENTS,
        MeshBudget, NumberFormat, ProbeDimension, ProbeMeasurement, RgbaColor, Scale,
        SceneDocument, ScopeContext, StreamlineDirection, TickMode, TitleConfig, TitleContext,
        colorbar_ticks, normalized_value, render_title, validate_custom_ticks,
    },
    streamlines::{
        StreamlineOverlay, VectorField, paint_streamlines, screen_to_data as streamline_seed_point,
    },
};

const APP_STORAGE_KEY: &str = "batsview-app-state-v1";
const ACCENT: Color32 = Color32::from_rgb(70, 160, 235);
const PANEL_BG: Color32 = Color32::from_rgb(15, 22, 31);
const DEEP_BG: Color32 = Color32::from_rgb(9, 14, 21);
const MUTED: Color32 = Color32::from_rgb(145, 158, 173);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum ViewMode {
    #[default]
    TwoD,
    ThreeD,
}

const fn default_cache_limit_mib() -> u32 {
    512
}

const fn default_playback_fps() -> f32 {
    5.0
}

fn mib_to_bytes(value: u32) -> usize {
    value as usize * 1024 * 1024
}

enum Event {
    DirectoryChosen(Option<PathBuf>),
    Scan {
        epoch: u64,
        result: Result<ScanResult>,
    },
    SceneSaved(Result<Option<PathBuf>>),
    SceneLoaded(Box<Result<Option<(PathBuf, SceneDocument)>>>),
    ExportPathChosen {
        path: Option<PathBuf>,
        settings: ExportSettings,
    },
    ImageSaved(Result<PathBuf>),
    StreamlinesComputed {
        generation: u64,
        path: String,
        section: Option<String>,
        horizontal_component: String,
        vertical_component: String,
        result: Result<(Arc<VectorField>, Vec<Vec<DataPoint>>)>,
    },
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum InspectorTab {
    #[default]
    Data,
    Appearance,
    Surfaces,
    Annotations,
    FieldLines,
    Metadata,
}

impl InspectorTab {
    const TWO_D: [Self; 5] = [
        Self::Data,
        Self::Appearance,
        Self::Annotations,
        Self::FieldLines,
        Self::Metadata,
    ];
    const THREE_D: [Self; 6] = [
        Self::Data,
        Self::Appearance,
        Self::Surfaces,
        Self::Annotations,
        Self::FieldLines,
        Self::Metadata,
    ];

    fn name(self) -> &'static str {
        match self {
            Self::Data => "Data",
            Self::Appearance => "Appearance",
            Self::Surfaces => "3D surfaces",
            Self::Annotations => "Annotations",
            Self::FieldLines => "Field lines",
            Self::Metadata => "Metadata",
        }
    }

    fn short_name(self) -> &'static str {
        match self {
            Self::Data => "Data",
            Self::Appearance => "Style",
            Self::Surfaces => "Surfaces",
            Self::Annotations => "Shapes",
            Self::FieldLines => "Fields",
            Self::Metadata => "Info",
        }
    }
}

#[derive(Clone, Copy)]
enum ToolbarIcon {
    Drawing(DrawingTool),
    FitView,
    Undo,
    Redo,
    StreamlineSeed,
    Probe,
    Previous,
    Play,
    Pause,
    Next,
}

struct PendingStreamlineLoad {
    generation: u64,
    path: String,
    section: Option<String>,
    horizontal_component: String,
    vertical_component: String,
    horizontal_request: u64,
    vertical_request: u64,
    horizontal: Option<Arc<PlotData>>,
    vertical: Option<Arc<PlotData>>,
}

struct ActiveVectorField {
    path: String,
    section: Option<String>,
    horizontal_component: String,
    vertical_component: String,
    field: Arc<VectorField>,
}

#[derive(Clone, Serialize, Deserialize)]
struct StoredRunScene {
    key: String,
    directory: String,
    scene: SceneDocument,
}

#[derive(Serialize, Deserialize)]
struct PersistedAppState {
    #[serde(default)]
    recursive: bool,
    #[serde(default)]
    recent_runs: Vec<StoredRunScene>,
    #[serde(default = "default_cache_limit_mib")]
    cache_limit_mib: u32,
    #[serde(default = "default_playback_fps")]
    playback_fps: f32,
    #[serde(default)]
    playback_loop: bool,
}

impl Default for PersistedAppState {
    fn default() -> Self {
        Self {
            recursive: false,
            recent_runs: Vec::new(),
            cache_limit_mib: default_cache_limit_mib(),
            playback_fps: default_playback_fps(),
            playback_loop: false,
        }
    }
}

pub struct ViewerApp {
    loader: PlotLoader,
    sender: mpsc::Sender<Event>,
    receiver: mpsc::Receiver<Event>,
    plot: PlotHandle,
    scene3d: Scene3dHandle,
    view_mode: ViewMode,
    directory: Option<PathBuf>,
    recursive: bool,
    files: Vec<PlotFile>,
    selected_path: Option<String>,
    displayed_path: Option<String>,
    info: Option<FileInfo>,
    displayed_info: Option<FileInfo>,
    selected_variable: Option<String>,
    displayed_variable: Option<String>,
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
    load_epoch: u64,
    active_inspect_request: Option<u64>,
    active_plot_request: Option<u64>,
    cache_limit_mib: u32,
    cache_stats: CacheStats,
    playback_fps: f32,
    playback_loop: bool,
    playing: bool,
    buffering: bool,
    next_frame_at: Option<Instant>,
    scrub_target: Option<usize>,
    scrub_changed_at: Option<Instant>,
    slice_changed_at: Option<Instant>,
    pending_streamlines: Option<PendingStreamlineLoad>,
    vector_field: Option<ActiveVectorField>,
    streamline_overlay: Option<StreamlineOverlay>,
    streamline_generation: u64,
    streamline_loading: bool,
    streamline_error: Option<String>,
    placing_streamline_seed: bool,
    fieldlines3d: Option<Arc<FieldLines3dData>>,
    active_fieldlines3d_request: Option<u64>,
    fieldlines3d_loading: bool,
    fieldlines3d_error: Option<String>,
    isosurface_drafts: BTreeMap<u64, IsosurfaceLayer>,
    selected_isosurface: Option<u64>,
    crop_draft: CropBox3d,
    probe_indexer: ProbeIndexer,
    probe_generation: u64,
    probe_index: Option<Arc<ProbeIndex>>,
    probe_indexing: bool,
    probe_mode: bool,
    hover_probe: Option<ProbeHit>,
}

impl ViewerApp {
    pub fn new(context: &eframe::CreationContext<'_>, initial_path: Option<PathBuf>) -> Self {
        configure_style(&context.egui_ctx);
        let plot = Arc::new(Mutex::new(SharedPlot::default()));
        let scene3d = Arc::new(Mutex::new(SharedScene3d::default()));
        if let Some(render_state) = &context.wgpu_render_state {
            let resources = PlotResources::new(
                &render_state.device,
                &render_state.queue,
                render_state.target_format,
                Some(VIEW_DEPTH_FORMAT),
            );
            render_state
                .renderer
                .write()
                .callback_resources
                .insert(resources);
            let resources3d = Scene3dResources::new(
                &render_state.device,
                &render_state.queue,
                render_state.target_format,
            );
            render_state
                .renderer
                .write()
                .callback_resources
                .insert(resources3d);
        }
        let persisted: PersistedAppState = context
            .storage
            .and_then(|storage| eframe::get_value(storage, APP_STORAGE_KEY))
            .unwrap_or_default();
        let cache_limit_mib = persisted.cache_limit_mib.clamp(64, 8192);
        let playback_fps = persisted.playback_fps.clamp(0.5, 30.0);
        let playback_loop = persisted.playback_loop;
        let recursive = persisted.recursive;
        let stored_runs = persisted
            .recent_runs
            .into_iter()
            .filter_map(|mut stored| {
                stored.scene = stored.scene.migrate().ok()?;
                Some(stored)
            })
            .collect();
        let loader = PlotLoader::new(Bridge::discover(), mib_to_bytes(cache_limit_mib));
        let (sender, receiver) = mpsc::channel();
        let mut app = Self {
            loader,
            sender,
            receiver,
            plot,
            scene3d,
            view_mode: ViewMode::TwoD,
            directory: None,
            recursive,
            files: Vec::new(),
            selected_path: None,
            displayed_path: None,
            info: None,
            displayed_info: None,
            selected_variable: None,
            displayed_variable: None,
            file_filter: String::new(),
            variable_filter: String::new(),
            choosing_run: false,
            loading: false,
            io_busy: false,
            status: "Choose a BATS-R-US output directory to begin".to_owned(),
            scene: SceneDocument::default(),
            stored_runs,
            current_run_key: None,
            editor: AnnotationEditor::default(),
            inspector_tab: InspectorTab::Data,
            show_export_dialog: false,
            export_settings: ExportSettings::default(),
            render_state: context.wgpu_render_state.clone(),
            last_export_rect: None,
            load_epoch: 1,
            active_inspect_request: None,
            active_plot_request: None,
            cache_limit_mib,
            cache_stats: CacheStats {
                limit_bytes: mib_to_bytes(cache_limit_mib),
                ..CacheStats::default()
            },
            playback_fps,
            playback_loop,
            playing: false,
            buffering: false,
            next_frame_at: None,
            scrub_target: None,
            scrub_changed_at: None,
            slice_changed_at: None,
            pending_streamlines: None,
            vector_field: None,
            streamline_overlay: None,
            streamline_generation: 1,
            streamline_loading: false,
            streamline_error: None,
            placing_streamline_seed: false,
            fieldlines3d: None,
            active_fieldlines3d_request: None,
            fieldlines3d_loading: false,
            fieldlines3d_error: None,
            isosurface_drafts: BTreeMap::new(),
            selected_isosurface: None,
            crop_draft: CropBox3d::default(),
            probe_indexer: ProbeIndexer::new(),
            probe_generation: 0,
            probe_index: None,
            probe_indexing: false,
            probe_mode: false,
            hover_probe: None,
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
        let directory = directory.canonicalize().unwrap_or(directory);
        self.load_epoch = self.load_epoch.wrapping_add(1);
        self.pause_playback();
        self.directory = Some(directory.clone());
        self.files.clear();
        self.info = None;
        self.displayed_info = None;
        self.selected_path = None;
        self.displayed_path = None;
        self.selected_variable = None;
        self.displayed_variable = None;
        self.active_inspect_request = None;
        self.active_plot_request = None;
        self.loader.cancel_auxiliary();
        self.pending_streamlines = None;
        self.vector_field = None;
        self.streamline_overlay = None;
        self.streamline_loading = false;
        self.streamline_error = None;
        self.placing_streamline_seed = false;
        self.fieldlines3d = None;
        self.active_fieldlines3d_request = None;
        self.fieldlines3d_loading = false;
        self.fieldlines3d_error = None;
        self.probe_generation = self.probe_generation.wrapping_add(1);
        self.probe_index = None;
        self.probe_indexing = false;
        self.hover_probe = None;
        self.plot.lock().unwrap().clear_data();
        self.scene3d.lock().unwrap().clear_data();
        self.view_mode = ViewMode::TwoD;
        self.loading = true;
        self.status = format!("Scanning {}…", directory.display());
        let sender = self.sender.clone();
        let recursive = self.recursive;
        let epoch = self.load_epoch;
        thread::spawn(move || {
            let _ = sender.send(Event::Scan {
                epoch,
                result: scan_directory(&directory, recursive),
            });
        });
    }

    fn inspect(&mut self, path: String) {
        let path = Path::new(&path)
            .canonicalize()
            .unwrap_or_else(|_| PathBuf::from(&path))
            .to_string_lossy()
            .into_owned();
        self.pause_playback();
        self.selected_path = Some(path.clone());
        self.info = None;
        self.loading = true;
        self.status = "Inspecting metadata…".to_owned();
        self.active_plot_request = None;
        self.active_inspect_request = Some(self.loader.inspect(self.load_epoch, path.into()));
    }

    fn load_variable(&mut self, variable: String) {
        self.pause_playback();
        self.selected_variable = Some(variable.clone());
        self.request_selected_plot(variable);
    }

    fn surface3d_request_parts(
        &self,
    ) -> (
        Vec<SlicePlaneRequest>,
        Vec<IsosurfaceRequest>,
        Crop3dRequest,
    ) {
        let planes = [SliceAxis::X, SliceAxis::Y, SliceAxis::Z]
            .into_iter()
            .enumerate()
            .map(|(index, axis)| SlicePlaneRequest {
                axis,
                position: self.scene.view3d.slice_fractions[index].clamp(0.0, 1.0),
                enabled: self.scene.view3d.slice_enabled[index],
                normalized: true,
                origin_if_available: self.scene.view3d.slice_auto_origin[index],
            })
            .collect();
        let section = self
            .info
            .as_ref()
            .and_then(|info| info.section.as_deref())
            .unwrap_or("3d");
        let isosurfaces = self
            .scene
            .isosurfaces_for(Some(section))
            .iter()
            .map(|layer| IsosurfaceRequest {
                id: layer.id,
                variable: layer.variable.clone(),
                isovalue: layer.isovalue,
                color_variable: match &layer.coloring {
                    IsosurfaceColoring::Solid { .. } => None,
                    IsosurfaceColoring::Scalar { variable, .. } => Some(variable.clone()),
                },
                triangle_limit: layer.mesh_budget.triangle_limit(),
            })
            .collect();
        let crop = Crop3dRequest {
            enabled: self.scene.view3d.crop.enabled,
            fractions: self.scene.view3d.crop.fractions,
        };
        (planes, isosurfaces, crop)
    }

    fn current_3d_section(&self) -> &str {
        self.info
            .as_ref()
            .and_then(|info| info.section.as_deref())
            .or_else(|| {
                self.displayed_info
                    .as_ref()
                    .and_then(|info| info.section.as_deref())
            })
            .unwrap_or("3d")
    }

    fn refresh_surface_drafts(&mut self) {
        self.isosurface_drafts = self
            .scene
            .isosurfaces_for(Some("3d"))
            .iter()
            .cloned()
            .map(|layer| (layer.id, layer))
            .collect();
        self.crop_draft = self.scene.view3d.crop;
        self.selected_isosurface = self
            .selected_isosurface
            .filter(|id| self.isosurface_drafts.contains_key(id));
    }

    fn add_isosurface_draft(&mut self) {
        if self.isosurface_drafts.len() >= MAX_ISOSURFACE_LAYERS {
            self.fail(format!(
                "A scene may contain at most {MAX_ISOSURFACE_LAYERS} isosurfaces"
            ));
            return;
        }
        let Some(variable) = self.selected_variable.clone() else {
            self.fail("Select a scalar variable before adding an isosurface".to_owned());
            return;
        };
        let range = self
            .scene3d
            .lock()
            .unwrap()
            .data
            .as_ref()
            .and_then(|data| data.header.volume_value_range)
            .unwrap_or([0.0, 1.0]);
        let appearance = self.scene.appearance_for(Some(&variable));
        let isovalue = if appearance.scale == Scale::Logarithmic && range[0] > 0.0 {
            f64::from((range[0] * range[1]).sqrt())
        } else {
            f64::from(0.5 * (range[0] + range[1]))
        };
        let id = self.scene.allocate_isosurface_id();
        let colors = [
            [57, 189, 248, 255],
            [250, 204, 21, 255],
            [244, 114, 182, 255],
            [74, 222, 128, 255],
            [167, 139, 250, 255],
            [251, 146, 60, 255],
            [45, 212, 191, 255],
            [248, 113, 113, 255],
        ];
        let layer = IsosurfaceLayer {
            id,
            name: format!("{variable} = {isovalue:.4}"),
            variable,
            isovalue,
            coloring: IsosurfaceColoring::Solid {
                color: RgbaColor(colors[self.isosurface_drafts.len() % colors.len()]),
            },
            ..IsosurfaceLayer::default()
        };
        self.isosurface_drafts.insert(id, layer);
        self.selected_isosurface = Some(id);
        self.inspector_tab = InspectorTab::Surfaces;
    }

    fn apply_isosurface_draft(&mut self, layer: IsosurfaceLayer) {
        let section = self.current_3d_section().to_owned();
        let mut layers = self.scene.isosurfaces_for(Some(&section)).to_vec();
        if let Some(existing) = layers.iter_mut().find(|candidate| candidate.id == layer.id) {
            *existing = layer.clone();
        } else if layers.len() < MAX_ISOSURFACE_LAYERS {
            layers.push(layer.clone());
        } else {
            self.fail(format!(
                "A scene may contain at most {MAX_ISOSURFACE_LAYERS} isosurfaces"
            ));
            return;
        }
        self.editor.checkpoint(&self.scene);
        self.scene.set_isosurfaces_for(Some(&section), layers);
        self.isosurface_drafts.insert(layer.id, layer);
        if let Some(variable) = self.selected_variable.clone() {
            self.request_selected_plot(variable);
        }
    }

    fn sync_isosurface_style(&mut self, draft: &IsosurfaceLayer) {
        let section = self.current_3d_section().to_owned();
        let mut layers = self.scene.isosurfaces_for(Some(&section)).to_vec();
        let Some(layer) = layers.iter_mut().find(|layer| layer.id == draft.id) else {
            return;
        };
        layer.name.clone_from(&draft.name);
        layer.visible = draft.visible;
        layer.locked = draft.locked;
        layer.opacity = draft.opacity;
        match (&mut layer.coloring, &draft.coloring) {
            (IsosurfaceColoring::Solid { color: target }, IsosurfaceColoring::Solid { color }) => {
                *target = *color
            }
            (
                IsosurfaceColoring::Scalar {
                    appearance: target, ..
                },
                IsosurfaceColoring::Scalar { appearance, .. },
            ) => target.clone_from(appearance),
            _ => {}
        }
        self.scene.set_isosurfaces_for(Some(&section), layers);
        self.sync_plot_appearance();
    }

    fn request_selected_plot(&mut self, variable: String) {
        let Some(path) = self.selected_path.clone() else {
            return;
        };
        if self.view_mode == ViewMode::ThreeD {
            let (planes, isosurfaces, crop) = self.surface3d_request_parts();
            if !planes.iter().any(|plane| plane.enabled) && isosurfaces.is_empty() {
                self.fail("Enable a 3D slice or add an isosurface".to_owned());
                return;
            }
            let Ok(key) =
                Surface3dKey::for_file(&path, variable.clone(), 0, &planes, &isosurfaces, crop)
            else {
                self.fail(format!("Could not read metadata for {path}"));
                return;
            };
            let reuse_mesh = self
                .scene3d
                .lock()
                .unwrap()
                .data
                .as_ref()
                .map(|data| data.mesh.clone());
            let request_id = self.loader.load_surface3d(
                self.load_epoch,
                key,
                RequestPriority::Foreground,
                reuse_mesh,
            );
            self.active_plot_request = Some(request_id);
            self.active_inspect_request = None;
            self.loading = true;
            self.status = format!("Extracting 3D surfaces for {variable}…");
            return;
        }
        let Ok(key) = PlotKey::for_file(&path, variable.clone(), 0) else {
            self.fail(format!("Could not read metadata for {path}"));
            return;
        };
        let reuse_mesh = self
            .plot
            .lock()
            .unwrap()
            .data
            .as_ref()
            .map(|data| data.mesh.clone());
        let request_id = self.loader.load(
            self.load_epoch,
            key,
            RequestPriority::Foreground,
            reuse_mesh,
        );
        self.active_plot_request = Some(request_id);
        self.active_inspect_request = None;
        self.loading = true;
        self.status = format!("Loading {variable}…");
    }

    fn poll_events(&mut self, context: &egui::Context) {
        self.poll_loader_events();
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
                Event::Scan { epoch, result } if epoch == self.load_epoch => match result {
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
                    match *result {
                        Ok(Some((path, scene))) => {
                            self.editor.checkpoint(&self.scene);
                            self.scene = scene;
                            self.refresh_surface_drafts();
                            self.editor.selected = None;
                            self.sync_plot_appearance();
                            if self.view_mode == ViewMode::ThreeD {
                                if let Some(variable) = self.selected_variable.clone() {
                                    self.request_selected_plot(variable);
                                }
                            } else {
                                self.request_streamlines_for_display();
                            }
                            self.status = format!("Scene loaded · {}", path.display());
                        }
                        Ok(None) => self.status = "Scene load canceled".to_owned(),
                        Err(error) => self.fail(error.to_string()),
                    }
                }
                Event::ExportPathChosen { path, settings } => {
                    self.io_busy = false;
                    if let Some(path) = path {
                        let sender = self.sender.clone();
                        let pixels_per_point = context.pixels_per_point();
                        match self.view_mode {
                            ViewMode::TwoD => {
                                if let Some(frame) =
                                    self.export_frame(path, settings, pixels_per_point)
                                {
                                    self.io_busy = true;
                                    self.status = "Rendering PNG…".to_owned();
                                    thread::spawn(move || {
                                        let _ =
                                            sender.send(Event::ImageSaved(render_plot_png(frame)));
                                    });
                                } else {
                                    self.fail(
                                        "No GPU plot is available to export; load a variable first"
                                            .to_owned(),
                                    );
                                }
                            }
                            ViewMode::ThreeD => {
                                if let Some(frame) =
                                    self.export_frame_3d(path, settings, pixels_per_point)
                                {
                                    self.io_busy = true;
                                    self.status = "Rendering 3D PNG…".to_owned();
                                    thread::spawn(move || {
                                        let _ = sender
                                            .send(Event::ImageSaved(render_scene3d_png(frame)));
                                    });
                                } else {
                                    self.fail(
                                        "No 3D scene is available to export; load a variable first"
                                            .to_owned(),
                                    );
                                }
                            }
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
                Event::StreamlinesComputed {
                    generation,
                    path,
                    section,
                    horizontal_component,
                    vertical_component,
                    result,
                } if generation == self.streamline_generation
                    && self.displayed_path.as_deref() == Some(path.as_str()) =>
                {
                    let settings = self.scene.streamlines_for(section.as_deref());
                    if !settings.enabled
                        || settings.horizontal_component.as_deref()
                            != Some(horizontal_component.as_str())
                        || settings.vertical_component.as_deref()
                            != Some(vertical_component.as_str())
                    {
                        continue;
                    }
                    match result {
                        Ok((field, lines)) => {
                            self.vector_field = Some(ActiveVectorField {
                                path: path.clone(),
                                section: section.clone(),
                                horizontal_component: horizontal_component.clone(),
                                vertical_component: vertical_component.clone(),
                                field,
                            });
                            self.streamline_overlay = Some(StreamlineOverlay {
                                path,
                                section,
                                horizontal_component,
                                vertical_component,
                                lines,
                                settings,
                            });
                            self.streamline_error = None;
                        }
                        Err(error) => {
                            self.streamline_overlay = None;
                            self.streamline_error = Some(error.to_string());
                        }
                    }
                    self.streamline_loading = false;
                    self.schedule_next_playback_frame();
                }
                _ => {}
            }
        }
    }

    fn poll_loader_events(&mut self) {
        while let Ok(event) = self.loader.try_recv() {
            match event {
                LoaderEvent::CacheStats(stats) => self.cache_stats = stats,
                LoaderEvent::Inspected {
                    request_id,
                    epoch,
                    path,
                    result,
                } if epoch == self.load_epoch
                    && self.active_inspect_request == Some(request_id)
                    && self.selected_path.as_deref() == path.to_str() =>
                {
                    self.active_inspect_request = None;
                    match result {
                        Ok(info) if info.protocol == BRIDGE_PROTOCOL => {
                            self.view_mode = if info
                                .zones
                                .first()
                                .is_some_and(|zone| zone.spatial_dimension == 3)
                            {
                                self.pending_streamlines = None;
                                self.vector_field = None;
                                self.streamline_overlay = None;
                                ViewMode::ThreeD
                            } else {
                                ViewMode::TwoD
                            };
                            let preserved = self.selected_variable.as_ref().and_then(|selected| {
                                info.variables
                                    .iter()
                                    .find(|variable| {
                                        &variable.canonical == selected
                                            && !is_coordinate(&variable.canonical)
                                    })
                                    .map(|variable| variable.canonical.clone())
                            });
                            let variable = preserved.or_else(|| {
                                info.variables
                                    .iter()
                                    .find(|variable| {
                                        !is_coordinate(&variable.source)
                                            && !is_coordinate(&variable.canonical)
                                    })
                                    .map(|variable| variable.canonical.clone())
                            });
                            self.info = Some(info);
                            if let Some(variable) = variable {
                                self.selected_variable = Some(variable.clone());
                                self.request_selected_plot(variable);
                            } else {
                                self.loading = false;
                                self.status = "No scalar variables found".to_owned();
                            }
                        }
                        Ok(info) => {
                            self.fail(format!("Unsupported bridge protocol {}", info.protocol))
                        }
                        Err(error) => self.fail(error),
                    }
                }
                LoaderEvent::Plot {
                    request_id,
                    epoch,
                    key,
                    priority: RequestPriority::Foreground,
                    from_cache,
                    result,
                } if epoch == self.load_epoch
                    && self.active_plot_request == Some(request_id)
                    && self.selected_path.as_deref() == key.path.to_str()
                    && self.selected_variable.as_deref() == Some(&key.variable) =>
                {
                    self.active_plot_request = None;
                    match result {
                        Ok(data) => {
                            let points = data.header.point_count;
                            let triangles = data.header.triangle_count;
                            self.displayed_path = Some(key.path.to_string_lossy().into_owned());
                            self.displayed_variable = Some(key.variable);
                            if self
                                .info
                                .as_ref()
                                .is_some_and(|info| Path::new(&info.path) == key.path.as_path())
                            {
                                self.displayed_info = self.info.clone();
                            } else if let Some(info) = &mut self.displayed_info {
                                info.path = key.path.to_string_lossy().into_owned();
                                info.title = data.header.title.clone();
                                info.section = data.header.section.clone();
                            }
                            self.plot.lock().unwrap().set_data(data.clone());
                            self.schedule_probe_2d(data);
                            self.sync_plot_appearance();
                            self.loading = false;
                            self.status = if from_cache {
                                format!("{points} points · {triangles} triangles · cached")
                            } else {
                                format!("{points} points · {triangles} triangles")
                            };
                            self.request_streamlines_for_display();
                            self.schedule_next_playback_frame();
                            self.schedule_prefetch();
                        }
                        Err(error) => {
                            self.pause_playback();
                            self.fail(error);
                        }
                    }
                }
                LoaderEvent::Surface3d {
                    request_id,
                    epoch,
                    key,
                    priority: RequestPriority::Foreground,
                    from_cache,
                    result,
                } if epoch == self.load_epoch
                    && self.active_plot_request == Some(request_id)
                    && self.selected_path.as_deref() == key.path.to_str()
                    && self.selected_variable.as_deref() == Some(&key.variable) =>
                {
                    self.active_plot_request = None;
                    match result {
                        Ok(data) => {
                            let vertices = data.header.vertex_count;
                            let triangles = data.header.triangle_count;
                            let bounds = data.header.bounds;
                            let view_bounds = data.header.active_bounds();
                            let resolved_layers = data.header.layers.clone();
                            self.displayed_path = Some(key.path.to_string_lossy().into_owned());
                            self.displayed_variable = Some(key.variable);
                            if self
                                .info
                                .as_ref()
                                .is_some_and(|info| Path::new(&info.path) == key.path.as_path())
                            {
                                self.displayed_info = self.info.clone();
                            } else if let Some(info) = &mut self.displayed_info {
                                info.path = key.path.to_string_lossy().into_owned();
                                info.title = data.header.title.clone();
                                info.section = Some(data.header.section.clone());
                            }
                            let fitted_camera = {
                                let mut scene = self.scene3d.lock().unwrap();
                                scene.set_data(data.clone());
                                if let Some(camera) = self
                                    .scene
                                    .view3d
                                    .camera
                                    .filter(|camera| camera.is_usable_for(view_bounds))
                                {
                                    scene.camera = camera;
                                } else {
                                    scene.camera.preset_isometric();
                                    scene.fit();
                                }
                                scene.display.opacity = self.scene.view3d.surface_opacity;
                                scene.display.show_axes = self.scene.view3d.show_axes;
                                scene.display.show_box = self.scene.view3d.show_box;
                                scene.display.show_reference_sphere =
                                    self.scene.view3d.show_reference_sphere;
                                scene.display.reference_sphere_radius =
                                    self.scene.view3d.reference_sphere_radius;
                                scene.camera
                            };
                            self.scene.view3d.camera = Some(fitted_camera);
                            let probe_data = self.scene3d.lock().unwrap().data.clone();
                            if let Some(probe_data) = probe_data {
                                self.schedule_probe_3d(probe_data);
                            }
                            for layer in resolved_layers.into_iter().filter(|layer| {
                                layer.kind == crate::protocol::SurfaceLayerKind::Slice
                            }) {
                                let Some(axis_name) = layer.axis.as_deref() else {
                                    continue;
                                };
                                let Some(position) = layer.position else {
                                    continue;
                                };
                                let axis = match axis_name {
                                    "x" => 0,
                                    "y" => 1,
                                    _ => 2,
                                };
                                self.scene.view3d.slice_fractions[axis] = ((position
                                    - bounds[axis * 2])
                                    / (bounds[axis * 2 + 1] - bounds[axis * 2]).max(1.0e-20))
                                .clamp(0.0, 1.0);
                            }
                            self.sync_plot_appearance();
                            self.loading = false;
                            self.status = if from_cache {
                                format!(
                                    "{vertices} surface vertices · {triangles} triangles · cached"
                                )
                            } else {
                                format!("{vertices} surface vertices · {triangles} triangles")
                            };
                            self.request_fieldlines3d_for_display();
                            self.schedule_next_playback_frame();
                            self.schedule_prefetch();
                        }
                        Err(error) => {
                            self.pause_playback();
                            self.fail(error);
                        }
                    }
                }
                LoaderEvent::Plot {
                    request_id,
                    epoch,
                    priority: RequestPriority::Overlay,
                    result,
                    ..
                } if epoch == self.load_epoch => {
                    self.accept_streamline_component(request_id, result);
                }
                LoaderEvent::FieldLines3d {
                    request_id,
                    epoch,
                    key,
                    from_cache,
                    result,
                } if epoch == self.load_epoch
                    && self.active_fieldlines3d_request == Some(request_id)
                    && self.displayed_path.as_deref() == key.path.to_str() =>
                {
                    let _ = from_cache;
                    self.active_fieldlines3d_request = None;
                    self.fieldlines3d_loading = false;
                    match result {
                        Ok(lines) => {
                            self.fieldlines3d = Some(lines);
                            self.fieldlines3d_error = None;
                        }
                        Err(error) => {
                            self.fieldlines3d_error = Some(error);
                        }
                    }
                    self.schedule_next_playback_frame();
                }
                LoaderEvent::Plot { .. }
                | LoaderEvent::Surface3d { .. }
                | LoaderEvent::FieldLines3d { .. }
                | LoaderEvent::Inspected { .. } => {}
            }
        }
    }

    fn fail(&mut self, message: String) {
        self.loading = false;
        self.io_busy = false;
        self.buffering = false;
        self.status = format!("Error: {message}");
    }

    fn sync_plot_appearance(&mut self) {
        let appearance = self
            .scene
            .appearance_for(self.displayed_variable.as_deref());
        self.plot.lock().unwrap().set_appearance(&appearance);
        let mut scene3d = self.scene3d.lock().unwrap();
        scene3d.set_appearance(&appearance);
        scene3d.display.opacity = self.scene.view3d.surface_opacity.clamp(0.05, 1.0);
        scene3d.display.show_axes = self.scene.view3d.show_axes;
        scene3d.display.show_box = self.scene.view3d.show_box;
        scene3d.display.show_reference_sphere = self.scene.view3d.show_reference_sphere;
        scene3d.display.reference_sphere_radius = self.scene.view3d.reference_sphere_radius;
        let section = scene3d
            .data
            .as_ref()
            .map(|data| data.header.section.as_str());
        let layer_styles = self
            .scene
            .isosurfaces_for(section)
            .iter()
            .enumerate()
            .map(|(order, layer)| LayerDisplay3d {
                layer_id: layer.id,
                visible: layer.visible,
                opacity: layer.opacity,
                solid_color: match layer.coloring {
                    IsosurfaceColoring::Solid { color } => Some(color),
                    IsosurfaceColoring::Scalar { .. } => None,
                },
                appearance: match &layer.coloring {
                    IsosurfaceColoring::Solid { .. } => AppearanceSettings::default(),
                    IsosurfaceColoring::Scalar { appearance, .. } => appearance.clone(),
                },
                order: order as u32,
            })
            .collect();
        scene3d.set_layer_styles(layer_styles);
    }

    fn active_3d_colorbar(
        &self,
        data: &crate::protocol::Surface3dData,
    ) -> Option<(AppearanceSettings, [f32; 2], String)> {
        if let Some(id) = self.selected_isosurface {
            let layer = self
                .scene
                .isosurfaces_for(Some(&data.header.section))
                .iter()
                .find(|layer| layer.id == id)?;
            let IsosurfaceColoring::Scalar { appearance, .. } = &layer.coloring else {
                return None;
            };
            let rendered = data
                .header
                .layers
                .iter()
                .find(|candidate| candidate.layer_id == Some(id))?;
            let automatic = rendered.value_range?;
            let requested = appearance.color_limits.unwrap_or(automatic);
            let limits = if requested.into_iter().all(f32::is_finite)
                && requested[1] > requested[0]
                && (appearance.scale == Scale::Linear || requested[0] > 0.0)
            {
                requested
            } else {
                automatic
            };
            return Some((appearance.clone(), limits, rendered.unit.clone()));
        }
        let appearance = self
            .scene
            .appearance_for(self.displayed_variable.as_deref());
        let limits = self.scene3d.lock().unwrap().display.limits;
        Some((appearance, limits, data.header.unit.clone()))
    }

    fn schedule_probe_2d(&mut self, data: Arc<PlotData>) {
        self.probe_generation = self.probe_indexer.schedule_2d(data);
        self.probe_index = None;
        self.probe_indexing = true;
        self.hover_probe = None;
    }

    fn schedule_probe_3d(&mut self, data: Arc<crate::protocol::Surface3dData>) {
        self.probe_generation = self.probe_indexer.schedule_3d(data);
        self.probe_index = None;
        self.probe_indexing = true;
        self.hover_probe = None;
    }

    fn poll_probe_index(&mut self) {
        if let Some(result) = self.probe_indexer.latest()
            && result.generation == self.probe_generation
        {
            self.probe_index = Some(result.index);
            self.probe_indexing = false;
        }
    }

    fn pin_probe(&mut self, hit: ProbeHit, dimension: ProbeDimension) {
        if self.scene.measurements.len() >= MAX_PROBE_MEASUREMENTS {
            self.fail(format!(
                "A scene may contain at most {MAX_PROBE_MEASUREMENTS} probe measurements"
            ));
            return;
        }
        let (_, scope_variable, relative_path) = self.scope_values();
        let id = self.scene.allocate_measurement_id();
        self.editor.checkpoint(&self.scene);
        self.scene.measurements.push(ProbeMeasurement {
            id,
            name: format!("Probe {id}"),
            dimension,
            position: hit.position.map(f64::from),
            value: f64::from(hit.value),
            variable: hit.variable,
            unit: hit.unit,
            relative_path: relative_path.unwrap_or_default(),
            scope_variable: scope_variable.unwrap_or_default(),
            layer_id: hit.layer_id,
            visible: true,
        });
    }

    fn measurement_matches_current(&self, measurement: &ProbeMeasurement) -> bool {
        let (_, variable, relative_path) = self.scope_values();
        measurement.relative_path == relative_path.unwrap_or_default()
            && measurement.scope_variable == variable.unwrap_or_default()
    }

    fn measurement_inspector(&mut self, ui: &mut egui::Ui) {
        ui.add_space(14.0);
        section_heading(ui, "Measurements");
        if self.probe_indexing {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(RichText::new("Probe indexing…").color(MUTED));
            });
        } else {
            ui.label(
                RichText::new("Hover the plot to inspect values. Press P or select Probe to pin.")
                    .small()
                    .color(MUTED),
            );
        }
        let mut delete = None;
        let mut clear = false;
        let current: Vec<usize> = self
            .scene
            .measurements
            .iter()
            .enumerate()
            .filter_map(|(index, measurement)| {
                self.measurement_matches_current(measurement)
                    .then_some(index)
            })
            .collect();
        for index in current {
            let measurement = &mut self.scene.measurements[index];
            egui::Frame::group(ui.style()).show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.checkbox(&mut measurement.visible, "");
                    ui.text_edit_singleline(&mut measurement.name);
                    if ui.small_button("Delete").clicked() {
                        delete = Some(index);
                    }
                });
                ui.monospace(format!(
                    "({:.3}, {:.3}, {:.3})  {:.6e}{}",
                    measurement.position[0],
                    measurement.position[1],
                    measurement.position[2],
                    measurement.value,
                    measurement
                        .unit
                        .as_deref()
                        .map_or(String::new(), |unit| format!(" {unit}"))
                ));
            });
        }
        if (delete.is_some() || !self.scene.measurements.is_empty())
            && ui.button("Clear visible plot measurements").clicked()
        {
            clear = true;
        }
        if let Some(index) = delete {
            self.editor.checkpoint(&self.scene);
            self.scene.measurements.remove(index);
        }
        if clear {
            self.editor.checkpoint(&self.scene);
            let (_, variable, relative_path) = self.scope_values();
            let variable = variable.unwrap_or_default();
            let relative_path = relative_path.unwrap_or_default();
            self.scene.measurements.retain(|measurement| {
                measurement.scope_variable != variable || measurement.relative_path != relative_path
            });
        }
    }

    fn request_fieldlines3d_for_display(&mut self) {
        self.loader.cancel_auxiliary();
        self.active_fieldlines3d_request = None;
        self.fieldlines3d_loading = false;
        self.fieldlines3d_error = None;

        let Some(path) = self.displayed_path.clone() else {
            return;
        };
        let data = self.scene3d.lock().unwrap().data.clone();
        let Some(data) = data else { return };
        let settings = self.scene.fieldlines3d_for(Some(&data.header.section));
        if !settings.enabled {
            self.fieldlines3d = None;
            return;
        }
        let [Some(x), Some(y), Some(z)] = settings.components.clone() else {
            self.fieldlines3d_error = Some("Choose all three vector components".to_owned());
            return;
        };
        if x == y || x == z || y == z {
            self.fieldlines3d_error = Some("Vector components must be different".to_owned());
            return;
        }
        let mut seeds: Vec<[f64; 3]> = latitude_footpoints3d(&settings.latitude_seeds)
            .into_iter()
            .chain(settings.custom_seeds.iter().copied())
            .map(DataPoint3::as_array)
            .collect();
        if let Some(region) = settings.seed_region {
            seeds.extend(
                seed_grid3d(region, settings.region_counts)
                    .into_iter()
                    .map(DataPoint3::as_array),
            );
        }
        if seeds.is_empty() {
            self.fieldlines3d_error =
                Some("Enable planetary footpoints or add seeds in a custom region".to_owned());
            return;
        }
        match self.loader.trace_fieldlines3d(
            self.load_epoch,
            path,
            0,
            FieldLineTrace3dRequest {
                components: [x, y, z],
                seeds,
                step: settings.step_size,
                max_steps: settings.max_steps,
                max_length: settings.max_length,
                planet_radius: self.scene.view3d.reference_sphere_radius,
                crop: Crop3dRequest {
                    enabled: self.scene.view3d.crop.enabled,
                    fractions: self.scene.view3d.crop.fractions,
                },
            },
        ) {
            Ok(request_id) => {
                self.active_fieldlines3d_request = Some(request_id);
                self.fieldlines3d_loading = true;
            }
            Err(error) => self.fieldlines3d_error = Some(error),
        }
    }

    fn request_streamlines_for_display(&mut self) {
        self.streamline_generation = self.streamline_generation.wrapping_add(1).max(1);
        self.loader.cancel_auxiliary();
        self.pending_streamlines = None;
        self.vector_field = None;
        self.streamline_loading = false;
        self.streamline_error = None;

        let Some(path) = self.displayed_path.clone() else {
            return;
        };
        let data = self.plot.lock().unwrap().data.clone();
        let Some(data) = data else { return };
        let section = data.header.section.clone();
        let settings = self.scene.streamlines_for(section.as_deref());
        if !settings.enabled {
            self.streamline_overlay = None;
            self.placing_streamline_seed = false;
            return;
        }
        let (Some(horizontal_component), Some(vertical_component)) = (
            settings.horizontal_component.clone(),
            settings.vertical_component.clone(),
        ) else {
            self.streamline_overlay = None;
            self.streamline_error = Some("Choose both vector components".to_owned());
            return;
        };
        if horizontal_component == vertical_component {
            self.streamline_overlay = None;
            self.streamline_error = Some("Vector components must be different".to_owned());
            return;
        }
        let overlay_is_compatible = self.streamline_overlay.as_ref().is_some_and(|overlay| {
            streamline_overlay_matches(
                overlay,
                section.as_deref(),
                &horizontal_component,
                &vertical_component,
            )
        });
        if overlay_is_compatible {
            if let Some(overlay) = &mut self.streamline_overlay {
                overlay.settings = settings.clone();
            }
        } else {
            self.streamline_overlay = None;
        }
        let horizontal_key = match PlotKey::for_file(&path, horizontal_component.clone(), 0) {
            Ok(key) => key,
            Err(error) => {
                self.streamline_overlay = None;
                self.streamline_error = Some(error);
                return;
            }
        };
        let vertical_key = match PlotKey::for_file(&path, vertical_component.clone(), 0) {
            Ok(key) => key,
            Err(error) => {
                self.streamline_overlay = None;
                self.streamline_error = Some(error);
                return;
            }
        };
        let reuse_mesh = Some(data.mesh.clone());
        let horizontal_request = self.loader.load(
            self.load_epoch,
            horizontal_key,
            RequestPriority::Overlay,
            reuse_mesh.clone(),
        );
        let vertical_request = self.loader.load(
            self.load_epoch,
            vertical_key,
            RequestPriority::Overlay,
            reuse_mesh,
        );
        self.pending_streamlines = Some(PendingStreamlineLoad {
            generation: self.streamline_generation,
            path,
            section,
            horizontal_component,
            vertical_component,
            horizontal_request,
            vertical_request,
            horizontal: None,
            vertical: None,
        });
        self.streamline_loading = true;
    }

    fn accept_streamline_component(
        &mut self,
        request_id: u64,
        result: Result<Arc<PlotData>, String>,
    ) {
        let mut ready = None;
        let mut failed = None;
        if let Some(pending) = &mut self.pending_streamlines {
            let target = if pending.horizontal_request == request_id {
                Some(&mut pending.horizontal)
            } else if pending.vertical_request == request_id {
                Some(&mut pending.vertical)
            } else {
                None
            };
            if let Some(target) = target {
                match result {
                    Ok(data) => *target = Some(data),
                    Err(error) => failed = Some(error),
                }
                if failed.is_none()
                    && let (Some(horizontal), Some(vertical)) =
                        (pending.horizontal.clone(), pending.vertical.clone())
                {
                    ready = Some((
                        pending.generation,
                        pending.path.clone(),
                        pending.section.clone(),
                        pending.horizontal_component.clone(),
                        pending.vertical_component.clone(),
                        horizontal,
                        vertical,
                    ));
                }
            }
        }
        if let Some(error) = failed {
            self.pending_streamlines = None;
            self.streamline_loading = false;
            self.streamline_overlay = None;
            self.streamline_error = Some(error);
            self.schedule_next_playback_frame();
        } else if let Some((
            generation,
            path,
            section,
            horizontal_name,
            vertical_name,
            horizontal,
            vertical,
        )) = ready
        {
            self.pending_streamlines = None;
            let settings = self.scene.streamlines_for(section.as_deref());
            let sender = self.sender.clone();
            thread::spawn(move || {
                let result = VectorField::new(horizontal, vertical).map(|field| {
                    let field = Arc::new(field);
                    let lines = field.integrate(&settings);
                    (field, lines)
                });
                let _ = sender.send(Event::StreamlinesComputed {
                    generation,
                    path,
                    section,
                    horizontal_component: horizontal_name,
                    vertical_component: vertical_name,
                    result,
                });
            });
        }
    }

    fn recompute_streamlines(&mut self) {
        self.streamline_generation = self.streamline_generation.wrapping_add(1).max(1);
        let generation = self.streamline_generation;
        let Some(active) = &self.vector_field else {
            self.request_streamlines_for_display();
            return;
        };
        let settings = self.scene.streamlines_for(active.section.as_deref());
        if !settings.enabled
            || settings.horizontal_component.as_deref()
                != Some(active.horizontal_component.as_str())
            || settings.vertical_component.as_deref() != Some(active.vertical_component.as_str())
            || self.displayed_path.as_deref() != Some(active.path.as_str())
        {
            self.request_streamlines_for_display();
            return;
        }
        let path = active.path.clone();
        let section = active.section.clone();
        let horizontal_component = active.horizontal_component.clone();
        let vertical_component = active.vertical_component.clone();
        let field = active.field.clone();
        let sender = self.sender.clone();
        self.streamline_loading = true;
        thread::spawn(move || {
            let lines = field.integrate(&settings);
            let _ = sender.send(Event::StreamlinesComputed {
                generation,
                path,
                section,
                horizontal_component,
                vertical_component,
                result: Ok((field, lines)),
            });
        });
    }

    fn update_streamline_style(&mut self) {
        if let Some(overlay) = &mut self.streamline_overlay {
            overlay.settings = self.scene.streamlines_for(overlay.section.as_deref());
        }
    }

    fn stash_current_scene(&mut self) {
        let Some(key) = self.current_run_key.clone() else {
            return;
        };
        if self.view_mode == ViewMode::ThreeD {
            let shared = self.scene3d.lock().unwrap();
            if shared.data.is_some() {
                self.scene.view3d.camera = Some(shared.camera);
            }
        }
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
        self.scene = restored
            .and_then(|scene| scene.migrate().ok())
            .unwrap_or_default();
        self.refresh_surface_drafts();
        if self.scene.source_run.is_none() {
            self.scene.source_run = Path::new(&directory)
                .file_name()
                .and_then(|name| name.to_str())
                .map(str::to_owned);
        }
        self.current_run_key = Some(key);
        self.editor = AnnotationEditor::default();
    }

    fn timeline_indices(&self) -> Vec<usize> {
        let Some(path) = self
            .selected_path
            .as_deref()
            .or(self.displayed_path.as_deref())
        else {
            return Vec::new();
        };
        timeline_indices(&self.files, path)
    }

    fn timeline_position(&self, timeline: &[usize]) -> Option<usize> {
        let path = self
            .selected_path
            .as_deref()
            .or(self.displayed_path.as_deref())?;
        timeline
            .iter()
            .position(|index| self.files[*index].path == path)
    }

    fn displayed_timeline_position(&self, timeline: &[usize]) -> Option<usize> {
        let path = self.displayed_path.as_deref()?;
        timeline
            .iter()
            .position(|index| self.files[*index].path == path)
    }

    fn request_timeline_position(&mut self, position: usize, manual: bool) {
        let timeline = self.timeline_indices();
        let Some(file_index) = timeline.get(position).copied() else {
            return;
        };
        let path = self.files[file_index].path.clone();
        if manual {
            self.pause_playback();
        }
        self.selected_path = Some(path);
        if let Some(variable) = self.selected_variable.clone() {
            self.request_selected_plot(variable);
        } else if let Some(path) = self.selected_path.clone() {
            self.inspect(path);
        }
    }

    fn frame_duration(&self) -> Duration {
        Duration::from_secs_f32(1.0 / self.playback_fps.clamp(0.5, 30.0))
    }

    fn schedule_next_playback_frame(&mut self) {
        if !self.playing {
            self.buffering = false;
            self.next_frame_at = None;
            return;
        }
        if self.loading || self.streamline_loading || self.fieldlines3d_loading {
            self.buffering = true;
            self.next_frame_at = None;
        } else {
            self.buffering = false;
            self.next_frame_at = Some(Instant::now() + self.frame_duration());
        }
    }

    fn pause_playback(&mut self) {
        self.playing = false;
        self.buffering = false;
        self.next_frame_at = None;
    }

    fn toggle_playback(&mut self) {
        if self.playing {
            self.pause_playback();
        } else if self.timeline_indices().len() > 1
            && match self.view_mode {
                ViewMode::TwoD => self.plot.lock().unwrap().data.is_some(),
                ViewMode::ThreeD => self.scene3d.lock().unwrap().data.is_some(),
            }
        {
            self.playing = true;
            self.schedule_next_playback_frame();
        }
    }

    fn playback_tick(&mut self) {
        if !self.playing {
            return;
        }
        if self.loading || self.streamline_loading || self.fieldlines3d_loading {
            self.buffering = true;
            self.next_frame_at = None;
            return;
        }
        let now = Instant::now();
        let frame_duration = self.frame_duration();
        let deadline = self.next_frame_at.get_or_insert(now + frame_duration);
        if now < *deadline {
            return;
        }
        let timeline = self.timeline_indices();
        let Some(position) = self.displayed_timeline_position(&timeline) else {
            self.pause_playback();
            return;
        };
        let Some(next) = next_playback_position(position, timeline.len(), self.playback_loop)
        else {
            self.pause_playback();
            return;
        };
        self.buffering = true;
        self.next_frame_at = None;
        self.request_timeline_position(next, false);
    }

    fn slice_debounce_tick(&mut self, context: &egui::Context) {
        if self.view_mode != ViewMode::ThreeD
            || self
                .slice_changed_at
                .is_none_or(|changed| changed.elapsed() < Duration::from_millis(100))
            || context.input(|input| input.pointer.primary_down())
        {
            return;
        }
        self.slice_changed_at = None;
        if self
            .scene
            .view3d
            .slice_enabled
            .into_iter()
            .any(|enabled| enabled)
            && let Some(variable) = self.selected_variable.clone()
        {
            self.request_selected_plot(variable);
        }
    }

    fn schedule_prefetch(&mut self) {
        let timeline = self.timeline_indices();
        if timeline.len() < 2 {
            return;
        }
        let Some(position) = self.displayed_timeline_position(&timeline) else {
            return;
        };
        let Some(variable) = self.displayed_variable.clone() else {
            return;
        };
        let previous = if position > 0 {
            Some(position - 1)
        } else {
            self.playback_loop.then_some(timeline.len() - 1)
        };
        let next = if position + 1 < timeline.len() {
            Some(position + 1)
        } else {
            self.playback_loop.then_some(0)
        };
        if self.view_mode == ViewMode::ThreeD {
            let reuse_mesh = self
                .scene3d
                .lock()
                .unwrap()
                .data
                .as_ref()
                .map(|data| data.mesh.clone());
            let (planes, isosurfaces, crop) = self.surface3d_request_parts();
            for neighbor in [previous, next].into_iter().flatten() {
                let path = self.files[timeline[neighbor]].path.clone();
                if let Ok(key) =
                    Surface3dKey::for_file(path, variable.clone(), 0, &planes, &isosurfaces, crop)
                {
                    self.loader.load_surface3d(
                        self.load_epoch,
                        key,
                        RequestPriority::Prefetch,
                        reuse_mesh.clone(),
                    );
                }
            }
            return;
        }
        let reuse_mesh = self
            .plot
            .lock()
            .unwrap()
            .data
            .as_ref()
            .map(|data| data.mesh.clone());
        for neighbor in [previous, next].into_iter().flatten() {
            let path = self.files[timeline[neighbor]].path.clone();
            if let Ok(key) = PlotKey::for_file(path, variable.clone(), 0) {
                self.loader.load(
                    self.load_epoch,
                    key,
                    RequestPriority::Prefetch,
                    reuse_mesh.clone(),
                );
            }
        }
    }

    fn save_scene_dialog(&mut self) {
        self.io_busy = true;
        if self.view_mode == ViewMode::ThreeD {
            self.scene.view3d.camera = Some(self.scene3d.lock().unwrap().camera);
        }
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
                    let scene = scene.migrate()?;
                    Ok((path, scene))
                })
                .transpose();
            let _ = sender.send(Event::SceneLoaded(Box::new(result)));
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
            self.show_export_dialog = match self.view_mode {
                ViewMode::TwoD => self.plot.lock().unwrap().data.is_some(),
                ViewMode::ThreeD => self.scene3d.lock().unwrap().data.is_some(),
            };
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
            self.recompute_streamlines();
        }
        if redo {
            self.editor.redo(&mut self.scene);
            self.sync_plot_appearance();
            self.recompute_streamlines();
        }
        if !context.egui_wants_keyboard_input() {
            if context.input(|input| input.key_pressed(egui::Key::Escape)) {
                self.placing_streamline_seed = false;
            }
            if context.input(|input| input.key_pressed(egui::Key::Space)) {
                self.toggle_playback();
            }
            if context.input(|input| input.key_pressed(egui::Key::P)) {
                self.probe_mode = !self.probe_mode;
                self.placing_streamline_seed = false;
                self.editor.cancel_drawing();
                self.editor.tool = DrawingTool::Select;
                self.inspector_tab = InspectorTab::Data;
            }
            if context.input(|input| {
                input.key_pressed(egui::Key::Delete) || input.key_pressed(egui::Key::Backspace)
            }) {
                self.editor.delete_selected(&mut self.scene);
            }
            if context.input(|input| input.key_pressed(egui::Key::F)) {
                match self.view_mode {
                    ViewMode::TwoD => self.plot.lock().unwrap().reset_view(),
                    ViewMode::ThreeD => {
                        let mut scene = self.scene3d.lock().unwrap();
                        scene.fit();
                        if scene.data.is_some() {
                            self.scene.view3d.camera = Some(scene.camera);
                        }
                    }
                }
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
                        ui.label(
                            RichText::new("SCIENTIFIC DATA VIEWER")
                                .size(9.0)
                                .color(MUTED),
                        );
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
                    if self.view_mode == ViewMode::ThreeD
                        && ui
                            .add_enabled(
                                self.selected_variable.is_some()
                                    && self.isosurface_drafts.len() < MAX_ISOSURFACE_LAYERS,
                                egui::Button::new("+ Isosurface"),
                            )
                            .on_hover_text(
                                "Add an optional 3D isosurface from the selected variable",
                            )
                            .clicked()
                    {
                        self.add_isosurface_draft();
                    }
                    let has_plot = match self.view_mode {
                        ViewMode::TwoD => self.plot.lock().unwrap().data.is_some(),
                        ViewMode::ThreeD => self.scene3d.lock().unwrap().data.is_some(),
                    };
                    if ui
                        .add_enabled(has_plot, egui::Button::new("Export PNG"))
                        .on_hover_text("Export the plot as PNG  Ctrl/Cmd+E")
                        .clicked()
                    {
                        self.show_export_dialog = true;
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
                    egui::ScrollArea::vertical().show_rows(ui, 62.0, visible.len(), |ui, range| {
                        for row in range {
                            let file = &self.files[visible[row]];
                            let selected = self.selected_path.as_deref() == Some(&file.path);
                            let mut details = Vec::new();
                            if let Some(section) = &file.section {
                                details.push(section.clone());
                            }
                            if let Some(time) = file.time_step {
                                details.push(format!("t={time}"));
                            }
                            if let Some(dump) = file.dump_index {
                                details.push(format!("n={dump}"));
                            }
                            details.push(format!("{:.1} MB", file.size as f64 / 1_048_576.0));
                            if selectable_metadata_row(
                                ui,
                                selected,
                                &file.name,
                                &details.join("  ·  "),
                            )
                            .on_hover_text(&file.path)
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
                let tabs: &[InspectorTab] = if self.view_mode == ViewMode::ThreeD {
                    &InspectorTab::THREE_D
                } else {
                    &InspectorTab::TWO_D
                };
                ui.horizontal(|ui| {
                    let count = tabs.len() as f32;
                    let width = (ui.available_width()
                        - (count - 1.0) * ui.spacing().item_spacing.x)
                        / count;
                    for &tab in tabs {
                        let (short_name, full_name) = if self.view_mode == ViewMode::ThreeD
                            && tab == InspectorTab::Annotations
                        {
                            ("Scene", "3D scene")
                        } else {
                            (tab.short_name(), tab.name())
                        };
                        if ui
                            .add_sized(
                                [width, 32.0],
                                egui::Button::selectable(self.inspector_tab == tab, short_name),
                            )
                            .on_hover_text(full_name)
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
                    InspectorTab::Surfaces => self.surface3d_inspector(ui),
                    InspectorTab::Annotations if self.view_mode == ViewMode::ThreeD => {
                        self.scene3d_inspector(ui)
                    }
                    InspectorTab::Annotations => self.annotation_inspector(ui),
                    InspectorTab::FieldLines if self.view_mode == ViewMode::ThreeD => {
                        self.fieldline3d_inspector(ui)
                    }
                    InspectorTab::FieldLines => self.streamline_inspector(ui),
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
                        let mut secondary = format!("Source: {}", variable.source);
                        if let Some(unit) = &variable.unit {
                            secondary.push_str(&format!("  ·  [{unit}]"));
                        }
                        if selectable_metadata_row(ui, selected, &variable.canonical, &secondary)
                            .on_hover_text(format!("{} · {}", variable.canonical, secondary))
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

        if self.view_mode == ViewMode::ThreeD {
            ui.add_space(14.0);
            section_heading(ui, "3D dataset");
            if let Some(zone) = self.info.as_ref().and_then(|info| info.zones.first()) {
                ui.label(format!("{} · {} points", zone.zone_type, zone.num_points));
                ui.label(
                    RichText::new(format!("{} volume cells", zone.num_elements))
                        .small()
                        .color(MUTED),
                );
            }
            ui.small(
                RichText::new("Rotate: left drag · Pan: right drag · Zoom: wheel · Fit: F")
                    .color(MUTED),
            );
            self.measurement_inspector(ui);
            self.performance_inspector(ui);
            return;
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
        drop(shared);

        self.measurement_inspector(ui);
        self.performance_inspector(ui);
    }

    fn performance_inspector(&mut self, ui: &mut egui::Ui) {
        ui.add_space(14.0);
        section_heading(ui, "Performance");
        let before = self.cache_limit_mib;
        ui.horizontal(|ui| {
            ui.label("Memory cache");
            ui.add(
                egui::DragValue::new(&mut self.cache_limit_mib)
                    .range(64..=8192)
                    .speed(64)
                    .suffix(" MiB"),
            );
        });
        if self.cache_limit_mib != before {
            self.loader
                .set_limit_bytes(mib_to_bytes(self.cache_limit_mib));
        }
        let used_mib = self.cache_stats.used_bytes as f64 / (1024.0 * 1024.0);
        ui.label(
            RichText::new(format!(
                "{used_mib:.1} MiB used · {} cached frames",
                self.cache_stats.entries
            ))
            .small()
            .color(MUTED),
        );
        if ui.button("Clear cache").clicked() {
            self.loader.clear();
        }
    }

    fn surface3d_inspector(&mut self, ui: &mut egui::Ui) {
        section_heading(ui, "3D surfaces");
        ui.label(
            RichText::new(
                "Slices remain available by default. Isosurfaces are added only when you choose to create one.",
            )
            .small()
            .color(MUTED),
        );
        if ui
            .add_enabled(
                self.selected_variable.is_some()
                    && self.isosurface_drafts.len() < MAX_ISOSURFACE_LAYERS,
                egui::Button::new("+ Add isosurface"),
            )
            .clicked()
        {
            self.add_isosurface_draft();
        }
        if ui
            .selectable_label(
                self.selected_isosurface.is_none(),
                "Slice group · use the plot colorbar",
            )
            .clicked()
        {
            self.selected_isosurface = None;
        }

        ui.add_space(14.0);
        section_heading(ui, "Shared crop box");
        let bounds = self
            .scene3d
            .lock()
            .unwrap()
            .data
            .as_ref()
            .map(|data| data.header.bounds);
        let mut crop = self.crop_draft;
        ui.checkbox(&mut crop.enabled, "Crop all 3D data geometry");
        if crop.enabled {
            for (axis, label) in ["X", "Y", "Z"].into_iter().enumerate() {
                ui.horizontal(|ui| {
                    ui.label(label);
                    let low = axis * 2;
                    let high = low + 1;
                    let high_fraction = crop.fractions[high];
                    ui.add(
                        egui::DragValue::new(&mut crop.fractions[low])
                            .range(0.0..=high_fraction - 0.001)
                            .speed(0.005)
                            .prefix("min "),
                    );
                    let low_fraction = crop.fractions[low];
                    ui.add(
                        egui::DragValue::new(&mut crop.fractions[high])
                            .range(low_fraction + 0.001..=1.0)
                            .speed(0.005)
                            .prefix("max "),
                    );
                    if let Some(bounds) = bounds {
                        let span = bounds[high] - bounds[low];
                        let actual_low = bounds[low] + crop.fractions[low] * span;
                        let actual_high = bounds[low] + crop.fractions[high] * span;
                        ui.label(
                            RichText::new(format!("{actual_low:.2}…{actual_high:.2}"))
                                .small()
                                .color(MUTED),
                        );
                    }
                });
            }
        }
        self.crop_draft = crop;
        ui.horizontal(|ui| {
            if ui.button("Apply crop").clicked() {
                self.editor.checkpoint(&self.scene);
                self.scene.view3d.crop = self.crop_draft;
                if let Some(variable) = self.selected_variable.clone() {
                    self.request_selected_plot(variable);
                }
            }
            if ui.button("Reset full domain").clicked() {
                self.crop_draft = CropBox3d::default();
                self.scene.view3d.crop = self.crop_draft;
                if let Some(variable) = self.selected_variable.clone() {
                    self.request_selected_plot(variable);
                }
            }
        });

        ui.add_space(14.0);
        section_heading(ui, "Isosurface layers");
        let variables = self
            .info
            .as_ref()
            .map(|info| {
                info.variables
                    .iter()
                    .filter(|variable| !is_coordinate(&variable.source))
                    .map(|variable| (variable.canonical.clone(), variable.source.clone()))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let rendered_layers = self
            .scene3d
            .lock()
            .unwrap()
            .data
            .as_ref()
            .map(|data| data.header.layers.clone())
            .unwrap_or_default();
        let section = self.current_3d_section().to_owned();
        let applied_order = self
            .scene
            .isosurfaces_for(Some(&section))
            .iter()
            .map(|layer| layer.id)
            .collect::<Vec<_>>();
        let mut ids = applied_order.clone();
        let draft_only = self
            .isosurface_drafts
            .keys()
            .copied()
            .filter(|id| !ids.contains(id))
            .collect::<Vec<_>>();
        ids.extend(draft_only);
        if ids.is_empty() {
            ui.label(
                RichText::new("No isosurfaces have been added.")
                    .italics()
                    .color(MUTED),
            );
        }
        for id in ids {
            let Some(mut draft) = self.isosurface_drafts.get(&id).cloned() else {
                continue;
            };
            let before = draft.clone();
            let applied = applied_order.contains(&id);
            let selected = self.selected_isosurface == Some(id);
            let mut apply = false;
            let mut duplicate = false;
            let mut delete = false;
            let mut move_by = 0_i32;
            egui::Frame::group(ui.style())
                .fill(if selected {
                    Color32::from_rgb(24, 38, 53)
                } else {
                    PANEL_BG
                })
                .inner_margin(egui::Margin::same(9))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        if ui
                            .selectable_label(selected, if applied { "Surface" } else { "Draft" })
                            .clicked()
                        {
                            self.selected_isosurface = Some(id);
                        }
                        ui.text_edit_singleline(&mut draft.name);
                        ui.checkbox(&mut draft.visible, "Visible");
                    });
                    ui.horizontal(|ui| {
                        ui.label("Variable");
                        egui::ComboBox::from_id_salt(("iso_variable", id))
                            .selected_text(&draft.variable)
                            .show_ui(ui, |ui| {
                                for (canonical, source) in &variables {
                                    ui.selectable_value(
                                        &mut draft.variable,
                                        canonical.clone(),
                                        format!("{canonical}  ·  {source}"),
                                    );
                                }
                            });
                    });
                    ui.horizontal(|ui| {
                        ui.label("Exact isovalue");
                        let speed = draft.isovalue.abs().max(1.0) * 0.002;
                        ui.add(egui::DragValue::new(&mut draft.isovalue).speed(speed));
                    });
                    ui.add(egui::Slider::new(&mut draft.opacity, 0.05..=1.0).text("Opacity"));
                    let scalar = matches!(draft.coloring, IsosurfaceColoring::Scalar { .. });
                    let mut scalar_mode = scalar;
                    ui.horizontal(|ui| {
                        ui.label("Color");
                        ui.selectable_value(&mut scalar_mode, false, "Solid");
                        ui.selectable_value(&mut scalar_mode, true, "Scalar variable");
                    });
                    if scalar_mode != scalar {
                        draft.coloring = if scalar_mode {
                            IsosurfaceColoring::Scalar {
                                variable: draft.variable.clone(),
                                appearance: self.scene.appearance_for(Some(&draft.variable)),
                            }
                        } else {
                            IsosurfaceColoring::Solid {
                                color: RgbaColor::default(),
                            }
                        };
                    }
                    match &mut draft.coloring {
                        IsosurfaceColoring::Solid { color } => {
                            let mut edited = color.to_egui();
                            if ui.color_edit_button_srgba(&mut edited).changed() {
                                *color = RgbaColor::from_egui(edited);
                            }
                        }
                        IsosurfaceColoring::Scalar {
                            variable,
                            appearance,
                        } => {
                            egui::ComboBox::from_id_salt(("iso_color_variable", id))
                                .selected_text(variable.as_str())
                                .show_ui(ui, |ui| {
                                    for (canonical, source) in &variables {
                                        ui.selectable_value(
                                            variable,
                                            canonical.clone(),
                                            format!("{canonical}  ·  {source}"),
                                        );
                                    }
                                });
                            ui.horizontal(|ui| {
                                ui.label("Colormap");
                                egui::ComboBox::from_id_salt(("iso_colormap", id))
                                    .selected_text(appearance.colormap.name())
                                    .show_ui(ui, |ui| {
                                        for map in Colormap::ALL {
                                            ui.selectable_value(
                                                &mut appearance.colormap,
                                                map,
                                                map.name(),
                                            );
                                        }
                                    });
                                ui.checkbox(&mut appearance.reversed, "Reverse");
                            });
                            ui.horizontal(|ui| {
                                ui.selectable_value(&mut appearance.scale, Scale::Linear, "Linear");
                                ui.selectable_value(
                                    &mut appearance.scale,
                                    Scale::Logarithmic,
                                    "Log",
                                );
                            });
                            ui.collapsing("Scalar colorbar options", |ui| {
                                paint_colormap_preview(ui, appearance);
                                ui.horizontal(|ui| {
                                    if ui
                                        .selectable_label(
                                            appearance.color_mode == ColorMode::Continuous,
                                            "Continuous",
                                        )
                                        .clicked()
                                    {
                                        appearance.color_mode = ColorMode::Continuous;
                                    }
                                    let discrete =
                                        matches!(appearance.color_mode, ColorMode::Discrete { .. });
                                    if ui.selectable_label(discrete, "Discrete").clicked()
                                        && !discrete
                                    {
                                        appearance.color_mode = ColorMode::Discrete { bins: 10 };
                                    }
                                });
                                if let ColorMode::Discrete { bins } = &mut appearance.color_mode {
                                    ui.add(egui::Slider::new(bins, 2..=32).text("Bins"));
                                }
                                let effective_limits = rendered_layers
                                    .iter()
                                    .find(|layer| layer.layer_id == Some(id))
                                    .and_then(|layer| layer.value_range)
                                    .unwrap_or([0.0, 1.0]);
                                let mut automatic = appearance.color_limits.is_none();
                                if ui.checkbox(&mut automatic, "Automatic limits").changed() {
                                    appearance.color_limits =
                                        (!automatic).then_some(effective_limits);
                                }
                                if let Some(limits) = &mut appearance.color_limits {
                                    let speed = color_limit_speed(*limits);
                                    ui.horizontal(|ui| {
                                        ui.add(egui::DragValue::new(&mut limits[0]).speed(speed));
                                        ui.label("to");
                                        ui.add(egui::DragValue::new(&mut limits[1]).speed(speed));
                                    });
                                    if !limits.iter().all(|value| value.is_finite())
                                        || limits[1] <= limits[0]
                                        || (appearance.scale == Scale::Logarithmic
                                            && limits[0] <= 0.0)
                                    {
                                        ui.colored_label(
                                            Color32::from_rgb(241, 126, 126),
                                            "Enter ordered finite limits (positive for log scale).",
                                        );
                                    }
                                }
                                tick_controls(ui, appearance, effective_limits);
                            });
                        }
                    }
                    ui.horizontal(|ui| {
                        ui.label("Triangle budget");
                        egui::ComboBox::from_id_salt(("iso_budget", id))
                            .selected_text(match draft.mesh_budget {
                                MeshBudget::Auto => "Auto · 500k".to_owned(),
                                MeshBudget::Limited(limit) => format!("{}k", limit / 1000),
                                MeshBudget::Full => "Full".to_owned(),
                            })
                            .show_ui(ui, |ui| {
                                ui.selectable_value(
                                    &mut draft.mesh_budget,
                                    MeshBudget::Auto,
                                    "Auto · 500k",
                                );
                                for limit in [100_000, 250_000, 500_000, 1_000_000, 2_000_000] {
                                    ui.selectable_value(
                                        &mut draft.mesh_budget,
                                        MeshBudget::Limited(limit),
                                        format!("{}k", limit / 1000),
                                    );
                                }
                                ui.selectable_value(
                                    &mut draft.mesh_budget,
                                    MeshBudget::Full,
                                    "Full",
                                );
                            });
                    });
                    if !draft.isovalue.is_finite() || draft.variable.trim().is_empty() {
                        ui.colored_label(
                            Color32::from_rgb(241, 126, 126),
                            "Choose a variable and enter a finite isovalue.",
                        );
                    }
                    if let Some(header) = rendered_layers
                        .iter()
                        .find(|layer| layer.layer_id == Some(id))
                    {
                        if let Some(error) = &header.inactive_reason {
                            ui.colored_label(Color32::from_rgb(241, 166, 92), error);
                        } else {
                            ui.label(
                                RichText::new(format!(
                                    "{} triangles{}",
                                    header.rendered_triangles,
                                    if header.source_triangles > header.rendered_triangles {
                                        format!(" · reduced from {}", header.source_triangles)
                                    } else {
                                        String::new()
                                    }
                                ))
                                .small()
                                .color(MUTED),
                            );
                        }
                    }
                    ui.horizontal_wrapped(|ui| {
                        apply = ui
                            .add_enabled(
                                draft.isovalue.is_finite() && !draft.variable.trim().is_empty(),
                                egui::Button::new(if applied { "Apply changes" } else { "Apply" }),
                            )
                            .clicked();
                        duplicate = ui.button("Duplicate").clicked();
                        ui.add_enabled_ui(applied, |ui| {
                            if ui.small_button("Up").clicked() {
                                move_by = -1;
                            }
                            if ui.small_button("Down").clicked() {
                                move_by = 1;
                            }
                        });
                        delete = ui.button("Delete").clicked();
                        ui.checkbox(&mut draft.locked, "Lock");
                    });
                });
            if draft != before {
                self.isosurface_drafts.insert(id, draft.clone());
                self.sync_isosurface_style(&draft);
            }
            if apply {
                self.apply_isosurface_draft(draft.clone());
            }
            if duplicate && self.isosurface_drafts.len() < MAX_ISOSURFACE_LAYERS {
                let duplicate_id = self.scene.allocate_isosurface_id();
                let mut copy = draft.clone();
                copy.id = duplicate_id;
                copy.name = format!("{} copy", copy.name);
                self.isosurface_drafts.insert(duplicate_id, copy);
                self.selected_isosurface = Some(duplicate_id);
            }
            if move_by != 0 && applied {
                let mut layers = self.scene.isosurfaces_for(Some(&section)).to_vec();
                if let Some(index) = layers.iter().position(|layer| layer.id == id) {
                    let target =
                        (index as i32 + move_by).clamp(0, layers.len() as i32 - 1) as usize;
                    let layer = layers.remove(index);
                    layers.insert(target, layer);
                    self.scene.set_isosurfaces_for(Some(&section), layers);
                    self.sync_plot_appearance();
                }
            }
            if delete {
                let mut layers = self.scene.isosurfaces_for(Some(&section)).to_vec();
                let was_applied = layers.iter().any(|layer| layer.id == id);
                layers.retain(|layer| layer.id != id);
                self.scene.set_isosurfaces_for(Some(&section), layers);
                self.isosurface_drafts.remove(&id);
                self.selected_isosurface = None;
                if was_applied
                    && self.scene.isosurfaces_for(Some(&section)).is_empty()
                    && !self
                        .scene
                        .view3d
                        .slice_enabled
                        .into_iter()
                        .any(|enabled| enabled)
                {
                    self.scene.view3d.slice_enabled[0] = true;
                }
                if was_applied && let Some(variable) = self.selected_variable.clone() {
                    self.request_selected_plot(variable);
                }
            }
            ui.add_space(7.0);
        }
    }

    fn scene3d_inspector(&mut self, ui: &mut egui::Ui) {
        section_heading(ui, "Slice surfaces");
        ui.label(
            RichText::new("Enable the planes you want to see, then drag their positions.")
                .small()
                .color(MUTED),
        );
        let bounds = self
            .scene3d
            .lock()
            .unwrap()
            .data
            .as_ref()
            .map(|data| data.header.bounds);
        let mut reload = false;
        for (index, label) in ["X", "Y", "Z"].into_iter().enumerate() {
            ui.horizontal(|ui| {
                if ui
                    .checkbox(&mut self.scene.view3d.slice_enabled[index], label)
                    .changed()
                {
                    reload = true;
                }
                if let Some(bounds) = bounds {
                    let low = bounds[index * 2];
                    let high = bounds[index * 2 + 1];
                    let mut actual = low
                        + self.scene.view3d.slice_fractions[index].clamp(0.0, 1.0) * (high - low);
                    let response = ui.add(
                        egui::Slider::new(&mut actual, low..=high)
                            .show_value(true)
                            .custom_formatter(|value, _| format!("{value:.3}")),
                    );
                    if response.changed() {
                        self.scene.view3d.slice_auto_origin[index] = false;
                        self.scene.view3d.slice_fractions[index] =
                            ((actual - low) / (high - low).max(1.0e-20)).clamp(0.0, 1.0);
                        self.slice_changed_at = Some(Instant::now());
                        reload |= response.drag_stopped();
                    }
                } else {
                    let response = ui.add(
                        egui::Slider::new(&mut self.scene.view3d.slice_fractions[index], 0.0..=1.0)
                            .show_value(false),
                    );
                    if response.changed() {
                        self.scene.view3d.slice_auto_origin[index] = false;
                        self.slice_changed_at = Some(Instant::now());
                        reload |= response.drag_stopped();
                    }
                }
            });
        }
        let has_applied_isosurface = !self
            .scene
            .isosurfaces_for(Some(self.current_3d_section()))
            .is_empty();
        if !self
            .scene
            .view3d
            .slice_enabled
            .into_iter()
            .any(|enabled| enabled)
            && !has_applied_isosurface
        {
            ui.colored_label(
                Color32::from_rgb(241, 126, 126),
                "Enable a plane or apply an isosurface.",
            );
        }
        if reload
            && (self
                .scene
                .view3d
                .slice_enabled
                .into_iter()
                .any(|enabled| enabled)
                || has_applied_isosurface)
            && let Some(variable) = self.selected_variable.clone()
        {
            self.slice_changed_at = None;
            self.request_selected_plot(variable);
        }

        ui.add_space(14.0);
        section_heading(ui, "Camera");
        let mut scene = self.scene3d.lock().unwrap();
        let mut camera_changed = false;
        ui.add_enabled_ui(bounds.is_some(), |ui| {
            ui.horizontal_wrapped(|ui| {
                if ui.button("Isometric").clicked() {
                    scene.camera.preset_isometric();
                    camera_changed = true;
                }
                if ui.button("View X").clicked() {
                    scene.camera.preset_x();
                    camera_changed = true;
                }
                if ui.button("View Y").clicked() {
                    scene.camera.preset_y();
                    camera_changed = true;
                }
                if ui.button("View Z").clicked() {
                    scene.camera.preset_z();
                    camera_changed = true;
                }
                if ui.button("Reset and fit").clicked() {
                    scene.camera.preset_isometric();
                    scene.fit();
                    camera_changed = true;
                }
            });
            if let Some(bounds) = bounds {
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    if ui.button("Zoom out").clicked() {
                        scene.camera.zoom_by_factor(1.25, bounds);
                        camera_changed = true;
                    }
                    if ui.button("Zoom in").clicked() {
                        scene.camera.zoom_by_factor(0.8, bounds);
                        camera_changed = true;
                    }
                    if ui.button("Fit all").clicked() {
                        scene.fit();
                        camera_changed = true;
                    }
                });
                ui.label(RichText::new("Move view target").small().color(MUTED));
                ui.horizontal_wrapped(|ui| {
                    for (label, x, y) in [
                        ("Left", -40.0, 0.0),
                        ("Right", 40.0, 0.0),
                        ("Up", 0.0, -40.0),
                        ("Down", 0.0, 40.0),
                    ] {
                        if ui.small_button(label).clicked() {
                            scene.camera.pan(x, y);
                            camera_changed = true;
                        }
                    }
                });
                ui.label(
                    RichText::new("Exact target coordinates")
                        .small()
                        .color(MUTED),
                );
                ui.horizontal(|ui| {
                    for (axis, coordinate) in
                        ["x", "y", "z"].into_iter().zip(&mut scene.camera.target)
                    {
                        if ui
                            .add(
                                egui::DragValue::new(coordinate)
                                    .speed(0.1)
                                    .prefix(format!("{axis} ")),
                            )
                            .changed()
                        {
                            camera_changed = true;
                        }
                    }
                });
            }
            ui.horizontal(|ui| {
                ui.label("Projection");
                camera_changed |= ui
                    .selectable_value(
                        &mut scene.camera.projection,
                        Projection3d::Perspective,
                        "Perspective",
                    )
                    .changed();
                camera_changed |= ui
                    .selectable_value(
                        &mut scene.camera.projection,
                        Projection3d::Orthographic,
                        "Orthographic",
                    )
                    .changed();
            });
        });
        ui.small(
            RichText::new(
                "Left drag rotates. Shift+left, right, or middle drag pans. Wheel or pinch zooms.",
            )
            .color(MUTED),
        );
        if camera_changed && bounds.is_some() {
            self.scene.view3d.camera = Some(scene.camera);
        }

        ui.add_space(14.0);
        section_heading(ui, "Scene");
        if ui
            .add(
                egui::Slider::new(&mut self.scene.view3d.surface_opacity, 0.05..=1.0)
                    .text("Surface opacity"),
            )
            .changed()
        {
            scene.display.opacity = self.scene.view3d.surface_opacity;
        }
        if ui
            .checkbox(&mut self.scene.view3d.show_axes, "Axes")
            .changed()
        {
            scene.display.show_axes = self.scene.view3d.show_axes;
        }
        if ui
            .checkbox(&mut self.scene.view3d.show_box, "Domain box")
            .changed()
        {
            scene.display.show_box = self.scene.view3d.show_box;
        }
        let mut retrace_fieldlines = false;
        if ui
            .checkbox(
                &mut self.scene.view3d.show_reference_sphere,
                "Planet / inner boundary",
            )
            .changed()
        {
            scene.display.show_reference_sphere = self.scene.view3d.show_reference_sphere;
        }
        ui.horizontal(|ui| {
            ui.label("Planet radius");
            if ui
                .add(
                    egui::DragValue::new(&mut self.scene.view3d.reference_sphere_radius)
                        .range(0.1..=20.0)
                        .speed(0.05)
                        .suffix(" Re"),
                )
                .changed()
            {
                scene.display.reference_sphere_radius = self.scene.view3d.reference_sphere_radius;
                retrace_fieldlines = true;
            }
        });
        ui.small(
            RichText::new("Centered at (0, 0, 0); the default radius is 2.5 Re.").color(MUTED),
        );
        drop(scene);
        if retrace_fieldlines {
            self.request_fieldlines3d_for_display();
        }
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
        let effective_limits = match self.view_mode {
            ViewMode::TwoD => self.plot.lock().unwrap().display.limits,
            ViewMode::ThreeD => self.scene3d.lock().unwrap().display.limits,
        };

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

        if self.view_mode == ViewMode::TwoD {
            ui.add_space(14.0);
            section_heading(ui, "Planet overlays");
            let before_view = self.scene.view2d.clone();
            let mut view = before_view.clone();
            ui.checkbox(
                &mut view.show_inner_boundary,
                "Show gray inner-boundary disk",
            );
            if view.show_inner_boundary {
                ui.add(
                    egui::DragValue::new(&mut view.inner_boundary_radius)
                        .range(0.1..=20.0)
                        .speed(0.05)
                        .prefix("Boundary radius ")
                        .suffix(" Re"),
                );
            }
            ui.checkbox(&mut view.show_earth, "Show day/night Earth disk");
            if view.show_earth {
                ui.add(
                    egui::DragValue::new(&mut view.earth_radius)
                        .range(0.1..=10.0)
                        .speed(0.05)
                        .prefix("Earth radius ")
                        .suffix(" Re"),
                );
                ui.horizontal(|ui| {
                    ui.label("White dayside faces");
                    ui.selectable_value(
                        &mut view.dayside_direction,
                        DaysideDirection2d::PositiveX,
                        "+X",
                    );
                    ui.selectable_value(
                        &mut view.dayside_direction,
                        DaysideDirection2d::NegativeX,
                        "−X",
                    );
                });
            }
            ui.small(
                RichText::new(
                    "Both are centered at (0, 0). +X is the standard sunward convention; use −X only for a reversed display convention.",
                )
                .color(MUTED),
            );
            if view != before_view {
                self.editor.checkpoint(&self.scene);
                self.scene.view2d = view;
            }
        }

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

    fn streamline_inspector(&mut self, ui: &mut egui::Ui) {
        let data = self.plot.lock().unwrap().data.clone();
        let Some(data) = data else {
            section_heading(ui, "Field lines");
            ui.label(RichText::new("Load a plot to configure streamlines.").color(MUTED));
            return;
        };
        let section = data.header.section.clone();
        let before = self.scene.streamlines_for(section.as_deref());
        let mut edited = before.clone();
        let variables: Vec<_> = self
            .displayed_info
            .as_ref()
            .or(self.info.as_ref())
            .map(|info| {
                info.variables
                    .iter()
                    .filter(|variable| {
                        !is_coordinate(&variable.source) && !is_coordinate(&variable.canonical)
                    })
                    .map(|variable| {
                        (
                            variable.canonical.clone(),
                            if variable.canonical == variable.source {
                                variable.source.clone()
                            } else {
                                format!("{} · {}", variable.canonical, variable.source)
                            },
                        )
                    })
                    .collect()
            })
            .unwrap_or_default();

        section_heading(ui, "Optional field-line overlay");
        ui.label(
            RichText::new(format!(
                "Section · {}",
                section.as_deref().unwrap_or("unclassified")
            ))
            .small()
            .color(MUTED),
        );
        let magnetic = self.magnetic_components();
        if !edited.enabled {
            ui.add_space(8.0);
            egui::Frame::group(ui.style())
                .fill(Color32::from_rgb(18, 29, 41))
                .stroke(Stroke::new(1.0, Color32::from_rgb(42, 65, 84)))
                .inner_margin(egui::Margin::same(12))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.label(RichText::new("No field lines are being drawn").strong());
                    ui.label(
                        RichText::new(
                            "Streamtraces are optional. Add an overlay only when you need one.",
                        )
                        .small()
                        .color(MUTED),
                    );
                    ui.add_space(8.0);
                    let add_magnetic = ui
                        .add_enabled(
                            magnetic.is_some(),
                            egui::Button::new(
                                RichText::new("Add magnetic field lines").strong(),
                            )
                            .fill(Color32::from_rgb(34, 91, 137))
                            .min_size(egui::vec2(ui.available_width(), 36.0)),
                        )
                        .on_hover_text(
                            "Use the magnetic-field components aligned with the plot axes",
                        );
                    if add_magnetic.clicked()
                        && let Some((horizontal, vertical)) = magnetic.clone()
                    {
                        edited.enabled = true;
                        edited.horizontal_component = Some(horizontal);
                        edited.vertical_component = Some(vertical);
                    }
                    if magnetic.is_none() {
                        ui.label(
                            RichText::new(
                                "No magnetic-field pair matches the current plot axes. Use a custom vector field below.",
                            )
                            .small()
                            .color(MUTED),
                        );
                    }
                });

            ui.add_space(14.0);
            section_heading(ui, "Or add a custom vector field");
            ui.label(
                RichText::new("Choose the two components that point along the plot axes.")
                    .small()
                    .color(MUTED),
            );
            variable_combo(
                ui,
                "Horizontal component",
                &mut edited.horizontal_component,
                &variables,
            );
            variable_combo(
                ui,
                "Vertical component",
                &mut edited.vertical_component,
                &variables,
            );
            let custom_is_valid = edited.horizontal_component.is_some()
                && edited.vertical_component.is_some()
                && edited.horizontal_component != edited.vertical_component;
            if edited.horizontal_component.is_some()
                && edited.horizontal_component == edited.vertical_component
            {
                ui.colored_label(
                    Color32::from_rgb(241, 126, 126),
                    "Choose two different vector components.",
                );
            }
            if ui
                .add_enabled(
                    custom_is_valid,
                    egui::Button::new("Add custom field lines")
                        .min_size(egui::vec2(ui.available_width(), 32.0)),
                )
                .clicked()
            {
                edited.enabled = true;
            }
        } else {
            ui.add_space(8.0);
            egui::Frame::group(ui.style())
                .fill(Color32::from_rgb(17, 38, 47))
                .stroke(Stroke::new(1.0, Color32::from_rgb(47, 101, 112)))
                .inner_margin(egui::Margin::same(10))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new("Field-line overlay is on")
                                .strong()
                                .color(ACCENT),
                        );
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui
                                .button("Hide")
                                .on_hover_text("Remove field lines from the plot")
                                .clicked()
                            {
                                edited.enabled = false;
                            }
                        });
                    });
                });

            ui.add_space(10.0);
            section_heading(ui, "Vector components");
            if ui
                .add_enabled(
                    magnetic.is_some(),
                    egui::Button::new("Use magnetic-field components"),
                )
                .on_hover_text("Choose the magnetic components aligned with the plot axes")
                .clicked()
                && let Some((horizontal, vertical)) = magnetic
            {
                edited.horizontal_component = Some(horizontal);
                edited.vertical_component = Some(vertical);
            }
            variable_combo(
                ui,
                "Horizontal component",
                &mut edited.horizontal_component,
                &variables,
            );
            variable_combo(
                ui,
                "Vertical component",
                &mut edited.vertical_component,
                &variables,
            );
            if edited.horizontal_component.is_some()
                && edited.horizontal_component == edited.vertical_component
            {
                ui.colored_label(
                    Color32::from_rgb(241, 126, 126),
                    "Choose two different vector components.",
                );
            }

            ui.add_space(12.0);
            latitude_seed_controls(ui, &mut edited.latitude_seeds, true);

            ui.add_space(12.0);
            section_heading(ui, "Additional seed points");
            ui.label(
                RichText::new("Add individual points or a regular grid elsewhere in the plot.")
                    .small()
                    .color(MUTED),
            );
            ui.horizontal(|ui| {
                let can_place = edited.horizontal_component.is_some()
                    && edited.vertical_component.is_some()
                    && edited.horizontal_component != edited.vertical_component;
                if ui
                    .add_enabled(
                        can_place,
                        egui::Button::selectable(self.placing_streamline_seed, "Place on plot"),
                    )
                    .on_hover_text("Click the canvas to add streamline seeds")
                    .clicked()
                {
                    self.placing_streamline_seed = !self.placing_streamline_seed;
                    if self.placing_streamline_seed {
                        self.editor.cancel_drawing();
                        self.editor.tool = DrawingTool::Select;
                    }
                }
                if ui
                    .add_enabled(!edited.seeds.is_empty(), egui::Button::new("Clear"))
                    .clicked()
                {
                    edited.seeds.clear();
                }
                ui.label(
                    RichText::new(format!("{} seeds", edited.seeds.len()))
                        .small()
                        .color(MUTED),
                );
            });
            ui.horizontal(|ui| {
                ui.label("Grid");
                ui.add(
                    egui::DragValue::new(&mut edited.seed_columns)
                        .range(1..=16)
                        .prefix("columns "),
                );
                ui.add(
                    egui::DragValue::new(&mut edited.seed_rows)
                        .range(1..=16)
                        .prefix("rows "),
                );
            });
            let mut use_region = edited.seed_region.is_some();
            if ui
                .checkbox(&mut use_region, "Limit grid to a custom region")
                .changed()
            {
                edited.seed_region = use_region.then_some(data.header.bounds);
            }
            if let Some(region) = &mut edited.seed_region {
                ui.horizontal(|ui| {
                    ui.label(&data.header.x_label);
                    ui.add(
                        egui::DragValue::new(&mut region[0])
                            .speed(0.1)
                            .prefix("min "),
                    );
                    ui.add(
                        egui::DragValue::new(&mut region[1])
                            .speed(0.1)
                            .prefix("max "),
                    );
                });
                ui.horizontal(|ui| {
                    ui.label(&data.header.y_label);
                    ui.add(
                        egui::DragValue::new(&mut region[2])
                            .speed(0.1)
                            .prefix("min "),
                    );
                    ui.add(
                        egui::DragValue::new(&mut region[3])
                            .speed(0.1)
                            .prefix("max "),
                    );
                });
            }
            if ui.button("Replace custom seeds with this grid").clicked() {
                edited.seeds = seed_grid(
                    edited.seed_region.unwrap_or(data.header.bounds),
                    edited.seed_columns,
                    edited.seed_rows,
                );
            }
            if edited.seeds.is_empty() && !edited.latitude_seeds.enabled {
                ui.label(
                    RichText::new("Enable planetary footpoints, place seeds, or generate a grid.")
                        .color(MUTED),
                );
            } else {
                let mut remove = None;
                egui::ScrollArea::vertical()
                    .id_salt("streamline_seed_list")
                    .max_height(170.0)
                    .show(ui, |ui| {
                        for (index, seed) in edited.seeds.iter_mut().enumerate() {
                            ui.horizontal(|ui| {
                                ui.small(format!("{}", index + 1));
                                ui.add(egui::DragValue::new(&mut seed.x).speed(0.01).prefix("x "));
                                ui.add(egui::DragValue::new(&mut seed.y).speed(0.01).prefix("y "));
                                if ui.small_button("×").on_hover_text("Remove seed").clicked() {
                                    remove = Some(index);
                                }
                            });
                        }
                    });
                if let Some(index) = remove {
                    edited.seeds.remove(index);
                }
            }

            ui.add_space(12.0);
            section_heading(ui, "Integration");
            egui::ComboBox::from_label("Direction")
                .selected_text(match edited.direction {
                    StreamlineDirection::Forward => "Forward",
                    StreamlineDirection::Backward => "Backward",
                    StreamlineDirection::Both => "Both directions",
                })
                .show_ui(ui, |ui| {
                    ui.selectable_value(
                        &mut edited.direction,
                        StreamlineDirection::Both,
                        "Both directions",
                    );
                    ui.selectable_value(
                        &mut edited.direction,
                        StreamlineDirection::Forward,
                        "Forward",
                    );
                    ui.selectable_value(
                        &mut edited.direction,
                        StreamlineDirection::Backward,
                        "Backward",
                    );
                });
            let mut step_percent = edited.step_fraction * 100.0;
            if ui
                .add(
                    egui::DragValue::new(&mut step_percent)
                        .range(0.001..=5.0)
                        .speed(0.01)
                        .suffix("% domain / step"),
                )
                .changed()
            {
                edited.step_fraction = step_percent / 100.0;
            }
            ui.add(
                egui::DragValue::new(&mut edited.max_steps)
                    .range(10..=5_000)
                    .speed(50)
                    .suffix(" max steps"),
            );

            ui.add_space(12.0);
            section_heading(ui, "Line style");
            let mut color = edited.color.to_egui();
            ui.horizontal(|ui| {
                ui.label("Color");
                if ui.color_edit_button_srgba(&mut color).changed() {
                    edited.color = RgbaColor::from_egui(color);
                }
            });
            ui.add(
                egui::Slider::new(&mut edited.width, 0.25..=8.0)
                    .text("Width")
                    .suffix(" px"),
            );
            ui.checkbox(&mut edited.arrows, "Show direction arrows");
            if edited.arrows {
                ui.add(
                    egui::Slider::new(&mut edited.arrow_size, 3.0..=20.0)
                        .text("Arrow size")
                        .suffix(" px"),
                );
            }

            if self.streamline_loading {
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    ui.spinner();
                    let message = if self.streamline_overlay.is_some() {
                        "Updating field lines… previous frame remains visible"
                    } else {
                        "Computing field lines…"
                    };
                    ui.label(RichText::new(message).color(MUTED));
                });
            } else if let Some(error) = &self.streamline_error {
                ui.add_space(8.0);
                ui.colored_label(Color32::from_rgb(241, 126, 126), error);
            } else if let Some(overlay) = &self.streamline_overlay {
                ui.add_space(8.0);
                ui.label(
                    RichText::new(format!(
                        "{} field lines · {} / {}",
                        overlay.lines.len(),
                        overlay.horizontal_component,
                        overlay.vertical_component
                    ))
                    .small()
                    .color(MUTED),
                );
            }
        }

        if edited != before {
            let reload = edited.enabled != before.enabled
                || edited.horizontal_component != before.horizontal_component
                || edited.vertical_component != before.vertical_component;
            let reintegrate = edited.seeds != before.seeds
                || edited.latitude_seeds != before.latitude_seeds
                || edited.step_fraction != before.step_fraction
                || edited.max_steps != before.max_steps
                || edited.direction != before.direction;
            self.editor.checkpoint(&self.scene);
            self.scene
                .set_streamlines_for(section.as_deref(), edited.clone());
            if !edited.enabled
                || edited.horizontal_component.is_none()
                || edited.vertical_component.is_none()
                || edited.horizontal_component == edited.vertical_component
            {
                self.placing_streamline_seed = false;
            }
            if reload {
                self.request_streamlines_for_display();
            } else if reintegrate {
                self.recompute_streamlines();
            } else {
                self.update_streamline_style();
            }
        }
    }

    fn fieldline3d_inspector(&mut self, ui: &mut egui::Ui) {
        let data = self.scene3d.lock().unwrap().data.clone();
        let Some(data) = data else {
            section_heading(ui, "3D field lines");
            ui.label(RichText::new("Load a 3D file to configure field lines.").color(MUTED));
            return;
        };
        let section = data.header.section.clone();
        let before = self.scene.fieldlines3d_for(Some(&section));
        let mut edited = before.clone();
        let variables: Vec<_> = self
            .displayed_info
            .as_ref()
            .or(self.info.as_ref())
            .map(|info| {
                info.variables
                    .iter()
                    .filter(|variable| {
                        !is_coordinate(&variable.source) && !is_coordinate(&variable.canonical)
                    })
                    .map(|variable| {
                        (
                            variable.canonical.clone(),
                            if variable.canonical == variable.source {
                                variable.source.clone()
                            } else {
                                format!("{} · {}", variable.canonical, variable.source)
                            },
                        )
                    })
                    .collect()
            })
            .unwrap_or_default();
        let magnetic = self.magnetic_components3d();

        section_heading(ui, "Optional 3D field lines");
        ui.label(
            RichText::new("Field lines are never added automatically. Choose a vector field when you want this overlay.")
                .small()
                .color(MUTED),
        );
        if !edited.enabled {
            ui.add_space(8.0);
            egui::Frame::group(ui.style())
                .fill(Color32::from_rgb(18, 29, 41))
                .stroke(Stroke::new(1.0, Color32::from_rgb(42, 65, 84)))
                .inner_margin(egui::Margin::same(12))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.label(RichText::new("No 3D field lines are being drawn").strong());
                    ui.label(
                        RichText::new("The default seeds are invisible planetary footpoints distributed by latitude and longitude.")
                            .small()
                            .color(MUTED),
                    );
                    ui.add_space(8.0);
                    if ui
                        .add_enabled(
                            magnetic.is_some(),
                            egui::Button::new(RichText::new("Add magnetic field lines").strong())
                                .fill(Color32::from_rgb(34, 91, 137))
                                .min_size(egui::vec2(ui.available_width(), 36.0)),
                        )
                        .on_hover_text("Trace magnetic_field.x, .y, and .z")
                        .clicked()
                        && let Some(components) = magnetic.clone()
                    {
                        edited.components = components.map(Some);
                        edited.enabled = true;
                    }
                    if magnetic.is_none() {
                        ui.label(
                            RichText::new("Magnetic-field components were not detected; choose a custom vector field below.")
                                .small()
                                .color(MUTED),
                        );
                    }
                });
            ui.add_space(14.0);
            section_heading(ui, "Or add a custom vector field");
            for (index, label) in ["X component", "Y component", "Z component"]
                .into_iter()
                .enumerate()
            {
                variable_combo(ui, label, &mut edited.components[index], &variables);
            }
            let custom_is_valid = components_are_distinct(&edited.components);
            if ui
                .add_enabled(
                    custom_is_valid,
                    egui::Button::new("Add custom 3D field lines")
                        .min_size(egui::vec2(ui.available_width(), 32.0)),
                )
                .clicked()
            {
                edited.enabled = true;
            }
        } else {
            ui.add_space(8.0);
            egui::Frame::group(ui.style())
                .fill(Color32::from_rgb(17, 38, 47))
                .stroke(Stroke::new(1.0, Color32::from_rgb(47, 101, 112)))
                .inner_margin(egui::Margin::same(10))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new("3D field-line overlay is on")
                                .strong()
                                .color(ACCENT),
                        );
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.button("Hide").clicked() {
                                edited.enabled = false;
                            }
                        });
                    });
                });

            ui.add_space(12.0);
            section_heading(ui, "Vector components");
            if ui
                .add_enabled(
                    magnetic.is_some(),
                    egui::Button::new("Use magnetic field Bx / By / Bz"),
                )
                .clicked()
                && let Some(components) = magnetic
            {
                edited.components = components.map(Some);
            }
            for (index, label) in ["X component", "Y component", "Z component"]
                .into_iter()
                .enumerate()
            {
                variable_combo(ui, label, &mut edited.components[index], &variables);
            }
            if !components_are_distinct(&edited.components) {
                ui.colored_label(
                    Color32::from_rgb(241, 126, 126),
                    "Choose three different vector components.",
                );
            }

            ui.add_space(14.0);
            latitude_seed_controls(ui, &mut edited.latitude_seeds, true);

            ui.add_space(14.0);
            section_heading(ui, "Additional seed region");
            ui.label(
                RichText::new("Optionally trace a regular 3D grid in another part of the domain.")
                    .small()
                    .color(MUTED),
            );
            let mut use_region = edited.seed_region.is_some();
            if ui
                .checkbox(&mut use_region, "Include a custom region")
                .changed()
            {
                edited.seed_region = use_region.then_some(data.header.bounds);
            }
            if let Some(region) = &mut edited.seed_region {
                for (label, low, high) in [("X", 0, 1), ("Y", 2, 3), ("Z", 4, 5)] {
                    ui.horizontal(|ui| {
                        ui.label(label);
                        ui.add(
                            egui::DragValue::new(&mut region[low])
                                .speed(0.1)
                                .prefix("min "),
                        );
                        ui.add(
                            egui::DragValue::new(&mut region[high])
                                .speed(0.1)
                                .prefix("max "),
                        );
                    });
                }
                ui.horizontal(|ui| {
                    for (axis, count) in ["X", "Y", "Z"].into_iter().zip(&mut edited.region_counts)
                    {
                        ui.add(
                            egui::DragValue::new(count)
                                .range(1..=12)
                                .prefix(format!("{axis} ")),
                        );
                    }
                });
                let count = edited
                    .region_counts
                    .iter()
                    .map(|value| usize::from(*value))
                    .product::<usize>();
                ui.small(
                    RichText::new(format!("{count} generated region seeds (not displayed)"))
                        .color(MUTED),
                );
            }

            ui.add_space(10.0);
            section_heading(ui, "Individual seed points");
            ui.horizontal(|ui| {
                if ui.button("Add point").clicked() {
                    edited.custom_seeds.push(DataPoint3::new(3.0, 0.0, 0.0));
                }
                if ui
                    .add_enabled(!edited.custom_seeds.is_empty(), egui::Button::new("Clear"))
                    .clicked()
                {
                    edited.custom_seeds.clear();
                }
                ui.small(
                    RichText::new(format!("{} custom", edited.custom_seeds.len())).color(MUTED),
                );
            });
            let mut remove = None;
            for (index, seed) in edited.custom_seeds.iter_mut().enumerate() {
                ui.horizontal(|ui| {
                    ui.small(format!("{}", index + 1));
                    ui.add(egui::DragValue::new(&mut seed.x).speed(0.05).prefix("x "));
                    ui.add(egui::DragValue::new(&mut seed.y).speed(0.05).prefix("y "));
                    ui.add(egui::DragValue::new(&mut seed.z).speed(0.05).prefix("z "));
                    if ui.small_button("×").clicked() {
                        remove = Some(index);
                    }
                });
            }
            if let Some(index) = remove {
                edited.custom_seeds.remove(index);
            }

            ui.add_space(14.0);
            section_heading(ui, "Integration");
            ui.add(
                egui::DragValue::new(&mut edited.step_size)
                    .range(0.005..=5.0)
                    .speed(0.01)
                    .suffix(" Re / step"),
            );
            ui.add(
                egui::DragValue::new(&mut edited.max_steps)
                    .range(10..=20_000)
                    .speed(100)
                    .suffix(" max steps"),
            );
            ui.add(
                egui::DragValue::new(&mut edited.max_length)
                    .range(1.0..=5_000.0)
                    .speed(1.0)
                    .suffix(" Re max length"),
            );

            ui.add_space(14.0);
            section_heading(ui, "Line style");
            let mut color = edited.color.to_egui();
            ui.horizontal(|ui| {
                ui.label("Color");
                if ui.color_edit_button_srgba(&mut color).changed() {
                    edited.color = RgbaColor::from_egui(color);
                }
            });
            ui.add(
                egui::Slider::new(&mut edited.width, 0.25..=8.0)
                    .text("Width")
                    .suffix(" px"),
            );
            ui.checkbox(&mut edited.arrows, "Show direction arrows");
            if edited.arrows {
                ui.add(
                    egui::Slider::new(&mut edited.arrow_size, 3.0..=20.0)
                        .text("Arrow size")
                        .suffix(" px"),
                );
            }

            if self.fieldlines3d_loading {
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(
                        RichText::new(if self.fieldlines3d.is_some() {
                            "Tracing next frame… previous lines remain visible"
                        } else {
                            "Tracing 3D field lines…"
                        })
                        .color(MUTED),
                    );
                });
            } else if let Some(error) = &self.fieldlines3d_error {
                ui.add_space(8.0);
                ui.colored_label(Color32::from_rgb(241, 126, 126), error);
            } else if let Some(lines) = &self.fieldlines3d {
                ui.add_space(8.0);
                ui.small(
                    RichText::new(format!(
                        "{} field lines · {} points",
                        lines.header.line_count, lines.header.point_count
                    ))
                    .color(MUTED),
                );
            }
        }

        if edited != before {
            let retrace = edited.enabled != before.enabled
                || edited.components != before.components
                || edited.latitude_seeds != before.latitude_seeds
                || edited.custom_seeds != before.custom_seeds
                || edited.seed_region != before.seed_region
                || edited.region_counts != before.region_counts
                || edited.step_size != before.step_size
                || edited.max_steps != before.max_steps
                || edited.max_length != before.max_length;
            self.editor.checkpoint(&self.scene);
            self.scene
                .set_fieldlines3d_for(Some(&section), edited.clone());
            if !edited.enabled {
                self.loader.cancel_auxiliary();
                self.active_fieldlines3d_request = None;
                self.fieldlines3d_loading = false;
                self.fieldlines3d = None;
                self.fieldlines3d_error = None;
            } else if retrace {
                self.request_fieldlines3d_for_display();
            }
        }
    }

    fn magnetic_components3d(&self) -> Option<[String; 3]> {
        let info = self.displayed_info.as_ref().or(self.info.as_ref())?;
        let component = |axis: char| {
            let canonical = format!("magnetic_field.{axis}");
            info.variables
                .iter()
                .find(|variable| variable.canonical.eq_ignore_ascii_case(&canonical))
                .map(|variable| variable.canonical.clone())
                .or_else(|| {
                    info.variables
                        .iter()
                        .find(|variable| {
                            let source = variable.source.to_ascii_lowercase().replace(' ', "");
                            source.starts_with(&format!("b_{axis}"))
                                || source.starts_with(&format!("b{axis}["))
                        })
                        .map(|variable| variable.canonical.clone())
                })
        };
        Some([component('x')?, component('y')?, component('z')?])
    }

    fn magnetic_components(&self) -> Option<(String, String)> {
        let data = self.plot.lock().unwrap().data.clone()?;
        let horizontal_axis = coordinate_axis(&data.header.x_label)?;
        let vertical_axis = coordinate_axis(&data.header.y_label)?;
        let info = self.displayed_info.as_ref().or(self.info.as_ref())?;
        let component = |axis: char| {
            let canonical = format!("magnetic_field.{axis}");
            info.variables
                .iter()
                .find(|variable| variable.canonical.eq_ignore_ascii_case(&canonical))
                .map(|variable| variable.canonical.clone())
                .or_else(|| {
                    info.variables
                        .iter()
                        .find(|variable| {
                            let source = variable.source.to_ascii_lowercase().replace(' ', "");
                            source.starts_with(&format!("b_{axis}"))
                                || source.starts_with(&format!("b{axis}["))
                        })
                        .map(|variable| variable.canonical.clone())
                })
        };
        Some((component(horizontal_axis)?, component(vertical_axis)?))
    }

    fn metadata_inspector(&mut self, ui: &mut egui::Ui) {
        section_heading(ui, "Dataset");
        if let Some(info) = &self.displayed_info {
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
        if self.view_mode == ViewMode::ThreeD {
            let shared = self.scene3d.lock().unwrap();
            if let Some(data) = &shared.data {
                ui.add_space(14.0);
                section_heading(ui, "Loaded 3D scene");
                metadata_row(ui, "Variable", &data.header.canonical_name);
                metadata_row(ui, "Source", &data.header.variable);
                metadata_row(ui, "Slice planes", &data.header.layers.len().to_string());
                metadata_row(ui, "Triangles", &data.header.triangle_count.to_string());
                metadata_row(ui, "Vertices", &data.header.vertex_count.to_string());
            }
            return;
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
        if self.view_mode == ViewMode::ThreeD {
            self.plot_panel_3d(root);
            return;
        }
        let can_place_streamline_seed = self
            .plot
            .lock()
            .unwrap()
            .data
            .as_ref()
            .map(|data| self.scene.streamlines_for(data.header.section.as_deref()))
            .is_some_and(|settings| {
                settings.enabled
                    && settings.horizontal_component.is_some()
                    && settings.vertical_component.is_some()
                    && settings.horizontal_component != settings.vertical_component
            });
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
                                    self.placing_streamline_seed = false;
                                    self.probe_mode = false;
                                }
                            }
                            ui.separator();
                            if toolbar_icon_button(
                                ui,
                                ToolbarIcon::StreamlineSeed,
                                self.placing_streamline_seed,
                                can_place_streamline_seed,
                                "Place field-line seeds on the plot",
                            ) {
                                self.editor.cancel_drawing();
                                self.editor.tool = DrawingTool::Select;
                                self.placing_streamline_seed = !self.placing_streamline_seed;
                                self.probe_mode = false;
                                self.inspector_tab = InspectorTab::FieldLines;
                            }
                            if toolbar_icon_button(
                                ui,
                                ToolbarIcon::Probe,
                                self.probe_mode,
                                self.probe_index.is_some(),
                                "Probe and pin interpolated values  P",
                            ) {
                                self.editor.cancel_drawing();
                                self.editor.tool = DrawingTool::Select;
                                self.placing_streamline_seed = false;
                                self.probe_mode = !self.probe_mode;
                                self.inspector_tab = InspectorTab::Data;
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
                                self.recompute_streamlines();
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
                                self.recompute_streamlines();
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
                let plot_size = egui::vec2(available.x, (available.y - 57.0).max(120.0));
                let (export_rect, _) = ui.allocate_exact_size(plot_size, Sense::hover());
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
                if let Some(data) = data {
                    let chart_outer = egui::Rect::from_min_max(
                        export_rect.min + egui::vec2(72.0, 72.0),
                        export_rect.max - egui::vec2(120.0, 58.0),
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
                    ui.painter()
                        .add(PlotCallback::paint_callback(plot_rect, self.plot.clone()));
                    if let Some(overlay) = &self.streamline_overlay {
                        paint_streamlines(ui, plot_rect, display.view_bounds, overlay);
                    }
                    paint_reference_bodies_2d(
                        ui,
                        plot_rect,
                        display.view_bounds,
                        &data.header.x_label,
                        &data.header.y_label,
                        &self.scene.view2d,
                    );

                    let (section, variable, relative_path) = self.scope_values();
                    let scope = ScopeContext {
                        section: section.as_deref(),
                        variable: variable.as_deref(),
                        relative_path: relative_path.as_deref(),
                    };
                    let (pointer_clicked, pointer_released, raw_release, latest_pointer) =
                        ui.input(|input| {
                        let raw_release = input.events.iter().rev().find_map(|event| match event {
                            egui::Event::PointerButton {
                                pos,
                                button: egui::PointerButton::Primary,
                                pressed: false,
                                ..
                            } => Some(*pos),
                            _ => None,
                        });
                        (
                            input.pointer.primary_clicked(),
                            input.pointer.primary_released(),
                            raw_release,
                            input.pointer.latest_pos(),
                        )
                    });
                    let seed_click = response.clicked_by(egui::PointerButton::Primary)
                        || response.drag_stopped_by(egui::PointerButton::Primary)
                        || pointer_clicked
                        || pointer_released
                        || raw_release.is_some();
                    let new_seed = (self.placing_streamline_seed && seed_click)
                        .then(|| {
                            response
                                .interact_pointer_pos()
                                .or(raw_release)
                                .or(latest_pointer)
                        })
                        .flatten()
                        .filter(|pointer| plot_rect.contains(*pointer))
                        .map(|pointer| {
                            streamline_seed_point(pointer, plot_rect, display.view_bounds)
                        });
                    let probe_hit = response
                        .hover_pos()
                        .and_then(|pointer| {
                            let point = streamline_seed_point(pointer, plot_rect, display.view_bounds);
                            self.probe_index
                                .as_ref()
                                .and_then(|index| index.query_2d([point.x as f32, point.y as f32]))
                        });
                    self.hover_probe = probe_hit.clone();
                    let probe_pinned = self.probe_mode
                        && response.clicked_by(egui::PointerButton::Primary)
                        && probe_hit.is_some();
                    if probe_pinned && let Some(hit) = probe_hit.clone() {
                        self.pin_probe(hit, ProbeDimension::TwoD);
                    }
                    let consumed = if self.probe_mode {
                        true
                    } else if self.placing_streamline_seed {
                        new_seed.is_some()
                    } else {
                        self.editor.interact(
                            ui,
                            &response,
                            plot_rect,
                            display.view_bounds,
                            &mut self.scene,
                            &scope,
                        )
                    };
                    if let Some(seed) = new_seed {
                        self.editor.checkpoint(&self.scene);
                        let mut settings = self.scene.streamlines_for(section.as_deref());
                        settings.seeds.push(seed);
                        self.scene
                            .set_streamlines_for(section.as_deref(), settings);
                        self.recompute_streamlines();
                    }
                    if self.editor.tool == DrawingTool::Select && !consumed {
                        let mut shared = self.plot.lock().unwrap();
                        if response.dragged() {
                            let delta = ui.input(|input| input.pointer.delta());
                            shared
                                .pan_view(-delta.x / plot_rect.width(), delta.y / plot_rect.height());
                        }
                        if response.double_clicked() && !self.placing_streamline_seed {
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
                    paint_probe_measurements_2d(
                        ui,
                        plot_rect,
                        display.view_bounds,
                        &self.scene.measurements,
                        relative_path.as_deref().unwrap_or_default(),
                        variable.as_deref().unwrap_or_default(),
                    );
                    if let (Some(pointer), Some(hit)) = (response.hover_pos(), probe_hit.as_ref()) {
                        paint_probe_readout(ui, plot_rect, pointer, hit);
                    } else if self.probe_indexing && response.hovered() {
                        paint_probe_status(ui, plot_rect, "Probe indexing…");
                    }
                    let appearance = self
                        .scene
                        .appearance_for(self.displayed_variable.as_deref());
                    paint_plot_chrome(
                        ui,
                        export_rect,
                        plot_rect,
                        &self.plot_chrome(&data.header),
                        &display,
                        &appearance,
                        PlotColors { foreground, muted },
                    );
                } else {
                    ui.painter().text(
                        export_rect.center(),
                        egui::Align2::CENTER_CENTER,
                        "Open a run and select a variable",
                        FontId::proportional(18.0),
                        muted,
                    );
                }
                ui.add_space(7.0);
                self.timeline_bar(ui);
            });
    }

    fn plot_panel_3d(&mut self, root: &mut egui::Ui) {
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
                            ui.label(RichText::new("3D VIEW").size(9.5).strong().color(MUTED));
                            if toolbar_icon_button(
                                ui,
                                ToolbarIcon::Probe,
                                self.probe_mode,
                                self.probe_index.is_some(),
                                "Probe and pin the nearest visible surface  P",
                            ) {
                                self.probe_mode = !self.probe_mode;
                                self.inspector_tab = InspectorTab::Data;
                            }
                            ui.separator();
                            let mut scene = self.scene3d.lock().unwrap();
                            let bounds = scene
                                .data
                                .as_ref()
                                .map(|data| data.header.active_bounds());
                            let enabled = bounds.is_some();
                            let mut camera_changed = false;
                            for (label, tooltip, preset) in [
                                ("Iso", "Isometric view", 0),
                                ("X", "Look along X", 1),
                                ("Y", "Look along Y", 2),
                                ("Z", "Look along Z", 3),
                            ] {
                                if ui
                                    .add_enabled(enabled, egui::Button::new(label))
                                    .on_hover_text(tooltip)
                                    .clicked()
                                {
                                    match preset {
                                        0 => scene.camera.preset_isometric(),
                                        1 => scene.camera.preset_x(),
                                        2 => scene.camera.preset_y(),
                                        _ => scene.camera.preset_z(),
                                    }
                                    camera_changed = true;
                                }
                            }
                            if ui
                                .add_enabled(enabled, egui::Button::new("Fit all"))
                                .on_hover_text("Fit the complete domain  F")
                                .clicked()
                            {
                                scene.fit();
                                camera_changed = true;
                            }
                            if ui
                                .add_enabled(enabled, egui::Button::new("Reset view"))
                                .on_hover_text("Restore the isometric view and fit the complete domain")
                                .clicked()
                            {
                                scene.camera.preset_isometric();
                                scene.fit();
                                camera_changed = true;
                            }
                            ui.separator();
                            if ui
                                .add_enabled(enabled, egui::Button::new("Zoom -"))
                                .on_hover_text("Zoom out")
                                .clicked()
                                && let Some(bounds) = bounds
                            {
                                scene.camera.zoom_by_factor(1.25, bounds);
                                camera_changed = true;
                            }
                            if ui
                                .add_enabled(enabled, egui::Button::new("Zoom +"))
                                .on_hover_text("Zoom in")
                                .clicked()
                                && let Some(bounds) = bounds
                            {
                                scene.camera.zoom_by_factor(0.8, bounds);
                                camera_changed = true;
                            }
                            ui.separator();
                            let perspective = scene.camera.projection == Projection3d::Perspective;
                            if ui
                                .add_enabled(
                                    enabled,
                                    egui::Button::selectable(perspective, "Perspective"),
                                )
                                .on_hover_text("Toggle perspective / orthographic projection")
                                .clicked()
                            {
                                scene.camera.projection = if perspective {
                                    Projection3d::Orthographic
                                } else {
                                    Projection3d::Perspective
                                };
                                camera_changed = true;
                            }
                            if camera_changed {
                                self.scene.view3d.camera = Some(scene.camera);
                            }
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    ui.label(
                                        RichText::new(
                                            "Drag: rotate  ·  Shift/right drag: pan  ·  Wheel/pinch: zoom",
                                        )
                                        .small()
                                        .color(MUTED),
                                    );
                                },
                            );
                        });
                    });
                ui.add_space(7.0);
                let available = ui.available_size();
                let canvas_size = egui::vec2(available.x, (available.y - 57.0).max(120.0));
                let (canvas_rect, response) =
                    ui.allocate_exact_size(canvas_size, Sense::click_and_drag());
                self.last_export_rect = Some(canvas_rect);
                ui.painter().rect_filled(canvas_rect, 6.0, DEEP_BG);
                ui.painter().rect_stroke(
                    canvas_rect,
                    6.0,
                    Stroke::new(1.0, Color32::from_rgb(37, 50, 65)),
                    StrokeKind::Inside,
                );

                let data = self.scene3d.lock().unwrap().data.clone();
                if let Some(data) = data {
                    let scene_rect = egui::Rect::from_min_max(
                        canvas_rect.min + egui::vec2(18.0, 66.0),
                        canvas_rect.max - egui::vec2(112.0, 24.0),
                    );
                    ui.painter().add(Scene3dCallback::paint_callback(
                        scene_rect,
                        self.scene3d.clone(),
                    ));
                    let fieldline_settings =
                        self.scene.fieldlines3d_for(Some(&data.header.section));
                    if let Some(lines) = &self.fieldlines3d {
                        paint_fieldlines3d(
                            ui,
                            scene_rect,
                            &self.scene3d,
                            lines,
                            &fieldline_settings,
                        );
                    }
                    paint_scene_overlays(ui, scene_rect, &self.scene3d);

                    let scene_response = ui.interact(
                        scene_rect,
                        ui.id().with("scene3d_interaction"),
                        Sense::click_and_drag(),
                    );
                    let mut changed = false;
                    {
                        let mut scene = self.scene3d.lock().unwrap();
                        let delta = ui.input(|input| input.pointer.delta());
                        if scene_response.dragged_by(egui::PointerButton::Primary) {
                            let pan = ui.input(|input| input.modifiers.shift);
                            if pan {
                                scene.camera.pan(delta.x, delta.y);
                            } else {
                                scene.camera.orbit(delta.x, delta.y);
                            }
                            changed = true;
                        }
                        if scene_response.dragged_by(egui::PointerButton::Secondary)
                            || scene_response.dragged_by(egui::PointerButton::Middle)
                        {
                            scene.camera.pan(delta.x, delta.y);
                            changed = true;
                        }
                        if scene_response.double_clicked() {
                            scene.fit();
                            changed = true;
                        }
                        if scene_response.hovered() {
                            let (scroll, pinch) = ui.input(|input| {
                                (input.smooth_scroll_delta.y, input.zoom_delta())
                            });
                            if scroll != 0.0 {
                                scene.camera.zoom(scroll, data.header.active_bounds());
                                changed = true;
                            }
                            if (pinch - 1.0).abs() > 1.0e-3 {
                                scene
                                    .camera
                                    .zoom_by_factor(1.0 / pinch, data.header.active_bounds());
                                changed = true;
                            }
                        }
                        if changed {
                            self.scene.view3d.camera = Some(scene.camera);
                        }
                    }

                    let probe_hit = scene_response.hover_pos().and_then(|pointer| {
                        let camera = self.scene3d.lock().unwrap().camera;
                        let visible_layers = self
                            .scene
                            .isosurfaces_for(Some(&data.header.section))
                            .iter()
                            .filter(|layer| layer.visible)
                            .map(|layer| layer.id)
                            .collect::<Vec<_>>();
                        let ray = camera_ray(camera, scene_rect, pointer);
                        self.probe_index.as_ref().and_then(|index| {
                            index.query_3d(ray, |layer_id| {
                                layer_id.is_none_or(|id| visible_layers.contains(&id))
                            })
                        })
                    });
                    self.hover_probe = probe_hit.clone();
                    if self.probe_mode
                        && scene_response.clicked_by(egui::PointerButton::Primary)
                        && let Some(hit) = probe_hit.clone()
                    {
                        self.pin_probe(hit, ProbeDimension::ThreeD);
                    }
                    let (_, scope_variable, relative_path) = self.scope_values();
                    paint_probe_measurements_3d(
                        ui,
                        scene_rect,
                        &self.scene3d,
                        &self.scene.measurements,
                        relative_path.as_deref().unwrap_or_default(),
                        scope_variable.as_deref().unwrap_or_default(),
                    );
                    if let (Some(pointer), Some(hit)) =
                        (scene_response.hover_pos(), probe_hit.as_ref())
                    {
                        paint_probe_readout(ui, scene_rect, pointer, hit);
                    } else if self.probe_indexing && scene_response.hovered() {
                        paint_probe_status(ui, scene_rect, "Probe indexing…");
                    }

                    let appearance = self
                        .scene
                        .appearance_for(self.displayed_variable.as_deref());
                    let title = self
                        .title_with_surface(&appearance.title, &data.header)
                        .unwrap_or_else(|_| data.header.canonical_name.clone());
                    ui.painter().text(
                        canvas_rect.left_top() + egui::vec2(20.0, 15.0),
                        egui::Align2::LEFT_TOP,
                        title,
                        FontId::proportional(24.0),
                        Color32::from_rgb(226, 232, 240),
                    );
                    ui.painter().text(
                        canvas_rect.left_top() + egui::vec2(20.0, 45.0),
                        egui::Align2::LEFT_TOP,
                        format!(
                            "{} · {} · {} rendered layer{}",
                            data.header.variable,
                            data.header.zone_name,
                            data.header.layers.len(),
                            if data.header.layers.len() == 1 {
                                ""
                            } else {
                                "s"
                            }
                        ),
                        FontId::proportional(13.5),
                        MUTED,
                    );
                    if let Some((colorbar_appearance, colorbar_limits, unit)) =
                        self.active_3d_colorbar(&data)
                    {
                        let colorbar = colorbar_rect_3d(canvas_rect);
                        let steps = colorbar.height().max(1.0) as usize;
                        for index in 0..steps {
                            let t = 1.0 - index as f32 / steps.max(1) as f32;
                            let color = sample_appearance(&colorbar_appearance, t);
                            let y = colorbar.top() + index as f32;
                            ui.painter().line_segment(
                                [
                                    egui::pos2(colorbar.left(), y),
                                    egui::pos2(colorbar.right(), y),
                                ],
                                Stroke::new(1.5, color),
                            );
                        }
                        ui.painter().rect_stroke(
                            colorbar,
                            0.0,
                            Stroke::new(1.0, MUTED),
                            StrokeKind::Inside,
                        );
                        for tick in colorbar_ticks(
                            &colorbar_appearance.ticks,
                            colorbar_limits,
                            colorbar_appearance.scale,
                        ) {
                            if let Some(normalized) = normalized_value(
                                tick.value,
                                colorbar_limits,
                                colorbar_appearance.scale,
                            ) {
                                let y = colorbar.bottom() - normalized * colorbar.height();
                                ui.painter().line_segment(
                                    [
                                        egui::pos2(colorbar.right(), y),
                                        egui::pos2(colorbar.right() + 4.0, y),
                                    ],
                                    Stroke::new(1.0, MUTED),
                                );
                                ui.painter().text(
                                    egui::pos2(colorbar.right() + 7.0, y),
                                    egui::Align2::LEFT_CENTER,
                                    tick.label,
                                    FontId::monospace(12.5),
                                    Color32::from_rgb(226, 232, 240),
                                );
                            }
                        }
                        if !unit.is_empty() {
                            ui.painter().text(
                                colorbar.center_top() - egui::vec2(0.0, 8.0),
                                egui::Align2::CENTER_BOTTOM,
                                unit,
                                FontId::proportional(12.5),
                                Color32::from_rgb(226, 232, 240),
                            );
                        }
                    }
                } else {
                    ui.painter().text(
                        canvas_rect.center(),
                        egui::Align2::CENTER_CENTER,
                        "Open a 3D BATS-R-US file and select a variable",
                        FontId::proportional(20.0),
                        MUTED,
                    );
                }
                if self.loading {
                    ui.painter().text(
                        canvas_rect.left_bottom() + egui::vec2(18.0, -16.0),
                        egui::Align2::LEFT_BOTTOM,
                        "Extracting 3D scene… previous frame remains visible",
                        FontId::proportional(13.0),
                        MUTED,
                    );
                }
                let _ = response;
                ui.add_space(7.0);
                self.timeline_bar(ui);
            });
    }

    fn timeline_bar(&mut self, ui: &mut egui::Ui) {
        let timeline = self.timeline_indices();
        let position = self.timeline_position(&timeline).unwrap_or(0);
        let mut slider_position = self.scrub_target.unwrap_or(position);
        let previous_fps = self.playback_fps;
        let previous_loop = self.playback_loop;
        let mut requested_position = None;
        let mut toggle_playback = false;
        let mut scrub_stopped = false;
        egui::Frame::new()
            .fill(PANEL_BG)
            .corner_radius(6)
            .stroke(Stroke::new(1.0, Color32::from_rgb(31, 43, 57)))
            .inner_margin(egui::Margin::symmetric(10, 6))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    let has_previous = !timeline.is_empty() && slider_position > 0;
                    if toolbar_icon_button(
                        ui,
                        ToolbarIcon::Previous,
                        false,
                        has_previous,
                        "Previous frame",
                    ) {
                        requested_position = slider_position.checked_sub(1);
                    }
                    if toolbar_icon_button(
                        ui,
                        if self.playing {
                            ToolbarIcon::Pause
                        } else {
                            ToolbarIcon::Play
                        },
                        self.playing,
                        timeline.len() > 1
                            && match self.view_mode {
                                ViewMode::TwoD => self.plot.lock().unwrap().data.is_some(),
                                ViewMode::ThreeD => self.scene3d.lock().unwrap().data.is_some(),
                            },
                        if self.playing { "Pause" } else { "Play" },
                    ) {
                        toggle_playback = true;
                    }
                    let has_next = slider_position + 1 < timeline.len();
                    if toolbar_icon_button(ui, ToolbarIcon::Next, false, has_next, "Next frame") {
                        requested_position = Some(slider_position + 1);
                    }
                    ui.add_space(4.0);
                    let slider_width = (ui.available_width() - 330.0).max(100.0);
                    let slider = ui.add_sized(
                        [slider_width, 24.0],
                        egui::Slider::new(
                            &mut slider_position,
                            0..=timeline.len().saturating_sub(1),
                        )
                        .show_value(false),
                    );
                    if slider.changed() {
                        self.pause_playback();
                        self.scrub_target = Some(slider_position);
                        self.scrub_changed_at = Some(Instant::now());
                    }
                    scrub_stopped = slider.drag_stopped();
                    ui.label(format!(
                        "{} / {}",
                        slider_position.saturating_add(1).min(timeline.len()),
                        timeline.len()
                    ));
                    if let Some(index) = timeline.get(slider_position) {
                        let file = &self.files[*index];
                        ui.label(
                            RichText::new(format!(
                                "t={}  n={}",
                                file.time_step.map_or_else(|| "-".into(), |v| v.to_string()),
                                file.dump_index
                                    .map_or_else(|| "-".into(), |v| v.to_string())
                            ))
                            .color(MUTED),
                        );
                    }
                    ui.add(
                        egui::DragValue::new(&mut self.playback_fps)
                            .range(0.5..=30.0)
                            .speed(0.5)
                            .fixed_decimals(1)
                            .suffix(" FPS"),
                    );
                    ui.checkbox(&mut self.playback_loop, "Loop");
                    if self.buffering {
                        ui.spinner();
                        ui.label(RichText::new("Buffering").small().color(MUTED));
                    }
                });
            });
        if self.playing && self.playback_fps != previous_fps {
            self.schedule_next_playback_frame();
        }
        if self.playback_loop != previous_loop {
            self.schedule_prefetch();
        }
        if toggle_playback {
            self.toggle_playback();
        }
        if let Some(position) = requested_position {
            self.scrub_target = None;
            self.scrub_changed_at = None;
            self.request_timeline_position(position, true);
        } else if (scrub_stopped
            || self
                .scrub_changed_at
                .is_some_and(|changed| changed.elapsed() >= Duration::from_millis(75)))
            && self.scrub_target.is_some()
        {
            let position = self.scrub_target.take().expect("scrub target checked");
            self.scrub_changed_at = None;
            self.request_timeline_position(position, true);
        }
    }

    fn preview_title(&self, config: &TitleConfig) -> Result<String, String> {
        if self.view_mode == ViewMode::ThreeD {
            let shared = self.scene3d.lock().unwrap();
            let data = shared
                .data
                .as_ref()
                .ok_or_else(|| "Load a variable to preview the title".to_owned())?;
            return self.title_with_surface(config, &data.header);
        }
        let shared = self.plot.lock().unwrap();
        let data = shared
            .data
            .as_ref()
            .ok_or_else(|| "Load a variable to preview the title".to_owned())?;
        self.title_with_header(config, &data.header)
    }

    fn plot_chrome(&self, header: &crate::protocol::PlotHeader) -> PlotChrome {
        let appearance = self
            .scene
            .appearance_for(self.displayed_variable.as_deref());
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
        let appearance = self
            .scene
            .appearance_for(self.displayed_variable.as_deref());
        Some(ExportFrame {
            render_state,
            plot: self.plot.clone(),
            scene: self.scene.clone(),
            scope_section,
            scope_variable,
            scope_relative_path,
            appearance,
            streamlines: self
                .streamline_overlay
                .as_ref()
                .filter(|overlay| self.displayed_path.as_deref() == Some(overlay.path.as_str()))
                .cloned(),
            chrome: self.plot_chrome(&header),
            logical_size,
            pixels_per_point,
            settings,
            destination,
        })
    }

    fn export_frame_3d(
        &self,
        destination: PathBuf,
        settings: ExportSettings,
        pixels_per_point: f32,
    ) -> Option<ExportFrame3d> {
        let render_state = self.render_state.clone()?;
        let logical_size = self.last_export_rect?.size();
        let data = self.scene3d.lock().unwrap().data.clone()?;
        let appearance = self
            .scene
            .appearance_for(self.displayed_variable.as_deref());
        let active_colorbar = self.active_3d_colorbar(data.as_ref());
        let title = self
            .title_with_surface(&appearance.title, &data.header)
            .unwrap_or_else(|_| data.header.canonical_name.clone());
        let subtitle = format!(
            "{} · {} · {} rendered layer{}{}",
            data.header.variable,
            data.header.zone_name,
            data.header.layers.len(),
            if data.header.layers.len() == 1 {
                ""
            } else {
                "s"
            },
            if data.header.unit.is_empty() {
                String::new()
            } else {
                format!(" · {}", data.header.unit)
            }
        );
        Some(ExportFrame3d {
            render_state,
            scene: self.scene3d.clone(),
            appearance: active_colorbar
                .as_ref()
                .map_or_else(|| appearance.clone(), |active| active.0.clone()),
            show_colorbar: active_colorbar.is_some(),
            colorbar_limits: active_colorbar
                .as_ref()
                .map_or([0.0, 1.0], |active| active.1),
            title,
            subtitle,
            unit: active_colorbar.map_or_else(String::new, |active| active.2),
            fieldlines: self.fieldlines3d.clone(),
            fieldline_settings: self.scene.fieldlines3d_for(Some(&data.header.section)),
            measurements: self.scene.measurements.clone(),
            scope_variable: self.displayed_variable.clone().unwrap_or_default(),
            scope_relative_path: self.scope_values().2.unwrap_or_default(),
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

    fn title_with_surface(
        &self,
        config: &TitleConfig,
        header: &crate::protocol::Surface3dHeader,
    ) -> Result<String, String> {
        let file = Path::new(&header.source)
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or(&header.source);
        let run = self
            .directory
            .as_ref()
            .and_then(|path| path.file_name())
            .and_then(|name| name.to_str())
            .unwrap_or("");
        render_title(
            config,
            &TitleContext {
                variable: &header.canonical_name,
                source: &header.variable,
                unit: (!header.unit.is_empty()).then_some(header.unit.as_str()),
                section: Some(&header.section),
                time: header
                    .time
                    .filter(|value| value.is_finite() && *value >= 0.0)
                    .map(|value| value.round() as u64),
                dump: header
                    .dump
                    .filter(|value| *value >= 0)
                    .map(|value| value as u64),
                zone: &header.zone_name,
                file,
                run,
                dataset_title: &header.dataset_title,
            },
        )
    }

    fn selected_file(&self) -> Option<&PlotFile> {
        let path = self.displayed_path.as_deref()?;
        self.files.iter().find(|file| file.path == path)
    }

    fn scope_values(&self) -> (Option<String>, Option<String>, Option<String>) {
        let section = self.selected_file().and_then(|file| file.section.clone());
        let variable = self.displayed_variable.clone();
        let relative = self.displayed_path.as_ref().map(|path| {
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
        self.poll_probe_index();
        self.slice_debounce_tick(&context);
        self.playback_tick();
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
        if self.loading
            || self.choosing_run
            || self.io_busy
            || self.streamline_loading
            || self.fieldlines3d_loading
            || self.probe_indexing
            || self.playing
            || self.scrub_target.is_some()
            || self.slice_changed_at.is_some()
        {
            context.request_repaint_after(Duration::from_millis(40));
        }
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        self.stash_current_scene();
        let state = PersistedAppState {
            recursive: self.recursive,
            recent_runs: self.stored_runs.clone(),
            cache_limit_mib: self.cache_limit_mib,
            playback_fps: self.playback_fps,
            playback_loop: self.playback_loop,
        };
        eframe::set_value(storage, APP_STORAGE_KEY, &state);
    }
}

fn configure_style(context: &egui::Context) {
    context.set_theme(egui::Theme::Dark);
    let mut style = (*context.style_of(egui::Theme::Dark)).clone();
    style.text_styles.insert(
        egui::TextStyle::Heading,
        FontId::new(22.0, egui::FontFamily::Proportional),
    );
    style.text_styles.insert(
        egui::TextStyle::Body,
        FontId::new(15.5, egui::FontFamily::Proportional),
    );
    style.text_styles.insert(
        egui::TextStyle::Button,
        FontId::new(14.5, egui::FontFamily::Proportional),
    );
    style.text_styles.insert(
        egui::TextStyle::Small,
        FontId::new(13.0, egui::FontFamily::Proportional),
    );
    style.text_styles.insert(
        egui::TextStyle::Monospace,
        FontId::new(13.5, egui::FontFamily::Monospace),
    );
    style.spacing.item_spacing = egui::vec2(9.0, 9.0);
    style.spacing.button_padding = egui::vec2(12.0, 7.0);
    style.spacing.interact_size = egui::vec2(42.0, 32.0);
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
        ToolbarIcon::StreamlineSeed => {
            let points: Vec<_> = (0..12)
                .map(|index| {
                    let t = index as f32 / 11.0;
                    egui::pos2(
                        left + t * rect.width(),
                        center.y + (t * std::f32::consts::TAU).sin() * rect.height() * 0.22,
                    )
                })
                .collect();
            painter.add(egui::Shape::line(points, stroke));
            let plus = egui::pos2(right - 2.5, top + 2.5);
            painter.line_segment(
                [plus - egui::vec2(3.0, 0.0), plus + egui::vec2(3.0, 0.0)],
                stroke,
            );
            painter.line_segment(
                [plus - egui::vec2(0.0, 3.0), plus + egui::vec2(0.0, 3.0)],
                stroke,
            );
        }
        ToolbarIcon::Probe => {
            painter.circle_stroke(center, rect.width() * 0.3, stroke);
            painter.line_segment(
                [egui::pos2(center.x, top), egui::pos2(center.x, bottom)],
                stroke,
            );
            painter.line_segment(
                [egui::pos2(left, center.y), egui::pos2(right, center.y)],
                stroke,
            );
            painter.circle_filled(center, 1.8, color);
        }
        ToolbarIcon::Previous | ToolbarIcon::Next => {
            let next = matches!(icon, ToolbarIcon::Next);
            let direction = if next { 1.0 } else { -1.0 };
            let bar_x = if next { right - 1.5 } else { left + 1.5 };
            painter.line_segment(
                [
                    egui::pos2(bar_x, top + 1.0),
                    egui::pos2(bar_x, bottom - 1.0),
                ],
                stroke,
            );
            let tip_x = if next { right - 4.0 } else { left + 4.0 };
            let base_x = tip_x - direction * (rect.width() - 7.0);
            painter.add(egui::Shape::convex_polygon(
                vec![
                    egui::pos2(tip_x, center.y),
                    egui::pos2(base_x, top + 2.0),
                    egui::pos2(base_x, bottom - 2.0),
                ],
                color,
                Stroke::NONE,
            ));
        }
        ToolbarIcon::Play => {
            painter.add(egui::Shape::convex_polygon(
                vec![
                    egui::pos2(left + 3.0, top + 1.0),
                    egui::pos2(right - 1.0, center.y),
                    egui::pos2(left + 3.0, bottom - 1.0),
                ],
                color,
                Stroke::NONE,
            ));
        }
        ToolbarIcon::Pause => {
            let width = 4.0;
            painter.rect_filled(
                egui::Rect::from_min_max(
                    egui::pos2(left + 3.0, top + 1.0),
                    egui::pos2(left + 3.0 + width, bottom - 1.0),
                ),
                0.5,
                color,
            );
            painter.rect_filled(
                egui::Rect::from_min_max(
                    egui::pos2(right - 3.0 - width, top + 1.0),
                    egui::pos2(right - 3.0, bottom - 1.0),
                ),
                0.5,
                color,
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

fn paint_probe_readout(ui: &egui::Ui, clip: egui::Rect, pointer: egui::Pos2, hit: &ProbeHit) {
    let unit = hit
        .unit
        .as_deref()
        .map_or(String::new(), |unit| format!(" {unit}"));
    let text = if hit.position[2].abs() > 1.0e-8 || hit.layer_id.is_some() {
        format!(
            "{}\n({:.3}, {:.3}, {:.3})\n{} = {:.6e}{}",
            hit.layer_name,
            hit.position[0],
            hit.position[1],
            hit.position[2],
            hit.variable,
            hit.value,
            unit
        )
    } else {
        format!(
            "({:.3}, {:.3})\n{} = {:.6e}{}",
            hit.position[0], hit.position[1], hit.variable, hit.value, unit
        )
    };
    let size = egui::vec2(
        (text.lines().map(str::len).max().unwrap_or(1) as f32 * 7.2 + 16.0).min(300.0),
        text.lines().count() as f32 * 17.0 + 12.0,
    );
    let mut position = pointer + egui::vec2(14.0, 14.0);
    if position.x + size.x > clip.right() {
        position.x = pointer.x - size.x - 14.0;
    }
    if position.y + size.y > clip.bottom() {
        position.y = pointer.y - size.y - 14.0;
    }
    let rect = egui::Rect::from_min_size(position, size);
    let painter = ui.painter().with_clip_rect(clip);
    painter.rect_filled(rect, 5.0, Color32::from_rgba_unmultiplied(7, 12, 18, 235));
    painter.rect_stroke(
        rect,
        5.0,
        Stroke::new(1.0, Color32::from_rgb(79, 112, 143)),
        StrokeKind::Inside,
    );
    painter.text(
        rect.left_top() + egui::vec2(8.0, 6.0),
        egui::Align2::LEFT_TOP,
        text,
        FontId::monospace(12.5),
        Color32::from_rgb(226, 232, 240),
    );
}

fn paint_probe_status(ui: &egui::Ui, clip: egui::Rect, text: &str) {
    ui.painter().with_clip_rect(clip).text(
        clip.left_bottom() + egui::vec2(10.0, -10.0),
        egui::Align2::LEFT_BOTTOM,
        text,
        FontId::proportional(12.5),
        MUTED,
    );
}

fn paint_probe_measurements_2d(
    ui: &egui::Ui,
    rect: egui::Rect,
    bounds: [f32; 4],
    measurements: &[ProbeMeasurement],
    relative_path: &str,
    variable: &str,
) {
    let painter = ui.painter().with_clip_rect(rect);
    for measurement in measurements.iter().filter(|measurement| {
        measurement.visible
            && measurement.dimension == ProbeDimension::TwoD
            && measurement.relative_path == relative_path
            && measurement.scope_variable == variable
    }) {
        let position = crate::annotations::data_to_screen(
            DataPoint::new(measurement.position[0], measurement.position[1]),
            rect,
            bounds,
        );
        paint_probe_pin(&painter, position, measurement);
    }
}

fn paint_probe_measurements_3d(
    ui: &egui::Ui,
    rect: egui::Rect,
    scene: &Scene3dHandle,
    measurements: &[ProbeMeasurement],
    relative_path: &str,
    variable: &str,
) {
    let shared = scene.lock().unwrap();
    let Some(data) = &shared.data else { return };
    let aspect = rect.width() / rect.height().max(1.0);
    let painter = ui.painter().with_clip_rect(rect);
    for measurement in measurements.iter().filter(|measurement| {
        measurement.visible
            && measurement.dimension == ProbeDimension::ThreeD
            && measurement.relative_path == relative_path
            && measurement.scope_variable == variable
    }) {
        let point = measurement.position.map(|value| value as f32);
        let Some(projected) = shared
            .camera
            .project(point, data.header.active_bounds(), aspect)
        else {
            continue;
        };
        if !(0.0..=1.0).contains(&projected[2]) {
            continue;
        }
        let position = egui::pos2(
            rect.left() + (projected[0] + 1.0) * 0.5 * rect.width(),
            rect.bottom() - (projected[1] + 1.0) * 0.5 * rect.height(),
        );
        paint_probe_pin(&painter, position, measurement);
    }
}

fn paint_probe_pin(painter: &egui::Painter, position: egui::Pos2, measurement: &ProbeMeasurement) {
    let color = Color32::from_rgb(255, 205, 74);
    painter.circle_filled(position, 3.5, color);
    painter.circle_stroke(position, 5.5, Stroke::new(1.0, Color32::BLACK));
    painter.text(
        position + egui::vec2(8.0, -7.0),
        egui::Align2::LEFT_BOTTOM,
        format!("{}  {:.5e}", measurement.name, measurement.value),
        FontId::proportional(12.0),
        color,
    );
}

fn section_heading(ui: &mut egui::Ui, text: &str) {
    ui.label(
        RichText::new(text.to_uppercase())
            .size(12.0)
            .strong()
            .color(ACCENT),
    );
    ui.add_space(3.0);
}

fn selectable_metadata_row(
    ui: &mut egui::Ui,
    selected: bool,
    primary: &str,
    secondary: &str,
) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(
        egui::vec2(ui.available_width().max(1.0), 58.0),
        Sense::click(),
    );
    let fill = if selected {
        Color32::from_rgb(28, 66, 96)
    } else if response.hovered() {
        Color32::from_rgb(25, 36, 49)
    } else {
        Color32::from_rgb(18, 27, 38)
    };
    let border = if selected {
        Color32::from_rgb(74, 166, 226)
    } else if response.hovered() {
        Color32::from_rgb(54, 78, 99)
    } else {
        Color32::from_rgb(31, 43, 57)
    };
    ui.painter().rect_filled(rect, 5.0, fill);
    ui.painter()
        .rect_stroke(rect, 5.0, Stroke::new(1.0, border), StrokeKind::Inside);
    if selected {
        ui.painter().line_segment(
            [
                rect.left_top() + egui::vec2(2.0, 7.0),
                rect.left_bottom() + egui::vec2(2.0, -7.0),
            ],
            Stroke::new(3.0, ACCENT),
        );
    }
    let painter = ui
        .painter()
        .with_clip_rect(rect.shrink2(egui::vec2(9.0, 0.0)));
    painter.text(
        rect.left_top() + egui::vec2(11.0, 8.0),
        egui::Align2::LEFT_TOP,
        primary,
        FontId::proportional(14.5),
        ui.visuals().strong_text_color(),
    );
    painter.text(
        rect.left_top() + egui::vec2(11.0, 33.0),
        egui::Align2::LEFT_TOP,
        secondary,
        FontId::proportional(12.5),
        MUTED,
    );
    response
}

fn metadata_row(ui: &mut egui::Ui, label: &str, value: &str) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(label).color(MUTED));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(value);
        });
    });
}

fn variable_combo(
    ui: &mut egui::Ui,
    label: &str,
    selected: &mut Option<String>,
    variables: &[(String, String)],
) {
    egui::ComboBox::from_label(label)
        .selected_text(selected.as_deref().unwrap_or("Choose a variable"))
        .width(ui.available_width().min(250.0))
        .show_ui(ui, |ui| {
            for (canonical, display) in variables {
                ui.selectable_value(selected, Some(canonical.clone()), display);
            }
        });
}

fn components_are_distinct(components: &[Option<String>; 3]) -> bool {
    let [Some(x), Some(y), Some(z)] = components else {
        return false;
    };
    x != y && x != z && y != z
}

fn latitude_seed_controls(
    ui: &mut egui::Ui,
    settings: &mut LatitudeSeedSettings,
    include_longitudes: bool,
) {
    section_heading(ui, "Planetary footpoints");
    ui.checkbox(&mut settings.enabled, "Seed along selected latitudes");
    ui.label(
        RichText::new("Seeds are used for tracing but are not drawn in the viewer.")
            .small()
            .color(MUTED),
    );
    if !settings.enabled {
        return;
    }
    ui.horizontal(|ui| {
        ui.label("Footpoint radius");
        ui.add(
            egui::DragValue::new(&mut settings.radius)
                .range(0.1..=50.0)
                .speed(0.05)
                .suffix(" Re"),
        );
    });
    if include_longitudes {
        ui.add(
            egui::DragValue::new(&mut settings.longitude_count)
                .range(1..=36)
                .prefix("Longitudes per latitude "),
        );
    }
    ui.label(RichText::new("Latitudes").small().color(MUTED));
    let mut remove = None;
    for (index, latitude) in settings.latitudes.iter_mut().enumerate() {
        ui.horizontal(|ui| {
            ui.add(
                egui::DragValue::new(latitude)
                    .range(-90.0..=90.0)
                    .speed(1.0)
                    .suffix("°"),
            );
            if ui
                .small_button("×")
                .on_hover_text("Remove latitude")
                .clicked()
            {
                remove = Some(index);
            }
        });
    }
    if let Some(index) = remove {
        settings.latitudes.remove(index);
    }
    ui.horizontal(|ui| {
        if ui.small_button("Add latitude").clicked() {
            settings.latitudes.push(0.0);
        }
        let count = settings.latitudes.len()
            * if include_longitudes {
                usize::from(settings.longitude_count.max(1))
            } else {
                2
            };
        ui.small(RichText::new(format!("{count} generated footpoints")).color(MUTED));
    });
}

fn coordinate_axis(label: &str) -> Option<char> {
    label
        .trim_start()
        .chars()
        .next()
        .map(|axis| axis.to_ascii_lowercase())
        .filter(|axis| matches!(axis, 'x' | 'y' | 'z'))
}

fn seed_grid(bounds: [f32; 4], columns: u8, rows: u8) -> Vec<DataPoint> {
    let columns = usize::from(columns.clamp(1, 16));
    let rows = usize::from(rows.clamp(1, 16));
    let x_margin = (bounds[1] - bounds[0]) * 0.06;
    let y_margin = (bounds[3] - bounds[2]) * 0.06;
    let x_min = bounds[0] + x_margin;
    let x_max = bounds[1] - x_margin;
    let y_min = bounds[2] + y_margin;
    let y_max = bounds[3] - y_margin;
    let coordinate = |index: usize, count: usize, minimum: f32, maximum: f32| {
        if count == 1 {
            0.5 * (minimum + maximum)
        } else {
            minimum + (maximum - minimum) * index as f32 / (count - 1) as f32
        }
    };
    (0..rows)
        .flat_map(|row| {
            (0..columns).map(move |column| {
                DataPoint::new(
                    f64::from(coordinate(column, columns, x_min, x_max)),
                    f64::from(coordinate(row, rows, y_min, y_max)),
                )
            })
        })
        .collect()
}

fn latitude_footpoints3d(settings: &LatitudeSeedSettings) -> Vec<DataPoint3> {
    if !settings.enabled || !settings.radius.is_finite() || settings.radius <= 0.0 {
        return Vec::new();
    }
    let longitude_count = usize::from(settings.longitude_count.clamp(1, 36));
    settings
        .latitudes
        .iter()
        .copied()
        .filter(|latitude| latitude.is_finite() && (-90.0..=90.0).contains(latitude))
        .flat_map(|latitude| {
            let latitude = f64::from(latitude).to_radians();
            (0..longitude_count).map(move |index| {
                let longitude = std::f64::consts::TAU * index as f64 / longitude_count as f64;
                let radius = f64::from(settings.radius);
                DataPoint3::new(
                    radius * latitude.cos() * longitude.cos(),
                    radius * latitude.cos() * longitude.sin(),
                    radius * latitude.sin(),
                )
            })
        })
        .collect()
}

fn seed_grid3d(bounds: [f32; 6], counts: [u8; 3]) -> Vec<DataPoint3> {
    if bounds.iter().any(|value| !value.is_finite())
        || bounds[0] > bounds[1]
        || bounds[2] > bounds[3]
        || bounds[4] > bounds[5]
    {
        return Vec::new();
    }
    let counts = counts.map(|count| usize::from(count.clamp(1, 12)));
    let coordinate = |index: usize, count: usize, minimum: f32, maximum: f32| {
        if count == 1 {
            0.5 * f64::from(minimum + maximum)
        } else {
            f64::from(minimum) + f64::from(maximum - minimum) * index as f64 / (count - 1) as f64
        }
    };
    let mut seeds = Vec::with_capacity(counts[0] * counts[1] * counts[2]);
    for z in 0..counts[2] {
        for y in 0..counts[1] {
            for x in 0..counts[0] {
                seeds.push(DataPoint3::new(
                    coordinate(x, counts[0], bounds[0], bounds[1]),
                    coordinate(y, counts[1], bounds[2], bounds[3]),
                    coordinate(z, counts[2], bounds[4], bounds[5]),
                ));
            }
        }
    }
    seeds
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

fn next_playback_position(position: usize, frame_count: usize, looping: bool) -> Option<usize> {
    if position + 1 < frame_count {
        Some(position + 1)
    } else if looping && frame_count > 1 {
        Some(0)
    } else {
        None
    }
}

fn streamline_overlay_matches(
    overlay: &StreamlineOverlay,
    section: Option<&str>,
    horizontal_component: &str,
    vertical_component: &str,
) -> bool {
    overlay.section.as_deref() == section
        && overlay.horizontal_component == horizontal_component
        && overlay.vertical_component == vertical_component
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

    #[test]
    fn playback_advances_in_order_and_only_wraps_when_looping() {
        assert_eq!(next_playback_position(0, 3, false), Some(1));
        assert_eq!(next_playback_position(2, 3, false), None);
        assert_eq!(next_playback_position(2, 3, true), Some(0));
        assert_eq!(next_playback_position(0, 1, true), None);
    }

    #[test]
    fn compatible_streamline_overlay_can_bridge_a_timestep_change() {
        let overlay = StreamlineOverlay {
            path: "frame-1.plt".into(),
            section: Some("z=0".into()),
            horizontal_component: "magnetic_field.x".into(),
            vertical_component: "magnetic_field.y".into(),
            lines: Vec::new(),
            settings: Default::default(),
        };
        assert!(streamline_overlay_matches(
            &overlay,
            Some("z=0"),
            "magnetic_field.x",
            "magnetic_field.y"
        ));
        assert!(!streamline_overlay_matches(
            &overlay,
            Some("y=0"),
            "magnetic_field.x",
            "magnetic_field.z"
        ));
    }

    #[test]
    fn plot_axis_labels_map_to_magnetic_component_axes() {
        assert_eq!(coordinate_axis("X [R]"), Some('x'));
        assert_eq!(coordinate_axis(" y [R]"), Some('y'));
        assert_eq!(coordinate_axis("Z"), Some('z'));
        assert_eq!(coordinate_axis("Longitude"), None);
    }

    #[test]
    fn seed_grid_stays_inside_bounds_and_has_requested_size() {
        let seeds = seed_grid([-10.0, 10.0, -5.0, 5.0], 4, 3);
        assert_eq!(seeds.len(), 12);
        assert!(seeds.iter().all(|seed| seed.x > -10.0 && seed.x < 10.0));
        assert!(seeds.iter().all(|seed| seed.y > -5.0 && seed.y < 5.0));
    }

    #[test]
    fn three_dimensional_footpoints_follow_latitude_and_radius() {
        let settings = LatitudeSeedSettings {
            enabled: true,
            radius: 2.5,
            latitudes: vec![-30.0, 30.0],
            longitude_count: 4,
        };
        let seeds = latitude_footpoints3d(&settings);
        assert_eq!(seeds.len(), 8);
        assert!(seeds.iter().all(|seed| {
            (seed.x * seed.x + seed.y * seed.y + seed.z * seed.z - 6.25).abs() < 1.0e-9
        }));
        assert!(seeds.iter().any(|seed| (seed.z - 1.25).abs() < 1.0e-6));
        assert!(seeds.iter().any(|seed| (seed.z + 1.25).abs() < 1.0e-6));
    }

    #[test]
    fn custom_three_dimensional_seed_grid_has_exact_bounds() {
        let seeds = seed_grid3d([-1.0, 1.0, -2.0, 2.0, 3.0, 5.0], [2, 3, 2]);
        assert_eq!(seeds.len(), 12);
        assert!(
            seeds
                .iter()
                .any(|seed| seed.as_array() == [-1.0, -2.0, 3.0])
        );
        assert!(seeds.iter().any(|seed| seed.as_array() == [1.0, 2.0, 5.0]));
    }
}
