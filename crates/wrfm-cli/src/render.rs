use ratatui_wireframe::model::Model;
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap, HashSet};
use wrfm_raster::geometry::{auto_dist, bounds, world_rot};
use wrfm_raster::projection::{Camera, focal};
use wrfm_raster::raster::{Bounds, dots_to_lines, rasterize_line};

/// Camera distance used when `auto_dist=false` — the fork's default distance (no auto camera distance).
pub const DEFAULT_DIST: f64 = 8.0;

/// Output format for a rendered frame.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Format {
    /// The real terminal output (braille characters).
    Braille,
    /// Per-dot ASCII map (`#` lit, `.` unlit) — classic ASCII art.
    Ascii,
    /// A coarse density grid of digits (0-9), easiest for text-only models.
    Grid,
    /// Braille and ASCII sections.
    Both,
}

impl Format {
    pub fn parse(s: &str) -> Result<Format, String> {
        match s {
            "braille" => Ok(Format::Braille),
            "ascii" => Ok(Format::Ascii),
            "grid" => Ok(Format::Grid),
            "both" => Ok(Format::Both),
            other => Err(format!(
                "unknown format '{other}' (expected braille|ascii|grid|both)"
            )),
        }
    }
}

/// Standard camera views, expressed in the wireforge fork's world-frame yaw/pitch.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum View {
    /// No rotation — the +Z face (the fork's default orientation).
    Front,
    /// Yaw 180° — the -Z face.
    Back,
    /// Yaw -90° — the +X face (the object's own left side; the object faces +Z).
    /// Positive yaw turns the object's nose to its own left, so revealing its
    /// left side means yawing right by 90°, i.e. yaw -90.
    Left,
    /// Yaw +90° — the -X face (the object's own right side).
    Right,
    /// The classic "side" projection (alias for the left view).
    Side,
    /// Pitch +90° — looking down from +Y (the object's top face).
    Top,
    /// Pitch -90° — looking up from -Y (the object's bottom face).
    Bottom,
    /// 3/4 perspective (pitch 30, yaw 45) — best overall impression.
    Iso,
}

impl View {
    /// All valid view names, for tool documentation.
    pub const ALL: [&'static str; 8] = [
        "front", "back", "left", "right", "top", "bottom", "iso", "side",
    ];

    pub fn parse(s: &str) -> Result<View, String> {
        match s.trim().to_ascii_lowercase().as_str() {
            "front" => Ok(View::Front),
            "back" => Ok(View::Back),
            "left" => Ok(View::Left),
            "side" => Ok(View::Side),
            "right" => Ok(View::Right),
            "top" => Ok(View::Top),
            "bottom" => Ok(View::Bottom),
            "iso" => Ok(View::Iso),
            other => Err(format!(
                "unknown view '{other}' (expected {})",
                View::ALL.join("|")
            )),
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            View::Front => "front",
            View::Back => "back",
            View::Left => "left",
            View::Right => "right",
            View::Side => "side",
            View::Top => "top",
            View::Bottom => "bottom",
            View::Iso => "iso",
        }
    }

    /// `(pitch_deg, yaw_deg, roll_deg)` applied as model rotation.
    pub fn rotation(self) -> (f64, f64, f64) {
        match self {
            View::Front => (0.0, 0.0, 0.0),
            View::Back => (0.0, 180.0, 0.0),
            // Object-side naming (drafting convention): every view name
            // promises the OBJECT side facing the camera — Left shows the
            // object's left (+X when it faces +Z), Top shows its top (+Y).
            // With positive yaw = the object's nose turning to its own left,
            // Left is yaw -90 and Right is yaw +90.
            View::Left => (0.0, -90.0, 0.0),
            View::Right => (0.0, 90.0, 0.0),
            View::Side => (0.0, -90.0, 0.0),
            View::Top => (90.0, 0.0, 0.0),
            View::Bottom => (-90.0, 0.0, 0.0),
            View::Iso => (30.0, -45.0, 0.0),
        }
    }
}

/// Parameters for one render (or one multi-view render).
#[derive(Clone, Debug)]
pub struct RenderOptions {
    /// Canvas width in characters (each character holds a 2x4 braille block).
    pub width: u16,
    /// Canvas height in characters.
    pub height: u16,
    pub pitch_deg: f64,
    pub yaw_deg: f64,
    pub roll_deg: f64,
    pub format: Format,
    /// Auto camera distance: set the camera distance from the model's geometric-mean extent (the fork's fit_to math). When `false`, use `dist` (or the fixed default of 8.0).
    pub auto_dist: bool,
    /// Explicit camera distance (world units); `None` (or 0) = the fixed default (8.0).
    pub dist: Option<f64>,
    /// Aim-point X offset in world units (the fork's pan_x): translates the whole rotated model in the screen X plane, like the TUI's Shift+<-/->.
    pub pan_x: f64,
    /// Aim-point Y offset in world units (the fork's pan_y), like the TUI's Shift+^/v.
    pub pan_y: f64,
    /// When non-empty, render one frame per standard view (overrides pitch_deg/yaw_deg/roll_deg). Empty = one frame from the explicit angles.
    pub views: Vec<View>,
    /// Density grid columns (used by `Format::Grid` and `diff_wrfm`).
    pub grid_w: usize,
    /// Density grid rows (used by `Format::Grid` and `diff_wrfm`).
    pub grid_h: usize,
    /// Zoom into a sub-region of the projected view, as normalized fractions `(x0, y0, x1, y1)` in 0..1 of the canvas (y grows downward). None = no region crop; the whole canvas is rendered.
    pub region: Option<[f64; 4]>,
    /// `--fit content`: crop each frame to the projected content bounding box, so the model fills the canvas. Applied per view AFTER the camera transform, so each view's content fills its canvas.
    pub fit_content: bool,
}

/// Inter-group adjacency for every group: how many BOUNDARY edges (exactly one endpoint in the group) connect to each OTHER group (the other endpoint is credited to the first group in file order owning it).
pub(crate) fn adjacent_groups(m: &Model, groups: &[wrfm::Group]) -> Vec<BTreeMap<String, usize>> {
    let n = m.vertices.len();
    // Global vertex -> first group (file order) that owns it.
    let mut owner: Vec<Option<usize>> = vec![None; n];
    for (gi, g) in groups.iter().enumerate() {
        for slot in owner
            .iter_mut()
            .take(g.vertex_end.min(n))
            .skip(g.vertex_start.min(n))
        {
            if slot.is_none() {
                *slot = Some(gi);
            }
        }
    }
    let mut result: Vec<BTreeMap<String, usize>> = groups.iter().map(|_| BTreeMap::new()).collect();
    for (gi, g) in groups.iter().enumerate() {
        let s = g.vertex_start.min(n);
        let e = g.vertex_end.min(n);
        for &(a, b) in &m.edges {
            let ai = a >= s && a < e;
            let bi = b >= s && b < e;
            if ai == bi {
                continue; // internal (both in) or external (both out)
            }
            let other = if ai { b } else { a };
            let Some(og) = owner.get(other).copied().flatten() else {
                continue; // other endpoint in no group -> ignored
            };
            *result[gi].entry(groups[og].name.clone()).or_insert(0) += 1;
        }
    }
    result
}

