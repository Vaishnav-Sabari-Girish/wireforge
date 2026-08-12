# wrfm-cli

A read-only, streaming command-line tool for `.wrfm` 3D wireframe models:
validate, inspect, query, view, render, transform, edit and diff — all over
plain stdio, no files written, no TUI needed.

It consumes the [`wrfm`](../wrfm) parser library (the single authority for
syntax and edge-index range) and computes geometry with the same engine as
the [`wireforge`](../../) TUI, so scripts, editors and agents all get the
same numbers.

## Features

* **Verify intent, not just health:** `wrfm check` reports L2 health (`ok` / `warn` / `broken`) plus informational
  quality diagnostics; `wrfm verify` asserts DECLARED intent (size / center / closed / axis / symmetry / groups) with
  per-expectation deltas and fix commands.
* **Facts before pixels:** `wrfm info` / `wrfm geometry` / `wrfm query` compute structured numbers; `wrfm view` gives
  exact per-view occlusion facts — reason precisely without rendering.
* **Character rendering:** `wrfm render` draws braille / ascii / grid to the terminal (six standard views, region zoom,
  token budget) — the same projection as the wireforge TUI.
* **Safe edits over streams:** `wrfm transform` (rigid: rotate / scale / shear / mirror / translate / pivot / align /
  normalize) and `wrfm edit` (topology: delete / extract / clean / dedupe / merge) print the result to stdout — chain
  with pipes, verify with `wrfm check`.
* **Compare:** `wrfm diff` shows exactly what changed between two models (density-grid or structured JSON).

## Getting started

The repo ships sample models in `wrfm_files/`. Every subcommand takes a
path — or `-` for stdin — and prints its result to stdout; nothing is
ever written to disk, so you keep a result with shell redirection.

```bash
# health check: ok / warn / broken
wrfm check wrfm_files/cube.wrfm
# ok: cube (8 vertices, 12 edges)

# braille render straight to the terminal (six views by default)
wrfm render wrfm_files/cube.wrfm
wrfm render wrfm_files/cube.wrfm --views top
```

## Inspect a model

`wrfm info` / `wrfm geometry` / `wrfm query` print structured facts —
JSON for scripting, plain text for reading:

```bash
wrfm info wrfm_files/cube.wrfm
```

```json
{
  "name": "cube",
  "version": 1,
  "vertices": 8,
  "edges": 12,
  "groups": [],
  "bytes": 227,
  "bounds": { "center": [0.0, 0.0, 0.0], "max": [1.0, 1.0, 1.0], "min": [-1.0, -1.0, -1.0] }
}
```

```bash
wrfm query wrfm_files/cube.wrfm extents
```

```text
extents:
  min=[-1.000,-1.000,-1.000] max=[1.000,1.000,1.000] center=[0.000,0.000,0.000]
  span x=2.000 y=2.000 z=2.000  (longest axis: z = 2.000)
  proportions x:y:z = 1.00:1.00:1.00
```

Queries: `extents | topology | edge_stats | profile | cross_section |
vertices | distance | connectivity`. For bounds, centroid, PCA axes,
symmetry and alignment add `wrfm geometry wrfm_files/cube.wrfm`.

## Verify intent

Assert what a model *should* be — size, center, closedness, axis,
symmetry or groups — and get pass/fail with deltas and a fix command:

```bash
wrfm verify wrfm_files/tetrahedron.wrfm --expect-size 2,2,2 --expect-closed
# verdict: pass   (exit code 0)

wrfm verify wrfm_files/tetrahedron.wrfm --expect-size 1,1,1
# verdict: fail   (exit code 1)
# suggestion: wrfm transform - --scale-x 0.5 && wrfm transform - --scale-y 0.5 && wrfm transform - --scale-z 0.5
```

## Transform and edit over stdio

Rigid transforms (`wrfm transform`) and topology edits (`wrfm edit`) print
the resulting `.wrfm` text to stdout, so the original file is never
touched:

```bash
wrfm transform wrfm_files/cube.wrfm --scale 2 --rotate-y 45 > cube-2x.wrfm
wrfm edit wrfm_files/cube.wrfm --delete-vertices 0,1 > trimmed.wrfm
wrfm edit model.wrfm --extract-group cabinet > cabinet.wrfm
```

## Compare models

`wrfm diff` shows exactly what changed — a density-grid in the terminal,
or structured JSON with displacement vectors:

```bash
wrfm diff wrfm_files/cube.wrfm cube-2x.wrfm
# vertices: 8 -> 8 (+0)    edges: 12 -> 12 (+0)
# bbox:  min[-1.00,-1.00,-1.00] max[1.00,1.00,1.00]  ->  min[-2.00,-2.00,-2.00] max[2.00,2.00,2.00]

wrfm diff wrfm_files/cube.wrfm cube-2x.wrfm --format json
# moved vertices with per-vertex displacement and distance, added/removed edges
```

## Command reference

```bash
wrfm check model.wrfm                  # health: ok / warn / broken + quality diagnostics
wrfm verify model.wrfm --expect-closed # intent assertions (pass / fail)
wrfm info model.wrfm                   # metadata: counts, groups, bounding box
wrfm geometry model.wrfm               # bounds, PCA, topology, symmetry, alignment
wrfm query model.wrfm extents          # extents | topology | edge_stats | profile | ...
wrfm group model.wrfm                  # per-part (group) facts
wrfm view model.wrfm --yaw 45          # exact per-view facts (occlusion, silhouette, depth)
wrfm render model.wrfm --views top     # braille / ascii / grid
wrfm transform model.wrfm --scale 2    # affine transforms, printed to stdout
wrfm edit model.wrfm --delete-vertices 0,1   # topology edits, printed to stdout
wrfm diff a.wrfm b.wrfm --format json  # structured or density-grid diff
wrfm format                            # print the .wrfm v1 spec itself
```

## Composing commands with pipes

Every command is read-only and streaming: it reads a model from a file or
`-` (stdin) and prints the result to stdout, so atomic operations chain
into complex transforms with pipes. Each stage can be verified with
`wrfm check`:

```bash
# Extract the "cabinet" group, scale it 2x, rotate it, then clean up
wrfm edit model.wrfm --extract-group cabinet \
  | wrfm transform - --scale 2 --rotate-y 45 \
  | wrfm edit - --clean \
  > cabinet-final.wrfm

# ...or pipe straight into the viewer
wrfm edit model.wrfm --extract-group cabinet \
  | wrfm transform - --scale 2 | wireforge -
```

## Exit codes

| code | meaning |
| :--- | :--- |
| `0` | result produced, clean (`ok` / `warn` / `verify: pass`) |
| `1` | result produced with an issue (`check: broken` / `verify: fail` — intent not met) |
| `2` | no result (usage / parse / I/O) |

## License

MIT.
