use ratatui_wireframe::model::Model;
use std::collections::HashSet;

/// True when `i` is doomed for removal; out-of-range indices count as doomed so an invalid edge endpoint is never kept or indexed.
fn doomed(mark: &[bool], i: usize) -> bool {
 mark.get(i).copied().unwrap_or(true)
}

/// Delete vertices (set semantics — duplicates are fine) and EVERY edge touching them; remap all remaining edge indices into the new vertex
pub fn delete_vertices(m: &Model, remove: &[usize]) -> Model {
 let mut mark = vec![false; m.vertices.len()];
 for &i in remove {
 if i < m.vertices.len() {
 mark[i] = true;
 }
 }
 // New index of each kept vertex, and the kept vertices themselves.
 let mut new_index = vec![0usize; m.vertices.len()];
 let mut kept: Vec<(f64, f64, f64)> = Vec::new();
 for (i, &v) in m.vertices.iter().enumerate() {
 if !mark[i] {
 new_index[i] = kept.len();
 kept.push(v);
 }
 }
 let mut remapped = Vec::new();
 for &(a, b) in &m.edges {
 if !doomed(&mark, a) && !doomed(&mark, b) {
 remapped.push((new_index[a], new_index[b]));
 }
 }
 Model {
 vertices: kept,
 edges: remapped,
 }
}

/// Delete edges by their 0-based index in file order (set semantics); vertices and groups are unchanged.
pub fn delete_edges(m: &Model, remove: &[usize]) -> Model {
 let mut mark = vec![false; m.edges.len()];
 for &i in remove {
 if i < m.edges.len() {
 mark[i] = true;
 }
 }
 let edges = m
 .edges
 .iter()
 .enumerate()
 .filter(|(i, _)| !mark[*i])
 .map(|(_, e)| *e)
 .collect();
 Model {
 vertices: m.vertices.clone(),
 edges,
 }
}

/// Extract a group as its own model: vertices = the group's own `V_g` (global range `[vertex_start, vertex_end)`), edges = the group's
pub fn extract_group(m: &Model, g: &wrfm::Group) -> Model {
 let s = g.vertex_start.min(m.vertices.len());
 let e = g.vertex_end.min(m.vertices.len());
 let mut out_edges = Vec::new();
 for &(a, b) in &m.edges {
 let ai = a >= s && a < e;
 let bi = b >= s && b < e;
 if ai && bi {
 out_edges.push((a - s, b - s));
 }
 }
 Model {
 vertices: m.vertices[s..e].to_vec(),
 edges: out_edges,
 }
}

/// Remap group index ranges after deleting vertices: `new_start = start - |removed before start|`, `new_end = new_start +
pub fn remap_groups(groups: &[wrfm::Group], remove: &[usize]) -> Vec<wrfm::Group> {
 let doomed: HashSet<usize> = remove.iter().copied().collect();
 groups
 .iter()
 .map(|g| {
 let removed_before = doomed.iter().filter(|&&r| r < g.vertex_start).count();
 let kept = (g.vertex_start..g.vertex_end)
 .filter(|v| !doomed.contains(v))
 .count();
 wrfm::Group {
 name: g.name.clone(),
 vertex_start: g.vertex_start - removed_before,
 vertex_end: g.vertex_start - removed_before + kept,
 }
 })
 .collect()
}

// clean / dedupe / merge

/// `clean`: remove degenerate topology (zero-length edges, dangling edges, isolated vertices).
pub fn clean(m: &Model) -> (Model, Vec<usize>) {
 let n = m.vertices.len();
 let mut live = vec![true; m.edges.len()];
 let mut deg = vec![0usize; n];

 // Degree of each vertex over the current live edges (recomputed every
 // peel iteration; out-of-range endpoints are not counted).
 fn recompute(m: &Model, live: &[bool], deg: &mut [usize]) {
 deg.fill(0);
 for (ei, &(a, b)) in m.edges.iter().enumerate() {
 if !live[ei] {
 continue;
 }
 if a < deg.len() && b < deg.len() {
 deg[a] += 1;
 deg[b] += 1;
 }
 }
 }

 // 1. Drop zero-length edges (a == b).
 for (ei, &(a, b)) in m.edges.iter().enumerate() {
 if a == b {
 live[ei] = false;
 }
 }
 // 2. Iterative peel: drop every edge touching a degree-1 vertex until
 // no such edge remains. Out-of-range endpoints count as degree-1
 // (doomed), so an invalid edge is dropped rather than indexed.
 loop {
 recompute(m, &live, &mut deg);
 let mut dropped_any = false;
 for (ei, &(a, b)) in m.edges.iter().enumerate() {
 if !live[ei] {
 continue;
 }
 let dangling = deg.get(a).is_none_or(|&d| d == 1) || deg.get(b).is_none_or(|&d| d == 1);
 if dangling {
 live[ei] = false;
 dropped_any = true;
 }
 }
 if !dropped_any {
 break;
 }
 }
 recompute(m, &live, &mut deg);

 // 3. Removed = isolated vertices (degree 0); remap the survivors.
 let mut removed = Vec::new();
 let mut new_index = vec![0usize; n];
 let mut kept: Vec<(f64, f64, f64)> = Vec::new();
 for (i, &v) in m.vertices.iter().enumerate() {
 if deg[i] == 0 {
 removed.push(i);
 } else {
 new_index[i] = kept.len();
 kept.push(v);
 }
 }
 // 4. Remap the surviving edges (all live edges have valid endpoints by
 // construction — an out-of-range endpoint was dropped as dangling).
 let mut edges = Vec::new();
 for (ei, &(a, b)) in m.edges.iter().enumerate() {
 if live[ei] {
 edges.push((new_index[a], new_index[b]));
 }
 }
 (
 Model {
 vertices: kept,
 edges,
 },
 removed,
 )
}

