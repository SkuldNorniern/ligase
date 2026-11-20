//! user settings from `settings.toml` in the config folder. every key is
//! optional; a missing or bad one keeps its default, and what was wrong is
//! said once, not fatal. the file is untrusted text like any other.

use std::path::PathBuf;

pub mod view;

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

/// where the rail of panel buttons sits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RailPlace {
    Left,
    Right,
    Top,
    Bottom,
}

/// when the rail is in sight.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RailShow {
    Always,
    Hidden,
    /// slides out over the work when the pointer reaches its edge.
    Edge,
}

/// which edge the side bar sits on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SidePlace {
    Left,
    Right,
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
    /// the file's place in the folder, above the text.
    pub breadcrumbs: bool,
    /// marks for problems and find hits beside the scroll bar.
    pub overview: bool,
    /// the lines that open the scopes the top line is in stay on top.
    pub sticky_scroll: bool,
    /// the language server formats a file before it is saved.
    pub format_on_save: bool,
    pub whitespace: Whitespace,
    /// other copies of the word under the caret lit.
    pub word_highlight: bool,
    /// brackets and quotes typed in pairs.
    pub auto_pairs: bool,
    pub terminal_size: f32,
    /// the coding agent the agent panel talks to.
    pub agent: crate::agent::Agent,
    /// a command that speaks acp in place of the agent's own; empty for none.
    pub agent_command: Option<String>,
    /// the built-in agent's server, up to `/chat/completions`.
    pub openai_base_url: String,
    /// the built-in agent's model; none until set up.
    pub openai_model: Option<String>,
    /// the model's context in tokens; auto compact starts at three quarters.
    pub openai_context: f32,
    /// language servers set here, `[servers.<language>]`: new ones, or the
    /// built-in and package ones changed or turned off.
    pub servers: Vec<ServerRule>,
    /// spaces and tabs at line ends go when saving.
    pub trim_trailing_whitespace: bool,
    /// a saved file ends in a line break.
    pub final_newline: bool,
    /// files with edits are saved when the window loses focus.
    pub save_on_focus_loss: bool,
    /// long lines go on in the next row instead of past the right edge.
    pub word_wrap: bool,
    /// where the rail of panel buttons sits.
    pub rail: RailPlace,
    /// when the rail shows; its edge is the one it sits on.
    pub rail_show: RailShow,
    /// which edge the side bar sits on.
    pub side_bar: SidePlace,
    /// the side bar floats over the editor and folds away when the editor is clicked.
    pub side_overlay: bool,
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
            breadcrumbs: true,
            overview: true,
            sticky_scroll: true,
            format_on_save: false,
            whitespace: Whitespace::Selection,
            word_highlight: true,
            auto_pairs: true,
            terminal_size: 13.0,
            agent: crate::agent::Agent::Claude,
            agent_command: None,
            openai_base_url: "https://api.openai.com/v1".to_owned(),
            openai_model: None,
            openai_context: 32_000.0,
            servers: Vec::new(),
            trim_trailing_whitespace: false,
            final_newline: false,
            save_on_focus_loss: false,
            word_wrap: false,
            rail: RailPlace::Left,
            rail_show: RailShow::Always,
            side_bar: SidePlace::Left,
            side_overlay: false,
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
# breadcrumbs = true
# overview = true          # problem and find marks by the scroll bar
# sticky_scroll = true     # the lines a scope opens on stay on top
# format_on_save = false   # the language server formats before a save
# whitespace = "selection"  # "none", "selection" or "all"
# word_highlight = true
# auto_pairs = true
# terminal_size = 13
# agent = "claude"          # "claude", "codex", "gemini" or "opencode"
# agent_command = ""        # a program and args that speak acp, in place of the agent's own
# openai_base_url = "https://api.openai.com/v1"  # any openai-compatible server, for agent = "openai"
# openai_model = ""         # the model it asks; the key lives in the agent-key file
# openai_context = 32000    # the model's context in tokens; older talk is summarized past 3/4

