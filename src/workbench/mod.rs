//! the dock and the rules on top of it: one editor area always stays, closing
//! never drops unsaved work, tool panels go back to their home spot, and the
//! layout saves as stable names, not runtime ids.

pub mod panels;
pub mod rail;

use std::path::{Path, PathBuf};

use aurea::ui::{Dock, DockPlan, DockSide, DockTree};

use crate::document::{Documents, ViewId};
use panels::{PaneId, Panel, Panels};

/// bumped when saved layouts can't be read the old way.
const LAYOUT_VERSION: i64 = 1;

pub struct Workbench {
    pub dock: Dock<PaneId>,
    pub panels: Panels,
    pub side: Side,
}

/// side bar width when nothing says otherwise.
pub const SIDE_WIDTH: f32 = 260.0;
/// side bar widths a session file may ask for.
const SIDE_WIDTHS: std::ops::RangeInclusive<f32> = 120.0..=1200.0;

/// the side bar beside the rail: one tool panel at a time, or hidden.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Side {
    pub shown: Option<Panel>,
    /// what comes back when the side bar opens again.
    pub last: Panel,
    pub width: f32,
}

impl Default for Side {
    fn default() -> Self {
        Self {
            shown: Some(Panel::Explorer),
            last: Panel::Explorer,
            width: SIDE_WIDTH,
        }
    }
}

/// why a close was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refused {
    /// the last view of a document with unsaved changes.
    Unsaved,
    /// the welcome tab holds the editor area open.
    KeepsEditorArea,
}

impl Default for Workbench {
    fn default() -> Self {
        let mut panels = Panels::default();
        side_panes(&mut panels);
        let welcome = panels.pane(Panel::Welcome);
        let mut tree = DockTree::new(welcome);
        let terminal = panels.pane(Panel::Terminal);
        tree.insert_sized(terminal, welcome, DockSide::Bottom, 0.28);
        tree.activate(welcome);
        Self {
            dock: Dock::new(tree),
            panels,
            side: Side::default(),
        }
    }
}

impl Workbench {
    fn tree(&mut self) -> &mut DockTree<PaneId> {
        &mut self.dock.tree
    }

    /// some pane in the editor area, preferring an active one.
    fn editor_area(&self) -> Option<PaneId> {
        let mut area = self
            .panels
            .iter()
            .filter(|(id, p)| p.is_editor_area() && self.dock.tree.contains(*id));
        let first = area.next()?;
        Some(
            std::iter::once(first)
                .chain(area)
                .find(|(id, _)| self.dock.tree.is_active(*id))
                .unwrap_or(first)
                .0,
        )
    }

    /// shows `view` as a tab in the editor area, replacing the welcome tab.
    pub fn show_editor(&mut self, view: ViewId) -> PaneId {
        self.show_in_editor_area(Panel::Editor(view))
    }

    /// shows `panel` as a tab in the editor area, replacing the welcome tab.
    pub fn show_in_editor_area(&mut self, panel: Panel) -> PaneId {
        let pane = self.panels.pane(panel);
        // squeezed to a sliver by an old layout or a narrow window: it goes
        // back to its home spot at its home size
        if self.squeezed(pane) {
            self.tree().remove(pane);
        }
        if !self.dock.tree.contains(pane) {
            match self.editor_area() {
                Some(beside) => {
                    self.tree().insert(pane, beside, DockSide::Center);
                }
                None => *self.tree() = DockTree::new(pane),
            }
            if let Some(welcome) = self.panels.find(Panel::Welcome) {
                self.tree().remove(welcome);
                self.panels.remove(welcome);
            }
        }
        self.tree().activate(pane);
        pane
    }

    /// closes a tab if that loses nothing. editors hand their view to
    /// `docs`; the last editor leaves a welcome tab behind.
    pub fn close(&mut self, pane: PaneId, docs: &mut Documents) -> Result<(), Refused> {
        self.close_with(pane, docs, false)
    }

    /// closes a tab even with unsaved edits; the user chose to drop them.
    pub fn close_discarding(&mut self, pane: PaneId, docs: &mut Documents) -> Result<(), Refused> {
        self.close_with(pane, docs, true)
    }

