//! `wrfm verify` — intent assertions. The agent declares what it WANTED
//! (size / center / closed / axis / symmetry / groups / redundant) and this
//! module reports pass/fail per expectation, the delta, and an actionable fix
//! command. Geometry facts come from `crate::geometry::analyze` (the single
//! authority); open edges and the redundant-vertex count come from
//! `crate::check`.

use crate::check::{bridges, degrees, redundant_vertices};
use crate::geometry::analyze;
use ratatui_wireframe::model::Model;
use serde_json::{Value, json};

/// Declared intent for one `wrfm verify` run.
pub struct VerifyOptions<'a> {
    /// Source description echoed into the report (`<path>` or `-`).
    pub source: &'a str,
    pub expect_size: Option<[f64; 3]>,
    pub expect_center: Option<[f64; 3]>,
    pub expect_closed: bool,
    /// "x" | "y" | "z" (validated by the caller).
    pub expect_axis: Option<&'a str>,
    /// Each letter "x" | "y" | "z" (validated by the caller).
    pub expect_symmetric: Vec<&'a str>,
    pub expect_groups: Vec<String>,
    /// Exact expected count of redundant (collinear degree-2) vertices —
    /// the accepted degenerate midpoints a `wrfm check` would still warn.
    pub expect_redundant: Option<usize>,
    /// Relative tolerance for numeric expectations (0.05 = 5%).
    pub tolerance: f64,
}

/// Compact number formatting: integers print without a decimal point, the
/// rest with up to 4 decimals (trailing zeros trimmed).
fn fmt_num(x: f64) -> String {
    if !x.is_finite() {
        return x.to_string();
    }
    if (x - x.round()).abs() < 1e-9 {
        format!("{}", x.round() as i64)
    } else {
        let s = format!("{x:.4}");
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    }
}

/// Read a JSON array of three numbers into `[f64; 3]`.
fn arr3(v: &Value) -> [f64; 3] {
    let mut out = [0.0; 3];
    if let Some(a) = v.as_array() {
        for (i, x) in a.iter().take(3).enumerate() {
            out[i] = x.as_f64().unwrap_or(0.0);
        }
    }
    out
}

