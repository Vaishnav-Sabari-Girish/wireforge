# wrfm-cli

A read-only, streaming command-line tool for `.wrfm` 3D wireframe models.
It validates, inspects, queries, views, renders, transforms, edits and diffs
models, all over plain stdio, with no files written and no TUI needed.

It consumes the [`wrfm`](../wrfm) parser library (the single authority for
syntax and edge-index range) and computes geometry with the same engine as
the [`wireforge`](../../) TUI, so scripts, editors and agents all get the
same numbers.

## Features

* **Verify intent, not just health.** `wrfm check` reports L2 health (`ok` / `warn` / `broken`) plus informational
  quality diagnostics; `wrfm verify` asserts DECLARED intent (size / center / closed / axis / symmetry / groups /
  redundant vertices) with per-expectation deltas and fix commands.
* **Facts before pixels.** `wrfm info` / `wrfm geometry` / `wrfm query` compute structured numbers; `wrfm view` gives
  exact per-view occlusion facts, so you can reason precisely without rendering. `wrfm group` prints per-part facts as
  JSON (`jq -r '.groups[] | "\(.name):\(.verdict)"'` for a one-line verdict list).
* **Character rendering.** `wrfm render` draws braille / ascii / grid to the terminal (six standard views,
  `--fit content` auto-framing, region zoom), using the same projection as the wireforge TUI.
* **Safe edits over streams.** `wrfm transform` (rigid: rotate / scale / shear / mirror / translate / pivot / align /
  normalize) and `wrfm edit` (topology: delete / extract / clean / dedupe / weld / merge) print the result to stdout,
  ready to chain with pipes and verify with `wrfm check`.
* **OBJ into the pipeline.** `wrfm convert` reads a Wavefront OBJ (a deliberate subset: `v` vertices, `f` face
  rings, `l` chains, with shared edges deduplicated) and prints canonical `.wrfm`. The input format is detected from
  the content, so stdin and files behave the same. Surface data (`vt`/`vn`/materials) is dropped and `o`/`g`
  groups are never invented.
* **Compare.** `wrfm diff` shows exactly what changed between two models (density-grid or structured JSON).

## Getting started

The repo ships sample models in `wrfm_files/`. Every subcommand takes a
path, or `-` for stdin, and prints its result to stdout. Nothing is ever
written to disk, so you keep a result with shell redirection.

```bash
# health check: ok / warn / broken
wrfm check wrfm_files/cube.wrfm
# ok: cube (8 vertices, 12 edges)

# first line only, for logs and scripts
wrfm check wrfm_files/cube.wrfm | head -1
# ok: cube (8 vertices, 12 edges)

# braille render straight to the terminal (six views by default)
wrfm render wrfm_files/cube.wrfm
wrfm render wrfm_files/cube.wrfm --views top
```

## Inspect a model

`wrfm info` / `wrfm geometry` / `wrfm query` print structured facts, as JSON
for scripting or plain text for reading:

```bash
wrfm info wrfm_files/cube.wrfm
```

```json
{
  "name": "cube",
  "version": 2,
  "vertices": 8,
  "edges": 12,
  "groups": [],
  "bytes": 227,
  "bounds": { "center": [0.0, 0.0, 0.0], "max": [1.0, 1.0, 1.0], "min": [-1.0, -1.0, -1.0], "size": [2.0, 2.0, 2.0] }
}
```

```bash
wrfm query wrfm_files/cube.wrfm profile
```

```text
profile (axis span vs edge-cover extent):
  x: span=2.000 edge_cover=[-1.000,1.000]
  y: span=2.000 edge_cover=[-1.000,1.000]
  z: span=2.000 edge_cover=[-1.000,1.000]
```

Queries: `profile | cross_section | vertices | distance | connectivity`.
For bounds, centroid, PCA axes, topology, edge stats, symmetry and
alignment add `wrfm geometry wrfm_files/cube.wrfm`.

## Verify intent

Assert what a model *should* be, such as its size, center, closedness,
axis, symmetry, groups, or an accepted redundant-vertex count, and get
pass/fail with deltas and a fix command:

```bash
wrfm verify wrfm_files/tetrahedron.wrfm --expect-size 2,2,2 --expect-closed
# verdict: pass   (exit code 0)

wrfm verify wrfm_files/tetrahedron.wrfm --expect-size 1,1,1
# verdict: fail   (exit code 2)
# suggestion: wrfm transform - --scale-x 0.5 && wrfm transform - --scale-y 0.5 && wrfm transform - --scale-z 0.5
```

`check` warns about *redundant* vertices, meaning degree-2 midpoints that
sit dead straight on the chord between their neighbours (degenerate data
like collapsed control rows; ordinary corners and closed loops are healthy).
`--expect-redundant N` declares the exact count you accept, so the gate
still catches a newly introduced one:

```bash
wrfm verify model.wrfm --expect-redundant 12
# verdict: pass   (exit code 0) — 12 degenerate midpoints are accepted
wrfm verify model.wrfm --expect-redundant 0
# verdict: fail   (exit code 2)
# suggestion: re-declare with the actual count: --expect-redundant 12
```

## Transform and edit over stdio

Rigid transforms (`wrfm transform`) and topology edits (`wrfm edit`) print
the resulting `.wrfm` text to stdout, so the original file is never
touched:

```bash
wrfm transform wrfm_files/cube.wrfm --scale 2 --rotate-y 45 > cube-2x.wrfm
wrfm edit wrfm_files/cube.wrfm --delete-vertices 0,1 > trimmed.wrfm
wrfm edit model.wrfm --extract-group cabinet > cabinet.wrfm

# Real-world data: two samples of the same curve differ by ~1e-15, which
# exact `--dedupe` cannot see. `--weld TOL` merges vertices strictly
# closer than TOL world units and performs the same edge cleanup
# (duplicate / zero-length edges dropped); the merged vertex keeps the
# FIRST group section that touched it. One edit op per call.
wrfm edit model.wrfm --weld 1e-6 > welded.wrfm
```

