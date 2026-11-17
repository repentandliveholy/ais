import curses
import threading
import time
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import patch

from ai_sessions.model import Session
from ai_sessions.ui import View, browse
from ai_sessions.usage import Usage


class RefreshTests(unittest.TestCase):
    def run_startup(self, *, navigate=False, anchored=False):
        cwd = str(Path.cwd())
        oldest = Session("agy", "old", cwd=cwd, title="SESSION_OLD", updated=100, exact=True)
        middle = Session("claude", "middle", cwd=cwd, title="SESSION_MIDDLE", updated=200, exact=True)
        newest = Session("omp", "new", cwd=cwd, title="SESSION_NEWEST", updated=300, exact=True)
        release = threading.Event()
        view = View(cwd, Path.home(), selected_key=oldest.key if anchored else "")
        titles = []

        def scan(publish):
            publish([oldest, middle])
            release.wait(timeout=2)
            publish([oldest, middle, newest])
            return [oldest, middle, newest]

        collector = SimpleNamespace(scan=scan, detail=lambda s:s, errors=[], sources={},
                                    cache=SimpleNamespace(put=lambda *_:None, save=lambda:None))
        navigated, deadline = False, time.monotonic() + 3

        class Window:
            def keypad(self, _): pass
            def timeout(self, _): pass
            def getmaxyx(self): return 44,144
            def get_wch(self):
                nonlocal navigated
                if titles and not release.is_set():
                    if navigate and not navigated:
                        navigated = True
                        return curses.KEY_DOWN
                    release.set()
                if view.loaded or time.monotonic() > deadline:
                    return "q"
                time.sleep(.01)
                raise curses.error()

        def paint(_, screen, __):
            for row in screen.cells:
                text = "".join(c.char for c in row if c.selected)
                if "SESSION_" in text:
                    titles.append(text.strip())

        with patch("ai_sessions.ui.curses.wrapper", side_effect=lambda f:f(Window())), \
             patch("ai_sessions.ui.curses.set_escdelay"), patch("ai_sessions.ui.curses.curs_set"), \
             patch("ai_sessions.ui.terminal_palette", return_value={}), patch("ai_sessions.ui.paint", side_effect=paint):
            browse(collector, view)
        return view, titles

    def test_fresh_start_selects_first_row_after_all_sources_arrive(self):
        view, titles = self.run_startup()
        self.assertEqual((view.selected, view.top), (0, 0))
        self.assertIn("SESSION_NEWEST", titles[-1])

    def test_navigation_during_startup_keeps_user_selected_session(self):
        view, titles = self.run_startup(navigate=True)
        self.assertEqual(view.selected, 2)
        self.assertIn("SESSION_OLD", titles[-1])

    def test_returning_to_launcher_keeps_explicit_selection_anchor(self):
        view, titles = self.run_startup(anchored=True)
        self.assertEqual(view.selected, 2)
        self.assertIn("SESSION_OLD", titles[-1])

    def run_refresh(self, *, during_initial_scan=False, during_detail=False, reorder=False, remove=False, automatic=False, from_bottom=False):
        cwd = str(Path.cwd())
        selected = Session("omp", "selected", cwd=cwd, title="SESSION_SELECTED", updated=1000, exact=True)
        other = Session("agy", "other", cwd=cwd, title="SESSION_OTHER", updated=100, exact=True)
        newest = Session("claude", "newest", cwd=cwd, title="SESSION_NEWEST", updated=2000, exact=True)
        original = [selected, other]
        if from_bottom:
            original += [Session("omp", str(i), cwd=cwd, title=f"SESSION_{i}", updated=90-i, exact=True)
                         for i in range(20)]
        fresh = [other] if remove else [selected, other, newest] if reorder else original
        view = View(cwd, Path.home())
        scans, selected_titles = [], []
        release = threading.Event()

        def scan(publish):
            scans.append(True)
            if len(scans) == 1:
                if during_initial_scan:
                    release.wait(timeout=2)
                publish(original)
                return original
            publish([other])  # agy is read before the selected OMP session
            time.sleep(.03)
            publish(fresh)
            time.sleep(.02)
            return fresh

        def detail(session):
            if during_detail:
                time.sleep(.06)
            return session

        collector = SimpleNamespace(scan=scan, detail=detail, errors=[], sources={},
                                    cache=SimpleNamespace(put=lambda *_: None, save=lambda: None))
        phase, deadline = (-1 if from_bottom else 0), time.monotonic() + 3

        class Window:
            def keypad(self, _): pass
            def timeout(self, _): pass
            def getmaxyx(self): return 44, 144
            def get_wch(self):
                nonlocal phase
                ready = during_initial_scan or (view.loaded and (not during_detail or view.busy_detail))
                if phase == -1 and ready:
                    phase = 0
                    return curses.KEY_END
                if phase == 0 and ready and not automatic:
                    phase = 1
                    release.set()
                    return "r"
                if (phase or automatic) and len(scans) >= 2 and view.loaded:
                    return "q"
                if time.monotonic() > deadline:
                    return "q"
                time.sleep(.01)
                raise curses.error()

        def paint(_, screen, __):
            for row in screen.cells:
                text = "".join(c.char for c in row if c.selected)
                if "SESSION_" in text:
                    selected_titles.append(text.strip())

        with patch("ai_sessions.ui.curses.wrapper", side_effect=lambda f: f(Window())), \
             patch("ai_sessions.ui.curses.set_escdelay"), patch("ai_sessions.ui.curses.curs_set"), \
             patch("ai_sessions.ui.HISTORY_REFRESH_SECONDS", .03 if automatic else 10), \
             patch("ai_sessions.ui.terminal_palette", return_value={}), patch("ai_sessions.ui.paint", side_effect=paint):
            browse(collector, view)
        return view, selected_titles, len(scans)

    def test_refresh_preserves_session_through_partial_source_batches(self):
        view, titles, scans = self.run_refresh()
        self.assertEqual(scans, 2)
        self.assertEqual(view.query, "")
        self.assertTrue(titles)
        self.assertTrue(all("SESSION_SELECTED" in title for title in titles), titles)

    def test_manual_refresh_selects_first_row_when_newer_session_is_added(self):
        view, titles, _ = self.run_refresh(reorder=True)
        self.assertIn("SESSION_NEWEST", titles[-1])
        self.assertEqual((view.selected, view.top), (0, 0))

    def test_manual_refresh_returns_from_bottom_to_first_row(self):
        view, titles, scans = self.run_refresh(from_bottom=True)
        self.assertEqual(scans, 2)
        self.assertTrue(any("SESSION_19" in title for title in titles))
        self.assertIn("SESSION_SELECTED", titles[-1])
        self.assertEqual((view.selected, view.top), (0, 0))

    def test_automatic_refresh_preserves_identity_through_partial_batches(self):
        view, titles, scans = self.run_refresh(automatic=True, reorder=True)
        self.assertEqual(scans, 2)
        self.assertEqual(view.query, "")
        self.assertTrue(all("SESSION_SELECTED" in title for title in titles), titles)
        self.assertEqual(view.selected, 1)

    def test_refresh_during_initial_load_is_queued_instead_of_searching_for_r(self):
        view, titles, scans = self.run_refresh(during_initial_scan=True, reorder=True)
        self.assertEqual(view.query, "")
        self.assertEqual(scans, 2)
        self.assertIn("SESSION_NEWEST", titles[-1])

    def test_missing_selected_session_falls_back_after_complete_refresh(self):
        view, titles, _ = self.run_refresh(remove=True)
        self.assertEqual(view.selected, 0)
        self.assertIn("SESSION_OTHER", titles[-1])

    def test_refresh_during_detail_read_is_queued_instead_of_changing_query(self):
        view, titles, scans = self.run_refresh(during_detail=True, reorder=True)
        self.assertEqual(view.query, "")
        self.assertEqual(scans, 2)
        self.assertIn("SESSION_NEWEST", titles[-1])

    def run_periodic_usage(self, *, enabled=True, cached=False, hide=False, fail=False):
        view = View(str(Path.cwd()), Path.home(), tab="usage",
                    auto_refresh=enabled, live_usage=not cached)
        calls, finishes = [], []
        collector = SimpleNamespace(scan=lambda publish: [], errors=[], sources={},
                                    cache=SimpleNamespace(save=lambda: None))
        deadline = time.monotonic() + .4
        hidden_at = None

        def fetch(*_):
            calls.append(time.monotonic())
            time.sleep(.06)  # Longer than the interval: never queue duplicates.
            finishes.append(time.monotonic())
            if fail:
                raise TimeoutError("test request timed out")
            return Usage(loaded=True, live=True)

        class Window:
            def keypad(self, _): pass
            def timeout(self, _): pass
            def getmaxyx(self): return 44, 144
            def get_wch(self):
                nonlocal hidden_at
                if hide and finishes and hidden_at is None:
                    hidden_at = time.monotonic()
                    return "s"
                if time.monotonic() > deadline:
                    return "q"
                time.sleep(.005)
                raise curses.error()

        with patch("ai_sessions.ui.curses.wrapper", side_effect=lambda f: f(Window())), \
             patch("ai_sessions.ui.curses.set_escdelay"), patch("ai_sessions.ui.curses.curs_set"), \
             patch("ai_sessions.ui.terminal_palette", return_value={}), patch("ai_sessions.ui.paint"), \
             patch("ai_sessions.ui.USAGE_REFRESH_SECONDS", .03), \
             patch("ai_sessions.ui.load_usage", return_value=Usage(loaded=True)) as load, \
             patch("ai_sessions.ui.fetch_live_usage", side_effect=fetch):
            browse(collector, view)
        return calls, finishes, load.call_count

    def test_usage_timer_waits_for_request_completion_even_after_failure(self):
        for fail in (False, True):
            with self.subTest(fail=fail):
                calls, finishes, _ = self.run_periodic_usage(fail=fail)
                self.assertGreaterEqual(len(calls), 2)
                for start, finish in zip(calls[1:], finishes):
                    self.assertGreaterEqual(start - finish, .03)

    def test_hidden_or_disabled_usage_does_not_repeat_requests(self):
        for options in ({"hide": True}, {"enabled": False}):
            with self.subTest(options=options):
                calls, _, loads = self.run_periodic_usage(**options)
                self.assertEqual(len(calls), 1)
                self.assertEqual(loads, 1)

    def test_cached_usage_timer_never_launches_clis(self):
        calls, _, loads = self.run_periodic_usage(cached=True)
        self.assertFalse(calls)
        self.assertGreaterEqual(loads, 2)


if __name__ == "__main__":
    unittest.main()
