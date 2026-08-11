mod check;
mod edit;
mod geometry;
mod load;
mod query;
mod render;
mod transform;
mod view;

use clap::{Parser, Subcommand};
use serde_json::json;
use std::io::Write;

/// The complete `.wrfm` v1 format spec, as printed by `wrfm format`.
const FORMAT_SPEC: &str = r#".wrfm format v1 — plain text, one element per line
===================================================

MAGIC  (line 1, required)   wrfm 1
COUNTS (required, next)     vertices <N>   edges <M>
   N = total 'v' lines, M = total 'e' lines (whole file);
   a mismatch is a format error (never silently tolerated).
VERTEX   v <x> <y> <z>       index = file order, 0-based, full f64
EDGE     e <i> <j>           0-based, both in range 0..N
GROUP    group <name>        optional; opens a section (example below)
COMMENT  # to end of line    ignored (except before the magic line)
COORDS   write the shortest decimal that round-trips (0.5, 1.4142135623730951)
AXIS     Y-UP: +Y is vertical (height); X and Z form the ground plane.
         A model built with Z as height appears lying on its side.

GROUP EXAMPLE — groups are SECTIONS of the ONE global vertex list. The v
lines under a group RE-LIST the global vertices that belong to it (in
file order); indexing NEVER restarts in a group.

  wrfm 1
  vertices 5   edges 4

  group body
    v 0 0 0        # global vertex 0
    v 1 0 0        # global vertex 1
    v 1 1 0        # global vertex 2
  group head
    v 0.5 2 0.25   # global vertex 3
    v 0 1 2        # global vertex 4

  e 0 1            # edges may be written anywhere (they are global)
  e 1 2
  e 2 0
  e 0 4            # index 4 = the vertex in group head (GLOBAL index)

STREAMS (three-channel contract — read this before piping):
  stdout = pure result (model text / JSON / render) — the data.
  stderr = "[wrfm] check: ..." report (warnings / broken) — diagnostics.
  exit   = 0 ok/warn · 1 result produced but check broken · 2 no result.
  To pipe JSON/data do NOT merge stderr: use `wrfm info a.wrfm 2>/dev/null`
  (never `2>&1`) so the report cannot corrupt the data stream.

Authoritative spec: FORMAT.md (wrfm crate docs).
"#;

