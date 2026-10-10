//! Camera projection: model-space vertex -> canvas coordinates.
//!
//! The math is the wireforge fork's, hoisted so every consumer — the TUI's
//! batch projection, `wrfm-cli`'s frame render, its depth analysis — runs the
//! same operations in the same order: world rotation + pan, camera-depth
//! cull, roll about the panned pivot, focal divide. Operation order is
//! load-bearing: the golden tests compare rendered frames bit for bit.

use crate::geometry::{FOV_DEG, Mat3};

/// How close to the camera a vertex may come before it counts as behind the
/// camera plane: [`Camera::project_full`] culls a vertex whose depth
/// (`dist - rz`, see [`Camera::camera_space`]) is at or below this, and
/// [`crate::raster::rasterize_camera_line`] clips a segment crossing it.
///
/// The two MUST use the same value: a segment is only ever passed to the
/// clipper with at least one endpoint the projection kept, and the clipper
/// has to agree on where "kept" ends.
pub const NEAR: f64 = 0.1;

/// Focal length in canvas units for a dot-grid height of `px_h`:
/// `f = (px_h / 2) / tan(FOV / 2)`.
///
/// `px_h` is the grid height in DOTS (character rows x 4 for braille).
pub fn focal(px_h: f64) -> f64 {
    (px_h / 2.0) / (FOV_DEG / 2.0).to_radians().tan()
}

/// Project one CAMERA-SPACE point (`Camera::camera_space` output) to canvas
/// coordinates at focal length `f`: the focal divide `[x, y] * f / z`, with
/// no rotation, pan or roll — those are already in the point.
///
/// This is what a clipped segment is drawn through, so the visible part
/// keeps the position the vertex projection would have given it. The point
/// must be in front of the camera plane (`z > NEAR`), which is what
/// [`crate::raster::rasterize_camera_line`] guarantees when it calls this.
pub fn project_camera_point(p: [f64; 3], f: f64) -> (f64, f64) {
    (p[0] * f / p[2], p[1] * f / p[2])
}

/// One projected vertex: canvas coordinates (origin at the view centre,
/// +x right, +y up) plus the camera depth.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Projected {
    /// Canvas x (before any grid rounding).
    pub px: f64,
    /// Canvas y (+up, before any grid rounding).
    pub py: f64,
    /// Distance from the camera plane along -Z (SMALL = closer).
    pub depth: f64,
}

/// The shared camera: model -> world rotation, distance from the file
/// origin, roll around the view axis, and the aim-point (pan) offset.
///
/// The roll sine/cosine are precomputed in the constructor. `f64::sin_cos`
/// is deterministic, so this is bit-identical to computing them on every
/// call while hoisting them out of per-vertex loops.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Camera {
    /// Model -> world rotation matrix (yaw + pitch, etc.).
    pub rot: Mat3,
    /// Camera distance from the file origin (zoom).
    pub dist: f64,
    /// Roll around the view axis (radians).
    pub roll: f64,
    /// Aim point X offset (world units).
    pub pan_x: f64,
    /// Aim point Y offset (world units).
    pub pan_y: f64,
    roll_sin: f64,
    roll_cos: f64,
}

impl Camera {
    /// A camera with the given pose; `roll` is in radians and its
    /// `sin_cos` is computed once here.
    pub fn new(rot: Mat3, dist: f64, roll: f64, pan_x: f64, pan_y: f64) -> Self {
        let (roll_sin, roll_cos) = roll.sin_cos();
        Self {
            rot,
            dist,
            roll,
            pan_x,
            pan_y,
            roll_sin,
            roll_cos,
        }
    }

    /// Project one vertex, returning coordinates + depth.
    ///
    /// `None` when the vertex is at or behind the camera plane
    /// (`dist - rz <= 0.1`).
    pub fn project_full(&self, p: (f64, f64, f64), f: f64) -> Option<Projected> {
        // v' = R * v + pan: rotate around the file origin, then translate,
        // so the rotation centre is always the (panned) origin.
        let rot = self.rot;
        let rx = rot[0][0] * p.0 + rot[0][1] * p.1 + rot[0][2] * p.2 + self.pan_x;
        let ry = rot[1][0] * p.0 + rot[1][1] * p.1 + rot[1][2] * p.2 + self.pan_y;
        let rz = rot[2][0] * p.0 + rot[2][1] * p.1 + rot[2][2] * p.2;

        // Fixed camera at [0,0,dist] looking down -Z.
        let z = self.dist - rz;
        if z <= NEAR {
            return None;
        }

        // Roll the camera frame about the panned origin so the pivot stays
        // fixed (reduces to screen-centre roll at pan = 0).
        let (sr, cr) = (self.roll_sin, self.roll_cos);
        let (dx, dy) = (rx - self.pan_x, ry - self.pan_y);
        let rxr = self.pan_x + dx * cr - dy * sr;
        let ryr = self.pan_y + dx * sr + dy * cr;
        Some(Projected {
            px: f * rxr / z,
            py: f * ryr / z,
            depth: z,
        })
    }

