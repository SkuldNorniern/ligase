//! a folder on another machine in the side bar: its tree read over the
//! ssh link, files fetched into a local copy to edit and sent back on save.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use aurea::render::Rect;
use aurea::ui::{Align, Button, ListView, Sense, TextField, Ui};
use aurea::{AureaResult, CursorIcon};

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
    /// typed after a refused login; sent once, then cleared.
    pub password: String,
    /// the password the link was made with, kept in memory only so a
    /// reconnect needs no typing. never written anywhere.
    pub secret: Option<String>,
    /// the folder shown; known once the link is up.
    pub root: String,
    pub state: State,
    pub rows: Vec<Node>,
    scroll: f32,
    picked: Option<usize>,
    /// the search box, open or not, and what it holds.
    pub search_open: bool,
    pub query: String,
    /// what the last search found: path, line from 1, the line.
    pub hits: Vec<(String, usize, String)>,
    pub searching: bool,
    hit_scroll: f32,
}

/// the panel before a link: a target to type, hosts to pick.
#[derive(Default)]
pub struct Picker {
    pub input: String,
    /// recent targets first, then `~/.ssh/config` hosts.
    pub hosts: Vec<String>,
    scroll: f32,
}

/// what the panel asks the app to do.
pub enum Ask {
    /// read this folder's entries.
    List(String),
    /// fetch this file and open it.
    Open(String),
    /// fetch this file and open it at this line (from 1).
    OpenAt(String, usize),
    /// search the folder for this text.
    Search(String),
    /// a shell on the host, in the folder.
    Terminal,
    /// read the folder again.
    Refresh,
    /// connect again after the link went down.
    Reconnect,
    /// let the link go.
    Disconnect,
    /// connect to `user@host:/path` from the picker.
    Connect(String),
    /// a right click at this point, on this row (path, folder) or the
    /// empty space.
    Menu(aurea::render::Point, Option<(String, bool)>),
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
/// folder, in a folder named by a hash of the whole path, under the file's
/// own name made safe. two remote paths never share a copy, even where
/// `a b` and `a_b`, or `A` and `a`, would be one name here. none without
/// a config folder.
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
    let name = path.split('/').rfind(|p| !p.is_empty())?;
    // fnv-1a: the same path finds the same copy across runs
    let hash = path.bytes().fold(0xcbf2_9ce4_8422_2325_u64, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
    });
    Some(
        aurea::config_dir("ligase")?
            .join("remote")
            .join(safe(host))
            .join(format!("{hash:016x}"))
            .join(safe(name)),
    )
}

impl Browser {
    pub fn new(host: String) -> Self {
        Self {
            host,
            password: String::new(),
            secret: None,
            root: String::new(),
            state: State::Connecting,
            rows: Vec::new(),
            scroll: 0.0,
            picked: None,
            search_open: false,
            query: String::new(),
            hits: Vec::new(),
            searching: false,
            hit_scroll: 0.0,
        }
    }

    /// the root read again: the tree starts over, folded.
    pub fn reset_root(&mut self, entries: &[Entry]) {
        self.rows = nodes(&self.root, entries, 0);
        self.picked = None;
    }

