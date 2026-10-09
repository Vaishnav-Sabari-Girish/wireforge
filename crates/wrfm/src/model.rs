use alloc::string::String;
use alloc::vec::Vec;

#[cfg(feature = "std")]
use crate::error::LoadError;
#[cfg(feature = "std")]
use core::fmt;
#[cfg(feature = "std")]
use std::fs::File;
#[cfg(feature = "std")]
use std::io::Write;
#[cfg(feature = "std")]
use std::path::Path;

/// A parsed `.wrfm` model (v1): a display `name`, a list of
/// `vertices`, 0-based `edges`, the format `version` and optional `groups`.
///
/// ```
/// use wrfm::WrfmModel;
///
/// let mut model = WrfmModel::new("triangle");
/// model.vertices.push((0.0, 1.0, 0.0));
/// model.edges.push((0, 1));
/// assert_eq!(model.name, "triangle");
/// assert_eq!(model.version, 1);
/// assert_eq!(model.vertices.len(), 1);
/// assert_eq!(model.edges, vec![(0, 1)]);
/// assert!(model.groups.is_empty());
/// ```
#[derive(Debug, Clone, PartialEq)]
pub struct WrfmModel {
    pub name: String,
    pub vertices: Vec<(f64, f64, f64)>,
    pub edges: Vec<(usize, usize)>,
    /// Format version parsed from the magic line (`1` for v1).
    pub version: u32,
    /// Named sections over the global vertex list (empty when the file has no `group` lines).
    pub groups: Vec<Group>,
}

impl Default for WrfmModel {
    /// `version` defaults to 1 (never 0), `groups` to empty.
    ///
    /// ```
    /// use wrfm::WrfmModel;
    ///
    /// let m = WrfmModel::default();
    /// assert_eq!(m.version, 1); // never 0
    /// assert_eq!(m.name, "");
    /// assert!(m.vertices.is_empty());
    /// assert!(m.edges.is_empty());
    /// assert!(m.groups.is_empty());
    /// ```
    fn default() -> Self {
        Self {
            name: String::new(),
            vertices: Vec::new(),
            edges: Vec::new(),
            version: 1,
            groups: Vec::new(),
        }
    }
}

