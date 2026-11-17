from __future__ import annotations

from .i18n import tr

import hashlib
import json
import os
import sqlite3
from collections import Counter
from dataclasses import dataclass
from pathlib import Path
from typing import Callable

from .model import CLIENTS, Session, canonical, clean, line, path_from_uri, stamp
from .storage import Cache, json_records, read_database


def text_content(content: object) -> str:
    if isinstance(content, str):
        return clean(content, 6000)
    if isinstance(content, list):
        return clean("\n".join(str(b.get("text", "")) for b in content
                               if isinstance(b, dict) and b.get("type") in ("text", "input_text", "output_text")), 6000)
    return ""


def tool_blocks(content: object) -> list[dict]:
    return [b for b in content if isinstance(b, dict) and b.get("type") in ("tool_use", "toolCall")] if isinstance(content, list) else []


def set_message(s: Session, role: str, content: object, message: dict):
    if role not in ("user", "assistant"):
        return
    text = text_content(content)
    if role == "user" and text:
        if text.lstrip().startswith(("<local-command-", "<command-name>", "<system-reminder>")):
            return
        s.last_user = text
        s.messages += 1
        s.status = "user"
        s.status_detail = tr('Last record: user prompt')
        if not s.title:
            s.title = line(text, 160)
    elif role == "assistant":
        if text:
            s.last_assistant = text
        s.messages += 1
        if message.get("model") and message["model"] != "<synthetic>":
            model = message["model"]
            s.model = line(model.get("id", model.get("name", "")) if isinstance(model, dict) else model)
        tools = tool_blocks(content)
        s.tools += len(tools)
        if tools:
            name = tools[-1].get("name", "tool")
            s.status = "awaiting" if name.lower() in ("ask", "askuserquestion") else "tool"
            s.last_action = line(tools[-1].get("intent") or name, 1000)
            s.status_detail = tr('Last record: tool call {v0}', v0=name)
        elif text:
            s.status = "ready"
            s.status_detail = tr('Last record: assistant response')
        elif isinstance(content, list) and any(b.get("type") == "thinking" for b in content if isinstance(b, dict)):
            s.status = "thinking"
            s.status_detail = tr('Last record: thinking')
        reason = str(message.get("stopReason", message.get("stop_reason", ""))).lower()
        if reason in ("error", "aborted", "interrupted"):
            s.status = "error" if reason == "error" else "interrupted"
            s.status_detail = line(message.get("errorMessage") or reason, 2000)


