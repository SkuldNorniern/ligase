//! quick open and the command palette: one list over the workbench,
//! narrowed by a fuzzy match as you type.

pub mod view;

use std::path::Path;

/// files listed for quick open; a bigger tree is cut here.
pub const MAX_FILES: usize = 50_000;
/// rows kept after a filter.
const MAX_SHOWN: usize = 300;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Files,
    Commands,
}

/// one row: what is shown, and a note on the right like a shortcut.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Item {
    pub text: String,
    pub note: String,
}

/// a row that matched: which item, and the byte offsets of the matched
/// chars, for drawing them lit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Shown {
    pub item: usize,
    pub marks: Vec<usize>,
}

#[derive(Default)]
pub struct Palette {
    pub kind: Option<Kind>,
    pub query: String,
    pub files: Vec<Item>,
    /// files are being listed on the worker.
    pub listing: bool,
    pub commands: Vec<Item>,
    pub shown: Vec<Shown>,
    pub pick: usize,
    /// first row in sight.
    pub top: usize,
    /// what `shown` was made for.
    made_for: Option<(Kind, String, usize)>,
    /// bumped each time the file list is replaced.
    generation: usize,
    /// just opened; it slides in from here.
    pub opened: bool,
}

impl Palette {
    pub fn is_open(&self) -> bool {
        self.kind.is_some()
    }

    pub fn open(&mut self, kind: Kind) {
        if self.kind.is_none() {
            self.opened = true;
        }
        self.kind = Some(kind);
        self.query = if kind == Kind::Commands {
            ">".to_owned()
        } else {
            String::new()
        };
        self.pick = 0;
        self.top = 0;
        self.made_for = None;
    }

    pub fn close(&mut self) {
        self.kind = None;
    }

    /// a new file list; rows made from the old one are made again.
    pub fn set_files(&mut self, files: Vec<Item>) {
        self.files = files;
        self.listing = false;
        self.generation += 1;
    }

    /// the list asked for by the query: a `>` in front means commands.
    pub fn wanted(&self) -> (Kind, &str) {
        match self.query.strip_prefix('>') {
            Some(rest) => (Kind::Commands, rest.trim_start()),
            None => (Kind::Files, self.query.trim()),
        }
    }

    pub fn items(&self, kind: Kind) -> &[Item] {
        match kind {
            Kind::Files => &self.files,
            Kind::Commands => &self.commands,
        }
    }

    /// filters again when the query or the list changed.
    pub fn refresh(&mut self) {
        let (kind, query) = self.wanted();
        let key = (kind, query.to_owned(), self.generation);
        if self.made_for.as_ref() == Some(&key) {
            return;
        }
        let items = self.items(kind);
        let shown = if query.starts_with(':') {
            // ":12" is a line in the editor, not a filter
            Vec::new()
        } else if query.is_empty() {
            (0..items.len().min(MAX_SHOWN))
                .map(|item| Shown {
                    item,
                    marks: Vec::new(),
                })
                .collect()
        } else {
            let mut scored: Vec<(i32, Shown)> = items
                .iter()
                .enumerate()
                .filter_map(|(item, it)| {
                    score(query, &it.text).map(|(s, marks)| (s, Shown { item, marks }))
                })
                .collect();
            scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.item.cmp(&b.1.item)));
            scored.truncate(MAX_SHOWN);
            scored.into_iter().map(|(_, s)| s).collect()
        };
        self.shown = shown;
        self.pick = 0;
        self.top = 0;
        self.made_for = Some(key);
    }

    /// moves the pick, keeping it in sight of `rows` rows.
    pub fn step(&mut self, by: isize, rows: usize) {
        if self.shown.is_empty() {
            return;
        }
        let last = self.shown.len() - 1;
        self.pick = self.pick.saturating_add_signed(by).min(last);
        if self.pick < self.top {
            self.top = self.pick;
        } else if self.pick >= self.top + rows {
            self.top = self.pick + 1 - rows;
        }
    }

    /// the item picked now.
    pub fn picked(&self) -> Option<usize> {
        self.shown.get(self.pick).map(|s| s.item)
    }
}

fn boundary(prev: Option<char>, c: char) -> bool {
    match prev {
        None => true,
        Some(p) => {
            matches!(p, '/' | '\\' | '_' | '-' | '.' | ' ' | ':')
                || (p.is_lowercase() && c.is_uppercase())
        }
    }
}