    /// Project one vertex to canvas coordinates; `None` behind the camera.
    pub fn project(&self, p: (f64, f64, f64), f: f64) -> Option<(f64, f64)> {
        self.project_full(p, f).map(|q| (q.px, q.py))
    }

    /// The camera-space position of a model-space vertex: `[x, y, z]` with
    /// the pan added to `x`/`y`, straight through the model -> world
    /// rotation, and `z` the depth in front of the camera (the `dist - rz`
    /// of [`Camera::project_full`], so `z <= NEAR` is behind the plane).
    ///
    /// Canvas coordinates follow as `[x, y] * scale` — the [`focal`] length
    /// of the grid. Callers that rasterize SEGMENTS rather than vertices
    /// keep these points so they can clip at the near plane (see
    /// [`crate::raster::rasterize_camera_line`]) instead of dropping a whole
    /// segment because one endpoint is behind the camera.
    pub fn camera_space(&self, p: (f64, f64, f64)) -> [f64; 3] {
        let rot = self.rot;
        [
            rot[0][0] * p.0 + rot[0][1] * p.1 + rot[0][2] * p.2 + self.pan_x,
            rot[1][0] * p.0 + rot[1][1] * p.1 + rot[1][2] * p.2 + self.pan_y,
            self.dist - (rot[2][0] * p.0 + rot[2][1] * p.1 + rot[2][2] * p.2),
        ]
    }

    /// Project one vertex into pre-allocated slots (for hot loops and rayon
    /// closures): `out` is only written when `ok` becomes `true`, mirroring
    /// the batch paths that leave stale coordinates behind the caller's
    /// "in front of the camera" flags.
    pub fn project_into(&self, p: (f64, f64, f64), f: f64, out: &mut [f64; 2], ok: &mut bool) {
        match self.project_full(p, f) {
            Some(q) => {
                *out = [q.px, q.py];
                *ok = true;
            }
            None => *ok = false,
        }
    }

    /// Batch-project every vertex (serial). Parallel callers wrap
    /// [`Camera::project_into`] in their own `par_iter` instead.
    pub fn project_all(
        &self,
        verts: &[(f64, f64, f64)],
        f: f64,
        out: &mut [[f64; 2]],
        ok: &mut [bool],
    ) {
        debug_assert_eq!(verts.len(), out.len());
        debug_assert_eq!(verts.len(), ok.len());
        for i in 0..verts.len() {
            self.project_into(verts[i], f, &mut out[i], &mut ok[i]);
        }
    }

    /// [`Camera::project_all`] plus the camera-space point of every vertex
    /// that projected, for callers that rasterize SEGMENTS: a segment is
    /// clipped at the near plane with [`crate::raster::rasterize_camera_line`]
    /// rather than dropped when one of its endpoints is culled.
    ///
    /// `cam` is only written where `ok` becomes `true`, and the `out`/`ok`
    /// results are identical to [`Camera::project_all`]'s.
    pub fn project_all_with_camera_space(
        &self,
        verts: &[(f64, f64, f64)],
        f: f64,
        out: &mut [[f64; 2]],
        ok: &mut [bool],
        cam: &mut [[f64; 3]],
    ) {
        debug_assert_eq!(verts.len(), out.len());
        debug_assert_eq!(verts.len(), ok.len());
        debug_assert_eq!(verts.len(), cam.len());
        for i in 0..verts.len() {
            self.project_into(verts[i], f, &mut out[i], &mut ok[i]);
            if ok[i] {
                cam[i] = self.camera_space(verts[i]);
            }
        }
    }
}