/// `dedupe`: merge duplicate vertices (EXACT same (x,y,z) — the bit pattern, since `f64` is not reliably hashable) and drop duplicate /
pub fn dedupe(m: &Model) -> (Model, Vec<usize>) {
 use std::collections::{HashMap, HashSet};

 // 1. First-occurrence index keyed by the exact coordinate bit pattern.
 let mut first: HashMap<(u64, u64, u64), usize> = HashMap::new();
 let mut map = vec![0usize; m.vertices.len()];
 let mut kept: Vec<(f64, f64, f64)> = Vec::new();
 let mut removed = Vec::new();
 for (i, &(x, y, z)) in m.vertices.iter().enumerate() {
 let key = (x.to_bits(), y.to_bits(), z.to_bits());
 if let Some(&j) = first.get(&key) {
 map[i] = j;
 removed.push(i);
 } else {
 first.insert(key, i);
 map[i] = kept.len();
 kept.push((x, y, z));
 }
 }

 // 2. Edges: drop edges that collapse to zero length (original a == b,
 // or both endpoints merged to the same kept vertex), then dedupe by
 // the unordered kept pair, keeping the first occurrence.
 let mut seen: HashSet<(usize, usize)> = HashSet::new();
 let mut edges = Vec::new();
 for &(a, b) in &m.edges {
 let (Some(&ma), Some(&mb)) = (map.get(a), map.get(b)) else {
 continue; // out-of-range endpoint -> dropped
 };
 if ma == mb {
 continue; // zero-length after dedupe
 }
 let key = (ma.min(mb), ma.max(mb));
 if seen.insert(key) {
 edges.push((ma, mb));
 }
 }
 (
 Model {
 vertices: kept,
 edges,
 },
 removed,
 )
}

/// `merge`: concatenate model `b` into model `a` (vertices appended, edges offset).
pub fn merge(a: &Model, b: &Model) -> Model {
 let off = a.vertices.len();
 let mut vertices = a.vertices.clone();
 vertices.extend_from_slice(&b.vertices);
 let mut edges = a.edges.clone();
 edges.extend(b.edges.iter().map(|&(i, j)| (i + off, j + off)));
 Model { vertices, edges }
}

/// Merge two group lists: `a`'s groups keep their ranges, `b`'s groups are offset by `off` (the main model's vertex count — the append point).
pub fn merge_groups(a: &[wrfm::Group], b: &[wrfm::Group], off: usize) -> Vec<wrfm::Group> {
 let mut groups = a.to_vec();
 groups.extend(b.iter().map(|g| wrfm::Group {
 name: g.name.clone(),
 vertex_start: g.vertex_start + off,
 vertex_end: g.vertex_end + off,
 }));
 groups
}

#[cfg(test)]
mod tests {
 use super::*;

 fn verts(n: usize) -> Vec<(f64, f64, f64)> {
 (0..n).map(|i| (i as f64, 0.0, 0.0)).collect()
 }

 fn group(name: &str, s: usize, e: usize) -> wrfm::Group {
 wrfm::Group {
 name: name.to_string(),
 vertex_start: s,
 vertex_end: e,
 }
 }

 fn m(verts: Vec<(f64, f64, f64)>, edges: Vec<(usize, usize)>) -> Model {
 Model {
 vertices: verts,
 edges,
 }
 }

 #[test]
 fn delete_vertices_middle_remaps() {
 // remove=[1]: verts 0..3, edge (0,2) -> kept [0,2], edge (0,1).
 let out = delete_vertices(&m(verts(3), vec![(0, 2)]), &[1]);
 assert_eq!(out.vertices.len(), 2);
 assert_eq!(out.edges, vec![(0, 1)]);
 }

