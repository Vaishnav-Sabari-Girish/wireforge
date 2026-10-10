use clap::Parser;
use crossterm::{
    cursor::{Hide, Show},
    event::{
        self, DisableFocusChange, EnableFocusChange, Event, KeyCode, KeyEventKind, KeyModifiers,
        KeyboardEnhancementFlags, ModifierKeyCode, PopKeyboardEnhancementFlags,
        PushKeyboardEnhancementFlags,
    },
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Padding, Paragraph, Widget},
};

use std::{
    collections::HashMap,
    error::Error,
    io::{self, IsTerminal, Read},
    path::{Path, PathBuf},
    sync::OnceLock,
    sync::mpsc::{self, Sender},
    thread,
    time::{Duration, Instant},
};
use wrfm::WrfmModel;
use wrfm_raster::Model;

mod render;
mod timer;
mod view;
use timer::{TimerId, TimerScheduler};
use view::ViewState;

// Speed constants are dyadic fractions (exact in binary) with ~1.3x
// translation / ~1.1x rotation speedup over the upstream values.

/// Smooth continuous rotation rate (radians per second) while a key is held.
const ROT_RATE: f64 = 169.0 / 128.0;
/// Smooth continuous translation rate: fraction of the model extent moved per second.
const MOVE_RATE: f64 = 83.0 / 128.0;

/// Auto-spin yaw rate (radians per second): 169/256 = 0.66015625, a dyadic (exact-in-binary) constant.
const SPIN_RATE: f64 = 169.0 / 256.0;
/// How often the idle loop re-checks the terminal size (resize fallback).
const RESIZE_CHECK_INTERVAL: Duration = Duration::from_millis(500);
/// How long the keyboard may stay silent before a hold is dropped, on a
/// terminal that never reports a key-up. Silence is the only evidence there
/// is; 1 s outlasts the default OS initial repeat delay (660 ms on X11) and
/// keeps a tap's drift short. Terminals that DO report Release never use it.
const LEGACY_HOLD_TIMEOUT: Duration = Duration::from_secs(1);

#[derive(Parser, Debug)]
#[command(
    name = "wireforge",
    author,
    version,
    about = "TUI viewer for .wrfm 3D wireframe models"
)]
struct Args {
    /// `.wrfm` file to open, or `-` to read from stdin (the default when stdin is
    /// not a terminal). Omit it on a terminal to open an empty canvas
    file: Option<PathBuf>,
}

/// HUD folding state: `?` toggles Collapsed <-> Expanded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Hud {
    Collapsed,
    Expanded,
}

/// One continuous degree of freedom: rotation (yaw / pitch / roll) and translation (pan / dolly).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Motion {
    // Rotation (world-frame: fixed world axes; h/l/d/f read left/right as
    // the viewer sees them — the model faces out of the screen).
    YawLeft,
    YawRight,
    PitchUp,
    PitchDown,
    RollPlus,
    RollMinus,
    // Rotation (local-frame: the model's own axes and its own left/right).
    LocalYawLeft,
    LocalYawRight,
    LocalPitchUp,
    LocalPitchDown,
    LocalRollPlus,
    LocalRollMinus,
    // Translation (absolute world coordinates).
    MoveLeft,
    MoveRight,
    MoveUp,
    MoveDown,
    MoveForward,
    MoveBack,
}

/// What one key press does. `Motion` keys are continuous (held until Release,
/// FocusLost or the legacy timeout); every other action fires once per press.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Action {
    Quit,
    Help,
    Spin,
    Axes,
    Reset,
    Motion(Motion),
}

/// The flag a bare modifier key raises: `LeftControl` and `RightControl` are
/// one `CONTROL`, and a chord never cares which hand pressed it.
///
/// The two ISO level shifts are layout keys rather than modifiers a chord can
/// wear, so they raise nothing.
fn modifier_flag(m: ModifierKeyCode) -> KeyModifiers {
    match m {
        ModifierKeyCode::LeftShift | ModifierKeyCode::RightShift => KeyModifiers::SHIFT,
        ModifierKeyCode::LeftControl | ModifierKeyCode::RightControl => KeyModifiers::CONTROL,
        ModifierKeyCode::LeftAlt | ModifierKeyCode::RightAlt => KeyModifiers::ALT,
        ModifierKeyCode::LeftSuper | ModifierKeyCode::RightSuper => KeyModifiers::SUPER,
        ModifierKeyCode::LeftHyper | ModifierKeyCode::RightHyper => KeyModifiers::HYPER,
        ModifierKeyCode::LeftMeta | ModifierKeyCode::RightMeta => KeyModifiers::META,
        ModifierKeyCode::IsoLevel3Shift | ModifierKeyCode::IsoLevel5Shift => KeyModifiers::empty(),
    }
}

/// Fold one terminal key event into the key's identity (its *base*, un-shifted
/// form) plus whether SHIFT is in effect, so every later decision sees one
/// protocol-independent shape instead of two:
///
/// * the kitty protocol sends the layout's **shifted glyph** for every key
///   that has an alternate (`REPORT_ALTERNATE_KEYS` is pushed): `Shift+h` ->
///   `Char('H')`, `Shift+-` -> `Char('_')` — crossterm applies the alternate
///   and clears SHIFT. Keys without an alternate keep the modifier instead
///   (`Shift+Left` -> `Left` + SHIFT, `Shift+Space` -> `Char(' ')` + SHIFT);
/// * a legacy terminal sends the shifted **glyph** too, and crossterm
///   additionally synthesises SHIFT for upper-case letters (`Shift+h` ->
///   `Char('H')` + SHIFT, `Shift+/` -> `Char('?')` with NO modifier at all).
///
/// So an upper-case char implies shift, a known shifted glyph folds back to
/// its base key + shift, and anything else keeps the SHIFT modifier. The glyph
/// table maps the three shifted glyphs the bindings use back to their US base
/// keys — the layouts' own glyphs arrive unchanged, so the same table serves
/// both paths. Caps lock therefore counts as shift on the legacy path and is
/// a no-op under kitty (crossterm files the kitty caps-lock bit under
/// `KeyEventState`, which is never read here) — the two cannot be told apart
/// from here.
const fn canonical_key(code: KeyCode, mods: KeyModifiers) -> (KeyCode, bool) {
    use KeyCode::Char;
    let shift = mods.contains(KeyModifiers::SHIFT);
    // Const-evaluable throughout: the keymap is folded at compile time, so
    // this may not reach for Unicode tables (`is_uppercase`) or allocating
    // case folding (`to_lowercase`). Every chord is ASCII.
    match code {
        Char(c) if c.is_ascii_uppercase() => (Char(c.to_ascii_lowercase()), true),
        Char('?') => (Char('/'), true),
        Char('_') => (Char('-'), true),
        Char('+') => (Char('='), true),
        // BackTab *is* Shift+Tab: the legacy encoding reports it as its own
        // key with no modifier, the kitty one keeps SHIFT. Folding it to the
        // shifted form lets one chord cover both.
        KeyCode::BackTab => (KeyCode::BackTab, true),
        // Space and Esc are the two keys a terminal cannot report SHIFT for:
        // the legacy encoding sends the same bytes either way, so treating the
        // shifted spelling as a different key could only make the two terminal
        // paths disagree about a key that is in fact the same one.
        Char(' ') | KeyCode::Esc => (code, false),
        _ => (code, shift),
    }
}

/// The task a binding belongs to. Grouping is by what the operator wants to
/// *do*, never by which modifier the chord wears: "how do I move it?" is a
/// question about the task, while "what does Shift do?" is a question about
/// the parser. The statusline's modifier hints key off this; the help overlay
/// is written by hand and does not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Group {
    /// Turning the model on the world's axes (h / l / k / j / r / e).
    Rotate,
    /// Sliding the model in the world's plane (Shift + hjkl, = / -).
    Move,
    /// Turning the model on its own axes (Ctrl + hjkl / r / e).
    Local,
    /// Where the model sits on screen (c).
    Frame,
    /// Session control (Space / Tab / ? / q).
    Session,
}

/// One way to press a binding: the terminal key event *as the terminal
/// reports it*, plus the label shown to the operator.
///
/// `code` / `mods` are the legacy (no kitty protocol) encoding, which is the
/// more explicit of the two: a shifted letter arrives as its upper-case glyph
/// **with** SHIFT still set, and a shifted symbol as that symbol (`?`, `+`,
/// `_`) with no modifier. Feeding these through `canonical_key` folds them
/// back to the documented chord, so the cross-check tests can prove each
/// binding resolves to the action it claims. The kitty encoding is covered
/// separately by `resolve_key_event_folds_both_terminal_encodings`.
///
/// `label` follows one convention everywhere: modifiers in `Ctrl+Shift+`
/// order, then the key — lower-case for a bare letter (`h`), upper-case when
/// SHIFT is part of the chord (`Shift+H`), verbatim for named keys
/// (`Left`, `Space`, `Tab`).
/// A chord is plain data, small enough to copy: the compile-time keymap
/// indexes chords by value.
#[derive(Clone, Copy)]
struct Chord {
    code: KeyCode,
    mods: KeyModifiers,
}

/// A chord with no modifier.
const fn key(code: KeyCode) -> Chord {
    Chord {
        code,
        mods: KeyModifiers::NONE,
    }
}

/// `Shift` + `code`.
const fn shift(code: KeyCode) -> Chord {
    Chord {
        code,
        mods: KeyModifiers::SHIFT,
    }
}

/// `Ctrl` + `code`.
const fn ctrl(code: KeyCode) -> Chord {
    Chord {
        code,
        mods: KeyModifiers::CONTROL,
    }
}

/// One binding: an action, the task it belongs to, and every way to press it.
///
/// This table is what the statusline hints read and — via [`keymap`] — what
/// the resolver itself is built from. It is deliberately *not* what the help
/// overlay is drawn from: that page is written out by hand under
/// [`HELP_LEFT`] / [`HELP_RIGHT`], and the cross-check test is what keeps the
/// two honest about each other.
#[derive(Clone, Copy)]
struct Binding {
    action: Action,
    group: Group,
    /// Every documented way to press it.
    chords: &'static [Chord],
}

/// Every binding, in the order the statusline hints list them. This is the
/// keymap's one source: the lookup the viewer resolves against is built from
/// it by the compiler, so a binding cannot exist without a key to press, and a
/// key cannot be pressed without a binding.
const BINDINGS: &[Binding] = &[
    // --- Rotate: the world's axes ---
    Binding {
        action: Action::Motion(Motion::YawLeft),
        group: Group::Rotate,
        chords: &[key(KeyCode::Char('h')), key(KeyCode::Left)],
    },
    Binding {
        action: Action::Motion(Motion::YawRight),
        group: Group::Rotate,
        chords: &[key(KeyCode::Char('l')), key(KeyCode::Right)],
    },
    Binding {
        action: Action::Motion(Motion::PitchUp),
        group: Group::Rotate,
        chords: &[key(KeyCode::Char('k')), key(KeyCode::Up)],
    },
    Binding {
        action: Action::Motion(Motion::PitchDown),
        group: Group::Rotate,
        chords: &[key(KeyCode::Char('j')), key(KeyCode::Down)],
    },
    Binding {
        action: Action::Motion(Motion::RollPlus),
        group: Group::Rotate,
        chords: &[key(KeyCode::Char('d'))],
    },
    Binding {
        action: Action::Motion(Motion::RollMinus),
        group: Group::Rotate,
        chords: &[key(KeyCode::Char('f'))],
    },
    // --- Move: slide in the world's plane ---
    Binding {
        action: Action::Motion(Motion::MoveLeft),
        group: Group::Move,
        chords: &[shift(KeyCode::Char('H')), shift(KeyCode::Left)],
    },
    Binding {
        action: Action::Motion(Motion::MoveRight),
        group: Group::Move,
        chords: &[shift(KeyCode::Char('L')), shift(KeyCode::Right)],
    },
    Binding {
        action: Action::Motion(Motion::MoveUp),
        group: Group::Move,
        chords: &[shift(KeyCode::Char('K')), shift(KeyCode::Up)],
    },
    Binding {
        action: Action::Motion(Motion::MoveDown),
        group: Group::Move,
        chords: &[shift(KeyCode::Char('J')), shift(KeyCode::Down)],
    },
    Binding {
        action: Action::Motion(Motion::MoveForward),
        group: Group::Move,
        // `+` is how several layouts spell the shifted `=`; it folds to `=`.
        chords: &[key(KeyCode::Char('=')), key(KeyCode::Char('+'))],
    },
    Binding {
        action: Action::Motion(Motion::MoveBack),
        group: Group::Move,
        // `_` is the shifted `-` on several layouts; it folds to `-`.
        chords: &[key(KeyCode::Char('-')), key(KeyCode::Char('_'))],
    },
    // --- Local: the model's own axes ---
    Binding {
        action: Action::Motion(Motion::LocalYawLeft),
        group: Group::Local,
        chords: &[ctrl(KeyCode::Char('h')), ctrl(KeyCode::Left)],
    },
    Binding {
        action: Action::Motion(Motion::LocalYawRight),
        group: Group::Local,
        chords: &[ctrl(KeyCode::Char('l')), ctrl(KeyCode::Right)],
    },
    Binding {
        action: Action::Motion(Motion::LocalPitchUp),
        group: Group::Local,
        chords: &[ctrl(KeyCode::Char('k')), ctrl(KeyCode::Up)],
    },
    Binding {
        action: Action::Motion(Motion::LocalPitchDown),
        group: Group::Local,
        chords: &[ctrl(KeyCode::Char('j')), ctrl(KeyCode::Down)],
    },
    Binding {
        action: Action::Motion(Motion::LocalRollPlus),
        group: Group::Local,
        chords: &[ctrl(KeyCode::Char('d'))],
    },
    Binding {
        action: Action::Motion(Motion::LocalRollMinus),
        group: Group::Local,
        chords: &[ctrl(KeyCode::Char('f'))],
    },
    // --- Frame: where it sits on screen ---
    Binding {
        action: Action::Reset,
        group: Group::Frame,
        chords: &[key(KeyCode::Char('c'))],
    },
    // --- Session ---
    Binding {
        action: Action::Spin,
        group: Group::Session,
        chords: &[key(KeyCode::Char(' '))],
    },
    Binding {
        action: Action::Axes,
        group: Group::Session,
        chords: &[
            key(KeyCode::Tab),
            // A legacy terminal reports Shift+Tab as BackTab; kitty reports
            // Tab with SHIFT. BackTab is the spelling both paths agree on.
            shift(KeyCode::BackTab),
        ],
    },
    Binding {
        action: Action::Help,
        group: Group::Session,
        chords: &[key(KeyCode::Char('?'))],
    },
    Binding {
        action: Action::Quit,
        group: Group::Session,
        chords: &[
            key(KeyCode::Char('q')),
            key(KeyCode::Esc),
            ctrl(KeyCode::Char('c')),
            // Raw mode turns Ctrl+C and Ctrl+Q into plain key events (no
            // SIGINT), so both are ours to bind.
            ctrl(KeyCode::Char('q')),
        ],
    },
];

impl Chord {
    /// The label as shown to the operator, derived from the chord itself so
    /// it can never disagree with the key it names. Modifiers come first in
    /// `Ctrl+Shift+` order, then the key: lower-case for a bare letter (`h`),
    /// upper-case when SHIFT is part of the chord (`Shift+H`), a plain glyph
    /// for symbols, and the key's name for named keys (`Space`, `Left`).
    fn label(&self) -> String {
        let mut out = String::new();
        if self.mods.contains(KeyModifiers::CONTROL) {
            out.push_str("Ctrl+");
        }
        if self.mods.contains(KeyModifiers::SHIFT) {
            out.push_str("Shift+");
        }
        match self.code {
            KeyCode::Char(' ') => out.push_str("Space"),
            KeyCode::Char(c) => out.push(c),
            KeyCode::Esc => out.push_str("Esc"),
            KeyCode::Tab => out.push_str("Tab"),
            // BackTab IS Shift+Tab; spelling the modifier again would double it.
            KeyCode::BackTab => out.push_str(if self.mods.contains(KeyModifiers::SHIFT) {
                "Tab"
            } else {
                "BackTab"
            }),
            KeyCode::Left => out.push_str("Left"),
            KeyCode::Right => out.push_str("Right"),
            KeyCode::Up => out.push_str("Up"),
            KeyCode::Down => out.push_str("Down"),
            other => out.push_str(&format!("{other:?}")),
        }
        out
    }
}

/// Resolve a terminal key event to the key's identity and the action it fires.
///
/// A lookup, not a list: the keymap was built from [`BINDINGS`] by the
/// compiler, so a rebind is one edit that the resolver cannot be left behind
/// by. The identity that comes back is what a hold is filed under, so a
/// Release with different modifiers still finds the motion it started.
fn resolve_key_event(code: KeyCode, mods: KeyModifiers) -> Option<(KeyCode, Action)> {
    // A chord is a key plus at most Shift and Ctrl: Alt, Super, Hyper and Meta
    // bind nothing, whatever the key is.
    let binds = KeyModifiers::SHIFT.union(KeyModifiers::CONTROL);
    if !mods.difference(binds).is_empty() {
        return None;
    }
    let (base, shift) = canonical_key(code, mods);
    let mut want = if shift {
        KeyModifiers::SHIFT
    } else {
        KeyModifiers::empty()
    };
    if mods.contains(KeyModifiers::CONTROL) {
        want = want.union(KeyModifiers::CONTROL);
    }
    KEYMAP
        .iter()
        .find(|entry| entry.base == base && want.intersection(entry.mask) == entry.mods)
        .map(|entry| (base, entry.action))
}

