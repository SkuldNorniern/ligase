//! the settings sheet: a card over the window with a search on top, group
//! chips under it, and one grouped panel per run of settings. a choice that
//! has a picture shows as cards you can see; the rest keep their control. a
//! change goes out at once as one line of the file; the file stays the truth.

use std::collections::HashMap;

use aurea::AureaResult;
use aurea::CursorIcon;
use aurea::render::{Color, Point, Rect};
use aurea::ui::{
    Align, Button, Channel, Component, Dropdown, FocusRing, Palette, ScrollArea, Segmented, Sense,
    Stepper, TextField, Toggle, Ui,
};

use super::{FIELDS, Field, FieldKind, RailPlace, Settings, current, quote};
use crate::app::Id;
use crate::lang::Texts;

const GROUPS: [&str; 9] = [
    "All",
    "Look",
    "Editor",
    "Files",
    "Layout",
    "Terminal",
    "Agent",
    "Models",
    "Language Servers",
];
/// a row with an inline control.
const ROW: f32 = 58.0;
const CONTROL: f32 = 230.0;
const PAD: f32 = 18.0;
/// pixels one wheel notch moves the list.
const WHEEL: f32 = 48.0;
/// a row's side inset inside its panel.
const INSET: f32 = 12.0;
/// the caption above a panel, when the list shows every group.
const HEAD: f32 = 24.0;
/// space between two panels.
const GAP: f32 = 14.0;

// a card row: title and about, then the cards in lines.
const CARD_W: f32 = 84.0;
const CARD_H: f32 = 58.0;
const CARD_GAP: f32 = 10.0;
const CAP_GAP: f32 = 4.0;
const CAP_H: f32 = 16.0;
const LINE_GAP: f32 = 8.0;
const CARD_TOP: f32 = 10.0;
const TITLE_H: f32 = 18.0;
const ABOUT_H: f32 = 15.0;
/// where the first line of cards starts, from the top of the row.
const CARDS_AT: f32 = 53.0;
const CARD_BOTTOM: f32 = 10.0;
/// the first part of a card's id; a row's card parts are 16 apart.
const CARD_PARTS: u16 = 0x4000;

