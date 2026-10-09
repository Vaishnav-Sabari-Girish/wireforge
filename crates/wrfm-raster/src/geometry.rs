//! Model-space geometry: rotations, bounding box, extent and the auto-fit
//! camera distance — the camera-independent half of the shared math.

use crate::Model;

/// Vertical field of view in degrees — the wireforge fork's projection.
pub const FOV_DEG: f64 = 60.0;

/// Auto-fit headroom: the model fills 1/FIT_MARGIN of the screen height.
pub const FIT_MARGIN: f64 = 2.0;

/// A 3x3 row-major rotation matrix (model -> world).
pub type Mat3 = [[f64; 3]; 3];

/// Identity rotation (model axes aligned with the world axes).
pub const IDENTITY: Mat3 = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];

/// Multiply two 3x3 matrices.
pub fn mat_mul(a: Mat3, b: Mat3) -> Mat3 {
    let mut c = [[0.0; 3]; 3];
    for i in 0..3 {
        for j in 0..3 {
            c[i][j] = a[i][0] * b[0][j] + a[i][1] * b[1][j] + a[i][2] * b[2][j];
        }
    }
    c
}

/// Rotation around the world X axis (positive angle tips +Y toward +Z).
pub fn rot_x(a: f64) -> Mat3 {
    let (s, c) = a.sin_cos();
    [[1.0, 0.0, 0.0], [0.0, c, -s], [0.0, s, c]]
}

/// Rotation around the world Y axis (positive angle turns +Z toward +X).
pub fn rot_y(a: f64) -> Mat3 {
    let (s, c) = a.sin_cos();
    [[c, 0.0, s], [0.0, 1.0, 0.0], [-s, 0.0, c]]
}

/// Rotation around the world Z axis (positive angle turns +X toward +Y).
pub fn rot_z(a: f64) -> Mat3 {
    let (s, c) = a.sin_cos();
    [[c, -s, 0.0], [s, c, 0.0], [0.0, 0.0, 1.0]]
}

/// Rotation around an ARBITRARY axis by Rodrigues' formula:
/// `v' = v·cosθ + (k×v)·sinθ + k(k·v)(1−cosθ)` with `k` the normalized axis.
/// A zero-length axis leaves the model untouched.
pub fn rot_axis(axis: [f64; 3], angle_deg: f64) -> Mat3 {
    let norm = (axis[0] * axis[0] + axis[1] * axis[1] + axis[2] * axis[2]).sqrt();
    if norm < f64::EPSILON {
        return IDENTITY;
    }
    let (kx, ky, kz) = (axis[0] / norm, axis[1] / norm, axis[2] / norm);
    let (s, c) = angle_deg.to_radians().sin_cos();
    let t = 1.0 - c;
    [
        [c + kx * kx * t, kx * ky * t - kz * s, kx * kz * t + ky * s],
        [ky * kx * t + kz * s, c + ky * ky * t, ky * kz * t - kx * s],
        [kz * kx * t - ky * s, kz * ky * t + kx * s, c + kz * kz * t],
    ]
}

/// World-frame model rotation for absolute (pitch, yaw): the model is first
/// pitched around the world X axis, then yawed around the world vertical axis.
///
/// The yaw sign follows wireforge's global convention (post-unification):
/// positive yaw turns the object's nose to its own left, so `--yaw 90` shows
/// the object's right side.
pub fn world_rot(pitch_deg: f64, yaw_deg: f64) -> Mat3 {
    mat_mul(rot_y(yaw_deg.to_radians()), rot_x(pitch_deg.to_radians()))
}

/// Bounding box of the model: `(min, max)` corners.
///
/// An empty model yields `(inf, -inf)` — callers that need a "no geometry"
/// answer should check `vertices.is_empty()` first.
pub fn bounds(m: &Model) -> ([f64; 3], [f64; 3]) {
    let mut min = [f64::INFINITY; 3];
    let mut max = [f64::NEG_INFINITY; 3];
    for &(x, y, z) in &m.vertices {
        min[0] = min[0].min(x);
        min[1] = min[1].min(y);
        min[2] = min[2].min(z);
        max[0] = max[0].max(x);
        max[1] = max[1].max(y);
        max[2] = max[2].max(z);
    }
    (min, max)
}

