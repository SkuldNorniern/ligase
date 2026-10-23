//! app state, input routing and one frame. long work goes to the file
//! worker; results come back through `Done`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use aurea::render::{CanvasId, DrawingContext, Rect, request_canvas_redraw};
use aurea::ui::{Align, DockStyle, DockTabEvent, DockView, Theme, Ui, UiState};
use aurea::{AureaResult, Chord, KeyCode, Keymap, WindowEvent};

use resolvase::{Encoding, Point, SavePoint};

use crate::document::{self, DocumentId, Documents, ViewId};
use crate::editor::{self, LineOp, input};
use crate::explorer::{self, Click, Explorer};
use crate::jobs::Jobs;
use crate::lang::{Lang, Texts};
use crate::lsp::{self, Lsp};
use crate::search::{self, Finder};
use crate::syntax::Syntax;
use crate::terminal::{self, Shell};
use crate::workbench::panels::{PaneId, Panel};
use crate::workbench::{self, Refused, Workbench};

const STATUS_HEIGHT: f32 = 24.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Id {
    Dock,
    Explorer,
    Editor(ViewId),
    Terminal,
    SearchField,
    SearchList,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    Save,
    Undo,
    Redo,
    Cut,
    Copy,
    Paste,
    SelectAll,
    Line(LineOp),
    GotoDefinition,
    ShowHover,
    FindInFiles,
    ToggleExplorer,
    ToggleTerminal,
    ResetLayout,
}

/// what the worker hands back.
enum Done {
    Listed(PathBuf, Result<Vec<(String, bool)>, String>),
    Loaded(DocumentId, Result<String, String>),
    Session(Option<String>),
    Saved(Result<(), String>),
    Wrote(DocumentId, String, SavePoint, Result<(), String>),
    Shell(Result<vanta::Terminal, String>),
    Server(&'static str, Result<lsp::server::Server, String>),
}

pub struct App {
    pub ui: UiState<Id>,
    theme: Theme,
    pub texts: Texts,
    pub keymap: Keymap<Command>,
    bench: Workbench,
    docs: Documents,
    explorer: Explorer,
    jobs: Jobs<Done>,
    syntax: Syntax,
    shell: Shell,
    finder: Finder,
    lsp: Lsp,
    /// hover text for a view, while its caret stays where it was asked.
    hover: Option<(ViewId, usize, String)>,
    /// a whole line copied with nothing selected; pasting it goes in above.
    line_clip: Option<String>,
    /// the terminal panel had the keyboard last frame.
    shell_focus: bool,
    canvas: Option<CanvasId>,
    /// session file for this workspace; none when there is no config dir.
    session: Option<PathBuf>,
    status: String,
    /// the editor view with the keyboard, from the last frame.
    focus: Option<ViewId>,
    /// caret box of that view, for the ime window.
    pub ime: Option<Rect>,
    /// the layout changed since it was last written.
    layout_dirty: bool,
    /// the saved session came back, or there was none.
    restored: bool,
    /// a save is out and the window closes once it is back.
    closing: bool,
    /// the window should close now; main does it.
    pub close_now: bool,
}

impl App {
    pub fn new(root: &Path, lang: Lang) -> Self {
        let mut theme = Theme::dark();
        lang.font_family().clone_into(&mut theme.font_family);
        let mut app = Self {
            ui: UiState::new(),
            theme,
            texts: Texts::new(lang),
            keymap: keymap(),
            bench: Workbench::default(),
            docs: Documents::default(),
            explorer: Explorer::new(root),
            jobs: Jobs::default(),
            syntax: Syntax::default(),
            shell: Shell::default(),
            finder: Finder::default(),
            lsp: Lsp::default(),
            hover: None,
            line_clip: None,
            shell_focus: false,
            canvas: None,
            session: workbench::session_path(root),
            status: String::new(),
            focus: None,
            ime: None,
            layout_dirty: false,
            restored: false,
            closing: false,
            close_now: false,
        };
        app.scan_pending();
        match app.session.clone() {
            Some(path) => {
                app.jobs
                    .run(move || Done::Session(std::fs::read_to_string(path).ok()));
            }
            None => app.restored = true,
        }
        app
    }

    pub fn set_canvas(&mut self, id: CanvasId) {
        self.jobs.set_canvas(id);
        self.syntax.set_canvas(id);
        self.canvas = Some(id);
    }

    pub fn busy(&self) -> bool {
        self.jobs.busy()
    }

