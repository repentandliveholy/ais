//! Command-line entry point: arguments, text/JSON output and launching.

use crate::adapters::Collector;
use crate::i18n::{LANGUAGES, home, preferred_language, save_language, set_language, tr};
use crate::model::{
    CLIENTS, Filter, Grouping, Session, age, canonical, clear_path_caches, filter_sessions, group_projects, label, now, pretty_path,
    status_text, validate_launch, which,
};
use crate::screen::{client_style, status_style};
use crate::storage::{Cache, default_cache};
use crate::text::{line, ljust, rjust, shell_quote};
use crate::trf;
use crate::ui::{Outcome, View, browse, render};
use crate::usage::{
    Cancel, Usage, deepseek_key_path, demo_usage, fetch_live_usage, load_usage, render_usage, save_deepseek_key, snapshot_text,
    usage_content,
};
use crate::warp::{open_tab, warp_scheme};
use clap::{Arg, ArgAction, ArgMatches, Command, error::ErrorKind};
use indexmap::IndexMap;
use serde_json::{Value, json};
use std::io::IsTerminal;
use std::path::{Path, PathBuf};

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

fn command() -> Command {
    let flag = |name: &'static str, help: &str| Arg::new(name).long(name).action(ArgAction::SetTrue).help(tr(help));
    Command::new("ais")
        .about(tr("AI Sessions · all conversations, one entry point. Browse local AI CLI history and resume sessions."))
        .version(VERSION)
        .disable_version_flag(true)
        .arg(Arg::new("version").long("version").action(ArgAction::Version).help("Print version"))
        .arg(Arg::new("lang").long("lang").value_parser(LANGUAGES).help(tr("Interface language (saved for future launches)")))
        .arg(Arg::new("cwd").short('C').long("cwd").value_name("CWD").help(tr("Project directory (default: cwd)")))
        .arg(Arg::new("all").short('a').long("all").action(ArgAction::SetTrue).help(tr("History from all projects")))
        .arg(Arg::new("client").long("client").value_parser(CLIENTS).help(tr("Filter by client")))
        .arg(Arg::new("query").short('q').long("query").default_value("").help(tr("Search history")))
        .arg(flag("children", "Show child sessions"))
        .arg(flag("archived", "Include Codex archive"))
        .arg(flag("list", "Print a list without the TUI"))
        .arg(flag("json", "JSON instead of the TUI"))
        .arg(flag("doctor", "Diagnose history sources and installed CLIs"))
        .arg(flag("sessions", "Open the Sessions tab instead of Projects"))
        .arg(flag("usage", "Open provider usage limits: 5h / week"))
        .arg(flag("set-deepseek-key", "Save a DeepSeek API key (hidden input) to show its balance in Usage"))
        .arg(flag("cached-usage", "Show only cached usage without launching official CLIs"))
        .arg(flag("no-auto-refresh", "Disable periodic history and usage refresh"))
        .arg(Arg::new("resume").long("resume").value_name("ID").help(tr("Resume by session ID or unique prefix")))
        .arg(flag("last", "Resume the most recent session in the directory (respects --client, --query)"))
        .arg(flag("stats", "Summary of all history: clients, projects, models, activity"))
        .arg(
            Arg::new("limit")
                .short('n')
                .long("limit")
                .value_name("N")
                .value_parser(clap::value_parser!(i64))
                .default_value("0")
                .help(tr("Print at most N sessions with --list / --json")),
        )
        .arg(Arg::new("new").long("new").value_parser(CLIENTS).help(tr("New session in the selected CLI")))
        .arg(flag("new-tab", "Open sessions in a new Warp tab").conflicts_with("same-tab"))
        .arg(flag("same-tab", "Launch sessions in the current tab"))
        .arg(flag("no-return", "Exit ais when the CLI exits, without returning to the list"))
        .arg(flag("print-command", "Print the resume command instead of launching"))
        .arg(Arg::new("home").long("home").value_name("HOME").help(tr("Home directory for sources (diagnostics)")))
        .arg(
            Arg::new("source")
                .long("source")
                .value_name("CLIENT=PATH")
                .action(ArgAction::Append)
                .help(tr("Explicit client data directory; may be repeated")),
        )
        .arg(flag("no-cache", "Do not read or write the ais index"))
        .arg(Arg::new("cache-dir").long("cache-dir").value_name("CACHE_DIR").help(tr("ais index directory")))
        .arg(Arg::new("snapshot").long("snapshot").value_name("SNAPSHOT").help(tr("Save the screen as SVG / HTML / TXT")))
        .arg(Arg::new("cols").long("cols").value_parser(clap::value_parser!(i64)).default_value("144").help(tr("Snapshot width")))
        .arg(Arg::new("rows").long("rows").value_parser(clap::value_parser!(i64)).default_value("44").help(tr("Snapshot height")))
        .arg(flag("demo", "Demo data without reading history"))
        .arg(flag("update", "Download and install the latest ais release now"))
        .arg(flag("no-update-check", "Do not check for ais updates in the background"))
        .arg(Arg::new("warp-serve").long("warp-serve").value_name("STATE").hide(true))
}

