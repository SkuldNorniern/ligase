//! the settings sheet: a card over the window with a search on top, group
//! chips under it, and one row per setting with its control. a change goes
//! out at once as one line of the file; the file stays the truth.

use std::collections::HashMap;

use aurea::AureaResult;
use aurea::CursorIcon;
use aurea::render::{Point, Rect};
use aurea::ui::{
    Align, Button, Channel, Dropdown, ScrollArea, Segmented, Sense, Stepper, TextField, Toggle, Ui,
};

use super::{FIELDS, Field, FieldKind, Settings, current, quote};
use crate::app::Id;
use crate::lang::Texts;

const GROUPS: [&str; 6] = ["All", "Look", "Editor", "Files", "Layout", "Terminal"];
const ROW: f32 = 58.0;
const CONTROL: f32 = 230.0;
const PAD: f32 = 18.0;
/// pixels one wheel notch moves the list.
const WHEEL: f32 = 48.0;

/// what the sheet asks the app.
#[derive(Clone, Debug, PartialEq)]
pub enum Ask {
    /// write `key` as this toml literal, or unset it.
    Set(&'static str, Option<String>),
    /// the file itself, in an editor.
    OpenFile,
    Close,
}

#[derive(Default)]
pub struct Sheet {
    open: bool,
    pub query: String,
    group: usize,
    scroll: f32,
    /// text fields being typed in, by key; read from the settings on open.
    typing: HashMap<&'static str, String>,
}

impl Sheet {
    pub fn is_open(&self) -> bool {
        self.open
    }

    pub fn open(&mut self, settings: &Settings) {
        self.open = true;
        self.query.clear();
        self.scroll = 0.0;
        self.typing = FIELDS
            .iter()
            .filter(|f| matches!(f.kind, FieldKind::Text))
            .map(|f| {
                (
                    f.key,
                    unquote(&current(settings, f.key).unwrap_or_default()),
                )
            })
            .collect();
    }

    pub fn close(&mut self) {
        self.open = false;
    }

    /// a wheel turn on the list; the list keeps the offset in range.
    pub fn wheel(&mut self, delta_y: f64) {
        self.scroll = (self.scroll - delta_y as f32 * WHEEL).max(0.0);
    }

    /// the rows the search and the chip let through.
    fn shown(&self, texts: &Texts) -> Vec<&'static Field> {
        let query = self.query.trim().to_lowercase();
        let group = GROUPS.get(self.group).copied().unwrap_or("All");
        FIELDS
            .iter()
            .filter(|f| group == "All" || f.group == group)
            .filter(|f| {
                query.is_empty()
                    || [
                        f.key,
                        f.label,
                        f.about,
                        texts.tr(f.label),
                        texts.tr(f.about),
                    ]
                    .iter()
                    .any(|s| s.to_lowercase().contains(&query))
            })
            .collect()
    }
}

fn unquote(literal: &str) -> String {
    literal
        .strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .unwrap_or(literal)
        .to_owned()
}

/// a number as the file would have it: no `.0` on whole ones.
fn number(n: f32, step: f32) -> String {
    if step >= 1.0 || n.fract() == 0.0 {
        format!("{}", n.round() as i64)
    } else {
        let s = format!("{n:.2}");
        s.trim_end_matches('0').trim_end_matches('.').to_owned()
    }
}

