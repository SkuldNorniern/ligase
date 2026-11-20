//! the problems panel: every diagnostic of the open files in one list,
//! worst first. a click goes there.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use aurea::AureaResult;
use aurea::render::{Color, Rect};
use aurea::ui::{Align, ListView, Palette, Ui};

use crate::app::Id;
use crate::document::Documents;
use crate::icons;
use crate::lang::Texts;
use crate::lsp::Severity;

const ROW: f32 = 24.0;
const PAD: f32 = 8.0;
/// rows the panel lists; a file with more shows the first ones.
const MAX_ROWS: usize = 5000;

/// one row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Problem {
    pub path: PathBuf,
    pub severity: Severity,
    /// line and column from 0, the column in chars.
    pub line: usize,
    pub column: usize,
    pub message: String,
}

#[derive(Default)]
pub struct Problems {
    pub rows: Vec<Problem>,
    /// the documents' stamp the rows were gathered at.
    seen: Option<u64>,
    scroll: f32,
    picked: Option<usize>,
}

impl Problems {
    /// lists the diagnostics of every open document, worst first, then by
    /// file and place.
    pub fn gather(&mut self, docs: &Documents) {
        // nothing moved since last time: the rows stand
        let stamp = docs.stamp();
        if self.seen == Some(stamp) {
            return;
        }
        self.seen = Some(stamp);
        let mut rows = Vec::new();
        'docs: for doc in docs.all() {
            let text = doc.buffer.text();
            for diag in &doc.diags {
                let Ok(at) = text.point(diag.range.start, resolvase::Encoding::Utf32) else {
                    continue;
                };
                rows.push(Problem {
                    path: doc.path.clone(),
                    severity: diag.severity,
                    line: at.line,
                    column: at.column,
                    message: one_line(&diag.message),
                });
                if rows.len() >= MAX_ROWS {
                    break 'docs;
                }
            }
        }
        rows.sort_by(|a, b| {
            (a.severity, &a.path, a.line, a.column).cmp(&(b.severity, &b.path, b.line, b.column))
        });
        self.rows = rows;
    }
}

/// one file's rows, as indices into the row list, in the order they came.
struct Group<'a> {
    path: &'a Path,
    rows: Vec<usize>,
}

/// one list row: a file's header, or a problem.
#[derive(Clone, Copy)]
enum Line {
    Head(usize),
    Row(usize),
}

/// the rows grouped by file, files in the order they first came (worst
/// first, as the rows are).
fn group(rows: &[Problem]) -> Vec<Group<'_>> {
    let mut groups: Vec<Group<'_>> = Vec::new();
    let mut at: HashMap<&Path, usize> = HashMap::new();
    for (i, row) in rows.iter().enumerate() {
        let path = row.path.as_path();
        let g = match at.get(path) {
            Some(&g) => g,
            None => {
                groups.push(Group {
                    path,
                    rows: Vec::new(),
                });
                at.insert(path, groups.len() - 1);
                groups.len() - 1
            }
        };
        groups[g].rows.push(i);
    }
    groups
}

