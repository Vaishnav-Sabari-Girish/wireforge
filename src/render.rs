use ratatui::style::Color;
use std::io::Write;
use wrfm_raster::Model;

use crate::view::{self, ViewState};
use wrfm_raster::raster::{Bounds, rasterize_line};

/// An empty cell: a space in the terminal's own colors, on its own background.
const SPACE: u64 = (' ' as u64) << 16;

/// Pack a char + foreground + background ink index into one u64 cell.
///
/// Foreground sits in the low byte so `cell as u8` still reads back the
/// foreground, which is how the rasterizer-only tests have always inspected a
/// cell. Foreground `Default` on background `Default` is `SPACE`.
#[inline(always)]
fn pack(ch: char, fg: u8, bg: u8) -> u64 {
    (ch as u64) << 16 | (bg as u64) << 8 | fg as u64
}

/// Model edges are rasterized in parallel above this many edges (Stage C).
const PARALLEL_EDGE_THRESHOLD: usize = 50_000;
/// The f32 batch-projection path is used above this many vertices.
const F32_PROJECT_THRESHOLD: usize = 4_096;

/// One slot of the viewer's 16-color palette, usable as a foreground *or* a
/// background: the two uses differ only by the ANSI `30↔40` / `90↔100` offset,
/// so one enum covers both.
///
/// The slots are named after the ANSI color a terminal palette entry holds.
/// Wireforge draws in the *theme's* colors, so the theme decides the actual
/// RGB: on the everforest palette these names were chosen against, `Green` is
/// `#A7C080` and `Grey` is `#5d686f`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum Ink {
    /// The terminal's own foreground / background: no SGR color is set.
    Default = 0,
    Cyan = 1,
    Red = 2,
    Yellow = 3,
    LightBlue = 4,
    /// ANSI 0 — the statusline's dark block.
    Black = 5,
    /// ANSI 2 — the accent, worn by the statusline's leading pill.
    Green = 6,
    /// ANSI 8 (bright black) — the statusline's lighter block.
    Grey = 7,
    /// ANSI 7 — body text.
    White = 8,
}

impl Ink {
    /// Recover the [`Ink`] from a packed-cell ink byte.
    pub fn from_u8(v: u8) -> Ink {
        match v {
            1 => Ink::Cyan,
            2 => Ink::Red,
            3 => Ink::Yellow,
            4 => Ink::LightBlue,
            5 => Ink::Black,
            6 => Ink::Green,
            7 => Ink::Grey,
            8 => Ink::White,
            _ => Ink::Default,
        }
    }

    /// The SGR color parameter for this slot, or `None` when it is
    /// [`Ink::Default`] and the terminal's own color should stand.
    fn code(self) -> Option<u8> {
        match self {
            Ink::Default => None,
            Ink::Black => Some(30),
            Ink::Red => Some(31),
            Ink::Green => Some(32),
            Ink::Yellow => Some(33),
            Ink::Cyan => Some(36),
            Ink::White => Some(37),
            Ink::Grey => Some(90),
            Ink::LightBlue => Some(94),
        }
    }
}

/// Map a ratatui `Color` to the palette slot holding it. Colors outside the
/// palette (including `Color::Reset`) fall back to the terminal default.
pub fn ink_idx(c: Color) -> u8 {
    match c {
        Color::Cyan => Ink::Cyan as u8,
        Color::Red => Ink::Red as u8,
        Color::Yellow => Ink::Yellow as u8,
        Color::LightBlue => Ink::LightBlue as u8,
        Color::Black => Ink::Black as u8,
        Color::Green => Ink::Green as u8,
        Color::DarkGray => Ink::Grey as u8,
        Color::White => Ink::White as u8,
        _ => Ink::Default as u8,
    }
}

/// Append the SGR sequence selecting `fg` on `bg`.
///
/// Always opens with a reset so a cell can never inherit an ink from its
/// neighbour; when both slots are default that is exactly `ESC[0m`, the
/// sequence this writer emitted before backgrounds existed.
fn push_sgr(out: &mut Vec<u8>, fg: Ink, bg: Ink) {
    out.extend_from_slice(b"\x1b[0");
    // A background slot is its foreground slot's SGR parameter + 10: 30..37
    // become 40..47, 90..97 become 100..107.
    for code in [fg.code(), bg.code().map(|c| c + 10)].into_iter().flatten() {
        out.push(b';');
        Screen::push_usize(out, code as usize);
    }
    out.push(b'm');
}

/// A retained-mode full-screen cell grid (packed u64 cells).
pub struct Screen {
    w: usize,
    h: usize,
    cur: Vec<u64>,
    prev: Vec<u64>,
    /// reusable ANSI output buffer
    out: Vec<u8>,
    /// True after a resize: the next present must clear the whole screen first.
    full_repaint: bool,
}

impl Screen {
    /// Create a screen of `w` x `h` cells; `prev` starts all-space.
    pub fn new(w: usize, h: usize) -> Self {
        // ratatui's braille table maps a row-major 2x4 pattern value to the
        // Unicode braille char (a permutation of U+2800+pattern — NOT the
        // identity, e.g. BRAILLE[0x55] == '\u{2847}'). Using the exact table
        // keeps output byte-identical to the ratatui canvas path it replaces.
        let n = w * h;
        Screen {
            w,
            h,
            cur: vec![SPACE; n],
            prev: vec![SPACE; n],
            out: Vec::with_capacity(4096),
            full_repaint: false,
        }
    }

    /// Resize the screen; reallocates the grids and flags a full repaint.
    pub fn resize(&mut self, w: usize, h: usize) {
        if w == self.w && h == self.h {
            return;
        }
        self.w = w;
        self.h = h;
        self.cur = vec![SPACE; w * h];
        self.prev = vec![SPACE; w * h];
        // The terminal's alt-screen still holds the OLD size's pixels.
        // Mark the next present as a full repaint so it clears them.
        self.full_repaint = true;
    }

    /// Dimensions in cells.
    pub fn size(&self) -> (usize, usize) {
        (self.w, self.h)
    }

