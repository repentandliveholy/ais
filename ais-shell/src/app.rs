use crate::cli::{CliKind, canonical_path, home_dir};
use crate::input::split_args;
use crate::keys::{Binding, Command as Cmd, MOD, fixed_conflict_reason};
use crate::log;
use crate::projects::{CliSnapshot, Project, ScanCache, ScanResult, scan_projects_cached};
use crate::sessions::{Session, SessionStore, find_ais, truncate};
use crate::terminal::{LaunchSpec, PaneEvent, QueryColors, SearchMatch, TerminalPane, shell_args, user_shell};
use crate::theme::{
    DEFAULT_FONT_SIZE, FontChoice, Palette, Settings, SpaceLayout, TerminalPalette, Theme, UiTheme, settings_path,
};
use crate::update::{self, Installed, UpdateStatus};
use crate::usage::UsageStore;
use crate::view::BOLD_FAMILY;
use crate::widgets::{choose_folder, load_first, open_external, plural, system_notification};
use eframe::egui::{self, Color32, FontData, FontDefinitions, FontFamily, Id, Key, Modifiers, Rect, Vec2};
use std::collections::HashMap;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::{Arc, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

pub const APP_NAME: &str = "ais";
pub const APP_TITLE: &str = "ais · AI Sessions";
pub const MAX_PANES: usize = 4;
pub const REVEAL_LABEL: &str = if cfg!(target_os = "macos") {
    "Reveal in Finder"
} else if cfg!(target_os = "windows") {
    "Show in Explorer"
} else {
    "Show in file manager"
};
pub const NARROW_WIDTH: f32 = 1100.0;
pub const SEARCH_ID: &str = "title-search";
pub const TITLE_HEIGHT: f32 = 38.0;

/// macOS without the system title bar: ais paints its own, window buttons
/// included. Decided once at startup (the setting needs a restart).
pub fn custom_titlebar() -> bool {
    static VALUE: OnceLock<bool> = OnceLock::new();
    *VALUE.get_or_init(|| {
        cfg!(target_os = "macos") && !Settings::load_checked(&settings_path(&home_dir())).0.native_titlebar
    })
}

/// Width limits of a side panel, so it never eats the workspace.
pub fn side_width(ctx: &egui::Context, default: f32) -> (f32, std::ops::RangeInclusive<f32>) {
    let screen = ctx.screen_rect().width();
    let max = (screen * 0.34).clamp(200.0, 480.0);
    (default.min(max), 200.0..=max)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LaunchMode {
    New,
    Continue,
}

/// Where a launch lands: the given pane if it is empty, otherwise a new split.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    Auto,
    Pane(u64),
    Split,
}

#[derive(Clone, Debug)]
pub enum Action {
    OpenProject(PathBuf),
    PickFolder,
    LaunchIn(CliKind, PathBuf),
    ShellIn(PathBuf),
    SelectSpace(usize),
    CloseSpace(usize),
    MoveSpace(usize, isize),
    RetrySpace(usize),
    Launch(CliKind, LaunchMode, Target),
    Shell(Target),
    Resume(Box<Session>, Target),
    Split(bool),
    ClosePane(u64),
    RestartPane(u64),
    FocusPane(u64),
    FocusNext(bool),
    CycleSpace(bool),
    ToggleZoom,
    ToggleFullscreenTerminal,
    ToggleWindowFullscreen,
    ClearScrollback,
    OpenFind,
    InsertText(String),
    CopyText(String, &'static str),
    OpenUrl(String),
    Reveal(PathBuf),
    ToggleTheme,
    SetUiTheme(UiTheme),
    ToggleTitleBar,
    ToggleSidebar,
    ToggleFiles,
    ToggleUsage,
    ToggleToolbar,
    ToggleAgents,
    TogglePaneHeaders,
    ToggleStatusBar,
    ShowAllBlocks,
    RefreshUsage,
    FocusAgent(CliKind),
    ShowSessions(bool),
    OpenSettings,
    OpenPalette,
    OpenNotices,
    ClearNotices,
    SetRightTab(Option<RightTab>),
    RefreshProjects,
    RefreshSessions,
    FontSize(f32),
    CheckUpdates,
    RestartToUpdate,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RightTab {
    Files,
    Usage,
    Notices,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SettingsTab {
    Appearance,
    Terminal,
    Keyboard,
    Clis,
    About,
}

impl SettingsTab {
    pub const ALL: [SettingsTab; 5] = [
        SettingsTab::Appearance,
        SettingsTab::Terminal,
        SettingsTab::Keyboard,
        SettingsTab::Clis,
        SettingsTab::About,
    ];

    pub fn label(self) -> &'static str {
        match self {
            SettingsTab::Appearance => "Appearance",
            SettingsTab::Terminal => "Terminal",
            SettingsTab::Keyboard => "Keyboard",
            SettingsTab::Clis => "CLIs",
            SettingsTab::About => "About",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NoticeKind {
    Info,
    Success,
    Error,
    Attention,
}

pub struct Notice {
    pub at: Instant,
    pub kind: NoticeKind,
    pub text: String,
}

#[derive(Clone)]
pub struct CliStatus {
    pub installed: bool,
    pub snapshot: Option<CliSnapshot>,
}

#[derive(Clone)]
pub struct FileEntry {
    pub name: String,
    pub path: PathBuf,
    pub is_dir: bool,
    pub size: u64,
}

#[derive(Clone, Default)]
pub struct PaneSearch {
    pub open: bool,
    pub query: String,
    pub matches: Vec<SearchMatch>,
    pub current: usize,
    pub focus_input: bool,
}

type FilesReceiver = Receiver<(PathBuf, PathBuf, Result<Vec<FileEntry>, String>)>;

pub struct PendingPaste {
    pub pane_uid: u64,
    pub text: String,
    pub lines: usize,
}

pub struct Space {
    pub path: PathBuf,
    pub name: String,
    pub available: bool,
    pub panes: Vec<TerminalPane>,
    pub active: usize,
    /// Second pane went below the first (rows) instead of to the right.
    pub stacked: bool,
    pub zoomed: bool,
    pub split_x: f32,
    pub split_y: f32,
    pub files_dir: PathBuf,
    pub files: Vec<FileEntry>,
    pub files_error: Option<String>,
    pub files_show_hidden: bool,
    pub files_filter: String,
    pub files_dirty: bool,
    pub last_files_refresh: Instant,
}

impl Space {
    pub fn new(path: PathBuf, layout: Option<SpaceLayout>) -> Self {
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .filter(|name| !name.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| path.display().to_string());
        let available = path.is_dir();
        let layout = layout.unwrap_or(SpaceLayout {
            stacked: false,
            split_x: 0.5,
            split_y: 0.5,
        });
        Self {
            files_dir: path.clone(),
            path,
            name,
            available,
            panes: Vec::new(),
            active: 0,
            stacked: layout.stacked,
            zoomed: false,
            split_x: layout.split_x,
            split_y: layout.split_y,
            files: Vec::new(),
            files_error: None,
            files_show_hidden: false,
            files_filter: String::new(),
            files_dirty: available,
            last_files_refresh: Instant::now() - Duration::from_secs(10),
        }
    }

    pub fn running_count(&self) -> usize {
        self.panes.iter().filter(|pane| pane.is_running()).count()
    }

    pub fn busy(&self) -> bool {
        self.panes
            .iter()
            .any(|pane| pane.is_busy() && pane.client().is_some_and(CliKind::is_ai))
    }

    pub fn attention(&self) -> bool {
        self.panes.iter().any(|pane| pane.attention)
    }

    pub fn index_of(&self, uid: u64) -> Option<usize> {
        self.panes.iter().position(|pane| pane.uid == uid)
    }

    pub fn active_pane(&self) -> Option<&TerminalPane> {
        self.panes.get(self.active)
    }

    pub fn active_pane_mut(&mut self) -> Option<&mut TerminalPane> {
        self.panes.get_mut(self.active)
    }
}

pub fn read_directory_entries(dir: &Path, show_hidden: bool, filter: &str) -> Result<Vec<FileEntry>, String> {
    let filter = filter.to_lowercase();
    let entries = fs::read_dir(dir).map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !show_hidden && name.starts_with('.') {
            continue;
        }
        if !filter.is_empty() && !name.to_lowercase().contains(&filter) {
            continue;
        }
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        let is_dir = kind.is_dir() || (kind.is_symlink() && entry.path().is_dir());
        let size = entry.metadata().map(|meta| meta.len()).unwrap_or(0);
        out.push(FileEntry {
            name,
            path: entry.path(),
            is_dir,
            size,
        });
    }
    out.sort_by(|left, right| {
        right
            .is_dir
            .cmp(&left.is_dir)
            .then_with(|| left.name.to_lowercase().cmp(&right.name.to_lowercase()))
    });
    Ok(out)
}

#[derive(Default)]
pub struct MouseState {
    pub pressed_buttons: [Option<(usize, usize)>; 3],
    pub last_hover_cell: Option<(usize, usize)>,
}

pub struct PaletteState {
    pub open: bool,
    pub query: String,
    pub selected: usize,
    pub just_opened: bool,
    pub cached_items: Vec<PaletteItem>,
    pub cache_dirty: bool,
}

#[derive(Clone, Copy, Default)]
pub struct ListNav {
    pub up: bool,
    pub down: bool,
    pub enter: bool,
    pub escape: bool,
}

#[derive(Clone)]
pub struct PaletteItem {
    pub group: &'static str,
    pub title: String,
    pub detail: String,
    pub color: Option<Color32>,
    pub action: Action,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum AgentState {
    NeedsYou,
    Error,
    Working,
    Waiting,
    Starting,
    Done,
}

impl AgentState {
    pub fn of(pane: &TerminalPane) -> Option<Self> {
        let started = pane.started.map(|at| at.elapsed()).unwrap_or_default();
        Some(match &pane.exit {
            Some(exit) if exit.code != 0 || exit.signal.is_some() => AgentState::Error,
            Some(_) => AgentState::Done,
            None if pane.spec.is_none() => return None,
            None if pane.attention => AgentState::NeedsYou,
            None if pane.is_busy() => AgentState::Working,
            None if started < Duration::from_secs(2) => AgentState::Starting,
            None => AgentState::Waiting,
        })
    }

    pub fn label(self) -> &'static str {
        match self {
            AgentState::NeedsYou => "needs you",
            AgentState::Error => "error",
            AgentState::Working => "working",
            AgentState::Waiting => "waiting for input",
            AgentState::Starting => "starting",
            AgentState::Done => "finished",
        }
    }

    pub fn symbol(self) -> &'static str {
        match self {
            AgentState::NeedsYou => "▲",
            AgentState::Error => "!",
            AgentState::Working => "●",
            AgentState::Waiting => "○",
            AgentState::Starting => "◌",
            AgentState::Done => "✓",
        }
    }

    pub fn color(self, palette: &Palette) -> Color32 {
        match self {
            AgentState::NeedsYou => palette.amber,
            AgentState::Error => palette.red,
            AgentState::Working => palette.accent,
            AgentState::Waiting => palette.green,
            AgentState::Starting => palette.cyan,
            AgentState::Done => palette.faint,
        }
    }
}

pub struct AgentInstance {
    pub space: usize,
    pub uid: u64,
    pub state: AgentState,
    pub project: String,
    pub detail: String,
}

pub struct AisApp {
    pub home: PathBuf,
    pub settings_file: PathBuf,
    pub settings: Settings,
    pub settings_dirty: Option<Instant>,
    pub settings_tab: SettingsTab,
    pub settings_search: String,
    pub reset_confirm_at: Option<Instant>,
    pub projects: Vec<Project>,
    pub project_search: String,
    pub session_search: String,
    pub scan_cache: ScanCache,
    pub scan_rx: Option<Receiver<(ScanResult, ScanCache)>>,
    pub scanning: bool,
    pub last_scan: Instant,
    pub cli_rx: Option<Receiver<[bool; 6]>>,
    pub last_cli_check: Instant,
    pub files_rx: Option<FilesReceiver>,
    pub files_loading: bool,
    pub spaces: Vec<Space>,
    pub active_space: Option<usize>,
    pub cli: Vec<CliStatus>,
    pub notices: Vec<Notice>,
    pub unread: usize,
    pub show_notices: bool,
    pub show_settings: bool,
    pub palette_state: PaletteState,
    pub sessions: SessionStore,
    pub usage: UsageStore,
    pub usage_requested: bool,
    pub folder_rx: Option<Receiver<Option<PathBuf>>>,
    pub waker: Arc<dyn Fn() + Send + Sync>,
    pub actions: Vec<Action>,
    pub pending_focus: Option<u64>,
    pub close_armed: Option<(u64, Instant)>,
    pub reveal_tab: bool,
    pub window_focused: bool,
    pub mouse: HashMap<u64, MouseState>,
    pub pane_searches: HashMap<u64, PaneSearch>,
    pub pending_paste: Option<PendingPaste>,
    pub last_title: String,
    pub last_launch: Option<(String, Instant)>,
    pub query_colors_for: Option<(Theme, UiTheme, TerminalPalette)>,
    pub system_light: bool,
    pub last_usage_cache: Instant,
    pub last_side_right: bool,
    pub search_open: bool,
    pub search_rect: Rect,
    pub search_drop_rect: Rect,
    pub search_nav: ListNav,
    pub capturing: Option<Cmd>,
    pub capture_error: Option<String>,
    pub git_branches: HashMap<PathBuf, (Instant, Option<String>)>,
    pub icon_font: bool,
    pub last_window_rect: Option<Rect>,
    pub terminal_fullscreen: bool,
    pub pre_fullscreen_zoomed: Option<bool>,
    pub update_status: UpdateStatus,
    pub update_rx: Option<Receiver<Result<Installed, String>>>,
    pub update_manual: bool,
}

impl AisApp {
    pub fn new(cc: &eframe::CreationContext<'_>, no_update_check: bool) -> Self {
        let home = home_dir();
        let settings_file = settings_path(&home);
        let (settings, settings_warnings) = Settings::load_checked(&settings_file);
        let ctx = cc.egui_ctx.clone();
        let waker: Arc<dyn Fn() + Send + Sync> = Arc::new(move || ctx.request_repaint());
        let ais = find_ais();
        let mut sessions = SessionStore::new(ais.clone());
        sessions.set_waker(waker.clone());
        let mut usage = UsageStore::new(ais);
        usage.set_waker(waker.clone());
        let cli = CliKind::ALL
            .iter()
            .map(|kind| {
                let custom = settings.cli_commands.get(kind.id()).map(String::as_str);
                CliStatus {
                    installed: kind.installed_with(custom),
                    snapshot: None,
                }
            })
            .collect();
        let mut app = Self {
            home,
            settings_file,
            settings,
            settings_dirty: None,
            settings_tab: SettingsTab::Appearance,
            settings_search: String::new(),
            reset_confirm_at: None,
            projects: Vec::new(),
            project_search: String::new(),
            session_search: String::new(),
            scan_cache: ScanCache::default(),
            scan_rx: None,
            scanning: false,
            last_scan: Instant::now(),
            cli_rx: None,
            last_cli_check: Instant::now(),
            files_rx: None,
            files_loading: false,
            spaces: Vec::new(),
            active_space: None,
            cli,
            notices: Vec::new(),
            unread: 0,
            show_notices: false,
            show_settings: false,
            palette_state: PaletteState {
                open: false,
                query: String::new(),
                selected: 0,
                just_opened: false,
                cached_items: Vec::new(),
                cache_dirty: true,
            },
            sessions,
            usage,
            usage_requested: false,
            folder_rx: None,
            waker,
            actions: Vec::new(),
            pending_focus: None,
            close_armed: None,
            reveal_tab: true,
            window_focused: true,
            mouse: Default::default(),
            pane_searches: Default::default(),
            pending_paste: None,
            last_title: String::new(),
            last_launch: None,
            query_colors_for: None,
            system_light: matches!(cc.egui_ctx.system_theme(), Some(egui::Theme::Light)),
            last_usage_cache: Instant::now(),
            last_side_right: false,
            search_open: false,
            search_rect: Rect::NOTHING,
            search_drop_rect: Rect::NOTHING,
            search_nav: ListNav::default(),
            capturing: None,
            capture_error: None,
            git_branches: HashMap::new(),
            icon_font: false,
            last_window_rect: None,
            terminal_fullscreen: false,
            pre_fullscreen_zoomed: None,
            update_status: UpdateStatus::Idle,
            update_rx: None,
            update_manual: false,
        };
        for warning in settings_warnings {
            log::warn(&warning);
            app.notice(NoticeKind::Error, warning);
        }
        cc.egui_ctx.options_mut(|options| {
            options.zoom_with_keyboard = false;
            // egui defaults line_scroll_speed to 40pt per LineDelta notch, which feels
            // sluggish on Windows (1 wheel detent = LineDelta 1.0).
            options.line_scroll_speed = (crate::widgets::wheel_scroll_lines() * 36.0).max(96.0);
        });
        app.configure_fonts(&cc.egui_ctx);
        app.configure_style(&cc.egui_ctx);
        for path in app.settings.open_spaces.clone() {
            app.spaces.push(app.new_space(path));
        }
        if !app.spaces.is_empty() {
            app.active_space = Some(app.settings.active_space.min(app.spaces.len() - 1));
        }
        let args: Vec<String> = env::args().skip(1).collect();
        let mut run = None;
        let mut iter = args.iter();
        while let Some(arg) = iter.next() {
            match arg.as_str() {
                "--run" => run = iter.next().cloned(),
                other if !other.starts_with('-') => {
                    let path = canonical_path(&PathBuf::from(other));
                    app.actions.push(Action::OpenProject(path));
                }
                _ => {}
            }
        }
        match run.as_deref() {
            Some("shell") | Some("terminal") => app.actions.push(Action::Shell(Target::Auto)),
            Some(id) => match CliKind::from_id(id) {
                Some(kind) => app.actions.push(Action::Launch(kind, LaunchMode::New, Target::Auto)),
                None => app.notice(NoticeKind::Error, format!("Unknown CLI for --run: {id}")),
            },
            None => {}
        }
        app.refresh_projects(false);
        if !no_update_check && update::should_auto_check(app.settings.auto_update) {
            app.start_update_check(false);
        }
        app
    }

    pub fn palette(&self) -> Palette {
        Palette::for_theme(
            self.effective_theme(),
            self.settings.ui_theme,
            self.settings.terminal_palette,
        )
    }

    pub fn effective_theme(&self) -> Theme {
        self.settings.theme.resolve(self.system_light)
    }

    pub fn right_tab(&self) -> Option<RightTab> {
        if !self.settings.show_files {
            None
        } else if self.show_notices {
            Some(RightTab::Notices)
        } else if self.settings.right_usage {
            Some(RightTab::Usage)
        } else {
            Some(RightTab::Files)
        }
    }

    pub fn set_right_tab(&mut self, tab: Option<RightTab>) {
        match tab {
            None => self.settings.show_files = false,
            Some(tab) => {
                self.settings.show_files = true;
                self.last_side_right = true;
                self.show_notices = tab == RightTab::Notices;
                if tab != RightTab::Notices {
                    self.settings.right_usage = tab == RightTab::Usage;
                }
                if self.show_notices {
                    self.unread = 0;
                }
            }
        }
        if !self.settings.show_files {
            self.show_notices = false;
        }
        self.mark_settings_dirty();
    }

    pub fn toggle_right_tab(&mut self, tab: RightTab) {
        let next = if self.right_tab() == Some(tab) { None } else { Some(tab) };
        self.set_right_tab(next);
    }

    pub fn mark_settings_dirty(&mut self) {
        self.settings_dirty = Some(Instant::now());
        self.palette_state.cache_dirty = true;
    }

    pub fn flush_settings(&mut self) {
        self.settings_dirty = None;
        self.settings.open_spaces = self.spaces.iter().map(|space| space.path.clone()).collect();
        self.settings.active_space = self.active_space.unwrap_or(0);
        for space in &self.spaces {
            let key = space.path.to_string_lossy().into_owned();
            self.settings.space_layouts.insert(
                key,
                SpaceLayout {
                    stacked: space.stacked,
                    split_x: space.split_x,
                    split_y: space.split_y,
                },
            );
        }
        if let Err(error) = self.settings.save(&self.settings_file) {
            log::error(&error);
            self.notice(NoticeKind::Error, error);
        }
    }

    pub fn configure_fonts(&mut self, ctx: &egui::Context) {
        let mut fonts = FontDefinitions::default();
        let lib = self.home.join("Library/Fonts");
        let ui_files = |weights: &[&str]| {
            let mut files = self.settings.ui_font.files(&self.home, "Propo", weights);
            files.extend(FontChoice::JetBrains.files(&self.home, "Propo", weights));
            files
        };
        let mut ui_candidates = ui_files(&["Medium", "Regular"]);
        ui_candidates.extend([
            PathBuf::from("/System/Library/Fonts/SFNS.ttf"),
            PathBuf::from("/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf"),
        ]);
        #[cfg(windows)]
        if let Some(windir) = env::var_os("WINDIR").map(PathBuf::from) {
            let fonts_dir = windir.join("Fonts");
            ui_candidates.extend([fonts_dir.join("seguisb.ttf"), fonts_dir.join("segoeui.ttf")]);
        }
        if self.settings.ui_font == FontChoice::System {
            ui_candidates.insert(0, PathBuf::from("/System/Library/Fonts/SFNS.ttf"));
            #[cfg(windows)]
            if let Some(windir) = env::var_os("WINDIR").map(PathBuf::from) {
                ui_candidates.insert(0, windir.join("Fonts/segoeui.ttf"));
            }
        }
        let ui_font = load_first(&ui_candidates);
        let mut icon_candidates = FontChoice::JetBrains.files(&self.home, "Propo", &["Regular"]);
        icon_candidates.extend(FontChoice::FiraCode.files(&self.home, "Propo", &["Regular", "Medium"]));
        icon_candidates.extend(FontChoice::Hack.files(&self.home, "Propo", &["Regular"]));
        icon_candidates.push(lib.join("SymbolsNerdFont-Regular.ttf"));
        icon_candidates.push(PathBuf::from(
            "/usr/share/fonts/truetype/nerd-fonts/SymbolsNerdFont-Regular.ttf",
        ));
        let icon_font = load_first(&icon_candidates);
        let mut mono_candidates = self
            .settings
            .term_font
            .files(&self.home, "Mono", &["Regular", "Medium"]);
        mono_candidates.extend(FontChoice::JetBrains.files(&self.home, "Mono", &["Regular"]));
        mono_candidates.extend([
            PathBuf::from("/System/Library/Fonts/SFNSMono.ttf"),
            PathBuf::from("/System/Library/Fonts/Menlo.ttc"),
            PathBuf::from("/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf"),
        ]);
        #[cfg(windows)]
        if let Some(windir) = env::var_os("WINDIR").map(PathBuf::from) {
            let fonts_dir = windir.join("Fonts");
            mono_candidates.extend([
                fonts_dir.join("CascadiaMono.ttf"),
                fonts_dir.join("CascadiaCode.ttf"),
                fonts_dir.join("consola.ttf"),
            ]);
        }
        if self.settings.term_font == FontChoice::System {
            mono_candidates.insert(0, PathBuf::from("/System/Library/Fonts/SFNSMono.ttf"));
            #[cfg(windows)]
            if let Some(windir) = env::var_os("WINDIR").map(PathBuf::from) {
                let fonts_dir = windir.join("Fonts");
                mono_candidates.insert(0, fonts_dir.join("consola.ttf"));
                mono_candidates.insert(0, fonts_dir.join("CascadiaMono.ttf"));
            }
        }
        let mono = load_first(&mono_candidates);
        let mut bold_candidates = self.settings.term_font.files(&self.home, "Mono", &["Bold", "SemiBold"]);
        bold_candidates.extend(FontChoice::JetBrains.files(&self.home, "Mono", &["Bold"]));
        bold_candidates.push(PathBuf::from(
            "/usr/share/fonts/truetype/dejavu/DejaVuSansMono-Bold.ttf",
        ));
        #[cfg(windows)]
        if let Some(windir) = env::var_os("WINDIR").map(PathBuf::from) {
            bold_candidates.push(windir.join("Fonts/consolab.ttf"));
        }
        let mono_bold = load_first(&bold_candidates);
        #[cfg_attr(not(windows), allow(unused_mut))]
        let mut cjk_candidates = vec![
            PathBuf::from("/System/Library/Fonts/Hiragino Sans GB.ttc"),
            PathBuf::from("/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc"),
            PathBuf::from("/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc"),
        ];
        #[cfg(windows)]
        if let Some(windir) = env::var_os("WINDIR").map(PathBuf::from) {
            let fonts_dir = windir.join("Fonts");
            cjk_candidates.extend([
                fonts_dir.join("YuGothR.ttc"),
                fonts_dir.join("meiryo.ttc"),
                fonts_dir.join("msyh.ttc"),
            ]);
        }
        let cjk = load_first(&cjk_candidates);
        #[cfg_attr(not(windows), allow(unused_mut))]
        let mut symbol_candidates = vec![
            PathBuf::from("/System/Library/Fonts/Apple Symbols.ttf"),
            PathBuf::from("/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf"),
        ];
        #[cfg(windows)]
        if let Some(windir) = env::var_os("WINDIR").map(PathBuf::from) {
            let fonts_dir = windir.join("Fonts");
            symbol_candidates.extend([fonts_dir.join("seguiemj.ttf"), fonts_dir.join("seguisym.ttf")]);
        }
        let symbols = load_first(&symbol_candidates);
        let mut add = |name: &str, bytes: Option<Vec<u8>>| -> bool {
            match bytes {
                Some(bytes) => {
                    fonts
                        .font_data
                        .insert(name.to_string(), Arc::new(FontData::from_owned(bytes)));
                    true
                }
                None => false,
            }
        };
        let has_ui = add("ais-ui", ui_font);
        let has_icons = add("ais-icons", icon_font);
        let has_mono = add("ais-mono", mono);
        let has_bold = add("ais-mono-bold-face", mono_bold);
        let has_symbols = add("ais-symbols", symbols);
        let has_cjk = add("ais-cjk", cjk);

        let proportional = fonts.families.entry(FontFamily::Proportional).or_default();
        if has_icons {
            proportional.insert(0, "ais-icons".into());
        }
        if has_ui {
            proportional.insert(0, "ais-ui".into());
        }
        if has_symbols {
            proportional.push("ais-symbols".into());
        }
        let monospace = fonts.families.entry(FontFamily::Monospace).or_default();
        if has_mono {
            monospace.insert(0, "ais-mono".into());
        }
        if has_icons {
            monospace.push("ais-icons".into());
        }
        if has_symbols {
            monospace.push("ais-symbols".into());
        }
        if has_cjk {
            for family in [FontFamily::Proportional, FontFamily::Monospace] {
                fonts.families.entry(family).or_default().push("ais-cjk".into());
            }
        }
        let mut bold = fonts.families[&FontFamily::Monospace].clone();
        if has_bold {
            bold.insert(0, "ais-mono-bold-face".into());
        }
        fonts.families.insert(FontFamily::Name(BOLD_FAMILY.into()), bold);
        ctx.set_fonts(fonts);
        self.icon_font = has_icons;
    }

    pub fn configure_style(&self, ctx: &egui::Context) {
        let palette = self.palette();
        let mut visuals = if palette.light {
            egui::Visuals::light()
        } else {
            egui::Visuals::dark()
        };
        visuals.panel_fill = palette.bg;
        visuals.window_fill = palette.panel;
        visuals.window_stroke = egui::Stroke::new(1.0, palette.border);
        visuals.window_corner_radius = egui::CornerRadius::same(12);
        visuals.menu_corner_radius = egui::CornerRadius::same(8);
        visuals.extreme_bg_color = palette.raised;
        visuals.faint_bg_color = palette.panel;
        visuals.override_text_color = Some(palette.text);
        visuals.hyperlink_color = palette.accent;
        for (widget, fill) in [
            (&mut visuals.widgets.noninteractive, palette.panel),
            (&mut visuals.widgets.inactive, palette.raised),
            (&mut visuals.widgets.hovered, palette.hover),
            (&mut visuals.widgets.active, palette.hover),
            (&mut visuals.widgets.open, palette.hover),
        ] {
            widget.bg_fill = fill;
            widget.weak_bg_fill = fill;
            widget.corner_radius = egui::CornerRadius::same(7);
        }
        visuals.widgets.noninteractive.bg_stroke = egui::Stroke::new(1.0, palette.border);
        visuals.widgets.inactive.bg_stroke = egui::Stroke::new(1.0, palette.border);
        visuals.widgets.hovered.bg_stroke = egui::Stroke::new(1.0, palette.border_strong);
        visuals.widgets.active.bg_stroke = egui::Stroke::new(1.0, palette.accent);
        visuals.widgets.inactive.fg_stroke = egui::Stroke::new(1.0, palette.text);
        visuals.widgets.noninteractive.fg_stroke = egui::Stroke::new(1.0, palette.text);
        visuals.selection.bg_fill = palette.accent.gamma_multiply(0.30);
        visuals.selection.stroke = egui::Stroke::new(1.0, palette.accent);
        visuals.popup_shadow = egui::Shadow {
            offset: [0, 8],
            blur: 24,
            spread: 0,
            color: Color32::from_black_alpha(if palette.light { 40 } else { 120 }),
        };
        visuals.window_shadow = visuals.popup_shadow;
        ctx.set_theme(if palette.light {
            egui::Theme::Light
        } else {
            egui::Theme::Dark
        });
        ctx.set_visuals(visuals);

        let mut style = (*ctx.style()).clone();
        style.spacing.item_spacing = Vec2::new(8.0, 6.0);
        style.spacing.button_padding = Vec2::new(10.0, 5.0);
        style.spacing.interact_size.y = 26.0;
        style.spacing.window_margin = egui::Margin::symmetric(14, 10);
        style.interaction.selectable_labels = false;
        style.interaction.multi_widget_text_select = false;
        style.text_styles = [
            (
                egui::TextStyle::Heading,
                egui::FontId::new(14.0, FontFamily::Proportional),
            ),
            (egui::TextStyle::Body, egui::FontId::new(14.0, FontFamily::Proportional)),
            (
                egui::TextStyle::Button,
                egui::FontId::new(13.5, FontFamily::Proportional),
            ),
            (
                egui::TextStyle::Small,
                egui::FontId::new(11.5, FontFamily::Proportional),
            ),
            (
                egui::TextStyle::Monospace,
                egui::FontId::new(self.settings.font_size, FontFamily::Monospace),
            ),
        ]
        .into();
        ctx.set_style(style);
    }

    pub fn new_pane(&self) -> TerminalPane {
        let pane = TerminalPane::empty();
        pane.set_waker(self.waker.clone());
        pane.set_query_colors(self.query_colors());
        pane
    }

    pub fn new_space(&self, path: PathBuf) -> Space {
        let layout = self
            .settings
            .space_layouts
            .get(&path.to_string_lossy().into_owned())
            .copied();
        let mut space = Space::new(path, layout);
        space.panes.push(self.new_pane());
        space
    }

    pub fn query_colors(&self) -> QueryColors {
        let term = self.palette().term;
        let arr = |color: Color32| [color.r(), color.g(), color.b()];
        QueryColors {
            fg: arr(term.fg),
            bg: arr(term.bg),
            cursor: arr(term.cursor),
            ansi: term.ansi.map(arr),
        }
    }

    // ----- background work ---------------------------------------------------

    pub fn refresh_projects(&mut self, announce: bool) {
        if self.scanning {
            return;
        }
        let (tx, rx) = mpsc::channel();
        let home = self.home.clone();
        let waker = self.waker.clone();
        let mut cache = std::mem::take(&mut self.scan_cache);
        self.scan_rx = Some(rx);
        self.scanning = true;
        if announce {
            self.notice(NoticeKind::Info, "Rescanning local AI history…");
        }
        if let Err(error) = thread::Builder::new().name("ais-scan".into()).spawn(move || {
            let result = scan_projects_cached(&home, &mut cache);
            let _ = tx.send((result, cache));
            waker();
        }) {
            log::error(&format!("Project scan thread spawn failed: {error}"));
            self.scan_rx = None;
            self.scanning = false;
            self.notice(NoticeKind::Error, "Could not start background project scan");
        }
    }

    fn preload_sessions(&mut self) {
        const RECENT: usize = 8;
        let mut paths: Vec<PathBuf> = self
            .spaces
            .iter()
            .filter(|space| space.available)
            .map(|space| space.path.clone())
            .collect();
        for project in self.projects.iter().take(RECENT) {
            if !paths.contains(&project.path) {
                paths.push(project.path.clone());
            }
        }
        self.sessions.preload(paths.iter().map(PathBuf::as_path));
    }

    fn poll_scan(&mut self) {
        let Some(rx) = self.scan_rx.as_ref() else {
            return;
        };
        match rx.try_recv() {
            Ok((result, cache)) => {
                self.scan_cache = cache;
                self.projects = result.projects;
                for (kind, snapshot) in result.latest {
                    self.cli[kind.index()].snapshot = Some(snapshot);
                }
                self.scan_rx = None;
                self.scanning = false;
                self.last_scan = Instant::now();
                self.palette_state.cache_dirty = true;
                self.preload_sessions();
                if self.spaces.is_empty()
                    && let Some(project) = self.projects.first()
                {
                    self.actions.push(Action::OpenProject(project.path.clone()));
                }
            }
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => {
                self.scan_rx = None;
                self.scanning = false;
                self.notice(NoticeKind::Error, "Project scan stopped unexpectedly");
            }
        }
    }

    pub fn start_cli_check(&mut self) {
        if self.cli_rx.is_some() {
            return;
        }
        self.last_cli_check = Instant::now();
        let (tx, rx) = mpsc::channel();
        let waker = self.waker.clone();
        let cmds: [Option<String>; 6] =
            std::array::from_fn(|i| self.settings.cli_commands.get(CliKind::ALL[i].id()).cloned());
        self.cli_rx = Some(rx);
        if let Err(error) = thread::Builder::new().name("ais-cli-check".into()).spawn(move || {
            let mut flags = [false; 6];
            for (i, kind) in CliKind::ALL.into_iter().enumerate() {
                flags[i] = kind.installed_with(cmds[i].as_deref());
            }
            let _ = tx.send(flags);
            waker();
        }) {
            log::warn(&format!("CLI check thread spawn failed: {error}"));
            self.cli_rx = None;
        }
    }

    pub fn start_update_check(&mut self, manual: bool) {
        if self.update_rx.is_some() {
            return;
        }
        self.update_manual = manual;
        self.update_status = UpdateStatus::Checking;
        let (tx, rx) = mpsc::channel();
        let waker = self.waker.clone();
        let ais = self.sessions.ais().map(Path::to_path_buf);
        self.update_rx = Some(rx);
        if let Err(error) = thread::Builder::new().name("ais-update".into()).spawn(move || {
            let result = update::check_and_install(ais.as_deref());
            let _ = tx.send(result);
            waker();
        }) {
            log::warn(&format!("Update check thread spawn failed: {error}"));
            self.update_rx = None;
            self.update_status = UpdateStatus::Error(error.to_string());
        }
    }

    fn poll_update(&mut self) {
        let Some(rx) = self.update_rx.as_ref() else {
            return;
        };
        match rx.try_recv() {
            Ok(result) => {
                self.update_rx = None;
                let manual = self.update_manual;
                self.update_manual = false;
                match result {
                    Ok(Installed::Updated(version)) => {
                        self.update_status = UpdateStatus::Updated(version.clone());
                        self.notice(
                            NoticeKind::Success,
                            format!("ais updated to v{version} — restart to apply"),
                        );
                    }
                    Ok(Installed::UpToDate(version)) => {
                        self.update_status = UpdateStatus::UpToDate(version.clone());
                        if manual {
                            self.notice(NoticeKind::Info, format!("Already up to date (v{version})"));
                        }
                    }
                    Ok(Installed::Available(version, reason)) => {
                        self.update_status = UpdateStatus::Available {
                            version: version.clone(),
                            reason: reason.clone(),
                        };
                        self.notice(NoticeKind::Info, format!("ais v{version} is available ({reason})"));
                    }
                    Err(error) => {
                        log::warn(&format!("Update check failed: {error}"));
                        self.update_status = UpdateStatus::Error(error.clone());
                        if manual {
                            self.notice(NoticeKind::Error, format!("Update check failed: {error}"));
                        }
                    }
                }
            }
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => {
                self.update_rx = None;
                self.update_status = UpdateStatus::Idle;
            }
        }
    }

    fn poll_cli_check(&mut self) {
        let Some(rx) = self.cli_rx.as_ref() else {
            return;
        };
        match rx.try_recv() {
            Ok(flags) => {
                self.cli_rx = None;
                for (i, installed) in flags.into_iter().enumerate() {
                    if self.cli[i].installed != installed {
                        self.cli[i].installed = installed;
                        self.palette_state.cache_dirty = true;
                    }
                }
            }
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => self.cli_rx = None,
        }
    }

    fn pump_files_refresh(&mut self) {
        if self.files_loading || self.right_tab() != Some(RightTab::Files) {
            return;
        }
        let Some(space_index) = self.active_space else {
            return;
        };
        let space = &mut self.spaces[space_index];
        if !space.available {
            return;
        }
        if !space.files_dirty && space.last_files_refresh.elapsed() < Duration::from_secs(2) {
            return;
        }
        space.files_dirty = false;
        space.last_files_refresh = Instant::now();
        let space_path = space.path.clone();
        let dir = space.files_dir.clone();
        let show_hidden = space.files_show_hidden;
        let filter = space.files_filter.clone();
        let (tx, rx) = mpsc::channel();
        let waker = self.waker.clone();
        self.files_rx = Some(rx);
        self.files_loading = true;
        if let Err(error) = thread::Builder::new().name("ais-files".into()).spawn(move || {
            let entries = read_directory_entries(&dir, show_hidden, &filter);
            let _ = tx.send((space_path, dir, entries));
            waker();
        }) {
            log::warn(&format!("File scan thread spawn failed: {error}"));
            self.files_rx = None;
            self.files_loading = false;
        }
    }

    fn poll_files(&mut self) {
        let Some(rx) = self.files_rx.as_ref() else {
            return;
        };
        match rx.try_recv() {
            Ok((space_path, dir, result)) => {
                self.files_rx = None;
                self.files_loading = false;
                if let Some(space) = self
                    .spaces
                    .iter_mut()
                    .find(|s| s.path == space_path && s.files_dir == dir)
                {
                    match result {
                        Ok(entries) => {
                            space.files = entries;
                            space.files_error = None;
                        }
                        Err(error) => {
                            space.files.clear();
                            space.files_error = Some(error);
                        }
                    }
                }
            }
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => {
                self.files_rx = None;
                self.files_loading = false;
            }
        }
    }

    fn poll_panes(&mut self, ctx: &egui::Context) {
        let mut messages: Vec<(NoticeKind, String, bool)> = Vec::new();
        let mut clipboard = None;
        let mut next_deadline: Option<Instant> = None;
        let mut any_busy = false;
        let mut any_running = false;
        let mut refresh_sessions = Vec::new();
        for (space_index, space) in self.spaces.iter_mut().enumerate() {
            let visible_space = self.active_space == Some(space_index);
            let active_uid = space.active_pane().map(|pane| pane.uid);
            for pane in &mut space.panes {
                let (exit, events, deadline) = pane.poll();
                if let Some(deadline) = deadline {
                    next_deadline = Some(next_deadline.map_or(deadline, |d| d.min(deadline)));
                }
                let watched = visible_space && active_uid == Some(pane.uid) && self.window_focused;
                let label = pane.title();
                for event in events {
                    match event {
                        PaneEvent::Clipboard(text) => clipboard = Some(text),
                        PaneEvent::Bell if !watched => {
                            messages.push((
                                NoticeKind::Attention,
                                format!("{label} needs you · {}", space.name),
                                true,
                            ));
                        }
                        _ => {}
                    }
                }
                if pane.is_running() {
                    any_running = true;
                }
                if pane.is_busy() {
                    any_busy = true;
                }
                if pane.take_finished_turn() && pane.client().is_some_and(CliKind::is_ai) && !watched {
                    pane.attention = true;
                    messages.push((
                        NoticeKind::Attention,
                        format!("{label} finished · {}", space.name),
                        true,
                    ));
                }
                if watched {
                    pane.attention = false;
                }
                if let Some(exit) = exit {
                    let ok = exit.code == 0 && exit.signal.is_none();
                    messages.push((
                        if ok { NoticeKind::Info } else { NoticeKind::Error },
                        format!("{label} exited · {} · {}", exit.describe(), space.name),
                        !watched && pane.client().is_some_and(CliKind::is_ai),
                    ));
                    if pane.client().is_some_and(CliKind::is_ai) {
                        refresh_sessions.push(space.path.clone());
                    }
                }
            }
        }
        for path in refresh_sessions {
            self.sessions.refresh(&path);
        }
        if let Some(text) = clipboard
            && self.settings.allow_osc52
        {
            ctx.copy_text(text);
        }
        for (kind, text, system) in messages {
            if system && !self.window_focused && self.settings.system_notifications {
                system_notification(&text);
            }
            self.notice(kind, text);
        }
        if let Some(deadline) = next_deadline {
            ctx.request_repaint_after(deadline.saturating_duration_since(Instant::now()));
        }
        // Only wake at high cadence while agents are actively running.
        let interval = if any_busy {
            Duration::from_millis(120)
        } else if any_running {
            Duration::from_secs(1)
        } else {
            Duration::from_secs(5)
        };
        ctx.request_repaint_after(interval);
    }

    pub fn notice(&mut self, kind: NoticeKind, text: impl Into<String>) {
        self.notices.push(Notice {
            at: Instant::now(),
            kind,
            text: text.into(),
        });
        if !self.show_notices && matches!(kind, NoticeKind::Attention | NoticeKind::Error) {
            self.unread += 1;
        }
        if self.notices.len() > 200 {
            self.notices.remove(0);
        }
    }

    // ----- actions -----------------------------------------------------------

    fn open_project(&mut self, path: PathBuf) {
        let path = canonical_path(&path);
        if let Some(index) = self.spaces.iter().position(|space| space.path == path) {
            self.select_space(index);
            return;
        }
        if !path.is_dir() {
            self.notice(NoticeKind::Error, format!("Folder not found: {}", path.display()));
            return;
        }
        let space = self.new_space(path);
        self.spaces.push(space);
        self.select_space(self.spaces.len() - 1);
        self.mark_settings_dirty();
    }

    fn select_space(&mut self, index: usize) {
        if index >= self.spaces.len() {
            return;
        }
        self.active_space = Some(index);
        self.reveal_tab = true;
        let space = &self.spaces[index];
        self.pending_focus = space.active_pane().map(|pane| pane.uid);
        let path = space.path.clone();
        if space.available {
            self.sessions.ensure(&path, Duration::from_secs(60));
        }
        self.mark_settings_dirty();
    }

    fn close_space(&mut self, index: usize) {
        if index >= self.spaces.len() {
            return;
        }
        let mut space = self.spaces.remove(index);
        for pane in &mut space.panes {
            self.pane_searches.remove(&pane.uid);
            pane.kill();
        }
        self.active_space = match self.active_space {
            _ if self.spaces.is_empty() => None,
            Some(active) if active > index => Some(active - 1),
            Some(active) if active == index => Some(index.min(self.spaces.len() - 1)),
            other => other,
        };
        self.notice(NoticeKind::Info, format!("Closed {}", space.name));
        self.mark_settings_dirty();
    }

    fn move_space(&mut self, index: usize, delta: isize) {
        let target = index as isize + delta;
        if index >= self.spaces.len() || target < 0 || target >= self.spaces.len() as isize {
            return;
        }
        let target = target as usize;
        self.spaces.swap(index, target);
        if self.active_space == Some(index) {
            self.active_space = Some(target);
        } else if self.active_space == Some(target) {
            self.active_space = Some(index);
        }
        self.reveal_tab = true;
        self.mark_settings_dirty();
    }

    fn retry_space(&mut self, index: usize) {
        let Some(space) = self.spaces.get_mut(index) else {
            return;
        };
        space.available = space.path.is_dir();
        let avail = space.available;
        let name = space.name.clone();
        let path = space.path.clone();
        if avail {
            space.files_dirty = true;
            self.sessions.refresh(&path);
            self.notice(NoticeKind::Success, format!("Reconnected {name}"));
        } else {
            self.notice(NoticeKind::Error, format!("Still unavailable: {}", path.display()));
        }
    }

    fn ensure_space(&mut self) -> Option<usize> {
        if self.active_space.is_none() {
            let path = self
                .projects
                .first()
                .map(|project| project.path.clone())
                .unwrap_or_else(|| self.home.clone());
            self.open_project(path);
        }
        self.active_space
    }

    fn target_pane(&mut self, space_index: usize, target: Target) -> Result<usize, String> {
        let new_pane = self.new_pane();
        let space = &mut self.spaces[space_index];
        let is_errored = |pane: &TerminalPane| pane.exit.as_ref().is_some_and(|e| e.code != 0 || e.signal.is_some());
        let reuse = match target {
            Target::Pane(uid) => space.index_of(uid).filter(|&i| !space.panes[i].is_running()),
            Target::Auto => {
                if space
                    .panes
                    .get(space.active)
                    .is_some_and(|pane| !pane.is_running() && !is_errored(pane))
                {
                    Some(space.active)
                } else {
                    space.panes.iter().position(|pane| pane.is_empty())
                }
            }
            Target::Split => None,
        };
        if let Some(index) = reuse {
            return Ok(index);
        }
        if space.panes.len() >= MAX_PANES {
            return Err(format!(
                "All {MAX_PANES} panes are busy — close one first ({})",
                self.key_hint(Cmd::ClosePane)
            ));
        }
        if space.panes.len() == 1 {
            space.split_x = if target == Target::Split { 0.5 } else { 0.58 };
        }
        space.panes.push(new_pane);
        space.zoomed = false;
        Ok(space.panes.len() - 1)
    }

    fn start(&mut self, spec: LaunchSpec, target: Target) {
        let Some(space_index) = self.ensure_space() else {
            return;
        };
        if !self.spaces[space_index].available {
            self.notice(
                NoticeKind::Error,
                format!("Folder unavailable: {}", self.spaces[space_index].path.display()),
            );
            return;
        }
        if let Target::Pane(uid) = target {
            let key = format!("{space_index}:{uid}:{}:{:?}", spec.label, spec.args);
            if self
                .last_launch
                .as_ref()
                .is_some_and(|(last, at)| *last == key && at.elapsed() < Duration::from_millis(400))
            {
                return;
            }
            self.last_launch = Some((key, Instant::now()));
        }
        let index = match self.target_pane(space_index, target) {
            Ok(index) => index,
            Err(message) => {
                self.notice(NoticeKind::Error, message);
                return;
            }
        };
        let label = spec.label.clone();
        let space = &mut self.spaces[space_index];
        let pane = &mut space.panes[index];
        match pane.launch(spec) {
            Ok(()) => {
                let uid = pane.uid;
                space.active = index;
                self.pending_focus = Some(uid);
                let name = space.name.clone();
                self.notice(NoticeKind::Success, format!("Started {label} · {name}"));
            }
            Err(message) => self.notice(NoticeKind::Error, message),
        }
    }

    pub fn cli_program(&self, kind: CliKind) -> String {
        self.settings
            .cli_commands
            .get(kind.id())
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| kind.command())
            .to_string()
    }

    pub fn launch_cli(&mut self, kind: CliKind, mode: LaunchMode, target: Target) {
        self.launch_cli_in(kind, mode, target, None);
    }

    fn launch_cli_in(&mut self, kind: CliKind, mode: LaunchMode, target: Target, cwd: Option<PathBuf>) {
        if !self.cli[kind.index()].installed {
            self.notice(
                NoticeKind::Error,
                format!(
                    "{} is not installed (`{}` not found in PATH)",
                    kind.label(),
                    self.cli_program(kind)
                ),
            );
            return;
        }
        let Some(space_index) = self.ensure_space() else {
            return;
        };
        let cwd = cwd.unwrap_or_else(|| self.spaces[space_index].path.clone());
        let mut args: Vec<String> = Vec::new();
        let label = match mode {
            LaunchMode::New => kind.label().to_string(),
            LaunchMode::Continue => match kind.continue_args() {
                Some(extra) => {
                    args.extend(extra.iter().map(|arg| arg.to_string()));
                    format!("{} · continue", kind.label())
                }
                None => {
                    self.notice(
                        NoticeKind::Error,
                        format!("{} has no “continue last” flag", kind.label()),
                    );
                    return;
                }
            },
        };
        if let Some(extra) = self.settings.cli_args.get(kind.id()) {
            args.extend(split_args(extra));
        }
        self.start(
            LaunchSpec {
                client: kind,
                program: self.cli_program(kind),
                args,
                cwd,
                label,
            },
            target,
        );
    }

    fn launch_shell(&mut self, target: Target) {
        self.launch_shell_in(target, None);
    }

    fn launch_shell_in(&mut self, target: Target, cwd: Option<PathBuf>) {
        let Some(space_index) = self.ensure_space() else {
            return;
        };
        let cwd = cwd.unwrap_or_else(|| self.spaces[space_index].path.clone());
        let shell = user_shell();
        let args = shell_args(&shell);
        let name = Path::new(&shell)
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "shell".into());
        self.start(
            LaunchSpec {
                client: CliKind::Shell,
                program: shell,
                args,
                cwd,
                label: format!("Terminal · {name}"),
            },
            target,
        );
    }

    fn resume(&mut self, session: &Session, target: Target) {
        let Some(space_index) = self.ensure_space() else {
            return;
        };
        let fallback = self.spaces[space_index].path.clone();
        let custom_cmd = self.settings.cli_commands.get(session.client.id()).map(String::as_str);
        let extra_args = self
            .settings
            .cli_args
            .get(session.client.id())
            .map(|s| split_args(s))
            .unwrap_or_default();
        match session.resume_spec(&fallback, self.sessions.ais(), custom_cmd, &extra_args) {
            Ok(spec) => {
                if spec.program == self.cli_program(session.client) && !self.cli[session.client.index()].installed {
                    self.notice(
                        NoticeKind::Error,
                        format!("{} is not installed", session.client.label()),
                    );
                    return;
                }
                self.start(spec, target);
            }
            Err(message) => self.notice(NoticeKind::Error, message),
        }
    }

    pub fn find_pane(&self, uid: u64) -> Option<(usize, usize)> {
        self.spaces
            .iter()
            .enumerate()
            .find_map(|(space_index, space)| space.index_of(uid).map(|pane_index| (space_index, pane_index)))
    }

    fn close_pane(&mut self, uid: u64) {
        let Some((space_index, index)) = self.find_pane(uid) else {
            return;
        };
        let replacement = self.new_pane();
        let space = &mut self.spaces[space_index];
        let mut pane = space.panes.remove(index);
        pane.kill();
        self.mouse.remove(&uid);
        self.pane_searches.remove(&uid);
        if space.panes.is_empty() {
            space.panes.push(replacement);
            space.zoomed = false;
        }
        if index < space.active {
            space.active -= 1;
        }
        space.active = space.active.min(space.panes.len() - 1);
        self.pending_focus = space.active_pane().map(|pane| pane.uid);
    }

    pub fn confirm_close(&mut self, key: u64, what: &str) -> bool {
        if self
            .close_armed
            .is_some_and(|(armed, at)| armed == key && at.elapsed() < Duration::from_secs(5))
        {
            self.close_armed = None;
            return true;
        }
        self.close_armed = Some((key, Instant::now()));
        self.notice(NoticeKind::Attention, format!("{what} — close again to confirm"));
        false
    }

    fn apply(&mut self, ctx: &egui::Context, action: Action) {
        match action {
            Action::OpenProject(path) => self.open_project(path),
            Action::PickFolder => self.pick_folder(),
            Action::LaunchIn(kind, cwd) => self.launch_cli_in(kind, LaunchMode::New, Target::Auto, Some(cwd)),
            Action::ShellIn(cwd) => self.launch_shell_in(Target::Auto, Some(cwd)),
            Action::SelectSpace(index) => self.select_space(index),
            Action::CloseSpace(index) => {
                let index = if index == usize::MAX {
                    self.active_space.unwrap_or(usize::MAX)
                } else {
                    index
                };
                let agents = self.spaces.get(index).map_or(0, |space| {
                    space
                        .panes
                        .iter()
                        .filter(|pane| pane.is_running() && pane.client().is_some_and(CliKind::is_ai))
                        .count()
                });
                if agents == 0
                    || self.confirm_close(
                        u64::MAX - 1 - index as u64,
                        &format!("{} still running here", plural(agents, "agent")),
                    )
                {
                    self.close_space(index);
                }
            }
            Action::MoveSpace(index, delta) => self.move_space(index, delta),
            Action::RetrySpace(index) => self.retry_space(index),
            Action::Launch(kind, mode, target) => self.launch_cli(kind, mode, target),
            Action::Shell(target) => self.launch_shell(target),
            Action::Resume(session, target) => self.resume(&session, target),
            Action::Split(right) => {
                if let Some(space) = self.active_space.and_then(|i| self.spaces.get_mut(i))
                    && space.panes.len() == 1
                {
                    space.stacked = !right;
                }
                self.launch_shell(Target::Split);
            }
            Action::ClosePane(uid) => {
                let uid = self.resolve_uid(uid);
                let busy = self.find_pane(uid).and_then(|(space, pane)| {
                    let pane = &self.spaces[space].panes[pane];
                    (pane.is_busy() && pane.client().is_some_and(CliKind::is_ai)).then(|| pane.title())
                });
                match busy {
                    Some(title) if !self.confirm_close(uid, &format!("{} is still working", truncate(&title, 40))) => {}
                    _ => self.close_pane(uid),
                }
            }
            Action::RestartPane(uid) => {
                let uid = self.resolve_uid(uid);
                if let Some((space_index, index)) = self.find_pane(uid) {
                    let pane = &mut self.spaces[space_index].panes[index];
                    let label = pane.title();
                    match pane.restart() {
                        Ok(()) => self.notice(NoticeKind::Info, format!("Restarted {label}")),
                        Err(message) => self.notice(NoticeKind::Error, message),
                    }
                    self.pending_focus = Some(uid);
                }
            }
            Action::FocusPane(uid) => {
                if let Some((space_index, index)) = self.find_pane(uid) {
                    self.spaces[space_index].active = index;
                    if self.active_space != Some(space_index) {
                        self.select_space(space_index);
                    }
                    self.pending_focus = Some(uid);
                }
            }
            Action::FocusNext(forward) => {
                if let Some(space) = self.active_space.and_then(|i| self.spaces.get_mut(i)) {
                    let count = space.panes.len();
                    if count > 1 {
                        space.active = if forward {
                            (space.active + 1) % count
                        } else {
                            (space.active + count - 1) % count
                        };
                        self.pending_focus = space.active_pane().map(|pane| pane.uid);
                    }
                }
            }
            Action::ToggleZoom => {
                if let Some(space) = self.active_space.and_then(|i| self.spaces.get_mut(i)) {
                    space.zoomed = !space.zoomed && space.panes.len() > 1;
                    self.pending_focus = space.active_pane().map(|pane| pane.uid);
                }
            }
            Action::ToggleFullscreenTerminal => {
                self.terminal_fullscreen = !self.terminal_fullscreen;
                if self.terminal_fullscreen {
                    if let Some(space) = self.active_space.and_then(|i| self.spaces.get_mut(i)) {
                        if space.panes.len() > 1 {
                            self.pre_fullscreen_zoomed = Some(space.zoomed);
                            space.zoomed = true;
                        } else {
                            self.pre_fullscreen_zoomed = None;
                        }
                        self.pending_focus = space.active_pane().map(|pane| pane.uid);
                    }
                } else {
                    if let Some(prev) = self.pre_fullscreen_zoomed.take()
                        && let Some(space) = self.active_space.and_then(|i| self.spaces.get_mut(i))
                    {
                        space.zoomed = prev && space.panes.len() > 1;
                    }
                    self.pending_focus = self
                        .active_space
                        .and_then(|i| self.spaces.get(i))
                        .and_then(Space::active_pane)
                        .map(|pane| pane.uid);
                }
            }
            Action::ToggleWindowFullscreen => {
                let fullscreen = ctx.input(|i| i.viewport().fullscreen.unwrap_or(false));
                ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(!fullscreen));
            }
            Action::ClearScrollback => {
                if let Some(pane) = self.active_pane_mut() {
                    pane.clear_history();
                    let mode = pane.mode();
                    if pane.client() == Some(CliKind::Shell)
                        && !mode.contains(alacritty_terminal::term::TermMode::ALT_SCREEN)
                    {
                        pane.write(b"\x0c");
                    }
                }
            }
            Action::OpenFind => {
                let uid = self.resolve_uid(u64::MAX);
                if uid != u64::MAX {
                    let entry = self.pane_searches.entry(uid).or_default();
                    entry.open = true;
                    entry.focus_input = true;
                }
            }
            Action::InsertText(text) => {
                let uid = self.active_pane_mut().map(|pane| {
                    pane.paste(&text);
                    pane.uid
                });
                match uid {
                    Some(uid) => self.pending_focus = Some(uid),
                    None => self.notice(NoticeKind::Error, "Start a CLI first, then insert paths"),
                }
            }
            Action::CopyText(text, what) => {
                ctx.copy_text(text);
                self.notice(NoticeKind::Info, format!("Copied {what}"));
            }
            Action::OpenUrl(url) => open_external(&url, false),
            Action::Reveal(path) => open_external(&path.to_string_lossy(), true),
            Action::SetUiTheme(theme) => {
                self.settings.ui_theme = theme;
                self.configure_style(ctx);
                self.mark_settings_dirty();
            }
            Action::ToggleTheme => {
                self.settings.theme = self.settings.theme.next();
                self.configure_style(ctx);
                self.mark_settings_dirty();
            }
            Action::ToggleTitleBar => {
                self.settings.show_title_bar = !self.settings.show_title_bar;
                self.mark_settings_dirty();
            }
            Action::ToggleSidebar => {
                self.settings.show_sidebar = !self.settings.show_sidebar;
                self.last_side_right = !self.settings.show_sidebar;
                self.mark_settings_dirty();
            }
            Action::ToggleFiles => self.toggle_right_tab(RightTab::Files),
            Action::ToggleUsage => self.toggle_right_tab(RightTab::Usage),
            Action::ToggleToolbar => {
                self.settings.show_toolbar = !self.settings.show_toolbar;
                self.mark_settings_dirty();
            }
            Action::ToggleAgents => {
                self.settings.show_agents = !self.settings.show_agents;
                self.mark_settings_dirty();
            }
            Action::TogglePaneHeaders => {
                self.settings.show_pane_headers = !self.settings.show_pane_headers;
                self.mark_settings_dirty();
            }
            Action::ToggleStatusBar => {
                self.settings.show_status_bar = !self.settings.show_status_bar;
                self.mark_settings_dirty();
            }
            Action::ShowAllBlocks => {
                self.terminal_fullscreen = false;
                if let Some(prev) = self.pre_fullscreen_zoomed.take()
                    && let Some(space) = self.active_space.and_then(|i| self.spaces.get_mut(i))
                {
                    space.zoomed = prev && space.panes.len() > 1;
                }
                self.settings.show_title_bar = true;
                self.settings.show_sidebar = true;
                self.settings.show_toolbar = true;
                self.settings.show_agents = true;
                self.settings.show_pane_headers = true;
                self.settings.show_status_bar = true;
                self.mark_settings_dirty();
            }
            Action::SetRightTab(tab) => self.set_right_tab(tab),
            Action::RefreshUsage => self.usage.refresh_live(),
            Action::FocusAgent(kind) => self.focus_agent(kind),
            Action::ShowSessions(show) => {
                self.settings.show_sidebar = true;
                self.last_side_right = false;
                self.settings.sidebar_sessions = show;
                self.mark_settings_dirty();
            }
            Action::OpenSettings => self.show_settings = true,
            Action::CycleSpace(forward) => {
                let count = self.spaces.len();
                if count > 1 {
                    let current = self.active_space.unwrap_or(0);
                    let next = if forward {
                        (current + 1) % count
                    } else {
                        (current + count - 1) % count
                    };
                    self.select_space(next);
                }
            }
            Action::OpenPalette => {
                self.palette_state.query.clear();
                self.palette_state.selected = 0;
                self.palette_state.cache_dirty = true;
                if self.settings.search_center {
                    self.search_open = true;
                    ctx.memory_mut(|memory| memory.request_focus(Id::new(SEARCH_ID)));
                } else {
                    self.palette_state.open = true;
                    self.palette_state.just_opened = true;
                }
                if let Some(path) = self.active_space.map(|i| self.spaces[i].path.clone()) {
                    self.sessions.ensure(&path, Duration::from_secs(60));
                }
            }
            Action::OpenNotices => self.toggle_right_tab(RightTab::Notices),
            Action::ClearNotices => {
                self.notices.clear();
                self.unread = 0;
            }
            Action::RefreshProjects => {
                self.refresh_projects(true);
                self.start_cli_check();
            }
            Action::RefreshSessions => {
                if let Some(path) = self.active_space.map(|i| self.spaces[i].path.clone()) {
                    self.sessions.refresh(&path);
                }
            }
            Action::FontSize(delta) => {
                self.settings.font_size = if delta == 0.0 {
                    DEFAULT_FONT_SIZE
                } else {
                    (self.settings.font_size + delta).clamp(10.0, 28.0)
                };
                self.configure_style(ctx);
                self.mark_settings_dirty();
            }
            Action::CheckUpdates => self.start_update_check(true),
            Action::RestartToUpdate => {
                let running_agents: usize = self
                    .spaces
                    .iter()
                    .flat_map(|s| s.panes.iter())
                    .filter(|p| p.is_running() && p.client().is_some_and(CliKind::is_ai))
                    .count();
                if running_agents > 0
                    && !self.confirm_close(
                        u64::MAX - 99,
                        &format!("{} still running", plural(running_agents, "agent")),
                    )
                {
                    return;
                }
                self.flush_settings();
                for space in &mut self.spaces {
                    for pane in &mut space.panes {
                        pane.kill_blocking();
                    }
                }
                match update::spawn_restart() {
                    Ok(()) => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
                    Err(error) => self.notice(NoticeKind::Error, error),
                }
            }
        }
    }

    pub fn resolve_uid(&self, uid: u64) -> u64 {
        if uid != u64::MAX {
            return uid;
        }
        self.active_space
            .and_then(|i| self.spaces.get(i))
            .and_then(Space::active_pane)
            .map(|pane| pane.uid)
            .unwrap_or(u64::MAX)
    }

    fn pick_folder(&mut self) {
        if self.folder_rx.is_some() {
            return;
        }
        let start = self
            .active_space
            .and_then(|i| self.spaces.get(i))
            .and_then(|space| space.path.parent().map(Path::to_path_buf))
            .unwrap_or_else(|| self.home.clone());
        let (tx, rx) = mpsc::channel();
        self.folder_rx = Some(rx);
        let waker = self.waker.clone();
        if let Err(error) = thread::Builder::new().name("ais-folder".into()).spawn(move || {
            let _ = tx.send(choose_folder(&start));
            waker();
        }) {
            log::warn(&format!("Folder picker thread spawn failed: {error}"));
            self.folder_rx = None;
        }
    }

    fn poll_folder(&mut self) {
        let Some(rx) = &self.folder_rx else {
            return;
        };
        match rx.try_recv() {
            Ok(choice) => {
                self.folder_rx = None;
                if let Some(path) = choice {
                    self.open_project(path);
                }
            }
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => self.folder_rx = None,
        }
    }

    pub fn active_pane_mut(&mut self) -> Option<&mut TerminalPane> {
        let index = self.active_space?;
        self.spaces.get_mut(index)?.active_pane_mut()
    }

    pub fn key_hint(&self, command: Cmd) -> String {
        self.settings.keymap.hint(command)
    }

    pub fn read_list_nav(&self, ctx: &egui::Context) -> ListNav {
        let vim = self.settings.nav_vim;
        ctx.input_mut(|i| {
            let mut nav = ListNav {
                up: i.consume_key(Modifiers::NONE, Key::ArrowUp),
                down: i.consume_key(Modifiers::NONE, Key::ArrowDown),
                enter: i.consume_key(Modifiers::NONE, Key::Enter),
                escape: i.consume_key(Modifiers::NONE, Key::Escape),
            };
            if vim {
                nav.up |= i.consume_key(Modifiers::CTRL, Key::K) | i.consume_key(Modifiers::CTRL, Key::P);
                nav.down |= i.consume_key(Modifiers::CTRL, Key::J) | i.consume_key(Modifiers::CTRL, Key::N);
            }
            nav
        })
    }

    fn capture_shortcut(&mut self, ctx: &egui::Context) {
        let Some(command) = self.capturing else {
            return;
        };
        let mut result: Option<Option<Binding>> = None;
        let mut cancel = false;
        let mut error = None;
        ctx.input_mut(|input| {
            input.events.retain(|event| {
                let egui::Event::Key {
                    key,
                    physical_key,
                    pressed: true,
                    modifiers,
                    ..
                } = event
                else {
                    return true;
                };
                let key = physical_key.unwrap_or(*key);
                if matches!(key, Key::Escape) && !modifiers.command {
                    cancel = true;
                } else if matches!(key, Key::Backspace | Key::Delete) && !modifiers.command {
                    result = Some(None);
                } else {
                    match Binding::from_press(key, *modifiers) {
                        Some(binding) => {
                            if let Some(reason) =
                                fixed_conflict_reason(binding, self.settings.nav_arrows, self.settings.nav_vim)
                            {
                                error = Some(format!("{} is {reason}", binding.label()));
                            } else {
                                result = Some(Some(binding));
                            }
                        }
                        None => {
                            error = Some(format!(
                                "Shortcuts need {} — plain keys belong to the terminal",
                                MOD.trim_end_matches('+')
                            ))
                        }
                    }
                }
                false
            });
        });
        if cancel {
            self.capturing = None;
            self.capture_error = None;
        } else if let Some(binding) = result {
            if let Some(other) = binding.and_then(|binding| self.settings.keymap.conflict(command, binding)) {
                self.settings.keymap.set(other, None);
                self.notice(NoticeKind::Info, format!("“{}” is now unbound", other.label()));
            }
            self.settings.keymap.set(command, binding);
            self.capturing = None;
            self.capture_error = None;
            self.mark_settings_dirty();
        } else if error.is_some() {
            self.capture_error = error;
        }
    }

    fn shortcuts(&mut self, ctx: &egui::Context) {
        let mut actions = Vec::new();
        ctx.input_mut(|input| {
            let mut keep = Vec::with_capacity(input.events.len());
            for event in input.events.drain(..) {
                let mut handled = None;
                if let egui::Event::Key {
                    key,
                    physical_key,
                    pressed: true,
                    modifiers,
                    ..
                } = &event
                    && modifiers.command
                {
                    let key = physical_key.unwrap_or(*key);
                    handled = shortcut_action(&self.settings, key, *modifiers);
                }
                match handled {
                    Some(action) => actions.push(action),
                    None => keep.push(event),
                }
            }
            input.events = keep;
        });
        self.actions.extend(actions);
    }

    pub fn focus_agent(&mut self, kind: CliKind) {
        let instances = self.agent_instances(kind);
        if instances.is_empty() {
            self.launch_cli(kind, LaunchMode::New, Target::Auto);
            return;
        }
        let current = self.resolve_uid(u64::MAX);
        let next = instances
            .iter()
            .position(|instance| instance.uid == current)
            .map(|index| (index + 1) % instances.len())
            .unwrap_or(0);
        self.actions.push(Action::FocusPane(instances[next].uid));
    }

    pub fn agent_instances(&self, kind: CliKind) -> Vec<AgentInstance> {
        let mut instances = Vec::new();
        for (space_index, space) in self.spaces.iter().enumerate() {
            for pane in &space.panes {
                if pane.client() != Some(kind) {
                    continue;
                }
                let Some(state) = AgentState::of(pane) else {
                    continue;
                };
                let detail = match state {
                    AgentState::Error | AgentState::Done => {
                        pane.exit.as_ref().map(|exit| exit.describe()).unwrap_or_default()
                    }
                    AgentState::Waiting | AgentState::NeedsYou => crate::widgets::format_age(pane.idle_for()),
                    _ => pane
                        .osc_title
                        .as_deref()
                        .map(|title| truncate(title.trim_start_matches(|c: char| !c.is_alphanumeric()), 28))
                        .unwrap_or_default(),
                };
                instances.push(AgentInstance {
                    space: space_index,
                    uid: pane.uid,
                    state,
                    project: space.name.clone(),
                    detail,
                });
            }
        }
        instances.sort_by_key(|instance| (instance.state, instance.space != self.active_space.unwrap_or(0)));
        instances
    }

    fn update_window_title(&mut self, ctx: &egui::Context) {
        let title = match self.active_space.and_then(|i| self.spaces.get(i)) {
            Some(space) => format!("{} — {APP_TITLE}", space.name),
            None => APP_TITLE.to_string(),
        };
        if title != self.last_title {
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(title.clone()));
            self.last_title = title;
        }
    }
}

