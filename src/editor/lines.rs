//! edits on whole lines: move, copy, delete, comment, indent. each one
//! works out its edits and where the selection lands; nothing is applied
//! here.

use std::ops::Range;

use resolvase::TextSnapshot;

use super::cursor::{Selection, line_end};

/// edits to make, then the selection to show.
#[derive(Debug, PartialEq, Eq)]
pub struct Change {
    pub edits: Vec<(Range<usize>, String)>,
    pub sel: Selection,
}

/// first and last line the selection covers. a selection ending at a line
/// start leaves that line out, like a triple click.
pub fn block(text: &TextSnapshot, sel: Selection) -> (usize, usize) {
    let range = sel.range();
    let a = text.line_of(range.start).unwrap_or(0);
    let mut b = text.line_of(range.end).unwrap_or(a);
    if b > a && text.line_start(b).is_ok_and(|s| s == range.end) {
        b -= 1;
    }
    (a, b)
}

fn start(text: &TextSnapshot, line: usize) -> usize {
    text.line_start(line).unwrap_or(text.len())
}

/// lines of `range` without their breaks.
fn split(text: &TextSnapshot, range: Range<usize>) -> Vec<String> {
    let s = text.slice(range).unwrap_or_default();
    s.split('\n')
        .map(|l| l.strip_suffix('\r').unwrap_or(l).to_owned())
        .collect()
}

fn shifted(sel: Selection, by: isize) -> Selection {
    Selection {
        anchor: sel.anchor.saturating_add_signed(by),
        head: sel.head.saturating_add_signed(by),
    }
}

/// swaps the block with the line above or below.
pub fn move_lines(text: &TextSnapshot, sel: Selection, up: bool, lb: &str) -> Option<Change> {
    let (a, b) = block(text, sel);
    let (region, by) = if up {
        let prev = a.checked_sub(1)?;
        let region = start(text, prev)..line_end(text, b);
        let mut lines = split(text, region.clone());
        let first = lines.remove(0);
        let by = -((first.len() + lb.len()) as isize);
        lines.push(first);
        ((region, lines.join(lb)), by)
    } else {
        if b + 1 >= text.line_count() {
            return None;
        }
        let region = start(text, a)..line_end(text, b + 1);
        let mut lines = split(text, region.clone());
        let last = lines.pop()?;
        let by = (last.len() + lb.len()) as isize;
        lines.insert(0, last);
        ((region, lines.join(lb)), by)
    };
    Some(Change {
        edits: vec![region],
        sel: shifted(sel, by),
    })
}

/// copies the block above or below itself; the selection goes with the
/// copy that moved.
pub fn copy_lines(text: &TextSnapshot, sel: Selection, up: bool, lb: &str) -> Change {
    let (a, b) = block(text, sel);
    let end = line_end(text, b);
    let lines = text.slice(start(text, a)..end).unwrap_or_default();
    let by = if up {
        0
    } else {
        (lines.len() + lb.len()) as isize
    };
    Change {
        edits: vec![(end..end, format!("{lb}{lines}"))],
        sel: shifted(sel, by),
    }
}

/// removes the block and its line break.
pub fn delete_lines(text: &TextSnapshot, sel: Selection) -> Change {
    let (a, b) = block(text, sel);
    let (range, caret) = if b + 1 < text.line_count() {
        (start(text, a)..start(text, b + 1), start(text, a))
    } else if a > 0 {
        // last line: take the break before it instead
        (line_end(text, a - 1)..text.len(), start(text, a - 1))
    } else {
        (0..text.len(), 0)
    };
    Change {
        edits: vec![(range, String::new())],
        sel: Selection::caret(caret),
    }
}

/// line comment marker for a language, if it has one.
pub fn comment_for(language: Option<&str>) -> Option<&'static str> {
    Some(match language? {
        "rust" | "c" | "verilog" | "systemverilog" | "bsv" | "oxygen" => "//",
        "python" | "toml" | "yaml" | "sdc" => "#",
        "vhdl" | "bh" => "--",
        _ => return None,
    })
}