#[derive(Parser, Debug)]
#[command(
    name = "wrfm",
 version,
    about = "Read-only streaming CLI for .wrfm 3D wireframe models",
 after_help = "Every model-input command accepts '-' for stdin. Nothing writes a file: use shell redirection.\nExit codes: 0 ok/warn · 1 result produced but check broken · 2 could not produce a result.\nCompose atomic operations with pipes, e.g.:\n  wrfm edit m.wrfm --extract-group body | wrfm transform - --scale 2 --rotate-y 45\n  wrfm transform m.wrfm --mirror x | wrfm edit - --clean\nFormat spec: wrfm format (self-contained; no source needed)."
)]
struct Cli {
 #[command(subcommand)]
 command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
/// Health check (L2 geometry): duplicate vertices, zero-length / duplicate edges, dangling edges, isolated and non-manifold vertices.
 Check {
 /// Path to the .wrfm file, or '-' to read the model from stdin.
 file: String,
/// Scope the check to one group (the verdict is for the part only).
 #[arg(long)]
 group: Option<String>,
/// Upgrade warning-level issues (duplicates / dangling / non-manifold) to a broken verdict — only a fully clean model
 #[arg(long)]
 strict: bool,
 },
 /// Print model metadata (name/version/vertices/edges/groups/bounds) as JSON.
 Info {
 /// Path to the .wrfm file, or '-' to read the model from stdin.
 file: String,
 },
 /// Describe named group(s) as structured facts (JSON).
 Group {
 /// Path to the .wrfm file, or '-' to read the model from stdin.
 file: String,
 /// Group name; omit to describe every group.
 name: Option<String>,
 },
/// Print structured geometry facts (bounds / centroid / PCA / topology / edge stats / closure / symmetry / alignment / spans) as JSON.
 Geometry {
 /// Path to the .wrfm file, or '-' to read the model from stdin.
 file: String,
 /// Include the PCA eigenvalues (full report).
 #[arg(long)]
 full: bool,
 },
/// Run a safe geometric query (extents | topology | edge_stats | profile | cross_section | vertices | distance | connectivity).
 Query {
 /// Path to the .wrfm file, or '-' to read the model from stdin.
 file: String,
/// extents | topology | edge_stats | profile | cross_section | vertices | distance | connectivity.
 query: String,
 /// Scope the query to one group (its vertices + the edges touching it).
 #[arg(long)]
 group: Option<String>,
 /// Plane z value for cross_section.
 #[arg(long, default_value_t = 0.0)]
 at: f64,
/// Inclusive 0-based global index range "a,b" for `vertices` (mutually exclusive with `--group`).
 #[arg(long, default_value = "")]
 range: String,
 /// First global vertex index for distance / connectivity.
 #[arg(long)]
 from: Option<usize>,
 /// Second global vertex index for distance / connectivity.
 #[arg(long)]
 to: Option<usize>,
 },
/// Exact per-view facts (visible/occluded edges, silhouette, depth order) at a camera (pitch/yaw/roll/dist/pan) as JSON.
 View {
 /// Path to the .wrfm file, or '-' to read the model from stdin.
 file: String,
 /// Pitch around the world X axis (degrees).
 #[arg(long, default_value_t = 0.0)]
 pitch: f64,
 /// Yaw around the world Y axis (turntable, degrees).
 #[arg(long, default_value_t = 0.0)]
 yaw: f64,
 /// Roll around the view axis (degrees).
 #[arg(long, default_value_t = 0.0)]
 roll: f64,
 /// Auto camera distance from the model extent (default true).
 #[arg(long, default_value_t = true, action = clap::ArgAction::Set)]
 auto_dist: bool,
 /// Explicit camera distance (world units); used when auto_dist=false.
 #[arg(long)]
 dist: Option<f64>,
 /// Aim-point X offset (world units).
 #[arg(long, default_value_t = 0.0)]
 pan_x: f64,
 /// Aim-point Y offset (world units).
 #[arg(long, default_value_t = 0.0)]
 pan_y: f64,
 /// Scope the view to one group.
 #[arg(long)]
 group: Option<String>,
 /// Max visible/occluded edges listed (sorted near-to-far).
 #[arg(long, default_value_t = 100)]
 limit: usize,
 },
 /// Render the model to terminal text (braille / ascii / grid).
 Render {
 /// Path to the .wrfm file, or '-' to read the model from stdin.
 file: String,
/// Comma-separated views (front,back,left,right,top,bottom,iso, side); '' = single frame with the explicit --pitch/--yaw/--roll.
 #[arg(long, default_value = "front,back,left,right,top,bottom")]
 views: String,
 /// braille | ascii | grid | both.
 #[arg(long, default_value = "braille")]
 format: String,
 /// Canvas width in characters.
 #[arg(long, default_value_t = 60)]
 width: u16,
 /// Canvas height in characters.
 #[arg(long, default_value_t = 24)]
 height: u16,
 /// Density grid columns (format=grid).
 #[arg(long, default_value_t = 16)]
 grid_w: usize,
 /// Density grid rows (format=grid).
 #[arg(long, default_value_t = 8)]
 grid_h: usize,
 /// overview | standard | fine (preset; overrides format/size/grid).
 #[arg(long)]
 detail: Option<String>,
 /// Zoom into a sub-region 'x0,y0,x1,y1' (normalized 0-1).
 #[arg(long)]
 region: Option<String>,
 /// Auto camera distance from the model extent (default true).
 #[arg(long, default_value_t = true, action = clap::ArgAction::Set)]
 auto_dist: bool,
 /// Explicit camera distance (world units); used when auto_dist=false.
 #[arg(long)]
 dist: Option<f64>,
 /// Aim-point X offset (world units).
 #[arg(long, default_value_t = 0.0)]
 pan_x: f64,
 /// Aim-point Y offset (world units).
 #[arg(long, default_value_t = 0.0)]
 pan_y: f64,
/// World-frame pitch in degrees (the fork's HUD pitch). Used ONLY for a single explicit-angle frame: --views ''.
 #[arg(long, default_value_t = 0.0)]
 pitch: f64,
/// World-frame yaw in degrees (turntable, the fork's HUD yaw). Used ONLY for a single explicit-angle frame: --views ''.
 #[arg(long, default_value_t = 0.0)]
 yaw: f64,
/// Roll in degrees around the view axis. Used ONLY for a single explicit-angle frame: --views ''.
 #[arg(long, default_value_t = 0.0)]
 roll: f64,
 /// Render ONLY this group (its vertices + the edges touching it).
 #[arg(long)]
 group: Option<String>,
 /// Refuse renders whose estimated token cost exceeds this number.
 #[arg(long)]
 budget: Option<u64>,
 },
/// Apply an affine transform and print the result as canonical .wrfm text.
 Transform {
 /// Path to the .wrfm file, or '-' to read the model from stdin.
 file: String,
 /// Uniform scale factor (applied around the pivot, after rotation).
 #[arg(long, default_value_t = 1.0)]
 scale: f64,
/// Per-axis scale factor; the total per-axis factor is `--scale * --scale-x` etc. (each default 1.0).
 #[arg(long, default_value_t = 1.0)]
 scale_x: f64,
/// Per-axis scale factor; total per-axis factor is `--scale * --scale-z` etc.
 #[arg(long, default_value_t = 1.0)]
 scale_y: f64,
/// Per-axis scale factor; total per-axis factor is `--scale * --scale-z` etc.
 #[arg(long, default_value_t = 1.0)]
 scale_z: f64,
 /// Shear K: x' = x + K·y (upper-triangular, applied after rotation).
 #[arg(long, default_value_t = 0.0)]
 shear_xy: f64,
 /// Shear K: x' = x + K·z (upper-triangular, applied after rotation).
 #[arg(long, default_value_t = 0.0)]
 shear_xz: f64,
 /// Shear K: y' = y + K·z (upper-triangular, applied after rotation).
 #[arg(long, default_value_t = 0.0)]
 shear_yz: f64,
/// Pivot point the whole transform happens about: origin | center | bbox | 'x,y,z' (default origin).
 #[arg(long, default_value = "origin")]
 pivot: String,
/// Pivot about the bbox centre AND move it to the origin after the transform (overrides --pivot; --translate still applies).
 #[arg(long)]
 center: bool,
 /// Rotate the model's longest PCA principal axis onto x | y | z.
 #[arg(long)]
 align: Option<String>,
/// Uniformly scale so the model's largest bbox span equals SIZE (must be > 0).
 #[arg(long)]
 normalize: Option<f64>,
 /// Rotation around the world X axis (degrees).
 #[arg(long, default_value_t = 0.0)]
 rotate_x: f64,
 /// Rotation around the world Y axis (degrees).
 #[arg(long, default_value_t = 0.0)]
 rotate_y: f64,
 /// Rotation around the world Z axis (degrees).
 #[arg(long, default_value_t = 0.0)]
 rotate_z: f64,
 /// Arbitrary rotation axis 'x,y,z' (non-zero).
 #[arg(long)]
 rotate_axis: Option<String>,
 /// Degrees for the arbitrary-axis rotation.
 #[arg(long, default_value_t = 0.0)]
 rotate_angle: f64,
 /// Translation 'x,y,z' (applied last).
 #[arg(long)]
 translate: Option<String>,
/// Mirror across the plane perpendicular to x | y | z (applied after the linear part, in the pivot frame).
 #[arg(long)]
 mirror: Option<String>,
 },
 /// Apply exactly one topology edit and print the result as .wrfm text.
 Edit {
 /// Path to the .wrfm file, or '-' to read the model from stdin.
 file: String,
 /// Delete vertices 'a,b,c' (and every edge touching them).
 #[arg(long)]
 delete_vertices: Option<String>,
 /// Delete edges by 0-based edge index '0,2,5'.
 #[arg(long)]
 delete_edges: Option<String>,
 /// Extract a group as its own model.
 #[arg(long)]
 extract_group: Option<String>,
/// Remove degenerate topology (zero-length / dangling edges, isolated vertices) until every vertex has degree >= 2.
 #[arg(long)]
 clean: bool,
/// Merge duplicate vertices (exact position) and drop duplicate / zero-length edges.
 #[arg(long)]
 dedupe: bool,
 /// Merge another model (path or '-') into this one.
 #[arg(long)]
 merge: Option<String>,
 },
/// Compare two models (baseline vs edited): density-grid deltas (text) or a structured diff (JSON). No check semantics.
 Diff {
 /// Baseline .wrfm path, or '-' for stdin (at most one side).
 a: String,
 /// Edited .wrfm path, or '-' for stdin (at most one side).
 b: String,
 /// text | json.
 #[arg(long, default_value = "text")]
 format: String,
 },
/// Print the complete .wrfm v1 format spec (magic, header, groups, precision, streams) to stdout. Learn the format from the CLI itself —
 Format,
}

