# Changelog

## v0.1.0 - 2026-08-11

### :rocket: New features

- **(verify)** `wrfm verify` — intent assertions: size / center / closed / axis / symmetry / groups, with per-expectation deltas and fix commands
- **(check)** informational quality diagnostics — open edges (bridges), proportion (aspect ratio), orientation (lying-on-side); never affect the verdict
- **(symmetry)** `wrfm geometry` and `wrfm verify` report BOTH origin-plane and bbox-centre mirror symmetry; `--expect-symmetric` uses the centre (position-independent) fields
- **(format)** `wrfm format` — self-contained `.wrfm` v1 spec and streams guide
- **(check)** `wrfm check` — L2 health (duplicates, zero-length, dangling, isolated, non-manifold), `--group` and `--strict`
- **(info)** `wrfm info` — metadata: counts, groups, bounding box
- **(geometry)** `wrfm geometry` — structured facts: bounds, centroid, topology, symmetry, alignment
- **(query)** `wrfm query` — extents, topology, edge stats, profile, cross-section, vertices, distance, connectivity
- **(group)** `wrfm group` — per-part facts and group-aware scoping
- **(view)** `wrfm view` — exact per-view facts: occlusion, silhouette, depth order
- **(render)** `wrfm render` — braille / ascii / grid, six standard views or a custom camera
- **(transform)** `wrfm transform` — full affine: rotate / scale / shear / mirror / translate / pivot / align / normalize
- **(edit)** `wrfm edit` — delete vertices / edges, extract a group, clean / dedupe / merge
- **(diff)** `wrfm diff` — structured vertex/edge diff or density-grid comparison

### :recycle: Refactoring

- **(transform)** rename `--center` to `--to-origin` — distinct from the `--pivot center` value
- **(query)** unify the vertex-index API: `--from` / `--to` removed, `--range "a,b"` now covers vertices, distance and connectivity
- **(group)** `--group` scopes info / geometry / verify / diff too (previously check / query / view / render only)

### :zap: Performance

- **(render)** cached batch projection, parallel rasterization for large models

### :hammer: Build

- **(streams)** read-only, streaming: every command accepts `-` for stdin

<!-- ComChan -->
