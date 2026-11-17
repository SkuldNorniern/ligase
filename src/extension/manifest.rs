//! extension.toml parsing. the file is untrusted: sizes are capped, every
//! value is checked, and bad input comes back as messages, never panics.

use std::collections::HashSet;

use toml::{Table, Value};

use super::ui::{self, Node};

/// largest manifest read, in bytes. bigger text is refused before parsing.
pub const MAX_BYTES: usize = 256 * 1024;

/// the extension api this build of ligase speaks.
pub const API: &str = "1";

const MAX_ID: usize = 100;
const MAX_NAME: usize = 80;
const MAX_DESCRIPTION: usize = 300;
const MAX_EVENTS: usize = 50;
const MAX_LANGUAGES: usize = 100;
const MAX_LANG_ID: usize = 40;
const MAX_LANG_NAME: usize = 40;
const MAX_EXTENSIONS: usize = 20;
const MAX_EXTENSION: usize = 16;
const MAX_FILES: usize = 20;
const MAX_FILE: usize = 64;
const MAX_COMMENT: usize = 8;
const MAX_SNIPPETS: usize = 500;
const MAX_PREFIX: usize = 40;
const MAX_BODY: usize = 4096;
const MAX_SNIPPET_DESCRIPTION: usize = 120;
const MAX_VIEWS: usize = 20;
const MAX_SYNTAXES: usize = 10;
const MAX_SERVERS: usize = 5;
const MAX_ABBREV_SETS: usize = 10;
const MAX_ARGS: usize = 20;
const MAX_ARG: usize = 200;
const MAX_ROOTS: usize = 8;
const MAX_PATH: usize = 200;
const MAX_VIEW_ID: usize = 64;
const MAX_TITLE: usize = 40;

const LANG_ID_RULE: &str = "1-40 of a-z 0-9 _ + -, starting with a letter";

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Manifest {
    pub id: String,
    pub name: String,
    pub version: (u32, u32, u32),
    pub description: String,
    pub activation: Vec<Activation>,
    pub permissions: Permissions,
    pub languages: Vec<Language>,
    pub snippets: Vec<Snippet>,
    pub views: Vec<View>,
    /// grammar files in the package, relative paths with `/`.
    pub syntaxes: Vec<String>,
    /// language servers to start, once the user allows it.
    pub servers: Vec<Server>,
    /// abbreviation tables, each a file in the package.
    pub abbreviations: Vec<AbbrevSet>,
    /// things skipped but not fatal, e.g. unknown keys.
    pub warnings: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Activation {
    Startup,
    Language(String),
    Command(String),
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Permissions {
    pub read_workspace: bool,
    pub write_documents: bool,
    pub spawn_process: bool,
    pub network: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Language {
    pub id: String,
    pub name: String,
    pub extensions: Vec<String>,
    pub files: Vec<String>,
    pub comment: Option<String>,
    pub syntax: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Snippet {
    pub language: String,
    pub prefix: String,
    pub body: String,
    pub description: String,
}

/// a panel a package adds, drawn by ligase from a ui tree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct View {
    pub id: String,
    pub title: String,
    pub placement: Placement,
    /// a bundled icon name for its rail button.
    pub icon: String,
    pub body: Node,
    /// a server request whose answer shows under the body.
    /// sections under the body, each filled from where the caret is.
    pub feeds: Vec<Feed>,
}

/// what feeds a view: a request to the language server of the file in
/// front, at its caret, the answer's `field` shown as markdown.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Feed {
    /// heading over the section; none when empty.
    pub title: String,
    pub source: Source,
    /// the text shown as a code block, not markdown.
    pub code: bool,
    /// a dotted path into the result; empty for the result itself.
    pub field: String,
    /// shown while there is nothing.
    pub empty: String,
}

/// where a section's text comes from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Source {
    /// a server request at the caret, by method name.
    Request(String),
    /// the problems on the caret's line, whole.
    Messages,
}

/// most sections in one view.
const MAX_FEEDS: usize = 8;

/// abbreviations for a language: a leader char and a table file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AbbrevSet {
    pub language: String,
    pub leader: char,
    /// a toml file of `"key" = "text"` inside the package.
    pub file: String,
}

/// a language server a package starts for a language's files.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Server {
    /// the language (a grammar or bundled syntax name) it serves.
    pub language: String,
    /// a program on the path; never a path itself.
    pub command: String,
    pub args: Vec<String>,
    /// the id sent with each file; the language when not given.
    pub language_id: String,
    /// file names that mark a project's folder. the server starts in the
    /// nearest folder above the file holding one; the workspace when none.
    pub roots: Vec<String>,
}

impl Server {
    /// program then args, the way a grant records it.
    pub fn line(&self) -> Vec<String> {
        std::iter::once(self.command.clone())
            .chain(self.args.iter().cloned())
            .collect()
    }
}

/// where a view sits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Placement {
    /// in the side bar, one panel at a time, with a rail button.
    Side,
    /// a tab with the terminal and problems.
    Bottom,
}

/// collected while parsing. errors make the parse fail, warnings do not.
#[derive(Default)]
struct Report {
    errors: Vec<String>,
    warnings: Vec<String>,
}

impl Report {
    fn error(&mut self, path: &str, msg: &str) {
        self.errors.push(format!("{path}: {msg}"));
    }
}

/// parses an extension.toml. all errors are returned together.
pub fn parse(text: &str) -> Result<Manifest, Vec<String>> {
    if text.len() > MAX_BYTES {
        return Err(vec![format!(
            "manifest is {} bytes, the limit is {MAX_BYTES}",
            text.len()
        )]);
    }
    let root: Table = toml::from_str(text).map_err(|e| vec![format!("toml: {e}")])?;

    let mut report = Report::default();
    check_keys(
        &root,
        "",
        &["package", "activation", "permissions", "contributions"],
        &mut report,
    );
    let mut manifest = Manifest::default();

    let empty = Table::new();
    let package = subtable(&root, "", "package", &mut report).unwrap_or(&empty);
    parse_package(package, &mut manifest, &mut report);

    if let Some(t) = subtable(&root, "", "activation", &mut report) {
        manifest.activation = parse_activation(t, &mut report);
    }
    if let Some(t) = subtable(&root, "", "permissions", &mut report) {
        manifest.permissions = parse_permissions(t, &mut report);
    }
    if let Some(t) = subtable(&root, "", "contributions", &mut report) {
        parse_contributions(t, &mut manifest, &mut report);
    }

    if report.errors.is_empty() {
        manifest.warnings = report.warnings;
        Ok(manifest)
    } else {
        Err(report.errors)
    }
}

