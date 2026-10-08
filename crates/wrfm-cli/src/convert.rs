//! `wrfm convert` — turn an OBJ model into `.wrfm`. The input format is
//! detected from the CONTENT (extension-independent, so a piped stream
//! behaves exactly like a file); `.wrfm` input is rejected as already
//! converted and anything without OBJ geometry is unrecognized — a mistyped
//! path can never silently produce an empty model.
//!
//! The OBJ reader is a deliberate subset: geometry in, lines out.
//! `v` defines vertices, `f` contributes the ring of edges around each
//! face, `l` contributes its chain of edges, and everything decorative
//! (`vt`/`vn`/`o`/`g`/`s`/materials) is skipped rather than guessed at —
//! no groups are ever invented.

use std::collections::HashSet;
use std::io::Read;
use wrfm::WrfmModel;

/// Convert the model read from `source` (`<path>`, or `-` for stdin) to
/// `.wrfm` — v1's only direction (obj -> wrfm).
pub fn convert(source: &str) -> Result<WrfmModel, String> {
    let text = read_source(source)?;
    // Strip a UTF-8 BOM: a BOM before the first record must not hide it
    // from detection or from the parser (where it would silently drop the
    // first vertex).
    let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
    let origin = if source == "-" { "stdin" } else { source };
    detect_obj(text).map_err(|e| format!("{origin}: {e}"))?;
    obj_to_wrfm(text).map_err(|e| format!("{origin}: {e}"))
}

/// Content-first input detection: a `wrfm <version>` magic line means the
/// input is already `.wrfm`; at least one geometry record (`v`/`f`/`l` —
/// the records this reader converts) makes it OBJ; anything else is
/// unrecognized.
fn detect_obj(text: &str) -> Result<(), String> {
    let first_line = text.lines().next().unwrap_or("");
    if first_line.split_whitespace().next() == Some("wrfm") {
        return Err("input is already .wrfm — convert only reads OBJ".to_string());
    }
    let has_geometry = text.lines().any(|raw| {
        let line = raw.trim_start();
        !line.is_empty()
            && !line.starts_with('#')
            && matches!(line.split_whitespace().next(), Some("v" | "f" | "l"))
    });
    if !has_geometry {
        return Err("unrecognized input: expected OBJ geometry (v / f / l records)".to_string());
    }
    Ok(())
}

/// Read `<path>` (or all of stdin for `-`) as UTF-8 text.
fn read_source(source: &str) -> Result<String, String> {
    if source == "-" {
        let mut buf = String::new();
        std::io::stdin()
            .read_to_string(&mut buf)
            .map_err(|e| format!("cannot read stdin: {e}"))?;
        return Ok(buf);
    }
    std::fs::read_to_string(source).map_err(|e| format!("cannot read '{source}': {e}"))
}

/// Parse the supported OBJ subset into a v1 model: `v` appends a vertex,
/// `f` appends the ring of edges around each face, `l` appends its chain
/// (both into one deduplicating set); every other record is skipped rather
/// than guessed at — groups are never invented. Vertex references resolve
/// against the vertices defined so far (forward references are rejected);
/// edges are stored undirected as `(min, max)`.
fn obj_to_wrfm(text: &str) -> Result<WrfmModel, String> {
    let mut model = WrfmModel::new("");
    let mut seen = HashSet::new();
    for (i, raw) in text.lines().enumerate() {
        let lineno = i + 1;
        let line = raw.trim_start();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut toks = line.split_whitespace();
        let Some(kind) = toks.next() else { continue };
        match kind {
            "v" => {
                let coords: Vec<&str> = toks.collect();
                if coords.len() < 3 {
                    return Err(format!(
                        "line {lineno}: vertex needs 3 coordinates, got {}",
                        coords.len()
                    ));
                }
                let mut xyz = [0.0f64; 3];
                for (slot, tok) in coords[..3].iter().enumerate() {
                    xyz[slot] = tok
                        .parse()
                        .map_err(|_| format!("line {lineno}: bad coordinate '{tok}'"))?;
                }
                model.vertices.push((xyz[0], xyz[1], xyz[2]));
            }
            "f" | "l" => {
                let refs: Vec<&str> = toks.collect();
                let need = if kind == "f" { 3 } else { 2 };
                if refs.len() < need {
                    return Err(format!(
                        "line {lineno}: {kind} needs at least {need} vertices, got {}",
                        refs.len()
                    ));
                }
                let mut idxs = Vec::with_capacity(refs.len());
                for field in &refs {
                    idxs.push(resolve_index(field, model.vertices.len(), lineno)?);
                }
                // `f` closes its ring; `l` is an open chain.
                let pairs = if kind == "f" {
                    idxs.len()
                } else {
                    idxs.len() - 1
                };
                for a in 0..pairs {
                    let (x, y) = (idxs[a], idxs[(a + 1) % idxs.len()]);
                    let key = (x.min(y), x.max(y));
                    if seen.insert(key) {
                        model.edges.push(key);
                    }
                }
            }
            // Decorative records (`vt`/`vn`/`o`/`g`/`s`/materials/…) and
            // anything unknown: skipped, never guessed at.
            _ => {}
        }
    }
    Ok(model)
}

