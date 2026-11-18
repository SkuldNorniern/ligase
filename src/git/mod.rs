//! what git knows about a file: its text at HEAD, the lines changed since,
//! and the branch. git runs as a program on the file worker; no repository
//! is ever read on the ui thread.

pub mod blame;
pub mod diff;
pub mod graph;
pub mod view;

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::io::Read;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use resolvase::Revision;

/// files past this get no change marks.
const MAX_BASE: usize = 8 * 1024 * 1024;
/// edits past this many in one diff mark the whole changed middle instead.
const MAX_EDITS: usize = 1000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Added,
    Modified,
    /// lines went before this line; the range is empty.
    Deleted,
}

/// lines of the text now that differ from HEAD.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hunk {
    pub lines: Range<usize>,
    pub kind: Kind,
}

/// git's side of one document.
#[derive(Default)]
pub struct State {
    /// the file at HEAD; none when it is not in a repository or not tracked.
    pub base: Option<Arc<str>>,
    pub hunks: Vec<Hunk>,
    /// the revision last sent off for a diff, and whether it is still out.
    pub asked: Option<Revision>,
    pub pending: bool,
}

/// fetch, pull and push past this long are killed.
const SYNC_LIMIT: Duration = Duration::from_secs(120);

/// the git command for `args` in `dir`, no window, no prompts: a credential
/// question fails instead of waiting on a terminal nobody sees.
fn command(dir: &Path, args: &[&str]) -> Command {
    let mut cmd = Command::new("git");
    cmd.arg("--literal-pathspecs")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

/// runs git in `dir`; its output, or what it said went wrong. pathspecs are
/// taken literally, so a file named `*` is only that file.
fn run(dir: &Path, args: &[&str]) -> Result<Vec<u8>, String> {
    let out = command(dir, args)
        .output()
        .map_err(|e| format!("git: {e}"))?;
    if out.status.success() {
        return Ok(out.stdout);
    }
    Err(refusal(&out.stdout, &out.stderr))
}

/// what git said went wrong: its first line, from stderr or else stdout
/// (some refusals, "nothing added to commit", come on stdout).
fn refusal(stdout: &[u8], stderr: &[u8]) -> String {
    let first = |bytes: &[u8]| {
        String::from_utf8_lossy(bytes)
            .lines()
            .map(str::trim)
            .find(|l| !l.is_empty())
            .map(str::to_owned)
    };
    first(stderr)
        .or_else(|| first(stdout))
        .unwrap_or_else(|| "git failed".to_owned())
}

/// reads a pipe to its end on its own thread, so a full pipe never stalls git.
fn drain<R: Read + Send + 'static>(pipe: Option<R>) -> std::thread::JoinHandle<Vec<u8>> {
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        if let Some(mut pipe) = pipe {
            let _ = pipe.read_to_end(&mut bytes);
        }
        bytes
    })
}

/// like `run`, for the network commands: killed past `SYNC_LIMIT`. the
/// answer is git's first line, or nothing, when it worked.
fn run_limited(dir: &Path, args: &[&str]) -> Result<String, String> {
    let mut child: Child = command(dir, args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("git: {e}"))?;
    let stdout = drain(child.stdout.take());
    let stderr = drain(child.stderr.take());
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() < SYNC_LIMIT => {
                std::thread::sleep(Duration::from_millis(20));
            }
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("git took too long".to_owned());
            }
            Err(e) => {
                let _ = child.kill();
                return Err(format!("git: {e}"));
            }
        }
    };
    let stdout = stdout.join().unwrap_or_default();
    let stderr = stderr.join().unwrap_or_default();
    if !status.success() {
        return Err(refusal(&stdout, &stderr));
    }
    // progress and summaries come on either stream, depending on the command
    let line = |bytes: &[u8]| {
        String::from_utf8_lossy(bytes)
            .lines()
            .map(str::trim)
            .find(|l| !l.is_empty())
            .map(str::to_owned)
    };
    Ok(line(&stderr).or_else(|| line(&stdout)).unwrap_or_default())
}

fn git(dir: &Path, args: &[&str]) -> Option<Vec<u8>> {
    run(dir, args).ok()
}