    /// Set one cell of the current frame: a char drawn in `fg` on `bg`
    /// (out-of-range writes are ignored).
    #[inline]
    pub fn set(&mut self, x: usize, y: usize, ch: char, fg: u8, bg: u8) {
        if x < self.w && y < self.h {
            self.cur[y * self.w + x] = pack(ch, fg, bg);
        }
    }

    /// The packed value of a current-frame cell (used by the golden tests).
    #[cfg(test)]
    pub fn cell(&self, x: usize, y: usize) -> u64 {
        self.cur[y * self.w + x]
    }

    /// The char drawn in a current-frame cell (used by the frame tests, which
    /// read composition through the screen rather than through the widgets).
    #[cfg(test)]
    pub fn symbol(&self, x: usize, y: usize) -> char {
        char::from_u32((self.cur[y * self.w + x] >> 16) as u32).unwrap_or(' ')
    }

    /// Append a decimal integer to the output buffer (no allocation).
    fn push_usize(out: &mut Vec<u8>, mut n: usize) {
        if n == 0 {
            out.push(b'0');
            return;
        }
        let mut tmp = [0u8; 20];
        let mut i = tmp.len();
        while n > 0 {
            i -= 1;
            tmp[i] = b'0' + (n % 10) as u8;
            n /= 10;
        }
        out.extend_from_slice(&tmp[i..]);
    }

    /// Append a cursor-position escape (`ESC[row;colH`, 1-based) to `out`.
    fn push_cursor(out: &mut Vec<u8>, row: usize, col: usize) {
        out.push(0x1b);
        out.push(b'[');
        Self::push_usize(out, row + 1);
        out.push(b';');
        Self::push_usize(out, col + 1);
        out.push(b'H');
    }

    /// Diff the current frame against the last, write the changed cells as one batch.
    ///
    /// Every frame leaves the terminal's SGR at *defaults* (see the two resets
    /// below): the last run a frame writes is the statusline, whose ground is a
    /// filled background, and a selected background is contagious — any erase
    /// that follows, ours or the terminal's own, paints with it.
    pub fn present<W: Write>(&mut self, out: &mut W) -> std::io::Result<usize> {
        self.out.clear();
        if self.full_repaint {
            // Reset BEFORE erasing. An erase fills with the background the
            // terminal currently has selected (background-colour-erase, the
            // default in xterm and friends), and the frame before this one
            // ended on the statusline's ground — so clearing in that state
            // would flood the whole screen, canvas included, with the
            // strip's colour instead of blanking it.
            self.out.extend_from_slice(b"\x1b[0m");
            // Wipe the previous size's pixels from the terminal once; the
            // diff below then redraws every non-space cell of the new frame
            // (prev is all-space after the resize).
            self.out.extend_from_slice(b"\x1b[2J");
            self.full_repaint = false;
        }
        let mut changed = 0;
        for y in 0..self.h {
            let base = y * self.w;
            let cur_row = &self.cur[base..base + self.w];
            let prev_row = &self.prev[base..base + self.w];
            if cur_row == prev_row {
                continue;
            }
            let mut x = 0;
            while x < self.w {
                if cur_row[x] == prev_row[x] {
                    x += 1;
                    continue;
                }
                let start = x;
                while x < self.w && cur_row[x] != prev_row[x] {
                    x += 1;
                }
                let end = x;
                Self::push_cursor(&mut self.out, y, start);
                let mut last = (u8::MAX, u8::MAX);
                for &cell in &cur_row[start..end] {
                    let fg = (cell & 0xff) as u8;
                    let bg = ((cell >> 8) & 0xff) as u8;
                    if (fg, bg) != last {
                        push_sgr(&mut self.out, Ink::from_u8(fg), Ink::from_u8(bg));
                        last = (fg, bg);
                    }
                    let ch = char::from_u32((cell >> 16) as u32).unwrap_or(' ');
                    let mut b = [0u8; 4];
                    self.out
                        .extend_from_slice(ch.encode_utf8(&mut b).as_bytes());
                }
                changed += end - start;
            }
        }
        if changed > 0 {
            // Reset AFTER writing, for the erases the terminal does by itself:
            // the fill of the rows a resize exposes, a clear triggered by the
            // window manager, an erase on scroll. Without this the strip's
            // ground stays selected between frames and spills over the canvas
            // the moment the terminal erases anything — which is exactly what
            // a window resize looked like.
            self.out.extend_from_slice(b"\x1b[0m");
        }
        std::mem::swap(&mut self.cur, &mut self.prev);
        // `cur` now holds the old `prev`; blank it for the next frame.
        self.cur.fill(SPACE);
        out.write_all(&self.out)?;
        out.flush()?;
        Ok(changed)
    }
}

/// The retained rasterizer: canvas dot grid + cached vertex projection.
pub struct Rasterizer {
    /// canvas cell width
    cw: usize,
    /// canvas cell height
    ch: usize,
    /// per-cell braille pattern (cw*ch)
    dots: Vec<u8>,
    /// per-cell color index (cw*ch)
    colors: Vec<u8>,
    /// braille pattern -> char lookup (U+2800 + pattern)
    braille: [char; 256],
    /// cached projected screen points per vertex
    proj: Vec<[f64; 2]>,
    /// cached f32 projections (large-model path)
    proj32: Vec<[f32; 2]>,
    /// per-vertex "in front of the camera" flags
    proj_ok: Vec<bool>,
    /// projection cache key: the view that produced the cache
    proj_view: ViewState,
    /// projection cache key: dot-grid width
    proj_px_w: usize,
    /// projection cache key: dot-grid height
    proj_px_h: usize,
    /// projection cache key: vertex count
    proj_len: usize,
}

impl Default for Rasterizer {
    fn default() -> Self {
        Self::new()
    }
}

impl Rasterizer {
    /// A rasterizer with no canvas (call [`Rasterizer::resize`] first).
    pub fn new() -> Self {
        Rasterizer {
            cw: 0,
            ch: 0,
            braille: ratatui::symbols::braille::BRAILLE,
            dots: Vec::new(),
            colors: Vec::new(),
            proj: Vec::new(),
            proj32: Vec::new(),
            proj_ok: Vec::new(),
            proj_view: ViewState::default(),
            proj_px_w: 0,
            proj_px_h: 0,
            proj_len: 0,
        }
    }

