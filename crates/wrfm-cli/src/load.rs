use ratatui_wireframe::model::Model;
use std::io::Read;
use wrfm::WrfmModel;

/// A loaded model plus the parser's metadata (display name, format version, groups, source, byte count).
pub struct Loaded {
    pub model: Model,
    pub name: String,
    /// Format version from the magic line (`1` for v1).
    pub version: u32,
    /// Named sections over the global vertex list.
    pub groups: Vec<wrfm::Group>,
    /// Source description for reports (`<path>` or `-` for stdin).
    pub source: String,
    /// Number of bytes read from the source (file size / stdin length).
    pub bytes: usize,
}

/// The failure mode of [`load`]: one user-facing message, already formatted for stderr.
pub struct LoadError(pub String);

/// Load a model from `<file|->`. `-` reads all of stdin once (pipeline mode: the stream is buffered in memory).
pub fn load(source: &str) -> Result<Loaded, LoadError> {
    if source == "-" {
        let mut buf = String::new();
        std::io::stdin()
            .read_to_string(&mut buf)
            .map_err(|e| LoadError(format!("error: cannot read stdin: {e}")))?;
        return from_text("stdin", &buf, "-");
    }
    let raw = WrfmModel::from_file(source).map_err(|e| LoadError(render_load_error(source, &e)))?;
    let bytes = std::fs::metadata(source)
        .map(|m| m.len() as usize)
        .unwrap_or(0);
    Ok(Loaded {
        model: Model {
            vertices: raw.vertices,
            edges: raw.edges,
        },
        name: raw.name,
        version: raw.version,
        groups: raw.groups,
        source: source.to_string(),
        bytes,
    })
}

/// Load from an in-memory buffer (stdin), naming the model `stdin`.
fn from_text(name: &str, text: &str, source: &str) -> Result<Loaded, LoadError> {
    let bytes = text.len();
    let raw = WrfmModel::from_str(name, text).map_err(|e| LoadError(e.to_string()))?;
    Ok(Loaded {
        model: Model {
            vertices: raw.vertices,
            edges: raw.edges,
        },
        name: raw.name,
        version: raw.version,
        groups: raw.groups,
        source: source.to_string(),
        bytes,
    })
}

/// Render a `wrfm::LoadError` (I/O vs parse) into the single stderr line the caller prints.
fn render_load_error(source: &str, e: &wrfm::LoadError) -> String {
    match e {
        wrfm::LoadError::Io(e) => format!("error: cannot read '{source}': {e}"),
        wrfm::LoadError::Parse(e) => e.to_string(),
    }
}

/// Serialize a geometry `Model` back to canonical `.wrfm` v1 TEXT (magic + counts header + groups + edges).
pub fn serialize_model(m: &Model, name: &str, groups: &[wrfm::Group], version: u32) -> String {
    let mut raw = WrfmModel::new(name);
    raw.vertices = m.vertices.clone();
    raw.edges = m.edges.clone();
    raw.groups = groups.to_vec();
    raw.version = version;
    raw.to_string()
}
