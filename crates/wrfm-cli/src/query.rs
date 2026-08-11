use crate::render::bounds;
use ratatui_wireframe::model::Model;
use serde_json::json;

/// A query that can be run against a model.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Query {
 /// Bounding box, center, proportions, longest axis.
 Extents,
 /// Vertex/edge counts, degree distribution, dangling edges, components.
 Topology,
 /// Edge length statistics (min/max/avg, count of long edges).
 EdgeStats,
 /// Axis-aligned span plus how much of each axis is covered by edges.
 Profile,
 /// Edges crossing the plane `z = at` (default 0).
 CrossSection,
/// Every vertex (index + coords), optionally filtered by `--range` / `--group` — JSON.
 Vertices,
 /// Euclidean distance between two vertices (JSON).
 Distance,
 /// Whether two vertices are in the same connected component (JSON).
 Connectivity,
}

impl Query {
 pub const ALL: [&'static str; 8] = [
        "extents",
        "topology",
        "edge_stats",
        "profile",
        "cross_section",
        "vertices",
        "distance",
        "connectivity",
 ];

 pub fn parse(s: &str) -> Result<Query, String> {
 match s.trim().to_ascii_lowercase().as_str() {
            "extents" => Ok(Query::Extents),
            "topology" => Ok(Query::Topology),
            "edge_stats" => Ok(Query::EdgeStats),
            "profile" => Ok(Query::Profile),
            "cross_section" => Ok(Query::CrossSection),
            "vertices" => Ok(Query::Vertices),
            "distance" => Ok(Query::Distance),
            "connectivity" => Ok(Query::Connectivity),
 other => Err(format!(
                "unknown query '{other}' (expected {})",
                Query::ALL.join("|")
 )),
 }
 }
}

/// Arguments for the parameterised queries. `range` selects the vertex indices [a, b] for `vertices` (inclusive, 0-based, global); `from`/`to`
pub struct QueryArgs {
 /// Plane z value for `cross_section`.
 pub at: f64,
 /// Inclusive 0-based index range for `vertices` (None = all vertices).
 pub range: Option<(usize, usize)>,
 /// First vertex for `distance` / `connectivity`.
 pub from: Option<usize>,
 /// Second vertex for `distance` / `connectivity`.
 pub to: Option<usize>,
}

fn dist(a: (f64, f64, f64), b: (f64, f64, f64)) -> f64 {
 ((a.0 - b.0).powi(2) + (a.1 - b.1).powi(2) + (a.2 - b.2).powi(2)).sqrt()
}

fn components(m: &Model) -> usize {
 let n = m.vertices.len();
 let mut parent: Vec<usize> = (0..n).collect();
 fn find(parent: &mut Vec<usize>, x: usize) -> usize {
 if parent[x] != x {
 parent[x] = find(parent, parent[x]);
 }
 parent[x]
 }
 for &(a, b) in &m.edges {
 let (ra, rb) = (find(&mut parent, a), find(&mut parent, b));
 if ra != rb {
 parent[ra] = rb;
 }
 }
 (0..n)
 .map(|i| find(&mut parent, i))
 .collect::<std::collections::HashSet<_>>()
 .len()
}

/// Whether `a` and `b` are in the same connected component (BFS over the edge graph; `a == b` is trivially connected). Defensive about
fn connected(m: &Model, a: usize, b: usize) -> bool {
 if a == b {
 return true;
 }
 let n = m.vertices.len();
 let mut adj: Vec<Vec<usize>> = vec![Vec::new(); n];
 for &(u, v) in &m.edges {
 if u < n && v < n {
 adj[u].push(v);
 adj[v].push(u);
 }
 }
 let mut visited = vec![false; n];
 let mut queue = std::collections::VecDeque::new();
 visited[a] = true;
 queue.push_back(a);
 while let Some(u) = queue.pop_front() {
 for &w in &adj[u] {
 if !visited[w] {
 if w == b {
 return true;
 }
 visited[w] = true;
 queue.push_back(w);
 }
 }
 }
 false
}

