//! `aos`: the AOS terminal on the Acceleration Chamber design, over the ACS bus.
//!
//! It reads the same bus.db as `qagent` and `acs`, and every write it makes is
//! the same bus call the CLI makes, so it appends the same events. Design
//! sources: AOS draft #5 (Chamber handoff 0, TERMINAL-FRAMES.md) and the terminal
//! contract in AOS `codex/cli-foundation` (TERMINAL-DESIGN.md).

pub mod crew;
pub mod demo;
pub mod frame;
pub mod view;

use crate::bus::{Bus, CreateTaskInput, ListTasksInput, SendInput};
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
use std::fs;
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
    pub paths: crew::Paths,
    /// Agent CLIs found on this computer, once something asked.
    pub found: Vec<crew::Found>,
    /// Lines typed in command home, oldest first, and where up/down is in them.
    pub typed: Vec<String>,
    pub typed_at: Option<usize>,
}

const TYPED_KEEP: usize = 500;

impl App {
    pub fn new(bus: Bus, stall_ms: i64) -> Result<App> {
        let frame = Frame::load(&bus, stall_ms)?;
        let paths = crew::Paths::for_db(&bus.db_path);
        let mut app = App {
            bus,
            frame,
            ui: Ui::default(),
            stall_ms,
            quit: false,
            typed: fs::read_to_string(paths.history_file())
                .map(|t| t.lines().map(str::to_string).collect())
                .unwrap_or_default(),
            typed_at: None,
            paths,
            found: Vec::new(),
        };
        app.refresh()?;
        Ok(app)
    }

    /// Remember a line typed in command home, here and in the history file.
    fn remember(&mut self, line: &str) {
        let line = line.trim();
        self.typed_at = None;
        if line.is_empty() || self.typed.last().map(String::as_str) == Some(line) {
            return;
        }
        self.typed.push(line.to_string());
        if self.typed.len() > TYPED_KEEP {
            self.typed.drain(..self.typed.len() - TYPED_KEEP);
        }
        let _ = fs::create_dir_all(&self.paths.dir);
        let _ = fs::write(self.paths.history_file(), self.typed.join("\n") + "\n");
    }

    /// Up (older) or down (newer) through the typed lines.
    fn recall(&mut self, older: bool) {
        let n = self.typed.len();
        if n == 0 {
            return;
        }
        let at = match (self.typed_at, older) {
            (None, true) => Some(n - 1),
            (None, false) => None,
            (Some(i), true) => Some(i.saturating_sub(1)),
            (Some(i), false) if i + 1 < n => Some(i + 1),
            (Some(_), false) => None,
        };
        self.typed_at = at;
        self.ui.prompt = at.map(|i| self.typed[i].clone()).unwrap_or_default();
    }

    /// The open goal, if the crew is configured but nobody is running it.
    pub fn stranded_goal(&self) -> Option<&crate::types::Task> {
        let c = &self.frame.crew;
        (c.configured && c.running() == 0)
            .then_some(())
            .and(self.frame.goal.as_ref())
    }

    /// First run: no crew file and nobody on the bus but the operator.
    pub fn first_run(&self) -> bool {
        !self.frame.crew.configured && self.frame.agents.is_empty()
    }

    /// Look for agent CLIs (runs each one's --version) and show the result.
    pub fn detect(&mut self) -> Result<()> {
        self.found = crew::detect();
        self.refresh()
    }

