//! quick open and the command palette: one list over the workbench,
//! narrowed by a fuzzy match as you type.

pub mod view;

use std::path::Path;

/// files listed for quick open; a bigger tree is cut here.
pub const MAX_FILES: usize = 50_000;
/// rows kept after a filter.
const MAX_SHOWN: usize = 300;
/// commands remembered as used lately.
const MAX_RECENT: usize = 8;
/// how much a recent command's score is lifted when filtering.
const RECENT_LIFT: i32 = 30;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Files,
    Commands,
    /// named things in the editor's file, `@` in front.
    Symbols,
    /// a line of text asked for (a new file's name, say); no list.
    Prompt,
    /// code actions the server offers at the caret.
    Actions,
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
    /// symbols of the file in the editor, and where each one starts.
    pub symbols: Vec<Item>,
    pub symbol_at: Vec<usize>,
    /// code actions to pick from.
    pub actions: Vec<Item>,
    pub shown: Vec<Shown>,
    pub pick: usize,
    /// first row in sight.
    pub top: usize,
    /// what `shown` was made for.
    made_for: Option<(Kind, String, usize)>,
    /// bumped each time the file list is replaced.
    generation: usize,
    /// what a prompt asks for.
    pub hint: String,
    /// commands run from here lately, newest first, by their place in the list.
    recent: Vec<usize>,
}

impl Palette {
    pub fn is_open(&self) -> bool {
        self.kind.is_some()
    }

    /// opens on `kind` with an empty query. commands and symbols opened
    /// this way stay so: no `>` in the field for the first key to wipe out.
    pub fn open(&mut self, kind: Kind) {
        self.kind = Some(kind);
        self.query = String::new();
        self.pick = 0;
        self.top = 0;
        self.made_for = None;
        self.hint.clear();
    }

    /// asks for a line of text, starting as `start`, with `hint` shown
    /// while it is empty.
    pub fn prompt(&mut self, hint: String, start: String) {
        self.open(Kind::Prompt);
        self.hint = hint;
        self.query = start;
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

    /// new code actions to pick from; rows are made again.
    pub fn set_actions(&mut self, actions: Vec<Item>) {
        self.actions = actions;
        self.generation += 1;
    }

    /// new symbols for `@`; rows made from the old ones are made again.
    pub fn set_symbols(&mut self, symbols: Vec<Item>, at: Vec<usize>) {
        if self.symbols != symbols {
            self.symbols = symbols;
            self.symbol_at = at;
            self.generation += 1;
        }
    }

    /// the list asked for. opened on commands or symbols, it stays there;
    /// opened on files, `>` in front means commands and `@` the symbols of
    /// the open file.
    pub fn wanted(&self) -> (Kind, &str) {
        match self.kind {
            Some(Kind::Prompt) => return (Kind::Prompt, self.query.trim()),
            Some(Kind::Commands) => {
                let q = self.query.strip_prefix('>').unwrap_or(&self.query);
                return (Kind::Commands, q.trim());
            }
            Some(Kind::Symbols) => {
                let q = self.query.strip_prefix('@').unwrap_or(&self.query);
                return (Kind::Symbols, q.trim());
            }
            Some(Kind::Actions) => return (Kind::Actions, self.query.trim()),
            Some(Kind::Files) | None => {}
        }
        if let Some(rest) = self.query.strip_prefix('>') {
            (Kind::Commands, rest.trim_start())
        } else if let Some(rest) = self.query.strip_prefix('@') {
            (Kind::Symbols, rest.trim_start())
        } else {
            (Kind::Files, self.query.trim())
        }
    }

    pub fn items(&self, kind: Kind) -> &[Item] {
        match kind {
            Kind::Files => &self.files,
            Kind::Commands => &self.commands,
            Kind::Symbols => &self.symbols,
            Kind::Actions => &self.actions,
            Kind::Prompt => &[],
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
            // commands used lately first, then the rest in their order
            let recent: Vec<usize> = if kind == Kind::Commands {
                self.recent
                    .iter()
                    .copied()
                    .filter(|&i| i < items.len())
                    .collect()
            } else {
                Vec::new()
            };
            recent
                .iter()
                .copied()
                .chain((0..items.len()).filter(|i| !recent.contains(i)))
                .take(MAX_SHOWN)
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
                    let lift = if kind == Kind::Commands && self.recent.contains(&item) {
                        RECENT_LIFT
                    } else {
                        0
                    };
                    score(query, &it.text).map(|(s, marks)| (s + lift, Shown { item, marks }))
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

    /// command `item` was run: it comes first next time.
    pub fn used(&mut self, item: usize) {
        self.recent.retain(|&i| i != item);
        self.recent.insert(0, item);
        self.recent.truncate(MAX_RECENT);
        self.made_for = None;
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

    /// moves the rows in sight by `by`, the pick staying where it is.
    pub fn scroll(&mut self, by: isize, rows: usize) {
        let most = self.shown.len().saturating_sub(rows);
        self.top = self.top.saturating_add_signed(by).min(most);
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

/// relative paths of the files under `root`, `/` between parts, leaving
/// out what search leaves out (`.gitignore` too). runs on the worker.
pub fn list_files(root: &Path) -> Vec<Item> {
    let mut out = Vec::new();
    let never = std::sync::atomic::AtomicBool::new(false);
    crate::search::files(root, &never, &mut |path| {
        if let Ok(rel) = path.strip_prefix(root) {
            out.push(Item {
                text: rel.to_string_lossy().replace('\\', "/"),
                note: String::new(),
            });
        }
        out.len() < MAX_FILES
    });
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
        p.scroll(-9, 1);
        assert_eq!((p.pick, p.top), (1, 0), "the wheel leaves the pick");
        p.scroll(9, 1);
        assert_eq!(p.top, 1, "never past the last row");
        p.open(Kind::Commands);
        p.query = "sav".into();
        assert_eq!(
            p.wanted(),
            (Kind::Commands, "sav"),
            "opened on commands, stays"
        );
        p.query = ">sav".into();
        assert_eq!(p.wanted(), (Kind::Commands, "sav"), "a typed > is fine too");
        p.open(Kind::Files);
        p.query = ">sav".into();
        assert_eq!(p.wanted(), (Kind::Commands, "sav"));
    }

    #[test]
    fn recent_commands_come_first() {
        let commands = ["Save", "Save As", "Split Editor", "Settings"]
            .iter()
            .map(|t| Item {
                text: (*t).to_owned(),
                note: String::new(),
            })
            .collect();
        let mut p = Palette {
            commands,
            ..Palette::default()
        };
        p.used(3);
        p.used(2);
        p.open(Kind::Commands);
        p.refresh();
        let order: Vec<usize> = p.shown.iter().map(|s| s.item).collect();
        assert_eq!(order, [2, 3, 0, 1]);
        p.query = "s".into();
        p.refresh();
        assert_eq!(p.picked(), Some(2), "a recent match wins a tie");
        for i in 0..20 {
            p.used(i % 4);
        }
        assert!(p.recent.len() <= MAX_RECENT);
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
