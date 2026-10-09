//! 3D camera projection and braille rasterization shared by `wireforge` (TUI
//! viewer) and `wrfm-cli` (stream tool).
//!
//! The raster stage is a port of ratatui's `Canvas` algorithm — same
//! Cohen–Sutherland clipping, `Painter::get_point` scaling, Bresenham
//! stepping, and braille pattern table. `tests/golden.rs` renders with both
//! and asserts byte-for-byte equality across centered, region-zoom and
//! arbitrary windows.
//!
//! # Modules
//!
//! * [`model`] — the vertex/edge input every raster entry point takes
//! * [`geometry`] — 3D rotations, bounding box, model extent, auto-fit distance
//! * [`projection`] — [`projection::Camera`] (world rotation, distance, roll,
//!   pan) and vertex projection to canvas coordinates
//! * [`raster`] — line clipping, Bresenham dot stepping, braille encoding
//!
//! ```
//! use wrfm_raster::geometry::world_rot;
//! use wrfm_raster::projection::{focal, Camera};
//! use wrfm_raster::raster::{dots_to_lines, rasterize_line, Bounds};
//!
//! // A 20x8-cell canvas (40x32 dots).
//! let (cw, ch) = (20usize, 8usize);
//! let (px_w, px_h) = (cw * 2, ch * 4);
//!
//! let cam = Camera::new(world_rot(0.0, 0.0), 8.0, 0.0, 0.0, 0.0);
//! let f = focal(px_h as f64);
//! let mut dots = vec![0u8; cw * ch];
//!
//! // A line from (-10, -5) to (10, 5) in canvas coordinates.
//! let (p1, p2) = (cam.project((-10.0, -5.0, 0.0), f), cam.project((10.0, 5.0, 0.0), f));
//! if let (Some(a), Some(b)) = (p1, p2) {
//!     rasterize_line(
//!         a.0,
//!         a.1,
//!         b.0,
//!         b.1,
//!         px_w,
//!         px_h,
//!         Bounds::centered(px_w, px_h),
//!         |cell, bit| dots[cell] |= bit,
//!     );
//! }
//!
//! let lines = dots_to_lines(&dots, cw, ch);
//! assert_eq!(lines.len(), ch);
//! assert!(
//!     lines.iter().any(|l| l.chars().any(|c| c != ' ')),
//!     "the line lights dots"
//! );
//! ```

pub mod geometry;
pub mod model;
pub mod projection;
pub mod raster;

pub use model::Model;
