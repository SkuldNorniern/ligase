//! draws a ui tree from an extension with aurea, and gives back what the
//! user did as plain events. the tree is data; nothing of aurea crosses to
//! the extension. what the user typed or toggled is kept here, per view.

use std::collections::HashMap;

use aurea::AureaResult;
use aurea::render::{Color, Rect};
use aurea::ui::{Align, Button, Palette, ScrollArea, TextField, Toggle, Ui};

use super::ui::{Event, Item, Node, Tone};
use crate::app::Id;

const GAP: f32 = 6.0;
const PAD: f32 = 10.0;
const CONTROL: f32 = 28.0;
const LINE: f32 = 18.0;
const ROW: f32 = 22.0;

/// what one view keeps between frames.
#[derive(Default)]
pub struct ViewState {
    pub scroll: f32,
    fields: HashMap<String, String>,
    toggles: HashMap<String, bool>,
    picked: HashMap<String, usize>,
    /// the tree measured at one width; measured again when it changes.
    layout: Option<Layout>,
}

/// heights and wrapped text of every node, by the node's place in order.
struct Layout {
    width: f32,
    heights: Vec<f32>,
    lines: HashMap<u16, Vec<String>>,
    markdown: HashMap<u16, Vec<MdRow>>,
}

/// one drawn row of a markdown node.
enum MdRow {
    Heading(String),
    Text(String),
    Code(String),
    Rule,
}

const CODE: f32 = 17.0;

impl MdRow {
    fn height(&self) -> f32 {
        match self {
            MdRow::Heading(_) => 24.0,
            MdRow::Text(_) => LINE,
            MdRow::Code(_) => CODE,
            MdRow::Rule => 9.0,
        }
    }
}

/// markdown laid out at `width`: paragraphs wrapped, code kept as lines.
fn markdown_rows(ui: &mut Ui<'_, Id>, text: &str, width: f32) -> Vec<MdRow> {
    use crate::editor::hover::{Block, blocks, plain};
    let small = ui.theme.font(12.0);
    let mut rows = Vec::new();
    for block in blocks(text) {
        match block {
            Block::Heading(h) => rows.push(MdRow::Heading(plain(&h))),
            Block::Text(t) => {
                for line in wrap(&plain(&t), width, |s| ui.text_width(s, &small)) {
                    rows.push(MdRow::Text(line));
                }
            }
            Block::Code(lines) => rows.extend(lines.into_iter().map(MdRow::Code)),
            Block::Rule => rows.push(MdRow::Rule),
        }
    }
    rows
}

/// `text` broken at spaces to fit `width`. a word wider than the width
/// gets a row of its own and is cut when drawn.
fn wrap(text: &str, width: f32, mut wide: impl FnMut(&str) -> f32) -> Vec<String> {
    let mut out = Vec::new();
    for line in text.split('\n') {
        let mut row = String::new();
        for word in line.split(' ') {
            let tried = if row.is_empty() {
                word.to_owned()
            } else {
                format!("{row} {word}")
            };
            if row.is_empty() || wide(&tried) <= width {
                row = tried;
            } else {
                out.push(std::mem::replace(&mut row, word.to_owned()));
            }
        }
        out.push(row);
    }
    out
}

