use std::collections::HashMap;
use std::path::Path;

use aurea::render::{Color, Font, Rect};
use aurea::ui::{Align, Button, ListView, TextField, Ui};
use aurea::{AureaResult, KeyCode};

use super::{Finder, Hit, matcher};
use crate::app::Id;
use crate::icons;
use crate::lang::Texts;

const FIELD: f32 = 28.0;
const NOTE: f32 = 22.0;
const ROW: f32 = 24.0;
const PAD: f32 = 8.0;

/// what the panel asks the app to do.
pub enum Ask {
    Search,
    Open(Hit),
    /// every hit's file opened, the hits replaced there, left unsaved.
    ReplaceAll,
}

/// one file's hits, as indices into the hit list, in the order they came.
struct Group<'a> {
    path: &'a Path,
    hits: Vec<usize>,
}

/// one list row: a file's header, or a hit.
#[derive(Clone, Copy)]
enum Line {
    Head(usize),
    Hit(usize),
}

/// the hits grouped by file, files in the order they first came.
fn group(hits: &[Hit]) -> Vec<Group<'_>> {
    let mut groups: Vec<Group<'_>> = Vec::new();
    let mut at: HashMap<&Path, usize> = HashMap::new();
    for (i, hit) in hits.iter().enumerate() {
        let path = hit.path.as_path();
        let g = match at.get(path) {
            Some(&g) => g,
            None => {
                groups.push(Group {
                    path,
                    hits: Vec::new(),
                });
                at.insert(path, groups.len() - 1);
                groups.len() - 1
            }
        };
        groups[g].hits.push(i);
    }
    groups
}

