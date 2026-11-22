//! TUI state, event loop, key handling and the three-mode renderer.
//!
//! The agent loop runs on its own tokio task and sends `Event`s over a channel.
//! The UI task merges those with crossterm key events, redraws on change, and
//! answers approval requests inline. Every mode renders the same `Timeline`.

pub mod composer;
pub mod overview;
pub mod picker;
pub mod raw;
pub mod timeline;
pub mod tools;

use std::collections::HashSet;
use std::io;
use std::path::PathBuf;
use std::process::Command as ProcessCommand;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use anyhow::Result;
use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event as CrosstermEvent, KeyCode, KeyEvent,
    KeyEventKind, KeyModifiers, MouseEvent, MouseEventKind,
};
use crossterm::execute;
use crossterm::terminal::{self, EnterAlternateScreen, LeaveAlternateScreen};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph};
use ratatui::{Frame, Terminal};
use tokio::sync::mpsc;

use crate::clock;
use crate::context;
use crate::memory;
use crate::models;
use crate::prompt;
use crate::providers::Provider;
use crate::r#loop::{self, ApprovalRequest, CancelToken, Event, Options};
use crate::session::Session;
use crate::tools::Registry;
use crate::types::{Message, Part, Role};

use composer::Composer;
use picker::Picker;
use timeline::Timeline;

/// How long the UI waits for a loop event before checking the keyboard.
const TICK: Duration = Duration::from_millis(40);
/// Lines moved by PgUp/PgDn.
const PAGE: usize = 10;
/// Lines moved by one mouse wheel notch.
const WHEEL: usize = 3;

type Tui = Terminal<CrosstermBackend<io::Stdout>>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mode {
    #[default]
    Overview,
    Tools,
    Raw,
}

impl Mode {
    pub fn index(self) -> usize {
        match self {
            Self::Overview => 0,
            Self::Tools => 1,
            Self::Raw => 2,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Overview => "overview",
            Self::Tools => "tools",
            Self::Raw => "raw",
        }
    }

    pub fn next(self) -> Self {
        match self {
            Self::Overview => Self::Tools,
            Self::Tools => Self::Raw,
            Self::Raw => Self::Overview,
        }
    }

    pub fn from_digit(ch: char) -> Option<Self> {
        match ch {
            '1' => Some(Self::Overview),
            '2' => Some(Self::Tools),
            '3' => Some(Self::Raw),
            _ => None,
        }
    }
}

/// Scroll position measured in lines back from the bottom, so appended output
/// does not move what the user is reading. `follow` pins the view to the end.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Scroll {
    pub back: usize,
    pub follow: bool,
}

impl Default for Scroll {
    fn default() -> Self {
        Self {
            back: 0,
            follow: true,
        }
    }
}

impl Scroll {
    fn up(&mut self, lines: usize) {
        self.follow = false;
        self.back = self.back.saturating_add(lines);
    }

    fn down(&mut self, lines: usize) {
        self.back = self.back.saturating_sub(lines);
        if self.back == 0 {
            self.follow = true;
        }
    }

    /// The first line shown for a body of `total` lines in `height` rows.
    pub fn start(&self, total: usize, height: usize) -> usize {
        if self.follow {
            total.saturating_sub(height)
        } else {
            total.saturating_sub(height.saturating_add(self.back))
        }
    }
}

pub struct StatusLine<'a> {
    pub provider: &'a str,
    pub model: &'a str,
    pub used: usize,
    /// Real usage from the provider, as opposed to a `~` estimate.
    pub real_usage: bool,
    pub window: Option<usize>,
    pub running: bool,
}

pub struct View<'a> {
    pub timeline: &'a Timeline,
    pub mode: Mode,
    pub scroll: Scroll,
    /// Tool call ids whose output is expanded (mode 2).
    pub expanded: &'a HashSet<String>,
    /// The selected tool call's id (mode 2).
    pub selected: Option<&'a str>,
    pub status: StatusLine<'a>,
}

/// Everything `main` hands to the TUI after `startup()`.
pub struct Setup {
    pub provider: Box<dyn Provider>,
    pub registry: Registry,
    pub options: Options,
    pub messages: Vec<Message>,
    pub session: Session,
    pub instructions: String,
    /// The memory path and text, or `None` with `--no-memory`.
    pub memory: Option<(PathBuf, String)>,
    pub memory_enabled: bool,
    pub memory_path: PathBuf,
    pub home: PathBuf,
    pub slug: String,
    pub workspace: PathBuf,
    pub provider_id: String,
    pub model: String,
}

/// Enter the alternate screen and run until /quit.
pub async fn run(setup: Setup) -> Result<()> {
    install_panic_hook();
    terminal::enable_raw_mode()?;
    execute!(
        io::stdout(),
        EnterAlternateScreen,
        EnableMouseCapture,
        crossterm::cursor::Hide
    )?;
    let _guard = TerminalGuard;

    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;
    terminal.clear()?;
    let mut app = App::new(setup);
    app.refresh_models(false);
    app.run(&mut terminal).await
}

/// Restore the terminal on unwind as well as on the normal path.
struct TerminalGuard;

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = terminal::disable_raw_mode();
        let _ = execute!(
            io::stdout(),
            LeaveAlternateScreen,
            DisableMouseCapture,
            crossterm::cursor::Show
        );
    }
}

fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = terminal::disable_raw_mode();
        let _ = execute!(
            io::stdout(),
            LeaveAlternateScreen,
            DisableMouseCapture,
            crossterm::cursor::Show
        );
        previous(info);
    }));
}

fn suspend() -> Result<()> {
    terminal::disable_raw_mode()?;
    execute!(
        io::stdout(),
        LeaveAlternateScreen,
        DisableMouseCapture,
        crossterm::cursor::Show
    )?;
    Ok(())
}

fn resume(terminal: &mut Tui) -> Result<()> {
    execute!(
        io::stdout(),
        EnterAlternateScreen,
        EnableMouseCapture,
        crossterm::cursor::Hide
    )?;
    terminal::enable_raw_mode()?;
    terminal.clear()?;
    Ok(())
}

/// The pure renderer: timeline + view state in, one frame out.
pub fn draw(frame: &mut Frame, view: &View) {
    let chunks = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).split(frame.area());
    draw_body(
        frame,
        chunks[0],
        view.timeline,
        view.mode,
        view.scroll,
        view.expanded,
        view.selected,
    );
    draw_status(frame, chunks[1], &view.status, view.mode);
}

fn draw_body(
    frame: &mut Frame,
    area: Rect,
    timeline: &Timeline,
    mode: Mode,
    scroll: Scroll,
    expanded: &HashSet<String>,
    selected: Option<&str>,
) {
    let lines = lines_with(timeline, mode, area.width, expanded, selected);
    let height = area.height as usize;
    let start = scroll.start(lines.len(), height);
    let visible: Vec<Line<'static>> = lines.into_iter().skip(start).take(height).collect();
    frame.render_widget(Paragraph::new(visible), area);
}

fn draw_status(frame: &mut Frame, area: Rect, status: &StatusLine, mode: Mode) {
    let used = if status.real_usage {
        format!("{}", status.used)
    } else {
        format!("~{}", status.used)
    };
    let window = status
        .window
        .map_or_else(|| "?".to_string(), |w| w.to_string());
    let running = if status.running {
        "● running"
    } else {
        "· idle"
    };
    let badge = format!(" {} ", mode.label().to_uppercase());
    let rest = format!(
        " {}/{} │ {} / {} tokens │ {running}",
        status.provider, status.model, used, window
    );
    let style = if status.running {
        Style::default().fg(Color::Yellow)
    } else {
        Style::default().add_modifier(Modifier::DIM)
    };
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                badge,
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(rest, style),
        ])),
        area,
    );
}

/// The lines of the selected mode with no tool output expanded and no row selected.
pub fn lines(timeline: &Timeline, mode: Mode, width: u16) -> Vec<Line<'static>> {
    lines_with(timeline, mode, width, &HashSet::new(), None)
}