/// measures `n` at `width`, children first in the order drawing numbers
/// them, so `heights[part]` is that node's height.
fn measure(ui: &mut Ui<'_, Id>, n: &Node, width: f32, layout: &mut Layout) -> f32 {
    let me = layout.heights.len();
    layout.heights.push(0.0);
    let h = match n {
        Node::Column(children) => {
            let mut total = 0.0;
            for child in children {
                total += measure(ui, child, width, layout);
            }
            total + GAP * children.len().saturating_sub(1) as f32
        }
        Node::Row(children) => {
            let share = share(width, children.len());
            let mut tallest: f32 = 0.0;
            for child in children {
                tallest = tallest.max(measure(ui, child, share, layout));
            }
            tallest
        }
        Node::Heading(_) => 24.0,
        Node::Markdown(text) => {
            let rows = markdown_rows(ui, text, width);
            let h = rows.iter().map(MdRow::height).sum::<f32>();
            layout
                .markdown
                .insert(u16::try_from(me).unwrap_or(u16::MAX), rows);
            h
        }
        Node::Text { text, .. } => {
            let small = ui.theme.font(12.0);
            let lines = wrap(text, width, |s| ui.text_width(s, &small));
            let h = lines.len().max(1) as f32 * LINE;
            layout
                .lines
                .insert(u16::try_from(me).unwrap_or(u16::MAX), lines);
            h
        }
        Node::Button { .. } | Node::Toggle { .. } | Node::Field { .. } => CONTROL,
        Node::List { items, .. } => items.len() as f32 * ROW,
        Node::Table { rows, .. } => (rows.len() + 1) as f32 * ROW,
        Node::Separator => 9.0,
    };
    layout.heights[me] = h;
    h
}

/// each child's width in a row of `n`.
fn share(width: f32, n: usize) -> f32 {
    if n == 0 {
        0.0
    } else {
        ((width - GAP * (n - 1) as f32) / n as f32).max(0.0)
    }
}

/// what came of a frame: events for the owner, commands to run by id.
#[derive(Default, Debug, PartialEq)]
pub struct Done {
    pub events: Vec<Event>,
    pub commands: Vec<String>,
}

fn tone(palette: &Palette, tone: &Tone) -> Color {
    match tone {
        Tone::Plain => palette.text,
        Tone::Dim => palette.dim,
        Tone::Good => palette.success,
        Tone::Warn => palette.warning,
        Tone::Bad => palette.danger,
    }
}

/// draws `root` filling `rect`, scrolling when taller. `view` tells the
/// views apart for widget ids.
pub fn show(
    ui: &mut Ui<'_, Id>,
    view: u16,
    root: &Node,
    state: &mut ViewState,
    rect: Rect,
) -> AureaResult<Done> {
    let mut done = Done::default();
    let inner = rect.inset(PAD);
    // the scroll bar's room is left whatever the height, so the width the
    // text wraps at doesn't swing with it
    let width = (inner.width - 12.0).max(0.0);
    if state.layout.as_ref().is_none_or(|l| l.width != width) {
        let mut layout = Layout {
            width,
            heights: Vec::new(),
            lines: HashMap::new(),
            markdown: HashMap::new(),
        };
        measure(ui, root, width, &mut layout);
        state.layout = Some(layout);
    }
    let ViewState {
        scroll,
        fields,
        toggles,
        picked,
        layout,
    } = state;
    let Some(layout) = layout.as_ref() else {
        return Ok(done);
    };
    let content = layout.heights.first().copied().unwrap_or(0.0);
    let mut kept = Kept {
        fields,
        toggles,
        picked,
    };
    ScrollArea::new(scroll, content).show(
        ui,
        Id::ExtView(view, u16::MAX),
        inner,
        |ui, at, sight| {
            let mut part = 0u16;
            let at = Rect::new(at.x, at.y, width, content);
            node(
                ui, view, root, at, sight, &mut part, layout, &mut kept, &mut done,
            )
        },
    )?;
    Ok(done)
}

struct Kept<'a> {
    fields: &'a mut HashMap<String, String>,
    toggles: &'a mut HashMap<String, bool>,
    picked: &'a mut HashMap<String, usize>,
}

