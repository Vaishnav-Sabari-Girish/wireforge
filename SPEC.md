# Wireforge SPEC

How to install Wireforge, use the viewer, and read and write the `.wrfm`
format. See [README.md](README.md) for the rest of the project.

## Installation

### AUR

```bash
yay -S wireforge
# or
paru -S wireforge
```

### crates.io

```bash
cargo install wireforge
```

### Build from source

```bash
git clone https://github.com/lenitain/wireforge.git
cd wireforge
cargo build --release
```

The `wrfm` CLI ships as a separate package; see
[crates/wrfm-cli/README.md](crates/wrfm-cli/README.md) for its installation
and full command reference.

## The viewer

Point `wireforge` at any `.wrfm` file. The format is detected from the file's
content, so the extension does not matter:

```bash
wireforge path/to/model.wrfm

# example
wireforge cube.wrfm
```

The file is read once, when the viewer starts; run `wireforge` again after
editing it.

Run `wireforge` with no file at all and it opens the viewer with an empty
model: the XYZ axes are drawn at the origin, so rotating, panning and zooming
still work, and Row 0 reads `Wireforge: no file`.
A menu entry or launcher gets the same empty canvas if it runs the viewer in a
terminal and gives it nothing on stdin. If stdin is piped or redirected, a
bare `wireforge` reads the stream as a model instead
([Stream input](#stream-input)):

```bash
wireforge
```

### Stream input

`wireforge -` reads a model from stdin (or a FIFO such as `<( cat model.wrfm )`)
once, at start-up. Keyboard input still works via the controlling terminal.
A bare `wireforge` does the same whenever stdin is not a terminal, so a pipe
or a redirect is read as a model. The empty canvas appears only on a terminal,
where there is no stream to read.

```bash
cat model.wrfm | wireforge -
wireforge <( cat model.wrfm )
```

OBJ files are converted first with `wrfm convert` (part of
[`wrfm-cli`](crates/wrfm-cli/README.md)), then piped in like any other model:

```bash
wrfm convert mouse.obj | wireforge -
```

The result of a `wrfm-cli` transform can be viewed directly:

```bash
wrfm edit model.wrfm --extract-group cabinet | wrfm transform - --scale 2 | wireforge -
```

### Key bindings

| Key                             | Action                                                                         |
| :------------------------------ | :----------------------------------------------------------------------------- |
| `Space`                         | Toggle automatic spinning                                                      |
| `↑` / `↓`                       | Rotate Pitch (X-axis)                                                          |
| `←` / `→`                       | Rotate Yaw (Y-axis)                                                            |
| `h` / `j` / `k` / `l`           | Rotate Yaw / Pitch (same as `←` / `→` / `↑` / `↓`)                             |
| `r` / `e`                       | Rotate Roll (view axis)                                                        |
| `Ctrl` + `←` / `→` / `↑` / `↓`  | Rotate Yaw / Pitch around the model's own axes (local frame)                   |
| `Ctrl` + `h` / `j` / `k` / `l`  | Rotate Yaw / Pitch around the model's own axes (same as `Ctrl` + arrows)       |
| `Ctrl` + `r` / `e`              | Rotate Roll around the model's own (local) Z-axis                              |
| `Shift` + `←` / `→` / `↑` / `↓` | Move the model                                                                 |
| `Shift` + `h` / `j` / `k` / `l` | Move the model                                                                 |
| `=` / `-`                       | Move nearer / farther                                                          |
| `f`                             | Center the world origin (0,0,0) on screen                                      |
| `Shift` + `f`                   | Fit the model to the view                                                      |
| `0`                             | Reset rotation, pan and distance                                               |
| `?`                             | Toggle the key help overlay                                                    |
| `Tab` / `Shift` + `Tab`         | Toggle the XYZ axes                                                            |
| `q` / `Esc` / `Ctrl` + `C`      | Quit the application                                                           |

## The `.wrfm` Format

The `.wrfm` format (v2) is a plain text format for 3D vertices and the edges
that connect them.

- The first line is the magic and version: `wrfm 2`.
- The counts header comes next, after any blank lines and `#` comments:
  `vertices <N>   edges <M>` (the declared counts must match the lines that
  follow).
- `v <x> <y> <z>` defines a vertex in 3D space.
- `e <index1> <index2>` defines an edge connecting two vertices (0-indexed
  based on the order they appear).
- Lines starting with `#` are comments; `group <name>` opens a named
  section (optional).

**Example: `tetrahedron.wrfm`**

```text
wrfm 2
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

The authoritative prose specification lives with the library in
[crates/wrfm/README.md](crates/wrfm/README.md). It covers BOM and whitespace
rules, the `f64` round-trip guarantee, and the parser's error behaviour.

**Version history.** v2 is the current format. v1 was the format of `wrfm`
0.4.0: no magic line, no counts header, no groups, edge indices unchecked and
errors reported as plain strings. v1 files are not read: without the magic
line they fail as `MissingMagic`, and a file stamped with another version
fails as `UnsupportedVersion`. To migrate one, prepend `wrfm 2` and a
`vertices <N>   edges <M>` header with the file's actual counts. The
[crate README](crates/wrfm/README.md#format-versions) tabulates every
difference.
