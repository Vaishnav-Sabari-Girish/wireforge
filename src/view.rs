use rayon::prelude::*;
use wrfm_raster::Model;
use wrfm_raster::geometry::{IDENTITY, auto_dist_from_extent, mat_mul, rot_x, rot_y, rot_z};
use wrfm_raster::projection::{Camera, CameraF32, focal};

/// Above this vertex count, projection and bounds switch to the rayon
/// parallel path (measured crossover ~72k; typical models stay serial).
const PARALLEL_THRESHOLD: usize = 100_000;

/// A 3x3 row-major rotation matrix (model -> world).
pub use wrfm_raster::geometry::Mat3;

/// The six camera degrees of freedom (world-frame yaw/pitch, roll around the view axis).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ViewState {
    /// Model -> world rotation matrix (yaw + pitch).
    pub rot: Mat3,
    /// Accumulated yaw angle (radians, display only).
    pub yaw: f64,
    /// Accumulated world-frame pitch (radians, display only).
    pub pitch: f64,
    /// Roll around the view axis (radians).
    pub roll: f64,
    /// Camera distance from the file origin (zoom).
    pub dist: f64,
    /// Aim point X offset (world units).
    pub pan_x: f64,
    /// Aim point Y offset (world units).
    pub pan_y: f64,
}

impl Default for ViewState {
    fn default() -> Self {
        Self {
            rot: IDENTITY,
            yaw: 0.0,
            pitch: 0.0,
            roll: 0.0,
            dist: 8.0,
            pan_x: 0.0,
            pan_y: 0.0,
        }
    }
}

/// Wrap an angle into [-PI, PI] (the projection is periodic).
pub fn normalize_angle(a: f64) -> f64 {
    use std::f64::consts::PI;
    let mut a = a % (2.0 * PI);
    if a > PI {
        a -= 2.0 * PI;
    } else if a < -PI {
        a += 2.0 * PI;
    }
    a
}

impl ViewState {
    /// Wrap yaw/pitch/roll into [-PI, PI] for display.
    pub fn normalize(&mut self) {
        self.yaw = normalize_angle(self.yaw);
        self.pitch = normalize_angle(self.pitch);
        self.roll = normalize_angle(self.roll);
    }

    /// Yaw the model around the world Y axis. One global direction convention:
    /// positive `d` yaws the model to its own LEFT (its nose turns toward its
    /// left side — `rot_y` positive is a right-handed turn about +Y, which
    /// carries the nose toward +X = the object's left when it faces +Z). The
    /// frame (world vs local) picks the AXIS, never the sense.
    pub fn add_yaw(&mut self, d: f64) {
        self.rot = mat_mul(rot_y(d), self.rot);
        self.yaw += d;
    }

    /// Pitch the model around the world X axis.
    pub fn add_pitch(&mut self, d: f64) {
        self.rot = mat_mul(rot_x(d), self.rot);
        self.pitch += d;
    }

    /// Yaw the model around its own (local) Y axis: `add_yaw` with the step
    /// post-multiplied instead of pre-multiplied, so the axis rides with the
    /// model instead of staying anchored to the world. The direction is the
    /// same convention as `add_yaw` (positive = the model's own left): when
    /// the two axes coincide the two keys produce the same rotation.
    pub fn add_yaw_local(&mut self, d: f64) {
        self.rot = mat_mul(self.rot, rot_y(d));
        self.yaw += d;
    }

    /// Pitch the model around its own (local) X axis: `add_pitch` with the
    /// step post-multiplied instead of pre-multiplied.
    pub fn add_pitch_local(&mut self, d: f64) {
        self.rot = mat_mul(self.rot, rot_x(d));
        self.pitch += d;
    }

    /// Roll the model around its own (local) Z axis: positive `d` banks the
    /// model to its own right (right-hand turn about the nose axis, so its
    /// starboard side dips). Plain `Motion::RollPlus` is the *viewer's* frame
    /// instead — it rolls about the sight line, which points into the screen
    /// while the nose points out of it, so at the default view the plain roll
    /// key (`d`) and `Ctrl+d` read as mirror images (plain `d` dips the
    /// viewer's right, `Ctrl+d` dips the model's starboard). Unlike yaw/pitch it does NOT
    /// touch the `roll` field: `roll` is the authoritative camera-frame
    /// angle applied by `project_point`, so ticking it here as well would
    /// rotate the image twice; the HUD therefore reports world-frame roll
    /// only.
    pub fn add_roll_local(&mut self, d: f64) {
        self.rot = mat_mul(self.rot, rot_z(d));
    }

