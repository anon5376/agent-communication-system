//! `aos`: the AOS terminal on the Acceleration Chamber design, over the ACS bus.
//!
//! It reads the same bus.db as `qagent` and `acs`, and every write it makes is
//! the same bus call the CLI makes, so it appends the same events. Design
//! sources: AOS draft #5 (Chamber handoff 0, TERMINAL-FRAMES.md) and the terminal
//! contract in AOS `codex/cli-foundation` (TERMINAL-DESIGN.md).

pub mod demo;
pub mod frame;
pub mod view;

use crate::bus::{Bus, CreateTaskInput, SendInput};
use crate::error::{BusError, Result};
use crate::identity::{self, Identity};
use crate::types::OPERATOR_ID;
use crate::watcher::{ChangeWatcher, ChangeWatcherOptions};
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use frame::{Frame, GateKind};
use ratatui::backend::CrosstermBackend;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Terminal;
use std::io::stdout;
use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};
use view::{Kind, Pending, PendingKind, Region, Role, Route, Target, Ui, VLine};

pub const DEFAULT_STALL_MIN: i64 = 30;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tier {
    Truecolor,
    Ansi16,
    Mono,
}

impl Tier {
    /// The contract's fallback order: explicit flag, NO_COLOR, dumb terminal,
    /// advertised truecolor, then 16 colours.
    pub fn detect(flag: Option<&str>) -> Tier {
        match flag {
            Some("truecolor") | Some("24bit") => return Tier::Truecolor,
            Some("16") | Some("ansi") => return Tier::Ansi16,
            Some("none") | Some("mono") | Some("no") => return Tier::Mono,
            _ => {}
        }
        if std::env::var_os("NO_COLOR").is_some_and(|v| !v.is_empty()) {
            return Tier::Mono;
        }
        if std::env::var("TERM").is_ok_and(|t| t == "dumb") {
            return Tier::Mono;
        }
        match std::env::var("COLORTERM") {
            Ok(v) if v == "truecolor" || v == "24bit" => Tier::Truecolor,
            _ => Tier::Ansi16,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Char(char),
    Enter,
    Esc,
    Tab,
    BackTab,
    Up,
    Down,
    Backspace,
    CtrlC,
    CtrlL,
}

pub fn key_from(ev: KeyEvent) -> Option<Key> {
    if ev.kind == KeyEventKind::Release {
        return None;
    }
    if ev.modifiers.contains(KeyModifiers::CONTROL) {
        return match ev.code {
            KeyCode::Char('c') => Some(Key::CtrlC),
            KeyCode::Char('l') => Some(Key::CtrlL),
            _ => None,
        };
    }
    Some(match ev.code {
        KeyCode::Char(c) => Key::Char(c),
        KeyCode::Enter => Key::Enter,
        KeyCode::Esc => Key::Esc,
        KeyCode::Tab => Key::Tab,
        KeyCode::BackTab => Key::BackTab,
        KeyCode::Up => Key::Up,
        KeyCode::Down => Key::Down,
        KeyCode::Backspace => Key::Backspace,
        _ => return None,
    })
}

pub struct App {
    pub bus: Bus,
    pub frame: Frame,
    pub ui: Ui,
    pub stall_ms: i64,
    pub quit: bool,
}

impl App {
    pub fn new(bus: Bus, stall_ms: i64) -> Result<App> {
        let frame = Frame::load(&bus, stall_ms)?;
        Ok(App {
            bus,
            frame,
            ui: Ui::default(),
            stall_ms,
            quit: false,
        })
    }

    pub fn refresh(&mut self) -> Result<()> {
        self.frame = Frame::load(&self.bus, self.stall_ms)?;
        let n = view::spine_len(&self.frame, &self.ui);
        if self.ui.sel >= n {
            self.ui.sel = n.saturating_sub(1);
        }
        if self.frame.gates.is_empty() && self.ui.region == Region::Gate {
            self.ui.region = Region::Field;
        }
        Ok(())
    }

    pub fn screen(&self) -> Vec<VLine> {
        view::screen(&self.frame, &self.ui)
    }

    fn operator(&self) -> Result<Identity> {
        self.bus.identify(Some(OPERATOR_ID))
    }

    fn receipt(&mut self, text: impl Into<String>) {
        self.ui.flash = Some((text.into(), true));
    }

    fn note(&mut self, text: impl Into<String>) {
        self.ui.flash = Some((text.into(), false));
    }

    fn go(&mut self, route: Route) {
        if self.ui.route != route {
            self.ui.prev.push(self.ui.route);
            if self.ui.prev.len() > 16 {
                self.ui.prev.remove(0);
            }
        }
        self.ui.route = route;
        self.ui.pending = None;
        self.ui.region = Region::Field;
    }

    fn back(&mut self) {
        if self.ui.route == Route::Swarm {
            self.ui.region = Region::Field;
            return;
        }
        self.ui.route = self.ui.prev.pop().unwrap_or(Route::Swarm);
    }

    fn gate_index(&self) -> Option<usize> {
        if self.frame.gates.is_empty() {
            None
        } else {
            Some(self.ui.gate_idx % self.frame.gates.len())
        }
    }

    fn choose(&mut self, n: u8) {
        let Some(i) = self.gate_index() else { return };
        let g = &self.frame.gates[i];
        let id = g.task.id;
        let kind = match (g.kind, n) {
            (GateKind::Review, 1) => Some(PendingKind::Accept),
            (GateKind::Review, 2) => Some(PendingKind::Revise),
            (GateKind::Stalled, 1) => Some(PendingKind::Requeue),
            (GateKind::Stalled, 3) => Some(PendingKind::Cancel),
            _ => None,
        };
        match kind {
            Some(kind) => {
                self.ui.pending = Some(Pending {
                    kind,
                    task_id: id,
                    typed: String::new(),
                })
            }
            None => {
                self.ui.gate_idx = self.ui.gate_idx.wrapping_add(1);
                self.note(format!("held #{id} / nothing written"));
            }
        }
    }

    fn commit(&mut self) -> Result<()> {
        let Some(p) = self.ui.pending.clone() else {
            return Ok(());
        };
        if let Some(word) = p.needs_word() {
            if p.typed != word {
                self.note(format!("type {word} exactly, or esc to cancel"));
                return Ok(());
            }
        }
        let reason = p.typed.trim().to_string();
        if p.needs_reason() && reason.is_empty() {
            self.note("a reason is required / it goes on the event");
            return Ok(());
        }
        let result = self.operator().and_then(|op| match p.kind {
            PendingKind::Accept => self
                .bus
                .review_task(&op, p.task_id, true, &reason)
                .map(|_| "accepted"),
            PendingKind::Revise => self
                .bus
                .review_task(&op, p.task_id, false, &reason)
                .map(|_| "sent back for changes"),
            PendingKind::Requeue => self
                .bus
                .requeue_task(
                    &op,
                    p.task_id,
                    Some(&reason).filter(|r| !r.is_empty()).map(|r| r.as_str()),
                )
                .map(|_| "requeued"),
            PendingKind::Cancel => self
                .bus
                .cancel_task(&op, p.task_id, Some("cancelled by the operator in aos"))
                .map(|_| "cancelled"),
        });
        self.ui.pending = None;
        match result {
            Ok(what) => {
                self.refresh()?;
                self.receipt(format!(
                    "#{} {} / event #{}",
                    p.task_id, what, self.frame.seq
                ));
                if self.ui.route == Route::Gate {
                    self.back();
                }
                self.ui.region = Region::Field;
            }
            Err(e) => self.note(format!("x FAILED / {}", e.message)),
        }
        Ok(())
    }

    fn command(&mut self, raw: &str) -> Result<()> {
        let input = raw.trim();
        self.ui.out.push(vec![
            ("> ".into(), Role::Run),
            (input.to_string(), Role::Plain),
        ]);
        let input = input
            .strip_prefix("qagent ")
            .or_else(|| input.strip_prefix("aos "))
            .unwrap_or(input);
        let mut words = input.split_whitespace();
        let Some(w) = words.next() else { return Ok(()) };
        let rest: Vec<&str> = words.collect();
        let route = match w {
            "swarm" | "s" => Some(Route::Swarm),
            "goal" | "tree" => Some(Route::Goal),
            "evidence" | "results" => Some(Route::Evidence),
            "memory" => Some(Route::Memory),
            "retro" | "log" => Some(Route::Retro),
            "providers" => Some(Route::Providers),
            "keys" => Some(Route::Help),
            _ => None,
        };
        if let Some(r) = route {
            self.go(r);
            return Ok(());
        }
        let out = |s: String| vec![(s, Role::Dim)];
        match w {
            "help" => self.ui.out.push(out("status  send <agent> <message>  task add <title>  swarm goal evidence retro providers memory".into())),
            "status" => {
                let f = &self.frame;
                let line = format!("{} agents / {} running / {} open tasks / {} reviews / {} stalled / event #{}", f.agents.len(), f.running(), f.open_tasks, f.reviews(), f.stalled(), f.seq);
                self.ui.out.push(vec![(line, Role::Plain)]);
            }
            "gate" => {
                if self.frame.gates.is_empty() {
                    self.ui.out.push(out("no gate is open".into()));
                } else {
                    self.go(Route::Gate);
                }
            }
            "send" if rest.len() >= 2 => {
                let body = rest[1..].join(" ");
                let subject: String = body.chars().take(60).collect();
                match self.operator().and_then(|op| self.bus.send(&op, SendInput { to: rest[0].into(), subject: Some(subject), body, msg_type: None, thread: None, task_id: None, refs: None, requires_ack: false })) {
                    Ok(msgs) => {
                        let seq = msgs.first().map(|m| m.seq).unwrap_or(0);
                        self.ui.out.push(vec![("[ ok ]".into(), Role::Bold), (format!(" sent #{seq} to {}", rest[0]), Role::Plain)]);
                    }
                    Err(e) => self.ui.out.push(vec![("x FAILED".into(), Role::Err), (format!(" / {}", e.message), Role::Plain)]),
                }
            }
            "task" if rest.first() == Some(&"add") && rest.len() >= 2 => {
                let title = rest[1..].join(" ");
                match self.operator().and_then(|op| self.bus.create_task(&op, CreateTaskInput { title, ..Default::default() })) {
                    Ok(t) => self.ui.out.push(vec![("[ ok ]".into(), Role::Bold), (format!(" task #{} created / {}", t.id, t.state), Role::Plain)]),
                    Err(e) => self.ui.out.push(vec![("x FAILED".into(), Role::Err), (format!(" / {}", e.message), Role::Plain)]),
                }
            }
            "send" | "task" => self.ui.out.push(out("usage: send <agent> <message>  /  task add <title>".into())),
            other => self.ui.out.push(vec![("? UNKNOWN".into(), Role::Dim), (format!(" / no command \"{}\" / type help", view::trunc(other, 20)), Role::Plain)]),
        }
        self.refresh()?;
        Ok(())
    }

    fn narrow(&self) -> bool {
        self.ui.width < 80
    }

    pub fn key(&mut self, k: Key) -> Result<()> {
        if k != Key::CtrlL {
            self.ui.flash = None;
        }
        if self.ui.quit_prompt {
            if matches!(k, Key::Char('y') | Key::CtrlC) {
                self.quit = true;
            }
            self.ui.quit_prompt = false;
            return Ok(());
        }
        if k == Key::CtrlC {
            self.ui.quit_prompt = true;
            return Ok(());
        }
        if let Some(p) = self.ui.pending.as_mut() {
            match k {
                Key::Esc => {
                    self.ui.pending = None;
                    self.note("choice cancelled / nothing written");
                }
                Key::Backspace => {
                    p.typed.pop();
                }
                Key::Enter => self.commit()?,
                Key::Char(c) if p.needs_word().is_some() => {
                    if p.typed.len() < 12 {
                        p.typed.push(c.to_ascii_uppercase());
                    }
                }
                Key::Char(c) if p.typed.chars().count() < 400 => p.typed.push(c),
                _ => {}
            }
            return Ok(());
        }
        if self.ui.filtering {
            match k {
                Key::Esc => {
                    self.ui.filtering = false;
                    self.ui.filter.clear();
                }
                Key::Enter => self.ui.filtering = false,
                Key::Backspace => {
                    self.ui.filter.pop();
                }
                Key::Char(c) if self.ui.filter.len() < 32 => self.ui.filter.push(c),
                _ => {}
            }
            self.ui.sel = 0;
            return Ok(());
        }
        if self.ui.route == Route::Home {
            match k {
                Key::Esc => self.back(),
                Key::Enter => {
                    let line = std::mem::take(&mut self.ui.prompt);
                    self.command(&line)?;
                }
                Key::Backspace => {
                    self.ui.prompt.pop();
                }
                Key::Char(c) if self.ui.prompt.chars().count() < 400 => self.ui.prompt.push(c),
                _ => {}
            }
            return Ok(());
        }
        match k {
            Key::Char('?') => {
                self.go(Route::Help);
                return Ok(());
            }
            Key::Esc => {
                if !self.ui.filter.is_empty() && self.ui.route == Route::Swarm {
                    self.ui.filter.clear();
                } else {
                    self.back();
                }
                return Ok(());
            }
            Key::Char('q') => {
                self.ui.quit_prompt = true;
                return Ok(());
            }
            Key::Char('f') => {
                self.ui.follow = !self.ui.follow;
                return Ok(());
            }
            Key::CtrlL => return Ok(()),
            Key::Char(c @ '1'..='3')
                if !self.frame.gates.is_empty()
                    && matches!(self.ui.route, Route::Swarm | Route::Gate | Route::Inspect) =>
            {
                self.choose(c as u8 - b'0');
                return Ok(());
            }
            _ => {}
        }
        if self.narrow() && self.ui.route == Route::Swarm {
            let n = view::narrow_objects(&self.frame).max(1);
            match k {
                Key::Char('n') | Key::Char('j') | Key::Down => {
                    self.ui.nobj = (self.ui.nobj + 1) % n;
                    self.ui.more = false;
                    return Ok(());
                }
                Key::Char('b') | Key::Char('k') | Key::Up => {
                    self.ui.nobj = (self.ui.nobj + n - 1) % n;
                    self.ui.more = false;
                    return Ok(());
                }
                Key::Char('m') => {
                    self.ui.more = !self.ui.more;
                    return Ok(());
                }
                _ => {}
            }
        }
        let route_key = match k {
            Key::Char('g') => Some(Route::Goal),
            Key::Char('s') => Some(Route::Swarm),
            Key::Char('e') => Some(Route::Evidence),
            Key::Char('m') => Some(Route::Memory),
            Key::Char('r') => Some(Route::Retro),
            Key::Char('c') => Some(Route::Home),
            Key::Char('p') => Some(Route::Providers),
            _ => None,
        };
        if let Some(r) = route_key {
            self.go(r);
            return Ok(());
        }
        let down = matches!(k, Key::Char('j') | Key::Down);
        let up = matches!(k, Key::Char('k') | Key::Up);
        match self.ui.route {
            Route::Evidence => {
                let n = self.frame.results.len();
                if down && n > 0 {
                    self.ui.ev_sel = (self.ui.ev_sel + 1).min(n - 1);
                }
                if up {
                    self.ui.ev_sel = self.ui.ev_sel.saturating_sub(1);
                }
                if k == Key::Enter {
                    if let Some(t) = self.frame.results.get(self.ui.ev_sel) {
                        self.ui.inspect = Some(Target::Task(t.id));
                        self.go(Route::Inspect);
                    }
                }
            }
            Route::Goal => {
                let n = self.frame.tree.len();
                if down && n > 0 {
                    self.ui.goal_sel = (self.ui.goal_sel + 1).min(n - 1);
                }
                if up {
                    self.ui.goal_sel = self.ui.goal_sel.saturating_sub(1);
                }
                if k == Key::Enter {
                    if let Some(node) = self.frame.tree.get(self.ui.goal_sel) {
                        self.ui.inspect = Some(Target::Task(node.task.id));
                        self.go(Route::Inspect);
                    }
                }
            }
            Route::Retro => {
                if up {
                    self.ui.retro_back += 1;
                    self.ui.follow = false;
                }
                if down {
                    self.ui.retro_back = self.ui.retro_back.saturating_sub(1);
                }
                if k == Key::Char('a') {
                    self.ui.retro_all = !self.ui.retro_all;
                    self.ui.retro_back = 0;
                }
            }
            Route::Inspect => {
                if let Some(Target::Agent(id)) = &self.ui.inspect {
                    if down || up {
                        let ids: Vec<String> =
                            self.frame.agents.iter().map(|a| a.id.clone()).collect();
                        if let Some(i) = ids.iter().position(|x| x == id) {
                            let n = ids.len();
                            let j = if down { (i + 1) % n } else { (i + n - 1) % n };
                            self.ui.inspect = Some(Target::Agent(ids[j].clone()));
                        }
                    }
                }
                if matches!(k, Key::Tab | Key::BackTab) && !self.frame.gates.is_empty() {
                    self.ui.route = Route::Swarm;
                    self.ui.region = Region::Gate;
                }
            }
            Route::Swarm => {
                let n = view::spine_len(&self.frame, &self.ui);
                if down || up {
                    self.ui.region = Region::Field;
                    if n > 0 {
                        self.ui.sel = if down {
                            (self.ui.sel + 1).min(n - 1)
                        } else {
                            self.ui.sel.saturating_sub(1)
                        };
                    }
                }
                match k {
                    Key::Tab | Key::BackTab => {
                        self.ui.region =
                            if self.ui.region == Region::Field && !self.frame.gates.is_empty() {
                                Region::Gate
                            } else {
                                Region::Field
                            };
                    }
                    Key::Char('d') => self.ui.expanded = !self.ui.expanded,
                    Key::Char('/') => {
                        self.ui.filtering = true;
                        self.ui.filter.clear();
                        self.ui.sel = 0;
                    }
                    Key::Enter => {
                        if self.ui.region == Region::Gate {
                            self.go(Route::Gate);
                        } else {
                            let agents = view::visible_agents(&self.frame, &self.ui);
                            if view::has_aggregate(&self.frame, &self.ui) && self.ui.sel + 1 == n {
                                self.ui.expanded = true;
                            } else if let Some(a) = agents.get(self.ui.sel) {
                                let id = a.id.clone();
                                self.ui.inspect = Some(Target::Agent(id));
                                self.go(Route::Inspect);
                            }
                        }
                    }
                    _ => {}
                }
            }
            _ => {}
        }
        Ok(())
    }
}

// ------------------------------------------------------------------ painting

const VOID: Color = Color::Rgb(0x05, 0x05, 0x09);
const COLD: Color = Color::Rgb(0xFA, 0xFB, 0xFF);

fn role_style(r: Role, tier: Tier) -> Style {
    match tier {
        Tier::Mono => Style::default(),
        Tier::Truecolor => {
            let fg = |c: Color| Style::default().fg(c);
            match r {
                Role::Plain => fg(COLD),
                Role::Dim => fg(Color::Rgb(0x7F, 0x81, 0x8C)),
                Role::Rule => fg(Color::Rgb(0x34, 0x34, 0x3F)),
                Role::Run => fg(Color::Rgb(0x6A, 0x73, 0xFF)),
                Role::Wait => fg(Color::Rgb(0xC9, 0xCA, 0xD3)),
                Role::Uv => fg(Color::Rgb(0x8F, 0x75, 0xFF)),
                Role::Err | Role::Gate => fg(Color::Rgb(0xFF, 0x3B, 0x12)),
                Role::Ok => fg(Color::Rgb(0xB8, 0xFF, 0x3D)),
                Role::Bold => fg(COLD).add_modifier(Modifier::BOLD),
                Role::Cursor => Style::default().fg(VOID).bg(COLD),
            }
        }
        Tier::Ansi16 => {
            let fg = |c: Color| Style::default().fg(c);
            match r {
                Role::Plain => fg(Color::White),
                Role::Dim => fg(Color::Gray),
                Role::Rule => fg(Color::DarkGray),
                Role::Run => fg(Color::LightBlue),
                Role::Wait => fg(Color::White),
                Role::Uv => fg(Color::LightMagenta),
                Role::Err | Role::Gate => fg(Color::LightRed),
                Role::Ok => fg(Color::LightGreen),
                Role::Bold => fg(Color::White).add_modifier(Modifier::BOLD),
                Role::Cursor => Style::default().add_modifier(Modifier::REVERSED),
            }
        }
    }
}

/// Paint composed lines in a colour tier. Mono emits no colour or attributes:
/// the selected row gets `>` in its first column and the cursor is `_`.
pub fn paint(lines: &[VLine], tier: Tier) -> Vec<Line<'static>> {
    lines
        .iter()
        .map(|l| {
            let mut segs = l.segs.clone();
            if tier != Tier::Truecolor {
                if l.kind == Kind::Sel {
                    if let Some((t, _)) = segs.first_mut() {
                        if !t.is_empty() {
                            *t = format!(">{}", t.chars().skip(1).collect::<String>());
                        }
                    }
                }
                for (t, r) in segs.iter_mut() {
                    if *r == Role::Cursor && tier == Tier::Mono {
                        *t = "_".into();
                    }
                }
            }
            let line_style = match (l.kind, tier) {
                (_, Tier::Mono) => Style::default(),
                (Kind::Rail, Tier::Truecolor) => {
                    Style::default().bg(Color::Rgb(0x10, 0x1C, 0xFF)).fg(COLD)
                }
                (Kind::Rail, Tier::Ansi16) => Style::default().bg(Color::Blue).fg(Color::White),
                (Kind::Sel, Tier::Truecolor) => Style::default().bg(Color::Rgb(0x23, 0x23, 0x2C)),
                (Kind::Sel, Tier::Ansi16) => Style::default().add_modifier(Modifier::BOLD),
                (Kind::Plain, Tier::Truecolor) => Style::default().bg(VOID),
                (Kind::Plain, Tier::Ansi16) => Style::default(),
            };
            let spans: Vec<Span<'static>> = segs
                .into_iter()
                .map(|(t, r)| {
                    let mut st = role_style(r, tier);
                    if l.kind == Kind::Rail && tier != Tier::Mono {
                        st = st.fg(if tier == Tier::Truecolor {
                            COLD
                        } else {
                            Color::White
                        });
                    }
                    if r != Role::Cursor {
                        if let Some(bg) = line_style.bg {
                            st = st.bg(bg);
                        }
                    }
                    Span::styled(t, st)
                })
                .collect();
            Line::from(spans).style(line_style)
        })
        .collect()
}

// ------------------------------------------------------------------ run

fn term_err(e: impl std::fmt::Display) -> BusError {
    BusError::invalid(format!("terminal: {e}"))
}

const READ_ONLY_NOTE: &str = "read only / operator token missing or not this bus's / see AOS.md";

/// Make sure the operator can write. A bus with no operator yet gets one
/// (`init`, which creates or adopts the token). A bus whose operator token file
/// is missing or does not match is left alone: rotating it here would break
/// whatever holds the current token (qagent, an MCP config). Returns false when
/// writes will be refused.
pub fn ensure_operator(bus: &Bus) -> Result<bool> {
    if bus.identify(Some(OPERATOR_ID)).is_ok() {
        return Ok(true);
    }
    if identity::stored_identity(&bus.conn, OPERATOR_ID)?.is_some() {
        return Ok(false);
    }
    bus.init()?;
    Ok(bus.identify(Some(OPERATOR_ID)).is_ok())
}

pub fn run(db_path: &Path, tier: Tier, stall_ms: i64) -> Result<i32> {
    let bus = Bus::open(Some(db_path))?;
    let writable = ensure_operator(&bus)?;
    let watcher = ChangeWatcher::new(
        &bus.db_path,
        ChangeWatcherOptions {
            min_poll_ms: 25,
            max_poll_ms: 500,
            fs_watch: true,
        },
    )?;
    let mut app = App::new(bus, stall_ms)?;
    if !writable {
        app.note(READ_ONLY_NOTE);
    }
    let mut seq = app.frame.seq;

    enable_raw_mode().map_err(term_err)?;
    let mut out = stdout();
    execute!(out, EnterAlternateScreen).map_err(term_err)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(out)).map_err(term_err)?;
    let result = (|| -> Result<()> {
        let mut dirty = true;
        let mut last_tick = Instant::now();
        let mut last_drawn: Vec<String> = Vec::new();
        let stop = AtomicBool::new(false);
        loop {
            if app.quit {
                return Ok(());
            }
            if event::poll(Duration::from_millis(250)).map_err(term_err)? {
                while event::poll(Duration::ZERO).map_err(term_err)? {
                    match event::read().map_err(term_err)? {
                        Event::Key(k) => {
                            if let Some(key) = key_from(k) {
                                if key == Key::CtrlL {
                                    terminal.clear().map_err(term_err)?;
                                    last_drawn.clear();
                                }
                                app.key(key)?;
                                dirty = true;
                            }
                        }
                        Event::Resize(..) => dirty = true,
                        _ => {}
                    }
                }
            }
            let next = watcher.next(seq, Duration::ZERO, &stop)?;
            if next > seq {
                seq = next;
                app.refresh()?;
                dirty = true;
            }
            // Ages and stall windows move with the clock; recompute twice a minute.
            if last_tick.elapsed() >= Duration::from_secs(30) {
                last_tick = Instant::now();
                app.refresh()?;
                dirty = true;
            }
            if dirty {
                dirty = false;
                let size = terminal.size().map_err(term_err)?;
                app.ui.width = size.width as usize;
                app.ui.height = size.height as usize;
                let lines = app.screen();
                let text: Vec<String> = lines.iter().map(|l| l.text()).collect();
                // Draw only when the screen would change: idle means no redraws.
                if text != last_drawn {
                    let painted = paint(&lines, tier);
                    terminal
                        .draw(|f| f.render_widget(Paragraph::new(painted.clone()), f.area()))
                        .map_err(term_err)?;
                    last_drawn = text;
                }
            }
        }
    })();
    disable_raw_mode().ok();
    let _ = execute!(terminal.backend_mut(), LeaveAlternateScreen);
    result?;
    Ok(0)
}