/// Run `query` against `m` (with optional cross-section plane `at`).
pub fn run(m: &Model, query: Query, args: &QueryArgs) -> String {
 let at = args.at;
 match query {
 Query::Extents => {
 let (min, max) = bounds(m);
 let c = [
 (min[0] + max[0]) / 2.0,
 (min[1] + max[1]) / 2.0,
 (min[2] + max[2]) / 2.0,
 ];
 let span = [max[0] - min[0], max[1] - min[1], max[2] - min[2]];
            let (longest, axis) = ["x", "y", "z"]
 .iter()
 .zip(span.iter())
 .max_by(|a, b| a.1.partial_cmp(b.1).unwrap())
 .map(|(a, &s)| (s, *a))
 .unwrap();
 format!(
                "extents:\n  min=[{:.3},{:.3},{:.3}] max=[{:.3},{:.3},{:.3}] center=[{:.3},{:.3},{:.3}]\n  span x={:.3} y={:.3} z={:.3}  (longest axis: {axis} = {longest:.3})\n  proportions x:y:z = {:.2}:{:.2}:{:.2}",
 min[0],
 min[1],
 min[2],
 max[0],
 max[1],
 max[2],
 c[0],
 c[1],
 c[2],
 span[0],
 span[1],
 span[2],
 span[0] / span.iter().cloned().fold(f64::MIN, f64::max).max(1e-9),
 span[1] / span.iter().cloned().fold(f64::MIN, f64::max).max(1e-9),
 span[2] / span.iter().cloned().fold(f64::MIN, f64::max).max(1e-9),
 )
 }
 Query::Topology => {
 let n = m.vertices.len();
 let mut deg = vec![0usize; n];
 for &(a, b) in &m.edges {
 deg[a] += 1;
 deg[b] += 1;
 }
 let dangling = deg.iter().filter(|&&d| d == 1).count();
 let max_deg = deg.iter().copied().max().unwrap_or(0);
 let avg_deg = if n > 0 {
 deg.iter().sum::<usize>() as f64 / n as f64
 } else {
 0.0
 };
 format!(
                "topology:\n  vertices={} edges={}\n  degree min=0 max={max_deg} avg={avg_deg:.2}\n  dangling_edges={dangling} (degree-1 vertices)\n  connected_components={}",
 n,
 m.edges.len(),
 components(m)
 )
 }
 Query::EdgeStats => {
 let mut lens: Vec<f64> = m
 .edges
 .iter()
 .map(|&(a, b)| dist(m.vertices[a], m.vertices[b]))
 .collect();
 lens.sort_by(|a, b| a.partial_cmp(b).unwrap());
 let (min, max) = (
 lens.first().copied().unwrap_or(0.0),
 lens.last().copied().unwrap_or(0.0),
 );
 let avg = if !lens.is_empty() {
 lens.iter().sum::<f64>() / lens.len() as f64
 } else {
 0.0
 };
 let long = lens.iter().filter(|&&l| l > avg * 2.0).count();
 format!(
                "edge_stats:\n  edges={}  length min={min:.3} max={max:.3} avg={avg:.3}\n  edges_longer_than_2x_avg={long}",
 lens.len()
 )
 }
 Query::Profile => {
 let (min, max) = bounds(m);
 let span = [max[0] - min[0], max[1] - min[1], max[2] - min[2]];
 // Coverage: project every edge onto each axis, union the intervals.
 let mut cov = [[f64::MIN, f64::MAX]; 3];
 for &(a, b) in &m.edges {
 let (va, vb) = (m.vertices[a], m.vertices[b]);
 for (i, (pa, pb)) in [(va.0, vb.0), (va.1, vb.1), (va.2, vb.2)]
 .into_iter()
 .enumerate()
 {
 cov[i][0] = cov[i][0].max(pa.min(pb));
 cov[i][1] = cov[i][1].min(pa.max(pb));
 }
 }
 let _ = span;
 format!(
                "profile (axis span vs edge-cover extent):\n  x: span={:.3} edge_cover=[{:.3},{:.3}]\n  y: span={:.3} edge_cover=[{:.3},{:.3}]\n  z: span={:.3} edge_cover=[{:.3},{:.3}]",
 max[0] - min[0],
 cov[0][0],
 cov[0][1],
 max[1] - min[1],
 cov[1][0],
 cov[1][1],
 max[2] - min[2],
 cov[2][0],
 cov[2][1],
 )
 }
 Query::CrossSection => {
 let mut count = 0usize;
 let mut xs = [f64::MAX, f64::MIN];
 let mut ys = [f64::MAX, f64::MIN];
 for &(a, b) in &m.edges {
 let (va, vb) = (m.vertices[a], m.vertices[b]);
 let (z0, z1) = (va.2, vb.2);
 if (z0 - at) * (z1 - at) <= 0.0 && (z0 - z1).abs() > 1e-12 {
 let t = (at - z0) / (z1 - z0);
 let x = va.0 + t * (vb.0 - va.0);
 let y = va.1 + t * (vb.1 - va.1);
 xs[0] = xs[0].min(x);
 xs[1] = xs[1].max(x);
 ys[0] = ys[0].min(y);
 ys[1] = ys[1].max(y);
 count += 1;
 }
 }
 if count == 0 {
                format!("cross_section z={at}: no edges cross this plane")
 } else {
 format!(
                    "cross_section z={at}: {count} edges cross  x=[{:.3},{:.3}] y=[{:.3},{:.3}]",
 xs[0], xs[1], ys[0], ys[1]
 )
 }
 }
 Query::Vertices => {
 let selected: Vec<usize> = match args.range {
 // A degenerate range (a > b, e.g. an empty group) -> empty.
 Some((a, b)) if a <= b => (a..=b).collect(),
 Some(_) => Vec::new(),
 None => (0..m.vertices.len()).collect(),
 };
 let vs: Vec<serde_json::Value> = selected
 .iter()
 .map(|&i| {
 let (x, y, z) = m.vertices[i];
                    json!({ "index": i, "x": x, "y": y, "z": z })
 })
 .collect();
            serde_json::to_string_pretty(&json!({ "vertices": vs })).unwrap()
 }
 Query::Distance => {
 // Indices validated by the CLI (exit 2 on out of range).
 let (a, b) = (args.from.unwrap(), args.to.unwrap());
 let d = dist(m.vertices[a], m.vertices[b]);
 serde_json::to_string_pretty(&json!({
                "distance": d,
                "from": a,
                "to": b,
 }))
 .unwrap()
 }
 Query::Connectivity => {
 let (a, b) = (args.from.unwrap(), args.to.unwrap());
 let c = connected(m, a, b);
 serde_json::to_string_pretty(&json!({
                "connected": c,
                "from": a,
                "to": b,
 }))
 .unwrap()
 }
 }
}