# a language server, new or in place of the one ligase knows:
# [servers.rust]
# command = "rust-analyzer"
# args = []
# roots = ["Cargo.lock", "Cargo.toml"]   # where it starts: the first found above the file
# files = ["*.rs"]                       # file names it takes: `*.ext` or a whole name
# disabled = false
# trim_trailing_whitespace = false
# final_newline = false
# save_on_focus_loss = false
# word_wrap = false         # alt+z switches it for now
# rail = "left"            # "left", "right", "top", "bottom" or "hidden"
# side_bar = "left"        # "left" or "right"
# side_overlay = false     # the side bar floats over the editor and folds away on a click
"#;

/// reads settings from `text`. unknown keys and bad values are skipped;
/// the second part says what was skipped, for the status bar. none when the
/// file isn't toml at all: then the settings in force stay as they are.
pub fn parse(text: &str) -> (Option<Settings>, Vec<String>) {
    let mut out = Settings::default();
    let mut problems = Vec::new();
    let table = match text.parse::<toml::Table>() {
        Ok(table) => table,
        Err(e) => {
            let first = e.to_string();
            let first = first.lines().next().unwrap_or_default().to_owned();
            return (None, vec![first]);
        }
    };
    for (key, value) in &table {
        if key == "servers" {
            match value.as_table() {
                Some(servers) => out.servers = server_rules(servers, &mut problems),
                None => problems.push("servers must be a table".to_owned()),
            }
            continue;
        }
        let ok = apply(&mut out, key, value);
        if !ok {
            problems.push(format!("{key} = {value}"));
        }
    }
    (Some(out), problems)
}

/// a language server from the settings file.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ServerRule {
    /// the language it serves: a bundled syntax name like `rust`, or a new
    /// one its `files` bring in.
    pub language: String,
    pub command: String,
    pub args: Vec<String>,
    /// file names that mark the folder it starts in, most wanted first.
    pub roots: Vec<String>,
    /// file names it takes: `*.ext`, or a whole name like `Justfile`.
    pub files: Vec<String>,
    /// the language gets no server, not even the built-in one.
    pub disabled: bool,
}

/// most server tables read.
const MAX_SERVERS: usize = 64;
/// most items in one list.
const MAX_ITEMS: usize = 64;
/// longest text in a rule.
const MAX_TEXT: usize = 300;

/// a language name a table may use: short, plain characters.
pub fn server_language_ok(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 40
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'+' | b'#' | b'.'))
}

