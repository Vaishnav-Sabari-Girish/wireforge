#![cfg_attr(not(feature = "std"), no_std)]

extern crate alloc;

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::fmt;

#[cfg(feature = "std")]
use std::fs::File;
#[cfg(feature = "std")]
use std::io::Write;
#[cfg(feature = "std")]
use std::path::Path;

/// A parsed `.wrfm` model: a name, vertices, 0-based edges, format version and optional groups.
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
    /// `version` defaults to 1 (never 0) and `groups` to empty.
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
    /// Create an empty model with the given display name.
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

    /// Parse a model directly from a string (useful for include_str!).
    /// Parses a model directly from a string (useful for include_str!).
    ///
    /// The input must be v1: a `wrfm 1` magic line, a
    /// `vertices <N> edges <M>` counts header, then `v` / `e` / `group`
    /// lines. Lenient by default: blank lines, `#` comments and unknown
    /// lines are skipped. Returns a structured [`ParseError`] on malformed
    /// input.
    ///
    /// ```
    /// use wrfm::WrfmModel;
    ///
    /// let model = WrfmModel::from_str(
    ///     "cube",
    ///     "wrfm 1\nvertices 2   edges 1\n\nv 0 0 0\nv 1 1 1\n\ne 0 1\n",
    /// )
    /// .unwrap();
    /// assert_eq!(model.name, "cube");
    /// assert_eq!(model.version, 1);
    /// assert_eq!(model.vertices.len(), 2);
    /// assert_eq!(model.edges, vec![(0, 1)]);
    /// ```
    pub fn from_str(name: &str, input: &str) -> Result<Self, ParseError> {
        Self::parse_with(name, input, false)
    }

    /// Parse with `strict` controlling whether unknown lines are rejected; the magic, header and counts are always enforced.
    /// Parses a model with `strict` controlling whether unknown lines are
    /// rejected (`UnknownDirective`) or silently skipped.
    ///
    /// The v1 magic, header and declared counts are REQUIRED in both modes —
    /// they are structural, not a policy . Out-of-range edge
    /// indices are never tolerated, regardless of `strict`.
    ///
    /// ```
    /// use wrfm::WrfmModel;
    ///
    /// // Lenient: unknown lines are skipped.
    /// let lenient =
    ///     WrfmModel::from_str("m", "wrfm 1\nvertices 1   edges 0\n\ngarbage\nv 0 0 0\n")
    /// .unwrap();
    /// assert_eq!(lenient.vertices.len(), 1);
    ///
    /// // Strict: unknown lines are an error.
    /// assert!(
    ///     WrfmModel::parse_with("m", "wrfm 1\nvertices 1   edges 0\n\ngarbage\nv 0 0 0\n", true)
    /// .is_err()
    /// );
    /// ```
    pub fn parse_with(name: &str, input: &str, strict: bool) -> Result<Self, ParseError> {
        let input = input.strip_prefix('\u{feff}').unwrap_or(input);
        let mut model = WrfmModel {
            name: name.to_string(),
            vertices: Vec::new(),
            edges: Vec::new(),
            version: 1,
            groups: Vec::new(),
        };
        // Source location of each parsed edge, needed to report OutOfRange.
        let mut edge_meta: Vec<(usize, usize, usize, String)> = Vec::new();

        let mut lines = input.lines();

        // Magic: line 1 must be `wrfm <version>`.
        let first_line = lines.next().unwrap_or("");
        let first_toks = tokenize(first_line);
        let Some(&(first_token, first_col)) = first_toks.first() else {
            return Err(ParseError::MissingMagic {
                line: 1,
                column: 1,
                line_text: first_line.to_string(),
            });
        };
        if first_token != "wrfm" {
            return Err(ParseError::MissingMagic {
                line: 1,
                column: first_col,
                line_text: first_line.to_string(),
            });
        }
        match first_toks.get(1) {
            Some(&(version_tok, version_col)) => match version_tok.parse::<u32>() {
                Ok(1) => model.version = 1,
                Ok(v) => {
                    return Err(ParseError::UnsupportedVersion {
                        line: 1,
                        column: version_col,
                        line_text: first_line.to_string(),
                        version: v,
                        token: version_tok.to_string(),
                    });
                }
                Err(_) => {
                    return Err(ParseError::MalformedMagic {
                        line: 1,
                        column: version_col,
                        line_text: first_line.to_string(),
                        token: version_tok.to_string(),
                    });
                }
            },
            None => {
                return Err(ParseError::MalformedMagic {
                    line: 1,
                    column: missing_value_column(&first_toks),
                    line_text: first_line.to_string(),
                    token: String::new(),
                });
            }
        }

        // Header + content.
        let mut declared_vertices = 0usize;
        let mut declared_edges = 0usize;
        let mut header_line = 0usize;
        let mut header_col = 1usize;
        let mut header_text = String::new();
        let mut seen_header = false;

        // `current_group` is the section being authored; `open_group` is the
        // `Group` being built — `None` until the first `v` inside the
        // section, so empty groups produce no entry.
        let mut current_group: Option<String> = None;
        let mut open_group: Option<Group> = None;

        for (idx, raw_line) in lines.enumerate() {
            let line = idx + 2; // the magic occupied line 1
            let trimmed = raw_line.trim();

            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }

            let toks = tokenize(raw_line);
            if toks.is_empty() {
                continue;
            }

            let (directive, dcol) = toks[0];

            if !seen_header {
                if directive != "vertices" {
                    return Err(ParseError::MissingHeader {
                        line,
                        column: dcol,
                        line_text: raw_line.to_string(),
                    });
                }
                // `vertices <N> edges <M>` — a trailing `# comment` on the
                // header line is ignored.
                let cut = toks
                    .iter()
                    .position(|&(t, _)| t == "#")
                    .unwrap_or(toks.len());
                let header_toks = &toks[..cut];
                let header_ok = header_toks.len() == 4
                    && header_toks[1].0.parse::<usize>().is_ok()
                    && header_toks[2].0 == "edges"
                    && header_toks[3].0.parse::<usize>().is_ok();
                if !header_ok {
                    let (column, token) = malformed_header_location(header_toks);
                    return Err(ParseError::MalformedHeader {
                        line,
                        column,
                        line_text: raw_line.to_string(),
                        token,
                    });
                }
                declared_vertices = header_toks[1].0.parse::<usize>().expect("validated above");
                declared_edges = header_toks[3].0.parse::<usize>().expect("validated above");
                header_line = line;
                header_col = dcol;
                header_text = raw_line.to_string();
                seen_header = true;
                continue;
            }

            match directive {
                "v" => {
                    // Open the group's vertex range on its first `v` line.
                    if let Some(name) = &current_group
                        && open_group.is_none()
                    {
                        open_group = Some(Group {
                            name: name.clone(),
                            vertex_start: model.vertices.len(),
                            vertex_end: 0,
                        });
                    }
                    // toks[0] is the directive; toks[1..] are x, y, z.
                    let axes = ['x', 'y', 'z'];
                    let mut coords = [0.0f64; 3];
                    for (i, axis) in axes.iter().enumerate() {
                        match toks.get(i + 1) {
                            Some(&(tok, col)) => match tok.parse::<f64>() {
                                Ok(v) => coords[i] = v,
                                Err(_) => {
                                    return Err(ParseError::InvalidVertex {
                                        line,
                                        column: col,
                                        line_text: raw_line.to_string(),
                                        detail: VertexError::NotANumber {
                                            token: tok.to_string(),
                                            axis: *axis,
                                        },
                                    });
                                }
                            },
                            None => {
                                // Report the actually missing axis: 'x' when
                                // the line is only `v`, 'y' when x is present,
                                // 'z' when x and y are present.
                                let axis = match i {
                                    0 => 'x',
                                    1 => 'y',
                                    _ => 'z',
                                };
                                let column = missing_value_column(&toks);
                                return Err(ParseError::InvalidVertex {
                                    line,
                                    column,
                                    line_text: raw_line.to_string(),
                                    detail: VertexError::MissingCoordinate { axis },
                                });
                            }
                        }
                    }
                    model.vertices.push((coords[0], coords[1], coords[2]));
                    if model.vertices.len() > declared_vertices {
                        return Err(ParseError::CountMismatch {
                            line,
                            column: dcol,
                            line_text: raw_line.to_string(),
                            which: CountKind::Vertices,
                            declared: declared_vertices,
                            actual: model.vertices.len(),
                        });
                    }
                }
                "e" => {
                    let mut indices = [0usize; 2];
                    let mut cols = [1usize; 2];
                    for (i, which) in [1u8, 2u8].iter().enumerate() {
                        match toks.get(i + 1) {
                            Some(&(tok, col)) => {
                                cols[i] = col;
                                // A negative literal (e.g. `-1`) is a
                                // NegativeIndex, not a generic NotAnIndex.
                                if let Some(rest) = tok.strip_prefix('-')
                                    && !rest.is_empty()
                                    && rest.bytes().all(|b| b.is_ascii_digit())
                                {
                                    let value: i64 = rest.parse().unwrap_or(0);
                                    return Err(ParseError::InvalidEdge {
                                        line,
                                        column: col,
                                        line_text: raw_line.to_string(),
                                        detail: EdgeError::NegativeIndex {
                                            index: -value,
                                            which: *which,
                                        },
                                    });
                                }
                                match tok.parse::<usize>() {
                                    Ok(v) => indices[i] = v,
                                    Err(_) => {
                                        return Err(ParseError::InvalidEdge {
                                            line,
                                            column: col,
                                            line_text: raw_line.to_string(),
                                            detail: EdgeError::NotAnIndex {
                                                token: tok.to_string(),
                                                which: *which,
                                            },
                                        });
                                    }
                                }
                            }
                            None => {
                                let column = missing_value_column(&toks);
                                return Err(ParseError::InvalidEdge {
                                    line,
                                    column,
                                    line_text: raw_line.to_string(),
                                    detail: EdgeError::MissingIndex { which: *which },
                                });
                            }
                        }
                    }
                    model.edges.push((indices[0], indices[1]));
                    edge_meta.push((line, cols[0], cols[1], raw_line.to_string()));
                    if model.edges.len() > declared_edges {
                        return Err(ParseError::CountMismatch {
                            line,
                            column: dcol,
                            line_text: raw_line.to_string(),
                            which: CountKind::Edges,
                            declared: declared_edges,
                            actual: model.edges.len(),
                        });
                    }
                }
                "group" => {
                    // Close the previous section (if it has vertices).
                    if let Some(mut g) = open_group.take() {
                        g.vertex_end = model.vertices.len();
                        model.groups.push(g);
                    }
                    match toks.get(1) {
                        Some(&(name_tok, _)) => current_group = Some(name_tok.to_string()),
                        None => {
                            return Err(ParseError::MissingGroupName {
                                line,
                                column: missing_value_column(&toks),
                                line_text: raw_line.to_string(),
                            });
                        }
                    }
                }
                other => {
                    if strict {
                        return Err(ParseError::UnknownDirective {
                            line,
                            column: 1,
                            line_text: raw_line.to_string(),
                            token: other.to_string(),
                        });
                    }
                    // Lenient: unknown lines are skipped silently.
                }
            }
        }

        // Close a trailing section that has vertices.
        if let Some(mut g) = open_group.take() {
            g.vertex_end = model.vertices.len();
            model.groups.push(g);
        }

        // Declared vs actual counts.
        if model.vertices.len() != declared_vertices {
            return Err(ParseError::CountMismatch {
                line: header_line,
                column: header_col,
                line_text: header_text.clone(),
                which: CountKind::Vertices,
                declared: declared_vertices,
                actual: model.vertices.len(),
            });
        }
        if model.edges.len() != declared_edges {
            return Err(ParseError::CountMismatch {
                line: header_line,
                column: header_col,
                line_text: header_text,
                which: CountKind::Edges,
                declared: declared_edges,
                actual: model.edges.len(),
            });
        }

        // Validate edge indices against the final vertex count, so forward
        // references to later-defined vertices are legal.
        let vertex_count = model.vertices.len();
        for (i, &(a, b)) in model.edges.iter().enumerate() {
            let (edge_line, col_a, col_b, line_text) = &edge_meta[i];
            if a >= vertex_count {
                return Err(ParseError::InvalidEdge {
                    line: *edge_line,
                    column: *col_a,
                    line_text: line_text.clone(),
                    detail: EdgeError::OutOfRange {
                        index: a,
                        vertex_count,
                        which: 1,
                    },
                });
            }
            if b >= vertex_count {
                return Err(ParseError::InvalidEdge {
                    line: *edge_line,
                    column: *col_b,
                    line_text: line_text.clone(),
                    detail: EdgeError::OutOfRange {
                        index: b,
                        vertex_count,
                        which: 2,
                    },
                });
            }
        }

        Ok(model)
    }

    /// Load the `.wrfm` file from a path; the model name is the file stem.
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
    /// Save the model to a `.wrfm` file in canonical v1 :
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
/// streaming, file-less form of [`save_to_file`], used by CLI pipelines
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
/// A named section of the global vertex list .
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

