//! app state, input routing and one frame. long work goes to the file
//! worker; results come back through `Done`.

use std::collections::HashMap;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::time::Instant;

use aurea::render::{
    CanvasId, DrawingContext, Rect, request_canvas_redraw, request_canvas_redraw_at,
};
use aurea::ui::{Align, DockStyle, DockTabEvent, DockView, Theme, Ui, UiState};
use aurea::{AureaResult, Chord, KeyCode, Keymap, WindowEvent};

use resolvase::{Encoding, Point, Revision, SavePoint};

use crate::completion::{self, Popup};
use crate::document::{self, DocumentId, Documents, ViewId};
use crate::editor::cursor::Selection;
use crate::editor::find::{self, Bar, Mode};
use crate::editor::{self, CaretOp, LineOp, input};
use crate::explorer::{self, Click, Explorer};
use crate::jobs::Jobs;
use crate::lang::{Lang, Texts};
use crate::lsp::{self, Lsp};
use crate::palette::{self, Kind, Palette};
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
    FindBar,
    FindField,
    ReplaceField,
    FindButton(u16),
    Palette,
    PaletteField,
    PaletteRow(u16),
    Completion,
    CompletionRow(u16),
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
    Caret(CaretOp),
    Find,
    Replace,
    FindNext,
    FindPrev,
    GotoLine,
    GotoDefinition,
    ShowHover,
    Complete,
    FindInFiles,
    ToggleExplorer,
    ToggleTerminal,
    ResetLayout,
    ZoomIn,
    ZoomOut,
    ZoomReset,
    NewFile,
    OpenFile,
    SaveAs,
    SaveAll,
    QuickOpen,
    Commands,
    CloseTab,
    NextTab,
    PrevTab,
}

/// every command by name, for the palette.
const COMMANDS: &[(&str, Command)] = &[
    ("Quick Open", Command::QuickOpen),
    ("New File", Command::NewFile),
    ("Open File", Command::OpenFile),
    ("Save", Command::Save),
    ("Save As", Command::SaveAs),
    ("Save All", Command::SaveAll),
    ("Close Tab", Command::CloseTab),
    ("Next Tab", Command::NextTab),
    ("Previous Tab", Command::PrevTab),
    ("Undo", Command::Undo),
    ("Redo", Command::Redo),
    ("Cut", Command::Cut),
    ("Copy", Command::Copy),
    ("Paste", Command::Paste),
    ("Select All", Command::SelectAll),
    ("Select Line", Command::Line(LineOp::Select)),
    ("Toggle Comment", Command::Line(LineOp::Comment)),
    ("Move Line Up", Command::Line(LineOp::Move(true))),
    ("Move Line Down", Command::Line(LineOp::Move(false))),
    ("Copy Line Up", Command::Line(LineOp::Copy(true))),
    ("Copy Line Down", Command::Line(LineOp::Copy(false))),
    ("Delete Line", Command::Line(LineOp::Delete)),
    ("Add Next Match", Command::Caret(CaretOp::AddNext)),
    ("Select All Matches", Command::Caret(CaretOp::SelectMatches)),
    ("Add Caret Above", Command::Caret(CaretOp::AddAbove)),
    ("Add Caret Below", Command::Caret(CaretOp::AddBelow)),
    ("Find", Command::Find),
    ("Replace", Command::Replace),
    ("Find Next", Command::FindNext),
    ("Find Previous", Command::FindPrev),
    ("Find in Files", Command::FindInFiles),
    ("Go to Line", Command::GotoLine),
    ("Go to Definition", Command::GotoDefinition),
    ("Show Hover", Command::ShowHover),
    ("Complete", Command::Complete),
    ("Explorer", Command::ToggleExplorer),
    ("Terminal", Command::ToggleTerminal),
    ("Zoom In", Command::ZoomIn),
    ("Zoom Out", Command::ZoomOut),
    ("Reset Zoom", Command::ZoomReset),
    ("Reset Layout", Command::ResetLayout),
];

/// a native dialog to show once the frame is done; main shows it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dialog {
    Open,
    SaveAs(ViewId),
}

