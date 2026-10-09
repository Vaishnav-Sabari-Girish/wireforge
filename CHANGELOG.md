
## Unreleased



### :rocket: New features

- **(wrfm)** .wrfm v2 format — magic line, counts header, groups, structured errors

- **(wireforge)** Drop file hot-reload and the inotify dependency

- **(viewer)** Read stdin when FILE is omitted

- **(cli)** Narrow degree-2 health to collinear midpoints

- **(cli)** Add obj -> wrfm conversion

- **(input)** Enable alternate keys for layout-independent chords

- **(wireforge)** Replace the x reload panel with a one-line status row


### :bug: Bug fixes

- **(workspace)** Pin the wrfm-raster version for publishing

- **(hooks)** Check every crate in the workspace

- **(rotation)** Read plain keys in the viewer's frame

- **(rotation)** Unify direction signs — body-frame keys, object-side views, yaw sign

- **(timer)** Fire due timers on the idle event path

- **(hooks)** Run cargo fmt/clippy per crate

- **(viewer)** Point the OBJ hint at `wrfm convert <file>`

- **(input)** Unify key handling and fix stuck holds

- **(input)** Regressions from the kitty keyboard protocol (41ab1c4)


### :recycle: Refactoring

- Own the model type and drop ratatui-wireframe

- **(wrfm)** Split lib.rs into model, parse, error and tests modules

- **(wrfm)** Drop parse_with strict mode and tidy docs

- Turn repo into a cargo workspace and extract shared wrfm-raster renderer

- **(cli)** Detect the convert input format from content

- **(viewer)** Drop the OBJ/ratty rendering path

- **(wrfm-cli)** Unify the rules, add --weld and --fit content


## v0.7.0 - 2026-08-19



### :rocket: New features

- Add wrfm-cli — streaming check/info/group/geometry/query/view/render/transform/edit/diff tool

- **(wireforge)** 6-DOF camera, HUD, stream input, event-driven render loop

- Add wrfm-cli — streaming check/info/group/geometry/query/view/render/transform/edit/diff tool

- **(wrfm)** .wrfm v1 format — magic line, counts header, groups, structured errors


### :zap: Performance

- Parallel projection/bounds, timer module, unified motion


### :art: Styling

- **(hud)** Revert help overlay arrows to ASCII for terminal compatibility


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

<!-- wireforge -->
