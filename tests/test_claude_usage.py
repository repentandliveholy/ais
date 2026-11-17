"""Native Claude Usage capture plus real PTY transport regressions."""
import os
import sys
import tempfile
import threading
import time
import unittest
from concurrent.futures import CancelledError, ThreadPoolExecutor
from pathlib import Path

from ai_sessions.claude_usage import native_usage_windows, read_claude_usage
from ai_sessions.usage import parse_claude_usage


FIXTURE = Path(__file__).parent / 'fixtures/claude_usage_2_1_325.txt'


class ClaudeUsageTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.binary = self.root / 'claude'
        self.cancel = threading.Event()

    def script(self, body):
        self.binary.write_text('#!' + sys.executable + '\n' + body)
        self.binary.chmod(0o755)

    def read(self, timeout=2):
        return read_claude_usage(str(self.binary), os.environ.copy(), self.root,
                                 self.cancel, timeout)

    def test_real_capture_preserves_all_plan_windows_excludes_local_attribution(self):
        windows = native_usage_windows(FIXTURE.read_text())
        self.assertEqual([w['used_percent'] for w in windows], [0, 14, 12])
        limits = parse_claude_usage(windows, 1234)
        self.assertEqual([r.used for r in limits], [0, .14, .12])
        self.assertEqual([r.window for r in limits], ['5h', 'week', 'week'])
        self.assertEqual([r.label for r in limits], ['Claude', 'All Claude models', 'Fable'])
        self.assertTrue(all(r.updated == 1234 and r.reset == 0 for r in limits))
        self.assertEqual(limits[0].reset_label, 'Resets 5:20pm (Europe/Berlin)')
        self.assertNotIn('50%', repr(limits))

    def test_last_quota_redraw_wins_and_partial_or_nonquota_frames_are_rejected(self):
        capture = FIXTURE.read_text()
        later = '\nCurrent week (all models)\n19% 19% used\nResets tomorrow\nEsc to cancel'
        windows = native_usage_windows(capture + later)
        self.assertEqual(windows[1]['used_percent'], 19)
        self.assertEqual(windows[1]['reset_text'], 'Resets tomorrow')
        for value in ['weekly 50% used',
                      'Settings Status Config Usage Stats\nSession\nUsage: 0 input, 0 output\nEsc to cancel',
                      'Settings Status Config Usage Stats\nCurrent session\n50% used\nResets tomorrow']:
            self.assertEqual(native_usage_windows(value), [])

    def test_transport_sends_only_usage_in_private_tty_and_waits_for_refresh(self):
        self.script("""import os,sys,tty,time
assert sys.argv[1:]==['--safe-mode','--ax-screen-reader','--tools','']
assert os.isatty(0) and os.isatty(1) and os.isatty(2)
tty.setraw(0)
print('Claude Code\\r\\n$ ',end='',flush=True)
command=b''
while not command.endswith(b'\\r'):
 command+=os.read(0,1)
assert command==b'/usage\\r'
print('Settings Status Config Usage Stats\\nCurrent session\\n0% 0% used\\nResets 5pm\\nRefreshing…\\nEsc to cancel',flush=True)
time.sleep(.5)
print('Current session\\n14% 14% used\\nResets 6pm\\nEsc to cancel',flush=True)
os.read(0,1)
""")
        self.assertEqual(self.read()[0]['used_percent'], 14)

    def test_no_login_or_trust_prompt_is_automatically_answered(self):
        for message, expected in [('Not logged in · Run /login', 'cannot see a login'),
                                  ('Enter y/n:', 'workspace trust')]:
            self.script("import os,time\nprint(%r,flush=True)\nos.read(0,1)\nraise AssertionError('NO INPUT EXPECTED')\n" % message)
            with self.assertRaisesRegex(ValueError, expected):
                self.read()

    def test_session_cost_only_and_error_exit_never_become_zero_plan_usage(self):
        self.script("import sys\nprint('SECRET error details',flush=True)\nsys.exit(3)\n")
        with self.assertRaises(ValueError) as error:
            self.read()
        self.assertNotIn('SECRET', str(error.exception))
        self.script("import os,time,tty\ntty.setraw(0)\nprint('$ ',end='',flush=True)\nos.read(0,64)\nprint('Settings Status Config Usage Stats\\nSession\\nUsage: 0 input, 0 output\\nEsc to cancel',flush=True)\ntime.sleep(30)\n")
        with self.assertRaisesRegex(ValueError, 'did not provide plan limits'):
            self.read(.2)

    def test_cancellation_kills_the_private_cli_process(self):
        marker = self.root / 'pid'
        self.script("import os,time\nfrom pathlib import Path\nPath(%r).write_text(str(os.getpid()))\ntime.sleep(30)\n" % str(marker))
        with ThreadPoolExecutor(max_workers=1) as pool:
            future = pool.submit(self.read, 30)
            deadline = time.monotonic() + 2
            while not marker.exists() and time.monotonic() < deadline:
                time.sleep(.01)
            self.assertTrue(marker.exists())
            self.cancel.set()
            with self.assertRaises(CancelledError):
                future.result(timeout=2)
        with self.assertRaises(ProcessLookupError):
            os.kill(int(marker.read_text()), 0)


if __name__ == '__main__':
    unittest.main()