    fn close_with(
        &mut self,
        pane: PaneId,
        docs: &mut Documents,
        discard: bool,
    ) -> Result<(), Refused> {
        match self.panels.get(pane) {
            None => {
                self.tree().remove(pane);
            }
            Some(Panel::Welcome) => return Err(Refused::KeepsEditorArea),
            Some(side) if side.is_side() => {
                if self.side.shown == Some(side) {
                    self.side.shown = None;
                }
            }
            Some(
                Panel::Explorer
                | Panel::Search
                | Panel::Git
                | Panel::Extensions
                | Panel::Remote
                | Panel::SideView(_)
                | Panel::BottomView(_)
                | Panel::Terminal
                | Panel::Problems
                | Panel::Agent,
            ) => {
                // hidden, not dropped; toggling brings it back
                self.tree().remove(pane);
            }
            Some(Panel::Diff(_)) => {
                let last = !self
                    .panels
                    .iter()
                    .any(|(id, p)| id != pane && p.is_editor_area() && self.dock.tree.contains(id));
                if last {
                    let welcome = self.panels.pane(Panel::Welcome);
                    self.tree().insert(welcome, pane, DockSide::Center);
                }
                self.tree().remove(pane);
                self.panels.remove(pane);
            }
            Some(Panel::Editor(view)) => {
                if !discard && docs.close_loses_work(view) {
                    return Err(Refused::Unsaved);
                }
                let last = !self
                    .panels
                    .iter()
                    .any(|(id, p)| id != pane && p.is_editor_area() && self.dock.tree.contains(id));
                if last {
                    let welcome = self.panels.pane(Panel::Welcome);
                    self.tree().insert(welcome, pane, DockSide::Center);
                }
                self.tree().remove(pane);
                self.panels.remove(pane);
                docs.close_view(view);
            }
        }
        Ok(())
    }

    /// shows the tab `by` places along from `pane` in its group, wrapping
    /// round. gives the tab now shown.
    pub fn cycle(&mut self, pane: PaneId, by: isize) -> Option<PaneId> {
        let group = self.dock.tree.group_of(&pane)?;
        let at = group.iter().position(|p| *p == pane)?;
        let len = isize::try_from(group.len()).ok()?;
        let next = group[usize::try_from((at as isize + by).rem_euclid(len)).ok()?];
        self.tree().activate(next);
        Some(next)
    }

    /// puts `view` beside `pane`, to the right: a second editor on the same
    /// file, or any view.
    pub fn split(&mut self, pane: PaneId, view: ViewId) -> PaneId {
        let new = self.panels.pane(Panel::Editor(view));
        if self.dock.tree.contains(pane) {
            self.tree().insert(new, pane, DockSide::Right);
        } else {
            self.show_editor(view);
        }
        self.tree().activate(new);
        new
    }

    /// shows or hides a tool panel. it comes back at its home spot.
    pub fn toggle(&mut self, panel: Panel) {
        if panel.is_side() && self.side.shown == Some(panel) {
            self.side.shown = None;
            return;
        }
        let pane = self.panels.pane(panel);
        if self.dock.tree.contains(pane) && !self.squeezed(pane) {
            self.tree().remove(pane);
            return;
        }
        self.show(panel);
    }

    /// `pane` is alone in a group too small to use.
    fn squeezed(&self, pane: PaneId) -> bool {
        let layout = self.dock.layout();
        self.dock.tree.group_of(&pane).is_some_and(|g| g.len() == 1)
            && layout.groups.iter().any(|g| {
                layout
                    .panels
                    .get(g.tabs.clone())
                    .is_some_and(|p| p.contains(&pane))
                    && (g.rect.width < 160.0 || g.rect.height < 80.0)
            })
    }

    /// shows or hides the side bar, coming back with what it last showed.
    pub fn toggle_side(&mut self) {
        self.side.shown = match self.side.shown {
            Some(_) => None,
            None => Some(self.side.last),
        };
    }

    /// is `panel` in sight now: in the side bar or in the dock.
    pub fn shows(&self, panel: Panel) -> bool {
        if panel.is_side() {
            return self.side.shown == Some(panel);
        }
        self.panels
            .find(panel)
            .is_some_and(|id| self.dock.tree.contains(id))
    }

