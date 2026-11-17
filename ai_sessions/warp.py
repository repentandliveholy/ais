"""Launch a CLI through Warp's new-tab file URI handler on macOS."""
from __future__ import annotations

import os
import plistlib
import fcntl
import hashlib
import json
import signal
import time
from contextlib import contextmanager
import shlex
import subprocess
import sys
import tempfile
from pathlib import Path
from urllib.parse import urlsplit

from .model import line


def find_warp_app(scheme: str) -> Path | None:
    names = {'warposs': ('WarpOss.app',), 'warp': ('Warp.app',),
             'warppreview': ('WarpPreview.app',), 'warpterminal': ('Warp.app',)}.get(scheme, ())
    for root in (Path('/Applications'), Path.home() / 'Applications'):
        for name in names:
            app = root / name
            try:
                with (app / 'Contents/Info.plist').open('rb') as source:
                    info = plistlib.load(source)
                schemes = [s for entry in info.get('CFBundleURLTypes', [])
                           for s in entry.get('CFBundleURLSchemes', [])]
                # Locally packaged OSS builds may omit CFBundleURLTypes.
                # Explicit `open -a` still delivers the URL to the app.
                matching_oss = scheme == 'warposs' and info.get('CFBundleIdentifier') == 'dev.warp.WarpOss'
                if (scheme in schemes or matching_oss) and (app / 'Contents/MacOS' / info['CFBundleExecutable']).is_file():
                    return app
            except (OSError, ValueError, KeyError, plistlib.InvalidFileException):
                continue
    return None


def warp_scheme() -> str | None:
    if sys.platform != 'darwin' or os.environ.get('TERM_PROGRAM') != 'WarpTerminal':
        return None
    preferred = urlsplit(os.environ.get('WARP_FOCUS_URL', '')).scheme
    # Preserve the running channel even when its custom scheme is not registered.
    return preferred if preferred in ('warp', 'warposs', 'warppreview', 'warpterminal') else 'warp'


def registry_path(cwd: str, scheme: str) -> Path:
    root = Path(tempfile.gettempdir()) / f'ais-warp-{os.getuid()}'
    root.mkdir(mode=0o700, exist_ok=True)
    if root.stat().st_uid != os.getuid() or root.stat().st_mode & 0o077:
        raise OSError('Warp tab registry must be private')
    key = hashlib.sha256((scheme + ':' + os.path.realpath(cwd)).encode()).hexdigest()[:32]
    return root / (key + '.json')


@contextmanager
def registry_lock(path: Path):
    with path.with_suffix('.lock').open('a') as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        yield


def read_state(path: Path) -> dict:
    try:
        state = json.loads(path.read_text())
        if state.get('pid'):
            os.kill(state['pid'], 0)
        elif time.time() - state.get('created', 0) > 30:
            return {}
        return state
    except (OSError, ValueError, TypeError):
        return {}


def write_state(path: Path, state: dict):
    temporary = path.with_suffix('.tmp')
    temporary.write_text(json.dumps(state))
    temporary.replace(path)


def open_uri(uri: str):
    app = find_warp_app(urlsplit(uri).scheme)
    if app is None:
        raise OSError('Cannot locate the Warp application for ' + urlsplit(uri).scheme)
    command = ['/usr/bin/open', '-a', str(app), uri]
    try:
        result = subprocess.run(command, capture_output=True, text=True, timeout=10)
    except subprocess.TimeoutExpired as exc:
        raise OSError('Warp did not respond within 10 seconds') from exc
    if result.returncode:
        raise OSError(line(result.stderr) or 'Warp could not open a tab')


def tab_config_dir(scheme: str) -> Path:
    directory = {'warposs': '.warp-oss', 'warppreview': '.warp-preview'}.get(scheme, '.warp')
    return Path.home() / directory / 'tab_configs'


def create_tab_config(cwd: str, scheme: str, script: Path, key: str) -> str:
    directory = tab_config_dir(scheme)
    directory.mkdir(parents=True, exist_ok=True)
    name = 'ais-' + key
    title = line(Path(cwd).name or cwd)
    quote = lambda value: json.dumps(value, ensure_ascii=False)
    text = ('name = ' + quote(title) + '\ntitle = ' + quote(title) + '\n'
            '\n[[panes]]\nid = "root"\nsplit = "horizontal"\nchildren = ["main", "right"]\n'
            '\n[[panes]]\nid = "main"\ntype = "terminal"\nis_focused = true\n'
            'directory = ' + quote(cwd) + '\ncommands = [' + quote(shlex.join(['/bin/sh', str(script)])) + ']\n'
            '\n[[panes]]\nid = "right"\nsplit = "vertical"\nchildren = ["top", "bottom"]\n'
            '\n[[panes]]\nid = "top"\ntype = "terminal"\ndirectory = ' + quote(cwd) + '\n'
            '\n[[panes]]\nid = "bottom"\ntype = "terminal"\ndirectory = ' + quote(cwd) + '\n')
    path = directory / (name + '.toml')
    path.write_text(text)
    path.chmod(0o600)
    return scheme + '://tab_config/' + name


