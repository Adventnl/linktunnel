//! `nexus dashboard`: every paired device with its live state, and one key to act on it.
//!
//! The terminal counterpart of the desktop dashboard (spec §8.1): status dots, one-key connect /
//! terminal / files / wake / lock / sleep / restart / shut down, a command palette (Ctrl-K), and
//! "wake and connect" as one action with a progress indicator. All logic lives in [`Model`] so it
//! is tested without a terminal; [`run`] only moves bytes between the terminal, the agent's
//! event stream and child `nexus` processes (which own the screen while a viewer or shell runs).

use std::process::ExitCode;
use std::time::{Duration, Instant};

use anyhow::Result;
use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use nexus_core::i18n::{Locale, tr, tr_args};
use nexus_proto::v1::{
    DeviceView, IpcEvent, OsFamily, PathKind, PresenceState, ipc_event::Body as Ev,
};
use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, Borders, Cell, Clear, List, ListItem, ListState, Paragraph, Row, Table, TableState,
};

use crate::AppContext;
use crate::agent_cmds::{connect_with_events, resolve};

/// How long "wake and connect" waits for the machine to come online.
const WAKE_TIMEOUT: Duration = Duration::from_secs(120);

/// What can be done to a device.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Verb {
    Screen,
    Terminal,
    Files,
    Wake,
    Lock,
    Sleep,
    Restart,
    Shutdown,
}

impl Verb {
    const ALL: [Self; 8] = [
        Self::Screen,
        Self::Terminal,
        Self::Files,
        Self::Wake,
        Self::Lock,
        Self::Sleep,
        Self::Restart,
        Self::Shutdown,
    ];

    fn key(self) -> &'static str {
        match self {
            Self::Screen => "cli.dash.verb.screen",
            Self::Terminal => "cli.dash.verb.terminal",
            Self::Files => "cli.dash.verb.files",
            Self::Wake => "cli.dash.verb.wake",
            Self::Lock => "cli.dash.verb.lock",
            Self::Sleep => "cli.dash.verb.sleep",
            Self::Restart => "cli.dash.verb.restart",
            Self::Shutdown => "cli.dash.verb.shutdown",
        }
    }

    /// Everything that interrupts the machine's user asks first.
    fn destructive(self) -> bool {
        matches!(self, Self::Sleep | Self::Restart | Self::Shutdown)
    }

    /// The `nexus power` action name.
    fn power_action(self) -> Option<&'static str> {
        match self {
            Self::Lock => Some("lock"),
            Self::Sleep => Some("sleep"),
            Self::Restart => Some("restart"),
            Self::Shutdown => Some("shutdown"),
            _ => None,
        }
    }
}

