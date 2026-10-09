use alloc::format;
use alloc::string::{String, ToString};
use core::fmt;

/// Which declared count (vertices or edges) disagreed with the parsed content.
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

/// The cause chain: `Io` exposes the wrapped I/O error, `Parse` exposes
/// the underlying [`ParseError`].
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
