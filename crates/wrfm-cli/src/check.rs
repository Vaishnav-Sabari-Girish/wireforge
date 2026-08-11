use ratatui_wireframe::model::Model;
use serde_json::{json, Value};
use std::collections::HashMap;

/// Coordinate tolerance for "duplicate" vertices / "zero-length" edges.
const EPS: f64 = 1e-6;
/// Quantization step for the duplicate-vertex hash (>> EPS so exact twins
const BUCKET: f64 = 1e-4;

/// Run the health check. `strict` upgrades warning-level issues (duplicates,
pub fn check(m: &Model, strict: bool) -> Value {
 let n = m.vertices.len();

 // Degree table (needed by several checks). All edge indices are valid
 // (see the module-level invariant), so `deg[a]` is in range directly.
 let mut deg = vec![0usize; n.max(1)];
 for &(a, b) in &m.edges {
 deg[a] += 1;
 deg[b] += 1;
 }

 // Duplicate vertices: hash by quantized coordinates, compare within bucket.
 let mut buckets: HashMap<(i64, i64, i64), Vec<usize>> = HashMap::new();
 for (i, &(x, y, z)) in m.vertices.iter().enumerate() {
 let key = (
 (x / BUCKET).round() as i64,
 (y / BUCKET).round() as i64,
 (z / BUCKET).round() as i64,
 );
 buckets.entry(key).or_default().push(i);
 }
 let mut dup_vertices: Vec<Value> = Vec::new();
 let mut seen_dup = vec![false; n];
 for bucket in buckets.values() {
 for (i, &a) in bucket.iter().enumerate() {
 for &b in &bucket[i + 1..] {
 let (va, vb) = (m.vertices[a], m.vertices[b]);
 let d2 = (va.0 - vb.0).powi(2) + (va.1 - vb.1).powi(2) + (va.2 - vb.2).powi(2);
 if d2 < EPS * EPS && !seen_dup[b] {
 seen_dup[b] = true;
 dup_vertices.push(json!({
                        "index": b,
                        "coords": [vb.0, vb.1, vb.2],
                        "twin": a,
 }));
 }
 }
 }
 }

 // Zero-length edges.
 let mut zero_edges: Vec<Value> = Vec::new();
 // Duplicate edges (unordered pairs).
 let mut edge_count: HashMap<(usize, usize), usize> = HashMap::new();
 for &(a, b) in &m.edges {
 // Edge indices are valid (module-level invariant), so `va`/`vb`
 // exist; the old `a < n && b < n` guard was removed (
 //).
 let (va, vb) = (m.vertices[a], m.vertices[b]);
 let d2 = (va.0 - vb.0).powi(2) + (va.1 - vb.1).powi(2) + (va.2 - vb.2).powi(2);
 if d2 < EPS * EPS {
            zero_edges.push(json!({ "edge": [a, b] }));
 }
 let key = (a.min(b), a.max(b));
 *edge_count.entry(key).or_insert(0) += 1;
 }
 let mut dup_edges: Vec<Value> = Vec::new();
 let mut counts: Vec<(usize, usize, usize)> = edge_count
 .iter()
 .filter(|(_, c)| **c > 1)
 .map(|(&(a, b), &c)| (a, b, c))
 .collect();
 counts.sort();
 for (a, b, c) in counts {
        dup_edges.push(json!({ "edge": [a, b], "repeats": c }));
 }

 // Dangling edges (touch a degree-1 vertex) and isolated vertices.
 let mut dangling: Vec<Value> = Vec::new();
 let mut isolated: Vec<usize> = Vec::new();
 let mut non_manifold: Vec<Value> = Vec::new();
 for (i, &d) in deg.iter().enumerate() {
 if d == 0 {
 isolated.push(i);
 } else if d == 1 {
 // The (up to two) edges touching this vertex.
 for (ei, &(a, b)) in m.edges.iter().enumerate() {
 if a == i || b == i {
                    dangling.push(json!({ "edge": [a, b], "vertex": i, "edge_index": ei }));
 }
 }
 } else if d == 2 {
            non_manifold.push(json!({ "vertex": i, "degree": 2 }));
 }
 }

 // Verdict.
 let broken = !zero_edges.is_empty() || !dup_edges.is_empty() || !isolated.is_empty();
 let warn = !dup_vertices.is_empty() || !dangling.is_empty() || !non_manifold.is_empty();
 let verdict = if broken || (warn && strict) {
        "broken"
 } else if warn {
        "warn"
 } else {
        "ok"
 };

 let n_issues = dup_vertices.len()
 + zero_edges.len()
 + dup_edges.len()
 + dangling.len()
 + isolated.len()
 + non_manifold.len();
 let summary = format!(
        "{n_issues} issue(s): {} duplicate vertices, {} zero-length edges, {} duplicate edges, {} dangling edges, {} isolated vertices, {} non-manifold vertices",
 dup_vertices.len(),
 zero_edges.len(),
 dup_edges.len(),
 dangling.len(),
 isolated.len(),
 non_manifold.len()
 );

 json!({
 // No "parseable" field: any file reaching
 // wrfm_check already passed the L1 load gate — syntax errors fail
        // at load, so the answer is always "true" by construction.
        "vertices": n,
        "edges": m.edges.len(),
        "issues": {
            "duplicate_vertices": dup_vertices,
            "zero_length_edges": zero_edges,
            "duplicate_edges": dup_edges,
            "dangling_edges": dangling,
            "isolated_vertices": isolated,
            "non_manifold_vertices": non_manifold,
 },
        "verdict": verdict,
        "summary": summary,
 })
}

