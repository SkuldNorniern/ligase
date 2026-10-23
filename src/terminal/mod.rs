//! terminal panel: one shell from ozone-term, started the first time the
//! panel is drawn. while the panel has focus its keys go to the shell.

use aurea::render::{Color, Font, Rect};
use aurea::ui::{Align, ScrollArea, Sense, Ui};
use aurea::{AureaResult, KeyCode, Modifiers, WindowEvent};
use ozone_term::{Cell, Color as TermColor, Terminal};

use crate::app::Id;
use crate::lang::Texts;

const SIZE: f32 = 13.0;
const LINE: f32 = 18.0;
const BAR: f32 = 14.0;
const PAD: f32 = 6.0;

const MONO: &str = if cfg!(windows) {
    "Consolas"
} else if cfg!(target_os = "macos") {
    "Menlo"
} else {
    "DejaVu Sans Mono"
};

/// the 16 ansi colours on a dark background.
const ANSI: [Color; 16] = [
    Color::rgb(0x1e, 0x22, 0x27),
    Color::rgb(0xe0, 0x6c, 0x75),
    Color::rgb(0x98, 0xc3, 0x79),
    Color::rgb(0xe5, 0xc0, 0x7b),
    Color::rgb(0x61, 0xaf, 0xef),
    Color::rgb(0xc6, 0x78, 0xdd),
    Color::rgb(0x56, 0xb6, 0xc2),
    Color::rgb(0xab, 0xb2, 0xbf),
    Color::rgb(0x5c, 0x63, 0x70),
    Color::rgb(0xff, 0x7b, 0x86),
    Color::rgb(0xb5, 0xe8, 0x90),
    Color::rgb(0xff, 0xd6, 0x8a),
    Color::rgb(0x7d, 0xc4, 0xff),
    Color::rgb(0xde, 0x95, 0xf5),
    Color::rgb(0x6c, 0xd6, 0xe2),
    Color::rgb(0xe6, 0xea, 0xf0),
];

#[derive(Debug, Default, PartialEq, Eq)]
pub enum State {
    #[default]
    NotStarted,
    Starting,
    Running,
    Ended,
    Failed(String),
}

#[derive(Default)]
pub struct Shell {
    term: Option<Terminal>,
    pub state: State,
    seen: u64,
    cells: Vec<Vec<Cell>>,
    cursor: (usize, usize),
    size: (u16, u16),
    scroll: f32,
    /// stay at the bottom as output comes in, until the user scrolls up.
    follow: bool,
}

impl Shell {
    /// the start job came back.
    pub fn started(&mut self, result: Result<Terminal, String>) {
        match result {
            Ok(term) => {
                self.term = Some(term);
                self.state = State::Running;
                self.seen = u64::MAX;
                self.size = (0, 0);
                self.follow = true;
            }
            Err(why) => self.state = State::Failed(why),
        }
    }

    /// sends typed bytes to the shell.
    pub fn write(&self, bytes: &str) {
        if let Some(term) = &self.term {
            term.write_str(bytes);
        }
    }

    fn refresh(&mut self) {
        let Some(term) = &self.term else { return };
        if !term.is_running() {
            self.state = State::Ended;
        }
        let version = term.version();
        if version != self.seen {
            self.seen = version;
            self.cells = term.cell_snapshot();
            self.cursor = term.cursor();
        }
    }
}

/// bytes a key or typed text sends to the shell, the way xterm sends them.
pub fn bytes(event: &WindowEvent) -> Option<String> {
    match event {
        WindowEvent::TextInput { text } => {
            // control chars come through key events instead
            let typed: String = text.chars().filter(|c| !c.is_control()).collect();
            (!typed.is_empty()).then_some(typed)
        }
        WindowEvent::KeyInput {
            key,
            pressed: true,
            modifiers,
        } => key_bytes(*key, *modifiers)
            .map(str::to_owned)
            .or_else(|| control(*key, *modifiers).map(|b| char::from(b).to_string())),
        _ => None,
    }
}

