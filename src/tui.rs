use crate::{
    Result,
    cli::{Options, runtime},
    gateway::{Connection, Gateway, Kind, Prepared, contexts, summary},
    policy::{Action, Approval},
    safety,
    themes::{self, Theme},
};
use crossterm::{
    cursor,
    event::{
        self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind, KeyModifiers,
        MouseEventKind,
    },
    execute, queue,
    style::{self, Color},
    terminal::{
        self, ClearType, DisableLineWrap, EnableLineWrap, EnterAlternateScreen,
        LeaveAlternateScreen,
    },
};
use serde_json::Value;
use std::{
    io::{self, IsTerminal, Write},
    sync::mpsc::{self, Receiver, SyncSender},
    time::{Duration, Instant},
};

struct Terminal;
impl Terminal {
    fn enter() -> Result<Self> {
        if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
            return Err("interactive mode needs a terminal; use list/get --json or --help".into());
        }
        terminal::enable_raw_mode().map_err(|_| "cannot enable raw mode")?;
        if execute!(
            io::stdout(),
            EnterAlternateScreen,
            DisableLineWrap,
            EnableMouseCapture,
            cursor::Hide
        )
        .is_err()
        {
            let _ = terminal::disable_raw_mode();
            return Err("cannot enter alternate screen".into());
        }
        std::panic::set_hook(Box::new(|_| {
            let _ = terminal::disable_raw_mode();
            let _ = execute!(
                io::stdout(),
                LeaveAlternateScreen,
                EnableLineWrap,
                DisableMouseCapture,
                cursor::Show,
                style::ResetColor
            );
            eprintln!("K8Borg encountered an internal error; no object contents recorded.");
        }));
        Ok(Self)
    }
}
impl Drop for Terminal {
    fn drop(&mut self) {
        let _ = terminal::disable_raw_mode();
        let _ = execute!(
            io::stdout(),
            LeaveAlternateScreen,
            EnableLineWrap,
            DisableMouseCapture,
            cursor::Show,
            style::ResetColor
        );
    }
}

enum Task {
    Connect(Connection),
    List(String, String),
    Get(String, String, String),
    Logs(String, String, bool),
    Discover,
    Prepare(String, String, String, Action),
    Execute(Box<Prepared>, Approval),
}
enum Reply {
    Connected(String, String, String, Vec<Kind>),
    Items(Value),
    Detail(String),
    Catalog(Vec<Kind>),
    Prepared(Box<Prepared>),
    Notice(String),
    Failure(String),
}
struct Worker {
    tx: SyncSender<(u64, Task)>,
    rx: Receiver<(u64, Reply)>,
}
fn worker() -> Worker {
    let (tx, commands) = mpsc::sync_channel::<(u64, Task)>(4);
    let (results, rx) = mpsc::sync_channel(4);
    std::thread::spawn(move || {
        let rt = match runtime() {
            Ok(rt) => rt,
            Err(e) => {
                let _ = results.send((0, Reply::Failure(e)));
                return;
            }
        };
        let mut gateway: Option<Gateway> = None;
        while let Ok((epoch, task)) = commands.recv() {
            let reply=rt.block_on(async {
                match task {
                    Task::Connect(options)=>{gateway=None;match tokio::time::timeout(Duration::from_secs(15),Gateway::connect(options)).await {
                        Ok(Ok(g))=>{let reply=Reply::Connected(g.context.clone(),g.server.clone(),g.namespace.clone(),g.kinds.clone());gateway=Some(g);reply},
                        Ok(Err(e))=>Reply::Failure(e),Err(_)=>Reply::Failure("connection timeout; select a context or retry".into()),
                    }},
                    task=>{
                        let Some(g)=gateway.as_mut() else{return Reply::Failure("not connected; select a context or retry".into());};
                        let future=async {
                            Ok(match task {
                                Task::List(k,n)=>Reply::Items(g.list(&k,&n).await?),
                                Task::Get(k,n,name)=>Reply::Detail(serde_json::to_string_pretty(&g.get(&k,&n,&name).await?).map_err(|_|"serialization failed")?),
                                Task::Logs(n,name,previous)=>Reply::Detail(g.logs(&n,&name,None,previous).await?),
                                Task::Discover=>Reply::Catalog(g.discover().await?),
                                Task::Prepare(k,n,name,a)=>Reply::Prepared(Box::new(g.prepare(&k,&n,&name,a).await?)),
                                Task::Execute(p,a)=>Reply::Notice(g.execute(*p,a).await?),
                                Task::Connect(_)=>return Err("invalid worker task".into()),
                            })
                        };
                        // Execution has its own timeout and must always finish its audit recording.
                        match tokio::time::timeout(Duration::from_secs(25),future).await {
                            Ok(Ok(reply))=>reply,Ok(Err(e))=>Reply::Failure(e),Err(_)=>Reply::Failure("request timed out; data STALE, mutation outcome potentially UNKNOWN".into()),
                        }
                    }
                }
            });
            if results.send((epoch, reply)).is_err() {
                break;
            }
        }
    });
    Worker { tx, rx }
}

