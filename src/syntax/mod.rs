//! syntax colours from sylven. parsing runs on its own thread; when edits
//! come faster than parses, only the newest text of each document is parsed.

use std::collections::HashMap;
use std::ops::Range;
use std::path::Path;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};

use aurea::render::{CanvasId, Color, request_canvas_redraw};
use resolvase::{Revision, TextSnapshot};
use sylven::{HighlightKind, LanguageId, SyntaxEngine};

use crate::document::DocumentId;

/// files past this get no colours, so a huge file stays quick to edit.
pub const MAX_COLOURED: usize = 4 * 1024 * 1024;

/// coloured spans of one document, sorted by start.
/// symbols kept per file.
const MAX_SYMBOLS: usize = 10_000;

pub type Spans = Vec<(Range<usize>, HighlightKind)>;

/// sylven language for a file, by its extension.
pub fn language_for(path: &Path) -> Option<&'static str> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    Some(match ext.as_str() {
        "rs" => "rust",
        "toml" => "toml",
        "json" => "json",
        "md" | "markdown" => "markdown",
        "yaml" | "yml" => "yaml",
        "c" | "h" => "c",
        "py" => "python",
        "v" | "vh" => "verilog",
        "sv" | "svh" => "systemverilog",
        "vhd" | "vhdl" => "vhdl",
        "bsv" => "bsv",
        "bs" => "bh",
        "sdc" | "xdc" => "sdc",
        "oxy" => "oxygen",
        _ => return None,
    })
}

/// colour for a kind, on a dark background.
/// colour for a kind on a light or dark background.
pub fn colour_on(kind: HighlightKind, dark: bool) -> Color {
    if dark {
        return colour(kind);
    }
    match kind {
        HighlightKind::Keyword | HighlightKind::KeywordControl => Color::rgb(0x8a, 0x2b, 0xc8),
        HighlightKind::Type => Color::rgb(0x1a, 0x7f, 0x6e),
        HighlightKind::String => Color::rgb(0x3b, 0x7d, 0x1e),
        HighlightKind::Comment => Color::rgb(0x7a, 0x82, 0x8c),
        HighlightKind::Number => Color::rgb(0xb0, 0x5a, 0x10),
        HighlightKind::Operator | HighlightKind::Punctuation => Color::rgb(0x55, 0x5d, 0x66),
        HighlightKind::Function => Color::rgb(0x1f, 0x5f, 0xc0),
        HighlightKind::Variable => Color::rgb(0x24, 0x29, 0x2f),
        HighlightKind::Attribute | HighlightKind::Macro => Color::rgb(0x95, 0x6a, 0x00),
        HighlightKind::Lifetime => Color::rgb(0xb3, 0x38, 0x38),
        HighlightKind::SectionHeader => Color::rgb(0x1f, 0x5f, 0xc0),
    }
}

pub fn colour(kind: HighlightKind) -> Color {
    match kind {
        HighlightKind::Keyword | HighlightKind::KeywordControl => Color::rgb(0xc6, 0x8a, 0xf0),
        HighlightKind::Type => Color::rgb(0x4e, 0xc9, 0xb0),
        HighlightKind::String => Color::rgb(0xa8, 0xd0, 0x8d),
        HighlightKind::Comment => Color::rgb(0x6f, 0x7a, 0x86),
        HighlightKind::Number => Color::rgb(0xe8, 0xa8, 0x6a),
        HighlightKind::Operator | HighlightKind::Punctuation => Color::rgb(0xa9, 0xb1, 0xbb),
        HighlightKind::Function => Color::rgb(0x6c, 0xb6, 0xff),
        HighlightKind::Variable => Color::rgb(0xd8, 0xdc, 0xe2),
        HighlightKind::Attribute | HighlightKind::Macro => Color::rgb(0xe0, 0xc0, 0x6a),
        HighlightKind::Lifetime => Color::rgb(0xe8, 0x8a, 0x8a),
        HighlightKind::SectionHeader => Color::rgb(0x6c, 0xb6, 0xff),
    }
}

/// spans moved through an edit, so colours stay on their text until the
/// next parse is back. text typed at a span's edge stays outside it.
pub fn follow(spans: &mut Spans, set: &resolvase::ChangeSet) {
    use resolvase::Bias;
    spans.retain_mut(|(range, _)| {
        let start = set.map(range.start, Bias::Right);
        let end = set.map(range.end, Bias::Left);
        *range = start..end.max(start);
        range.start < range.end
    });
}

struct Ask {
    doc: DocumentId,
    language: &'static str,
    text: TextSnapshot,
}

pub struct Parsed {
    pub doc: DocumentId,
    pub revision: Revision,
    pub spans: Spans,
    /// foldable ranges, sorted by start.
    pub folds: Vec<Range<usize>>,
    pub symbols: Vec<Symbol>,
}

/// a named thing in a file, for go to symbol.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Symbol {
    pub name: String,
    /// a short tag like `fn` or `struct`.
    pub kind: &'static str,
    /// where its name starts.
    pub at: usize,
}

pub struct Syntax {
    to_worker: Option<Sender<Ask>>,
    done: Receiver<Parsed>,
    canvas: Arc<Mutex<Option<CanvasId>>>,
}