#[cfg(test)]
mod tests {
 use super::*;

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

 #[test]
 fn query_parse() {
        assert_eq!(Query::parse("extents").unwrap(), Query::Extents);
        assert_eq!(Query::parse("EDGE_STATS").unwrap(), Query::EdgeStats);
        assert!(Query::parse("nope").is_err());
 }

 #[test]
 fn extents_of_cube() {
 let t = run(
 &cube(),
 Query::Extents,
 &QueryArgs {
 at: 0.0,
 range: None,
 from: None,
 to: None,
 },
 );
        assert!(t.contains("min=[-1.000,-1.000,-1.000]"));
        assert!(t.contains("max=[1.000,1.000,1.000]"));
        assert!(t.contains("span x=2.000 y=2.000 z=2.000"));
        assert!(t.contains("longest axis:"));
 }

 #[test]
 fn topology_of_cube() {
 let t = run(
 &cube(),
 Query::Topology,
 &QueryArgs {
 at: 0.0,
 range: None,
 from: None,
 to: None,
 },
 );
        assert!(t.contains("vertices=8 edges=12"));
        assert!(t.contains("max=3"));
        assert!(t.contains("connected_components=1"));
 }

 #[test]
 fn edge_stats_of_cube() {
 let t = run(
 &cube(),
 Query::EdgeStats,
 &QueryArgs {
 at: 0.0,
 range: None,
 from: None,
 to: None,
 },
 );
        assert!(t.contains("length min=2.000 max=2.000 avg=2.000"));
 }