#[allow(clippy::too_many_arguments)]
fn node(
    ui: &mut Ui<'_, Id>,
    view: u16,
    n: &Node,
    at: Rect,
    sight: Rect,
    part: &mut u16,
    layout: &Layout,
    kept: &mut Kept<'_>,
    done: &mut Done,
) -> AureaResult<()> {
    // every node takes a part number in order, drawn or not, so ids stay put
    let me = *part;
    *part = part.saturating_add(1);
    let id = Id::ExtView(view, me);
    if let Node::Column(children) | Node::Row(children) = n {
        let row = matches!(n, Node::Row(_));
        let share = share(at.width, children.len());
        let mut x = at.x;
        let mut y = at.y;
        for child in children {
            let h = layout
                .heights
                .get(usize::from(*part))
                .copied()
                .unwrap_or(0.0);
            let child_at = if row {
                Rect::new(x, at.y, share, h)
            } else {
                Rect::new(at.x, y, at.width, h)
            };
            node(ui, view, child, child_at, sight, part, layout, kept, done)?;
            x += share + GAP;
            y += h + GAP;
        }
        return Ok(());
    }
    if at.bottom() < sight.y || at.y > sight.bottom() {
        return Ok(());
    }
    let palette = ui.theme.palette;
    let font = ui.theme.font(13.0);
    let small = ui.theme.font(12.0);
    match n {
        Node::Column(_) | Node::Row(_) => {}
        Node::Heading(text) => {
            let big = ui.theme.font(14.5);
            ui.text_clipped(text, at, &big, palette.text, Align::Start)?;
        }
        Node::Markdown(_) => {
            let big = ui.theme.font(13.5);
            let mono = aurea::render::Font::new(crate::editor::MONO, 12.5);
            let mut y = at.y;
            for row in layout.markdown.get(&me).map_or(&[][..], Vec::as_slice) {
                let h = row.height();
                let line = Rect::new(at.x, y, at.width, h);
                y += h;
                if line.bottom() < sight.y || line.y > sight.bottom() {
                    continue;
                }
                match row {
                    MdRow::Heading(t) => {
                        ui.text_clipped(t, line, &big, palette.text, Align::Start)?;
                    }
                    MdRow::Text(t) => {
                        ui.text_clipped(t, line, &small, palette.text, Align::Start)?;
                    }
                    MdRow::Code(t) => {
                        // code sits on a faint plate, like the hover's
                        ui.fill(line, palette.surface)?;
                        let inner = line.inset_xy(6.0, 0.0);
                        ui.text_clipped(t, inner, &mono, palette.text, Align::Start)?;
                    }
                    MdRow::Rule => {
                        ui.fill(Rect::new(at.x, line.y + 4.0, at.width, 1.0), palette.border)?;
                    }
                }
            }
        }
        Node::Text { text, tone: t } => {
            let colour = tone(&palette, t);
            let fallback: Vec<String>;
            let lines = match layout.lines.get(&me) {
                Some(lines) => lines,
                None => {
                    fallback = text.lines().map(str::to_owned).collect();
                    &fallback
                }
            };
            for (i, line) in lines.iter().enumerate() {
                let line_at = Rect::new(at.x, at.y + i as f32 * LINE, at.width, LINE);
                ui.text_clipped(line, line_at, &small, colour, Align::Start)?;
            }
        }
        Node::Button {
            id: name,
            label,
            command,
        } => {
            if Button::new(label).show(ui, id, at)?.clicked {
                match command {
                    Some(command) => done.commands.push(command.clone()),
                    None => done.events.push(Event::Clicked(name.clone())),
                }
            }
        }
        Node::Toggle {
            id: name,
            label,
            on,
        } => {
            let value = kept.toggles.entry(name.clone()).or_insert(*on);
            let (switch, label_at) = at.split_right(42.0, 8.0);
            let switch = Rect::new(switch.x, switch.y + 2.0, switch.width, switch.height - 4.0);
            ui.text_clipped(label, label_at, &font, palette.text, Align::Start)?;
            if Toggle::new(value).show(ui, id, switch)?.changed {
                done.events.push(Event::Toggled(name.clone(), *value));
            }
        }
        Node::Field {
            id: name,
            placeholder,
            value,
        } => {
            let text = kept
                .fields
                .entry(name.clone())
                .or_insert_with(|| value.clone());
            let typed = TextField::new(text)
                .placeholder(placeholder)
                .max_chars(super::ui::MAX_TEXT)
                .show(ui, id, at)?;
            if typed.submitted {
                done.events
                    .push(Event::Submitted(name.clone(), text.clone()));
            }
        }
        Node::List { id: name, items } => {
            let lit = kept.picked.get(name).copied();
            for (
                i,
                Item {
                    label,
                    detail,
                    tone: t,
                },
            ) in items.iter().enumerate()
            {
                let row = Rect::new(at.x, at.y + i as f32 * ROW, at.width, ROW);
                if row.bottom() < sight.y || row.y > sight.bottom() {
                    continue;
                }
                let part = u16::try_from(i).unwrap_or(u16::MAX);
                let input = ui.interact(
                    Id::ExtRow(view, me),
                    part,
                    row,
                    aurea::ui::Sense::Click,
                    true,
                );
                if lit == Some(i) {
                    ui.fill(row, palette.selection)?;
                } else if input.hovered {
                    ui.fill(row, palette.hover)?;
                }
                let text = row.inset_xy(6.0, 0.0);
                let label_w = ui.text_width(label, &font).min(text.width);
                let (label_at, detail_at) = text.split_left(label_w, 10.0);
                ui.text_clipped(label, label_at, &font, tone(&palette, t), Align::Start)?;
                ui.text_clipped(detail, detail_at, &small, palette.dim, Align::Start)?;
                if input.clicked {
                    kept.picked.insert(name.clone(), i);
                    done.events.push(Event::Picked(name.clone(), i));
                }
            }
        }
        Node::Table {
            id: name,
            columns,
            rows,
        } => {
            let cols = columns.len().max(1) as f32;
            let w = at.width / cols;
            let cell = |i: usize, y: f32| Rect::new(at.x + i as f32 * w + 4.0, y, w - 8.0, ROW);
            for (i, title) in columns.iter().enumerate() {
                ui.text_clipped(title, cell(i, at.y), &small, palette.dim, Align::Start)?;
            }
            ui.fill(
                Rect::new(at.x, at.y + ROW - 1.0, at.width, 1.0),
                palette.border,
            )?;
            let lit = kept.picked.get(name).copied();
            for (r, cells) in rows.iter().enumerate() {
                let y = at.y + (r + 1) as f32 * ROW;
                let row = Rect::new(at.x, y, at.width, ROW);
                if row.bottom() < sight.y || row.y > sight.bottom() {
                    continue;
                }
                let part = u16::try_from(r).unwrap_or(u16::MAX);
                let input = ui.interact(
                    Id::ExtRow(view, me),
                    part,
                    row,
                    aurea::ui::Sense::Click,
                    true,
                );
                if lit == Some(r) {
                    ui.fill(row, palette.selection)?;
                } else if input.hovered {
                    ui.fill(row, palette.hover)?;
                }
                for (i, text) in cells.iter().enumerate() {
                    ui.text_clipped(text, cell(i, y), &small, palette.text, Align::Start)?;
                }
                if input.clicked {
                    kept.picked.insert(name.clone(), r);
                    done.events.push(Event::Picked(name.clone(), r));
                }
            }
        }
        Node::Separator => {
            ui.fill(Rect::new(at.x, at.y + 4.0, at.width, 1.0), palette.border)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_wraps_at_spaces_and_keeps_breaks() {
        let wide = |s: &str| s.chars().count() as f32;
        assert_eq!(
            wrap("one two three four", 9.0, wide),
            ["one two", "three", "four"]
        );
        assert_eq!(wrap("a\nb c", 9.0, wide), ["a", "b c"]);
        assert_eq!(
            wrap("unbreakableword x", 4.0, wide),
            ["unbreakableword", "x"]
        );
        assert_eq!(wrap("", 4.0, wide), [""]);
    }

    #[test]
    fn rows_share_their_width() {
        assert_eq!(share(100.0, 2), (100.0 - GAP) / 2.0);
        assert_eq!(share(100.0, 0), 0.0);
        assert_eq!(share(4.0, 3), 0.0, "never below zero");
    }
}