fn parse_package(table: &Table, out: &mut Manifest, report: &mut Report) {
    check_keys(
        table,
        "package",
        &["id", "name", "version", "api", "description"],
        report,
    );

    if let Some(id) = required_string(table, "package", "id", report) {
        if dotted_id_ok(id) {
            out.id = id.to_owned();
        } else {
            report.error(
                "package.id",
                "must be 2-100 bytes, dot-separated parts of a-z 0-9 and -, at least two parts, no leading or trailing -",
            );
        }
    }

    if let Some(name) = required_string(table, "package", "name", report) {
        if name_ok(name) {
            out.name = name.to_owned();
        } else {
            report.error("package.name", "must be 1-80 chars, no control chars");
        }
    }

    if let Some(version) = required_string(table, "package", "version", report) {
        match parse_version(version) {
            Some(v) => out.version = v,
            None => report.error(
                "package.version",
                "must be three dot-separated numbers, like 0.1.0",
            ),
        }
    }

    if let Some(api) = required_string(table, "package", "api", report)
        && api != API
    {
        report.error(
            "package.api",
            &format!("needs api {}, this ligase has {API}", shown(api)),
        );
    }

    if let Some(desc) = string(table, "package", "description", report) {
        if desc.chars().count() <= MAX_DESCRIPTION && !has_control(desc) {
            out.description = desc.to_owned();
        } else {
            report.error(
                "package.description",
                "must be at most 300 chars, no control chars",
            );
        }
    }
}

fn parse_activation(table: &Table, report: &mut Report) -> Vec<Activation> {
    check_keys(table, "activation", &["events"], report);
    let Some(items) = list(table, "activation", "events", MAX_EVENTS, report) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (i, item) in items.iter().enumerate() {
        let path = format!("activation.events[{i}]");
        let Some(text) = item_string(item, &path, report) else {
            continue;
        };
        match parse_event(text) {
            Some(event) => out.push(event),
            None => report.error(
                &path,
                &format!(
                    "bad event {}, use startup, language:<id> or command:<id>",
                    shown(text)
                ),
            ),
        }
    }
    out
}

fn parse_event(text: &str) -> Option<Activation> {
    if text == "startup" {
        return Some(Activation::Startup);
    }
    if let Some(id) = text.strip_prefix("language:") {
        return lang_id_ok(id).then(|| Activation::Language(id.to_owned()));
    }
    if let Some(id) = text.strip_prefix("command:") {
        return dotted_id_ok(id).then(|| Activation::Command(id.to_owned()));
    }
    None
}

fn parse_permissions(table: &Table, report: &mut Report) -> Permissions {
    check_keys(
        table,
        "permissions",
        &[
            "read_workspace",
            "write_documents",
            "spawn_process",
            "network",
        ],
        report,
    );
    Permissions {
        read_workspace: boolean(table, "permissions", "read_workspace", report).unwrap_or(false),
        write_documents: boolean(table, "permissions", "write_documents", report).unwrap_or(false),
        spawn_process: boolean(table, "permissions", "spawn_process", report).unwrap_or(false),
        network: boolean(table, "permissions", "network", report).unwrap_or(false),
    }
}