    pub fn title(&self) -> String {
        let root = self.explorer.root();
        let name = root.file_name().map_or_else(
            || root.display().to_string(),
            |n| n.to_string_lossy().into_owned(),
        );
        format!("{name} - {}", self.texts.tr("Ligase"))
    }

    /// one window event. true when something needs a redraw.
    pub fn event(&mut self, event: &WindowEvent) -> bool {
        if matches!(event, WindowEvent::CloseRequested) {
            self.request_close();
            return true;
        }
        if self.ui.popup_open() {
            return self.ui.handle(event);
        }
        if self.shell_focus {
            return self.shell_event(event);
        }
        if let Some(&command) = self.keymap.resolve(event, self.ui.text_focused()) {
            self.run(command);
            return true;
        }
        // aurea keeps typed text to its own fields, so the focused editor
        // takes its keys here, before the ui sees them
        if let Some(view) = self.focus
            && !self.ui.text_focused()
            && let Some(v) = self.docs.view(view)
            && let Some(action) = input::action(event, v.page, !v.sel.is_empty())
        {
            editor::act(&mut self.docs, view, action);
            self.hover = None;
            return true;
        }
        self.ui.handle(event)
    }

    /// keys while the terminal has focus: panel shortcuts still work, paste
    /// pastes into the shell, everything else is the shell's.
    fn shell_event(&mut self, event: &WindowEvent) -> bool {
        match self.keymap.resolve(event, false) {
            Some(
                command @ (Command::ToggleTerminal
                | Command::ToggleExplorer
                | Command::FindInFiles
                | Command::ResetLayout),
            ) => {
                let command = *command;
                self.run(command);
                return true;
            }
            Some(Command::Paste) => {
                if let Some(text) = aurea::clipboard::clipboard_text() {
                    self.shell.paste(&text);
                }
                return true;
            }
            _ => {}
        }
        let Some(bytes) = self.shell.bytes(event) else {
            return self.ui.handle(event);
        };
        if self.shell.state == terminal::State::Ended {
            if bytes == "\r" {
                self.start_shell();
            }
        } else {
            self.shell.write(&bytes);
        }
        true
    }

    /// starts a language server on the worker, in the workspace folder.
    fn start_server(&mut self, language: &'static str) {
        let Some((command, args, _)) = lsp::server_for(language) else {
            return;
        };
        let root = self.explorer.root().to_path_buf();
        let canvas = self.canvas;
        let job = move || {
            let wake = move || {
                if let Some(id) = canvas {
                    request_canvas_redraw(id);
                }
            };
            Done::Server(
                language,
                lsp::server::Server::spawn(command, args, &root, wake),
            )
        };
        if !self.jobs.run(job) {
            self.lsp
                .started(language, Err("the file worker is not running".to_owned()));
        }
    }

    /// starts the shell on the worker, in the workspace folder. its output
    /// wakes the canvas.
    fn start_shell(&mut self) {
        self.shell.state = terminal::State::Starting;
        let root = self.explorer.root().to_path_buf();
        let canvas = self.canvas;
        let job = move || {
            let wake = move || {
                if let Some(id) = canvas {
                    request_canvas_redraw(id);
                }
            };
            Done::Shell(terminal::spawn(&root, wake))
        };
        if !self.jobs.run(job) {
            self.shell
                .started(Err("the file worker is not running".to_owned()));
        }
    }

