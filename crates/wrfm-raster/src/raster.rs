//! Line rasterization: Cohen-Sutherland clipping, canvas -> dot-grid
//! mapping, Bresenham stepping and braille cell encoding.
//!
//! This is the algorithm behind ratatui's `Canvas` widget (its clipping is
//! the `line_clipping` crate's Cohen-Sutherland, its mapping is
//! `Painter::get_point`, its stepping is `for_each_line_point`), ported so
//! callers can rasterize into their own buffers without a widget tree —
//! with per-cell color, incremental frames, or parallel edge chunks.
//! `tests/golden.rs` renders with both and asserts byte-identical output.

/// ratatui's braille pattern table: a row-major 2x4 dot pattern -> the
/// Unicode braille char. A permutation of U+2800+pattern (e.g. `[0x55] ==
/// '\u{2847}'`), NOT the identity — using ratatui's own table is what keeps
/// the cells byte-identical to the Canvas path.
pub use ratatui::symbols::braille::BRAILLE;

/// The canvas-space window `[left, right] x [bottom, top]` that lines are
/// clipped against and mapped into (canvas y grows UP).
///
/// For the full frame this is the centered box `[-px_w/2, px_w/2] x
/// [-px_h/2, px_h/2]`; a region zoom (CLI `--region` / `--fit content`)
/// yields an off-center window with the same mapping.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Bounds {
    pub left: f64,
    pub right: f64,
    pub bottom: f64,
    pub top: f64,
}

impl Bounds {
    /// The window for a `px_w` x `px_h` dot grid centered on the origin.
    pub fn centered(px_w: usize, px_h: usize) -> Self {
        let (w, h) = (px_w as f64, px_h as f64);
        Self {
            left: -w / 2.0,
            right: w / 2.0,
            bottom: -h / 2.0,
            top: h / 2.0,
        }
    }

    /// Build from a canvas x/y bounds pair (`[min, max]` per axis), the
    /// shape ratatui's `Canvas::x_bounds` / `y_bounds` take.
    pub fn from_arrays(x_bounds: [f64; 2], y_bounds: [f64; 2]) -> Self {
        Self {
            left: x_bounds[0],
            right: x_bounds[1],
            bottom: y_bounds[0],
            top: y_bounds[1],
        }
    }
}

/// Rasterize one line segment into a 2x4-dot-per-cell braille grid.
///
/// * `(x1, y1) -> (x2, y2)` — canvas coordinates (+y up).
/// * `px_w`, `px_h` — the grid in DOTS (`cells * 2` wide, `cells * 4` tall).
/// * `bounds` — the canvas window to clip and map against.
/// * `on_cell(cell, bit)` — called for every dot on the line with its cell
///   index and pattern bit; the caller owns the buffers (pattern + color).
///   It is called even for dots that are already lit, so a later line can
///   overwrite the cell's color — exactly like the Canvas layer order.
///
/// A degenerate window (`right <= left` or `top <= bottom`) draws nothing,
/// matching ratatui's `get_point` guard.
#[allow(clippy::too_many_arguments)] // primitive geometry helper (x1,y1,x2,y2 + grid + window + sink)
#[inline]
pub fn rasterize_line(
    x1: f64,
    y1: f64,
    x2: f64,
    y2: f64,
    px_w: usize,
    px_h: usize,
    bounds: Bounds,
    mut on_cell: impl FnMut(usize, u8),
) {
    let Some((cx1, cy1, cx2, cy2)) = clip_line(x1, y1, x2, y2, bounds) else {
        return;
    };
    let Some((dx1, dy1)) = get_point(cx1, cy1, px_w, px_h, bounds) else {
        return;
    };
    let Some((dx2, dy2)) = get_point(cx2, cy2, px_w, px_h, bounds) else {
        return;
    };
    bresenham(dx1, dy1, dx2, dy2, |x, y| {
        if x >= px_w || y >= px_h {
            return;
        }
        let cell = (y >> 2) * (px_w / 2) + (x >> 1);
        on_cell(cell, 1 << ((x & 1) + 2 * (y & 3)));
    });
}

/// The braille char for a lit pattern (0..=255); empty cells are handled by
/// [`dots_to_lines`], which emits a plain space instead of U+2800.
#[inline]
pub fn braille_char(pattern: u8) -> char {
    BRAILLE[pattern as usize]
}

