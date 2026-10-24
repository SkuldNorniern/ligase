//! completion replies to plain items: what to show, what to put in, and
//! where. untrusted input: every field is optional and capped.

use std::ops::Range;

use resolvase::{Encoding, TextSnapshot};
use serde_json::Value;

/// items kept from one reply.
const MAX_ITEMS: usize = 2000;
/// longest label, detail or insert text kept.
const MAX_TEXT: usize = 400;

/// what sort of thing an item is, as a short tag and a colour group.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Function,
    Field,
    Variable,
    Type,
    Module,
    Keyword,
    Snippet,
    Constant,
    Other,
}

impl Kind {
    fn from_lsp(n: i64) -> Self {
        match n {
            2..=4 => Kind::Function,
            5 | 10 => Kind::Field,
            6 | 12 => Kind::Variable,
            7 | 8 | 13 | 22 | 25 => Kind::Type,
            9 => Kind::Module,
            14 => Kind::Keyword,
            15 => Kind::Snippet,
            20 | 21 => Kind::Constant,
            _ => Kind::Other,
        }
    }

    pub fn tag(self) -> &'static str {
        match self {
            Kind::Function => "fn",
            Kind::Field => "fd",
            Kind::Variable => "v",
            Kind::Type => "T",
            Kind::Module => "md",
            Kind::Keyword => "kw",
            Kind::Snippet => "sn",
            Kind::Constant => "c",
            Kind::Other => "·",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Item {
    pub label: String,
    pub detail: String,
    pub kind: Kind,
    /// text matched against what is typed.
    pub filter: String,
    pub sort: String,
    /// what goes in.
    pub insert: String,
    /// where the caret lands inside `insert`.
    pub caret: usize,
    /// where the replaced text starts, in the text the request was for;
    /// none means the word before the caret.
    pub start: Option<usize>,
}

fn capped(value: &Value) -> String {
    let mut s = value.as_str().unwrap_or_default().to_owned();
    if s.len() > MAX_TEXT {
        let mut cut = MAX_TEXT;
        while !s.is_char_boundary(cut) {
            cut -= 1;
        }
        s.truncate(cut);
    }
    s
}

/// a snippet to plain text: `${1:x}` keeps `x`, `$1` goes, `\$` is `$`.
/// the caret goes to the first tab stop, else the end.
pub fn plain_snippet(snippet: &str) -> (String, usize) {
    let mut out = String::with_capacity(snippet.len());
    let mut caret: Option<(u32, usize)> = None;
    let mark = |n: u32, at: usize, caret: &mut Option<(u32, usize)>| {
        // $0 is the final stop: only when there is no other
        let rank = if n == 0 { u32::MAX } else { n };
        if caret.is_none_or(|(r, _)| rank < r) {
            *caret = Some((rank, at));
        }
    };
    let mut chars = snippet.chars().peekable();
    // nested `${1:a${2:b}}` closes one level at a time
    let mut depth = 0usize;
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                if let Some(&next) = chars.peek() {
                    out.push(next);
                    chars.next();
                }
            }
            '$' => match chars.peek() {
                Some(d) if d.is_ascii_digit() => {
                    let mut n = 0u32;
                    while let Some(d) = chars.peek().and_then(|d| d.to_digit(10)) {
                        n = n.saturating_mul(10).saturating_add(d);
                        chars.next();
                    }
                    mark(n, out.len(), &mut caret);
                }
                Some('{') => {
                    chars.next();
                    let mut n = 0u32;
                    while let Some(d) = chars.peek().and_then(|d| d.to_digit(10)) {
                        n = n.saturating_mul(10).saturating_add(d);
                        chars.next();
                    }
                    mark(n, out.len(), &mut caret);
                    match chars.peek() {
                        Some(':') => {
                            chars.next();
                            depth += 1;
                        }
                        Some('|') => {
                            // a choice: keep the first one
                            chars.next();
                            for c in chars.by_ref() {
                                if c == ',' || c == '|' {
                                    break;
                                }
                                out.push(c);
                            }
                            for c in chars.by_ref() {
                                if c == '}' {
                                    break;
                                }
                            }
                        }
                        Some('}') => {
                            chars.next();
                        }
                        _ => {}
                    }
                }
                _ => out.push('$'),
            },
            '}' if depth > 0 => depth -= 1,
            c => out.push(c),
        }
    }
    let end = out.len();
    (out, caret.map_or(end, |(_, at)| at))
}

