//! the completion popup: items from the language server, narrowed as the
//! word under the caret grows.

pub mod view;

use resolvase::TextSnapshot;

use crate::document::ViewId;
use crate::lsp::complete::Item;

/// rows kept after a filter.
const MAX_SHOWN: usize = 200;

/// a row that matched: which item, and the lit byte offsets in its label.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Shown {
    pub item: usize,
    pub marks: Vec<usize>,
}

pub struct Popup {
    pub view: ViewId,
    pub items: Vec<Item>,
    /// where the word being completed starts.
    pub start: usize,
    pub shown: Vec<Shown>,
    pub pick: usize,
    pub top: usize,
    /// what `shown` was made for.
    typed: Option<String>,
}

/// identifier chars: what a completed word is made of.
pub fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// start of the word that ends at `at`, looking back a bounded way.
pub fn word_start(text: &TextSnapshot, at: usize) -> usize {
    let mut from = at.saturating_sub(256);
    while !text.is_char_boundary(from) {
        from += 1;
    }
    let Ok(before) = text.slice(from..at) else {
        return at;
    };
    let len: usize = before
        .chars()
        .rev()
        .take_while(|c| is_word(*c))
        .map(char::len_utf8)
        .sum();
    at - len
}

impl Popup {
    pub fn new(view: ViewId, items: Vec<Item>, start: usize) -> Self {
        Self {
            view,
            items,
            start,
            shown: Vec::new(),
            pick: 0,
            top: 0,
            typed: None,
        }
    }

    /// narrows the rows to what fits `typed`, best first.
    pub fn filter(&mut self, typed: &str) {
        if self.typed.as_deref() == Some(typed) {
            return;
        }
        let mut scored: Vec<(i32, &str, Shown)> = self
            .items
            .iter()
            .enumerate()
            .filter_map(|(item, it)| {
                if typed.is_empty() {
                    return Some((
                        0,
                        it.sort.as_str(),
                        Shown {
                            item,
                            marks: Vec::new(),
                        },
                    ));
                }
                let (score, marks) = crate::palette::score(typed, &it.filter)?;
                // marks only line up when the label is what was matched
                let marks = if it.filter == it.label {
                    marks
                } else {
                    Vec::new()
                };
                // an exact start of the word wins over a match spread out
                let bonus = if it.filter.starts_with(typed) { 100 } else { 0 };
                Some((score + bonus, it.sort.as_str(), Shown { item, marks }))
            })
            .collect();
        scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(b.1)));
        scored.truncate(MAX_SHOWN);
        self.shown = scored.into_iter().map(|(_, _, s)| s).collect();
        self.pick = 0;
        self.top = 0;
        self.typed = Some(typed.to_owned());
    }

    /// moves the pick, keeping it among `rows` rows in sight.
    pub fn step(&mut self, by: isize, rows: usize) {
        if self.shown.is_empty() {
            return;
        }
        let last = self.shown.len() - 1;
        // past either end wraps round, like vs code
        let next = self.pick as isize + by;
        self.pick = if by.abs() == 1 && next < 0 {
            last
        } else if by.abs() == 1 && next > last as isize {
            0
        } else {
            next.clamp(0, last as isize) as usize
        };
        if self.pick < self.top {
            self.top = self.pick;
        } else if self.pick >= self.top + rows {
            self.top = self.pick + 1 - rows;
        }
    }

    #[cfg(test)]
    pub fn picked(&self) -> Option<&Item> {
        self.shown.get(self.pick).map(|s| &self.items[s.item])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::Documents;
    use crate::lsp::complete::Kind;
    use resolvase::TextBuffer;
    use std::path::Path;

    fn item(label: &str, sort: &str) -> Item {
        Item {
            label: label.to_owned(),
            detail: String::new(),
            kind: Kind::Other,
            filter: label.to_owned(),
            sort: sort.to_owned(),
            insert: label.to_owned(),
            caret: label.len(),
            start: None,
        }
    }

    fn view() -> ViewId {
        let mut docs = Documents::default();
        let (doc, _) = docs.open(Path::new("x.rs"));
        docs.add_view(doc)
    }

    #[test]
    fn word_start_stops_at_marks() {
        let t = TextBuffer::from_text("a.foo_가 b").snapshot();
        assert_eq!(word_start(&t, 9), 2);
        assert_eq!(word_start(&t, 2), 2);
        assert_eq!(word_start(&t, 11), 10);
    }

    #[test]
    fn filter_ranks_prefix_then_sort_text() {
        let mut p = Popup::new(
            view(),
            vec![
                item("into_iter", "b"),
                item("iter", "c"),
                item("is_empty", "a"),
            ],
            0,
        );
        p.filter("");
        assert_eq!(
            p.picked().unwrap().label,
            "is_empty",
            "sort text when nothing typed"
        );
        p.filter("it");
        assert_eq!(p.picked().unwrap().label, "iter");
        assert_eq!(p.shown.len(), 3, "is_empty has an i and a t too");
        p.filter("zz");
        assert!(p.shown.is_empty());
    }

    #[test]
    fn step_wraps_one_at_a_time() {
        let mut p = Popup::new(
            view(),
            vec![item("a", "1"), item("b", "2"), item("c", "3")],
            0,
        );
        p.filter("");
        p.step(-1, 2);
        assert_eq!(p.pick, 2);
        assert_eq!(p.top, 1);
        p.step(1, 2);
        assert_eq!(p.pick, 0);
        p.step(10, 2);
        assert_eq!(p.pick, 2);
    }
}
