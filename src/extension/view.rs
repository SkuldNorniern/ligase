//! the extensions panel: each package as a card with its monogram, state,
//! what it adds, and what it asks for.

use aurea::AureaResult;
use aurea::render::{Color, Font, Rect};
use aurea::ui::{Align, Button, Palette, ScrollArea, Sense, Ui};

use super::manifest::Manifest;
use super::{Extensions, Package, State};
use crate::app::Id;
use crate::lang::Texts;

const PAD: f32 = 10.0;
const BAR: f32 = 28.0;
const GAP: f32 = 8.0;
const LINE: f32 = 16.0;
const CONTROL: f32 = 28.0;
/// inner padding of a card.
const INSET: f32 = 12.0;
const RADIUS: f32 = 10.0;
const TILE: f32 = 36.0;
/// space between the tile and the text column.
const TILE_GAP: f32 = 10.0;
const PILL: f32 = 18.0;
const CHIP: f32 = 20.0;
/// first hover part of a card on the list's id; buttons use their own.
const CARD_PART: u16 = 0x4000;
/// description lines shown; the rest is cut with an ellipsis.
const DESC_LINES: usize = 2;
/// problems and warnings shown per card; more are counted.
const MAX_LINES: usize = 3;

/// what the panel asks the app.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ask {
    /// the folder in the system's file manager, made when missing.
    OpenFolder,
    Reload,
    /// let the package at this place in the list start what it declares.
    Allow(usize),
    /// take that back.
    Revoke(usize),
}

/// a card has an allow button when its package declares servers.
fn asks_to_run(p: &Package) -> bool {
    matches!(&p.state, State::Ready(m) if !m.servers.is_empty())
}

/// how a line of text is drawn.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Tone {
    Title,
    Sub,
    Dim,
    Faint,
    Warn,
    Bad,
    Mono,
}

fn colour(p: &Palette, tone: Tone) -> Color {
    match tone {
        Tone::Title => p.text,
        Tone::Sub | Tone::Faint => p.faint,
        Tone::Dim | Tone::Mono => p.dim,
        Tone::Warn => p.warning,
        Tone::Bad => p.danger.mix(p.dim, 0.35),
    }
}

/// the fonts a card uses, made once per frame.
struct Faces {
    body: Font,
    small: Font,
    bold: Font,
    mono: Font,
}

fn faces(ui: &Ui<'_, Id>) -> Faces {
    Faces {
        body: ui.theme.font(12.0),
        small: ui.theme.font(11.5),
        bold: ui.theme.bold(13.0),
        mono: ui.theme.mono(11.5),
    }
}

/// one line of text, placed relative to its card.
struct Row {
    at: Rect,
    text: String,
    tone: Tone,
}

/// a rounded label: a version, a state or a contribution.
struct Pill {
    at: Rect,
    text: String,
    fill: Color,
    ink: Color,
}

/// a card with everything placed, relative to its top left.
struct Card {
    height: f32,
    tile: Rect,
    letters: String,
    tint: Color,
    title: Row,
    version: Option<Pill>,
    state: Pill,
    sub: Row,
    rows: Vec<Row>,
    chips: Vec<Pill>,
    /// the allow or revoke button, and whether it is on.
    button: Option<(Rect, bool)>,
}

/// the first letters of a name: initials of the first two words, or the
/// first two letters of a single word.
fn monogram(name: &str) -> String {
    let words: Vec<&str> = name
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect();
    let picked: String = match words.as_slice() {
        [] => "?".to_owned(),
        [one] => one.chars().take(2).collect(),
        [a, b, ..] => a.chars().take(1).chain(b.chars().take(1)).collect(),
    };
    picked.to_uppercase()
}

/// a tile colour from the id, so a package keeps its colour.
fn tint(p: &Palette, key: &str) -> Color {
    let mut hash: u32 = 0x811c_9dc5;
    for b in key.bytes() {
        hash ^= u32::from(b);
        hash = hash.wrapping_mul(0x0100_0193);
    }
    let options = [p.accent, p.success, p.warning, p.danger, p.dim];
    options[hash as usize % options.len()]
}

/// `text` wrapped at word boundaries to `width`, at most `max` lines; the
/// last line gets an ellipsis when more is left.
fn wrap(ui: &mut Ui<'_, Id>, text: &str, font: &Font, width: f32, max: usize) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut more = false;
    for word in text.split_whitespace() {
        let joined = if cur.is_empty() {
            word.to_owned()
        } else {
            format!("{cur} {word}")
        };
        if cur.is_empty() || ui.text_width(&joined, font) <= width {
            cur = joined;
            continue;
        }
        out.push(std::mem::replace(&mut cur, word.to_owned()));
        if out.len() == max {
            more = true;
            break;
        }
    }
    if !more && !cur.is_empty() {
        if out.len() < max {
            out.push(cur);
        } else {
            more = true;
        }
    }
    if more && let Some(last) = out.last_mut() {
        last.push('…');
    }
    out
}

