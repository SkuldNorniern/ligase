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
    /// a scan is out for this folder.
    scanning: bool,
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
                scanning: false,
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
        let scan = node.open && node.children.is_none() && !node.scanning;
        node.scanning |= scan;
        let path = node.path.clone();
        self.rebuild();
        Some(if scan {
            Click::Scan(path)
        } else {
            Click::Toggled
        })
    }

    /// scan result for `path`. ignored when the folder is gone from the tree
    /// or already listed.
    /// the path of row `index` and whether it is a folder.
    pub fn row_path(&self, index: usize) -> Option<(PathBuf, bool)> {
        let node = &self.nodes[*self.rows.get(index)?];
        Some((node.path.clone(), node.dir))
    }

    /// forgets what folder `path` held, so a new scan fills it again. true
    /// when it was listed and open, and so wants that scan now.
    pub fn forget(&mut self, path: &Path) -> bool {
        let Some(node) = self.nodes.iter_mut().find(|n| n.dir && n.path == path) else {
            return false;
        };
        if node.children.is_none() {
            return false;
        }
        node.children = None;
        let wants = node.open;
        node.scanning = wants;
        self.rebuild();
        wants
    }

    pub fn listed(&mut self, path: &Path, entries: Vec<(String, bool)>) {
        let Some(parent) = self
            .nodes
            .iter()
            .position(|n| n.dir && n.path == path && n.children.is_none())
        else {
            return;
        };
        self.nodes[parent].scanning = false;
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
                scanning: false,
            });
        }
        self.nodes[parent].children = Some(children);
        self.rebuild();
    }

    /// folders open but never scanned, like the root at start. marks them as
    /// scanning, so each one is handed out once.
    pub fn take_pending(&mut self) -> Vec<PathBuf> {
        self.nodes
            .iter_mut()
            .filter(|n| n.dir && n.open && n.children.is_none() && !n.scanning)
            .map(|n| {
                n.scanning = true;
                n.path.clone()
            })
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
/// a name typed for a new or renamed entry: one plain path part.
pub fn plain_name(name: &str) -> Option<&str> {
    let name = name.trim();
    let bad = name.is_empty()
        || name == "."
        || name == ".."
        || name.contains(['/', '\\', ':', '*', '?', '"', '<', '>', '|'])
        || name.chars().any(char::is_control)
        // windows drops these quietly, so the file would get another name
        || name.ends_with(['.', ' '])
        || reserved(name);
    (!bad).then_some(name)
}

/// windows device names, with or without an extension: `nul.txt` opens
/// the device, not a file.
fn reserved(name: &str) -> bool {
    let stem = name
        .split('.')
        .next()
        .unwrap_or_default()
        .to_ascii_uppercase();
    matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || (stem.len() == 4
            && (stem.starts_with("COM") || stem.starts_with("LPT"))
            && stem.as_bytes()[3].is_ascii_digit()
            && stem.as_bytes()[3] != b'0')
}

/// makes an empty file; never over one already there. runs on the worker.
pub fn create_file(path: &Path) -> Result<(), String> {
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map(drop)
        .map_err(|e| e.to_string())
}

pub fn create_dir(path: &Path) -> Result<(), String> {
    std::fs::create_dir(path).map_err(|e| e.to_string())
}

/// renames `from` to `to`; never over something already at `to`. a name
/// changing only its letter case is the same file and is fine.
pub fn rename(from: &Path, to: &Path) -> Result<(), String> {
    let same = to.exists() && std::fs::canonicalize(from).ok() == std::fs::canonicalize(to).ok();
    if to.exists() && !same {
        return Err("something with that name is already there".to_owned());
    }
    move_no_replace(from, to).map_err(|e| e.to_string())
}

/// a move that fails instead of replacing what appeared at `to` meanwhile.
#[cfg(windows)]
fn move_no_replace(from: &Path, to: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn MoveFileExW(existing: *const u16, new: *const u16, flags: u32) -> i32;
    }
    let wide = |p: &Path| -> Vec<u16> { p.as_os_str().encode_wide().chain([0]).collect() };
    let (a, b) = (wide(from), wide(to));
    // SAFETY: both are nul ended utf-16 paths that live past the call; no
    // flags, so an existing target is an error, never replaced
    let ok = unsafe { MoveFileExW(a.as_ptr(), b.as_ptr(), 0) };
    if ok == 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

/// unix rename replaces; the check above is the guard, with a small window.
#[cfg(not(windows))]
fn move_no_replace(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::rename(from, to)
}

/// deletes a file, or a folder with what is in it. the user said yes first.
pub fn delete(path: &Path) -> Result<(), String> {
    let meta = std::fs::symlink_metadata(path).map_err(|e| e.to_string())?;
    if meta.is_dir() {
        std::fs::remove_dir_all(path)
    } else {
        std::fs::remove_file(path)
    }
    .map_err(|e| e.to_string())
}

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
        assert_eq!(explorer.take_pending(), [PathBuf::from("proj")]);
        assert!(explorer.take_pending().is_empty(), "handed out once");
        explorer.listed(
            root,
            vec![("src".into(), true), ("Cargo.toml".into(), false)],
        );
        assert_eq!(names(&explorer), ["proj", "src", "Cargo.toml"]);
        assert!(explorer.take_pending().is_empty());

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
    fn folder_toggled_during_scan_scans_once() {
        let root = Path::new("p");
        let mut explorer = Explorer::new(root);
        explorer.listed(root, vec![("a".into(), true)]);
        assert_eq!(explorer.click(1), Some(Click::Scan(PathBuf::from("p/a"))));
        assert_eq!(explorer.click(1), Some(Click::Toggled));
        assert_eq!(explorer.click(1), Some(Click::Toggled), "scan still out");
        explorer.listed(Path::new("p/a"), vec![("x".into(), false)]);
        explorer.listed(Path::new("p/a"), vec![("x".into(), false)]);
        assert_eq!(explorer.len(), 3, "second listing ignored");
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

    #[test]
    fn names_are_one_plain_part() {
        assert_eq!(plain_name(" a.rs "), Some("a.rs"));
        assert_eq!(plain_name("a/b"), None);
        assert_eq!(plain_name(".."), None);
        assert_eq!(plain_name(""), None);
        assert_eq!(plain_name("가.txt"), Some("가.txt"));
        assert_eq!(plain_name("nul.txt"), None);
        assert_eq!(plain_name("COM3"), None);
        assert_eq!(plain_name("com0"), Some("com0"));
        assert_eq!(plain_name("a."), None);
        assert_eq!(plain_name("console.log"), Some("console.log"));
    }

    #[test]
    fn file_ops_never_overwrite() {
        let dir = std::env::temp_dir().join(format!("ligase-ops-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let a = dir.join("a.txt");
        create_file(&a).unwrap();
        assert!(create_file(&a).is_err(), "already there");
        let b = dir.join("b.txt");
        create_file(&b).unwrap();
        assert!(rename(&a, &b).is_err(), "never over b");
        rename(&a, &dir.join("c.txt")).unwrap();
        // only the case changes: the same file, allowed where names fold
        rename(&dir.join("c.txt"), &dir.join("C.txt")).unwrap();
        create_dir(&dir.join("sub")).unwrap();
        delete(&dir.join("sub")).unwrap();
        delete(&b).unwrap();
        assert!(!b.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
