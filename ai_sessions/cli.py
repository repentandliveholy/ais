from __future__ import annotations

from .i18n import preferred_language, save_language, set_language, tr

import argparse
import json
import os
import shlex
import shutil
import signal
import subprocess
import sys
import termios
import time
from pathlib import Path

from .warp import open_tab, warp_scheme
from . import __version__
from .adapters import Collector
from .brand import CLIENT_STYLE, STATUS_STYLE
from .model import CLIENTS, LABELS, STATUSES, Session, age, canonical, filter_sessions, group_projects, line, pretty_path, same_cwd, validate_launch
from .storage import Cache, default_cache
from .ui import RestartRequested, View, browse, render
from .usage import demo_usage, fetch_live_usage, load_usage, render_usage, reset_text, snapshot_text, usage_content


def parser() -> argparse.ArgumentParser:
    p = argparse.ArgumentParser(prog="ais", description=tr('AI Sessions · all conversations, one entry point. Browse local AI CLI history and resume sessions.'))
    p.add_argument("--version", action="version", version=f"ais {__version__}")
    p.add_argument("--lang", choices=("en", "ru"), help=tr("Interface language (saved for future launches)"))
    p.add_argument("-C", "--cwd", type=Path, default=Path.cwd(), help=tr('Project directory (default: cwd)'))
    p.add_argument("-a", "--all", action="store_true", help=tr('History from all projects'))
    p.add_argument("--client", choices=CLIENTS, help=tr('Filter by client'))
    p.add_argument("-q", "--query", default="", help=tr('Search history'))
    p.add_argument("--children", action="store_true", help=tr('Show child sessions'))
    p.add_argument("--archived", action="store_true", help=tr('Include Codex archive'))
    p.add_argument("--list", action="store_true", help=tr('Print a list without the TUI'))
    p.add_argument("--json", action="store_true", help=tr('JSON instead of the TUI'))
    p.add_argument("--doctor", action="store_true", help=tr('Diagnose history sources and installed CLIs'))
    p.add_argument("--sessions", action="store_true", help=tr('Open the Sessions tab instead of Projects'))
    p.add_argument("--usage", action="store_true", help=tr('Open provider usage limits: 5h / week'))
    p.add_argument("--set-deepseek-key", action="store_true",
                   help=tr("Save a DeepSeek API key (hidden input) to show its balance in Usage"))
    p.add_argument("--cached-usage", action="store_true", help=tr('Show only cached usage without launching official CLIs'))
    p.add_argument("--no-auto-refresh", action="store_true", help=tr('Disable periodic history and usage refresh'))
    p.add_argument("--resume", metavar="ID", help=tr('Resume by session ID or unique prefix'))
    p.add_argument("--last", action="store_true", help=tr('Resume the most recent session in the directory (respects --client, --query)'))
    p.add_argument("--stats", action="store_true", help=tr('Summary of all history: clients, projects, models, activity'))
    p.add_argument("-n", "--limit", type=int, default=0, metavar="N", help=tr('Print at most N sessions with --list / --json'))
    p.add_argument("--new", choices=CLIENTS, help=tr('New session in the selected CLI'))
    tabs = p.add_mutually_exclusive_group()
    tabs.add_argument("--new-tab", dest="new_tab", action="store_true", default=None,
                      help=tr("Open sessions in a new Warp tab"))
    tabs.add_argument("--same-tab", dest="new_tab", action="store_false",
                      help=tr("Launch sessions in the current tab"))
    p.add_argument("--no-return", action="store_true", help=tr('Exit ais when the CLI exits, without returning to the list'))
    p.add_argument("--print-command", action="store_true", help=tr('Print the resume command instead of launching'))
    p.add_argument("--home", type=Path, default=Path.home(), help=tr('Home directory for sources (diagnostics)'))
    p.add_argument("--source", action="append", default=[], metavar="CLIENT=PATH", help=tr('Explicit client data directory; may be repeated'))
    p.add_argument("--no-cache", action="store_true", help=tr('Do not read or write the ais index'))
    p.add_argument("--cache-dir", type=Path, help=tr('ais index directory'))
    p.add_argument("--snapshot", type=Path, help=tr('Save the screen as SVG / HTML / TXT'))
    p.add_argument("--cols", type=int, default=144, help=tr('Snapshot width'))
    p.add_argument("--rows", type=int, default=44, help=tr('Snapshot height'))
    p.add_argument("--demo", action="store_true", help=tr('Demo data without reading history'))
    return p


