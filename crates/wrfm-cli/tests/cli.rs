//! Integration tests for the `wrfm` CLI binary — the contract in
//! `docs/PLAN.md` (PLAN-cli-stream §3) is an exit-code + three-channel
//! contract, so these tests assert EXIT CODES and stream purity, not just
//! output text.
//!
//! Exit codes: 0 = ok/warn (result on stdout) · 1 = result produced but L2
//! check `broken` · 2 = no result (L1 parse / usage / I/O / unknown
//! argument). stdout carries the pure result; stderr carries `[wrfm]
//! check:` diagnostics (never mixed into stdout).
//!
//! Fixtures are FORMAT.md v1: a `wrfm 1` magic line and a
//! `vertices <V>   edges <M>` counts header.

use serde_json::Value;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_wrfm")
}

/// A valid v1 cube: magic + header + 8 vertices + 12 edges.
const CUBE: &str = "\
wrfm 1
vertices 8   edges 12

v 0 0 0
v 1 0 0
v 1 1 0
v 0 1 0
v 0 0 1
v 1 0 1
v 1 1 1
v 0 1 1
e 0 1
e 1 2
e 2 3
e 3 0
e 4 5
e 5 6
e 6 7
e 7 4
e 0 4
e 1 5
e 2 6
e 3 7
";

/// A valid v1 file with two groups: `body` (global vertices 0..3, a
/// triangle) and `head` (3..6, a triangle) — 6 vertices, 6 edges, every
/// vertex degree 2 (a `warn`-level health verdict, so these commands exit 0
/// and only print a stderr note).
const TWO_GROUPS: &str = "\
wrfm 1
vertices 6   edges 6

group body
  v 0 0 0
  v 1 0 0
  v 0 1 0
group head
  v 0 0 1
  v 1 0 1
  v 0.5 1 1
e 0 1
e 1 2
e 2 0
e 3 4
e 4 5
e 5 3
";

/// A model with a WARN-level issue: one duplicate vertex (twin of vertex 7).
const DUP_VERTEX: &str = "\
wrfm 1
vertices 9   edges 13

v 0 0 0
v 1 0 0
v 1 1 0
v 0 1 0
v 0 0 1
v 1 0 1
v 1 1 1
v 0 1 1
v 0 1 1
e 0 1
e 1 2
e 2 3
e 3 0
e 4 5
e 5 6
e 6 7
e 7 4
e 0 4
e 1 5
e 2 6
e 3 7
e 8 0
";

/// A model with a BROKEN issue: a zero-length edge (8,8).
const BROKEN: &str = "\
wrfm 1
vertices 8   edges 13

v 0 0 0
v 1 0 0
v 1 1 0
v 0 1 0
v 0 0 1
v 1 0 1
v 1 1 1
v 0 1 1
e 0 1
e 1 2
e 2 3
e 3 0
e 4 5
e 5 6
e 6 7
e 7 4
e 0 4
e 1 5
e 2 6
e 3 7
e 0 0
";

/// A valid v1 cube spanning [0,2]^3 (max span 2) — for normalize tests.
const CUBE2: &str = "\
wrfm 1
vertices 8   edges 12

v 0 0 0
v 2 0 0
v 2 2 0
v 0 2 0
v 0 0 2
v 2 0 2
v 2 2 2
v 0 2 2
e 0 1
e 1 2
e 2 3
e 3 0
e 4 5
e 5 6
e 6 7
e 7 4
e 0 4
e 1 5
e 2 6
e 3 7
";

/// CUBE translated by +5 in x: no shared vertices with [`CUBE`], so a
/// merge of the two is duplicate-free (fully healthy).
const CUBE_SHIFTED: &str = "\
wrfm 1
vertices 8   edges 12

v 5 0 0
v 6 0 0
v 6 1 0
v 5 1 0
v 5 0 1
v 6 0 1
v 6 1 1
v 5 1 1
e 0 1
e 1 2
e 2 3
e 3 0
e 4 5
e 5 6
e 6 7
e 7 4
e 0 4
e 1 5
e 2 6
e 3 7
";

/// A valid v1 box spanning x in [1,3], y/z in [0,1] (bbox centre
/// (2,0.5,0.5)) — the PLAN §5.2 "cube at x 1..3".
const CUBE_X13: &str = "\
wrfm 1
vertices 8   edges 12

v 1 0 0
v 3 0 0
v 3 1 0
v 1 1 0
v 1 0 1
v 3 0 1
v 3 1 1
v 1 1 1
e 0 1
e 1 2
e 2 3
e 3 0
e 4 5
e 5 6
e 6 7
e 7 4
e 0 4
e 1 5
e 2 6
e 3 7
";

/// A valid v1 1x1x4 box stretched along +z (z-span 4, x/y-span 1) — the
/// PLAN §5.2 "long-z model" (PCA longest axis exactly +z).
const LONG_Z: &str = "\
wrfm 1
vertices 8   edges 12

v 0 0 0
v 0 0 4
v 1 0 0
v 1 0 4
v 0 1 0
v 0 1 4
v 1 1 0
v 1 1 4
e 0 1
e 2 3
e 4 5
e 6 7
e 0 2
e 1 3
e 4 6
e 5 7
e 0 4
e 1 5
e 2 6
e 3 7
";

/// A tetrahedron (all degree-3 vertices -> a fully clean model) plus
/// degenerate junk: an isolated vertex (3), a dangling chain (4-5) and a
/// zero-length edge (7,7). `--clean` must strip all of it.
const CLEAN_ME: &str = "\
wrfm 1
vertices 8   edges 8

v 1 1 1
v 1 -1 -1
v -1 1 -1
v -1 -1 1
v 5 5 5
v 2 0 0
v 3 0 0
v 4 4 4
e 0 1
e 0 2
e 0 3
e 1 2
e 2 3
e 3 1
e 5 6
e 7 7
";

/// A model with a duplicate vertex (2 == 1), a duplicate edge (1,2) twice,
/// and a zero-length edge (0,0). `--dedupe` must reduce it to a single edge.
const DUP_MODEL: &str = "\
wrfm 1
vertices 3   edges 4

v 0 0 0
v 1 0 0
v 1 0 0
e 0 1
e 1 2
e 0 0
e 1 2
";

/// A chain 0-1-2 plus an isolated vertex 3 (connectivity fixture).
const CHAIN: &str = "\
wrfm 1
vertices 4   edges 2

v 0 0 0
v 1 0 0
v 2 0 0
v 5 5 5
e 0 1
e 1 2
";

/// Two groups (body 0..3, head 3..6) with one cross edge (1,4): the
/// `adjacent_groups` fixture.
const BODY_HEAD: &str = "\
wrfm 1
vertices 6   edges 7

group body
  v 0 0 0
  v 1 0 0
  v 0 1 0
group head
  v 0 0 1
  v 1 0 1
  v 0.5 1 1
e 0 1
e 1 2
e 2 0
e 3 4
e 4 5
e 5 3
e 1 4
";

/// A per-test scratch directory under the system temp dir.
fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("wrfm-cli-{}-{tag}", std::process::id()));
    fs::create_dir_all(&dir).expect("create scratch dir");
    dir
}

