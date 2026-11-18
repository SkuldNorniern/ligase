//! markdown as a page to read: blocks measured once per width, drawn in a
//! centred column, links that open.

use std::collections::HashMap;

use aurea::render::{Color, Font, FontStyle, FontWeight, Rect};
use aurea::ui::{Align, Palette, ScrollArea, Sense, Ui};
use aurea::{AureaResult, CursorIcon};

use super::parse::{Block, Span, Style};
use crate::app::Id;
use crate::document::ViewId;

/// widest the text column gets; past this lines are hard to follow.
const COLUMN: f32 = 760.0;
const PAD: f32 = 28.0;
const BODY: f32 = 15.0;
const MONO_SIZE: f32 = 13.0;
/// space between blocks.
const GAP: f32 = 12.0;
/// a list level's indent.
const INDENT: f32 = 22.0;
/// table columns never get narrower than this.
const MIN_COL: f32 = 60.0;

/// one view's page: what it was made from and how it was laid out.
#[derive(Default)]
pub struct Reader {
    /// the text revision the blocks came from.
    pub made_from: Option<resolvase::Revision>,
    /// a parse is out for this revision.
    pub asked: Option<resolvase::Revision>,
    pub blocks: Vec<(usize, Block)>,
    laid: Option<Laid>,
    pub scroll: f32,
    /// a source line to bring into sight once laid out.
    pub reveal_line: Option<usize>,
}

impl Reader {
    /// new blocks for `revision`; the layout is made again.
    pub fn set(&mut self, revision: resolvase::Revision, blocks: Vec<(usize, Block)>) {
        self.made_from = Some(revision);
        self.blocks = blocks;
        self.laid = None;
    }

    /// the source line at the top of the page, to go back to the text there.
    pub fn top_line(&self) -> Option<usize> {
        let laid = self.laid.as_ref()?;
        let i = laid
            .tops
            .partition_point(|y| *y <= self.scroll)
            .saturating_sub(1);
        self.blocks.get(i).map(|(line, _)| *line)
    }
}

/// the page at one width.
struct Laid {
    width: f32,
    /// each block's top, from the page top.
    tops: Vec<f32>,
    height: f32,
    items: Vec<Item>,
}

/// one thing drawn, placed from the page's top left.
enum Item {
    Text {
        x: f32,
        y: f32,
        text: String,
        font: Font,
        colour: Shade,
        link: Option<String>,
        strike: bool,
        /// a soft plate behind inline code.
        plate: bool,
    },
    Plate {
        rect: Rect,
        radius: f32,
        shade: Shade,
    },
    Bar {
        rect: Rect,
        shade: Shade,
    },
}

/// a colour by role, picked at draw time so a theme switch needs no layout.
#[derive(Clone, Copy)]
enum Shade {
    Text,
    Dim,
    Faint,
    Accent,
    Raised,
    Border,
}

impl Shade {
    fn of(self, p: &Palette) -> Color {
        match self {
            Shade::Text => p.text,
            Shade::Dim => p.dim,
            Shade::Faint => p.faint,
            Shade::Accent => p.accent,
            Shade::Raised => p.raised,
            Shade::Border => p.border,
        }
    }
}

/// what a click on the page asks for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Ask {
    Open(String),
}

struct Fonts {
    family: String,
}

impl Fonts {
    fn prose(&self, size: f32, style: Style) -> Font {
        if style.code {
            return Font::new(crate::editor::MONO, (size - 1.5).max(10.0));
        }
        let mut f = Font::new(&self.family, size);
        if style.strong {
            f = f.with_weight(FontWeight::Bold);
        }
        if style.emph {
            f = f.with_style(FontStyle::Italic);
        }
        f
    }
}