ANSI_COLORS = {"text": 255, "muted": 146, "faint": 60, "accent": 147, "cyan": 117,
               "green": 115, "amber": 222, "red": 211, "blue": 111}


def use_color(stream=sys.stdout) -> bool:
    if os.environ.get("NO_COLOR"):
        return False
    return bool(os.environ.get("FORCE_COLOR")) or (stream.isatty() and os.environ.get("TERM") != "dumb")


def paint(text: str, style: str, *, bold: bool = False, color: bool = True) -> str:
    if not color:
        return text
    return f"\x1b[{'1;' if bold else ''}38;5;{ANSI_COLORS.get(style, 255)}m{text}\x1b[0m"


def print_stats(sessions: list[Session], home: Path, color: bool):
    now = time.time()
    c = lambda text, style="text", bold=False: paint(text, style, bold=bold, color=color)
    print(c(tr("ais · statistics · {count} sessions", count=len(sessions)), "accent", True))
    recent = lambda items, days: sum(s.updated >= now - days * 86400 for s in items)
    print(c(tr("Activity: today {d1} · 7 days {d7} · 30 days {d30}",
               d1=recent(sessions, 1), d7=recent(sessions, 7), d30=recent(sessions, 30)), "muted"))
    print()
    print(c(f"{tr('CLIENT'):10}{tr('SESSIONS'):>10}{tr('7 DAYS'):>10}   {tr('LAST ACTIVE')}", "accent", True))
    for client in CLIENTS:
        items = [s for s in sessions if s.client == client]
        if not items:
            continue
        last = max(s.updated for s in items)
        installed = "" if shutil.which(client) else c("  " + tr("CLI not installed"), "faint")
        print(c(f"{LABELS[client]:10}", CLIENT_STYLE[client], True) + f"{len(items):>10}{recent(items, 7):>10}   " + c(age(last), "muted") + installed)
    projects = sorted(group_projects(sessions), key=lambda p: (-len(p.sessions), -p.updated))[:10]
    if projects:
        print()
        print(c(tr("TOP PROJECTS"), "accent", True))
        for project in projects:
            print(f"{len(project.sessions):>6}  " + c(pretty_path(project.cwd, home), "cyan") + c(" · " + age(project.updated), "muted"))
    models: dict[str, int] = {}
    for s in sessions:
        if s.model:
            models[s.model] = models.get(s.model, 0) + 1
    if models:
        print()
        print(c(tr("TOP MODELS"), "accent", True))
        for model, count in sorted(models.items(), key=lambda kv: -kv[1])[:8]:
            print(f"{count:>6}  {model}")
    statuses: dict[str, int] = {}
    for s in sessions:
        statuses[s.status] = statuses.get(s.status, 0) + 1
    if statuses:
        print()
        print(c(tr("SAVED STATUSES"), "accent", True))
        for status, count in sorted(statuses.items(), key=lambda kv: -kv[1]):
            print(f"{count:>6}  " + c(tr(STATUSES.get(status, status)), STATUS_STYLE.get(status, "muted")))