fn main() {
 let cli = Cli::parse();
 let code = match cli.command {
 Command::Check {
 file,
 group,
 strict,
 } => cmd_check(&file, group.as_deref(), strict),
 Command::Info { file } => cmd_info(&file),
 Command::Group { file, name } => cmd_group(&file, name.as_deref()),
 Command::Geometry { file, full } => cmd_geometry(&file, full),
 Command::Query {
 file,
 query,
 group,
 at,
 range,
 from,
 to,
 } => cmd_query(&file, &query, group.as_deref(), at, &range, from, to),
 Command::View {
 file,
 pitch,
 yaw,
 roll,
 auto_dist,
 dist,
 pan_x,
 pan_y,
 group,
 limit,
 } => cmd_view(
 &file,
 pitch,
 yaw,
 roll,
 auto_dist,
 dist,
 pan_x,
 pan_y,
 group.as_deref(),
 limit,
 ),
 Command::Render {
 file,
 views,
 format,
 width,
 height,
 grid_w,
 grid_h,
 detail,
 region,
 auto_dist,
 dist,
 pan_x,
 pan_y,
 pitch,
 yaw,
 roll,
 group,
 budget,
 } => cmd_render(
 &file,
 &views,
 &format,
 width,
 height,
 grid_w,
 grid_h,
 detail.as_deref(),
 region.as_deref(),
 auto_dist,
 dist,
 pan_x,
 pan_y,
 pitch,
 yaw,
 roll,
 group.as_deref(),
 budget,
 ),
 Command::Transform {
 file,
 scale,
 scale_x,
 scale_y,
 scale_z,
 shear_xy,
 shear_xz,
 shear_yz,
 pivot,
 center,
 align,
 normalize,
 rotate_x,
 rotate_y,
 rotate_z,
 rotate_axis,
 rotate_angle,
 translate,
 mirror,
 } => cmd_transform(
 &file,
 scale,
 scale_x,
 scale_y,
 scale_z,
 shear_xy,
 shear_xz,
 shear_yz,
 &pivot,
 center,
 align.as_deref(),
 normalize,
 rotate_x,
 rotate_y,
 rotate_z,
 rotate_axis.as_deref(),
 rotate_angle,
 translate.as_deref(),
 mirror.as_deref(),
 ),
 Command::Edit {
 file,
 delete_vertices,
 delete_edges,
 extract_group,
 clean,
 dedupe,
 merge,
 } => cmd_edit(
 &file,
 delete_vertices.as_deref(),
 delete_edges.as_deref(),
 extract_group.as_deref(),
 clean,
 dedupe,
 merge.as_deref(),
 ),
 Command::Diff { a, b, format } => cmd_diff(&a, &b, &format),
 Command::Format => cmd_format(),
 };
 std::process::exit(code);
}

// Shared helpers

