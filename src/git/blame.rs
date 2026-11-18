//! who last changed a line, and when: the note at the end of the caret's
//! line. read against the text as it is, so unsaved edits don't shift it.

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

/// the text sent to git for one blame; bigger files get no note.
pub const MAX_TEXT: usize = 4 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Blame {
    pub id: String,
    pub author: String,
    /// seconds since the epoch.
    pub time: i64,
    pub summary: String,
    /// the line isn't committed yet.
    pub uncommitted: bool,
}

/// line `line` (from 0) of `rel` in the repository at `top`, as `text`
/// has it now. runs on the worker.
pub fn line(top: &Path, rel: &str, line: usize, text: &str) -> Result<Blame, String> {
    if text.len() > MAX_TEXT {
        return Err("file too big to blame".to_owned());
    }
    let range = format!("{},{}", line + 1, line + 1);
    let mut cmd = Command::new("git");
    cmd.arg("--literal-pathspecs")
        .arg("-C")
        .arg(top)
        .args([
            "blame",
            "--porcelain",
            "-L",
            &range,
            "--contents",
            "-",
            "--",
            rel,
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let mut child = cmd.spawn().map_err(|e| format!("git: {e}"))?;
    if let Some(mut stdin) = child.stdin.take() {
        // git may stop reading early; a broken pipe is not our error
        let _ = stdin.write_all(text.as_bytes());
    }
    let out = child.wait_with_output().map_err(|e| format!("git: {e}"))?;
    if !out.status.success() {
        return Err("not blamed".to_owned());
    }
    parse(&String::from_utf8_lossy(&out.stdout)).ok_or_else(|| "not blamed".to_owned())
}

fn parse(out: &str) -> Option<Blame> {
    let mut lines = out.lines();
    let id = lines.next()?.split(' ').next()?.to_owned();
    if id.len() != 40 || !id.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let mut blame = Blame {
        uncommitted: id.bytes().all(|b| b == b'0'),
        id,
        author: String::new(),
        time: 0,
        summary: String::new(),
    };
    for l in lines {
        if let Some(a) = l.strip_prefix("author ") {
            blame.author = a.chars().take(80).collect();
        } else if let Some(t) = l.strip_prefix("author-time ") {
            blame.time = t.trim().parse().unwrap_or(0);
        } else if let Some(s) = l.strip_prefix("summary ") {
            blame.summary = s.chars().take(200).collect();
        } else if l.starts_with('\t') {
            break;
        }
    }
    Some(blame)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_porcelain() {
        let out = "0123456789abcdef0123456789abcdef01234567 3 3 1\nauthor Eira\nauthor-mail <e@x>\nauthor-time 1700000000\nsummary fix: keep it\nfilename a.rs\n\tlet x = 1;\n";
        let b = parse(out).unwrap();
        assert_eq!(b.author, "Eira");
        assert_eq!(b.time, 1_700_000_000);
        assert_eq!(b.summary, "fix: keep it");
        assert!(!b.uncommitted);
        let mine =
            parse("0000000000000000000000000000000000000000 1 1 1\nauthor Not Committed Yet\n")
                .unwrap();
        assert!(mine.uncommitted);
        assert!(parse("").is_none());
        assert!(parse("zz 1 1 1\n").is_none());
    }

    #[test]
    fn blames_a_line_of_this_repository() {
        let top = Path::new(env!("CARGO_MANIFEST_DIR"));
        let Ok(text) = std::fs::read_to_string(top.join("Cargo.toml")) else {
            return;
        };
        // no git here: nothing to check
        if let Ok(b) = line(top, "Cargo.toml", 0, &text) {
            assert!(!b.uncommitted);
            assert!(!b.author.is_empty());
        }
        let edited = format!("new first line\n{text}");
        if let Ok(b) = line(top, "Cargo.toml", 0, &edited) {
            assert!(b.uncommitted, "an unsaved line is nobody's yet");
        }
    }
}
