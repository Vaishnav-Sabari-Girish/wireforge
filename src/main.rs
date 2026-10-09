use clap::{CommandFactory, Parser};
use crossterm::{
    cursor::{Hide, Show},
    event::{
        self, DisableFocusChange, EnableFocusChange, Event, KeyCode, KeyEventKind, KeyModifiers,
        KeyboardEnhancementFlags, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
    },
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    widgets::{Block, BorderType, Borders, Padding, Paragraph, Widget},
};

use std::{
    collections::HashMap,
    error::Error,
    io::{self, IsTerminal, Read},
    path::{Path, PathBuf},
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
    about = "TUI editor and viewer for .wrfm 3D models"
)]
struct Args {
    /// `.wrfm` file to open, or `-` to read from stdin (the default when stdin is not a terminal)
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
    // Rotation (world-frame: fixed world axes; h/l/r/e read left/right as
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
    Center,
    Fit,
    Reset,
    Motion(Motion),
}

/// Fold one terminal key event into the key's identity (its *base*, un-shifted
/// form) plus whether SHIFT is in effect, so every later decision sees one
/// protocol-independent shape instead of two:
///
/// * the kitty protocol sends the layout's **shifted glyph** for every key
///   that has an alternate (`REPORT_ALTERNATE_KEYS` is pushed): `Shift+h` ->
///   `Char('H')`, `Shift+0` -> `Char(')')` — crossterm applies the alternate
///   and clears SHIFT. Keys without an alternate keep the modifier instead
///   (`Shift+Left` -> `Left` + SHIFT, `Shift+Space` -> `Char(' ')` + SHIFT);
/// * a legacy terminal sends the shifted **glyph** too, and crossterm
///   additionally synthesises SHIFT for upper-case letters (`Shift+h` ->
///   `Char('H')` + SHIFT, `Shift+/` -> `Char('?')` with NO modifier at all).
///
/// So an upper-case char implies shift, a known shifted glyph folds back to
/// its base key + shift, and anything else keeps the SHIFT modifier. The glyph
/// table maps the four shifted glyphs the bindings use back to their US base
/// keys — the layouts' own glyphs arrive unchanged, so the same table serves
/// both paths. Caps lock therefore counts as shift on the legacy path and is
/// a no-op under kitty (crossterm files the kitty caps-lock bit under
/// `KeyEventState`, which is never read here) — the two cannot be told apart
/// from here.
fn canonical_key(code: KeyCode, mods: KeyModifiers) -> (KeyCode, bool) {
    use KeyCode::Char;
    let shift = mods.contains(KeyModifiers::SHIFT);
    match code {
        Char(c) if c.is_uppercase() => (Char(c.to_lowercase().next().unwrap_or(c)), true),
        Char('?') => (Char('/'), true),
        Char('_') => (Char('-'), true),
        Char('+') => (Char('='), true),
        Char(')') => (Char('0'), true),
        _ => (code, shift),
    }
}