/// What the runner must do after a key press.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Outcome {
    /// Nothing (the model changed or the key was ignored).
    None,
    /// Leave the dashboard.
    Quit,
    /// Do `verb` to the device with this `name`.
    Run(Verb, String),
    /// Wake this device, then connect to it once it is online.
    WakeAndConnect(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Mode {
    Normal,
    Confirm(Verb, String),
    Palette {
        query: String,
        selected: usize,
    },
    /// Waiting for a woken device to come online.
    Waking {
        name: String,
        id: Vec<u8>,
        since: Instant,
    },
}

/// Everything the screen shows.
#[derive(Debug)]
pub(crate) struct Model {
    pub devices: Vec<DeviceView>,
    pub selected: usize,
    pub signal: bool,
    pub mode: Mode,
    pub message: String,
    pub locale: Locale,
}

fn is_asleep(state: i32) -> bool {
    matches!(
        PresenceState::try_from(state),
        Ok(PresenceState::Sleeping | PresenceState::Hibernating)
    )
}

impl Model {
    pub(crate) fn new(locale: Locale, devices: Vec<DeviceView>, signal: bool) -> Self {
        let mut m = Self {
            devices,
            selected: 0,
            signal,
            mode: Mode::Normal,
            message: String::new(),
            locale,
        };
        m.sort();
        m.selected = 0;
        m
    }

    /// Online devices first, then by name.
    fn sort(&mut self) {
        let keep = self.current().map(|d| d.device_id.clone());
        self.devices.sort_by(|a, b| {
            b.online
                .cmp(&a.online)
                .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
        });
        if let Some(id) = keep
            && let Some(i) = self.devices.iter().position(|d| d.device_id == id)
        {
            self.selected = i;
        }
        self.selected = self.selected.min(self.devices.len().saturating_sub(1));
    }

    pub(crate) fn current(&self) -> Option<&DeviceView> {
        self.devices.get(self.selected)
    }

    /// Applies a pushed agent event.
    pub(crate) fn apply(&mut self, ev: &IpcEvent) -> Outcome {
        match &ev.body {
            Some(Ev::DeviceChanged(c)) => {
                if let Some(d) = &c.device {
                    match self.devices.iter_mut().find(|x| x.device_id == d.device_id) {
                        Some(slot) => *slot = d.clone(),
                        None => self.devices.push(d.clone()),
                    }
                    self.sort();
                    if let Mode::Waking { name, id, .. } = &self.mode
                        && *id == d.device_id
                        && d.online
                        && !is_asleep(d.state)
                    {
                        let name = name.clone();
                        self.message = tr_args(self.locale, "cli.dash.woke", &[("name", &name)]);
                        self.mode = Mode::Normal;
                        return Outcome::Run(Verb::Screen, name);
                    }
                }
            }
            Some(Ev::DeviceRemoved(r)) => {
                self.devices.retain(|d| d.device_id != r.device_id);
                self.sort();
            }
            Some(Ev::SignalState(s)) => self.signal = s.connected,
            _ => {}
        }
        Outcome::None
    }

    /// Called on a timer: gives up waiting for a device that never came back.
    pub(crate) fn tick(&mut self) {
        if let Mode::Waking { name, since, .. } = &self.mode
            && since.elapsed() > WAKE_TIMEOUT
        {
            self.message = tr_args(
                self.locale,
                "cli.dash.wake_timeout",
                &[
                    ("name", name),
                    ("secs", &WAKE_TIMEOUT.as_secs().to_string()),
                ],
            );
            self.mode = Mode::Normal;
        }
    }

    fn pick(&mut self, verb: Verb) -> Outcome {
        let Some(d) = self.current().cloned() else {
            self.message = tr(self.locale, "cli.dash.no_selection");
            return Outcome::None;
        };
        let asleep = is_asleep(d.state);
        match verb {
            Verb::Wake => Outcome::Run(Verb::Wake, d.name),
            // Connecting to a sleeping machine wakes it first, as one action.
            Verb::Screen if asleep => Outcome::WakeAndConnect(d.name),
            _ if !d.online => {
                self.message = tr_args(self.locale, "cli.dash.offline", &[("name", &d.name)]);
                Outcome::None
            }
            v if v.destructive() => {
                self.mode = Mode::Confirm(v, d.name);
                Outcome::None
            }
            v => Outcome::Run(v, d.name),
        }
    }

    /// Palette entries matching the query, as (label, verb, device index).
    pub(crate) fn palette_entries(&self, query: &str) -> Vec<(String, Verb, usize)> {
        let words: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
        let mut out = Vec::new();
        for (i, d) in self.devices.iter().enumerate() {
            for v in Verb::ALL {
                let label = format!("{} · {}", tr(self.locale, v.key()), d.name);
                let hay = label.to_lowercase();
                if words.iter().all(|w| hay.contains(w)) {
                    out.push((label, v, i));
                }
            }
        }
        out
    }

    /// Handles one key press.
    pub(crate) fn key(&mut self, key: KeyEvent) -> Outcome {
        if key.kind == KeyEventKind::Release {
            return Outcome::None;
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if ctrl && key.code == KeyCode::Char('c') {
            return Outcome::Quit;
        }
        match self.mode.clone() {
            Mode::Confirm(verb, name) => {
                self.mode = Mode::Normal;
                if matches!(key.code, KeyCode::Char('y' | 'Y')) {
                    Outcome::Run(verb, name)
                } else {
                    Outcome::None
                }
            }
            Mode::Palette { query, selected } => self.palette_key(key, query, selected),
            Mode::Waking { .. } => {
                if key.code == KeyCode::Esc {
                    self.mode = Mode::Normal;
                }
                Outcome::None
            }
            Mode::Normal => self.normal_key(key, ctrl),
        }
    }

    fn normal_key(&mut self, key: KeyEvent, ctrl: bool) -> Outcome {
        self.message.clear();
        let last = self.devices.len().saturating_sub(1);
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => Outcome::Quit,
            KeyCode::Char('k') if ctrl => {
                self.mode = Mode::Palette {
                    query: String::new(),
                    selected: 0,
                };
                Outcome::None
            }
            KeyCode::Char('/' | ':') => {
                self.mode = Mode::Palette {
                    query: String::new(),
                    selected: 0,
                };
                Outcome::None
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.selected = self.selected.saturating_sub(1);
                Outcome::None
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.selected = (self.selected + 1).min(last);
                Outcome::None
            }
            KeyCode::Home => {
                self.selected = 0;
                Outcome::None
            }
            KeyCode::End => {
                self.selected = last;
                Outcome::None
            }
            KeyCode::Enter | KeyCode::Char('c') => self.pick(Verb::Screen),
            KeyCode::Char('t') => self.pick(Verb::Terminal),
            KeyCode::Char('f') => self.pick(Verb::Files),
            KeyCode::Char('w') => self.pick(Verb::Wake),
            KeyCode::Char('l') => self.pick(Verb::Lock),
            KeyCode::Char('s') => self.pick(Verb::Sleep),
            KeyCode::Char('r') => self.pick(Verb::Restart),
            KeyCode::Char('x') => self.pick(Verb::Shutdown),
            _ => Outcome::None,
        }
    }

    fn palette_key(&mut self, key: KeyEvent, mut query: String, mut selected: usize) -> Outcome {
        let count = self.palette_entries(&query).len();
        match key.code {
            KeyCode::Esc => {
                self.mode = Mode::Normal;
                return Outcome::None;
            }
            KeyCode::Enter => {
                let entry = self.palette_entries(&query).get(selected).cloned();
                self.mode = Mode::Normal;
                if let Some((_, verb, idx)) = entry {
                    self.selected = idx;
                    return self.pick(verb);
                }
                return Outcome::None;
            }
            KeyCode::Up => selected = selected.saturating_sub(1),
            KeyCode::Down => selected = (selected + 1).min(count.saturating_sub(1)),
            KeyCode::Backspace => {
                query.pop();
                selected = 0;
            }
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                query.push(c);
                selected = 0;
            }
            _ => {}
        }
        self.mode = Mode::Palette { query, selected };
        Outcome::None
    }
}

