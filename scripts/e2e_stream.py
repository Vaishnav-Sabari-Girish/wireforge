#!/usr/bin/env python3
"""End-to-end stream-input tests for the wireforge TUI (requires a PTY).

Drives the REAL TUI binary and verifies the PLAN-cli-stream §6 stream-input
contract:

  1. `stdin_opens_wrfm`        — pipe a v1 wrfm model into `wireforge -`
     (stdin is a pipe with EOF): it renders, and Row 0 shows the model name
     "stdin" (ratatui skips spaces in the captured stream, so the needle is
     space-free).
  2. `stdin_garbage_is_unrecognized` — `wireforge -` with garbage fails
     fast with "unrecognized file format" and exits 1.
  3. `fifo_path_loads_once`    — `wireforge <real-fifo>` reads the fifo ONCE
     and renders; Row 0 shows the fifo path.
  4. `fifo_ignores_later_writes` — after the one-shot load, a second writer to
     the same fifo does NOT re-render (the model is read only once).
  5. `pipe_stdin_keyboard`     — `cat model | wireforge -` (real pipe stdin)
     in a shell with a controlling terminal: keyboard is read from
     `/dev/tty`, so 'q' quits.
  6. `pipe_stdin_no_terminal`  — `wireforge -` with a piped model and NO
     controlling terminal fails fast with a clear "cannot open a terminal
     for keyboard input (/dev/tty)" message (never a cryptic ENXIO).

(OBJ over stdin is covered by the unit test
`load_model_from_text_obj_errors_with_a_convert_hint` — it needs no PTY
and runs under a plain `cargo test`.)

Matching notes (why fragments instead of full strings): ratatui renders
with diff-based cell updates, so a captured stream only contains the cells
that CHANGED between frames — spaces and identical characters are skipped.
So we match "Wireforge:", "stdin" and "stream.fifo" fragments after
stripping ANSI sequences.

Usage:  scripts/e2e_stream.py [path/to/wireforge-binary]
Defaults to ./target/debug/wireforge (run `cargo build` first).

Exit status: 0 = all checks passed, 1 = a check failed, 2 = usage error.
"""

import fcntl
import os
import pty
import re
import select
import shutil
import signal
import struct
import subprocess
import sys
import tempfile
import termios
import time

ANSI_CSI = re.compile(rb"\x1b\[[0-9;?]*[a-zA-Z]")
ANSI_OSC = re.compile(rb"\x1b\][^\x07\x1b]*(?:\x07|\x1b\\)")


def strip_ansi(data: bytes) -> str:
    data = ANSI_OSC.sub(b"", data)
    data = ANSI_CSI.sub(b"", data)
    return data.decode("utf-8", "replace")


def set_winsize(fd: int, rows: int, cols: int) -> None:
    fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", rows, cols, 0, 0))