fn fail(message: &str) -> ! {
    command().error(ErrorKind::ArgumentConflict, message).exit()
}

fn use_color() -> bool {
    if std::env::var_os("NO_COLOR").is_some_and(|v| !v.is_empty()) {
        return false;
    }
    std::env::var_os("FORCE_COLOR").is_some_and(|v| !v.is_empty())
        || (std::io::stdout().is_terminal() && std::env::var("TERM").as_deref() != Ok("dumb"))
}

fn paint(text: &str, style: &str, bold: bool, color: bool) -> String {
    if !color {
        return text.to_string();
    }
    format!("\x1b[{}38;5;{}m{text}\x1b[0m", if bold { "1;" } else { "" }, crate::screen::ansi_color(style))
}

fn print_stats(sessions: &[Session], home_dir: &Path, color: bool) {
    let current = now();
    let c = |text: &str, style: &str, bold: bool| paint(text, style, bold, color);
    println!("{}", c(&trf!("ais · statistics · {count} sessions", count = sessions.len()), "accent", true));
    let recent = |items: &[&Session], days: f64| items.iter().filter(|s| s.updated >= current - days * 86400.0).count();
    let all: Vec<&Session> = sessions.iter().collect();
    println!(
        "{}",
        c(
            &trf!(
                "Activity: today {d1} · 7 days {d7} · 30 days {d30}",
                d1 = recent(&all, 1.0),
                d7 = recent(&all, 7.0),
                d30 = recent(&all, 30.0)
            ),
            "muted",
            false
        )
    );
    println!();
    println!(
        "{}",
        c(
            &format!("{}{}{}   {}", ljust(&tr("CLIENT"), 10), rjust(&tr("SESSIONS"), 10), rjust(&tr("7 DAYS"), 10), tr("LAST ACTIVE")),
            "accent",
            true
        )
    );
    for client in CLIENTS {
        let items: Vec<&Session> = sessions.iter().filter(|s| s.client == client).collect();
        if items.is_empty() {
            continue;
        }
        let last = items.iter().map(|s| s.updated).fold(f64::MIN, f64::max);
        let installed = if which(client).is_some() { String::new() } else { c(&format!("  {}", tr("CLI not installed")), "faint", false) };
        println!(
            "{}{}{}   {}{installed}",
            c(&ljust(label(client), 10), client_style(client), true),
            rjust(&items.len().to_string(), 10),
            rjust(&recent(&items, 7.0).to_string(), 10),
            c(&age(last, None), "muted", false)
        );
    }
    let mut projects = group_projects(sessions, &Grouping::all(), false);
    projects.sort_by(|a, b| {
        b.sessions.len().cmp(&a.sessions.len()).then(b.updated().partial_cmp(&a.updated()).unwrap_or(std::cmp::Ordering::Equal))
    });
    if !projects.is_empty() {
        println!();
        println!("{}", c(&tr("TOP PROJECTS"), "accent", true));
        for project in projects.iter().take(10) {
            println!(
                "{}  {}{}",
                rjust(&project.sessions.len().to_string(), 6),
                c(&pretty_path(&project.cwd, Some(home_dir)), "cyan", false),
                c(&format!(" · {}", age(project.updated(), None)), "muted", false)
            );
        }
    }
    let mut models: IndexMap<&str, usize> = IndexMap::new();
    for s in sessions.iter().filter(|s| !s.model.is_empty()) {
        *models.entry(&s.model).or_default() += 1;
    }
    if !models.is_empty() {
        println!();
        println!("{}", c(&tr("TOP MODELS"), "accent", true));
        let mut ordered: Vec<(&str, usize)> = models.into_iter().collect();
        ordered.sort_by(|a, b| b.1.cmp(&a.1));
        for (model, count) in ordered.into_iter().take(8) {
            println!("{}  {model}", rjust(&count.to_string(), 6));
        }
    }
    let mut statuses: IndexMap<&str, usize> = IndexMap::new();
    for s in sessions {
        *statuses.entry(&s.status).or_default() += 1;
    }
    if !statuses.is_empty() {
        println!();
        println!("{}", c(&tr("SAVED STATUSES"), "accent", true));
        let mut ordered: Vec<(&str, usize)> = statuses.into_iter().collect();
        ordered.sort_by(|a, b| b.1.cmp(&a.1));
        for (status, count) in ordered {
            println!("{}  {}", rjust(&count.to_string(), 6), c(&status_text(status), status_style(status), false));
        }
    }
}