/// Which declared count (vertices or edges) disagreed with the parsed content.
/// Which declared count disagreed with the parsed content
/// ([`ParseError::CountMismatch`]).
///
/// ```
/// use wrfm::{CountKind, ParseError, WrfmModel};
///
/// let err = WrfmModel::from_str(
///     "m",
///     "wrfm 1\nvertices 5   edges 0\n\nv 0 0 0\nv 1 1 1\n",
/// )
/// .unwrap_err();
/// match err {
/// ParseError::CountMismatch { which, declared, actual, .. } => {
/// assert_eq!(which, CountKind::Vertices);
/// assert_eq!((declared, actual), (5, 2));
/// }
///     other => panic!("unexpected error: {other:?}"),
/// }
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CountKind {
    /// The header's vertex count disagreed with the parsed `v` lines.
    Vertices,
    /// The header's edge count disagreed with the parsed `e` lines.
    Edges,
}

/// A structured parse error with exact position, source context and a classified detail.
/// A structured parse error with exact position, source context and a
/// classified detail. `no_std`-safe (core + alloc only).
///
/// ```
/// use wrfm::{ParseError, WrfmModel};
///
/// let err = WrfmModel::from_str(
///     "m",
///     "wrfm 1\nvertices 1   edges 0\n\nv 1.0 2.0 abc\n",
/// )
/// .unwrap_err();
/// match err {
/// ParseError::InvalidVertex { line, column, .. } => {
/// assert_eq!((line, column), (4, 11));
/// }
///     other => panic!("unexpected error: {other:?}"),
/// }
/// ```
#[derive(Debug, Clone, PartialEq)]
pub enum ParseError {
    /// A malformed `v <x> <y> <z>` vertex line.
    InvalidVertex {
        /// 1-based line number of the offending line.
        line: usize,
        /// 1-based column of the offending token (or of the first missing
        /// position at end of line).
        column: usize,
        /// The raw offending line (untrimmed).
        line_text: String,
        detail: VertexError,
    },
    /// A malformed `e <v1> <v2>` edge line.
    InvalidEdge {
        line: usize,
        column: usize,
        line_text: String,
        detail: EdgeError,
    },
    /// An unrecognized directive (only reported in strict mode).
    UnknownDirective {
        line: usize,
        column: usize,
        line_text: String,
        /// The directive word (first whitespace-separated token).
        token: String,
    },
    /// Line 1 is not `wrfm <version>` — the file is not a wrfm file
    ///.
    MissingMagic {
        line: usize,
        column: usize,
        line_text: String,
    },
    /// Line 1 starts with `wrfm` but the version token is missing or not a
    /// positive integer.
    MalformedMagic {
        line: usize,
        column: usize,
        line_text: String,
        /// The offending version token (empty when it is missing entirely).
        token: String,
    },
    /// Line 1 declares a format version this crate does not implement.
    UnsupportedVersion {
        line: usize,
        column: usize,
        line_text: String,
        /// The declared version.
        version: u32,
        /// The version token as written.
        token: String,
    },
    /// No `vertices <N> edges <M>` header before the first content line
    ///.
    MissingHeader {
        line: usize,
        column: usize,
        line_text: String,
    },
    /// The counts header line is present but malformed.
    MalformedHeader {
        line: usize,
        column: usize,
        line_text: String,
        /// The offending token (empty when the header is merely incomplete).
        token: String,
    },
    /// The declared vertex or edge count disagrees with the parsed content
    ///.
    CountMismatch {
        line: usize,
        column: usize,
        line_text: String,
        /// Which count disagreed.
        which: CountKind,
        /// The count declared in the header.
        declared: usize,
        /// The count actually parsed.
        actual: usize,
    },
    /// A `group` line without a name.
    MissingGroupName {
        line: usize,
        column: usize,
        line_text: String,
    },
}

