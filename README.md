# Wireforge

[![Crates.io](https://img.shields.io/crates/v/wireforge)](https://crates.io/crates/wireforge)

Wireforge is a TUI viewer for 3D wireframe models. Point it at a `.wrfm` file
and turn the model around with the keyboard. The repository also holds the
`.wrfm` file format, a Rust library that reads and writes it, and a `wrfm`
command line tool that converts, checks and edits models.

![Wireforge viewing a braille-rendered Utah teapot](assets/wireforge.png)

## Overview

`.wrfm` is a 3D model format containing a set of points and the lines between
them. The file opens with a magic line (`wrfm 2`) and a counts header, then
`v <x> <y> <z>` lines for the points and `e <a> <b>` lines for the edges. `#`
starts a comment; named groups are optional. A tetrahedron fits in twenty
lines.

Because it is plain text, the ordinary tools work on it: your editor edits it,
`diff` shows what changed, and a whole model fits in a code review.

The repository holds the viewer and the pieces around it:

- **`wireforge`** — the viewer, and the main program here. Open a file and
  turn it around with the keyboard; run it with no file and the empty space
  still comes up, XYZ axes and all.
- **`.wrfm`** — the format the viewer reads (v2; files from the old `wrfm`
  0.4.0 are not read).
- **[`wrfm`](crates/wrfm)** — a zero-dependency Rust library that parses and
  serializes that format, with `no_std` support and exact `f64` round-trips.
  The viewer and the CLI read models through it.
- **[`wrfm` CLI](crates/wrfm-cli)** — the command line tool, installed as
  `wrfm` from the `wrfm-cli` package. It has thirteen subcommands that check,
  verify, inspect, query, transform, edit, render, convert and diff models
  over plain stdio. The viewer does not need it.
- **[`wrfm-raster`](crates/wrfm-raster)** — the shared renderer: camera
  projection and braille rasterization used by the viewer and the CLI,
  byte-identical to ratatui's canvas.

## Features

- **Content, not extensions.** The viewer looks at the file contents, not the
  name: a model in a `.txt` opens as a model, OBJ content is rejected with a
  hint to run `wrfm convert`, and a broken model fails with the parser's
  line-and-column report on stderr.
- **Keyboard control.** Rotate, pan and zoom with `hjkl`, the arrow
  keys, `d`/`f` and `=`/`-`. `Ctrl` turns the model around its own axes instead
  of the world's, and `c` resets the view to the file's own framing. A telemetry
  line reports the camera, a statusline strip names the model and carries the
  everyday keys, and `?` opens a grouped reference to every binding.
- **Everything is a pipe.** The viewer reads stdin (`wireforge -`) and the CLI
  writes nothing to disk, so commands chain with pipes:

  ```bash
  wrfm convert mouse.obj | wireforge -
  wrfm edit model.wrfm --extract-group cabinet | wrfm transform - --scale 2 | wireforge -
  ```

- **CPU rendering.** Rendering runs on the CPU only, drawing braille cells, with
  no GPU, no GUI toolkit, and no display server. The event loop sleeps when
  nothing changes and redraws at full speed while you animate. Projection and
  rasterization switch to parallel (rayon) paths on large models.
- **Built for scripts.** Data goes to stdout, diagnostics go to stderr, and the
  exit code tells you how things are: ok, warn, broken, or no result at all. A
  CI job can assert on a model the same way it asserts on a test suite.

## Getting started

```bash
cargo install wireforge    # the viewer
cargo install wrfm-cli     # optional: the `wrfm` command line tool
```

It is also on the [AUR](https://aur.archlinux.org/packages/wireforge), and can
be built from source with `cargo build --release`. [SPEC.md](SPEC.md) walks
through installation, the viewer, its key bindings, and the format itself.

## Documentation

[SPEC.md](SPEC.md) — installation, viewer usage, the full key table, and the
`.wrfm` format.

[crates/wrfm-cli/README.md](crates/wrfm-cli/README.md) — the complete CLI
reference, covering every subcommand, recipe, and exit-code contract.

[crates/wrfm/README.md](crates/wrfm/README.md) — using the parser library and
the authoritative prose specification of the format.

[crates/wrfm-raster/README.md](crates/wrfm-raster/README.md) — the shared
camera projection and braille rasterization API.

[CHANGELOG.md](CHANGELOG.md) — release history.

## License

[MIT](LICENSE)
