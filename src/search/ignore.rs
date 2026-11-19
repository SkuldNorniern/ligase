//! `.gitignore` rules, enough of them for walking a workspace: `*`, `?`,
//! `**`, `[a-z]` classes, `!` to take a file back, a trailing `/` for
//! folders only, and a `/` before or inside to tie a rule to its folder.
//! the files are untrusted: size and rule caps, no recursion in matching
//! deeper than the pattern.

use std::path::Path;
use std::sync::Arc;

/// biggest ignore file read; past this the rest is left out.
const MAX_FILE: u64 = 256 * 1024;
/// most rules kept from one file.
const MAX_RULES: usize = 4096;
/// longest pattern kept.
const MAX_PATTERN: usize = 1024;

#[derive(Debug)]
struct Rule {
    glob: Vec<u8>,
    negate: bool,
    folder_only: bool,
    /// tied to the folder the file is in: matched against the whole path
    /// from there, not just the name.
    anchored: bool,
}

/// the rules of one ignore file, and the folder they count from.
#[derive(Debug)]
pub struct Rules {
    base: std::path::PathBuf,
    rules: Vec<Rule>,
}

impl Rules {
    /// reads `file` for rules counted from `base`. none when it can't be read
    /// or holds no rule.
    pub fn read(base: &Path, file: &Path) -> Option<Arc<Self>> {
        use std::io::Read;
        let mut text = String::new();
        std::fs::File::open(file)
            .ok()?
            .take(MAX_FILE)
            .read_to_string(&mut text)
            .ok()?;
        let rules = Self::parse(base, &text);
        (!rules.rules.is_empty()).then(|| Arc::new(rules))
    }

    pub fn parse(base: &Path, text: &str) -> Self {
        let mut rules = Vec::new();
        for line in text.lines() {
            if rules.len() >= MAX_RULES {
                break;
            }
            let mut line = line.trim_end_matches([' ', '\t', '\r']);
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let negate = line.starts_with('!');
            if negate {
                line = &line[1..];
            }
            // `\#` and `\!` stand for the character itself
            if line.starts_with('\\') {
                line = &line[1..];
            }
            let folder_only = line.ends_with('/');
            let line = line.trim_end_matches('/');
            let anchored = line.contains('/');
            let line = line.trim_start_matches('/');
            if line.is_empty() || line.len() > MAX_PATTERN {
                continue;
            }
            rules.push(Rule {
                glob: line.as_bytes().to_vec(),
                negate,
                folder_only,
                anchored,
            });
        }
        Self {
            base: base.to_path_buf(),
            rules,
        }
    }

    /// what these rules say of `path`: `Some(true)` ignored, `Some(false)`
    /// taken back by a `!` rule, none when no rule speaks of it.
    fn verdict(&self, path: &Path, folder: bool) -> Option<bool> {
        let rel = path.strip_prefix(&self.base).ok()?;
        let rel = rel.to_string_lossy().replace('\\', "/");
        let name = rel.rsplit('/').next().unwrap_or(&rel);
        // the last rule that matches wins
        self.rules.iter().rev().find_map(|rule| {
            if rule.folder_only && !folder {
                return None;
            }
            let hit = if rule.anchored {
                glob(&rule.glob, rel.as_bytes())
            } else {
                glob(&rule.glob, name.as_bytes())
            };
            hit.then_some(!rule.negate)
        })
    }
}

/// every ignore file that counts for a folder, outermost first.
#[derive(Clone, Debug, Default)]
pub struct Stack(Vec<Arc<Rules>>);

impl Stack {
    /// this stack with `rules` on top, for a folder below.
    pub fn with(&self, rules: Arc<Rules>) -> Self {
        let mut more = self.0.clone();
        more.push(rules);
        Self(more)
    }

    /// whether `path` is ignored: the deepest file that speaks of it wins.
    pub fn ignored(&self, path: &Path, folder: bool) -> bool {
        self.0
            .iter()
            .rev()
            .find_map(|rules| rules.verdict(path, folder))
            .unwrap_or(false)
    }
}

