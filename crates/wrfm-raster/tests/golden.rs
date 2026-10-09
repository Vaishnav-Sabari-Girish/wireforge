//! Golden tests: the shared rasterizer must be BYTE-identical to ratatui's
//! `Canvas` widget — the reference the wireforge TUI and `wrfm-cli` were
//! both verified against.
//!
//! Both renderers receive the same already-projected coordinates, so this
//! isolates the raster stage (clip -> map -> Bresenham -> braille cell).
//! Two window shapes are covered:
//!
//! * the centered full-frame window (the TUI path), and
//! * off-center windows from region zooms (the CLI `--region` /
//!   `--fit content` path) plus an arbitrary window — `Bounds` is not
//!   assumed to match the grid resolution.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Color;
use ratatui::symbols;
use ratatui::widgets::Widget;
use ratatui::widgets::canvas::{Canvas, Line};
use ratatui_wireframe::model::Model;
use wrfm_raster::geometry::{auto_dist, world_rot};
use wrfm_raster::projection::{Camera, focal};
use wrfm_raster::raster::{Bounds, dots_to_lines, rasterize_line};

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

/// Project every vertex ONCE; both renderers consume the same coordinates.
fn projected(m: &Model, cam: &Camera, f: f64) -> Vec<Option<(f64, f64)>> {
    m.vertices.iter().map(|&v| cam.project(v, f)).collect()
}

/// The shared path: `rasterize_line` + `dots_to_lines`.
fn render_ours(m: &Model, cam: &Camera, f: f64, w: usize, h: usize, bounds: Bounds) -> Vec<String> {
    let (px_w, px_h) = (w * 2, h * 4);
    let proj = projected(m, cam, f);
    let mut dots = vec![0u8; w * h];
    for &(a, b) in &m.edges {
        if let (Some(p1), Some(p2)) = (proj[a], proj[b]) {
            rasterize_line(p1.0, p1.1, p2.0, p2.1, px_w, px_h, bounds, |cell, bit| {
                dots[cell] |= bit
            });
        }
    }
    dots_to_lines(&dots, w, h)
}

/// The golden: ratatui's `Canvas` widget with the same coordinates and
/// window, read back cell by cell (empty cells read as the buffer's space).
fn render_ratatui(m: &Model, cam: &Camera, f: f64, w: u16, h: u16, bounds: Bounds) -> Vec<String> {
    let proj = projected(m, cam, f);
    let mut buf = Buffer::empty(Rect::new(0, 0, w, h));
    let canvas = Canvas::default()
        .marker(symbols::Marker::Braille)
        .x_bounds([bounds.left, bounds.right])
        .y_bounds([bounds.bottom, bounds.top])
        .paint(|ctx| {
            for &(a, b) in &m.edges {
                if let (Some(p1), Some(p2)) = (proj[a], proj[b]) {
                    ctx.draw(&Line {
                        x1: p1.0,
                        y1: p1.1,
                        x2: p2.0,
                        y2: p2.1,
                        color: Color::White,
                    });
                }
            }
        });
    canvas.render(Rect::new(0, 0, w, h), &mut buf);
    (0..h)
        .map(|y| {
            (0..w)
                .map(|x| buf[(x, y)].symbol().chars().next().unwrap_or(' '))
                .collect::<String>()
        })
        .collect()
}

fn assert_frames_equal(ours: &[String], golden: &[String], label: &str) {
    assert_eq!(ours.len(), golden.len(), "{label}: row count");
    for (y, (a, b)) in ours.iter().zip(golden.iter()).enumerate() {
        assert_eq!(a, b, "{label}: row {y}\n  ours:   {a:?}\n  golden: {b:?}");
    }
}

/// A camera set exercising rotation, pan, zoom and roll (the TUI's golden
/// cases): `(label, camera)`.
fn cameras(m: &Model) -> Vec<(&'static str, Camera)> {
    let fit = auto_dist(m);
    vec![
        (
            "identity",
            Camera::new(world_rot(0.0, 0.0), fit, 0.0, 0.0, 0.0),
        ),
        (
            "rot-pan-zoom",
            Camera::new(world_rot(-22.9, 40.1), fit * 0.6, 0.0, 3.0, -2.0),
        ),
        ("roll", Camera::new(world_rot(0.0, 0.0), fit, 0.9, 0.0, 0.0)),
        (
            "all",
            Camera::new(world_rot(51.6, 68.8), fit * 1.3, -0.9, -1.5, 2.5),
        ),
    ]
}