/// fuzzy match: every query char in order, any case. higher is better:
/// runs, word starts and the file name part count most. none when it
/// doesn't match.
pub fn score(query: &str, text: &str) -> Option<(i32, Vec<usize>)> {
    let name_at = text.rfind('/').map_or(0, |i| i + 1);
    // the whole query in the file name beats a match spread over the path
    if let Some(found) = walk(query, &text[name_at..], name_at) {
        return Some((found.0 + 40, found.1));
    }
    walk(query, text, 0)
}

fn walk(query: &str, text: &str, base: usize) -> Option<(i32, Vec<usize>)> {
    let mut marks = Vec::with_capacity(query.len());
    let mut want = query.chars().flat_map(char::to_lowercase).peekable();
    let mut score = 0;
    let mut prev: Option<char> = None;
    let mut last: Option<usize> = None;
    for (i, c) in text.char_indices() {
        let Some(&w) = want.peek() else { break };
        if c.to_lowercase().eq(std::iter::once(w)) {
            want.next();
            score += 1;
            if last.is_some_and(|l| l + text[l..].chars().next().map_or(1, char::len_utf8) == i) {
                score += 8;
            }
            if boundary(prev, c) {
                score += 10;
            }
            marks.push(base + i);
            last = Some(i);
        }
        prev = Some(c);
    }
    if want.peek().is_some() {
        return None;
    }
    // shorter paths first when all else is even
    let len = i32::try_from(text.len() / 8).unwrap_or(i32::MAX);
    Some((score * 4 - len, marks))
}

/// relative paths of the files under `root`, `/` between parts, skipping
/// the folders search skips. runs on the worker.
pub fn list_files(root: &Path) -> Vec<Item> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            let path = entry.path();
            if kind.is_dir() {
                if !crate::search::SKIP.iter().any(|s| entry.file_name() == *s) {
                    stack.push(path);
                }
            } else if kind.is_file()
                && let Ok(rel) = path.strip_prefix(root)
            {
                out.push(Item {
                    text: rel.to_string_lossy().replace('\\', "/"),
                    note: String::new(),
                });
                if out.len() >= MAX_FILES {
                    return out;
                }
            }
        }
    }
    out.sort_by(|a, b| a.text.cmp(&b.text));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fuzzy_prefers_names_runs_and_word_starts() {
        assert!(score("xyz", "src/main.rs").is_none());
        let (name, marks) = score("main", "src/main.rs").unwrap();
        assert_eq!(marks, vec![4, 5, 6, 7]);
        let (spread, _) = score("main", "m/a/i/n.rs").unwrap();
        assert!(name > spread);
        let (word, _) = score("dm", "document_mod.rs").unwrap();
        let (inner, _) = score("dm", "addmod.rs").unwrap();
        assert!(word > inner, "{word} {inner}");
        assert!(score("MR", "src/main.rs").is_some(), "any case");
        assert_eq!(score("가", "a/가.rs").unwrap().1, vec![2]);
    }

    #[test]
    fn filter_sorts_and_moves_the_pick() {
        let files = ["src/app.rs", "src/editor/mod.rs", "README.md"]
            .iter()
            .map(|t| Item {
                text: (*t).to_owned(),
                note: String::new(),
            })
            .collect();
        let mut p = Palette {
            files,
            ..Palette::default()
        };
        p.open(Kind::Files);
        p.refresh();
        assert_eq!(p.shown.len(), 3);
        p.query = "edmod".into();
        p.refresh();
        assert_eq!(p.picked(), Some(1));
        p.query = "rs".into();
        p.refresh();
        p.step(5, 1);
        assert_eq!(p.pick, 1);
        assert_eq!(p.top, 1);
        p.query = ">sav".into();
        assert_eq!(p.wanted(), (Kind::Commands, "sav"));
    }

    #[test]
    fn lists_files_without_skipped_folders() {
        let dir = std::env::temp_dir().join(format!("ligase-palette-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::create_dir_all(dir.join("target")).unwrap();
        std::fs::write(dir.join("src/a.rs"), "").unwrap();
        std::fs::write(dir.join("target/b"), "").unwrap();
        std::fs::write(dir.join("c.md"), "").unwrap();
        let files: Vec<String> = list_files(&dir).into_iter().map(|i| i.text).collect();
        assert_eq!(files, ["c.md", "src/a.rs"]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