impl WrfmModel {
    /// Create an empty v1 model with the given display `name`.
    ///
    /// ```
    /// use wrfm::WrfmModel;
    ///
    /// let m = WrfmModel::new("cube");
    /// assert_eq!(m.name, "cube");
    /// assert_eq!(m.version, 1);
    /// assert!(m.vertices.is_empty());
    /// assert!(m.edges.is_empty());
    /// assert!(m.groups.is_empty());
    /// ```
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            vertices: Vec::new(),
            edges: Vec::new(),
            version: 1,
            groups: Vec::new(),
        }
    }

    /// Load the `.wrfm` file from a path; the model name is the file stem.
    /// Returns a [`LoadError`] for I/O failures or parse errors.
    ///
    /// ```
    /// use wrfm::WrfmModel;
    ///
    /// # let dir = std::env::temp_dir().join(format!("wrfm-doc-{}", std::process::id()));
    /// # std::fs::create_dir_all(&dir).unwrap();
    /// # let path = dir.join("model.wrfm");
    /// # std::fs::write(&path, "wrfm 1\nvertices 2   edges 1\n\nv 0 0 0\nv 1 1 1\n\ne 0 1\n").unwrap();
    ///
    /// let model = WrfmModel::from_file(&path).unwrap();
    /// assert_eq!(model.name, "model"); // the file stem
    /// assert_eq!(model.vertices.len(), 2);
    /// # let _ = std::fs::remove_dir_all(&dir);
    /// ```
    #[cfg(feature = "std")]
    pub fn from_file<P: AsRef<Path>>(path: P) -> Result<Self, LoadError> {
        let content = std::fs::read_to_string(path.as_ref()).map_err(LoadError::Io)?;
        let name = path
            .as_ref()
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("Unknown");
        Self::from_str(name, &content).map_err(LoadError::Parse)
    }

    /// Save the model to a `.wrfm` file in canonical v1.
    /// `wrfm 1`, a counts header, optional `group` sections and then all
    /// edges. Coordinates use the shortest round-trip representation.
    ///
    /// ```
    /// use wrfm::{Group, WrfmModel};
    ///
    /// let mut model = WrfmModel::new("triangle");
    /// model.vertices.push((0.0, 0.0, 0.0));
    /// model.vertices.push((1.0, 0.0, 0.0));
    /// model.edges.push((0, 1));
    /// model.groups.push(Group {
    ///     name: "base".to_string(),
    /// vertex_start: 0,
    /// vertex_end: 2,
    /// });
    /// # let dir = std::env::temp_dir().join(format!("wrfm-doc-save-{}", std::process::id()));
    /// # std::fs::create_dir_all(&dir).unwrap();
    /// # let path = dir.join("tri.wrfm");
    ///
    /// model.save_to_file(&path).unwrap();
    /// let back = WrfmModel::from_file(&path).unwrap();
    /// assert_eq!(back.edges, vec![(0, 1)]);
    /// assert_eq!(back.groups, model.groups); // groups round-trip
    /// # let _ = std::fs::remove_dir_all(&dir);
    /// ```
    #[cfg(feature = "std")]
    pub fn save_to_file<P: AsRef<Path>>(&self, path: P) -> std::io::Result<()> {
        let mut file = File::create(path)?;
        self.write_to(&mut file)
    }

    /// Serialize the canonical v1 text into `w`.
    #[cfg(feature = "std")]
    fn write_to<W: Write>(&self, w: &mut W) -> std::io::Result<()> {
        writeln!(w, "wrfm {}", self.version)?;
        writeln!(
            w,
            "vertices {}   edges {}",
            self.vertices.len(),
            self.edges.len()
        )?;
        writeln!(w)?;

        let n = self.vertices.len();
        if self.groups.is_empty() {
            for (x, y, z) in &self.vertices {
                writeln!(w, "v {} {} {}", x, y, z)?;
            }
        } else {
            // Vertices before the first group (or between groups) form the
            // implicit unnamed section and are written without a `group`
            // line, in global order.
            let mut cursor = 0;
            for g in &self.groups {
                let gs = g.vertex_start.min(n);
                let ge = g.vertex_end.min(n);
                for (x, y, z) in &self.vertices[cursor..gs] {
                    writeln!(w, "v {} {} {}", x, y, z)?;
                }
                writeln!(w, "group {}", g.name)?;
                if ge > gs {
                    for (x, y, z) in &self.vertices[gs..ge] {
                        writeln!(w, "  v {} {} {}", x, y, z)?;
                    }
                }
                cursor = ge.max(gs);
            }
            for (x, y, z) in &self.vertices[cursor..] {
                writeln!(w, "v {} {} {}", x, y, z)?;
            }
        }

        writeln!(w)?;
        for (a, b) in &self.edges {
            writeln!(w, "e {} {}", a, b)?;
        }
        Ok(())
    }
}

/// Serialize the canonical v1 text as a `String` — the
/// streaming, file-less form of [`WrfmModel::save_to_file`], used by CLI pipelines
/// (`wrfm transform a --scale 2 > big.wrfm`). Same canonical output:
/// `wrfm <version>`, counts header, optional `group` sections, edges,
/// and shortest-round-trip coordinates.
///
/// ```
/// use wrfm::WrfmModel;
///
/// let mut model = WrfmModel::new("tri");
/// model.vertices.push((0.0, 0.0, 0.0));
/// model.edges.push((0, 0));
/// let text = model.to_string();
/// assert!(text.starts_with("wrfm 1\nvertices 1   edges 1\n"));
/// ```
#[cfg(feature = "std")]
impl fmt::Display for WrfmModel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Vec<u8> implements io::Write; serialization cannot fail.
        let mut buf: Vec<u8> = Vec::new();
        self.write_to(&mut buf)
            .expect("wrfm serialization into a Vec<u8> cannot fail");
        f.write_str(&String::from_utf8(buf).expect("canonical v1 text is always valid UTF-8"))
    }
}

/// A named section of the global vertex list (`vertex_start..vertex_end` is half-open).
///
/// `vertex_start..vertex_end` is a half-open range over the model's global
/// `vertices`; edges are never tracked per group.
///
/// ```
/// use wrfm::{Group, WrfmModel};
///
/// let model = WrfmModel::from_str(
///     "m",
///     "wrfm 1\nvertices 3   edges 0\n\ngroup body\n  v 0 0 0\n  v 1 0 0\n  v 1 1 0\n",
/// )
/// .unwrap();
/// assert_eq!(
/// model.groups,
/// vec![Group {
///         name: "body".to_string(),
/// vertex_start: 0,
/// vertex_end: 3,
/// }]
/// );
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Group {
    /// The group's single-token name.
    pub name: String,
    /// First global vertex index in the group.
    pub vertex_start: usize,
    /// One past the last global vertex index in the group.
    pub vertex_end: usize,
}