fn stats_json(sessions: &[Session]) -> Value {
    let current = now();
    let by = |get: &dyn Fn(&Session) -> &str| -> Value {
        let mut counts: IndexMap<String, usize> = IndexMap::new();
        for s in sessions {
            let key = get(s);
            if !key.is_empty() {
                *counts.entry(key.to_string()).or_default() += 1;
            }
        }
        json!(counts)
    };
    let mut projects = group_projects(sessions, &Grouping::all(), false);
    projects.sort_by(|a, b| b.sessions.len().cmp(&a.sessions.len()));
    let projects: IndexMap<String, usize> = projects.into_iter().map(|p| (p.cwd.clone(), p.sessions.len())).collect();
    let activity: IndexMap<String, usize> = [1.0, 7.0, 30.0]
        .iter()
        .map(|d| (format!("{}d", *d as i64), sessions.iter().filter(|s| s.updated >= current - d * 86400.0).count()))
        .collect();
    json!({
        "sessions": sessions.len(),
        "activity": activity,
        "clients": by(&|s| &s.client),
        "models": by(&|s| &s.model),
        "statuses": by(&|s| &s.status),
        "projects": projects,
    })
}

pub fn demo_sessions(cwd: &str) -> Vec<Session> {
    let current = now();
    let values = [
        ("omp", "One launcher for AI clients", "gpt-6.1-sol", "ready", "Created a colorful session list and client adapters", 120.0),
        ("agy", "Start screen design", "Gemini", "closed", "Saved the conversation picker layout", 2400.0),
        ("claude", "Authentication refactoring", "claude-opus", "awaiting", "Clarifying the sign-in method before proceeding", 7200.0),
        ("codex", "Fixing tests and build", "gpt-6.1-sol", "ready", "Verified tests and launch from another directory", 86400.0),
        ("omp", "Checking error handling", "claude-opus", "interrupted", "The user interrupted the check", 172800.0),
        ("gemini", "Project structure review", "gemini-pro", "saved", "Saved a module overview", 259200.0),
        ("pi", "Installation documentation", "gpt-6.1-sol", "ready", "Updated command examples", 345600.0),
    ];
    values
        .iter()
        .enumerate()
        .map(|(i, (client, title, model, status, action, ago))| Session {
            cwd: cwd.into(),
            title: tr(title),
            updated: current - ago,
            created: current - ago - 3700.0,
            model: (*model).into(),
            provider: if model.contains("gpt") { "openai".into() } else { String::new() },
            status: (*status).into(),
            status_detail: tr("Last saved record: ") + &tr(action),
            last_action: tr(action),
            last_user: tr("Build a unified launcher with history from all CLIs in the current directory. Add colors, statuses, models and project switching."),
            last_assistant: tr("Done: the list combines client history. The selected session opens in its original CLI and directory. Directory, model and last status are visible at a glance."),
            messages: 42 + i as i64 * 7,
            tools: 18 + i as i64 * 3,
            tokens: Some(34850),
            branch: "main".into(),
            exact: true,
            source: tr("Demo data"),
            path: format!("/demo/{client}/session.jsonl"),
            ..Session::new(client, &format!("019e7000-aaaa-7000-bbbb-{i:012}"))
        })
        .collect()
}

// ---------------------------------------------------------------- terminal state

