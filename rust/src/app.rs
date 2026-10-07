//! `acs-app` — a lightweight terminal control app for the bus: live agents,
//! tasks and the message stream in one screen, driven by `ChangeWatcher` so
//! updates arrive as fast as the CLI would see them. Runs as the operator.

use crate::bus::{Bus, CreateTaskInput, ListTasksInput, SendInput};
use crate::error::BusError;
use crate::error::Result;
use crate::identity::Identity;
use crate::types::{MessageSummary, TaskSummary, OPERATOR_ID};
use crate::watcher::{ChangeWatcher, ChangeWatcherOptions};
use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyModifiers,
    MouseButton, MouseEventKind,
};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, Borders, Cell as TableCell, Clear, List, ListItem, ListState, Paragraph, Row, Table,
    TableState, Wrap,
};
use ratatui::Terminal;
use std::cell::Cell;
use std::io::stdout;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

struct AgentRow {
    id: String,
    role: String,
    stored_status: String,
    wait_until_ms: Option<i64>,
    last_seen_ms: Option<i64>,
    runtime_note: Option<String>,
}

struct State {
    agents: Vec<AgentRow>,
    tasks: Vec<TaskSummary>,
    messages: Vec<MessageSummary>,
}

fn load_state(bus: &Bus) -> Result<State> {
    let agents = bus
        .list_agents()?
        .into_iter()
        .filter(|(a, _)| a.id != OPERATOR_ID)
        .map(|(a, _unread)| {
            let runtime_note = crate::supervisor::runtime_note(&bus.home, &a.id);
            AgentRow {
                id: a.id,
                role: a.role,
                stored_status: a.stored_status,
                wait_until_ms: a.wait_until_ms,
                last_seen_ms: a.last_seen_ms,
                runtime_note,
            }
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
        format!("{s}s ago")
    } else if s / 60 < 60 {
        format!("{}m ago", s / 60)
    } else if s / 3600 < 48 {
        format!("{}h ago", s / 3600)
    } else {
        format!("{}d ago", s / 86400)
    }
}

fn agent_state(a: &AgentRow, now: i64) -> &'static str {
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
        Some(seen) if now - seen <= crate::types::STALE_AGENT_MS => "online",
        _ => "offline",
    }
}

fn state_label(state: &str) -> &'static str {
    match state {
        "waiting" => "waiting for work",
        "online" => "online",
        _ => "offline",
    }
}

const PANES: [Pane; 3] = [Pane::Agents, Pane::Tasks, Pane::Messages];

#[derive(Clone, Copy, PartialEq)]
enum Pane {
    Agents,
    Tasks,
    Messages,
}

