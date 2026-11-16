//! ligase. native development environment on aurea.
//!
//! `ligase [folder]` opens the folder, or the current one.

mod app;
mod completion;
mod context_menu;
mod document;
mod editor;
mod explorer;
mod git;
mod jobs;
mod lang;
mod lsp;
mod palette;
mod problems;
mod search;
mod settings;
mod syntax;
mod terminal;
mod workbench;

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::rc::{Rc, Weak};

use aurea::dialog::{FileDialog, MessageButtons, MessageChoice, MessageDialog, MessageKind};
use aurea::elements::{Orientation, Stack};
use aurea::menu::{MenuItem, SubMenu};
use aurea::render::{Canvas, CanvasId, RendererBackend, request_canvas_redraw};
use aurea::{AureaResult, Container, Window};

use crate::app::{App, Command, Dialog, DialogStart};
use crate::editor::{CaretOp, FoldOp, LineOp};
use crate::lang::Lang;
use crate::workbench::panels::Panel;

const WIDTH: u32 = 1280;
const HEIGHT: u32 = 800;

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("ligase: {e}");
            ExitCode::FAILURE
        }
    }
}

/// `path:line` split, when the path alone is a file; lines count from 1
/// there and from 0 back here. anything else stays as it is.
fn with_line(arg: PathBuf) -> (PathBuf, Option<usize>) {
    if arg.exists() {
        return (arg, None);
    }
    let text = arg.to_string_lossy();
    if let Some((path, line)) = text.rsplit_once(':')
        && let Ok(line) = line.parse::<usize>()
        && Path::new(path).is_file()
    {
        return (PathBuf::from(path), Some(line.saturating_sub(1)));
    }
    (arg, None)
}

fn run() -> AureaResult<()> {
    let arg = std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."));
    let arg = std::path::absolute(&arg).unwrap_or(arg);
    // a file opens in its folder, like `code file.rs`; `file.rs:12` at that line
    let (arg, line) = with_line(arg);
    let (root, file) = match arg.parent() {
        Some(dir) if arg.is_file() => (dir.to_path_buf(), Some(arg.clone())),
        _ => (arg, None),
    };
    let app = Rc::new(RefCell::new(App::new(&root, Lang::detect(None))));
    if let Some(file) = file {
        app.borrow_mut().open_at_start(file, line);
    }
    let title = app.borrow().title();
    let mut window = Window::new(&title, WIDTH.cast_signed(), HEIGHT.cast_signed())?;
    window.set_min_size(640, 400)?;
    let canvas = Canvas::new(WIDTH, HEIGHT, RendererBackend::Cpu)?;
    let id = canvas.id();
    app.borrow_mut().set_canvas(id);
    let mut ticks = menus(&mut window, &app, id)?;
    // the window only exists once built; frames reach it here
    let shown: Rc<RefCell<Weak<Window>>> = Rc::default();

    let drawn = Rc::clone(&app);
    let shown_in_draw = Rc::clone(&shown);
    canvas.set_draw_callback(move |ctx| {
        let window = shown_in_draw.borrow().upgrade();
        let (result, close, again) = {
            let mut a = drawn.borrow_mut();
            let result = a.draw(ctx);
            ticks.update(&a);
            if let Some(window) = &window {
                a.ui.sync_window(window);
            }
            let close = std::mem::take(&mut a.close_now);
            let dialog = a.take_dialog().map(|d| (d, a.dialog_start(d)));
            if let Some(dialog) = dialog {
                show_dialog(&drawn, &shown_in_draw, id, dialog);
            }
            (result, close, a.ui.wants_frame() || a.busy())
        };
        // the app is not borrowed past here; closing may send events back
        if close && let Some(window) = &window {
            window.close();
        }
        if again {
            request_canvas_redraw(id);
        }
        if let Err(e) = &result {
            eprintln!("ligase: draw: {e}");
        }
        result
    })?;

    let mut layout = Stack::new(Orientation::Vertical)?;
    layout.add_weighted(canvas, 1.0)?;
    window.set_content(layout)?;
    // closing waits for the layout to be written
    window.set_close_guard(true);
    let window = Rc::new(window);
    *shown.borrow_mut() = Rc::downgrade(&window);
    let input = Rc::clone(&app);
    let weak = Rc::downgrade(&window);
    window.on_event(move |event| {
        let Ok(mut a) = input.try_borrow_mut() else {
            // an event sent from inside a frame; the frame redraws anyway
            return;
        };
        let redraw = a.event(event);
        let close = std::mem::take(&mut a.close_now);
        drop(a);
        if close && let Some(window) = weak.upgrade() {
            window.close();
        }
        if redraw {
            request_canvas_redraw(id);
        }
    });
    window.show();
    window.run()
}

