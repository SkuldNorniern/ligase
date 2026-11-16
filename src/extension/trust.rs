//! the trust file: which packages may start programs, and the exact command
//! lines each one was allowed. the file is untrusted: sizes are capped, every
//! entry is checked, and bad input comes back as reasons, never panics.

use toml::{Table, Value};

/// largest trust file read.
pub const MAX_BYTES: usize = 64 * 1024;
/// grants kept at most.
pub const MAX_GRANTS: usize = 500;

const MAX_PACKAGE: usize = 100;
const MAX_LINES: usize = 10;
const MAX_PARTS: usize = 20;
const MAX_PART: usize = 200;

const HEADER: &str = "# packages allowed to start programs. remove a block to take it back.\n";

/// one package's grant: the command lines it may start, each as program + args.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Grant {
    pub package: String,
    pub commands: Vec<Vec<String>>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Trust {
    grants: Vec<Grant>,
}

impl Trust {
    /// reads the file's text. bad or oversized text gives an empty trust and the reasons; it never panics.
    pub fn read(text: &str) -> (Trust, Vec<String>) {
        if text.len() > MAX_BYTES {
            return (
                Trust::default(),
                vec![format!(
                    "trust file is {} bytes, the limit is {MAX_BYTES}",
                    text.len()
                )],
            );
        }
        let root: Table = match toml::from_str(text) {
            Ok(root) => root,
            Err(e) => return (Trust::default(), vec![format!("toml: {e}")]),
        };
        let mut trust = Trust::default();
        let mut reasons = Vec::new();
        let Some(value) = root.get("grant") else {
            return (trust, reasons);
        };
        let Value::Array(items) = value else {
            return (
                Trust::default(),
                vec!["grant: expected an array of tables".to_owned()],
            );
        };
        if items.len() > MAX_GRANTS {
            reasons.push(format!(
                "grant: {} entries, only the first {MAX_GRANTS} kept",
                items.len()
            ));
        }
        for (i, item) in items.iter().take(MAX_GRANTS).enumerate() {
            let path = format!("grant[{i}]");
            match parse_grant(item) {
                Ok(grant) => match trust.grants.iter_mut().find(|g| g.package == grant.package) {
                    Some(old) => {
                        reasons.push(format!(
                            "{path}: {} is listed twice, keeps the last",
                            grant.package
                        ));
                        *old = grant;
                    }
                    None => trust.grants.push(grant),
                },
                Err(why) => reasons.push(format!("{path}: {why}")),
            }
        }
        (trust, reasons)
    }

    /// the file's text. reading it back gives the same trust.
    pub fn write(&self) -> String {
        let mut root = Table::new();
        if !self.grants.is_empty() {
            let grants = self.grants.iter().map(grant_value).collect();
            root.insert("grant".to_owned(), Value::Array(grants));
        }
        // a table of strings and arrays always serializes, so the default
        // is never taken in practice.
        let body = toml::to_string(&root).unwrap_or_default();
        format!("{HEADER}{body}")
    }

    /// true when `package` was allowed exactly these command lines. order of the list matters not; each line's program and args must match exactly.
    pub fn allows(&self, package: &str, commands: &[Vec<String>]) -> bool {
        // nothing to start means nothing to allow.
        if commands.is_empty() {
            return true;
        }
        let Some(grant) = self.grants.iter().find(|g| g.package == package) else {
            return false;
        };
        let mut want: Vec<&Vec<String>> = commands.iter().collect();
        let mut have: Vec<&Vec<String>> = grant.commands.iter().collect();
        want.sort();
        have.sort();
        want == have
    }

    /// records a grant, replacing any older one for that package.
    pub fn allow(&mut self, package: &str, commands: Vec<Vec<String>>) {
        let grant = Grant {
            package: package.to_owned(),
            commands,
        };
        match self.grants.iter_mut().find(|g| g.package == package) {
            Some(old) => *old = grant,
            None => self.grants.push(grant),
        }
    }

    /// drops a package's grant.
    pub fn revoke(&mut self, package: &str) {
        self.grants.retain(|g| g.package != package);
    }
}

/// the trust file in the config folder.
pub fn path() -> Option<std::path::PathBuf> {
    aurea::config_dir("ligase").map(|d| d.join("trust.toml"))
}

fn parse_grant(value: &Value) -> Result<Grant, String> {
    let Value::Table(table) = value else {
        return Err("expected a table".to_owned());
    };
    let package = match table.get("package") {
        Some(Value::String(s)) if (1..=MAX_PACKAGE).contains(&s.len()) => s.clone(),
        Some(Value::String(_)) => return Err(format!("package must be 1-{MAX_PACKAGE} bytes")),
        Some(_) => return Err("package must be a string".to_owned()),
        None => return Err("package is missing".to_owned()),
    };
    let lines = match table.get("commands") {
        Some(Value::Array(lines)) => lines,
        Some(_) => return Err("commands must be an array of command lines".to_owned()),
        None => return Err("commands is missing".to_owned()),
    };
    if lines.len() > MAX_LINES {
        return Err(format!(
            "at most {MAX_LINES} command lines, got {}",
            lines.len()
        ));
    }
    let commands = lines
        .iter()
        .map(parse_line)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Grant { package, commands })
}