    /// puts a tool panel in front: side ones in the side bar, the terminal
    /// and problems under the editors, at their home spot if hidden.
    pub fn show(&mut self, panel: Panel) {
        if panel.is_side() {
            self.side.shown = Some(panel);
            self.side.last = panel;
            return;
        }
        let pane = self.panels.pane(panel);
        if !self.dock.tree.contains(pane) {
            // the bottom ones share a spot
            // the first of these in sight takes it as a tab
            let near: &[Panel] = match panel {
                Panel::Terminal => &[Panel::Problems],
                Panel::Problems => &[Panel::Terminal],
                Panel::BottomView(_) => &[Panel::Terminal, Panel::Problems],
                _ => &[],
            };
            let sibling = near
                .iter()
                .filter_map(|p| self.panels.find(*p))
                .find(|id| self.dock.tree.contains(*id));
            match sibling {
                Some(beside) => {
                    self.tree().insert(pane, beside, DockSide::Center);
                }
                None => {
                    let Some(area) = self.editor_area() else {
                        return;
                    };
                    // the agent talks beside the code, the rest sit under it
                    if panel == Panel::Agent {
                        self.tree().insert_sized(pane, area, DockSide::Right, 0.34);
                    } else {
                        self.tree().insert_sized(pane, area, DockSide::Bottom, 0.28);
                    }
                }
            }
        }
        self.tree().activate(pane);
    }

    /// takes every extension view out: the side bar, the dock and the
    /// pane list. their numbers change when packages are read again.
    pub fn drop_views(&mut self) {
        if matches!(self.side.shown, Some(Panel::SideView(_))) {
            self.side.shown = None;
        }
        if matches!(self.side.last, Panel::SideView(_)) {
            self.side.last = Panel::Explorer;
        }
        let views: Vec<PaneId> = self
            .panels
            .iter()
            .filter(|(_, p)| matches!(p, Panel::SideView(_) | Panel::BottomView(_)))
            .map(|(id, _)| id)
            .collect();
        for pane in views {
            self.tree().remove(pane);
            self.panels.remove(pane);
        }
    }

    /// the layout as a session file. editors save their file path.
    pub fn save(&self, docs: &Documents) -> String {
        let mut editors: Vec<String> = Vec::new();
        let names: Vec<(PaneId, String)> = self
            .panels
            .iter()
            .map(|(id, panel)| {
                let name = match panel {
                    Panel::Explorer => "explorer".to_owned(),
                    Panel::Search => "search".to_owned(),
                    Panel::Git => "git".to_owned(),
                    Panel::Extensions => "extensions".to_owned(),
                    // a link isn't kept across runs
                    Panel::Remote => "remote".to_owned(),
                    Panel::Terminal => "terminal".to_owned(),
                    Panel::Problems => "problems".to_owned(),
                    Panel::Agent => "agent".to_owned(),
                    Panel::Welcome => "welcome".to_owned(),
                    Panel::Editor(view) => {
                        // a file never saved has no path to come back to
                        let path = docs
                            .doc_of(view)
                            .filter(|d| !d.untitled)
                            .map(|d| d.path.display().to_string())
                            .unwrap_or_default();
                        editors.push(path);
                        format!("editor{}", editors.len() - 1)
                    }
                    // a diff left in the layout stands for the editor area
                    // left out below: extension views come back from their package
                    Panel::Diff(_) | Panel::SideView(_) | Panel::BottomView(_) => {
                        "welcome".to_owned()
                    }
                };
                (id, name)
            })
            .collect();
        let name = |pane: PaneId| {
            names
                .iter()
                .find(|(id, _)| *id == pane)
                .map_or_else(|| "welcome".to_owned(), |(_, n)| n.clone())
        };
        // diffs aren't kept: their file's editor is. one alone in the editor
        // area stays, as the welcome tab, so the area comes back
        let mut tree = self.dock.tree.clone();
        let views: Vec<PaneId> = self
            .panels
            .iter()
            .filter(|(id, p)| matches!(p, Panel::BottomView(_)) && tree.contains(*id))
            .map(|(id, _)| id)
            .collect();
        for pane in views {
            tree.remove(pane);
        }
        let diffs: Vec<PaneId> = self
            .panels
            .iter()
            .filter(|(id, p)| matches!(p, Panel::Diff(_)) && tree.contains(*id))
            .map(|(id, _)| id)
            .collect();
        let others = self
            .panels
            .iter()
            .any(|(id, p)| matches!(p, Panel::Welcome | Panel::Editor(_)) && tree.contains(id));
        for (i, pane) in diffs.into_iter().enumerate() {
            if others || i > 0 {
                tree.remove(pane);
            }
        }
        let layout = tree
            .plan()
            .map(|plan| plan.to_text(name))
            .unwrap_or_default();
        let mut file = toml::Table::new();
        file.insert("version".into(), LAYOUT_VERSION.into());
        file.insert("layout".into(), layout.into());
        file.insert(
            "editors".into(),
            toml::Value::Array(editors.into_iter().map(Into::into).collect()),
        );
        let side_name = |p: Panel| match p {
            Panel::Search => "search",
            Panel::Git => "git",
            Panel::Extensions => "extensions",
            _ => "explorer",
        };
        let shown = match self.side.shown {
            Some(Panel::SideView(_)) | None => "",
            Some(p) => side_name(p),
        };
        file.insert("side".into(), shown.into());
        let last = match self.side.last {
            Panel::SideView(_) => Panel::Explorer,
            p => p,
        };
        file.insert("side_last".into(), side_name(last).into());
        file.insert("side_width".into(), f64::from(self.side.width).into());
        file.to_string()
    }

