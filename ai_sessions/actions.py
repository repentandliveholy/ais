"""Small desktop side effects: clipboard and revealing a folder.

Every helper is best-effort and never raises into the TUI loop.
"""
from __future__ import annotations

import base64
import os
import shutil
import subprocess
import sys

CLIPBOARD_COMMANDS = (
    ("pbcopy",),
    ("wl-copy",),
    ("xclip", "-selection", "clipboard"),
    ("xsel", "--clipboard", "--input"),
    ("clip.exe",),
)


def copy_text(text: str) -> str:
    """Copy text; return the method used ('' when nothing worked)."""
    for command in CLIPBOARD_COMMANDS:
        binary = shutil.which(command[0])
        if not binary:
            continue
        if command[0] == "wl-copy" and not os.environ.get("WAYLAND_DISPLAY"):
            continue
        if command[0] in ("xclip", "xsel") and not os.environ.get("DISPLAY"):
            continue
        try:
            subprocess.run([binary, *command[1:]], input=text.encode(), check=True,
                           stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=3)
            return command[0]
        except (OSError, subprocess.SubprocessError):
            continue
    if sys.stdout.isatty():
        # OSC 52 works over SSH and inside most modern terminals.
        payload = base64.b64encode(text.encode()).decode()
        sys.stdout.write(f"\x1b]52;c;{payload}\x07")
        sys.stdout.flush()
        return "osc52"
    return ""


def reveal(path: str) -> str:
    """Open a folder in the system file manager; return the opener's name."""
    if not path or not os.path.isdir(path):
        raise ValueError(path)
    for opener in (("open",) if sys.platform == "darwin" else ("xdg-open", "gio")):
        binary = shutil.which(opener)
        if not binary:
            continue
        argv = [binary, "open", path] if opener == "gio" else [binary, path]
        subprocess.Popen(argv, stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
                         stderr=subprocess.DEVNULL, start_new_session=True)
        return opener
    raise FileNotFoundError("open / xdg-open")