fn parse_contributions(table: &Table, out: &mut Manifest, report: &mut Report) {
    check_keys(
        table,
        "contributions",
        &[
            "languages",
            "snippets",
            "views",
            "syntaxes",
            "servers",
            "abbreviations",
        ],
        report,
    );

    if let Some(items) = list(
        table,
        "contributions",
        "abbreviations",
        MAX_ABBREV_SETS,
        report,
    ) {
        for (i, item) in items.iter().enumerate() {
            let path = format!("contributions.abbreviations[{i}]");
            let Some(t) = item_table(item, &path, report) else {
                continue;
            };
            check_keys(t, &path, &["language", "leader", "file"], report);
            let language = required_string(t, &path, "language", report).and_then(|l| {
                let ok = lang_id_ok(l);
                if !ok {
                    report.error(&join(&path, "language"), LANG_ID_RULE);
                }
                ok.then(|| l.to_owned())
            });
            let leader = match string(t, &path, "leader", report) {
                None => Some('\\'),
                Some(l) => {
                    let mut chars = l.chars();
                    match (chars.next(), chars.next()) {
                        (Some(c), None)
                            if !c.is_alphanumeric() && !c.is_whitespace() && !c.is_control() =>
                        {
                            Some(c)
                        }
                        _ => {
                            report.error(
                                &join(&path, "leader"),
                                "must be one char, not a letter, digit or space",
                            );
                            None
                        }
                    }
                }
            };
            let file = required_string(t, &path, "file", report).and_then(|f| {
                let ok = package_path_ok(f) && f.ends_with(".toml");
                if !ok {
                    report.error(
                        &join(&path, "file"),
                        "must be a path inside the package ending in .toml",
                    );
                }
                ok.then(|| f.to_owned())
            });
            if let (Some(language), Some(leader), Some(file)) = (language, leader, file) {
                out.abbreviations.push(AbbrevSet {
                    language,
                    leader,
                    file,
                });
            }
        }
    }

    if let Some(items) = list(table, "contributions", "servers", MAX_SERVERS, report) {
        if !items.is_empty() && !out.permissions.spawn_process {
            report.error(
                "contributions.servers",
                "starting a server needs [permissions] spawn_process = true",
            );
        }
        for (i, item) in items.iter().enumerate() {
            let path = format!("contributions.servers[{i}]");
            let Some(t) = item_table(item, &path, report) else {
                continue;
            };
            let Some(server) = parse_server(t, &path, report) else {
                continue;
            };
            if out.servers.iter().any(|s| s.language == server.language) {
                report.error(
                    &join(&path, "language"),
                    &format!("a second server for {}", shown(&server.language)),
                );
            } else {
                out.servers.push(server);
            }
        }
    }

    if let Some(items) = list(table, "contributions", "syntaxes", MAX_SYNTAXES, report) {
        for (i, item) in items.iter().enumerate() {
            let path = format!("contributions.syntaxes[{i}]");
            let Some(t) = item_table(item, &path, report) else {
                continue;
            };
            check_keys(t, &path, &["file"], report);
            let Some(file) = required_string(t, &path, "file", report) else {
                continue;
            };
            if !package_path_ok(file) || !file.ends_with(".sylven") {
                report.error(
                    &join(&path, "file"),
                    "must be a path inside the package ending in .sylven, parts of a-z A-Z 0-9 _ . - split by /",
                );
            } else if out.syntaxes.iter().any(|f| f == file) {
                report.error(&join(&path, "file"), "listed twice");
            } else {
                out.syntaxes.push(file.to_owned());
            }
        }
    }

    if let Some(items) = list(table, "contributions", "views", MAX_VIEWS, report) {
        for (i, item) in items.iter().enumerate() {
            let path = format!("contributions.views[{i}]");
            let Some(t) = item_table(item, &path, report) else {
                continue;
            };
            let Some(view) = parse_view(t, &path, report) else {
                continue;
            };
            if out.views.iter().any(|other| other.id == view.id) {
                report.error(
                    &join(&path, "id"),
                    &format!("duplicate view {}", shown(&view.id)),
                );
            } else {
                out.views.push(view);
            }
        }
    }

    if let Some(items) = list(table, "contributions", "languages", MAX_LANGUAGES, report) {
        for (i, item) in items.iter().enumerate() {
            let path = format!("contributions.languages[{i}]");
            let Some(t) = item_table(item, &path, report) else {
                continue;
            };
            let Some(lang) = parse_language(t, &path, report) else {
                continue;
            };
            if out.languages.iter().any(|other| other.id == lang.id) {
                report.error(
                    &join(&path, "id"),
                    &format!("duplicate language {}", shown(&lang.id)),
                );
            } else {
                out.languages.push(lang);
            }
        }
    }

    if let Some(items) = list(table, "contributions", "snippets", MAX_SNIPPETS, report) {
        let mut seen = HashSet::new();
        for (i, item) in items.iter().enumerate() {
            let path = format!("contributions.snippets[{i}]");
            let Some(t) = item_table(item, &path, report) else {
                continue;
            };
            let Some(snippet) = parse_snippet(t, &path, report) else {
                continue;
            };
            if seen.insert((snippet.language.clone(), snippet.prefix.clone())) {
                out.snippets.push(snippet);
            } else {
                report.error(
                    &join(&path, "prefix"),
                    &format!(
                        "duplicate prefix {} for language {}",
                        shown(&snippet.prefix),
                        shown(&snippet.language)
                    ),
                );
            }
        }
    }
}

fn parse_feed(table: &Table, path: &str, report: &mut Report) -> Option<Feed> {
    check_keys(
        table,
        path,
        &["title", "request", "source", "field", "empty", "code"],
        report,
    );
    let source = match (table.get("request"), string(table, path, "source", report)) {
        (None, Some("messages")) => Some(Source::Messages),
        (None, Some(_)) => {
            report.error(&join(path, "source"), "must be messages");
            None
        }
        (Some(_), Some(_)) => {
            report.error(path, "has both request and source; give one");
            None
        }
        (_, None) => parse_request(table, path, report).map(Source::Request),
    };
    let title = match string(table, path, "title", report) {
        None => Some(String::new()),
        Some(t) if t.chars().count() <= 60 && !has_control(t) => Some(t.to_owned()),
        Some(_) => {
            report.error(
                &join(path, "title"),
                "must be at most 60 chars, no control chars",
            );
            None
        }
    };
    let code = match table.get("code") {
        None => false,
        Some(Value::Boolean(b)) => *b,
        Some(other) => {
            report.error(
                &join(path, "code"),
                &format!("must be true or false, not {}", kind(other)),
            );
            false
        }
    };

    let field = match string(table, path, "field", report) {
        None => Some(String::new()),
        Some(f)
            if f.len() <= 64
                && f.split('.').all(|p| {
                    !p.is_empty() && p.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
                }) =>
        {
            Some(f.to_owned())
        }
        Some(_) => {
            report.error(
                &join(path, "field"),
                "must be a dotted path like rendered or goal.text",
            );
            None
        }
    };
    let empty = match string(table, path, "empty", report) {
        None => Some(String::new()),
        Some(e) if e.chars().count() <= 200 && !has_control(e) => Some(e.to_owned()),
        Some(_) => {
            report.error(
                &join(path, "empty"),
                "must be at most 200 chars, no control chars",
            );
            None
        }
    };
    Some(Feed {
        title: title?,
        source: source?,
        code,
        field: field?,
        empty: empty?,
    })
}

fn parse_request(table: &Table, path: &str, report: &mut Report) -> Option<String> {
    required_string(table, path, "request", report).and_then(|r| {
        let ok = !r.is_empty()
            && r.len() <= 100
            && r.bytes().all(|b| {
                b.is_ascii_alphanumeric() || matches!(b, b'_' | b'$' | b'/' | b'.' | b'-')
            });
        if !ok {
            report.error(
                &join(path, "request"),
                "must be a method name, 1-100 of a-z A-Z 0-9 _ $ / . -",
            );
        }
        ok.then(|| r.to_owned())
    })
}