impl ParseError {
    /// 1-based line number of the error.
    /// 1-based line number of the error.
    ///
    /// ```
    /// use wrfm::WrfmModel;
    ///
    /// let err = WrfmModel::from_str(
    ///     "m",
    ///     "wrfm 1\nvertices 1   edges 0\n\nv 1.0 2.0 abc\n",
    /// )
    /// .unwrap_err();
    /// assert_eq!(err.line(), 4);
    /// ```
    pub fn line(&self) -> usize {
        match self {
            ParseError::InvalidVertex { line, .. }
            | ParseError::InvalidEdge { line, .. }
            | ParseError::UnknownDirective { line, .. }
            | ParseError::MissingMagic { line, .. }
            | ParseError::MalformedMagic { line, .. }
            | ParseError::UnsupportedVersion { line, .. }
            | ParseError::MissingHeader { line, .. }
            | ParseError::MalformedHeader { line, .. }
            | ParseError::CountMismatch { line, .. }
            | ParseError::MissingGroupName { line, .. } => *line,
        }
    }

    /// 1-based column of the error.
    /// 1-based column of the error.
    ///
    /// ```
    /// use wrfm::WrfmModel;
    ///
    /// let err = WrfmModel::from_str(
    ///     "m",
    ///     "wrfm 1\nvertices 1   edges 0\n\nv 1.0 2.0 abc\n",
    /// )
    /// .unwrap_err();
    /// assert_eq!(err.column(), 11);
    /// ```
    pub fn column(&self) -> usize {
        match self {
            ParseError::InvalidVertex { column, .. }
            | ParseError::InvalidEdge { column, .. }
            | ParseError::UnknownDirective { column, .. }
            | ParseError::MissingMagic { column, .. }
            | ParseError::MalformedMagic { column, .. }
            | ParseError::UnsupportedVersion { column, .. }
            | ParseError::MissingHeader { column, .. }
            | ParseError::MalformedHeader { column, .. }
            | ParseError::CountMismatch { column, .. }
            | ParseError::MissingGroupName { column, .. } => *column,
        }
    }

    fn line_text(&self) -> &str {
        match self {
            ParseError::InvalidVertex { line_text, .. }
            | ParseError::InvalidEdge { line_text, .. }
            | ParseError::UnknownDirective { line_text, .. }
            | ParseError::MissingMagic { line_text, .. }
            | ParseError::MalformedMagic { line_text, .. }
            | ParseError::UnsupportedVersion { line_text, .. }
            | ParseError::MissingHeader { line_text, .. }
            | ParseError::MalformedHeader { line_text, .. }
            | ParseError::CountMismatch { line_text, .. }
            | ParseError::MissingGroupName { line_text, .. } => line_text,
        }
    }

