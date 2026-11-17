import curses
import tempfile
import time
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import patch

from ai_sessions.i18n import set_language
from ai_sessions.model import Project, Session, folder_entries, group_projects, age
from ai_sessions.ui import View, browse, enter_folder, enter_project, leave_folder, open_projects, render, visible_entries, project_detail_lines, detail_scroll_limit, date


class ProjectTests(unittest.TestCase):
    def setUp(self):
        self.sessions = [Session('omp', 'a', cwd='/projects/alpha', updated=20),
                         Session('codex', 'b', cwd='/projects/alpha', updated=30),
                         Session('claude', 'c', cwd='/other/alpha', updated=10),
                         Session('agy', 'd', cwd='/projects/beta', workspaces=['/projects/alpha'], updated=40),
                         Session('omp', 'child', cwd='/projects/beta', parent='d'),
                         Session('omp', 'archive', cwd='/projects/beta', archived=True),
                         Session('pi', 'unknown')]

    def tearDown(self):
        set_language('en')

    def test_groups_distinguish_full_paths_and_include_workspaces(self):
        projects = group_projects(self.sessions)
        self.assertEqual([p.cwd for p in projects], ['/projects/alpha', '/projects/beta', '/other/alpha', ''])
        alpha = projects[0]
        self.assertEqual({s.id for s in alpha.sessions}, {'a', 'b', 'd'})
        self.assertEqual(alpha.updated, 40)
        self.assertEqual(len(next(p for p in group_projects(self.sessions, children=True, archived=True) if p.cwd == '/projects/beta').sessions), 3)
        self.assertEqual([p.cwd for p in group_projects(self.sessions, client='codex')], ['/projects/alpha'])
        self.assertEqual([p.cwd for p in group_projects(self.sessions, query='/other')], ['/other/alpha'])

    def test_projects_follow_latest_session_update(self):
        self.sessions[2].updated = 100
        projects = group_projects(self.sessions)
        self.assertEqual(projects[0].cwd, '/other/alpha')
        self.assertEqual([p.updated for p in projects], [100, 40, 40, 0])

    def test_subfolders_are_nested_inside_the_project_tree(self):
        sessions = [
            Session('omp', 'root', cwd='/projects', updated=10),
            Session('omp', 'alpha', cwd='/projects/alpha', updated=30),
            Session('omp', 'deep', cwd='/projects/alpha/deep', updated=20),
            Session('omp', 'beta', cwd='/projects/beta', updated=40),
            Session('omp', 'sibling', cwd='/projects/alpha2', updated=50),
        ]
        flat = group_projects(sessions)
        self.assertEqual([p.cwd for p in flat],
                         ['/projects/alpha2', '/projects/beta', '/projects/alpha',
                          '/projects/alpha/deep', '/projects'])
        self.assertTrue(all(p.depth == 0 and p.prefix == '' for p in flat))
        tree = group_projects(sessions, tree=True)
        self.assertEqual([p.cwd for p in tree],
                         ['/projects', '/projects/alpha2', '/projects/beta',
                          '/projects/alpha', '/projects/alpha/deep'])
        self.assertEqual([p.depth for p in tree], [0, 1, 1, 1, 2])
        self.assertEqual([p.prefix for p in tree],
                         ['', '├─ ', '├─ ', '└─ ', '   └─ '])
        root = tree[0]
        self.assertEqual([p.cwd for p in root.children],
                         ['/projects/alpha2', '/projects/beta', '/projects/alpha'])
        self.assertEqual(root.subtree_updated, 50)
        self.assertEqual(root.updated, 10)

    def test_folder_view_lists_direct_subfolders_then_own_sessions(self):
        sessions = [
            Session('omp', 'root', cwd='/projects', updated=10),
            Session('omp', 'alpha', cwd='/projects/alpha', updated=30),
            Session('omp', 'deep', cwd='/projects/alpha/deep', updated=20),
            Session('omp', 'beta', cwd='/projects/beta', updated=40),
            Session('omp', 'sibling', cwd='/projects/alpha2', updated=50),
        ]
        entries = folder_entries(sessions, '/projects')
        self.assertEqual([entry.cwd if isinstance(entry, Project) else entry.id for entry in entries],
                         ['/projects/alpha2', '/projects/beta', '/projects/alpha', 'root'])
        alpha = folder_entries(sessions, '/projects/alpha')
        self.assertEqual([entry.cwd if isinstance(entry, Project) else entry.id for entry in alpha],
                         ['/projects/alpha/deep', 'alpha'])
        self.assertEqual([entry.id for entry in folder_entries(sessions, '/projects/beta')], ['beta'])
        self.assertEqual(folder_entries(sessions, '/missing'), [])

    def test_path_prefix_without_a_separator_is_not_a_subfolder(self):
        projects = group_projects([Session('omp', 'a', cwd='/projects/alpha'),
                                   Session('omp', 'b', cwd='/projects/alpha2')])
        self.assertEqual([p.cwd for p in projects], ['/projects/alpha', '/projects/alpha2'])
        self.assertTrue(all(p.depth == 0 and p.prefix == '' and not p.children for p in projects))

    def test_symlink_alias_is_one_project(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            (root / 'real').mkdir()
            (root / 'alias').symlink_to(root / 'real')
            projects = group_projects([Session('omp', 'a', cwd=str(root / 'real')),
                                       Session('codex', 'b', cwd=str(root / 'alias'))])
            self.assertEqual(len(projects), 1)
            self.assertEqual(len(projects[0].sessions), 2)

    def test_enter_project_and_restore_project_position(self):
        v = View('/unrelated', Path.home(), tab='projects', selected=2, top=1)
        project = next(p for p in visible_entries(self.sessions, v) if p.cwd == '/projects/alpha')
        enter_project(v, project)
        self.assertEqual({s.id for s in visible_entries(self.sessions, v)}, {'a', 'b', 'd'})
        v.query = 'b'
        open_projects(v)
        self.assertEqual((v.tab, v.selected, v.top, v.query), ('projects', 2, 1, ''))
        enter_project(v, Project('', []))
        self.assertEqual([s.id for s in visible_entries(self.sessions, v)], ['unknown'])

    def test_tree_toggle_and_folder_drill(self):
        sessions = [Session('omp', 'root', cwd='/projects', updated=10),
                    Session('omp', 'alpha', cwd='/projects/alpha', updated=30),
                    Session('omp', 'deep', cwd='/projects/alpha/deep', updated=20)]
        v = View('/unrelated', Path.home(), tab='projects')
        self.assertFalse(v.tree)
        self.assertEqual([p.cwd for p in visible_entries(sessions, v)],
                         ['/projects/alpha', '/projects/alpha/deep', '/projects'])
        v.tree = True
        tree = visible_entries(sessions, v)
        self.assertEqual([p.cwd for p in tree],
                         ['/projects', '/projects/alpha', '/projects/alpha/deep'])
        self.assertEqual([p.prefix for p in tree], ['', '└─ ', '   └─ '])
        v.tree = False
        v.selected = 2
        enter_folder(v, visible_entries(sessions, v)[2])
        self.assertEqual(v.folder_path, '/projects')
        entries = visible_entries(sessions, v)
        self.assertEqual([entry.cwd if isinstance(entry, Project) else entry.id for entry in entries],
                         ['/projects/alpha', 'root'])
        leave_folder(v)
        self.assertIsNone(v.folder_path)
        self.assertEqual(v.selected, 2)

    def test_render_projects_in_both_languages_and_terminal_sizes(self):
        for lang, heading in [('en', 'PROJECTS'), ('ru', 'ПРОЕКТЫ')]:
            set_language(lang)
            for cols, rows in [(44, 16), (80, 30), (144, 44)]:
                v = View('/unrelated', Path.home(), tab='projects', loaded=True)
                text = render(self.sessions, v, cols, rows).plain()
                self.assertIn(heading, text)
                self.assertIn('[P]', text)
                if cols >= 112:
                    right = '\n'.join(row[cols // 2 + 4:] for row in text.splitlines())
                    self.assertIn('/projects/alpha', right)
                    self.assertIn('RECENT SESSIONS' if lang == 'en' else 'НЕДАВНИЕ СЕССИИ', right)
                else:
                    self.assertNotIn('DETAILS', text)

    def test_project_details_scroll_and_expand(self):
        self.sessions[0].model = 'test-model'
        self.sessions[0].branch = 'feature/details'
        self.sessions[0].last_action = 'Recent project action'
        self.sessions.extend(Session('omp', f'extra-{i}', cwd='/projects/alpha', title=f'Extra {i}', updated=1) for i in range(15))
        project = group_projects(self.sessions)[0]
        compact = '\n'.join(text for text, _ in project_detail_lines(project, 60, Path.home()))
        self.assertNotIn('Recent project action', compact)
        self.assertNotIn('Extra 14', compact)
        view = View('/unrelated', Path.home(), tab='projects', loaded=True)
        self.assertEqual(detail_scroll_limit(project, view, 144, 44), 0)
        view.narrow_detail = True
        details = '\n'.join(text for text, _ in project_detail_lines(project, 60, Path.home(), expanded=True))
        for value in ('test-model', 'feature/details', 'Recent project action', 'Extra 14'):
            self.assertIn(value, details)
        limit = detail_scroll_limit(project, view, 144, 44)
        self.assertGreater(limit, 0)
        view.detail_scroll = limit
        self.assertIn('Extra 14', render(self.sessions, view, 144, 44).plain())

    def test_invalid_history_dates_do_not_crash_project_or_session_details(self):
        for invalid in (-62167219200, float('nan'), float('inf'), 1e30):
            with self.subTest(timestamp=invalid):
                self.assertEqual(date(invalid), '—')
                self.assertEqual(age(invalid), '—')
                session = Session('omp', 'bad-date', cwd='/projects/alpha',
                                  created=invalid, updated=invalid)
                for tab in ('projects', 'sessions'):
                    for expanded in (False, True):
                        view = View('/projects/alpha', Path.home(), tab=tab,
                                    narrow_detail=expanded, loaded=True)
                        self.assertIn('—', render([session], view, 144, 44).plain())

    def test_keyboard_open_escape_and_resume(self):
        session = Session('omp', 'a', cwd='/projects/alpha', title='TARGET', exact=True)
        collector = SimpleNamespace(scan=lambda publish: [session], detail=lambda s: s,
                                    errors=[], sources={}, cache=SimpleNamespace(save=lambda: None))
        view = View('/unrelated', Path.home())
        keys = iter(['p', '\n', '\x1b', '\n', '\n'])
        states = []
        deadline = time.monotonic() + 2

        class Window:
            def keypad(self, _): pass
            def timeout(self, _): pass
            def getmaxyx(self): return 44, 144
            def get_wch(self):
                if time.monotonic() > deadline:
                    return 'q'
                if not view.loaded:
                    time.sleep(.005)
                    raise curses.error()
                return next(keys, 'q')

        with patch('ai_sessions.ui.curses.wrapper', side_effect=lambda f: f(Window())), \
             patch('ai_sessions.ui.curses.set_escdelay'), patch('ai_sessions.ui.curses.curs_set'), \
             patch('ai_sessions.ui.terminal_palette', return_value={}), \
             patch('ai_sessions.ui.paint', side_effect=lambda *_: states.append((view.tab, view.project_path)) if view.loaded else None), \
             patch('ai_sessions.ui.validate_launch'):
            result = browse(collector, view)
        self.assertIs(result, session)
        self.assertEqual(states, [('sessions', None), ('projects', None),
                                  ('sessions', '/projects/alpha'), ('projects', None),
                                  ('sessions', '/projects/alpha')])

    def test_backspace_returns_to_projects_and_edits_search(self):
        session = Session('omp', 'a', cwd='/projects/alpha', title='TARGET', exact=True)
        collector = SimpleNamespace(scan=lambda publish: [session], detail=lambda s: s,
                                    errors=[], sources={}, cache=SimpleNamespace(save=lambda: None))
        view = View('/unrelated', Path.home(), tab='projects')
        keys = iter(['\n', '\x7f', 's', '\x7f', '/', 'ab', '\x7f', '\n', 'q'])
        states = []
        deadline = time.monotonic() + 2

        class Window:
            def keypad(self, _): pass
            def timeout(self, _): pass
            def getmaxyx(self): return 44, 144
            def get_wch(self):
                if time.monotonic() > deadline:
                    return 'q'
                if not view.loaded:
                    time.sleep(.005)
                    raise curses.error()
                return next(keys, 'q')

        def paint(*_):
            state = (view.tab, view.project_path)
            if view.loaded and (not states or states[-1] != state):
                states.append(state)

        with patch('ai_sessions.ui.curses.wrapper', side_effect=lambda f: f(Window())), \
             patch('ai_sessions.ui.curses.set_escdelay'), patch('ai_sessions.ui.curses.curs_set'), \
             patch('ai_sessions.ui.terminal_palette', return_value={}), \
             patch('ai_sessions.ui.paint', side_effect=paint), \
             patch('ai_sessions.ui.validate_launch'):
            result = browse(collector, view)
        self.assertIsNone(result)
        self.assertEqual(states, [('projects', None), ('sessions', '/projects/alpha'),
                                  ('projects', None), ('sessions', None),
                                  ('projects', None)])
        self.assertEqual(view.query, 'a')

    def test_backspace_leaves_a_project_folder(self):
        sessions = [Session('omp', 'root', cwd='/projects', updated=100),
                    Session('omp', 'child', cwd='/projects/alpha', updated=50)]
        collector = SimpleNamespace(scan=lambda publish: sessions, detail=lambda s: s,
                                    errors=[], sources={}, cache=SimpleNamespace(save=lambda: None))
        view = View('/unrelated', Path.home(), tab='projects')
        keys = iter(['\n', '\x7f', 'q'])
        states = []
        deadline = time.monotonic() + 2

        class Window:
            def keypad(self, _): pass
            def timeout(self, _): pass
            def getmaxyx(self): return 44, 144
            def get_wch(self):
                if time.monotonic() > deadline:
                    return 'q'
                if not view.loaded:
                    time.sleep(.005)
                    raise curses.error()
                return next(keys, 'q')

        def paint(*_):
            state = (view.tab, view.folder_path, view.project_path)
            if view.loaded and (not states or states[-1] != state):
                states.append(state)

        with patch('ai_sessions.ui.curses.wrapper', side_effect=lambda f: f(Window())), \
             patch('ai_sessions.ui.curses.set_escdelay'), patch('ai_sessions.ui.curses.curs_set'), \
             patch('ai_sessions.ui.terminal_palette', return_value={}), \
             patch('ai_sessions.ui.paint', side_effect=paint), \
             patch('ai_sessions.ui.validate_launch'):
            result = browse(collector, view)
        self.assertIsNone(result)
        self.assertEqual(states, [('projects', None, None),
                                  ('projects', '/projects', None),
                                  ('projects', None, None)])

    def test_o_opens_folder_sessions_and_t_toggles_tree(self):
        sessions = [Session('omp', 'root', cwd='/projects', updated=100),
                    Session('omp', 'child', cwd='/projects/alpha', updated=50)]
        collector = SimpleNamespace(scan=lambda publish: sessions, detail=lambda s: s,
                                    errors=[], sources={}, cache=SimpleNamespace(save=lambda: None))
        view = View('/unrelated', Path.home(), tab='projects')
        keys = iter(['o', '\x7f', 't', 't', 'q'])
        states = []
        deadline = time.monotonic() + 2

        class Window:
            def keypad(self, _): pass
            def timeout(self, _): pass
            def getmaxyx(self): return 44, 144
            def get_wch(self):
                if time.monotonic() > deadline:
                    return 'q'
                if not view.loaded:
                    time.sleep(.005)
                    raise curses.error()
                return next(keys, 'q')

        def paint(*_):
            state = (view.tab, view.project_path, view.tree)
            if view.loaded and (not states or states[-1] != state):
                states.append(state)

        with patch('ai_sessions.ui.curses.wrapper', side_effect=lambda f: f(Window())), \
             patch('ai_sessions.ui.curses.set_escdelay'), patch('ai_sessions.ui.curses.curs_set'), \
             patch('ai_sessions.ui.terminal_palette', return_value={}), \
             patch('ai_sessions.ui.paint', side_effect=paint), \
             patch('ai_sessions.ui.validate_launch'):
            result = browse(collector, view)
        self.assertIsNone(result)
        self.assertEqual(states, [('projects', None, False),
                                  ('sessions', '/projects', False),
                                  ('projects', None, False),
                                  ('projects', None, True),
                                  ('projects', None, False)])