/// where an offset goes once `edits` (sorted, not overlapping) are made.
/// `high` is the far end of a selection: text put right at it pushes it on.
fn map(at: usize, edits: &[(Range<usize>, String)], high: bool) -> usize {
    let mut out = at as isize;
    for (range, new) in edits {
        if range.start > at || (range.start == at && range.is_empty() && !high) {
            break;
        }
        if at < range.end {
            // inside removed text: lands at its start
            return (range.start as isize + (out - at as isize)).max(0) as usize;
        }
        out += new.len() as isize - range.len() as isize;
    }
    out.max(0) as usize
}

fn mapped(sel: Selection, edits: &[(Range<usize>, String)]) -> Selection {
    let low = map(sel.range().start, edits, sel.is_empty());
    let high = map(sel.range().end, edits, true);
    if sel.anchor <= sel.head {
        Selection {
            anchor: low,
            head: high,
        }
    } else {
        Selection {
            anchor: high,
            head: low,
        }
    }
}

fn leading(line: &str) -> usize {
    line.len() - line.trim_start_matches([' ', '\t']).len()
}

/// comments the block out, or back in when every line with text already is.
pub fn toggle_comment(text: &TextSnapshot, sel: Selection, mark: &str) -> Option<Change> {
    let (a, b) = block(text, sel);
    let lines: Vec<(usize, String)> = (a..=b)
        .map(|l| {
            let s = start(text, l);
            (s, text.slice(s..line_end(text, l)).unwrap_or_default())
        })
        .filter(|(_, l)| !l.trim().is_empty())
        .collect();
    if lines.is_empty() {
        return None;
    }
    let commented = lines.iter().all(|(_, l)| l.trim_start().starts_with(mark));
    let edits: Vec<_> = if commented {
        lines
            .iter()
            .map(|(s, l)| {
                let at = s + leading(l);
                let rest = &l[leading(l) + mark.len()..];
                let len = mark.len() + usize::from(rest.starts_with(' '));
                (at..at + len, String::new())
            })
            .collect()
    } else {
        let indent = lines.iter().map(|(_, l)| leading(l)).min().unwrap_or(0);
        lines
            .iter()
            .map(|(s, _)| (s + indent..s + indent, format!("{mark} ")))
            .collect()
    };
    let sel = mapped(sel, &edits);
    Some(Change { edits, sel })
}

/// puts `unit` before every line of the block; blank lines stay blank when
/// there is more than one line.
pub fn indent_lines(text: &TextSnapshot, sel: Selection, unit: &str) -> Change {
    let (a, b) = block(text, sel);
    let edits: Vec<_> = (a..=b)
        .filter(|&l| a == b || line_end(text, l) > start(text, l))
        .map(|l| {
            let s = start(text, l);
            (s..s, unit.to_owned())
        })
        .collect();
    let sel = mapped(sel, &edits);
    Change { edits, sel }
}

/// takes up to one `unit` of spaces, or one tab, off every line of the
/// block.
pub fn outdent_lines(text: &TextSnapshot, sel: Selection, unit: &str) -> Change {
    let (a, b) = block(text, sel);
    let edits: Vec<_> = (a..=b)
        .filter_map(|l| {
            let s = start(text, l);
            let head = text
                .slice(s..line_end(text, l).min(s + unit.len()))
                .unwrap_or_default();
            let take = if head.starts_with('\t') {
                1
            } else {
                head.len() - head.trim_start_matches(' ').len()
            };
            (take > 0).then(|| (s..s + take, String::new()))
        })
        .collect();
    let sel = mapped(sel, &edits);
    Change { edits, sel }
}

#[cfg(test)]
mod tests {
    use super::*;
    use resolvase::TextBuffer;

    fn snap(text: &str) -> TextSnapshot {
        TextBuffer::from_text(text).snapshot()
    }

