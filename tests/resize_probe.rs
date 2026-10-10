//! Integration probes driven through a real pty:
//!
//! * `sigwinch_delivers_resize_event` — acceptance criterion "resize
//!   redraws": the real binary, in a PTY, must repaint when the terminal is
//!   resized. The primary path is crossterm's `Resize` event; a 500 ms
//!   size-check timer is the fallback (PLAN §4.3 timers). The probe waits
//!   for the app to be quiet, resizes the pty, and asserts the app writes a
//!   repaint.
//! * `no_file_argument_opens_the_empty_space` — `wireforge` with NO arguments
//!   on a terminal must open the viewer on an empty model instead of failing
//!   with a clap "required arguments were not provided" error: the HUD names
//!   the empty state, the XYZ axes of the empty space are rasterized, the app
//!   stays interactive, and `q` quits.
use std::io::{Read, Write};
use std::os::unix::io::AsRawFd;
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// Spawn `bin` (with `args`) on a fresh 30x100 pty; returns the child and the
/// master side of the pty. stdout/stderr/stdin of the child are the pts, so
/// the CLI sees a terminal on stdin exactly like an interactive shell.
fn spawn_on_pty(bin: &str, args: &[&str]) -> (Child, std::fs::File) {
    let slave = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/ptmx")
        .expect("open ptmx");
    unsafe {
        assert_eq!(libc::unlockpt(slave.as_raw_fd()), 0, "unlockpt");
    }
    let ptsname = unsafe {
        let p = libc::ptsname(slave.as_raw_fd());
        assert!(!p.is_null());
        std::ffi::CStr::from_ptr(p).to_str().unwrap().to_string()
    };
    let master = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&ptsname)
        .expect("open pts");
    let ws = libc::winsize {
        ws_row: 30,
        ws_col: 100,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    unsafe {
        assert_eq!(libc::ioctl(slave.as_raw_fd(), libc::TIOCSWINSZ, &ws), 0);
    }
    let slave_fd = slave.as_raw_fd();
    let child = unsafe {
        Command::new(bin)
            .args(args)
            .pre_exec(move || {
                libc::dup2(slave_fd, 0);
                libc::dup2(slave_fd, 1);
                libc::dup2(slave_fd, 2);
                libc::setsid();
                Ok(())
            })
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn")
    };
    // The child owns the terminal now; the parent keeps the master only.
    drop(slave);
    (child, master)
}

/// Wait for `f` to hold, up to `timeout` (the app needs a moment to exit).
fn wait_until(timeout: Duration, mut f: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if f() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    f()
}

/// Put the pty master in non-blocking mode.
fn set_nonblock(fd: i32, on: bool) {
    unsafe {
        let flags = libc::fcntl(fd, libc::F_GETFL);
        let flags = if on {
            flags | libc::O_NONBLOCK
        } else {
            flags & !libc::O_NONBLOCK
        };
        libc::fcntl(fd, libc::F_SETFL, flags);
    }
}

