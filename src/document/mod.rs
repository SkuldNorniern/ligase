//! open documents and the views on them. one document per path, many views
//! per document; views hold scroll and cursor, never their own text copy.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use resolvase::TextBuffer;

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
}

/// one presentation of a document.
#[derive(Clone, Debug)]
pub struct View {
    pub doc: DocumentId,
    /// scroll from the top, in px.
    pub scroll: f32,
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
            },
        );
        (id, true)
    }

    pub fn add_view(&mut self, doc: DocumentId) -> ViewId {
        let id = ViewId(self.take_id());
        self.views.insert(id, View { doc, scroll: 0.0 });
        id
    }

    pub fn doc(&self, id: DocumentId) -> Option<&Document> {
        self.docs.get(&id)
    }

    #[cfg(test)]
    pub fn doc_mut(&mut self, id: DocumentId) -> Option<&mut Document> {
        self.docs.get_mut(&id)
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
        docs.docs
            .get_mut(&a)
            .unwrap()
            .buffer
            .insert(1, "y")
            .unwrap();
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