// ---- rendering --------------------------------------------------------------------------------

fn dot(state: i32, online: bool) -> (&'static str, Color) {
    match PresenceState::try_from(state).unwrap_or(PresenceState::Unspecified) {
        PresenceState::Online | PresenceState::Idle => ("●", Color::Green),
        PresenceState::InUse => ("●", Color::Blue),
        PresenceState::Locked => ("●", Color::Yellow),
        PresenceState::Sleeping | PresenceState::Hibernating => ("●", Color::Rgb(255, 140, 0)),
        PresenceState::Offline => ("●", Color::Red),
        PresenceState::Unspecified if online => ("●", Color::Green),
        PresenceState::Unspecified => ("○", Color::Gray),
    }
}

fn state_text(l: Locale, state: i32) -> String {
    let key = match PresenceState::try_from(state).unwrap_or(PresenceState::Unspecified) {
        PresenceState::Online => "state.online",
        PresenceState::Idle => "state.idle",
        PresenceState::Locked => "state.locked",
        PresenceState::InUse => "state.in_use",
        PresenceState::Sleeping => "state.sleeping",
        PresenceState::Hibernating => "state.hibernating",
        PresenceState::Offline => "state.offline",
        PresenceState::Unspecified => "state.unknown",
    };
    tr(l, key)
}

fn path_text(l: Locale, path: i32) -> String {
    let key = match PathKind::try_from(path).unwrap_or(PathKind::Unspecified) {
        PathKind::Lan => "path.lan",
        PathKind::Direct => "path.direct",
        PathKind::Relay => "path.relay",
        PathKind::Unspecified => "path.none",
    };
    tr(l, key)
}

fn os_text(d: &DeviceView) -> String {
    let Some(os) = &d.os else { return "-".into() };
    let family = match OsFamily::try_from(os.family).unwrap_or(OsFamily::Unspecified) {
        OsFamily::Windows => "Windows",
        OsFamily::Macos => "macOS",
        OsFamily::Linux => "Linux",
        OsFamily::Unspecified => "",
    };
    if os.version.is_empty() {
        family.to_owned()
    } else {
        os.version.clone()
    }
}

/// "12s ago" / "3h ago" for a millisecond timestamp.
pub(crate) fn seen_text(l: Locale, last_seen_ms: u64, now_ms: u64) -> String {
    if last_seen_ms == 0 {
        return tr(l, "cli.dash.seen_never");
    }
    let secs = now_ms.saturating_sub(last_seen_ms) / 1000;
    let (key, n) = match secs {
        0..=4 => return tr(l, "cli.dash.seen_now"),
        5..=59 => ("cli.dash.seen_secs", secs),
        60..=3599 => ("cli.dash.seen_mins", secs / 60),
        3600..=86_399 => ("cli.dash.seen_hours", secs / 3600),
        _ => ("cli.dash.seen_days", secs / 86_400),
    };
    tr_args(l, key, &[("n", &n.to_string())])
}

