//! `acs-app` — a lightweight terminal control app for the bus: live agents,
//! tasks and the message stream in one screen, driven by `ChangeWatcher` so
//! updates arrive as fast as the CLI would see them. Runs as the operator.

use crate::bus::{Bus, CreateTaskInput, ListTasksInput, SendInput};
use crate::error::Result;
use crate::error::BusError;
use crate::identity::Identity;
use crate::types::{AgentSummary, MessageSummary, OPERATOR_ID, TaskSummary};
use crate::watcher::{ChangeWatcher, ChangeWatcherOptions};
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph, Wrap};
use std::io::stdout;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

struct State {
    agents: Vec<AgentSummary>,
    tasks: Vec<TaskSummary>,
    messages: Vec<MessageSummary>,
}

fn load_state(bus: &Bus) -> Result<State> {
    let agents = bus
        .list_agents()?
        .into_iter()
        .map(|(a, _unread)| AgentSummary {
            id: a.id,
            stored_status: a.stored_status,
            wait_until_ms: a.wait_until_ms,
            last_seen_ms: a.last_seen_ms,
        })
        .collect();
    let mut tasks = bus.list_tasks(ListTasksInput {
        mine: None,
        states: None,
        include_closed: false,
        limit: Some(200),
    })?;
    tasks.sort_by_key(|t| t.id);
    let tasks = tasks
        .iter()
        .map(|t| TaskSummary {
            id: t.id,
            title: t.title.clone(),
            assignee: t.assignee.clone(),
            state: t.state.clone(),
            created_ms: t.created_ms,
            updated_ms: t.updated_ms,
        })
        .collect();
    // Latest 50 messages, chronological.
    let messages = bus
        .get_messages(None, Some(50), None, None)?
        .into_iter()
        .map(|m| MessageSummary {
            seq: m.seq,
            ts_ms: m.ts_ms,
            sender: m.sender,
            recipient: m.recipient,
            subject: m.subject,
            body: m.body,
        })
        .collect();
    Ok(State {
        agents,
        tasks,
        messages,
    })
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

fn ago(ms: Option<i64>, now: i64) -> String {
    let Some(ms) = ms else {
        return "never".into();
    };
    let s = ((now - ms) / 1000).max(0);
    if s < 10 {
        "now".into()
    } else if s < 60 {
        format!("{s}s")
    } else if s / 60 < 60 {
        format!("{}m", s / 60)
    } else if s / 3600 < 48 {
        format!("{}h", s / 3600)
    } else {
        format!("{}d", s / 86400)
    }
}

fn agent_state(a: &AgentSummary, now: i64) -> &'static str {
    if a.stored_status == "waiting" {
        return match a.wait_until_ms {
            Some(until) if until >= now => "waiting",
            _ => "offline",
        };
    }
    if a.stored_status == "offline" {
        return "offline";
    }
    match a.last_seen_ms {
        Some(seen) if now - seen <= crate::types::STALE_AGENT_MS => "idle",
        _ => "offline",
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Pane {
    Agents,
    Tasks,
    Messages,
}

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Navigate,
    ComposeTo,
    ComposeSubject,
    ComposeBody,
    TaskTitle,
    ConfirmCancel,
}

struct App {
    bus: Bus,
    watcher: ChangeWatcher,
    seq: i64,
    state: State,
    pane: Pane,
    mode: Mode,
    sel: [usize; 3],
    // compose buffers
    to: String,
    subject: String,
    body: String,
    task_title: String,
    flash: String,
    flash_until: Instant,
    dirty: bool,
    quit: bool,
}

impl App {
    fn refresh(&mut self) -> Result<()> {
        self.state = load_state(&self.bus)?;
        for sel in self.sel.iter_mut() {
            *sel = (*sel).min(199);
        }
        self.dirty = true;
        Ok(())
    }

    fn flash(&mut self, text: impl Into<String>) {
        self.flash = text.into();
        self.flash_until = Instant::now() + Duration::from_secs(4);
        self.dirty = true;
    }

    fn identity(&self) -> Result<Identity> {
        self.bus.identify(Some(OPERATOR_ID))
    }

    fn send(&mut self) -> Result<()> {
        let ident = self.identity()?;
        self.bus.send(
            &ident,
            SendInput {
                to: self.to.clone(),
                subject: Some(self.subject.clone()).filter(|s| !s.is_empty()),
                body: self.body.clone(),
                msg_type: None,
                thread: None,
                task_id: None,
                refs: None,
                requires_ack: false,
            },
        )?;
        self.flash(format!("sent to {}", self.to));
        Ok(())
    }

    fn add_task(&mut self) -> Result<()> {
        let ident = self.identity()?;
        self.bus.create_task(
            &ident,
            CreateTaskInput {
                title: self.task_title.clone(),
                ..Default::default()
            },
        )?;
        self.flash(format!("task added: {}", self.task_title));
        Ok(())
    }

    fn cancel_selected(&mut self) -> Result<()> {
        if let Some(task) = self.state.tasks.get(self.sel[1]) {
            let ident = self.identity()?;
            self.bus.cancel_task(&ident, task.id, Some("cancelled from acs-app"))?;
            self.flash(format!("task #{} cancelled", task.id));
        }
        Ok(())
    }
}

fn render(f: &mut ratatui::Frame, app: &App) {
    let area = f.area();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(10),
            Constraint::Length(3),
            Constraint::Length(1),
        ])
        .split(area);
    let top = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(28),
            Constraint::Percentage(36),
            Constraint::Percentage(36),
        ])
        .split(chunks[0]);

    render_agents(f, app, top[0]);
    render_tasks(f, app, top[1]);
    render_messages(f, app, top[2]);
    render_footer(f, app, chunks[1], chunks[2]);
}