/// one changed file: where it is, and git's two letters for it, the index
/// (staged) one and the working tree one, `?` for untracked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Change {
    pub path: PathBuf,
    /// from the repository's top, `/` between parts, as git takes it back.
    pub rel: String,
    /// where a rename or copy came from, the same way.
    pub old: Option<String>,
    pub index: char,
    pub tree: char,
}

impl Change {
    pub fn staged(&self) -> bool {
        !matches!(self.index, ' ' | '?')
    }

    pub fn unstaged(&self) -> bool {
        self.tree != ' '
    }
}

/// the repository's changed files.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Status {
    pub top: PathBuf,
    pub changes: Vec<Change>,
    /// recent history for the graph, and its lanes; empty until asked.
    pub history: Vec<graph::Commit>,
    pub rows: Vec<graph::Row>,
    /// the branch and its counts against the upstream; none until asked.
    pub tracking: Option<Tracking>,
}

/// one repository in the workspace, as the panel lists it. branch and counts
/// are empty for the ones not read (see the app's status refresh).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RepoSummary {
    pub path: PathBuf,
    /// changed files in it.
    pub changes: usize,
    pub branch: String,
    pub ahead: usize,
    pub behind: usize,
}

/// most repositories found under a workspace.
pub const MAX_REPOS: usize = 32;
/// how deep below the workspace repositories are looked for.
const REPO_DEPTH: usize = 3;
/// most folders looked in while finding them.
const MAX_LOOKED: usize = 4000;

/// the repositories of a workspace at `root`: the one it is in, if any,
/// then ones in folders below it, a few levels down, in name order. runs
/// on the worker.
pub fn repos(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(top) = git(root, &["rev-parse", "--show-toplevel"])
        .and_then(|t| String::from_utf8(t).ok())
        .map(|t| PathBuf::from(t.trim()))
    {
        out.push(top);
    }
    let own = out.len();
    let mut stack = vec![(root.to_path_buf(), 0usize)];
    let mut looked = 0;
    while let Some((dir, depth)) = stack.pop() {
        looked += 1;
        if looked > MAX_LOOKED || out.len() >= MAX_REPOS {
            break;
        }
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        let mut dirs: Vec<PathBuf> = entries
            .flatten()
            .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
            .filter(|e| {
                let name = e.file_name();
                let name = name.to_string_lossy();
                !name.starts_with('.') && !crate::search::SKIP.iter().any(|s| name == *s)
            })
            .map(|e| e.path())
            .collect();
        dirs.sort();
        for d in dirs.into_iter().rev() {
            if d.join(".git").exists() && !out.iter().any(|o| same(o, &d)) {
                out.push(d.clone());
            }
            if depth + 1 < REPO_DEPTH {
                stack.push((d, depth + 1));
            }
        }
    }
    out.truncate(MAX_REPOS);
    // the workspace's own first, the rest by path
    out[own..].sort();
    out
}

/// two paths name one folder, whatever their case and separators.
fn same(a: &Path, b: &Path) -> bool {
    let norm = |p: &Path| p.to_string_lossy().replace('\\', "/").to_lowercase();
    norm(a) == norm(b)
}

/// changes past this many are not listed.
const MAX_CHANGES: usize = 5000;

/// what changed in the repository `dir` is in; none outside one.
pub fn status(dir: &Path) -> Option<Status> {
    let top = git(dir, &["rev-parse", "--show-toplevel"])?;
    let top = PathBuf::from(String::from_utf8(top).ok()?.trim());
    let out = git(
        &top,
        &["status", "--porcelain=v1", "-z", "--untracked-files=all"],
    )?;
    Some(Status {
        changes: parse_status(&top, &out),
        top,
        history: Vec::new(),
        rows: Vec::new(),
        tracking: None,
    })
}

fn parse_status(top: &Path, out: &[u8]) -> Vec<Change> {
    let mut changes = Vec::new();
    let mut fields = out.split(|b| *b == 0).filter(|f| !f.is_empty());
    while let Some(field) = fields.next() {
        let text = String::from_utf8_lossy(field);
        let mut chars = text.chars();
        let (Some(index), Some(tree), Some(' ')) = (chars.next(), chars.next(), chars.next())
        else {
            continue;
        };
        let rel = chars.as_str().to_owned();
        // a rename or copy names the old path next; the new one is the file
        let old = (matches!(index, 'R' | 'C') || matches!(tree, 'R' | 'C'))
            .then(|| {
                fields
                    .next()
                    .map(|f| String::from_utf8_lossy(f).into_owned())
            })
            .flatten();
        changes.push(Change {
            path: top.join(&rel),
            rel,
            old,
            index,
            tree,
        });
        if changes.len() >= MAX_CHANGES {
            break;
        }
    }
    changes.sort_by(|a, b| a.rel.cmp(&b.rel));
    changes
}

