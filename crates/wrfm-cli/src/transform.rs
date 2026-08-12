use crate::render::{mat_mul, rot_axis, rot_x, rot_y, rot_z};
use ratatui_wireframe::model::Model;

/// The pivot point the whole transform happens about.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Pivot {
 /// The world origin `(0,0,0)` (the default).
 Origin,
 /// The model's centroid (mean of all vertices).
 Centroid,
 /// The centre of the model's bounding box.
 Bbox,
 /// An explicit point `(x,y,z)`.
 Point([f64; 3]),
}

/// `--align <axis>`: rotate the model so its longest PCA axis points at the world axis.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum AlignAxis {
 X,
 Y,
 Z,
}

/// A transform specification (all optional; identity when all defaults).
#[derive(Clone, Debug)]
pub struct Transform {
 pub rotate_x_deg: f64,
 pub rotate_y_deg: f64,
 pub rotate_z_deg: f64,
/// Arbitrary-axis rotation: axis direction + angle in degrees, applied after the coordinate-axis rotations.
 pub rotate_axis: Option<[f64; 3]>,
 pub rotate_angle_deg: f64,
 /// Uniform scale factor.
 pub scale: f64,
/// Anisotropic scale per axis (each default 1.0; total per-axis factor is `--scale * --scale-x` etc.).
 pub scale_x: f64,
 pub scale_y: f64,
 pub scale_z: f64,
/// Shears (3 degrees of freedom): `x' = x + shear_xy·y`, `x' = x + shear_xz·z`, `y' = y + shear_yz·z` (composed in that order).
 pub shear_xy: f64,
 pub shear_xz: f64,
 pub shear_yz: f64,
 /// The point the transform is applied about (default origin).
 pub pivot: Pivot,
/// Shorthand: pivot about the bbox centre AND translate by −centre, so the result's bbox centre lands at the origin (overrides `pivot`).
 pub to_origin: bool,
 /// Rotate the model's longest PCA principal axis onto this world axis.
 pub align: Option<AlignAxis>,
/// Uniformly scale so the ORIGINAL model's largest bbox span equals `SIZE` (must be > 0). `None` = no normalize step.
 pub normalize: Option<f64>,
 /// Translation offset (applied last; `--to-origin` appends −pivot here).
 pub translate: [f64; 3],
/// Mirror axis: 0 = x (x -> -x), 1 = y, 2 = z (applied after the linear part, still in the `(v−P)` frame).
 pub mirror: Option<usize>,
}

impl Default for Transform {
 fn default() -> Self {
 Self {
 rotate_x_deg: 0.0,
 rotate_y_deg: 0.0,
 rotate_z_deg: 0.0,
 rotate_axis: None,
 rotate_angle_deg: 0.0,
 scale: 1.0,
 scale_x: 1.0,
 scale_y: 1.0,
 scale_z: 1.0,
 shear_xy: 0.0,
 shear_xz: 0.0,
 shear_yz: 0.0,
 pivot: Pivot::Origin,
 to_origin: false,
 align: None,
 normalize: None,
 translate: [0.0; 3],
 mirror: None,
 }
 }
}

/// The 3x3 identity matrix.
const IDENTITY3: [[f64; 3]; 3] = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];

/// A diagonal 3x3 matrix `diag(a, b, c)`.
fn diag(a: f64, b: f64, c: f64) -> [[f64; 3]; 3] {
 [[a, 0.0, 0.0], [0.0, b, 0.0], [0.0, 0.0, c]]
}

/// Arithmetic mean of the vertices (`(0,0,0)` for an empty model).
fn centroid(verts: &[(f64, f64, f64)]) -> [f64; 3] {
 let n = verts.len() as f64;
 if n == 0.0 {
 return [0.0; 3];
 }
 let mut c = [0.0; 3];
 for &(x, y, z) in verts {
 c[0] += x;
 c[1] += y;
 c[2] += z;
 }
 for v in &mut c {
 *v /= n;
 }
 c
}

