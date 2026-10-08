//! Integration tests for the `wrfm` CLI binary — the contract in
//! `docs/PLAN.md` (PLAN-cli-stream §3) is an exit-code + three-channel
//! contract, so these tests assert EXIT CODES and stream purity, not just
//! output text.
//!
//! Exit codes — FOUR tiers, larger = more severe: 0 = ok (clean result /
//! verify pass) · 1 = warn (a warning-level health issue; `check` only) ·
//! 2 = broken (repair required: `check` broken, an unmet `verify`
//! declaration, or `--strict` upgrading a warn) · 3 = no result (L1 parse /
//! usage / I/O / unknown argument — clap's own argument errors are mapped
//! onto 3 as well). stdout carries the pure result;
//! stderr carries diagnostics and errors only — never a health note, never
//! mixed into stdout (the verdict travels on the exit code).
//!
//! Fixtures are FORMAT.md v1: a `wrfm 1` magic line and a
//! `vertices <V>   edges <M>` counts header.

use serde_json::Value;
use std::fs;
use std::io::{Read, Write};
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
/// triangle) and `head` (3..6, a triangle) — 6 vertices, 6 edges. Every
/// vertex is a triangle corner: two healthy closed loops, so the whole
/// model is an `ok` verdict and these commands exit 0.
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

/// A valid v1 cube spanning [-1,1]^3 — mirror-symmetric about every
/// origin plane (unlike the [0,1]^3 `CUBE`, whose symmetry plane does not
/// pass through the origin). bbox SIZE is [2,2,2].
const CUBE_CENTERED: &str = "\
wrfm 1
vertices 8   edges 12

v -1 -1 -1
v 1 -1 -1
v 1 1 -1
v -1 1 -1
v -1 -1 1
v 1 -1 1
v 1 1 1
v -1 1 1
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

/// A closed loop with ONE dead-straight midpoint: bottom run 0-1-2 is
/// collinear (vertex 1 is redundant), the other four vertices turn corners.
/// Every vertex is degree 2 — under the old rule all five warned, now only
/// the straight-through midpoint does.
const REDUNDANT_MIDPOINT: &str = "\
wrfm 1
vertices 5   edges 5

