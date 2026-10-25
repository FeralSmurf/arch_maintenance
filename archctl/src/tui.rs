use anyhow::Result;
use crossterm::{
    cursor::{Hide, Show},
    event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap},
    Frame, Terminal,
};
use std::io::stdout;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::time::Duration;

use crate::journal;
use crate::runner::{self, RunOutcome};
use crate::steps::{Category, Step};

type Tui = Terminal<ratatui::backend::CrosstermBackend<std::io::Stdout>>;

pub struct StepState {
    pub step: Step,
    pub enabled: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepStatus {
    Ran,
    Aborted,
    NotRun,
}

pub struct ResultsView {
    pub rows: Vec<(&'static str, StepStatus)>,
    pub exit: Option<i32>,
    pub success: bool,
    pub duration: Duration,
}

pub struct JournalScreen {
    pub report: journal::Report,
    pub expanded: bool,
}

#[derive(Clone, Copy)]
enum Updates {
    Checking,
    Count(usize),
    Unavailable,
}

pub enum View {
    Menu,
    Confirm,
    DryRun(Text<'static>),
    Results(ResultsView),
    Journal(JournalScreen),
    Text(Text<'static>),
    Help,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Screen {
    Menu,
    Confirm,
    DryRun,
    Results,
    Journal,
    Text,
    Help,
}

impl View {
    fn screen(&self) -> Screen {
        match self {
            View::Menu => Screen::Menu,
            View::Confirm => Screen::Confirm,
            View::DryRun(_) => Screen::DryRun,
            View::Results(_) => Screen::Results,
            View::Journal(_) => Screen::Journal,
            View::Text(_) => Screen::Text,
            View::Help => Screen::Help,
        }
    }
}

enum Action {
    Run,
}

#[derive(Clone)]
struct LastRun {
    when: String,
    ok: bool,
    steps: usize,
}

pub struct App {
    pub steps: Vec<StepState>,
    pub list_state: ListState,
    pub selected: usize,
    pub view: View,
    pub scroll: u16,
    pub auto_confirm: Option<bool>,
    pub dry_run: bool,
    pub script_path: String,
    pub should_quit: bool,
    pub boot_summary: Option<journal::Summary>,
    pub upgrade_incomplete: bool,
    updates: Updates,
    updates_rx: Option<Receiver<Result<Option<usize>>>>,
    last_run: Option<LastRun>,
}

impl App {
    pub fn new(
        steps: Vec<Step>,
        auto_confirm: Option<bool>,
        dry_run: bool,
        script_path: String,
        boot_summary: Option<journal::Summary>,
    ) -> Self {
        let step_states: Vec<StepState> = steps
            .into_iter()
            .map(|s| StepState {
                step: s,
                enabled: true,
            })
            .collect();

        let mut list_state = ListState::default();
        list_state.select(Some(0));

        App {
            steps: step_states,
            list_state,
            selected: 0,
            view: View::Menu,
            scroll: 0,
            auto_confirm,
            dry_run,
            script_path,
            should_quit: false,
            boot_summary,
            upgrade_incomplete: false,
            updates: Updates::Checking,
            updates_rx: None,
            last_run: last_run(),
        }
    }

    fn enabled_count(&self) -> usize {
        self.steps.iter().filter(|s| s.enabled).count()
    }

    fn selected_steps(&self) -> Vec<&Step> {
        self.steps
            .iter()
            .filter(|s| s.enabled)
            .map(|s| &s.step)
            .collect()
    }

    pub fn toggle(&mut self) {
        if let Some(step) = self.steps.get_mut(self.selected) {
            step.enabled = !step.enabled;
        }
    }

    pub fn select_all(&mut self) {
        for s in &mut self.steps {
            s.enabled = true;
        }
    }

    pub fn deselect_all(&mut self) {
        for s in &mut self.steps {
            s.enabled = false;
        }
    }

    pub fn invert_selection(&mut self) {
        for s in &mut self.steps {
            s.enabled = !s.enabled;
        }
    }

    pub fn next(&mut self) {
        if !self.steps.is_empty() && self.selected < self.steps.len() - 1 {
            self.selected += 1;
            self.list_state.select(Some(self.selected));
        }
    }

    pub fn previous(&mut self) {
        if self.selected > 0 {
            self.selected -= 1;
            self.list_state.select(Some(self.selected));
        }
    }

    pub fn first(&mut self) {
        if !self.steps.is_empty() {
            self.selected = 0;
            self.list_state.select(Some(0));
        }
    }

    pub fn last(&mut self) {
        if !self.steps.is_empty() {
            self.selected = self.steps.len() - 1;
            self.list_state.select(Some(self.selected));
        }
    }

    /// Open the confirmation screen, refusing package operations after a
    /// failed upgrade (a partial-upgrade state must be resolved first).
    fn open_confirm(&mut self) {
        if self.enabled_count() == 0 {
            self.message(
                "No steps selected!\n\n\
                 Select at least one step with Space, or press 'a' to select all.",
            );
            return;
        }

        if self.upgrade_incomplete && self.selected_steps().iter().any(|s| s.mutates_packages()) {
            self.message(
                "A previous system upgrade did not complete.\n\n\
                 Finish it before running more package operations:\n\n    \
                 sudo pacman -Syu\n\n\
                 Package operations stay blocked until an upgrade finishes cleanly.",
            );
            return;
        }

        self.view = View::Confirm;
    }

    fn message(&mut self, text: &str) {
        self.view = View::Text(Text::from(text.to_string()));
        self.scroll = 0;
    }

    fn show_dry_run(&mut self) {
        let selected = self.selected_steps();
        let mut lines = vec![
            "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━".to_string(),
            " ARCHCTL - DRY RUN EXECUTION PLAN".to_string(),
            "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━".to_string(),
            String::new(),
            format!(" Target script : {}", self.script_path),
            format!(" Auto-confirm  : {}", auto_confirm_label(self.auto_confirm)),
            format!(" Selected steps: {} of {}", selected.len(), self.steps.len()),
            String::new(),
            "Execution sequence:".to_string(),
        ];

        for (i, step) in selected.iter().enumerate() {
            lines.push(format!(
                "  {:2}. [{}] {} - {}",
                i + 1,
                category_label(step.category).0,
                step.name,
                step.description
            ));
        }

        lines.push(String::new());
        lines.push("No changes were made to your system.".to_string());
        lines.push("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━".to_string());

        self.view = View::DryRun(Text::from(lines.join("\n")));
        self.scroll = 0;
    }

    fn show_journal(&mut self) {
        match journal::fetch_report() {
            Ok(report) => {
                self.view = View::Journal(JournalScreen {
                    report,
                    expanded: false,
                });
            }
            Err(e) => {
                self.message(&format!(
                    "Failed to fetch journald warnings:\n\n{}\n\nPress Esc to return.",
                    e
                ));
            }
        }
        self.scroll = 0;
    }

    fn show_script(&mut self) {
        let selected = self.selected_steps();
        let preview = runner::generate_script(&self.script_path, &selected, self.auto_confirm)
            .and_then(|prepared| prepared.script_text());

        match preview {
            Ok(text) => {
                self.view = View::Text(Text::from(text));
                self.scroll = 0;
            }
            Err(e) => {
                self.message(&format!("Could not generate the script:\n\n{}", e));
            }
        }
    }

    fn poll_updates(&mut self) {
        if let Some(rx) = self.updates_rx.take() {
            match rx.try_recv() {
                Ok(Ok(Some(count))) => self.updates = Updates::Count(count),
                Ok(Ok(None)) | Ok(Err(_)) | Err(TryRecvError::Disconnected) => {
                    self.updates = Updates::Unavailable
                }
                Err(TryRecvError::Empty) => self.updates_rx = Some(rx),
            }
        }
    }

    fn on_key(&mut self, key: KeyEvent) -> Option<Action> {
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            self.should_quit = true;
            return None;
        }

        match self.view.screen() {
            Screen::Menu => self.on_menu_key(key),
            Screen::Confirm => {
                match key.code {
                    KeyCode::Enter if self.dry_run => self.show_dry_run(),
                    KeyCode::Enter => return Some(Action::Run),
                    KeyCode::Esc | KeyCode::Char('q') => self.back_to_menu(),
                    _ => {}
                }
                None
            }
            Screen::DryRun => {
                match key.code {
                    KeyCode::Char('r') | KeyCode::Char('R') => {
                        self.dry_run = false;
                        return Some(Action::Run);
                    }
                    KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q') => self.back_to_menu(),
                    _ => self.scroll_key(key),
                }
                None
            }
            Screen::Results => {
                match key.code {
                    KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q') => self.back_to_menu(),
                    _ => self.scroll_key(key),
                }
                None
            }
            Screen::Journal => {
                match key.code {
                    KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q') => self.back_to_menu(),
                    KeyCode::Char('t') | KeyCode::Char('T') => {
                        if let View::Journal(screen) = &mut self.view {
                            screen.expanded = !screen.expanded;
                        }
                        self.scroll = 0;
                    }
                    _ => self.scroll_key(key),
                }
                None
            }
            Screen::Text => {
                match key.code {
                    KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q') => self.back_to_menu(),
                    _ => self.scroll_key(key),
                }
                None
            }
            Screen::Help => {
                match key.code {
                    KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('?') => self.back_to_menu(),
                    _ => self.scroll_key(key),
                }
                None
            }
        }
    }

    fn on_menu_key(&mut self, key: KeyEvent) -> Option<Action> {
        match key.code {
            KeyCode::Char('q') | KeyCode::Char('Q') => self.should_quit = true,
            KeyCode::Down | KeyCode::Char('j') => self.next(),
            KeyCode::Up | KeyCode::Char('k') => self.previous(),
            KeyCode::Home | KeyCode::Char('g') => self.first(),
            KeyCode::End | KeyCode::Char('G') => self.last(),
            KeyCode::Char(' ') => self.toggle(),
            KeyCode::Char('a') | KeyCode::Char('A') => self.select_all(),
            KeyCode::Char('d') | KeyCode::Char('D') => self.deselect_all(),
            KeyCode::Char('i') | KeyCode::Char('I') => self.invert_selection(),
            KeyCode::Char('y') | KeyCode::Char('Y') => {
                self.auto_confirm = if self.auto_confirm == Some(true) {
                    None
                } else {
                    Some(true)
                };
            }
            KeyCode::Char('n') | KeyCode::Char('N') => {
                self.auto_confirm = if self.auto_confirm == Some(false) {
                    None
                } else {
                    Some(false)
                };
            }
            KeyCode::Char('c') | KeyCode::Char('C') => self.auto_confirm = None,
            KeyCode::Char('J') => self.show_journal(),
            KeyCode::Char('v') | KeyCode::Char('V') => self.show_script(),
            KeyCode::Char('?') => {
                self.view = View::Help;
                self.scroll = 0;
            }
            KeyCode::Enter => self.open_confirm(),
            _ => {}
        }
        None
    }

    fn back_to_menu(&mut self) {
        self.view = View::Menu;
        self.scroll = 0;
    }

    fn scroll_key(&mut self, key: KeyEvent) {
        self.scroll = match key.code {
            KeyCode::Up | KeyCode::Char('k') => self.scroll.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => self.scroll.saturating_add(1),
            KeyCode::PageUp => self.scroll.saturating_sub(15),
            KeyCode::PageDown => self.scroll.saturating_add(15),
            KeyCode::Home | KeyCode::Char('g') => 0,
            KeyCode::End | KeyCode::Char('G') => u16::MAX / 2,
            _ => self.scroll,
        };
    }
}

pub fn run(script_path: &str, auto_confirm: Option<bool>, dry_run: bool) -> Result<()> {
    if !std::path::Path::new(script_path).exists() {
        anyhow::bail!("Script not found: {}", script_path);
    }

    // Set panic hook to ensure terminal is restored if panic occurs
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let _ = execute!(std::io::stdout(), LeaveAlternateScreen, Show);
        default_hook(info);
    }));