class StreamTui:
    """A wireforge TUI fed by a stream.

    The TUI ALWAYS gets the PTY slave on stdin: crossterm's raw mode and
    key reading operate on stdin, so it must be a terminal. For `-` (stdin)
    mode the payload is delivered over the PTY in canonical mode and ended
    with a VEOF (Ctrl-D): the terminal then returns 0 on read -> EOF ->
    read_to_string returns, exactly like a user pasting a model and
    pressing Ctrl-D. Echo is disabled so the model text is not echoed back
    into the captured stream. For a FIFO path the TUI opens the fifo itself;
    stdin just sits on the PTY unused.
    """

    KEEP_TAIL = 200_000

    def __init__(self, binary: str, arg: str, payload: bytes | None):
        master, slave = pty.openpty()
        set_winsize(slave, 30, 100)
        # No echo: the model payload must not pollute the captured render.
        attrs = termios.tcgetattr(slave)
        attrs[3] = attrs[3] & ~termios.ECHO
        termios.tcsetattr(slave, termios.TCSANOW, attrs)
        self.proc = subprocess.Popen(
            [binary, arg],
            stdin=slave,
            stdout=slave,
            stderr=slave,
            close_fds=True,
        )
        os.close(slave)
        if payload is not None:
            # Deliver the payload, let the line discipline drain it, then
            # signal EOF with a VEOF (Ctrl-D) so read_to_string returns.
            os.write(master, payload)
            time.sleep(0.3)
            os.write(master, b"\x04")
        self.buf = bytearray()
        self.history = ""
        self.master = master

    def drain(self, timeout: float) -> None:
        end = time.monotonic() + timeout
        while time.monotonic() < end:
            ready, _, _ = select.select([self.master], [], [], 0.05)
            if not ready:
                continue
            try:
                chunk = os.read(self.master, 65536)
            except OSError:
                return
            if not chunk:
                return
            self.buf.extend(chunk)
            self.history += strip_ansi(chunk)
            if len(self.history) > self.KEEP_TAIL * 4:
                self.history = self.history[-self.KEEP_TAIL:]
            if len(self.buf) > self.KEEP_TAIL * 2:
                self.buf = self.buf[-self.KEEP_TAIL:]

    def text(self) -> str:
        return strip_ansi(bytes(self.buf))

    def wait_for(self, needle: str, timeout: float = 8.0) -> bool:
        end = time.monotonic() + timeout
        while time.monotonic() < end:
            self.drain(0.4)
            if needle in self.text():
                return True
        print(f"--- timed out waiting for {needle!r}; output tail:")
        print(self.text()[-2000:])
        return False

    def clear(self) -> None:
        self.buf.clear()

    def quit(self) -> int:
        try:
            os.write(self.master, b"q")
        except OSError:
            pass
        try:
            return self.proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            self.proc.kill()
            self.proc.wait()
            return -1

    def close(self) -> None:
        try:
            os.close(self.master)
        except OSError:
            pass


def read_model(path: str) -> str:
    with open(path) as f:
        return f.read()


def fifo_writer(src: str, fifo: str) -> subprocess.Popen:
    """Spawn `cat src > fifo` in its OWN session/process group.

    The writer blocks in open() until a reader opens the fifo. Without a
    new session, `proc.kill()` only kills the `sh` wrapper and the blocked
    `cat` survives as an orphan (leaking fd 1 into any pipe the test runs
    under, which made `| tail` pipelines appear to hang forever). With a
    dedicated process group we can reap the whole tree via killpg.
    """
    return subprocess.Popen(
        ["sh", "-c", f"cat {src} > {fifo}"],
        start_new_session=True,
    )


def kill_process_group(proc: subprocess.Popen) -> None:
    """Kill the writer and every child (the blocked cat) and reap it."""
    try:
        os.killpg(os.getpgid(proc.pid), signal.SIGKILL)
    except ProcessLookupError:
        pass  # already gone
    try:
        proc.wait(timeout=5)
    except subprocess.TimeoutExpired:
        proc.kill()
        proc.wait()


def wait_writer(proc: subprocess.Popen, timeout: float) -> bool:
    """Wait for a writer that is EXPECTED to exit; force-clean on timeout.

    Returns True when it exited on its own, False when it had to be killed
    (in which case the writer's check already recorded the failure — the
    point here is only to never leak the process).
    """
    try:
        proc.wait(timeout=timeout)
        return True
    except subprocess.TimeoutExpired:
        kill_process_group(proc)
        return False


