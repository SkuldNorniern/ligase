//! window events to editor actions. only plain editing keys live here;
//! shortcuts like save and undo go through the app keymap.

use aurea::{KeyCode, Modifiers, WindowEvent};

use super::cursor::Motion;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    /// move the caret; true keeps the anchor (shift).
    Move(Motion, bool),
    Insert(String),
    Backspace,
    Delete,
    Newline,
    Indent,
    /// drop the selection, keep the caret.
    Collapse,
    /// ime text being composed; empty ends it.
    Preedit(String, Option<usize>),
}

/// the action for `event`, or none when the editor leaves it alone.
/// `page` is how many lines fit, for page up and down.
pub fn action(event: &WindowEvent, page: usize, has_selection: bool) -> Option<Action> {
    match event {
        WindowEvent::KeyInput {
            key,
            pressed: true,
            modifiers,
        } => key_action(*key, *modifiers, page, has_selection),
        WindowEvent::TextInput { text } => {
            // ctrl+letter can arrive as a control char; those are shortcuts
            let typed: String = text.chars().filter(|c| !c.is_control()).collect();
            (!typed.is_empty()).then_some(Action::Insert(typed))
        }
        WindowEvent::ImePreedit { text, cursor } => Some(Action::Preedit(text.clone(), *cursor)),
        _ => None,
    }
}

fn key_action(key: KeyCode, m: Modifiers, page: usize, has_selection: bool) -> Option<Action> {
    let mac = cfg!(target_os = "macos");
    // word moves: ctrl on windows and linux, option on mac
    let word = if mac { m.alt } else { m.ctrl };
    // line and document jumps with cmd on mac
    let jump = mac && m.meta;
    let shift = m.shift;
    let motion = match key {
        KeyCode::Left if jump => Motion::LineStart,
        KeyCode::Right if jump => Motion::LineEnd,
        KeyCode::Up if jump => Motion::DocStart,
        KeyCode::Down if jump => Motion::DocEnd,
        KeyCode::Left if word => Motion::WordLeft,
        KeyCode::Right if word => Motion::WordRight,
        KeyCode::Left => Motion::Left,
        KeyCode::Right => Motion::Right,
        KeyCode::Up => Motion::Up,
        KeyCode::Down => Motion::Down,
        KeyCode::PageUp => Motion::PageUp(page),
        KeyCode::PageDown => Motion::PageDown(page),
        KeyCode::Home if m.ctrl => Motion::DocStart,
        KeyCode::End if m.ctrl => Motion::DocEnd,
        KeyCode::Home => Motion::LineStart,
        KeyCode::End => Motion::LineEnd,
        _ => {
            return match key {
                KeyCode::Backspace => Some(Action::Backspace),
                KeyCode::Delete => Some(Action::Delete),
                KeyCode::Enter if !m.ctrl && !m.alt && !m.meta => Some(Action::Newline),
                KeyCode::Tab if !shift && !m.ctrl && !m.alt => Some(Action::Indent),
                KeyCode::Escape if has_selection => Some(Action::Collapse),
                _ => None,
            };
        }
    };
    Some(Action::Move(motion, shift))
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
    fn keys_map_to_actions() {
        let none = Modifiers::default();
        let shift = Modifiers {
            shift: true,
            ..none
        };
        assert_eq!(
            action(&key(KeyCode::Left, shift), 10, false),
            Some(Action::Move(Motion::Left, true))
        );
        assert_eq!(
            action(&key(KeyCode::PageDown, none), 10, false),
            Some(Action::Move(Motion::PageDown(10), false))
        );
        assert_eq!(action(&key(KeyCode::Escape, none), 10, false), None);
        assert_eq!(
            action(&key(KeyCode::Escape, none), 10, true),
            Some(Action::Collapse)
        );
        assert_eq!(
            action(&key(KeyCode::A, none), 10, false),
            None,
            "letters come as text"
        );
        let released = WindowEvent::KeyInput {
            key: KeyCode::Left,
            pressed: false,
            modifiers: none,
        };
        assert_eq!(action(&released, 10, false), None);
    }

    #[test]
    fn control_chars_are_not_typed() {
        let ev = WindowEvent::TextInput {
            text: "\u{1a}".into(),
        };
        assert_eq!(action(&ev, 1, false), None);
        let ev = WindowEvent::TextInput { text: "가".into() };
        assert_eq!(action(&ev, 1, false), Some(Action::Insert("가".into())));
    }
}