    /// reads a session file back. `open` makes a view for an editor's path,
    /// or none when the file can't come back; that tab is dropped. none for a
    /// broken or old file, so the caller keeps the default layout.
    pub fn restore(text: &str, mut open: impl FnMut(&Path) -> Option<ViewId>) -> Option<Workbench> {
        let file: toml::Table = text.parse().ok()?;
        if file.get("version")?.as_integer()? != LAYOUT_VERSION {
            return None;
        }
        let editors: Vec<PathBuf> = file
            .get("editors")
            .and_then(toml::Value::as_array)
            .map(|list| {
                list.iter()
                    .filter_map(toml::Value::as_str)
                    .map(PathBuf::from)
                    .collect()
            })
            .unwrap_or_default();

        // read names into slots first; the parser can't make panels itself
        let names = std::cell::RefCell::new(Vec::<String>::new());
        let slot = |name: &str| {
            let mut names = names.borrow_mut();
            let index = names.iter().position(|n| n == name).unwrap_or_else(|| {
                names.push(name.to_owned());
                names.len() - 1
            });
            Some(index)
        };
        // a layout file is ours, but read it as untrusted: a size cap first
        let text = file
            .get("layout")?
            .as_str()
            .filter(|t| t.len() <= 64 * 1024)?;
        let plan = DockPlan::from_text(text, slot)?;

        let mut panels = Panels::default();
        side_panes(&mut panels);
        let slots: Vec<Option<PaneId>> = names
            .into_inner()
            .iter()
            .map(|name| {
                let panel = match name.as_str() {
                    // side panels moved out of the dock; older files had them in it
                    "explorer" | "search" | "git" | "extensions" => return None,
                    "terminal" => Panel::Terminal,
                    "problems" => Panel::Problems,
                    "agent" => Panel::Agent,
                    "welcome" => Panel::Welcome,
                    other => {
                        let index: usize = other.strip_prefix("editor")?.parse().ok()?;
                        Panel::Editor(open(editors.get(index)?)?)
                    }
                };
                Some(panels.pane(panel))
            })
            .collect();
        let plan = map_plan(plan, &|slot| slots.get(slot).copied().flatten())?;
        let side_panel = |key: &str| match file.get(key).and_then(toml::Value::as_str) {
            Some("explorer") => Some(Some(Panel::Explorer)),
            Some("search") => Some(Some(Panel::Search)),
            Some("git") => Some(Some(Panel::Git)),
            Some("extensions") => Some(Some(Panel::Extensions)),
            Some("") => Some(None),
            _ => None,
        };
        let mut side = Side::default();
        if let Some(shown) = side_panel("side") {
            side.shown = shown;
        }
        if let Some(Some(last)) = side_panel("side_last") {
            side.last = last;
        }
        if let Some(width) = file.get("side_width").and_then(toml::Value::as_float) {
            #[allow(clippy::cast_possible_truncation)]
            let width = width as f32;
            if SIDE_WIDTHS.contains(&width) {
                side.width = width;
            }
        }
        let mut bench = Workbench {
            dock: Dock::new(DockTree::from_plan(&plan)),
            panels,
            side,
        };
        if bench.editor_area().is_none() {
            // every editor was dropped; keep an editor area
            let welcome = bench.panels.pane(Panel::Welcome);
            let anchor = bench
                .panels
                .iter()
                .map(|(id, _)| id)
                .find(|&id| bench.dock.tree.contains(id));
            match anchor {
                Some(beside) => {
                    bench
                        .tree()
                        .insert_sized(welcome, beside, DockSide::Right, 0.75);
                }
                None => *bench.tree() = DockTree::new(welcome),
            }
        }
        Some(bench)
    }
}

