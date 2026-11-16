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

/// how tall a node is; rows are as tall as their tallest child.
fn height(node: &Node) -> f32 {
    match node {
        Node::Column(children) => {
            let inner: f32 = children.iter().map(height).sum();
            inner + GAP * children.len().saturating_sub(1) as f32
        }
        Node::Row(children) => children.iter().map(height).fold(0.0, f32::max),
        Node::Heading(_) => 24.0,
        Node::Text { text, .. } => text.lines().count().max(1) as f32 * LINE,
        Node::Button { .. } | Node::Toggle { .. } | Node::Field { .. } => CONTROL,
        Node::List { items, .. } => items.len() as f32 * ROW,
        Node::Table { rows, .. } => (rows.len() + 1) as f32 * ROW,
        Node::Separator => 9.0,
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
    let content = height(root);
    let ViewState {
        scroll,
        fields,
        toggles,
        picked,
    } = state;
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
            let at = Rect::new(at.x, at.y, at.width, content);
            node(ui, view, root, at, sight, &mut part, &mut kept, &mut done)
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
    kept: &mut Kept<'_>,
    done: &mut Done,
) -> AureaResult<()> {
    // every node takes a part number in order, drawn or not, so ids stay put
    let me = *part;
    *part = part.saturating_add(1);
    let id = Id::ExtView(view, me);
    if let Node::Column(children) | Node::Row(children) = n {
        let row = matches!(n, Node::Row(_));
        let share = if children.is_empty() {
            0.0
        } else {
            (at.width - GAP * (children.len() - 1) as f32) / children.len() as f32
        };
        let mut x = at.x;
        let mut y = at.y;
        for child in children {
            let h = height(child);
            let child_at = if row {
                Rect::new(x, at.y, share.max(0.0), h)
            } else {
                Rect::new(at.x, y, at.width, h)
            };
            node(ui, view, child, child_at, sight, part, kept, done)?;
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
        Node::Text { text, tone: t } => {
            let colour = tone(&palette, t);
            for (i, line) in text.lines().enumerate() {
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
            let (label_at, switch) = at.split_right(42.0, 8.0);
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
    fn heights_add_up() {
        let tree = Node::Column(vec![
            Node::Heading("h".into()),
            Node::Row(vec![
                Node::Separator,
                Node::Button {
                    id: "b".into(),
                    label: "B".into(),
                    command: None,
                },
            ]),
            Node::Text {
                text: "a\nb".into(),
                tone: Tone::Dim,
            },
        ]);
        assert_eq!(height(&tree), 24.0 + CONTROL + 2.0 * LINE + 2.0 * GAP);
        assert_eq!(height(&Node::Column(Vec::new())), 0.0);
    }
}
