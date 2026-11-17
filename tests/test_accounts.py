import json
import sys
import tempfile
import time
import unittest
from pathlib import Path
from unittest.mock import patch

from ai_sessions.adapters import Collector
from ai_sessions.screen import Screen
from ai_sessions.storage import Cache
from ai_sessions.usage import Limit, demo_usage, fetch_live_usage, parse_reset_text, pace, render_usage


FIXTURE = (Path(__file__).parent / "fixtures/claude_usage_2_1_325.txt").read_text()


class ResetAndPaceTests(unittest.TestCase):
    def test_native_reset_text_becomes_epoch_only_for_exact_forms(self):
        now = time.mktime((2026, 11, 17, 12, 0, 0, 0, 0, -1))
        self.assertEqual(time.localtime(parse_reset_text("Resets 6pm", now))[3:5], (18, 0))
        self.assertGreater(parse_reset_text("Resets 9:30am", now), now + 20 * 3600)  # Tomorrow.
        self.assertGreater(parse_reset_text("Resets Nov 20 at 6am (Europe/Berlin)", now), now)
        self.assertEqual(parse_reset_text("Resets tomorrow", now), 0)
        self.assertEqual(parse_reset_text("Resets 6pm (Not/AZone)", now), 0)

    def test_pace_projects_running_out_before_reset(self):
        now = 1_000_000
        fast = Limit("x", "#a", "L", "5h", .8, now + 3 * 3600, now, "s")  # 80% after 2h.
        self.assertIn("runs out", pace(fast, now)[0])
        slow = Limit("x", "#a", "L", "5h", .2, now + 2 * 3600, now, "s")  # 20% after 3h.
        self.assertIn("on pace", pace(slow, now)[0])
        self.assertIsNone(pace(Limit("x", "#a", "L", "5h", .5, 0, now, "s"), now))

    def test_demo_usage_shows_two_claude_accounts_separately(self):
        text = render_usage(Screen(144, 70), demo_usage()).plain()
        self.assertIn("dev@example.com", text)
        self.assertIn("side@example.com", text)
        self.assertIn("separate account", text)


class ClaudeAccountsTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name).resolve()
        self.bin = self.root / "bin"
        self.bin.mkdir()
        for name in (".claude", ".claude-work"):
            (self.root / name / "projects").mkdir(parents=True)
        (self.root / ".claude-unrelated").mkdir()  # No history or login: ignored.
        self.which = patch("ai_sessions.usage.shutil.which",
                           side_effect=lambda name: str(self.bin / name) if (self.bin / name).exists() else None)
        self.which.start()
        self.addCleanup(self.which.stop)

    def claude(self, emails):
        # auth status answers per CLAUDE_CONFIG_DIR; /usage prints the fixture.
        script = self.bin / "claude"
        script.write_text("#!" + sys.executable + """
import os, sys, json, tty
emails = %r
store = os.environ.get('CLAUDE_CONFIG_DIR', '')
if sys.argv[1:] == ['auth', 'status', '--json']:
    email = emails[os.path.basename(store)]
    print(json.dumps({'loggedIn': True, 'email': email, 'orgId': 'org-' + email, 'orgName': 'Org',
                      'subscriptionType': 'max', 'loginExpiresAt': '2030-01-01T00:00:00Z'}))
    sys.exit(0)
tty.setraw(0)
print('Claude Code\\r\\n$ ', end='', flush=True)
command = b''
while not command.endswith(b'\\r'):
    command += os.read(0, 1)
print(%r, flush=True)
os.read(0, 1)
""" % (emails, FIXTURE))
        script.chmod(0o755)

    def fetch(self):
        collector = Collector(self.root, Cache(self.root / "cache"))
        return fetch_live_usage(collector)

    def test_two_logins_are_two_accounts(self):
        self.claude({".claude": "one@example.com", ".claude-work": "two@example.com"})
        usage = self.fetch()
        claude = [a for a in usage.accounts if a.provider == "anthropic"]
        self.assertEqual(sorted(a.email for a in claude), ["one@example.com", "two@example.com"])
        self.assertEqual({a.plan for a in claude}, {"Max"})
        keys = {r.account for r in usage.limits if r.provider == "anthropic" and r.status != "unavailable"}
        self.assertEqual(len(keys), 2)
        text = render_usage(Screen(144, 80), usage).plain()
        self.assertIn("separate account", text)
        self.assertIn("~/.claude-work", text)
        self.assertNotIn(".claude-unrelated", text)
        cached = json.loads(next((self.root / "cache").glob("official-usage-*.json")).read_text())
        self.assertEqual(len(cached["accounts"]), len(usage.accounts))

    def test_same_login_in_two_dirs_is_one_account(self):
        self.claude({".claude": "same@example.com", ".claude-work": "same@example.com"})
        usage = self.fetch()
        claude = [a for a in usage.accounts if a.provider == "anthropic"]
        self.assertEqual(len(claude), 1)
        self.assertEqual(len(claude[0].dirs), 2)
        self.assertIn("same account", render_usage(Screen(144, 60), usage).plain())


if __name__ == "__main__":
    unittest.main()
