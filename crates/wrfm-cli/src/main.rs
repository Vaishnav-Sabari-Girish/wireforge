mod check;
mod edit;
mod geometry;
mod load;
mod output;
mod proximity;
mod query;
mod render;
mod transform;
mod verify;
mod view;

/// Write to stdout through the broken-pipe-tolerant writer (see `output`):
/// a consumer that closes the pipe early is not an error, the exit code
/// still carries the verdict.
macro_rules! out {
    ($($arg:tt)*) => { $crate::output::stdout(format_args!($($arg)*)) };
}

/// Write to stderr through the broken-pipe-tolerant writer.
macro_rules! err {
    ($($arg:tt)*) => { $crate::output::stderr(format_args!($($arg)*)) };
}

use clap::{Parser, Subcommand};
use serde_json::json;

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
  stderr = diagnostics and errors; the health verdict travels on the exit
           code — run `wrfm check` for the report.
  exit   = 0 ok · 1 warn (a warning-level health issue; `check` only)
           · 2 broken (repair required: check broken, verify fail, or
           --strict upgrading a warn) · 3 no result (unreadable file,
           corrupt model, or usage error).
  To pipe JSON/data do NOT merge stderr: use `wrfm info a.wrfm 2>/dev/null`
  (never `2>&1`) so diagnostics cannot corrupt the data stream.
  A consumer that closes the pipe early (e.g. `wrfm render m.wrfm | head`)
  is NOT an error: the remaining writes are dropped and the verdict still
  travels on the exit code.

POINT TOLERANCE
  Vertices strictly closer than 1e-6 world units are the SAME POINT:
  `wrfm check` reports them as near-duplicate vertices, and
  `wrfm edit m.wrfm --weld 1e-6` merges them (the merged vertex keeps the
  FIRST group section that touched it). The threshold is ABSOLUTE — pass an
  explicit --weld TOL for models whose units are far from 1.

REPAIR MAP (finding -> command)
  duplicate vertices (exact)     -> wrfm edit --dedupe
  near-duplicate vertices        -> wrfm edit --weld 1e-6
  duplicate edges                -> wrfm edit --dedupe
  zero-length edges              -> wrfm edit --clean
  dangling edges                 -> wrfm edit --clean
  isolated vertices              -> wrfm edit --clean
  non-manifold (degree-2)        -> no repair action (an accepted state)

OUTPUT ENCODING
  `--format text|json` on the data commands: json is the machine-readable
  form, text is its projection. `render --format` instead selects the
  render style (braille|ascii|grid|both), never a data encoding.

Authoritative spec: FORMAT.md (wrfm crate docs).
"#;

