//! what git knows about a file: its text at HEAD, the lines changed since,
//! and the branch. git runs as a program on the file worker; no repository
//! is ever read on the ui thread.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::ops::Range;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::Arc;

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

fn git(dir: &Path, args: &[&str]) -> Option<Vec<u8>> {
    let mut cmd = Command::new("git");
    cmd.arg("-C")
        .arg(dir)
        .args(args)
        .stdin(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let out = cmd.output().ok()?;
    out.status.success().then_some(out.stdout)
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

    fn kinds(base: &str, now: &str) -> Vec<(Range<usize>, Kind)> {
        hunks(base, now)
            .into_iter()
            .map(|h| (h.lines, h.kind))
            .collect()
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
}