    /// Spin the model around its own (local) Y axis (Space auto-spin).
    pub fn spin_local(&mut self, d: f64) {
        self.rot = mat_mul(self.rot, rot_y(d));
        self.yaw += d;
    }

    /// Add a signed distance delta (zoom), clamped to the allowed range.
    pub fn add_dist_delta(&mut self, delta: f64) {
        self.dist = (self.dist + delta).clamp(0.05, 100_000.0);
    }

    /// Auto-fit: set the distance so the model fills the view. An empty
    /// model has no extent to frame, so it is framed as the unit scene (see
    /// `extent_from_bounds`) — a finite distance, never the NaN an
    /// `(inf, -inf)` bounding box would produce.
    ///
    /// Convenience for tests, which hold a model but no cached extent.
    /// Production code caches the extent and calls
    /// [`ViewState::fit_to_extent`], so the O(n) bounds scan stays off the
    /// per-frame path.
    #[cfg(test)]
    pub fn fit_to(&mut self, m: &Model) {
        self.fit_to_extent(model_extent(m));
    }

    /// [`ViewState::fit_to`] from an already-computed [`model_extent`]: the
    /// distance is exactly what `fit_to` would set, without re-scanning every
    /// vertex for a bounding box that cannot have changed.
    pub fn fit_to_extent(&mut self, extent: f64) {
        self.dist = auto_dist_from_extent(extent);
    }

    /// Reset rotation/pan and re-fit the distance (see [`ViewState::fit_to`]).
    #[cfg(test)]
    pub fn reset(&mut self, m: &Model) {
        self.reset_with_extent(model_extent(m));
    }

    /// [`ViewState::reset`] from an already-computed [`model_extent`].
    pub fn reset_with_extent(&mut self, extent: f64) {
        self.rot = IDENTITY;
        self.yaw = 0.0;
        self.pitch = 0.0;
        self.roll = 0.0;
        self.pan_x = 0.0;
        self.pan_y = 0.0;
        self.fit_to_extent(extent);
    }
}

/// Bounding box of the model: `(min, max)` corners.
///
/// The serial path is `wrfm_raster::geometry::bounds`; this wrapper only
/// adds the rayon reduction above [`PARALLEL_THRESHOLD`] (bit-identical:
/// per-vertex min/max is order-independent).
pub fn bounds(m: &Model) -> ([f64; 3], [f64; 3]) {
    if m.vertices.len() >= PARALLEL_THRESHOLD {
        // rayon: per-vertex min/max is an independent reduction (large models).
        let init = || ([f64::INFINITY; 3], [f64::NEG_INFINITY; 3]);
        m.vertices
            .par_iter()
            .fold(init, |(mut mn, mut mx), &(x, y, z)| {
                mn[0] = mn[0].min(x);
                mn[1] = mn[1].min(y);
                mn[2] = mn[2].min(z);
                mx[0] = mx[0].max(x);
                mx[1] = mx[1].max(y);
                mx[2] = mx[2].max(z);
                (mn, mx)
            })
            .reduce(init, |(a1, a2), (b1, b2)| {
                (
                    [a1[0].min(b1[0]), a1[1].min(b1[1]), a1[2].min(b1[2])],
                    [a2[0].max(b2[0]), a2[1].max(b2[1]), a2[2].max(b2[2])],
                )
            })
    } else {
        wrfm_raster::geometry::bounds(m)
    }
}

/// The model's geometric-mean length (cbrt of the bounding-box dimensions).
///
/// Goes through this module's rayon-capable [`bounds`], so the large-model
/// path stays parallel; the extent math itself lives in wrfm-raster.
pub fn model_extent(m: &Model) -> f64 {
    wrfm_raster::geometry::extent_from_bounds(bounds(m))
}

