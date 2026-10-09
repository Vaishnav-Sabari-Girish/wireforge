use alloc::format;
use alloc::string::{String, ToString};

use super::*;

const CUBE: &str = "\
wrfm 2
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

/// Wrap `body` in a v2 magic line + counts header so tests stay focused.
fn v2(vertices: usize, edges: usize, body: &str) -> String {
    format!("wrfm 2\nvertices {vertices}   edges {edges}\n\n{body}")
}

#[test]
fn parses_valid_model() {
    let model = WrfmModel::from_str("cube", CUBE).expect("cube parses");
    assert_eq!(model.name, "cube");
    assert_eq!(model.version, 2);
    assert_eq!(model.vertices.len(), 8);
    assert_eq!(model.edges.len(), 12);
    assert!(model.groups.is_empty());
}

#[test]
fn empty_v2_model_ok() {
    let model = WrfmModel::from_str("m", "wrfm 2\nvertices 0   edges 0\n")
        .expect("the empty v2 model parses");
    assert_eq!(model.version, 2);
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
    let err = WrfmModel::from_str("m", "# hi\n\n").expect_err("a leading comment is missing magic");
    assert!(matches!(err, ParseError::MissingMagic { line: 1, .. }));
}

#[test]
fn comment_after_magic_is_ok() {
    let model = WrfmModel::from_str(
        "m",
        "wrfm 2\nvertices 2   edges 1\n\n# hi\nv 0 0 0\nv 1 1 1\n\ne 0 1\n",
    )
    .expect("comments after the header are fine");
    assert_eq!(model.vertices.len(), 2);
    assert_eq!(model.edges.len(), 1);
}

#[test]
fn bom_before_magic_ok() {
    let model = WrfmModel::from_str(
        "m",
        "\u{feff}wrfm 2\nvertices 2   edges 1\n\nv 0 0 0\nv 1 1 1\ne 0 1\n",
    )
    .expect("BOM before the magic is stripped");
    assert_eq!(model.version, 2);
    assert_eq!(model.vertices.len(), 2);
    assert_eq!(model.edges.len(), 1);
}

#[test]
fn crlf_lines_parse() {
    let model = WrfmModel::from_str(
        "m",
        "wrfm 2\r\nvertices 2   edges 1\r\n\r\nv 0 0 0\r\nv 1 1 1\r\n\ne 0 1\r\n",
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
        "# hi\nwrfm 2\nvertices 2   edges 1\n\nv 0 0 0\nv 1 1 1\ne 0 1\n",
    )
    .expect_err("the magic must be the very first line");
    assert!(matches!(err, ParseError::MissingMagic { .. }));
}

#[test]
fn unsupported_version() {
    let err = WrfmModel::from_str("m", "wrfm 3\nvertices 0   edges 0\n")
        .expect_err("version 3 is not supported");
    match err {
        ParseError::UnsupportedVersion { version, .. } => assert_eq!(version, 3),
        other => panic!("unexpected error: {other:?}"),
    }
}

#[test]
fn malformed_magic() {
    // `wrfm` with no version token.
    let err = WrfmModel::from_str("m", "wrfm\n").expect_err("missing version is malformed");
    assert!(matches!(err, ParseError::MalformedMagic { .. }));

    // `wrfm x` — non-integer version.
    let err = WrfmModel::from_str("m", "wrfm x\n").expect_err("non-integer version is malformed");
    assert!(matches!(err, ParseError::MalformedMagic { .. }));
}

#[test]
fn header_required() {
    let err = WrfmModel::from_str("m", "wrfm 2\nv 0 0 0\n")
        .expect_err("a vertex before the header is an error");
    assert!(matches!(err, ParseError::MissingHeader { line: 2, .. }));
}

#[test]
fn malformed_header() {
    // Incomplete: `vertices 4` only.
    let err = WrfmModel::from_str("m", "wrfm 2\nvertices 4\n")
        .expect_err("an incomplete header is malformed");
    assert!(matches!(err, ParseError::MalformedHeader { .. }));

    // Non-integer edge count.
    let err = WrfmModel::from_str("m", "wrfm 2\nvertices 4 edges x\n")
        .expect_err("a non-integer edge count is malformed");
    assert!(matches!(err, ParseError::MalformedHeader { .. }));

    // Typo (`edge` instead of `edges`).
    let err = WrfmModel::from_str("m", "wrfm 2\nvertices 4 edge 4\n")
        .expect_err("a header typo is malformed");
    assert!(matches!(err, ParseError::MalformedHeader { .. }));
}

#[test]
fn header_with_trailing_comment_ok() {
    let model = WrfmModel::from_str(
        "m",
        "wrfm 2\nvertices 2   edges 1 # authored by hand\n\nv 0 0 0\nv 1 1 1\ne 0 1\n",
    )
    .expect("a trailing header comment is ignored");
    assert_eq!(model.vertices.len(), 2);
    assert_eq!(model.edges.len(), 1);
}