/// Load a model through the L1 gate; any failure (I/O, parse, usage) is reported on stderr and mapped to exit 2.
fn load(source: &str) -> Result<load::Loaded, i32> {
 match load::load(source) {
 Ok(l) => Ok(l),
 Err(e) => {
            eprintln!("{}", e.0);
 Err(2)
 }
 }
}

/// The stderr `[wrfm] check:` note. `always` forces the note even for `ok` (transform/edit — the check report is part of their contract);
fn check_note(verdict: &str, summary: &str, always: bool) {
    if always || verdict != "ok" {
        if verdict == "ok" {
            eprintln!("[wrfm] check: ok");
 } else {
            eprintln!("[wrfm] check: {verdict} ({summary})");
 }
 }
}

/// Exit code from the L2 verdict: broken → 1, ok/warn → 0.
fn exit_for(verdict: &str) -> i32 {
    if verdict == "broken" { 1 } else { 0 }
}

/// Verdict of the L2 check for a loaded model, plus the summary line.
fn verdict_of(c: &serde_json::Value) -> (&str, &str) {
 (
        c["verdict"].as_str().unwrap_or("ok"),
        c["summary"].as_str().unwrap_or(""),
 )
}

/// Resolve a group by name (first match wins, as authored).
fn resolve_group<'a>(
 groups: &'a [wrfm::Group],
 want: &str,
 source: &str,
) -> Result<&'a wrfm::Group, i32> {
 groups.iter().find(|g| g.name == want).ok_or_else(|| {
        eprintln!("error: group '{want}' not found in '{source}'");
 2
 })
}

/// Build the `RenderOptions` for `wrfm render`, parsing the string options (views / format / detail / region) with usage-error → exit 2 mapping.
#[allow(clippy::too_many_arguments)]
fn render_opts(
 views: &str,
 format: &str,
 width: u16,
 height: u16,
 grid_w: usize,
 grid_h: usize,
 detail: Option<&str>,
 region: Option<&str>,
 auto_dist: bool,
 dist: Option<f64>,
 pan_x: f64,
 pan_y: f64,
 pitch: f64,
 yaw: f64,
 roll: f64,
) -> Result<render::RenderOptions, i32> {
 let parsed_views = parse_views(views)?;
 let fmt = render::Format::parse(format).map_err(|e| usage(&e))?;
 let det = match detail {
 None => None,
 Some(s) => Some(render::Detail::parse(s).map_err(|e| usage(&e))?),
 };
 let reg = match region {
 None => None,
 Some(s) => parse_region(s)?,
 };
 let mut opts = render::RenderOptions {
 width,
 height,
 pitch_deg: pitch,
 yaw_deg: yaw,
 roll_deg: roll,
 format: fmt,
 auto_dist,
 dist,
 pan_x,
 pan_y,
 views: parsed_views,
 grid_w,
 grid_h,
 region: reg,
 };
 if let Some(d) = det {
 d.apply(&mut opts);
 }
 Ok(opts)
}

/// Parse a comma-separated view list (`""` = empty = explicit angles).
fn parse_views(s: &str) -> Result<Vec<render::View>, i32> {
 if s.trim().is_empty() {
 return Ok(Vec::new());
 }
 s.split(',')
 .map(|part| render::View::parse(part).map_err(|e| usage(&e)))
 .collect()
}

/// Parse a region `"x0,y0,x1,y1"` (normalized 0-1 canvas fractions).
fn parse_region(s: &str) -> Result<Option<[f64; 4]>, i32> {
 if s.trim().is_empty() {
 return Ok(None);
 }
 let parts: Vec<f64> = s
 .split(',')
 .map(|p| {
 p.trim().parse::<f64>().map_err(|_| {
 usage(&format!(
                    "region must be 4 numbers 'x0,y0,x1,y1', got '{s}'"
 ))
 })
 })
 .collect::<Result<_, _>>()?;
 if parts.len() != 4 {
 return Err(usage(&format!(
            "region must be 4 numbers 'x0,y0,x1,y1', got '{s}'"
 )));
 }
 Ok(Some([parts[0], parts[1], parts[2], parts[3]]))
}

/// Parse a comma-separated list of 0-based non-negative indices.
fn parse_index_list(s: &str, what: &str) -> Result<Vec<usize>, i32> {
 let s = s.trim();
 if s.is_empty() {
 return Ok(Vec::new());
 }
 s.split(',')
 .map(|p| {
 p.trim().parse::<usize>().map_err(|_| {
 usage(&format!(
                    "{what} must be comma-separated non-negative integers, got '{s}'"
 ))
 })
 })
 .collect()
}

/// Parse a vertex `--range "a,b"` (inclusive, 0-based) and validate it against the model's vertex count (malformed / out of range -> exit 2).
fn parse_range(s: &str, n: usize) -> Result<(usize, usize), i32> {
 let parts: Vec<usize> = s
 .split(',')
 .map(|p| {
 p.trim().parse::<usize>().map_err(|_| {
 usage(&format!(
                    "range must be 'a,b' of non-negative integers, got '{s}'"
 ))
 })
 })
 .collect::<Result<_, _>>()?;
 if parts.len() != 2 {
 return Err(usage(&format!(
            "range must be 'a,b' (two indices), got '{s}'"
 )));
 }
 let (a, b) = (parts[0], parts[1]);
 if a > b {
        return Err(usage(&format!("range 'a,b' requires a <= b, got '{s}'")));
 }
 if b >= n {
 return Err(usage(&format!(
            "range index {b} out of range (vertices={n})"
 )));
 }
 Ok((a, b))
}

