"""Read Claude's built-in interactive /usage command without a model turn.

The official print command returns session costs instead of plan limits. A
private PTY keeps the supported interactive command available while preventing
its terminal controls from reaching the ais UI. User customizations are disabled.
"""
from __future__ import annotations

import errno
import fcntl
import os
import pty
import re
import select
import signal
import struct
import subprocess
import termios
import threading
import time
from concurrent.futures import CancelledError

from .i18n import tr
from .model import clean


# Read only complete native Usage frames. Session token/cost summaries and user
# status-line output must never masquerade as subscription quota.
_USAGE_HEADER = re.compile(r"Settings\s+Status\s+Config\s+Usage\s+Stats")


def native_usage_windows(screen: str) -> list[dict]:
    """Read only native quota sections, choosing the last redraw of each one.

    Verified with Claude Code 2.1.325's screen-reader Usage pane. The separate
    local-session attribution percentages below it are not account limits.
    """
    frames = _USAGE_HEADER.split(screen)
    if len(frames) < 2 or "Esc to cancel" not in frames[-1]:
        return []
    frame = frames[-1]
    windows = {}
    pattern = (r"(?P<label>Current session|Current week(?:[ \t]*\([^\n)]+\))?)\n"
               r"(?P<progress>[^\n]*)\n(?P<reset>Resets[^\n]+)")
    for match in re.finditer(pattern, frame, re.I):
        percent = re.search(r"(\d+(?:\.\d+)?)%\s+used\b", match["progress"], re.I)
        if not percent or not 0 <= float(percent[1]) <= 100:
            continue
        label = match["label"]
        windows[label.casefold()] = {"label": label, "used_percent": float(percent[1]),
                                      "reset_text": match["reset"]}
    return list(windows.values())


def _refreshing(screen: str) -> bool:
    # screen-reader redraws append changed rows without repeating the header.
    # A later completed footer supersedes the initial Refreshing marker.
    pieces = screen.rsplit("Esc to cancel", 2)
    latest = pieces[-2] if len(pieces) > 1 else screen
    return "Refreshing" in latest


def read_claude_usage(binary, env, cwd, cancel: threading.Event, timeout: float) -> list[dict]:
    if cancel.is_set():
        raise CancelledError()
    master, slave = pty.openpty()
    process = None
    try:
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 70, 180, 0, 0))
        command = [binary, "--safe-mode", "--ax-screen-reader", "--tools", ""]
        process = subprocess.Popen(command, stdin=slave, stdout=slave, stderr=slave,
                                   env={**env, "TERM": "xterm-256color"}, cwd=cwd,
                                   start_new_session=True)
        os.close(slave)
        slave = None
        deadline = time.monotonic() + timeout
        raw, sent, last_change, report = b"", False, time.monotonic(), None
        while True:
            if cancel.is_set():
                raise CancelledError()
            now = time.monotonic()
            # Wait for the final redraw/network update to settle before taking
            # a snapshot. Never expose a partial loading frame as live usage.
            if report and now - last_change >= .35:
                return report
            if now >= deadline:
                if sent:
                    raise ValueError(tr('Claude /usage did not provide plan limits'))
                raise TimeoutError(tr('CLI request timed out'))
            if not select.select([master], [], [], min(.1, deadline - now))[0]:
                continue
            try:
                chunk = os.read(master, 65536)
            except OSError as exc:
                if exc.errno != errno.EIO:
                    raise
                chunk = b""
            if not chunk:
                if report:
                    return report
                code = process.poll()
                if code:
                    raise ValueError(tr('CLI exited with code {code}', code=code))
                raise ValueError(tr('Claude /usage did not provide plan limits'))
            last_change = time.monotonic()
            raw += chunk
            if len(raw) > 2 * 1024 * 1024:
                raise ValueError(tr('CLI response too large'))
            screen = clean(raw.decode("utf-8", errors="replace"))
            if re.search(r"Not logged in|Please (?:run /login|log in)|Login required", screen, re.I):
                raise ValueError(tr('Claude CLI cannot see a login in this environment; check claude auth status in the same terminal'))
            if re.search(r"Enter y/n:|Do you trust|Is this a project you created or one you trust", screen, re.I):
                raise ValueError(tr('Claude requires workspace trust; open claude in this directory first'))
            if not sent and re.search(r"(?m)^(?:\$|❯|>)\s*$", screen):
                # The only input ever submitted is a documented built-in UI
                # command. No initial prompt, inference, login or auto-approval.
                os.write(master, b"/usage\r")
                raw, sent = b"", True
                continue
            if sent:
                if re.search(r"failed to (?:fetch|load).*usage|error fetching usage|usage.*(?:unauthorized|forbidden)", screen, re.I):
                    raise ValueError(tr('Claude could not load plan limits; check /usage in the official CLI'))
                report = native_usage_windows(screen) if not _refreshing(screen) else None
    finally:
        if process is not None:
            try:
                os.killpg(process.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            process.wait()
        os.close(master)
        if slave is not None:
            os.close(slave)