/// The lines of the selected mode; the single seam mode 2 and 3 plug into.
pub fn lines_with(
    timeline: &Timeline,
    mode: Mode,
    width: u16,
    expanded: &HashSet<String>,
    selected: Option<&str>,
) -> Vec<Line<'static>> {
    match mode {
        Mode::Overview => overview::lines(timeline, width),
        Mode::Tools => tools::lines_expanded(timeline, width, expanded, selected),
        Mode::Raw => raw::lines(timeline, width),
    }
}

/// Word-agnostic wrapping at `width`, preserving explicit newlines.
pub fn wrap(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut lines = Vec::new();
    for source in text.split('\n') {
        if source.is_empty() {
            lines.push(String::new());
            continue;
        }
        let mut line = String::new();
        let mut count = 0;
        for ch in source.chars() {
            if count == width {
                lines.push(std::mem::take(&mut line));
                count = 0;
            }
            line.push(ch);
            count += 1;
        }
        lines.push(line);
    }
    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Action {
    Continue,
    EditMemory,
    Quit,
}

enum Wake {
    Loop(Option<Event>),
    Models(Option<Result<models::Catalog>>),
    Keys,
}

struct App {
    timeline: Timeline,
    composer: Composer,
    provider: Arc<dyn Provider>,
    registry: Arc<Registry>,
    options: Options,
    messages: Vec<Message>,
    session: Session,
    home: PathBuf,
    slug: String,
    workspace: PathBuf,
    memory_path: PathBuf,
    memory_enabled: bool,
    provider_id: String,
    model: String,
    /// The last good catalog (cache, then a live fetch). `None` means fallbacks.
    catalog: Option<models::Catalog>,
    models_loading: bool,
    model_events: Option<mpsc::UnboundedReceiver<Result<models::Catalog>>>,
    /// The inline `/model` picker, when open.
    picker: Option<Picker>,
    mode: Mode,
    scroll: [Scroll; 3],
    /// The body area of the last frame, used to clamp scrolling to the content.
    body_width: u16,
    body_height: usize,
    /// Mode 2: tool call ids whose output is fully expanded.
    expanded: HashSet<String>,
    /// Mode 2: index into the timeline's tool runs of the selected row.
    selected: usize,
    running: bool,
    cancel: Option<CancelToken>,
    pending: Option<ApprovalRequest>,
    always: HashSet<String>,
    events: Option<mpsc::UnboundedReceiver<Event>>,
}

impl App {
    fn new(setup: Setup) -> Self {
        let Setup {
            provider,
            registry,
            mut options,
            messages,
            session,
            instructions,
            memory,
            memory_enabled,
            memory_path,
            home,
            slug,
            workspace,
            provider_id,
            model,
        } = setup;

        options.conversation_id = conversation_id(&session);
        let timeline =
            Timeline::from_messages(instructions, memory, registry.schemas(), messages.clone());
        let catalog = models::load_cache(&home);

        Self {
            timeline,
            composer: Composer::new(),
            provider: Arc::from(provider),
            registry: Arc::new(registry),
            options,
            messages,
            session,
            home,
            slug,
            workspace,
            memory_path,
            memory_enabled,
            provider_id,
            model,
            catalog,
            models_loading: false,
            model_events: None,
            picker: None,
            mode: Mode::Overview,
            scroll: [Scroll::default(); 3],
            body_width: 80,
            body_height: 24,
            expanded: HashSet::new(),
            selected: 0,
            running: false,
            cancel: None,
            pending: None,
            always: HashSet::new(),
            events: None,
        }
    }

    async fn run(mut self, terminal: &mut Tui) -> Result<()> {
        let mut dirty = true;
        loop {
            if dirty {
                self.draw(terminal)?;
                dirty = false;
            }

            match self.next_wake().await {
                Wake::Loop(Some(event)) => {
                    self.on_loop_event(event);
                    dirty = true;
                }
                Wake::Loop(None) => {
                    self.events = None;
                    if self.running {
                        // The task ended without its final `TurnEnd`.
                        self.running = false;
                        self.cancel = None;
                        self.pending = None;
                        self.timeline.notice("turn ended without a final event");
                    }
                    dirty = true;
                }
                Wake::Models(result) => {
                    self.on_models(result);
                    dirty = true;
                }
                Wake::Keys => {
                    while event::poll(Duration::ZERO)? {
                        match event::read()? {
                            CrosstermEvent::Key(key) if key.kind != KeyEventKind::Release => {
                                match self.on_key(key)? {
                                    Action::Continue => dirty = true,
                                    Action::EditMemory => {
                                        suspend()?;
                                        let result = self.edit_memory();
                                        resume(terminal)?;
                                        if let Err(error) = result {
                                            self.timeline.notice(format!("memory: {error}"));
                                        }
                                        dirty = true;
                                    }
                                    Action::Quit => return Ok(()),
                                }
                            }
                            CrosstermEvent::Resize(_, _) => dirty = true,
                            CrosstermEvent::Mouse(mouse) => {
                                self.on_mouse(mouse);
                                dirty = true;
                            }
                            _ => {}
                        }
                    }
                }
            }
        }
    }

    async fn next_wake(&mut self) -> Wake {
        match (&mut self.events, &mut self.model_events) {
            (Some(events), Some(models)) => tokio::select! {
                event = events.recv() => Wake::Loop(event),
                result = models.recv() => Wake::Models(result),
                _ = tokio::time::sleep(TICK) => Wake::Keys,
            },
            (Some(events), None) => tokio::select! {
                event = events.recv() => Wake::Loop(event),
                _ = tokio::time::sleep(TICK) => Wake::Keys,
            },
            (None, Some(models)) => tokio::select! {
                result = models.recv() => Wake::Models(result),
                _ = tokio::time::sleep(TICK) => Wake::Keys,
            },
            (None, None) => {
                tokio::time::sleep(TICK).await;
                Wake::Keys
            }
        }
    }

    fn draw<B>(&mut self, terminal: &mut Terminal<B>) -> Result<()>
    where
        B: ratatui::backend::Backend,
        B::Error: std::error::Error + Send + Sync + 'static,
    {
        let mode = self.mode;
        let scroll = self.scroll[mode.index()];
        let selected = self.selected_tool_id();
        let approval_height = if self.pending.is_some() { 2 } else { 0 };
        let composer_height = (self.composer.lines().len() as u16 + 1).clamp(2, 7);
        let size = terminal.size()?;
        let body = Layout::vertical([
            Constraint::Min(1),
            Constraint::Length(approval_height),
            Constraint::Length(composer_height),
            Constraint::Length(1),
        ])
        .split(Rect::new(0, 0, size.width, size.height))[0];
        self.body_width = body.width;
        self.body_height = body.height as usize;
        let status = self.status();

        terminal.draw(|frame| {
            let chunks = Layout::vertical([
                Constraint::Min(1),
                Constraint::Length(approval_height),
                Constraint::Length(composer_height),
                Constraint::Length(1),
            ])
            .split(frame.area());

            draw_body(
                frame,
                chunks[0],
                &self.timeline,
                mode,
                scroll,
                &self.expanded,
                selected.as_deref(),
            );
            if let Some(picker) = &self.picker {
                draw_picker(frame, chunks[0], picker);
            }
            if let Some(request) = &self.pending {
                draw_approval(frame, chunks[1], request);
            }
            self.composer.render(frame, chunks[2]);
            draw_status(frame, chunks[3], &status, mode);
        })?;
        Ok(())
    }

    fn status(&self) -> StatusLine<'_> {
        let (used, real_usage) = self.timeline.used();
        StatusLine {
            provider: &self.provider_id,
            model: &self.model,
            used,
            real_usage,
            window: context::context_window_from(
                self.catalog.as_ref(),
                &self.provider_id,
                &self.model,
            ),
            running: self.running,
        }
    }

    fn on_loop_event(&mut self, event: Event) {
        match event {
            Event::ApprovalRequest(request) => {
                if self.always.contains(&request.name) {
                    let _ = request.reply.send(true);
                } else {
                    self.pending = Some(request);
                }
            }
            Event::MessageAdded(message) => {
                if let Err(error) = self.session.append(&message) {
                    self.timeline.notice(format!("session: {error}"));
                }
                self.messages.push(message.clone());
                self.timeline.apply(Event::MessageAdded(message));
            }
            Event::TurnEnd { stop } => {
                self.running = false;
                self.cancel = None;
                self.pending = None;
                self.timeline.apply(Event::TurnEnd { stop });
            }
            event => self.timeline.apply(event),
        }
    }

    fn on_key(&mut self, key: KeyEvent) -> Result<Action> {
        if self.pending.is_some() {
            self.answer_approval(key);
            return Ok(Action::Continue);
        }

        if self.picker.is_some() {
            self.picker_key(key);
            return Ok(Action::Continue);
        }

        if key.code == KeyCode::Esc {
            if self.running {
                self.interrupt();
            }
            return Ok(Action::Continue);
        }

        if key.modifiers.contains(KeyModifiers::CONTROL) {
            match key.code {
                KeyCode::Char('c') => {
                    if self.running {
                        self.interrupt();
                    } else {
                        return Ok(Action::Quit);
                    }
                }
                KeyCode::Char('a') => self.composer.home(),
                KeyCode::Char('e') => self.composer.end(),
                KeyCode::Char('u') => self.composer.kill_to_start(),
                KeyCode::Char('w') => self.composer.delete_word(),
                KeyCode::Char('j') => self.composer.insert_newline(),
                KeyCode::Char('n') if self.mode == Mode::Tools && self.composer.is_empty() => {
                    self.select_tool(1);
                }
                KeyCode::Char('p') if self.mode == Mode::Tools && self.composer.is_empty() => {
                    self.select_tool(-1);
                }
                KeyCode::Up => self.scroll_up(1),
                KeyCode::Down => self.scroll_down(1),
                _ => {}
            }
            return Ok(Action::Continue);
        }

        let mode = self.mode.index();
        match key.code {
            KeyCode::Tab => self.mode = self.mode.next(),
            KeyCode::Char(ch)
                if self.composer.is_empty() && !key.modifiers.contains(KeyModifiers::ALT) =>
            {
                if let Some(mode) = Mode::from_digit(ch) {
                    self.mode = mode;
                } else {
                    self.composer.insert_char(ch);
                }
            }
            KeyCode::PageUp => self.scroll_up(PAGE),
            KeyCode::PageDown => self.scroll_down(PAGE),
            KeyCode::Up => {
                if key.modifiers.contains(KeyModifiers::ALT)
                    || (self.composer.is_empty() && (self.running || self.scroll[mode].back > 0))
                {
                    self.scroll_up(1);
                } else {
                    self.composer.up();
                }
            }
            KeyCode::Down => {
                if key.modifiers.contains(KeyModifiers::ALT)
                    || (self.composer.is_empty() && (self.running || self.scroll[mode].back > 0))
                {
                    self.scroll_down(1);
                } else {
                    self.composer.down();
                }
            }
            KeyCode::Enter
                if key
                    .modifiers
                    .intersects(KeyModifiers::SHIFT | KeyModifiers::ALT) =>
            {
                self.composer.insert_newline();
            }
            KeyCode::Enter => {
                if self.mode == Mode::Tools && self.composer.is_empty() {
                    self.toggle_selected();
                } else if let Some(action) = self.submit()? {
                    return Ok(action);
                }
            }
            KeyCode::Backspace => self.composer.backspace(),
            KeyCode::Delete => self.composer.delete(),
            KeyCode::Left => self.composer.left(),
            KeyCode::Right => self.composer.right(),
            KeyCode::Home => {
                if self.composer.is_empty() {
                    self.scroll_top();
                } else {
                    self.composer.home();
                }
            }
            KeyCode::End => {
                if self.composer.is_empty() {
                    self.scroll_to_bottom();
                } else {
                    self.composer.end();
                }
            }
            KeyCode::Char(ch) if !key.modifiers.contains(KeyModifiers::ALT) => {
                self.composer.insert_char(ch);
            }
            _ => {}
        }
        Ok(Action::Continue)
    }

    fn picker_key(&mut self, key: KeyEvent) {
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            if key.code == KeyCode::Char('c') {
                self.picker = None;
            }
            return;
        }
        match key.code {
            KeyCode::Esc => self.picker = None,
            KeyCode::Enter => self.choose_picker(),
            KeyCode::Up => {
                if let Some(picker) = &mut self.picker {
                    picker.move_by(-1);
                }
            }
            KeyCode::Down => {
                if let Some(picker) = &mut self.picker {
                    picker.move_by(1);
                }
            }
            KeyCode::PageUp => {
                if let Some(picker) = &mut self.picker {
                    picker.move_by(-(PAGE as isize));
                }
            }
            KeyCode::PageDown => {
                if let Some(picker) = &mut self.picker {
                    picker.move_by(PAGE as isize);
                }
            }
            KeyCode::Backspace => {
                if let Some(picker) = &mut self.picker {
                    picker.pop_filter();
                }
            }
            KeyCode::Char(ch) if !key.modifiers.contains(KeyModifiers::ALT) => {
                if let Some(picker) = &mut self.picker {
                    picker.push_filter(ch);
                }
            }
            _ => {}
        }
    }

    fn open_picker(&mut self) {
        self.refresh_models(false);
        let groups = self.picker_groups();
        self.picker = Some(Picker::new(
            groups,
            &self.provider_id,
            &self.model,
            self.models_loading,
        ));
    }

    fn picker_groups(&self) -> Vec<picker::Group> {
        picker::groups(
            self.catalog.as_ref(),
            &self.provider_id,
            &self.model,
            &self.home,
            |name| std::env::var(name).ok(),
        )
    }

    fn choose_picker(&mut self) {
        let Some((provider, model)) = self.picker.as_ref().and_then(Picker::selected_choice) else {
            return;
        };
        self.picker = None;
        self.switch_model(&provider, &model);
    }

    fn set_model(&mut self, model: &str) {
        self.model = model.to_string();
        self.options.model = model.to_string();
        self.timeline.notice(format!("model: {model} (next turn)"));
        if let Err(error) = models::persist_config(&self.home, &self.provider_id, model) {
            self.timeline.notice(format!("config: {error}"));
        }
    }

    /// Rebuild the provider for the next turn and record the switch.
    fn switch_model(&mut self, provider: &str, model: &str) {
        let dev = std::env::var("YOKE_DEV").ok();
        match crate::providers::make(provider, model, dev.as_deref(), &self.home, |name| {
            std::env::var(name).ok()
        }) {
            Ok(built) => {
                self.provider = Arc::from(built);
                self.provider_id = provider.to_string();
                self.model = model.to_string();
                self.options.model = model.to_string();
                self.timeline
                    .notice(format!("model: {provider}/{model} (next turn)"));
                if let Err(error) = models::persist_config(&self.home, provider, model) {
                    self.timeline.notice(format!("config: {error}"));
                }
            }
            Err(error) => self.timeline.notice(format!("model: {error}")),
        }
    }

    fn catalog_has(&self, provider: &str, model: &str) -> bool {
        models::models_for(self.catalog.as_ref(), provider, None)
            .iter()
            .any(|entry| entry.id == model)
    }

    fn refresh_models(&mut self, force: bool) {
        if self.models_loading || self.model_events.is_some() {
            return;
        }
        if !force && models::is_fresh(self.catalog.as_ref(), models::now()) {
            return;
        }
        self.models_loading = true;
        let home = self.home.clone();
        let (base, key) = models::compat_env();
        let (sender, receiver) = mpsc::unbounded_channel();
        self.model_events = Some(receiver);
        tokio::spawn(async move {
            let result = models::fetch(&home, base, key).await;
            let _ = sender.send(result);
        });
    }

    fn on_models(&mut self, result: Option<Result<models::Catalog>>) {
        self.models_loading = false;
        self.model_events = None;
        match result {
            Some(Ok(catalog)) => {
                self.catalog = Some(catalog);
                let groups = self.picker_groups();
                if let Some(picker) = &mut self.picker {
                    picker.set_groups(groups);
                    picker.set_loading(false);
                    picker.set_error(None);
                }
            }
            Some(Err(error)) => {
                if let Some(picker) = &mut self.picker {
                    picker.set_loading(false);
                    picker.set_error(Some(error.to_string()));
                }
            }
            None => {
                if let Some(picker) = &mut self.picker {
                    picker.set_loading(false);
                }
            }
        }
    }

    fn on_mouse(&mut self, mouse: MouseEvent) {
        if self.picker.is_some() {
            let delta = match mouse.kind {
                MouseEventKind::ScrollUp => -(WHEEL as isize),
                MouseEventKind::ScrollDown => WHEEL as isize,
                _ => return,
            };
            if let Some(picker) = &mut self.picker {
                picker.move_by(delta);
            }
            return;
        }
        match mouse.kind {
            MouseEventKind::ScrollUp => self.scroll_up(WHEEL),
            MouseEventKind::ScrollDown => self.scroll_down(WHEEL),
            _ => {}
        }
    }

    /// Lines the current mode can scroll back before reaching its first line.
    fn max_back(&self) -> usize {
        let selected = self.selected_tool_id();
        let total = lines_with(
            &self.timeline,
            self.mode,
            self.body_width,
            &self.expanded,
            selected.as_deref(),
        )
        .len();
        total.saturating_sub(self.body_height)
    }

    /// Scroll up in the current mode, clamped to the content and no longer following.
    fn scroll_up(&mut self, lines: usize) {
        let max = self.max_back();
        if max == 0 {
            self.scroll_to_bottom();
            return;
        }
        let scroll = &mut self.scroll[self.mode.index()];
        scroll.up(lines);
        scroll.back = scroll.back.min(max);
    }

    fn scroll_down(&mut self, lines: usize) {
        self.scroll[self.mode.index()].down(lines);
    }

    fn scroll_top(&mut self) {
        let max = self.max_back();
        let scroll = &mut self.scroll[self.mode.index()];
        scroll.back = max;
        scroll.follow = max == 0;
    }

    fn scroll_to_bottom(&mut self) {
        self.scroll[self.mode.index()] = Scroll::default();
    }

    /// Every tool run in the timeline, in display order.
    fn tool_ids(&self) -> Vec<String> {
        self.timeline
            .entries
            .iter()
            .filter_map(|entry| match &entry.kind {
                timeline::EntryKind::Tool(run) => Some(run.id.clone()),
                _ => None,
            })
            .collect()
    }

    /// The id of the row marked in mode 2, clamped to the runs that exist.
    fn selected_tool_id(&self) -> Option<String> {
        let ids = self.tool_ids();
        if ids.is_empty() {
            return None;
        }
        ids.get(self.selected.min(ids.len() - 1)).cloned()
    }

    /// Move the mode 2 selection by `delta` rows, stopping at either end.
    fn select_tool(&mut self, delta: isize) {
        let len = self.tool_ids().len();
        if len == 0 {
            return;
        }
        self.selected = self.selected.min(len - 1);
        if delta < 0 {
            self.selected = self.selected.saturating_sub(delta.unsigned_abs());
        } else {
            self.selected = (self.selected + delta as usize).min(len - 1);
        }
    }

    /// Expand or fold the selected tool call's output.
    fn toggle_selected(&mut self) {
        if let Some(id) = self.selected_tool_id() {
            if !self.expanded.remove(&id) {
                self.expanded.insert(id);
            }
        }
    }

    fn submit(&mut self) -> Result<Option<Action>> {
        if self.composer.text().trim().is_empty() {
            self.composer.take();
            return Ok(None);
        }
        let text = self.composer.take();
        let text = text.trim().to_string();
        if text.starts_with('/') {
            return self.command(&text);
        }
        if self.running {
            self.composer.set_text(text);
            return Ok(None);
        }
        self.send(text);
        Ok(None)
    }

    fn command(&mut self, input: &str) -> Result<Option<Action>> {
        let mut words = input.splitn(2, char::is_whitespace);
        let command = words.next().unwrap_or_default();
        let rest = words.next().unwrap_or_default().trim();
        match command {
            "/quit" | "/exit" => return Ok(Some(Action::Quit)),
            "/model" => {
                if rest.is_empty() {
                    self.open_picker();
                } else if let Some((provider, model)) = rest.split_once('/') {
                    if provider.is_empty() || model.is_empty() {
                        self.timeline
                            .notice("model: usage: /model, /model <id>, /model <provider>/<id>");
                    } else if known_provider(provider) && !self.catalog_has(&self.provider_id, rest)
                    {
                        self.switch_model(provider, model);
                    } else {
                        // A model id may contain a slash (`openai/gpt-oss-120b`).
                        self.set_model(rest);
                    }
                } else {
                    self.set_model(rest);
                }
            }
            "/memory" => return Ok(Some(Action::EditMemory)),
            "/clear" if self.running => self
                .timeline
                .notice("/clear: interrupt the running turn first"),
            "/clear" => match Session::create(&self.home, &self.slug) {
                Ok(session) => {
                    self.options.conversation_id = conversation_id(&session);
                    self.session = session;
                    self.messages.clear();
                    self.timeline.clear_messages();
                    self.scroll = [Scroll::default(); 3];
                    self.expanded.clear();
                    self.selected = 0;
                    self.timeline.notice(format!(
                        "new conversation · session {}",
                        self.session.path().display()
                    ));
                }
                Err(error) => self.timeline.notice(format!("/clear: {error}")),
            },
            "/save" => self
                .timeline
                .notice(format!("session: {}", self.session.path().display())),
            other => self.timeline.notice(format!(
                "unknown command: {other} · try /model, /memory, /clear, /save, /quit"
            )),
        }
        Ok(None)
    }

    fn send(&mut self, text: String) {
        let message = Message {
            role: Role::User,
            parts: vec![Part::Text { text }],
        };
        if let Err(error) = self.session.append(&message) {
            self.timeline.notice(format!("session: {error}"));
        }
        self.messages.push(message.clone());
        self.timeline.push_message(message);
        self.spawn_turn();
    }

    fn spawn_turn(&mut self) {
        self.running = true;
        self.scroll = [Scroll::default(); 3];
        let cancel = CancelToken::new();
        self.cancel = Some(cancel.clone());
        let provider = Arc::clone(&self.provider);
        let registry = Arc::clone(&self.registry);
        let options = self.options.clone();
        let mut messages = self.messages.clone();
        let (tx, rx) = mpsc::unbounded_channel();
        self.events = Some(rx);
        tokio::spawn(async move {
            let _ = r#loop::run(&*provider, &registry, &options, &mut messages, &cancel, &tx).await;
        });
    }

    fn answer_approval(&mut self, key: KeyEvent) {
        let answer = match key.code {
            KeyCode::Esc => {
                self.interrupt();
                return;
            }
            KeyCode::Char(ch) => match ch.to_ascii_lowercase() {
                'y' => Some(true),
                'n' => Some(false),
                'a' => {
                    if let Some(request) = &self.pending {
                        self.always.insert(request.name.clone());
                    }
                    Some(true)
                }
                _ => None,
            },
            _ => None,
        };
        if let Some(allow) = answer {
            if let Some(request) = self.pending.take() {
                let _ = request.reply.send(allow);
            }
        }
    }

    fn interrupt(&mut self) {
        if let Some(cancel) = &self.cancel {
            cancel.cancel();
        }
        if let Some(request) = self.pending.take() {
            let _ = request.reply.send(false);
        }
    }

    fn edit_memory(&mut self) -> Result<()> {
        if !self.memory_enabled {
            self.timeline.notice("memory is disabled (--no-memory)");
            return Ok(());
        }
        if let Some(parent) = self.memory_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        if !self.memory_path.exists() {
            std::fs::write(&self.memory_path, "")?;
        }

        let editor = std::env::var("EDITOR").unwrap_or_else(|_| "vi".to_string());
        let mut words = editor.split_whitespace();
        let Some(command) = words.next() else {
            anyhow::bail!("$EDITOR is empty");
        };
        let status = ProcessCommand::new(command)
            .args(words)
            .arg(&self.memory_path)
            .status()?;
        if !status.success() {
            anyhow::bail!("{editor} exited with {status}");
        }

        let memory = memory::read(&self.memory_path);
        let instructions = prompt::build(
            &self.workspace,
            &clock::date(SystemTime::now()),
            &self.registry.names(),
            Some((&self.memory_path, &memory)),
        );
        self.options.instructions = instructions.clone();
        self.timeline.set_context(instructions, Some(memory));
        self.timeline
            .notice(format!("memory: {}", self.memory_path.display()));
        Ok(())
    }
}

