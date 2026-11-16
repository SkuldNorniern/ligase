//! extensions: packages in the config folder, read on the worker and held
//! in a plain list. only declarative ones for now: file types that borrow
//! a bundled syntax, and snippets. nothing in a package runs.

pub mod draw;
pub mod manifest;
pub mod trust;
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
    /// grammars read from the package, ready to register.
    pub grammars: Vec<Grammar>,
    /// what went wrong with parts of a package that still loaded.
    pub problems: Vec<String>,
    /// abbreviation tables read from the package.
    pub abbrevs: Vec<AbbrevTable>,
}

/// key and text pairs.
pub type Pairs = Vec<(String, String)>;

/// an abbreviation table a package brings for a language.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AbbrevTable {
    pub language: String,
    pub leader: char,
    pub pairs: Pairs,
}

/// largest abbreviation file read.
pub const MAX_ABBREV_FILE: u64 = 256 * 1024;
/// abbreviations kept per file.
pub const MAX_ABBREVS: usize = 5000;

/// a sylven grammar a package brings.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Grammar {
    /// the language id the spec declares.
    pub name: String,
    pub text: String,
}

/// a fed view's state: what it asked last and the tree it shows now.
#[derive(Default)]
pub struct FeedState {
    /// the editor view, caret and text revision last asked about.
    pub asked: Option<(crate::document::ViewId, usize, resolvase::Revision)>,
    /// a request is out, since then.
    pub waiting: Option<std::time::Instant>,
    /// the body with the answer under it.
    pub shown: Option<ui::Node>,
}

/// longest answer shown in a fed view.
pub const MAX_FEED: usize = 64 * 1024;

/// `field` (a dotted path, empty for the whole) of a server's answer, as
/// text: a string as it is, nothing for null or a missing field, else the
/// value as json. cut to `MAX_FEED`.
pub fn pick(result: &serde_json::Value, field: &str) -> Option<String> {
    let mut at = result;
    for part in field.split('.').filter(|p| !p.is_empty()) {
        at = at.get(part)?;
    }
    let mut text = match at {
        serde_json::Value::Null => return None,
        serde_json::Value::String(s) => s.clone(),
        other => other.to_string(),
    };
    if text.len() > MAX_FEED {
        let mut cut = MAX_FEED;
        while !text.is_char_boundary(cut) {
            cut -= 1;
        }
        text.truncate(cut);
    }
    (!text.trim().is_empty()).then_some(text)
}

/// largest grammar file read.
pub const MAX_GRAMMAR: u64 = 128 * 1024;

#[derive(Clone, Debug, PartialEq)]
pub enum State {
    Ready(Box<Manifest>),
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
    /// which packages may start the programs they declare.
    pub trust: trust::Trust,
    /// grammar names kept for the life of the app, one copy per name.
    names: HashSet<&'static str>,
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
            let mut grammars = Vec::new();
            let mut problems = Vec::new();
            let mut abbrevs = Vec::new();
            if let State::Ready(m) = &state {
                for set in &m.abbreviations {
                    match abbreviations(&dir, &set.file, set.leader) {
                        Ok((pairs, skipped)) => {
                            problems
                                .extend(skipped.into_iter().map(|w| format!("{}: {w}", set.file)));
                            abbrevs.push(AbbrevTable {
                                language: set.language.clone(),
                                leader: set.leader,
                                pairs,
                            });
                        }
                        Err(why) => problems.push(format!("{}: {why}", set.file)),
                    }
                }
                for file in &m.syntaxes {
                    match grammar(&dir, file) {
                        Ok(g) if grammars.iter().any(|o: &Grammar| o.name == g.name) => {
                            problems.push(format!("{file}: {} is declared twice", g.name));
                        }
                        Ok(g) => grammars.push(g),
                        Err(why) => problems.push(format!("{file}: {why}")),
                    }
                }
            }
            Package {
                dir,
                state,
                grammars,
                problems,
                abbrevs,
            }
        })
        .collect();
    (packages, notes)
}