/// Centre of the bounding box (`(0,0,0)` for an empty model).
fn bbox_center(verts: &[(f64, f64, f64)]) -> [f64; 3] {
 let mut min = [f64::INFINITY; 3];
 let mut max = [f64::NEG_INFINITY; 3];
 for &(x, y, z) in verts {
 min[0] = min[0].min(x);
 min[1] = min[1].min(y);
 min[2] = min[2].min(z);
 max[0] = max[0].max(x);
 max[1] = max[1].max(y);
 max[2] = max[2].max(z);
 }
 if min[0].is_infinite() {
 return [0.0; 3];
 }
 [
 (min[0] + max[0]) / 2.0,
 (min[1] + max[1]) / 2.0,
 (min[2] + max[2]) / 2.0,
 ]
}

/// Largest bounding-box span (`0.0` for an empty model).
fn max_span(verts: &[(f64, f64, f64)]) -> f64 {
 let mut min = [f64::INFINITY; 3];
 let mut max = [f64::NEG_INFINITY; 3];
 for &(x, y, z) in verts {
 min[0] = min[0].min(x);
 min[1] = min[1].min(y);
 min[2] = min[2].min(z);
 max[0] = max[0].max(x);
 max[1] = max[1].max(y);
 max[2] = max[2].max(z);
 }
 if min[0].is_infinite() {
 return 0.0;
 }
 (max[0] - min[0]).max(max[1] - min[1]).max(max[2] - min[2])
}