/// f32 projection for very large models: the same math as [`Camera`] with
/// every camera component cast to `f32` first (the TUI's large-model path).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CameraF32 {
    rot: [[f32; 3]; 3],
    dist: f32,
    roll_sin: f32,
    roll_cos: f32,
    pan_x: f32,
    pan_y: f32,
}

impl CameraF32 {
    /// Cast an f64 camera down to f32, component by component, exactly as
    /// the large-model path does (rotation first, then `sin_cos`, pan and
    /// distance — the cast order is fixed for bit-identical results).
    pub fn new(cam: &Camera) -> Self {
        let rot = [
            [
                cam.rot[0][0] as f32,
                cam.rot[0][1] as f32,
                cam.rot[0][2] as f32,
            ],
            [
                cam.rot[1][0] as f32,
                cam.rot[1][1] as f32,
                cam.rot[1][2] as f32,
            ],
            [
                cam.rot[2][0] as f32,
                cam.rot[2][1] as f32,
                cam.rot[2][2] as f32,
            ],
        ];
        Self {
            rot,
            dist: cam.dist as f32,
            roll_sin: cam.roll_sin as f32,
            roll_cos: cam.roll_cos as f32,
            pan_x: cam.pan_x as f32,
            pan_y: cam.pan_y as f32,
        }
    }

    /// f32 focal length: the f64 value of [`focal`] rounded to f32 once.
    pub fn focal(px_h: f64) -> f32 {
        focal(px_h) as f32
    }

    /// Project one vertex with f32 camera math; `out` is only written when
    /// `ok` becomes `true`.
    pub fn project_into(&self, p: (f64, f64, f64), f: f32, out: &mut [f32; 2], ok: &mut bool) {
        let p0 = p.0 as f32;
        let p1 = p.1 as f32;
        let p2 = p.2 as f32;
        let r = self.rot;
        let rx = r[0][0] * p0 + r[0][1] * p1 + r[0][2] * p2 + self.pan_x;
        let ry = r[1][0] * p0 + r[1][1] * p1 + r[1][2] * p2 + self.pan_y;
        let rz = r[2][0] * p0 + r[2][1] * p1 + r[2][2] * p2;
        let z = self.dist - rz;
        if z <= 0.1 {
            *ok = false;
            return;
        }
        let (sr, cr) = (self.roll_sin, self.roll_cos);
        let (dx, dy) = (rx - self.pan_x, ry - self.pan_y);
        let rxr = self.pan_x + dx * cr - dy * sr;
        let ryr = self.pan_y + dx * sr + dy * cr;
        *out = [f * rxr / z, f * ryr / z];
        *ok = true;
    }