fn focused(app: &App, pane: Pane) -> Style {
    if app.pane == pane {
        Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)
    } else {
        Style::default()
    }
}

fn render_agents(f: &mut ratatui::Frame, app: &App, area: Rect) {
    let now = now_ms();
    let items: Vec<ListItem> = app
        .state
        .agents
        .iter()
        .map(|a| {
            let state = agent_state(a, now);
            let style = match state {
                "waiting" => Style::default().fg(Color::Yellow),
                "offline" => Style::default().fg(Color::DarkGray),
                _ => Style::default().fg(Color::Green),
            };
            ListItem::new(Line::from(vec![
                Span::raw(format!("{:<14}", a.id)),
                Span::styled(format!("{state:<8}"), style),
                Span::styled(
                    ago(a.last_seen_ms, now),
                    Style::default().fg(Color::DarkGray),
                ),
            ]))
        })
        .collect();
    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Agents ")
                .border_style(focused(app, Pane::Agents)),
        )
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED));
    let mut list_state = ratatui::widgets::ListState::default();
    list_state.select(if app.state.agents.is_empty() {
        None
    } else {
        Some(app.sel[0].min(app.state.agents.len() - 1))
    });
    f.render_stateful_widget(list, area, &mut list_state);
}

fn render_tasks(f: &mut ratatui::Frame, app: &App, area: Rect) {
    let items: Vec<ListItem> = app
        .state
        .tasks
        .iter()
        .map(|t| {
            let state = serde_json::to_value(&t.state)
                .ok()
                .and_then(|v| v.as_str().map(str::to_string))
                .unwrap_or_default()
                .replace('_', " ");
            ListItem::new(Line::from(vec![
                Span::raw(format!("#{:<4}", t.id)),
                Span::raw(format!("{:<22.22}", t.title)),
                Span::styled(
                    format!("{:<14.14}", t.assignee.clone().unwrap_or_else(|| "-".into())),
                    Style::default().fg(Color::DarkGray),
                ),
                Span::raw(state),
            ]))
        })
        .collect();
    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Open tasks ")
                .border_style(focused(app, Pane::Tasks)),
        )
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED));
    let mut list_state = ratatui::widgets::ListState::default();
    list_state.select(if app.state.tasks.is_empty() {
        None
    } else {
        Some(app.sel[1].min(app.state.tasks.len() - 1))
    });
    f.render_stateful_widget(list, area, &mut list_state);
}

fn render_messages(f: &mut ratatui::Frame, app: &App, area: Rect) {
    let now = now_ms();
    let items: Vec<ListItem> = app
        .state
        .messages
        .iter()
        .map(|m| {
            let line = m
                .subject
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
            let line = if line.is_empty() {
                m.body.split_whitespace().collect::<Vec<_>>().join(" ")
            } else {
                line
            };
            ListItem::new(Line::from(vec![
                Span::raw(format!("{:<12.12}", m.sender)),
                Span::raw("→"),
                Span::styled(
                    format!("{:<12.12}", m.recipient.clone().unwrap_or_else(|| "*".into())),
                    Style::default().fg(Color::DarkGray),
                ),
                Span::raw(format!(" {:<40.40}", line)),
                Span::styled(
                    ago(Some(m.ts_ms), now),
                    Style::default().fg(Color::DarkGray),
                ),
            ]))
        })
        .collect();
    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Messages ")
                .border_style(focused(app, Pane::Messages)),
        )
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED));
    let mut list_state = ratatui::widgets::ListState::default();
    list_state.select(if app.state.messages.is_empty() {
        None
    } else {
        Some(app.sel[2].min(app.state.messages.len() - 1))
    });
    f.render_stateful_widget(list, area, &mut list_state);
}

