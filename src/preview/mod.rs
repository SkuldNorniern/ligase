//! a file drawn as what it means instead of as text. each kind is a row in
//! `KINDS` and an arm in the matches below; a new kind adds both, and its
//! parse runs on the worker like the others.

use std::collections::HashMap;
use std::ops::Range;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use aurea::AureaResult;
use aurea::render::Rect;
use aurea::ui::Ui;

use crate::app::Id;
use crate::document::ViewId;
use crate::lang::Texts;
use crate::markdown;

pub mod rows;
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
    /// a big table's index of the file, made on the worker.
    Indexed(Arc<rows::Index>),
    /// a big table that can't be indexed, and why.
    Failed(String),
}

/// parses `text` for `kind`. runs on the worker.
pub fn parse(kind: Kind, text: &str) -> Parsed {
    match kind {
        Kind::Markdown => Parsed::Markdown(markdown::parse::parse(text)),
        Kind::Table => Parsed::Table(table::parse(text)),
    }
}

/// a big table reads the file itself when its text failed to load, for any
/// reason, or when the text is over the table's cap. unsaved edits are not
/// shown in a big table; it shows what is on disk.
pub fn big(kind: Kind, failed: bool, len: usize) -> bool {
    kind == Kind::Table && (failed || len > table::MAX_BYTES)
}

/// indexes the file at `path`. runs on the worker. `progress` takes the bytes
/// read so far, and `wake` is called with each report.
pub fn index(path: &Path, progress: &AtomicU64, stop: &AtomicBool, wake: impl Fn()) -> Parsed {
    let report = |n: u64| {
        progress.store(n, Ordering::Relaxed);
        wake();
    };
    match rows::Index::build(path, report, stop) {
        Ok(index) => Parsed::Indexed(Arc::new(index)),
        Err(why) => Parsed::Failed(why),
    }
}

/// a big table's reader, shared with the jobs that read rows. the first read
/// opens it on the worker, so the ui thread never touches the file.
pub type ReaderSlot = Arc<Mutex<Option<rows::Reader>>>;

/// the rows in `range` of an indexed file, through `slot`. runs on the worker.
///
/// # Errors
/// the file can't be opened or read, or changed since it was indexed.
pub fn read_rows(
    index: &Arc<rows::Index>,
    slot: &ReaderSlot,
    range: Range<usize>,
) -> Result<Vec<Vec<String>>, String> {
    let mut reader = slot
        .lock()
        .map_err(|_| "the row reader is poisoned".to_owned())?;
    if reader.is_none() {
        *reader = Some(rows::Reader::open(Arc::clone(index))?);
    }
    match reader.as_mut() {
        Some(reader) => reader.rows(range),
        None => Err("no row reader".to_owned()),
    }
}

enum Page {
    Markdown(markdown::read::Reader),
    Table(table::TableState),
    Big(table::BigState),
}

/// one view's preview.
pub struct Preview {
    pub kind: Kind,
    /// the text revision the page was made from.
    pub made_from: Option<resolvase::Revision>,
    /// a parse is out for this revision.
    pub asked: Option<resolvase::Revision>,
    page: Page,
    /// set when the preview goes away, so a running index stops.
    stop: Arc<AtomicBool>,
}

impl Drop for Preview {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

impl Preview {
    /// a preview of `kind` that opens at source line `line`.
    pub fn new(kind: Kind, line: Option<usize>, texts: &Texts) -> Self {
        let page = match kind {
            Kind::Markdown => {
                let mut reader = markdown::read::Reader::default();
                reader.reveal_line = line;
                Page::Markdown(reader)
            }
            Kind::Table => {
                let mut table = table::TableState::default();
                table.reveal_line = line;
                table.labels = table::Labels::new(texts);
                Page::Table(table)
            }
        };
        Self {
            kind,
            made_from: None,
            asked: None,
            page,
            stop: Arc::new(AtomicBool::new(false)),
        }
    }

    /// the page reads the file through an index from now on. gives the
    /// progress counter and the stop flag for the index job.
    pub fn start_big(&mut self) -> (Arc<AtomicU64>, Arc<AtomicBool>) {
        let progress = Arc::new(AtomicU64::new(0));
        let labels = match &self.page {
            Page::Table(table) => table.labels.clone(),
            _ => table::Labels::default(),
        };
        self.page = Page::Big(table::BigState::new(labels, Arc::clone(&progress)));
        (progress, Arc::clone(&self.stop))
    }

    /// the page reads the file through an index.
    pub fn is_big(&self) -> bool {
        matches!(self.page, Page::Big(_))
    }

    /// a parse for `revision` came back.
    pub fn set(&mut self, revision: resolvase::Revision, parsed: Parsed) {
        self.made_from = Some(revision);
        match (&mut self.page, parsed) {
            (Page::Markdown(reader), Parsed::Markdown(blocks)) => reader.set(blocks),
            (Page::Table(table), Parsed::Table(parsed)) => table.set(parsed),
            (Page::Big(big), Parsed::Indexed(index)) => big.set_index(index),
            (Page::Big(big), Parsed::Failed(why)) => big.fail(why),
            // a parse is always of the kind the page was made for
            _ => {}
        }
    }