fn server_rules(table: &toml::Table, problems: &mut Vec<String>) -> Vec<ServerRule> {
    let mut out = Vec::new();
    for (language, value) in table.iter().take(MAX_SERVERS) {
        let Some(t) = value.as_table().filter(|_| server_language_ok(language)) else {
            problems.push(format!("servers.{language}"));
            continue;
        };
        let text = |key: &str| {
            t.get(key)
                .and_then(toml::Value::as_str)
                .map(str::trim)
                .filter(|s| s.len() <= MAX_TEXT)
                .unwrap_or("")
                .to_owned()
        };
        let list = |key: &str| -> Vec<String> {
            t.get(key)
                .and_then(toml::Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(toml::Value::as_str)
                .filter(|s| !s.is_empty() && s.len() <= MAX_TEXT)
                .take(MAX_ITEMS)
                .map(str::to_owned)
                .collect()
        };
        let rule = ServerRule {
            language: language.clone(),
            command: text("command"),
            args: list("args"),
            roots: list("roots"),
            files: list("files"),
            disabled: t
                .get("disabled")
                .and_then(toml::Value::as_bool)
                .unwrap_or(false),
        };
        if rule.command.is_empty()
            && !rule.disabled
            && rule.files.is_empty()
            && rule.roots.is_empty()
        {
            problems.push(format!("servers.{language}: no command"));
            continue;
        }
        out.push(rule);
    }
    out
}

/// `text` with `language`'s server table put in, or taken out when `rule`
/// is none. the rest of the file stays as it was written.
pub fn with_server(text: &str, language: &str, rule: Option<&ServerRule>) -> String {
    // quoted: `c++`, `c#` or a `.` are no bare toml keys, and one written
    // bare would stop the whole file from reading
    let header = format!("[servers.{}]", quote(language));
    let bare = format!("[servers.{language}]");
    let mut out = String::new();
    let mut skipping = false;
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            skipping = trimmed == header || trimmed == bare;
        }
        if !skipping {
            out.push_str(line);
            out.push('\n');
        }
    }
    let Some(rule) = rule else { return out };
    let list = |items: &[String]| {
        let quoted: Vec<String> = items.iter().map(|i| quote(i)).collect();
        format!("[{}]", quoted.join(", "))
    };
    while out.ends_with("\n\n") {
        out.pop();
    }
    out.push('\n');
    out.push_str(&header);
    out.push('\n');
    if !rule.command.is_empty() {
        out.push_str(&format!("command = {}\n", quote(&rule.command)));
    }
    if !rule.args.is_empty() {
        out.push_str(&format!("args = {}\n", list(&rule.args)));
    }
    if !rule.roots.is_empty() {
        out.push_str(&format!("roots = {}\n", list(&rule.roots)));
    }
    if !rule.files.is_empty() {
        out.push_str(&format!("files = {}\n", list(&rule.files)));
    }
    if rule.disabled {
        out.push_str("disabled = true\n");
    }
    out
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
        "openai_base_url" => {
            match value.as_str().map(str::trim).filter(|s| {
                s.len() <= 500 && (s.starts_with("http://") || s.starts_with("https://"))
            }) {
                Some(url) => out.openai_base_url = url.trim_end_matches('/').to_owned(),
                None => return false,
            }
        }
        "openai_model" => match value.as_str().map(str::trim).filter(|s| s.len() <= 200) {
            Some("") => out.openai_model = None,
            Some(m) => out.openai_model = Some(m.to_owned()),
            None => return false,
        },
        "openai_context" => match number(value, 2_000.0, 2_000_000.0) {
            Some(n) => out.openai_context = n,
            None => return false,
        },
        "agent_command" => match value.as_str().filter(|s| s.len() <= 1000) {
            Some(c) if c.trim().is_empty() => out.agent_command = None,
            Some(c) => out.agent_command = Some(c.trim().to_owned()),
            None => return false,
        },
        "agent" => match word(value, &["claude", "codex", "gemini", "opencode", "openai"])
            .and_then(crate::agent::Agent::from_word)
        {
            Some(agent) => out.agent = agent,
            None => return false,
        },
        "terminal_size" => match number(value, 6.0, 72.0) {
            Some(n) => out.terminal_size = n,
            None => return false,
        },
        "caret_blink"
        | "smooth_caret"
        | "current_line"
        | "indent_guides"
        | "breadcrumbs"
        | "overview"
        | "sticky_scroll"
        | "format_on_save"
        | "word_highlight"
        | "auto_pairs"
        | "trim_trailing_whitespace"
        | "final_newline"
        | "save_on_focus_loss"
        | "word_wrap"
        | "side_overlay" => {
            let Some(on) = value.as_bool() else {
                return false;
            };
            let slot = match key {
                "caret_blink" => &mut out.caret_blink,
                "smooth_caret" => &mut out.smooth_caret,
                "current_line" => &mut out.current_line,
                "indent_guides" => &mut out.indent_guides,
                "breadcrumbs" => &mut out.breadcrumbs,
                "overview" => &mut out.overview,
                "sticky_scroll" => &mut out.sticky_scroll,
                "format_on_save" => &mut out.format_on_save,
                "word_highlight" => &mut out.word_highlight,
                "trim_trailing_whitespace" => &mut out.trim_trailing_whitespace,
                "final_newline" => &mut out.final_newline,
                "save_on_focus_loss" => &mut out.save_on_focus_loss,
                "word_wrap" => &mut out.word_wrap,
                "side_overlay" => &mut out.side_overlay,
                _ => &mut out.auto_pairs,
            };
            *slot = on;
        }
        "rail" => match word(value, &["left", "right", "top", "bottom", "hidden"]) {
            Some("left") => out.rail = RailPlace::Left,
            Some("right") => out.rail = RailPlace::Right,
            Some("top") => out.rail = RailPlace::Top,
            Some("bottom") => out.rail = RailPlace::Bottom,
            // the old way to hide it: hidden, on the left when shown again
            Some(_) => out.rail_show = RailShow::Hidden,
            None => return false,
        },
        "rail_show" => match word(value, &["always", "hidden", "edge"]) {
            Some("always") => out.rail_show = RailShow::Always,
            Some("hidden") => out.rail_show = RailShow::Hidden,
            Some(_) => out.rail_show = RailShow::Edge,
            None => return false,
        },
        "side_bar" => match word(value, &["left", "right"]) {
            Some("left") => out.side_bar = SidePlace::Left,
            Some(_) => out.side_bar = SidePlace::Right,
            None => return false,
        },
        _ => return false,
    }
    true
}