/// Describe one named group as structured facts: vertex range/count, bounds, edge split.
pub(crate) fn group_facts(m: &Model, g: &wrfm::Group, adjacent: &BTreeMap<String, usize>) -> Value {
    let s = g.vertex_start.min(m.vertices.len());
    let e = g.vertex_end.min(m.vertices.len());
    // V_g bounds (all-zero for an empty group, s == e).
    let mut min = [f64::INFINITY; 3];
    let mut max = [f64::NEG_INFINITY; 3];
    for &(x, y, z) in &m.vertices[s..e] {
        min[0] = min[0].min(x);
        min[1] = min[1].min(y);
        min[2] = min[2].min(z);
        max[0] = max[0].max(x);
        max[1] = max[1].max(y);
        max[2] = max[2].max(z);
    }
    let (min, max) = if s == e {
        ([0.0; 3], [0.0; 3])
    } else {
        (min, max)
    };
    let center = [
        (min[0] + max[0]) / 2.0,
        (min[1] + max[1]) / 2.0,
        (min[2] + max[2]) / 2.0,
    ];
    // Edge split by endpoints vs V_g, and per-V_g-vertex degree (edge
    // indices are valid — `load` guarantees — so `a`/`b` index `degree`
    // only after the in-group check).
    let mut internal = 0usize;
    let mut boundary = 0usize;
    let mut degree = vec![0usize; e - s];
    for &(a, b) in &m.edges {
        let ai = a >= s && a < e;
        let bi = b >= s && b < e;
        match (ai, bi) {
            (true, true) => {
                internal += 1;
                degree[a - s] += 1;
                degree[b - s] += 1;
            }
            (true, false) => {
                boundary += 1;
                degree[a - s] += 1;
            }
            (false, true) => {
                boundary += 1;
                degree[b - s] += 1;
            }
            (false, false) => {}
        }
    }
    // Whole-model indices of the group's isolated vertices.
    let isolated = (0..(e - s))
        .filter(|&i| degree[i] == 0)
        .map(|i| s + i)
        .collect::<Vec<_>>();
    json!({
           "name": g.name,
           "vertex_start": g.vertex_start,
           "vertex_end": g.vertex_end,
           "vertex_count": e - s,
           "bounds": { "min": min, "max": max, "center": center },
           "internal_edges": internal,
           "boundary_edges": boundary,
           "isolated_vertices": isolated,
           "adjacent_groups": adjacent,
           "verdict": if isolated.is_empty() { "ok" } else { "warn" },
    })
}

/// A group-scoped sub-model: vertices = `V_g` plus the OUT-OF-GROUP endpoints of boundary edges (so every edge in `E_g` is kept, its outside endpoints added as extras).
pub(crate) fn submodel_for_group(m: &Model, g: &wrfm::Group) -> Model {
    let s = g.vertex_start.min(m.vertices.len());
    let e = g.vertex_end.min(m.vertices.len());
    let mut verts: Vec<(f64, f64, f64)> = m.vertices[s..e].to_vec();
    let mut edge_map: Vec<(usize, usize)> = Vec::new();
    let mut extra_index: HashMap<usize, usize> = HashMap::new();
    for &(a, b) in &m.edges {
        let ai = a >= s && a < e;
        let bi = b >= s && b < e;
        if !ai && !bi {
            continue;
        }
        let ia = if ai {
            a - s
        } else {
            *extra_index.entry(a).or_insert_with(|| {
                verts.push(m.vertices[a]);
                verts.len() - 1
            })
        };
        let ib = if bi {
            b - s
        } else {
            *extra_index.entry(b).or_insert_with(|| {
                verts.push(m.vertices[b]);
                verts.len() - 1
            })
        };
        edge_map.push((ia, ib));
    }
    Model {
        vertices: verts,
        edges: edge_map,
    }
}

/// `--fit content`: the normalized region `(x0, y0, x1, y1)` (0..1 canvas
/// fractions, y growing downward) that crops this camera's frame to the
/// projected model content, so the content fills the canvas. Computed with
/// the SAME projection the frame uses (world rotation, dist, focal length,
/// roll, pan), per view. Returns `None` when nothing projects (e.g. every
/// vertex behind the camera) — the caller then keeps the full frame.
///
/// An explicit `--region` is never overridden: `--fit content` only fills
/// in the region when the caller did not pass one.
pub fn fit_content_region(
    m: &Model,
    pitch_deg: f64,
    yaw_deg: f64,
    roll_deg: f64,
    opts: &RenderOptions,
) -> Option<[f64; 4]> {
    let px_w = opts.width.max(1) as f64 * 2.0;
    let px_h = opts.height.max(1) as f64 * 4.0;
    let dist = if opts.auto_dist {
        auto_dist(m)
    } else {
        opts.dist.unwrap_or(DEFAULT_DIST)
    };
    let cam = Camera::new(
        world_rot(pitch_deg, yaw_deg),
        dist,
        roll_deg.to_radians(),
        opts.pan_x,
        opts.pan_y,
    );
    let f = focal(px_h);

    // Projected content bounding box in canvas pixels (origin = centre).
    let (mut minx, mut maxx) = (f64::INFINITY, f64::NEG_INFINITY);
    let (mut miny, mut maxy) = (f64::INFINITY, f64::NEG_INFINITY);
    let mut any = false;
    for &v in &m.vertices {
        if let Some((x, y)) = cam.project(v, f) {
            minx = minx.min(x);
            maxx = maxx.max(x);
            miny = miny.min(y);
            maxy = maxy.max(y);
            any = true;
        }
    }
    if !any {
        return None;
    }

    // Canvas fractions; y flips (canvas y grows downward).
    let (mut x0, mut x1) = ((minx + px_w / 2.0) / px_w, (maxx + px_w / 2.0) / px_w);
    let (mut y0, mut y1) = ((px_h / 2.0 - maxy) / px_h, (px_h / 2.0 - miny) / px_h);
    // 2% breathing room per side (>= 0.001 so a degenerate point/edge-on
    // line still yields a usable window) — keeps the outer braille dots
    // from being clipped by the frame.
    let padx = ((x1 - x0) * 0.02).max(0.001);
    let pady = ((y1 - y0) * 0.02).max(0.001);
    x0 = (x0 - padx).clamp(0.0, 1.0);
    x1 = (x1 + padx).clamp(0.0, 1.0);
    y0 = (y0 - pady).clamp(0.0, 1.0);
    y1 = (y1 + pady).clamp(0.0, 1.0);
    if x1 <= x0 || y1 <= y0 {
        return None;
    }
    Some([x0, y0, x1, y1])
}

/// Geometry / view parameters for a single off-screen render.
struct GeomParams {
    pitch_deg: f64,
    yaw_deg: f64,
    roll_deg: f64,
    w: u16,
    h: u16,
    auto_dist: bool,
    dist: Option<f64>,
    pan_x: f64,
    pan_y: f64,
    region: Option<[f64; 4]>,
}