fn parse_view(table: &Table, path: &str, report: &mut Report) -> Option<View> {
    check_keys(
        table,
        path,
        &["id", "title", "placement", "icon", "body", "feed"],
        report,
    );
    let id = required_string(table, path, "id", report);
    let id_ok = id.is_some_and(|id| {
        let ok = !id.is_empty()
            && id.len() <= MAX_VIEW_ID
            && id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-'));
        if !ok {
            report.error(
                &join(path, "id"),
                &format!("must be 1-{MAX_VIEW_ID} of a-z A-Z 0-9 _ . -"),
            );
        }
        ok
    });
    let title = required_string(table, path, "title", report);
    let title_ok = title.is_some_and(|t| {
        let ok = !t.is_empty() && t.chars().count() <= MAX_TITLE && !has_control(t);
        if !ok {
            report.error(
                &join(path, "title"),
                &format!("must be 1-{MAX_TITLE} chars, no control chars"),
            );
        }
        ok
    });
    let placement = match string(table, path, "placement", report) {
        None | Some("side") => Some(Placement::Side),
        Some("bottom") => Some(Placement::Bottom),
        Some(other) => {
            report.error(
                &join(path, "placement"),
                &format!("{} is not side or bottom", shown(other)),
            );
            None
        }
    };
    let icon = match string(table, path, "icon", report) {
        None => Some("extensions"),
        Some(icon)
            if icon.len() <= 32 && icon.bytes().all(|b| b.is_ascii_lowercase() || b == b'-') =>
        {
            Some(icon)
        }
        Some(other) => {
            report.error(
                &join(path, "icon"),
                &format!("{} is not an icon name", shown(other)),
            );
            None
        }
    };
    let body = match table.get("body") {
        None => {
            report.error(&join(path, "body"), "missing required field");
            None
        }
        Some(value) => match ui::read(value, &join(path, "body")) {
            Ok((node, warnings)) => {
                // what read lets through, check lets through too
                debug_assert!(ui::check(&node).is_ok());
                report.warnings.extend(warnings);
                Some(node)
            }
            Err(errors) => {
                report.errors.extend(errors);
                None
            }
        },
    };
    // one table, or a list of them for sections
    let feeds = match table.get("feed") {
        None => Some(Vec::new()),
        Some(Value::Table(t)) => parse_feed(t, &join(path, "feed"), report).map(|f| vec![f]),
        Some(Value::Array(items)) if items.len() <= MAX_FEEDS => {
            let mut out = Vec::new();
            let mut ok = true;
            for (i, item) in items.iter().enumerate() {
                let at = format!("{path}.feed[{i}]");
                match item {
                    Value::Table(t) => match parse_feed(t, &at, report) {
                        Some(f) => out.push(f),
                        None => ok = false,
                    },
                    other => {
                        report.error(&at, &format!("must be a table, not {}", kind(other)));
                        ok = false;
                    }
                }
            }
            ok.then_some(out)
        }
        Some(Value::Array(_)) => {
            report.error(
                &join(path, "feed"),
                &format!("at most {MAX_FEEDS} sections"),
            );
            None
        }
        Some(other) => {
            report.error(
                &join(path, "feed"),
                &format!("must be a table or a list of them, not {}", kind(other)),
            );
            None
        }
    };
    if !id_ok || !title_ok {
        return None;
    }
    Some(View {
        feeds: feeds?,
        id: id?.to_owned(),
        title: title?.to_owned(),
        placement: placement?,
        icon: icon?.to_owned(),
        body: body?,
    })
}

fn parse_language(table: &Table, path: &str, report: &mut Report) -> Option<Language> {
    check_keys(
        table,
        path,
        &["id", "name", "extensions", "files", "comment", "syntax"],
        report,
    );

    let id = match required_string(table, path, "id", report) {
        Some(id) if lang_id_ok(id) => Some(id.to_owned()),
        Some(_) => {
            report.error(&join(path, "id"), LANG_ID_RULE);
            None
        }
        None => None,
    };

    let name = match string(table, path, "name", report) {
        Some(n) if n.chars().count() <= MAX_LANG_NAME => n.to_owned(),
        Some(_) => {
            report.error(&join(path, "name"), "must be at most 40 chars");
            String::new()
        }
        None => String::new(),
    };

    let mut extensions = Vec::new();
    if let Some(items) = list(table, path, "extensions", MAX_EXTENSIONS, report) {
        for (i, item) in items.iter().enumerate() {
            let p = format!("{path}.extensions[{i}]");
            if let Some(ext) = item_string(item, &p, report) {
                let lower = ext.to_ascii_lowercase();
                if extension_ok(&lower) {
                    extensions.push(lower);
                } else {
                    report.error(&p, "must be 1-16 of a-z 0-9 _ + - (no dot)");
                }
            }
        }
    }

    let mut files = Vec::new();
    if let Some(items) = list(table, path, "files", MAX_FILES, report) {
        for (i, item) in items.iter().enumerate() {
            let p = format!("{path}.files[{i}]");
            if let Some(file) = item_string(item, &p, report) {
                if file_ok(file) {
                    files.push(file.to_owned());
                } else {
                    report.error(&p, "must be 1-64 chars, no / \\ or control chars");
                }
            }
        }
    }

    let comment = match string(table, path, "comment", report) {
        Some(c) if (1..=MAX_COMMENT).contains(&c.chars().count()) && !has_space(c) => {
            Some(c.to_owned())
        }
        Some(_) => {
            report.error(&join(path, "comment"), "must be 1-8 chars, no whitespace");
            None
        }
        None => None,
    };

    let syntax = match string(table, path, "syntax", report) {
        Some(s) if lang_id_ok(s) => Some(s.to_owned()),
        Some(_) => {
            report.error(&join(path, "syntax"), LANG_ID_RULE);
            None
        }
        None => None,
    };

    let id = id?;
    let name = if name.is_empty() { id.clone() } else { name };
    Some(Language {
        id,
        name,
        extensions,
        files,
        comment,
        syntax,
    })
}