def parse_jsonl(path: Path, client: str, full: bool = False, profile: str = "", state_root: str = "") -> Session:
    st = path.stat()
    s = Session(client=client, id=path.stem, path=str(path), updated=st.st_mtime,
                profile=profile, state_root=state_root, source="JSONL",
                exact=full or st.st_size <= 384 * 1024)
    s._source_sig = [st.st_mtime_ns, st.st_size, st.st_ino]
    if client in ("omp", "pi") and "_" in path.stem:
        s.id = path.stem.rsplit("_", 1)[-1]
    if client == "codex" and path.stem.startswith("rollout-"):
        s.id = path.stem[-36:]
    explicit_title = ""
    for r in json_records(path, full):
        kind = r.get("type")
        if kind in ("session", "session_meta"):
            payload = r.get("payload", r)
            s.id = str(payload.get("id") or payload.get("session_id") or s.id)
            s.cwd = canonical(payload.get("cwd", s.cwd))
            s.created = stamp(payload.get("timestamp"), s.created)
            if payload.get("title"):
                explicit_title = line(payload["title"], 500)
            s.parent = str(payload.get("parentSessionId") or payload.get("parentSession") or s.parent)
            s.provider = line(payload.get("model_provider", s.provider))
            s.source = line(payload.get("originator") or payload.get("source") or s.source)
            source = payload.get("source")
            if isinstance(source, dict) and "subagent" in source:
                agent = source["subagent"]
                s.parent = str(agent.get("thread_spawn", {}).get("parent_thread_id", "subagent")) if isinstance(agent, dict) else "subagent"
        if r.get("cwd"):
            s.cwd = canonical(r["cwd"])
        if r.get("gitBranch"):
            s.branch = line(r["gitBranch"])
        if r.get("isSidechain"):
            s.parent = str(r.get("parentSessionId") or "subagent")
        if kind in ("title", "title_change", "session_info", "ai-title", "summary"):
            title = r.get("title") or r.get("name") or r.get("aiTitle") or r.get("summary")
            if title:
                explicit_title = line(title, 500)
        if kind == "model_change":
            if r.get("role") not in (None, "default", "main"):
                continue
            model = r.get("model") or r.get("modelId") or ""
            s.model = line(model.get("id", model.get("name", "")) if isinstance(model, dict) else model)
            s.provider = line((model.get("provider") if isinstance(model, dict) else "") or r.get("provider") or s.provider)
        if kind == "turn_context":
            p = r.get("payload", {})
            s.model = line(p.get("model", s.model))
            s.branch = line(p.get("git_branch", s.branch))
        if kind in ("message", "assistant", "user"):
            m = r.get("message", {})
            if isinstance(m, dict):
                set_message(s, m.get("role", kind), m.get("content"), m)
        if kind == "custom":
            ct, data = r.get("customType"), r.get("data", {})
            if not isinstance(data, dict):
                continue
            if ct == "tool_execution_start":
                name = str(data.get("toolName", "tool"))
                s.status = "awaiting" if name.lower() in ("ask", "askuserquestion") else "tool"
                s.last_action = line(data.get("intent") or name, 1000)
                s.status_detail = tr('Last record: {v0} · {v1}', v0=name, v1=s.last_action)
            elif ct == "session_exit":
                reason = str(data.get("kind") or data.get("reason") or "normal")
                s.status = "error" if "error" in reason else "interrupted" if any(x in reason for x in ("crash", "interrupt", "signal")) else "closed"
                s.status_detail = tr('CLI exit: ') + line(data.get("reason") or reason)
            elif ct in ("user_todo_edit", "todo_update"):
                s.todo = [{"text": line(t.get("content") or t.get("text"), 500), "status": line(t.get("status"))}
                          for phase in data.get("phases", []) if isinstance(phase, dict)
                          for t in phase.get("tasks", []) if isinstance(t, dict)][:40]
        if kind == "cost-state":
            s.cost = r.get("totalCostUSD")
        if kind == "model_usage":
            # Different OMP versions use different usage envelopes.
            usage = r.get("usage", {})
            if isinstance(usage, dict) and isinstance(usage.get("totalTokens"), (int, float)):
                s.tokens = (s.tokens or 0) + int(usage["totalTokens"])
        if kind == "system" and r.get("subtype") in ("api_error", "error"):
            s.status = "error"
            s.status_detail = line(r.get("content") or r.get("error") or tr('API error'), 1500)
        if kind == "response_item":
            p = r.get("payload", {})
            pk = p.get("type")
            if pk == "message" and p.get("role") in ("user", "assistant"):
                text = text_content(p.get("content"))
                if p.get("role") == "user" and text.startswith(("<environment_context>", "# AGENTS.md", "<INSTRUCTIONS>")):
                    continue
                set_message(s, p["role"], p.get("content"), p)
            elif pk in ("function_call", "custom_tool_call"):
                s.tools += 1
                name = p.get("name", "tool")
                s.status = "awaiting" if "request_user_input" in name else "tool"
                s.last_action = line(name)
                s.status_detail = tr('Last record: tool call ') + line(name)
        if kind == "event_msg":
            p = r.get("payload", {})
            pk = p.get("type")
            if pk in ("task_complete", "turn_complete", "task_completed"):
                s.status = "ready"
                s.status_detail = tr('Last record: turn completed')
                if p.get("last_agent_message"):
                    s.last_assistant = clean(p["last_agent_message"], 6000)
            elif pk in ("turn_aborted", "task_failed"):
                s.status = "interrupted" if pk == "turn_aborted" else "error"
                s.status_detail = line(p.get("reason") or pk, 1500)
            elif pk == "token_count":
                info = p.get("info") or {}
                total = info.get("total_token_usage") or {}
                if total.get("total_tokens") is not None:
                    s.tokens = total["total_tokens"]
            elif pk in ("agent_message", "user_message"):
                # Legacy rollouts may contain only events. Avoid double-counting
                # mirrored response_item messages in modern rollouts.
                text = clean(p.get("message", ""), 6000)
                if text:
                    if pk == "agent_message":
                        s.last_assistant = text
                    else:
                        s.last_user = text
                        if not s.title:
                            s.title = line(text, 160)
    s.title = explicit_title or s.title or line(s.last_user, 160) or tr('Untitled')
    s.cwd = s.cwd or ""
    return s


