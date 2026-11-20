//! the agent panel drawn: a header with the agent and its state, the
//! conversation, a permission card when the agent waits on one, and the
//! box to write in with the files put before the message. rows are laid
//! out once per entry and width, and only the ones in sight are drawn.

use std::collections::HashMap;
use std::path::PathBuf;

use aurea::AureaResult;
use aurea::CursorIcon;
use aurea::render::{Color, Font, Rect};
use aurea::ui::{Align, Button, ScrollArea, Sense, TextArea, TextField, Ui};

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
    /// the built-in agent's setup card: open, a preset, save, close.
    SetupOpen,
    /// a profile to edit, by place; one past the last for a new one.
    SetupPick(usize),
    SetupPreset(usize),
    SetupSave,
    SetupDelete,
    SetupCancel,
    /// the agent picker, then a pick from it.
    Agents,
    PickAgent(usize),
    /// the saved conversations, then one to read.
    History,
    OpenPast(usize),
    /// back from history, a past conversation or the picker.
    CloseOver,
    /// the agent to this mode.
    Mode(String),
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

impl std::fmt::Debug for Cache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Cache({} entries)", self.0.len())
    }
}

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
    // a saved conversation is drawn as the live one is: its entries, rows
    // and scroll are swapped in for the frame, and back out after
    if let super::Over::Past(_, entries, laid, scroll) = &mut chat.over {
        let (mut entries, mut laid, mut scroll) =
            (std::mem::take(entries), std::mem::take(laid), *scroll);
        std::mem::swap(&mut chat.entries, &mut entries);
        std::mem::swap(&mut chat.laid, &mut laid);
        std::mem::swap(&mut chat.scroll, &mut scroll);
        let follow = std::mem::replace(&mut chat.follow, false);
        let ask = show_inner(ui, chat, rect, texts, true);
        std::mem::swap(&mut chat.entries, &mut entries);
        std::mem::swap(&mut chat.laid, &mut laid);
        std::mem::swap(&mut chat.scroll, &mut scroll);
        chat.follow = follow;
        if let super::Over::Past(_, e, l, s) = &mut chat.over {
            *e = entries;
            *l = laid;
            *s = scroll;
        }
        return ask;
    }
    show_inner(ui, chat, rect, texts, false)
}