enum Modal {
    None,
    Input(&'static str, String),
    Detail(String, usize),
    Picker(&'static str, Vec<String>, usize),
    Themes(Vec<Theme>, usize, Theme),
    Confirm(Box<Prepared>, String),
    Scale(String, String, String),
    Help(usize),
}
struct App {
    options: Options,
    theme: Theme,
    context: String,
    server: String,
    namespace: String,
    kinds: Vec<Kind>,
    kind_index: usize,
    items: Vec<Value>,
    selected: usize,
    filter: String,
    left_focus: bool,
    status: String,
    stale: bool,
    truncated: bool,
    modal: Modal,
    epoch: u64,
    busy: bool,
    write_inflight: bool,
    connected: bool,
    refresh: bool,
    last_refresh: Instant,
    worker: Worker,
}
impl App {
    fn kind(&self) -> String {
        self.kinds
            .get(self.kind_index)
            .map(Kind::key)
            .unwrap_or_else(|| "pods".into())
    }
    fn visible(&self) -> Vec<&Value> {
        let filter = self.filter.to_ascii_lowercase();
        self.items
            .iter()
            .filter(|v| {
                v["metadata"]["name"]
                    .as_str()
                    .unwrap_or("")
                    .to_ascii_lowercase()
                    .contains(&filter)
                    || v["metadata"]["namespace"]
                        .as_str()
                        .unwrap_or("")
                        .to_ascii_lowercase()
                        .contains(&filter)
            })
            .collect()
    }
    fn selected_target(&self) -> Option<(String, String, String)> {
        self.visible().get(self.selected).map(|v| {
            (
                self.kind(),
                v["metadata"]["namespace"]
                    .as_str()
                    .unwrap_or(&self.namespace)
                    .into(),
                v["metadata"]["name"].as_str().unwrap_or("").into(),
            )
        })
    }
    fn send(&mut self, task: Task) {
        if self.busy {
            self.status = "Busy; wait for the current request.".into();
            return;
        }
        let writes = matches!(&task, Task::Execute(..));
        match self.worker.tx.try_send((self.epoch, task)) {
            Ok(()) => {
                self.busy = true;
                self.write_inflight = writes;
            }
            Err(_) => self.status = "Request queue is full/closed.".into(),
        }
    }
    fn changed(&mut self) {
        self.epoch += 1;
        self.items.clear();
        self.selected = 0;
        self.stale = true;
        self.refresh = true;
    }
    fn connect(&mut self) {
        self.changed();
        self.connected = false;
        self.server = "connecting...".into();
        self.send(Task::Connect(self.options.connection.clone()));
    }
    fn receive(&mut self) -> bool {
        let mut changed = false;
        while let Ok((epoch, reply)) = self.worker.rx.try_recv() {
            changed = true;
            self.busy = false;
            self.write_inflight = false;
            if epoch != self.epoch && !matches!(&reply, Reply::Connected(..)) {
                continue;
            }
            match reply {
                Reply::Connected(c, s, n, k) => {
                    self.context = c;
                    self.server = s;
                    self.namespace = n;
                    self.kinds = k;
                    self.connected = true;
                    self.refresh = true;
                    self.status = "Connected. READ ONLY unless explicitly enabled.".into();
                }
                Reply::Items(value) => {
                    self.items = value["items"].as_array().cloned().unwrap_or_default();
                    self.truncated = value["truncated"] == true;
                    self.selected = self.selected.min(self.visible().len().saturating_sub(1));
                    self.stale = false;
                    self.status = format!(
                        "{} objects | cached filter | refresh every 5s",
                        self.items.len()
                    );
                }
                Reply::Detail(text) => self.modal = Modal::Detail(text, 0),
                Reply::Catalog(k) => {
                    self.kinds = k;
                    self.kind_index = 0;
                    self.changed();
                    self.status = "API discovery complete (Secrets excluded).".into();
                }
                Reply::Prepared(p) => self.modal = Modal::Confirm(p, String::new()),
                Reply::Notice(text) => {
                    self.modal = Modal::None;
                    self.status = text;
                    self.refresh = true;
                }
                Reply::Failure(error) => {
                    self.stale = true;
                    if error.contains("API 401") || error.contains("API 403") {
                        self.items.clear();
                        self.modal = Modal::None;
                    }
                    self.status = error;
                }
            }
        }
        changed
    }
    fn theme_picker(&mut self) -> Result<()> {
        let all = themes::all()?;
        let index = all.iter().position(|t| t.id == self.theme.id).unwrap_or(0);
        self.modal = Modal::Themes(all, index, self.theme.clone());
        Ok(())
    }
    fn prepare(&mut self, action: Action) {
        if let Some((k, n, name)) = self.selected_target() {
            self.send(Task::Prepare(k, n, name, action));
        }
    }
    fn key(&mut self, code: KeyCode) -> Result<bool> {
        if self.write_inflight {
            self.status =
                "Mutation in progress; wait for the result before navigating or quitting.".into();
            return Ok(false);
        }
        let modal = std::mem::replace(&mut self.modal, Modal::None);
        match modal {
            Modal::Input(label, mut text) => match code {
                KeyCode::Esc => {}
                KeyCode::Enter => {
                    if label == "Filter" {
                        self.filter = text;
                        self.selected = 0;
                    } else if label == "Namespace" {
                        if text == "*" || safety::valid_id(&text) {
                            self.namespace = text;
                            self.changed();
                        } else {
                            self.status = "Invalid namespace.".into();
                        }
                    }
                }
                KeyCode::Backspace => {
                    text.pop();
                    self.modal = Modal::Input(label, text);
                }
                KeyCode::Char(c) => {
                    if !c.is_control() && text.len() < 100 {
                        text.push(c);
                    }
                    self.modal = Modal::Input(label, text);
                }
                _ => self.modal = Modal::Input(label, text),
            },
            Modal::Detail(text, mut offset) => match code {
                KeyCode::Esc | KeyCode::Char('q') => {}
                KeyCode::Down | KeyCode::Char('j') => {
                    offset = (offset + 1).min(text.lines().count().saturating_sub(1));
                    self.modal = Modal::Detail(text, offset);
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    self.modal = Modal::Detail(text, offset.saturating_sub(1));
                }
                KeyCode::PageDown => self.modal = Modal::Detail(text, (offset + 15).min(10000)),
                KeyCode::PageUp => self.modal = Modal::Detail(text, offset.saturating_sub(15)),
                _ => self.modal = Modal::Detail(text, offset),
            },
            Modal::Picker(label, list, mut index) => match code {
                KeyCode::Esc => {}
                KeyCode::Down | KeyCode::Char('j') => {
                    index = (index + 1).min(list.len().saturating_sub(1));
                    self.modal = Modal::Picker(label, list, index);
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    self.modal = Modal::Picker(label, list, index.saturating_sub(1))
                }
                KeyCode::Enter => {
                    if let Some(value) = list.get(index) {
                        if label == "Context" {
                            if !self.busy {
                                self.options.connection.context = Some(value.clone());
                                self.connect();
                            } else {
                                self.status =
                                    "Wait for the current request before changing context.".into();
                            }
                        } else {
                            self.namespace = value.clone();
                            self.changed();
                        }
                    }
                }
                _ => self.modal = Modal::Picker(label, list, index),
            },
            Modal::Themes(list, mut index, old) => match code {
                KeyCode::Esc => self.theme = old,
                KeyCode::Down | KeyCode::Char('j') => {
                    index = (index + 1).min(list.len().saturating_sub(1));
                    if let Some(t) = list.get(index) {
                        self.theme = t.clone();
                    }
                    self.modal = Modal::Themes(list, index, old);
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    index = index.saturating_sub(1);
                    if let Some(t) = list.get(index) {
                        self.theme = t.clone();
                    }
                    self.modal = Modal::Themes(list, index, old);
                }
                KeyCode::Enter => {
                    themes::use_theme(&self.theme.id)?;
                    self.status = format!("Theme saved: {}", self.theme.name);
                }
                _ => self.modal = Modal::Themes(list, index, old),
            },
            Modal::Confirm(p, mut text) => match code {
                KeyCode::Esc => self.status = "Operation canceled.".into(),
                KeyCode::Enter => {
                    let (width, height) = terminal::size().map_err(|_| "terminal size failed")?;
                    if confirmation_lines(&p.summary, &text, width).len()
                        > height.saturating_sub(5) as usize
                    {
                        self.status="Enlarge terminal to inspect the complete confirmation before approving.".into();
                        self.modal = Modal::Confirm(p, text);
                        return Ok(false);
                    }
                    match Approval::confirm(p.target.clone(), p.action.clone(), &text) {
                        Ok(a) => self.send(Task::Execute(p, a)),
                        Err(e) => {
                            self.status = e;
                            self.modal = Modal::Confirm(p, text);
                        }
                    }
                }
                KeyCode::Backspace => {
                    text.pop();
                    self.modal = Modal::Confirm(p, text);
                }
                KeyCode::Char(c) => {
                    if c.is_ascii_graphic() && text.len() < 320 {
                        text.push(c);
                    }
                    self.modal = Modal::Confirm(p, text);
                }
                _ => self.modal = Modal::Confirm(p, text),
            },
            Modal::Scale(k, n, mut text) => match code {
                KeyCode::Esc => {}
                KeyCode::Enter => {
                    if let Ok(replicas) = text.parse() {
                        if let Some((_, _, name)) = self.selected_target() {
                            self.send(Task::Prepare(k, n, name, Action::Scale(replicas)));
                        }
                    } else {
                        self.status = "Enter replicas 0..20".into();
                    }
                }
                KeyCode::Char(c) if c.is_ascii_digit() && text.len() < 2 => {
                    text.push(c);
                    self.modal = Modal::Scale(k, n, text);
                }
                KeyCode::Backspace => {
                    text.pop();
                    self.modal = Modal::Scale(k, n, text);
                }
                _ => self.modal = Modal::Scale(k, n, text),
            },
            Modal::Help(offset) => match code {
                KeyCode::Esc | KeyCode::Char('?') | KeyCode::Char('q') => {}
                KeyCode::PageDown => self.modal = Modal::Help((offset + 15).min(40)),
                KeyCode::PageUp => self.modal = Modal::Help(offset.saturating_sub(15)),
                _ => self.modal = Modal::Help(offset),
            },
            Modal::None => match code {
                KeyCode::Char('q') => return Ok(true),
                KeyCode::Esc => {
                    self.filter.clear();
                    self.selected = 0;
                }
                KeyCode::Tab => self.left_focus = !self.left_focus,
                KeyCode::Down | KeyCode::Char('j') => {
                    if self.left_focus {
                        self.kind_index =
                            (self.kind_index + 1).min(self.kinds.len().saturating_sub(1));
                        self.changed();
                    } else {
                        self.selected =
                            (self.selected + 1).min(self.visible().len().saturating_sub(1));
                    }
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    if self.left_focus {
                        self.kind_index = self.kind_index.saturating_sub(1);
                        self.changed();
                    } else {
                        self.selected = self.selected.saturating_sub(1);
                    }
                }
                KeyCode::Enter => {
                    if self.left_focus {
                        self.left_focus = false;
                    } else if let Some((k, n, name)) = self.selected_target() {
                        self.send(Task::Get(k, n, name));
                    }
                }
                KeyCode::Char('/') => self.modal = Modal::Input("Filter", self.filter.clone()),
                KeyCode::Char('n') => {
                    self.modal = Modal::Input("Namespace", self.namespace.clone())
                }
                KeyCode::Char('c') => {
                    let list = contexts(&self.options.connection)?;
                    let index = list.iter().position(|c| c == &self.context).unwrap_or(0);
                    self.modal = Modal::Picker("Context", list, index);
                }
                KeyCode::Char('t') => self.theme_picker()?,
                KeyCode::Char('?') => self.modal = Modal::Help(0),
                KeyCode::Char('r') => {
                    if self.connected {
                        self.refresh = true;
                    } else if !self.busy {
                        self.connect();
                    }
                }
                KeyCode::Char('R') => self.send(Task::Discover),
                KeyCode::Char('l') | KeyCode::Char('L') => {
                    if let Some((k, n, name)) = self.selected_target() {
                        if k == "pods" {
                            self.send(Task::Logs(n, name, code == KeyCode::Char('L')));
                        } else {
                            self.status = "Logs apply to Pods; choose a pod.".into();
                        }
                    }
                }
                KeyCode::Char('s') => {
                    if let Some((k, n, _)) = self.selected_target() {
                        self.modal = Modal::Scale(k, n, String::new());
                    }
                }
                KeyCode::Char('x') => self.prepare(Action::Restart),
                KeyCode::Delete => self.prepare(Action::Delete),
                _ => {}
            },
        }
        Ok(false)
    }
}

fn display(text: &str, width: usize) -> String {
    safety::safe_text(text, width)
        .chars()
        .map(|c| if c.is_ascii() { c } else { '?' })
        .take(width)
        .collect()
}
fn row(
    out: &mut impl Write,
    y: u16,
    width: u16,
    text: &str,
    fg: Color,
    bg: Color,
) -> io::Result<()> {
    queue!(
        out,
        cursor::MoveTo(0, y),
        style::SetForegroundColor(fg),
        style::SetBackgroundColor(bg),
        style::Print(format!(
            "{:<width$}",
            display(text, width as usize),
            width = width as usize
        ))
    )
}
fn draw(app: &App) -> Result<()> {
    let (w, h) = terminal::size().map_err(|_| "cannot read terminal size")?;
    let mut out = io::stdout().lock();
    let t = &app.theme;
    let color = |role: &str| {
        if app.options.color && !t.terminal {
            t.get(role).color()
        } else {
            Color::Reset
        }
    };
    let bg = color("background");
    let fg = color("text");
    queue!(
        out,
        style::SetBackgroundColor(bg),
        terminal::Clear(ClearType::All)
    )
    .map_err(|_| "draw failed")?;
    if w < 50 || h < 12 {
        row(
            &mut out,
            0,
            w,
            "K8Borg: enlarge terminal to at least 50x12 (q to quit)",
            fg,
            bg,
        )
        .map_err(|_| "draw failed")?;
        out.flush().map_err(|_| "draw failed")?;
        return Ok(());
    }
    let mode = if app.options.connection.demo {
        "DEMO / SAMPLE DATA"
    } else if app.options.connection.writable {
        "PRODUCTION / WRITE ENABLED"
    } else {
        "PRODUCTION / READ ONLY"
    };
    let header = format!(" K8Borg >_  | {mode} | {}", app.theme.name);
    row(
        &mut out,
        0,
        w,
        &header,
        color("status_text"),
        color("status_background"),
    )
    .map_err(|_| "draw failed")?;
    row(
        &mut out,
        1,
        w,
        &format!(
            " context: {} | ns: {} | {}",
            app.context, app.namespace, app.server
        ),
        color("accent"),
        bg,
    )
    .map_err(|_| "draw failed")?;
    row(
        &mut out,
        2,
        w,
        &format!(
            " [c] contexts [n] namespace [t] themes [?] help | {}{}",
            if app.busy { "LOADING " } else { "" },
            if app.stale { "STALE" } else { "" }
        ),
        color("muted"),
        bg,
    )
    .map_err(|_| "draw failed")?;
    let height = h.saturating_sub(7) as usize;
    let left = w.min(28) as usize;
    row(
        &mut out,
        3,
        w,
        &format!(
            " {:<left$}| NAMESPACE       NAME / STATUS",
            "RESOURCES (Tab)",
            left = left - 1
        ),
        color("muted"),
        bg,
    )
    .map_err(|_| "draw failed")?;
    let visible = app.visible();
    let start = app.selected.saturating_sub(height.saturating_sub(1));
    let kstart = app.kind_index.saturating_sub(height.saturating_sub(1));
    for r in 0..height {
        let kind = app.kinds.get(kstart + r).map(Kind::key).unwrap_or_default();
        let marker = if app.kind_index == kstart + r {
            if app.left_focus { ">" } else { "*" }
        } else {
            " "
        };
        let mut line = format!(
            "{marker}{:<width$}|",
            display(&kind, left - 2),
            width = left - 2
        );
        if let Some(object) = visible.get(start + r) {
            let namespace = object["metadata"]["namespace"].as_str().unwrap_or("-");
            let name = object["metadata"]["name"].as_str().unwrap_or("-");
            line += &format!(
                " {:<15} {}  [{}]",
                display(namespace, 15),
                name,
                summary(object)
            );
        }
        let selected = !app.left_focus && start + r == app.selected && r + start < visible.len();
        row(
            &mut out,
            4 + r as u16,
            w,
            &line,
            if selected {
                color("selection_text")
            } else {
                fg
            },
            if selected {
                color("selection_background")
            } else {
                bg
            },
        )
        .map_err(|_| "draw failed")?;
    }
    row(
        &mut out,
        h - 3,
        w,
        &format!(
            " {}{} | filter: {}",
            app.status,
            if app.truncated { " [TRUNCATED]" } else { "" },
            app.filter
        ),
        if app.stale {
            color("warning")
        } else {
            color("muted")
        },
        bg,
    )
    .map_err(|_| "draw failed")?;
    row(
        &mut out,
        h - 2,
        w,
        " Enter details | l logs | / filter | r refresh | s scale | x restart | Del delete",
        color("accent"),
        bg,
    )
    .map_err(|_| "draw failed")?;
    row(
        &mut out,
        h - 1,
        w,
        " q quit | AI / plugins / exec / forwarding disabled | No telemetry",
        color("muted"),
        bg,
    )
    .map_err(|_| "draw failed")?;
    match &app.modal {
        Modal::None => {}
        modal => {
            for y in 3..h - 2 {
                row(&mut out, y, w, "", fg, bg).map_err(|_| "draw failed")?;
            }
            let lines = match modal {
                Modal::Input(label, text) => vec![
                    format!(" {label}: {text}_"),
                    " Enter apply | Esc cancel".into(),
                ],
                Modal::Detail(text, offset) => {
                    std::iter::once(" DETAILS / LOGS — PgUp/PgDown scroll; Esc close".into())
                        .chain(text.lines().skip(*offset).map(|s| format!(" {s}")))
                        .collect()
                }
                Modal::Picker(label, list, index) => {
                    std::iter::once(format!(" {label} — arrows / Enter select / Esc cancel"))
                        .chain(
                            list.iter()
                                .enumerate()
                                .skip(index.saturating_sub(height.saturating_sub(2)))
                                .map(|(i, s)| {
                                    format!(" {} {s}", if i == *index { ">" } else { " " })
                                }),
                        )
                        .collect()
                }
                Modal::Themes(list, index, _) => {
                    let mut lines = vec![
                        " THEMES — arrows preview; Enter save; Esc restore".into(),
                        " PREVIEW: PRODUCTION | READ ONLY | STALE | ERROR | +added -removed".into(),
                    ];
                    lines.extend(
                        list.iter()
                            .enumerate()
                            .skip(index.saturating_sub(height.saturating_sub(3)))
                            .map(|(i, t)| {
                                format!(
                                    " {} {} ({})",
                                    if i == *index { ">" } else { " " },
                                    t.name,
                                    t.id
                                )
                            }),
                    );
                    lines
                }
                Modal::Confirm(p, text) => confirmation_lines(&p.summary, text, w),
                Modal::Scale(_, _, text) => vec![
                    format!(" SCALE DEPLOYMENT — enter replicas 0..20: {text}_"),
                    " Enter server dry-run/review | Esc cancel".into(),
                ],
                Modal::Help(offset) => {
                    std::iter::once(" HELP - PgUp/PgDown scroll; Esc close".into())
                        .chain(
                            include_str!("../docs/CLI_HELP.txt")
                                .lines()
                                .skip(*offset)
                                .map(|s| format!(" {s}")),
                        )
                        .collect()
                }
                Modal::None => vec![],
            };
            for (index, line) in lines.iter().take((h - 5) as usize).enumerate() {
                row(
                    &mut out,
                    3 + index as u16,
                    w,
                    line,
                    if matches!(modal, Modal::Confirm(..)) {
                        color("warning")
                    } else {
                        fg
                    },
                    bg,
                )
                .map_err(|_| "draw failed")?;
            }
        }
    }
    out.flush().map_err(|_| "draw failed".into())
}

fn confirmation_lines(summary: &str, text: &str, width: u16) -> Vec<String> {
    let content = format!("{summary}\nCONFIRM > {text}_\nEnter execute | Esc cancel");
    let available = usize::from(width).saturating_sub(2).max(1);
    content
        .lines()
        .flat_map(|line| {
            let chars: Vec<char> = line.chars().collect();
            if chars.is_empty() {
                return vec![" ".into()];
            }
            chars
                .chunks(available)
                .map(|chunk| format!(" {}", chunk.iter().collect::<String>()))
                .collect()
        })
        .collect()
}

pub fn run(options: Options) -> Result<()> {
    let first_run = options.theme.is_none() && themes::first_run()?;
    let theme = themes::find(options.theme.as_deref().unwrap_or(&themes::selected()))?;
    let _terminal = Terminal::enter()?;
    if options.splash && themes::animation_enabled() && !startup_animation(&theme, options.color)? {
        return Ok(());
    }
    let mut app = App {
        namespace: options.connection.namespace.clone(),
        context: options
            .connection
            .context
            .clone()
            .unwrap_or_else(|| "selected kubeconfig context".into()),
        server: "connecting...".into(),
        theme,
        options,
        kinds: crate::gateway::kinds(),
        kind_index: 0,
        items: vec![],
        selected: 0,
        filter: String::new(),
        left_focus: false,
        status: "Starting...".into(),
        stale: true,
        truncated: false,
        modal: Modal::None,
        epoch: 0,
        busy: false,
        write_inflight: false,
        connected: false,
        refresh: true,
        last_refresh: Instant::now(),
        worker: worker(),
    };
    if first_run {
        themes::use_theme("catppuccin-mocha")?;
        app.theme_picker()?;
        app.status = "Welcome! Arrows preview, Enter chooses; Esc keeps Catppuccin Mocha.".into();
    }
    app.connect();
    draw(&app)?;
    let mut dirty = false;
    loop {
        if app.receive() {
            dirty = true;
        }
        if app.connected
            && !app.busy
            && matches!(app.modal, Modal::None)
            && (app.refresh
                || app.last_refresh.elapsed() > Duration::from_secs(if app.stale { 10 } else { 5 }))
        {
            app.refresh = false;
            app.last_refresh = Instant::now();
            app.send(Task::List(app.kind(), app.namespace.clone()));
            dirty = true;
        }
        // At most one redraw per event-loop tick; polling never blocks terminal input.
        if dirty || app.busy {
            draw(&app)?;
            dirty = false;
        }
        if event::poll(Duration::from_millis(100)).map_err(|_| "input failed")? {
            match event::read().map_err(|_| "input failed")? {
                Event::Key(key) if key.kind == KeyEventKind::Press => {
                    if key.modifiers.contains(KeyModifiers::CONTROL)
                        && key.code == KeyCode::Char('c')
                    {
                        if app.write_inflight {
                            app.status =
                                "Mutation in progress; wait for the result before quitting.".into();
                            dirty = true;
                            continue;
                        }
                        break;
                    }
                    if app.key(key.code)? {
                        break;
                    }
                    dirty = true;
                }
                Event::Resize(_, _) => dirty = true,
                Event::Mouse(mouse) => {
                    if matches!(app.modal, Modal::None) {
                        match mouse.kind {
                            MouseEventKind::ScrollDown => {
                                app.selected =
                                    (app.selected + 1).min(app.visible().len().saturating_sub(1));
                            }
                            MouseEventKind::ScrollUp => {
                                app.selected = app.selected.saturating_sub(1)
                            }
                            MouseEventKind::Down(_) => {
                                let (w, h) =
                                    terminal::size().map_err(|_| "terminal size failed")?;
                                if mouse.row >= 4 && mouse.row < h.saturating_sub(3) {
                                    let row = (mouse.row - 4) as usize;
                                    let height = h.saturating_sub(7) as usize;
                                    if mouse.column < w.min(28) {
                                        let start =
                                            app.kind_index.saturating_sub(height.saturating_sub(1));
                                        app.kind_index =
                                            (start + row).min(app.kinds.len().saturating_sub(1));
                                        app.left_focus = false;
                                        app.changed();
                                    } else {
                                        let start =
                                            app.selected.saturating_sub(height.saturating_sub(1));
                                        app.selected = (start + row)
                                            .min(app.visible().len().saturating_sub(1));
                                        app.left_focus = false;
                                    }
                                }
                            }
                            _ => {}
                        }
                        dirty = true;
                    }
                }
                _ => {}
            }
        }
        // Render when new worker state arrives as well as on user input.
        if app.receive() {
            dirty = true;
        }
    }
    Ok(())
}

fn startup_animation(theme: &Theme, color: bool) -> Result<bool> {
    const LOGO: &[&str] = &[
        "            .--------.            ",
        "        .--'    |     '--.        ",
        "       /     ---+---      \\       ",
        "      |    /    |    \\     |      ",
        "      | --+----[O]----+--  |      ",
        "      |    \\    |    /     |      ",
        "       \\     ---+---      /       ",
        "        '--.    |     .--'        ",
        "            '--------'            ",
        "             K8BORG               ",
        " Kubernetes under your control    ",
    ];
    let started = Instant::now();
    let mut frame = 0usize;
    while started.elapsed() < Duration::from_secs(2) {
        let (width, height) = terminal::size().map_err(|_| "terminal size failed")?;
        if width < 40 || height < 16 {
            return Ok(true);
        }
        let mut out = io::stdout();
        queue!(out, terminal::Clear(ClearType::All)).map_err(|_| "startup draw failed")?;
        let bg = if color {
            theme.get("background").color()
        } else {
            Color::Reset
        };
        let fg = if color {
            theme
                .get(if frame.is_multiple_of(2) {
                    "accent"
                } else {
                    "text"
                })
                .color()
        } else {
            Color::Reset
        };
        let y = height.saturating_sub(14) / 2;
        for (index, line) in LOGO.iter().enumerate() {
            let padding = usize::from(width).saturating_sub(line.len()) / 2;
            row(
                &mut out,
                y + index as u16,
                width,
                &format!("{}{line}", " ".repeat(padding)),
                fg,
                bg,
            )
            .map_err(|_| "startup draw failed")?;
        }
        let progress = format!(
            "{}  Starting...  (any key skips)",
            ["|", "/", "-", "\\"][frame % 4]
        );
        row(
            &mut out,
            y + 12,
            width,
            &format!(
                "{}{}",
                " ".repeat(usize::from(width).saturating_sub(progress.len()) / 2),
                progress
            ),
            fg,
            bg,
        )
        .map_err(|_| "startup draw failed")?;
        out.flush().map_err(|_| "startup draw failed")?;
        frame += 1;
        if event::poll(Duration::from_millis(100)).map_err(|_| "startup input failed")?
            && let Event::Key(key) = event::read().map_err(|_| "startup input failed")?
            && key.kind == KeyEventKind::Press
        {
            return Ok(
                !(key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c'))
            );
        }
    }
    Ok(true)
}

pub fn preview(theme: Theme, color: bool) -> Result<()> {
    if !io::stdout().is_terminal() {
        println!("{} ({}) — validated palette", theme.name, theme.id);
        return Ok(());
    }
    let mut out = io::stdout();
    let bg = if color {
        theme.get("background").color()
    } else {
        Color::Reset
    };
    for (role, text) in [
        ("text", "Resource: demo/api — 2/2 ready"),
        ("success", "SUCCESS rollout healthy"),
        ("warning", "PRODUCTION / READ ONLY / STALE"),
        ("danger", "ERROR destructive operation requires approval"),
        ("accent", "FOCUS > details | +added -removed"),
    ] {
        queue!(
            out,
            style::SetForegroundColor(if color {
                theme.get(role).color()
            } else {
                Color::Reset
            }),
            style::SetBackgroundColor(bg),
            style::Print(format!("{text}\n"))
        )
        .map_err(|_| "preview failed")?;
    }
    execute!(out, style::ResetColor).map_err(|_| "preview failed".into())
}

#[cfg(test)]
mod layout_tests {
    use super::confirmation_lines;
    #[test]
    fn confirmation_wrap_preserves_complete_target_and_effect() {
        let summary = format!(
            "target: {}/sample\n{}",
            "a".repeat(63),
            "effect ".repeat(20)
        );
        let lines = confirmation_lines(&summary, "typed-target", 80);
        assert!(lines.iter().all(|line| line.chars().count() <= 79));
        let displayed: String = lines.iter().map(|line| &line[1..]).collect();
        assert!(displayed.contains(&summary.replace('\n', "")));
        assert!(displayed.contains("CONFIRM > typed-target_"));
    }
}