/// Map one terminal key event to the identity its hold is stored under and the
/// action it performs; `None` for an unbound key (which includes the modifier
/// keys themselves, reported by the kitty protocol).
///
/// SHIFT is the only modifier that changes an action (see `canonical_key`),
/// and a shifted chord acts only where the table below lists it: `Shift+0` is
/// `)` and does nothing, while `Shift+h` is the documented pan. Ctrl binds
/// quitting (raw mode delivers Ctrl+C / Ctrl+Q as key events, with no
/// SIGINT) plus the local-frame rotation chords `Ctrl` + arrows / `hjkl` /
/// `e` / `r` — rotation around the model's own axes instead of the world's;
/// every other modifier swallows the key, so a stray `Alt+h` can never
/// rotate the model.
fn resolve_key_event(code: KeyCode, mods: KeyModifiers) -> Option<(KeyCode, Action)> {
    use KeyCode::*;
    // Alt / Super / Hyper / Meta bind nothing.
    let other = KeyModifiers::ALT | KeyModifiers::SUPER | KeyModifiers::HYPER | KeyModifiers::META;
    if mods.intersects(other) {
        return None;
    }
    let (base, shift) = canonical_key(code, mods);
    if mods.contains(KeyModifiers::CONTROL) {
        // Ctrl+C / Ctrl+Q quit (raw mode turns them into plain key events,
        // with no SIGINT); the rotation arrows and letters rotate in the
        // model's own frame. Every other Ctrl chord stays unbound.
        let action = match base {
            Char('q') | Char('c') => Action::Quit,
            Left | Char('h') => Action::Motion(Motion::LocalYawLeft),
            Right | Char('l') => Action::Motion(Motion::LocalYawRight),
            Up | Char('k') => Action::Motion(Motion::LocalPitchUp),
            Down | Char('j') => Action::Motion(Motion::LocalPitchDown),
            Char('r') => Action::Motion(Motion::LocalRollPlus),
            Char('e') => Action::Motion(Motion::LocalRollMinus),
            _ => return None,
        };
        return Some((base, action));
    }
    let action = match (base, shift) {
        // One-shot keys. Shift is a real chord component, so a shifted key is
        // bound ONLY where it is listed below: `Shift+0` (which a US layout
        // types as `)`) is not `0`, and `Shift+Q` is not `q`. The two
        // exceptions are Space and Esc, which a legacy terminal encodes as the
        // very same bytes with or without Shift — strictness there could only
        // make the two terminal paths disagree.
        (Char('q'), false) | (Esc, _) => Action::Quit,
        (Char(' '), _) => Action::Spin,
        // Shift+Tab arrives as BackTab on both paths and IS listed.
        (Tab, false) | (BackTab, _) => Action::Axes,
        (Char('0'), false) => Action::Reset,
        // Help is `?`, i.e. the listed chord shift + `/`.
        (Char('/'), true) => Action::Help,
        (Char('f'), true) => Action::Fit,
        (Char('f'), false) => Action::Center,
        // Unshifted arrows / hjkl rotate; r / e roll.
        (Left, false) | (Char('h'), false) => Action::Motion(Motion::YawLeft),
        (Right, false) | (Char('l'), false) => Action::Motion(Motion::YawRight),
        (Up, false) | (Char('k'), false) => Action::Motion(Motion::PitchUp),
        (Down, false) | (Char('j'), false) => Action::Motion(Motion::PitchDown),
        (Char('r'), false) => Action::Motion(Motion::RollPlus),
        (Char('e'), false) => Action::Motion(Motion::RollMinus),
        // The listed shifted chords translate instead.
        (Left, true) | (Char('h'), true) => Action::Motion(Motion::MoveLeft),
        (Right, true) | (Char('l'), true) => Action::Motion(Motion::MoveRight),
        (Up, true) | (Char('k'), true) => Action::Motion(Motion::MoveUp),
        (Down, true) | (Char('j'), true) => Action::Motion(Motion::MoveDown),
        // Dolly: `=` / `-` plus their shifted spellings `+` / `_`, which is
        // how those keys are labelled on several layouts.
        (Char('='), _) => Action::Motion(Motion::MoveForward),
        (Char('-'), _) => Action::Motion(Motion::MoveBack),
        _ => return None,
    };
    Some((base, action))
}

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
        // View-frame roll: `r` rolls about the sight line (into the
        // screen) in the viewer's sense — the viewer's right side (screen
        // right) dips, i.e. the image turns clockwise; `e` is the mirror.
        // That axis is anti-parallel to the model's front (+Z out of the
        // screen), so plain r and Ctrl+r read as mirror images at the
        // default view: Ctrl+r keeps the body reading (starboard dips).
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

