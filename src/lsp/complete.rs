//! completion replies to plain items: what to show, what to put in, and
//! where. untrusted input: every field is optional and capped.

use std::collections::BTreeMap;
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
    /// a snippet's tab stops in `insert`, in visiting order; see [`Snippet`].
    pub stops: Vec<Vec<Range<usize>>>,
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

/// longest snippet parsed; longer input is kept as plain text.
const MAX_SNIPPET: usize = 64 * 1024;
/// distinct tab stops kept.
const MAX_STOPS: usize = 100;
/// ranges kept over all stops.
const MAX_RANGES: usize = 1000;
/// `${` nesting kept as stops; deeper ones stay literal.
const MAX_DEPTH: usize = 32;

/// a parsed snippet: the text that goes in and its tab stops.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Snippet {
    pub text: String,
    /// stops in visiting order: $1, $2, ... then $0 last. each stop lists
    /// every place it appears (mirrors), byte ranges into `text`, in text order.
    pub stops: Vec<Vec<Range<usize>>>,
}

/// a snippet to text and tab stops: `$N`, `${N:x}`, `${N|a,b|}`, variables
/// and `\` escapes. untrusted input, capped, never panics.
pub fn parse_snippet(snippet: &str) -> Snippet {
    if snippet.len() > MAX_SNIPPET {
        return Snippet {
            text: snippet.to_owned(),
            stops: Vec::new(),
        };
    }
    let chars: Vec<char> = snippet.chars().collect();
    let mut parser = Parser::default();
    parser.run(&chars);
    parser.finish()
}

/// a stop while parsing: its ranges, and the text of its first placeholder.
#[derive(Default)]
struct StopState {
    ranges: Vec<Range<usize>>,
    fill: Option<String>,
}

/// an open `${` frame: where its text starts, and its stop (with the slot of
/// its range, none when a cap dropped it). none for a variable default.
struct Open {
    start: usize,
    stop: Option<(u32, Option<usize>)>,
}

#[derive(Default)]
struct Parser {
    out: String,
    stops: BTreeMap<u32, StopState>,
    total: usize,
    open: Vec<Open>,
    /// `${` kept as text past the depth cap, waiting for its `}`.
    literal: usize,
}

impl Parser {
    fn run(&mut self, cs: &[char]) {
        let mut i = 0;
        while i < cs.len() {
            let c = cs[i];
            i += 1;
            match c {
                '\\' => match cs.get(i).copied() {
                    Some(e @ ('$' | '}' | '\\')) => {
                        self.out.push(e);
                        i += 1;
                    }
                    _ => self.out.push('\\'),
                },
                '$' => i = self.dollar(cs, i),
                '}' if self.literal > 0 => {
                    self.literal -= 1;
                    self.out.push('}');
                }
                '}' if !self.open.is_empty() => self.close(),
                c => self.out.push(c),
            }
        }
        // unclosed frames end at the input end
        while !self.open.is_empty() {
            self.close();
        }
    }

    /// `i` is just past the `$`.
    fn dollar(&mut self, cs: &[char], i: usize) -> usize {
        match cs.get(i).copied() {
            Some(d) if d.is_ascii_digit() => {
                let (n, j) = number(cs, i);
                self.stop(n);
                j
            }
            Some('{') => self.brace(cs, i + 1),
            // a bare variable: nothing goes in
            Some(c) if is_name_start(c) => name_end(cs, i),
            _ => {
                self.out.push('$');
                i
            }
        }
    }

    /// `i` is just past the `${`.
    fn brace(&mut self, cs: &[char], i: usize) -> usize {
        if self.open.len() >= MAX_DEPTH {
            return self.literal_brace(i);
        }
        match cs.get(i).copied() {
            Some(d) if d.is_ascii_digit() => {
                let (n, j) = number(cs, i);
                match cs.get(j).copied() {
                    Some(':') => {
                        self.begin(Some(n));
                        j + 1
                    }
                    Some('|') => self.choice(n, cs, j + 1),
                    Some('}') => {
                        self.stop(n);
                        j + 1
                    }
                    _ => {
                        self.stop(n);
                        j
                    }
                }
            }
            Some(c) if is_name_start(c) => {
                let j = name_end(cs, i);
                match cs.get(j).copied() {
                    Some(':') => {
                        self.begin(None);
                        j + 1
                    }
                    Some('}') => j + 1,
                    _ => j,
                }
            }
            _ => self.literal_brace(i),
        }
    }

