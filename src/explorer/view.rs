use std::path::Path;

use aurea::render::{Point, Rect};
use aurea::ui::{Align, ListView, Ui};
use aurea::{AureaResult, KeyCode};

use super::{Click, Explorer};
use crate::app::Id;
use crate::icons;

const ROW: f32 = 24.0;
const INDENT: f32 = 14.0;

/// what a frame of the tree tells the app.
#[derive(Default)]
pub struct Shown {
    /// a click on a row to act on.
    pub click: Option<Click>,
    /// a right click: where, and the row under it if any.
    pub menu: Option<(Point, Option<usize>)>,
    /// a file being dragged out, by name, while it is.
    pub dragging: Option<String>,
    /// a file let go away from the tree, and where.
    pub dropped: Option<(std::path::PathBuf, Point)>,
}

/// how far a press moves before it is a drag.
const DRAG_FROM: f32 = 6.0;

/// the char a letter or digit key types, for jumping by name.
fn letter(key: KeyCode) -> Option<char> {
    use KeyCode::*;
    let c = match key {
        A => 'a',
        B => 'b',
        C => 'c',
        D => 'd',
        E => 'e',
        F => 'f',
        G => 'g',
        H => 'h',
        I => 'i',
        J => 'j',
        K => 'k',
        L => 'l',
        M => 'm',
        N => 'n',
        O => 'o',
        P => 'p',
        Q => 'q',
        R => 'r',
        S => 's',
        T => 't',
        U => 'u',
        V => 'v',
        W => 'w',
        X => 'x',
        Y => 'y',
        Z => 'z',
        Key0 => '0',
        Key1 => '1',
        Key2 => '2',
        Key3 => '3',
        Key4 => '4',
        Key5 => '5',
        Key6 => '6',
        Key7 => '7',
        Key8 => '8',
        Key9 => '9',
        _ => return None,
    };
    Some(c)
}

