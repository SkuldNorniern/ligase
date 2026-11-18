//! the commit graph: history read from git, laid out in lanes so merges and
//! branches show as lines between rows.

use std::path::Path;

/// most commits read for the graph.
pub const MAX_COMMITS: usize = 400;
/// most lanes drawn; history wider than this folds into the last one.
pub const MAX_LANES: usize = 24;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Commit {
    pub id: String,
    pub parents: Vec<String>,
    pub author: String,
    /// seconds since the epoch.
    pub time: i64,
    pub subject: String,
    /// branch and tag names pointing here, `HEAD` first when it does.
    pub refs: Vec<String>,
    /// on HEAD but not on its upstream yet.
    pub outgoing: bool,
}

/// the history of the repository at `top`, newest first, all branches.
/// runs on the worker.
pub fn log(top: &Path) -> Result<Vec<Commit>, String> {
    let n = format!("-n{MAX_COMMITS}");
    let out = super::run(
        top,
        &[
            "log",
            "--all",
            "--topo-order",
            "-z",
            &n,
            "--format=%H%x1f%P%x1f%an%x1f%at%x1f%s%x1f%D",
        ],
    )?;
    let mut commits = parse(&String::from_utf8_lossy(&out));
    // ahead of the upstream; no upstream, nothing is marked
    if let Ok(ahead) = super::run(top, &["rev-list", "@{upstream}..HEAD"]) {
        let ahead = String::from_utf8_lossy(&ahead);
        let ids: std::collections::HashSet<&str> = ahead.lines().collect();
        for c in &mut commits {
            c.outgoing = ids.contains(c.id.as_str());
        }
    }
    Ok(commits)
}

fn parse(out: &str) -> Vec<Commit> {
    out.split('\0')
        .filter_map(|record| {
            let mut f = record.trim_start_matches('\n').split('\x1f');
            let id = f.next()?.trim();
            if id.len() < 7 || !id.bytes().all(|b| b.is_ascii_hexdigit()) {
                return None;
            }
            let parents = f.next()?.split_whitespace().map(str::to_owned).collect();
            let author = f.next()?.chars().take(80).collect();
            let time = f.next()?.trim().parse().unwrap_or(0);
            let subject = f.next()?.chars().take(300).collect();
            let refs = refs(f.next().unwrap_or_default());
            Some(Commit {
                id: id.to_owned(),
                parents,
                author,
                time,
                subject,
                refs,
                outgoing: false,
            })
        })
        .take(MAX_COMMITS)
        .collect()
}

/// `HEAD -> main, origin/main, tag: v1` into names; `HEAD` alone when
/// detached.
fn refs(decorated: &str) -> Vec<String> {
    let mut out = Vec::new();
    for part in decorated
        .split(", ")
        .map(str::trim)
        .filter(|p| !p.is_empty())
    {
        if let Some(branch) = part.strip_prefix("HEAD -> ") {
            out.insert(0, branch.to_owned());
        } else if let Some(tag) = part.strip_prefix("tag: ") {
            out.push(tag.to_owned());
        } else if !part.ends_with("/HEAD") {
            out.push(part.to_owned());
        }
    }
    out.truncate(8);
    out
}

/// one row of the graph: the commit's lane, and the lines that leave this
/// row's centre for the next one's, as (lane here, lane there).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Row {
    pub lane: usize,
    pub down: Vec<(usize, usize)>,
    /// lines that come in from the row above to this commit.
    pub merge_in: Vec<usize>,
    /// lanes in use across this row, for the width.
    pub width: usize,
}

/// lays `commits` (newest first, topological) out in lanes.
pub fn layout(commits: &[Commit]) -> Vec<Row> {
    // the commit each lane waits for
    let mut lanes: Vec<Option<&str>> = Vec::new();
    let mut rows = Vec::with_capacity(commits.len());
    for c in commits {
        // every lane waiting for this commit ends here; the first one keeps it
        let waiting: Vec<usize> = lanes
            .iter()
            .enumerate()
            .filter(|(_, w)| **w == Some(c.id.as_str()))
            .map(|(i, _)| i)
            .collect();
        let lane = match waiting.first() {
            Some(&l) => l,
            None => free(&mut lanes),
        };
        let merge_in: Vec<usize> = waiting.iter().copied().filter(|l| *l != lane).collect();
        for &l in &merge_in {
            lanes[l] = None;
        }
        lanes[lane] = None;
        // parents: the first keeps this lane, the others find or open one
        let mut from_node = Vec::new();
        // a lane to the right waiting for the first parent folds into ours,
        // so the main line keeps to the left
        let mut folded = Vec::new();
        for (k, p) in c.parents.iter().enumerate() {
            let found = lanes.iter().position(|w| *w == Some(p.as_str()));
            let target = if let (0, Some(i)) = (k, found)
                && i > lane
            {
                lanes[i] = None;
                lanes[lane] = Some(p.as_str());
                folded.push(i);
                lane
            } else if let Some(i) = found {
                i
            } else if k == 0 {
                lanes[lane] = Some(p.as_str());
                lane
            } else {
                let i = free(&mut lanes);
                lanes[i] = Some(p.as_str());
                i
            };
            from_node.push(target);
        }
        let mut down: Vec<(usize, usize)> = from_node.into_iter().map(|t| (lane, t)).collect();
        down.extend(folded.iter().map(|&i| (i, lane)));
        for (i, w) in lanes.iter().enumerate() {
            if w.is_some() && i != lane && !down.iter().any(|(_, t)| *t == i) {
                down.push((i, i));
            }
        }
        while lanes.last().is_some_and(Option::is_none) {
            lanes.pop();
        }
        let width = lanes
            .len()
            .max(lane + 1)
            .max(merge_in.iter().max().map_or(0, |m| m + 1));
        // a lane ending here bends into this commit from the row above
        if let Some(prev) = rows.last_mut() {
            let prev: &mut Row = prev;
            for &m in &merge_in {
                let m = m.min(MAX_LANES - 1);
                for edge in &mut prev.down {
                    if edge.1 == m {
                        edge.1 = lane.min(MAX_LANES - 1);
                    }
                }
            }
        }
        rows.push(Row {
            lane: lane.min(MAX_LANES - 1),
            down: down
                .into_iter()
                .map(|(a, b)| (a.min(MAX_LANES - 1), b.min(MAX_LANES - 1)))
                .collect(),
            merge_in: merge_in.into_iter().map(|m| m.min(MAX_LANES - 1)).collect(),
            width: width.min(MAX_LANES),
        });
    }
    rows
}