#[test]
fn count_mismatch_vertices() {
    let err = WrfmModel::from_str("m", &v2(5, 0, "v 0 0 0\nv 1 1 1\nv 2 2 2\nv 3 3 3\n"))
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
    let err = WrfmModel::from_str("m", &v2(0, 3, "e 0 1\ne 1 2\ne 2 3\ne 3 4\n"))
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
 &v2(
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
    let err = WrfmModel::from_str("m", &v2(0, 0, "group\n"))
        .expect_err("a group without a name is an error");
    assert!(matches!(err, ParseError::MissingGroupName { .. }));
}

#[test]
fn group_optional() {
    let model = WrfmModel::from_str("m", &v2(1, 0, "v 0 0 0\n")).expect("groups are optional");
    assert!(model.groups.is_empty());
}

#[test]
fn empty_group_not_recorded() {
    let model = WrfmModel::from_str("m", &v2(1, 0, "group empty\ngroup body\n  v 0 0 0\n"))
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
    let model = WrfmModel::from_str("m", &v2(3, 0, "v 9 9 9\ngroup body\n v 0 0 0\n v 1 1 1\n"))
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
        &v2(2, 1, "group a\n  v 0 0 0\ngroup b\n  v 1 1 1\n  e 0 1\n"),
    )
    .expect("an edge in group B may reference a vertex in group A");
    assert_eq!(model.edges, vec![(0, 1)]);
    assert_eq!(model.groups.len(), 2);
}

#[test]
fn v_alone_reports_missing_x() {
    let err = WrfmModel::from_str("m", &v2(1, 0, "v\n")).expect_err("a bare v is an error");
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
    let err = WrfmModel::from_str("m", &v2(1, 0, "v 1.0\n")).expect_err("missing y is an error");
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
    let err =
        WrfmModel::from_str("m", &v2(1, 0, "\n\nv 1.0 2.0\n")).expect_err("missing z is an error");
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
    let err =
        WrfmModel::from_str("m", &v2(1, 0, "v 1.0 2.0 abc\n")).expect_err("abc is not a number");
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
    let err =
        WrfmModel::from_str("m", &v2(1, 0, "v 1.0 abc 3.0\n")).expect_err("abc is not a number");
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
        WrfmModel::from_str("m", &v2(1, 0, "v 1 2 3 4 5\n")).expect("extra values are ignored");
    assert_eq!(model.vertices.len(), 1);
    assert_eq!(model.vertices[0], (1.0, 2.0, 3.0));
}

#[test]
fn edge_missing_first_index() {
    let err = WrfmModel::from_str("m", &v2(1, 1, "v 0 0 0\n\ne\n"))
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
    let err = WrfmModel::from_str("m", &v2(2, 1, "v 0 0 0\nv 1 1 1\ne 0\n"))
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
    let err = WrfmModel::from_str("m", &v2(2, 1, "v 0 0 0\nv 1 1 1\ne a b\n"))
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
    let err = WrfmModel::from_str("m", &v2(2, 1, "v 0 0 0\nv 1 1 1\ne 0 -1\n"))
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
    let err = WrfmModel::from_str("m", &v2(2, 1, "v 0 0 0\nv 1 1 1\ne 0 5\n"))
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
    let err = WrfmModel::from_str("m", &v2(2, 1, "v 0 0 0\nv 1 1 1\ne 5 0\n"))
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
    let model = WrfmModel::from_str("m", &v2(2, 1, "v 0 0 0\nv 1 1 1\ne 0 1\n"))
        .expect("edges referencing defined vertices parse");
    assert_eq!(model.vertices.len(), 2);
    assert_eq!(model.edges.len(), 1);
}

#[test]
fn unknown_line_skipped() {
    let model =
        WrfmModel::from_str("m", &v2(1, 0, "garbage x\nv 0 0 0\n")).expect("unknown line skipped");
    assert_eq!(model.vertices.len(), 1);
}

#[test]
fn display_shows_rustc_style() {
    let err =
        WrfmModel::from_str("m", &v2(1, 0, "v 1.0 2.0 abc\n")).expect_err("abc is not a number");
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
    let err =
        WrfmModel::from_str("m", &v2(1, 0, "\nv 1.0 2.0 abc\n")).expect_err("abc is not a number");
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
    assert_eq!(back.version, 2);
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
fn saved_file_is_canonical_v2() {
    let mut model = WrfmModel::new("cube");
    model.vertices.push((0.0, 0.0, 0.0));

    let dir = temp_dir("canonical");
    let path = dir.join("cube.wrfm");
    model.save_to_file(&path).unwrap();
    let content = std::fs::read_to_string(&path).unwrap();
    assert!(
        content.starts_with("wrfm 2\nvertices 1   edges 0\n"),
        "{content}"
    );
    assert!(
        !content.contains("wireforge"),
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
