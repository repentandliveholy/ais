from __future__ import annotations

from .i18n import LANGUAGES, language, read_settings, save_language, save_settings, set_language, tr

import curses
import locale
import queue
import re
import shlex
import shutil
import sqlite3
import subprocess
import sys
import threading
import time
from concurrent.futures import CancelledError, ThreadPoolExecutor
from collections import deque
from dataclasses import dataclass, field
from typing import NamedTuple
from datetime import datetime
from pathlib import Path

from .actions import copy_text, reveal
from .adapters import Collector
from .brand import CLIENT_STYLE, PALETTE, STATUS_STYLE, WORDMARK
from .model import CLIENTS, LABELS, STATUSES, Session, Project, folder_entries, group_projects, age, canonical, clean, filter_sessions, line, number, parse_query, pretty_path, same_cwd, validate_launch
from .screen import Screen, clip, width, wrap
from .screen import width as width_of
from .usage import Usage, fetch_live_usage, load_usage, max_usage_scroll, render_usage, usage_strip_lines


@dataclass
class View:
    cwd: str
    home: Path
    scope: str = "cwd"
    client: str = "all"
    query: str = ""
    selected: int = 0
    selected_key: str = ""
    top: int = 0
    detail_scroll: int = 0
    children: bool = False
    archived: bool = False
    searching: bool = False
    loaded: bool = False
    busy_detail: bool = False
    message: str = ""
    modal: str = ""
    new_client: int = 0
    language_index: int = 0
    new_cwd: str = ""
    narrow_detail: bool = False
    # The CLI opens Projects explicitly; library callers keep the session list.
    tab: str = "sessions"
    usage_scroll: int = 0
    live_usage: bool = True
    auto_refresh: bool = True
    project_path: str | None = None
    project_index: int = 0
    project_top: int = 0
    tree: bool = False
    folder_path: str | None = None
    folder_stack: list[tuple[str | None, int, int]] = field(default_factory=list)
    usage_tab: str = "sessions"
    usage_project: str | None = None
    pins: set[str] = field(default_factory=set)
    help_scroll: int = 0
    # Usage survives returning from a CLI, so the strip never starts empty.
    usage: Usage | None = None
    usage_live_at: float = 0
    usage_refreshing: bool = False


HISTORY_REFRESH_SECONDS = 10
USAGE_REFRESH_SECONDS = 60
# The strip refreshes rarely: each refresh runs the official CLIs.
STRIP_REFRESH_SECONDS = 300
RESTART_KEY_SECONDS = .5
SCROLL_STEP_INTERVAL = .04
BACKSPACE_KEYS = (curses.KEY_BACKSPACE, "\x7f", "\b")


DOUBLE_CLICK_SECONDS = .45


class Click(NamedTuple):
    """A left-button press at a 0-based terminal cell."""
    x: int
    y: int


class RestartRequested(Exception):
    """Restart only after curses and background workers have shut down."""


@dataclass
class WheelScroll:
    moved_at: float = float("-inf")
    direction: int | None = None

    def step(self, direction: int, now: float) -> int:
        # Limit actual moves, not input reports. Ignored reports neither queue
        # motion nor postpone the next allowed step during continuous scrolling.
        if direction == self.direction and now - self.moved_at < SCROLL_STEP_INTERVAL:
            return 0
        self.moved_at, self.direction = now, direction
        return 1


def date(value: float) -> str:
    try:
        return datetime.fromtimestamp(value).strftime("%d.%m.%Y %H:%M:%S") if value else "—"
    except (ValueError, OverflowError, OSError, TypeError):
        return "—"


