# Wireforge

A TUI-based viewer and live-editor for `.wrfm` files. 

It allows users to visualize, test, and create braille-based 3D wireframes which can be natively consumed by the [`ratatui-wireframe`](https://crates.io/crates/ratatui-wireframe) rendering crate.

## Features

* **Instant Hot-Reloading:** Open a `.wrfm` file in your favorite text editor (Neovim, VSCode, etc.) and run `wireforge` in an adjacent terminal pane. Every time you save the file, the 3D model instantly updates on screen.
* **Interactive 3D Viewport:** Freely rotate and inspect your wireframe models using your keyboard.
* **Zero-Dependency CPU Rendering:** Uses mathematical projection and braille characters to render 3D shapes in any standard terminal emulator.

## Installation

## Build from source:

```bash
git clone https://github.com/Vaishnav-Sabari-Girish/wireforge.git
cd wireforge
cargo build --release
```

## Usage

Point `wireforge` to any valid `.wrfm` file:

```bash
wireforge path/to/model.wrfm

# Example
wireforge cube.wrfm
```

### TUI Controls

| Key | Action |
| :--- | :--- |
| `Space` | Toggle automatic spinning |
| `↑` / `↓` | Rotate Pitch (X-axis) |
| `←` / `→` | Rotate Yaw (Y-axis) |
| `r` / `e` | Rotate Roll (Z-axis) |
| `q` / `Esc` | Quit the application |

## The `.wrfm` Format

The `.wrfm` format is a dead-simple, human-readable text format for defining 3D vertices and the edges that connect them. 

* `v <x> <y> <z>` defines a vertex in 3D space.
* `e <index1> <index2>` defines an edge connecting two vertices (0-indexed based on the order they appear).
* Lines starting with `#` are comments.

To view the example files you can view it in the `wfrm_files/` directory.

This is the simplest example of a Tetrahedron

```wrfm
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
