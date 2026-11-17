//! language servers. one per language, started the first time a file of
//! that language is open, never before. documents sync as whole text for
//! now; every position a server sends is checked and clamped, and read
//! against the text of the version it was made for.

pub mod action;
pub mod complete;
pub mod edit;
pub mod rpc;
pub mod server;
pub mod uri;

use std::collections::{HashMap, VecDeque};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use resolvase::{Encoding, Point, Revision, TextSnapshot};
use serde_json::{Value, json};

use crate::document::{Documents, Load, ViewId};
use server::Server;

/// files past this are not sent to servers.
const MAX_SYNCED: usize = 8 * 1024 * 1024;
/// old versions kept so late diagnostics land on the right text.
const KEEP_VERSIONS: usize = 8;
/// most still-checking ranges kept from one progress note.
const MAX_BUSY: usize = 1000;
/// a completion request not answered by then no longer holds up the next.
const COMPLETE_TIMEOUT: Duration = Duration::from_secs(3);

/// how to start the server for a language's files.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServerSpec {
    /// the sylven language whose files it serves.
    pub language: String,
    pub command: String,
    pub args: Vec<String>,
    /// the language id sent with each file.
    pub language_id: String,
    /// the package that declared it; none for the ones ligase knows.
    pub package: Option<String>,
    /// file names that mark the folder to start in; see [`root_for`].
    pub roots: Vec<String>,
}

/// most folders walked up looking for a root mark.
const MAX_UP: usize = 64;

/// the folder a server for `file` starts in: the nearest one above it that
/// holds one of `marks`, else `workspace`. reads the disk: worker only.
pub fn root_for(file: &Path, marks: &[String], workspace: &Path) -> PathBuf {
    if !marks.is_empty() {
        for dir in file.ancestors().skip(1).take(MAX_UP) {
            if marks.iter().any(|m| dir.join(m).is_file()) {
                return dir.to_path_buf();
            }
        }
    }
    workspace.to_path_buf()
}

/// servers ligase knows: sylven language, program, args, lsp language id.
const BUILT_IN: &[(&str, &str, &[&str], &str)] = &[
    ("rust", "rust-analyzer", &[], "rust"),
    ("python", "pyright-langserver", &["--stdio"], "python"),
    ("c", "clangd", &[], "c"),
    ("toml", "taplo", &["lsp", "stdio"], "toml"),
];