/// stages `rels` in the repository at `top`.
pub fn stage(top: &Path, rels: &[String]) -> Result<(), String> {
    let mut args = vec!["add", "--"];
    args.extend(rels.iter().map(String::as_str));
    run(top, &args).map(drop)
}

/// takes `rels` out of the index, back to what HEAD has. a rename takes both
/// its paths, or the old one stays staged as deleted.
pub fn unstage(top: &Path, rels: &[String]) -> Result<(), String> {
    let mut args = vec!["restore", "--staged", "--"];
    args.extend(rels.iter().map(String::as_str));
    run(top, &args).map(drop)
}

/// puts `rels` in the working tree back to what the index has, edits gone.
pub fn discard(top: &Path, rels: &[String]) -> Result<(), String> {
    let mut args = vec!["restore", "--worktree", "--"];
    args.extend(rels.iter().map(String::as_str));
    run(top, &args).map(drop)
}

/// commits what is staged with `message`.
pub fn commit(top: &Path, message: &str) -> Result<(), String> {
    run(top, &["commit", "--quiet", "-m", message]).map(drop)
}

/// the branch checked out and how far it is from its upstream.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tracking {
    /// the branch's name; the short commit when detached.
    pub branch: String,
    /// the remote branch it follows, as `origin/main`; none when it has none.
    pub upstream: Option<String>,
    /// commits here that the upstream doesn't have.
    pub ahead: usize,
    /// commits upstream that aren't here.
    pub behind: usize,
}

/// the branch under `top` and its counts against the upstream. none when
/// git isn't there or there is no commit yet.
pub fn tracking(top: &Path) -> Option<Tracking> {
    let branch = match current_branch(top) {
        Some(name) => name,
        None => git(top, &["rev-parse", "--short", "HEAD"]).and_then(one_line)?,
    };
    let up = git(
        top,
        &[
            "rev-parse",
            "--abbrev-ref",
            "--symbolic-full-name",
            "@{upstream}",
        ],
    )
    .and_then(one_line);
    let counts = up
        .as_ref()
        .and_then(|_| {
            git(
                top,
                &["rev-list", "--left-right", "--count", "HEAD...@{upstream}"],
            )
        })
        .and_then(|bytes| String::from_utf8(bytes).ok())
        .and_then(|text| parse_counts(&text));
    let (upstream, ahead, behind) = match counts {
        Some((ahead, behind)) => (up, ahead, behind),
        // no upstream, or one git can't count against: nothing to pull or push
        None => (None, 0, 0),
    };
    Some(Tracking {
        branch,
        upstream,
        ahead,
        behind,
    })
}

/// `rev-list --left-right --count` output: the left count, then the right,
/// tab apart. left is ahead here, right is behind.
fn parse_counts(text: &str) -> Option<(usize, usize)> {
    let mut parts = text.split_whitespace();
    let ahead = parts.next()?.parse().ok()?;
    let behind = parts.next()?.parse().ok()?;
    parts.next().is_none().then_some((ahead, behind))
}

/// the checked out branch's name; none when detached.
fn current_branch(top: &Path) -> Option<String> {
    git(top, &["symbolic-ref", "--short", "-q", "HEAD"]).and_then(one_line)
}

/// the first non-empty line of git's output, trimmed.
fn one_line(bytes: Vec<u8>) -> Option<String> {
    let text = String::from_utf8(bytes).ok()?;
    let line = text.lines().map(str::trim).find(|l| !l.is_empty())?;
    Some(line.to_owned())
}

fn has_origin(top: &Path) -> bool {
    git(top, &["remote"])
        .and_then(|bytes| String::from_utf8(bytes).ok())
        .is_some_and(|text| text.lines().any(|l| l.trim() == "origin"))
}

