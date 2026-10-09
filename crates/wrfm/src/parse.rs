use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::error::{CountKind, EdgeError, ParseError, VertexError};
use crate::model::{Group, WrfmModel};

impl WrfmModel {
    /// Parses a model directly from a string (useful for include_str!).
    ///
    /// The input must be v2 (v1 files are rejected): a `wrfm 2` magic line, a
    /// `vertices <N> edges <M>` counts header, then `v` / `e` / `group`
    /// lines. Lenient: blank lines, `#` comments and unknown
    /// lines are skipped. Returns a structured [`ParseError`] on malformed
    /// input.
    ///
    /// ```
    /// use wrfm::WrfmModel;
    ///
    /// let model = WrfmModel::from_str(
    ///     "cube",
    ///     "wrfm 2\nvertices 2   edges 1\n\nv 0 0 0\nv 1 1 1\n\ne 0 1\n",
    /// )
    /// .unwrap();
    /// assert_eq!(model.name, "cube");
    /// assert_eq!(model.version, 2);
    /// assert_eq!(model.vertices.len(), 2);
    /// assert_eq!(model.edges, vec![(0, 1)]);
    /// ```
    pub fn from_str(name: &str, input: &str) -> Result<Self, ParseError> {
        let input = input.strip_prefix('\u{feff}').unwrap_or(input);
        let mut model = WrfmModel {
            name: name.to_string(),
            vertices: Vec::new(),
            edges: Vec::new(),
            version: 2,
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
                Ok(2) => model.version = 2,
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
                // Unknown lines are skipped silently.
                _ => {}
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
