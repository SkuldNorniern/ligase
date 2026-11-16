//! extensions: packages in the config folder, read on the worker and held
//! in a plain list. only declarative ones for now: file types that borrow
//! a bundled syntax, and snippets. nothing in a package runs.

pub mod draw;
pub mod manifest;
pub mod ui;
pub mod view;

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use manifest::{MAX_BYTES, Manifest, Snippet, View};

use crate::syntax::{self, Associations};

/// packages read at most; the rest are left alone and said so.
pub const MAX_PACKAGES: usize = 200;
/// entries in the folder looked at, packages or not.
const MAX_ENTRIES: usize = 10_000;
/// the manifest's name in a package folder.
pub const FILE: &str = "extension.toml";

/// one folder under the extensions folder.
#[derive(Clone, Debug, PartialEq)]
pub struct Package {
    pub dir: PathBuf,
    pub state: State,
}

#[derive(Clone, Debug, PartialEq)]
pub enum State {
    Ready(Manifest),
    /// why it could not be read.
    Broken(Vec<String>),
    /// a package read before this one has the same id; this one is off.
    Duplicate(String),
}

/// what the scan found, and what came of it.
#[derive(Default)]
pub struct Extensions {
    pub packages: Vec<Package>,
    /// said once per scan: unknown syntax names, packages past the cap.
    pub notes: Vec<String>,
    pub scroll: f32,
}

/// the extensions folder, when there is a config folder.
pub fn folder() -> Option<PathBuf> {
    aurea::config_dir("ligase").map(|d| d.join("extensions"))
}

/// reads every package under `root`, in name order. a missing folder is
/// no packages. runs on the worker.
pub fn scan(root: &Path) -> (Vec<Package>, Vec<String>) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return (Vec::new(), Vec::new());
    };
    let mut dirs: Vec<PathBuf> = entries
        .take(MAX_ENTRIES)
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .map(|e| e.path())
        .collect();
    dirs.sort();
    let mut notes = Vec::new();
    if dirs.len() > MAX_PACKAGES {
        notes.push(format!(
            "{} extension folders, only the first {MAX_PACKAGES} read",
            dirs.len()
        ));
        dirs.truncate(MAX_PACKAGES);
    }
    let mut seen = HashSet::new();
    let packages = dirs
        .into_iter()
        .map(|dir| {
            let state = match read(&dir.join(FILE)) {
                State::Ready(m) if !seen.insert(m.id.clone()) => State::Duplicate(m.id),
                state => state,
            };
            Package { dir, state }
        })
        .collect();
    (packages, notes)
}

/// one manifest, size checked before it is read.
fn read(path: &Path) -> State {
    let broken = |why: String| State::Broken(vec![why]);
    match std::fs::metadata(path) {
        Ok(meta) if meta.len() > MAX_BYTES as u64 => {
            return broken(format!("{FILE} is over {} KiB", MAX_BYTES / 1024));
        }
        Ok(_) => {}
        Err(e) => return broken(format!("no {FILE}: {e}")),
    }
    let text = match std::fs::read(path) {
        Ok(bytes) => match String::from_utf8(bytes) {
            Ok(text) => text,
            Err(_) => return broken(format!("{FILE} is not utf-8")),
        },
        Err(e) => return broken(format!("{FILE}: {e}")),
    };
    match manifest::parse(&text) {
        Ok(m) => State::Ready(m),
        Err(errors) => State::Broken(errors),
    }
}

impl Extensions {
    pub fn ready(&self) -> impl Iterator<Item = &Manifest> {
        self.packages.iter().filter_map(|p| match &p.state {
            State::Ready(m) => Some(m),
            _ => None,
        })
    }

    /// every view of the ready packages, numbered in package order, with
    /// the package it is from.
    pub fn views(&self) -> impl Iterator<Item = (u16, &Manifest, &View)> {
        self.ready()
            .flat_map(|m| m.views.iter().map(move |v| (m, v)))
            .take(usize::from(u16::MAX))
            .enumerate()
            .map(|(i, (m, v))| (u16::try_from(i).unwrap_or(u16::MAX), m, v))
    }

    pub fn view(&self, n: u16) -> Option<(&Manifest, &View)> {
        self.views()
            .find(|(i, _, _)| *i == n)
            .map(|(_, m, v)| (m, v))
    }

    /// packages that are off: broken or a second copy.
    pub fn broken(&self) -> usize {
        self.packages
            .iter()
            .filter(|p| !matches!(p.state, State::Ready(_)))
            .count()
    }