fn render_footer(f: &mut ratatui::Frame, app: &App, input_area: Rect, status_area: Rect) {
    // Input box: compose / task-title entry, or a hint in Navigate mode.
    let (title, text) = match app.mode {
        Mode::ComposeTo => (" Send: to ", app.to.as_str()),
        Mode::ComposeSubject => (" Send: subject ", app.subject.as_str()),
        Mode::ComposeBody => (" Send: body ", app.body.as_str()),
        Mode::TaskTitle => (" New task title ", app.task_title.as_str()),
        Mode::ConfirmCancel => (" Confirm ", "cancel selected task? [y/N]"),
        Mode::Navigate => (
            " ",
            "tab: pane · m: send · t: new task · x: cancel task · r: refresh · q: quit",
        ),
    };
    let editing = app.mode != Mode::Navigate;
    let input = Paragraph::new(text.to_string()).block(
        Block::default()
            .borders(Borders::ALL)
            .title(title)
            .border_style(if editing {
                Style::default().fg(Color::Yellow)
            } else {
                Style::default().fg(Color::DarkGray)
            }),
    );
    f.render_widget(input, input_area);

    let flash = if Instant::now() < app.flash_until {
        app.flash.clone()
    } else {
        String::new()
    };
    let status = Paragraph::new(format!(
        " {}  ·  seq {}  ·  {} agents · {} open tasks{}",
        app.bus.db_path.display(),
        app.seq,
        app.state.agents.len(),
        app.state.tasks.len(),
        if flash.is_empty() {
            String::new()
        } else {
            format!("  ·  {flash}")
        },
    ))
    .style(Style::default().fg(Color::DarkGray))
    .wrap(Wrap { trim: true });
    f.render_widget(status, status_area);
}

fn handle_key(app: &mut App, key: KeyEvent) -> Result<()> {
    // Ctrl-C always quits.
    if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
        app.quit = true;
        return Ok(());
    }
    match app.mode {
        Mode::Navigate => match key.code {
            KeyCode::Char('q') | KeyCode::Esc => app.quit = true,
            KeyCode::Tab | KeyCode::Right => {
                app.pane = match app.pane {
                    Pane::Agents => Pane::Tasks,
                    Pane::Tasks => Pane::Messages,
                    Pane::Messages => Pane::Agents,
                }
            }
            KeyCode::BackTab | KeyCode::Left => {
                app.pane = match app.pane {
                    Pane::Agents => Pane::Messages,
                    Pane::Tasks => Pane::Agents,
                    Pane::Messages => Pane::Tasks,
                }
            }
            KeyCode::Up | KeyCode::Char('k') => {
                let i = match app.pane {
                    Pane::Agents => 0,
                    Pane::Tasks => 1,
                    Pane::Messages => 2,
                };
                app.sel[i] = app.sel[i].saturating_sub(1);
            }
            KeyCode::Down | KeyCode::Char('j') => {
                let i = match app.pane {
                    Pane::Agents => 0,
                    Pane::Tasks => 1,
                    Pane::Messages => 2,
                };
                let len = match i {
                    0 => app.state.agents.len(),
                    1 => app.state.tasks.len(),
                    _ => app.state.messages.len(),
                };
                if len > 0 {
                    app.sel[i] = (app.sel[i] + 1).min(len - 1);
                }
            }
            KeyCode::Char('m') => {
                app.mode = Mode::ComposeTo;
                app.to.clear();
                app.subject.clear();
                app.body.clear();
            }
            KeyCode::Char('t') => {
                app.mode = Mode::TaskTitle;
                app.task_title.clear();
            }
            KeyCode::Char('x') => {
                if app.pane == Pane::Tasks && !app.state.tasks.is_empty() {
                    app.mode = Mode::ConfirmCancel;
                }
            }
            KeyCode::Char('r') => app.refresh()?,
            _ => {}
        },
        Mode::ConfirmCancel => match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') => {
                app.mode = Mode::Navigate;
                app.cancel_selected()?;
            }
            _ => app.mode = Mode::Navigate,
        },
        Mode::ComposeTo | Mode::ComposeSubject | Mode::ComposeBody | Mode::TaskTitle => {
            let buf = match app.mode {
                Mode::ComposeTo => &mut app.to,
                Mode::ComposeSubject => &mut app.subject,
                Mode::ComposeBody => &mut app.body,
                Mode::TaskTitle => &mut app.task_title,
                _ => unreachable!(),
            };
            match key.code {
                KeyCode::Esc => app.mode = Mode::Navigate,
                KeyCode::Enter => {
                    let done = match app.mode {
                        Mode::ComposeTo => {
                            if !app.to.is_empty() {
                                app.mode = Mode::ComposeSubject;
                            }
                            false
                        }
                        Mode::ComposeSubject => {
                            app.mode = Mode::ComposeBody;
                            false
                        }
                        Mode::ComposeBody => {
                            app.mode = Mode::Navigate;
                            if !app.body.is_empty() {
                                if let Err(e) = app.send() {
                                    app.flash(format!("send failed: {}", e.message));
                                }
                            }
                            true
                        }
                        Mode::TaskTitle => {
                            app.mode = Mode::Navigate;
                            if !app.task_title.is_empty() {
                                if let Err(e) = app.add_task() {
                                    app.flash(format!("task failed: {}", e.message));
                                }
                            }
                            true
                        }
                        _ => true,
                    };
                    let _ = done;
                }
                KeyCode::Char(c) => buf.push(c),
                KeyCode::Backspace => {
                    buf.pop();
                }
                _ => {}
            }
        }
    }
    app.dirty = true;
    Ok(())
}