    enable_raw_mode()?;
    let mut stdout = stdout();
    execute!(stdout, EnterAlternateScreen, Hide)?;
    let backend = ratatui::backend::CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let steps = Step::all();
    // A missing journalctl must not stop the TUI from starting.
    let boot_summary = journal::fetch_summary().ok();
    let mut app = App::new(
        steps,
        auto_confirm,
        dry_run,
        script_path.to_string(),
        boot_summary,
    );
    app.updates_rx = Some(spawn_updates_check());

    while !app.should_quit {
        terminal.draw(|frame| ui(frame, &mut app))?;
        app.poll_updates();

        // Poll instead of blocking so the background update check can land.
        if !event::poll(Duration::from_millis(200))? {
            continue;
        }
        let Event::Key(key) = event::read()? else {
            continue;
        };
        // Only handle key press events, ignore key release/repeat
        if key.kind != KeyEventKind::Press {
            continue;
        }

        if let Some(Action::Run) = app.on_key(key) {
            execute(&mut terminal, &mut app)?;
        }
    }

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen, Show)?;
    terminal.show_cursor()?;
    Ok(())
}

/// Leave the TUI, run the selected steps interactively, then come back with a
/// results screen.
fn execute(terminal: &mut Tui, app: &mut App) -> Result<()> {
    let selected = app.selected_steps();

    // Generate before leaving the alternate screen so contract errors (a
    // missing step marker) are shown inside the TUI.
    let prepared =
        match runner::generate_script(&app.script_path, &selected, app.auto_confirm) {
            Ok(prepared) => prepared,
            Err(e) => {
                app.view = View::Text(Text::from(format!(
                    "Could not prepare the run:\n\n{}\n\nNothing was executed.",
                    e
                )));
                app.scroll = 0;
                return Ok(());
            }
        };

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen, Show)?;

    let outcome = prepared.run(app.auto_confirm);

    println!();
    println!("\x1b[1;36m━━━┫ Execution Finished ┣━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━\x1b[0m");
    match &outcome {
        Ok(outcome) if outcome.status.success() => {
            println!("\x1b[1;32m[✓] Run completed (exit 0).\x1b[0m");
        }
        Ok(outcome) => match outcome.status.code() {
            Some(code) => println!("\x1b[1;31m[✗] Run failed (exit {}).\x1b[0m", code),
            None => println!("\x1b[1;33m[!] Run was interrupted.\x1b[0m"),
        },
        Err(e) => println!("\x1b[1;31m[✗] Execution error: {}\x1b[0m", e),
    }
    println!();
    println!("\x1b[2mPress Enter to return to archctl...\x1b[0m");
    let mut buf = String::new();
    let _ = std::io::stdin().read_line(&mut buf);

    execute!(terminal.backend_mut(), EnterAlternateScreen, Hide)?;
    enable_raw_mode()?;
    terminal.clear()?;

    match outcome {
        Ok(outcome) => {
            let success = outcome.status.success();
            let rows = classify(&selected, &outcome);
            record_history(&rows, success, outcome.status.code());
            if selected.iter().any(|s| s.category == Category::Upgrade) {
                app.upgrade_incomplete = !success;
            }
            app.view = View::Results(ResultsView {
                rows,
                exit: outcome.status.code(),
                success,
                duration: outcome.duration,
            });
        }
        Err(e) => {
            app.message(&format!("The run could not be started:\n\n{}", e));
        }
    }
    app.scroll = 0;
    Ok(())
}