/// Parse `translate` / `rotate_axis` (comma-separated 3 numbers).
fn parse_xyz(s: &str, what: &str) -> Result<[f64; 3], i32> {
 let parts: Vec<f64> = s
 .split(',')
 .map(|p| {
 p.trim()
 .parse::<f64>()
                .map_err(|_| usage(&format!("{what} must be 'x,y,z', got '{s}'")))
 })
 .collect::<Result<_, _>>()?;
 if parts.len() != 3 {
        return Err(usage(&format!("{what} must be 'x,y,z', got '{s}'")));
 }
 Ok([parts[0], parts[1], parts[2]])
}

/// Parse `mirror` (x | y | z).
fn parse_mirror(s: &str) -> Result<Option<usize>, i32> {
 match s.trim() {
        "" => Ok(None),
        "x" => Ok(Some(0)),
        "y" => Ok(Some(1)),
        "z" => Ok(Some(2)),
 other => Err(usage(&format!(
            "mirror must be x|y|z or empty, got '{other}'"
 ))),
 }
}

/// A usage/argument error: exit 2 with the message on stderr.
fn usage(msg: &str) -> i32 {
    eprintln!("error: {msg}");
 2
}

/// Print pretty JSON to stdout (a contract result).
fn print_json(v: &serde_json::Value) {
    let s = serde_json::to_string_pretty(v).unwrap_or_else(|_| "{}".to_string());
    println!("{s}");
}

// Commands

fn cmd_check(file: &str, group: Option<&str>, strict: bool) -> i32 {
 let loaded = match load(file) {
 Ok(l) => l,
 Err(code) => return code,
 };
 let model = match group {
 Some(want) => {
 let g = match resolve_group(&loaded.groups, want, &loaded.source) {
 Ok(g) => g,
 Err(code) => return code,
 };
 render::submodel_for_group(&loaded.model, g)
 }
 None => loaded.model.clone(),
 };
 let c = check::check(&model, strict);
 print!(
        "{}",
 check::report_text(&loaded.name, model.vertices.len(), model.edges.len(), &c)
 );
 let _ = std::io::stdout().flush();
 // check's contract differs: warn and broken both exit 1.
    if c["verdict"].as_str().unwrap_or("ok") == "ok" {
 0
 } else {
 1
 }
}
fn cmd_info(file: &str) -> i32 {
 let loaded = match load(file) {
 Ok(l) => l,
 Err(code) => return code,
 };
 let c = check::check(&loaded.model, false);
 let (verdict, summary) = verdict_of(&c);
 let (min, max) = render::bounds(&loaded.model);
 let center = [
 (min[0] + max[0]) / 2.0,
 (min[1] + max[1]) / 2.0,
 (min[2] + max[2]) / 2.0,
 ];
 let groups: Vec<serde_json::Value> = loaded
 .groups
 .iter()
 .map(|g| {
 json!({
                "name": g.name,
                "vertex_start": g.vertex_start,
                "vertex_end": g.vertex_end,
 })
 })
 .collect();
 print_json(&json!({
        "name": loaded.name,
        "source": loaded.source,
        "version": loaded.version,
        "vertices": loaded.model.vertices.len(),
        "edges": loaded.model.edges.len(),
        "bytes": loaded.bytes,
        "groups": groups,
        "bounds": {
            "min": [min[0], min[1], min[2]],
            "max": [max[0], max[1], max[2]],
            "center": [center[0], center[1], center[2]],
 },
 }));
 check_note(verdict, summary, false);
 exit_for(verdict)
}

fn cmd_group(file: &str, name: Option<&str>) -> i32 {
 let loaded = match load(file) {
 Ok(l) => l,
 Err(code) => return code,
 };
 let c = check::check(&loaded.model, false);
 let (verdict, summary) = verdict_of(&c);
 // Selected group POSITIONS in the full group list (resolve_group returns
 // a reference into `loaded.groups`, so identity locates it exactly even
 // with duplicate names — first match wins, consistent with the rest).
 let selected: Vec<usize> = match name {
 Some(want) => {
 let g = match resolve_group(&loaded.groups, want, &loaded.source) {
 Ok(g) => g,
 Err(code) => return code,
 };
 let idx = loaded
 .groups
 .iter()
 .position(|x| std::ptr::eq(x, g))
                .expect("resolve_group returns a reference into loaded.groups");
 vec![idx]
 }
 None => (0..loaded.groups.len()).collect(),
 };
 let adj = render::adjacent_groups(&loaded.model, &loaded.groups);
 let entries: Vec<serde_json::Value> = selected
 .iter()
 .map(|&i| render::group_facts(&loaded.model, &loaded.groups[i], &adj[i]))
 .collect();
    print_json(&json!({ "name": loaded.name, "groups": entries }));
 check_note(verdict, summary, false);
 exit_for(verdict)
}

fn cmd_geometry(file: &str, full: bool) -> i32 {
 let loaded = match load(file) {
 Ok(l) => l,
 Err(code) => return code,
 };
 let c = check::check(&loaded.model, false);
 let (verdict, summary) = verdict_of(&c);
 let g = if full {
 geometry::analyze_full(&loaded.model)
 } else {
 geometry::analyze(&loaded.model)
 };
 print_json(&g);
 check_note(verdict, summary, false);
 exit_for(verdict)
}