    /// Batch-project every vertex in f32 (serial).
    pub fn project_all(
        &self,
        verts: &[(f64, f64, f64)],
        f: f32,
        out: &mut [[f32; 2]],
        ok: &mut [bool],
    ) {
        debug_assert_eq!(verts.len(), out.len());
        debug_assert_eq!(verts.len(), ok.len());
        for i in 0..verts.len() {
            self.project_into(verts[i], f, &mut out[i], &mut ok[i]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::{rot_axis, world_rot};

    /// The fork's original single-vertex formula, inlined verbatim as a
    /// reference: the shared `Camera` must match it BIT for bit.
    fn reference_project(
        p: (f64, f64, f64),
        rot: &Mat3,
        dist: f64,
        f: f64,
        pan_x: f64,
        pan_y: f64,
        roll: f64,
    ) -> Option<(f64, f64)> {
        let r = rot;
        let rx = r[0][0] * p.0 + r[0][1] * p.1 + r[0][2] * p.2 + pan_x;
        let ry = r[1][0] * p.0 + r[1][1] * p.1 + r[1][2] * p.2 + pan_y;
        let rz = r[2][0] * p.0 + r[2][1] * p.1 + r[2][2] * p.2;
        let z = dist - rz;
        if z <= 0.1 {
            return None;
        }
        let (sr, cr) = roll.sin_cos();
        let (dx, dy) = (rx - pan_x, ry - pan_y);
        let (rxr, ryr) = (pan_x + dx * cr - dy * sr, pan_y + dx * sr + dy * cr);
        Some((f * rxr / z, f * ryr / z))
    }

    /// The fork's original f32 batch body, inlined verbatim.
    fn reference_project_f32(
        p: (f64, f64, f64),
        rot: &Mat3,
        dist: f64,
        f: f64,
        pan_x: f64,
        pan_y: f64,
        roll: f64,
    ) -> Option<(f32, f32)> {
        let f = f as f32;
        let (sr, cr) = roll.sin_cos();
        let (sr, cr) = (sr as f32, cr as f32);
        let r = [
            [rot[0][0] as f32, rot[0][1] as f32, rot[0][2] as f32],
            [rot[1][0] as f32, rot[1][1] as f32, rot[1][2] as f32],
            [rot[2][0] as f32, rot[2][1] as f32, rot[2][2] as f32],
        ];
        let px = pan_x as f32;
        let py = pan_y as f32;
        let dist = dist as f32;
        let p0 = p.0 as f32;
        let p1 = p.1 as f32;
        let p2 = p.2 as f32;
        let rx = r[0][0] * p0 + r[0][1] * p1 + r[0][2] * p2 + px;
        let ry = r[1][0] * p0 + r[1][1] * p1 + r[1][2] * p2 + py;
        let rz = r[2][0] * p0 + r[2][1] * p1 + r[2][2] * p2;
        let z = dist - rz;
        if z <= 0.1 {
            return None;
        }
        let (dx, dy) = (rx - px, ry - py);
        let rxr = px + dx * cr - dy * sr;
        let ryr = py + dx * sr + dy * cr;
        Some((f * rxr / z, f * ryr / z))
    }

    /// A deterministic spread of vertices (in front of, at and behind the camera).
    fn points() -> Vec<(f64, f64, f64)> {
        let mut v = Vec::new();
        for i in 0..40 {
            let t = i as f64;
            v.push((
                (t * 3.7) % 17.0 - 8.0,
                (t * 5.1) % 13.0 - 6.0,
                (t * 2.3) % 21.0 - 4.0,
            ));
        }
        v.extend([
            (0.0, 0.0, 0.0),
            (8.1, 0.0, 0.0),
            (0.0, 0.0, 7.95),
            (0.0, 0.0, 8.5),
        ]);
        v
    }

    fn cameras() -> Vec<(Camera, f64)> {
        let mut out = Vec::new();
        let cases: [(f64, f64, f64, f64, f64, f64); 5] = [
            (0.0, 0.0, 0.0, 0.0, 0.0, 8.0),
            (30.0, -45.0, 0.0, 0.0, 0.0, 6.0),
            (0.7, 0.4, 0.9, 3.0, -2.0, 4.5),
            (-90.0, 180.0, -1.2, -5.0, 4.0, 12.0),
            (12.0, 350.0, 3.0, 0.5, 0.5, 0.05),
        ];
        for (pitch, yaw, roll, pan_x, pan_y, dist) in cases {
            let cam = Camera::new(world_rot(pitch, yaw), dist, roll.to_radians(), pan_x, pan_y);
            out.push((cam, focal(240.0)));
        }
        out
    }

    #[test]
    fn focal_matches_the_fork_formula() {
        for px_h in [1.0, 32.0, 96.0, 480.0, 4096.0] {
            let expected = (px_h / 2.0) / (FOV_DEG / 2.0).to_radians().tan();
            assert_eq!(focal(px_h).to_bits(), expected.to_bits(), "px_h={px_h}");
        }
        // The f32 path rounds the same f64 value once.
        assert_eq!(CameraF32::focal(240.0), focal(240.0) as f32);
    }

    #[test]
    fn camera_is_bit_identical_to_the_reference_formula() {
        for (cam, f) in cameras() {
            for p in points() {
                let got = cam.project_full(p, f);
                let want =
                    reference_project(p, &cam.rot, cam.dist, f, cam.pan_x, cam.pan_y, cam.roll);
                match (got, want) {
                    (Some(a), Some(b)) => {
                        assert_eq!(a.px.to_bits(), b.0.to_bits(), "px {p:?}");
                        assert_eq!(a.py.to_bits(), b.1.to_bits(), "py {p:?}");
                        assert_eq!(
                            a.depth.to_bits(),
                            (cam.dist
                                - (cam.rot[2][0] * p.0
                                    + cam.rot[2][1] * p.1
                                    + cam.rot[2][2] * p.2))
                                .to_bits(),
                            "depth {p:?}"
                        );
                    }
                    (None, None) => {}
                    (got, want) => panic!("cull mismatch for {p:?}: got {got:?} want {want:?}"),
                }
                // project and project_into agree with project_full bitwise.
                let (mut out, mut ok) = ([0.0; 2], false);
                cam.project_into(p, f, &mut out, &mut ok);
                assert_eq!(ok, got.is_some(), "ok flag {p:?}");
                if let Some(q) = got {
                    assert_eq!(out[0].to_bits(), q.px.to_bits(), "into px {p:?}");
                    assert_eq!(out[1].to_bits(), q.py.to_bits(), "into py {p:?}");
                }
            }
        }
    }

    #[test]
    fn camera_f32_is_bit_identical_to_the_reference_f32_formula() {
        for (cam, f) in cameras() {
            let cam32 = CameraF32::new(&cam);
            let f32v = CameraF32::focal(240.0);
            assert_eq!(f32v, f as f32);
            for p in points() {
                let (mut out, mut ok) = ([0.0f32; 2], false);
                cam32.project_into(p, f32v, &mut out, &mut ok);
                let want =
                    reference_project_f32(p, &cam.rot, cam.dist, f, cam.pan_x, cam.pan_y, cam.roll);
                match (ok, want) {
                    (true, Some((wx, wy))) => {
                        assert_eq!(out[0].to_bits(), wx.to_bits(), "px {p:?}");
                        assert_eq!(out[1].to_bits(), wy.to_bits(), "py {p:?}");
                    }
                    (false, None) => {}
                    (ok, want) => panic!("f32 cull mismatch {p:?}: ok={ok} want={want:?}"),
                }
            }
        }
    }

    #[test]
    fn batch_projection_with_camera_space_matches_project_all() {
        // The camera-space points are what the near-plane clipper clips:
        // they must be the projection's own `(rx, ry, dist - rz)`, and `out`
        // / `ok` must stay identical to the plain batch projection.
        let cam = Camera::new(rot_axis([1.0, 2.0, 0.5], 37.0), 9.0, 0.4, 1.0, -1.0);
        let f = focal(320.0);
        let verts = points();
        let n = verts.len();
        let mut out = vec![[0.0; 2]; n];
        let mut ok = vec![false; n];
        let mut space = vec![[0.0; 3]; n];
        cam.project_all_with_camera_space(&verts, f, &mut out, &mut ok, &mut space);
        let mut want_out = vec![[0.0; 2]; n];
        let mut want_ok = vec![false; n];
        cam.project_all(&verts, f, &mut want_out, &mut want_ok);
        assert_eq!(out, want_out, "screen coordinates to the bit");
        assert_eq!(ok, want_ok, "cull flags to the bit");
        for i in 0..n {
            if !ok[i] {
                continue;
            }
            let want = cam.camera_space(verts[i]);
            assert_eq!(space[i], want, "camera-space point {i}");
            // Depth counts UP from the camera and a projected vertex is in
            // front of the plane.
            let z = space[i][2];
            assert!(z > NEAR, "a projected vertex sits in front: {i} z={z}");
            // The canvas coordinate is the camera-space point at this scale
            // up to the camera roll (which `camera_space` deliberately omits:
            // clipping happens before it is applied).
            let (sr, cr) = cam.roll.sin_cos();
            let (dx, dy) = (space[i][0] - cam.pan_x, space[i][1] - cam.pan_y);
            let rx = cam.pan_x + dx * cr - dy * sr;
            let ry = cam.pan_y + dx * sr + dy * cr;
            assert!((out[i][0] - rx * f / z).abs() < 1e-9, "px {i}");
            assert!((out[i][1] - ry * f / z).abs() < 1e-9, "py {i}");
        }
    }

    #[test]
    fn project_all_matches_project_one() {
        let cam = Camera::new(rot_axis([1.0, 2.0, 0.5], 37.0), 9.0, 0.4, 1.0, -1.0);
        let f = focal(320.0);
        let verts = points();
        let mut out = vec![[0.0; 2]; verts.len()];
        let mut ok = vec![false; verts.len()];
        cam.project_all(&verts, f, &mut out, &mut ok);
        for (i, &p) in verts.iter().enumerate() {
            let q = cam.project_full(p, f);
            assert_eq!(ok[i], q.is_some(), "vertex {i}");
            if let Some(q) = q {
                assert_eq!(out[i][0].to_bits(), q.px.to_bits());
                assert_eq!(out[i][1].to_bits(), q.py.to_bits());
            }
        }
    }
}