/// every command line `m` would start, as a grant records them.
pub fn lines(m: &Manifest) -> Vec<Vec<String>> {
    m.servers.iter().map(manifest::Server::line).collect()
}

/// an abbreviation table: `"key" = "text"` pairs. keys are 1-32 chars, no
/// spaces and no leader; texts 1-32 chars, no control chars. a bad pair is
/// skipped and said, the rest load.
fn abbreviations(dir: &Path, file: &str, leader: char) -> Result<(Pairs, Vec<String>), String> {
    let path = file
        .split('/')
        .fold(dir.to_path_buf(), |p, part| p.join(part));
    let meta = std::fs::metadata(&path).map_err(|e| e.to_string())?;
    if meta.len() > MAX_ABBREV_FILE {
        return Err(format!("over {} KiB", MAX_ABBREV_FILE / 1024));
    }
    let text = String::from_utf8(std::fs::read(&path).map_err(|e| e.to_string())?)
        .map_err(|_| "not utf-8".to_owned())?;
    let table: toml::Table = text.parse().map_err(|e: toml::de::Error| {
        e.to_string().lines().next().unwrap_or_default().to_owned()
    })?;
    let mut pairs = Vec::new();
    let mut skipped = Vec::new();
    for (key, value) in &table {
        if pairs.len() == MAX_ABBREVS {
            skipped.push(format!("more than {MAX_ABBREVS}, the rest left out"));
            break;
        }
        let key_ok = (1..=editor_key_max()).contains(&key.chars().count())
            && !key
                .chars()
                .any(|c| c.is_whitespace() || c.is_control() || c == leader);
        let text = value
            .as_str()
            .filter(|t| (1..=32).contains(&t.chars().count()) && !t.chars().any(char::is_control));
        match (key_ok, text) {
            (true, Some(text)) => pairs.push((key.clone(), text.to_owned())),
            _ => skipped.push(format!(
                "{key:?} skipped: key 1-32 chars, no spaces, text 1-32 chars"
            )),
        }
    }
    Ok((pairs, skipped))
}

fn editor_key_max() -> usize {
    crate::editor::abbrev::MAX_KEY
}

/// a grammar file from a package: read with a size cap, parsed under a
/// guard so a spec that trips the parser only loses itself.
fn grammar(dir: &Path, file: &str) -> Result<Grammar, String> {
    let path = file
        .split('/')
        .fold(dir.to_path_buf(), |p, part| p.join(part));
    let meta = std::fs::metadata(&path).map_err(|e| e.to_string())?;
    if meta.len() > MAX_GRAMMAR {
        return Err(format!("over {} KiB", MAX_GRAMMAR / 1024));
    }
    let text = String::from_utf8(std::fs::read(&path).map_err(|e| e.to_string())?)
        .map_err(|_| "not utf-8".to_owned())?;
    let parsed = std::panic::catch_unwind(|| sylven_dsl::parse_spec(&text))
        .map_err(|_| "the grammar parser gave up on it".to_owned())?;
    let spec = parsed.map_err(|errors| {
        let first = errors.first().map(|e| format!("{e:?}")).unwrap_or_default();
        format!("{} grammar errors, first: {first}", errors.len())
    })?;
    let name = spec.language.id;
    let name_ok = !name.is_empty()
        && name.len() <= 40
        && name.starts_with(|c: char| c.is_ascii_lowercase())
        && name.bytes().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'_' | b'-' | b'+')
        });
    if !name_ok {
        return Err(format!(
            "language id {name:?} is not a plain lowercase name"
        ));
    }
    if syntax::bundled(&name).is_some() {
        return Err(format!("{name} is a bundled language; pick another id"));
    }
    Ok(Grammar { name, text })
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
        Ok(m) => State::Ready(Box::new(m)),
        Err(errors) => State::Broken(errors),
    }
}