fn built_in(language: &str) -> Option<ServerSpec> {
    BUILT_IN
        .iter()
        .find(|(l, ..)| *l == language)
        .map(|(l, command, args, id)| ServerSpec {
            language: (*l).to_owned(),
            command: (*command).to_owned(),
            args: args.iter().map(|a| (*a).to_owned()).collect(),
            language_id: (*id).to_owned(),
            package: None,
            roots: Vec::new(),
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
    /// the server's own fields (`code`, `data` ...) as json, sent back
    /// when asking for code actions.
    pub extra: Option<String>,
}

/// what the app should act on.
pub enum Event {
    /// diagnostics for a document, in the text of `revision`.
    Diagnostics(crate::document::DocumentId, Revision, Vec<Diag>),
    /// code actions to pick from, for a document as of a revision.
    Actions(
        crate::document::DocumentId,
        Revision,
        Seen,
        &'static str,
        Vec<action::Action>,
        Encoding,
    ),
    /// text the server is still checking, and whether each stopped on a
    /// fatal error (lean's `$/lean/fileProgress`).
    Busy(
        crate::document::DocumentId,
        Revision,
        Vec<(Range<usize>, bool)>,
    ),
    /// open this file there.
    Goto(PathBuf, Point, Encoding),
    /// hover text for the caret of a view, if it is still at `at`.
    Hover(ViewId, usize, String),
    /// changes to make in files, positions in `Encoding`: a rename or a
    /// format. `revision` is the asking document's, to drop a late one.
    Edits(
        crate::document::DocumentId,
        Revision,
        Seen,
        Vec<(PathBuf, Vec<edit::Edit>)>,
        Encoding,
    ),
    /// places a name is used, for find references.
    Locations(Vec<(PathBuf, Point)>, Encoding),
    /// completions for a view, positions in the text of `revision`.
    Completion(ViewId, Revision, Vec<complete::Item>),
    /// what a view's feed asked for at a caret: the feed, the editor view,
    /// the caret, and the server's result as it came.
    Feed(u32, ViewId, usize, Value),
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
    /// a rename or format for a document as of a revision, and every open
    /// document's revision then.
    Edits(crate::document::DocumentId, Revision, Seen),
    References,
    Complete(ViewId, Revision, crate::document::DocumentId),
    /// code actions for a document as of a revision.
    Actions(crate::document::DocumentId, Revision, Seen),
    /// an action's edit, filled in by the server.
    Resolved(crate::document::DocumentId, Revision, Seen),
    Feed(u32, ViewId, usize),
}

/// how a language's server is doing, for the status bar.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServerState {
    Starting,
    Running,
    Missing,
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
    /// servers packages declared and the user allowed.
    extra: Vec<ServerSpec>,
}

impl Lsp {
    /// the server for files in `language`: a built-in one first, then one a
    /// package brings.
    pub fn server_for(&self, language: &str) -> Option<ServerSpec> {
        built_in(language).or_else(|| self.extra.iter().find(|s| s.language == language).cloned())
    }

    /// lets `language`'s server go so the next frame starts it again. the
    /// files it had open are sent afresh then. gives the old server, to be
    /// dropped off the ui thread: dropping waits on the process.
    pub fn restart(&mut self, language: &str) -> Option<Box<Server>> {
        let old = match self.servers.remove(language) {
            Some(Slot::Running(server, _)) => Some(server),
            _ => None,
        };
        self.synced.retain(|_, s| s.language != language);
        self.pending.retain(|(l, _), _| *l != language);
        if self.completing.is_some_and(|((l, _), _)| l == language) {
            self.completing = None;
        }
        old
    }

    /// takes the servers packages bring now. a language that had none and
    /// failed to start gets another try with the new one.
    pub fn set_extra(&mut self, specs: Vec<ServerSpec>) {
        for spec in &specs {
            if matches!(self.servers.get(spec.language.as_str()), Some(Slot::Failed))
                && !self.extra.contains(spec)
            {
                self.servers.remove(spec.language.as_str());
            }
        }
        self.extra = specs;
    }

    /// languages that want a server and have none yet, with the file that
    /// asked. marks them starting.
    pub fn wanted(&mut self, docs: &Documents) -> Vec<(&'static str, PathBuf)> {
        let mut out = Vec::new();
        for doc in docs.all() {
            let Some(language) = doc.language else {
                continue;
            };
            if doc.load != Load::Ready || self.server_for(language).is_none() {
                continue;
            }
            if !self.servers.contains_key(language) {
                self.servers.insert(language, Slot::Starting);
                out.push((language, doc.path.clone()));
            }
        }
        out
    }

    /// the server of `language`, none when it has none or none was asked.
    pub fn state(&self, language: &str) -> Option<ServerState> {
        Some(match self.servers.get(language)? {
            Slot::Starting => ServerState::Starting,
            Slot::Running(..) => ServerState::Running,
            Slot::Failed => ServerState::Missing,
        })
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
        // closed documents, and ones whose file moved (renamed folder, save
        // as): closed under the old uri, opened again under the new one
        let gone: Vec<_> = self
            .synced
            .iter()
            .filter(|(id, s)| {
                docs.doc(**id)
                    .is_none_or(|d| uri::from_path(&d.path) != s.uri)
            })
            .map(|(id, _)| *id)
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
            let Some(spec) = self.server_for(language) else {
                continue;
            };
            let language_id = spec.language_id;
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
        self.ask(
            docs,
            view,
            "textDocument/definition",
            Pending::Definition,
            json!({}),
        )
        .is_some()
    }

    /// asks to rename the name at the caret to `name` everywhere.
    pub fn rename(&mut self, docs: &Documents, view: ViewId, name: &str) -> bool {
        let Some(kind) = edits_for(docs, view) else {
            return false;
        };
        let extra = json!({ "newName": name });
        self.ask(docs, view, "textDocument/rename", kind, extra)
            .is_some()
    }

    /// asks to format the whole document of `view`.
    pub fn format(&mut self, docs: &Documents, view: ViewId, tab: usize) -> bool {
        let Some(kind) = edits_for(docs, view) else {
            return false;
        };
        let extra = json!({ "options": { "tabSize": tab, "insertSpaces": true } });
        self.ask(docs, view, "textDocument/formatting", kind, extra)
            .is_some()
    }

    /// asks for code actions over the selection of `view`, with the
    /// problems it touches.
    pub fn actions(&mut self, docs: &Documents, view: ViewId) -> bool {
        let Some(Pending::Edits(id, revision, seen)) = edits_for(docs, view) else {
            return false;
        };
        let (Some(v), Some(doc)) = (docs.view(view), docs.doc(id)) else {
            return false;
        };
        let Some(synced) = self.synced.get(&id) else {
            return false;
        };
        if synced.revision != revision {
            return false;
        }
        let Some(Slot::Running(server, encoding)) = self.servers.get_mut(synced.language) else {
            return false;
        };
        let text = doc.buffer.text();
        let range = v.sel.range();
        let Some(at) = range_json(text, range.clone(), *encoding) else {
            return false;
        };
        let diagnostics: Vec<Value> = doc
            .diags
            .iter()
            .filter(|d| d.range.start <= range.end && range.start <= d.range.end)
            .take(50)
            .filter_map(|d| {
                let r = range_json(text, d.range.clone(), *encoding)?;
                let severity = match d.severity {
                    Severity::Error => 1,
                    Severity::Warning => 2,
                    Severity::Info => 3,
                    Severity::Hint => 4,
                };
                let extra = d
                    .extra
                    .as_deref()
                    .and_then(|e| serde_json::from_str(e).ok());
                Some(action::diag_json(r, severity, &d.message, extra.as_ref()))
            })
            .collect();
        let params = json!({
            "textDocument": { "uri": synced.uri },
            "range": at,
            "context": { "diagnostics": diagnostics, "triggerKind": 1 },
        });
        let language = synced.language;
        let id_out = server.request("textDocument/codeAction", params);
        self.pending
            .insert((language, id_out), Pending::Actions(id, revision, seen));
        true
    }

    /// asks the server to fill in the edit of `action`, picked from the
    /// actions for `doc` at `revision`.
    pub fn resolve_action(
        &mut self,
        language: &'static str,
        action: Value,
        doc: crate::document::DocumentId,
        revision: Revision,
        seen: Seen,
    ) -> bool {
        let Some(Slot::Running(server, _)) = self.servers.get_mut(language) else {
            return false;
        };
        let id = server.request("codeAction/resolve", action);
        self.pending
            .insert((language, id), Pending::Resolved(doc, revision, seen));
        true
    }

    /// asks where the name at the caret is used.
    pub fn references(&mut self, docs: &Documents, view: ViewId) -> bool {
        let extra = json!({ "context": { "includeDeclaration": true } });
        self.ask(
            docs,
            view,
            "textDocument/references",
            Pending::References,
            extra,
        )
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
        let Some(key) = self.ask(docs, view, "textDocument/completion", kind, json!({})) else {
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
        self.ask(
            docs,
            view,
            "textDocument/hover",
            Pending::Hover(view, at),
            json!({}),
        )
        .is_some()
    }

    /// asks the server of `view`'s file `method` at its caret, for the
    /// view fed by feed `key`. false when there is no server to ask yet.
    pub fn feed(&mut self, docs: &Documents, view: ViewId, method: &str, key: u32) -> bool {
        let at = docs.view(view).map_or(0, |v| v.sel.head);
        self.ask(docs, view, method, Pending::Feed(key, view, at), json!({}))
            .is_some()
    }

    /// sends `method` for the caret of `view`; gives the request's key.
    fn ask(
        &mut self,
        docs: &Documents,
        view: ViewId,
        method: &str,
        kind: Pending,
        extra: Value,
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
        let mut params = json!({
            "textDocument": { "uri": synced.uri },
            "position": { "line": at.line, "character": at.column },
        });
        if let (Some(params), Value::Object(extra)) = (params.as_object_mut(), extra) {
            params.extend(extra);
        }
        let id = server.request(method, params);
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
            (Some("$/lean/fileProgress"), None) => {
                if let Some(event) = self.progress(&msg["params"]) {
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
                    Some(Pending::Edits(doc, revision, seen)) => {
                        if let Some(why) = msg["error"]["message"].as_str() {
                            events.push(Event::Status(why.chars().take(200).collect()));
                            return;
                        }
                        let result = &msg["result"];
                        // a rename answers with a workspace edit, a format
                        // with the edits of the one document
                        let files = if result.is_array() {
                            let path = self.synced.get(&doc).and_then(|s| uri::to_path(&s.uri));
                            path.map(|p| vec![(p, edit::text_edits(result))])
                                .unwrap_or_default()
                        } else {
                            edit::workspace_edit(result)
                        };
                        events.push(Event::Edits(doc, revision, seen, files, encoding));
                    }
                    Some(Pending::Actions(doc, revision, seen)) => {
                        let actions = action::read(&msg["result"]);
                        events.push(Event::Actions(
                            doc, revision, seen, language, actions, encoding,
                        ));
                    }
                    Some(Pending::Resolved(doc, revision, seen)) => {
                        let files = edit::workspace_edit(&msg["result"]["edit"]);
                        events.push(Event::Edits(doc, revision, seen, files, encoding));
                    }
                    Some(Pending::References) => {
                        events.push(Event::Locations(edit::locations(&msg["result"]), encoding));
                    }
                    Some(Pending::Definition) => {
                        if let Some(event) = definition(&msg["result"], encoding) {
                            events.push(event);
                        }
                    }
                    Some(Pending::Feed(key, view, at)) => {
                        // an error reply is an empty one: the view says so
                        events.push(Event::Feed(key, view, at, msg["result"].clone()));
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

    /// the synced file `uri` names, the text a reply with `version` was
    /// made for (the newest when none), and the server's encoding. servers
    /// spell uris their own way (`c:` vs `C:`, `%3A`), so paths are matched.
    fn sent_for(
        &self,
        uri: &str,
        version: Option<i64>,
    ) -> Option<(crate::document::DocumentId, &TextSnapshot, Encoding)> {
        let path = uri::to_path(uri)?;
        let (&doc, synced) = self
            .synced
            .iter()
            .find(|(_, s)| uri::to_path(&s.uri).as_ref() == Some(&path))?;
        let Some(Slot::Running(_, encoding)) = self.servers.get(synced.language) else {
            return None;
        };
        let (_, text) = match version {
            Some(v) => synced.sent.iter().find(|(sent, _)| *sent == v)?,
            None => synced.sent.back()?,
        };
        Some((doc, text, *encoding))
    }

    fn progress(&self, params: &Value) -> Option<Event> {
        let doc = &params["textDocument"];
        let (id, text, encoding) = self.sent_for(doc["uri"].as_str()?, doc["version"].as_i64())?;
        let busy = params["processing"]
            .as_array()?
            .iter()
            .take(MAX_BUSY)
            .filter_map(|p| {
                let range = range_in(text, &p["range"], encoding)?;
                Some((range, p["kind"].as_i64() == Some(2)))
            })
            .collect();
        Some(Event::Busy(id, text.revision(), busy))
    }

    fn diagnostics(&self, params: &Value) -> Option<Event> {
        let (doc, text, encoding) =
            self.sent_for(params["uri"].as_str()?, params["version"].as_i64())?;
        let diags = params["diagnostics"]
            .as_array()?
            .iter()
            .filter_map(|d| {
                let range = range_in(text, &d["range"], encoding)?;
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
                    extra: action::diag_extra(d).map(|e| e.to_string()),
                })
            })
            .collect();
        Some(Event::Diagnostics(doc, text.revision(), diags))
    }
}

/// every open document and its revision when a request went out.
pub type Seen = Vec<(crate::document::DocumentId, Revision)>;

/// what an edit request is for: the document of `view` as it is now.
fn edits_for(docs: &Documents, view: ViewId) -> Option<Pending> {
    let v = docs.view(view)?;
    let doc = docs.doc(v.doc)?;
    let seen = docs
        .all_with_ids()
        .map(|(id, d)| (id, d.buffer.revision()))
        .collect();
    Some(Pending::Edits(v.doc, doc.buffer.revision(), seen))
}

fn point(value: &Value) -> Option<Point> {
    let line = usize::try_from(value["line"].as_u64()?).ok()?;
    let column = usize::try_from(value["character"].as_u64()?).ok()?;
    Some(Point::new(line, column))
}

/// a byte range as a server range.
fn range_json(text: &TextSnapshot, range: Range<usize>, encoding: Encoding) -> Option<Value> {
    let a = text.point(range.start, encoding).ok()?;
    let b = text.point(range.end, encoding).ok()?;
    Some(json!({
        "start": { "line": a.line, "character": a.column },
        "end": { "line": b.line, "character": b.column },
    }))
}

fn range_in(text: &TextSnapshot, range: &Value, encoding: Encoding) -> Option<Range<usize>> {
    let start = offset_near(text, point(&range["start"])?, encoding);
    let end = offset_near(text, point(&range["end"])?, encoding);
    Some(start.min(end)..start.max(end))
}

/// byte offset of a server position, pulled back onto the text when it is
/// past a line end, past the last line, or inside a char.
pub fn offset_near(text: &TextSnapshot, at: Point, encoding: Encoding) -> usize {
    // the line after the last one is the end of the text (an edit to the
    // end of a file without a final break names it)
    if at.line >= text.line_count() {
        return text.len();
    }
    let line = at.line;
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
    // markdown as it came; the card reads its fences
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use resolvase::TextBuffer;

    #[test]
    fn servers_start_in_the_nearest_marked_folder() {
        let dir = std::env::temp_dir().join(format!("ligase-root-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("proj/Sub/Deep")).unwrap();
        std::fs::write(dir.join("proj/lakefile.toml"), "").unwrap();
        let file = dir.join("proj/Sub/Deep/A.lean");
        let marks = vec!["lakefile.lean".to_owned(), "lakefile.toml".to_owned()];
        assert_eq!(
            root_for(&file, &marks, &dir.join("proj/Sub")),
            dir.join("proj")
        );
        assert_eq!(root_for(&file, &[], &dir), dir, "no marks, the workspace");
        let none = vec!["Cargo.toml".to_owned()];
        assert_eq!(root_for(&file, &none, &dir), dir, "no mark found");
        let _ = std::fs::remove_dir_all(&dir);
    }

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
        assert_eq!(
            offset_near(&text, Point::new(9, 1), Encoding::Utf8),
            text.len(),
            "past the last line is the end"
        );
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
        assert_eq!(hover_text(&markup), "```rust\nfn x()\n```\ndoes x");
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
        assert_eq!(lsp.wanted(&docs), [("rust", main.clone())]);
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
        // rename `x` (offset 20) to `renamed`
        docs.view_mut(view).unwrap().sel = crate::editor::cursor::Selection::caret(20);
        assert!(lsp.rename(&docs, view, "renamed"));
        let start = Instant::now();
        let mut renamed = None;
        while start.elapsed() < Duration::from_secs(30) && renamed.is_none() {
            std::thread::sleep(Duration::from_millis(100));
            for event in lsp.poll() {
                if let Event::Edits(_, _, _, files, _) = event {
                    renamed = Some(files);
                }
            }
        }
        let files = renamed.expect("a rename edit");
        assert!(
            files
                .iter()
                .any(|(_, edits)| edits.iter().any(|e| e.text == "renamed"))
        );
        // completion after "x." on a u32 lists its methods
        let end = docs.doc(doc).unwrap().buffer.len() - 2;
        docs.edit(view, vec![(end..end, "\n    x.".to_owned())], false);
        let caret = end + "\n    x.".len();
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