pub(crate) fn render(frame: &mut Frame<'_>, model: &Model, now_ms: u64) {
    let l = model.locale;
    let area = frame.area();
    let [head, body, foot] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(3),
        Constraint::Length(3),
    ])
    .areas(area);

    let signal = if model.signal {
        tr(l, "cli.dash.signal_on")
    } else {
        tr(l, "cli.dash.signal_off")
    };
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                tr(l, "cli.dash.title"),
                Style::new().add_modifier(Modifier::BOLD),
            ),
            Span::raw("   "),
            Span::styled(
                signal,
                Style::new().fg(if model.signal {
                    Color::Green
                } else {
                    Color::Yellow
                }),
            ),
        ])),
        head,
    );

    if model.devices.is_empty() {
        frame.render_widget(
            Paragraph::new(tr(l, "cli.dash.empty")).wrap(ratatui::widgets::Wrap { trim: true }),
            body,
        );
    } else {
        let header = Row::new(
            [
                "",
                "col_name",
                "col_state",
                "col_path",
                "col_user",
                "col_os",
                "col_battery",
                "col_seen",
            ]
            .map(|k| {
                if k.is_empty() {
                    String::new()
                } else {
                    tr(l, &format!("cli.dash.{k}"))
                }
            }),
        )
        .style(Style::new().add_modifier(Modifier::BOLD | Modifier::UNDERLINED));
        let rows: Vec<Row<'_>> = model
            .devices
            .iter()
            .map(|d| {
                let (glyph, color) = dot(d.state, d.online);
                let battery = if d.battery_percent == 0 || d.battery_percent > 100 {
                    "-".to_owned()
                } else {
                    format!("{}%", d.battery_percent)
                };
                Row::new([
                    Cell::from(Span::styled(glyph, Style::new().fg(color))),
                    Cell::from(d.name.clone()),
                    Cell::from(state_text(l, d.state)),
                    Cell::from(path_text(l, d.path)),
                    Cell::from(if d.active_user.is_empty() {
                        "-".to_owned()
                    } else {
                        d.active_user.clone()
                    }),
                    Cell::from(os_text(d)),
                    Cell::from(battery),
                    Cell::from(seen_text(l, d.last_seen_ms, now_ms)),
                ])
            })
            .collect();
        let table = Table::new(
            rows,
            [
                Constraint::Length(2),
                Constraint::Min(14),
                Constraint::Length(24),
                Constraint::Length(7),
                Constraint::Length(12),
                Constraint::Min(14),
                Constraint::Length(8),
                Constraint::Length(12),
            ],
        )
        .header(header)
        .row_highlight_style(Style::new().add_modifier(Modifier::REVERSED))
        .highlight_symbol("▶ ");
        let mut state = TableState::default().with_selected(Some(model.selected));
        frame.render_stateful_widget(table, body, &mut state);
    }

    let status = match &model.mode {
        Mode::Waking { name, since, .. } => tr_args(
            l,
            "cli.dash.waking",
            &[
                ("name", name),
                ("secs", &since.elapsed().as_secs().to_string()),
            ],
        ),
        _ => model.message.clone(),
    };
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(Span::styled(status, Style::new().fg(Color::Cyan))),
            Line::from(Span::styled(
                tr(l, "cli.dash.keys"),
                Style::new().add_modifier(Modifier::DIM),
            )),
        ])
        .wrap(ratatui::widgets::Wrap { trim: true }),
        foot,
    );

    match &model.mode {
        Mode::Confirm(verb, name) => {
            let text = tr_args(
                l,
                "cli.dash.confirm",
                &[("action", &tr(l, verb.key())), ("name", name)],
            );
            let r = centered(area, 60, 5);
            frame.render_widget(Clear, r);
            frame.render_widget(
                Paragraph::new(text)
                    .wrap(ratatui::widgets::Wrap { trim: true })
                    .block(
                        Block::new()
                            .borders(Borders::ALL)
                            .border_style(Style::new().fg(Color::Red)),
                    ),
                r,
            );
        }
        Mode::Palette { query, selected } => {
            let r = centered(area, 70, 14);
            frame.render_widget(Clear, r);
            let block = Block::new()
                .borders(Borders::ALL)
                .title(tr(l, "cli.dash.palette"))
                .title_bottom(
                    Line::from(tr(l, "cli.dash.palette_hint")).alignment(Alignment::Right),
                );
            let inner = block.inner(r);
            frame.render_widget(block, r);
            let [input, list] =
                Layout::vertical([Constraint::Length(1), Constraint::Min(1)]).areas(inner);
            frame.render_widget(Paragraph::new(format!("› {query}")), input);
            let entries = model.palette_entries(query);
            if entries.is_empty() {
                frame.render_widget(Paragraph::new(tr(l, "cli.dash.no_match")), list);
            } else {
                let items: Vec<ListItem<'_>> = entries
                    .iter()
                    .map(|(label, ..)| ListItem::new(label.clone()))
                    .collect();
                let mut st = ListState::default().with_selected(Some(*selected));
                frame.render_stateful_widget(
                    List::new(items).highlight_style(Style::new().add_modifier(Modifier::REVERSED)),
                    list,
                    &mut st,
                );
            }
        }
        _ => {}
    }
}

fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let w = width.min(area.width);
    let h = height.min(area.height);
    Rect::new(
        area.x + (area.width - w) / 2,
        area.y + (area.height - h) / 3,
        w,
        h,
    )
}

/// Renders one frame to plain text (for `--snapshot`, tests and bug reports).
pub(crate) fn snapshot(model: &Model, width: u16, now_ms: u64) -> String {
    let height = u16::try_from(model.devices.len())
        .unwrap_or(40)
        .saturating_add(5)
        .max(8);
    let mut terminal =
        match ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height)) {
            Ok(t) => t,
            Err(e) => match e {},
        };
    let _ = terminal.draw(|f| render(f, model, now_ms));
    let buf = terminal.backend().buffer().clone();
    let mut out = String::new();
    for y in 0..buf.area.height {
        let mut line = String::new();
        let mut skip = 0usize;
        for x in 0..buf.area.width {
            // A double-width character (CJK) covers the next cell too; do not print it twice.
            if skip > 0 {
                skip -= 1;
                continue;
            }
            let sym = buf[(x, y)].symbol();
            skip = unicode_width::UnicodeWidthStr::width(sym).saturating_sub(1);
            line.push_str(sym);
        }
        out.push_str(line.trim_end());
        out.push('\n');
    }
    out
}

// ---- the interactive runner -------------------------------------------------------------------

fn now_ms() -> u64 {
    nexus_core::clock::unix_ms()
}

/// Runs a child `nexus` command with the terminal handed over to it.
fn run_child(ctx: &AppContext, args: &[&str], pause_after: bool) -> Result<bool> {
    ratatui::restore();
    let exe = std::env::current_exe()?;
    let status = std::process::Command::new(exe)
        .args(&ctx.passthrough)
        .args(args)
        .status();
    if pause_after {
        println!("\n{}", tr(ctx.locale, "cli.dash.press_enter"));
        let mut s = String::new();
        let _ = std::io::stdin().read_line(&mut s);
    }
    Ok(status.is_ok_and(|s| s.success()))
}

fn args_for(verb: Verb, name: &str) -> Vec<String> {
    match verb {
        Verb::Screen => vec![
            "remote".into(),
            "view".into(),
            name.into(),
            "--audio".into(),
        ],
        Verb::Terminal => vec!["shell".into(), name.into()],
        Verb::Files => vec!["ls".into(), name.into()],
        Verb::Wake => vec!["wake".into(), name.into()],
        v => vec![
            "power".into(),
            name.into(),
            v.power_action().unwrap_or("lock").into(),
            "--yes".into(),
        ],
    }
}

