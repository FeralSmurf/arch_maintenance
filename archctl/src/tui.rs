use anyhow::Result;
use crossterm::{
    cursor::{Hide, Show},
    event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
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

use crate::journal;
use crate::runner;
use crate::steps::{Category, Step};

pub struct StepState {
    pub step: Step,
    pub enabled: bool,
}

pub struct App {
    pub steps: Vec<StepState>,
    pub list_state: ListState,
    pub selected: usize,
    pub output: Option<Text<'static>>,
    pub scroll: u16,
    pub auto_confirm: Option<bool>,
    pub dry_run: bool,
    pub script_path: String,
    pub should_quit: bool,
    /// Severity counts for the current boot, fetched at startup.
    pub boot_summary: Option<journal::Summary>,
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
            output: None,
            scroll: 0,
            auto_confirm,
            dry_run,
            script_path,
            should_quit: false,
            boot_summary,
        }
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

    pub fn show_dry_run(&mut self) {
        let selected: Vec<&Step> = self
            .steps
            .iter()
            .filter(|s| s.enabled)
            .map(|s| &s.step)
            .collect();

        let mut lines = Vec::new();
        lines.push("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━".to_string());
        lines.push(" ARCHCTL - DRY RUN EXECUTION PLAN".to_string());
        lines.push("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━".to_string());
        lines.push(String::new());
        lines.push(format!(" Target script : {}", self.script_path));
        let auto_str = match self.auto_confirm {
            Some(true) => "YES (auto-confirm all prompts)",
            Some(false) => "NO (auto-decline all prompts)",
            None => "ASK (interactive prompts)",
        };
        lines.push(format!(" Auto-confirm  : {}", auto_str));
        lines.push(format!(" Selected steps: {} of {}", selected.len(), self.steps.len()));
        lines.push(String::new());
        lines.push("Execution sequence:".to_string());
        for (i, step) in selected.iter().enumerate() {
            let cat_label = match step.category {
                Category::PreFlight => "PRE-FLIGHT ",
                Category::Upgrade => "UPGRADE    ",
                Category::Maintenance => "MAINTENANCE",
                Category::Reboot => "REBOOT     ",
            };
            lines.push(format!("  {:2}. [{}] {} - {}", i + 1, cat_label, step.name, step.description));
        }
        lines.push(String::new());
        lines.push("No changes were made to your system.".to_string());
        lines.push("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━".to_string());
        lines.push("Press Esc, Enter, or 'q' to return to the menu.".to_string());

        self.output = Some(Text::from(lines.join("\n")));
        self.scroll = 0;
    }

    pub fn show_journal(&mut self) {
        match journal::fetch_boot_warnings() {
            Ok(report) => {
                let total: usize = report.groups.iter().map(|g| g.count).sum();
                let mut lines = vec![
                    Line::from("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"),
                    Line::from(" JOURNALD — CRITICAL & SERIOUS WARNINGS (Current Boot)"),
                    Line::from("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"),
                    Line::from(format!(
                        "  Total: {} entries — priority levels: emerg(0), alert(1), crit(2), err(3)",
                        total
                    )),
                    Line::from("  Grouped by severity, most severe first"),
                    Line::from("  Source: journalctl -p 0..3 -b"),
                    Line::from("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"),
                ];

                if report.groups.is_empty() {
                    match &report.stderr {
                        Some(stderr) => {
                            lines.push(Line::from("  No critical/serious messages found, but journalctl reported:"));
                            for line in indent(stderr).lines() {
                                lines.push(Line::from(line.to_string()));
                            }
                        }
                        None => {
                            lines.push(Line::from("  No critical or serious journald warnings found for the current boot."));
                        }
                    }
                } else {
                    for group in &report.groups {
                        lines.push(Line::from(""));
                        lines.push(section_header(group.label, group.priority, group.count));
                        for text in &group.lines {
                            lines.push(Line::from(Span::styled(
                                text.clone(),
                                severity_style(group.priority),
                            )));
                        }
                    }
                }

                lines.push(Line::from(""));
                lines.push(Line::from("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"));
                lines.push(Line::from("Press Esc, Enter, or 'q' to return to the menu."));

                self.output = Some(Text::from(lines));
            }
            Err(e) => {
                self.output = Some(Text::from(format!(
                    "Failed to fetch journald warnings:\n\n{}\n\nPress Esc, Enter, or 'q' to return to the menu.",
                    e
                )));
            }
        }
        self.scroll = 0;
    }

    pub fn run_selected(&self) -> Result<std::process::ExitStatus> {
        let selected: Vec<&Step> = self
            .steps
            .iter()
            .filter(|s| s.enabled)
            .map(|s| &s.step)
            .collect();

        let script = runner::generate_script(&self.script_path, &selected, self.auto_confirm)?;
        let result = runner::run_interactive(&script, self.auto_confirm);
        runner::cleanup(&script);
        result
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

    while !app.should_quit {
        terminal.draw(|frame| ui(frame, &mut app))?;

        if let Event::Key(key) = event::read()? {
            // Only handle key press events, ignore key release/repeat
            if key.kind != KeyEventKind::Press {
                continue;
            }

            // Ctrl+C quits from anywhere
            if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
                app.should_quit = true;
                break;
            }

            if app.output.is_some() {
                // Output view keybindings
                match key.code {
                    KeyCode::Esc
                    | KeyCode::Enter
                    | KeyCode::Char('q')
                    | KeyCode::Char('Q')
                    | KeyCode::Backspace => {
                        app.output = None;
                        app.scroll = 0;
                    }
                    KeyCode::Up | KeyCode::Char('k') => {
                        app.scroll = app.scroll.saturating_sub(1);
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        app.scroll = app.scroll.saturating_add(1);
                    }
                    KeyCode::PageUp => {
                        app.scroll = app.scroll.saturating_sub(15);
                    }
                    KeyCode::PageDown => {
                        app.scroll = app.scroll.saturating_add(15);
                    }
                    KeyCode::Home | KeyCode::Char('g') => {
                        app.scroll = 0;
                    }
                    KeyCode::End | KeyCode::Char('G') => {
                        app.scroll = u16::MAX / 2;
                    }
                    _ => {}
                }
            } else {
                // Menu view keybindings
                match key.code {
                    KeyCode::Char('q') | KeyCode::Char('Q') => {
                        app.should_quit = true;
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        app.next();
                    }
                    KeyCode::Up | KeyCode::Char('k') => {
                        app.previous();
                    }
                    KeyCode::Home | KeyCode::Char('g') => {
                        app.first();
                    }
                    KeyCode::End | KeyCode::Char('G') => {
                        app.last();
                    }
                    KeyCode::Char(' ') => {
                        app.toggle();
                    }
                    KeyCode::Char('a') | KeyCode::Char('A') => {
                        app.select_all();
                    }
                    KeyCode::Char('d') | KeyCode::Char('D') => {
                        app.deselect_all();
                    }
                    KeyCode::Char('i') | KeyCode::Char('I') => {
                        app.invert_selection();
                    }
                    KeyCode::Char('y') | KeyCode::Char('Y') => {
                        if app.auto_confirm == Some(true) {
                            app.auto_confirm = None;
                        } else {
                            app.auto_confirm = Some(true);
                        }
                    }
                    KeyCode::Char('n') | KeyCode::Char('N') => {
                        if app.auto_confirm == Some(false) {
                            app.auto_confirm = None;
                        } else {
                            app.auto_confirm = Some(false);
                        }
                    }
                    KeyCode::Char('c') | KeyCode::Char('C') => {
                        app.auto_confirm = None;
                    }
                    KeyCode::Char('J') => {
                        app.show_journal();
                    }
                    KeyCode::Enter => {
                        let enabled_count = app.steps.iter().filter(|s| s.enabled).count();
                        if enabled_count == 0 {
                            app.output = Some(Text::from(
                                "No steps selected!\n\nPlease select at least one step using Space, or press 'a' to select all steps.\n\nPress Enter, Esc, or 'q' to return to the menu.",
                            ));
                            app.scroll = 0;
                        } else if app.dry_run {
                            app.show_dry_run();
                        } else {
                            // Leave alternate screen & disable raw mode for interactive execution
                            disable_raw_mode()?;
                            execute!(terminal.backend_mut(), LeaveAlternateScreen, Show)?;

                            let run_result = app.run_selected();

                            // Print execution summary banner
                            println!();
                            println!("\x1b[1;36m━━━┫ Execution Finished ┣━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━\x1b[0m");
                            match run_result {
                                Ok(status) => {
                                    if status.success() {
                                        println!("\x1b[1;32m[✓] All selected tasks completed successfully.\x1b[0m");
                                    } else if let Some(code) = status.code() {
                                        println!("\x1b[1;31m[✗] Process exited with status code: {}\x1b[0m", code);
                                    } else {
                                        println!("\x1b[1;33m[!] Process was interrupted.\x1b[0m");
                                    }
                                }
                                Err(e) => {
                                    println!("\x1b[1;31m[✗] Execution error: {}\x1b[0m", e);
                                }
                            }
                            println!();
                            println!("\x1b[2mPress Enter to return to archctl...\x1b[0m");
                            let mut buf = String::new();
                            let _ = std::io::stdin().read_line(&mut buf);

                            // Re-enter alternate screen & re-enable raw mode
                            execute!(terminal.backend_mut(), EnterAlternateScreen, Hide)?;
                            enable_raw_mode()?;
                            terminal.clear()?;
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen, Show)?;
    terminal.show_cursor()?;
    Ok(())
}

fn ui(frame: &mut Frame, app: &mut App) {
    let area = frame.area();

    if app.output.is_some() {
        draw_output(frame, area, app);
    } else {
        draw_menu(frame, area, app);
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

    let enabled_count = app.steps.iter().filter(|s| s.enabled).count();
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
        Span::styled(&app.script_path, Style::default().fg(Color::White)),
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
    let header = Paragraph::new(vec![header_line, boot_summary_line(&app.boot_summary)]).block(
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

            let (cat_str, cat_style) = match s.step.category {
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
            };

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

    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Steps to Execute ")
                .border_style(Style::default().fg(Color::Blue)),
        )
        .highlight_style(
            Style::default()
                .bg(Color::Rgb(40, 44, 52))
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▶ ");

    frame.render_stateful_widget(list, chunks[1], &mut app.list_state);

    // 3. Footer
    let auto_style = match app.auto_confirm {
        Some(true) => Style::default()
            .fg(Color::Green)
            .add_modifier(Modifier::BOLD),
        Some(false) => Style::default()
            .fg(Color::Red)
            .add_modifier(Modifier::BOLD),
        None => Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
    };
    let auto_text = match app.auto_confirm {
        Some(true) => "YES (Auto-confirm)",
        Some(false) => "NO (Auto-decline)",
        None => "ASK (Interactive)",
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
            Span::styled(" Run  ", Style::default().fg(Color::Green)),
            Span::styled("J", Style::default().fg(Color::Red)),
            Span::raw(" Journal  "),
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

fn draw_output(frame: &mut Frame, area: Rect, app: &App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(3)])
        .split(area);

    let paragraph = Paragraph::new(app.output.clone().unwrap_or_default())
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Execution Plan / Output ")
                .border_style(Style::default().fg(Color::Cyan)),
        )
        .wrap(Wrap { trim: false })
        .scroll((app.scroll, 0));
    frame.render_widget(paragraph, chunks[0]);

    let footer = Paragraph::new(Line::from(vec![
        Span::styled(" [↑/↓/j/k] ", Style::default().fg(Color::Cyan)),
        Span::raw("Scroll  "),
        Span::styled("[PgUp/PgDn] ", Style::default().fg(Color::Cyan)),
        Span::raw("Page  "),
        Span::styled("[Home/End] ", Style::default().fg(Color::Cyan)),
        Span::raw("Top/Bottom  │  "),
        Span::styled(
            "[Esc/Enter/q] ",
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

/// The startup summary row: severity counts for the current boot.
fn boot_summary_line(summary: &Option<journal::Summary>) -> Line<'static> {
    let mut spans = vec![Span::styled(" Boot: ", Style::default().fg(Color::DarkGray))];

    let levels: Vec<&journal::Level> = match summary {
        Some(summary) => summary.levels.iter().filter(|l| l.count > 0).collect(),
        None => {
            spans.push(Span::styled(
                "journal unavailable",
                Style::default().fg(Color::DarkGray),
            ));
            return Line::from(spans);
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

    Line::from(spans)
}

/// Color a journald line by its syslog priority (lower number is more severe).
fn severity_style(priority: u8) -> Style {
    match priority {
        0 | 1 => Style::default()
            .fg(Color::Red)
            .add_modifier(Modifier::BOLD),
        2 => Style::default().fg(Color::Red),
        3 | 4 => Style::default().fg(Color::Yellow),
        5 => Style::default().fg(Color::Cyan),
        6 => Style::default().fg(Color::White),
        _ => Style::default().fg(Color::DarkGray),
    }
}

/// A filled section header, e.g. `━━ CRITICAL — 3 entries ━━━━━━━━━━━━━━━━━━━`.
fn section_header(label: &str, priority: u8, count: usize) -> Line<'static> {
    const WIDTH: usize = 72;
    let title = format!(" {} — {} entries ", label, count);
    let fill = "━".repeat(WIDTH.saturating_sub(title.chars().count() + 2));
    Line::from(Span::styled(
        format!("━━{title}{fill}"),
        severity_style(priority),
    ))
}

fn indent(text: &str) -> String {
    text.lines()
        .map(|line| format!("    {}", line))
        .collect::<Vec<_>>()
        .join("\n")
}