#[cfg(unix)]
pub type SavedTerminal = Option<libc::termios>;
#[cfg(not(unix))]
pub type SavedTerminal = Option<()>;

pub fn save_terminal() -> SavedTerminal {
    #[cfg(unix)]
    {
        if !std::io::stdin().is_terminal() {
            return None;
        }
        // SAFETY: tcgetattr writes into a zeroed termios for a valid fd.
        unsafe {
            let mut attributes: libc::termios = std::mem::zeroed();
            if libc::tcgetattr(0, &mut attributes) == 0 { Some(attributes) } else { None }
        }
    }
    #[cfg(not(unix))]
    None
}

pub fn restore_terminal(saved: SavedTerminal) {
    #[cfg(unix)]
    if let Some(attributes) = saved {
        // SAFETY: restoring attributes previously read from the same fd.
        unsafe {
            libc::tcsetattr(0, libc::TCSANOW, &attributes);
        }
    }
    #[cfg(not(unix))]
    let _ = saved;
}

#[cfg(unix)]
extern "C" fn ignore_signal(_: libc::c_int) {}

/// What to launch: a saved session, or a new session of `client` in `cwd`.
pub enum Pending {
    Session(Box<Session>),
    New(String, String),
}

fn environment(set: &[(String, String)], remove: &[String]) -> Vec<(String, String)> {
    let mut env: Vec<(String, String)> = std::env::vars().filter(|(k, _)| !remove.contains(k)).collect();
    for (key, value) in set {
        env.retain(|(k, _)| k != key);
        env.push((key.clone(), value.clone()));
    }
    env
}

fn launch(pending: &Pending, return_to_ais: bool, new_tab: bool) -> Result<i32, String> {
    let (argv, cwd, set, remove) = match pending {
        Pending::Session(session) => {
            let argv = validate_launch(session)?;
            let (set, remove) = session.environment();
            (argv, session.cwd.clone(), set, remove)
        }
        Pending::New(client, cwd) => {
            let binary = which(client).ok_or_else(|| trf!("{v0} not found in PATH", v0 = client))?;
            if !Path::new(cwd).is_dir() {
                return Err(trf!("Directory does not exist: {v0}", v0 = cwd));
            }
            (vec![binary.to_string_lossy().into_owned()], cwd.clone(), Vec::new(), Vec::new())
        }
    };
    if new_tab {
        let result = open_tab(&argv, &cwd, &environment(&set, &remove), &remove)?;
        return Ok(if result == "focused" { 1 } else { 0 });
    }
    let name = Path::new(&argv[0]).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    println!("{}", trf!("\n  Continuing in {v0} · {v1}\n", v0 = name, v1 = pretty_path(&cwd, None)));
    let mut command = std::process::Command::new(&argv[0]);
    command.args(&argv[1..]).current_dir(&cwd);
    for key in &remove {
        command.env_remove(key);
    }
    for (key, value) in &set {
        command.env(key, value);
    }
    if !return_to_ais {
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            let error = command.exec();
            return Err(error.to_string());
        }
        #[cfg(not(unix))]
        {
            let status = command.status().map_err(|e| e.to_string())?;
            std::process::exit(status.code().unwrap_or(1));
        }
    }
    let saved = save_terminal();
    // Parent and child share the foreground process group. A caught no-op
    // keeps ais waiting when Ctrl+C interrupts a turn in the CLI. Unlike
    // SIG_IGN, a caught handler resets to SIG_DFL when the child execs.
    #[cfg(unix)]
    let previous = unsafe {
        // SAFETY: installing a trivial async-signal-safe handler.
        (libc::signal(libc::SIGINT, ignore_signal as libc::sighandler_t), libc::signal(libc::SIGQUIT, ignore_signal as libc::sighandler_t))
    };
    let status = command.status();
    restore_terminal(saved);
    if std::io::stdout().is_terminal() {
        // Also recover from a client that crashed in raw/alternate-screen
        // mode with mouse reporting or bracketed paste still enabled.
        print!("\x1b[0m\x1b[?25h\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1006l\x1b[?1004l\x1b[?2004l\x1b[?1049l");
        use std::io::Write;
        let _ = std::io::stdout().flush();
    }
    #[cfg(unix)]
    unsafe {
        // SAFETY: restoring the handlers saved above.
        libc::signal(libc::SIGINT, previous.0);
        libc::signal(libc::SIGQUIT, previous.1);
    }
    let status = status.map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(signal) = status.signal() {
            return Ok(-signal);
        }
    }
    Ok(status.code().unwrap_or(1))
}