pub(crate) async fn run(ctx: &AppContext, once: bool, width: u16) -> Result<ExitCode> {
    let (client, mut events) = connect_with_events(ctx).await?;
    let status = client.status().await?;
    let devices = client.list_devices().await?.devices;
    let mut model = Model::new(ctx.locale, devices, status.signal_connected);
    if once {
        print!("{}", snapshot(&model, width, now_ms()));
        return Ok(ExitCode::SUCCESS);
    }

    let (keys_tx, mut keys_rx) = tokio::sync::mpsc::unbounded_channel::<KeyEvent>();
    let suspended = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    {
        let suspended = std::sync::Arc::clone(&suspended);
        std::thread::spawn(move || {
            loop {
                // While a child owns the terminal this thread must not steal its input.
                if suspended.load(std::sync::atomic::Ordering::SeqCst) {
                    std::thread::sleep(Duration::from_millis(50));
                    continue;
                }
                match crossterm::event::poll(Duration::from_millis(100)) {
                    Ok(true) => {
                        if let Ok(Event::Key(k)) = crossterm::event::read()
                            && keys_tx.send(k).is_err()
                        {
                            return;
                        }
                    }
                    Ok(false) => {}
                    Err(_) => return,
                }
            }
        });
    }

    let mut terminal = ratatui::init();
    let mut tick = tokio::time::interval(Duration::from_millis(250));
    let result: Result<()> = async {
        loop {
            terminal.draw(|f| render(f, &model, now_ms()))?;
            let outcome = tokio::select! {
                Some(k) = keys_rx.recv() => model.key(k),
                ev = events.next() => match ev {
                    Some(ev) => model.apply(&ev),
                    None => { model.message = "lost the connection to the agent".into(); Outcome::Quit }
                },
                _ = tick.tick() => { model.tick(); Outcome::None }
            };
            match outcome {
                Outcome::None => {}
                Outcome::Quit => return Ok(()),
                Outcome::WakeAndConnect(name) => {
                    let devices = client.list_devices().await?.devices;
                    let dev = resolve(ctx.locale, &devices, &name)?;
                    match client.wake_device(dev.device_id.to_vec(), Vec::new()).await {
                        Ok(()) => {
                            model.mode = Mode::Waking { name, id: dev.device_id.to_vec(), since: Instant::now() };
                        }
                        Err(e) => {
                            model.message = tr_args(ctx.locale, "cli.dash.wake_failed", &[("name", &name), ("reason", &e.to_string())]);
                        }
                    }
                }
                Outcome::Run(Verb::Wake, name) => {
                    let devices = client.list_devices().await?.devices;
                    let dev = resolve(ctx.locale, &devices, &name)?;
                    model.message = match client.wake_device(dev.device_id.to_vec(), Vec::new()).await {
                        Ok(()) => tr_args(ctx.locale, "cli.dash.wake_sent", &[("name", &name)]),
                        Err(e) => tr_args(ctx.locale, "cli.dash.wake_failed", &[("name", &name), ("reason", &e.to_string())]),
                    };
                }
                Outcome::Run(verb, name) => {
                    suspended.store(true, std::sync::atomic::Ordering::SeqCst);
                    let args = args_for(verb, &name);
                    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
                    let pause = matches!(verb, Verb::Files);
                    let ok = run_child(ctx, &refs, pause)?;
                    terminal = ratatui::init();
                    // Drop key presses made while the child ran.
                    while keys_rx.try_recv().is_ok() {}
                    suspended.store(false, std::sync::atomic::Ordering::SeqCst);
                    if ok {
                        model.message = tr_args(
                            ctx.locale,
                            "cli.dash.ran",
                            &[("action", &tr(ctx.locale, verb.key())), ("name", &name)],
                        );
                    }
                }
            }
        }
    }
    .await;
    ratatui::restore();
    result?;
    Ok(ExitCode::SUCCESS)
}

#[cfg(test)]
mod tests {
    use nexus_proto::v1::{DeviceChanged, DeviceRemoved, OsInfo, SignalState};

    use super::*;

    fn device(name: &str, id: u8, state: PresenceState, online: bool) -> DeviceView {
        DeviceView {
            device_id: vec![id; 16].into(),
            name: name.into(),
            state: state as i32,
            online,
            last_seen_ms: 1_000_000,
            os: Some(OsInfo {
                family: OsFamily::Windows as i32,
                version: "Windows 11".into(),
                ..Default::default()
            }),
            path: PathKind::Lan as i32,
            active_user: "alex".into(),
            battery_percent: 80,
            ..Default::default()
        }
    }

    fn model() -> Model {
        Model::new(
            Locale::En,
            vec![
                device("Zed Server", 3, PresenceState::Offline, false),
                device("Windows PC", 1, PresenceState::Idle, true),
                device("Old Laptop", 2, PresenceState::Sleeping, false),
                device("MacBook", 4, PresenceState::InUse, true),
            ],
            true,
        )
    }

    fn press(m: &mut Model, c: KeyCode) -> Outcome {
        m.key(KeyEvent::new(c, KeyModifiers::NONE))
    }

    #[test]
    fn online_devices_sort_first_then_by_name() {
        let m = model();
        let names: Vec<&str> = m.devices.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(names, ["MacBook", "Windows PC", "Old Laptop", "Zed Server"]);
    }

