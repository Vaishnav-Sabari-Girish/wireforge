# wrfm

A fast, zero-dependency parser and serializer for the `.wrfm` 3D wireframe format. 

This crate provides a memory-efficient `WrfmModel` struct to load, manipulate, and save 3D models consisting of vertices and edges. It is designed to act as the core data backbone for tools like `wireforge` and terminal rendering engines like `ratatui-wireframe`.

## The `.wrfm` Format

The `.wrfm` file type is a minimalist, human-readable text format used to define 3D wireframes.

* `v <x> <y> <z>`: Defines a vertex in 3D space (floating point).
* `e <index1> <index2>`: Defines an edge connecting two vertices (0-indexed).
* Lines starting with `#` are safely ignored as comments.

### Example File (`cube.wrfm`)
```text
# ComChan wireframe format
# Name: cube

v -1.000000 -1.000000 -1.000000
v 1.000000 -1.000000 -1.000000
v 1.000000 1.000000 -1.000000
v -1.000000 1.000000 -1.000000

e 0 1
e 1 2
e 2 3
e 3 0
```

## Usage

Add `wrfm` to your `Cargo.toml`.

### Loading a Model
You can parse a model directly from a file path. The parser gracefully skips empty lines and comments.

```rust
use wrfm::WrfmModel;

fn main() -> std::io::Result<()> {
    // Loads the model and extracts the filename stem as the model name
    let model = WrfmModel::from_file("path/to/model.wrfm")?;
    
    println!("Loaded model: {}", model.name);
    println!("Vertices: {}", model.vertices.len());
    println!("Edges: {}", model.edges.len());
    
    Ok(())
}
```

### Creating and Saving a Model
You can build a `WrfmModel` programmatically and serialize it back to disk.

```rust
use wrfm::WrfmModel;

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
    
    // Save to disk
    model.save_to_file("triangle.wrfm")?;
    
    Ok(())
}
```