/// The shared camera built from this view state. `Camera::new` precomputes
/// `roll.sin_cos()`, which is bit-identical to computing it per call.
fn camera(v: &ViewState) -> Camera {
    Camera::new(v.rot, v.dist, v.roll, v.pan_x, v.pan_y)
}

/// Project a model-space vertex to canvas coordinates; `None` when behind the camera.
pub fn project_point(p: (f64, f64, f64), v: &ViewState, px_h: usize) -> Option<(f64, f64)> {
    camera(v).project(p, focal(px_h as f64))
}

/// Batch-project all vertices (identical math to `project_point`).
///
/// Above [`PARALLEL_THRESHOLD`] the per-vertex work runs on rayon threads
/// through the shared `project_into`; the serial path is the shared
/// `project_all`. Both go through the same single-vertex implementation, so
/// the parallel and serial results stay bit-identical.
pub fn project_batch(
    verts: &[(f64, f64, f64)],
    v: &ViewState,
    px_h: usize,
    out: &mut [[f64; 2]],
    ok: &mut [bool],
) {
    let f = focal(px_h as f64);
    let cam = camera(v);
    if verts.len() >= PARALLEL_THRESHOLD {
        // rayon: vertex projections are independent of each other (the render vertex bottleneck).
        // Map each vertex to its result (pure function, no shared mutable state), then write back in order.
        verts
            .par_iter()
            .zip(out.par_iter_mut())
            .zip(ok.par_iter_mut())
            .for_each(|((p, o), ok_slot)| cam.project_into(*p, f, o, ok_slot));
        return;
    }
    cam.project_all(verts, f, out, ok);
}

/// f32 batch projection for very large models (> 4096 vertices): same
/// structure as [`project_batch`] with the camera and focal length cast
/// down to f32 first (the large-model path).
pub fn project_batch_f32(
    verts: &[(f64, f64, f64)],
    v: &ViewState,
    px_h: usize,
    out: &mut [[f32; 2]],
    ok: &mut [bool],
) {
    let f = CameraF32::focal(px_h as f64);
    let cam = CameraF32::new(&camera(v));
    if verts.len() >= PARALLEL_THRESHOLD {
        verts
            .par_iter()
            .zip(out.par_iter_mut())
            .zip(ok.par_iter_mut())
            .for_each(|((p, o), ok_slot)| cam.project_into(*p, f, o, ok_slot));
        return;
    }
    cam.project_all(verts, f, out, ok);
}

#[cfg(test)]
mod tests {
    use super::*;
    use wrfm_raster::geometry::{FIT_MARGIN, FOV_DEG};

    fn cube() -> Model {
        let h = 1.0;
        let mut verts = Vec::new();
        let mut edges = Vec::new();
        for i in 0..2 {
            for j in 0..2 {
                for k in 0..2 {
                    verts.push((
                        if i == 0 { -h } else { h },
                        if j == 0 { -h } else { h },
                        if k == 0 { -h } else { h },
                    ));
                }
            }
        }
        for i in 0..2 {
            for j in 0..2 {
                for k in 0..2 {
                    for (di, dj, dk) in [(1, 0, 0), (0, 1, 0), (0, 0, 1)] {
                        if i + di < 2 && j + dj < 2 && k + dk < 2 {
                            edges.push((i * 4 + j * 2 + k, (i + di) * 4 + (j + dj) * 2 + (k + dk)));
                        }
                    }
                }
            }
        }
        Model {
            vertices: verts,
            edges,
        }
    }

    fn view() -> ViewState {
        ViewState {
            dist: 8.0,
            ..Default::default()
        }
    }

    #[test]
    fn normalize_angle_wraps_periodically() {
        use std::f64::consts::PI;
        assert!((normalize_angle(0.0) - 0.0).abs() < 1e-9);
        assert!((normalize_angle(PI) - PI).abs() < 1e-9);
        assert!((normalize_angle(4.0) - (4.0 - 2.0 * PI)).abs() < 1e-9);
        // -20.44 rad keeps accumulating: wrapped it lands in [-PI, PI].
        let w = normalize_angle(-20.44);
        assert!((-PI..=PI).contains(&w), "wrapped value out of range: {w}");
        assert!((w + 1.59044).abs() < 1e-3, "expected ~-1.59, got {w}");
        // Wrapping is idempotent.
        assert!((normalize_angle(w) - w).abs() < 1e-12);
    }