    fn literal_brace(&mut self, i: usize) -> usize {
        self.out.push_str("${");
        self.literal += 1;
        i
    }

    /// an empty stop, or a mirror of the first placeholder of the same stop.
    fn stop(&mut self, n: u32) {
        let start = self.out.len();
        if let Some(text) = self.stops.get(&n).and_then(|s| s.fill.as_deref()) {
            self.out.push_str(text);
        }
        let end = self.out.len();
        self.record(n, start..end);
    }

    /// `${N:` placeholder, or a variable default when `n` is none.
    fn begin(&mut self, n: Option<u32>) {
        let start = self.out.len();
        let stop = n.map(|n| (n, self.record(n, start..start)));
        self.open.push(Open { start, stop });
    }

    /// `${N|a,b|}`: the first option goes in and is the stop.
    fn choice(&mut self, n: u32, cs: &[char], mut i: usize) -> usize {
        self.begin(Some(n));
        while let Some(&c) = cs.get(i) {
            match c {
                '\\' => match cs.get(i + 1).copied() {
                    Some(e @ (',' | '|' | '\\')) => {
                        self.out.push(e);
                        i += 2;
                    }
                    _ => {
                        self.out.push('\\');
                        i += 1;
                    }
                },
                ',' | '|' => break,
                c => {
                    self.out.push(c);
                    i += 1;
                }
            }
        }
        // skip the other options up to `|}`
        while let Some(&c) = cs.get(i) {
            if c == '\\' {
                i += 2;
            } else if c == '|' && cs.get(i + 1) == Some(&'}') {
                i += 2;
                break;
            } else {
                i += 1;
            }
        }
        self.close();
        i.min(cs.len())
    }

    fn close(&mut self) {
        let Some(open) = self.open.pop() else {
            return;
        };
        let end = self.out.len();
        if let Some((n, Some(slot))) = open.stop
            && let Some(state) = self.stops.get_mut(&n)
        {
            if let Some(range) = state.ranges.get_mut(slot) {
                range.end = end;
            }
            if state.fill.is_none() {
                state.fill = Some(self.out[open.start..end].to_owned());
            }
        }
    }

    /// keeps a range for stop `n` if the caps allow. returns its slot.
    fn record(&mut self, n: u32, range: Range<usize>) -> Option<usize> {
        if self.total >= MAX_RANGES {
            return None;
        }
        if self.stops.len() >= MAX_STOPS && !self.stops.contains_key(&n) {
            return None;
        }
        self.total += 1;
        let state = self.stops.entry(n).or_default();
        state.ranges.push(range);
        Some(state.ranges.len() - 1)
    }

    fn finish(self) -> Snippet {
        // BTreeMap order is ascending; $0 is moved to the end
        let mut stops = Vec::with_capacity(self.stops.len());
        let mut last = None;
        for (n, state) in self.stops {
            if n == 0 {
                last = Some(state.ranges);
            } else {
                stops.push(state.ranges);
            }
        }
        stops.extend(last);
        Snippet {
            text: self.out,
            stops,
        }
    }
}

fn number(cs: &[char], mut i: usize) -> (u32, usize) {
    let mut n = 0u32;
    while let Some(d) = cs.get(i).and_then(|c| c.to_digit(10)) {
        n = n.saturating_mul(10).saturating_add(d);
        i += 1;
    }
    (n, i)
}

fn is_name_start(c: char) -> bool {
    c == '_' || c.is_ascii_alphabetic()
}

fn name_end(cs: &[char], mut i: usize) -> usize {
    while cs
        .get(i)
        .is_some_and(|c| *c == '_' || c.is_ascii_alphanumeric())
    {
        i += 1;
    }
    i
}

impl Snippet {
    /// where the caret lands: the first tab stop, else the end.
    pub fn caret(&self) -> usize {
        self.stops
            .first()
            .and_then(|ranges| ranges.first())
            .map_or(self.text.len(), |r| r.start)
    }
}