/// how a field is edited in a settings ui.
#[derive(Clone, Copy, Debug)]
pub enum FieldKind {
    Toggle,
    /// one of these words.
    Choice(&'static [&'static str]),
    Number {
        min: f32,
        max: f32,
        step: f32,
    },
    /// free text; empty means unset.
    Text,
}

#[derive(Clone, Copy, Debug)]
pub struct Field {
    pub key: &'static str,
    /// english label, also the translation key.
    pub label: &'static str,
    /// one short line on what it does, english, also a translation key.
    pub about: &'static str,
    /// "Look", "Editor", "Files", "Layout" or "Terminal".
    pub group: &'static str,
    pub kind: FieldKind,
}

/// every key `apply` takes, in the order a settings ui shows them.
pub const FIELDS: &[Field] = &[
    Field {
        key: "theme",
        label: "Theme",
        about: "Dark, light, or follow the system.",
        group: "Look",
        kind: FieldKind::Choice(&["dark", "light", "system"]),
    },
    Field {
        key: "language",
        label: "Language",
        about: "Interface language. System follows the OS.",
        group: "Look",
        kind: FieldKind::Choice(&["system", "en", "ko"]),
    },
    Field {
        key: "editor_font",
        label: "Editor Font",
        about: "Editor font name. Empty uses the default.",
        group: "Look",
        kind: FieldKind::Text,
    },
    Field {
        key: "editor_size",
        label: "Font Size",
        about: "Editor text size.",
        group: "Look",
        kind: FieldKind::Number {
            min: 6.0,
            max: 72.0,
            step: 1.0,
        },
    },
    Field {
        key: "line_height",
        label: "Line Height",
        about: "Line height as a multiple of font size.",
        group: "Look",
        kind: FieldKind::Number {
            min: 1.0,
            max: 3.0,
            step: 0.05,
        },
    },
    Field {
        key: "caret",
        label: "Caret Style",
        about: "Text cursor shape.",
        group: "Look",
        kind: FieldKind::Choice(&["bar", "block", "underline"]),
    },
    Field {
        key: "caret_blink",
        label: "Caret Blink",
        about: "Blink the caret when idle.",
        group: "Look",
        kind: FieldKind::Toggle,
    },
    Field {
        key: "smooth_caret",
        label: "Smooth Caret",
        about: "Animate caret movement.",
        group: "Look",
        kind: FieldKind::Toggle,
    },
    Field {
        key: "line_numbers",
        label: "Line Numbers",
        about: "Gutter line numbers. Relative counts from the caret line.",
        group: "Look",
        kind: FieldKind::Choice(&["on", "relative", "off"]),
    },
    Field {
        key: "current_line",
        label: "Current Line",
        about: "Highlight the current line.",
        group: "Look",
        kind: FieldKind::Toggle,
    },
    Field {
        key: "indent_guides",
        label: "Indent Guides",
        about: "Show vertical indent guides.",
        group: "Look",
        kind: FieldKind::Toggle,
    },
    Field {
        key: "overview",
        label: "Scroll Bar Marks",
        about: "Show problem and search marks beside the scroll bar.",
        group: "Look",
        kind: FieldKind::Toggle,
    },
    Field {
        key: "agent",
        label: "Coding Agent",
        about: "The agent the agent panel talks to. It signs in on its own.",
        group: "Agent",
        kind: FieldKind::Choice(&["claude", "codex", "gemini", "opencode", "openai"]),
    },
    Field {
        key: "openai_base_url",
        label: "Model Server",
        about: "An OpenAI-compatible server for the built-in agent (agent = openai).",
        group: "Agent",
        kind: FieldKind::Text,
    },
    Field {
        key: "openai_model",
        label: "Model",
        about: "The model the built-in agent asks.",
        group: "Agent",
        kind: FieldKind::Text,
    },
    Field {
        key: "openai_context",
        label: "Model Context",
        about: "The model's context in tokens. Older talk is summarized past three quarters of it.",
        group: "Agent",
        kind: FieldKind::Number {
            min: 2_000.0,
            max: 2_000_000.0,
            step: 1_000.0,
        },
    },
    Field {
        key: "agent_command",
        label: "Agent Command",
        about: "A program and args that speak ACP, in place of the agent's own.",
        group: "Agent",
        kind: FieldKind::Text,
    },
    Field {
        key: "terminal_size",
        label: "Terminal Font Size",
        about: "Terminal text size.",
        group: "Terminal",
        kind: FieldKind::Number {
            min: 6.0,
            max: 72.0,
            step: 1.0,
        },
    },
    Field {
        key: "tab_size",
        label: "Tab Size",
        about: "Columns per tab.",
        group: "Editor",
        kind: FieldKind::Number {
            min: 1.0,
            max: 16.0,
            step: 1.0,
        },
    },
    Field {
        key: "whitespace",
        label: "Show Whitespace",
        about: "Show spaces and tabs as dots and arrows.",
        group: "Editor",
        kind: FieldKind::Choice(&["none", "selection", "all"]),
    },
    Field {
        key: "sticky_scroll",
        label: "Sticky Scroll",
        about: "Pin lines that open the scopes above.",
        group: "Editor",
        kind: FieldKind::Toggle,
    },
    Field {
        key: "word_highlight",
        label: "Word Highlight",
        about: "Highlight other copies of the word under the caret.",
        group: "Editor",
        kind: FieldKind::Toggle,
    },
    Field {
        key: "auto_pairs",
        label: "Auto Pairs",
        about: "Insert matching brackets and quotes.",
        group: "Editor",
        kind: FieldKind::Toggle,
    },
    Field {
        key: "word_wrap",
        label: "Word Wrap",
        about: "Wrap long lines.",
        group: "Editor",
        kind: FieldKind::Toggle,
    },
    Field {
        key: "format_on_save",
        label: "Format on Save",
        about: "Format the file with the language server on save.",
        group: "Files",
        kind: FieldKind::Toggle,
    },
    Field {
        key: "trim_trailing_whitespace",
        label: "Trim Trailing Whitespace",
        about: "Remove trailing whitespace on save.",
        group: "Files",
        kind: FieldKind::Toggle,
    },
    Field {
        key: "final_newline",
        label: "Final Newline",
        about: "End files with a newline on save.",
        group: "Files",
        kind: FieldKind::Toggle,
    },
    Field {
        key: "save_on_focus_loss",
        label: "Save on Focus Loss",
        about: "Save edited files when the window loses focus.",
        group: "Files",
        kind: FieldKind::Toggle,
    },
    Field {
        key: "breadcrumbs",
        label: "Breadcrumbs",
        about: "Show the file path above the editor.",
        group: "Layout",
        kind: FieldKind::Toggle,
    },
    Field {
        key: "rail",
        label: "Panel Rail",
        about: "Panel button rail position.",
        group: "Layout",
        kind: FieldKind::Choice(&["left", "right", "top", "bottom"]),
    },
    Field {
        key: "rail_show",
        label: "Show Rail",
        about: "Always, never, or when the pointer reaches the rail's edge.",
        group: "Layout",
        kind: FieldKind::Choice(&["always", "hidden", "edge"]),
    },
    Field {
        key: "side_bar",
        label: "Side Bar Edge",
        about: "Side bar edge.",
        group: "Layout",
        kind: FieldKind::Choice(&["left", "right"]),
    },
    Field {
        key: "side_overlay",
        label: "Side Bar Overlay",
        about: "Side bar floats over the editor and hides on click outside.",
        group: "Layout",
        kind: FieldKind::Toggle,
    },
];