/// the list rows: each file's header, then its hits.
fn lines(groups: &[Group<'_>]) -> Vec<Line> {
    let mut lines = Vec::new();
    for (g, group) in groups.iter().enumerate() {
        lines.push(Line::Head(g));
        lines.extend(group.hits.iter().map(|&h| Line::Hit(h)));
    }
    lines
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

    // a quiet line: the count, the files, and whether the cap stopped it
    let status = if finder.searching() {
        format!("{} ({})", texts.tr("Searching…"), finder.hits.len())
    } else if let Some((files, capped)) = finder.done {
        let mut line = format!(
            "{} {} · {} {}",
            finder.hits.len(),
            texts.tr("hits"),
            files,
            texts.tr("files")
        );
        if capped {
            line.push_str(&format!(" · {}", texts.tr("Limit reached")));
        }
        line
    } else {
        String::new()
    };
    ui.text_clipped(&status, note, &small, palette.dim, Align::Start)?;

    let hits = &finder.hits;
    let groups = group(hits);
    let lines = lines(&groups);
    let find = matcher(&finder.query);
    // line numbers share one column, as wide as the widest
    let widest = hits.iter().map(|h| h.line + 1).max().unwrap_or(1);
    let number_w = ui.text_width(&widest.to_string(), &small);

    // the pick is a hit; its row in the list is looked up each frame
    let before = finder.selected.and_then(|h| {
        lines
            .iter()
            .position(|l| matches!(l, Line::Hit(i) if *i == h))
    });
    let mut picked = before;
    let mut scroll = finder.scroll;
    let response = ListView::new(&mut scroll, lines.len(), ROW)
        .selected(&mut picked)
        .show(ui, Id::SearchList, list, |ui, row| {
            let Some(line) = lines.get(row.index).copied() else {
                return Ok(());
            };
            match line {
                Line::Head(g) => {
                    let Some(group) = groups.get(g) else {
                        return Ok(());
                    };
                    let count = group.hits.len().to_string();
                    let pill = pill_rect(ui, row.rect, &count, &small);
                    ui.round(pill, (ROW - 8.0) / 2.0, palette.raised)?;
                    ui.text(&count, pill, &small, palette.dim, Align::Center)?;
                    let icon_at = Rect::new(
                        row.rect.x + 6.0,
                        row.rect.y + (ROW - 16.0) / 2.0,
                        16.0,
                        16.0,
                    );
                    let name = icons::for_path(group.path, false, false);
                    icons::draw(ui, name, icon_at, palette.dim)?;
                    let shown = group.path.strip_prefix(root).unwrap_or(group.path);
                    let title = shown
                        .file_name()
                        .map(|n| n.to_string_lossy())
                        .unwrap_or_default();
                    let dir = shown
                        .parent()
                        .map(|p| p.to_string_lossy())
                        .unwrap_or_default();
                    let body = Rect::new(
                        icon_at.right() + 6.0,
                        row.rect.y,
                        (pill.x - 8.0 - icon_at.right() - 6.0).max(0.0),
                        ROW,
                    );
                    name_and_folder(ui, body, &title, &dir, &font, &small, palette.text)?;
                }
                Line::Hit(h) => {
                    let Some(hit) = hits.get(h) else {
                        return Ok(());
                    };
                    let lit = row.rect.inset_xy(2.0, 1.0);
                    if row.picked {
                        ui.round(lit, 6.0, palette.selection)?;
                    } else if row.hovered {
                        ui.round(lit, 6.0, palette.hover)?;
                    }
                    // indented under the file's icon: line number, then the line
                    let num_x = row.rect.x + 30.0;
                    let number = (hit.line + 1).to_string();
                    let num_at = Rect::new(num_x, row.rect.y, number_w, ROW);
                    ui.text(&number, num_at, &small, palette.dim, Align::End)?;
                    let text_x = num_x + number_w + 10.0;
                    let body = Rect::new(
                        text_x,
                        row.rect.y,
                        (row.rect.right() - 8.0 - text_x).max(0.0),
                        ROW,
                    );
                    // the match, on a soft accent plate behind the text
                    if let Some((at, len)) = find(&hit.preview) {
                        let before_w = ui.text_width(&hit.preview[..at], &font);
                        let match_w = ui.text_width(&hit.preview[at..at + len], &font);
                        let left = (body.x + before_w).min(body.right());
                        let plate_w = match_w.min(body.right() - left).max(0.0);
                        if plate_w > 0.0 {
                            let plate = Rect::new(left, body.y + 3.0, plate_w, ROW - 6.0);
                            ui.round(plate, 4.0, palette.accent.fade(0.22))?;
                        }
                    }
                    ui.text_clipped(&hit.preview, body, &font, palette.text, Align::Start)?;
                }
            }
            Ok(())
        })?;
    finder.scroll = scroll;

    // a header is no pick: the list moves on to the hit beside it, the
    // way the pick came
    let on_head = matches!(picked.and_then(|p| lines.get(p)), Some(Line::Head(_)));
    if let (true, Some(p)) = (on_head, picked) {
        let up = before.is_some_and(|b| p < b);
        picked = if up {
            p.checked_sub(1).or(Some(p + 1))
        } else {
            Some(p + 1)
        };
    }
    let picked_hit = match picked.and_then(|p| lines.get(p)) {
        Some(Line::Hit(h)) => Some(*h),
        _ => None,
    };
    finder.selected = match picked_hit {
        Some(h) => Some(h),
        None if picked.is_none() => None,
        None => finder.selected,
    };
    // like the explorer: a mouse pick opens, arrow keys only move
    let keyed = ui.keys().any(|(key, _)| {
        matches!(
            key,
            KeyCode::Up | KeyCode::Down | KeyCode::PageUp | KeyCode::PageDown
        )
    });
    let moved = picked != before && !keyed && !on_head;
    if (moved || (response.activated && !on_head))
        && let Some(hit) = picked_hit.and_then(|h| finder.hits.get(h))
    {
        ask = Some(Ask::Open(hit.clone()));
    }
    Ok(ask)
}

/// a count pill at the right end of `row`, its width fit to `text`.
fn pill_rect(ui: &mut Ui<'_, Id>, row: Rect, text: &str, font: &Font) -> Rect {
    let w = (ui.text_width(text, font) + 12.0).max(ROW - 8.0);
    Rect::new(row.right() - 8.0 - w, row.y + 4.0, w, ROW - 8.0)
}

/// a file's name, then its folder dim after it, both clipped to `body`.
fn name_and_folder(
    ui: &mut Ui<'_, Id>,
    body: Rect,
    name: &str,
    dir: &str,
    font: &Font,
    small: &Font,
    colour: Color,
) -> AureaResult<()> {
    let palette = ui.theme.palette;
    let name_w = ui.text_width(name, font).min(body.width);
    let (name_at, rest) = body.split_left(name_w, 8.0);
    ui.text_clipped(name, name_at, font, colour, Align::Start)?;
    if !dir.is_empty() {
        ui.text_clipped(dir, rest, small, palette.dim, Align::Start)?;
    }
    Ok(())
}
