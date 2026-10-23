//! ligase. native development environment on aurea.
//!
//! `ligase [folder]` opens the folder, or the current one.

mod app;
mod document;
mod editor;
mod explorer;
mod jobs;
mod lang;
mod workbench;

use std::cell::RefCell;
use std::path::PathBuf;
use std::process::ExitCode;
use std::rc::{Rc, Weak};

use aurea::elements::{Orientation, Stack};
use aurea::menu::{MenuItem, SubMenu};
use aurea::render::{Canvas, CanvasId, RendererBackend, request_canvas_redraw};
use aurea::{AureaResult, Container, Window};

use crate::app::{App, Command};
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

fn run() -> AureaResult<()> {
    let root = std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."));
    let root = std::path::absolute(&root).unwrap_or(root);
    let app = Rc::new(RefCell::new(App::new(&root, Lang::detect(None))));
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
                // the editor is not an aurea text field, so its caret is set here
                if let Some(caret) = a.ime {
                    let _ = window.set_ime_cursor_area(caret);
                }
            }
            let close = std::mem::take(&mut a.close_now);
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
        let redraw = a.event(&event);
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
    add(&mut file, "Save", Command::Save)?;
    let mut edit = bar.add_submenu(a.texts.tr("Edit"))?;
    add(&mut edit, "Undo", Command::Undo)?;
    add(&mut edit, "Redo", Command::Redo)?;
    edit.add_separator()?;
    add(&mut edit, "Cut", Command::Cut)?;
    add(&mut edit, "Copy", Command::Copy)?;
    add(&mut edit, "Paste", Command::Paste)?;
    edit.add_separator()?;
    add(&mut edit, "Select All", Command::SelectAll)?;
    let mut view = bar.add_submenu(a.texts.tr("View"))?;
    let explorer = add(&mut view, "Explorer", Command::ToggleExplorer)?;
    let terminal = add(&mut view, "Terminal", Command::ToggleTerminal)?;
    view.add_separator()?;
    add(&mut view, "Reset Layout", Command::ResetLayout)?;
    Ok(Ticks {
        items: vec![(Panel::Explorer, explorer), (Panel::Terminal, terminal)],
        shown: vec![false, false],
    })
}
