use crate::render::bounds;
use ratatui_wireframe::model::Model;
use serde_json::{json, Value};

/// A query that can be run against a model.
///
/// Only the queries that are NOT already covered by `wrfm geometry` live
/// here: `profile` and `cross_section` need parameters, `vertices` /
/// `distance` / `connectivity` answer per-index questions. The three
/// summary queries (`extents` / `topology` / `edge_stats`) printed a subset
/// of `geometry`'s JSON and were removed — `Query::parse` points callers at
/// the replacement.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Query {
 /// Axis-aligned span plus the extent each axis is covered by edges.
 Profile,
 /// Edges crossing the plane `z = at` (default 0).
 CrossSection,
 /// Every vertex (index + coords), optionally filtered by `--range` / `--group`.
 Vertices,
 /// Euclidean distance between two vertices.
 Distance,
 /// Whether two vertices are in the same connected component.
 Connectivity,
}

impl Query {
 pub const ALL: [&'static str; 5] = [
        "profile",
        "cross_section",
        "vertices",
        "distance",
        "connectivity",
 ];

 pub fn parse(s: &str) -> Result<Query, String> {
 match s.trim().to_ascii_lowercase().as_str() {
            "profile" => Ok(Query::Profile),
            "cross_section" => Ok(Query::CrossSection),
            "vertices" => Ok(Query::Vertices),
            "distance" => Ok(Query::Distance),
            "connectivity" => Ok(Query::Connectivity),
 // Summarised by `wrfm geometry` (same numbers, JSON): name the
 // replacement instead of a bare "unknown query".
            "extents" => Err(moved_to_geometry("extents", "bounds")),
            "topology" => Err(moved_to_geometry("topology", "topology")),
            "edge_stats" => Err(moved_to_geometry("edge_stats", "edge_lengths")),
  other => Err(format!(
                "unknown query '{other}' (expected {})",
                Query::ALL.join("|")
  )),
 }
 }
}

/// The migration message for a query whose numbers `wrfm geometry` now owns.
fn moved_to_geometry(old: &str, field: &str) -> String {
    format!(
        "query '{old}' moved to `wrfm geometry` (see .{field}) — the same numbers, as JSON"
    )
}

/// Arguments for the parameterised queries. `range` selects the vertex indices [a, b] (inclusive, 0-based, global) for `vertices` / `distance` / `connectivity` (None = all vertices for `vertices`).
pub struct QueryArgs {
 /// Plane z value for `cross_section`.
 pub at: f64,
 /// Inclusive 0-based index range for `vertices` (None = all vertices); the two endpoint indices for `distance` / `connectivity`.
 pub range: Option<(usize, usize)>,
}