impl Default for Syntax {
    fn default() -> Self {
        let (to_worker, asks) = channel::<Ask>();
        let (results, done) = channel();
        let canvas: Arc<Mutex<Option<CanvasId>>> = Arc::default();
        let wake = Arc::clone(&canvas);
        let started = std::thread::Builder::new()
            .name("ligase-syntax".into())
            .spawn(move || {
                let mut engine = SyntaxEngine::new();
                sylven_runtime::register_bundled(engine.registry_mut());
                while let Ok(first) = asks.recv() {
                    // keep only the newest ask per document
                    let mut newest: HashMap<DocumentId, Ask> = HashMap::new();
                    for ask in std::iter::once(first).chain(asks.try_iter()) {
                        newest.insert(ask.doc, ask);
                    }
                    for ask in newest.into_values() {
                        let parsed = parse(&engine, ask);
                        if results.send(parsed).is_err() {
                            return;
                        }
                    }
                    if let Some(id) = wake.lock().ok().and_then(|c| *c) {
                        request_canvas_redraw(id);
                    }
                }
            })
            .inspect_err(|e| eprintln!("ligase: can't start the syntax worker: {e}"));
        Self {
            to_worker: started.ok().map(|_| to_worker),
            done,
            canvas,
        }
    }
}

impl Syntax {
    pub fn set_canvas(&mut self, canvas: CanvasId) {
        if let Ok(mut c) = self.canvas.lock() {
            *c = Some(canvas);
        }
    }

    /// asks for colours of `text`. false when there is no worker.
    pub fn ask(&self, doc: DocumentId, language: &'static str, text: TextSnapshot) -> bool {
        self.to_worker.as_ref().is_some_and(|tx| {
            tx.send(Ask {
                doc,
                language,
                text,
            })
            .is_ok()
        })
    }

    pub fn take(&self) -> Option<Parsed> {
        self.done.try_recv().ok()
    }
}

fn parse(engine: &SyntaxEngine, ask: Ask) -> Parsed {
    let revision = ask.text.revision();
    let mut spans = Spans::new();
    let mut folds = Vec::new();
    let mut symbols = Vec::new();
    if ask.text.len() <= MAX_COLOURED {
        let source = sylven::TextSnapshot::new(
            sylven::DocumentId(0),
            sylven::RevisionId(revision.get()),
            ask.text.to_string(),
        );
        // a parser bug on some odd file must not end colouring for every
        // file: that one file just stays plain
        let parsed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            engine.parse(LanguageId(ask.language), &source)
        }));
        if let Ok(Some(result)) = parsed {
            spans = result
                .features
                .highlights
                .iter()
                .map(|h| (h.range.start().to_usize()..h.range.end().to_usize(), h.kind))
                .filter(|(range, _)| !range.is_empty())
                .collect();
            spans.sort_by_key(|(range, _)| range.start);
            folds = result
                .features
                .folds
                .iter()
                .map(|f| f.start().to_usize()..f.end().to_usize())
                .filter(|f| f.start < f.end && f.end <= ask.text.len())
                .collect();
            folds.sort_by_key(|f| (f.start, std::cmp::Reverse(f.end)));
            symbols = result
                .features
                .symbols
                .iter()
                .filter(|s| s.name_range.start().to_usize() <= ask.text.len())
                .take(MAX_SYMBOLS)
                .map(|s| Symbol {
                    name: s.name.chars().take(200).collect(),
                    kind: s.kind.label(),
                    at: s.name_range.start().to_usize(),
                })
                .collect();
        }
    }
    Parsed {
        doc: ask.doc,
        revision,
        spans,
        folds,
        symbols,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use resolvase::TextBuffer;

    #[test]
    fn languages_by_extension() {
        assert_eq!(language_for(Path::new("a/main.RS")), Some("rust"));
        assert_eq!(language_for(Path::new("Cargo.toml")), Some("toml"));
        assert_eq!(language_for(Path::new("notes.txt")), None);
        assert_eq!(language_for(Path::new("Makefile")), None);
    }

    #[test]
    fn rust_gets_colours() {
        let mut engine = SyntaxEngine::new();
        sylven_runtime::register_bundled(engine.registry_mut());
        let text = TextBuffer::from_text("fn main() { let x = 1; } // hi\n").snapshot();
        let parsed = parse(
            &engine,
            Ask {
                doc: crate::document::Documents::default()
                    .open(Path::new("a.rs"))
                    .0,
                language: "rust",
                text,
            },
        );
        assert!(
            parsed
                .spans
                .iter()
                .any(|(r, k)| *k == HighlightKind::Keyword && *r == (0..2))
        );
        assert!(
            parsed
                .spans
                .iter()
                .any(|(_, k)| *k == HighlightKind::Comment)
        );
        assert!(
            parsed
                .spans
                .windows(2)
                .all(|w| w[0].0.start <= w[1].0.start)
        );
    }

    #[test]
    fn spans_follow_edits() {
        let mut buffer = TextBuffer::from_text("let x");
        let mut spans: Spans = vec![
            (0..3, HighlightKind::Keyword),
            (4..5, HighlightKind::Variable),
        ];
        let set = buffer.insert(0, "  ").unwrap();
        follow(&mut spans, &set);
        assert_eq!(spans[0].0, 2..5);
        // typing right after a keyword doesn't stretch its colour
        let set = buffer.insert(5, "z").unwrap();
        follow(&mut spans, &set);
        assert_eq!(spans[0].0, 2..5);
        let set = buffer.delete(7..8).unwrap();
        follow(&mut spans, &set);
        assert_eq!(spans.len(), 1, "deleted span is dropped");
    }
}