fn classify(selected: &[&Step], outcome: &RunOutcome) -> Vec<(&'static str, StepStatus)> {
    let mut rows = Vec::new();
    let mut aborted = outcome.status.success();

    for step in selected {
        if outcome.ran.iter().any(|name| name == step.name) {
            rows.push((step.name, StepStatus::Ran));
        } else if !aborted {
            rows.push((step.name, StepStatus::Aborted));
            aborted = true;
        } else {
            rows.push((step.name, StepStatus::NotRun));
        }
    }

    rows
}

fn ui(frame: &mut Frame, app: &mut App) {
    let area = frame.area();

    if matches!(app.view, View::Menu) {
        draw_menu(frame, area, app);
        return;
    }

    match &app.view {
        View::Menu => {}
        View::Confirm => draw_scrollable(
            frame,
            area,
            " Confirm Run ",
            confirm_view(app),
            " Enter = run    Esc = back ",
            app.scroll,
        ),
        View::DryRun(text) => draw_scrollable(
            frame,
            area,
            " Dry Run ",
            text.clone(),
            " R = run for real    Esc = back ",
            app.scroll,
        ),
        View::Results(results) => draw_scrollable(
            frame,
            area,
            " Run Results ",
            results_view(results),
            " Esc = back ",
            app.scroll,
        ),
        View::Journal(screen) => draw_journal(frame, area, screen, app.scroll),
        View::Text(text) => draw_scrollable(
            frame,
            area,
            " archctl ",
            text.clone(),
            " Esc = back ",
            app.scroll,
        ),
        View::Help => draw_scrollable(frame, area, " Help ", help_view(), " Esc = back ", app.scroll),
    }
}

