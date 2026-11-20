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
const ICONS: [(&str, &str); 54] = [
    ("remote", include_str!("../../assets/icons/remote.svg")),
    ("agent", include_str!("../../assets/icons/agent.svg")),
    ("branch", include_str!("../../assets/icons/branch.svg")),
    (
        "extensions",
        include_str!("../../assets/icons/extensions.svg"),
    ),
    ("files", include_str!("../../assets/icons/files.svg")),
    ("problems", include_str!("../../assets/icons/problems.svg")),
    ("settings", include_str!("../../assets/icons/settings.svg")),
    ("c", include_str!("../../assets/icons/c.svg")),
    ("check", include_str!("../../assets/icons/check.svg")),
    (
        "chevron-down",
        include_str!("../../assets/icons/chevron-down.svg"),
    ),
    (
        "chevron-right",
        include_str!("../../assets/icons/chevron-right.svg"),
    ),
    ("close", include_str!("../../assets/icons/close.svg")),
    ("config", include_str!("../../assets/icons/config.svg")),
    ("cpp", include_str!("../../assets/icons/cpp.svg")),
    ("csharp", include_str!("../../assets/icons/csharp.svg")),
    ("css", include_str!("../../assets/icons/css.svg")),
    ("discard", include_str!("../../assets/icons/discard.svg")),
    ("docker", include_str!("../../assets/icons/docker.svg")),
    ("file", include_str!("../../assets/icons/file.svg")),
    (
        "file-text",
        include_str!("../../assets/icons/file-text.svg"),
    ),
    ("folder", include_str!("../../assets/icons/folder.svg")),
    (
        "folder-open",
        include_str!("../../assets/icons/folder-open.svg"),
    ),
    ("git", include_str!("../../assets/icons/git.svg")),
    ("go", include_str!("../../assets/icons/go.svg")),
    ("html", include_str!("../../assets/icons/html.svg")),
    ("image", include_str!("../../assets/icons/image.svg")),
    ("java", include_str!("../../assets/icons/java.svg")),
    (
        "javascript",
        include_str!("../../assets/icons/javascript.svg"),
    ),
    ("json", include_str!("../../assets/icons/json.svg")),
    ("kotlin", include_str!("../../assets/icons/kotlin.svg")),
    ("lock", include_str!("../../assets/icons/lock.svg")),
    ("lua", include_str!("../../assets/icons/lua.svg")),
    ("makefile", include_str!("../../assets/icons/makefile.svg")),
    ("markdown", include_str!("../../assets/icons/markdown.svg")),
    ("minus", include_str!("../../assets/icons/minus.svg")),
    (
        "open-file",
        include_str!("../../assets/icons/open-file.svg"),
    ),
    ("php", include_str!("../../assets/icons/php.svg")),
    ("plus", include_str!("../../assets/icons/plus.svg")),
    (
        "powershell",
        include_str!("../../assets/icons/powershell.svg"),
    ),
    ("python", include_str!("../../assets/icons/python.svg")),
    ("refresh", include_str!("../../assets/icons/refresh.svg")),
    ("ruby", include_str!("../../assets/icons/ruby.svg")),
    ("rust", include_str!("../../assets/icons/rust.svg")),
    ("search", include_str!("../../assets/icons/search.svg")),
    ("shell", include_str!("../../assets/icons/shell.svg")),
    (
        "source-control",
        include_str!("../../assets/icons/source-control.svg"),
    ),
    ("sql", include_str!("../../assets/icons/sql.svg")),
    ("swift", include_str!("../../assets/icons/swift.svg")),
    ("toml", include_str!("../../assets/icons/toml.svg")),
    (
        "typescript",
        include_str!("../../assets/icons/typescript.svg"),
    ),
    ("verilog", include_str!("../../assets/icons/verilog.svg")),
    ("xml", include_str!("../../assets/icons/xml.svg")),
    ("yaml", include_str!("../../assets/icons/yaml.svg")),
    ("zig", include_str!("../../assets/icons/zig.svg")),
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