/// lays `spans` out from `x0` to `x1` starting at `y`, a word at a time.
/// gives the bottom.
#[allow(clippy::too_many_arguments)]
fn flow(
    ui: &mut Ui<'_, Id>,
    fonts: &Fonts,
    spans: &[Span],
    (x0, x1): (f32, f32),
    y: f32,
    (size, line_h): (f32, f32),
    (shade, bold): (Shade, bool),
    items: &mut Vec<Item>,
) -> f32 {
    let mut x = x0;
    let mut y = y;
    let mut wrote = false;
    for span in spans {
        let mut style = span.style;
        style.strong |= bold;
        let font = fonts.prose(size, style);
        let colour = if span.link.is_some() {
            Shade::Accent
        } else {
            shade
        };
        // words keep the space after them so runs join up
        for (n, piece) in span.text.split('\n').enumerate() {
            if n > 0 {
                x = x0;
                y += line_h;
            }
            // code stays one plate unless it can't fit a line
            let whole = style.code && ui.text_width(piece, &font) <= x1 - x0;
            let words: Vec<&str> = if whole {
                vec![piece]
            } else {
                piece.split_inclusive(' ').collect()
            };
            for word in words {
                let w = ui.text_width(word, &font);
                if x + w > x1 && x > x0 {
                    x = x0;
                    y += line_h;
                }
                let shown = if x == x0 { word.trim_start() } else { word };
                if shown.is_empty() {
                    continue;
                }
                items.push(Item::Text {
                    x,
                    y,
                    text: shown.to_owned(),
                    font: font.clone(),
                    colour,
                    link: span.link.clone(),
                    strike: style.strike,
                    plate: style.code,
                });
                x += if x == x0 {
                    ui.text_width(shown, &font)
                } else {
                    w
                };
                wrote = true;
            }
        }
    }
    if wrote { y + line_h } else { y }
}

fn lay(ui: &mut Ui<'_, Id>, blocks: &[(usize, Block)], width: f32) -> Laid {
    let fonts = Fonts {
        family: ui.theme.font(BODY).family,
    };
    let mono = Font::new(crate::editor::MONO, MONO_SIZE);
    let body_h = (BODY * 1.65).round();
    let mut items = Vec::new();
    let mut tops = Vec::with_capacity(blocks.len());
    let mut y = 0.0;
    for (_, block) in blocks {
        tops.push(y);
        y = lay_block(
            ui,
            &fonts,
            &mono,
            block,
            (0.0, width),
            y,
            body_h,
            &mut items,
        );
        y += GAP;
    }
    Laid {
        width,
        tops,
        height: y + PAD,
        items,
    }
}

