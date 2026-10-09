# wrfm

A zero-dependency parser and serializer for the `.wrfm` 3D wireframe
format.

This crate provides a `WrfmModel` struct to load, manipulate, and save
models of vertices and edges. It is the parser behind `wireforge` (the TUI
viewer) and `wrfm-cli` (the stream tool).

## Format versions

The format has two versions. **v2 is current**; v1 is the format of `wrfm`
0.4.0, which predates the version marker.

| | v1 (0.4.0) | v2 (current) |
| :--- | :--- | :--- |
| Version marker | — | `wrfm 2` magic line, required first |
| Counts header | — | `vertices <N>   edges <M>`, required and enforced |
| Groups | — (`group` lines are skipped) | `group <name>` sections |
| Edge indices | unvalidated (out-of-range indices parse silently) | checked against the vertex count |
| Unknown lines | skipped anywhere | blanks and comments only before the counts header; skipped after it |
| Parse failures | a plain `String` | structured `ParseError` with line and column |
| Writing | `# ComChan wireframe format` comment, `{:.6}` coordinates | canonical v2, shortest round-trip `f64` |

The version bump is not cosmetic. v1 could not tell a valid model from a
truncated or mangled one: it declared no counts and checked no indices, so a
half-written file quietly parsed into a different, broken model. v2 makes
the file self-describing and fails loudly instead.

Only v2 is read. A v1 file is recognizable by the absence of a marker: it
opens with `#` comments (0.4.0 writes `# ComChan wireframe format`) or
directly with `v` / `e` lines. Such a file is `MissingMagic`; a file stamped
with any other version (including `wrfm 1`) is `UnsupportedVersion`. To
migrate a v1 file, prepend the magic line and a counts header holding the
file's actual `v` / `e` totals:

```text
wrfm 2
vertices <N>   edges <M>
... (the v / e lines, unchanged)
```

## The `.wrfm` Format

The `.wrfm` file type is a plain text format for defining 3D wireframes. A
v2 file is UTF-8 text with one element per line:

* `wrfm <version>` — magic line, required as the FIRST line of the file (a
  UTF-8 BOM is tolerated before it; nothing else may precede it). Version `2`
  is the current format; other versions are rejected.
* `vertices <N>   edges <M>` — counts header, required before any content. A
  file whose actual `v` / `e` lines disagree with these counts is rejected.
* `v <x> <y> <z>` — a vertex in 3D space (full f64 precision).
* `e <index1> <index2>` — an edge connecting two vertices (0-indexed, global).
* `group <name>` — opens a named section; the following `v` lines belong to
  it until the next `group`. Groups are optional and organize the global
  vertex list.
* Lines starting with `#` are safely ignored as comments (except before the
  magic line).

### Example File (`cube.wrfm`)

```text
wrfm 2
vertices 4   edges 4

group bottom
  v -1 -1 -1
  v 1 -1 -1
  v 1 1 -1
  v -1 1 -1
  e 0 1
  e 1 2
  e 2 3
  e 3 0
```

Coordinates are parsed and serialized at full `f64` precision with the
shortest round-trip representation, so `1.0 / 3.0` round-trips as
`0.3333333333333333` and is never quantized.

## Usage

Add `wrfm` to your `Cargo.toml`.

### Loading a Model

You can parse a model directly from a file path. The parser skips empty
lines and comments. `from_file` returns a `LoadError`, which is either an
I/O error or a structured `ParseError`:

```rust
use wrfm::{LoadError, WrfmModel};

fn main() -> Result<(), LoadError> {
    // Loads the model and extracts the filename stem as the model name
    let model = WrfmModel::from_file("path/to/model.wrfm")?;

    println!("Loaded model: {}", model.name);
    println!("Vertices: {}", model.vertices.len());
    println!("Edges: {}", model.edges.len());
    println!("Version: {}", model.version);

    Ok(())
}
```

### Handling Parse Errors

`from_str` returns a classified, positioned `ParseError`.
Render it with `Display` for a rustc-style report (line/column header, source
line, caret, expected-vs-found), or inspect the structured fields:

```rust
use wrfm::{ParseError, WrfmModel};

fn main() {
    let input = "wrfm 2\nvertices 1   edges 0\n\nv 1.0 2.0 abc\n";

    match WrfmModel::from_str("model", input) {
        Ok(model) => println!("{} vertices", model.vertices.len()),
        Err(err) => {
            // Positioned accessors + the classified variant.
            eprintln!("line {}: {}", err.line(), err);
            match err {
                ParseError::InvalidVertex { detail, .. } => eprintln!("bad vertex: {detail:?}"),
                ParseError::InvalidEdge { detail, .. } => eprintln!("bad edge: {detail:?}"),
            }
        }
    }
}
```

Parsing skips unknown lines silently. Note that out-of-range edge indices are
never tolerated (validated against the final vertex count, so forward
references to later-defined vertices are fine). The v2 magic, counts header
and declared counts are always enforced.

### Creating and Saving a Model

You can build a `WrfmModel` programmatically and serialize it back to disk.
`save_to_file` writes canonical v2, containing the magic line, a counts
header, optional `group` sections (in order) and then all edges.

```rust
use wrfm::{Group, WrfmModel};

fn main() -> std::io::Result<()> {
    let mut model = WrfmModel::new("triangle");

    // Add vertices
    model.vertices.push((0.0, 1.0, 0.0));
    model.vertices.push((-1.0, -1.0, 0.0));
    model.vertices.push((1.0, -1.0, 0.0));

    // Connect them
    model.edges.push((0, 1));
    model.edges.push((1, 2));
    model.edges.push((2, 0));

    // Name the section (optional)
    model.groups.push(Group {
        name: "main".to_string(),
        vertex_start: 0,
        vertex_end: 3,
    });

    // Save to disk
    model.save_to_file("triangle.wrfm")?;

    Ok(())
}
```