@dataclass
class SourceInfo:
    client: str
    count: int = 0
    roots: list[str] | None = None
    error: str = ""


SIBLING_MARKERS = {"claude": ("projects", ".claude.json"), "codex": ("sessions", "auth.json")}


class Collector:
    def __init__(self, home: Path, cache: Cache, overrides: dict[str, list[Path]] | None = None):
        self.home, self.cache = home, cache
        self.overrides = overrides or {}
        self.sources = {c: SourceInfo(c, roots=[]) for c in CLIENTS}
        self.errors: list[str] = []

    def root(self, client: str) -> list[Path]:
        if client in self.overrides:
            return self.overrides[client]
        defaults = {
            "omp": [self.home / ".omp/agent"],
            "pi": [self.home / ".pi/agent"],
            "claude": [self.home / ".claude"],
            "codex": [self.home / ".codex"],
            "agy": [self.home / ".gemini/antigravity-cli"],
            "gemini": [self.home / ".gemini"],
        }
        env = {"omp": "PI_CODING_AGENT_DIR", "claude": "CLAUDE_CONFIG_DIR", "codex": "CODEX_HOME"}.get(client)
        roots = defaults[client]
        if env and os.environ.get(env) and self.home == Path.home():
            roots = [Path(os.environ[env]).expanduser()]
        elif client in SIBLING_MARKERS:
            # Extra accounts are usually separate config dirs: ~/.claude-work,
            # ~/.codex-personal. Only real stores with history or a login count.
            roots = [*roots, *sorted(p for p in self.home.glob(f".{client}-*")
                                     if p.is_dir() and any((p / m).exists() for m in SIBLING_MARKERS[client]))]
        if client == "omp":
            roots = [*roots, *sorted((self.home / ".omp/profiles").glob("*/agent"))]
        return roots

    def scan(self, publish: Callable[[list[Session]], None] | None = None) -> list[Session]:
        all_sessions: dict[str, Session] = {}
        # Small, indexed stores appear first while OMP's file index is built.
        for client in ("agy", "codex", "claude", "gemini", "pi", "omp"):
            for root in self.root(client):
                if not root.exists():
                    continue
                self.sources[client].roots.append(str(root))
                try:
                    def partial(rows):
                        for item in rows:
                            all_sessions[item.key] = item
                        if publish:
                            publish(list(all_sessions.values()))

                    rows = self.scan_agy(root) if client == "agy" else self.scan_gemini(root) if client == "gemini" else self.scan_files(root, client, partial if publish else None)
                    for s in rows:
                        all_sessions[s.key] = s
                    self.sources[client].count += len(rows)
                except (OSError, ValueError, TypeError, sqlite3.Error) as exc:
                    self.warn(client, f"{root}: {exc}")
                if publish:
                    publish(list(all_sessions.values()))
        self.cache.save()
        return sorted(all_sessions.values(), key=lambda s: s.updated, reverse=True)

    def warn(self, client: str, message: str):
        message = line(message, 600)
        self.sources[client].error = message
        self.errors.append(f"{client}: {message}")

    def scan_files(self, root: Path, client: str, progress=None) -> list[Session]:
        dirs = [root / "projects"] if client == "claude" else [root / "sessions"]
        if client == "codex":
            dirs.append(root / "archived_sessions")
        # Pi supports an explicit session directory independently of its state.
        if client == "pi" and os.environ.get("PI_CODING_AGENT_SESSION_DIR") and self.home == Path.home():
            dirs = [Path(os.environ["PI_CODING_AGENT_SESSION_DIR"])]
        profile = root.parent.name if client == "omp" and root.parent.parent.name == "profiles" else ""
        if client in SIBLING_MARKERS and root.name.startswith(f".{client}-"):
            profile = root.name[len(client) + 2:]
        sessions: dict[str, Session] = {}
        for folder in dirs:
            paths = sorted(folder.rglob("*.jsonl"), key=lambda p: p.stat().st_mtime if p.exists() else 0, reverse=True)
            for index, path in enumerate(paths):
                # Claude's subagent files should not pollute the top-level list.
                if client == "claude" and ("subagents" in path.parts or path.name.startswith("agent-")):
                    continue
                try:
                    s = self.cache.get(path, client, profile)
                    if s is None:
                        s = parse_jsonl(path, client, profile=profile, state_root=str(root))
                        self.cache.put(path, s)
                    if folder.name == "archived_sessions":
                        s.archived = True
                    previous = sessions.get(s.id)
                    if previous is None or s.updated > previous.updated:
                        sessions[s.id] = s
                except (OSError, ValueError, TypeError, AttributeError) as exc:
                    self.warn(client, f"{path.name}: {exc}")
                if progress and index % 64 == 0:
                    progress(list(sessions.values()))
        if client == "codex":
            self.enrich_codex(root, sessions)
        if client == "omp":
            self.enrich_omp(root, sessions)
        return list(sessions.values())

    def enrich_omp(self, root: Path, sessions: dict[str, Session]):
        path = root / "history.db"
        if not path.exists():
            return
        try:
            with read_database(path) as con:
                for row in con.execute("SELECT session_id,title FROM session_titles"):
                    if row["session_id"] in sessions:
                        sessions[row["session_id"]].title = line(row["title"], 500)
        except sqlite3.Error as exc:
            self.warn("omp", tr('Titles from history.db: {v0}', v0=exc))

    def enrich_codex(self, root: Path, sessions: dict[str, Session]):
        paths = sorted(root.glob("state_*.sqlite"), key=lambda p: p.stat().st_mtime, reverse=True)
        if not paths:
            return
        try:
            with read_database(paths[0]) as con:
                for row in con.execute("SELECT * FROM threads"):
                    r = dict(row)
                    s = sessions.get(r["id"])
                    if s is None:
                        path = Path(r.get("rollout_path", ""))
                        if not path.is_file():
                            continue
                        s = Session("codex", r["id"], path=str(path), state_root=str(root))
                        sessions[s.id] = s
                    s.title = line(r.get("title") or s.title, 500)
                    s.cwd = canonical(r.get("cwd") or s.cwd)
                    s.updated = max(s.updated, stamp(r.get("updated_at_ms") or r.get("updated_at")))
                    s.created = stamp(r.get("created_at_ms") or r.get("created_at"), s.created)
                    s.model = line(r.get("model") or s.model)
                    s.provider = line(r.get("model_provider") or s.provider)
                    s.branch = line(r.get("git_branch") or s.branch)
                    s.tokens = r.get("tokens_used") or s.tokens
                    s.archived = bool(r.get("archived"))
                    s.parent = str(r.get("attached_thread_parent_id") or s.parent)
                    source = str(r.get("source", ""))
                    if '"subagent"' in source or source == "subagent":
                        s.parent = s.parent or "subagent"
                    s.source = line(r.get("originator") or source or s.source)
        except (sqlite3.Error, OSError) as exc:
            self.warn("codex", tr('Codex index: {v0}', v0=exc))

    def scan_agy(self, root: Path) -> list[Session]:
        path = root / "conversation_summaries.db"
        if not path.exists():
            return []
        result = []
        with read_database(path) as con:
            for row in con.execute("SELECT * FROM conversation_summaries"):
                r = dict(row)
                uris = json.loads(r.get("workspace_uris") or "[]")
                workspaces = [canonical(p) for p in uris if isinstance(p, str)] if isinstance(uris, list) else []
                status = str(r.get("status") or "")
                mapped = {"completed": "ready", "done": "ready", "error": "error", "failed": "error",
                          "cancelled": "interrupted", "waiting": "awaiting", "running": "tool"}.get(status.lower(), "saved")
                if r.get("killed"):
                    mapped = "interrupted"
                s = Session("agy", r["conversation_id"], cwd=workspaces[0] if workspaces else "",
                            title=line(r.get("title") or r.get("preview") or tr('Untitled'), 500),
                            path=str(path), updated=stamp(r.get("last_modified_time")),
                            status=mapped, status_detail=tr('Status in agy index: ') + (status or tr('saved')),
                            last_user=clean(r.get("preview"), 6000), steps=r.get("step_count"),
                            source=line(r.get("source") or "agy CLI"), project_id=str(r.get("project_id") or ""),
                            parent=str(r.get("parent_conversation_id") or ""), state_root=str(root),
                            workspaces=workspaces, exact=True)
                s.note = tr('Status was saved by the client; it does not confirm a running process.')
                if canonical(str(root)) != canonical(str(Path.home() / ".gemini/antigravity-cli")):
                    s.resume_supported = False
                    s.note += tr(' Resuming from a custom agy directory is not supported.')
                result.append(s)
        return result

    def scan_gemini(self, root: Path) -> list[Session]:
        project_map: dict[str, str] = {}
        mapping = root / "projects.json"
        if mapping.exists():
            try:
                for cwd, slug in json.loads(mapping.read_text()).get("projects", {}).items():
                    project_map[str(slug)] = canonical(cwd)
                    project_map[hashlib.sha256(cwd.encode()).hexdigest()] = canonical(cwd)
            except (OSError, ValueError, AttributeError):
                pass
        # Older Gemini uses sha256(cwd); its history file contains the original
        # project path. No directory name is decoded heuristically.
        history = root / "history.json"
        if history.exists():
            try:
                for item in json.loads(history.read_text()):
                    cwd = item.get("project") if isinstance(item, dict) else None
                    if cwd:
                        project_map[hashlib.sha256(cwd.encode()).hexdigest()] = canonical(cwd)
            except (OSError, ValueError, TypeError):
                pass
        result = []
        for path in (root / "tmp").glob("*/chats/session-*.json"):
            try:
                folder = path.parent.parent
                cwd = project_map.get(folder.name, "")
                marker = folder / ".project_root"
                if marker.exists():
                    cwd = canonical(marker.read_text().strip())
                r = json.loads(path.read_text())
                s = Session("gemini", str(r["sessionId"]), cwd=cwd, path=str(path),
                            updated=stamp(r.get("lastUpdated"), path.stat().st_mtime),
                            created=stamp(r.get("startTime")), title=line(r.get("summary"), 500),
                            state_root=str(root), source="Gemini CLI", exact=True)
                for m in r.get("messages", []):
                    set_message(s, "assistant" if m.get("type") == "gemini" else m.get("type", ""), m.get("content"), m)
                    s.tools += len(m.get("toolCalls") or [])
                    s.model = line(m.get("model") or s.model)
                s.title = s.title or line(s.last_user, 160) or tr('Untitled')
                if not cwd:
                    s.resume_supported = False
                    s.note = tr('History found, but the original Gemini directory is unknown.')
                elif canonical(str(root)) != canonical(str(Path.home() / ".gemini")):
                    s.resume_supported = False
                    s.note = tr('Resuming from a custom Gemini directory is not supported.')
                result.append(s)
            except (OSError, ValueError, KeyError, TypeError, AttributeError) as exc:
                self.warn("gemini", f"{path.name}: {exc}")
        return result

    def detail(self, session: Session) -> Session:
        if session.client == "agy":
            from dataclasses import replace
            s = replace(session)
            try:
                with read_database(Path(s.path)) as con:
                    rows = con.execute("SELECT user_body,agent_body FROM conversations_fts WHERE conversation_id=? ORDER BY CAST(chunk_index AS INTEGER) DESC LIMIT 1", (s.id,)).fetchall()
                    if rows:
                        s.last_user = clean(rows[0]["user_body"], 6000).strip() or s.last_user
                        s.last_assistant = clean(str(rows[0]["agent_body"] or "").strip()[-6000:])
            except sqlite3.Error:
                pass
            return s
        if session.client == "gemini" or session.exact:
            return session
        s = parse_jsonl(Path(session.path), session.client, full=True,
                        profile=session.profile, state_root=session.state_root)
        # Index-provided metadata is authoritative for titles and archive flags.
        s.title, s.archived, s.parent = session.title, session.archived, session.parent
        s.updated = max(s.updated, session.updated)
        s.tokens = session.tokens if session.tokens is not None else s.tokens
        s.branch = s.branch or session.branch
        s.model = s.model or session.model
        return s
