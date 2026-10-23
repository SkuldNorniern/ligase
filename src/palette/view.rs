//! the palette card: a field and the rows under it.

use aurea::AureaResult;
use aurea::render::{Color, Font, Rect};
use aurea::ui::{Align, Channel, Sense, TextField, Ui};

use super::{Kind, Palette};
use crate::app::Id;
use crate::lang::Texts;

const FIELD: f32 = 32.0;
const ROW: f32 = 26.0;
pub const ROWS: usize = 12;
const PAD: f32 = 8.0;

/// what the card asks the app.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ask {
    /// the picked row, or the `:line` in the field.
    Choose,
    Close,
}

/// draws `text` from `x`, the chars at `marks` lit, the rest in `plain`
/// up to `split` and `rest` after it.
#[allow(clippy::too_many_arguments)]
fn marked(
    ui: &mut Ui<'_, Id>,
    text: &str,
    marks: &[usize],
    split: usize,
    rect: Rect,
    font: &Font,
    (plain, rest, lit): (Color, Color, Color),
) -> AureaResult<()> {
    let colour = |i: usize| {
        if marks.binary_search(&i).is_ok() {
            lit
        } else if i < split {
            plain
        } else {
            rest
        }
    };
    // runs of one colour, drawn one after another
    let mut runs: Vec<(usize, usize, Color)> = Vec::new();
    for (i, _) in text.char_indices() {
        let c = colour(i);
        match runs.last_mut() {
            Some(last) if last.2 == c => last.1 = i,
            _ => runs.push((i, i, c)),
        }
    }
    let mut x = rect.x;
    for (n, &(from, _, c)) in runs.iter().enumerate() {
        let to = runs.get(n + 1).map_or(text.len(), |r| r.0);
        if x > rect.x + rect.width {
            break;
        }
        let run = &text[from..to];
        let at = Rect::new(x, rect.y, (rect.x + rect.width - x).max(0.0), rect.height);
        ui.text_clipped(run, at, font, c, Align::Start)?;
        x += ui.text_width(run, font);
    }
    Ok(())
}

pub fn show(
    ui: &mut Ui<'_, Id>,
    palette: &mut Palette,
    area: Rect,
    texts: &Texts,
) -> AureaResult<Option<Ask>> {
    if !palette.is_open() {
        return Ok(None);
    }
    palette.refresh();
    let colours = ui.theme.palette;
    let font = ui.theme.font(13.0);
    let small = ui.theme.font(12.0);
    let rows = palette.shown.len().min(ROWS);
    let width = 620.0_f32.min(area.width - 32.0).max(240.0);
    let height = FIELD
        + 2.0 * PAD
        + if rows > 0 {
            rows as f32 * ROW + PAD
        } else {
            0.0
        };
    if std::mem::take(&mut palette.opened) {
        ui.set_animated(Id::Palette, 0, Channel::Custom(0), 0.0);
    }
    let shown = ui.animate(Id::Palette, 0, Channel::Custom(0), 1.0, 18.0);
    let card = Rect::new(
        area.x + (area.width - width) / 2.0,
        area.y + 8.0 - (1.0 - shown) * 12.0,
        width,
        height,
    );
    // a shade behind it, darker as it comes
    ui.round(card.inset(-3.0), 9.0, colours.shadow.fade(0.35 * shown))?;
    ui.round(card, 8.0, colours.raised.fade(shown))?;
    ui.border(card, 8.0, 1.0, colours.border.fade(shown))?;
    let inner = card.inset(PAD);
    let (field, list) = inner.split_top(FIELD, PAD);
    let hint = match palette.kind {
        Some(Kind::Commands) => texts.tr("type a command"),
        _ => texts.tr("file name, > for commands, : for a line"),
    };
    let typed = TextField::new(&mut palette.query)
        .placeholder(hint)
        .max_chars(300)
        .show(ui, Id::PaletteField, field)?;
    let mut ask = None;
    if typed.submitted {
        ask = Some(Ask::Choose);
    }
    if typed.cancelled || typed.focus_lost {
        ask = Some(Ask::Close);
    }
    if rows == 0 {
        if palette.listing && palette.wanted().0 == Kind::Files {
            let note = Rect::new(field.x, field.y + FIELD, field.width, ROW);
            ui.text(
                texts.tr("listing files"),
                note,
                &small,
                colours.dim,
                Align::Start,
            )?;
        }
        return Ok(ask);
    }
    // the pick's light glides from row to row
    let target = (palette.pick - palette.top) as f32 * ROW;
    let y = ui.animate(Id::Palette, 1, Channel::Custom(1), target, 30.0);
    let light = Rect::new(
        list.x,
        list.y + y.clamp(0.0, (rows - 1) as f32 * ROW),
        list.width,
        ROW,
    );
    ui.round(light, 5.0, colours.selection.fade(shown))?;
    let (wanted, _) = palette.wanted();
    let lit = colours.accent;
    let wheel = ui.wheel().1;
    if wheel != 0.0 && ui.pointer().is_some_and(|p| list.contains(p)) {
        let last = palette.shown.len().saturating_sub(ROWS);
        let by = if wheel > 0.0 { -3 } else { 3 };
        palette.top = palette.top.saturating_add_signed(by).min(last);
        ui.consume_wheel((0.0, wheel));
    }
    for r in 0..rows {
        let index = palette.top + r;
        let Some(row) = palette.shown.get(index) else {
            break;
        };
        let rect = Rect::new(list.x, list.y + r as f32 * ROW, list.width, ROW);
        let hit = ui.interact(Id::PaletteRow(r as u16), 0, rect, Sense::Click, true);
        if hit.hovered && index != palette.pick {
            ui.round(rect, 5.0, colours.hover.fade(0.5))?;
        }
        if ui.press(Id::PaletteRow(r as u16), 0).is_some() {
            palette.pick = index;
            ask = Some(Ask::Choose);
        }
        let item = &palette.items(wanted)[row.item];
        let body = rect.inset_xy(10.0, 0.0);
        let (note, body) = body.split_right(140.0, 8.0);
        let split = match wanted {
            // the folder part of a path is dim
            Kind::Files => item.text.rfind('/').map_or(0, |i| i + 1),
            Kind::Commands => item.text.len(),
        };
        let (plain, rest) = if wanted == Kind::Files {
            (colours.dim, colours.text)
        } else {
            (colours.text, colours.text)
        };
        marked(
            ui,
            &item.text,
            &row.marks,
            split,
            body,
            &font,
            (plain, rest, lit),
        )?;
        ui.text(&item.note, note, &small, colours.faint, Align::End)?;
    }
    Ok(ask)
}
