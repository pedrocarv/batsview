use eframe::egui::epaint::EllipseShape;
use eframe::egui::{self, Color32, FontId, Pos2, Rect, Response, Shape, Stroke, StrokeKind};

use crate::scene::{
    Annotation, AnnotationGeometry, AnnotationScope, AnnotationStyle, DashStyle, DataPoint,
    SceneDocument, ScopeContext,
};

const HANDLE_RADIUS: f32 = 5.0;
const HIT_SLOP: f32 = 7.0;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DrawingTool {
    #[default]
    Select,
    Line,
    Arrow,
    Rectangle,
    Ellipse,
    Polyline,
    Polygon,
    Text,
}

impl DrawingTool {
    pub const ALL: [Self; 8] = [
        Self::Select,
        Self::Line,
        Self::Arrow,
        Self::Rectangle,
        Self::Ellipse,
        Self::Polyline,
        Self::Polygon,
        Self::Text,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Self::Select => "Select",
            Self::Line => "Line",
            Self::Arrow => "Arrow",
            Self::Rectangle => "Rectangle",
            Self::Ellipse => "Ellipse",
            Self::Polyline => "Polyline",
            Self::Polygon => "Polygon",
            Self::Text => "Text",
        }
    }

    fn is_two_point(self) -> bool {
        matches!(
            self,
            Self::Line | Self::Arrow | Self::Rectangle | Self::Ellipse
        )
    }
}

#[derive(Clone, Debug)]
enum Draft {
    TwoPoint {
        start: DataPoint,
        current: DataPoint,
        lock_aspect: bool,
    },
    MultiPoint {
        points: Vec<DataPoint>,
        hover: DataPoint,
    },
}

#[derive(Clone, Copy, Debug)]
enum DragTarget {
    Point { id: u64, index: usize },
    Center { id: u64 },
    RectangleCorner { id: u64, corner: u8 },
    EllipseRadius { id: u64, axis: u8 },
    Whole { id: u64 },
}

#[derive(Default)]
pub struct AnnotationEditor {
    pub tool: DrawingTool,
    pub selected: Option<u64>,
    draft: Option<Draft>,
    dragging: Option<(DragTarget, DataPoint)>,
    undo: Vec<SceneDocument>,
    redo: Vec<SceneDocument>,
}