 #[test]
 fn delete_vertices_drops_touching_edges() {
 // Edge (0,1) and (1,2) both touch vertex 1 -> both dropped.
 let out = delete_vertices(&m(verts(3), vec![(0, 1), (1, 2)]), &[1]);
 assert_eq!(out.vertices.len(), 2);
 assert!(out.edges.is_empty());
 }

 #[test]
 fn delete_vertices_deduped_ok() {
 // Duplicate remove indices are set semantics: no double decrement.
 let a = delete_vertices(&m(verts(3), vec![(0, 2)]), &[1, 1]);
 let b = delete_vertices(&m(verts(3), vec![(0, 2)]), &[1]);
 assert_eq!(a.vertices, b.vertices);
 assert_eq!(a.edges, b.edges);
 }

 #[test]
 fn delete_vertices_out_of_range_ignored() {
 let out = delete_vertices(&m(verts(3), vec![(0, 2)]), &[99]);
 assert_eq!(out.vertices.len(), 3);
 assert_eq!(out.edges, vec![(0, 2)]);
 }

 #[test]
 fn delete_edges_by_index() {
 let model = m(verts(4), vec![(0, 1), (1, 2), (2, 3)]);
 let out = delete_edges(&model, &[1]);
 assert_eq!(out.edges, vec![(0, 1), (2, 3)]); // order preserved
 }

 #[test]
 fn delete_edges_out_of_range_ignored() {
 let model = m(verts(3), vec![(0, 1), (1, 2)]);
 let out = delete_edges(&model, &[5]);
 assert_eq!(out.edges, model.edges);
 }

 #[test]
 fn extract_group_keeps_internal_only() {
 // body 0..2: (0,1) internal, (1,2) boundary -> only (0,1) kept.
        let out = extract_group(&m(verts(3), vec![(0, 1), (1, 2)]), &group("body", 0, 2));
 assert_eq!(out.vertices.len(), 2);
 assert_eq!(out.edges, vec![(0, 1)]);
 }

 #[test]
 fn extract_group_remaps() {
 // body 0..3, internal edge (2,0): start=0 so it stays (2,0).
        let out = extract_group(&m(verts(3), vec![(2, 0)]), &group("body", 0, 3));
 assert_eq!(out.vertices.len(), 3);
 assert_eq!(out.edges, vec![(2, 0)]);
 }

 #[test]
 fn extract_group_nonzero_start_remaps() {
 // head 2..4, internal edge (2,3) -> (0,1) in the new model.
        let out = extract_group(&m(verts(4), vec![(2, 3)]), &group("head", 2, 4));
 assert_eq!(out.vertices.len(), 2);
 assert_eq!(out.edges, vec![(0, 1)]);
 }

 #[test]
 fn remap_groups_simple() {
        let gs = remap_groups(&[group("a", 2, 5)], &[3]);
 assert_eq!((gs[0].vertex_start, gs[0].vertex_end), (2, 4));
 }

 #[test]
 fn remap_groups_with_earlier_removals() {
 // g=[2,5), remove=[0,3]: removed_before=1, kept={2,4}=2 -> [1,3).
        let gs = remap_groups(&[group("a", 2, 5)], &[0, 3]);
 assert_eq!((gs[0].vertex_start, gs[0].vertex_end), (1, 3));
 }

 #[test]
 fn remap_groups_all_deleted_empty() {
 // g=[0,3) fully removed -> empty group kept ([0,0)).
        let gs = remap_groups(&[group("a", 0, 3)], &[0, 1, 2]);
 assert_eq!((gs[0].vertex_start, gs[0].vertex_end), (0, 0));
        assert_eq!(gs[0].name, "a");
 }

 #[test]
 fn remap_groups_unaffected_group_untouched() {
 // g=[5,8), remove=[0,1] -> removed_before=2, kept=3 -> [3,6).
        let gs = remap_groups(&[group("a", 5, 8)], &[0, 1]);
 assert_eq!((gs[0].vertex_start, gs[0].vertex_end), (3, 6));
 }
 // -- clean -----------------------------------------------------------------

 #[test]
 fn clean_drops_zero_length_and_dangling_and_isolated() {
 // Zero-length edge (0,0), dangling edge (1,2) on vertex 2, isolated
 // vertex 3, plus a healthy triangle 4-5-6.
 let out = clean(&m(
 verts(7),
 vec![(0, 0), (1, 2), (3, 3), (4, 5), (5, 6), (6, 4)],
 ))
 .0;
        assert_eq!(out.vertices.len(), 3, "only the triangle survives");
 assert_eq!(out.edges, vec![(0, 1), (1, 2), (2, 0)]);
 // Triangle vertices remapped to 0..3, edges re-referenced.
 }

