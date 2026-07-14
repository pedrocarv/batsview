use std::collections::{BTreeMap, HashSet};

use anyhow::{Result, bail};
use eframe::egui::Color32;
use serde::{Deserialize, Serialize};

pub const SCENE_VERSION: u32 = 1;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Scale {
    #[default]
    Linear,
    Logarithmic,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Colormap {
    #[default]
    Viridis,
    Plasma,
    Inferno,
    Magma,
    Cividis,
    Turbo,
    Gray,
    Coolwarm,
    RdBu,
}

impl Colormap {
    pub const ALL: [Self; 9] = [
        Self::Viridis,
        Self::Plasma,
        Self::Inferno,
        Self::Magma,
        Self::Cividis,
        Self::Turbo,
        Self::Gray,
        Self::Coolwarm,
        Self::RdBu,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Self::Viridis => "Viridis",
            Self::Plasma => "Plasma",
            Self::Inferno => "Inferno",
            Self::Magma => "Magma",
            Self::Cividis => "Cividis",
            Self::Turbo => "Turbo",
            Self::Gray => "Gray",
            Self::Coolwarm => "Coolwarm",
            Self::RdBu => "RdBu",
        }
    }

    pub fn index(self) -> usize {
        Self::ALL
            .iter()
            .position(|candidate| *candidate == self)
            .unwrap_or(0)
    }

    pub fn sample(self, value: f32) -> RgbaColor {
        let x = value.clamp(0.0, 1.0);
        if self == Self::Turbo {
            return turbo(x);
        }
        let stops = match self {
            Self::Viridis => &VIRIDIS[..],
            Self::Plasma => &PLASMA[..],
            Self::Inferno => &INFERNO[..],
            Self::Magma => &MAGMA[..],
            Self::Cividis => &CIVIDIS[..],
            Self::Gray => &GRAY[..],
            Self::Coolwarm => &COOLWARM[..],
            Self::RdBu => &RDBU[..],
            Self::Turbo => unreachable!(),
        };
        interpolate_stops(stops, x)
    }

    pub fn lookup_texture() -> Vec<u8> {
        let mut values = Vec::with_capacity(Self::ALL.len() * 256 * 4);
        for map in Self::ALL {
            for index in 0..256 {
                values.extend_from_slice(&map.sample(index as f32 / 255.0).0);
            }
        }
        values
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum ColorMode {
    #[default]
    Continuous,
    Discrete {
        bins: u8,
    },
}

impl ColorMode {
    pub fn bins(self) -> Option<u8> {
        match self {
            Self::Continuous => None,
            Self::Discrete { bins } => Some(bins.clamp(2, 32)),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ColorbarTick {
    pub value: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

impl Default for ColorbarTick {
    fn default() -> Self {
        Self {
            value: 0.0,
            label: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum TickMode {
    Automatic { count: u8 },
    Custom { ticks: Vec<ColorbarTick> },
}

impl Default for TickMode {
    fn default() -> Self {
        Self::Automatic { count: 7 }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "style", content = "precision", rename_all = "snake_case")]
pub enum NumberFormat {
    #[default]
    Automatic,
    Fixed(u8),
    Scientific(u8),
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ColorbarTickConfig {
    #[serde(default)]
    pub mode: TickMode,
    #[serde(default)]
    pub format: NumberFormat,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TitleConfig {
    #[serde(default = "default_title_template")]
    pub template: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub override_text: Option<String>,
}

impl Default for TitleConfig {
    fn default() -> Self {
        Self {
            template: default_title_template(),
            override_text: None,
        }
    }
}

fn default_title_template() -> String {
    "{variable}".to_owned()
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AppearanceSettings {
    #[serde(default)]
    pub scale: Scale,
    #[serde(default)]
    pub colormap: Colormap,
    #[serde(default)]
    pub reversed: bool,
    #[serde(default)]
    pub color_mode: ColorMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color_limits: Option<[f32; 2]>,
    #[serde(default)]
    pub ticks: ColorbarTickConfig,
    #[serde(default)]
    pub title: TitleConfig,
}

impl Default for AppearanceSettings {
    fn default() -> Self {
        Self {
            scale: Scale::Linear,
            colormap: Colormap::Viridis,
            reversed: false,
            color_mode: ColorMode::Continuous,
            color_limits: None,
            ticks: ColorbarTickConfig::default(),
            title: TitleConfig::default(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RgbaColor(pub [u8; 4]);

impl RgbaColor {
    pub fn to_egui(self) -> Color32 {
        Color32::from_rgba_unmultiplied(self.0[0], self.0[1], self.0[2], self.0[3])
    }

    pub fn from_egui(color: Color32) -> Self {
        Self(color.to_srgba_unmultiplied())
    }
}

impl Default for RgbaColor {
    fn default() -> Self {
        Self([92, 200, 255, 255])
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct DataPoint {
    pub x: f64,
    pub y: f64,
}

impl DataPoint {
    pub fn new(x: f64, y: f64) -> Self {
        Self { x, y }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StreamlineDirection {
    Forward,
    Backward,
    #[default]
    Both,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StreamlineSettings {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub horizontal_component: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vertical_component: Option<String>,
    #[serde(default)]
    pub seeds: Vec<DataPoint>,
    #[serde(default = "default_streamline_step")]
    pub step_fraction: f32,
    #[serde(default = "default_streamline_steps")]
    pub max_steps: u32,
    #[serde(default)]
    pub direction: StreamlineDirection,
    #[serde(default = "default_streamline_color")]
    pub color: RgbaColor,
    #[serde(default = "default_streamline_width")]
    pub width: f32,
    #[serde(default = "default_true")]
    pub arrows: bool,
    #[serde(default = "default_streamline_arrow_size")]
    pub arrow_size: f32,
    #[serde(default = "default_seed_columns")]
    pub seed_columns: u8,
    #[serde(default = "default_seed_rows")]
    pub seed_rows: u8,
}

fn default_streamline_step() -> f32 {
    0.003
}

const fn default_streamline_steps() -> u32 {
    1_200
}

fn default_streamline_color() -> RgbaColor {
    RgbaColor([238, 244, 252, 230])
}

fn default_streamline_width() -> f32 {
    1.5
}

fn default_streamline_arrow_size() -> f32 {
    7.0
}

const fn default_seed_columns() -> u8 {
    8
}

const fn default_seed_rows() -> u8 {
    6
}

impl Default for StreamlineSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            horizontal_component: None,
            vertical_component: None,
            seeds: Vec::new(),
            step_fraction: default_streamline_step(),
            max_steps: default_streamline_steps(),
            direction: StreamlineDirection::Both,
            color: default_streamline_color(),
            width: default_streamline_width(),
            arrows: true,
            arrow_size: default_streamline_arrow_size(),
            seed_columns: default_seed_columns(),
            seed_rows: default_seed_rows(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AnnotationGeometry {
    Line {
        start: DataPoint,
        end: DataPoint,
    },
    Arrow {
        start: DataPoint,
        end: DataPoint,
    },
    Rectangle {
        start: DataPoint,
        end: DataPoint,
    },
    Ellipse {
        start: DataPoint,
        end: DataPoint,
        #[serde(default)]
        lock_aspect: bool,
    },
    Polyline {
        points: Vec<DataPoint>,
    },
    Polygon {
        points: Vec<DataPoint>,
    },
    Text {
        position: DataPoint,
        text: String,
    },
}

impl AnnotationGeometry {
    pub fn points(&self) -> Vec<DataPoint> {
        match self {
            Self::Line { start, end }
            | Self::Arrow { start, end }
            | Self::Rectangle { start, end }
            | Self::Ellipse { start, end, .. } => vec![*start, *end],
            Self::Polyline { points } | Self::Polygon { points } => points.clone(),
            Self::Text { position, .. } => vec![*position],
        }
    }

    pub fn points_mut(&mut self) -> Vec<&mut DataPoint> {
        match self {
            Self::Line { start, end }
            | Self::Arrow { start, end }
            | Self::Rectangle { start, end }
            | Self::Ellipse { start, end, .. } => vec![start, end],
            Self::Polyline { points } | Self::Polygon { points } => points.iter_mut().collect(),
            Self::Text { position, .. } => vec![position],
        }
    }

    pub fn display_name(&self) -> &'static str {
        match self {
            Self::Line { .. } => "Line",
            Self::Arrow { .. } => "Arrow",
            Self::Rectangle { .. } => "Rectangle",
            Self::Ellipse { .. } => "Ellipse",
            Self::Polyline { .. } => "Polyline",
            Self::Polygon { .. } => "Polygon",
            Self::Text { .. } => "Text",
        }
    }

    pub fn bounds(&self) -> Option<(DataPoint, DataPoint)> {
        let points = self.points();
        let first = *points.first()?;
        let mut minimum = first;
        let mut maximum = first;
        for point in points.into_iter().skip(1) {
            minimum.x = minimum.x.min(point.x);
            minimum.y = minimum.y.min(point.y);
            maximum.x = maximum.x.max(point.x);
            maximum.y = maximum.y.max(point.y);
        }
        Some((minimum, maximum))
    }

    pub fn center(&self) -> Option<DataPoint> {
        let (minimum, maximum) = self.bounds()?;
        Some(DataPoint::new(
            0.5 * (minimum.x + maximum.x),
            0.5 * (minimum.y + maximum.y),
        ))
    }

    pub fn translate(&mut self, dx: f64, dy: f64) {
        for point in self.points_mut() {
            point.x += dx;
            point.y += dy;
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DashStyle {
    #[default]
    Solid,
    Dashed,
    Dotted,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AnnotationStyle {
    #[serde(default)]
    pub stroke: RgbaColor,
    #[serde(default = "default_stroke_width")]
    pub stroke_width: f32,
    #[serde(default)]
    pub dash: DashStyle,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fill: Option<RgbaColor>,
    #[serde(default = "default_text_size")]
    pub text_size: f32,
    #[serde(default = "default_arrowhead")]
    pub arrowhead_size: f32,
}

fn default_stroke_width() -> f32 {
    2.0
}

fn default_text_size() -> f32 {
    16.0
}

fn default_arrowhead() -> f32 {
    10.0
}

impl Default for AnnotationStyle {
    fn default() -> Self {
        Self {
            stroke: RgbaColor::default(),
            stroke_width: default_stroke_width(),
            dash: DashStyle::Solid,
            fill: None,
            text_size: default_text_size(),
            arrowhead_size: default_arrowhead(),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "scope", rename_all = "snake_case")]
pub enum AnnotationScope {
    #[default]
    Run,
    Section {
        section: String,
    },
    Variable {
        variable: String,
    },
    Plot {
        relative_path: String,
        variable: String,
    },
}

impl AnnotationScope {
    pub fn label(&self) -> String {
        match self {
            Self::Run => "Whole run".to_owned(),
            Self::Section { section } => format!("Section · {section}"),
            Self::Variable { variable } => format!("Variable · {variable}"),
            Self::Plot {
                relative_path,
                variable,
            } => format!("Plot · {relative_path} · {variable}"),
        }
    }

    pub fn matches(&self, context: &ScopeContext<'_>) -> bool {
        match self {
            Self::Run => true,
            Self::Section { section } => context.section == Some(section.as_str()),
            Self::Variable { variable } => context.variable == Some(variable.as_str()),
            Self::Plot {
                relative_path,
                variable,
            } => {
                context.relative_path == Some(relative_path.as_str())
                    && context.variable == Some(variable.as_str())
            }
        }
    }
}

pub struct ScopeContext<'a> {
    pub section: Option<&'a str>,
    pub variable: Option<&'a str>,
    pub relative_path: Option<&'a str>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Annotation {
    pub id: u64,
    pub name: String,
    pub geometry: AnnotationGeometry,
    #[serde(default)]
    pub style: AnnotationStyle,
    #[serde(default)]
    pub scope: AnnotationScope,
    #[serde(default = "default_true")]
    pub visible: bool,
    #[serde(default)]
    pub locked: bool,
}

fn default_true() -> bool {
    true
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SceneDocument {
    pub version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_run: Option<String>,
    #[serde(default)]
    pub run_defaults: AppearanceSettings,
    #[serde(default)]
    pub variable_overrides: BTreeMap<String, AppearanceSettings>,
    #[serde(default)]
    pub annotations: Vec<Annotation>,
    #[serde(default)]
    pub streamlines: BTreeMap<String, StreamlineSettings>,
    #[serde(default = "first_annotation_id")]
    pub next_annotation_id: u64,
}

fn first_annotation_id() -> u64 {
    1
}

impl Default for SceneDocument {
    fn default() -> Self {
        Self {
            version: SCENE_VERSION,
            source_run: None,
            run_defaults: AppearanceSettings::default(),
            variable_overrides: BTreeMap::new(),
            annotations: Vec::new(),
            streamlines: BTreeMap::new(),
            next_annotation_id: first_annotation_id(),
        }
    }
}

impl SceneDocument {
    pub fn validate(&self) -> Result<()> {
        if self.version != SCENE_VERSION {
            bail!(
                "scene version {} is not supported (expected {})",
                self.version,
                SCENE_VERSION
            );
        }
        Ok(())
    }

    pub fn appearance_for(&self, variable: Option<&str>) -> AppearanceSettings {
        variable
            .and_then(|name| self.variable_overrides.get(name))
            .cloned()
            .unwrap_or_else(|| self.run_defaults.clone())
    }

    pub fn streamlines_for(&self, section: Option<&str>) -> StreamlineSettings {
        self.streamlines
            .get(section.unwrap_or_default())
            .cloned()
            .unwrap_or_default()
    }

    pub fn set_streamlines_for(&mut self, section: Option<&str>, settings: StreamlineSettings) {
        let key = section.unwrap_or_default().to_owned();
        if settings == StreamlineSettings::default() {
            self.streamlines.remove(&key);
        } else {
            self.streamlines.insert(key, settings);
        }
    }

    pub fn set_variable_override(&mut self, variable: &str, enabled: bool) {
        if enabled {
            let defaults = self.run_defaults.clone();
            self.variable_overrides
                .entry(variable.to_owned())
                .or_insert(defaults);
        } else {
            self.variable_overrides.remove(variable);
        }
    }

    pub fn add_annotation(
        &mut self,
        geometry: AnnotationGeometry,
        style: AnnotationStyle,
        scope: AnnotationScope,
    ) -> u64 {
        let id = self.next_annotation_id.max(1);
        self.next_annotation_id = id.saturating_add(1);
        let name = format!("{} {id}", geometry.display_name());
        self.annotations.push(Annotation {
            id,
            name,
            geometry,
            style,
            scope,
            visible: true,
            locked: false,
        });
        id
    }
}

pub struct TitleContext<'a> {
    pub variable: &'a str,
    pub source: &'a str,
    pub unit: Option<&'a str>,
    pub section: Option<&'a str>,
    pub time: Option<u64>,
    pub dump: Option<u64>,
    pub zone: &'a str,
    pub file: &'a str,
    pub run: &'a str,
    pub dataset_title: &'a str,
}

pub fn render_title(config: &TitleConfig, context: &TitleContext<'_>) -> Result<String, String> {
    if let Some(override_text) = config.override_text.as_deref()
        && !override_text.trim().is_empty()
    {
        return Ok(override_text.to_owned());
    }
    let mut output = String::new();
    let mut rest = config.template.as_str();
    while let Some(open) = rest.find('{') {
        output.push_str(&rest[..open]);
        let after_open = &rest[open + 1..];
        let Some(close) = after_open.find('}') else {
            return Err("title contains an unmatched '{'".to_owned());
        };
        let token = &after_open[..close];
        let replacement = match token {
            "variable" => context.variable.to_owned(),
            "source" => context.source.to_owned(),
            "unit" => context.unit.unwrap_or("").to_owned(),
            "section" => context.section.unwrap_or("").to_owned(),
            "time" => context
                .time
                .map(|value| value.to_string())
                .unwrap_or_default(),
            "dump" => context
                .dump
                .map(|value| value.to_string())
                .unwrap_or_default(),
            "zone" => context.zone.to_owned(),
            "file" => context.file.to_owned(),
            "run" => context.run.to_owned(),
            "dataset_title" => context.dataset_title.to_owned(),
            _ => return Err(format!("unknown title token {{{token}}}")),
        };
        output.push_str(&replacement);
        rest = &after_open[close + 1..];
    }
    output.push_str(rest);
    Ok(output)
}

#[derive(Clone, Debug, PartialEq)]
pub struct RenderedTick {
    pub value: f64,
    pub label: String,
}

pub fn colorbar_ticks(
    config: &ColorbarTickConfig,
    limits: [f32; 2],
    scale: Scale,
) -> Vec<RenderedTick> {
    let low = limits[0] as f64;
    let high = limits[1] as f64;
    if !low.is_finite() || !high.is_finite() || high <= low {
        return Vec::new();
    }
    match &config.mode {
        TickMode::Automatic { count } => {
            let values = match scale {
                Scale::Linear => automatic_linear_ticks(low, high, (*count).clamp(2, 12)),
                Scale::Logarithmic => automatic_log_ticks(low, high, (*count).clamp(2, 12)),
            };
            values
                .into_iter()
                .map(|value| RenderedTick {
                    value,
                    label: format_tick(value, config.format),
                })
                .collect()
        }
        TickMode::Custom { ticks } => {
            let errors = validate_custom_ticks(ticks, limits, scale);
            ticks
                .iter()
                .zip(errors)
                .filter_map(|(tick, error)| {
                    error.is_none().then(|| RenderedTick {
                        value: tick.value,
                        label: tick
                            .label
                            .as_deref()
                            .filter(|label| !label.is_empty())
                            .map(str::to_owned)
                            .unwrap_or_else(|| format_tick(tick.value, config.format)),
                    })
                })
                .collect()
        }
    }
}

pub fn validate_custom_ticks(
    ticks: &[ColorbarTick],
    limits: [f32; 2],
    scale: Scale,
) -> Vec<Option<String>> {
    let low = limits[0] as f64;
    let high = limits[1] as f64;
    let mut seen = HashSet::new();
    ticks
        .iter()
        .map(|tick| {
            if !tick.value.is_finite() {
                return Some("Value must be finite".to_owned());
            }
            if scale == Scale::Logarithmic && tick.value <= 0.0 {
                return Some("Logarithmic ticks must be positive".to_owned());
            }
            if tick.value < low || tick.value > high {
                return Some("Outside the color limits".to_owned());
            }
            let duplicate_key = if tick.value == 0.0 {
                0.0_f64.to_bits()
            } else {
                tick.value.to_bits()
            };
            if !seen.insert(duplicate_key) {
                return Some("Duplicate tick value".to_owned());
            }
            None
        })
        .collect()
}

pub fn format_tick(value: f64, format: NumberFormat) -> String {
    match format {
        NumberFormat::Automatic => {
            if value.abs() >= 10_000.0 || (value != 0.0 && value.abs() < 0.001) {
                format!("{value:.2e}")
            } else {
                let text = format!("{value:.4}");
                text.trim_end_matches('0').trim_end_matches('.').to_owned()
            }
        }
        NumberFormat::Fixed(precision) => {
            format!("{value:.precision$}", precision = precision.min(9) as usize)
        }
        NumberFormat::Scientific(precision) => {
            format!(
                "{value:.precision$e}",
                precision = precision.min(9) as usize
            )
        }
    }
}

pub fn normalized_value(value: f64, limits: [f32; 2], scale: Scale) -> Option<f32> {
    let mut value = value;
    let mut low = limits[0] as f64;
    let mut high = limits[1] as f64;
    if scale == Scale::Logarithmic {
        if value <= 0.0 || low <= 0.0 || high <= 0.0 {
            return None;
        }
        value = value.log10();
        low = low.log10();
        high = high.log10();
    }
    (value.is_finite() && low.is_finite() && high.is_finite() && high > low)
        .then_some(((value - low) / (high - low)).clamp(0.0, 1.0) as f32)
}

fn automatic_linear_ticks(low: f64, high: f64, count: u8) -> Vec<f64> {
    let raw = (high - low) / (count.saturating_sub(1).max(1) as f64);
    let magnitude = 10_f64.powf(raw.abs().log10().floor());
    let normalized = raw / magnitude;
    let nice = if normalized <= 1.0 {
        1.0
    } else if normalized <= 2.0 {
        2.0
    } else if normalized <= 2.5 {
        2.5
    } else if normalized <= 5.0 {
        5.0
    } else {
        10.0
    };
    let step = nice * magnitude;
    let mut value = (low / step).ceil() * step;
    let mut values = Vec::new();
    while value <= high + step * 1.0e-9 && values.len() < 100 {
        values.push(if value.abs() < step * 1.0e-12 {
            0.0
        } else {
            value
        });
        value += step;
    }
    if values.len() < 2 {
        vec![low, high]
    } else {
        values
    }
}

fn automatic_log_ticks(low: f64, high: f64, count: u8) -> Vec<f64> {
    if low <= 0.0 || high <= low {
        return Vec::new();
    }
    let first = low.log10().ceil() as i32;
    let last = high.log10().floor() as i32;
    let values: Vec<f64> = (first..=last)
        .map(|power| 10_f64.powi(power))
        .filter(|value| *value >= low && *value <= high)
        .collect();
    if values.len() > count as usize {
        let last = values.len() - 1;
        (0..count as usize)
            .map(|index| {
                let source = index * last / (count as usize - 1);
                values[source]
            })
            .collect()
    } else if values.len() >= 2 {
        values
    } else {
        let low_log = low.log10();
        let high_log = high.log10();
        (0..count)
            .map(|index| {
                let fraction = index as f64 / count.saturating_sub(1).max(1) as f64;
                10_f64.powf(low_log + fraction * (high_log - low_log))
            })
            .collect()
    }
}

fn interpolate_stops(stops: &[[u8; 3]], value: f32) -> RgbaColor {
    let scaled = value * (stops.len() - 1) as f32;
    let low = scaled.floor() as usize;
    let high = (low + 1).min(stops.len() - 1);
    let fraction = scaled - low as f32;
    let channel = |index| {
        (stops[low][index] as f32 * (1.0 - fraction) + stops[high][index] as f32 * fraction).round()
            as u8
    };
    RgbaColor([channel(0), channel(1), channel(2), 255])
}

fn turbo(x: f32) -> RgbaColor {
    let channel = |coefficients: [f32; 6]| {
        let result = coefficients
            .iter()
            .rev()
            .fold(0.0, |sum, &coefficient| sum * x + coefficient);
        (result.clamp(0.0, 1.0) * 255.0).round() as u8
    };
    RgbaColor([
        channel([
            0.13572138, 4.6153926, -42.660324, 132.13109, -152.9424, 59.28638,
        ]),
        channel([
            0.09140261, 2.1941884, 4.8429666, -14.185033, 4.2772985, 2.829566,
        ]),
        channel([
            0.1066733, 12.641946, -60.582047, 110.36277, -89.90311, 27.34825,
        ]),
        255,
    ])
}

const VIRIDIS: [[u8; 3]; 9] = [
    [68, 1, 84],
    [71, 44, 122],
    [59, 82, 139],
    [44, 113, 142],
    [33, 145, 140],
    [39, 173, 129],
    [92, 200, 99],
    [170, 220, 50],
    [253, 231, 37],
];
const PLASMA: [[u8; 3]; 9] = [
    [13, 8, 135],
    [75, 3, 161],
    [125, 3, 168],
    [168, 34, 150],
    [203, 70, 121],
    [229, 107, 93],
    [248, 148, 65],
    [253, 195, 40],
    [240, 249, 33],
];
const INFERNO: [[u8; 3]; 9] = [
    [0, 0, 4],
    [31, 12, 72],
    [85, 15, 109],
    [136, 34, 106],
    [186, 54, 85],
    [227, 89, 51],
    [249, 140, 10],
    [249, 201, 50],
    [252, 255, 164],
];
const MAGMA: [[u8; 3]; 9] = [
    [0, 0, 4],
    [28, 16, 68],
    [79, 18, 123],
    [129, 37, 129],
    [181, 54, 122],
    [229, 80, 100],
    [251, 135, 97],
    [254, 194, 135],
    [252, 253, 191],
];
const CIVIDIS: [[u8; 3]; 9] = [
    [0, 34, 78],
    [40, 52, 110],
    [67, 72, 110],
    [88, 91, 110],
    [111, 112, 115],
    [135, 134, 120],
    [162, 158, 116],
    [196, 184, 101],
    [254, 232, 56],
];
const GRAY: [[u8; 3]; 2] = [[0, 0, 0], [255, 255, 255]];
const COOLWARM: [[u8; 3]; 9] = [
    [59, 76, 192],
    [93, 124, 230],
    [141, 176, 254],
    [192, 212, 245],
    [221, 221, 221],
    [244, 196, 173],
    [238, 133, 105],
    [214, 82, 67],
    [180, 4, 38],
];
const RDBU: [[u8; 3]; 9] = [
    [103, 0, 31],
    [178, 24, 43],
    [214, 96, 77],
    [244, 165, 130],
    [247, 247, 247],
    [146, 197, 222],
    [67, 147, 195],
    [33, 102, 172],
    [5, 48, 97],
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_ellipse_scene_defaults_to_unlocked_aspect() {
        let geometry: AnnotationGeometry = serde_json::from_str(
            r#"{"kind":"ellipse","start":{"x":-1.0,"y":-2.0},"end":{"x":1.0,"y":2.0}}"#,
        )
        .unwrap();
        assert!(matches!(
            geometry,
            AnnotationGeometry::Ellipse {
                lock_aspect: false,
                ..
            }
        ));
    }

    #[test]
    fn colormaps_have_stable_endpoints_and_texture_size() {
        assert_eq!(Colormap::Viridis.sample(0.0).0, [68, 1, 84, 255]);
        assert_eq!(Colormap::Viridis.sample(1.0).0, [253, 231, 37, 255]);
        assert_eq!(
            Colormap::lookup_texture().len(),
            Colormap::ALL.len() * 256 * 4
        );
    }

    #[test]
    fn custom_ticks_are_validated_and_formatted() {
        let ticks = vec![
            ColorbarTick {
                value: 1.0,
                label: None,
            },
            ColorbarTick {
                value: 1.0,
                label: None,
            },
            ColorbarTick {
                value: -2.0,
                label: None,
            },
            ColorbarTick {
                value: 20.0,
                label: None,
            },
        ];
        let errors = validate_custom_ticks(&ticks, [0.1, 10.0], Scale::Logarithmic);
        assert!(errors[0].is_none());
        assert!(errors[1].as_deref().unwrap().contains("Duplicate"));
        assert!(errors[2].as_deref().unwrap().contains("positive"));
        assert!(errors[3].as_deref().unwrap().contains("Outside"));
        assert_eq!(format_tick(0.00001, NumberFormat::Automatic), "1.00e-5");
        assert_eq!(format_tick(1.25, NumberFormat::Fixed(2)), "1.25");

        let signed_zeroes = vec![
            ColorbarTick {
                value: 0.0,
                label: None,
            },
            ColorbarTick {
                value: -0.0,
                label: None,
            },
        ];
        let errors = validate_custom_ticks(&signed_zeroes, [-1.0, 1.0], Scale::Linear);
        assert!(errors[1].as_deref().unwrap().contains("Duplicate"));
    }

    #[test]
    fn automatic_log_ticks_respect_the_requested_count() {
        let config = ColorbarTickConfig {
            mode: TickMode::Automatic { count: 5 },
            format: NumberFormat::Automatic,
        };
        let ticks = colorbar_ticks(&config, [1.0e-8, 1.0e8], Scale::Logarithmic);
        assert_eq!(ticks.len(), 5);
        assert_eq!(ticks.first().unwrap().value, 1.0e-8);
        assert_eq!(ticks.last().unwrap().value, 1.0e8);
    }

    #[test]
    fn title_templates_expand_and_reject_unknown_tokens() {
        let context = TitleContext {
            variable: "density",
            source: "Rho",
            unit: Some("amu/cm3"),
            section: Some("z=0"),
            time: Some(10),
            dump: Some(2),
            zone: "cut",
            file: "plot.plt",
            run: "example",
            dataset_title: "BATS-R-US",
        };
        let config = TitleConfig {
            template: "{variable} · {section} · t={time}".into(),
            override_text: None,
        };
        assert_eq!(
            render_title(&config, &context).unwrap(),
            "density · z=0 · t=10"
        );
        let bad = TitleConfig {
            template: "{missing}".into(),
            override_text: None,
        };
        assert!(render_title(&bad, &context).is_err());
    }

    #[test]
    fn scene_round_trip_preserves_annotations_and_overrides() {
        let mut scene = SceneDocument::default();
        scene.set_variable_override("density", true);
        scene.set_streamlines_for(
            Some("z=0"),
            StreamlineSettings {
                enabled: true,
                horizontal_component: Some("magnetic_field.x".into()),
                vertical_component: Some("magnetic_field.y".into()),
                seeds: vec![DataPoint::new(1.0, 2.0)],
                ..StreamlineSettings::default()
            },
        );
        scene.add_annotation(
            AnnotationGeometry::Line {
                start: DataPoint::new(0.0, 1.0),
                end: DataPoint::new(2.0, 3.0),
            },
            AnnotationStyle::default(),
            AnnotationScope::Variable {
                variable: "density".into(),
            },
        );
        let encoded = serde_json::to_string(&scene).unwrap();
        let decoded: SceneDocument = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded, scene);
        assert!(decoded.validate().is_ok());
    }

    #[test]
    fn legacy_scene_defaults_to_no_field_lines() {
        let decoded: SceneDocument = serde_json::from_str(
            r#"{"version":1,"run_defaults":{},"variable_overrides":{},"annotations":[],"next_annotation_id":1}"#,
        )
        .unwrap();
        assert!(decoded.streamlines.is_empty());
        assert!(!decoded.streamlines_for(Some("z=0")).enabled);
    }

    #[test]
    fn scopes_match_current_plot() {
        let context = ScopeContext {
            section: Some("z=0"),
            variable: Some("density"),
            relative_path: Some("plots/a.plt"),
        };
        assert!(AnnotationScope::Run.matches(&context));
        assert!(
            AnnotationScope::Section {
                section: "z=0".into()
            }
            .matches(&context)
        );
        assert!(
            !AnnotationScope::Variable {
                variable: "pressure".into()
            }
            .matches(&context)
        );
    }
}