fn cmd_query(
 file: &str,
 query: &str,
 group: Option<&str>,
 at: f64,
 range: &str,
 from: Option<usize>,
 to: Option<usize>,
) -> i32 {
 let loaded = match load(file) {
 Ok(l) => l,
 Err(code) => return code,
 };
 let c = check::check(&loaded.model, false);
 let (verdict, summary) = verdict_of(&c);
 let q = match query::Query::parse(query) {
 Ok(q) => q,
 Err(e) => return usage(&e),
 };
 let n = loaded.model.vertices.len();
 let mut args = query::QueryArgs {
 at,
 range: None,
 from,
 to,
 };
 let model = match q {
 // vertices: `--range` XOR `--group` (never both). `--group` selects
 // the group's OWN vertices `[start, end)` with their GLOBAL indices
 // (unlike the submodel-scoped text queries).
 query::Query::Vertices => {
 if group.is_some() && !range.trim().is_empty() {
                return usage("vertices accepts --range OR --group, not both");
 }
 if let Some(want) = group {
 let g = match resolve_group(&loaded.groups, want, &loaded.source) {
 Ok(g) => g,
 Err(code) => return code,
 };
 // Empty group [s, s) -> (s, s-1), a degenerate range that
 // selects nothing.
 args.range = Some((g.vertex_start, g.vertex_end.saturating_sub(1)));
 } else if !range.trim().is_empty() {
 args.range = Some(match parse_range(range, n) {
 Ok(r) => r,
 Err(code) => return code,
 });
 }
 loaded.model.clone()
 }
 // distance / connectivity: whole model, global --from/--to.
 query::Query::Distance | query::Query::Connectivity => {
 if group.is_some() {
 return usage(&format!(
                    "--group is not valid for '{query}' (it takes global --from/--to indices)"
 ));
 }
 let a = match from {
 Some(i) if i < n => i,
 Some(i) => {
                    return usage(&format!("--from index {i} out of range (vertices={n})"));
 }
                None => return usage(&format!("'{query}' requires --from <index>")),
 };
 let b = match to {
 Some(i) if i < n => i,
 Some(i) => {
                    return usage(&format!("--to index {i} out of range (vertices={n})"));
 }
                None => return usage(&format!("'{query}' requires --to <index>")),
 };
 args.from = Some(a);
 args.to = Some(b);
 loaded.model.clone()
 }
 _ => match group {
 Some(want) => {
 let g = match resolve_group(&loaded.groups, want, &loaded.source) {
 Ok(g) => g,
 Err(code) => return code,
 };
 render::submodel_for_group(&loaded.model, g)
 }
 None => loaded.model.clone(),
 },
 };
    print!("{}", query::run(&model, q, &args));
 let _ = std::io::stdout().flush();
 check_note(verdict, summary, false);
 exit_for(verdict)
}

#[allow(clippy::too_many_arguments)]
fn cmd_view(
 file: &str,
 pitch: f64,
 yaw: f64,
 roll: f64,
 auto_dist: bool,
 dist: Option<f64>,
 pan_x: f64,
 pan_y: f64,
 group: Option<&str>,
 limit: usize,
) -> i32 {
 let loaded = match load(file) {
 Ok(l) => l,
 Err(code) => return code,
 };
 let c = check::check(&loaded.model, false);
 let (verdict, summary) = verdict_of(&c);
 let model = match group {
 Some(want) => {
 let g = match resolve_group(&loaded.groups, want, &loaded.source) {
 Ok(g) => g,
 Err(code) => return code,
 };
 render::submodel_for_group(&loaded.model, g)
 }
 None => loaded.model.clone(),
 };
 let dist = if auto_dist {
 render::auto_dist(&model)
 } else {
 dist.unwrap_or(render::DEFAULT_DIST)
 };
 let v = view::analyze(
 &model,
 &view::ViewParams {
 pitch_deg: pitch,
 yaw_deg: yaw,
 roll_deg: roll,
 dist,
 pan_x,
 pan_y,
 limit,
 },
 );
 print_json(&v);
 check_note(verdict, summary, false);
 exit_for(verdict)
}