/// HUD layout: Row 0 is the fixed model + view line. `label` overrides the
/// model name (the FIFO path for a stream preview, whose model name is only
/// the file stem); regular files pass `None` and show their own name.
fn hud_layout(name: &str, view: &ViewState, collapsed: bool, label: Option<&str>) -> String {
    let title = label.unwrap_or(name);
    let mut row0 = format!(
        "Wireforge: {} | yaw={:.2} pitch={:.2} roll={:.2} dist={:.2} pan=({:.2},{:.2})",
        title, view.yaw, view.pitch, view.roll, view.dist, view.pan_x, view.pan_y
    );
    if collapsed {
        row0.push_str("   [?] keys");
    }
    row0
}

/// The full help overlay (shown when the HUD is expanded).
const HELP: &[&str] = &[
    "=== wireforge keys ===",
    "",
    "No Shift (world axes; left/right as you see them):",
    "  yaw left  <- / h       yaw right  -> / l",
    "  pitch up  ^ / k        pitch down v / j",
    "  roll      r / e        farther    -",
    "  nearer    =",
    "",
    "Shift:",
    "  left      <- / h       right      -> / l",
    "  up        ^ / k        down       v / j",
    "",
    "Ctrl (the model's own axes / its own left-right):",
    "  yaw left  Ctrl+h       yaw right  Ctrl+l",
    "  pitch up  Ctrl+k       pitch down Ctrl+j",
    "  roll      Ctrl+r / e",
    "  Ctrl + arrows work like Ctrl + hjkl",
    "",
    "Keys:",
    "  center    f            fit        Shift+f",
    "  reset     0            spin       Space",
    "  axes      Tab          help       ?",
    "  quit      q / Esc / Ctrl+C",
    "",
    "[?] close help",
];

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
    name: String,
    /// Row 0 label override: the FIFO path for a stream preview; `None`
    /// shows the model name.
    row0_label: Option<String>,
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
    /// True when the screen must be repainted before the loop blocks again.
    dirty: bool,
}

impl App {
    fn new(current: Model, name: String, row0_label: Option<String>) -> Self {
        App {
            current,
            name,
            row0_label,
            view: ViewState::default(),
            held: HashMap::new(),
            release_seen: false,
            auto_spin: false,
            hud: Hud::Collapsed,
            show_axes: true,
            dirty: true,
        }
    }

    /// Translation speed scales with the model's geometric-mean extent.
    fn move_scale(&self) -> f64 {
        view::model_extent(&self.current)
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
            if !self.held.is_empty() {
                self.held.clear();
                self.dirty = true;
            }
            return false;
        }
        let Event::Key(key) = ev else {
            return false;
        };
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
            // Fit: the target at an appropriate distance (dist only — angles
            // and pan are kept). Center: the file origin on screen (pan only).
            Action::Fit => {
                self.view.fit_to(&self.current);
                self.dirty = true;
            }
            Action::Center => {
                self.view.center_origin();
                self.dirty = true;
            }
            Action::Reset => {
                self.view.reset(&self.current);
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
}

impl Engine {
    fn new(w: usize, h: usize) -> Self {
        Engine {
            screen: render::Screen::new(w, h),
            raster: render::Rasterizer::new(),
            hud_buf: None,
        }
    }