/// Upper-triangular shears composed as `Sh_xy · Sh_xz · Sh_yz`: `--shear-xy K`: x' = x + K·y · `--shear-xz K`: x' = x + K·z ·
fn shear(k_xy: f64, k_xz: f64, k_yz: f64) -> [[f64; 3]; 3] {
 let sh_xy = [[1.0, k_xy, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
 let sh_xz = [[1.0, 0.0, k_xz], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
 let sh_yz = [[1.0, 0.0, 0.0], [0.0, 1.0, k_yz], [0.0, 0.0, 1.0]];
 mat_mul(sh_xy, mat_mul(sh_xz, sh_yz))
}

/// Resolve the pivot point from the transform's `Pivot` setting.
fn pivot_point(verts: &[(f64, f64, f64)], pivot: &Pivot) -> [f64; 3] {
 match pivot {
 Pivot::Origin => [0.0; 3],
 Pivot::Centroid => centroid(verts),
 Pivot::Bbox => bbox_center(verts),
 Pivot::Point(pt) => *pt,
 }
}

/// Rotation that maps the model's longest PCA axis onto `+axis` (Rodrigues about `u × e`).
fn align_rot(m: &Model, axis: AlignAxis) -> [[f64; 3]; 3] {
 let (axes, evals) = crate::geometry::principal_axes(m);
 if evals[0] < 1e-12 {
 return IDENTITY3;
 }
 let u0 = axes[0];
 let un = (u0[0] * u0[0] + u0[1] * u0[1] + u0[2] * u0[2]).sqrt();
 if un < 1e-12 {
 return IDENTITY3;
 }
 let e: [f64; 3] = match axis {
 AlignAxis::X => [1.0, 0.0, 0.0],
 AlignAxis::Y => [0.0, 1.0, 0.0],
 AlignAxis::Z => [0.0, 0.0, 1.0],
 };
 let mut u = [u0[0] / un, u0[1] / un, u0[2] / un];
 let mut dot = u[0] * e[0] + u[1] * e[1] + u[2] * e[2];
 if dot < 0.0 {
 u = [-u[0], -u[1], -u[2]];
 dot = -dot;
 }
 let cross = [
 u[1] * e[2] - u[2] * e[1],
 u[2] * e[0] - u[0] * e[2],
 u[0] * e[1] - u[1] * e[0],
 ];
 let cross_norm = (cross[0] * cross[0] + cross[1] * cross[1] + cross[2] * cross[2]).sqrt();
 if cross_norm < 1e-12 {
 // u is parallel to e (same direction after the flip): already aligned.
 return IDENTITY3;
 }
 let angle_deg = dot.min(1.0).acos().to_degrees();
 rot_axis(cross, angle_deg)
}

/// Apply the transform to every vertex; edges (indices) are unchanged.
pub fn apply(model: &Model, t: &Transform) -> Model {
 // 1. Pivot point; `--to-origin` overrides it to the bbox centre and adds
 // `-P` to the final translation.
 let (p, t_extra) = if t.to_origin {
 let c = bbox_center(&model.vertices);
 (c, [-c[0], -c[1], -c[2]])
 } else {
 (pivot_point(&model.vertices, &t.pivot), [0.0; 3])
 };

 // 2. Linear part L = N · (s · Sa · Sh · R · Ra). Leftmost = applied
 // last to the vector, so compose right-to-left from identity.
 let mut l = IDENTITY3;
 if let Some(axis) = t.align {
 l = mat_mul(align_rot(model, axis), l);
 }
 // Rotations keep the pre-existing relative order: x -> y -> z -> axis.
 l = mat_mul(rot_x(t.rotate_x_deg.to_radians()), l);
 l = mat_mul(rot_y(t.rotate_y_deg.to_radians()), l);
 l = mat_mul(rot_z(t.rotate_z_deg.to_radians()), l);
 if let Some(axis) = t.rotate_axis {
 l = mat_mul(rot_axis(axis, t.rotate_angle_deg), l);
 }
 l = mat_mul(shear(t.shear_xy, t.shear_xz, t.shear_yz), l);
 // Uniform scale (defensive: 0 is treated as 1, matching the old CLI)
 // combined with the per-axis factors.
 let s = if t.scale == 0.0 { 1.0 } else { t.scale };
 l = mat_mul(diag(s * t.scale_x, s * t.scale_y, s * t.scale_z), l);
 // Normalize is computed from the ORIGINAL model's bbox, then folded in
 // as the last uniform scale.
 if let Some(size) = t.normalize {
 let span = max_span(&model.vertices);
 if span > 0.0 {
 l = mat_mul(diag(size / span, size / span, size / span), l);
 }
 }

 // 3. Mirror, applied AFTER the linear part, still in the (v−P) frame.
 let mmat = match t.mirror {
 Some(0) => diag(-1.0, 1.0, 1.0),
 Some(1) => diag(1.0, -1.0, 1.0),
 Some(2) => diag(1.0, 1.0, -1.0),
 _ => IDENTITY3,
 };
 let mat = mat_mul(mmat, l);
 let tt = [
 t.translate[0] + t_extra[0],
 t.translate[1] + t_extra[1],
 t.translate[2] + t_extra[2],
 ];

 let vertices = model
 .vertices
 .iter()
 .map(|&(x, y, z)| {
 let dx = x - p[0];
 let dy = y - p[1];
 let dz = z - p[2];
 (
 mat[0][0] * dx + mat[0][1] * dy + mat[0][2] * dz + p[0] + tt[0],
 mat[1][0] * dx + mat[1][1] * dy + mat[1][2] * dz + p[1] + tt[1],
 mat[2][0] * dx + mat[2][1] * dy + mat[2][2] * dz + p[2] + tt[2],
 )
 })
 .collect();
 Model {
 vertices,
 edges: model.edges.clone(),
 }
}

#[cfg(test)]
mod tests {
 use super::*;

 /// The original test cube: span 2, centred at the origin (vertices ±1).
 fn cube() -> Model {
 let mut verts = Vec::new();
 let mut edges = Vec::new();
 for i in 0..2 {
 for j in 0..2 {
 for k in 0..2 {
 verts.push((
 if i == 0 { -1.0 } else { 1.0 },
 if j == 0 { -1.0 } else { 1.0 },
 if k == 0 { -1.0 } else { 1.0 },
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

 /// A unit cube spanning `[0,1]^3` (the "unit cube").
 fn unit_cube() -> Model {
 let mut verts = Vec::new();
 let mut edges = Vec::new();
 for i in 0..2 {
 for j in 0..2 {
 for k in 0..2 {
 verts.push((i as f64, j as f64, k as f64));
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

 /// A box spanning x∈[1,3], y∈[0,1], z∈[0,1] (bbox centre (2,0.5,0.5)).
 fn cube_at_x_1_3() -> Model {
 let m = unit_cube();
 Model {
 vertices: m
 .vertices
 .iter()
 .map(|&(x, y, z)| (1.0 + 2.0 * x, y, z))
 .collect(),
 edges: m.edges,
 }
 }

/// A 1×1×4 box stretched along +z (symmetric, so the PCA longest axis is exactly +z).
 fn long_z() -> Model {
 Model {
 vertices: vec![
 (0.0, 0.0, 0.0),
 (0.0, 0.0, 4.0),
 (1.0, 0.0, 0.0),
 (1.0, 0.0, 4.0),
 (0.0, 1.0, 0.0),
 (0.0, 1.0, 4.0),
 (1.0, 1.0, 0.0),
 (1.0, 1.0, 4.0),
 ],
 edges: vec![],
 }
 }

 fn approx(a: (f64, f64, f64), b: (f64, f64, f64)) -> bool {
 (a.0 - b.0).abs() < 1e-6 && (a.1 - b.1).abs() < 1e-6 && (a.2 - b.2).abs() < 1e-6
 }

 fn bbox(model: &Model) -> ([f64; 3], [f64; 3]) {
 crate::render::bounds(model)
 }

 fn span(model: &Model) -> f64 {
 let (min, max) = bbox(model);
 (max[0] - min[0]).max(max[1] - min[1]).max(max[2] - min[2])
 }

 #[test]
 fn default_is_identity() {
 // No flags: every vertex is unchanged (within 1e-12).
 let t = Transform::default();
 let out = apply(&cube(), &t);
 for (va, vb) in out.vertices.iter().zip(cube().vertices.iter()) {
 assert!(
 approx(*va, *vb),
                "identity must leave {vb:?} unchanged, got {va:?}"
 );
 }
 assert_eq!(out.edges, cube().edges);
 }

 #[test]
 fn rotate_y_180_flips_x_and_z() {
 let t = Transform {
 rotate_y_deg: 180.0,
 ..Transform::default()
 };
 let out = apply(&cube(), &t);
 // (1,1,1) -> (-1,1,-1)
 assert!(approx(out.vertices[7], (-1.0, 1.0, -1.0)));
 // Every vertex stays on the cube (norm preserved).
 for &(x, y, z) in &out.vertices {
 assert!((x * x + y * y + z * z - 3.0).abs() < 1e-6);
 }
 }

 #[test]
 fn scale_halves_size() {
 let t = Transform {
 scale: 0.5,
 ..Transform::default()
 };
 let out = apply(&cube(), &t);
 assert_eq!(out.vertices[7], (0.5, 0.5, 0.5));
 assert_eq!(out.vertices[0], (-0.5, -0.5, -0.5));
 }

 #[test]
 fn translate_shifts_all() {
 let t = Transform {
 translate: [3.0, 0.0, 2.0],
 ..Transform::default()
 };
 let out = apply(&cube(), &t);
 assert_eq!(out.vertices[7], (4.0, 1.0, 3.0));
 }

 #[test]
 fn mirror_x_flips_x() {
 let t = Transform {
 mirror: Some(0),
 ..Transform::default()
 };
 let out = apply(&cube(), &t);
 assert_eq!(out.vertices[7], (-1.0, 1.0, 1.0));
 assert_eq!(out.vertices[0], (1.0, -1.0, -1.0));
 }

 #[test]
 fn combined_rotate_scale_translate() {
 let t = Transform {
 rotate_y_deg: 90.0,
 scale: 2.0,
 translate: [1.0, 0.0, 0.0],
 ..Transform::default()
 };
 let out = apply(&cube(), &t);
 // (1,1,1): rotate y 90 -> [1,1,-1]; scale 2 -> [2,2,-2]; translate -> [3,2,-2]
 assert!(approx(out.vertices[7], (3.0, 2.0, -2.0)));
 }

 #[test]
 fn edges_preserved() {
 let t = Transform {
 rotate_z_deg: 45.0,
 ..Transform::default()
 };
 let out = apply(&cube(), &t);
 assert_eq!(out.edges.len(), cube().edges.len());
 assert_eq!(out.vertices.len(), cube().vertices.len());
 }

 #[test]
 fn rotate_axis_y_equals_rotate_y() {
 // (0,1,0) around 90deg must be numerically identical to rotate_y_deg.
 let via_axis = Transform {
 rotate_axis: Some([0.0, 1.0, 0.0]),
 rotate_angle_deg: 90.0,
 ..Transform::default()
 };
 let via_y = Transform {
 rotate_y_deg: 90.0,
 ..Transform::default()
 };
 let a = apply(&cube(), &via_axis);
 let b = apply(&cube(), &via_y);
 for (va, vb) in a.vertices.iter().zip(b.vertices.iter()) {
 assert!(
 approx(*va, *vb),
                "axis (0,1,0) 90deg must equal rotate_y 90deg: {va:?} vs {vb:?}"
 );
 }
 }

 #[test]
 fn rotate_axis_arbitrary_rodrigues() {
 // Axis (1,1,0) normalized to (1/√2,1/√2,0), angle 90deg on vertex
 // (1,1,1): Rodrigues gives v' = k×v + k(k·v) = (1+1/√2, 1-1/√2, 0).
 let t = Transform {
 rotate_axis: Some([1.0, 1.0, 0.0]),
 rotate_angle_deg: 90.0,
 ..Transform::default()
 };
 let out = apply(&cube(), &t);
 let root2 = std::f64::consts::SQRT_2;
 assert!(approx(
 out.vertices[7],
 (1.0 + root2 / 2.0, 1.0 - root2 / 2.0, 0.0)
 ));
 // Rotation preserves the norm of every cube vertex.
 for &(x, y, z) in &out.vertices {
 assert!((x * x + y * y + z * z - 3.0).abs() < 1e-6);
 }
 }

 #[test]
 fn rotate_axis_zero_is_identity() {
 // Defensive: a zero axis must not panic; it rotates nothing (the
 // MCP layer rejects it with -32602 before this point).
 let t = Transform {
 rotate_axis: Some([0.0, 0.0, 0.0]),
 rotate_angle_deg: 90.0,
 ..Transform::default()
 };
 let out = apply(&cube(), &t);
 assert_eq!(out.vertices, cube().vertices);
 }

 #[test]
 fn rotate_angle_ignored_without_axis() {
 let t = Transform {
 rotate_angle_deg: 90.0,
 ..Transform::default()
 };
 let out = apply(&cube(), &t);
 assert_eq!(out.vertices, cube().vertices);
 }

 #[test]
 fn combined_mirror_rotate_axis_scale() {
 // New fixed order: mirror is applied AFTER the linear part,
 // still in the (v−P) frame. So:
 // v=(1,1,1) -> rot_axis (0,1,0) 90 -> (1,1,-1) -> scale 2 -> (2,2,-2)
 // -> mirror x -> (-2,2,-2).
 let t = Transform {
 mirror: Some(0),
 rotate_axis: Some([0.0, 1.0, 0.0]),
 rotate_angle_deg: 90.0,
 scale: 2.0,
 ..Transform::default()
 };
 let out = apply(&cube(), &t);
 assert!(approx(out.vertices[7], (-2.0, 2.0, -2.0)));
 assert!(approx(out.vertices[0], (2.0, -2.0, 2.0)));
 }

 // ------------------------------------------------------------------
 // anisotropic scale
 // ------------------------------------------------------------------

 #[test]
 fn anisotropic_scale_y_doubles_y_span() {
 let t = Transform {
 scale_y: 2.0,
 ..Transform::default()
 };
 let out = apply(&unit_cube(), &t);
 let (min, max) = bbox(&out);
        assert!((max[0] - min[0] - 1.0).abs() < 1e-9, "x-span");
        assert!((max[1] - min[1] - 2.0).abs() < 1e-9, "y-span");
        assert!((max[2] - min[2] - 1.0).abs() < 1e-9, "z-span");
 }

 #[test]
 fn anisotropic_scale_combines_with_uniform() {
 let t = Transform {
 scale: 2.0,
 scale_x: 3.0,
 ..Transform::default()
 };
 let out = apply(&unit_cube(), &t);
 let (min, max) = bbox(&out);
        assert!((max[0] - min[0] - 6.0).abs() < 1e-9, "x-span");
        assert!((max[1] - min[1] - 2.0).abs() < 1e-9, "y-span");
        assert!((max[2] - min[2] - 2.0).abs() < 1e-9, "z-span");
 }

 // ------------------------------------------------------------------
 // shear
 // ------------------------------------------------------------------

 #[test]
 fn shear_xy_shifts_x_by_y() {
 let t = Transform {
 shear_xy: 1.0,
 ..Transform::default()
 };
 let out = apply(&unit_cube(), &t);
 // Vertex (1,1,0): x' = x + 1·y = 2.
 assert!(approx(out.vertices[6], (2.0, 1.0, 0.0)));
 }

 #[test]
 fn shear_xz_shifts_x_by_z() {
 let t = Transform {
 shear_xz: 2.0,
 ..Transform::default()
 };
 let out = apply(&unit_cube(), &t);
 // Vertex (0,0,1): x' = x + 2·z = 2.
 assert!(approx(out.vertices[1], (2.0, 0.0, 1.0)));
 }

 #[test]
 fn shear_yz_shifts_y_by_z() {
 let t = Transform {
 shear_yz: 2.0,
 ..Transform::default()
 };
 let out = apply(&unit_cube(), &t);
 // Vertex (0,0,1): y' = y + 2·z = 2.
 assert!(approx(out.vertices[1], (0.0, 2.0, 1.0)));
 }

 // ------------------------------------------------------------------
 // pivot / to_origin
 // ------------------------------------------------------------------

 #[test]
 fn pivot_bbox_rotate_180_keeps_bbox() {
 let t = Transform {
 pivot: Pivot::Bbox,
 rotate_x_deg: 180.0,
 ..Transform::default()
 };
 let out = apply(&unit_cube(), &t);
 let (min, max) = bbox(&out);
 assert!((min[0] - 0.0).abs() < 1e-9);
 assert!((max[0] - 1.0).abs() < 1e-9);
 assert!((min[1] - 0.0).abs() < 1e-9);
 assert!((max[1] - 1.0).abs() < 1e-9);
 assert!((min[2] - 0.0).abs() < 1e-9);
 assert!((max[2] - 1.0).abs() < 1e-9);
 }

 #[test]
 fn pivot_point_rotate_180_maps_point_to_itself() {
 // The "cube + point (0.5,0.5,0.5)": the cube plus an extra
 // vertex at its bbox centre, which is exactly the pivot point.
 let mut cube = unit_cube();
 cube.vertices.push((0.5, 0.5, 0.5));
 let t = Transform {
 pivot: Pivot::Point([0.5, 0.5, 0.5]),
 rotate_y_deg: 180.0,
 ..Transform::default()
 };
 let out = apply(&cube, &t);
 // The pivot vertex stays put under a 180 rotation about itself.
 assert!(
 out.vertices.iter().any(|&v| approx(v, (0.5, 0.5, 0.5))),
            "pivot vertex must stay at (0.5,0.5,0.5): {:?}",
 out.vertices
 );
 // The whole cube still occupies [0,1]^3 (rotate 180 about its own
 // centre maps the bbox onto itself).
 let (min, max) = bbox(&out);
 assert!((min[0] - 0.0).abs() < 1e-9 && (max[0] - 1.0).abs() < 1e-9);
 }

 #[test]
 fn center_moves_bbox_center_to_origin() {
 let t = Transform {
 to_origin: true,
 ..Transform::default()
 };
 let out = apply(&cube_at_x_1_3(), &t);
 let (min, max) = bbox(&out);
 let c = [
 (min[0] + max[0]) / 2.0,
 (min[1] + max[1]) / 2.0,
 (min[2] + max[2]) / 2.0,
 ];
 assert!(c[0].abs() < 1e-9 && c[1].abs() < 1e-9 && c[2].abs() < 1e-9);
 }

 // ------------------------------------------------------------------
 // align
 // ------------------------------------------------------------------

 #[test]
 fn align_y_puts_longest_axis_on_y() {
 let t = Transform {
 align: Some(AlignAxis::Y),
 ..Transform::default()
 };
 let out = apply(&long_z(), &t);
 let (min, max) = bbox(&out);
 let y_span = max[1] - min[1];
 let z_span = max[2] - min[2];
        assert!((y_span - 4.0).abs() < 1e-3, "y-span {y_span}");
        assert!((z_span - 1.0).abs() < 1e-3, "z-span {z_span}");
 }

 #[test]
 fn align_opposite_axis_flips_180() {
 // The long axis is +z; aligning onto +x rotates it 90deg there.
 let t = Transform {
 align: Some(AlignAxis::X),
 ..Transform::default()
 };
 let out = apply(&long_z(), &t);
 let (min, max) = bbox(&out);
 let x_span = max[0] - min[0];
        assert!((x_span - 4.0).abs() < 1e-3, "x-span {x_span}");
 }

 // ------------------------------------------------------------------
 // normalize
 // ------------------------------------------------------------------

 #[test]
 fn normalize_scales_max_span_to_size() {
 let t = Transform {
 normalize: Some(1.0),
 ..Transform::default()
 };
 let out = apply(&cube(), &t);
        assert!((span(&out) - 1.0).abs() < 1e-9, "max span {}", span(&out));
 }

 #[test]
 fn normalize_center_keeps_bbox_center() {
 let t = Transform {
 to_origin: true,
 normalize: Some(1.0),
 ..Transform::default()
 };
 let out = apply(&cube(), &t);
 let (min, max) = bbox(&out);
 let c = [
 (min[0] + max[0]) / 2.0,
 (min[1] + max[1]) / 2.0,
 (min[2] + max[2]) / 2.0,
 ];
 assert!(c[0].abs() < 1e-9 && c[1].abs() < 1e-9 && c[2].abs() < 1e-9);
        assert!((span(&out) - 1.0).abs() < 1e-9, "max span {}", span(&out));
 }

 // ------------------------------------------------------------------
 // mirror after pivot
 // ------------------------------------------------------------------

 #[test]
 fn mirror_still_works_after_pivot() {
 let t = Transform {
 pivot: Pivot::Bbox,
 mirror: Some(0),
 ..Transform::default()
 };
 let out = apply(&cube(), &t);
 // Bbox centre stays at the origin; x is inverted around it (the
 // x-span is unchanged, the x-coordinates are negated).
 let (min, max) = bbox(&out);
 assert!(
 ((min[0] + max[0]) / 2.0).abs() < 1e-9,
            "bbox centre x must stay put, got [{}, {}]",
 min[0],
 max[0]
 );
        assert!((max[0] - min[0] - 2.0).abs() < 1e-9, "x-span unchanged");
 assert!((min[1] + 1.0).abs() < 1e-9 && (max[1] - 1.0).abs() < 1e-9);
 assert!(approx(out.vertices[7], (-1.0, 1.0, 1.0)));
 assert!(approx(out.vertices[0], (1.0, -1.0, -1.0)));
 }

 // ------------------------------------------------------------------
 // degenerate align stays identity
 // ------------------------------------------------------------------

 #[test]
 fn align_degenerate_is_identity() {
 let degenerate = Model {
 vertices: vec![(0.5, 0.5, 0.5); 4],
 edges: vec![],
 };
 let t = Transform {
 align: Some(AlignAxis::Y),
 ..Transform::default()
 };
 let out = apply(&degenerate, &t);
 for (va, vb) in out.vertices.iter().zip(degenerate.vertices.iter()) {
 assert!(approx(*va, *vb));
 }
 }
}

