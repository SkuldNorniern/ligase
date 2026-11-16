//! the rail at the far left. side bar panels sit at the top, the panels
//! under the editors and settings at the bottom, so one strip opens every
//! tool. a click on the lit one folds it away.

use aurea::render::{Color, Rect};
use aurea::ui::{Align, Response, Sense, Ui};
use aurea::{AureaResult, CursorIcon, Keymap};

use crate::app::{Command, Id};
use crate::lang::Texts;

pub const WIDTH: f32 = 46.0;
const CELL: f32 = 44.0;
const ICON: f32 = 20.0;
/// height of the side bar's title row.
pub const HEAD: f32 = 32.0;

/// one button on the rail.
pub struct Item {
    pub icon: &'static str,
    /// english, the key for the tooltip text.
    pub label: &'static str,
    pub command: Command,
    /// its panel is in sight.
    pub lit: bool,
    /// a count on the corner, none at 0.
    pub badge: usize,
}

/// draws the rail. gives the command of a clicked button.
pub fn show(
    ui: &mut Ui<'_, Id>,
    rect: Rect,
    (top, bottom): (&[Item], &[Item]),
    texts: &Texts,
    keymap: &Keymap<Command>,
) -> AureaResult<Option<Command>> {
    let palette = ui.theme.palette;
    ui.fill(rect, palette.surface)?;
    ui.fill(
        Rect::new(rect.right() - 1.0, rect.y, 1.0, rect.height),
        palette.border,
    )?;
    let mut picked = None;
    let width = rect.width - 1.0;
    let bottom_y = rect.bottom() - 6.0 - bottom.len() as f32 * CELL;
    let at_top = (0..top.len()).map(|i| rect.y + 6.0 + i as f32 * CELL);
    let at_bottom = (0..bottom.len()).map(|i| bottom_y + i as f32 * CELL);
    for (part, (item, y)) in top
        .iter()
        .zip(at_top)
        .chain(bottom.iter().zip(at_bottom))
        .enumerate()
    {
        let at = Rect::new(rect.x, y, width, CELL);
        if y + CELL > rect.bottom() {
            continue;
        }
        let part = u16::try_from(part).unwrap_or(u16::MAX);
        if cell(ui, at, item, part, (texts, keymap))? {
            picked = Some(item.command);
        }
    }
    Ok(picked)
}

fn cell(
    ui: &mut Ui<'_, Id>,
    at: Rect,
    item: &Item,
    part: u16,
    (texts, keymap): (&Texts, &Keymap<Command>),
) -> AureaResult<bool> {
    let palette = ui.theme.palette;
    let input = ui.interact(Id::Rail, part, at, Sense::Click, true);
    let eased = ui.eased(Id::Rail, part, input);
    let plate = at.inset_xy(5.0, 3.0);
    if eased.hover > 0.0 || eased.press > 0.0 {
        let tint = palette.hover.mix(palette.pressed, eased.press);
        ui.round(plate, 7.0, tint.fade(eased.hover.max(eased.press)))?;
    }
    if item.lit {
        ui.round(plate, 7.0, palette.accent.fade(0.14))?;
        ui.fill(
            Rect::new(at.x, at.y + 10.0, 2.0, at.height - 20.0),
            palette.accent,
        )?;
    }
    let colour = if item.lit {
        palette.text
    } else {
        palette.dim.mix(palette.text, eased.hover)
    };
    let icon = Rect::new(
        at.x + (at.width - ICON) / 2.0,
        at.y + (at.height - ICON) / 2.0,
        ICON,
        ICON,
    );
    crate::icons::draw(ui, item.icon, icon, colour)?;
    if item.badge > 0 {
        badge(ui, icon, item.badge, palette.accent, palette.on_accent)?;
    }
    let label = texts.tr(item.label);
    let tip = match keymap.chord_for(&item.command) {
        Some(chord) => format!("{label}  {}", chord.label()),
        None => label.to_owned(),
    };
    ui.tooltip(&Response::new(at, input), &tip);
    if input.hovered {
        ui.set_cursor(CursorIcon::Pointer);
    }
    Ok(input.clicked)
}

/// a count in a pill on the icon's top right corner.
fn badge(ui: &mut Ui<'_, Id>, icon: Rect, n: usize, fill: Color, ink: Color) -> AureaResult<()> {
    let text = if n > 99 {
        "99+".to_owned()
    } else {
        n.to_string()
    };
    let font = ui.theme.font(9.5);
    let w = (ui.text_width(&text, &font) + 8.0).max(15.0);
    let pill = Rect::new(icon.right() - w / 2.0 + 2.0, icon.y - 5.0, w, 15.0);
    ui.round(pill.inset_xy(-1.5, -1.5), 9.0, ui.theme.palette.surface)?;
    ui.round(pill, 7.5, fill)?;
    ui.text(&text, pill, &font, ink, Align::Center)
}

/// the side bar's title row: the panel name small and caps, then what it
/// is about (the folder, the branch) in plain text.
pub fn head(ui: &mut Ui<'_, Id>, rect: Rect, title: &str, about: &str) -> AureaResult<()> {
    let palette = ui.theme.palette;
    ui.fill(
        Rect::new(rect.x, rect.bottom() - 1.0, rect.width, 1.0),
        palette.border,
    )?;
    let inner = rect.inset_xy(14.0, 0.0);
    let small = ui.theme.font(11.0);
    let caps = title.to_uppercase();
    let caps_w = ui.text_width(&caps, &small).min(inner.width);
    let (title_at, rest) = inner.split_left(caps_w, 10.0);
    ui.text_clipped(&caps, title_at, &small, palette.dim, Align::Start)?;
    if !about.is_empty() {
        let font = ui.theme.font(12.5);
        ui.text_clipped(about, rest, &font, palette.text, Align::Start)?;
    }
    Ok(())
}
