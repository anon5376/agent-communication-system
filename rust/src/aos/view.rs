//! Pure screen composition for `aos`. Every screen is a list of lines of
//! (text, role) segments at an exact width, so it can be tested as plain text
//! and painted in any colour tier. Meaning is carried by words and markers;
//! the role only adds colour.

use super::frame::{age, clock, reviewer_of, span, task_state, AgentView, Frame, GateKind, St};
use crate::types::OPERATOR_ID;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Plain,
    Dim,
    Rule,
    Run,
    Wait,
    Uv,
    Err,
    Gate,
    Ok,
    Bold,
    Cursor,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Plain,
    Rail,
    Sel,
}

#[derive(Debug, Clone)]
pub struct VLine {
    pub segs: Vec<(String, Role)>,
    pub kind: Kind,
}

impl VLine {
    pub fn text(&self) -> String {
        self.segs.iter().map(|(t, _)| t.as_str()).collect()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Route {
    Swarm,
    Inspect,
    Gate,
    Goal,
    Evidence,
    Memory,
    Retro,
    Providers,
    Help,
    Home,
    /// First run: what was found on this computer and the crew aos proposes.
    Welcome,
}

impl Route {
    pub fn name(self) -> &'static str {
        match self {
            Route::Swarm => "swarm",
            Route::Inspect => "inspect",
            Route::Gate => "gate",
            Route::Goal => "goal",
            Route::Evidence => "evidence",
            Route::Memory => "memory",
            Route::Retro => "retro",
            Route::Providers => "crew",
            Route::Help => "keys",
            Route::Home => "home",
            Route::Welcome => "welcome",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    Agent(String),
    Task(i64),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Region {
    Field,
    Gate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PendingKind {
    Accept,
    Revise,
    Requeue,
    Cancel,
    /// Cancel the goal and every open task under it.
    Stop,
    /// Start a typed line that is not a command as a goal.
    Goal,
    /// Let agents work in the current folder, then start the crew.
    Trust,
}

#[derive(Debug, Clone)]
pub struct Pending {
    pub kind: PendingKind,
    pub task_id: i64,
    pub typed: String,
    /// For Goal and Trust: the mission and the goal waiting on this choice.
    pub text: Option<(String, String)>,
}

/// The actions the operator can take on a task in its current state, as
/// (option number, action). Closed tasks have none.
pub fn task_options(t: &crate::types::Task) -> Vec<(u8, PendingKind)> {
    match t.state.as_str() {
        "submitted" => vec![
            (1, PendingKind::Accept),
            (2, PendingKind::Revise),
            (3, PendingKind::Cancel),
        ],
        "claimed" => vec![(1, PendingKind::Requeue), (3, PendingKind::Cancel)],
        "open" | "blocked" | "changes_requested" => vec![(3, PendingKind::Cancel)],
        _ => Vec::new(),
    }
}

fn option_word(k: PendingKind) -> &'static str {
    match k {
        PendingKind::Accept => "ACCEPT",
        PendingKind::Revise => "REVISE",
        PendingKind::Requeue => "REQUEUE",
        PendingKind::Cancel => "CANCEL",
        PendingKind::Stop => "STOP",
        PendingKind::Goal => "START",
        PendingKind::Trust => "TRUST",
    }
}

fn task_options_line(t: &crate::types::Task) -> Option<VLine> {
    let opts = task_options(t);
    if opts.is_empty() {
        return None;
    }
    let text = opts
        .iter()
        .map(|(n, k)| format!("[{n}] {}", option_word(*k)))
        .collect::<Vec<_>>()
        .join("   ");
    Some(lv("options", vec![seg(text, Role::Plain)]))
}

impl Pending {
    /// What has to be typed before Enter commits, if anything.
    pub fn needs_word(&self) -> Option<&'static str> {
        match self.kind {
            PendingKind::Cancel => Some("CANCEL"),
            PendingKind::Stop => Some("STOP"),
            _ => None,
        }
    }
    pub fn needs_reason(&self) -> bool {
        matches!(self.kind, PendingKind::Accept | PendingKind::Revise)
    }
}

pub struct Ui {
    pub route: Route,
    pub prev: Vec<Route>,
    pub region: Region,
    pub sel: usize,
    pub expanded: bool,
    pub filter: String,
    pub filtering: bool,
    pub gate_idx: usize,
    pub pending: Option<Pending>,
    pub inspect: Option<Target>,
    pub goal_sel: usize,
    pub ev_sel: usize,
    pub retro_back: usize,
    pub retro_all: bool,
    pub follow: bool,
    pub nobj: usize,
    pub more: bool,
    pub prompt: String,
    pub out: Vec<Vec<(String, Role)>>,
    /// (text, is_receipt): receipts print `[ ok ]`, notes `[ -- ]`.
    pub flash: Option<(String, bool)>,
    pub quit_prompt: bool,
    pub width: usize,
    pub height: usize,
}

impl Default for Ui {
    fn default() -> Self {
        Ui {
            route: Route::Swarm,
            prev: Vec::new(),
            region: Region::Field,
            sel: 0,
            expanded: false,
            filter: String::new(),
            filtering: false,
            gate_idx: 0,
            pending: None,
            inspect: None,
            goal_sel: 0,
            ev_sel: 0,
            retro_back: 0,
            retro_all: false,
            follow: false,
            nobj: 0,
            more: false,
            prompt: String::new(),
            out: vec![vec![(
                "type what you want done and press enter, or help".into(),
                Role::Dim,
            )]],
            flash: None,
            quit_prompt: false,
            width: 80,
            height: 24,
        }
    }
}

// ------------------------------------------------------------------ text

fn chars(s: &str) -> usize {
    s.chars().count()
}

/// Single-line, printable: control characters and newlines become spaces.
fn clean(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

pub fn trunc(s: &str, n: usize) -> String {
    let s = clean(s);
    if chars(&s) <= n {
        s
    } else if n <= 3 {
        s.chars().take(n).collect()
    } else {
        let mut out: String = s.chars().take(n - 3).collect();
        out.push_str("...");
        out
    }
}

pub fn pad(s: &str, n: usize) -> String {
    let mut s = s.to_string();
    let len = chars(&s);
    if len < n {
        s.push_str(&" ".repeat(n - len));
    }
    s
}

fn pt(s: &str, n: usize) -> String {
    pad(&trunc(s, n), n)
}

fn wrap(s: &str, width: usize, max_lines: usize) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    let mut cur = String::new();
    for word in clean(s).split_whitespace() {
        if !cur.is_empty() && chars(&cur) + 1 + chars(word) > width {
            lines.push(std::mem::take(&mut cur));
        }
        if !cur.is_empty() {
            cur.push(' ');
        }
        cur.push_str(word);
    }
    if !cur.is_empty() {
        lines.push(cur);
    }
    if lines.len() > max_lines {
        lines.truncate(max_lines);
        if let Some(last) = lines.last_mut() {
            *last = trunc(&format!("{last} ..."), width);
        }
    }
    lines.into_iter().map(|l| trunc(&l, width)).collect()
}

fn seg(t: impl Into<String>, r: Role) -> (String, Role) {
    (t.into(), r)
}

fn line(segs: Vec<(String, Role)>) -> VLine {
    VLine {
        segs,
        kind: Kind::Plain,
    }
}

fn plain(t: impl Into<String>) -> VLine {
    line(vec![seg(t, Role::Plain)])
}

fn dim(t: impl Into<String>) -> VLine {
    line(vec![seg(t, Role::Dim)])
}

fn blank() -> VLine {
    plain("")
}

fn sel_if(mut l: VLine, on: bool) -> VLine {
    if on {
        l.kind = Kind::Sel;
    }
    l
}

/// Label exactly 12 columns, value from column 13.
fn lv(label: &str, mut value: Vec<(String, Role)>) -> VLine {
    let mut segs = vec![seg(pad(label, 12), Role::Dim)];
    segs.append(&mut value);
    line(segs)
}

fn lvs(label: &str, value: impl Into<String>) -> VLine {
    lv(label, vec![seg(value, Role::Plain)])
}

pub fn st_role(s: St) -> Role {
    match s {
        St::Running => Role::Run,
        St::Waiting => Role::Wait,
        St::Blocked => Role::Uv,
        St::Failed | St::Conflict | St::Disconnected => Role::Err,
        St::Gate => Role::Gate,
        St::Verified => Role::Ok,
        St::Complete => Role::Bold,
        St::Unknown | St::Unavailable => Role::Dim,
    }
}

/// The state field: at least `width` columns and always one trailing space.
fn st_seg(s: St, width: usize) -> (String, Role) {
    let label = s.label();
    let w = width.max(chars(&label) + 1);
    seg(pad(&label, w), st_role(s))
}

fn st_width(s: St) -> usize {
    12.max(chars(&s.label()) + 1)
}

/// Fit a line to exactly `w` columns.
pub fn fit(l: &VLine, w: usize) -> VLine {
    let total: usize = l.segs.iter().map(|(t, _)| chars(t)).sum();
    // Text cut at the edge ends in "..." so a clipped value never reads as whole.
    let limit = if total > w && w > 3 { w - 3 } else { w };
    let mut used = 0;
    let mut out = Vec::new();
    for (t, r) in &l.segs {
        if used >= limit {
            break;
        }
        let piece: String = t.chars().take(limit - used).collect();
        used += chars(&piece);
        out.push((piece, *r));
    }
    if limit < w {
        let role = out.last().map(|(_, r)| *r).unwrap_or(Role::Plain);
        out.push(("...".into(), role));
        used += 3;
    }
    if used < w {
        out.push((" ".repeat(w - used), Role::Plain));
    }
    VLine {
        segs: out,
        kind: l.kind,
    }
}

// ------------------------------------------------------------------ frame chrome

fn scope(f: &Frame) -> String {
    if f.crew.simulated {
        return "SIMULATED demo".into();
    }
    let path = std::path::Path::new(&f.db_path);
    let dir = path
        .parent()
        .and_then(|p| p.file_name())
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    if dir == ".agent-bus" || dir.is_empty() {
        "local".into()
    } else {
        dir
    }
}

fn rail(f: &Frame, ui: &Ui, route: &str) -> VLine {
    let w = ui.width;
    let left = format!("AOS > <  {} / {}", scope(f), route);
    let right = if w >= 80 {
        format!("{}  live #{}", f.overall().label(), f.seq)
    } else {
        format!("#{}", f.seq)
    };
    let gap = w.saturating_sub(chars(&left) + chars(&right)).max(1);
    VLine {
        segs: vec![seg(
            format!("{left}{}{right}", " ".repeat(gap)),
            Role::Plain,
        )],
        kind: Kind::Rail,
    }
}

fn rule(w: usize) -> VLine {
    line(vec![seg("-".repeat(w), Role::Rule)])
}

fn status_line(f: &Frame, ui: &Ui) -> VLine {
    let follow = if ui.follow { "on" } else { "off" };
    if let Some((text, ok)) = &ui.flash {
        return line(vec![
            seg(
                if *ok { "[ ok ]" } else { "[ -- ]" },
                if *ok { Role::Bold } else { Role::Dim },
            ),
            seg(format!(" {text} / follow {follow}"), Role::Plain),
        ]);
    }
    let last = match f.events.last() {
        Some(e) => format!(
            "last event #{} {} / {} / {}",
            e.seq,
            e.kind,
            e.actor,
            age(Some(e.ts_ms), f.now)
        ),
        None => "no events yet".into(),
    };
    line(vec![
        seg("[ -- ]", Role::Dim),
        seg(format!(" {last} / follow {follow}"), Role::Plain),
    ])
}

fn gate_open(f: &Frame) -> bool {
    !f.gates.is_empty()
}

fn keys_line(f: &Frame, ui: &Ui) -> VLine {
    if ui.quit_prompt {
        return line(vec![
            seg("leave aos? agents keep running", Role::Bold),
            seg("   y leave  n stay", Role::Plain),
        ]);
    }
    if ui.filtering {
        return line(vec![
            seg("/ ", Role::Bold),
            seg(ui.filter.clone(), Role::Bold),
            seg(" ", Role::Cursor),
            seg("   enter keep  esc clear", Role::Plain),
        ]);
    }
    if let Some(p) = &ui.pending {
        let t = if p.kind == PendingKind::Goal {
            "enter start the goal  esc cancel".to_string()
        } else if p.kind == PendingKind::Trust {
            "enter trust this folder and start the crew  esc cancel".to_string()
        } else if let Some(word) = p.needs_word() {
            format!("type {word}  enter confirm  esc cancel")
        } else if p.needs_reason() {
            "type the reason  enter confirm  esc cancel".into()
        } else {
            "enter confirm (reason optional)  esc cancel".into()
        };
        return plain(t);
    }
    let narrow = ui.width < 80;
    let t = match ui.route {
        Route::Inspect => "1-3 act  w message  esc back  j/k next  tab gate  ? keys",
        Route::Gate => "1-3 choose  esc back  ? keys",
        Route::Help => "esc back  q leave",
        Route::Home => "type a goal or a command  tab complete  up/down earlier lines  esc back",
        Route::Goal => "j/k move  enter inspect  s swarm  esc back  ? keys",
        Route::Evidence => "j/k move  enter inspect  s swarm  esc back  ? keys",
        Route::Retro => "j/k scroll  a all events  f follow  esc back  ? keys",
        Route::Memory => "s swarm  g goal  esc back  ? keys  q leave",
        Route::Providers => "c commands (start, stop, setup)  s swarm  esc back  ? keys",
        Route::Welcome if super::crew::plan(&f.crew.found).is_empty() => {
            "r look again  ? keys  q leave"
        }
        Route::Welcome => "enter set up this crew  r look again  c commands  q leave",
        Route::Swarm if narrow => {
            if gate_open(f) {
                "n next  b back  m more  1-3 choose  ? keys"
            } else {
                "n next  b back  m more  g goal  ? keys"
            }
        }
        Route::Swarm if ui.region == Region::Gate => {
            "tab region  enter open gate  1-3 choose  esc back  ? keys"
        }
        Route::Swarm => {
            if gate_open(f) {
                "tab region  enter inspect  1-3 gate  / filter  d depth  ? keys  q leave"
            } else {
                "enter inspect  / filter  d depth  g goal  e evidence  ? keys  q leave"
            }
        }
    };
    plain(t)
}

fn compose(f: &Frame, ui: &Ui, route: &str, body: Vec<VLine>) -> Vec<VLine> {
    let h = ui.height;
    let mut lines = vec![rail(f, ui, route)];
    lines.extend(body);
    lines.truncate(h.saturating_sub(3));
    while lines.len() < h.saturating_sub(3) {
        lines.push(blank());
    }
    lines.push(rule(ui.width));
    lines.push(keys_line(f, ui));
    lines.push(status_line(f, ui));
    lines.iter().map(|l| fit(l, ui.width)).collect()
}

// ------------------------------------------------------------------ swarm

fn spine_prefix(lasts: &[bool]) -> String {
    let mut s = String::new();
    for (i, last) in lasts.iter().enumerate() {
        if i + 1 == lasts.len() {
            s.push_str(if *last { "+-- " } else { "|-- " });
        } else {
            s.push_str(if *last { "    " } else { "|   " });
        }
    }
    s
}

/// Rows the spine can show: the contract's budgets by width.
fn row_budget(w: usize) -> usize {
    if w >= 160 {
        9
    } else if w >= 120 {
        7
    } else {
        5
    }
}

pub fn visible_agents<'a>(f: &'a Frame, ui: &Ui) -> Vec<&'a AgentView> {
    let q = ui.filter.to_lowercase();
    f.agents
        .iter()
        .filter(|a| {
            q.is_empty() || {
                let task = a
                    .task
                    .as_ref()
                    .map(|(id, t)| format!("#{id} {t}"))
                    .unwrap_or_default();
                format!("{} {} {} {} {}", a.id, a.role, a.model, a.harness, task)
                    .to_lowercase()
                    .contains(&q)
            }
        })
        .collect()
}

fn spine_height(ui: &Ui) -> usize {
    ui.height.saturating_sub(4 + 13).max(2)
}

/// Agent rows shown before the aggregate row: the width budget, bounded by height.
fn spine_budget(ui: &Ui) -> usize {
    row_budget(ui.width).min(spine_height(ui).saturating_sub(2).max(1))
}

/// True when the collapsed spine folds agents into an aggregate row.
pub fn has_aggregate(f: &Frame, ui: &Ui) -> bool {
    !ui.expanded && visible_agents(f, ui).len() > spine_budget(ui)
}

/// Selectable spine rows: agents, then the aggregate row when rows are hidden.
pub fn spine_len(f: &Frame, ui: &Ui) -> usize {
    if has_aggregate(f, ui) {
        spine_budget(ui) + 1
    } else {
        visible_agents(f, ui).len()
    }
}

fn name_end(w: usize) -> usize {
    45 + w.saturating_sub(80) / 2
}

fn agent_row(a: &AgentView, w: usize, selected: bool) -> VLine {
    let c1 = format!("{}{}", spine_prefix(&a.lasts), a.id);
    let c1w = (chars(&c1) + 2).max(12);
    let ne = name_end(w).max(c1w + 12 + 6);
    let name = match &a.task {
        Some((id, title)) => format!("#{id} {title}"),
        None => a.role.clone(),
    };
    let right_role = match a.st {
        St::Blocked => Role::Uv,
        St::Disconnected => Role::Dim,
        _ => Role::Plain,
    };
    let sw = st_width(a.st);
    sel_if(
        line(vec![
            seg(pad(&c1, c1w), Role::Plain),
            st_seg(a.st, 12),
            seg(
                pt(&name, ne.saturating_sub(c1w + sw + 1).max(4)) + " ",
                if selected { Role::Bold } else { Role::Plain },
            ),
            seg(trunc(&a.phrase, w.saturating_sub(ne)), right_role),
        ]),
        selected,
    )
}

fn readouts(f: &Frame) -> Vec<VLine> {
    let goal = match &f.goal {
        Some(t) => lv(
            "goal",
            vec![seg(format!("#{} {}", t.id, t.title), Role::Bold)],
        ),
        None => lv(
            "goal",
            vec![seg(
                "no goal yet / c, then type what you want done",
                Role::Dim,
            )],
        ),
    };
    let checks: (usize, usize) = f.results.iter().fold((0, 0), |acc, t| {
        let v = &t.result.as_ref().unwrap().validation;
        (
            acc.0 + v.iter().filter(|o| o.passed).count(),
            acc.1 + v.iter().filter(|o| !o.passed).count(),
        )
    });
    vec![
        goal,
        lvs(
            "run",
            if f.crew.configured && f.crew.error.is_none() {
                format!(
                    "{} of {} crew up / {} working / {} open tasks / {} waiting on you",
                    f.crew.running(),
                    f.crew.members.len(),
                    f.running(),
                    f.open_tasks,
                    f.reviews()
                )
            } else {
                format!(
                    "{} of {} agents running / {} open tasks / {} waiting on you",
                    f.running(),
                    f.agents.len(),
                    f.open_tasks,
                    f.reviews()
                )
            },
        ),
        cost_line(f),
        lvs(
            "evidence",
            format!(
                "{} results / {} checks passed, {} failed",
                f.results.len(),
                checks.0,
                checks.1
            ),
        ),
        needs_line(f),
    ]
}

/// What needs the operator, most urgent first: reviews, failures, blockers,
/// stalled claims. Then the work in progress and the queue.
fn needs_line(f: &Frame) -> VLine {
    let parts: Vec<String> = [
        (GateKind::Review, "to review"),
        (GateKind::Failed, "failed"),
        (GateKind::Blocker, "blocked"),
        (GateKind::Stalled, "stalled"),
    ]
    .iter()
    .filter_map(|(k, word)| {
        let n = f.count(*k);
        (n > 0).then(|| format!("{n} {word}"))
    })
    .collect();
    let rest = format!(
        "{} working, {} queued",
        f.tree
            .iter()
            .filter(|n| n.task.state == "claimed" && !n.stalled)
            .count(),
        f.queued()
    );
    if parts.is_empty() {
        lv(
            "needs you",
            vec![
                seg("nothing", Role::Ok),
                seg(format!(" / {rest}"), Role::Plain),
            ],
        )
    } else {
        lv(
            "needs you",
            vec![
                seg(parts.join(", "), Role::Gate),
                seg(format!(" / {rest}"), Role::Plain),
            ],
        )
    }
}

pub fn tokens_text(n: f64) -> String {
    if n >= 1_000_000.0 {
        format!("{:.1}M", n / 1_000_000.0)
    } else if n >= 1_000.0 {
        format!("{:.1}k", n / 1_000.0)
    } else {
        format!("{n:.0}")
    }
}

/// Cost as the CLIs reported it to the supervisor; never estimated.
fn cost_line(f: &Frame) -> VLine {
    let c = &f.crew;
    if c.cost_usd > 0.0 {
        lv(
            "cost",
            vec![seg(
                format!(
                    "${:.2} / {} tokens, as the CLIs reported them, all time",
                    c.cost_usd,
                    tokens_text(c.tokens)
                ),
                Role::Plain,
            )],
        )
    } else if c.tokens > 0.0 {
        lv(
            "cost",
            vec![seg(
                format!(
                    "{} tokens reported / no price reported by the CLIs",
                    tokens_text(c.tokens)
                ),
                Role::Plain,
            )],
        )
    } else if c.configured {
        lv(
            "cost",
            vec![
                seg("- NONE YET", Role::Dim),
                seg(" / nothing reported by the CLIs yet", Role::Plain),
            ],
        )
    } else {
        lv(
            "cost",
            vec![
                seg("? UNKNOWN", Role::Dim),
                seg(" / no aos crew, so no usage is recorded", Role::Plain),
            ],
        )
    }
}

fn current_gate(f: &Frame, ui: &Ui) -> Option<usize> {
    if f.gates.is_empty() {
        None
    } else {
        Some(ui.gate_idx % f.gates.len())
    }
}

fn confirm_line(p: &Pending) -> VLine {
    let (label, prompt) = match p.kind {
        PendingKind::Accept => ("reason", "why accept: "),
        PendingKind::Revise => ("feedback", "what to change: "),
        PendingKind::Requeue => ("reason", "why requeue (optional): "),
        PendingKind::Cancel => ("confirm", "type CANCEL to cancel the task: "),
        PendingKind::Stop => (
            "confirm",
            "type STOP to cancel the goal and its open tasks: ",
        ),
        PendingKind::Goal => {
            let goal = p.text.as_ref().map(|(_, g)| g.as_str()).unwrap_or("");
            return lv(
                "start?",
                vec![
                    seg("start this as a goal: ", Role::Plain),
                    seg(format!("\"{}\"", trunc(goal, 120)), Role::Bold),
                ],
            );
        }
        PendingKind::Trust => {
            let dir = std::env::current_dir()
                .map(|d| super::crew::Paths::show(&d))
                .unwrap_or_default();
            return lv(
                "trust?",
                vec![seg(
                    format!("agents will run commands and edit files in {dir}"),
                    Role::Gate,
                )],
            );
        }
    };
    lv(
        label,
        vec![
            seg(
                prompt,
                if matches!(p.kind, PendingKind::Cancel | PendingKind::Stop) {
                    Role::Gate
                } else {
                    Role::Bold
                },
            ),
            seg(p.typed.clone(), Role::Bold),
            seg(" ", Role::Cursor),
        ],
    )
}

fn options_line(f: &Frame, i: usize) -> VLine {
    let g = &f.gates[i];
    let opts = match g.kind {
        GateKind::Review => "[1] ACCEPT   [2] REVISE   [3] HOLD",
        GateKind::Stalled => "[1] REQUEUE   [2] HOLD   [3] CANCEL",
        GateKind::Failed | GateKind::Blocker => "[1] TYPE THE FIX   [3] HOLD",
    };
    let more = if f.gates.len() > 1 {
        format!("   {} of {}", i + 1, f.gates.len())
    } else {
        String::new()
    };
    lv(
        "options",
        vec![seg(opts, Role::Plain), seg(more, Role::Dim)],
    )
}

fn gate_strip(f: &Frame, ui: &Ui) -> Vec<VLine> {
    let w = ui.width;
    let Some(i) = current_gate(f, ui) else {
        let last = match f.events.last() {
            Some(e) => format!(
                "#{} {} / {} / {}",
                e.seq,
                e.kind,
                e.actor,
                age(Some(e.ts_ms), f.now)
            ),
            None => "none yet".into(),
        };
        return vec![
            lv("quiet", vec![seg("nothing needs you", Role::Bold)]),
            lvs("reviews", "none addressed to the operator"),
            lvs("last event", last),
            lvs("next", "g goal tree / e evidence / c commands"),
            blank(),
        ];
    };
    let g = &f.gates[i];
    let t = &g.task;
    let selected = ui.region == Region::Gate && ui.route == Route::Swarm;
    let who = t.assignee.clone().unwrap_or_else(|| "nobody".into());
    let mut lines = match g.kind {
        GateKind::Review => {
            let r = t.result.as_ref();
            let (pass, fail) = r
                .map(|r| {
                    (
                        r.validation.iter().filter(|v| v.passed).count(),
                        r.validation.iter().filter(|v| !v.passed).count(),
                    )
                })
                .unwrap_or((0, 0));
            vec![
                sel_if(
                    line(vec![
                        st_seg(St::Gate, 12),
                        seg(
                            trunc(
                                &format!("#{} {} / {}, round {}", t.id, t.title, who, t.round),
                                w - 12,
                            ),
                            Role::Bold,
                        ),
                    ]),
                    selected,
                ),
                lvs(
                    "result",
                    r.map(|r| r.summary.clone())
                        .unwrap_or_else(|| "no summary".into()),
                ),
                if pass + fail == 0 {
                    lv("checks", vec![seg("none reported", Role::Dim)])
                } else if fail > 0 {
                    lv(
                        "checks",
                        vec![
                            seg(format!("{pass} passed / "), Role::Plain),
                            seg(format!("x {fail} failed"), Role::Err),
                        ],
                    )
                } else {
                    lv(
                        "checks",
                        vec![
                            seg(format!("+ {pass} passed"), Role::Ok),
                            seg(" / 0 failed", Role::Plain),
                        ],
                    )
                },
                lvs(
                    "reversible",
                    format!("1 no: accepted stays closed / 2 yes: back to {who}"),
                ),
                options_line(f, i),
            ]
        }
        GateKind::Stalled => {
            let idle = f.now - t.updated_ms;
            let impact = if g.dependents.is_empty() {
                String::new()
            } else {
                format!(
                    " / blocks {}",
                    g.dependents
                        .iter()
                        .map(|d| format!("#{d}"))
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            };
            vec![
                sel_if(
                    line(vec![
                        st_seg(St::Blocked, 12),
                        seg(
                            trunc(
                                &format!("#{} {} / {} idle {}", t.id, t.title, who, span(idle)),
                                w - 12,
                            ),
                            Role::Bold,
                        ),
                    ]),
                    selected,
                ),
                lvs("why", format!("{}{impact}", g.reason)),
                lvs("evidence", g.evidence.clone()),
                lvs("next", format!("{} / 1 back to the pool, 3 CANCEL", g.next)),
                options_line(f, i),
            ]
        }
        GateKind::Failed | GateKind::Blocker => {
            let st = if g.kind == GateKind::Failed {
                St::Failed
            } else {
                St::Blocked
            };
            vec![
                sel_if(
                    line(vec![
                        st_seg(st, 12),
                        seg(
                            trunc(&format!("#{} {} / {}", t.id, t.title, who), w - 12),
                            Role::Bold,
                        ),
                    ]),
                    selected,
                ),
                lvs("why", g.reason.clone()),
                lvs("evidence", g.evidence.clone()),
                lv(
                    "next",
                    vec![
                        seg(g.next.clone(), Role::Bold),
                        seg(" / 1 types it", Role::Dim),
                    ],
                ),
                options_line(f, i),
            ]
        }
    };
    if let Some(p) = &ui.pending {
        lines[3] = confirm_line(p);
    }
    lines
}

pub fn swarm(f: &Frame, ui: &Ui) -> Vec<VLine> {
    if ui.width < 80 {
        return narrow(f, ui);
    }
    let w = ui.width;
    let mut body = vec![rule(w)];
    body.extend(readouts(f));
    body.push(rule(w));
    // Spine height: whatever the frame leaves after the fixed regions.
    let spine_h = spine_height(ui);
    let agents = visible_agents(f, ui);
    let mut sp: Vec<VLine> = Vec::new();
    let unread = if f.operator_unread > 0 {
        format!("{} unread for you", f.operator_unread)
    } else {
        String::new()
    };
    sp.push(line(vec![
        seg("ROOT OPR    ", Role::Plain),
        seg(pad("operator", 12), Role::Bold),
        seg(pad("", name_end(w).saturating_sub(24)), Role::Plain),
        seg(unread, Role::Plain),
    ]));
    let budget = spine_budget(ui);
    if agents.is_empty() {
        sp.push(dim(if ui.filter.is_empty() {
            "+-- no agents yet / c, then setup"
        } else {
            "+-- no agent matches the filter"
        }));
    } else if !ui.expanded && agents.len() > budget {
        for (i, a) in agents.iter().take(budget).enumerate() {
            sp.push(agent_row(a, w, ui.region == Region::Field && ui.sel == i));
        }
        let hidden = &agents[budget..];
        let running = hidden.iter().filter(|a| a.st == St::Running).count();
        let blocked = hidden.iter().filter(|a| a.st == St::Blocked).count();
        let offline = hidden.iter().filter(|a| a.st == St::Disconnected).count();
        sp.push(sel_if(
            line(vec![
                seg(
                    format!(
                        "+-- +{} hidden / {} running, {} blocked, {} offline",
                        hidden.len(),
                        running,
                        blocked,
                        offline
                    ),
                    Role::Plain,
                ),
                seg("   [d] expand", Role::Dim),
            ]),
            ui.region == Region::Field && ui.sel == budget,
        ));
    } else {
        let rows = spine_h.saturating_sub(2).max(1);
        let start = if agents.len() <= rows {
            0
        } else {
            ui.sel.saturating_sub(rows / 2).min(agents.len() - rows)
        };
        for (i, a) in agents.iter().enumerate().skip(start).take(rows) {
            sp.push(agent_row(a, w, ui.region == Region::Field && ui.sel == i));
        }
        if agents.len() > budget {
            sp.push(dim(format!(
                "rows {}-{} of {}   [d] collapse",
                start + 1,
                (start + rows).min(agents.len()),
                agents.len()
            )));
        }
    }
    sp.truncate(spine_h);
    while sp.len() < spine_h {
        sp.push(blank());
    }
    body.extend(sp);
    body.push(rule(w));
    body.extend(gate_strip(f, ui));
    let route = if ui.filter.is_empty() {
        "swarm".to_string()
    } else {
        format!("swarm / filter: {}", ui.filter)
    };
    compose(f, ui, &route, body)
}

/// Under 80 columns: one object at a time, state and next action first.
pub fn narrow_objects(f: &Frame) -> usize {
    f.gates.len() + f.agents.len()
}

fn narrow(f: &Frame, ui: &Ui) -> Vec<VLine> {
    let w = ui.width;
    let n = narrow_objects(f);
    if n == 0 {
        let body = vec![
            dim("no agents yet"),
            lvs("next", "qagent agent add <id> --role worker"),
            rule(w),
        ];
        return compose(f, ui, "swarm", body);
    }
    let i = ui.nobj % n;
    let mut body = Vec::new();
    if i < f.gates.len() {
        let g = &f.gates[i];
        let t = &g.task;
        let (st, title) = match g.kind {
            GateKind::Review => (St::Gate, "review"),
            GateKind::Stalled => (St::Blocked, "stalled"),
            GateKind::Failed => (St::Failed, "failed"),
            GateKind::Blocker => (St::Blocked, "blocked"),
        };
        body.push(line(vec![
            st_seg(st, 12),
            seg(trunc(&format!("#{} {}", t.id, t.title), w - 12), Role::Bold),
        ]));
        body.push(lvs("next", format!("{title} gate / choose below")));
        body.push(rule(w));
        body.push(lvs(
            "assignee",
            t.assignee.clone().unwrap_or_else(|| "nobody".into()),
        ));
        if g.kind == GateKind::Review {
            body.push(lvs("reviewer", reviewer_of(t)));
            body.push(lvs("round", t.round.to_string()));
        } else {
            for (k, l) in wrap(&g.reason, w - 12, 2).into_iter().enumerate() {
                body.push(lvs(if k == 0 { "why" } else { "" }, l));
            }
            body.push(lvs("next", g.next.clone()));
        }
        if let Some(r) = &t.result {
            for (k, l) in wrap(&r.summary, w - 12, if ui.more { 4 } else { 2 })
                .into_iter()
                .enumerate()
            {
                body.push(lvs(if k == 0 { "result" } else { "" }, l));
            }
            body.push(lvs("files", format!("{} changed", r.changed_files.len())));
        } else {
            body.push(lvs("idle", span(f.now - t.updated_ms)));
        }
        while body.len() < ui.height.saturating_sub(4 + 4) {
            body.push(blank());
        }
        body.push(rule(w));
        match g.kind {
            GateKind::Review => {
                body.push(line(vec![
                    seg(pad("[1] ACCEPT", 18), Role::Plain),
                    seg("irreversible / reason required", Role::Gate),
                ]));
                body.push(line(vec![
                    seg(pad("[2] REVISE", 18), Role::Plain),
                    seg("reversible / back to assignee", Role::Dim),
                ]));
                body.push(line(vec![
                    seg(pad("[3] HOLD", 18), Role::Plain),
                    seg("nothing written / next gate", Role::Dim),
                ]));
            }
            GateKind::Stalled => {
                body.push(line(vec![
                    seg(pad("[1] REQUEUE", 18), Role::Plain),
                    seg("reversible / back to the pool", Role::Dim),
                ]));
                body.push(line(vec![
                    seg(pad("[2] HOLD", 18), Role::Plain),
                    seg("nothing written / next gate", Role::Dim),
                ]));
                body.push(line(vec![
                    seg(pad("[3] CANCEL", 18), Role::Plain),
                    seg("irreversible / type CANCEL", Role::Gate),
                ]));
            }
            GateKind::Failed | GateKind::Blocker => {
                body.push(line(vec![
                    seg(pad("[1] TYPE THE FIX", 18), Role::Plain),
                    seg("opens command home with it typed", Role::Dim),
                ]));
                body.push(line(vec![
                    seg(pad("[3] HOLD", 18), Role::Plain),
                    seg("nothing written / next gate", Role::Dim),
                ]));
                body.push(blank());
            }
        }
        if let Some(p) = &ui.pending {
            let last = body.len() - 1;
            body[last] = confirm_line(p);
        }
        return compose(f, ui, &format!("swarm / #{}", t.id), body);
    }
    let a = &f.agents[i - f.gates.len()];
    body.push(line(vec![
        st_seg(a.st, 12),
        seg(trunc(&format!("{} {}", a.id, a.role), w - 12), Role::Bold),
    ]));
    body.push(lvs(
        "next",
        if f.gates.is_empty() {
            "nothing needs you here"
        } else {
            "a gate is open / n to reach it"
        },
    ));
    body.push(rule(w));
    body.extend(agent_facts(f, a, w, if ui.more { 12 } else { 9 }));
    compose(f, ui, &format!("swarm / {}", a.id), body)
}

fn agent_facts(f: &Frame, a: &AgentView, w: usize, max: usize) -> Vec<VLine> {
    let mut v = Vec::new();
    if let Some(m) = f.crew.members.iter().find(|m| m.id == a.id) {
        v.push(lvs(
            "process",
            match (m.pid, &m.last_words) {
                (Some(pid), _) => format!("running / pid {pid} / {} CLI", m.cli),
                (None, Some(last)) => trunc(&format!("stopped / last: {last}"), w - 12),
                (None, None) => "stopped / c, then start".into(),
            },
        ));
    }
    if let Some(why) = &a.paused {
        v.push(lvs(
            "paused",
            trunc(&format!("{why} / c, then resume {}", a.id), w - 12),
        ));
    }
    if let Some(b) = &a.budget {
        v.push(lvs(
            "budget",
            trunc(
                &format!(
                    "{}{}",
                    b.line(),
                    b.over().map(|_| " / used up").unwrap_or_default()
                ),
                w - 12,
            ),
        ));
    }
    v.extend([
        lvs("state", a.phrase.clone()),
        lvs("role", a.role.clone()),
        lvs("authority", a.authority.clone()),
        lvs(
            "harness",
            if a.harness.is_empty() {
                "-".into()
            } else {
                a.harness.clone()
            },
        ),
        lvs(
            "model",
            if a.model.is_empty() {
                "-".into()
            } else {
                a.model.clone()
            },
        ),
        lvs(
            "parent",
            a.parent.clone().unwrap_or_else(|| OPERATOR_ID.into()),
        ),
        lvs(
            "task",
            a.task
                .as_ref()
                .map(|(id, t)| trunc(&format!("#{id} {t}"), w - 12))
                .unwrap_or_else(|| "none claimed".into()),
        ),
        lvs("accepted", format!("{} tasks", a.accepted)),
        lvs(
            "seen",
            format!(
                "{} / stored {}",
                age(a.last_seen_ms, f.now),
                a.stored_status
            ),
        ),
        lvs("mail", format!("{} unread", a.unread)),
        lvs(
            "last event",
            f.last_event_by(&a.id)
                .map(|e| format!("#{} {} / {}", e.seq, e.kind, age(Some(e.ts_ms), f.now)))
                .unwrap_or_else(|| "none in the last 400".into()),
        ),
    ]);
    v.truncate(max);
    v
}

// ------------------------------------------------------------------ inspect and gate

fn event_line(f: &Frame, e: &crate::types::BusEvent, w: usize) -> VLine {
    let what = if e.entity == "task" {
        format!("task #{}", e.entity_id)
    } else {
        format!("{} {}", e.entity, e.entity_id)
    };
    let _ = f;
    line(vec![
        seg(pad(&format!("#{}", e.seq), 8), Role::Dim),
        seg(pad(&clock(e.ts_ms), 11), Role::Dim),
        seg(pad(&trunc(&e.actor, 10), 11), Role::Plain),
        seg(pad(&trunc(&e.kind, 24), 25), event_role(&e.kind)),
        seg(trunc(&what, w.saturating_sub(55)), Role::Plain),
    ])
}

fn event_role(kind: &str) -> Role {
    match kind {
        "task_accepted" => Role::Ok,
        "task_failed" | "task_cancelled" => Role::Err,
        "task_submitted" => Role::Gate,
        "task_claimed" => Role::Run,
        "task_changes_requested" | "task_requeued" | "task_released" => Role::Uv,
        _ => Role::Plain,
    }
}

pub fn inspect(f: &Frame, ui: &Ui) -> Vec<VLine> {
    let w = ui.width;
    let mut body = vec![rule(w)];
    match &ui.inspect {
        Some(Target::Agent(id)) => {
            let Some(a) = f.agents.iter().find(|a| &a.id == id) else {
                body.push(dim(format!("{id} is no longer on the bus")));
                return compose(f, ui, "inspect", body);
            };
            body.push(line(vec![
                st_seg(a.st, 12),
                seg(format!("{} {}", a.id, a.role), Role::Bold),
            ]));
            let next = match a.st {
                St::Blocked => "claim is stalled / 1 requeue or 3 cancel below",
                St::Disconnected => "agent is offline / c, then start to wake the crew",
                _ => "nothing needs you on this agent",
            };
            body.push(lvs("next", next));
            if let Some(t) = a
                .task
                .as_ref()
                .and_then(|(tid, _)| f.tree.iter().find(|n| n.task.id == *tid))
            {
                if let Some(mut l) = task_options_line(&t.task) {
                    l.segs
                        .push(seg(format!("  on #{}   w message", t.task.id), Role::Dim));
                    body.push(l);
                }
                if let Some(p) = ui.pending.as_ref().filter(|p| p.task_id == t.task.id) {
                    body.push(confirm_line(p));
                }
            } else {
                body.push(lvs("options", "w message"));
            }
            body.push(rule(w));
            body.extend(agent_facts(f, a, w, if ui.height >= 30 { 11 } else { 9 }));
            body.push(rule(w));
            body.push(dim("events by this agent, newest last"));
            let room = ui.height.saturating_sub(3 + 1 + body.len()).max(1);
            let evs: Vec<_> = f.events.iter().filter(|e| e.actor == a.id).collect();
            for e in evs.iter().skip(evs.len().saturating_sub(room)) {
                body.push(event_line(f, e, w));
            }
            compose(f, ui, &format!("swarm / {}", a.id), body)
        }
        Some(Target::Task(id)) => {
            let node = f.tree.iter().find(|n| n.task.id == *id);
            let Some(n) = node else {
                body.push(dim(format!("task #{id} is not in view")));
                return compose(f, ui, "inspect", body);
            };
            let t = &n.task;
            let (st, phrase) = task_state(t, n.stalled);
            body.push(line(vec![
                st_seg(st, 12),
                seg(trunc(&format!("#{} {}", t.id, t.title), w - 12), Role::Bold),
            ]));
            body.push(lvs("state", phrase));
            let before_opts = body.len();
            if let Some(l) = task_options_line(t) {
                body.push(l);
            }
            if let Some(p) = ui.pending.as_ref().filter(|p| p.task_id == t.id) {
                body.push(confirm_line(p));
            }
            let opts = body.len() - before_opts;
            body.push(rule(w));
            body.push(lvs(
                "assignee",
                t.assignee.clone().unwrap_or_else(|| "nobody".into()),
            ));
            body.push(lvs("reviewer", reviewer_of(t)));
            body.push(lvs("creator", t.creator.clone()));
            body.push(lvs(
                "parent",
                t.parent_id
                    .map(|p| format!("#{p}"))
                    .unwrap_or_else(|| "none".into()),
            ));
            body.push(lvs(
                "depends on",
                if t.dependencies.is_empty() {
                    "nothing".into()
                } else {
                    t.dependencies
                        .iter()
                        .map(|d| format!("#{d}"))
                        .collect::<Vec<_>>()
                        .join(", ")
                },
            ));
            body.push(lvs(
                "round",
                format!(
                    "{} / attempts {} of {}",
                    t.round,
                    t.attempts,
                    t.max_retries + 1
                ),
            ));
            body.push(lvs("updated", age(Some(t.updated_ms), f.now)));
            for (k, l) in wrap(&t.brief, w - 12, 2).into_iter().enumerate() {
                body.push(lvs(if k == 0 { "brief" } else { "" }, l));
            }
            if let Some(r) = &t.result {
                body.push(lvs("result", trunc(&r.summary, w - 12)));
            }
            if let Some(r) = &t.review {
                body.push(lvs(
                    "review",
                    trunc(
                        &format!(
                            "{} by {}: {}",
                            if r.accepted { "accepted" } else { "revise" },
                            r.reviewer,
                            r.feedback
                        ),
                        w - 12,
                    ),
                ));
            }
            body.truncate(opts + if ui.height >= 30 { 16 } else { 13 });
            body.push(rule(w));
            body.push(dim("events on this task, newest last"));
            let room = ui.height.saturating_sub(3 + 1 + body.len()).max(1);
            let key = t.id.to_string();
            let evs: Vec<_> = f
                .events
                .iter()
                .filter(|e| e.entity == "task" && e.entity_id == key)
                .collect();
            for e in evs.iter().skip(evs.len().saturating_sub(room)) {
                body.push(event_line(f, e, w));
            }
            compose(f, ui, &format!("task / #{}", t.id), body)
        }
        None => compose(f, ui, "inspect", body),
    }
}

pub fn gate(f: &Frame, ui: &Ui) -> Vec<VLine> {
    let w = ui.width;
    let mut body = vec![rule(w)];
    let Some(i) = current_gate(f, ui) else {
        body.push(lv("quiet", vec![seg("no gate is open", Role::Bold)]));
        return compose(f, ui, "gate", body);
    };
    let g = &f.gates[i];
    let t = &g.task;
    let st = match g.kind {
        GateKind::Review => St::Gate,
        GateKind::Failed => St::Failed,
        GateKind::Blocker | GateKind::Stalled => St::Blocked,
    };
    body.push(line(vec![
        st_seg(st, 12),
        seg(trunc(&format!("#{} {}", t.id, t.title), w - 12), Role::Bold),
    ]));
    if g.kind == GateKind::Review {
        body.push(lvs("next", "choose 1, 2 or 3 / esc returns"));
    } else {
        body.push(lvs("why", trunc(&g.reason, w - 12)));
        for (k, l) in wrap(&g.evidence, w - 12, 2).into_iter().enumerate() {
            body.push(lvs(if k == 0 { "evidence" } else { "" }, l));
        }
        body.push(lv("next", vec![seg(trunc(&g.next, w - 12), Role::Bold)]));
    }
    body.push(rule(w));
    body.push(lvs(
        "assignee",
        format!(
            "{} / reviewer {} / round {}",
            t.assignee.clone().unwrap_or_else(|| "nobody".into()),
            reviewer_of(t),
            t.round
        ),
    ));
    for (k, l) in wrap(
        if t.acceptance.is_empty() {
            &t.brief
        } else {
            &t.acceptance
        },
        w - 12,
        2,
    )
    .into_iter()
    .enumerate()
    {
        body.push(lvs(
            if k == 0 {
                if t.acceptance.is_empty() {
                    "brief"
                } else {
                    "acceptance"
                }
            } else {
                ""
            },
            l,
        ));
    }
    if let Some(r) = &t.result {
        for (k, l) in wrap(&format!("{} {}", r.summary, r.details), w - 12, 3)
            .into_iter()
            .enumerate()
        {
            body.push(lvs(if k == 0 { "result" } else { "" }, l));
        }
        let files = if r.changed_files.is_empty() {
            "none listed".to_string()
        } else {
            format!(
                "{} / {}",
                r.changed_files.len(),
                r.changed_files
                    .iter()
                    .take(3)
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        };
        body.push(lvs("files", trunc(&files, w - 12)));
        if r.validation.is_empty() {
            body.push(lv("checks", vec![seg("none reported", Role::Dim)]));
        }
        for (k, v) in r.validation.iter().take(3).enumerate() {
            let (m, role) = if v.passed {
                ("+ VERIFIED ", Role::Ok)
            } else {
                ("x FAILED   ", Role::Err)
            };
            body.push(lv(
                if k == 0 { "checks" } else { "" },
                vec![
                    seg(m, role),
                    seg(
                        trunc(
                            &format!("{} {}", v.command.clone().unwrap_or_default(), v.summary),
                            w - 23,
                        ),
                        Role::Plain,
                    ),
                ],
            ));
        }
    } else if g.kind != GateKind::Failed {
        body.push(lvs(
            "idle",
            format!(
                "{} since the last claim or note",
                span(f.now - t.updated_ms)
            ),
        ));
    }
    body.truncate(ui.height.saturating_sub(4 + 5));
    while body.len() < ui.height.saturating_sub(4 + 5) {
        body.push(blank());
    }
    body.push(rule(w));
    match g.kind {
        GateKind::Review => {
            body.push(line(vec![
                seg(pad("[1] ACCEPT", 21), Role::Plain),
                seg(
                    "irreversible / closes the task, unblocks dependents",
                    Role::Gate,
                ),
            ]));
            body.push(line(vec![
                seg(pad("[2] REVISE", 21), Role::Plain),
                seg(
                    "reversible / sends your feedback to the assignee",
                    Role::Dim,
                ),
            ]));
            body.push(line(vec![
                seg(pad("[3] HOLD", 21), Role::Plain),
                seg("nothing written / moves to the next gate", Role::Dim),
            ]));
        }
        GateKind::Stalled => {
            body.push(line(vec![
                seg(pad("[1] REQUEUE", 21), Role::Plain),
                seg("reversible / anyone may claim it again", Role::Dim),
            ]));
            body.push(line(vec![
                seg(pad("[2] HOLD", 21), Role::Plain),
                seg("nothing written / moves to the next gate", Role::Dim),
            ]));
            body.push(line(vec![
                seg(pad("[3] CANCEL", 21), Role::Plain),
                seg("irreversible / type CANCEL", Role::Gate),
            ]));
        }
        GateKind::Failed | GateKind::Blocker => {
            body.push(line(vec![
                seg(pad("[1] TYPE THE FIX", 21), Role::Plain),
                seg(
                    "opens command home with the next line typed; enter runs it",
                    Role::Dim,
                ),
            ]));
            body.push(line(vec![
                seg(pad("[3] HOLD", 21), Role::Plain),
                seg("nothing written / moves to the next gate", Role::Dim),
            ]));
            body.push(blank());
        }
    }
    body.push(match &ui.pending {
        Some(p) => confirm_line(p),
        None => blank(),
    });
    compose(f, ui, "gate", body)
}

// ------------------------------------------------------------------ routes

pub fn goal(f: &Frame, ui: &Ui) -> Vec<VLine> {
    let w = ui.width;
    let mut body = vec![rule(w)];
    match &f.goal {
        Some(t) => body.push(lv(
            "goal",
            vec![seg(
                trunc(&format!("#{} {}", t.id, t.title), w - 12),
                Role::Bold,
            )],
        )),
        None => body.push(lv("goal", vec![seg("no open top-level task", Role::Dim)])),
    }
    let closed = f.tree.len() - f.open_tasks.min(f.tree.len());
    body.push(lvs(
        "tasks",
        format!(
            "{} open / {} recently closed / tree by parent",
            f.open_tasks, closed
        ),
    ));
    body.push(rule(w));
    let rows = ui.height.saturating_sub(4 + 4);
    if f.tree.is_empty() {
        body.push(dim("no tasks yet / c, then task add <title>"));
    }
    let start = if f.tree.len() <= rows {
        0
    } else {
        ui.goal_sel
            .saturating_sub(rows / 2)
            .min(f.tree.len() - rows)
    };
    let ne = name_end(w);
    for (i, n) in f.tree.iter().enumerate().skip(start).take(rows) {
        let t = &n.task;
        let (st, phrase) = task_state(t, n.stalled);
        let c1 = format!("{}#{}", spine_prefix(&n.lasts), t.id);
        let c1w = (chars(&c1) + 2).max(10);
        let selected = i == ui.goal_sel;
        body.push(sel_if(
            line(vec![
                seg(pad(&c1, c1w), Role::Plain),
                st_seg(st, 12),
                seg(
                    pt(&t.title, ne.saturating_sub(c1w + st_width(st) + 1).max(8)) + " ",
                    if selected { Role::Bold } else { Role::Plain },
                ),
                seg(trunc(&phrase, w.saturating_sub(ne)), Role::Dim),
            ]),
            selected,
        ));
    }
    compose(f, ui, "goal", body)
}

pub fn evidence(f: &Frame, ui: &Ui) -> Vec<VLine> {
    let w = ui.width;
    let mut body = vec![rule(w)];
    body.push(lvs(
        "evidence",
        format!("{} task results / newest first", f.results.len()),
    ));
    body.push(rule(w));
    if f.results.is_empty() {
        body.push(dim(
            "no results yet / agents add them with qagent task submit",
        ));
        return compose(f, ui, "evidence", body);
    }
    let sel = ui.ev_sel.min(f.results.len() - 1);
    let list_rows = 8.min(ui.height.saturating_sub(4 + 4 + 8)).max(2);
    let start = if f.results.len() <= list_rows {
        0
    } else {
        sel.saturating_sub(list_rows / 2)
            .min(f.results.len() - list_rows)
    };
    for (i, t) in f.results.iter().enumerate().skip(start).take(list_rows) {
        let (st, _) = task_state(t, false);
        let summary = t
            .result
            .as_ref()
            .map(|r| r.summary.clone())
            .unwrap_or_default();
        body.push(sel_if(
            line(vec![
                seg(pad(&format!("#{}", t.id), 7), Role::Plain),
                st_seg(st, 12),
                seg(
                    pt(&summary, w.saturating_sub(7 + 12 + 12)) + " ",
                    if i == sel { Role::Bold } else { Role::Plain },
                ),
                seg(
                    trunc(&t.assignee.clone().unwrap_or_default(), 11),
                    Role::Dim,
                ),
            ]),
            i == sel,
        ));
    }
    if f.results.len() > list_rows {
        body.push(dim(format!(
            "{} of {} shown / j k to scroll",
            list_rows,
            f.results.len()
        )));
    }
    let t = &f.results[sel];
    let r = t.result.as_ref().unwrap();
    body.push(rule(w));
    body.push(lv(
        "dossier",
        vec![seg(
            trunc(&format!("#{} {}", t.id, t.title), w - 12),
            Role::Bold,
        )],
    ));
    body.push(lvs("claim", trunc(&r.summary, w - 12)));
    body.push(lvs(
        "provenance",
        format!(
            "{} / round {} / {}",
            t.assignee.clone().unwrap_or_default(),
            t.round,
            clock(r.completed_ms)
        ),
    ));
    body.push(lvs(
        "files",
        trunc(
            &if r.changed_files.is_empty() {
                "none listed".to_string()
            } else {
                r.changed_files.join(", ")
            },
            w - 12,
        ),
    ));
    let fails = r.validation.iter().filter(|v| !v.passed).count();
    body.push(if r.validation.is_empty() {
        lv("checks", vec![seg("none reported", Role::Dim)])
    } else if fails > 0 {
        lv(
            "checks",
            vec![seg(
                format!("x {} of {} failed", fails, r.validation.len()),
                Role::Err,
            )],
        )
    } else {
        lv(
            "checks",
            vec![seg(format!("+ {} passed", r.validation.len()), Role::Ok)],
        )
    });
    body.push(lvs(
        "review",
        match &t.review {
            Some(rv) => trunc(
                &format!(
                    "{} by {}: {}",
                    if rv.accepted { "accepted" } else { "revise" },
                    rv.reviewer,
                    rv.feedback
                ),
                w - 12,
            ),
            None => format!("waiting on {}", reviewer_of(t)),
        },
    ));
    compose(f, ui, "evidence", body)
}

pub fn memory(f: &Frame, ui: &Ui) -> Vec<VLine> {
    let body = vec![
        rule(ui.width),
        line(vec![
            st_seg(St::Unavailable, 14),
            seg("/ memory", Role::Bold),
        ]),
        lvs("cause", "ACS has no memory store yet"),
        lvs("impact", "agents keep context in task notes and mail"),
        lvs("next", "proposal P11 in AOS / esc to leave"),
    ];
    compose(f, ui, "memory", body)
}

const PIVOTAL: &[&str] = &[
    "task_created",
    "task_claimed",
    "task_submitted",
    "task_accepted",
    "task_changes_requested",
    "task_failed",
    "task_cancelled",
    "task_requeued",
    "task_released",
    "task_unblocked",
    "agent_added",
];

pub fn retro_events<'a>(f: &'a Frame, ui: &Ui) -> Vec<&'a crate::types::BusEvent> {
    f.events
        .iter()
        .filter(|e| ui.retro_all || PIVOTAL.contains(&e.kind.as_str()))
        .collect()
}

pub fn retro(f: &Frame, ui: &Ui) -> Vec<VLine> {
    let w = ui.width;
    let evs = retro_events(f, ui);
    let mut body = vec![rule(w)];
    body.push(lvs(
        "events",
        format!(
            "{} {} / last {} on the bus / newest last",
            evs.len(),
            if ui.retro_all {
                "of all kinds"
            } else {
                "pivotal"
            },
            f.events.len()
        ),
    ));
    body.push(rule(w));
    let rows = ui.height.saturating_sub(4 + 3);
    let back = if ui.follow {
        0
    } else {
        ui.retro_back.min(evs.len().saturating_sub(rows))
    };
    let end = evs.len() - back.min(evs.len());
    let start = end.saturating_sub(rows);
    if evs.is_empty() {
        body.push(dim("no events yet"));
    }
    for e in &evs[start..end] {
        body.push(event_line(f, e, w));
    }
    compose(f, ui, "retro", body)
}

fn proc_seg(m: &super::crew::MemberInfo) -> (String, Role) {
    match m.pid {
        Some(_) => seg(pad("* RUNNING", 12), Role::Run),
        None => seg(pad("- STOPPED", 12), Role::Dim),
    }
}

/// One line per CLI found, plus the crew-ready ones that are missing. A
/// crew-ready CLI reads as installed, then signed in, then ready.
fn found_lines(found: &[super::crew::Found], w: usize) -> Vec<VLine> {
    use super::crew::SignIn;
    let mut v = Vec::new();
    for f in found {
        let version = f.version.clone().unwrap_or_else(|| "installed".into());
        let (mark, role, what) = match (&f.path, f.cli.crew_ready, &f.sign_in) {
            (Some(_), true, SignIn::Found(why)) => (
                "+",
                Role::Ok,
                format!("ready / {version} / signed in: {why}"),
            ),
            (Some(_), true, SignIn::Unknown(why)) => (
                "?",
                Role::Plain,
                format!(
                    "installed, sign-in not checked ({why}) / if turns fail: {}",
                    f.cli.sign_in
                ),
            ),
            (Some(_), true, SignIn::Missing) => (
                "x",
                Role::Err,
                format!("installed, not signed in / {}", f.cli.sign_in),
            ),
            (Some(_), false, _) => (
                "~",
                Role::Dim,
                format!("{} found / can't join a crew from aos yet", f.cli.name),
            ),
            (None, true, _) => ("-", Role::Dim, format!("not installed / {}", f.cli.install)),
            (None, false, _) => continue,
        };
        v.push(line(vec![
            seg(format!("  {mark} "), role),
            seg(pad(f.cli.id, 9), Role::Bold),
            seg(trunc(&what, w.saturating_sub(13)), Role::Plain),
        ]));
    }
    v
}

pub fn crew(f: &Frame, ui: &Ui) -> Vec<VLine> {
    let w = ui.width;
    let c = &f.crew;
    let mut body = vec![rule(w)];
    if !c.configured {
        body.push(lv(
            "crew",
            vec![
                seg("- NONE YET", Role::Dim),
                seg(
                    " / c, then setup, makes one from the CLIs on this computer",
                    Role::Plain,
                ),
            ],
        ));
    } else if let Some(e) = &c.error {
        body.push(lv(
            "crew",
            vec![
                seg("x FAILED", Role::Err),
                seg(format!(" / crew.json does not load: {e}"), Role::Plain),
            ],
        ));
        body.push(lvs(
            "fix",
            "edit it, or c then setup --force to write a fresh one",
        ));
    } else {
        body.push(lvs(
            "crew",
            format!(
                "{} agents / {} running{}",
                c.members.len(),
                c.running(),
                c.workdir
                    .as_ref()
                    .map(|d| format!(" / works in {d}"))
                    .unwrap_or_default()
            ),
        ));
        body.push(rule(w));
        for m in &c.members {
            body.push(line(vec![
                seg(pad(&m.id, 11), Role::Bold),
                proc_seg(m),
                seg(pad(&m.cli, 9), Role::Plain),
                seg(trunc(&m.description, w.saturating_sub(32)), Role::Dim),
            ]));
            let view = f.agents.iter().find(|a| a.id == m.id);
            let mut notes: Vec<String> = Vec::new();
            if let Some(why) = view.and_then(|a| a.paused.as_ref()) {
                notes.push(format!("paused: {why}"));
            }
            if let Some(b) = view.and_then(|a| a.budget.as_ref()) {
                notes.push(format!("budget {}", b.line()));
            }
            if !notes.is_empty() {
                body.push(line(vec![
                    seg(pad("", 11), Role::Plain),
                    seg(trunc(&notes.join(" / "), w.saturating_sub(11)), Role::Dim),
                ]));
            }
            if let (None, Some(last)) = (m.pid, &m.last_words) {
                body.push(line(vec![
                    seg(pad("", 11), Role::Plain),
                    seg(
                        trunc(&format!("last: {last}"), w.saturating_sub(11)),
                        Role::Dim,
                    ),
                ]));
            }
        }
    }
    body.push(rule(w));
    body.push(cost_line(f));
    body.push(lvs(
        "files",
        format!("{}  crew.json  roles/  missions/", c.dir),
    ));
    body.push(lvs(
        "missions",
        trunc(
            &c.missions
                .iter()
                .map(|(n, _)| n.as_str())
                .collect::<Vec<_>>()
                .join(" "),
            w - 12,
        ),
    ));
    if !c.found.is_empty() {
        body.push(rule(w));
        body.push(dim("on this computer"));
        body.extend(found_lines(&c.found, w));
    }
    body.push(rule(w));
    body.push(dim(
        "start / stop agents / pause / resume / budget: type them in command home (c)",
    ));
    compose(f, ui, "crew", body)
}

pub fn welcome(f: &Frame, ui: &Ui) -> Vec<VLine> {
    let w = ui.width;
    let c = &f.crew;
    let mut body = vec![
        rule(w),
        line(vec![
            seg("welcome to aos", Role::Bold),
            seg(
                "  mission control for a team of AI coding agents",
                Role::Dim,
            ),
        ]),
    ];
    for l in wrap(
        "You type a task. A builder agent does it here and hands in the result with the checks it ran; a reviewer on another model checks it and accepts it or sends it back. With one model family, you are the reviewer.",
        w,
        3,
    ) {
        body.push(plain(l));
    }
    body.push(rule(w));
    body.push(dim("found on this computer"));
    if c.found.is_empty() {
        body.push(dim("  looking..."));
    }
    body.extend(found_lines(&c.found, w));
    body.push(rule(w));
    let members = super::crew::plan(&c.found);
    if members.is_empty() {
        let signed_out = c.found.iter().any(|x| {
            x.path.is_some() && x.cli.crew_ready && x.sign_in == super::crew::SignIn::Missing
        });
        body.push(line(vec![
            seg("no crew yet", Role::Bold),
            seg(
                if signed_out {
                    " / sign in to a CLI marked x above, then press r"
                } else {
                    " / install one CLI marked - above, sign in to it, then press r"
                },
                Role::Plain,
            ),
        ]));
        body.push(dim("or try a simulated team first: q, then aos demo"));
    } else {
        body.push(dim("your crew"));
        for m in &members {
            body.push(line(vec![
                seg(pad(&format!("  {}", m.id), 12), Role::Bold),
                seg(pad(m.cli.id, 9), Role::Plain),
                seg(trunc(m.description, w.saturating_sub(21)), Role::Dim),
            ]));
        }
        if !members.iter().any(|m| m.id == "reviewer") {
            body.push(line(vec![
                seg(pad("  reviewer", 12), Role::Bold),
                seg(pad("you", 9), Role::Plain),
                seg(
                    trunc(
                        "one model family installed, so each result comes to you",
                        w.saturating_sub(21),
                    ),
                    Role::Dim,
                ),
            ]));
        }
        for l in wrap("Agents work in the folder you start aos in, and can run commands and edit files there.", w, 2) {
            body.push(line(vec![seg(l, Role::Gate)]));
        }
    }
    compose(f, ui, "welcome", body)
}

pub fn help(f: &Frame, ui: &Ui) -> Vec<VLine> {
    let keys = [
        ("j k  arrows", "move in the list or tree"),
        ("tab", "move between the spine and the gate"),
        ("enter", "inspect, or open a gate (never approves)"),
        ("esc", "close detail, cancel, back one level"),
        ("1 2 3", "choose an option on a gate or task; each confirms"),
        ("w", "write to the selected agent (opens home)"),
        ("/", "filter the spine (never changes the bus)"),
        ("d", "expand or collapse hidden agents"),
        ("f", "follow the newest events"),
        ("g s e m r", "goal, swarm, evidence, memory, retro"),
        ("c", "command home: type a goal, start, stop, send, reply"),
        ("p", "crew: who is on the team, running or stopped"),
        ("q  ctrl-c", "leave aos (agents keep running)"),
        ("n b m", "under 80 columns: next, back, more"),
    ];
    let mut body = vec![
        rule(ui.width),
        line(vec![
            seg("keys", Role::Bold),
            seg("  every write is one bus event, same as qagent", Role::Dim),
        ]),
        rule(ui.width),
    ];
    for (k, d) in keys {
        body.push(line(vec![seg(pad(k, 14), Role::Plain), seg(d, Role::Dim)]));
    }
    compose(f, ui, "keys", body)
}

/// Command-home words, for the suggestions under the prompt and for tab.
/// (word, how to use it, what it does). Missions are added from their files.
pub const COMMANDS: &[(&str, &str, &str)] = &[
    ("help", "help", "every command on one screen"),
    (
        "resume",
        "resume [agent|all]",
        "carry on: start the crew, or lift a pause",
    ),
    ("history", "history", "your past goals and how each ended"),
    ("start", "start [agent]", "start the crew in this folder"),
    (
        "stop",
        "stop agents | stop [#]",
        "stop the crew, or a goal and all under it",
    ),
    (
        "pause",
        "pause <agent|all> [why]",
        "no new turns until you resume it",
    ),
    (
        "budget",
        "budget <agent|all> 20 turns 60 min",
        "limits per agent; off clears",
    ),
    ("status", "status", "agents, open tasks, reviews, stalls"),
    ("gate", "gate", "open the result waiting for your decision"),
    ("accept", "accept # <reason>", "accept a result"),
    (
        "revise",
        "revise # <feedback>",
        "send a result back for changes",
    ),
    ("send", "send <agent|all> <message>", "write to an agent"),
    ("reply", "reply <msg#> <text>", "answer a message"),
    ("read", "read", "mark the operator's mail read"),
    ("missions", "missions", "list mission templates"),
    (
        "setup",
        "setup [--force]",
        "find agent CLIs and write your crew",
    ),
    ("doctor", "doctor", "check everything and say what to fix"),
    ("swarm", "swarm", "the live view of agents and work"),
    ("goal", "goal", "the open goal as a tree"),
    ("evidence", "evidence", "submitted results"),
    ("crew", "crew", "who is on the team, running or stopped"),
    ("retro", "retro", "the event log"),
];

/// Up to `max` commands and missions whose word starts with what is typed.
/// Only while the first word is still being typed; a leading / is allowed.
pub fn suggestions(f: &Frame, typed: &str, max: usize) -> Vec<(String, String)> {
    let t = typed.trim_start().trim_start_matches('/');
    if typed.trim().is_empty() || t.contains(char::is_whitespace) {
        return Vec::new();
    }
    let t = t.to_lowercase();
    let mut out: Vec<(String, String)> = Vec::new();
    for (name, summary) in &f.crew.missions {
        if name.starts_with(&t) && name != "run" {
            out.push((format!("{name} <what>"), summary.clone()));
        }
    }
    for (word, usage, what) in COMMANDS {
        if word.starts_with(&t) && !out.iter().any(|(u, _)| u.split(' ').next() == Some(word)) {
            out.push((usage.to_string(), what.to_string()));
        }
    }
    out.truncate(max);
    out
}

pub fn home(f: &Frame, ui: &Ui) -> Vec<VLine> {
    let w = ui.width;
    let mut body = vec![rule(w)];
    body.push(lvs(
        "mail",
        format!(
            "{} unread for the operator / latest {} shown",
            f.operator_unread,
            f.mail.len().min(5)
        ),
    ));
    for m in f
        .mail
        .iter()
        .rev()
        .take(5)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
    {
        body.push(line(vec![
            seg(pad(&format!("#{}", m.seq), 8), Role::Dim),
            seg(pad(&trunc(&m.sender, 10), 11), Role::Plain),
            seg(
                trunc(&format!("{} / {}", m.subject, m.body), w.saturating_sub(19)),
                Role::Plain,
            ),
        ]));
    }
    if f.mail.is_empty() {
        body.push(dim("no mail for the operator"));
    }
    body.push(rule(w));
    let hints = if ui.pending.is_none() {
        suggestions(f, &ui.prompt, 5)
    } else {
        Vec::new()
    };
    let room = ui.height.saturating_sub(4 + body.len() + 1 + hints.len());
    let out: Vec<_> = ui
        .out
        .iter()
        .skip(ui.out.len().saturating_sub(room))
        .cloned()
        .collect();
    for segs in out {
        body.push(line(segs));
    }
    while body.len() < ui.height.saturating_sub(4 + 1 + hints.len()) {
        body.push(blank());
    }
    for (i, (usage, what)) in hints.iter().enumerate() {
        body.push(line(vec![
            seg(if i == 0 { "tab " } else { "    " }, Role::Dim),
            seg(pad(usage, 28), Role::Plain),
            seg(trunc(what, w.saturating_sub(32)), Role::Dim),
        ]));
    }
    body.push(match &ui.pending {
        Some(p) => confirm_line(p),
        None => line(vec![
            seg("> ", Role::Run),
            seg(ui.prompt.clone(), Role::Bold),
            seg(" ", Role::Cursor),
        ]),
    });
    compose(f, ui, "home", body)
}

pub fn too_small(ui: &Ui) -> Vec<VLine> {
    let mut v = vec![
        plain("AOS > <"),
        plain(format!(
            "needs at least 60x20 / now {}x{}",
            ui.width, ui.height
        )),
        dim("resize the terminal, or q to leave"),
    ];
    v.truncate(ui.height);
    v.iter().map(|l| fit(l, ui.width)).collect()
}

/// The whole screen for the current route.
pub fn screen(f: &Frame, ui: &Ui) -> Vec<VLine> {
    if ui.width < 60 || ui.height < 20 {
        return too_small(ui);
    }
    match ui.route {
        Route::Swarm => swarm(f, ui),
        Route::Inspect => inspect(f, ui),
        Route::Gate => gate(f, ui),
        Route::Goal => goal(f, ui),
        Route::Evidence => evidence(f, ui),
        Route::Memory => memory(f, ui),
        Route::Retro => retro(f, ui),
        Route::Providers => crew(f, ui),
        Route::Welcome => welcome(f, ui),
        Route::Help => help(f, ui),
        Route::Home => home(f, ui),
    }
}