/// the value of `key` in `s` as a toml literal. none for an unknown key.
pub fn current(s: &Settings, key: &str) -> Option<String> {
    let text = match key {
        "theme" => quote(match s.theme {
            ThemeChoice::Dark => "dark",
            ThemeChoice::Light => "light",
            ThemeChoice::System => "system",
        }),
        "language" => quote(s.language.as_deref().unwrap_or("system")),
        "editor_font" => quote(s.editor_font.as_deref().unwrap_or("")),
        "editor_size" => number_literal(s.editor_size),
        "line_height" => number_literal(s.line_height),
        "tab_size" => s.tab_size.to_string(),
        "caret" => quote(match s.caret {
            CaretStyle::Bar => "bar",
            CaretStyle::Block => "block",
            CaretStyle::Underline => "underline",
        }),
        "line_numbers" => quote(match s.line_numbers {
            LineNumbers::On => "on",
            LineNumbers::Relative => "relative",
            LineNumbers::Off => "off",
        }),
        "whitespace" => quote(match s.whitespace {
            Whitespace::None => "none",
            Whitespace::Selection => "selection",
            Whitespace::All => "all",
        }),
        "terminal_size" => number_literal(s.terminal_size),
        "agent" => quote(s.agent.word()),
        "agent_command" => quote(s.agent_command.as_deref().unwrap_or("")),
        "openai_base_url" => quote(&s.openai_base_url),
        "openai_model" => quote(s.openai_model.as_deref().unwrap_or("")),
        "openai_context" => number_literal(s.openai_context),
        "rail" => quote(match s.rail {
            RailPlace::Left => "left",
            RailPlace::Right => "right",
            RailPlace::Top => "top",
            RailPlace::Bottom => "bottom",
        }),
        "rail_show" => quote(match s.rail_show {
            RailShow::Always => "always",
            RailShow::Hidden => "hidden",
            RailShow::Edge => "edge",
        }),
        "side_bar" => quote(match s.side_bar {
            SidePlace::Left => "left",
            SidePlace::Right => "right",
        }),
        "caret_blink" => s.caret_blink.to_string(),
        "smooth_caret" => s.smooth_caret.to_string(),
        "current_line" => s.current_line.to_string(),
        "indent_guides" => s.indent_guides.to_string(),
        "breadcrumbs" => s.breadcrumbs.to_string(),
        "overview" => s.overview.to_string(),
        "sticky_scroll" => s.sticky_scroll.to_string(),
        "format_on_save" => s.format_on_save.to_string(),
        "word_highlight" => s.word_highlight.to_string(),
        "auto_pairs" => s.auto_pairs.to_string(),
        "trim_trailing_whitespace" => s.trim_trailing_whitespace.to_string(),
        "final_newline" => s.final_newline.to_string(),
        "save_on_focus_loss" => s.save_on_focus_loss.to_string(),
        "word_wrap" => s.word_wrap.to_string(),
        "side_overlay" => s.side_overlay.to_string(),
        _ => return None,
    };
    Some(text)
}