    /// Resize the canvas (in cells). Invalidates the projection cache.
    pub fn resize(&mut self, cw: usize, ch: usize) {
        self.cw = cw;
        self.ch = ch;
        self.dots = vec![0; cw * ch];
        self.colors = vec![0; cw * ch];
        self.proj_px_w = 0;
        self.proj_px_h = 0;
    }

    /// The canvas grid size in cells, `(width, height)` (used by the canvas
    /// sizing tests, which read the grid the resize path installed).
    #[cfg(test)]
    pub fn canvas_size(&self) -> (usize, usize) {
        (self.cw, self.ch)
    }

    /// Batch-project all vertices when the cache is stale.
    fn project(&mut self, model: &Model, view: &ViewState, px_w: usize, px_h: usize) {
        let n = model.vertices.len();
        if self.proj_len == n
            && self.proj_view == *view
            && self.proj_px_w == px_w
            && self.proj_px_h == px_h
        {
            return;
        }
        self.proj.clear();
        self.proj.resize(n, [0.0; 2]);
        self.proj32.clear();
        self.proj32.resize(n, [0.0; 2]);
        self.proj_ok.clear();
        self.proj_ok.resize(n, false);
        if n >= F32_PROJECT_THRESHOLD {
            view::project_batch_f32(
                &model.vertices,
                view,
                px_h,
                &mut self.proj32,
                &mut self.proj_ok,
            );
            // Widen f32 -> f64 (exact) so the downstream clip/rounding runs
            // in the same f64 math as the exact path.
            for i in 0..n {
                self.proj[i] = [self.proj32[i][0] as f64, self.proj32[i][1] as f64];
            }
        } else {
            view::project_batch(
                &model.vertices,
                view,
                px_h,
                &mut self.proj,
                &mut self.proj_ok,
            );
        }
        self.proj_view = *view;
        self.proj_px_w = px_w;
        self.proj_px_h = px_h;
        self.proj_len = n;
    }

    /// Render the model (edges + optional axes + labels) into the screen.
    ///
    /// `extent` is the model's geometric-mean length ([`view::model_extent`]),
    /// which sizes the axes. It is passed in rather than recomputed here
    /// because the caller knows the model is immutable: re-deriving it would
    /// put an O(n) bounds scan on every frame.
    pub fn render(
        &mut self,
        model: &Model,
        view: &ViewState,
        region: (usize, usize, usize, usize),
        show_axes: bool,
        extent: f64,
        screen: &mut Screen,
    ) {
        let (rx, ry, cw, ch) = region;
        debug_assert!(cw > 0 && ch > 0);
        let px_w = cw * 2;
        let px_h = ch * 4;
        self.project(model, view, px_w, px_h);

        self.dots.fill(0);
        self.colors.fill(0);

        let cyan = Ink::Cyan as u8;
        if model.edges.len() > PARALLEL_EDGE_THRESHOLD {
            // Stage C: parallel rasterization over edge chunks. Each
            // worker paints into its own scratch grid; all model dots are
            // cyan, so the merge is order-independent.
            let (dots, colors) = rasterize_edges_par(
                &model.edges,
                &self.proj,
                &self.proj_ok,
                self.cw,
                px_w,
                px_h,
                cyan,
            );
            self.dots = dots;
            self.colors = colors;
        } else {
            for &(a, b) in &model.edges {
                if self.proj_ok[a] && self.proj_ok[b] {
                    let [x1, y1] = self.proj[a];
                    let [x2, y2] = self.proj[b];
                    paint_line_into(
                        x1,
                        y1,
                        x2,
                        y2,
                        cyan,
                        self.cw,
                        px_w,
                        px_h,
                        &mut self.dots,
                        &mut self.colors,
                    );
                }
            }
        }

        // Axes: 0.618 of the model's geometric-mean extent, transformed with
        // the model, Red/Yellow/LightBlue for X/Y/Z, toggled with Tab.
        //
        // An EMPTY model (the blank start-up view) still draws them: there
        // the extent is the unit scene, so the three axes are the space
        // itself — the origin cross that shows which way X, Y and Z point
        // and that every camera key still works. It is also why the model
        // extent is never zero or NaN (see `extent_from_bounds`): the axes
        // would otherwise collapse onto the origin as a single dot.
        let mut labels: Vec<(f64, f64, &str, u8)> = Vec::new();
        if show_axes {
            let axis_len = extent / 0.618;
            let origin = view::project_point((0.0, 0.0, 0.0), view, px_h);
            let ends = [
                (
                    view::project_point((axis_len, 0.0, 0.0), view, px_h),
                    "X",
                    Ink::Red as u8,
                ),
                (
                    view::project_point((0.0, axis_len, 0.0), view, px_h),
                    "Y",
                    Ink::Yellow as u8,
                ),
                (
                    view::project_point((0.0, 0.0, axis_len), view, px_h),
                    "Z",
                    Ink::LightBlue as u8,
                ),
            ];
            if let Some((ox, oy)) = origin {
                for (end, label, color) in ends {
                    if let Some((ex, ey)) = end {
                        paint_line_into(
                            ox,
                            oy,
                            ex,
                            ey,
                            color,
                            self.cw,
                            px_w,
                            px_h,
                            &mut self.dots,
                            &mut self.colors,
                        );
                        labels.push((ex, ey, label, color));
                    }
                }
            }
        }

        // Encode the dot grid into screen cells (2x4 dots per braille char).
        let braille = self.braille;
        for cy in 0..ch {
            for cx in 0..cw {
                let i = cy * cw + cx;
                let p = self.dots[i];
                if p != 0 {
                    screen.set(
                        rx + cx,
                        ry + cy,
                        braille[p as usize],
                        self.colors[i],
                        Ink::Default as u8,
                    );
                }
            }
        }

        // Labels are drawn ON TOP of the rasterized dots, exactly like
        // ratatui's canvas draws labels after the layers.
        for (ex, ey, label, color) in labels {
            self.place_label(ex, ey, label, color, px_w, px_h, rx, ry, screen);
        }
    }

