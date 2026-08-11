//! Integration probe for acceptance criterion "resize redraws": the real
//! binary, in a PTY, must repaint when the terminal is resized. The
//! primary path is crossterm's `Resize` event; a 500 ms size-check timer
//! is the fallback (PLAN §4.3 timers). The probe waits for the app to be
//! quiet, resizes the pty, and asserts the app writes a repaint.
use std::io::Read;
use std::os::unix::io::AsRawFd;
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

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
    // drain until no bytes arrive for 400 ms (app fully idle)
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut quiet = false;
    while Instant::now() < deadline {
        let read_start = Instant::now();
        let mut any = false;
        while Instant::now() - read_start < Duration::from_millis(400) {
            match master.read(&mut buf) {
                Ok(0) => {}
                Ok(_) => any = true,
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
    let mut total = 0;
    while let Ok(n) = master.read(&mut buf) {
        total += n;
    }
    set_nonblock(master.as_raw_fd(), false);
    let _ = child.kill();
    let _ = child.wait();
    // The repaint must wipe the OLD size's pixels first (ESC[2J), otherwise
    // stale model pixels survive next to the re-projected model.
    let has_clear = buf.windows(4).any(|w| w == b"\x1b[2J");
    eprintln!("resize probe: repaint bytes after resize = {total}, clear-screen = {has_clear}");
    assert!(
        total > 0,
        "no repaint after resize (resize handling broken)"
    );
    assert!(
        has_clear,
        "resize repaint must emit a clear-screen (stale-model bug)"
    );
}