/// The centered full-frame window (the TUI path).
#[test]
fn centered_window_matches_ratatui_canvas() {
    for (model, name) in [(cube(), "cube"), (tetra(), "tetra")] {
        for (cam_label, cam) in cameras(&model) {
            for (w, h) in [(40usize, 20usize), (17, 9), (60, 24)] {
                let (px_w, px_h) = (w * 2, h * 4);
                let f = focal(px_h as f64);
                let bounds = Bounds::centered(px_w, px_h);
                let ours = render_ours(&model, &cam, f, w, h, bounds);
                let golden = render_ratatui(&model, &cam, f, w as u16, h as u16, bounds);
                assert_frames_equal(
                    &ours,
                    &golden,
                    &format!("{name}/{cam_label}/{w}x{h} centered"),
                );
            }
        }
    }
}

/// Region-zoom windows (the CLI `--region` / `--fit content` path): the
/// window no longer matches the grid resolution, so the scale-and-round
/// mapping is exercised for real.
#[test]
fn region_window_matches_ratatui_canvas() {
    let regions: [[f64; 4]; 4] = [
        [0.25, 0.25, 0.75, 0.75], // symmetric crop
        [0.1, 0.0, 0.65, 0.9],    // asymmetric crop
        [0.0, 0.5, 1.0, 1.0],     // half frame
        [0.45, 0.45, 0.55, 0.55], // tiny centre window
    ];
    for (model, name) in [(cube(), "cube"), (tetra(), "tetra")] {
        for (cam_label, cam) in cameras(&model) {
            for (w, h) in [(40usize, 20usize), (13, 7)] {
                let (px_w, px_h) = (w * 2, h * 4);
                let f = focal(px_h as f64);
                for r in regions {
                    // The CLI's exact window derivation from normalized fractions.
                    let bounds = Bounds::from_arrays(
                        [
                            -(px_w as f64) / 2.0 + r[0] * px_w as f64,
                            -(px_w as f64) / 2.0 + r[2] * px_w as f64,
                        ],
                        [
                            (px_h as f64) / 2.0 - r[3] * px_h as f64,
                            (px_h as f64) / 2.0 - r[1] * px_h as f64,
                        ],
                    );
                    let ours = render_ours(&model, &cam, f, w, h, bounds);
                    let golden = render_ratatui(&model, &cam, f, w as u16, h as u16, bounds);
                    assert_frames_equal(
                        &ours,
                        &golden,
                        &format!("{name}/{cam_label}/{w}x{h} region {r:?}"),
                    );
                }
            }
        }
    }
}

/// An arbitrary window with no relation to the grid (neither centered nor
/// a clean fraction): `Bounds` must be honored as given.
#[test]
fn arbitrary_window_matches_ratatui_canvas() {
    let windows: [[f64; 4]; 3] = [
        [-137.0, 88.0, -41.5, 203.0], // off-center both axes
        [-3.0, 1000.0, -500.0, 12.0], // huge span
        [-16.0, -4.0, -8.0, -2.0],    // entirely negative quadrant
    ];
    let m = cube();
    for (cam_label, cam) in cameras(&m) {
        for (w, h) in [(40usize, 20usize), (23, 11)] {
            let f = focal((h * 4) as f64);
            for win in windows {
                let bounds = Bounds::from_arrays([win[0], win[1]], [win[2], win[3]]);
                let ours = render_ours(&m, &cam, f, w, h, bounds);
                let golden = render_ratatui(&m, &cam, f, w as u16, h as u16, bounds);
                assert_frames_equal(
                    &ours,
                    &golden,
                    &format!("cube/{cam_label}/{w}x{h} window {win:?}"),
                );
            }
        }
    }
}

/// Degenerate windows (what a `--width 0` / `--height 0` canvas derives)
/// and blank windows: both renderers must output pure spaces — never
/// U+2800.
#[test]
fn degenerate_and_blank_windows_yield_spaces() {
    let m = cube();
    let cam = Camera::new(world_rot(30.0, -45.0), auto_dist(&m), 0.0, 0.0, 0.0);
    let (w, h) = (40usize, 20usize);
    let f = focal((h * 4) as f64);
    for bounds in [
        Bounds::from_arrays([0.0, 0.0], [0.0, 0.0]),
        Bounds::from_arrays([-20.0, 20.0], [0.0, 0.0]),
        // Window far away from anything the camera projects.
        Bounds::from_arrays([4000.0, 5000.0], [4000.0, 5000.0]),
    ] {
        let ours = render_ours(&m, &cam, f, w, h, bounds);
        let golden = render_ratatui(&m, &cam, f, w as u16, h as u16, bounds);
        assert_frames_equal(&ours, &golden, &format!("degenerate {bounds:?}"));
        assert!(
            ours.iter().all(|row| row.chars().all(|c| c == ' ')),
            "expected an all-space frame, got {ours:?}"
        );
    }
}