/// Clip one segment against the camera's near plane and rasterize the
/// visible part into a braille grid.
///
/// `a` and `b` are camera-space points (`[x, y, z]`, `z` = distance in front
/// of the camera, with `z <= NEAR` behind it, see
/// [`crate::projection::NEAR`]); `project` maps such a point to canvas
/// coordinates — for [`crate::projection::Camera`] that is the focal divide
/// `[x, y] * focal / z`. [`crate::projection::project_camera_point`] is the
/// free-function form of it.
///
/// Dropping a segment because ONE endpoint sits behind the camera plane
/// throws away the part that is still in view: the whole segment vanishes.
/// This clips the segment at the plane first and rasterizes the remainder
/// through the SAME `project`, so the visible part lands exactly where its
/// unclipped projection would. A segment entirely behind the plane draws
/// nothing, and `project` is only ever called with points in front of the
/// plane, so it never has to divide by a zero depth.
pub fn rasterize_camera_line(
    a: [f64; 3],
    b: [f64; 3],
    project: impl Fn([f64; 3]) -> (f64, f64),
    px_w: usize,
    px_h: usize,
    bounds: Bounds,
    on_cell: impl FnMut(usize, u8),
) {
    let Some((p1, p2)) = clip_near(a, b) else {
        return;
    };
    let (x1, y1) = project(p1);
    let (x2, y2) = project(p2);
    rasterize_line(x1, y1, x2, y2, px_w, px_h, bounds, on_cell);
}

/// Clip a camera-space segment at the near plane, returning two
/// camera-space points that are both in front of it (`z > NEAR`).
///
/// `None` when the whole segment is behind the plane. This is what
/// [`rasterize_camera_line`] clips with; callers that want the clipped
/// endpoint itself (to hang a label on, say) can call it directly.
pub fn clip_near(a: [f64; 3], b: [f64; 3]) -> Option<([f64; 3], [f64; 3])> {
    let near = crate::projection::NEAR;
    let behind_a = a[2] <= near;
    let behind_b = b[2] <= near;
    if behind_a && behind_b {
        return None;
    }
    if !behind_a && !behind_b {
        return Some((a, b));
    }
    // One endpoint is behind the plane: solve `lerp(front, behind, t).z == near`.
    let (front, behind) = if behind_a { (b, a) } else { (a, b) };
    let t = (near - front[2]) / (behind[2] - front[2]);
    let hit = [
        front[0] + t * (behind[0] - front[0]),
        front[1] + t * (behind[1] - front[1]),
        near,
    ];
    if behind_a {
        Some((hit, b))
    } else {
        Some((a, hit))
    }
}

/// Encode a dot grid (`cw * ch` per-cell patterns, row-major) as braille
/// lines. A zero pattern becomes a space — that is what ratatui's Canvas
/// leaves in an empty buffer cell, so text output stays byte-identical.
pub fn dots_to_lines(dots: &[u8], cw: usize, ch: usize) -> Vec<String> {
    debug_assert_eq!(dots.len(), cw * ch);
    (0..ch)
        .map(|cy| {
            (0..cw)
                .map(|cx| {
                    let p = dots[cy * cw + cx];
                    if p == 0 { ' ' } else { BRAILLE[p as usize] }
                })
                .collect()
        })
        .collect()
}

/// Cohen-Sutherland clip against `[left, right] x [bottom, top]` — the same
/// algorithm, region order and intersection expressions as the
/// `line_clipping` crate ratatui uses (verified bit-identical).
fn clip_line(x1: f64, y1: f64, x2: f64, y2: f64, b: Bounds) -> Option<(f64, f64, f64, f64)> {
    let (left, right, bottom, top) = (b.left, b.right, b.bottom, b.top);
    let mut p1 = (x1, y1);
    let mut p2 = (x2, y2);
    let mut r1 = region_code(p1, left, right, bottom, top);
    let mut r2 = region_code(p2, left, right, bottom, top);
    loop {
        if r1 & r2 != 0 {
            return None;
        }
        if r1 != 0 {
            p1 = intersect(p1, p2, r1, left, right, bottom, top);
            r1 = region_code(p1, left, right, bottom, top);
        } else if r2 != 0 {
            p2 = intersect(p2, p1, r2, left, right, bottom, top);
            r2 = region_code(p2, left, right, bottom, top);
        } else {
            return Some((p1.0, p1.1, p2.0, p2.1));
        }
    }
}