fn draw_menu(frame: &mut Frame, area: Rect, app: &mut App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(4),
            Constraint::Min(8),
            Constraint::Length(4),
        ])
        .split(area);

    let enabled_count = app.enabled_count();
    let total_count = app.steps.len();

    // 1. Header
    let header_line = Line::from(vec![
        Span::styled(
            " archctl ",
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            " Arch Linux Maintenance ",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled("│ Script: ", Style::default().fg(Color::DarkGray)),
        Span::styled(app.script_path.as_str(), Style::default().fg(Color::White)),
        Span::styled(
            format!(" │ Selected: {}/{} ", enabled_count, total_count),
            Style::default()
                .fg(if enabled_count > 0 {
                    Color::Green
                } else {
                    Color::Yellow
                })
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("│ Mode: {} ", if app.dry_run { "DRY-RUN" } else { "LIVE" }),
            Style::default()
                .fg(if app.dry_run {
                    Color::Yellow
                } else {
                    Color::Green
                })
                .add_modifier(Modifier::BOLD),
        ),
    ]);

    let mut status_spans = boot_summary_spans(&app.boot_summary);
    status_spans.push(Span::styled(" │ ", Style::default().fg(Color::DarkGray)));
    match app.updates {
        Updates::Checking => status_spans.push(Span::styled(
            "Updates: checking…",
            Style::default().fg(Color::DarkGray),
        )),
        Updates::Count(0) => status_spans.push(Span::styled(
            "Updates: up to date",
            Style::default().fg(Color::Green),
        )),
        Updates::Count(count) => status_spans.push(Span::styled(
            format!("Updates: {} pending", count),
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )),
        Updates::Unavailable => status_spans.push(Span::styled(
            "Updates: n/a",
            Style::default().fg(Color::DarkGray),
        )),
    }

    let header = Paragraph::new(vec![header_line, Line::from(status_spans)]).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Cyan)),
    );
    frame.render_widget(header, chunks[0]);

    // 2. Steps List
    let items: Vec<ListItem> = app
        .steps
        .iter()
        .map(|s| {
            let (check_str, check_style) = if s.enabled {
                (
                    "[✓] ",
                    Style::default()
                        .fg(Color::Green)
                        .add_modifier(Modifier::BOLD),
                )
            } else {
                ("[ ] ", Style::default().fg(Color::DarkGray))
            };

            let (cat_str, cat_style) = category_label(s.step.category);

            let name_style = if s.enabled {
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::DarkGray)
            };

            let desc_style = Style::default().fg(Color::DarkGray);

            ListItem::new(Line::from(vec![
                Span::styled(check_str, check_style),
                Span::styled(cat_str, cat_style),
                Span::styled(format!(" {:<24} ", s.step.name), name_style),
                Span::styled(s.step.description, desc_style),
            ]))
        })
        .collect();

    let title = if app.upgrade_incomplete {
        Line::from(Span::styled(
            " ⚠ Upgrade did not complete — finish pacman -Syu first ",
            Style::default()
                .fg(Color::Red)
                .add_modifier(Modifier::BOLD),
        ))
    } else {
        Line::from(" Steps to Execute ")
    };

    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(title)
                .border_style(Style::default().fg(if app.upgrade_incomplete {
                    Color::Red
                } else {
                    Color::Blue
                })),
        )
        .highlight_style(
            Style::default()
                .bg(Color::Rgb(40, 44, 52))
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▶ ");

    frame.render_stateful_widget(list, chunks[1], &mut app.list_state);

    // 3. Footer
    let (auto_text, auto_style) = match app.auto_confirm {
        Some(true) => (
            "YES (Auto-confirm)",
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        ),
        Some(false) => (
            "NO (Auto-decline)",
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        ),
        None => (
            "ASK (Interactive)",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
    };

    let footer_lines = vec![
        Line::from(vec![
            Span::styled(" Navigation: ", Style::default().fg(Color::DarkGray)),
            Span::styled("↑/↓/j/k", Style::default().fg(Color::Cyan)),
            Span::raw(" Select  "),
            Span::styled("Space", Style::default().fg(Color::Cyan)),
            Span::raw(" Toggle  "),
            Span::styled("a", Style::default().fg(Color::Cyan)),
            Span::raw(" All  "),
            Span::styled("d", Style::default().fg(Color::Cyan)),
            Span::raw(" None  "),
            Span::styled("i", Style::default().fg(Color::Cyan)),
            Span::raw(" Invert  "),
            Span::styled(
                "Enter",
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(" Confirm  ", Style::default().fg(Color::Green)),
            Span::styled("J", Style::default().fg(Color::Red)),
            Span::raw(" Journal  "),
            Span::styled("v", Style::default().fg(Color::Cyan)),
            Span::raw(" Script  "),
            Span::styled("?", Style::default().fg(Color::Cyan)),
            Span::raw(" Help  "),
            Span::styled("q", Style::default().fg(Color::Red)),
            Span::raw(" Quit"),
        ]),
        Line::from(vec![
            Span::styled(" Options:    ", Style::default().fg(Color::DarkGray)),
            Span::styled("Auto-Confirm: ", Style::default().fg(Color::White)),
            Span::styled(format!("[{}]", auto_text), auto_style),
            Span::styled(" (Toggle: ", Style::default().fg(Color::DarkGray)),
            Span::styled("y", Style::default().fg(Color::Green)),
            Span::styled("=Yes, ", Style::default().fg(Color::DarkGray)),
            Span::styled("n", Style::default().fg(Color::Red)),
            Span::styled("=No, ", Style::default().fg(Color::DarkGray)),
            Span::styled("c", Style::default().fg(Color::Yellow)),
            Span::styled("=Ask)", Style::default().fg(Color::DarkGray)),
        ]),
    ];

    let footer = Paragraph::new(footer_lines).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::DarkGray)),
    );
    frame.render_widget(footer, chunks[2]);
}