/// shows a native file dialog once this frame is over, outside the app
/// borrow: its modal loop can send events back to the window.
fn show_dialog(
    app: &Rc<RefCell<App>>,
    window: &Rc<RefCell<Weak<Window>>>,
    id: CanvasId,
    (dialog, start): (Dialog, DialogStart),
) {
    let app = Rc::clone(app);
    let window = Rc::clone(window);
    aurea::after_frame(move || {
        let Some(window) = window.borrow().upgrade() else {
            return;
        };
        let (name, folder) = match start {
            DialogStart::File(name, folder) => (name, folder),
            DialogStart::Message(title, text) | DialogStart::Confirm(title, text) => {
                let buttons = if matches!(dialog, Dialog::ConfirmDelete) {
                    MessageButtons::OkCancel
                } else {
                    MessageButtons::SaveDiscardCancel
                };
                let choice = MessageDialog::new(title)
                    .message(text)
                    .kind(MessageKind::Warning)
                    .buttons(buttons)
                    .show(&window)
                    .unwrap_or(MessageChoice::Cancel);
                if let Ok(mut a) = app.try_borrow_mut() {
                    a.answered(dialog, choice);
                }
                request_canvas_redraw(id);
                return;
            }
        };
        let picked = match dialog {
            Dialog::Open => FileDialog::new()
                .directory(&folder)
                .multiple(true)
                .open(&window)
                .unwrap_or_default(),
            Dialog::SaveAs(_) => FileDialog::new()
                .directory(&folder)
                .file_name(&name)
                .save(&window)
                .ok()
                .flatten()
                .into_iter()
                .collect(),
            Dialog::Unsaved(_) | Dialog::ConfirmDelete => Vec::new(),
        };
        if let Ok(mut a) = app.try_borrow_mut() {
            a.dialog_done(dialog, picked);
        }
        request_canvas_redraw(id);
    });
}

/// view menu items, ticked while their panel is in the dock.
struct Ticks {
    items: Vec<(Panel, MenuItem)>,
    shown: Vec<bool>,
}

impl Ticks {
    fn update(&mut self, app: &App) {
        for ((panel, item), shown) in self.items.iter_mut().zip(&mut self.shown) {
            let now = app.shows(*panel);
            if now != *shown {
                *shown = now;
                let _ = item.set_checked(now);
            }
        }
    }
}

