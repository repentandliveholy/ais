import json
import os
import re
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from string import Formatter
from unittest.mock import patch

from ai_sessions.cli import demo_sessions
from ai_sessions.i18n import preferred_language, save_language, set_language, tr
from ai_sessions.locales import RU
from ai_sessions.screen import Screen
from ai_sessions.ui import View, render
from ai_sessions.usage import Limit, Usage, demo_usage, render_usage


ROOT = Path(__file__).resolve().parents[1]


class LanguageTests(unittest.TestCase):
    def tearDown(self):
        set_language("en")

    def test_preference_defaults_to_english_and_preserves_settings(self):
        with tempfile.TemporaryDirectory() as temp, patch.dict(os.environ, {"XDG_CONFIG_HOME": temp}):
            self.assertEqual(preferred_language(), "en")
            path = Path(temp) / "ai-sessions/settings.json"
            path.parent.mkdir()
            path.write_text('{"other": true}')
            save_language("ru")
            self.assertEqual(preferred_language(), "ru")
            self.assertTrue(json.loads(path.read_text())["other"])
            self.assertEqual(path.stat().st_mode & 0o777, 0o600)
            path.write_text('invalid')
            self.assertEqual(preferred_language(), "en")
            save_language("en")
            self.assertEqual(preferred_language(), "en")

    def test_english_and_russian_screens_keep_user_content(self):
        cwd = str(ROOT)
        for lang, heading, status in (("en", "DETAILS", "Response ready"),
                                     ("ru", "ПОДРОБНОСТИ", "Ответ готов")):
            set_language(lang)
            sessions = demo_sessions(cwd)
            sessions[0].title = "Пользовательский текст USER_TEXT"
            screen = render(sessions, View(cwd, Path.home(), loaded=True), 144, 44).plain()
            self.assertIn(heading, screen)
            self.assertIn(status, screen)
            self.assertIn("Пользовательский текст USER_TEXT", screen)
            usage = render_usage(Screen(144, 44), demo_usage()).plain()
            self.assertNotRegex(usage, r"Данные:|данные .* назад|Data:|data .* ago")
            stale = Usage([Limit("test", "#id", "test", "5h", .2, 0, 1, "test")], loaded=True)
            self.assertIn(tr(" · stale").strip(), render_usage(Screen(144, 44), stale).plain())

    def test_english_demo_has_no_untranslated_interface_strings(self):
        set_language("en")
        sessions = demo_sessions(str(ROOT))
        text = render(sessions, View(str(ROOT), Path.home(), loaded=True), 144, 60).plain()
        text += render_usage(Screen(144, 44), demo_usage()).plain()
        self.assertIsNone(re.search("[А-Яа-яЁё]", text))

    def test_translation_templates_keep_all_placeholders(self):
        def placeholders(text):
            return sorted((name, spec, conversion) for _, name, spec, conversion in Formatter().parse(text)
                          if name is not None)
        for english, russian in RU.items():
            self.assertEqual(placeholders(english), placeholders(russian), english)

    def test_cli_language_help_and_saved_preference(self):
        with tempfile.TemporaryDirectory() as temp:
            env = {**os.environ, "XDG_CONFIG_HOME": temp}
            def run(*args):
                return subprocess.check_output([sys.executable, str(ROOT / "ais"), *args], env=env, text=True)
            self.assertIn("Project directory", run("--help"))
            self.assertIn("Каталог проекта", run("--lang", "ru", "--help"))
            self.assertIn("sessions", run("--lang", "en", "--demo", "--list"))
            self.assertIn("сессий", run("--lang", "ru", "--demo", "--list"))
            self.assertIn("Каталог проекта", run("--help"))


if __name__ == "__main__":
    unittest.main()