#[derive(Parser, Debug)]
#[command(
    name = "wrfm",
    version,
    about = "Read-only streaming CLI for .wrfm 3D wireframe models",
    after_help = "Every model-input command accepts '-' for stdin. Nothing writes a file: use shell redirection.\nExit codes: 0 ok (clean result / verify pass) · 1 warn (a warning-level health issue; `check` only) · 2 broken (repair required: check broken, an unmet verify declaration, or --strict upgrading a warn) · 3 no result (unreadable file, corrupt model, or usage error).\nPoint identity: vertices strictly closer than 1e-6 world units are the SAME point — `check` reports them as near-duplicate vertices and `wrfm edit --weld 1e-6` merges them (absolute threshold; pass an explicit --weld TOL when the model's units are far from 1).\n--format text|json selects the OUTPUT ENCODING of the data commands: json is the machine-readable form, text is its projection. `render --format` names the render style instead (braille|ascii|grid|both).\nRepair map (finding -> command): duplicate vertices -> --dedupe · near-duplicate vertices -> --weld 1e-6 · duplicate edges -> --dedupe · zero-length or dangling edges -> --clean · isolated vertices -> --clean · non-manifold (degree-2) vertices have no repair action (an accepted state).\nstderr carries diagnostics and errors only — the health verdict travels on the exit code (run `wrfm check` for the report).\nCompose atomic operations with pipes, e.g.:\n  wrfm edit m.wrfm --extract-group body | wrfm transform - --scale 2 --rotate-y 45\n  wrfm transform m.wrfm --mirror x | wrfm edit - --clean\nFormat spec: wrfm format (self-contained; no source needed)."
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
        /// Upgrade warning-level issues (duplicates / dangling / non-manifold) to a broken verdict — without it they report `warn`.
        #[arg(long)]
        strict: bool,
        /// text | json. JSON is the one machine-readable health form; text is its projection.
        #[arg(long, default_value = "text")]
        format: String,
    },
    /// Verify the model against declared intent (size / center / closed / axis / symmetry / groups).
    Verify {
        /// Path to the .wrfm file, or '-' to read the model from stdin.
        file: String,
        /// Expected bounding-box size 'X,Y,Z' (within --tolerance).
        #[arg(long)]
        expect_size: Option<String>,
        /// Expected bounding-box center 'X,Y,Z' (within --tolerance).
        #[arg(long)]
        expect_center: Option<String>,
        /// Expect every edge to lie in a cycle (no open edges / bridges).
        #[arg(long)]
        expect_closed: bool,
        /// Expect the longest axis to be x | y | z (when axes tie, the tie resolves z>y>x, matching geometry::analyze).
        #[arg(long)]
        expect_axis: Option<String>,
        /// Expect mirror symmetry across the plane perpendicular to x|y|z (comma list).
        #[arg(long)]
        expect_symmetric: Option<String>,
        /// Expect these named groups to exist (comma list).
        #[arg(long)]
        expect_groups: Option<String>,
        /// Relative tolerance for numeric expectations (default 0.05 = 5%).
        #[arg(long, default_value_t = 0.05)]
        tolerance: f64,
        /// Scope the expectations to one group (the part's size/center/closed/axis/symmetry); --expect-groups still checks the whole file.
        #[arg(long)]
        group: Option<String>,
    },
    /// Print model metadata (name/version/vertices/edges/groups/bounds) as JSON.
    Info {
        /// Path to the .wrfm file, or '-' to read the model from stdin.
        file: String,
        /// Scope the report to one group (counts/bounds for the part; the groups list still covers the whole file).
        #[arg(long)]
        group: Option<String>,
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
        /// Scope the analysis to one group (its vertices + the edges touching it).
        #[arg(long)]
        group: Option<String>,
    },
    /// Run a safe geometric query (profile | cross_section | vertices | distance | connectivity). The summary queries (extents / topology / edge_stats) live in `geometry`.
    Query {
        /// Path to the .wrfm file, or '-' to read the model from stdin.
        file: String,
        /// profile | cross_section | vertices | distance | connectivity.
        query: String,
        /// Scope the query to one group (its vertices + the edges touching it).
        #[arg(long)]
        group: Option<String>,
        /// Plane z value for cross_section.
        #[arg(long, default_value_t = 0.0)]
        at: f64,
        /// Inclusive 0-based global index range "a,b" for `vertices` / `distance` / `connectivity` (mutually exclusive with `--group` for `vertices`).
        #[arg(long, default_value = "")]
        range: String,
        /// text | json.
        #[arg(long, default_value = "text")]
        format: String,
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
    },
    /// Render the model to terminal text (braille / ascii / grid).
    Render {
        /// Path to the .wrfm file, or '-' to read the model from stdin.
        file: String,
        /// Comma-separated views (front,back,left,right,top,bottom,iso, side); '' = single frame with the explicit --pitch/--yaw/--roll.
        #[arg(long, default_value = "front,back,left,right,top,bottom")]
        views: String,
        /// braille | ascii | grid | both. ascii is a per-dot #/. map — best at small canvases;
        /// braille is the real terminal output (large canvases).
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
        /// Zoom into a sub-region 'x0,y0,x1,y1' (normalized 0-1).
        #[arg(long)]
        region: Option<String>,
        /// Auto-frame: 'content' crops every frame to the projected content bbox so the model fills the canvas (computed per view; combines with --views/--width/--height).
        /// Priority: an explicit --region WINS and --fit content is ignored; --auto-dist/--dist still choose the camera distance (fit only crops what the camera sees).
        #[arg(long, value_name = "MODE")]
        fit: Option<String>,
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
        to_origin: bool,
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
        /// Merge vertices within TOL (world units) — catches near-duplicates exact --dedupe misses (real data differs by ~1e-15). Same cleanup as --dedupe (duplicate / zero-length edges dropped); the merged vertex keeps the FIRST group section that touched it.
        #[arg(long, value_name = "TOL")]
        weld: Option<f64>,
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
    /// Print the complete .wrfm v1 format spec (magic, header, groups, precision, streams) to stdout. Learn the format from the CLI itself — no separate manual to keep in sync.
    Format,
}