#[allow(clippy::too_many_arguments)]
fn cmd_render(
 file: &str,
 views: &str,
 format: &str,
 width: u16,
 height: u16,
 grid_w: usize,
 grid_h: usize,
 detail: Option<&str>,
 region: Option<&str>,
 auto_dist: bool,
 dist: Option<f64>,
 pan_x: f64,
 pan_y: f64,
 pitch: f64,
 yaw: f64,
 roll: f64,
 group: Option<&str>,
 budget: Option<u64>,
) -> i32 {
 let loaded = match load(file) {
 Ok(l) => l,
 Err(code) => return code,
 };
 let c = check::check(&loaded.model, false);
 let (verdict, summary) = verdict_of(&c);
 let opts = match render_opts(
 views, format, width, height, grid_w, grid_h, detail, region, auto_dist, dist, pan_x,
 pan_y, pitch, yaw, roll,
 ) {
 Ok(o) => o,
 Err(code) => return code,
 };
 let (model, group_tag) = match group {
 Some(want) => {
 let g = match resolve_group(&loaded.groups, want, &loaded.source) {
 Ok(g) => g,
 Err(code) => return code,
 };
 (
 render::submodel_for_group(&loaded.model, g),
                format!("  group={want}"),
 )
 }
 None => (loaded.model.clone(), String::new()),
 };
 let (min, max) = render::bounds(&model);

 // Optional token-budget gate: refuse oversized renders so a
 // text-only consumer does not blow its context window.
 if let Some(budget) = budget {
 let nviews = opts.views.len().max(1);
 let per_view = match opts.format {
 render::Format::Grid => opts.grid_w * opts.grid_h * 2,
 render::Format::Braille | render::Format::Ascii => {
 (opts.width as usize) * (opts.height as usize)
 }
 render::Format::Both => 2 * (opts.width as usize) * (opts.height as usize),
 };
 let est = per_view * nviews;
 if est as u64 > budget {
 return usage(&format!(
                "estimated render cost ~{est} tokens exceeds budget={budget}; use a smaller canvas, fewer views, or format=grid"
 ));
 }
 }

 let fmt = match opts.format {
        render::Format::Braille => "braille",
        render::Format::Ascii => "ascii",
        render::Format::Grid => "grid",
        render::Format::Both => "both",
 };
    let detail_name = detail.unwrap_or("-");
 let region_name = match opts.region {
        Some(r) => format!("  region=[{:.2},{:.2},{:.2},{:.2}]", r[0], r[1], r[2], r[3]),
 None => String::new(),
 };
 // P0: echo the effective camera so a single-angle frame is verifiable
 // at a glance (the angles are only meaningful when views is empty).
 let camera_note = if opts.views.is_empty() {
        format!("  camera=pitch={pitch} yaw={yaw} roll={roll}")
 } else {
 String::new()
 };
 println!(
        "# wrfm render  format={fmt}  detail={detail_name}{region_name}  canvas={}x{} chars ({}x{} px)  auto_dist={auto_dist}{camera_note}",
 opts.width,
 opts.height,
 opts.width * 2,
 opts.height * 4
 );
 println!(
        "# name={}  version={}  vertices={}  edges={}{}  bounds=min[{:.3},{:.3},{:.3}] max[{:.3},{:.3},{:.3}]",
 loaded.name,
 loaded.version,
 model.vertices.len(),
 model.edges.len(),
 group_tag,
 min[0],
 min[1],
 min[2],
 max[0],
 max[1],
 max[2],
 );
    print!("{}", render::render_to_text(&model, &opts));
 let _ = std::io::stdout().flush();
 check_note(verdict, summary, false);
 exit_for(verdict)
}

#[allow(clippy::too_many_arguments)]
fn cmd_transform(
 file: &str,
 scale: f64,
 scale_x: f64,
 scale_y: f64,
 scale_z: f64,
 shear_xy: f64,
 shear_xz: f64,
 shear_yz: f64,
 pivot: &str,
 center: bool,
 align: Option<&str>,
 normalize: Option<f64>,
 rotate_x: f64,
 rotate_y: f64,
 rotate_z: f64,
 rotate_axis: Option<&str>,
 rotate_angle: f64,
 translate: Option<&str>,
 mirror: Option<&str>,
) -> i32 {
 let loaded = match load(file) {
 Ok(l) => l,
 Err(code) => return code,
 };
 let axis = match rotate_axis {
 None => None,
 Some(s) => {
            let a = match parse_xyz(s, "rotate_axis") {
 Ok(a) => a,
 Err(code) => return code,
 };
 if a.iter().all(|&x| x.abs() < f64::EPSILON) {
                return usage("rotate_axis must be non-zero (the rotation axis direction)");
 }
 Some(a)
 }
 };
 let pivot = match pivot {
        "origin" => transform::Pivot::Origin,
        "center" => transform::Pivot::Centroid,
        "bbox" => transform::Pivot::Bbox,
        s => match parse_xyz(s, "pivot") {
 Ok(pt) => transform::Pivot::Point(pt),
 Err(code) => return code,
 },
 };
 let align = match align {
 None => None,
        Some("x") => Some(transform::AlignAxis::X),
        Some("y") => Some(transform::AlignAxis::Y),
        Some("z") => Some(transform::AlignAxis::Z),
 Some(other) => {
            return usage(&format!("align must be x|y|z, got '{other}'"));
 }
 };
 let normalize = match normalize {
 None => None,
 Some(v) => {
 if v <= 0.0 {
                return usage(&format!("normalize must be > 0, got {v}"));
 }
 Some(v)
 }
 };
 let t = transform::Transform {
 rotate_x_deg: rotate_x,
 rotate_y_deg: rotate_y,
 rotate_z_deg: rotate_z,
 rotate_axis: axis,
 rotate_angle_deg: rotate_angle,
 scale,
 scale_x,
 scale_y,
 scale_z,
 shear_xy,
 shear_xz,
 shear_yz,
 pivot,
 center,
 align,
 normalize,
 translate: match translate {
 None => [0.0; 3],
            Some(s) => match parse_xyz(s, "translate") {
 Ok(t) => t,
 Err(code) => return code,
 },
 },
 mirror: match mirror {
 None => None,
 Some(s) => match parse_mirror(s) {
 Ok(m) => m,
 Err(code) => return code,
 },
 },
 };
 let out_model = transform::apply(&loaded.model, &t);
 let c = check::check(&out_model, false);
 let (verdict, summary) = verdict_of(&c);
 print!(
        "{}",
 load::serialize_model(&out_model, &loaded.name, &loaded.groups, loaded.version)
 );
 let _ = std::io::stdout().flush();
 check_note(verdict, summary, true);
 exit_for(verdict)
}

