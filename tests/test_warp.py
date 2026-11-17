import json
import os
import subprocess
import sys
import tempfile
import time
import unittest
from pathlib import Path
from urllib.parse import parse_qs, urlsplit
from unittest.mock import patch

from ai_sessions.warp import find_warp_app, open_uri, open_tab, read_state, registry_path, warp_scheme, write_state


class WarpTests(unittest.TestCase):
    def test_detects_warp_channel(self):
        with patch('ai_sessions.warp.sys.platform', 'darwin'), patch('ai_sessions.warp.find_warp_app', return_value=Path('/Applications/WarpOss.app')), patch.dict(os.environ, {'TERM_PROGRAM': 'WarpTerminal', 'WARP_FOCUS_URL': 'warposs://session/abc'}):
            self.assertEqual(warp_scheme(), 'warposs')
        with patch('ai_sessions.warp.sys.platform', 'linux'):
            self.assertIsNone(warp_scheme())

    def test_unregistered_oss_scheme_preserves_oss_channel(self):
        with patch('ai_sessions.warp.sys.platform', 'darwin'), \
             patch.dict(os.environ, {'TERM_PROGRAM': 'WarpTerminal', 'WARP_FOCUS_URL': 'warposs://session/abc'}), \
             patch('ai_sessions.warp.find_warp_app', side_effect=lambda scheme: Path('/Applications/Warp.app') if scheme == 'warp' else None):
            self.assertEqual(warp_scheme(), 'warposs')

    def test_local_oss_bundle_without_url_registration(self):
        import io
        import plistlib
        data = plistlib.dumps({'CFBundleIdentifier': 'dev.warp.WarpOss',
                              'CFBundleExecutable': 'warp-oss'})
        with patch('ai_sessions.warp.Path.open', side_effect=lambda *args, **kwargs: io.BytesIO(data)), \
             patch('ai_sessions.warp.Path.is_file', return_value=True):
            self.assertEqual(find_warp_app('warposs'), Path('/Applications/WarpOss.app'))

    def test_missing_oss_never_opens_proprietary_warp(self):
        with patch('ai_sessions.warp.find_warp_app', return_value=None), \
             patch('ai_sessions.warp.subprocess.run') as run:
            with self.assertRaisesRegex(OSError, 'warposs'):
                open_uri('warposs://session/test')
            run.assert_not_called()

    def test_open_targets_warp_app_explicitly(self):
        with patch('ai_sessions.warp.find_warp_app', return_value=Path('/Applications/Warp.app')), \
             patch('ai_sessions.warp.subprocess.run') as run:
            run.return_value.returncode = 0
            uri = 'warp://action/new_tab?path=%2Ftmp%2Flaunch.sh'
            open_uri(uri)
            self.assertEqual(run.call_args.args[0], ['/usr/bin/open', '-a', '/Applications/Warp.app', uri])

    def test_reuses_idle_project_and_only_focuses_busy_project(self):
        with tempfile.TemporaryDirectory() as temp, patch('ai_sessions.warp.tempfile.gettempdir', return_value=temp), patch('ai_sessions.warp.open_uri') as opened:
            cwd = str(Path(temp) / 'project')
            state_path = registry_path(cwd, 'warposs')
            focus = 'warposs://session/abc'
            write_state(state_path, {'pid': os.getpid(), 'focus_url': focus, 'busy': False})
            self.assertEqual(open_tab(['/fake/cli', 'id'], cwd, {'PATH': '/bin'}, scheme='warposs'), 'reused')
            self.assertEqual(read_state(state_path)['request']['argv'], ['/fake/cli', 'id'])
            opened.assert_called_once_with(focus)
            self.assertEqual(open_tab(['/another/cli'], cwd, {}, scheme='warposs'), 'focused')
            self.assertEqual(read_state(state_path)['request']['argv'], ['/fake/cli', 'id'])
            write_state(state_path, {'pid': os.getpid(), 'focus_url': focus, 'busy': True})
            self.assertEqual(open_tab(['/another/cli'], cwd, {}, scheme='warposs'), 'focused')
            self.assertNotIn('request', read_state(state_path))

    def test_failed_open_cleans_request_and_script(self):
        with tempfile.TemporaryDirectory() as temp, patch('ai_sessions.warp.tempfile.gettempdir', return_value=temp), patch('ai_sessions.warp.open_uri', side_effect=OSError('failed')), patch('ai_sessions.warp.tab_config_dir', return_value=Path(temp) / 'configs'):
            with self.assertRaisesRegex(OSError, 'failed'):
                open_tab(['/bin/true'], temp, {}, scheme='warposs')
            self.assertEqual(read_state(registry_path(temp, 'warposs')), {})

    @unittest.skipIf(sys.version_info < (3, 11), "tomllib is Python 3.11+")
    def test_worker_executes_exact_argv_cwd_title_and_reuses_tab(self):
        with tempfile.TemporaryDirectory() as temp, patch('ai_sessions.warp.tempfile.gettempdir', return_value=temp), patch('ai_sessions.warp.tab_config_dir', return_value=Path(temp) / 'configs'):
            cwd = Path(temp) / "project spaces ' $()"
            cwd.mkdir()
            output = Path(temp) / 'result.json'
            payload = "id; $(echo unsafe) ' quote"
            command = [sys.executable, '-c', 'import os,sys,json; open(sys.argv[1],"w").write(json.dumps([sys.argv[2:],os.getcwd(),os.environ.get("CODEX_HOME"),os.environ.get("SECRET_TOKEN")]))', str(output), payload]
            uris = []
            with patch('ai_sessions.warp.open_uri', side_effect=uris.append):
                self.assertEqual(open_tab(command, str(cwd), {'PATH': os.environ['PATH'], 'CODEX_HOME': '/source with spaces', 'SECRET_TOKEN': 'do-not-copy'}, scheme='warposs'), 'opened')
                import tomllib, shlex
                config_path = Path(temp) / 'configs' / (urlsplit(uris[0]).path.lstrip('/') + '.toml')
                config = tomllib.loads(config_path.read_text())
                self.assertEqual(config['title'], cwd.name)
                self.assertEqual(config['panes'][0]['children'], ['main', 'right'])
                self.assertEqual(config['panes'][2]['children'], ['top', 'bottom'])
                self.assertEqual([p['directory'] for p in config['panes'] if p.get('type') == 'terminal'], [str(cwd)] * 3)
                script = Path(shlex.split(config['panes'][1]['commands'][0])[1])
                self.assertEqual(script.stat().st_mode & 0o777, 0o700)
                state_path = registry_path(str(cwd), 'warposs')
                self.assertNotIn('do-not-copy', state_path.read_text())
                env = {**os.environ, 'WARP_FOCUS_URL': 'warposs://session/test'}
                env.pop('SECRET_TOKEN', None)
                worker = subprocess.Popen(['/bin/sh', str(script)], env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
                try:
                    deadline = time.monotonic() + 3
                    while not output.exists() or read_state(state_path).get('busy'):
                        if time.monotonic() > deadline:
                            self.fail('worker failed to finish first command')
                        time.sleep(.01)
                    self.assertEqual(json.loads(output.read_text()), [[payload], str(cwd.resolve()), '/source with spaces', None])
                    self.assertFalse(script.exists())
                    output.unlink()
                    self.assertEqual(open_tab(command, str(cwd), {'PATH': os.environ['PATH']}, scheme='warposs'), 'reused')
                    deadline = time.monotonic() + 3
                    while not output.exists():
                        if time.monotonic() > deadline:
                            self.fail('worker failed to run second command')
                        time.sleep(.01)
                finally:
                    worker.terminate()
                    stdout, stderr = worker.communicate(timeout=3)
                self.assertIn(('\x1b]0;' + cwd.name + '\x07').encode(), stdout)
                self.assertEqual(stderr, b'')
                self.assertIsNone(json.loads(output.read_text())[2])