impl Pane {
    fn index(self) -> usize {
        match self {
            Pane::Agents => 0,
            Pane::Tasks => 1,
            Pane::Messages => 2,
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Navigate,
    PickRecipient,
    ComposeSubject,
    ComposeBody,
    TaskTitle,
    AgentName,
    AgentRole,
    ConfirmCancel,
    Detail,
    Help,
    Setup,
}

/// First-run team presets. `role` drives task routing — a task created with
/// `--role worker-hard` is only claimable by the hard worker. `manager`
/// authority is reserved for the lead so it can coordinate.
struct Preset {
    id: &'static str,
    role: &'static str,
    authority: &'static str,
    blurb: &'static str,
    charter: &'static str,
}

const PLANNER_CHARTER: &str = "\
You are the planner on this agent bus. Your job is to turn goals into tasks.

- When the operator or lead sends you work, break it into concrete tasks.
- Create each with `qagent task add <title> --brief <details> --role <role>`.
- Route deep or complex work with --role worker-hard, small quick work with
  --role worker-easy, and coordination with --role orchestrator.
- Never implement yourself — plan, then hand off. Report back to the
  operator with `qagent send operator <subject> <body>`.";

const LEAD_CHARTER: &str = "\
You are the lead (orchestrator) on this agent bus. Your job is to keep work moving.

- Watch the task list (`qagent task list`) and your inbox (`qagent inbox`,
  or block on `qagent wait`).
- Route unassigned tasks to the right worker: `qagent task add` again with
  --role worker-hard / worker-easy or --to <agent>.
- When a worker submits, review it: `qagent task review <id> --accept` or
  --revise --feedback <what to fix>.
- If a request needs planning, hand it to the planner. Keep the operator
  posted with `qagent send operator <subject> <body>`.";

const WORKER_HARD_CHARTER: &str = "\
You are the heavy worker on this agent bus — the deep, complex tasks are yours.

- Claim work with `qagent task claim` (tasks with role worker-hard match you).
- Post progress as you go: `qagent task note <id> <what you found or did>`.
- Finish with `qagent task submit <id> --summary <what changed>`.
- Blocked? Message the lead (`qagent send lead <subject> <details>`) and
  release the task so it isn't held: `qagent task release` via the lead.";

const WORKER_EASY_CHARTER: &str = "\
You are the fast worker on this agent bus — the small, quick tasks are yours.

- Claim work with `qagent task claim` (tasks with role worker-easy match you).
- Finish fast: `qagent task submit <id> --summary <what changed>`.
- If a task turns out bigger than it looked, message the lead
  (`qagent send lead <subject> <details>`) to hand it up instead of stalling.";

const PRESETS: &[Preset] = &[
    Preset {
        id: "planner",
        role: "planner",
        authority: "worker",
        blurb: "turns goals into concrete tasks",
        charter: PLANNER_CHARTER,
    },
    Preset {
        id: "lead",
        role: "orchestrator",
        authority: "manager",
        blurb: "assigns tasks to workers, reviews results",
        charter: LEAD_CHARTER,
    },
    Preset {
        id: "worker-hard",
        role: "worker-hard",
        authority: "worker",
        blurb: "takes the big, complex tasks",
        charter: WORKER_HARD_CHARTER,
    },
    Preset {
        id: "worker-easy",
        role: "worker-easy",
        authority: "worker",
        blurb: "takes the small, quick tasks",
        charter: WORKER_EASY_CHARTER,
    },
];

struct App {
    bus: Bus,
    watcher: ChangeWatcher,
    seq: i64,
    state: State,
    pane: Pane,
    mode: Mode,
    sel: [usize; 3],
    to_pick: usize,
    // compose buffers
    to: String,
    subject: String,
    body: String,
    task_title: String,
    agent_name: String,
    agent_role: String,
    detail_title: String,
    detail: String,
    setup_sel: usize,
    setup_on: Vec<bool>,
    flash: String,
    flash_until: Instant,
    areas: Cell<[Rect; 3]>,
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

    fn selected_task(&self) -> Option<&TaskSummary> {
        self.state
            .tasks
            .get(self.sel[1].min(self.state.tasks.len().saturating_sub(1)))
    }

    fn send(&mut self) -> Result<()> {
        let ident = self.identity()?;
        let to = self.to.clone();
        self.bus.send(
            &ident,
            SendInput {
                to: to.clone(),
                subject: Some(self.subject.clone()).filter(|s| !s.is_empty()),
                body: self.body.clone(),
                msg_type: None,
                thread: None,
                task_id: None,
                refs: None,
                requires_ack: false,
            },
        )?;
        let target = if to == "*" { "everyone" } else { &to };
        self.flash(format!("Sent to {target}"));
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
        self.flash(format!("Task created: {}", self.task_title));
        Ok(())
    }

    fn add_agent(&mut self) -> Result<()> {
        let ident = self.identity()?;
        let (agent, token_path) = self.bus.add_agent(
            &ident,
            &self.agent_name,
            Some(self.agent_role.clone())
                .filter(|r| !r.is_empty())
                .as_deref(),
            None,
            None,
            None,
            None,
        )?;
        self.flash(format!(
            "Agent '{}' added — its token is at {}",
            agent.id,
            token_path.display()
        ));
        Ok(())
    }

    fn cancel_selected(&mut self) -> Result<()> {
        if let Some(id) = self.selected_task().map(|t| t.id) {
            let ident = self.identity()?;
            self.bus
                .cancel_task(&ident, id, Some("cancelled from acs"))?;
            self.flash(format!("Task #{id} cancelled"));
        }
        Ok(())
    }

    fn apply_presets(&mut self) {
        let Ok(ident) = self.identity() else {
            self.flash("Couldn't sign in as operator".to_string());
            return;
        };
        let mut made: Vec<&str> = Vec::new();
        let mut skipped: Vec<&str> = Vec::new();
        for (i, p) in PRESETS.iter().enumerate() {
            if !self.setup_on.get(i).copied().unwrap_or(false) {
                continue;
            }
            match self.bus.add_agent(
                &ident,
                p.id,
                Some(p.role),
                None,
                None,
                None,
                Some(p.authority),
            ) {
                Ok((agent, _)) => {
                    let dir = self.bus.home.join("charters");
                    let _ = std::fs::create_dir_all(&dir);
                    let _ = std::fs::write(dir.join(format!("{}.md", p.id)), p.charter);
                    let _ = self.bus.send(
                        &ident,
                        SendInput {
                            to: agent.id.clone(),
                            subject: Some(format!("Your role on the bus: {}", p.id)),
                            body: p.charter.to_string(),
                            msg_type: None,
                            thread: None,
                            task_id: None,
                            refs: None,
                            requires_ack: false,
                        },
                    );
                    made.push(p.id);
                }
                Err(_) => skipped.push(p.id),
            }
        }
        let _ = self.refresh();
        if made.is_empty() && skipped.is_empty() {
            self.flash("Nothing selected — pick with space, or press s to skip".to_string());
            return;
        }
        let mut msg = if made.is_empty() {
            String::new()
        } else {
            format!("Team ready: {}.", made.join(", "))
        };
        if !skipped.is_empty() {
            msg.push_str(&format!(" Already existed: {}.", skipped.join(", ")));
        }
        if !made.is_empty() {
            msg.push_str(" Their first message explains each role.");
        }
        self.flash(msg);
        self.mode = Mode::Navigate;
    }

    fn move_selection(&mut self, delta: i64) {
        let i = self.pane.index();
        let len = match i {
            0 => self.state.agents.len(),
            1 => self.state.tasks.len(),
            _ => self.state.messages.len(),
        };
        if len == 0 {
            return;
        }
        let next = app_clamp(self.sel[i] as i64 + delta, 0, len as i64 - 1);
        self.sel[i] = next as usize;
    }
}

fn app_clamp(v: i64, lo: i64, hi: i64) -> i64 {
    v.max(lo).min(hi)
}

fn render(f: &mut ratatui::Frame, app: &App) {
    let area = f.area();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(8),
            Constraint::Length(3),
            Constraint::Length(1),
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
    app.areas.set([top[0], top[1], top[2]]);

    render_agents(f, app, top[0]);
    render_tasks(f, app, top[1]);
    render_messages(f, app, top[2]);
    render_input(f, app, chunks[1]);
    render_hints(f, app, chunks[2]);
    render_status(f, app, chunks[3]);

    match app.mode {
        Mode::PickRecipient => render_recipient_popup(f, app),
        Mode::ConfirmCancel => render_cancel_popup(f, app),
        Mode::Help => render_help(f),
        Mode::Detail => render_detail(f, app),
        Mode::Setup => render_setup(f, app),
        _ => {}
    }
}

fn render_setup(f: &mut ratatui::Frame, app: &App) {
    let area = centered_rect(64, 17, f.area());
    f.render_widget(Clear, area);
    let mut lines = vec![
        Line::from("Pick the agents you want. Each one is created on the bus"),
        Line::from("with a starter prompt waiting in its inbox."),
        Line::from(""),
    ];
    for (i, p) in PRESETS.iter().enumerate() {
        let checked = app.setup_on.get(i).copied().unwrap_or(false);
        let marker = if checked { "[x]" } else { "[ ]" };
        let style = if i == app.setup_sel {
            Style::default().add_modifier(Modifier::REVERSED)
        } else {
            Style::default()
        };
        lines.push(Line::from(vec![
            Span::styled(format!(" {marker} {:<12}", p.id), style),
            Span::styled(
                format!(" {}", p.blurb),
                Style::default().fg(Color::DarkGray),
            ),
        ]));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "  space toggle · ↑↓ move · enter create team · s skip",
        Style::default().fg(Color::DarkGray),
    )));
    let p = Paragraph::new(lines).block(
        Block::default()
            .borders(Borders::ALL)
            .title(" Set up your team ")
            .border_style(Style::default().fg(Color::Cyan)),
    );
    f.render_widget(p, area);
}

