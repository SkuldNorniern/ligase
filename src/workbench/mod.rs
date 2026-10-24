//! the dock and the rules on top of it: one editor area always stays, closing
//! never drops unsaved work, tool panels go back to their home spot, and the
//! layout saves as stable names, not runtime ids.

pub mod panels;

use std::path::{Path, PathBuf};

use aurea::ui::{Dock, DockPlan, DockSide, DockTree};

use crate::document::{Documents, ViewId};
use panels::{PaneId, Panel, Panels};

/// bumped when saved layouts can't be read the old way.
const LAYOUT_VERSION: i64 = 1;

pub struct Workbench {
    pub dock: Dock<PaneId>,
    pub panels: Panels,
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
        let welcome = panels.pane(Panel::Welcome);
        let mut tree = DockTree::new(welcome);
        let explorer = panels.pane(Panel::Explorer);
        tree.insert_sized(explorer, welcome, DockSide::Left, 0.22);
        let search = panels.pane(Panel::Search);
        tree.insert(search, explorer, DockSide::Center);
        tree.activate(explorer);
        let terminal = panels.pane(Panel::Terminal);
        tree.insert_sized(terminal, welcome, DockSide::Bottom, 0.28);
        tree.activate(welcome);
        Self {
            dock: Dock::new(tree),
            panels,
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
        let pane = self.panels.pane(Panel::Editor(view));
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
        match self.panels.get(pane) {
            None => {
                self.tree().remove(pane);
            }
            Some(Panel::Welcome) => return Err(Refused::KeepsEditorArea),
            Some(Panel::Explorer | Panel::Search | Panel::Terminal) => {
                // hidden, not dropped; toggling brings it back
                self.tree().remove(pane);
            }
            Some(Panel::Editor(view)) => {
                if docs.close_loses_work(view) {
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

    /// shows or hides a tool panel. it comes back at its home spot.
    pub fn toggle(&mut self, panel: Panel) {
        let pane = self.panels.pane(panel);
        if self.dock.tree.contains(pane) {
            self.tree().remove(pane);
            return;
        }
        self.show(panel);
    }

    /// puts a tool panel in front, at its home spot if it was hidden:
    /// terminal under the editors, explorer and search side by side as tabs
    /// on the left.
    pub fn show(&mut self, panel: Panel) {
        let pane = self.panels.pane(panel);
        if !self.dock.tree.contains(pane) {
            let sibling = match panel {
                Panel::Explorer => Some(Panel::Search),
                Panel::Search => Some(Panel::Explorer),
                _ => None,
            }
            .and_then(|p| self.panels.find(p))
            .filter(|id| self.dock.tree.contains(*id));
            match sibling {
                Some(beside) => {
                    self.tree().insert(pane, beside, DockSide::Center);
                }
                None => {
                    let Some(area) = self.editor_area() else {
                        return;
                    };
                    let (side, share) = match panel {
                        Panel::Terminal => (DockSide::Bottom, 0.28),
                        _ => (DockSide::Left, 0.22),
                    };
                    self.tree().insert_sized(pane, area, side, share);
                }
            }
        }
        self.tree().activate(pane);
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
                    Panel::Terminal => "terminal".to_owned(),
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
        let layout = self
            .dock
            .tree
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
        let plan = DockPlan::from_text(file.get("layout")?.as_str()?, slot)?;

        let mut panels = Panels::default();
        let slots: Vec<Option<PaneId>> = names
            .into_inner()
            .iter()
            .map(|name| {
                let panel = match name.as_str() {
                    "explorer" => Panel::Explorer,
                    "search" => Panel::Search,
                    "terminal" => Panel::Terminal,
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
        let mut bench = Workbench {
            dock: Dock::new(DockTree::from_plan(&plan)),
            panels,
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
    fn search_sits_with_explorer() {
        let mut bench = Workbench::default();
        let explorer = bench.panels.find(Panel::Explorer).unwrap();
        assert!(bench.dock.tree.is_active(explorer));
        bench.show(Panel::Search);
        let search = bench.panels.find(Panel::Search).unwrap();
        assert!(bench.dock.tree.is_active(search));
        assert!(
            !bench.dock.tree.is_active(explorer),
            "same group, one shows"
        );
        bench.toggle(Panel::Search);
        assert!(!in_tree(&bench, Panel::Search));
        bench.toggle(Panel::Search);
        assert!(bench.dock.tree.is_active(search));
    }

    #[test]
    fn tools_hide_and_come_back() {
        let mut bench = Workbench::default();
        bench.toggle(Panel::Terminal);
        assert!(!in_tree(&bench, Panel::Terminal));
        bench.toggle(Panel::Terminal);
        assert!(in_tree(&bench, Panel::Terminal));
        let explorer = bench.panels.find(Panel::Explorer).unwrap();
        let mut docs = Documents::default();
        assert_eq!(bench.close(explorer, &mut docs), Ok(()));
        assert!(!in_tree(&bench, Panel::Explorer));
        assert!(bench.panels.find(Panel::Explorer).is_some(), "kept alive");
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
        assert!(in_tree(&back, Panel::Explorer));
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
        assert!(in_tree(&back, Panel::Explorer));
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
