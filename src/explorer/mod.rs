//! file tree for one workspace folder. folders load lazily: opening one asks
//! for a scan, the worker lists it, the result fills it in.

pub mod view;

use std::path::{Path, PathBuf};

/// most entries listed per folder; the rest are left out.
pub const MAX_ENTRIES: usize = 10_000;

/// names never shown.
const HIDDEN: [&str; 1] = [".git"];

#[derive(Clone, Debug)]
struct Node {
    name: String,
    path: PathBuf,
    dir: bool,
    depth: u16,
    open: bool,
    /// none until scanned.
    children: Option<Vec<usize>>,
}

/// one visible row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row<'a> {
    pub name: &'a str,
    pub path: &'a Path,
    pub dir: bool,
    pub open: bool,
    pub depth: u16,
}

/// what a click on a row asks for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Click {
    /// folder opened and never listed; scan it.
    Scan(PathBuf),
    /// folder opened or closed, nothing to load.
    Toggled,
    Open(PathBuf),
}

pub struct Explorer {
    nodes: Vec<Node>,
    /// node ids of visible rows, rebuilt on change.
    rows: Vec<usize>,
    pub scroll: f32,
    pub selected: Option<usize>,
}

impl Explorer {
    /// a tree with the root folder open and waiting for its scan.
    pub fn new(root: &Path) -> Self {
        let name = root.file_name().map_or_else(
            || root.display().to_string(),
            |n| n.to_string_lossy().into_owned(),
        );
        let mut explorer = Self {
            nodes: vec![Node {
                name,
                path: root.to_path_buf(),
                dir: true,
                depth: 0,
                open: true,
                children: None,
            }],
            rows: Vec::new(),
            scroll: 0.0,
            selected: None,
        };
        explorer.rebuild();
        explorer
    }

    pub fn root(&self) -> &Path {
        &self.nodes[0].path
    }

    pub fn len(&self) -> usize {
        self.rows.len()
    }

    pub fn row(&self, index: usize) -> Option<Row<'_>> {
        let node = &self.nodes[*self.rows.get(index)?];
        Some(Row {
            name: &node.name,
            path: &node.path,
            dir: node.dir,
            open: node.open,
            depth: node.depth,
        })
    }

    /// a click on row `index`.
    pub fn click(&mut self, index: usize) -> Option<Click> {
        let id = *self.rows.get(index)?;
        let node = &mut self.nodes[id];
        if !node.dir {
            return Some(Click::Open(node.path.clone()));
        }
        node.open = !node.open;
        let scan = node.open && node.children.is_none();
        let path = node.path.clone();
        self.rebuild();
        Some(if scan {
            Click::Scan(path)
        } else {
            Click::Toggled
        })
    }

    /// scan result for `path`. ignored when the folder is gone from the tree.
    pub fn listed(&mut self, path: &Path, entries: Vec<(String, bool)>) {
        let Some(parent) = self.nodes.iter().position(|n| n.dir && n.path == path) else {
            return;
        };
        let depth = self.nodes[parent].depth + 1;
        let mut children = Vec::with_capacity(entries.len());
        for (name, dir) in entries {
            children.push(self.nodes.len());
            self.nodes.push(Node {
                path: path.join(&name),
                name,
                dir,
                depth,
                open: false,
                children: None,
            });
        }
        self.nodes[parent].children = Some(children);
        self.rebuild();
    }

    /// folders that are open but not scanned yet, like the root at start.
    pub fn pending(&self) -> Vec<PathBuf> {
        self.nodes
            .iter()
            .filter(|n| n.dir && n.open && n.children.is_none())
            .map(|n| n.path.clone())
            .collect()
    }

    fn rebuild(&mut self) {
        let selected = self.selected.and_then(|row| self.rows.get(row).copied());
        self.rows.clear();
        let mut stack = vec![0];
        while let Some(id) = stack.pop() {
            self.rows.push(id);
            let node = &self.nodes[id];
            if node.open
                && let Some(children) = &node.children
            {
                stack.extend(children.iter().rev());
            }
        }
        self.selected = selected.and_then(|id| self.rows.iter().position(|&r| r == id));
    }
}

