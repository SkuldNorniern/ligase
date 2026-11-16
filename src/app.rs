//! app state, input routing and one frame. long work goes to the file
//! worker; results come back through `Done`.

use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::time::Instant;

use aurea::render::{
    CanvasId, DrawingContext, Rect, request_canvas_redraw, request_canvas_redraw_at,
};
use aurea::ui::{Align, DockStyle, DockTabEvent, DockView, Sense, Theme, Ui, UiState};
use aurea::{AureaResult, Chord, CursorIcon, KeyCode, Keymap, WindowEvent};

use resolvase::{Encoding, Point, Revision, SavePoint};

use crate::completion::{self, Popup};
use crate::context_menu::{self, Act, ContextMenu};
use crate::document::{self, DocumentId, Documents, ViewId};
use crate::editor::cursor::Selection;
use crate::editor::find::{self, Bar, Mode};
use crate::editor::{self, CaretOp, FoldOp, LineOp, input};
use crate::explorer::{self, Click, Explorer};
use crate::jobs::Jobs;
use crate::lang::{Lang, Texts};
use crate::lsp::{self, Lsp};
use crate::palette::{self, Kind, Palette};
use crate::problems::{self, Problems};
use crate::search::{self, Finder};
use crate::settings::{self, Settings, ThemeChoice};
use crate::syntax::Syntax;
use crate::terminal::{self, Shell};
use crate::workbench::panels::{PaneId, Panel};
use crate::workbench::{self, Refused, Workbench};

/// tabs a rename may open for files that weren't open.
const MAX_OPENED_BY_EDIT: usize = 50;
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
    ProblemList,
    Menu,
    Welcome,
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
    Fold(FoldOp),
    Find,
    Replace,
    FindNext,
    FindPrev,
    GotoLine,
    GotoDefinition,
    ShowHover,
    Rename,
    Format,
    References,
    Complete,
    FindInFiles,
    ToggleExplorer,
    ToggleTerminal,
    ToggleProblems,
    SplitEditor,
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
    GotoSymbol,
    OpenSettings,
    ReopenClosed,
    ToggleWrap,
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
    ("Reopen Closed Tab", Command::ReopenClosed),
    ("Word Wrap", Command::ToggleWrap),
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
    ("Fold", Command::Fold(FoldOp::Fold)),
    ("Unfold", Command::Fold(FoldOp::Unfold)),
    ("Fold All", Command::Fold(FoldOp::FoldAll)),
    ("Unfold All", Command::Fold(FoldOp::UnfoldAll)),
    ("Find", Command::Find),
    ("Replace", Command::Replace),
    ("Find Next", Command::FindNext),
    ("Find Previous", Command::FindPrev),
    ("Find in Files", Command::FindInFiles),
    ("Go to Line", Command::GotoLine),
    ("Go to Symbol", Command::GotoSymbol),
    ("Go to Definition", Command::GotoDefinition),
    ("Show Hover", Command::ShowHover),
    ("Rename Symbol", Command::Rename),
    ("Format Document", Command::Format),
    ("Find References", Command::References),
    ("Complete", Command::Complete),
    ("Explorer", Command::ToggleExplorer),
    ("Terminal", Command::ToggleTerminal),
    ("Problems", Command::ToggleProblems),
    ("Split Editor", Command::SplitEditor),
    ("Zoom In", Command::ZoomIn),
    ("Zoom Out", Command::ZoomOut),
    ("Reset Zoom", Command::ZoomReset),
    ("Reset Layout", Command::ResetLayout),
    ("Settings", Command::OpenSettings),
];

/// a native dialog to show once the frame is done; main shows it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dialog {
    Open,
    SaveAs(ViewId),
    /// unsaved edits would go: the window closing (none) or one tab.
    Unsaved(Option<PaneId>),
    /// delete the file or folder in `App::delete_path`?
    ConfirmDelete,
}

/// how a dialog starts out.
pub enum DialogStart {
    /// a file dialog: the name and the folder.
    File(String, PathBuf),
    /// a message box: its title and text.
    Message(String, String),
    /// an ok / cancel question: its title and text.
    Confirm(String, String),
}

/// what a right-click menu was opened on.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Target {
    /// an explorer entry, and whether it is a folder.
    Entry(PathBuf, bool),
    /// the explorer's empty space: the workspace folder.
    Root,
    Tab(PaneId),
    Editor(ViewId),
}

/// what a prompt in the palette is for.
#[derive(Clone, Debug, PartialEq, Eq)]
enum PromptFor {
    NewFile(PathBuf),
    NewFolder(PathBuf),
    Rename(PathBuf),
}

