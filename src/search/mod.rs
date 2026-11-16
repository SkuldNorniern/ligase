//! find in files. a search walks the workspace on its own thread and sends
//! hits back file by file; a new search stops the old one.

pub mod view;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, channel};

use aurea::render::{CanvasId, request_canvas_redraw};

/// most hits kept; the search stops there.
pub const MAX_HITS: usize = 5000;
/// files past this are skipped.
const MAX_FILE: u64 = 4 * 1024 * 1024;
/// folders never searched.
pub const SKIP: [&str; 4] = [".git", "target", "node_modules", ".hg"];
/// longest line text kept with a hit.
const MAX_PREVIEW: usize = 200;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hit {
    pub path: PathBuf,
    /// line from 0.
    pub line: usize,
    /// byte column of the match in the line.
    pub column: usize,
    pub len: usize,
    /// the line, trimmed and cut short.
    pub preview: String,
}

enum Found {
    Hits(Vec<Hit>),
    Done { files: usize, capped: bool },
}

/// one running search.
pub struct Search {
    found: Receiver<Found>,
    stop: Arc<AtomicBool>,
}

impl Drop for Search {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

#[derive(Default)]
pub struct Finder {
    pub query: String,
    /// what replace all puts in place of each hit.
    pub replace: String,
    pub hits: Vec<Hit>,
    running: Option<Search>,
    /// files looked at and whether the hit cap was reached, once done.
    pub done: Option<(usize, bool)>,
    pub scroll: f32,
    pub selected: Option<usize>,
}

impl Finder {
    pub fn searching(&self) -> bool {
        self.running.is_some()
    }

    /// shows `hits` found some other way (a language server's references)
    /// as the panel's results, under `title`.
    pub fn show_results(&mut self, title: String, hits: Vec<Hit>) {
        self.running = None;
        let files = {
            let mut paths: Vec<&PathBuf> = hits.iter().map(|h| &h.path).collect();
            paths.sort();
            paths.dedup();
            paths.len()
        };
        self.query = title;
        self.hits = hits;
        self.done = Some((files, false));
        self.scroll = 0.0;
        self.selected = None;
    }

    /// starts a search for the current query, stopping any old one.
    pub fn start(&mut self, root: &Path, canvas: Option<CanvasId>) {
        self.running = None;
        self.hits.clear();
        self.done = None;
        self.selected = None;
        self.scroll = 0.0;
        if self.query.is_empty() {
            return;
        }
        let (tx, found) = channel();
        let stop = Arc::new(AtomicBool::new(false));
        let quit = Arc::clone(&stop);
        let root = root.to_path_buf();
        let query = self.query.clone();
        let started = std::thread::Builder::new()
            .name("ligase-search".into())
            .spawn(move || {
                let wake = || {
                    if let Some(id) = canvas {
                        request_canvas_redraw(id);
                    }
                };
                let (files, capped) = walk(&root, &query, &quit, &mut |hits| {
                    let _ = tx.send(Found::Hits(hits));
                    wake();
                });
                let _ = tx.send(Found::Done { files, capped });
                wake();
            });
        if started.is_ok() {
            self.running = Some(Search { found, stop });
        }
    }

    /// takes in what the search found since last frame.
    pub fn poll(&mut self) {
        let Some(search) = &self.running else { return };
        let mut finished = None;
        while let Ok(found) = search.found.try_recv() {
            match found {
                Found::Hits(hits) => self.hits.extend(hits),
                Found::Done { files, capped } => finished = Some((files, capped)),
            }
        }
        if finished.is_some() {
            self.done = finished;
            self.running = None;
        }
    }
}

/// smart case: a query with no capitals matches any case. gives the match
/// as (byte start, byte length) in `line`, always on char edges.
pub fn matcher(query: &str) -> impl Fn(&str) -> Option<(usize, usize)> + '_ {
    let fold = !query.chars().any(char::is_uppercase);
    let lower: Vec<char> = query.chars().flat_map(char::to_lowercase).collect();
    let ascii = query.to_ascii_lowercase();
    move |line: &str| {
        if !fold {
            return line.find(query).map(|at| (at, query.len()));
        }
        if line.is_ascii() && query.is_ascii() {
            // ascii folding keeps lengths, so offsets carry over
            let at = line.to_ascii_lowercase().find(&ascii)?;
            return Some((at, query.len()));
        }
        // fold char by char, no copies; a char may fold to more than one
        'start: for (start, _) in line.char_indices() {
            let mut want = lower.iter();
            for (i, c) in line[start..].char_indices() {
                for folded in c.to_lowercase() {
                    if want.next() != Some(&folded) {
                        continue 'start;
                    }
                }
                if want.as_slice().is_empty() {
                    return Some((start, i + c.len_utf8()));
                }
            }
        }
        None
    }
}

