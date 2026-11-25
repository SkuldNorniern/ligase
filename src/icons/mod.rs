//! the svg icons in `assets/icons`, built in. each is parsed once, the first
//! time it is drawn. file icons carry their own colours; action icons take
//! the colour they are drawn in.

use std::collections::HashMap;
use std::path::Path;
use std::sync::OnceLock;

use aurea::AureaResult;
use aurea::render::{Color, Rect, VectorImage};
use aurea::ui::{Component, IconMark, Ui};

use crate::app::Id;

/// every icon by name, its svg text built in.
const ICONS: [(&str, &str); 68] = [
    // ui: actions, panes and tree folders, drawn in currentColor or their own fill.
    ("agent", include_str!("../../assets/icons/ui/agent.svg")),
    ("branch", include_str!("../../assets/icons/ui/branch.svg")),
    ("check", include_str!("../../assets/icons/ui/check.svg")),
    (
        "chevron-down",
        include_str!("../../assets/icons/ui/chevron-down.svg"),
    ),
    (
        "chevron-right",
        include_str!("../../assets/icons/ui/chevron-right.svg"),
    ),
    ("close", include_str!("../../assets/icons/ui/close.svg")),
    ("discard", include_str!("../../assets/icons/ui/discard.svg")),
    (
        "extensions",
        include_str!("../../assets/icons/ui/extensions.svg"),
    ),
    ("files", include_str!("../../assets/icons/ui/files.svg")),
    (
        "fit-width",
        include_str!("../../assets/icons/ui/fit-width.svg"),
    ),
    ("folder", include_str!("../../assets/icons/ui/folder.svg")),
    (
        "folder-open",
        include_str!("../../assets/icons/ui/folder-open.svg"),
    ),
    ("history", include_str!("../../assets/icons/ui/history.svg")),
    ("minus", include_str!("../../assets/icons/ui/minus.svg")),
    (
        "open-file",
        include_str!("../../assets/icons/ui/open-file.svg"),
    ),
    ("plus", include_str!("../../assets/icons/ui/plus.svg")),
    (
        "problems",
        include_str!("../../assets/icons/ui/problems.svg"),
    ),
    ("refresh", include_str!("../../assets/icons/ui/refresh.svg")),
    ("remote", include_str!("../../assets/icons/ui/remote.svg")),
    ("search", include_str!("../../assets/icons/ui/search.svg")),
    (
        "settings",
        include_str!("../../assets/icons/ui/settings.svg"),
    ),
    (
        "source-control",
        include_str!("../../assets/icons/ui/source-control.svg"),
    ),
    ("at", include_str!("../../assets/icons/ui/at.svg")),
    ("bolt", include_str!("../../assets/icons/ui/bolt.svg")),
    ("bulb", include_str!("../../assets/icons/ui/bulb.svg")),
    ("chip", include_str!("../../assets/icons/ui/chip.svg")),
    ("eye", include_str!("../../assets/icons/ui/eye.svg")),
    ("globe", include_str!("../../assets/icons/ui/globe.svg")),
    ("pencil", include_str!("../../assets/icons/ui/pencil.svg")),
    ("plan", include_str!("../../assets/icons/ui/plan.svg")),
    ("send", include_str!("../../assets/icons/ui/send.svg")),
    ("shield", include_str!("../../assets/icons/ui/shield.svg")),
    ("stop", include_str!("../../assets/icons/ui/stop.svg")),
    (
        "terminal",
        include_str!("../../assets/icons/ui/terminal.svg"),
    ),
    // lang: language and file-type icons, each with its own colours.
    ("c", include_str!("../../assets/icons/lang/c.svg")),
    ("config", include_str!("../../assets/icons/lang/config.svg")),
    ("cpp", include_str!("../../assets/icons/lang/cpp.svg")),
    ("csharp", include_str!("../../assets/icons/lang/csharp.svg")),
    ("css", include_str!("../../assets/icons/lang/css.svg")),
    ("docker", include_str!("../../assets/icons/lang/docker.svg")),
    ("file", include_str!("../../assets/icons/lang/file.svg")),
    (
        "file-text",
        include_str!("../../assets/icons/lang/file-text.svg"),
    ),
    ("git", include_str!("../../assets/icons/lang/git.svg")),
    ("go", include_str!("../../assets/icons/lang/go.svg")),
    ("html", include_str!("../../assets/icons/lang/html.svg")),
    ("image", include_str!("../../assets/icons/lang/image.svg")),
    ("java", include_str!("../../assets/icons/lang/java.svg")),
    (
        "javascript",
        include_str!("../../assets/icons/lang/javascript.svg"),
    ),
    ("json", include_str!("../../assets/icons/lang/json.svg")),
    ("kotlin", include_str!("../../assets/icons/lang/kotlin.svg")),
    ("lock", include_str!("../../assets/icons/lang/lock.svg")),
    ("lua", include_str!("../../assets/icons/lang/lua.svg")),
    (
        "makefile",
        include_str!("../../assets/icons/lang/makefile.svg"),
    ),
    (
        "markdown",
        include_str!("../../assets/icons/lang/markdown.svg"),
    ),
    ("php", include_str!("../../assets/icons/lang/php.svg")),
    (
        "powershell",
        include_str!("../../assets/icons/lang/powershell.svg"),
    ),
    ("python", include_str!("../../assets/icons/lang/python.svg")),
    ("ruby", include_str!("../../assets/icons/lang/ruby.svg")),
    ("rust", include_str!("../../assets/icons/lang/rust.svg")),
    ("shell", include_str!("../../assets/icons/lang/shell.svg")),
    ("sql", include_str!("../../assets/icons/lang/sql.svg")),
    ("swift", include_str!("../../assets/icons/lang/swift.svg")),
    ("toml", include_str!("../../assets/icons/lang/toml.svg")),
    (
        "typescript",
        include_str!("../../assets/icons/lang/typescript.svg"),
    ),
    (
        "verilog",
        include_str!("../../assets/icons/lang/verilog.svg"),
    ),
    ("xml", include_str!("../../assets/icons/lang/xml.svg")),
    ("yaml", include_str!("../../assets/icons/lang/yaml.svg")),
    ("zig", include_str!("../../assets/icons/lang/zig.svg")),
];