/// Run the TUI. `stop` lets a test drive one loop iteration and bail.
pub fn run(db_path: &std::path::Path, stop: Option<Arc<AtomicBool>>) -> Result<i32> {
    let bus = Bus::open(Some(db_path))?;
    let watcher = ChangeWatcher::new(
        &bus.db_path,
        ChangeWatcherOptions {
            min_poll_ms: 25,
            max_poll_ms: 500,
            fs_watch: true,
        },
    )?;
    let seq = bus.latest_seq()?;
    let mut app = App {
        seq,
        state: load_state(&bus)?,
        bus,
        watcher,
        pane: Pane::Agents,
        mode: Mode::Navigate,
        sel: [0, 0, 0],
        to: String::new(),
        subject: String::new(),
        body: String::new(),
        task_title: String::new(),
        flash: String::new(),
        flash_until: Instant::now(),
        dirty: true,
        quit: false,
    };

    enable_raw_mode().map_err(|e| BusError::invalid(format!("terminal: {e}")))?;
    let mut stdout = stdout();
    execute!(stdout, EnterAlternateScreen).map_err(|e| BusError::invalid(format!("terminal: {e}")))?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal =
        Terminal::new(backend).map_err(|e| BusError::invalid(format!("terminal: {e}")))?;

    let result = (|| -> Result<()> {
        loop {
            if app.quit || stop.as_ref().is_some_and(|s| s.load(Ordering::SeqCst)) {
                return Ok(());
            }
            // Drain every queued key before blocking on the watcher, so typing
            // never waits for a poll round-trip.
            while event::poll(Duration::ZERO)
                .map_err(|e| BusError::invalid(format!("terminal: {e}")))?
            {
                match event::read().map_err(|e| BusError::invalid(format!("terminal: {e}")))? {
                    Event::Key(key) => handle_key(&mut app, key)?,
                    Event::Resize(..) => app.dirty = true,
                    _ => {}
                }
            }
            let stop_flag = AtomicBool::new(false);
            let next = app
                .watcher
                .next(app.seq, Duration::from_millis(25), &stop_flag)?;
            if next > app.seq {
                app.seq = next;
                app.refresh()?;
            }
            if app.dirty {
                terminal
                    .draw(|f| render(f, &app))
                    .map_err(|e| BusError::invalid(format!("terminal: {e}")))?;
                app.dirty = false;
            }
        }
    })();

    disable_raw_mode().ok();
    let _ = execute!(terminal.backend_mut(), LeaveAlternateScreen);
    result?;
    Ok(0)
}

pub fn main() -> i32 {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    if argv.iter().any(|a| a == "--help" || a == "-h") {
        eprintln!("acs-app — terminal control app for the agent bus\n\nusage: acs-app [--db PATH]\n\nkeys: tab pane · m send · t new task · x cancel · r refresh · q quit");
        return 0;
    }
    let db_flag = argv
        .iter()
        .position(|a| a == "--db")
        .and_then(|i| argv.get(i + 1).cloned())
        .or_else(|| {
            argv.iter()
                .find(|a| a.starts_with("--db="))
                .map(|a| a[5..].to_string())
        });
    let db_path = crate::db::resolve_db_path_with(db_flag.as_deref(), |name| {
        std::env::var(name).ok()
    });
    match run(&db_path, None) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("acs-app: {}", e.message);
            1
        }
    }
}