/// draws the tree. a click on a row comes back for the app to act on.
/// `active` is the file in the editor with the keyboard; its row is marked.
pub fn show(
    ui: &mut Ui<'_, Id>,
    explorer: &mut Explorer,
    rect: Rect,
    active: Option<&Path>,
) -> AureaResult<Shown> {
    let font = ui.theme.font(13.0);
    let palette = ui.theme.palette;
    let before = explorer.selected;
    let mut selected = explorer.selected;
    let mut scroll = explorer.scroll;
    let tree = &*explorer;
    let response = ListView::new(&mut scroll, tree.len(), ROW)
        .selected(&mut selected)
        .show(ui, Id::Explorer, rect, |ui, row| {
            let Some(entry) = tree.row(row.index) else {
                return Ok(());
            };
            // inset and rounded, so a row reads as a chip, not a stripe
            let lit = row.rect.inset_xy(4.0, 1.0);
            if row.picked {
                ui.round(lit, 6.0, palette.selection)?;
            } else if active.is_some_and(|a| same_path(a, entry.path)) {
                ui.round(lit, 6.0, palette.raised)?;
            } else if row.hovered {
                ui.round(lit, 6.0, palette.hover)?;
            }
            // one faint line per folder this row sits in, under its arrow
            for depth in 0..entry.depth {
                let x = row.rect.x + 8.0 + f32::from(depth) * INDENT + 6.0;
                let line = Rect::new(x.round(), row.rect.y, 1.0, row.rect.height);
                ui.fill(line, palette.border.fade(0.55))?;
            }
            let indent = 8.0 + f32::from(entry.depth) * INDENT;
            let (mark, rest) = row.rect.split_left(indent + 12.0, 4.0);
            if entry.dir {
                chevron(ui, mark.split_right(12.0, 0.0).0, entry.open, palette.dim)?;
            }
            // the file or folder's icon, then its name
            let (icon_at, name) = rest.split_left(16.0, 6.0);
            let icon_box = Rect::new(icon_at.x, icon_at.y + (ROW - 16.0) / 2.0, 16.0, 16.0);
            let icon = icons::for_path(entry.path, entry.dir, entry.open);
            icons::draw(ui, icon, icon_box, palette.dim)?;
            ui.text_clipped(entry.name, name, &font, palette.text, Align::Start)?;
            Ok(())
        })?;
    explorer.scroll = scroll;
    explorer.selected = selected;
    // a click picks a row; arrow keys only move the selection.
    // double click or enter acts on the row already picked.
    let keyed = ui.keys().any(|(key, _)| {
        matches!(
            key,
            KeyCode::Up
                | KeyCode::Down
                | KeyCode::PageUp
                | KeyCode::PageDown
                | KeyCode::Home
                | KeyCode::End
        )
    });
    // letters jump to the next row starting with what was typed; a pause
    // starts over
    let letters: String = ui
        .keys()
        .filter(|(_, m)| !m.ctrl && !m.alt && !m.meta)
        .filter_map(|(key, _)| letter(key))
        .collect();
    let mut jumped = false;
    if !letters.is_empty() {
        let now = std::time::Instant::now();
        let fresh = explorer
            .typed
            .1
            .is_none_or(|t| now.duration_since(t) > std::time::Duration::from_millis(900));
        if fresh {
            explorer.typed.0.clear();
        }
        explorer.typed.0.push_str(&letters);
        explorer.typed.1 = Some(now);
        // the same letter again steps on to the next match
        let one = explorer
            .typed
            .0
            .chars()
            .all(|c| explorer.typed.0.starts_with(c));
        let prefix = if one {
            explorer.typed.0.chars().take(1).collect::<String>()
        } else {
            explorer.typed.0.clone()
        };
        // the same letter again steps on; a fresh word starts at the row
        // it is on, so "ca" on Cargo.toml stays there
        let from = match explorer.selected {
            Some(i) if one => i + 1,
            Some(i) => i,
            None => 0,
        };
        if let Some(i) = explorer.find_from(from, &prefix) {
            explorer.selected = Some(i);
            // into sight
            let top = i as f32 * ROW;
            if top < explorer.scroll || top + ROW > explorer.scroll + rect.height {
                explorer.scroll = (top - rect.height / 3.0).max(0.0);
            }
            jumped = true;
        }
    }
    // a file row pressed and pulled away is a drag; let go outside the
    // tree, it lands where the pointer is
    let mut dragging = None;
    let mut dropped = None;
    let held = ui.held_at(Id::Explorer, 0).is_some();
    let mut released_drag = false;
    match (ui.held_at(Id::Explorer, 0), ui.pointer()) {
        (Some(from), Some(at)) => {
            if explorer.drag.is_none() {
                let row = ((from.y - rect.y + scroll) / ROW).floor();
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                let row = (row >= 0.0).then_some(row as usize);
                explorer.drag = row
                    .and_then(|r| explorer.row(r))
                    .filter(|r| !r.dir)
                    .map(|r| (r.path.to_path_buf(), false));
            }
            if let Some((path, started)) = &mut explorer.drag {
                *started |= (at.x - from.x).hypot(at.y - from.y) > DRAG_FROM;
                if *started {
                    dragging = path.file_name().map(|n| n.to_string_lossy().into_owned());
                }
            }
        }
        _ => {
            if let Some((path, true)) = explorer.drag.take() {
                released_drag = true;
                if let Some(at) = ui.pointer()
                    && !rect.contains(at)
                {
                    dropped = Some((path, at));
                }
            }
            explorer.drag = None;
        }
    }
    let mut acted = (selected != before && !keyed && !jumped) || response.activated;
    // a press picks the row now and opens it on release, so pulling it
    // away to a pane doesn't open it here first
    if acted && held && !response.activated {
        explorer.click_later = selected;
        acted = false;
    } else if !held
        && let Some(row) = explorer.click_later.take()
        && !released_drag
        && dropped.is_none()
    {
        selected = Some(row);
        acted = true;
    }
    let menu = ui
        .secondary_at(Id::Explorer, 1)
        .or_else(|| ui.secondary_at(Id::Explorer, 0))
        .map(|at| {
            // the row under the pointer, from the scroll and row height
            let row = ((at.y - rect.y + scroll) / ROW).floor();
            let row = (row >= 0.0)
                .then_some(row as usize)
                .filter(|&r| r < explorer.len());
            if row.is_some() {
                explorer.selected = row;
            }
            (at, row)
        });
    Ok(Shown {
        click: selected
            .filter(|_| acted && dropped.is_none() && dragging.is_none())
            .and_then(|row| explorer.click(row)),
        menu,
        dragging,
        dropped,
    })
}

/// paths alike as the file system sees them: windows ignores case.
fn same_path(a: &Path, b: &Path) -> bool {
    if cfg!(windows) {
        a.as_os_str().eq_ignore_ascii_case(b.as_os_str())
    } else {
        a == b
    }
}

/// small arrow: right when closed, down when open.
fn chevron(
    ui: &mut Ui<'_, Id>,
    rect: Rect,
    open: bool,
    color: aurea::render::Color,
) -> AureaResult<()> {
    let c = rect.center();
    let points = if open {
        [
            Point::new(c.x - 3.5, c.y - 1.5),
            Point::new(c.x, c.y + 2.0),
            Point::new(c.x + 3.5, c.y - 1.5),
        ]
    } else {
        [
            Point::new(c.x - 1.5, c.y - 3.5),
            Point::new(c.x + 2.0, c.y),
            Point::new(c.x - 1.5, c.y + 3.5),
        ]
    };
    ui.polyline(&points, 1.5, color)
}