fn focused(app: &App, pane: Pane) -> Style {
    if app.pane == pane && app.mode == Mode::Navigate {
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default()
    }
}

fn centered_rect(percent_x: u16, lines: u16, area: Rect) -> Rect {
    let width = (area.width * percent_x / 100).max(20).min(area.width);
    let height = lines.min(area.height);
    Rect {
        x: area.x + (area.width - width) / 2,
        y: area.y + (area.height - height) / 2,
        width,
        height,
    }
}

fn empty_note(f: &mut ratatui::Frame, area: Rect, text: &str) {
    let inner = Rect {
        x: area.x + 1,
        y: area.y + 1,
        width: area.width.saturating_sub(2),
        height: area.height.saturating_sub(2),
    };
    let note = Paragraph::new(text)
        .style(Style::default().fg(Color::DarkGray))
        .wrap(Wrap { trim: true });
    f.render_widget(note, inner);
}

fn render_agents(f: &mut ratatui::Frame, app: &App, area: Rect) {
    let now = now_ms();
    let items: Vec<ListItem> = app
        .state
        .agents
        .iter()
        .map(|a| {
            let state = agent_state(a, now);
            let (dot, style) = match state {
                "waiting" => ("◉", Style::default().fg(Color::Yellow)),
                "online" => ("●", Style::default().fg(Color::Green)),
                _ => ("○", Style::default().fg(Color::DarkGray)),
            };
            let mut spans = vec![
                Span::styled(dot, style),
                Span::raw(format!(" {:<11.11}", a.id)),
                Span::styled(
                    format!("{:<13.13}", a.role),
                    Style::default().fg(Color::Magenta),
                ),
                Span::styled(
                    ago(a.last_seen_ms, now),
                    Style::default().fg(Color::DarkGray),
                ),
            ];
            if a.runtime_note.is_some() {
                spans.push(Span::styled(" ! failure", Style::default().fg(Color::Red)));
            }
            ListItem::new(Line::from(spans))
        })
        .collect();
    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(format!(" Agents ({}) ", app.state.agents.len()))
                .border_style(focused(app, Pane::Agents)),
        )
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED));
    let mut list_state = ListState::default();
    list_state.select(if app.state.agents.is_empty() {
        None
    } else {
        Some(app.sel[0].min(app.state.agents.len() - 1))
    });
    f.render_stateful_widget(list, area, &mut list_state);
    if app.state.agents.is_empty() {
        empty_note(
            f,
            area,
            "No agents yet.\n\nPress a to add an agent.\nTry a safe sample: aos demo\nSet up real tools: aos setup\nHelp: github.com/anon5376/agent-communication-system",
        );
    }
}