    /// a search came back.
    pub fn found(&mut self, hits: Vec<(String, usize, String)>) {
        self.searching = false;
        self.hits = hits;
        self.hit_scroll = 0.0;
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
    let ready = matches!(browser.state, State::Ready(_));
    // tools at the head's right: search, a shell there, read again, let go
    let tools: &[(&str, u16)] = if ready {
        &[("search", 1), ("shell", 2), ("refresh", 3), ("close", 4)]
    } else {
        &[("close", 4)]
    };
    let mut asked = None;
    let mut tx = head.right() - 4.0;
    for (icon, part) in tools.iter().rev() {
        tx -= 22.0;
        let b = Rect::new(tx, head.y + 2.0, 22.0, 22.0);
        let input = ui.interact(Id::RemoteTool(*part), 0, b, Sense::Click, true);
        let lit = *part == 1 && browser.search_open;
        if input.hovered || lit {
            ui.round(b, 6.0, palette.hover)?;
        }
        if input.hovered {
            ui.set_cursor(CursorIcon::Pointer);
        }
        icons::draw(
            ui,
            icon,
            Rect::new(b.x + 3.0, b.y + 3.0, 16.0, 16.0),
            palette.dim,
        )?;
        if input.clicked {
            asked = match part {
                1 => {
                    browser.search_open = !browser.search_open;
                    if !browser.search_open {
                        browser.hits.clear();
                    }
                    None
                }
                2 => Some(Ask::Terminal),
                3 => Some(Ask::Refresh),
                _ => Some(Ask::Disconnect),
            };
        }
    }
    let text = Rect::new(
        head.x + 24.0,
        head.y,
        (tx - head.x - 28.0).max(0.0),
        head.height,
    );
    ui.text_clipped(&label, text, &small, palette.dim, Align::Start)?;
    if let State::Failed(why) = &browser.state {
        // a host that wants a password: a field for it, ssh asks ligase
        let refused = why.contains("Permission denied") || why.contains("password");
        let mut y = list.y + 6.0;
        let mut typed = false;
        if refused {
            let field = Rect::new(list.x, y, list.width.min(320.0), 28.0);
            typed = TextField::new(&mut browser.password)
                .password(true)
                .placeholder(texts.tr("Password"))
                .max_chars(1000)
                .show(ui, Id::RemoteTool(7), field)?
                .submitted;
            y += 36.0;
        }
        let label = if refused {
            texts.tr("Connect with password")
        } else {
            texts.tr("Reconnect")
        };
        let w = (ui.text_width(label, &small) + 32.0)
            .max(140.0)
            .min(list.width);
        let b = Rect::new(list.x, y, w, 28.0);
        if Button::new(label)
            .primary()
            .show(ui, Id::RemoteTool(5), b)?
            .clicked
            || typed
        {
            asked = Some(Ask::Reconnect);
        }
    }
    if !ready {
        return Ok(asked);
    }
    let list = if browser.search_open {
        let (field, rest) = list.split_top(28.0, 6.0);
        let typed = TextField::new(&mut browser.query)
            .placeholder(texts.tr("Search the remote folder"))
            .max_chars(500)
            .show(ui, Id::RemoteSearch, field)?;
        if typed.submitted && !browser.query.trim().is_empty() {
            browser.searching = true;
            asked = Some(Ask::Search(browser.query.clone()));
        }
        rest
    } else {
        list
    };
    if browser.search_open && (browser.searching || !browser.hits.is_empty()) {
        if browser.searching {
            ui.text(
                texts.tr("Searching…"),
                Rect::new(list.x + 8.0, list.y, list.width, ROW),
                &small,
                palette.dim,
                Align::Start,
            )?;
            return Ok(asked);
        }
        let hits = &browser.hits;
        let root = browser.root.as_str();
        let mut none = None;
        let response = ListView::new(&mut browser.hit_scroll, hits.len(), ROW * 1.6)
            .selected(&mut none)
            .show(ui, Id::RemoteHits, list, |ui, row| {
                let Some((path, line, text)) = hits.get(row.index) else {
                    return Ok(());
                };
                if row.hovered {
                    ui.round(row.rect.inset_xy(2.0, 1.0), 6.0, palette.hover)?;
                }
                let rel = path
                    .strip_prefix(root)
                    .unwrap_or(path)
                    .trim_start_matches('/');
                let top = Rect::new(
                    row.rect.x + 8.0,
                    row.rect.y + 1.0,
                    row.rect.width - 12.0,
                    ROW * 0.8,
                );
                ui.text_clipped(
                    &format!("{rel}:{line}"),
                    top,
                    &small,
                    palette.dim,
                    Align::Start,
                )?;
                let bottom = Rect::new(
                    row.rect.x + 8.0,
                    row.rect.y + ROW * 0.75,
                    row.rect.width - 12.0,
                    ROW * 0.8,
                );
                ui.text_clipped(text, bottom, &font, palette.text, Align::Start)?;
                Ok(())
            })?;
        if let Some(i) = response.row_clicked
            && let Some((path, line, _)) = hits.get(i)
        {
            asked = Some(Ask::OpenAt(path.clone(), *line));
        }
        return Ok(asked);
    }
    if browser.search_open && browser.query.is_empty() {
        // the tree stays below an empty search box
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
    // a right click: the row under it, from the scroll and row height
    if let Some(at) = ui.secondary_at(Id::RemoteList, 0) {
        let row = ((at.y - list.y + browser.scroll) / ROW).floor();
        let node = (row >= 0.0)
            .then_some(row as usize)
            .and_then(|r| browser.rows.get(r))
            .map(|n| (n.path.clone(), n.dir));
        return Ok(Some(Ask::Menu(at, node)));
    }
    let clicked = response
        .row_clicked
        .or(response.activated.then_some(picked).flatten());
    Ok(clicked.and_then(|i| browser.click(i)).or(asked))
}

/// the panel before a link: a target to type, then known hosts to pick.
pub fn picker(
    ui: &mut Ui<'_, Id>,
    pick: &mut Picker,
    rect: Rect,
    texts: &Texts,
) -> AureaResult<Option<Ask>> {
    let palette = ui.theme.palette;
    let font = ui.theme.font(13.0);
    let small = ui.theme.font(12.0);
    let inner = rect.inset_xy(8.0, 8.0);
    let mut ask = None;
    let (title, rest) = inner.split_top(22.0, 4.0);
    ui.text(
        texts.tr("Open a remote folder"),
        title,
        &ui.theme.bold(13.0),
        palette.text,
        Align::Start,
    )?;
    let (note, rest) = rest.split_top(34.0, 6.0);
    ui.text_clipped(
        texts.tr("Over ssh: keys and ~/.ssh/config are ssh's own."),
        note,
        &small,
        palette.dim,
        Align::Start,
    )?;
    let (field, rest) = rest.split_top(30.0, 6.0);
    let typed = TextField::new(&mut pick.input)
        .placeholder(texts.tr("user@host:/path"))
        .max_chars(500)
        .show(ui, Id::RemoteSearch, field)?;
    let (go, rest) = rest.split_top(28.0, 12.0);
    let go = Rect::new(go.x, go.y, 120.0_f32.min(go.width), go.height);
    let target = pick.input.trim().to_owned();
    if (Button::new(texts.tr("Connect"))
        .primary()
        .enabled(!target.is_empty())
        .show(ui, Id::RemoteTool(6), go)?
        .clicked
        || typed.submitted)
        && !target.is_empty()
    {
        ask = Some(Ask::Connect(target));
    }
    if pick.hosts.is_empty() {
        return Ok(ask);
    }
    let (head, list) = rest.split_top(20.0, 4.0);
    ui.text(
        texts.tr("Known hosts"),
        head,
        &small,
        palette.dim,
        Align::Start,
    )?;
    let hosts = &pick.hosts;
    let mut none = None;
    let response = ListView::new(&mut pick.scroll, hosts.len(), ROW)
        .selected(&mut none)
        .show(ui, Id::RemoteHits, list, |ui, row| {
            let Some(host) = hosts.get(row.index) else {
                return Ok(());
            };
            if row.hovered {
                ui.round(row.rect.inset_xy(2.0, 1.0), 6.0, palette.hover)?;
                ui.set_cursor(CursorIcon::Pointer);
            }
            let icon = Rect::new(
                row.rect.x + 6.0,
                row.rect.y + (ROW - 16.0) / 2.0,
                16.0,
                16.0,
            );
            icons::draw(ui, "folder", icon, palette.dim)?;
            let at = Rect::new(row.rect.x + 28.0, row.rect.y, row.rect.width - 32.0, ROW);
            ui.text_clipped(host, at, &font, palette.text, Align::Start)?;
            Ok(())
        })?;
    if let Some(i) = response.row_clicked
        && let Some(host) = hosts.get(i)
    {
        ask = Some(Ask::Connect(host.clone()));
    }
    Ok(ask)
}

/// where recent remote targets are kept, one a line, newest first.
pub fn recent_path() -> Option<PathBuf> {
    aurea::config_dir("ligase").map(|d| d.join("remote-recent.txt"))
}

/// the picker's hosts: recent targets, then `~/.ssh/config` ones. reads the
/// disk: worker only.
pub fn known_hosts() -> Vec<String> {
    let mut out: Vec<String> = recent_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .map(|t| {
            t.lines()
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .take(10)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME"));
    if let Some(home) = home {
        let dot = PathBuf::from(home).join(".ssh");
        for text in ssh_configs(&dot) {
            for host in super::ssh_hosts(&text) {
                if !out
                    .iter()
                    .any(|h| h == &host || h.starts_with(&format!("{host}:")))
                {
                    out.push(host);
                }
            }
        }
    }
    out
}

/// `~/.ssh/config` and the files its `Include` lines name, one level deep:
/// `~/` and paths relative to `~/.ssh`, a `*` in the file name. at most 32
/// files, each under 1 MiB.
fn ssh_configs(dot: &std::path::Path) -> Vec<String> {
    let read = |p: &std::path::Path| {
        std::fs::metadata(p)
            .ok()
            .filter(|m| m.is_file() && m.len() < 1024 * 1024)
            .and_then(|_| std::fs::read_to_string(p).ok())
    };
    let Some(main) = read(&dot.join("config")) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for line in main.lines().take(10_000) {
        let line = line.trim();
        let Some(rest) = line
            .get(..8)
            .filter(|w| w.eq_ignore_ascii_case("include "))
            .map(|_| &line[8..])
        else {
            continue;
        };
        for pattern in rest.split_whitespace() {
            let path = match pattern.strip_prefix("~/") {
                Some(r) => dot
                    .parent()
                    .map_or_else(|| dot.join(r), |home| home.join(r)),
                None => dot.join(pattern),
            };
            let (Some(dir), Some(name)) =
                (path.parent(), path.file_name().and_then(|n| n.to_str()))
            else {
                continue;
            };
            if let Some((head, tail)) = name.split_once('*') {
                let Ok(list) = std::fs::read_dir(dir) else {
                    continue;
                };
                let mut found: Vec<PathBuf> = list
                    .flatten()
                    .map(|e| e.path())
                    .filter(|p| {
                        p.file_name().and_then(|n| n.to_str()).is_some_and(|n| {
                            n.starts_with(head)
                                && n.ends_with(tail)
                                && n.len() >= head.len() + tail.len()
                        })
                    })
                    .collect();
                found.sort();
                for p in found {
                    if out.len() < 32
                        && let Some(t) = read(&p)
                    {
                        out.push(t);
                    }
                }
            } else if out.len() < 32
                && let Some(t) = read(&path)
            {
                out.push(t);
            }
        }
    }
    out.insert(0, main);
    out
}

/// puts `target` first in the recent list, ten kept. worker only.
pub fn remember(target: &str) {
    let Some(path) = recent_path() else { return };
    let mut list: Vec<String> = std::fs::read_to_string(&path)
        .map(|t| t.lines().map(str::to_owned).collect())
        .unwrap_or_default();
    list.retain(|l| l.trim() != target && !l.trim().is_empty());
    list.insert(0, target.to_owned());
    list.truncate(10);
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(&path, list.join("\n") + "\n");
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
    fn included_ssh_configs_are_read() {
        let dot = std::env::temp_dir().join(format!("ligase-ssh-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dot);
        std::fs::create_dir_all(dot.join("config.d")).unwrap();
        std::fs::write(dot.join("config"), "Include config.d/*\nHost main\n").unwrap();
        std::fs::write(dot.join("config.d/work"), "Host build-box\n").unwrap();
        let hosts: Vec<String> = ssh_configs(&dot)
            .iter()
            .flat_map(|t| crate::remote::ssh_hosts(t))
            .collect();
        assert_eq!(hosts, ["main", "build-box"]);
        std::fs::remove_dir_all(&dot).unwrap();
    }

    #[test]
    fn cache_paths_stay_inside() {
        let Some(p) = cache_path("me@box", "/srv/../etc/pass:wd") else {
            return;
        };
        let s = p.to_string_lossy();
        assert!(
            p.components()
                .all(|c| !matches!(c, std::path::Component::ParentDir)),
            "{s}"
        );
        assert!(s.ends_with("pass_wd"), "{s}");
        assert!(cache_path("box", "/").is_none());
        // names this file system would fold together stay apart
        assert_ne!(cache_path("box", "/srv/a b"), cache_path("box", "/srv/a_b"));
        assert_ne!(
            cache_path("box", "/srv/A.txt"),
            cache_path("box", "/srv/a.txt")
        );
        assert_eq!(cache_path("box", "/srv/a"), cache_path("box", "/srv/a"));
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
