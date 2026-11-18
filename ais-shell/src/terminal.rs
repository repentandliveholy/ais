//! PTY-backed terminal panes.
//!
//! Terminal state is handled by `alacritty_terminal` (the emulator used by
//! Alacritty and Zed). Unlike the earlier `vt100` model it answers terminal
//! queries (DA, DSR cursor position, OSC 10/11 colors, kitty keyboard), tracks
//! bracketed paste / mouse / focus modes and handles wide characters, which the
//! TUI layers of Claude Code, Codex, Gemini and OMP rely on.

use crate::cli::CliKind;
#[cfg(windows)]
use crate::cli::which;
use alacritty_terminal::event::{Event, EventListener, WindowSize};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Line, Point, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::term::{Config, Term, TermMode, viewport_to_point};
use alacritty_terminal::vte::ansi::{Processor, Rgb, StdSyncHandler};
use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};
use std::env;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant};

pub const SCROLLBACK_LINES: usize = 10_000;
/// Maximum OSC 52 clipboard write size accepted from a running program.
pub const MAX_OSC52_BYTES: usize = 256 * 1024;
/// Startup grace before output marks an agent as "working".
pub const BUSY_STARTUP_GRACE: Duration = Duration::from_millis(1500);
/// Output recency threshold for the "working" indicator.
pub const BUSY_IDLE_THRESHOLD: Duration = Duration::from_millis(1200);
/// Quiet time after a busy turn before emitting a "finished" attention alert.
pub const TURN_FINISHED_IDLE: Duration = Duration::from_secs(3);

#[derive(Clone, Debug)]
pub struct ExitInfo {
    pub code: u32,
    pub signal: Option<String>,
}

impl ExitInfo {
    pub fn describe(&self) -> String {
        match &self.signal {
            Some(signal) => signal.clone(),
            None => format!("exit {}", self.code),
        }
    }
}

/// Everything needed to start (or restart) the same program again.
#[derive(Clone, Debug)]
pub struct LaunchSpec {
    pub client: CliKind,
    pub program: String,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    /// Human label for the pane header, e.g. "Claude · continue".
    pub label: String,
}