    pub fn run(&mut self, command: Command) {
        let focus = self.focus;
        match command {
            Command::Save => {
                if let Some(view) = focus {
                    self.save(view);
                }
                return;
            }
            Command::Undo | Command::Redo => {
                if let Some(view) = focus {
                    if command == Command::Undo {
                        self.docs.undo(view);
                    } else {
                        self.docs.redo(view);
                    }
                }
                return;
            }
            Command::Copy | Command::Cut => {
                let Some(view) = focus else { return };
                // nothing selected takes the whole line
                let (text, line) = match editor::selected(&self.docs, view) {
                    Some(text) => (text, None),
                    None => match editor::caret_line(&self.docs, view) {
                        Some((range, text)) => (text, Some(range)),
                        None => return,
                    },
                };
                if let Err(e) = aurea::clipboard::set_clipboard_text(&text) {
                    self.status = e.to_string();
                    return;
                }
                self.line_clip = line.is_some().then(|| text.clone());
                if command == Command::Cut {
                    match line {
                        Some(range) => {
                            self.docs.edit(view, vec![(range, String::new())], false);
                        }
                        None => {
                            editor::act(&mut self.docs, view, input::Action::Delete);
                        }
                    }
                }
                return;
            }
            Command::Paste => {
                if let Some(view) = focus
                    && let Some(text) = aurea::clipboard::clipboard_text()
                {
                    let empty = self.docs.view(view).is_some_and(|v| v.sel.is_empty());
                    if empty && self.line_clip.as_deref() == Some(text.as_str()) {
                        editor::paste_line(&mut self.docs, view, &text);
                    } else {
                        editor::paste(&mut self.docs, view, &text);
                    }
                }
                return;
            }
            Command::Line(op) => {
                if let Some(view) = focus {
                    editor::line_op(&mut self.docs, view, op);
                }
                return;
            }
            Command::GotoDefinition => {
                if let Some(view) = focus
                    && !self.lsp.definition(&self.docs, view)
                {
                    self.status = self.texts.tr("no language server for this yet").to_owned();
                }
                return;
            }
            Command::ShowHover => {
                if let Some(view) = focus
                    && !self.lsp.hover(&self.docs, view)
                {
                    self.status = self.texts.tr("no language server for this yet").to_owned();
                }
                return;
            }
            Command::SelectAll => {
                if let Some(view) = focus {
                    editor::select_all(&mut self.docs, view);
                }
                return;
            }
            Command::FindInFiles => {
                self.bench.show(Panel::Search);
                self.ui.focus(Id::SearchField, 0);
            }
            Command::ToggleExplorer => self.bench.toggle(Panel::Explorer),
            Command::ToggleTerminal => self.bench.toggle(Panel::Terminal),
            Command::ResetLayout => {
                // editors stay open, only their places reset
                let views: Vec<ViewId> = self
                    .bench
                    .panels
                    .iter()
                    .filter_map(|(_, p)| match p {
                        Panel::Editor(v) => Some(v),
                        _ => None,
                    })
                    .collect();
                self.bench = Workbench::default();
                for view in views {
                    self.bench.show_editor(view);
                }
            }
        }
        self.layout_dirty = true;
    }

    /// is a panel in the dock now; for menu ticks.
    pub fn shows(&self, panel: Panel) -> bool {
        self.bench
            .panels
            .find(panel)
            .is_some_and(|id| self.bench.dock.tree.contains(id))
    }

    fn request_close(&mut self) {
        if self.closing {
            return;
        }
        if self.docs.any_dirty() {
            self.status = self.texts.tr("save it first, it has changes").to_owned();
            return;
        }
        // before the saved session is back, writing would wipe it
        if self.restored && self.save_layout() {
            self.closing = true;
        } else {
            self.close_now = true;
        }
    }

    /// queues a session write. false when there is nowhere to write.
    fn save_layout(&mut self) -> bool {
        let Some(path) = self.session.clone() else {
            return false;
        };
        let text = self.bench.save(&self.docs);
        self.layout_dirty = false;
        self.jobs
            .run(move || Done::Saved(workbench::write_session(&path, &text)))
    }

    fn save(&mut self, view: ViewId) {
        let Some((doc, path, text, point)) = self.docs.save_job(view) else {
            return;
        };
        let name = path
            .file_name()
            .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
        let job = move || Done::Wrote(doc, name, point, document::write_atomic(&path, &text));
        if !self.jobs.run(job) {
            self.status = format!(
                "{}: the file worker is not running",
                self.texts.tr("can't save")
            );
        }
    }

    fn scan_pending(&mut self) {
        for path in self.explorer.take_pending() {
            self.scan(path);
        }
    }

    fn scan(&mut self, path: PathBuf) {
        let at = path.clone();
        if !self
            .jobs
            .run(move || Done::Listed(at.clone(), explorer::scan(&at)))
        {
            self.explorer.listed(&path, Vec::new());
        }
    }

    fn open_file(&mut self, path: &Path) -> ViewId {
        let (doc, new) = self.docs.open(path);
        if new {
            self.load(doc, path);
        }
        // a tab on this document already: show it, don't make another
        let existing = self.bench.panels.iter().find_map(|(_, p)| match p {
            Panel::Editor(v) if self.docs.view(v).is_some_and(|v| v.doc == doc) => Some(v),
            _ => None,
        });
        let view = existing.unwrap_or_else(|| self.docs.add_view(doc));
        self.bench.show_editor(view);
        self.layout_dirty = true;
        view
    }