/// One entry of the keymap: a chord's canonical identity, and what it does.
///
/// The identity is what [`canonical_key`] folds a terminal event to, so the
/// chord *as written in the table* and the event *as the terminal reports it*
/// meet on the same value — that is what lets the table be built ahead of time
/// instead of matched by hand.
#[derive(Clone, Copy)]
struct KeymapEntry {
    /// The key's identity, which is also the id a hold is filed under.
    base: KeyCode,
    /// Which modifier bits this chord cares about, and which it requires.
    ///
    /// A Ctrl chord ignores Shift: `Ctrl+Shift+h` is `Ctrl+h`, because the
    /// shift is not part of what the operator meant to press. Every other
    /// chord is strict, because there `Shift` *is* the difference between
    /// rotating and panning.
    mask: KeyModifiers,
    mods: KeyModifiers,
    action: Action,
}

/// How many chords the keymap holds: every chord of every binding.
const fn chord_count() -> usize {
    let mut n = 0;
    let mut b = 0;
    while b < BINDINGS.len() {
        n += BINDINGS[b].chords.len();
        b += 1;
    }
    n
}

/// Flatten [`BINDINGS`] into the keymap, folding each chord exactly the way an
/// incoming event is folded.
///
/// This runs in the compiler. The viewer therefore has no key list to
/// initialise, no lookup to build and nothing to keep in step: the declaration
/// above *is* the keymap.
const fn keymap() -> [KeymapEntry; chord_count()] {
    let mut out = [KeymapEntry {
        base: KeyCode::Null,
        mask: KeyModifiers::empty(),
        mods: KeyModifiers::empty(),
        action: Action::Quit,
    }; chord_count()];
    let mut written = 0;
    let mut b = 0;
    while b < BINDINGS.len() {
        let binding = BINDINGS[b];
        let mut c = 0;
        while c < binding.chords.len() {
            let chord = binding.chords[c];
            let (base, shift) = canonical_key(chord.code, chord.mods);
            let control = chord.mods.contains(KeyModifiers::CONTROL);
            let mut mods = if shift {
                KeyModifiers::SHIFT
            } else {
                KeyModifiers::empty()
            };
            if control {
                mods = mods.union(KeyModifiers::CONTROL);
            }
            out[written] = KeymapEntry {
                base,
                mask: if control {
                    KeyModifiers::CONTROL
                } else {
                    KeyModifiers::SHIFT.union(KeyModifiers::CONTROL)
                },
                mods,
                action: binding.action,
            };
            written += 1;
            c += 1;
        }
        b += 1;
    }
    out
}

/// The whole keymap, ready in read-only memory before `main` runs.
const KEYMAP: [KeymapEntry; chord_count()] = keymap();

/// One frame of smooth continuous motion for a held key (model follows key).
fn continuous_step(view: &mut ViewState, m: Motion, scale: f64, dt: f64) {
    apply_motion_step(view, m, ROT_RATE * dt, scale * MOVE_RATE * dt);
}

/// Apply one motion step: rotation around the world axes (or, local-frame,
/// the model's own axes), pan in world X/Y, dolly along the view axis.
fn apply_motion_step(view: &mut ViewState, m: Motion, rot: f64, mv: f64) {
    match m {
        // World-frame rotation: yaw/pitch pre-multiply the model->world
        // matrix with a fixed world-axis rotation, so the axes are anchored
        // to the world and never follow the model (no gimbal collapse).
        // The SENSE of the plain keys is the viewer's: the model faces out
        // of the screen while the sight line points into it, so plain h
        // sweeps the nose the way the viewer reads "left" (screen-left =
        // the model's own right) — the mirror of Ctrl+h, which yaws the
        // model to its own left. Pitch has no mirror (the model's up is
        // the viewer's up), so k/j read the same in both frames.
        Motion::YawLeft => view.add_yaw(-rot),
        Motion::YawRight => view.add_yaw(rot),
        Motion::PitchUp => view.add_pitch(-rot),
        Motion::PitchDown => view.add_pitch(rot),
        // View-frame roll: `d` rolls about the sight line (into the
        // screen) in the viewer's sense — the viewer's right side (screen
        // right) dips, i.e. the image turns clockwise; `f` is the mirror.
        // That axis is anti-parallel to the model's front (+Z out of the
        // screen), so plain d and Ctrl+d read as mirror images at the
        // default view: Ctrl+d keeps the body reading (starboard dips).
        Motion::RollPlus => view.roll -= rot,
        Motion::RollMinus => view.roll += rot,
        // Local-frame rotation: the same step post-multiplied, so the axis
        // rides with the model. Local roll lives in the rotation matrix (see
        // ViewState::add_roll_local), never in the screen-space `roll`.
        Motion::LocalYawLeft => view.add_yaw_local(rot),
        Motion::LocalYawRight => view.add_yaw_local(-rot),
        Motion::LocalPitchUp => view.add_pitch_local(-rot),
        Motion::LocalPitchDown => view.add_pitch_local(rot),
        Motion::LocalRollPlus => view.add_roll_local(rot),
        Motion::LocalRollMinus => view.add_roll_local(-rot),
        // Pan shifts the model in world X/Y at any orientation; the rotation
        // centre is the panned file origin (see view::project_point).
        Motion::MoveLeft => view.pan_x -= mv,
        Motion::MoveRight => view.pan_x += mv,
        Motion::MoveUp => view.pan_y += mv,
        Motion::MoveDown => view.pan_y -= mv,
        // Translation along the view axis (camera distance).
        Motion::MoveForward => view.add_dist_delta(-mv),
        Motion::MoveBack => view.add_dist_delta(mv),
    }
    // Angles are periodic; keep the displayed values in [-PI, PI].
    view.normalize();
}

/// How many leading bytes are probed to detect the file format.
const PROBE_BYTES: usize = 4096;

/// True when `path` is a FIFO (named pipe, e.g. bash's `<( cmd )` process substitution).
#[cfg(unix)]
fn is_fifo_path(path: &Path) -> bool {
    use std::os::unix::fs::FileTypeExt;
    std::fs::metadata(path)
        .map(|m| m.file_type().is_fifo())
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_fifo_path(_path: &Path) -> bool {
    false
}

/// Verify a controlling terminal is available when stdin is a pipe.
#[cfg(unix)]
fn ensure_keyboard_terminal() -> Result<(), String> {
    if std::io::stdin().is_terminal() {
        return Ok(());
    }
    // stdin is not a terminal: keyboard must come from /dev/tty.
    std::fs::OpenOptions::new()
 .read(true)
 .write(true)
        .open("/dev/tty")
 .map(|_| ())
 .map_err(|e| {
 format!(
                "cannot open a terminal for keyboard input (/dev/tty): {e}\n'wireforge -' with stdin piped needs a controlling terminal to be interactive.\nUse a FIFO so the terminal stays free for keyboard:\n    wireforge <( cat model.wrfm )"
 )
 })
}

#[cfg(not(unix))]
fn ensure_keyboard_terminal() -> Result<(), String> {
    Ok(())
}

/// Probe a byte buffer: `.wrfm` magic passes, OBJ content gets a pointer
/// to the converter, everything else is unrecognized. The magic always
/// wins (obj-looking markers later in the head do not matter).
fn probe_bytes(buf: &[u8]) -> Result<(), String> {
    // Lossy: a stray non-UTF-8 byte must never decide the format.
    let head = String::from_utf8_lossy(buf);
    // Strip a leading BOM like the wrfm parser does, so a BOM before the
    // magic line does not hide it.
    let head = head.strip_prefix('\u{feff}').unwrap_or(&head);

    // Magic first: the first line of a v2 wrfm file is `wrfm <version>`.
    // The magic is a short line, so it always fits within PROBE_BYTES.
    let first_line = head.lines().next().unwrap_or("");
    if first_line.split_whitespace().next() == Some("wrfm") {
        return Ok(());
    }

    if looks_like_obj(head) {
        return Err(
            "OBJ content: wireforge reads .wrfm only -- convert it first: \
             `wrfm convert <file> | wireforge -`"
                .to_string(),
        );
    }
    Err(
        "unrecognized file format: no wrfm magic line (`wrfm <version>`). \
         Got an OBJ file? Convert it first: `wrfm convert <file>`"
            .to_string(),
    )
}

/// True for the OBJ marker shapes the converter accepts: real markers
/// (`f`/`vt`/`vn`), or vertices without wrfm edges (a bare point cloud).
fn looks_like_obj(head: &str) -> bool {
    let mut has_vertex = false;
    let mut has_wrfm_edge = false;
    let mut has_obj_marker = false;
    for raw in head.lines() {
        let line = raw.trim_start();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        match line.split_whitespace().next() {
            Some("v") => has_vertex = true,
            Some("e") => has_wrfm_edge = true,
            Some("f") | Some("vt") | Some("vn") => has_obj_marker = true,
            _ => {}
        }
    }
    has_obj_marker || (has_vertex && !has_wrfm_edge)
}

/// Probe the file's content; errors are path-prefixed.
fn probe_format(target_file: &Path) -> Result<(), String> {
    let mut buf = [0u8; PROBE_BYTES];
    let mut file = std::fs::File::open(target_file)
        .map_err(|e| format!("cannot open '{}': {e}", target_file.display()))?;
    let n = file
        .read(&mut buf)
        .map_err(|e| format!("cannot read '{}': {e}", target_file.display()))?;
    probe_bytes(&buf[..n]).map_err(|e| format!("'{}': {e}", target_file.display()))
}

/// Validate the stream as `.wrfm`, then parse the file.
fn load_model(target_file: &Path) -> Result<(Model, String), String> {
    probe_format(target_file)?;
    let wrfm_data = WrfmModel::from_file(target_file).map_err(|e| e.to_string())?;
    let model = Model {
        vertices: wrfm_data.vertices,
        edges: wrfm_data.edges,
    };
    Ok((model, wrfm_data.name))
}

/// Validate a stream (stdin / FIFO) as `.wrfm`, then parse it; `name` labels it.
fn load_model_from_text(name: &str, text: &str) -> Result<(Model, String), String> {
    probe_bytes(text.as_bytes())?;
    let wrfm_data = WrfmModel::from_str(name, text).map_err(|e| e.to_string())?;
    let model = Model {
        vertices: wrfm_data.vertices,
        edges: wrfm_data.edges,
    };
    Ok((model, wrfm_data.name))
}

/// Row 0 of the HUD: the camera telemetry line, and nothing else.
///
/// The model's name is not repeated here — it leads the [`status_line`], which
/// is where the eye already goes for "what am I looking at", and one fact
/// belongs in one place. What is left is deliberately *only* telemetry: this
/// line is routinely wider than the terminal, so anything appended to it (as
/// the old `[?] keys` hint was) is the first thing clipped.
fn telemetry_line(view: &ViewState) -> String {
    format!(
        "yaw={:.2} pitch={:.2} roll={:.2} dist={:.2} pan=({:.2},{:.2})",
        view.yaw, view.pitch, view.roll, view.dist, view.pan_x, view.pan_y
    )
}

/// The order the statusline's modifier hints list their groups in. This is the
/// *hints'* split, not the overlay's: the help page has its own hand-written
/// columns below and reads nothing from here.
const HINT_GROUPS: &[&[Group]] = &[
    &[Group::Rotate, Group::Move],
    &[Group::Local, Group::Frame, Group::Session],
];

// ---------------------------------------------------------------------------
// The help overlay
// ---------------------------------------------------------------------------

/// One row of the overlay: `(chord, doc)` — the chord as the operator reads
/// it, and what it does. Written by hand — nothing derives these from
/// [`BINDINGS`], so the page says only what its author typed, and
/// `help_overlay_lists_every_binding_once` is what proves the two have not
/// drifted apart.
#[derive(Clone, Copy)]
struct HelpRow(&'static str, &'static str);

/// One group of the overlay: `(heading, rows)`. The frame is named on every
/// heading, so no reader has to inherit it from a section header the way the
/// old modifier-grouped overlay demanded.
#[derive(Clone, Copy)]
struct HelpGroup(&'static str, &'static [HelpRow]);

/// The overlay's left column: Rotate and Move, the world-frame pair.
const HELP_LEFT: &[HelpGroup] = &[
    HelpGroup(
        "ROTATE  world axes",
        &[
            HelpRow("h", "yaw left"),
            HelpRow("l", "yaw right"),
            HelpRow("k", "pitch up"),
            HelpRow("j", "pitch down"),
            HelpRow("d", "roll clockwise"),
            HelpRow("f", "roll anticlockwise"),
        ],
    ),
    HelpGroup(
        "MOVE  world axes",
        &[
            HelpRow("Shift+H", "pan left"),
            HelpRow("Shift+L", "pan right"),
            HelpRow("Shift+K", "pan up"),
            HelpRow("Shift+J", "pan down"),
            HelpRow("=", "dolly nearer"),
            HelpRow("-", "dolly farther"),
        ],
    ),
];

/// The overlay's right column: Local mirrors Rotate on the model's own axes,
/// so it sits opposite it; Frame and Session are short and stack under it.
const HELP_RIGHT: &[HelpGroup] = &[
    HelpGroup(
        "ROTATE  the model's own axes",
        &[
            HelpRow("Ctrl+h", "yaw to its own left"),
            HelpRow("Ctrl+l", "yaw to its own right"),
            HelpRow("Ctrl+k", "pitch up"),
            HelpRow("Ctrl+j", "pitch down"),
            HelpRow("Ctrl+d", "roll clockwise"),
            HelpRow("Ctrl+f", "roll anticlockwise"),
        ],
    ),
    HelpGroup("FRAME", &[HelpRow("c", "reset the view")]),
    HelpGroup(
        "SESSION",
        &[
            HelpRow("Space", "toggle spinning"),
            HelpRow("Tab", "toggle the XYZ axes"),
            HelpRow("?", "toggle this help"),
            HelpRow("q", "quit"),
        ],
    ),
];

/// One column of the overlay: a heading per group and one `chord  doc` row per
/// binding, with the chord column padded to a width *measured from the data*
/// rather than hand-counted spaces.
///
/// The rows are the hand-written ones above; only the padding is computed.
/// Aliases are not listed at all, which is what keeps the overlay inside an
/// 80-column terminal.
fn help_column(groups: &[HelpGroup]) -> Vec<Line<'static>> {
    let chord_w = groups
        .iter()
        .flat_map(|HelpGroup(_, rows)| rows.iter())
        .map(|HelpRow(chord, _)| chord.len())
        .max()
        .unwrap_or(0);

    let mut lines = Vec::new();
    for &HelpGroup(title, rows) in groups {
        lines.push(Line::from(Span::styled(
            title,
            Style::default().fg(Color::Yellow),
        )));
        for &HelpRow(chord, doc) in rows {
            lines.push(Line::from(vec![
                // Keys are cyan and labels light blue, so the eye can scan a
                // column of chords without reading the prose.
                Span::styled(
                    format!("  {chord:<chord_w$}"),
                    Style::default().fg(Color::Cyan),
                ),
                Span::styled(format!("  {doc}"), Style::default().fg(Color::LightBlue)),
            ]));
        }
        lines.push(Line::default());
    }
    lines.pop(); // no trailing gap after the last group
    lines
}

/// The overlay, built once for the life of the process.
///
/// Its content is a pure function of [`HELP_LEFT`] / [`HELP_RIGHT`] — no
/// width, no state, no clock — so there is nothing to rebuild between frames,
/// and the panel centres itself and clips at the edges instead of reflowing.
/// Building it costs about as much as rasterizing a small model, which is not
/// a price to pay per frame for a view that cannot have changed.
fn help_lines() -> &'static [Line<'static>] {
    static LINES: OnceLock<Vec<Line<'static>>> = OnceLock::new();
    LINES.get_or_init(build_help_lines)
}

/// The full help overlay as styled lines: two columns of task groups, each
/// column's chord gutter measured from the data.
fn build_help_lines() -> Vec<Line<'static>> {
    let left = help_column(HELP_LEFT);
    let right = help_column(HELP_RIGHT);
    let left_w = left.iter().map(Line::width).max().unwrap_or(0);

    let mut out = Vec::with_capacity(left.len().max(right.len()));
    for i in 0..left.len().max(right.len()) {
        let mut spans = left.get(i).map_or_else(Vec::new, |l| l.spans.clone());
        // Pad the left column to its widest line so the right column starts in
        // one place, then a fixed gap. A line with no left content contributes
        // nothing, so pad by what is actually present.
        // All help text is ASCII, so byte length == display width.
        let have: usize = spans.iter().map(|s| s.content.len()).sum();
        spans.push(Span::raw(" ".repeat(left_w.saturating_sub(have) + 2)));
        if let Some(r) = right.get(i) {
            spans.extend(r.spans.clone());
        }
        out.push(Line::from(spans));
    }
    out
}

// ---------------------------------------------------------------------------
// The statusline (the last row)
// ---------------------------------------------------------------------------

/// The statusline's inks, named for the *role* each plays. Wireforge draws in
/// the terminal theme's own palette, so these are ANSI slots rather than fixed
/// RGB: on the everforest palette this was built against, `STRIP` is `#475258`,
/// `LAMP` is `#5d686f`, `ACCENT` is `#A7C080` and `BODY` is `#D3C6AA` — the same
/// dark / mid / accent triad lualine's everforest theme uses for its `c`, `b`
/// and `a` sections.
const STRIP: Color = Color::Black; // ANSI 0 — ink, and the full-width ground
const LAMP: Color = Color::DarkGray; // ANSI 8 — one step lighter
const ACCENT: Color = Color::Green; // ANSI 2 — the model-name pill
const BODY: Color = Color::White; // ANSI 7 — text on a block

