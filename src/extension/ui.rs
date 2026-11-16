//! extension ui: a view tree an extension describes as plain data. the host
//! checks it and draws it. this file is the model, the toml reader and the
//! checks. nothing here draws. trees sent by wasm guests go through `check`.

use std::collections::HashSet;

use toml::{Table, Value};

/// most nodes in one tree.
pub const MAX_NODES: usize = 2000;
/// deepest nesting, the root counting as 1.
pub const MAX_DEPTH: usize = 16;
/// most bytes in one string.
pub const MAX_TEXT: usize = 4096;
/// most list items or table rows.
pub const MAX_ITEMS: usize = 1000;
/// most table columns.
pub const MAX_COLUMNS: usize = 12;

const MAX_ID: usize = 64;
const MAX_COMMAND: usize = 100;
const ID_RULE: &str = "1-64 bytes of a-z A-Z 0-9 _ . -";
const COMMAND_RULE: &str = "1-100 bytes of a-z A-Z 0-9 _ . : -";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Tone {
    Plain,
    Dim,
    Good,
    Warn,
    Bad,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Node {
    Column(Vec<Node>),
    Row(Vec<Node>),
    Heading(String),
    Text {
        text: String,
        tone: Tone,
    },
    /// markdown: headings, paragraphs, code blocks, rules.
    Markdown(String),
    /// a clickable button. `command` is a command id to run; with none the click comes back as an event.
    Button {
        id: String,
        label: String,
        command: Option<String>,
    },
    Toggle {
        id: String,
        label: String,
        on: bool,
    },
    /// one line of typed text; submit comes back as an event.
    Field {
        id: String,
        placeholder: String,
        value: String,
    },
    List {
        id: String,
        items: Vec<Item>,
    },
    Table {
        id: String,
        columns: Vec<String>,
        rows: Vec<Vec<String>>,
    },
    Separator,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Item {
    pub label: String,
    pub detail: String,
    pub tone: Tone,
}

/// what a user did in a view, sent back to whoever drew it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    Clicked(String),
    Toggled(String, bool),
    Submitted(String, String),
    /// list or table id, row index.
    Picked(String, usize),
}

/// reads a view from toml. a node is a table with a `kind`; a bare array is
/// a column. `path` is the prefix used in every message. warnings come back
/// with the node, errors make the read fail.
pub fn read(value: &Value, path: &str) -> Result<(Node, Vec<String>), Vec<String>> {
    let mut cx = Ctx::default();
    let node = match value {
        Value::Array(items) => {
            cx.count(path);
            Some(Node::Column(cx.read_children(items, path, 2)))
        }
        other => cx.read_node(other, path, 1),
    };
    match node {
        Some(node) if cx.errors.is_empty() => Ok((node, cx.warnings)),
        _ => {
            if cx.errors.is_empty() {
                cx.errors.push(format!("{}: not a node", at(path)));
            }
            Err(cx.errors)
        }
    }
}

/// checks a tree built in code, with the same rules `read` applies.
pub fn check(node: &Node) -> Result<(), Vec<String>> {
    let mut cx = Ctx::default();
    cx.check_node(node, "", 1);
    if cx.errors.is_empty() {
        Ok(())
    } else {
        Err(cx.errors)
    }
}

/// state for one read or check: what was found, and the ids seen so far.
#[derive(Default)]
struct Ctx {
    errors: Vec<String>,
    warnings: Vec<String>,
    nodes: usize,
    over: bool,
    ids: HashSet<String>,
}

impl Ctx {
    fn error(&mut self, path: &str, msg: &str) {
        self.errors.push(format!("{}: {msg}", at(path)));
    }

    /// counts one node. false once the tree is past MAX_NODES, and the
    /// count stops there.
    fn count(&mut self, path: &str) -> bool {
        if self.nodes < MAX_NODES {
            self.nodes += 1;
            return true;
        }
        if !self.over {
            self.over = true;
            self.error(path, &format!("more than {MAX_NODES} nodes"));
        }
        false
    }

    fn claim_id(&mut self, path: &str, id: &str) -> bool {
        if !id_ok(id) {
            self.error(path, &format!("must be {ID_RULE}"));
            false
        } else if !self.ids.insert(id.to_owned()) {
            self.error(path, &format!("duplicate id {id}"));
            false
        } else {
            true
        }
    }

    fn check_command(&mut self, path: &str, command: &str) -> bool {
        if command_ok(command) {
            true
        } else {
            self.error(path, &format!("must be {COMMAND_RULE}"));
            false
        }
    }

    fn text_check(&mut self, path: &str, s: &str, newline: bool) -> bool {
        match string_problem(s, newline) {
            Some(msg) => {
                self.error(path, &msg);
                false
            }
            None => true,
        }
    }

    fn keys(&mut self, table: &Table, path: &str, known: &[&str]) {
        for key in table.keys() {
            if !known.contains(&key.as_str()) {
                self.warnings
                    .push(format!("unknown key: {}", join(path, key)));
            }
        }
    }

    /// a string field. absent is `""` when optional. a wrong type, or a
    /// missing required field, is reported and gives None.
    fn raw<'a>(
        &mut self,
        table: &'a Table,
        path: &str,
        key: &str,
        required: bool,
    ) -> Option<&'a str> {
        match table.get(key) {
            None if required => {
                self.error(&join(path, key), "missing required field");
                None
            }
            None => Some(""),
            Some(Value::String(s)) => Some(s.as_str()),
            Some(other) => {
                self.error(
                    &join(path, key),
                    &format!("expected a string, got {}", kind_of(other)),
                );
                None
            }
        }
    }

    /// a string field that passes the text rules.
    fn text<'a>(
        &mut self,
        table: &'a Table,
        path: &str,
        key: &str,
        required: bool,
        newline: bool,
    ) -> Option<&'a str> {
        let s = self.raw(table, path, key, required)?;
        self.text_check(&join(path, key), s, newline).then_some(s)
    }

    fn id_field(&mut self, table: &Table, path: &str) -> Option<String> {
        let s = self.raw(table, path, "id", true)?;
        self.claim_id(&join(path, "id"), s).then(|| s.to_owned())
    }

    fn command_field(&mut self, table: &Table, path: &str) -> Option<String> {
        if !table.contains_key("command") {
            return None;
        }
        let s = self.raw(table, path, "command", true)?;
        self.check_command(&join(path, "command"), s)
            .then(|| s.to_owned())
    }

    fn tone(&mut self, table: &Table, path: &str) -> Option<Tone> {
        if !table.contains_key("tone") {
            return Some(Tone::Plain);
        }
        let s = self.raw(table, path, "tone", true)?;
        let tone = tone_of(s);
        if tone.is_none() {
            self.error(
                &join(path, "tone"),
                &format!("must be plain, dim, good, warn or bad, got {}", shown(s)),
            );
        }
        tone
    }

    fn boolean(&mut self, table: &Table, path: &str, key: &str) -> bool {
        match table.get(key) {
            None => false,
            Some(Value::Boolean(b)) => *b,
            Some(other) => {
                self.error(
                    &join(path, key),
                    &format!("expected a bool, got {}", kind_of(other)),
                );
                false
            }
        }
    }

    /// an array field with at most `max` entries. over the cap it is reported
    /// and treated as empty.
    fn array<'a>(&mut self, table: &'a Table, path: &str, key: &str, max: usize) -> &'a [Value] {
        match table.get(key) {
            None => &[],
            Some(Value::Array(items)) if items.len() > max => {
                self.error(
                    &join(path, key),
                    &format!("at most {max} entries, got {}", items.len()),
                );
                &[]
            }
            Some(Value::Array(items)) => items.as_slice(),
            Some(other) => {
                self.error(
                    &join(path, key),
                    &format!("expected an array, got {}", kind_of(other)),
                );
                &[]
            }
        }
    }

    fn cell(&mut self, value: &Value, path: &str) -> Option<String> {
        match value {
            Value::String(s) => self.text_check(path, s, false).then(|| s.clone()),
            other => {
                self.error(path, &format!("expected a string, got {}", kind_of(other)));
                None
            }
        }
    }

    fn read_node(&mut self, value: &Value, path: &str, depth: usize) -> Option<Node> {
        let Value::Table(table) = value else {
            self.error(path, &format!("expected a table, got {}", kind_of(value)));
            return None;
        };
        if !self.count(path) {
            return None;
        }
        let kind = self.raw(table, path, "kind", true)?;
        match kind {
            "column" => {
                self.keys(table, path, &["kind", "children"]);
                Some(Node::Column(self.children(table, path, depth)))
            }
            "row" => {
                self.keys(table, path, &["kind", "children"]);
                Some(Node::Row(self.children(table, path, depth)))
            }
            "heading" => {
                self.keys(table, path, &["kind", "text"]);
                let text = self.text(table, path, "text", true, true);
                Some(Node::Heading(text?.to_owned()))
            }
            "markdown" => {
                self.keys(table, path, &["kind", "text"]);
                let text = self.text(table, path, "text", true, true);
                Some(Node::Markdown(text?.to_owned()))
            }
            "text" => {
                self.keys(table, path, &["kind", "text", "tone"]);
                let text = self.text(table, path, "text", true, true);
                let tone = self.tone(table, path);
                Some(Node::Text {
                    text: text?.to_owned(),
                    tone: tone?,
                })
            }
            "button" => {
                self.keys(table, path, &["kind", "id", "label", "command"]);
                let id = self.id_field(table, path);
                let label = self.text(table, path, "label", true, false);
                let command = self.command_field(table, path);
                Some(Node::Button {
                    id: id?,
                    label: label?.to_owned(),
                    command,
                })
            }
            "toggle" => {
                self.keys(table, path, &["kind", "id", "label", "on"]);
                let id = self.id_field(table, path);
                let label = self.text(table, path, "label", true, false);
                let on = self.boolean(table, path, "on");
                Some(Node::Toggle {
                    id: id?,
                    label: label?.to_owned(),
                    on,
                })
            }
            "field" => {
                self.keys(table, path, &["kind", "id", "placeholder", "value"]);
                let id = self.id_field(table, path);
                let placeholder = self.text(table, path, "placeholder", false, false);
                let value = self.text(table, path, "value", false, false);
                Some(Node::Field {
                    id: id?,
                    placeholder: placeholder?.to_owned(),
                    value: value?.to_owned(),
                })
            }
            "list" => {
                self.keys(table, path, &["kind", "id", "items"]);
                let id = self.id_field(table, path);
                let items = self.items(table, path);
                Some(Node::List { id: id?, items })
            }
            "table" => {
                self.keys(table, path, &["kind", "id", "columns", "rows"]);
                let id = self.id_field(table, path);
                let (columns, rows) = self.grid(table, path);
                Some(Node::Table {
                    id: id?,
                    columns,
                    rows,
                })
            }
            "separator" => {
                self.keys(table, path, &["kind"]);
                Some(Node::Separator)
            }
            other => {
                self.error(
                    path,
                    &format!(
                        "unknown kind {}, use column, row, heading, text, button, toggle, field, list, table or separator",
                        shown(other)
                    ),
                );
                None
            }
        }
    }

    fn children(&mut self, table: &Table, path: &str, depth: usize) -> Vec<Node> {
        match table.get("children") {
            None => Vec::new(),
            Some(Value::Array(items)) => self.read_children(items, path, depth + 1),
            Some(other) => {
                self.error(
                    &join(path, "children"),
                    &format!("expected an array, got {}", kind_of(other)),
                );
                Vec::new()
            }
        }
    }

    /// children of `parent`, which sit at `depth`. stops at the first node
    /// past the node cap.
    fn read_children(&mut self, items: &[Value], parent: &str, depth: usize) -> Vec<Node> {
        let mut out = Vec::new();
        if items.is_empty() {
            return out;
        }
        if depth > MAX_DEPTH {
            self.error(
                parent,
                &format!("children nested deeper than {MAX_DEPTH} levels"),
            );
            return out;
        }
        for (i, item) in items.iter().enumerate() {
            if self.over {
                break;
            }
            let path = join(parent, &format!("children[{i}]"));
            if let Some(node) = self.read_node(item, &path, depth) {
                out.push(node);
            }
        }
        out
    }

    fn items(&mut self, table: &Table, path: &str) -> Vec<Item> {
        let mut out = Vec::new();
        for (i, value) in self
            .array(table, path, "items", MAX_ITEMS)
            .iter()
            .enumerate()
        {
            let p = join(path, &format!("items[{i}]"));
            if let Some(item) = self.item(value, &p) {
                out.push(item);
            }
        }
        out
    }

    fn item(&mut self, value: &Value, path: &str) -> Option<Item> {
        let Value::Table(table) = value else {
            self.error(path, &format!("expected a table, got {}", kind_of(value)));
            return None;
        };
        self.keys(table, path, &["label", "detail", "tone"]);
        let label = self.text(table, path, "label", true, false);
        let detail = self.text(table, path, "detail", false, false);
        let tone = self.tone(table, path);
        Some(Item {
            label: label?.to_owned(),
            detail: detail?.to_owned(),
            tone: tone?,
        })
    }

    /// columns and rows of a table node. every row must have one cell per column.
    fn grid(&mut self, table: &Table, path: &str) -> (Vec<String>, Vec<Vec<String>>) {
        let mut columns = Vec::new();
        for (i, value) in self
            .array(table, path, "columns", MAX_COLUMNS)
            .iter()
            .enumerate()
        {
            if let Some(c) = self.cell(value, &join(path, &format!("columns[{i}]"))) {
                columns.push(c);
            }
        }

        let mut rows = Vec::new();
        for (i, row) in self
            .array(table, path, "rows", MAX_ITEMS)
            .iter()
            .enumerate()
        {
            let rp = join(path, &format!("rows[{i}]"));
            let Value::Array(cells) = row else {
                self.error(&rp, &format!("expected an array, got {}", kind_of(row)));
                continue;
            };
            if cells.len() != columns.len() {
                self.error(
                    &rp,
                    &format!(
                        "has {} cells, the table has {} columns",
                        cells.len(),
                        columns.len()
                    ),
                );
                continue;
            }
            let mut out = Vec::new();
            for (j, cell) in cells.iter().enumerate() {
                if let Some(s) = self.cell(cell, &format!("{rp}[{j}]")) {
                    out.push(s);
                }
            }
            rows.push(out);
        }
        (columns, rows)
    }

    fn check_node(&mut self, node: &Node, path: &str, depth: usize) {
        if !self.count(path) {
            return;
        }
        match node {
            Node::Column(children) | Node::Row(children) => {
                self.check_children(children, path, depth + 1);
            }
            Node::Heading(text) | Node::Text { text, .. } | Node::Markdown(text) => {
                self.text_check(&join(path, "text"), text, true);
            }
            Node::Button { id, label, command } => {
                self.claim_id(&join(path, "id"), id);
                self.text_check(&join(path, "label"), label, false);
                if let Some(c) = command {
                    self.check_command(&join(path, "command"), c);
                }
            }
            Node::Toggle { id, label, .. } => {
                self.claim_id(&join(path, "id"), id);
                self.text_check(&join(path, "label"), label, false);
            }
            Node::Field {
                id,
                placeholder,
                value,
            } => {
                self.claim_id(&join(path, "id"), id);
                self.text_check(&join(path, "placeholder"), placeholder, false);
                self.text_check(&join(path, "value"), value, false);
            }
            Node::List { id, items } => {
                self.claim_id(&join(path, "id"), id);
                if items.len() > MAX_ITEMS {
                    self.error(
                        &join(path, "items"),
                        &format!("at most {MAX_ITEMS} entries, got {}", items.len()),
                    );
                }
                for (i, item) in items.iter().enumerate() {
                    let p = join(path, &format!("items[{i}]"));
                    self.text_check(&join(&p, "label"), &item.label, false);
                    self.text_check(&join(&p, "detail"), &item.detail, false);
                }
            }
            Node::Table { id, columns, rows } => {
                self.claim_id(&join(path, "id"), id);
                if columns.len() > MAX_COLUMNS {
                    self.error(
                        &join(path, "columns"),
                        &format!("at most {MAX_COLUMNS} entries, got {}", columns.len()),
                    );
                }
                for (i, c) in columns.iter().enumerate() {
                    self.text_check(&join(path, &format!("columns[{i}]")), c, false);
                }
                if rows.len() > MAX_ITEMS {
                    self.error(
                        &join(path, "rows"),
                        &format!("at most {MAX_ITEMS} entries, got {}", rows.len()),
                    );
                }
                for (i, row) in rows.iter().enumerate() {
                    let rp = join(path, &format!("rows[{i}]"));
                    if row.len() != columns.len() {
                        self.error(
                            &rp,
                            &format!(
                                "has {} cells, the table has {} columns",
                                row.len(),
                                columns.len()
                            ),
                        );
                        continue;
                    }
                    for (j, cell) in row.iter().enumerate() {
                        self.text_check(&format!("{rp}[{j}]"), cell, false);
                    }
                }
            }
            Node::Separator => {}
        }
    }

    fn check_children(&mut self, nodes: &[Node], parent: &str, depth: usize) {
        if nodes.is_empty() {
            return;
        }
        if depth > MAX_DEPTH {
            self.error(
                parent,
                &format!("children nested deeper than {MAX_DEPTH} levels"),
            );
            return;
        }
        for (i, node) in nodes.iter().enumerate() {
            if self.over {
                break;
            }
            self.check_node(node, &join(parent, &format!("children[{i}]")), depth);
        }
    }
}