fn dist(a: (f64, f64, f64), b: (f64, f64, f64)) -> f64 {
 ((a.0 - b.0).powi(2) + (a.1 - b.1).powi(2) + (a.2 - b.2).powi(2)).sqrt()
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

/// The machine-readable answer — the authoritative form of every query.
/// `run` renders its text projection; nothing computes a number twice.
pub fn value(m: &Model, query: Query, args: &QueryArgs) -> Value {
 let at = args.at;
 match query {
 Query::Profile => {
  let (min, max) = bounds(m);
  let span = [max[0] - min[0], max[1] - min[1], max[2] - min[2]];
  // Coverage: the UNION of every edge's projection onto each axis
  // ([min of lower ends, max of upper ends]). The old code intersected
  // them (max of lower ends / min of upper ends), which for most models
  // collapsed to an inverted, meaningless interval.
  let mut cover: Option<[[f64; 2]; 3]> = None;
  for &(a, b) in &m.edges {
  let (va, vb) = (m.vertices[a], m.vertices[b]);
  let c = cover.get_or_insert([[f64::INFINITY, f64::NEG_INFINITY]; 3]);
  for (i, (pa, pb)) in [(va.0, vb.0), (va.1, vb.1), (va.2, vb.2)]
  .into_iter()
  .enumerate()
  {
   c[i][0] = c[i][0].min(pa.min(pb));
   c[i][1] = c[i][1].max(pa.max(pb));
  }
  }
  let axis = |i: usize| {
  json!({
                    "span": span[i],
                    "edge_cover": cover.map(|c| json!([c[i][0], c[i][1]])),
  })
  };
  json!({ "profile": { "x": axis(0), "y": axis(1), "z": axis(2) } })
 }
 Query::CrossSection => {
  let mut crossing: Vec<Value> = Vec::new();
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
   crossing.push(json!([a, b]));
  }
  }
  let empty = crossing.is_empty();
  json!({
            "at": at,
            "axis": "z",
            "edges_crossing": crossing,
            "x": if empty { Value::Null } else { json!([xs[0], xs[1]]) },
            "y": if empty { Value::Null } else { json!([ys[0], ys[1]]) },
  })
 }
 Query::Vertices => {
  let selected: Vec<usize> = match args.range {
  // A degenerate range (a > b, e.g. an empty group) -> empty.
  Some((a, b)) if a <= b => (a..=b).collect(),
  Some(_) => Vec::new(),
  None => (0..m.vertices.len()).collect(),
  };
  let vs: Vec<Value> = selected
  .iter()
  .map(|&i| {
  let (x, y, z) = m.vertices[i];
                json!({ "index": i, "x": x, "y": y, "z": z })
  })
  .collect();
  json!({ "vertices": vs })
 }
 Query::Distance => {
  // Indices validated by the CLI (exit 3 on out of range; the CLI
  // guarantees `range` is present for distance/connectivity).
  let (a, b) = args
  .range
  .expect("distance requires a --range (guaranteed by cmd_query)");
  json!({ "from": a, "to": b, "distance": dist(m.vertices[a], m.vertices[b]) })
 }
 Query::Connectivity => {
  let (a, b) = args
  .range
  .expect("connectivity requires a --range (guaranteed by cmd_query)");
  json!({ "from": a, "to": b, "connected": connected(m, a, b) })
 }
 }
}

