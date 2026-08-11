# wrfm-cli

A read-only, streaming command-line tool for `.wrfm` 3D wireframe models:
validate, inspect, query, view, render, transform, edit and diff — all over
plain stdio, no files written, no TUI needed.

It consumes the [`wrfm`](../wrfm) parser library (the single authority for
syntax and edge-index range) and computes geometry with the same engine as
the [`wireforge`](../../) TUI, so scripts, editors and agents all get the
same numbers.

## Usage

```bash
wrfm check model.wrfm          # health check (ok / warn / broken)
wrfm info model.wrfm           # metadata: counts, groups, bounding box
wrfm render model.wrfm         # braille render (six views by default)
wrfm transform a --scale 2 > big.wrfm   # rigid transforms, printed to stdout
wrfm edit model.wrfm --delete-vertices 0,1 > cleaned.wrfm
wrfm diff a.wrfm b.wrfm        # structured or density-grid diff
```

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
| `0` | result produced, clean (`ok` / `warn`) |
| `1` | result produced but `check: broken` |
| `2` | no result (usage / parse / I/O) |

## License

MIT.
