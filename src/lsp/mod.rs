//! language servers. one per language, started the first time a file of
//! that language is open, never before. documents sync as whole text for
//! now; every position a server sends is checked and clamped, and read
//! against the text of the version it was made for.

pub mod complete;
pub mod rpc;
pub mod server;
pub mod uri;

use std::collections::{HashMap, VecDeque};
use std::ops::Range;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use resolvase::{Encoding, Point, Revision, TextSnapshot};
use serde_json::{Value, json};

use crate::document::{Documents, Load, ViewId};
use server::Server;

/// files past this are not sent to servers.
const MAX_SYNCED: usize = 8 * 1024 * 1024;
/// old versions kept so late diagnostics land on the right text.
const KEEP_VERSIONS: usize = 8;
/// a completion request not answered by then no longer holds up the next.
const COMPLETE_TIMEOUT: Duration = Duration::from_secs(3);

/// the server for a sylven language: command, args, lsp language id.
pub fn server_for(language: &str) -> Option<(&'static str, &'static [&'static str], &'static str)> {
    Some(match language {
        "rust" => ("rust-analyzer", &[], "rust"),
        "python" => ("pyright-langserver", &["--stdio"], "python"),
        "c" => ("clangd", &[], "c"),
        "toml" => ("taplo", &["lsp", "stdio"], "toml"),
        _ => return None,
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    Error,
    Warning,
    Info,
    Hint,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diag {
    pub range: Range<usize>,
    pub severity: Severity,
    pub message: String,
}

/// what the app should act on.
pub enum Event {
    /// diagnostics for a document, in the text of `revision`.
    Diagnostics(crate::document::DocumentId, Revision, Vec<Diag>),
    /// open this file there.
    Goto(PathBuf, Point, Encoding),
    /// hover text for the caret of a view, if it is still at `at`.
    Hover(ViewId, usize, String),
    /// completions for a view, positions in the text of `revision`.
    Completion(ViewId, Revision, Vec<complete::Item>),
    Status(String),
}

enum Slot {
    Starting,
    Running(Box<Server>, Encoding),
    Failed,
}

struct Synced {
    language: &'static str,
    uri: String,
    version: i64,
    revision: Revision,
    /// recent versions sent, oldest first.
    sent: VecDeque<(i64, TextSnapshot)>,
}

enum Pending {
    Definition,
    Hover(ViewId, usize),
    Complete(ViewId, Revision, crate::document::DocumentId),
}

#[derive(Default)]
pub struct Lsp {
    servers: HashMap<&'static str, Slot>,
    synced: HashMap<crate::document::DocumentId, Synced>,
    pending: HashMap<(&'static str, i64), Pending>,
    /// chars that ask for completion, per language, from the server.
    triggers: HashMap<&'static str, Vec<String>>,
    /// the completion request out now and when it went; one at a time.
    completing: Option<((&'static str, i64), Instant)>,
    /// asked again while one was out: ask once that one is back.
    again: Option<ViewId>,
}

impl Lsp {
    /// languages that want a server and have none yet. marks them starting.
    pub fn wanted(&mut self, docs: &Documents) -> Vec<&'static str> {
        let mut out = Vec::new();
        for doc in docs.all() {
            let Some(language) = doc.language else {
                continue;
            };
            if doc.load != Load::Ready || server_for(language).is_none() {
                continue;
            }
            if !self.servers.contains_key(language) {
                self.servers.insert(language, Slot::Starting);
                out.push(language);
            }
        }
        out
    }

    /// the start job came back.
    pub fn started(
        &mut self,
        language: &'static str,
        result: Result<Server, String>,
    ) -> Option<String> {
        match result {
            Ok(server) => {
                self.servers
                    .insert(language, Slot::Running(Box::new(server), Encoding::Utf16));
                None
            }
            Err(why) => {
                self.servers.insert(language, Slot::Failed);
                Some(why)
            }
        }
    }

    /// sends opens, changes and closes so servers see what the editor has.
    pub fn sync(&mut self, docs: &Documents) {
        // closed documents
        let gone: Vec<_> = self
            .synced
            .keys()
            .filter(|id| docs.doc(**id).is_none())
            .copied()
            .collect();
        for id in gone {
            if let Some(synced) = self.synced.remove(&id)
                && let Some(Slot::Running(server, _)) = self.servers.get_mut(synced.language)
            {
                server.notify(
                    "textDocument/didClose",
                    json!({ "textDocument": { "uri": synced.uri } }),
                );
            }
        }
        for (id, doc) in docs.all_with_ids() {
            let Some(language) = doc.language else {
                continue;
            };
            let Some((_, _, language_id)) = server_for(language) else {
                continue;
            };
            if doc.load != Load::Ready || doc.buffer.len() > MAX_SYNCED {
                continue;
            }
            let Some(Slot::Running(server, _)) = self.servers.get_mut(language) else {
                continue;
            };
            let now = doc.buffer.revision();
            let snapshot = doc.buffer.snapshot();
            match self.synced.get_mut(&id) {
                None => {
                    let uri = uri::from_path(&doc.path);
                    server.notify(
                        "textDocument/didOpen",
                        json!({ "textDocument": {
                            "uri": uri, "languageId": language_id, "version": 1,
                            "text": snapshot.to_string(),
                        }}),
                    );
                    self.synced.insert(
                        id,
                        Synced {
                            language,
                            uri,
                            version: 1,
                            revision: now,
                            sent: VecDeque::from([(1, snapshot)]),
                        },
                    );
                }
                Some(synced) if synced.revision != now => {
                    synced.version += 1;
                    synced.revision = now;
                    server.notify(
                        "textDocument/didChange",
                        json!({
                            "textDocument": { "uri": synced.uri, "version": synced.version },
                            "contentChanges": [{ "text": snapshot.to_string() }],
                        }),
                    );
                    synced.sent.push_back((synced.version, snapshot));
                    if synced.sent.len() > KEEP_VERSIONS {
                        synced.sent.pop_front();
                    }
                }
                Some(_) => {}
            }
        }
    }

    /// asks where the thing at the caret of `view` is defined.
    pub fn definition(&mut self, docs: &Documents, view: ViewId) -> bool {
        self.ask(docs, view, "textDocument/definition", Pending::Definition)
            .is_some()
    }

    /// asks for completions at the caret of `view`.
    pub fn complete(&mut self, docs: &Documents, view: ViewId) -> bool {
        let Some(v) = docs.view(view) else {
            return false;
        };
        let Some(doc) = docs.doc(v.doc) else {
            return false;
        };
        // one out at a time: a fast typist asks once per reply, not per key;
        // a server that never answers is given up on after a while
        if let Some((_, sent)) = self.completing
            && sent.elapsed() < COMPLETE_TIMEOUT
        {
            self.again = Some(view);
            return true;
        }
        if let Some((key, _)) = self.completing.take() {
            self.pending.remove(&key);
        }
        let kind = Pending::Complete(view, doc.buffer.revision(), v.doc);
        let Some(key) = self.ask(docs, view, "textDocument/completion", kind) else {
            return false;
        };
        self.completing = Some((key, Instant::now()));
        true
    }

    /// a view that asked for completion while a request was out, once that
    /// request is back.
    pub fn take_again(&mut self) -> Option<ViewId> {
        if self.completing.is_some() {
            return None;
        }
        self.again.take()
    }

    /// does typing `typed` in a file of `language` ask for completion.
    pub fn triggers(&self, language: Option<&str>, typed: &str) -> bool {
        let Some(language) = language else {
            return false;
        };
        self.triggers
            .iter()
            .find(|(l, _)| **l == language)
            .is_some_and(|(_, t)| t.iter().any(|t| typed.ends_with(t.as_str())))
    }

    pub fn hover(&mut self, docs: &Documents, view: ViewId) -> bool {
        let at = docs.view(view).map_or(0, |v| v.sel.head);
        self.ask(docs, view, "textDocument/hover", Pending::Hover(view, at))
            .is_some()
    }

    /// sends `method` for the caret of `view`; gives the request's key.
    fn ask(
        &mut self,
        docs: &Documents,
        view: ViewId,
        method: &str,
        kind: Pending,
    ) -> Option<(&'static str, i64)> {
        let v = docs.view(view)?;
        let synced = self.synced.get(&v.doc)?;
        let doc = docs.doc(v.doc)?;
        // the server only knows what was synced; ask only when that is now
        if synced.revision != doc.buffer.revision() {
            return None;
        }
        let Some(Slot::Running(server, encoding)) = self.servers.get_mut(synced.language) else {
            return None;
        };
        let at = doc.buffer.text().point(v.sel.head, *encoding).ok()?;
        let id = server.request(
            method,
            json!({
                "textDocument": { "uri": synced.uri },
                "position": { "line": at.line, "character": at.column },
            }),
        );
        self.pending.insert((synced.language, id), kind);
        Some((synced.language, id))
    }

    /// reads what the servers sent.
    pub fn poll(&mut self) -> Vec<Event> {
        let mut events = Vec::new();
        let languages: Vec<&'static str> = self.servers.keys().copied().collect();
        for language in languages {
            let mut messages = Vec::new();
            if let Some(Slot::Running(server, _)) = self.servers.get_mut(language) {
                messages.extend(server.from_server.try_iter());
                if messages.is_empty() && !server.alive() {
                    self.servers.insert(language, Slot::Failed);
                    events.push(Event::Status(format!("{language} language server stopped")));
                    continue;
                }
            }
            for msg in messages {
                self.handle(language, &msg, &mut events);
            }
        }
        events
    }

    fn handle(&mut self, language: &'static str, msg: &Value, events: &mut Vec<Event>) {
        if std::env::var_os("LIGASE_LSP_TRACE").is_some() {
            let text = msg.to_string();
            eprintln!("lsp <- {}", &text[..text.len().min(300)]);
        }
        let Some(Slot::Running(server, encoding)) = self.servers.get_mut(language) else {
            return;
        };
        let method = msg.get("method").and_then(Value::as_str);
        let id = msg.get("id");
        match (method, id) {
            // a request from the server: always answer, or some servers stall
            (Some(method), Some(id)) => {
                let result = match method {
                    "workspace/configuration" => {
                        let n = msg["params"]["items"].as_array().map_or(0, Vec::len);
                        Value::Array(vec![Value::Null; n])
                    }
                    _ => Value::Null,
                };
                server.reply(id, result);
            }
            (Some("textDocument/publishDiagnostics"), None) => {
                if let Some(event) = self.diagnostics(&msg["params"]) {
                    events.push(event);
                }
            }
            (Some("window/showMessage"), None) => {
                if let Some(text) = msg["params"]["message"].as_str() {
                    events.push(Event::Status(text.to_owned()));
                }
            }
            (Some(_), None) => {}
            (None, Some(id)) => {
                let Some(id) = id.as_i64() else { return };
                if id == server::INIT {
                    let caps = &msg["result"]["capabilities"];
                    *encoding = match caps["positionEncoding"].as_str() {
                        Some("utf-8") => Encoding::Utf8,
                        _ => Encoding::Utf16,
                    };
                    let triggers = caps["completionProvider"]["triggerCharacters"]
                        .as_array()
                        .map(|list| {
                            list.iter()
                                .filter_map(Value::as_str)
                                .filter(|t| !t.is_empty() && t.len() <= 4)
                                .take(16)
                                .map(str::to_owned)
                                .collect()
                        })
                        .unwrap_or_default();
                    self.triggers.insert(language, triggers);
                    server.initialized();
                    return;
                }
                let encoding = *encoding;
                match self.pending.remove(&(language, id)) {
                    Some(Pending::Definition) => {
                        if let Some(event) = definition(&msg["result"], encoding) {
                            events.push(event);
                        }
                    }
                    Some(Pending::Hover(view, at)) => {
                        let text = hover_text(&msg["result"]["contents"]);
                        if !text.trim().is_empty() {
                            events.push(Event::Hover(view, at, text));
                        }
                    }
                    Some(Pending::Complete(view, revision, doc)) => {
                        self.completing = None;
                        // positions are in the text the request was made in
                        let text = self
                            .synced
                            .get(&doc)
                            .and_then(|s| s.sent.iter().find(|(_, t)| t.revision() == revision))
                            .map(|(_, t)| t.clone());
                        if let Some(text) = text {
                            let items = complete::items(&msg["result"], &text, encoding);
                            events.push(Event::Completion(view, revision, items));
                        }
                    }
                    None => {}
                }
            }
            (None, None) => {}
        }
    }

    fn diagnostics(&self, params: &Value) -> Option<Event> {
        let uri = params["uri"].as_str()?;
        // servers spell uris their own way (`c:` vs `C:`, `%3A`), so match
        // the paths they name, not the strings
        let path = uri::to_path(uri)?;
        let (&doc, synced) = self
            .synced
            .iter()
            .find(|(_, s)| uri::to_path(&s.uri).as_ref() == Some(&path))?;
        let Some(Slot::Running(_, encoding)) = self.servers.get(synced.language) else {
            return None;
        };
        // the text these were made for: the stated version, else the newest
        let version = params["version"].as_i64();
        let (_, text) = match version {
            Some(v) => synced.sent.iter().find(|(sent, _)| *sent == v)?,
            None => synced.sent.back()?,
        };
        let diags = params["diagnostics"]
            .as_array()?
            .iter()
            .filter_map(|d| {
                let range = range_in(text, &d["range"], *encoding)?;
                let severity = match d["severity"].as_i64() {
                    Some(1) | None => Severity::Error,
                    Some(2) => Severity::Warning,
                    Some(3) => Severity::Info,
                    _ => Severity::Hint,
                };
                let message = d["message"].as_str()?.to_owned();
                Some(Diag {
                    range,
                    severity,
                    message,
                })
            })
            .collect();
        Some(Event::Diagnostics(doc, text.revision(), diags))
    }
}

fn point(value: &Value) -> Option<Point> {
    let line = usize::try_from(value["line"].as_u64()?).ok()?;
    let column = usize::try_from(value["character"].as_u64()?).ok()?;
    Some(Point::new(line, column))
}

fn range_in(text: &TextSnapshot, range: &Value, encoding: Encoding) -> Option<Range<usize>> {
    let start = offset_near(text, point(&range["start"])?, encoding);
    let end = offset_near(text, point(&range["end"])?, encoding);
    Some(start.min(end)..start.max(end))
}

/// byte offset of a server position, pulled back onto the text when it is
/// past a line end, past the last line, or inside a char.
pub fn offset_near(text: &TextSnapshot, at: Point, encoding: Encoding) -> usize {
    let line = at.line.min(text.line_count() - 1);
    let mut column = at.column;
    loop {
        match text.offset(Point::new(line, column), encoding) {
            Ok(offset) => return offset,
            Err(resolvase::Error::ColumnOutOfBounds { .. }) => {
                return crate::editor::cursor::line_end(text, line);
            }
            Err(_) if column > 0 => column -= 1,
            Err(_) => return text.line_start(line).unwrap_or(0),
        }
    }
}

fn definition(result: &Value, encoding: Encoding) -> Option<Event> {
    let first = match result {
        Value::Array(list) => list.first()?,
        Value::Object(_) => result,
        _ => return None,
    };
    // Location or LocationLink
    let uri = first["uri"].as_str().or(first["targetUri"].as_str())?;
    let range = if first.get("targetSelectionRange").is_some() {
        &first["targetSelectionRange"]
    } else {
        &first["range"]
    };
    Some(Event::Goto(
        uri::to_path(uri)?,
        point(&range["start"])?,
        encoding,
    ))
}

/// hover contents as plain text: markup, a marked string, or a list of them.
fn hover_text(contents: &Value) -> String {
    let one = |v: &Value| -> String {
        match v {
            Value::String(s) => s.clone(),
            Value::Object(o) => o
                .get("value")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned(),
            _ => String::new(),
        }
    };
    let text = match contents {
        Value::Array(list) => list.iter().map(one).collect::<Vec<_>>().join("\n\n"),
        other => one(other),
    };
    // code fences only get in the way in a plain text box
    text.lines()
        .filter(|l| !l.trim_start().starts_with("```"))
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use resolvase::TextBuffer;

    #[test]
    fn positions_are_clamped_onto_text() {
        let text = TextBuffer::from_text("a😀b\nxy").snapshot();
        assert_eq!(offset_near(&text, Point::new(0, 1), Encoding::Utf16), 1);
        assert_eq!(
            offset_near(&text, Point::new(0, 2), Encoding::Utf16),
            1,
            "inside the pair"
        );
        assert_eq!(offset_near(&text, Point::new(0, 3), Encoding::Utf16), 5);
        assert_eq!(offset_near(&text, Point::new(0, 99), Encoding::Utf16), 6);
        assert_eq!(offset_near(&text, Point::new(9, 1), Encoding::Utf8), 8);
    }

    #[test]
    fn definition_takes_location_or_link() {
        let loc = json!([{ "uri": "file:///D:/a.rs",
            "range": { "start": { "line": 3, "character": 4 }, "end": { "line": 3, "character": 5 } } }]);
        let Some(Event::Goto(path, at, _)) = definition(&loc, Encoding::Utf16) else {
            panic!("goto");
        };
        assert!(path.ends_with("a.rs"));
        assert_eq!(at, Point::new(3, 4));
        let link = json!([{ "targetUri": "file:///D:/b.rs",
            "targetRange": { "start": { "line": 0, "character": 0 }, "end": { "line": 9, "character": 0 } },
            "targetSelectionRange": { "start": { "line": 2, "character": 7 }, "end": { "line": 2, "character": 9 } } }]);
        let Some(Event::Goto(_, at, _)) = definition(&link, Encoding::Utf16) else {
            panic!("goto");
        };
        assert_eq!(at, Point::new(2, 7));
        assert!(definition(&Value::Null, Encoding::Utf16).is_none());
    }

    #[test]
    fn hover_text_from_any_shape() {
        let markup = json!({ "kind": "markdown", "value": "```rust\nfn x()\n```\ndoes x" });
        assert_eq!(hover_text(&markup), "fn x()\ndoes x");
        let list = json!(["a", { "language": "rust", "value": "b" }]);
        assert_eq!(hover_text(&list), "a\n\nb");
    }

    /// talks to a real rust-analyzer. needs it on PATH and takes a while:
    /// `cargo test -- --ignored real_rust_analyzer`
    #[test]
    #[ignore = "needs rust-analyzer on PATH"]
    fn real_rust_analyzer() {
        use crate::document::Documents;
        use std::time::{Duration, Instant};

        let dir = std::env::temp_dir().join(format!("ligase-ra-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(
            dir.join("Cargo.toml"),
            "[package]\nname = \"t\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
        )
        .unwrap();
        let main = dir.join("src").join("main.rs");
        std::fs::write(&main, "fn main() {\n    let x: u32 = \"no\";\n}\n").unwrap();

        let mut docs = Documents::default();
        let (doc, _) = docs.open(&main);
        let view = docs.add_view(doc);
        docs.loaded(doc, crate::document::read_text(&main));
        let mut lsp = Lsp::default();
        assert_eq!(lsp.wanted(&docs), ["rust"]);
        let server = Server::spawn("rust-analyzer", &[], &dir, || {}).unwrap();
        assert!(lsp.started("rust", Ok(server)).is_none());

        let start = Instant::now();
        let mut got = None;
        while start.elapsed() < Duration::from_secs(120) && got.is_none() {
            lsp.sync(&docs);
            for event in lsp.poll() {
                if let Event::Diagnostics(_, revision, diags) = event
                    && !diags.is_empty()
                {
                    got = Some((revision, diags));
                }
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        let (revision, diags) = got.expect("diagnostics within two minutes");
        docs.set_diags(doc, revision, diags);
        let text = docs.doc(doc).unwrap().buffer.text().to_string();
        let diags = &docs.doc(doc).unwrap().diags;
        assert!(
            diags
                .iter()
                .any(|d| d.severity == Severity::Error && text[d.range.clone()].contains("no")),
            "an error squiggle on the string: {diags:?}"
        );
        assert!(lsp.hover(&docs, view));
        // completion after "x." on a u32 lists its methods
        let end = docs.doc(doc).unwrap().buffer.len() - 2;
        docs.edit(
            view,
            vec![(
                end..end,
                "
    x."
                .to_owned(),
            )],
            false,
        );
        let caret = end
            + "
    x."
            .len();
        docs.view_mut(view).unwrap().sel = crate::editor::cursor::Selection::caret(caret);
        lsp.sync(&docs);
        let start = Instant::now();
        let mut items = Vec::new();
        // early replies hold only postfix snippets, until std is indexed
        let has_pow = |items: &[complete::Item]| items.iter().any(|i| i.label.contains("pow"));
        while start.elapsed() < Duration::from_secs(90) && !has_pow(&items) {
            lsp.complete(&docs, view);
            std::thread::sleep(Duration::from_millis(300));
            for event in lsp.poll() {
                if let Event::Completion(_, _, got) = event {
                    items = got;
                }
            }
        }
        assert!(
            has_pow(&items),
            "u32 methods: {:?}",
            items.iter().map(|i| &i.label).collect::<Vec<_>>()
        );
        drop(lsp);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