    /// Place an axis label at the character cell nearest the projected endpoint.
    #[allow(clippy::too_many_arguments)] // primitive geometry helper
    fn place_label(
        &self,
        ex: f64,
        ey: f64,
        label: &str,
        color: u8,
        px_w: usize,
        px_h: usize,
        rx: usize,
        ry: usize,
        screen: &mut Screen,
    ) {
        if self.cw < 2 || self.ch < 2 {
            return;
        }
        let left = -(px_w as f64) / 2.0;
        let right = (px_w as f64) / 2.0;
        let top = (px_h as f64) / 2.0;
        let bottom = -(px_h as f64) / 2.0;
        let cell_w = (px_w as f64) / (self.cw - 1) as f64;
        let cell_h = (px_h as f64) / (self.ch - 1) as f64;
        // The app's pre-snap (main.rs draw closure): snap the label to the
        // character cell nearest the moving endpoint.
        let label_x = left + ((ex - left) / cell_w).round() * cell_w;
        let label_y = top - ((top - ey) / cell_h).round() * cell_h;
        // ratatui's canvas filter + scale-and-truncate (`as u16`).
        if label_x >= left && label_x <= right && label_y <= top && label_y >= bottom {
            let x = ((label_x - left) * (self.cw - 1) as f64 / px_w as f64) as u16;
            let y = ((top - label_y) * (self.ch - 1) as f64 / px_h as f64) as u16;
            if let Some(ch) = label.chars().next() {
                screen.set(
                    rx + x as usize,
                    ry + y as usize,
                    ch,
                    color,
                    Ink::Default as u8,
                );
            }
        }
    }
}

/// Clip and rasterize one line into the shared braille grid (wrfm-raster's
/// port of ratatui's canvas algorithm), stamping each lit cell's color.
#[allow(clippy::too_many_arguments)] // primitive geometry helper
fn paint_line_into(
    x1: f64,
    y1: f64,
    x2: f64,
    y2: f64,
    color: u8,
    cw: usize,
    px_w: usize,
    px_h: usize,
    dots: &mut [u8],
    colors: &mut [u8],
) {
    debug_assert_eq!(cw * 2, px_w, "dot width must be 2 per character cell");
    rasterize_line(
        x1,
        y1,
        x2,
        y2,
        px_w,
        px_h,
        Bounds::centered(px_w, px_h),
        |cell, bit| {
            dots[cell] |= bit;
            colors[cell] = color;
        },
    );
}