fn write(dir: &Path, name: &str, content: &str) -> PathBuf {
    let path = dir.join(name);
    fs::write(&path, content).expect("write temp file");
    path
}

/// Run with optional stdin text.
fn run(args: &[&str]) -> Output {
    Command::new(bin()).args(args).output().expect("run wrfm")
}

fn run_stdin(args: &[&str], stdin: &str) -> Output {
    let mut child = Command::new(bin())
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn wrfm");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(stdin.as_bytes())
        .expect("write stdin");
    child.wait_with_output().expect("wait wrfm")
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

fn stdout_json(o: &Output) -> Value {
    serde_json::from_str(stdout(o).trim()).expect("stdout is JSON")
}

/// Bounding box (min, max) of a parsed .wrfm model's vertices.
fn bbox_of(m: &wrfm::WrfmModel) -> ([f64; 3], [f64; 3]) {
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
    (min, max)
}

// ---------------------------------------------------------------------------
// check
// ---------------------------------------------------------------------------

#[test]
fn check_ok_exit_zero() {
    let dir = scratch("check_ok_exit_zero");
    let path = write(&dir, "cube.wrfm", CUBE);
    let out = run(&["check", path.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    let so = stdout(&out);
    assert!(
        so.contains("ok: cube (8 vertices, 12 edges)"),
        "stdout: {so}"
    );
    // The report is the result: stderr stays empty.
    assert!(stderr(&out).is_empty(), "stderr: {}", stderr(&out));
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn check_warn_exit_one() {
    let dir = scratch("check_warn_exit_one");
    let path = write(&dir, "dup.wrfm", DUP_VERTEX);
    let out = run(&["check", path.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(1), "stderr: {}", stderr(&out));
    let so = stdout(&out);
    assert!(
        so.starts_with("warn: dup (9 vertices, 13 edges)"),
        "stdout: {so}"
    );
    assert!(so.contains("duplicate vertices"), "stdout: {so}");
    assert!(stderr(&out).is_empty(), "stderr: {}", stderr(&out));
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn check_broken_exit_one() {
    let dir = scratch("check_broken_exit_one");
    let path = write(&dir, "broken.wrfm", BROKEN);
    let out = run(&["check", path.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(1), "stderr: {}", stderr(&out));
    let so = stdout(&out);
    assert!(
        so.starts_with("broken: broken (8 vertices, 13 edges)"),
        "stdout: {so}"
    );
    assert!(so.contains("zero-length edges"), "stdout: {so}");
    assert!(stderr(&out).is_empty(), "stderr: {}", stderr(&out));
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn check_parse_error_exit_two() {
    // NEW contract (PLAN §3): L1 parse failure → exit 2, no stdout result.
    let dir = scratch("check_parse_error_exit_two");
    let path = write(
        &dir,
        "bad.wrfm",
        "wrfm 1\nvertices 1   edges 0\n\nv 1.0 2.0 abc\n",
    );
    let out = run(&["check", path.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(2), "stderr: {}", stderr(&out));
    assert!(stdout(&out).is_empty(), "no result on L1 failure");
    let se = stderr(&out);
    assert!(se.contains("line") && se.contains("column"), "stderr: {se}");
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn check_out_of_range_exit_two() {
    // Out-of-range edges are L1 (parser) failures now → exit 2.
    let dir = scratch("check_out_of_range_exit_two");
    let path = write(
        &dir,
        "oor.wrfm",
        "wrfm 1\nvertices 2   edges 1\n\nv 0 0 0\nv 1 1 1\ne 0 5\n",
    );
    let out = run(&["check", path.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(2), "stderr: {}", stderr(&out));
    assert!(
        stderr(&out).contains("out of range"),
        "stderr: {}",
        stderr(&out)
    );
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn check_io_error_exit_two() {
    let missing =
        std::env::temp_dir().join(format!("wrfm-cli-missing-{}.wrfm", std::process::id()));
    let _ = fs::remove_file(&missing);
    let out = run(&["check", missing.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(2));
    assert!(
        stderr(&out).contains("cannot read"),
        "stderr: {}",
        stderr(&out)
    );
    assert!(stdout(&out).is_empty());
}

#[test]
fn check_stdin() {
    let out = run_stdin(&["check", "-"], CUBE);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    assert!(
        stdout(&out).contains("ok: stdin (8 vertices, 12 edges)"),
        "stdout: {}",
        stdout(&out)
    );
    assert!(stderr(&out).is_empty(), "stderr: {}", stderr(&out));
}

#[test]
fn check_stdin_garbage_exit_two() {
    let out = run_stdin(&["check", "-"], "not a wrfm file at all\n");
    assert_eq!(out.status.code(), Some(2));
    assert!(stdout(&out).is_empty());
}

#[test]
fn check_usage_exit_two() {
    let out = run(&["check"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(!stderr(&out).is_empty(), "clap prints a usage error");
}

#[test]
fn check_group_scope() {
    // --group scopes the L2 check to the part: the grouped model is a
    // `warn` whole-model verdict (degree-2 vertices) but each triangle
    // part is healthy — wait, degree-2 is a warning in both scopes. Instead
    // assert the scoped report carries the PART's counts.
    let dir = scratch("check_group_scope");
    let path = write(&dir, "two.wrfm", TWO_GROUPS);
    let out = run(&["check", path.to_str().unwrap(), "--group", "body"]);
    assert_eq!(out.status.code(), Some(1), "degree-2 warns -> exit 1");
    let so = stdout(&out);
    assert!(
        so.starts_with("warn: two (3 vertices, 3 edges)"),
        "scoped counts: {so}"
    );
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn check_strict_upgrades_warn_to_broken() {
    // --strict turns warning-level issues (duplicate vertex) into broken.
    let dir = scratch("check_strict_upgrades_warn_to_broken");
    let path = write(&dir, "dup.wrfm", DUP_VERTEX);
    let out = run(&["check", path.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(1)); // warn -> 1
    let so = stdout(&out);
    assert!(so.starts_with("warn: dup"), "normal: {so}");
    let out2 = run(&["check", path.to_str().unwrap(), "--strict"]);
    assert_eq!(out2.status.code(), Some(1), "strict broken still exits 1");
    let so2 = stdout(&out2);
    assert!(
        so2.starts_with("broken: dup"),
        "strict upgrades to broken: {so2}"
    );
    fs::remove_dir_all(&dir).ok();
}

// ---------------------------------------------------------------------------
// info
// ---------------------------------------------------------------------------

#[test]
fn info_ok_json() {
    let dir = scratch("info_ok_json");
    let path = write(&dir, "cube.wrfm", CUBE);
    let out = run(&["info", path.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    // stdout purity: ok model → stderr stays empty.
    assert!(stderr(&out).is_empty(), "stderr: {}", stderr(&out));
    let j = stdout_json(&out);
    assert_eq!(j["name"], "cube");
    assert_eq!(j["source"], path.to_str().unwrap());
    assert_eq!(j["version"], 1);
    assert_eq!(j["vertices"], 8);
    assert_eq!(j["edges"], 12);
    assert_eq!(j["groups"].as_array().unwrap().len(), 0);
    assert_eq!(j["bounds"]["min"][0], 0.0);
    assert_eq!(j["bounds"]["max"][0], 1.0);
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn info_reports_groups_in_json() {
    let dir = scratch("info_reports_groups_in_json");
    let path = write(&dir, "two.wrfm", TWO_GROUPS);
    let out = run(&["info", path.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    let j = stdout_json(&out);
    assert_eq!(j["version"], 1);
    assert_eq!(j["vertices"], 6);
    assert_eq!(j["edges"], 6);
    let g = j["groups"].as_array().unwrap();
    assert_eq!(g.len(), 2);
    assert_eq!(g[0]["name"], "body");
    assert_eq!(g[0]["vertex_start"], 0);
    assert_eq!(g[0]["vertex_end"], 3);
    assert_eq!(g[1]["name"], "head");
    assert_eq!(g[1]["vertex_end"], 6);
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn info_broken_exit_one_with_stderr_note() {
    let dir = scratch("info_broken_exit_one");
    let path = write(&dir, "broken.wrfm", BROKEN);
    let out = run(&["info", path.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(1), "stderr: {}", stderr(&out));
    // The JSON result is STILL emitted on stdout.
    let j = stdout_json(&out);
    assert_eq!(j["vertices"], 8);
    // The check report goes to stderr, never stdout.
    let se = stderr(&out);
    assert!(se.contains("[wrfm] check: broken"), "stderr: {se}");
    assert!(
        !stdout(&out).contains("[wrfm] check:"),
        "stdout must stay pure"
    );
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn info_parse_error_exit_two() {
    let dir = scratch("info_parse_error_exit_two");
    let path = write(
        &dir,
        "bad.wrfm",
        "wrfm 1\nvertices 1   edges 0\n\nv 1.0 2.0 abc\n",
    );
    let out = run(&["info", path.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(2));
    assert!(stdout(&out).is_empty());
    assert!(stderr(&out).contains("line"), "stderr: {}", stderr(&out));
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn info_stdin() {
    let out = run_stdin(&["info", "-"], CUBE);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    let j = stdout_json(&out);
    assert_eq!(j["name"], "stdin");
    assert_eq!(j["source"], "-");
    assert_eq!(j["vertices"], 8);
    assert!(stderr(&out).is_empty());
}

// ---------------------------------------------------------------------------
// group
// ---------------------------------------------------------------------------

#[test]
fn group_all_groups_json() {
    let dir = scratch("group_all_groups_json");
    let path = write(&dir, "two.wrfm", TWO_GROUPS);
    let out = run(&["group", path.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    let j = stdout_json(&out);
    assert_eq!(j["name"], "two");
    let g = j["groups"].as_array().unwrap();
    assert_eq!(g.len(), 2);
    assert_eq!(g[0]["name"], "body");
    assert_eq!(g[0]["vertex_count"], 3);
    assert_eq!(g[1]["name"], "head");
    assert_eq!(g[1]["vertex_count"], 3);
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn group_named() {
    let dir = scratch("group_named");
    let path = write(&dir, "two.wrfm", TWO_GROUPS);
    let out = run(&["group", path.to_str().unwrap(), "head"]);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    let j = stdout_json(&out);
    let g = j["groups"].as_array().unwrap();
    assert_eq!(g.len(), 1);
    assert_eq!(g[0]["name"], "head");
    assert_eq!(g[0]["bounds"]["max"][1], 1.0);
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn group_unknown_exit_two() {
    let dir = scratch("group_unknown_exit_two");
    let path = write(&dir, "two.wrfm", TWO_GROUPS);
    let out = run(&["group", path.to_str().unwrap(), "nope"]);
    assert_eq!(out.status.code(), Some(2), "stderr: {}", stderr(&out));
    assert!(stdout(&out).is_empty());
    assert!(
        stderr(&out).contains("not found"),
        "stderr: {}",
        stderr(&out)
    );
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn group_reports_adjacent_groups() {
    let dir = scratch("group_reports_adjacent_groups");
    let path = write(&dir, "bh.wrfm", BODY_HEAD);
    let out = run(&["group", path.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    let j = stdout_json(&out);
    let g = j["groups"].as_array().unwrap();
    assert_eq!(g[0]["name"], "body");
    assert_eq!(g[0]["adjacent_groups"], serde_json::json!({"head": 1}));
    assert_eq!(g[1]["name"], "head");
    assert_eq!(g[1]["adjacent_groups"], serde_json::json!({"body": 1}));
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn group_adjacent_groups_empty_without_cross_edges() {
    let dir = scratch("group_adjacent_groups_empty_without_cross_edges");
    let path = write(&dir, "two.wrfm", TWO_GROUPS);
    let out = run(&["group", path.to_str().unwrap(), "body"]);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    let j = stdout_json(&out);
    assert_eq!(j["groups"][0]["adjacent_groups"], serde_json::json!({}));
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn group_no_groups_empty_list() {
    let dir = scratch("group_no_groups_empty_list");
    let path = write(&dir, "cube.wrfm", CUBE);
    let out = run(&["group", path.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(0));
    let j = stdout_json(&out);
    assert_eq!(j["groups"].as_array().unwrap().len(), 0);
    fs::remove_dir_all(&dir).ok();
}

// ---------------------------------------------------------------------------
// geometry
// ---------------------------------------------------------------------------

#[test]
fn geometry_json_has_spans() {
    let dir = scratch("geometry_json_has_spans");
    let path = write(&dir, "cube.wrfm", CUBE);
    let out = run(&["geometry", path.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    let j = stdout_json(&out);
    // New top-level spans field (PLAN §4.4).
    assert_eq!(
        j["spans"],
        serde_json::json!({"x": 1.0, "y": 1.0, "z": 1.0})
    );
    assert_eq!(j["bounds"]["size"], serde_json::json!([1.0, 1.0, 1.0]));
    assert_eq!(j["topology"]["vertices"], 8);
    assert_eq!(j["topology"]["edges"], 12);
    assert!(
        j["principal_axes"].get("eigenvalues").is_none(),
        "base report has no eigenvalues"
    );
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn geometry_full_has_eigenvalues() {
    let dir = scratch("geometry_full_has_eigenvalues");
    let path = write(&dir, "cube.wrfm", CUBE);
    let out = run(&["geometry", path.to_str().unwrap(), "--full"]);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    let j = stdout_json(&out);
    assert_eq!(j["spans"]["x"], 1.0);
    let ev = j["principal_axes"]["eigenvalues"].as_array().unwrap();
    assert_eq!(ev.len(), 3);
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn geometry_broken_exit_one() {
    let dir = scratch("geometry_broken_exit_one");
    let path = write(&dir, "broken.wrfm", BROKEN);
    let out = run(&["geometry", path.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(1), "stderr: {}", stderr(&out));
    let j = stdout_json(&out); // result still emitted
    assert_eq!(j["topology"]["edges"], 13);
    assert!(
        stderr(&out).contains("[wrfm] check: broken"),
        "stderr: {}",
        stderr(&out)
    );
    fs::remove_dir_all(&dir).ok();
}

// ---------------------------------------------------------------------------
// query
// ---------------------------------------------------------------------------

#[test]
fn query_extents_of_cube() {
    let dir = scratch("query_extents_of_cube");
    let path = write(&dir, "cube.wrfm", CUBE);
    let out = run(&["query", path.to_str().unwrap(), "extents"]);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    let so = stdout(&out);
    assert!(so.contains("min=[0.000,0.000,0.000]"), "stdout: {so}");
    assert!(so.contains("max=[1.000,1.000,1.000]"), "stdout: {so}");
    assert!(so.contains("span x=1.000 y=1.000 z=1.000"), "stdout: {so}");
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn query_topology_of_cube() {
    let dir = scratch("query_topology_of_cube");
    let path = write(&dir, "cube.wrfm", CUBE);
    let out = run(&["query", path.to_str().unwrap(), "topology"]);
    assert_eq!(out.status.code(), Some(0));
    let so = stdout(&out);
    assert!(so.contains("vertices=8 edges=12"), "stdout: {so}");
    assert!(so.contains("connected_components=1"), "stdout: {so}");
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn query_cross_section() {
    let dir = scratch("query_cross_section");
    let path = write(&dir, "cube.wrfm", CUBE);
    let out = run(&[
        "query",
        path.to_str().unwrap(),
        "cross_section",
        "--at",
        "0.5",
    ]);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    assert!(
        stdout(&out).contains("edges cross"),
        "stdout: {}",
        stdout(&out)
    );
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn query_group_scope() {
    let dir = scratch("query_group_scope");
    let path = write(&dir, "two.wrfm", TWO_GROUPS);
    let out = run(&[
        "query",
        path.to_str().unwrap(),
        "topology",
        "--group",
        "body",
    ]);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    let so = stdout(&out);
    assert!(so.contains("vertices=3"), "group-scoped query: {so}");
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn query_vertices_range() {
    let dir = scratch("query_vertices_range");
    let path = write(&dir, "cube.wrfm", CUBE);
    let out = run(&[
        "query",
        path.to_str().unwrap(),
        "vertices",
        "--range",
        "0,3",
    ]);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    let j = stdout_json(&out);
    let vs = j["vertices"].as_array().unwrap();
    let idx: Vec<usize> = vs
        .iter()
        .map(|v| v["index"].as_u64().unwrap() as usize)
        .collect();
    assert_eq!(idx, vec![0, 1, 2, 3]);
    assert_eq!(vs[0]["x"], 0.0);
    assert_eq!(vs[3]["z"], 0.0);
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn query_vertices_all() {
    let dir = scratch("query_vertices_all");
    let path = write(&dir, "cube.wrfm", CUBE);
    let out = run(&["query", path.to_str().unwrap(), "vertices"]);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    let j = stdout_json(&out);
    assert_eq!(j["vertices"].as_array().unwrap().len(), 8);
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn query_vertices_group() {
    let dir = scratch("query_vertices_group");
    let path = write(&dir, "two.wrfm", TWO_GROUPS);
    let out = run(&[
        "query",
        path.to_str().unwrap(),
        "vertices",
        "--group",
        "body",
    ]);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    let j = stdout_json(&out);
    let vs = j["vertices"].as_array().unwrap();
    let idx: Vec<usize> = vs
        .iter()
        .map(|v| v["index"].as_u64().unwrap() as usize)
        .collect();
    assert_eq!(
        idx,
        vec![0, 1, 2],
        "group-scoped vertices keep GLOBAL indices"
    );
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn query_vertices_range_and_group_errors() {
    let dir = scratch("query_vertices_range_and_group_errors");
    let path = write(&dir, "two.wrfm", TWO_GROUPS);
    let out = run(&[
        "query",
        path.to_str().unwrap(),
        "vertices",
        "--range",
        "0,1",
        "--group",
        "body",
    ]);
    assert_eq!(out.status.code(), Some(2));
    assert!(stdout(&out).is_empty());
    assert!(stderr(&out).contains("OR"), "stderr: {}", stderr(&out));
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn query_vertices_range_out_of_range_errors() {
    let dir = scratch("query_vertices_range_out_of_range_errors");
    let path = write(&dir, "cube.wrfm", CUBE);
    let out = run(&[
        "query",
        path.to_str().unwrap(),
        "vertices",
        "--range",
        "0,99",
    ]);
    assert_eq!(out.status.code(), Some(2));
    assert!(
        stderr(&out).contains("out of range"),
        "stderr: {}",
        stderr(&out)
    );
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn query_distance() {
    let dir = scratch("query_distance");
    let path = write(&dir, "cube.wrfm", CUBE);
    // CUBE spans [0,1]^3: vertices 0 (0,0,0) and 1 (1,0,0) -> distance 1.
    let out = run(&[
        "query",
        path.to_str().unwrap(),
        "distance",
        "--from",
        "0",
        "--to",
        "1",
    ]);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    let j = stdout_json(&out);
    assert_eq!(j["distance"], 1.0);
    assert_eq!(
        (j["from"].as_u64().unwrap(), j["to"].as_u64().unwrap()),
        (0, 1)
    );
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn query_distance_out_of_range() {
    let dir = scratch("query_distance_out_of_range");
    let path = write(&dir, "cube.wrfm", CUBE);
    let out = run(&[
        "query",
        path.to_str().unwrap(),
        "distance",
        "--from",
        "99",
        "--to",
        "1",
    ]);
    assert_eq!(out.status.code(), Some(2));
    assert!(stdout(&out).is_empty());
    assert!(
        stderr(&out).contains("out of range"),
        "stderr: {}",
        stderr(&out)
    );
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn query_distance_missing_args() {
    let dir = scratch("query_distance_missing_args");
    let path = write(&dir, "cube.wrfm", CUBE);
    let out = run(&["query", path.to_str().unwrap(), "distance", "--from", "0"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(
        stderr(&out).contains("requires --to"),
        "stderr: {}",
        stderr(&out)
    );
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn query_connectivity_connected_and_not() {
    let dir = scratch("query_connectivity_connected_and_not");
    let path = write(&dir, "chain.wrfm", CHAIN);
    let yes = run(&[
        "query",
        path.to_str().unwrap(),
        "connectivity",
        "--from",
        "0",
        "--to",
        "2",
    ]);
    // The chain fixture is L2-broken (dangling + isolated) -> exit 1, but
    // the JSON result is still emitted.
    assert_eq!(yes.status.code(), Some(1), "stderr: {}", stderr(&yes));
    assert_eq!(stdout_json(&yes)["connected"], true);
    let no = run(&[
        "query",
        path.to_str().unwrap(),
        "connectivity",
        "--from",
        "0",
        "--to",
        "3",
    ]);
    assert_eq!(no.status.code(), Some(1), "stderr: {}", stderr(&no));
    assert_eq!(stdout_json(&no)["connected"], false);
    // A == B is trivially connected.
    let same = run(&[
        "query",
        path.to_str().unwrap(),
        "connectivity",
        "--from",
        "3",
        "--to",
        "3",
    ]);
    assert_eq!(stdout_json(&same)["connected"], true);
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn query_unknown_exit_two() {
    let dir = scratch("query_unknown_exit_two");
    let path = write(&dir, "cube.wrfm", CUBE);
    let out = run(&["query", path.to_str().unwrap(), "nope"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(stdout(&out).is_empty());
    assert!(
        stderr(&out).contains("unknown query"),
        "stderr: {}",
        stderr(&out)
    );
    fs::remove_dir_all(&dir).ok();
}

// ---------------------------------------------------------------------------
// view
// ---------------------------------------------------------------------------

#[test]
fn view_front_occludes_back_edges() {
    let dir = scratch("view_front_occludes_back_edges");
    let path = write(&dir, "cube.wrfm", CUBE);
    // Explicit camera (dist 8): the 0..1 cube's near face (z=1) is larger on
    // screen, so the far face + connector edges are occluded by it.
    let out = run(&[
        "view",
        path.to_str().unwrap(),
        "--auto-dist",
        "false",
        "--dist",
        "8",
    ]);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    let j = stdout_json(&out);
    assert_eq!(j["totals"]["edges"], 12);
    assert_eq!(j["totals"]["behind_camera"], 0);
    // Every edge is either visible or occluded; the z-buffer pass splits them.
    let visible = j["totals"]["visible"].as_u64().unwrap();
    let occluded = j["totals"]["occluded"].as_u64().unwrap();
    assert_eq!(
        visible + occluded,
        12,
        "visible={visible} occluded={occluded}"
    );
    assert!(occluded >= 4, "the far face is hidden behind the near face");
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn view_group_scope() {
    let dir = scratch("view_group_scope");
    let path = write(&dir, "two.wrfm", TWO_GROUPS);
    let out = run(&["view", path.to_str().unwrap(), "--group", "body"]);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    let j = stdout_json(&out);
    assert_eq!(j["totals"]["edges"], 3); // the body triangle
    fs::remove_dir_all(&dir).ok();
}

// ---------------------------------------------------------------------------
// render
// ---------------------------------------------------------------------------

#[test]
fn render_grid_six_views() {
    let dir = scratch("render_grid_six_views");
    let path = write(&dir, "cube.wrfm", CUBE);
    let out = run(&["render", path.to_str().unwrap(), "--format", "grid"]);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    let so = stdout(&out);
    for v in ["front", "back", "left", "right", "top", "bottom"] {
        assert!(
            so.contains(&format!("[view={v}]")),
            "missing view {v} in:\n{so}"
        );
    }
    assert!(so.contains("# grid"), "density grid header:\n{so}");
    assert!(stderr(&out).is_empty(), "ok render → no stderr note");
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn render_budget_exit_two() {
    let dir = scratch("render_budget_exit_two");
    let path = write(&dir, "cube.wrfm", CUBE);
    let out = run(&[
        "render",
        path.to_str().unwrap(),
        "--format",
        "braille",
        "--budget",
        "1",
    ]);
    assert_eq!(out.status.code(), Some(2), "stderr: {}", stderr(&out));
    assert!(stdout(&out).is_empty());
    assert!(stderr(&out).contains("budget"), "stderr: {}", stderr(&out));
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn render_broken_exit_one_still_emits() {
    let dir = scratch("render_broken_exit_one");
    let path = write(&dir, "broken.wrfm", BROKEN);
    let out = run(&["render", path.to_str().unwrap(), "--format", "grid"]);
    assert_eq!(out.status.code(), Some(1), "stderr: {}", stderr(&out));
    assert!(
        stdout(&out).contains("# grid"),
        "render still emitted:\n{}",
        stdout(&out)
    );
    assert!(
        stderr(&out).contains("[wrfm] check: broken"),
        "stderr: {}",
        stderr(&out)
    );
    fs::remove_dir_all(&dir).ok();
}

// ---------------------------------------------------------------------------
// transform
// ---------------------------------------------------------------------------

#[test]
fn transform_scale_prints_model_text() {
    let dir = scratch("transform_scale_prints_model_text");
    let path = write(&dir, "cube.wrfm", CUBE);
    let out = run(&["transform", path.to_str().unwrap(), "--scale", "2"]);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    let so = stdout(&out);
    assert!(so.starts_with("wrfm 1\n"), "canonical magic:\n{so}");
    // The result is a valid model: round-trip through the parser.
    let back = wrfm::WrfmModel::from_str("scaled", &so).expect("output parses");
    assert_eq!(back.vertices.len(), 8);
    assert!(
        back.vertices
            .iter()
            .all(|&(x, y, _)| (0.0..=2.0).contains(&x) && (0.0..=2.0).contains(&y))
    );
    // Transform always reports the check verdict on stderr (even ok).
    assert!(
        stderr(&out).contains("[wrfm] check: ok"),
        "stderr: {}",
        stderr(&out)
    );
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn transform_stdin() {
    let out = run_stdin(&["transform", "-", "--scale", "2"], CUBE);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    let so = stdout(&out);
    assert!(so.starts_with("wrfm 1\n"), "stdout:\n{so}");
    fs::remove_dir_all(std::env::temp_dir().join("x")).ok();
}

#[test]
fn transform_broken_exit_one_result_emitted() {
    let dir = scratch("transform_broken_exit_one");
    let path = write(&dir, "broken.wrfm", BROKEN);
    let out = run(&["transform", path.to_str().unwrap(), "--scale", "1"]);
    assert_eq!(out.status.code(), Some(1), "stderr: {}", stderr(&out));
    // Mechanism A: the result is STILL emitted even when broken.
    assert!(
        stdout(&out).starts_with("wrfm 1\n"),
        "stdout:\n{}",
        stdout(&out)
    );
    assert!(
        stderr(&out).contains("[wrfm] check: broken"),
        "stderr: {}",
        stderr(&out)
    );
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn transform_bad_translate_exit_two() {
    let dir = scratch("transform_bad_translate_exit_two");
    let path = write(&dir, "cube.wrfm", CUBE);
    let out = run(&["transform", path.to_str().unwrap(), "--translate", "1,2"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(stdout(&out).is_empty());
    assert!(
        stderr(&out).contains("translate"),
        "stderr: {}",
        stderr(&out)
    );
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn transform_bad_mirror_exit_two() {
    let dir = scratch("transform_bad_mirror_exit_two");
    let path = write(&dir, "cube.wrfm", CUBE);
    let out = run(&["transform", path.to_str().unwrap(), "--mirror", "w"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(stderr(&out).contains("mirror"), "stderr: {}", stderr(&out));
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn transform_zero_axis_exit_two() {
    let dir = scratch("transform_zero_axis_exit_two");
    let path = write(&dir, "cube.wrfm", CUBE);
    let out = run(&[
        "transform",
        path.to_str().unwrap(),
        "--rotate-axis",
        "0,0,0",
    ]);
    assert_eq!(out.status.code(), Some(2));
    assert!(
        stderr(&out).contains("non-zero"),
        "stderr: {}",
        stderr(&out)
    );
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn transform_anisotropic_scale_end_to_end() {
    let dir = scratch("transform_anisotropic_scale_end_to_end");
    let path = write(&dir, "cube.wrfm", CUBE);
    let out = run(&["transform", path.to_str().unwrap(), "--scale-y", "2"]);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    let so = stdout(&out);
    let back = wrfm::WrfmModel::from_str("scaled", &so).expect("output parses");
    let (min, max) = bbox_of(&back);
    assert!(
        (max[1] - min[1] - 2.0).abs() < 1e-9,
        "y-span must double to 2, got {}",
        max[1] - min[1]
    );
    assert!((max[0] - min[0] - 1.0).abs() < 1e-9, "x-span unchanged");
    assert!((max[2] - min[2] - 1.0).abs() < 1e-9, "z-span unchanged");
    // `wrfm info` agrees: pipe the result back in.
    let info = run_stdin(&["info", "-"], &so);
    assert_eq!(info.status.code(), Some(0), "stderr: {}", stderr(&info));
    let j = stdout_json(&info);
    let yspan = j["bounds"]["max"][1].as_f64().unwrap() - j["bounds"]["min"][1].as_f64().unwrap();
    assert!((yspan - 2.0).abs() < 1e-9, "info y-span {yspan}");
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn transform_shear_end_to_end() {
    let dir = scratch("transform_shear_end_to_end");
    let path = write(&dir, "cube.wrfm", CUBE);
    let out = run(&["transform", path.to_str().unwrap(), "--shear-xy", "1"]);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    let back = wrfm::WrfmModel::from_str("sheared", &stdout(&out)).expect("output parses");
    // PLAN §5.1: cube vertex (1,1,0) -> x' = x + 1·y = (2,1,0).
    assert!(
        back.vertices.contains(&(2.0, 1.0, 0.0)),
        "vertex (1,1,0) must shear to (2,1,0): {:?}",
        back.vertices
    );
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn transform_pivot_center_rotate() {
    let dir = scratch("transform_pivot_center_rotate");
    let path = write(&dir, "cube.wrfm", CUBE);
    let out = run(&[
        "transform",
        path.to_str().unwrap(),
        "--pivot",
        "bbox",
        "--rotate-z",
        "90",
    ]);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    let back = wrfm::WrfmModel::from_str("rotated", &stdout(&out)).expect("output parses");
    // Rotating a [0,1]^3 cube 90deg about its own bbox centre maps the
    // bbox onto itself.
    let (min, max) = bbox_of(&back);
    assert!(
        (min[0] - 0.0).abs() < 1e-9 && (max[0] - 1.0).abs() < 1e-9,
        "x bbox"
    );
    assert!(
        (min[1] - 0.0).abs() < 1e-9 && (max[1] - 1.0).abs() < 1e-9,
        "y bbox"
    );
    assert!(
        (min[2] - 0.0).abs() < 1e-9 && (max[2] - 1.0).abs() < 1e-9,
        "z bbox"
    );
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn transform_center_end_to_end() {
    let dir = scratch("transform_center_end_to_end");
    let path = write(&dir, "cube13.wrfm", CUBE_X13);
    let out = run(&["transform", path.to_str().unwrap(), "--center"]);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    let back = wrfm::WrfmModel::from_str("centered", &stdout(&out)).expect("output parses");
    let (min, max) = bbox_of(&back);
    let cx = (min[0] + max[0]) / 2.0;
    let cy = (min[1] + max[1]) / 2.0;
    let cz = (min[2] + max[2]) / 2.0;
    assert!(
        cx.abs() < 1e-9 && cy.abs() < 1e-9 && cz.abs() < 1e-9,
        "bbox centre must land on the origin, got ({cx},{cy},{cz})"
    );
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn transform_align_end_to_end() {
    let dir = scratch("transform_align_end_to_end");
    let path = write(&dir, "longz.wrfm", LONG_Z);
    let out = run(&["transform", path.to_str().unwrap(), "--align", "y"]);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    // `wrfm geometry` on the result: the longest axis must now be y.
    let geo = run_stdin(&["geometry", "-"], &stdout(&out));
    assert_eq!(geo.status.code(), Some(0), "stderr: {}", stderr(&geo));
    let j = stdout_json(&geo);
    assert_eq!(j["alignment"]["longest_axis"], "y", "{}", stdout(&geo));
    assert!(
        (j["spans"]["y"].as_f64().unwrap() - 4.0).abs() < 1e-6,
        "y span: {}",
        j["spans"]
    );
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn transform_normalize_end_to_end() {
    let dir = scratch("transform_normalize_end_to_end");
    let path = write(&dir, "cube2.wrfm", CUBE2);
    let out = run(&["transform", path.to_str().unwrap(), "--normalize", "1"]);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    let back = wrfm::WrfmModel::from_str("normalized", &stdout(&out)).expect("output parses");
    let (min, max) = bbox_of(&back);
    let span = (max[0] - min[0]).max(max[1] - min[1]).max(max[2] - min[2]);
    assert!((span - 1.0).abs() < 1e-9, "max span {span}");
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn transform_bad_pivot_errors() {
    let dir = scratch("transform_bad_pivot_errors");
    let path = write(&dir, "cube.wrfm", CUBE);
    let out = run(&["transform", path.to_str().unwrap(), "--pivot", "nope"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(stdout(&out).is_empty());
    assert!(stderr(&out).contains("pivot"), "stderr: {}", stderr(&out));
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn transform_bad_align_errors() {
    let dir = scratch("transform_bad_align_errors");
    let path = write(&dir, "cube.wrfm", CUBE);
    let out = run(&["transform", path.to_str().unwrap(), "--align", "q"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(stdout(&out).is_empty());
    assert!(stderr(&out).contains("align"), "stderr: {}", stderr(&out));
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn transform_bad_normalize_errors() {
    let dir = scratch("transform_bad_normalize_errors");
    let path = write(&dir, "cube.wrfm", CUBE);
    let out = run(&["transform", path.to_str().unwrap(), "--normalize", "0"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(stdout(&out).is_empty());
    assert!(
        stderr(&out).contains("normalize"),
        "stderr: {}",
        stderr(&out)
    );
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn transform_pipe_with_new_flags() {
    // `wrfm transform - --scale-y 2 | wrfm check -` — the new flags work in
    // a stdin pipeline and the result is a healthy model.
    let script = format!(
        "set -e; printf '{}' | '{}' transform - --scale-y 2 | '{}' check -",
        CUBE.replace('\n', "\\n"),
        bin(),
        bin()
    );
    let out = Command::new("bash")
        .arg("-c")
        .arg(&script)
        .output()
        .expect("run bash");
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        String::from_utf8_lossy(&out.stdout).starts_with("ok:"),
        "stdout: {}",
        String::from_utf8_lossy(&out.stdout)
    );
}

// ---------------------------------------------------------------------------
// edit
// ---------------------------------------------------------------------------

#[test]
fn edit_delete_vertices_remaps_edges() {
    let dir = scratch("edit_delete_vertices_remaps_edges");
    let path = write(&dir, "cube.wrfm", CUBE);
    let out = run(&["edit", path.to_str().unwrap(), "--delete-vertices", "7"]);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    let so = stdout(&out);
    let back = wrfm::WrfmModel::from_str("edited", &so).expect("output parses");
    assert_eq!(back.vertices.len(), 7);
    // Every remaining edge references an existing vertex.
    for &(a, b) in &back.edges {
        assert!(
            a < back.vertices.len() && b < back.vertices.len(),
            "edge {a}-{b} out of range"
        );
    }
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn edit_delete_edges() {
    let dir = scratch("edit_delete_edges");
    let path = write(&dir, "cube.wrfm", CUBE);
    let out = run(&["edit", path.to_str().unwrap(), "--delete-edges", "0,1,2"]);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    let back = wrfm::WrfmModel::from_str("edited", &stdout(&out)).expect("output parses");
    assert_eq!(back.vertices.len(), 8);
    assert_eq!(back.edges.len(), 9);
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn edit_extract_group() {
    let dir = scratch("edit_extract_group");
    let path = write(&dir, "two.wrfm", TWO_GROUPS);
    let out = run(&["edit", path.to_str().unwrap(), "--extract-group", "body"]);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    let so = stdout(&out);
    let back = wrfm::WrfmModel::from_str("edited", &so).expect("output parses");
    assert_eq!(back.vertices.len(), 3);
    assert!(so.contains("group body"), "group preserved:\n{so}");
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn edit_two_ops_exit_two() {
    let dir = scratch("edit_two_ops_exit_two");
    let path = write(&dir, "cube.wrfm", CUBE);
    let out = run(&[
        "edit",
        path.to_str().unwrap(),
        "--delete-vertices",
        "0",
        "--delete-edges",
        "1",
    ]);
    assert_eq!(out.status.code(), Some(2));
    assert!(stdout(&out).is_empty());
    assert!(
        stderr(&out).contains("exactly ONE"),
        "stderr: {}",
        stderr(&out)
    );
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn edit_no_op_exit_two() {
    let dir = scratch("edit_no_op_exit_two");
    let path = write(&dir, "cube.wrfm", CUBE);
    let out = run(&["edit", path.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(2));
    assert!(
        stderr(&out).contains("requires one of"),
        "stderr: {}",
        stderr(&out)
    );
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn edit_clean_end_to_end() {
    let dir = scratch("edit_clean_end_to_end");
    let path = write(&dir, "dirty.wrfm", CLEAN_ME);
    let out = run(&["edit", path.to_str().unwrap(), "--clean"]);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    let so = stdout(&out);
    let back = wrfm::WrfmModel::from_str("edited", &so).expect("output parses");
    assert_eq!(back.vertices.len(), 4, "only the tetra survives");
    assert_eq!(back.edges.len(), 6);
    // The cleaned model is fully healthy: `wrfm check -` reports ok.
    let chk = run_stdin(&["check", "-"], &so);
    assert_eq!(chk.status.code(), Some(0), "check stderr: {}", stderr(&chk));
    assert!(
        stdout(&chk).starts_with("ok:"),
        "check stdout: {}",
        stdout(&chk)
    );
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn edit_dedupe_end_to_end() {
    let dir = scratch("edit_dedupe_end_to_end");
    let path = write(&dir, "dup.wrfm", DUP_MODEL);
    let out = run(&["edit", path.to_str().unwrap(), "--dedupe"]);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    let so = stdout(&out);
    let back = wrfm::WrfmModel::from_str("edited", &so).expect("output parses");
    assert_eq!(back.vertices.len(), 2, "duplicate vertex merged");
    assert_eq!(
        back.edges,
        vec![(0, 1)],
        "duplicate + zero-length edges dropped"
    );
    // The survivor is a single dangling edge (L2 broken -> check exits 1),
    // but it must have NO duplicate or zero-length edges any more.
    let chk = run_stdin(&["check", "-"], &so);
    let co = stdout(&chk);
    assert!(
        !co.contains("  duplicate edges:") && !co.contains("  zero-length edges:"),
        "check still reports dup/zero-length edges:\n{co}"
    );
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn edit_merge_end_to_end() {
    let dir = scratch("edit_merge_end_to_end");
    let a = write(&dir, "a.wrfm", CUBE);
    let b = write(&dir, "b.wrfm", CUBE_SHIFTED); // shifted -> no shared vertices
    let out = run(&["edit", a.to_str().unwrap(), "--merge", b.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    let so = stdout(&out);
    let back = wrfm::WrfmModel::from_str("edited", &so).expect("output parses");
    assert_eq!(back.vertices.len(), 16);
    assert_eq!(back.edges.len(), 24);
    // The merged model is a pair of closed boxes at different positions ->
    // fully healthy.
    let chk = run_stdin(&["check", "-"], &so);
    assert_eq!(chk.status.code(), Some(0), "check stderr: {}", stderr(&chk));
    assert!(
        stdout(&chk).starts_with("ok:"),
        "check stdout: {}",
        stdout(&chk)
    );
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn edit_merge_offsets_groups() {
    let dir = scratch("edit_merge_offsets_groups");
    let a = write(&dir, "a.wrfm", CUBE);
    let b = write(&dir, "b.wrfm", BODY_HEAD);
    let out = run(&["edit", a.to_str().unwrap(), "--merge", b.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    let back = wrfm::WrfmModel::from_str("edited", &stdout(&out)).expect("output parses");
    // b's groups were offset by a's 8 vertices: body [8,11), head [11,14).
    assert_eq!(back.groups.len(), 2);
    assert_eq!(back.groups[0].name, "body");
    assert_eq!(
        (back.groups[0].vertex_start, back.groups[0].vertex_end),
        (8, 11)
    );
    assert_eq!(
        (back.groups[1].vertex_start, back.groups[1].vertex_end),
        (11, 14)
    );
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn edit_merge_pipe_both_sources() {
    let dir = scratch("edit_merge_pipe_both_sources");
    let b = write(&dir, "b.wrfm", CUBE);
    // cat a | wrfm edit - --merge b.wrfm
    let out = run_stdin(&["edit", "-", "--merge", b.to_str().unwrap()], CUBE);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    let back = wrfm::WrfmModel::from_str("edited", &stdout(&out)).expect("output parses");
    assert_eq!(back.vertices.len(), 16);
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn edit_merge_two_stdin_errors() {
    let out = run_stdin(&["edit", "-", "--merge", "-"], CUBE);
    assert_eq!(out.status.code(), Some(2));
    assert!(stdout(&out).is_empty());
    assert!(
        stderr(&out).contains("at most one input may be stdin"),
        "stderr: {}",
        stderr(&out)
    );
}

#[test]
fn edit_two_ops_still_errors() {
    let dir = scratch("edit_two_ops_still_errors");
    let path = write(&dir, "cube.wrfm", CUBE);
    let out = run(&["edit", path.to_str().unwrap(), "--clean", "--dedupe"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(stdout(&out).is_empty());
    assert!(
        stderr(&out).contains("exactly ONE"),
        "stderr: {}",
        stderr(&out)
    );
    // A new op conflicts with an existing op too.
    let out = run(&[
        "edit",
        path.to_str().unwrap(),
        "--clean",
        "--delete-vertices",
        "0",
    ]);
    assert_eq!(out.status.code(), Some(2));
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn edit_out_of_range_exit_two() {
    let dir = scratch("edit_out_of_range_exit_two");
    let path = write(&dir, "cube.wrfm", CUBE);
    let out = run(&["edit", path.to_str().unwrap(), "--delete-vertices", "99"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(
        stderr(&out).contains("out of range"),
        "stderr: {}",
        stderr(&out)
    );
    fs::remove_dir_all(&dir).ok();
}

// ---------------------------------------------------------------------------
// diff
// ---------------------------------------------------------------------------

#[test]
fn diff_text_reports_changes() {
    let dir = scratch("diff_text_reports_changes");
    let a = write(&dir, "a.wrfm", CUBE);
    let scaled = CUBE.replace("v 1 0 0\nv 1 1 0", "v 2 0 0\nv 2 1 0");
    let b = write(&dir, "b.wrfm", &scaled);
    let out = run(&["diff", a.to_str().unwrap(), b.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    let so = stdout(&out);
    assert!(so.contains("# summary:"), "stdout:\n{so}");
    // diff has no check semantics: stderr stays empty.
    assert!(stderr(&out).is_empty(), "stderr: {}", stderr(&out));
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn diff_json_structured() {
    let dir = scratch("diff_json_structured");
    let a = write(&dir, "a.wrfm", CUBE);
    let b = write(&dir, "b.wrfm", &CUBE.replace("v 0 1 1", "v 0 1 2"));
    let out = run(&[
        "diff",
        a.to_str().unwrap(),
        b.to_str().unwrap(),
        "--format",
        "json",
    ]);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    let j = stdout_json(&out);
    assert_eq!(j["a"]["vertices"], 8);
    assert_eq!(j["b"]["vertices"], 8);
    assert_eq!(j["vertices"]["moved_count"], 1);
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn diff_one_stdin() {
    let dir = scratch("diff_one_stdin");
    let a = write(&dir, "a.wrfm", CUBE);
    let out = run_stdin(&["diff", a.to_str().unwrap(), "-"], CUBE);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn diff_both_stdin_exit_two() {
    let out = run_stdin(&["diff", "-", "-"], CUBE);
    assert_eq!(out.status.code(), Some(2));
    assert!(
        stderr(&out).contains("at most one"),
        "stderr: {}",
        stderr(&out)
    );
}

#[test]
fn diff_bad_format_exit_two() {
    let dir = scratch("diff_bad_format_exit_two");
    let a = write(&dir, "a.wrfm", CUBE);
    let b = write(&dir, "b.wrfm", CUBE);
    let out = run(&[
        "diff",
        a.to_str().unwrap(),
        b.to_str().unwrap(),
        "--format",
        "xml",
    ]);
    assert_eq!(out.status.code(), Some(2));
    fs::remove_dir_all(&dir).ok();
}

// ---------------------------------------------------------------------------
// three-channel contract + pipelines
// ---------------------------------------------------------------------------

#[test]
fn stderr_never_mixes_into_stdout() {
    let dir = scratch("stderr_never_mixes_into_stdout");
    let path = write(&dir, "broken.wrfm", BROKEN);
    // A broken model: the note goes to stderr, stdout stays pure JSON.
    let out = run(&["geometry", path.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(1));
    assert!(
        !stdout(&out).contains("[wrfm] check:"),
        "stdout: {}",
        stdout(&out)
    );
    assert!(
        stderr(&out).contains("[wrfm] check:"),
        "stderr: {}",
        stderr(&out)
    );
    // And stdout still parses as JSON.
    let _ = stdout_json(&out);
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn pipefail_propagates_broken_exit() {
    // `set -o pipefail` makes a pipeline fail when any stage exits non-zero
    // (PLAN §3) — here the `wrfm check -` stage exits 1 (broken model).
    let dir = scratch("pipefail_propagates_broken_exit");
    let script = format!(
        "set -o pipefail; printf '{}' | '{}' check - | wc -l >/dev/null",
        BROKEN.replace('\n', "\\n"),
        bin()
    );
    let out = Command::new("bash")
        .arg("-c")
        .arg(&script)
        .output()
        .expect("run bash");
    assert_eq!(
        out.status.code(),
        Some(1),
        "pipefail must surface wrfm's exit 1: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn transform_pipeline_roundtrip() {
    // `wrfm transform - --scale 2 | wrfm info -` — two CLI stages composed
    // over stdin, no files touched.
    let script = format!(
        "set -e; printf '{}' | '{}' transform - --scale 2 | '{}' info -",
        CUBE.replace('\n', "\\n"),
        bin(),
        bin()
    );
    let out = Command::new("bash")
        .arg("-c")
        .arg(&script)
        .output()
        .expect("run bash");
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let j: Value = serde_json::from_str(&String::from_utf8_lossy(&out.stdout)).expect("info JSON");
    assert_eq!(j["vertices"], 8);
}

// ---------------------------------------------------------------------------
// format
// ---------------------------------------------------------------------------

#[test]
fn format_exits_zero_with_clean_stderr() {
    let out = run(&["format"]);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    assert!(!stdout(&out).is_empty(), "stdout must carry the spec");
    assert!(stderr(&out).is_empty(), "stderr: {}", stderr(&out));
}

#[test]
fn format_teaches_magic_and_header() {
    let out = run(&["format"]);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    let so = stdout(&out);
    assert!(so.contains("wrfm 1"), "stdout:\n{so}");
    assert!(so.contains("vertices <N>   edges <M>"), "stdout:\n{so}");
}

#[test]
fn format_teaches_group_semantics() {
    let out = run(&["format"]);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    let so = stdout(&out);
    // The "groups re-list global vertices; indexing never restarts" rule.
    assert!(so.contains("RE-LIST"), "stdout:\n{so}");
    assert!(so.contains("NEVER restarts"), "stdout:\n{so}");
    // The complete group example.
    assert!(so.contains("group body"), "stdout:\n{so}");
    assert!(so.contains("group head"), "stdout:\n{so}");
    assert!(
        so.contains("index 4 = the vertex in group head (GLOBAL index)"),
        "stdout:\n{so}"
    );
}

#[test]
fn format_teaches_streams_contract() {
    let out = run(&["format"]);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    let so = stdout(&out);
    // The 2>&1 corruption warning and the diagnostic channel spelling.
    assert!(so.contains("2>/dev/null"), "stdout:\n{so}");
    assert!(so.contains("stderr = \"[wrfm] check:"), "stdout:\n{so}");
}

#[test]
fn format_teaches_y_up() {
    let out = run(&["format"]);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    let so = stdout(&out);
    assert!(so.contains("Y-UP"), "stdout:\n{so}");
    assert!(so.contains("+Y is vertical"), "stdout:\n{so}");
}

#[test]
fn help_mentions_format() {
    let out = run(&["--help"]);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    let so = stdout(&out);
    assert!(so.contains("wrfm format"), "stdout:\n{so}");
}