fn render_tasks(f: &mut ratatui::Frame, app: &App, area: Rect) {
    let items: Vec<Row> = app
        .state
        .tasks
        .iter()
        .map(|t| {
            let state = serde_json::to_value(&t.state)
                .ok()
                .and_then(|v| v.as_str().map(str::to_string))
                .unwrap_or_default()
                .replace('_', " ");
            Row::new(vec![
                TableCell::from(format!("#{}", t.id)),
                TableCell::from(t.title.clone()),
                TableCell::from(t.assignee.clone().unwrap_or_else(|| "anyone".into()))
                    .style(Style::default().fg(Color::DarkGray)),
                TableCell::from(state),
            ])
        })
        .collect();
    let width = area.width.saturating_sub(2);
    let list = Table::new(
        items,
        [
            Constraint::Length(7.min(width / 8)),
            Constraint::Fill(1),
            Constraint::Length(12.min(width / 4)),
            Constraint::Length(17.min(width / 4)),
        ],
    )
    .block(
        Block::default()
            .borders(Borders::ALL)
            .title(format!(" Open tasks ({}) ", app.state.tasks.len()))
            .border_style(focused(app, Pane::Tasks)),
    )
    .row_highlight_style(Style::default().add_modifier(Modifier::REVERSED));
    let mut list_state = TableState::default();
    list_state.select(if app.state.tasks.is_empty() {
        None
    } else {
        Some(app.sel[1].min(app.state.tasks.len() - 1))
    });
    f.render_stateful_widget(list, area, &mut list_state);
    if app.state.tasks.is_empty() {
        empty_note(
            f,
            area,
            "No open tasks.\n\nPress t to create one and an agent can pick it up.",
        );
    }
}