impl eframe::App for AisApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        ctx.set_zoom_factor(self.settings.ui_scale);
        self.window_focused = ctx.input(|i| i.viewport().focused.unwrap_or(true));

        // Confirm before closing the window when AI agents are still running.
        if ctx.input(|i| i.viewport().close_requested()) {
            let running_agents: usize = self
                .spaces
                .iter()
                .flat_map(|s| s.panes.iter())
                .filter(|p| p.is_running() && p.client().is_some_and(CliKind::is_ai))
                .count();
            if running_agents > 0
                && !self.confirm_close(u64::MAX, &format!("{} still running", plural(running_agents, "agent")))
            {
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            }
        }

        // Persist window geometry changes cleanly via debounced settings save.
        if let Some(rect) = ctx.input(|i| i.viewport().inner_rect)
            && self.last_window_rect != Some(rect)
        {
            self.last_window_rect = Some(rect);
            let outer = ctx.input(|i| i.viewport().outer_rect).unwrap_or(rect);
            let new_size = [rect.width(), rect.height()];
            let new_pos = [outer.min.x, outer.min.y];
            if self.settings.window_size != Some(new_size) || self.settings.window_pos != Some(new_pos) {
                self.settings.window_size = Some(new_size);
                self.settings.window_pos = Some(new_pos);
                self.mark_settings_dirty();
            }
        }

        // Open any dropped directory as a project space.
        let dropped_dirs: Vec<PathBuf> = ctx.input(|i| {
            i.raw
                .dropped_files
                .iter()
                .filter_map(|f| f.path.as_ref())
                .filter(|p| p.is_dir())
                .cloned()
                .collect()
        });
        for dir in dropped_dirs {
            self.actions.push(Action::OpenProject(dir));
        }

        if let Some(system) = ctx.system_theme() {
            let light = system == egui::Theme::Light;
            if light != self.system_light {
                self.system_light = light;
                if self.settings.theme == Theme::System {
                    self.configure_style(ctx);
                }
            }
        }
        let colors_key = (
            self.effective_theme(),
            self.settings.ui_theme,
            self.settings.terminal_palette,
        );
        if self.query_colors_for != Some(colors_key) {
            let colors = self.query_colors();
            for pane in self.spaces.iter().flat_map(|space| space.panes.iter()) {
                pane.set_query_colors(colors);
            }
            self.query_colors_for = Some(colors_key);
        }

        self.poll_scan();
        self.poll_cli_check();
        self.poll_update();
        self.poll_files();
        self.sessions.poll();
        self.usage.poll();
        self.poll_folder();
        self.pump_files_refresh();

        if !self.usage_requested {
            self.usage_requested = true;
            self.usage.load_cached();
        }
        let usage_visible = self.right_tab() == Some(RightTab::Usage);
        let live_every = Duration::from_secs(if usage_visible { 300 } else { 900 });
        if self.usage.available() {
            if self.settings.auto_live_usage
                && !self.usage.loading_live
                && self.usage.live_at.is_none_or(|at| at.elapsed() > live_every)
            {
                self.usage.refresh_live();
            } else if self.last_usage_cache.elapsed() > Duration::from_secs(120) {
                self.last_usage_cache = Instant::now();
                self.usage.load_cached();
            }
        }

        self.poll_panes(ctx);
        if self.last_cli_check.elapsed() > Duration::from_secs(10) {
            self.start_cli_check();
        }
        if !self.scanning && self.last_scan.elapsed() > Duration::from_secs(180) {
            self.refresh_projects(false);
        }
        self.capture_shortcut(ctx);
        if !self.palette_state.open && self.capturing.is_none() && self.pending_paste.is_none() {
            self.shortcuts(ctx);
        }
        self.search_nav = if self.search_open {
            self.read_list_nav(ctx)
        } else {
            ListNav::default()
        };

        let swap = self.settings.swap_sides;
        ctx.data_mut(|data| data.insert_temp(Id::new("swap-sides"), swap));
        if !self.terminal_fullscreen {
            if self.settings.show_title_bar {
                self.title_bar(ctx);
            }
            if self.settings.show_status_bar {
                self.status_bar(ctx);
            }
            let narrow = ctx.screen_rect().width() < NARROW_WIDTH;
            let both = self.settings.show_sidebar && self.right_tab().is_some();
            let show_sidebar = self.settings.show_sidebar && !(narrow && both && self.last_side_right);
            let show_right = !(narrow && both && !self.last_side_right);
            if show_sidebar {
                self.sidebar(ctx);
            }
            match self.right_tab().filter(|_| show_right) {
                Some(RightTab::Files) => self.files_panel(ctx),
                Some(RightTab::Usage) => self.usage_panel(ctx),
                Some(RightTab::Notices) => self.notices_panel(ctx),
                None => {}
            }
        }
        let palette = self.palette();
        let central_margin = if self.terminal_fullscreen { 0 } else { 8 };
        egui::CentralPanel::default()
            .frame(
                egui::Frame::NONE
                    .fill(palette.bg)
                    .inner_margin(egui::Margin::same(central_margin)),
            )
            .show(ctx, |ui| self.workspace(ui));
        if self.terminal_fullscreen || !self.settings.show_title_bar {
            self.floating_chrome_bar(ctx);
        }
        self.search_dropdown(ctx);
        self.command_palette(ctx);
        self.settings_window(ctx);
        self.pending_paste_modal(ctx);
        self.toast(ctx);
        self.update_window_title(ctx);

        let actions = std::mem::take(&mut self.actions);
        for action in actions {
            self.apply(ctx, action);
        }
        if self
            .settings_dirty
            .is_some_and(|at| at.elapsed() >= Duration::from_millis(350))
        {
            self.flush_settings();
        }
        if !self.actions.is_empty() {
            ctx.request_repaint();
        }
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.flush_settings();
        for space in &mut self.spaces {
            for pane in &mut space.panes {
                pane.kill_blocking();
            }
        }
    }
}