fn region_code(p: (f64, f64), left: f64, right: f64, bottom: f64, top: f64) -> u8 {
    let mut r = 0u8;
    if p.0 < left {
        r |= 1;
    } else if p.0 > right {
        r |= 2;
    }
    if p.1 < bottom {
        r |= 4;
    } else if p.1 > top {
        r |= 8;
    }
    r
}

/// Intersect the segment with the window boundary identified by `code`.
fn intersect(
    p1: (f64, f64),
    p2: (f64, f64),
    region: u8,
    left: f64,
    right: f64,
    bottom: f64,
    top: f64,
) -> (f64, f64) {
    let dx = p2.0 - p1.0;
    let dy = p2.1 - p1.1;
    if region & 1 != 0 {
        let y = p1.1 + (left - p1.0) * dy / dx;
        return (left, y);
    }
    if region & 2 != 0 {
        let y = p1.1 + (right - p1.0) * dy / dx;
        return (right, y);
    }
    if region & 4 != 0 {
        let x = p1.0 + (bottom - p1.1) * dx / dy;
        return (x, bottom);
    }
    debug_assert!(region & 8 != 0);
    let x = p1.0 + (top - p1.1) * dx / dy;
    (x, top)
}

/// Canvas -> dot-grid mapping (`Painter::get_point`): reject points outside
/// the window, then scale-and-round into `[0, px_w - 1] x [0, px_h - 1]`
/// using the GRID resolution for the scale and the window for the span.
/// A degenerate window maps nothing (ratatui's `width <= 0` guard).
fn get_point(x: f64, y: f64, px_w: usize, px_h: usize, b: Bounds) -> Option<(usize, usize)> {
    if x < b.left || x > b.right || y < b.bottom || y > b.top {
        return None;
    }
    let width = b.right - b.left;
    let height = b.top - b.bottom;
    if width <= 0.0 || height <= 0.0 {
        return None;
    }
    let px_wf = px_w as f64;
    let px_hf = px_h as f64;
    let xd = ((x - b.left) * (px_wf - 1.0) / width).round() as usize;
    let yd = ((b.top - y) * (px_hf - 1.0) / height).round() as usize;
    Some((xd, yd))
}

/// Bresenham line stepping, byte-identical to ratatui's
/// `for_each_line_point`.
fn bresenham(x1: usize, y1: usize, x2: usize, y2: usize, mut f: impl FnMut(usize, usize)) {
    let dx = x2.abs_diff(x1);
    let dy = y2.abs_diff(y1);
    if dx == 0 {
        for y in y1.min(y2)..=y1.max(y2) {
            f(x1, y);
        }
    } else if dy == 0 {
        for x in x1.min(x2)..=x1.max(x2) {
            f(x, y1);
        }
    } else if dy < dx {
        if x1 > x2 {
            line_low(x2, y2, x1, y1, &mut f);
        } else {
            line_low(x1, y1, x2, y2, &mut f);
        }
    } else if y1 > y2 {
        line_high(x2, y2, x1, y1, &mut f);
    } else {
        line_high(x1, y1, x2, y2, &mut f);
    }
}

fn line_low(x1: usize, y1: usize, x2: usize, y2: usize, f: &mut impl FnMut(usize, usize)) {
    let dx = (x2 - x1) as isize;
    let dy = (y2 as isize - y1 as isize).abs();
    let mut d = 2 * dy - dx;
    let mut y = y1;
    for x in x1..=x2 {
        f(x, y);
        if d > 0 {
            y = if y1 > y2 {
                y.saturating_sub(1)
            } else {
                y.saturating_add(1)
            };
            d -= 2 * dx;
        }
        d += 2 * dy;
    }
}