def main() -> int:
    binary = sys.argv[1] if len(sys.argv) > 1 else "./target/debug/wireforge"
    binary = os.path.abspath(binary)
    if not os.path.exists(binary):
        print(f"binary not found: {binary} (run `cargo build` first)")
        return 2

    repo = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
    cube_path = os.path.join(repo, "wrfm_files", "cube.wrfm")
    tetra_path = os.path.join(repo, "wrfm_files", "tetrahedron.wrfm")
    cube = read_model(cube_path)

    checks = []
    with tempfile.TemporaryDirectory(prefix="wrfm-stream-e2e-") as td:
        # --- 1. stdin_opens_wrfm -------------------------------------------
        tui = StreamTui(binary, "-", cube.encode())
        checks.append(("stdin renders", tui.wait_for("Wireforge:")))
        checks.append(("stdin row0 shows the model name", "stdin" in tui.text()))
        tui.clear()
        code = tui.quit()
        checks.append(("stdin quits cleanly", code == 0))
        tui.close()

        # --- 2. stdin_garbage_is_unrecognized ------------------------------
        bad = StreamTui(binary, "-", b"this is not a wireframe\nno markers here\n")
        # It must fail fast (exit 1) with an "unrecognized" message on stderr.
        try:
            rc = bad.proc.wait(timeout=10)
        except subprocess.TimeoutExpired:
            bad.proc.kill()
            bad.proc.wait()
            rc = -1
        bad.drain(0.3)
        checks.append(("garbage exits 1", rc == 1))
        checks.append(("garbage says unrecognized", "unrecognized" in bad.text()))
        bad.close()

        # --- 3. fifo_path_loads_once ---------------------------------------
        fifo = os.path.join(td, "stream.fifo")
        os.mkfifo(fifo)
        # A writer that opens the fifo (blocks until the TUI reader opens),
        # writes the cube once and exits -> EOF.
        writer = fifo_writer(cube_path, fifo)
        tui = StreamTui(binary, fifo, None)
        checks.append(("fifo renders", tui.wait_for("Wireforge:")))
        checks.append(("fifo row0 shows the fifo path", "stream.fifo" in tui.text()))
        wait_writer(writer, timeout=10)
        tui.clear()
        # The fifo is read ONCE, at start-up: after the writer's EOF the
        # screen is stable and no further write can reach the viewer.
        time.sleep(1.5)
        tui.drain(0.5)

        # --- 4. fifo_ignores_later_writes ----------------------------------
        # A second writer to the same fifo must NOT re-render: wireforge
        # loaded once, so this writer blocks forever with no reader. The
        # screen keeps showing the cube.
        before = tui.text()
        second = fifo_writer(tetra_path, fifo)
        time.sleep(1.5)
        tui.drain(0.5)
        checks.append(("fifo second write does not re-render", tui.text() == before))
        kill_process_group(second)
        code = tui.quit()
        checks.append(("fifo quits cleanly", code == 0))
        tui.close()

        # --- 5. pipe_stdin_keyboard: real pipe stdin + controlling terminal ---
        # `cat model | wireforge -` in an interactive bash on a PTY: stdin is
        # a pipe (the model), so keyboard MUST come from /dev/tty (the
        # controlling terminal). 'q' must quit.
        pmaster, pslave = pty.openpty()
        set_winsize(pslave, 30, 100)
        shell = subprocess.Popen(
            ["bash", "-i"],
            stdin=pslave,
            stdout=pslave,
            stderr=pslave,
            close_fds=True,
            preexec_fn=os.setsid,
        )
        os.close(pslave)
        time.sleep(1.0)

        def pty_drain(fd, timeout):
            out = bytearray()
            end = time.monotonic() + timeout
            while time.monotonic() < end:
                ready, _, _ = select.select([fd], [], [], 0.05)
                if not ready:
                    continue
                try:
                    chunk = os.read(fd, 65536)
                except OSError:
                    break
                if not chunk:
                    break
                out.extend(chunk)
            return out

        os.write(pmaster, f"cat {cube_path} | {binary} -\n".encode())
        time.sleep(1.5)
        launch = pty_drain(pmaster, 2.0)
        checks.append(("pipe stdin renders", b"Wireforge" in launch))
        os.write(pmaster, b"q")
        time.sleep(1.5)
        after_q = pty_drain(pmaster, 1.0)
        # After 'q' the shell prompt returns => wireforge exited via /dev/tty keys.
        checks.append(("pipe stdin keyboard quits (q via /dev/tty)", b"]$" in after_q or b"$" in after_q))
        shell.kill()
        shell.wait()
        os.close(pmaster)

        # --- 6. pipe_stdin_no_terminal: clear error instead of ENXIO --------
        r, w = os.pipe()
        os.write(w, cube.encode())
        os.close(w)
        notty = subprocess.Popen(
            [binary, "-"],
            stdin=r,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        os.close(r)
        _, nerr = notty.communicate(timeout=10)
        checks.append(("pipe stdin no-terminal exits 1", notty.returncode == 1))
        checks.append(
            ("pipe stdin no-terminal clear error",
             b"cannot open a terminal for keyboard input" in nerr and b"/dev/tty" in nerr)
        )

    failed = [name for name, ok in checks if not ok]
    for name, ok in checks:
        print(f"[{'PASS' if ok else 'FAIL'}] {name}")
    if failed:
        print(f"FAILED: {failed}")
        return 1
    print("All stream-input e2e checks passed.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
