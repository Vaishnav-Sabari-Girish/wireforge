//! Shared camera projection and braille rasterization for the wireforge
//! family — `wireforge` (the TUI viewer) and `wrfm-cli` (the stream tool)
//! both render the same `.wrfm` models, so they share the math and the
//! rasterizer here instead of keeping two drifting copies.
//!
//! # Layers
//!
//! * [`geometry`] — 3D rotations, bounding box, model extent, auto-fit distance
//! * [`projection`] — [`projection::Camera`] (world rotation, distance, roll,
//!   pan) and vertex projection to canvas coordinates
//! * [`raster`] — line clipping, Bresenham dot stepping, braille encoding
//!
//! # Byte-identical to ratatui
//!
//! The rasterizer is a port of the algorithm behind ratatui's
//! `Canvas` widget (Cohen–Sutherland clipping, the same scale-and-round
//! mapping, the same Bresenham stepping), and the cells are encoded with
//! ratatui's own braille pattern table. `tests/golden.rs` renders with both
//! and asserts the output is byte-for-byte identical, which is what keeps the
//! TUI's frames and the CLI's text output in lockstep.
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
pub mod projection;
pub mod raster;
