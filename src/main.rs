use clap::Parser;
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
    Terminal,
    backend::CrosstermBackend,
    buffer::Buffer,
    layout::Rect,
    widgets::{Block, Borders, Paragraph, Widget},
};

#[cfg(feature = "ratty")]
use ratatui::style::Style;

use ratatui_wireframe::model::Model;
use std::{
    collections::HashMap,
    error::Error,
    io::{self, Read},
    path::{Path, PathBuf},
    sync::mpsc::{self, Sender},
    thread,
    time::{Duration, Instant},
};
use wrfm::WrfmModel;

mod reload;
mod render;
mod timer;
mod view;
use reload::{ReloadEvent, ReloadWatch};
use timer::{TimerId, TimerScheduler};
use view::ViewState;

#[cfg(feature = "ratty")]
use ratatui_ratty::{ObjectFormat, RattyGraphic, RattyGraphicSettings};

// Speed constants are dyadic fractions (exact in binary) with ~1.3x
// translation / ~1.1x rotation speedup over the upstream values.

/// Smooth continuous rotation rate (radians per second) while a key is held.
const ROT_RATE: f64 = 169.0 / 128.0;
/// Smooth continuous translation rate: fraction of the model extent moved per second.
const MOVE_RATE: f64 = 83.0 / 128.0;

/// How long the transient reload status line (row 1) stays on screen
/// before it is cleared and those rows go back to the model canvas.
const STATUS_TIMEOUT: Duration = Duration::from_secs(5);
/// Auto-spin yaw rate (radians per second): 169/256 = 0.66015625, a dyadic (exact-in-binary) constant.
const SPIN_RATE: f64 = 169.0 / 256.0;
/// How often the idle loop re-checks the terminal size (resize fallback).
const RESIZE_CHECK_INTERVAL: Duration = Duration::from_millis(500);
/// How long to let a file write settle before re-parsing after a change.
const SETTLE_DELAY: Duration = Duration::from_millis(50);
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
    about = "TUI editor and viewer for .wrfm and .obj 3D models"
)]
struct Args {
    #[arg(required = true)]
    file: PathBuf,
}