/// fetches every remote, dropping the branches they deleted.
pub fn fetch(top: &Path) -> Result<String, String> {
    run_limited(top, &["fetch", "--prune"])
}

/// fast-forwards the current branch to its upstream. never a merge commit
/// or a rebase: a branch that has diverged is refused.
pub fn pull(top: &Path) -> Result<String, String> {
    run_limited(top, &["pull", "--ff-only"])
}

/// pushes the current branch. with no upstream yet, sets `origin` as it
/// when there is an origin remote.
pub fn push(top: &Path) -> Result<String, String> {
    let has_upstream = tracking(top).is_some_and(|t| t.upstream.is_some());
    if has_upstream {
        return run_limited(top, &["push"]);
    }
    match current_branch(top) {
        Some(branch) if has_origin(top) => {
            run_limited(top, &["push", "-u", "origin", branch.as_str()])
        }
        _ => Err("No upstream branch".to_owned()),
    }
}

/// the local branches, in git's order. a list past `MAX_BRANCHES` is cut.
pub fn branches(top: &Path) -> Result<Vec<String>, String> {
    let out = run(top, &["branch", "--format=%(refname:short)"])?;
    Ok(String::from_utf8_lossy(&out)
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .take(MAX_BRANCHES)
        .map(str::to_owned)
        .collect())
}

/// most branches listed in the picker.
const MAX_BRANCHES: usize = 500;

/// checks out the local branch `name`. a name that could read as an option
/// is refused before git sees it.
pub fn switch(top: &Path, name: &str) -> Result<(), String> {
    if name.is_empty() || name.starts_with('-') {
        return Err(format!("not a branch: {name}"));
    }
    run(top, &["switch", "--no-guess", name]).map(drop)
}

/// the file's text at HEAD. none when git isn't there, the file isn't in a
/// repository or isn't tracked, or it isn't text.
pub fn base(path: &Path) -> Option<Arc<str>> {
    let dir = path.parent()?;
    let name = path.file_name()?.to_str()?;
    let bytes = git(dir, &["show", &format!("HEAD:./{name}")])?;
    if bytes.len() > MAX_BASE {
        return None;
    }
    String::from_utf8(bytes).ok().map(Arc::from)
}

/// the branch checked out under `dir`, or the short commit when detached.
pub fn branch(dir: &Path) -> Option<String> {
    let line = |bytes: Vec<u8>| {
        let text = String::from_utf8(bytes).ok()?;
        let name = text.trim();
        (!name.is_empty()).then(|| name.to_owned())
    };
    git(dir, &["branch", "--show-current"])
        .and_then(line)
        .or_else(|| git(dir, &["rev-parse", "--short", "HEAD"]).and_then(line))
}

/// the lines of `text` as the diff counts them, `\r` off their ends.
fn lines_of(text: &str) -> Vec<&str> {
    let mut lines: Vec<&str> = text
        .split('\n')
        .map(|l| l.strip_suffix('\r').unwrap_or(l))
        .collect();
    if lines.last() == Some(&"") {
        lines.pop();
    }
    lines
}

fn line_hashes(text: &str) -> Vec<u64> {
    let mut lines: Vec<&str> = text.split('\n').collect();
    // a final newline ends the last line; it doesn't start another
    if lines.last() == Some(&"") {
        lines.pop();
    }
    lines
        .into_iter()
        .map(|l| {
            let mut h = DefaultHasher::new();
            l.strip_suffix('\r').unwrap_or(l).hash(&mut h);
            h.finish()
        })
        .collect()
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Op {
    Keep,
    Insert,
    Delete,
}

/// myers' shortest edit script from `a` to `b`, none past `cap` edits.
fn edits(a: &[u64], b: &[u64], cap: usize) -> Option<Vec<Op>> {
    let (n, m) = (a.len() as isize, b.len() as isize);
    let max = (n + m) as usize;
    let off = max as isize + 1;
    let mut v = vec![0isize; 2 * max + 3];
    // v as it was before each step, only the diagonals that step reads
    let mut trace: Vec<Vec<isize>> = Vec::new();
    let mut done = None;
    'search: for d in 0..=max.min(cap) as isize {
        trace.push(v[(off - d) as usize..=(off + d) as usize].to_vec());
        let mut k = -d;
        while k <= d {
            let at = (off + k) as usize;
            let mut x = if k == -d || (k != d && v[at - 1] < v[at + 1]) {
                v[at + 1]
            } else {
                v[at - 1] + 1
            };
            let mut y = x - k;
            while x < n && y < m && a[x as usize] == b[y as usize] {
                x += 1;
                y += 1;
            }
            v[at] = x;
            if x >= n && y >= m {
                done = Some(d);
                break 'search;
            }
            k += 2;
        }
    }
    let last = done?;
    let mut ops = Vec::new();
    let (mut x, mut y) = (n, m);
    for d in (0..=last).rev() {
        let row = &trace[d as usize];
        let get = |k: isize| row[(k + d) as usize];
        let k = x - y;
        let prev_k = if k == -d || (k != d && get(k - 1) < get(k + 1)) {
            k + 1
        } else {
            k - 1
        };
        let prev_x = if d == 0 { 0 } else { get(prev_k) };
        let prev_y = prev_x - prev_k;
        while x > prev_x.max(0) && y > prev_y.max(0) {
            ops.push(Op::Keep);
            x -= 1;
            y -= 1;
        }
        if d > 0 {
            ops.push(if x == prev_x { Op::Insert } else { Op::Delete });
        }
        x = prev_x;
        y = prev_y;
    }
    ops.reverse();
    Some(ops)
}