def list_page_size(cols: int, rows: int) -> int:
    header = 5 if rows >= 26 and cols >= 70 else 4
    row_height = 4 if rows >= 26 else 3
    # One blank line separates items; the last item needs no trailing gap.
    return max(1, (rows - 4 - (header + 7) + 1) // (row_height + 1))


def keep_selection_visible(view: View, count: int, page_size: int):
    view.selected = max(0, min(view.selected, count - 1))
    view.top = max(0, min(view.top, count - page_size))
    if view.selected < view.top:
        view.top = view.selected
    elif view.selected >= view.top + page_size:
        view.top = view.selected - page_size + 1


def pinned_first(items: list, pins: set[str]) -> list:
    # Stable: pinned rows keep their recency order above the others.
    return sorted(items, key=lambda e: e.key not in pins) if pins else items


def visible_entries(sessions: list[Session], v: View) -> list[Session | Project]:
    if v.tab == "projects":
        if v.folder_path is not None:
            entries = folder_entries(sessions, v.folder_path, v.client, v.query, v.children, v.archived, v.pins)
            folders = [e for e in entries if isinstance(e, Project)]
            own = [e for e in entries if not isinstance(e, Project)]
            return [*pinned_first(folders, v.pins), *pinned_first(own, v.pins)]
        projects = group_projects(sessions, v.client, v.query, v.children, v.archived, tree=v.tree, pins=v.pins)
        return projects if v.tree else pinned_first(projects, v.pins)
    return pinned_first(filter_sessions(scoped_sessions(sessions, v), v.cwd,
                                        "all" if v.project_path is not None else v.scope,
                                        v.client, v.query, v.children, v.archived, v.pins), v.pins)


def draw_usage_strip(screen: Screen, usage: Usage | None, x: int, top: int, bottom: int,
                     width: int, refreshing: bool = False):
    for y in range(top, bottom):
        screen.put(y, x, "│", "faint")
    cx, cw = x + 2, width - 4
    title = tr("LIMITS")
    screen.put(top, cx, title, "accent", bold=True, limit=cw)
    if refreshing:
        screen.put(top, cx + width_of(title) + 1, "↻", "cyan")
    screen.hit(top, x, width, ("tab", "usage"), h=bottom - top)
    if usage is None or not usage.loaded:
        screen.put(top + 2, cx, tr("loading…"), "muted", limit=cw)
        return
    y = top + 2
    for parts in usage_strip_lines(usage, cw, time.time()):
        if y >= bottom - 1:
            break
        screen.spans(y, cx, parts, limit=cw)
        y += 1
    screen.put(bottom - 1, cx, tr("U — full usage"), "faint", limit=cw)


def put_highlighted(screen: Screen, y: int, x: int, text: str, style: str, words: list[str], *,
                    selected: bool = False, bold: bool = False, limit: int | None = None):
    """Put text, then repaint plain-word search matches in amber."""
    screen.put(y, x, text, style, selected=selected, bold=bold, limit=limit)
    if not words or (limit is not None and limit <= 0):
        return
    shown = clip(clean(text).replace("\n", " ").replace("\t", " "),
                 min(screen.cols - max(x, 0), limit if limit is not None else screen.cols))
    folded = shown.casefold()
    if len(folded) != len(shown):
        return  # Case folding changed lengths (for example ß); skip safely.
    for word in words:
        start = 0
        while word and (index := folded.find(word, start)) >= 0:
            screen.put(y, x + width(shown[:index]), shown[index:index + len(word)], "amber",
                       selected=selected, bold=True)
            start = index + len(word)


def copy_payload(entry: Session | Project, identity: bool = False) -> str:
    if isinstance(entry, Project):
        return entry.cwd
    if identity:
        return entry.id
    command = entry.command()
    return f"cd {shlex.quote(entry.cwd)} && {command}" if entry.cwd else command


def load_pins() -> set[str]:
    pins = read_settings().get("pins")
    return {str(p) for p in pins} if isinstance(pins, list) else set()


def scoped_sessions(sessions: list[Session], v: View) -> list[Session]:
    if v.project_path is None:
        return sessions
    return [s for s in sessions if
            (s.in_cwd(v.project_path) if v.project_path else not s.cwd and not s.workspaces)]


def open_projects(v: View, *, root: bool = False):
    if root:
        v.folder_path, v.folder_stack = None, []
        v.project_index, v.project_top = 0, 0
    v.tab, v.project_path = "projects", None
    v.selected, v.top = v.project_index, v.project_top
    v.query, v.searching, v.narrow_detail = "", False, False
    v.selected_key = ""


def enter_folder(v: View, project: Project):
    v.folder_stack.append((v.folder_path, v.selected, v.top))
    v.folder_path = project.cwd
    v.query, v.searching = "", False
    v.selected, v.top, v.detail_scroll = 0, 0, 0


def leave_folder(v: View):
    if v.folder_stack:
        v.folder_path, v.selected, v.top = v.folder_stack.pop()
    else:
        v.folder_path, v.selected, v.top = None, 0, 0
    v.query, v.searching, v.detail_scroll = "", False, 0


def enter_project(v: View, project: Project):
    v.project_index, v.project_top = v.selected, v.top
    v.tab, v.project_path = "sessions", project.cwd
    v.query, v.searching = "", False
    v.selected, v.top, v.detail_scroll = 0, 0, 0


def open_usage(v: View):
    if v.tab != "usage":
        v.usage_tab, v.usage_project = v.tab, v.project_path
    v.tab, v.message = "usage", ""


def close_usage(v: View):
    v.tab = v.usage_tab if v.usage_tab != "usage" else "sessions"
    v.project_path = v.usage_project
    v.message = ""


def project_detail_lines(project: Project, maximum: int, home: Path, expanded: bool = False) -> list[tuple[str, str]]:
    result: list[tuple[str, str]] = []

    def add(text: str, style="text"):
        result.extend((row, style) for row in wrap(text, maximum))

    add(Path(project.cwd).name or tr('No directory'), "green")
    add(tr('DIRECTORY'), "muted")
    add(pretty_path(project.cwd, home), "cyan")
    add(tr("{count} sessions · {elapsed}", count=len(project.sessions), elapsed=age(project.updated)), "muted")
    add(tr('Updated  ') + date(project.updated), "muted")
    add("")
    for client in CLIENTS:
        count = sum(s.client == client for s in project.sessions)
        if count:
            add(f"{LABELS[client]}  {count}", CLIENT_STYLE[client])
    if expanded:
        created = [s.created for s in project.sessions if s.created]
        if created:
            add(tr('Created  ') + date(min(created)), "muted")
        add("")
        add(tr('SAVED STATUSES'), "accent")
        for status in dict.fromkeys(s.status for s in project.sessions):
            add(tr(STATUSES.get(status, status)) + "  " + str(sum(s.status == status for s in project.sessions)), STATUS_STYLE.get(status, "muted"))
        for label, values in ((tr('MODEL / PROVIDER'), [" / ".join(v for v in (s.model, s.provider) if v) for s in project.sessions]),
                              (tr('BRANCHES'), [s.branch for s in project.sessions])):
            recorded = [value for value in values if value]
            if recorded:
                add("")
                add(label, "accent")
                for value in dict.fromkeys(recorded):
                    add(f"{value}  · {recorded.count(value)}", "cyan")
    add("")
    add(tr('RECENT SESSIONS'), "accent")
    recent = sorted(project.sessions, key=lambda s: s.updated, reverse=True)
    for session in recent if expanded else recent[:3]:
        add(session.title or session.id if expanded else clip(session.title, maximum))
        add(LABELS[session.client] + " · " + tr(STATUSES.get(session.status, session.status)) + " · " + age(session.updated), "muted")
        if expanded:
            if session.model or session.branch:
                add(" · ".join(v for v in (session.model, session.branch) if v), "cyan")
            preview = session.last_action or session.last_assistant or session.last_user or session.status_detail
            if preview:
                add("↳ " + line(preview), "muted")
            add("ID  " + session.id, "muted")
            add("")
    return result


def detail_lines(s: Session | Project, maximum: int, home: Path) -> list[tuple[str, str]]:
    if isinstance(s, Project):
        return project_detail_lines(s, maximum, home, expanded=True)
    result: list[tuple[str, str]] = []

    def add(text: str, style="text"):
        result.extend((r, style) for r in wrap(text, maximum))

    def field(label: str, value: str, style="text"):
        add(label, "muted")
        add(value or tr('Not recorded'), style)

    add(LABELS[s.client] + (f"  /  {s.profile}" if s.profile else ""), CLIENT_STYLE[s.client])
    title_rows = wrap(s.title, maximum)
    result.extend((r, "text") for r in title_rows[:2])
    if len(title_rows) > 2:
        result[-1] = (clip(result[-1][0], max(1, maximum - 1)) + "…", "text")
    add("")
    field(tr('DIRECTORY'), pretty_path(s.cwd, home), "cyan")
    if len(s.workspaces) > 1:
        for cwd in s.workspaces[1:]:
            add(pretty_path(cwd, home), "cyan")
    field(tr('MODEL / PROVIDER'), " · ".join(v for v in [s.model, s.provider] if v))
    field(tr('LAST SAVED STATUS'), tr(STATUSES.get(s.status, s.status)), STATUS_STYLE.get(s.status, "muted"))
    if s.status_detail:
        add(s.status_detail, "muted")
    if s.last_action:
        field(tr('LAST ACTION'), s.last_action)
    add("")
    if s.steps is not None:
        add(tr('{v0} steps · {v1} tokens', v0=s.steps, v1=number(s.tokens)), "muted")
    else:
        prefix = "" if s.exact else "≥"
        add(tr('{v0}{v1} messages · {v2}{v3} tool calls · {v4} tokens', v0=prefix, v1=s.messages, v2=prefix, v3=s.tools, v4=number(s.tokens)), "muted")
    if s.cost is not None:
        add(tr('Cost reported by client: ${v0:.4f}', v0=s.cost), "muted")
    if not s.exact:
        add(tr('Preview from the start and end of the file; details load when selected.'), "muted")
    if s.branch:
        add("Git: " + s.branch, "green")
    add(tr('Updated  ') + date(s.updated), "muted")
    add("ID  " + s.id, "muted")
    if s.parent:
        field(tr('PARENT SESSION'), s.parent, "muted")
    if s.todo:
        add("")
        add(tr('TASKS'), "accent")
        for task in s.todo:
            done = task.get("status") in ("completed", "done")
            add(("✓ " if done else "◦ ") + task.get("text", ""), "green" if done else "text")
    add("")
    add(tr('LAST PROMPT'), "blue")
    add(s.last_user or tr('Text is not saved in the available index'), "muted")
    add("")
    add(tr('LAST RESPONSE'), "green")
    add(s.last_assistant or tr('Text is not saved in the available index'))
    add("")
    add(tr('RESUME IN ') + LABELS[s.client].upper(), CLIENT_STYLE[s.client])
    add(s.command(), "cyan")
    if not shutil.which(s.client):
        add(tr('CLI not found in PATH'), "amber")
    if not s.cwd or not Path(s.cwd).is_dir():
        add(tr('Original directory unavailable'), "amber")
    if s.note:
        add(s.note, "muted")
    add("")
    field(tr('SOURCE'), s.source or s.client, "muted")
    add(tr('Created  ') + date(s.created), "muted")
    add(pretty_path(s.path, home), "muted")
    return result


STRIP_WIDTH = 27
STRIP_MIN_COLS = 150


def layout(cols: int, view: View) -> tuple[bool, int, int]:
    """(wide, list width, usage strip width): identical for Projects and Sessions."""
    wide = cols >= 112 and not view.narrow_detail
    strip = STRIP_WIDTH if wide and cols >= STRIP_MIN_COLS else 0
    left = max(54, int((cols - strip) * .52)) if wide else cols - 3
    return wide, left, strip


def detail_width(cols: int, view: View) -> int:
    wide, left, strip = layout(cols, view)
    if not wide:
        return cols - 6
    return (cols - strip - 1) - (left + 1) - 4 if strip else cols - left - 7


def detail_scroll_limit(session: Session | Project | None, view: View, cols: int, rows: int) -> int:
    if session is None or (isinstance(session, Project) and not view.narrow_detail):
        return 0
    header = 5 if rows >= 26 and cols >= 70 else 4
    wide = layout(cols, view)[0]
    text_width = detail_width(cols, view)
    visible_rows = max(1, rows - 4 - (header + 7) - (1 if wide else 0))
    return max(0, len(detail_lines(session, max(4, text_width), view.home)) - visible_rows)


def render(sessions: list[Session], v: View, cols: int, rows: int, errors: list[str] | None = None,
           usage: Usage | None = None) -> Screen:
    screen = Screen(cols, rows)
    if cols < 44 or rows < 16:
        screen.put(1, 1, "ais · AI Sessions", "accent", bold=True)
        screen.put(3, 1, tr('Enlarge terminal: minimum 44 × 16'), "amber")
        screen.put(5, 1, tr('Q / Ctrl+C — quit'), "muted")
        return screen
    wide = cols >= 112 and not v.narrow_detail
    if rows >= 26 and cols >= 70:
        for i, row in enumerate(WORDMARK):
            # A, i, and s keep their own colors in the terminal wordmark.
            for x, c in enumerate(row):
                screen.put(i + 1, x + 2, c, "cyan" if x < 6 else "accent" if x < 10 else "green", bold=True)
        screen.put(1, 22, "AI SESSIONS", "text", bold=True)
        screen.put(2, 22, tr('All conversations. One entry point.'), "muted")
        screen.put(3, 22, pretty_path(v.cwd, v.home), "cyan", limit=cols - 25)
        header = 5
    else:
        screen.put(1, 2, "◈ ais", "accent", bold=True)
        screen.put(1, 10, "AI SESSIONS", "text", bold=True)
        screen.put(2, 2, pretty_path(v.cwd, v.home), "cyan", limit=cols - 5)
        header = 4
    screen.rule(header - 1)
    tabs = (("projects", tr(" Projects [P] "), "green"),
            ("sessions", tr(' Sessions [S] '), "text"),
            ("usage", " Usage [U] ", "cyan"))
    x = 2
    for name, label, style in tabs:
        text = label if cols >= 65 else f" [{name[0].upper()}] "
        screen.put(header + 1, x, text, style, selected=v.tab == name, bold=v.tab == name)
        screen.hit(header + 1, x, width(text), ("tab", name))
        x += width(text) + 1
    screen.put(header + 1, cols - 10, f"{language().upper()} [L]", "muted", limit=8)
    screen.hit(header + 1, cols - 10, 6, ("key", "L"))
    x = 2
    for name, label in (("cwd", tr('Current directory')), ("all", tr('All projects'))):
        chosen = v.scope == name and v.tab != "projects" and v.project_path is None
        text = (" ● " if chosen else " ○ ") + label + " "
        screen.put(header, x, text, "accent" if chosen else "muted", selected=chosen, bold=chosen)
        screen.hit(header, x, width(text), ("scope", name))
        x += width(text) + 2
    hint = tr('Tab — switch')
    if cols >= 80:
        screen.put(header, cols - width(hint) - 3, hint, "muted")
    counted = filter_sessions(scoped_sessions(sessions, v), v.cwd,
                              "all" if v.tab == "projects" or v.project_path is not None else v.scope,
                              children=v.children, archived=v.archived)
    counts = {c: sum(s.client == c for s in counted) for c in CLIENTS}
    x = 2
    clients = ["all", *CLIENTS]
    for client in clients:
        label = tr('All') if client == "all" else LABELS[client]
        count = sum(counts.values()) if client == "all" else counts[client]
        text = f" {label} {count} "
        if x + width(text) >= cols - 1:
            break
        chosen = v.client == client
        screen.put(header + 2, x, text, "text" if client == "all" else CLIENT_STYLE[client], selected=chosen, bold=chosen)
        screen.hit(header + 2, x, width(text), ("client", client))
        x += width(text) + 1
    if x + 14 < cols - 1 and cols >= 100:
        screen.put(header + 2, x + 1, tr("0–6 · ←→"), "faint")
    flags = []
    if v.children:
        flags.append((tr("◆ child sessions"), "cyan"))
    if v.archived:
        flags.append((tr("◆ archive"), "cyan"))
    if v.tree and v.tab == "projects":
        flags.append((tr("◆ tree"), "green"))
    flags_text = "  ".join(text for text, _ in flags)
    flags_room = width(flags_text) + 3 if flags and cols >= 80 else 0
    if v.query or v.searching:
        search = "⌕ " + v.query + ("▏" if v.searching else "")
    else:
        search = tr('⌕ Search: title, directory, model, ID, last message…') + (
            tr("   client: model: status: since:3d -word") if cols - flags_room >= 120 else "")
    screen.put(header + 4, 2, search, "text" if v.query or v.searching else "muted", limit=cols - 4 - flags_room)
    screen.hit(header + 4, 2, cols - 4 - flags_room, ("search",))
    if flags_room:
        screen.spans(header + 4, cols - width(flags_text) - 2,
                     [part for flag in flags for part in (flag, ("  ", "faint"))][:-1])
    words = parse_query(v.query).highlights
    screen.rule(header + 5)
    filtered = visible_entries(sessions, v)
    keep_selection_visible(v, len(filtered), list_page_size(cols, rows))
    selected = v.selected
    current = filtered[selected] if filtered else None
    v.detail_scroll = max(0, min(v.detail_scroll, detail_scroll_limit(current, v, cols, rows)))
    start, bottom = header + 7, rows - 4
    wide, left_width, strip = layout(cols, v)
    title = tr('SESSIONS  {v0}', v0=len(filtered)) + (f" / {len(sessions)}" if len(filtered) != len(sessions) else "")
    if v.tab == "projects":
        title = tr("PROJECTS  {v0}", v0=len(filtered))
        if v.folder_path is not None:
            title += " · " + pretty_path(v.folder_path, v.home)
    elif v.project_path is not None:
        title = pretty_path(v.project_path, v.home) + " · " + title
    if not v.loaded:
        title += tr('  ·  loading history…')
    screen.put(start - 1, 2, title, "muted", bold=True, limit=left_width - 3)
    if v.narrow_detail and current:
        for i, (text, style) in enumerate(detail_lines(current, cols - 6, v.home)[v.detail_scroll:]):
            if start + i >= bottom:
                break
            screen.put(start + i, 3, text, style, limit=cols - 6)
    elif filtered:
        row_height = 4 if rows >= 26 else 3
        capacity = list_page_size(cols, rows)
        top = v.top
        for index, session in enumerate(filtered[top:top + capacity], start=top):
            y = start + (index - top) * (row_height + 1)
            chosen = index == selected
            for sub in range(row_height):
                screen.fill(y + sub, 2, left_width - 3, selected=chosen)
            screen.put(y, 2, "▎" if chosen else " ", "accent", selected=chosen)
            if isinstance(session, Project):
                prefix = clip(session.prefix, max(0, left_width - 24), ellipsis=False)
                indent = width(prefix)
                name_x = 4 + indent
                if prefix:
                    screen.put(y, 4, prefix, "faint", selected=chosen)
                if session.key in v.pins:
                    screen.put(y, name_x, "★", "amber", selected=chosen, bold=True)
                    name_x += 2
                    indent += 2
                put_highlighted(screen, y, name_x, Path(session.cwd).name or tr('No directory'), "text", words,
                                selected=chosen, bold=True, limit=max(1, left_width - 8 - indent))
                if session.children and not v.tree:
                    nested = tr("  ▸ subfolders: {count}", count=len(session.children))
                    screen.put(y, min(left_width - 4, name_x + width(Path(session.cwd).name) + 1), nested, "faint",
                               selected=chosen, limit=max(0, left_width - name_x - width(Path(session.cwd).name) - 5))
                put_highlighted(screen, y + 1, name_x, pretty_path(session.cwd, v.home), "cyan", words,
                                selected=chosen, limit=max(1, left_width - 7 - indent))
                counts = []
                for c in CLIENTS:
                    total = sum(s.client == c for s in session.sessions)
                    if total:
                        counts += [(f"{LABELS[c]} {total}", CLIENT_STYLE[c], chosen), (" · ", "faint")]
                counts = counts[:-1]
                summary = tr("{count} sessions · {elapsed}", count=len(session.sessions), elapsed=age(session.updated))
                room = max(1, left_width - 8 - indent)
                if row_height == 4:
                    screen.put(y + 2, name_x, summary, "muted", selected=chosen, limit=room)
                    screen.spans(y + 3, name_x, counts, selected=chosen, limit=room)
                else:
                    screen.spans(y + 2, name_x, [(summary + " · ", "muted"), *counts], selected=chosen, limit=room)
                for sub in range(row_height):
                    screen.shade(y + sub, 2, left_width - 3, "stripe" if index % 2 else "row")
                screen.hit(y, 2, left_width - 3, ("row", index), h=row_height)
                continue
            elapsed = age(session.updated)
            title_x = 4
            if session.key in v.pins:
                screen.put(y, 4, "★", "amber", selected=chosen, bold=True)
                title_x = 6
            put_highlighted(screen, y, title_x, session.title or session.id, "text", words, selected=chosen, bold=chosen,
                            limit=left_width - width(elapsed) - 5 - title_x)
            screen.put(y, left_width - width(elapsed) - 2, elapsed, "muted", selected=chosen)
            client = LABELS[session.client]
            screen.put(y + 1, 4, client, CLIENT_STYLE[session.client], selected=chosen, bold=True)
            state = tr(STATUSES.get(session.status, session.status))
            screen.put(y + 1, 13, "● " + state, STATUS_STYLE.get(session.status, "muted"), selected=chosen, limit=21)
            if left_width >= 64:
                screen.put(y + 1, 36, session.model or tr('model not recorded'), "muted", selected=chosen, limit=left_width - 39)
            location = pretty_path(session.cwd, v.home) + (f" · {session.branch}" if session.branch else "")
            put_highlighted(screen, y + 2, 4, location, "cyan", words, selected=chosen, limit=left_width - 7)
            if row_height == 4:
                preview = session.last_action or session.last_assistant or session.last_user
                put_highlighted(screen, y + 3, 4, "↳ " + line(preview or session.status_detail), "muted", words,
                                selected=chosen, limit=left_width - 7)
            for sub in range(row_height):
                screen.shade(y + sub, 2, left_width - 3, "stripe" if index % 2 else "row")
            screen.hit(y, 2, left_width - 3, ("row", index), h=row_height)
        if len(filtered) > capacity:
            position = f"{selected + 1} / {len(filtered)}"
            screen.put(start - 1, left_width - width(position) - 3, position, "muted")
    else:
        screen.put(start + 1, 4, tr('No matching sessions yet'), "text", bold=True)
        screen.put(start + 3, 4, tr('Tab — all projects   ·   / — search   ·   N — new'), "muted", limit=left_width - 6)
        if v.query:
            screen.put(start + 5, 4, tr('Esc — clear search'), "muted")
        screen.put(start + 7 if v.query else start + 5, 4, tr('? — search syntax and all keys'), "faint", limit=left_width - 6)
    if wide:
        project = filtered[selected] if filtered and isinstance(filtered[selected], Project) else None
        divider = left_width + 1
        for y in range(start - 1, bottom):
            screen.put(y, divider, "│", "faint")
        dx, dw = divider + 3, detail_width(cols, v)
        screen.put(start - 1, dx, tr('DETAILS') + (tr(' · loading…') if v.busy_detail else ""), "accent", bold=True, limit=dw)
        if project:
            for i, (text, style) in enumerate(project_detail_lines(project, dw, v.home)):
                if start + i >= bottom - 1:
                    break
                screen.put(start + i, dx, text, style, limit=dw)
            screen.put(bottom - 1, dx, tr('D expand · O sessions · E folder · F pin'), 'muted', limit=dw)
        elif current:
            details = detail_lines(current, dw, v.home)
            for i, (text, style) in enumerate(details[v.detail_scroll:]):
                if start + i >= bottom - 1:
                    break
                screen.put(start + i, dx, text, style, limit=dw)
            screen.put(bottom - 1, dx, tr('[ ] scroll · D expand · Y copy · F pin'), "muted", limit=dw)
        else:
            screen.put(start + 1, dx, tr('Select a session on the left'), "muted", limit=dw)
        if strip:
            draw_usage_strip(screen, usage, cols - strip, start - 1, bottom, strip, v.usage_refreshing)
    screen.rule(rows - 4)
    primary = tr('↑↓ select   Enter resume   Tab directory/all   ←→ client   / search') if cols >= 75 else tr('↑↓ select  Enter run  Tab scope  / search')
    if v.tab == "projects":
        primary = tr("↑↓ select   Enter open   O folder sessions   ←→ client   / search")
    secondary = (tr('N new   F pin   Y copy   E folder   D details   R refresh   ? help   Esc back   Q quit') if cols >= 100 else
                 tr('N new  F pin  Y copy  D details  ? help  Esc back  Q quit'))
    if v.tab == "projects":
        secondary = (tr("T tree   F pin   E folder   D details   S sessions   U usage   ? help   Esc back   Q quit")
                     if cols >= 100 else
                     tr("T tree  F pin  D details  S sessions  ? help  Esc back  Q quit"))
    screen.put(rows - 3, 2, primary, "text", limit=cols - 4)
    screen.put(rows - 2, 2, secondary, "muted", limit=cols - 4)
    note = v.message or (tr('{v0} read errors · ? details', v0=len(errors)) if errors else tr('Statuses saved in history · source files are read-only'))
    screen.put(rows - 1, 2, note, "amber" if v.message or errors else "faint", limit=cols - 4)
    if v.modal:
        draw_modal(screen, v, current, errors or [])
    return screen


HELP_SECTIONS = (
    ("NAVIGATION", (
        ("↑↓  j k", "select"),
        ("PgUp PgDn", "page up / down"),
        ("Home End  g G", "first / last"),
        ("Enter", "open folder · resume session"),
        ("Esc  Backspace", "back · reset filters"),
        ("Tab", "current directory / all projects"),
        ("← →  0–6", "filter by client"),
        ("Mouse", "click select · double-click open"),
    )),
    ("ACTIONS", (
        ("N", "new session in current directory"),
        ("Shift+N", "new session in selected directory"),
        ("F", "pin / unpin (pinned stay on top)"),
        ("y", "copy resume command"),
        ("Y", "copy session ID / folder path"),
        ("E", "open folder in file manager"),
        ("O", "open the selected folder sessions"),
    )),
    ("VIEW", (
        ("P  S  U", "projects · sessions · usage"),
        ("T", "project tree on / off"),
        ("D  [ ]", "expand details · scroll"),
        ("C  A", "child sessions · Codex archive"),
        ("L", "language: English / Russian"),
        ("R  rr", "refresh · restart ais"),
        ("Q  Ctrl+C", "quit"),
    )),
    ("SEARCH  / or .", (
        ("word  -word", "must contain / must not contain"),
        ("client:claude", "also c: · cli:"),
        ("model:opus", "model or provider · m:"),
        ("status:error", "saved status · st:"),
        ("branch:main  dir:api", "git branch · directory (b: in:)"),
        ("since:3d", "90m · 6h · 2w · today · yesterday"),
        ("is:pinned", "is:child · is:archived"),
        ("Ctrl+U  Ctrl+W", "clear query · delete word"),
    )),
)


def help_lines(columns: int) -> list[list[tuple[str, str]]]:
    """Help as rows of (text, style) parts; two sections per row when wide."""
    def section(index: int, key_width: int) -> list[list[tuple[str, str]]]:
        title, rows = HELP_SECTIONS[index]
        result = [[(tr(title), "title")]]
        for key, text in rows:
            result.append([(key.ljust(key_width), "cyan"), (tr(text), "text")])
        return result

    if columns == 2:
        left = section(0, 16) + [[]] + section(1, 16)
        right = section(2, 22) + [[]] + section(3, 22)
        return [(left[i] if i < len(left) else []) + [("\t", "")] + (right[i] if i < len(right) else [])
                for i in range(max(len(left), len(right)))]
    rows = []
    for index in range(len(HELP_SECTIONS)):
        rows += section(index, 22 if index >= 2 else 16) + [[]]
    return rows[:-1]


def draw_help(screen: Screen, v: View, errors: list[str]):
    two = screen.cols >= 104
    w = min(120 if two else 84, screen.cols - 6)
    body = help_lines(2 if two else 1)
    footer = [[]]
    footer.append([(tr('Client colors: '), "muted")] + [part for c in CLIENTS for part in ((LABELS[c], CLIENT_STYLE[c]), ("  ", "muted"))])
    footer.append([(tr('Statuses describe the last saved record.'), "muted")])
    if errors:
        footer += [[], [(tr('READ ERRORS'), "amber")]] + [[(line(e), "amber")] for e in errors[:4]]
    lines = body + footer
    viewport = max(3, screen.rows - 8)
    v.help_scroll = max(0, min(v.help_scroll, len(lines) - viewport))
    shown = lines[v.help_scroll:v.help_scroll + viewport]
    height = len(shown) + 4
    x, y = (screen.cols - w) // 2, max(1, (screen.rows - height) // 2)
    screen.hit(0, 0, screen.cols, ("close",), h=screen.rows)
    screen.hit(y, x, w, ("noop",), h=height)
    for i in range(height):
        screen.fill(y + i, x, w, selected=True)
    screen.put(y + 1, x + 2, tr('AI SESSIONS · CONTROLS'), "accent", selected=True, bold=True, limit=w - 4)
    more = len(lines) > viewport
    hint = (tr('↑↓ scroll · Esc / ? close') if more else tr('Esc / ? — close'))
    screen.put(y + 1, x + w - width(hint) - 2, hint, "muted", selected=True)
    column = (w - 4) // 2 + 2
    for i, parts in enumerate(shown):
        px = x + 2
        for text, style in parts:
            if text == "\t":
                px = x + column
                continue
            bold = style == "title"
            style = "accent" if bold else style
            end = x + w - 2 if px >= x + column or not two else x + column - 2
            screen.put(y + 3 + i, px, text, style, selected=True, bold=bold, limit=max(0, end - px))
            px += width(text)


def draw_modal(screen: Screen, v: View, current: Session | None, errors: list[str]):
    if v.modal == "help":
        draw_help(screen, v, errors)
        return
    w = min(84, screen.cols - 8)
    x, y = (screen.cols - w) // 2, max(2, (screen.rows - 18) // 2)
    options: dict[int, tuple] = {}
    if v.modal == "language":
        lines = [(tr("LANGUAGE"), "accent"), ("", "text"),
                 (tr("↑↓ choose · Enter save · Esc cancel"), "muted"), ("", "text")]
        for i, label in enumerate(("English", "Русский")):
            options[len(lines)] = ("lang", i)
            lines.append((("› " if i == v.language_index else "  ") + label, "text"))
    else:
        lines = [(tr('NEW SESSION'), "accent"), ("", "text"),
                 (tr('Choose CLI: ↑↓ or 1–6 · Enter — launch'), "muted"),
                 (tr('Directory: ') + pretty_path(v.new_cwd or v.cwd, v.home), "cyan"), ("", "text")]
        for i, client in enumerate(CLIENTS):
            available = bool(shutil.which(client))
            options[len(lines)] = ("new", i)
            lines.append((("› " if i == v.new_client else "  ") + f"{i + 1}  " + LABELS[client] + ("" if available else tr(' · not installed')), CLIENT_STYLE[client] if available else "faint"))
        lines += [("", "text"), (tr('Esc — cancel'), "muted")]
    lines = lines[:screen.rows - 6]
    screen.hit(0, 0, screen.cols, ("close",), h=screen.rows)
    screen.hit(y, x, w, ("noop",), h=len(lines) + 2)
    for i in range(len(lines) + 2):
        screen.fill(y + i, x, w, selected=True)
    for i, (text, style) in enumerate(lines):
        screen.put(y + i + 1, x + 2, text, style, selected=True, bold=i == 0, limit=w - 4)
        if i in options:
            screen.hit(y + i + 1, x + 1, w - 2, options[i])


def terminal_palette() -> dict[tuple[str, str], int]:
    if not curses.has_colors():
        return {}
    curses.start_color()
    curses.use_default_colors()
    palette = {}
    colors = {"text": 255, "muted": 146, "faint": 60, "accent": 147, "cyan": 117,
              "green": 115, "amber": 222, "red": 211, "blue": 111}
    if curses.COLORS < 256:
        colors = {"text": 7, "muted": 7, "faint": 4, "accent": 5, "cyan": 6,
                  "green": 2, "amber": 3, "red": 1, "blue": 4}
    backgrounds = {"default": 233 if curses.COLORS >= 256 else -1,
                   "row": 235 if curses.COLORS >= 256 else -1,
                   "stripe": 236 if curses.COLORS >= 256 else -1,
                   "selected": 238 if curses.COLORS >= 256 else 0}
    pair = 1
    for name, color in colors.items():
        for background, background_color in backgrounds.items():
            curses.init_pair(pair, color, background_color)
            palette[name, background] = curses.color_pair(pair)
            pair += 1
    return palette


def paint(win, screen: Screen, palette: dict):
    win.erase()
    for y in range(screen.rows):
        for x, value, (style, selected, bold, background) in screen.runs(y):
            state = "selected" if selected else background or "default"
            attr = palette.get((style, state), 0) | (curses.A_BOLD if bold else 0)
            if not palette and selected:
                attr |= curses.A_REVERSE
            try:
                win.addstr(y, x, value, attr)
            except curses.error:
                pass  # The bottom-right cell and resizes can race a repaint.
    win.noutrefresh()
    curses.doupdate()


def enable_mouse():
    """Request wheel events without motion/drag reports or click delays."""
    try:
        curses.mouseinterval(0)
        curses.mousemask(curses.BUTTON1_PRESSED | curses.BUTTON4_PRESSED | wheel_down_mask())
    except curses.error:
        pass  # Keyboard navigation still works on terminals without a mouse.
    if sys.stdout.isatty():
        # Warp forwards wheel reports only in SGR mode. Older macOS ncurses
        # cannot decode that mode, so TerminalInput handles its reports below.
        sys.stdout.write("\x1b[?1007l\x1b[?1000h\x1b[?1006h")
        sys.stdout.flush()


def wheel_down_mask() -> int:
    # macOS's older ncurses ABI exposes wheel-down in this bit but does not
    # export BUTTON5_PRESSED. It also expects X10 rather than SGR reports.
    return getattr(curses, "BUTTON5_PRESSED", curses.REPORT_MOUSE_POSITION)


def wheel_key() -> int | Click | None:
    try:
        _, x, y, _, buttons = curses.getmouse()
    except curses.error:
        return None
    if buttons & curses.BUTTON1_PRESSED:
        return Click(x, y)
    if buttons & curses.BUTTON4_PRESSED:
        return curses.KEY_UP
    if buttons & wheel_down_mask():
        return curses.KEY_DOWN
    return None


class TerminalInput:
    """Decode SGR wheel reports even with macOS's older ncurses ABI."""

    def __init__(self, win):
        self.win = win
        self.pending = deque()
        self.mouse_seen = False

    def read(self, wait_ms=100) -> tuple[object, bool]:
        if self.pending:
            return self.pending.popleft(), False
        key = self.win.get_wch()
        if key == curses.KEY_MOUSE:
            self.mouse_seen = True
            event = wheel_key()
            # Clicks are discrete commands, never part of a wheel burst.
            return event, not isinstance(event, Click)
        if key != "\x1b":
            return key, False
        tail = []
        deadline = time.monotonic() + .03
        try:
            # A single Escape remains a back action. Only CSI < ... M/m is
            # consumed as a mouse report; other input is retained in order.
            for prefix in ("[", "<"):
                self.win.timeout(max(0, int((deadline - time.monotonic()) * 1000)))
                value = self.win.get_wch()
                tail.append(value)
                if value != prefix:
                    break
            else:
                for _ in range(48):
                    self.win.timeout(max(0, int((deadline - time.monotonic()) * 1000)))
                    value = self.win.get_wch()
                    tail.append(value)
                    if not isinstance(value, str):
                        break
                    if value in ("M", "m"):
                        report = re.fullmatch(r"\[<(\d+);(\d+);(\d+)([Mm])", "".join(tail))
                        if report:
                            self.mouse_seen = True
                            button = int(report[1])
                            if report[4] == "M" and button & 195 in (64, 65):
                                return (curses.KEY_DOWN if button & 1 else curses.KEY_UP), True
                            if report[4] == "M" and button & 227 == 0:
                                return Click(int(report[2]) - 1, int(report[3]) - 1), False
                            return None, True
                        break
                    if value not in "0123456789;":
                        break
        except curses.error:
            pass
        finally:
            self.win.timeout(wait_ms)
        self.pending.extend(tail)
        return "\x1b", False


def wheel_burst(win, direction: int, read_input=None) -> tuple[int, tuple[object, bool] | None]:
    """Discard queued wheel motion; keep its latest direction and the next key.

    There is intentionally no event-count cap: leaving part of a burst in
    curses would replay stale motion across subsequent UI frames.
    """
    if read_input is None:
        read_input = TerminalInput(win).read
    win.timeout(0)
    try:
        while True:
            try:
                key, mouse = read_input(0)
            except curses.error:
                break
            if mouse:
                if key is None:
                    continue
                direction = key
            else:
                return direction, (key, False)
        return direction, None
    finally:
        win.timeout(100)


def arrow_burst(win, direction: int, read_input) -> tuple[int, tuple[object, bool] | None]:
    """Coalesce scroll-to-arrow emulation while mouse reports are unavailable."""
    win.timeout(0)
    try:
        while True:
            try:
                key, mouse = read_input(0)
            except curses.error:
                return direction, None
            if mouse or key not in (curses.KEY_UP, curses.KEY_DOWN):
                return direction, (key, mouse)
            direction = key
    finally:
        win.timeout(100)


def browse(collector: Collector, view: View) -> Session | tuple[str, str] | None:
    locale.setlocale(locale.LC_ALL, "")
    messages: queue.Queue = queue.Queue()
    sessions: list[Session] = []
    executor = ThreadPoolExecutor(max_workers=2, thread_name_prefix="ais")
    detail_cache: dict[str, Session] = {}
    requested: set[str] = set()
    usage = view.usage or Usage()
    usage_requested = False
    usage_refreshing = False
    usage_cancel = threading.Event()
    usage_future = None
    restore_key = view.selected_key
    select_first_while_loading = not bool(restore_key)
    refreshing_history = False
    refresh_pending = False
    refresh_to_first = False
    detail_generation = 0
    history_finished_at = None
    usage_finished_at = None
    view.loaded, view.busy_detail = False, False
    if not view.pins:
        view.pins = load_pins()

    def save_pins():
        try:
            save_settings(pins=sorted(view.pins))
        except OSError as exc:
            view.message = tr("Could not save pins: {error}", error=line(exc))

    def scan():
        try:
            result = collector.scan(lambda rows: messages.put(("batch", rows)))
            messages.put(("done", result))
        except Exception as exc:
            messages.put(("error", str(exc)))

    def detail(s: Session, generation: int):
        try:
            messages.put(("detail", (generation, collector.detail(s))))
        except (OSError, ValueError, TypeError, AttributeError) as exc:
            messages.put(("detail_error", (generation, str(exc))))

    def usage_scan(cancel):
        try:
            try:
                messages.put(("usage", load_usage(collector)))
            except (OSError, ValueError, sqlite3.Error) as exc:
                messages.put(("usage", Usage(errors=[str(exc)], loaded=True)))
            if not view.live_usage:
                return
            try:
                messages.put(("usage_live", fetch_live_usage(collector, cancel)))
            except CancelledError:
                return
            except (OSError, ValueError, TimeoutError) as exc:
                if not cancel.is_set():
                    messages.put(("usage_failure", line(exc)))
        finally:
            messages.put(("usage_done", None))

    scan_future = executor.submit(scan)

    def loop(win):
        nonlocal sessions, scan_future, usage, usage_requested, restore_key
        nonlocal usage_refreshing, usage_cancel, usage_future
        nonlocal refreshing_history, refresh_pending, refresh_to_first, detail_generation
        nonlocal select_first_while_loading
        nonlocal history_finished_at, usage_finished_at
        curses.set_escdelay(30)
        try:
            curses.curs_set(0)
        except curses.error:
            pass
        win.keypad(True)
        win.timeout(100)
        enable_mouse()
        try:
            # ncurses otherwise postpones drawing while more terminal input is
            # pending, making a trackpad burst look like delayed scrolling.
            curses.typeahead(-1)
        except curses.error:
            pass
        palette = terminal_palette()
        last_key, chosen_since = "", time.monotonic()
        dirty = True
        last_shape = None
        last_render = 0.0
        from_mouse = False
        pending_input = None
        refresh_key_at = None
        wheel_scroll = WheelScroll()
        terminal_input = TerminalInput(win)
        last_screen: Screen | None = None
        last_click = (float("-inf"), None)
        while True:
            while True:
                try:
                    kind, value = messages.get_nowait()
                except queue.Empty:
                    break
                old = visible_entries(sessions, view)
                old_key = old[min(view.selected, len(old) - 1)].key if old else ""
                if kind in ("batch", "done"):
                    if kind == "batch" and refreshing_history:
                        # Keep the visible list stable until all adapters finish.
                        # A partial batch may not contain the selected client yet.
                        continue
                    sessions = [detail_cache.get(s.key, s) for s in value]
                    wanted_key = restore_key or old_key
                    if select_first_while_loading:
                        # Startup and manual refresh follow the first sorted row.
                        view.selected, view.top = 0, 0
                    elif wanted_key:
                        fresh = visible_entries(sessions, view)
                        found = next((i for i, s in enumerate(fresh) if s.key == wanted_key), None)
                        if found is not None:
                            view.selected = found
                            restore_key = ""
                        elif kind == "done":
                            restore_key = ""
                    view.loaded = kind == "done"
                    if kind == "done":
                        refreshing_history = False
                        select_first_while_loading = False
                        history_finished_at = time.monotonic()
                elif kind == "detail":
                    generation, session = value
                    if generation != detail_generation:
                        continue
                    detail_cache[session.key] = session
                    sessions = [session if s.key == session.key else s for s in sessions]
                    if session.client != "agy":
                        collector.cache.put(Path(session.path), session)
                    view.busy_detail = False
                elif kind == "error":
                    view.loaded, view.message = True, value
                    refreshing_history, restore_key = False, ""
                    select_first_while_loading = False
                    history_finished_at = time.monotonic()
                elif kind == "detail_error":
                    if value[0] != detail_generation:
                        continue
                    view.busy_detail, view.message = False, tr('Details: ') + value[1]
                elif kind == "usage":
                    # Keep the last live report visible while fetching again.
                    if not usage.live or not view.live_usage:
                        usage = value
                elif kind == "usage_live":
                    usage, usage_refreshing = value, False
                    if view.tab == "usage":
                        view.message = ""
                elif kind == "usage_failure":
                    usage_refreshing = False
                    if view.tab == "usage":
                        view.message = value
                    if value not in usage.errors:
                        usage.errors.append(value)
                elif kind == "usage_done":
                    usage_refreshing = False
                    usage_finished_at = time.monotonic()
                view.usage, view.usage_refreshing = usage, usage_refreshing
                dirty = True
            filtered = visible_entries(sessions, view)
            view.selected = max(0, min(view.selected, len(filtered) - 1))
            entry = filtered[view.selected] if filtered else None
            current = entry if isinstance(entry, Session) else None
            if (entry.key if entry else "") != last_key:
                last_key = entry.key if entry else ""
                chosen_since = time.monotonic()
                view.detail_scroll = 0
                view.busy_detail = False
            now = time.monotonic()
            history_due = (view.auto_refresh and view.tab in ("sessions", "projects") and
                           history_finished_at is not None and
                           now - history_finished_at >= HISTORY_REFRESH_SECONDS)
            if (refresh_pending or history_due) and scan_future.done() and not view.busy_detail:
                select_first_while_loading = refresh_to_first
                restore_key = "" if refresh_to_first else entry.key if entry else ""
                view.selected_key = restore_key
                refresh_to_first = False
                view.loaded, view.message = False, ""
                refreshing_history, refresh_pending = True, False
                history_finished_at = None
                detail_generation += 1
                same_cwd.cache_clear()
                canonical.cache_clear()
                detail_cache.clear()
                requested.clear()
                collector.errors.clear()
                for source in collector.sources.values():
                    source.count, source.error, source.roots = 0, "", []
                scan_future = executor.submit(scan)
                dirty = True
            if current and view.loaded and current.key not in requested and time.monotonic() - chosen_since > 0.25:
                requested.add(current.key)
                view.busy_detail = True
                executor.submit(detail, current, detail_generation)
                dirty = True
            h, w = win.getmaxyx()
            if (h, w) != last_shape:
                last_shape, dirty = (h, w), True
            usage_due = (view.auto_refresh and usage_finished_at is not None and
                         now - usage_finished_at >= USAGE_REFRESH_SECONDS)
            # The strip loads the cached snapshot once, then refreshes live rarely.
            strip_due = view.tab != "usage" and layout(w, view)[2] > 0 and (
                not usage.loaded or
                (view.live_usage and (view.auto_refresh or not view.usage_live_at) and
                 time.time() - view.usage_live_at >= STRIP_REFRESH_SECONDS))
            if strip_due and (usage_future is None or usage_future.done()):
                view.usage_live_at = time.time()
            if (((view.tab == "usage" and (not usage_requested or usage_due)) or strip_due) and
                    (usage_future is None or usage_future.done())):
                usage_requested = True
                usage_refreshing = view.live_usage
                usage_finished_at = None
                usage_cancel = threading.Event()
                usage_future = executor.submit(usage_scan, usage_cancel)
                dirty = True
            # Accepted wheel motion is already rate-limited. Paint it right
            # away; deferring to the next input timeout adds visible latency.
            elapsed = time.monotonic() - last_render
            if dirty or elapsed > 15:
                view.usage_refreshing = usage_refreshing
                screen = render(sessions, view, w, h, collector.errors, usage)
                if view.tab == "usage":
                    view.usage_scroll = max(0, min(view.usage_scroll, max_usage_scroll(usage, w, h)))
                    screen = render_usage(screen, usage, view.usage_scroll, refreshing=usage_refreshing, message=view.message)
                    if view.modal:
                        draw_modal(screen, view, current, collector.errors)
                paint(win, screen, palette)
                last_screen = screen
                dirty, last_render = False, time.monotonic()
            try:
                if pending_input is not None:
                    key, from_mouse = pending_input
                    pending_input = None
                else:
                    key, from_mouse = terminal_input.read()
            except curses.error:
                continue
            if key is None:
                continue
            if key in ("\x03", "q", "Q"):
                return None
            steps = 1
            if from_mouse:
                key, pending_input = wheel_burst(win, key, terminal_input.read)
                steps = wheel_scroll.step(key, time.monotonic())
                if not steps:
                    continue
            elif key in (curses.KEY_UP, curses.KEY_DOWN) and not terminal_input.mouse_seen:
                # Some terminals emulate wheel input with indistinguishable
                # arrow keys. Protect that path until real mouse reports arrive.
                key, pending_input = arrow_burst(win, key, terminal_input.read)
                steps = wheel_scroll.step(key, time.monotonic())
                if not steps:
                    continue
            dirty = True
            if key == curses.KEY_RESIZE:
                # get_wch already updates the curses window size. A queued
                # resize must never be interpreted as navigation or search.
                continue
            if isinstance(key, Click):
                action = last_screen.hit_at(key.y, key.x) if last_screen else None
                clicked_at = time.monotonic()
                last_click_time, last_click_action = last_click
                double = (action is not None and last_click_action == action and
                          clicked_at - last_click_time <= DOUBLE_CLICK_SECONDS)
                last_click = (clicked_at, action)
                key = None
                if action is None or action[0] == "noop":
                    pass
                elif action[0] == "close":
                    view.modal = ""
                elif action[0] in ("new", "lang"):
                    if action[0] == "new":
                        view.new_client = action[1]
                    else:
                        view.language_index = action[1]
                    key = "\n"
                elif view.modal:
                    pass
                elif action[0] == "tab":
                    if not (view.tab == "usage" and action[1] == "usage"):
                        key = {"projects": "P", "sessions": "S", "usage": "U"}[action[1]]
                    if action[1] == "sessions" and view.tab == "sessions" and view.project_path is None:
                        key = None
                elif action[0] == "key":
                    key = action[1]
                elif view.tab == "usage":
                    pass
                elif action[0] == "scope":
                    view.tab, view.project_path, view.scope = "sessions", None, action[1]
                    view.selected, view.top, restore_key = 0, 0, ""
                elif action[0] == "client":
                    view.client, view.selected, view.top, restore_key = action[1], 0, 0, ""
                elif action[0] == "search":
                    view.searching = True
                elif action[0] == "row":
                    restore_key = ""
                    if double and view.selected == action[1]:
                        key = "\n"
                        last_click = (float("-inf"), None)
                    else:
                        view.selected = action[1]
                        keep_selection_visible(view, len(filtered), list_page_size(w, h))
                if key is None:
                    continue
            if not view.searching and isinstance(key, str) and key in ("j", "k", "g", "G"):
                # Vim-style aliases share the arrow / Home / End code paths.
                key = {"j": curses.KEY_DOWN, "k": curses.KEY_UP, "g": curses.KEY_HOME, "G": curses.KEY_END}[key]
            if key == "\x1b":
                refresh_key_at = None
                if view.modal:
                    view.modal = ""
                elif view.tab == "usage":
                    close_usage(view)
                elif view.project_path is not None:
                    open_projects(view)
                    restore_key = ""
                elif view.tab == "projects":
                    if view.folder_path is not None:
                        leave_folder(view)
                    elif view.query or view.searching:
                        view.query, view.searching = "", False
                        view.selected, view.top = 0, 0
                else:
                    restore_key, view.selected_key = "", ""
                    view.query, view.searching, view.narrow_detail = "", False, False
                    view.scope, view.client = "cwd", "all"
                    view.children, view.archived = False, False
                    view.selected, view.top, view.detail_scroll = 0, 0, 0
                    view.message = ""
                continue
            if key in BACKSPACE_KEYS and not view.searching:
                # Backspace walks back one screen; text fields keep their
                # normal delete behavior in the search branch below.
                refresh_key_at = None
                if view.modal:
                    view.modal = ""
                elif view.tab == "usage":
                    close_usage(view)
                elif view.project_path is not None:
                    open_projects(view)
                    restore_key = ""
                elif view.tab == "projects" and view.folder_path is not None:
                    leave_folder(view)
                elif view.tab == "sessions":
                    open_projects(view)
                    restore_key = ""
                continue
            if key in ("r", "R") and not view.searching and not view.modal:
                now = time.monotonic()
                if refresh_key_at is not None and now - refresh_key_at <= RESTART_KEY_SECONDS:
                    raise RestartRequested()
                refresh_key_at = now
            else:
                refresh_key_at = None
            if view.modal == "language":
                if key in (curses.KEY_UP, curses.KEY_DOWN, curses.KEY_LEFT, curses.KEY_RIGHT):
                    view.language_index = (view.language_index + steps) % len(LANGUAGES)
                elif key in ("\n", "\r", curses.KEY_ENTER):
                    set_language(LANGUAGES[view.language_index])
                    view.modal, view.message = "", ""
                    try:
                        save_language(language())
                    except OSError as exc:
                        view.message = tr("Could not save language: {error}", error=line(exc))
                    refresh_pending = True
                    if usage_future is None or usage_future.done():
                        usage_requested = False
                continue
            if key in ("l", "L") and not view.searching and not view.modal:
                view.modal = "language"
                view.language_index = LANGUAGES.index(language())
                continue
            if key in ("t", "T") and not view.searching and not view.modal:
                view.tree = not view.tree
                view.folder_path, view.folder_stack = None, []
                view.selected, view.top = 0, 0
                view.message = tr("Project tree: on") if view.tree else tr("Project tree: off")
                restore_key = ""
                continue
            if key in ("p", "P") and not view.searching and not view.modal:
                if view.tab != "projects" or view.folder_path is not None:
                    open_projects(view, root=True)
                restore_key = ""
                continue
            if view.tab == "usage":
                if key in ("u", "U"):
                    close_usage(view)
                elif key in ("s", "S"):
                    view.tab, view.message = "sessions", ""
                elif key in (curses.KEY_DOWN, curses.KEY_NPAGE, "]"):
                    view.usage_scroll = min(max_usage_scroll(usage, w, h), view.usage_scroll + (8 if key == curses.KEY_NPAGE else steps))
                elif key in (curses.KEY_UP, curses.KEY_PPAGE, "["):
                    view.usage_scroll = max(0, view.usage_scroll - (8 if key == curses.KEY_PPAGE else steps))
                elif key == curses.KEY_HOME:
                    view.usage_scroll = 0
                elif key == curses.KEY_END:
                    view.usage_scroll = max_usage_scroll(usage, w, h)
                elif key in ("r", "R"):
                    view.usage_scroll = 0
                    if usage_future is None or usage_future.done():
                        usage_requested, view.message = False, ""
                continue
            if view.modal:
                if key == "?":
                    view.modal = ""
                elif view.modal == "help":
                    if key in (curses.KEY_DOWN, curses.KEY_NPAGE):
                        view.help_scroll += 8 if key == curses.KEY_NPAGE else steps
                    elif key in (curses.KEY_UP, curses.KEY_PPAGE):
                        view.help_scroll = max(0, view.help_scroll - (8 if key == curses.KEY_PPAGE else steps))
                    elif key == curses.KEY_HOME:
                        view.help_scroll = 0
                elif view.modal == "new":
                    digit = key if isinstance(key, str) and len(key) == 1 and key.isdigit() else ""
                    if digit and 1 <= int(digit) <= len(CLIENTS):
                        view.new_client, key = int(digit) - 1, "\n"
                    if key in (curses.KEY_DOWN, curses.KEY_RIGHT):
                        view.new_client = (view.new_client + steps) % len(CLIENTS)
                    elif key in (curses.KEY_UP, curses.KEY_LEFT):
                        view.new_client = (view.new_client - steps) % len(CLIENTS)
                    elif key in ("\n", "\r", curses.KEY_ENTER):
                        client = CLIENTS[view.new_client]
                        if shutil.which(client):
                            try:
                                save_settings(new_client=client)
                            except OSError:
                                pass  # Remembering the choice is a convenience only.
                            return client, view.new_cwd or view.cwd
                        view.message = tr('{v0} is not installed', v0=client)
                continue
            if from_mouse and view.narrow_detail:
                limit = detail_scroll_limit(entry, view, w, h)
                delta = steps if key == curses.KEY_DOWN else -steps
                view.detail_scroll = max(0, min(limit, view.detail_scroll + delta))
                continue
            if key in ("\n", "\r", curses.KEY_ENTER):
                if view.searching:
                    view.searching = False
                elif isinstance(entry, Project):
                    if entry.children:
                        enter_folder(view, entry)
                    else:
                        enter_project(view, entry)
                    restore_key = ""
                elif current:
                    try:
                        validate_launch(current)
                        view.selected_key = current.key
                        return current
                    except ValueError as exc:
                        view.message = str(exc)
            elif key in ("o", "O") and not view.searching and isinstance(entry, Project):
                enter_project(view, entry)
                restore_key = ""
            elif key == "\t":
                restore_key = ""
                view.project_path = None
                view.tab = "sessions"
                view.scope = "all" if view.scope == "cwd" else "cwd"
                view.selected, view.top = 0, 0
            elif key in (curses.KEY_DOWN, curses.KEY_UP, curses.KEY_NPAGE, curses.KEY_PPAGE, curses.KEY_HOME, curses.KEY_END):
                restore_key = ""
                if filtered:
                    select_first_while_loading = key == curses.KEY_HOME and not view.loaded
                page = list_page_size(w, h)
                if key == curses.KEY_DOWN:
                    view.selected += steps
                elif key == curses.KEY_UP:
                    view.selected -= steps
                elif key == curses.KEY_NPAGE:
                    view.selected += page
                elif key == curses.KEY_PPAGE:
                    view.selected -= page
                elif key == curses.KEY_HOME:
                    view.selected = 0
                else:
                    view.selected = len(filtered) - 1
                keep_selection_visible(view, len(filtered), page)
            elif key in (curses.KEY_LEFT, curses.KEY_RIGHT) and not view.searching:
                restore_key = ""
                choices = ["all", *CLIENTS]
                view.client = choices[(choices.index(view.client) + (1 if key == curses.KEY_RIGHT else -1)) % len(choices)]
                view.selected, view.top = 0, 0
            elif view.searching and key in (curses.KEY_BACKSPACE, "\x7f", "\b"):
                view.query, view.selected, view.top = view.query[:-1], 0, 0
            elif view.searching and key == "\x15":
                view.query, view.selected, view.top = "", 0, 0
            elif view.searching and key == "\x17":
                view.query, view.selected, view.top = re.sub(r"\S*\s*$", "", view.query.rstrip()), 0, 0
            elif view.searching and isinstance(key, str) and key.isprintable():
                view.query += key
                view.selected, view.top = 0, 0
            elif key in ("/", "."):
                view.searching = True
            elif key in ("u", "U"):
                open_usage(view)
                if usage_future is None or usage_future.done():
                    usage_requested = False
            elif key in ("s", "S"):
                view.tab, view.project_path = "sessions", None
                view.selected, view.top = 0, 0
            elif key in ("n", "N"):
                view.new_cwd = (entry.cwd if isinstance(entry, Project) and entry.cwd else
                                view.project_path or (current.cwd if key == "N" and current and current.cwd else view.cwd))
                remembered = read_settings().get("new_client")
                view.new_client = (CLIENTS.index(current.client) if current else
                                   CLIENTS.index(remembered) if remembered in CLIENTS else 0)
                view.modal = "new"
            elif key in ("d", "D"):
                view.narrow_detail = not view.narrow_detail
            elif key == "]":
                limit = detail_scroll_limit(entry, view, w, h)
                view.detail_scroll = min(limit, view.detail_scroll)
                view.detail_scroll = min(limit, view.detail_scroll + 5)
            elif key == "[":
                view.detail_scroll = min(detail_scroll_limit(entry, view, w, h), view.detail_scroll)
                view.detail_scroll = max(0, view.detail_scroll - 5)
            elif key in ("c", "C"):
                view.children = not view.children
                view.message = tr("Child sessions: shown") if view.children else tr("Child sessions: hidden")
                view.selected, view.top = 0, 0
            elif key in ("a", "A"):
                view.archived = not view.archived
                view.message = tr("Archive: shown") if view.archived else tr("Archive: hidden")
                view.selected, view.top = 0, 0
            elif key in ("r", "R"):
                # Consume R even during a scan/detail read; it must never become
                # a search character because a background job is still busy.
                restore_key, view.selected_key = "", ""
                view.selected, view.top, view.detail_scroll = 0, 0, 0
                select_first_while_loading = True
                if not refreshing_history:
                    refresh_pending, refresh_to_first = True, True
            elif key == "?":
                view.modal, view.help_scroll = "help", 0
            elif isinstance(key, str) and len(key) == 1 and key in "0123456" and int(key) <= len(CLIENTS):
                restore_key = ""
                view.client = ["all", *CLIENTS][int(key)]
                view.selected, view.top = 0, 0
            elif key in ("f", "F", "*") and entry is not None:
                name = (Path(entry.cwd).name or tr('No directory')) if isinstance(entry, Project) else (entry.title or entry.id)
                if entry.key in view.pins:
                    view.pins.discard(entry.key)
                    view.message = tr("Unpinned: {name}", name=line(name, 60))
                else:
                    view.pins.add(entry.key)
                    view.message = tr("Pinned: {name}", name=line(name, 60))
                save_pins()
                fresh = visible_entries(sessions, view)
                view.selected = next((i for i, e in enumerate(fresh) if e.key == entry.key), view.selected)
                keep_selection_visible(view, len(fresh), list_page_size(w, h))
            elif key in ("y", "Y") and entry is not None:
                text = copy_payload(entry, identity=key == "Y")
                method = copy_text(text)
                view.message = (tr("Copied: {text}", text=line(text, 90)) if method else
                                tr("Clipboard unavailable: install pbcopy, wl-copy or xclip"))
            elif key in ("e", "E") and entry is not None:
                folder = entry.cwd
                try:
                    reveal(folder)
                    view.message = tr("Opened folder: {path}", path=pretty_path(folder, view.home))
                except (OSError, ValueError):
                    view.message = tr("Cannot open folder: {path}", path=pretty_path(folder, view.home))

    try:
        if sys.stdout.isatty():
            # Save/disable alternate-scroll arrow emulation while receiving
            # mouse reports, then restore the terminal's previous setting.
            sys.stdout.write("\x1b[?1007s\x1b[?1006s")
            sys.stdout.flush()
        return curses.wrapper(loop)
    finally:
        if sys.stdout.isatty():
            sys.stdout.write("\x1b[?1006r\x1b[?1007r")
            sys.stdout.flush()
        usage_cancel.set()
        executor.shutdown(wait=True, cancel_futures=True)
        collector.cache.save()
