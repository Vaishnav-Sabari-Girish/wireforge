# Wireforge

A TUI-based viewer and live-editor for `.wrfm` files.

It allows users to visualize, test, and create braille-based 3D wireframes
which can be natively consumed by the
[`ratatui-wireframe`](https://crates.io/crates/ratatui-wireframe) rendering
crate.

## Features

* **Instant Hot-Reloading:** Open a `.wrfm` file in your favorite text editor
  (Neovim, VSCode, etc.) and run `wireforge` in an adjacent terminal pane.
  Every time the file changes on disk the 3D model instantly updates on
  screen. The file is watched by polling its modification time and length,
  so atomic-rename saves and file re-creation are caught too; a half-written
  or deleted file keeps the last good model on screen and recovers
  automatically. The camera is preserved across reloads.
* **Interactive 6-DOF Viewport:** Freely rotate (yaw / pitch / roll) and
  move (Shift + arrows or hjkl, plain `=` / `-`) the model with your
  keyboard, toggle auto-spin with `Space`, center with `f` or fit with
  `Shift + f`, and read the current camera from the HUD line. XYZ axes can
  be toggled with `Tab`.
* **Stream input:** `wireforge -` reads a model from stdin (or a FIFO such
  as `<( cat model.wrfm )`) as a one-shot preview with no hot-reload.
  Keyboard input still works via the controlling terminal.
* **Zero-Dependency CPU Rendering:** Uses mathematical projection and braille
  characters to render 3D shapes in any standard terminal emulator. The
  render loop is event-driven: it is fully idle (0% CPU) when nothing
  changes and redraws uncapped while you animate.

## Installation

### AUR

```bash
# Without 3D model support
yay -S wireforge
# OR 
paru -S wireforge
```

### crates.io

```bash
# Without 3D
cargo install wireforge
```

### Build from source

```bash
git clone https://github.com/Vaishnav-Sabari-Girish/wireforge.git
cd wireforge
cargo build --release
```

## Usage

Point `wireforge` to any `.wrfm` file (the format is detected from the file's
content, so the extension does not matter):

```bash
wireforge path/to/model.wrfm

# Example
wireforge cube.wrfm
```

OBJ files are converted first with `wrfm convert` (part of
[`wrfm-cli`](crates/wrfm-cli/README.md)), then piped in like any other model:

```bash
wrfm convert --from obj --to wrfm mouse.obj | wireforge -
```

Or pipe a model in for a one-shot preview (no hot-reload):

```bash
cat model.wrfm | wireforge -
wireforge <( cat model.wrfm )
```

You can also build a transform with `wrfm-cli` and view it directly:

```bash
wrfm edit model.wrfm --extract-group cabinet | wrfm transform - --scale 2 | wireforge -
```

### TUI Controls

| Key | Action |
| :--- | :--- |
| `Space` | Toggle automatic spinning |
| `↑` / `↓` | Rotate Pitch (X-axis) |
| `←` / `→` | Rotate Yaw (Y-axis) |
| `h` / `j` / `k` / `l` | Rotate Yaw / Pitch (same as `←` / `→` / `↑` / `↓`) |
| `r` / `e` | Rotate Roll (Z-axis) |
| `Shift` + `←` / `→` / `↑` / `↓` | Move the model |
| `Shift` + `h` / `j` / `k` / `l` | Move the model |
| `=` / `-` | Move nearer / farther |
| `f` | Center the file origin |
| `Shift` + `f` | Fit the model to the view |
| `0` | Reset rotation and distance |
| `?` | Toggle the key help overlay |
| `Tab` / `Shift` + `Tab` | Toggle the XYZ axes |
| `q` / `Esc` / `Ctrl` + `C` | Quit the application |

## The `.wrfm` Format

The `.wrfm` format (v1) is a dead-simple, human-readable text format for
defining 3D vertices and the edges that connect them.

* The first line is the magic and version: `wrfm 1`.
* The second line is a counts header: `vertices <N>   edges <M>` (the
  declared counts must match the lines that follow).
* `v <x> <y> <z>` defines a vertex in 3D space.
* `e <index1> <index2>` defines an edge connecting two vertices (0-indexed
  based on the order they appear).
* Lines starting with `#` are comments; `group <name>` opens a named
  section (optional).

**Example: `tetrahedron.wrfm`**

```text
wrfm 1
vertices 4   edges 6

# Name: Regular Tetrahedron
v 1.0 1.0 1.0
v 1.0 -1.0 -1.0
v -1.0 1.0 -1.0
v -1.0 -1.0 1.0

e 0 1
e 0 2
e 0 3
e 1 2
e 2 3
e 3 1
```