    fn load(&mut self, doc: DocumentId, path: &Path) {
        let path = path.to_path_buf();
        if !self
            .jobs
            .run(move || Done::Loaded(doc, document::read_text(&path)))
        {
            self.docs
                .loaded(doc, Err("the file worker is not running".to_owned()));
        }
    }

    fn poll(&mut self) {
        self.finder.poll();
        for event in self.lsp.poll() {
            match event {
                lsp::Event::Diagnostics(doc, revision, diags) => {
                    self.docs.set_diags(doc, revision, diags);
                }
                lsp::Event::Goto(path, at, encoding) => {
                    let view = self.open_file(&path);
                    if let Some(v) = self.docs.view_mut(view) {
                        v.goto = Some((at, encoding));
                    }
                }
                lsp::Event::Hover(view, at, text) => {
                    if self.docs.view(view).is_some_and(|v| v.sel.head == at) {
                        self.hover = Some((view, at, text));
                    }
                }
                lsp::Event::Status(text) => self.status = text,
            }
        }
        for language in self.lsp.wanted(&self.docs) {
            self.start_server(language);
        }
        self.lsp.sync(&self.docs);
        while let Some(parsed) = self.syntax.take() {
            self.docs.coloured(parsed);
        }
        for (doc, language, text) in self.docs.wanting_colours() {
            self.syntax.ask(doc, language, text);
        }
        while let Some(done) = self.jobs.take() {
            match done {
                Done::Listed(path, Ok(entries)) => self.explorer.listed(&path, entries),
                Done::Listed(path, Err(why)) => {
                    self.status = format!("{}: {why}", path.display());
                    self.explorer.listed(&path, Vec::new());
                }
                Done::Loaded(doc, result) => self.docs.loaded(doc, result),
                Done::Session(text) => self.restore(text),
                Done::Wrote(doc, name, point, result) => match result {
                    Ok(()) => {
                        self.docs.saved(doc, point);
                        self.status = format!("{} {name}", self.texts.tr("saved"));
                    }
                    Err(why) => {
                        self.status = format!("{} {name}: {why}", self.texts.tr("can't save"));
                    }
                },
                Done::Shell(result) => self.shell.started(result),
                Done::Server(language, result) => {
                    if let Some(why) = self.lsp.started(language, result) {
                        // a missing server is fine; the editor works without it
                        self.status = format!("{}: {why}", self.texts.tr("no language server"));
                    }
                }
                Done::Saved(result) => {
                    if let Err(why) = result {
                        self.status = format!("{}: {why}", self.texts.tr("can't save the layout"));
                    }
                    if self.closing {
                        self.close_now = true;
                    }
                }
            }
        }
    }

    fn restore(&mut self, text: Option<String>) {
        if std::mem::replace(&mut self.restored, true) || self.layout_dirty {
            // the user already moved things; theirs wins
            return;
        }
        let Some(text) = text else { return };
        let mut made: Vec<ViewId> = Vec::new();
        let mut fresh: Vec<(DocumentId, PathBuf)> = Vec::new();
        let docs = &mut self.docs;
        let bench = Workbench::restore(&text, |path| {
            let (doc, new) = docs.open(path);
            let view = docs.add_view(doc);
            made.push(view);
            if new {
                fresh.push((doc, path.to_path_buf()));
            }
            Some(view)
        });
        match bench {
            Some(bench) => {
                self.bench = bench;
                for (doc, path) in fresh {
                    self.load(doc, &path);
                }
            }
            None => {
                for view in made {
                    self.docs.close_view(view);
                }
            }
        }
    }

    fn titles(&self) -> HashMap<PaneId, String> {
        self.bench
            .panels
            .iter()
            .map(|(id, panel)| {
                let title = match panel {
                    Panel::Explorer => self.texts.tr("Explorer").to_owned(),
                    Panel::Search => self.texts.tr("Search").to_owned(),
                    Panel::Terminal => self.texts.tr("Terminal").to_owned(),
                    Panel::Welcome => self.texts.tr("Welcome").to_owned(),
                    Panel::Editor(view) => self.docs.doc_of(view).map_or_else(String::new, |d| {
                        if d.is_dirty() {
                            format!("{} *", d.name())
                        } else {
                            d.name()
                        }
                    }),
                };
                (id, title)
            })
            .collect()
    }