impl AnnotationEditor {
    pub fn checkpoint(&mut self, scene: &SceneDocument) {
        if self.undo.last() != Some(scene) {
            self.undo.push(scene.clone());
            if self.undo.len() > 100 {
                self.undo.remove(0);
            }
        }
        self.redo.clear();
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    pub fn undo(&mut self, scene: &mut SceneDocument) {
        if let Some(previous) = self.undo.pop() {
            self.redo.push(scene.clone());
            *scene = previous;
            if self
                .selected
                .is_some_and(|id| !scene.annotations.iter().any(|item| item.id == id))
            {
                self.selected = None;
            }
        }
    }

    pub fn redo(&mut self, scene: &mut SceneDocument) {
        if let Some(next) = self.redo.pop() {
            self.undo.push(scene.clone());
            *scene = next;
            if self
                .selected
                .is_some_and(|id| !scene.annotations.iter().any(|item| item.id == id))
            {
                self.selected = None;
            }
        }
    }

    pub fn cancel_drawing(&mut self) {
        self.draft = None;
        self.dragging = None;
    }

    pub fn delete_selected(&mut self, scene: &mut SceneDocument) {
        let Some(id) = self.selected else { return };
        if let Some(index) = scene
            .annotations
            .iter()
            .position(|annotation| annotation.id == id)
        {
            self.checkpoint(scene);
            scene.annotations.remove(index);
            self.selected = None;
        }
    }

    pub fn duplicate_selected(&mut self, scene: &mut SceneDocument) {
        let Some(id) = self.selected else { return };
        let Some(annotation) = scene.annotations.iter().find(|item| item.id == id).cloned() else {
            return;
        };
        self.checkpoint(scene);
        let mut duplicate = annotation;
        duplicate.id = scene.next_annotation_id.max(1);
        scene.next_annotation_id = duplicate.id.saturating_add(1);
        duplicate.name = format!("{} copy", duplicate.name);
        translate_geometry(&mut duplicate.geometry, 0.02, -0.02);
        self.selected = Some(duplicate.id);
        scene.annotations.push(duplicate);
    }

    pub fn bring_forward(&mut self, scene: &mut SceneDocument) {
        let Some(id) = self.selected else { return };
        let Some(index) = scene.annotations.iter().position(|item| item.id == id) else {
            return;
        };
        if index + 1 < scene.annotations.len() {
            self.checkpoint(scene);
            scene.annotations.swap(index, index + 1);
        }
    }

    pub fn send_backward(&mut self, scene: &mut SceneDocument) {
        let Some(id) = self.selected else { return };
        let Some(index) = scene.annotations.iter().position(|item| item.id == id) else {
            return;
        };
        if index > 0 {
            self.checkpoint(scene);
            scene.annotations.swap(index, index - 1);
        }
    }

    pub fn interact(
        &mut self,
        ui: &egui::Ui,
        response: &Response,
        plot_rect: Rect,
        bounds: [f32; 4],
        scene: &mut SceneDocument,
        scope: &ScopeContext<'_>,
    ) -> bool {
        let escape = ui.input(|input| input.key_pressed(egui::Key::Escape));
        if escape {
            self.cancel_drawing();
        }
        let enter = ui.input(|input| input.key_pressed(egui::Key::Enter));
        let shift = ui.input(|input| input.modifiers.shift);
        let pointer = response
            .interact_pointer_pos()
            .or_else(|| ui.input(|input| input.pointer.hover_pos()));

        if matches!(self.tool, DrawingTool::Polyline | DrawingTool::Polygon) {
            if let (Some(Draft::MultiPoint { hover, .. }), Some(pointer)) =
                (&mut self.draft, pointer)
            {
                *hover = screen_to_data(pointer, plot_rect, bounds);
            }
            if enter || response.double_clicked() {
                self.finish_multi(scene);
            } else if response.clicked()
                && let Some(pointer) = pointer
            {
                let point = screen_to_data(pointer, plot_rect, bounds);
                match &mut self.draft {
                    Some(Draft::MultiPoint { points, hover }) => {
                        points.push(point);
                        *hover = point;
                    }
                    _ => {
                        self.draft = Some(Draft::MultiPoint {
                            points: vec![point],
                            hover: point,
                        });
                    }
                }
            }
            return true;
        }

        if self.tool == DrawingTool::Text {
            if response.clicked()
                && let Some(pointer) = pointer
            {
                self.checkpoint(scene);
                let id = scene.add_annotation(
                    AnnotationGeometry::Text {
                        position: screen_to_data(pointer, plot_rect, bounds),
                        text: "Text".to_owned(),
                    },
                    AnnotationStyle::default(),
                    AnnotationScope::Run,
                );
                self.selected = Some(id);
                self.tool = DrawingTool::Select;
            }
            return true;
        }

        if self.tool.is_two_point() {
            if response.drag_started()
                && let Some(pointer) = pointer
            {
                let start = screen_to_data(pointer, plot_rect, bounds);
                self.draft = Some(Draft::TwoPoint {
                    start,
                    current: start,
                    lock_aspect: shift,
                });
            }
            if response.dragged()
                && let (
                    Some(pointer),
                    Some(Draft::TwoPoint {
                        start,
                        current,
                        lock_aspect,
                    }),
                ) = (pointer, &mut self.draft)
            {
                let start_screen = data_to_screen(*start, plot_rect, bounds);
                let constrained = if shift {
                    constrain_pointer(self.tool, start_screen, pointer)
                } else {
                    pointer
                };
                *current = screen_to_data(constrained, plot_rect, bounds);
                *lock_aspect = shift;
            }
            if response.drag_stopped() {
                self.finish_two_point(scene);
            }
            return true;
        }

        if response.drag_started()
            && let Some(pointer) = pointer
        {
            if let Some(target) = self.hit_handle(scene, plot_rect, bounds, scope, pointer) {
                let id = target.id();
                if scene
                    .annotations
                    .iter()
                    .find(|item| item.id == id)
                    .is_some_and(|item| !item.locked)
                {
                    self.checkpoint(scene);
                    self.selected = Some(id);
                    self.dragging = Some((target, screen_to_data(pointer, plot_rect, bounds)));
                }
            } else if let Some(id) = hit_annotation(scene, plot_rect, bounds, scope, pointer) {
                self.selected = Some(id);
                if scene
                    .annotations
                    .iter()
                    .find(|item| item.id == id)
                    .is_some_and(|item| !item.locked)
                {
                    self.checkpoint(scene);
                    self.dragging = Some((
                        DragTarget::Whole { id },
                        screen_to_data(pointer, plot_rect, bounds),
                    ));
                }
            }
        }

        if response.dragged()
            && let (Some(pointer), Some((target, previous))) = (pointer, &mut self.dragging)
        {
            let current = screen_to_data(pointer, plot_rect, bounds);
            if let Some(annotation) = scene
                .annotations
                .iter_mut()
                .find(|item| item.id == target.id())
            {
                match *target {
                    DragTarget::Point { index, .. } => {
                        if let Some(point) = annotation.geometry.points_mut().into_iter().nth(index)
                        {
                            *point = current;
                        }
                    }
                    DragTarget::Center { .. } | DragTarget::Whole { .. } => {
                        annotation
                            .geometry
                            .translate(current.x - previous.x, current.y - previous.y);
                    }
                    DragTarget::RectangleCorner { corner, .. } => {
                        resize_rectangle(&mut annotation.geometry, corner, current);
                    }
                    DragTarget::EllipseRadius { axis, .. } => {
                        resize_ellipse(&mut annotation.geometry, axis, current);
                    }
                }
            }
            *previous = current;
        }
        if response.drag_stopped() {
            self.dragging = None;
        }
        if response.clicked()
            && let Some(pointer) = pointer
        {
            self.selected = hit_annotation(scene, plot_rect, bounds, scope, pointer);
        }
        self.dragging.is_some()
    }

    pub fn paint(
        &self,
        ui: &egui::Ui,
        plot_rect: Rect,
        bounds: [f32; 4],
        scene: &SceneDocument,
        scope: &ScopeContext<'_>,
        show_editor_adornments: bool,
    ) {
        let painter = ui.painter().with_clip_rect(plot_rect);
        for annotation in scene
            .annotations
            .iter()
            .filter(|item| item.visible && item.scope.matches(scope))
        {
            paint_annotation(&painter, annotation, plot_rect, bounds);
            if show_editor_adornments && self.selected == Some(annotation.id) {
                paint_handles(&painter, annotation, plot_rect, bounds);
            }
        }
        if show_editor_adornments && let Some(draft) = &self.draft {
            let style = AnnotationStyle::default();
            let geometry = match draft {
                Draft::TwoPoint {
                    start,
                    current,
                    lock_aspect,
                } => match self.tool {
                    DrawingTool::Line => AnnotationGeometry::Line {
                        start: *start,
                        end: *current,
                    },
                    DrawingTool::Arrow => AnnotationGeometry::Arrow {
                        start: *start,
                        end: *current,
                    },
                    DrawingTool::Rectangle => AnnotationGeometry::Rectangle {
                        start: *start,
                        end: *current,
                    },
                    DrawingTool::Ellipse => AnnotationGeometry::Ellipse {
                        start: *start,
                        end: *current,
                        lock_aspect: *lock_aspect,
                    },
                    _ => return,
                },
                Draft::MultiPoint { points, hover } => {
                    let mut points = points.clone();
                    points.push(*hover);
                    if self.tool == DrawingTool::Polygon {
                        AnnotationGeometry::Polygon { points }
                    } else {
                        AnnotationGeometry::Polyline { points }
                    }
                }
            };
            let draft = Annotation {
                id: 0,
                name: "Draft".into(),
                geometry,
                style,
                scope: AnnotationScope::Run,
                visible: true,
                locked: false,
            };
            paint_annotation(&painter, &draft, plot_rect, bounds);
        }
    }

    fn finish_two_point(&mut self, scene: &mut SceneDocument) {
        let Some(Draft::TwoPoint {
            start,
            current,
            lock_aspect,
        }) = self.draft.take()
        else {
            return;
        };
        if start == current {
            return;
        }
        let geometry = match self.tool {
            DrawingTool::Line => AnnotationGeometry::Line {
                start,
                end: current,
            },
            DrawingTool::Arrow => AnnotationGeometry::Arrow {
                start,
                end: current,
            },
            DrawingTool::Rectangle => AnnotationGeometry::Rectangle {
                start,
                end: current,
            },
            DrawingTool::Ellipse => AnnotationGeometry::Ellipse {
                start,
                end: current,
                lock_aspect,
            },
            _ => return,
        };
        self.checkpoint(scene);
        self.selected =
            Some(scene.add_annotation(geometry, AnnotationStyle::default(), AnnotationScope::Run));
    }

    fn finish_multi(&mut self, scene: &mut SceneDocument) {
        let Some(Draft::MultiPoint { points, .. }) = self.draft.take() else {
            return;
        };
        let minimum = if self.tool == DrawingTool::Polygon {
            3
        } else {
            2
        };
        if points.len() < minimum {
            return;
        }
        let geometry = if self.tool == DrawingTool::Polygon {
            AnnotationGeometry::Polygon { points }
        } else {
            AnnotationGeometry::Polyline { points }
        };
        self.checkpoint(scene);
        self.selected =
            Some(scene.add_annotation(geometry, AnnotationStyle::default(), AnnotationScope::Run));
    }

    fn hit_handle(
        &self,
        scene: &SceneDocument,
        rect: Rect,
        bounds: [f32; 4],
        scope: &ScopeContext<'_>,
        pointer: Pos2,
    ) -> Option<DragTarget> {
        let id = self.selected?;
        let annotation = scene
            .annotations
            .iter()
            .find(|item| item.id == id && item.visible && item.scope.matches(scope))?;
        annotation_handles(annotation)
            .into_iter()
            .find(|(_, point)| data_to_screen(*point, rect, bounds).distance(pointer) <= HIT_SLOP)
            .map(|(target, _)| target)
    }
}

impl DragTarget {
    fn id(self) -> u64 {
        match self {
            Self::Point { id, .. }
            | Self::Center { id }
            | Self::RectangleCorner { id, .. }
            | Self::EllipseRadius { id, .. }
            | Self::Whole { id } => id,
        }
    }
}

pub fn data_to_screen(point: DataPoint, rect: Rect, bounds: [f32; 4]) -> Pos2 {
    let x = (point.x as f32 - bounds[0]) / (bounds[1] - bounds[0]).max(f32::EPSILON);
    let y = (point.y as f32 - bounds[2]) / (bounds[3] - bounds[2]).max(f32::EPSILON);
    Pos2::new(
        rect.left() + x * rect.width(),
        rect.bottom() - y * rect.height(),
    )
}

pub fn screen_to_data(point: Pos2, rect: Rect, bounds: [f32; 4]) -> DataPoint {
    let x = ((point.x - rect.left()) / rect.width().max(1.0)).clamp(0.0, 1.0);
    let y = ((rect.bottom() - point.y) / rect.height().max(1.0)).clamp(0.0, 1.0);
    DataPoint::new(
        (bounds[0] + x * (bounds[1] - bounds[0])) as f64,
        (bounds[2] + y * (bounds[3] - bounds[2])) as f64,
    )
}

fn constrain_pointer(tool: DrawingTool, start: Pos2, current: Pos2) -> Pos2 {
    let delta = current - start;
    if matches!(tool, DrawingTool::Rectangle | DrawingTool::Ellipse) {
        let size = delta.x.abs().max(delta.y.abs());
        return start + egui::vec2(size.copysign(delta.x), size.copysign(delta.y));
    }
    let length = delta.length();
    if length <= f32::EPSILON {
        return current;
    }
    let angle = delta.angle();
    let snapped = (angle / std::f32::consts::FRAC_PI_4).round() * std::f32::consts::FRAC_PI_4;
    start + egui::Vec2::angled(snapped) * length
}

fn paint_annotation(
    painter: &egui::Painter,
    annotation: &Annotation,
    rect: Rect,
    bounds: [f32; 4],
) {
    let style = &annotation.style;
    let stroke = Stroke::new(style.stroke_width.clamp(0.5, 20.0), style.stroke.to_egui());
    match &annotation.geometry {
        AnnotationGeometry::Line { start, end } => {
            paint_path(
                painter,
                &[
                    data_to_screen(*start, rect, bounds),
                    data_to_screen(*end, rect, bounds),
                ],
                stroke,
                style.dash,
                false,
            );
        }
        AnnotationGeometry::Arrow { start, end } => {
            let start = data_to_screen(*start, rect, bounds);
            let end = data_to_screen(*end, rect, bounds);
            paint_path(painter, &[start, end], stroke, style.dash, false);
            let direction = (start - end).normalized();
            let normal = egui::vec2(-direction.y, direction.x);
            let size = style.arrowhead_size.clamp(4.0, 40.0);
            let left = end + direction * size + normal * size * 0.45;
            let right = end + direction * size - normal * size * 0.45;
            painter.add(Shape::convex_polygon(
                vec![end, left, right],
                style.stroke.to_egui(),
                Stroke::NONE,
            ));
        }
        AnnotationGeometry::Rectangle { start, end } => {
            let shape_rect = Rect::from_two_pos(
                data_to_screen(*start, rect, bounds),
                data_to_screen(*end, rect, bounds),
            );
            painter.rect_filled(
                shape_rect,
                0.0,
                style
                    .fill
                    .map_or(Color32::TRANSPARENT, |fill| fill.to_egui()),
            );
            if style.dash == DashStyle::Solid {
                painter.rect_stroke(shape_rect, 0.0, stroke, StrokeKind::Middle);
            } else {
                let points = vec![
                    shape_rect.left_top(),
                    shape_rect.right_top(),
                    shape_rect.right_bottom(),
                    shape_rect.left_bottom(),
                    shape_rect.left_top(),
                ];
                paint_path(painter, &points, stroke, style.dash, false);
            }
        }
        AnnotationGeometry::Ellipse { start, end, .. } => {
            let shape_rect = Rect::from_two_pos(
                data_to_screen(*start, rect, bounds),
                data_to_screen(*end, rect, bounds),
            );
            painter.add(EllipseShape {
                center: shape_rect.center(),
                radius: shape_rect.size() * 0.5,
                fill: style
                    .fill
                    .map_or(Color32::TRANSPARENT, |fill| fill.to_egui()),
                stroke: if style.dash == DashStyle::Solid {
                    stroke
                } else {
                    Stroke::NONE
                },
                angle: 0.0,
            });
            if style.dash != DashStyle::Solid {
                let points: Vec<_> = (0..64)
                    .map(|index| {
                        let angle = std::f32::consts::TAU * index as f32 / 64.0;
                        shape_rect.center()
                            + egui::vec2(
                                angle.cos() * shape_rect.width() * 0.5,
                                angle.sin() * shape_rect.height() * 0.5,
                            )
                    })
                    .collect();
                paint_path(painter, &points, stroke, style.dash, true);
            }
        }
        AnnotationGeometry::Polyline { points } => {
            let points: Vec<_> = points
                .iter()
                .map(|point| data_to_screen(*point, rect, bounds))
                .collect();
            paint_path(painter, &points, stroke, style.dash, false);
        }
        AnnotationGeometry::Polygon { points } => {
            let points: Vec<_> = points
                .iter()
                .map(|point| data_to_screen(*point, rect, bounds))
                .collect();
            if let Some(fill) = style.fill {
                painter.add(Shape::convex_polygon(
                    points.clone(),
                    fill.to_egui(),
                    Stroke::NONE,
                ));
            }
            paint_path(painter, &points, stroke, style.dash, true);
        }
        AnnotationGeometry::Text { position, text } => {
            painter.text(
                data_to_screen(*position, rect, bounds),
                egui::Align2::LEFT_BOTTOM,
                text,
                FontId::proportional(style.text_size.clamp(8.0, 72.0)),
                style.stroke.to_egui(),
            );
        }
    }
}

fn paint_path(
    painter: &egui::Painter,
    points: &[Pos2],
    stroke: Stroke,
    dash: DashStyle,
    closed: bool,
) {
    if points.len() < 2 {
        return;
    }
    let mut path = points.to_vec();
    if closed {
        path.push(points[0]);
    }
    match dash {
        DashStyle::Solid => {
            painter.add(Shape::line(path, stroke));
        }
        DashStyle::Dashed => {
            painter.extend(Shape::dashed_line(&path, stroke, 8.0, 5.0));
        }
        DashStyle::Dotted => {
            painter.extend(Shape::dotted_line(&path, stroke.color, 5.0, stroke.width));
        }
    }
}

fn paint_handles(painter: &egui::Painter, annotation: &Annotation, rect: Rect, bounds: [f32; 4]) {
    for (target, point) in annotation_handles(annotation) {
        let center = data_to_screen(point, rect, bounds);
        let fill = if matches!(target, DragTarget::Center { .. }) {
            Color32::from_rgb(92, 200, 255)
        } else {
            Color32::from_rgb(15, 22, 31)
        };
        painter.circle_filled(center, HANDLE_RADIUS, fill);
        painter.circle_stroke(
            center,
            HANDLE_RADIUS,
            Stroke::new(1.5, Color32::from_rgb(92, 200, 255)),
        );
    }
}

fn annotation_handles(annotation: &Annotation) -> Vec<(DragTarget, DataPoint)> {
    let id = annotation.id;
    match &annotation.geometry {
        AnnotationGeometry::Rectangle { .. } => {
            let Some((minimum, maximum)) = annotation.geometry.bounds() else {
                return Vec::new();
            };
            let center = annotation.geometry.center().unwrap_or(minimum);
            vec![
                (DragTarget::Center { id }, center),
                (DragTarget::RectangleCorner { id, corner: 0 }, minimum),
                (
                    DragTarget::RectangleCorner { id, corner: 1 },
                    DataPoint::new(maximum.x, minimum.y),
                ),
                (DragTarget::RectangleCorner { id, corner: 2 }, maximum),
                (
                    DragTarget::RectangleCorner { id, corner: 3 },
                    DataPoint::new(minimum.x, maximum.y),
                ),
            ]
        }
        AnnotationGeometry::Ellipse { start, end, .. } => {
            let center = DataPoint::new(0.5 * (start.x + end.x), 0.5 * (start.y + end.y));
            let radius_x = 0.5 * (end.x - start.x).abs();
            let radius_y = 0.5 * (end.y - start.y).abs();
            vec![
                (DragTarget::Center { id }, center),
                (
                    DragTarget::EllipseRadius { id, axis: 0 },
                    DataPoint::new(center.x + radius_x, center.y),
                ),
                (
                    DragTarget::EllipseRadius { id, axis: 0 },
                    DataPoint::new(center.x - radius_x, center.y),
                ),
                (
                    DragTarget::EllipseRadius { id, axis: 1 },
                    DataPoint::new(center.x, center.y + radius_y),
                ),
                (
                    DragTarget::EllipseRadius { id, axis: 1 },
                    DataPoint::new(center.x, center.y - radius_y),
                ),
            ]
        }
        _ => {
            let mut handles: Vec<_> = annotation
                .geometry
                .points()
                .into_iter()
                .enumerate()
                .map(|(index, point)| (DragTarget::Point { id, index }, point))
                .collect();
            if handles.len() > 1
                && let Some(center) = annotation.geometry.center()
            {
                handles.push((DragTarget::Center { id }, center));
            }
            handles
        }
    }
}

fn resize_rectangle(geometry: &mut AnnotationGeometry, corner: u8, point: DataPoint) {
    let AnnotationGeometry::Rectangle { start, end } = geometry else {
        return;
    };
    let minimum = DataPoint::new(start.x.min(end.x), start.y.min(end.y));
    let maximum = DataPoint::new(start.x.max(end.x), start.y.max(end.y));
    let opposite = match corner {
        0 => maximum,
        1 => DataPoint::new(minimum.x, maximum.y),
        2 => minimum,
        _ => DataPoint::new(maximum.x, minimum.y),
    };
    *start = opposite;
    *end = point;
}

fn resize_ellipse(geometry: &mut AnnotationGeometry, axis: u8, point: DataPoint) {
    let AnnotationGeometry::Ellipse {
        start,
        end,
        lock_aspect,
    } = geometry
    else {
        return;
    };
    let center = DataPoint::new(0.5 * (start.x + end.x), 0.5 * (start.y + end.y));
    let mut radius_x = 0.5 * (end.x - start.x).abs();
    let mut radius_y = 0.5 * (end.y - start.y).abs();
    if axis == 0 {
        radius_x = (point.x - center.x).abs();
        if *lock_aspect {
            radius_y = radius_x;
        }
    } else {
        radius_y = (point.y - center.y).abs();
        if *lock_aspect {
            radius_x = radius_y;
        }
    }
    *start = DataPoint::new(center.x - radius_x, center.y - radius_y);
    *end = DataPoint::new(center.x + radius_x, center.y + radius_y);
}

fn hit_annotation(
    scene: &SceneDocument,
    rect: Rect,
    bounds: [f32; 4],
    scope: &ScopeContext<'_>,
    pointer: Pos2,
) -> Option<u64> {
    scene
        .annotations
        .iter()
        .rev()
        .find(|annotation| {
            annotation.visible
                && annotation.scope.matches(scope)
                && annotation_screen_bounds(annotation, rect, bounds)
                    .expand(HIT_SLOP)
                    .contains(pointer)
        })
        .map(|annotation| annotation.id)
}

fn annotation_screen_bounds(annotation: &Annotation, rect: Rect, bounds: [f32; 4]) -> Rect {
    let points = annotation.geometry.points();
    let mut screen = points
        .iter()
        .map(|point| data_to_screen(*point, rect, bounds));
    let Some(first) = screen.next() else {
        return Rect::NOTHING;
    };
    screen.fold(Rect::from_min_max(first, first), |bounds, point| {
        bounds.union(Rect::from_min_max(point, point))
    })
}

fn translate_geometry(geometry: &mut AnnotationGeometry, dx: f64, dy: f64) {
    geometry.translate(dx, dy);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coordinate_round_trip_is_stable() {
        let rect = Rect::from_min_max(Pos2::new(10.0, 20.0), Pos2::new(410.0, 220.0));
        let bounds = [-10.0, 30.0, -5.0, 15.0];
        let point = DataPoint::new(4.0, 8.0);
        let restored = screen_to_data(data_to_screen(point, rect, bounds), rect, bounds);
        assert!((restored.x - point.x).abs() < 1.0e-5);
        assert!((restored.y - point.y).abs() < 1.0e-5);
    }

    #[test]
    fn undo_redo_restores_scene_edits() {
        let mut editor = AnnotationEditor::default();
        let mut scene = SceneDocument::default();
        editor.checkpoint(&scene);
        scene.add_annotation(
            AnnotationGeometry::Text {
                position: DataPoint::new(0.0, 0.0),
                text: "A".into(),
            },
            AnnotationStyle::default(),
            AnnotationScope::Run,
        );
        editor.undo(&mut scene);
        assert!(scene.annotations.is_empty());
        editor.redo(&mut scene);
        assert_eq!(scene.annotations.len(), 1);
    }

    #[test]
    fn circle_radius_handle_preserves_center_and_equal_radii() {
        let mut geometry = AnnotationGeometry::Ellipse {
            start: DataPoint::new(-1.0, -1.0),
            end: DataPoint::new(1.0, 1.0),
            lock_aspect: true,
        };
        resize_ellipse(&mut geometry, 0, DataPoint::new(3.0, 0.0));
        let AnnotationGeometry::Ellipse { start, end, .. } = geometry else {
            unreachable!();
        };
        assert_eq!(start, DataPoint::new(-3.0, -3.0));
        assert_eq!(end, DataPoint::new(3.0, 3.0));
    }

    #[test]
    fn rectangle_corner_handle_keeps_the_opposite_corner_fixed() {
        let mut geometry = AnnotationGeometry::Rectangle {
            start: DataPoint::new(-1.0, -2.0),
            end: DataPoint::new(1.0, 2.0),
        };
        resize_rectangle(&mut geometry, 0, DataPoint::new(-4.0, -5.0));
        let AnnotationGeometry::Rectangle { start, end } = geometry else {
            unreachable!();
        };
        assert_eq!(start, DataPoint::new(1.0, 2.0));
        assert_eq!(end, DataPoint::new(-4.0, -5.0));
    }
}
