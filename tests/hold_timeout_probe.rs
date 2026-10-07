//! Integration probe for held keys on a terminal that never reports key-up:
//! the pty here is dumb, so it cannot answer the kitty keyboard protocol and
//! the app only ever receives Press events. A held motion key must still stop
//! by itself (hold timeout) instead of drifting forever at 100% CPU.
use std::io::{ErrorKind, Read, Write};
use std::os::unix::io::AsRawFd;
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Drain `master` until `window` passes without a single byte (the app is
/// quiet) or `deadline` expires. Returns the bytes read and whether the app
/// went quiet. A WouldBlock read continues the window instead of ending it,
/// so the gap between two frames is never mistaken for idleness.
fn drain_until_quiet(
    master: &mut std::fs::File,
    buf: &mut [u8],
    deadline: Instant,
    window: Duration,
) -> (usize, bool) {
    let mut total = 0;
    let mut quiet_since: Option<Instant> = None;
    loop {
        let now = Instant::now();
        if now >= deadline {
            return (total, false);
        }
        let got_byte = match master.read(buf) {
            Ok(0) => false,
            Ok(n) => {
                total += n;
                true
            }
            Err(e) if e.kind() == ErrorKind::WouldBlock => false,
            // The pty is gone: nothing else can arrive.
            Err(_) => return (total, true),
        };
        if got_byte {
            quiet_since = None;
        } else {
            if now.duration_since(*quiet_since.get_or_insert(now)) >= window {
                return (total, true);
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

#[test]
fn held_key_without_release_stops_by_itself() {
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

    let mut buf = [0u8; 8192];
    // The initial frame must finish before the key press.
    let (_, quiet) = drain_until_quiet(
        &mut master,
        &mut buf,
        Instant::now() + Duration::from_secs(5),
        Duration::from_millis(400),
    );
    assert!(quiet, "app never went quiet before the key press");

    // One press, no key-up: the pty can never report a Release.
    master.write_all(b"h").expect("write key press");

    // The hold must expire on its own, so the output settles again before the
    // deadline. Without the timeout the model keeps drifting and the app keeps
    // repainting forever, so `quiet` never turns true.
    let (motion, quiet) = drain_until_quiet(
        &mut master,
        &mut buf,
        Instant::now() + Duration::from_secs(5),
        Duration::from_millis(400),
    );
    set_nonblock(master.as_raw_fd(), false);
    let _ = child.kill();
    let _ = child.wait();
    eprintln!("hold probe: repaint bytes after the press = {motion}, quiet = {quiet}");
    assert!(
        motion > 0,
        "the press must move the model at least once (input handling broken)"
    );
    assert!(
        quiet,
        "a held key with no Release must stop by itself (stuck motion / 100% CPU)"
    );
}