pub fn show(
    ui: &mut Ui<'_, Id>,
    sheet: &mut Sheet,
    settings: &Settings,
    area: Rect,
    texts: &Texts,
) -> AureaResult<Option<Ask>> {
    if !sheet.open {
        return Ok(None);
    }
    let colours = ui.theme.palette;
    let shown = ui.animate_from(Id::Settings, 0, Channel::Custom(0), 0.0, 1.0, 16.0);
    // a scrim that eats clicks; one on it closes the sheet
    ui.fill(area, colours.shadow.fade(0.45 * shown))?;
    let mut ask = None;
    if ui
        .interact(Id::Settings, 1, area, Sense::Click, true)
        .clicked
    {
        ask = Some(Ask::Close);
    }
    let width = 780.0_f32.min(area.width - 48.0).max(300.0);
    let height = 640.0_f32.min(area.height - 48.0).max(240.0);
    let card = Rect::new(
        area.x + (area.width - width) / 2.0,
        area.y + (area.height - height) / 2.0 + (1.0 - shown) * 14.0,
        width,
        height,
    );
    ui.shadow(card, 12.0, 28.0, colours.shadow.fade(0.5 * shown))?;
    ui.round(card, 12.0, colours.raised)?;
    ui.border(card, 12.0, 1.0, colours.border)?;
    // the card itself takes clicks so they don't reach the scrim
    ui.interact(Id::Settings, 2, card, Sense::Click, true);

    let inner = card.inset(PAD);
    let (head, rest) = inner.split_top(34.0, 12.0);
    let (chips, list) = rest.split_top(28.0, 12.0);

    // head: title, search, the file, close
    let title_font = ui.theme.font(17.0);
    let (title, head_rest) = head.split_left(110.0, 12.0);
    ui.text(
        texts.tr("Settings"),
        title,
        &title_font,
        colours.text,
        Align::Start,
    )?;
    let (close, head_rest) = head_rest.split_right(34.0, 8.0);
    let (file, search) = head_rest.split_right(150.0, 10.0);
    let typed = TextField::new(&mut sheet.query)
        .placeholder(texts.tr("search settings"))
        .max_chars(100)
        .show(ui, Id::SettingsSearch, search)?;
    if typed.cancelled {
        ask = Some(Ask::Close);
    }
    if typed.changed {
        sheet.scroll = 0.0;
    }
    if Button::new(texts.tr("Open settings file"))
        .quiet()
        .show(ui, Id::SettingsFile, file)?
        .clicked
    {
        ask = Some(Ask::OpenFile);
    }
    let x = ui.interact(Id::SettingsClose, 0, close, Sense::Click, true);
    if x.hovered {
        ui.round(close, 6.0, colours.hover)?;
        ui.set_cursor(CursorIcon::Pointer);
    }
    crate::icons::draw(ui, "close", close.inset(9.0), colours.dim)?;
    if x.clicked {
        ask = Some(Ask::Close);
    }

    // group chips, one lit
    let chip_font = ui.theme.font(12.0);
    let mut cx = chips.x;
    for (i, group) in GROUPS.iter().enumerate() {
        let label = texts.tr(group);
        let w = ui.text_width(label, &chip_font) + 22.0;
        let at = Rect::new(cx, chips.y, w, chips.height);
        cx += w + 6.0;
        if at.right() > chips.right() {
            break;
        }
        let part = u16::try_from(i).unwrap_or(u16::MAX);
        let input = ui.interact(Id::SettingsChip, part, at, Sense::Click, true);
        let lit = sheet.group == i;
        let fill = if lit {
            colours.accent.fade(0.22)
        } else if input.hovered {
            colours.hover
        } else {
            colours.surface
        };
        ui.round(at, at.height / 2.0, fill)?;
        let ink = if lit { colours.text } else { colours.dim };
        ui.text(label, at, &chip_font, ink, Align::Center)?;
        if input.hovered {
            ui.set_cursor(CursorIcon::Pointer);
        }
        if input.clicked {
            sheet.group = i;
            sheet.scroll = 0.0;
        }
    }

    let rows = sheet.shown(texts);
    if rows.is_empty() {
        let small = ui.theme.font(13.0);
        ui.text(
            texts.tr("no setting matches"),
            list,
            &small,
            colours.dim,
            Align::Center,
        )?;
        return Ok(ask);
    }
    let defaults = Settings::default();
    let content = rows.len() as f32 * ROW;
    sheet.scroll = sheet.scroll.min((content - list.height).max(0.0));
    let typing = &mut sheet.typing;
    let (_, asked) = ScrollArea::new(&mut sheet.scroll, content).show(
        ui,
        Id::SettingsList,
        list,
        |ui, content, view| {
            let mut asked = None;
            for (i, field) in rows.iter().enumerate() {
                let at = Rect::new(content.x, content.y + i as f32 * ROW, content.width, ROW);
                if at.bottom() < view.y || at.y > view.bottom() {
                    continue;
                }
                let part = u16::try_from(i).unwrap_or(u16::MAX);
                if let Some(a) = row(ui, field, at, part, (settings, &defaults), typing, texts)? {
                    asked = Some(a);
                }
            }
            Ok(asked)
        },
    )?;
    Ok(asked.or(ask))
}

