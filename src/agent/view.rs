//! the agent panel drawn: a header with the agent and its state, the
//! conversation, a permission card when the agent waits on one, and the
//! box to write in with the files put before the message. rows are laid
//! out once per entry and width, and only the ones in sight are drawn.

use std::collections::HashMap;
use std::path::PathBuf;

use aurea::AureaResult;
use aurea::CursorIcon;
use aurea::render::{Color, Font, Rect};
use aurea::ui::{Align, Button, ScrollArea, Sense, TextArea, Ui};

use super::{Chat, Entry, State, Step};
use crate::app::Id;
use crate::editor::hover;
use crate::lang::Texts;

const PAD: f32 = 12.0;
const HEAD: f32 = 36.0;
const GAP: f32 = 12.0;
const TEXT_ROW: f32 = 19.0;
const CODE_ROW: f32 = 18.0;
const HEAD_ROW: f32 = 24.0;
const INPUT: f32 = 72.0;
const CHIP: f32 = 22.0;
/// thought lines shown before the rest is folded away.
const THOUGHT_ROWS: usize = 4;

/// what a click in the panel asks for.
#[derive(Debug, PartialEq)]
pub enum Ask {
    Send,
    Stop,
    /// a new conversation, the old one let go.
    New,
    /// start the agent again after it failed.
    Start,
    Answer(String),
    SignIn(usize),
    /// try the session again after a sign in somewhere else.
    Retry,
    Open(PathBuf, Option<u32>),
    /// take the file at this place off the next message.
    Unref(usize),
}

#[derive(Clone, Debug)]
enum Row {
    Text(String),
    Head(String),
    Code(String),
    Dim(String),
    /// a file the tool touches, clickable.
    Place(String, PathBuf, Option<u32>),
    /// a tool's own line: its mark, colour key and title.
    Tool(char, &'static str, String),
    Step(Step, String),
}

/// laid out rows per entry: (width, text length, rows, height).
#[derive(Default)]
pub struct Cache(HashMap<usize, (f32, usize, Vec<Row>, f32)>);

impl Cache {
    pub fn forget(&mut self, i: usize) {
        self.0.remove(&i);
    }