/// Read from `master` until `window` passes without a single byte (the app is
/// quiet, i.e. waiting for input) or `deadline` expires. Returns every byte
/// read and whether the app went quiet. The master must be non-blocking.
fn drain_until_quiet(
    master: &mut std::fs::File,
    buf: &mut [u8],
    deadline: Instant,
    window: Duration,
) -> (Vec<u8>, bool) {
    let mut seen: Vec<u8> = Vec::new();
    let mut quiet_since: Option<Instant> = None;
    loop {
        let now = Instant::now();
        if now >= deadline {
            return (seen, false);
        }
        let got_byte = match master.read(buf) {
            Ok(0) => false,
            Ok(n) => {
                seen.extend_from_slice(&buf[..n]);
                true
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => false,
            // The pty is gone: nothing else can arrive.
            Err(_) => return (seen, true),
        };
        if got_byte {
            quiet_since = None;
        } else if now.duration_since(*quiet_since.get_or_insert(now)) >= window {
            return (seen, true);
        } else {
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

/// Whether `params` (the bytes of one `ESC[…m`) leave a background colour
/// selected. `0` resets, `49` is the default background, `40..=47` /
/// `100..=107` pick one — Wireforge only emits a leading reset, an fg slot
/// and a bg slot, but this reads any SGR.
fn sgr_selects_background(params: &[u8]) -> bool {
    let mut bg = false;
    for part in String::from_utf8_lossy(params).split(';') {
        match part.parse::<u16>() {
            Ok(0) | Ok(49) => bg = false,
            Ok(40..=47) | Ok(100..=107) => bg = true,
            _ => {}
        }
    }
    bg
}

/// Walk one session's captured output — every frame concatenated, exactly as
/// the terminal sees it — and report, as `(clear, end)`, whether any `ESC[2J`
/// ran while a background colour was selected and whether the stream ends
/// with one still selected.
///
/// Both matter: an erase fills with the *selected* background on any terminal
/// with background-colour-erase (xterm's default), so a clear in that state
/// paints the screen with that colour instead of blanking it — and a
/// background left selected paints the next erase the terminal does on its
/// own, which is what a window resize exposes.
fn background_leaks(data: &[u8]) -> (bool, bool) {
    let mut bg = false;
    let mut clear = false;
    let mut i = 0;
    while i < data.len() {
        if data[i] != 0x1b {
            i += 1;
            continue;
        }
        if data.get(i + 1) != Some(&b'[') {
            i += 2;
            continue;
        }
        let start = i + 2;
        let mut end = start;
        while end < data.len() && !(0x40..=0x7e).contains(&data[end]) {
            end += 1;
        }
        if end >= data.len() {
            break;
        }
        match data[end] {
            b'm' => bg = sgr_selects_background(&data[start..end]),
            b'J' if &data[start..end] == b"2" && bg => clear = true,
            _ => {}
        }
        i = end + 1;
    }
    (clear, bg)
}

/// Remove ANSI escape sequences from captured pty output, leaving the text
/// the terminal would display.
fn strip_ansi(data: &str) -> String {
    let mut out = String::new();
    let mut chars = data.chars();
    while let Some(c) = chars.next() {
        if c != '\x1b' {
            out.push(c);
        } else if let Some('[') = chars.next() {
            // CSI: parameters/intermediates, ending with a final byte @..~.
            for c in chars.by_ref() {
                if ('@'..='~').contains(&c) {
                    break;
                }
            }
        }
        // A lone ESC or any other two-character escape is dropped as well.
    }
    out
}

#[test]
fn no_file_argument_opens_the_empty_space() {
    let bin = env!("CARGO_BIN_EXE_wireforge");
    let (mut child, mut master) = spawn_on_pty(bin, &[]);
    set_nonblock(master.as_raw_fd(), true);
    let mut buf = [0u8; 8192];

    // Read until the first frame has been written and the app goes quiet
    // (waiting for input). A quiet window, not "the first read returned
    // nothing": the terminal setup bytes arrive before the first frame does.
    let (seen, quiet) = drain_until_quiet(
        &mut master,
        &mut buf,
        Instant::now() + Duration::from_secs(10),
        Duration::from_millis(600),
    );
    assert!(quiet, "a blank viewer must render and then idle");
    let text = strip_ansi(&String::from_utf8_lossy(&seen));
    eprintln!("no-args probe: {} bytes of TUI output", seen.len());
    assert!(
        text.contains("yaw=") && text.contains("dist="),
        "blank viewer must render its telemetry row, got: {text:?}"
    );
    // ratatui's diff-based present skips spaces and repaints only changed
    // cells, so compare with every space removed: a text cell like "no file"
    // arrives as the fragments "no" and "file".
    let packed: String = text.chars().filter(|c| !c.is_whitespace()).collect();
    assert!(
        packed.contains("nofile\u{e0b0}"),
        "the statusline pill must name the empty state, got: {text:?}"
    );
    assert!(
        !text.contains("Wireforge:"),
        "the model's name belongs on the statusline, not Row 0: {text:?}"
    );
    // The empty model still draws the space: the origin cross is rasterized
    // as braille dots (`U+2800..=U+28FF`) and carries the X/Y/Z axis labels.
    let braille = text
        .chars()
        .filter(|c| ('\u{2800}'..='\u{28ff}').contains(c))
        .count();
    assert!(
        braille > 0,
        "the empty scene must draw the XYZ axes, got: {text:?}"
    );
    assert!(
        text.contains('X') || text.contains('Y') || text.contains('Z'),
        "the axes must keep their labels, got: {text:?}"
    );
    eprintln!("no-args probe: {braille} braille cells on the empty scene");
    // The statusline: the lamps and the everyday chords are on screen, and the
    // ground under them reached the terminal as a background SGR. Wireforge
    // paints no background anywhere else, so this is also the end-to-end proof
    // that a cell's background survives the blit and the diffing writer.
    assert!(
        packed.contains("●SPIN") && packed.contains("●AXES"),
        "the statusline must carry its state lamps, got: {text:?}"
    );
    assert!(
        packed.contains("?help") && packed.contains("qquit"),
        "the statusline must carry the everyday chords, got: {text:?}"
    );
    assert!(
        seen.windows(10).any(|w| w == b"\x1b[0;30;42m"),
        "the name pill must paint its accent ground, got {} bytes",
        seen.len()
    );
    assert!(
        seen.windows(7).any(|w| w == b"\x1b[0;40m"),
        "the strip must paint its own ground, got {} bytes",
        seen.len()
    );
    assert!(
        !text.contains("required arguments"),
        "no arguments must start the viewer, not fail with a clap error: {text:?}"
    );
    assert!(
        child.try_wait().expect("try_wait").is_none(),
        "the empty viewer must still be running, waiting for input"
    );

    // Holding a modifier switches the hints. The kitty keyboard protocol
    // reports the modifier key itself — `ESC[57442u` is left Ctrl down, and
    // with event types on, `ESC[57442;1:3u` is it coming back up. This pty is
    // not a kitty terminal, but the app parses whatever bytes it is handed,
    // which is the half of the feature that lives here.
    let mut next_frame = |master: &mut std::fs::File| -> String {
        let (seen, quiet) = drain_until_quiet(
            master,
            &mut buf,
            Instant::now() + Duration::from_secs(5),
            Duration::from_millis(300),
        );
        assert!(quiet, "the viewer must repaint and then idle");
        strip_ansi(&String::from_utf8_lossy(&seen))
    };
    master.write_all(b"\x1b[57442u").expect("ctrl down");
    let held = next_frame(&mut master);
    let held_packed: String = held.chars().filter(|c| !c.is_whitespace()).collect();
    assert!(
        held_packed.contains("Ctrl+h/l/k/j/d/f"),
        "holding Ctrl must switch the hints to the Ctrl chords, got: {held:?}"
    );
    assert!(
        !held_packed.contains("?help"),
        "the everyday hints step aside while Ctrl is held, got: {held:?}"
    );

    master.write_all(b"\x1b[57442;1:3u").expect("ctrl up");
    let released = next_frame(&mut master);
    let released_packed: String = released.chars().filter(|c| !c.is_whitespace()).collect();
    assert!(
        released_packed.contains("?help") && released_packed.contains("Spacespin"),
        "letting Ctrl go must bring the everyday hints back, got: {released:?}"
    );

    // `q` quits the empty view like it quits a model view.
    master.write_all(b"q").expect("write key press");
    let exited = wait_until(Duration::from_secs(5), || {
        child.try_wait().expect("try_wait").is_some()
    });
    assert!(exited, "`q` must quit the empty viewer");
    let status = child.wait().expect("wait");
    let _ = child.kill();
    eprintln!("no-args probe: exit status = {status:?}");
    assert!(
        status.success(),
        "the blank viewer must quit cleanly, got {status:?}"
    );
}

#[test]
fn sigwinch_delivers_resize_event() {
    let bin = env!("CARGO_BIN_EXE_wireforge");
    let slave = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/ptmx")
        .expect("open ptmx");
    unsafe {
        assert_eq!(libc::unlockpt(slave.as_raw_fd()), 0, "unlockpt");
    }
    let ptsname = unsafe {
        let p = libc::ptsname(slave.as_raw_fd());
        assert!(!p.is_null());
        std::ffi::CStr::from_ptr(p).to_str().unwrap().to_string()
    };
    let mut master = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&ptsname)
        .expect("open pts");
    let ws = libc::winsize {
        ws_row: 30,
        ws_col: 100,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    unsafe {
        assert_eq!(libc::ioctl(slave.as_raw_fd(), libc::TIOCSWINSZ, &ws), 0);
    }
    let slave_fd = slave.as_raw_fd();
    let mut child = unsafe {
        Command::new(bin)
            .arg("wrfm_files/cube.wrfm")
            .pre_exec(move || {
                libc::dup2(slave_fd, 0);
                libc::dup2(slave_fd, 1);
                libc::dup2(slave_fd, 2);
                libc::setsid();
                Ok(())
            })
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn")
    };
    drop(slave);

    // Let the app fully settle, then drain until it is quiet.
    std::thread::sleep(Duration::from_millis(2000));
    let mut buf = [0u8; 8192];
    let set_nonblock = |fd: i32, on: bool| unsafe {
        let flags = libc::fcntl(fd, libc::F_GETFL);
        let flags = if on {
            flags | libc::O_NONBLOCK
        } else {
            flags & !libc::O_NONBLOCK
        };
        libc::fcntl(fd, libc::F_SETFL, flags);
    };
    set_nonblock(master.as_raw_fd(), true);
    // Everything the app has written so far, frames concatenated: the SGR
    // state a later escape runs in is the one the earlier frames left.
    let mut stream: Vec<u8> = Vec::new();
    // drain until no bytes arrive for 400 ms (app fully idle)
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut quiet = false;
    while Instant::now() < deadline {
        let read_start = Instant::now();
        let mut any = false;
        while Instant::now() - read_start < Duration::from_millis(400) {
            match master.read(&mut buf) {
                Ok(0) => {}
                Ok(n) => {
                    stream.extend_from_slice(&buf[..n]);
                    any = true;
                }
                Err(_) => break,
            }
        }
        if !any {
            quiet = true;
            break;
        }
    }
    assert!(quiet, "app never went quiet before the resize");

    // Resize the pty (40x120) and nudge SIGWINCH.
    let ws2 = libc::winsize {
        ws_row: 40,
        ws_col: 120,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    unsafe {
        assert_eq!(libc::ioctl(master.as_raw_fd(), libc::TIOCSWINSZ, &ws2), 0);
        assert_eq!(libc::kill(child.id() as i32, libc::SIGWINCH), 0);
    }
    // Wait for the repaint (Resize event or the 500 ms fallback timer).
    std::thread::sleep(Duration::from_millis(2500));
    let mut post: Vec<u8> = Vec::new();
    while let Ok(n) = master.read(&mut buf) {
        if n == 0 {
            break;
        }
        post.extend_from_slice(&buf[..n]);
    }
    stream.extend_from_slice(&post);
    set_nonblock(master.as_raw_fd(), false);
    let _ = child.kill();
    let _ = child.wait();
    // The repaint must wipe the OLD size's pixels first (ESC[2J), otherwise
    // stale model pixels survive next to the re-projected model — and it must
    // erase at DEFAULT colours: the frame before it ends on the statusline's
    // ground, so an ESC[2J issued with that background still selected fills
    // the whole screen with it (background-colour-erase) and the strip's
    // colour floods the canvas.
    let has_clear = post.windows(4).any(|w| w == b"\x1b[2J");
    let (clear_leak, end_leak) = background_leaks(&stream);
    let post_len = post.len();
    eprintln!(
        "resize probe: repaint bytes after resize = {post_len}, clear-screen = {has_clear}, \
         clear-at-background = {clear_leak}, ends-with-background = {end_leak}"
    );
    assert!(
        !post.is_empty(),
        "no repaint after resize (resize handling broken)"
    );
    assert!(
        has_clear,
        "resize repaint must emit a clear-screen (stale-model bug)"
    );
    assert!(
        !clear_leak,
        "ESC[2J must erase at default colours, never with the statusline's \
         background selected (it would flood the canvas)"
    );
    assert!(
        !end_leak,
        "the app must leave no background selected between frames, or the \
         terminal's own erase on the next resize paints the strip's colour"
    );
}
