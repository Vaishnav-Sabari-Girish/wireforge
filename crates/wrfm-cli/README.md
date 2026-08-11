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

## Usage

```bash
wrfm check model.wrfm          # health check (ok / warn / broken) + quality diagnostics
wrfm verify model.wrfm --expect-size 2,3,4 --expect-closed   # intent assertions (pass / fail)
wrfm info model.wrfm           # metadata: counts, groups, bounding box
wrfm geometry model.wrfm       # structured geometry facts (bounds/PCA/topology/symmetry)
wrfm query model.wrfm topology # safe geometric queries (extents/cross_section/vertices/...)
wrfm group model.wrfm          # per-part (group) facts
wrfm view model.wrfm --yaw 45  # exact per-view facts (occlusion, silhouette, depth)
wrfm render model.wrfm         # braille render (six views by default)
wrfm transform a --scale 2 > big.wrfm   # rigid transforms, printed to stdout
wrfm edit model.wrfm --delete-vertices 0,1 > cleaned.wrfm
wrfm diff a.wrfm b.wrfm        # structured or density-grid diff
```

`wrfm check` also reports informational QUALITY diagnostics (open edges,
proportion, Y-up orientation) that never affect the verdict. `wrfm verify`
asserts DECLARED intent (size / center / closed / axis / symmetry / groups)
and returns per-expectation pass/fail with the delta and an actionable fix
command (e.g. `wrfm transform - --scale-y 2`).

Every command accepts `-` for stdin, and nothing ever writes a file — save
with shell redirection. See `wrfm --help` and `wrfm format` for the
complete command set and the `.wrfm` v1 format spec.

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
