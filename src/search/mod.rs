//! find in files. one thread walks the workspace, skipping what
//! `.gitignore` leaves out, and hands files to a few readers; their hits come
//! back file by file, put in walk order. a new search stops the old one.

pub mod ignore;
pub mod view;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, channel, sync_channel};
use std::sync::{Arc, Mutex};

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
    /// the hits of the file walked `n`th.
    Hits(usize, Vec<Hit>),
    Done {
        files: usize,
        capped: bool,
    },
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
    /// match case and whole word, for the next search.
    pub opts: Opts,
    /// the query and options the hits were found with; none for hits from
    /// elsewhere (references), which replace must never take as text to
    /// change.
    pub searched: Option<(String, Opts)>,
    /// for each hit, the walk order of its file, so readers finishing out
    /// of turn still list in order.
    order: Vec<usize>,
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

    /// the text replace all may change: the query the finished hits came
    /// from, while the box still says it and the cap didn't cut the search
    /// short. none otherwise, so a changed box or a reference list never
    /// gets replaced.
    pub fn replaceable(&self) -> Option<(&str, Opts)> {
        let (searched, opts) = self.searched.as_ref()?;
        let complete = matches!(self.done, Some((_, false)));
        (complete
            && !self.hits.is_empty()
            && !self.searching()
            && *searched == self.query
            && *opts == self.opts)
            .then_some((searched.as_str(), *opts))
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
        self.searched = None;
        self.order = vec![0; hits.len()];
        self.hits = hits;
        self.done = Some((files, false));
        self.scroll = 0.0;
        self.selected = None;
    }

    /// starts a search for the current query, stopping any old one.
    pub fn start(&mut self, root: &Path, canvas: Option<CanvasId>) {
        self.running = None;
        self.hits.clear();
        self.order.clear();
        self.done = None;
        self.searched = (!self.query.is_empty()).then(|| (self.query.clone(), self.opts));
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
        let opts = self.opts;
        let started = std::thread::Builder::new()
            .name("ligase-search".into())
            .spawn(move || {
                let wake = || {
                    if let Some(id) = canvas {
                        request_canvas_redraw(id);
                    }
                };
                let (files, capped) = walk(&root, &query, opts, &quit, &mut |n, hits| {
                    let _ = tx.send(Found::Hits(n, hits));
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
                Found::Hits(n, hits) => {
                    let at = self.order.partition_point(|&o| o <= n);
                    self.order
                        .splice(at..at, std::iter::repeat_n(n, hits.len()));
                    self.hits.splice(at..at, hits);
                }
                Found::Done { files, capped } => finished = Some((files, capped)),
            }
        }
        if finished.is_some() {
            self.done = finished;
            self.running = None;
        }
    }
}

/// how a query matches: smart case unless `case`, anywhere unless `whole`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Opts {
    /// capitals count even in a query with none.
    pub case: bool,
    /// only where neither side touches a letter, digit or `_`.
    pub whole: bool,
}

