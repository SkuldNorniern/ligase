//! terminal panel: one shell from vanta, started the first time the panel
//! is drawn. while the panel has focus its keys go to the shell.

use std::path::Path;

use aurea::render::{Color, Font, Rect};
use aurea::ui::{Align, ScrollArea, ScrollStyle, Sense, Ui};
use aurea::{AureaResult, KeyCode, Modifiers, WindowEvent};
use vanta::vt::Attrs;
use vanta::{Cell, CellKind, Color as TermColor, SpawnConfig, Terminal};

use crate::app::Id;
use crate::lang::Texts;

const BAR: f32 = 14.0;
const PAD: f32 = 12.0;

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

/// escape, the start of every key sequence below.
const ESC: &str = "\x1b";

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
    size: (u16, u16),
    scroll: f32,
    /// stay at the bottom as output comes in, until the user scrolls up.
    follow: bool,
    /// the shell asked for `ESC O` arrows (DECCKM), seen last frame.
    app_keys: bool,
    /// lines dropped off the top of scrollback, as of last frame.
    dropped: u64,
}

/// starts a shell in `root`. runs on the worker; `wake` runs on vanta's
/// reader thread after each chunk of output.
pub fn spawn(root: &Path, wake: impl Fn() + Send + 'static) -> Result<Terminal, String> {
    let config = SpawnConfig {
        cwd: Some(root.to_path_buf()),
        ..SpawnConfig::default()
    };
    Terminal::spawn_notify(&config, wake).map_err(|e| e.to_string())
}

impl Shell {
    /// the start job came back.
    pub fn started(&mut self, result: Result<Terminal, String>) {
        match result {
            Ok(term) => {
                self.term = Some(term);
                self.state = State::Running;
                self.size = (0, 0);
                self.follow = true;
            }
            Err(why) => self.state = State::Failed(why),
        }
    }

    /// sends typed bytes to the shell.
    pub fn write(&self, bytes: &str) {
        if let Some(term) = &self.term {
            let _ = term.write_str(bytes);
        }
    }

    /// pastes text, in bracketed-paste markers when the shell wants them.
    pub fn paste(&self, text: &str) {
        if let Some(term) = &self.term {
            let _ = term.paste_str(text);
        }
    }

    /// bytes for a window event, in the key mode the shell asked for.
    pub fn bytes(&self, event: &WindowEvent) -> Option<String> {
        bytes(event, self.app_keys)
    }
}

/// bytes a key or typed text sends to the shell, the way xterm sends them.
/// `app_keys` is DECCKM: plain arrows as `ESC O A` instead of `ESC [ A`.
pub fn bytes(event: &WindowEvent, app_keys: bool) -> Option<String> {
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
        } => key_bytes(*key, *modifiers, app_keys),
        _ => None,
    }
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