/// The filled wedge that *closes* a block (U+E0B0): its ink is the block's own
/// ground, so that colour tapers rightwards over whatever follows.
const WEDGE_CLOSE: char = '\u{e0b0}';
/// The filled wedge that *opens* a block (U+E0B2): same ink, but the point
/// faces left, so the block arrives out of the ground it sits on.
const WEDGE_OPEN: char = '\u{e0b2}';
/// The thin divider between two items of one block (U+E0B1).
const ITEM_DIVIDER: char = '\u{e0b1}';
/// Strip cells kept between the left blocks and the right-hand hints, so the
/// two never fuse into one long bar.
const MIN_GAP: usize = 2;

/// One item on the statusline: runs of text, each in its own ink, all drawn on
/// the section's ground. A lamp spends two runs so that its dot can carry the
/// state while its label stays readable — an unlit lamp dims its *dot*, not the
/// word naming it.
struct Item {
    runs: Vec<(String, Color)>,
}

impl Item {
    /// An item of one run.
    fn plain(text: String, ink: Color) -> Item {
        Item {
            runs: vec![(text, ink)],
        }
    }

    /// The item's text width, without the padding the section adds.
    fn text_width(&self) -> usize {
        self.runs.iter().map(|(t, _)| t.chars().count()).sum()
    }
}

/// The lines the strip shows while a modifier is held, in place of the
/// everyday hints: the keys that modifier unlocks, and what they do.
///
/// The keys are read out of [`BINDINGS`] — every binding whose canonical chord
/// wears the modifier — so a rebind moves the hint with it, and only the verbs
/// are local wording. They are joined rather than listed one row per binding
/// because six `Ctrl+…  verb` rows do not fit in a strip that also names the
/// model, and there is one row per *group* because a modifier can reach more
/// than one — one verb covering two groups would be a lie.
///
/// Empty when nothing is held, or when what is held unlocks nothing (Alt and
/// Super are free), which leaves the everyday hints in place.
fn modifier_hint(mods: KeyModifiers) -> Vec<Item> {
    // Control wins when both are down: it is the one with more to say here.
    let (modifier, name) = if mods.contains(KeyModifiers::CONTROL) {
        (KeyModifiers::CONTROL, "Ctrl+")
    } else if mods.contains(KeyModifiers::SHIFT) {
        (KeyModifiers::SHIFT, "Shift+")
    } else {
        return Vec::new();
    };
    let mut items = Vec::new();
    // Group order follows HINT_GROUPS, which is the order the keys are
    // documented in.
    for group in HINT_GROUPS.iter().flat_map(|column| column.iter()).copied() {
        let keys: Vec<String> = BINDINGS
            .iter()
            .filter(|b| b.group == group)
            .map(|b| &b.chords[0])
            .filter(|c| c.mods.contains(modifier))
            .map(chord_key)
            .collect();
        if !keys.is_empty() {
            items.push(Item::plain(
                format!("{name}{} {}", keys.join("/"), group_verb(group)),
                BODY,
            ));
        }
    }
    items
}

/// What a group does, in the few words the strip can afford. The overlay's
/// headings spell the same thing out for a reader who has stopped to read;
/// this is for a glance.
fn group_verb(g: Group) -> &'static str {
    match g {
        Group::Rotate => "turn the view",
        Group::Move => "pan the view",
        Group::Local => "turn its own axes",
        Group::Frame => "frame the model",
        Group::Session => "session control",
    }
}

/// The key half of a chord's label, lower-cased when it is a single letter:
/// `Shift+H` reads as `h`, so a row of keys looks like keys instead of like
/// shouting.
fn chord_key(c: &Chord) -> String {
    let label = c.label();
    let key = ["Ctrl+", "Shift+"]
        .iter()
        .find_map(|prefix| label.strip_prefix(prefix))
        .unwrap_or(&label);
    if key.chars().count() == 1 {
        key.to_lowercase()
    } else {
        key.to_string()
    }
}

/// A run of items sharing one ground — lualine's `a` / `b` / `c` section.
struct Section {
    items: Vec<Item>,
    ground: Color,
}

impl Section {
    /// Cells this section occupies between `behind` and `ahead`: a cell of
    /// padding around every item, a divider between items, and a wedge at each
    /// end — but only where the ground actually changes, because a wedge drawn
    /// in the ground's own colour would be invisible anyway (lualine's rule).
    fn width(&self, behind: Color, ahead: Color) -> usize {
        let text: usize = self.items.iter().map(|i| i.text_width() + 2).sum();
        text + self.items.len().saturating_sub(1)
            + usize::from(behind != self.ground)
            + usize::from(ahead != self.ground)
    }

    /// Append the section to `spans`, wedging out of `behind` and into `ahead`.
    fn push(&self, spans: &mut Vec<Span<'static>>, behind: Color, ahead: Color) {
        // A divider is inked one step off its own ground, so it reads as a
        // seam rather than as one more glyph.
        let divider = if self.ground == STRIP { LAMP } else { STRIP };
        let glyph = |ch: char, fg: Color, bg: Color| {
            Span::styled(ch.to_string(), Style::default().fg(fg).bg(bg))
        };
        // The padding belongs to the item, so the item's ground covers it.
        let pad = |spans: &mut Vec<Span<'static>>, fg: Color| {
            spans.push(Span::styled(" ", Style::default().fg(fg).bg(self.ground)));
        };
        if behind != self.ground {
            spans.push(glyph(WEDGE_OPEN, self.ground, behind));
        }
        for (i, item) in self.items.iter().enumerate() {
            if i > 0 {
                spans.push(glyph(ITEM_DIVIDER, divider, self.ground));
            }
            let first = item.runs.first().map_or(BODY, |(_, ink)| *ink);
            pad(spans, first);
            for (text, ink) in &item.runs {
                spans.push(Span::styled(
                    text.clone(),
                    Style::default().fg(*ink).bg(self.ground),
                ));
            }
            pad(spans, first);
        }
        if ahead != self.ground {
            spans.push(glyph(WEDGE_CLOSE, self.ground, ahead));
        }
    }
}

/// The statusline as cells, ready to paint: one `(char, fg, bg)` per column,
/// exactly `width` of them.
///
/// The strip is a single left-aligned row whose sections already cover it edge
/// to edge, so cells *are* the whole of it — no widget has to lay it out, and
/// a caller that caches them skips both the layout and the copy of it that
/// handing a `Line` to `Paragraph` would cost.
fn status_cells_for(app: &App, width: u16) -> Vec<(char, Color, Color)> {
    status_line(app, width)
        .spans
        .iter()
        .flat_map(|span| {
            let fg = span.style.fg.unwrap_or(Color::Reset);
            let bg = span.style.bg.unwrap_or(Color::Reset);
            span.content.chars().map(move |c| (c, fg, bg))
        })
        .collect()
}

/// The statusline: the last row, built as a lualine-style strip.
///
/// Three things give lualine's statusline its look, and this copies all three:
/// a full-width ground that carries the empty middle, sections in a lighter or
/// accent colour sitting on that ground, and powerline wedges that hand one
/// section's colour to the next. Left to right:
///
/// ```text
///  model.wrfm ▶ ● SPIN  ● AXES ▶        ◀ ? help  q quit  Space spin  Tab axes
/// ```
///
/// The pill names the model — the only place it appears, which is why Row 0 is
/// free to be pure telemetry. The lamps are live state, and the hints are the
/// chords that matter most, read out of [`BINDINGS`] so a key can never drift
/// out of sync with what it claims; the hand-written overlay is checked
/// against the same table by `help_overlay_lists_every_binding_once`.
///
/// The row is filled edge to edge and never exceeds `width` cells. In a narrow
/// terminal content is shed by how little it would be missed — hints from the
/// last one backwards, then the lamps, then the name is cut short — so the
/// strip stays a clean band instead of a ragged line. The overlay always has
/// the full list.
fn status_line(app: &App, width: u16) -> Line<'static> {
    /// The hints, most useful first: the order they appear in, and the order
    /// they are shed in a narrow terminal.
    const HINTS: &[(Action, &str)] = &[
        (Action::Help, "help"),
        (Action::Quit, "quit"),
        (Action::Spin, "spin"),
        (Action::Axes, "axes"),
    ];
    let width = width as usize;
    // Narrower than the pill's own padding leaves nothing to draw but the
    // strip itself; an empty pill would just be a green blob.
    if width < 3 {
        return Line::from(Span::styled(" ".repeat(width), Style::default().bg(STRIP)));
    }
    let hint = |action: Action, label: &str| -> Option<Item> {
        BINDINGS.iter().find(|b| b.action == action).map(|b| {
            // The canonical chord only: aliases would double the width of a
            // row that is always on screen, and the overlay names the same
            // chord once.
            Item::plain(format!("{} {label}", b.chords[0].label()), BODY)
        })
    };
    // A lamp is a filled dot whose ink carries the state; the label keeps the
    // body ink either way, so "off" reads as unlit rather than as unreadable.
    // Every lamp lights in the same accent: one colour means "on", so the ink
    // reads as state instead of as a code the operator has to remember.
    let lamp = |on: bool, label: &str| Item {
        runs: vec![
            ("● ".to_string(), if on { ACCENT } else { STRIP }),
            (label.to_string(), BODY),
        ],
    };

    // Hold a modifier and the hints become that modifier's chords: the strip
    // answers the question the hand is already asking. Nothing held — or a
    // terminal that never reports a bare modifier — leaves the everyday hints.
    let held = modifier_hint(app.held_modifiers());
    let mut hints: Vec<Item> = if held.is_empty() {
        HINTS
            .iter()
            .filter_map(|(action, label)| hint(*action, label))
            .collect()
    } else {
        held
    };
    let lamps = Section {
        items: vec![lamp(app.auto_spin, "SPIN"), lamp(app.show_axes, "AXES")],
        ground: LAMP,
    };
    let lamps_width = lamps.width(LAMP, STRIP);

    // The pill names the model: the override when there is one (a stream's
    // path, whose model name is only a file stem), the model name otherwise.
    let named = app.strip_label.as_deref().unwrap_or(&app.name);
    let full_len = named.chars().count();
    // The whole pill — a cell of padding, the name, a cell of padding and the
    // wedge that closes it — yields at most half the row. A model name is
    // short, but a stream's path is not, and a pill that long would starve the
    // lamps and the hints it shares the strip with.
    let pill_cap = (width / 2).saturating_sub(3).max(1);
    let mut name: Vec<char> = named.chars().take(pill_cap).collect();
    let mut show_lamps = true;
    // Every section draws a wedge only where its ground changes, so the pill
    // always costs one closing wedge and the hints one opening wedge. The gap
    // between them is the elastic part, exactly like lualine's `%=`: it takes
    // everything the content leaves, but never shrinks below MIN_GAP while
    // there are hints to keep clear of.
    let hints_width = |hints: &[Item]| {
        if hints.is_empty() {
            0
        } else {
            hints.iter().map(|i| i.text_width() + 2).sum::<usize>() + hints.len()
        }
    };
    let content = |name_len: usize, show_lamps: bool, hints: &[Item]| {
        name_len + 3 + if show_lamps { lamps_width } else { 0 } + hints_width(hints)
    };
    let fits = |name_len: usize, show_lamps: bool, hints: &[Item]| {
        content(name_len, show_lamps, hints) + if hints.is_empty() { 0 } else { MIN_GAP } <= width
    };

    // Shed content until the strip fits, the least-missed first: hints from
    // the last one backwards, then the lamps, then the name a character at a
    // time. The overlay is the backstop and always has the full list.
    while !hints.is_empty() && !fits(name.len(), show_lamps, &hints) {
        hints.pop();
    }
    if !fits(name.len(), show_lamps, &hints) {
        show_lamps = false;
    }
    while !fits(name.len(), show_lamps, &hints) && !name.is_empty() {
        name.pop();
    }

    // A name cut to fit is marked, so a shortened name is never mistaken for
    // the whole one. It comes out empty only on a terminal too narrow for even
    // the pill's padding.
    let mut label: String = name.iter().collect();
    if name.len() < full_len && !label.is_empty() {
        label.pop();
        label.push('…');
    }

    let pill = Section {
        items: vec![Item::plain(label, STRIP)],
        ground: ACCENT,
    };
    let mut spans: Vec<Span<'static>> = Vec::with_capacity(hints.len() * 2 + 6);
    // The pill starts flush at the left edge — nothing precedes it to wedge out
    // of — and hands its colour to the lamps, or straight back to the strip
    // when the lamps had to go.
    pill.push(&mut spans, ACCENT, if show_lamps { LAMP } else { STRIP });
    if show_lamps {
        lamps.push(&mut spans, LAMP, STRIP);
    }
    spans.push(Span::styled(
        " ".repeat(width.saturating_sub(content(name.len(), show_lamps, &hints))),
        Style::default().bg(STRIP),
    ));
    if !hints.is_empty() {
        Section {
            items: hints,
            ground: LAMP,
        }
        .push(&mut spans, STRIP, LAMP);
    }
    Line::from(spans)
}

// Game-engine event loop: an input thread + channel, one scheduler owned
// by the loop, a two-mode main loop, and dirty-flag rendering.

/// A key that is currently down. Its `motion` is sampled when the key goes
/// down, so a modifier change mid-hold can never fork one key into two motions.
#[derive(Debug, Clone, Copy)]
struct Hold {
    motion: Motion,
    /// Last Press/Repeat seen for this key: the clock the legacy (no key-up)
    /// timeout in `update_held` runs on.
    seen: Instant,
}

/// All viewer state owned by the main loop.
struct App {
    current: Model,
    /// The model's geometric-mean length ([`view::model_extent`]), computed
    /// once because `current` is never mutated after `App::new`.
    ///
    /// It sizes the axes and the pan/zoom step, so re-deriving it per frame
    /// would put an O(n) bounds scan on every frame — and it is needed twice
    /// per frame (axes + held motion). The bounding box only depends on the
    /// model, never on the view, so one scan at load is enough.
    extent: f64,
    name: String,
    /// Statusline label override: the FIFO path for a stream preview, whose
    /// model name is only the file stem; `None` shows the model name.
    strip_label: Option<String>,
    view: ViewState,
    /// Keys currently held down, keyed by their identity (the base key from
    /// `canonical_key`), so Release finds the right hold even when the
    /// modifiers changed since the Press.
    held: HashMap<KeyCode, Hold>,
    /// True once the terminal delivered a Release. From then on key-up is
    /// authoritative, so holds end on Release / FocusLost and are never
    /// dropped for staying quiet (see `update_held`).
    release_seen: bool,
    auto_spin: bool,
    hud: Hud,
    show_axes: bool,
    /// Bare modifier keys reported down right now (`LeftControl`, `RightShift`,
    /// ...). A kitty-protocol terminal reports them on their own, which is what
    /// lets the statusline follow a held modifier; a legacy terminal reports
    /// none, and then this stays empty and the strip keeps its everyday hints.
    mods_down: Vec<ModifierKeyCode>,
    /// True when the screen must be repainted before the loop blocks again.
    dirty: bool,
}

impl App {
    fn new(current: Model, name: String, strip_label: Option<String>) -> Self {
        // One extent for the whole session: it frames the model now and then
        // serves every later frame (see the field's docs).
        let extent = view::model_extent(&current);
        let mut view = ViewState::default();
        // Frame the model (an empty one lands at the default distance), so a
        // fresh App is always correctly framed — including the blank start-up
        // view, whose extent is undefined.
        view.fit_to_extent(extent);
        App {
            current,
            extent,
            name,
            strip_label,
            view,
            held: HashMap::new(),
            release_seen: false,
            auto_spin: false,
            hud: Hud::Collapsed,
            show_axes: true,
            mods_down: Vec::new(),
            dirty: true,
        }
    }

    /// The modifiers held down right now, as the statusline reads them.
    fn held_modifiers(&self) -> KeyModifiers {
        self.mods_down
            .iter()
            .fold(KeyModifiers::empty(), |acc, m| acc | modifier_flag(*m))
    }

    /// Record a bare modifier key going down (`down`) or coming up. Returns
    /// whether the *set* of held modifiers changed, which is what the
    /// statusline's hints follow: pressing the right Ctrl while the left one is
    /// already down changes nothing on screen.
    fn set_modifier(&mut self, m: ModifierKeyCode, down: bool) -> bool {
        let before = self.held_modifiers();
        if down {
            if !self.mods_down.contains(&m) {
                self.mods_down.push(m);
            }
        } else {
            self.mods_down.retain(|k| *k != m);
        }
        self.held_modifiers() != before
    }

    /// Translation speed scales with the model's geometric-mean extent.
    fn move_scale(&self) -> f64 {
        self.extent
    }

    /// Record that the keyboard said something at `now`.
    ///
    /// On a terminal that never reports key-up this is the only way to tell a
    /// hold that is still down from one that was released: the OS repeats
    /// only the most recently pressed key, so a second key silences the
    /// first, and refreshing per key would drop the earlier one mid-hold
    /// (press `j`, then `l`, and `j` stops rotating after the timeout).
    /// Refreshing every hold means silence alone ends them.
    fn note_key_event(&mut self, now: Instant) {
        if self.release_seen {
            return;
        }
        for hold in self.held.values_mut() {
            hold.seen = now;
        }
    }

