
## Unreleased



### :rocket: New features

- **(wireforge)** Drop file hot-reload and the inotify dependency

- **(viewer)** Read stdin when FILE is omitted

- **(input)** Enable alternate keys for layout-independent chords

- **(wireforge)** Replace the x reload panel with a one-line status row

- Add wrfm-cli — streaming check/info/group/geometry/query/view/render/transform/edit/diff tool

- **(wireforge)** 6-DOF camera, HUD, stream input, event-driven render loop

- **(wrfm)** .wrfm v1 format — magic line, counts header, groups, structured errors

- **(3d)** 3D objects viewing using RGP

- **(no_std)** Add no_std support for wrfm

- **(viewer)** The viewer has been implemented

- **(initial)** Initial commit


### :bug: Bug fixes

- **(rotation)** Read plain keys in the viewer's frame

- **(rotation)** Unify direction signs — body-frame keys, object-side views, yaw sign

- **(timer)** Fire due timers on the idle event path

- **(hooks)** Run cargo fmt/clippy per crate

- **(viewer)** Point the OBJ hint at `wrfm convert <file>`

- **(input)** Unify key handling and fix stuck holds

- **(input)** Regressions from the kitty keyboard protocol (41ab1c4)


### :zap: Performance

- Parallel projection/bounds, timer module, unified motion


### :recycle: Refactoring

- Turn repo into a cargo workspace and extract shared wrfm-raster renderer

- **(cli)** Detect the convert input format from content

- **(viewer)** Drop the OBJ/ratty rendering path


### :art: Styling

- **(hud)** Revert help overlay arrows to ASCII for terminal compatibility

- **(fmt)** Formatted

- **(file)** Change wfrm to wrfm

<!-- wireforge -->