/// the side bar's panels get their panes up front; they never sit in the
/// dock, so nothing else makes them.
fn side_panes(panels: &mut Panels) {
    for panel in [
        Panel::Explorer,
        Panel::Search,
        Panel::Git,
        Panel::Extensions,
    ] {
        panels.pane(panel);
    }
}

/// plan with slots swapped for panes. slots that map to none are dropped;
/// a group left empty goes too and its split collapses.
fn map_plan(
    plan: DockPlan<usize>,
    f: &impl Fn(usize) -> Option<PaneId>,
) -> Option<DockPlan<PaneId>> {
    match plan {
        DockPlan::Tabs { panels, active } => {
            let shown = panels.get(active).copied();
            let kept: Vec<PaneId> = panels.into_iter().filter_map(f).collect();
            if kept.is_empty() {
                return None;
            }
            let active = shown
                .and_then(f)
                .and_then(|p| kept.iter().position(|&k| k == p))
                .unwrap_or(0);
            Some(DockPlan::Tabs {
                panels: kept,
                active,
            })
        }
        DockPlan::Split {
            axis,
            ratio,
            first,
            second,
        } => match (map_plan(*first, f), map_plan(*second, f)) {
            (Some(a), Some(b)) => Some(DockPlan::Split {
                axis,
                ratio,
                first: Box::new(a),
                second: Box::new(b),
            }),
            (a, b) => a.or(b),
        },
    }
}

/// where the session file for workspace `root` lives. one file per folder,
/// named by a hash of its path.
pub fn session_path(root: &Path) -> Option<PathBuf> {
    // fnv-1a: tiny, and stable across rust versions unlike `DefaultHasher`
    let hash = root
        .to_string_lossy()
        .bytes()
        .fold(0xcbf2_9ce4_8422_2325_u64, |h, b| {
            (h ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3)
        });
    aurea::config_dir("Ligase").map(|dir| dir.join("sessions").join(format!("{hash:016x}.toml")))
}

/// folders kept in the recent list.
const MAX_RECENT: usize = 10;

/// the recent folders file, beside the sessions.
pub fn recent_path() -> Option<PathBuf> {
    aurea::config_dir("Ligase").map(|dir| dir.join("recent.txt"))
}

/// puts `root` first in the recent list `text` (one folder a line) and
/// gives the new list, at most a few long. runs on the worker.
pub fn recent_with(text: &str, root: &Path) -> Vec<PathBuf> {
    let mut out = vec![root.to_path_buf()];
    for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
        let path = PathBuf::from(line);
        if !out.contains(&path) && out.len() < MAX_RECENT {
            out.push(path);
        }
    }
    out
}