fn confirm_view(app: &App) -> Text<'static> {
    let mut lines = vec![Line::from(Span::styled(
        "The following steps will run:",
        Style::default().add_modifier(Modifier::BOLD),
    ))];

    for step in app.selected_steps() {
        let (label, style) = category_label(step.category);
        let mut spans = vec![
            Span::styled(format!(" {} ", label), style),
            Span::styled(format!(" {}", step.name), Style::default().fg(Color::White)),
        ];
        if step.is_destructive() {
            spans.push(Span::styled(
                "  destructive",
                Style::default()
                    .fg(Color::Red)
                    .add_modifier(Modifier::BOLD),
            ));
        }
        lines.push(Line::from(spans));
        lines.push(Line::from(Span::styled(
            format!("    {}", step.description),
            Style::default().fg(Color::DarkGray),
        )));
    }

    lines.push(Line::from(""));
    lines.push(Line::from(format!(
        " Auto-confirm: {}",
        auto_confirm_label(app.auto_confirm)
    )));
    lines.push(Line::from(format!(
        " Mode:         {}",
        if app.dry_run {
            "DRY-RUN (plan only)"
        } else {
            "LIVE"
        }
    )));
    if let Some(last) = &app.last_run {
        lines.push(Line::from(format!(
            " Last run:     {} — {} ({} step{})",
            last.when,
            if last.ok { "ok" } else { "failed" },
            last.steps,
            if last.steps == 1 { "" } else { "s" }
        )));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "Enter to start, Esc to go back.",
        Style::default().fg(Color::Green),
    )));

    Text::from(lines)
}

