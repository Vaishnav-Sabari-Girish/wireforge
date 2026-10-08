use crate::render::world_rot;
use ratatui_wireframe::model::Model;
use serde_json::{json, Value};

/// Camera / projection parameters for a `wrfm_view` call.
pub struct ViewParams {
 pub pitch_deg: f64,
 pub yaw_deg: f64,
 pub roll_deg: f64,
 pub dist: f64,
 /// Aim-point X offset (world units, the fork's pan_x).
 pub pan_x: f64,
 /// Aim-point Y offset (world units, the fork's pan_y).
 pub pan_y: f64,
}

/// Projected vertex: screen coords (relative units, +y up) + camera depth.
struct Proj {
 px: f64,
 py: f64,
 /// Distance from the camera plane along -Z (SMALL = closer).
 depth: f64,
}

/// Project every vertex with the fork's world-frame math (rotate, pan, roll, focal divide).
fn project(m: &Model, p: &ViewParams) -> Vec<Option<Proj>> {
 let rot = world_rot(p.pitch_deg, p.yaw_deg);
 let f = 100.0;
 let (sr, cr) = p.roll_deg.to_radians().sin_cos();
 m.vertices
 .iter()
 .map(|&(x, y, z)| {
 // v' = R * v + pan (rotate around the file origin, then
 // translate), so the rotation centre is always the (panned)
 // origin — the fork's project_point exactly.
 let rx = rot[0][0] * x + rot[0][1] * y + rot[0][2] * z + p.pan_x;
 let ry = rot[1][0] * x + rot[1][1] * y + rot[1][2] * z + p.pan_y;
 let rz = rot[2][0] * x + rot[2][1] * y + rot[2][2] * z;
 let depth = p.dist - rz;
 if depth <= 0.1 {
 return None;
 }
 // Roll around the view axis through the (panned) pivot (fork).
 let (dx, dy) = (rx - p.pan_x, ry - p.pan_y);
 let (rxr, ryr) = (p.pan_x + dx * cr - dy * sr, p.pan_y + dx * sr + dy * cr);
 Some(Proj {
 px: f * rxr / depth,
 py: f * ryr / depth,
 depth,
 })
 })
 .collect()
}

/// A fully-projectable edge.
struct VisEdge {
 a: usize,
 b: usize,
 avg_depth: f64,
}

/// A screen-space sampling grid for the z-buffer occlusion pass.
struct ScreenGrid {
 minx: f64,
 miny: f64,
 step: f64,
 w: usize,
 h: usize,
}

impl ScreenGrid {
/// Sample an edge every `step` screen units, calling `f` with the grid cell (and its interpolated depth) each sample lands in.
 fn for_each_sample(&self, p1: &Proj, p2: &Proj, mut f: impl FnMut(usize, usize, f64)) {
 let (dx, dy) = (p2.px - p1.px, p2.py - p1.py);
 let len = (dx * dx + dy * dy).sqrt();
 let n = ((len / self.step).ceil() as usize).max(1);
 for i in 0..=n {
 let t = i as f64 / n as f64;
 let x = p1.px + t * dx;
 let y = p1.py + t * dy;
 let d = p1.depth + t * (p2.depth - p1.depth);
 let gx = ((x - self.minx) / self.step) as i64;
 let gy = ((y - self.miny) / self.step) as i64;
 if gx >= 0 && gy >= 0 && (gx as usize) < self.w && (gy as usize) < self.h {
 f(gx as usize, gy as usize, d);
 }
 }
 }
}

/// Monotone-chain convex hull of projected visible vertices.
fn convex_hull(pts: &[(f64, f64, usize)]) -> Vec<usize> {
 if pts.len() <= 1 {
 return pts.iter().map(|&(_, _, i)| i).collect();
 }
 let mut p = pts.to_vec();
 p.sort_by(|a, b| {
 a.0.partial_cmp(&b.0)
 .unwrap()
 .then(a.1.partial_cmp(&b.1).unwrap())
 });
 let cross = |o: &(f64, f64, usize), a: &(f64, f64, usize), b: &(f64, f64, usize)| -> f64 {
 (a.0 - o.0) * (b.1 - o.1) - (a.1 - o.1) * (b.0 - o.0)
 };
 let mut lower: Vec<usize> = Vec::new();
 for i in 0..p.len() {
 while lower.len() >= 2
 && cross(
 &p[lower[lower.len() - 2]],
 &p[lower[lower.len() - 1]],
 &p[i],
 ) <= 0.0
 {
 lower.pop();
 }
 lower.push(i);
 }
 let mut upper: Vec<usize> = Vec::new();
 for i in (0..p.len()).rev() {
 while upper.len() >= 2
 && cross(
 &p[upper[upper.len() - 2]],
 &p[upper[upper.len() - 1]],
 &p[i],
 ) <= 0.0
 {
 upper.pop();
 }
 upper.push(i);
 }
 lower.pop();
 upper.pop();
 lower.extend(upper);
 lower.into_iter().map(|i| p[i].2).collect()
}

