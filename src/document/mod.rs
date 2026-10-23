//! open documents and the views on them. one document per path, many views
//! per document; views hold scroll and cursor, never their own text copy.

use std::collections::{HashMap, VecDeque};
use std::ops::Range;
use std::path::{Path, PathBuf};

use resolvase::{Bias, ChangeSet, Encoding, Point, Revision, SavePoint, TextBuffer, TextSnapshot};

use crate::editor::cursor::Selection;
use crate::lsp::Diag;
use crate::syntax::{self, Parsed, Spans};

/// files bigger than this are refused for now.
pub const MAX_FILE: u64 = 64 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct DocumentId(u32);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ViewId(u32);

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Load {
    Loading,
    Ready,
    Failed(String),
}

pub struct Document {
    pub path: PathBuf,
    pub buffer: TextBuffer,
    pub load: Load,
    /// the file used `\r\n`; new lines follow it.
    pub crlf: bool,
    /// sylven language, from the file extension.
    pub language: Option<&'static str>,
    /// syntax colours, moved along with edits between parses.
    pub spans: Spans,
    /// newest revision sent for colouring.
    asked: Option<Revision>,
    /// language server diagnostics, moved along with edits.
    pub diags: Vec<Diag>,
    /// the last few edits, to move late results onto the text now.
    recent: VecDeque<ChangeSet>,
}

impl Document {
    pub fn name(&self) -> String {
        self.path.file_name().map_or_else(
            || self.path.display().to_string(),
            |n| n.to_string_lossy().into_owned(),
        )
    }

    pub fn is_dirty(&self) -> bool {
        !self.buffer.is_saved()
    }

    pub fn line_break(&self) -> &'static str {
        if self.crlf { "\r\n" } else { "\n" }
    }
}

/// one presentation of a document.
#[derive(Clone, Debug)]
pub struct View {
    pub doc: DocumentId,
    /// scroll from the top, in px.
    pub scroll: f32,
    pub sel: Selection,
    /// column up and down aim for.
    pub goal: Option<usize>,
    /// ime text not committed yet, and the caret inside it.
    pub preedit: Option<(String, Option<usize>)>,
    /// scroll the caret into sight next frame.
    pub reveal: bool,
    /// lines that fit, from the last frame; for page up and down.
    pub page: usize,
    /// where to put the caret once the text is loaded.
    pub goto: Option<(Point, Encoding)>,
}

#[derive(Default)]
pub struct Documents {
    docs: HashMap<DocumentId, Document>,
    views: HashMap<ViewId, View>,
    next: u32,
}

impl Documents {
    fn take_id(&mut self) -> u32 {
        self.next += 1;
        self.next
    }

    /// the document for `path`, made in `Loading` state if new. true when new,
    /// so the caller knows to start a load.
    pub fn open(&mut self, path: &Path) -> (DocumentId, bool) {
        if let Some((&id, _)) = self.docs.iter().find(|(_, d)| d.path == path) {
            return (id, false);
        }
        let id = DocumentId(self.take_id());
        self.docs.insert(
            id,
            Document {
                path: path.to_path_buf(),
                buffer: TextBuffer::new(),
                load: Load::Loading,
                crlf: false,
                language: syntax::language_for(path),
                spans: Spans::new(),
                asked: None,
                diags: Vec::new(),
                recent: VecDeque::new(),
            },
        );
        (id, true)
    }

    pub fn add_view(&mut self, doc: DocumentId) -> ViewId {
        let id = ViewId(self.take_id());
        self.views.insert(
            id,
            View {
                doc,
                scroll: 0.0,
                sel: Selection::default(),
                goal: None,
                preedit: None,
                reveal: false,
                page: 20,
                goto: None,
            },
        );
        id
    }

    pub fn doc(&self, id: DocumentId) -> Option<&Document> {
        self.docs.get(&id)
    }