/// shell-style match of `pat` against all of `text`: `*` and `?` stay within
/// one path part, `**` crosses parts. two-pointer with one backtrack point per
/// star kind, so the time stays near pattern times text.
fn glob(pat: &[u8], text: &[u8]) -> bool {
    let (mut p, mut t) = (0, 0);
    // where to go back to: after the last `*` (part-bound) or `**`
    let mut star: Option<(usize, usize)> = None;
    let mut globstar: Option<(usize, usize)> = None;
    while t < text.len() {
        if p < pat.len() {
            match pat[p] {
                b'*' if pat.get(p + 1) == Some(&b'*') => {
                    // `**/` matches no folder at all, too
                    let mut next = p + 2;
                    if pat.get(next) == Some(&b'/') {
                        next += 1;
                    }
                    globstar = Some((next, t));
                    star = None;
                    p = next;
                    continue;
                }
                b'*' => {
                    star = Some((p + 1, t));
                    p += 1;
                    continue;
                }
                b'?' if text[t] != b'/' => {
                    p += 1;
                    t += 1;
                    continue;
                }
                b'[' => {
                    if let Some((hit, end)) = class(&pat[p..], text[t])
                        && hit
                    {
                        p += end;
                        t += 1;
                        continue;
                    }
                }
                c if c == text[t] => {
                    p += 1;
                    t += 1;
                    continue;
                }
                _ => {}
            }
        }
        // no match here: let the last star take one more byte
        if let Some((sp, st)) = star
            && text[st] != b'/'
        {
            star = Some((sp, st + 1));
            p = sp;
            t = st + 1;
            continue;
        }
        if let Some((gp, gt)) = globstar {
            globstar = Some((gp, gt + 1));
            star = None;
            p = gp;
            t = gt + 1;
            continue;
        }
        return false;
    }
    // what's left of the pattern must match nothing
    while p < pat.len() && pat[p] == b'*' {
        p += 1;
    }
    p == pat.len()
}

/// a `[...]` class at the start of `pat` against `c`: whether it matched and
/// how long the class is. none when it never closes.
fn class(pat: &[u8], c: u8) -> Option<(bool, usize)> {
    let mut i = 1;
    let negate = matches!(pat.get(i), Some(b'!' | b'^'));
    if negate {
        i += 1;
    }
    let mut hit = false;
    let mut first = true;
    while i < pat.len() {
        if pat[i] == b']' && !first {
            return Some((hit != negate, i + 1));
        }
        first = false;
        if pat.get(i + 1) == Some(&b'-') && pat.get(i + 2).is_some_and(|&e| e != b']') {
            hit |= (pat[i]..=pat[i + 2]).contains(&c);
            i += 3;
        } else {
            hit |= pat[i] == c;
            i += 1;
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn globs() {
        assert!(glob(b"*.rs", b"main.rs"));
        assert!(!glob(b"*.rs", b"src/main.rs"));
        assert!(glob(b"src/*.rs", b"src/main.rs"));
        assert!(glob(b"**/build", b"a/b/build"));
        assert!(glob(b"**/build", b"build"));
        assert!(glob(b"a/**/z", b"a/z"));
        assert!(glob(b"a/**/z", b"a/b/c/z"));
        assert!(glob(b"a/**", b"a/b/c"));
        assert!(glob(b"?.txt", b"a.txt"));
        assert!(!glob(b"?.txt", b"/.txt"));
        assert!(glob(b"[abc].md", b"b.md"));
        assert!(glob(b"[a-c].md", b"b.md"));
        assert!(!glob(b"[!a-c].md", b"b.md"));
        assert!(glob(b"[!a-c].md", b"x.md"));
        assert!(!glob(b"[abc", b"a"), "an open class matches nothing");
        // a long run of stars stays quick
        let text = vec![b'a'; 4000];
        assert!(!glob(b"*a*a*a*a*a*a*a*a*b", &text));
    }

    #[test]
    fn rules_follow_git() {
        let base = Path::new("/w");
        let rules = Rules::parse(
            base,
            "# comment\n\ntarget/\n*.log\n!keep.log\n/root.txt\ndocs/*.tmp\n\\#hash\n",
        );
        let stack = Stack::default().with(Arc::new(rules));
        assert!(stack.ignored(Path::new("/w/target"), true));
        assert!(
            !stack.ignored(Path::new("/w/target"), false),
            "folders only"
        );
        assert!(stack.ignored(Path::new("/w/a/b/x.log"), false));
        assert!(
            !stack.ignored(Path::new("/w/keep.log"), false),
            "taken back"
        );
        assert!(stack.ignored(Path::new("/w/root.txt"), false));
        assert!(
            !stack.ignored(Path::new("/w/sub/root.txt"), false),
            "tied to its folder"
        );
        assert!(stack.ignored(Path::new("/w/docs/a.tmp"), false));
        assert!(!stack.ignored(Path::new("/w/docs/sub/a.tmp"), false));
        assert!(stack.ignored(Path::new("/w/#hash"), false));
        assert!(!stack.ignored(Path::new("/w/main.rs"), false));
    }

    #[test]
    fn a_deeper_file_wins() {
        let top = Arc::new(Rules::parse(Path::new("/w"), "*.gen\n"));
        let sub = Arc::new(Rules::parse(Path::new("/w/sub"), "!*.gen\n"));
        let stack = Stack::default().with(top).with(sub);
        assert!(!stack.ignored(Path::new("/w/sub/a.gen"), false));
        assert!(stack.ignored(Path::new("/w/b.gen"), false));
    }
}
