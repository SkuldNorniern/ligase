//! brackets and quotes typed in pairs, and the matching bracket for the
//! caret.

use resolvase::TextSnapshot;

use super::cursor::Selection;
use super::lines::Change;

/// how far the bracket match looks each way.
const MATCH_REACH: usize = 16 * 1024;

pub fn closer(open: char) -> Option<char> {
    Some(match open {
        '(' => ')',
        '[' => ']',
        '{' => '}',
        '"' => '"',
        '\'' => '\'',
        '`' => '`',
        _ => return None,
    })
}

pub fn char_before(text: &TextSnapshot, at: usize) -> Option<char> {
    let mut from = at.saturating_sub(4);
    while !text.is_char_boundary(from) {
        from += 1;
    }
    text.slice(from..at).ok()?.chars().next_back()
}

pub fn char_after(text: &TextSnapshot, at: usize) -> Option<char> {
    let mut to = (at + 4).min(text.len());
    while !text.is_char_boundary(to) {
        to -= 1;
    }
    text.slice(at..to).ok()?.chars().next()
}

/// what typing `typed` does when it is a bracket or quote: wrap the
/// selection, put in a pair, or step over the closer already there.
pub fn typed(text: &TextSnapshot, sel: Selection, typed: &str) -> Option<Change> {
    let mut chars = typed.chars();
    let c = chars.next()?;
    if chars.next().is_some() {
        return None;
    }
    let range = sel.range();
    let after = char_after(text, range.end);
    if sel.is_empty() && after == Some(c) && matches!(c, ')' | ']' | '}' | '"' | '\'' | '`') {
        return Some(Change {
            edits: Vec::new(),
            sel: Selection::caret(sel.head + c.len_utf8()),
        });
    }
    let close = closer(c)?;
    if !sel.is_empty() {
        let a = c.len_utf8();
        return Some(Change {
            edits: vec![
                (range.start..range.start, c.to_string()),
                (range.end..range.end, close.to_string()),
            ],
            sel: Selection {
                anchor: sel.anchor + a,
                head: sel.head + a,
            },
        });
    }
    // only where a pair makes sense: before a gap or a closer
    let free = after.is_none_or(|n| n.is_whitespace() || matches!(n, ')' | ']' | '}' | ',' | ';'));
    let quote = c == close;
    // a quote after a word is an apostrophe, or closes a string
    let before = char_before(text, range.start);
    let word = before.is_some_and(|p| p.is_alphanumeric() || p == '_' || p == c);
    if !free || (quote && word) {
        return None;
    }
    Some(Change {
        edits: vec![(range.clone(), format!("{c}{close}"))],
        sel: Selection::caret(range.start + c.len_utf8()),
    })
}

/// backspace between an empty pair takes both.
pub fn backspace(text: &TextSnapshot, at: usize) -> Option<Change> {
    let open = char_before(text, at)?;
    let close = char_after(text, at)?;
    (closer(open) == Some(close)).then(|| Change {
        edits: vec![(at - open.len_utf8()..at + close.len_utf8(), String::new())],
        sel: Selection::caret(at - open.len_utf8()),
    })
}

/// the bracket matching the one just after or just before the caret, as
/// (this bracket, its match). looks a bounded way only.
pub fn matching(text: &TextSnapshot, at: usize) -> Option<(usize, usize)> {
    let pick = |c: char| matches!(c, '(' | ')' | '[' | ']' | '{' | '}');
    let (here, c) = match char_after(text, at) {
        Some(c) if pick(c) => (at, c),
        _ => {
            let c = char_before(text, at).filter(|c| pick(*c))?;
            (at - c.len_utf8(), c)
        }
    };
    let (open, close, forward) = match c {
        '(' => ('(', ')', true),
        '[' => ('[', ']', true),
        '{' => ('{', '}', true),
        ')' => ('(', ')', false),
        ']' => ('[', ']', false),
        _ => ('{', '}', false),
    };
    let mut depth = 0usize;
    if forward {
        let mut end = (here + MATCH_REACH).min(text.len());
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        let ahead = text.slice(here..end).ok()?;
        for (i, ch) in ahead.char_indices() {
            if ch == open {
                depth += 1;
            } else if ch == close {
                depth -= 1;
                if depth == 0 {
                    return Some((here, here + i));
                }
            }
        }
    } else {
        let mut from = (here + 1).saturating_sub(MATCH_REACH);
        while !text.is_char_boundary(from) {
            from += 1;
        }
        let behind = text.slice(from..here + 1).ok()?;
        for (i, ch) in behind.char_indices().rev() {
            if ch == close {
                depth += 1;
            } else if ch == open {
                depth -= 1;
                if depth == 0 {
                    return Some((here, from + i));
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use resolvase::TextBuffer;

    fn snap(text: &str) -> TextSnapshot {
        TextBuffer::from_text(text).snapshot()
    }

    #[test]
    fn pairs_go_in_where_they_fit() {
        let t = snap("x  y");
        let c = typed(&t, Selection::caret(2), "(").unwrap();
        assert_eq!(c.edits, vec![(2..2, "()".to_owned())]);
        assert_eq!(c.sel, Selection::caret(3));
        assert!(
            typed(&t, Selection::caret(3), "(").is_none(),
            "before a word"
        );
        assert!(typed(&snap("it"), Selection::caret(2), "'").is_none());
        assert!(typed(&t, Selection::caret(2), "a").is_none());
    }

    #[test]
    fn closer_steps_over_and_selection_wraps() {
        let t = snap("()");
        let c = typed(&t, Selection::caret(1), ")").unwrap();
        assert!(c.edits.is_empty());
        assert_eq!(c.sel, Selection::caret(2));
        let t = snap("ab");
        let c = typed(&t, Selection { anchor: 0, head: 2 }, "\"").unwrap();
        assert_eq!(c.edits.len(), 2);
        assert_eq!(c.sel, Selection { anchor: 1, head: 3 });
    }

    #[test]
    fn backspace_takes_an_empty_pair() {
        let c = backspace(&snap("[]"), 1).unwrap();
        assert_eq!(c.edits, vec![(0..2, String::new())]);
        assert!(backspace(&snap("[x]"), 1).is_none());
    }

    #[test]
    fn brackets_match_both_ways() {
        let t = snap("f(a[1], {b})");
        assert_eq!(matching(&t, 1), Some((1, 11)));
        assert_eq!(matching(&t, 12), Some((11, 1)));
        assert_eq!(matching(&t, 3), Some((3, 5)));
        assert_eq!(matching(&t, 0), None);
        assert_eq!(matching(&snap("(("), 0), None);
    }
}
