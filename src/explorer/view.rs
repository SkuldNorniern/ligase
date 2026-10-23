use aurea::render::{Point, Rect};
use aurea::ui::{Align, ListView, Ui};
use aurea::{AureaResult, KeyCode};

use super::{Click, Explorer};
use crate::app::Id;

const ROW: f32 = 22.0;
const INDENT: f32 = 14.0;

/// draws the tree. a click on a row comes back for the app to act on.
pub fn show(
    ui: &mut Ui<'_, Id>,
    explorer: &mut Explorer,
    rect: Rect,
) -> AureaResult<Option<Click>> {
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
            if row.picked {
                ui.fill(row.rect, palette.selection)?;
            } else if row.hovered {
                ui.fill(row.rect, palette.hover)?;
            }
            let indent = 8.0 + f32::from(entry.depth) * INDENT;
            let (mark, name) = row.rect.split_left(indent + 12.0, 4.0);
            if entry.dir {
                chevron(ui, mark.split_right(12.0, 0.0).0, entry.open, palette.dim)?;
            }
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
    Ok(selected
        .filter(|_| acted)
        .and_then(|row| explorer.click(row)))
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