    /// the index rows of a big table to read next, when the view needs some.
    /// one read is out at a time: none is asked while it is.
    pub fn wants_rows(&mut self) -> Option<Range<usize>> {
        match &mut self.page {
            Page::Big(big) => big.want(),
            _ => None,
        }
    }

    /// the index and reader slot a read of a big table goes through.
    pub fn rows_source(&self) -> Option<(Arc<rows::Index>, ReaderSlot)> {
        match &self.page {
            Page::Big(big) => big.source(),
            _ => None,
        }
    }

    /// the rows of a read came back.
    pub fn got_rows(&mut self, range: Range<usize>, rows: Vec<Vec<String>>) {
        if let Page::Big(big) = &mut self.page {
            big.got(range, rows);
        }
    }

    /// a read failed; the table shows why.
    pub fn rows_failed(&mut self, why: String) {
        if let Page::Big(big) = &mut self.page {
            big.fail(why);
        }
    }

    /// the source line at the top of the preview, to go back to.
    pub fn top_line(&self) -> Option<usize> {
        match &self.page {
            Page::Markdown(reader) => reader.top_line(),
            Page::Table(table) => table.top_line(),
            // a big table has no lines to go back to
            Page::Big(_) => None,
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
            Page::Big(big) => table::show_big(ui, view, big, rect),
        }
    }
}

/// previews by view; one exists while its view previews.
pub type Previews = HashMap<ViewId, Preview>;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lang::Lang;
    use resolvase::Revision;
    use std::path::PathBuf;

    /// a temp file, removed on drop.
    struct Tmp(PathBuf);

    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    fn tmp_csv(name: &str, text: &str) -> Tmp {
        let path =
            std::env::temp_dir().join(format!("ligase_preview_{}_{name}.csv", std::process::id()));
        std::fs::write(&path, text).unwrap();
        Tmp(path)
    }

    fn index_of(t: &Tmp) -> Arc<rows::Index> {
        let index = rows::Index::build(&t.0, |_| {}, &AtomicBool::new(false)).unwrap();
        Arc::new(index)
    }

    #[test]
    fn kinds_by_language() {
        assert_eq!(kind_for(Some("markdown")), Some(Kind::Markdown));
        assert_eq!(kind_for(Some("csv")), Some(Kind::Table));
        assert_eq!(kind_for(Some("rust")), None);
        assert_eq!(kind_for(None), None);
    }

    #[test]
    fn big_when_the_load_failed_or_the_text_is_long() {
        assert!(big(Kind::Table, true, 0));
        assert!(big(Kind::Table, false, table::MAX_BYTES + 1));
        assert!(!big(Kind::Table, false, table::MAX_BYTES));
        assert!(!big(Kind::Table, false, 10));
        assert!(!big(Kind::Markdown, true, 0));
    }

    #[test]
    fn indexed_parse_switches_the_page_to_big() {
        let file = tmp_csv("switch", "a,b\n1,2\n3,4\n");
        let texts = Texts::new(Lang::En);
        let mut preview = Preview::new(Kind::Table, None, &texts);
        assert!(!preview.is_big());
        let (_progress, _stop) = preview.start_big();
        assert!(preview.is_big());
        assert_eq!(preview.wants_rows(), None, "no index yet");
        assert!(preview.rows_source().is_none());

        preview.set(Revision::default(), Parsed::Indexed(index_of(&file)));
        assert_eq!(preview.wants_rows(), Some(0..1), "the header first");
        let (index, slot) = preview.rows_source().unwrap();
        let got = read_rows(&index, &slot, 0..3).unwrap();
        assert_eq!(got[0], ["a", "b"]);
        assert_eq!(got[2], ["3", "4"]);
        preview.got_rows(0..3, got);
        assert_eq!(preview.top_line(), None);
    }

    #[test]
    fn failed_index_shows_no_rows() {
        let texts = Texts::new(Lang::En);
        let mut preview = Preview::new(Kind::Table, None, &texts);
        preview.start_big();
        preview.set(
            Revision::default(),
            Parsed::Failed("cannot open file".into()),
        );
        assert!(preview.is_big());
        assert_eq!(preview.wants_rows(), None);
        assert!(preview.rows_source().is_none());
    }

    #[test]
    fn a_parse_for_the_wrong_kind_is_dropped() {
        let texts = Texts::new(Lang::En);
        let mut preview = Preview::new(Kind::Table, None, &texts);
        preview.set(
            Revision::default(),
            Parsed::Indexed(index_of(&tmp_csv("wrong", "a\n"))),
        );
        assert!(!preview.is_big(), "an index needs a big page");
    }

    #[test]
    fn dropping_the_preview_stops_its_index() {
        let texts = Texts::new(Lang::En);
        let mut preview = Preview::new(Kind::Table, None, &texts);
        let (_progress, stop) = preview.start_big();
        assert!(!stop.load(Ordering::Relaxed));
        drop(preview);
        assert!(stop.load(Ordering::Relaxed));
    }
}
