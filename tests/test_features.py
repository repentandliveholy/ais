import curses
import json
import os
import subprocess
import sys
import tempfile
import time
import unittest
from pathlib import Path
from unittest.mock import patch

from ai_sessions.cli import demo_sessions
from ai_sessions.i18n import read_settings, save_settings, set_language
from ai_sessions.model import Session, filter_sessions, parse_query
from ai_sessions.screen import Screen
from ai_sessions.ui import Click, TerminalInput, View, copy_payload, help_lines, render, visible_entries, draw_modal


ROOT = Path(__file__).resolve().parents[1]


def session(client="omp", sid="s", **values):
    values.setdefault("cwd", "/tmp")
    values.setdefault("updated", time.time())
    return Session(client, sid, **values)


class QueryTests(unittest.TestCase):
    def test_fields_negation_and_unknown_prefix(self):
        parsed = parse_query('auth -flaky client:claude -status:error "two words" http://host')
        self.assertEqual(parsed.words, ["auth", "two words", "http://host"])
        self.assertEqual(parsed.excluded, ["flaky"])
        self.assertEqual(parsed.fields, [("client", "claude", False), ("status", "error", True)])

    def test_filters_by_field_values(self):
        now = time.time()
        rows = [
            session("claude", "a", title="auth flow", model="claude-opus", status="error", branch="main", updated=now),
            session("codex", "b", title="auth tests", model="gpt", status="ready", branch="dev", updated=now - 10 * 86400),
            session("omp", "c", title="flaky auth", model="gpt", status="ready", cwd="/srv/api", updated=now - 3600),
        ]
        ids = lambda q, **kw: [s.id for s in filter_sessions(rows, "/tmp", "all", query=q, **kw)]
        self.assertEqual(ids("client:claude"), ["a"])
        self.assertEqual(ids("c:cod"), ["b"])
        self.assertEqual(ids("model:gpt -flaky"), ["b"])
        self.assertEqual(ids("-status:error"), ["c", "b"])
        self.assertEqual(ids("since:2d"), ["a", "c"])
        self.assertEqual(ids("branch:dev"), ["b"])
        self.assertEqual(ids("dir:api"), ["c"])
        self.assertEqual(ids("is:pinned", pins={rows[1].key}), ["b"])
        self.assertEqual(ids("since:nonsense"), ["a", "c", "b"])  # Invalid durations never hide rows.

    def test_is_child_and_archived_reveal_hidden_rows(self):
        rows = [session(sid="parent"), session(sid="kid", parent="parent"), session(sid="old", archived=True)]
        self.assertEqual([s.id for s in filter_sessions(rows, "", "all", query="is:child")], ["kid"])
        self.assertEqual([s.id for s in filter_sessions(rows, "", "all", query="is:archived")], ["old"])


class PinTests(unittest.TestCase):
    def test_pinned_rows_come_first_and_keep_order(self):
        cwd = str(ROOT)
        rows = demo_sessions(cwd)
        view = View(cwd, Path.home(), loaded=True, pins={rows[4].key, rows[2].key})
        ordered = visible_entries(rows, view)
        self.assertEqual([s.key for s in ordered[:2]], [rows[2].key, rows[4].key])
        self.assertIn("★", render(rows, view, 144, 44).plain())

    def test_settings_merge_keeps_other_keys(self):
        with tempfile.TemporaryDirectory() as temp, patch.dict(os.environ, {"XDG_CONFIG_HOME": temp}):
            save_settings(language="ru")
            save_settings(pins=["a", "b"], new_client="codex")
            self.assertEqual(read_settings(), {"language": "ru", "pins": ["a", "b"], "new_client": "codex"})

    def test_copy_payload(self):
        s = session("claude", "abc", cwd="/tmp/my dir")
        self.assertEqual(copy_payload(s), "cd '/tmp/my dir' && claude --resume abc")
        self.assertEqual(copy_payload(s, identity=True), "abc")