/// walks `root` for `query`. sends hits in batches; returns files read and
/// whether the cap stopped it.
fn walk(
    root: &Path,
    query: &str,
    stop: &AtomicBool,
    send: &mut dyn FnMut(Vec<Hit>),
) -> (usize, bool) {
    let find = matcher(query);
    let mut stack = vec![root.to_path_buf()];
    let mut files = 0;
    let mut total = 0;
    while let Some(dir) = stack.pop() {
        if stop.load(Ordering::Relaxed) {
            break;
        }
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        let mut entries: Vec<_> = entries.flatten().collect();
        entries.sort_by_key(std::fs::DirEntry::file_name);
        let mut dirs = Vec::new();
        for entry in entries {
            if stop.load(Ordering::Relaxed) {
                break;
            }
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            let name = entry.file_name();
            if kind.is_dir() {
                if !SKIP.iter().any(|s| name == *s) {
                    dirs.push(entry.path());
                }
                continue;
            }
            if !kind.is_file() || entry.metadata().is_ok_and(|m| m.len() > MAX_FILE) {
                continue;
            }
            let Ok(bytes) = std::fs::read(entry.path()) else {
                continue;
            };
            // a nul byte early on means binary
            if bytes.iter().take(8192).any(|&b| b == 0) {
                continue;
            }
            let Ok(text) = std::str::from_utf8(&bytes) else {
                continue;
            };
            files += 1;
            let mut hits = Vec::new();
            for (line, body) in text.lines().enumerate() {
                if let Some((column, len)) = find(body) {
                    let mut preview = body.trim().to_owned();
                    if preview.len() > MAX_PREVIEW {
                        let mut cut = MAX_PREVIEW;
                        while !preview.is_char_boundary(cut) {
                            cut -= 1;
                        }
                        preview.truncate(cut);
                    }
                    hits.push(Hit {
                        path: entry.path(),
                        line,
                        column,
                        len,
                        preview,
                    });
                    total += 1;
                    if total >= MAX_HITS {
                        send(hits);
                        return (files, true);
                    }
                }
            }
            if !hits.is_empty() {
                send(hits);
            }
        }
        // sorted names, popped from the back: push in reverse
        stack.extend(dirs.into_iter().rev());
    }
    (files, false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn smart_case() {
        let find = matcher("todo");
        assert_eq!(find("// TODO: x"), Some((3, 4)));
        let find = matcher("Todo");
        assert_eq!(find("// TODO: x"), None);
        assert_eq!(find("a Todo"), Some((2, 4)));
        // folding that changes byte length still lands on the real text
        let find = matcher("i");
        let line = "İx i";
        let (at, len) = find(line).unwrap();
        assert!(line.is_char_boundary(at) && line.is_char_boundary(at + len));
        assert_eq!(
            &line[at..at + len],
            "i",
            "İ folds to two chars, not a match"
        );
        assert_eq!(matcher("가나")("x 가나다"), Some((2, 6)));
        assert_eq!(matcher("abc")("xAbC"), Some((1, 3)));
    }

    #[test]
    fn walks_text_skips_binary_and_skipped_dirs() {
        let dir = std::env::temp_dir().join(format!("ligase-find-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::create_dir_all(dir.join("target")).unwrap();
        std::fs::write(dir.join("src/a.rs"), "fn main() {}\n// needle here\n").unwrap();
        std::fs::write(dir.join("target/b.rs"), "needle").unwrap();
        std::fs::write(dir.join("c.bin"), b"needle\0\0").unwrap();
        std::fs::write(dir.join("d.txt"), "가 needle\r\n").unwrap();
        let mut hits = Vec::new();
        let (files, capped) = walk(&dir, "needle", &AtomicBool::new(false), &mut |h| {
            hits.extend(h)
        });
        assert!(!capped);
        assert_eq!(files, 2);
        let names: Vec<String> = hits
            .iter()
            .map(|h| h.path.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, ["d.txt", "a.rs"]);
        assert_eq!((hits[1].line, hits[1].column), (1, 3));
        assert_eq!(hits[0].column, "가 ".len());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn stop_ends_walk() {
        let mut hits = Vec::new();
        let (files, _) = walk(Path::new("."), "x", &AtomicBool::new(true), &mut |h| {
            hits.extend(h)
        });
        assert_eq!(files, 0);
        assert!(hits.is_empty());
    }
}