#[allow(clippy::too_many_arguments)]
fn lay_block(
    ui: &mut Ui<'_, Id>,
    fonts: &Fonts,
    mono: &Font,
    block: &Block,
    (x0, x1): (f32, f32),
    y: f32,
    body_h: f32,
    items: &mut Vec<Item>,
) -> f32 {
    match block {
        Block::Heading { level, spans } => {
            let size = match level {
                1 => 28.0,
                2 => 22.0,
                3 => 18.5,
                4 => 16.0,
                _ => BODY,
            };
            let top = y + if *level <= 2 { 10.0 } else { 4.0 };
            let line_h = (size * 1.3).round();
            let shade = if *level >= 5 { Shade::Dim } else { Shade::Text };
            let bottom = flow(
                ui,
                fonts,
                spans,
                (x0, x1),
                top,
                (size, line_h),
                (shade, true),
                items,
            );
            if *level <= 2 {
                // a hairline under the big ones
                items.push(Item::Bar {
                    rect: Rect::new(x0, bottom + 4.0, x1 - x0, 1.0),
                    shade: Shade::Border,
                });
                return bottom + 8.0;
            }
            bottom
        }
        Block::Paragraph(spans) => flow(
            ui,
            fonts,
            spans,
            (x0, x1),
            y,
            (BODY, body_h),
            (Shade::Text, false),
            items,
        ),
        Block::Code { lang, lines } => {
            let row = (MONO_SIZE * 1.5).round();
            let top = y;
            let mut at = y + 12.0;
            if !lang.is_empty() {
                items.push(Item::Text {
                    x: x1 - 12.0 - ui.text_width(lang, mono),
                    y: y + 6.0,
                    text: lang.clone(),
                    font: mono.clone(),
                    colour: Shade::Faint,
                    link: None,
                    strike: false,
                    plate: false,
                });
            }
            let first = items.len();
            for line in lines.iter().take(5000) {
                items.push(Item::Text {
                    x: x0 + 14.0,
                    y: at,
                    text: line.clone(),
                    font: mono.clone(),
                    colour: Shade::Text,
                    link: None,
                    strike: false,
                    plate: false,
                });
                at += row;
            }
            let bottom = at + 12.0;
            // the plate goes under the lines
            items.insert(
                first.min(items.len()),
                Item::Plate {
                    rect: Rect::new(x0, top, x1 - x0, bottom - top),
                    radius: 8.0,
                    shade: Shade::Raised,
                },
            );
            bottom
        }
        Block::Quote(inner) => {
            let top = y;
            let mut at = y;
            for b in inner {
                at = lay_block(ui, fonts, mono, b, (x0 + 18.0, x1), at, body_h, items);
                at += GAP / 2.0;
            }
            items.push(Item::Bar {
                rect: Rect::new(x0 + 2.0, top + 2.0, 3.0, (at - top - GAP / 2.0).max(4.0)),
                shade: Shade::Accent,
            });
            at - GAP / 2.0
        }
        Block::Item {
            depth: level,
            marker,
            task,
            spans,
        } => {
            let x = x0 + f32::from(*level) * INDENT;
            let mark = match (task, marker) {
                (Some(true), _) => "☑".to_owned(),
                (Some(false), _) => "☐".to_owned(),
                (None, Some(n)) => format!("{n}."),
                (None, None) => if *level % 2 == 0 { "•" } else { "◦" }.to_owned(),
            };
            let font = fonts.prose(BODY, Style::default());
            let mw = ui.text_width(&mark, &font);
            items.push(Item::Text {
                x: x + INDENT - 8.0 - mw,
                y,
                text: mark,
                font,
                colour: Shade::Dim,
                link: None,
                strike: false,
                plate: false,
            });
            let done = *task == Some(true);
            let shade = if done { Shade::Dim } else { Shade::Text };
            flow(
                ui,
                fonts,
                spans,
                (x + INDENT, x1),
                y,
                (BODY, body_h),
                (shade, false),
                items,
            ) - GAP
                + 4.0
        }
        Block::Rule => {
            items.push(Item::Bar {
                rect: Rect::new(x0 + (x1 - x0) * 0.3, y + 10.0, (x1 - x0) * 0.4, 1.0),
                shade: Shade::Border,
            });
            y + 20.0
        }
        Block::Table { head, rows } => lay_table(ui, fonts, head, rows, (x0, x1), y, body_h, items),
        Block::Image { alt, src } => {
            let h = 44.0;
            items.push(Item::Plate {
                rect: Rect::new(x0, y, x1 - x0, h),
                radius: 8.0,
                shade: Shade::Raised,
            });
            let label = if alt.is_empty() {
                src.clone()
            } else {
                alt.clone()
            };
            items.push(Item::Text {
                x: x0 + 14.0,
                y: y + (h - body_h) / 2.0,
                text: format!("▣  {label}"),
                font: fonts.prose(BODY - 1.0, Style::default()),
                colour: Shade::Dim,
                link: Some(src.clone()),
                strike: false,
                plate: false,
            });
            y + h
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn lay_table(
    ui: &mut Ui<'_, Id>,
    fonts: &Fonts,
    head: &[Vec<Span>],
    rows: &[Vec<Vec<Span>>],
    (x0, x1): (f32, f32),
    y: f32,
    body_h: f32,
    items: &mut Vec<Item>,
) -> f32 {
    let cols = head
        .len()
        .max(rows.iter().map(Vec::len).max().unwrap_or(0))
        .clamp(1, 64);
    // columns share the room by how much text they hold
    let mut want = vec![MIN_COL; cols];
    let font = fonts.prose(BODY - 1.0, Style::default());
    for row in std::iter::once(head)
        .chain(rows.iter().map(Vec::as_slice))
        .take(200)
    {
        for (c, cell) in row.iter().enumerate().take(cols) {
            let text: String = cell.iter().map(|s| s.text.as_str()).collect();
            want[c] = want[c].max(ui.text_width(&text, &font) + 24.0);
        }
    }
    let total: f32 = want.iter().sum();
    let room = x1 - x0;
    let widths: Vec<f32> = if total <= room {
        want
    } else {
        want.iter()
            .map(|w| (w / total * room).max(MIN_COL.min(room / cols as f32)))
            .collect()
    };
    let line_h = ((BODY - 1.0) * 1.55).round();
    let mut at = y;
    let mut draw_row = |items: &mut Vec<Item>, cells: &[Vec<Span>], at: f32, bold: bool| -> f32 {
        let mut x = x0;
        let mut bottom = at + line_h;
        for (c, w) in widths.iter().enumerate() {
            if let Some(cell) = cells.get(c) {
                let b = flow(
                    ui,
                    fonts,
                    cell,
                    (x + 10.0, x + w - 10.0),
                    at + 6.0,
                    (BODY - 1.0, line_h),
                    (Shade::Text, bold),
                    items,
                );
                bottom = bottom.max(b + 6.0);
            }
            x += w;
        }
        bottom
    };
    let head_bottom = draw_row(items, head, at, true);
    items.push(Item::Bar {
        rect: Rect::new(x0, head_bottom, widths.iter().sum(), 1.0),
        shade: Shade::Border,
    });
    at = head_bottom + 1.0;
    for row in rows.iter().take(2000) {
        let b = draw_row(items, row, at, false);
        items.push(Item::Bar {
            rect: Rect::new(x0, b, widths.iter().sum(), 1.0),
            shade: Shade::Border,
        });
        at = b + 1.0;
    }
    let _ = body_h;
    at
}

/// draws the page in `rect`. gives what a click asked for.
pub fn show(
    ui: &mut Ui<'_, Id>,
    view: ViewId,
    reader: &mut Reader,
    rect: Rect,
) -> AureaResult<Option<Ask>> {
    let palette = ui.theme.palette;
    ui.fill(rect, palette.background)?;
    let width = (rect.width - 2.0 * PAD).clamp(120.0, COLUMN);
    if reader.laid.as_ref().is_none_or(|l| l.width != width) {
        reader.laid = Some(lay(ui, &reader.blocks, width));
    }
    let Some(laid) = reader.laid.as_ref() else {
        return Ok(None);
    };
    if let Some(line) = reader.reveal_line.take() {
        let i = reader
            .blocks
            .partition_point(|(l, _)| *l <= line)
            .saturating_sub(1);
        reader.scroll = laid.tops.get(i).copied().unwrap_or(0.0);
    }
    let height = laid.height + PAD;
    let mut ask = None;
    ScrollArea::new(&mut reader.scroll, height).show(
        ui,
        Id::Reader(view),
        rect,
        |ui, content, seen| {
            let left = content.x + ((content.width - width) / 2.0).max(PAD);
            let top = content.y + PAD;
            let pointer = ui.pointer();
            let mut n: u16 = 0;
            for item in &laid.items {
                match item {
                    Item::Plate {
                        rect: r,
                        radius,
                        shade,
                    } => {
                        let r = Rect::new(left + r.x, top + r.y, r.width, r.height);
                        if r.y + r.height >= seen.y && r.y <= seen.y + seen.height {
                            ui.round(r, *radius, shade.of(&palette))?;
                        }
                    }
                    Item::Bar { rect: r, shade } => {
                        let r = Rect::new(left + r.x, top + r.y, r.width, r.height);
                        if r.y + r.height >= seen.y && r.y <= seen.y + seen.height {
                            ui.fill(r, shade.of(&palette))?;
                        }
                    }
                    Item::Text {
                        x,
                        y,
                        text,
                        font,
                        colour,
                        link,
                        strike,
                        plate,
                    } => {
                        let line_h = (font.size * 1.65).round();
                        let at_y = top + y;
                        if at_y + line_h < seen.y || at_y > seen.y + seen.height {
                            continue;
                        }
                        let w = ui.text_width(text, font);
                        let r = Rect::new(left + x, at_y, w, line_h);
                        if *plate {
                            // the plate hugs the code, not the space after it
                            let inked = ui.text_width(text.trim_end(), font);
                            let back = Rect::new(r.x - 2.0, r.y + 3.0, inked + 4.0, line_h - 6.0);
                            ui.round(back, 4.0, palette.raised)?;
                        }
                        let mut c = colour.of(&palette);
                        if let Some(url) = link {
                            n = n.wrapping_add(1);
                            let input = ui.interact(Id::Reader(view), n, r, Sense::Click, false);
                            if input.hovered || pointer.is_some_and(|p| r.contains(p)) {
                                ui.set_cursor(CursorIcon::Pointer);
                                c = palette.text;
                            }
                            if input.clicked {
                                ask = Some(Ask::Open(url.clone()));
                            }
                            ui.fill(Rect::new(r.x, r.y + line_h * 0.78, w, 1.0), c.fade(0.6))?;
                        }
                        ui.text(text, r, font, c, Align::Start)?;
                        if *strike {
                            ui.fill(Rect::new(r.x, r.y + line_h * 0.5, w, 1.0), c)?;
                        }
                    }
                }
            }
            Ok(())
        },
    )?;
    Ok(ask)
}

/// readers by view; one exists while its view reads.
pub type Readers = HashMap<ViewId, Reader>;