v 0 0 0
v 1 0 0
v 2 0 0
v 2 1 0
v 0 1 0
e 0 1
e 1 2
e 2 3
e 3 4
e 4 0
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
fn check_broken_exit_two() {
    let dir = scratch("check_broken_exit_two");
    let path = write(&dir, "broken.wrfm", BROKEN);
    let out = run(&["check", path.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(2), "stderr: {}", stderr(&out));
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
fn check_parse_error_exit_three() {
    // NEW contract: L1 parse failure → exit 3 (no result), no stdout result.
    let dir = scratch("check_parse_error_exit_three");
    let path = write(
        &dir,
        "bad.wrfm",
        "wrfm 1\nvertices 1   edges 0\n\nv 1.0 2.0 abc\n",
    );
    let out = run(&["check", path.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(3), "stderr: {}", stderr(&out));
    assert!(stdout(&out).is_empty(), "no result on L1 failure");
    let se = stderr(&out);
    assert!(se.contains("line") && se.contains("column"), "stderr: {se}");
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn check_out_of_range_exit_three() {
    // Out-of-range edges are L1 (parser) failures now → exit 3.
    let dir = scratch("check_out_of_range_exit_three");
    let path = write(
        &dir,
        "oor.wrfm",
        "wrfm 1\nvertices 2   edges 1\n\nv 0 0 0\nv 1 1 1\ne 0 5\n",
    );
    let out = run(&["check", path.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(3), "stderr: {}", stderr(&out));
    assert!(
        stderr(&out).contains("out of range"),
        "stderr: {}",
        stderr(&out)
    );
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn check_io_error_exit_three() {
    let missing =
        std::env::temp_dir().join(format!("wrfm-cli-missing-{}.wrfm", std::process::id()));
    let _ = fs::remove_file(&missing);
    let out = run(&["check", missing.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(3));
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
fn check_stdin_garbage_exit_three() {
    let out = run_stdin(&["check", "-"], "not a wrfm file at all\n");
    assert_eq!(out.status.code(), Some(3));
    assert!(stdout(&out).is_empty());
}

#[test]
fn check_usage_exit_three() {
    let out = run(&["check"]);
    assert_eq!(out.status.code(), Some(3));
    assert!(!stderr(&out).is_empty(), "clap prints a usage error");
}

#[test]
fn check_group_scope() {
    // --group scopes the L2 check to the part: the scoped report carries
    // the PART's counts (3 vertices / 3 edges for the body triangle) while
    // the whole model stays healthy — corner loops have no redundancy.
    let dir = scratch("check_group_scope");
    let path = write(&dir, "two.wrfm", TWO_GROUPS);
    let out = run(&["check", path.to_str().unwrap(), "--group", "body"]);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    let so = stdout(&out);
    assert!(
        so.starts_with("ok: two (3 vertices, 3 edges)"),
        "scoped counts: {so}"
    );
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn check_reports_only_collinear_midpoints_as_redundant() {
    let out = run_stdin(&["check", "-", "--format", "json"], REDUNDANT_MIDPOINT);
    assert_eq!(out.status.code(), Some(1), "redundant midpoint -> warn");
    let j = stdout_json(&out);
    let ids = j["issues"]["redundant_vertices"].as_array().unwrap();
    assert_eq!(ids.len(), 1, "only the straight-through midpoint: {j}");
    assert_eq!(ids[0]["vertex"], 1);

    // The text form names the category the same way.
    let out = run_stdin(&["check", "-"], REDUNDANT_MIDPOINT);
    assert!(
        stdout(&out).contains("redundant vertices:"),
        "stdout: {}",
        stdout(&out)
    );

    // Bend the midpoint off the chord: an ordinary corner is healthy, so
    // the whole loop is `ok` (the old rule warned on every degree-2 vertex).
    let bent = REDUNDANT_MIDPOINT.replace("v 1 0 0", "v 1 0.2 0");
    let out = run_stdin(&["check", "-"], &bent);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    assert!(
        stdout(&out).starts_with("ok: stdin (5 vertices, 5 edges)"),
        "stdout: {}",
        stdout(&out)
    );
}

#[test]
fn verify_expect_redundant_declares_accepted_midpoints() {
    let out = run_stdin(
        &["verify", "-", "--expect-redundant", "1"],
        REDUNDANT_MIDPOINT,
    );
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    let j = stdout_json(&out);
    assert_eq!(j["verdict"], "pass");
    assert_eq!(j["expectations"][0]["name"], "redundant");
    assert_eq!(j["expectations"][0]["actual"], 1);

    // A stale declaration is a failed one: the exact count is asserted so a
    // newly introduced midpoint cannot slip in unnoticed.
    let out = run_stdin(
        &["verify", "-", "--expect-redundant", "0"],
        REDUNDANT_MIDPOINT,
    );
    assert_eq!(
        out.status.code(),
        Some(2),
        "unmet declaration -> broken tier"
    );
    let j = stdout_json(&out);
    assert_eq!(j["verdict"], "fail");
    let s = j["expectations"][0]["suggestion"].as_str().unwrap();
    assert!(s.contains("--expect-redundant 1"), "suggestion: {s}");
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
    assert_eq!(out2.status.code(), Some(2), "--strict upgrades warn to 2");
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
fn info_group_scopes_counts_and_bounds() {
    let dir = scratch("info_group_scopes_counts_and_bounds");
    let path = write(&dir, "two.wrfm", TWO_GROUPS);
    let out = run(&["info", path.to_str().unwrap(), "--group", "body"]);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    let j = stdout_json(&out);
    // The body triangle: 3 vertices, 3 edges, bbox [0,1]^2 at z=0.
    assert_eq!(j["vertices"], 3);
    assert_eq!(j["edges"], 3);
    assert_eq!(j["bounds"]["min"][0], 0.0);
    assert_eq!(j["bounds"]["max"][0], 1.0);
    assert_eq!(j["bounds"]["max"][1], 1.0);
    assert_eq!(j["bounds"]["max"][2], 0.0);
    // The groups list still covers the whole file.
    assert_eq!(j["groups"].as_array().unwrap().len(), 2);
    // A missing group is a usage error.
    let out = run(&["info", path.to_str().unwrap(), "--group", "nope"]);
    assert_eq!(out.status.code(), Some(3));
    assert!(
        stderr(&out).contains("group 'nope' not found"),
        "stderr: {}",
        stderr(&out)
    );
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn info_broken_exit_two_with_pure_stdout() {
    let dir = scratch("info_broken_exit_two");
    let path = write(&dir, "broken.wrfm", BROKEN);
    let out = run(&["info", path.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(2), "stderr: {}", stderr(&out));
    // The JSON result is STILL emitted on stdout.
    let j = stdout_json(&out);
    assert_eq!(j["vertices"], 8);
    // No health note anywhere: stdout stays pure JSON, stderr carries only
    // real errors (this run has none) — the verdict is the exit code.
    assert!(stderr(&out).is_empty(), "stderr: {}", stderr(&out));
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn info_parse_error_exit_three() {
    let dir = scratch("info_parse_error_exit_three");
    let path = write(
        &dir,
        "bad.wrfm",
        "wrfm 1\nvertices 1   edges 0\n\nv 1.0 2.0 abc\n",
    );
    let out = run(&["info", path.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(3));
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
fn group_unknown_exit_three() {
    let dir = scratch("group_unknown_exit_three");
    let path = write(&dir, "two.wrfm", TWO_GROUPS);
    let out = run(&["group", path.to_str().unwrap(), "nope"]);
    assert_eq!(out.status.code(), Some(3), "stderr: {}", stderr(&out));
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
fn geometry_broken_exit_two() {
    let dir = scratch("geometry_broken_exit_two");
    let path = write(&dir, "broken.wrfm", BROKEN);
    let out = run(&["geometry", path.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(2), "stderr: {}", stderr(&out));
    let j = stdout_json(&out); // result still emitted
    assert_eq!(j["topology"]["edges"], 13);
    // No health note: the verdict travels on exit 2, stderr stays clean.
    assert!(stderr(&out).is_empty(), "stderr: {}", stderr(&out));
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn geometry_group_scopes_topology() {
    let dir = scratch("geometry_group_scopes_topology");
    let path = write(&dir, "two.wrfm", TWO_GROUPS);
    let out = run(&["geometry", path.to_str().unwrap(), "--group", "body"]);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    let j = stdout_json(&out);
    // The body triangle: 3 vertices, 3 edges, bbox size [1,1,0].
    assert_eq!(j["topology"]["vertices"], 3);
    assert_eq!(j["topology"]["edges"], 3);
    assert_eq!(j["bounds"]["size"], serde_json::json!([1.0, 1.0, 0.0]));
    // A missing group is a usage error.
    let out = run(&["geometry", path.to_str().unwrap(), "--group", "nope"]);
    assert_eq!(out.status.code(), Some(3));
    assert!(
        stderr(&out).contains("group 'nope' not found"),
        "stderr: {}",
        stderr(&out)
    );
    fs::remove_dir_all(&dir).ok();
}

// ---------------------------------------------------------------------------
// query
// ---------------------------------------------------------------------------

#[test]
fn query_summary_modes_moved_to_geometry() {
    let dir = scratch("query_summary_modes_moved_to_geometry");
    let path = write(&dir, "cube.wrfm", CUBE);
    let f = path.to_str().unwrap();
    for (old, field) in [
        ("extents", "bounds"),
        ("topology", "topology"),
        ("edge_stats", "edge_lengths"),
    ] {
        let out = run(&["query", f, old]);
        assert_eq!(out.status.code(), Some(3), "{old} must be a usage error");
        let e = stderr(&out);
        assert!(
            e.contains("wrfm geometry") && e.contains(field),
            "{old} must name its replacement: {e}"
        );
        // The replacement really carries the numbers (no information loss).
        let j = stdout_json(&run(&["geometry", f]));
        assert!(j.get(field).is_some(), "geometry must expose .{field}");
    }
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
        "profile",
        "--group",
        "body",
    ]);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    let so = stdout(&out);
    // The `body` triangle spans x = 0..1 (the whole model spans 0..1 too,
    // but the head group's vertices are excluded from the submodel).
    assert!(so.contains("x: span=1.000"), "group-scoped query: {so}");
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
    assert_eq!(out.status.code(), Some(3));
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
    assert_eq!(out.status.code(), Some(3));
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
        "--range",
        "0,1",
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
        "--range",
        "0,99",
    ]);
    assert_eq!(out.status.code(), Some(3));
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
    let out = run(&["query", path.to_str().unwrap(), "distance"]);
    assert_eq!(out.status.code(), Some(3));
    assert!(
        stderr(&out).contains("requires --range"),
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
        "--range",
        "0,2",
    ]);
    // The chain fixture is L2-broken (isolated vertex) -> exit 2, but
    // the JSON result is still emitted.
    assert_eq!(yes.status.code(), Some(2), "stderr: {}", stderr(&yes));
    assert_eq!(stdout_json(&yes)["connected"], true);
    let no = run(&[
        "query",
        path.to_str().unwrap(),
        "connectivity",
        "--range",
        "0,3",
    ]);
    assert_eq!(no.status.code(), Some(2), "stderr: {}", stderr(&no));
    assert_eq!(stdout_json(&no)["connected"], false);
    // A == B is trivially connected.
    let same = run(&[
        "query",
        path.to_str().unwrap(),
        "connectivity",
        "--range",
        "3,3",
    ]);
    assert_eq!(stdout_json(&same)["connected"], true);
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn query_unknown_exit_three() {
    let dir = scratch("query_unknown_exit_three");
    let path = write(&dir, "cube.wrfm", CUBE);
    let out = run(&["query", path.to_str().unwrap(), "nope"]);
    assert_eq!(out.status.code(), Some(3));
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

#[test]
fn view_json_is_never_truncated() {
    // 150 disjoint vertical segments (300 vertices, 150 edges): every one of
    // them is visible from the front, so the edge lists carry >100 entries —
    // `view` prints them ALL: JSON output is never truncated.
    let dir = scratch("view_json_is_never_truncated");
    let mut m = String::from("wrfm 1\nvertices 300   edges 150\n\n");
    for i in 0..150 {
        m.push_str(&format!("v {i} 0 0\nv {i} 1 0\n"));
    }
    for i in 0..150 {
        m.push_str(&format!("e {} {}\n", 2 * i, 2 * i + 1));
    }
    let path = write(&dir, "bars.wrfm", &m);
    // Explicit camera: the fixture is flat (z-span 0), so the geometric-mean
    // auto distance would be microscopic.
    let out = run(&[
        "view",
        path.to_str().unwrap(),
        "--auto-dist",
        "false",
        "--dist",
        "100",
    ]);
    // Degree-1 vertices are a warn-level issue -> exit 1, result still full.
    assert_eq!(out.status.code(), Some(1), "stderr: {}", stderr(&out));
    let j = stdout_json(&out);
    let visible = j["totals"]["visible"].as_u64().unwrap() as usize;
    assert!(visible > 100, "fixture must exceed any old cap: {visible}");
    assert_eq!(
        j["visible_edges"].as_array().unwrap().len(),
        visible,
        "every visible edge is listed"
    );
    assert_eq!(
        j["occluded_edges"].as_array().unwrap().len(),
        j["totals"]["occluded"].as_u64().unwrap() as usize,
        "every occluded edge is listed"
    );
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
fn render_broken_exit_two_still_emits() {
    let dir = scratch("render_broken_exit_two");
    let path = write(&dir, "broken.wrfm", BROKEN);
    let out = run(&["render", path.to_str().unwrap(), "--format", "grid"]);
    assert_eq!(out.status.code(), Some(2), "stderr: {}", stderr(&out));
    assert!(
        stdout(&out).contains("# grid"),
        "render still emitted:\n{}",
        stdout(&out)
    );
    // stderr carries only real errors — never the health report.
    assert!(stderr(&out).is_empty(), "stderr: {}", stderr(&out));
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
    // No health note on stderr (even for ok): the verdict is the exit code.
    assert!(stderr(&out).is_empty(), "stderr: {}", stderr(&out));
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
fn transform_broken_exit_two_result_emitted() {
    let dir = scratch("transform_broken_exit_two");
    let path = write(&dir, "broken.wrfm", BROKEN);
    let out = run(&["transform", path.to_str().unwrap(), "--scale", "1"]);
    assert_eq!(out.status.code(), Some(2), "stderr: {}", stderr(&out));
    // Mechanism A: the result is STILL emitted even when broken.
    assert!(
        stdout(&out).starts_with("wrfm 1\n"),
        "stdout:\n{}",
        stdout(&out)
    );
    // Broken is reported by the EXIT CODE (2), not by a stderr note.
    assert!(stderr(&out).is_empty(), "stderr: {}", stderr(&out));
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn transform_bad_translate_exit_three() {
    let dir = scratch("transform_bad_translate_exit_three");
    let path = write(&dir, "cube.wrfm", CUBE);
    let out = run(&["transform", path.to_str().unwrap(), "--translate", "1,2"]);
    assert_eq!(out.status.code(), Some(3));
    assert!(stdout(&out).is_empty());
    assert!(
        stderr(&out).contains("translate"),
        "stderr: {}",
        stderr(&out)
    );
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn transform_bad_mirror_exit_three() {
    let dir = scratch("transform_bad_mirror_exit_three");
    let path = write(&dir, "cube.wrfm", CUBE);
    let out = run(&["transform", path.to_str().unwrap(), "--mirror", "w"]);
    assert_eq!(out.status.code(), Some(3));
    assert!(stderr(&out).contains("mirror"), "stderr: {}", stderr(&out));
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn transform_zero_axis_exit_three() {
    let dir = scratch("transform_zero_axis_exit_three");
    let path = write(&dir, "cube.wrfm", CUBE);
    let out = run(&[
        "transform",
        path.to_str().unwrap(),
        "--rotate-axis",
        "0,0,0",
    ]);
    assert_eq!(out.status.code(), Some(3));
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
fn transform_to_origin_end_to_end() {
    let dir = scratch("transform_to_origin_end_to_end");
    let path = write(&dir, "cube13.wrfm", CUBE_X13);
    let out = run(&["transform", path.to_str().unwrap(), "--to-origin"]);
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
    assert_eq!(out.status.code(), Some(3));
    assert!(stdout(&out).is_empty());
    assert!(stderr(&out).contains("pivot"), "stderr: {}", stderr(&out));
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn transform_bad_align_errors() {
    let dir = scratch("transform_bad_align_errors");
    let path = write(&dir, "cube.wrfm", CUBE);
    let out = run(&["transform", path.to_str().unwrap(), "--align", "q"]);
    assert_eq!(out.status.code(), Some(3));
    assert!(stdout(&out).is_empty());
    assert!(stderr(&out).contains("align"), "stderr: {}", stderr(&out));
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn transform_bad_normalize_errors() {
    let dir = scratch("transform_bad_normalize_errors");
    let path = write(&dir, "cube.wrfm", CUBE);
    let out = run(&["transform", path.to_str().unwrap(), "--normalize", "0"]);
    assert_eq!(out.status.code(), Some(3));
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
    // The deleted corner's neighbours become real corners of the hole —
    // no redundant midpoints, so the edited model is healthy.
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
    assert_eq!(out.status.code(), Some(1), "stderr: {}", stderr(&out));
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
    // A lone triangle of corners is a healthy closed loop.
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    let so = stdout(&out);
    let back = wrfm::WrfmModel::from_str("edited", &so).expect("output parses");
    assert_eq!(back.vertices.len(), 3);
    assert!(so.contains("group body"), "group preserved:\n{so}");
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn edit_two_ops_exit_three() {
    let dir = scratch("edit_two_ops_exit_three");
    let path = write(&dir, "cube.wrfm", CUBE);
    let out = run(&[
        "edit",
        path.to_str().unwrap(),
        "--delete-vertices",
        "0",
        "--delete-edges",
        "1",
    ]);
    assert_eq!(out.status.code(), Some(3));
    assert!(stdout(&out).is_empty());
    assert!(
        stderr(&out).contains("exactly ONE"),
        "stderr: {}",
        stderr(&out)
    );
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn edit_no_op_exit_three() {
    let dir = scratch("edit_no_op_exit_three");
    let path = write(&dir, "cube.wrfm", CUBE);
    let out = run(&["edit", path.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(3));
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
    assert_eq!(out.status.code(), Some(1), "stderr: {}", stderr(&out));
    let so = stdout(&out);
    let back = wrfm::WrfmModel::from_str("edited", &so).expect("output parses");
    assert_eq!(back.vertices.len(), 2, "duplicate vertex merged");
    assert_eq!(
        back.edges,
        vec![(0, 1)],
        "duplicate + zero-length edges dropped"
    );
    // The survivor is a single dangling edge (L2 warn -> the edit exits 1),
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
    assert_eq!(out.status.code(), Some(1), "stderr: {}", stderr(&out));
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
    assert_eq!(out.status.code(), Some(1), "stderr: {}", stderr(&out));
    let back = wrfm::WrfmModel::from_str("edited", &stdout(&out)).expect("output parses");
    assert_eq!(back.vertices.len(), 16);
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn edit_merge_two_stdin_errors() {
    let out = run_stdin(&["edit", "-", "--merge", "-"], CUBE);
    assert_eq!(out.status.code(), Some(3));
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
    assert_eq!(out.status.code(), Some(3));
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
    assert_eq!(out.status.code(), Some(3));
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn edit_out_of_range_exit_three() {
    let dir = scratch("edit_out_of_range_exit_three");
    let path = write(&dir, "cube.wrfm", CUBE);
    let out = run(&["edit", path.to_str().unwrap(), "--delete-vertices", "99"]);
    assert_eq!(out.status.code(), Some(3));
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
fn diff_both_stdin_exit_three() {
    let out = run_stdin(&["diff", "-", "-"], CUBE);
    assert_eq!(out.status.code(), Some(3));
    assert!(
        stderr(&out).contains("at most one"),
        "stderr: {}",
        stderr(&out)
    );
}

#[test]
fn diff_bad_format_exit_three() {
    let dir = scratch("diff_bad_format_exit_three");
    let a = write(&dir, "a.wrfm", CUBE);
    let b = write(&dir, "b.wrfm", CUBE);
    let out = run(&[
        "diff",
        a.to_str().unwrap(),
        b.to_str().unwrap(),
        "--format",
        "xml",
    ]);
    assert_eq!(out.status.code(), Some(3));
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn diff_scopes_by_extraction_pipelines() {
    // `diff --group` is gone: scope a diff by extracting the group first.
    let dir = scratch("diff_scopes_by_extraction");
    let a = write(&dir, "a.wrfm", TWO_GROUPS);
    // Only the body triangle's vertex (1,0,0) moves to (2,0,0); head
    // (which has its own "v 1 0 1") is untouched.
    let b = write(&dir, "b.wrfm", &TWO_GROUPS.replace("v 1 0 0", "v 2 0 0"));
    // Whole-model diff sees the moved vertex.
    let whole = run(&[
        "diff",
        a.to_str().unwrap(),
        b.to_str().unwrap(),
        "--format",
        "json",
    ]);
    assert_eq!(whole.status.code(), Some(0), "stderr: {}", stderr(&whole));
    assert_eq!(stdout_json(&whole)["vertices"]["moved_count"], 1);
    // Scoping to a group is `edit --extract-group` on each side:
    // extracting `head` gives an unchanged pair.
    let ea = run(&["edit", a.to_str().unwrap(), "--extract-group", "head"]);
    let eb = run(&["edit", b.to_str().unwrap(), "--extract-group", "head"]);
    assert_eq!(ea.status.code(), Some(0), "stderr: {}", stderr(&ea));
    assert_eq!(eb.status.code(), Some(0), "stderr: {}", stderr(&eb));
    let (head_a, head_b) = (stdout(&ea), stdout(&eb));
    let ha = write(&dir, "head-a.wrfm", &head_a);
    let hb = write(&dir, "head-b.wrfm", &head_b);
    let out = run(&[
        "diff",
        ha.to_str().unwrap(),
        hb.to_str().unwrap(),
        "--format",
        "json",
    ]);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    assert_eq!(stdout_json(&out)["vertices"]["moved_count"], 0);
    // A group that does not exist is a usage error (exit 3) on the extract.
    let out = run(&["edit", a.to_str().unwrap(), "--extract-group", "nope"]);
    assert_eq!(out.status.code(), Some(3));
    assert!(
        stderr(&out).contains("group 'nope' not found"),
        "stderr: {}",
        stderr(&out)
    );
    fs::remove_dir_all(&dir).ok();
}

// ---------------------------------------------------------------------------
// three-channel contract + pipelines
// ---------------------------------------------------------------------------

#[test]
fn stderr_never_mixes_into_stdout() {
    let dir = scratch("stderr_never_mixes_into_stdout");
    let path = write(&dir, "broken.wrfm", BROKEN);
    // A broken model: the result goes to stdout as pure JSON, stderr carries
    // only real errors (there are none here) and stdout parses cleanly.
    let out = run(&["geometry", path.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(2));
    let _ = stdout_json(&out);
    assert!(stderr(&out).is_empty(), "stderr: {}", stderr(&out));
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn pipefail_propagates_broken_exit() {
    // `set -o pipefail` makes a pipeline fail when any stage exits non-zero
    // (PLAN §3) — here the `wrfm check -` stage exits 2 (broken model).
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
        Some(2),
        "pipefail must surface wrfm's exit 2: {}",
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
    assert!(
        so.contains("stderr = diagnostics and errors"),
        "stdout:\n{so}"
    );
    assert!(
        so.contains("the health verdict travels on the exit"),
        "stdout:\n{so}"
    );
    // The four-tier exit-code contract, as taught by `wrfm format` itself
    // (verify fail sits in the "repair required" tier, not in warn).
    assert!(so.contains("0 ok · 1 warn"), "stdout:\n{so}");
    assert!(so.contains("2 broken (repair required"), "stdout:\n{so}");
    assert!(so.contains("3 no result"), "stdout:\n{so}");
    assert!(so.contains("--strict upgrading a warn"), "stdout:\n{so}");
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
    // The after_help exit-code table matches the four-tier contract.
    assert!(
        so.contains("0 ok (clean result / verify pass) · 1 warn"),
        "stdout:\n{so}"
    );
    assert!(so.contains("2 broken"), "stdout:\n{so}");
    assert!(so.contains("3 no result"), "stdout:\n{so}");
}

// ---------------------------------------------------------------------------
// verify
// ---------------------------------------------------------------------------

#[test]
fn verify_pass_exit_zero() {
    let dir = scratch("verify_pass_exit_zero");
    let path = write(&dir, "cube.wrfm", CUBE);
    let out = run(&[
        "verify",
        path.to_str().unwrap(),
        "--expect-size",
        "1,1,1",
        "--expect-closed",
    ]);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    let v = stdout_json(&out);
    assert_eq!(v["verdict"], "pass");
    assert_eq!(v["summary"], "2 of 2 expectations met");
    // A passing run reports nothing on stderr.
    assert!(stderr(&out).is_empty(), "stderr: {}", stderr(&out));
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn verify_fail_exit_two() {
    let dir = scratch("verify_fail_exit_two");
    let path = write(&dir, "cube.wrfm", CUBE);
    let out = run(&["verify", path.to_str().unwrap(), "--expect-size", "3,3,3"]);
    // An unmet declaration is "repair required", not a warning.
    assert_eq!(out.status.code(), Some(2), "stderr: {}", stderr(&out));
    let v = stdout_json(&out);
    assert_eq!(v["verdict"], "fail");
    let e = &v["expectations"][0];
    assert_eq!(e["name"], "size");
    assert_eq!(e["pass"], false);
    assert_eq!(e["delta"], serde_json::json!([-2.0, -2.0, -2.0]));
    let s = e["suggestion"].as_str().unwrap();
    assert!(s.contains("--scale-x 3"), "suggestion: {s}");
    // The fail summary is mirrored on stderr.
    assert!(
        stderr(&out).contains("[wrfm] verify: 0 of 1 expectations met"),
        "stderr: {}",
        stderr(&out)
    );
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn verify_no_expectations_exit_three() {
    let dir = scratch("verify_no_expectations_exit_three");
    let path = write(&dir, "cube.wrfm", CUBE);
    let out = run(&["verify", path.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(3), "stderr: {}", stderr(&out));
    assert!(
        stderr(&out).contains("needs at least one --expect-* flag"),
        "stderr: {}",
        stderr(&out)
    );
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn verify_parse_error_exit_three() {
    let dir = scratch("verify_parse_error_exit_three");
    let path = write(&dir, "cube.wrfm", CUBE);
    let out = run(&["verify", path.to_str().unwrap(), "--expect-size", "1,,2"]);
    assert_eq!(out.status.code(), Some(3), "stderr: {}", stderr(&out));
    assert!(
        stderr(&out).contains("expect_size must be 'x,y,z'"),
        "stderr: {}",
        stderr(&out)
    );
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn verify_missing_file_exit_three() {
    let dir = scratch("verify_missing_file_exit_three");
    let missing = dir.join("nope.wrfm");
    let out = run(&[
        "verify",
        missing.to_str().unwrap(),
        "--expect-size",
        "1,1,1",
    ]);
    assert_eq!(out.status.code(), Some(3), "stderr: {}", stderr(&out));
    assert!(
        stderr(&out).contains("cannot read"),
        "stderr: {}",
        stderr(&out)
    );
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn verify_symmetric_centered_cube_passes() {
    let dir = scratch("verify_symmetric_centered_cube_passes");
    let path = write(&dir, "ccube.wrfm", CUBE_CENTERED);
    let out = run(&[
        "verify",
        path.to_str().unwrap(),
        "--expect-size",
        "2,2,2",
        "--expect-closed",
        "--expect-symmetric",
        "x,y,z",
    ]);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    let v = stdout_json(&out);
    assert_eq!(v["verdict"], "pass");
    assert_eq!(v["summary"], "5 of 5 expectations met");
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn verify_group_scopes_expectations() {
    let dir = scratch("verify_group_scopes_expectations");
    let path = write(&dir, "two.wrfm", TWO_GROUPS);
    // size/center/closed/axis/symmetry apply to the body part only: the
    // body triangle is [0,1]^2 at z=0, so size 1,1,0 passes...
    let out = run(&[
        "verify",
        path.to_str().unwrap(),
        "--group",
        "body",
        "--expect-size",
        "1,1,0",
    ]);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    let v = stdout_json(&out);
    assert_eq!(v["verdict"], "pass");
    // ...while the whole-model size 1,1,1 fails against the part.
    let out = run(&[
        "verify",
        path.to_str().unwrap(),
        "--group",
        "body",
        "--expect-size",
        "1,1,1",
    ]);
    assert_eq!(
        out.status.code(),
        Some(2),
        "unmet intent -> repair required"
    );
    let v = stdout_json(&out);
    assert_eq!(v["verdict"], "fail");
    // --expect-groups still checks the FULL group list (head exists even
    // though the expectations are scoped to the body part).
    let out = run(&[
        "verify",
        path.to_str().unwrap(),
        "--group",
        "body",
        "--expect-groups",
        "head",
    ]);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    let v = stdout_json(&out);
    assert_eq!(v["verdict"], "pass");
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn verify_symmetric_off_center_cube_passes() {
    // The [0,1]^3 CUBE is symmetric about its own bbox centre, so the
    // symmetry expectation PASSES regardless of position — verify uses the
    // centre-plane semantics (position-independent shape symmetry).
    let dir = scratch("verify_symmetric_off_center_cube_passes");
    let path = write(&dir, "cube.wrfm", CUBE);
    let out = run(&["verify", path.to_str().unwrap(), "--expect-symmetric", "x"]);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    let v = stdout_json(&out);
    assert_eq!(v["verdict"], "pass");
    assert_eq!(v["expectations"][0]["name"], "symmetric_x");
    assert_eq!(v["expectations"][0]["pass"], true);
    fs::remove_dir_all(&dir).ok();
}

// ---------------------------------------------------------------------------
// Review-doc additions (docs/wrfm-cli-review.md #1 #2 #3 #4 #6 #7)
// ---------------------------------------------------------------------------

/// A tetrahedron plus a 5th vertex that differs from vertex 3 by ~2e-16
/// (float noise between two samples of the same curve), wired into the
/// tetra's three other corners. `--dedupe` (exact match) keeps it;
/// `--weld 1e-9` merges it, and its three edges collapse onto the
/// tetra's existing edges (dropped by the shared cleanup).
const NEAR_DUP_TETRA: &str = "\
wrfm 1
vertices 5   edges 9

v 0 0 0
v 1 0 0
v 0 1 0
v 0 0 1
v 0 0 1.0000000000000002
e 0 1
e 0 2
e 0 3
e 1 2
e 1 3
e 2 3
e 0 4
e 1 4
e 2 4
";

/// Lit (`#`) dots in a `--format ascii` render, skipping the `# ...`
/// header lines and the `[view=...]` markers.
fn lit_dots(render_stdout: &str) -> usize {
    render_stdout
        .lines()
        .filter(|l| !l.starts_with('#') && !l.starts_with("[view="))
        .map(|l| l.chars().filter(|&c| c == '#').count())
        .sum()
}

#[test]
fn check_always_prints_the_full_report() {
    let dir = scratch("check_full_report");
    let ok = write(&dir, "cube.wrfm", CUBE);
    let out = run(&["check", ok.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    assert!(
        stdout(&out).starts_with("ok: cube (8 vertices, 12 edges)"),
        "stdout: {}",
        stdout(&out)
    );
    assert!(stderr(&out).is_empty(), "stderr: {}", stderr(&out));

    let warn = write(&dir, "dup.wrfm", DUP_VERTEX);
    let out = run(&["check", warn.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(1), "warn stays 1: {}", stderr(&out));
    assert!(
        stdout(&out).starts_with("warn: dup (9 vertices, 13 edges)"),
        "stdout: {}",
        stdout(&out)
    );

    let broken = write(&dir, "broken.wrfm", BROKEN);
    let out = run(&["check", broken.to_str().unwrap()]);
    assert_eq!(
        out.status.code(),
        Some(2),
        "broken is 2, never 1: {}",
        stderr(&out)
    );
    assert!(
        stdout(&out).starts_with("broken: broken (8 vertices, 13 edges)"),
        "stdout: {}",
        stdout(&out)
    );
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn group_always_prints_json() {
    let dir = scratch("group_always_json");
    let path = write(&dir, "two.wrfm", TWO_GROUPS);
    // The WHOLE model is healthy (two corner loops) -> exit 0.
    let out = run(&["group", path.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    let j = stdout_json(&out);
    let names: Vec<&str> = j["groups"]
        .as_array()
        .unwrap()
        .iter()
        .map(|g| g["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, vec!["body", "head"], "file order, every group");
    // Each group's own health verdict comes from the JSON
    // (`jq -r '.groups[] | "\(.name):\(.verdict)"'`).
    assert_eq!(j["groups"][0]["verdict"], "ok");
    // Scoped to one group, JSON mode untouched.
    let out = run(&["group", path.to_str().unwrap(), "head"]);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    let j = stdout_json(&out);
    assert_eq!(j["groups"].as_array().unwrap().len(), 1);
    assert_eq!(j["groups"][0]["name"], "head");
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn render_stderr_stays_empty_for_warn_and_broken() {
    let dir = scratch("render_stderr_empty");
    // Broken model: the exit code keeps the verdict, stderr carries nothing.
    let path = write(&dir, "broken.wrfm", BROKEN);
    let out = run(&["render", path.to_str().unwrap(), "--format", "grid"]);
    assert_eq!(
        out.status.code(),
        Some(2),
        "exit still carries broken: {}",
        stderr(&out)
    );
    assert!(stdout(&out).contains("# grid"), "stdout: {}", stdout(&out));
    assert!(stderr(&out).is_empty(), "stderr: {}", stderr(&out));

    // Warn model: same silence, exit 1.
    let warn = write(&dir, "warn.wrfm", REDUNDANT_MIDPOINT);
    let out = run(&["render", warn.to_str().unwrap(), "--format", "grid"]);
    assert_eq!(out.status.code(), Some(1), "stderr: {}", stderr(&out));
    assert!(stderr(&out).is_empty(), "stderr: {}", stderr(&out));
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn render_fit_content_fills_the_canvas() {
    let dir = scratch("render_fit_content");
    let path = write(&dir, "cube.wrfm", CUBE);
    let mut args: Vec<&str> = vec![
        "render",
        path.to_str().unwrap(),
        "--views",
        "front",
        "--format",
        "ascii",
        "--width",
        "40",
        "--height",
        "16",
    ];
    let plain = run(&args);
    assert_eq!(plain.status.code(), Some(0), "stderr: {}", stderr(&plain));
    args.extend(["--fit", "content"]);
    let fit = run(&args);
    assert_eq!(fit.status.code(), Some(0), "stderr: {}", stderr(&fit));
    let head = stdout(&fit).lines().next().unwrap_or("").to_string();
    assert!(head.contains("fit=content"), "header: {head}");
    let (p, f) = (lit_dots(&stdout(&plain)), lit_dots(&stdout(&fit)));
    assert!(f > p, "fit must light more dots: plain={p} fit={f}");

    // Priority contract: an explicit --region WINS over --fit content.
    args.extend(["--region", "0.25,0.25,0.75,0.75"]);
    let reg = run(&args);
    assert_eq!(reg.status.code(), Some(0), "stderr: {}", stderr(&reg));
    let head = stdout(&reg).lines().next().unwrap_or("").to_string();
    assert!(head.contains("region=["), "header: {head}");
    assert!(!head.contains("fit=content"), "region wins: {head}");

    // An unknown fit mode is a usage error -> 3.
    let out = run(&[
        "render",
        path.to_str().unwrap(),
        "--views",
        "front",
        "--fit",
        "zoom",
    ]);
    assert_eq!(out.status.code(), Some(3), "stderr: {}", stderr(&out));
    assert!(
        stderr(&out).contains("fit must be"),
        "stderr: {}",
        stderr(&out)
    );
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn edit_weld_merges_vertices_within_tolerance() {
    let dir = scratch("edit_weld_merges");
    let path = write(&dir, "tetra.wrfm", NEAR_DUP_TETRA);

    // `--dedupe` is EXACT: the ~2e-16 twin survives -> warn, exit 1.
    let out = run(&["edit", path.to_str().unwrap(), "--dedupe"]);
    assert_eq!(out.status.code(), Some(1), "stderr: {}", stderr(&out));
    let back = wrfm::WrfmModel::from_str("deduped", &stdout(&out)).expect("parses");
    assert_eq!(back.vertices.len(), 5, "exact dedupe cannot merge noise");

    // `--weld 1e-9` merges the twin; the model becomes a healthy tetra.
    let out = run(&["edit", path.to_str().unwrap(), "--weld", "1e-9"]);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    let so = stdout(&out);
    let back = wrfm::WrfmModel::from_str("welded", &so).expect("parses");
    assert_eq!(back.vertices.len(), 4, "twin merged away");
    assert_eq!(back.edges.len(), 6, "collapsed + duplicate edges dropped");
    let chk = run_stdin(&["check", "-"], &so);
    assert_eq!(chk.status.code(), Some(0), "check stderr: {}", stderr(&chk));

    // A tolerance below the gap merges nothing (still warn).
    let out = run(&["edit", path.to_str().unwrap(), "--weld", "1e-18"]);
    assert_eq!(out.status.code(), Some(1), "stderr: {}", stderr(&out));
    let back = wrfm::WrfmModel::from_str("unwelded", &stdout(&out)).expect("parses");
    assert_eq!(back.vertices.len(), 5, "nothing within 1e-18");
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn edit_weld_usage_errors_exit_three() {
    let dir = scratch("edit_weld_usage_errors");
    let path = write(&dir, "tetra.wrfm", NEAR_DUP_TETRA);
    for tol in ["0", "nan", "inf"] {
        let out = run(&["edit", path.to_str().unwrap(), "--weld", tol]);
        assert_eq!(out.status.code(), Some(3), "tol={tol}: {}", stderr(&out));
        assert!(
            stderr(&out).contains("weld tolerance"),
            "tol={tol}: {}",
            stderr(&out)
        );
    }
    // `-1` never reaches the validator (clap reads it as a flag) — still a
    // usage error, still 3; spelled `--weld=-1` it reaches ours.
    let out = run(&["edit", path.to_str().unwrap(), "--weld", "-1"]);
    assert_eq!(out.status.code(), Some(3), "stderr: {}", stderr(&out));
    let out = run(&["edit", path.to_str().unwrap(), "--weld=-1"]);
    assert_eq!(out.status.code(), Some(3), "stderr: {}", stderr(&out));
    assert!(
        stderr(&out).contains("weld tolerance"),
        "stderr: {}",
        stderr(&out)
    );
    // A non-numeric tolerance is a clap argument error -> 3 as well.
    let out = run(&["edit", path.to_str().unwrap(), "--weld", "abc"]);
    assert_eq!(out.status.code(), Some(3), "stderr: {}", stderr(&out));
    // One edit op per call: --weld does not compose either.
    let out = run(&["edit", path.to_str().unwrap(), "--weld", "1e-9", "--clean"]);
    assert_eq!(out.status.code(), Some(3), "stderr: {}", stderr(&out));
    assert!(
        stderr(&out).contains("exactly ONE"),
        "stderr: {}",
        stderr(&out)
    );
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn clap_argument_errors_exit_three_and_help_exits_zero() {
    let dir = scratch("clap_exit_contract");
    let path = write(&dir, "cube.wrfm", CUBE);
    // Unknown flag: clap's own usage error is forced onto exit 3 — 2 is
    // reserved for `broken`, never an argument mistake.
    let out = run(&["check", path.to_str().unwrap(), "--nope"]);
    assert_eq!(out.status.code(), Some(3), "stderr: {}", stderr(&out));
    assert!(!stderr(&out).is_empty(), "clap prints the usage error");
    assert!(stdout(&out).is_empty(), "no result on a usage error");
    // Help and version are results, not errors.
    let out = run(&["check", "--help"]);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    assert!(stdout(&out).contains("--strict"), "help lists new flags");
    let out = run(&["--version"]);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn help_documents_the_new_flags() {
    for (args, needle) in [
        (vec!["check", "--help"], "--strict"),
        (vec!["check", "--help"], "--format"),
        (vec!["query", "--help"], "--format"),
        (vec!["verify", "--help"], "--expect-closed"),
        (vec!["group", "--help"], "structured facts"),
        (vec!["render", "--help"], "--fit"),
        (vec!["edit", "--help"], "--weld"),
        (vec!["verify", "--help"], "--expect-redundant"),
        (vec!["convert", "--help"], "--from"),
        (vec!["convert", "--help"], "obj -> wrfm"),
    ] {
        let out = run(&args);
        assert_eq!(out.status.code(), Some(0), "{args:?}: {}", stderr(&out));
        assert!(
            stdout(&out).contains(needle),
            "{args:?} help never mentions {needle}"
        );
    }
}

// ---------------------------------------------------------------------------
// Point identity: `check` and `edit --weld` share ONE definition (strictly
// closer than 1e-6 world units), so "check -> repair -> check" converges.
// ---------------------------------------------------------------------------

/// Two vertices ~1e-15 apart (not bit-identical) plus two clean chains.
const NEAR_TWIN: &str = "\
wrfm 1
vertices 4   edges 2

v 1.0 1.0 1.0
v 0.0 0.0 0.0
v 1.0000000000000002 1.0 1.0
v 2.0 0.0 0.0
e 0 1
e 2 3
";

#[test]
fn check_reports_near_duplicates_and_weld_clears_them() {
    let dir = scratch("check_reports_near_duplicates_and_weld_clears_them");
    let path = write(&dir, "near.wrfm", NEAR_TWIN);
    let f = path.to_str().unwrap();

    let out = run(&["check", f]);
    let so = stdout(&out);
    assert!(so.contains("near-duplicate vertices"), "stdout: {so}");
    // NEAR_TWIN also has dangling edges, so this asserts the tolerance line
    // stays attached to the near-duplicate list instead of repeating once
    // per problem section.
    assert_eq!(so.matches("tolerance: 1e-6").count(), 1, "stdout: {so}");

    // Near-duplicates are NOT bit-identical: the exact counter stays empty.
    let j = stdout_json(&run(&["check", f, "--format", "json"]));
    assert_eq!(
        j["issues"]["duplicate_vertices"].as_array().unwrap().len(),
        0,
        "{j}"
    );
    assert_eq!(
        j["issues"]["near_duplicate_vertices"]
            .as_array()
            .unwrap()
            .len(),
        1,
        "{j}"
    );
    assert_eq!(j["tolerance"], 1e-6);

    // `--dedupe` is bit-exact and cannot clear a near-duplicate...
    let deduped = run(&["edit", f, "--dedupe"]);
    let after = run_stdin(&["check", "-"], &stdout(&deduped));
    assert!(
        stdout(&after).contains("near-duplicate vertices"),
        "--dedupe must not clear a near-duplicate: {}",
        stdout(&after)
    );

    // ...`--weld 1e-6` is the repair, and re-checking converges.
    let welded = run(&["edit", f, "--weld", "1e-6"]);
    let after = run_stdin(&["check", "-", "--format", "json"], &stdout(&welded));
    let j = stdout_json(&after);
    assert_eq!(
        j["issues"]["near_duplicate_vertices"]
            .as_array()
            .unwrap()
            .len(),
        0,
        "weld must clear the finding: {j}"
    );
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn check_finds_near_duplicates_across_a_cell_boundary() {
    // Regression: 2e-7 apart, straddling the old single-bucket boundary —
    // the code used to report "0 duplicate vertices" while calling the very
    // same pair a zero-length edge.
    let dir = scratch("check_finds_near_duplicates_across_a_cell_boundary");
    let straddle = "\
wrfm 1
vertices 2   edges 1

v 4.99e-05 0 0
v 5.0100000000000005e-05 0 0
e 0 1
";
    let path = write(&dir, "straddle.wrfm", straddle);
    let j = stdout_json(&run(&["check", path.to_str().unwrap(), "--format", "json"]));
    assert_eq!(
        j["issues"]["near_duplicate_vertices"]
            .as_array()
            .unwrap()
            .len(),
        1,
        "{j}"
    );
    // Both statements about the same pair agree now.
    assert_eq!(
        j["issues"]["zero_length_edges"].as_array().unwrap().len(),
        1
    );
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn point_tolerance_boundary_is_strict() {
    let dir = scratch("point_tolerance_boundary_is_strict");
    let exact = "\
wrfm 1
vertices 2   edges 1

v 0 0 0
v 1e-6 0 0
e 0 1
";
    let path = write(&dir, "exact.wrfm", exact);
    let f = path.to_str().unwrap();
    let j = stdout_json(&run(&["check", f, "--format", "json"]));
    assert_eq!(
        j["issues"]["near_duplicate_vertices"]
            .as_array()
            .unwrap()
            .len(),
        0,
        "exactly 1e-6 apart is not the same point: {j}"
    );
    assert_eq!(
        j["issues"]["zero_length_edges"].as_array().unwrap().len(),
        0
    );
    let welded = run(&["edit", f, "--weld", "1e-6"]);
    assert!(
        stdout(&welded).contains("vertices 2"),
        "weld must not merge a pair exactly tol apart: {}",
        stdout(&welded)
    );
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn check_json_is_the_machine_form() {
    let dir = scratch("check_json_is_the_machine_form");
    for (name, fixture) in [("cube.wrfm", CUBE), ("broken.wrfm", BROKEN)] {
        let path = write(&dir, name, fixture);
        let out = run(&["check", path.to_str().unwrap(), "--format", "json"]);
        assert!(stderr(&out).is_empty(), "stderr: {}", stderr(&out));
        let j = stdout_json(&out);
        for key in [
            "name",
            "source",
            "vertices",
            "edges",
            "tolerance",
            "verdict",
            "summary",
            "issues",
            "quality",
        ] {
            assert!(j.get(key).is_some(), "check --format json lacks {key}: {j}");
        }
        assert_eq!(j["tolerance"], 1e-6);
        // Every category is a list and every entry an object — the isolated
        // vertices used to be bare integers, the one shape outlier.
        for (k, v) in j["issues"].as_object().unwrap() {
            let list = v.as_array().unwrap_or_else(|| panic!("{k} must be a list"));
            for item in list {
                assert!(item.is_object(), "{k} entries must be objects: {item}");
            }
        }
    }
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn query_profile_json_edge_cover_is_ordered() {
    let dir = scratch("query_profile_json_edge_cover_is_ordered");
    let path = write(&dir, "cube.wrfm", CUBE);
    let out = run(&[
        "query",
        path.to_str().unwrap(),
        "profile",
        "--format",
        "json",
    ]);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    let j = stdout_json(&out);
    for axis in ["x", "y", "z"] {
        let c = j["profile"][axis]["edge_cover"].as_array().unwrap();
        let (lo, hi) = (c[0].as_f64().unwrap(), c[1].as_f64().unwrap());
        assert!(
            lo <= hi,
            "{axis}: edge_cover must be ordered, got {lo}..{hi}"
        );
        assert_eq!((lo, hi), (0.0, 1.0), "{axis}: union of the edges' extents");
        assert_eq!(j["profile"][axis]["span"], 1.0);
    }
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn info_bounds_matches_geometry_bounds() {
    let dir = scratch("info_bounds_matches_geometry_bounds");
    let path = write(&dir, "cube.wrfm", CUBE);
    let f = path.to_str().unwrap();
    let info = stdout_json(&run(&["info", f]));
    let geo = stdout_json(&run(&["geometry", f]));
    assert_eq!(info["bounds"], geo["bounds"], "one shape for bounds");
    assert!(info["bounds"]["size"].is_array(), "info.bounds needs size");
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn diff_reports_group_membership_changes() {
    let dir = scratch("diff_reports_group_membership_changes");
    let a = write(&dir, "a.wrfm", TWO_GROUPS);
    // Same vertices, same edges — vertex 2 moved from `body` to `head`.
    let regrouped = "\
wrfm 1
vertices 6   edges 6

group body
  v 0 0 0
  v 1 0 0
group head
  v 0 1 0
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
    let b = write(&dir, "b.wrfm", regrouped);
    let out = run(&[
        "diff",
        a.to_str().unwrap(),
        b.to_str().unwrap(),
        "--format",
        "json",
    ]);
    let j = stdout_json(&out);
    let changed = j["groups"]["membership_changed"].as_array().unwrap();
    let body = changed
        .iter()
        .find(|c| c["name"] == "body")
        .unwrap_or_else(|| panic!("body must be reported: {j}"));
    assert_eq!(body["only_in_a"], serde_json::json!([2]));
    assert_eq!(body["only_in_b"], serde_json::json!([]));
    let head = changed
        .iter()
        .find(|c| c["name"] == "head")
        .unwrap_or_else(|| panic!("head must be reported: {j}"));
    assert_eq!(head["only_in_b"], serde_json::json!([2]));
    // The geometry is identical, so the vertex/edge diff stays empty.
    assert_eq!(j["vertices"]["moved_count"], 0);
    assert!(j["edges"]["added"].as_array().unwrap().is_empty());
    // The text form names the change instead of silently reporting nothing.
    let t = run(&["diff", a.to_str().unwrap(), b.to_str().unwrap()]);
    assert!(
        stdout(&t).contains("changed: body"),
        "stdout: {}",
        stdout(&t)
    );
    fs::remove_dir_all(&dir).ok();
}

// ---------------------------------------------------------------------------
// convert: v1 is one direction (obj -> wrfm). The direction and both
// formats are explicit parameters; anything else is a usage error (exit 3),
// and a successful conversion emits canonical .wrfm text on stdout with the
// health tier of the PRODUCED model on the exit code (like transform/edit).
// ---------------------------------------------------------------------------

/// A minimal OBJ: three vertices and one triangular face.
const TRI_OBJ: &str = "\
v 0 0 0
v 1 0 0
v 0 1 0
f 1 2 3
";

#[test]
fn convert_obj_to_wrfm_writes_canonical_wrfm_to_stdout() {
    let dir = scratch("convert_obj_to_wrfm");
    let path = write(&dir, "tri.obj", TRI_OBJ);
    let out = run(&[
        "convert",
        "--from",
        "obj",
        "--to",
        "wrfm",
        path.to_str().unwrap(),
    ]);
    // The tier (0..=2) is the health verdict; anything else means no
    // result was produced (3) or a panic (101).
    let code = out.status.code().unwrap_or(3);
    assert!(code <= 2, "conversion produced no result: {}", stderr(&out));
    let s = stdout(&out);
    assert!(
        s.starts_with("wrfm 1\nvertices 3   edges 3\n"),
        "canonical wrfm on stdout, got: {s}"
    );
    let back = wrfm::WrfmModel::from_str("roundtrip", &s).expect("output parses");
    assert_eq!(back.edges.len(), 3, "the face's ring survives");
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn convert_rejects_an_unsupported_direction_with_exit_three() {
    let out = run(&[
        "convert",
        "--from",
        "wrfm",
        "--to",
        "obj",
        "no-such-file.wrfm",
    ]);
    assert_eq!(out.status.code(), Some(3), "stderr: {}", stderr(&out));
    assert!(stdout(&out).is_empty(), "no result on a usage error");
    assert!(
        stderr(&out).contains("unsupported"),
        "stderr: {}",
        stderr(&out)
    );
}

#[test]
fn convert_reads_an_obj_stream_from_stdin() {
    let out = run_stdin(&["convert", "--from", "obj", "--to", "wrfm", "-"], TRI_OBJ);
    let code = out.status.code().unwrap_or(3);
    assert!(
        code <= 2,
        "stdin conversion produced no result: {}",
        stderr(&out)
    );
    assert!(
        stdout(&out).starts_with("wrfm 1\nvertices 3   edges 3\n"),
        "got: {}",
        stdout(&out)
    );
}

// ---------------------------------------------------------------------------
// A consumer that closes the pipe early (head / an early-exiting jq) is not
// an error: the remaining stdout writes are dropped, no panic, and the verdict
// still reaches the exit code (see src/output.rs).
// ---------------------------------------------------------------------------

/// Spawn `wrfm <args>`, read a little of stdout, then close the read end so
/// the next write really hits `EPIPE`.
fn run_with_closed_pipe(args: &[&str]) -> Output {
    let mut child = Command::new(bin())
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn wrfm");
    {
        let mut out = child.stdout.take().expect("stdout");
        let mut buf = [0u8; 64];
        let _ = out.read(&mut buf);
    } // dropping `out` closes the read end
    child.wait_with_output().expect("wait wrfm")
}

/// Big enough that the render cannot fit in the 64 KiB pipe buffer.
fn big_render_args(model: &str) -> Vec<String> {
    [
        "render",
        model,
        "--views",
        "front,back,top",
        "--width",
        "300",
        "--height",
        "300",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

#[test]
fn closed_pipe_is_not_a_panic() {
    let dir = scratch("closed_pipe_is_not_a_panic");
    let path = write(&dir, "cube.wrfm", CUBE);
    let args = big_render_args(path.to_str().unwrap());
    let argv: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    // Premise: the output really exceeds the 64 KiB pipe buffer, otherwise
    // the child would finish before the reader closes and the test would
    // pass vacuously.
    let full = run(&argv);
    assert!(
        full.stdout.len() > 64 * 1024,
        "test premise: output must exceed the pipe buffer, got {} bytes",
        full.stdout.len()
    );
    let out = run_with_closed_pipe(&argv);
    assert!(
        !stderr(&out).contains("panicked"),
        "stderr must stay clean: {}",
        stderr(&out)
    );
    assert_eq!(
        out.status.code(),
        Some(0),
        "a closed pipe must not change the verdict: {}",
        stderr(&out)
    );
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn verdict_survives_a_closed_pipe() {
    let dir = scratch("verdict_survives_a_closed_pipe");
    let path = write(&dir, "broken.wrfm", BROKEN);
    let args = big_render_args(path.to_str().unwrap());
    let argv: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    let out = run_with_closed_pipe(&argv);
    assert_eq!(
        out.status.code(),
        Some(2),
        "the broken verdict still travels on the exit code: {}",
        stderr(&out)
    );
    fs::remove_dir_all(&dir).ok();
}
