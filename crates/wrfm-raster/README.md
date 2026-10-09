# wrfm-raster

Shared camera projection and braille rasterization for the wireforge
family: the `wireforge` TUI viewer and the `wrfm-cli` stream tool render the
same `.wrfm` models, so they share one implementation here instead of keeping
two drifting copies.

## Layers

| Module | Contents |
|---|---|
| `geometry` | 3D rotations (`rot_x/y/z`, Rodrigues `rot_axis`, `world_rot`), `bounds`, `model_extent`, `auto_dist`, `FOV_DEG` / `FIT_MARGIN` |
| `projection` | `Camera` (rotation, distance, roll, pan), `focal`, `project` / `project_full` / `project_into` / `project_all`, plus the `CameraF32` large-model path |
| `raster` | `Bounds` (centered or arbitrary window), `rasterize_line` (Cohen–Sutherland clip + Bresenham + braille cell), `braille_char`, `dots_to_lines` |

## Byte-identical to ratatui's Canvas

The rasterizer is a port of the algorithm behind ratatui's `Canvas` widget —
the same clipping (`line_clipping`'s Cohen–Sutherland), the same
scale-and-round mapping (`Painter::get_point`), the same Bresenham stepping —
and cells are encoded with ratatui's own braille pattern table. The golden
tests render with **both** implementations and assert byte-for-byte equality
for centered windows, region-zoom windows and arbitrary windows. That is what
lets the TUI and the CLI swap their canvas path for this crate without any
output changing.

## Example

```rust
use wrfm_raster::geometry::world_rot;
use wrfm_raster::projection::{focal, Camera};
use wrfm_raster::raster::{dots_to_lines, rasterize_line, Bounds};

let (cw, ch) = (20usize, 8usize);
let (px_w, px_h) = (cw * 2, ch * 4);

let cam = Camera::new(world_rot(0.0, 0.0), 8.0, 0.0, 0.0, 0.0);
let f = focal(px_h as f64);
let mut dots = vec![0u8; cw * ch];

if let (Some(a), Some(b)) = (
    cam.project((-10.0, -5.0, 0.0), f),
    cam.project((10.0, 5.0, 0.0), f),
) {
    rasterize_line(
        a.0,
        a.1,
        b.0,
        b.1,
        px_w,
        px_h,
        Bounds::centered(px_w, px_h),
        |cell, bit| dots[cell] |= bit,
    );
}

let lines = dots_to_lines(&dots, cw, ch);
```

## Scope

Deliberately **not** here: rayon parallelism (callers wrap `project_into` /
`rasterize_line` in their own parallel loops), the TUI's retained-mode screen
diff, and `wrfm-cli`'s ASCII/density output formats.

`unsafe_code` is forbidden in this crate.

## License

MIT (same as the rest of the repository).
