//! user settings from `settings.toml` in the config folder. every key is
//! optional; a missing or bad one keeps its default, and what was wrong is
//! said once, not fatal. the file is untrusted text like any other.

use std::path::PathBuf;

/// the file's name in the config folder.
pub const FILE: &str = "settings.toml";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ThemeChoice {
    Dark,
    Light,
    /// follows the system, and its changes.
    System,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineNumbers {
    On,
    /// distance from the caret's line, its own number on it.
    Relative,
    Off,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CaretStyle {
    Bar,
    Block,
    Underline,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Whitespace {
    None,
    /// dots and arrows only inside the selection.
    Selection,
    All,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
    pub theme: ThemeChoice,
    /// "en", "ko", or none for the system language.
    pub language: Option<String>,
    pub editor_font: Option<String>,
    pub editor_size: f32,
    /// line height as a multiple of the font size.
    pub line_height: f32,
    pub tab_size: usize,
    pub caret: CaretStyle,
    pub caret_blink: bool,
    /// the caret glides to its place; off when the system asks for less
    /// motion too.
    pub smooth_caret: bool,
    pub line_numbers: LineNumbers,
    pub current_line: bool,
    pub indent_guides: bool,
    pub whitespace: Whitespace,
    /// other copies of the word under the caret lit.
    pub word_highlight: bool,
    /// brackets and quotes typed in pairs.
    pub auto_pairs: bool,
    pub terminal_size: f32,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            theme: ThemeChoice::Dark,
            language: None,
            editor_font: None,
            editor_size: 14.0,
            line_height: 1.45,
            tab_size: 4,
            caret: CaretStyle::Bar,
            caret_blink: true,
            smooth_caret: true,
            line_numbers: LineNumbers::On,
            current_line: true,
            indent_guides: true,
            whitespace: Whitespace::Selection,
            word_highlight: true,
            auto_pairs: true,
            terminal_size: 13.0,
        }
    }
}

/// the settings file, when there is a config folder.
pub fn path() -> Option<PathBuf> {
    aurea::config_dir("ligase").map(|d| d.join(FILE))
}

/// what a new settings file starts with: every key at its default,
/// commented out, so the file shows what can be set.
pub const STARTER: &str = r#"# ligase settings. remove the # in front of a line to change it.
# saved changes apply at once.

# theme = "dark"            # "dark", "light" or "system"
# language = "en"           # "en" or "ko"; the system language when unset
# editor_font = "Consolas"
# editor_size = 14
# line_height = 1.45
# tab_size = 4
# caret = "bar"             # "bar", "block" or "underline"
# caret_blink = true
# smooth_caret = true
# line_numbers = "on"       # "on", "relative" or "off"
# current_line = true
# indent_guides = true
# whitespace = "selection"  # "none", "selection" or "all"
# word_highlight = true
# auto_pairs = true
# terminal_size = 13
"#;

/// reads settings from `text`. unknown keys and bad values are skipped;
/// the second part says what was skipped, for the status bar.
pub fn parse(text: &str) -> (Settings, Vec<String>) {
    let mut out = Settings::default();
    let mut problems = Vec::new();
    let table = match text.parse::<toml::Table>() {
        Ok(table) => table,
        Err(e) => {
            let first = e.to_string();
            let first = first.lines().next().unwrap_or_default().to_owned();
            return (out, vec![first]);
        }
    };
    for (key, value) in &table {
        let ok = apply(&mut out, key, value);
        if !ok {
            problems.push(format!("{key} = {value}"));
        }
    }
    (out, problems)
}

fn word<'a>(value: &'a toml::Value, choices: &[&str]) -> Option<&'a str> {
    value.as_str().filter(|s| choices.contains(s))
}

fn number(value: &toml::Value, min: f64, max: f64) -> Option<f32> {
    let n = value
        .as_float()
        .or_else(|| value.as_integer().map(|i| i as f64))?;
    (min..=max).contains(&n).then_some(n as f32)
}