    fn run(text: &str, change: &Change) -> String {
        let mut out = text.to_owned();
        for (range, new) in change.edits.iter().rev() {
            out.replace_range(range.clone(), new);
        }
        out
    }

    #[test]
    fn move_down_and_up_keep_the_caret_on_its_line() {
        let src = "a\nbb\nccc";
        let t = snap(src);
        let c = move_lines(&t, Selection::caret(3), false, "\n").unwrap();
        assert_eq!(run(src, &c), "a\nccc\nbb");
        assert_eq!(c.sel, Selection::caret(7));
        let c = move_lines(&t, Selection::caret(3), true, "\n").unwrap();
        assert_eq!(run(src, &c), "bb\na\nccc");
        assert_eq!(c.sel, Selection::caret(1));
        assert!(move_lines(&t, Selection::caret(0), true, "\n").is_none());
        assert!(move_lines(&t, Selection::caret(6), false, "\n").is_none());
    }

    #[test]
    fn move_keeps_crlf() {
        let src = "a\r\nb\r\n";
        let t = snap(src);
        let c = move_lines(&t, Selection::caret(0), false, "\r\n").unwrap();
        assert_eq!(run(src, &c), "b\r\na\r\n");
    }

    #[test]
    fn selection_ending_at_line_start_leaves_that_line() {
        let t = snap("a\nb\nc");
        assert_eq!(block(&t, Selection { anchor: 0, head: 2 }), (0, 0));
        assert_eq!(block(&t, Selection { anchor: 0, head: 3 }), (0, 1));
    }

    #[test]
    fn copy_and_delete() {
        let src = "x\ny";
        let t = snap(src);
        let c = copy_lines(&t, Selection::caret(1), false, "\n");
        assert_eq!(run(src, &c), "x\nx\ny");
        assert_eq!(c.sel, Selection::caret(3));
        let c = copy_lines(&t, Selection::caret(1), true, "\n");
        assert_eq!(c.sel, Selection::caret(1));
        let c = delete_lines(&t, Selection::caret(0));
        assert_eq!(run(src, &c), "y");
        let c = delete_lines(&t, Selection::caret(3));
        assert_eq!(run(src, &c), "x");
        assert_eq!(
            run("only", &delete_lines(&snap("only"), Selection::caret(2))),
            ""
        );
    }

    #[test]
    fn comment_toggles_at_the_shallowest_indent() {
        let src = "    a\n\n  b\n";
        let t = snap(src);
        let sel = Selection {
            anchor: 0,
            head: 10,
        };
        let c = toggle_comment(&t, sel, "//").unwrap();
        let done = run(src, &c);
        assert_eq!(done, "  //   a\n\n  // b\n");
        let t2 = snap(&done);
        let back = toggle_comment(
            &t2,
            Selection {
                anchor: 0,
                head: 16,
            },
            "//",
        )
        .unwrap();
        assert_eq!(run(&done, &back), src);
        assert!(toggle_comment(&snap("  \n"), Selection::caret(0), "#").is_none());
    }

    #[test]
    fn caret_follows_its_text_through_a_comment() {
        let t = snap("ab");
        let c = toggle_comment(&t, Selection::caret(1), "#").unwrap();
        assert_eq!(c.sel, Selection::caret(3));
    }

    #[test]
    fn indent_and_outdent() {
        let src = "a\n\n\tb\n  c";
        let t = snap(src);
        let all = Selection {
            anchor: 0,
            head: src.len(),
        };
        let c = indent_lines(&t, all, "    ");
        assert_eq!(run(src, &c), "    a\n\n    \tb\n      c");
        assert_eq!(
            c.sel,
            Selection {
                anchor: 0,
                head: src.len() + 12
            }
        );
        let c = outdent_lines(&t, all, "    ");
        assert_eq!(run(src, &c), "a\n\nb\nc");
    }
}