impl Extensions {
    pub fn ready(&self) -> impl Iterator<Item = &Manifest> {
        self.packages.iter().filter_map(|p| match &p.state {
            State::Ready(m) => Some(m.as_ref()),
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

    /// true when `m` declares servers and the user allowed exactly those.
    pub fn allowed(&self, m: &Manifest) -> bool {
        !m.servers.is_empty() && self.trust.allows(&m.id, &lines(m))
    }

    /// the servers of allowed packages, for the language client.
    pub fn servers(&self) -> Vec<crate::lsp::ServerSpec> {
        self.ready()
            .filter(|m| self.allowed(m))
            .flat_map(|m| {
                m.servers.iter().map(|s| crate::lsp::ServerSpec {
                    language: s.language.clone(),
                    command: s.command.clone(),
                    args: s.args.clone(),
                    language_id: s.language_id.clone(),
                    package: Some(m.id.clone()),
                })
            })
            .collect()
    }

    /// packages that are off: broken or a second copy.
    pub fn broken(&self) -> usize {
        self.packages
            .iter()
            .filter(|p| !matches!(p.state, State::Ready(_)))
            .count()
    }

    /// a grammar name that lives as long as the app. each name is kept
    /// once, so reading the packages again adds nothing.
    fn intern(&mut self, name: &str) -> &'static str {
        if let Some(kept) = self.names.get(name) {
            return kept;
        }
        let kept: &'static str = Box::leak(name.to_owned().into_boxed_str());
        self.names.insert(kept);
        kept
    }

    /// abbreviation tables by language for the editor. tables of one
    /// language from several packages join; the later package wins a key.
    pub fn abbrevs(
        &mut self,
    ) -> std::collections::HashMap<&'static str, std::sync::Arc<crate::editor::abbrev::Abbrevs>>
    {
        let mut joined: Vec<AbbrevTable> = Vec::new();
        for p in &self.packages {
            if !matches!(p.state, State::Ready(_)) {
                continue;
            }
            for table in &p.abbrevs {
                match joined
                    .iter_mut()
                    .find(|t| t.language == table.language && t.leader == table.leader)
                {
                    Some(slot) => slot.pairs.extend(table.pairs.iter().cloned()),
                    None => joined.push(table.clone()),
                }
            }
        }
        joined
            .into_iter()
            .map(
                |AbbrevTable {
                     language,
                     leader,
                     pairs,
                 }| {
                    let name = syntax::bundled(&language).unwrap_or_else(|| self.intern(&language));
                    (
                        name,
                        std::sync::Arc::new(crate::editor::abbrev::Abbrevs::new(leader, pairs)),
                    )
                },
            )
            .collect()
    }

    /// every grammar the ready packages bring, first one per name.
    pub fn grammars(&self) -> Vec<&Grammar> {
        let mut out: Vec<&Grammar> = Vec::new();
        for p in &self.packages {
            if matches!(p.state, State::Ready(_)) {
                for g in &p.grammars {
                    if !out.iter().any(|o| o.name == g.name) {
                        out.push(g);
                    }
                }
            }
        }
        out
    }

    /// file types the ready packages add. a language borrows a bundled
    /// syntax by name; one that names none, or one not bundled, gets no
    /// colours yet and a note says so. the first package to claim an
    /// extension keeps it.
    pub fn associations(&mut self) -> (Associations, Vec<String>) {
        let mut out = Associations::default();
        let mut notes = Vec::new();
        let grammars: Vec<String> = self.grammars().iter().map(|g| g.name.clone()).collect();
        let named: Vec<(String, Option<&'static str>)> = grammars
            .iter()
            .map(|n| (n.clone(), Some(self.intern(n))))
            .collect();
        for m in self.ready() {
            for lang in &m.languages {
                let Some(name) = &lang.syntax else { continue };
                let package = named.iter().find(|(n, _)| n == name).and_then(|(_, s)| *s);
                let Some(syntax) = syntax::bundled(name).or(package) else {
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
    fn grammars_load_and_bad_ones_are_named() {
        let root = std::env::temp_dir().join(format!("ligase-ext-gram-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let head =
            "[package]\nid = \"org.example.g\"\nname = \"G\"\nversion = \"0.1.0\"\napi = \"1\"\n";
        let files = "[[contributions.syntaxes]]\nfile = \"syntax/good.sylven\"\n\
                     [[contributions.syntaxes]]\nfile = \"syntax/broken.sylven\"\n\
                     [[contributions.syntaxes]]\nfile = \"syntax/rust.sylven\"\n\
                     [[contributions.syntaxes]]\nfile = \"syntax/missing.sylven\"\n\
                     [[contributions.languages]]\nid = \"good\"\nextensions = [\"good\"]\nsyntax = \"goodlang\"\n";
        write(&root, "g", &format!("{head}{files}"));
        let syntax = root.join("g").join("syntax");
        std::fs::create_dir_all(&syntax).unwrap();
        std::fs::write(
            syntax.join("good.sylven"),
            "language {\n  id \"goodlang\"\n  extensions [\".good\"]\n  comment.line \"--\"\n}\n\ntokens {\n  keyword [\"let\"]\n  ident /[a-z]+/\n}\n",
        )
        .unwrap();
        std::fs::write(syntax.join("broken.sylven"), "language { id ").unwrap();
        std::fs::write(syntax.join("rust.sylven"), "language {\n  id \"rust\"\n}\n").unwrap();
        let (packages, _) = scan(&root);
        let _ = std::fs::remove_dir_all(&root);
        let p = &packages[0];
        assert!(matches!(p.state, State::Ready(_)));
        let names: Vec<&str> = p.grammars.iter().map(|g| g.name.as_str()).collect();
        assert_eq!(names, ["goodlang"]);
        assert_eq!(p.problems.len(), 3, "{:?}", p.problems);
        assert!(p.problems.iter().any(|w| w.contains("bundled")));
        let mut ext = Extensions {
            packages,
            ..Extensions::default()
        };
        let (assoc, notes) = ext.associations();
        assert_eq!(assoc.extensions.get("good"), Some(&"goodlang"));
        assert!(notes.is_empty(), "{notes:?}");
        // a second read keeps one copy of the name
        let (again, _) = ext.associations();
        assert!(std::ptr::eq(
            *assoc.extensions.get("good").unwrap(),
            *again.extensions.get("good").unwrap()
        ));
    }

    #[test]
    fn grammar_paths_stay_in_the_package() {
        let head =
            "[package]\nid = \"org.example.g\"\nname = \"G\"\nversion = \"0.1.0\"\napi = \"1\"\n";
        for bad in [
            "../x.sylven",
            "/abs.sylven",
            "a//b.sylven",
            "a/./b.sylven",
            "x.toml",
            "C:\\\\x.sylven",
        ] {
            let text = format!("{head}[[contributions.syntaxes]]\nfile = \"{bad}\"\n");
            assert!(manifest::parse(&text).is_err(), "{bad}");
        }
    }

    #[test]
    fn feeds_pick_their_field() {
        let r =
            serde_json::json!({ "rendered": "⊢ a", "goal": { "text": "x" }, "n": 3, "none": null });
        assert_eq!(pick(&r, "rendered").as_deref(), Some("⊢ a"));
        assert_eq!(pick(&r, "goal.text").as_deref(), Some("x"));
        assert_eq!(pick(&r, "n").as_deref(), Some("3"));
        assert_eq!(pick(&r, "none"), None);
        assert_eq!(pick(&r, "missing.deep"), None);
        assert_eq!(pick(&serde_json::Value::Null, ""), None);
        let long = serde_json::json!("가".repeat(MAX_FEED));
        let cut = pick(&long, "").unwrap();
        assert!(
            cut.len() <= MAX_FEED && cut.ends_with('가'),
            "cut on a char"
        );
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
        let mut ext = Extensions {
            packages: vec![Package {
                dir: PathBuf::new(),
                state: State::Ready(Box::new(manifest)),
                grammars: Vec::new(),
                problems: Vec::new(),
                abbrevs: Vec::new(),
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