/// Rasterize a large edge set in parallel over chunks (> 50k edges).
#[allow(clippy::too_many_arguments)] // primitive geometry helper
fn rasterize_edges_par(
    edges: &[(usize, usize)],
    proj: &[[f64; 2]],
    ok: &[bool],
    cw: usize,
    px_w: usize,
    px_h: usize,
    color: u8,
) -> (Vec<u8>, Vec<u8>) {
    use rayon::prelude::*;
    let cells = cw * (px_h / 4);
    let n_edges = edges.len();
    let chunk_size = n_edges.div_ceil(rayon::current_num_threads() * 4).max(1);
    edges
        .par_chunks(chunk_size)
        .fold(
            || (vec![0u8; cells], vec![0u8; cells]),
            |(mut pat, mut col), chunk| {
                for &(a, b) in chunk {
                    if ok[a] && ok[b] {
                        let [x1, y1] = proj[a];
                        let [x2, y2] = proj[b];
                        paint_line_into(x1, y1, x2, y2, color, cw, px_w, px_h, &mut pat, &mut col);
                    }
                }
                (pat, col)
            },
        )
        .reduce(
            || (vec![0u8; cells], vec![0u8; cells]),
            |(mut pat, mut col), (op, oc)| {
                for i in 0..cells {
                    pat[i] |= op[i];
                    if oc[i] != 0 {
                        col[i] = oc[i];
                    }
                }
                (pat, col)
            },
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;
    use ratatui::style::{Color, Style};
    use ratatui::symbols;
    use ratatui::text::Span;
    use ratatui::widgets::Widget;
    use ratatui::widgets::canvas::{Canvas, Line};
    use wrfm_raster::Model;

    fn cube() -> Model {
        Model {
            vertices: vec![
                (-1.0, -1.0, -1.0),
                (1.0, -1.0, -1.0),
                (1.0, 1.0, -1.0),
                (-1.0, 1.0, -1.0),
                (-1.0, -1.0, 1.0),
                (1.0, -1.0, 1.0),
                (1.0, 1.0, 1.0),
                (-1.0, 1.0, 1.0),
            ],
            edges: vec![
                (0, 1),
                (1, 2),
                (2, 3),
                (3, 0),
                (4, 5),
                (5, 6),
                (6, 7),
                (7, 4),
                (0, 4),
                (1, 5),
                (2, 6),
                (3, 7),
            ],
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

    /// Render the model exactly as the pre-L2 braille path did (golden).
    fn render_ratatui(model: &Model, view: &ViewState, w: u16, h: u16) -> Buffer {
        let px_w = w as usize * 2;
        let px_h = h as usize * 4;
        let mut buf = Buffer::empty(Rect::new(0, 0, w, h));
        let canvas = Canvas::default()
            .marker(symbols::Marker::Braille)
            .x_bounds([-(px_w as f64) / 2.0, (px_w as f64) / 2.0])
            .y_bounds([-(px_h as f64) / 2.0, (px_h as f64) / 2.0])
            .paint(|ctx| {
                for &(a, b) in &model.edges {
                    if let (Some(p1), Some(p2)) = (
                        crate::view::project_point(model.vertices[a], view, px_h),
                        crate::view::project_point(model.vertices[b], view, px_h),
                    ) {
                        ctx.draw(&Line {
                            x1: p1.0,
                            y1: p1.1,
                            x2: p2.0,
                            y2: p2.1,
                            color: Color::Cyan,
                        });
                    }
                }
                let axis_len = crate::view::model_extent(model) / 0.618;
                let origin = crate::view::project_point((0.0, 0.0, 0.0), view, px_h);
                let ends = [
                    (
                        crate::view::project_point((axis_len, 0.0, 0.0), view, px_h),
                        "X",
                        Color::Red,
                    ),
                    (
                        crate::view::project_point((0.0, axis_len, 0.0), view, px_h),
                        "Y",
                        Color::Yellow,
                    ),
                    (
                        crate::view::project_point((0.0, 0.0, axis_len), view, px_h),
                        "Z",
                        Color::LightBlue,
                    ),
                ];
                if let Some(o) = origin {
                    for (end, label, color) in ends {
                        if let Some(e) = end {
                            ctx.draw(&Line {
                                x1: o.0,
                                y1: o.1,
                                x2: e.0,
                                y2: e.1,
                                color,
                            });
                            let left = -(px_w as f64) / 2.0;
                            let top = (px_h as f64) / 2.0;
                            let cell_w = (px_w as f64) / (w - 1) as f64;
                            let cell_h = (px_h as f64) / (h - 1) as f64;
                            let label_x = left + ((e.0 - left) / cell_w).round() * cell_w;
                            let label_y = top - ((top - e.1) / cell_h).round() * cell_h;
                            ctx.print(
                                label_x,
                                label_y,
                                Span::styled(label, Style::default().fg(color)),
                            );
                        }
                    }
                }
            });
        canvas.render(Rect::new(0, 0, w, h), &mut buf);
        buf
    }

    /// Compare the L2 rasterizer against the ratatui golden render cell by cell.
    fn assert_identical(model: &Model, view: &ViewState, w: u16, h: u16) {
        let golden = render_ratatui(model, view, w, h);
        let mut screen = Screen::new(w as usize, h as usize);
        let mut raster = Rasterizer::new();
        raster.resize(w as usize, h as usize);
        raster.render(
            model,
            view,
            (0, 0, w as usize, h as usize),
            true,
            view::model_extent(model),
            &mut screen,
        );
        for y in 0..h {
            for x in 0..w {
                let cell = &golden[(x, y)];
                let expected_ch = cell.symbol().chars().next().unwrap_or(' ');
                let expected_fg = ink_idx(cell.fg);
                let got = screen.cell(x as usize, y as usize);
                let got_ch = char::from_u32((got >> 16) as u32).unwrap();
                let got_fg = (got & 0xff) as u8;
                assert_eq!(
                    got_ch, expected_ch,
                    "char mismatch at ({x},{y}) view={view:?}"
                );
                assert_eq!(
                    got_fg, expected_fg,
                    "fg mismatch at ({x},{y}) view={view:?}"
                );
            }
        }
    }

    fn fit(m: &Model) -> ViewState {
        let mut v = ViewState::default();
        v.fit_to(m);
        v
    }

    #[test]
    fn rasterizer_matches_ratatui_canvas_cube() {
        let m = cube();
        let mut v = fit(&m);
        assert_identical(&m, &v, 40, 20);
        // A rotated + panned + zoomed view (different projection paths).
        v.add_yaw(0.7);
        v.add_pitch(-0.4);
        v.add_dist_delta(-1.5);
        v.pan_x = 3.0;
        v.pan_y = -2.0;
        assert_identical(&m, &v, 40, 20);
        assert_identical(&m, &v, 60, 30);
        assert_identical(&m, &v, 17, 9);
        // Roll.
        let mut v2 = fit(&m);
        v2.roll = 0.9;
        assert_identical(&m, &v2, 40, 20);
    }

    #[test]
    fn rasterizer_matches_ratatui_canvas_tetra() {
        let m = tetra();
        let mut v = fit(&m);
        assert_identical(&m, &v, 40, 20);
        v.add_yaw(1.2);
        v.add_pitch(0.3);
        assert_identical(&m, &v, 33, 11);
    }

    #[test]
    fn project_batch_equals_project_point_bitwise() {
        let m = cube();
        let mut v = fit(&m);
        v.add_yaw(0.7);
        v.add_pitch(-0.4);
        v.roll = 0.9;
        v.pan_x = 3.0;
        v.pan_y = -2.0;
        let mut out = vec![[0.0; 2]; m.vertices.len()];
        let mut ok = vec![false; m.vertices.len()];
        crate::view::project_batch(&m.vertices, &v, 80, &mut out, &mut ok);
        for (i, p) in m.vertices.iter().enumerate() {
            match crate::view::project_point(*p, &v, 80) {
                Some((x, y)) => {
                    assert!(ok[i], "vertex {i} should project");
                    assert_eq!(out[i][0].to_bits(), x.to_bits(), "x bits vertex {i}");
                    assert_eq!(out[i][1].to_bits(), y.to_bits(), "y bits vertex {i}");
                }
                None => assert!(!ok[i], "vertex {i} should be behind camera"),
            }
        }
    }

    #[test]
    fn screen_present_writes_the_background_with_the_foreground() {
        // The statusline is drawn on a filled ground, so a cell's SGR has to
        // carry both inks; the default pair must still collapse to a bare
        // reset, which is what the canvas (all default cells) relies on.
        let mut s = Screen::new(2, 1);
        s.set(0, 0, 'A', Ink::White as u8, Ink::Green as u8);
        s.set(1, 0, 'B', Ink::Default as u8, Ink::Default as u8);
        let mut out = Vec::new();
        s.present(&mut out).unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("\x1b[0;37;42m"), "white on green: {text:?}");
        assert!(
            text.contains("\x1b[0m"),
            "a default pair is a bare reset: {text:?}"
        );
    }

    #[test]
    fn screen_treats_a_repainted_background_as_a_change() {
        // Diffs compare the whole cell, so repainting a ground under unchanged
        // text still reaches the terminal.
        let mut s = Screen::new(1, 1);
        s.set(0, 0, 'A', Ink::White as u8, Ink::Default as u8);
        let mut out = Vec::new();
        s.present(&mut out).unwrap();
        s.set(0, 0, 'A', Ink::White as u8, Ink::Green as u8);
        out.clear();
        assert_eq!(s.present(&mut out).unwrap(), 1, "the ground changed");
        // The render loop redraws every frame; the same ground again is a
        // no-op, so the repaint costs nothing after the first.
        s.set(0, 0, 'A', Ink::White as u8, Ink::Green as u8);
        out.clear();
        assert_eq!(s.present(&mut out).unwrap(), 0, "and only once");
    }

    #[test]
    fn screen_present_writes_only_changed_cells() {
        let mut s = Screen::new(5, 3);
        let draw = |s: &mut Screen| {
            s.set(0, 0, 'A', Ink::Cyan as u8, Ink::Default as u8);
            s.set(4, 2, '⣿', Ink::Red as u8, Ink::Default as u8);
        };
        draw(&mut s);
        let mut out = Vec::new();
        let changed = s.present(&mut out).unwrap();
        assert_eq!(changed, 2, "two cells changed on first frame");
        assert!(!out.is_empty());
        // A no-op frame (same content) writes nothing.
        draw(&mut s);
        out.clear();
        let changed2 = s.present(&mut out).unwrap();
        assert_eq!(changed2, 0);
        assert!(out.is_empty(), "identical frame must emit nothing");
        // Changing one cell emits only that cell's run.
        draw(&mut s);
        s.set(2, 1, 'B', Ink::Default as u8, Ink::Default as u8);
        out.clear();
        let changed3 = s.present(&mut out).unwrap();
        assert_eq!(changed3, 1);
    }

    #[test]
    fn screen_resize_emits_clear_screen() {
        // Regression: after a resize the terminal's OLD pixels must be
        // wiped with ESC[2J, or stale model pixels survive alongside the
        // re-projected model ("two models" after resizing).
        let mut s = Screen::new(4, 2);
        s.set(0, 0, 'Z', Ink::Yellow as u8, Ink::Default as u8);
        let mut out = Vec::new();
        s.present(&mut out).unwrap();
        s.resize(8, 3);
        out.clear();
        s.present(&mut out).unwrap();
        assert!(
            out.windows(4).any(|w| w == b"\x1b[2J"),
            "resize must emit a clear-screen; got {:?}",
            String::from_utf8_lossy(&out)
        );
        // The second present after the resize must NOT clear again.
        out.clear();
        s.present(&mut out).unwrap();
        assert!(
            !out.windows(4).any(|w| w == b"\x1b[2J"),
            "only the first post-resize present clears"
        );
    }

    #[test]
    fn screen_resize_repaints_fully() {
        let mut s = Screen::new(4, 2);
        s.set(0, 0, 'Z', Ink::Yellow as u8, Ink::Default as u8);
        let mut out = Vec::new();
        s.present(&mut out).unwrap();
        s.resize(8, 3);
        s.set(7, 2, 'Q', Ink::Red as u8, Ink::Default as u8);
        out.clear();
        // After a resize the previous frame is all-space, so the single
        // non-space cell is the only change.
        let changed = s.present(&mut out).unwrap();
        assert_eq!(changed, 1);
    }

    /// Whether `params` (the bytes of one `ESC[…m`) leave a background colour
    /// selected. Our writer only ever emits a leading reset, an fg slot and a
    /// bg slot, but this reads any SGR: `0` clears, `49` is the default
    /// background, `40..=47` / `100..=107` select one.
    fn sgr_selects_background(params: &[u8]) -> bool {
        let mut bg = false;
        for part in String::from_utf8_lossy(params).split(';') {
            match part.parse::<u16>() {
                Ok(0) | Ok(49) => bg = false,
                Ok(40..=47) | Ok(100..=107) => bg = true,
                _ => {}
            }
        }
        bg
    }

    /// Whether any `ESC[2J` in `data` runs while a background colour is still
    /// selected. An erase uses the *selected* background on any terminal with
    /// background-colour-erase (xterm's default), so a clear in that state
    /// floods the screen with the colour instead of blanking it.
    fn clears_with_background_selected(data: &[u8]) -> bool {
        let mut bg = false;
        let mut i = 0;
        while i < data.len() {
            if data[i] != 0x1b {
                i += 1;
                continue;
            }
            if data.get(i + 1) != Some(&b'[') {
                i += 2;
                continue;
            }
            let start = i + 2;
            let mut end = start;
            while end < data.len() && !(0x40..=0x7e).contains(&data[end]) {
                end += 1;
            }
            if end >= data.len() {
                break;
            }
            match data[end] {
                b'm' => bg = sgr_selects_background(&data[start..end]),
                b'J' if &data[start..end] == b"2" && bg => return true,
                _ => {}
            }
            i = end + 1;
        }
        false
    }

    #[test]
    fn present_leaves_no_background_selected() {
        // A frame's last run is the statusline, whose ground is a filled
        // background. Leaving it selected hands every later erase — the
        // clear-screen of a resize, the terminal's own fill of the rows a
        // resize exposes — the strip's colour, which then shows up as the
        // statusline's background flooding the canvas.
        let mut s = Screen::new(4, 2);
        s.set(0, 1, 'X', Ink::White as u8, Ink::Black as u8);
        let mut out = Vec::new();
        s.present(&mut out).unwrap();
        assert!(
            out.ends_with(b"\x1b[0m"),
            "the frame must end with default colours: {:?}",
            String::from_utf8_lossy(&out)
        );
        // A frame that redraws exactly what is already on the terminal has
        // nothing to write — and so nothing to reset either.
        s.set(0, 1, 'X', Ink::White as u8, Ink::Black as u8);
        out.clear();
        assert_eq!(s.present(&mut out).unwrap(), 0);
        assert!(out.is_empty(), "an idle frame writes nothing: {out:?}");
    }

    #[test]
    fn resize_clear_never_erases_with_a_background_selected() {
        // Regression: the frame before the resize leaves the statusline's
        // ground selected, so the resize's ESC[2J would paint the whole
        // screen — canvas included — with it.
        // Both frames go to the SAME stream, exactly as stdout sees them:
        // the clear is emitted by the second frame, but the colours it erases
        // with are the ones the first frame left selected.
        let mut s = Screen::new(4, 2);
        s.set(0, 1, 'X', Ink::White as u8, Ink::Black as u8);
        let mut out = Vec::new();
        s.present(&mut out).unwrap();
        s.resize(8, 4);
        s.set(0, 3, 'Y', Ink::Cyan as u8, Ink::Default as u8);
        s.present(&mut out).unwrap();
        assert!(
            !clears_with_background_selected(&out),
            "ESC[2J must run at default colours: {:?}",
            String::from_utf8_lossy(&out)
        );
        assert!(
            out.windows(4).any(|w| w == b"\x1b[2J"),
            "the resize still clears: {:?}",
            String::from_utf8_lossy(&out)
        );
    }

    /// The rows `(top, bottom)` that hold a cell of `color`, or `None` when
    /// no cell of that color is on screen.
    fn color_rows(screen: &Screen, color: Ink) -> Option<(usize, usize)> {
        let (w, h) = screen.size();
        let mut rows: Vec<usize> = Vec::new();
        for y in 0..h {
            if (0..w).any(|x| screen.cell(x, y) as u8 == color as u8) {
                rows.push(y);
            }
        }
        rows.first().copied().zip(rows.last().copied())
    }

    #[test]
    fn empty_model_renders_the_origin_cross() {
        // The blank start-up view (`wireforge` with no FILE): no geometry, but
        // the space is still there — the three axes are drawn from the unit
        // scene (see `extent_from_bounds`), so the viewer shows an origin
        // cross instead of an empty field, and X/Y/Z stay visible labels.
        let m = Model::default();
        let e = view::model_extent(&m);
        let mut v = ViewState::default();
        v.fit_to(&m);
        let mut raster = Rasterizer::new();
        raster.resize(120, 29);
        let mut screen = Screen::new(120, 30);
        raster.render(&m, &v, (0, 1, 120, 29), true, e, &mut screen);

        // All three axes are on screen, in their own colors.
        for (axis, color) in [("X", Ink::Red), ("Y", Ink::Yellow), ("Z", Ink::LightBlue)] {
            assert!(
                color_rows(&screen, color).is_some(),
                "{axis} axis must be drawn for an empty model"
            );
        }
        // The cross is sized by the unit scene, not collapsed into one dot:
        // the Y arm is 1.618 world units at the fit distance, which is a
        // substantial part of a 29-row canvas.
        let (top, bottom) = color_rows(&screen, Ink::Yellow).expect("Y axis");
        assert!(
            bottom - top >= 8,
            "the origin cross must have real size, got rows {top}..={bottom}"
        );
        // It is a cross, not a full field: most cells stay untouched.
        let lit = (0..30)
            .flat_map(|y| (0..120).map(move |x| (x, y)))
            .filter(|&(x, y)| {
                screen.cell(x, y) != pack(' ', Ink::Default as u8, Ink::Default as u8)
            })
            .count();
        assert!(
            lit < 30 * 120 / 4,
            "an empty scene must stay mostly blank, lit {lit} cells"
        );
    }

    #[test]
    fn empty_model_view_rotates_and_pans() {
        // "The model is empty but the space still exists": every camera key
        // must still change what is on screen, so the origin cross (the only
        // geometry there is) has to move when the view does.
        let m = Model::default();
        let e = view::model_extent(&m);
        let mut v = ViewState::default();
        v.fit_to(&m);
        let mut raster = Rasterizer::new();
        raster.resize(120, 29);
        let frame = |v: &ViewState, raster: &mut Rasterizer| {
            let mut screen = Screen::new(120, 30);
            raster.render(&m, v, (0, 1, 120, 29), true, e, &mut screen);
            (0..30)
                .flat_map(|y| (0..120).map(move |x| (x, y)))
                .map(|(x, y)| screen.cell(x, y))
                .collect::<Vec<u64>>()
        };
        // The X arm's endpoint is the corner the cross is read by.
        let x_end = view::model_extent(&m) / 0.618;
        let base = frame(&v, &mut raster);
        let (x_before, _) =
            view::project_point((x_end, 0.0, 0.0), &v, 116).expect("X arm must project");

        v.add_yaw(0.4);
        let yawed = frame(&v, &mut raster);
        assert_ne!(base, yawed, "yaw must redraw the empty scene");
        let (x_after, _) =
            view::project_point((x_end, 0.0, 0.0), &v, 116).expect("X arm must project");
        assert!(
            (x_before - x_after).abs() > 1.0,
            "yaw must swing the X axis across the canvas: {x_before} vs {x_after}"
        );

        v.pan_x = 1.0;
        let panned = frame(&v, &mut raster);
        assert_ne!(yawed, panned, "pan must redraw the empty scene");

        v.add_dist_delta(-1.0);
        let dollied = frame(&v, &mut raster);
        assert_ne!(panned, dollied, "dolly must redraw the empty scene");

        // Tab still turns the axes off: with no geometry, that now leaves a
        // genuinely blank canvas.
        let mut screen = Screen::new(120, 30);
        raster.render(&m, &v, (0, 1, 120, 29), false, e, &mut screen);
        let lit = (0..30)
            .flat_map(|y| (0..120).map(move |x| (x, y)))
            .filter(|&(x, y)| {
                screen.cell(x, y) != pack(' ', Ink::Default as u8, Ink::Default as u8)
            })
            .count();
        assert_eq!(lit, 0, "axes off on an empty model draws nothing");
    }

    #[test]
    fn braille_lut_is_ratatui_permutation() {
        // ratatui's table maps a row-major pattern to the Unicode braille
        // char and is a permutation of U+2800+pattern (e.g. 0x55 -> '\u{2847}').
        for (i, c) in ratatui::symbols::braille::BRAILLE.iter().enumerate() {
            assert!((0x2800u32..=0x28ff).contains(&(*c as u32)), "LUT[{i}]");
        }
        assert_eq!(ratatui::symbols::braille::BRAILLE[0x55], '\u{2847}');
        assert_eq!(ratatui::symbols::braille::BRAILLE[0x02], '\u{2808}');
        assert_eq!(ratatui::symbols::braille::BRAILLE[255], '\u{28ff}');
    }
    /// Build a `nx`x`ny` planar grid model (100x100 = 10k verts / 19.8k edges).
    fn grid(nx: usize, ny: usize) -> Model {
        let mut verts = Vec::with_capacity(nx * ny);
        for y in 0..ny {
            for x in 0..nx {
                verts.push((x as f64 * 0.05, y as f64 * 0.05, 0.0));
            }
        }
        let mut edges = Vec::new();
        let vid = |x: usize, y: usize| y * nx + x;
        for y in 0..ny {
            for x in 0..nx {
                if x + 1 < nx {
                    edges.push((vid(x, y), vid(x + 1, y)));
                }
                if y + 1 < ny {
                    edges.push((vid(x, y), vid(x, y + 1)));
                }
            }
        }
        Model {
            vertices: verts,
            edges,
        }
    }

    /// Acceptance-criterion #7 measurement: per-frame cost at 120x30.
    #[test]
    fn bench_frame_cost_small() {
        let m = cube();
        let e = view::model_extent(&m);
        let mut v = ViewState::default();
        v.fit_to(&m);
        let mut raster = Rasterizer::new();
        raster.resize(120, 29);
        let mut screen = Screen::new(120, 30);
        let mut out = Vec::new();
        // warm-up (allocation + caches)
        for _ in 0..100 {
            v.add_yaw(0.001);
            raster.render(&m, &v, (0, 1, 120, 29), true, e, &mut screen);
            let _ = screen.present(&mut out).unwrap();
            out.clear();
        }
        let n = 2000;
        let t0 = std::time::Instant::now();
        for _ in 0..n {
            v.add_yaw(0.001);
            raster.render(&m, &v, (0, 1, 120, 29), true, e, &mut screen);
            let _ = screen.present(&mut out).unwrap();
            out.clear();
        }
        let dt = t0.elapsed() / n;
        println!(
            "BENCH small (120x30, {}v/{}e): {:?}/frame ({:.0} FPS)",
            m.vertices.len(),
            m.edges.len(),
            dt,
            1e9 / dt.as_nanos() as f64
        );
    }

    #[test]
    fn bench_frame_cost_20k() {
        let m = grid(100, 100); // 10k verts / 19.8k edges
        let e = view::model_extent(&m);
        let mut v = ViewState::default();
        v.fit_to(&m);
        let mut raster = Rasterizer::new();
        raster.resize(120, 29);
        let mut screen = Screen::new(120, 30);
        let mut out = Vec::new();
        for _ in 0..20 {
            v.add_yaw(0.001);
            raster.render(&m, &v, (0, 1, 120, 29), true, e, &mut screen);
            let _ = screen.present(&mut out).unwrap();
            out.clear();
        }
        let n = 200;
        let t0 = std::time::Instant::now();
        for _ in 0..n {
            v.add_yaw(0.001);
            raster.render(&m, &v, (0, 1, 120, 29), true, e, &mut screen);
            let _ = screen.present(&mut out).unwrap();
            out.clear();
        }
        let dt = t0.elapsed() / n;
        println!(
            "BENCH 20k (120x30, {}v/{}e): {:?}/frame ({:.0} FPS)",
            m.vertices.len(),
            m.edges.len(),
            dt,
            1e9 / dt.as_nanos() as f64
        );
    }

    #[test]
    fn bench_frame_cost_medium_and_parallel() {
        // ~182-vertex "real" model (the plan's exit.wrfm was 182v/193e).
        let med = grid(14, 13); // 182 verts / 338 edges
        let me = view::model_extent(&med);
        let mut vm = ViewState::default();
        vm.fit_to(&med);
        let mut r1 = Rasterizer::new();
        r1.resize(120, 29);
        let mut s1 = Screen::new(120, 30);
        let mut out = Vec::new();
        for _ in 0..100 {
            vm.add_yaw(0.001);
            r1.render(&med, &vm, (0, 1, 120, 29), true, me, &mut s1);
            let _ = s1.present(&mut out).unwrap();
            out.clear();
        }
        let n = 1000;
        let t0 = std::time::Instant::now();
        for _ in 0..n {
            vm.add_yaw(0.001);
            r1.render(&med, &vm, (0, 1, 120, 29), true, me, &mut s1);
            let _ = s1.present(&mut out).unwrap();
            out.clear();
        }
        let d1 = t0.elapsed() / n;
        println!(
            "BENCH medium (120x30, {}v/{}e): {:?}/frame ({:.0} FPS)",
            med.vertices.len(),
            med.edges.len(),
            d1,
            1e9 / d1.as_nanos() as f64
        );

        // >50k edges: exercises the parallel rasterization path (Stage C).
        let big = grid(240, 240); // 57.6k verts / 114.7k edges
        let be = view::model_extent(&big);
        let mut vb = ViewState::default();
        vb.fit_to(&big);
        let mut r2 = Rasterizer::new();
        r2.resize(120, 29);
        let mut s2 = Screen::new(120, 30);
        for _ in 0..3 {
            vb.add_yaw(0.001);
            r2.render(&big, &vb, (0, 1, 120, 29), true, be, &mut s2);
            let _ = s2.present(&mut out).unwrap();
            out.clear();
        }
        let nb = 30;
        let t1 = std::time::Instant::now();
        for _ in 0..nb {
            vb.add_yaw(0.001);
            r2.render(&big, &vb, (0, 1, 120, 29), true, be, &mut s2);
            let _ = s2.present(&mut out).unwrap();
            out.clear();
        }
        let d2 = t1.elapsed() / nb;
        println!(
            "BENCH 100k-parallel (120x30, {}v/{}e): {:?}/frame ({:.0} FPS)",
            big.vertices.len(),
            big.edges.len(),
            d2,
            1e9 / d2.as_nanos() as f64
        );
    }
}