/// the contribution chips a package adds, only the ones it has.
fn chip_labels(m: &Manifest, p: &Package, texts: &Texts) -> Vec<String> {
    let count = |n: usize, one: &str, many: &str| {
        format!("{n} {}", texts.tr(if n == 1 { one } else { many }))
    };
    let mut out = Vec::new();
    if !m.languages.is_empty() {
        out.push(count(m.languages.len(), "language", "languages"));
    }
    if !m.snippets.is_empty() {
        out.push(count(m.snippets.len(), "snippet", "snippets"));
    }
    if !m.views.is_empty() {
        out.push(count(m.views.len(), "view", "views"));
    }
    if !m.servers.is_empty() {
        out.push(count(m.servers.len(), "server", "servers"));
    }
    if !p.grammars.is_empty() {
        out.push(count(p.grammars.len(), "grammar", "grammars"));
    }
    if !m.abbreviations.is_empty() {
        out.push(texts.tr("abbreviations").to_owned());
    }
    out
}

/// lays out one card for a width. measures text, so it runs before the
/// list is drawn.
fn build(ui: &mut Ui<'_, Id>, p: &Package, texts: &Texts, allowed: bool, width: f32) -> Card {
    let palette = ui.theme.palette;
    let f = faces(ui);
    let folder = p
        .dir
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let text_x = INSET + TILE + TILE_GAP;
    let text_w = (width - text_x - INSET).max(0.0);
    let right = text_x + text_w;

    // name, sub line, version, state and the key the tile colour comes from
    let (name, sub, version, state, state_ink, key) = match &p.state {
        State::Ready(m) => {
            let (label, ink) = if !m.servers.is_empty() && !allowed {
                (texts.tr("Needs permission"), palette.warning)
            } else {
                (texts.tr("Ready"), palette.success)
            };
            (
                m.name.clone(),
                m.id.clone(),
                Some(format!("{}.{}.{}", m.version.0, m.version.1, m.version.2)),
                label,
                ink,
                m.id.clone(),
            )
        }
        State::Broken(_) => (
            folder.clone(),
            texts.tr("not loaded").to_owned(),
            None,
            texts.tr("Broken"),
            palette.danger,
            folder.clone(),
        ),
        State::Duplicate(id) => (
            folder.clone(),
            id.clone(),
            None,
            texts.tr("Duplicate"),
            palette.dim,
            folder.clone(),
        ),
    };
    let tile_ink = tint(&palette, &key);

    // title row: the state pill on the right, the version after the name
    let state_w = ui.text_width(state, &f.small) + 14.0;
    let state_x = right - state_w;
    let limit = state_x - 8.0;
    let ver = version.map(|v| {
        let w = ui.text_width(&v, &f.small) + 14.0;
        (v, w)
    });
    let ver_w = ver.as_ref().map_or(0.0, |(_, w)| w + 6.0);
    let title_max = (limit - text_x - ver_w).max(0.0);
    let title_w = ui.text_width(&name, &f.bold).min(title_max);
    let version = ver.map(|(v, w)| Pill {
        at: Rect::new(text_x + title_w + 6.0, INSET, w, PILL),
        text: v,
        fill: palette.surface,
        ink: palette.dim,
    });
    let state = Pill {
        at: Rect::new(state_x, INSET, state_w, PILL),
        text: state.to_owned(),
        fill: state_ink.fade(0.16),
        ink: state_ink,
    };
    let title = Row {
        at: Rect::new(text_x, INSET, title_max, PILL),
        text: name,
        tone: Tone::Title,
    };
    let sub = Row {
        at: Rect::new(text_x, INSET + 20.0, text_w, LINE),
        text: sub,
        tone: Tone::Sub,
    };

    let mut y = INSET + TILE;
    let mut rows = Vec::new();
    let mut chips = Vec::new();
    let mut button = None;
    let mut notes: Vec<(String, Tone)> = Vec::new();

    if let State::Ready(m) = &p.state {
        if !m.description.is_empty() {
            y += GAP;
            for line in wrap(ui, &m.description, &f.body, text_w, DESC_LINES) {
                rows.push(Row {
                    at: Rect::new(text_x, y, text_w, LINE),
                    text: line,
                    tone: Tone::Dim,
                });
                y += LINE;
            }
        }
        let labels = chip_labels(m, p, texts);
        if !labels.is_empty() {
            y += GAP;
            let mut x = text_x;
            for label in labels {
                let w = ui.text_width(&label, &f.small) + 14.0;
                if x > text_x && x + w > right {
                    x = text_x;
                    y += CHIP + 4.0;
                }
                chips.push(Pill {
                    at: Rect::new(x, y, w, CHIP),
                    text: label,
                    fill: palette.surface,
                    ink: palette.dim,
                });
                x += w + 4.0;
            }
            y += CHIP;
        }

        // what it asks for: the permissions and the programs it would start
        let asks: Vec<&str> = [
            (m.permissions.read_workspace, "reads the workspace"),
            (m.permissions.write_documents, "edits documents"),
            (m.permissions.spawn_process, "runs programs"),
            (m.permissions.network, "uses the network"),
        ]
        .iter()
        .filter(|(on, _)| *on)
        .map(|(_, what)| *what)
        .collect();
        if !asks.is_empty() || !m.servers.is_empty() {
            y += GAP;
        }
        if !asks.is_empty() {
            rows.push(Row {
                at: Rect::new(text_x, y, text_w, LINE),
                text: format!("asks to: {}", asks.join(", ")),
                tone: Tone::Warn,
            });
            y += LINE;
        }
        for s in &m.servers {
            let lang_w = ui.text_width(&s.language, &f.small) + 8.0;
            rows.push(Row {
                at: Rect::new(text_x, y, lang_w, LINE),
                text: s.language.clone(),
                tone: Tone::Faint,
            });
            rows.push(Row {
                at: Rect::new(text_x + lang_w, y, (text_w - lang_w).max(0.0), LINE),
                text: s.line().join(" "),
                tone: Tone::Mono,
            });
            y += LINE;
        }
        if asks_to_run(p) {
            y += 4.0;
            button = Some((Rect::new(text_x, y, text_w.min(220.0), CONTROL), allowed));
            y += CONTROL;
        }

        notes.extend(
            p.problems
                .iter()
                .take(MAX_LINES)
                .map(|w| (w.clone(), Tone::Bad)),
        );
        notes.extend(
            m.warnings
                .iter()
                .take(MAX_LINES)
                .map(|w| (w.clone(), Tone::Dim)),
        );
    } else {
        notes.extend(
            p.problems
                .iter()
                .take(MAX_LINES)
                .map(|w| (w.clone(), Tone::Bad)),
        );
        match &p.state {
            State::Broken(why) => {
                notes.extend(why.iter().take(MAX_LINES).map(|w| (w.clone(), Tone::Bad)));
                if why.len() > MAX_LINES {
                    notes.push((format!("{} more", why.len() - MAX_LINES), Tone::Bad));
                }
            }
            State::Duplicate(id) => {
                notes.push((
                    format!("{id} is already loaded from another folder"),
                    Tone::Bad,
                ));
            }
            State::Ready(_) => {}
        }
    }
    if !notes.is_empty() {
        y += GAP;
        for (text, tone) in notes {
            rows.push(Row {
                at: Rect::new(text_x, y, text_w, LINE),
                text,
                tone,
            });
            y += LINE;
        }
    }

    let letters = monogram(&title.text);
    Card {
        height: y + INSET,
        tile: Rect::new(INSET, INSET, TILE, TILE),
        letters,
        tint: tile_ink,
        title,
        version,
        state,
        sub,
        rows,
        chips,
        button,
    }
}

