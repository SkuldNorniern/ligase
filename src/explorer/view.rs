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
    let acted = (selected != before && !keyed) || response.activated;
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
            .filter(|_| acted)
            .and_then(|row| explorer.click(row)),
        menu,
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