fn line_high(x1: usize, y1: usize, x2: usize, y2: usize, f: &mut impl FnMut(usize, usize)) {
    let dx = (x2 as isize - x1 as isize).abs();
    let dy = (y2 - y1) as isize;
    let mut d = 2 * dx - dy;
    let mut x = x1;
    for y in y1..=y2 {
        f(x, y);
        if d > 0 {
            x = if x1 > x2 {
                x.saturating_sub(1)
            } else {
                x.saturating_add(1)
            };
            d -= 2 * dy;
        }
        d += 2 * dx;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Collect the dots a single line lights, as (cell, bit) pairs.
    fn dots_of(
        x1: f64,
        y1: f64,
        x2: f64,
        y2: f64,
        px_w: usize,
        px_h: usize,
        bounds: Bounds,
    ) -> Vec<(usize, u8)> {
        let mut v = Vec::new();
        rasterize_line(x1, y1, x2, y2, px_w, px_h, bounds, |cell, bit| {
            v.push((cell, bit))
        });
        v
    }

    #[test]
    fn braille_table_is_ratatui_permutation() {
        for (i, c) in BRAILLE.iter().enumerate() {
            assert!((0x2800u32..=0x28ff).contains(&(*c as u32)), "LUT[{i}]");
        }
        assert_eq!(BRAILLE[0x55], '\u{2847}');
        assert_eq!(BRAILLE[0x02], '\u{2808}');
        assert_eq!(BRAILLE[255], '\u{28ff}');
        assert_eq!(braille_char(0x01), BRAILLE[1]);
    }

    #[test]
    fn dots_to_lines_uses_spaces_for_empty_cells() {
        // ratatui's Canvas leaves the buffer's default space in empty
        // cells (its PatternGrid saves pattern 0 as "no symbol") — never
        // U+2800.
        assert_eq!(dots_to_lines(&[0, 0, 0, 0], 2, 2), ["  ", "  "]);
        assert_eq!(dots_to_lines(&[0x01, 0], 2, 1), ["⠁ "]);
        assert_eq!(dots_to_lines(&[0xff], 1, 1), ["⣿"]);
    }

    #[test]
    fn centered_line_lights_the_expected_cells() {
        // 4x2 cells = 8x8 dots; window [-4,4]x[-4,4].
        let b = Bounds::centered(8, 8);
        // A horizontal line across the middle row of dots (y=0 -> dot row 3).
        let dots = dots_of(-4.0, 0.0, 4.0, 0.0, 8, 8, b);
        assert!(!dots.is_empty(), "a full-width line must light dots");
        // Every dot: cell = (y>>2)*4 + (x>>1), bit = (x&1) + 2*(y&3) — all
        // in range for an 8x8 grid.
        for (cell, bit) in &dots {
            assert!(*cell < 8, "cell {cell} out of range");
            assert!(bit.count_ones() == 1 && *bit != 0, "bit {bit}");
        }
        // Single dot at the origin maps to the center-ish dot of the grid.
        let dots = dots_of(0.0, 0.0, 0.0, 0.0, 8, 8, b);
        assert_eq!(dots.len(), 1, "a point lights exactly one dot");
    }

    #[test]
    fn degenerate_bounds_draw_nothing() {
        // w=0/h=0 CLI canvases derive a zero-width window: ratatui's
        // get_point guard rejects everything, so the frame stays blank.
        let b = Bounds::from_arrays([0.0, 0.0], [0.0, 0.0]);
        let dots = dots_of(-5.0, -5.0, 5.0, 5.0, 2, 4, b);
        assert!(dots.is_empty(), "degenerate window must draw nothing");
        let b = Bounds::from_arrays([-10.0, 10.0], [5.0, -5.0]); // inverted y
        let dots = dots_of(-5.0, -5.0, 5.0, 5.0, 2, 4, b);
        assert!(dots.is_empty(), "inverted window must draw nothing");
    }

    #[test]
    fn off_window_lines_are_clipped_or_rejected() {
        let b = Bounds::centered(8, 8);
        // Fully outside -> nothing.
        assert!(dots_of(100.0, 100.0, 200.0, 200.0, 8, 8, b).is_empty());
        // Crossing the window -> clipped to it (endpoints inside).
        let dots = dots_of(-100.0, 0.0, 100.0, 0.0, 8, 8, b);
        assert!(!dots.is_empty());
        // Partially outside, diagonal.
        let dots = dots_of(-50.0, -50.0, 1.0, 1.0, 8, 8, b);
        assert!(!dots.is_empty());
    }

    #[test]
    fn get_point_rejects_outside_and_maps_edges() {
        let b = Bounds::centered(40, 32);
        assert_eq!(get_point(0.0, 0.0, 40, 32, b), Some((20, 16)));
        assert_eq!(get_point(-20.0, 16.0, 40, 32, b), Some((0, 0)));
        assert_eq!(get_point(20.0, -16.0, 40, 32, b), Some((39, 31)));
        assert_eq!(get_point(-20.1, 0.0, 40, 32, b), None);
        assert_eq!(get_point(0.0, 16.1, 40, 32, b), None);
    }

    /// Collect the cells `rasterize_camera_line` lights. A scale of 1 makes
    /// the canvases below read as camera-space coordinates.
    fn camera_line_cells(a: [f64; 3], b: [f64; 3], scale: f64) -> Vec<usize> {
        let (px_w, px_h) = (40usize, 32usize);
        let mut cells = Vec::new();
        rasterize_camera_line(
            a,
            b,
            |p| crate::projection::project_camera_point(p, scale),
            px_w,
            px_h,
            Bounds::centered(px_w, px_h),
            |cell, _| cells.push(cell),
        );
        cells.sort_unstable();
        cells.dedup();
        cells
    }

    #[test]
    fn camera_line_clips_at_the_near_plane() {
        // 40x32 dots with scale 1: the canvas covers x in [-20, 20].
        // A segment from (30, 0, 2) — off the canvas to the right — to
        // (2.5, 0, 0.05), whose second endpoint is behind the camera plane.
        // It crosses the plane at x = 5.286…, so the visible part runs from
        // the canvas edge in to x = 5.286: cells must be lit. Dropping the
        // segment with its culled endpoint lit nothing at all.
        let scale = 1.0;
        let cells = camera_line_cells([30.0, 0.0, 2.0], [2.5, 0.0, 0.05], scale);
        assert!(
            !cells.is_empty(),
            "the part of the segment in front of the camera must be drawn"
        );
        // The visible part ends mid-canvas, so the head of the clipped
        // segment is lit while the tail (off the canvas edge) is not: the
        // clipped segment spans a proper sub-run of the row, not the whole
        // canvas width.
        let (px_w, _px_h) = (40usize, 32usize);
        assert!(
            cells.len() < px_w / 2,
            "the clipped segment must not span the canvas: {cells:?}"
        );
        assert!(
            cells
                .iter()
                .all(|c| c / (px_w / 2) == cells[0] / (px_w / 2)),
            "the segment stays on one row: {cells:?}"
        );
        // A segment whose visible part lies entirely outside the canvas
        // draws nothing — clipping must not invent geometry.
        assert!(
            camera_line_cells([60.0, 0.0, 2.0], [40.0, 0.0, 0.05], scale).is_empty(),
            "an off-canvas visible part stays off canvas"
        );
    }

    #[test]
    fn camera_line_keeps_fully_visible_segments_identical() {
        // Both endpoints in front of the plane: this entry point must
        // rasterize exactly what `rasterize_line` does with the same
        // projection applied to the endpoints.
        let f = crate::projection::focal(32.0);
        let project = |p: [f64; 3]| crate::projection::project_camera_point(p, f);
        let (a, b) = ([3.0, -2.0, 6.0], [-4.0, 5.0, 9.0]);
        let want = {
            let (px_w, px_h) = (40usize, 32usize);
            let (x1, y1) = project(a);
            let (x2, y2) = project(b);
            let mut cells = Vec::new();
            rasterize_line(
                x1,
                y1,
                x2,
                y2,
                px_w,
                px_h,
                Bounds::centered(px_w, px_h),
                |cell, _| cells.push(cell),
            );
            cells.sort_unstable();
            cells.dedup();
            cells
        };
        assert_eq!(camera_line_cells(a, b, f), want);
    }

    #[test]
    fn camera_line_behind_the_camera_draws_nothing() {
        // Both endpoints behind the plane: no dot, no panic (the clip
        // denominator would be zero or negative here).
        for b in [[0.0, 0.0, 0.05], [0.0, 0.0, 0.0], [0.0, 0.0, -5.0]] {
            assert!(
                camera_line_cells([1.0, 1.0, 0.1], b, 100.0).is_empty(),
                "segment behind the camera plane drew dots: b={b:?}"
            );
        }
    }

    #[test]
    fn camera_line_at_the_plane_does_not_produce_nan() {
        // An endpoint exactly ON the plane with the other one in front:
        // the interpolation divides by (behind - front).z, which is zero
        // when the "behind" endpoint sits exactly at NEAR. The clipped dot
        // grid must stay a real coordinate (no NaN cell index).
        let cells = camera_line_cells([0.0, 0.0, 1.0], [10.0, 0.0, crate::projection::NEAR], 1.0);
        assert!(!cells.is_empty(), "the segment in front must still draw");
        assert!(
            cells.iter().all(|&c| c < 40 * 32),
            "cell indices must stay in the grid: {cells:?}"
        );
    }
}