/// a string as a toml basic string, escaped.
pub fn quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if c.is_control() => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// a number as a toml literal: whole numbers without a point, others to 2 decimals.
fn number_literal(n: f32) -> String {
    if n.fract() == 0.0 {
        return (n as i64).to_string();
    }
    let text = format!("{n:.2}");
    text.trim_end_matches('0').trim_end_matches('.').to_owned()
}

/// for a settings line about `key`: true when it sets the key, false when it
/// has it commented out. none when the line is about another key.
fn key_line(line: &str, key: &str) -> Option<bool> {
    let trimmed = line.trim_start();
    let (active, rest) = match trimmed.strip_prefix('#') {
        Some(commented) => (false, commented.trim_start()),
        None => (true, trimmed),
    };
    let rest = rest.strip_prefix(key)?.trim_start();
    rest.starts_with('=').then_some(active)
}

/// the settings text with `key` set to `literal`, a toml literal. every other
/// line and comment is kept. none comments the key out. a key not in the text
/// is added at the end.
pub fn with_line(text: &str, key: &str, literal: Option<&str>) -> String {
    let eol = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let mut lines: Vec<String> = text.lines().map(str::to_owned).collect();
    // top-level keys live before the first table; one written after it
    // would belong to that table
    let tables = lines
        .iter()
        .position(|l| l.trim_start().starts_with('['))
        .unwrap_or(lines.len());
    let found = lines[..tables]
        .iter()
        .position(|line| key_line(line, key).is_some());
    match (found, literal) {
        (Some(i), Some(value)) => lines[i] = format!("{key} = {value}"),
        (Some(i), None) => {
            if key_line(&lines[i], key) == Some(true) {
                let commented = format!("# {}", lines[i].trim_start());
                lines[i] = commented;
            }
        }
        (None, Some(value)) => lines.insert(tables, format!("{key} = {value}")),
        (None, None) => return text.to_owned(),
    }
    let mut out = lines.join(eol);
    out.push_str(eol);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_tables_read_write_and_keep_their_place() {
        let text = "theme = \"light\"

[servers.nim]
command = \"nimlangserver\"
files = [\"*.nim\"]

[servers.rust]
disabled = true
";
        let (s, problems) = parse(text);
        let s = s.unwrap();
        assert!(problems.is_empty(), "{problems:?}");
        assert_eq!(s.servers.len(), 2);
        let nim = s.servers.iter().find(|r| r.language == "nim").unwrap();
        assert_eq!(
            (nim.command.as_str(), nim.files.clone()),
            ("nimlangserver", vec!["*.nim".to_owned()])
        );
        assert!(s.servers.iter().any(|r| r.language == "rust" && r.disabled));
        // a new top-level key lands before the tables, not in the last one
        let more = with_line(text, "tab_size", Some("2"));
        let (s2, problems) = parse(&more);
        assert!(problems.is_empty(), "{problems:?}");
        assert_eq!(s2.unwrap().tab_size, 2);
        // a table is replaced in place of the old one, others kept
        let rule = ServerRule {
            language: "nim".into(),
            command: "nim-lsp".into(),
            args: vec!["--stdio".into()],
            ..ServerRule::default()
        };
        let changed = with_server(&more, "nim", Some(&rule));
        let s3 = parse(&changed).0.unwrap();
        let nim = s3.servers.iter().find(|r| r.language == "nim").unwrap();
        assert_eq!(
            (nim.command.as_str(), nim.args.clone()),
            ("nim-lsp", vec!["--stdio".to_owned()])
        );
        assert!(s3.servers.iter().any(|r| r.language == "rust"));
        let gone = with_server(&changed, "nim", None);
        assert!(
            parse(&gone)
                .0
                .unwrap()
                .servers
                .iter()
                .all(|r| r.language != "nim")
        );

        // names toml can't take bare still write a file that reads
        let cpp = ServerRule {
            language: "c++".into(),
            command: "clangd".into(),
            ..ServerRule::default()
        };
        let text = with_server(&gone, "c++", Some(&cpp));
        let (s5, problems) = parse(&text);
        assert!(problems.is_empty(), "{problems:?}");
        assert!(s5.unwrap().servers.iter().any(|r| r.language == "c++"));
        assert!(
            parse(&with_server(&text, "c++", None))
                .0
                .unwrap()
                .servers
                .iter()
                .all(|r| r.language != "c++")
        );
        // only roots: kept
        let roots = ServerRule {
            language: "rust".into(),
            roots: vec!["Cargo.toml".into()],
            ..ServerRule::default()
        };
        let s6 = parse(&with_server("", "rust", Some(&roots))).0.unwrap();
        assert_eq!(s6.servers[0].roots, ["Cargo.toml"]);
        // bad names and empty tables are named, not taken
        let (s4, problems) = parse(
            "[servers.\"a b\"]
command = \"x\"
[servers.empty]
",
        );
        assert!(s4.unwrap().servers.is_empty());
        assert_eq!(problems.len(), 2);
    }

    #[test]
    fn starter_file_is_all_defaults() {
        let (s, problems) = parse(STARTER);
        assert_eq!(s, Some(Settings::default()));
        assert!(problems.is_empty());
    }

    #[test]
    fn values_apply_and_bad_ones_are_named() {
        let text = "theme = \"light\"\neditor_size = 16\nline_numbers = \"relative\"\ntab_size = 2.5\nwat = 1\ncaret_blink = false";
        let (s, problems) = parse(text);
        let s = s.unwrap();
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
        assert_eq!(s, None, "the settings in force stay");
        assert_eq!(problems.len(), 1);
        let (s, _) = parse("editor_size = 9000");
        assert_eq!(s.unwrap().editor_size, 14.0);
    }

    #[test]
    fn rail_side_and_overlay_apply() {
        let (s, problems) = parse("rail = \"top\"\nside_bar = \"right\"\nside_overlay = true");
        assert!(problems.is_empty());
        let s = s.unwrap();
        assert_eq!(s.rail, RailPlace::Top);
        assert_eq!(s.side_bar, SidePlace::Right);
        assert!(s.side_overlay);
    }

    #[test]
    fn every_field_default_is_accepted_by_apply() {
        for field in FIELDS {
            let literal = current(&Settings::default(), field.key).expect("field key");
            let unset_text = matches!(field.kind, FieldKind::Text) && literal == "\"\"";
            let unset_lang = field.key == "language" && literal == "\"system\"";
            if unset_text || unset_lang {
                continue;
            }
            let (s, problems) = parse(&format!("{} = {literal}", field.key));
            assert!(problems.is_empty(), "{}: {problems:?}", field.key);
            assert_eq!(s, Some(Settings::default()), "{}", field.key);
        }
    }

    #[test]
    fn every_apply_key_has_a_field() {
        const KEYS: &[&str] = &[
            "theme",
            "language",
            "editor_font",
            "editor_size",
            "line_height",
            "tab_size",
            "caret",
            "caret_blink",
            "smooth_caret",
            "line_numbers",
            "current_line",
            "indent_guides",
            "breadcrumbs",
            "overview",
            "sticky_scroll",
            "format_on_save",
            "whitespace",
            "word_highlight",
            "auto_pairs",
            "terminal_size",
            "agent",
            "agent_command",
            "openai_base_url",
            "openai_model",
            "openai_context",
            "trim_trailing_whitespace",
            "final_newline",
            "save_on_focus_loss",
            "word_wrap",
            "rail",
            "rail_show",
            "side_bar",
            "side_overlay",
        ];
        assert_eq!(FIELDS.len(), KEYS.len());
        for key in KEYS {
            assert!(FIELDS.iter().any(|f| f.key == *key), "{key} has no field");
        }
    }

    #[test]
    fn current_prints_toml_literals() {
        let mut s = Settings::default();
        assert_eq!(current(&s, "editor_size").as_deref(), Some("14"));
        assert_eq!(current(&s, "line_height").as_deref(), Some("1.45"));
        assert_eq!(current(&s, "language").as_deref(), Some("\"system\""));
        assert_eq!(current(&s, "editor_font").as_deref(), Some("\"\""));
        assert_eq!(current(&s, "caret_blink").as_deref(), Some("true"));
        assert_eq!(current(&s, "theme").as_deref(), Some("\"dark\""));
        s.editor_font = Some("Consolas".to_owned());
        s.line_height = 1.5;
        assert_eq!(current(&s, "editor_font").as_deref(), Some("\"Consolas\""));
        assert_eq!(current(&s, "line_height").as_deref(), Some("1.5"));
        assert_eq!(current(&s, "nope"), None);
    }

    #[test]
    fn with_line_replaces_an_active_line() {
        let text = "tab_size = 2\nword_wrap = true\n";
        assert_eq!(
            with_line(text, "tab_size", Some("8")),
            "tab_size = 8\nword_wrap = true\n"
        );
    }

    #[test]
    fn with_line_uncomments_a_commented_line() {
        let text = "# tab_size = 4   # default\nword_wrap = true\n";
        assert_eq!(
            with_line(text, "tab_size", Some("8")),
            "tab_size = 8\nword_wrap = true\n"
        );
    }

    #[test]
    fn with_line_appends_a_missing_key() {
        assert_eq!(
            with_line("word_wrap = true", "tab_size", Some("8")),
            "word_wrap = true\ntab_size = 8\n"
        );
    }

    #[test]
    fn with_line_does_not_match_a_longer_key() {
        let text = "tab_size_x = 3\n";
        assert_eq!(
            with_line(text, "tab_size", Some("8")),
            "tab_size_x = 3\ntab_size = 8\n"
        );
    }

    #[test]
    fn with_line_unset_comments_out() {
        assert_eq!(
            with_line("tab_size = 8\n", "tab_size", None),
            "# tab_size = 8\n"
        );
        let text = "# tab_size = 4\n";
        assert_eq!(with_line(text, "tab_size", None), text);
    }

    #[test]
    fn with_line_keeps_crlf() {
        let text = "theme = \"dark\"\r\nword_wrap = true\r\n";
        assert_eq!(
            with_line(text, "tab_size", Some("8")),
            "theme = \"dark\"\r\nword_wrap = true\r\ntab_size = 8\r\n"
        );
        assert_eq!(
            with_line(text, "word_wrap", Some("false")),
            "theme = \"dark\"\r\nword_wrap = false\r\n"
        );
    }
}
