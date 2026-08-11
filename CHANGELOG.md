
## Unreleased

### :rocket: New features

- **(cli)** New `wrfm` CLI with check / info / group / geometry / query / view / render / transform / edit / diff / format subcommands
- **(format)** .wrfm v1 format: `wrfm 1` magic line, counts header, `group` sections
- **(format)** Structured parse errors with line / column / context (ParseError / LoadError)
- **(viewer)** 6-DOF camera: rotate (yaw / pitch / roll) and move, HUD, XYZ axes toggle
- **(viewer)** Stream input: stdin / FIFO one-shot previews
- **(viewer)** Content-first file detection via the `wrfm <version>` magic
- **(viewer)** Hot-reload status line (4 s) and `x` reload-status panel
- **(viewer)** Camera preserved across hot reloads

### :bug: Bug fixes

- **(viewer)** Restore mouse drag-selection (mouse capture off)
- **(viewer)** Resize no longer leaves a stale model on screen
- **(viewer)** Hide the cursor to stop flicker on every redraw
- **(viewer)** Piped stdin reads keyboard from the controlling terminal

### :zap: Performance

- **(viewer)** Event-driven render loop: 0% CPU while idle, uncapped animation
- **(viewer)** Owned braille renderer: cached projection, packed-cell diff, single-write present

### :recycle: Refactoring

- **(viewer)** Split the renderer into engine modules (render / reload / view)

### :hammer: Build

- **(workspace)** Add `wrfm-cli` as a workspace member
- **(deps)** Add rayon and Linux inotify support

### :art: Styling

- **(fmt)** Formatted

## v0.6.0 - 2026-06-13

### :rocket: New features

- **(3d)** 3D objects viewing using RGP

### :art: Styling

- **(fmt)** Formatted

## v0.5.0 - 2026-06-01

### :rocket: New features

- **(no_std)** Add no_std support for wrfm

- **(read_from_string)** Read data from string in `wrfm`

- **(viewer)** The viewer has been implemented

- **(initial)** Initial commit

### :art: Styling

- **(file)** Change wfrm to wrfm

### :hammer: Build

- **(publish)** Add LICENSE and details in Cargo.toml

<!-- ComChan -->