fn parse_snippet(table: &Table, path: &str, report: &mut Report) -> Option<Snippet> {
    check_keys(
        table,
        path,
        &["language", "prefix", "body", "description"],
        report,
    );

    let language = match required_string(table, path, "language", report) {
        Some(l) if lang_id_ok(l) => Some(l.to_owned()),
        Some(_) => {
            report.error(&join(path, "language"), LANG_ID_RULE);
            None
        }
        None => None,
    };

    let prefix = match required_string(table, path, "prefix", report) {
        Some(p) if (1..=MAX_PREFIX).contains(&p.chars().count()) && !has_space(p) => {
            Some(p.to_owned())
        }
        Some(_) => {
            report.error(&join(path, "prefix"), "must be 1-40 chars, no whitespace");
            None
        }
        None => None,
    };

    let body = match required_string(table, path, "body", report) {
        Some(b) if (1..=MAX_BODY).contains(&b.len()) => Some(b.to_owned()),
        Some(_) => {
            report.error(&join(path, "body"), "must be 1-4096 bytes");
            None
        }
        None => None,
    };

    let description = match string(table, path, "description", report) {
        Some(d) if d.chars().count() <= MAX_SNIPPET_DESCRIPTION => d.to_owned(),
        Some(_) => {
            report.error(&join(path, "description"), "must be at most 120 chars");
            String::new()
        }
        None => String::new(),
    };

    Some(Snippet {
        language: language?,
        prefix: prefix?,
        body: body?,
        description,
    })
}

/// key path for a field: `path.key`, or just `key` at the root.
fn join(path: &str, key: &str) -> String {
    if path.is_empty() {
        key.to_owned()
    } else {
        format!("{path}.{key}")
    }
}

/// a value echoed back in a message. cut short and escaped, since it came from outside.
fn shown(s: &str) -> String {
    s.chars().take(40).flat_map(char::escape_debug).collect()
}

fn kind(value: &Value) -> &'static str {
    match value {
        Value::String(_) => "a string",
        Value::Integer(_) => "an integer",
        Value::Float(_) => "a float",
        Value::Boolean(_) => "a bool",
        Value::Datetime(_) => "a datetime",
        Value::Array(_) => "an array",
        Value::Table(_) => "a table",
    }
}

/// adds a warning for every key that is not in `known`.
fn check_keys(table: &Table, path: &str, known: &[&str], report: &mut Report) {
    for key in table.keys() {
        if !known.contains(&key.as_str()) {
            report
                .warnings
                .push(format!("unknown key: {}", join(path, key)));
        }
    }
}

/// a string field. `None` when absent or not a string (the latter is reported).
fn string<'a>(table: &'a Table, path: &str, key: &str, report: &mut Report) -> Option<&'a str> {
    let value = table.get(key)?;
    match value {
        Value::String(s) => Some(s.as_str()),
        other => {
            report.error(
                &join(path, key),
                &format!("expected a string, got {}", kind(other)),
            );
            None
        }
    }
}

/// a string field that must be there.
fn required_string<'a>(
    table: &'a Table,
    path: &str,
    key: &str,
    report: &mut Report,
) -> Option<&'a str> {
    if table.contains_key(key) {
        string(table, path, key, report)
    } else {
        report.error(&join(path, key), "missing required field");
        None
    }
}

fn boolean(table: &Table, path: &str, key: &str, report: &mut Report) -> Option<bool> {
    let value = table.get(key)?;
    match value {
        Value::Boolean(b) => Some(*b),
        other => {
            report.error(
                &join(path, key),
                &format!("expected a bool, got {}", kind(other)),
            );
            None
        }
    }
}

fn subtable<'a>(table: &'a Table, path: &str, key: &str, report: &mut Report) -> Option<&'a Table> {
    let value = table.get(key)?;
    match value {
        Value::Table(t) => Some(t),
        other => {
            report.error(
                &join(path, key),
                &format!("expected a table, got {}", kind(other)),
            );
            None
        }
    }
}

/// an array field with at most `max` entries. over the cap it is reported and skipped.
fn list<'a>(
    table: &'a Table,
    path: &str,
    key: &str,
    max: usize,
    report: &mut Report,
) -> Option<&'a [Value]> {
    let value = table.get(key)?;
    let Value::Array(items) = value else {
        report.error(
            &join(path, key),
            &format!("expected an array, got {}", kind(value)),
        );
        return None;
    };
    if items.len() > max {
        report.error(
            &join(path, key),
            &format!("at most {max} entries, got {}", items.len()),
        );
        return None;
    }
    Some(items.as_slice())
}

fn item_string<'a>(value: &'a Value, path: &str, report: &mut Report) -> Option<&'a str> {
    match value {
        Value::String(s) => Some(s.as_str()),
        other => {
            report.error(path, &format!("expected a string, got {}", kind(other)));
            None
        }
    }
}

fn item_table<'a>(value: &'a Value, path: &str, report: &mut Report) -> Option<&'a Table> {
    match value {
        Value::Table(t) => Some(t),
        other => {
            report.error(path, &format!("expected a table, got {}", kind(other)));
            None
        }
    }
}

fn parse_server(table: &Table, path: &str, report: &mut Report) -> Option<Server> {
    check_keys(
        table,
        path,
        &["language", "command", "args", "language_id", "roots"],
        report,
    );
    let mut ok = true;
    let mut word = |key: &str, required: bool, report: &mut Report| -> Option<String> {
        let value = if required {
            required_string(table, path, key, report)
        } else {
            string(table, path, key, report)
        }?;
        if !lang_id_ok(value) {
            report.error(&join(path, key), LANG_ID_RULE);
            ok = false;
            return None;
        }
        Some(value.to_owned())
    };
    let language = word("language", true, report);
    let language_id = word("language_id", false, report);
    let command = required_string(table, path, "command", report).and_then(|c| {
        let fine = !c.is_empty()
            && c.len() <= 64
            && c.bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-'));
        if !fine {
            report.error(
                &join(path, "command"),
                "must be a program name, 1-64 of a-z A-Z 0-9 _ . -, no path",
            );
        }
        fine.then(|| c.to_owned())
    });
    let mut args = Vec::new();
    if let Some(items) = list(table, path, "args", MAX_ARGS, report) {
        for (i, item) in items.iter().enumerate() {
            let at = format!("{path}.args[{i}]");
            let Some(arg) = item_string(item, &at, report) else {
                ok = false;
                continue;
            };
            if arg.is_empty() || arg.len() > MAX_ARG || has_control(arg) {
                report.error(&at, &format!("must be 1-{MAX_ARG} bytes, no control chars"));
                ok = false;
            } else {
                args.push(arg.to_owned());
            }
        }
    }
    let mut roots = Vec::new();
    if let Some(items) = list(table, path, "roots", MAX_ROOTS, report) {
        for (i, item) in items.iter().enumerate() {
            let at = format!("{path}.roots[{i}]");
            let Some(name) = item_string(item, &at, report) else {
                ok = false;
                continue;
            };
            if package_path_ok(name) && !name.contains('/') {
                roots.push(name.to_owned());
            } else {
                report.error(&at, "must be a file name: a-z A-Z 0-9 _ . -, no path");
                ok = false;
            }
        }
    }
    let language = language?;
    if !ok {
        return None;
    }
    Some(Server {
        language_id: language_id.unwrap_or_else(|| language.clone()),
        language,
        command: command?,
        args,
        roots,
    })
}