fn signal_name(number: i32) -> String {
    #[cfg(unix)]
    {
        let name = match number {
            libc::SIGHUP => "SIGHUP",
            libc::SIGINT => "SIGINT",
            libc::SIGQUIT => "SIGQUIT",
            libc::SIGABRT => "SIGABRT",
            libc::SIGKILL => "SIGKILL",
            libc::SIGSEGV => "SIGSEGV",
            libc::SIGPIPE => "SIGPIPE",
            libc::SIGTERM => "SIGTERM",
            libc::SIGBUS => "SIGBUS",
            _ => "",
        };
        if !name.is_empty() {
            return name.into();
        }
    }
    number.to_string()
}

fn browser_loop(
    collector: &mut Collector,
    view: &mut View,
    mut pending: Option<Pending>,
    no_return: bool,
    new_tab: bool,
) -> Result<i32, String> {
    loop {
        let next = match pending.take() {
            Some(p) => p,
            None => match browse(collector, view).map_err(|e| e.to_string())? {
                Outcome::Quit => return Ok(0),
                Outcome::Restart => crate::restart(),
                Outcome::Resume(session) => Pending::Session(session),
                Outcome::New(client, cwd) => Pending::New(client, cwd),
            },
        };
        let (client, folder) = match &next {
            Pending::Session(s) => {
                view.selected_key = s.key();
                (s.client.clone(), s.cwd.clone())
            }
            Pending::New(client, cwd) => (client.clone(), cwd.clone()),
        };
        view.modal = "";
        match launch(&next, !no_return, new_tab) {
            Ok(code) if new_tab => {
                let folder = crate::model::file_name(&folder);
                view.message = if code == 1 {
                    trf!("Project tab is busy; focused existing session: {folder}", client = label(&client), folder = folder)
                } else {
                    trf!("Opened {client} in Warp tab: {folder}", client = label(&client), folder = folder)
                };
                if no_return {
                    return Ok(code);
                }
            }
            Ok(code) if code < 0 => {
                view.message = trf!("{v0} terminated by signal {v1}", v0 = label(&client), v1 = signal_name(-code));
            }
            Ok(code) => view.message = trf!("Returned from {v0} · exit code {v1}", v0 = label(&client), v1 = code as i64),
            Err(error) => view.message = trf!("Launching {v0}: {v1}", v0 = label(&client), v1 = line(&error, 0)),
        }
        view.loaded = false;
        view.busy_detail = false;
        clear_path_caches();
        collector.reset();
    }
}

fn usage_for_output(collector: &Collector, cached: bool) -> Usage {
    let mut usage = load_usage(collector);
    if !cached {
        match fetch_live_usage(collector, &Cancel::default(), 30.0) {
            Ok(live) => return live,
            Err(error) => usage.errors.push(line(&error.safe(), 0)),
        }
    }
    usage
}

fn print_json(value: &Value) {
    println!("{}", serde_json::to_string_pretty(value).unwrap_or_default());
}