fn main() {
    // Parse manually so clap's own usage errors (unknown flag, missing
    // argument, bad value) map onto THIS exit-code contract: help/version
    // keep exiting 0, every other clap error is a usage error -> 3. clap's
    // default of 2 would collide with `broken`, which must stay unique.
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(e) => {
            let is_help_or_version = matches!(
                e.kind(),
                clap::error::ErrorKind::DisplayHelp | clap::error::ErrorKind::DisplayVersion
            );
            let _ = e.print();
            std::process::exit(if is_help_or_version {
                EXIT_OK
            } else {
                EXIT_NO_RESULT
            });
        }
    };
    let code = match cli.command {
        Command::Check {
            file,
            group,
            strict,
            format,
        } => cmd_check(&file, group.as_deref(), strict, &format),
        Command::Verify {
            file,
            expect_size,
            expect_center,
            expect_closed,
            expect_axis,
            expect_symmetric,
            expect_groups,
            tolerance,
            group,
        } => cmd_verify(
            &file,
            expect_size.as_deref(),
            expect_center.as_deref(),
            expect_closed,
            expect_axis.as_deref(),
            expect_symmetric.as_deref(),
            expect_groups.as_deref(),
            tolerance,
            group.as_deref(),
        ),
        Command::Info { file, group } => cmd_info(&file, group.as_deref()),
        Command::Group { file, name } => cmd_group(&file, name.as_deref()),
        Command::Geometry { file, full, group } => cmd_geometry(&file, full, group.as_deref()),
        Command::Query {
            file,
            query,
            group,
            at,
            range,
            format,
        } => cmd_query(&file, &query, group.as_deref(), at, &range, &format),
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
        ),
        Command::Render {
            file,
            views,
            format,
            width,
            height,
            grid_w,
            grid_h,
            region,
            fit,
            auto_dist,
            dist,
            pan_x,
            pan_y,
            pitch,
            yaw,
            roll,
            group,
        } => cmd_render(
            &file,
            &views,
            &format,
            width,
            height,
            grid_w,
            grid_h,
            region.as_deref(),
            fit.as_deref(),
            auto_dist,
            dist,
            pan_x,
            pan_y,
            pitch,
            yaw,
            roll,
            group.as_deref(),
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
            to_origin,
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
            to_origin,
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
            weld,
            merge,
        } => cmd_edit(
            &file,
            delete_vertices.as_deref(),
            delete_edges.as_deref(),
            extract_group.as_deref(),
            clean,
            dedupe,
            weld,
            merge.as_deref(),
        ),
        Command::Diff { a, b, format } => cmd_diff(&a, &b, &format),
        Command::Format => cmd_format(),
    };
    std::process::exit(code);
}

// Shared helpers

// Exit-code contract — FOUR tiers, shared by every subcommand (larger =
// more severe). 0 = ok · 1 = warn · 2 = broken · 3 = no result (unreadable
// file, corrupt model, or usage error — clap's own argument errors are
// mapped onto 3 too, so 2 always and only means `broken`).
const EXIT_OK: i32 = 0;
const EXIT_WARN: i32 = 1;
const EXIT_BROKEN: i32 = 2;
const EXIT_NO_RESULT: i32 = 3;