/// one setting: the label and its line on the left, a dot when it differs
/// from the default (a click puts the default back), the control on the right.
fn row(
    ui: &mut Ui<'_, Id>,
    field: &'static Field,
    at: Rect,
    part: u16,
    (settings, defaults): (&Settings, &Settings),
    typing: &mut HashMap<&'static str, String>,
    texts: &Texts,
) -> AureaResult<Option<Ask>> {
    let colours = ui.theme.palette;
    ui.fill(
        Rect::new(at.x, at.bottom() - 1.0, at.width, 1.0),
        colours.border.fade(0.5),
    )?;
    let now = current(settings, field.key).unwrap_or_default();
    let changed = current(defaults, field.key).is_some_and(|d| d != now);
    let mut ask = None;
    let (control, words) = at.inset_xy(0.0, 10.0).split_right(CONTROL, 16.0);
    let (dot, words) = words.split_left(14.0, 4.0);
    if changed {
        let centre = Point::new(dot.x + 5.0, dot.y + 11.0);
        let hit = Rect::new(dot.x - 2.0, dot.y + 3.0, 16.0, 16.0);
        let input = ui.interact(Id::SettingsReset, part, hit, Sense::Click, true);
        let r = if input.hovered { 5.0 } else { 3.5 };
        ui.circle(centre, r, colours.accent)?;
        if input.hovered {
            ui.set_cursor(CursorIcon::Pointer);
            ui.tooltip_at(
                Point::new(hit.x, hit.bottom()),
                texts.tr("put the default back"),
            );
        }
        if input.clicked {
            ask = Some(Ask::Set(field.key, None));
        }
    }
    let (label_at, about_at) = words.split_top(20.0, 2.0);
    let font = ui.theme.font(13.5);
    let small = ui.theme.font(12.0);
    ui.text_clipped(
        texts.tr(field.label),
        label_at,
        &font,
        colours.text,
        Align::Start,
    )?;
    ui.text_clipped(
        texts.tr(field.about),
        about_at,
        &small,
        colours.dim,
        Align::Start,
    )?;

    let id = Id::SettingsRow(part);
    match field.kind {
        FieldKind::Toggle => {
            let mut on = now == "true";
            let w = 42.0;
            let switch = Rect::new(control.right() - w, control.y + 6.0, w, 24.0);
            if Toggle::new(&mut on).show(ui, id, switch)?.changed {
                ask = Some(Ask::Set(field.key, Some(on.to_string())));
            }
        }
        FieldKind::Choice(words) => {
            let word = unquote(&now);
            let mut picked = words.iter().position(|w| *w == word).unwrap_or(0);
            let before = picked;
            let labels: Vec<&str> = words.iter().map(|w| texts.tr(w)).collect();
            let response = if words.len() <= 3 {
                Segmented::new(&labels, &mut picked).show(ui, id, control.inset_xy(0.0, 3.0))?
            } else {
                Dropdown::new(&labels, &mut picked).show(ui, id, control.inset_xy(0.0, 3.0))?
            };
            if (response.changed || picked != before) && picked != before {
                let word = words[picked];
                let literal = (!(field.key == "language" && word == "system")).then(|| quote(word));
                ask = Some(Ask::Set(field.key, literal));
            }
        }
        FieldKind::Number { min, max, step } => {
            let mut value: f32 = now.parse().unwrap_or(min);
            let before = value;
            let decimals = if step < 1.0 { 2 } else { 0 };
            let w = 150.0;
            let at = Rect::new(
                control.right() - w,
                control.y + 3.0,
                w,
                control.height - 6.0,
            );
            Stepper::new(&mut value, min..=max)
                .step(step)
                .decimals(decimals)
                .show(ui, id, at)?;
            if (value - before).abs() > f32::EPSILON {
                ask = Some(Ask::Set(field.key, Some(number(value, step))));
            }
        }
        FieldKind::Text => {
            let text = typing.entry(field.key).or_default();
            let typed = TextField::new(text)
                .placeholder(texts.tr("default"))
                .max_chars(100)
                .show(ui, id, control.inset_xy(0.0, 3.0))?;
            if typed.submitted || typed.focus_lost {
                let trimmed = text.trim();
                let literal = (!trimmed.is_empty()).then(|| quote(trimmed));
                if literal.as_deref() != Some(now.as_str()) && !(literal.is_none() && now == "\"\"")
                {
                    ask = Some(Ask::Set(field.key, literal));
                }
            }
        }
    }
    Ok(ask)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_are_written_plain() {
        assert_eq!(number(14.0, 1.0), "14");
        assert_eq!(number(1.5, 0.05), "1.5");
        assert_eq!(number(1.45, 0.05), "1.45");
        assert_eq!(quote("a\"b"), "\"a\\\"b\"");
        assert_eq!(unquote("\"dark\""), "dark");
    }
}