/// draws one card at `at` and says which ask its button made, if any.
fn draw(
    ui: &mut Ui<'_, Id>,
    card: &Card,
    at: Rect,
    texts: &Texts,
    index: usize,
) -> AureaResult<Option<Ask>> {
    let palette = ui.theme.palette;
    let part = CARD_PART + u16::try_from(index).unwrap_or(0);
    let hit = ui.interact(Id::ExtensionList, part, at, Sense::Hover, true);
    let eased = ui.eased(Id::ExtensionList, part, hit);
    ui.round(
        at,
        RADIUS,
        palette.raised.mix(palette.hover, eased.hover * 0.4),
    )?;
    ui.border(
        at,
        RADIUS,
        1.0,
        palette.border.mix(palette.dim, eased.hover * 0.6),
    )?;
    let abs = |r: Rect| Rect::new(at.x + r.x, at.y + r.y, r.width, r.height);
    let f = faces(ui);

    ui.round(abs(card.tile), 9.0, card.tint.fade(0.2))?;
    ui.text(
        &card.letters,
        abs(card.tile),
        &f.bold,
        card.tint,
        Align::Center,
    )?;

    let row = |ui: &mut Ui<'_, Id>, r: &Row| -> AureaResult<()> {
        let font = match r.tone {
            Tone::Title => &f.bold,
            Tone::Mono => &f.mono,
            Tone::Sub | Tone::Faint => &f.small,
            _ => &f.body,
        };
        ui.text_clipped(
            &r.text,
            abs(r.at),
            font,
            colour(&palette, r.tone),
            Align::Start,
        )?;
        Ok(())
    };
    let pill = |ui: &mut Ui<'_, Id>, p: &Pill| -> AureaResult<()> {
        let at = abs(p.at);
        ui.round(at, at.height / 2.0, p.fill)?;
        ui.text(&p.text, at, &f.small, p.ink, Align::Center)
    };

    row(ui, &card.title)?;
    if let Some(v) = &card.version {
        pill(ui, v)?;
    }
    pill(ui, &card.state)?;
    row(ui, &card.sub)?;
    for r in &card.rows {
        row(ui, r)?;
    }
    for c in &card.chips {
        pill(ui, c)?;
    }

    if let Some((r, on)) = card.button {
        let label = if on {
            texts.tr("Revoke")
        } else {
            texts.tr("Allow Programs")
        };
        let button = Button::new(label);
        let button = if on { button.quiet() } else { button.primary() };
        let part = u16::try_from(index + 2).unwrap_or(u16::MAX);
        if button.show(ui, Id::ExtensionButton(part), abs(r))?.clicked {
            return Ok(Some(if on {
                Ask::Revoke(index)
            } else {
                Ask::Allow(index)
            }));
        }
    }
    Ok(None)
}