/// what the sheet asks the app.
#[derive(Clone, Debug, PartialEq)]
pub enum Ask {
    /// write `key` as this toml literal, or unset it.
    Set(&'static str, Option<String>),
    /// the file itself, in an editor.
    OpenFile,
    /// write `language`'s server table, or take it out.
    Server(String, Option<crate::settings::ServerRule>),
    /// the custom agent uses this profile from now on.
    UseModel(String),
    /// edit this profile in the agent panel; none for a new one.
    EditModel(Option<String>),
    DeleteModel(String),
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
    /// the language servers, as the list edits them.
    pub servers: Vec<ServerRow>,
    /// the language typed to add a server for.
    adding: String,
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
    fn shown(&self, texts: &Texts, settings: &Settings) -> Vec<&'static Field> {
        let query = self.query.trim().to_lowercase();
        let group = GROUPS.get(self.group).copied().unwrap_or("All");
        let custom = settings.agent == crate::agent::Agent::OpenAi;
        FIELDS
            .iter()
            // a program for an outside agent means nothing to the custom one
            .filter(|f| !(custom && f.key == "agent_command"))
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

/// the ask that sets `key` to the choice `word`, as the file wants it.
fn pick(key: &'static str, word: &str) -> Ask {
    let literal = (!(key == "language" && word == "system")).then(|| quote(word));
    Ask::Set(key, literal)
}

/// the choices drawn as cards instead of a control.
fn pictured(key: &str) -> bool {
    matches!(
        key,
        "rail" | "rail_show" | "side_bar" | "theme" | "caret" | "line_numbers" | "whitespace"
    )
}

/// how many cards fit across `inner` pixels.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn card_columns(inner: f32) -> usize {
    ((inner + CARD_GAP) / (CARD_W + CARD_GAP)).floor().max(1.0) as usize
}

/// the height of a row for a list `width` wide.
#[allow(clippy::cast_precision_loss)]
fn row_height(field: &Field, width: f32) -> f32 {
    match field.kind {
        FieldKind::Choice(words) if pictured(field.key) => {
            let per = card_columns(width - 2.0 * INSET);
            let lines = words.len().div_ceil(per).max(1) as f32;
            CARDS_AT + lines * (CARD_H + CAP_GAP + CAP_H) + (lines - 1.0) * LINE_GAP + CARD_BOTTOM
        }
        _ => ROW,
    }
}

/// a run of rows drawn as one rounded panel, with a caption above it when
/// the list shows every group.
struct Panel {
    title: Option<&'static str>,
    /// where the caption starts, from the top of the list.
    top: f32,
    /// where the panel and its rows start.
    body: f32,
    bottom: f32,
}

/// where a row sits in the list, from the top of the list.
struct Slot {
    index: usize,
    y: f32,
    h: f32,
    /// the last row of its panel: no divider under it.
    last: bool,
}

/// the panels and rows for `rows`, and the height of all of them.
fn layout(rows: &[&'static Field], width: f32, titles: bool) -> (Vec<Panel>, Vec<Slot>, f32) {
    let mut panels = Vec::new();
    let mut slots = Vec::new();
    let mut y = 0.0;
    let mut start = 0;
    while start < rows.len() {
        let group = rows[start].group;
        let end = rows[start..]
            .iter()
            .position(|f| f.group != group)
            .map_or(rows.len(), |n| start + n);
        let top = y;
        if titles {
            y += HEAD;
        }
        let body = y;
        for (n, field) in rows[start..end].iter().enumerate() {
            let h = row_height(field, width);
            slots.push(Slot {
                index: start + n,
                y,
                h,
                last: start + n + 1 == end,
            });
            y += h;
        }
        panels.push(Panel {
            title: titles.then_some(group),
            top,
            body,
            bottom: y,
        });
        y += GAP;
        start = end;
    }
    (panels, slots, (y - GAP).max(0.0))
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
    ui.round(card, 12.0, colours.surface)?;
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
    // the file button gives way first on a narrow card
    let file_w = if head_rest.width >= 150.0 + 140.0 {
        150.0
    } else {
        0.0
    };
    let (file, search) = head_rest.split_right(file_w, 10.0);
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
    if file.width > 0.0
        && Button::new(texts.tr("Open Settings File"))
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
    let custom = settings.agent == crate::agent::Agent::OpenAi;
    if !custom && GROUPS.get(sheet.group).copied() == Some("Models") {
        sheet.group = 0;
    }
    for (i, group) in GROUPS.iter().enumerate() {
        // model servers only mean something for the custom agent
        if *group == "Models" && !custom {
            continue;
        }
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
            colours.background
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

    if GROUPS.get(sheet.group).copied() == Some("Language Servers") {
        return Ok(servers_list(ui, sheet, list, texts)?.or(ask));
    }
    if GROUPS.get(sheet.group).copied() == Some("Models") {
        return Ok(models_list(ui, sheet, settings, list, texts)?.or(ask));
    }
    let rows = sheet.shown(texts, settings);
    if rows.is_empty() {
        let small = ui.theme.font(13.0);
        ui.text(
            texts.tr("No matching settings"),
            list,
            &small,
            colours.dim,
            Align::Center,
        )?;
        return Ok(ask);
    }
    let defaults = Settings::default();
    let titles = GROUPS.get(sheet.group).copied().unwrap_or("All") == "All";
    let (panels, slots, content) = layout(&rows, list.width, titles);
    sheet.scroll = sheet.scroll.min((content - list.height).max(0.0));
    let typing = &mut sheet.typing;
    let (_, asked) = ScrollArea::new(&mut sheet.scroll, content).show(
        ui,
        Id::SettingsList,
        list,
        |ui, content, view| {
            let visible = |top: f32, bottom: f32| {
                content.y + bottom >= view.y && content.y + top <= view.bottom()
            };
            for panel in &panels {
                if !visible(panel.top, panel.bottom) {
                    continue;
                }
                if let Some(title) = panel.title {
                    let font = ui.theme.bold(12.0);
                    let at = Rect::new(
                        content.x + INSET,
                        content.y + panel.top,
                        content.width - 2.0 * INSET,
                        HEAD,
                    );
                    ui.text(texts.tr(title), at, &font, colours.dim, Align::Start)?;
                }
                let body = Rect::new(
                    content.x,
                    content.y + panel.body,
                    content.width,
                    panel.bottom - panel.body,
                );
                ui.round(body, 10.0, colours.raised)?;
            }
            let mut asked = None;
            for slot in &slots {
                if !visible(slot.y, slot.y + slot.h) {
                    continue;
                }
                let field: &'static Field = rows[slot.index];
                let top = content.y + slot.y;
                let at = Rect::new(content.x + INSET, top, content.width - 2.0 * INSET, slot.h);
                if !slot.last {
                    ui.fill(
                        Rect::new(at.x, at.bottom() - 1.0, at.width, 1.0),
                        colours.border.fade(0.5),
                    )?;
                }
                let part = u16::try_from(slot.index).unwrap_or(u16::MAX);
                if let Some(a) = row(ui, field, at, part, (settings, &defaults), typing, texts)? {
                    asked = Some(a);
                }
            }
            Ok(asked)
        },
    )?;
    Ok(asked.or(ask))
}

/// one language's server, as the settings list edits it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ServerRow {
    pub language: String,
    /// program and arguments, split on spaces.
    pub line: String,
    /// file names, split on commas or spaces.
    pub files: String,
    pub roots: String,
    pub off: bool,
    /// what serves it with no setting, shown when `line` is empty.
    pub default_line: String,
    /// the settings file has a table for it.
    pub set: bool,
}

impl ServerRow {
    /// the table this row writes; none when it says nothing past the
    /// default, so the table goes.
    pub fn rule(&self) -> Option<crate::settings::ServerRule> {
        let words = |s: &str| -> Vec<String> {
            s.split([',', ' '])
                .map(str::trim)
                .filter(|w| !w.is_empty())
                .map(str::to_owned)
                .collect()
        };
        let mut line = words(&self.line).into_iter();
        let rule = crate::settings::ServerRule {
            language: self.language.clone(),
            command: line.next().unwrap_or_default(),
            args: line.collect(),
            roots: words(&self.roots),
            files: words(&self.files),
            disabled: self.off,
        };
        let empty = rule.command.is_empty()
            && rule.files.is_empty()
            && rule.roots.is_empty()
            && !rule.disabled;
        (!empty).then_some(rule)
    }

    pub fn from_rule(rule: &crate::settings::ServerRule, default_line: String) -> Self {
        let mut line = rule.command.clone();
        for a in &rule.args {
            line.push(' ');
            line.push_str(a);
        }
        Self {
            language: rule.language.clone(),
            line,
            files: rule.files.join(", "),
            roots: rule.roots.join(", "),
            off: rule.disabled,
            default_line,
            set: true,
        }
    }
}

/// the language servers: one card each, then a row to add one.
fn servers_list(
    ui: &mut Ui<'_, Id>,
    sheet: &mut Sheet,
    list: Rect,
    texts: &Texts,
) -> AureaResult<Option<Ask>> {
    let colours = ui.theme.palette;
    let font = ui.theme.font(13.0);
    let small = ui.theme.font(12.0);
    let bold = ui.theme.bold(13.0);
    const CARD: f32 = 168.0;
    const ADD: f32 = 56.0;
    let content = sheet.servers.len() as f32 * (CARD + 10.0) + ADD + 40.0;
    sheet.scroll = sheet.scroll.min((content - list.height).max(0.0));
    let rows = &mut sheet.servers;
    let adding = &mut sheet.adding;
    let (_, asked) = ScrollArea::new(&mut sheet.scroll, content).show(ui, Id::SettingsList, list, |ui, content, view| {
        let mut asked = None;
        let mut y = content.y;
        let about = texts.tr("A server set here takes over from the built-in one. Empty fields keep the built-in values.");
        ui.text(about, Rect::new(content.x + INSET, y, content.width - 2.0 * INSET, 20.0), &small, colours.dim, Align::Start)?;
        y += 30.0;
        for (i, row) in rows.iter_mut().enumerate() {
            let card = Rect::new(content.x, y, content.width, CARD);
            y += CARD + 10.0;
            if card.bottom() < view.y || card.y > view.bottom() {
                continue;
            }
            ui.round(card, 10.0, colours.raised)?;
            let inner = card.inset(14.0);
            let n = u16::try_from(i).unwrap_or(u16::MAX / 8) * 8;
            ui.text(&row.language, Rect::new(inner.x, inner.y, inner.width, 20.0), &bold, colours.text, Align::Start)?;
            let state = if row.off {
                texts.tr("off")
            } else if row.set {
                texts.tr("set here")
            } else {
                texts.tr("built in")
            };
            ui.text(state, Rect::new(inner.x, inner.y, inner.width, 20.0), &small, colours.dim, Align::End)?;
            let label_w = 76.0_f32.min(inner.width * 0.3);
            let field = |ui: &mut Ui<'_, Id>, y: f32, label: &str| -> AureaResult<Rect> {
                ui.text(label, Rect::new(inner.x, y, label_w, 28.0), &small, colours.dim, Align::Start)?;
                Ok(Rect::new(inner.x + label_w, y, inner.width - label_w, 28.0))
            };
            let at = field(ui, inner.y + 26.0, texts.tr("Command"))?;
            let hint = if row.default_line.is_empty() { texts.tr("program and arguments") } else { row.default_line.as_str() };
            TextField::new(&mut row.line).placeholder(hint).max_chars(300).show(ui, Id::ServerField(n), at)?;
            let at = field(ui, inner.y + 60.0, texts.tr("Files"))?;
            TextField::new(&mut row.files).placeholder("*.ext, Name").max_chars(300).show(ui, Id::ServerField(n + 1), at)?;
            let at = field(ui, inner.y + 94.0, texts.tr("Roots"))?;
            TextField::new(&mut row.roots).placeholder("Cargo.toml, package.json").max_chars(300).show(ui, Id::ServerField(n + 2), at)?;
            let by = inner.y + 128.0;
            let mut bx = inner.right();
            let mut button = |ui: &mut Ui<'_, Id>, label: &str, part: u16, primary: bool| -> AureaResult<bool> {
                let w = ui.text_width(label, &small) + 26.0;
                bx -= w;
                let r = Rect::new(bx, by, w, 26.0);
                bx -= 6.0;
                let b = if primary { Button::new(label).primary() } else { Button::new(label).quiet() };
                Ok(r.x > inner.x && b.show(ui, Id::ServerField(n + part), r)?.clicked)
            };
            if button(ui, texts.tr("Save"), 3, true)? {
                asked = Some(Ask::Server(row.language.clone(), row.rule()));
            }
            if row.set && button(ui, texts.tr("Reset"), 4, false)? {
                asked = Some(Ask::Server(row.language.clone(), None));
            }
            let toggle = if row.off { texts.tr("Turn on") } else { texts.tr("Turn off") };
            if button(ui, toggle, 5, false)? {
                row.off = !row.off;
                asked = Some(Ask::Server(row.language.clone(), row.rule()));
            }
        }
        // a server for another language
        let at = Rect::new(content.x + INSET, y + 8.0, (content.width - 2.0 * INSET - 90.0).max(60.0), 30.0);
        let typed = TextField::new(adding)
            .placeholder(texts.tr("another language, like nim"))
            .max_chars(40)
            .show(ui, Id::ServerField(u16::MAX - 1), at)?;
        let add = Rect::new(at.right() + 8.0, at.y, 82.0, 30.0);
        let name = adding.trim().to_lowercase();
        let ok = crate::settings::server_language_ok(&name) && !rows.iter().any(|r| r.language == name);
        if (Button::new(texts.tr("Add")).enabled(ok).show(ui, Id::ServerField(u16::MAX), add)?.clicked || typed.submitted) && ok {
            rows.push(ServerRow { language: name, ..ServerRow::default() });
            adding.clear();
        }
        let _ = &font;
        Ok(asked)
    })?;
    Ok(asked)
}

