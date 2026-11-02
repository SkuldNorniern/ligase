//! the popup under the caret.

use aurea::AureaResult;
use aurea::render::{Color, Font, Rect};
use aurea::ui::{Align, Channel, Palette, Sense, Ui};

use super::Popup;
use crate::app::Id;
use crate::lsp::complete::Kind;

const ROW: f32 = 22.0;
pub const ROWS: usize = 10;
const WIDTH: f32 = 420.0;
const PAD: f32 = 4.0;

fn tag_colour(palette: &Palette, kind: Kind) -> Color {
    match kind {
        Kind::Function => palette.accent,
        Kind::Type | Kind::Module => palette.success,
        Kind::Field | Kind::Variable | Kind::Constant => palette.warning,
        Kind::Keyword | Kind::Snippet | Kind::Other => palette.dim,
    }
}

/// the label, lit chars in `lit`.
fn label(
    ui: &mut Ui<'_, Id>,
    text: &str,
    marks: &[usize],
    rect: Rect,
    font: &Font,
    (plain, lit): (Color, Color),
) -> AureaResult<f32> {
    let mut x = rect.x;
    let mut run_start = 0;
    let mut run_lit = None;
    let flush = |ui: &mut Ui<'_, Id>, from: usize, to: usize, lit_run: bool, x: &mut f32| {
        let run = &text[from..to];
        let at = Rect::new(*x, rect.y, (rect.x + rect.width - *x).max(0.0), rect.height);
        let colour = if lit_run { lit } else { plain };
        let r = ui.text_clipped(run, at, font, colour, Align::Start);
        *x += ui.text_width(run, font);
        r
    };
    for (i, _) in text.char_indices() {
        let is_lit = marks.binary_search(&i).is_ok();
        match run_lit {
            Some(l) if l != is_lit => {
                flush(ui, run_start, i, l, &mut x)?;
                run_start = i;
                run_lit = Some(is_lit);
            }
            None => run_lit = Some(is_lit),
            _ => {}
        }
    }
    if let Some(l) = run_lit {
        flush(ui, run_start, text.len(), l, &mut x)?;
    }
    Ok(x - rect.x)
}

/// draws the popup under `caret` (or over it near the bottom of `area`).
/// gives the row clicked.
pub fn show(
    ui: &mut Ui<'_, Id>,
    popup: &mut Popup,
    caret: Rect,
    area: Rect,
) -> AureaResult<Option<usize>> {
    let rows = popup.shown.len().min(ROWS);
    if rows == 0 {
        return Ok(None);
    }
    let palette = ui.theme.palette;
    let font = ui.theme.font(13.0);
    let small = ui.theme.font(11.0);
    let height = rows as f32 * ROW + 2.0 * PAD;
    let width = WIDTH.min(area.width - 16.0).max(160.0);
    let below = caret.y + caret.height + 2.0;
    let y = if below + height <= area.y + area.height {
        below
    } else {
        (caret.y - height - 2.0).max(area.y)
    };
    let x = caret.x.min(area.x + area.width - width).max(area.x);
    let shown = ui.animate_from(Id::Completion, 0, Channel::Custom(0), 0.0, 1.0, 22.0);
    let card = Rect::new(x, y + (1.0 - shown) * 4.0, width, height);
    ui.round(card.inset(-2.0), 7.0, palette.shadow.fade(0.3 * shown))?;
    ui.round(card, 6.0, palette.raised.fade(shown))?;
    ui.border(card, 6.0, 1.0, palette.border.fade(shown))?;
    let list = card.inset(PAD);
    let target = popup.pick.saturating_sub(popup.top) as f32 * ROW;
    let light_y = ui.animate(Id::Completion, 1, Channel::Custom(1), target, 34.0);
    let light = Rect::new(
        list.x,
        list.y + light_y.clamp(0.0, (rows - 1) as f32 * ROW),
        list.width,
        ROW,
    );
    ui.round(light, 4.0, palette.selection.fade(shown))?;
    let wheel = ui.wheel().1;
    if wheel != 0.0 && ui.pointer().is_some_and(|p| card.contains(p)) {
        let last = popup.shown.len().saturating_sub(ROWS);
        let by: isize = if wheel > 0.0 { -2 } else { 2 };
        popup.top = popup.top.saturating_add_signed(by).min(last);
        popup.pick = popup
            .pick
            .clamp(popup.top, popup.top + ROWS - 1)
            .min(popup.shown.len() - 1);
        ui.consume_wheel((0.0, wheel));
    }
    let mut clicked = None;
    for r in 0..rows {
        let index = popup.top + r;
        let Some(row) = popup.shown.get(index) else {
            break;
        };
        let rect = Rect::new(list.x, list.y + r as f32 * ROW, list.width, ROW);
        ui.interact(Id::CompletionRow(r as u16), 0, rect, Sense::Click, true);
        if ui.press(Id::CompletionRow(r as u16), 0).is_some() {
            clicked = Some(index);
        }
        let item = &popup.items[row.item];
        let (tag, rest) = rect.inset_xy(6.0, 0.0).split_left(22.0, 6.0);
        let colour = tag_colour(&palette, item.kind);
        ui.text(
            item.kind.tag(),
            tag,
            &small,
            colour.fade(shown),
            Align::Center,
        )?;
        let used = label(
            ui,
            &item.label,
            &row.marks,
            rest,
            &font,
            (palette.text.fade(shown), palette.accent.fade(shown)),
        )?;
        // the detail takes what room the label leaves
        let room = rest.width - used - 12.0;
        if room > 40.0 && !item.detail.is_empty() {
            let at = Rect::new(rest.x + used + 12.0, rest.y, room, ROW);
            ui.text_clipped(
                &item.detail,
                at,
                &small,
                palette.faint.fade(shown),
                Align::End,
            )?;
        }
    }
    Ok(clicked)
}