/// Load a model through the L1 gate; any failure (I/O, parse, usage) is reported on stderr and mapped to exit 3 (no result).
fn load(source: &str) -> Result<load::Loaded, i32> {
    match load::load(source) {
        Ok(l) => Ok(l),
        Err(e) => {
            err!("{}\n", e.0);
            Err(EXIT_NO_RESULT)
        }
    }
}

/// Exit code from the L2 verdict: the shared four-tier mapping (ok → 0,
/// warn → 1, broken → 2) used by every subcommand that runs a check.
fn exit_for(verdict: &str) -> i32 {
    match verdict {
        "ok" => EXIT_OK,
        "warn" => EXIT_WARN,
        _ => EXIT_BROKEN,
    }
}

/// Verdict of the L2 check for a loaded model (ok / warn / broken). The
/// health verdict travels on the EXIT CODE; `wrfm check` prints the report.
fn verdict_of(c: &serde_json::Value) -> &str {
    c["verdict"].as_str().unwrap_or("ok")
}

/// Resolve a group by name (first match wins, as authored).
fn resolve_group<'a>(
    groups: &'a [wrfm::Group],
    want: &str,
    source: &str,
) -> Result<&'a wrfm::Group, i32> {
    groups.iter().find(|g| g.name == want).ok_or_else(|| {
        err!("error: group '{want}' not found in '{source}'\n");
        EXIT_NO_RESULT
    })
}