/// a word character, for whole-word matches.
fn wordy(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// the first match of `query` in a line with `opts`: (byte start, byte
/// length), on char edges.
pub fn matcher_with(query: &str, opts: Opts) -> impl Fn(&str) -> Option<(usize, usize)> + '_ {
    let smart = matcher(query);
    move |line: &str| {
        let mut from = 0;
        loop {
            let rest = line.get(from..)?;
            let (at, len) = if opts.case {
                rest.find(query).map(|at| (at, query.len()))?
            } else {
                smart(rest)?
            };
            let (start, end) = (from + at, from + at + len);
            let before = line[..start].chars().next_back().is_some_and(wordy);
            let after = line[end..].chars().next().is_some_and(wordy);
            if !opts.whole || (!before && !after) {
                return Some((start, len));
            }
            // past this one's first char and look again
            from = start + line[start..].chars().next().map_or(1, char::len_utf8);
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

/// the files under `root` in walk order (names sorted, a folder's files
/// before its folders), leaving out [`SKIP`] and what `.gitignore` files
/// ignore, from the repository's top down. `visit` gives false to stop.
/// reads the disk: worker only.
pub fn files(root: &Path, stop: &AtomicBool, visit: &mut dyn FnMut(PathBuf) -> bool) {
    let mut stack = vec![(root.to_path_buf(), outer_rules(root))];
    while let Some((dir, rules)) = stack.pop() {
        if stop.load(Ordering::Relaxed) {
            return;
        }
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        let mut entries: Vec<_> = entries.flatten().collect();
        entries.sort_by_key(std::fs::DirEntry::file_name);
        let rules = match entries.iter().find(|e| e.file_name() == ".gitignore") {
            Some(e) => {
                ignore::Rules::read(&dir, &e.path()).map_or(rules.clone(), |r| rules.with(r))
            }
            None => rules,
        };
        let mut dirs = Vec::new();
        for entry in entries {
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            let path = entry.path();
            if kind.is_dir() {
                if !SKIP.iter().any(|s| entry.file_name() == *s) && !rules.ignored(&path, true) {
                    dirs.push(path);
                }
            } else if kind.is_file() && !rules.ignored(&path, false) && !visit(path) {
                return;
            }
        }
        // sorted names, popped from the back: push in reverse
        stack.extend(dirs.into_iter().rev().map(|d| (d, rules.clone())));
    }
}

/// most folders walked up looking for the repository's top.
const MAX_UP: usize = 64;

/// rules from above `root` that still count inside it: the repository's
/// `.git/info/exclude` and the `.gitignore` files between its top and
/// `root`. none outside a repository.
fn outer_rules(root: &Path) -> ignore::Stack {
    let Some(top) = root
        .ancestors()
        .take(MAX_UP)
        .find(|d| d.join(".git").exists())
    else {
        return ignore::Stack::default();
    };
    let mut stack = ignore::Stack::default();
    if let Some(r) = ignore::Rules::read(top, &top.join(".git/info/exclude")) {
        stack = stack.with(r);
    }
    // the root's own .gitignore is read by the walk
    // folders between the root and the repository top, the top too; none
    // when the root is the top, and never past it
    let mut above: Vec<&Path> = Vec::new();
    if top != root {
        above = root.ancestors().skip(1).take_while(|d| *d != top).collect();
        above.push(top);
    }
    for dir in above.into_iter().rev() {
        if let Some(r) = ignore::Rules::read(dir, &dir.join(".gitignore")) {
            stack = stack.with(r);
        }
    }
    stack
}

/// readers searching files at once. past about four, opening files is the
/// limit, not the cores (86k files: 7.8 s with one, 3.2 s with four).
fn readers() -> usize {
    std::thread::available_parallelism().map_or(4, |n| n.get().clamp(2, 4))
}

/// files handed to a reader at a time, so the queue lock is taken rarely.
const BATCH: usize = 64;

/// walks `root` for `query` with a few readers. sends each file's hits with
/// its place in the walk; returns files read and whether the cap stopped it.
fn walk(
    root: &Path,
    query: &str,
    opts: Opts,
    stop: &AtomicBool,
    send: &mut (dyn FnMut(usize, Vec<Hit>) + Send),
) -> (usize, bool) {
    let read = AtomicUsize::new(0);
    let total = AtomicUsize::new(0);
    let capped = AtomicBool::new(false);
    let send = Mutex::new(send);
    // a short queue: the walk stays a little ahead of the readers
    let (tx, rx) = sync_channel::<Vec<(usize, PathBuf)>>(8);
    let rx = Mutex::new(rx);
    std::thread::scope(|scope| {
        for _ in 0..readers() {
            scope.spawn(|| {
                loop {
                    let next = rx.lock().ok().and_then(|r| r.recv().ok());
                    let Some(batch) = next else { return };
                    for (n, path) in batch {
                        if stop.load(Ordering::Relaxed) || capped.load(Ordering::Relaxed) {
                            break;
                        }
                        let Some(text) = read_text(&path) else {
                            continue;
                        };
                        read.fetch_add(1, Ordering::Relaxed);
                        let mut hits: Vec<Hit> = matches(&text, query, opts)
                            .into_iter()
                            .map(|(line, column, len, body)| Hit {
                                path: path.clone(),
                                line,
                                column,
                                len,
                                preview: preview(body),
                            })
                            .collect();
                        if hits.is_empty() {
                            continue;
                        }
                        let before = total.fetch_add(hits.len(), Ordering::Relaxed);
                        if before + hits.len() >= MAX_HITS {
                            hits.truncate(MAX_HITS.saturating_sub(before));
                            capped.store(true, Ordering::Relaxed);
                        }
                        if !hits.is_empty()
                            && let Ok(mut send) = send.lock()
                        {
                            send(n, hits);
                        }
                    }
                }
            });
        }
        let mut n = 0;
        let mut batch = Vec::with_capacity(BATCH);
        files(root, stop, &mut |path| {
            if capped.load(Ordering::Relaxed) {
                return false;
            }
            batch.push((n, path));
            n += 1;
            batch.len() < BATCH || tx.send(std::mem::take(&mut batch)).is_ok()
        });
        if !batch.is_empty() {
            let _ = tx.send(batch);
        }
        drop(tx);
    });
    (read.into_inner(), capped.into_inner())
}

/// up to `max` hits for `query` under `root`, in walk order. for the
/// built-in agent's search; runs where it is called, so off the ui thread.
pub fn grep(root: &Path, query: &str, max: usize) -> Vec<Hit> {
    let stop = AtomicBool::new(false);
    let found: Mutex<Vec<(usize, Vec<Hit>)>> = Mutex::default();
    let count = AtomicUsize::new(0);
    walk(root, query, Opts::default(), &stop, &mut |n, hits| {
        let total = count.fetch_add(hits.len(), Ordering::Relaxed) + hits.len();
        if let Ok(mut f) = found.lock() {
            f.push((n, hits));
        }
        if total >= max {
            stop.store(true, Ordering::Relaxed);
        }
    });
    let mut found = found.into_inner().unwrap_or_default();
    found.sort_by_key(|f| f.0);
    found.into_iter().flat_map(|f| f.1).take(max).collect()
}

/// a file's text: none when it is too big, binary or not UTF-8.
fn read_text(path: &Path) -> Option<String> {
    use std::io::Read;
    // one open: on windows each open costs more than the read
    let file = std::fs::File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    if len > MAX_FILE {
        return None;
    }
    let mut bytes = Vec::with_capacity(usize::try_from(len).ok()?);
    file.take(MAX_FILE).read_to_end(&mut bytes).ok()?;
    // a nul byte early on means binary
    if bytes.iter().take(8192).any(|&b| b == 0) {
        return None;
    }
    String::from_utf8(bytes).ok()
}

/// the line, trimmed and cut short on a char edge.
fn preview(body: &str) -> String {
    let mut preview = body.trim().to_owned();
    if preview.len() > MAX_PREVIEW {
        let mut cut = MAX_PREVIEW;
        while !preview.is_char_boundary(cut) {
            cut -= 1;
        }
        preview.truncate(cut);
    }
    preview
}

/// the first match on each line of `text`: (line from 0, byte column, byte
/// length, the line without its ending), smart case like [`matcher`]. an
/// ascii query is found in the whole text at once, in a copy lowered once
/// for any case, not a line at a time.
fn matches<'t>(text: &'t str, query: &str, opts: Opts) -> Vec<(usize, usize, usize, &'t str)> {
    let mut out = Vec::new();
    let fold = !query.chars().any(char::is_uppercase);
    // an option on: line by line, the plain way; the fast way is for the
    // smart case search most are
    if query.is_empty() || opts != Opts::default() || (fold && !query.is_ascii()) {
        let find = matcher_with(query, opts);
        for (line, body) in text.lines().enumerate() {
            if let Some((column, len)) = find(body) {
                out.push((line, column, len, body));
            }
        }
        return out;
    }
    let lowered;
    let (hay, needle): (&str, String) = if fold {
        lowered = text.to_ascii_lowercase();
        (&lowered, query.to_ascii_lowercase())
    } else {
        (text, query.to_owned())
    };
    let bytes = text.as_bytes();
    let (mut from, mut line, mut counted) = (0, 0, 0);
    while let Some(at) = hay
        .get(from..)
        .and_then(|h| h.find(&needle))
        .map(|i| i + from)
    {
        let start = hay[..at].rfind('\n').map_or(0, |n| n + 1);
        line += bytes[counted..start]
            .iter()
            .filter(|&&b| b == b'\n')
            .count();
        counted = start;
        let end = hay[at..].find('\n').map_or(hay.len(), |n| n + at);
        // like `str::lines`: a `\r` goes with a `\n` after it, not alone at the end
        let body_end = if end > start && end < bytes.len() && bytes[end - 1] == b'\r' {
            end - 1
        } else {
            end
        };
        if at + needle.len() <= body_end {
            out.push((line, at - start, needle.len(), &text[start..body_end]));
        }
        from = end + 1;
    }
    out
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
        let mut batches = Vec::new();
        let (files, capped) = walk(
            &dir,
            "needle",
            Opts::default(),
            &AtomicBool::new(false),
            &mut |n, h| {
                batches.push((n, h));
            },
        );
        // readers finish in any order; the walk order puts them back
        batches.sort_by_key(|b| b.0);
        hits.extend(batches.into_iter().flat_map(|b| b.1));
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
    fn whole_text_matching_agrees_with_line_matching() {
        // a small generator, no crate: lines from a few pieces
        let pieces = [
            "ab", "AB", "aB", "x", " ", "\r", "가", "needle", "NeEdLe", "\t",
        ];
        let mut seed: u32 = 7;
        let mut next = || {
            seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12345);
            (seed >> 16) as usize
        };
        for _ in 0..300 {
            let mut text = String::new();
            for _ in 0..next() % 40 {
                if next() % 5 == 0 {
                    text.push(if next() % 3 == 0 { '\r' } else { '\n' });
                    if text.ends_with('\r') {
                        text.push('\n');
                    }
                } else {
                    text.push_str(pieces[next() % pieces.len()]);
                }
            }
            for query in ["ab", "AB", "needle", "x x", "b\tx", "가"] {
                let find = matcher(query);
                let want: Vec<_> = text
                    .lines()
                    .enumerate()
                    .filter_map(|(i, body)| find(body).map(|(c, l)| (i, c, l, body)))
                    .collect();
                assert_eq!(
                    matches(&text, query, Opts::default()),
                    want,
                    "{text:?} {query:?}"
                );
            }
        }
    }

    #[test]
    fn rules_above_the_repository_are_not_read() {
        let dir = std::env::temp_dir().join(format!("ligase-above-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let repo = dir.join("repo");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        // a folder over the repository ignores everything; git never reads it
        std::fs::write(
            dir.join(".gitignore"),
            "*
",
        )
        .unwrap();
        std::fs::write(repo.join("a.rs"), "x").unwrap();
        let mut seen = Vec::new();
        files(&repo, &AtomicBool::new(false), &mut |p| {
            seen.push(p.file_name().unwrap().to_string_lossy().into_owned());
            true
        });
        assert_eq!(seen, ["a.rs"]);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_walk_keeps_to_gitignore() {
        let dir = std::env::temp_dir().join(format!("ligase-ignore-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join(".git/info")).unwrap();
        std::fs::create_dir_all(dir.join("out")).unwrap();
        std::fs::create_dir_all(dir.join("src/gen")).unwrap();
        std::fs::write(dir.join(".gitignore"), "out/\n*.log\n").unwrap();
        std::fs::write(dir.join(".git/info/exclude"), "secret.txt\n").unwrap();
        std::fs::write(dir.join("src/.gitignore"), "gen/\n!keep.log\n").unwrap();
        for f in [
            "a.rs",
            "x.log",
            "secret.txt",
            "out/b.rs",
            "src/c.rs",
            "src/keep.log",
            "src/gen/d.rs",
        ] {
            std::fs::write(dir.join(f), "x").unwrap();
        }
        let mut seen = Vec::new();
        files(&dir, &AtomicBool::new(false), &mut |p| {
            seen.push(
                p.strip_prefix(&dir)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/"),
            );
            true
        });
        assert_eq!(
            seen,
            [
                ".gitignore",
                "a.rs",
                "src/.gitignore",
                "src/c.rs",
                "src/keep.log"
            ]
        );
        // a folder inside the repository still sees the rules above it
        let mut inner = Vec::new();
        files(&dir.join("src"), &AtomicBool::new(false), &mut |p| {
            inner.push(p.file_name().unwrap().to_string_lossy().into_owned());
            true
        });
        assert_eq!(inner, [".gitignore", "c.rs", "keep.log"]);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn replace_takes_only_the_searched_text() {
        let hit = Hit {
            path: PathBuf::from("a.rs"),
            line: 0,
            column: 0,
            len: 3,
            preview: String::new(),
        };
        let mut f = Finder::default();
        // references: their title is no text to replace
        f.show_results("references".to_owned(), vec![hit.clone()]);
        assert_eq!(f.replaceable(), None);
        // a finished search, then the box changed
        f.query = "old".to_owned();
        f.searched = Some(("old".to_owned(), Opts::default()));
        f.hits = vec![hit];
        f.done = Some((1, false));
        assert_eq!(f.replaceable(), Some(("old", Opts::default())));
        f.opts.whole = true;
        assert_eq!(f.replaceable(), None, "the options moved on");
        f.opts.whole = false;
        f.query = "older".to_owned();
        assert_eq!(f.replaceable(), None, "the box moved on");
        f.query = "old".to_owned();
        f.done = Some((1, true));
        assert_eq!(f.replaceable(), None, "the cap cut it short");
    }

    #[test]
    fn stop_ends_walk() {
        let mut hits = Vec::new();
        let (files, _) = walk(
            Path::new("."),
            "x",
            Opts::default(),
            &AtomicBool::new(true),
            &mut |_, h| hits.extend(h),
        );
        assert_eq!(files, 0);
        assert!(hits.is_empty());
    }
}
