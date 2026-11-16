//! abbreviations: `\to` turns into `→` the way lean's editors do it. a
//! leader char starts one; when the next char typed can't go on any
//! abbreviation, a finished one before the caret is swapped for its text.
//! nothing is kept between keys: it looks back from the caret each time.

use std::ops::Range;

use resolvase::TextSnapshot;

/// longest key looked back for, in chars.
pub const MAX_KEY: usize = 32;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Abbrevs {
    leader: char,
    /// key and replacement, sorted by key.
    table: Vec<(String, String)>,
}

impl Abbrevs {
    /// `pairs` as (key, replacement); later ones win over earlier ones
    /// with the same key.
    pub fn new(leader: char, pairs: impl IntoIterator<Item = (String, String)>) -> Self {
        let mut table: Vec<(String, String)> = Vec::new();
        for (key, value) in pairs {
            match table.iter_mut().find(|(k, _)| *k == key) {
                Some(slot) => slot.1 = value,
                None => table.push((key, value)),
            }
        }
        table.sort_by(|a, b| a.0.cmp(&b.0));
        Self { leader, table }
    }

    fn get(&self, key: &str) -> Option<&str> {
        self.table
            .binary_search_by(|(k, _)| k.as_str().cmp(key))
            .ok()
            .map(|i| self.table[i].1.as_str())
    }

    /// true when some key starts with `prefix`.
    fn goes_on(&self, prefix: &str) -> bool {
        let at = self.table.partition_point(|(k, _)| k.as_str() < prefix);
        self.table
            .get(at)
            .is_some_and(|(k, _)| k.starts_with(prefix))
    }

    /// the leader and key right before `at` on its line: where the leader
    /// is, and the key.
    fn pending(&self, text: &TextSnapshot, at: usize) -> Option<(usize, String)> {
        let line_start = text.line_start(text.line_of(at).ok()?).ok()?;
        let from = at.saturating_sub(MAX_KEY * 4 + 4).max(line_start);
        // step to a char start
        let from = (from..at).find(|&i| text.slice(i..at).is_ok())?;
        let before = text.slice(from..at).ok()?;
        let lead = before.rfind(self.leader)?;
        let key = &before[lead + self.leader.len_utf8()..];
        if key.is_empty() || key.chars().count() > MAX_KEY || key.chars().any(char::is_whitespace) {
            return None;
        }
        Some((from + lead, key.to_owned()))
    }

    /// what typing `typed` at `at` does: when it can't go on the key before
    /// the caret and that key is whole, the leader and key give way to the
    /// replacement, `typed` after it.
    pub fn expand(
        &self,
        text: &TextSnapshot,
        at: usize,
        typed: char,
    ) -> Option<(Range<usize>, String)> {
        let (lead, key) = self.pending(text, at)?;
        let longer = format!("{key}{typed}");
        if self.goes_on(&longer) {
            return None;
        }
        let value = self.get(&key)?;
        Some((lead..at, format!("{value}{typed}")))
    }

    /// what tab does at `at`: a whole key before the caret turns into its
    /// replacement, nothing else.
    pub fn finish(&self, text: &TextSnapshot, at: usize) -> Option<(Range<usize>, String)> {
        let (lead, key) = self.pending(text, at)?;
        Some((lead..at, self.get(&key)?.to_owned()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap(s: &str) -> TextSnapshot {
        resolvase::TextBuffer::from_text(s).snapshot()
    }

    fn table() -> Abbrevs {
        Abbrevs::new(
            '\\',
            [
                ("to", "→"),
                ("top", "⊤"),
                ("a", "α"),
                ("and", "∧"),
                ("N", "ℕ"),
                ("1", "₁"),
            ]
            .into_iter()
            .map(|(k, v)| (k.to_owned(), v.to_owned())),
        )
    }

    #[test]
    fn a_whole_key_turns_over_when_the_next_char_ends_it() {
        let t = table();
        let s = snap("x \\to");
        assert_eq!(t.expand(&s, 5, ' '), Some((2..5, "→ ".to_owned())));
        assert_eq!(t.expand(&s, 5, 'p'), None, "top goes on");
        let s = snap("\\a");
        assert_eq!(t.expand(&s, 2, 'n'), None, "and goes on");
        assert_eq!(t.expand(&s, 2, ')'), Some((0..2, "α)".to_owned())));
        let s = snap("h\\1");
        assert_eq!(t.expand(&s, 3, ' '), Some((1..3, "₁ ".to_owned())));
    }

    #[test]
    fn unknown_keys_spaces_and_other_lines_stay() {
        let t = table();
        assert_eq!(t.expand(&snap("\\zz"), 3, ' '), None);
        assert_eq!(
            t.expand(&snap("\\to x"), 5, ' '),
            None,
            "a space ends the key"
        );
        assert_eq!(
            t.expand(&snap("\\to\nx"), 5, ' '),
            None,
            "the leader is on another line"
        );
        assert_eq!(t.expand(&snap("to"), 2, ' '), None, "no leader");
    }

    #[test]
    fn tab_finishes_a_whole_key() {
        let t = table();
        assert_eq!(t.finish(&snap("\\to"), 3), Some((0..3, "→".to_owned())));
        assert_eq!(t.finish(&snap("\\t"), 2), None);
        assert_eq!(t.finish(&snap("α \\N"), 5), Some((3..5, "ℕ".to_owned())));
    }

    #[test]
    fn later_pairs_win() {
        let t = Abbrevs::new('\\', [("x".into(), "1".into()), ("x".into(), "2".into())]);
        assert_eq!(t.table.len(), 1);
        assert_eq!(t.finish(&snap("\\x"), 2), Some((0..2, "2".to_owned())));
    }
}