    /// Terminal resize: reallocate the screen and drop the HUD buffer (it was sized for the old screen).
    fn resize(&mut self, w: usize, h: usize) {
        self.screen.resize(w, h);
        self.hud_buf = None;
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

/// Copy rows `[y0, y1)` of the HUD buffer into the screen (chars + fg).
fn blit_hud_rows(screen: &mut render::Screen, hud: &Buffer, y0: u16, y1: u16) {
    let (w, _) = screen.size();
    for y in y0..y1 {
        for x in 0..w {
            let cell = &hud[(x as u16, y)];
            let ch = cell.symbol().chars().next().unwrap_or(' ');
            screen.set(x, y as usize, ch, render::color_idx(cell.fg));
        }
    }
}

/// Render the current state into the terminal.
fn render_frame(
    app: &mut App,
    engine: &mut Engine,
    stdout: &mut io::Stdout,
) -> Result<(), Box<dyn Error>> {
    let (w, h) = engine.screen.size();
    if w == 0 || h == 0 {
        return Ok(());
    }
    let w16 = w as u16;
    let h16 = h as u16;
    let row0 = hud_layout(
        &app.name,
        &app.view,
        app.hud == Hud::Collapsed,
        app.row0_label.as_deref(),
    );
    // Row 0 is the fixed model+view line; the canvas starts right below it.
    let canvas_top: u16 = 1;
    let overlay = if app.hud == Hud::Expanded {
        Some(HELP.iter().map(|s| s.to_string()).collect::<Vec<_>>())
    } else {
        None
    };

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

    // Row 0 (always present above the canvas).
    {
        let hud = engine.hud_buf.as_mut().unwrap();
        Paragraph::new(row0).render(Rect::new(0, 0, w16, 1), hud);
    }

    // The overlay (help panel) or the model canvas fills everything below
    // the HUD row.
    let canvas_area = if let Some(lines) = overlay {
        let top = canvas_top;
        let height = h16.saturating_sub(top);
        let area = Rect::new(0, top, w16, height);
        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .padding(Padding::new(2, 2, 1, 1));
        let inner = block.inner(area);
        let hud = engine.hud_buf.as_mut().unwrap();
        block.render(area, hud);
        for (i, line) in lines.iter().enumerate() {
            if i as u16 >= inner.height {
                break;
            }
            Paragraph::new(line.as_str())
                .render(Rect::new(inner.x, inner.y + i as u16, inner.width, 1), hud);
        }
        None
    } else {
        Some(Rect::new(
            0,
            canvas_top,
            w16,
            h16.saturating_sub(canvas_top),
        ))
    };

    // Copy the HUD rows into the screen.
    {
        let screen = &mut engine.screen;
        let hud = engine.hud_buf.as_ref().unwrap();
        blit_hud_rows(screen, hud, 0, canvas_top);
    }

    if let Some(area) = canvas_area {
        // Model canvas: rasterize into the screen directly.
        let cw = area.width as usize;
        let ch = area.height as usize;
        if cw > 0 && ch > 0 {
            engine.raster.resize(cw, ch);
            engine.raster.render(
                &app.current,
                &app.view,
                (0, canvas_top as usize, cw, ch),
                app.show_axes,
                &mut engine.screen,
            );
        }
    } else {
        // Overlay region: copy the panel/help text from the buffer.
        let screen = &mut engine.screen;
        let hud = engine.hud_buf.as_ref().unwrap();
        blit_hud_rows(screen, hud, canvas_top, h16);
    }

    // Present: packed-cell diff + one batched write.
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
    // No FILE: read stdin when it is not a terminal (`cat m.wrfm | wireforge`).
    // On a terminal the argument is genuinely missing, so fail the way clap
    // would — same message and exit code 2 — instead of blocking on stdin EOF.
    let target_file = match args.file {
        Some(file) => file,
        None if !io::stdin().is_terminal() => PathBuf::from("-"),
        None => Args::command()
            .error(
                clap::error::ErrorKind::MissingRequiredArgument,
                "the following required arguments were not provided:\n  <FILE>\n\n  \
                 provide a .wrfm file path, or `-` (or a pipe) to read the model from stdin",
            )
            .exit(),
    };

    // Regular files are probed through PROBE_BYTES and loaded from the path;
    // `-`/FIFO read the whole stream once, at start-up.
    let is_stdin = target_file == Path::new("-");
    let is_fifo = !is_stdin && is_fifo_path(&target_file);
    let is_stream = is_stdin || is_fifo;
    // Row 0 label override: the FIFO's full path (its model name is only the
    // file stem). stdin and regular files show their model name.
    let row0_label: Option<String> = if is_fifo {
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

    let (current_model, model_name) = if is_stream {
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
    let mut app = App::new(current_model, model_name, row0_label);
    app.view.fit_to(&app.current);
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

    // --- HUD layout (row 0) ---

    #[test]
    fn hud_row0_carries_the_name_view_and_optional_label() {
        // Row 0 is always the fixed model + view line. A stream preview may
        // override the title (the FIFO path, whose model name is only the
        // file stem); otherwise the model name shows, and the collapsed HUD
        // appends the help hint.
        let view = ViewState::default();
        let row0 = hud_layout("cube", &view, true, None);
        assert!(
            row0.starts_with("Wireforge: cube | yaw="),
            "fixed row must carry the model name and view: {row0}"
        );
        assert!(
            row0.contains("dist=") && row0.contains("pan=("),
            "row0: {row0}"
        );
        assert!(row0.ends_with("[?] keys"), "collapsed hint: {row0}");

        let labelled = hud_layout("stream", &view, false, Some("/tmp/stream.fifo"));
        assert!(
            labelled.starts_with("Wireforge: /tmp/stream.fifo |"),
            "row0: {labelled}"
        );
        assert!(
            !labelled.contains("[?] keys"),
            "the expanded HUD hides the hint: {labelled}"
        );
    }

    // ---------- stream input (stdin / FIFO) ----------

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
    fn load_model_from_text_garbage_fails() {
        let err = load_model_from_text("stream", "garbage here\nno markers\n")
            .expect_err("garbage stream must fail");
        assert!(err.contains("unrecognized"), "error: {err}");
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
    fn load_model_from_text_obj_errors_with_a_convert_hint() {
        let text = "v 0 0 0\nv 1 0 0\nv 0 1 0\nf 1 2 3\n";
        let err = load_model_from_text("cube", text).expect_err("obj stream must not load");
        assert!(err.contains("wrfm convert"), "error: {err}");
    } // ---------- event loop (input / holds) ----------

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
    fn app_handle_input_motion_press_starts_continuous() {
        let mut app = App::new(
            Model {
                vertices: vec![(0.0, 0.0, 0.0), (1.0, 0.0, 0.0)],
                edges: vec![(0, 1)],
            },
            "cube".to_string(),
            None,
        );
        app.view.fit_to(&app.current);
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

    /// The same physical input can arrive in two shapes: the **base** key plus
    /// SHIFT (a path without alternate keys — still accepted) or the shifted
    /// **glyph** (what a legacy terminal sends, SHIFT synthesised for
    /// upper-case letters, and what the kitty path now sends for keys with an
    /// alternate, SHIFT cleared). Both must resolve alike — to the same action
    /// under the same identity when the chord is listed, or to nothing at all
    /// when it is not (`Shift+0` is `)`, and `)` is not `0`).
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
                (Char('f'), KeyModifiers::SHIFT),
                (Char('F'), KeyModifiers::SHIFT),
                Some(Action::Fit),
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
            (Char('F'), Char('f'), Action::Fit),
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
        // `Shift+0` types `)` and must not reset, `Shift+q` must not quit.
        assert_eq!(resolve_key_event(Char(')'), KeyModifiers::NONE), None);
        assert_eq!(resolve_key_event(Char('Q'), KeyModifiers::NONE), None);
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
            (Char('0'), Action::Reset),
            (Char('f'), Action::Center),
            (Char('r'), Action::Motion(Motion::RollPlus)),
            (Char('e'), Action::Motion(Motion::RollMinus)),
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
            "plain r rolls about the sight line: the viewer's right dips (image CW)"
        );

        let mut v = ViewState::default();
        apply_motion_step(&mut v, Motion::LocalRollPlus, 0.3, 0.0);
        // Starboard is the model's local -X; a right bank dips it (y < 0).
        assert!(-v.rot[1][0] < 0.0, "Ctrl+r must dip the model's starboard");

        let mut v = ViewState::default();
        apply_motion_step(&mut v, Motion::PitchUp, 0.3, 0.0);
        assert!(v.rot[1][2] > 0.0, "k must tip the nose up in both frames");
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
}
