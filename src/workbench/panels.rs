//! what sits in each dock tab. the dock only knows `PaneId`s; this maps
//! them to panels.

use crate::document::{DocumentId, ViewId};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PaneId(u32);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Panel {
    Explorer,
    Search,
    /// git: what changed, staging and commits.
    Git,
    Terminal,
    /// diagnostics of every open file.
    Problems,
    /// stands in the editor area when no file is open.
    Welcome,
    Editor(ViewId),
    /// a file against HEAD.
    Diff(DocumentId),
}

impl Panel {
    /// editors and the welcome tab make up the editor area.
    pub fn is_editor_area(self) -> bool {
        matches!(self, Panel::Welcome | Panel::Editor(_) | Panel::Diff(_))
    }
}

#[derive(Default)]
pub struct Panels {
    list: Vec<(PaneId, Panel)>,
    next: u32,
}

impl Panels {
    /// the pane for `panel`, made if missing. one pane per panel.
    pub fn pane(&mut self, panel: Panel) -> PaneId {
        if let Some(id) = self.find(panel) {
            return id;
        }
        self.next += 1;
        let id = PaneId(self.next);
        self.list.push((id, panel));
        id
    }

    pub fn find(&self, panel: Panel) -> Option<PaneId> {
        self.list
            .iter()
            .find(|(_, p)| *p == panel)
            .map(|(id, _)| *id)
    }

    pub fn get(&self, id: PaneId) -> Option<Panel> {
        self.list.iter().find(|(i, _)| *i == id).map(|(_, p)| *p)
    }

    pub fn remove(&mut self, id: PaneId) {
        self.list.retain(|(i, _)| *i != id);
    }

    pub fn iter(&self) -> impl Iterator<Item = (PaneId, Panel)> + '_ {
        self.list.iter().copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_pane_per_panel() {
        let mut panels = Panels::default();
        let a = panels.pane(Panel::Explorer);
        assert_eq!(panels.pane(Panel::Explorer), a);
        let b = panels.pane(Panel::Terminal);
        assert_ne!(a, b);
        panels.remove(a);
        assert_eq!(panels.get(a), None);
        assert_ne!(panels.pane(Panel::Explorer), a, "ids are never reused");
    }
}