fn render_messages(f: &mut ratatui::Frame, app: &App, area: Rect) {
    let now = now_ms();
    let items: Vec<ListItem> = app
        .state
        .messages
        .iter()
        .map(|m| {
            let line = m.subject.split_whitespace().collect::<Vec<_>>().join(" ");
            let line = if line.is_empty() {
                m.body.split_whitespace().collect::<Vec<_>>().join(" ")
            } else {
                line
            };
            ListItem::new(Line::from(vec![
                Span::raw(format!("{:<10.10}", m.sender)),
                Span::styled("→", Style::default().fg(Color::DarkGray)),
                Span::styled(
                    format!(
                        "{:<10.10}",
                        m.recipient.clone().unwrap_or_else(|| "everyone".into())
                    ),
                    Style::default().fg(Color::DarkGray),
                ),
                Span::raw(format!(" {:<34.34}", line)),
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
    let mut list_state = ListState::default();
    list_state.select(if app.state.messages.is_empty() {
        None
    } else {
        Some(app.sel[2].min(app.state.messages.len() - 1))
    });
    f.render_stateful_widget(list, area, &mut list_state);
    if app.state.messages.is_empty() {
        empty_note(f, area, "No messages yet.\n\nPress m to send one.");
    }
}

fn render_input(f: &mut ratatui::Frame, app: &App, area: Rect) {
    let (title, text) = match app.mode {
        Mode::PickRecipient => (
            " Send a message ".to_string(),
            "choose who gets it…".to_string(),
        ),
        Mode::ComposeSubject => (
            format!(" Send to {} — subject (optional) ", app.to),
            app.subject.clone(),
        ),
        Mode::ComposeBody => (
            format!(" Send to {} — your message ", app.to),
            app.body.clone(),
        ),
        Mode::TaskTitle => (
            " New task — what needs doing? ".to_string(),
            app.task_title.clone(),
        ),
        Mode::AgentName => (
            " Add an agent — a name for it (letters, digits, -) ".to_string(),
            app.agent_name.clone(),
        ),
        Mode::AgentRole => (
            format!(
                " Agent '{}' — its role, e.g. planner (optional) ",
                app.agent_name
            ),
            app.agent_role.clone(),
        ),
        Mode::ConfirmCancel | Mode::Detail | Mode::Help | Mode::Setup | Mode::Navigate => {
            (" ".to_string(), String::new())
        }
    };
    let editing = matches!(
        app.mode,
        Mode::ComposeSubject
            | Mode::ComposeBody
            | Mode::TaskTitle
            | Mode::AgentName
            | Mode::AgentRole
    );
    let input = Paragraph::new(text).block(
        Block::default()
            .borders(Borders::ALL)
            .title(title)
            .border_style(if editing || app.mode == Mode::PickRecipient {
                Style::default().fg(Color::Yellow)
            } else {
                Style::default().fg(Color::DarkGray)
            }),
    );
    f.render_widget(input, area);
    if editing {
        // Place the cursor at the end of the input text.
        let x = area.x + 1 + (app.input_len() as u16).min(area.width.saturating_sub(2));
        f.set_cursor_position((x.min(area.x + area.width - 2), area.y + 1));
    }
}

impl App {
    fn input_len(&self) -> usize {
        match self.mode {
            Mode::ComposeSubject => self.subject.len(),
            Mode::ComposeBody => self.body.len(),
            Mode::TaskTitle => self.task_title.len(),
            Mode::AgentName => self.agent_name.len(),
            Mode::AgentRole => self.agent_role.len(),
            _ => 0,
        }
    }
}

fn render_hints(f: &mut ratatui::Frame, app: &App, area: Rect) {
    let hint = match app.mode {
        Mode::Navigate => match app.pane {
            Pane::Agents => {
                "enter view · m send · a add agent · t new task · ↑↓/wheel · tab/click · ? help · q quit"
            }
            Pane::Tasks => {
                "enter view · x cancel · t new task · m send · ↑↓/wheel move · ? help · q quit"
            }
            Pane::Messages => {
                "enter read · m send · t new task · ↑↓/wheel move · ? help · q quit"
            }
        },
        Mode::PickRecipient => "↑↓ choose who gets it · enter next · esc cancel",
        Mode::ComposeSubject => "enter next (or leave empty) · esc cancel",
        Mode::ComposeBody => "enter send · esc cancel",
        Mode::TaskTitle => "enter create · esc cancel",
        Mode::AgentName => "enter next · esc cancel",
        Mode::AgentRole => "enter add agent · esc cancel",
        Mode::ConfirmCancel => "y cancel the task · n/esc keep it",
        Mode::Detail => "esc/enter close",
        Mode::Help => "press any key to close",
        Mode::Setup => "space toggle · ↑↓ move · enter create team · s skip",
    };
    let p = Paragraph::new(format!(" {hint}")).style(Style::default().fg(Color::DarkGray));
    f.render_widget(p, area);
}

fn render_status(f: &mut ratatui::Frame, app: &App, area: Rect) {
    let flash = if Instant::now() < app.flash_until {
        app.flash.clone()
    } else {
        String::new()
    };
    let status = Paragraph::new(format!(
        " {}  ·  {} agents · {} open tasks{}",
        app.bus.db_path.display(),
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
    f.render_widget(status, area);
}

fn render_recipient_popup(f: &mut ratatui::Frame, app: &App) {
    let count = app.state.agents.len() + 1;
    let area = centered_rect(50, (count as u16) + 3, f.area());
    f.render_widget(Clear, area);
    let items: Vec<ListItem> = std::iter::once(ListItem::new(Line::from(Span::styled(
        "Everyone",
        Style::default().fg(Color::Cyan),
    ))))
    .chain(app.state.agents.iter().map(|a| {
        ListItem::new(Line::from(vec![
            Span::raw(format!(" {:<16.16}", a.id)),
            Span::styled(a.role.clone(), Style::default().fg(Color::DarkGray)),
        ]))
    }))
    .collect();
    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Send to ")
                .border_style(Style::default().fg(Color::Yellow)),
        )
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED));
    let mut state = ListState::default();
    state.select(Some(app.to_pick.min(count - 1)));
    f.render_stateful_widget(list, area, &mut state);
}

fn render_cancel_popup(f: &mut ratatui::Frame, app: &App) {
    let task = app.selected_task().cloned();
    let area = centered_rect(50, 6, f.area());
    f.render_widget(Clear, area);
    let text = match task {
        Some(t) => format!(
            "Cancel task #{} \"{}\"?\n\ny = yes, cancel it    n = no, keep it",
            t.id, t.title
        ),
        None => "No task selected.".to_string(),
    };
    let p = Paragraph::new(text)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Please confirm ")
                .border_style(Style::default().fg(Color::Red)),
        )
        .wrap(Wrap { trim: true });
    f.render_widget(p, area);
}

fn render_help(f: &mut ratatui::Frame) {
    let text = "\
Welcome to acs — your agent control panel.

Everything on this screen updates live as agents
work; you never need to refresh.

  ↑ / ↓ or mouse wheel   move the selection
  tab, ← →, or click     switch panel
  m                      send a message to an agent or everyone
  t                      create a task an agent can pick up
  x                      cancel the selected task
  a                      add a new agent (creates its login token)
  enter                  read the selected message or task
  r                      reload now (rarely needed)
  ?                      this help
  q / esc                quit

Agents show ● online, ◉ waiting for work, ○ offline.";
    let area = centered_rect(60, 22, f.area());
    f.render_widget(Clear, area);
    let p = Paragraph::new(text)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Help ")
                .border_style(Style::default().fg(Color::Cyan)),
        )
        .wrap(Wrap { trim: false });
    f.render_widget(p, area);
}

fn render_detail(f: &mut ratatui::Frame, app: &App) {
    let area = centered_rect(70, (f.area().height * 60 / 100).max(10), f.area());
    f.render_widget(Clear, area);
    let p = Paragraph::new(app.detail.clone())
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(format!(" {} ", app.detail_title))
                .border_style(Style::default().fg(Color::Cyan)),
        )
        .wrap(Wrap { trim: false });
    f.render_widget(p, area);
}

fn open_detail(app: &mut App) {
    match app.pane {
        Pane::Messages => {
            if let Some(m) = app
                .state
                .messages
                .get(app.sel[2].min(app.state.messages.len().saturating_sub(1)))
            {
                app.detail_title = format!("Message #{}", m.seq);
                app.detail = format!(
                    "From: {}\nTo:   {}\nWhen: {}\n\nSubject: {}\n\n{}",
                    m.sender,
                    m.recipient.clone().unwrap_or_else(|| "everyone".into()),
                    ago(Some(m.ts_ms), now_ms()),
                    if m.subject.is_empty() {
                        "(none)"
                    } else {
                        &m.subject
                    },
                    m.body,
                );
                app.mode = Mode::Detail;
            }
        }
        Pane::Tasks => {
            if let Some(t) = app.selected_task().cloned() {
                app.detail_title = format!("Task #{}", t.id);
                let state = serde_json::to_value(&t.state)
                    .ok()
                    .and_then(|v| v.as_str().map(str::to_string))
                    .unwrap_or_default()
                    .replace('_', " ");
                app.detail = format!(
                    "Title:    {}\nState:    {}\nAssigned: {}\nUpdated:  {}",
                    t.title,
                    state,
                    t.assignee.clone().unwrap_or_else(|| "anyone".into()),
                    ago(Some(t.updated_ms), now_ms()),
                );
                app.mode = Mode::Detail;
            }
        }
        Pane::Agents => {
            if let Some(a) = app
                .state
                .agents
                .get(app.sel[0].min(app.state.agents.len().saturating_sub(1)))
            {
                let row = (
                    a.id.clone(),
                    a.role.clone(),
                    state_label(agent_state(a, now_ms())),
                    ago(a.last_seen_ms, now_ms()),
                );
                app.detail_title = format!("Agent {}", row.0);
                let detail = format!(
                    "Name:      {}\nRole:      {}\nStatus:    {}\nLast seen: {}",
                    row.0, row.1, row.2, row.3,
                );
                app.detail = match &a.runtime_note {
                    Some(note) => format!("{detail}\n\nFailure:\n{note}"),
                    None => detail,
                };
                app.mode = Mode::Detail;
            }
        }
    }
}

fn editing_buf(app: &mut App) -> &mut String {
    match app.mode {
        Mode::ComposeSubject => &mut app.subject,
        Mode::ComposeBody => &mut app.body,
        Mode::TaskTitle => &mut app.task_title,
        Mode::AgentName => &mut app.agent_name,
        Mode::AgentRole => &mut app.agent_role,
        _ => unreachable!(),
    }
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
            KeyCode::Char('?') => app.mode = Mode::Help,
            KeyCode::Tab | KeyCode::Right => {
                app.pane = PANES[(app.pane.index() + 1) % 3];
            }
            KeyCode::BackTab | KeyCode::Left => {
                app.pane = PANES[(app.pane.index() + 2) % 3];
            }
            KeyCode::Up | KeyCode::Char('k') => app.move_selection(-1),
            KeyCode::Down | KeyCode::Char('j') => app.move_selection(1),
            KeyCode::Char('m') => {
                app.to.clear();
                app.subject.clear();
                app.body.clear();
                app.to_pick = if app.pane == Pane::Agents {
                    (app.sel[0] + 1).min(app.state.agents.len())
                } else {
                    0
                };
                app.mode = Mode::PickRecipient;
            }
            KeyCode::Char('t') => {
                app.mode = Mode::TaskTitle;
                app.task_title.clear();
            }
            KeyCode::Char('a') => {
                app.mode = Mode::AgentName;
                app.agent_name.clear();
                app.agent_role.clear();
            }
            KeyCode::Char('x') => {
                if app.pane == Pane::Tasks && !app.state.tasks.is_empty() {
                    app.mode = Mode::ConfirmCancel;
                }
            }
            KeyCode::Enter => open_detail(app),
            KeyCode::Char('r') => app.refresh()?,
            _ => {}
        },
        Mode::Help | Mode::Detail => {
            app.mode = Mode::Navigate;
        }
        Mode::Setup => match key.code {
            KeyCode::Char('s') | KeyCode::Char('S') | KeyCode::Esc => {
                app.mode = Mode::Navigate;
            }
            KeyCode::Up | KeyCode::Char('k') => {
                app.setup_sel = app.setup_sel.saturating_sub(1);
            }
            KeyCode::Down | KeyCode::Char('j') => {
                app.setup_sel = (app.setup_sel + 1).min(PRESETS.len() - 1);
            }
            KeyCode::Char(' ') => {
                let on = app.setup_on.get_mut(app.setup_sel);
                if let Some(on) = on {
                    *on = !*on;
                }
            }
            KeyCode::Enter => app.apply_presets(),
            _ => {}
        },
        Mode::ConfirmCancel => match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') => {
                app.mode = Mode::Navigate;
                app.cancel_selected()?;
            }
            _ => app.mode = Mode::Navigate,
        },
        Mode::PickRecipient => match key.code {
            KeyCode::Esc => app.mode = Mode::Navigate,
            KeyCode::Up | KeyCode::Char('k') => {
                app.to_pick = app.to_pick.saturating_sub(1);
            }
            KeyCode::Down | KeyCode::Char('j') => {
                app.to_pick = (app.to_pick + 1).min(app.state.agents.len());
            }
            KeyCode::Enter => {
                app.to = if app.to_pick == 0 {
                    "*".to_string()
                } else {
                    app.state
                        .agents
                        .get(app.to_pick - 1)
                        .map(|a| a.id.clone())
                        .unwrap_or_else(|| "*".into())
                };
                app.mode = Mode::ComposeSubject;
            }
            _ => {}
        },
        Mode::ComposeSubject
        | Mode::ComposeBody
        | Mode::TaskTitle
        | Mode::AgentName
        | Mode::AgentRole => match key.code {
            KeyCode::Esc => app.mode = Mode::Navigate,
            KeyCode::Enter => match app.mode {
                Mode::ComposeSubject => app.mode = Mode::ComposeBody,
                Mode::ComposeBody => {
                    app.mode = Mode::Navigate;
                    if !app.body.is_empty() || !app.subject.is_empty() {
                        if let Err(e) = app.send() {
                            app.flash(format!("Couldn't send: {}", e.message));
                        }
                    }
                }
                Mode::TaskTitle => {
                    app.mode = Mode::Navigate;
                    if !app.task_title.is_empty() {
                        if let Err(e) = app.add_task() {
                            app.flash(format!("Couldn't create task: {}", e.message));
                        }
                    }
                }
                Mode::AgentName => {
                    if app
                        .agent_name
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
                        && !app.agent_name.is_empty()
                    {
                        app.mode = Mode::AgentRole;
                    } else {
                        app.flash("Agent names use letters, digits, - and _ only".to_string());
                    }
                }
                Mode::AgentRole => {
                    app.mode = Mode::Navigate;
                    if let Err(e) = app.add_agent() {
                        app.flash(format!("Couldn't add agent: {}", e.message));
                    }
                }
                _ => unreachable!(),
            },
            KeyCode::Backspace => {
                editing_buf(app).pop();
            }
            KeyCode::Char(c) => editing_buf(app).push(c),
            _ => {}
        },
    }
    app.dirty = true;
    Ok(())
}