class MouseTests(unittest.TestCase):
    def test_sgr_left_click_is_a_discrete_command(self):
        class Window:
            def __init__(self, text): self.keys = iter(text)
            def timeout(self, _): pass
            def get_wch(self):
                try:
                    return next(self.keys)
                except StopIteration:
                    raise curses.error()

        reader = TerminalInput(Window("\x1b[<0;5;7M\x1b[<0;5;7m\x1b[<2;1;1M\x1b[<32;3;3M"))
        self.assertEqual(reader.read(), (Click(4, 6), False))
        self.assertEqual(reader.read(), (None, True))  # Release.
        self.assertEqual(reader.read(), (None, True))  # Right button.
        self.assertEqual(reader.read(), (None, True))  # Drag motion.

    def test_render_maps_rows_tabs_and_clients_to_actions(self):
        cwd = str(ROOT)
        rows = demo_sessions(cwd)
        screen = render(rows, View(cwd, Path.home(), loaded=True), 144, 44)
        text = screen.plain().splitlines()
        y = next(i for i, row in enumerate(text) if "Fixing tests and build" in row)
        self.assertEqual(screen.hit_at(y, 10), ("row", 3))
        self.assertEqual(screen.hit_at(y + 2, 10), ("row", 3))
        tabs = next(i for i, row in enumerate(text) if "Usage [U]" in row)
        self.assertEqual(screen.hit_at(tabs, text[tabs].index("Usage")), ("tab", "usage"))
        chips = next(i for i, row in enumerate(text) if "Claude 1" in row)
        self.assertEqual(screen.hit_at(chips, text[chips].index("Claude")), ("client", "claude"))

    def test_modal_options_are_clickable_and_backdrop_closes(self):
        cwd = str(ROOT)
        view = View(cwd, Path.home(), loaded=True)
        view.modal = "new"
        screen = render(demo_sessions(cwd), view, 144, 44)
        text = screen.plain().splitlines()
        y = next(i for i, row in enumerate(text) if "3  Codex" in row)
        self.assertEqual(screen.hit_at(y, text[y].index("Codex")), ("new", 2))
        self.assertEqual(screen.hit_at(0, 0), ("close",))


class HelpTests(unittest.TestCase):
    def tearDown(self):
        set_language("en")

    def test_help_lists_new_keys_in_both_languages_and_fits(self):
        cwd = str(ROOT)
        for lang, needle in (("en", "pin / unpin"), ("ru", "закрепить / открепить")):
            set_language(lang)
            for cols, rows in ((144, 44), (80, 24)):
                view = View(cwd, Path.home(), loaded=True)
                view.modal = "help"
                text = render(demo_sessions(cwd), view, cols, rows).plain()
                self.assertIn("CONTROLS" if lang == "en" else "УПРАВЛЕНИЕ", text.upper())
                if rows == 44:
                    self.assertIn(needle, text)
                    self.assertIn("since:3d", text)
        self.assertTrue(help_lines(2) and help_lines(1))


class CommandLineTests(unittest.TestCase):
    def run_ais(self, *args, env=None):
        return subprocess.run([sys.executable, str(ROOT / "ais"), *args], capture_output=True, text=True,
                              env={**os.environ, "NO_COLOR": "1", **(env or {})})

    def test_last_print_command_uses_newest_session_in_cwd(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp).resolve()
            project = root / "project"
            project.mkdir()
            folder = root / "home/.omp/agent/sessions/p"
            folder.mkdir(parents=True)
            for name, stamp in (("old", 1000), ("new", 2000)):
                path = folder / f"{name}.jsonl"
                path.write_text(json.dumps({"type": "session", "id": f"{name}-id", "cwd": str(project), "title": name}) + "\n")
                os.utime(path, (stamp, stamp))
            common = ["--home", str(root / "home"), "--no-cache", "-C", str(project)]
            result = self.run_ais("--last", "--print-command", *common)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertIn(str(folder / "new.jsonl"), result.stdout)
            listed = self.run_ais("--list", "-n", "1", *common)
            self.assertIn("new", listed.stdout)
            self.assertNotIn("old-id", listed.stdout)
            self.assertNotIn("\x1b[", listed.stdout)
            stats = json.loads(self.run_ais("--stats", "--json", *common).stdout)
            self.assertEqual(stats["sessions"], 2)
            self.assertEqual(stats["clients"], {"omp": 2})
            empty = self.run_ais("--last", "--print-command", "--home", str(root / "home"), "--no-cache", "-C", str(root))
            self.assertEqual(empty.returncode, 1)
            self.assertIn("--all", empty.stderr)

    def test_stats_text_and_conflicting_flags(self):
        result = self.run_ais("--demo", "--stats")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("TOP PROJECTS", result.stdout)
        self.assertNotEqual(self.run_ais("--last", "--new", "omp").returncode, 0)


if __name__ == "__main__":
    unittest.main()
