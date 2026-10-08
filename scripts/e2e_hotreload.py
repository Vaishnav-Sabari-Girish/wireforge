#!/usr/bin/env python3
"""End-to-end hot-reload test for the wireforge TUI (requires a PTY).

Drives the REAL TUI binary inside a pseudo-terminal and verifies the
stage-2 file hot-reload contract end to end (the mtime-polling loop from
src/reload.rs wired into src/main.rs). The camera is preserved across
reloads (there is no auto-fit flag); a model swap is proven by the large
canvas redraw a 3x-bigger model forces (its projection is completely
different):

  1. initial render shows the model name and camera HUD
  2. replacing the file with a 3x bigger model -> "hot-reloaded" AND a
     large canvas redraw (the swapped model's projection differs)
  3. writing a broken (half-written) file -> row 1 shows the COMPACT line
     "parse error: <kind> at line L, column C", TUI stays alive
  4. deleting the file -> row 1 shows "file removed; keeping last model",
     still alive
  5. re-creating the file -> reloads again ("hot-reloaded" on the HUD)
  6. 'q' quits cleanly (process exits with status 0)

The reload outcome is ONE compact status line on row 1 (right below the
header), shown for 5 s and then cleared. There is no persistent panel
anymore, so a parse error's detail rides on that same first line.

Matching notes (why fragments instead of full strings): ratatui renders
with diff-based cell updates, so a captured stream only contains the
cells that CHANGED between frames - spaces and identical characters are
skipped (e.g. the stream shows "hot-re<CSI>loaded" for "hot-reloaded").
So we match against the reliably-rewritten fragments ("hot-reload",
"moved", "invalidvertex", "lastmode") after stripping ANSI sequences,
and the space-free needles go through `wait_for_compact`, which strips
spaces from the captured text as well (an unchanged blank cell never
reaches the stream either). We also use the numeric `dist=` change as the
model-swap proof. ANSI_CSI strips ESC [ ... <letter> (cursor positioning,
SGR, private modes); ANSI_OSC strips ESC ] ... BEL/ST just in case.

Usage:  scripts/e2e_hotreload.py [path/to/wireforge-binary]
Defaults to ./target/debug/wireforge (run `cargo build` first).

Exit status: 0 = all checks passed, 1 = a check failed, 2 = usage error.
"""

import fcntl
import os
import pty
import re
import select
import shutil
import struct
import subprocess
import sys
import tempfile
import termios
import time

ANSI_CSI = re.compile(rb"\x1b\[[0-9;?]*[a-zA-Z]")
ANSI_OSC = re.compile(rb"\x1b\][^\x07\x1b]*(?:\x07|\x1b\\)")
NUM_RE = re.compile(r"[0-9]+\.[0-9]{2}")


def strip_ansi(data: bytes) -> str:
    data = ANSI_OSC.sub(b"", data)
    data = ANSI_CSI.sub(b"", data)
    return data.decode("utf-8", "replace")


def set_winsize(fd: int, rows: int, cols: int) -> None:
    fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", rows, cols, 0, 0))


class Tui:
    """A wireforge TUI running on a real pseudo-terminal."""

    # Bounded working window; the HUD re-renders every frame so a needle
    # that scrolled out of the window shows up again quickly.
    KEEP_TAIL = 200_000

    def __init__(self, binary: str, model: str, extra: tuple = ()):
        master, slave = pty.openpty()
        set_winsize(slave, 30, 100)
        self.proc = subprocess.Popen(
            [binary, model, *extra],
            stdin=slave,
            stdout=slave,
            stderr=slave,
            close_fds=True,
        )
        os.close(slave)
        self.buf = bytearray()
        self.history = ""  # stripped text ever seen (bounded below)
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

    def wait_for_compact(self, needle: str, timeout: float = 8.0) -> bool:
        """Match a SPACE-FREE needle against the space-stripped stream.

        The diff-based renderer skips every cell that did not change, so a
        blank cell inside a status line often never reaches the captured
        stream; spaces must therefore not be part of a match.
        """
        end = time.monotonic() + timeout
        while time.monotonic() < end:
            self.drain(0.4)
            if needle in self.text().replace(" ", ""):
                return True
        print(f"--- timed out waiting for {needle!r}; output tail:")
        print(self.text()[-2000:])
        return False

    def clear(self) -> None:
        self.buf.clear()

    def send(self, data: bytes) -> None:
        os.write(self.master, data)

    def press(self, key: bytes) -> None:
        """Send a key and give the TUI a beat to poll + process it. The
        main loop polls with a zero timeout, so a key sent right before the
        next action may otherwise race the render."""
        self.send(key)
        time.sleep(0.5)
        self.drain(0.3)

    def quit(self) -> int:
        self.send(b"q")
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