def stats_dict(sessions: list[Session]) -> dict:
    now = time.time()
    by = lambda attr: {k: sum(getattr(s, attr) == k for s in sessions)
                       for k in dict.fromkeys(getattr(s, attr) for s in sessions if getattr(s, attr))}
    return {"sessions": len(sessions),
            "activity": {f"{d}d": sum(s.updated >= now - d * 86400 for s in sessions) for d in (1, 7, 30)},
            "clients": by("client"), "models": by("model"), "statuses": by("status"),
            "projects": {p.cwd: len(p.sessions) for p in sorted(group_projects(sessions), key=lambda p: -len(p.sessions))}}


def demo_sessions(cwd: str) -> list[Session]:
    now = time.time()
    values = [
        ("omp", tr('One launcher for AI clients'), "gpt-6.1-sol", "ready", tr('Created a colorful session list and client adapters'), 120),
        ("agy", tr('Start screen design'), "Gemini", "closed", tr('Saved the conversation picker layout'), 2400),
        ("claude", tr('Authentication refactoring'), "claude-opus", "awaiting", tr('Clarifying the sign-in method before proceeding'), 7200),
        ("codex", tr('Fixing tests and build'), "gpt-6.1-sol", "ready", tr('Verified tests and launch from another directory'), 86400),
        ("omp", tr('Checking error handling'), "claude-opus", "interrupted", tr('The user interrupted the check'), 172800),
        ("gemini", tr('Project structure review'), "gemini-pro", "saved", tr('Saved a module overview'), 259200),
        ("pi", tr('Installation documentation'), "gpt-6.1-sol", "ready", tr('Updated command examples'), 345600),
    ]
    result = []
    for i, (client, title, model, status, action, ago) in enumerate(values):
        result.append(Session(client, f"019e7000-aaaa-7000-bbbb-{i:012d}", cwd=cwd,
                              title=title, updated=now - ago, created=now - ago - 3700,
                              model=model, provider="openai" if "gpt" in model else "",
                              status=status, status_detail=tr('Last saved record: ') + action,
                              last_action=action, last_user=tr('Build a unified launcher with history from all CLIs in the current directory. Add colors, statuses, models and project switching.'),
                              last_assistant=tr('Done: the list combines client history. The selected session opens in its original CLI and directory. Directory, model and last status are visible at a glance.'),
                              messages=42 + i * 7, tools=18 + i * 3, tokens=34850,
                              branch="main", exact=True, source=tr('Demo data'),
                              path=f"/demo/{client}/session.jsonl"))
    return result


def launch(session: Session | tuple[str, str], *, return_to_ais: bool = False, new_tab: bool = False) -> int:
    if isinstance(session, Session):
        argv = validate_launch(session)
        cwd, env = session.cwd, session.environment()
    else:
        client, cwd = session
        binary = shutil.which(client)
        if not binary:
            raise ValueError(tr('{v0} not found in PATH', v0=client))
        if not Path(cwd).is_dir():
            raise ValueError(tr('Directory does not exist: {v0}', v0=cwd))
        argv, env = [binary], os.environ.copy()
    if new_tab:
        result = open_tab(argv, cwd, env)
        return 1 if result == 'focused' else 0
    # curses.wrapper restores the terminal before either launch mode. Both
    # modes use the client's real terminal and argv, without an extra shell/PTY.
    print(tr('\n  Continuing in {v0} · {v1}\n', v0=Path(argv[0]).name, v1=pretty_path(cwd)), flush=True)
    if not return_to_ais:
        os.chdir(cwd)
        os.execvpe(argv[0], argv, env)
    attributes = None
    if sys.stdin.isatty():
        try:
            attributes = termios.tcgetattr(sys.stdin.fileno())
        except (OSError, termios.error):
            pass
    handlers = {}
    try:
        # Parent and child share the foreground process group. A caught no-op
        # keeps ais waiting when Ctrl+C interrupts a turn in the CLI. Unlike
        # SIG_IGN, a caught handler resets to SIG_DFL when the child execs.
        for signum in (signal.SIGINT, signal.SIGQUIT):
            handlers[signum] = signal.signal(signum, lambda *_: None)
        return subprocess.run(argv, cwd=cwd, env=env).returncode
    finally:
        if attributes is not None:
            try:
                termios.tcsetattr(sys.stdin.fileno(), termios.TCSANOW, attributes)
            except (OSError, termios.error):
                pass
        if sys.stdout.isatty():
            # Also recover from a client that crashed in raw/alternate-screen
            # mode with mouse reporting or bracketed paste still enabled.
            sys.stdout.write("\x1b[0m\x1b[?25h\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1006l\x1b[?1004l\x1b[?2004l\x1b[?1049l")
            sys.stdout.flush()
        for signum, handler in handlers.items():
            signal.signal(signum, handler)