    #[test]
    fn navigation_is_clamped() {
        let mut m = model();
        press(&mut m, KeyCode::Up);
        assert_eq!(m.selected, 0);
        for _ in 0..10 {
            press(&mut m, KeyCode::Down);
        }
        assert_eq!(m.selected, 3);
        press(&mut m, KeyCode::Home);
        assert_eq!(m.selected, 0);
    }

    #[test]
    fn enter_connects_to_online_devices() {
        let mut m = model();
        assert_eq!(
            press(&mut m, KeyCode::Enter),
            Outcome::Run(Verb::Screen, "MacBook".into())
        );
        assert_eq!(
            press(&mut m, KeyCode::Char('t')),
            Outcome::Run(Verb::Terminal, "MacBook".into())
        );
    }

    #[test]
    fn connecting_to_a_sleeping_device_wakes_it_first() {
        let mut m = model();
        m.selected = 2;
        assert_eq!(
            press(&mut m, KeyCode::Enter),
            Outcome::WakeAndConnect("Old Laptop".into())
        );
        // Terminal on a sleeper is refused with a hint, not silently queued.
        assert_eq!(press(&mut m, KeyCode::Char('t')), Outcome::None);
        assert!(m.message.contains("offline"));
    }

    #[test]
    fn offline_devices_refuse_everything_but_wake() {
        let mut m = model();
        m.selected = 3;
        assert_eq!(press(&mut m, KeyCode::Enter), Outcome::None);
        assert_eq!(press(&mut m, KeyCode::Char('l')), Outcome::None);
        assert_eq!(
            press(&mut m, KeyCode::Char('w')),
            Outcome::Run(Verb::Wake, "Zed Server".into())
        );
    }

    #[test]
    fn destructive_actions_need_an_explicit_yes() {
        let mut m = model();
        assert_eq!(press(&mut m, KeyCode::Char('x')), Outcome::None);
        assert!(matches!(m.mode, Mode::Confirm(Verb::Shutdown, _)));
        // Anything but y cancels.
        assert_eq!(press(&mut m, KeyCode::Enter), Outcome::None);
        assert_eq!(m.mode, Mode::Normal);
        press(&mut m, KeyCode::Char('r'));
        assert_eq!(
            press(&mut m, KeyCode::Char('y')),
            Outcome::Run(Verb::Restart, "MacBook".into())
        );
        // Locking is not destructive.
        assert_eq!(
            press(&mut m, KeyCode::Char('l')),
            Outcome::Run(Verb::Lock, "MacBook".into())
        );
    }