/// `path.key`, or just `key` at the root.
fn join(path: &str, key: &str) -> String {
    if path.is_empty() {
        key.to_owned()
    } else {
        format!("{path}.{key}")
    }
}

/// the path shown in a message. the root has none, so it is named.
fn at(path: &str) -> &str {
    if path.is_empty() { "root" } else { path }
}

/// a value echoed back in a message. cut short and escaped, since it came from outside.
fn shown(s: &str) -> String {
    s.chars().take(40).flat_map(char::escape_debug).collect()
}

fn kind_of(value: &Value) -> &'static str {
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

fn tone_of(s: &str) -> Option<Tone> {
    Some(match s {
        "plain" => Tone::Plain,
        "dim" => Tone::Dim,
        "good" => Tone::Good,
        "warn" => Tone::Warn,
        "bad" => Tone::Bad,
        _ => return None,
    })
}

/// 1-64 bytes of a-z A-Z 0-9 _ . -
fn id_ok(s: &str) -> bool {
    (1..=MAX_ID).contains(&s.len())
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-'))
}

/// 1-100 bytes of a-z A-Z 0-9 _ . : -
fn command_ok(s: &str) -> bool {
    (1..=MAX_COMMAND).contains(&s.len())
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b':' | b'-'))
}

/// at most MAX_TEXT bytes, no control chars. `newline` lets '\n' through.
fn string_problem(s: &str, newline: bool) -> Option<String> {
    if s.len() > MAX_TEXT {
        return Some(format!("is {} bytes, the limit is {MAX_TEXT}", s.len()));
    }
    if s.chars().any(|c| c.is_control() && !(newline && c == '\n')) {
        return Some("has a control char".to_owned());
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXAMPLE: &str = r#"kind = "column"
children = [
{ kind = "heading", text = "Goals" },
{ kind = "text", text = "no goals", tone = "dim" },
{ kind = "text", text = "line one\nline two" },
{ kind = "row", children = [{ kind = "button", id = "refresh", label = "Refresh", command = "view.problems" }, { kind = "button", id = "open-log", label = "Open log" }] },
{ kind = "toggle", id = "auto", label = "Auto refresh", on = true },
{ kind = "field", id = "q", placeholder = "search", value = "" },
{ kind = "list", id = "goals", items = [{ label = "x", detail = "y", tone = "warn" }, { label = "z" }] },
{ kind = "table", id = "t", columns = ["a", "b"], rows = [["1", "2"]] },
{ kind = "separator" },
]
"#;

    fn parse(text: &str) -> Value {
        Value::Table(toml::from_str(text).expect("test toml parses"))
    }

    /// a single node written as a toml value, e.g. `{ kind = "separator" }`.
    fn node_value(text: &str) -> Value {
        let mut table: Table = toml::from_str(&format!("n = {text}")).expect("test toml parses");
        table.remove("n").expect("test key is there")
    }

    fn ok(value: &Value, path: &str) -> (Node, Vec<String>) {
        match read(value, path) {
            Ok(v) => v,
            Err(e) => panic!("expected ok, got {e:?}"),
        }
    }

    fn errs(value: &Value, path: &str) -> Vec<String> {
        match read(value, path) {
            Ok(v) => panic!("expected errors, got {v:?}"),
            Err(e) => e,
        }
    }

    fn has(list: &[String], needle: &str) -> bool {
        list.iter().any(|m| m.contains(needle))
    }

    fn separators(n: usize) -> Value {
        let cells = vec!["{ kind = \"separator\" }"; n].join(",");
        node_value(&format!("{{ kind = \"column\", children = [{cells}] }}"))
    }

    fn nested(levels: usize) -> Value {
        let mut text = String::from("{ kind = \"separator\" }");
        for _ in 0..levels {
            text = format!("{{ kind = \"column\", children = [{text}] }}");
        }
        node_value(&text)
    }

    fn nested_node(levels: usize) -> Node {
        let mut node = Node::Separator;
        for _ in 0..levels {
            node = Node::Column(vec![node]);
        }
        node
    }

    #[test]
    fn full_example_reads_to_the_expected_tree() {
        let (node, warnings) = ok(&parse(EXAMPLE), "");
        let expected = Node::Column(vec![
            Node::Heading("Goals".into()),
            Node::Text {
                text: "no goals".into(),
                tone: Tone::Dim,
            },
            Node::Text {
                text: "line one\nline two".into(),
                tone: Tone::Plain,
            },
            Node::Row(vec![
                Node::Button {
                    id: "refresh".into(),
                    label: "Refresh".into(),
                    command: Some("view.problems".into()),
                },
                Node::Button {
                    id: "open-log".into(),
                    label: "Open log".into(),
                    command: None,
                },
            ]),
            Node::Toggle {
                id: "auto".into(),
                label: "Auto refresh".into(),
                on: true,
            },
            Node::Field {
                id: "q".into(),
                placeholder: "search".into(),
                value: String::new(),
            },
            Node::List {
                id: "goals".into(),
                items: vec![
                    Item {
                        label: "x".into(),
                        detail: "y".into(),
                        tone: Tone::Warn,
                    },
                    Item {
                        label: "z".into(),
                        detail: String::new(),
                        tone: Tone::Plain,
                    },
                ],
            },
            Node::Table {
                id: "t".into(),
                columns: vec!["a".into(), "b".into()],
                rows: vec![vec!["1".into(), "2".into()]],
            },
            Node::Separator,
        ]);
        assert_eq!(node, expected);
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(check(&node), Ok(()));
    }

    #[test]
    fn bare_array_is_a_column() {
        let value = node_value(r#"[{ kind = "separator" }]"#);
        let (node, _) = ok(&value, "");
        assert_eq!(node, Node::Column(vec![Node::Separator]));
    }

    #[test]
    fn unknown_kind_is_an_error() {
        let e = errs(&node_value(r#"{ kind = "gauge" }"#), "views[0]");
        assert!(has(&e, "views[0]: unknown kind"), "{e:?}");
    }

    #[test]
    fn unknown_key_is_a_warning_with_the_prefix() {
        let value = node_value(r#"{ kind = "separator", colour = "red" }"#);
        let (node, warnings) = ok(&value, "views[0]");
        assert_eq!(node, Node::Separator);
        assert_eq!(warnings, ["unknown key: views[0].colour"]);
    }

    #[test]
    fn duplicate_ids_are_an_error() {
        let value = node_value(
            r#"[{ kind = "button", id = "a", label = "x" }, { kind = "toggle", id = "a", label = "y" }]"#,
        );
        let e = errs(&value, "");
        assert!(has(&e, "children[1].id: duplicate id a"), "{e:?}");
    }

    #[test]
    fn bad_id_charset_is_an_error() {
        let long = "x".repeat(65);
        for id in ["", "a b", "a/b", long.as_str()] {
            let value = node_value(&format!(
                r#"{{ kind = "toggle", id = "{id}", label = "x" }}"#
            ));
            let e = errs(&value, "");
            assert!(has(&e, "id: must be 1-64 bytes"), "{id:?}: {e:?}");
        }
    }

    #[test]
    fn table_row_with_wrong_cell_count_is_an_error() {
        let value =
            node_value(r#"{ kind = "table", id = "t", columns = ["a", "b"], rows = [["1"]] }"#);
        let e = errs(&value, "");
        assert!(
            has(&e, "rows[0]: has 1 cells, the table has 2 columns"),
            "{e:?}"
        );
    }

    #[test]
    fn node_count_is_capped() {
        let (node, _) = ok(&separators(MAX_NODES - 1), "");
        assert_eq!(check(&node), Ok(()));

        let e = errs(&separators(MAX_NODES), "");
        assert!(has(&e, "more than 2000 nodes"), "{e:?}");
        assert_eq!(e.len(), 1, "{e:?}");

        let big = Node::Column(vec![Node::Separator; MAX_NODES]);
        let e = check(&big).expect_err("too many nodes");
        assert!(has(&e, "more than 2000 nodes"), "{e:?}");
    }

    #[test]
    fn depth_is_capped() {
        let (node, _) = ok(&nested(MAX_DEPTH - 1), "");
        assert_eq!(check(&node), Ok(()));
        assert_eq!(check(&nested_node(MAX_DEPTH - 1)), Ok(()));

        let e = errs(&nested(MAX_DEPTH), "");
        assert!(has(&e, "nested deeper than 16"), "{e:?}");

        let e = check(&nested_node(MAX_DEPTH)).expect_err("too deep");
        assert!(has(&e, "nested deeper than 16"), "{e:?}");
    }

    #[test]
    fn very_deep_value_is_refused_without_overflow() {
        let handle = std::thread::Builder::new()
            .stack_size(64 * 1024 * 1024)
            .spawn(|| {
                let mut table = Table::new();
                table.insert("kind".to_owned(), Value::String("separator".to_owned()));
                let mut value = Value::Table(table);
                for _ in 0..10_000 {
                    let mut column = Table::new();
                    column.insert("kind".to_owned(), Value::String("column".to_owned()));
                    column.insert("children".to_owned(), Value::Array(vec![value]));
                    value = Value::Table(column);
                }
                read(&value, "")
            })
            .expect("thread starts");
        let result = handle.join().expect("no panic");
        let e = result.expect_err("too deep");
        assert!(has(&e, "nested deeper than 16"), "{e:?}");
    }

    #[test]
    fn long_text_is_an_error() {
        let long = "x".repeat(MAX_TEXT + 1);
        let e = errs(
            &node_value(&format!(r#"{{ kind = "heading", text = "{long}" }}"#)),
            "",
        );
        assert!(has(&e, "text: is 4097 bytes, the limit is 4096"), "{e:?}");

        let max = "x".repeat(MAX_TEXT);
        let value = node_value(&format!(r#"{{ kind = "heading", text = "{max}" }}"#));
        assert!(read(&value, "").is_ok());
    }

    #[test]
    fn control_chars_are_an_error_except_newline_in_text() {
        let e = errs(
            &node_value(r#"{ kind = "toggle", id = "a", label = "x\ty" }"#),
            "",
        );
        assert!(has(&e, "label: has a control char"), "{e:?}");

        let e = errs(&node_value(r#"{ kind = "heading", text = "x\ty" }"#), "");
        assert!(has(&e, "text: has a control char"), "{e:?}");

        assert!(read(&node_value(r#"{ kind = "heading", text = "x\ny" }"#), "").is_ok());
    }

    #[test]
    fn check_finds_duplicate_ids_in_a_hand_built_tree() {
        let bad = Node::Column(vec![
            Node::Button {
                id: "a".into(),
                label: "x".into(),
                command: None,
            },
            Node::Toggle {
                id: "a".into(),
                label: "y".into(),
                on: false,
            },
        ]);
        let e = check(&bad).expect_err("duplicate id");
        assert!(has(&e, "children[1].id: duplicate id a"), "{e:?}");
    }

    #[test]
    fn truncated_example_never_panics() {
        for i in 0..=EXAMPLE.len() {
            if !EXAMPLE.is_char_boundary(i) {
                continue;
            }
            if let Ok(value) = toml::from_str::<Value>(&EXAMPLE[..i]) {
                let _ = read(&value, "");
            }
        }
    }
}