pub fn show(
    ui: &mut Ui<'_, Id>,
    ext: &mut Extensions,
    rect: Rect,
    texts: &Texts,
) -> AureaResult<Option<Ask>> {
    let palette = ui.theme.palette;
    let small = ui.theme.font(12.0);
    let inner = rect.inset(PAD);
    let (bar, list) = inner.split_top(BAR, 8.0);
    let (open, reload) = bar.split_left((bar.width - 6.0) / 2.0, 6.0);
    let mut ask = None;
    if Button::new(texts.tr("Open Folder"))
        .quiet()
        .show(ui, Id::ExtensionButton(0), open)?
        .clicked
    {
        ask = Some(Ask::OpenFolder);
    }
    if Button::new(texts.tr("Reload"))
        .quiet()
        .show(ui, Id::ExtensionButton(1), reload)?
        .clicked
    {
        ask = Some(Ask::Reload);
    }
    if ext.packages.is_empty() {
        let hint = texts
            .tr("No extensions installed. Add a folder with an extension.toml to the extensions folder.");
        let at = Rect::new(list.x, list.y + 8.0, list.width, 40.0);
        ui.text_clipped(hint, at, &small, palette.dim, Align::Start)?;
        return Ok(ask);
    }
    let allowed: Vec<bool> = ext
        .packages
        .iter()
        .map(|p| matches!(&p.state, State::Ready(m) if ext.allowed(m)))
        .collect();
    let packages = &ext.packages;
    let notes = &ext.notes;
    let mut card_ask = None;
    ScrollArea::new(&mut ext.scroll, 0.0).show_measured(
        ui,
        Id::ExtensionList,
        list,
        |ui, width| {
            let mut cards = Vec::with_capacity(packages.len());
            for (p, on) in packages.iter().zip(&allowed) {
                cards.push(build(ui, p, texts, *on, width));
            }
            let content: f32 =
                cards.iter().map(|c| c.height + GAP).sum::<f32>() + notes.len() as f32 * LINE;
            Ok((content, cards))
        },
        |ui, content, view, cards: Vec<Card>| {
            let mut y = content.y;
            for note in notes {
                let at = Rect::new(content.x, y, content.width, LINE);
                ui.text_clipped(note, at, &small, palette.warning, Align::Start)?;
                y += LINE;
            }
            for (i, card) in cards.iter().enumerate() {
                let at = Rect::new(content.x, y, content.width, card.height);
                y += card.height + GAP;
                if at.bottom() < view.y || at.y > view.bottom() {
                    continue;
                }
                if let Some(picked) = draw(ui, card, at, texts, i)? {
                    card_ask = Some(picked);
                }
            }
            Ok(())
        },
    )?;
    Ok(card_ask.or(ask))
}