/// Events the UI thread must act on.
#[derive(Clone, Debug)]
pub enum PaneEvent {
    Title(Option<String>),
    Bell,
    Clipboard(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchMatch {
    pub line: i32,
    pub col: usize,
    pub len: usize,
}

#[derive(Clone, Copy, Debug)]
pub struct QueryColors {
    pub fg: [u8; 3],
    pub bg: [u8; 3],
    pub cursor: [u8; 3],
    pub ansi: [[u8; 3]; 16],
}

impl Default for QueryColors {
    fn default() -> Self {
        Self {
            fg: [220, 223, 228],
            bg: [16, 18, 24],
            cursor: [185, 163, 255],
            ansi: [[128, 128, 128]; 16],
        }
    }
}

/// State shared with the PTY reader thread and the emulator's event proxy.
struct Shared {
    writer: Mutex<Option<Box<dyn Write + Send>>>,
    colors: Mutex<QueryColors>,
    window: Mutex<WindowSize>,
    events: Mutex<Vec<PaneEvent>>,
    waker: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
    /// Milliseconds since `epoch` of the last PTY output.
    last_output: AtomicU64,
    epoch: Instant,
}

impl Shared {
    fn new() -> Self {
        Self {
            writer: Mutex::new(None),
            colors: Mutex::new(QueryColors::default()),
            window: Mutex::new(WindowSize {
                num_lines: 24,
                num_cols: 80,
                cell_width: 8,
                cell_height: 16,
            }),
            events: Mutex::new(Vec::new()),
            waker: Mutex::new(None),
            last_output: AtomicU64::new(0),
            epoch: Instant::now(),
        }
    }

    fn write(&self, bytes: &[u8]) {
        if let Some(writer) = lock(&self.writer).as_mut() {
            let _ = writer.write_all(bytes);
            let _ = writer.flush();
        }
    }

    fn wake(&self) {
        let waker = lock(&self.waker).clone();
        if let Some(waker) = waker {
            waker();
        }
    }

    fn color(&self, index: usize) -> Rgb {
        let colors = *lock(&self.colors);
        let [r, g, b] = match index {
            0..=15 => colors.ansi[index],
            // NamedColor::Foreground = 256, Background = 257, Cursor = 258.
            256 => colors.fg,
            257 => colors.bg,
            258 => colors.cursor,
            16..=231 => {
                const LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];
                let value = index - 16;
                [LEVELS[value / 36], LEVELS[(value % 36) / 6], LEVELS[value % 6]]
            }
            232..=255 => {
                let gray = 8 + (index as u8 - 232) * 10;
                [gray, gray, gray]
            }
            _ => colors.fg,
        };
        Rgb { r, g, b }
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[derive(Clone)]
pub struct Listener(Arc<Shared>);

impl EventListener for Listener {
    fn send_event(&self, event: Event) {
        let shared = &self.0;
        match event {
            Event::PtyWrite(text) => shared.write(text.as_bytes()),
            Event::ColorRequest(index, format) => {
                let reply = format(shared.color(index));
                shared.write(reply.as_bytes());
            }
            Event::TextAreaSizeRequest(format) => {
                let size = *lock(&shared.window);
                shared.write(format(size).as_bytes());
            }
            Event::Title(title) => lock(&shared.events).push(PaneEvent::Title(Some(title))),
            Event::ResetTitle => lock(&shared.events).push(PaneEvent::Title(None)),
            Event::Bell => lock(&shared.events).push(PaneEvent::Bell),
            Event::ClipboardStore(_, text) => {
                if text.len() <= MAX_OSC52_BYTES {
                    lock(&shared.events).push(PaneEvent::Clipboard(text));
                }
            }
            // Programs must not read the user's clipboard behind their back.
            Event::ClipboardLoad(..) => {}
            _ => {}
        }
    }
}

#[derive(Clone, Copy)]
struct GridSize {
    cols: usize,
    lines: usize,
}

impl Dimensions for GridSize {
    fn total_lines(&self) -> usize {
        self.lines
    }

    fn screen_lines(&self) -> usize {
        self.lines
    }

    fn columns(&self) -> usize {
        self.cols
    }
}

pub struct Emulator {
    pub term: Term<Listener>,
    processor: Processor<StdSyncHandler>,
    /// The user's selection. Programs that redraw constantly (Claude Code,
    /// Codex) erase lines all the time, which makes alacritty drop the
    /// selection; it stays pinned until the user clears it.
    pinned: Option<Selection>,
}

impl Emulator {
    fn new(rows: u16, cols: u16, listener: Listener) -> Self {
        let config = Config {
            scrolling_history: SCROLLBACK_LINES,
            kitty_keyboard: true,
            ..Config::default()
        };
        let size = GridSize {
            cols: usize::from(cols.max(2)),
            lines: usize::from(rows.max(1)),
        };
        Self {
            term: Term::new(config, &size, listener),
            processor: Processor::new(),
            pinned: None,
        }
    }

    fn advance(&mut self, bytes: &[u8]) {
        self.processor.advance(&mut self.term, bytes);
        self.keep_selection();
    }

    fn keep_selection(&mut self) {
        match (&self.term.selection, &self.pinned) {
            (None, Some(pinned)) => self.term.selection = Some(pinned.clone()),
            // Scrolling rotates the live selection; follow it.
            (Some(live), _) => self.pinned = Some(live.clone()),
            (None, None) => {}
        }
    }

    fn set_selection(&mut self, selection: Option<Selection>) {
        self.pinned = selection.clone();
        self.term.selection = selection;
    }

    /// Synchronized updates (DEC 2026) are flushed when their timeout expires.
    /// Returns the pending deadline, if any.
    fn check_sync(&mut self) -> Option<Instant> {
        let deadline = self.processor.sync_timeout().sync_timeout()?;
        if deadline <= Instant::now() {
            self.processor.stop_sync(&mut self.term);
            None
        } else {
            Some(deadline)
        }
    }
}

static NEXT_UID: AtomicU64 = AtomicU64::new(1);

pub struct TerminalPane {
    /// Stable identity for widget ids and focus, independent of pane order.
    pub uid: u64,
    pub spec: Option<LaunchSpec>,
    pub exit: Option<ExitInfo>,
    /// Title set by the program through OSC 0/2.
    pub osc_title: Option<String>,
    /// The program asked for attention (bell) or finished while unfocused.
    pub attention: bool,
    pub rows: u16,
    pub cols: u16,
    pub started: Option<Instant>,
    emulator: Arc<Mutex<Emulator>>,
    shared: Arc<Shared>,
    master: Option<Box<dyn MasterPty + Send>>,
    child: Option<Box<dyn Child + Send + Sync>>,
    focused_reported: Option<bool>,
    scroll_remainder: f32,
    was_busy: bool,
    /// The user sent input since the last finished turn (a turn has begun).
    turn_open: bool,
}

impl TerminalPane {
    pub fn empty() -> Self {
        let shared = Arc::new(Shared::new());
        let emulator = Emulator::new(24, 80, Listener(shared.clone()));
        Self {
            uid: NEXT_UID.fetch_add(1, Ordering::Relaxed),
            spec: None,
            exit: None,
            osc_title: None,
            attention: false,
            rows: 24,
            cols: 80,
            started: None,
            emulator: Arc::new(Mutex::new(emulator)),
            shared,
            master: None,
            child: None,
            focused_reported: None,
            scroll_remainder: 0.0,
            was_busy: false,
            turn_open: false,
        }
    }

    pub fn client(&self) -> Option<CliKind> {
        self.spec.as_ref().map(|spec| spec.client)
    }

    pub fn cwd(&self) -> Option<&Path> {
        self.spec.as_ref().map(|spec| spec.cwd.as_path())
    }

    pub fn title(&self) -> String {
        match &self.spec {
            Some(spec) => spec.label.clone(),
            None => "New pane".to_string(),
        }
    }

    pub fn is_running(&self) -> bool {
        self.spec.is_some() && self.exit.is_none()
    }

    pub fn is_empty(&self) -> bool {
        self.spec.is_none()
    }

    pub fn emulator(&self) -> MutexGuard<'_, Emulator> {
        lock(&self.emulator)
    }

    pub fn set_waker(&self, waker: Arc<dyn Fn() + Send + Sync>) {
        *lock(&self.shared.waker) = Some(waker);
    }

    pub fn set_query_colors(&self, colors: QueryColors) {
        *lock(&self.shared.colors) = colors;
    }

    /// Time since the last byte of program output.
    pub fn idle_for(&self) -> Duration {
        let last = self.shared.last_output.load(Ordering::Relaxed);
        let now = self.shared.epoch.elapsed().as_millis() as u64;
        Duration::from_millis(now.saturating_sub(last))
    }

    /// The program produced output very recently (spinners, streaming text).
    pub fn is_busy(&self) -> bool {
        self.is_running()
            && self.started.is_some_and(|at| at.elapsed() > BUSY_STARTUP_GRACE)
            && self.idle_for() < BUSY_IDLE_THRESHOLD
    }

    /// Returns true once when a busy pane goes quiet after the user sent
    /// something (an agent finished a turn). Startup output does not count.
    pub fn take_finished_turn(&mut self) -> bool {
        let busy = self.is_busy();
        let finished = self.turn_open && self.was_busy && !busy && self.idle_for() > TURN_FINISHED_IDLE;
        if finished {
            self.turn_open = false;
        }
        if busy {
            self.was_busy = true;
        } else if finished || !self.is_running() {
            self.was_busy = false;
        }
        finished
    }

    pub fn launch(&mut self, spec: LaunchSpec) -> Result<(), String> {
        if self.is_running() {
            return Err("This pane is already running a program".to_string());
        }
        self.kill();
        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(PtySize {
                rows: self.rows.max(4),
                cols: self.cols.max(20),
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|error| format!("Cannot open PTY: {error}"))?;
        let mut command = build_pty_command(&spec);
        command.cwd(&spec.cwd);
        apply_child_env(&mut command);
        let child = pair
            .slave
            .spawn_command(command)
            .map_err(|error| format!("Cannot start {}: {error}", spec.program))?;
        drop(pair.slave);
        let mut reader = pair
            .master
            .try_clone_reader()
            .map_err(|error| format!("Cannot read PTY: {error}"))?;
        let writer = pair
            .master
            .take_writer()
            .map_err(|error| format!("Cannot write PTY: {error}"))?;

        let shared = Arc::new(Shared::new());
        *lock(&shared.colors) = *lock(&self.shared.colors);
        *lock(&shared.waker) = lock(&self.shared.waker).clone();
        *lock(&shared.window) = *lock(&self.shared.window);
        *lock(&shared.writer) = Some(writer);
        shared
            .last_output
            .store(shared.epoch.elapsed().as_millis() as u64, Ordering::Relaxed);
        let emulator = Arc::new(Mutex::new(Emulator::new(
            self.rows.max(4),
            self.cols.max(20),
            Listener(shared.clone()),
        )));

        let thread_emulator = emulator.clone();
        let thread_shared = shared.clone();
        thread::Builder::new()
            .name(format!("ais-pty-{}", spec.client.command()))
            .spawn(move || {
                let mut buffer = vec![0u8; 64 * 1024];
                loop {
                    match reader.read(&mut buffer) {
                        Ok(0) => break,
                        Ok(size) => {
                            lock(&thread_emulator).advance(&buffer[..size]);
                            thread_shared
                                .last_output
                                .store(thread_shared.epoch.elapsed().as_millis() as u64, Ordering::Relaxed);
                            thread_shared.wake();
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                        Err(_) => break,
                    }
                }
                thread_shared.wake();
            })
            .map_err(|error| format!("Cannot start PTY reader: {error}"))?;

        self.spec = Some(spec);
        self.exit = None;
        self.osc_title = None;
        self.attention = false;
        self.started = Some(Instant::now());
        self.emulator = emulator;
        self.shared = shared;
        self.master = Some(pair.master);
        self.child = Some(child);
        self.focused_reported = None;
        self.was_busy = false;
        self.turn_open = false;
        Ok(())
    }

    pub fn restart(&mut self) -> Result<(), String> {
        let spec = self.spec.clone().ok_or("Nothing to restart")?;
        self.kill();
        self.exit = Some(ExitInfo {
            code: 0,
            signal: Some("restarted".into()),
        });
        self.launch(spec)
    }

    /// Polls the child and emulator; returns the exit status once.
    pub fn poll(&mut self) -> (Option<ExitInfo>, Vec<PaneEvent>, Option<Instant>) {
        let deadline = self.emulator().check_sync();
        let events: Vec<PaneEvent> = std::mem::take(&mut *lock(&self.shared.events));
        for event in &events {
            match event {
                PaneEvent::Title(title) => self.osc_title = title.clone(),
                PaneEvent::Bell => self.attention = true,
                PaneEvent::Clipboard(_) => {}
            }
        }
        let mut exited = None;
        if self.exit.is_none() {
            let status = self.child.as_mut().and_then(|child| child.try_wait().ok().flatten());
            if let Some(status) = status {
                let info = ExitInfo {
                    code: status.exit_code(),
                    signal: status.signal().map(str::to_string),
                };
                self.exit = Some(info.clone());
                self.child = None;
                exited = Some(info);
            }
        }
        (exited, events, deadline)
    }

    pub fn resize(&mut self, rows: u16, cols: u16, cell_width: f32, cell_height: f32) {
        let rows = rows.max(2);
        let cols = cols.max(10);
        if rows == self.rows && cols == self.cols {
            return;
        }
        self.rows = rows;
        self.cols = cols;
        *lock(&self.shared.window) = WindowSize {
            num_lines: rows,
            num_cols: cols,
            cell_width: cell_width.round() as u16,
            cell_height: cell_height.round() as u16,
        };
        self.emulator().term.resize(GridSize {
            cols: usize::from(cols),
            lines: usize::from(rows),
        });
        if let Some(master) = &self.master {
            let _ = master.resize(PtySize {
                rows,
                cols,
                pixel_width: (f32::from(cols) * cell_width) as u16,
                pixel_height: (f32::from(rows) * cell_height) as u16,
            });
        }
    }

    pub fn mode(&self) -> TermMode {
        *self.emulator().term.mode()
    }

    pub fn cursor_viewport_cell(&self) -> Option<(usize, usize)> {
        let emulator = self.emulator();
        let content = emulator.term.renderable_content();
        let row = content.cursor.point.line.0 + content.display_offset as i32;
        if row < 0 || row >= emulator.term.screen_lines() as i32 {
            return None;
        }
        Some((row as usize, content.cursor.point.column.0))
    }

    /// Converts a viewport cell into a grid point (history lines are negative).
    fn grid_point(&self, row: usize, col: usize) -> Point {
        let emulator = self.emulator();
        let offset = emulator.term.grid().display_offset();
        let cols = emulator.term.columns();
        let rows = emulator.term.screen_lines();
        viewport_to_point(
            offset,
            Point::new(row.min(rows.saturating_sub(1)), Column(col.min(cols.saturating_sub(1)))),
        )
    }

    pub fn begin_selection(&mut self, row: usize, col: usize, side_right: bool, clicks: u8) {
        let point = self.grid_point(row, col);
        let kind = match clicks {
            2 => SelectionType::Semantic,
            3 => SelectionType::Lines,
            _ => SelectionType::Simple,
        };
        let side = if side_right { Side::Right } else { Side::Left };
        self.emulator().set_selection(Some(Selection::new(kind, point, side)));
    }

    pub fn update_selection(&mut self, row: usize, col: usize, side_right: bool) {
        let point = self.grid_point(row, col);
        let side = if side_right { Side::Right } else { Side::Left };
        let mut emulator = self.emulator();
        let mut selection = emulator.term.selection.clone().or_else(|| emulator.pinned.clone());
        if let Some(selection) = selection.as_mut() {
            selection.update(point, side);
        }
        emulator.set_selection(selection);
    }

    pub fn clear_selection(&mut self) {
        self.emulator().set_selection(None);
    }

    pub fn has_selection(&self) -> bool {
        self.emulator()
            .term
            .selection
            .as_ref()
            .is_some_and(|selection| !selection.is_empty())
    }

    pub fn select_all(&mut self) {
        let mut emulator = self.emulator();
        let top = Line(-(emulator.term.history_size() as i32));
        let bottom = Line(emulator.term.screen_lines() as i32 - 1);
        let last = Column(emulator.term.columns().saturating_sub(1));
        let mut selection = Selection::new(SelectionType::Simple, Point::new(top, Column(0)), Side::Left);
        selection.update(Point::new(bottom, last), Side::Right);
        emulator.set_selection(Some(selection));
    }

    pub fn selected_text(&self) -> Option<String> {
        self.emulator()
            .term
            .selection_to_string()
            .filter(|text| !text.is_empty())
    }

    /// Returns an OSC 8 hyperlink or plain http(s) URL at viewport `(row, col)`.
    pub fn hyperlink_at(&self, row: usize, col: usize) -> Option<String> {
        let osc8 = {
            let emulator = self.emulator();
            let offset = emulator.term.grid().display_offset();
            let cols = emulator.term.columns();
            let rows = emulator.term.screen_lines();
            if row < rows && col < cols {
                let point = viewport_to_point(offset, Point::new(row, Column(col)));
                emulator.term.grid()[point]
                    .hyperlink()
                    .map(|link| link.uri().to_string())
            } else {
                None
            }
        };
        osc8.or_else(|| crate::input::url_at(&self.row_text(row), col))
    }

    /// Text of one visible row, used for URL detection.
    pub fn row_text(&self, row: usize) -> String {
        let emulator = self.emulator();
        let cols = emulator.term.columns();
        let start = self.grid_point_locked(&emulator, row, 0);
        let end = Point::new(start.line, Column(cols.saturating_sub(1)));
        emulator.term.bounds_to_string(start, end)
    }

    fn grid_point_locked(&self, emulator: &Emulator, row: usize, col: usize) -> Point {
        let offset = emulator.term.grid().display_offset();
        viewport_to_point(offset, Point::new(row, Column(col)))
    }

    /// Case-insensitive substring search across scrollback + screen lines.
    pub fn scrollback_search(&self, query: &str) -> Vec<SearchMatch> {
        let needle = query.trim().to_lowercase();
        if needle.is_empty() {
            return Vec::new();
        }
        let emulator = self.emulator();
        let history = emulator.term.history_size() as i32;
        let screen = emulator.term.screen_lines() as i32;
        let cols = emulator.term.columns();
        let mut matches = Vec::new();
        let last_col = Column(cols.saturating_sub(1));
        for line in -history..screen {
            let row_text = emulator
                .term
                .bounds_to_string(Point::new(Line(line), Column(0)), Point::new(Line(line), last_col));
            let lower = row_text.to_lowercase();
            let mut offset = 0;
            while let Some(byte_at) = lower[offset..].find(&needle) {
                let abs_byte = offset + byte_at;
                let char_col = lower[..abs_byte].chars().count();
                let char_len = needle.chars().count();
                matches.push(SearchMatch {
                    line,
                    col: char_col,
                    len: char_len,
                });
                offset = abs_byte + needle.len().max(1);
                if matches.len() >= 1000 {
                    return matches;
                }
            }
        }
        matches
    }

    /// Scrolls the viewport so `grid_line` is centered when possible.
    pub fn scroll_to_line(&mut self, grid_line: i32) {
        let mut emulator = self.emulator();
        let screen = emulator.term.screen_lines() as i32;
        let history = emulator.term.history_size() as i32;
        let desired_offset = ((screen / 2) - grid_line).clamp(0, history);
        let current = emulator.term.grid().display_offset() as i32;
        emulator.term.scroll_display(Scroll::Delta(desired_offset - current));
    }

    /// Scrolls the local scrollback by whole lines (positive = into history).
    pub fn scroll_lines(&mut self, lines: i32) {
        if lines != 0 {
            self.emulator().term.scroll_display(Scroll::Delta(lines));
        }
    }

    /// Accumulates fractional wheel deltas into whole lines.
    pub fn accumulate_scroll(&mut self, lines: f32) -> i32 {
        self.scroll_remainder += lines;
        let whole = self.scroll_remainder.trunc();
        self.scroll_remainder -= whole;
        whole as i32
    }

    pub fn scroll_page(&mut self, up: bool) {
        self.emulator()
            .term
            .scroll_display(if up { Scroll::PageUp } else { Scroll::PageDown });
    }

    pub fn scroll_to_bottom(&mut self) {
        self.emulator().term.scroll_display(Scroll::Bottom);
    }

    /// Scrolls so that `offset` lines of history are above the viewport.
    pub fn scroll_to_offset(&mut self, offset: usize) {
        let mut emulator = self.emulator();
        let current = emulator.term.grid().display_offset() as i32;
        let target = offset.min(emulator.term.history_size()) as i32;
        emulator.term.scroll_display(Scroll::Delta(target - current));
    }

    /// (display offset, history size, screen lines)
    pub fn scroll_state(&self) -> (usize, usize, usize) {
        let emulator = self.emulator();
        (
            emulator.term.grid().display_offset(),
            emulator.term.history_size(),
            emulator.term.screen_lines(),
        )
    }

    pub fn clear_history(&mut self) {
        let mut emulator = self.emulator();
        emulator.term.grid_mut().clear_history();
        emulator.set_selection(None);
    }

    /// Writes user input; returns to the live screen like a normal terminal.
    pub fn write(&mut self, bytes: &[u8]) {
        if bytes.is_empty() || !self.is_running() {
            return;
        }
        {
            let mut emulator = self.emulator();
            emulator.set_selection(None);
            emulator.term.scroll_display(Scroll::Bottom);
        }
        self.attention = false;
        if bytes.contains(&b'\r') || bytes.contains(&b'\n') {
            self.turn_open = true;
        }
        self.shared.write(bytes);
    }

    /// Writes bytes without touching scroll/selection (mouse reports, focus).
    pub fn write_raw(&self, bytes: &[u8]) {
        if self.is_running() {
            self.shared.write(bytes);
        }
    }

    /// Pastes text, honouring bracketed paste mode and stripping dangerous
    /// control characters in raw mode.
    pub fn paste(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        if self.mode().contains(TermMode::BRACKETED_PASTE) {
            // A pasted ESC could terminate the bracket early; drop it.
            let clean = text.replace('\x1b', "");
            let mut bytes = Vec::with_capacity(clean.len() + 12);
            bytes.extend_from_slice(b"\x1b[200~");
            bytes.extend_from_slice(clean.as_bytes());
            bytes.extend_from_slice(b"\x1b[201~");
            self.write(&bytes);
        } else {
            let normalized = text.replace("\r\n", "\r").replace('\n', "\r");
            let sanitized: String = normalized
                .chars()
                .filter(|&c| c == '\r' || c == '\t' || !c.is_control())
                .collect();
            self.write(sanitized.as_bytes());
        }
    }

    /// Sends focus in/out reports when the program asked for them.
    pub fn report_focus(&mut self, focused: bool) {
        if self.focused_reported == Some(focused) {
            return;
        }
        self.focused_reported = Some(focused);
        self.emulator().term.is_focused = focused;
        if self.mode().contains(TermMode::FOCUS_IN_OUT) {
            self.write_raw(if focused { b"\x1b[I" } else { b"\x1b[O" });
        }
    }

    /// Stops the program without blocking the UI: the hang-up, its grace
    /// period and the final `wait` (so no zombie is left) run on a helper thread.
    pub fn kill(&mut self) {
        self.shutdown(false);
    }

    /// Like `kill`, but returns only once the child is gone. Used on exit,
    /// when a helper thread would not get the chance to run.
    pub fn kill_blocking(&mut self) {
        self.shutdown(true);
    }

    fn shutdown(&mut self, blocking: bool) {
        *lock(&self.shared.writer) = None;
        self.master = None;
        let Some(mut child) = self.child.take() else {
            return;
        };
        let mut reap = move || {
            let _ = child.kill();
            let _ = child.wait();
        };
        if blocking {
            reap();
        } else if let Err(error) = thread::Builder::new().name("ais-reap".into()).spawn(reap) {
            drop(error);
        }
    }
}

impl Drop for TerminalPane {
    fn drop(&mut self) {
        self.kill();
    }
}

/// Resolves `.cmd` / `.bat` wrappers on Windows so `CreateProcessW` via
/// ConPTY can run npm-installed CLIs (`claude.cmd`, `codex.cmd`, etc.).
fn build_pty_command(spec: &LaunchSpec) -> CommandBuilder {
    #[cfg(windows)]
    {
        let resolved = which(&spec.program).unwrap_or_else(|| PathBuf::from(&spec.program));
        let ext = resolved
            .extension()
            .and_then(|e| e.to_str())
            .map(str::to_ascii_lowercase);
        if matches!(ext.as_deref(), Some("cmd" | "bat")) {
            let comspec = env::var("ComSpec").unwrap_or_else(|_| "cmd.exe".to_string());
            let mut command = CommandBuilder::new(comspec);
            command.arg("/C");
            command.arg(resolved);
            command.args(&spec.args);
            return command;
        }
        let mut command = CommandBuilder::new(resolved);
        command.args(&spec.args);
        command
    }
    #[cfg(not(windows))]
    {
        let mut command = CommandBuilder::new(&spec.program);
        command.args(&spec.args);
        command
    }
}

/// The login shell used for "Terminal" panes.
pub fn user_shell() -> String {
    #[cfg(windows)]
    {
        if let Some(shell) = env::var_os("AIS_SHELL")
            .or_else(|| env::var_os("SHELL"))
            .map(PathBuf::from)
            .filter(|path| path.is_file())
        {
            return shell.to_string_lossy().into_owned();
        }
        for candidate in ["pwsh", "powershell"] {
            if let Some(found) = which(candidate) {
                return found.to_string_lossy().into_owned();
            }
        }
        env::var("ComSpec").unwrap_or_else(|_| "cmd.exe".into())
    }
    #[cfg(not(windows))]
    {
        env::var_os("SHELL")
            .map(PathBuf::from)
            .filter(|path| path.is_file())
            .unwrap_or_else(|| PathBuf::from("/bin/sh"))
            .to_string_lossy()
            .into_owned()
    }
}

/// Default interactive flags for `user_shell()`.
pub fn shell_args(shell: &str) -> Vec<String> {
    let name = Path::new(shell)
        .file_stem()
        .and_then(|s| s.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    if cfg!(windows) {
        match name.as_str() {
            "pwsh" | "powershell" => vec!["-NoLogo".into()],
            "bash" | "zsh" | "fish" => vec!["-l".into()],
            _ => Vec::new(),
        }
    } else {
        vec!["-l".into()]
    }
}

fn apply_child_env(command: &mut CommandBuilder) {
    // Warp/IDE terminals often export NO_COLOR=1 / TERM=dumb. The integrated
    // PTY is a real color terminal, so strip those overrides without forcing
    // color on non-TTY subprocesses.
    for key in [
        "NO_COLOR",
        "NODE_DISABLE_COLORS",
        "CLICOLOR_FORCE",
        "WARP_FOCUS_URL",
        "WARP_IS_LOCAL_SHELL_SESSION",
        "WARP_SESSION_ID",
        "WARP_TERMINAL_SESSION_UUID",
        "WARP_CLIENT_VERSION",
        "WARP_HONOR_PS1",
        "WARP_DISABLE_AUTO_TITLE",
        "WARP_ENABLE_WAYLAND",
        "TERM_SESSION_ID",
        "ITERM_SESSION_ID",
        "ITERM_PROFILE",
        "LC_TERMINAL",
        "LC_TERMINAL_VERSION",
        "VSCODE_INJECTION",
    ] {
        command.env_remove(key);
    }
    for (key, _) in env::vars_os() {
        let key = key.to_string_lossy();
        let session_marker = matches!(
            key.as_ref(),
            "CLAUDECODE"
                | "CLAUDE_PID"
                | "CLAUDE_EFFORT"
                | "CODEX_THREAD_ID"
                | "CODEX_SANDBOX"
                | "CODEX_SANDBOX_NETWORK_DISABLED"
        ) || (key.starts_with("CLAUDE_CODE_")
            && matches!(
                &key["CLAUDE_CODE_".len()..],
                "ENTRYPOINT"
                    | "MESSAGING_SOCKET"
                    | "MESSAGING_TOKEN"
                    | "EXECPATH"
                    | "SESSION_ID"
                    | "CHILD_SESSION"
                    | "SESSION_ATTENDED"
                    | "VERSION"
                    | "SSE_PORT"
            ));
        if session_marker {
            command.env_remove(key.as_ref());
        }
    }
    command.env("TERM_PROGRAM", "ais");
    command.env("TERM_PROGRAM_VERSION", env!("CARGO_PKG_VERSION"));
    command.env("TERM", "xterm-256color");
    command.env("COLORTERM", "truecolor");
    if env::var_os("LANG").is_none() {
        command.env("LANG", "en_US.UTF-8");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg_attr(not(unix), allow(dead_code))]
    fn spec(program: &str, args: &[&str]) -> LaunchSpec {
        LaunchSpec {
            client: CliKind::Shell,
            program: program.to_string(),
            args: args.iter().map(|arg| arg.to_string()).collect(),
            cwd: std::env::temp_dir(),
            label: "test".to_string(),
        }
    }

    fn screen_text(pane: &TerminalPane) -> String {
        let emulator = pane.emulator();
        let lines = emulator.term.screen_lines();
        let cols = emulator.term.columns();
        let offset = emulator.term.grid().display_offset();
        let start = viewport_to_point(offset, Point::new(0, Column(0)));
        let end = viewport_to_point(offset, Point::new(lines - 1, Column(cols - 1)));
        emulator.term.bounds_to_string(start, end)
    }

    fn wait_for(pane: &mut TerminalPane, needle: &str) -> Option<ExitInfo> {
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut exit = None;
        while Instant::now() < deadline {
            exit = exit.or_else(|| pane.poll().0);
            if screen_text(pane).contains(needle) {
                return exit;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        panic!("PTY did not show {needle:?}; screen was {:?}", screen_text(pane));
    }

    #[cfg(windows)]
    #[test]
    fn windows_conpty_round_trip() {
        let mut pane = TerminalPane::empty();
        pane.launch(spec("cmd.exe", &["/C", "echo AIS_WIN_PTY_OK"]))
            .expect("spawn cmd.exe in ConPTY");
        wait_for(&mut pane, "AIS_WIN_PTY_OK");
    }

    #[cfg(unix)]
    #[test]
    fn pty_round_trip_selection_and_exit_status() {
        let mut pane = TerminalPane::empty();
        pane.launch(spec(
            "/bin/sh",
            &[
                "-c",
                "printf 'FAKE_READY\\n'; IFS= read -r line; printf 'GOT:%s\\n' \"$line\"",
            ],
        ))
        .expect("spawn fake CLI");
        wait_for(&mut pane, "FAKE_READY");
        pane.begin_selection(0, 0, false, 1);
        pane.update_selection(0, 9, true);
        assert_eq!(pane.selected_text().as_deref(), Some("FAKE_READY"));
        pane.write(b"hello terminal\r");
        let exit = wait_for(&mut pane, "GOT:hello terminal");
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut exit = exit;
        while exit.is_none() && Instant::now() < deadline {
            exit = pane.poll().0;
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(exit.expect("child should exit").code, 0);
    }

    #[cfg(unix)]
    #[test]
    fn child_env_sets_colorterm() {
        let mut pane = TerminalPane::empty();
        pane.launch(spec(
            "/bin/sh",
            &[
                "-c",
                "printf 'NO_COLOR=%s FORCE_COLOR=%s TERM=%s COLORTERM=%s TERM_PROGRAM=%s\\n' \"$NO_COLOR\" \"$FORCE_COLOR\" \"$TERM\" \"$COLORTERM\" \"$TERM_PROGRAM\"; sleep 1",
            ],
        ))
        .expect("spawn env probe");
        wait_for(
            &mut pane,
            "NO_COLOR= FORCE_COLOR= TERM=xterm-256color COLORTERM=truecolor TERM_PROGRAM=ais",
        );
    }

    #[cfg(unix)]
    #[test]
    fn answers_cursor_position_query() {
        let mut pane = TerminalPane::empty();
        pane.launch(spec(
            "/bin/sh",
            &[
                "-c",
                "stty raw -echo; printf 'ab\\033[6n'; reply=$(dd bs=1 count=6 2>/dev/null | od -An -c | tr -d ' \\n'); stty sane; printf '\\nREPLY:%s\\n' \"$reply\"; sleep 1",
            ],
        ))
        .expect("spawn DSR probe");
        wait_for(&mut pane, "REPLY:033[1;3R");
    }

    #[cfg(unix)]
    #[test]
    fn shell_runs_commands_and_bracketed_paste() {
        let mut pane = TerminalPane::empty();
        pane.launch(spec(
            "/bin/sh",
            &[
                "-c",
                "printf '\\033[?2004h'; stty raw -echo; v=$(dd bs=1 count=13 2>/dev/null | od -An -c | tr -d ' \\n'); stty sane; printf '\\nPASTE:%s\\n' \"$v\"; sleep 1",
            ],
        ))
        .expect("spawn paste probe");
        std::thread::sleep(Duration::from_millis(200));
        pane.poll();
        assert!(pane.mode().contains(TermMode::BRACKETED_PASTE));
        pane.paste("x");
        wait_for(&mut pane, "PASTE:033[200~x033[201~");
    }

    #[cfg(unix)]
    #[test]
    fn scrollback_moves_and_returns_to_bottom() {
        let mut pane = TerminalPane::empty();
        pane.resize(12, 40, 8.0, 16.0);
        pane.launch(spec(
            "/bin/sh",
            &[
                "-c",
                "i=1; while [ $i -le 120 ]; do echo LINE$i; i=$((i+1)); done; sleep 2",
            ],
        ))
        .expect("spawn scrollback CLI");
        wait_for(&mut pane, "LINE120");
        let (_, history, _) = pane.scroll_state();
        assert!(history > 0);
        pane.scroll_lines(10);
        assert_eq!(pane.scroll_state().0, 10);
        pane.scroll_to_bottom();
        assert_eq!(pane.scroll_state().0, 0);
    }

    #[cfg(unix)]
    #[test]
    fn restart_relaunches_same_program() {
        let mut pane = TerminalPane::empty();
        pane.launch(spec("/bin/sh", &["-c", "echo RUN_ONCE"])).expect("spawn");
        wait_for(&mut pane, "RUN_ONCE");
        let deadline = Instant::now() + Duration::from_secs(5);
        while pane.exit.is_none() && Instant::now() < deadline {
            pane.poll();
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(pane.exit.is_some());
        pane.restart().expect("restart");
        assert!(pane.is_running());
        wait_for(&mut pane, "RUN_ONCE");
    }
}