def browser_loop(collector: Collector, view: View,
                 pending: Session | tuple[str, str] | None = None, *, no_return: bool = False, new_tab: bool = False) -> int:
    while True:
        if pending is None:
            pending = browse(collector, view)
        if pending is None:
            return 0
        if isinstance(pending, Session):
            view.selected_key = pending.key
            client = pending.client
        else:
            client = pending[0]
        view.modal = ""
        try:
            code = launch(pending, return_to_ais=not no_return, new_tab=new_tab)
            if new_tab:
                view.message = tr("Project tab is busy; focused existing session: {folder}" if code == 1 else "Opened {client} in Warp tab: {folder}", client=LABELS[client],
                                  folder=Path(pending.cwd if isinstance(pending, Session) else pending[1]).name)
                if no_return:
                    return code
            elif code < 0:
                try:
                    reason = signal.Signals(-code).name
                except ValueError:
                    reason = str(-code)
                view.message = tr('{v0} terminated by signal {v1}', v0=LABELS[client], v1=reason)
            else:
                view.message = tr('Returned from {v0} · exit code {v1}', v0=LABELS[client], v1=code)
        except (OSError, ValueError) as exc:
            view.message = tr('Launching {v0}: {v1}', v0=LABELS[client], v1=line(exc))
        pending = None
        view.loaded, view.busy_detail = False, False
        same_cwd.cache_clear()
        canonical.cache_clear()
        collector.errors.clear()
        for source in collector.sources.values():
            source.count, source.error, source.roots = 0, "", []


def usage_for_output(collector: Collector, *, cached: bool = False):
    usage = load_usage(collector)
    if not cached:
        try:
            return fetch_live_usage(collector)
        except (OSError, ValueError, TimeoutError) as exc:
            usage.errors.append(line(exc))
    return usage