/// Compute per-view facts: visible and occluded edges (z-buffer occlusion), screen extent, totals and the silhouette (screen-space hull).
pub fn analyze(m: &Model, p: &ViewParams) -> Value {
 let proj = project(m, p);

 // Edges with both endpoints in front of the camera.
 let mut vis: Vec<VisEdge> = Vec::new();
 let mut behind = 0usize;
 for &(a, b) in &m.edges {
 match (proj[a].as_ref(), proj[b].as_ref()) {
 (Some(pa), Some(pb)) => vis.push(VisEdge {
 a,
 b,
 avg_depth: (pa.depth + pb.depth) / 2.0,
 }),
 _ => behind += 1,
 }
 }

 // Screen bounds of all visible vertices.
 let mut minx = f64::INFINITY;
 let mut maxx = f64::NEG_INFINITY;
 let mut miny = f64::INFINITY;
 let mut maxy = f64::NEG_INFINITY;
 let mut hull_pts: Vec<(f64, f64, usize)> = Vec::new();
 for (i, pr) in proj.iter().enumerate() {
 if let Some(q) = pr {
 minx = minx.min(q.px);
 maxx = maxx.max(q.px);
 miny = miny.min(q.py);
 maxy = maxy.max(q.py);
 hull_pts.push((q.px, q.py, i));
 }
 }
 let grid = if vis.is_empty() || minx >= maxx || miny >= maxy {
 ScreenGrid {
 minx,
 miny,
 step: 4.0,
 w: 1,
 h: 1,
 }
 } else {
 ScreenGrid {
 minx,
 miny,
 step: 4.0,
 w: (((maxx - minx) / 4.0).ceil() as usize + 1).clamp(1, 512),
 h: (((maxy - miny) / 4.0).ceil() as usize + 1).clamp(1, 512),
 }
 };
 let (w, h) = (grid.w, grid.h);

 // First pass: nearest-depth z-buffer per screen cell.
 let mut zbuf: Vec<Vec<(f64, i32)>> = vec![vec![(f64::INFINITY, -1); w]; h];
 for (ei, e) in vis.iter().enumerate() {
 let (pa, pb) = (&proj[e.a].as_ref().unwrap(), &proj[e.b].as_ref().unwrap());
 grid.for_each_sample(pa, pb, |gx, gy, d| {
 if d < zbuf[gy][gx].0 - 1e-9 {
 zbuf[gy][gx] = (d, ei as i32);
 }
 });
 }

 // Second pass: per-edge coverage + occluders.
 let mut visible_edges: Vec<Value> = Vec::new();
 let mut occluded_edges: Vec<Value> = Vec::new();
 let mut visible_total = 0usize;
 let mut occluded_total = 0usize;
 for (ei, e) in vis.iter().enumerate() {
 let (pa, pb) = (&proj[e.a].as_ref().unwrap(), &proj[e.b].as_ref().unwrap());
 let mut total = 0usize;
 let mut covered = 0usize;
 let mut occluders: Vec<usize> = Vec::new();
 grid.for_each_sample(pa, pb, |gx, gy, d| {
 total += 1;
 let (min_d, owner) = zbuf[gy][gx];
 if owner >= 0 && owner as usize != ei && min_d < d - 0.05 {
 covered += 1;
 if !occluders.contains(&(owner as usize)) {
 occluders.push(owner as usize);
 }
 }
 });
 let ratio = if total > 0 {
 covered as f64 / total as f64
 } else {
 0.0
 };
 let entry = json!({
            "edge": [e.a, e.b],
            "avg_depth": (e.avg_depth * 100.0).round() / 100.0,
 });
 if ratio >= 0.5 {
 occluded_total += 1;
 occluded_edges.push(json!({
                "edge": [e.a, e.b],
                "avg_depth": (e.avg_depth * 100.0).round() / 100.0,
                "covered_ratio": (ratio * 100.0).round() / 100.0,
                "occluded_by": occluders
 .iter()
 .map(|&i| json!([vis[i].a, vis[i].b]))
 .collect::<Vec<_>>(),
 }));
 } else {
 visible_total += 1;
 visible_edges.push(entry);
 }
 }
 // Near-to-far.
 visible_edges.sort_by(|a, b| {
        a["avg_depth"]
 .as_f64()
 .unwrap()
            .partial_cmp(&b["avg_depth"].as_f64().unwrap())
 .unwrap()
 });
 occluded_edges.sort_by(|a, b| {
        a["avg_depth"]
 .as_f64()
 .unwrap()
            .partial_cmp(&b["avg_depth"].as_f64().unwrap())
 .unwrap()
 });

 // Silhouette: convex hull of visible vertices in screen space.
 let hull = convex_hull(&hull_pts);
 let hull_set: std::collections::HashSet<usize> = hull.iter().copied().collect();
 let hull_edges: Vec<[usize; 2]> = (0..hull.len())
 .map(|i| [hull[i], hull[(i + 1) % hull.len()]])
 .collect();
 // Model edges whose both ends sit on the hull (approximate outline).
 let outline_edges: Vec<[usize; 2]> = m
 .edges
 .iter()
 .filter(|&&(a, b)| hull_set.contains(&a) && hull_set.contains(&b))
 .map(|&(a, b)| [a, b])
 .collect();

 let r3 = |x: f64| (x * 1000.0).round() / 1000.0;
 json!({
        "view": {
            "pitch_deg": p.pitch_deg,
            "yaw_deg": p.yaw_deg,
            "roll_deg": p.roll_deg,
            "dist": p.dist,
 },
        "screen_extent": {
            "min": [r3(minx), r3(miny)],
            "max": [r3(maxx), r3(maxy)],
 },
        "totals": {
            "edges": m.edges.len(),
            "visible": visible_total,
            "occluded": occluded_total,
            "behind_camera": behind,
 },
        "visible_edges": visible_edges,
        "occluded_edges": occluded_edges,
        "silhouette": {
            "hull_vertices": hull,
            "hull_polygon": hull_edges
 .iter()
 .map(|&[a, b]| json!([a, b]))
 .collect::<Vec<_>>(),
            "outline_edges": outline_edges
 .iter()
 .map(|&[a, b]| json!([a, b]))
 .collect::<Vec<_>>(),
 },
        "depth_hint": "visible_edges are listed near-to-far (avg_depth); smaller avg_depth = closer to the camera",
 })
}