    #[test]
    fn palette_filters_by_every_word_and_runs_the_choice() {
        let mut m = model();
        m.key(KeyEvent::new(KeyCode::Char('k'), KeyModifiers::CONTROL));
        assert!(matches!(m.mode, Mode::Palette { .. }));
        for c in "term win".chars() {
            press(&mut m, KeyCode::Char(c));
        }
        let Mode::Palette { query, .. } = &m.mode else {
            panic!()
        };
        let hits = m.palette_entries(query);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].1, Verb::Terminal);
        assert_eq!(
            press(&mut m, KeyCode::Enter),
            Outcome::Run(Verb::Terminal, "Windows PC".into())
        );
        assert_eq!(m.mode, Mode::Normal);
    }

    #[test]
    fn palette_escape_and_empty_results() {
        let mut m = model();
        press(&mut m, KeyCode::Char('/'));
        for c in "zzzz".chars() {
            press(&mut m, KeyCode::Char(c));
        }
        assert_eq!(press(&mut m, KeyCode::Enter), Outcome::None);
        press(&mut m, KeyCode::Char('/'));
        press(&mut m, KeyCode::Esc);
        assert_eq!(m.mode, Mode::Normal);
    }

    #[test]
    fn quit_keys() {
        assert_eq!(press(&mut model(), KeyCode::Char('q')), Outcome::Quit);
        assert_eq!(
            model().key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            Outcome::Quit
        );
    }

    #[test]
    fn events_update_the_list_and_selection_follows_the_device() {
        let mut m = model();
        m.selected = 1; // Windows PC
        // The PC goes offline: it sorts below; the cursor must stay on it.
        let mut pc = device("Windows PC", 1, PresenceState::Offline, false);
        pc.last_seen_ms = 5;
        m.apply(&IpcEvent {
            body: Some(Ev::DeviceChanged(DeviceChanged { device: Some(pc) })),
        });
        assert_eq!(m.current().unwrap().name, "Windows PC");
        assert_eq!(m.devices[0].name, "MacBook");
        m.apply(&IpcEvent {
            body: Some(Ev::DeviceRemoved(DeviceRemoved {
                device_id: vec![4; 16].into(),
            })),
        });
        assert!(m.devices.iter().all(|d| d.name != "MacBook"));
        m.apply(&IpcEvent {
            body: Some(Ev::SignalState(SignalState { connected: false })),
        });
        assert!(!m.signal);
        // A brand-new device appears.
        m.apply(&IpcEvent {
            body: Some(Ev::DeviceChanged(DeviceChanged {
                device: Some(device("NAS", 9, PresenceState::Online, true)),
            })),
        });
        assert!(m.devices.iter().any(|d| d.name == "NAS"));
    }

    #[test]
    fn wake_and_connect_connects_when_the_device_comes_online_and_times_out_otherwise() {
        let mut m = model();
        let id = vec![2u8; 16];
        m.mode = Mode::Waking {
            name: "Old Laptop".into(),
            id: id.clone(),
            since: Instant::now(),
        };
        // Still asleep: keep waiting.
        let still = device("Old Laptop", 2, PresenceState::Sleeping, true);
        assert_eq!(
            m.apply(&IpcEvent {
                body: Some(Ev::DeviceChanged(DeviceChanged {
                    device: Some(still)
                }))
            }),
            Outcome::None
        );
        assert!(matches!(m.mode, Mode::Waking { .. }));
        // Online: connect.
        let up = device("Old Laptop", 2, PresenceState::Online, true);
        assert_eq!(
            m.apply(&IpcEvent {
                body: Some(Ev::DeviceChanged(DeviceChanged { device: Some(up) }))
            }),
            Outcome::Run(Verb::Screen, "Old Laptop".into())
        );
        assert_eq!(m.mode, Mode::Normal);
        // Timeout.
        m.mode = Mode::Waking {
            name: "Old Laptop".into(),
            id,
            since: Instant::now()
                .checked_sub(WAKE_TIMEOUT + Duration::from_secs(1))
                .unwrap(),
        };
        m.tick();
        assert_eq!(m.mode, Mode::Normal);
        assert!(m.message.contains("did not come online"));
    }

    #[test]
    fn the_screen_shows_devices_states_and_hints() {
        let m = model();
        let text = snapshot(&m, 110, 1_030_000);
        for needle in [
            "Nexus Remote",
            "MacBook",
            "Windows PC",
            "Old Laptop",
            "Zed Server",
            "in use",
            "sleeping (can be woken)",
            "offline",
            "alex",
            "80%",
            "30s ago",
            "Enter connect",
        ] {
            assert!(text.contains(needle), "missing {needle:?} in:\n{text}");
        }
    }

    #[test]
    fn the_screen_renders_modals_and_the_empty_state_and_chinese() {
        let mut m = model();
        press(&mut m, KeyCode::Char('x'));
        assert!(snapshot(&m, 110, 0).contains("MacBook"));
        assert!(snapshot(&m, 110, 0).contains("[y]"));
        let mut m = model();
        press(&mut m, KeyCode::Char('/'));
        assert!(snapshot(&m, 110, 0).contains("Command palette"));
        let empty = Model::new(Locale::En, vec![], false);
        assert!(snapshot(&empty, 100, 0).contains("No paired devices"));
        let zh = Model::new(
            Locale::ZhCn,
            vec![device("台式机", 1, PresenceState::Online, true)],
            true,
        );
        let t = snapshot(&zh, 110, 0);
        assert!(t.contains("设备") && t.contains("台式机"), "{t}");
    }

    #[test]
    fn seen_text_buckets() {
        let l = Locale::En;
        assert_eq!(seen_text(l, 0, 5000), "never");
        assert_eq!(seen_text(l, 10_000, 11_000), "now");
        assert_eq!(seen_text(l, 10_000, 40_000), "30s ago");
        assert_eq!(seen_text(l, 1000, 1000 + 5 * 60_000), "5m ago");
        assert_eq!(seen_text(l, 1000, 1000 + 3 * 3_600_000), "3h ago");
        assert_eq!(seen_text(l, 1000, 1000 + 2 * 86_400_000), "2d ago");
    }

    #[test]
    fn children_are_invoked_with_the_right_arguments() {
        assert_eq!(
            args_for(Verb::Screen, "PC"),
            ["remote", "view", "PC", "--audio"]
        );
        assert_eq!(
            args_for(Verb::Shutdown, "PC"),
            ["power", "PC", "shutdown", "--yes"]
        );
        assert_eq!(args_for(Verb::Files, "PC"), ["ls", "PC"]);
    }
}