fn results_view(results: &ResultsView) -> Text<'static> {
    let (headline, style) = if results.success {
        (
            format!("Exit 0 — completed in {:.1}s", results.duration.as_secs_f64()),
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        )
    } else {
        (
            format!(
                "Exit {} — failed after {:.1}s",
                results
                    .exit
                    .map(|c| c.to_string())
                    .unwrap_or_else(|| "signal".to_string()),
                results.duration.as_secs_f64()
            ),
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        )
    };

    let mut lines = vec![Line::from(Span::styled(headline, style)), Line::from("")];

    for (name, status) in &results.rows {
        let (symbol, label, style) = match status {
            StepStatus::Ran => (
                "✓",
                "ran",
                Style::default().fg(Color::Green),
            ),
            StepStatus::Aborted => (
                "✗",
                "aborted here",
                Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
            ),
            StepStatus::NotRun => ("–", "not reached", Style::default().fg(Color::DarkGray)),
        };
        lines.push(Line::from(vec![
            Span::styled(format!(" {} ", symbol), style),
            Span::styled(format!("{:<28}", name), Style::default().fg(Color::White)),
            Span::styled(label, style),
        ]));
    }

    Text::from(lines)
}

fn help_view() -> Text<'static> {
    let lines = vec![
        "Menu",
        "  ↑/↓, j/k        Move selection",
        "  Space           Toggle step",
        "  a / d / i       Select all / none / invert",
        "  Enter           Review and run",
        "  y / n / c       Auto-confirm yes / no / ask",
        "  J               Journal, grouped by message",
        "  v               View the generated script",
        "  ?               This help",
        "  q               Quit",
        "",
        "Confirm",
        "  Enter           Start the run",
        "  Esc             Back",
        "",
        "Journal",
        "  t               Expand / collapse details",
        "  j/k, PgUp/PgDn  Scroll",
        "  Esc             Back",
        "",
        "Dry run",
        "  R               Run for real",
        "",
        "Ctrl+C quits from anywhere.",
    ];
    Text::from(lines.join("\n"))
}

fn draw_scrollable(
    frame: &mut Frame,
    area: Rect,
    title: &str,
    text: Text<'static>,
    footer: &str,
    scroll: u16,
) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(3)])
        .split(area);

    let paragraph = Paragraph::new(text)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(title.to_string())
                .border_style(Style::default().fg(Color::Cyan)),
        )
        .wrap(Wrap { trim: false })
        .scroll((scroll, 0));
    frame.render_widget(paragraph, chunks[0]);

    let footer = Paragraph::new(Line::from(Span::styled(
        footer,
        Style::default().fg(Color::Cyan),
    )))
    .block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::DarkGray)),
    );
    frame.render_widget(footer, chunks[1]);
}