    /// a loaded document and one of its views, for edits. none while the
    /// file is loading or failed, so nothing can save over it.
    pub fn editable(&mut self, view: ViewId) -> Option<(&mut Document, &mut View)> {
        let v = self.views.get_mut(&view)?;
        let doc = self.docs.get_mut(&v.doc)?;
        (doc.load == Load::Ready).then_some((doc, v))
    }

    /// applies edits as one undo step and moves every view's selection
    /// along. the acting view's caret lands after the new text.
    pub fn edit(&mut self, view: ViewId, edits: Vec<(Range<usize>, String)>, join: bool) -> bool {
        let Some((doc, _)) = self.editable(view) else {
            return false;
        };
        let mut tx = doc.buffer.transaction().join(join);
        for (range, text) in edits {
            tx = tx.replace(range, text);
        }
        match tx.commit() {
            Ok(set) => {
                self.follow(view, &set);
                true
            }
            Err(_) => false,
        }
    }

    pub fn undo(&mut self, view: ViewId) -> bool {
        let Some((doc, _)) = self.editable(view) else {
            return false;
        };
        let Some(set) = doc.buffer.undo() else {
            return false;
        };
        self.follow(view, &set);
        true
    }

    pub fn redo(&mut self, view: ViewId) -> bool {
        let Some((doc, _)) = self.editable(view) else {
            return false;
        };
        let Some(set) = doc.buffer.redo() else {
            return false;
        };
        self.follow(view, &set);
        true
    }

    fn follow(&mut self, actor: ViewId, set: &ChangeSet) {
        let Some(doc) = self.views.get(&actor).map(|v| v.doc) else {
            return;
        };
        if let Some(d) = self.docs.get_mut(&doc) {
            syntax::follow(&mut d.spans, set);
            for diag in &mut d.diags {
                diag.range = map_range(set, diag.range.clone());
            }
            d.recent.push_back(set.clone());
            if d.recent.len() > RECENT {
                d.recent.pop_front();
            }
        }
        for (&id, v) in self.views.iter_mut().filter(|(_, v)| v.doc == doc) {
            // others stay put when text lands right at their caret
            let bias = if id == actor { Bias::Right } else { Bias::Left };
            v.sel.anchor = set.map(v.sel.anchor, bias);
            v.sel.head = set.map(v.sel.head, bias);
            if id == actor {
                v.sel.anchor = v.sel.head;
                v.goal = None;
                v.reveal = true;
            }
        }
    }

    /// what a save of `view`'s document writes: path, text, and the state to
    /// mark saved once the write is done. none unless loaded.
    pub fn save_job(
        &mut self,
        view: ViewId,
    ) -> Option<(DocumentId, PathBuf, TextSnapshot, SavePoint)> {
        let id = self.views.get(&view)?.doc;
        let (doc, _) = self.editable(view)?;
        let point = doc.buffer.save_point();
        Some((id, doc.path.clone(), doc.buffer.snapshot(), point))
    }

    /// loaded documents whose current text has not been sent for colours.
    pub fn wanting_colours(&mut self) -> Vec<(DocumentId, &'static str, TextSnapshot)> {
        let mut out = Vec::new();
        for (&id, doc) in &mut self.docs {
            let Some(language) = doc.language else {
                continue;
            };
            let now = doc.buffer.revision();
            if doc.load == Load::Ready && doc.asked != Some(now) {
                doc.asked = Some(now);
                out.push((id, language, doc.buffer.snapshot()));
            }
        }
        out
    }

    /// colours are back. kept only when they match the text now; older
    /// ones would sit on the wrong text, the moved spans are better.
    pub fn coloured(&mut self, parsed: Parsed) {
        if let Some(doc) = self.docs.get_mut(&parsed.doc)
            && doc.buffer.revision() == parsed.revision
        {
            doc.spans = parsed.spans;
        }
    }

    /// every open document.
    pub fn all(&self) -> impl Iterator<Item = &Document> {
        self.docs.values()
    }

    pub fn all_with_ids(&self) -> impl Iterator<Item = (DocumentId, &Document)> {
        self.docs.iter().map(|(id, d)| (*id, d))
    }