/// State enum to track which rendering engine we are using.
enum RenderMode {
    Braille(Model),
    #[cfg(feature = "ratty")]
    Hardware3D(RattyGraphic<'static>),
}

impl RenderMode {
    fn braille(&self) -> Option<&Model> {
        match self {
            RenderMode::Braille(m) => Some(m),
            #[cfg(feature = "ratty")]
            RenderMode::Hardware3D(_) => None,
        }
    }
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
    // Rotation (world-frame: fixed world axes).
    YawLeft,
    YawRight,
    PitchUp,
    PitchDown,
    RollPlus,
    RollMinus,
    // Translation (absolute world coordinates).
    MoveLeft,
    MoveRight,
    MoveUp,
    MoveDown,
    MoveForward,
    MoveBack,
}

/// Map a key (with its modifiers) to a motion. Returns `None` for keys bound to no motion (quit, toggles, unknown).
fn motion_for(code: KeyCode, shift: bool) -> Option<Motion> {
    use KeyCode::*;
    Some(match code {
        // Translation: Shift + arrows / hjkl (right/up), = / - (forward).
        Left if shift => Motion::MoveLeft,
        Right if shift => Motion::MoveRight,
        Up if shift => Motion::MoveUp,
        Down if shift => Motion::MoveDown,
        Char('h') | Char('H') if shift => Motion::MoveLeft,
        Char('l') | Char('L') if shift => Motion::MoveRight,
        Char('k') | Char('K') if shift => Motion::MoveUp,
        Char('j') | Char('J') if shift => Motion::MoveDown,
        Char('=') | Char('+') => Motion::MoveForward,
        Char('-') => Motion::MoveBack,
        // Rotation: arrows / hjkl (yaw, pitch), r / e (roll).
        Left | Char('h') => Motion::YawLeft,
        Right | Char('l') => Motion::YawRight,
        Up | Char('k') => Motion::PitchUp,
        Down | Char('j') => Motion::PitchDown,
        Char('r') => Motion::RollPlus,
        Char('e') => Motion::RollMinus,
        _ => return None,
    })
}

/// One frame of smooth continuous motion for a held key (model follows key).
fn continuous_step(view: &mut ViewState, m: Motion, scale: f64, dt: f64) {
    apply_motion_step(view, m, ROT_RATE * dt, scale * MOVE_RATE * dt);
}

/// Apply one motion step: rotation around the world axes, pan in world X/Y, dolly along the view axis.
fn apply_motion_step(view: &mut ViewState, m: Motion, rot: f64, mv: f64) {
    match m {
        // World-frame rotation: yaw/pitch pre-multiply the model->world
        // matrix with a fixed world-axis rotation, so the axes are anchored
        // to the world and never follow the model (no gimbal collapse).
        Motion::YawLeft => view.add_yaw(rot),
        Motion::YawRight => view.add_yaw(-rot),
        Motion::PitchUp => view.add_pitch(-rot),
        Motion::PitchDown => view.add_pitch(rot),
        Motion::RollPlus => view.roll -= rot,
        Motion::RollMinus => view.roll += rot,
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

/// A file format detected by probing the file's first bytes (mpv-style sniffing).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FileFormat {
    /// A v1 `.wrfm` file: line 1 is the `wrfm <version>` magic.
    Wrfm,
    /// A Wavefront `.obj` file: `v`/`f`/`vt`/`vn` lines (rendered by ratty).
    Obj,
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
    use std::io::IsTerminal;
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

/// Probe a byte buffer for wrfm / obj markers.
fn probe_bytes(buf: &[u8]) -> Result<FileFormat, String> {
    // Lossy: a stray non-UTF-8 byte must never decide the format.
    let head = String::from_utf8_lossy(buf);
    // Strip a leading BOM like the wrfm parser does, so a BOM before the
    // magic line does not hide it.
    let head = head.strip_prefix('\u{feff}').unwrap_or(&head);

    // Magic first: the first line of a v1 wrfm file is `wrfm <version>`.
    // The magic is a short line, so it always fits within PROBE_BYTES.
    let first_line = head.lines().next().unwrap_or("");
    if first_line.split_whitespace().next() == Some("wrfm") {
        return Ok(FileFormat::Wrfm);
    }

    // Otherwise scan for obj markers: a magic-less `v`/`e` file is
    // unrecognized; only `v`-without-`e` or real obj markers (`f`/`vt`/`vn`)
    // route to obj.
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

    if has_obj_marker || (has_vertex && !has_wrfm_edge) {
        Ok(FileFormat::Obj)
    } else {
        Err(
            "unrecognized file format: no wrfm magic line (`wrfm <version>`) or obj (`v`/`f`/`vt`/`vn`) markers"
 .to_string(),
 )
    }
}

/// Probe the file's content for wrfm / obj markers.
fn probe_format(target_file: &Path) -> Result<FileFormat, String> {
    let mut buf = [0u8; PROBE_BYTES];
    let mut file = std::fs::File::open(target_file)
        .map_err(|e| format!("cannot open '{}': {e}", target_file.display()))?;
    let n = file
        .read(&mut buf)
        .map_err(|e| format!("cannot read '{}': {e}", target_file.display()))?;
    probe_bytes(&buf[..n]).map_err(|e| format!("'{}': {e}", target_file.display()))
}

/// Route a file to the correct parser or widget.
fn load_model(target_file: &Path) -> Result<(RenderMode, String), String> {
    match probe_format(target_file)? {
        FileFormat::Wrfm => {
            let wrfm_data = WrfmModel::from_file(target_file).map_err(|e| e.to_string())?;
            let model = Model {
                vertices: wrfm_data.vertices,
                edges: wrfm_data.edges,
            };
            Ok((RenderMode::Braille(model), wrfm_data.name))
        }
        FileFormat::Obj => {
            #[cfg(feature = "ratty")]
            {
                let name = target_file
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("OBJ Model")
                    .to_string();
                let abs_path = std::fs::canonicalize(target_file)
                    .unwrap_or_else(|_| target_file.to_path_buf());
                let path_str = abs_path.to_string_lossy().into_owned();
                let settings = RattyGraphicSettings::new(path_str)
                    .id(1)
                    .format(ObjectFormat::Obj)
                    .scale(0.30)
                    .brightness(1.5);
                let graphic = RattyGraphic::new(settings);
                if let Err(e) = graphic.register() {
                    return Err(format!("Ratty Registration Error: {e}"));
                }
                Ok((RenderMode::Hardware3D(graphic), name))
            }
            #[cfg(not(feature = "ratty"))]
            {
                Err(
                    "OBJ parsing requires the 'ratty' feature. Recompile with --features ratty"
                        .to_string(),
                )
            }
        }
    }
}

/// Route a stream buffer (stdin / FIFO) to the correct parser or widget.
fn load_model_from_text(name: &str, text: &str) -> Result<(RenderMode, String), String> {
    match probe_bytes(text.as_bytes())? {
        FileFormat::Wrfm => {
            let wrfm_data = WrfmModel::from_str(name, text).map_err(|e| e.to_string())?;
            let model = Model {
                vertices: wrfm_data.vertices,
                edges: wrfm_data.edges,
            };
            Ok((RenderMode::Braille(model), wrfm_data.name))
        }
        FileFormat::Obj => {
            #[cfg(feature = "ratty")]
            {
                let settings = RattyGraphicSettings::new(format!("{name}.obj"))
                    .id(1)
                    .format(ObjectFormat::Obj)
                    .scale(0.30)
                    .brightness(1.5);
                let graphic = RattyGraphic::new(settings);
                if let Err(e) = graphic.register_payload(text.as_bytes()) {
                    return Err(format!("Ratty Registration Error: {e}"));
                }
                Ok((RenderMode::Hardware3D(graphic), name.to_string()))
            }
            #[cfg(not(feature = "ratty"))]
            {
                Err(
                    "OBJ parsing requires the 'ratty' feature. Recompile with --features ratty"
                        .to_string(),
                )
            }
        }
    }
}

/// Apply a successful reload: swap in the new render and name; the camera pose stays as the user left it.
fn apply_reload(
    current: &mut RenderMode,
    name: &mut String,
    new_render: RenderMode,
    new_name: String,
) {
    *current = new_render;
    *name = new_name;
}

#[cfg_attr(not(test), allow(dead_code))] // exercised by the hot-reload unit tests
/// Apply one hot-reload poll result to the running viewer.
fn handle_reload_event(
    event: ReloadEvent,
    current: &mut RenderMode,
    name: &mut String,
    target_file: &Path,
) -> Option<ReloadRecord> {
    match event {
        ReloadEvent::Changed => {
            // Let a half-written file settle before re-parsing it, so a
            // mid-write read is less likely; a failed parse still keeps the
            // last good model and retries on the next poll.
            std::thread::sleep(SETTLE_DELAY);
            Some(match load_model(target_file) {
                Ok((new_render, new_name)) => {
                    apply_reload(current, name, new_render, new_name);
                    ReloadRecord {
                        outcome: ReloadOutcome::Ok,
                        detail: "model reloaded".to_string(),
                    }
                }
                Err(e) => ReloadRecord {
                    outcome: ReloadOutcome::ParseError,
                    detail: format!("Parse Error: {e}"),
                },
            })
        }
        ReloadEvent::Missing => Some(ReloadRecord {
            outcome: ReloadOutcome::FileRemoved,
            detail: "file removed (deleted or renamed away); keeping last model".to_string(),
        }),
        ReloadEvent::Unchanged => None,
    }
}

/// The outcome class of a hot-reload action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReloadOutcome {
    /// The file changed and re-parsed successfully; the model was replaced.
    Ok,
    /// The file changed but failed to parse; the last good model was kept.
    ParseError,
    /// The file disappeared (deleted or renamed away); the last model kept.
    FileRemoved,
}

/// The most recent hot-reload action: an outcome plus its detail, the input to `status_line`.
#[derive(Debug, Clone, PartialEq)]
struct ReloadRecord {
    outcome: ReloadOutcome,
    /// The action's detail: the full parse-error report for `ParseError`, a one-line note for the other outcomes.
    detail: String,
}

/// The transient row-1 status label for a reload outcome — one of the three fixed labels.
fn hud_status_for(outcome: ReloadOutcome) -> &'static str {
    match outcome {
        ReloadOutcome::Ok => "hot-reloaded",
        ReloadOutcome::ParseError => "parse error",
        ReloadOutcome::FileRemoved => "file removed",
    }
}

#[cfg_attr(not(test), allow(dead_code))] // exercised by the status-expiry unit tests
/// True when the armed status-line deadline has passed; `None` (nothing showing) never expires.
fn status_expired(deadline: Option<Instant>, now: Instant) -> bool {
    deadline.is_some_and(|d| now >= d)
}

/// The transient status line under Row 0: the outcome label plus a
/// COMPACT detail. For a parse error only the report's first line survives
/// (`<kind> at line L, column C`): the line must stay ONE row tall, so the
/// rustc-style body (source line + caret) is not shown.
fn status_line(record: &ReloadRecord) -> String {
    let label = hud_status_for(record.outcome);
    match record.outcome {
        ReloadOutcome::Ok => label.to_string(),
        ReloadOutcome::FileRemoved => format!("{label}; keeping last model"),
        ReloadOutcome::ParseError => {
            // detail = "Parse Error: error: <kind> at line L, column C\n..."
            let Some(first) = record.detail.lines().next() else {
                return label.to_string();
            };
            let head = first.strip_prefix("Parse Error: ").unwrap_or(first);
            let head = head.strip_prefix("error: ").unwrap_or(head);
            if head.is_empty() {
                label.to_string()
            } else {
                format!("{label}: {head}")
            }
        }
    }
}

/// HUD layout: Row 0 is the fixed model + view line; below it the transient event region.
fn hud_layout(
    name: &str,
    view: &ViewState,
    status: &str,
    height: u16,
    collapsed: bool,
    one_shot: Option<&str>,
) -> (String, Vec<String>, u16) {
    // In one-shot (stdin / FIFO) mode Row 0 advertises that the preview will
    // NOT auto-refresh, so Row 0 warns up front and the user is never surprised.
    let label = one_shot.unwrap_or(name);
    let mut row0 = format!(
        "Wireforge: {} | yaw={:.2} pitch={:.2} roll={:.2} dist={:.2} pan=({:.2},{:.2})",
        label, view.yaw, view.pitch, view.roll, view.dist, view.pan_x, view.pan_y
    );
    if collapsed {
        row0.push_str("   [?] keys");
    }
    let event_lines: Vec<String> = if status.is_empty() {
        Vec::new()
    } else {
        status.lines().map(str::to_string).collect()
    };
    // Reserve only as many rows as fit below the fixed row 0; a long Parse
    // Error must never be squeezed into Row 0.
    let reserved = event_lines.len().min(height.saturating_sub(1) as usize) as u16;
    (row0, event_lines, reserved)
}

/// The full help overlay (shown when the HUD is expanded).
const HELP: &[&str] = &[
    "=== wireforge keys ===",
    "",
    "Rotate:",
    "  yaw left  <- / h       yaw right  -> / l",
    "  pitch up  ^ / k        pitch down v / j",
    "  roll      r / e",
    "",
    "Move:",
    "  left      Shift+<- / h  right     Shift+-> / l",
    "  up        Shift+^ / k   down      Shift+v / j",
    "  nearer    =             farther   -",
    "",
    "Keys:",
    "  center   f          fit       Shift+f",
    "  reset    0          spin      Space",
    "  axes     Tab        quit      q / Esc",
    "",
    "[?] close help",
];

// Game-engine event loop: an input thread + channel, one scheduler owned
// by the loop, a two-mode main loop, and dirty-flag rendering.

/// Events the main loop blocks on: terminal input, reload events and timer timeouts.
pub(crate) enum LoopEvent {
    Input(Event),
    Reload(ReloadEvent),
}

/// All viewer state owned by the main loop.
struct App {
    current: RenderMode,
    name: String,
    one_shot: Option<String>,
    target_file: PathBuf,
    view: ViewState,
    /// Motion keys that are currently down (value = last Press/Repeat seen).
    held: HashMap<Motion, Instant>,
    /// True once the terminal delivered a Release. From then on key-up is
    /// authoritative, so holds end on Release / FocusLost and are never
    /// dropped for staying quiet (see `update_held`).
    release_seen: bool,
    auto_spin: bool,
    hud: Hud,
    show_axes: bool,
    status_msg: String,
    /// True when the screen must be repainted before the loop blocks again.
    dirty: bool,
}

impl App {
    fn new(
        current: RenderMode,
        name: String,
        one_shot: Option<String>,
        target_file: PathBuf,
    ) -> Self {
        App {
            current,
            name,
            one_shot,
            target_file,
            view: ViewState::default(),
            held: HashMap::new(),
            release_seen: false,
            auto_spin: false,
            hud: Hud::Collapsed,
            show_axes: true,
            status_msg: String::new(),
            dirty: true,
        }
    }