    /// A motion key went down: sample its action now. The hold is keyed by the
    /// key's identity, so `Shift+h`, `Ctrl+h` and `h` share one slot and can
    /// never run side by side, and a Repeat of an already-held key only
    /// re-arms it.
    fn hold_key_down(&mut self, hold_id: KeyCode, motion: Motion) {
        self.held.insert(
            hold_id,
            Hold {
                motion,
                seen: Instant::now(),
            },
        );
        self.dirty = true;
    }

    /// Auto-repeat only refreshes the clock: a Repeat never re-samples the
    /// motion, so pressing or releasing Shift mid-hold cannot swap the motion
    /// out from under a key that is still down.
    fn hold_key_repeat(&mut self, hold_id: KeyCode) {
        if let Some(hold) = self.held.get_mut(&hold_id) {
            hold.seen = Instant::now();
        }
    }

    /// Key-up: drop the hold by identity. The modifiers of the Release event
    /// are irrelevant — they may well differ from the Press (release Shift
    /// before `h` and the event no longer carries SHIFT).
    fn hold_key_up(&mut self, hold_id: KeyCode) {
        if self.held.remove(&hold_id).is_some() {
            self.dirty = true;
        }
    }

    /// Handle one terminal input event. Returns true when the loop must break (quit).
    fn handle_input(&mut self, ev: Event) -> bool {
        // Key and focus events are the only input handled: mouse capture is
        // OFF (native terminal drag-selection works), so the app never
        // receives mouse events; Event::Resize is handled by the loop (it
        // needs the Engine).
        // Focus loss: the key-up of a key held across an alt-tab reaches the
        // other window, so no Release ever arrives — drop the holds here.
        if let Event::FocusLost = ev {
            if !self.held.is_empty() || !self.mods_down.is_empty() {
                self.held.clear();
                self.mods_down.clear();
                self.dirty = true;
            }
            return false;
        }
        let Event::Key(key) = ev else {
            return false;
        };
        // A bare modifier key: no action of its own, but the statusline follows
        // it. Its hints are the chord list for whatever is held, so pressing
        // Ctrl turns the strip into the Ctrl chords before the next key lands.
        // A terminal that does not report modifiers never sends this and the
        // strip simply keeps its everyday hints.
        if let KeyCode::Modifier(m) = key.code {
            if key.kind == KeyEventKind::Release {
                self.release_seen = true;
            }
            if self.set_modifier(m, key.kind != KeyEventKind::Release) {
                self.dirty = true;
            }
            return false;
        }
        // A Release is direct evidence that the terminal reports key-up: from
        // here on a hold ends when the key does, never on a timeout. It is
        // matched by IDENTITY, because a Release may carry different modifiers
        // than the Press (release Shift before `h` and SHIFT is gone) — keying
        // it by the resulting motion would leave the motion running forever.
        if key.kind == KeyEventKind::Release {
            self.release_seen = true;
            let (hold_id, _) = canonical_key(key.code, key.modifiers);
            self.hold_key_up(hold_id);
            return false;
        }
        // Any other event is evidence that some key is still down.
        self.note_key_event(Instant::now());
        // One event -> one (identity, action). Unbound keys stop here, which
        // includes the bare modifier keys the kitty protocol reports.
        let Some((hold_id, action)) = resolve_key_event(key.code, key.modifiers) else {
            return false;
        };
        match action {
            // Quit applies to Press and Repeat alike (holding `q` quits now).
            Action::Quit => return true,
            // A motion key: Press samples the action, Repeat only re-arms it.
            Action::Motion(motion) => match key.kind {
                KeyEventKind::Repeat => self.hold_key_repeat(hold_id),
                _ => self.hold_key_down(hold_id, motion),
            },
            // One-shot actions fire once per tap, never at the repeat rate.
            _ if key.kind == KeyEventKind::Repeat => {}
            Action::Help => {
                self.hud = match self.hud {
                    Hud::Collapsed => Hud::Expanded,
                    Hud::Expanded => Hud::Collapsed,
                };
                self.dirty = true;
            }
            Action::Spin => {
                self.auto_spin = !self.auto_spin;
                self.dirty = true;
            }
            Action::Axes => {
                self.show_axes = !self.show_axes;
                self.dirty = true;
            }
            // Reset: the file's own framing (rotation, pan and distance).
            Action::Reset => {
                self.view.reset_with_extent(self.extent);
                self.dirty = true;
            }
        }
        false
    }

    /// Apply continuous motion to every held key each frame (dt-scaled), then
    /// drop the keys that stopped reporting. Release (kitty protocol) and
    /// FocusLost are the normal stops; the timeout is the only stop on a
    /// terminal that never sends a Release.
    fn update_held(&mut self, now: Instant, dt: f64) {
        // With key-up reporting there is nothing to time out: Release ends the
        // hold and FocusLost covers an alt-tab, so a timeout could only ever
        // kill a key that is still down — the OS repeats only the most recent
        // one, so a quiet hold is not evidence of a released key. Without
        // key-up reporting, silence is the only evidence there is and one
        // second of it ends the hold.
        if !self.release_seen {
            self.held
                .retain(|_, hold| now.saturating_duration_since(hold.seen) <= LEGACY_HOLD_TIMEOUT);
        }
        if self.held.is_empty() {
            return;
        }
        let move_scale = self.move_scale();
        for hold in self.held.values() {
            continuous_step(&mut self.view, hold.motion, move_scale, dt);
        }
    }
}

/// L2 engine state: the retained-mode screen and the rasterizer.
struct Engine {
    screen: render::Screen,
    raster: render::Rasterizer,
    hud_buf: Option<Buffer>,
    /// The statusline, kept between frames. Its look depends on a handful of
    /// values, so it is rebuilt when one of them changes rather than every
    /// frame — and rebuilding is the common case only while a modifier is
    /// being held down.
    status: Option<StatusCache>,
}

/// The statusline's cells, and the inputs that produced them.
struct StatusCache {
    /// The pill's text: the model's name, or a stream's path.
    named: String,
    spin: bool,
    axes: bool,
    mods: KeyModifiers,
    width: u16,
    cells: Vec<(char, Color, Color)>,
}

impl Engine {
    fn new(w: usize, h: usize) -> Self {
        let mut engine = Engine {
            screen: render::Screen::new(w, h),
            raster: render::Rasterizer::new(),
            hud_buf: None,
            status: None,
        };
        // No Resize event precedes the first frame, so the opening frame needs
        // a canvas already in place.
        engine.size_raster_to_canvas();
        engine
    }

    /// Point the rasterizer at the canvas of the screen's current size.
    ///
    /// The screen is the authority, so this is safe to call after any change
    /// to it: the rasterizer's grid always matches the region `compose_frame`
    /// renders through.
    fn size_raster_to_canvas(&mut self) {
        let (w, h) = self.screen.size();
        let (_, _, cw, ch) = model_canvas_rect(w, h);
        self.raster.resize(cw, ch);
    }

    /// Terminal resize: reallocate the screen, drop the HUD buffer (it was
    /// sized for the old screen) and carry the rasterizer's canvas along.
    /// The `Resize` event is the only thing that changes the canvas size, so
    /// this is the only place a frame's grid is ever allocated.
    fn resize(&mut self, w: usize, h: usize) {
        self.screen.resize(w, h);
        self.hud_buf = None;
        self.size_raster_to_canvas();
    }

    /// Paint the statusline on `row`, rebuilding it only when something it
    /// depends on has changed.
    ///
    /// It goes straight to the screen rather than through the HUD buffer: a
    /// row of cells is what the strip *is*, and copying it into a widget to
    /// copy it back out again would cost more than the layout it saves.
    fn paint_status(&mut self, app: &App, width: u16, row: usize) {
        let named = app.strip_label.as_deref().unwrap_or(&app.name);
        let mods = app.held_modifiers();
        let stale = self.status.as_ref().is_none_or(|cache| {
            cache.named != named
                || cache.spin != app.auto_spin
                || cache.axes != app.show_axes
                || cache.mods != mods
                || cache.width != width
        });
        if stale {
            self.status = Some(StatusCache {
                named: named.to_string(),
                spin: app.auto_spin,
                axes: app.show_axes,
                mods,
                width,
                cells: status_cells_for(app, width),
            });
        }
        if let Some(cache) = &self.status {
            for (x, &(ch, fg, bg)) in cache.cells.iter().enumerate() {
                self.screen
                    .set(x, row, ch, render::ink_idx(fg), render::ink_idx(bg));
            }
        }
    }
}

/// The only crossterm event reader in the process (blocks on the tty).
fn spawn_input_thread(tx: Sender<Event>) {
    let _ = thread::Builder::new()
        .name("wireforge-input".into())
        .spawn(move || {
            while let Ok(ev) = event::read() {
                if tx.send(ev).is_err() {
                    break;
                }
            }
        });
}

/// Fallback resize detection (timer-driven): resize and mark dirty when the terminal size no longer matches the screen.
fn check_resize(app: &mut App, engine: &mut Engine, timers: &mut TimerScheduler) {
    if let Ok((cols, rows)) = crossterm::terminal::size() {
        let (w, h) = engine.screen.size();
        if cols as usize != w || rows as usize != h {
            engine.resize(cols as usize, rows as usize);
            // A resize is a visual change: the dirty flag makes the idle
            // branch repaint right after this timer fire.
            app.dirty = true;
        }
    }
    timers.schedule(TimerId::ResizeCheck, Instant::now() + RESIZE_CHECK_INTERVAL);
}

/// Fire every timer due at `now` through its dedicated handler. Called from
/// both loop modes — and after each idle event, so a steady event stream can
/// never postpone a due timer.
fn fire_due_timers(app: &mut App, engine: &mut Engine, timers: &mut TimerScheduler, now: Instant) {
    for id in timers.fire_due(now) {
        match id {
            TimerId::ResizeCheck => check_resize(app, engine, timers),
        }
    }
}

/// Copy the rectangle `r` of the HUD buffer into the screen (char + fg + bg).
///
/// The rect, not the row, is the unit: the help panel is centred over the
/// canvas, so only the columns it actually covers may overwrite the model.
///
/// Backgrounds matter here and nowhere else: the model canvas paints dots on
/// the terminal's own background, while the statusline block is the one place
/// Wireforge fills cells.
fn blit_hud_rect(screen: &mut render::Screen, hud: &Buffer, r: Rect) {
    let (w, h) = screen.size();
    let (w16, h16) = (w as u16, h as u16);
    let y_end = r.y.saturating_add(r.height).min(h16);
    let x_end = r.x.saturating_add(r.width).min(w16);
    for y in r.y..y_end {
        for x in r.x..x_end {
            let cell = &hud[(x, y)];
            let ch = cell.symbol().chars().next().unwrap_or(' ');
            screen.set(
                x as usize,
                y as usize,
                ch,
                render::ink_idx(cell.fg),
                render::ink_idx(cell.bg),
            );
        }
    }
}

/// The model canvas on a screen of `w`x`h` cells, as `(x, y, w, h)`: row 0 is
/// the fixed telemetry band and the last row is the hint footer, so the canvas
/// is every row between them. Height 0 when the terminal is too short to hold
/// both bands.
///
/// The one place the canvas geometry is spelled out: the terminal Resize path
/// sizes the rasterizer with it and `compose_frame` renders with it, so the
/// grid the model is painted into and the region it is painted through cannot
/// drift apart.
fn model_canvas_rect(w: usize, h: usize) -> (usize, usize, usize, usize) {
    (0, 1, w, h.saturating_sub(2))
}

/// Compose one frame into the engine's retained screen: the telemetry band,
/// the model canvas, the help panel over it (in its own rect only) and the
/// statusline. Presenting is `render_frame`'s job; splitting the two is what
/// lets the frame tests read composition off `engine.screen`.
///
/// The rasterizer's canvas is NOT sized here: it follows the screen in
/// `Engine::resize`, so a frame costs no allocation.
fn compose_frame(app: &mut App, engine: &mut Engine) {
    let (w, h) = engine.screen.size();
    if w == 0 || h == 0 {
        return;
    }
    let w16 = w as u16;
    let h16 = h as u16;
    let row0 = telemetry_line(&app.view);
    let (canvas_x, canvas_top, canvas_w, canvas_h) = model_canvas_rect(w, h);
    let canvas_top = canvas_top as u16;
    let canvas_h = canvas_h as u16;
    let footer_top: u16 = h16.saturating_sub(1);
    let overlay = app.hud == Hud::Expanded;

    // Reuse a persistent ratatui Buffer for the cold overlays.
    let needs_realloc = engine
        .hud_buf
        .as_ref()
        .is_none_or(|b| b.area.width != w16 || b.area.height != h16);
    if needs_realloc {
        engine.hud_buf = Some(Buffer::empty(Rect::new(0, 0, w16, h16)));
    } else if let Some(b) = engine.hud_buf.as_mut() {
        b.reset();
    }

    // Row 0 (telemetry): always present, always its own row. The statusline
    // goes straight to the screen later, once nothing else can paint over it.
    {
        let hud = engine.hud_buf.as_mut().unwrap();
        Paragraph::new(row0).render(Rect::new(0, 0, w16, 1), hud);
    }

    // The panel's rect on screen, when it is up. The model is rasterized
    // first and the panel's rows are blitted over exactly this rect, so the
    // panel is opaque where it covers and invisible everywhere else.
    let panel = if overlay {
        // Help panel: centred over the canvas, not a full-screen takeover.
        let lines = help_lines();
        let inner_w = lines.iter().map(Line::width).max().unwrap_or(0) as u16 + 4;
        let inner_h = lines.len() as u16 + 2; // +2 for the rounded border
        // Fall back to the whole canvas when the terminal is too small to
        // centre in; the text then clips at the panel edge rather than
        // silently vanishing, which is the honest failure.
        let mut area = Rect {
            x: 0,
            y: canvas_top,
            width: w16.min(inner_w.max(8)),
            height: canvas_h.min(inner_h.max(3)),
        };
        area.x = (w16.saturating_sub(area.width)) / 2;
        area.y = canvas_top + (canvas_h.saturating_sub(area.height)) / 2;
        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .padding(Padding::new(2, 2, 0, 0));
        let inner = block.inner(area);
        let hud = engine.hud_buf.as_mut().unwrap();
        block.render(area, hud);
        for (i, line) in lines.iter().enumerate() {
            if i as u16 >= inner.height {
                break;
            }
            Paragraph::new(line.clone())
                .render(Rect::new(inner.x, inner.y + i as u16, inner.width, 1), hud);
        }
        Some(area)
    } else {
        None
    };

    // Blit row 0 from the HUD buffer; the canvas never paints over it.
    {
        let screen = &mut engine.screen;
        let hud = engine.hud_buf.as_ref().unwrap();
        blit_hud_rect(screen, hud, Rect::new(0, 0, w16, canvas_top));
    }

    if canvas_h > 0 {
        // Model canvas: rasterize into the screen directly. It draws under
        // the help panel too — the panel only claims the cells it covers.
        engine.raster.render(
            &app.current,
            &app.view,
            (canvas_x, canvas_top as usize, canvas_w, canvas_h as usize),
            app.show_axes,
            app.extent,
            &mut engine.screen,
        );
    }

    // The panel, if any, paints over the model in exactly its own rectangle.
    if let Some(panel) = panel {
        let screen = &mut engine.screen;
        let hud = engine.hud_buf.as_ref().unwrap();
        blit_hud_rect(screen, hud, panel);
    }

    // The statusline always paints last, over whatever the canvas left behind.
    engine.paint_status(app, w16, footer_top as usize);
}

/// Render the current state into the terminal: compose a frame, then present
/// it (packed-cell diff + one batched write).
fn render_frame(
    app: &mut App,
    engine: &mut Engine,
    stdout: &mut io::Stdout,
) -> Result<(), Box<dyn Error>> {
    compose_frame(app, engine);
    engine.screen.present(stdout)?;
    Ok(())
}

/// Undo everything `main` did to the terminal: the keyboard-enhancement flags
/// (they are sticky — without this the shell would keep receiving escape
/// codes for modified keys), focus reporting, cursor, alternate screen and
/// raw mode. Idempotent and a no-op unless raw mode was enabled, so the
/// guard, the panic hook and the normal exit path can all call it.
fn restore_terminal() {
    if !crossterm::terminal::is_raw_mode_enabled().unwrap_or(false) {
        return;
    }
    let mut stdout = io::stdout();
    let _ = execute!(
        stdout,
        PopKeyboardEnhancementFlags,
        DisableFocusChange,
        Show,
        LeaveAlternateScreen
    );
    let _ = disable_raw_mode();
}

