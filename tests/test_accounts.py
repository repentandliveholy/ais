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


class AgyAccountTests(unittest.TestCase):
    def test_identity_comes_only_from_logs_of_this_run(self):
        from ai_sessions.usage import agy_identity
        import os
        with tempfile.TemporaryDirectory() as temp:
            log = Path(temp) / "log"
            log.mkdir()
            old = log / "cli-old.log"
            old.write_text("server_oauth.go:221] applyAuthResult: email=old@example.com, authMethod=consumer, quotaProject=\n")
            os.utime(old, (1000, 1000))
            self.assertEqual(agy_identity(Path(temp), time.time()), ("", ""))
            (log / "cli-new.log").write_text("x\nserver_oauth.go:221] applyAuthResult: email=new@example.com, authMethod=consumer, quotaProject=\n")
            self.assertEqual(agy_identity(Path(temp), time.time()), ("new@example.com", "consumer"))


class DeepSeekTests(unittest.TestCase):
    def test_balance_parsing_and_rendering(self):
        from ai_sessions.usage import Usage, parse_deepseek_balance, usage_content
        payload = {"is_available": True, "balance_infos": [
            {"currency": "CNY", "total_balance": "110.00", "granted_balance": "10.00", "topped_up_balance": "100.00"},
            {"currency": "USD", "total_balance": "0.00", "granted_balance": "0.00", "topped_up_balance": "0.00"}]}
        limits = parse_deepseek_balance(payload, time.time())
        self.assertEqual([l.status for l in limits], ["balance"])  # Empty USD is hidden.
        text = "\n".join(t for t, _ in usage_content(Usage(limits, loaded=True), 140, time.time()))
        self.assertIn("balance ¥110.00", text)
        self.assertIn("topped up ¥100.00", text)
        self.assertNotIn("balance exhausted", text)
        empty = parse_deepseek_balance({"is_available": False, "balance_infos": [
            {"currency": "USD", "total_balance": "0.00", "granted_balance": "0", "topped_up_balance": "0"}]}, 0)
        self.assertEqual(empty[0].status, "balance-empty")
        self.assertIn("balance exhausted", "\n".join(t for t, _ in usage_content(Usage(empty, loaded=True), 140, time.time())))
        with self.assertRaises(ValueError):
            parse_deepseek_balance({"error": "x"}, 0)

    def test_key_source_and_errors_never_echo_the_key(self):
        import os
        import urllib.error
        from ai_sessions.usage import deepseek_key, fetch_deepseek_balance, save_deepseek_key
        import threading
        with tempfile.TemporaryDirectory() as temp, patch.dict(os.environ, {"XDG_CONFIG_HOME": temp}, clear=False):
            os.environ.pop("DEEPSEEK_API_KEY", None)
            self.assertEqual(deepseek_key(), "")
            path = save_deepseek_key("sk-secret-1234")
            self.assertEqual(path.stat().st_mode & 0o777, 0o600)
            self.assertEqual(deepseek_key(), "sk-secret-1234")
            with patch.dict(os.environ, {"DEEPSEEK_API_KEY": "sk-env"}):
                self.assertEqual(deepseek_key(), "sk-env")
            error = urllib.error.HTTPError("u", 401, "no", {}, None)
            with patch("urllib.request.urlopen", side_effect=error):
                with self.assertRaises(ValueError) as caught:
                    fetch_deepseek_balance("sk-secret-1234", threading.Event(), 5)
            self.assertIn("401", str(caught.exception))
            self.assertNotIn("secret", str(caught.exception))