/// a snippet to plain text and the caret.
#[cfg(test)]
fn plain_snippet(snippet: &str) -> (String, usize) {
    let parsed = parse_snippet(snippet);
    let caret = parsed.caret();
    (parsed.text, caret)
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
    let (insert, caret, stops) = if it["insertTextFormat"].as_i64() == Some(2) {
        let snippet = parse_snippet(&raw);
        let caret = snippet.caret();
        (snippet.text, caret, snippet.stops)
    } else {
        let end = raw.len();
        (raw, end, Vec::new())
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
        stops,
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

    #[test]
    fn mirrors_repeat_the_placeholder() {
        let s = parse_snippet("${1:x} = $1");
        assert_eq!(s.text, "x = x");
        assert_eq!(s.stops, vec![vec![0..1, 4..5]]);
        assert_eq!(parse_snippet("${1:x} $1").text, "x x");
        // a bare mirror before the placeholder stays empty
        let early = parse_snippet("$1 ${1:x}");
        assert_eq!(
            (early.text.as_str(), early.stops),
            (" x", vec![vec![0..0, 1..2]])
        );
    }

    #[test]
    fn nested_placeholders_both_recorded() {
        let s = parse_snippet("${1:a${2:b}} $1");
        assert_eq!(s.text, "ab ab");
        assert_eq!(s.stops, vec![vec![0..2, 3..5], vec![1..2]]);
    }

    #[test]
    fn choice_keeps_first_option() {
        let s = parse_snippet(r"x${1|a\,b,c\|d|}y");
        assert_eq!(s.text, "xa,by");
        assert_eq!(s.stops, vec![vec![1..4]]);
    }

    #[test]
    fn variables_keep_only_defaults() {
        let s = parse_snippet("a $HOME b ${USER:bob} ${1:x}");
        assert_eq!(s.text, "a  b bob x");
        assert_eq!(s.stops, vec![vec![9..10]]);
        assert_eq!(parse_snippet("${NAME:${1:z}}").stops, vec![vec![0..1]]);
    }

    #[test]
    fn escapes_follow_vscode() {
        assert_eq!(parse_snippet(r"\$\}\\\,\x").text, r"$}\\,\x");
        assert_eq!(parse_snippet(r"\{\1").text, r"\{\1");
    }

    #[test]
    fn zero_stop_goes_last() {
        let s = parse_snippet("$0 ${2:b} ${1:a} $3");
        assert_eq!(s.text, " b a ");
        assert_eq!(
            s.stops,
            vec![vec![3..4], vec![1..2], vec![5..5], vec![0..0]]
        );
        assert_eq!(parse_snippet("f($0)").stops, vec![vec![2..2]]);
    }

    #[test]
    fn unclosed_placeholder_closes_at_end() {
        let s = parse_snippet("${1:abc");
        assert_eq!((s.text.as_str(), s.stops), ("abc", vec![vec![0..3]]));
        assert_eq!(parse_snippet("${1").stops, vec![vec![0..0]]);
        assert_eq!(parse_snippet("${").text, "${");
    }

    #[test]
    fn caps_keep_text() {
        let many: String = (1..=150).map(|n| format!("${n}")).collect();
        let s = parse_snippet(&many);
        assert_eq!(s.stops.len(), 100);
        assert_eq!(s.text, "");
        let repeated = "$1".repeat(1500);
        let s = parse_snippet(&repeated);
        assert_eq!(s.stops.iter().map(Vec::len).sum::<usize>(), 1000);
        let deep = format!("{}x{}", "${1:".repeat(40), "}".repeat(40));
        assert_eq!(
            parse_snippet(&deep).text,
            format!("{}x{}", "${1:".repeat(8), "}".repeat(8))
        );
        let long = "a".repeat(MAX_SNIPPET + 1);
        let s = parse_snippet(&long);
        assert_eq!((s.text.len(), s.stops.len()), (MAX_SNIPPET + 1, 0));
    }

    #[test]
    fn fuzz_ranges_stay_in_bounds() {
        let alphabet: Vec<char> = "${}12:|,\\a가".chars().collect();
        let mut seed = 0x2545_f491_4f6c_dd1du64;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        for _ in 0..400 {
            let len = (next() % 48) as usize;
            let input: String = (0..len)
                .map(|_| alphabet[(next() % alphabet.len() as u64) as usize])
                .collect();
            let s = parse_snippet(&input);
            for r in s.stops.iter().flatten() {
                assert!(r.start <= r.end && r.end <= s.text.len(), "{input:?}");
                assert!(
                    s.text.is_char_boundary(r.start) && s.text.is_char_boundary(r.end),
                    "{input:?}"
                );
            }
            let _ = plain_snippet(&input);
        }
    }
}