fn menus(window: &mut Window, app: &Rc<RefCell<App>>, id: CanvasId) -> AureaResult<Ticks> {
    let a = app.borrow();
    let bar = window.create_menu_bar()?;
    let call = |command: Command| {
        let app = Rc::clone(app);
        move || {
            if let Ok(mut a) = app.try_borrow_mut() {
                a.run(command);
            }
            request_canvas_redraw(id);
        }
    };
    let add = |menu: &mut SubMenu, label: &str, command: Command| -> AureaResult<MenuItem> {
        let title = a.texts.tr(label);
        match a.keymap.chord_for(&command) {
            Some(chord) => menu.add_item_with_chord(title, chord, call(command)),
            None => menu.add_item(title, call(command)),
        }
    };
    let mut file = bar.add_submenu(a.texts.tr("File"))?;
    add(&mut file, "New File", Command::NewFile)?;
    add(&mut file, "Open File", Command::OpenFile)?;
    add(&mut file, "Quick Open", Command::QuickOpen)?;
    file.add_separator()?;
    add(&mut file, "Save", Command::Save)?;
    add(&mut file, "Save As", Command::SaveAs)?;
    add(&mut file, "Save All", Command::SaveAll)?;
    add(&mut file, "Close Tab", Command::CloseTab)?;
    add(&mut file, "Reopen Closed Tab", Command::ReopenClosed)?;
    let mut edit = bar.add_submenu(a.texts.tr("Edit"))?;
    add(&mut edit, "Undo", Command::Undo)?;
    add(&mut edit, "Redo", Command::Redo)?;
    edit.add_separator()?;
    add(&mut edit, "Cut", Command::Cut)?;
    add(&mut edit, "Copy", Command::Copy)?;
    add(&mut edit, "Paste", Command::Paste)?;
    edit.add_separator()?;
    add(&mut edit, "Select All", Command::SelectAll)?;
    add(&mut edit, "Select Line", Command::Line(LineOp::Select))?;
    add(
        &mut edit,
        "Add Next Match",
        Command::Caret(CaretOp::AddNext),
    )?;
    add(
        &mut edit,
        "Select All Matches",
        Command::Caret(CaretOp::SelectMatches),
    )?;
    add(
        &mut edit,
        "Add Caret Above",
        Command::Caret(CaretOp::AddAbove),
    )?;
    add(
        &mut edit,
        "Add Caret Below",
        Command::Caret(CaretOp::AddBelow),
    )?;
    edit.add_separator()?;
    add(&mut edit, "Find", Command::Find)?;
    add(&mut edit, "Replace", Command::Replace)?;
    add(&mut edit, "Find Next", Command::FindNext)?;
    add(&mut edit, "Find Previous", Command::FindPrev)?;
    edit.add_separator()?;
    add(&mut edit, "Toggle Comment", Command::Line(LineOp::Comment))?;
    add(&mut edit, "Move Line Up", Command::Line(LineOp::Move(true)))?;
    add(
        &mut edit,
        "Move Line Down",
        Command::Line(LineOp::Move(false)),
    )?;
    add(&mut edit, "Copy Line Up", Command::Line(LineOp::Copy(true)))?;
    add(
        &mut edit,
        "Copy Line Down",
        Command::Line(LineOp::Copy(false)),
    )?;
    add(&mut edit, "Delete Line", Command::Line(LineOp::Delete))?;
    let mut go = bar.add_submenu(a.texts.tr("Go"))?;
    add(&mut go, "Go to Line", Command::GotoLine)?;
    add(&mut go, "Go to Symbol", Command::GotoSymbol)?;
    add(&mut go, "Go to Definition", Command::GotoDefinition)?;
    add(&mut go, "Show Hover", Command::ShowHover)?;
    add(&mut go, "Find References", Command::References)?;
    go.add_separator()?;
    add(&mut go, "Rename Symbol", Command::Rename)?;
    add(&mut go, "Format Document", Command::Format)?;
    go.add_separator()?;
    add(&mut go, "Next Tab", Command::NextTab)?;
    add(&mut go, "Previous Tab", Command::PrevTab)?;
    let mut view = bar.add_submenu(a.texts.tr("View"))?;
    add(&mut view, "Command Palette", Command::Commands)?;
    add(&mut view, "Settings", Command::OpenSettings)?;
    add(&mut view, "Word Wrap", Command::ToggleWrap)?;
    view.add_separator()?;
    add(&mut view, "Fold", Command::Fold(FoldOp::Fold))?;
    add(&mut view, "Unfold", Command::Fold(FoldOp::Unfold))?;
    add(&mut view, "Fold All", Command::Fold(FoldOp::FoldAll))?;
    add(&mut view, "Unfold All", Command::Fold(FoldOp::UnfoldAll))?;
    view.add_separator()?;
    let explorer = add(&mut view, "Explorer", Command::ToggleExplorer)?;
    add(&mut view, "Find in Files", Command::FindInFiles)?;
    let terminal = add(&mut view, "Terminal", Command::ToggleTerminal)?;
    let problems = add(&mut view, "Problems", Command::ToggleProblems)?;
    let git = add(&mut view, "Source Control", Command::ToggleGit)?;
    add(&mut view, "Split Editor", Command::SplitEditor)?;
    view.add_separator()?;
    add(&mut view, "Zoom In", Command::ZoomIn)?;
    add(&mut view, "Zoom Out", Command::ZoomOut)?;
    add(&mut view, "Reset Zoom", Command::ZoomReset)?;
    view.add_separator()?;
    add(&mut view, "Reset Layout", Command::ResetLayout)?;
    Ok(Ticks {
        items: vec![
            (Panel::Explorer, explorer),
            (Panel::Terminal, terminal),
            (Panel::Problems, problems),
            (Panel::Git, git),
        ],
        shown: vec![false, false, false, false],
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_line_after_a_file_is_split_off() {
        let dir = std::env::temp_dir().join(format!("ligase-arg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a.rs");
        std::fs::write(&file, "x").unwrap();
        let with = PathBuf::from(format!("{}:12", file.display()));
        assert_eq!(with_line(with), (file.clone(), Some(11)));
        assert_eq!(with_line(file.clone()), (file.clone(), None));
        let missing = dir.join("b.rs:3");
        assert_eq!(with_line(missing.clone()), (missing, None), "no such file");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
