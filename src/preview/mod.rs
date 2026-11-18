//! a file drawn as what it means instead of as text. each kind is a row in
//! `KINDS` and an arm in the matches below; a new kind adds both, and its
//! parse runs on the worker like the others.

use std::collections::HashMap;

use aurea::AureaResult;
use aurea::render::Rect;
use aurea::ui::Ui;

use crate::app::Id;
use crate::document::ViewId;
use crate::markdown;

mod table;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Markdown,
    Table,
}

/// which languages have a preview, and of what kind.
const KINDS: &[(&str, Kind)] = &[("markdown", Kind::Markdown), ("csv", Kind::Table)];

/// the preview kind for files in `language`; none when there is none.
pub fn kind_for(language: Option<&str>) -> Option<Kind> {
    let language = language?;
    KINDS.iter().find(|(l, _)| *l == language).map(|(_, k)| *k)
}

/// what a click on a preview asks for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Ask {
    /// a link: a url or a path next to the file.
    Open(String),
    /// back to the text.
    Edit,
}

/// a kind's parse of the text, made on the worker.
pub enum Parsed {
    Markdown(Vec<(usize, markdown::parse::Block)>),
    Table(table::Table),
}

/// parses `text` for `kind`. runs on the worker.
pub fn parse(kind: Kind, text: &str) -> Parsed {
    match kind {
        Kind::Markdown => Parsed::Markdown(markdown::parse::parse(text)),
        Kind::Table => Parsed::Table(table::parse(text)),
    }
}

enum Page {
    Markdown(markdown::read::Reader),
    Table(table::TableState),
}

/// one view's preview.
pub struct Preview {
    pub kind: Kind,
    /// the text revision the page was made from.
    pub made_from: Option<resolvase::Revision>,
    /// a parse is out for this revision.
    pub asked: Option<resolvase::Revision>,
    page: Page,
}

impl Preview {
    /// a preview of `kind` that opens at source line `line`.
    pub fn new(kind: Kind, line: Option<usize>) -> Self {
        let page = match kind {
            Kind::Markdown => {
                let mut reader = markdown::read::Reader::default();
                reader.reveal_line = line;
                Page::Markdown(reader)
            }
            Kind::Table => {
                let mut table = table::TableState::default();
                table.reveal_line = line;
                Page::Table(table)
            }
        };
        Self {
            kind,
            made_from: None,
            asked: None,
            page,
        }
    }

    /// a parse for `revision` came back.
    pub fn set(&mut self, revision: resolvase::Revision, parsed: Parsed) {
        self.made_from = Some(revision);
        match (&mut self.page, parsed) {
            (Page::Markdown(reader), Parsed::Markdown(blocks)) => reader.set(blocks),
            (Page::Table(table), Parsed::Table(parsed)) => table.set(parsed),
            // a parse is always of the kind the page was made for
            _ => {}
        }
    }

    /// the source line at the top of the preview, to go back to.
    pub fn top_line(&self) -> Option<usize> {
        match &self.page {
            Page::Markdown(reader) => reader.top_line(),
            Page::Table(table) => table.top_line(),
        }
    }

    /// draws the preview in `rect`; what a click asked for.
    pub fn show(
        &mut self,
        ui: &mut Ui<'_, Id>,
        view: ViewId,
        rect: Rect,
    ) -> AureaResult<Option<Ask>> {
        match &mut self.page {
            Page::Markdown(reader) => markdown::read::show(ui, view, reader, rect),
            Page::Table(table) => table::show(ui, view, table, rect),
        }
    }
}

/// previews by view; one exists while its view previews.
pub type Previews = HashMap<ViewId, Preview>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds_by_language() {
        assert_eq!(kind_for(Some("markdown")), Some(Kind::Markdown));
        assert_eq!(kind_for(Some("csv")), Some(Kind::Table));
        assert_eq!(kind_for(Some("rust")), None);
        assert_eq!(kind_for(None), None);
    }
}