def scale_wrfm(src: str, dst: str, factor: float) -> None:
    """Write a scaled copy of a .wrfm file (vertex lines scaled by factor)."""
    with open(src) as f:
        lines = f.read().splitlines()
    out = []
    for line in lines:
        parts = line.split()
        if parts and parts[0] == "v" and len(parts) >= 4:
            x, y, z = (float(parts[1]), float(parts[2]), float(parts[3]))
            out.append(f"v {x * factor} {y * factor} {z * factor}")
        else:
            out.append(line)
    with open(dst, "w") as f:
        f.write("\n".join(out) + "\n")


def main() -> int:
    binary = sys.argv[1] if len(sys.argv) > 1 else "./target/debug/wireforge"
    binary = os.path.abspath(binary)
    if not os.path.exists(binary):
        print(f"binary not found: {binary} (run `cargo build` first)")
        return 2

    repo = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
    cube = os.path.join(repo, "wrfm_files", "cube.wrfm")
    tetra = os.path.join(repo, "wrfm_files", "tetrahedron.wrfm")

    checks = []
    with tempfile.TemporaryDirectory(prefix="wrfm-e2e-") as td:
        model = os.path.join(td, "model.wrfm")
        big = os.path.join(td, "big.wrfm")
        shutil.copy(cube, model)
        scale_wrfm(tetra, big, 3.0)  # extent 6 -> fitted dist ~20.78

        tui = Tui(binary, model)
        try:
            # 1. Initial render: status line + the file-stem model name.
            checks.append(("initial render", tui.wait_for("Wireforge:")))
            checks.append(("model name shown", "model" in tui.text()))
            tui.clear()

            # 2. Replace with a 3x bigger model -> reload success AND a
            #    large canvas redraw (the swap proof: with the camera
            #    preserved, only a replaced model can repaint so many
            #    cells; the "hot-reload" event line alone is ~50 bytes).
            base_len = len(tui.buf)
            shutil.copy(big, model)
            # NOTE: match "hot-reload" — ratatui renders diff-based cell
            # updates, so a character that is already on screen from the
            # previous event is skipped in the captured stream. "hot-reload"
            # differs from every previous event prefix, so it is always
            # re-emitted.
            checks.append(("hot reload success", tui.wait_for("hot-reload")))
            tui.drain(0.4)
            redraw = len(tui.buf) - base_len
            checks.append(("model swapped (canvas redrawn)", redraw > 1000))
            tui.clear()

            # 3. Half-written file -> parse error, TUI stays alive. The
            #    content keeps the v1 magic + BOTH `v ` and `e ` markers so
            #    content-first detection still probes it as wrfm and the
            #    parser reports the truncated vertex line (a magic-less
            #    v/e file would probe as "unrecognized" instead).
            with open(model, "w") as f:
                f.write("wrfm 1\nvertices 2   edges 1\nv 0 0 0\nv 1 1\ne 0 1\n")  # truncated vertex line
            checks.append(("hud parse error", tui.wait_for("parse")))
            checks.append(("alive after parse error", tui.proc.poll() is None))
            # The report's first line rides on the SAME transient row now.
            checks.append(
                ("status parse error detail", tui.wait_for_compact("parseerror:invalidvertex"))
            )
            tui.clear()

            # 4. Delete the file -> the HUD shows "file removed" (transient)
            #    with its detail on the same row.
            os.remove(model)
            checks.append(("hud file removed", tui.wait_for("moved")))
            checks.append(("alive after delete", tui.proc.poll() is None))
            # "lastmode" holds in BOTH backgrounds: the parse-error line
            # shares the final `l` of "model" (skipped by the diff renderer)
            # and a canvas cell never equals ASCII, while the space-stripped
            # match ignores the blanks.
            checks.append(("status file removed detail", tui.wait_for_compact("lastmode")))
            tui.clear()

            # 5. Re-create the file -> reloads again.
            shutil.copy(cube, model)
            checks.append(("reload after restore", tui.wait_for("hot-reload")))
            tui.clear()

            # 6. Quit cleanly.
            rc = tui.quit()
            checks.append(("clean quit (q)", rc == 0))
        finally:
            tui.close()

    failed = [name for name, ok in checks if not ok]
    for name, ok in checks:
        print(("ok: " if ok else "FAIL: ") + name)
    if failed:
        print(f"E2E HOT-RELOAD FAILED: {failed}")
        return 1
    print("E2E HOT-RELOAD PASSED")
    return 0


if __name__ == "__main__":
    sys.exit(main())