    /// file types the ready packages add. a language borrows a bundled
    /// syntax by name; one that names none, or one not bundled, gets no
    /// colours yet and a note says so. the first package to claim an
    /// extension keeps it.
    pub fn associations(&self) -> (Associations, Vec<String>) {
        let mut out = Associations::default();
        let mut notes = Vec::new();
        for m in self.ready() {
            for lang in &m.languages {
                let Some(name) = &lang.syntax else { continue };
                let Some(syntax) = syntax::bundled(name) else {
                    notes.push(format!("{}: no bundled syntax {name}", m.id));
                    continue;
                };
                for ext in &lang.extensions {
                    out.extensions.entry(ext.clone()).or_insert(syntax);
                }
                for file in &lang.files {
                    out.files.entry(file.clone()).or_insert(syntax);
                }
            }
        }
        (out, notes)
    }

    /// snippets for a file in `language` (a bundled syntax name): ones
    /// written for it, and ones for a package language borrowing it.
    pub fn snippets<'a>(&'a self, language: &'a str) -> impl Iterator<Item = &'a Snippet> + 'a {
        self.ready().flat_map(move |m| {
            m.snippets.iter().filter(move |s| {
                s.language == language
                    || m.languages
                        .iter()
                        .any(|l| l.id == s.language && l.syntax.as_deref() == Some(language))
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(root: &Path, dir: &str, text: &str) {
        std::fs::create_dir_all(root.join(dir)).unwrap();
        std::fs::write(root.join(dir).join(FILE), text).unwrap();
    }

    const LEAN: &str = r#"
[package]
id = "org.example.lean"
name = "Lean"
version = "0.1.0"
api = "1"

[[contributions.languages]]
id = "lean"
extensions = ["lean"]
syntax = "rust"

[[contributions.languages]]
id = "odd"
extensions = ["odd"]
syntax = "nothing-like-it"

[[contributions.snippets]]
language = "lean"
prefix = "thm"
body = "theorem $1 := by\n  $0"
"#;

    #[test]
    fn scan_reads_packages_in_order_and_flags_bad_ones() {
        let root = std::env::temp_dir().join(format!("ligase-ext-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        write(&root, "b-lean", LEAN);
        write(&root, "c-copy", LEAN);
        write(&root, "a-broken", "[package]\nid = 1\n");
        std::fs::create_dir_all(root.join("d-empty")).unwrap();
        std::fs::write(root.join("loose.toml"), "not a folder").unwrap();
        let (packages, notes) = scan(&root);
        let _ = std::fs::remove_dir_all(&root);
        assert!(notes.is_empty());
        let states: Vec<&State> = packages.iter().map(|p| &p.state).collect();
        assert_eq!(states.len(), 4, "folders only");
        assert!(matches!(states[0], State::Broken(_)));
        assert!(matches!(states[1], State::Ready(m) if m.id == "org.example.lean"));
        assert!(matches!(states[2], State::Duplicate(id) if id == "org.example.lean"));
        assert!(matches!(states[3], State::Broken(why) if why[0].starts_with("no ")));
        assert_eq!(scan(&root.join("missing")).0, Vec::new());
    }

    #[test]
    fn big_manifest_is_not_read() {
        let root = std::env::temp_dir().join(format!("ligase-ext-big-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        write(&root, "big", &"#".repeat(MAX_BYTES + 1));
        let (packages, _) = scan(&root);
        let _ = std::fs::remove_dir_all(&root);
        assert!(matches!(&packages[0].state, State::Broken(why) if why[0].contains("KiB")));
    }

    #[test]
    fn languages_borrow_syntax_and_snippets_follow() {
        let manifest = manifest::parse(LEAN).unwrap();
        let ext = Extensions {
            packages: vec![Package {
                dir: PathBuf::new(),
                state: State::Ready(manifest),
            }],
            ..Extensions::default()
        };
        let (assoc, notes) = ext.associations();
        assert_eq!(assoc.extensions.get("lean"), Some(&"rust"));
        assert!(!assoc.extensions.contains_key("odd"));
        assert_eq!(notes.len(), 1, "the unknown syntax is named");
        let prefixes: Vec<&str> = ext.snippets("rust").map(|s| s.prefix.as_str()).collect();
        assert_eq!(prefixes, ["thm"]);
        assert_eq!(ext.snippets("python").count(), 0);
    }
}
