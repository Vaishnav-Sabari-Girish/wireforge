# wrfm

A zero-dependency parser and serializer for the `.wrfm` 3D wireframe
format.

This crate provides a `WrfmModel` struct to load, manipulate, and save
models of vertices and edges. `wireforge` uses it, and the format itself is
consumed natively by
[`ratatui-wireframe`](https://crates.io/crates/ratatui-wireframe).

## The `.wrfm` Format

The `.wrfm` file type is a plain text format for defining 3D wireframes. A
v1 file is UTF-8 text with one element per line:

* `wrfm <version>` — magic line, required as the FIRST line of the file (a
  UTF-8 BOM is tolerated before it; nothing else may precede it). Version `1`
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
wrfm 1
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
    let input = "wrfm 1\nvertices 1   edges 0\n\nv 1.0 2.0 abc\n";

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
references to later-defined vertices are fine). The v1 magic, counts header
and declared counts are always enforced.

### Creating and Saving a Model

You can build a `WrfmModel` programmatically and serialize it back to disk.
`save_to_file` writes canonical v1, containing the magic line, a counts
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