fn draw_journal(frame: &mut Frame, area: Rect, screen: &JournalScreen, scroll: u16) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(3)])
        .split(area);

    let report = &screen.report;
    let inner_width = area.width.saturating_sub(2) as usize;

    let mut lines: Vec<Line> = Vec::new();
    if report.groups.is_empty() {
        lines.push(Line::from(Span::styled(
            "No critical or serious messages for the current boot.",
            Style::default().fg(Color::Green),
        )));
    } else if screen.expanded {
        for group in &report.groups {
            lines.push(group_header(group));
            for detail in group.detail.lines() {
                lines.push(Line::from(Span::styled(
                    format!("    {}", detail),
                    severity_style(group.priority),
                )));
            }
            if group.frames > 0 {
                lines.push(Line::from(Span::styled(
                    format!("    [{} backtrace frame lines collapsed]", group.frames),
                    Style::default().fg(Color::DarkGray),
                )));
            }
            lines.push(Line::from(""));
        }
    } else {
        for group in &report.groups {
            let frames = if group.frames > 0 {
                format!(" +{}f", group.frames)
            } else {
                String::new()
            };
            let reserved = 6 + 5 + 17 + 12 + frames.chars().count();
            let message = truncate_chars(
                &group.message,
                inner_width.saturating_sub(reserved).max(12),
            );
            lines.push(Line::from(vec![
                Span::styled(
                    format!("{} ", severity_tag(group.priority)),
                    severity_style(group.priority),
                ),
                Span::styled(
                    format!("{:>3}× ", group.count),
                    Style::default()
                        .fg(if group.count > 1 {
                            Color::Yellow
                        } else {
                            Color::DarkGray
                        })
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!("{:<16} ", truncate_chars(&group.source, 16)),
                    Style::default().fg(Color::Cyan),
                ),
                Span::styled(message, Style::default().fg(Color::White)),
                Span::styled(frames, Style::default().fg(Color::DarkGray)),
                Span::styled(
                    format!(" {}", group.last),
                    Style::default().fg(Color::DarkGray),
                ),
            ]));
        }
    }

    let title = format!(
        " Journal — {} entries in {} groups ",
        report.total,
        report.groups.len()
    );
    let paragraph = Paragraph::new(lines)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(title)
                .border_style(Style::default().fg(Color::Cyan)),
        )
        .wrap(Wrap { trim: false })
        .scroll((scroll, 0));
    frame.render_widget(paragraph, chunks[0]);

    let footer = Paragraph::new(Line::from(vec![
        Span::styled(" [t] ", Style::default().fg(Color::Cyan)),
        Span::raw(if screen.expanded {
            "Collapse  "
        } else {
            "Expand  "
        }),
        Span::styled("[↑/↓/j/k] ", Style::default().fg(Color::Cyan)),
        Span::raw("Scroll  "),
        Span::styled("[PgUp/PgDn] ", Style::default().fg(Color::Cyan)),
        Span::raw("Page  │  "),
        Span::styled(
            "[Esc] ",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("Return to menu"),
    ]))
    .block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::DarkGray)),
    );
    frame.render_widget(footer, chunks[1]);
}

fn group_header(group: &journal::Group) -> Line<'static> {
    Line::from(vec![
        Span::styled(
            format!("{} ", severity_tag(group.priority)),
            severity_style(group.priority),
        ),
        Span::styled(
            format!("{}× ", group.count),
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("{} ", group.source),
            Style::default().fg(Color::Cyan),
        ),
        Span::styled(
            format!("{} → {}", group.first, group.last),
            Style::default().fg(Color::DarkGray),
        ),
    ])
}

fn category_label(category: Category) -> (&'static str, Style) {
    match category {
        Category::PreFlight => (
            " PRE-FLIGHT ",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
        Category::Upgrade => (
            "  UPGRADE   ",
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        ),
        Category::Maintenance => (
            "MAINTENANCE ",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Category::Reboot => (
            "   REBOOT   ",
            Style::default()
                .fg(Color::Magenta)
                .add_modifier(Modifier::BOLD),
        ),
    }
}

fn auto_confirm_label(auto_confirm: Option<bool>) -> &'static str {
    match auto_confirm {
        Some(true) => "YES (auto-confirm all prompts)",
        Some(false) => "NO (auto-decline all prompts)",
        None => "ASK (interactive prompts)",
    }
}

/// The startup summary row: journald severity counts for the current boot.
fn boot_summary_spans(summary: &Option<journal::Summary>) -> Vec<Span<'static>> {
    let mut spans = vec![Span::styled(" Boot: ", Style::default().fg(Color::DarkGray))];

    let levels: Vec<&journal::Level> = match summary {
        Some(summary) => summary.levels.iter().filter(|l| l.count > 0).collect(),
        None => {
            spans.push(Span::styled(
                "journal unavailable",
                Style::default().fg(Color::DarkGray),
            ));
            return spans;
        }
    };

    if levels.is_empty() {
        spans.push(Span::styled(
            "no critical or serious errors",
            Style::default().fg(Color::Green),
        ));
    } else {
        for (i, level) in levels.iter().enumerate() {
            if i > 0 {
                spans.push(Span::styled(", ", Style::default().fg(Color::DarkGray)));
            }
            spans.push(Span::styled(
                format!("{} {}", level.count, level.label),
                severity_style(level.priority),
            ));
        }
    }

    spans
}

/// Color a journald line by its syslog priority (lower number is more severe).
fn severity_style(priority: u8) -> Style {
    match priority {
        0 | 1 => Style::default()
            .fg(Color::Red)
            .add_modifier(Modifier::BOLD),
        2 => Style::default().fg(Color::Red),
        3 => Style::default().fg(Color::Yellow),
        _ => Style::default().fg(Color::DarkGray),
    }
}

fn severity_tag(priority: u8) -> &'static str {
    match priority {
        0 => "EMERG",
        1 => "ALERT",
        2 => "CRIT ",
        _ => "ERROR",
    }
}

