use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Projection3d {
    #[default]
    Perspective,
    Orthographic,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Camera3d {
    pub target: [f32; 3],
    /// Unit vector from the target toward the camera.
    pub direction: [f32; 3],
    pub up: [f32; 3],
    pub distance: f32,
    pub projection: Projection3d,
    pub field_of_view_degrees: f32,
    pub orthographic_scale: f32,
}

impl Default for Camera3d {
    fn default() -> Self {
        Self {
            target: [0.0; 3],
            direction: normalize([1.0, -1.0, 0.8]),
            up: normalize([-0.35, 0.35, 0.88]),
            distance: 4.0,
            projection: Projection3d::Perspective,
            field_of_view_degrees: 38.0,
            orthographic_scale: 2.0,
        }
    }
}

impl Camera3d {
    pub fn fit(&mut self, bounds: [f32; 6]) {
        self.target = bounds_center(bounds);
        let radius = bounds_radius(bounds);
        let half_fov = 0.5 * self.field_of_view_degrees.clamp(10.0, 100.0).to_radians();
        self.distance = radius / half_fov.tan() * 1.15;
        self.orthographic_scale = radius * 1.15;
    }

    pub fn orbit(&mut self, delta_x: f32, delta_y: f32) {
        let yaw = -delta_x * 0.008;
        let pitch = -delta_y * 0.008;
        self.direction = normalize(rotate(self.direction, self.up, yaw));
        let right = normalize(cross(self.up, self.direction));
        self.direction = normalize(rotate(self.direction, right, pitch));
        self.up = normalize(rotate(self.up, right, pitch));
        self.up = normalize(sub(
            self.up,
            mul(self.direction, dot(self.up, self.direction)),
        ));
    }

    pub fn pan(&mut self, delta_x: f32, delta_y: f32) {
        let right = normalize(cross(self.up, self.direction));
        let scale = self.distance.max(1.0e-6) * 0.0015;
        self.target = add(
            self.target,
            add(mul(right, -delta_x * scale), mul(self.up, delta_y * scale)),
        );
    }

    pub fn zoom(&mut self, scroll_delta: f32, bounds: [f32; 6]) {
        self.zoom_by_factor((-scroll_delta * 0.0015).exp(), bounds);
    }

    pub fn zoom_by_factor(&mut self, factor: f32, bounds: [f32; 6]) {
        let radius = bounds_radius(bounds);
        let factor = factor.clamp(0.05, 20.0);
        self.distance = (self.distance * factor).clamp(radius * 0.05, radius * 100.0);
        self.orthographic_scale =
            (self.orthographic_scale * factor).clamp(radius * 0.01, radius * 100.0);
    }

    pub fn is_usable_for(&self, bounds: [f32; 6]) -> bool {
        let radius = bounds_radius(bounds);
        let unfitted_placeholder = *self == Self::default() && radius > self.distance * 0.8;
        !unfitted_placeholder
            && self.target.into_iter().all(f32::is_finite)
            && self.direction.into_iter().all(f32::is_finite)
            && self.up.into_iter().all(f32::is_finite)
            && self.distance.is_finite()
            && self.orthographic_scale.is_finite()
            && self.field_of_view_degrees.is_finite()
            && self.distance >= radius * 0.05
            && self.distance <= radius * 100.0
            && self.orthographic_scale >= radius * 0.01
            && self.orthographic_scale <= radius * 100.0
    }

    pub fn preset_isometric(&mut self) {
        self.direction = normalize([1.0, -1.0, 0.8]);
        self.up = normalize([-0.35, 0.35, 0.88]);
    }

    pub fn preset_x(&mut self) {
        self.direction = [1.0, 0.0, 0.0];
        self.up = [0.0, 0.0, 1.0];
    }

    pub fn preset_y(&mut self) {
        self.direction = [0.0, 1.0, 0.0];
        self.up = [0.0, 0.0, 1.0];
    }

    pub fn preset_z(&mut self) {
        self.direction = [0.0, 0.0, 1.0];
        self.up = [0.0, 1.0, 0.0];
    }

    pub fn view_projection(&self, bounds: [f32; 6], aspect: f32) -> [f32; 16] {
        let radius = bounds_radius(bounds);
        let eye = add(self.target, mul(self.direction, self.distance));
        let view = look_at_rh(eye, self.target, self.up);
        let near = (self.distance - radius * 1.8)
            .max(radius * 0.001)
            .max(1.0e-5);
        let far = (self.distance + radius * 3.0).max(near * 2.0);
        let projection = match self.projection {
            Projection3d::Perspective => perspective_rh_zo(
                self.field_of_view_degrees.clamp(10.0, 100.0).to_radians(),
                aspect.max(1.0e-3),
                near,
                far,
            ),
            Projection3d::Orthographic => {
                let half_height = self.orthographic_scale.max(radius * 0.001);
                orthographic_rh_zo(
                    -half_height * aspect,
                    half_height * aspect,
                    -half_height,
                    half_height,
                    near,
                    far,
                )
            }
        };
        multiply(projection, view)
    }

    pub fn project(&self, point: [f32; 3], bounds: [f32; 6], aspect: f32) -> Option<[f32; 3]> {
        let matrix = self.view_projection(bounds, aspect);
        project_point(matrix, point)
    }
}

pub fn project_point(matrix: [f32; 16], point: [f32; 3]) -> Option<[f32; 3]> {
    let clip = transform(matrix, [point[0], point[1], point[2], 1.0]);
    (clip[3].abs() > 1.0e-7).then(|| [clip[0] / clip[3], clip[1] / clip[3], clip[2] / clip[3]])
}

pub fn bounds_center(bounds: [f32; 6]) -> [f32; 3] {
    [
        0.5 * (bounds[0] + bounds[1]),
        0.5 * (bounds[2] + bounds[3]),
        0.5 * (bounds[4] + bounds[5]),
    ]
}

pub fn bounds_radius(bounds: [f32; 6]) -> f32 {
    let span = [
        (bounds[1] - bounds[0]).abs(),
        (bounds[3] - bounds[2]).abs(),
        (bounds[5] - bounds[4]).abs(),
    ];
    (0.5 * (span[0] * span[0] + span[1] * span[1] + span[2] * span[2]).sqrt()).max(1.0e-6)
}

fn add(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}

fn sub(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn mul(value: [f32; 3], scale: f32) -> [f32; 3] {
    [value[0] * scale, value[1] * scale, value[2] * scale]
}

fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn normalize(value: [f32; 3]) -> [f32; 3] {
    let length = dot(value, value).sqrt().max(1.0e-20);
    mul(value, 1.0 / length)
}

fn rotate(value: [f32; 3], axis: [f32; 3], angle: f32) -> [f32; 3] {
    let axis = normalize(axis);
    let (sin, cos) = angle.sin_cos();
    add(
        add(mul(value, cos), mul(cross(axis, value), sin)),
        mul(axis, dot(axis, value) * (1.0 - cos)),
    )
}

fn look_at_rh(eye: [f32; 3], target: [f32; 3], up: [f32; 3]) -> [f32; 16] {
    let forward = normalize(sub(target, eye));
    let right = normalize(cross(forward, up));
    let up = cross(right, forward);
    [
        right[0],
        up[0],
        -forward[0],
        0.0,
        right[1],
        up[1],
        -forward[1],
        0.0,
        right[2],
        up[2],
        -forward[2],
        0.0,
        -dot(right, eye),
        -dot(up, eye),
        dot(forward, eye),
        1.0,
    ]
}

fn perspective_rh_zo(fov_y: f32, aspect: f32, near: f32, far: f32) -> [f32; 16] {
    let focal = 1.0 / (0.5 * fov_y).tan();
    [
        focal / aspect,
        0.0,
        0.0,
        0.0,
        0.0,
        focal,
        0.0,
        0.0,
        0.0,
        0.0,
        far / (near - far),
        -1.0,
        0.0,
        0.0,
        near * far / (near - far),
        0.0,
    ]
}

fn orthographic_rh_zo(
    left: f32,
    right: f32,
    bottom: f32,
    top: f32,
    near: f32,
    far: f32,
) -> [f32; 16] {
    [
        2.0 / (right - left),
        0.0,
        0.0,
        0.0,
        0.0,
        2.0 / (top - bottom),
        0.0,
        0.0,
        0.0,
        0.0,
        1.0 / (near - far),
        0.0,
        -(right + left) / (right - left),
        -(top + bottom) / (top - bottom),
        near / (near - far),
        1.0,
    ]
}

fn multiply(a: [f32; 16], b: [f32; 16]) -> [f32; 16] {
    let mut result = [0.0; 16];
    for column in 0..4 {
        for row in 0..4 {
            result[column * 4 + row] = (0..4)
                .map(|index| a[index * 4 + row] * b[column * 4 + index])
                .sum();
        }
    }
    result
}

fn transform(matrix: [f32; 16], value: [f32; 4]) -> [f32; 4] {
    let mut result = [0.0; 4];
    for row in 0..4 {
        result[row] = (0..4)
            .map(|column| matrix[column * 4 + row] * value[column])
            .sum();
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn orbit_remains_orthonormal() {
        let mut camera = Camera3d::default();
        for _ in 0..100 {
            camera.orbit(7.0, -4.0);
        }
        assert!((dot(camera.direction, camera.direction) - 1.0).abs() < 1.0e-4);
        assert!((dot(camera.up, camera.up) - 1.0).abs() < 1.0e-4);
        assert!(dot(camera.direction, camera.up).abs() < 1.0e-4);
    }

    #[test]
    fn fit_and_projection_are_finite() {
        let bounds = [-10.0, 20.0, -4.0, 5.0, -2.0, 8.0];
        let mut camera = Camera3d::default();
        camera.fit(bounds);
        assert_eq!(camera.target, [5.0, 0.5, 3.0]);
        assert!(
            camera
                .view_projection(bounds, 16.0 / 9.0)
                .into_iter()
                .all(f32::is_finite)
        );
        let projected_target = camera.project(camera.target, bounds, 16.0 / 9.0).unwrap();
        assert!(projected_target[0].abs() < 1.0e-5);
        assert!(projected_target[1].abs() < 1.0e-5);
    }

    #[test]
    fn fit_keeps_every_domain_corner_inside_the_view() {
        let bounds = [-224.0, 32.0, -128.0, 128.0, -128.0, 128.0];
        let mut camera = Camera3d::default();
        camera.fit(bounds);
        for x in [bounds[0], bounds[1]] {
            for y in [bounds[2], bounds[3]] {
                for z in [bounds[4], bounds[5]] {
                    let projected = camera.project([x, y, z], bounds, 1.0).unwrap();
                    assert!(projected[0].abs() <= 1.0);
                    assert!(projected[1].abs() <= 1.0);
                    assert!((0.0..=1.0).contains(&projected[2]));
                }
            }
        }
    }

    #[test]
    fn rejects_an_unfitted_placeholder_camera_for_a_large_domain() {
        let bounds = [-100.0, 100.0, -100.0, 100.0, -100.0, 100.0];
        assert!(!Camera3d::default().is_usable_for(bounds));
        let mut fitted = Camera3d::default();
        fitted.fit(bounds);
        assert!(fitted.is_usable_for(bounds));
    }
}