/// writes the session file through a temp file, so a crash mid-write
/// never leaves half a file. runs on the worker.
pub fn write_session(path: &Path, text: &str) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let temp = path.with_extension("toml.tmp");
    std::fs::write(&temp, text).map_err(|e| e.to_string())?;
    std::fs::rename(&temp, path).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recent_puts_the_open_folder_first_once() {
        let text = "C:\\a\nC:\\b\n\nC:\\a\n";
        let got = recent_with(text, Path::new("C:\\b"));
        assert_eq!(got, [PathBuf::from("C:\\b"), PathBuf::from("C:\\a")]);
        let many: String = (0..30).map(|i| format!("C:\\{i}\n")).collect();
        assert_eq!(recent_with(&many, Path::new("x")).len(), MAX_RECENT);
    }

    fn in_tree(bench: &Workbench, panel: Panel) -> bool {
        bench
            .panels
            .find(panel)
            .is_some_and(|id| bench.dock.tree.contains(id))
    }

    fn open_file(docs: &mut Documents, path: &str) -> ViewId {
        let (doc, _) = docs.open(Path::new(path));
        docs.loaded(doc, Ok(String::new()));
        docs.add_view(doc)
    }

    #[test]
    fn first_editor_replaces_welcome_last_brings_it_back() {
        let mut docs = Documents::default();
        let mut bench = Workbench::default();
        assert!(in_tree(&bench, Panel::Welcome));
        let view = open_file(&mut docs, "a.rs");
        let pane = bench.show_editor(view);
        assert!(!in_tree(&bench, Panel::Welcome));
        assert!(bench.dock.tree.is_active(pane));
        assert_eq!(bench.close(pane, &mut docs), Ok(()));
        assert!(in_tree(&bench, Panel::Welcome));
        assert!(docs.view(view).is_none());
    }

    #[test]
    fn split_puts_a_second_view_beside() {
        let mut docs = Documents::default();
        let mut bench = Workbench::default();
        let view = open_file(&mut docs, "a.rs");
        let pane = bench.show_editor(view);
        let doc = docs.view(view).unwrap().doc;
        let other = docs.add_view(doc);
        let new = bench.split(pane, other);
        assert!(bench.dock.tree.contains(new));
        assert_ne!(
            bench.dock.tree.group_of(&new),
            bench.dock.tree.group_of(&pane)
        );
    }

    #[test]
    fn tabs_cycle_in_their_group() {
        let mut docs = Documents::default();
        let mut bench = Workbench::default();
        let a = bench.show_editor(open_file(&mut docs, "a.rs"));
        let b = bench.show_editor(open_file(&mut docs, "b.rs"));
        assert_eq!(bench.cycle(b, 1), Some(a));
        assert!(bench.dock.tree.is_active(a));
        assert_eq!(bench.cycle(a, -1), Some(b));
    }

    #[test]
    fn welcome_and_unsaved_refuse_close() {
        let mut docs = Documents::default();
        let mut bench = Workbench::default();
        let welcome = bench.panels.find(Panel::Welcome).unwrap();
        assert_eq!(
            bench.close(welcome, &mut docs),
            Err(Refused::KeepsEditorArea)
        );

        let view = open_file(&mut docs, "a.rs");
        let pane = bench.show_editor(view);
        assert!(docs.edit(view, vec![(0..0, "x".into())], false));
        assert_eq!(bench.close(pane, &mut docs), Err(Refused::Unsaved));
        assert!(bench.dock.tree.contains(pane));
        assert!(docs.view(view).is_some());
    }

    #[test]
    fn side_bar_shows_one_panel_and_folds_away() {
        let mut bench = Workbench::default();
        assert!(bench.shows(Panel::Explorer));
        assert!(!in_tree(&bench, Panel::Explorer), "never a dock tab");
        bench.show(Panel::Search);
        assert!(bench.shows(Panel::Search));
        assert!(!bench.shows(Panel::Explorer), "one at a time");
        bench.toggle(Panel::Search);
        assert_eq!(bench.side.shown, None);
        bench.toggle_side();
        assert!(bench.shows(Panel::Search), "comes back with the last one");
        bench.toggle(Panel::Git);
        assert!(bench.shows(Panel::Git));
    }

    #[test]
    fn bottom_views_join_the_terminal_and_go_on_drop() {
        let mut bench = Workbench::default();
        bench.show(Panel::BottomView(0));
        let view = bench.panels.find(Panel::BottomView(0)).unwrap();
        let terminal = bench.panels.find(Panel::Terminal).unwrap();
        assert_eq!(
            bench.dock.tree.group_of(&view),
            bench.dock.tree.group_of(&terminal),
            "a tab beside the terminal"
        );
        bench.show(Panel::SideView(1));
        assert!(bench.shows(Panel::SideView(1)));
        bench.drop_views();
        assert!(!in_tree(&bench, Panel::BottomView(0)));
        assert_eq!(bench.side.shown, None);
        assert!(bench.panels.find(Panel::BottomView(0)).is_none());
    }

    #[test]
    fn tools_hide_and_come_back() {
        let mut bench = Workbench::default();
        bench.toggle(Panel::Terminal);
        assert!(!in_tree(&bench, Panel::Terminal));
        bench.toggle(Panel::Terminal);
        assert!(in_tree(&bench, Panel::Terminal));
        let terminal = bench.panels.find(Panel::Terminal).unwrap();
        let mut docs = Documents::default();
        assert_eq!(bench.close(terminal, &mut docs), Ok(()));
        assert!(!in_tree(&bench, Panel::Terminal));
        assert!(bench.panels.find(Panel::Terminal).is_some(), "kept alive");
    }

    #[test]
    fn layout_round_trips_with_paths() {
        let mut docs = Documents::default();
        let mut bench = Workbench::default();
        let a = open_file(&mut docs, r"C:\work\src\main.rs");
        let b = open_file(&mut docs, "/home/x/lib (copy), v2.rs");
        bench.show_editor(a);
        bench.show_editor(b);
        bench.toggle(Panel::Terminal);
        bench.show(Panel::Git);
        bench.side.width = 333.0;
        let saved = bench.save(&docs);

        let mut opened = Vec::new();
        let mut fresh = Documents::default();
        let back = Workbench::restore(&saved, |path| {
            opened.push(path.to_path_buf());
            Some(open_file(&mut fresh, &path.display().to_string()))
        })
        .expect("restores");
        assert_eq!(opened.len(), 2);
        assert!(opened.contains(&PathBuf::from(r"C:\work\src\main.rs")));
        assert_eq!(back.side, bench.side);
        assert!(!in_tree(&back, Panel::Terminal));
        assert!(!in_tree(&back, Panel::Welcome));
        assert_eq!(back.save(&fresh).lines().count(), saved.lines().count());
    }

    #[test]
    fn missing_files_drop_their_tabs_and_keep_an_editor_area() {
        let mut docs = Documents::default();
        let mut bench = Workbench::default();
        bench.show_editor(open_file(&mut docs, "gone.rs"));
        let saved = bench.save(&docs);
        let back = Workbench::restore(&saved, |_| None).expect("restores");
        assert!(in_tree(&back, Panel::Welcome));
        assert!(back.shows(Panel::Explorer));
    }

    #[test]
    fn old_layout_with_side_tabs_moves_them_out() {
        let old = "version = 1\nlayout = \"row(0.22, tabs(0: explorer, search), tabs(0: welcome))\"\nside_width = 99999.0\n";
        let back = Workbench::restore(old, |_| None).expect("restores");
        assert!(!in_tree(&back, Panel::Explorer));
        assert!(in_tree(&back, Panel::Welcome));
        assert_eq!(back.side.width, SIDE_WIDTH, "a silly width is ignored");
    }

    #[test]
    fn broken_or_old_session_is_refused() {
        assert!(Workbench::restore("not toml [", |_| None).is_none());
        assert!(Workbench::restore("version = 0\nlayout = \"\"", |_| None).is_none());
        assert!(Workbench::restore("version = 1\nlayout = \"tabs(\"", |_| None).is_none());
    }

    #[test]
    fn sessions_differ_per_folder() {
        let (Some(a), Some(b)) = (
            session_path(Path::new("C:/work/a")),
            session_path(Path::new("C:/work/b")),
        ) else {
            return;
        };
        assert_ne!(a, b);
        assert_eq!(session_path(Path::new("C:/work/a")), Some(a));
    }

    #[test]
    fn session_write_replaces_whole_file() {
        let dir = std::env::temp_dir().join(format!("ligase-session-{}", std::process::id()));
        let path = dir.join("session.toml");
        write_session(&path, "one").unwrap();
        write_session(&path, "two").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "two");
        assert!(!path.with_extension("toml.tmp").exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