/// Render the L2 check report as the `wrfm check` stdout text:
pub fn report_text(name: &str, vertices: usize, edges: usize, c: &serde_json::Value) -> String {
    let verdict = c["verdict"].as_str().unwrap_or("ok");
    let mut out = format!("{verdict}: {name} ({vertices} vertices, {edges} edges)\n");
    let issues = &c["issues"];
    let summary = c["summary"].as_str().unwrap_or("");
    if verdict != "ok" {
 out.push_str(summary);
 out.push('\n');
 for (kind, label) in [
            ("duplicate_vertices", "duplicate vertices"),
            ("zero_length_edges", "zero-length edges"),
            ("duplicate_edges", "duplicate edges"),
            ("dangling_edges", "dangling edges"),
            ("isolated_vertices", "isolated vertices"),
            ("non_manifold_vertices", "non-manifold vertices"),
 ] {
 let list = issues.get(kind).and_then(|v| v.as_array());
 if let Some(list) = list.filter(|l| !l.is_empty()) {
                out.push_str(&format!("  {label}:\n"));
 for item in list {
 match kind {
                        "duplicate_vertices" => out.push_str(&format!(
                            "    vertex {} at [{}, {}, {}] (twin {})\n",
                            item["index"],
                            item["coords"][0],
                            item["coords"][1],
                            item["coords"][2],
                            item["twin"],
 )),
                        "zero_length_edges" => out.push_str(&format!(
                            "    edge [{}, {}]\n",
                            item["edge"][0], item["edge"][1]
 )),
                        "duplicate_edges" => out.push_str(&format!(
                            "    edge [{}, {}] ({} repeats)\n",
                            item["edge"][0], item["edge"][1], item["repeats"],
 )),
                        "dangling_edges" => out.push_str(&format!(
                            "    edge [{}, {}] (edge index {}, touches degree-1 vertex {})\n",
                            item["edge"][0], item["edge"][1], item["edge_index"], item["vertex"],
 )),
                        "isolated_vertices" => {
                            out.push_str(&format!("    vertex {}\n", item.as_i64().unwrap_or(-1),))
 }
 _ => out.push_str(&format!(
                            "    vertex {} (degree {})\n",
                            item["vertex"], item["degree"],
 )),
 }
 }
 }
 }
 }
 out
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

 #[test]
 fn clean_cube_is_ok() {
 let r = check(&cube(), false);
        assert_eq!(r["verdict"], "ok");
 assert_eq!(
            r["issues"]["duplicate_vertices"].as_array().unwrap().len(),
 0
 );
 assert_eq!(
            r["issues"]["zero_length_edges"].as_array().unwrap().len(),
 0
 );
 }

 #[test]
 fn duplicate_vertex_detected() {
 let mut m = cube();
 m.vertices.push((1.0, 1.0, 1.0)); // twin of vertex 7
 m.edges.push((8, 0)); // keep it attached (degree 1, not isolated)
 let r = check(&m, false);
        assert_eq!(r["verdict"], "warn");
        let dups = r["issues"]["duplicate_vertices"].as_array().unwrap();
 assert_eq!(dups.len(), 1);
        assert_eq!(dups[0]["twin"], 7);
        assert_eq!(dups[0]["index"], 8);
 }

 #[test]
 fn zero_length_edge_is_broken() {
 let mut m = cube();
 m.vertices.push((0.0, 0.0, 0.0));
 m.edges.push((8, 8)); // same vertex
 let r = check(&m, false);
        assert_eq!(r["verdict"], "broken");
 assert_eq!(
            r["issues"]["zero_length_edges"].as_array().unwrap().len(),
 1
 );
 }

 #[test]
 fn dangling_and_isolated_detected() {
 let mut m = cube();
 // Dangling: an edge whose other end is a fresh degree-1 vertex.
 m.vertices.push((5.0, 5.0, 5.0));
 m.edges.push((0, 8));
 // Isolated: a fresh vertex with no edges.
 m.vertices.push((9.0, 9.0, 9.0));
 let r = check(&m, false);
        assert_eq!(r["verdict"], "broken"); // isolated -> broken
 assert_eq!(
            r["issues"]["isolated_vertices"].as_array().unwrap().len(),
 1
 );
        assert!(r["issues"]["dangling_edges"]
 .as_array()
 .unwrap()
 .iter()
            .any(|e| e["edge_index"] == 12));
 }

 #[test]
 fn duplicate_edge_detected() {
 let mut m = cube();
 m.edges.push((0, 1)); // duplicate of edge 0-1
 let r = check(&m, false);
        assert_eq!(r["verdict"], "broken");
        assert_eq!(r["issues"]["duplicate_edges"].as_array().unwrap().len(), 1);
 }

 #[test]
 fn strict_upgrades_warn_to_broken() {
 let mut m = cube();
 m.vertices.push((1.0, 1.0, 1.0)); // duplicate -> warn normally
 let r = check(&m, true);
        assert_eq!(r["verdict"], "broken");
 }
}