 #[test]
 fn clean_removes_2_vertex_isolated_component() {
 // Two vertices joined by one edge: both endpoints degree 1, so the
 // edge is dropped and both vertices become isolated -> removed.
 let out = clean(&m(verts(2), vec![(0, 1)]));
 assert!(out.0.vertices.is_empty());
 assert!(out.0.edges.is_empty());
 assert_eq!(out.1, vec![0, 1]);
 }

 #[test]
 fn clean_preserves_healthy_model() {
 // A closed triangle is already clean: identical output.
 let model = m(verts(3), vec![(0, 1), (1, 2), (2, 0)]);
 let out = clean(&model);
 assert_eq!(out.0.vertices, model.vertices);
 assert_eq!(out.0.edges, model.edges);
 assert!(out.1.is_empty());
 }

 #[test]
 fn clean_peels_dangling_chain() {
 // Chain 0-1-2 with 2 dangling; the whole chain is peeled, but a
 // separate triangle 3-4-5 survives.
 let out = clean(&m(verts(6), vec![(0, 1), (1, 2), (3, 4), (4, 5), (5, 3)])).0;
 assert_eq!(out.vertices.len(), 3);
 assert_eq!(out.edges, vec![(0, 1), (1, 2), (2, 0)]);
 }

 // -- dedupe ----------------------------------------------------------------

 #[test]
 fn dedupe_merges_duplicate_vertices() {
 // Vertices 0 and 2 are both (1,1,1); edges (0,1) and (2,3) each
 // reach a distinct other vertex -> both survive, mapped to vertex 0.
 let model = m(
 vec![
 (1.0, 1.0, 1.0),
 (0.0, 0.0, 0.0),
 (1.0, 1.0, 1.0),
 (2.0, 2.0, 2.0),
 ],
 vec![(0, 1), (2, 3)],
 );
 let out = dedupe(&model);
 assert_eq!(out.0.vertices.len(), 3);
 assert_eq!(out.0.edges, vec![(0, 1), (0, 2)]);
 assert_eq!(out.1, vec![2]);
 }

 #[test]
 fn dedupe_drops_duplicate_edges() {
 // Edges (0,1), (1,0), (0,1): one unordered pair kept.
 let out = dedupe(&m(verts(2), vec![(0, 1), (1, 0), (0, 1)]));
 assert_eq!(out.0.edges, vec![(0, 1)]);
 assert!(out.1.is_empty());
 }

 #[test]
 fn dedupe_remaps_indices() {
 // verts 0,1,2 where 2 == 1 (duplicate): edges referencing 2 map to 1.
 let model = m(
 vec![(0.0, 0.0, 0.0), (1.0, 1.0, 1.0), (1.0, 1.0, 1.0)],
 vec![(0, 2)],
 );
 let out = dedupe(&model);
 assert_eq!(out.0.vertices.len(), 2);
 assert_eq!(out.0.edges, vec![(0, 1)]);
 assert_eq!(out.1, vec![2]);
 }

 #[test]
 fn dedupe_drops_edge_between_duplicates() {
 // Edge (1,2) where 1 and 2 are duplicates collapses to a zero-length
 // edge and is dropped.
 let model = m(
 vec![(0.0, 0.0, 0.0), (1.0, 1.0, 1.0), (1.0, 1.0, 1.0)],
 vec![(1, 2)],
 );
 let out = dedupe(&model);
 assert_eq!(out.0.vertices.len(), 2);
 assert!(out.0.edges.is_empty());
 }

 #[test]
 fn dedupe_zero_length_edge_dropped() {
 let out = dedupe(&m(verts(3), vec![(1, 1)]));
 assert!(out.0.edges.is_empty());
 }

 // -- merge -----------------------------------------------------------------

 #[test]
 fn merge_concatenates_and_offsets() {
 // a: 2 verts 1 edge; b: 3 verts 2 edges + a group (via merge_groups).
 let a = m(verts(2), vec![(0, 1)]);
 let b = m(verts(3), vec![(0, 1), (1, 2)]);
 let out = merge(&a, &b);
 assert_eq!(out.vertices.len(), 5);
 assert_eq!(out.edges, vec![(0, 1), (2, 3), (3, 4)]);
 }

 #[test]
 fn merge_groups_offset() {
 // a: 2 verts (no groups), b: group [0,3) -> merged group [2,5).
 let a = m(verts(2), vec![(0, 1)]);
 let b = m(verts(3), vec![(0, 1)]);
        let groups = merge_groups(&[], &[group("body", 0, 3)], a.vertices.len());
 assert_eq!(groups.len(), 1);
 assert_eq!((groups[0].vertex_start, groups[0].vertex_end), (2, 5));
 let _ = b;
 }
}