fn free(lanes: &mut Vec<Option<&str>>) -> usize {
    match lanes.iter().position(Option::is_none) {
        Some(i) => i,
        None => {
            lanes.push(None);
            lanes.len() - 1
        }
    }
}

/// `seconds` ago as a short phrase: "now", "5 min", "3 h", "2 d", "4 w",
/// "7 mo", "2 y".
pub fn ago(now: i64, then: i64) -> String {
    let s = (now - then).max(0);
    match s {
        0..60 => "now".to_owned(),
        60..3600 => format!("{} min", s / 60),
        3600..86_400 => format!("{} h", s / 3600),
        86_400..604_800 => format!("{} d", s / 86_400),
        604_800..2_629_800 => format!("{} w", s / 604_800),
        2_629_800..31_557_600 => format!("{} mo", s / 2_629_800),
        _ => format!("{} y", s / 31_557_600),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(id: &str, parents: &[&str]) -> Commit {
        Commit {
            id: id.repeat(7),
            parents: parents.iter().map(|p| p.repeat(7)).collect(),
            author: String::new(),
            time: 0,
            subject: String::new(),
            refs: Vec::new(),
            outgoing: false,
        }
    }

    #[test]
    fn reads_git_log_records() {
        let out = "aaaaaaa1\x1fbbbbbbb2 ccccccc3\x1fEira\x1f1700000000\x1fMerge it\x1fHEAD -> main, origin/main, origin/HEAD, tag: v1\0\nbbbbbbb2\x1f\x1fEira\x1f1600000000\x1froot\x1f\0junk\0";
        let cs = parse(out);
        assert_eq!(cs.len(), 2);
        assert_eq!(cs[0].parents.len(), 2);
        assert_eq!(cs[0].refs, ["main", "origin/main", "v1"]);
        assert!(cs[1].parents.is_empty());
        assert_eq!(cs[1].subject, "root");
    }

    #[test]
    fn a_line_stays_in_one_lane() {
        let rows = layout(&[c("c", &["b"]), c("b", &["a"]), c("a", &[])]);
        assert!(rows.iter().all(|r| r.lane == 0 && r.width == 1));
        assert_eq!(rows[0].down, [(0, 0)]);
        assert!(rows[2].down.is_empty());
    }

    #[test]
    fn a_merge_opens_a_lane_and_closes_it() {
        // m merges b into a's line: m -> a, b; b -> r; a -> r
        let rows = layout(&[
            c("m", &["a", "b"]),
            c("b", &["r"]),
            c("a", &["r"]),
            c("r", &[]),
        ]);
        assert_eq!(rows[0].lane, 0);
        assert_eq!(rows[0].down, [(0, 0), (0, 1)]);
        assert_eq!(rows[1].lane, 1, "b sits in the lane opened for it");
        assert!(rows[1].down.contains(&(0, 0)), "a's lane passes by");
        assert_eq!(rows[2].lane, 0);
        assert!(rows[2].down.contains(&(1, 0)), "b's line folds back left");
        assert_eq!(rows[3].lane, 0, "the root stays on the main line");
        assert!(rows[3].merge_in.is_empty());
    }

    #[test]
    fn wide_history_stays_capped() {
        // many branch tips all onto one root
        let mut cs: Vec<Commit> = (0..40u8)
            .map(|i| {
                let mut x = c("x", &["r"]);
                x.id = format!("{:07x}", u32::from(i) + 0x100_0000);
                x
            })
            .collect();
        cs.push(c("r", &[]));
        let rows = layout(&cs);
        assert!(
            rows.iter()
                .all(|r| r.lane < MAX_LANES && r.width <= MAX_LANES)
        );
        assert!(
            rows.iter()
                .flat_map(|r| &r.down)
                .all(|(a, b)| *a < MAX_LANES && *b < MAX_LANES)
        );
    }

    #[test]
    fn reads_this_repository() {
        // a checkout without git or history just has nothing to check
        let Ok(commits) = log(Path::new(env!("CARGO_MANIFEST_DIR"))) else {
            return;
        };
        let rows = layout(&commits);
        assert_eq!(rows.len(), commits.len());
        assert!(commits.iter().all(|c| c.id.len() == 40));
    }

    #[test]
    fn short_ages() {
        assert_eq!(ago(100, 100), "now");
        assert_eq!(ago(4000, 100), "1 h");
        assert_eq!(ago(200_000, 0), "2 d");
        assert_eq!(ago(0, 100), "now", "clock skew");
    }
}