/// The human-readable projection of [`value`] — it must not contain a number
/// the JSON does not have. `vertices` / `distance` / `connectivity` have no
/// separate text form: their pretty-printed JSON *is* the text.
pub fn run(m: &Model, query: Query, args: &QueryArgs) -> String {
 let v = value(m, query, args);
 match query {
 Query::Profile => {
  let axis = |name: &str| {
  let a = &v["profile"][name];
  let cover = match a["edge_cover"].as_array() {
                    Some(c) => format!(
                        "[{:.3},{:.3}]",
                        c[0].as_f64().unwrap_or(0.0),
                        c[1].as_f64().unwrap_or(0.0)
                    ),
                    None => "none".to_string(),
  };
  format!(
                    "  {name}: span={:.3} edge_cover={cover}",
                    a["span"].as_f64().unwrap_or(0.0)
  )
  };
  format!(
                "profile (axis span vs edge-cover extent):\n{}\n{}\n{}\n",
  axis("x"),
  axis("y"),
  axis("z")
  )
 }
 Query::CrossSection => {
  let at = v["at"].as_f64().unwrap_or(0.0);
  let n = v["edges_crossing"].as_array().map(|a| a.len()).unwrap_or(0);
  if n == 0 {
                return format!("cross_section z={at}: no edges cross this plane\n");
  }
  let (x0, x1) = (
  v["x"][0].as_f64().unwrap_or(0.0),
  v["x"][1].as_f64().unwrap_or(0.0),
  );
  let (y0, y1) = (
  v["y"][0].as_f64().unwrap_or(0.0),
  v["y"][1].as_f64().unwrap_or(0.0),
  );
  format!(
                "cross_section z={at}: {n} edges cross  x=[{x0:.3},{x1:.3}] y=[{y0:.3},{y1:.3}]\n"
  )
 }
 _ => serde_json::to_string_pretty(&v).unwrap(),
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
 fn query_parse_keeps_the_five_computational_queries() {
        assert_eq!(Query::parse("profile").unwrap(), Query::Profile);
        assert_eq!(Query::parse("CROSS_SECTION").unwrap(), Query::CrossSection);
        assert_eq!(Query::parse("vertices").unwrap(), Query::Vertices);
        assert_eq!(Query::parse("DISTANCE").unwrap(), Query::Distance);
        assert_eq!(Query::parse("connectivity").unwrap(), Query::Connectivity);
        assert!(Query::parse("nope").is_err());
 }

 #[test]
 fn query_parse_redirects_the_three_summary_queries() {
        // Removed modes must name their replacement, not say "unknown".
 for (old, field) in [
  ("extents", "bounds"),
  ("topology", "topology"),
  ("edge_stats", "edge_lengths"),
 ] {
  let e = Query::parse(old).unwrap_err();
  assert!(e.contains("wrfm geometry"), "{e}");
  assert!(e.contains(field), "{e}");
 }
 }

 #[test]
 fn profile_edge_cover_is_a_union_not_an_intersection() {
        // Regression: the intersection of every edge's projection is
        // degenerate for most models (cube: [1,-1], min > max).
 let v = value(&cube(), Query::Profile, &args());
 for axis in ["x", "y", "z"] {
  let c = v["profile"][axis]["edge_cover"].as_array().unwrap();
  let (lo, hi) = (c[0].as_f64().unwrap(), c[1].as_f64().unwrap());
  assert_eq!((lo, hi), (-1.0, 1.0), "{axis}");
  assert!(lo <= hi, "edge_cover must be ordered: {axis} {lo} {hi}");
  assert_eq!(v["profile"][axis]["span"], 2.0);
 }
 }

 #[test]
 fn profile_text_projection_matches_the_json() {
 let t = run(&cube(), Query::Profile, &args());
 assert!(t.contains("x: span=2.000 edge_cover=[-1.000,1.000]"), "{t}");
 assert!(t.ends_with('\n'), "text queries end with a newline");
 }

 #[test]
 fn profile_without_edges_has_no_cover() {
 let m = Model {
 vertices: vec![(0.0, 0.0, 0.0), (1.0, 0.0, 0.0)],
 edges: vec![],
 };
 let v = value(&m, Query::Profile, &args());
 assert!(v["profile"]["x"]["edge_cover"].is_null());
 assert!(run(&m, Query::Profile, &args()).contains("edge_cover=none"));
 }

 #[test]
 fn cross_section_plane_zero() {
 let t = run(
 &cube(),
 Query::CrossSection,
 &QueryArgs { at: 0.0, range: None },
 );
        assert!(t.contains("edges cross"));
        assert!(t.contains("x=[-1.000,1.000] y=[-1.000,1.000]"));
        // The count in the text is the length of the JSON edge list.
 let v = value(&cube(), Query::CrossSection, &args());
 let n = v["edges_crossing"].as_array().unwrap().len();
 assert!(t.contains(&format!("{n} edges cross")));
 }

 #[test]
 fn cross_section_without_crossings_is_empty_not_inverted() {
 let m = Model {
 vertices: vec![(0.0, 0.0, 0.0), (1.0, 0.0, 1.0)],
 edges: vec![(0, 1)],
 };
 let v = value(&m, Query::CrossSection, &QueryArgs { at: 5.0, ..args() });
 assert!(v["edges_crossing"].as_array().unwrap().is_empty());
 assert!(v["x"].is_null() && v["y"].is_null());
 assert!(run(&m, Query::CrossSection, &QueryArgs { at: 5.0, ..args() })
 .contains("no edges cross"));
 }

 fn verts(n: usize) -> Vec<(f64, f64, f64)> {
 (0..n).map(|i| (i as f64, 0.0, 0.0)).collect()
 }

 fn args() -> QueryArgs {
 QueryArgs { at: 0.0, range: None }
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
 range: Some((0, 1)),
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
 range: Some((f, t)),
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
 range: Some((2, 2)),
 ..args()
 },
 );
 let j: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(j["connected"], true);
 }
}