    pub fn clear(&mut self) {
        self.0.clear();
    }
}

fn row_height(row: &Row) -> f32 {
    match row {
        Row::Code(_) => CODE_ROW,
        Row::Head(_) | Row::Tool(..) => HEAD_ROW,
        _ => TEXT_ROW,
    }
}

/// a length that changes when a streamed entry grows.
fn size_of(entry: &Entry) -> usize {
    match entry {
        Entry::User { text, refs } => text.len() + refs.len(),
        Entry::Agent(t) | Entry::Thought(t) => t.len(),
        Entry::Note { text, .. } => text.len(),
        Entry::Tool(_) | Entry::Plan(_) => 0,
    }
}

fn markdown_rows(ui: &mut Ui<'_, Id>, text: &str, font: &Font, width: f32, out: &mut Vec<Row>) {
    for block in hover::blocks(text) {
        match block {
            hover::Block::Code(lines) => out.extend(lines.into_iter().map(Row::Code)),
            hover::Block::Text(t) => {
                out.extend(hover::wrap(ui, &t, font, width).into_iter().map(Row::Text))
            }
            hover::Block::Heading(t) => out.push(Row::Head(t)),
            hover::Block::Rule => {}
        }
    }
}

fn lay(ui: &mut Ui<'_, Id>, entry: &Entry, width: f32, root: &std::path::Path) -> Vec<Row> {
    let font = ui.theme.font(13.0);
    let mut rows = Vec::new();
    match entry {
        Entry::User { text, refs } => {
            if !refs.is_empty() {
                let names: Vec<String> = refs.iter().map(super::Ref::label).collect();
                rows.push(Row::Dim(format!("@ {}", names.join("  "))));
            }
            for line in text.lines() {
                rows.extend(
                    hover::wrap(ui, line, &font, width - 20.0)
                        .into_iter()
                        .map(Row::Text),
                );
            }
        }
        Entry::Agent(text) => markdown_rows(ui, text, &font, width, &mut rows),
        Entry::Thought(text) => {
            let mut all = Vec::new();
            for line in text.lines() {
                all.extend(hover::wrap(ui, line, &font, width - 12.0));
            }
            let more = all.len() > THOUGHT_ROWS;
            rows.extend(all.into_iter().take(THOUGHT_ROWS).map(Row::Dim));
            if more {
                rows.push(Row::Dim("…".to_owned()));
            }
        }
        Entry::Tool(t) => {
            let (mark, tone) = match t.status.as_str() {
                "completed" => ('✓', "ok"),
                "failed" => ('✗', "bad"),
                "in_progress" => ('◐', "busy"),
                _ => ('○', "dim"),
            };
            let title = if t.title.is_empty() {
                t.kind.clone()
            } else {
                t.title.clone()
            };
            rows.push(Row::Tool(mark, tone, title));
            let short = |p: &std::path::Path| {
                p.strip_prefix(root)
                    .unwrap_or(p)
                    .to_string_lossy()
                    .replace('\\', "/")
            };
            for (path, gone, added) in t.diffs.iter().take(6) {
                rows.push(Row::Place(
                    format!("{}  +{added} −{gone}", short(path)),
                    path.clone(),
                    None,
                ));
            }
            if t.diffs.is_empty() {
                for (path, line) in t.places.iter().take(4) {
                    let label = match line {
                        Some(l) => format!("{}:{l}", short(path)),
                        None => short(path),
                    };
                    rows.push(Row::Place(label, path.clone(), *line));
                }
            }
            for line in t.output.lines().filter(|l| !l.trim().is_empty()).take(3) {
                rows.extend(
                    hover::wrap(ui, line, &font, width - 24.0)
                        .into_iter()
                        .take(1)
                        .map(Row::Dim),
                );
            }
        }
        Entry::Plan(steps) => {
            rows.push(Row::Head("Plan".to_owned()));
            for (text, step) in steps {
                for (i, line) in hover::wrap(ui, text, &font, width - 24.0)
                    .into_iter()
                    .enumerate()
                {
                    rows.push(if i == 0 {
                        Row::Step(*step, line)
                    } else {
                        Row::Dim(line)
                    });
                }
            }
        }
        Entry::Note { text, .. } => {
            for line in text.lines().take(12) {
                rows.extend(
                    hover::wrap(ui, line, &font, width)
                        .into_iter()
                        .map(Row::Dim),
                );
            }
        }
    }
    rows
}

/// the panel in `rect`. gives what was clicked.
pub fn show(
    ui: &mut Ui<'_, Id>,
    chat: &mut Chat,
    rect: Rect,
    texts: &Texts,
) -> AureaResult<Option<Ask>> {
    let palette = ui.theme.palette;
    let font = ui.theme.font(13.0);
    let small = ui.theme.font(12.0);
    let bold = ui
        .theme
        .font(13.0)
        .with_weight(aurea::render::FontWeight::Bold);
    let mono = Font::new(crate::editor::MONO, 12.5);
    ui.fill(rect, palette.surface)?;
    let mut ask = None;

    // header: the agent, its state, a new conversation
    let (head, rest) = rect.split_top(HEAD, 0.0);
    ui.fill(
        Rect::new(head.x, head.bottom() - 1.0, head.width, 1.0),
        palette.border,
    )?;
    let icon = Rect::new(head.x + PAD, head.y + (HEAD - 16.0) / 2.0, 16.0, 16.0);
    crate::icons::draw(ui, "agent", icon, palette.accent)?;
    // the name, its first word when narrow, none when narrower still
    let room = head.right() - PAD - 56.0 - 8.0 - (icon.right() + 8.0);
    let full = chat.agent.name();
    let short = full.split(' ').next().unwrap_or(full);
    let shown = [full, short]
        .into_iter()
        .find(|n| ui.text_width(n, &bold) <= room)
        .unwrap_or("");
    let name = Rect::new(icon.right() + 8.0, head.y, room.max(0.0), HEAD);
    ui.text(shown, name, &bold, palette.text, Align::Start)?;
    let name_w = ui.text_width(shown, &bold);
    let (state, tone) = match &chat.state {
        State::Off => (texts.tr("not started"), palette.faint),
        State::Starting => (texts.tr("starting…"), palette.dim),
        State::SignIn(_) => (texts.tr("sign in"), palette.warning),
        State::Ready => (texts.tr("ready"), palette.success),
        State::Working => (texts.tr("working…"), palette.accent),
        State::Failed(_) => (texts.tr("stopped"), palette.danger),
    };
    let new = Rect::new(head.right() - PAD - 56.0, head.y + 6.0, 56.0, HEAD - 12.0);
    // the state's dot always; its word only where it fits before the button
    let dot = Rect::new(name.x + name_w + 10.0, head.y + HEAD / 2.0 - 3.0, 6.0, 6.0);
    if dot.right() < new.x - 4.0 {
        ui.round(dot, 3.0, tone)?;
        let room = new.x - 8.0 - (dot.right() + 6.0);
        if ui.text_width(state, &small) <= room {
            let state_at = Rect::new(dot.right() + 6.0, head.y, room, HEAD);
            ui.text(state, state_at, &small, palette.dim, Align::Start)?;
        }
    }
    if Button::new(texts.tr("New"))
        .quiet()
        .show(ui, Id::AgentButton(0), new)?
        .clicked
    {
        ask = Some(Ask::New);
    }

    // the box to write in, and what goes with it, at the bottom
    let refs_h = if chat.refs.is_empty() {
        0.0
    } else {
        CHIP + 8.0
    };
    // split_bottom gives the bottom part first
    let (foot, talk) = rest.split_bottom(INPUT + refs_h + 44.0 + PAD, 0.0);
    ui.fill(Rect::new(foot.x, foot.y, foot.width, 1.0), palette.border)?;
    let foot = foot.inset(PAD);
    let mut y = foot.y;
    if !chat.refs.is_empty() {
        let mut x = foot.x;
        for (i, r) in chat.refs.iter().enumerate() {
            let label = format!("@ {}  ×", r.label());
            let w = ui.text_width(&label, &small) + 16.0;
            let chip = Rect::new(x, y, w, CHIP);
            let n = u16::try_from(i + 1).unwrap_or(u16::MAX);
            let input = ui.interact(Id::AgentRef(n), 0, chip, Sense::Click, true);
            ui.round(
                chip,
                6.0,
                if input.hovered {
                    palette.hover
                } else {
                    palette.raised
                },
            )?;
            ui.text(&label, chip, &small, palette.dim, Align::Center)?;
            if input.hovered {
                ui.set_cursor(CursorIcon::Pointer);
            }
            if input.clicked {
                ask = Some(Ask::Unref(i));
            }
            x += w + 6.0;
            if x > foot.right() - 60.0 {
                break;
            }
        }
        y += CHIP + 8.0;
    }
    let field = Rect::new(foot.x, y, foot.width, INPUT);
    let asking = format!("{} {}…", texts.tr("Ask"), chat.agent.name());
    let hint = match chat.state {
        State::Working => texts.tr("Working… Stop to break in"),
        _ => asking.as_str(),
    };
    let typed = TextArea::new(&mut chat.input)
        .placeholder(hint)
        .max_chars(20_000)
        .show(ui, Id::AgentInput, field)?;
    if typed.submitted && chat.state != State::Working {
        ask = Some(Ask::Send);
    }
    let buttons = Rect::new(foot.x, field.bottom() + 8.0, foot.width, 28.0);
    let go = Rect::new(buttons.right() - 76.0, buttons.y, 76.0, 28.0);
    // the keys, quietly, where there is room left of the button
    let keys = texts.tr("Enter to send · Ctrl+Shift+J adds lines");
    let keys_at = Rect::new(buttons.x, buttons.y, go.x - buttons.x - 8.0, 28.0);
    if ui.text_width(keys, &small) <= keys_at.width {
        ui.text(keys, keys_at, &small, palette.faint, Align::Start)?;
    }
    if chat.state == State::Working {
        if Button::new(texts.tr("Stop"))
            .show(ui, Id::AgentButton(1), go)?
            .clicked
        {
            ask = Some(Ask::Stop);
        }
    } else if Button::new(texts.tr("Send"))
        .primary()
        .enabled(!chat.input.trim().is_empty())
        .show(ui, Id::AgentButton(2), go)?
        .clicked
    {
        ask = Some(Ask::Send);
    }

    // a permission the agent waits on sits over the box
    let talk = if let Some(p) = &chat.permission {
        let lines = hover::wrap(ui, &p.title, &font, talk.width - 2.0 * PAD - 20.0);
        let card_h = 30.0 + lines.len().min(4) as f32 * TEXT_ROW + 40.0;
        let (card, above) = talk.split_bottom(card_h + PAD, 0.0);
        let card = Rect::new(card.x + PAD, card.y, card.width - 2.0 * PAD, card_h);
        ui.round(card, 8.0, palette.raised)?;
        ui.fill(
            Rect::new(card.x, card.y + 8.0, 3.0, card.height - 16.0),
            palette.warning,
        )?;
        let mut cy = card.y + 8.0;
        ui.text(
            texts.tr("The agent asks"),
            Rect::new(card.x + 12.0, cy, card.width, 20.0),
            &small,
            palette.dim,
            Align::Start,
        )?;
        cy += 22.0;
        for line in lines.iter().take(4) {
            ui.text(
                line,
                Rect::new(card.x + 12.0, cy, card.width - 24.0, TEXT_ROW),
                &font,
                palette.text,
                Align::Start,
            )?;
            cy += TEXT_ROW;
        }
        let mut bx = card.x + 12.0;
        for (i, (id, label, allows)) in p.options.iter().enumerate() {
            let w = ui.text_width(label, &small) + 24.0;
            let b = Rect::new(bx, card.bottom() - 36.0, w, 26.0);
            let n = u16::try_from(10 + i).unwrap_or(u16::MAX);
            let button = if *allows {
                Button::new(label).primary()
            } else {
                Button::new(label)
            };
            if button.show(ui, Id::AgentButton(n), b)?.clicked {
                ask = Some(Ask::Answer(id.clone()));
            }
            bx += w + 6.0;
        }
        above
    } else {
        talk
    };

    // nothing said yet: what this is, or what is in the way
    if chat.entries.is_empty() {
        let mid = talk.inset(PAD * 2.0);
        let mut cy = mid.y + 24.0;
        let line = |ui: &mut Ui<'_, Id>, text: &str, y: f32, f: &Font, c: Color| {
            ui.text(
                text,
                Rect::new(mid.x, y, mid.width, 20.0),
                f,
                c,
                Align::Start,
            )
        };
        line(ui, chat.agent.name(), cy, &bold, palette.text)?;
        cy += 24.0;
        for l in hover::wrap(
            ui,
            texts.tr("Ask it to read, change or run things in this folder. It signs in on its own; Ligase never sees its keys. Edits land in open files and undo like your own."),
            &small,
            mid.width,
        ) {
            line(ui, &l, cy, &small, palette.dim)?;
            cy += 18.0;
        }
        cy += 12.0;
        if let State::SignIn(ways) = &chat.state {
            for (i, way) in ways.iter().enumerate() {
                let b = Rect::new(mid.x, cy, 220.0_f32.min(mid.width), 28.0);
                let n = u16::try_from(30 + i).unwrap_or(u16::MAX);
                if Button::new(&way.name)
                    .primary()
                    .show(ui, Id::AgentButton(n), b)?
                    .clicked
                {
                    ask = Some(Ask::SignIn(i));
                }
                cy += 34.0;
            }
            let b = Rect::new(mid.x, cy, 220.0_f32.min(mid.width), 28.0);
            if Button::new(texts.tr("Signed in, try again"))
                .show(ui, Id::AgentButton(3), b)?
                .clicked
            {
                ask = Some(Ask::Retry);
            }
        }
        if chat.state == State::Off {
            let b = Rect::new(mid.x, cy, 160.0_f32.min(mid.width), 28.0);
            if Button::new(texts.tr("Start"))
                .primary()
                .show(ui, Id::AgentButton(5), b)?
                .clicked
            {
                ask = Some(Ask::Start);
            }
        }
        if let State::Failed(why) = &chat.state {
            for l in hover::wrap(ui, why, &mono, mid.width).into_iter().take(10) {
                line(ui, &l, cy, &mono, palette.danger)?;
                cy += 17.0;
            }
            let b = Rect::new(mid.x, cy + 8.0, 160.0_f32.min(mid.width), 28.0);
            if Button::new(texts.tr("Start again"))
                .show(ui, Id::AgentButton(4), b)?
                .clicked
            {
                ask = Some(Ask::Start);
            }
        }
        return Ok(ask);
    }