pub fn command_action(command: Cmd) -> Action {
    match command {
        Cmd::Search => Action::OpenPalette,
        Cmd::Find => Action::OpenFind,
        Cmd::OpenFolder => Action::PickFolder,
        Cmd::NewShell => Action::Shell(Target::Split),
        Cmd::SplitRight => Action::Split(true),
        Cmd::SplitDown => Action::Split(false),
        Cmd::ClosePane => Action::ClosePane(u64::MAX),
        Cmd::CloseSpace => Action::CloseSpace(usize::MAX),
        Cmd::RestartPane => Action::RestartPane(u64::MAX),
        Cmd::ClearScrollback => Action::ClearScrollback,
        Cmd::ZoomPane => Action::ToggleZoom,
        Cmd::FullscreenTerminal => Action::ToggleFullscreenTerminal,
        Cmd::ToggleWindowFullscreen => Action::ToggleWindowFullscreen,
        Cmd::NextPane => Action::FocusNext(true),
        Cmd::PrevPane => Action::FocusNext(false),
        Cmd::NextSpace => Action::CycleSpace(true),
        Cmd::PrevSpace => Action::CycleSpace(false),
        Cmd::ToggleTitleBar => Action::ToggleTitleBar,
        Cmd::ToggleSidebar => Action::ToggleSidebar,
        Cmd::ToggleFiles => Action::ToggleFiles,
        Cmd::ToggleUsage => Action::ToggleUsage,
        Cmd::ToggleAlerts => Action::OpenNotices,
        Cmd::ToggleToolbar => Action::ToggleToolbar,
        Cmd::ToggleAgents => Action::ToggleAgents,
        Cmd::TogglePaneHeaders => Action::TogglePaneHeaders,
        Cmd::ToggleStatusBar => Action::ToggleStatusBar,
        Cmd::ShowConversations => Action::ShowSessions(true),
        Cmd::RefreshSessions => Action::RefreshSessions,
        Cmd::Rescan => Action::RefreshProjects,
        Cmd::ToggleTheme => Action::ToggleTheme,
        Cmd::Settings => Action::OpenSettings,
        Cmd::FontBigger => Action::FontSize(1.0),
        Cmd::FontSmaller => Action::FontSize(-1.0),
        Cmd::FontReset => Action::FontSize(0.0),
    }
}