def open_tab(argv: list[str], cwd: str, env: dict[str, str], *, scheme: str | None = None) -> str:
    scheme = scheme or warp_scheme()
    if scheme not in ('warp', 'warposs', 'warppreview', 'warpterminal'):
        raise ValueError('Opening a Warp tab requires macOS and Warp')
    state_path = registry_path(cwd, scheme)
    config = {key: env[key] for key in
              ('PATH', 'CODEX_HOME', 'CLAUDE_CONFIG_DIR', 'PI_CODING_AGENT_DIR', 'OMP_PROFILE') if key in env}
    request = {'argv': argv, 'cwd': cwd, 'env': config}
    with registry_lock(state_path):
        state = read_state(state_path)
        if state:
            if not state.get('focus_url'):
                raise ValueError('Warp tab is still opening; try again in a moment')
            # Never send a second CLI into an active agent's terminal input.
            if state.get('busy') or state.get('request'):
                open_uri(state['focus_url'])
                return 'focused'
            open_uri(state['focus_url'])
            state['request'] = request
            write_state(state_path, state)
            return 'reused'
        folder = Path(tempfile.mkdtemp(prefix='ais-warp-launch-'))
        script = folder / 'launch.sh'
        bootstrap = ('import sys; sys.path.insert(0, ' + repr(str(Path(__file__).resolve().parent.parent)) + '); '
                     'from ai_sessions.warp import serve_tab; serve_tab(' + repr(str(state_path)) + ')')
        lines = ['#!/bin/sh', 'cd -- ' + shlex.quote(cwd) + ' || exit 1', 'rm -f -- "$0"', 'rmdir -- ' + shlex.quote(str(folder)),
                 'exec ' + shlex.join([sys.executable, '-c', bootstrap])]
        try:
            script.write_text('\n'.join(lines) + '\n')
            script.chmod(0o700)
            write_state(state_path, {'created': time.time(), 'request': request})
            open_uri(create_tab_config(cwd, scheme, script, state_path.stem))
        except BaseException:
            state_path.unlink(missing_ok=True)
            script.unlink(missing_ok=True)
            folder.rmdir()
            raise
        return 'opened'


def serve_tab(path: str):
    """Keep one terminal per project available after a client exits."""
    state_path = Path(path)
    focus_url = os.environ.get('WARP_FOCUS_URL', '')
    if not focus_url:
        raise ValueError('Warp did not supply the new tab session URL')
    def interrupted(*_):
        # A CLI runs in our foreground process group and receives Ctrl+C too.
        pass
    signal.signal(signal.SIGINT, interrupted)
    signal.signal(signal.SIGQUIT, interrupted)
    with registry_lock(state_path):
        state = read_state(state_path)
        state.update(pid=os.getpid(), focus_url=focus_url, busy=False)
        write_state(state_path, state)
    try:
        while True:
            request = None
            with registry_lock(state_path):
                state = read_state(state_path)
                request = state.pop('request', None)
                if request:
                    state['busy'] = True
                    write_state(state_path, state)
            if not request:
                time.sleep(.1)
                continue
            title = line(Path(request['cwd']).name or request['cwd'])
            os.chdir(request['cwd'])
            print('\033]7;' + Path(request['cwd']).resolve().as_uri() + '\007'
                  + '\033]0;' + title + '\007', end='', flush=True)
            env = os.environ.copy()
            env.pop('OMP_PROFILE', None)
            env.update(request['env'])
            attributes = None
            try:
                import termios
                if sys.stdin.isatty():
                    attributes = termios.tcgetattr(sys.stdin.fileno())
                result = subprocess.run(request['argv'], cwd=request['cwd'], env=env)
                print('\n' + title + ' · exit ' + str(result.returncode) + ' · select another session in ais', flush=True)
            except OSError as exc:
                print(str(exc), file=sys.stderr)
            finally:
                if attributes is not None:
                    termios.tcsetattr(sys.stdin.fileno(), termios.TCSANOW, attributes)
                print('\033[0m\033[?25h\033[?1000l\033[?1002l\033[?1003l\033[?1006l\033[?2004l\033[?1049l'
                      + '\033]0;' + title + '\007', end='', flush=True)
                with registry_lock(state_path):
                    state = read_state(state_path)
                    state['busy'] = False
                    write_state(state_path, state)
    finally:
        with registry_lock(state_path):
            state_path.unlink(missing_ok=True)
