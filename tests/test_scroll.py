import curses
import time
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import patch

from ai_sessions.model import Session
from ai_sessions.ui import TerminalInput, View, WheelScroll, browse, detail_scroll_limit, keep_selection_visible, list_page_size, render, wheel_burst, wheel_down_mask, wheel_key


class ScrollTests(unittest.TestCase):
    def test_sgr_reports_decode_without_native_ncurses_mouse_support(self):
        class Window:
            def __init__(self, text): self.keys = iter(text)
            def timeout(self, _): pass
            def get_wch(self):
                try:
                    return next(self.keys)
                except StopIteration:
                    raise curses.error()

        reader = TerminalInput(Window("\x1b[<65;10;15M\x1b[<64;10;15M\x1b[<66;10;15M\x1bq"))
        self.assertEqual(reader.read(), (curses.KEY_DOWN, True))
        self.assertEqual(reader.read(), (curses.KEY_UP, True))
        self.assertEqual(reader.read(), (None, True))  # Horizontal wheel.
        self.assertEqual(reader.read(), ("\x1b", False))
        self.assertEqual(reader.read(), ("q", False))

    def test_continuous_reports_keep_moving_at_limited_speed_without_backlog(self):
        scroll = WheelScroll()
        self.assertEqual(scroll.step(curses.KEY_DOWN, 0), 1)
        moves = [0.0]
        for i in range(1, 501):
            now = i / 100
            if scroll.step(curses.KEY_DOWN, now):
                moves.append(now)
        self.assertGreater(len(moves), 95)  # Continuous input must not lock up.
        self.assertLessEqual(len(moves), 126)
        self.assertTrue(all(b-a >= .04 for a, b in zip(moves, moves[1:])))
        # Resuming after a pause gives one move, without replaying old reports.
        self.assertEqual(scroll.step(curses.KEY_DOWN, 6), 1)
        self.assertEqual(scroll.step(curses.KEY_DOWN, 6.01), 0)

    def test_reversing_at_either_edge_never_waits_for_inertia_to_stop(self):
        scroll = WheelScroll()
        self.assertEqual(scroll.step(curses.KEY_DOWN, 0), 1)
        self.assertEqual(scroll.step(curses.KEY_DOWN, .01), 0)
        self.assertEqual(scroll.step(curses.KEY_UP, .02), 1)
        self.assertEqual(scroll.step(curses.KEY_UP, .03), 0)
        self.assertEqual(scroll.step(curses.KEY_DOWN, .04), 1)

    def test_wheel_drains_large_queue_keeps_latest_direction_and_keyboard_input(self):
        class Window:
            def __init__(self, keys):
                self.keys = iter(keys)
                self.timeouts = []
            def timeout(self, value): self.timeouts.append(value)
            def get_wch(self):
                try:
                    return next(self.keys)
                except StopIteration:
                    raise curses.error()

        win = Window([curses.KEY_MOUSE] * 10001 + ["q"])
        with patch("ai_sessions.ui.wheel_key", side_effect=[curses.KEY_DOWN] * 10000 + [curses.KEY_UP]):
            self.assertEqual(wheel_burst(win, curses.KEY_DOWN), (curses.KEY_UP, ("q", False)))
        self.assertEqual(wheel_burst(win, curses.KEY_UP), (curses.KEY_UP, None))
        self.assertEqual(win.timeouts, [0, 100, 0, 100])

    def test_wheel_handles_both_directions_with_native_curses_masks(self):
        for buttons, expected in ((curses.BUTTON4_PRESSED, curses.KEY_UP),
                                  (wheel_down_mask(), curses.KEY_DOWN), (0, None)):
            with patch("ai_sessions.ui.curses.getmouse", return_value=(0, 10, 15, 0, buttons)):
                self.assertEqual(wheel_key(), expected)

    def test_large_stale_bursts_at_both_edges_do_not_block_reverse_or_keyboard(self):
        cwd = str(Path.cwd())
        sessions = [Session("omp", str(i), cwd=cwd, title=f"Session {i:02}",
                            updated=1000-i, exact=True) for i in range(20)]
        view = View(cwd, Path.home(), auto_refresh=False)
        collector = SimpleNamespace(scan=lambda publish: sessions, detail=lambda s: s,
                                    errors=[], cache=SimpleNamespace(put=lambda *_: None, save=lambda: None))
        keys = iter([curses.KEY_END] + [curses.KEY_MOUSE] * 10001 +
                    [curses.KEY_HOME] + [curses.KEY_MOUSE] * 10001 + ["q"])
        directions = ([curses.KEY_DOWN] * 10000 + [curses.KEY_UP] +
                      [curses.KEY_UP] * 10000 + [curses.KEY_DOWN])
        states = []

        class Window:
            def keypad(self, _): pass
            def timeout(self, _): pass
            def getmaxyx(self): return 44, 144
            def get_wch(self):
                if not view.loaded:
                    time.sleep(.001)
                    raise curses.error()
                return next(keys)

        with patch("ai_sessions.ui.curses.wrapper", side_effect=lambda f: f(Window())), \
             patch("ai_sessions.ui.curses.set_escdelay"), patch("ai_sessions.ui.curses.curs_set"), \
             patch("ai_sessions.ui.terminal_palette", return_value={}), \
             patch("ai_sessions.ui.wheel_key", side_effect=directions), \
             patch("ai_sessions.ui.paint", side_effect=lambda *_: states.append(view.selected)):
            browse(collector, view)
        # There must be no intermediate replay of the 20,000 old reports.
        changes = [value for i, value in enumerate(states) if not i or value != states[i-1]]
        self.assertEqual(changes, [0, 19, 18, 0, 1])

    def test_up_moves_selection_inside_viewport_before_scrolling_window(self):
        cwd = str(Path.cwd())
        sessions = [Session("omp", str(i), cwd=cwd, title=f"Session {i:02}",
                            updated=1000-i, exact=True) for i in range(30)]
        view = View(cwd, Path.home())
        states = []

        def scan(publish):
            publish(sessions)
            return sessions

        collector = SimpleNamespace(scan=scan, detail=lambda s: s, errors=[],
                                    cache=SimpleNamespace(put=lambda *_: None, save=lambda: None))
        keys = iter([curses.KEY_DOWN] * 12 + [curses.KEY_UP] * 3 + ["q"])

        class Window:
            wait_ms = 100
            def keypad(self, _): pass
            def timeout(self, value): self.wait_ms = value
            def getmaxyx(self): return 44, 144
            def get_wch(self):
                if not view.loaded:
                    time.sleep(.01)
                    raise curses.error()
                # These are deliberate separate key presses, not a buffered
                # terminal-emulated scroll burst.
                if self.wait_ms == 0:
                    raise curses.error()
                clock[0] += .4
                return next(keys)

        clock = [0.0]
        with patch("ai_sessions.ui.curses.wrapper", side_effect=lambda f: f(Window())), \
             patch("ai_sessions.ui.curses.set_escdelay"), patch("ai_sessions.ui.curses.curs_set"), \
             patch("ai_sessions.ui.time", SimpleNamespace(monotonic=lambda: clock[0])), \
             patch("ai_sessions.ui.terminal_palette", return_value={}), \
             patch("ai_sessions.ui.paint", side_effect=lambda *_: states.append((view.selected, view.top))):
            browse(collector, view)
        peak = next(i for i, state in enumerate(states) if state[0] == 12)
        before = states[peak]
        after = next(state for state in states[peak+1:] if state[0] == 11)
        self.assertEqual(after[1], before[1], "Moving up inside the viewport must not scroll the list under a pinned cursor")

    def test_list_moves_window_only_when_selection_crosses_visible_edge(self):
        view = View(str(Path.cwd()), Path.home(), selected=15, top=10)
        page = list_page_size(144, 44)
        for index in (14, 13, 12, 11, 10):
            view.selected = index
            keep_selection_visible(view, 30, page)
            self.assertEqual(view.top, 10)
        view.selected = 9
        keep_selection_visible(view, 30, page)
        self.assertEqual(view.top, 9)
        view.selected = 10000
        keep_selection_visible(view, 30, page)
        self.assertEqual(view.selected, 29)
        view.selected -= 1
        top = view.top
        keep_selection_visible(view, 30, page)
        self.assertEqual(view.top, top)

    def test_detail_scroll_clamps_to_real_viewport_after_resize(self):
        cwd = str(Path.cwd())
        session = Session("omp", "details", cwd=cwd, title="Details", exact=True,
                          last_assistant="\n".join(f"detail-line-{i:03}" for i in range(100)))
        view = View(cwd, Path.home(), loaded=True, narrow_detail=True, detail_scroll=10000)
        render([session], view, 80, 30)
        self.assertEqual(view.detail_scroll, detail_scroll_limit(session, view, 80, 30))
        before = view.detail_scroll
        view.detail_scroll -= 5
        render([session], view, 80, 30)
        self.assertEqual(view.detail_scroll, before - 5)
        view.detail_scroll = 10000
        screen = render([session], view, 144, 60)
        self.assertEqual(view.detail_scroll, detail_scroll_limit(session, view, 144, 60))
        self.assertIn("detail-line-099", screen.plain())


if __name__ == "__main__":
    unittest.main()