    fn detail_line(&self) -> String {
        match self {
            ParseError::InvalidVertex { detail, .. } => match detail {
                VertexError::MissingCoordinate { axis } => {
                    format!("vertex is missing the {axis} coordinate")
                }
                VertexError::NotANumber { token, axis } => {
                    format!("expected a number for the {axis} coordinate, got `{token}`")
                }
            },
            ParseError::InvalidEdge { detail, .. } => match detail {
                EdgeError::MissingIndex { which } => {
                    format!("edge is missing its {} index", ordinal(*which))
                }
                EdgeError::NotAnIndex { token, which } => format!(
                    "expected a vertex index for the {} index, got `{token}`",
                    ordinal(*which)
                ),
                EdgeError::NegativeIndex { index, .. } => {
                    format!("vertex index must be non-negative, got {index}")
                }
                EdgeError::OutOfRange {
                    index,
                    vertex_count,
                    ..
                } => format!(
                    "vertex index {index} is out of range (model has {vertex_count} vertices)"
                ),
            },
            ParseError::UnknownDirective { token, .. } => {
                format!("unknown directive `{token}` (only `v`, `e` and `group` are valid)")
            }
            ParseError::MissingMagic { .. } => {
                "expected `wrfm <version>` as the first line of the file".to_string()
            }
            ParseError::MalformedMagic { .. } => {
                "expected `wrfm <version>` where `<version>` is a positive integer".to_string()
            }
            ParseError::UnsupportedVersion { version, .. } => {
                format!("this file is wrfm version {version}; only version 1 is supported")
            }
            ParseError::MissingHeader { .. } => {
                "expected a `vertices <N>   edges <M>` counts header before the first \
                 vertex, edge or group"
                    .to_string()
            }
            ParseError::MalformedHeader { .. } => {
                "expected `vertices <N>   edges <M>` with non-negative integer counts".to_string()
            }
            ParseError::CountMismatch {
                which,
                declared,
                actual,
                ..
            } => match which {
                CountKind::Vertices => format!("declared {declared} vertices, found {actual}"),
                CountKind::Edges => format!("declared {declared} edges, found {actual}"),
            },
            ParseError::MissingGroupName { .. } => "expected `group <name>`".to_string(),
        }
    }

    /// Caret length in the rustc-style rendering (1 for a missing value, otherwise the token length).
    fn caret_len(&self) -> usize {
        match self {
            ParseError::InvalidVertex { detail, .. } => match detail {
                VertexError::MissingCoordinate { .. } => 1,
                VertexError::NotANumber { token, .. } => token.chars().count().max(1),
            },
            ParseError::InvalidEdge { detail, .. } => match detail {
                EdgeError::MissingIndex { .. } => 1,
                EdgeError::NotAnIndex { token, .. } => token.chars().count().max(1),
                EdgeError::NegativeIndex { index, .. } => index.to_string().chars().count(),
                EdgeError::OutOfRange { index, .. } => index.to_string().chars().count(),
            },
            ParseError::UnknownDirective { token, .. } => token.chars().count().max(1),
            ParseError::MissingMagic { .. } => 1,
            ParseError::MalformedMagic { token, .. } => token.chars().count().max(1),
            ParseError::UnsupportedVersion { token, .. } => token.chars().count().max(1),
            ParseError::MissingHeader { .. } => 1,
            ParseError::MalformedHeader { token, .. } => token.chars().count().max(1),
            ParseError::CountMismatch { .. } => 1,
            ParseError::MissingGroupName { .. } => 1,
        }
    }
}

/// Renders a rustc-style error report with the source line, a caret and a detail message.
/// Renders a rustc-style error report: `error: <kind> at line L, column C`,
/// the offending source line with a caret, and a detail message
/// .
///
/// ```
/// use wrfm::WrfmModel;
///
/// let err = WrfmModel::from_str("m", "v 0 0 0\n").unwrap_err();
/// let rendered = format!("{err}");
/// assert!(
///     rendered.contains("error: missing wrfm magic at line 1, column 1"),
///     "{rendered}"
/// );
/// assert!(
///     rendered.contains("expected `wrfm <version>` as the first line of the file"),
///     "{rendered}"
/// );
/// ```
impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let line = self.line();
        let column = self.column();
        let kind = match self {
            ParseError::InvalidVertex { .. } => "invalid vertex",
            ParseError::InvalidEdge { .. } => "invalid edge",
            ParseError::UnknownDirective { .. } => "unknown directive",
            ParseError::MissingMagic { .. } => "missing wrfm magic",
            ParseError::MalformedMagic { .. } => "malformed wrfm magic",
            ParseError::UnsupportedVersion { .. } => "unsupported wrfm version",
            ParseError::MissingHeader { .. } => "missing counts header",
            ParseError::MalformedHeader { .. } => "malformed counts header",
            ParseError::CountMismatch { which, .. } => match which {
                CountKind::Vertices => "vertex count mismatch",
                CountKind::Edges => "edge count mismatch",
            },
            ParseError::MissingGroupName { .. } => "group without a name",
        };
        writeln!(f, "error: {kind} at line {line}, column {column}")?;
        let width = line.to_string().len();
        let ruler = format!("{:>width$} |", "");
        writeln!(f, "{ruler}")?;

        let line_text = self.line_text();
        let trimmed = line_text.trim_start();
        let leading = line_text.chars().take_while(|c| c.is_whitespace()).count();
        let caret_col = column.saturating_sub(leading);
        writeln!(f, "{line:>width$} | {trimmed}")?;

        write!(
            f,
            "{ruler}{}{}",
            " ".repeat(caret_col),
            "^".repeat(self.caret_len())
        )?;
        writeln!(f)?;
        write!(f, "{}", self.detail_line())
    }
}