/// the custom agent's model profiles: the one in use marked, each with
/// use, edit and delete; editing happens in the agent panel's card.
fn models_list(
    ui: &mut Ui<'_, Id>,
    sheet: &mut Sheet,
    settings: &Settings,
    list: Rect,
    texts: &Texts,
) -> AureaResult<Option<Ask>> {
    let colours = ui.theme.palette;
    let small = ui.theme.font(12.0);
    let bold = ui.theme.bold(13.0);
    const ROW_H: f32 = 64.0;
    let models = &settings.models;
    let active = settings.active_model().map(|m| m.name.clone());
    let content = models.len() as f32 * (ROW_H + 8.0) + 100.0;
    sheet.scroll = sheet.scroll.min((content - list.height).max(0.0));
    let (_, asked) = ScrollArea::new(&mut sheet.scroll, content).show(
        ui,
        Id::SettingsList,
        list,
        |ui, content, _| {
            let mut asked = None;
            let mut y = content.y;
            let about = texts.tr(
                "Model servers the custom agent can use. Keys are kept apart from these settings.",
            );
            ui.text_clipped(
                about,
                Rect::new(content.x + INSET, y, content.width - 2.0 * INSET, 20.0),
                &small,
                colours.dim,
                Align::Start,
            )?;
            y += 30.0;
            for (i, m) in models.iter().enumerate() {
                let card = Rect::new(content.x, y, content.width, ROW_H);
                y += ROW_H + 8.0;
                ui.round(card, 10.0, colours.raised)?;
                let inner = card.inset_xy(14.0, 10.0);
                let n = u16::try_from(i).unwrap_or(u16::MAX / 4) * 4;
                let on = active.as_deref() == Some(m.name.as_str());
                if on {
                    ui.round(
                        Rect::new(inner.x, inner.y + 6.0, 7.0, 7.0),
                        3.5,
                        colours.success,
                    )?;
                }
                let x = inner.x + if on { 14.0 } else { 0.0 };
                // the buttons first, so the words know their room
                let mut bx = inner.right();
                let mut button = |ui: &mut Ui<'_, Id>,
                                  label: &str,
                                  part: u16,
                                  primary: bool|
                 -> AureaResult<bool> {
                    let w = ui.text_width(label, &small) + 24.0;
                    bx -= w;
                    let r = Rect::new(bx, inner.y + (inner.height - 26.0) / 2.0, w, 26.0);
                    bx -= 6.0;
                    let b = if primary {
                        Button::new(label).primary()
                    } else {
                        Button::new(label).quiet()
                    };
                    Ok(r.x > x + 60.0 && b.show(ui, Id::ModelField(n + part), r)?.clicked)
                };
                if button(ui, texts.tr("Delete"), 0, false)? {
                    asked = Some(Ask::DeleteModel(m.name.clone()));
                }
                if button(ui, texts.tr("Edit"), 1, false)? {
                    asked = Some(Ask::EditModel(Some(m.name.clone())));
                }
                if !on && button(ui, texts.tr("Use"), 2, true)? {
                    asked = Some(Ask::UseModel(m.name.clone()));
                }
                let words = (bx - x - 8.0).max(0.0);
                ui.text_clipped(
                    &m.name,
                    Rect::new(x, inner.y, words, 20.0),
                    &bold,
                    colours.text,
                    Align::Start,
                )?;
                let line = format!("{} · {}", m.model, m.base_url);
                ui.text_clipped(
                    &line,
                    Rect::new(inner.x, inner.y + 24.0, words + (x - inner.x), 18.0),
                    &small,
                    colours.dim,
                    Align::Start,
                )?;
            }
            let add = Rect::new(content.x, y + 4.0, 170.0_f32.min(content.width), 30.0);
            if Button::new(texts.tr("Add model server"))
                .show(ui, Id::ModelField(u16::MAX), add)?
                .clicked
            {
                asked = Some(Ask::EditModel(None));
            }
            Ok(asked)
        },
    )?;
    Ok(asked)
}