    /// diagnostics made for the text at `revision`, moved onto the text
    /// now through the recent edits. too old to move: dropped, the server
    /// sends fresh ones anyway.
    pub fn set_diags(&mut self, id: DocumentId, revision: Revision, mut diags: Vec<Diag>) {
        let Some(doc) = self.docs.get_mut(&id) else {
            return;
        };
        let now = doc.buffer.revision();
        if revision != now {
            let Some(from) = doc.recent.iter().position(|set| set.before == revision) else {
                return;
            };
            for set in doc.recent.iter().skip(from) {
                for diag in &mut diags {
                    diag.range = map_range(set, diag.range.clone());
                }
            }
        }
        diags.sort_by_key(|d| (d.range.start, d.severity));
        doc.diags = diags;
    }

    /// a save is done. ignored when the document was closed meanwhile.
    pub fn saved(&mut self, id: DocumentId, point: SavePoint) {
        if let Some(doc) = self.docs.get_mut(&id) {
            doc.buffer.mark_saved_at(point);
        }
    }

    pub fn view(&self, id: ViewId) -> Option<&View> {
        self.views.get(&id)
    }

    pub fn view_mut(&mut self, id: ViewId) -> Option<&mut View> {
        self.views.get_mut(&id)
    }

    pub fn doc_of(&self, view: ViewId) -> Option<&Document> {
        self.view(view).and_then(|v| self.doc(v.doc))
    }

    pub fn any_dirty(&self) -> bool {
        self.docs.values().any(Document::is_dirty)
    }

    /// true when this is the last view of a document with unsaved changes;
    /// closing it would drop work.
    pub fn close_loses_work(&self, view: ViewId) -> bool {
        let Some(v) = self.view(view) else {
            return false;
        };
        let others = self
            .views
            .iter()
            .any(|(&id, o)| id != view && o.doc == v.doc);
        !others && self.doc(v.doc).is_some_and(Document::is_dirty)
    }

    /// drops a view, and its document when no view is left.
    pub fn close_view(&mut self, view: ViewId) {
        let Some(v) = self.views.remove(&view) else {
            return;
        };
        if !self.views.values().any(|o| o.doc == v.doc) {
            self.docs.remove(&v.doc);
        }
    }

    /// result of a load job. ignored when the document was closed meanwhile.
    pub fn loaded(&mut self, id: DocumentId, result: Result<String, String>) {
        let Some(doc) = self.docs.get_mut(&id) else {
            return;
        };
        match result {
            Ok(text) => {
                doc.crlf = text.contains("\r\n");
                doc.buffer = TextBuffer::from_text(&text);
                doc.load = Load::Ready;
            }
            Err(why) => doc.load = Load::Failed(why),
        }
    }
}

