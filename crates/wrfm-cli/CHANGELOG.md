# Changelog

## 0.1.0 - 2026-08-11

### :rocket: New features

- **(format)** `wrfm format` — self-contained `.wrfm` v1 spec and streams guide
- **(check)** `wrfm check` — L2 health (duplicates, zero-length, dangling, isolated, non-manifold), `--group` and `--strict`
- **(info)** `wrfm info` — metadata: counts, groups, bounding box
- **(geometry)** `wrfm geometry` — structured facts: bounds, centroid, topology, symmetry, alignment
- **(query)** `wrfm query` — extents, topology, edge stats, profile, cross-section, connectivity
- **(group)** `wrfm group` — per-part facts and group-aware scoping
- **(view)** `wrfm view` — exact per-view facts: occlusion, silhouette, depth order
- **(render)** `wrfm render` — braille / ascii / grid, six standard views or a custom camera
- **(transform)** `wrfm transform` — full affine: rotate / scale / shear / mirror / translate / pivot / align
- **(edit)** `wrfm edit` — delete vertices / edges, extract a group, clean / dedupe / merge
- **(diff)** `wrfm diff` — structured vertex/edge diff or density-grid comparison

### :zap: Performance

- **(render)** cached batch projection, parallel rasterization for large models

### :hammer: Build

- **(streams)** read-only, streaming: every command accepts `-` for stdin

<!-- wireforge -->