/// Classified vertex-line error.
/// Classified vertex-line error.
///
/// ```
/// use wrfm::{ParseError, VertexError, WrfmModel};
///
/// let err = WrfmModel::from_str("m", "wrfm 1\nvertices 1   edges 0\n\nv 1.0 2.0\n")
/// .unwrap_err();
/// assert!(matches!(
/// err,
/// ParseError::InvalidVertex {
/// detail: VertexError::MissingCoordinate { axis: 'z' },
/// ..
/// }
/// ));
/// ```
#[derive(Debug, Clone, PartialEq)]
pub enum VertexError {
    /// "v 1.0" or "v 1.0 2.0" — the coordinate named by `axis` is missing.
    MissingCoordinate {
        /// 'y' when only x is present; 'z' when x and y are present.
        axis: char,
    },
    /// "v 1.0 2.0 abc" — `token` is not a number for the coordinate `axis`.
    NotANumber {
        token: String,
        /// 'x', 'y' or 'z'.
        axis: char,
    },
}

/// Classified edge-line error.
/// Classified edge-line error.
///
/// ```
/// use wrfm::{EdgeError, ParseError, WrfmModel};
///
/// let err = WrfmModel::from_str(
///     "m",
///     "wrfm 1\nvertices 2   edges 1\n\nv 0 0 0\nv 1 1 1\ne 0 5\n",
/// )
/// .unwrap_err();
/// assert!(matches!(
/// err,
/// ParseError::InvalidEdge {
/// detail: EdgeError::OutOfRange { index: 5, .. },
/// ..
/// }
/// ));
/// ```
#[derive(Debug, Clone, PartialEq)]
pub enum EdgeError {
    /// "e 0" — the index named by `which` is missing.
    MissingIndex {
        /// 1 or 2.
        which: u8,
    },
    /// "e a b" — `token` is not an index.
    NotAnIndex {
        token: String,
        /// 1 or 2.
        which: u8,
    },
    /// "e 0 -1" — negative indices are invalid.
    NegativeIndex {
        index: i64,
        /// 1 or 2.
        which: u8,
    },
    /// "e 0 5" with 4 vertices.
    OutOfRange {
        index: usize,
        vertex_count: usize,
        /// 1 or 2.
        which: u8,
    },
}

/// Error returned by `from_file`: an I/O failure or a structured parse error.
/// Error returned by [`WrfmModel::from_file`]: either an I/O failure or a
/// structured parse error. Only available with the `std` feature.
///
/// ```
/// use wrfm::{LoadError, WrfmModel};
///
/// let err = WrfmModel::from_file("/no/such/file.wrfm").unwrap_err();
/// assert!(matches!(err, LoadError::Io(_)));
/// ```
#[cfg(feature = "std")]
#[derive(Debug)]
pub enum LoadError {
    Io(std::io::Error),
    Parse(ParseError),
}

/// Displays the I/O error or the wrapped parse report.
/// Displays a load failure: the underlying I/O error, or the wrapped
/// [`ParseError`] report.
///
/// ```
/// use wrfm::{LoadError, WrfmModel};
///
/// let parse =
///     LoadError::Parse(WrfmModel::from_str("m", "wrfm 2\n").unwrap_err());
/// assert!(format!("{parse}").contains("unsupported wrfm version"));
///
/// let io = LoadError::Io(std::io::Error::new(std::io::ErrorKind::NotFound, "nope"));
/// assert!(format!("{io}").contains("io error: nope"));
/// ```
#[cfg(feature = "std")]
impl fmt::Display for LoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LoadError::Io(e) => write!(f, "io error: {e}"),
            LoadError::Parse(e) => write!(f, "{e}"),
        }
    }
}

/// The cause chain: `Io` wraps the I/O error, `Parse` wraps the `ParseError`.
/// The cause chain: `Io` exposes the wrapped I/O error, `Parse` exposes
/// the underlying [`ParseError`] .
///
/// ```
/// use std::error::Error;
/// use wrfm::{LoadError, WrfmModel};
///
/// let parse =
///     LoadError::Parse(WrfmModel::from_str("m", "wrfm 2\n").unwrap_err());
/// assert!(parse.source().is_some());
///
/// let io = LoadError::Io(std::io::Error::new(std::io::ErrorKind::Other, "boom"));
/// assert!(io.source().is_some());
/// ```
#[cfg(feature = "std")]
impl std::error::Error for LoadError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            LoadError::Io(e) => Some(e),
            LoadError::Parse(p) => Some(p),
        }
    }
}

/// `ParseError` is a leaf error: `source()` is `None`.
/// [`ParseError`] is a leaf error: it has no source (`source()` is `None`).
///
/// ```
/// use std::error::Error;
/// use wrfm::WrfmModel;
///
/// let err = WrfmModel::from_str("m", "wrfm 2\n").unwrap_err();
/// assert!(err.source().is_none());
/// ```
#[cfg(feature = "std")]
impl std::error::Error for ParseError {}

fn ordinal(which: u8) -> &'static str {
    match which {
        1 => "1st",
        2 => "2nd",
        _ => "?th",
    }
}

/// Column of a missing value: one past the last parsed token (2 for a bare directive).
fn missing_value_column(toks: &[(&str, usize)]) -> usize {
    if toks.len() <= 1 {
        // Only the directive is present: the missing value begins right
        // past it (column 2 for `v` / `e`).
        return 2;
    }
    let &(tok, col) = toks.last().expect("len > 1");
    col + tok.chars().count() - 1
}

/// Column + token text of the offending part of a malformed counts header.
fn malformed_header_location(toks: &[(&str, usize)]) -> (usize, String) {
    // toks[0] is `vertices`.
    if toks.len() < 2 || toks[1].0.parse::<usize>().is_err() {
        return (
            missing_value_column(toks),
            toks.get(1).map(|t| t.0.to_string()).unwrap_or_default(),
        );
    }
    if toks.len() < 3 || toks[2].0 != "edges" {
        return (
            missing_value_column(toks),
            toks.get(2).map(|t| t.0.to_string()).unwrap_or_default(),
        );
    }
    if toks.len() < 4 || toks[3].0.parse::<usize>().is_err() {
        return (
            missing_value_column(toks),
            toks.get(3).map(|t| t.0.to_string()).unwrap_or_default(),
        );
    }
    if toks.len() > 4 {
        return (toks[4].1, toks[4].0.to_string());
    }
    // Unreachable for a malformed header: exactly four valid tokens.
    (toks[0].1, toks[0].0.to_string())
}

