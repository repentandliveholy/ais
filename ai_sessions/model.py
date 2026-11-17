from __future__ import annotations

from .i18n import tr

import os
import re
import shlex
import shutil
import time
import unicodedata
from functools import lru_cache
from dataclasses import asdict, dataclass, field
from datetime import datetime
from pathlib import Path
from urllib.parse import unquote, urlparse


CLIENTS = ("omp", "claude", "codex", "agy", "gemini", "pi")
LABELS = {"omp": "OMP", "claude": "Claude", "codex": "Codex", "agy": "agy", "gemini": "Gemini", "pi": "Pi"}
STATUSES = {
    "saved": 'Saved', "ready": 'Response ready', "closed": 'Closed',
    "interrupted": 'Interrupted', "error": 'Error', "awaiting": 'Question',
    "tool": 'Tool', "thinking": 'Thinking', "user": 'Prompt',
}
# Strip all terminal controls, including OSC hyperlinks and bidi overrides.
ANSI = re.compile(r"\x1b\][^\x07\x1b]*(?:\x07|\x1b\\)|\x1b\[[0-?]*[ -/]*[@-~]|\x1b[@-_]")


def clean(value: object, limit: int = 0) -> str:
    text = str(value or "")
    if limit:
        text = text[:limit * 2 + 256]
    text = ANSI.sub("", text)
    # Private-use font icons and invisible format controls are not portable.
    text = "".join(c for c in text if c in "\n\t" or unicodedata.category(c)[0] != "C")
    return text[:limit] if limit else text


def line(value: object, limit: int = 0) -> str:
    text = " ".join(clean(value, limit * 2 if limit else 0).split())
    return text[:limit] if limit else text


def stamp(value: object, default: float = 0) -> float:
    try:
        if isinstance(value, (float, int)):
            return value / 1000 if value > 1e11 else float(value)
        return datetime.fromisoformat(str(value).replace("Z", "+00:00")).timestamp()
    except (ValueError, TypeError, OverflowError):
        return default


def path_from_uri(value: str) -> str:
    if value.startswith("file://"):
        return unquote(urlparse(value).path)
    return value


@lru_cache(maxsize=8192)
def canonical(value: str) -> str:
    if not value:
        return ""
    return os.path.realpath(os.path.expanduser(path_from_uri(value)))


@lru_cache(maxsize=8192)
def same_cwd(left: str, right: str) -> bool:
    if not left or not right:
        return False
    try:
        return os.path.samefile(left, right)
    except OSError:
        return canonical(left) == canonical(right)


def pretty_path(value: str, home: Path | None = None) -> str:
    if not value:
        return tr('Directory not recorded')
    home_str = str(home or Path.home())
    value = canonical(value)
    return "~" + value[len(home_str):] if value == home_str or value.startswith(home_str + "/") else value