fn show_inner(
    ui: &mut Ui<'_, Id>,
    chat: &mut Chat,
    rect: Rect,
    texts: &Texts,
    reading: bool,
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
    let gear_w = if chat.agent == super::Agent::OpenAi {
        30.0
    } else {
        0.0
    };
    let room = head.right() - PAD - 56.0 - gear_w - 30.0 - 8.0 - (icon.right() + 8.0) - 14.0;
    let full = chat.title.as_str();
    let short = full.split(' ').next().unwrap_or(full);
    let shown = [full, short]
        .into_iter()
        .find(|n| ui.text_width(n, &bold) <= room)
        .unwrap_or("");
    let name = Rect::new(icon.right() + 8.0, head.y, room.max(0.0), HEAD);
    let name_w = ui.text_width(shown, &bold);
    // the name is the agent switch: a click lists the others
    let hit = Rect::new(icon.x - 4.0, head.y + 5.0, name_w + 40.0, HEAD - 10.0);
    let input = ui.interact(Id::AgentButton(20), 0, hit, Sense::Click, true);
    if input.hovered {
        ui.round(hit, 6.0, palette.hover)?;
        ui.set_cursor(CursorIcon::Pointer);
    }
    ui.text(shown, name, &bold, palette.text, Align::Start)?;
    let arrow = Rect::new(name.x + name_w + 4.0, head.y, 12.0, HEAD);
    ui.text("▾", arrow, &small, palette.dim, Align::Start)?;
    if input.clicked {
        ask = Some(if matches!(chat.over, super::Over::Agents(_)) {
            Ask::CloseOver
        } else {
            Ask::Agents
        });
    }
    let name_w = name_w + 14.0;
    let (state, tone) = match &chat.state {
        State::Off => (texts.tr("not started"), palette.faint),
        State::Starting => (texts.tr("starting…"), palette.dim),
        State::SignIn(_) => (texts.tr("sign in"), palette.warning),
        State::Ready => (texts.tr("ready"), palette.success),
        State::Working => (texts.tr("working…"), palette.accent),
        State::Failed(_) => (texts.tr("stopped"), palette.danger),
    };
    let new = Rect::new(head.right() - PAD - 56.0, head.y + 6.0, 56.0, HEAD - 12.0);
    // the saved conversations
    let clock = Rect::new(new.x - 30.0, head.y + 6.0, 26.0, HEAD - 12.0);
    let input = ui.interact(Id::AgentButton(21), 0, clock, Sense::Click, true);
    if input.hovered || matches!(chat.over, super::Over::History(_) | super::Over::Past(..)) {
        ui.round(clock, 6.0, palette.hover)?;
    }
    if input.hovered {
        ui.set_cursor(CursorIcon::Pointer);
    }
    let at = Rect::new(
        clock.x + 5.0,
        clock.y + (clock.height - 16.0) / 2.0,
        16.0,
        16.0,
    );
    crate::icons::draw(ui, "history", at, palette.dim)?;
    if input.clicked {
        ask = Some(if matches!(chat.over, super::Over::History(_)) {
            Ask::CloseOver
        } else {
            Ask::History
        });
    }
    // the built-in agent's model can change any time
    let gear = Rect::new(clock.x - 30.0, head.y + 6.0, 26.0, HEAD - 12.0);
    if chat.agent == super::Agent::OpenAi {
        let input = ui.interact(Id::AgentButton(6), 0, gear, Sense::Click, true);
        if input.hovered {
            ui.round(gear, 6.0, palette.hover)?;
            ui.set_cursor(CursorIcon::Pointer);
        }
        let icon = Rect::new(
            gear.x + 5.0,
            gear.y + (gear.height - 16.0) / 2.0,
            16.0,
            16.0,
        );
        crate::icons::draw(ui, "settings", icon, palette.dim)?;
        if input.clicked {
            ask = Some(Ask::SetupOpen);
        }
    }
    // the state's dot always; its word only where it fits before the button
    let first_button = if chat.agent == super::Agent::OpenAi {
        gear.x
    } else {
        clock.x
    };
    let dot = Rect::new(name.x + name_w + 10.0, head.y + HEAD / 2.0 - 3.0, 6.0, 6.0);
    if dot.right() < first_button - 4.0 {
        ui.round(dot, 3.0, tone)?;
        let room = first_button - 8.0 - (dot.right() + 6.0);
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

    if chat.setup.is_some() {
        if let Some(a) = setup_card(ui, chat, rest, texts)? {
            ask = Some(a);
        }
        return Ok(ask);
    }

    // the agent picker or the history, over everything below the header
    if let Some(a) = over_list(ui, chat, rest, texts)? {
        return Ok(Some(a));
    }
    if matches!(chat.over, super::Over::Agents(_) | super::Over::History(_)) {
        return Ok(ask);
    }
    // reading a saved one: a bar to go back in place of the box
    if reading {
        let (bar, talk) = rest.split_bottom(48.0, 0.0);
        ui.fill(Rect::new(bar.x, bar.y, bar.width, 1.0), palette.border)?;
        let inner = bar.inset(PAD).inset_xy(0.0, -4.0);
        let back = Rect::new(inner.right() - 80.0, inner.y, 80.0, 28.0);
        let title = match &chat.over {
            super::Over::Past(t, ..) => t.clone(),
            _ => String::new(),
        };
        let words = Rect::new(inner.x, inner.y, (back.x - inner.x - 8.0).max(0.0), 28.0);
        ui.text_clipped(
            &format!("{} · {title}", texts.tr("Saved")),
            words,
            &small,
            palette.dim,
            Align::Start,
        )?;
        if Button::new(texts.tr("Back"))
            .show(ui, Id::AgentButton(22), back)?
            .clicked
        {
            ask = Some(Ask::CloseOver);
        }
        return Ok(transcript(ui, chat, talk, (&font, &small, &bold, &mono))?.or(ask));
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
            // a chip that doesn't fit waits; the rest still go with the message
            if x > foot.x && x + w > foot.right() {
                break;
            }
            let chip = Rect::new(x, y, w.min(foot.width), CHIP);
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
    // the agent's mode left of the button, a click for the next; with no
    // modes, the keys, quietly
    let left = Rect::new(buttons.x, buttons.y, go.x - buttons.x - 8.0, 28.0);
    if chat.modes.is_empty() {
        let keys = texts.tr("Enter to send · Ctrl+Shift+J adds lines");
        if ui.text_width(keys, &small) <= left.width {
            ui.text(keys, left, &small, palette.faint, Align::Start)?;
        }
    } else {
        let name = chat
            .mode
            .as_ref()
            .and_then(|m| chat.modes.iter().find(|(id, _)| id == m))
            .map_or_else(|| chat.modes[0].1.clone(), |(_, n)| n.clone());
        let label = format!("◇ {name}");
        let w = (ui.text_width(&label, &small) + 22.0).min(left.width);
        let chip = Rect::new(left.x, left.y + 2.0, w, 24.0);
        let input = ui.interact(Id::AgentButton(23), 0, chip, Sense::Click, true);
        ui.round(
            chip,
            12.0,
            if input.hovered {
                palette.hover
            } else {
                palette.raised
            },
        )?;
        ui.text_clipped(
            &label,
            chip.inset_xy(10.0, 0.0),
            &small,
            palette.text,
            Align::Start,
        )?;
        if input.hovered {
            ui.set_cursor(CursorIcon::Pointer);
        }
        if input.clicked
            && let Some(next) = chat.next_mode()
        {
            ask = Some(Ask::Mode(next));
        }
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
        // the options on one row when they fit, else one under another
        let inner_w = talk.width - 2.0 * PAD - 24.0;
        let widths: Vec<f32> = p
            .options
            .iter()
            .map(|(_, label, _)| ui.text_width(label, &small) + 24.0)
            .collect();
        let row = widths.iter().sum::<f32>() + 6.0 * widths.len().saturating_sub(1) as f32;
        let stacked = row > inner_w;
        let button_rows = if stacked { widths.len() } else { 1 };
        let card_h = 30.0 + lines.len().min(4) as f32 * TEXT_ROW + 8.0 + button_rows as f32 * 32.0;
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
            Rect::new(card.x + 12.0, cy, card.width - 24.0, 20.0),
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
        let mut by = cy + 8.0;
        for (i, ((id, label, allows), w)) in p.options.iter().zip(&widths).enumerate() {
            let b = if stacked {
                Rect::new(card.x + 12.0, by, inner_w.max(40.0), 26.0)
            } else {
                Rect::new(bx, by, *w, 26.0)
            };
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
            if stacked {
                by += 32.0;
            }
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

    Ok(transcript(ui, chat, talk, (&font, &small, &bold, &mono))?.or(ask))
}

/// the conversation in `talk`: rows laid out where they changed, the ones
/// in sight drawn, following the end while it is in sight.
fn transcript(
    ui: &mut Ui<'_, Id>,
    chat: &mut Chat,
    talk: Rect,
    (font, small, bold, mono): (&Font, &Font, &Font, &Font),
) -> AureaResult<Option<Ask>> {
    let palette = ui.theme.palette;
    let (font, small, bold, mono) = (font.clone(), small.clone(), bold.clone(), mono.clone());
    let mut ask = None;
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

/// the custom agent's model profiles: one to pick at the top, then the
/// picked one's server, model, context and key. the key field never shows
/// a saved key; it only says there is one.
fn setup_card(
    ui: &mut Ui<'_, Id>,
    chat: &mut Chat,
    rect: Rect,
    texts: &Texts,
) -> AureaResult<Option<Ask>> {
    let palette = ui.theme.palette;
    let small = ui.theme.font(12.0);
    let bold = ui
        .theme
        .font(14.0)
        .with_weight(aurea::render::FontWeight::Bold);
    let Some(setup) = chat.setup.as_mut() else {
        return Ok(None);
    };
    let mut ask = None;
    let area = rect.inset(PAD);
    let mut y = area.y + 4.0;
    ui.text(
        texts.tr("Model servers"),
        Rect::new(area.x, y, area.width, 22.0),
        &bold,
        palette.text,
        Align::Start,
    )?;
    y += 26.0;
    let about = texts.tr("Any OpenAI-compatible server, as many as you like. Keys stay in a file in Ligase's config folder; local servers need none.");
    for line in hover::wrap(ui, about, &small, area.width) {
        ui.text(
            &line,
            Rect::new(area.x, y, area.width, 17.0),
            &small,
            palette.dim,
            Align::Start,
        )?;
        y += 17.0;
    }
    y += 10.0;
    // chips that wrap: the profiles, then a new one; presets further down
    let chips = |ui: &mut Ui<'_, Id>,
                 y: &mut f32,
                 items: &[(String, bool)],
                 base: u16|
     -> AureaResult<Option<usize>> {
        let mut x = area.x;
        let mut picked = None;
        for (i, (name, lit)) in items.iter().enumerate() {
            let w = ui.text_width(name, &small) + 20.0;
            if x > area.x && x + w > area.right() {
                x = area.x;
                *y += CHIP + 6.0;
            }
            let chip = Rect::new(x, *y, w.min(area.width), CHIP);
            let n = base.saturating_add(u16::try_from(i).unwrap_or(u16::MAX - base));
            let input = ui.interact(Id::AgentButton(n), 0, chip, Sense::Click, true);
            let fill = if *lit {
                palette.accent.fade(0.25)
            } else if input.hovered {
                palette.hover
            } else {
                palette.raised
            };
            ui.round(chip, 11.0, fill)?;
            ui.text_clipped(
                name,
                chip.inset_xy(8.0, 0.0),
                &small,
                if *lit { palette.text } else { palette.dim },
                Align::Center,
            )?;
            if input.hovered {
                ui.set_cursor(CursorIcon::Pointer);
            }
            if input.clicked {
                picked = Some(i);
            }
            x += w + 6.0;
        }
        *y += CHIP + 12.0;
        Ok(picked)
    };
    let mut profiles: Vec<(String, bool)> = setup
        .profiles
        .iter()
        .map(|p| (p.clone(), setup.editing.as_deref() == Some(p.as_str())))
        .collect();
    profiles.push((texts.tr("+ New").to_owned(), setup.editing.is_none()));
    if let Some(i) = chips(ui, &mut y, &profiles, 60)? {
        ask = Some(Ask::SetupPick(i));
    }
    let field = |ui: &mut Ui<'_, Id>, label: &str, y: f32| -> AureaResult<Rect> {
        ui.text(
            label,
            Rect::new(area.x, y, area.width, 18.0),
            &small,
            palette.dim,
            Align::Start,
        )?;
        Ok(Rect::new(area.x, y + 20.0, area.width, 30.0))
    };
    let at = field(ui, texts.tr("Name"), y)?;
    TextField::new(&mut setup.name)
        .placeholder("openai, local, work…")
        .max_chars(60)
        .show(ui, Id::AgentField(3), at)?;
    y = at.bottom() + 10.0;
    ui.text(
        texts.tr("Start from"),
        Rect::new(area.x, y, area.width, 18.0),
        &small,
        palette.dim,
        Align::Start,
    )?;
    y += 20.0;
    let presets: Vec<(String, bool)> = super::native::PRESETS
        .iter()
        .map(|(name, base, ..)| ((*name).to_owned(), setup.base == *base))
        .collect();
    if let Some(i) = chips(ui, &mut y, &presets, 80)? {
        ask = Some(Ask::SetupPreset(i));
    }
    let at = field(ui, texts.tr("Server URL"), y)?;
    TextField::new(&mut setup.base)
        .placeholder("https://api.openai.com/v1")
        .max_chars(500)
        .show(ui, Id::AgentField(0), at)?;
    y = at.bottom() + 10.0;
    // model and context side by side when there is room
    let wide = area.width >= 300.0;
    let model_w = if wide { area.width - 110.0 } else { area.width };
    ui.text(
        texts.tr("Model"),
        Rect::new(area.x, y, model_w, 18.0),
        &small,
        palette.dim,
        Align::Start,
    )?;
    let model_at = Rect::new(area.x, y + 20.0, model_w, 30.0);
    TextField::new(&mut setup.model)
        .placeholder("gpt-4.1")
        .max_chars(200)
        .show(ui, Id::AgentField(1), model_at)?;
    let context_at = if wide {
        let x = area.x + model_w + 10.0;
        ui.text(
            texts.tr("Context"),
            Rect::new(x, y, 100.0, 18.0),
            &small,
            palette.dim,
            Align::Start,
        )?;
        y = model_at.bottom() + 10.0;
        Rect::new(x, model_at.y, 100.0, 30.0)
    } else {
        let at = field(ui, texts.tr("Context"), model_at.bottom() + 10.0)?;
        y = at.bottom() + 10.0;
        at
    };
    TextField::new(&mut setup.context)
        .placeholder("32000")
        .max_chars(8)
        .show(ui, Id::AgentField(4), context_at)?;
    let at = field(ui, texts.tr("API key"), y)?;
    let saved = setup.saved_keys.iter().any(|k| *k == setup.name.trim());
    let hint = if saved {
        texts.tr("a key is saved; empty keeps it")
    } else {
        texts.tr("sk-…, empty for local servers")
    };
    let typed = TextField::new(&mut setup.key)
        .placeholder(hint)
        .password(true)
        .max_chars(1000)
        .show(ui, Id::AgentField(2), at)?;
    y = at.bottom() + 16.0;
    let ready = !setup.name.trim().is_empty()
        && !setup.base.trim().is_empty()
        && !setup.model.trim().is_empty();
    let save = Rect::new(area.x, y, 130.0_f32.min(area.width), 30.0);
    if (Button::new(texts.tr("Save and use"))
        .primary()
        .enabled(ready)
        .show(ui, Id::AgentButton(7), save)?
        .clicked
        || typed.submitted)
        && ready
    {
        ask = Some(Ask::SetupSave);
    }
    let mut x = save.right() + 8.0;
    if setup.editing.is_some() {
        let delete = Rect::new(x, y, 80.0, 30.0);
        if delete.right() <= area.right()
            && Button::new(texts.tr("Delete"))
                .quiet()
                .show(ui, Id::AgentButton(9), delete)?
                .clicked
        {
            ask = Some(Ask::SetupDelete);
        }
        x = delete.right() + 8.0;
    }
    let cancel = Rect::new(x, y, 80.0, 30.0);
    if cancel.right() <= area.right()
        && Button::new(texts.tr("Cancel"))
            .quiet()
            .show(ui, Id::AgentButton(8), cancel)?
            .clicked
    {
        ask = Some(Ask::SetupCancel);
    }
    Ok(ask)
}

/// the agent picker or the saved conversations, in `rect`: rows to click.
/// none when neither is up.
fn over_list(
    ui: &mut Ui<'_, Id>,
    chat: &mut Chat,
    rect: Rect,
    texts: &Texts,
) -> AureaResult<Option<Ask>> {
    let palette = ui.theme.palette;
    let font = ui.theme.font(13.0);
    let small = ui.theme.font(12.0);
    let rows: Vec<(String, String, bool)> = match &chat.over {
        super::Over::Agents(list) => list
            .iter()
            .map(|(label, word, profile)| {
                let lit = match profile {
                    Some(p) => {
                        chat.agent == super::Agent::OpenAi && chat.title.starts_with(p.as_str())
                    }
                    None => chat.agent.word() == word,
                };
                (label.clone(), String::new(), lit)
            })
            .collect(),
        super::Over::History(list) => list
            .iter()
            .map(|p| {
                (
                    p.title.clone(),
                    format!("{} · {}", p.agent, ago(p.when)),
                    false,
                )
            })
            .collect(),
        _ => return Ok(None),
    };
    let agents = matches!(chat.over, super::Over::Agents(_));
    let area = rect.inset(PAD);
    let head = if agents {
        texts.tr("Talk to")
    } else {
        texts.tr("Saved conversations")
    };
    ui.text(
        head,
        Rect::new(area.x, area.y, area.width, 20.0),
        &small,
        palette.dim,
        Align::Start,
    )?;
    if rows.is_empty() {
        ui.text(
            texts.tr("Nothing saved yet"),
            Rect::new(area.x, area.y + 28.0, area.width, 20.0),
            &font,
            palette.faint,
            Align::Start,
        )?;
        return Ok(None);
    }
    let row_h = if agents { 32.0 } else { 46.0 };
    let list = Rect::new(area.x, area.y + 26.0, area.width, area.height - 26.0);
    let mut ask = None;
    let mut none = None;
    let response = aurea::ui::ListView::new(&mut chat.list_scroll, rows.len(), row_h)
        .selected(&mut none)
        .show(ui, Id::AgentList, list, |ui, row| {
            let Some((title, note, lit)) = rows.get(row.index) else {
                return Ok(());
            };
            let r = row.rect.inset_xy(0.0, 2.0);
            if *lit {
                ui.round(r, 8.0, palette.accent.fade(0.18))?;
            } else if row.hovered {
                ui.round(r, 8.0, palette.hover)?;
            }
            if row.hovered {
                ui.set_cursor(CursorIcon::Pointer);
            }
            if note.is_empty() {
                ui.text_clipped(
                    title,
                    r.inset_xy(12.0, 0.0),
                    &font,
                    palette.text,
                    Align::Start,
                )?;
            } else {
                let top = Rect::new(r.x + 12.0, r.y + 4.0, r.width - 24.0, 20.0);
                ui.text_clipped(title, top, &font, palette.text, Align::Start)?;
                let bottom = Rect::new(r.x + 12.0, r.y + 23.0, r.width - 24.0, 16.0);
                ui.text_clipped(note, bottom, &small, palette.dim, Align::Start)?;
            }
            Ok(())
        })?;
    if let Some(i) = response.row_clicked {
        ask = Some(if agents {
            Ask::PickAgent(i)
        } else {
            Ask::OpenPast(i)
        });
    }
    Ok(ask)
}

/// how long ago `when` (seconds since 1970) was, said short.
fn ago(when: u64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let s = now.saturating_sub(when);
    match s {
        0..60 => "now".to_owned(),
        60..3600 => format!("{}m", s / 60),
        3600..86_400 => format!("{}h", s / 3600),
        _ => format!("{}d", s / 86_400),
    }
}