    /// Translation speed scales with the model's geometric-mean extent.
    fn move_scale(&self) -> f64 {
        self.current
            .braille()
            .map(view::model_extent)
            .unwrap_or(1.0)
    }

    /// Auto-fit the camera to the braille model (Shift+f).
    fn fit_if_braille(&mut self) {
        if let Some(m) = self.current.braille() {
            self.view.fit_to(m);
        }
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
        for seen in self.held.values_mut() {
            *seen = now;
        }
    }

    /// Mark a motion key as held (Press/Repeat). `update_held` applies the
    /// continuous motion every frame until Release or the hold timeout.
    fn hold(&mut self, m: Motion) {
        self.held.insert(m, Instant::now());
        self.dirty = true;
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
        if let Event::Key(key) = ev {
            // A Release is direct evidence that the terminal reports key-up:
            // from here on a hold ends when the key does, never on a timeout.
            if key.kind == KeyEventKind::Release {
                self.release_seen = true;
            } else {
                self.note_key_event(Instant::now());
            }
            let shift = key.modifiers.contains(KeyModifiers::SHIFT);
            // The kitty protocol reports shifted punctuation as the base key
            // plus the SHIFT modifier (REPORT_ALTERNATE_KEYS is not enabled),
            // i.e. Shift+'/' arrives as Char('/') rather than Char('?').
            // Normalise once so every branch below sees the shifted character.
            let code = match key.code {
                KeyCode::Char('/') if shift => KeyCode::Char('?'),
                code => code,
            };
            // Kitty keyboard protocol: a Release event stops the motion immediately.
            if key.kind == KeyEventKind::Release {
                if let Some(m) = motion_for(code, shift) {
                    self.held.remove(&m);
                    self.dirty = true;
                }
                return false;
            }
            match code {
                KeyCode::Char('q') | KeyCode::Esc => return true,
                // Auto-repeat only refreshes the hold: the toggles below fire
                // once per tap, never at the terminal's repeat rate.
                _ if key.kind == KeyEventKind::Repeat => {
                    if let Some(m) = motion_for(code, shift) {
                        self.hold(m);
                    }
                }
                // One-shot keys act immediately (no debounce).
                KeyCode::Char('?') => {
                    self.hud = match self.hud {
                        Hud::Collapsed => Hud::Expanded,
                        Hud::Expanded => Hud::Collapsed,
                    };
                    self.dirty = true;
                }
                KeyCode::Char(' ') => {
                    self.auto_spin = !self.auto_spin;
                    self.dirty = true;
                }
                KeyCode::Tab => {
                    self.show_axes = !self.show_axes;
                    self.dirty = true;
                }
                // Shift+f: fit the target to an appropriate distance
                // (dist only — angles and pan are kept). f: centre the
                // file origin on screen (pan only).
                KeyCode::Char('f') | KeyCode::Char('F') if shift => {
                    self.fit_if_braille();
                    self.dirty = true;
                }
                KeyCode::Char('f') | KeyCode::Char('F') => {
                    self.view.center_origin();
                    self.dirty = true;
                }
                KeyCode::Char('0') => {
                    if let Some(m) = self.current.braille() {
                        self.view.reset(m);
                    }
                    self.dirty = true;
                }
                // Motion keys.
                _ => {
                    if let Some(m) = motion_for(code, shift) {
                        self.hold(m);
                    }
                }
            }
        }
        false
    }

    /// Apply continuous motion to every held key each frame (dt-scaled), then
    /// drop the keys that stopped reporting. Release (kitty protocol) and
    /// FocusLost are the normal stops; the timeout is the only stop on a
    /// terminal that never sends a Release, and a backstop when one is lost.
    fn update_held(&mut self, now: Instant, dt: f64) {
        // With key-up reporting there is nothing to time out: Release ends the
        // hold and FocusLost covers an alt-tab, so a timeout would only ever
        // kill a key that is still down. Without it, silence is the only
        // evidence available and one second of it ends the hold.
        if !self.release_seen {
            self.held
                .retain(|_, seen| now.saturating_duration_since(*seen) <= LEGACY_HOLD_TIMEOUT);
        }
        if self.held.is_empty() {
            return;
        }
        let move_scale = self.move_scale();
        for m in self.held.keys() {
            continuous_step(&mut self.view, *m, move_scale, dt);
        }
    }

