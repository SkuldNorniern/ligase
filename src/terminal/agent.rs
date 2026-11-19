//! coding agents that live in a terminal, like claude code or codex. ligase
//! starts the agent's own program in a terminal tab, in the workspace: it
//! signs in and keeps its own keys, ligase never sees them. what ligase adds
//! is telling the agent where to look: a mention of the file and lines.

use std::path::{Path, PathBuf};

/// the agents ligase knows how to start and mention files to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Agent {
    Claude,
    Codex,
    Gemini,
    OpenCode,
    Aider,
}

impl Agent {
    pub const ALL: [Self; 5] = [
        Self::Claude,
        Self::Codex,
        Self::Gemini,
        Self::OpenCode,
        Self::Aider,
    ];

    /// the settings word.
    pub fn word(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::Gemini => "gemini",
            Self::OpenCode => "opencode",
            Self::Aider => "aider",
        }
    }

    pub fn from_word(word: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|a| a.word() == word)
    }

    /// shown on the terminal tab.
    pub fn name(self) -> &'static str {
        match self {
            Self::Claude => "Claude Code",
            Self::Codex => "Codex",
            Self::Gemini => "Gemini",
            Self::OpenCode => "OpenCode",
            Self::Aider => "Aider",
        }
    }

    /// the program on PATH.
    fn program(self) -> &'static str {
        self.word()
    }

    /// what is typed into the agent to point it at `rel` (`/` between
    /// parts), and at lines `lines` (from 1) when given. ends in a space so
    /// the agent's own file picker closes and the person types on.
    pub fn mention(self, rel: &str, lines: Option<(usize, usize)>) -> String {
        let rel = if rel.contains(' ') {
            format!("\"{rel}\"")
        } else {
            rel.to_owned()
        };
        match (self, lines) {
            (Self::Claude, Some((a, b))) if a == b => format!("@{rel}#L{a} "),
            (Self::Claude, Some((a, b))) => format!("@{rel}#L{a}-{b} "),
            (Self::Aider, None) => format!("{rel} "),
            (Self::Aider, Some((a, b))) => format!("{rel} lines {a}-{b} "),
            (_, None) => format!("@{rel} "),
            (_, Some((a, b))) if a == b => format!("@{rel} (line {a}) "),
            (_, Some((a, b))) => format!("@{rel} (lines {a}-{b}) "),
        }
    }

    /// the program and arguments that start it: the program found on PATH,
    /// through `cmd /d /c` for a windows `.cmd` or `.bat` (npm installs
    /// those). none when it isn't installed. reads the disk: worker only.
    pub fn command(self) -> Option<(PathBuf, Vec<String>)> {
        let found = find_on_path(self.program())?;
        let script = found
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("cmd") || e.eq_ignore_ascii_case("bat"));
        if script {
            let shell =
                std::env::var_os("COMSPEC").map_or_else(|| PathBuf::from("cmd.exe"), PathBuf::from);
            return Some((
                shell,
                vec![
                    "/d".into(),
                    "/c".into(),
                    found.to_string_lossy().into_owned(),
                ],
            ));
        }
        Some((found, Vec::new()))
    }
}

/// most PATH entries looked through.
const MAX_PATH_DIRS: usize = 256;

/// `program` in the PATH folders, with the endings windows runs.
fn find_on_path(program: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    let endings: &[&str] = if cfg!(windows) {
        &["exe", "cmd", "bat"]
    } else {
        &[""]
    };
    std::env::split_paths(&path)
        .take(MAX_PATH_DIRS)
        .find_map(|dir| find_in(&dir, program, endings))
}

fn find_in(dir: &Path, program: &str, endings: &[&str]) -> Option<PathBuf> {
    endings.iter().find_map(|ending| {
        let file = if ending.is_empty() {
            dir.join(program)
        } else {
            dir.join(format!("{program}.{ending}"))
        };
        file.is_file().then_some(file)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn words_round_trip() {
        for agent in Agent::ALL {
            assert_eq!(Agent::from_word(agent.word()), Some(agent));
        }
        assert_eq!(Agent::from_word("nope"), None);
    }

    #[test]
    fn mentions_follow_each_agent() {
        assert_eq!(Agent::Claude.mention("src/a.rs", None), "@src/a.rs ");
        assert_eq!(
            Agent::Claude.mention("src/a.rs", Some((3, 3))),
            "@src/a.rs#L3 "
        );
        assert_eq!(
            Agent::Claude.mention("src/a.rs", Some((3, 9))),
            "@src/a.rs#L3-9 "
        );
        assert_eq!(
            Agent::Codex.mention("src/a.rs", Some((3, 9))),
            "@src/a.rs (lines 3-9) "
        );
        assert_eq!(
            Agent::Gemini.mention("my dir/a.rs", None),
            "@\"my dir/a.rs\" "
        );
        assert_eq!(Agent::Aider.mention("a.rs", None), "a.rs ");
    }

    #[test]
    fn programs_are_found_with_windows_endings() {
        let dir = std::env::temp_dir().join(format!("ligase-agent-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("tool.cmd"), "").unwrap();
        assert_eq!(
            find_in(&dir, "tool", &["exe", "cmd"]),
            Some(dir.join("tool.cmd"))
        );
        assert_eq!(find_in(&dir, "other", &["exe", "cmd"]), None);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