/// what the worker hands back.
enum Done {
    Listed(PathBuf, Result<Vec<(String, bool)>, String>),
    Loaded(DocumentId, Result<String, String>, Option<document::Stamp>),
    /// stamps of open files now, for the ones that changed.
    Disk(Vec<(DocumentId, Option<document::Stamp>)>),
    Reloaded(
        DocumentId,
        Revision,
        Result<String, String>,
        Option<document::Stamp>,
    ),
    /// the file changed on disk since it was read; the save did not write.
    Conflict(DocumentId, String, Option<document::Stamp>),
    Session(Option<String>),
    Saved(Result<(), String>),
    Wrote(
        DocumentId,
        String,
        SavePoint,
        Result<Option<document::Stamp>, String>,
    ),
    Shell(Result<vanta::Terminal, String>),
    Server(&'static str, Result<lsp::server::Server, String>),
    Found(DocumentId, Revision, String, Vec<Range<usize>>),
    Files(Vec<palette::Item>),
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
    /// editor font size.
    zoom: f32,
    /// find, replace and go to line for one editor.
    bar: Bar,
    /// quick open and commands.
    palette: Palette,
    /// language server completions under the caret.
    completion: Option<Popup>,
    /// ask for completions once the server has the text as it is now.
    want_complete: Option<ViewId>,
    /// the view completions were last asked for; replies for anything else
    /// (or after escape) are dropped.
    complete_for: Option<ViewId>,
    /// the editor that had the keyboard last, even while a field has it.
    last_editor: Option<ViewId>,
    /// last key or click in an editor; the caret blinks from there.
    typed_at: Instant,
    /// alt held, from the last event that said; alt and a click add a caret.
    alt_down: bool,
    /// ctrl (cmd on mac) held, the same way; ctrl and a click go to definition.
    ctrl_down: bool,
    /// a dialog main should show after this frame.
    dialog: Option<Dialog>,
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
            zoom: editor::SIZE,
            bar: Bar::default(),
            palette: Palette::default(),
            completion: None,
            want_complete: None,
            complete_for: None,
            last_editor: None,
            typed_at: Instant::now(),
            alt_down: false,
            ctrl_down: false,
            dialog: None,
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
        if matches!(event, WindowEvent::Focused) {
            // back from another program: files may have changed under us
            self.check_disk();
        }
        match event {
            WindowEvent::KeyInput { modifiers, .. }
            | WindowEvent::MouseButton { modifiers, .. }
            | WindowEvent::MouseMove { modifiers, .. } => {
                self.alt_down = modifiers.alt;
                self.ctrl_down = if cfg!(target_os = "macos") {
                    modifiers.meta
                } else {
                    modifiers.ctrl
                };
            }
            WindowEvent::Unfocused => {
                self.alt_down = false;
                self.ctrl_down = false;
            }
            _ => {}
        }
        if self.ui.popup_open() {
            return self.ui.handle(event);
        }
        if self.palette.is_open()
            && let WindowEvent::KeyInput {
                key, pressed: true, ..
            } = event
        {
            let rows = palette::view::ROWS;
            let step = match key {
                KeyCode::Up => Some(-1),
                KeyCode::Down => Some(1),
                KeyCode::PageUp => Some(-(rows as isize)),
                KeyCode::PageDown => Some(rows as isize),
                _ => None,
            };
            if let Some(by) = step {
                self.palette.step(by, rows);
                return true;
            }
        }
        if let WindowEvent::MouseWheel {
            delta_x,
            delta_y,
            modifiers,
        } = event
        {
            if modifiers.ctrl && !self.shell_focus && *delta_y != 0.0 {
                // ctrl and the wheel zoom the editor
                self.zoom_by(if *delta_y > 0.0 { 1.0 } else { -1.0 });
                return true;
            }
            if modifiers.shift && *delta_x == 0.0 {
                // shift and the wheel go sideways
                let sideways = WindowEvent::MouseWheel {
                    delta_x: *delta_y,
                    delta_y: 0.0,
                    modifiers: *modifiers,
                };
                return self.ui.handle(&sideways);
            }
        }
        if self.shell_focus {
            return self.shell_event(event);
        }
        if self.completion.is_some()
            && !self.ui.text_focused()
            && let WindowEvent::KeyInput {
                key,
                pressed: true,
                modifiers,
            } = event
            && !modifiers.ctrl
            && !modifiers.alt
            && !modifiers.shift
            && self.completion_key(*key)
        {
            return true;
        }
        if let Some(&command) = self.keymap.resolve(event, self.ui.text_focused()) {
            self.run(command);
            return true;
        }
        if self.bar.is_open()
            && !self.ui.text_focused()
            && matches!(
                event,
                WindowEvent::KeyInput {
                    key: KeyCode::Escape,
                    pressed: true,
                    ..
                }
            )
            && self
                .focus
                .and_then(|v| self.docs.view(v))
                .is_some_and(|v| v.sel.is_empty())
        {
            self.bar_ask(find::Ask::Close);
            return true;
        }
        // aurea keeps typed text to its own fields, so the focused editor
        // takes its keys here, before the ui sees them
        if let Some(view) = self.focus
            && !self.ui.text_focused()
            && let Some(v) = self.docs.view(view)
            && let Some(action) =
                input::action(event, v.page, !v.sel.is_empty() || !v.extra.is_empty())
        {
            let typed = match &action {
                input::Action::Insert(t) => Some(t.clone()),
                _ => None,
            };
            let moved = matches!(action, input::Action::Move(..) | input::Action::Collapse);
            editor::act(&mut self.docs, view, action);
            self.hover = None;
            if moved {
                self.completion = None;
                self.complete_for = None;
            }
            if let Some(typed) = typed {
                self.typed_for_completion(view, &typed);
            }
            self.typed_at = Instant::now();
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
        self.typed_at = Instant::now();
        match command {
            Command::Save => {
                if let Some(view) = focus.or(self.last_editor) {
                    self.save(view);
                }
                return;
            }
            Command::SaveAs => {
                if let Some(view) = focus.or(self.last_editor) {
                    self.dialog = Some(Dialog::SaveAs(view));
                }
                return;
            }
            Command::SaveAll => {
                let views = self.docs.dirty_views();
                for view in views {
                    if self.docs.doc_of(view).is_some_and(|d| !d.untitled) {
                        self.save(view);
                    }
                }
                return;
            }
            Command::NewFile => {
                let doc = self.docs.new_untitled();
                let view = self.docs.add_view(doc);
                self.bench.show_editor(view);
                self.ui.focus(Id::Editor(view), 1);
                self.last_editor = Some(view);
            }
            Command::OpenFile => {
                self.dialog = Some(Dialog::Open);
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
                            if let Some(v) = self.docs.view_mut(view) {
                                v.extra.clear();
                            }
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
            Command::Find | Command::Replace | Command::GotoLine => {
                let mode = match command {
                    Command::Find => Mode::Find,
                    Command::Replace => Mode::Replace,
                    _ => Mode::Goto,
                };
                if let Some(view) = focus.or(self.last_editor) {
                    self.open_bar(view, mode);
                }
                return;
            }
            Command::FindNext | Command::FindPrev => {
                if self.bar.is_open() {
                    let ask = if command == Command::FindNext {
                        find::Ask::Next
                    } else {
                        find::Ask::Prev
                    };
                    self.bar_ask(ask);
                } else if let Some(view) = focus.or(self.last_editor) {
                    self.open_bar(view, Mode::Find);
                }
                return;
            }
            Command::QuickOpen | Command::Commands => {
                if command == Command::QuickOpen {
                    self.palette.open(Kind::Files);
                    self.list_files();
                } else {
                    self.palette.commands = COMMANDS
                        .iter()
                        .map(|(label, c)| palette::Item {
                            text: self.texts.tr(label).to_owned(),
                            note: self
                                .keymap
                                .chord_for(c)
                                .map(|chord| chord.label())
                                .unwrap_or_default(),
                        })
                        .collect();
                    self.palette.open(Kind::Commands);
                }
                self.ui.focus(Id::PaletteField, 0);
                return;
            }
            Command::CloseTab => {
                if let Some(view) = focus.or(self.last_editor)
                    && let Some(pane) = self.bench.panels.find(Panel::Editor(view))
                {
                    self.close_pane(pane);
                }
                return;
            }
            Command::NextTab | Command::PrevTab => {
                let by = if command == Command::NextTab { 1 } else { -1 };
                if let Some(view) = focus.or(self.last_editor)
                    && let Some(pane) = self.bench.panels.find(Panel::Editor(view))
                    && let Some(next) = self.bench.cycle(pane, by)
                    && let Some(Panel::Editor(v)) = self.bench.panels.get(next)
                {
                    self.ui.focus(Id::Editor(v), 1);
                    self.last_editor = Some(v);
                }
            }
            Command::Caret(op) => {
                if let Some(view) = focus {
                    editor::caret_op(&mut self.docs, view, op);
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
            Command::Complete => {
                if let Some(view) = focus {
                    self.want_complete = Some(view);
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
            Command::ZoomIn | Command::ZoomOut | Command::ZoomReset => {
                match command {
                    Command::ZoomIn => self.zoom_by(1.0),
                    Command::ZoomOut => self.zoom_by(-1.0),
                    _ => self.zoom = editor::SIZE,
                }
                return;
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

    /// keys while completions are up: they move the pick, take it or close.
    /// true when the key was used.
    fn completion_key(&mut self, key: KeyCode) -> bool {
        let rows = completion::view::ROWS;
        let Some(popup) = &mut self.completion else {
            return false;
        };
        match key {
            KeyCode::Up => popup.step(-1, rows),
            KeyCode::Down => popup.step(1, rows),
            KeyCode::PageUp => popup.step(-(rows as isize), rows),
            KeyCode::PageDown => popup.step(rows as isize, rows),
            KeyCode::Enter | KeyCode::Tab => {
                let pick = popup.pick;
                self.accept_completion(pick);
            }
            KeyCode::Escape => {
                self.completion = None;
                // a reply still on its way must not bring it back
                self.complete_for = None;
            }
            _ => return false,
        }
        true
    }

    /// after typing: a word char or a trigger char asks for completions.
    fn typed_for_completion(&mut self, view: ViewId, typed: &str) {
        let word = typed.chars().all(completion::is_word);
        if word && self.completion.as_ref().is_some_and(|p| p.view == view) {
            // already up: typing a word just narrows it
            return;
        }
        // anything else ends this list; a trigger char starts the next
        self.completion = None;
        let language = self.docs.doc_of(view).and_then(|d| d.language);
        if word || self.lsp.triggers(language, typed) {
            self.want_complete = Some(view);
        }
    }

    /// puts the picked completion in, over the word typed so far.
    fn accept_completion(&mut self, index: usize) {
        let Some(popup) = self.completion.take() else {
            return;
        };
        let Some(item) = popup.shown.get(index).map(|s| popup.items[s.item].clone()) else {
            return;
        };
        let Some(head) = self.docs.view(popup.view).map(|v| v.sel.head) else {
            return;
        };
        let from = item.start.unwrap_or(popup.start).min(popup.start).min(head);
        let change = editor::lines::Change {
            sel: editor::cursor::Selection::caret(from + item.caret),
            edits: vec![(from..head, item.insert)],
        };
        if let Some((doc, _)) = self.docs.editable(popup.view) {
            doc.buffer.break_join();
        }
        editor::apply(&mut self.docs, popup.view, change, false);
        self.typed_at = Instant::now();
    }

    /// new completions from the server; shown when the caret is still on
    /// the word they were asked for.
    fn completed(
        &mut self,
        view: ViewId,
        revision: Revision,
        mut items: Vec<crate::lsp::complete::Item>,
    ) {
        if items.is_empty()
            || self.complete_for != Some(view)
            || self.focus.or(self.last_editor) != Some(view)
        {
            return;
        }
        let Some(v) = self.docs.view(view) else {
            return;
        };
        let doc_id = v.doc;
        // one word is completed; with more carets there is no one place
        if !v.sel.is_empty() || !v.extra.is_empty() {
            return;
        }
        // starts are offsets in the text the request was for; move them
        // over whatever was typed since, or drop the reply when too old
        for item in &mut items {
            if let Some(start) = item.start {
                let Some(now) = self.docs.map_offset(doc_id, revision, start) else {
                    return;
                };
                item.start = Some(now);
            }
        }
        let Some(v) = self.docs.view(view) else {
            return;
        };
        let Some(doc) = self.docs.doc(v.doc) else {
            return;
        };
        let start = completion::word_start(doc.buffer.text(), v.sel.head);
        let mut popup = Popup::new(view, items, start);
        // keep the light on the same row when a new list replaces an old one
        if let Some(old) = &self.completion
            && old.view == view
        {
            popup.opened = false;
        }
        self.completion = Some(popup);
    }

    /// narrows the popup to the word typed so far, or closes it once the
    /// caret has left that word.
    fn refilter_completion(&mut self) {
        let Some(popup) = &mut self.completion else {
            return;
        };
        let typed = (|| {
            let v = self.docs.view(popup.view)?;
            let doc = self.docs.doc(v.doc)?;
            if !v.sel.is_empty() || v.sel.head < popup.start {
                return None;
            }
            let typed = doc.buffer.text().slice(popup.start..v.sel.head).ok()?;
            typed.chars().all(completion::is_word).then_some(typed)
        })();
        match typed {
            Some(typed) => {
                popup.filter(&typed);
                if popup.shown.is_empty() {
                    self.completion = None;
                }
            }
            None => self.completion = None,
        }
    }

    /// lists the workspace files on the worker for quick open. the old
    /// list stays up meanwhile.
    fn list_files(&mut self) {
        if self.palette.listing {
            return;
        }
        let root = self.explorer.root().to_path_buf();
        self.palette.listing = self
            .jobs
            .run(move || Done::Files(palette::list_files(&root)));
    }

    /// a tab asked to close; unsaved work keeps it open.
    fn close_pane(&mut self, pane: PaneId) {
        match self.bench.close(pane, &mut self.docs) {
            Ok(()) => {
                self.layout_dirty = true;
                if self
                    .last_editor
                    .is_some_and(|v| self.docs.view(v).is_none())
                {
                    self.last_editor = None;
                }
            }
            Err(Refused::Unsaved) => {
                self.status = self.texts.tr("save it first, it has changes").to_owned();
            }
            Err(Refused::KeepsEditorArea) => {}
        }
    }

    /// what the palette asked for.
    fn palette_ask(&mut self, ask: palette::view::Ask) {
        let editor = self.last_editor;
        let (kind, query) = self.palette.wanted();
        let goto = (kind == Kind::Files)
            .then(|| query.strip_prefix(':').and_then(find::parse_goto))
            .flatten();
        let picked = self.palette.picked();
        self.palette.close();
        // the keyboard goes back first; whatever runs next may move it on
        if let Some(view) = editor {
            self.ui.focus(Id::Editor(view), 1);
        }
        if ask == palette::view::Ask::Close {
            return;
        }
        if let Some((line, column)) = goto {
            if let Some(view) = editor
                && let Some(v) = self.docs.view_mut(view)
            {
                v.goto = Some((Point::new(line, column), Encoding::Utf32));
            }
            return;
        }
        let Some(item) = picked else { return };
        match kind {
            Kind::Files => {
                let Some(rel) = self.palette.files.get(item).map(|i| i.text.clone()) else {
                    return;
                };
                let path = self.explorer.root().join(rel);
                let view = self.open_file(&path);
                self.ui.focus(Id::Editor(view), 1);
                self.last_editor = Some(view);
            }
            Kind::Commands => {
                if let Some(&(_, command)) = COMMANDS.get(item) {
                    self.run(command);
                }
            }
        }
    }

    /// opens the bar on `view`. find starts from the selection, when it is
    /// a bit of one line.
    fn open_bar(&mut self, view: ViewId, mode: Mode) {
        let was_open = self.bar.is_open();
        self.bar.mode = Some(mode);
        self.bar.view = Some(view);
        self.bar.opened = !was_open;
        if mode == Mode::Goto {
            self.bar.goto.clear();
        } else {
            if let Some(picked) = editor::selected(&self.docs, view)
                && !picked.contains('\n')
                && picked.len() <= 200
            {
                self.bar.query = picked;
            }
            self.bar.jump_from = self.docs.view(view).map(|v| v.sel.range().start);
        }
        self.ui.focus(Id::FindField, 0);
    }

    /// what the bar asked for, on its editor.
    fn bar_ask(&mut self, ask: find::Ask) {
        let Some(view) = self.bar.view else { return };
        let Some((doc, sel)) = self.docs.view(view).map(|v| (v.doc, v.sel)) else {
            self.bar.mode = None;
            return;
        };
        let revision = self.docs.doc(doc).map(|d| d.buffer.revision());
        let hits: Vec<Range<usize>> = revision
            .map(|r| self.bar.fresh(doc, r).to_vec())
            .unwrap_or_default();
        let stale = !self.bar.query.is_empty()
            && revision.is_some_and(|r| {
                self.bar.found_for.as_ref() != Some(&(doc, r, self.bar.query.clone()))
            });
        if stale && matches!(ask, find::Ask::Next | find::Ask::Prev | find::Ask::Replace) {
            // matches are on the way; this runs once they are in
            self.bar.pending = Some(ask);
            return;
        }
        match ask {
            find::Ask::Close => {
                self.bar.mode = None;
                self.ui.focus(Id::Editor(view), 1);
            }
            find::Ask::ToggleReplace => {
                self.bar.mode = Some(match self.bar.mode {
                    Some(Mode::Replace) => Mode::Find,
                    _ => Mode::Replace,
                });
            }
            find::Ask::Changed => {
                self.bar.jump_from = Some(sel.range().start);
            }
            find::Ask::Next => {
                let i = self
                    .bar
                    .after(&hits, sel.range().end.max(sel.range().start + 1));
                let i = i.or_else(|| self.bar.after(&hits, 0));
                self.pick_hit(view, &hits, i);
            }
            find::Ask::Prev => {
                let i = self.bar.before(&hits, sel.range().start);
                self.pick_hit(view, &hits, i);
            }
            find::Ask::Replace => {
                let range = sel.range();
                if hits.contains(&range) {
                    let with = self.bar.with.clone();
                    let next = range.start + with.len();
                    self.docs.edit(view, vec![(range, with)], false);
                    self.bar.jump_from = Some(next);
                } else {
                    self.bar_ask(find::Ask::Next);
                }
            }
            find::Ask::ReplaceAll => {
                if hits.is_empty() {
                    return;
                }
                if hits.len() >= find::MAX_HITS {
                    self.status = self
                        .texts
                        .tr("too many matches to replace at once")
                        .to_owned();
                    return;
                }
                let count = hits.len();
                let with = self.bar.with.clone();
                let edits = hits.into_iter().map(|h| (h, with.clone())).collect();
                if self.docs.edit(view, edits, false) {
                    self.status = format!("{} {count}", self.texts.tr("replaced"));
                }
            }
            find::Ask::Goto => {
                if let Some((line, column)) = find::parse_goto(&self.bar.goto)
                    && let Some(v) = self.docs.view_mut(view)
                {
                    v.goto = Some((Point::new(line, column), Encoding::Utf32));
                    self.bar.mode = None;
                    self.ui.focus(Id::Editor(view), 1);
                }
            }
        }
    }

    /// selects match `i` in `view` and brings it into sight.
    fn pick_hit(&mut self, view: ViewId, hits: &[Range<usize>], i: Option<usize>) {
        let Some((i, hit)) = i.and_then(|i| hits.get(i).map(|h| (i, h.clone()))) else {
            return;
        };
        if let Some(v) = self.docs.view_mut(view) {
            v.sel = Selection {
                anchor: hit.start,
                head: hit.end,
            };
            v.extra.clear();
            v.reveal = true;
            v.goal = None;
        }
        self.bar.current = Some(i);
    }

    /// asks the worker for matches when the query or the text changed.
    fn find_again(&mut self) {
        if !matches!(self.bar.mode, Some(Mode::Find | Mode::Replace)) {
            return;
        }
        let Some(view) = self.bar.view else { return };
        let Some(doc_id) = self.docs.view(view).map(|v| v.doc) else {
            self.bar.mode = None;
            return;
        };
        let Some(doc) = self.docs.doc(doc_id) else {
            return;
        };
        let key = (doc_id, doc.buffer.revision(), self.bar.query.clone());
        if self.bar.asked.as_ref() == Some(&key) {
            return;
        }
        self.bar.asked = Some(key.clone());
        let text = doc.buffer.snapshot();
        let (d, r, q) = key;
        let job = move || {
            let hits = find::find_all(&text, &q);
            Done::Found(d, r, q, hits)
        };
        self.jobs.run(job);
    }

    fn found(&mut self, key: (DocumentId, Revision, String), hits: Vec<Range<usize>>) {
        if self.bar.asked.as_ref() != Some(&key) {
            return;
        }
        self.bar.hits = hits;
        self.bar.found_for = Some(key);
        let Some(view) = self.bar.view else { return };
        let hits = self.bar.hits.clone();
        if let Some(from) = self.bar.jump_from.take() {
            let i = self.bar.after(&hits, from);
            self.pick_hit(view, &hits, i);
        } else {
            let sel = self.docs.view(view).map(|v| v.sel.range());
            self.bar.current = sel.and_then(|s| hits.iter().position(|h| *h == s));
        }
        if let Some(ask) = self.bar.pending.take() {
            self.bar_ask(ask);
        }
    }

    /// "3 of 12" for the bar.
    fn found_note(&self) -> String {
        let Some((d, r, q)) = &self.bar.found_for else {
            return String::new();
        };
        let now = self.docs.doc(*d).map(|doc| doc.buffer.revision());
        if self.bar.query.is_empty() || *q != self.bar.query || now != Some(*r) {
            return String::new();
        }
        let total = self.bar.hits.len();
        if total == 0 {
            return self.texts.tr("no results").to_owned();
        }
        let more = if total >= find::MAX_HITS { "+" } else { "" };
        match self.bar.current {
            Some(i) => format!("{} / {total}{more}", i + 1),
            None => format!("{total}{more}"),
        }
    }

    fn zoom_by(&mut self, by: f32) {
        self.zoom = (self.zoom + by).clamp(8.0, 40.0);
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

    /// the dialog to show after this frame, once.
    pub fn take_dialog(&mut self) -> Option<Dialog> {
        self.dialog.take()
    }

    /// default name and folder for a save as dialog.
    pub fn save_as_start(&self, view: ViewId) -> (String, PathBuf) {
        let root = self.explorer.root().to_path_buf();
        self.docs.doc_of(view).map_or_else(
            || (String::new(), root.clone()),
            |d| {
                let folder = if d.untitled {
                    root.clone()
                } else {
                    d.path
                        .parent()
                        .map_or_else(|| root.clone(), Path::to_path_buf)
                };
                (d.name(), folder)
            },
        )
    }

    pub fn root(&self) -> &Path {
        self.explorer.root()
    }

    /// what the user picked in a dialog; empty when cancelled.
    pub fn dialog_done(&mut self, dialog: Dialog, picked: Vec<PathBuf>) {
        match dialog {
            Dialog::Open => {
                for path in picked {
                    let view = self.open_file(&path);
                    self.ui.focus(Id::Editor(view), 1);
                    self.last_editor = Some(view);
                }
            }
            Dialog::SaveAs(view) => {
                let Some(path) = picked.into_iter().next() else {
                    return;
                };
                let Some(doc) = self.docs.view(view).map(|v| v.doc) else {
                    return;
                };
                let taken = self
                    .docs
                    .all_with_ids()
                    .any(|(id, d)| id != doc && d.path == path);
                if taken {
                    self.status = self.texts.tr("that file is open in another tab").to_owned();
                    return;
                }
                self.docs.set_path(doc, &path);
                self.layout_dirty = true;
                self.save_with(view, true);
            }
        }
    }

    fn save(&mut self, view: ViewId) {
        self.save_with(view, false);
    }

    /// saves `view`'s document. the worker checks the file is still what
    /// was read or what the user was warned about; `force` skips that, for
    /// a path just picked in save as (the dialog asked already).
    fn save_with(&mut self, view: ViewId, force: bool) {
        if self.docs.doc_of(view).is_some_and(|d| d.untitled) {
            // no file yet: ask where first
            self.dialog = Some(Dialog::SaveAs(view));
            return;
        }
        let Some((doc, path, text, point)) = self.docs.save_job(view) else {
            return;
        };
        let name = path
            .file_name()
            .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
        let Some(d) = self.docs.doc(doc) else { return };
        let expected = d.overwrite.unwrap_or(d.disk);
        let job = move || {
            let now = document::stamp(&path);
            if !force && now != expected {
                return Done::Conflict(doc, name, now);
            }
            let wrote = document::write_atomic(&path, &text).map(|()| document::stamp(&path));
            Done::Wrote(doc, name, point, wrote)
        };
        if !self.jobs.run(job) {
            self.status = format!(
                "{}: the file worker is not running",
                self.texts.tr("can't save")
            );
        }
    }

    /// stats every open file on the worker to see what changed outside.
    fn check_disk(&mut self) {
        let files = self.docs.on_disk();
        if files.is_empty() {
            return;
        }
        self.jobs.run(move || {
            let changed = files
                .into_iter()
                .filter_map(|(id, path, was)| {
                    let now = document::stamp(&path);
                    (was.is_some() && now != was).then_some((id, now))
                })
                .collect();
            Done::Disk(changed)
        });
    }

    fn disk_checked(&mut self, changed: Vec<(DocumentId, Option<document::Stamp>)>) {
        for (id, now) in changed {
            let Some(path) = self.docs.doc(id).map(|d| d.path.clone()) else {
                continue;
            };
            let name = self
                .docs
                .doc(id)
                .map(document::Document::name)
                .unwrap_or_default();
            if now.is_none() {
                // gone: keep the text, a save puts the file back
                self.docs.mark_conflict(id);
                self.status = format!("{name}: {}", self.texts.tr("deleted on disk"));
                continue;
            }
            if self.docs.changed_on_disk(id) {
                let Some(revision) = self.docs.doc(id).map(|d| d.buffer.revision()) else {
                    continue;
                };
                self.jobs.run(move || {
                    let disk = document::stamp(&path);
                    Done::Reloaded(id, revision, document::read_text(&path), disk)
                });
            } else {
                self.status = format!(
                    "{name}: {}",
                    self.texts
                        .tr("changed on disk, save again to write over it")
                );
            }
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
        if !self.jobs.run(move || {
            let disk = document::stamp(&path);
            Done::Loaded(doc, document::read_text(&path), disk)
        }) {
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
                lsp::Event::Completion(view, revision, items) => {
                    self.completed(view, revision, items);
                }
                lsp::Event::Hover(view, at, text) => {
                    if self.docs.view(view).is_some_and(|v| v.sel.head == at) {
                        self.hover = Some((view, at, text));
                    }
                }
                lsp::Event::Status(text) => self.status = text,
            }
        }
        self.find_again();
        for language in self.lsp.wanted(&self.docs) {
            self.start_server(language);
        }
        self.lsp.sync(&self.docs);
        if let Some(view) = self.lsp.take_again() {
            self.want_complete.get_or_insert(view);
        }
        if let Some(view) = self.want_complete.take()
            && self.lsp.complete(&self.docs, view)
        {
            self.complete_for = Some(view);
        }
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
                Done::Loaded(doc, result, disk) => {
                    self.docs.loaded(doc, result);
                    self.docs.set_disk(doc, disk);
                }
                Done::Disk(stamps) => self.disk_checked(stamps),
                Done::Reloaded(doc, revision, Ok(text), disk) => {
                    let now = self.docs.doc(doc).map(|d| d.buffer.revision());
                    if now != Some(revision) {
                        // the text moved on while the file was read; a
                        // later focus looks again
                        continue;
                    }
                    let name = self.docs.doc(doc).map(document::Document::name);
                    self.docs.reload(doc, &text, disk);
                    if let Some(name) = name {
                        self.status = format!("{} {name}", self.texts.tr("reloaded"));
                    }
                }
                Done::Reloaded(_, _, Err(why), _) => self.status = why,
                Done::Conflict(doc, name, now) => {
                    // this is the warning; the next save writes over what
                    // is there now, if nothing else changes it first
                    self.docs.mark_conflict(doc);
                    self.docs.arm_overwrite(doc, now);
                    self.status = format!(
                        "{name}: {}",
                        self.texts
                            .tr("changed on disk, save again to write over it")
                    );
                }
                Done::Session(text) => self.restore(text),
                Done::Wrote(doc, name, point, result) => match result {
                    Ok(disk) => {
                        self.docs.saved(doc, point);
                        self.docs.set_disk(doc, disk);
                        self.status = format!("{} {name}", self.texts.tr("saved"));
                    }
                    Err(why) => {
                        self.status = format!("{} {name}: {why}", self.texts.tr("can't save"));
                    }
                },
                Done::Shell(result) => self.shell.started(result),
                Done::Found(doc, revision, query, hits) => self.found((doc, revision, query), hits),
                Done::Files(files) => self.palette.set_files(files),
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
            if path.as_os_str().is_empty() {
                return None;
            }
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
                        // ! when the file also changed on disk
                        match (d.is_dirty(), d.conflict) {
                            (_, true) => format!("{} !", d.name()),
                            (true, false) => format!("{} *", d.name()),
                            (false, false) => d.name(),
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
        self.refilter_completion();
        let mut picked_completion = None;
        let mut found = None;
        let mut blink = None;
        let idle = self.typed_at.elapsed();
        let zoom = self.zoom;
        let alt = self.alt_down;
        let ctrl = self.ctrl_down;
        let mut definition = None;
        let mut rects: Vec<(ViewId, Rect)> = Vec::new();
        let mut bar_ask = None;
        let palette_ask;
        let note = self.found_note();
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
            let bar = &mut self.bar;
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
                        let found = if bar.is_open() && bar.view == Some(view) {
                            bar.fresh(doc_id, doc.buffer.revision())
                        } else {
                            &[]
                        };
                        let current = bar.current.and_then(|i| found.get(i)).cloned();
                        let look = editor::Look {
                            size: zoom,
                            idle,
                            hover,
                            alt,
                            ctrl,
                            found,
                            current,
                        };
                        rects.push((view, rect));
                        let shown = editor::show(ui, view, doc, &mut state, rect, texts, &look)?;
                        if shown.focused {
                            focus = Some(view);
                            ime = shown.caret;
                            blink = shown.blink;
                        }
                        if shown.pressed {
                            pressed.push(view);
                        }
                        if shown.definition {
                            definition = Some(view);
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
            if let Some(view) = bar.view
                && let Some((_, rect)) = rects.iter().find(|(v, _)| *v == view)
            {
                bar_ask = find::show(&mut ui, bar, *rect, &note, texts)?;
            } else if bar.is_open() {
                // its editor is not in sight; the bar goes with it
                bar_ask = Some(find::Ask::Close);
            }
            if let Some(popup) = &mut self.completion
                && focus == Some(popup.view)
                && let Some(caret) = ime
            {
                picked_completion = completion::view::show(&mut ui, popup, caret, main)?;
            }
            palette_ask = palette::view::show(&mut ui, &mut self.palette, main, texts)?;
            status_bar(&mut ui, status, &message, &place)?;
        }
        self.focus = focus;
        if focus.is_some() {
            self.last_editor = focus;
        }
        self.ime = ime;
        if let Some(ask) = bar_ask {
            self.bar_ask(ask);
        }
        if let Some(ask) = palette_ask {
            self.palette_ask(ask);
        }
        if let Some(index) = picked_completion {
            self.accept_completion(index);
        } else if !pressed.is_empty() || focus.is_none() {
            // a click elsewhere, or the editor lost the keyboard
            self.completion = None;
        }
        if let Some(view) = definition
            && !self.lsp.definition(&self.docs, view)
        {
            self.status = self.texts.tr("no language server for this yet").to_owned();
        }
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
            self.typed_at = Instant::now();
        }
        // the next blink is its own frame; nothing runs in between
        if let (Some(wait), Some(canvas)) = (blink, self.canvas) {
            request_canvas_redraw_at(canvas, Instant::now() + wait);
        }
        if changed {
            self.layout_dirty = true;
        }
        for event in self.bench.dock.take_tab_events() {
            if let DockTabEvent::CloseRequested(pane) = event {
                self.close_pane(pane);
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
        .bind(Chord::command(KeyCode::S).shift(), Command::SaveAs)
        .bind(Chord::command(KeyCode::N), Command::NewFile)
        .bind(Chord::command(KeyCode::O), Command::OpenFile)
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
        .bind(Chord::command(KeyCode::D), Command::Caret(CaretOp::AddNext))
        .bind(
            Chord::command(KeyCode::L).shift(),
            Command::Caret(CaretOp::SelectMatches),
        )
        .bind(
            Chord::key(KeyCode::Up).ctrl().alt(),
            Command::Caret(CaretOp::AddAbove),
        )
        .bind(
            Chord::key(KeyCode::Down).ctrl().alt(),
            Command::Caret(CaretOp::AddBelow),
        )
        .bind(Chord::command(KeyCode::F).shift(), Command::FindInFiles)
        .bind(Chord::command(KeyCode::P), Command::QuickOpen)
        .bind(Chord::command(KeyCode::P).shift(), Command::Commands)
        .bind(Chord::command(KeyCode::W), Command::CloseTab)
        .bind(Chord::key(KeyCode::Tab).ctrl(), Command::NextTab)
        .bind(Chord::key(KeyCode::Tab).ctrl().shift(), Command::PrevTab)
        .bind(Chord::command(KeyCode::F), Command::Find)
        .bind(Chord::command(KeyCode::H), Command::Replace)
        .bind(Chord::key(KeyCode::F3), Command::FindNext)
        .bind(Chord::key(KeyCode::F3).shift(), Command::FindPrev)
        .bind(Chord::command(KeyCode::G), Command::GotoLine)
        .bind(Chord::key(KeyCode::F12), Command::GotoDefinition)
        .bind(Chord::command(KeyCode::I), Command::ShowHover)
        .bind(Chord::key(KeyCode::Space).ctrl(), Command::Complete)
        .bind(Chord::command(KeyCode::Equals), Command::ZoomIn)
        .bind(Chord::command(KeyCode::Minus), Command::ZoomOut)
        .bind(Chord::command(KeyCode::Key0), Command::ZoomReset)
        .bind(Chord::command(KeyCode::B), Command::ToggleExplorer)
        .bind(Chord::command(KeyCode::Grave), Command::ToggleTerminal);
    map
}