const USAGE: &str = "aos - the AOS terminal over the ACS bus

usage:
  aos [--db PATH] [--color truecolor|16|none] [--stall-min N]
  aos demo [--db PATH]       seed a sample bus, then open it
  aos --print WxH [ROUTE]    print one frame as plain text and exit
                             (routes: swarm goal evidence retro providers memory keys home gate)

The bus defaults to ~/.agent-bus/bus.db (or $QAGENT_BUS_DB, or bus.db in $QAGENT_HOME), the same file qagent and acs use.
NO_COLOR, TERM=dumb and --color none give plain text with markers and state words.
Press ? inside for keys.";

fn flag(argv: &[String], name: &str) -> Option<String> {
    argv.iter()
        .position(|a| a == name)
        .and_then(|i| argv.get(i + 1).cloned())
        .or_else(|| {
            argv.iter()
                .find_map(|a| a.strip_prefix(&format!("{name}=")).map(String::from))
        })
}

fn route_named(name: &str) -> Option<Route> {
    Some(match name {
        "swarm" => Route::Swarm,
        "goal" => Route::Goal,
        "evidence" => Route::Evidence,
        "retro" => Route::Retro,
        "providers" => Route::Providers,
        "memory" => Route::Memory,
        "keys" | "help" => Route::Help,
        "home" => Route::Home,
        "gate" => Route::Gate,
        _ => return None,
    })
}

