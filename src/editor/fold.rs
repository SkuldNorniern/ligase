//! folded ranges and the rows they leave. a fold hides the lines after its
//! first; one that ends on a closing bracket keeps that line in sight, like
//! `fn f() {⋯` over `}`.

use std::ops::Range;

use resolvase::TextSnapshot;

/// lines hidden by folds: sorted, apart, inclusive `(first, last)`.
pub type Hidden = Vec<(usize, usize)>;

/// the first and last hidden line of `fold`, or none when it hides nothing.
pub fn hides(text: &TextSnapshot, fold: &Range<usize>) -> Option<(usize, usize)> {
    let first = text.line_of(fold.start).ok()?;
    let end = text.line_of(fold.end.min(text.len())).ok()?;
    let closes = fold.end > 0
        && text
            .slice(fold.end - 1..fold.end)
            .is_ok_and(|s| matches!(s.as_str(), ")" | "]" | "}"));
    let last = if closes { end.checked_sub(1)? } else { end };
    (last > first).then_some((first + 1, last))
}

pub fn hidden(text: &TextSnapshot, folded: &[Range<usize>]) -> Hidden {
    let mut spans: Hidden = folded.iter().filter_map(|f| hides(text, f)).collect();
    spans.sort_unstable();
    let mut out: Hidden = Vec::with_capacity(spans.len());
    for (a, b) in spans {
        match out.last_mut() {
            Some((_, last)) if a <= *last + 1 => *last = (*last).max(b),
            _ => out.push((a, b)),
        }
    }
    out
}

/// the row `line` is drawn on. a hidden line gives its fold's row.
pub fn row_of(line: usize, hidden: &Hidden) -> usize {
    let mut row = line;
    for &(a, b) in hidden {
        if a > line {
            break;
        }
        if line <= b {
            // inside: the fold line just above it
            return row - (line - a) - 1;
        }
        row -= b - a + 1;
    }
    row
}

/// the line drawn on `row`.
pub fn line_of_row(row: usize, hidden: &Hidden) -> usize {
    let mut line = row;
    for &(a, b) in hidden {
        if a > line {
            break;
        }
        line += b - a + 1;
    }
    line
}

pub fn rows(lines: usize, hidden: &Hidden) -> usize {
    lines - hidden.iter().map(|(a, b)| b - a + 1).sum::<usize>()
}

pub fn is_hidden(line: usize, hidden: &Hidden) -> Option<(usize, usize)> {
    hidden
        .iter()
        .copied()
        .find(|&(a, b)| a <= line && line <= b)
}

/// the parser's fold that starts on `line`, the widest one when several do.
pub fn starting_on(
    text: &TextSnapshot,
    folds: &[Range<usize>],
    line: usize,
) -> Option<Range<usize>> {
    folds
        .iter()
        .filter(|f| text.line_of(f.start).ok() == Some(line) && hides(text, f).is_some())
        .max_by_key(|f| f.end)
        .cloned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use resolvase::TextBuffer;

    fn snap(t: &str) -> TextSnapshot {
        TextBuffer::from_text(t).snapshot()
    }

    #[test]
    fn bracket_folds_keep_their_closing_line() {
        let t = snap("fn f() {\n    a;\n    b;\n}\nx");
        let fold = 7..24;
        assert_eq!(hides(&t, &fold), Some((1, 2)));
        // a section fold hides through its last line
        let t2 = snap("# a\ntext\nmore\n# b");
        assert_eq!(hides(&t2, &(0..13)), Some((1, 2)));
        assert_eq!(hides(&t2, &(0..3)), None, "one line hides nothing");
    }

    #[test]
    fn rows_skip_hidden_lines() {
        let hidden: Hidden = vec![(2, 4), (7, 7)];
        assert_eq!(rows(10, &hidden), 6);
        assert_eq!(row_of(1, &hidden), 1);
        assert_eq!(row_of(5, &hidden), 2);
        assert_eq!(row_of(3, &hidden), 1, "inside goes to the fold line");
        assert_eq!(row_of(8, &hidden), 4);
        assert_eq!(line_of_row(2, &hidden), 5);
        assert_eq!(line_of_row(4, &hidden), 8);
        for line in [0, 1, 5, 6, 8, 9] {
            assert_eq!(line_of_row(row_of(line, &hidden), &hidden), line);
        }
    }

    #[test]
    fn overlapping_folds_merge() {
        let t = snap("a {\nb {\nc\n}\nd\n}\ne");
        let outer = 2..t.len() - 2;
        let inner = 6..11;
        let h = hidden(&t, &[inner.clone(), outer.clone()]);
        assert_eq!(h, vec![(1, 4)]);
        assert_eq!(starting_on(&t, &[inner, outer.clone()], 0), Some(outer));
    }
}