/// one row of a unified diff.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DiffLine {
    /// a line both have: its number in HEAD and now, from 0.
    Same(usize, usize, String),
    /// a line only HEAD had.
    Removed(usize, String),
    /// a line only the text now has.
    Added(usize, String),
    /// this many lines alike, folded away.
    Gap(usize),
}

/// `base` against `now` as a unified diff: changes with `context` lines
/// around them, longer stretches alike folded into gaps. empty when alike.
pub fn unified(base: &str, now: &str, context: usize) -> Vec<DiffLine> {
    let (old, new) = (lines_of(base), lines_of(now));
    let (a, b) = (line_hashes(base), line_hashes(now));
    let head = a.iter().zip(&b).take_while(|(x, y)| x == y).count();
    let tail = a[head..]
        .iter()
        .rev()
        .zip(b[head..].iter().rev())
        .take_while(|(x, y)| x == y)
        .count();
    let (mid_a, mid_b) = (&a[head..a.len() - tail], &b[head..b.len() - tail]);
    if mid_a.is_empty() && mid_b.is_empty() {
        return Vec::new();
    }
    let mid = edits(mid_a, mid_b, MAX_EDITS).unwrap_or_else(|| {
        let mut ops = vec![Op::Delete; mid_a.len()];
        ops.extend(std::iter::repeat_n(Op::Insert, mid_b.len()));
        ops
    });
    let ops = std::iter::repeat_n(Op::Keep, head)
        .chain(mid)
        .chain(std::iter::repeat_n(Op::Keep, tail));
    let (mut i, mut j) = (0, 0);
    let mut rows = Vec::new();
    for op in ops {
        match op {
            Op::Keep => {
                rows.push(DiffLine::Same(i, j, new[j].to_owned()));
                i += 1;
                j += 1;
            }
            Op::Delete => {
                rows.push(DiffLine::Removed(i, old[i].to_owned()));
                i += 1;
            }
            Op::Insert => {
                rows.push(DiffLine::Added(j, new[j].to_owned()));
                j += 1;
            }
        }
    }
    // keep `context` alike lines on each side of a change, fold the rest
    let changed: Vec<bool> = rows
        .iter()
        .map(|r| !matches!(r, DiffLine::Same(..)))
        .collect();
    let total = rows.len();
    let near = |k: usize| {
        let lo = k.saturating_sub(context);
        let hi = (k + context + 1).min(total);
        changed[lo..hi].iter().any(|c| *c)
    };
    let mut out = Vec::new();
    let mut folded = 0;
    for (k, row) in rows.into_iter().enumerate() {
        if matches!(row, DiffLine::Same(..)) && !near(k) {
            folded += 1;
            continue;
        }
        if folded > 0 {
            out.push(DiffLine::Gap(folded));
            folded = 0;
        }
        out.push(row);
    }
    if folded > 0 {
        out.push(DiffLine::Gap(folded));
    }
    out
}