## Compare models

`wrfm diff` shows exactly what changed, as a density-grid in the terminal
or structured JSON with displacement vectors:

```bash
wrfm diff wrfm_files/cube.wrfm cube-2x.wrfm
# vertices: 8 -> 8 (+0)    edges: 12 -> 12 (+0)
# bbox:  min[-1.00,-1.00,-1.00] max[1.00,1.00,1.00]  ->  min[-2.00,-2.00,-2.00] max[2.00,2.00,2.00]

wrfm diff wrfm_files/cube.wrfm cube-2x.wrfm --format json
# moved vertices with per-vertex displacement and distance, added/removed edges
```

## Frame the shot, then rasterize

`--fit content` crops every view to the projected model so it fills the
canvas, computed per view so it works across `--views`. An explicit
`--region x0,y0,x1,y1` (normalized 0–1) always takes precedence over
`--fit`, and `--fit` only crops, it never moves the camera. Use
`--auto-dist` or `--dist` for that:

```bash
wrfm render model.wrfm --views front --fit content
# # wrfm render  format=braille  fit=content  canvas=60x24 chars ...
```

There is no built-in PNG encoder. `render` emits text, and the terminal
image is one pipe away. Braille text piped through ImageMagick defaults to
**16-bit** PNG, which some viewers reject, so pin the depth:

```bash
wrfm render model.wrfm --views front --width 96 --height 32 --fit content \
  | magick -background '#0d1117' -fill '#9cdcfe' -font DejaVu-Sans \
      -pointsize 16 label:@- -depth 8 -type TrueColor out.png
```

(`label:@-` reports a misleading *label expected '@-'* error when stdin is
empty, i.e. when `wrfm render` produced nothing; check the stage above
first.)

## Command reference

```bash
wrfm check model.wrfm                  # health: ok / warn / broken + quality diagnostics
wrfm verify model.wrfm --expect-closed # intent assertions (pass / fail)
wrfm info model.wrfm                   # metadata: counts, groups, bounding box
wrfm geometry model.wrfm               # bounds, PCA, topology, symmetry, alignment
wrfm query model.wrfm profile           # profile | cross_section | vertices | distance | connectivity
wrfm group model.wrfm [name]           # per-part facts as JSON (verdict, bounds, adjacency)
wrfm view model.wrfm --yaw 45          # exact per-view facts (occlusion, silhouette, depth)
wrfm render model.wrfm --views top     # braille / ascii / grid (--fit content, --region)
wrfm transform model.wrfm --scale 2    # affine transforms, printed to stdout
wrfm edit model.wrfm --delete-vertices 0,1   # topology edits (--dedupe / --weld TOL)
wrfm convert model.obj                 # OBJ -> wrfm (input format detected from the content)
wrfm diff a.wrfm b.wrfm --format json  # structured or density-grid diff
wrfm format                            # print the .wrfm v2 spec itself
```

## Composing commands with pipes

Every command is read-only and streaming. It reads a model from a file or
from `-` (stdin) and prints the result to stdout, so atomic operations
chain into complex transforms with pipes. Each stage can be verified with
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

# bring an OBJ into the toolchain, then view it like any other model
wrfm convert mouse.obj | wireforge -
```

## Recipes

Small pipe compositions for the things that used to be dedicated flags:

```bash
# one line of group verdicts (was `group --verdicts`)
wrfm group m.wrfm | jq -r '.groups[] | "\(.name):\(.verdict)"'
# rim:ok
# body:warn

# one-line check summary for logs (was `check --quiet`)
wrfm check m.wrfm | head -1
# warn: model (278 vertices, 529 edges)

# render a group as its own model (compare parts without touching the file)
wrfm edit m.wrfm --extract-group g | wrfm render -

# diff one group (was `diff --group`): extract it from both sides first
wrfm edit a.wrfm --extract-group g > a-g.wrfm
wrfm edit b.wrfm --extract-group g > b-g.wrfm
wrfm diff a-g.wrfm b-g.wrfm

# cap a view's edge list yourself (was `view --limit`)
wrfm view m.wrfm | jq '.visible_edges[:10]'
```

`render --detail` presets are plain options (documentation only):

| preset | equivalent |
| --- | --- |
| `overview` | `--format grid --width 40 --height 16` |
| `standard` | `--format grid --width 60 --height 24 --grid-w 32 --grid-h 16` |
| `fine` | `--format ascii --width 40 --height 16` |

## Exit codes

Four tiers, larger = more severe. Every subcommand shares them, so a
script can branch on the number alone:

| code | meaning |
| :--- | :--- |
| `0` | ok — result produced, health `ok` (`verify: pass`, `diff`, `format`, help) |
| `1` | warn — result produced with a warning-level issue (`check: warn`) |
| `2` | broken — repair is required (`check: broken`, `verify: fail`; `--strict` upgrades a warn to this) |
| `3` | no result — unreadable file, corrupt model, or usage error (including clap's own argument errors) |

`render` / `info` / `group` / `geometry` / `query` / `view` / `transform` /
`edit` / `convert` still emit their result on stdout and only the exit code
carries the health tier; `check` puts its report on stdout and exits 1 for
`warn`, 2 for `broken`.

```bash
wrfm check model.wrfm; echo "exit $?"
# warn: model (278 vertices, 529 edges)
# exit 1          # acceptable warning
# exit 2          # must fix
# exit 3          # could not even read/parse/understand the request
```

## License

MIT.