/// a relative path that stays in the package: no root, no `..`, plain parts.
fn package_path_ok(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= MAX_PATH
        && s.split('/').all(|part| {
            !part.is_empty()
                && part != "."
                && part != ".."
                && part
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-'))
        })
}

fn word_byte(b: u8) -> bool {
    b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'_' | b'+' | b'-')
}

fn has_control(s: &str) -> bool {
    s.chars().any(char::is_control)
}

fn has_space(s: &str) -> bool {
    s.chars().any(char::is_whitespace)
}

/// 2-100 bytes, at least two dot-separated parts, each a-z 0-9 - and not
/// starting or ending with '-'.
fn dotted_id_ok(s: &str) -> bool {
    if !(2..=MAX_ID).contains(&s.len()) {
        return false;
    }
    let mut parts = 0;
    for part in s.split('.') {
        parts += 1;
        let b = part.as_bytes();
        let ok = !b.is_empty()
            && b.iter()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'-')
            && b.first() != Some(&b'-')
            && b.last() != Some(&b'-');
        if !ok {
            return false;
        }
    }
    parts >= 2
}

/// 1-40 of a-z 0-9 _ + -, starting with a letter.
fn lang_id_ok(s: &str) -> bool {
    (1..=MAX_LANG_ID).contains(&s.len())
        && s.as_bytes().first().is_some_and(u8::is_ascii_lowercase)
        && s.bytes().all(word_byte)
}

/// 1-16 of a-z 0-9 _ + - with no dot. callers lowercase first.
fn extension_ok(s: &str) -> bool {
    (1..=MAX_EXTENSION).contains(&s.len()) && s.bytes().all(word_byte)
}

fn file_ok(s: &str) -> bool {
    let n = s.chars().count();
    (1..=MAX_FILE).contains(&n) && !s.chars().any(|c| matches!(c, '/' | '\\') || c.is_control())
}

fn name_ok(s: &str) -> bool {
    (1..=MAX_NAME).contains(&s.chars().count()) && !has_control(s)
}