def age(value: float, now: float | None = None) -> str:
    if not value:
        return "—"
    try:
        recorded = datetime.fromtimestamp(value)
        seconds = max(0, int((now or time.time()) - value))
    except (ValueError, OverflowError, OSError, TypeError):
        return "—"
    if seconds < 60:
        return tr('now')
    if seconds < 3600:
        return tr('{v0} min', v0=seconds // 60)
    if seconds < 86400:
        return tr('{v0} h', v0=seconds // 3600)
    if seconds < 86400 * 30:
        return tr('{v0} d', v0=seconds // 86400)
    return recorded.strftime("%d.%m.%y")


def number(value: int | float | None) -> str:
    if value is None:
        return "—"
    if value >= 1_000_000:
        return f"{value / 1_000_000:.1f}M"
    if value >= 1000:
        return f"{value / 1000:.1f}k"
    return str(value)


@dataclass
class Session:
    client: str
    id: str
    cwd: str = ""
    title: str = ""
    path: str = ""
    updated: float = 0
    created: float = 0
    model: str = ""
    provider: str = ""
    status: str = "saved"
    status_detail: str = ""
    last_action: str = ""
    last_user: str = ""
    last_assistant: str = ""
    messages: int = 0
    tools: int = 0
    tokens: int | None = None
    cost: float | None = None
    branch: str = ""
    profile: str = ""
    source: str = ""
    project_id: str = ""
    steps: int | None = None
    parent: str = ""
    todo: list[dict] = field(default_factory=list)
    exact: bool = False
    archived: bool = False
    state_root: str = ""
    workspaces: list[str] = field(default_factory=list)
    resume_supported: bool = True
    note: str = ""

    @property
    def key(self) -> str:
        return f"{self.client}:{self.state_root}:{self.profile}:{self.id}"

    @property
    def project(self) -> str:
        return Path(self.cwd).name if self.cwd else tr('No directory')

    @property
    def search_text(self) -> str:
        return " ".join((self.title, self.cwd, self.client, self.model, self.id,
                         self.last_user, self.last_assistant, self.last_action,
                         self.branch, self.profile, self.status, tr(STATUSES.get(self.status, "")))).casefold()

    def in_cwd(self, cwd: str) -> bool:
        return any(same_cwd(p, cwd) for p in [self.cwd, *self.workspaces])

    def to_dict(self) -> dict:
        return asdict(self)

    def argv(self) -> list[str]:
        if not self.resume_supported:
            raise ValueError(self.note or tr('This client does not support resuming by ID'))
        if self.client == "omp":
            return ["omp", *(["--profile", self.profile] if self.profile else []), "--resume", self.path or self.id]
        if self.client == "pi":
            return ["pi", "--session", self.path or self.id]
        if self.client == "claude":
            return ["claude", "--resume", self.id]
        if self.client == "codex":
            return ["codex", "resume", self.id, "--cd", self.cwd] if self.cwd else ["codex", "resume", self.id]
        if self.client == "agy":
            return ["agy", *(["--project", self.project_id] if self.project_id else []), "--conversation", self.id]
        if self.client == "gemini":
            return ["gemini", "--resume", self.id]
        raise ValueError(tr('No launch adapter for {v0}', v0=self.client))

    def command(self) -> str:
        try:
            return shlex.join(self.argv())
        except ValueError:
            return tr('Resume unavailable')

    def environment(self) -> dict[str, str]:
        env = os.environ.copy()
        if self.client == "omp":
            if self.profile:
                env["OMP_PROFILE"] = self.profile
            else:
                env.pop("OMP_PROFILE", None)
        if self.state_root:
            key = {"codex": "CODEX_HOME", "claude": "CLAUDE_CONFIG_DIR",
                   "omp": "PI_CODING_AGENT_DIR", "pi": "PI_CODING_AGENT_DIR"}.get(self.client)
            if key and self.client == "claude" and Path(self.state_root) == Path.home() / ".claude" \
                    and not os.environ.get(key):
                # The default store must stay default: Claude keys its keychain
                # login by config dir, so an explicit path is another login.
                env.pop(key, None)
            elif key:
                env[key] = self.state_root
        return env


FIELDS = {
    "client": "client", "c": "client", "cli": "client",
    "model": "model", "m": "model",
    "status": "status", "st": "status",
    "branch": "branch", "b": "branch",
    "dir": "dir", "in": "dir", "cwd": "dir",
    "profile": "profile",
    "since": "since", "age": "since",
    "is": "is",
}
DURATION = re.compile(r"(\d+(?:\.\d+)?)\s*(m|min|h|d|w|mo|y)?$")
UNITS = {"m": 60, "min": 60, "h": 3600, "d": 86400, "w": 604800, "mo": 2592000, "y": 31536000}
TOKEN = re.compile(r'(-?)(?:([A-Za-z]+):)?("[^"]*"?|\S+)')


def duration(value: str) -> float | None:
    """'90m', '6h', '3d', '2w', 'today', 'yesterday' -> seconds back from now."""
    value = value.casefold().strip()
    if value in ("today", "сегодня"):
        midnight = datetime.now().replace(hour=0, minute=0, second=0, microsecond=0)
        return max(1.0, time.time() - midnight.timestamp())
    if value in ("yesterday", "вчера"):
        midnight = datetime.now().replace(hour=0, minute=0, second=0, microsecond=0)
        return time.time() - midnight.timestamp() + 86400
    match = DURATION.match(value)
    if not match:
        return None
    return float(match[1]) * UNITS[match[2] or "d"]


@dataclass
class Query:
    words: list[str] = field(default_factory=list)
    excluded: list[str] = field(default_factory=list)
    fields: list[tuple[str, str, bool]] = field(default_factory=list)

    @property
    def highlights(self) -> list[str]:
        return [w for w in self.words if w]


def parse_query(query: str) -> Query:
    """Plain words, -excluded words and field:value filters (see ? help)."""
    parsed = Query()
    for negative, name, value in TOKEN.findall(query or ""):
        value = value.strip('"').casefold()
        key = FIELDS.get(name.casefold()) if name else None
        if name and key is None:
            # An unknown prefix (for example http:) is ordinary text.
            value, name = f"{name.casefold()}:{value}", ""
        if not value:
            continue
        if key:
            parsed.fields.append((key, value, bool(negative)))
        elif negative and value != "-":
            parsed.excluded.append(value)
        else:
            parsed.words.append(negative + value if value == "-" else value)
    return parsed


def field_match(s: Session, key: str, value: str, pins: frozenset | set) -> bool:
    if key == "client":
        return s.client.startswith(value) or LABELS[s.client].casefold().startswith(value)
    if key == "model":
        return value in f"{s.model} {s.provider}".casefold()
    if key == "status":
        label = tr(STATUSES.get(s.status, s.status)).casefold()
        return s.status.startswith(value) or value in label or value in STATUSES.get(s.status, "").casefold()
    if key == "branch":
        return value in s.branch.casefold()
    if key == "dir":
        return any(value in p.casefold() for p in [s.cwd, *s.workspaces] if p)
    if key == "profile":
        return value in s.profile.casefold()
    if key == "since":
        seconds = duration(value)
        return seconds is None or s.updated >= time.time() - seconds
    if key == "is":
        return {"pinned": s.key in pins, "pin": s.key in pins, "fav": s.key in pins,
                "child": bool(s.parent), "archived": s.archived, "archive": s.archived,
                "exact": s.exact}.get(value, True)
    return True


def query_match(s: Session, query: Query, pins: frozenset | set = frozenset()) -> bool:
    text = s.search_text
    return (all(w in text for w in query.words)
            and not any(w in text for w in query.excluded)
            and all(field_match(s, key, value, pins) != negative for key, value, negative in query.fields))


def filter_sessions(sessions: list[Session], cwd: str, scope: str = "cwd",
                    client: str = "all", query: str = "", children: bool = False,
                    archived: bool = False, pins: frozenset | set = frozenset()) -> list[Session]:
    parsed = parse_query(query)
    explicit = {(key, value) for key, value, negative in parsed.fields if key == "is" and not negative}
    # An explicit is:child / is:archived filter implies showing those rows.
    children = children or any(v == "child" for _, v in explicit)
    archived = archived or any(v in ("archived", "archive") for _, v in explicit)
    return sorted((s for s in sessions
                   if (scope == "all" or s.in_cwd(cwd))
                   and (client == "all" or s.client == client)
                   and (children or not s.parent)
                   and (archived or not s.archived)
                   and query_match(s, parsed, pins)),
                  key=lambda s: s.updated, reverse=True)


@dataclass
class Project:
    cwd: str
    sessions: list[Session]
    depth: int = 0
    prefix: str = ""
    children: list["Project"] = field(default_factory=list)

    @property
    def key(self) -> str:
        return "project:" + self.cwd

    @property
    def updated(self) -> float:
        return max((s.updated for s in self.sessions), default=0)

    @property
    def subtree_updated(self) -> float:
        """Latest activity in this folder or any nested project folder."""
        return max([self.updated, *(child.subtree_updated for child in self.children)])


def project_parent(path: str, projects: dict[str, Project]) -> Project | None:
    """Nearest ancestor folder that is itself a project."""
    if not path:
        return None
    current = Path(path)
    while True:
        parent = current.parent
        if parent == current:
            return None
        found = projects.get(str(parent))
        if found is not None:
            return found
        current = parent


def project_tree(sessions: list[Session], client: str = "all", query: str = "",
                 children: bool = False, archived: bool = False,
                 pins: frozenset | set = frozenset()) -> tuple[list[Project], dict[str, Project]]:
    groups: dict[str, list[Session]] = {}
    for session in filter_sessions(sessions, "", "all", client, query, children, archived, pins):
        # Unknown directories remain visible, without pretending they are cwd.
        paths = dict.fromkeys(canonical(p) for p in [session.cwd, *session.workspaces] if p)
        for path in paths or {"": None}:
            groups.setdefault(path, []).append(session)
    projects = [Project(path, items) for path, items in groups.items()]
    by_path = {project.cwd: project for project in projects}
    roots: list[Project] = []
    for project in projects:
        parent = project_parent(project.cwd, by_path)
        if parent is None:
            roots.append(project)
        else:
            parent.children.append(project)

    def order(items: list[Project]):
        # A parent follows its freshest descendant so active child folders stay visible.
        items.sort(key=lambda p: (-p.subtree_updated, Path(p.cwd).name.casefold(), p.cwd))
        for item in items:
            order(item.children)

    order(roots)
    return roots, by_path


def group_projects(sessions: list[Session], client: str = "all", query: str = "",
                   children: bool = False, archived: bool = False,
                   *, tree: bool = False, pins: frozenset | set = frozenset()) -> list[Project]:
    roots, by_path = project_tree(sessions, client, query, children, archived, pins)
    if tree:
        flat: list[Project] = []

        def flatten(items: list[Project], depth: int, prefix: str):
            for index, project in enumerate(items):
                last = index == len(items) - 1
                project.depth = depth
                project.prefix = prefix + ("" if depth == 0 else ("└─ " if last else "├─ "))
                flat.append(project)
                flatten(project.children, depth + 1,
                        prefix + ("" if depth == 0 else ("   " if last else "│  ")))

        flatten(roots, 0, "")
        return flat
    projects = sorted(by_path.values(),
                      key=lambda p: (-p.updated, Path(p.cwd).name.casefold(), p.cwd))
    for project in projects:
        project.depth, project.prefix = 0, ""
    return projects


def folder_entries(sessions: list[Session], path: str, client: str = "all", query: str = "",
                   children: bool = False, archived: bool = False,
                   pins: frozenset | set = frozenset()) -> list[Session | Project]:
    """Direct subfolders with sessions, then the folder's own sessions."""
    _, by_path = project_tree(sessions, client, query, children, archived, pins)
    project = by_path.get(path)
    if project is None:
        return []
    folders = sorted(project.children,
                     key=lambda p: (-p.subtree_updated, Path(p.cwd).name.casefold(), p.cwd))
    for folder in folders:
        folder.depth, folder.prefix = 0, ""
    return [*folders, *sorted(project.sessions, key=lambda s: s.updated, reverse=True)]


def validate_launch(session: Session) -> list[str]:
    argv = session.argv()
    if not session.cwd:
        raise ValueError(tr('Session directory unknown; resume disabled'))
    if not Path(session.cwd).is_dir():
        raise ValueError(tr('Directory no longer exists: {v0}', v0=session.cwd))
    binary = shutil.which(argv[0])
    if not binary:
        raise ValueError(tr('Client {v0} not found in PATH', v0=argv[0]))
    argv[0] = binary
    return argv