/// the parsed icons; one that doesn't parse draws nothing.
fn parsed() -> &'static HashMap<&'static str, VectorImage> {
    static ALL: OnceLock<HashMap<&'static str, VectorImage>> = OnceLock::new();
    ALL.get_or_init(|| {
        ICONS
            .iter()
            .map(|(name, svg)| {
                let image = VectorImage::parse(svg)
                    .unwrap_or_else(|_| VectorImage::empty(Rect::new(0.0, 0.0, 16.0, 16.0)));
                (*name, image)
            })
            .collect()
    })
}

/// icon `name`, for a widget that takes an image; an unknown name draws nothing.
/// the bundled icon called `name`, as a name that lives on.
pub fn named(name: &str) -> Option<&'static str> {
    ICONS.iter().find(|(n, _)| *n == name).map(|(n, _)| *n)
}

pub fn image(name: &str) -> &'static VectorImage {
    static NONE: OnceLock<VectorImage> = OnceLock::new();
    parsed()
        .get(name)
        .unwrap_or_else(|| NONE.get_or_init(|| VectorImage::empty(Rect::new(0.0, 0.0, 16.0, 16.0))))
}

/// draws icon `name` in `rect`; `color` tints the ones drawn in `currentColor`.
pub fn draw(ui: &mut Ui<'_, Id>, name: &str, rect: Rect, color: Color) -> AureaResult<()> {
    match parsed().get(name) {
        Some(image) => IconMark::new(image, color).paint(ui, rect),
        None => Ok(()),
    }
}

/// the icon for a file or folder.
pub fn for_path(path: &Path, dir: bool, open: bool) -> &'static str {
    if dir {
        return if open { "folder-open" } else { "folder" };
    }
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    match name.as_str() {
        "dockerfile" | "containerfile" => return "docker",
        "makefile" | "gnumakefile" => return "makefile",
        ".gitignore" | ".gitattributes" | ".gitmodules" => return "git",
        ".env" | ".editorconfig" => return "config",
        _ => {}
    }
    if name.ends_with(".lock") || name == "package-lock.json" {
        return "lock";
    }
    let ext = name.rsplit_once('.').map_or("", |(_, e)| e);
    match ext {
        "rs" => "rust",
        "toml" => "toml",
        "json" | "jsonc" => "json",
        "md" | "markdown" => "markdown",
        "yaml" | "yml" => "yaml",
        "c" | "h" => "c",
        "cpp" | "cc" | "cxx" | "hpp" | "hh" | "hxx" => "cpp",
        "cs" => "csharp",
        "py" => "python",
        "js" | "mjs" | "cjs" | "jsx" => "javascript",
        "ts" | "mts" | "cts" | "tsx" => "typescript",
        "go" => "go",
        "java" => "java",
        "kt" | "kts" => "kotlin",
        "swift" => "swift",
        "lua" => "lua",
        "rb" | "rake" | "gemspec" => "ruby",
        "php" => "php",
        "html" | "htm" | "xhtml" | "vue" | "svelte" => "html",
        "css" | "scss" | "less" => "css",
        "sh" | "bash" | "zsh" => "shell",
        "ps1" | "psm1" | "psd1" => "powershell",
        "sql" => "sql",
        "xml" | "xaml" | "csproj" | "plist" | "xsd" => "xml",
        "png" | "jpg" | "jpeg" | "gif" | "bmp" | "ico" | "webp" | "svg" => "image",
        "ini" | "cfg" | "conf" | "properties" | "env" => "config",
        "v" | "vh" | "sv" | "svh" | "vhd" | "vhdl" | "bsv" => "verilog",
        "zig" => "zig",
        "txt" | "log" => "file-text",
        _ => "file",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_icon_parses() {
        for (name, svg) in ICONS {
            let image = VectorImage::parse(svg);
            assert!(image.is_ok(), "{name}: {:?}", image.err());
            assert!(!image.unwrap().is_empty(), "{name} draws nothing");
        }
    }

    #[test]
    fn files_get_their_icons() {
        let names: Vec<&str> = ICONS.iter().map(|(n, _)| *n).collect();
        for (path, want) in [
            ("src/main.rs", "rust"),
            ("Cargo.lock", "lock"),
            (".gitignore", "git"),
            ("web/app.TSX", "typescript"),
            ("README", "file"),
            ("Dockerfile", "docker"),
        ] {
            let got = for_path(Path::new(path), false, false);
            assert_eq!(got, want, "{path}");
            assert!(names.contains(&got), "{got} is an icon");
        }
        assert_eq!(for_path(Path::new("src"), true, true), "folder-open");
    }
}
