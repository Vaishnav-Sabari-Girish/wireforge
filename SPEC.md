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
still work, and the statusline's pill reads `no file`.
A menu entry or launcher gets the same empty canvas if it runs the viewer in a
terminal and gives it nothing on stdin. If stdin is piped or redirected, a
bare `wireforge` reads the stream as a model instead
([Stream input](#stream-input)):

```bash
wireforge
```

### The screen

Three regions, top to bottom. Row 0 is the camera's telemetry: yaw, pitch,
roll, distance and pan, and nothing else — the model's name is not repeated
there. The middle is the model itself. The last row is a statusline strip,
which is where that name lives:

```text
 model.wrfm ▶ ● SPIN  ● AXES ▶        ◀ ? help  q quit  Space spin  Tab axes
```

The name sits on the accent colour, the two lamps report whether the model is
spinning and whether the axes are drawn (a lit dot means on), and the
right-hand hints carry the everyday chords. The strip fills the row edge to
edge, and the pill yields at most half of it so a long name can never crowd
out the rest. When the terminal is too narrow for all of it, hints are dropped
from the right, then the lamps, then the name is shortened with `…`. `?`
always opens the full list.

The hints also follow a held modifier, so the strip answers the question the
hand is already asking:

```text
 model.wrfm ▶ ● SPIN  ● AXES ▶        ◀ Ctrl+h/l/k/j/d/f turn its own axes
```

Press `Ctrl` and the everyday chords give way to the chords `Ctrl` unlocks;
let it go and they come back. This needs a terminal that reports the modifier
key on its own — the kitty keyboard protocol, which foot, kitty, wezterm and
ghostty speak. A terminal that does not report modifiers simply keeps the
everyday hints, so nothing is lost where the protocol is missing.

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

Bindings are grouped by what you are doing, not by which modifier you hold.
Chords are spelled `Ctrl+Shift+Key`; the arrow keys work wherever `hjkl` do.

The bindings are declared once, in one table in the source. The viewer's key
lookup is generated from that table by the compiler, and the statusline hints
read it directly — so a rebind moves the keyboard and the hints together. The
help overlay is not generated: it is written out by hand, and a test checks
every chord in the table against that page, so the two cannot drift apart
without the suite failing. The tables below are that same list written out for
reading.

#### Rotate — the world's axes

Left and right read as you see them: the model faces out of the screen, so
`h` sweeps its nose toward your left.

| Key                       | Action                              |
| :------------------------ | :---------------------------------- |
| `h` / `Left`              | Yaw left                            |
| `l` / `Right`             | Yaw right                           |
| `k` / `Up`                | Pitch up                            |
| `j` / `Down`              | Pitch down                          |
| `d`                       | Roll clockwise (view axis)          |
| `f`                       | Roll anticlockwise (view axis)      |

#### Move — the world's plane

| Key                            | Action                              |
| :----------------------------- | :---------------------------------- |
| `Shift+h` / `Shift+Left`       | Pan left                            |
| `Shift+l` / `Shift+Right`      | Pan right                           |
| `Shift+k` / `Shift+Up`         | Pan up                              |
| `Shift+j` / `Shift+Down`       | Pan down                            |
| `=` / `+`                      | Dolly nearer                        |
| `-` / `_`                      | Dolly farther                       |

#### Rotate — the model's own axes

Same turns, but around the model's own frame: `Ctrl+h` yaws it to *its* left.

| Key                              | Action                              |
| :------------------------------- | :---------------------------------- |
| `Ctrl+h` / `Ctrl+Left`           | Yaw to its own left                 |
| `Ctrl+l` / `Ctrl+Right`          | Yaw to its own right                |
| `Ctrl+k` / `Ctrl+Up`             | Pitch up                            |
| `Ctrl+j` / `Ctrl+Down`           | Pitch down                          |
| `Ctrl+d`                         | Roll clockwise                      |
| `Ctrl+f`                         | Roll anticlockwise                  |

#### Frame — where the model sits on screen

| Key        | Action                              |
| :--------- | :---------------------------------- |
| `c`        | Reset rotation, pan and distance    |

#### Session

| Key                       | Action                              |
| :------------------------ | :---------------------------------- |
| `Space`                   | Toggle automatic spinning           |
| `Tab` / `Shift+Tab`       | Toggle the XYZ axes                 |
| `?`                       | Toggle the key help overlay         |
| `q` / `Esc` / `Ctrl+c`    | Quit the application                |

The help overlay lists every binding, grouped the same way.

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