/// Split a line into `(token, 1-based char column)` pairs, ignoring surrounding whitespace.
fn tokenize(line: &str) -> Vec<(&str, usize)> {
    let mut out = Vec::new();
    let mut chars = line.char_indices().peekable();
    let mut col = 1usize;
    while let Some(&(b, c)) = chars.peek() {
        if c.is_whitespace() {
            col += 1;
            chars.next();
            continue;
        }
        let start_byte = b;
        let start_col = col;
        while let Some(&(_, c2)) = chars.peek() {
            if c2.is_whitespace() {
                break;
            }
            col += 1;
            chars.next();
        }
        let end_byte = chars.peek().map(|&(b2, _)| b2).unwrap_or(line.len());
        out.push((&line[start_byte..end_byte], start_col));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const CUBE: &str = "\
wrfm 1
vertices 8 edges 12

v -1.000000 -1.000000 -1.000000
v 1.000000 -1.000000 -1.000000
v 1.000000 1.000000 -1.000000
v -1.000000 1.000000 -1.000000
v -1.000000 -1.000000 1.000000
v 1.000000 -1.000000 1.000000
v 1.000000 1.000000 1.000000
v -1.000000 1.000000 1.000000

e 0 1
e 1 2
e 2 3
e 3 0
e 4 5
e 5 6
e 6 7
e 7 4
e 0 4
e 1 5
e 2 6
e 3 7
";

    /// Wrap `body` in a v1 magic line + counts header so tests stay focused.
    fn v1(vertices: usize, edges: usize, body: &str) -> String {
        format!("wrfm 1\nvertices {vertices}   edges {edges}\n\n{body}")
    }

    #[test]
    fn parses_valid_model() {
        let model = WrfmModel::from_str("cube", CUBE).expect("cube parses");
        assert_eq!(model.name, "cube");
        assert_eq!(model.version, 1);
        assert_eq!(model.vertices.len(), 8);
        assert_eq!(model.edges.len(), 12);
        assert!(model.groups.is_empty());
    }

    #[test]
    fn empty_v1_model_ok() {
        let model = WrfmModel::from_str("m", "wrfm 1\nvertices 0   edges 0\n")
            .expect("the empty v1 model parses");
        assert_eq!(model.version, 1);
        assert_eq!(model.vertices.len(), 0);
        assert_eq!(model.edges.len(), 0);
        assert!(model.groups.is_empty());
    }

    #[test]
    fn empty_input_is_missing_magic() {
        let err = WrfmModel::from_str("m", "").expect_err("empty input is missing magic");
        match err {
            ParseError::MissingMagic { line, column, .. } => {
                assert_eq!((line, column), (1, 1));
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn comment_first_is_missing_magic() {
        let err =
            WrfmModel::from_str("m", "# hi\n\n").expect_err("a leading comment is missing magic");
        assert!(matches!(err, ParseError::MissingMagic { line: 1, .. }));
    }

    #[test]
    fn comment_after_magic_is_ok() {
        let model = WrfmModel::from_str(
            "m",
            "wrfm 1\nvertices 2   edges 1\n\n# hi\nv 0 0 0\nv 1 1 1\n\ne 0 1\n",
        )
        .expect("comments after the header are fine");
        assert_eq!(model.vertices.len(), 2);
        assert_eq!(model.edges.len(), 1);
    }

    #[test]
    fn bom_before_magic_ok() {
        let model = WrfmModel::from_str(
            "m",
            "\u{feff}wrfm 1\nvertices 2   edges 1\n\nv 0 0 0\nv 1 1 1\ne 0 1\n",
        )
        .expect("BOM before the magic is stripped");
        assert_eq!(model.version, 1);
        assert_eq!(model.vertices.len(), 2);
        assert_eq!(model.edges.len(), 1);
    }

    #[test]
    fn crlf_lines_parse() {
        let model = WrfmModel::from_str(
            "m",
            "wrfm 1\r\nvertices 2   edges 1\r\n\r\nv 0 0 0\r\nv 1 1 1\r\n\ne 0 1\r\n",
        )
        .expect("CRLF lines parse");
        assert_eq!(model.vertices.len(), 2);
        assert_eq!(model.edges.len(), 1);
    }

    #[test]
    fn magic_required_line_one() {
        let err = WrfmModel::from_str("m", "v 0 0 0\nv 1 1 1\ne 0 1\n")
            .expect_err("a v/e file without magic is rejected");
        assert!(matches!(err, ParseError::MissingMagic { line: 1, .. }));
    }

    #[test]
    fn magic_after_comment_rejected() {
        let err = WrfmModel::from_str(
            "m",
            "# hi\nwrfm 1\nvertices 2   edges 1\n\nv 0 0 0\nv 1 1 1\ne 0 1\n",
        )
        .expect_err("the magic must be the very first line");
        assert!(matches!(err, ParseError::MissingMagic { .. }));
    }

    #[test]
    fn unsupported_version() {
        let err = WrfmModel::from_str("m", "wrfm 2\nvertices 0   edges 0\n")
            .expect_err("version 2 is not supported");
        match err {
            ParseError::UnsupportedVersion { version, .. } => assert_eq!(version, 2),
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn malformed_magic() {
        // `wrfm` with no version token.
        let err = WrfmModel::from_str("m", "wrfm\n").expect_err("missing version is malformed");
        assert!(matches!(err, ParseError::MalformedMagic { .. }));

        // `wrfm x` — non-integer version.
        let err =
            WrfmModel::from_str("m", "wrfm x\n").expect_err("non-integer version is malformed");
        assert!(matches!(err, ParseError::MalformedMagic { .. }));
    }

    #[test]
    fn header_required() {
        let err = WrfmModel::from_str("m", "wrfm 1\nv 0 0 0\n")
            .expect_err("a vertex before the header is an error");
        assert!(matches!(err, ParseError::MissingHeader { line: 2, .. }));
    }

    #[test]
    fn malformed_header() {
        // Incomplete: `vertices 4` only.
        let err = WrfmModel::from_str("m", "wrfm 1\nvertices 4\n")
            .expect_err("an incomplete header is malformed");
        assert!(matches!(err, ParseError::MalformedHeader { .. }));

        // Non-integer edge count.
        let err = WrfmModel::from_str("m", "wrfm 1\nvertices 4 edges x\n")
            .expect_err("a non-integer edge count is malformed");
        assert!(matches!(err, ParseError::MalformedHeader { .. }));

        // Typo (`edge` instead of `edges`).
        let err = WrfmModel::from_str("m", "wrfm 1\nvertices 4 edge 4\n")
            .expect_err("a header typo is malformed");
        assert!(matches!(err, ParseError::MalformedHeader { .. }));
    }

    #[test]
    fn header_with_trailing_comment_ok() {
        let model = WrfmModel::from_str(
            "m",
            "wrfm 1\nvertices 2   edges 1 # authored by hand\n\nv 0 0 0\nv 1 1 1\ne 0 1\n",
        )
        .expect("a trailing header comment is ignored");
        assert_eq!(model.vertices.len(), 2);
        assert_eq!(model.edges.len(), 1);
    }

    #[test]
    fn count_mismatch_vertices() {
        let err = WrfmModel::from_str("m", &v1(5, 0, "v 0 0 0\nv 1 1 1\nv 2 2 2\nv 3 3 3\n"))
            .expect_err("fewer vertices than declared");
        match err {
            ParseError::CountMismatch {
                which,
                declared,
                actual,
                line,
                ..
            } => {
                assert_eq!(which, CountKind::Vertices);
                assert_eq!((declared, actual), (5, 4));
                assert_eq!(line, 2); // the header line
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn count_mismatch_edges() {
        let err = WrfmModel::from_str("m", &v1(0, 3, "e 0 1\ne 1 2\ne 2 3\ne 3 4\n"))
            .expect_err("more edges than declared");
        match err {
            ParseError::CountMismatch {
                which,
                declared,
                actual,
                ..
            } => {
                assert_eq!(which, CountKind::Edges);
                assert_eq!((declared, actual), (3, 4));
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn groups_recorded() {
        let model = WrfmModel::from_str(
            "m",
 &v1(
 4,
 3,
                "group body\n  v 0 0 0\n  v 1 0 0\n  v 1 1 0\n  e 0 1\n  e 1 2\ngroup head\n  v 0.5 2 0.25\n  e 2 3\n",
 ),
 )
        .expect("groups parse");
        assert_eq!(model.vertices.len(), 4);
        assert_eq!(
            model.groups,
            vec![
                Group {
                    name: "body".to_string(),
                    vertex_start: 0,
                    vertex_end: 3
                },
                Group {
                    name: "head".to_string(),
                    vertex_start: 3,
                    vertex_end: 4
                },
            ]
        );
    }

    #[test]
    fn group_no_name() {
        let err = WrfmModel::from_str("m", &v1(0, 0, "group\n"))
            .expect_err("a group without a name is an error");
        assert!(matches!(err, ParseError::MissingGroupName { .. }));
    }

    #[test]
    fn group_optional() {
        let model = WrfmModel::from_str("m", &v1(1, 0, "v 0 0 0\n")).expect("groups are optional");
        assert!(model.groups.is_empty());
    }

    #[test]
    fn empty_group_not_recorded() {
        let model = WrfmModel::from_str("m", &v1(1, 0, "group empty\ngroup body\n  v 0 0 0\n"))
            .expect("an empty group is allowed and not recorded");
        assert_eq!(
            model.groups,
            vec![Group {
                name: "body".to_string(),
                vertex_start: 0,
                vertex_end: 1
            }]
        );
    }

    #[test]
    fn unnamed_vertices_before_first_group() {
        let model =
            WrfmModel::from_str("m", &v1(3, 0, "v 9 9 9\ngroup body\n v 0 0 0\n v 1 1 1\n"))
                .expect("vertices before the first group are unnamed");
        assert_eq!(
            model.groups,
            vec![Group {
                name: "body".to_string(),
                vertex_start: 1,
                vertex_end: 3
            }]
        );
    }

    #[test]
    fn forward_ref_across_groups() {
        let model = WrfmModel::from_str(
            "m",
            &v1(2, 1, "group a\n  v 0 0 0\ngroup b\n  v 1 1 1\n  e 0 1\n"),
        )
        .expect("an edge in group B may reference a vertex in group A");
        assert_eq!(model.edges, vec![(0, 1)]);
        assert_eq!(model.groups.len(), 2);
    }

    #[test]
    fn v_alone_reports_missing_x() {
        let err = WrfmModel::from_str("m", &v1(1, 0, "v\n")).expect_err("a bare v is an error");
        match err {
            ParseError::InvalidVertex {
                detail: VertexError::MissingCoordinate { axis },
                ..
            } => assert_eq!(axis, 'x'),
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn missing_y_reports_line_column() {
        let err =
            WrfmModel::from_str("m", &v1(1, 0, "v 1.0\n")).expect_err("missing y is an error");
        match err {
            ParseError::InvalidVertex {
                line,
                column,
                detail: VertexError::MissingCoordinate { axis },
                ..
            } => {
                assert_eq!((line, column), (4, 5));
                assert_eq!(axis, 'y');
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn missing_z_reports_line_column() {
        // Blank lines after the header are ignored.
        let err = WrfmModel::from_str("m", &v1(1, 0, "\n\nv 1.0 2.0\n"))
            .expect_err("missing z is an error");
        match err {
            ParseError::InvalidVertex {
                line,
                column,
                detail: VertexError::MissingCoordinate { axis },
                ..
            } => {
                assert_eq!((line, column), (6, 9));
                assert_eq!(axis, 'z');
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn not_a_number_reports_token() {
        let err = WrfmModel::from_str("m", &v1(1, 0, "v 1.0 2.0 abc\n"))
            .expect_err("abc is not a number");
        match err {
            ParseError::InvalidVertex {
                column,
                detail: VertexError::NotANumber { token, axis },
                ..
            } => {
                assert_eq!(token, "abc");
                assert_eq!(axis, 'z');
                assert_eq!(column, 11);
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn not_a_number_in_y() {
        let err = WrfmModel::from_str("m", &v1(1, 0, "v 1.0 abc 3.0\n"))
            .expect_err("abc is not a number");
        match err {
            ParseError::InvalidVertex {
                detail: VertexError::NotANumber { token, axis },
                ..
            } => {
                assert_eq!(token, "abc");
                assert_eq!(axis, 'y');
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn extra_values_ignored() {
        let model =
            WrfmModel::from_str("m", &v1(1, 0, "v 1 2 3 4 5\n")).expect("extra values are ignored");
        assert_eq!(model.vertices.len(), 1);
        assert_eq!(model.vertices[0], (1.0, 2.0, 3.0));
    }

    #[test]
    fn edge_missing_first_index() {
        let err = WrfmModel::from_str("m", &v1(1, 1, "v 0 0 0\n\ne\n"))
            .expect_err("missing index is an error");
        match err {
            ParseError::InvalidEdge {
                line,
                column,
                detail: EdgeError::MissingIndex { which },
                ..
            } => {
                assert_eq!((line, column), (6, 2));
                assert_eq!(which, 1);
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn edge_missing_second_index() {
        let err = WrfmModel::from_str("m", &v1(2, 1, "v 0 0 0\nv 1 1 1\ne 0\n"))
            .expect_err("missing second index is an error");
        match err {
            ParseError::InvalidEdge {
                detail: EdgeError::MissingIndex { which },
                ..
            } => assert_eq!(which, 2),
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn edge_not_an_index() {
        let err = WrfmModel::from_str("m", &v1(2, 1, "v 0 0 0\nv 1 1 1\ne a b\n"))
            .expect_err("letters are not indices");
        match err {
            ParseError::InvalidEdge {
                detail: EdgeError::NotAnIndex { token, which },
                ..
            } => {
                assert_eq!(token, "a");
                assert_eq!(which, 1);
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn edge_negative_index() {
        let err = WrfmModel::from_str("m", &v1(2, 1, "v 0 0 0\nv 1 1 1\ne 0 -1\n"))
            .expect_err("negative index is an error");
        match err {
            ParseError::InvalidEdge {
                detail: EdgeError::NegativeIndex { index, which },
                ..
            } => {
                assert_eq!(index, -1);
                assert_eq!(which, 2);
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn edge_out_of_range() {
        let err = WrfmModel::from_str("m", &v1(2, 1, "v 0 0 0\nv 1 1 1\ne 0 5\n"))
            .expect_err("out of range index is an error");
        match err {
            ParseError::InvalidEdge {
                detail:
                    EdgeError::OutOfRange {
                        index,
                        vertex_count,
                        which,
                    },
                ..
            } => {
                assert_eq!((index, vertex_count), (5, 2));
                assert_eq!(which, 2);
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn edge_out_of_range_first() {
        let err = WrfmModel::from_str("m", &v1(2, 1, "v 0 0 0\nv 1 1 1\ne 5 0\n"))
            .expect_err("out of range first index is an error");
        match err {
            ParseError::InvalidEdge {
                detail:
                    EdgeError::OutOfRange {
                        index,
                        vertex_count,
                        which,
                    },
                ..
            } => {
                assert_eq!((index, vertex_count), (5, 2));
                assert_eq!(which, 1);
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn forward_reference_is_ok() {
        let model = WrfmModel::from_str("m", &v1(2, 1, "v 0 0 0\nv 1 1 1\ne 0 1\n"))
            .expect("edges referencing defined vertices parse");
        assert_eq!(model.vertices.len(), 2);
        assert_eq!(model.edges.len(), 1);
    }

    #[test]
    fn unknown_line_skipped_default() {
        let model = WrfmModel::from_str("m", &v1(1, 0, "garbage x\nv 0 0 0\n"))
            .expect("unknown line skipped");
        assert_eq!(model.vertices.len(), 1);
    }

    #[test]
    fn unknown_line_strict_reports() {
        let err = WrfmModel::parse_with("m", &v1(1, 0, "garbage x\nv 0 0 0\n"), true)
            .expect_err("strict mode rejects unknown lines");
        match err {
            ParseError::UnknownDirective {
                line,
                column,
                token,
                ..
            } => {
                assert_eq!((line, column), (4, 1));
                assert_eq!(token, "garbage");
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn parse_with_false_equals_from_str() {
        let input = v1(2, 1, "v 0 0 0\nv 1 1 1\ne 0 1\n");
        let a = WrfmModel::from_str("m", &input);
        let b = WrfmModel::parse_with("m", &input, false);
        assert_eq!(a, b);
    }

    #[test]
    fn display_shows_rustc_style() {
        let err = WrfmModel::from_str("m", &v1(1, 0, "v 1.0 2.0 abc\n"))
            .expect_err("abc is not a number");
        let rendered = format!("{err}");
        assert!(
            rendered.contains("error: invalid vertex at line 4, column 11"),
            "{rendered}"
        );
        assert!(rendered.contains("4 | v 1.0 2.0 abc"), "{rendered}");
        assert!(rendered.contains("|           ^^^"), "{rendered}");
        assert!(
            rendered.contains("expected a number for the z coordinate, got `abc`"),
            "{rendered}"
        );
    }

    #[test]
    fn display_shows_rustc_style_for_magic() {
        let err = WrfmModel::from_str("m", "v 0 0 0\n").expect_err("no magic");
        let rendered = format!("{err}");
        assert!(
            rendered.contains("error: missing wrfm magic at line 1, column 1"),
            "{rendered}"
        );
        assert!(rendered.contains("1 | v 0 0 0"), "{rendered}");
        assert!(
            rendered.contains("expected `wrfm <version>` as the first line of the file"),
            "{rendered}"
        );
    }

    #[test]
    fn line_and_column_accessors() {
        let err = WrfmModel::from_str("m", &v1(1, 0, "\nv 1.0 2.0 abc\n"))
            .expect_err("abc is not a number");
        match &err {
            ParseError::InvalidVertex { line, column, .. } => {
                assert_eq!(err.line(), *line);
                assert_eq!(err.column(), *column);
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn roundtrip_exact() {
        let mut model = WrfmModel::new("exact");
        model.vertices.push((1.0 / 3.0, 2.0f64.sqrt(), -0.75));
        model.vertices.push((0.1, 1e-8, 3.0));
        model.edges.push((0, 1));
        model.groups.push(Group {
            name: "points".to_string(),
            vertex_start: 0,
            vertex_end: 2,
        });

        let dir = temp_dir("roundtrip");
        let path = dir.join("exact.wrfm");
        model.save_to_file(&path).unwrap();
        let back = WrfmModel::from_file(&path).unwrap();
        assert_eq!(back.vertices, model.vertices); // bit-identical f64s
        assert_eq!(back.groups, model.groups);
        assert_eq!(back.version, 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn shortest_roundtrip_serialized() {
        let mut model = WrfmModel::new("third");
        model.vertices.push((1.0 / 3.0, 0.0, 0.0));

        let dir = temp_dir("third");
        let path = dir.join("third.wrfm");
        model.save_to_file(&path).unwrap();
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("0.3333333333333333"), "{content}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn saved_file_is_canonical_v1() {
        let mut model = WrfmModel::new("cube");
        model.vertices.push((0.0, 0.0, 0.0));

        let dir = temp_dir("canonical");
        let path = dir.join("cube.wrfm");
        model.save_to_file(&path).unwrap();
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(
            content.starts_with("wrfm 1\nvertices 1   edges 0\n"),
            "{content}"
        );
        assert!(
            !content.contains("ComChan"),
            "legacy comment dropped: {content}"
        );
        assert!(
            !content.contains("Name:"),
            "name is never written: {content}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn temp_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("wrfm-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }
}