/// Render `model` into an off-screen buffer and return the braille lines.
fn render_braille(m: &Model, g: &GeomParams) -> Vec<String> {
    let (w, h) = (g.w, g.h);
    // Canvas bounds are in "pixels" (2 x 4 braille dots per character),
    // centered on the origin — the same as the wireforge fork's canvas.
    let px_w = (w as f64) * 2.0;
    let px_h = (h as f64) * 4.0;

    // Region zoom, normalized canvas fractions `(x0, y0, x1, y1)` with y
    // growing downward (0 = top, 1 = bottom).
    let (x0, y0, x1, y1) = match g.region {
        Some(r) => (
            r[0].clamp(0.0, 1.0),
            r[1].clamp(0.0, 1.0),
            r[2].clamp(0.0, 1.0),
            r[3].clamp(0.0, 1.0),
        ),
        None => (0.0, 0.0, 1.0, 1.0),
    };
    let (x0, x1) = if x1 <= x0 {
        (x0, (x0 + 0.01).min(1.0))
    } else {
        (x0, x1)
    };
    let (y0, y1) = if y1 <= y0 {
        (y0, (y0 + 0.01).min(1.0))
    } else {
        (y0, y1)
    };
    let x_bounds = [-px_w / 2.0 + x0 * px_w, -px_w / 2.0 + x1 * px_w];
    let y_bounds = [px_h / 2.0 - y1 * px_h, px_h / 2.0 - y0 * px_h];

    // Shared camera: world-frame rotation, camera distance (auto-fit, or
    // the explicit/default distance), FOV focal length, roll and pan.
    let dist = if g.auto_dist {
        auto_dist(m)
    } else {
        g.dist.unwrap_or(DEFAULT_DIST)
    };
    let cam = Camera::new(
        world_rot(g.pitch_deg, g.yaw_deg),
        dist,
        g.roll_deg.to_radians(),
        g.pan_x,
        g.pan_y,
    );
    let f = focal(px_h);

    // Off-screen dot grid: characters x (2, 4) braille dots. A 0 canvas
    // dimension still allocates one cell (the same `max(1)` area ratatui
    // derives), while the zero-size WINDOW maps nothing — blank frame.
    let (cw, ch) = (w.max(1) as usize, h.max(1) as usize);
    let (dots_w, dots_h) = (cw * 2, ch * 4);
    let win = Bounds::from_arrays(x_bounds, y_bounds);

    let mut dots = vec![0u8; cw * ch];
    for &(a, b) in &m.edges {
        if let (Some(p1), Some(p2)) = (cam.project(m.vertices[a], f), cam.project(m.vertices[b], f))
        {
            rasterize_line(p1.0, p1.1, p2.0, p2.1, dots_w, dots_h, win, |cell, bit| {
                dots[cell] |= bit
            });
        }
    }
    dots_to_lines(&dots, cw, ch)
}

/// Render `model` and return the frame text for the requested format(s).
pub fn render_to_text(m: &Model, opts: &RenderOptions) -> String {
    if !opts.views.is_empty() {
        let mut out = String::new();
        for v in &opts.views {
            let (p, y, r) = v.rotation();
            let sub = RenderOptions {
                views: Vec::new(),
                ..opts.clone()
            };
            out.push_str(&format!("[view={}]\n", v.name()));
            out.push_str(&render_single_to_text(m, p, y, r, &sub));
        }
        return out;
    }
    render_single_to_text(m, opts.pitch_deg, opts.yaw_deg, opts.roll_deg, opts)
}

/// Render a single frame (used by `render_to_text` for one view).
fn render_single_to_text(
    m: &Model,
    pitch_deg: f64,
    yaw_deg: f64,
    roll_deg: f64,
    opts: &RenderOptions,
) -> String {
    let (w, h) = (opts.width, opts.height);
    // `--fit content` fills in the region PER VIEW when no explicit
    // --region was passed (an explicit --region always wins).
    let region = if opts.fit_content && opts.region.is_none() {
        fit_content_region(m, pitch_deg, yaw_deg, roll_deg, opts)
    } else {
        opts.region
    };
    let g = GeomParams {
        pitch_deg,
        yaw_deg,
        roll_deg,
        w,
        h,
        auto_dist: opts.auto_dist,
        dist: opts.dist,
        pan_x: opts.pan_x,
        pan_y: opts.pan_y,
        region,
    };
    let braille = render_braille(m, &g);

    let mut out = String::new();
    match opts.format {
        Format::Braille => {
            for l in &braille {
                out.push_str(l);
                out.push('\n');
            }
        }
        Format::Ascii => {
            for l in ascii_dots(&braille) {
                out.push_str(&l);
                out.push('\n');
            }
        }
        Format::Grid => {
            out.push_str(&format_grid_header(w, h, opts.grid_w, opts.grid_h));
            for row in density_grid(&braille, opts.grid_w, opts.grid_h) {
                out.push_str(&format_row(&row));
                out.push('\n');
            }
        }
        Format::Both => {
            out.push_str("[braille]\n");
            for l in &braille {
                out.push_str(l);
                out.push('\n');
            }
            out.push_str("[ascii]\n");
            for l in ascii_dots(&braille) {
                out.push_str(&l);
                out.push('\n');
            }
        }
    }
    out
}

/// Convert braille lines to a per-dot ASCII map (`#` lit, `.` unlit). Each braille cell (2 wide x 4 tall dots) becomes a 2x4 block of ASCII.
fn ascii_dots(braille: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    for line in braille {
        for dy in 0..4u8 {
            let mut row = String::new();
            for c in line.chars() {
                let mask = braille_mask(c);
                for dx in 0..2u8 {
                    let bit = 1u8 << (dx * 4 + dy);
                    row.push(if mask & bit != 0 { '#' } else { '.' });
                }
            }
            out.push(row);
        }
    }
    out
}

/// Unicode braille codepoint -> 8-bit dot mask (0 for non-braille cells).
fn braille_mask(c: char) -> u8 {
    let cp = c as u32;
    if (0x2800..=0x28FF).contains(&cp) {
        (cp - 0x2800) as u8
    } else {
        0
    }
}

/// One line of a density grid header (grid dims + per-cell dot budget).
fn format_grid_header(w: u16, h: u16, grid_w: usize, grid_h: usize) -> String {
    let (gw, gh) = (grid_w.max(1), grid_h.max(1));
    let (pw, ph) = ((w as usize) * 2, (h as usize) * 4);
    let cw = pw.div_ceil(gw);
    let ch = ph.div_ceil(gh);
    format!(
        "# grid {gw}x{gh} cells (canvas {w}x{h} chars = {pw}x{ph} dots, ~{cw}x{ch} dots/cell; digit 0-9 = density)\n"
    )
}

/// Down-sample rendered braille lines into a `grid_h x grid_w` density grid. Each digit is the fill fraction (0-9) of lit dots inside that cell.
fn density_grid(braille: &[String], grid_w: usize, grid_h: usize) -> Vec<Vec<u8>> {
    let gw = grid_w.max(1);
    let gh = grid_h.max(1);
    let (cw, ch) = (
        braille.first().map(|l| l.chars().count()).unwrap_or(0),
        braille.len(),
    );
    let (pw, ph) = (cw * 2, ch * 4);
    let cell_w = (pw / gw).max(1);
    let cell_h = (ph / gh).max(1);
    let max_per_cell = (cell_w * cell_h).max(1);

    let mut counts = vec![vec![0u64; gw]; gh];
    for (cy, line) in braille.iter().enumerate() {
        for (cx, c) in line.chars().enumerate() {
            let mask = braille_mask(c);
            for dy in 0..4u8 {
                for dx in 0..2u8 {
                    let bit = 1u8 << (dx * 4 + dy);
                    if mask & bit != 0 {
                        let (px, py) = (cx * 2 + dx as usize, cy * 4 + dy as usize);
                        let (gx, gy) = ((px / cell_w).min(gw - 1), (py / cell_h).min(gh - 1));
                        counts[gy][gx] += 1;
                    }
                }
            }
        }
    }
    counts
        .iter()
        .map(|row| {
            row.iter()
                .map(|&c| {
                    let frac = c as f64 / max_per_cell as f64;
                    ((frac * 9.0).round() as u8).min(9)
                })
                .collect()
        })
        .collect()
}