fn key_bytes(key: KeyCode, m: Modifiers) -> Option<&'static str> {
    if m.ctrl || m.alt || m.meta {
        return None;
    }
    Some(match key {
        KeyCode::Enter => "\r",
        KeyCode::Backspace => "\x7f",
        KeyCode::Tab => "\t",
        KeyCode::Escape => "\x1b",
        KeyCode::Up => "\x1b[A",
        KeyCode::Down => "\x1b[B",
        KeyCode::Right => "\x1b[C",
        KeyCode::Left => "\x1b[D",
        KeyCode::Home => "\x1b[H",
        KeyCode::End => "\x1b[F",
        KeyCode::Delete => "\x1b[3~",
        KeyCode::PageUp => "\x1b[5~",
        KeyCode::PageDown => "\x1b[6~",
        _ => return None,
    })
}

/// ctrl+letter as its control byte, like ctrl+c = 3.
fn control(key: KeyCode, m: Modifiers) -> Option<u8> {
    if !m.ctrl || m.alt || m.meta {
        return None;
    }
    const LETTERS: [KeyCode; 26] = [
        KeyCode::A,
        KeyCode::B,
        KeyCode::C,
        KeyCode::D,
        KeyCode::E,
        KeyCode::F,
        KeyCode::G,
        KeyCode::H,
        KeyCode::I,
        KeyCode::J,
        KeyCode::K,
        KeyCode::L,
        KeyCode::M,
        KeyCode::N,
        KeyCode::O,
        KeyCode::P,
        KeyCode::Q,
        KeyCode::R,
        KeyCode::S,
        KeyCode::T,
        KeyCode::U,
        KeyCode::V,
        KeyCode::W,
        KeyCode::X,
        KeyCode::Y,
        KeyCode::Z,
    ];
    let index = LETTERS.iter().position(|k| *k == key)?;
    u8::try_from(index + 1).ok()
}

fn resolve(color: TermColor, default: Color) -> Color {
    match color {
        TermColor::Default => default,
        TermColor::Indexed(i @ 0..=15) => ANSI[usize::from(i)],
        TermColor::Indexed(i @ 16..=231) => {
            let i = i - 16;
            let scale = |v: u8| if v == 0 { 0 } else { 55 + v * 40 };
            Color::rgb(scale(i / 36), scale((i % 36) / 6), scale(i % 6))
        }
        TermColor::Indexed(i) => {
            let v = 8 + (i - 232) * 10;
            Color::rgb(v, v, v)
        }
        TermColor::Rgb(r, g, b) => Color::rgb(r, g, b),
    }
}