fn parse_line(value: &Value) -> Result<Vec<String>, String> {
    let Value::Array(parts) = value else {
        return Err("a command line must be an array of strings".to_owned());
    };
    if !(1..=MAX_PARTS).contains(&parts.len()) {
        return Err(format!(
            "a command line has 1-{MAX_PARTS} parts, got {}",
            parts.len()
        ));
    }
    let mut out = Vec::with_capacity(parts.len());
    for part in parts {
        let Value::String(s) = part else {
            return Err("command parts must be strings".to_owned());
        };
        if s.is_empty() || s.len() > MAX_PART {
            return Err(format!("command parts are 1-{MAX_PART} bytes"));
        }
        if s.chars().any(char::is_control) {
            return Err("command parts have no control chars".to_owned());
        }
        out.push(s.clone());
    }
    Ok(out)
}

fn grant_value(grant: &Grant) -> Value {
    let mut table = Table::new();
    table.insert("package".to_owned(), Value::String(grant.package.clone()));
    let lines = grant
        .commands
        .iter()
        .map(|line| {
            Value::Array(
                line.iter()
                    .map(|part| Value::String(part.clone()))
                    .collect(),
            )
        })
        .collect();
    table.insert("commands".to_owned(), Value::Array(lines));
    Value::Table(table)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|p| (*p).to_owned()).collect()
    }

    fn has(list: &[String], needle: &str) -> bool {
        list.iter().any(|m| m.contains(needle))
    }

    #[test]
    fn round_trip_gives_the_same_trust() {
        let mut t = Trust::default();
        t.allow("org.leanprover.lean4", vec![line(&["lake", "serve"])]);
        t.allow(
            "org.example.other",
            vec![line(&["x", "--flag=\"a\\b\""]), line(&["y"])],
        );
        let (back, reasons) = Trust::read(&t.write());
        assert!(reasons.is_empty(), "{reasons:?}");
        assert_eq!(back, t);
    }

    #[test]
    fn allows_matches_exact_lines_in_any_order() {
        let mut t = Trust::default();
        t.allow("a.b", vec![line(&["lake", "serve"]), line(&["x"])]);
        assert!(t.allows("a.b", &[line(&["x"]), line(&["lake", "serve"])]));
        assert!(!t.allows("a.b", &[line(&["lake", "serv"]), line(&["x"])]));
        assert!(!t.allows(
            "a.b",
            &[line(&["lake", "serve"]), line(&["x"]), line(&["y"])]
        ));
        assert!(!t.allows("a.b", &[line(&["lake", "serve"])]));
        assert!(!t.allows("c.d", &[line(&["lake", "serve"])]));
        assert!(t.allows("c.d", &[]));
    }

    #[test]
    fn allow_replaces_and_revoke_removes() {
        let mut t = Trust::default();
        t.allow("a.b", vec![line(&["one"])]);
        t.allow("a.b", vec![line(&["two"])]);
        assert_eq!(t.grants.len(), 1);
        assert!(t.allows("a.b", &[line(&["two"])]));
        assert!(!t.allows("a.b", &[line(&["one"])]));
        t.revoke("a.b");
        assert!(t.grants.is_empty());
        assert!(!t.allows("a.b", &[line(&["two"])]));
    }

    #[test]
    fn bad_entries_are_skipped_with_reasons() {
        let parts = format!("[{}]", vec!["\"p\""; 21].join(", "));
        let text = format!(
            "[[grant]]\ncommands = [[\"lake\"]]\n\n\
             [[grant]]\npackage = \"a.bad\"\ncommands = \"lake\"\n\n\
             [[grant]]\npackage = \"a.empty\"\ncommands = [[]]\n\n\
             [[grant]]\npackage = \"a.ctrl\"\ncommands = [[\"a\\tb\"]]\n\n\
             [[grant]]\npackage = \"a.parts\"\ncommands = [{parts}]\n\n\
             [[grant]]\npackage = \"a.good\"\ncommands = [[\"lake\", \"serve\"]]\n"
        );
        let (t, reasons) = Trust::read(&text);
        assert_eq!(reasons.len(), 5, "{reasons:?}");
        assert!(has(&reasons, "grant[0]: package is missing"), "{reasons:?}");
        assert!(has(&reasons, "grant[1]: commands must be"), "{reasons:?}");
        assert!(
            has(&reasons, "grant[2]: a command line has 1-20"),
            "{reasons:?}"
        );
        assert!(
            has(&reasons, "grant[3]: command parts have no control"),
            "{reasons:?}"
        );
        assert!(
            has(&reasons, "grant[4]: a command line has 1-20"),
            "{reasons:?}"
        );
        assert_eq!(t.grants.len(), 1);
        assert!(t.allows("a.good", &[line(&["lake", "serve"])]));
    }

    #[test]
    fn oversized_text_gives_empty_trust_and_a_reason() {
        let text = " ".repeat(MAX_BYTES + 1);
        let (t, reasons) = Trust::read(&text);
        assert!(t.grants.is_empty());
        assert_eq!(reasons.len(), 1);
        assert!(reasons[0].contains("bytes"), "{reasons:?}");
    }

    #[test]
    fn malformed_toml_gives_empty_trust_and_a_reason() {
        let (t, reasons) = Trust::read("[[grant\npackage =");
        assert!(t.grants.is_empty());
        assert!(has(&reasons, "toml:"), "{reasons:?}");
    }

    #[test]
    fn truncated_text_never_panics() {
        let mut t = Trust::default();
        t.allow("org.example.one", vec![line(&["lake", "serve"])]);
        t.allow("org.example.two", vec![line(&["x", "é"])]);
        let text = t.write();
        for i in 0..=text.len() {
            if text.is_char_boundary(i) {
                let _ = Trust::read(&text[..i]);
            }
        }
    }
}
