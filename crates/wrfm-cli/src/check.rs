use crate::proximity::{Grid, POINT_TOL};
use ratatui_wireframe::model::Model;
use serde_json::{json, Value};
use std::collections::HashMap;

/// Tarjan bridge-finding: every edge whose removal disconnects its connected
/// component (an "open edge" — no cycle covers it). Runs the DFS over EACH
/// connected component separately (a model may be disconnected). Returns
/// vertex pairs (a, b). Parallel edges are handled by tracking the DFS tree
/// edge id, so a duplicated pair is never a bridge.
pub(crate) fn bridges(m: &Model) -> Vec<(usize, usize)> {
    let n = m.vertices.len();
    // (neighbor, edge id) adjacency — edge ids let a parallel edge to the
    // parent be treated as a back edge (never a bridge).
    let mut adj: Vec<Vec<(usize, usize)>> = vec![Vec::new(); n];
    for (ei, &(a, b)) in m.edges.iter().enumerate() {
        adj[a].push((b, ei));
        adj[b].push((a, ei));
    }
    let mut disc = vec![usize::MAX; n];
    let mut low = vec![0usize; n];
    let mut visited = vec![false; n];
    let mut out = Vec::new();
    let mut time = 0usize;

    #[allow(clippy::too_many_arguments)] // Tarjan state bundle for a tiny recursive helper
    fn dfs(
        u: usize,
        parent_edge: Option<usize>,
        adj: &[Vec<(usize, usize)>],
        disc: &mut [usize],
        low: &mut [usize],
        visited: &mut [bool],
        time: &mut usize,
        out: &mut Vec<(usize, usize)>,
    ) {
        visited[u] = true;
        disc[u] = *time;
        low[u] = *time;
        *time += 1;
        for &(v, ei) in &adj[u] {
            if !visited[v] {
                dfs(v, Some(ei), adj, disc, low, visited, time, out);
                low[u] = low[u].min(low[v]);
                if low[v] > disc[u] {
                    out.push((u, v));
                }
            } else if Some(ei) != parent_edge {
                low[u] = low[u].min(disc[v]);
            }
        }
    }

    for i in 0..n {
        if !visited[i] {
            dfs(
                i,
                None,
                &adj,
                &mut disc,
                &mut low,
                &mut visited,
                &mut time,
                &mut out,
            );
        }
    }
    out
}

/// Per-vertex degree over every edge — the table `check` itself is built on
/// (out-of-range endpoints are ignored so a corrupt index can never panic).
pub fn degrees(m: &Model) -> Vec<usize> {
    let mut deg = vec![0usize; m.vertices.len()];
    for &(a, b) in &m.edges {
        // One endpoint at a time: two `get_mut` calls cannot live in the
        // same borrow expression, and an out-of-range endpoint is skipped.
        if let Some(x) = deg.get_mut(a) {
            *x += 1;
        }
        if let Some(y) = deg.get_mut(b) {
            *y += 1;
        }
    }
    deg
}

/// Vertices of degree exactly 2 — the L2 "non-manifold" (pinch) verdict,
/// extracted from `check`'s degree scan so every caller counts the same set.
pub fn non_manifold_vertices(deg: &[usize]) -> Vec<usize> {
    deg.iter()
        .enumerate()
        .filter(|(_, d)| **d == 2)
        .map(|(i, _)| i)
        .collect()
}