    #[test]
    fn origin_projects_to_center() {
        let v = view();
        let (x, y) = project_point((0.0, 0.0, 0.0), &v, 100).unwrap();
        assert!((x.abs() < 1e-9) && (y.abs() < 1e-9));
    }

    #[test]
    fn up_is_up_right_is_right() {
        let v = view();
        let (_, y) = project_point((0.0, 1.0, 0.0), &v, 100).unwrap();
        assert!(y > 0.0, "world +Y should project up, got {y}");
        let (x, _) = project_point((1.0, 0.0, 0.0), &v, 100).unwrap();
        assert!(x > 0.0, "world +X should project right, got {x}");
    }

    #[test]
    fn yaw_turns_the_view() {
        // A point at +X, viewed after yaw=90deg, appears near the centre.
        let mut v = view();
        v.add_yaw(90.0f64.to_radians());
        let (x, y) = project_point((1.0, 0.0, 0.0), &v, 100).unwrap();
        assert!(x.abs() < 1.0, "yaw 90 should bring +X to front, x={x}");
        assert!(y.abs() < 1.0, "yaw 90 should keep it centered, y={y}");
    }

    #[test]
    fn pitch_has_no_limits() {
        let mut v = view();
        v.add_pitch(10_000.0);
        assert!(
            (v.pitch - 10_000.0).abs() < 1e-9,
            "pitch must not be clamped"
        );
        v.add_pitch(-30_000.0);
        assert!((v.pitch + 20_000.0).abs() < 1e-9);
    }

    #[test]
    fn pitch_at_pole_projects() {
        // Straight down (pitch=90): the top vertex lands at the centre and
        // the view is still well-defined.
        let mut v = view();
        v.add_pitch(90.0f64.to_radians());
        let (x, y) = project_point((0.0, 1.0, 0.0), &v, 100).expect("must project at the pole");
        assert!(
            x.abs() < 1.0 && y.abs() < 1.0,
            "top vertex at centre, got ({x},{y})"
        );
        let (rx, _) = project_point((1.0, 0.0, 0.0), &v, 100).unwrap();
        assert!(rx > 0.0, "right vertex stays right in top view, got {rx}");
    }

    #[test]
    fn pitch_beyond_pole_projects() {
        // Past the pole the camera flips over, but projection stays valid.
        let mut v = view();
        v.add_pitch(120.0f64.to_radians());
        assert!(project_point((0.0, 0.0, 0.0), &v, 100).is_some());
        v.add_pitch(-120.0f64.to_radians());
        assert!(project_point((0.0, 0.0, 0.0), &v, 100).is_some());
    }

    #[test]
    fn fit_distance_frames_cube() {
        let m = cube(); // geomean of (2,2,2) = 2 -> characteristic radius 2
        let mut v = view();
        v.fit_to(&m);
        let expected = 2.0 / (FOV_DEG / 2.0).to_radians().tan() * FIT_MARGIN;
        assert!(
            (v.dist - expected).abs() < 1e-9,
            "dist={} expected={expected}",
            v.dist
        );
        // A face vertex (half-extent = geomean/2) must be on-screen at the
        // fit distance.
        let (x, y) = project_point((1.0, 0.0, 0.0), &v, 100).unwrap();
        assert!(
            x.abs() < 100.0 && y.abs() < 50.0,
            "face vertex should be on-screen, got ({x},{y})"
        );
    }

