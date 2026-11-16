//! the extensions panel: each package as a card with what it asks for and
//! adds, or why it is off.

use aurea::AureaResult;
use aurea::render::{Color, Rect};
use aurea::ui::{Align, Button, ScrollArea, Ui};

use super::{Extensions, Package, State};
use crate::app::Id;
use crate::lang::Texts;

const PAD: f32 = 10.0;
const BAR: f32 = 28.0;
const LINE: f32 = 18.0;
/// problems shown per package; more are counted.
const MAX_SHOWN: usize = 6;

/// what the panel asks the app.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ask {
    /// the folder in the system's file manager, made when missing.
    OpenFolder,
    Reload,
}

/// the lines a card has under its title.
fn lines(p: &Package) -> Vec<(String, Tone)> {
    let mut out = Vec::new();
    match &p.state {
        State::Ready(m) => {
            if !m.description.is_empty() {
                out.push((m.description.clone(), Tone::Plain));
            }
            let mut adds = Vec::new();
            if !m.languages.is_empty() {
                let names: Vec<&str> = m.languages.iter().map(|l| l.name.as_str()).collect();
                adds.push(format!("languages: {}", names.join(", ")));
            }
            if !m.snippets.is_empty() {
                let n = m.snippets.len();
                adds.push(if n == 1 {
                    "1 snippet".to_owned()
                } else {
                    format!("{n} snippets")
                });
            }
            if !adds.is_empty() {
                out.push((adds.join("  ·  "), Tone::Dim));
            }
            let p = &m.permissions;
            let asks: Vec<&str> = [
                (p.read_workspace, "reads the workspace"),
                (p.write_documents, "edits documents"),
                (p.spawn_process, "runs programs"),
                (p.network, "uses the network"),
            ]
            .iter()
            .filter(|(on, _)| *on)
            .map(|(_, what)| *what)
            .collect();
            if !asks.is_empty() {
                out.push((format!("asks to: {}", asks.join(", ")), Tone::Warn));
            }
            for w in m.warnings.iter().take(MAX_SHOWN) {
                out.push((w.clone(), Tone::Dim));
            }
        }
        State::Broken(why) => {
            for w in why.iter().take(MAX_SHOWN) {
                out.push((w.clone(), Tone::Bad));
            }
            if why.len() > MAX_SHOWN {
                out.push((format!("{} more", why.len() - MAX_SHOWN), Tone::Bad));
            }
        }
        State::Duplicate(id) => {
            out.push((
                format!("{id} is already loaded from another folder"),
                Tone::Bad,
            ));
        }
    }
    if !p.grammars.is_empty() {
        let names: Vec<&str> = p.grammars.iter().map(|g| g.name.as_str()).collect();
        out.push((format!("grammars: {}", names.join(", ")), Tone::Dim));
    }
    for w in p.problems.iter().take(MAX_SHOWN) {
        out.push((w.clone(), Tone::Bad));
    }
    out
}

#[derive(Clone, Copy)]
enum Tone {
    Plain,
    Dim,
    Warn,
    Bad,
}

fn height(p: &Package) -> f32 {
    LINE * 2.0 + 14.0 + lines(p).len() as f32 * LINE
}

pub fn show(
    ui: &mut Ui<'_, Id>,
    ext: &mut Extensions,
    rect: Rect,
    texts: &Texts,
) -> AureaResult<Option<Ask>> {
    let palette = ui.theme.palette;
    let small = ui.theme.font(12.0);
    let font = ui.theme.font(13.0);
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
            .tr("no extensions yet: put a folder with an extension.toml in the extensions folder");
        let at = Rect::new(list.x, list.y + 8.0, list.width, 40.0);
        ui.text_clipped(hint, at, &small, palette.dim, Align::Start)?;
        return Ok(ask);
    }
    let heights: Vec<f32> = ext.packages.iter().map(height).collect();
    let content: f32 = heights.iter().map(|h| h + 8.0).sum::<f32>() + ext.notes.len() as f32 * LINE;
    let packages = &ext.packages;
    let notes = &ext.notes;
    ScrollArea::new(&mut ext.scroll, content).show(
        ui,
        Id::ExtensionList,
        list,
        |ui, content, view| {
            let mut y = content.y;
            for note in notes {
                let at = Rect::new(content.x, y, content.width, LINE);
                ui.text_clipped(note, at, &small, palette.warning, Align::Start)?;
                y += LINE;
            }
            for (p, h) in packages.iter().zip(&heights) {
                let card = Rect::new(content.x, y, content.width, *h);
                y += h + 8.0;
                if card.bottom() < view.y || card.y > view.bottom() {
                    continue;
                }
                ui.round(card, 8.0, palette.raised)?;
                ui.border(card, 8.0, 1.0, palette.border)?;
                let body = card.inset_xy(12.0, 7.0);
                let (title, sub) = match &p.state {
                    State::Ready(m) => (
                        format!(
                            "{}  {}.{}.{}",
                            m.name, m.version.0, m.version.1, m.version.2
                        ),
                        m.id.clone(),
                    ),
                    _ => (
                        p.dir
                            .file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_default(),
                        texts.tr("not loaded").to_owned(),
                    ),
                };
                let dot: Color = match &p.state {
                    State::Ready(_) => palette.success,
                    _ => palette.danger,
                };
                let (mark, rest) = body.split_left(8.0, 8.0);
                ui.circle(
                    aurea::render::Point::new(mark.x + 4.0, mark.y + LINE / 2.0),
                    3.5,
                    dot,
                )?;
                let mut line_y = rest.y;
                ui.text_clipped(
                    &title,
                    Rect::new(rest.x, line_y, rest.width, LINE),
                    &font,
                    palette.text,
                    Align::Start,
                )?;
                line_y += LINE;
                ui.text_clipped(
                    &sub,
                    Rect::new(rest.x, line_y, rest.width, LINE),
                    &small,
                    palette.faint,
                    Align::Start,
                )?;
                line_y += LINE + 4.0;
                for (text, tone) in lines(p) {
                    let colour = match tone {
                        Tone::Plain => palette.text,
                        Tone::Dim => palette.dim,
                        Tone::Warn => palette.warning,
                        Tone::Bad => palette.danger,
                    };
                    ui.text_clipped(
                        &text,
                        Rect::new(rest.x, line_y, rest.width, LINE),
                        &small,
                        colour,
                        Align::Start,
                    )?;
                    line_y += LINE;
                }
            }
            Ok(())
        },
    )?;
    Ok(ask)
}