/// sets one key. false when the key is unknown or the value doesn't fit.
fn apply(out: &mut Settings, key: &str, value: &toml::Value) -> bool {
    match key {
        "theme" => match word(value, &["dark", "light", "system"]) {
            Some("dark") => out.theme = ThemeChoice::Dark,
            Some("light") => out.theme = ThemeChoice::Light,
            Some(_) => out.theme = ThemeChoice::System,
            None => return false,
        },
        "language" => match word(value, &["en", "ko"]) {
            Some(lang) => out.language = Some(lang.to_owned()),
            None => return false,
        },
        "editor_font" => match value.as_str().filter(|s| !s.is_empty() && s.len() <= 100) {
            Some(font) => out.editor_font = Some(font.to_owned()),
            None => return false,
        },
        "editor_size" => match number(value, 6.0, 72.0) {
            Some(n) => out.editor_size = n,
            None => return false,
        },
        "line_height" => match number(value, 1.0, 3.0) {
            Some(n) => out.line_height = n,
            None => return false,
        },
        "tab_size" => match number(value, 1.0, 16.0) {
            Some(n) if n.fract() == 0.0 => out.tab_size = n as usize,
            _ => return false,
        },
        "caret" => match word(value, &["bar", "block", "underline"]) {
            Some("bar") => out.caret = CaretStyle::Bar,
            Some("block") => out.caret = CaretStyle::Block,
            Some(_) => out.caret = CaretStyle::Underline,
            None => return false,
        },
        "line_numbers" => match word(value, &["on", "relative", "off"]) {
            Some("on") => out.line_numbers = LineNumbers::On,
            Some("relative") => out.line_numbers = LineNumbers::Relative,
            Some(_) => out.line_numbers = LineNumbers::Off,
            None => return false,
        },
        "whitespace" => match word(value, &["none", "selection", "all"]) {
            Some("none") => out.whitespace = Whitespace::None,
            Some("selection") => out.whitespace = Whitespace::Selection,
            Some(_) => out.whitespace = Whitespace::All,
            None => return false,
        },
        "terminal_size" => match number(value, 6.0, 72.0) {
            Some(n) => out.terminal_size = n,
            None => return false,
        },
        "caret_blink" | "smooth_caret" | "current_line" | "indent_guides" | "word_highlight"
        | "auto_pairs" => {
            let Some(on) = value.as_bool() else {
                return false;
            };
            let slot = match key {
                "caret_blink" => &mut out.caret_blink,
                "smooth_caret" => &mut out.smooth_caret,
                "current_line" => &mut out.current_line,
                "indent_guides" => &mut out.indent_guides,
                "word_highlight" => &mut out.word_highlight,
                _ => &mut out.auto_pairs,
            };
            *slot = on;
        }
        _ => return false,
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starter_file_is_all_defaults() {
        let (s, problems) = parse(STARTER);
        assert_eq!(s, Settings::default());
        assert!(problems.is_empty());
    }

    #[test]
    fn values_apply_and_bad_ones_are_named() {
        let text = "theme = \"light\"\neditor_size = 16\nline_numbers = \"relative\"\ntab_size = 2.5\nwat = 1\ncaret_blink = false";
        let (s, problems) = parse(text);
        assert_eq!(s.theme, ThemeChoice::Light);
        assert_eq!(s.editor_size, 16.0);
        assert_eq!(s.line_numbers, LineNumbers::Relative);
        assert!(!s.caret_blink);
        assert_eq!(s.tab_size, 4, "a bad value keeps the default");
        assert_eq!(problems.len(), 2);
    }

    #[test]
    fn broken_file_gives_defaults_and_why() {
        let (s, problems) = parse("theme = ");
        assert_eq!(s, Settings::default());
        assert_eq!(problems.len(), 1);
        let (s, _) = parse("editor_size = 9000");
        assert_eq!(s.editor_size, 14.0);
    }
}