    pub fn refresh(&mut self) -> Result<()> {
        self.frame = Frame::load(&self.bus, self.stall_ms)?;
        self.frame.crew = crew::gather(&self.paths, self.found.clone());
        self.frame.add_crew_blockers();
        // The bus only learns an agent went away after its staleness window;
        // the crew's pid files know now.
        for a in self.frame.agents.iter_mut() {
            if let Some(m) = self.frame.crew.members.iter().find(|m| m.id == a.id) {
                if m.pid.is_none() && a.st != frame::St::Disconnected {
                    a.st = frame::St::Disconnected;
                    a.phrase = "stopped / c, then start".into();
                }
            }
        }
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

    /// The task an option key acts on in inspect: the inspected task, or the
    /// inspected agent's claimed task.
    fn inspected_task(&self) -> Option<&crate::types::Task> {
        let id = match self.ui.inspect.as_ref()? {
            Target::Task(id) => *id,
            Target::Agent(a) => {
                self.frame
                    .agents
                    .iter()
                    .find(|x| &x.id == a)?
                    .task
                    .as_ref()?
                    .0
            }
        };
        self.frame
            .tree
            .iter()
            .find(|n| n.task.id == id)
            .map(|n| &n.task)
    }

    fn choose_on_task(&mut self, n: u8) {
        let Some(t) = self.inspected_task() else {
            self.note("nothing to act on here");
            return;
        };
        let id = t.id;
        match view::task_options(t).into_iter().find(|(k, _)| *k == n) {
            Some((_, kind)) => self.ask(kind, id),
            None => self.note(format!("no option {n} on #{id} / see options")),
        }
    }

    fn ask(&mut self, kind: PendingKind, task_id: i64) {
        self.ui.pending = Some(Pending {
            kind,
            task_id,
            typed: String::new(),
            text: None,
        });
    }

    /// Cancel the goal and every open task under it, deepest first. Returns
    /// how many tasks were cancelled.
    fn stop_goal(&self, op: &Identity, goal: i64) -> Result<usize> {
        let all = self.bus.list_tasks(ListTasksInput {
            mine: None,
            states: None,
            include_closed: true,
            limit: Some(1000),
        })?;
        let parent: std::collections::HashMap<i64, Option<i64>> =
            all.iter().map(|t| (t.id, t.parent_id)).collect();
        let depth_under = |mut id: i64| -> Option<usize> {
            let mut d = 0;
            loop {
                if id == goal {
                    return Some(d);
                }
                id = (*parent.get(&id)?)?;
                d += 1;
                if d > 64 {
                    return None;
                }
            }
        };
        let mut doomed: Vec<(usize, i64)> = all
            .iter()
            .filter(|t| !matches!(t.state.as_str(), "accepted" | "failed" | "cancelled"))
            .filter_map(|t| depth_under(t.id).map(|d| (d, t.id)))
            .collect();
        if doomed.is_empty() {
            return Err(BusError::invalid(format!(
                "#{goal} has no open tasks to stop"
            )));
        }
        doomed.sort_by_key(|(d, id)| (std::cmp::Reverse(*d), *id));
        for (_, id) in &doomed {
            self.bus
                .cancel_task(op, *id, Some("stopped by the operator in aos"))?;
        }
        Ok(doomed.len())
    }

    fn choose(&mut self, n: u8) {
        let Some(i) = self.gate_index() else { return };
        let g = &self.frame.gates[i];
        let id = g.task.id;
        if matches!(g.kind, GateKind::Failed | GateKind::Blocker) && n == 1 {
            self.ui.prompt = g.next.clone();
            self.go(Route::Home);
            return;
        }
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
                    text: None,
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
        if matches!(p.kind, PendingKind::Goal | PendingKind::Trust) {
            self.ui.pending = None;
            if p.kind == PendingKind::Trust {
                let cwd = std::env::current_dir()?;
                crew::trust(&self.paths, &cwd)?;
                self.ui.out.push(vec![
                    ("[ ok ]".into(), Role::Bold),
                    (format!(" trusted {}", crew::Paths::show(&cwd)), Role::Plain),
                ]);
                if p.text.is_none() {
                    self.start_crew(&[])?;
                    return self.refresh();
                }
            }
            if let Some((mission, goal)) = p.text {
                self.start_goal(&mission, &goal, None)?;
            }
            return self.refresh();
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
                .map(|_| "accepted".to_string()),
            PendingKind::Revise => self
                .bus
                .review_task(&op, p.task_id, false, &reason)
                .map(|_| "sent back for changes".to_string()),
            PendingKind::Requeue => self
                .bus
                .requeue_task(
                    &op,
                    p.task_id,
                    Some(&reason).filter(|r| !r.is_empty()).map(|r| r.as_str()),
                )
                .map(|_| "requeued".to_string()),
            PendingKind::Cancel => self
                .bus
                .cancel_task(&op, p.task_id, Some("cancelled by the operator in aos"))
                .map(|_| "cancelled".to_string()),
            PendingKind::Goal | PendingKind::Trust => unreachable!("handled above"),
            PendingKind::Stop => self.stop_goal(&op, p.task_id).map(|n| {
                format!(
                    "stopped / {n} task{} cancelled",
                    if n == 1 { "" } else { "s" }
                )
            }),
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
        if self.ui.route == Route::Home {
            if let Some((text, ok)) = self.ui.flash.clone() {
                let tag = if ok { "[ ok ]" } else { "[ -- ]" };
                self.ui.out.push(vec![
                    (tag.into(), Role::Bold),
                    (format!(" {text}"), Role::Plain),
                ]);
            }
        }
        Ok(())
    }

    /// Run a write and print its outcome in the home transcript.
    fn outcome<T>(&mut self, r: Result<T>, ok: impl FnOnce(T) -> String) {
        let line = match r {
            Ok(v) => vec![
                ("[ ok ]".into(), Role::Bold),
                (format!(" {}", ok(v)), Role::Plain),
            ],
            Err(e) => vec![
                ("x FAILED".into(), Role::Err),
                (format!(" / {}", e.message), Role::Plain),
            ],
        };
        self.ui.out.push(line);
    }

    /// Run one command-home line, as if typed and entered.
    pub fn command_line(&mut self, raw: &str) -> Result<()> {
        self.command(raw)
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
        // People used to other agent CLIs type /help, /resume: same thing.
        let input = input.strip_prefix('/').unwrap_or(input);
        let mut words = input.split_whitespace();
        let Some(w) = words.next() else { return Ok(()) };
        let rest: Vec<&str> = words.collect();
        let route = match w {
            "swarm" | "s" => Some(Route::Swarm),
            "goal" | "tree" => Some(Route::Goal),
            "evidence" | "results" => Some(Route::Evidence),
            "memory" => Some(Route::Memory),
            "retro" | "log" => Some(Route::Retro),
            "providers" | "crew" => Some(Route::Providers),
            "keys" => Some(Route::Help),
            _ => None,
        };
        if let Some(r) = route.filter(|_| rest.is_empty()) {
            self.go(r);
            return Ok(());
        }
        let out = |s: String| vec![(s, Role::Dim)];
        match w {
            "help" if rest.is_empty() => {
                for l in [
                    "<anything>        type what you want done; the crew takes it as a goal",
                    "fix|build|research|review|explain|docs <what>   start from a mission",
                    "missions          list mission templates (your own files, editable)",
                    "history           past goals   resume   start the crew and carry on",
                    "pause <agent|all> [why]   resume <agent|all>   budget [<agent|all> 20 turns 60 min | off]",
                    "start [agent]     start the crew here   stop agents|<agent>   stop it",
                    "setup [--force]   find agent CLIs, make the crew   doctor   check all",
                    "accept # <reason>   revise # <feedback>   requeue # [reason]",
                    "cancel #   stop [#]   cancel a task, or a goal and all under it",
                    "send <agent|all> <message>   reply <msg#> <text>   ack <msg#>   read",
                    "task add <title> [--to agent] [--under #] [--review]   status",
                    "screens: swarm goal evidence retro crew memory keys",
                    "tab completes a command; up and down bring back earlier lines",
                ] {
                    self.ui.out.push(out(l.into()));
                }
            }
            "status" if rest.is_empty() => {
                let f = &self.frame;
                let line = format!(
                    "{} agents / {} running / {} open tasks / {} reviews / {} stalled / event #{}",
                    f.agents.len(),
                    f.running(),
                    f.open_tasks,
                    f.reviews(),
                    f.stalled(),
                    f.seq
                );
                self.ui.out.push(vec![(line, Role::Plain)]);
            }
            "gate" if rest.is_empty() => {
                if self.frame.gates.is_empty() {
                    self.ui.out.push(out("no gate is open".into()));
                } else {
                    self.go(Route::Gate);
                }
            }
            "send" if rest.len() >= 2 => {
                let to = if rest[0] == "all" { "*" } else { rest[0] };
                let body = rest[1..].join(" ");
                let subject: String = body.chars().take(60).collect();
                let r = self.operator().and_then(|op| {
                    self.bus.send(
                        &op,
                        SendInput {
                            to: to.into(),
                            subject: Some(subject),
                            body,
                            msg_type: None,
                            thread: None,
                            task_id: None,
                            refs: None,
                            requires_ack: false,
                        },
                    )
                });
                let who = rest[0].to_string();
                self.outcome(r, |msgs| {
                    format!(
                        "sent #{} to {who}",
                        msgs.first().map(|m| m.seq).unwrap_or(0)
                    )
                });
            }
            "reply" if rest.len() >= 2 => {
                let Some(m) = parse_id(rest[0])
                    .and_then(|n| self.frame.mail.iter().find(|m| m.seq == n).cloned())
                else {
                    self.ui.out.push(out(format!(
                        "no message {} in the operator's mail",
                        rest[0]
                    )));
                    return self.refresh();
                };
                let body = rest[1..].join(" ");
                let subject = if m.subject.starts_with("re: ") {
                    m.subject.clone()
                } else {
                    format!("re: {}", m.subject)
                };
                let r = self.operator().and_then(|op| {
                    let sent = self.bus.send(
                        &op,
                        SendInput {
                            to: m.sender.clone(),
                            subject: Some(subject),
                            body,
                            msg_type: Some("answer".into()),
                            thread: Some(m.thread.clone()),
                            task_id: m.task_id,
                            refs: None,
                            requires_ack: false,
                        },
                    )?;
                    if m.requires_ack {
                        self.bus.ack(&op, m.seq)?;
                    }
                    Ok(sent)
                });
                self.outcome(r, |msgs| {
                    format!(
                        "replied #{} to {} on #{}",
                        msgs.first().map(|x| x.seq).unwrap_or(0),
                        m.sender,
                        m.seq
                    )
                });
            }
            "ack" if rest.len() == 1 => {
                let r = parse_id(rest[0])
                    .ok_or_else(|| BusError::invalid("ack takes a message number"))
                    .and_then(|n| self.operator().and_then(|op| self.bus.ack(&op, n)));
                self.outcome(r, |_| format!("acknowledged {}", rest[0]));
            }
            "read" if rest.is_empty() => {
                let r = self
                    .operator()
                    .and_then(|op| self.bus.inbox(&op, false, Some(500)));
                self.outcome(r, |i| {
                    format!("{} marked read / cursor #{}", i.messages.len(), i.cursor)
                });
            }
            "run" if !rest.is_empty() => {
                let (args, to, _, _) = task_flags(&rest);
                let goal = args.join(" ");
                self.start_goal("run", &goal, to)?;
            }
            "setup" if rest.iter().all(|w| *w == "--force") => {
                let force = rest.contains(&"--force");
                self.setup(force)?;
            }
            "start"
                if rest.iter().all(|w| {
                    matches!(*w, "all" | "agents" | "crew")
                        || self.frame.crew.members.iter().any(|m| m.id == *w)
                }) =>
            {
                let only: Vec<String> = rest
                    .iter()
                    .filter(|w| !matches!(**w, "all" | "agents" | "crew"))
                    .map(|w| w.to_string())
                    .collect();
                self.start_crew(&only)?;
            }
            "stop"
                if rest
                    .first()
                    .is_some_and(|w| matches!(*w, "agents" | "all" | "crew")) =>
            {
                self.stop_crew(&[])?;
            }
            "stop"
                if rest.len() == 1 && self.frame.crew.members.iter().any(|m| m.id == rest[0]) =>
            {
                self.stop_crew(&[rest[0].to_string()])?;
            }
            "history" if rest.is_empty() => self.history()?,
            "resume" | "continue" if rest.is_empty() => {
                if !self.frame.crew.configured {
                    self.say(dim_line("no crew yet / setup makes one"));
                } else {
                    let goal = self.frame.goal.clone();
                    if self.frame.crew.running() < self.frame.crew.members.len()
                        && !self.start_crew(&[])?
                    {
                        return Ok(());
                    }
                    match goal {
                        Some(g) => self.say(ok_line(format!(
                            "carrying on with goal #{} / {}",
                            g.id,
                            view::trunc(&g.title, 50)
                        ))),
                        None => self.say(dim_line(
                            "nothing open to carry on / type what you want done",
                        )),
                    }
                    let paused: Vec<(String, String)> = self
                        .frame
                        .agents
                        .iter()
                        .filter_map(|a| a.paused.clone().map(|p| (a.id.clone(), p)))
                        .collect();
                    for (id, why) in paused {
                        self.say(dim_line(format!(
                            "{id} is paused ({why}) / resume {id} lifts it"
                        )));
                    }
                }
            }
            "pause" if !rest.is_empty() && self.is_target(rest[0]) => {
                let reason = rest[1..].join(" ");
                for id in self.targets(rest[0]) {
                    let r = self.operator().and_then(|op| {
                        self.bus
                            .pause_agent(&op, &id, Some(reason.as_str()).filter(|r| !r.is_empty()))
                    });
                    self.outcome(r, |_| {
                        format!("{id} paused / it finishes any turn it is in, then starts no new one")
                    });
                }
            }
            "resume" | "continue" if rest.len() == 1 && self.is_target(rest[0]) => {
                for id in self.targets(rest[0]) {
                    let r = self
                        .operator()
                        .and_then(|op| self.bus.resume_agent(&op, &id));
                    self.outcome(r, |a| {
                        let fresh = if a.meta.get("budget").is_some() {
                            " / its budget starts again"
                        } else {
                            ""
                        };
                        format!("{id} resumed{fresh}")
                    });
                }
            }
            "budget" if rest.is_empty() => self.budgets(),
            "budget" if rest.len() >= 2 && self.is_target(rest[0]) => {
                let limits = if rest[1..] == ["off"] {
                    Ok(None)
                } else {
                    parse_limits(&rest[1..]).map(Some)
                };
                match limits {
                    Err(e) => self.say(fail_line(e)),
                    Ok(limits) => {
                        for id in self.targets(rest[0]) {
                            let r = self
                                .operator()
                                .and_then(|op| self.bus.set_budget(&op, &id, limits));
                            self.outcome(r, |_| match limits {
                                Some(l) => format!("{id} budget: {}, counted from now", l.describe()),
                                None => format!("{id} has no budget now"),
                            });
                        }
                        if limits.is_some_and(|l| l.usd.is_some()) {
                            self.say(dim_line(USD_NOTE));
                        }
                    }
                }
            }
            "missions" if rest.is_empty() => {
                for m in crew::missions(&self.paths) {
                    let usage = format!("{} <...>", m.name);
                    self.ui.out.push(vec![
                        (view::pad(&usage, 16), Role::Plain),
                        (m.summary, Role::Dim),
                    ]);
                }
                self.say(dim_line(format!(
                    "edit or add .md files in {}",
                    crew::Paths::show(&self.paths.missions())
                )));
            }
            "doctor" if rest.is_empty() => {
                for c in crew::doctor(&self.bus.db_path) {
                    self.ui.out.push(vec![
                        (
                            format!("{} ", c.mark()),
                            if c.ok == Some(false) {
                                Role::Err
                            } else {
                                Role::Dim
                            },
                        ),
                        (view::pad(&c.label, 10), Role::Bold),
                        (c.detail, Role::Plain),
                    ]);
                }
            }
            "task" if rest.first() == Some(&"add") && rest.len() >= 2 => {
                let (args, to, under, review) = task_flags(&rest[1..]);
                let title = args.join(" ");
                if title.is_empty() {
                    self.ui.out.push(out(
                        "usage: task add <title> [--to agent] [--under #] [--review]".into(),
                    ));
                } else {
                    let reviewer = review.then(|| OPERATOR_ID.to_string());
                    let r = self.operator().and_then(|op| {
                        self.bus.create_task(
                            &op,
                            CreateTaskInput {
                                title,
                                to,
                                parent_id: under,
                                reviewer,
                                ..Default::default()
                            },
                        )
                    });
                    self.outcome(r, |t| format!("task #{} created / {}", t.id, t.state));
                }
            }
            "accept" | "revise" if rest.len() >= 2 && parse_id(rest[0]).is_some() => {
                let id = parse_id(rest[0]).unwrap_or(0);
                let reason = rest[1..].join(" ");
                let ok = w == "accept";
                let r = self
                    .operator()
                    .and_then(|op| self.bus.review_task(&op, id, ok, &reason));
                self.outcome(r, |_| {
                    format!(
                        "#{id} {}",
                        if ok {
                            "accepted"
                        } else {
                            "sent back for changes"
                        }
                    )
                });
            }
            "requeue" if !rest.is_empty() && parse_id(rest[0]).is_some() => {
                let id = parse_id(rest[0]).unwrap_or(0);
                let reason = rest[1..].join(" ");
                let r = self.operator().and_then(|op| {
                    self.bus
                        .requeue_task(&op, id, Some(reason.as_str()).filter(|r| !r.is_empty()))
                });
                self.outcome(r, |_| format!("#{id} requeued"));
            }
            "cancel" if rest.len() == 1 && parse_id(rest[0]).is_some() => {
                self.ask(PendingKind::Cancel, parse_id(rest[0]).unwrap_or(0));
            }
            "stop" if rest.len() <= 1 => {
                let goal = match rest.first() {
                    Some(r) => parse_id(r),
                    None => self.frame.goal.as_ref().map(|g| g.id),
                };
                match goal {
                    Some(id) => self.ask(PendingKind::Stop, id),
                    None => self
                        .ui
                        .out
                        .push(out("no open goal to stop / stop <#>".into())),
                }
            }
            "send" | "task" | "run" | "reply" | "ack" | "accept" | "revise" | "requeue"
            | "cancel" | "stop"
                if rest.len() < 2 =>
            {
                self.ui.out.push(out(format!("usage for {w}: type help")))
            }
            m if self.frame.crew.missions.iter().any(|(n, _)| n == m) => {
                let goal = rest.join(" ");
                self.start_goal(m, &goal, None)?;
            }
            other if rest.is_empty() => self.ui.out.push(vec![
                ("? UNKNOWN".into(), Role::Dim),
                (
                    format!(
                        " / no command \"{}\" / type help, or a whole sentence to start a goal",
                        view::trunc(other, 20)
                    ),
                    Role::Plain,
                ),
            ]),
            _ => {
                self.ui.pending = Some(Pending {
                    kind: PendingKind::Goal,
                    task_id: 0,
                    typed: String::new(),
                    text: Some(("run".into(), input.to_string())),
                });
            }
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
                Key::Char(_) if matches!(p.kind, PendingKind::Goal | PendingKind::Trust) => {}
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
        if self.ui.route == Route::Welcome && self.frame.crew.configured {
            self.ui.route = Route::Swarm;
        }
        if self.ui.route == Route::Welcome {
            match k {
                Key::Enter => {
                    self.setup(false)?;
                    if self.frame.crew.configured {
                        // Welcome is done for good: home replaces it, esc leads to swarm.
                        self.ui.prev.clear();
                        self.ui.route = Route::Home;
                    } else {
                        self.note("nothing to set up yet / install a CLI marked - , then r");
                    }
                }
                Key::Char('r') => {
                    self.detect()?;
                    self.note(format!(
                        "looked again / {} agent CLIs found",
                        self.found.iter().filter(|f| f.path.is_some()).count()
                    ));
                }
                Key::Char('c') => self.go(Route::Home),
                Key::Char('q') => self.ui.quit_prompt = true,
                Key::Char('?') => self.go(Route::Help),
                Key::Esc => self.go(Route::Swarm),
                _ => {}
            }
            return Ok(());
        }
        if self.ui.route == Route::Home {
            match k {
                Key::Esc => self.back(),
                Key::Enter => {
                    let line = std::mem::take(&mut self.ui.prompt);
                    self.remember(&line);
                    self.command(&line)?;
                }
                Key::Backspace => {
                    self.ui.prompt.pop();
                }
                Key::Tab => {
                    if let Some((usage, _)) = view::suggestions(&self.frame, &self.ui.prompt, 1)
                        .into_iter()
                        .next()
                    {
                        let word = usage.split(' ').next().unwrap_or("").to_string();
                        self.ui.prompt = format!("{word} ");
                    }
                }
                Key::Up => self.recall(true),
                Key::Down => self.recall(false),
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
            Key::Char(c @ '1'..='3') if self.ui.route == Route::Inspect => {
                self.choose_on_task(c as u8 - b'0');
                return Ok(());
            }
            Key::Char('w') if matches!(self.ui.route, Route::Swarm | Route::Inspect) => {
                let target = match &self.ui.inspect {
                    Some(Target::Agent(a)) if self.ui.route == Route::Inspect => Some(a.clone()),
                    _ if self.ui.route == Route::Swarm && !self.narrow() => {
                        view::visible_agents(&self.frame, &self.ui)
                            .get(self.ui.sel)
                            .map(|a| a.id.clone())
                    }
                    _ => None,
                };
                match target {
                    Some(id) => {
                        self.go(Route::Home);
                        self.ui.prompt = format!("send {id} ");
                    }
                    None => self.note("select an agent to write to"),
                }
                return Ok(());
            }
            Key::Char(c @ '1'..='3')
                if !self.frame.gates.is_empty()
                    && matches!(self.ui.route, Route::Swarm | Route::Gate) =>
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

// ------------------------------------------------------------------ the crew

fn ok_line(text: impl Into<String>) -> Vec<(String, Role)> {
    vec![
        ("[ ok ]".into(), Role::Bold),
        (format!(" {}", text.into()), Role::Plain),
    ]
}

fn fail_line(text: impl Into<String>) -> Vec<(String, Role)> {
    vec![
        ("x FAILED".into(), Role::Err),
        (format!(" / {}", text.into()), Role::Plain),
    ]
}

fn dim_line(text: impl Into<String>) -> Vec<(String, Role)> {
    vec![(text.into(), Role::Dim)]
}

impl App {
    fn say(&mut self, l: Vec<(String, Role)>) {
        self.ui.out.push(l);
    }

    /// Detect CLIs, write the crew and presets, put the crew on the bus.
    pub fn setup(&mut self, force: bool) -> Result<()> {
        self.found = crew::detect();
        match crew::setup(&self.bus, &self.paths, &self.found, force) {
            Ok(rep) => {
                let who = rep
                    .members
                    .iter()
                    .map(|(id, cli)| format!("{id} {cli}"))
                    .collect::<Vec<_>>()
                    .join(" / ");
                self.say(ok_line(if rep.wrote_crew {
                    format!("crew ready: {who}")
                } else {
                    format!("kept your crew.json: {who} (setup --force rewrites it)")
                }));
                self.say(dim_line(format!(
                    "prompts and missions are plain files in {}",
                    crew::Paths::show(&self.paths.dir)
                )));
                self.say(dim_line(
                    "now type what you want done, e.g. fix the failing test in parser.rs",
                ));
                self.say(dim_line(format!(
                    "or start from a mission: {} <what>",
                    crew::missions(&self.paths)
                        .iter()
                        .filter(|m| m.name != "run")
                        .map(|m| m.name.clone())
                        .collect::<Vec<_>>()
                        .join("|")
                )));
            }
            Err(e) => self.say(fail_line(e.message)),
        }
        self.refresh()
    }

    fn crew_config(&mut self) -> Option<crate::config::BusConfig> {
        match crew::load_crew(&self.paths) {
            Ok(Some(c)) => Some(c),
            Ok(None) => {
                self.say(fail_line(
                    "no crew yet / type setup to make one from the CLIs on this computer",
                ));
                None
            }
            Err(e) => {
                self.say(fail_line(format!(
                    "crew.json does not load: {} / fix it, or setup --force",
                    e.message
                )));
                None
            }
        }
    }

    /// The folder agents work in: where the crew already runs, else here.
    fn workdir(&self) -> Result<std::path::PathBuf> {
        if self.frame.crew.running() > 0 {
            if let Some(d) = crew::crew_workdir(&self.paths) {
                return Ok(d);
            }
        }
        Ok(std::env::current_dir()?)
    }

    /// Start crew members (all when `only` is empty). Asks to trust the folder
    /// first. Returns false when it stopped to ask.
    fn start_crew(&mut self, only: &[String]) -> Result<bool> {
        if self.frame.crew.simulated {
            self.say(fail_line(crew::SIMULATED_NOTE));
            return Ok(false);
        }
        let Some(config) = self.crew_config() else {
            return Ok(false);
        };
        let ids: Vec<String> = if only.is_empty() {
            crew::member_ids(&config)
        } else {
            only.to_vec()
        };
        if let Some(bad) = ids.iter().find(|id| !config.agents.contains_key(*id)) {
            self.say(fail_line(format!(
                "{bad} is not in your crew / crew lists who is"
            )));
            return Ok(false);
        }
        let dir = self.workdir()?;
        if let Some(why) = crew::unsafe_workdir(&dir) {
            self.say(fail_line(why));
            return Ok(false);
        }
        if !crew::is_trusted(&self.paths, &dir) {
            self.ui.pending = Some(Pending {
                kind: PendingKind::Trust,
                task_id: 0,
                typed: String::new(),
                text: None,
            });
            return Ok(false);
        }
        crew::sync_bus(&self.bus, &config)?;
        for (id, r) in crew::start(&self.bus.db_path, &self.paths, &ids, &dir) {
            match r {
                Ok(pid) => self.say(ok_line(format!("{id} running / pid {pid}"))),
                Err(e) => self.say(fail_line(format!("{id} did not start: {}", e.message))),
            }
        }
        self.say(dim_line(format!(
            "agents work in {}",
            crew::Paths::show(&dir)
        )));
        self.say(dim_line(
            "they keep running after you leave aos / stop agents stops them",
        ));
        self.refresh()?;
        Ok(true)
    }

    fn stop_crew(&mut self, only: &[String]) -> Result<()> {
        let ids: Vec<String> = if only.is_empty() {
            self.frame
                .crew
                .members
                .iter()
                .map(|m| m.id.clone())
                .collect()
        } else {
            only.to_vec()
        };
        for (id, r) in crew::stop(&self.paths, &ids) {
            match r {
                Ok(true) => self.say(ok_line(format!("{id} stopped"))),
                Ok(false) => self.say(dim_line(format!("{id} was not running"))),
                Err(e) => self.say(fail_line(e.message)),
            }
        }
        self.refresh()
    }

    /// Start a goal from a mission template and hand it to the lead, starting
    /// the crew first if nobody is running.
    /// An agent id on the bus, or all.
    fn is_target(&self, word: &str) -> bool {
        word == "all" || self.frame.agents.iter().any(|a| a.id == word)
    }

    /// The agents a word names: one agent, or with all the crew (or every agent when there is no crew).
    fn targets(&self, word: &str) -> Vec<String> {
        if word != "all" {
            return vec![word.to_string()];
        }
        if self.frame.crew.configured {
            self.frame
                .crew
                .members
                .iter()
                .map(|m| m.id.clone())
                .collect()
        } else {
            self.frame.agents.iter().map(|a| a.id.clone()).collect()
        }
    }

    /// Every agent's budget, and what a budget can measure.
    fn budgets(&mut self) {
        let rows: Vec<(String, String)> = self
            .frame
            .agents
            .iter()
            .map(|a| {
                let text = match &a.budget {
                    Some(b) => format!(
                        "{}{}",
                        b.line(),
                        b.over().map(|_| " / used up").unwrap_or_default()
                    ),
                    None => "no budget".into(),
                };
                (a.id.clone(), text)
            })
            .collect();
        for (id, text) in rows {
            self.ui
                .out
                .push(vec![(view::pad(&id, 11), Role::Bold), (text, Role::Plain)]);
        }
        self.say(dim_line(
            "set one: budget <agent|all> 20 turns 60 min $2 / budget <agent|all> off",
        ));
        self.say(dim_line(
            "an agent that reaches its budget pauses itself and writes to you",
        ));
        self.say(dim_line(USD_NOTE));
    }

    /// The goals the operator started, newest first, and how each one ended.
    fn history(&mut self) -> Result<()> {
        let ids: Vec<i64> = {
            let mut stmt = self.bus.conn.prepare_cached(
                "SELECT id FROM tasks WHERE parent_id IS NULL AND creator = ? ORDER BY id DESC LIMIT 12",
            )?;
            let rows = stmt.query_map([OPERATOR_ID], |row| row.get::<_, i64>(0))?;
            rows.collect::<std::result::Result<_, _>>()?
        };
        if ids.is_empty() {
            self.say(dim_line("no goals yet / type what you want done"));
            return Ok(());
        }
        let now = self.frame.now;
        let w = self.ui.width.max(60);
        let mut at_gate = None;
        for id in ids {
            let t = self.bus.get_task(id)?.task;
            let state = match t.state.as_str() {
                "accepted" => "done",
                "failed" => "failed",
                "cancelled" => "stopped",
                "submitted" => "at your gate",
                _ => "open",
            };
            if t.state == "submitted" && at_gate.is_none() {
                at_gate = Some(t.id);
            }
            self.ui.out.push(vec![
                (view::pad(&format!("#{}", t.id), 6), Role::Dim),
                (view::pad(state, 14), Role::Plain),
                (view::trunc(&t.title, w.saturating_sub(32)), Role::Plain),
                (
                    format!("  {}", frame::age(Some(t.updated_ms), now)),
                    Role::Dim,
                ),
            ]);
        }
        match at_gate {
            Some(id) => self.say(dim_line(format!(
                "#{id} waits for you: accept {id} <why>, or revise {id} <what to change>"
            ))),
            None => self.say(dim_line("details of one: aos task show <#>")),
        }
        Ok(())
    }

    fn start_goal(&mut self, mission: &str, goal: &str, to: Option<String>) -> Result<()> {
        let goal = goal.trim();
        if goal.is_empty() {
            self.say(dim_line(format!("usage: {mission} <what you want done>")));
            return Ok(());
        }
        let Some(m) = crew::missions(&self.paths)
            .into_iter()
            .find(|m| m.name == mission)
        else {
            self.say(fail_line(format!(
                "no mission {mission} / missions lists them"
            )));
            return Ok(());
        };
        let (title, brief, acceptance) = crew::expand(&m, goal);
        if !self.frame.crew.configured {
            // A bus without an aos crew (agents run by qagent supervise, or none
            // yet): write the goal and leave starting agents to the operator.
            let r = self.operator().and_then(|op| {
                self.bus.create_task(
                    &op,
                    CreateTaskInput {
                        title,
                        brief: Some(brief),
                        acceptance: Some(acceptance).filter(|a| !a.is_empty()),
                        to,
                        ..Default::default()
                    },
                )
            });
            self.outcome(r, |t| {
                format!(
                    "goal #{} started / {}{}",
                    t.id,
                    t.state,
                    t.assignee.map(|a| format!(" for {a}")).unwrap_or_default()
                )
            });
            if self.frame.agents.is_empty() {
                self.say(dim_line("no agents yet, so it waits / setup makes a crew"));
            }
            return self.refresh();
        }
        let Some(config) = self.crew_config() else {
            return Ok(());
        };
        let lead = to.or_else(|| crew::goal_owner(&config));
        // A lead plans and hands the goal back to the operator; a worker's result
        // goes to an independent reviewer, or to the operator when there is none.
        let reviewer = match lead.as_deref() {
            Some(id)
                if config
                    .agents
                    .get(id)
                    .is_some_and(|a| a.authority != "manager") =>
            {
                crew::reviewer_for(&config, id)
            }
            _ => OPERATOR_ID.to_string(),
        };
        if self.frame.crew.running() == 0 {
            let started = self.start_crew(&[])?;
            if !started {
                if let Some(p) = self.ui.pending.as_mut() {
                    p.text = Some((mission.to_string(), goal.to_string()));
                }
                return Ok(());
            }
        }
        let r = self.operator().and_then(|op| {
            self.bus.create_task(
                &op,
                CreateTaskInput {
                    title,
                    brief: Some(brief),
                    acceptance: Some(acceptance).filter(|a| !a.is_empty()),
                    to: lead.clone(),
                    reviewer: Some(reviewer.clone()),
                    ..Default::default()
                },
            )
        });
        match r {
            Ok(t) => {
                self.say(ok_line(format!(
                    "goal #{} started / {} is on it",
                    t.id,
                    lead.unwrap_or_else(|| "the first free agent".into())
                )));
                self.say(dim_line("watch it: esc, then s swarm or g goal"));
                self.say(dim_line(if reviewer == OPERATOR_ID {
                    "the result comes to your gate to accept or send back".to_string()
                } else {
                    format!("{reviewer} reviews the result independently; it can send it back")
                }));
            }
            Err(e) => self.say(fail_line(e.message)),
        }
        self.refresh()
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
    } else if app.first_run() {
        app.detect()?;
        app.ui.route = Route::Welcome;
    } else if let Some(g) = app.stranded_goal() {
        let text = format!(
            "goal #{} is still open and the crew is stopped / c, then resume",
            g.id
        );
        app.note(text);
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

const USAGE: &str = "aos - mission control for a team of AI coding agents

  aos                       open aos in this folder; the first time, it sets up your crew
  aos \"<what you want>\"     hand a goal to your crew and return
  aos fix <what is broken>  start from a mission: build, fix, research, review,
                            explain, docs, or any mission file you add
  aos start | aos stop      start the crew in this folder (it keeps running) / stop it
  aos resume                start the crew again and carry on with what is open
  aos history               your past goals and how each ended
  aos pause <agent|all>     no new turns until  aos resume <agent|all>
  aos budget all 20 turns 60 min   limits per agent; budget lists them, off clears
  aos setup [--force]       find agent CLIs on this computer and write your crew
  aos doctor                check everything and say what to fix
  aos missions              list the mission templates
  aos demo                  try aos on a sample team; nothing real runs

options: --db PATH   --color truecolor|16|none   --stall-min N   --print WxH [screen]
         --yes       allow agents to work in this folder without asking

Your crew, role prompts and missions are plain files in ~/.agent-bus/aos.
Every qagent command works too: aos task list, aos log --follow, aos mcp.
Inside aos: press ? for keys, or c and type help.";

/// qagent commands `aos` hands to the CLI unchanged, so one binary does both.
const QAGENT_COMMANDS: &[&str] = &[
    "init",
    "agent",
    "token",
    "whoami",
    "status",
    "send",
    "inbox",
    "ack",
    "wait",
    "task",
    "log",
    "trace",
    "import",
    "mcp",
    "mcp-config",
    "supervise",
    "dashboard",
    "fake-harness",
];

/// Positional words, skipping flags and their values.
fn positionals(argv: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < argv.len() {
        let a = &argv[i];
        if a == "--" {
            out.extend(argv[i + 1..].iter().cloned());
            break;
        }
        if let Some(name) = a.strip_prefix("--") {
            if !a.contains('=')
                && matches!(
                    name,
                    "db" | "color" | "stall-min" | "print" | "as" | "config"
                )
            {
                i += 1;
            }
        } else {
            out.push(a.clone());
        }
        i += 1;
    }
    out
}

fn line_text(segs: &[(String, Role)]) -> String {
    segs.iter().map(|(t, _)| t.as_str()).collect::<String>()
}

fn ask_yes(question: &str) -> bool {
    use std::io::{IsTerminal, Write};
    if !std::io::stdin().is_terminal() {
        return false;
    }
    eprint!("{question} [y/N] ");
    let _ = std::io::stderr().flush();
    let mut answer = String::new();
    let _ = std::io::stdin().read_line(&mut answer);
    matches!(answer.trim(), "y" | "Y" | "yes" | "YES")
}

/// `aos <words>` from the shell: the same command home line aos runs inside,
/// with its transcript printed and its questions asked on the terminal.
fn shell_line(db_path: &Path, words: &[String], yes: bool, stall_ms: i64) -> i32 {
    let bus = match Bus::open(Some(db_path)) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("aos: {}", e.message);
            return 1;
        }
    };
    if let Err(e) = ensure_operator(&bus) {
        eprintln!("aos: {}", e.message);
        return 1;
    }
    let mut app = match App::new(bus, stall_ms) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("aos: {}", e.message);
            return 1;
        }
    };
    app.ui.out.clear();
    // From the shell, a bare `aos stop` means the agents, not the goal.
    let line = if words.len() == 1 && words[0] == "stop" {
        "stop agents".to_string()
    } else {
        words.join(" ")
    };
    let run = |app: &mut App| -> Result<()> {
        app.command_line(&line)?;
        // A whole sentence typed at the shell is meant as a goal: no second ask.
        if app
            .ui
            .pending
            .as_ref()
            .is_some_and(|p| p.kind == PendingKind::Goal)
        {
            app.commit()?;
        }
        if app
            .ui
            .pending
            .as_ref()
            .is_some_and(|p| p.kind == PendingKind::Trust)
        {
            let dir = std::env::current_dir()
                .map(|d| crew::Paths::show(&d))
                .unwrap_or_default();
            if yes
                || ask_yes(&format!(
                    "aos: agents will run commands and edit files in {dir}. Allow?"
                ))
            {
                app.commit()?;
            } else {
                app.ui.pending = None;
                app.say(fail_line(format!(
                    "not started / agents need your OK to work in {dir}: answer y, or pass --yes"
                )));
            }
        }
        if let Some(p) = app.ui.pending.take() {
            let word = p.needs_word().unwrap_or("it");
            app.say(fail_line(format!(
                "nothing written / this needs you to type {word}: open aos, press c and run it there"
            )));
        }
        Ok(())
    };
    let r = run(&mut app);
    let mut failed = r.is_err();
    for l in app.ui.out.iter().skip(1) {
        let t = line_text(l);
        failed |= t.starts_with("x FAILED") || t.starts_with("? UNKNOWN");
        println!("{}", t.trim_end());
    }
    if let Err(e) = r {
        eprintln!("aos: {}", e.message);
    }
    if line.trim() == "doctor" {
        failed = crew::doctor(db_path).iter().any(|c| c.ok == Some(false));
    }
    i32::from(failed)
}

const USD_NOTE: &str = "turns and minutes are always counted; dollars only as each CLI reports them, and a CLI that reports none counts as $0";

/// "20 turns 60 min $2" (also "1 h", "2 usd") into budget limits.
fn parse_limits(words: &[&str]) -> std::result::Result<crate::control::Limits, String> {
    let usage = "usage: budget <agent|all> 20 turns 60 min $2, or budget <agent|all> off";
    let mut l = crate::control::Limits::default();
    let mut i = 0;
    while i < words.len() {
        let w = words[i].to_lowercase();
        if let Some(n) = w.strip_prefix('$') {
            l.usd = Some(n.parse().map_err(|_| usage.to_string())?);
            i += 1;
            continue;
        }
        let n: f64 = w.parse().map_err(|_| usage.to_string())?;
        let unit = words
            .get(i + 1)
            .map(|u| u.to_lowercase())
            .unwrap_or_default();
        match unit.as_str() {
            "turn" | "turns" => l.turns = Some(n),
            "min" | "mins" | "minute" | "minutes" | "m" => l.minutes = Some(n),
            "h" | "hour" | "hours" => l.minutes = Some(n * 60.0),
            "usd" | "dollar" | "dollars" | "$" => l.usd = Some(n),
            _ => return Err(usage.to_string()),
        }
        i += 2;
    }
    if l.is_empty() {
        return Err(usage.to_string());
    }
    Ok(l)
}

/// `#12` or `12`.
fn parse_id(s: &str) -> Option<i64> {
    s.trim_start_matches('#').parse().ok().filter(|n| *n > 0)
}

/// Split `--to A`, `--under #` and `--review` out of a command's words.
fn task_flags<'a>(words: &[&'a str]) -> (Vec<&'a str>, Option<String>, Option<i64>, bool) {
    let mut rest = Vec::new();
    let (mut to, mut under, mut review) = (None, None, false);
    let mut i = 0;
    while i < words.len() {
        match words[i] {
            "--to" if i + 1 < words.len() => {
                to = Some(words[i + 1].to_string());
                i += 1;
            }
            "--under" if i + 1 < words.len() => {
                under = parse_id(words[i + 1]);
                i += 1;
            }
            "--review" => review = true,
            w => rest.push(w),
        }
        i += 1;
    }
    (rest, to, under, review)
}

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
        "crew" => Route::Providers,
        "welcome" => Route::Welcome,
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
    if route == Route::Welcome {
        app.detect()?;
    }
    let mut s = String::new();
    for l in app.screen() {
        s.push_str(l.text().trim_end());
        s.push('\n');
    }
    Ok(s)
}

/// Removes a folder when dropped: the demo's temporary bus.
struct RemoveOnDrop(std::path::PathBuf);

impl Drop for RemoveOnDrop {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

pub fn main() -> i32 {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let words = positionals(&argv);
    if let Some(first) = words.first() {
        if QAGENT_COMMANDS.contains(&first.as_str()) {
            let mut io = crate::cli::Io::default();
            return crate::cli::run(&argv, &mut io);
        }
    }
    if argv.iter().any(|a| a == "--help" || a == "-h") || words.first().is_some_and(|w| w == "help")
    {
        println!("{USAGE}");
        return 0;
    }
    if argv.iter().any(|a| a == "--version" || a == "-V") {
        println!("aos {}", env!("CARGO_PKG_VERSION"));
        return 0;
    }
    let demo = words.first().is_some_and(|a| a == "demo");
    let db_flag = flag(&argv, "--db");
    // The demo gets a fresh sample bus in its own temporary folder each time,
    // removed when aos leaves, so it never touches a real bus and never goes stale.
    let demo_dir = (demo && db_flag.is_none()).then(|| {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default();
        std::env::temp_dir().join(format!("aos-demo-{}-{nanos}", std::process::id()))
    });
    let _cleanup = demo_dir.clone().map(RemoveOnDrop);
    let db_path = if let Some(dir) = &demo_dir {
        crate::db::absolutize(&dir.join("bus.db"))
    } else {
        crate::db::resolve_db_path_with(db_flag.as_deref(), |name| std::env::var(name).ok())
    };
    let stall_ms = flag(&argv, "--stall-min")
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(DEFAULT_STALL_MIN)
        .max(1)
        * 60_000;
    if !demo && !words.is_empty() && flag(&argv, "--print").is_none() {
        let yes = argv.iter().any(|a| a == "--yes" || a == "-y");
        return shell_line(&db_path, &words, yes, stall_ms);
    }
    if demo {
        match demo::seed(&db_path) {
            Ok(true) => eprintln!(
                "aos demo: a SIMULATED team on a temporary bus at {}; nothing real runs, no account or key is used, and it is deleted when you leave",
                db_path.display()
            ),
            Ok(false) => eprintln!(
                "aos demo: opening the existing sample bus at {}",
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
    {
        use std::io::IsTerminal;
        if !std::io::stdout().is_terminal() || !std::io::stdin().is_terminal() {
            eprintln!("aos: the console needs a terminal. In scripts use aos --print 80x24, or the shell commands in aos --help.");
            return 2;
        }
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
