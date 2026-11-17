from __future__ import annotations

import contextlib
import io
import json
import os
import sqlite3
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

from ai_sessions.adapters import Collector, parse_jsonl
from ai_sessions.cli import main
from ai_sessions.model import Session, clean, filter_sessions, validate_launch
from ai_sessions.screen import Screen, clip, width
from ai_sessions.storage import Cache, json_records, read_database
from ai_sessions.ui import View, render


def write_jsonl(path, records):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text("".join(json.dumps(r, ensure_ascii=False) + "\n" for r in records))
    return path


class SessionTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.root = Path(self.tmp.name).resolve()
        self.project = self.root / "Проект с пробелами"
        self.project.mkdir()

    def tearDown(self):
        self.tmp.cleanup()

    def omp(self, extra=()):
        return write_jsonl(self.root / "home/.omp/agent/sessions/project/one.jsonl", [
            {"type": "session", "id": "omp-id", "cwd": str(self.project), "timestamp": "2026-11-12T10:00:00Z", "title": "Сохранённое название"},
            {"type": "model_change", "model": {"id": "test-model", "provider": "test-provider"}},
            {"type": "message", "message": {"role": "user", "content": "Проверь сборку"}},
            {"type": "custom", "customType": "tool_execution_start", "data": {"toolName": "bash", "intent": "Проверка сборки"}},
            {"type": "message", "message": {"role": "assistant", "content": [{"type": "text", "text": "Сборка прошла"}]}},
            *extra,
        ])

    def test_omp_title_status_last_action_and_partial_write(self):
        path = self.omp([{"type": "custom", "customType": "session_exit", "data": {"kind": "normal", "reason": "dispose"}}])
        with path.open("a") as f:
            f.write('{"type":"message","message":')
        s = parse_jsonl(path, "omp")
        self.assertEqual((s.id, s.title, s.model, s.provider), ("omp-id", "Сохранённое название", "test-model", "test-provider"))
        self.assertEqual((s.status, s.last_action, s.last_assistant), ("closed", "Проверка сборки", "Сборка прошла"))
        self.assertTrue(s.exact)

    def test_omp_aborted_and_ask_status(self):
        path = self.omp([{"type": "message", "message": {"role": "assistant", "content": [], "stopReason": "aborted"}}])
        self.assertEqual(parse_jsonl(path, "omp").status, "interrupted")
        path = self.omp([{"type": "custom", "customType": "tool_execution_start", "data": {"toolName": "ask", "intent": "Выбор режима"}}])
        self.assertEqual(parse_jsonl(path, "omp").status, "awaiting")

    def test_large_partial_jsonl_keeps_tail_state_and_marks_sample(self):
        path = self.omp([{"type": "irrelevant", "data": "x" * 600000},
                         {"type": "custom", "customType": "session_exit", "data": {"kind": "interrupted"}}])
        sampled = parse_jsonl(path, "omp")
        full = parse_jsonl(path, "omp", full=True)
        self.assertFalse(sampled.exact)
        self.assertTrue(full.exact)
        self.assertEqual(sampled.status, "interrupted")
        self.assertEqual(sampled.title, full.title)

    def test_claude_metadata_and_tool_results_do_not_override_reply(self):
        path = write_jsonl(self.root / "claude.jsonl", [
            {"type": "user", "cwd": str(self.project), "gitBranch": "fix/readme", "message": {"role": "user", "content": "Исправь ошибку"}},
            {"type": "ai-title", "aiTitle": "Починка"},
            {"type": "assistant", "message": {"role": "assistant", "model": "claude-test", "content": [{"type": "text", "text": "Готово"}], "stop_reason": "end_turn"}},
            {"type": "user", "message": {"role": "user", "content": [{"type": "tool_result", "content": "done"}]}},
            {"type": "cost-state", "totalCostUSD": 0.125},
        ])
        s = parse_jsonl(path, "claude")
        self.assertEqual((s.title, s.status, s.last_user, s.last_assistant, s.cost), ("Починка", "ready", "Исправь ошибку", "Готово", 0.125))
        self.assertEqual(s.branch, "fix/readme")

    def test_codex_source_filters_children_and_usage(self):
        path = write_jsonl(self.root / "codex.jsonl", [
            {"type": "session_meta", "payload": {"id": "codex-id", "cwd": str(self.project), "source": {"subagent": {"thread_spawn": {"parent_thread_id": "parent"}}}}},
            {"type": "turn_context", "payload": {"model": "codex-test"}},
            {"type": "response_item", "payload": {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "Сделай тесты"}]}},
            {"type": "event_msg", "payload": {"type": "task_complete", "last_agent_message": "Готово"}},
            {"type": "event_msg", "payload": {"type": "token_count", "info": {"total_token_usage": {"total_tokens": 1234}}}},
        ])
        s = parse_jsonl(path, "codex")
        self.assertEqual((s.id, s.status, s.tokens, s.parent), ("codex-id", "ready", 1234, "parent"))
        self.assertEqual(filter_sessions([s], str(self.project)), [])
        self.assertEqual(filter_sessions([s], str(self.project), children=True), [s])

    def test_agy_database_uri_fts_and_no_source_writes(self):
        root = self.root / "home/.gemini/antigravity-cli"
        root.mkdir(parents=True)
        db = root / "conversation_summaries.db"
        con = sqlite3.connect(db)
        con.execute("CREATE TABLE conversation_summaries (conversation_id TEXT, title TEXT, workspace_uris TEXT, preview TEXT, step_count INTEGER, last_modified_time TEXT, status TEXT, project_id TEXT, parent_conversation_id TEXT)")
        con.execute("INSERT INTO conversation_summaries VALUES (?,?,?,?,?,?,?,?,?)", ("agy-id", "Сессия agy", json.dumps([self.project.as_uri()]), "Вопрос", 14, "2026-11-12T10:00:00Z", "running", "project-id", ""))
        con.execute("CREATE VIRTUAL TABLE conversations_fts USING fts5(conversation_id, chunk_index, user_body, agent_body)")
        con.execute("INSERT INTO conversations_fts VALUES (?,?,?,?)", ("agy-id", "0", "Последний вопрос", "Последний ответ"))
        con.commit()
        con.close()
        before = {p.name: (p.stat().st_mtime_ns, p.read_bytes()) for p in root.iterdir()}
        collector = Collector(self.root / "home", Cache(None, False))
        with patch("ai_sessions.adapters.Path.home", return_value=self.root / "home"):
            sessions = collector.scan_agy(root)
        s = collector.detail(sessions[0])
        self.assertEqual((s.cwd, s.status, s.steps, s.last_assistant), (str(self.project), "tool", 14, "Последний ответ"))
        self.assertEqual(s.argv(), ["agy", "--project", "project-id", "--conversation", "agy-id"])
        self.assertEqual(before, {p.name: (p.stat().st_mtime_ns, p.read_bytes()) for p in root.iterdir()})

    def test_live_sqlite_wal_is_not_ignored(self):
        db = self.root / "live.db"
        con = sqlite3.connect(db)
        con.execute("PRAGMA journal_mode=WAL")
        con.execute("CREATE TABLE items (value TEXT)")
        con.commit()
        con.execute("INSERT INTO items VALUES ('new WAL entry')")
        con.commit()
        with read_database(db) as reader:
            self.assertEqual(reader.execute("SELECT value FROM items").fetchone()[0], "new WAL entry")
        con.close()

    def test_gemini_named_project_mapping(self):
        home = self.root / "home"
        root = home / ".gemini"
        folder = root / "tmp/named/chats"
        folder.mkdir(parents=True)
        (root / "projects.json").write_text(json.dumps({"projects": {str(self.project): "named"}}))
        (folder / "session-one.json").write_text(json.dumps({"sessionId": "gemini-id", "messages": [{"type": "user", "content": "Вопрос"}, {"type": "gemini", "content": "Ответ", "model": "gemini-test"}]}))
        s = Collector(home, Cache(None, False)).scan_gemini(root)[0]
        self.assertTrue(s.in_cwd(str(self.project)))
        self.assertEqual((s.model, s.status), ("gemini-test", "ready"))

    def test_cache_permissions_and_invalidation(self):
        path = self.omp()
        cache = Cache(self.root / "cache")
        s = parse_jsonl(path, "omp")
        cache.put(path, s)
        cache.save()
        self.assertEqual((self.root / "cache/index.json").stat().st_mode & 0o777, 0o600)
        restored = Cache(self.root / "cache").get(path, "omp")
        self.assertEqual(restored.id, s.id)
        with path.open("a") as f:
            f.write("{}\n")
        self.assertIsNone(cache.get(path, "omp"))

    def test_cache_does_not_stamp_stale_result_with_new_file_signature(self):
        path = self.omp()
        session = parse_jsonl(path, "omp")
        with path.open("a") as f:
            f.write('{"type":"custom","customType":"session_exit","data":{"kind":"normal"}}\n')
        cache = Cache(self.root / "cache")
        cache.put(path, session)
        self.assertIsNone(cache.get(path, "omp"))

    def test_duplicate_moved_sessions_keep_newest_file(self):
        old = self.omp()
        newer = write_jsonl(old.parent / "newer.jsonl", [
            {"type": "session", "id": "omp-id", "cwd": str(self.project), "title": "Новое название"},
        ])
        os.utime(old, (1000, 1000))
        os.utime(newer, (2000, 2000))
        collector = Collector(self.root / "home", Cache(None, False))
        sessions = collector.scan_files(self.root / "home/.omp/agent", "omp")
        self.assertEqual(len(sessions), 1)
        self.assertEqual(sessions[0].path, str(newer))

    def test_resume_arguments_use_native_flags(self):
        expected = {
            "omp": ["omp", "--resume", "/tmp/session.jsonl"],
            "claude": ["claude", "--resume", "session-id"],
            "codex": ["codex", "resume", "session-id", "--cd", str(self.project)],
            "agy": ["agy", "--conversation", "session-id"],
            "gemini": ["gemini", "--resume", "session-id"],
            "pi": ["pi", "--session", "/tmp/session.jsonl"],
        }
        for client, argv in expected.items():
            self.assertEqual(Session(client, "session-id", path="/tmp/session.jsonl", cwd=str(self.project)).argv(), argv)
        pi_path = write_jsonl(self.root / "pi.jsonl", [
            {"type": "session", "id": "pi-id", "cwd": str(self.project)},
            {"type": "model_change", "provider": "anthropic", "modelId": "claude-test"},
        ])
        pi_session = parse_jsonl(pi_path, "pi")
        self.assertEqual((pi_session.model, pi_session.provider), ("claude-test", "anthropic"))

    def test_cwd_uses_identity_and_not_path_prefix(self):
        alias = self.root / "symlink"
        alias.symlink_to(self.project)
        sibling = self.root / "Проект с пробелами-другой"
        sibling.mkdir()
        sessions = [Session("omp", "one", cwd=str(self.project), title="Название", model="opus", updated=3),
                    Session("agy", "two", cwd=str(sibling), updated=4)]
        self.assertEqual(len(filter_sessions(sessions, str(alias))), 1)
        self.assertEqual(len(filter_sessions(sessions, str(alias), "all")), 2)
        self.assertEqual(len(filter_sessions(sessions, str(alias), query="НАЗВ opus")), 1)

    def test_cli_resume_is_unambiguous_and_shell_quotes_paths(self):
        path = self.omp()
        with contextlib.redirect_stdout(io.StringIO()) as out:
            self.assertEqual(main(["--home", str(self.root / "home"), "--no-cache", "--resume", "omp-id", "--print-command"]), 0)
        command = out.getvalue()
        self.assertIn("--resume", command)
        self.assertIn("'" + str(self.project) + "'", command)

    def test_launch_validation_no_shell_or_approval_overrides(self):
        s = Session("claude", "id; echo danger", cwd=str(self.project))
        with patch("ai_sessions.model.shutil.which", return_value="/fake/claude"):
            self.assertEqual(validate_launch(s), ["/fake/claude", "--resume", "id; echo danger"])
        self.assertNotIn("dangerously", s.command())

    def test_controls_are_removed_before_render(self):
        self.assertEqual(clean("\x1b]0;bad\x07Hello\x1b[31m!\x00\u202e"), "Hello!")
        self.assertEqual(width(clip("目录路径", 5)), 5)
        self.assertEqual(clip("目录路径", 5), "目录…")

    def test_render_all_terminal_sizes_and_details(self):
        s = parse_jsonl(self.omp(), "omp")
        s.title = "Русский заголовок · 目录 · " + "x" * 100
        for cols, rows in [(30, 10), (44, 16), (60, 24), (80, 30), (112, 36), (144, 44), (200, 60)]:
            v = View(str(self.project), self.root, loaded=True)
            screen = render([s], v, cols, rows)
            self.assertEqual(len(screen.cells), rows)
            self.assertTrue(all(len(row) == cols for row in screen.cells))
            self.assertTrue(all(width(row) <= cols for row in screen.plain().splitlines()))
            self.assertNotIn("\x1b", screen.plain())
        screen = render([s], View(str(self.project), self.root, loaded=True), 144, 44)
        self.assertIn("DETAILS", screen.plain())
        self.assertIn("Current directory", screen.plain())
        self.assertIn("All projects", screen.plain())

    def test_archive_and_children_filters_independent(self):
        rows = [Session("codex", "a", cwd=str(self.project), archived=True),
                Session("codex", "b", cwd=str(self.project), parent="a"),
                Session("claude", "c", cwd=str(self.project))]
        self.assertEqual([s.id for s in filter_sessions(rows, str(self.project))], ["c"])
        self.assertEqual(len(filter_sessions(rows, str(self.project), archived=True, children=True)), 3)


if __name__ == "__main__":
    unittest.main()