 #[test]
 fn cross_section_plane_zero() {
 let t = run(
 &cube(),
 Query::CrossSection,
 &QueryArgs {
 at: 0.0,
 range: None,
 from: None,
 to: None,
 },
 );
        assert!(t.contains("edges cross"));
        assert!(t.contains("x=[-1.000,1.000] y=[-1.000,1.000]"));
 }
 fn verts(n: usize) -> Vec<(f64, f64, f64)> {
 (0..n).map(|i| (i as f64, 0.0, 0.0)).collect()
 }

 fn args() -> QueryArgs {
 QueryArgs {
 at: 0.0,
 range: None,
 from: None,
 to: None,
 }
 }

 #[test]
 fn query_parse_new_types() {
        assert_eq!(Query::parse("vertices").unwrap(), Query::Vertices);
        assert_eq!(Query::parse("DISTANCE").unwrap(), Query::Distance);
        assert_eq!(Query::parse("connectivity").unwrap(), Query::Connectivity);
 }

 #[test]
 fn vertices_query_filters_range() {
        // 5 verts, range "1,3" -> indices 1,2,3.
 let m = Model {
 vertices: verts(5),
 edges: vec![(0, 1)],
 };
 let out = run(
 &m,
 Query::Vertices,
 &QueryArgs {
 range: Some((1, 3)),
 ..args()
 },
 );
 let j: serde_json::Value = serde_json::from_str(&out).unwrap();
        let vs = j["vertices"].as_array().unwrap();
 let idx: Vec<usize> = vs
 .iter()
            .map(|v| v["index"].as_u64().unwrap() as usize)
 .collect();
 assert_eq!(idx, vec![1, 2, 3]);
        assert_eq!(vs[0]["x"], 1.0);
        assert_eq!(vs[2]["z"], 0.0);
 }

 #[test]
 fn vertices_query_all_when_no_filter() {
 let m = Model {
 vertices: verts(3),
 edges: vec![],
 };
 let out = run(&m, Query::Vertices, &args());
 let j: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(j["vertices"].as_array().unwrap().len(), 3);
        assert_eq!(j["vertices"][0]["index"], 0);
 }

 #[test]
 fn vertices_query_degenerate_range_empty() {
 // a > b (an empty group's [s, s) -> (s, s-1)) selects nothing.
 let m = Model {
 vertices: verts(4),
 edges: vec![],
 };
 let out = run(
 &m,
 Query::Vertices,
 &QueryArgs {
 range: Some((2, 1)),
 ..args()
 },
 );
 let j: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert!(j["vertices"].as_array().unwrap().is_empty());
 }

 #[test]
 fn distance_query_is_euclidean() {
 // (0,0,0) and (3,4,0) -> 5.0.
 let m = Model {
 vertices: vec![(0.0, 0.0, 0.0), (3.0, 4.0, 0.0)],
 edges: vec![],
 };
 let out = run(
 &m,
 Query::Distance,
 &QueryArgs {
 from: Some(0),
 to: Some(1),
 ..args()
 },
 );
 let j: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(j["distance"], 5.0);
 assert_eq!(
            (j["from"].as_u64().unwrap(), j["to"].as_u64().unwrap()),
 (0, 1)
 );
 }

 #[test]
 fn connectivity_query_bfs() {
 // Chain 0-1-2 vs isolated 3.
 let m = Model {
 vertices: verts(4),
 edges: vec![(0, 1), (1, 2)],
 };
 let a = |f, t| {
 run(
 &m,
 Query::Connectivity,
 &QueryArgs {
 from: Some(f),
 to: Some(t),
 ..args()
 },
 )
 };
 let j: serde_json::Value = serde_json::from_str(&a(0, 2)).unwrap();
        assert_eq!(j["connected"], true);
 let j: serde_json::Value = serde_json::from_str(&a(0, 3)).unwrap();
        assert_eq!(j["connected"], false);
 }

 #[test]
 fn connectivity_same_vertex_true() {
 let m = Model {
 vertices: verts(4),
 edges: vec![(0, 1)],
 };
 let out = run(
 &m,
 Query::Connectivity,
 &QueryArgs {
 from: Some(2),
 to: Some(2),
 ..args()
 },
 );
 let j: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(j["connected"], true);
 }
}