/// Build the `RenderOptions` for `wrfm render`, parsing the string options (views / format / region / fit) with usage-error → exit 3 mapping.
#[allow(clippy::too_many_arguments)]
fn render_opts(
    views: &str,
    format: &str,
    width: u16,
    height: u16,
    grid_w: usize,
    grid_h: usize,
    region: Option<&str>,
    fit: Option<&str>,
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
    let reg = match region {
        None => None,
        Some(s) => parse_region(s)?,
    };
    // `--fit content` only arms the auto-framing; it never overrides an
    // explicit --region (render_opts keeps both, render_single_to_text
    // prefers opts.region).
    let fit_content = match fit {
        None => false,
        Some(s) => match s.trim().to_ascii_lowercase().as_str() {
            "content" => true,
            "" => false,
            other => {
                return Err(usage(&format!(
                    "fit must be 'content', got '{other}' (auto-framing crops to the projected model)"
                )));
            }
        },
    };
    Ok(render::RenderOptions {
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
        fit_content,
    })
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

/// Parse a vertex `--range "a,b"` (inclusive, 0-based) and validate it against the model's vertex count (malformed / out of range -> exit 3).
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

/// The `--format text|json` output encoding. `json` is the authoritative
/// machine-readable form of a data command; `text` is its projection.
fn parse_output_format(s: &str) -> Result<bool, i32> {
    match s.trim().to_ascii_lowercase().as_str() {
        "text" => Ok(false),
        "json" => Ok(true),
        other => Err(usage(&format!(
            "unknown format '{other}' (expected text|json)"
        ))),
    }
}

/// A usage/argument error: exit 3 (no result) with the message on stderr.
fn usage(msg: &str) -> i32 {
    err!("error: {msg}\n");
    EXIT_NO_RESULT
}

/// Print pretty JSON to stdout (a contract result).
fn print_json(v: &serde_json::Value) {
    let s = serde_json::to_string_pretty(v).unwrap_or_else(|_| "{}".to_string());
    out!("{s}\n");
}

// Commands

fn cmd_check(file: &str, group: Option<&str>, strict: bool, format: &str) -> i32 {
    let json = match parse_output_format(format) {
        Ok(j) => j,
        Err(code) => return code,
    };
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
    // `strict` is applied HERE so the verdict (and therefore the exit code)
    // already carries the upgrade: warn → broken.
    let c = check::check(&model, strict);
    let verdict = verdict_of(&c);
    // The full report IS the result: stdout, never stderr. JSON (the machine
    // form) is the report plus the identity of what was checked.
    if json {
        let mut out = c.clone();
        out["name"] = json!(loaded.name);
        out["source"] = json!(loaded.source);
        print_json(&out);
    } else {
        out!(
            "{}",
            check::report_text(&loaded.name, model.vertices.len(), model.edges.len(), &c)
        );
    }
    output::flush();
    exit_for(verdict)
}

#[allow(clippy::too_many_arguments)] // verify's option surface (mirrors cmd_render / cmd_transform)
fn cmd_verify(
    file: &str,
    expect_size: Option<&str>,
    expect_center: Option<&str>,
    expect_closed: bool,
    expect_axis: Option<&str>,
    expect_symmetric: Option<&str>,
    expect_groups: Option<&str>,
    tolerance: f64,
    group: Option<&str>,
) -> i32 {
    let loaded = match load(file) {
        Ok(l) => l,
        Err(code) => return code,
    };
    // Parse the numeric "X,Y,Z" expectations with the shared parser.
    let expect_size = match expect_size {
        None => None,
        Some(s) => match parse_xyz(s, "expect_size") {
            Ok(v) => Some(v),
            Err(code) => return code,
        },
    };
    let expect_center = match expect_center {
        None => None,
        Some(s) => match parse_xyz(s, "expect_center") {
            Ok(v) => Some(v),
            Err(code) => return code,
        },
    };
    // expect_axis is a single letter.
    let expect_axis = match expect_axis {
        None => None,
        Some(s) => {
            let t = s.trim();
            if t == "x" || t == "y" || t == "z" {
                Some(t)
            } else {
                return usage(&format!("expect_axis must be x|y|z, got '{s}'"));
            }
        }
    };
    // expect_symmetric is a comma list of x|y|z letters.
    let expect_symmetric: Vec<&str> = match expect_symmetric {
        None => Vec::new(),
        Some(s) => {
            let mut out = Vec::new();
            for part in s.split(',') {
                let t = part.trim();
                if t == "x" || t == "y" || t == "z" {
                    out.push(t);
                } else {
                    return usage(&format!(
                        "expect_symmetric must be a comma list of x|y|z, got '{s}'"
                    ));
                }
            }
            out
        }
    };
    // expect_groups is a comma list of names (empty entries tolerated).
    let expect_groups: Vec<String> = match expect_groups {
        None => Vec::new(),
        Some(s) => s
            .split(',')
            .map(|p| p.trim().to_string())
            .filter(|p| !p.is_empty())
            .collect(),
    };
    // At least one expectation is required.
    if expect_size.is_none()
        && expect_center.is_none()
        && !expect_closed
        && expect_axis.is_none()
        && expect_symmetric.is_empty()
        && expect_groups.is_empty()
    {
        return usage("wrfm verify needs at least one --expect-* flag");
    }
    let opts = verify::VerifyOptions {
        source: &loaded.source,
        expect_size,
        expect_center,
        expect_closed,
        expect_axis,
        expect_symmetric,
        expect_groups,
        tolerance,
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
    let report = verify::verify(&model, &loaded.groups, &opts);
    let verdict = report["verdict"].as_str().unwrap_or("fail");
    let summary = report["summary"].as_str().unwrap_or("");
    print_json(&report);
    if verdict == "fail" {
        err!("[wrfm] verify: {summary}\n");
        // An unmet declaration is "repair required" — the same tier as a broken
        // check, not a warning that can be ignored.
        EXIT_BROKEN
    } else {
        EXIT_OK
    }
}

fn cmd_info(file: &str, group: Option<&str>) -> i32 {
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
    let c = check::check(&model, false);
    let verdict = verdict_of(&c);
    let (min, max) = render::bounds(&model);
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
           "vertices": model.vertices.len(),
           "edges": model.edges.len(),
           "bytes": loaded.bytes,
           "groups": groups,
           "bounds": {
               "min": [min[0], min[1], min[2]],
               "max": [max[0], max[1], max[2]],
               "size": [max[0] - min[0], max[1] - min[1], max[2] - min[2]],
               "center": [center[0], center[1], center[2]],
    },
    }));
    exit_for(verdict)
}