/// One OBJ vertex reference: the first `/`-separated field only (`vt`/`vn`
/// are ignored) — 1-based from the front, negative from the tail, 0 invalid.
fn resolve_index(field: &str, len: usize, lineno: usize) -> Result<usize, String> {
    let vertex = field.split('/').next().unwrap_or("");
    let n: i64 = vertex
        .parse()
        .map_err(|_| format!("line {lineno}: bad vertex reference '{field}'"))?;
    if n == 0 {
        return Err(format!(
            "line {lineno}: vertex index 0 is invalid (OBJ indices are 1-based)"
        ));
    }
    let zero = if n > 0 { n - 1 } else { len as i64 + n };
    if zero < 0 || zero >= len as i64 {
        return Err(format!(
            "line {lineno}: vertex index {n} out of range ({len} vertices)"
        ));
    }
    Ok(zero as usize)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The reference model: three vertices, one triangular face.
    const TRI: &str = "\
v 0 0 0
v 1 0 0
v 0 1 0
f 1 2 3
";

    /// The canonical `(min, max)` edge set of the reference triangle.
    fn tri_edges() -> Vec<(usize, usize)> {
        vec![(0, 1), (1, 2), (0, 2)]
    }

    #[test]
    fn face_cycle_becomes_ring_edges() {
        let m = obj_to_wrfm(TRI).expect("triangle parses");
        assert_eq!(m.vertices.len(), 3, "every obj vertex is kept");
        assert_eq!(m.edges, tri_edges(), "ring edges, undirected");
        assert!(m.groups.is_empty(), "groups are never invented");
    }

    #[test]
    fn quad_keeps_four_ring_edges_without_diagonals() {
        let obj = "\
v 0 0 0
v 1 0 0
v 1 1 0
v 0 1 0
f 1 2 3 4
";
        let m = obj_to_wrfm(obj).expect("quad parses");
        assert_eq!(
            m.edges,
            vec![(0, 1), (1, 2), (2, 3), (0, 3)],
            "the polygon ring only — triangulation would add the (0,2) diagonal"
        );
    }

    #[test]
    fn shared_edges_are_deduplicated() {
        let obj = "\
v 0 0 0
v 1 0 0
v 1 1 0
v 0 1 0
f 1 2 3
f 3 4 1
";
        // tri1 = (0,1)(1,2)(0,2); tri2 = (2,3)(0,3)(0,2) — the last is shared.
        let m = obj_to_wrfm(obj).expect("two faces parse");
        assert_eq!(
            m.edges,
            vec![(0, 1), (1, 2), (0, 2), (2, 3), (0, 3)],
            "the shared edge appears exactly once"
        );
    }

    #[test]
    fn negative_indices_count_from_the_tail() {
        let obj = "\
v 0 0 0
v 1 0 0
v 0 1 0
f -3 -2 -1
";
        let m = obj_to_wrfm(obj).expect("negative indices parse");
        assert_eq!(m.edges, tri_edges(), "-3 -2 -1 is 1 2 3");
    }

    #[test]
    fn texcoord_and_normal_refs_are_ignored() {
        // `v/vt/vn`, `v//vn` and bare `v` all reduce to the vertex number;
        // vt/vn are never looked up (there are no vt/vn lines at all here).
        let obj = "\
v 0 0 0
v 1 0 0
v 0 1 0
f 1/1/1 2/2/1 3//1
";
        let m = obj_to_wrfm(obj).expect("v/vt/vn forms parse");
        assert_eq!(m.vertices.len(), 3);
        assert_eq!(m.edges, tri_edges());
    }

    #[test]
    fn polyline_records_become_edges_without_wrapping() {
        let obj = "\
v 0 0 0
v 1 0 0
v 1 1 0
l 1 2 3
";
        let m = obj_to_wrfm(obj).expect("polyline parses");
        assert_eq!(
            m.edges,
            vec![(0, 1), (1, 2)],
            "a chain — unlike a face, it does not close itself"
        );
    }

    #[test]
    fn point_cloud_keeps_every_vertex_with_no_edges() {
        let m = obj_to_wrfm("v 0 0 0\nv 1 0 0\nv 2 0 0\n").expect("vertices only parse");
        assert_eq!(m.vertices.len(), 3, "no faces, no dropped vertices");
        assert!(m.edges.is_empty());
    }

    #[test]
    fn unknown_records_are_skipped() {
        let obj = "\
mtllib scene.mtl
# a comment
o Widget
usemtl Steel
g body
s 1
v 0 0 0
v 1 0 0
v 0 1 0
vt 0 0
vn 0 0 1
p 1
f 1 2 3
";
        let m = obj_to_wrfm(obj).expect("decorative records skip");
        assert_eq!(m.vertices.len(), 3, "only `v` grows the vertex list");
        assert_eq!(m.edges, tri_edges());
    }

    #[test]
    fn malformed_records_are_errors() {
        let base = "v 0 0 0\nv 1 0 0\nv 0 1 0\n";
        // Out of range: there is no vertex 9.
        let err = obj_to_wrfm(&format!("{base}f 1 2 9")).expect_err("index 9 must be rejected");
        assert!(err.contains("line 4"), "names the failing line: {err}");
        // OBJ indices are 1-based: 0 never refers to a vertex.
        let err = obj_to_wrfm(&format!("{base}f 0 1 2")).expect_err("index 0 must be rejected");
        assert!(err.contains("line 4"), "names the failing line: {err}");
        // A non-numeric reference is a parse error, not a silent skip.
        let err = obj_to_wrfm(&format!("{base}f x 1 2")).expect_err("non-numeric ref");
        assert!(err.contains("bad vertex reference"), "{err}");
        // A face needs at least three vertices; a vertex needs three coords.
        let err = obj_to_wrfm(&format!("{base}f 1 2")).expect_err("two-point face");
        assert!(err.contains("at least 3"), "{err}");
        let err = obj_to_wrfm("v 0 1\n").expect_err("two coordinates");
        assert!(err.contains("3 coordinates"), "{err}");
    }

    #[test]
    fn detection_rejects_wrfm_and_garbage() {
        // An already-converted model is not OBJ input.
        let err = detect_obj("wrfm 1\nvertices 0   edges 0\n").expect_err("wrfm is not OBJ");
        assert!(err.contains("already .wrfm"), "{err}");
        // No OBJ geometry at all: never a silent empty model.
        let err = detect_obj("# just a comment\nmtllib scene.mtl\n").expect_err("no geometry");
        assert!(err.contains("unrecognized"), "{err}");
        // Any geometry record this reader converts makes it OBJ — even a
        // bare point cloud.
        detect_obj("v 0 0 0\n").expect("v is a geometry record");
        detect_obj("f 1 2 3\n").expect("f is a geometry record");
        detect_obj("l 1 2\n").expect("l is a geometry record");
    }

    #[test]
    fn a_missing_source_is_an_io_error() {
        // Detection only ever runs on content that was actually read.
        let err = convert("no-such-file-convert-test.obj").expect_err("missing source");
        assert!(err.contains("cannot read"), "{err}");
        assert!(!err.contains("unrecognized"), "{err}");
    }

    #[test]
    fn output_reparses_as_canonical_wrfm() {
        let text = obj_to_wrfm(TRI).expect("parses").to_string();
        assert!(
            text.starts_with("wrfm 1\nvertices 3   edges 3\n"),
            "canonical v1 header, got: {text}"
        );
        let back = WrfmModel::from_str("roundtrip", &text).expect("own output parses");
        assert_eq!(back.vertices.len(), 3);
        assert_eq!(back.edges, tri_edges());
    }
}