/// exactly three dot-separated u32 numbers.
fn parse_version(s: &str) -> Option<(u32, u32, u32)> {
    let mut nums = [0u32; 3];
    let mut count = 0;
    for part in s.split('.') {
        if count == 3 || part.is_empty() || !part.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        nums[count] = part.parse().ok()?;
        count += 1;
    }
    (count == 3).then_some((nums[0], nums[1], nums[2]))
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXAMPLE: &str = r#"[package]
id = "org.example.lean"
name = "Lean Support"
version = "0.1.0"
api = "1"
description = "Lean 4 language support."

[activation]
events = ["language:lean", "command:org.example.lean.goals", "startup"]

[permissions]
read_workspace = true
write_documents = false
spawn_process = true
network = false

[[contributions.languages]]
id = "lean"
name = "Lean"
extensions = ["lean"]
files = ["Leanfile"]
comment = "--"
syntax = "rust"

[[contributions.snippets]]
language = "lean"
prefix = "thm"
body = "theorem $1 : $2 := by\n  $0"
description = "a theorem"
"#;

    const PACKAGE: &str = "[package]\nid = \"org.example.lean\"\nname = \"Lean Support\"\nversion = \"0.1.0\"\napi = \"1\"\n";

    fn ok(text: &str) -> Manifest {
        match parse(text) {
            Ok(m) => m,
            Err(e) => panic!("expected ok, got {e:?}"),
        }
    }

    fn errs(text: &str) -> Vec<String> {
        match parse(text) {
            Ok(m) => panic!("expected errors, got {m:?}"),
            Err(e) => e,
        }
    }

    fn has(list: &[String], needle: &str) -> bool {
        list.iter().any(|m| m.contains(needle))
    }

    #[test]
    fn full_example_parses() {
        let m = ok(EXAMPLE);
        let expected = Manifest {
            views: Vec::new(),
            syntaxes: Vec::new(),
            servers: Vec::new(),
            abbreviations: Vec::new(),
            id: "org.example.lean".into(),
            name: "Lean Support".into(),
            version: (0, 1, 0),
            description: "Lean 4 language support.".into(),
            activation: vec![
                Activation::Language("lean".into()),
                Activation::Command("org.example.lean.goals".into()),
                Activation::Startup,
            ],
            permissions: Permissions {
                read_workspace: true,
                write_documents: false,
                spawn_process: true,
                network: false,
            },
            languages: vec![Language {
                id: "lean".into(),
                name: "Lean".into(),
                extensions: vec!["lean".into()],
                files: vec!["Leanfile".into()],
                comment: Some("--".into()),
                syntax: Some("rust".into()),
            }],
            snippets: vec![Snippet {
                language: "lean".into(),
                prefix: "thm".into(),
                body: "theorem $1 : $2 := by\n  $0".into(),
                description: "a theorem".into(),
            }],
            warnings: Vec::new(),
        };
        assert_eq!(m, expected);
    }

    #[test]
    fn minimal_manifest_parses() {
        let m = ok(PACKAGE);
        assert_eq!(m.id, "org.example.lean");
        assert_eq!(m.name, "Lean Support");
        assert_eq!(m.version, (0, 1, 0));
        assert_eq!(m.description, "");
        assert!(m.activation.is_empty());
        assert_eq!(m.permissions, Permissions::default());
        assert!(m.languages.is_empty());
        assert!(m.snippets.is_empty());
        assert!(m.warnings.is_empty());
    }

    #[test]
    fn language_name_defaults_to_id_and_extensions_lowercase() {
        let text = format!(
            "{PACKAGE}\n[[contributions.languages]]\nid = \"lean\"\nextensions = [\"LEAN\"]\n"
        );
        let m = ok(&text);
        assert_eq!(m.languages[0].name, "lean");
        assert_eq!(m.languages[0].extensions, ["lean"]);
    }

    #[test]
    fn missing_required_fields_are_named() {
        let fields = [
            ("id", "id = \"org.example.lean\""),
            ("name", "name = \"Lean Support\""),
            ("version", "version = \"0.1.0\""),
            ("api", "api = \"1\""),
        ];
        for (key, _) in fields {
            let mut text = String::from("[package]\n");
            for (k, line) in fields {
                if k != key {
                    text.push_str(line);
                    text.push('\n');
                }
            }
            let e = errs(&text);
            assert!(has(&e, &format!("package.{key}: missing")), "{key}: {e:?}");
        }
    }

    #[test]
    fn bad_ids_are_rejected() {
        let long = format!("a.{}", "b".repeat(99));
        assert_eq!(long.len(), 101);
        let bad = ["Org.x", "x", "a..b", "-a.b", "a.b-", long.as_str()];
        for id in bad {
            let text = format!(
                "[package]\nid = \"{id}\"\nname = \"n\"\nversion = \"0.1.0\"\napi = \"1\"\n"
            );
            let e = errs(&text);
            assert!(has(&e, "package.id:"), "{id}: {e:?}");
        }
    }

    #[test]
    fn bad_versions_are_rejected() {
        for v in ["1.0", "1.0.x", "1.0.0.0"] {
            let text = format!(
                "[package]\nid = \"org.x\"\nname = \"n\"\nversion = \"{v}\"\napi = \"1\"\n"
            );
            let e = errs(&text);
            assert!(has(&e, "package.version:"), "{v}: {e:?}");
        }
    }

    #[test]
    fn unknown_api_is_rejected() {
        let text = "[package]\nid = \"org.x\"\nname = \"n\"\nversion = \"0.1.0\"\napi = \"2\"\n";
        let e = errs(text);
        assert!(has(&e, "needs api 2, this ligase has 1"), "{e:?}");
    }

    #[test]
    fn unknown_keys_are_warnings() {
        let text = format!(
            "{PACKAGE}foo = 1\n[bogus]\nx = 2\n[permissions]\nfs = true\n[contributions.themes]\nid = \"t\"\n"
        );
        let m = ok(&text);
        assert!(
            has(&m.warnings, "unknown key: package.foo"),
            "{:?}",
            m.warnings
        );
        assert!(has(&m.warnings, "unknown key: bogus"), "{:?}", m.warnings);
        assert!(
            has(&m.warnings, "unknown key: permissions.fs"),
            "{:?}",
            m.warnings
        );
        assert!(
            has(&m.warnings, "unknown key: contributions.themes"),
            "{:?}",
            m.warnings
        );
        assert_eq!(m.permissions, Permissions::default());
    }

    #[test]
    fn wrong_types_are_errors() {
        let text = format!(
            "{PACKAGE}\n[permissions]\nnetwork = \"yes\"\n[[contributions.languages]]\nid = \"lean\"\nextensions = \"lean\"\n"
        );
        let e = errs(&text);
        assert!(has(&e, "permissions.network: expected a bool"), "{e:?}");
        assert!(
            has(
                &e,
                "contributions.languages[0].extensions: expected an array"
            ),
            "{e:?}"
        );

        let text = "[package]\nid = \"org.x\"\nname = \"n\"\nversion = 1\napi = \"1\"\n";
        let e = errs(text);
        assert!(has(&e, "package.version: expected a string"), "{e:?}");
    }

    #[test]
    fn bad_events_are_errors() {
        let text = format!("{PACKAGE}\n[activation]\nevents = [\"open\", \"language:Lean\"]\n");
        let e = errs(&text);
        assert!(has(&e, "activation.events[0]: bad event"), "{e:?}");
        assert!(has(&e, "activation.events[1]: bad event"), "{e:?}");
    }

    #[test]
    fn too_many_languages_are_rejected() {
        let mut text = String::from(PACKAGE);
        for i in 0..101 {
            text.push_str(&format!("\n[[contributions.languages]]\nid = \"l{i}\"\n"));
        }
        let e = errs(&text);
        assert!(
            has(&e, "contributions.languages: at most 100 entries"),
            "{e:?}"
        );
    }

    #[test]
    fn too_many_extensions_are_rejected() {
        let exts: Vec<String> = (0..21).map(|i| format!("\"e{i}\"")).collect();
        let text = format!(
            "{PACKAGE}\n[[contributions.languages]]\nid = \"lean\"\nextensions = [{}]\n",
            exts.join(", ")
        );
        let e = errs(&text);
        assert!(
            has(
                &e,
                "contributions.languages[0].extensions: at most 20 entries"
            ),
            "{e:?}"
        );
    }

    #[test]
    fn oversized_snippet_body_is_rejected() {
        let text = format!(
            "{PACKAGE}\n[[contributions.snippets]]\nlanguage = \"lean\"\nprefix = \"x\"\nbody = \"{}\"\n",
            "x".repeat(4097)
        );
        let e = errs(&text);
        assert!(
            has(&e, "contributions.snippets[0].body: must be 1-4096 bytes"),
            "{e:?}"
        );
    }

    #[test]
    fn text_over_max_bytes_is_refused() {
        let text = " ".repeat(MAX_BYTES + 1);
        let e = errs(&text);
        assert_eq!(e.len(), 1);
        assert!(e[0].contains("bytes"), "{e:?}");
    }

    #[test]
    fn duplicate_language_ids_are_rejected() {
        let text = format!(
            "{PACKAGE}\n[[contributions.languages]]\nid = \"lean\"\n\n[[contributions.languages]]\nid = \"lean\"\n"
        );
        let e = errs(&text);
        assert!(
            has(&e, "contributions.languages[1].id: duplicate language"),
            "{e:?}"
        );
    }

    #[test]
    fn duplicate_snippet_prefixes_are_rejected() {
        let snippet =
            "\n[[contributions.snippets]]\nlanguage = \"lean\"\nprefix = \"thm\"\nbody = \"b\"\n";
        let text = format!("{PACKAGE}{snippet}{snippet}");
        let e = errs(&text);
        assert!(
            has(&e, "contributions.snippets[1].prefix: duplicate prefix"),
            "{e:?}"
        );
    }

    #[test]
    fn malformed_toml_is_an_error() {
        let e = errs("[package\nid =");
        assert!(has(&e, "toml:"), "{e:?}");
    }

    #[test]
    fn truncated_and_mutated_example_never_panics() {
        for i in 0..=EXAMPLE.len() {
            if let Some(prefix) = EXAMPLE.get(..i) {
                let _ = parse(prefix);
            }
        }
        for i in 0..EXAMPLE.len() {
            if EXAMPLE.as_bytes()[i].is_ascii() {
                let mut bytes = EXAMPLE.as_bytes().to_vec();
                bytes[i] = b'"';
                if let Ok(text) = String::from_utf8(bytes) {
                    let _ = parse(&text);
                }
            }
        }
    }
    #[test]
    fn views_take_one_feed_or_sections() {
        let view =
            "[[contributions.views]]\nid = \"g\"\ntitle = \"G\"\nplacement = \"side\"\nbody = []\n";
        let one = format!(
            "{PACKAGE}\n{view}feed = {{ request = \"$/lean/plainGoal\", field = \"rendered\" }}\n"
        );
        let m = ok(&one);
        assert_eq!(m.views[0].feeds.len(), 1);
        assert_eq!(
            m.views[0].feeds[0].source,
            Source::Request("$/lean/plainGoal".into())
        );
        let many = format!(
            "{PACKAGE}\n{view}feed = [{{ title = \"Goals\", request = \"$/lean/plainGoal\" }}, {{ title = \"Expected type\", request = \"$/lean/plainTermGoal\", field = \"goal\", code = true }}, {{ title = \"Messages\", source = \"messages\" }}]\n"
        );
        let m = ok(&many);
        let feeds = &m.views[0].feeds;
        assert_eq!(feeds.len(), 3);
        assert!(feeds[1].code);
        assert_eq!(feeds[2].source, Source::Messages);
        assert_eq!(feeds[2].title, "Messages");
        for bad in [
            "feed = { source = \"weather\" }",
            "feed = { request = \"a\", source = \"messages\" }",
            "feed = { request = \"a\", code = 1 }",
            "feed = [1]",
            "feed = 3",
        ] {
            assert!(manifest_err(&format!("{PACKAGE}\n{view}{bad}\n")), "{bad}");
        }
        let nine = ["{ source = \"messages\" }"; 9].join(", ");
        assert!(manifest_err(&format!("{PACKAGE}\n{view}feed = [{nine}]\n")));
    }

    #[test]
    fn views_read_their_tree_and_say_what_is_wrong() {
        let text = format!(
            "{PACKAGE}\n[[contributions.views]]\nid = \"goals\"\ntitle = \"Goals\"\nplacement = \"bottom\"\nbody = [{{ kind = \"heading\", text = \"Goals\" }}, {{ kind = \"button\", id = \"go\", label = \"Go\", command = \"view.problems\" }}]\n"
        );
        let m = ok(&text);
        assert_eq!(m.views.len(), 1);
        assert_eq!(m.views[0].placement, Placement::Bottom);
        assert_eq!(m.views[0].icon, "extensions");
        assert!(matches!(&m.views[0].body, Node::Column(c) if c.len() == 2));
        let bad = format!(
            "{PACKAGE}\n[[contributions.views]]\nid = \"a b\"\ntitle = \"\"\nplacement = \"left\"\nbody = [{{ kind = \"blink\" }}]\n"
        );
        let e = errs(&bad);
        for field in ["id", "title", "placement", "body"] {
            assert!(
                e.iter()
                    .any(|m| m.starts_with(&format!("contributions.views[0].{field}"))),
                "{field}: {e:?}"
            );
        }
    }

    #[test]
    fn servers_need_the_permission_and_plain_programs() {
        let server = "[[contributions.servers]]\nlanguage = \"lean4\"\ncommand = \"lake\"\nargs = [\"serve\"]\n";
        let m = ok(&format!(
            "{PACKAGE}[permissions]\nspawn_process = true\n{server}"
        ));
        assert_eq!(m.servers.len(), 1);
        assert_eq!(m.servers[0].line(), ["lake", "serve"]);
        assert_eq!(m.servers[0].language_id, "lean4");
        let e = errs(&format!("{PACKAGE}{server}"));
        assert!(has(&e, "spawn_process"), "{e:?}");
        for bad in ["../lake", "C:\\\\x\\\\lake.exe", "lake serve", ""] {
            let text = format!(
                "{PACKAGE}[permissions]\nspawn_process = true\n[[contributions.servers]]\nlanguage = \"lean4\"\ncommand = \"{bad}\"\n"
            );
            assert!(manifest_err(&text), "{bad}");
        }
        let marked = format!(
            "{PACKAGE}[permissions]
spawn_process = true
{server}roots = [\"lakefile.toml\", \"lean-toolchain\"]
"
        );
        assert_eq!(
            ok(&marked).servers[0].roots,
            ["lakefile.toml", "lean-toolchain"]
        );
        for bad in ["../lakefile.toml", "a/b", "", "x y"] {
            let text = format!(
                "{PACKAGE}[permissions]
spawn_process = true
{server}roots = [\"{bad}\"]
"
            );
            assert!(manifest_err(&text), "{bad}");
        }
    }

    fn manifest_err(text: &str) -> bool {
        parse(text).is_err()
    }
}