fn cmd_group(file: &str, name: Option<&str>) -> i32 {
    let loaded = match load(file) {
        Ok(l) => l,
        Err(code) => return code,
    };
    let c = check::check(&loaded.model, false);
    let verdict = verdict_of(&c);
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
    exit_for(verdict)
}

fn cmd_geometry(file: &str, full: bool, group: Option<&str>) -> i32 {
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
    let c = check::check(&model, false);
    let verdict = verdict_of(&c);
    let g = if full {
        geometry::analyze_full(&model)
    } else {
        geometry::analyze(&model)
    };
    print_json(&g);
    exit_for(verdict)
}

fn cmd_query(
    file: &str,
    query: &str,
    group: Option<&str>,
    at: f64,
    range: &str,
    format: &str,
) -> i32 {
    let json = match parse_output_format(format) {
        Ok(j) => j,
        Err(code) => return code,
    };
    let loaded = match load(file) {
        Ok(l) => l,
        Err(code) => return code,
    };
    let c = check::check(&loaded.model, false);
    let verdict = verdict_of(&c);
    let q = match query::Query::parse(query) {
        Ok(q) => q,
        Err(e) => return usage(&e),
    };
    let n = loaded.model.vertices.len();
    let mut args = query::QueryArgs { at, range: None };
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
        // distance / connectivity: whole model, global --range "a,b".
        query::Query::Distance | query::Query::Connectivity => {
            if group.is_some() {
                return usage(&format!(
                    "--group is not valid for '{query}' (it takes a global --range \"a,b\")"
                ));
            }
            if range.trim().is_empty() {
                return usage(&format!("'{query}' requires --range \"a,b\""));
            }
            args.range = Some(match parse_range(range, n) {
                Ok(r) => r,
                Err(code) => return code,
            });
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
    if json {
        print_json(&query::value(&model, q, &args));
    } else {
        out!("{}", query::run(&model, q, &args));
    }
    output::flush();
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
) -> i32 {
    let loaded = match load(file) {
        Ok(l) => l,
        Err(code) => return code,
    };
    let c = check::check(&loaded.model, false);
    let verdict = verdict_of(&c);
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
        },
    );
    print_json(&v);
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
    region: Option<&str>,
    fit: Option<&str>,
    auto_dist: bool,
    dist: Option<f64>,
    pan_x: f64,
    pan_y: f64,
    pitch: f64,
    yaw: f64,
    roll: f64,
    group: Option<&str>,
) -> i32 {
    let loaded = match load(file) {
        Ok(l) => l,
        Err(code) => return code,
    };
    let c = check::check(&loaded.model, false);
    let verdict = verdict_of(&c);
    let opts = match render_opts(
        views, format, width, height, grid_w, grid_h, region, fit, auto_dist, dist, pan_x, pan_y,
        pitch, yaw, roll,
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

    let fmt = match opts.format {
        render::Format::Braille => "braille",
        render::Format::Ascii => "ascii",
        render::Format::Grid => "grid",
        render::Format::Both => "both",
    };
    let region_name = match opts.region {
        Some(r) => format!("  region=[{:.2},{:.2},{:.2},{:.2}]", r[0], r[1], r[2], r[3]),
        None => String::new(),
    };
    // Echo an effective `--fit content` (an explicit --region overrides it,
    // so the tag only appears when the auto-framing actually cropped).
    let fit_name = if opts.fit_content && opts.region.is_none() {
        "  fit=content"
    } else {
        ""
    };
    // P0: echo the effective camera so a single-angle frame is verifiable
    // at a glance (the angles are only meaningful when views is empty).
    let camera_note = if opts.views.is_empty() {
        format!("  camera=pitch={pitch} yaw={yaw} roll={roll}")
    } else {
        String::new()
    };
    out!(
        "# wrfm render  format={fmt}{region_name}{fit_name}  canvas={}x{} chars ({}x{} px)  auto_dist={auto_dist}{camera_note}\n",
        opts.width,
        opts.height,
        opts.width * 2,
        opts.height * 4
    );
    out!(
        "# name={}  version={}  vertices={}  edges={}{}  bounds=min[{:.3},{:.3},{:.3}] max[{:.3},{:.3},{:.3}]\n",
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
    out!("{}", render::render_to_text(&model, &opts));
    output::flush();
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
    to_origin: bool,
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
        to_origin,
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
    let verdict = verdict_of(&c);
    out!(
        "{}",
        load::serialize_model(&out_model, &loaded.name, &loaded.groups, loaded.version)
    );
    output::flush();
    exit_for(verdict)
}

#[allow(clippy::too_many_arguments)] // one positional per edit op (mirrors cmd_transform)
fn cmd_edit(
    file: &str,
    dv: Option<&str>,
    de: Option<&str>,
    eg: Option<&str>,
    clean: bool,
    dedupe: bool,
    weld: Option<f64>,
    merge: Option<&str>,
) -> i32 {
    // Exactly one of the SEVEN edit ops per call.
    let n_ops = dv.is_some() as usize
        + de.is_some() as usize
        + eg.is_some() as usize
        + clean as usize
        + dedupe as usize
        + weld.is_some() as usize
        + merge.is_some() as usize;
    if n_ops > 1 {
        return usage(
            "wrfm edit accepts exactly ONE of --delete-vertices / --delete-edges / --extract-group / --clean / --dedupe / --weld / --merge per call",
        );
    }
    if n_ops == 0 {
        return usage(
            "wrfm edit requires one of --delete-vertices / --delete-edges / --extract-group / --clean / --dedupe / --weld / --merge",
        );
    }
    // `--weld TOL` must be a usable tolerance (NaN / inf / 0 would weld the
    // whole model or nothing) — rejected as a usage error, before any I/O.
    if let Some(tol) = weld.filter(|&tol| !tol.is_finite() || tol <= 0.0) {
        return usage(&format!("weld tolerance must be finite and > 0, got {tol}"));
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
    } else if let Some(tol) = weld {
        let (out, removed) = edit::weld(&loaded.model, tol);
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
        // Validate indices BEFORE running — clean exit 3, never a panic.
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
    let verdict = verdict_of(&c);
    out!(
        "{}",
        load::serialize_model(&out_model, &name, &new_groups, loaded.version)
    );
    output::flush();
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
    // Whole-model comparison — to diff a single group, extract it first:
    // `wrfm edit m.wrfm --extract-group g | wrfm diff - other.wrfm`.
    let (ma, mb) = (la.model, lb.model);
    let groups = render::groups_diff(&la.groups, &lb.groups);
    if format == "json" {
        let mut out = render::diff_to_json(&ma, &mb, 100);
        out["groups"] = groups;
        print_json(&out);
        return 0;
    }
    // A pure regrouping is invisible to the vertex/edge diff, so the text form
    // says so explicitly instead of reporting "no change".
    if let Some(line) = render::groups_diff_line(&groups) {
        out!("{line}\n");
    }
    let opts = render::DiffOptions {
        view: render::View::Iso,
        width: 60,
        height: 24,
        grid_w: 16,
        grid_h: 8,
    };
    out!("{}", render::diff_to_text(&ma, &mb, &opts, false));
    output::flush();
    0
}

/// `wrfm format` — print the complete `.wrfm` v1 spec to stdout. Pure informational command: no model input, no L2 check, exit 0 always.
fn cmd_format() -> i32 {
    out!("{FORMAT_SPEC}");
    output::flush();
    0
}