    /// one frame.
    ///
    /// # Errors
    ///
    /// what drawing reports.
    pub fn draw(&mut self, ctx: &mut dyn DrawingContext) -> AureaResult<()> {
        self.poll();
        ctx.clear(self.theme.palette.background)?;
        let area = Rect::new(0.0, 0.0, ctx.width() as f32, ctx.height() as f32);
        let titles = self.titles();
        let mut clicks: Vec<Click> = Vec::new();
        let mut focus = None;
        let mut ime = None;
        let mut pressed: Vec<ViewId> = Vec::new();
        let mut shell_focus = false;
        let mut found = None;
        let changed;
        let place = self.place();
        let message = if self.status.is_empty() {
            self.diag_here()
        } else {
            self.status.clone()
        };
        let root = self.explorer.root().to_path_buf();
        {
            let Workbench { dock, panels } = &mut self.bench;
            let (explorer, docs, texts) = (&mut self.explorer, &mut self.docs, &self.texts);
            let shell = &mut self.shell;
            let hover = &self.hover;
            let finder = &mut self.finder;
            let mut ui = self.ui.frame(ctx, &self.theme);
            let (status, main) = area.split_bottom(STATUS_HEIGHT, 0.0);
            let title = |pane: PaneId| titles.get(&pane).map_or("", String::as_str);
            let content = |ui: &mut Ui<'_, Id>, pane: PaneId, rect: Rect| match panels.get(pane) {
                Some(Panel::Explorer) => {
                    clicks.extend(explorer::view::show(ui, explorer, rect)?);
                    Ok(())
                }
                Some(Panel::Editor(view)) => {
                    let Some(doc_id) = docs.view(view).map(|v| v.doc) else {
                        return Ok(());
                    };
                    let mut state = docs.view(view).cloned().expect("view exists");
                    if let Some(doc) = docs.doc(doc_id) {
                        let hover = hover
                            .as_ref()
                            .filter(|(v, at, _)| *v == view && state.sel.head == *at)
                            .map(|(_, _, text)| text.as_str());
                        let shown = editor::show(ui, view, doc, &mut state, rect, texts, hover)?;
                        if shown.focused {
                            focus = Some(view);
                            ime = shown.caret;
                        }
                        if shown.pressed {
                            pressed.push(view);
                        }
                    }
                    if let Some(v) = docs.view_mut(view) {
                        *v = state;
                    }
                    Ok(())
                }
                Some(Panel::Search) => {
                    found = search::view::show(ui, finder, &root, rect, texts)?;
                    Ok(())
                }
                Some(Panel::Terminal) => {
                    shell_focus = terminal::show(ui, shell, rect, texts)?;
                    Ok(())
                }
                Some(Panel::Welcome) | None => {
                    hint(ui, rect, texts.tr("open a file from the explorer"))
                }
            };
            let response = DockView::new(dock, title, content)
                .style(DockStyle::new(&self.theme))
                .close_buttons(true)
                .show(&mut ui, Id::Dock, main)?;
            changed = response.changed;
            status_bar(&mut ui, status, &message, &place)?;
        }
        self.focus = focus;
        self.ime = ime;
        self.shell_focus = shell_focus;
        match found {
            Some(search::view::Ask::Search) => {
                let root = self.explorer.root().to_path_buf();
                self.finder.start(&root, self.canvas);
            }
            Some(search::view::Ask::Open(hit)) => {
                let view = self.open_file(&hit.path);
                if let Some(v) = self.docs.view_mut(view) {
                    v.goto = Some((Point::new(hit.line, hit.column), Encoding::Utf8));
                }
            }
            None => {}
        }
        // the shell starts the first time its panel is drawn, not before
        if self.shell.state == terminal::State::NotStarted && self.shows(Panel::Terminal) {
            self.start_shell();
        }
        for view in pressed {
            if let Some((doc, _)) = self.docs.editable(view) {
                doc.buffer.break_join();
            }
        }
        if changed {
            self.layout_dirty = true;
        }
        for event in self.bench.dock.take_tab_events() {
            if let DockTabEvent::CloseRequested(pane) = event {
                match self.bench.close(pane, &mut self.docs) {
                    Ok(()) => self.layout_dirty = true,
                    Err(Refused::Unsaved) => {
                        self.status = self.texts.tr("save it first, it has changes").to_owned();
                    }
                    Err(Refused::KeepsEditorArea) => {}
                }
            }
        }
        for click in clicks {
            match click {
                Click::Open(path) => {
                    self.open_file(&path);
                }
                Click::Scan(path) => self.scan(path),
                Click::Toggled => {}
            }
        }
        if self.layout_dirty && self.restored && self.bench.dock.dragging().is_none() {
            self.save_layout();
        }
        Ok(())
    }
}