fn conversation_id(session: &Session) -> Option<String> {
    session
        .path()
        .file_stem()
        .and_then(|stem| stem.to_str())
        .map(str::to_string)
}

fn known_provider(id: &str) -> bool {
    id == "mock" || crate::providers::PROVIDERS.iter().any(|info| info.id == id)
}

fn draw_approval(frame: &mut Frame, area: Rect, request: &ApprovalRequest) {
    let args = serde_json::to_string(&request.args).unwrap_or_else(|_| "{}".into());
    let args: String = if args.chars().count() > 120 {
        let mut cut: String = args.chars().take(119).collect();
        cut.push('…');
        cut
    } else {
        args
    };
    let lines = vec![
        Line::from(Span::styled(
            format!("Allow {} {args}?", request.name),
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(
            format!(
                "[y] yes   [n] no   [a] always for {}   [Esc] interrupt",
                request.name
            ),
            Style::default().add_modifier(Modifier::DIM),
        )),
    ];
    frame.render_widget(Paragraph::new(lines), area);
}

fn draw_picker(frame: &mut Frame, area: Rect, picker: &Picker) {
    if area.height < 4 || area.width < 12 {
        return;
    }
    let max_rows = area.height.saturating_sub(2) as usize;
    let rows = picker.rows_window(max_rows);
    let height = (rows.len() as u16 + 2).min(area.height);
    let overlay = Rect {
        x: area.x,
        y: area.y + area.height - height,
        width: area.width,
        height,
    };
    let block = Block::bordered()
        .title(" model ")
        .border_style(Style::default().fg(Color::Cyan));
    frame.render_widget(Clear, overlay);
    frame.render_widget(Paragraph::new(rows).block(block), overlay);
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use std::collections::BTreeMap;
    use tokio::sync::oneshot;

    use crate::providers::mock::MockProvider;
    use crate::session::Session;
    use crate::tools::testutil::TempDir;
    use crate::tui::timeline::{EntryKind, RunStatus, ToolRun};

    fn app(home: &TempDir) -> App {
        App::new(Setup {
            provider: Box::new(MockProvider::dev()),
            registry: Registry::new(),
            options: Options::new("mock", "system"),
            messages: Vec::new(),
            session: Session::create(home.path(), "-w").unwrap(),
            instructions: "system".into(),
            memory: Some((home.path().join("memory.md"), String::new())),
            memory_enabled: true,
            memory_path: home.path().join("memory.md"),
            home: home.path().to_path_buf(),
            slug: "-w".into(),
            workspace: home.path().to_path_buf(),
            provider_id: "mock".into(),
            model: "mock".into(),
        })
    }

    fn approval(name: &str) -> (ApprovalRequest, oneshot::Receiver<bool>) {
        let (reply, granted) = oneshot::channel();
        (
            ApprovalRequest {
                id: "c1".into(),
                name: name.into(),
                args: serde_json::json!({"path": "a.rs"}),
                reply,
            },
            granted,
        )
    }

    fn press(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn buffer_text(terminal: &Terminal<TestBackend>) -> String {
        let buffer = terminal.backend().buffer();
        let mut text = String::new();
        for y in 0..buffer.area.height {
            let mut line = String::new();
            for x in 0..buffer.area.width {
                line.push_str(buffer.cell((x, y)).map_or(" ", |cell| cell.symbol()));
            }
            text.push_str(line.trim_end());
            if y + 1 < buffer.area.height {
                text.push('\n');
            }
        }
        text.trim_end().to_string()
    }

    fn render(timeline: &Timeline, width: u16, height: u16) -> String {
        render_state(
            timeline,
            Mode::Overview,
            width,
            height,
            &HashSet::new(),
            None,
        )
    }

    fn render_state(
        timeline: &Timeline,
        mode: Mode,
        width: u16,
        height: u16,
        expanded: &HashSet<String>,
        selected: Option<&str>,
    ) -> String {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        let status = StatusLine {
            provider: "mock",
            model: "mock",
            used: timeline.used().0,
            real_usage: false,
            window: None,
            running: false,
        };
        let view = View {
            timeline,
            mode,
            scroll: Scroll::default(),
            expanded,
            selected,
            status,
        };
        terminal.draw(|frame| draw(frame, &view)).unwrap();
        buffer_text(&terminal)
    }

    fn draw_app(app: &mut App, width: u16, height: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        app.draw(&mut terminal).unwrap();
        buffer_text(&terminal)
    }

    /// The number of lines the app's current mode renders at its last body width.
    fn body_len(app: &App) -> usize {
        let selected = app.selected_tool_id();
        lines_with(
            &app.timeline,
            app.mode,
            app.body_width,
            &app.expanded,
            selected.as_deref(),
        )
        .len()
    }

    fn wheel(kind: MouseEventKind) -> MouseEvent {
        MouseEvent {
            kind,
            column: 0,
            row: 0,
            modifiers: KeyModifiers::NONE,
        }
    }

    /// An app whose timeline is long enough to scroll in a 80×24 terminal.
    fn long_app(home: &TempDir) -> App {
        let mut app = app(home);
        for n in 1..=60 {
            app.on_loop_event(Event::Notice(format!("note {n:02}")));
        }
        app
    }

    #[test]
    fn mouse_wheel_scrolls_three_lines_and_shows_earlier_content() {
        let home = TempDir::new("tui-wheel");
        let mut app = long_app(&home);
        let bottom = draw_app(&mut app, 80, 24);
        assert!(bottom.contains("note 60"));
        assert!(app.scroll[0].follow);

        app.on_mouse(wheel(MouseEventKind::ScrollUp));
        assert_eq!(app.scroll[0].back, WHEEL);
        assert!(!app.scroll[0].follow);
        let scrolled = draw_app(&mut app, 80, 24);
        assert_ne!(bottom, scrolled, "the body did not move");
        assert!(
            !scrolled.contains("note 60"),
            "the last note should be hidden"
        );
        assert_eq!(app.scroll[1].back, 0, "scroll is per mode");
        assert_eq!(app.scroll[2].back, 0, "scroll is per mode");

        // Scrolling all the way up stops at the first line.
        for _ in 0..50 {
            app.on_mouse(wheel(MouseEventKind::ScrollUp));
        }
        let total = body_len(&app);
        assert_eq!(app.scroll[0].back, total.saturating_sub(app.body_height));
        let top = draw_app(&mut app, 80, 24);
        assert!(top.contains("note 01"), "earlier lines should be visible");
        assert!(!top.contains("note 60"));

        for _ in 0..50 {
            app.on_mouse(wheel(MouseEventKind::ScrollDown));
        }
        assert_eq!(app.scroll[0].back, 0);
        assert!(app.scroll[0].follow);
        assert_eq!(draw_app(&mut app, 80, 24), bottom);
    }

    #[test]
    fn keyboard_scrolls_home_end_and_the_composer_coexist() {
        let home = TempDir::new("tui-keys");
        let mut app = long_app(&home);
        let bottom = draw_app(&mut app, 80, 24);

        // At the bottom with an idle turn, Up still drives composer history.
        app.composer.set_text("older");
        app.composer.take();
        app.on_key(press(KeyCode::Up)).unwrap();
        assert_eq!(app.scroll[0].back, 0);
        assert_eq!(app.composer.text(), "older");
        app.composer.take();

        // PageUp scrolls a page and disables follow; then Up scrolls a line
        // instead of touching history.
        app.on_key(press(KeyCode::PageUp)).unwrap();
        assert_eq!(app.scroll[0].back, PAGE);
        assert!(!app.scroll[0].follow);
        app.on_key(press(KeyCode::Up)).unwrap();
        assert_eq!(app.scroll[0].back, PAGE + 1);
        assert_eq!(app.composer.text(), "");

        // Home shows the top; End returns to the bottom and follows again.
        app.on_key(press(KeyCode::Home)).unwrap();
        let max = body_len(&app).saturating_sub(app.body_height);
        assert_eq!(app.scroll[0].back, max);
        let top = draw_app(&mut app, 80, 24);
        assert!(top.contains("note 01"));
        assert!(!top.contains("note 60"));
        app.on_key(press(KeyCode::End)).unwrap();
        assert_eq!(app.scroll[0].back, 0);
        assert!(app.scroll[0].follow);
        assert_eq!(draw_app(&mut app, 80, 24), bottom);

        // During a running turn, Up scrolls even at the bottom.
        app.running = true;
        app.on_key(press(KeyCode::Up)).unwrap();
        assert_eq!(app.scroll[0].back, 1);
        app.running = false;

        // Ctrl/Alt arrows always scroll, even with text in the composer.
        app.composer.set_text("draft");
        app.on_key(KeyEvent::new(KeyCode::Up, KeyModifiers::CONTROL))
            .unwrap();
        assert_eq!(app.scroll[0].back, 2);
        app.on_key(KeyEvent::new(KeyCode::Down, KeyModifiers::ALT))
            .unwrap();
        assert_eq!(app.scroll[0].back, 1);
        assert_eq!(app.composer.text(), "draft");
    }

    #[test]
    fn scroll_positions_are_per_mode_and_clamped_to_each_modes_content() {
        let home = TempDir::new("tui-per-mode");
        let mut app = long_app(&home);

        for mode in [Mode::Overview, Mode::Tools, Mode::Raw] {
            app.mode = mode;
            draw_app(&mut app, 80, 24);
            let before = app.scroll;
            for _ in 0..500 {
                app.on_mouse(wheel(MouseEventKind::ScrollUp));
            }
            let total = body_len(&app);
            assert!(total > app.body_height, "{mode:?} is too short to scroll");
            assert_eq!(
                app.scroll[mode.index()].back,
                total - app.body_height,
                "{mode:?} scroll should stop at its first line"
            );
            assert_eq!(
                app.scroll[mode.index()].start(total, app.body_height),
                0,
                "{mode:?} scrolled past the top"
            );
            for other in [Mode::Overview, Mode::Tools, Mode::Raw] {
                if other != mode {
                    assert_eq!(app.scroll[other.index()], before[other.index()]);
                }
            }
        }
    }

    #[tokio::test]
    async fn sending_a_message_returns_every_mode_to_the_bottom() {
        let home = TempDir::new("tui-send-scroll");
        let mut app = long_app(&home);
        draw_app(&mut app, 80, 24);
        app.on_key(press(KeyCode::PageUp)).unwrap();
        app.mode = Mode::Tools;
        app.on_key(press(KeyCode::PageUp)).unwrap();
        assert_ne!(app.scroll[0], Scroll::default());
        assert_ne!(app.scroll[1], Scroll::default());

        app.mode = Mode::Overview;
        app.composer.set_text("hello");
        app.on_key(press(KeyCode::Enter)).unwrap();
        assert_eq!(app.scroll, [Scroll::default(); 3]);
    }

    fn model_catalog() -> models::Catalog {
        use crate::models::Model;
        let model = |id: &str, name: &str, context: usize| Model {
            id: id.into(),
            name: name.into(),
            context: Some(context),
        };
        models::Catalog {
            fetched_at: models::now(),
            providers: BTreeMap::from([
                (
                    "openai".to_string(),
                    vec![
                        model("gpt-5.4", "GPT-5.4", 400_000),
                        model("gpt-5.5", "GPT-5.5", 1_000_000),
                    ],
                ),
                (
                    "compat".to_string(),
                    vec![model("local-model", "Local Model", 32_768)],
                ),
            ]),
        }
    }

    fn open_model_picker(app: &mut App) {
        let groups = picker::groups(
            app.catalog.as_ref(),
            &app.provider_id,
            &app.model,
            &app.home,
            |_| None,
        );
        app.picker = Some(Picker::new(groups, &app.provider_id, &app.model, false));
    }

    #[test]
    fn model_picker_lists_connected_providers_filters_and_switches_on_enter() {
        let home = TempDir::new("tui-model-picker");
        let mut app = app(&home);
        app.catalog = Some(model_catalog());
        app.provider_id = "openai".into();
        app.model = "gpt-5.4".into();
        crate::auth::set_endpoint(home.path(), "compat", "http://localhost:11434/v1", None)
            .unwrap();
        open_model_picker(&mut app);

        let picker = app.picker.as_ref().unwrap();
        let providers: Vec<&str> = picker
            .visible_groups()
            .iter()
            .map(|(group, _)| group.provider.as_str())
            .collect();
        assert_eq!(
            providers,
            ["openai", "compat"],
            "the active provider plus the configured compat server"
        );

        let overview = draw_app(&mut app, 80, 24);
        for expected in [
            " model ",
            "openai · OpenAI",
            "gpt-5.4",
            "400k",
            "current",
            "Enter select",
        ] {
            assert!(
                overview.contains(expected),
                "missing {expected:?}:\n{overview}"
            );
        }

        for ch in "local".chars() {
            app.on_key(press(KeyCode::Char(ch))).unwrap();
        }
        let filtered = draw_app(&mut app, 80, 24);
        assert!(filtered.contains("local-model"), "{filtered}");
        assert!(!filtered.contains("GPT-5.4 (gpt-5.4)"), "{filtered}");
        assert_eq!(
            app.picker.as_ref().unwrap().selected_choice().unwrap().1,
            "local-model"
        );

        // A pre-existing config.toml is updated; otherwise nothing is written.
        std::fs::write(home.path().join("config.toml"), "provider = \"mock\"\n").unwrap();
        app.on_key(press(KeyCode::Enter)).unwrap();
        assert!(app.picker.is_none());
        assert_eq!(app.provider_id, "compat");
        assert_eq!(app.model, "local-model");
        assert_eq!(app.options.model, "local-model");
        assert!(app.timeline.entries.iter().any(|entry| matches!(
            &entry.kind,
            EntryKind::Notice { text, .. } if text.contains("model: compat/local-model")
        )));
        let config = std::fs::read_to_string(home.path().join("config.toml")).unwrap();
        assert!(config.contains("provider = \"compat\""), "{config}");
        assert!(config.contains("model = \"local-model\""), "{config}");
    }

    #[test]
    fn model_command_opens_the_picker_and_accepts_both_direct_forms() {
        let home = TempDir::new("tui-model-command");
        let mut app = app(&home);
        app.catalog = Some(model_catalog());
        crate::auth::set_endpoint(home.path(), "compat", "http://localhost:11434/v1", None)
            .unwrap();

        app.command("/model").unwrap();
        assert!(app.picker.is_some());
        app.on_key(press(KeyCode::Esc)).unwrap();
        assert!(app.picker.is_none());

        app.command("/model compat/local-model").unwrap();
        assert_eq!(app.provider_id, "compat");
        assert_eq!(app.model, "local-model");

        // `openai/gpt-oss-120b` is a groq model id, not a provider switch.
        let mut catalog = model_catalog();
        catalog.providers.insert(
            "groq".to_string(),
            vec![crate::models::Model {
                id: "openai/gpt-oss-120b".into(),
                name: "GPT OSS 120B".into(),
                context: Some(131_072),
            }],
        );
        app.catalog = Some(catalog);
        app.provider_id = "groq".into();
        app.command("/model openai/gpt-oss-120b").unwrap();
        assert_eq!(app.provider_id, "groq");
        assert_eq!(app.model, "openai/gpt-oss-120b");
    }

    #[test]
    fn overview_snapshot_has_header_messages_folded_tools_and_streaming() {
        let mut timeline = Timeline::new(
            "You are Yoke.",
            Some((
                PathBuf::from("/home/u/.yoke/projects/-w/memory.md"),
                "prefers small diffs".into(),
            )),
            vec![serde_json::json!({"name": "read"})],
        );
        timeline.push_message(Message {
            role: Role::User,
            parts: vec![Part::Text {
                text: "please change it".into(),
            }],
        });
        timeline.push_message(Message {
            role: Role::Assistant,
            parts: vec![Part::Text {
                text: "reading both files".into(),
            }],
        });
        for (id, name, args) in [
            ("c1", "read", serde_json::json!({"path": "a.rs"})),
            ("c2", "read", serde_json::json!({"path": "b.rs"})),
            (
                "c3",
                "edit",
                serde_json::json!({"path": "a.rs", "old_string": "one\ntwo", "new_string": "one\ntwo\nthree"}),
            ),
        ] {
            timeline.apply(Event::ToolStart {
                id: id.into(),
                name: name.into(),
                args,
            });
            timeline.apply(Event::ToolEnd {
                id: id.into(),
                result: crate::tools::ToolResult::ok("ok"),
                status: crate::tools::ToolStatus::Ok,
            });
        }
        // A live streaming buffer, as if the next assistant text were arriving.
        timeline.entries.push(timeline::Entry {
            step: Some(2),
            kind: EntryKind::Streaming {
                text: "streaming now".into(),
                reasoning: String::new(),
            },
        });

        let rendered = render(&timeline, 60, 20);
        let expected = "\
context
  system prompt   ~4 tokens
  memory          ~5 tokens
  tools           ~5 tokens
  messages        ~36 tokens
  total           ~50 tokens
────────────────────────────────────────

you
please change it

assistant
reading both files

▸ 3 tools · read ×2, edit · +3 −2

assistant
streaming now▌

 OVERVIEW  mock/mock │ ~50 / ? tokens │ · idle";
        assert_eq!(rendered, expected);
    }

    /// One timeline with everything the three modes render: messages, reasoning,
    /// a three-call run (edit diff + long bash output), usage and a wire body.
    fn scripted() -> Timeline {
        use crate::types::{StreamEvent, Usage};
        use serde_json::json;

        let mut timeline = Timeline::new(
            "You are Yoke.\nRead files before editing.",
            Some((
                PathBuf::from("/home/u/.yoke/projects/-w/memory.md"),
                "prefers small diffs".into(),
            )),
            vec![json!({"name": "read", "description": "read a file"})],
        );
        timeline.push_message(Message {
            role: Role::User,
            parts: vec![Part::Text {
                text: "please change a.rs and run the tests".into(),
            }],
        });
        timeline.push_message(Message {
            role: Role::Assistant,
            parts: vec![
                Part::Reasoning {
                    text: "checking the file first".into(),
                    raw: None,
                },
                Part::Text {
                    text: "reading a.rs".into(),
                },
                Part::ToolCall {
                    id: "c1".into(),
                    name: "read".into(),
                    args: json!({"path": "a.rs"}),
                },
                Part::ToolCall {
                    id: "c2".into(),
                    name: "edit".into(),
                    args: json!({
                        "path": "a.rs",
                        "old_string": "one\ntwo",
                        "new_string": "one\ntwo\nthree"
                    }),
                },
                Part::ToolCall {
                    id: "c3".into(),
                    name: "bash".into(),
                    args: json!({"command": "cargo test"}),
                },
            ],
        });

        let long_output = (1..=20)
            .map(|n| format!("test line {n}"))
            .collect::<Vec<_>>()
            .join("\n");
        for (id, name, args, output) in [
            ("c1", "read", json!({"path": "a.rs"}), "one\ntwo"),
            (
                "c2",
                "edit",
                json!({
                    "path": "a.rs",
                    "old_string": "one\ntwo",
                    "new_string": "one\ntwo\nthree"
                }),
                "edited a.rs",
            ),
            (
                "c3",
                "bash",
                json!({"command": "cargo test"}),
                long_output.as_str(),
            ),
        ] {
            timeline.apply(Event::ToolStart {
                id: id.into(),
                name: name.into(),
                args,
            });
            timeline.apply(Event::ToolEnd {
                id: id.into(),
                result: crate::tools::ToolResult::ok(output),
                status: crate::tools::ToolStatus::Ok,
            });
        }
        timeline.push_message(Message {
            role: Role::Tool,
            parts: vec![Part::ToolResult {
                id: "c3".into(),
                output: long_output,
                is_error: false,
            }],
        });
        timeline.apply(Event::Stream(StreamEvent::Usage(Usage {
            input_tokens: 120,
            output_tokens: 45,
        })));
        timeline.apply(Event::Stream(StreamEvent::Wire(json!({
            "model": "mock",
            "headers": {"Authorization": "Bearer secret"},
            "messages": []
        }))));
        timeline
    }

    #[test]
    fn each_mode_renders_the_same_scripted_timeline() {
        let timeline = scripted();

        let overview = render_state(&timeline, Mode::Overview, 100, 40, &HashSet::new(), None);
        for expected in [
            "context",
            "system prompt",
            "please change a.rs and run the tests",
            "reading a.rs",
            "▸ 3 tools · read, edit, bash · +3 −2",
        ] {
            assert!(overview.contains(expected), "overview missing {expected:?}");
        }

        let tools = render_state(&timeline, Mode::Tools, 100, 40, &HashSet::new(), None);
        for expected in [
            "● read",
            "● edit",
            "● bash",
            "  │ - one",
            "  │ - two",
            "  │ + three",
            "… 12 lines …",
            "reasoning",
            "checking the file first",
        ] {
            assert!(tools.contains(expected), "tools missing {expected:?}");
        }
        assert!(!tools.contains("▸"));

        let raw = render_state(&timeline, Mode::Raw, 100, 400, &HashSet::new(), None);
        for expected in [
            "system",
            "You are Yoke.",
            "memory · /home/u/.yoke/projects/-w/memory.md",
            "tool schemas · 1",
            "tool 1 · read",
            "message 1 · user",
            "message 2 · assistant",
            "tool run · read · ok",
            "tool call c1 · read",
            "tool result c3 · ok",
            "usage",
            "input tokens: 120",
            "request body · secrets redacted",
            "[redacted]",
        ] {
            assert!(raw.contains(expected), "raw missing {expected:?}");
        }
    }

    #[test]
    fn rendering_is_a_pure_function_of_timeline_mode_scroll_and_width() {
        let timeline = scripted();
        for mode in [Mode::Overview, Mode::Tools, Mode::Raw] {
            let first = render_state(&timeline, mode, 90, 30, &HashSet::new(), None);
            let second = render_state(&timeline, mode, 90, 30, &HashSet::new(), None);
            assert_eq!(first, second, "{:?} rendering is not stable", mode);
        }
    }

    #[test]
    fn narrow_width_renders_every_mode_without_panicking() {
        let timeline = scripted();
        for mode in [Mode::Overview, Mode::Tools, Mode::Raw] {
            let rendered = render_state(&timeline, mode, 40, 30, &HashSet::new(), None);
            assert!(!rendered.is_empty(), "{mode:?} rendered nothing");
        }
    }

    #[test]
    fn the_status_line_shows_mode_model_and_context_in_every_mode() {
        let timeline = scripted();
        for (mode, badge) in [
            (Mode::Overview, "OVERVIEW"),
            (Mode::Tools, "TOOLS"),
            (Mode::Raw, "RAW"),
        ] {
            let rendered = render_state(&timeline, mode, 100, 40, &HashSet::new(), None);
            let status = rendered.lines().last().unwrap();
            assert!(status.contains(badge), "{mode:?}: {status}");
            assert!(status.contains("mock/mock"), "{mode:?}: {status}");
            assert!(status.contains("/ ? tokens"), "{mode:?}: {status}");
        }
    }

    #[test]
    fn ctrl_n_and_ctrl_p_select_tool_rows_and_enter_expands_them() {
        let home = TempDir::new("tui-selection");
        let mut app = app(&home);
        app.mode = Mode::Tools;

        let long_output = (1..=20)
            .map(|n| format!("line {n}"))
            .collect::<Vec<_>>()
            .join("\n");
        for (id, name, args, output) in [
            ("c1", "read", serde_json::json!({"path": "a.rs"}), "one"),
            (
                "c2",
                "bash",
                serde_json::json!({"command": "seq"}),
                long_output.as_str(),
            ),
        ] {
            app.on_loop_event(Event::ToolStart {
                id: id.into(),
                name: name.into(),
                args,
            });
            app.on_loop_event(Event::ToolEnd {
                id: id.into(),
                result: crate::tools::ToolResult::ok(output),
                status: crate::tools::ToolStatus::Ok,
            });
        }

        let ctrl = |ch| KeyEvent::new(KeyCode::Char(ch), KeyModifiers::CONTROL);
        assert_eq!(app.selected_tool_id().as_deref(), Some("c1"));
        app.on_key(ctrl('n')).unwrap();
        assert_eq!(app.selected_tool_id().as_deref(), Some("c2"));
        app.on_key(ctrl('n')).unwrap();
        assert_eq!(app.selected_tool_id().as_deref(), Some("c2"));
        app.on_key(ctrl('p')).unwrap();
        assert_eq!(app.selected_tool_id().as_deref(), Some("c1"));

        // The selected row is marked, and Enter expands its output.
        let state = |app: &App| {
            render_state(
                &app.timeline,
                app.mode,
                80,
                40,
                &app.expanded,
                app.selected_tool_id().as_deref(),
            )
        };
        assert!(state(&app).contains("▶ ● read"));

        app.on_key(ctrl('n')).unwrap();
        assert!(!state(&app).contains("│ line 5"));
        app.on_key(press(KeyCode::Enter)).unwrap();
        assert!(app.expanded.contains("c2"));
        assert!(state(&app).contains("│ line 5"));
        app.on_key(press(KeyCode::Enter)).unwrap();
        assert!(!app.expanded.contains("c2"));

        // With text in the composer the keys type instead of selecting.
        app.on_key(press(KeyCode::Char('x'))).unwrap();
        app.on_key(ctrl('p')).unwrap();
        assert_eq!(app.selected_tool_id().as_deref(), Some("c2"));
        assert_eq!(app.composer.text(), "x");

        // Overview's Enter with an empty composer never toggles a tool.
        app.composer.take();
        app.mode = Mode::Overview;
        app.on_key(press(KeyCode::Enter)).unwrap();
        assert!(app.expanded.is_empty());
    }

    #[test]
    fn tabs_cycle_modes_and_digits_jump() {
        assert_eq!(Mode::Overview.next(), Mode::Tools);
        assert_eq!(Mode::Tools.next(), Mode::Raw);
        assert_eq!(Mode::Raw.next(), Mode::Overview);
        assert_eq!(Mode::from_digit('2'), Some(Mode::Tools));
        assert_eq!(Mode::from_digit('9'), None);
    }

    #[test]
    fn scroll_starts_at_the_bottom_and_clamps() {
        let follow = Scroll::default();
        assert_eq!(follow.start(100, 10), 90);
        let mut scroll = Scroll::default();
        scroll.up(5);
        assert_eq!(scroll.start(100, 10), 85);
        scroll.down(5);
        assert!(scroll.follow);
        assert_eq!(scroll.start(100, 10), 90);
        scroll.up(1000);
        assert_eq!(scroll.start(100, 10), 0);
    }

    #[test]
    fn wrap_preserves_newlines_and_limits_width() {
        assert_eq!(wrap("abcdef", 3), ["abc", "def"]);
        assert_eq!(wrap("a\n\nb", 3), ["a", "", "b"]);
        assert_eq!(wrap("", 3), [""]);
    }

    #[test]
    fn tool_runs_and_approvals_do_not_leak_into_the_timeline() {
        let mut timeline = Timeline::new("s", None, vec![]);
        timeline.apply(Event::Notice("hi".into()));
        let notes = timeline
            .entries
            .iter()
            .filter(|entry| matches!(entry.kind, EntryKind::Notice { .. }))
            .count();
        assert_eq!(notes, 1);

        let run = ToolRun {
            id: "c1".into(),
            name: "read".into(),
            args: serde_json::json!({}),
            status: RunStatus::Running,
            output: String::new(),
            duration: None,
            started: None,
        };
        assert_eq!(run.status.label(), "running");
    }

    #[test]
    fn approvals_answer_yes_no_always_and_always_is_sticky() {
        let home = TempDir::new("tui-approval");
        let mut app = app(&home);

        let (request, granted) = approval("bash");
        app.pending = Some(request);
        app.answer_approval(press(KeyCode::Char('y')));
        assert!(granted.blocking_recv().unwrap());

        let (request, granted) = approval("bash");
        app.pending = Some(request);
        app.answer_approval(press(KeyCode::Char('a')));
        assert!(granted.blocking_recv().unwrap());
        assert!(app.always.contains("bash"));

        let (request, granted) = approval("bash");
        app.on_loop_event(Event::ApprovalRequest(request));
        assert!(app.pending.is_none());
        assert!(granted.blocking_recv().unwrap());

        let (request, granted) = approval("write");
        app.pending = Some(request);
        app.answer_approval(press(KeyCode::Char('n')));
        assert!(!granted.blocking_recv().unwrap());
    }

    #[test]
    fn escaping_an_approval_interrupts_the_turn() {
        let home = TempDir::new("tui-interrupt");
        let mut app = app(&home);
        let cancel = CancelToken::new();
        app.cancel = Some(cancel.clone());

        let (request, granted) = approval("bash");
        app.pending = Some(request);
        app.answer_approval(press(KeyCode::Esc));

        assert!(cancel.is_cancelled());
        assert!(app.pending.is_none());
        assert!(!granted.blocking_recv().unwrap());
    }

    #[test]
    fn model_clear_and_unknown_commands_are_visible() {
        let home = TempDir::new("tui-commands");
        let mut app = app(&home);
        let old_session = app.session.path().to_path_buf();

        app.command("/model other").unwrap();
        assert_eq!(app.model, "other");
        assert_eq!(app.options.model, "other");
        assert!(app.timeline.entries.iter().any(|entry| matches!(
            &entry.kind,
            EntryKind::Notice { text, .. } if text.contains("model: other")
        )));

        app.messages.push(Message {
            role: Role::User,
            parts: vec![Part::Text { text: "hi".into() }],
        });
        app.timeline.push_message(Message {
            role: Role::User,
            parts: vec![Part::Text { text: "hi".into() }],
        });
        app.command("/clear").unwrap();
        assert!(app.messages.is_empty());
        assert_eq!(app.timeline.entries.len(), 4); // header + the notice
        assert_ne!(app.session.path(), old_session);
        assert_eq!(app.options.conversation_id, conversation_id(&app.session));

        let session = app.session.path().to_path_buf();
        app.running = true;
        app.command("/clear").unwrap();
        assert_eq!(app.session.path(), session);
        assert!(app.timeline.entries.iter().any(|entry| matches!(
            &entry.kind,
            EntryKind::Notice { text, .. } if text.contains("interrupt the running turn")
        )));
        app.running = false;

        app.command("/nope").unwrap();
        assert!(app.timeline.entries.iter().any(|entry| matches!(
            &entry.kind,
            EntryKind::Notice { text, .. } if text.contains("unknown command")
        )));
    }
}