/// a file operation's outcome, for the explorer and open documents.
#[derive(Debug, Default)]
struct FileDone {
    /// the folder to list again.
    rescan: PathBuf,
    open: Option<PathBuf>,
    moved: Option<(PathBuf, PathBuf)>,
    deleted: Option<PathBuf>,
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
    FileOp(Result<FileDone, String>),
    /// something the worker did that only has a status to say.
    Note(Option<String>),
    /// the settings file's text, when there is one.
    Settings(Option<String>),
    /// the settings file to open, made with every key shown when missing.
    SettingsFile(Result<PathBuf, String>),
    /// a file's text at HEAD; none when git doesn't track it.
    GitBase(DocumentId, Option<std::sync::Arc<str>>),
    GitHunks(DocumentId, Revision, Vec<crate::git::Hunk>),
    Branch(Option<String>),
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
    problems: Problems,
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
    menu: ContextMenu,
    menu_target: Option<Target>,
    prompt_for: Option<PromptFor>,
    /// what a delete waiting on its yes / no would remove.
    delete_path: Option<PathBuf>,
    /// tabs close all / close others still has to close.
    close_queue: Vec<PaneId>,
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
    /// a dialog main should show after this frame.
    dialog: Option<Dialog>,
    settings: Settings,
    /// server edits for files still being read, put in once they are.
    waiting_edits: Vec<(DocumentId, Vec<lsp::edit::Edit>, Encoding)>,
    /// files of tabs closed lately, newest last, for reopen.
    closed: Vec<PathBuf>,
    /// the theme in force is dark.
    dark: bool,
    edit_opts: editor::EditOpts,
    /// the system asked for less motion; read when the window gets focus.
    /// "save" was the answer to closing: close once every file is written.
    close_after_save: bool,
    /// the same for one tab.
    close_pane_after_save: Option<PaneId>,
    /// the terminal panel had the keyboard last frame.
    shell_focus: bool,
    canvas: Option<CanvasId>,
    /// session file for this workspace; none when there is no config dir.
    session: Option<PathBuf>,
    status: String,
    /// the branch the workspace folder is on, for the status bar.
    branch: Option<String>,
    /// the message as last drawn, and since when; it goes after a while.
    status_shown: (String, Instant),
    /// the editor view with the keyboard, from the last frame.
    focus: Option<ViewId>,
    /// the layout changed since it was last written.
    layout_dirty: bool,
    /// the saved session came back, or there was none.
    restored: bool,
    /// a file named on the command line, opened once the layout is back.
    open_after: Option<PathBuf>,
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
            problems: Problems::default(),
            lsp: Lsp::default(),
            hover: None,
            line_clip: None,
            zoom: editor::SIZE,
            bar: Bar::default(),
            palette: Palette::default(),
            menu: ContextMenu::default(),
            menu_target: None,
            prompt_for: None,
            delete_path: None,
            close_queue: Vec::new(),
            completion: None,
            want_complete: None,
            complete_for: None,
            last_editor: None,
            typed_at: Instant::now(),
            dialog: None,
            settings: Settings::default(),
            closed: Vec::new(),
            waiting_edits: Vec::new(),
            dark: true,
            edit_opts: editor::EditOpts::default(),
            close_after_save: false,
            close_pane_after_save: None,
            shell_focus: false,
            canvas: None,
            session: workbench::session_path(root),
            status: String::new(),
            branch: None,
            status_shown: (String::new(), Instant::now()),
            focus: None,
            layout_dirty: false,
            restored: false,
            open_after: None,
            closing: false,
            close_now: false,
        };
        app.scan_pending();
        if let Some(path) = settings::path() {
            app.jobs
                .run(move || Done::Settings(std::fs::read_to_string(path).ok()));
        }
        match app.session.clone() {
            Some(path) => {
                app.jobs
                    .run(move || Done::Session(std::fs::read_to_string(path).ok()));
            }
            None => app.restored = true,
        }
        app
    }

    /// opens `path` in an editor, after the saved layout when one is still
    /// being read, so the layout isn't thrown away for it.
    pub fn open_at_start(&mut self, path: PathBuf) {
        if self.restored {
            self.open_file(&path);
        } else {
            self.open_after = Some(path);
        }
    }

    pub fn set_canvas(&mut self, id: CanvasId) {
        self.jobs.set_canvas(id);
        self.syntax.set_canvas(id);
        self.canvas = Some(id);
        // the branch, now that its answer has a canvas to wake
        let root = self.explorer.root().to_path_buf();
        self.jobs
            .run(move || Done::Branch(crate::git::branch(&root)));
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
            self.refresh_git();
        }
        if matches!(event, WindowEvent::Unfocused) && self.settings.save_on_focus_loss {
            for view in self.docs.dirty_views() {
                if self.docs.doc_of(view).is_some_and(|d| !d.untitled) {
                    self.save(view);
                }
            }
        }
        if matches!(event, WindowEvent::ThemeChanged { .. })
            && self.settings.theme == ThemeChoice::System
        {
            self.apply_settings(self.settings.clone(), Vec::new());
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
            delta_y, modifiers, ..
        } = event
            && modifiers.command()
            && !self.shell_focus
            && *delta_y != 0.0
        {
            // ctrl (cmd on a mac) and the wheel zoom the editor
            self.zoom_by(if *delta_y > 0.0 { 1.0 } else { -1.0 });
            return true;
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
            editor::act_with(&mut self.docs, view, action, &self.edit_opts);
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
                            editor::act_with(
                                &mut self.docs,
                                view,
                                input::Action::Delete,
                                &self.edit_opts,
                            );
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
            Command::ToggleWrap => {
                self.settings.word_wrap = !self.settings.word_wrap;
                return;
            }
            Command::ReopenClosed => {
                if let Some(path) = self.closed.pop() {
                    let view = self.open_file(&path);
                    self.ui.focus(Id::Editor(view), 1);
                    self.last_editor = Some(view);
                }
                return;
            }
            Command::OpenSettings => {
                let Some(path) = settings::path() else {
                    self.status = self.texts.tr("no config folder for settings").to_owned();
                    return;
                };
                self.jobs
                    .run(move || Done::SettingsFile(settings_file(&path)));
                return;
            }
            Command::GotoSymbol => {
                self.palette.open(Kind::Symbols);
                self.ui.focus(Id::PaletteField, 0);
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
            Command::Fold(op) => {
                if let Some(view) = focus.or(self.last_editor) {
                    editor::fold_op(&mut self.docs, view, op);
                }
                return;
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
            Command::Rename => {
                if let Some(view) = focus.or(self.last_editor) {
                    // the bar starts with the name under the caret
                    let word =
                        self.docs
                            .doc_of(view)
                            .zip(self.docs.view(view))
                            .and_then(|(d, v)| {
                                let text = d.buffer.text();
                                text.slice(editor::cursor::word_at(text, v.sel.head).range())
                                    .ok()
                            });
                    self.open_bar(view, Mode::Rename);
                    self.bar.rename = word.unwrap_or_default();
                }
                return;
            }
            Command::Format => {
                if let Some(view) = focus.or(self.last_editor)
                    && !self.lsp.format(&self.docs, view, self.settings.tab_size)
                {
                    self.status = self.texts.tr("no language server for this yet").to_owned();
                }
                return;
            }
            Command::References => {
                if let Some(view) = focus.or(self.last_editor)
                    && !self.lsp.references(&self.docs, view)
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
            Command::ToggleProblems => self.bench.toggle(Panel::Problems),
            Command::SplitEditor => {
                let Some(view) = focus.or(self.last_editor) else {
                    return;
                };
                let Some(pane) = self.bench.panels.find(Panel::Editor(view)) else {
                    return;
                };
                let Some(old) = self.docs.view(view).cloned() else {
                    return;
                };
                // the new view starts where the old one is
                let other = self.docs.add_view(old.doc);
                if let Some(v) = self.docs.view_mut(other) {
                    v.sel = old.sel;
                    v.scroll = old.scroll;
                    v.folded = old.folded;
                    v.reveal = true;
                }
                self.bench.split(pane, other);
                self.ui.focus(Id::Editor(other), 1);
                self.last_editor = Some(other);
            }
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

    /// a rename or format came back: its edits go in, one undo step per
    /// file, all or none. dropped when any open file changed since the
    /// request: the server's positions are for the text it had.
    fn server_edits(
        &mut self,
        asking: DocumentId,
        revision: Revision,
        seen: lsp::Seen,
        files: Vec<(PathBuf, Vec<lsp::edit::Edit>)>,
        encoding: Encoding,
    ) {
        let moved = self.docs.doc(asking).map(|d| d.buffer.revision()) != Some(revision)
            || seen.iter().any(|(id, r)| {
                self.docs
                    .doc(*id)
                    .is_some_and(|d| d.buffer.revision() != *r)
            });
        if moved {
            self.status = self.texts.tr("the text changed, try again").to_owned();
            return;
        }
        let mut open = Vec::new();
        let mut closed = Vec::new();
        for (path, edits) in files {
            match self.view_of_path(&path) {
                Some(view) => open.push((view, edits)),
                None => closed.push((path, edits)),
            }
        }
        // every file that isn't open becomes a tab; past this many, no
        if closed.len() > MAX_OPENED_BY_EDIT {
            self.status = format!(
                "{} ({})",
                self.texts.tr("too many files to change here"),
                open.len() + closed.len()
            );
            return;
        }
        // all checked first, so a bad file never leaves the rest half done
        let mut planned = Vec::new();
        for (view, edits) in &open {
            match self.plan_server_edits(*view, edits, encoding) {
                Some(ranges) => planned.push((*view, ranges)),
                None => {
                    let name = self
                        .docs
                        .doc_of(*view)
                        .map(document::Document::name)
                        .unwrap_or_default();
                    self.status =
                        format!("{} {name}", self.texts.tr("the server's edits don't fit"));
                    return;
                }
            }
        }
        let changed = planned.len() + closed.len();
        for (view, ranges) in planned {
            self.commit_server_edits(view, ranges);
        }
        for (path, edits) in closed {
            let view = self.open_file(&path);
            if let Some(doc) = self.docs.view(view).map(|v| v.doc) {
                // goes in once the file is read
                self.waiting_edits.push((doc, edits, encoding));
            }
        }
        self.status = format!("{} {changed}", self.texts.tr("files changed:"));
    }

    /// a view on the document at `path`, when one is open.
    fn view_of_path(&self, path: &Path) -> Option<ViewId> {
        self.bench.panels.iter().find_map(|(_, p)| match p {
            Panel::Editor(v) if self.docs.doc_of(v).is_some_and(|d| d.path == path) => Some(v),
            _ => None,
        })
    }

    /// server edits as byte ranges in `view`'s text, sorted. none when
    /// they overlap or name lines the text doesn't have.
    fn plan_server_edits(
        &self,
        view: ViewId,
        edits: &[lsp::edit::Edit],
        encoding: Encoding,
    ) -> Option<Vec<(Range<usize>, String)>> {
        let text = self.docs.doc_of(view)?.buffer.text();
        let lines = text.line_count();
        if edits
            .iter()
            .any(|e| e.start.line > lines || e.end.line > lines)
        {
            return None;
        }
        let mut ranges: Vec<(Range<usize>, String)> = edits
            .iter()
            .map(|e| {
                let start = lsp::offset_near(text, e.start, encoding);
                let end = lsp::offset_near(text, e.end, encoding).max(start);
                (start..end, e.text.clone())
            })
            .collect();
        ranges.sort_by_key(|(r, _)| (r.start, r.end));
        if ranges.windows(2).any(|w| w[0].0.end > w[1].0.start) {
            return None;
        }
        Some(ranges)
    }

    /// makes planned edits as one undo step; the caret stays near where it
    /// was, not at the last edit.
    fn commit_server_edits(&mut self, view: ViewId, ranges: Vec<(Range<usize>, String)>) -> bool {
        if let Some((doc, _)) = self.docs.editable(view) {
            doc.buffer.break_join();
        }
        let keep = self.docs.view(view).map(|v| v.sel);
        let done = self.docs.edit(view, ranges, false);
        if let (true, Some(sel)) = (done, keep)
            && let Some((doc, v)) = self.docs.editable(view)
        {
            let len = doc.buffer.len();
            v.sel = Selection {
                anchor: sel.anchor.min(len),
                head: sel.head.min(len),
            };
        }
        done
    }

    /// references from a server, listed in the search panel.
    fn show_locations(&mut self, places: Vec<(PathBuf, Point)>, encoding: Encoding) {
        let hits = places
            .into_iter()
            .map(|(path, at)| {
                // open files show the line; others just their place
                let (column, preview) = self
                    .view_of_path(&path)
                    .and_then(|v| self.docs.doc_of(v))
                    .map(|d| {
                        let text = d.buffer.text();
                        let offset = lsp::offset_near(text, at, encoding);
                        let start = text.line_start(at.line).unwrap_or(0);
                        let end = editor::cursor::line_end(text, at.line);
                        let line = text.slice(start..end).unwrap_or_default();
                        (
                            offset.saturating_sub(start),
                            line.trim().chars().take(200).collect(),
                        )
                    })
                    // a closed file's column is in the server's encoding;
                    // its line start is the safe place to land
                    .unwrap_or((0, String::new()));
                search::Hit {
                    path,
                    line: at.line,
                    column,
                    len: 0,
                    preview,
                }
            })
            .collect::<Vec<_>>();
        if hits.is_empty() {
            self.status = self.texts.tr("no references").to_owned();
            return;
        }
        let title = self.texts.tr("references").to_owned();
        self.finder.show_results(title, hits);
        self.bench.show(Panel::Search);
        self.layout_dirty = true;
    }

    /// a right-click menu row was picked.
    fn menu_act(&mut self, act: Act) {
        let Some(target) = self.menu_target.take() else {
            return;
        };
        let root = self.explorer.root().to_path_buf();
        // the folder new entries go in, and the path the row is about
        let (folder, path) = match &target {
            Target::Entry(path, true) => (path.clone(), Some(path.clone())),
            Target::Entry(path, false) => (
                path.parent()
                    .map_or_else(|| root.clone(), Path::to_path_buf),
                Some(path.clone()),
            ),
            Target::Root => (root.clone(), None),
            Target::Tab(pane) => match self.bench.panels.get(*pane) {
                Some(Panel::Editor(view)) => {
                    let p = self
                        .docs
                        .doc_of(view)
                        .filter(|d| !d.untitled)
                        .map(|d| d.path.clone());
                    (root.clone(), p)
                }
                _ => (root.clone(), None),
            },
            Target::Editor(view) => (
                root.clone(),
                self.docs
                    .doc_of(*view)
                    .filter(|d| !d.untitled)
                    .map(|d| d.path.clone()),
            ),
        };
        match act {
            Act::Run(command) => {
                // the command acts on what was clicked
                let view = match target {
                    Target::Editor(view) => Some(view),
                    Target::Tab(pane) => match self.bench.panels.get(pane) {
                        Some(Panel::Editor(view)) => {
                            self.bench.dock.tree.activate(pane);
                            Some(view)
                        }
                        _ => None,
                    },
                    _ => None,
                };
                if let Some(view) = view {
                    self.focus = Some(view);
                    self.last_editor = Some(view);
                    self.ui.focus(Id::Editor(view), 1);
                }
                self.run(command);
            }
            Act::NewFile | Act::NewFolder => {
                let hint = if act == Act::NewFile {
                    self.texts.tr("new file's name")
                } else {
                    self.texts.tr("new folder's name")
                };
                self.prompt_for = Some(if act == Act::NewFile {
                    PromptFor::NewFile(folder)
                } else {
                    PromptFor::NewFolder(folder)
                });
                self.palette.prompt(hint.to_owned(), String::new());
                self.ui.focus(Id::PaletteField, 0);
            }
            Act::Rename => {
                let Some(path) = path else { return };
                let name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                self.prompt_for = Some(PromptFor::Rename(path));
                self.palette
                    .prompt(self.texts.tr("new name").to_owned(), name);
                self.ui.focus(Id::PaletteField, 0);
            }
            Act::Delete => {
                if let Some(path) = path {
                    self.delete_path = Some(path);
                    self.dialog = Some(Dialog::ConfirmDelete);
                }
            }
            Act::Reveal => {
                if let Some(path) = path {
                    // starts a file manager: a process, so on the worker
                    self.jobs.run(move || {
                        Done::Note(aurea::reveal_path(&path).err().map(|e| e.to_string()))
                    });
                }
            }
            Act::CopyPath => {
                if let Some(path) = path {
                    let _ = aurea::clipboard::set_clipboard_text(&path.display().to_string());
                }
            }
            Act::CloseOthers | Act::CloseAll => {
                let keep = match target {
                    Target::Tab(pane) if act == Act::CloseOthers => Some(pane),
                    _ => None,
                };
                let panes: Vec<PaneId> = self
                    .bench
                    .panels
                    .iter()
                    .filter(|(id, p)| matches!(p, Panel::Editor(_)) && Some(*id) != keep)
                    .map(|(id, _)| id)
                    .collect();
                // ones with unsaved edits ask, one at a time; each answer
                // goes on to the next
                self.close_queue = panes;
                self.close_next();
            }
        }
    }

    /// closes queued tabs until one asks about unsaved edits.
    fn close_next(&mut self) {
        while let Some(pane) = self.close_queue.first().copied() {
            self.close_queue.remove(0);
            if self.bench.panels.get(pane).is_none() {
                continue;
            }
            self.close_pane(pane);
            if self.dialog.is_some() {
                return;
            }
        }
    }

    /// the line typed into a prompt.
    fn prompt_done(&mut self, answer: &str) {
        let Some(prompt) = self.prompt_for.take() else {
            return;
        };
        let Some(name) = explorer::plain_name(answer) else {
            self.status = self.texts.tr("that name won't do").to_owned();
            return;
        };
        let name = name.to_owned();
        let job: Box<dyn FnOnce() -> Done + Send> = match prompt {
            PromptFor::NewFile(dir) => Box::new(move || {
                let path = dir.join(&name);
                Done::FileOp(explorer::create_file(&path).map(|()| FileDone {
                    rescan: dir,
                    open: Some(path),
                    ..FileDone::default()
                }))
            }),
            PromptFor::NewFolder(dir) => Box::new(move || {
                Done::FileOp(explorer::create_dir(&dir.join(&name)).map(|()| FileDone {
                    rescan: dir,
                    ..FileDone::default()
                }))
            }),
            PromptFor::Rename(from) => Box::new(move || {
                let dir = from.parent().map(Path::to_path_buf).unwrap_or_default();
                let to = dir.join(&name);
                Done::FileOp(explorer::rename(&from, &to).map(|()| FileDone {
                    rescan: dir,
                    moved: Some((from, to)),
                    ..FileDone::default()
                }))
            }),
        };
        self.jobs.run(job);
    }

    /// a file operation came back: the explorer lists the folder again,
    /// open documents follow a rename, a deleted file's text is kept.
    fn file_done(&mut self, done: FileDone) {
        if self.explorer.forget(&done.rescan) {
            self.scan(done.rescan.clone());
        }
        if let Some((from, to)) = &done.moved {
            self.docs.moved(from, to);
            self.layout_dirty = true;
        }
        if let Some(path) = &done.deleted {
            for id in self.docs.under(path) {
                // the text stays; saving puts the file back
                self.docs.mark_conflict(id);
            }
        }
        if let Some(path) = done.open {
            let view = self.open_file(&path);
            self.ui.focus(Id::Editor(view), 1);
            self.last_editor = Some(view);
        }
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
        self.completion = Some(Popup::new(view, items, start));
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
        let path = match self.bench.panels.get(pane) {
            Some(Panel::Editor(view)) => self
                .docs
                .doc_of(view)
                .filter(|d| !d.untitled)
                .map(|d| d.path.clone()),
            _ => None,
        };
        let result = self.bench.close(pane, &mut self.docs);
        if result.is_ok()
            && let Some(path) = path
        {
            self.remember_closed(path);
        }
        match result {
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
                if self.dialog.is_none() {
                    self.dialog = Some(Dialog::Unsaved(Some(pane)));
                }
            }
            Err(Refused::KeepsEditorArea) => {}
        }
    }

    /// keeps a closed tab's file for reopen; the last 20, no repeats.
    fn remember_closed(&mut self, path: PathBuf) {
        self.closed.retain(|p| *p != path);
        self.closed.push(path);
        if self.closed.len() > 20 {
            self.closed.remove(0);
        }
    }

    /// names of the files with unsaved edits that a close would drop.
    fn unsaved_names(&self, pane: Option<PaneId>) -> Vec<String> {
        match pane {
            Some(pane) => match self.bench.panels.get(pane) {
                Some(Panel::Editor(view)) => self
                    .docs
                    .doc_of(view)
                    .map(document::Document::name)
                    .into_iter()
                    .collect(),
                _ => Vec::new(),
            },
            None => self
                .docs
                .all()
                .filter(|d| d.is_dirty())
                .map(document::Document::name)
                .collect(),
        }
    }

    /// how the dialog main is about to show starts out.
    pub fn dialog_start(&self, dialog: Dialog) -> DialogStart {
        match dialog {
            Dialog::SaveAs(view) => {
                let (name, folder) = self.save_as_start(view);
                DialogStart::File(name, folder)
            }
            Dialog::Open => DialogStart::File(String::new(), self.root().to_path_buf()),
            Dialog::ConfirmDelete => {
                let name = self
                    .delete_path
                    .as_ref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_default();
                // open files with edits in there are named: their text stays
                // open, but there will be no folder to save them to
                let dirty: Vec<String> = self
                    .delete_path
                    .as_ref()
                    .map(|p| {
                        self.docs
                            .under(p)
                            .into_iter()
                            .filter_map(|id| self.docs.doc(id))
                            .filter(|d| d.is_dirty())
                            .map(document::Document::name)
                            .take(8)
                            .collect()
                    })
                    .unwrap_or_default();
                let mut text = format!("{}\n\n{name}", self.texts.tr("delete this for good?"));
                if !dirty.is_empty() {
                    text.push_str(&format!(
                        "\n\n{} {}",
                        self.texts.tr("unsaved changes in:"),
                        dirty.join(", ")
                    ));
                }
                DialogStart::Confirm(self.texts.tr("Delete").to_owned(), text)
            }
            Dialog::Unsaved(pane) => {
                let names = self.unsaved_names(pane);
                let mut list: Vec<String> = names.iter().take(8).cloned().collect();
                if names.len() > list.len() {
                    list.push(format!("+{}", names.len() - list.len()));
                }
                let title = self.texts.tr("Unsaved changes").to_owned();
                let text = format!(
                    "{}\n\n{}",
                    self.texts.tr("save the changes before closing?"),
                    list.join("\n")
                );
                DialogStart::Message(title, text)
            }
        }
    }

    /// the answer to a save / discard / cancel box.
    pub fn answered(&mut self, dialog: Dialog, choice: aurea::dialog::MessageChoice) {
        use aurea::dialog::MessageChoice;
        if dialog == Dialog::ConfirmDelete {
            if let (MessageChoice::Ok, Some(path)) = (choice, self.delete_path.take()) {
                let rescan = path.parent().map(Path::to_path_buf).unwrap_or_default();
                self.jobs.run(move || {
                    Done::FileOp(explorer::delete(&path).map(|()| FileDone {
                        rescan,
                        deleted: Some(path),
                        ..FileDone::default()
                    }))
                });
            }
            return;
        }
        let Dialog::Unsaved(pane) = dialog else {
            return;
        };
        match (pane, choice) {
            (None, MessageChoice::Save) => {
                self.close_after_save = true;
                self.save_all_for_close();
            }
            (None, MessageChoice::Discard) => self.close_saving_layout(),
            (Some(pane), MessageChoice::Save) => {
                if let Some(Panel::Editor(view)) = self.bench.panels.get(pane) {
                    self.close_pane_after_save = Some(pane);
                    self.save(view);
                }
            }
            (Some(_), MessageChoice::Cancel) => self.close_queue.clear(),
            (Some(pane), MessageChoice::Discard)
                if self.bench.close_discarding(pane, &mut self.docs).is_ok() =>
            {
                self.layout_dirty = true;
                if self
                    .last_editor
                    .is_some_and(|v| self.docs.view(v).is_none())
                {
                    self.last_editor = None;
                }
            }
            _ => {}
        }
        if pane.is_some() && choice != MessageChoice::Save {
            self.close_next();
        }
    }

    /// saves every file with edits for a close; a never saved one asks
    /// where first and the close waits for it.
    fn save_all_for_close(&mut self) {
        let views = self.docs.dirty_views();
        if views.is_empty() {
            self.close_saving_layout();
            return;
        }
        for view in views {
            // untitled ones ask where, one at a time
            if self.docs.doc_of(view).is_some_and(|d| d.untitled) {
                if self.dialog.is_none() {
                    self.dialog = Some(Dialog::SaveAs(view));
                }
            } else {
                self.save(view);
            }
        }
    }

    /// a save came back: a close that waited on it goes ahead.
    fn after_save(&mut self, doc: DocumentId, ok: bool) {
        if !ok {
            // a failed save keeps everything open
            self.close_after_save = false;
            self.close_pane_after_save = None;
            return;
        }
        if let Some(pane) = self.close_pane_after_save
            && let Some(Panel::Editor(view)) = self.bench.panels.get(pane)
            && self.docs.view(view).is_some_and(|v| v.doc == doc)
        {
            self.close_pane_after_save = None;
            self.close_pane(pane);
            self.close_next();
        }
        if self.close_after_save && !self.docs.any_dirty() {
            self.close_after_save = false;
            self.close_saving_layout();
        } else if self.close_after_save && self.dialog.is_none() {
            // more never saved files: ask for the next one
            self.save_all_for_close();
        }
    }

    /// what the palette asked for.
    fn palette_ask(&mut self, ask: palette::view::Ask) {
        let editor = self.last_editor;
        let (kind, query) = self.palette.wanted();
        let query = query.to_owned();
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
        if kind == Kind::Prompt {
            self.prompt_done(&query);
            return;
        }
        let Some(item) = picked else { return };
        match kind {
            Kind::Prompt => {}
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
            Kind::Symbols => {
                let at = self.palette.symbol_at.get(item).copied();
                if let (Some(view), Some(at)) = (editor, at)
                    && let Some(v) = self.docs.view_mut(view)
                {
                    v.sel = editor::cursor::Selection::caret(at);
                    v.extra.clear();
                    v.reveal = true;
                }
            }
        }
    }

    /// puts settings in force: theme, fonts, editing, language.
    fn apply_settings(&mut self, settings: Settings, problems: Vec<String>) {
        let dark = match settings.theme {
            ThemeChoice::Dark => true,
            ThemeChoice::Light => false,
            ThemeChoice::System => aurea::prefers_dark().unwrap_or(true),
        };
        let family = std::mem::take(&mut self.theme.font_family);
        self.theme = if dark { Theme::dark() } else { Theme::light() };
        self.dark = dark;
        self.theme.font_family = family;
        if settings.editor_size != self.settings.editor_size || self.zoom == editor::SIZE {
            self.zoom = settings.editor_size;
        }
        self.edit_opts = editor::EditOpts {
            unit: " ".repeat(settings.tab_size),
            auto_pairs: settings.auto_pairs,
        };
        if settings.language != self.settings.language {
            let lang = Lang::detect(settings.language.as_deref());
            lang.font_family().clone_into(&mut self.theme.font_family);
            self.texts = Texts::new(lang);
        }
        self.settings = settings;
        if !problems.is_empty() {
            self.status = format!(
                "{}: {}",
                self.texts.tr("settings skipped"),
                problems.join(", ")
            );
        }
    }

    fn settings_problem(&mut self, problems: &[String]) {
        self.status = format!(
            "{}: {}",
            self.texts.tr("settings skipped"),
            problems.join(", ")
        );
    }

    /// the settings file was saved: read it from the buffer, no disk.
    fn settings_saved(&mut self, doc: DocumentId) {
        let Some(d) = self.docs.doc(doc) else { return };
        if settings::path().as_deref() != Some(d.path.as_path()) {
            return;
        }
        let (settings, problems) = settings::parse(&d.buffer.text().to_string());
        let clean = problems.is_empty();
        match settings {
            Some(settings) => self.apply_settings(settings, problems),
            // not toml: keep what is in force, say why
            None => self.settings_problem(&problems),
        }
        if clean {
            self.status = self.texts.tr("settings applied").to_owned();
        }
    }

    /// the `@` list: symbols of the file in the last editor, in file order.
    fn fill_symbols(&mut self) {
        if self.palette.wanted().0 != Kind::Symbols {
            return;
        }
        let symbols = self
            .last_editor
            .and_then(|v| self.docs.doc_of(v))
            .map(|d| d.symbols.clone())
            .unwrap_or_default();
        let at = symbols.iter().map(|s| s.at).collect();
        let items = symbols
            .into_iter()
            .map(|s| palette::Item {
                text: s.name,
                note: s.kind.to_owned(),
            })
            .collect();
        self.palette.set_symbols(items, at);
    }

    /// opens the bar on `view`. find starts from the selection, when it is
    /// a bit of one line.
    fn open_bar(&mut self, view: ViewId, mode: Mode) {
        self.bar.mode = Some(mode);
        self.bar.view = Some(view);
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
            find::Ask::Rename => {
                let name = self.bar.rename.trim().to_owned();
                self.bar.mode = None;
                self.ui.focus(Id::Editor(view), 1);
                if name.is_empty() {
                    return;
                }
                if !self.lsp.rename(&self.docs, view, &name) {
                    self.status = self.texts.tr("no language server for this yet").to_owned();
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
            // ask: save, drop them, or stay; never over a dialog already asked
            if self.dialog.is_none() {
                self.dialog = Some(Dialog::Unsaved(None));
            }
            return;
        }
        self.close_saving_layout();
    }

    /// closes the window once the layout is written.
    fn close_saving_layout(&mut self) {
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
            // answered through `answered`
            Dialog::Unsaved(_) | Dialog::ConfirmDelete => {}
            Dialog::Open => {
                for path in picked {
                    let view = self.open_file(&path);
                    self.ui.focus(Id::Editor(view), 1);
                    self.last_editor = Some(view);
                }
            }
            Dialog::SaveAs(view) => {
                let Some(path) = picked.into_iter().next() else {
                    // cancelled: a close waiting on this file stays open
                    self.close_after_save = false;
                    self.close_pane_after_save = None;
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
        let (trim, end) = (
            self.settings.trim_trailing_whitespace,
            self.settings.final_newline,
        );
        if trim || end {
            editor::tidy_for_save(&mut self.docs, view, trim, end);
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
    /// asks git for `doc`'s text at HEAD, so its changed lines can be marked.
    fn ask_git_base(&mut self, doc: DocumentId) {
        let Some(path) = self
            .docs
            .doc(doc)
            .filter(|d| !d.untitled)
            .map(|d| d.path.clone())
        else {
            return;
        };
        self.jobs
            .run(move || Done::GitBase(doc, crate::git::base(&path)));
    }

    /// asks again for every file's HEAD and the branch: a commit or a
    /// checkout outside changes both.
    fn refresh_git(&mut self) {
        for (doc, _) in self.docs.files() {
            self.ask_git_base(doc);
        }
        let root = self.explorer.root().to_path_buf();
        self.jobs
            .run(move || Done::Branch(crate::git::branch(&root)));
    }

    /// sends a diff off for each file whose text moved on.
    fn ask_git_hunks(&mut self) {
        for (doc, revision, text, base) in self.docs.git_wanted() {
            self.jobs.run(move || {
                let now = text.slice(0..text.len()).unwrap_or_default();
                Done::GitHunks(doc, revision, crate::git::hunks(&base, &now))
            });
        }
    }

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
        self.ask_git_hunks();
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
                lsp::Event::Edits(doc, revision, seen, files, encoding) => {
                    self.server_edits(doc, revision, seen, files, encoding);
                }
                lsp::Event::Locations(places, encoding) => self.show_locations(places, encoding),
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
                    self.ask_git_base(doc);
                    let waiting: Vec<_> = self
                        .waiting_edits
                        .iter()
                        .filter(|w| w.0 == doc)
                        .cloned()
                        .collect();
                    self.waiting_edits.retain(|w| w.0 != doc);
                    for (_, edits, encoding) in waiting {
                        let view = self.view_of_path(
                            &self
                                .docs
                                .doc(doc)
                                .map(|d| d.path.clone())
                                .unwrap_or_default(),
                        );
                        let planned = view.and_then(|v| {
                            self.plan_server_edits(v, &edits, encoding).map(|r| (v, r))
                        });
                        match planned {
                            Some((view, ranges)) => {
                                self.commit_server_edits(view, ranges);
                            }
                            None => {
                                self.status =
                                    self.texts.tr("the server's edits don't fit").to_owned();
                            }
                        }
                    }
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
                    // not written: a close waiting on it stays open
                    self.after_save(doc, false);
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
                Done::Session(text) => {
                    self.restore(text);
                    if let Some(path) = self.open_after.take() {
                        self.open_file(&path);
                    }
                }
                Done::Wrote(doc, name, point, result) => match result {
                    Ok(disk) => {
                        self.docs.saved(doc, point);
                        self.docs.set_disk(doc, disk);
                        self.status = format!("{} {name}", self.texts.tr("saved"));
                        self.settings_saved(doc);
                        self.after_save(doc, true);
                    }
                    Err(why) => {
                        self.status = format!("{} {name}: {why}", self.texts.tr("can't save"));
                        self.after_save(doc, false);
                    }
                },
                Done::Shell(result) => self.shell.started(result),
                Done::Found(doc, revision, query, hits) => self.found((doc, revision, query), hits),
                Done::Files(files) => self.palette.set_files(files),
                Done::FileOp(Ok(done)) => self.file_done(done),
                Done::FileOp(Err(why)) => self.status = why,
                Done::GitBase(doc, base) => self.docs.set_git_base(doc, base),
                Done::GitHunks(doc, revision, hunks) => {
                    self.docs.set_git_hunks(doc, revision, hunks);
                }
                Done::Branch(branch) => self.branch = branch,
                Done::Note(note) => {
                    if let Some(note) = note {
                        self.status = note;
                    }
                }
                Done::Settings(text) => {
                    if let Some(text) = text {
                        match settings::parse(&text) {
                            (Some(settings), problems) => self.apply_settings(settings, problems),
                            (None, problems) => self.settings_problem(&problems),
                        }
                    }
                }
                Done::SettingsFile(Ok(path)) => {
                    let view = self.open_file(&path);
                    self.ui.focus(Id::Editor(view), 1);
                    self.last_editor = Some(view);
                }
                Done::SettingsFile(Err(why)) => {
                    self.status = format!("{}: {why}", self.texts.tr("can't open the settings"));
                }
                Done::Server(language, result) => {
                    // a missing server is fine; the editor works without it, and
                    // the status bar says so beside the language
                    let _ = self.lsp.started(language, result);
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
                    Panel::Problems => {
                        let n = self.problems.rows.len();
                        if n == 0 {
                            self.texts.tr("Problems").to_owned()
                        } else {
                            format!("{} {n}", self.texts.tr("Problems"))
                        }
                    }
                    Panel::Welcome => self.texts.tr("Welcome").to_owned(),
                    Panel::Editor(view) => self.docs.doc_of(view).map_or_else(String::new, |d| {
                        // ! when the file also changed on disk
                        if d.conflict {
                            format!("{} !", d.name())
                        } else {
                            d.name()
                        }
                    }),
                };
                (id, title)
            })
            .collect()
    }

    /// tabs with unsaved changes, for the dock's dots.
    fn dirty_panes(&self) -> HashSet<PaneId> {
        self.bench
            .panels
            .iter()
            .filter(|(_, panel)| {
                matches!(panel, Panel::Editor(view)
                    if self.docs.doc_of(*view).is_some_and(|d| d.is_dirty()))
            })
            .map(|(id, _)| id)
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
        let dirty = self.dirty_panes();
        let mut clicks: Vec<Click> = Vec::new();
        let mut welcome_pick: Option<Command> = None;
        let mut explorer_menu: Option<(aurea::render::Point, Option<(PathBuf, bool)>)> = None;
        let mut editor_menu: Option<(aurea::render::Point, ViewId)> = None;
        let menu_act;
        let mut focus = None;
        let mut ime = None;
        let mut pressed: Vec<ViewId> = Vec::new();
        let mut shell_focus = false;
        self.refilter_completion();
        if self.shows(Panel::Problems) {
            self.problems.gather(&self.docs);
        }
        let mut problem_picked = None;
        self.fill_symbols();
        let mut picked_completion = None;
        let mut found = None;
        let mut blink = None;
        let idle = self.typed_at.elapsed();
        let zoom = self.zoom;
        let mut definition = None;
        let mut rects: Vec<(ViewId, Rect)> = Vec::new();
        let settings = &self.settings;
        let dark = self.dark;
        let terminal_size = self.settings.terminal_size;
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
        let active_file = self
            .last_editor
            .and_then(|v| self.docs.doc_of(v))
            .filter(|d| !d.untitled)
            .map(|d| d.path.clone());
        let keymap = &self.keymap;
        {
            let Workbench { dock, panels } = &mut self.bench;
            let (explorer, docs, texts) = (&mut self.explorer, &mut self.docs, &self.texts);
            let shell = &mut self.shell;
            let hover = &self.hover;
            let finder = &mut self.finder;
            let problems = &mut self.problems;
            let bar = &mut self.bar;
            let mut ui = self.ui.frame(ctx, &self.theme);
            let (status, main) = area.split_bottom(STATUS_HEIGHT, 0.0);
            let title = |pane: PaneId| titles.get(&pane).map_or("", String::as_str);
            let content = |ui: &mut Ui<'_, Id>, pane: PaneId, rect: Rect| match panels.get(pane) {
                Some(Panel::Explorer) => {
                    let shown = explorer::view::show(ui, explorer, rect, active_file.as_deref())?;
                    clicks.extend(shown.click);
                    if let Some((at, row)) = shown.menu {
                        explorer_menu = Some((at, row.and_then(|r| explorer.row_path(r))));
                    }
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
                        let crumbs = (!doc.untitled).then(|| {
                            let path = doc.path.strip_prefix(&root).unwrap_or(&doc.path);
                            path.to_string_lossy().replace('\\', "/")
                        });
                        let look = editor::Look {
                            settings,
                            dark,
                            crumbs: crumbs.as_deref(),
                            size: zoom,
                            idle,
                            hover,
                            found,
                            current,
                        };
                        rects.push((view, rect));
                        let shown = editor::show(ui, view, doc, &mut state, rect, texts, &look)?;
                        if shown.focused {
                            focus = Some(view);
                            ime = shown.caret;
                            if let Some(caret) = ime {
                                ui.set_ime_area(caret);
                            }
                            blink = shown.blink;
                        }
                        if shown.pressed {
                            pressed.push(view);
                        }
                        if shown.definition {
                            definition = Some(view);
                        }
                        if let Some(at) = shown.menu {
                            editor_menu = Some((at, view));
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
                Some(Panel::Problems) => {
                    problem_picked = problems::show(ui, problems, &root, rect, texts)?;
                    Ok(())
                }
                Some(Panel::Terminal) => {
                    shell_focus = terminal::show(ui, shell, rect, texts, terminal_size)?;
                    Ok(())
                }
                Some(Panel::Welcome) | None => {
                    welcome_pick = welcome(ui, rect, texts, keymap)?.or(welcome_pick);
                    Ok(())
                }
            };
            let response = DockView::new(dock, title, content)
                .style(DockStyle::new(&self.theme))
                .close_buttons(true)
                .dirty(|pane| dirty.contains(&pane))
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
            menu_act = self.menu.show(&mut ui, texts, &self.keymap);
            palette_ask = palette::view::show(&mut ui, &mut self.palette, main, texts)?;
            status_bar(&mut ui, status, self.branch.as_deref(), &message, &place)?;
        }
        self.focus = focus;
        if focus.is_some() {
            self.last_editor = focus;
        }
        if let Some(ask) = bar_ask {
            self.bar_ask(ask);
        }
        if let Some(ask) = palette_ask {
            self.palette_ask(ask);
        }
        if let Some(p) = problem_picked {
            let view = self.open_file(&p.path);
            if let Some(v) = self.docs.view_mut(view) {
                v.goto = Some((Point::new(p.line, p.column), Encoding::Utf32));
            }
            self.ui.focus(Id::Editor(view), 1);
            self.last_editor = Some(view);
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
        // a message stays a while, then the frame after takes it away
        if self.status != self.status_shown.0 {
            self.status_shown = (self.status.clone(), Instant::now());
            if let (false, Some(canvas)) = (self.status.is_empty(), self.canvas) {
                request_canvas_redraw_at(canvas, Instant::now() + STATUS_FOR);
            }
        } else if !self.status.is_empty() && self.status_shown.1.elapsed() >= STATUS_FOR {
            self.status.clear();
            self.status_shown.0.clear();
            if let Some(canvas) = self.canvas {
                request_canvas_redraw(canvas);
            }
        }
        if changed {
            self.layout_dirty = true;
        }
        for event in self.bench.dock.take_tab_events() {
            match event {
                DockTabEvent::CloseRequested(pane) => self.close_pane(pane),
                DockTabEvent::ContextMenu { panel, at } => {
                    let editor = matches!(self.bench.panels.get(panel), Some(Panel::Editor(_)));
                    self.menu_target = Some(Target::Tab(panel));
                    self.menu.ask(at, context_menu::tab(editor));
                }
            }
        }
        if let Some((at, entry)) = explorer_menu {
            // the workspace folder's own row is never renamed or deleted
            let root = self.explorer.root().to_path_buf();
            let entry = entry.filter(|(path, _)| *path != root);
            let on_entry = entry.is_some();
            self.menu_target = Some(match entry {
                Some((path, dir)) => Target::Entry(path, dir),
                None => Target::Root,
            });
            self.menu.ask(at, context_menu::explorer(on_entry));
        }
        if let Some((at, view)) = editor_menu {
            self.menu_target = Some(Target::Editor(view));
            self.menu.ask(at, context_menu::editor());
        }
        if let Some(command) = welcome_pick {
            self.run(command);
        }
        if let Some(act) = menu_act {
            self.menu_act(act);
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
        let indent = format!("{} {}", self.texts.tr("Spaces:"), self.settings.tab_size);
        let eol = if doc.crlf { "CRLF" } else { "LF" };
        let language = match doc.language {
            Some(language) => match self.lsp.state(language) {
                Some(lsp::ServerState::Missing) => {
                    format!("{language} ({})", self.texts.tr("no language server"))
                }
                Some(lsp::ServerState::Starting) => format!("{language} …"),
                _ => language.to_owned(),
            },
            None => "text".to_owned(),
        };
        let about = format!("{at}    {indent}    UTF-8    {eol}    {language}");
        if errors + warnings == 0 {
            about
        } else {
            format!("E {errors}  W {warnings}    {about}")
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

/// the settings file, made with every key shown (commented out) when it
/// isn't there yet. runs on the worker.
fn settings_file(path: &Path) -> Result<PathBuf, String> {
    if !path.exists() {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        std::fs::write(path, settings::STARTER).map_err(|e| e.to_string())?;
    }
    Ok(path.to_path_buf())
}

/// what the welcome page offers, with the label its menu item has.
const WELCOME: [(&str, Command); 7] = [
    ("Quick Open", Command::QuickOpen),
    ("Command Palette", Command::Commands),
    ("New File", Command::NewFile),
    ("Open File", Command::OpenFile),
    ("Find in Files", Command::FindInFiles),
    ("Terminal", Command::ToggleTerminal),
    ("Settings", Command::OpenSettings),
];

/// the page with no file open: a few commands and their keys, each one a
/// button. the command clicked comes back.
fn welcome(
    ui: &mut Ui<'_, Id>,
    rect: Rect,
    texts: &Texts,
    keymap: &Keymap<Command>,
) -> AureaResult<Option<Command>> {
    const ROW: f32 = 30.0;
    let palette = ui.theme.palette;
    let title_font = ui.theme.font(22.0);
    let font = ui.theme.font(13.0);
    let width = 340.0_f32.min(rect.width - 32.0).max(0.0);
    let height = 44.0 + 28.0 + WELCOME.len() as f32 * ROW;
    let x = rect.x + (rect.width - width) / 2.0;
    let mut y = rect.y + ((rect.height - height) / 2.0).max(16.0);
    let title = Rect::new(x + 10.0, y, width, 36.0);
    ui.text(
        texts.tr("Ligase"),
        title,
        &title_font,
        palette.text,
        Align::Start,
    )?;
    y += 44.0;
    let note = Rect::new(x + 10.0, y, width, 20.0);
    let line = texts.tr("open a file from the explorer");
    ui.text(line, note, &font, palette.dim, Align::Start)?;
    y += 28.0;
    let mut picked = None;
    for (i, (label, command)) in WELCOME.iter().enumerate() {
        let row = Rect::new(x, y + i as f32 * ROW, width, ROW - 2.0);
        let input = ui.interact(Id::Welcome, i as u16, row, Sense::Click, true);
        if input.hovered {
            ui.round(row, 5.0, palette.hover)?;
            ui.set_cursor(CursorIcon::Pointer);
        }
        let inner = row.inset_xy(10.0, 0.0);
        ui.text(texts.tr(label), inner, &font, palette.text, Align::Start)?;
        if let Some(chord) = keymap.chord_for(command) {
            ui.text(&chord.label(), inner, &font, palette.dim, Align::End)?;
        }
        if input.clicked {
            picked = Some(*command);
        }
    }
    Ok(picked)
}

/// how long a status message stays.
const STATUS_FOR: std::time::Duration = std::time::Duration::from_secs(8);

fn status_bar(
    ui: &mut Ui<'_, Id>,
    rect: Rect,
    branch: Option<&str>,
    message: &str,
    place: &str,
) -> AureaResult<()> {
    let palette = ui.theme.palette;
    ui.fill(rect, palette.surface)?;
    ui.fill(Rect::new(rect.x, rect.y, rect.width, 1.0), palette.border)?;
    let font = ui.theme.font(12.0);
    let mut inner = rect.inset_xy(10.0, 0.0);
    // the branch first, the way vs code and zed start theirs
    if let Some(branch) = branch {
        let wanted = ui.text_width(branch, &font) + 16.0;
        let (left, rest) = inner.split_left(wanted.min(inner.width * 0.3), 8.0);
        ui.text_clipped(branch, left, &font, palette.text, Align::Start)?;
        inner = rest;
    }
    // the file's facts take what they need, the message the rest
    let wanted = ui.text_width(place, &font) + 8.0;
    let (right, left) = inner.split_right(wanted.min(inner.width * 0.6), 8.0);
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
            Chord::command(KeyCode::LeftBracket).shift(),
            Command::Fold(FoldOp::Fold),
        )
        .bind(
            Chord::command(KeyCode::RightBracket).shift(),
            Command::Fold(FoldOp::Unfold),
        )
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
        .bind(Chord::command(KeyCode::O).shift(), Command::GotoSymbol)
        .bind(Chord::command(KeyCode::W), Command::CloseTab)
        .bind(Chord::command(KeyCode::T).shift(), Command::ReopenClosed)
        .bind(Chord::key(KeyCode::Z).alt(), Command::ToggleWrap)
        .bind(Chord::key(KeyCode::Tab).ctrl(), Command::NextTab)
        .bind(Chord::key(KeyCode::Tab).ctrl().shift(), Command::PrevTab)
        .bind(Chord::command(KeyCode::F), Command::Find)
        .bind(Chord::command(KeyCode::H), Command::Replace)
        .bind(Chord::key(KeyCode::F3), Command::FindNext)
        .bind(Chord::key(KeyCode::F3).shift(), Command::FindPrev)
        .bind(Chord::command(KeyCode::G), Command::GotoLine)
        .bind(Chord::key(KeyCode::F12), Command::GotoDefinition)
        .bind(Chord::key(KeyCode::F12).shift(), Command::References)
        .bind(Chord::key(KeyCode::F2), Command::Rename)
        .bind(Chord::key(KeyCode::F).alt().shift(), Command::Format)
        .bind(Chord::command(KeyCode::I), Command::ShowHover)
        .bind(Chord::key(KeyCode::Space).ctrl(), Command::Complete)
        .bind(Chord::command(KeyCode::Equals), Command::ZoomIn)
        .bind(Chord::command(KeyCode::Minus), Command::ZoomOut)
        .bind(Chord::command(KeyCode::Key0), Command::ZoomReset)
        .bind(Chord::command(KeyCode::B), Command::ToggleExplorer)
        .bind(Chord::command(KeyCode::Grave), Command::ToggleTerminal)
        .bind(Chord::command(KeyCode::M).shift(), Command::ToggleProblems)
        .bind(Chord::command(KeyCode::Backslash), Command::SplitEditor);
    map
}

#[cfg(test)]
mod tests {
    use super::*;
    use aurea::dialog::MessageChoice;

    fn app() -> App {
        let dir = std::env::temp_dir().join(format!("ligase-app-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        App::new(&dir, Lang::En)
    }

    /// a file with an edit in it, open in a tab.
    fn dirty(app: &mut App) -> ViewId {
        let doc = app.docs.new_untitled();
        let view = app.docs.add_view(doc);
        app.docs.edit(view, vec![(0..0, "x".to_owned())], false);
        app.bench.show_editor(view);
        view
    }

    #[test]
    fn close_with_nothing_unsaved_closes() {
        let mut app = app();
        app.event(&WindowEvent::CloseRequested);
        assert!(app.close_now || app.closing);
        assert!(app.take_dialog().is_none());
    }

    #[test]
    fn close_with_edits_asks_and_discard_closes() {
        let mut app = app();
        dirty(&mut app);
        app.event(&WindowEvent::CloseRequested);
        assert!(!app.close_now && !app.closing, "not before the answer");
        let dialog = app.take_dialog().expect("asks");
        assert_eq!(dialog, Dialog::Unsaved(None));
        app.answered(dialog, MessageChoice::Cancel);
        assert!(!app.close_now && !app.closing);
        app.answered(dialog, MessageChoice::Discard);
        assert!(app.close_now || app.closing);
    }

    #[test]
    fn save_on_close_asks_where_for_a_new_file() {
        let mut app = app();
        let view = dirty(&mut app);
        app.answered(Dialog::Unsaved(None), MessageChoice::Save);
        assert_eq!(app.take_dialog(), Some(Dialog::SaveAs(view)));
        // cancelling that keeps the window open
        app.dialog_done(Dialog::SaveAs(view), Vec::new());
        assert!(!app.close_after_save);
    }

    #[test]
    fn closing_a_dirty_tab_asks_and_discard_drops_it() {
        let mut app = app();
        let view = dirty(&mut app);
        let pane = app.bench.panels.find(Panel::Editor(view)).unwrap();
        app.close_pane(pane);
        assert_eq!(app.take_dialog(), Some(Dialog::Unsaved(Some(pane))));
        app.answered(Dialog::Unsaved(Some(pane)), MessageChoice::Discard);
        assert!(app.docs.view(view).is_none());
    }

    #[test]
    fn new_file_from_the_menu_asks_a_name() {
        let mut app = app();
        app.menu_target = Some(Target::Root);
        app.menu_act(Act::NewFile);
        assert!(matches!(app.prompt_for, Some(PromptFor::NewFile(_))));
        assert_eq!(app.palette.kind, Some(Kind::Prompt));
        // a name with a slash is refused before any file work
        app.prompt_done("a/b");
        assert!(app.prompt_for.is_none());
        assert!(!app.status.is_empty());
    }
}
