//! the problems panel: every diagnostic of the open files in one list,
//! worst first. a click goes there.

use std::path::{Path, PathBuf};

use aurea::AureaResult;
use aurea::render::Rect;
use aurea::ui::{Align, ListView, Ui};

use crate::app::Id;
use crate::document::Documents;
use crate::lang::Texts;
use crate::lsp::Severity;

const ROW: f32 = 22.0;
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
    scroll: f32,
    picked: Option<usize>,
}

impl Problems {
    /// lists the diagnostics of every open document, worst first, then by
    /// file and place.
    pub fn gather(&mut self, docs: &Documents) {
        let mut rows = Vec::new();
        for doc in docs.all() {
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
                    message: diag.message.lines().next().unwrap_or_default().to_owned(),
                });
                if rows.len() >= MAX_ROWS {
                    break;
                }
            }
        }
        rows.sort_by(|a, b| {
            (a.severity, &a.path, a.line, a.column).cmp(&(b.severity, &b.path, b.line, b.column))
        });
        self.rows = rows;
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
        let hint = texts.tr("no problems in the open files");
        ui.text(hint, inner, &small, palette.dim, Align::Center)?;
        return Ok(None);
    }
    let rows = &problems.rows;
    let mut picked = problems.picked;
    let response = ListView::new(&mut problems.scroll, rows.len(), ROW)
        .selected(&mut picked)
        .show(ui, Id::ProblemList, inner, |ui, row| {
            let Some(p) = rows.get(row.index) else {
                return Ok(());
            };
            if row.picked {
                ui.fill(row.rect, palette.selection)?;
            } else if row.hovered {
                ui.fill(row.rect, palette.hover)?;
            }
            let colour = match p.severity {
                Severity::Error => palette.danger,
                Severity::Warning => palette.warning,
                Severity::Info => palette.accent,
                Severity::Hint => palette.dim,
            };
            let (mark, rest) = row.rect.inset_xy(4.0, 0.0).split_left(10.0, 6.0);
            let dot = Rect::new(mark.x + 2.0, mark.y + ROW / 2.0 - 3.0, 6.0, 6.0);
            ui.round(dot, 3.0, colour)?;
            let shown = p.path.strip_prefix(root).unwrap_or(&p.path);
            let place = format!("{}:{}:{}", shown.display(), p.line + 1, p.column + 1);
            let (left, right) =
                rest.split_left(ui.text_width(&place, &small).min(rest.width * 0.4), 10.0);
            ui.text_clipped(&place, left, &small, palette.dim, Align::Start)?;
            ui.text_clipped(&p.message, right, &font, palette.text, Align::Start)?;
            Ok(())
        })?;
    problems.picked = picked;
    Ok(response
        .row_clicked
        .or(response.activated.then_some(picked).flatten())
        .and_then(|i| problems.rows.get(i).cloned()))
}

#[cfg(test)]
mod tests {
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
            problems.rows[0].message, "e",
            "first line only, errors first"
        );
        assert_eq!((problems.rows[0].line, problems.rows[0].column), (2, 0));
        assert_eq!(problems.rows[1].severity, Severity::Warning);
    }
}