/// the lines of `now` that differ from `base`.
pub fn hunks(base: &str, now: &str) -> Vec<Hunk> {
    let (a, b) = (line_hashes(base), line_hashes(now));
    let head = a.iter().zip(&b).take_while(|(x, y)| x == y).count();
    let tail = a[head..]
        .iter()
        .rev()
        .zip(b[head..].iter().rev())
        .take_while(|(x, y)| x == y)
        .count();
    let (mid_a, mid_b) = (&a[head..a.len() - tail], &b[head..b.len() - tail]);
    let ops = edits(mid_a, mid_b, MAX_EDITS).unwrap_or_else(|| {
        // too different to say line by line: the whole middle changed
        let mut ops = vec![Op::Delete; mid_a.len()];
        ops.extend(std::iter::repeat_n(Op::Insert, mid_b.len()));
        ops
    });
    let mut out = Vec::new();
    let mut line = head;
    let (mut start, mut added, mut deleted) = (head, 0, 0);
    let flush = |start: usize, added: usize, deleted: usize, out: &mut Vec<Hunk>| {
        let kind = match (added, deleted) {
            (0, 0) => return,
            (0, _) => Kind::Deleted,
            (_, 0) => Kind::Added,
            _ => Kind::Modified,
        };
        out.push(Hunk {
            lines: start..start + added,
            kind,
        });
    };
    for op in ops {
        match op {
            Op::Keep => {
                flush(start, added, deleted, &mut out);
                (added, deleted) = (0, 0);
                line += 1;
                start = line;
            }
            Op::Insert => {
                added += 1;
                line += 1;
            }
            Op::Delete => deleted += 1,
        }
    }
    flush(start, added, deleted, &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_repositories_below_a_workspace() {
        let dir = std::env::temp_dir().join(format!("ligase-repos-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for p in [
            "b/.git",
            "a/.git",
            "deep/x/y/.git",
            "deeper/1/2/3/.git",
            "target/t/.git",
            ".hid/.git",
        ] {
            std::fs::create_dir_all(dir.join(p)).unwrap();
        }
        let found: Vec<String> = repos(&dir)
            .iter()
            .filter_map(|p| p.strip_prefix(&dir).ok())
            .map(|p| p.to_string_lossy().replace('\\', "/"))
            .collect();
        assert_eq!(
            found,
            ["a", "b", "deep/x/y"],
            "three deep, skipped and hidden folders out"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn kinds(base: &str, now: &str) -> Vec<(Range<usize>, Kind)> {
        hunks(base, now)
            .into_iter()
            .map(|h| (h.lines, h.kind))
            .collect()
    }

    #[test]
    fn status_lines_and_renames() {
        let top = Path::new("/r");
        let out = b"M  a.rs\0 M b.rs\0?? new dir/c.rs\0R  d.rs\0old.rs\0MM e.rs\0";
        let got = parse_status(top, out);
        let rels: Vec<&str> = got.iter().map(|c| c.rel.as_str()).collect();
        assert_eq!(rels, ["a.rs", "b.rs", "d.rs", "e.rs", "new dir/c.rs"]);
        let by = |rel: &str| got.iter().find(|c| c.rel == rel).unwrap();
        assert!(by("a.rs").staged() && !by("a.rs").unstaged());
        assert!(!by("b.rs").staged() && by("b.rs").unstaged());
        assert!(by("e.rs").staged() && by("e.rs").unstaged());
        assert!(!by("new dir/c.rs").staged() && by("new dir/c.rs").unstaged());
        assert_eq!(by("d.rs").index, 'R');
        assert_eq!(by("d.rs").old.as_deref(), Some("old.rs"));
        assert_eq!(by("a.rs").old, None);
        assert_eq!(by("a.rs").path, top.join("a.rs"));
    }

    #[test]
    fn unified_shows_changes_with_context_and_folds_the_rest() {
        let base: String = (1..=20).map(|i| format!("l{i}\n")).collect();
        let now = base.replace("l10\n", "ten\n").replace("l15\n", "");
        let rows = unified(&base, &now, 2);
        assert_eq!(rows.first(), Some(&DiffLine::Gap(7)), "lines 1 to 7 folded");
        assert!(rows.contains(&DiffLine::Removed(9, "l10".into())));
        assert!(rows.contains(&DiffLine::Added(9, "ten".into())));
        assert!(rows.contains(&DiffLine::Removed(14, "l15".into())));
        assert!(
            rows.contains(&DiffLine::Same(11, 11, "l12".into())),
            "context kept"
        );
        assert_eq!(rows.last(), Some(&DiffLine::Gap(3)));
        assert!(unified(&base, &base, 3).is_empty());
        assert_eq!(unified("", "a\n", 3), [DiffLine::Added(0, "a".into())]);
        assert_eq!(unified("a\r\n", "a\n", 3), [], "line ends aside");
    }

    #[test]
    fn same_text_has_no_hunks() {
        assert!(kinds("a\nb\n", "a\nb\n").is_empty());
        assert!(kinds("a\r\nb\r\n", "a\nb\n").is_empty(), "line ends aside");
        assert!(kinds("", "").is_empty());
    }

    #[test]
    fn added_changed_and_deleted_lines() {
        assert_eq!(kinds("a\nc\n", "a\nb\nc\n"), [(1..2, Kind::Added)]);
        assert_eq!(kinds("a\nb\nc\n", "a\nB\nc\n"), [(1..2, Kind::Modified)]);
        assert_eq!(kinds("a\nb\nc\n", "a\nc\n"), [(1..1, Kind::Deleted)]);
        assert_eq!(kinds("", "x\ny\n"), [(0..2, Kind::Added)]);
        assert_eq!(kinds("x\n", ""), [(0..0, Kind::Deleted)]);
    }

    #[test]
    fn separate_changes_stay_separate() {
        let base = "1\n2\n3\n4\n5\n6\n";
        let now = "1\nX\n3\n4\n5\n6\n7\n";
        assert_eq!(
            kinds(base, now),
            [(1..2, Kind::Modified), (6..7, Kind::Added)]
        );
        let now = "0\n1\n2\n4\n5\n6\n";
        assert_eq!(
            kinds(base, now),
            [(0..1, Kind::Added), (3..3, Kind::Deleted)]
        );
    }

    #[test]
    fn random_edits_cover_every_changed_line() {
        let mut seed = 0x9e37_79b9u32;
        let mut roll = move |n: u32| {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            seed % n
        };
        for _ in 0..300 {
            let base: Vec<String> = (0..roll(30)).map(|i| format!("l{i}")).collect();
            let mut now = base.clone();
            for _ in 0..roll(6) {
                let at = roll(now.len() as u32 + 1) as usize;
                match roll(3) {
                    0 => now.insert(at, format!("new{}", roll(1000))),
                    1 if at < now.len() => drop(now.remove(at)),
                    _ if at < now.len() => now[at] = format!("ch{}", roll(1000)),
                    _ => {}
                }
            }
            let (a, b) = (base.join("\n"), now.join("\n"));
            let marked: Vec<usize> = hunks(&a, &b).into_iter().flat_map(|h| h.lines).collect();
            // every line of now that isn't marked is a line base has
            for (i, line) in now.iter().enumerate() {
                if !marked.contains(&i) {
                    assert!(base.contains(line), "{a:?} -> {b:?}: line {i}");
                }
            }
            assert!(marked.iter().all(|&i| i < now.len()));
        }
    }

    #[test]
    fn rev_list_counts() {
        assert_eq!(parse_counts("3\t1\n"), Some((3, 1)), "ahead, then behind");
        assert_eq!(parse_counts("0 0\n"), Some((0, 0)));
        assert_eq!(parse_counts(""), None);
        assert_eq!(parse_counts("3"), None);
        assert_eq!(parse_counts("1 2 3"), None);
        assert_eq!(parse_counts("x\ty"), None);
    }

    #[test]
    fn tracking_of_this_crate() {
        // skipped when the crate isn't in a git repository
        let Some(t) = tracking(Path::new(env!("CARGO_MANIFEST_DIR"))) else {
            return;
        };
        assert!(!t.branch.is_empty());
        if t.upstream.is_none() {
            assert_eq!((t.ahead, t.behind), (0, 0), "no upstream, no counts");
        }
    }

    #[test]
    fn option_looking_branch_names_are_refused() {
        assert!(switch(Path::new("."), "-x").is_err());
        assert!(switch(Path::new("."), "--detach").is_err());
        assert!(switch(Path::new("."), "").is_err());
    }
}
