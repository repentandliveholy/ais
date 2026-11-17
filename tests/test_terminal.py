"""Exercise a real PTY, including terminal restoration and shell-free dispatch."""
import errno
import curses
import fcntl
import json
import os
import pty
import select
import signal
import struct
import subprocess
import sys
import tempfile
import termios
import time
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]


class TerminalTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.root = Path(self.tmp.name).resolve()
        self.cwd = self.root / "project with spaces"
        self.cwd.mkdir()
        folder = self.root / "home/.omp/agent/sessions/project"
        folder.mkdir(parents=True)
        self.session = folder / "session.jsonl"
        records = [
            {"type": "session", "id": "test-session-id", "cwd": str(self.cwd), "title": "PTY test session"},
            {"type": "message", "message": {"role": "user", "content": "test prompt"}},
            {"type": "message", "message": {"role": "assistant", "content": [{"type": "text", "text": "done"}]}},
        ]
        self.session.write_text("".join(json.dumps(r) + "\n" for r in records))
        bindir = self.root / "bin"
        bindir.mkdir()
        binary = bindir / "omp"
        self.binary = binary
        self.client_script = "#!" + sys.executable + "\nimport os,sys,json,termios\na=termios.tcgetattr(0)\nprint('DISPATCH='+json.dumps({'argv':sys.argv[1:],'cwd':os.getcwd(),'tty':os.isatty(0),'echo':bool(a[3]&termios.ECHO),'canonical':bool(a[3]&termios.ICANON)}),flush=True)\n"
        binary.write_text(self.client_script)
        binary.chmod(0o755)
        # Usage tests must never reach a real provider from the inherited PATH.
        for client in ('codex', 'agy', 'claude'):
            stub = bindir / client
            stub.write_text('#!' + sys.executable + '\nimport sys\nsys.exit(1)\n')
            stub.chmod(0o755)
        self.master, self.slave = pty.openpty()
        fcntl.ioctl(self.slave, termios.TIOCSWINSZ, struct.pack("HHHH", 44, 144, 0, 0))
        env = {**os.environ, "TERM": "xterm-256color", "XDG_CONFIG_HOME": str(self.root / "config"), "PATH": str(bindir) + os.pathsep + os.environ["PATH"]}
        for key in ("PI_CODING_AGENT_DIR", "CLAUDE_CONFIG_DIR", "CODEX_HOME"):
            env.pop(key, None)
        self.command = [sys.executable, str(ROOT / "ais"), "--sessions", "--same-tab", "--home", str(self.root / "home"), "--no-cache", "-C", str(self.cwd)]
        self.env = env
        self.start()

    def start(self, *extra):
        self.process = subprocess.Popen([*self.command, *extra], stdin=self.slave, stdout=self.slave, stderr=self.slave, env=self.env)
        self.output = b""

    def tearDown(self):
        if self.process.poll() is None:
            self.process.terminate()
            try:
                self.process.wait(timeout=1)
            except subprocess.TimeoutExpired:
                self.process.kill()
                self.process.wait(timeout=5)
        os.close(self.master)
        os.close(self.slave)
        self.tmp.cleanup()

    def until(self, needle, timeout=6):
        needle = needle.encode() if isinstance(needle, str) else needle
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if needle in self.output:
                return self.output
            if select.select([self.master], [], [], .05)[0]:
                try:
                    chunk = os.read(self.master, 65536)
                except OSError as exc:
                    if exc.errno == errno.EIO:
                        break
                    raise
                if not chunk:
                    break
                self.output += chunk
        self.fail(f"PTY did not show {needle!r}; tail={self.output[-2000:]!r}")

    def send(self, value):
        self.output = b""
        pending = value.encode()
        blocking = os.get_blocking(self.master)
        os.set_blocking(self.master, False)
        deadline = time.monotonic() + 6
        try:
            while pending and time.monotonic() < deadline:
                readable, writable, _ = select.select([self.master], [self.master], [], .05)
                if readable:
                    self.output += os.read(self.master, 65536)
                if writable:
                    try:
                        pending = pending[os.write(self.master, pending):]
                    except BlockingIOError:
                        pass
            self.assertFalse(pending, "PTY input stopped accepting keys")
        finally:
            os.set_blocking(self.master, blocking)

    def exit_code(self, timeout=6):
        # A PTY has a small output buffer. Keep draining screen repaints while
        # waiting, just as a real terminal does, or the child blocks in write().
        deadline = time.monotonic() + timeout
        while self.process.poll() is None and time.monotonic() < deadline:
            if select.select([self.master], [], [], .05)[0]:
                try:
                    self.output += os.read(self.master, 65536)
                except OSError as exc:
                    if exc.errno != errno.EIO:
                        raise
        return self.process.wait(timeout=1)

    def test_enter_launches_exact_session_and_restores_terminal(self):
        initial = self.until("PTY test session")
        self.assertIn(b"\x1b[38;5;", initial)
        # Unbound typing must not activate a search or prevent resume.
        self.send("xzwж\n")
        output = self.until("DISPATCH=")
        # Drain to the newline containing the dispatch record.
        for _ in range(10):
            if b"\r\n" in output.split(b"DISPATCH=", 1)[1]:
                break
            if select.select([self.master], [], [], .1)[0]:
                output += os.read(self.master, 65536)
        payload = json.loads(output.split(b"DISPATCH=", 1)[1].splitlines()[0])
        self.assertEqual(payload["argv"], ["--resume", str(self.session)])
        self.assertEqual(payload["cwd"], str(self.cwd))
        self.assertTrue(payload["tty"] and payload["echo"] and payload["canonical"])
        self.assertIn(b"\x1b[?1049l", output)
        self.until("Returned from OMP · exit code 0")
        self.assertIsNone(self.process.poll())
        self.send("q")
        self.assertEqual(self.exit_code(), 0)

    def test_pin_digit_filter_and_double_click_launch(self):
        self.until("PTY test session")
        self.send("f")
        self.until("Pinned: PTY test session")
        settings = json.loads((self.root / "config/ai-sessions/settings.json").read_text())
        self.assertEqual(len(settings["pins"]), 1)
        self.send("2")  # Claude only: the OMP session disappears.
        self.until("No matching sessions yet")
        self.send("0")
        self.until("PTY test session")
        click = "\x1b[<0;10;13M\x1b[<0;10;13m"
        self.send(click + click)  # Double-click the first row (1-based SGR cell).
        self.until("DISPATCH=")
        self.until("Returned from OMP · exit code 0")
        self.send("q")
        self.assertEqual(self.exit_code(), 0)

    def test_projects_is_default_and_backspace_returns_from_folder(self):
        self.send("q")
        self.assertEqual(self.exit_code(), 0)
        self.command = [part for part in self.command if part != "--sessions"]
        self.start()
        self.until("1 sessions")
        self.send("\n")
        self.until("PTY test session")
        self.send("\x7f")
        self.until("PROJECTS")
        self.send("q")
        self.assertEqual(self.exit_code(), 0)

    def test_projects_folder_drill_and_backspace(self):
        self.send("q")
        self.assertEqual(self.exit_code(), 0)
        sub = self.cwd / "sub"
        sub.mkdir()
        child = self.session.parent / "child.jsonl"
        child.write_text(json.dumps({"type": "session", "id": "child-session-id",
                                    "cwd": str(sub), "title": "CHILD SESSION"}) + "\n")
        os.utime(child, (500, 500))
        self.command = [part for part in self.command if part != "--sessions"]
        self.start()
        self.until("1 sessions")
        self.send("\n")  # enter the parent folder
        self.until("PTY test session")  # folder view lists its own session
        self.send("\n")  # enter the child project
        self.until("CHILD SESSION")
        self.send("\x7f")  # back to the parent folder
        self.until("PTY test session")
        self.send("\x7f")  # back to the projects root
        self.until("project with spaces")
        self.send("q")
        self.assertEqual(self.exit_code(), 0)

    def test_no_return_keeps_direct_exec_and_exit_code(self):
        self.until("PTY test session")
        self.send("q")
        self.assertEqual(self.exit_code(), 0)
        self.binary.write_text(self.client_script + "raise SystemExit(7)\n")
        self.start("--no-return")
        self.until("PTY test session")
        self.send("\n")
        self.until("DISPATCH=")
        self.assertEqual(self.exit_code(), 7)

    def test_client_error_returns_to_launcher(self):
        self.binary.write_text(self.client_script + "raise SystemExit(7)\n")
        self.until("PTY test session")
        self.send("\n")
        self.until("Returned from OMP · exit code 7")
        self.assertIsNone(self.process.poll())
        self.send("q")
        self.assertEqual(self.exit_code(), 0)

    def test_raw_terminal_after_signal_is_restored(self):
        self.binary.write_text(self.client_script + "import signal,tty\ntty.setraw(0)\nos.kill(os.getpid(),signal.SIGTERM)\n")
        self.until("PTY test session")
        self.send("\n")
        self.until("OMP terminated by signal SIGTERM")
        self.send("q")
        self.assertEqual(self.exit_code(), 0)
        attrs = termios.tcgetattr(self.slave)
        self.assertTrue(attrs[3] & termios.ECHO)
        self.assertTrue(attrs[3] & termios.ICANON)

    def test_parent_ignores_interrupt_while_client_can_continue(self):
        self.binary.write_text(self.client_script + "import signal\nassert signal.getsignal(signal.SIGINT) != signal.SIG_IGN\nos.kill(os.getppid(),signal.SIGINT)\nprint('CLIENT_WAITING',flush=True)\nassert input() == '/exit'\n")
        self.until("PTY test session")
        self.send("\n")
        self.until("CLIENT_WAITING")
        self.assertIsNone(self.process.poll())
        self.send("/exit\n")
        self.until("Returned from OMP · exit code 0")
        self.send("q")
        self.assertEqual(self.exit_code(), 0)

    def test_return_reloads_history_and_preserves_session_identity(self):
        other = self.session.parent / "other.jsonl"
        other_cwd = self.root / "another project"
        other_cwd.mkdir()
        other.write_text(json.dumps({"type":"session", "id":"other-id", "cwd":str(other_cwd), "title":"Other PTY session"}) + "\n")
        os.utime(self.session, (2000, 2000))
        os.utime(other, (1000, 1000))
        self.binary.write_text(self.client_script + "\nif '--resume' in sys.argv:\n p=sys.argv[sys.argv.index('--resume')+1]\n with open(p,'a') as f: f.write(json.dumps({'type':'title','title':'UPDATED PTY SESSION'})+'\\n')\n")
        self.until("PTY test session")
        self.send("\t")
        self.until("Other PTY session")
        self.send("\x1bOC")  # filter: all -> OMP
        self.until("OMP")
        self.send("/PTY")
        self.until("Y▏")
        self.send("\n")  # finish search
        self.send("\x1bOB")  # select older session; it becomes newest on exit
        self.until("Other PTY session")
        self.send("\n")
        self.until("Returned from OMP · exit code 0")
        self.until("UPDATED PTY SESSION")
        self.assertIn(b"UPDATED PTY SESSION", other.read_bytes())
        self.assertNotIn(b"UPDATED PTY SESSION", self.session.read_bytes())
        self.assertIn("⌕ PTY".encode(), self.output)
        self.send("\n")
        output = self.until("DISPATCH=")
        self.assertIn(str(other).encode(), output)
        self.assertIn(str(other_cwd).encode(), output)
        self.until("Returned from OMP · exit code 0")
        self.send("q")
        self.assertEqual(self.exit_code(), 0)

    def test_new_session_returns_without_reopening_new_modal(self):
        self.until("PTY test session")
        self.send("n")
        self.until("NEW SESSION")
        self.send("\n")
        self.until("Returned from OMP · exit code 0")
        self.until("ID test-session-id")
        self.send("\n")
        output = self.until("DISPATCH=")
        self.assertIn(b"--resume", output)
        self.until("Returned from OMP · exit code 0")
        self.send("q")
        self.assertEqual(self.exit_code(), 0)

    def test_usage_search_resize_and_return(self):
        self.until("PTY test session")
        self.send("u")
        self.until("PROVIDER LIMITS")
        self.send("s")
        self.until("PTY test session")
        self.send("/missingxyz")
        self.until("No matching sessions yet")
        self.send("\x1b")
        self.until("PTY test session")
        fcntl.ioctl(self.slave, termios.TIOCSWINSZ, struct.pack("HHHH", 24, 80, 0, 0))
        os.kill(self.process.pid, __import__("signal").SIGWINCH)
        self.send("?")
        self.until("CONTROLS")
        self.send("\x1b")
        self.until("PTY test session")
        self.send("q")
        self.assertEqual(self.exit_code(), 0)

    def test_escape_keeps_app_open_dot_searches_and_q_quits_from_modal(self):
        self.until("PTY test session")
        self.send("\x1b")
        self.assertIsNone(self.process.poll())
        self.send(".missing.value")
        self.until("No matching sessions yet")
        self.send("\x1b")
        self.until("PTY test session")
        self.send("u")
        self.until("PROVIDER LIMITS")
        self.send("l")
        self.until("LANGUAGE")
        self.send("\x1b\x1b")  # Close the picker, then return from Usage.
        self.until("PTY test session")
        self.send("?")
        self.until("CONTROLS")
        self.send("q")
        self.assertEqual(self.exit_code(), 0)

    def test_rr_restarts_process_and_returns_to_initial_screen(self):
        self.until("PTY test session")
        self.send("u")
        self.until("PROVIDER LIMITS")
        pid = self.process.pid
        self.send("rr")
        output = self.until("PTY test session")
        self.assertIn(b"\x1b[?1049l", output)
        self.assertIn(b"\x1b[?1049h", output)
        self.assertEqual(self.process.pid, pid)
        self.assertIsNone(self.process.poll())
        self.send("q")
        self.assertEqual(self.exit_code(), 0)

    def test_usage_o_cannot_launch_omp_report(self):
        marker = self.root / 'omp-usage-called'
        self.binary.write_text('#!' + sys.executable + "\nfrom pathlib import Path\nPath(%r).touch()\n" % str(marker))
        self.until("PTY test session")
        self.send("u")
        self.until("PROVIDER LIMITS")
        self.send("o")
        time.sleep(.2)
        self.assertFalse(marker.exists())
        self.send("q")
        self.assertEqual(self.exit_code(), 0)

    def test_usage_fetches_official_codex_automatically_and_refreshes(self):
        payload = {"rateLimits": {"limitName": "FRESH_PROVIDER_USAGE", "primary": {
            "usedPercent": 22, "windowDurationMins": 300}}}
        counter = self.root / "usage-calls"
        binary = self.root / 'bin/codex'
        binary.write_text('#!' + sys.executable + "\nimport sys,json\nfrom pathlib import Path\n"
            "assert sys.argv[1:]==['app-server']\n"
            "first=json.loads(sys.stdin.readline())\n"
            "print(json.dumps({'id':first['id'],'result':{}}),flush=True)\n"
            "assert json.loads(sys.stdin.readline())['method']=='initialized'\n"
            "req=json.loads(sys.stdin.readline())\n"
            "assert req['method']=='account/rateLimits/read'\n"
            "p=Path(%r)\np.write_text(p.read_text()+'x' if p.exists() else 'x')\n"
            "print(json.dumps({'id':req['id'],'result':%r}),flush=True)\nsys.stdin.read()\n" % (str(counter), payload))
        self.until("PTY test session")
        self.send("u")
        self.until("FRESH_PROVIDER_USAGE")
        self.until("Codex CLI · live")
        self.assertEqual(counter.read_text(), "x")
        self.send("r")
        deadline = time.monotonic() + 3
        while counter.read_text() != "xx" and time.monotonic() < deadline:
            if select.select([self.master], [], [], .05)[0]:
                self.output += os.read(self.master, 65536)
        self.assertEqual(counter.read_text(), "xx")
        self.send("q")
        self.assertEqual(self.exit_code(), 0)

    def test_history_refreshes_changed_file_without_keyboard_input(self):
        self.until("PTY test session")
        self.send("q")
        self.assertEqual(self.exit_code(), 0)
        self.command = [sys.executable, "-c",
                        "import ai_sessions.ui as ui; ui.HISTORY_REFRESH_SECONDS=.3; "
                        "from ai_sessions.cli import entrypoint; entrypoint()",
                        *self.command[2:]]
        self.start()
        self.until("PTY test session")
        # Let the full-detail cache populate before appending a new response.
        deadline = time.monotonic() + .6
        while time.monotonic() < deadline:
            if select.select([self.master], [], [], .05)[0]:
                self.output += os.read(self.master, 65536)
        with self.session.open("a") as stream:
            stream.write(json.dumps({"type": "message", "message": {
                "role": "assistant", "content": "AUTO_REFRESH_NEW_RESPONSE"}}) + "\n")
        self.until("AUTO_REFRESH_NEW_RESPONSE")
        self.send("q")
        self.assertEqual(self.exit_code(), 0)

    def test_trackpad_burst_is_limited_without_backlog_and_keyboard_stays_fast(self):
        payload = {"rateLimitsByLimitId": {f"{i:03}": {
            "limitName": f"WHEEL_LIMIT_{i:03}",
            "primary": {"usedPercent": 22, "windowDurationMins": 300}}
            for i in range(60)}}
        (self.root/'bin/codex').write_text('#!' + sys.executable + "\nimport sys,json\n"
            "first=json.loads(sys.stdin.readline())\n"
            "print(json.dumps({'id':first['id'],'result':{}}),flush=True)\n"
            "sys.stdin.readline()\nreq=json.loads(sys.stdin.readline())\n"
            "print(json.dumps({'id':req['id'],'result':%r}),flush=True)\nsys.stdin.read()\n" % payload)
        self.until("PTY test session")
        self.send("u")
        self.until("WHEEL_LIMIT_000")

        def wheel(down):
            if hasattr(curses, "BUTTON5_PRESSED"):
                return f"\x1b[<{65 if down else 64};10;15M"
            return "\x1b[M" + chr(97 if down else 96) + chr(42) + chr(47)

        self.send(wheel(True) * 500)
        # After consuming a trackpad burst, output must settle on its own.
        deadline = time.monotonic() + .5
        while time.monotonic() < deadline:
            if select.select([self.master], [], [], .05)[0]:
                self.output += os.read(self.master, 65536)
        self.assertFalse(select.select([self.master], [], [], .25)[0], "wheel output kept scrolling")
        self.assertLess(len(self.output), 200000)
        self.assertNotIn(b"WHEEL_LIMIT_059", self.output, "one flick jumped to the end")
        self.send("\x1bOF")  # End still jumps immediately.
        self.until("59")  # curses may redraw only the changed numeric suffix.
        self.send(wheel(False) * 500)
        deadline = time.monotonic() + .5
        while time.monotonic() < deadline:
            if select.select([self.master], [], [], .05)[0]:
                self.output += os.read(self.master, 65536)
        self.assertNotIn(b"WHEEL_LIMIT_000", self.output, "one reverse flick jumped to the start")
        self.send("\x1bOH")
        self.until("00")
        self.send("q")
        self.assertEqual(self.exit_code(), 0)

    def test_sgr_and_emulated_arrows_cannot_jump_300_sessions_or_stick_at_edges(self):
        self.until("PTY test session")
        self.send("q")
        self.assertEqual(self.exit_code(), 0)
        for i in range(1, 321):
            path = self.session.parent / f"extra-{i:03}.jsonl"
            path.write_text(json.dumps({"type": "session", "id": f"extra-{i:03}",
                                       "cwd": str(self.cwd), "title": f"Session {i:03}"}) + "\n")
            updated = self.session.stat().st_mtime - i
            os.utime(path, (updated, updated))
        states = self.root / "positions.jsonl"
        # Observe the real TUI's selection while retaining its actual curses
        # input, drawing and terminal modes (including old macOS ncurses).
        script = ("import ai_sessions.ui as ui, json\n"
                  "from pathlib import Path\n"
                  f"positions = Path({str(states)!r})\n"
                  "original_render = ui.render\n"
                  "def observe(sessions, view, *args, **kwargs):\n"
                  "    screen = original_render(sessions, view, *args, **kwargs)\n"
                  "    if view.loaded:\n"
                  "        with positions.open('a') as stream:\n"
                  "            stream.write(json.dumps(view.selected)+'\\n')\n"
                  "    return screen\n"
                  "ui.render = observe\n"
                  "from ai_sessions.cli import entrypoint\n"
                  "entrypoint()\n")
        self.command = [sys.executable, "-c", script, *self.command[2:]]
        def wait_position(expected):
            deadline = time.monotonic() + 2
            while time.monotonic() < deadline:
                if states.exists() and states.read_text().splitlines()[-1:] == [str(expected)]:
                    return
                if select.select([self.master], [], [], .01)[0]:
                    self.output += os.read(self.master, 65536)
            self.fail(f"expected position {expected}; observed {states.read_text()}")

        def drain_output(duration):
            deadline = time.monotonic() + duration
            while time.monotonic() < deadline:
                if select.select([self.master], [], [], .01)[0]:
                    self.output += os.read(self.master, 65536)

        for down, up in (("\x1b[<65;10;15M", "\x1b[<64;10;15M"),
                         ("\x1bOB", "\x1bOA")):
            with self.subTest(down=down):
                states.unlink(missing_ok=True)
                self.start("--no-auto-refresh")
                initial = self.until("PTY test session")
                self.assertIn(b"\x1b[?1006h", initial)
                self.assertNotIn(b"\x1b[?1006l", initial)
                self.send(down * 300)
                wait_position(1)
                self.send(up * 300)
                wait_position(0)
                self.send(up * 300 + down)
                wait_position(1)
                self.send("\x1bOF")
                wait_position(320)
                self.send(down * 300 + up)
                wait_position(319)
                observed = [int(value) for value in states.read_text().splitlines()]
                self.assertTrue(set(observed) <= {0, 1, 319, 320}, observed)
                self.send("\x1bOH")
                wait_position(0)
                # Sustained input must keep moving instead of extending a
                # debounce lock forever. Stopping input must stop movement.
                deadline = time.monotonic() + .8
                while time.monotonic() < deadline:
                    self.send(down * 3)
                    drain_output(.02)
                drain_output(.2)
                stopped = int(states.read_text().splitlines()[-1])
                self.assertGreaterEqual(stopped, 3, "continuous scroll locked after one move")
                self.assertLessEqual(stopped, 26, "scroll speed was not limited")
                drain_output(.25)
                self.assertEqual(int(states.read_text().splitlines()[-1]), stopped,
                                 "old scroll reports kept replaying after input stopped")
                self.send("q")
                self.assertEqual(self.exit_code(), 0)

    def test_language_picker_works_in_usage_and_persists_after_restart(self):
        self.until("PTY test session")
        self.send("u")
        self.until("PROVIDER LIMITS")
        self.send("l")
        self.until("LANGUAGE")
        self.send("\x1bOB\n")  # application-mode Down, then Enter
        self.until("ЛИМИТЫ ПРОВАЙДЕРОВ")
        path = self.root / "config/ai-sessions/settings.json"
        self.assertEqual(json.loads(path.read_text())["language"], "ru")
        self.send("q")
        self.assertEqual(self.exit_code(), 0)
        self.start()
        self.until("Текущий каталог")
        self.send("l")
        self.until("ЯЗЫК")
        self.send("\x1bOA\n")
        self.until("Current directory")
        self.assertEqual(json.loads(path.read_text())["language"], "en")
        self.send("q")
        self.assertEqual(self.exit_code(), 0)


if __name__ == "__main__":
    unittest.main()