/// ⌘-chords: fixed ⌘1–9 / ⌘⇧1–6, optional arrow / vim navigation, then the
/// user's keymap.
pub fn shortcut_action(settings: &Settings, key: Key, mods: Modifiers) -> Option<Action> {
    if !mods.command {
        return None;
    }
    if !mods.alt
        && let Ok(digit) = key.name().parse::<usize>()
        && (1..=9).contains(&digit)
    {
        return Some(if mods.shift {
            Action::Launch(*CliKind::ALL.get(digit - 1)?, LaunchMode::New, Target::Auto)
        } else {
            Action::SelectSpace(digit - 1)
        });
    }
    if mods.alt && !mods.shift {
        let nav = match key {
            Key::ArrowDown if settings.nav_arrows => Some(Action::FocusNext(true)),
            Key::ArrowUp if settings.nav_arrows => Some(Action::FocusNext(false)),
            Key::ArrowRight if settings.nav_arrows => Some(Action::CycleSpace(true)),
            Key::ArrowLeft if settings.nav_arrows => Some(Action::CycleSpace(false)),
            Key::J if settings.nav_vim => Some(Action::FocusNext(true)),
            Key::K if settings.nav_vim => Some(Action::FocusNext(false)),
            Key::L if settings.nav_vim => Some(Action::CycleSpace(true)),
            Key::H if settings.nav_vim => Some(Action::CycleSpace(false)),
            _ => None,
        };
        if nav.is_some() {
            return nav;
        }
    }
    settings.keymap.lookup(key, mods).map(command_action)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shortcuts_map_keys() {
        let cmd = Modifiers::COMMAND;
        let mut settings = Settings::default();
        let s = &settings;
        assert!(matches!(
            shortcut_action(s, Key::T, cmd),
            Some(Action::Shell(Target::Split))
        ));
        assert!(matches!(shortcut_action(s, Key::D, cmd), Some(Action::Split(true))));
        assert!(matches!(
            shortcut_action(s, Key::D, cmd | Modifiers::SHIFT),
            Some(Action::Split(false))
        ));
        assert!(matches!(shortcut_action(s, Key::F, cmd), Some(Action::OpenFind)));
        assert!(matches!(
            shortcut_action(s, Key::Enter, cmd | Modifiers::SHIFT),
            Some(Action::ToggleFullscreenTerminal)
        ));
        assert!(matches!(
            shortcut_action(s, Key::G, cmd | Modifiers::SHIFT),
            Some(Action::ToggleAgents)
        ));
        assert!(matches!(
            shortcut_action(s, Key::Num2, cmd),
            Some(Action::SelectSpace(1))
        ));
        assert!(matches!(
            shortcut_action(s, Key::Num2, cmd | Modifiers::SHIFT),
            Some(Action::Launch(CliKind::Claude, LaunchMode::New, Target::Auto))
        ));
        assert!(matches!(
            shortcut_action(s, Key::ArrowRight, cmd | Modifiers::ALT),
            Some(Action::CycleSpace(true))
        ));
        assert!(shortcut_action(s, Key::J, cmd | Modifiers::ALT).is_none());
        assert!(shortcut_action(s, Key::D, Modifiers::NONE).is_none());
        settings.nav_vim = true;
        assert!(matches!(
            shortcut_action(&settings, Key::J, cmd | Modifiers::ALT),
            Some(Action::FocusNext(true))
        ));
    }
}