/// Run the health check. `strict` upgrades warning-level issues (duplicates,
pub fn check(m: &Model, strict: bool) -> Value {
 let n = m.vertices.len();

 // Degree table (needed by several checks) — built by the shared `degrees`
 // helper, so `check` counts exactly the degrees any other caller sees.
 // All edge indices are valid (see the module-level invariant). An EMPTY
 // model keeps its historical one-slot table, so the scan below still
 // reports it `broken` (isolated vertex) instead of silently `ok`.
 let mut deg = degrees(m);
 if n == 0 {
  deg.push(0);
 }

 // Duplicate vertices, in TWO categories — the repair action differs:
 //
 // * `duplicate_vertices`      — the exact same coordinates (bit pattern).
 //                               Repair: `wrfm edit --dedupe`.
 // * `near_duplicate_vertices` — closer than POINT_TOL but not bit-equal
 //                               (independently sampled copies of one point
 //                               differ by ~1e-15). Repair: `wrfm edit
 //                               --weld 1e-6`.
 //
 // The near-duplicate scan uses the shared `proximity::Grid`, whose 27-cell
 // probe also finds pairs that straddle a cell boundary; the old
 // single-bucket probe silently missed those (regression test:
 // `check_finds_near_duplicates_across_a_cell_boundary`).
 let mut dup_vertices: Vec<Value> = Vec::new();
 let mut near_vertices: Vec<Value> = Vec::new();
 {
  let mut exact: HashMap<(u64, u64, u64), usize> = HashMap::new();
  let mut grid = Grid::new(POINT_TOL);
  for (i, &v) in m.vertices.iter().enumerate() {
   let bits = (v.0.to_bits(), v.1.to_bits(), v.2.to_bits());
   if let Some(&twin) = exact.get(&bits) {
    dup_vertices.push(json!({
                    "index": i,
                    "coords": [v.0, v.1, v.2],
                    "twin": twin,
    }));
   } else {
    exact.insert(bits, i);
    if let Some(twin) = grid.nearest_within(v, &m.vertices, POINT_TOL) {
     let w = m.vertices[twin];
     let d = ((v.0 - w.0).powi(2) + (v.1 - w.1).powi(2) + (v.2 - w.2).powi(2)).sqrt();
     near_vertices.push(json!({
                        "index": i,
                        "coords": [v.0, v.1, v.2],
                        "twin": twin,
                        "distance": d,
     }));
    }
   }
   grid.insert(i, v);
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
 if d2 < POINT_TOL * POINT_TOL {
            zero_edges.push(json!({ "edge": [a, b], "distance": d2.sqrt() }));
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
  }
 }
 // Isolated vertices are reported as objects like every other category
 // (they used to be bare integers, the one shape outlier in `issues`).
 let isolated_json: Vec<Value> = isolated.iter().map(|&i| json!({ "index": i })).collect();

 // Degree-2 pinch vertices (see `non_manifold_vertices`).
 let non_manifold: Vec<Value> = non_manifold_vertices(&deg)
 .into_iter()
 .map(|i| json!({ "vertex": i, "degree": 2 }))
 .collect();

 // Verdict.
 let broken = !zero_edges.is_empty() || !dup_edges.is_empty() || !isolated.is_empty();
 let warn = !dup_vertices.is_empty()
 || !near_vertices.is_empty()
 || !dangling.is_empty()
 || !non_manifold.is_empty();
 let verdict = if broken || (warn && strict) {
        "broken"
 } else if warn {
        "warn"
 } else {
        "ok"
 };

 let n_issues = dup_vertices.len()
 + near_vertices.len()
 + zero_edges.len()
 + dup_edges.len()
 + dangling.len()
 + isolated.len()
 + non_manifold.len();
 let summary = format!(
        "{n_issues} issue(s): {} duplicate vertices, {} near-duplicate vertices, {} zero-length edges, {} duplicate edges, {} dangling edges, {} isolated vertices, {} non-manifold vertices",
 dup_vertices.len(),
 near_vertices.len(),
 zero_edges.len(),
 dup_edges.len(),
 dangling.len(),
 isolated.len(),
 non_manifold.len()
 );

 let quality = quality_of(m);
 json!({
 // No "parseable" field: any file reaching
 // wrfm_check already passed the L1 load gate — syntax errors fail
        // at load, so the answer is always "true" by construction.
        "vertices": n,
        "edges": m.edges.len(),
        "issues": {
            "duplicate_vertices": dup_vertices,
            "near_duplicate_vertices": near_vertices,
            "zero_length_edges": zero_edges,
            "duplicate_edges": dup_edges,
            "dangling_edges": dangling,
            "isolated_vertices": isolated_json,
            "non_manifold_vertices": non_manifold,
 },
        "tolerance": POINT_TOL,
        "quality": quality,
        "verdict": verdict,
        "summary": summary,
 })
}

/// Informational quality diagnostics (open edges / proportion / orientation).
/// Computed for every model but NEVER affect the verdict and are NEVER
/// upgraded by `--strict` — they exist so `wrfm check` doubles as a
/// geometry-hint channel without changing the health contract.
fn quality_of(m: &Model) -> Value {
    // Axis spans of the bounding box.
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
    let empty = m.vertices.is_empty();
    let spans = [
        if empty { 0.0 } else { max[0] - min[0] },
        if empty { 0.0 } else { max[1] - min[1] },
        if empty { 0.0 } else { max[2] - min[2] },
    ];

    // Open edges = bridges, mapped back to their original edge indices.
    let open_edges: Vec<Value> = bridges(m)
        .iter()
        .flat_map(|&(a, b)| {
            m.edges
                .iter()
                .enumerate()
                .filter_map(move |(ei, &(ea, eb))| {
                    if (ea == a && eb == b) || (ea == b && eb == a) {
                        Some(json!({ "edge": [a, b], "edge_index": ei }))
                    } else {
                        None
                    }
                })
        })
        .collect();

    let mut quality = json!({
        "open_edges": open_edges,
        "orientation": {
            "x_span": spans[0],
            "y_span": spans[1],
            "z_span": spans[2],
            "lying_on_side": spans[2] > 2.0 * spans[1],
        },
    });

    // Proportion: omit the object when the model has no measurable span
    // (empty / single-point) — informational only, no threshold verdict.
    let min_span = spans[0].min(spans[1]).min(spans[2]);
    let max_span = spans[0].max(spans[1]).max(spans[2]);
    if min_span >= 1e-9 {
        let max_axis = if max_span == spans[0] {
            "x"
        } else if max_span == spans[1] {
            "y"
        } else {
            "z"
        };
        quality["proportion"] = json!({
            "aspect_ratio": max_span / min_span,
            "min_span": min_span,
            "max_span": max_span,
            "max_axis": max_axis,
        });
    }
    quality
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
            ("near_duplicate_vertices", "near-duplicate vertices"),
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
                        "near_duplicate_vertices" => out.push_str(&format!(
                            "    vertex {} at [{}, {}, {}] (twin {}, distance {:.3e})\n",
                            item["index"],
                            item["coords"][0],
                            item["coords"][1],
                            item["coords"][2],
                            item["twin"],
                            item["distance"].as_f64().unwrap_or(0.0),
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
                            out.push_str(&format!("    vertex {}\n", item["index"]))
 }
 _ => out.push_str(&format!(
                            "    vertex {} (degree {})\n",
                            item["vertex"], item["degree"],
 )),
 }
 }
 // The actionable threshold, printed ONCE and only under the list it
 // clears: `--weld` takes an explicit tolerance, and this is the value
 // that repairs exactly the near-duplicates above.
 if kind == "near_duplicate_vertices" {
 out.push_str(&format!(
 "  tolerance: {POINT_TOL:e} world units (repair with `wrfm edit <file> --weld {POINT_TOL:e}`)\n"
 ));
 }
 }
 }
 }
 // Informational quality line (never affects the verdict): emitted only
 // when at least one diagnostic is non-trivial (open edges / stretched
 // proportion / lying on side). The JSON keeps the full bridge list.
 if let Some(q) = c.get("quality") {
 let open = q["open_edges"].as_array().map(|a| a.len()).unwrap_or(0);
 let lying = q["orientation"]["lying_on_side"].as_bool().unwrap_or(false);
 let aspect = q["proportion"]["aspect_ratio"].as_f64();
 if open > 0 || lying || aspect.is_some_and(|a| a > 1.0) {
 let max_axis = q["proportion"]["max_axis"].as_str().unwrap_or("-");
 let y = q["orientation"]["y_span"].as_f64().unwrap_or(0.0);
 let z = q["orientation"]["z_span"].as_f64().unwrap_or(0.0);
 let mut line = format!("quality: {open} open edges");
 if let Some(a) = aspect {
 line.push_str(&format!(", aspect {a:.2} ({max_axis} longest)"));
 }
 if lying {
 line.push_str(&format!(", z {z:.1}x y {y:.1} - possibly lying on side"));
 }
 out.push_str(&line);
 out.push('\n');
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
 #[test]
 fn quality_open_edges_reported_but_verdict_unchanged() {
 // Two tetrahedra joined by ONE edge: every vertex has degree 3 (fully
 // "ok" health) yet edge (0, 4) is a bridge — removing it disconnects
 // the two components. (A literal chain would be `warn` for its dangling
 // degree-1 ends; this model isolates the "open edges" signal.)
 let m = Model {
 vertices: vec![
 (1.0, 1.0, 1.0),
 (1.0, -1.0, -1.0),
 (-1.0, 1.0, -1.0),
 (-1.0, -1.0, 1.0),
 (6.0, 1.0, 1.0),
 (6.0, -1.0, -1.0),
 (4.0, 1.0, -1.0),
 (4.0, -1.0, 1.0),
 ],
 edges: vec![
 (0, 1),
 (0, 2),
 (0, 3),
 (1, 2),
 (2, 3),
 (3, 1),
 (4, 5),
 (4, 6),
 (4, 7),
 (5, 6),
 (6, 7),
 (7, 5),
 (0, 4),
 ],
 };
 let r = check(&m, false);
 assert_eq!(r["verdict"], "ok");
 let open = r["quality"]["open_edges"].as_array().unwrap();
 assert_eq!(open.len(), 1);
 assert_eq!(open[0]["edge"], json!([0, 4]));
 }

 #[test]
 fn quality_orientation_flags_lying_model() {
 // z-span 4, y-span 1 -> lying_on_side true; verdict unchanged.
 let m = Model {
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
 edges: vec![
 (0, 1),
 (2, 3),
 (4, 5),
 (6, 7),
 (0, 2),
 (1, 3),
 (4, 6),
 (5, 7),
 (0, 4),
 (1, 5),
 (2, 6),
 (3, 7),
 ],
 };
 let r = check(&m, false);
 assert_eq!(r["quality"]["orientation"]["lying_on_side"], true);
 assert_eq!(r["quality"]["orientation"]["z_span"], 4.0);
 assert_eq!(r["quality"]["orientation"]["y_span"], 1.0);
 assert_eq!(r["verdict"], "ok");
 }

 #[test]
 fn quality_proportion_reports_aspect() {
 // Unit cube [-1,1]^3 -> aspect 1.0 (max_axis tie resolves to x).
 let r = check(&cube(), false);
 assert_eq!(r["quality"]["proportion"]["aspect_ratio"], 1.0);
 assert_eq!(r["quality"]["proportion"]["max_axis"], "x");
 // A 4 x 2 x 2 box -> aspect 2.0, x longest.
 let m = Model {
 vertices: vec![
 (-2.0, -1.0, -1.0),
 (2.0, -1.0, -1.0),
 (-2.0, 1.0, -1.0),
 (2.0, 1.0, -1.0),
 (-2.0, -1.0, 1.0),
 (2.0, -1.0, 1.0),
 (-2.0, 1.0, 1.0),
 (2.0, 1.0, 1.0),
 ],
 edges: vec![
 (0, 1),
 (2, 3),
 (4, 5),
 (6, 7),
 (0, 2),
 (1, 3),
 (4, 6),
 (5, 7),
 (0, 4),
 (1, 5),
 (2, 6),
 (3, 7),
 ],
 };
 let r2 = check(&m, false);
 assert_eq!(r2["quality"]["proportion"]["aspect_ratio"], 2.0);
 assert_eq!(r2["quality"]["proportion"]["max_axis"], "x");
 assert_eq!(r2["verdict"], "ok");
 }

}