/// Format one density row as space-separated digits.
fn format_row(row: &[u8]) -> String {
    row.iter()
        .map(|d| d.to_string())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Options for a render diff (`diff_wrfm`).
#[derive(Clone, Copy, Debug)]
pub struct DiffOptions {
    pub view: View,
    pub width: u16,
    pub height: u16,
    pub grid_w: usize,
    pub grid_h: usize,
}

/// Compare two models by rendering both to density grids at the same view and reporting per-cell deltas. Mirrors Blender MCP's "verify against" workflow.
pub fn diff_to_text(a: &Model, b: &Model, opts: &DiffOptions, detail: bool) -> String {
    let (pitch, yaw, roll) = opts.view.rotation();
    let (w, h) = (opts.width, opts.height);
    let g = GeomParams {
        pitch_deg: pitch,
        yaw_deg: yaw,
        roll_deg: roll,
        w,
        h,
        auto_dist: true,
        dist: None,
        pan_x: 0.0,
        pan_y: 0.0,
        region: None,
    };
    let braille_a = render_braille(a, &g);
    let braille_b = render_braille(b, &g);
    let ga = density_grid(&braille_a, opts.grid_w, opts.grid_h);
    let gb = density_grid(&braille_b, opts.grid_w, opts.grid_h);

    let (bmin_a, bmax_a) = bounds(a);
    let (bmin_b, bmax_b) = bounds(b);
    let mut out = String::new();
    out.push_str(&format!(
        "# wrfm diff  view={}  grid={}x{}  canvas={}x{} chars\n",
        opts.view.name(),
        opts.grid_w,
        opts.grid_h,
        w,
        h
    ));
    out.push_str(&format!(
        "# vertices: {} -> {} ({:+})    edges: {} -> {} ({:+})\n",
        a.vertices.len(),
        b.vertices.len(),
        b.vertices.len() as isize - a.vertices.len() as isize,
        a.edges.len(),
        b.edges.len(),
        b.edges.len() as isize - a.edges.len() as isize
    ));
    out.push_str(&format!(
        "# bbox:  min[{:.2},{:.2},{:.2}] max[{:.2},{:.2},{:.2}]  ->  min[{:.2},{:.2},{:.2}] max[{:.2},{:.2},{:.2}]\n",
 bmin_a[0], bmin_a[1], bmin_a[2], bmax_a[0], bmax_a[1], bmax_a[2],
 bmin_b[0], bmin_b[1], bmin_b[2], bmax_b[0], bmax_b[1], bmax_b[2],
 ));

    if detail {
        out.push_str("[before]\n");
        for row in &ga {
            out.push_str(&format_row(row));
            out.push('\n');
        }
        out.push_str("[after]\n");
        for row in &gb {
            out.push_str(&format_row(row));
            out.push('\n');
        }
    }

    out.push_str("[delta grid]  (0 = unchanged, nonzero = change magnitude)\n");
    for (ra, rb) in ga.iter().zip(gb.iter()) {
        let row: Vec<u8> = ra
            .iter()
            .zip(rb.iter())
            .map(|(&x, &y)| x.abs_diff(y))
            .collect();
        out.push_str(&format_row(&row));
        out.push('\n');
    }

    // Count changed cells for a compact summary.
    let changed: usize = ga
        .iter()
        .zip(gb.iter())
        .map(|(ra, rb)| ra.iter().zip(rb.iter()).filter(|(x, y)| x != y).count())
        .sum();
    let total = opts.grid_w * opts.grid_h;
    out.push_str(&format!("# summary: {changed}/{total} cells changed\n"));
    out
}

/// Structured diff between two models: vertex add/remove/move (matched by quantized coordinates, displacements reported) and edge add/remove, plus
/// Group comparison for `wrfm diff`: the group names on each side, the
/// add/remove sets, and — the case that used to be invisible — vertices that
/// changed group membership while the geometry stayed identical.
///
/// Membership is the group's vertex range `[vertex_start, vertex_end)` in
/// the ONE global vertex list, so a regrouping shows up as `only_in_a` /
/// `only_in_b` index lists.
pub fn groups_diff(a: &[wrfm::Group], b: &[wrfm::Group]) -> Value {
    let names = |gs: &[wrfm::Group]| -> Vec<String> { gs.iter().map(|g| g.name.clone()).collect() };
    let (na, nb) = (names(a), names(b));
    let added: Vec<&String> = nb.iter().filter(|n| !na.contains(n)).collect();
    let removed: Vec<&String> = na.iter().filter(|n| !nb.contains(n)).collect();
    let mut changed: Vec<Value> = Vec::new();
    for ga in a {
        let Some(gb) = b.iter().find(|g| g.name == ga.name) else {
            continue;
        };
        let in_a = |i: usize| (ga.vertex_start..ga.vertex_end).contains(&i);
        let in_b = |i: usize| (gb.vertex_start..gb.vertex_end).contains(&i);
        let lo = ga.vertex_start.min(gb.vertex_start);
        let hi = ga.vertex_end.max(gb.vertex_end);
        let only_in_a: Vec<usize> = (lo..hi).filter(|&i| in_a(i) && !in_b(i)).collect();
        let only_in_b: Vec<usize> = (lo..hi).filter(|&i| in_b(i) && !in_a(i)).collect();
        if !only_in_a.is_empty() || !only_in_b.is_empty() {
            changed.push(json!({
                "name": ga.name,
                "only_in_a": only_in_a,
                "only_in_b": only_in_b,
            }));
        }
    }
    json!({
        "a": na,
        "b": nb,
        "added": added,
        "removed": removed,
        "membership_changed": changed,
    })
}

/// The text projection of [`groups_diff`]: `None` when the group structure is
/// identical (so the text diff stays byte-for-byte what it was).
pub fn groups_diff_line(g: &Value) -> Option<String> {
    let count = |k: &str| g[k].as_array().map(|a| a.len()).unwrap_or(0);
    let (na, nb) = (count("a"), count("b"));
    let mut parts: Vec<String> = Vec::new();
    // `Value` Display would print JSON strings WITH their quotes.
    for name in g["added"].as_array().into_iter().flatten() {
        parts.push(format!("+{}", name.as_str().unwrap_or("-")));
    }
    for name in g["removed"].as_array().into_iter().flatten() {
        parts.push(format!("-{}", name.as_str().unwrap_or("-")));
    }
    for c in g["membership_changed"].as_array().into_iter().flatten() {
        let out = c["only_in_a"].as_array().map(|a| a.len()).unwrap_or(0);
        let inn = c["only_in_b"].as_array().map(|a| a.len()).unwrap_or(0);
        parts.push(format!(
            "{} (-{out} +{inn} vertices)",
            c["name"].as_str().unwrap_or("-")
        ));
    }
    if parts.is_empty() {
        None
    } else {
        Some(format!(
            "# groups: {na} -> {nb}   changed: {}",
            parts.join(", ")
        ))
    }
}

pub fn diff_to_json(a: &Model, b: &Model, limit: usize) -> Value {
    let r3 = |x: f64| (x * 1000.0).round() / 1000.0;
    let bucket = |v: &(f64, f64, f64)| -> (i64, i64, i64) {
        (
            (v.0 / 1e-4).round() as i64,
            (v.1 / 1e-4).round() as i64,
            (v.2 / 1e-4).round() as i64,
        )
    };
    // Pass 1: match vertices with identical coordinates (unchanged).
    let mut buckets: HashMap<(i64, i64, i64), Vec<usize>> = HashMap::new();
    for (i, v) in b.vertices.iter().enumerate() {
        buckets.entry(bucket(v)).or_default().push(i);
    }
    let eps = 1e-6;
    let mut matched_a = vec![false; a.vertices.len()];
    let mut matched_b = vec![false; b.vertices.len()];
    for (i, &va) in a.vertices.iter().enumerate() {
        let mut found: Option<usize> = None;
        if let Some(cands) = buckets.get(&bucket(&va)) {
            for &j in cands {
                if matched_b[j] {
                    continue;
                }
                let vb = b.vertices[j];
                if (vb.0 - va.0).abs() < eps
                    && (vb.1 - va.1).abs() < eps
                    && (vb.2 - va.2).abs() < eps
                {
                    found = Some(j);
                    break;
                }
            }
        }
        if let Some(j) = found {
            matched_a[i] = true;
            matched_b[j] = true;
        }
    }
    let a_rem: Vec<(usize, (f64, f64, f64))> = a
        .vertices
        .iter()
        .enumerate()
        .filter(|&(i, _)| !matched_a[i])
        .map(|(i, &v)| (i, v))
        .collect();
    let b_rem: Vec<(usize, (f64, f64, f64))> = b
        .vertices
        .iter()
        .enumerate()
        .filter(|&(i, _)| !matched_b[i])
        .map(|(i, &v)| (i, v))
        .collect();

    // Pass 2: when both sides have the same number of unmatched vertices,
    // pair them nearest-neighbor and report them as MOVED (this is how a
    // translate/scale edit looks). Otherwise report adds/removes.
    let mut moved: Vec<Value> = Vec::new();
    let mut moved_total = 0usize;
    let (removed, added) = if a_rem.len() == b_rem.len() {
        let mut b_used = vec![false; b_rem.len()];
        for (ia, va) in &a_rem {
            let mut best: Option<(usize, f64)> = None;
            for (j, (_, vb)) in b_rem.iter().enumerate() {
                if b_used[j] {
                    continue;
                }
                let d =
                    ((va.0 - vb.0).powi(2) + (va.1 - vb.1).powi(2) + (va.2 - vb.2).powi(2)).sqrt();
                if best.is_none_or(|(_, bd)| d < bd) {
                    best = Some((j, d));
                }
            }
            if let Some((j, dist)) = best {
                b_used[j] = true;
                let vb = b_rem[j].1;
                moved_total += 1;
                if moved.len() < limit {
                    moved.push(json!({
                        "index_a": ia,
                        "index_b": b_rem[j].0,
                        "delta": [r3(vb.0 - va.0), r3(vb.1 - va.1), r3(vb.2 - va.2)],
                        "distance": r3(dist),
 }));
                }
            }
        }
        (0usize, 0usize)
    } else {
        (a_rem.len(), b_rem.len())
    };

    let edge_set = |m: &Model| -> HashSet<(usize, usize)> {
        m.edges.iter().map(|&(x, y)| (x.min(y), x.max(y))).collect()
    };
    let (ea, eb) = (edge_set(a), edge_set(b));
    let added_edges: Vec<Value> = eb.difference(&ea).map(|&e| json!([e.0, e.1])).collect();
    let removed_edges: Vec<Value> = ea.difference(&eb).map(|&e| json!([e.0, e.1])).collect();

    let (amin, amax) = bounds(a);
    let (bmin, bmax) = bounds(b);
    json!({
           "a": { "vertices": a.vertices.len(), "edges": a.edges.len() },
           "b": { "vertices": b.vertices.len(), "edges": b.edges.len() },
           "vertices": {
               "added": added,
               "removed": removed,
               "moved_count": moved_total,
               "moved": moved,
               "moved_truncated": moved_total > moved.len(),
    },
           "edges": { "added": added_edges, "removed": removed_edges },
           "bounds": {
               "a_min": amin,
               "a_max": amax,
               "b_min": bmin,
               "b_max": bmax,
    },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use wrfm_raster::geometry::{FIT_MARGIN, FOV_DEG, model_extent};

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

    fn tetra() -> Model {
        Model {
            vertices: vec![
                (1.0, 1.0, 1.0),
                (1.0, -1.0, -1.0),
                (-1.0, 1.0, -1.0),
                (-1.0, -1.0, 1.0),
            ],
            edges: vec![(0, 1), (0, 2), (0, 3), (1, 2), (2, 3), (3, 1)],
        }
    }

    ///The group-aware fixture: two groups (body 0..3, head 3..5) with internal edges and one cross (boundary) edge.
    fn grouped_model() -> (Model, Vec<wrfm::Group>) {
        let m = Model {
            vertices: vec![
                (0.0, 0.0, 0.0),
                (1.0, 0.0, 0.0),
                (0.0, 1.0, 0.0),
                (0.5, 2.0, 0.25),
                (1.0, 1.0, 1.0),
            ],
            edges: vec![(0, 1), (1, 2), (3, 4), (1, 4)],
        };
        let groups = vec![
            wrfm::Group {
                name: "body".into(),
                vertex_start: 0,
                vertex_end: 3,
            },
            wrfm::Group {
                name: "head".into(),
                vertex_start: 3,
                vertex_end: 5,
            },
        ];
        (m, groups)
    }

    #[test]
    fn group_facts_single_group_reports_sets() {
        let (m, groups) = grouped_model();
        let f = group_facts(&m, &groups[0], &BTreeMap::new()); // body 0..3
        assert_eq!(f["name"], "body");
        assert_eq!(f["vertex_start"], 0);
        assert_eq!(f["vertex_end"], 3);
        assert_eq!(f["vertex_count"], 3);
        assert_eq!(f["internal_edges"], 2); // (0,1),(1,2)
        assert_eq!(f["boundary_edges"], 1); // (1,4)
        // Bounds = the body's own V_g box.
        assert_eq!(f["bounds"]["min"], json!([0.0, 0.0, 0.0]));
        assert_eq!(f["bounds"]["max"], json!([1.0, 1.0, 0.0]));
        assert_eq!(f["verdict"], "ok");
    }

    #[test]
    fn group_facts_reports_isolated_in_group() {
        let m = Model {
            vertices: vec![(0.0, 0.0, 0.0), (1.0, 0.0, 0.0), (5.0, 5.0, 5.0)],
            edges: vec![(0, 1)],
        };
        let g = wrfm::Group {
            name: "a".into(),
            vertex_start: 0,
            vertex_end: 3,
        };
        let f = group_facts(&m, &g, &BTreeMap::new());
        // Whole-model index of the degree-0 vertex, not group-local.
        assert_eq!(f["isolated_vertices"], json!([2]));
        assert_eq!(f["verdict"], "warn");
    }

    #[test]
    fn group_facts_empty_group() {
        let m = Model {
            vertices: vec![(0.0, 0.0, 0.0), (1.0, 0.0, 0.0)],
            edges: vec![(0, 1)],
        };
        let g = wrfm::Group {
            name: "empty".into(),
            vertex_start: 1,
            vertex_end: 1,
        };
        let f = group_facts(&m, &g, &BTreeMap::new());
        assert_eq!(f["vertex_count"], 0);
        assert_eq!(f["bounds"]["min"], json!([0.0, 0.0, 0.0]));
        assert_eq!(f["internal_edges"], 0);
        assert_eq!(f["boundary_edges"], 0);
        assert_eq!(f["isolated_vertices"], json!([]));
        assert_eq!(f["verdict"], "ok");
    }

    #[test]
    fn group_facts_duplicate_names_first_wins() {
        let (m, _) = grouped_model();
        let groups = [
            wrfm::Group {
                name: "a".into(),
                vertex_start: 0,
                vertex_end: 2,
            },
            wrfm::Group {
                name: "a".into(),
                vertex_start: 2,
                vertex_end: 4,
            },
        ];
        // The MCP layer resolves by name -> FIRST match; group_facts then
        // describes that first group.
        let f = group_facts(&m, &groups[0], &BTreeMap::new());
        assert_eq!(f["name"], "a");
        assert_eq!(f["vertex_start"], 0);
        assert_eq!(f["vertex_count"], 2);
    }

    #[test]
    fn submodel_includes_boundary_endpoints() {
        let m = Model {
            vertices: vec![
                (0.0, 0.0, 0.0),
                (1.0, 0.0, 0.0),
                (0.0, 1.0, 0.0),
                (9.0, 9.0, 9.0),
                (1.0, 1.0, 1.0),
            ],
            edges: vec![(1, 4)],
        };
        let g = wrfm::Group {
            name: "body".into(),
            vertex_start: 0,
            vertex_end: 2,
        };
        let scoped = submodel_for_group(&m, &g);
        // Two body vertices + the out-of-group endpoint 4 of the boundary edge.
        assert_eq!(scoped.vertices.len(), 3);
        assert_eq!(scoped.edges, vec![(1, 2)]); // (1,4) remapped
        assert_eq!(scoped.vertices[2], (1.0, 1.0, 1.0));
    }

    #[test]
    fn submodel_group_is_a_subset() {
        let m = Model {
            vertices: vec![
                (0.0, 0.0, 0.0),
                (1.0, 0.0, 0.0),
                (0.0, 1.0, 0.0),
                (1.0, 1.0, 1.0),
            ],
            edges: vec![(2, 3)],
        };
        let g = wrfm::Group {
            name: "a".into(),
            vertex_start: 0,
            vertex_end: 2,
        };
        let scoped = submodel_for_group(&m, &g);
        assert_eq!(scoped.vertices.len(), 2);
        assert!(scoped.edges.is_empty()); // outside-outside edge dropped
    }

    fn opts(format: Format, views: Vec<View>) -> RenderOptions {
        RenderOptions {
            width: 40,
            height: 16,
            pitch_deg: 0.0,
            yaw_deg: 0.0,
            roll_deg: 0.0,
            format,
            auto_dist: true,
            dist: None,
            pan_x: 0.0,
            pan_y: 0.0,
            views,
            grid_w: 16,
            grid_h: 8,
            region: None,
            fit_content: false,
        }
    }

    #[test]
    fn bounds_of_tetrahedron() {
        let (min, max) = bounds(&tetra());
        assert_eq!(min, [-1.0, -1.0, -1.0]);
        assert_eq!(max, [1.0, 1.0, 1.0]);
    }

    #[test]
    fn auto_dist_matches_fork() {
        // The fork's fit_to: dist = extent / tan(FOV/2) * FIT_MARGIN, where
        // extent is the geometric-mean bbox dimension. A +-1 tetrahedron has
        // extent 2.
        let m = tetra();
        let expected = 2.0 / (FOV_DEG / 2.0).to_radians().tan() * FIT_MARGIN;
        assert!(
            (auto_dist(&m) - expected).abs() < 1e-9,
            "auto_dist should equal fork's fit_to, got {}",
            auto_dist(&m)
        );
        // Elongated model: the geomean is not dominated by the long axis.
        let long = Model {
            vertices: vec![(0.0, 0.0, 0.0), (6.0, 4.0, 160.0)],
            edges: vec![(0, 1)],
        };
        assert!(
            model_extent(&long) < 20.0,
            "geomean must not be dominated by the long axis"
        );
    }

    #[test]
    fn projection_matches_fork_math() {
        // Identity view of the cube front vertex at the fork's default
        // distance: f = (px_h/2)/tan(30deg), z = dist - rz = 8 - 1 = 7.
        let cam = Camera::new(world_rot(0.0, 0.0), DEFAULT_DIST, 0.0, 0.0, 0.0);
        let f = focal(64.0);
        let p = cam.project((1.0, 1.0, 1.0), f).unwrap();
        let expected = f / 7.0;
        assert!(
            (p.0 - expected).abs() < 1e-9 && (p.1 - expected).abs() < 1e-9,
            "projection must match the fork, got ({},{}) vs ({expected},{expected})",
            p.0,
            p.1
        );
        // Points at/behind the camera plane are culled (z <= 0.1).
        assert!(cam.project((0.0, 0.0, 8.2), f).is_none());
        // World-frame turntable: at pitch 90 the model's own Y axis starts
        // centred and yaw swings it to screen-right (fork's regression
        // test), instead of Euler photo-spinning around the view axis.
        // Positive yaw = the object's nose turns to its own left, which at
        // pitch 90 (its Y pointing at the camera) swings that axis right.
        let cam = Camera::new(world_rot(90.0, 45.0), DEFAULT_DIST, 0.0, 0.0, 0.0);
        let (ox, _) = cam.project((0.0, 1.0, 0.0), f).unwrap();
        assert!(ox > 0.0, "model Y should swing to screen-right, got {ox}");
    }

    #[test]
    fn braille_mask_mapping() {
        assert_eq!(braille_mask(' '), 0);
        assert_eq!(braille_mask('\u{2800}'), 0);
        assert_eq!(braille_mask('\u{2801}'), 0x01); // dot 1 (top-left)
        assert_eq!(braille_mask('\u{28FF}'), 0xFF); // all 8 dots
    }

    #[test]
    fn ascii_dots_matches_braille_dots() {
        let lines = ascii_dots(&["\u{2811}".to_string()]);
        assert_eq!(lines.len(), 4);
        assert_eq!(lines[0], "##");
        assert!(lines[1..].iter().all(|l| l == ".."));
    }

    #[test]
    fn render_produces_nonempty_ascii() {
        let text = render_to_text(&tetra(), &opts(Format::Ascii, vec![]));
        assert!(text.contains('#'), "ascii frame should contain lit dots");
        let rows: Vec<&str> = text.lines().collect();
        assert_eq!(rows.len(), 16 * 4, "ascii is 4 rows per braille row");
        assert!(rows.iter().all(|r| r.len() == 40 * 2));
    }

    #[test]
    fn render_both_has_sections() {
        let mut o = opts(Format::Both, vec![]);
        o.width = 20;
        o.height = 8;
        let text = render_to_text(&tetra(), &o);
        assert!(text.starts_with("[braille]\n"));
        assert!(text.contains("\n[ascii]\n"));
    }

    #[test]
    fn view_parse_and_rotation() {
        assert_eq!(View::parse("front").unwrap(), View::Front);
        assert_eq!(View::parse("ISO").unwrap(), View::Iso);
        assert!(
            View::parse("bogus")
                .unwrap_err()
                .contains("unknown view 'bogus'")
        );
        assert_eq!(View::parse("side").unwrap(), View::Side);
        assert_eq!(View::Iso.rotation(), (30.0, -45.0, 0.0));
        // Object-side naming: Left = yaw -90 (the +X face, the object's left;
        // positive yaw is the object's nose turning to its own left, so
        // revealing its left side = yawing right = yaw -90), Right = yaw +90,
        // Side aliases Left, Top = pitch +90 (+Y face), Bottom = pitch -90
        // (-Y face).
        assert_eq!(View::Left.rotation(), (0.0, -90.0, 0.0));
        assert_eq!(View::Right.rotation(), (0.0, 90.0, 0.0));
        assert_eq!(View::Side.rotation(), (0.0, -90.0, 0.0));
        assert_eq!(View::Top.rotation(), (90.0, 0.0, 0.0));
        assert_eq!(View::Bottom.rotation(), (-90.0, 0.0, 0.0));
    }

    /// Regression guard against the 2026-10 view-name inversion: each view
    /// must put the OBJECT side it names nearest the camera (fixed camera at
    /// +Z looking down -Z, so larger rotated-z = nearer).
    #[test]
    fn view_names_show_the_object_side_they_promise() {
        for (view, named, opposite) in [
            (View::Front, [0.0, 0.0, 1.0], [0.0, 0.0, -1.0]),
            (View::Back, [0.0, 0.0, -1.0], [0.0, 0.0, 1.0]),
            (View::Left, [1.0, 0.0, 0.0], [-1.0, 0.0, 0.0]),
            (View::Right, [-1.0, 0.0, 0.0], [1.0, 0.0, 0.0]),
            (View::Side, [1.0, 0.0, 0.0], [-1.0, 0.0, 0.0]),
            (View::Top, [0.0, 1.0, 0.0], [0.0, -1.0, 0.0]),
            (View::Bottom, [0.0, -1.0, 0.0], [0.0, 1.0, 0.0]),
        ] {
            let (pitch, yaw, _) = view.rotation();
            let rot = world_rot(pitch, yaw);
            let depth = |p: [f64; 3]| rot[2][0] * p[0] + rot[2][1] * p[1] + rot[2][2] * p[2];
            assert!(
                depth(named) > depth(opposite),
                "{:?}: the side it names must face the camera (named z={} vs opposite z={})",
                view,
                depth(named),
                depth(opposite)
            );
        }
    }

    #[test]
    fn multi_view_renders_labeled_sections() {
        let text = render_to_text(
            &tetra(),
            &opts(Format::Ascii, vec![View::Front, View::Top, View::Iso]),
        );
        assert!(text.contains("[view=front]\n"));
        assert!(text.contains("[view=top]\n"));
        assert!(text.contains("[view=iso]\n"));
        assert!(!text.contains("[view=side]\n"));
    }

    #[test]
    fn grid_format_produces_digit_rows() {
        let text = render_to_text(&tetra(), &opts(Format::Grid, vec![]));
        assert!(text.starts_with("# grid 16x8 cells"), "grid header");
        let rows: Vec<&str> = text.lines().skip(1).collect();
        assert_eq!(rows.len(), 8, "grid_h rows");
        for row in &rows {
            let digits: Vec<&str> = row.split_whitespace().collect();
            assert_eq!(digits.len(), 16, "grid_w columns");
            assert!(
                digits
                    .iter()
                    .all(|d| d.len() == 1 && d.chars().all(|c| c.is_ascii_digit()))
            );
        }
        assert!(
            rows.iter().any(|r| r.split_whitespace().any(|d| d != "0")),
            "model should light cells"
        );
    }

    #[test]
    fn different_views_change_the_grid() {
        let front = render_to_text(&tetra(), &opts(Format::Grid, vec![View::Front]));
        let top = render_to_text(&tetra(), &opts(Format::Grid, vec![View::Top]));
        assert_ne!(front, top, "front and top views should differ");
    }

    #[test]
    fn diff_reports_changes() {
        let a = tetra();
        // b = a + one extra far vertex/edge
        let mut b = a.clone();
        b.vertices.push((2.0, 2.0, 2.0));
        b.edges.push((0, 4));
        let opts = DiffOptions {
            view: View::Iso,
            width: 40,
            height: 16,
            grid_w: 16,
            grid_h: 8,
        };
        let text = diff_to_text(&a, &b, &opts, false);
        assert!(text.contains("# vertices: 4 -> 5 (+1)"));
        assert!(text.contains("edges: 6 -> 7 (+1)"));
        assert!(text.contains("[delta grid]"));
        assert!(text.contains("# summary:"));
        // There must be at least one changed cell.
        let summary = text.lines().find(|l| l.starts_with("# summary:")).unwrap();
        assert!(!summary.contains("0/128 cells changed"));
    }

    #[test]
    fn region_zoom_changes_the_frame() {
        let full = render_to_text(&tetra(), &opts(Format::Grid, vec![]));
        let mut z = opts(Format::Grid, vec![]);
        z.region = Some([0.25, 0.25, 0.75, 0.75]);
        let zoomed = render_to_text(&tetra(), &z);
        assert_ne!(full, zoomed, "region zoom must change the render");
        // Zoomed view of a lit region must still contain lit cells.
        assert!(
            zoomed
                .lines()
                .skip(1)
                .any(|l| l.split_whitespace().any(|d| d != "0"))
        );
    }

    #[test]
    fn region_parse_guards_degenerate() {
        let mut z = opts(Format::Grid, vec![]);
        z.region = Some([0.5, 0.5, 0.5, 0.5]); // zero-area -> fallback
        let text = render_to_text(&tetra(), &z);
        assert!(!text.is_empty());
    }

    #[test]
    fn diff_json_identical_models() {
        let m = cube();
        let d = diff_to_json(&m, &m, 100);
        assert_eq!(d["vertices"]["added"], 0);
        assert_eq!(d["vertices"]["removed"], 0);
        assert_eq!(d["vertices"]["moved_count"], 0);
        assert_eq!(d["edges"]["added"].as_array().unwrap().len(), 0);
        assert_eq!(d["edges"]["removed"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn diff_json_reports_moves_and_edge_changes() {
        let a = cube();
        // Shift every vertex by +0.5 on x -> all 8 move (b = edited).
        let mut b = cube();
        b.vertices = b
            .vertices
            .iter()
            .map(|&(x, y, z)| (x + 0.5, y, z))
            .collect();
        b.edges.push((0, 7)); // one added edge
        let d = diff_to_json(&a, &b, 100);
        assert_eq!(d["vertices"]["moved_count"], 8);
        assert_eq!(d["vertices"]["added"], 0);
        assert_eq!(d["vertices"]["removed"], 0);
        assert_eq!(d["edges"]["added"].as_array().unwrap().len(), 1);
        assert_eq!(d["edges"]["removed"].as_array().unwrap().len(), 0);
        let mv = d["vertices"]["moved"].as_array().unwrap();
        assert_eq!(mv.len(), 8);
        assert!((mv[0]["distance"].as_f64().unwrap() - 0.5).abs() < 1e-6);
    }

    #[test]
    fn diff_json_limit_truncates_and_marks() {
        let a = cube();
        let mut b = cube();
        b.vertices = b
            .vertices
            .iter()
            .map(|&(x, y, z)| (x + 0.5, y, z))
            .collect();
        let d = diff_to_json(&a, &b, 3);
        assert_eq!(d["vertices"]["moved_count"], 8);
        assert_eq!(d["vertices"]["moved"].as_array().unwrap().len(), 3);
        assert_eq!(d["vertices"]["moved_truncated"], true);
    }

    #[test]
    fn diff_identical_models_no_change() {
        let a = tetra();
        let opts = DiffOptions {
            view: View::Iso,
            width: 40,
            height: 16,
            grid_w: 16,
            grid_h: 8,
        };
        let text = diff_to_text(&a, &a, &opts, false);
        let summary = text.lines().find(|l| l.starts_with("# summary:")).unwrap();
        assert!(summary.contains("0/128 cells changed"));
    }
    #[test]
    fn adjacent_groups_counts_cross_edges() {
        // body 0..3, head 3..5, edge (1,4) -> body {"head":1}, head
        // {"body":1}; the internal edges add nothing.
        let (m, groups) = grouped_model();
        let adj = adjacent_groups(&m, &groups);
        assert_eq!(adj[0].len(), 1);
        assert_eq!(adj[0].get("head"), Some(&1));
        assert_eq!(adj[1].get("body"), Some(&1));
    }

    #[test]
    fn adjacent_groups_ignores_ungrouped() {
        // Boundary edge (1,3) where vertex 3 is in NO group -> not counted.
        let m = Model {
            vertices: vec![
                (0.0, 0.0, 0.0),
                (1.0, 0.0, 0.0),
                (0.0, 1.0, 0.0),
                (9.0, 9.0, 9.0),
            ],
            edges: vec![(0, 1), (1, 2), (1, 3)],
        };
        let groups = vec![wrfm::Group {
            name: "body".into(),
            vertex_start: 0,
            vertex_end: 3,
        }];
        let adj = adjacent_groups(&m, &groups);
        assert!(adj[0].is_empty(), "ungrouped endpoint must be ignored");
    }

    #[test]
    fn adjacent_groups_duplicate_names_first_wins() {
        // Two groups share the name "a"; the boundary edge from group 1 to a
        // vertex owned by the second "a" counts against the NAME once.
        let m = Model {
            vertices: vec![
                (0.0, 0.0, 0.0),
                (1.0, 0.0, 0.0),
                (5.0, 5.0, 5.0),
                (6.0, 6.0, 6.0),
            ],
            edges: vec![(0, 2)],
        };
        let groups = vec![
            wrfm::Group {
                name: "a".into(),
                vertex_start: 0,
                vertex_end: 1,
            },
            wrfm::Group {
                name: "a".into(),
                vertex_start: 2,
                vertex_end: 4,
            },
        ];
        let adj = adjacent_groups(&m, &groups);
        assert_eq!(adj[0].get("a"), Some(&1));
    }

    #[test]
    fn group_facts_includes_adjacent_groups() {
        let (m, groups) = grouped_model();
        let adj = adjacent_groups(&m, &groups);
        let f = group_facts(&m, &groups[0], &adj[0]);
        assert_eq!(f["adjacent_groups"]["head"], 1);
    }

    #[test]
    fn single_frame_angles_change_the_view() {
        // P0: views=[] renders one frame at pitch_deg/yaw_deg/roll_deg, so a
        // non-default camera must change the output.
        let m = cube();
        let mut o0 = opts(Format::Braille, vec![]);
        o0.pitch_deg = 0.0;
        o0.yaw_deg = 0.0;
        o0.roll_deg = 0.0;
        let mut o1 = opts(Format::Braille, vec![]);
        o1.pitch_deg = 18.0;
        o1.yaw_deg = 35.0;
        o1.roll_deg = 0.0;
        let t0 = render_to_text(&m, &o0);
        let t1 = render_to_text(&m, &o1);
        assert_ne!(
            t0, t1,
            "explicit angles must change the single-frame render"
        );
    }

    #[test]
    fn named_views_override_explicit_angles() {
        // P0 contract: when views is non-empty the angles are ignored.
        let m = cube();
        let mut angled = opts(Format::Braille, vec![View::Front]);
        angled.pitch_deg = 18.0;
        angled.yaw_deg = 35.0;
        let plain = opts(Format::Braille, vec![View::Front]);
        assert_eq!(
            render_to_text(&m, &angled),
            render_to_text(&m, &plain),
            "named views must ignore explicit angles"
        );
    }

    // -- fit content ------------------------------------------------------------

    /// Fraction of the ascii frame covered by the lit-dot bounding box:
    /// `(columns, rows)`.
    fn ascii_coverage(text: &str) -> (f64, f64) {
        let rows: Vec<Vec<char>> = text.lines().map(|l| l.chars().collect()).collect();
        let (h, w) = (rows.len(), rows.first().map(|r| r.len()).unwrap_or(0));
        let (mut c0, mut c1, mut r0, mut r1) = (usize::MAX, 0usize, usize::MAX, 0usize);
        for (y, row) in rows.iter().enumerate() {
            for (x, &c) in row.iter().enumerate() {
                if c == '#' {
                    c0 = c0.min(x);
                    c1 = c1.max(x);
                    r0 = r0.min(y);
                    r1 = r1.max(y);
                }
            }
        }
        if c0 == usize::MAX || w == 0 || h == 0 {
            return (0.0, 0.0);
        }
        (
            (c1 - c0 + 1) as f64 / w as f64,
            (r1 - r0 + 1) as f64 / h as f64,
        )
    }

    #[test]
    fn fit_content_region_is_some_and_inside_the_canvas() {
        let m = cube();
        let o = opts(Format::Braille, vec![]);
        // front / top / iso: every view gets its own clamped, non-degenerate window.
        for (pitch, yaw) in [(0.0, 0.0), (-90.0, 0.0), (30.0, 45.0)] {
            let r = fit_content_region(&m, pitch, yaw, 0.0, &o).expect("cube projects");
            assert!(
                r[0] >= 0.0 && r[1] >= 0.0 && r[2] <= 1.0 && r[3] <= 1.0,
                "region must be clamped to the canvas: {r:?}"
            );
            assert!(
                r[2] > r[0] && r[3] > r[1],
                "region must be non-degenerate: {r:?}"
            );
            // auto_dist frames the model well inside the canvas, so the content
            // window is strictly smaller than the full frame.
            assert!(
                (r[2] - r[0]) < 0.99 && (r[3] - r[1]) < 0.99,
                "fit must crop, not keep the whole frame: {r:?}"
            );
        }
    }

    #[test]
    fn fit_content_fills_the_canvas() {
        let m = cube();
        let plain = opts(Format::Ascii, vec![]);
        let mut fit = opts(Format::Ascii, vec![]);
        fit.fit_content = true;
        let nofit = ascii_coverage(&render_to_text(&m, &plain));
        let fitted = ascii_coverage(&render_to_text(&m, &fit));
        assert!(
            fitted.0 >= 0.8 && fitted.1 >= 0.8,
            "fit content should fill the canvas, got {fitted:?}"
        );
        assert!(
            fitted.0 > nofit.0 + 0.2 && fitted.1 > nofit.1 + 0.2,
            "fit {fitted:?} must cover substantially more than {nofit:?}"
        );
    }

    #[test]
    fn fit_content_yields_to_an_explicit_region() {
        // Priority contract: --region beats --fit content.
        let m = cube();
        let mut o = opts(Format::Ascii, vec![]);
        o.region = Some([0.25, 0.25, 0.75, 0.75]);
        o.fit_content = true;
        let with_fit = render_to_text(&m, &o);
        o.fit_content = false;
        let without_fit = render_to_text(&m, &o);
        assert_eq!(
            with_fit, without_fit,
            "explicit --region must win over --fit"
        );
    }

    #[test]
    fn fit_content_none_when_nothing_projects() {
        // Camera pushed behind the model (z = dist - rz <= 0.1 for every
        // vertex): everything is culled, so there is no content to frame and
        // the caller keeps the full frame (None).
        let mut o = opts(Format::Braille, vec![]);
        o.auto_dist = false;
        o.dist = Some(-1.0);
        assert!(fit_content_region(&cube(), 0.0, 0.0, 0.0, &o).is_none());
    }
}