/// One frame as plain text: the log-mode and screen-reader view of a screen.
pub fn print_frame(
    db_path: &Path,
    w: usize,
    h: usize,
    route: Route,
    stall_ms: i64,
) -> Result<String> {
    let bus = Bus::open(Some(db_path))?;
    let mut app = App::new(bus, stall_ms)?;
    app.ui.width = w;
    app.ui.height = h;
    app.ui.route = route;
    let mut s = String::new();
    for l in app.screen() {
        s.push_str(l.text().trim_end());
        s.push('\n');
    }
    Ok(s)
}

pub fn main() -> i32 {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    if argv.iter().any(|a| a == "--help" || a == "-h") {
        println!("{USAGE}");
        return 0;
    }
    let demo = argv.first().is_some_and(|a| a == "demo");
    let db_flag = flag(&argv, "--db");
    let db_path = if demo && db_flag.is_none() {
        crate::db::absolutize(&std::env::temp_dir().join("aos-demo").join("bus.db"))
    } else {
        crate::db::resolve_db_path_with(db_flag.as_deref(), |name| std::env::var(name).ok())
    };
    let stall_ms = flag(&argv, "--stall-min")
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(DEFAULT_STALL_MIN)
        .max(1)
        * 60_000;
    if demo {
        match demo::seed(&db_path) {
            Ok(true) => eprintln!("aos: seeded a sample bus at {}", db_path.display()),
            Ok(false) => eprintln!(
                "aos: opening the existing sample bus at {}",
                db_path.display()
            ),
            Err(e) => {
                eprintln!("aos: {}", e.message);
                return 1;
            }
        }
    }
    if let Some(size) = flag(&argv, "--print") {
        let (w, h) = size
            .split_once('x')
            .and_then(|(w, h)| Some((w.parse().ok()?, h.parse().ok()?)))
            .unwrap_or((80, 24));
        let route = argv
            .iter()
            .skip_while(|a| !a.starts_with("--print"))
            .nth(if argv.iter().any(|a| a.starts_with("--print=")) {
                1
            } else {
                2
            })
            .and_then(|r| route_named(r))
            .unwrap_or(Route::Swarm);
        return match print_frame(&db_path, w, h, route, stall_ms) {
            Ok(s) => {
                print!("{s}");
                0
            }
            Err(e) => {
                eprintln!("aos: {}", e.message);
                1
            }
        };
    }
    let tier = Tier::detect(flag(&argv, "--color").as_deref());
    match run(&db_path, tier, stall_ms) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("aos: {}", e.message);
            1
        }
    }
}