    #[test]
    fn rotation_centre_is_file_origin() {
        // Rotation is about the file origin (0,0,0), not the box centre.
        let m = Model {
            vertices: vec![
                (9.0, 19.0, -1.0),
                (11.0, 19.0, -1.0),
                (11.0, 21.0, -1.0),
                (9.0, 21.0, -1.0),
                (9.0, 19.0, 1.0),
                (11.0, 19.0, 1.0),
                (11.0, 21.0, 1.0),
                (9.0, 21.0, 1.0),
            ],
            edges: vec![],
        };
        let mut v = view();
        v.fit_to(&m);
        // Origin (the rotation centre) projects to the view centre.
        let (x, y) = project_point((0.0, 0.0, 0.0), &v, 100).unwrap();
        assert!(
            x.abs() < 1e-6 && y.abs() < 1e-6,
            "file origin should project to the view centre, got ({x},{y})"
        );
        // The bbox centre is NOT the rotation centre: it must stay off-centre.
        let (bx, by) = project_point((10.0, 20.0, 0.0), &v, 100).unwrap();
        assert!(
            bx.abs() > 1.0 || by.abs() > 1.0,
            "bbox centre should not sit at the view centre, got ({bx},{by})"
        );
        // A yaw turn swings the model around the origin (turntable pivot).
        v.add_yaw(90.0f64.to_radians());
        let (px, py) = project_point((1.0, 0.0, 0.0), &v, 100).unwrap();
        assert!(
            px.abs() < 1.0 && py.abs() < 1.0,
            "yaw 90 should bring +X to the front around the origin, got ({px},{py})"
        );
    }

    #[test]
    fn pan_shifts_model_and_rotation_stays_centred() {
        // Move the model right on screen (pan_x+): the origin vertex's
        // projection moves right, and the rotated cube still surrounds it.
        let mut v = view();
        v.pan_x = 2.0;
        let (ox, _) = project_point((0.0, 0.0, 0.0), &v, 100).unwrap();
        assert!(ox > 0.0, "pan_x+ should move the model right on screen");
        // Rotating must keep the model centre (the pan point) at the same
        // screen position - i.e. the origin always projects through the pan.
        for yaw in [0.0f64, 45.0, 90.0, 180.0] {
            v.rot = rot_y(-yaw.to_radians());
            let (x, y) = project_point((0.0, 0.0, 0.0), &v, 100).unwrap();
            // origin -> v' = pan -> sx = f*pan_x/dist, constant for any yaw.
            let expected_x = (100.0f64 / 2.0) / (FOV_DEG / 2.0).to_radians().tan() * 2.0 / 8.0;
            assert!(
                (x - expected_x).abs() < 1e-6 && y.abs() < 1e-6,
                "rotation centre must stay on the panned model, yaw={yaw} got ({x},{y})"
            );
        }
    }

    #[test]
    fn roll_spins_around_panned_origin() {
        // Rolling must spin the model around its pivot (the panned file
        // origin), not around the screen centre: with a pan offset, the
        // origin's projection stays fixed under any roll.
        let mut v = view();
        v.pan_x = 5.0;
        v.pan_y = 3.0;
        let (ox0, oy0) = project_point((0.0, 0.0, 0.0), &v, 100).unwrap();
        for roll in [15.0f64, 90.0, 180.0, -45.0] {
            v.roll = roll.to_radians();
            let (ox, oy) = project_point((0.0, 0.0, 0.0), &v, 100).unwrap();
            assert!(
                (ox - ox0).abs() < 1e-6 && (oy - oy0).abs() < 1e-6,
                "origin (pivot) must stay fixed under roll, got ({ox},{oy}) vs ({ox0},{oy0})"
            );
        }
        // A point off the pivot keeps its on-screen distance from the pivot
        // while rolling (it spins around it).
        let (px, py) = project_point((1.0, 0.0, 0.0), &v, 100).unwrap();
        let d0 = ((px - ox0).powi(2) + (py - oy0).powi(2)).sqrt();
        v.roll += 90.0f64.to_radians();
        let (qx, qy) = project_point((1.0, 0.0, 0.0), &v, 100).unwrap();
        let d1 = ((qx - ox0).powi(2) + (qy - oy0).powi(2)).sqrt();
        assert!(
            (d0 - d1).abs() < 1e-3,
            "rolling must keep the distance from the pivot, {d0} vs {d1}"
        );
    }