/// the dot that marks a changed setting at `x`, `y`; true when a click puts
/// the default back.
fn reset_dot(ui: &mut Ui<'_, Id>, x: f32, y: f32, part: u16, texts: &Texts) -> AureaResult<bool> {
    let colours = ui.theme.palette;
    let centre = Point::new(x + 5.0, y + 11.0);
    let hit = Rect::new(x - 2.0, y + 3.0, 16.0, 16.0);
    let input = ui.interact(Id::SettingsReset, part, hit, Sense::Click, true);
    let r = if input.hovered { 5.0 } else { 3.5 };
    ui.circle(centre, r, colours.accent)?;
    if input.hovered {
        ui.set_cursor(CursorIcon::Pointer);
        ui.tooltip_at(
            Point::new(hit.x, hit.bottom()),
            texts.tr("Reset to default"),
        );
    }
    Ok(input.clicked)
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
    let now = current(settings, field.key).unwrap_or_default();
    let changed = current(defaults, field.key).is_some_and(|d| d != now);
    if let (FieldKind::Choice(words), true) = (field.kind, pictured(field.key)) {
        let picked = words.iter().position(|w| *w == unquote(&now)).unwrap_or(0);
        return cards(ui, field, words, at, part, (picked, changed), texts);
    }
    let mut ask = None;
    // the control keeps at most half a narrow card, the words the rest
    let row = at.inset_xy(0.0, 10.0);
    let (control, words) = row.split_right(CONTROL.min(row.width * 0.5), 16.0);
    let (dot, words) = words.split_left(14.0, 4.0);
    if changed && reset_dot(ui, dot.x, dot.y, part, texts)? {
        ask = Some(Ask::Set(field.key, None));
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
            if field.key == "side_overlay" {
                let pic = Rect::new(switch.x - 52.0, control.y + 6.0, 40.0, 24.0);
                overlay_picture(ui, pic, colours)?;
            }
        }
        FieldKind::Choice(words) => {
            let mut picked = words.iter().position(|w| *w == unquote(&now)).unwrap_or(0);
            let before = picked;
            let labels: Vec<&str> = words.iter().map(|w| texts.tr(w)).collect();
            if words.len() <= 3 {
                Segmented::new(&labels, &mut picked).show(ui, id, control.inset_xy(0.0, 3.0))?;
            } else {
                Dropdown::new(&labels, &mut picked).show(ui, id, control.inset_xy(0.0, 3.0))?;
            }
            if picked != before {
                ask = Some(pick(field.key, words[picked]));
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

/// a choice row: the label and about above, then one card per word.
fn cards(
    ui: &mut Ui<'_, Id>,
    field: &'static Field,
    words: &'static [&'static str],
    at: Rect,
    part: u16,
    (picked, changed): (usize, bool),
    texts: &Texts,
) -> AureaResult<Option<Ask>> {
    let colours = ui.theme.palette;
    let mut ask = None;
    let title = Rect::new(at.x, at.y + CARD_TOP, at.width - 24.0, TITLE_H);
    let title_font = ui.theme.bold(13.5);
    let small = ui.theme.font(12.0);
    ui.text_clipped(
        texts.tr(field.label),
        title,
        &title_font,
        colours.text,
        Align::Start,
    )?;
    if changed && reset_dot(ui, at.right() - 14.0, title.y, part, texts)? {
        ask = Some(Ask::Set(field.key, None));
    }
    let about = Rect::new(at.x, title.bottom() + 2.0, at.width, ABOUT_H);
    ui.text_clipped(
        texts.tr(field.about),
        about,
        &small,
        colours.dim,
        Align::Start,
    )?;

    let per = card_columns(at.width);
    let top = at.y + CARDS_AT;
    for (i, word) in words.iter().enumerate() {
        let x = at.x + (i % per) as f32 * (CARD_W + CARD_GAP);
        let y = top + (i / per) as f32 * (CARD_H + CAP_GAP + CAP_H + LINE_GAP);
        let card = Rect::new(x, y, CARD_W, CARD_H);
        let hit = Rect::new(x, y, CARD_W, CARD_H + CAP_GAP + CAP_H);
        let id = Id::SettingsRow(card_part(part, i));
        let input = ui.interact(id, 0, hit, Sense::Click, true);
        let state = ui.eased(id, 0, input);
        let on = i == picked;
        if input.clicked && !on {
            ask = Some(pick(field.key, word));
        }

        let fill = colours.background.mix(colours.hover, state.hover);
        ui.round(card, 9.0, fill)?;
        if on {
            ui.round(card, 9.0, colours.accent.fade(0.14))?;
            ui.border(card, 9.0, 2.0, colours.accent)?;
        } else {
            ui.border(card, 9.0, 1.0, colours.border)?;
        }
        picture(ui, field.key, word, card.inset(7.0), colours)?;
        FocusRing::new(ui.theme, 9.0, state.focus).paint(ui, card)?;

        let name = caption(word);
        let ink = if on {
            colours.text
        } else {
            colours.dim.mix(colours.text, state.hover)
        };
        let at_name = Rect::new(x, y + CARD_H + CAP_GAP, CARD_W, CAP_H);
        ui.text(texts.tr(&name), at_name, &small, ink, Align::Center)?;
        if input.hovered {
            ui.set_cursor(CursorIcon::Pointer);
        }
    }
    Ok(ask)
}

/// the part of a card's id: its row's part and its place in the row.
fn card_part(part: u16, i: usize) -> u16 {
    let j = u16::try_from(i).unwrap_or(0);
    CARD_PARTS
        .saturating_add(part.saturating_mul(16))
        .saturating_add(j)
}

/// a choice word as a card caption: the first letter up.
fn caption(word: &str) -> String {
    let mut chars = word.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// the small drawing on a card: what the word does on screen. `pic` is the
/// card inset, 70 by 44.
fn picture(ui: &mut Ui<'_, Id>, key: &str, word: &str, pic: Rect, c: Palette) -> AureaResult<()> {
    if key == "theme" {
        return theme_picture(ui, pic, word);
    }
    ui.round(pic, 5.0, c.surface)?;
    match (key, word) {
        ("rail", edge) => rail_window(ui, pic, rail_edge(edge), true, c)?,
        ("rail_show", "always") => rail_window(ui, pic, RailPlace::Left, true, c)?,
        ("rail_show", "hidden") => {
            ui.round(pic.inset(4.0), 3.0, c.raised)?;
        }
        ("rail_show", _) => edge_rail(ui, pic, c)?,
        ("side_bar", side) => side_window(ui, pic, side == "right", c)?,
        ("caret", style) => caret_picture(ui, pic, style, c)?,
        ("line_numbers", mode) => numbers_picture(ui, pic, mode, c)?,
        ("whitespace", mode) => whitespace_picture(ui, pic, mode, c)?,
        _ => {}
    }
    ui.border(pic, 5.0, 1.0, c.border)
}

fn rail_edge(word: &str) -> RailPlace {
    match word {
        "right" => RailPlace::Right,
        "top" => RailPlace::Top,
        "bottom" => RailPlace::Bottom,
        _ => RailPlace::Left,
    }
}

/// a window with the rail on `edge`, solid, and the editor beside it.
fn rail_window(
    ui: &mut Ui<'_, Id>,
    w: Rect,
    edge: RailPlace,
    solid: bool,
    c: Palette,
) -> AureaResult<()> {
    let (strip, editor) = match edge {
        RailPlace::Left => (
            Rect::new(w.x + 2.0, w.y + 2.0, 8.0, w.height - 4.0),
            Rect::new(w.x + 14.0, w.y + 4.0, w.width - 18.0, w.height - 8.0),
        ),
        RailPlace::Right => (
            Rect::new(w.right() - 10.0, w.y + 2.0, 8.0, w.height - 4.0),
            Rect::new(w.x + 4.0, w.y + 4.0, w.width - 18.0, w.height - 8.0),
        ),
        RailPlace::Top => (
            Rect::new(w.x + 2.0, w.y + 2.0, w.width - 4.0, 8.0),
            Rect::new(w.x + 4.0, w.y + 13.0, w.width - 8.0, w.height - 17.0),
        ),
        RailPlace::Bottom => (
            Rect::new(w.x + 2.0, w.bottom() - 10.0, w.width - 4.0, 8.0),
            Rect::new(w.x + 4.0, w.y + 4.0, w.width - 8.0, w.height - 17.0),
        ),
    };
    ui.round(editor, 3.0, c.raised)?;
    if !solid {
        return Ok(());
    }
    ui.round(strip, 3.0, c.border)?;
    let across = matches!(edge, RailPlace::Top | RailPlace::Bottom);
    for i in 0..3 {
        let f = (i as f32 + 0.5) / 3.0;
        let at = if across {
            Point::new(strip.x + strip.width * f, strip.center().y)
        } else {
            Point::new(strip.center().x, strip.y + strip.height * f)
        };
        ui.circle(at, 1.4, c.dim)?;
    }
    Ok(())
}

/// the rail slid half out at the left edge, with a chevron pointing out.
fn edge_rail(ui: &mut Ui<'_, Id>, w: Rect, c: Palette) -> AureaResult<()> {
    ui.round(
        Rect::new(w.x + 4.0, w.y + 4.0, w.width - 8.0, w.height - 8.0),
        3.0,
        c.raised,
    )?;
    ui.fill(
        Rect::new(w.x, w.y + 2.0, 5.0, w.height - 4.0),
        c.border.fade(0.6),
    )?;
    for (i, alpha) in [0.35, 0.2, 0.1].into_iter().enumerate() {
        let x = w.x + 5.0 + i as f32 * 1.5;
        ui.fill(
            Rect::new(x, w.y + 2.0, 1.5, w.height - 4.0),
            c.border.fade(alpha),
        )?;
    }
    let mid = w.y + w.height / 2.0;
    let x = w.x + 12.0;
    ui.line(
        Point::new(x, mid - 4.0),
        Point::new(x + 3.0, mid),
        1.4,
        c.dim,
    )?;
    ui.line(
        Point::new(x + 3.0, mid),
        Point::new(x, mid + 4.0),
        1.4,
        c.dim,
    )
}

/// a side panel on one edge of the window, two lines of text in it.
fn side_window(ui: &mut Ui<'_, Id>, w: Rect, right: bool, c: Palette) -> AureaResult<()> {
    let (panel, editor) = if right {
        (
            Rect::new(w.right() - 22.0, w.y + 3.0, 19.0, w.height - 6.0),
            Rect::new(w.x + 3.0, w.y + 3.0, w.width - 28.0, w.height - 6.0),
        )
    } else {
        (
            Rect::new(w.x + 3.0, w.y + 3.0, 19.0, w.height - 6.0),
            Rect::new(w.x + 25.0, w.y + 3.0, w.width - 28.0, w.height - 6.0),
        )
    };
    ui.round(editor, 3.0, c.raised)?;
    ui.round(panel, 3.0, c.border)?;
    for i in 0..2 {
        let y = panel.y + 6.0 + i as f32 * 6.0;
        ui.fill(
            Rect::new(panel.x + 4.0, y, panel.width - 8.0, 2.0),
            c.dim.fade(0.6),
        )?;
    }
    Ok(())
}

/// a mini editor in fixed colours: the theme shows what it means, not the
/// current palette.
struct Mini {
    bg: Color,
    gutter: Color,
    bars: [Color; 4],
}

const DARK: Mini = Mini {
    bg: Color::rgb(30, 31, 36),
    gutter: Color::rgb(42, 45, 53),
    bars: [
        Color::rgb(97, 175, 239),
        Color::rgb(198, 120, 221),
        Color::rgb(152, 195, 121),
        Color::rgb(209, 154, 102),
    ],
};

const LIGHT: Mini = Mini {
    bg: Color::rgb(251, 251, 253),
    gutter: Color::rgb(236, 238, 242),
    bars: [
        Color::rgb(64, 120, 242),
        Color::rgb(166, 38, 164),
        Color::rgb(80, 161, 79),
        Color::rgb(152, 104, 1),
    ],
};

fn mini(ui: &mut Ui<'_, Id>, r: Rect, m: &Mini) -> AureaResult<()> {
    const SHARE: [f32; 4] = [0.6, 0.35, 0.75, 0.45];
    ui.round(r, 4.0, m.bg)?;
    let gutter = Rect::new(r.x, r.y + 3.0, r.width * 0.2, r.height - 6.0);
    ui.fill(gutter, m.gutter)?;
    let x = gutter.right() + 4.0;
    let avail = (r.right() - x - 4.0).max(0.0);
    for (i, bar) in m.bars.iter().enumerate() {
        let y = r.y + 8.0 + i as f32 * 9.0;
        ui.round(Rect::new(x, y, avail * SHARE[i], 2.5), 1.2, *bar)?;
    }
    Ok(())
}

/// light, dark, or both halves for the system.
fn theme_picture(ui: &mut Ui<'_, Id>, pic: Rect, word: &str) -> AureaResult<()> {
    match word {
        "light" => mini(ui, pic, &LIGHT),
        "system" => {
            let half = (pic.width - 2.0) / 2.0;
            mini(ui, Rect::new(pic.x, pic.y, half, pic.height), &LIGHT)?;
            mini(
                ui,
                Rect::new(pic.x + half + 2.0, pic.y, half, pic.height),
                &DARK,
            )
        }
        _ => mini(ui, pic, &DARK),
    }
}

/// a few letters with the caret drawn after the second.
fn caret_picture(ui: &mut Ui<'_, Id>, w: Rect, style: &str, c: Palette) -> AureaResult<()> {
    let font = ui.theme.font(16.0);
    let text = Rect::new(w.x + 10.0, w.y + 10.0, w.width - 20.0, 24.0);
    ui.text("abc", text, &font, c.text, Align::Start)?;
    let x = text.x + ui.text_width("ab", &font);
    let cw = ui.text_width("c", &font);
    let bar = Rect::new(x, w.y + 12.0, cw, 20.0);
    match style {
        "block" => {
            ui.fill(bar, c.accent)?;
            ui.text(
                "c",
                Rect::new(x, text.y, cw, text.height),
                &font,
                c.surface,
                Align::Start,
            )
        }
        "underline" => ui.fill(Rect::new(x, w.y + 33.0, cw, 2.5), c.accent),
        _ => ui.fill(Rect::new(x - 1.0, bar.y, 2.0, bar.height), c.accent),
    }
}

/// a gutter with numbers, counted from the caret line or none.
fn numbers_picture(ui: &mut Ui<'_, Id>, w: Rect, mode: &str, c: Palette) -> AureaResult<()> {
    const SHARE: [f32; 4] = [0.6, 0.8, 0.45, 0.7];
    let relative = mode == "relative";
    let numbered = mode != "off";
    let code_x = if numbered { w.x + 20.0 } else { w.x + 6.0 };
    if numbered {
        ui.fill(
            Rect::new(w.x + 1.0, w.y + 1.0, 15.0, w.height - 2.0),
            c.raised,
        )?;
    }
    let font = ui.theme.font(7.5);
    let labels = if relative {
        ["2", "1", "0", "1"]
    } else {
        ["1", "2", "3", "4"]
    };
    for (i, label) in labels.iter().enumerate() {
        let y = w.y + 4.0 + i as f32 * 10.0;
        if numbered {
            let ink = if relative && i == 2 { c.text } else { c.dim };
            ui.text(
                label,
                Rect::new(w.x + 1.0, y, 13.0, 9.0),
                &font,
                ink,
                Align::End,
            )?;
        }
        let len = (w.right() - code_x - 5.0).max(0.0) * SHARE[i];
        ui.round(Rect::new(code_x, y + 3.0, len, 2.5), 1.2, c.dim.fade(0.5))?;
    }
    Ok(())
}

/// a line with a space in it, and the dots that show spaces.
fn whitespace_picture(ui: &mut Ui<'_, Id>, w: Rect, mode: &str, c: Palette) -> AureaResult<()> {
    let font = ui.theme.font(10.5);
    let x0 = w.x + 7.0;
    let y1 = w.y + 9.0;
    let y2 = w.y + 26.0;
    let h = 14.0;
    let one = ui.text_width("one", &font);
    let space = ui.text_width(" ", &font);
    let two = ui.text_width("two", &font);
    if mode == "selection" {
        let at = Rect::new(x0 + one, y1, space + two + 1.0, h);
        ui.round(at, 2.0, c.accent.fade(0.25))?;
    }
    ui.text(
        "one two",
        Rect::new(x0, y1, w.width, h),
        &font,
        c.text,
        Align::Start,
    )?;
    ui.text(
        "  six",
        Rect::new(x0, y2, w.width, h),
        &font,
        c.text,
        Align::Start,
    )?;
    if mode != "none" {
        ui.circle(Point::new(x0 + one + space / 2.0, y1 + 9.0), 1.1, c.dim)?;
    }
    if mode == "all" {
        ui.circle(Point::new(x0 + space / 2.0, y2 + 9.0), 1.1, c.dim)?;
        ui.circle(Point::new(x0 + space * 1.5, y2 + 9.0), 1.1, c.dim)?;
    }
    Ok(())
}

/// a window with a panel floating over it, for the side bar overlay toggle.
fn overlay_picture(ui: &mut Ui<'_, Id>, w: Rect, c: Palette) -> AureaResult<()> {
    ui.round(w, 4.0, c.surface)?;
    ui.round(w.inset(2.0), 3.0, c.raised)?;
    let panel = Rect::new(w.x + 3.0, w.y + 3.0, 14.0, w.height - 6.0);
    ui.shadow(panel, 3.0, 6.0, c.shadow.fade(0.6))?;
    ui.round(panel, 3.0, c.border)?;
    ui.border(w, 4.0, 1.0, c.border)
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

    #[test]
    fn groups_make_panels_and_the_last_row_has_no_divider() {
        let rows: Vec<&'static Field> = FIELDS.iter().filter(|f| f.group == "Look").collect();
        let (panels, slots, height) = layout(&rows, 720.0, true);
        assert_eq!(panels.len(), 1);
        assert_eq!(slots.len(), rows.len());
        assert!(slots.last().is_some_and(|s| s.last));
        assert!(slots[..slots.len() - 1].iter().all(|s| !s.last));
        assert!(height > 0.0);
    }

    #[test]
    fn cards_wrap_to_the_width() {
        assert_eq!(card_columns(720.0), 7);
        assert_eq!(card_columns(40.0), 1);
        let theme = FIELDS.iter().find(|f| f.key == "theme").expect("theme");
        assert!(row_height(theme, 744.0) > ROW);
        assert!(row_height(theme, 40.0) > row_height(theme, 744.0));
    }

    #[test]
    fn captions_lead_with_a_capital() {
        assert_eq!(caption("dark"), "Dark");
        assert_eq!(caption(""), "");
    }
}
