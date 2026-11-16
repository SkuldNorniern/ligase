//! the source control panel: a commit message, the commit button, then the
//! staged files and the changed ones. a click opens a file; the button on a
//! row stages or unstages it.

use std::path::PathBuf;

use aurea::AureaResult;
use aurea::render::Rect;
use aurea::ui::{Align, Button, ListView, TextField, Ui};

use super::{Change, Status};
use crate::app::Id;
use crate::icons;
use crate::lang::Texts;

const FIELD: f32 = 28.0;
const ROW: f32 = 22.0;
const PAD: f32 = 8.0;

/// what the panel asks the app to do.
pub enum Ask {
    Open(PathBuf),
    Stage(Vec<String>),
    Unstage(Vec<String>),
    Commit,
}

/// the panel's own state.
#[derive(Default)]
pub struct Scm {
    pub message: String,
    scroll: f32,
    picked: Option<usize>,
}

enum Line<'a> {
    Head(&'static str, usize),
    File(&'a Change, bool),
}

fn letter_colour(ui: &Ui<'_, Id>, letter: char) -> aurea::render::Color {
    let palette = ui.theme.palette;
    match letter {
        'A' | '?' => palette.success,
        'D' => palette.danger,
        'U' => palette.warning,
        _ => palette.accent,
    }
}

pub fn show(
    ui: &mut Ui<'_, Id>,
    scm: &mut Scm,
    status: Option<&Status>,
    rect: Rect,
    texts: &Texts,
) -> AureaResult<Option<Ask>> {
    let palette = ui.theme.palette;
    let font = ui.theme.font(13.0);
    let small = ui.theme.font(12.0);
    let inner = rect.inset(PAD);
    let Some(status) = status else {
        let hint = texts.tr("not a git repository");
        ui.text(hint, inner, &small, palette.dim, Align::Center)?;
        return Ok(None);
    };
    let mut ask = None;
    let (field, rest) = inner.split_top(FIELD, 6.0);
    let (button, list) = rest.split_top(FIELD, 8.0);
    let typed = TextField::new(&mut scm.message)
        .placeholder(texts.tr("commit message"))
        .max_chars(2000)
        .show(ui, Id::ScmField, field)?;
    let pressed = Button::new(texts.tr("Commit")).show(ui, Id::ScmCommit, button)?;
    if typed.submitted || pressed.clicked {
        ask = Some(Ask::Commit);
    }

    let staged: Vec<&Change> = status.changes.iter().filter(|c| c.staged()).collect();
    let changed: Vec<&Change> = status.changes.iter().filter(|c| c.unstaged()).collect();
    if staged.is_empty() && changed.is_empty() {
        let note = Rect::new(list.x, list.y + 8.0, list.width, ROW);
        ui.text(
            texts.tr("no changes"),
            note,
            &small,
            palette.dim,
            Align::Center,
        )?;
        return Ok(ask);
    }
    let mut lines: Vec<Line<'_>> = Vec::new();
    if !staged.is_empty() {
        lines.push(Line::Head("Staged Changes", staged.len()));
        lines.extend(staged.iter().map(|c| Line::File(c, true)));
    }
    lines.push(Line::Head("Changes", changed.len()));
    lines.extend(changed.iter().map(|c| Line::File(c, false)));

    let mut picked = scm.picked;
    let mut button_ask = None;
    let response = ListView::new(&mut scm.scroll, lines.len(), ROW)
        .selected(&mut picked)
        .show(ui, Id::ScmList, list, |ui, row| {
            let Some(line) = lines.get(row.index) else {
                return Ok(());
            };
            match line {
                Line::Head(label, n) => {
                    let text = format!("{}  {n}", texts.tr(label));
                    let at = row.rect.inset_xy(4.0, 0.0);
                    ui.text_clipped(&text, at, &small, palette.dim, Align::Start)?;
                }
                Line::File(change, staged) => {
                    let lit = row.rect.inset_xy(2.0, 1.0);
                    if row.picked {
                        ui.round(lit, 4.0, palette.selection)?;
                    } else if row.hovered {
                        ui.round(lit, 4.0, palette.hover)?;
                    }
                    let letter = if *staged { change.index } else { change.tree };
                    let letter = if letter == '?' { 'U' } else { letter };
                    // the file's icon, its name, its folder, then on the right
                    // the stage button (on hover) and git's letter
                    let inner = row.rect.inset_xy(8.0, 0.0);
                    let (icon_at, rest) = inner.split_left(16.0, 6.0);
                    let (rest, mark) = rest.split_right(14.0, 4.0);
                    let (body, button_at) = if row.hovered {
                        rest.split_right(22.0, 4.0)
                    } else {
                        (rest, Rect::new(rest.right(), rest.y, 0.0, 0.0))
                    };
                    let icon_box = Rect::new(icon_at.x, icon_at.y + (ROW - 16.0) / 2.0, 16.0, 16.0);
                    let icon = icons::for_path(&change.path, false, false);
                    icons::draw(ui, icon, icon_box, palette.dim)?;
                    let colour = letter_colour(ui, if change.tree == '?' { '?' } else { letter });
                    ui.text(&letter.to_string(), mark, &small, colour, Align::Center)?;
                    let (dir, name) = match change.rel.rsplit_once('/') {
                        Some((dir, name)) => (dir, name),
                        None => ("", change.rel.as_str()),
                    };
                    // a deleted file reads struck through, in git's colour
                    let name_colour = if letter == 'D' { colour } else { palette.text };
                    let name_w = ui.text_width(name, &font).min(body.width);
                    let (name_at, dir_at) = body.split_left(name_w, 8.0);
                    ui.text_clipped(name, name_at, &font, name_colour, Align::Start)?;
                    if letter == 'D' {
                        let mid = name_at.y + ROW / 2.0;
                        ui.fill(Rect::new(name_at.x, mid, name_w, 1.0), colour)?;
                    }
                    ui.text_clipped(dir, dir_at, &small, palette.dim, Align::Start)?;
                    // stage or unstage, on the row the pointer is on
                    if row.hovered {
                        let at = Rect::new(button_at.x, row.rect.y + 1.0, 22.0, ROW - 2.0);
                        let icon = icons::image(if *staged { "minus" } else { "plus" });
                        let part = u16::try_from(row.index).unwrap_or(u16::MAX);
                        let hit = Button::icon_only(icon).quiet().icon_size(14.0).show(
                            ui,
                            Id::ScmRow(part),
                            at,
                        )?;
                        if hit.clicked {
                            let mut rels = vec![change.rel.clone()];
                            rels.extend(change.old.clone());
                            button_ask = Some(if *staged {
                                Ask::Unstage(rels)
                            } else {
                                Ask::Stage(vec![change.rel.clone()])
                            });
                        }
                    }
                }
            }
            Ok(())
        })?;
    scm.picked = picked;
    if button_ask.is_some() {
        return Ok(button_ask);
    }
    let opened = response
        .row_clicked
        .or(response.activated.then_some(picked).flatten())
        .and_then(|i| match lines.get(i) {
            Some(Line::File(change, _)) if change.tree != 'D' && change.index != 'D' => {
                Some(Ask::Open(change.path.clone()))
            }
            _ => None,
        });
    Ok(opened.or(ask))
}