def main(argv: list[str] | None = None) -> int:
    # Select the language before building argparse so --help is localized too.
    language_args = argparse.ArgumentParser(add_help=False)
    language_args.add_argument("--lang", choices=("en", "ru"))
    selected_language, _ = language_args.parse_known_args(argv)
    set_language(selected_language.lang or preferred_language())
    args = parser().parse_args(argv)
    if args.lang:
        try:
            save_language(args.lang)
        except OSError as exc:
            print(tr("Could not save language: {error}", error=line(exc)), file=sys.stderr)
    if args.set_deepseek_key:
        import getpass
        from .usage import save_deepseek_key
        key = getpass.getpass(tr("DeepSeek API key (input hidden, empty to remove): ")).strip()
        if not key:
            from .usage import deepseek_key_path
            deepseek_key_path().unlink(missing_ok=True)
            print(tr("DeepSeek key removed"))
            return 0
        path = save_deepseek_key(key)
        print(tr("Saved to {path} (mode 600). Check it with: ais --usage", path=pretty_path(str(path))))
        return 0
    cwd, home = canonical(str(args.cwd)), args.home.expanduser().resolve()
    if not Path(cwd).is_dir():
        parser().error(tr('Directory does not exist: {v0}', v0=cwd))
    if args.new and args.resume:
        parser().error(tr('--new and --resume cannot be used together'))
    if args.last and (args.new or args.resume):
        parser().error(tr('--last cannot be combined with --new or --resume'))
    if args.print_command and not (args.new or args.resume or args.last):
        parser().error(tr('Use --resume ID or --new CLIENT with --print-command'))
    if args.demo and (args.new or args.resume or args.last):
        parser().error(tr('Demo sessions cannot be launched'))
    return_to_browser = (sys.stdin.isatty() and sys.stdout.isatty() and not args.no_return
                         and not (args.list or args.json or args.doctor or args.stats or args.snapshot or args.demo or args.print_command))
    new_tab = args.new_tab if args.new_tab is not None else bool(warp_scheme() and sys.stdin.isatty() and sys.stdout.isatty() and not args.no_return)
    pending = None
    if args.new:
        if args.print_command:
            print(f"cd {shlex.quote(cwd)} && {args.new}")
            return 0
        elif return_to_browser:
            pending = (args.new, cwd)
        else:
            launch((args.new, cwd), new_tab=new_tab)
            return 0
    overrides: dict[str, list[Path]] = {}
    for source in args.source:
        client, sep, path = source.partition("=")
        if not sep or client not in CLIENTS or not path:
            parser().error(tr('Source must have the form CLIENT=PATH'))
        overrides.setdefault(client, []).append(Path(path).expanduser().resolve())
    cache = Cache(args.cache_dir or default_cache(home), enabled=not args.no_cache and not args.demo)
    collector = Collector(home, cache, overrides)
    view = View(cwd, home, scope="all" if args.all else "cwd", client=args.client or "all",
                query=args.query, children=args.children, archived=args.archived,
                message=tr('Demo data') if args.demo else "",
                tab="usage" if args.usage else "sessions" if args.sessions else "projects")
    view.live_usage = not args.cached_usage
    view.auto_refresh = not args.no_auto_refresh
    output_mode = (args.list or args.json or args.doctor or args.stats or args.snapshot or args.resume or args.last
                   or not (sys.stdin.isatty() and sys.stdout.isatty()))
    if output_mode or args.demo:
        sessions = demo_sessions(cwd) if args.demo else [] if args.usage and not (args.resume or args.last or args.doctor or args.stats) else collector.scan()
        view.loaded = True
        selected = filter_sessions(sessions, cwd, view.scope, view.client, args.query, args.children, args.archived)
        latest = None
        if args.last:
            if not selected:
                raise ValueError(tr('No sessions in {path}; use --all to search every project', path=pretty_path(cwd, home)))
            latest = selected[0]
            if not args.print_command and not return_to_browser:
                print(tr('Resuming the latest session: {title}', title=line(latest.title or latest.id, 80)), file=sys.stderr)
        if args.limit > 0:
            selected = selected[:args.limit]
        if args.resume or latest:
            # An explicit ID may refer to another directory, but client, archive,
            # and subagent filters still apply. Never guess between matches.
            candidates = filter_sessions(sessions, cwd, "all", view.client, "", args.children, args.archived)
            exact = [latest] if latest else [s for s in candidates if s.id == args.resume]
            matches = exact or [s for s in candidates if s.id.startswith(args.resume)]
            if len(matches) != 1:
                raise ValueError(tr('Session not found') if not matches else tr('Ambiguous ID; specify --client or --source CLIENT=PATH'))
            session = matches[0]
            if args.print_command:
                print(f"cd {shlex.quote(session.cwd)} && {session.command()}")
            elif return_to_browser:
                pending = session
            else:
                launch(session, new_tab=new_tab)
        elif args.snapshot:
            screen = render(sessions, view, min(400, max(20, args.cols)), min(200, max(10, args.rows)), collector.errors)
            if args.usage:
                screen = render_usage(screen, demo_usage() if args.demo else usage_for_output(collector, cached=args.cached_usage), message=view.message)
            screen.save(args.snapshot)
            print(args.snapshot.resolve())
        elif args.usage:
            usage = demo_usage() if args.demo else usage_for_output(collector, cached=args.cached_usage)
            if args.json:
                print(json.dumps({"accounts": [a.to_dict() for a in usage.accounts],
                                  "limits": [r.to_dict() for r in usage.limits], "errors": usage.errors, "live": usage.live,
                                  "reports": [r.to_dict() for r in usage.reports], "checked": usage.checked},
                                 ensure_ascii=False, indent=2))
            else:
                color = use_color()
                columns = shutil.get_terminal_size((140, 40)).columns if sys.stdout.isatty() else 140
                print(paint(tr('ais · Usage · latest provider limits\n'), "accent", bold=True, color=color))
                for text, style in usage_content(usage, max(80, columns), time.time()):
                    print(paint(text, style, bold=style == "cyan", color=color) if text else "")
                for r in usage.reports:
                    print(f"{r.provider} · " + (tr('agy CLI · cached') if r.cached else tr('agy CLI · live')) + " · " + snapshot_text(r.updated))
                    print(r.text)
        elif args.stats:
            pool = filter_sessions(sessions, cwd, "all", view.client, args.query, args.children, args.archived)
            if args.json:
                print(json.dumps(stats_dict(pool), ensure_ascii=False, indent=2))
            else:
                print_stats(pool, home, use_color())
        elif args.doctor:
            data = {c: {"installed": shutil.which(c), "sessions": collector.sources[c].count,
                        "roots": collector.sources[c].roots, "error": collector.sources[c].error} for c in CLIENTS}
            if args.json:
                print(json.dumps(data, ensure_ascii=False, indent=2))
            else:
                for c, info in data.items():
                    print(tr('{v0:8} {v1:5} sessions · {v2}', v0=LABELS[c], v1=info['sessions'], v2=info['installed'] or tr('CLI not installed')))
                    for root in info["roots"] or []:
                        print("          " + root)
                    if info["error"]:
                        print(tr('          Error: ') + info["error"])
                print(tr('\nSources are read-only. All statuses are saved records, not live.'))
        elif args.json:
            print(json.dumps([s.to_dict() for s in selected], ensure_ascii=False, indent=2))
        elif args.demo and not output_mode:
            # A demo TUI uses the same renderer and key handling but cannot launch
            # fake conversations. Snapshot mode is the normal visual QA route.
            print(render(sessions, view, args.cols, args.rows).plain())
        else:
            color = use_color()
            print(paint(tr('ais · AI Sessions · {v0} · {v1} sessions\n', v0=pretty_path(cwd, home), v1=len(selected)), "accent", bold=True, color=color))
            for s in selected:
                status = tr(STATUSES.get(s.status, s.status))
                print(paint(f"{LABELS[s.client]:7}", CLIENT_STYLE[s.client], bold=True, color=color)
                      + paint(f" {age(s.updated):>9}  ", "muted", color=color)
                      + paint(f"{status:15}", STATUS_STYLE.get(s.status, "muted"), color=color)
                      + "  " + paint(line(s.title or s.id, 100), "text", bold=True, color=color))
                print("        " + paint(pretty_path(s.cwd, home), "cyan", color=color)
                      + paint(f" · {s.model or tr('model not recorded')} · {s.id}", "muted", color=color))
            for error in collector.errors:
                print(tr('Warning: ') + error, file=sys.stderr)
        if pending is None:
            return 0
    return browser_loop(collector, view, pending, no_return=args.no_return, new_tab=new_tab)


def entrypoint():
    try:
        try:
            raise SystemExit(main())
        except RestartRequested:
            os.execv(sys.executable, [sys.executable, *sys.argv])
    except KeyboardInterrupt:
        raise SystemExit(130)
    except (ValueError, OSError) as exc:
        print(f"ais: {line(exc)}", file=sys.stderr)
        raise SystemExit(1)