/// Run the TUI. `stop` lets a test drive one loop iteration and bail.
pub fn run(db_path: &std::path::Path, stop: Option<Arc<AtomicBool>>) -> Result<i32> {
    let bus = Bus::open(Some(db_path))?;
    // First-run friendliness: create the operator identity + token on demand
    // so `acs` works out of the box on a fresh database.
    let fresh = if bus.identify(Some(OPERATOR_ID)).is_err() {
        Some(bus.init()?.operator)
    } else {
        None
    };
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
        // First run: walk through team setup instead of dropping to an empty screen.
        mode: if fresh.is_some() {
            Mode::Setup
        } else {
            Mode::Navigate
        },
        sel: [0, 0, 0],
        to_pick: 0,
        to: String::new(),
        subject: String::new(),
        body: String::new(),
        task_title: String::new(),
        agent_name: String::new(),
        agent_role: String::new(),
        detail_title: String::new(),
        detail: String::new(),
        setup_sel: 0,
        setup_on: PRESETS.iter().map(|_| true).collect(),
        flash: if fresh.is_some() {
            "Created a new bus for you".to_string()
        } else {
            String::new()
        },
        flash_until: Instant::now() + Duration::from_secs(8),
        areas: Cell::new([Rect::default(); 3]),
        dirty: true,
        quit: false,
    };

    enable_raw_mode().map_err(|e| BusError::invalid(format!("terminal: {e}")))?;
    let mut stdout = stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)
        .map_err(|e| BusError::invalid(format!("terminal: {e}")))?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal =
        Terminal::new(backend).map_err(|e| BusError::invalid(format!("terminal: {e}")))?;

    let result = (|| -> Result<()> {
        loop {
            if app.quit || stop.as_ref().is_some_and(|s| s.load(Ordering::SeqCst)) {
                return Ok(());
            }
            // Drain every queued event before blocking on the watcher, so typing
            // and clicks never wait for a poll round-trip.
            while event::poll(Duration::ZERO)
                .map_err(|e| BusError::invalid(format!("terminal: {e}")))?
            {
                match event::read().map_err(|e| BusError::invalid(format!("terminal: {e}")))? {
                    Event::Key(key) => handle_key(&mut app, key)?,
                    Event::Mouse(mouse) => match mouse.kind {
                        MouseEventKind::Down(MouseButton::Left) => {
                            if app.mode == Mode::Navigate {
                                for (i, r) in app.areas.get().iter().enumerate() {
                                    if mouse.column >= r.x
                                        && mouse.column < r.x + r.width
                                        && mouse.row >= r.y
                                        && mouse.row < r.y + r.height
                                    {
                                        app.pane = PANES[i];
                                        app.dirty = true;
                                    }
                                }
                            }
                        }
                        MouseEventKind::ScrollUp => app.move_selection(-1),
                        MouseEventKind::ScrollDown => app.move_selection(1),
                        _ => {}
                    },
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
    let _ = execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    );
    result?;
    Ok(0)
}

pub fn main() -> i32 {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    if argv.iter().any(|a| a == "--help" || a == "-h") {
        eprintln!(
            "acs — your agent control panel\n\nusage: acs [--db PATH]\n\nPress ? inside the app for help. The bus at ~/.agent-bus/bus.db is\ncreated automatically on first run."
        );
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
    let db_path =
        crate::db::resolve_db_path_with(db_flag.as_deref(), |name| std::env::var(name).ok());
    match run(&db_path, None) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("acs: {}", e.message);
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn load_state_includes_the_persisted_runtime_failure_note() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "acs-app-runtime-note-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).unwrap();
        let db_path = dir.join("bus.db");
        let bus = Bus::open(Some(&db_path)).unwrap();
        bus.init().unwrap();
        let operator = bus.identify(Some(OPERATOR_ID)).unwrap();
        bus.add_agent(
            &operator,
            "builder",
            Some("implementation"),
            None,
            None,
            None,
            Some("worker"),
        )
        .unwrap();
        let sessions = bus.home.join("sessions");
        fs::create_dir_all(&sessions).unwrap();
        fs::write(
            sessions.join("builder.json"),
            serde_json::json!({
                "failure": {
                    "exitCode": 23,
                    "consecutiveFailures": 1,
                    "reason": "harness exited 23",
                    "backoffMs": 2000,
                    "nextRetryMs": 2_000
                }
            })
            .to_string(),
        )
        .unwrap();

        let state = load_state(&bus).unwrap();
        assert_eq!(state.agents.len(), 1);
        let note = state.agents[0].runtime_note.as_deref().unwrap();
        assert!(note.contains("harness exited 23 (exit 23)"), "{note}");
        assert!(note.contains("backoff 2s"), "{note}");

        drop(bus);
        fs::remove_dir_all(dir).unwrap();
    }
}