/// The model's comprehensive length (the fork's `model_extent`): the
/// geometric mean of the three bounding-box dimensions, i.e. `(dx*dy*dz)^(1/3)`.
///
/// The geometric mean keeps an elongated model from being dominated by its
/// longest axis.
pub fn model_extent(m: &Model) -> f64 {
    extent_from_bounds(bounds(m))
}

/// [`model_extent`] from an already-computed bounding box — for callers
/// that keep their own (e.g. rayon-parallel) `bounds` pass.
pub fn extent_from_bounds(b: ([f64; 3], [f64; 3])) -> f64 {
    let (min, max) = b;
    let (dx, dy, dz) = (
        (max[0] - min[0]).max(1e-9),
        (max[1] - min[1]).max(1e-9),
        (max[2] - min[2]).max(1e-9),
    );
    (dx * dy * dz).cbrt()
}

/// Auto camera distance (the fork's `fit_to` math): the geometric-mean
/// extent fills 1/FIT_MARGIN of the screen half-height.
pub fn auto_dist(m: &Model) -> f64 {
    let r = model_extent(m).max(1e-6);
    r / (FOV_DEG / 2.0).to_radians().tan() * FIT_MARGIN
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tetra() -> Model {
        Model {
            vertices: vec![
                (1.0, 1.0, 1.0),
                (1.0, -1.0, -1.0),
                (-1.0, 1.0, -1.0),
                (-1.0, -1.0, 1.0),
            ],
            edges: vec![(0, 1), (0, 2), (0, 3), (1, 2), (2, 3), (3, 1)],
        }
    }

    #[test]
    fn bounds_of_tetrahedron() {
        let (min, max) = bounds(&tetra());
        assert_eq!(min, [-1.0, -1.0, -1.0]);
        assert_eq!(max, [1.0, 1.0, 1.0]);
    }

    #[test]
    fn auto_dist_matches_the_fork() {
        // A +-1 tetrahedron has extent 2; the fork's fit_to is
        // extent / tan(FOV/2) * FIT_MARGIN.
        let expected = 2.0 / (FOV_DEG / 2.0).to_radians().tan() * FIT_MARGIN;
        assert!(
            (auto_dist(&tetra()) - expected).abs() < 1e-9,
            "got {} expected {expected}",
            auto_dist(&tetra())
        );
        // An elongated model: the geomean is not dominated by the long axis.
        let long = Model {
            vertices: vec![(0.0, 0.0, 0.0), (6.0, 4.0, 160.0)],
            edges: vec![(0, 1)],
        };
        assert!(
            model_extent(&long) < 20.0,
            "geomean dominated by the long axis"
        );
    }

    #[test]
    fn world_rot_pitches_then_yaws() {
        // Pitch 90 points the model's +Y at the camera; yaw then swings it.
        let rot = world_rot(90.0, 0.0);
        let (x, y, z) = (rot[2][0], rot[2][1], rot[2][2]);
        assert!(
            (z - 0.0).abs() < 1e-12 && (y - 1.0).abs() < 1e-12,
            "pitch 90 should map +Y onto +Z, got row ({x},{y},{z})"
        );
        // A zero axis leaves the model untouched (Rodrigues guard).
        assert_eq!(rot_axis([0.0, 0.0, 0.0], 45.0), IDENTITY);
    }

    #[test]
    fn rot_axis_quarter_turn_about_z() {
        let r = rot_axis([0.0, 0.0, 1.0], 90.0);
        // +X -> +Y under a 90° right-handed turn about +Z.
        let (x, y) = (r[0][0], r[1][0]);
        assert!(
            (x - 0.0).abs() < 1e-12 && (y - 1.0).abs() < 1e-12,
            "({x},{y})"
        );
    }
}