fn truncate_chars(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

fn spawn_updates_check() -> Receiver<Result<Option<usize>>> {
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = sender.send(check_updates());
    });
    receiver
}

/// Number of pending updates via checkupdates (pacman-contrib), which uses a
/// temporary database and is safe to run without a full upgrade.
fn check_updates() -> Result<Option<usize>> {
    let output = match std::process::Command::new("timeout")
        .args(["30", "checkupdates"])
        .output()
    {
        Ok(output) => output,
        Err(_) => std::process::Command::new("checkupdates")
            .output()
            .map_err(|e| anyhow::anyhow!("checkupdates unavailable: {}", e))?,
    };

    if output.status.code() == Some(127) {
        return Ok(None);
    }

    let count = String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|line| !line.trim().is_empty())
        .count();
    Ok(Some(count))
}

fn state_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("XDG_STATE_HOME") {
        return Some(PathBuf::from(dir).join("archctl"));
    }
    std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/state/archctl"))
}

fn history_path() -> Option<PathBuf> {
    state_dir().map(|dir| dir.join("history.log"))
}

fn now_string() -> String {
    std::process::Command::new("date")
        .arg("+%Y-%m-%dT%H:%M:%S%z")
        .output()
        .ok()
        .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_string())
        .filter(|stamp| !stamp.is_empty())
        .unwrap_or_else(|| "unknown".to_string())
}

fn record_history(rows: &[(&'static str, StepStatus)], success: bool, exit: Option<i32>) {
    let Some(path) = history_path() else { return };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    let steps = rows
        .iter()
        .map(|(name, _)| *name)
        .collect::<Vec<_>>()
        .join(", ");
    let status = if success { "ok" } else { "failed" };
    let exit = exit.map(|code| code.to_string()).unwrap_or_else(|| "signal".to_string());
    let line = format!("{}\t{}\t{}\t{}\n", now_string(), status, exit, steps);

    use std::io::Write;
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = file.write_all(line.as_bytes());
    }
}

fn last_run() -> Option<LastRun> {
    let text = std::fs::read_to_string(history_path()?).ok()?;
    let line = text.lines().last()?;
    let mut parts = line.split('\t');
    let when = parts.next()?.to_string();
    let ok = parts.next()? == "ok";
    let _exit = parts.next()?;
    let steps = parts.next().map(|names| names.split(',').count()).unwrap_or(0);

    Some(LastRun { when, ok, steps })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::process::ExitStatusExt;
    use std::process::ExitStatus;

    fn app() -> App {
        App::new(Step::all(), None, false, "update.sh".to_string(), None)
    }

    fn outcome(success: bool, ran: &[&str]) -> RunOutcome {
        RunOutcome {
            status: ExitStatus::from_raw(if success { 0 } else { 1 << 8 }),
            ran: ran.iter().map(|name| name.to_string()).collect(),
            duration: Duration::from_millis(10),
        }
    }

    fn enable_only(app: &mut App, name: &str) {
        app.deselect_all();
        let step = app
            .steps
            .iter_mut()
            .find(|state| state.step.name == name)
            .expect("step exists");
        step.enabled = true;
    }

    #[test]
    fn test_classify_marks_where_the_run_aborted() {
        let steps = Step::all();
        let selected = vec![&steps[0], &steps[1], &steps[3]];

        let rows = classify(&selected, &outcome(false, &["Pre-flight checks"]));
        assert_eq!(rows[0].1, StepStatus::Ran);
        assert_eq!(rows[1].1, StepStatus::Aborted);
        assert_eq!(rows[2].1, StepStatus::NotRun);

        let rows = classify(
            &selected,
            &outcome(
                true,
                &["Pre-flight checks", "System upgrade", "Orphaned packages"],
            ),
        );
        assert!(rows.iter().all(|(_, status)| *status == StepStatus::Ran));
    }

    #[test]
    fn test_failed_upgrade_blocks_package_operations_until_resolved() {
        let mut app = app();
        app.upgrade_incomplete = true;
        enable_only(&mut app, "Orphaned packages");
        app.open_confirm();
        assert!(
            matches!(app.view, View::Text(_)),
            "orphan removal must be blocked after a failed upgrade"
        );

        app.view = View::Menu;
        enable_only(&mut app, "Pacnew/pacsave check");
        app.open_confirm();
        assert!(
            matches!(app.view, View::Confirm),
            "read-only steps stay available"
        );

        app.view = View::Menu;
        app.upgrade_incomplete = false;
        enable_only(&mut app, "Orphaned packages");
        app.open_confirm();
        assert!(matches!(app.view, View::Confirm));
    }

    #[test]
    fn test_confirm_refuses_empty_selection() {
        let mut app = app();
        app.deselect_all();
        app.open_confirm();
        assert!(matches!(app.view, View::Text(_)));
    }
}