/// Run the intent assertions and produce the verify report JSON.
pub fn verify(m: &Model, groups: &[wrfm::Group], opts: &VerifyOptions) -> Value {
    let g = analyze(m);
    let mut expectations: Vec<Value> = Vec::new();

    // --- size expectation ---
    if let Some(exp) = opts.expect_size {
        let actual = arr3(&g["bounds"]["size"]);
        let mut pass = true;
        let mut delta = [0.0; 3];
        for i in 0..3 {
            delta[i] = actual[i] - exp[i];
            if delta[i].abs() > opts.tolerance * exp[i].abs() {
                pass = false;
            }
        }
        let suggestion = if pass {
            String::new()
        } else {
            // One scale command per failing axis (expected/actual).
            let cmds: Vec<String> = ["x", "y", "z"]
                .iter()
                .enumerate()
                .filter(|(i, _)| {
                    (actual[*i] - exp[*i]).abs() > opts.tolerance * exp[*i].abs()
                        && exp[*i] != 0.0
                        && actual[*i] != 0.0
                })
                .map(|(i, axis)| {
                    format!(
                        "wrfm transform - --scale-{axis} {}",
                        fmt_num(exp[i] / actual[i])
                    )
                })
                .collect();
            cmds.join(" && ")
        };
        expectations.push(json!({
            "name": "size",
            "expected": exp,
            "actual": actual,
            "pass": pass,
            "delta": delta,
            "suggestion": suggestion,
        }));
    }

    // --- center expectation ---
    if let Some(exp) = opts.expect_center {
        let actual = arr3(&g["bounds"]["center"]);
        let mut pass = true;
        let mut delta = [0.0; 3];
        for i in 0..3 {
            delta[i] = actual[i] - exp[i];
            if delta[i].abs() > opts.tolerance * exp[i].abs() {
                pass = false;
            }
        }
        let suggestion = if pass {
            String::new()
        } else {
            let d = [exp[0] - actual[0], exp[1] - actual[1], exp[2] - actual[2]];
            format!(
                "wrfm transform - --translate {},{},{}",
                fmt_num(d[0]),
                fmt_num(d[1]),
                fmt_num(d[2])
            )
        };
        expectations.push(json!({
            "name": "center",
            "expected": exp,
            "actual": actual,
            "pass": pass,
            "delta": delta,
            "suggestion": suggestion,
        }));
    }

    // --- closed expectation (no bridges / open edges) ---
    if opts.expect_closed {
        let open = bridges(m);
        let pass = open.is_empty();
        let suggestion = if pass {
            String::new()
        } else {
            let pairs: Vec<String> = open.iter().map(|&(a, b)| format!("[{a}, {b}]")).collect();
            format!(
                "add closing edges between the listed pairs: {}",
                pairs.join(", ")
            )
        };
        expectations.push(json!({
            "name": "closed",
            "expected": 0,
            "actual": open.len(),
            "pass": pass,
            "delta": open.len(),
            "suggestion": suggestion,
        }));
    }

    // --- axis expectation ---
    if let Some(axis) = opts.expect_axis {
        let actual = g["alignment"]["longest_axis"].as_str().unwrap_or("");
        let pass = actual == axis;
        let suggestion = if pass {
            String::new()
        } else {
            format!("wrfm transform - --align {axis}")
        };
        expectations.push(json!({
            "name": "axis",
            "expected": axis,
            "actual": actual,
            "pass": pass,
            "suggestion": suggestion,
        }));
    }

    // --- symmetry expectations (one per requested plane) ---
    for &axis in &opts.expect_symmetric {
        // SUBTLE mapping + SEMANTICS: `analyze` names a mirror plane by the
        // axes it spans — center_mirror_yz is the plane perpendicular to x
        // through the model's BBOX CENTRE. verify uses the CENTRE (shape)
        // fields, so a symmetric model passes regardless of where it sits.
        // Expecting symmetry across x reads center_mirror_yz.
        let field = match axis {
            "x" => "center_mirror_yz",
            "y" => "center_mirror_xz",
            "z" => "center_mirror_xy",
            _ => "center_mirror_yz", // unreachable: the caller validates x|y|z
        };
        let pass = g["symmetry"][field].as_bool().unwrap_or(false);
        let suggestion = if pass {
            String::new()
        } else {
            "regenerate symmetrically — no single transform fixes symmetry".to_string()
        };
        expectations.push(json!({
            "name": format!("symmetric_{axis}"),
            "expected": true,
            "actual": pass,
            "pass": pass,
            "suggestion": suggestion,
        }));
    }

    // --- group expectations (first match by name) ---
    for want in &opts.expect_groups {
        let pass = groups.iter().any(|g| g.name == *want);
        let suggestion = if pass {
            String::new()
        } else {
            format!("add a 'group {want}' section")
        };
        expectations.push(json!({
            "name": format!("group_{want}"),
            "expected": true,
            "actual": pass,
            "pass": pass,
            "suggestion": suggestion,
        }));
    }

    // --- redundant-vertex expectation (accepted degenerate midpoints) ---
    if let Some(exp) = opts.expect_redundant {
        let deg = degrees(m);
        let actual = redundant_vertices(m, &deg).len();
        let pass = actual == exp;
        let suggestion = if pass {
            String::new()
        } else {
            format!("re-declare with the actual count: --expect-redundant {actual}")
        };
        expectations.push(json!({
            "name": "redundant",
            "expected": exp,
            "actual": actual,
            "pass": pass,
            "delta": actual as i64 - exp as i64,
            "suggestion": suggestion,
        }));
    }

    let total = expectations.len();
    let met = expectations
        .iter()
        .filter(|e| e["pass"].as_bool().unwrap_or(false))
        .count();
    let verdict = if total > 0 && met == total {
        "pass"
    } else {
        "fail"
    };
    json!({
        "source": opts.source,
        "expectations": expectations,
        "verdict": verdict,
        "summary": format!("{met} of {total} expectations met"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A cube spanning [-1,1]^3 — bbox SIZE [2,2,2], mirror-symmetric about
    /// every origin plane (the same builder as check.rs's cube).
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

    /// An axis-aligned box centred on the origin with the given size.
    fn box_of(sx: f64, sy: f64, sz: f64) -> Model {
        let mut verts = Vec::new();
        let mut edges = Vec::new();
        for i in 0..2 {
            for j in 0..2 {
                for k in 0..2 {
                    verts.push((
                        if i == 0 { -sx / 2.0 } else { sx / 2.0 },
                        if j == 0 { -sy / 2.0 } else { sy / 2.0 },
                        if k == 0 { -sz / 2.0 } else { sz / 2.0 },
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

    /// An open chain 0-1-2-3 (every edge is a bridge).
    fn chain() -> Model {
        Model {
            vertices: vec![
                (0.0, 0.0, 0.0),
                (1.0, 0.0, 0.0),
                (2.0, 0.0, 0.0),
                (3.0, 0.0, 0.0),
            ],
            edges: vec![(0, 1), (1, 2), (2, 3)],
        }
    }

    fn opts() -> VerifyOptions<'static> {
        VerifyOptions {
            source: "-",
            expect_size: None,
            expect_center: None,
            expect_closed: false,
            expect_axis: None,
            expect_symmetric: Vec::new(),
            expect_groups: Vec::new(),
            expect_redundant: None,
            tolerance: 0.05,
        }
    }

    #[test]
    fn cube_size_assertion_passes() {
        let mut o = opts();
        o.expect_size = Some([2.0, 2.0, 2.0]);
        let r = verify(&cube(), &[], &o);
        assert_eq!(r["verdict"], "pass");
        assert_eq!(r["expectations"][0]["pass"], true);
    }

    #[test]
    fn size_mismatch_reports_delta_and_suggestion() {
        let mut o = opts();
        o.expect_size = Some([2.0, 3.0, 4.0]);
        // A 2 x 1.5 x 4 box: only y misses the expectation.
        let r = verify(&box_of(2.0, 1.5, 4.0), &[], &o);
        assert_eq!(r["verdict"], "fail");
        assert_eq!(r["expectations"][0]["delta"], json!([0.0, -1.5, 0.0]));
        let s = r["expectations"][0]["suggestion"].as_str().unwrap();
        assert!(s.contains("--scale-y 2"), "suggestion: {s}");
    }

    #[test]
    fn closed_assertion_cube_passes() {
        let mut o = opts();
        o.expect_closed = true;
        let r = verify(&cube(), &[], &o);
        assert_eq!(r["verdict"], "pass");
        assert_eq!(r["expectations"][0]["actual"], 0);
    }

    #[test]
    fn closed_assertion_chain_fails() {
        let mut o = opts();
        o.expect_closed = true;
        let r = verify(&chain(), &[], &o);
        assert_eq!(r["verdict"], "fail");
        let e = &r["expectations"][0];
        assert_eq!(e["actual"], 3);
        let s = e["suggestion"].as_str().unwrap();
        assert!(s.contains("add closing edges"), "suggestion: {s}");
        assert!(s.contains("[0, 1]"), "suggestion: {s}");
    }

    #[test]
    fn axis_and_symmetry_reuse_geometry() {
        // x-longest box -> axis passes; y does not (align suggestion).
        let mut o = opts();
        o.expect_axis = Some("x");
        let r = verify(&box_of(4.0, 2.0, 2.0), &[], &o);
        assert_eq!(r["verdict"], "pass");
        o.expect_axis = Some("y");
        let r = verify(&box_of(4.0, 2.0, 2.0), &[], &o);
        assert_eq!(r["verdict"], "fail");
        assert!(
            r["expectations"][0]["suggestion"]
                .as_str()
                .unwrap()
                .contains("--align y")
        );

        // The centred cube is symmetric about its own centre on every axis.
        let mut o = opts();
        o.expect_symmetric = vec!["x", "y", "z"];
        let r = verify(&cube(), &[], &o);
        assert_eq!(r["verdict"], "pass");
        // Position-independence: shifting the cube +1.5 along x does NOT
        // break centre-plane symmetry (the shape is still symmetric).
        let mut shifted = cube();
        for v in shifted.vertices.iter_mut() {
            v.0 += 1.5;
        }
        let mut o2 = opts();
        o2.expect_symmetric = vec!["x", "y", "z"];
        let r2 = verify(&shifted, &[], &o2);
        assert_eq!(r2["verdict"], "pass");
    }

    #[test]
    fn redundant_assertion_accepts_the_declared_midpoints() {
        // A closed loop with one dead-straight midpoint: declaring it 1
        // passes; the exact count means a NEW midpoint fails the gate.
        let m = Model {
            vertices: vec![
                (0.0, 0.0, 0.0),
                (1.0, 0.0, 0.0),
                (2.0, 0.0, 0.0),
                (2.0, 1.0, 0.0),
                (0.0, 1.0, 0.0),
            ],
            edges: vec![(0, 1), (1, 2), (2, 3), (3, 4), (4, 0)],
        };
        let mut o = opts();
        o.expect_redundant = Some(1);
        let r = verify(&m, &[], &o);
        assert_eq!(r["verdict"], "pass");
        assert_eq!(r["expectations"][0]["name"], "redundant");
        assert_eq!(r["expectations"][0]["actual"], 1);

        o.expect_redundant = Some(0);
        let r = verify(&m, &[], &o);
        assert_eq!(r["verdict"], "fail");
        assert_eq!(r["expectations"][0]["delta"], 1);
        assert!(
            r["expectations"][0]["suggestion"]
                .as_str()
                .unwrap()
                .contains("--expect-redundant 1")
        );
    }

    #[test]
    fn redundant_assertion_ignores_real_corners() {
        // A bent chain: the middle vertex is degree 2 but turns a corner —
        // not redundant, so 0 passes (dangling ends are a check concern,
        // not a verify one).
        let m = Model {
            vertices: vec![(0.0, 0.0, 0.0), (1.0, 0.0, 0.0), (1.0, 1.0, 0.0)],
            edges: vec![(0, 1), (1, 2)],
        };
        let mut o = opts();
        o.expect_redundant = Some(0);
        let r = verify(&m, &[], &o);
        assert_eq!(r["verdict"], "pass");
        assert_eq!(r["expectations"][0]["actual"], 0);
    }

    #[test]
    fn groups_assertion() {
        let groups = vec![wrfm::Group {
            name: "body".to_string(),
            vertex_start: 0,
            vertex_end: 4,
        }];
        let mut o = opts();
        o.expect_groups = vec!["body".to_string(), "head".to_string()];
        let r = verify(&cube(), &groups, &o);
        assert_eq!(r["verdict"], "fail");
        let ex = r["expectations"].as_array().unwrap();
        assert_eq!(ex[0]["pass"], true); // body present
        assert_eq!(ex[1]["pass"], false); // head absent
        assert!(
            ex[1]["suggestion"].as_str().unwrap().contains("group head"),
            "suggestion: {}",
            ex[1]["suggestion"]
        );
    }
}