#[cfg(test)]
mod tests {
 use super::*;

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

 fn params() -> ViewParams {
 ViewParams {
 pitch_deg: 0.0,
 yaw_deg: 0.0,
 roll_deg: 0.0,
 dist: 8.0,
 pan_x: 0.0,
 pan_y: 0.0,
 }
 }

 #[test]
 fn cube_front_occludes_back_edges() {
 let r = analyze(&cube(), &params());
        assert_eq!(r["totals"]["edges"], 12);
        assert_eq!(r["totals"]["behind_camera"], 0);
 // From the front, the 4 back-face edges (z=-1) sit inside the 4
 // front-face edges (z=+1) on screen and are occluded by them.
        assert_eq!(r["totals"]["occluded"], 4);
        assert_eq!(r["totals"]["visible"], 8);
 }

 #[test]
 fn front_edge_hides_far_edge() {
 // Near short edge B (z=5) occludes the middle of far long edge A (z=1)
 // when viewed from +Z (front).
 let m = Model {
 vertices: vec![
 (0.0, 0.0, 1.0), // A start (far)
 (2.0, 0.0, 1.0), // A end (far)
 (0.0, 0.0, 5.0), // B start (near)
 (0.5, 0.0, 5.0), // B end (near)
 ],
 edges: vec![(0, 1), (2, 3)],
 };
 let r = analyze(&m, &params());
 // A (edge 0-1) is largely covered by B -> occluded; B visible.
        let occ = r["occluded_edges"].as_array().unwrap();
        assert_eq!(occ.len(), 1, "expected edge A occluded: {r}");
        assert_eq!(occ[0]["edge"], json!([0, 1]));
        assert_eq!(occ[0]["occluded_by"], json!([[2, 3]]));
        assert_eq!(r["totals"]["visible"], 1);
        assert_eq!(r["totals"]["occluded"], 1);
 }

 #[test]
 fn silhouette_of_front_cube_is_outer_ring() {
 let r = analyze(&cube(), &params());
        let hull = r["silhouette"]["hull_vertices"].as_array().unwrap();
 // All 8 cube vertices are in front; the hull is the 4 front-face corners.
 assert_eq!(hull.len(), 4);
 }

 #[test]
 fn top_view_changes_silhouette() {
 let mut p = params();
 p.pitch_deg = -90.0;
 let r = analyze(&cube(), &p);
        let hull = r["silhouette"]["hull_vertices"].as_array().unwrap();
 assert_eq!(hull.len(), 4);
 }
}