fn cmd_edit(
 file: &str,
 dv: Option<&str>,
 de: Option<&str>,
 eg: Option<&str>,
 clean: bool,
 dedupe: bool,
 merge: Option<&str>,
) -> i32 {
 // Exactly one of the SIX edit ops per call.
 let n_ops = dv.is_some() as usize
 + de.is_some() as usize
 + eg.is_some() as usize
 + clean as usize
 + dedupe as usize
 + merge.is_some() as usize;
 if n_ops > 1 {
 return usage(
            "wrfm edit accepts exactly ONE of --delete-vertices / --delete-edges / --extract-group / --clean / --dedupe / --merge per call",
 );
 }
 if n_ops == 0 {
 return usage(
            "wrfm edit requires one of --delete-vertices / --delete-edges / --extract-group / --clean / --dedupe / --merge",
 );
 }
 // --merge is the only op with a second source; stdin can be consumed
 // once, so at most one of the two inputs may be '-'.
    if merge == Some("-") && file == "-" {
        return usage("at most one input may be stdin");
 }

 let loaded = match load(file) {
 Ok(l) => l,
 Err(code) => return code,
 };

 let (out_model, new_groups, name) = if clean {
 let (out, removed) = edit::clean(&loaded.model);
 (
 out,
 edit::remap_groups(&loaded.groups, &removed),
 loaded.name.clone(),
 )
 } else if dedupe {
 let (out, removed) = edit::dedupe(&loaded.model);
 (
 out,
 edit::remap_groups(&loaded.groups, &removed),
 loaded.name.clone(),
 )
 } else if let Some(other) = merge {
 let other_loaded = match load(other) {
 Ok(l) => l,
 Err(code) => return code,
 };
 let out = edit::merge(&loaded.model, &other_loaded.model);
 let groups = edit::merge_groups(
 &loaded.groups,
 &other_loaded.groups,
 loaded.model.vertices.len(),
 );
 (out, groups, loaded.name.clone())
 } else {
 let del_v = match dv {
 None => Vec::new(),
            Some(s) => match parse_index_list(s, "delete_vertices") {
 Ok(v) => v,
 Err(code) => return code,
 },
 };
 let del_e = match de {
 None => Vec::new(),
            Some(s) => match parse_index_list(s, "delete_edges") {
 Ok(v) => v,
 Err(code) => return code,
 },
 };
 // Validate indices BEFORE running — clean exit 2, never a panic.
 if let Some(&bad) = del_v.iter().find(|&&i| i >= loaded.model.vertices.len()) {
 return usage(&format!(
                "delete_vertices index {bad} out of range (vertices={})",
 loaded.model.vertices.len()
 ));
 }
 if let Some(&bad) = del_e.iter().find(|&&i| i >= loaded.model.edges.len()) {
 return usage(&format!(
                "delete_edges index {bad} out of range (edges={})",
 loaded.model.edges.len()
 ));
 }
 if !del_v.is_empty() {
 (
 edit::delete_vertices(&loaded.model, &del_v),
 edit::remap_groups(&loaded.groups, &del_v),
 loaded.name.clone(),
 )
 } else if !del_e.is_empty() {
 (
 edit::delete_edges(&loaded.model, &del_e),
 loaded.groups.clone(),
 loaded.name.clone(),
 )
 } else {
 let g = match resolve_group(&loaded.groups, eg.unwrap(), &loaded.source) {
 Ok(g) => g,
 Err(code) => return code,
 };
 let out = edit::extract_group(&loaded.model, g);
 let groups = vec![wrfm::Group {
 name: g.name.clone(),
 vertex_start: 0,
 vertex_end: out.vertices.len(),
 }];
 (out, groups, loaded.name.clone())
 }
 };
 let c = check::check(&out_model, false);
 let (verdict, summary) = verdict_of(&c);
 print!(
        "{}",
 load::serialize_model(&out_model, &name, &new_groups, loaded.version)
 );
 let _ = std::io::stdout().flush();
 check_note(verdict, summary, true);
 exit_for(verdict)
}

fn cmd_diff(a: &str, b: &str, format: &str) -> i32 {
    if a == "-" && b == "-" {
        return usage("at most one of a/b may be '-' (stdin can be consumed once)");
 }
    if format != "text" && format != "json" {
        return usage(&format!("unknown format '{format}' (expected text|json)"));
 }
 let la = match load(a) {
 Ok(l) => l,
 Err(code) => return code,
 };
 let lb = match load(b) {
 Ok(l) => l,
 Err(code) => return code,
 };
    if format == "json" {
 print_json(&render::diff_to_json(&la.model, &lb.model, 100));
 return 0;
 }
 let opts = render::DiffOptions {
 view: render::View::Iso,
 width: 60,
 height: 24,
 grid_w: 16,
 grid_h: 8,
 };
 print!(
        "{}",
 render::diff_to_text(&la.model, &lb.model, &opts, false)
 );
 let _ = std::io::stdout().flush();
 0
}

/// `wrfm format` — print the complete `.wrfm` v1 spec to stdout. Pure informational command: no model input, no L2 check, exit 0 always.
fn cmd_format() -> i32 {
    print!("{FORMAT_SPEC}");
 let _ = std::io::stdout().flush();
 0
}