/// reads a text file. runs on the worker. refuses big and non UTF-8 files
/// instead of guessing, so nothing gets mangled.
pub fn read_text(path: &Path) -> Result<String, String> {
    use std::io::Read;

    let file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    if !file.metadata().map_err(|e| e.to_string())?.is_file() {
        return Err("not a regular file".to_owned());
    }
    // read through a cap: the size can change after the check, and some
    // special files report none
    let mut bytes = Vec::new();
    file.take(MAX_FILE + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > MAX_FILE {
        return Err(format!("file is over {} MiB", MAX_FILE / 1024 / 1024));
    }
    String::from_utf8(bytes).map_err(|_| "not UTF-8 text".to_owned())
}

/// edits kept per document for moving late results.
const RECENT: usize = 64;

/// a range through an edit; text typed at its edges stays outside.
fn map_range(set: &ChangeSet, range: Range<usize>) -> Range<usize> {
    let start = set.map(range.start, Bias::Right);
    let end = set.map(range.end, Bias::Left).max(start);
    start..end
}

/// writes `text` over `path` without ever leaving half a file: a temp file
/// next to it, flushed to disk, then renamed over. the old file keeps its
/// place until the new one is whole. runs on the worker.
pub fn write_atomic(path: &Path, text: &TextSnapshot) -> Result<(), String> {
    use std::io::Write;

    // a link is saved through: write its target and keep the link
    let target;
    let path = match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => {
            target = std::fs::canonicalize(path).map_err(|e| e.to_string())?;
            target.as_path()
        }
        _ => path,
    };
    let name = path
        .file_name()
        .ok_or_else(|| "no file name".to_owned())?
        .to_string_lossy();
    let temp = path.with_file_name(format!(".{name}.ligase-save"));
    let result = (|| {
        let file = std::fs::File::create(&temp)?;
        let mut out = std::io::BufWriter::new(file);
        for chunk in text.chunks(0..text.len()).into_iter().flatten() {
            out.write_all(chunk.as_bytes())?;
        }
        let file = out
            .into_inner()
            .map_err(std::io::IntoInnerError::into_error)?;
        // keep the old file's permissions, like read-only bits
        if let Ok(meta) = std::fs::metadata(path) {
            file.set_permissions(meta.permissions())?;
        }
        file.sync_all()?;
        drop(file);
        std::fs::rename(&temp, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    result.map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_document_per_path_many_views() {
        let mut docs = Documents::default();
        let (a, new) = docs.open(Path::new("a.rs"));
        assert!(new);
        let (again, new) = docs.open(Path::new("a.rs"));
        assert_eq!(a, again);
        assert!(!new);
        let v1 = docs.add_view(a);
        let v2 = docs.add_view(a);
        docs.loaded(a, Ok("fn main() {}\n".into()));
        assert_eq!(docs.doc_of(v2).unwrap().load, Load::Ready);
        docs.close_view(v1);
        assert!(docs.doc(a).is_some());
        docs.close_view(v2);
        assert!(docs.doc(a).is_none());
    }

    #[test]
    fn last_view_of_dirty_doc_guards_close() {
        let mut docs = Documents::default();
        let (a, _) = docs.open(Path::new("a.rs"));
        let v1 = docs.add_view(a);
        let v2 = docs.add_view(a);
        docs.loaded(a, Ok("x".into()));
        assert!(docs.edit(v1, vec![(1..1, "y".into())], false));
        assert!(!docs.close_loses_work(v1));
        docs.close_view(v1);
        assert!(docs.close_loses_work(v2));
    }

    #[test]
    fn late_load_for_closed_doc_is_dropped() {
        let mut docs = Documents::default();
        let (a, _) = docs.open(Path::new("a.rs"));
        let v = docs.add_view(a);
        docs.close_view(v);
        docs.loaded(a, Ok("x".into()));
        assert!(docs.doc(a).is_none());
    }

    #[test]
    fn edits_move_every_view() {
        let mut docs = Documents::default();
        let (a, _) = docs.open(Path::new("a.rs"));
        let v1 = docs.add_view(a);
        let v2 = docs.add_view(a);
        assert!(
            !docs.edit(v1, vec![(0..0, "x".into())], false),
            "not loaded yet"
        );
        docs.loaded(a, Ok("hello".into()));
        docs.view_mut(v2).unwrap().sel = Selection::caret(5);
        docs.view_mut(v1).unwrap().sel = Selection::caret(5);
        assert!(docs.edit(v1, vec![(5..5, "!".into())], true));
        assert_eq!(docs.view(v1).unwrap().sel, Selection::caret(6));
        assert_eq!(
            docs.view(v2).unwrap().sel,
            Selection::caret(5),
            "other view stays"
        );
        assert!(docs.edit(v2, vec![(0..0, ">".into())], false));
        assert_eq!(docs.view(v1).unwrap().sel, Selection::caret(7));
        assert!(docs.undo(v1));
        assert_eq!(docs.doc(a).unwrap().buffer.text().to_string(), "hello!");
    }

    #[test]
    fn save_writes_whole_file_and_marks_saved() {
        let dir = std::env::temp_dir().join(format!("ligase-save-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("a.txt");
        std::fs::write(&path, "old\r\n").unwrap();
        let mut docs = Documents::default();
        let (a, _) = docs.open(&path);
        let v = docs.add_view(a);
        docs.loaded(a, read_text(&path));
        assert!(docs.doc(a).unwrap().crlf);
        docs.edit(v, vec![(0..3, "new".into())], false);
        let (id, at, text, point) = docs.save_job(v).unwrap();
        docs.edit(v, vec![(0..0, "more ".into())], false);
        write_atomic(&at, &text).unwrap();
        docs.saved(id, point);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "new\r\n");
        assert!(docs.doc(a).unwrap().is_dirty(), "edit after the snapshot");
        docs.undo(v);
        assert!(!docs.doc(a).unwrap().is_dirty());
        assert!(!dir.join(".a.txt.ligase-save").exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn colours_asked_once_per_revision_and_kept_only_when_current() {
        let mut docs = Documents::default();
        let (a, _) = docs.open(Path::new("a.rs"));
        let (_txt, _) = docs.open(Path::new("notes.txt"));
        let v = docs.add_view(a);
        assert!(docs.wanting_colours().is_empty(), "still loading");
        docs.loaded(a, Ok("fn x() {}".into()));
        let asked = docs.wanting_colours();
        assert_eq!(asked.len(), 1, "only the file with a language");
        assert!(docs.wanting_colours().is_empty());
        let old = asked[0].2.revision();
        docs.edit(v, vec![(0..0, " ".into())], false);
        docs.coloured(Parsed {
            doc: a,
            revision: old,
            spans: vec![(0..2, sylven::HighlightKind::Keyword)],
        });
        assert!(docs.doc(a).unwrap().spans.is_empty(), "stale parse dropped");
        let now = docs.wanting_colours()[0].2.revision();
        docs.coloured(Parsed {
            doc: a,
            revision: now,
            spans: vec![(1..3, sylven::HighlightKind::Keyword)],
        });
        assert_eq!(docs.doc(a).unwrap().spans.len(), 1);
    }

    #[test]
    fn late_diagnostics_move_onto_text_now() {
        use crate::lsp::Severity;
        let mut docs = Documents::default();
        let (a, _) = docs.open(Path::new("a.rs"));
        let v = docs.add_view(a);
        docs.loaded(a, Ok("let x = y;".into()));
        let old = docs.doc(a).unwrap().buffer.revision();
        docs.edit(v, vec![(0..0, "// hi\n".into())], false);
        let diag = Diag {
            range: 8..9,
            severity: Severity::Error,
            message: "no y".into(),
        };
        docs.set_diags(a, old, vec![diag.clone()]);
        assert_eq!(docs.doc(a).unwrap().diags[0].range, 14..15);
        // typing in front moves it again
        docs.edit(v, vec![(0..0, "x".into())], false);
        assert_eq!(docs.doc(a).unwrap().diags[0].range, 15..16);
        // a revision never seen is dropped
        let mut stranger = resolvase::TextBuffer::from_text("");
        stranger.insert(0, "zzzzzzz").unwrap();
        for _ in 0..80 {
            docs.edit(v, vec![(0..0, "q".into())], false);
        }
        docs.set_diags(a, old, vec![diag]);
        assert_eq!(
            docs.doc(a).unwrap().diags[0].range.start,
            15 + 80,
            "kept the moved ones"
        );
    }

    #[test]
    fn failed_load_is_never_saved() {
        let mut docs = Documents::default();
        let (a, _) = docs.open(Path::new("a.rs"));
        let v = docs.add_view(a);
        docs.loaded(a, Err("nope".into()));
        assert!(docs.save_job(v).is_none());
    }

    #[test]
    fn non_utf8_is_refused() {
        let dir = std::env::temp_dir().join(format!("ligase-doc-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("bad.txt");
        std::fs::write(&file, [0xff, 0xfe, 0x00]).unwrap();
        assert_eq!(read_text(&file), Err("not UTF-8 text".to_owned()));
        std::fs::write(&file, "ok").unwrap();
        assert_eq!(read_text(&file).as_deref(), Ok("ok"));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
