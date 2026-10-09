//! The vertex/edge model every raster entry point takes.

/// A 3D wireframe model in model space.
///
/// The viewer and the CLI both parse with `wrfm` and render through this
/// crate; owning the type here keeps the renderer's input stable and the
/// dependency graph free of a second `wrfm`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Model {
    /// Vertices in model space.
    pub vertices: Vec<(f64, f64, f64)>,
    /// 0-based edges into [`Model::vertices`].
    pub edges: Vec<(usize, usize)>,
}