/// lists one folder: folders first, then files, names compared without case.
/// runs on the worker.
pub fn scan(path: &Path) -> Result<Vec<(String, bool)>, String> {
    let mut entries = Vec::new();
    for entry in std::fs::read_dir(path).map_err(|e| e.to_string())? {
        let Ok(entry) = entry else { continue };
        let name = entry.file_name().to_string_lossy().into_owned();
        if HIDDEN.contains(&name.as_str()) {
            continue;
        }
        let dir = entry.file_type().is_ok_and(|t| t.is_dir());
        entries.push((name, dir));
        if entries.len() == MAX_ENTRIES {
            break;
        }
    }
    entries.sort_by(|a, b| {
        b.1.cmp(&a.1)
            .then_with(|| a.0.to_lowercase().cmp(&b.0.to_lowercase()))
    });
    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(explorer: &Explorer) -> Vec<String> {
        (0..explorer.len())
            .map(|i| explorer.row(i).unwrap().name.to_owned())
            .collect()
    }

    #[test]
    fn folders_open_lazily() {
        let root = Path::new("proj");
        let mut explorer = Explorer::new(root);
        assert_eq!(explorer.pending(), [PathBuf::from("proj")]);
        explorer.listed(
            root,
            vec![("src".into(), true), ("Cargo.toml".into(), false)],
        );
        assert_eq!(names(&explorer), ["proj", "src", "Cargo.toml"]);
        assert!(explorer.pending().is_empty());

        assert_eq!(
            explorer.click(1),
            Some(Click::Scan(PathBuf::from("proj/src")))
        );
        explorer.listed(Path::new("proj/src"), vec![("main.rs".into(), false)]);
        assert_eq!(names(&explorer), ["proj", "src", "main.rs", "Cargo.toml"]);
        assert_eq!(explorer.row(2).unwrap().depth, 2);

        assert_eq!(
            explorer.click(2),
            Some(Click::Open(PathBuf::from("proj/src/main.rs")))
        );
        // close and reopen: no second scan
        assert_eq!(explorer.click(1), Some(Click::Toggled));
        assert_eq!(names(&explorer), ["proj", "src", "Cargo.toml"]);
        assert_eq!(explorer.click(1), Some(Click::Toggled));
        assert_eq!(explorer.len(), 4);
    }

    #[test]
    fn selection_follows_its_node() {
        let root = Path::new("p");
        let mut explorer = Explorer::new(root);
        explorer.listed(root, vec![("a".into(), true), ("b.txt".into(), false)]);
        explorer.selected = Some(2);
        explorer.click(1);
        explorer.listed(Path::new("p/a"), vec![("x".into(), false)]);
        assert_eq!(
            explorer.row(explorer.selected.unwrap()).unwrap().name,
            "b.txt"
        );
    }

    #[test]
    fn stale_scan_is_ignored() {
        let mut explorer = Explorer::new(Path::new("p"));
        explorer.listed(Path::new("elsewhere"), vec![("x".into(), false)]);
        assert_eq!(explorer.len(), 1);
    }

    #[test]
    fn scan_sorts_and_hides() {
        let dir = std::env::temp_dir().join(format!("ligase-scan-{}", std::process::id()));
        std::fs::create_dir_all(dir.join(".git")).unwrap();
        std::fs::create_dir_all(dir.join("Zeta")).unwrap();
        std::fs::write(dir.join("b.rs"), "").unwrap();
        std::fs::write(dir.join("A.rs"), "").unwrap();
        let got = scan(&dir).unwrap();
        let names: Vec<&str> = got.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, ["Zeta", "A.rs", "b.rs"]);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