/// the list rows: each file's header, then its problems.
fn lines(groups: &[Group<'_>]) -> Vec<Line> {
    let mut lines = Vec::new();
    for (g, group) in groups.iter().enumerate() {
        lines.push(Line::Head(g));
        lines.extend(group.rows.iter().map(|&r| Line::Row(r)));
    }
    lines
}

fn severity_colour(palette: &Palette, severity: Severity) -> Color {
    match severity {
        Severity::Error => palette.danger,
        Severity::Warning => palette.warning,
        Severity::Info => palette.accent,
        Severity::Hint => palette.dim,
    }
}

/// draws the panel; gives the row clicked.
pub fn show(
    ui: &mut Ui<'_, Id>,
    problems: &mut Problems,
    root: &Path,
    rect: Rect,
    texts: &Texts,
) -> AureaResult<Option<Problem>> {
    let palette = ui.theme.palette;
    let font = ui.theme.font(13.0);
    let small = ui.theme.font(12.0);
    let inner = rect.inset(PAD);
    if problems.rows.is_empty() {
        let hint = texts.tr("No problems");
        ui.text(hint, inner, &font, palette.dim, Align::Center)?;
        return Ok(None);
    }
    let rows = &problems.rows;
    let groups = group(rows);
    let lines = lines(&groups);
    let before = problems.picked.and_then(|r| {
        lines
            .iter()
            .position(|l| matches!(l, Line::Row(i) if *i == r))
    });
    let mut picked = before;
    let response = ListView::new(&mut problems.scroll, lines.len(), ROW)
        .selected(&mut picked)
        .show(ui, Id::ProblemList, inner, |ui, row| {
            let Some(line) = lines.get(row.index).copied() else {
                return Ok(());
            };
            match line {
                Line::Head(g) => {
                    let Some(group) = groups.get(g) else {
                        return Ok(());
                    };
                    // a count pill per severity present, worst first
                    let mut pills = Vec::new();
                    for severity in [
                        Severity::Error,
                        Severity::Warning,
                        Severity::Info,
                        Severity::Hint,
                    ] {
                        let n = group
                            .rows
                            .iter()
                            .filter(|&&r| rows.get(r).is_some_and(|p| p.severity == severity))
                            .count();
                        if n > 0 {
                            pills.push((severity, n.to_string()));
                        }
                    }
                    let mut right = row.rect.right() - 8.0;
                    for (severity, text) in &pills {
                        let colour = severity_colour(&palette, *severity);
                        let w = (ui.text_width(text, &small) + 12.0).max(ROW - 8.0);
                        let pill = Rect::new(right - w, row.rect.y + 4.0, w, ROW - 8.0);
                        ui.round(pill, (ROW - 8.0) / 2.0, colour.fade(0.16))?;
                        ui.text(text, pill, &small, colour, Align::Center)?;
                        right -= w + 4.0;
                    }
                    let icon_at = Rect::new(
                        row.rect.x + 6.0,
                        row.rect.y + (ROW - 16.0) / 2.0,
                        16.0,
                        16.0,
                    );
                    let icon = icons::for_path(group.path, false, false);
                    icons::draw(ui, icon, icon_at, palette.dim)?;
                    let shown = group.path.strip_prefix(root).unwrap_or(group.path);
                    let title = shown
                        .file_name()
                        .map(|n| n.to_string_lossy())
                        .unwrap_or_default();
                    let dir = shown
                        .parent()
                        .map(|p| p.to_string_lossy())
                        .unwrap_or_default();
                    let body = Rect::new(
                        icon_at.right() + 6.0,
                        row.rect.y,
                        (right - icon_at.right() - 6.0).max(0.0),
                        ROW,
                    );
                    let name_w = ui.text_width(&title, &font).min(body.width);
                    let (name_at, folder_at) = body.split_left(name_w, 8.0);
                    ui.text_clipped(&title, name_at, &font, palette.text, Align::Start)?;
                    if !dir.is_empty() {
                        ui.text_clipped(&dir, folder_at, &small, palette.dim, Align::Start)?;
                    }
                }
                Line::Row(r) => {
                    let Some(p) = rows.get(r) else {
                        return Ok(());
                    };
                    let lit = row.rect.inset_xy(2.0, 1.0);
                    if row.picked {
                        ui.round(lit, 6.0, palette.selection)?;
                    } else if row.hovered {
                        ui.round(lit, 6.0, palette.hover)?;
                    }
                    // a severity dot under the file's icon, then the message
                    // and the place on the right
                    let dot = Rect::new(row.rect.x + 14.0, row.rect.y + ROW / 2.0 - 3.0, 6.0, 6.0);
                    ui.round(dot, 3.0, severity_colour(&palette, p.severity))?;
                    let place = format!("{}:{}", p.line + 1, p.column + 1);
                    let place_w = ui.text_width(&place, &small);
                    let text_x = row.rect.x + 28.0;
                    let body = Rect::new(
                        text_x,
                        row.rect.y,
                        (row.rect.right() - 8.0 - place_w - 10.0 - text_x).max(0.0),
                        ROW,
                    );
                    ui.text_clipped(&p.message, body, &font, palette.text, Align::Start)?;
                    let place_at =
                        Rect::new(row.rect.right() - 8.0 - place_w, row.rect.y, place_w, ROW);
                    ui.text(&place, place_at, &small, palette.dim, Align::End)?;
                }
            }
            Ok(())
        })?;
    // a header is no pick: the list moves on to the row beside it
    let on_head = matches!(picked.and_then(|p| lines.get(p)), Some(Line::Head(_)));
    if let (true, Some(p)) = (on_head, picked) {
        let up = before.is_some_and(|b| p < b);
        picked = if up {
            p.checked_sub(1).or(Some(p + 1))
        } else {
            Some(p + 1)
        };
    }
    let row_at = |l: usize| match lines.get(l) {
        Some(Line::Row(r)) => Some(*r),
        _ => None,
    };
    problems.picked = picked.and_then(row_at);
    let clicked = response.row_clicked.filter(|_| !on_head).and_then(row_at);
    let activated = response.activated.then_some(problems.picked).flatten();
    Ok(clicked
        .or(activated)
        .and_then(|i| problems.rows.get(i).cloned()))
}

/// a message on one line: its lines joined, blank ones dropped, cut short.
pub fn one_line(message: &str) -> String {
    let mut out = String::new();
    for line in message.lines().map(str::trim).filter(|l| !l.is_empty()) {
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(line);
        if out.len() > 500 {
            break;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn messages_join_onto_one_line() {
        assert_eq!(
            super::one_line(
                "Try this:
  [apply] exact h

"
            ),
            "Try this: [apply] exact h"
        );
    }

    use super::*;
    use crate::lsp::Diag;
    use std::path::Path;

    #[test]
    fn worst_first_then_by_place() {
        let mut docs = Documents::default();
        let (doc, _) = docs.open(Path::new("a.rs"));
        let view = docs.add_view(doc);
        docs.loaded(doc, Ok("one\ntwo\nthree".to_owned()));
        let revision = docs.doc(doc).unwrap().buffer.revision();
        let diag = |range: std::ops::Range<usize>, severity, message: &str| Diag {
            range,
            severity,
            message: message.to_owned(),
            extra: None,
        };
        docs.set_diags(
            doc,
            revision,
            vec![
                diag(4..7, Severity::Warning, "w"),
                diag(8..9, Severity::Error, "e\nmore"),
            ],
        );
        let _ = view;
        let mut problems = Problems::default();
        problems.gather(&docs);
        assert_eq!(problems.rows.len(), 2);
        assert_eq!(
            problems.rows[0].message, "e more",
            "lines joined, errors first"
        );
        assert_eq!((problems.rows[0].line, problems.rows[0].column), (2, 0));
        assert_eq!(problems.rows[1].severity, Severity::Warning);
    }
}