    #[test]
    fn yaw_is_world_frame_at_pitch_90() {
        // Yaw is a world-frame turntable even at pitch = 90, not a spin
        // about the model's own Y axis.
        let mut v = view();
        v.add_pitch(90.0f64.to_radians());
        // Model Y axis points at the camera before yawing: it projects to the
        // screen centre.
        let (oy0x, _) = project_point((0.0, 1.0, 0.0), &v, 100).unwrap();
        assert!(
            oy0x.abs() < 1e-6,
            "model Y should start at the screen centre, got {oy0x}"
        );
        // Yaw left (positive, the object's own left): the model Y axis —
        // pointing at the camera here — swings toward screen-right, where the
        // model's own left side (+X) sits at pitch 90.
        v.add_yaw(45.0f64.to_radians());
        let (oy1x, _) = project_point((0.0, 1.0, 0.0), &v, 100).unwrap();
        assert!(
            oy1x > 0.0,
            "model Y should swing to screen-right, got {oy1x}"
        );
        // The model front (pointing screen-down at pitch 90) stays on the
        // world-vertical yaw axis.
        let (f0x, _) = project_point((0.0, 0.0, 1.0), &v, 100).unwrap();
        assert!(
            f0x.abs() < 1e-6,
            "front should stay on the yaw axis, got {f0x}"
        );
    }

    #[test]
    fn local_spin_rotates_around_model_y_axis() {
        // Space auto-spin is about the model's own (local) Y axis: the same
        // rotation sense as arrow-key yaw (both turn positive = the model's
        // own left), but post-multiplied so the axis rides the model instead
        // of pre-multiplied onto the world axis (see
        // yaw_is_world_frame_at_pitch_90).
        let mut v = view();
        v.add_pitch(90.0f64.to_radians());
        let (y0x, y0y) = project_point((0.0, 1.0, 0.0), &v, 100).unwrap();
        assert!(
            y0x.abs() < 1e-6 && y0y.abs() < 1e-6,
            "model Y should start at the screen centre, got ({y0x},{y0y})"
        );
        let (x0x, x0y) = project_point((1.0, 0.0, 0.0), &v, 100).unwrap();
        v.spin_local(90.0f64.to_radians());
        // The model's own Y axis is invariant under a local spin.
        let (y1x, y1y) = project_point((0.0, 1.0, 0.0), &v, 100).unwrap();
        assert!(
            (y1x - y0x).abs() < 1e-9 && (y1y - y0y).abs() < 1e-9,
            "model Y must stay fixed under local spin, got ({y1x},{y1y}) vs ({y0x},{y0y})"
        );
        // A point off the model's own axis actually moves: the model spins.
        let (x1x, x1y) = project_point((1.0, 0.0, 0.0), &v, 100).unwrap();
        assert!(
            (x1x - x0x).abs() > 1e-6 || (x1y - x0y).abs() > 1e-6,
            "off-axis point must move under local spin, got ({x1x},{x1y}) vs ({x0x},{x0y})"
        );
        // The display angle accumulates, matching add_yaw's HUD behaviour.
        assert!(
            (v.yaw - 90.0f64.to_radians()).abs() < 1e-9,
            "yaw should accumulate under local spin, got {}",
            v.yaw
        );
    }

    #[test]
    fn empty_model_fits_at_a_finite_distance() {
        // The blank start-up view: no vertices, so there is no extent to
        // frame. The camera distance must still be a real number — a NaN or
        // infinite distance turns every later projection into NaN and the
        // canvas renders nothing at all.
        let empty = Model::default();
        let mut v = ViewState::default();
        v.fit_to(&empty);
        assert!(
            v.dist.is_finite() && v.dist > 0.0,
            "fit of an empty model must be finite and positive, got {}",
            v.dist
        );
        // The floor is the unit scene (see `extent_from_bounds`), so the
        // empty view is framed exactly like a unit-sized model would be.
        let expected = 1.0 / (FOV_DEG / 2.0).to_radians().tan() * FIT_MARGIN;
        assert!(
            (v.dist - expected).abs() < 1e-9,
            "got {} expected {expected}",
            v.dist
        );
        // The origin — the only thing an empty scene has — projects to the
        // centre of the canvas instead of vanishing to NaN.
        let (x, y) = project_point((0.0, 0.0, 0.0), &v, 100).expect("origin must project");
        assert!(x.abs() < 1e-9 && y.abs() < 1e-9, "origin at ({x},{y})");
        // Reset (key `c`) re-fits and is equally safe on an empty model.
        v.dist = 1234.0;
        v.reset(&empty);
        assert!(v.dist.is_finite() && v.dist > 0.0, "reset: {}", v.dist);
        // Continuous motion scales by the model extent, which is undefined
        // here: the pan must stay finite rather than running off to infinity.
        let scale = model_extent(&empty);
        assert!(
            scale.is_finite() && scale > 0.0,
            "move scale of an empty model must be finite, got {scale}"
        );
    }