/// RAII terminal cleanup. `main` creates it right after `enable_raw_mode`,
/// so every later exit path (normal return, `?` error, panic) restores the
/// terminal instead of leaving a half-configured screen behind.
struct TerminalGuard;

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        restore_terminal();
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    // Restore first, then report the panic: the message would otherwise be
    // printed inside the alternate screen and the sticky flags would survive.
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore_terminal();
        default_hook(info);
    }));
    let args = Args::parse();
    // FILE has no default value: what happens without it depends on stdin.
    //
    // * stdin is NOT a terminal: it is a stream (`cat m.wrfm | wireforge`),
    //   so read the model from there — the same thing `wireforge -` asks for.
    // * stdin IS a terminal: there is nothing to read and nothing to open,
    //   so start with an empty canvas. The viewer comes up blank and stays
    //   interactive (a model is only read at start-up; run wireforge with a
    //   path to view one).
    //
    // A terminal check rather than `None == blank` outright: that keeps
    // `wireforge < /dev/null` (a script, an editor, a null device) failing
    // loudly instead of parking an invisible TUI on a blank screen. Use `-`
    // for an explicit stdin read.
    let blank = args.file.is_none() && io::stdin().is_terminal();
    let target_file = if blank {
        // No FILE and no stream: nothing to load. `target_file` is only read
        // by the branches below, which this flag rules out.
        PathBuf::new()
    } else {
        // FILE, or `-` for the stream a bare invocation implies when stdin is
        // a pipe. A path is never guessed here.
        args.file.unwrap_or_else(|| PathBuf::from("-"))
    };

    // Regular files are probed through PROBE_BYTES and loaded from the path;
    // `-`/FIFO read the whole stream once, at start-up.
    let is_stdin = target_file == Path::new("-");
    let is_fifo = !is_stdin && !blank && is_fifo_path(&target_file);
    let is_stream = is_stdin || is_fifo;
    // Statusline label override: the FIFO's full path (its model name is only
    // the file stem). stdin and regular files show their model name.
    let strip_label: Option<String> = if is_fifo {
        Some(target_file.display().to_string())
    } else {
        None
    };

    // When stdin is a pipe the keyboard comes from the controlling terminal;
    // fail fast (before consuming the model) with a clear message if there
    // is none — never a cryptic ENXIO from raw-mode setup.
    if let Err(e) = ensure_keyboard_terminal() {
        eprintln!("{e}");
        std::process::exit(1);
    }

    let (current_model, model_name) = if blank {
        // No FILE on a terminal: nothing to read, nothing to open. The
        // viewer starts on an empty canvas — Row 0 says "no file" — and every
        // key keeps working; there is no model to rotate.
        (Model::default(), "no file".to_string())
    } else if is_stream {
        // Stream path: read all of stdin (or the FIFO) once, probe the
        // whole BUFFER (not the path) and parse it.
        let mut buf = String::new();
        if is_stdin {
            io::stdin()
                .read_to_string(&mut buf)
                .map_err(|e| format!("cannot read stdin: {e}"))?;
        } else {
            std::fs::File::open(&target_file)
                .map_err(|e| format!("cannot open '{}': {e}", target_file.display()))?
                .read_to_string(&mut buf)
                .map_err(|e| format!("cannot read '{}': {e}", target_file.display()))?;
        }
        let name = if is_stdin {
            "stdin".to_string()
        } else {
            target_file
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("fifo")
                .to_string()
        };
        let label = if is_stdin {
            "-".to_string()
        } else {
            target_file.display().to_string()
        };
        load_model_from_text(&name, &buf).unwrap_or_else(|e| {
            eprintln!("Failed to load '{label}': {e}");
            std::process::exit(1);
        })
    } else {
        load_model(&target_file).unwrap_or_else(|e| {
            eprintln!("Failed to load '{}': {}", target_file.display(), e);
            std::process::exit(1);
        })
    };

    // The event queue: one channel, one blocking consumer — the input thread
    // is the only crossterm reader, and it pushes events here.
    let (tx, rx) = mpsc::channel::<Event>();

    enable_raw_mode()?;
    // From here on every exit path (return value, `?` error or panic) must
    // hand back a usable terminal: the enhancement flags are sticky and would
    // otherwise leak into the shell. Dropping the guard runs `restore_terminal`.
    let _terminal = TerminalGuard;
    let mut stdout = io::stdout();
    // Mouse capture stays OFF (native drag-selection) and the cursor is
    // hidden for the session (the L2 direct-write present does not hide it).
    execute!(stdout, EnterAlternateScreen, Hide)?;
    // Enable the Kitty keyboard protocol for Press/Repeat/Release key events,
    // plus alternate key codes so a shifted chord arrives as the glyph the
    // active layout types (`Shift+0` -> `)`) rather than a layout-dependent
    // base key — that is what keeps `?` / `_` / `+` / `)` and the strict Shift
    // rules working on non-US layouts. Keyboard enhancement (kitty protocol)
    // is optional: terminals that don't support it (e.g. the legacy Windows
    // console API) run without it, and legacy terminals simply ignore the flag.
    let _ = execute!(
        stdout,
        PushKeyboardEnhancementFlags(
            KeyboardEnhancementFlags::REPORT_EVENT_TYPES
                | KeyboardEnhancementFlags::REPORT_ALTERNATE_KEYS
                | KeyboardEnhancementFlags::REPORT_ALL_KEYS_AS_ESCAPE_CODES
                | KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
        )
    );
    // Focus events: the key-up of a key held across an alt-tab goes to the
    // other window, so the app drops its holds itself on FocusLost.
    let _ = execute!(stdout, EnableFocusChange);

    // Input thread: block in the kernel on the tty, push events through the
    // channel. Started after raw mode so the tty is in the expected state.
    spawn_input_thread(tx.clone());

    // L2 engine + viewer state.
    let (cols, rows) = crossterm::terminal::size()?;
    let mut app = App::new(current_model, model_name, strip_label);
    let mut engine = Engine::new(cols as usize, rows as usize);
    let mut timers = TimerScheduler::new();
    let mut last = Instant::now();

    // Initial frame: the previous screen is all-space, so the first present
    // writes every non-space cell (same first-frame semantics as before).
    render_frame(&mut app, &mut engine, &mut stdout)?;
    app.dirty = false;
    timers.schedule(TimerId::ResizeCheck, Instant::now() + RESIZE_CHECK_INTERVAL);

    'main: loop {
        let now = Instant::now();
        let dt = (now - last).as_secs_f64().min(0.1);
        last = now;

        // The two SDL-style loop modes: animating = drain +
        // update + render as fast as possible, no waiting; idle = block in
        // the channel until the earliest timer or an event.
        let animating = app.auto_spin || !app.held.is_empty();

        if animating {
            // Drain the event backlog (no waiting, no cap).
            while let Ok(ev) = rx.try_recv() {
                match ev {
                    Event::Resize(cols, rows) => {
                        engine.resize(cols as usize, rows as usize);
                        app.dirty = true;
                    }
                    ev => {
                        if app.handle_input(ev) {
                            break 'main;
                        }
                    }
                }
            }
            // Timers can fire mid-animation too.
            fire_due_timers(&mut app, &mut engine, &mut timers, now);
            // One frame of smooth motion + held-key expiry.
            app.update_held(now, dt);
            if app.auto_spin {
                // Space auto-spin: rotate the model around its own (local) Y
                // axis (view::ViewState::spin_local) — a globe turning in
                // place, however it is pitched/rolled. The sign keeps the
                // spin agreeing with plain ← (the viewer's left).
                app.view.spin_local(-SPIN_RATE * dt);
                app.view.normalize();
            }
            render_frame(&mut app, &mut engine, &mut stdout)?;
        } else {
            // Idle: block until the earliest pending timer or an event —
            // the thread is parked in the kernel (0% CPU).
            let wait = timers
                .earliest()
                .map(|d| d.saturating_duration_since(Instant::now()));
            let msg: Result<Event, mpsc::RecvTimeoutError> = match wait {
                Some(t) => rx.recv_timeout(t),
                None => match rx.recv() {
                    Ok(e) => Ok(e),
                    Err(_) => Err(mpsc::RecvTimeoutError::Disconnected),
                },
            };
            match msg {
                Ok(ev) => {
                    match ev {
                        Event::Resize(cols, rows) => {
                            engine.resize(cols as usize, rows as usize);
                            app.dirty = true;
                        }
                        ev => {
                            if app.handle_input(ev) {
                                break 'main;
                            }
                        }
                    }
                    // Due timers also advance while events keep arriving, so a
                    // steady stream cannot starve them until the queue drains.
                    fire_due_timers(&mut app, &mut engine, &mut timers, now);
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    // The kernel woke us exactly at the earliest deadline.
                    fire_due_timers(&mut app, &mut engine, &mut timers, now);
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => break 'main,
            }
            // Expire held keys that stopped reporting (a lost Release, or a
            // terminal that never sends one) without spinning.
            app.update_held(now, dt);
            // Redraw only when something actually changed (dirty-flag).
            if app.dirty {
                render_frame(&mut app, &mut engine, &mut stdout)?;
                app.dirty = false;
            }
        }
    }

    // The TerminalGuard created after enable_raw_mode restores the terminal
    // (flags, focus reporting, cursor, screen, raw mode) when this returns.
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyEvent;
    use std::fs;

    /// The speed constants are dyadic fractions (exact in binary).
    #[test]
    fn speed_constants_are_dyadic_and_exact() {
        assert_eq!(ROT_RATE * 128.0, 169.0);
        assert_eq!(MOVE_RATE * 128.0, 83.0);
        assert_eq!(SPIN_RATE * 256.0, 169.0);
    }

    /// Write `content` to a uniquely named temp file and return its path.
    fn temp_file(name: &str, content: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("wrfm-main-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        fs::write(&path, content).unwrap();
        path
    }

    /// Write `content` to a uniquely named temp `.wrfm` file.
    fn temp_wrfm(name: &str, content: &str) -> PathBuf {
        temp_file(&format!("{name}.wrfm"), content)
    }

    // --- Content-first open detection ---

    #[test]
    fn load_model_accepts_valid_wrfm() {
        // A valid v2 file: `wrfm 2` magic, `vertices <N> edges <M>` counts
        // header, then `v` / `e` lines.
        let p = temp_wrfm(
            "valid",
            "wrfm 2\nvertices 2   edges 1\n\nv 0 0 0\nv 1 1 1\ne 0 1\n",
        );
        let (mode, name) = load_model(&p).expect("valid wrfm must load");
        let m = mode;
        assert_eq!(m.vertices.len(), 2);
        assert_eq!(m.edges.len(), 1);
        assert_eq!(
            name,
            p.file_stem().unwrap().to_str().unwrap(),
            "model name comes from the file stem"
        );
    }

    #[test]
    fn open_detection_txt_with_wrfm_content_opens() {
        // A `.txt` holding v2 wrfm content must open as wrfm — the extension
        // is only a hint, the content is authoritative.
        let p = temp_file(
            "model.txt",
            "wrfm 2\nvertices 2   edges 1\n\nv 0 0 0\nv 1 1 1\ne 0 1\n",
        );
        let (mode, name) = load_model(&p).expect("wrfm content in a .txt must open");
        let m = mode;
        assert_eq!(m.vertices.len(), 2);
        assert_eq!(m.edges.len(), 1);
        assert_eq!(name, "model");
    }

    #[test]
    fn open_detection_wrfm_with_garbage_is_unrecognized() {
        // Garbage is "unrecognized" (never an Ok empty model); a file
        // starting with `wrfm 2` is a parse (load) error instead.
        let p = temp_wrfm("garbage", "this is not a wireframe\nno markers here\n");
        let err = load_model(&p).expect_err("garbage must fail to load");
        assert!(
            err.contains("unrecognized"),
            "error should say 'unrecognized': {err}"
        );

        // Magic present, garbage after it: routed to the wrfm parser, which
        // must fail on the missing counts header — never "unrecognized".
        let p2 = temp_wrfm("magic-garbage", "wrfm 2\nthis is garbage after the magic\n");
        let err2 = load_model(&p2).expect_err("magic+garbage must fail to load");
        assert!(
            !err2.contains("unrecognized"),
            "a wrfm magic line must route to the wrfm parser: {err2}"
        );
    }

    #[test]
    fn open_detection_obj_content_points_at_the_converter() {
        // Real OBJ content: the probe points at `wrfm convert` and loading
        // fails with the same hint — never a silent empty model.
        let p = temp_file("cube.obj", "v 0 0 0\nv 1 0 0\nv 0 1 0\nf 1 2 3\n");
        let err = probe_format(&p).unwrap_err();
        assert!(err.contains("wrfm convert"), "hint: {err}");
        let err = load_model(&p).expect_err("obj file must not load");
        assert!(err.contains("wrfm convert"), "error: {err}");
    }

    #[test]
    fn open_detection_vertex_only_points_at_the_converter() {
        // A file with only `v ` lines (no `e `) is an OBJ point cloud:
        // v-only is an obj marker, not a wrfm without the magic.
        let p = temp_wrfm("vertex-only", "v 0 0 0\nv 1 1 1\n");
        let err = probe_format(&p).unwrap_err();
        assert!(err.contains("wrfm convert"), "hint: {err}");
    }

    #[test]
    fn wrfm_style_without_magic_is_unrecognized() {
        // The v2 breaking change: a file with wrfm-style
        // `v`/`e` lines but NO `wrfm <version>` magic is NOT wrfm and NOT
        // obj — it is "unrecognized".
        let v_e = temp_wrfm("v-e", "v 0 0 0\nv 1 1 1\ne 0 1\n");
        assert!(
            probe_format(&v_e).is_err(),
            "v/e without magic must not probe as wrfm"
        );
        let err = load_model(&v_e).expect_err("v/e without magic must fail");
        assert!(err.contains("unrecognized"), "error: {err}");

        // Edge-only or marker-less content is likewise never wrfm.
        let edge_only = temp_wrfm("edge-only", "e 0 1\n");
        assert!(
            probe_format(&edge_only).is_err(),
            "an edge-only file is not wrfm by the detection contract"
        );
        let comment_only = temp_wrfm("comment-only", "# just a comment\n");
        assert!(probe_format(&comment_only).is_err());
    }

    #[test]
    fn wrfm_magic_beats_obj_markers() {
        // The magic test comes FIRST: a file starting with `wrfm 2` is
        // wrfm even if later lines look obj-ish (`f` is an unknown directive
        // that lenient parsing skips).
        let p = temp_wrfm(
            "magic-obj",
            "wrfm 2\nvertices 2   edges 1\n\nv 0 0 0\nv 1 0 0\ne 0 1\nf 1 2 3\n",
        );
        probe_format(&p).expect("magic wins");
        let (mode, _) = load_model(&p).expect("magic must win over obj markers");
        let m = mode;
        assert_eq!(m.vertices.len(), 2);
        assert_eq!(m.edges.len(), 1);
    }

    #[test]
    fn empty_file_is_unrecognized() {
        // An empty file has no first line -> no magic -> unrecognized
        // — never an Ok empty model.
        let p = temp_wrfm("empty", "");
        assert!(probe_format(&p).is_err());
        let err = load_model(&p).expect_err("empty file must fail to load");
        assert!(
            err.contains("unrecognized"),
            "error should say 'unrecognized': {err}"
        );
    }

    #[test]
    fn load_model_rejects_half_written_file() {
        // Truncated v2 .wrfm still carries magic + `v`/`e` markers: the
        // parser fails and the caller keeps the last good model.
        let p = temp_wrfm(
            "half",
            "wrfm 2\nvertices 2   edges 1\n\nv 0 0 0\nv 1 1\ne 0 1\n",
        );
        assert!(load_model(&p).is_err());
    }

    #[test]
    fn load_model_rejects_bad_edge() {
        // An edge line missing its second index must fail to parse.
        let p = temp_wrfm(
            "bad-edge",
            "wrfm 2\nvertices 2   edges 1\n\nv 0 0 0\nv 1 1 1\ne 0",
        );
        assert!(load_model(&p).is_err());
    }

    #[test]
    fn load_model_error_reports_line() {
        // Surface the parser's structured error (line number + offending
        // line); magic routes content to the wrfm parser.
        let p = temp_wrfm(
            "bad-num",
            "wrfm 2\nvertices 1   edges 1\n\nv 1.0 2.0 abc\ne 0 0\n",
        );
        let err = load_model(&p).expect_err("bad-num must fail to load");
        assert!(err.contains("line"), "error should mention the line: {err}");
        assert!(
            err.contains("v 1.0 2.0 abc"),
            "error should carry the offending source line: {err}"
        );
    }

    #[test]
    fn load_model_rejects_deleted_file() {
        let p = temp_wrfm("gone", "wrfm 2\nvertices 1   edges 1\n\nv 0 0 0\ne 0 0\n");
        fs::remove_file(&p).unwrap();
        assert!(load_model(&p).is_err(), "a deleted file must fail to load");
    }

    // --- telemetry (row 0) ---

    #[test]
    fn telemetry_row_carries_the_camera_and_no_name() {
        // Row 0 is telemetry only. The model's name leads the statusline, so
        // repeating it here would be the one fact on the screen twice — and it
        // is routinely long enough to push the numbers it shares the row with
        // off the edge.
        let view = ViewState {
            yaw: 0.5,
            ..ViewState::default()
        };
        let row0 = telemetry_line(&view);
        assert!(row0.starts_with("yaw=0.50"), "camera leads the row: {row0}");
        assert!(
            row0.contains("pitch=") && row0.contains("roll="),
            "row0: {row0}"
        );
        assert!(
            row0.contains("dist=") && row0.contains("pan=("),
            "row0: {row0}"
        );
        assert!(
            !row0.contains("Wireforge") && !row0.contains("cube"),
            "row0 must not repeat the model's name: {row0}"
        );
        assert!(
            !row0.contains("[?]"),
            "row0 is telemetry only; the hint lives on the statusline: {row0}"
        );
    }

    // ---------- keymap: BINDINGS <-> resolve_key_event ----------

    #[test]
    fn keymap_resolves_every_documented_chord() {
        // The contract the generated keymap has to keep, written out as
        // behaviour rather than as a second copy of the table: each row is an
        // event as a terminal reports it, and what the viewer must do with it.
        use KeyCode::*;
        let cases: &[(&str, KeyCode, KeyModifiers, Option<Action>)] = &[
            (
                "h rotates",
                Char('h'),
                KeyModifiers::NONE,
                Some(Action::Motion(Motion::YawLeft)),
            ),
            (
                "Shift+h pans, spelled either way",
                Char('h'),
                KeyModifiers::SHIFT,
                Some(Action::Motion(Motion::MoveLeft)),
            ),
            (
                "Shift+H is the same chord",
                Char('H'),
                KeyModifiers::SHIFT,
                Some(Action::Motion(Motion::MoveLeft)),
            ),
            ("Shift+q is not q", Char('q'), KeyModifiers::SHIFT, None),
            (
                "Shift+Q is not q either",
                Char('Q'),
                KeyModifiers::SHIFT,
                None,
            ),
            (
                "Ctrl+c quits",
                Char('c'),
                KeyModifiers::CONTROL,
                Some(Action::Quit),
            ),
            (
                "Ctrl+q quits",
                Char('q'),
                KeyModifiers::CONTROL,
                Some(Action::Quit),
            ),
            (
                "Ctrl+h turns the model's own way",
                Char('h'),
                KeyModifiers::CONTROL,
                Some(Action::Motion(Motion::LocalYawLeft)),
            ),
            (
                "Ctrl+Shift+h is the same as Ctrl+h",
                Char('H'),
                KeyModifiers::CONTROL.union(KeyModifiers::SHIFT),
                Some(Action::Motion(Motion::LocalYawLeft)),
            ),
            (
                "Ctrl+Left is the same as Ctrl+h",
                Left,
                KeyModifiers::CONTROL,
                Some(Action::Motion(Motion::LocalYawLeft)),
            ),
            ("Alt swallows the key", Char('h'), KeyModifiers::ALT, None),
            (
                "Alt swallows even a bound one",
                Char(' '),
                KeyModifiers::ALT,
                None,
            ),
            (
                "Ctrl+Space is not Space",
                Char(' '),
                KeyModifiers::CONTROL,
                None,
            ),
            (
                "? is Shift+/",
                Char('?'),
                KeyModifiers::NONE,
                Some(Action::Help),
            ),
            (
                "so is the base spelling",
                Char('/'),
                KeyModifiers::SHIFT,
                Some(Action::Help),
            ),
            (
                "+ is Shift+=",
                Char('+'),
                KeyModifiers::NONE,
                Some(Action::Motion(Motion::MoveForward)),
            ),
            (
                "_ is Shift+-",
                Char('_'),
                KeyModifiers::NONE,
                Some(Action::Motion(Motion::MoveBack)),
            ),
            (
                "c resets the view",
                Char('c'),
                KeyModifiers::NONE,
                Some(Action::Reset),
            ),
            ("Shift+0 binds nothing", Char(')'), KeyModifiers::NONE, None),
            ("Esc quits", Esc, KeyModifiers::NONE, Some(Action::Quit)),
            (
                "Shift+Esc is the same key",
                Esc,
                KeyModifiers::SHIFT,
                Some(Action::Quit),
            ),
            (
                "Shift+Space is the same key",
                Char(' '),
                KeyModifiers::SHIFT,
                Some(Action::Spin),
            ),
            (
                "a legacy BackTab is Shift+Tab",
                BackTab,
                KeyModifiers::NONE,
                Some(Action::Axes),
            ),
            (
                "and so is the kitty one",
                BackTab,
                KeyModifiers::SHIFT,
                Some(Action::Axes),
            ),
            (
                "Tab toggles the axes",
                Tab,
                KeyModifiers::NONE,
                Some(Action::Axes),
            ),
            (
                "an unbound letter does nothing",
                Char('z'),
                KeyModifiers::NONE,
                None,
            ),
        ];
        for (what, code, mods, expected) in cases {
            assert_eq!(
                resolve_key_event(*code, *mods).map(|(_, action)| action),
                *expected,
                "{what}: {code:?}+{mods:?}"
            );
        }
    }

    #[test]
    fn bindings_cover_every_action_exactly_once() {
        // Two rows describing one action would make the overlay list it twice;
        // a row describing none would list a phantom.
        for b in BINDINGS {
            let hits = BINDINGS.iter().filter(|o| o.action == b.action).count();
            assert_eq!(hits, 1, "{:?} is documented {hits} times", b.action);
        }
    }

    #[test]
    fn chord_labels_follow_one_convention() {
        // A single spelling convention, checked mechanically: modifiers in
        // Ctrl-then-Shift order, no spaces, upper-case only when SHIFT is in
        // the chord. Anything else is a typo waiting to mislead someone.
        for b in BINDINGS {
            for c in b.chords {
                let l = c.label();
                assert!(!l.contains(' ') || l == "Space", "{l:?} has a stray space");
                assert!(
                    !l.contains("Shift+Ctrl"),
                    "{l}: modifier order is Ctrl+Shift+"
                );
                let ctrl_at = l.find("Ctrl+");
                let shift_at = l.find("Shift+");
                if let (Some(cs), Some(sc)) = (ctrl_at, shift_at) {
                    assert!(cs < sc, "{l}: Ctrl must come before Shift");
                }
                // A lower-case letter after Shift+ would mean the modifier is
                // claimed but the glyph is not shifted. Named keys (`Left`,
                // `Tab`) have no glyph to case, so only single letters check.
                if let Some([c]) = l.strip_prefix("Shift+").and_then(|r| {
                    let mut it = r.chars();
                    match (it.next(), it.next()) {
                        (Some(c), None) => Some([c]),
                        _ => None,
                    }
                }) {
                    assert!(
                        c.is_uppercase() || !c.is_alphabetic(),
                        "{l}: Shift+ must be followed by the shifted glyph"
                    );
                }
            }
        }
    }

    // ---------- help overlay layout ----------

    #[test]
    fn help_overlay_lists_every_binding_once() {
        // The overlay is written by hand, so the hand has to be checked
        // against the table the keyboard actually resolves from: every
        // binding's primary chord must appear on the page, or the two have
        // drifted apart and the operator is reading a lie.
        let text: String = help_lines()
            .iter()
            .map(|l| {
                let mut s: String = l.spans.iter().map(|s| s.content.as_ref()).collect();
                s.push('\n');
                s
            })
            .collect();
        for b in BINDINGS {
            let primary = b.chords[0].label();
            assert!(
                text.contains(&primary),
                "{primary} is missing from the overlay"
            );
        }
    }

    #[test]
    fn help_overlay_fits_eighty_columns() {
        // The old overlay was 24 lines of hand-aligned text that overflowed an
        // 80x24 terminal. The new one must fit both a common viewport.
        let widths: Vec<usize> = help_lines().iter().map(Line::width).collect();
        let widest = widths.iter().copied().max().unwrap_or(0);
        assert!(widest <= 76, "overlay is {widest} cols wide (budget 76)");
        assert!(
            help_lines().len() <= 22,
            "overlay is {} lines tall (budget 22)",
            help_lines().len()
        );
    }

    #[test]
    fn overlay_key_columns_are_aligned() {
        // Within each column the chord gutter is computed from the widest
        // chord, so every doc must start at the same offset. The rows are
        // hand-written; the spacing under them is not, and this is what keeps
        // it that way.
        for column in [HELP_LEFT, HELP_RIGHT] {
            let column = help_column(column);
            let gutter = column
                .iter()
                .filter(|l| l.spans.len() >= 2)
                .map(|l| l.spans[0].content.len())
                .max()
                .unwrap_or(0);
            for l in &column {
                if l.spans.len() < 2 {
                    continue;
                }
                assert_eq!(
                    l.spans[0].content.len(),
                    gutter,
                    "ragged chord column: {:?}",
                    l.spans[0].content
                );
            }
        }
    }

    // --- Event-driven canvas sizing ---

    #[test]
    fn model_canvas_rect_leaves_room_for_both_bands() {
        // Rows 1..23 of a 24-row screen: row 0 is the telemetry band and the
        // last row is the statusline.
        assert_eq!(model_canvas_rect(80, 24), (0, 1, 80, 22));
        // Too short for both bands: the canvas collapses to nothing rather
        // than underflowing.
        assert_eq!(model_canvas_rect(80, 2), (0, 1, 80, 0));
        assert_eq!(model_canvas_rect(80, 1), (0, 1, 80, 0));
        assert_eq!(model_canvas_rect(80, 0), (0, 1, 80, 0));
    }

    #[test]
    fn engine_new_sizes_the_raster_to_the_model_canvas() {
        // No Resize event precedes the first frame, so `Engine::new` must hand
        // the rasterizer a canvas itself — otherwise the opening frame
        // rasterizes into a 0x0 grid and draws nothing.
        let engine = Engine::new(80, 24);
        // Rows 1..23: row 0 is the telemetry band, the last row the statusline.
        assert_eq!(engine.raster.canvas_size(), (80, 22));
    }

    #[test]
    fn engine_resize_carries_the_raster_to_the_new_canvas() {
        // A terminal Resize event is the only thing that changes the canvas,
        // so `Engine::resize` is where the rasterizer follows it.
        let mut engine = Engine::new(80, 24);
        engine.resize(120, 40);
        assert_eq!(engine.screen.size(), (120, 40));
        assert_eq!(engine.raster.canvas_size(), (120, 38));
    }

    #[test]
    fn compose_frame_draws_inside_the_canvas_after_a_resize() {
        // The stride the rasterizer paints with and the region it is handed
        // must be the same canvas: a grid left at the old size paints outside
        // the new bounds (or panics) instead of into them.
        let mut app = App::new(Model::default(), "cube".to_string(), None);
        let mut engine = Engine::new(80, 24);
        compose_frame(&mut app, &mut engine);
        engine.resize(120, 40);
        compose_frame(&mut app, &mut engine);

        let (w, h) = engine.screen.size();
        let is_braille =
            |x: usize, y: usize| ('\u{2800}'..='\u{28ff}').contains(&engine.screen.symbol(x, y));
        let dots = (0..h)
            .flat_map(|y| (0..w).map(move |x| (x, y)))
            .filter(|&(x, y)| is_braille(x, y))
            .count();
        assert!(dots > 0, "the model must still be drawn after a resize");
        for y in [0, h - 1] {
            for x in 0..w {
                assert!(
                    !is_braille(x, y),
                    "the canvas must not paint over its bands: ({x},{y})"
                );
            }
        }
    }

    #[test]
    fn help_panel_occludes_only_the_cells_it_covers() {
        // Pressing `?` used to blank the canvas. The panel must instead claim
        // exactly its own rectangle on top of the model: whatever it covers is
        // occluded, and every cell outside it draws as if `?` were never
        // pressed — the two frames differ only inside the panel.
        let compose = |overlay: bool| {
            let mut app = App::new(Model::default(), "cube".to_string(), None);
            app.hud = if overlay {
                Hud::Expanded
            } else {
                Hud::Collapsed
            };
            let mut engine = Engine::new(120, 40);
            compose_frame(&mut app, &mut engine);
            engine
        };
        let plain = compose(false);
        let over = compose(true);

        let is_braille = |e: &Engine, x: usize, y: usize| {
            ('\u{2800}'..='\u{28ff}').contains(&e.screen.symbol(x, y))
        };
        let braille = |e: &Engine| {
            let (w, h) = e.screen.size();
            (0..h)
                .flat_map(|y| (0..w).map(move |x| (x, y)))
                .filter(|&(x, y)| is_braille(e, x, y))
                .count()
        };
        assert!(braille(&plain) > 0, "the empty scene must draw its axes");

        // Find the panel by its rounded corners and measure it on screen,
        // rather than re-deriving the layout math the renderer just ran.
        let (w, h) = over.screen.size();
        let (px, py) = (0..h)
            .flat_map(|y| (0..w).map(move |x| (x, y)))
            .find(|&(x, y)| over.screen.symbol(x, y) == '╭')
            .expect("the help panel must be on screen");
        let mut pw = 1;
        while px + pw < w && matches!(over.screen.symbol(px + pw, py), '─' | '╮') {
            pw += 1;
        }
        let mut ph = 1;
        while py + ph < h && matches!(over.screen.symbol(px, py + ph), '│' | '╰') {
            ph += 1;
        }

        let mut covered = 0usize; // model cells the panel hides
        let mut leaked = 0usize; // model cells surviving under the panel
        let mut touched = 0usize; // cells outside the panel the overlay altered
        for y in 0..h {
            for x in 0..w {
                if x >= px && x < px + pw && y >= py && y < py + ph {
                    leaked += usize::from(is_braille(&over, x, y));
                    covered += usize::from(is_braille(&plain, x, y));
                } else if over.screen.cell(x, y) != plain.screen.cell(x, y) {
                    touched += 1;
                }
            }
        }
        assert!(covered > 0, "the panel must sit over part of the model");
        assert_eq!(leaked, 0, "the panel must occlude the model underneath");
        assert_eq!(
            touched, 0,
            "outside the panel the frame must be identical to the one without the overlay"
        );
    }

    // ---------- statusline ----------

    /// The statusline as the terminal would show it: one `(char, fg, bg)` per
    /// cell, in order — the same cells the screen gets.
    fn status_cells(app: &App, width: u16) -> Vec<(char, Color, Color)> {
        status_cells_for(app, width)
    }

    /// The statusline's visible text.
    fn status_text(app: &App, width: u16) -> String {
        status_cells(app, width)
            .into_iter()
            .map(|(c, _, _)| c)
            .collect()
    }

    /// The ink of the cell where `needle` starts, so a test can assert what a
    /// lamp or a hint is actually drawn in.
    fn status_ink_of(app: &App, width: u16, needle: &str) -> Color {
        let cells = status_cells(app, width);
        let text: String = cells.iter().map(|(c, _, _)| *c).collect();
        let byte = text
            .find(needle)
            .unwrap_or_else(|| panic!("{needle:?} is not on the strip: {text:?}"));
        cells[text[..byte].chars().count()].1
    }

    fn status_app(name: &str) -> App {
        App::new(Model::default(), name.to_string(), None)
    }

    #[test]
    fn the_empty_state_is_named_on_the_statusline() {
        // Started with no FILE on a terminal: there is no model to name, so
        // the pill says so rather than leaving the strip anonymous.
        let app = App::new(Model::default(), "no file".to_string(), None);
        assert!(
            status_text(&app, 80).starts_with(" no file "),
            "the pill names the empty state"
        );
        assert!(
            telemetry_line(&app.view).contains("dist="),
            "row 0 keeps reporting the camera either way"
        );
    }

    #[test]
    fn the_pill_prefers_the_stream_path_over_the_file_stem() {
        // A stream's model name is only the file stem ("stream"), so the FIFO
        // path is what tells two previews apart. It is the pill's label now
        // that Row 0 no longer shows one.
        let app = App::new(
            Model::default(),
            "stream".to_string(),
            Some("/tmp/stream.fifo".to_string()),
        );
        let text = status_text(&app, 100);
        assert!(text.starts_with(" /tmp/stream.fifo "), "pill: {text:?}");
    }

    #[test]
    fn the_pill_never_takes_more_than_half_the_row() {
        // Otherwise a long path would starve the lamps and the hints that
        // share the strip with it, at any width.
        let mut app = App::new(Model::default(), "cube".to_string(), None);
        app.strip_label = Some("/home/someone/very/deep/project/tree/preview.fifo".to_string());
        for width in [20u16, 40, 80] {
            let text = status_text(&app, width);
            // `find` counts bytes and the ellipsis is three of them.
            let pill = text[..text.find(WEDGE_CLOSE).expect("the pill's closing wedge")]
                .chars()
                .count();
            assert!(
                pill <= width as usize / 2,
                "the pill takes {pill} of {width} cells: {text:?}"
            );
            assert!(text.contains('…'), "a cut path is marked: {text:?}");
        }
        // Roomy enough, and the path is shown whole and unmarked.
        let text = status_text(&app, 200);
        assert!(
            text.contains("/home/someone/very/deep/project/tree/preview.fifo"),
            "the whole path is shown when it fits: {text:?}"
        );
        assert!(!text.contains('…'), "nothing was cut: {text:?}");
    }

    /// A bare modifier key event, as the kitty protocol reports it.
    fn modifier_event(m: ModifierKeyCode, kind: KeyEventKind) -> Event {
        let mut e = KeyEvent::new(KeyCode::Modifier(m), KeyModifiers::empty());
        e.kind = kind;
        Event::Key(e)
    }

    #[test]
    fn bare_modifiers_are_tracked_only_while_they_are_down() {
        let mut app = App::new(Model::default(), "cube".to_string(), None);
        app.dirty = false;
        app.handle_input(modifier_event(
            ModifierKeyCode::LeftControl,
            KeyEventKind::Press,
        ));
        assert_eq!(app.held_modifiers(), KeyModifiers::CONTROL);
        assert!(app.dirty, "a modifier press repaints the strip");

        // The other hand's Ctrl is the same modifier: nothing changes on
        // screen, and letting one of the two go keeps CONTROL held.
        app.dirty = false;
        app.handle_input(modifier_event(
            ModifierKeyCode::RightControl,
            KeyEventKind::Press,
        ));
        assert_eq!(app.held_modifiers(), KeyModifiers::CONTROL);
        assert!(!app.dirty, "the second Ctrl has nothing new to show");
        app.handle_input(modifier_event(
            ModifierKeyCode::LeftControl,
            KeyEventKind::Release,
        ));
        assert_eq!(app.held_modifiers(), KeyModifiers::CONTROL);
        assert!(
            app.release_seen,
            "a modifier release is still evidence the terminal reports key-up"
        );
        app.dirty = false;
        app.handle_input(modifier_event(
            ModifierKeyCode::RightControl,
            KeyEventKind::Release,
        ));
        assert_eq!(app.held_modifiers(), KeyModifiers::empty());
        assert!(app.dirty, "letting go repaints the strip");
    }

    #[test]
    fn losing_focus_drops_held_modifiers() {
        // The release of a modifier held across an alt-tab goes to the other
        // window, exactly like the release of a held motion key.
        let mut app = App::new(Model::default(), "cube".to_string(), None);
        app.handle_input(modifier_event(
            ModifierKeyCode::LeftShift,
            KeyEventKind::Press,
        ));
        assert_eq!(app.held_modifiers(), KeyModifiers::SHIFT);
        app.handle_input(Event::FocusLost);
        assert_eq!(app.held_modifiers(), KeyModifiers::empty());
    }

    #[test]
    fn a_held_modifier_switches_the_strip_hints() {
        let mut app = status_app("cube");
        let everyday = status_text(&app, 100);
        assert!(
            everyday.contains("? help"),
            "hints by default: {everyday:?}"
        );

        app.handle_input(modifier_event(
            ModifierKeyCode::LeftControl,
            KeyEventKind::Press,
        ));
        let ctrl = status_text(&app, 100);
        assert!(ctrl.contains("Ctrl+h/l/k/j/d/f"), "the Ctrl keys: {ctrl:?}");
        assert!(
            !ctrl.contains("? help"),
            "the everyday hints step aside: {ctrl:?}"
        );

        app.handle_input(modifier_event(
            ModifierKeyCode::LeftControl,
            KeyEventKind::Release,
        ));
        assert_eq!(
            status_text(&app, 100),
            everyday,
            "and they come back on release"
        );

        app.handle_input(modifier_event(
            ModifierKeyCode::RightShift,
            KeyEventKind::Press,
        ));
        let shift = status_text(&app, 100);
        assert!(
            shift.contains("Shift+h/l/k/j pan the view"),
            "the Shift keys: {shift:?}"
        );
        assert!(
            !shift.contains("frame the model"),
            "no binding wears Shift in the Frame group any more: {shift:?}"
        );
    }

    #[test]
    fn a_modifier_hint_only_names_keys_that_modifier_binds() {
        // The hint is derived from BINDINGS, so this pins the derivation: every
        // key it lists must resolve to a real action *with that modifier*, and
        // every chord wearing the modifier must be listed — otherwise the strip
        // would be advertising a keyboard that does not exist.
        for (mods, name) in [
            (KeyModifiers::CONTROL, "Ctrl+"),
            (KeyModifiers::SHIFT, "Shift+"),
        ] {
            let rows = modifier_hint(mods);
            assert!(!rows.is_empty(), "a hint for a bound modifier");
            let mut hinted = 0;
            for row in rows {
                let text: String = row.runs.iter().map(|(t, _)| t.as_str()).collect();
                assert!(text.starts_with(name), "{text:?} must open with {name:?}");
                let keys = text[name.len()..].split(' ').next().unwrap();
                for key in keys.split('/') {
                    // A shifted letter is bound as its upper-case glyph.
                    let glyph = if mods.contains(KeyModifiers::SHIFT) {
                        key.to_uppercase()
                    } else {
                        key.to_string()
                    };
                    let code = KeyCode::Char(glyph.chars().next().expect("one char"));
                    assert!(
                        resolve_key_event(code, mods).is_some(),
                        "{name}{key} is hinted but unbound"
                    );
                    hinted += 1;
                }
            }
            let bound = BINDINGS
                .iter()
                .filter(|b| b.chords[0].mods.contains(mods))
                .count();
            assert_eq!(hinted, bound, "every {name} chord is hinted once");
        }
    }

    #[test]
    fn status_line_fills_the_row_exactly() {
        // The strip is the one place Wireforge paints a background, so it has
        // to cover the row edge to edge: a cell short leaves a hole in the
        // band, a cell long gets clipped. Absurd widths are included so the
        // fit arithmetic can never underflow, and every hint set is tried,
        // because a held modifier brings a much wider one.
        let mut cases = vec![status_app("wireforge.wrfm")];
        for m in [
            ModifierKeyCode::LeftControl,
            ModifierKeyCode::LeftShift,
            ModifierKeyCode::LeftAlt,
        ] {
            let mut app = status_app("wireforge.wrfm");
            app.mods_down.push(m);
            cases.push(app);
        }
        for app in &cases {
            for width in 0..=200u16 {
                let line = status_line(app, width);
                assert_eq!(
                    line.width(),
                    width as usize,
                    "at width {width} with {:?}: {line:?}",
                    app.mods_down
                );
            }
        }
    }

    #[test]
    fn status_line_paints_a_ground_across_the_whole_row() {
        // Every cell must carry a ground, or the band breaks and the terminal
        // shows through mid-strip.
        let app = status_app("cube");
        for width in [1u16, 12, 40, 80, 200] {
            for (i, (ch, _, bg)) in status_cells(&app, width).into_iter().enumerate() {
                assert_ne!(
                    bg,
                    Color::Reset,
                    "cell {i} ({ch:?}) has no ground at width {width}"
                );
            }
        }
    }

    #[test]
    fn status_line_leads_with_the_model_name_pill() {
        let app = status_app("cube");
        let text = status_text(&app, 80);
        assert!(
            text.starts_with(" cube "),
            "the pill leads the strip: {text:?}"
        );
        // The pill drops one cell of accent in before its text, and that cell
        // carries the pill's own inks.
        let cells = status_cells(&app, 80);
        assert_eq!(cells[1].2, ACCENT, "pill ground");
        assert_eq!(cells[2].1, STRIP, "pill text ink");
    }

    #[test]
    fn status_line_lamps_carry_their_state_in_the_ink() {
        // Both readings are the same glyph, so the ink is the whole signal.
        let mut app = status_app("cube");
        app.auto_spin = false;
        app.show_axes = false;
        assert_eq!(
            status_ink_of(&app, 80, "● SPIN"),
            STRIP,
            "an off lamp is unlit"
        );
        assert_eq!(
            status_ink_of(&app, 80, "● AXES"),
            STRIP,
            "an off lamp is unlit"
        );

        app.auto_spin = true;
        app.show_axes = true;
        assert_eq!(
            status_ink_of(&app, 80, "● SPIN"),
            ACCENT,
            "a spinning lamp lights"
        );
        assert_eq!(
            status_ink_of(&app, 80, "● AXES"),
            ACCENT,
            "both lamps light in the same accent, so the ink means 'on'"
        );
    }

    #[test]
    fn status_line_hints_name_the_canonical_chord() {
        // The hints are read out of BINDINGS, so a rebind can never leave the
        // strip advertising a key that does nothing.
        let app = status_app("cube");
        let text = status_text(&app, 100);
        for (action, label) in [
            (Action::Help, "help"),
            (Action::Quit, "quit"),
            (Action::Spin, "spin"),
            (Action::Axes, "axes"),
        ] {
            let b = BINDINGS.iter().find(|b| b.action == action).expect("bound");
            let want = format!("{} {label}", b.chords[0].label());
            assert!(text.contains(&want), "{want:?} missing from {text:?}");
        }
    }

    #[test]
    fn status_line_sheds_hints_then_lamps_then_the_name() {
        let app = status_app("wireforge.wrfm");
        // Roomy: every hint is on the strip.
        let wide = status_text(&app, 100);
        for hint in ["? help", "q quit", "Space spin", "Tab axes"] {
            assert!(
                wide.contains(hint),
                "{hint:?} missing at 100 cols: {wide:?}"
            );
        }
        // Narrow: the hints go first, and the name outlives them.
        let narrow = status_text(&app, 40);
        assert!(
            !narrow.contains("Tab axes"),
            "the least useful hint sheds first: {narrow:?}"
        );
        assert!(
            narrow.contains("wireforge.wrfm"),
            "the name outlives the hints: {narrow:?}"
        );
        // Narrower still: the lamps go, and a cut name is marked as cut.
        let tiny = status_text(&app, 14);
        assert!(!tiny.contains("SPIN"), "the lamps go next: {tiny:?}");
        assert!(tiny.contains('…'), "a cut name is marked: {tiny:?}");
    }

    #[test]
    fn status_line_separates_the_sections_with_powerline_wedges() {
        // The wedge is what makes the look: one closing the pill into the
        // lamps, one closing the lamps back onto the strip, and one opening
        // the hints. A wedge is inked with the outgoing section's ground and
        // sits on the incoming one.
        let app = status_app("cube");
        let cells = status_cells(&app, 80);
        let wedges = |wanted: char| -> Vec<(Color, Color)> {
            cells
                .iter()
                .filter(|(c, _, _)| *c == wanted)
                .map(|(_, fg, bg)| (*fg, *bg))
                .collect()
        };
        assert_eq!(
            wedges(WEDGE_CLOSE),
            vec![(ACCENT, LAMP), (LAMP, STRIP)],
            "the pill hands its colour to the lamps, which hand theirs to the strip"
        );
        assert_eq!(
            wedges(WEDGE_OPEN),
            vec![(LAMP, STRIP)],
            "the hints arrive out of the strip"
        );
    }

    /// `REPORT_ALTERNATE_KEYS` is pushed, so on the kitty path a shifted chord
    /// arrives as the glyph the active layout types with SHIFT **cleared**
    /// (crossterm applies the alternate and drops the modifier). That is the
    /// legacy shape minus its synthesised SHIFT, so it folds into the same
    /// identity and action — listed chord or not.
    #[test]
    fn alternate_key_glyphs_fold_into_the_same_chords() {
        use KeyCode::*;
        let bound = [
            (Char('H'), Char('h'), Action::Motion(Motion::MoveLeft)),
            (Char('?'), Char('/'), Action::Help),
            (Char('_'), Char('-'), Action::Motion(Motion::MoveBack)),
            (Char('+'), Char('='), Action::Motion(Motion::MoveForward)),
        ];
        for (glyph, id, expected) in bound {
            assert_eq!(
                resolve_key_event(glyph, KeyModifiers::NONE),
                Some((id, expected)),
                "glyph {glyph:?} must fold into its listed chord"
            );
        }
        // Chords that are NOT listed stay silent in that shape as well:
        // `Shift+0` types `)` and binds nothing, `Shift+q` must not quit.
        assert_eq!(resolve_key_event(Char(')'), KeyModifiers::NONE), None);
        assert_eq!(resolve_key_event(Char('Q'), KeyModifiers::NONE), None);
    }
    #[test]
    fn app_any_key_event_refreshes_every_hold() {
        // Without key-up reporting, the OS repeats only the most recently
        // pressed key, so `l` going down silences `j`. Refreshing per key
        // would drop `j` mid-hold, which is exactly the reported bug: the
        // model stops moving down after the timeout but keeps yawing.
        let mut app = App::new(
            Model {
                vertices: vec![(0.0, 0.0, 0.0), (1.0, 0.0, 0.0)],
                edges: vec![(0, 1)],
            },
            "cube".to_string(),
            None,
        );
        for key in ['j', 'l'] {
            app.handle_input(Event::Key(KeyEvent::new(
                KeyCode::Char(key),
                KeyModifiers::NONE,
            )));
        }
        assert_eq!(app.held.len(), 2, "both motion keys are held");
        let first = app
            .held
            .get(&KeyCode::Char('j'))
            .expect("j must hold the pitch motion")
            .seen;
        // Two seconds later only `l` is still repeating.
        let later = first + Duration::from_secs(2);
        app.note_key_event(later);
        app.update_held(later, 0.016);
        assert_eq!(
            app.held.len(),
            2,
            "the key that stopped repeating must survive"
        );
        // Silence past the timeout still ends both, so a lost key-up can
        // never leave the model spinning on its own.
        app.update_held(
            later + LEGACY_HOLD_TIMEOUT + Duration::from_millis(1),
            0.016,
        );
        assert!(app.held.is_empty(), "silence must still end the holds");
    }
    #[test]
    fn app_focus_lost_drops_held_keys() {
        // The key-up of a key held across an alt-tab reaches the other
        // window, so no Release arrives: FocusLost is the only signal.
        let mut app = App::new(
            Model {
                vertices: vec![(0.0, 0.0, 0.0), (1.0, 0.0, 0.0)],
                edges: vec![(0, 1)],
            },
            "cube".to_string(),
            None,
        );
        app.handle_input(Event::Key(KeyEvent::new(
            KeyCode::Right,
            KeyModifiers::NONE,
        )));
        assert_eq!(app.held.len(), 1);
        app.dirty = false;
        assert!(!app.handle_input(Event::FocusLost));
        assert!(app.held.is_empty(), "focus loss must stop every hold");
        assert!(app.dirty, "the cleared hold must repaint");
    }

    /// The model is immutable once loaded, so its extent is measured once and
    /// reused: the axes, the auto-fit distance and the pan/zoom step must all
    /// read that cached value instead of re-scanning every vertex per frame.
    #[test]
    fn app_caches_the_model_extent_for_every_frame_use() {
        // A +-1 cube: the geometric mean of (2,2,2) is 2.
        let cube = || Model {
            vertices: vec![
                (1.0, 1.0, 1.0),
                (1.0, -1.0, -1.0),
                (-1.0, 1.0, -1.0),
                (-1.0, -1.0, 1.0),
            ],
            edges: vec![(0, 1), (0, 2), (0, 3), (1, 2), (2, 3), (3, 1)],
        };
        let mut app = App::new(cube(), "cube".to_string(), None);
        assert_eq!(app.extent, 2.0, "the extent is cached at construction");

        // The framing distance comes from that same cached extent, so it cannot
        // drift from what re-measuring the model would have produced.
        let expected = wrfm_raster::geometry::auto_dist_from_extent(app.extent);
        assert_eq!(
            app.view.dist, expected,
            "App::new must frame from the cached extent"
        );

        // The pan/zoom step is the cached extent, not a per-frame scan.
        assert_eq!(app.move_scale(), app.extent);

        // Key `c` re-fits from the cache too, rather than a fresh bounds scan.
        app.view.dist = 1234.0;
        app.handle_input(Event::Key(KeyEvent::new(
            KeyCode::Char('c'),
            KeyModifiers::NONE,
        )));
        assert_eq!(
            app.view.dist, expected,
            "reset must re-fit from the cached extent"
        );
    }

    #[test]
    fn app_handle_input_motion_press_starts_continuous() {
        let mut app = App::new(
            Model {
                vertices: vec![(0.0, 0.0, 0.0), (1.0, 0.0, 0.0)],
                edges: vec![(0, 1)],
            },
            "cube".to_string(),
            None,
        );
        let yaw0 = app.view.yaw;
        // Press: enter continuous state (motion applied by update_held)
        app.handle_input(Event::Key(KeyEvent::new(
            KeyCode::Right,
            KeyModifiers::NONE,
        )));
        assert!(
            app.held.contains_key(&KeyCode::Right),
            "press must enter continuous state"
        );
        // Auto-repeat: must not add a new entry
        app.handle_input(Event::Key(KeyEvent::new(
            KeyCode::Right,
            KeyModifiers::NONE,
        )));
        assert_eq!(
            app.held.len(),
            1,
            "auto-repeat must not accumulate held entries"
        );
        // Continuous motion is applied by update_held
        app.update_held(Instant::now(), 0.016);
        assert_ne!(
            app.view.yaw, yaw0,
            "held key must rotate continuously immediately"
        );
    }
    #[test]
    fn app_handle_input_toggles() {
        let mut app = App::new(
            Model {
                vertices: vec![(0.0, 0.0, 0.0), (1.0, 0.0, 0.0)],
                edges: vec![(0, 1)],
            },
            "cube".to_string(),
            None,
        );
        app.dirty = false;

        // '?' toggles the help overlay and marks the screen dirty.
        assert!(!app.handle_input(Event::Key(KeyEvent::new(
            KeyCode::Char('?'),
            KeyModifiers::NONE
        ))));
        assert_eq!(app.hud, Hud::Expanded);
        assert!(app.dirty);

        app.dirty = false;
        app.handle_input(Event::Key(KeyEvent::new(
            KeyCode::Char('?'),
            KeyModifiers::NONE,
        )));
        assert_eq!(app.hud, Hud::Collapsed);

        // Shift+'/' also arrives as the base key plus SHIFT (a path without
        // alternate keys), so that shape must toggle too.
        app.dirty = false;
        app.handle_input(Event::Key(KeyEvent::new(
            KeyCode::Char('/'),
            KeyModifiers::SHIFT,
        )));
        assert_eq!(app.hud, Hud::Expanded);
        assert!(app.dirty);
        app.handle_input(Event::Key(KeyEvent::new(
            KeyCode::Char('/'),
            KeyModifiers::SHIFT,
        )));
        assert_eq!(app.hud, Hud::Collapsed);

        // Space toggles auto-spin.
        app.handle_input(Event::Key(KeyEvent::new(
            KeyCode::Char(' '),
            KeyModifiers::NONE,
        )));
        assert!(app.auto_spin);
        app.handle_input(Event::Key(KeyEvent::new(
            KeyCode::Char(' '),
            KeyModifiers::NONE,
        )));
        assert!(!app.auto_spin);

        // Tab toggles the axes.
        let before = app.show_axes;
        app.handle_input(Event::Key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE)));
        assert_ne!(app.show_axes, before);

        // q / Esc quit.
        assert!(app.handle_input(Event::Key(KeyEvent::new(
            KeyCode::Char('q'),
            KeyModifiers::NONE
        ))));
        assert!(app.handle_input(Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))));
    }
    #[test]
    fn app_held_key_expires_without_release() {
        // A terminal without the kitty protocol never sends Release, so the
        // hold timeout is the only thing that can stop the motion.
        let mut app = App::new(
            Model {
                vertices: vec![(0.0, 0.0, 0.0), (1.0, 0.0, 0.0)],
                edges: vec![(0, 1)],
            },
            "cube".to_string(),
            None,
        );
        app.handle_input(Event::Key(KeyEvent::new(
            KeyCode::Right,
            KeyModifiers::NONE,
        )));
        let seen = app
            .held
            .get(&KeyCode::Right)
            .expect("press must hold the motion")
            .seen;
        // Exactly at the timeout the key is still down (the bound is <=).
        app.update_held(seen + LEGACY_HOLD_TIMEOUT, 0.016);
        assert_eq!(app.held.len(), 1, "the timeout boundary must keep the hold");
        // One tick past it the key is gone and the model stops by itself.
        app.update_held(seen + LEGACY_HOLD_TIMEOUT + Duration::from_millis(1), 0.016);
        assert!(
            app.held.is_empty(),
            "a key that stopped reporting must expire"
        );
    }
    #[test]
    fn app_key_release_removes_held_entry() {
        let mut app = App::new(
            Model {
                vertices: vec![(0.0, 0.0, 0.0), (1.0, 0.0, 0.0)],
                edges: vec![(0, 1)],
            },
            "cube".to_string(),
            None,
        );
        // Press -> held; Release -> removed immediately (Kitty protocol).
        let press = Event::Key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
        app.handle_input(press);
        assert_eq!(app.held.len(), 1);
        let mut release = KeyEvent::new(KeyCode::Right, KeyModifiers::NONE);
        release.kind = KeyEventKind::Release;
        app.handle_input(Event::Key(release));
        assert!(app.held.is_empty(), "release must stop motion immediately");
    }
    #[test]
    fn app_key_repeat_only_refreshes_the_hold() {
        // The kitty protocol reports auto-repeat while a key is held; the
        // one-shot toggles must fire once per tap instead of on every repeat.
        let mut app = App::new(
            Model {
                vertices: vec![(0.0, 0.0, 0.0), (1.0, 0.0, 0.0)],
                edges: vec![(0, 1)],
            },
            "cube".to_string(),
            None,
        );
        app.handle_input(Event::Key(KeyEvent::new(
            KeyCode::Char(' '),
            KeyModifiers::NONE,
        )));
        assert!(app.auto_spin, "press must toggle auto-spin");
        let mut repeat = KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE);
        repeat.kind = KeyEventKind::Repeat;
        app.handle_input(Event::Key(repeat));
        assert!(app.auto_spin, "auto-repeat must not re-trigger a toggle");

        // A repeat of a motion key refreshes the hold instead.
        app.handle_input(Event::Key(KeyEvent::new(
            KeyCode::Right,
            KeyModifiers::NONE,
        )));
        let seen = app
            .held
            .get(&KeyCode::Right)
            .expect("press must hold the motion")
            .seen;
        let mut repeat = KeyEvent::new(KeyCode::Right, KeyModifiers::NONE);
        repeat.kind = KeyEventKind::Repeat;
        app.handle_input(Event::Key(repeat));
        let refreshed = app
            .held
            .get(&KeyCode::Right)
            .expect("repeat must keep the hold")
            .seen;
        assert!(refreshed >= seen, "auto-repeat must refresh the hold");
    }
    #[test]
    fn app_release_seen_holds_never_time_out() {
        // Once the terminal reports key-up, Release (and FocusLost) end a
        // hold. A timeout could only ever drop a key that is still down.
        let mut app = App::new(
            Model {
                vertices: vec![(0.0, 0.0, 0.0), (1.0, 0.0, 0.0)],
                edges: vec![(0, 1)],
            },
            "cube".to_string(),
            None,
        );
        let mut release = KeyEvent::new(KeyCode::Right, KeyModifiers::NONE);
        release.kind = KeyEventKind::Release;
        app.handle_input(Event::Key(release));
        assert!(app.release_seen, "a Release must enable key-up reporting");
        app.handle_input(Event::Key(KeyEvent::new(
            KeyCode::Right,
            KeyModifiers::NONE,
        )));
        let seen = app
            .held
            .get(&KeyCode::Right)
            .expect("press must hold the motion")
            .seen;
        // A minute of silence must not stop the rotation...
        app.update_held(seen + Duration::from_secs(60), 0.016);
        assert_eq!(
            app.held.len(),
            1,
            "a hold must survive silence once key-up is reported"
        );
        // ...and the Release is what ends it.
        let mut release = KeyEvent::new(KeyCode::Right, KeyModifiers::NONE);
        release.kind = KeyEventKind::Release;
        app.handle_input(Event::Key(release));
        assert!(app.held.is_empty(), "release must stop the motion");
    }
    #[test]
    fn load_model_from_text_garbage_fails() {
        let err = load_model_from_text("stream", "garbage here\nno markers\n")
            .expect_err("garbage stream must fail");
        assert!(err.contains("unrecognized"), "error: {err}");
    }
    #[test]
    fn load_model_from_text_obj_errors_with_a_convert_hint() {
        let text = "v 0 0 0\nv 1 0 0\nv 0 1 0\nf 1 2 3\n";
        let err = load_model_from_text("cube", text).expect_err("obj stream must not load");
        assert!(err.contains("wrfm convert"), "error: {err}");
    } // ---------- event loop (input / holds) ----------

    #[test]
    fn load_model_from_text_wrfm_parses() {
        let (mode, name) = load_model_from_text(
            "stream",
            "wrfm 2\nvertices 2   edges 1\n\nv 0 0 0\nv 1 1 1\ne 0 1\n",
        )
        .expect("wrfm stream must load");
        let m = mode;
        assert_eq!(m.vertices.len(), 2);
        assert_eq!(m.edges.len(), 1);
        assert_eq!(name, "stream");
    }
    #[test]
    fn plain_keys_are_the_viewers_frame_ctrl_the_models() {
        // The model faces out of the screen (+Z) while the sight line points
        // into it: plain h/r must read as the viewer's left/right, Ctrl as
        // the model's own — mirror images at the default view. Pitch has no
        // mirror: the model's up is the viewer's up, so k/j agree.
        let mut v = ViewState::default();
        apply_motion_step(&mut v, Motion::YawLeft, 0.3, 0.0);
        // The nose is the third column of the model->world matrix.
        assert!(v.rot[0][2] < 0.0, "plain h must sweep the nose screen-left");

        let mut v = ViewState::default();
        apply_motion_step(&mut v, Motion::LocalYawLeft, 0.3, 0.0);
        assert!(
            v.rot[0][2] > 0.0,
            "Ctrl+h must yaw to the model's own left (screen-right at default)"
        );

        let mut v = ViewState::default();
        apply_motion_step(&mut v, Motion::RollPlus, 0.3, 0.0);
        assert!(
            v.roll < 0.0,
            "plain d rolls about the sight line: the viewer's right dips (image CW)"
        );

        let mut v = ViewState::default();
        apply_motion_step(&mut v, Motion::LocalRollPlus, 0.3, 0.0);
        // Starboard is the model's local -X; a right bank dips it (y < 0).
        assert!(-v.rot[1][0] < 0.0, "Ctrl+d must dip the model's starboard");

        let mut v = ViewState::default();
        apply_motion_step(&mut v, Motion::PitchUp, 0.3, 0.0);
        assert!(v.rot[1][2] > 0.0, "k must tip the nose up in both frames");
    }
    #[test]
    fn probe_bytes_garbage_is_unrecognized() {
        // A stream of garbage (no magic, no obj markers) is "unrecognized" —
        // never an empty model; a magic-less `v`/`e` stream is likewise not
        // wrfm (the v2 breaking change).
        let err = probe_bytes(b"this is not a wireframe\n").unwrap_err();
        assert!(err.contains("unrecognized"), "error: {err}");
        assert!(probe_bytes(b"v 0 0 0\nv 1 1 1\ne 0 1\n").is_err());
        assert!(probe_bytes(b"").is_err(), "empty stream is unrecognized");
    }
    /// OBJ content must point at the converter — wireforge itself reads
    /// .wrfm only (the ratty/OBJ rendering path is gone).
    #[test]
    fn probe_bytes_obj_content_points_at_the_converter() {
        let err = probe_bytes(b"v 0 0 0\nv 1 0 0\nf 1 2 3\n").unwrap_err();
        assert!(err.contains("wrfm convert"), "hint: {err}");
        // The v-without-e heuristic (a bare point cloud) routes there too.
        let err = probe_bytes(b"v 0 0 0\nv 1 1 1\n").unwrap_err();
        assert!(err.contains("wrfm convert"), "hint: {err}");
    }
    #[test]
    fn probe_bytes_wrfm_magic() {
        // The buffer probe (used by stdin/FIFO) is the same content-first
        // authority as the file probe: `wrfm <version>` first line -> wrfm.
        let buf = b"wrfm 2\nvertices 2   edges 1\n\nv 0 0 0\nv 1 1 1\ne 0 1\n";
        probe_bytes(buf).expect("wrfm magic probes as wrfm");
        // The magic wins even with obj-looking markers later.
        let buf2 = b"wrfm 2\nf 1 2 3\n";
        probe_bytes(buf2).expect("magic wins over obj markers");
    }
    /// SHIFT is the only modifier that changes an action: Ctrl binds quitting
    /// (raw mode delivers Ctrl+C / Ctrl+Q as key events, with no SIGINT) plus
    /// the local-frame rotation letters, and every other modifier swallows the
    /// key.
    #[test]
    fn resolve_key_event_ctrl_quits_or_rotates_locally_and_alt_swallows() {
        use KeyCode::*;
        assert!(matches!(
            resolve_key_event(Char('c'), KeyModifiers::CONTROL),
            Some((_, Action::Quit))
        ));
        assert!(matches!(
            resolve_key_event(Char('q'), KeyModifiers::CONTROL),
            Some((_, Action::Quit))
        ));
        // Ctrl+h / Ctrl+Left rotate in the model's own (local) frame;
        // Alt+h must do nothing either.
        assert_eq!(
            resolve_key_event(Char('h'), KeyModifiers::CONTROL),
            Some((Char('h'), Action::Motion(Motion::LocalYawLeft)))
        );
        assert_eq!(
            resolve_key_event(Left, KeyModifiers::CONTROL),
            Some((Left, Action::Motion(Motion::LocalYawLeft)))
        );
        assert_eq!(resolve_key_event(Char('h'), KeyModifiers::ALT), None);
        assert_eq!(resolve_key_event(Char(' '), KeyModifiers::ALT), None);
        // Shift+Tab arrives as BackTab on both paths and still toggles the axes.
        assert!(matches!(
            resolve_key_event(BackTab, KeyModifiers::SHIFT),
            Some((_, Action::Axes))
        ));
    }
    /// The same physical input can arrive in two shapes: the **base** key plus
    /// SHIFT (a path without alternate keys — still accepted) or the shifted
    /// **glyph** (what a legacy terminal sends, SHIFT synthesised for
    /// upper-case letters, and what the kitty path now sends for keys with an
    /// alternate, SHIFT cleared). Both must resolve alike — to the same action
    /// under the same identity when the chord is listed, or to nothing at all
    /// when it is not (`Shift+0` types `)` and `)` binds nothing).
    #[test]
    fn resolve_key_event_folds_both_terminal_encodings() {
        use KeyCode::*;
        // Each row is (base+SHIFT form, shifted-glyph form, expected or None).
        let cases = [
            // Listed shifted chords: they bind, in either shape.
            (
                (Char('h'), KeyModifiers::SHIFT),
                (Char('H'), KeyModifiers::SHIFT),
                Some(Action::Motion(Motion::MoveLeft)),
            ),
            (
                (Char('/'), KeyModifiers::SHIFT),
                (Char('?'), KeyModifiers::NONE),
                Some(Action::Help),
            ),
            (
                (Char('-'), KeyModifiers::SHIFT),
                (Char('_'), KeyModifiers::NONE),
                Some(Action::Motion(Motion::MoveBack)),
            ),
            (
                (Char('='), KeyModifiers::SHIFT),
                (Char('+'), KeyModifiers::NONE),
                Some(Action::Motion(Motion::MoveForward)),
            ),
            // Shifted chords that are NOT listed: nothing happens, in either shape.
            (
                (Char('0'), KeyModifiers::SHIFT),
                (Char(')'), KeyModifiers::NONE),
                None,
            ),
            (
                (Char('q'), KeyModifiers::SHIFT),
                (Char('Q'), KeyModifiers::SHIFT),
                None,
            ),
            (
                (Char('r'), KeyModifiers::SHIFT),
                (Char('R'), KeyModifiers::SHIFT),
                None,
            ),
        ];
        for ((base_code, base_mods), (glyph_code, glyph_mods), expected) in cases {
            let label = format!("{base_code:?}+{base_mods:?} vs {glyph_code:?}+{glyph_mods:?}");
            let base = resolve_key_event(base_code, base_mods);
            let glyph = resolve_key_event(glyph_code, glyph_mods);
            assert_eq!(
                base.as_ref().map(|(_, action)| *action),
                expected,
                "base+SHIFT: {label}"
            );
            assert_eq!(
                glyph.as_ref().map(|(_, action)| *action),
                expected,
                "glyph: {label}"
            );
            if let (Some((base_id, _)), Some((glyph_id, _))) = (&base, &glyph) {
                assert_eq!(base_id, glyph_id, "both must hold the same key: {label}");
            }
        }
    }
    /// Shift is the only chord component that gained a restriction, so every
    /// plain key must still bind to its documented action.
    #[test]
    fn resolve_key_event_keeps_every_unshifted_key_bound() {
        use KeyCode::*;
        let documented = [
            (Char('q'), Action::Quit),
            (Esc, Action::Quit),
            (Char(' '), Action::Spin),
            (Tab, Action::Axes),
            (Char('c'), Action::Reset),
            (Char('d'), Action::Motion(Motion::RollPlus)),
            (Char('f'), Action::Motion(Motion::RollMinus)),
            (Char('h'), Action::Motion(Motion::YawLeft)),
            (Char('='), Action::Motion(Motion::MoveForward)),
            (Char('-'), Action::Motion(Motion::MoveBack)),
            (Left, Action::Motion(Motion::YawLeft)),
        ];
        for (code, expected) in documented {
            let (_, action) = resolve_key_event(code, KeyModifiers::NONE)
                .unwrap_or_else(|| panic!("{code:?} must stay bound"));
            assert_eq!(action, expected, "{code:?}");
        }
    }
    /// `r` and `e` gave their keys to `d`/`f`; nothing — plain or with Ctrl —
    /// may fire from them any more.
    #[test]
    fn r_and_e_are_unbound() {
        use KeyCode::*;
        for (code, mods) in [
            (Char('r'), KeyModifiers::NONE),
            (Char('e'), KeyModifiers::NONE),
            (Char('r'), KeyModifiers::CONTROL),
            (Char('e'), KeyModifiers::CONTROL),
        ] {
            assert!(
                resolve_key_event(code, mods).is_none(),
                "{code:?}+{mods:?} must bind nothing"
            );
        }
    }
    /// Regression: `held` used to be keyed by Motion, so a modifier change
    /// between Press and Release made key-up resolve to a *different* motion —
    /// the original one kept running, and with key-up reporting there is no
    /// timeout left to stop it.
    #[test]
    fn shift_change_mid_hold_cannot_leave_a_hold_running() {
        let mut app = App::new(
            Model {
                vertices: vec![(0.0, 0.0, 0.0), (1.0, 0.0, 0.0)],
                edges: vec![(0, 1)],
            },
            "cube".to_string(),
            None,
        );
        // Press `h` (rotate), then press Shift: the Repeat now spells "move",
        // but it is still the same key, so there is exactly one hold — and a
        // Repeat never re-samples the motion it started with.
        app.handle_input(Event::Key(KeyEvent::new(
            KeyCode::Char('h'),
            KeyModifiers::NONE,
        )));
        let mut shifted = KeyEvent::new(KeyCode::Char('h'), KeyModifiers::SHIFT);
        shifted.kind = KeyEventKind::Repeat;
        app.handle_input(Event::Key(shifted));
        assert_eq!(app.held.len(), 1, "one key must not fork into two motions");
        assert!(matches!(
            app.held[&KeyCode::Char('h')].motion,
            Motion::YawLeft
        ));
        // Release the key while Shift is still down: key-up carries SHIFT and
        // must still find the hold.
        let mut release = KeyEvent::new(KeyCode::Char('h'), KeyModifiers::SHIFT);
        release.kind = KeyEventKind::Release;
        app.handle_input(Event::Key(release));
        assert!(app.held.is_empty(), "shifted key-up must stop the hold");

        // The other order: press Shift+h (move), release Shift first, and the
        // key-up arrives WITHOUT SHIFT. Matching on the motion here is what
        // used to leave the model panning forever.
        app.handle_input(Event::Key(KeyEvent::new(
            KeyCode::Char('h'),
            KeyModifiers::SHIFT,
        )));
        assert!(matches!(
            app.held[&KeyCode::Char('h')].motion,
            Motion::MoveLeft
        ));
        let mut release = KeyEvent::new(KeyCode::Char('h'), KeyModifiers::NONE);
        release.kind = KeyEventKind::Release;
        app.handle_input(Event::Key(release));
        assert!(app.held.is_empty(), "unshifted key-up must stop the hold");
    }
}