pub fn main(args: Vec<String>) -> Result<i32, String> {
    // Select the language before building the parser so --help is localized too.
    let explicit = args.iter().enumerate().find_map(|(i, a)| {
        a.strip_prefix("--lang=").map(str::to_string).or_else(|| (a == "--lang").then(|| args.get(i + 1).cloned()).flatten())
    });
    let chosen: String = explicit.filter(|l| LANGUAGES.contains(&l.as_str())).unwrap_or_else(|| preferred_language().to_string());
    let _ = set_language(&chosen);
    let matches: ArgMatches = command().get_matches_from(&args);
    let flag = |name: &str| matches.get_flag(name);
    let text = |name: &str| matches.get_one::<String>(name).cloned();
    if let Some(state) = text("warp-serve") {
        crate::warp::serve_tab(Path::new(&state))?;
        return Ok(0);
    }
    if let Some(lang) = text("lang")
        && let Err(error) = save_language(&lang)
    {
        eprintln!("{}", trf!("Could not save language: {error}", error = line(&error.to_string(), 0)));
    }
    if flag("update") {
        return crate::update::run_manual();
    }
    if flag("set-deepseek-key") {
        let key = rpassword::prompt_password(tr("DeepSeek API key (input hidden, empty to remove): ")).map_err(|e| e.to_string())?;
        let key = key.trim();
        if key.is_empty() {
            let _ = std::fs::remove_file(deepseek_key_path());
            println!("{}", tr("DeepSeek key removed"));
            return Ok(0);
        }
        let path = save_deepseek_key(key).map_err(|e| e.to_string())?;
        println!("{}", trf!("Saved to {path} (mode 600). Check it with: ais --usage", path = pretty_path(&path.to_string_lossy(), None)));
        return Ok(0);
    }
    let cwd_arg = text("cwd").unwrap_or_else(|| std::env::current_dir().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default());
    let cwd = canonical(&cwd_arg);
    let home_dir = text("home").map(|h| crate::model::realpath(&expand(&h))).unwrap_or_else(home);
    if !Path::new(&cwd).is_dir() {
        fail(&trf!("Directory does not exist: {v0}", v0 = &cwd));
    }
    let (new, resume, last, print_command, demo) = (text("new"), text("resume"), flag("last"), flag("print-command"), flag("demo"));
    if new.is_some() && resume.is_some() {
        fail(&tr("--new and --resume cannot be used together"));
    }
    if last && (new.is_some() || resume.is_some()) {
        fail(&tr("--last cannot be combined with --new or --resume"));
    }
    if print_command && !(new.is_some() || resume.is_some() || last) {
        fail(&tr("Use --resume ID or --new CLIENT with --print-command"));
    }
    if demo && (new.is_some() || resume.is_some() || last) {
        fail(&tr("Demo sessions cannot be launched"));
    }
    let interactive = std::io::stdin().is_terminal() && std::io::stdout().is_terminal();
    let (list, json_out, doctor, stats, snapshot) = (flag("list"), flag("json"), flag("doctor"), flag("stats"), text("snapshot"));
    let return_to_browser =
        interactive && !flag("no-return") && !(list || json_out || doctor || stats || snapshot.is_some() || demo || print_command);
    let new_tab = if flag("new-tab") {
        true
    } else if flag("same-tab") {
        false
    } else {
        warp_scheme().is_some() && interactive && !flag("no-return")
    };
    let mut pending: Option<Pending> = None;
    if let Some(client) = &new {
        if print_command {
            println!("cd {} && {client}", shell_quote(&cwd));
            return Ok(0);
        } else if return_to_browser {
            pending = Some(Pending::New(client.clone(), cwd.clone()));
        } else {
            launch(&Pending::New(client.clone(), cwd.clone()), false, new_tab)?;
            return Ok(0);
        }
    }
    let mut overrides: IndexMap<String, Vec<PathBuf>> = IndexMap::new();
    for source in matches.get_many::<String>("source").into_iter().flatten() {
        let Some((client, path)) = source.split_once('=') else { fail(&tr("Source must have the form CLIENT=PATH")) };
        if !CLIENTS.contains(&client) || path.is_empty() {
            fail(&tr("Source must have the form CLIENT=PATH"));
        }
        overrides.entry(client.to_string()).or_default().push(crate::model::realpath(&expand(path)));
    }
    let cache_root = text("cache-dir").map(|p| expand(&p)).unwrap_or_else(|| default_cache(&home_dir));
    let cache = Cache::new(Some(cache_root), !flag("no-cache") && !demo);
    let mut collector = Collector::new(home_dir.clone(), cache, overrides);
    let mut view = View::new(&cwd, &home_dir);
    view.scope = if flag("all") { "all" } else { "cwd" };
    view.client = text("client").unwrap_or_else(|| "all".into());
    view.query = text("query").unwrap_or_default();
    view.children = flag("children");
    view.archived = flag("archived");
    view.message = if demo { tr("Demo data") } else { String::new() };
    view.tab = if flag("usage") {
        "usage"
    } else if flag("sessions") {
        "sessions"
    } else {
        "projects"
    };
    view.live_usage = !flag("cached-usage");
    view.auto_refresh = !flag("no-auto-refresh");
    let output_mode = list || json_out || doctor || stats || snapshot.is_some() || resume.is_some() || last || !interactive;
    if output_mode || demo {
        let usage_only = flag("usage") && !(resume.is_some() || last || doctor || stats);
        let sessions = if demo {
            demo_sessions(&cwd)
        } else if usage_only {
            Vec::new()
        } else {
            collector.scan(None)
        };
        view.loaded = true;
        let filter = Filter {
            cwd: &cwd,
            scope: view.scope,
            client: &view.client,
            query: &view.query,
            children: view.children,
            archived: view.archived,
            pins: crate::model::empty_pins(),
        };
        let mut selected = filter_sessions(&sessions, &filter);
        let mut latest = None;
        if last {
            if selected.is_empty() {
                return Err(trf!("No sessions in {path}; use --all to search every project", path = pretty_path(&cwd, Some(&home_dir))));
            }
            latest = Some(selected[0].clone());
            if !print_command && !return_to_browser {
                let shown = if selected[0].title.is_empty() { &selected[0].id } else { &selected[0].title };
                eprintln!("{}", trf!("Resuming the latest session: {title}", title = line(shown, 80)));
            }
        }
        let limit = *matches.get_one::<i64>("limit").unwrap_or(&0);
        if limit > 0 {
            selected.truncate(limit as usize);
        }
        if resume.is_some() || latest.is_some() {
            // An explicit ID may refer to another directory, but client, archive,
            // and subagent filters still apply. Never guess between matches.
            let candidates = filter_sessions(&sessions, &Filter { scope: "all", query: "", ..filter.clone() });
            let wanted = resume.clone().unwrap_or_default();
            let exact: Vec<Session> = match &latest {
                Some(s) => vec![s.clone()],
                None => candidates.iter().filter(|s| s.id == wanted).cloned().collect(),
            };
            let matches_found =
                if exact.is_empty() { candidates.iter().filter(|s| s.id.starts_with(&wanted)).cloned().collect() } else { exact };
            if matches_found.len() != 1 {
                return Err(if matches_found.is_empty() {
                    tr("Session not found")
                } else {
                    tr("Ambiguous ID; specify --client or --source CLIENT=PATH")
                });
            }
            let session = matches_found.into_iter().next().unwrap();
            if print_command {
                println!("cd {} && {}", shell_quote(&session.cwd), session.command());
            } else if return_to_browser {
                pending = Some(Pending::Session(Box::new(session)));
            } else {
                launch(&Pending::Session(Box::new(session)), false, new_tab)?;
            }
        } else if let Some(snapshot) = &snapshot {
            let strip_usage = if demo { demo_usage() } else { load_usage(&collector) };
            let cols = (*matches.get_one::<i64>("cols").unwrap()).clamp(20, 400) as usize;
            let rows = (*matches.get_one::<i64>("rows").unwrap()).clamp(10, 200) as usize;
            let errors = collector.errors.clone();
            let mut screen = render(&sessions, &mut view, cols, rows, &errors, Some(&strip_usage));
            if flag("usage") {
                let usage = if demo { demo_usage() } else { usage_for_output(&collector, flag("cached-usage")) };
                render_usage(&mut screen, &usage, 0, false, &view.message);
            }
            let path = PathBuf::from(snapshot);
            screen.save(&path).map_err(|e| e.to_string())?;
            println!("{}", crate::model::realpath(&path).display());
        } else if flag("usage") {
            let usage = if demo { demo_usage() } else { usage_for_output(&collector, flag("cached-usage")) };
            if json_out {
                print_json(&usage.to_json());
            } else {
                let color = use_color();
                let columns =
                    if std::io::stdout().is_terminal() { crossterm::terminal::size().map(|(c, _)| c as i64).unwrap_or(140) } else { 140 };
                println!("{}", paint(&tr("ais · Usage · latest provider limits\n"), "accent", true, color));
                for (text, style) in usage_content(&usage, columns.max(80), now()) {
                    println!("{}", if text.is_empty() { String::new() } else { paint(&text, style, style == "cyan", color) });
                }
                for r in &usage.reports {
                    let source = if r.cached { tr("agy CLI · cached") } else { tr("agy CLI · live") };
                    println!("{} · {source} · {}", r.provider, snapshot_text(r.updated));
                    println!("{}", r.text);
                }
            }
        } else if stats {
            let pool = filter_sessions(&sessions, &Filter { scope: "all", ..filter.clone() });
            if json_out {
                print_json(&stats_json(&pool));
            } else {
                print_stats(&pool, &home_dir, use_color());
            }
        } else if doctor {
            let data: IndexMap<&str, Value> = CLIENTS
                .iter()
                .map(|c| {
                    let source = &collector.sources[*c];
                    (
                        *c,
                        json!({
                            "installed": which(c).map(|p| p.to_string_lossy().into_owned()),
                            "sessions": source.count,
                            "roots": source.roots,
                            "error": source.error,
                        }),
                    )
                })
                .collect();
            if json_out {
                print_json(&json!(data));
            } else {
                for (c, info) in &data {
                    let installed = info["installed"].as_str().map(str::to_string).unwrap_or_else(|| tr("CLI not installed"));
                    println!(
                        "{}",
                        trf!("{v0:8} {v1:5} sessions · {v2}", v0 = label(c), v1 = info["sessions"].as_i64().unwrap_or(0), v2 = installed)
                    );
                    for root in info["roots"].as_array().into_iter().flatten() {
                        println!("          {}", root.as_str().unwrap_or(""));
                    }
                    if let Some(error) = info["error"].as_str().filter(|e| !e.is_empty()) {
                        println!("{}{error}", tr("          Error: "));
                    }
                }
                println!("{}", tr("\nSources are read-only. All statuses are saved records, not live."));
            }
        } else if json_out {
            print_json(&serde_json::to_value(&selected).unwrap_or_default());
        } else if demo && !output_mode {
            // A demo TUI uses the same renderer but cannot launch fake
            // conversations. Snapshot mode is the normal visual QA route.
            let cols = *matches.get_one::<i64>("cols").unwrap() as usize;
            let rows = *matches.get_one::<i64>("rows").unwrap() as usize;
            println!("{}", render(&sessions, &mut view, cols, rows, &[], None).plain());
        } else {
            let color = use_color();
            println!(
                "{}",
                paint(
                    &trf!("ais · AI Sessions · {v0} · {v1} sessions\n", v0 = pretty_path(&cwd, Some(&home_dir)), v1 = selected.len()),
                    "accent",
                    true,
                    color
                )
            );
            for s in &selected {
                let status = status_text(&s.status);
                println!(
                    "{}{}{}  {}",
                    paint(&ljust(label(&s.client), 7), client_style(&s.client), true, color),
                    paint(&format!(" {}  ", rjust(&age(s.updated, None), 9)), "muted", false, color),
                    paint(&ljust(&status, 15), status_style(&s.status), false, color),
                    paint(&line(if s.title.is_empty() { &s.id } else { &s.title }, 100), "text", true, color)
                );
                let model = if s.model.is_empty() { tr("model not recorded") } else { s.model.clone() };
                println!(
                    "        {}{}",
                    paint(&pretty_path(&s.cwd, Some(&home_dir)), "cyan", false, color),
                    paint(&format!(" · {model} · {}", s.id), "muted", false, color)
                );
            }
            for error in &collector.errors {
                eprintln!("{}{error}", tr("Warning: "));
            }
        }
        if pending.is_none() {
            return Ok(0);
        }
    }
    if !flag("no-update-check") {
        crate::update::background_check(&mut view);
    }
    browser_loop(&mut collector, &mut view, pending, flag("no-return"), new_tab)
}

fn expand(path: &str) -> PathBuf {
    match path.strip_prefix("~/") {
        Some(rest) => home().join(rest),
        None if path == "~" => home(),
        None => PathBuf::from(path),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stats_count_clients_projects_and_activity() {
        let sessions = demo_sessions("/tmp");
        let value = stats_json(&sessions);
        assert_eq!(value["sessions"], 7);
        assert_eq!(value["clients"]["omp"], 2);
        assert_eq!(value["activity"]["1d"], 3);
    }

    #[test]
    fn parser_accepts_documented_flags() {
        let m = command().try_get_matches_from(["ais", "--json", "-n", "3", "--source", "omp=/x", "--client", "codex"]).unwrap();
        assert!(m.get_flag("json"));
        assert_eq!(*m.get_one::<i64>("limit").unwrap(), 3);
        assert!(command().try_get_matches_from(["ais", "--new-tab", "--same-tab"]).is_err());
        assert!(command().try_get_matches_from(["ais", "--client", "nope"]).is_err());
    }
}