/// the items of a completion reply, positions read in `text`.
pub fn items(result: &Value, text: &TextSnapshot, encoding: Encoding) -> Vec<Item> {
    let list = match result {
        Value::Array(list) => list.as_slice(),
        Value::Object(_) => result["items"].as_array().map_or(&[][..], Vec::as_slice),
        _ => &[],
    };
    list.iter()
        .take(MAX_ITEMS)
        .filter_map(|it| item(it, text, encoding))
        .collect()
}

fn item(it: &Value, text: &TextSnapshot, encoding: Encoding) -> Option<Item> {
    let label = capped(&it["label"]);
    if label.is_empty() {
        return None;
    }
    let edit = &it["textEdit"];
    // a plain edit has a range; an insert/replace one has both, insert is
    // the one that leaves the text after the caret alone
    let range = if edit["range"].is_object() {
        &edit["range"]
    } else {
        &edit["insert"]
    };
    let start = super::range_in(text, range, encoding).map(|r: Range<usize>| r.start);
    let raw = if edit["newText"].is_string() {
        capped(&edit["newText"])
    } else if it["insertText"].is_string() {
        capped(&it["insertText"])
    } else {
        label.clone()
    };
    let (insert, caret) = if it["insertTextFormat"].as_i64() == Some(2) {
        plain_snippet(&raw)
    } else {
        let end = raw.len();
        (raw, end)
    };
    let filter = match it["filterText"].as_str() {
        Some(f) if !f.is_empty() => capped(&it["filterText"]),
        _ => label.clone(),
    };
    let sort = match it["sortText"].as_str() {
        Some(s) => capped(&Value::String(s.to_owned())),
        None => label.clone(),
    };
    Some(Item {
        detail: capped(&it["detail"]),
        kind: Kind::from_lsp(it["kind"].as_i64().unwrap_or(0)),
        filter,
        sort,
        insert,
        caret,
        start,
        label,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use resolvase::TextBuffer;
    use serde_json::json;

    #[test]
    fn snippets_become_plain_text() {
        assert_eq!(plain_snippet("foo($1)$0"), ("foo()".into(), 4));
        assert_eq!(plain_snippet("f(${1:a}, ${2:b})"), ("f(a, b)".into(), 2));
        assert_eq!(plain_snippet("x$0"), ("x".into(), 1));
        assert_eq!(plain_snippet("a\\$b"), ("a$b".into(), 3));
        assert_eq!(plain_snippet("${1|one,two|}"), ("one".into(), 0));
        assert_eq!(plain_snippet("${1:a${2:b}}c"), ("abc".into(), 0));
        assert_eq!(plain_snippet("$"), ("$".into(), 1));
    }

    #[test]
    fn reply_shapes_and_junk() {
        let text = TextBuffer::from_text("let ab").snapshot();
        let list = json!({ "items": [
            { "label": "abc", "kind": 6 },
            { "label": "abs()", "kind": 3, "insertText": "abs($1)", "insertTextFormat": 2 },
            { "label": "x", "textEdit": { "range": {
                "start": { "line": 0, "character": 4 }, "end": { "line": 0, "character": 6 } },
                "newText": "xyz" } },
            { "label": "" },
            { "kind": 3 },
            "nonsense",
        ]});
        let got = items(&list, &text, Encoding::Utf16);
        assert_eq!(got.len(), 3);
        assert_eq!(got[0].kind, Kind::Variable);
        assert_eq!((got[1].insert.as_str(), got[1].caret), ("abs()", 4));
        assert_eq!(got[2].start, Some(4));
        assert_eq!(got[2].insert, "xyz");
        assert!(items(&json!(null), &text, Encoding::Utf16).is_empty());
        let array = json!([{ "label": "z" }]);
        assert_eq!(items(&array, &text, Encoding::Utf16).len(), 1);
    }
}
