//! a folder on another machine in the side bar: its tree read over the
//! ssh link, files fetched into a local copy to edit and sent back on save.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use aurea::AureaResult;
use aurea::render::Rect;
use aurea::ui::{Align, ListView, Ui};

use super::{Entry, Session};
use crate::app::Id;
use crate::icons;
use crate::lang::Texts;

const ROW: f32 = 24.0;
const INDENT: f32 = 14.0;
/// most rows the tree keeps.
const MAX_ROWS: usize = 20_000;

/// the link, shared with worker jobs; the ui thread never locks it.
pub type Link = Arc<Mutex<Session>>;

pub enum State {
    Connecting,
    Ready(Link),
    Failed(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Node {
    /// the path on the far side.
    pub path: String,
    pub name: String,
    pub dir: bool,
    pub depth: u16,
    pub open: bool,
}

/// the remote folder's panel state.
pub struct Browser {
    pub host: String,
    /// the folder shown; known once the link is up.
    pub root: String,
    pub state: State,
    pub rows: Vec<Node>,
    scroll: f32,
    picked: Option<usize>,
}

/// what the panel asks the app to do.
pub enum Ask {
    /// read this folder's entries.
    List(String),
    /// fetch this file and open it.
    Open(String),
}

/// `dir` and `name` as one far-side path.
pub fn join(dir: &str, name: &str) -> String {
    if dir.ends_with('/') {
        format!("{dir}{name}")
    } else {
        format!("{dir}/{name}")
    }
}

/// `host` and `path` from `user@host:/path`; the path is none when not
/// given. a `[v6]` host keeps its colons.
pub fn split_target(text: &str) -> (String, Option<String>) {
    let text = text.trim();
    if let Some((host, path)) = text.rsplit_once(':')
        && path.starts_with('/')
        && !host.is_empty()
    {
        return (host.to_owned(), Some(path.to_owned()));
    }
    (text.to_owned(), None)
}

/// where the local copy of `path` on `host` lives: under the config
/// folder, each part made safe for this file system. none without one.
pub fn cache_path(host: &str, path: &str) -> Option<PathBuf> {
    let safe = |part: &str| -> String {
        let s: String = part
            .chars()
            .map(|c| {
                if c.is_alphanumeric() || matches!(c, '.' | '-' | '_' | '@') {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        // never `.` or `..`: a part names a file or folder, nothing above
        if s.chars().all(|c| c == '.') {
            format!("_{s}")
        } else {
            s
        }
    };
    let mut out = aurea::config_dir("ligase")?.join("remote").join(safe(host));
    let parts: Vec<&str> = path.split('/').filter(|p| !p.is_empty()).collect();
    if parts.is_empty() {
        return None;
    }
    for part in parts {
        out.push(safe(part));
    }
    Some(out)
}

impl Browser {
    pub fn new(host: String) -> Self {
        Self {
            host,
            root: String::new(),
            state: State::Connecting,
            rows: Vec::new(),
            scroll: 0.0,
            picked: None,
        }
    }

    /// the link came up on `root` with its entries.
    pub fn ready(&mut self, link: Link, root: String, entries: Vec<Entry>) {
        self.state = State::Ready(link);
        self.rows = nodes(&root, &entries, 0);
        self.root = root;
    }

    /// the entries of `dir` came: they go under its row, which opens.
    pub fn listed(&mut self, dir: &str, entries: &[Entry]) {
        let Some(i) = self.rows.iter().position(|n| n.path == dir && n.dir) else {
            return;
        };
        let depth = self.rows[i].depth.saturating_add(1);
        self.close(i);
        self.rows[i].open = true;
        let room = MAX_ROWS.saturating_sub(self.rows.len());
        let mut kids = nodes(dir, entries, depth);
        kids.truncate(room);
        self.rows.splice(i + 1..i + 1, kids);
    }

    /// shuts row `i`: its descendants go.
    fn close(&mut self, i: usize) {
        let depth = self.rows[i].depth;
        let end = self.rows[i + 1..]
            .iter()
            .position(|n| n.depth <= depth)
            .map_or(self.rows.len(), |k| i + 1 + k);
        self.rows.drain(i + 1..end);
        self.rows[i].open = false;
    }

    /// a click on row `i`: a folder opens or shuts, a file opens.
    fn click(&mut self, i: usize) -> Option<Ask> {
        let node = self.rows.get(i)?.clone();
        if !node.dir {
            return Some(Ask::Open(node.path));
        }
        if node.open {
            self.close(i);
            return None;
        }
        Some(Ask::List(node.path))
    }
}

fn nodes(dir: &str, entries: &[Entry], depth: u16) -> Vec<Node> {
    entries
        .iter()
        .map(|e| Node {
            path: join(dir, &e.name),
            name: e.name.clone(),
            dir: e.dir,
            depth,
            open: false,
        })
        .collect()
}

/// draws the panel. gives what a click asked for.
pub fn show(
    ui: &mut Ui<'_, Id>,
    browser: &mut Browser,
    rect: Rect,
    texts: &Texts,
) -> AureaResult<Option<Ask>> {
    let palette = ui.theme.palette;
    let font = ui.theme.font(13.0);
    let small = ui.theme.font(12.0);
    let inner = rect.inset_xy(6.0, 6.0);
    // where the link goes: host and folder, a dot for its state
    let (head, list) = inner.split_top(26.0, 4.0);
    let (dot, label) = match &browser.state {
        State::Connecting => (palette.warning, texts.tr("Connecting…").to_owned()),
        State::Ready(_) => (
            palette.success,
            format!("{}:{}", browser.host, browser.root),
        ),
        State::Failed(why) => (palette.danger, why.clone()),
    };
    ui.round(head, 8.0, palette.raised)?;
    let d = Rect::new(head.x + 10.0, head.y + 10.0, 7.0, 7.0);
    ui.round(d, 3.5, dot)?;
    let text = Rect::new(head.x + 24.0, head.y, head.width - 30.0, head.height);
    ui.text_clipped(&label, text, &small, palette.dim, Align::Start)?;
    if !matches!(browser.state, State::Ready(_)) {
        return Ok(None);
    }
    let mut picked = browser.picked;
    let rows = &browser.rows;
    let response = ListView::new(&mut browser.scroll, rows.len(), ROW)
        .selected(&mut picked)
        .show(ui, Id::RemoteList, list, |ui, row| {
            let Some(node) = rows.get(row.index) else {
                return Ok(());
            };
            let lit = row.rect.inset_xy(2.0, 1.0);
            if row.picked {
                ui.round(lit, 6.0, palette.selection)?;
            } else if row.hovered {
                ui.round(lit, 6.0, palette.hover)?;
            }
            let x = row.rect.x + 8.0 + f32::from(node.depth) * INDENT;
            let arrow = Rect::new(x, row.rect.y, 12.0, ROW);
            if node.dir {
                let glyph = if node.open { "▾" } else { "▸" };
                ui.text(glyph, arrow, &small, palette.dim, Align::Center)?;
            }
            let icon_box = Rect::new(x + 14.0, row.rect.y + (ROW - 16.0) / 2.0, 16.0, 16.0);
            let icon = icons::for_path(std::path::Path::new(&node.name), node.dir, node.open);
            icons::draw(ui, icon, icon_box, palette.dim)?;
            let name = Rect::new(
                x + 36.0,
                row.rect.y,
                (row.rect.right() - x - 40.0).max(0.0),
                ROW,
            );
            ui.text_clipped(&node.name, name, &font, palette.text, Align::Start)?;
            Ok(())
        })?;
    browser.picked = picked;
    let clicked = response
        .row_clicked
        .or(response.activated.then_some(picked).flatten());
    Ok(clicked.and_then(|i| browser.click(i)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn e(name: &str, dir: bool) -> Entry {
        Entry {
            name: name.to_owned(),
            dir,
        }
    }

    #[test]
    fn targets_and_paths() {
        assert_eq!(
            split_target("me@box:/srv/app"),
            ("me@box".into(), Some("/srv/app".into()))
        );
        assert_eq!(split_target("box"), ("box".into(), None));
        assert_eq!(split_target("[::1]"), ("[::1]".into(), None));
        assert_eq!(
            split_target("[::1]:/x"),
            ("[::1]".into(), Some("/x".into()))
        );
        assert_eq!(join("/", "a"), "/a");
        assert_eq!(join("/srv", "a"), "/srv/a");
    }

    #[test]
    fn cache_paths_stay_inside() {
        let Some(p) = cache_path("me@box", "/srv/../etc/pass:wd") else {
            return;
        };
        let s = p.to_string_lossy();
        assert!(
            p.components().all(|c| !matches!(c, std::path::Component::ParentDir)),
            "{s}"
        );
        assert!(s.ends_with("pass_wd"), "{s}");
        assert!(cache_path("box", "/").is_none());
    }

    #[test]
    fn folders_open_and_shut() {
        let mut b = Browser::new("box".into());
        b.root = "/srv".into();
        b.rows = nodes("/srv", &[e("src", true), e("a.rs", false)], 0);
        assert!(matches!(b.click(0), Some(Ask::List(p)) if p == "/srv/src"));
        b.listed("/srv/src", &[e("lib", true), e("main.rs", false)]);
        assert_eq!(b.rows.len(), 4);
        assert_eq!(b.rows[1].path, "/srv/src/lib");
        b.listed("/srv/src/lib", &[e("x.rs", false)]);
        assert_eq!(b.rows.len(), 5);
        assert!(b.click(0).is_none(), "an open folder shuts");
        assert_eq!(b.rows.len(), 2, "with everything under it");
        assert!(matches!(b.click(1), Some(Ask::Open(p)) if p == "/srv/a.rs"));
        b.listed("/nowhere", &[e("z", false)]);
        assert_eq!(b.rows.len(), 2);
    }
}