    /// Handle a timer that fired. Returns true when the screen changed.
    fn handle_timer(&mut self, id: TimerId, _now: Instant) -> bool {
        match id {
            TimerId::StatusExpiry => {
                self.status_msg.clear();
                self.dirty = true;
                true
            }
            TimerId::ReloadPoll | TimerId::ReloadParse | TimerId::ResizeCheck => false,
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
fn spawn_input_thread(tx: Sender<LoopEvent>) {
    let _ = thread::Builder::new()
        .name("wireforge-input".into())
        .spawn(move || {
            while let Ok(ev) = event::read() {
                if tx.send(LoopEvent::Input(ev)).is_err() {
                    break;
                }
            }
        });
}

/// Parse the target file and apply it on success, keeping the last good model on failure; called from the ReloadParse timer so the event loop never blocks on parsing.
fn handle_reload_changed(
    current: &mut RenderMode,
    name: &mut String,
    target_file: &Path,
) -> ReloadRecord {
    match load_model(target_file) {
        Ok((new_render, new_name)) => {
            apply_reload(current, name, new_render, new_name);
            ReloadRecord {
                outcome: ReloadOutcome::Ok,
                detail: "model reloaded".to_string(),
            }
        }
        Err(e) => ReloadRecord {
            outcome: ReloadOutcome::ParseError,
            detail: format!("Parse Error: {e}"),
        },
    }
}

/// Show a reload outcome on the row-1 status line (cleared after
/// STATUS_TIMEOUT): the ONE compact line, never the full report.
fn show_reload_status(app: &mut App, timers: &mut TimerScheduler, record: ReloadRecord) {
    let now = Instant::now();
    app.status_msg = status_line(&record);
    timers.schedule(TimerId::StatusExpiry, now + STATUS_TIMEOUT);
    app.dirty = true;
}

/// Handle a reload event from the inotify thread or the poll watch. A change is deferred by `SETTLE_DELAY` so a half-written file settles; a missing file reports on the status line at once.
fn handle_reload_event_loop(
    app: &mut App,
    timers: &mut TimerScheduler,
    event: ReloadEvent,
) -> bool {
    match event {
        ReloadEvent::Changed => {
            // Settle half-written files; the loop schedules ReloadParse
            // (non-blocking) and keeps the last good model on failure.
            timers.schedule(TimerId::ReloadParse, Instant::now() + SETTLE_DELAY);
            false
        }
        ReloadEvent::Missing => {
            show_reload_status(
                app,
                timers,
                ReloadRecord {
                    outcome: ReloadOutcome::FileRemoved,
                    detail: "file removed (deleted or renamed away); keeping last model"
                        .to_string(),
                },
            );
            true
        }
        ReloadEvent::Unchanged => false,
    }
}

/// The ReloadParse timer fired: parse the file and show the outcome.
fn apply_reload_parse(app: &mut App, timers: &mut TimerScheduler) {
    let record = handle_reload_changed(&mut app.current, &mut app.name, &app.target_file);
    show_reload_status(app, timers, record);
}

/// The ReloadPoll fallback timer fired: stat the file, handle the event and re-arm the poll timer.
fn poll_reload(watch: &mut Option<ReloadWatch>, app: &mut App, timers: &mut TimerScheduler) {
    if let Some(w) = watch.as_mut() {
        let event = w.poll();
        handle_reload_event_loop(app, timers, event);
        timers.schedule(TimerId::ReloadPoll, Instant::now() + reload::POLL_INTERVAL);
    }
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
#[cfg_attr(not(feature = "ratty"), allow(unused_variables))]
fn render_frame(
    app: &mut App,
    engine: &mut Engine,
    stdout: &mut io::Stdout,
    terminal: &mut Option<ratatui::Terminal<CrosstermBackend<io::Stdout>>>,
) -> Result<(), Box<dyn Error>> {
    match &app.current {
        RenderMode::Braille(_) => {
            let (w, h) = engine.screen.size();
            if w == 0 || h == 0 {
                return Ok(());
            }
            let w16 = w as u16;
            let h16 = h as u16;
            let (row0, event_lines, event_rows) = hud_layout(
                &app.name,
                &app.view,
                &app.status_msg,
                h16,
                app.hud == Hud::Collapsed,
                app.one_shot.as_deref(),
            );
            // Row 0 is the fixed model+view line; the transient event region
            // occupies rows 1..1+event_rows; the canvas starts below both.
            let canvas_top = 1 + event_rows;
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

            // Row 0 + transient event rows (always present above the canvas).
            {
                let hud = engine.hud_buf.as_mut().unwrap();
                Paragraph::new(row0).render(Rect::new(0, 0, w16, 1), hud);
                for (i, line) in event_lines.iter().enumerate() {
                    let row = 1 + i as u16;
                    if row >= canvas_top {
                        break;
                    }
                    Paragraph::new(line.as_str()).render(Rect::new(0, row, w16, 1), hud);
                }
            }

            // The overlay (help / reload panel) or the model canvas fills
            // everything below the HUD rows.
            let canvas_area = if let Some(lines) = overlay {
                let top = canvas_top;
                let height = h16.saturating_sub(top);
                let area = Rect::new(0, top, w16, height);
                let block = Block::default().borders(Borders::ALL);
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
                    if let Some(model) = app.current.braille() {
                        engine.raster.render(
                            model,
                            &app.view,
                            (0, canvas_top as usize, cw, ch),
                            app.show_axes,
                            &mut engine.screen,
                        );
                    }
                }
            } else {
                // Overlay region: copy the panel/help text from the buffer.
                let screen = &mut engine.screen;
                let hud = engine.hud_buf.as_ref().unwrap();
                blit_hud_rows(screen, hud, canvas_top, h16);
            }

            // Present: packed-cell diff + one batched write.
            engine.screen.present(stdout)?;
        }
        #[cfg(feature = "ratty")]
        RenderMode::Hardware3D(_) => {
            // Push the accumulated angles to the ratty emulator (best-effort).
            if let RenderMode::Hardware3D(graphic) = &mut app.current {
                graphic.settings_mut().rotation = [
                    app.view.pitch as f32,
                    app.view.yaw as f32,
                    app.view.roll as f32,
                ];
                let _ = graphic.update();
            }
            // The ratty path keeps using ratatui's Terminal untouched
            // (the .obj path is unchanged at every level).
            if let Some(t) = terminal.as_mut() {
                t.draw(|f| {
                    let area = f.area();
                    let title = format!(
                        "Wireforge: {} | yaw={:.2} pitch={:.2} roll={:.2} dist={:.2}",
                        app.name, app.view.yaw, app.view.pitch, app.view.roll, app.view.dist
                    );
                    let block = Block::default().borders(Borders::ALL).title(title);
                    let inner = block.inner(area);
                    let (_, event_lines, event_rows) = hud_layout(
                        &app.name,
                        &app.view,
                        &app.status_msg,
                        inner.height,
                        false,
                        app.one_shot.as_deref(),
                    );
                    for (i, line) in event_lines.iter().enumerate() {
                        if i as u16 >= event_rows {
                            break;
                        }
                        f.render_widget(
                            Paragraph::new(line.as_str()).style(Style::default()),
                            Rect::new(inner.x, inner.y + i as u16, inner.width, 1),
                        );
                    }
                    let graphic_area = Rect::new(
                        inner.x,
                        inner.y + event_rows,
                        inner.width,
                        inner.height.saturating_sub(event_rows),
                    );
                    f.render_widget(block, area);
                    if let RenderMode::Hardware3D(graphic) = &app.current {
                        f.render_widget(graphic, graphic_area);
                    }
                })?;
            }
        }
    }
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
    let target_file = args.file.clone();

    // Regular files probe PROBE_BYTES and keep hot-reload; `-`/FIFO read the
    // whole stream once as one-shot previews (no hot-reload).
    let is_stdin = target_file == Path::new("-");
    let is_fifo = !is_stdin && is_fifo_path(&target_file);
    // Row 0 label for one-shot mode ("stdin preview (no hot-reload)" or the
    // FIFO path); None for regular files (hot-reload stays on).
    let one_shot: Option<String> = if is_stdin {
        Some("stdin preview (no hot-reload)".to_string())
    } else if is_fifo {
        Some(format!("{} (no hot-reload)", target_file.display()))
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

    let (current_render, model_name) = if one_shot.is_some() {
        // Stream path: read all of stdin (or the FIFO) once, probe the
        // whole BUFFER (not the path), parse, and never poll.
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
        load_model_from_text(&name, &buf).unwrap_or_else(|e| {
            eprintln!(
                "Failed to load '{}': {e}",
                one_shot.as_deref().unwrap_or("-")
            );
            std::process::exit(1);
        })
    } else {
        load_model(&target_file).unwrap_or_else(|e| {
            eprintln!("Failed to load '{}': {}", target_file.display(), e);
            std::process::exit(1);
        })
    };

    // The event queue: one channel, one blocking consumer. The
    // input thread is the only crossterm reader; the inotify reload thread
    // (Linux) pushes reload events into the same channel.
    let (tx, rx) = mpsc::channel::<LoopEvent>();

    // File hot-reload: inotify on Linux (true event-driven),
    // otherwise the mtime+length poll on a 200 ms timer. One-shot
    // (stdin/FIFO) previews get NO watch — they load once and never poll.
    let mut watch: Option<ReloadWatch> = None;
    if one_shot.is_none() {
        #[cfg(target_os = "linux")]
        {
            if reload::spawn_inotify(&target_file, tx.clone()).is_none() {
                // inotify unavailable: fall back to the mtime-poll watch.
                let mut w = ReloadWatch::new(target_file.clone());
                w.baseline();
                watch = Some(w);
            }
        }
        #[cfg(not(target_os = "linux"))]
        {
            let mut w = ReloadWatch::new(target_file.clone());
            w.baseline();
            watch = Some(w);
        }
    }

    enable_raw_mode()?;
    // From here on every exit path (return value, `?` error or panic) must
    // hand back a usable terminal: the enhancement flags are sticky and would
    // otherwise leak into the shell. Dropping the guard runs `restore_terminal`.
    let _terminal = TerminalGuard;
    let mut stdout = io::stdout();
    // Mouse capture stays OFF (native drag-selection) and the cursor is
    // hidden for the session (the L2 direct-write present does not hide it).
    execute!(stdout, EnterAlternateScreen, Hide)?;
    // Enable the Kitty keyboard protocol for Press/Repeat/Release key events.
    // Keyboard enhancement (kitty protocol) is optional: terminals that don't
    // support it (e.g. the legacy Windows console API) run without it.
    let _ = execute!(
        stdout,
        PushKeyboardEnhancementFlags(
            KeyboardEnhancementFlags::REPORT_EVENT_TYPES
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
    let mut app = App::new(current_render, model_name, one_shot, target_file.clone());
    app.fit_if_braille();
    let mut engine = Engine::new(cols as usize, rows as usize);
    let mut terminal: Option<ratatui::Terminal<CrosstermBackend<io::Stdout>>> =
        Some(Terminal::new(CrosstermBackend::new(io::stdout()))?);

    let mut timers = TimerScheduler::new();
    let mut last = Instant::now();

    // Initial frame: the previous screen is all-space, so the first present
    // writes every non-space cell (same first-frame semantics as before).
    render_frame(&mut app, &mut engine, &mut stdout, &mut terminal)?;
    app.dirty = false;
    if watch.is_some() {
        timers.schedule(TimerId::ReloadPoll, Instant::now() + reload::POLL_INTERVAL);
    }
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
                    LoopEvent::Input(Event::Resize(cols, rows)) => {
                        engine.resize(cols as usize, rows as usize);
                        app.dirty = true;
                    }
                    LoopEvent::Input(ev) => {
                        if app.handle_input(ev) {
                            break 'main;
                        }
                    }
                    LoopEvent::Reload(rel) => {
                        handle_reload_event_loop(&mut app, &mut timers, rel);
                    }
                }
            }
            // Timers can fire mid-animation too (status expiry, reload).
            for id in timers.fire_due(now) {
                match id {
                    TimerId::ReloadParse => apply_reload_parse(&mut app, &mut timers),
                    TimerId::ReloadPoll => poll_reload(&mut watch, &mut app, &mut timers),
                    TimerId::ResizeCheck => check_resize(&mut app, &mut engine, &mut timers),
                    other => {
                        app.handle_timer(other, now);
                    }
                }
            }
            // One frame of smooth motion + held-key expiry.
            app.update_held(now, dt);
            if app.auto_spin {
                // Space auto-spin: rotate the model around its own (local) Y
                // axis (view::ViewState::spin_local) — a globe turning in
                // place, however it is pitched/rolled.
                app.view.spin_local(SPIN_RATE * dt);
                app.view.normalize();
            }
            render_frame(&mut app, &mut engine, &mut stdout, &mut terminal)?;
        } else {
            // Idle: block until the earliest pending timer or an event —
            // the thread is parked in the kernel (0% CPU).
            let wait = timers
                .earliest()
                .map(|d| d.saturating_duration_since(Instant::now()));
            let msg: Result<LoopEvent, mpsc::RecvTimeoutError> = match wait {
                Some(t) => rx.recv_timeout(t),
                None => match rx.recv() {
                    Ok(e) => Ok(e),
                    Err(_) => Err(mpsc::RecvTimeoutError::Disconnected),
                },
            };
            match msg {
                Ok(ev) => match ev {
                    LoopEvent::Input(Event::Resize(cols, rows)) => {
                        engine.resize(cols as usize, rows as usize);
                        app.dirty = true;
                    }
                    LoopEvent::Input(ev) => {
                        if app.handle_input(ev) {
                            break 'main;
                        }
                    }
                    LoopEvent::Reload(rel) => {
                        handle_reload_event_loop(&mut app, &mut timers, rel);
                    }
                },
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    // The kernel woke us exactly at the earliest deadline.
                    for id in timers.fire_due(now) {
                        match id {
                            TimerId::ReloadParse => apply_reload_parse(&mut app, &mut timers),
                            TimerId::ReloadPoll => poll_reload(&mut watch, &mut app, &mut timers),
                            TimerId::ResizeCheck => {
                                check_resize(&mut app, &mut engine, &mut timers)
                            }
                            other => {
                                app.handle_timer(other, now);
                            }
                        }
                    }
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => break 'main,
            }
            // Expire held keys that stopped reporting (a lost Release, or a
            // terminal that never sends one) without spinning.
            app.update_held(now, dt);
            // Redraw only when something actually changed (dirty-flag).
            if app.dirty {
                render_frame(&mut app, &mut engine, &mut stdout, &mut terminal)?;
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
        // A valid v1 file: `wrfm 1` magic, `vertices <N> edges <M>` counts
        // header, then `v` / `e` lines.
        let p = temp_wrfm(
            "valid",
            "wrfm 1\nvertices 2   edges 1\n\nv 0 0 0\nv 1 1 1\ne 0 1\n",
        );
        let (mode, name) = load_model(&p).expect("valid wrfm must load");
        let m = mode.braille().expect("wrfm loads as a braille model");
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
        // A `.txt` holding v1 wrfm content must open as wrfm — the extension
        // is only a hint, the content is authoritative.
        let p = temp_file(
            "model.txt",
            "wrfm 1\nvertices 2   edges 1\n\nv 0 0 0\nv 1 1 1\ne 0 1\n",
        );
        let (mode, name) = load_model(&p).expect("wrfm content in a .txt must open");
        let m = mode.braille().expect("wrfm loads as a braille model");
        assert_eq!(m.vertices.len(), 2);
        assert_eq!(m.edges.len(), 1);
        assert_eq!(name, "model");
    }

    #[test]
    fn open_detection_wrfm_with_garbage_is_unrecognized() {
        // Garbage is "unrecognized" (never an Ok empty model); a file
        // starting with `wrfm 1` is a parse (load) error instead.
        let p = temp_wrfm("garbage", "this is not a wireframe\nno markers here\n");
        let err = load_model(&p).err().expect("garbage must fail to load");
        assert!(
            err.contains("unrecognized"),
            "error should say 'unrecognized': {err}"
        );

        // Magic present, garbage after it: routed to the wrfm parser, which
        // must fail on the missing counts header — never "unrecognized".
        let p2 = temp_wrfm("magic-garbage", "wrfm 1\nthis is garbage after the magic\n");
        let err2 = load_model(&p2)
            .err()
            .expect("magic+garbage must fail to load");
        assert!(
            !err2.contains("unrecognized"),
            "a wrfm magic line must route to the wrfm parser: {err2}"
        );
    }

    #[test]
    fn open_detection_obj_content_probes_as_obj() {
        // Real obj content probes as obj regardless of the feature flag;
        // loading it requires the ratty feature (spec: "or 'requires ratty'
        // without the feature").
        let p = temp_file("cube.obj", "v 0 0 0\nv 1 0 0\nv 0 1 0\nf 1 2 3\n");
        assert_eq!(probe_format(&p).unwrap(), FileFormat::Obj);
        #[cfg(not(feature = "ratty"))]
        {
            let err = load_model(&p).err().expect("obj without ratty must fail");
            assert!(err.contains("ratty"), "error should mention ratty: {err}");
        }
    }

    #[test]
    fn open_detection_vertex_only_probes_as_obj() {
        // A file with only `v ` lines (no `e `) still probes as obj
        // (v-only is an obj marker, not a wrfm without the magic).
        let p = temp_wrfm("vertex-only", "v 0 0 0\nv 1 1 1\n");
        assert_eq!(probe_format(&p).unwrap(), FileFormat::Obj);
    }

    #[test]
    fn wrfm_style_without_magic_is_unrecognized() {
        // The v1 breaking change: a file with wrfm-style
        // `v`/`e` lines but NO `wrfm <version>` magic is NOT wrfm and NOT
        // obj — it is "unrecognized".
        let v_e = temp_wrfm("v-e", "v 0 0 0\nv 1 1 1\ne 0 1\n");
        assert!(
            !matches!(probe_format(&v_e), Ok(FileFormat::Wrfm)),
            "v/e without magic must not probe as wrfm"
        );
        assert!(
            !matches!(probe_format(&v_e), Ok(FileFormat::Obj)),
            "v/e without magic must not probe as obj"
        );
        let err = load_model(&v_e).err().expect("v/e without magic must fail");
        assert!(err.contains("unrecognized"), "error: {err}");

        // Edge-only or marker-less content is likewise never wrfm.
        let edge_only = temp_wrfm("edge-only", "e 0 1\n");
        assert!(
            !matches!(probe_format(&edge_only), Ok(FileFormat::Wrfm)),
            "an edge-only file is not wrfm by the detection contract"
        );
        let comment_only = temp_wrfm("comment-only", "# just a comment\n");
        assert!(probe_format(&comment_only).is_err());
    }

    #[test]
    fn wrfm_magic_beats_obj_markers() {
        // The magic test comes FIRST: a file starting with `wrfm 1` is
        // wrfm even if later lines look obj-ish (`f` is an unknown directive
        // that lenient parsing skips).
        let p = temp_wrfm(
            "magic-obj",
            "wrfm 1\nvertices 2   edges 1\n\nv 0 0 0\nv 1 0 0\ne 0 1\nf 1 2 3\n",
        );
        assert_eq!(probe_format(&p).unwrap(), FileFormat::Wrfm);
        let (mode, _) = load_model(&p).expect("magic must win over obj markers");
        let m = mode.braille().expect("wrfm loads as a braille model");
        assert_eq!(m.vertices.len(), 2);
        assert_eq!(m.edges.len(), 1);
    }

    #[test]
    fn empty_file_is_unrecognized() {
        // An empty file has no first line -> no magic -> unrecognized
        // — never an Ok empty model.
        let p = temp_wrfm("empty", "");
        assert!(probe_format(&p).is_err());
        let err = load_model(&p).err().expect("empty file must fail to load");
        assert!(
            err.contains("unrecognized"),
            "error should say 'unrecognized': {err}"
        );
    }

    #[test]
    fn load_model_rejects_half_written_file() {
        // Truncated v1 .wrfm still carries magic + `v`/`e` markers: the
        // parser fails and the caller keeps the last good model.
        let p = temp_wrfm(
            "half",
            "wrfm 1\nvertices 2   edges 1\n\nv 0 0 0\nv 1 1\ne 0 1\n",
        );
        assert!(load_model(&p).is_err());
    }

    #[test]
    fn load_model_rejects_bad_edge() {
        // An edge line missing its second index must fail to parse.
        let p = temp_wrfm(
            "bad-edge",
            "wrfm 1\nvertices 2   edges 1\n\nv 0 0 0\nv 1 1 1\ne 0",
        );
        assert!(load_model(&p).is_err());
    }

    #[test]
    fn load_model_error_reports_line() {
        // Surface the parser's structured error (line number + offending
        // line); magic routes content to the wrfm parser.
        let p = temp_wrfm(
            "bad-num",
            "wrfm 1\nvertices 1   edges 1\n\nv 1.0 2.0 abc\ne 0 0\n",
        );
        let err = load_model(&p).err().expect("bad-num must fail to load");
        assert!(err.contains("line"), "error should mention the line: {err}");
        assert!(
            err.contains("v 1.0 2.0 abc"),
            "error should carry the offending source line: {err}"
        );
    }

    #[test]
    fn load_model_rejects_deleted_file() {
        let p = temp_wrfm("gone", "wrfm 1\nvertices 1   edges 1\n\nv 0 0 0\ne 0 0\n");
        fs::remove_file(&p).unwrap();
        assert!(load_model(&p).is_err(), "a deleted file must fail to load");
    }

    // --- Hot-reload invariants (ok / error / missing) ---

    #[test]
    fn reload_preserves_view() {
        // Two models of very different sizes: after a reload the camera
        // keeps its distance and rotation by default ...
        let small = temp_wrfm(
            "small",
            "wrfm 1\nvertices 2   edges 1\n\nv 0 0 0\nv 1 0 0\ne 0 1\n",
        );
        let big = temp_wrfm(
            "big",
            "wrfm 1\nvertices 4   edges 3\n\nv 0 0 0\nv 100 0 0\nv 0 100 0\nv 0 0 100\ne 0 1\ne 0 2\ne 0 3\n",
        );
        let (mut current, mut name) = load_model(&small).unwrap();
        let mut view = ViewState::default();
        view.fit_to(current.braille().unwrap());
        view.add_yaw(1.0); // user turned the model
        let dist_before = view.dist;

        let (new_render, new_name) = load_model(&big).unwrap();
        apply_reload(&mut current, &mut name, new_render, new_name);
        assert_eq!(view.dist, dist_before, "distance must be preserved");
        assert!((view.yaw - 1.0).abs() < 1e-9, "rotation must be preserved");
        assert_eq!(name, big.file_stem().unwrap().to_str().unwrap());
    }

    #[test]
    fn reload_failure_keeps_old_render() {
        // A failed reload must not touch the current render: the swap only
        // happens inside apply_reload, which the caller invokes on success.
        let valid = temp_wrfm("valid2", "wrfm 1\nvertices 1   edges 1\n\nv 0 0 0\ne 0 0\n");
        let (current, _) = load_model(&valid).unwrap();
        let bad = temp_wrfm("bad", "wrfm 1\nvertices 1   edges 1\n\nv 0 0 0\ne 0 1\n");
        assert!(load_model(&bad).is_err());
        assert_eq!(
            current.braille().unwrap().vertices.len(),
            1,
            "the old model must still be held"
        );
    }

    #[test]
    fn hot_reload_changed_garbage_keeps_model_and_reports_parse_error() {
        // after a successful reload, write garbage -> the screen KEEPS
        // the last model + the event shows Parse Error.
        let p = temp_wrfm(
            "inv-garbage",
            "wrfm 1\nvertices 2   edges 1\n\nv 0 0 0\nv 1 0 0\ne 0 1\n",
        );
        let (mut current, mut name) = load_model(&p).unwrap();
        let mut view = ViewState::default();
        view.fit_to(current.braille().unwrap());
        let vertices_before = current.braille().unwrap().vertices.len();

        fs::write(&p, "this is garbage\n").unwrap();
        let record = handle_reload_event(ReloadEvent::Changed, &mut current, &mut name, &p)
            .expect("Changed must produce a reload record");
        assert_eq!(
            record.outcome,
            ReloadOutcome::ParseError,
            "garbage must record a parse-error outcome"
        );
        assert!(
            record.detail.contains("Parse Error"),
            "record: {:?}",
            record.detail
        );
        assert_eq!(
            current.braille().unwrap().vertices.len(),
            vertices_before,
            "garbage must never replace the on-screen model"
        );
    }

    #[test]
    fn hot_reload_missing_keeps_model_and_reports_file_removed() {
        // delete the file -> keeps the model + "file removed".
        let p = temp_wrfm(
            "inv-deleted",
            "wrfm 1\nvertices 2   edges 1\n\nv 0 0 0\nv 1 0 0\ne 0 1\n",
        );
        let (mut current, mut name) = load_model(&p).unwrap();
        let mut view = ViewState::default();
        view.fit_to(current.braille().unwrap());
        let vertices_before = current.braille().unwrap().vertices.len();

        fs::remove_file(&p).unwrap();
        let record = handle_reload_event(ReloadEvent::Missing, &mut current, &mut name, &p)
            .expect("Missing must produce a reload record");
        assert_eq!(
            record.outcome,
            ReloadOutcome::FileRemoved,
            "deletion must record a file-removed outcome"
        );
        assert!(
            record.detail.contains("file removed"),
            "record: {:?}",
            record.detail
        );
        assert_eq!(
            current.braille().unwrap().vertices.len(),
            vertices_before,
            "deletion must never replace the on-screen model"
        );
    }

    #[test]
    fn hot_reload_fixed_file_replaces_with_success() {
        // fix the file -> replaces + success (the only path that may
        // swap the model — the invariant).
        let p = temp_wrfm(
            "inv-fixed",
            "wrfm 1\nvertices 2   edges 1\n\nv 0 0 0\nv 1 0 0\ne 0 1\n",
        );
        let (mut current, mut name) = load_model(&p).unwrap();
        let mut view = ViewState::default();
        view.fit_to(current.braille().unwrap());

        // First break it: garbage -> Parse Error, model kept.
        fs::write(&p, "garbage\n").unwrap();
        let record =
            handle_reload_event(ReloadEvent::Changed, &mut current, &mut name, &p).unwrap();
        assert_eq!(record.outcome, ReloadOutcome::ParseError);
        let kept = current.braille().unwrap().vertices.len();

        // Then fix it: replaces + success.
        fs::write(
 &p,
            "wrfm 1\nvertices 4   edges 3\n\nv 0 0 0\nv 1 0 0\nv 0 1 0\nv 0 0 1\ne 0 1\ne 0 2\ne 0 3\n",
 )
 .unwrap();
        let record =
            handle_reload_event(ReloadEvent::Changed, &mut current, &mut name, &p).unwrap();
        assert_eq!(record.outcome, ReloadOutcome::Ok);
        assert_eq!(
            record.detail, "model reloaded",
            "record: {:?}",
            record.detail
        );
        assert_eq!(
            current.braille().unwrap().vertices.len(),
            4,
            "a fixed file must replace the model (was {kept})"
        );
    }

    #[test]
    fn hot_reload_unchanged_produces_no_event() {
        // Unchanged -> no event line (the previous event stays until
        // the next one replaces it).
        let p = temp_wrfm(
            "inv-unchanged",
            "wrfm 1\nvertices 1   edges 1\n\nv 0 0 0\ne 0 0\n",
        );
        let (mut current, mut name) = load_model(&p).unwrap();
        let mut view = ViewState::default();
        view.fit_to(current.braille().unwrap());
        assert_eq!(
            handle_reload_event(ReloadEvent::Unchanged, &mut current, &mut name, &p,),
            None,
            "Unchanged must not emit an event line"
        );
    }

    // --- HUD layout (status line / event rows) ---

    #[test]
    fn status_expired_after_timeout() {
        // The status line is transient: no deadline -> never expires; a
        // future deadline -> still shown; a passed deadline -> cleared so
        // the event region's rows return to the model canvas.
        let now = Instant::now();
        assert!(
            !status_expired(None, now),
            "no deadline must never be treated as expired"
        );
        assert!(
            !status_expired(Some(now + STATUS_TIMEOUT), now),
            "a deadline in the future must keep the status on screen"
        );
        assert!(
            status_expired(Some(now), now + STATUS_TIMEOUT),
            "a passed deadline must clear the status"
        );
    }

    #[test]
    fn hud_status_for_maps_all_four_states() {
        // Every reload outcome has a HUD line (transient 5 s); the four
        // states are ok / parse error / file removed (Unchanged shows
        // nothing — it never produces a record).
        assert_eq!(hud_status_for(ReloadOutcome::Ok), "hot-reloaded");
        assert_eq!(hud_status_for(ReloadOutcome::ParseError), "parse error");
        assert_eq!(hud_status_for(ReloadOutcome::FileRemoved), "file removed");
    }

    #[test]
    fn status_line_is_single_and_compact() {
        // The row-1 status is ONE line: the outcome label plus a compact
        // detail (the parse error's first line only, never the report body).
        let ok = ReloadRecord {
            outcome: ReloadOutcome::Ok,
            detail: "model reloaded".to_string(),
        };
        assert_eq!(status_line(&ok), "hot-reloaded");

        let err = ReloadRecord {
            outcome: ReloadOutcome::ParseError,
            detail: "Parse Error: error: invalid vertex at line 4, column 1\n 4 | v 1 1\n   |   ^^"
                .to_string(),
        };
        let line = status_line(&err);
        assert_eq!(line, "parse error: invalid vertex at line 4, column 1");
        assert!(!line.contains('\n'), "the report body must be cut: {line}");

        let removed = ReloadRecord {
            outcome: ReloadOutcome::FileRemoved,
            detail: "file removed (deleted or renamed away); keeping last model".to_string(),
        };
        let line = status_line(&removed);
        assert!(!line.contains('\n'), "must stay one row: {line}");
        assert!(line.contains("keeping last model"), "{line}");
    }

    #[test]
    fn hud_fixed_row_always_present_and_quiet_has_no_event_region() {
        // The fixed Row 0 is always present; with no event the event region
        // is absent and the canvas starts at row 1.
        let view = ViewState::default();
        let (row0, events, reserved) = hud_layout("cube", &view, "", 24, true, None);
        assert!(
            row0.starts_with("Wireforge: cube | yaw="),
            "fixed row must carry the model name and view: {row0}"
        );
        assert!(
            row0.contains("dist=") && row0.contains("pan=("),
            "row0: {row0}"
        );
        assert!(events.is_empty(), "no event -> no event lines");
        assert_eq!(reserved, 0, "quiet -> canvas starts at row 1");
    }

    #[test]
    fn hud_event_rows_appear_and_reserve_canvas_rows() {
        // On an event the event region renders below Row 0 (multiline for a
        // Parse Error) and the canvas loses those rows.
        let view = ViewState::default();
        let msg = "Parse Error: error: invalid vertex at line 2, column 1\n   |\n 2 | v 1 1\n   |   ^^\nexpected a number for the z coordinate, got `2`";
        let (row0, events, reserved) = hud_layout("cube", &view, msg, 24, true, None);
        assert_eq!(events.len(), 5, "multiline event splits into lines");
        assert_eq!(reserved, 5, "event region reserves its rows below row 0");
        assert!(
            !row0.contains("Parse Error"),
            "event text must never be squeezed into Row 0"
        );
        assert!(events[0].starts_with("Parse Error:"), "{}", events[0]);
    }

    #[test]
    fn hud_event_region_clamped_to_available_rows() {
        // A very long Parse Error must not push the canvas off screen: the
        // event region is clamped to height - 1.
        let view = ViewState::default();
        let msg = (0..40)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let (_, events, reserved) = hud_layout("cube", &view, &msg, 5, true, None);
        assert_eq!(reserved, 4, "event region clamps to height - 1");
        assert_eq!(events.len(), 40, "all lines are kept for rendering");
    }

    // ---------- stream input (stdin / FIFO one-shot) ----------

    #[test]
    fn probe_bytes_wrfm_magic() {
        // The buffer probe (used by stdin/FIFO) is the same content-first
        // authority as the file probe: `wrfm <version>` first line -> wrfm.
        let buf = b"wrfm 1\nvertices 2   edges 1\n\nv 0 0 0\nv 1 1 1\ne 0 1\n";
        assert_eq!(probe_bytes(buf).unwrap(), FileFormat::Wrfm);
        // The magic wins even with obj-looking markers later.
        let buf2 = b"wrfm 1\nf 1 2 3\n";
        assert_eq!(probe_bytes(buf2).unwrap(), FileFormat::Wrfm);
    }

    #[test]
    fn probe_bytes_obj_markers() {
        assert_eq!(
            probe_bytes(b"v 0 0 0\nv 1 0 0\nf 1 2 3\n").unwrap(),
            FileFormat::Obj
        );
        // v-only (no `e` line) is an obj marker too.
        assert_eq!(probe_bytes(b"v 0 0 0\nv 1 1 1\n").unwrap(), FileFormat::Obj);
    }

    #[test]
    fn probe_bytes_garbage_is_unrecognized() {
        // A stream of garbage (no magic, no obj markers) is "unrecognized" —
        // never an empty model; a magic-less `v`/`e` stream is likewise not
        // wrfm (the v1 breaking change).
        let err = probe_bytes(b"this is not a wireframe\n").unwrap_err();
        assert!(err.contains("unrecognized"), "error: {err}");
        assert!(probe_bytes(b"v 0 0 0\nv 1 1 1\ne 0 1\n").is_err());
        assert!(probe_bytes(b"").is_err(), "empty stream is unrecognized");
    }

    #[test]
    fn load_model_from_text_wrfm_parses() {
        let (mode, name) = load_model_from_text(
            "stream",
            "wrfm 1\nvertices 2   edges 1\n\nv 0 0 0\nv 1 1 1\ne 0 1\n",
        )
        .expect("wrfm stream must load");
        let m = mode.braille().expect("wrfm loads as a braille model");
        assert_eq!(m.vertices.len(), 2);
        assert_eq!(m.edges.len(), 1);
        assert_eq!(name, "stream");
    }

    #[test]
    fn load_model_from_text_garbage_fails() {
        let err = load_model_from_text("stream", "garbage here\nno markers\n")
            .err()
            .expect("garbage stream must fail");
        assert!(err.contains("unrecognized"), "error: {err}");
    }

    #[test]
    fn load_model_from_text_obj_routes_to_ratty() {
        // With the ratty feature a real obj stream routes to the Hardware3D
        // path (register_payload, no temp file); without it, it must fail
        // with the feature hint.
        let text = "v 0 0 0\nv 1 0 0\nv 0 1 0\nf 1 2 3\n";
        #[cfg(feature = "ratty")]
        {
            let (mode, name) = load_model_from_text("cube", text).expect("obj stream must load");
            assert!(
                matches!(mode, RenderMode::Hardware3D(_)),
                "obj routes to ratty"
            );
            assert_eq!(name, "cube");
        }
        #[cfg(not(feature = "ratty"))]
        {
            let err = load_model_from_text("cube", text)
                .err()
                .expect("obj without ratty must fail");
            assert!(err.contains("ratty"), "error: {err}");
        }
    }

    #[test]
    fn hud_one_shot_label_in_row_zero() {
        // In one-shot mode Row 0 advertises the no-hot-reload preview so the
        // user is never surprised by a preview that never refreshes.
        let view = ViewState::default();
        let (row0, _, _) = hud_layout(
            "stdin",
            &view,
            "",
            24,
            true,
            Some("stdin preview (no hot-reload)"),
        );
        assert!(
            row0.starts_with("Wireforge: stdin preview (no hot-reload) |"),
            "row0: {row0}"
        );
        // A regular file passes None and keeps the model name.
        let (row0b, _, _) = hud_layout("cube", &view, "", 24, true, None);
        assert!(row0b.starts_with("Wireforge: cube |"), "row0: {row0b}");
    }

    // ---------- event loop (input / holds) ----------

    #[test]
    fn app_handle_input_toggles() {
        let mut app = App::new(
            RenderMode::Braille(Model {
                vertices: vec![(0.0, 0.0, 0.0), (1.0, 0.0, 0.0)],
                edges: vec![(0, 1)],
            }),
            "cube".to_string(),
            None,
            PathBuf::from("cube.wrfm"),
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

        // Kitty protocol: Shift+'/' arrives as the base key plus SHIFT
        // (REPORT_ALTERNATE_KEYS is not pushed), so it must also toggle.
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
            RenderMode::Braille(Model {
                vertices: vec![(0.0, 0.0, 0.0), (1.0, 0.0, 0.0)],
                edges: vec![(0, 1)],
            }),
            "cube".to_string(),
            None,
            PathBuf::from("cube.wrfm"),
        );
        app.view.fit_to(app.current.braille().unwrap());
        let yaw0 = app.view.yaw;
        // Press: enter continuous state (motion applied by update_held)
        app.handle_input(Event::Key(KeyEvent::new(
            KeyCode::Right,
            KeyModifiers::NONE,
        )));
        assert!(
            app.held.contains_key(&Motion::YawRight),
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
            RenderMode::Braille(Model {
                vertices: vec![(0.0, 0.0, 0.0), (1.0, 0.0, 0.0)],
                edges: vec![(0, 1)],
            }),
            "cube".to_string(),
            None,
            PathBuf::from("cube.wrfm"),
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
    fn app_held_key_expires_without_release() {
        // A terminal without the kitty protocol never sends Release, so the
        // hold timeout is the only thing that can stop the motion.
        let mut app = App::new(
            RenderMode::Braille(Model {
                vertices: vec![(0.0, 0.0, 0.0), (1.0, 0.0, 0.0)],
                edges: vec![(0, 1)],
            }),
            "cube".to_string(),
            None,
            PathBuf::from("cube.wrfm"),
        );
        app.handle_input(Event::Key(KeyEvent::new(
            KeyCode::Right,
            KeyModifiers::NONE,
        )));
        let seen = *app
            .held
            .get(&Motion::YawRight)
            .expect("press must hold the motion");
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
            RenderMode::Braille(Model {
                vertices: vec![(0.0, 0.0, 0.0), (1.0, 0.0, 0.0)],
                edges: vec![(0, 1)],
            }),
            "cube".to_string(),
            None,
            PathBuf::from("cube.wrfm"),
        );
        let mut release = KeyEvent::new(KeyCode::Right, KeyModifiers::NONE);
        release.kind = KeyEventKind::Release;
        app.handle_input(Event::Key(release));
        assert!(app.release_seen, "a Release must enable key-up reporting");
        app.handle_input(Event::Key(KeyEvent::new(
            KeyCode::Right,
            KeyModifiers::NONE,
        )));
        let seen = *app
            .held
            .get(&Motion::YawRight)
            .expect("press must hold the motion");
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
            RenderMode::Braille(Model {
                vertices: vec![(0.0, 0.0, 0.0), (1.0, 0.0, 0.0)],
                edges: vec![(0, 1)],
            }),
            "cube".to_string(),
            None,
            PathBuf::from("cube.wrfm"),
        );
        for key in ['j', 'l'] {
            app.handle_input(Event::Key(KeyEvent::new(
                KeyCode::Char(key),
                KeyModifiers::NONE,
            )));
        }
        assert_eq!(app.held.len(), 2, "both motion keys are held");
        let first = *app
            .held
            .get(&Motion::PitchDown)
            .expect("j must hold the pitch motion");
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
            RenderMode::Braille(Model {
                vertices: vec![(0.0, 0.0, 0.0), (1.0, 0.0, 0.0)],
                edges: vec![(0, 1)],
            }),
            "cube".to_string(),
            None,
            PathBuf::from("cube.wrfm"),
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
            RenderMode::Braille(Model {
                vertices: vec![(0.0, 0.0, 0.0), (1.0, 0.0, 0.0)],
                edges: vec![(0, 1)],
            }),
            "cube".to_string(),
            None,
            PathBuf::from("cube.wrfm"),
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
        let seen = *app
            .held
            .get(&Motion::YawRight)
            .expect("press must hold the motion");
        let mut repeat = KeyEvent::new(KeyCode::Right, KeyModifiers::NONE);
        repeat.kind = KeyEventKind::Repeat;
        app.handle_input(Event::Key(repeat));
        let refreshed = *app
            .held
            .get(&Motion::YawRight)
            .expect("repeat must keep the hold");
        assert!(refreshed >= seen, "auto-repeat must refresh the hold");
    }

    #[test]
    fn reload_event_loop_schedules_deferred_parse() {
        let mut timers = TimerScheduler::new();
        let mut app = App::new(
            RenderMode::Braille(Model {
                vertices: vec![(0.0, 0.0, 0.0), (1.0, 0.0, 0.0)],
                edges: vec![(0, 1)],
            }),
            "cube".to_string(),
            None,
            PathBuf::from("cube.wrfm"),
        );
        // Changed -> no immediate status, but a deferred parse is scheduled.
        let changed = handle_reload_event_loop(&mut app, &mut timers, ReloadEvent::Changed);
        assert!(!changed);
        let parse_deadline = timers
            .earliest()
            .expect("a change must arm the deferred parse");
        let expected = Instant::now() + SETTLE_DELAY;
        let diff = if parse_deadline > expected {
            parse_deadline - expected
        } else {
            expected - parse_deadline
        };
        assert!(
            diff <= Duration::from_millis(50),
            "deferred parse deadline should be ~SETTLE_DELAY from now"
        );
        // Missing -> immediate status + StatusExpiry timer.
        let missing = handle_reload_event_loop(&mut app, &mut timers, ReloadEvent::Missing);
        assert!(missing);
        assert!(app.status_msg.contains("removed"));
        assert!(
            !app.status_msg.contains('\n'),
            "the status must stay a single row: {:?}",
            app.status_msg
        );
        // Clear the pending parse so the status-expiry timer is the earliest.
        timers.cancel(TimerId::ReloadParse);
        assert!(timers.earliest().is_some(), "status expiry must be armed");
        assert!(app.dirty);
    }
}
