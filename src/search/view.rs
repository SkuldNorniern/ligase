use std::path::Path;

use aurea::render::Rect;
use aurea::ui::{Align, Button, ListView, TextField, Ui};
use aurea::{AureaResult, KeyCode};

use super::{Finder, Hit};
use crate::app::Id;
use crate::lang::Texts;

const FIELD: f32 = 28.0;
const NOTE: f32 = 22.0;
const ROW: f32 = 22.0;
const PAD: f32 = 8.0;

/// what the panel asks the app to do.
pub enum Ask {
    Search,
    Open(Hit),
    /// every hit's file opened, the hits replaced there, left unsaved.
    ReplaceAll,
}

pub fn show(
    ui: &mut Ui<'_, Id>,
    finder: &mut Finder,
    root: &Path,
    rect: Rect,
    texts: &Texts,
) -> AureaResult<Option<Ask>> {
    let palette = ui.theme.palette;
    let font = ui.theme.font(13.0);
    let small = ui.theme.font(12.0);
    let inner = rect.inset(PAD);
    let (field, rest) = inner.split_top(FIELD, 6.0);
    let (replace_row, rest) = rest.split_top(FIELD, 6.0);
    let (note, list) = rest.split_top(NOTE, 2.0);

    let mut ask = None;
    let typed = TextField::new(&mut finder.query)
        .placeholder(texts.tr("Search in files"))
        .max_chars(500)
        .show(ui, Id::SearchField, field)?;
    if typed.submitted {
        ask = Some(Ask::Search);
    }
    let (button, replace_field) = replace_row.split_right(86.0, 6.0);
    TextField::new(&mut finder.replace)
        .placeholder(texts.tr("replace with"))
        .max_chars(500)
        .show(ui, Id::ReplaceInFiles, replace_field)?;
    let can_replace = !finder.hits.is_empty() && !finder.searching();
    if Button::new(texts.tr("Replace All"))
        .enabled(can_replace)
        .show(ui, Id::ReplaceAllButton, button)?
        .clicked
        && can_replace
    {
        ask = Some(Ask::ReplaceAll);
    }

    let status = if finder.searching() {
        format!("{} ({})", texts.tr("Searching…"), finder.hits.len())
    } else if let Some((files, capped)) = finder.done {
        let mut line = format!(
            "{} {} / {} {}",
            finder.hits.len(),
            texts.tr("hits"),
            files,
            texts.tr("files")
        );
        if capped {
            line.push_str(&format!(" ({})", texts.tr("Limit reached")));
        }
        line
    } else {
        String::new()
    };
    ui.text_clipped(&status, note, &small, palette.dim, Align::Start)?;

    let before = finder.selected;
    let mut selected = finder.selected;
    let mut scroll = finder.scroll;
    let hits = &finder.hits;
    let response = ListView::new(&mut scroll, hits.len(), ROW)
        .selected(&mut selected)
        .show(ui, Id::SearchList, list, |ui, row| {
            let Some(hit) = hits.get(row.index) else {
                return Ok(());
            };
            if row.picked {
                ui.fill(row.rect, palette.selection)?;
            } else if row.hovered {
                ui.fill(row.rect, palette.hover)?;
            }
            let shown = hit.path.strip_prefix(root).unwrap_or(&hit.path);
            let place = format!("{}:{}", shown.display(), hit.line + 1);
            let (left, right) = row.rect.inset_xy(4.0, 0.0).split_left(
                ui.text_width(&place, &small).min(row.rect.width * 0.45),
                10.0,
            );
            ui.text_clipped(&place, left, &small, palette.dim, Align::Start)?;
            ui.text_clipped(&hit.preview, right, &font, palette.text, Align::Start)?;
            Ok(())
        })?;
    finder.scroll = scroll;
    finder.selected = selected;
    // like the explorer: a mouse pick opens, arrow keys only move
    let keyed = ui.keys().any(|(key, _)| {
        matches!(
            key,
            KeyCode::Up | KeyCode::Down | KeyCode::PageUp | KeyCode::PageDown
        )
    });
    if ((selected != before && !keyed) || response.activated)
        && let Some(hit) = selected.and_then(|i| finder.hits.get(i))
    {
        ask = Some(Ask::Open(hit.clone()));
    }
    Ok(ask)
}