/// draws the panel. true when it has the keyboard.
pub fn show(
    ui: &mut Ui<'_, Id>,
    shell: &mut Shell,
    rect: Rect,
    texts: &Texts,
) -> AureaResult<bool> {
    let palette = ui.theme.palette;
    let font = Font::new(MONO, SIZE);
    let hint = |ui: &mut Ui<'_, Id>, text: &str| {
        let small = ui.theme.font(13.0);
        ui.text(text, rect, &small, palette.dim, Align::Center)
    };
    match &shell.state {
        State::NotStarted | State::Starting => {
            hint(ui, texts.tr("starting the shell"))?;
            return Ok(false);
        }
        State::Failed(why) => {
            hint(ui, &format!("{}: {why}", texts.tr("can't start the shell")))?;
            return Ok(false);
        }
        State::Running | State::Ended => {}
    }
    shell.refresh();
    let cell = ui.text_width("M", &font).max(1.0);
    let cols = ((rect.width - BAR - 2.0 * PAD) / cell).floor().max(10.0) as u16;
    let rows = ((rect.height - 2.0 * PAD) / LINE).floor().max(2.0) as u16;
    if shell.size != (cols, rows) {
        shell.size = (cols, rows);
        if let Some(term) = &shell.term {
            term.resize(cols, rows);
        }
    }
    let ended = shell.state == State::Ended;
    let lines = shell.cells.len() + usize::from(ended);
    let height = lines as f32 * LINE + 2.0 * PAD;
    if shell.follow {
        shell.scroll = (height - rect.height).max(0.0);
    }
    let Shell {
        cells,
        cursor,
        scroll,
        ..
    } = shell;
    let mut focused = false;
    ScrollArea::new(scroll, height).show(ui, Id::Terminal, rect, |ui, content, seen| {
        let hit = Rect::new(seen.x, seen.y, (seen.width - BAR).max(0.0), seen.height);
        focused = ui
            .interact(Id::Terminal, 1, hit, Sense::Click, true)
            .focused;
        let first = ((seen.y - content.y - PAD) / LINE).floor().max(0.0) as usize;
        let count = (seen.height / LINE).ceil() as usize + 1;
        let x0 = content.x + PAD;
        for (row, line) in cells.iter().enumerate().skip(first).take(count) {
            let y = content.y + PAD + row as f32 * LINE;
            let mut col = 0;
            while col < line.len() {
                // one run of cells with the same look
                let look = &line[col];
                let end = line[col..]
                    .iter()
                    .position(|c| c.fg != look.fg || c.bg != look.bg || c.inverse != look.inverse)
                    .map_or(line.len(), |n| col + n);
                let (mut fg, mut bg) = (
                    resolve(look.fg, palette.text),
                    resolve(look.bg, palette.background),
                );
                if look.inverse {
                    std::mem::swap(&mut fg, &mut bg);
                }
                let at = Rect::new(x0 + col as f32 * cell, y, (end - col) as f32 * cell, LINE);
                if look.bg != TermColor::Default || look.inverse {
                    ui.fill(at, bg)?;
                }
                let text: String = line[col..end].iter().map(|c| c.ch).collect();
                if !text.trim().is_empty() {
                    ui.text(&text, at, &font, fg, Align::Start)?;
                }
                col = end;
            }
        }
        if ended {
            let y = content.y + PAD + cells.len() as f32 * LINE;
            let row = Rect::new(x0, y, content.width, LINE);
            let small = ui.theme.font(12.0);
            ui.text(
                texts.tr("the shell ended, press enter for a new one"),
                row,
                &small,
                palette.dim,
                Align::Start,
            )?;
        } else if focused {
            let (line, col) = *cursor;
            let at = Rect::new(
                x0 + col as f32 * cell,
                content.y + PAD + line as f32 * LINE,
                cell,
                LINE,
            );
            ui.fill(at, palette.text.with_alpha(120))?;
        }
        Ok(())
    })?;
    // scrolled back to the bottom: follow again
    shell.follow = shell.scroll + rect.height >= height - LINE;
    Ok(focused)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(key: KeyCode, modifiers: Modifiers) -> WindowEvent {
        WindowEvent::KeyInput {
            key,
            pressed: true,
            modifiers,
        }
    }

    #[test]
    fn keys_turn_into_xterm_bytes() {
        let none = Modifiers::default();
        let ctrl = Modifiers { ctrl: true, ..none };
        assert_eq!(bytes(&key(KeyCode::Enter, none)).as_deref(), Some("\r"));
        assert_eq!(bytes(&key(KeyCode::Up, none)).as_deref(), Some("\x1b[A"));
        assert_eq!(bytes(&key(KeyCode::C, ctrl)).as_deref(), Some("\x03"));
        assert_eq!(bytes(&key(KeyCode::A, none)), None, "letters come as text");
        let typed = WindowEvent::TextInput {
            text: "ls\u{3}".into(),
        };
        assert_eq!(bytes(&typed).as_deref(), Some("ls"));
    }

    #[test]
    fn colours_resolve() {
        let d = Color::rgb(1, 2, 3);
        assert_eq!(resolve(TermColor::Default, d), d);
        assert_eq!(resolve(TermColor::Indexed(1), d), ANSI[1]);
        assert_eq!(resolve(TermColor::Indexed(16), d), Color::rgb(0, 0, 0));
        assert_eq!(
            resolve(TermColor::Indexed(231), d),
            Color::rgb(255, 255, 255)
        );
        assert_eq!(
            resolve(TermColor::Indexed(255), d),
            Color::rgb(238, 238, 238)
        );
    }
}
