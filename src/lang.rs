//! english and korean. english text is the key, korean lives in
//! `lang/ko.toml`; missing text stays english.

use std::collections::HashMap;

const KO: &str = include_str!("../lang/ko.toml");

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Lang {
    #[default]
    En,
    Ko,
}

impl Lang {
    /// `LIGASE_LANG` first, then `preferred` (from settings), then the system.
    pub fn detect(preferred: Option<&str>) -> Self {
        let wanted = std::env::var("LIGASE_LANG")
            .ok()
            .or_else(|| preferred.filter(|tag| !tag.is_empty()).map(str::to_owned))
            .or_else(aurea::locale)
            .unwrap_or_default();
        Self::from_tag(&wanted)
    }

    /// `ko`, `ko-KR`, `ko_KR.UTF-8` are korean; anything else is english.
    pub fn from_tag(tag: &str) -> Self {
        if tag.to_ascii_lowercase().starts_with("ko") {
            Self::Ko
        } else {
            Self::En
        }
    }

    /// system ui font that has the letters for this language.
    pub const fn font_family(self) -> &'static str {
        let hangul = matches!(self, Lang::Ko);
        if cfg!(windows) {
            if hangul { "Malgun Gothic" } else { "Segoe UI" }
        } else if cfg!(target_os = "macos") {
            if hangul {
                "AppleSDGothicNeo"
            } else {
                "Helvetica"
            }
        } else if hangul {
            "NotoSansCJK"
        } else {
            "DejaVuSans"
        }
    }
}

/// text lookup for one language.
pub struct Texts {
    table: HashMap<String, String>,
}

impl Texts {
    pub fn new(lang: Lang) -> Self {
        let table = match lang {
            Lang::En => HashMap::new(),
            Lang::Ko => parse(KO),
        };
        Self { table }
    }

    pub fn tr<'a>(&'a self, en: &'a str) -> &'a str {
        self.table.get(en).map_or(en, String::as_str)
    }
}

/// a broken table means english everywhere, not a crash.
fn parse(source: &str) -> HashMap<String, String> {
    let Ok(table) = source.parse::<toml::Table>() else {
        return HashMap::new();
    };
    table
        .into_iter()
        .filter_map(|(en, value)| match value {
            toml::Value::String(text) => Some((en, text)),
            _ => None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tags_map_to_languages() {
        assert_eq!(Lang::from_tag("ko_KR.UTF-8"), Lang::Ko);
        assert_eq!(Lang::from_tag("en-US"), Lang::En);
        assert_eq!(Lang::from_tag(""), Lang::En);
    }

    #[test]
    fn korean_table_parses_and_has_no_question_marks() {
        // an editor that can't write hangul leaves "??" behind
        for line in KO.lines().filter(|l| l.contains(" = ")) {
            assert!(!line.contains("??"), "broken korean: {line}");
        }
        let texts = Texts::new(Lang::Ko);
        assert_eq!(texts.tr("Explorer"), "탐색기");
        assert_eq!(texts.tr("no such text"), "no such text");
        assert_eq!(Texts::new(Lang::En).tr("Explorer"), "Explorer");
        assert!(parse("= broken").is_empty());
    }
}
