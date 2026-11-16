//! a file against HEAD, one column: lines gone on red, lines new on green,
//! a few alike around each change, longer stretches folded.

use aurea::AureaResult;
use aurea::render::{Font, Rect};
use aurea::ui::{Align, Button, ListView, Ui};

use super::DiffLine;
use crate::app::Id;
use crate::document::DocumentId;
use crate::lang::Texts;

const ROW: f32 = 20.0;
const HEAD: f32 = 32.0;

/// what the diff asks the app to do.
pub enum Ask {
    /// the file, at this line (from 0) of the text now.
    Open(usize),
}

/// one diff tab's place.
#[derive(Default)]
pub struct Shown {
    pub scroll: f32,
    pub picked: Option<usize>,
}

pub fn show(
    ui: &mut Ui<'_, Id>,
    doc: DocumentId,
    name: &str,
    // the rows when worked out, and whether git has the file at all
    (rows, tracked): (Option<&[DiffLine]>, bool),
    shown: &mut Shown,
    rect: Rect,
    (texts, mono): (&Texts, &Font),
) -> AureaResult<Option<Ask>> {
    let palette = ui.theme.palette;
    let small = ui.theme.font(12.0);
    let (head, list) = rect.split_top(HEAD, 0.0);
    ui.fill(head, palette.surface)?;
    ui.fill(
        Rect::new(head.x, head.bottom() - 1.0, head.width, 1.0),
        palette.border,
    )?;
    let (button, title) = head.inset_xy(10.0, 4.0).split_right(90.0, 8.0);
    let label = format!("{name}  ·  {}", texts.tr("changes against HEAD"));
    ui.text_clipped(&label, title, &small, palette.dim, Align::Start)?;
    let mut ask = None;
    if Button::new(texts.tr("Open File"))
        .quiet()
        .show(ui, Id::DiffOpen(doc), button)?
        .clicked
    {
        ask = Some(Ask::Open(0));
    }
    let Some(rows) = rows else {
        // a file git doesn't have never gets rows
        let note = if tracked { "loading" } else { "not in git" };
        ui.text(texts.tr(note), list, &small, palette.dim, Align::Center)?;
        return Ok(ask);
    };
    if rows.is_empty() {
        ui.text(
            texts.tr("no changes"),
            list,
            &small,
            palette.dim,
            Align::Center,
        )?;
        return Ok(ask);
    }
    let number_w = ui.text_width("00000", mono);
    let mut picked = shown.picked;
    let response = ListView::new(&mut shown.scroll, rows.len(), ROW)
        .selected(&mut picked)
        .show(ui, Id::DiffList(doc), list, |ui, row| {
            let Some(line) = rows.get(row.index) else {
                return Ok(());
            };
            let (old, new, sign, text, tint) = match line {
                DiffLine::Same(a, b, t) => (Some(*a), Some(*b), " ", t.as_str(), None),
                DiffLine::Removed(a, t) => (Some(*a), None, "-", t.as_str(), Some(palette.danger)),
                DiffLine::Added(b, t) => (None, Some(*b), "+", t.as_str(), Some(palette.success)),
                DiffLine::Gap(n) => {
                    ui.fill(row.rect, palette.surface)?;
                    let note = format!("⋯  {n} {}", texts.tr("lines alike"));
                    ui.text(&note, row.rect, &small, palette.dim, Align::Center)?;
                    return Ok(());
                }
            };
            if let Some(tint) = tint {
                ui.fill(row.rect, tint.fade(0.14))?;
            }
            if row.hovered {
                ui.fill(row.rect, palette.hover.fade(0.6))?;
            }
            let mut x = row.rect.x + 6.0;
            for number in [old, new] {
                let at = Rect::new(x, row.rect.y, number_w, ROW);
                let shown = number.map(|n| (n + 1).to_string()).unwrap_or_default();
                ui.text(&shown, at, mono, palette.faint, Align::End)?;
                x += number_w + 6.0;
            }
            let sign_colour = tint.unwrap_or(palette.faint);
            ui.text(
                sign,
                Rect::new(x, row.rect.y, 14.0, ROW),
                mono,
                sign_colour,
                Align::Center,
            )?;
            x += 18.0;
            let body = Rect::new(x, row.rect.y, (row.rect.right() - x - 6.0).max(0.0), ROW);
            ui.text_clipped(text, body, mono, palette.text, Align::Start)?;
            Ok(())
        })?;
    shown.picked = picked;
    // a click on a line goes to it in the file; a removed one to where it was
    let clicked = response.row_clicked.and_then(|i| {
        let near = rows[..=i.min(rows.len() - 1)]
            .iter()
            .rev()
            .find_map(|r| match r {
                DiffLine::Same(_, b, _) | DiffLine::Added(b, _) => Some(*b),
                _ => None,
            });
        near.or(Some(0))
    });
    Ok(clicked.map(Ask::Open).or(ask))
}