    // the conversation: lay out what changed, then draw what is in sight
    let width = (talk.width - 2.0 * PAD).max(80.0);
    let root = chat.root.clone();
    let mut tops = Vec::with_capacity(chat.entries.len());
    let mut total = PAD;
    for (i, entry) in chat.entries.iter().enumerate() {
        let size = size_of(entry);
        let fresh = chat
            .laid
            .0
            .get(&i)
            .is_some_and(|(w, s, ..)| *w == width && *s == size);
        if !fresh {
            let rows = lay(ui, entry, width, &root);
            let h: f32 = rows.iter().map(row_height).sum();
            let pad = if matches!(entry, Entry::User { .. } | Entry::Plan(_)) {
                20.0
            } else {
                0.0
            };
            chat.laid.0.insert(i, (width, size, rows, h + pad));
        }
        tops.push(total);
        total += chat.laid.0.get(&i).map_or(0.0, |l| l.3) + GAP;
    }
    let bottom = (total - talk.height).max(0.0);
    if chat.follow {
        chat.scroll = bottom;
    }
    let entries = &chat.entries;
    let laid = &chat.laid;
    ScrollArea::new(&mut chat.scroll, total).show(
        ui,
        Id::AgentList,
        talk,
        |ui, content, seen| {
            for (i, entry) in entries.iter().enumerate() {
                let Some((_, _, rows, h)) = laid.0.get(&i) else {
                    continue;
                };
                let top = content.y + tops[i];
                if top > seen.bottom() || top + h < seen.y {
                    continue;
                }
                let x = content.x + PAD;
                let mut y = top;
                match entry {
                    Entry::User { .. } => {
                        ui.round(Rect::new(x, y, width, *h), 8.0, palette.raised)?;
                        y += 10.0;
                    }
                    Entry::Plan(_) => {
                        ui.round(Rect::new(x, y, width, *h), 8.0, palette.background)?;
                        y += 10.0;
                    }
                    _ => {}
                }
                let inset = match entry {
                    Entry::User { .. } | Entry::Plan(_) => 10.0,
                    Entry::Thought(_) => 10.0,
                    _ => 0.0,
                };
                if matches!(entry, Entry::Thought(_)) {
                    ui.fill(Rect::new(x, y + 2.0, 2.0, h - 4.0), palette.border)?;
                }
                for (r, row) in rows.iter().enumerate() {
                    let rh = row_height(row);
                    let at = Rect::new(x + inset, y, width - inset, rh);
                    if at.bottom() >= seen.y && at.y <= seen.bottom() {
                        match row {
                            Row::Text(t) => {
                                let c = if matches!(entry, Entry::Note { bad: true, .. }) {
                                    palette.danger
                                } else {
                                    palette.text
                                };
                                ui.text(t, at, &font, c, Align::Start)?;
                            }
                            Row::Head(t) => ui.text(t, at, &bold, palette.text, Align::Start)?,
                            Row::Code(t) => {
                                ui.fill(Rect::new(x, y, width, rh), palette.background)?;
                                ui.text(
                                    t,
                                    Rect::new(x + 8.0, y, width - 8.0, rh),
                                    &mono,
                                    palette.text,
                                    Align::Start,
                                )?;
                            }
                            Row::Dim(t) => {
                                let c = match entry {
                                    Entry::Note { bad: true, .. } => palette.danger,
                                    _ => palette.dim,
                                };
                                ui.text(t, at, &small, c, Align::Start)?;
                            }
                            Row::Tool(mark, tone, title) => {
                                let c = match *tone {
                                    "ok" => palette.success,
                                    "bad" => palette.danger,
                                    "busy" => palette.accent,
                                    _ => palette.faint,
                                };
                                ui.text(
                                    &mark.to_string(),
                                    Rect::new(x, y, 16.0, rh),
                                    &font,
                                    c,
                                    Align::Center,
                                )?;
                                ui.text(
                                    title,
                                    Rect::new(x + 20.0, y, width - 20.0, rh),
                                    &font,
                                    palette.text,
                                    Align::Start,
                                )?;
                            }
                            Row::Step(step, t) => {
                                let (mark, c) = match step {
                                    Step::Done => ("☑", palette.success),
                                    Step::Doing => ("◐", palette.accent),
                                    Step::Pending => ("☐", palette.faint),
                                };
                                ui.text(
                                    mark,
                                    Rect::new(at.x, y, 18.0, rh),
                                    &font,
                                    c,
                                    Align::Start,
                                )?;
                                let tc = if *step == Step::Done {
                                    palette.dim
                                } else {
                                    palette.text
                                };
                                ui.text(
                                    t,
                                    Rect::new(at.x + 22.0, y, at.width - 22.0, rh),
                                    &font,
                                    tc,
                                    Align::Start,
                                )?;
                            }
                            Row::Place(label, path, line) => {
                                let link = Rect::new(
                                    x + 20.0,
                                    y,
                                    ui.text_width(label, &small).min(width - 20.0),
                                    rh,
                                );
                                // the entry names the widget, the row its part
                                let n = u16::try_from(i % usize::from(u16::MAX)).unwrap_or(0);
                                let part = u16::try_from(r).unwrap_or(u16::MAX);
                                let input =
                                    ui.interact(Id::AgentLink(n), part, link, Sense::Click, true);
                                let c = if input.hovered {
                                    palette.accent
                                } else {
                                    palette.dim
                                };
                                ui.text(label, link, &small, c, Align::Start)?;
                                if input.hovered {
                                    ui.set_cursor(CursorIcon::Pointer);
                                }
                                if input.clicked {
                                    ask = Some(Ask::Open(path.clone(), *line));
                                }
                            }
                        }
                    }
                    y += rh;
                }
            }
            Ok(())
        },
    )?;
    // scrolled up: stop following; back at the bottom: follow again
    chat.follow = chat.scroll >= bottom - 4.0;
    Ok(ask)
}