/// a key as xterm sends it. alt puts ESC in front, ctrl+letter is its
/// control byte, arrows carry shift/alt/ctrl as `ESC [ 1 ; n X`.
fn key_bytes(key: KeyCode, m: Modifiers, app_keys: bool) -> Option<String> {
    if m.meta {
        return None;
    }
    let arrow = match key {
        KeyCode::Up => Some('A'),
        KeyCode::Down => Some('B'),
        KeyCode::Right => Some('C'),
        KeyCode::Left => Some('D'),
        KeyCode::Home => Some('H'),
        KeyCode::End => Some('F'),
        _ => None,
    };
    if let Some(end) = arrow {
        let code = 1 + u8::from(m.shift) + 2 * u8::from(m.alt) + 4 * u8::from(m.ctrl);
        return Some(match (code, app_keys) {
            (1, true) => format!("{ESC}O{end}"),
            (1, false) => format!("{ESC}[{end}"),
            _ => format!("{ESC}[1;{code}{end}"),
        });
    }
    if key == KeyCode::Tab && m.shift && !m.ctrl && !m.alt {
        return Some(format!("{ESC}[Z"));
    }
    let plain = match key {
        KeyCode::Enter => Some("\r".to_owned()),
        KeyCode::Backspace => Some("\x7f".to_owned()),
        KeyCode::Tab => Some("\t".to_owned()),
        KeyCode::Escape => Some(ESC.to_owned()),
        KeyCode::Delete => Some(format!("{ESC}[3~")),
        KeyCode::PageUp => Some(format!("{ESC}[5~")),
        KeyCode::PageDown => Some(format!("{ESC}[6~")),
        _ => None,
    };
    if let Some(plain) = plain {
        if m.ctrl {
            return None;
        }
        return Some(if m.alt {
            format!("{ESC}{plain}")
        } else {
            plain
        });
    }
    let letter = u8::try_from(LETTERS.iter().position(|k| *k == key)?).ok()?;
    match (m.ctrl, m.alt) {
        (true, false) => Some(char::from(letter + 1).to_string()),
        (true, true) => Some(format!("{ESC}{}", char::from(letter + 1))),
        (false, true) => {
            let c = if m.shift { b'A' } else { b'a' } + letter;
            Some(format!("{ESC}{}", char::from(c)))
        }
        (false, false) => None,
    }
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

/// a cell's look, for grouping cells into runs.
fn look(cell: &Cell) -> (TermColor, TermColor, bool) {
    (cell.fg, cell.bg, cell.attrs.contains(Attrs::INVERSE))
}

fn push_text(cell: &Cell, out: &mut String) {
    match &cell.kind {
        CellKind::Empty => out.push(' '),
        CellKind::Char(c) => out.push(*c),
        CellKind::Cluster(s) => out.push_str(s),
        CellKind::Continuation => {}
    }
}

/// one line: runs of narrow cells as text, wide cells one by one at their
/// column so a font's own width can't push the grid out of line.
fn draw_row(
    ui: &mut Ui<'_, Id>,
    line: &[Cell],
    (x0, y): (f32, f32),
    (cell, line_h): (f32, f32),
    font: &Font,
) -> AureaResult<()> {
    let palette = ui.theme.palette;
    let mut col = 0;
    while col < line.len() {
        let first = &line[col];
        let wide = first.width > 1;
        let end = if wide {
            (col + 1..line.len())
                .find(|&i| !matches!(line[i].kind, CellKind::Continuation))
                .unwrap_or(line.len())
        } else {
            (col + 1..line.len())
                .find(|&i| line[i].width != 1 || look(&line[i]) != look(first))
                .unwrap_or(line.len())
        };
        let (fg, bg, inverse) = look(first);
        let (mut fg, mut bg) = (resolve(fg, palette.text), resolve(bg, palette.background));
        if inverse {
            std::mem::swap(&mut fg, &mut bg);
        }
        let at = Rect::new(x0 + col as f32 * cell, y, (end - col) as f32 * cell, line_h);
        if first.bg != TermColor::Default || inverse {
            ui.fill(at, bg)?;
        }
        let mut text = String::new();
        for c in &line[col..end] {
            push_text(c, &mut text);
        }
        if !text.trim().is_empty() {
            ui.text(&text, at, font, fg, Align::Start)?;
        }
        col = end.max(col + 1);
    }
    Ok(())
}

/// draws the panel. true when it has the keyboard.
pub fn show(
    ui: &mut Ui<'_, Id>,
    shell: &mut Shell,
    rect: Rect,
    texts: &Texts,
    size: f32,
) -> AureaResult<bool> {
    let palette = ui.theme.palette;
    let font = Font::new(MONO, size);
    let line_h = (size * 1.38).round();
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
    let Some(term) = &shell.term else {
        return Ok(false);
    };
    if !term.is_running() || term.is_closed() {
        shell.state = State::Ended;
    }
    let cell = ui.text_width("M", &font).max(1.0);
    let cols = ((rect.width - BAR - 2.0 * PAD) / cell).floor().max(10.0) as u16;
    let rows = ((rect.height - 2.0 * PAD) / line_h).floor().max(2.0) as u16;
    if shell.size != (cols, rows) {
        shell.size = (cols, rows);
        let _ = term.resize(cols, rows);
    }
    let ended = shell.state == State::Ended;
    // only the lines in sight are copied out, never the whole scrollback
    let head = term.window(0, 0);
    let total = head.lines;
    let height = (total + usize::from(ended)) as f32 * line_h + 2.0 * PAD;
    // scrolled back while old lines fall off the top: every line moved up
    // that much, so the view moves with them, at once, not eased
    let gone = head.dropped.saturating_sub(shell.dropped);
    shell.dropped = head.dropped;
    let shifted = !shell.follow && gone > 0;
    if shifted {
        shell.scroll = (shell.scroll - gone as f32 * line_h).max(0.0);
    }
    if shell.follow {
        shell.scroll = (height - rect.height).max(0.0);
    }
    let mut style = ScrollStyle::new(ui.theme);
    style.smooth = !shifted;
    let mut focused = false;
    let mut app_keys = shell.app_keys;
    ScrollArea::new(&mut shell.scroll, height)
        .style(style)
        .show(ui, Id::Terminal, rect, |ui, content, seen| {
            let hit = Rect::new(seen.x, seen.y, (seen.width - BAR).max(0.0), seen.height);
            focused = ui
                .interact(Id::Terminal, 1, hit, Sense::Click, true)
                .focused;
            let first = ((seen.y - content.y - PAD) / line_h).floor().max(0.0) as usize;
            let count = (seen.height / line_h).ceil() as usize + 1;
            let x0 = content.x + PAD;
            let window = term.window(first, count);
            app_keys = window.application_cursor_keys;
            for (i, line) in window.rows.iter().enumerate() {
                let y = content.y + PAD + (first + i) as f32 * line_h;
                draw_row(ui, line, (x0, y), (cell, line_h), &font)?;
            }
            if ended {
                let y = content.y + PAD + window.lines as f32 * line_h;
                let row = Rect::new(x0, y, content.width, line_h);
                let small = ui.theme.font(12.0);
                ui.text(
                    texts.tr("the shell ended, press enter for a new one"),
                    row,
                    &small,
                    palette.dim,
                    Align::Start,
                )?;
            } else if focused && window.cursor_visible {
                let (line, col) = window.cursor;
                let col = col.min(usize::from(cols).saturating_sub(1));
                let at = Rect::new(
                    x0 + col as f32 * cell,
                    content.y + PAD + line as f32 * line_h,
                    cell,
                    line_h,
                );
                ui.fill(at, palette.text.with_alpha(120))?;
            }
            Ok(())
        })?;
    shell.app_keys = app_keys;
    // scrolled back to the bottom: follow again
    shell.follow = shell.scroll + rect.height >= height - line_h;
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
        let alt = Modifiers { alt: true, ..none };
        let shift = Modifiers {
            shift: true,
            ..none
        };
        let sent = |k, m| bytes(&key(k, m), false);
        assert_eq!(sent(KeyCode::Enter, none).as_deref(), Some("\r"));
        assert_eq!(sent(KeyCode::Up, none).as_deref(), Some("\x1b[A"));
        assert_eq!(sent(KeyCode::C, ctrl).as_deref(), Some("\x03"));
        assert_eq!(sent(KeyCode::A, none), None, "letters come as text");
        assert_eq!(sent(KeyCode::B, alt).as_deref(), Some("\x1bb"));
        assert_eq!(sent(KeyCode::Backspace, alt).as_deref(), Some("\x1b\x7f"));
        assert_eq!(sent(KeyCode::Left, ctrl).as_deref(), Some("\x1b[1;5D"));
        assert_eq!(sent(KeyCode::Tab, shift).as_deref(), Some("\x1b[Z"));
        let typed = WindowEvent::TextInput {
            text: "ls\u{3}".into(),
        };
        assert_eq!(bytes(&typed, false).as_deref(), Some("ls"));
    }

    #[test]
    fn app_cursor_keys_use_esc_o() {
        let none = Modifiers::default();
        let ctrl = Modifiers { ctrl: true, ..none };
        assert_eq!(
            bytes(&key(KeyCode::Up, none), true).as_deref(),
            Some("\x1bOA")
        );
        assert_eq!(
            bytes(&key(KeyCode::Up, ctrl), true).as_deref(),
            Some("\x1b[1;5A"),
            "modified arrows keep the CSI form"
        );
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
