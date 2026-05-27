use std::fs::File;
use std::io::{self, BufRead, BufReader, Write};
use std::path::Path;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct WrfmModel {
    pub name: String,
    pub vertices: Vec<(f64, f64, f64)>,
    pub edges: Vec<(usize, usize)>,
}

impl WrfmModel {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            vertices: Vec::new(),
            edges: Vec::new(),
        }
    }

    // Load the .wrfm file
    pub fn from_file<P: AsRef<Path>>(path: P) -> io::Result<Self> {
        let file = File::open(&path)?;

        let reader = BufReader::new(file);

        let mut model = WrfmModel {
            name: path
                .as_ref()
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("Unknown")
                .to_string(),
            vertices: Vec::new(),
            edges: Vec::new(),
        };

        for (line_num, line_result) in reader.lines().enumerate() {
            let line = line_result?;
            let trimmed = line.trim();

            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }

            let mut parts = trimmed.split_whitespace();
            match parts.next() {
                Some("v") => {
                    // Parse vertices (x, y, and z)
                    let x = parts.next().and_then(|s| s.parse::<f64>().ok());
                    let y = parts.next().and_then(|s| s.parse::<f64>().ok());
                    let z = parts.next().and_then(|s| s.parse::<f64>().ok());

                    if let (Some(x), Some(y), Some(z)) = (x, y, z) {
                        model.vertices.push((x, y, z));
                    } else {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            format!("Invalid vertex on line {}", line_num + 1),
                        ));
                    }
                }
                Some("e") => {
                    // Parse edge (e v1 v2)
                    let v1 = parts.next().and_then(|s| s.parse::<usize>().ok());
                    let v2 = parts.next().and_then(|s| s.parse::<usize>().ok());

                    if let (Some(v1), Some(v2)) = (v1, v2) {
                        model.edges.push((v1, v2));
                    } else {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            format!("Invalid edge data on line {}", line_num + 1),
                        ));
                    }
                }

                _ => {
                    continue;
                }
            }
        }
        Ok(model)
    }

    pub fn save_to_file<P: AsRef<Path>>(&self, path: P) -> io::Result<()> {
        let mut file = File::create(path)?;

        writeln!(file, "# ComChan wireframe format")?;
        writeln!(file, "# Name: {}", self.name)?;
        writeln!(file)?;

        for (x, y, z) in &self.vertices {
            writeln!(file, "v {:.6} {:.6} {:.6}", x, y, z)?;
        }

        writeln!(file)?;

        for (v1, v2) in &self.edges {
            writeln!(file, "e {} {}", v1, v2)?;
        }

        Ok(())
    }
}