    #[test]
    fn model_extent_of_cube() {
        let m = cube(); // +-1 cube: geomean of (2,2,2) = 2
        let e = model_extent(&m);
        assert!((e - 2.0).abs() < 1e-9, "extent={e}");
        // Axis length at the golden ratio: model : axis = 0.618, so the axis
        // is longer than the model (extent / 0.618).
        let axis = e / 0.618;
        assert!((axis - 2.0 / 0.618).abs() < 1e-9);
        // Elongated model: the geomean is not dominated by the long axis.
        let long_model = Model {
            vertices: vec![(0.0, 0.0, 0.0), (6.0, 4.0, 160.0)],
            edges: vec![(0, 1)],
        };
        let e2 = model_extent(&long_model);
        assert!(
            e2 < 20.0,
            "geomean must not be dominated by the long axis, got {e2}"
        );
    }

    #[test]
    fn zoom_changes_scale() {
        let mut v = view();
        v.dist = 8.0;
        let far = project_point((1.0, 0.0, 0.0), &v, 100).unwrap().0;
        v.add_dist_delta(-4.0); // zoom in (dist 8 -> 4)
        let near = project_point((1.0, 0.0, 0.0), &v, 100).unwrap().0;
        assert!(
            near > far,
            "zooming in should enlarge the projection, far={far} near={near}"
        );
    }

    #[test]
    fn parallel_projection_and_bounds_match_serial() {
        // 110k verts: exercises the parallel path (>= PARALLEL_THRESHOLD).
        let verts: Vec<(f64, f64, f64)> = (0..110_000)
            .map(|i| {
                let t = i as f64;
                (t % 100.0, (t * 1.7) % 100.0, (t * 0.3) % 100.0)
            })
            .collect();
        let model = Model {
            vertices: verts.clone(),
            edges: vec![],
        };
        let v = ViewState::default();

        // bounds: parallel must match the serial reduction exactly.
        let b_par = bounds(&model);
        let b_ser = {
            let mut min = [f64::INFINITY; 3];
            let mut max = [f64::NEG_INFINITY; 3];
            for &(x, y, z) in &verts {
                min[0] = min[0].min(x);
                min[1] = min[1].min(y);
                min[2] = min[2].min(z);
                max[0] = max[0].max(x);
                max[1] = max[1].max(y);
                max[2] = max[2].max(z);
            }
            (min, max)
        };
        assert_eq!(b_par, b_ser, "parallel bounds must match serial");

        // projection: parallel (project_batch) vs a serial reference, bitwise.
        let mut out_par = vec![[0.0; 2]; verts.len()];
        let mut ok_par = vec![false; verts.len()];
        project_batch(&verts, &v, 120, &mut out_par, &mut ok_par);
        let mut out_ser = vec![[0.0; 2]; verts.len()];
        let mut ok_ser = vec![false; verts.len()];
        let f = (120.0 / 2.0) / (FOV_DEG / 2.0).to_radians().tan();
        let (sr, cr) = v.roll.sin_cos();
        let r = v.rot;
        let px = v.pan_x;
        let py = v.pan_y;
        let dist = v.dist;
        for (i, p) in verts.iter().enumerate() {
            let rx = r[0][0] * p.0 + r[0][1] * p.1 + r[0][2] * p.2 + px;
            let ry = r[1][0] * p.0 + r[1][1] * p.1 + r[1][2] * p.2 + py;
            let rz = r[2][0] * p.0 + r[2][1] * p.1 + r[2][2] * p.2;
            let z = dist - rz;
            if z <= 0.1 {
                ok_ser[i] = false;
                continue;
            }
            let (dx, dy) = (rx - px, ry - py);
            let rxr = px + dx * cr - dy * sr;
            let ryr = py + dx * sr + dy * cr;
            out_ser[i] = [f * rxr / z, f * ryr / z];
            ok_ser[i] = true;
        }
        assert_eq!(
            out_par, out_ser,
            "parallel projection output must match serial bitwise"
        );
        assert_eq!(
            ok_par, ok_ser,
            "parallel projection ok flags must match serial"
        );
    }
}