impl App {
    /// "Ln 3, Col 7" for the focused editor.
    fn place(&self) -> String {
        let Some(view) = self.focus.and_then(|v| self.docs.view(v)) else {
            return String::new();
        };
        let Some(doc) = self.docs.doc(view.doc) else {
            return String::new();
        };
        let errors = doc
            .diags
            .iter()
            .filter(|d| d.severity == lsp::Severity::Error)
            .count();
        let warnings = doc
            .diags
            .iter()
            .filter(|d| d.severity == lsp::Severity::Warning)
            .count();
        let at = doc
            .buffer
            .text()
            .point(view.sel.head, Encoding::Utf32)
            .map_or_else(
                |_| String::new(),
                |p| format!("Ln {}, Col {}", p.line + 1, p.column + 1),
            );
        if errors + warnings == 0 {
            at
        } else {
            format!("E {errors}  W {warnings}   {at}")
        }
    }

    /// the message of the diagnostic under the caret, for the status bar.
    fn diag_here(&self) -> String {
        let Some(view) = self.focus.and_then(|v| self.docs.view(v)) else {
            return String::new();
        };
        let Some(doc) = self.docs.doc(view.doc) else {
            return String::new();
        };
        let at = view.sel.head;
        doc.diags
            .iter()
            .filter(|d| d.range.start <= at && at <= d.range.end)
            .min_by_key(|d| d.severity)
            .map(|d| d.message.lines().next().unwrap_or_default().to_owned())
            .unwrap_or_default()
    }
}

fn hint(ui: &mut Ui<'_, Id>, rect: Rect, text: &str) -> AureaResult<()> {
    let font = ui.theme.font(13.0);
    let color = ui.theme.palette.dim;
    ui.text(text, rect, &font, color, Align::Center)
}

fn status_bar(ui: &mut Ui<'_, Id>, rect: Rect, message: &str, place: &str) -> AureaResult<()> {
    let palette = ui.theme.palette;
    ui.fill(rect, palette.surface)?;
    ui.fill(Rect::new(rect.x, rect.y, rect.width, 1.0), palette.border)?;
    let font = ui.theme.font(12.0);
    let inner = rect.inset_xy(10.0, 0.0);
    let (right, left) = inner.split_right(220.0, 8.0);
    ui.text_clipped(message, left, &font, palette.dim, Align::Start)?;
    ui.text(place, right, &font, palette.dim, Align::End)?;
    Ok(())
}

fn keymap() -> Keymap<Command> {
    let mut map = Keymap::new();
    map.bind(Chord::command(KeyCode::S), Command::Save)
        .bind(Chord::command(KeyCode::Z), Command::Undo)
        .bind(Chord::command(KeyCode::Z).shift(), Command::Redo)
        .bind(Chord::command(KeyCode::Y), Command::Redo)
        .bind(Chord::command(KeyCode::X), Command::Cut)
        .bind(Chord::command(KeyCode::C), Command::Copy)
        .bind(Chord::command(KeyCode::V), Command::Paste)
        .bind(Chord::command(KeyCode::A), Command::SelectAll)
        .bind(
            Chord::key(KeyCode::Up).alt(),
            Command::Line(LineOp::Move(true)),
        )
        .bind(
            Chord::key(KeyCode::Down).alt(),
            Command::Line(LineOp::Move(false)),
        )
        .bind(
            Chord::key(KeyCode::Up).alt().shift(),
            Command::Line(LineOp::Copy(true)),
        )
        .bind(
            Chord::key(KeyCode::Down).alt().shift(),
            Command::Line(LineOp::Copy(false)),
        )
        .bind(
            Chord::command(KeyCode::K).shift(),
            Command::Line(LineOp::Delete),
        )
        .bind(
            Chord::command(KeyCode::Slash),
            Command::Line(LineOp::Comment),
        )
        .bind(Chord::command(KeyCode::L), Command::Line(LineOp::Select))
        .bind(Chord::command(KeyCode::F).shift(), Command::FindInFiles)
        .bind(Chord::key(KeyCode::F12), Command::GotoDefinition)
        .bind(Chord::command(KeyCode::I), Command::ShowHover)
        .bind(Chord::command(KeyCode::B), Command::ToggleExplorer)
        .bind(Chord::command(KeyCode::Grave), Command::ToggleTerminal);
    map
}
