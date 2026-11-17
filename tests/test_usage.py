import json
import os
import sys
import threading
import tempfile
import time
import unittest
from concurrent.futures import CancelledError, ThreadPoolExecutor
from pathlib import Path
from unittest.mock import patch

from ai_sessions.adapters import Collector
from ai_sessions.screen import Screen
from ai_sessions.storage import Cache
from ai_sessions.usage import (Limit, Usage, _agy_report, _cache_path, demo_usage,
                               fetch_live_usage, load_usage, max_usage_scroll,
                               parse_agy_usage, parse_codex_usage, render_usage, reset_text, window_name)


class UsageTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.bin = self.root / "bin"
        self.bin.mkdir()
        self.collector = Collector(self.root, Cache(self.root / "cache"))
        self.commands = []
        self.which = patch("ai_sessions.usage.shutil.which", side_effect=lambda name: str(self.bin / name) if (self.bin / name).exists() else None)
        self.which.start()
        self.addCleanup(self.which.stop)

    @staticmethod
    def payload():
        primary = {"limitId": "codex", "limitName": "Codex", "primary": {
            "usedPercent": 25, "windowDurationMins": 300, "resetsAt": 1900000000},
            "secondary": {"usedPercent": 60, "windowDurationMins": 10080, "resetsAt": 1900001000}}
        return {"accountId": "private-account", "rateLimits": primary,
                "rateLimitsByLimitId": {"codex": primary,
                    "other": {"limitName": "Other model", "primary": {"usedPercent": 40, "windowDurationMins": 60}}}}

    def binary(self, name, script):
        path = self.bin / name
        path.write_text("#!" + sys.executable + "\n" + script)
        path.chmod(0o755)
        return path

    def codex(self, payload=None, response=None, before_reply=""):
        if payload is None:
            payload = self.payload()
        self.binary("codex", """import sys,json,os
from pathlib import Path
assert sys.argv[1:] == ['app-server']
assert os.environ['CODEX_HOME'] == %r
first=json.loads(sys.stdin.readline())
assert first['method']=='initialize' and first['params']['clientInfo']['name']=='ai_sessions'
print(json.dumps({'method':'notice','params':{'ignored':'SECRET'}}),flush=True)
print(json.dumps({'id':first['id'],'result':{'userAgent':'test'}}),flush=True)
assert json.loads(sys.stdin.readline())['method']=='initialized'
request=json.loads(sys.stdin.readline())
assert request['method']=='account/rateLimits/read'
Path(%r).write_text(json.dumps(request))
%s
print(json.dumps(%r),flush=True)
sys.stdin.read()
""" % (str(self.root / '.codex'), str(self.root / 'request.json'), before_reply,
       response if response is not None else {'id': 2, 'result': payload}))

    def claude(self, text=None):
        if text is None:
            text = (Path(__file__).parent/'fixtures/claude_usage_2_1_325.txt').read_text()
        self.binary('claude', """import sys,os,tty
assert sys.argv[1:]==['--safe-mode','--ax-screen-reader','--tools','']
assert os.isatty(0) and os.isatty(1)
assert os.environ['CLAUDE_CONFIG_DIR']==%r
tty.setraw(0)
print('Claude Code v2.1.325\\r\\n$ ',end='',flush=True)
command=b''
while not command.endswith(b'\\r'):
 command+=os.read(0,1)
assert command==b'/usage\\r'
print(%r,flush=True)
os.read(0,1)
""" % (str(self.root/'.claude'), text))

    def agy(self):
        text = (Path(__file__).parent/'fixtures/agy_usage_1_5_9.tsv').read_text()
        self.binary('agy', "import sys\nassert sys.argv[1:]==['-p','/usage']\nprint(%r)\n" % text)

    def test_windows_have_consistent_labels(self):
        for value in ['5 hours', '5 Hour', '5h', '300 min']:
            self.assertEqual(window_name(value), '5h')
        for value in ['Weekly', '7 days', '1w']:
            self.assertEqual(window_name(value), 'week')

    def test_codex_public_schema_multiple_buckets_without_legacy_duplicates(self):
        limits = parse_codex_usage(self.payload(), 1234)
        self.assertEqual(len(limits), 3)
        self.assertEqual([r.used for r in limits], [.25, .6, .4])
        self.assertEqual([r.window for r in limits], ['5h', 'week', '60m'])
        self.assertEqual(limits[0].reset, 1900000000)
        self.assertEqual(limits[0].updated, 1234)
        self.assertNotIn('private-account', repr(limits))
        legacy = {'rateLimits': self.payload()['rateLimits']}
        self.assertEqual(len(parse_codex_usage(legacy, 1234)), 2)

    def test_codex_missing_invalid_or_nonfinite_amounts_remain_unknown(self):
        for value in [None, True, '50', float('nan'), float('inf'), -2, 101]:
            payload = {'rateLimits': {'primary': {'usedPercent': value, 'resetsAt': float('inf')}}}
            limit = parse_codex_usage(payload, 1234)[0]
            self.assertIsNone(limit.used)
            self.assertEqual(limit.reset, 0)
        for payload in [None, {}, {'rateLimits': {}}, {'rateLimitsByLimitId': []}, {'rateLimits': {'primary': 3}}]:
            with self.assertRaises(ValueError):
                parse_codex_usage(payload, 1234)

    def test_only_official_cli_requests_and_atomic_private_cache(self):
        self.codex()
        self.agy()
        self.binary('omp', "raise AssertionError('OMP MUST NEVER RUN')\n")
        self.claude()
        result = fetch_live_usage(self.collector)
        self.assertTrue(result.live)
        self.assertEqual(result.errors, [])
        self.assertEqual(len([r for r in result.limits if r.provider=='openai-codex']), 3)
        self.assertEqual(len([r for r in result.limits if r.provider=='google-antigravity']), 3)
        self.assertEqual(json.loads((self.root/'request.json').read_text())['method'], 'account/rateLimits/read')
        cached = load_usage(self.collector)
        self.assertFalse(cached.live)
        agy_cached = [r for r in cached.limits if r.provider=='google-antigravity']
        agy_live = [r for r in result.limits if r.provider=='google-antigravity']
        self.assertTrue(all(r.source=='agy CLI · cached' for r in agy_cached))
        self.assertEqual(agy_cached[0].updated, agy_live[0].updated)
        self.assertEqual(cached.limits[1].updated, result.limits[1].updated)
        self.assertEqual(_cache_path(self.collector).stat().st_mode & 0o777, 0o600)
        self.assertIn('cached', cached.limits[1].source)
        self.assertNotIn('private-account', _cache_path(self.collector).read_text())
        self.assertTrue(any(r.provider=='anthropic' and r.source=='Claude CLI · cached' for r in cached.limits))

    def test_failures_keep_successful_snapshot_timestamp_and_sanitize_errors(self):
        self.codex()
        first = fetch_live_usage(self.collector)
        self.codex(response={'id': 2, 'error': {'code': 401, 'message': 'SECRET user@example.test'}})
        result = fetch_live_usage(self.collector)
        limits = [r for r in result.limits if r.provider=='openai-codex']
        self.assertEqual(limits[0].updated, first.limits[1].updated)
        self.assertIn('cached', limits[0].source)
        self.assertTrue(any('401' in e for e in result.errors))
        self.assertNotIn('SECRET', repr(result))
        self.assertNotIn('user@example.test', repr(result))
        self.assertFalse(result.live)

    def test_claude_auth_failure_retains_cached_native_windows_and_reset_labels(self):
        self.claude()
        first = fetch_live_usage(self.collector)
        before = [r for r in first.limits if r.provider=='anthropic']
        self.binary('claude', "import time\nprint('Not logged in · Run /login',flush=True)\ntime.sleep(30)\n")
        result = fetch_live_usage(self.collector)
        after = [r for r in result.limits if r.provider=='anthropic']
        self.assertEqual(len(after), 3)
        self.assertEqual([r.updated for r in after], [r.updated for r in before])
        self.assertEqual([r.reset_label for r in after], [r.reset_label for r in before])
        self.assertTrue(all(r.source=='Claude CLI · cached' for r in after))
        self.assertIn('cannot see a login', ' '.join(result.errors))
        self.assertFalse(result.live)

    def test_independent_agy_refresh_failure_keeps_old_limits_and_timestamp(self):
        self.codex()
        self.agy()
        first = fetch_live_usage(self.collector)
        self.binary('agy', "import sys\nprint('Error: SECRET user@example.test')\nsys.exit(0)\n")
        result = fetch_live_usage(self.collector)
        self.assertTrue(result.live)  # Codex succeeded independently.
        before = next(r for r in first.limits if r.provider=='google-antigravity')
        after = next(r for r in result.limits if r.provider=='google-antigravity')
        self.assertEqual(after.source, 'agy CLI · cached')
        self.assertEqual(after.updated, before.updated)
        self.assertEqual(after.used, before.used)
        self.assertEqual(len([r for r in result.limits if r.provider=='google-antigravity']), 3)
        self.assertNotIn('SECRET', repr(result))
        self.assertIn('agy:', ' '.join(result.errors))

    def test_server_auth_requests_are_declined_without_credential_access(self):
        before = """print(json.dumps({'id':99,'method':'account/chatgptAuthTokens/refresh','params':{}}),flush=True)
reply=next(m for m in map(json.loads, iter(sys.stdin.readline, '')) if m.get('id')==99)
assert reply['id']==99 and reply['error']['code']==-32601
assert 'result' not in reply
"""
        self.codex(before_reply=before)
        result = fetch_live_usage(self.collector)
        self.assertTrue(result.live)
        self.assertFalse(any(e.startswith('codex:') for e in result.errors))

    def test_report_wrapping_timestamps_scrolling_and_localized_unavailable_states(self):
        from ai_sessions.i18n import set_language
        from ai_sessions.usage import Report, usage_content
        try:
            for lang, expected in [('en', 'No saved Codex'), ('ru', 'Нет сохранённых лимитов')]:
                set_language(lang)
                usage = load_usage(self.collector)
                self.assertIn(expected, '\n'.join(text for text, _ in usage_content(usage, 44, time.time())))
            set_language('en')
            usage = Usage(loaded=True, reports=[Report('google-antigravity',
                'Gemini models\n' + '\n'.join(f'ROW_{i:03} ' + 'x'*130 for i in range(20)), 1700000000)])
            content = usage_content(usage, 44, time.time())
            from ai_sessions.screen import width
            self.assertTrue(all(width(text)<=40 for text,_ in content))
            self.assertIn(time.strftime('%Y-%m-%d %H:%M:%S', time.localtime(1700000000)), '\n'.join(text for text,_ in content))
            limit = max_usage_scroll(usage, 44, 16)
            self.assertGreater(limit, 0)
            self.assertIn('ROW_019', render_usage(Screen(44,16), usage, limit-3).plain())
        finally:
            set_language('en')

    def test_no_omp_database_or_rollout_fallback_and_missing_clis(self):
        (self.root/'.omp/agent').mkdir(parents=True)
        (self.root/'.omp/agent/agent.db').write_text('not sqlite')
        (self.root/'.codex/sessions').mkdir(parents=True)
        (self.root/'.codex/sessions/session.jsonl').write_text(json.dumps({'type':'event_msg', 'payload':{'rate_limits':{'primary':{'used_percent':5}}}}))
        self.assertEqual(load_usage(self.collector).errors, [])
        result = fetch_live_usage(self.collector)
        self.assertFalse(result.live)
        self.assertEqual(len(result.errors), 3)
        self.assertTrue(all(r.used is None and r.status=='unavailable' for r in result.limits))
        self.assertEqual(result.reports, [])

    def test_cache_disabled_and_namespace_isolation_and_malformed_cache(self):
        self.codex()
        collector = Collector(self.root, Cache(None, False))
        self.assertTrue(fetch_live_usage(collector).live)
        self.assertIsNone(_cache_path(collector))
        self.assertFalse(any(r.used is not None for r in load_usage(self.collector).limits))
        fetch_live_usage(self.collector)
        other = Collector(self.root, self.collector.cache, {'codex':[self.root/'alternate']})
        self.assertNotEqual(_cache_path(other), _cache_path(self.collector))
        self.assertTrue(all(r.used is None for r in load_usage(other).limits))
        _cache_path(self.collector).write_text('{broken')
        self.assertIn('cache could not be read', load_usage(self.collector).errors[0])

    def test_agy_alternate_source_does_not_query_default_account(self):
        self.binary('agy', "raise AssertionError('wrong account')\n")
        other = Collector(self.root, Cache(None, False), {'agy':[self.root/'alternate']})
        result = fetch_live_usage(other)
        self.assertIn('agy CLI does not support an alternate usage store', ' '.join(result.errors))

    def test_agy_public_text_stays_text_and_filters_hidden_model_groups(self):
        report = _agy_report('\x1b[31mGemini Flash\n50% remaining\nResets in 2h\nClaude GPT\n30% remaining\nGemini Pro\n75% remaining\naccount user@example.test')
        self.assertIn('Gemini Flash\n50% remaining', report)
        self.assertIn('75% remaining', report)
        self.assertNotIn('30%', report)
        self.assertNotIn('Claude', report)
        self.assertNotIn('user@example.test', report)
        self.assertNotIn('\x1b', report)
        for text in ['', 'Error: SECRET auth failure', 'Usage: agy [options]', 'Claude\n50% remaining']:
            with self.assertRaises(ValueError):
                _agy_report(text)

    def test_agy_native_tsv_remaining_percent_windows_and_model_pools(self):
        text = (Path(__file__).parent/'fixtures/agy_usage_1_5_9.tsv').read_text()
        # The captured official report is all 100% remaining. Exercise the
        # direction conversion too, without inventing a different CLI schema.
        limits = parse_agy_usage(text.replace('100%', '35%', 1), 1234)
        self.assertEqual(len(limits), 3)
        self.assertEqual(limits[0].used, 0)
        self.assertAlmostEqual(limits[1].used, .65)
        self.assertEqual([r.window for r in limits], ['5h', 'week', 'week'])
        self.assertEqual(limits[2].label, 'Gemini 4 Argon')
        self.assertTrue(all(r.reset > 0 and r.updated==1234 for r in limits))
        self.assertNotIn('Claude', repr(limits))
        self.assertNotIn('GPT', repr(limits))
        for changed in [text.replace('100%', 'unknown', 1),
                        text.replace('100%', '101%', 1),
                        text.replace('2026-11-21T09:27:29Z', 'unknown', 1),
                        text.replace('Weekly Limit Remaining', 'Unknown Limit', 1),
                        text.replace('\t', ' ', 1)]:
            with self.assertRaises(ValueError):
                parse_agy_usage(changed, 1234)

    def test_legacy_official_agy_text_cache_migrates_to_bars(self):
        path = _cache_path(self.collector)
        path.parent.mkdir(parents=True)
        text = (Path(__file__).parent/'fixtures/agy_usage_1_5_9.tsv').read_text()
        path.write_text(json.dumps({'version':1, 'limits':[], 'reports':[
            {'provider':'google-antigravity','text':text,'updated':1700000000,'cached':False}]}))
        cached = load_usage(self.collector)
        self.assertEqual(cached.errors, [])
        self.assertEqual(cached.reports, [])
        limits = [r for r in cached.limits if r.provider=='google-antigravity']
        self.assertEqual(len(limits), 3)
        self.assertTrue(all(r.source=='agy CLI · cached' and r.updated==1700000000 for r in limits))

    def test_timeout_exit_and_malformed_responses_do_not_leak_stderr(self):
        self.binary('codex', "import time\ntime.sleep(30)\n")
        start = time.monotonic()
        result = fetch_live_usage(self.collector, timeout=.15)
        self.assertLess(time.monotonic()-start, 2)
        self.assertIn('timed out', ' '.join(result.errors))
        self.binary('codex', "import sys\nprint('SECRET',file=sys.stderr)\nsys.exit(2)\n")
        result = fetch_live_usage(self.collector)
        self.assertIn('code 2', ' '.join(result.errors))
        self.assertNotIn('SECRET', repr(result))
        self.binary('codex', "import sys\nsys.stdin.readline()\nprint('not json',flush=True)\nsys.stdin.read()\n")
        self.assertIn('Unsupported CLI response', ' '.join(fetch_live_usage(self.collector).errors))

    def test_live_request_cancels_promptly_without_leaving_a_child(self):
        marker = self.root/'started'
        self.binary('codex', "import time,os\nfrom pathlib import Path\nPath(%r).write_text(str(os.getpid()))\ntime.sleep(30)\n" % str(marker))
        cancel = threading.Event()
        with ThreadPoolExecutor(max_workers=1) as pool:
            future = pool.submit(fetch_live_usage, self.collector, cancel)
            deadline = time.monotonic()+3
            while not marker.exists() and time.monotonic()<deadline:
                time.sleep(.01)
            self.assertTrue(marker.exists())
            cancel.set()
            with self.assertRaises(CancelledError):
                future.result(timeout=2)
        with self.assertRaises(ProcessLookupError):
            os.kill(int(marker.read_text()), 0)

    def test_unknown_usage_is_not_reported_as_zero(self):
        usage = Usage([Limit("test", "#account", "Limit", "week", None, 0, 0, "OMP")], loaded=True)
        screen = render_usage(Screen(144, 44), usage)
        self.assertIn("remaining usage unknown", screen.plain())
        self.assertNotIn("100% remaining", screen.plain())

    def test_expired_window_is_explicit(self):
        now = time.time()
        self.assertIn("window expired", reset_text(now - 1, now))
        self.assertIn("resets in 2h", reset_text(now + 7200, now))

    def test_usage_layout_and_scroll_at_narrow_and_wide_sizes(self):
        usage = demo_usage()
        for cols, rows in [(44, 16), (80, 30), (144, 44)]:
            limit = max_usage_scroll(usage, cols, rows)
            screen = render_usage(Screen(cols, rows), usage, scroll=limit)
            self.assertIn("LIMITS", screen.plain())
            self.assertIn("remaining", screen.plain())
            self.assertGreaterEqual(limit, 0)

    def test_scroll_clamps_to_actual_content_in_both_directions(self):
        # A provider may reappear after another source/account. Count the
        # actual headings rather than the number of distinct provider keys.
        usage = demo_usage()
        usage.limits = usage.limits * 10
        usage.errors = ["TAIL_ERROR"]
        for cols, rows in [(44, 16), (80, 30), (144, 44)]:
            limit = max_usage_scroll(usage, cols, rows)
            end = render_usage(Screen(cols, rows), usage, limit).plain()
            self.assertIn("TAIL_ERROR", end)
            self.assertEqual(end, render_usage(Screen(cols, rows), usage, 100000).plain())
            self.assertEqual(render_usage(Screen(cols, rows), usage).plain(),
                             render_usage(Screen(cols, rows), usage, -100).plain())


if __name__ == "__main__":
    unittest.main()
