//! pdf pages drawn by hayro, pure rust and built in: no c library, nothing
//! loaded at run time. a small face over it: open a pdf from memory, the
//! page sizes, a page into rgba.
//!
//! text in a font the pdf does not carry falls back to hayro's own base 14,
//! or for korean, chinese and japanese to a system font read on first need.

use std::collections::HashMap;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::Path;
use std::sync::{Arc, Mutex};

use aurea::render::Image;
use hayro::hayro_interpret::InterpreterSettings;
use hayro::hayro_interpret::font::{FontData, FontQuery};
use hayro::hayro_interpret::hayro_cmap::CidFamily;
use hayro::hayro_syntax::{LoadPdfError, Pdf};
use hayro::vello_cpu::color::palette::css::WHITE;
use hayro::{PixmapSettings, RenderCache, RenderSettings};

/// most pages a document is read for.
const MAX_PAGES: usize = 20_000;
/// biggest page bitmap, in pixels.
const MAX_PIXELS: f32 = 40_000_000.0;
/// biggest side of a page bitmap; hayro's canvas is u16 wide.
const MAX_SIDE: f32 = 12_000.0;
/// biggest system font read for a fallback.
const MAX_FONT: u64 = 64 << 20;

/// system fonts for one script, found once and kept for the book.
type Fallbacks = Arc<Mutex<HashMap<Script, Option<(FontData, u32)>>>>;

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Script {
    Korean,
    Chinese,
    Japanese,
}

/// where each script's font is looked for, with the face inside a
/// collection.
fn font_places(script: Script) -> &'static [(&'static str, u32)] {
    #[cfg(windows)]
    match script {
        Script::Korean => &[
            ("C:\\Windows\\Fonts\\malgun.ttf", 0),
            ("C:\\Windows\\Fonts\\gulim.ttc", 0),
        ],
        Script::Chinese => &[
            ("C:\\Windows\\Fonts\\msyh.ttc", 0),
            ("C:\\Windows\\Fonts\\simsun.ttc", 0),
        ],
        Script::Japanese => &[
            ("C:\\Windows\\Fonts\\YuGothM.ttc", 0),
            ("C:\\Windows\\Fonts\\msgothic.ttc", 0),
        ],
    }
    #[cfg(target_os = "macos")]
    match script {
        Script::Korean => &[("/System/Library/Fonts/AppleSDGothicNeo.ttc", 0)],
        Script::Chinese => &[
            ("/System/Library/Fonts/PingFang.ttc", 0),
            ("/System/Library/Fonts/STHeiti Light.ttc", 0),
        ],
        Script::Japanese => &[("/System/Library/Fonts/Hiragino Sans GB.ttc", 0)],
    }
    // noto cjk collections keep jp, kr, sc, tc in that order
    #[cfg(all(unix, not(target_os = "macos")))]
    match script {
        Script::Korean => &[
            ("/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc", 1),
            ("/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc", 1),
            (
                "/usr/share/fonts/google-noto-cjk/NotoSansCJK-Regular.ttc",
                1,
            ),
            ("/usr/share/fonts/truetype/nanum/NanumGothic.ttf", 0),
        ],
        Script::Chinese => &[
            ("/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc", 2),
            ("/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc", 2),
            (
                "/usr/share/fonts/google-noto-cjk/NotoSansCJK-Regular.ttc",
                2,
            ),
        ],
        Script::Japanese => &[
            ("/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc", 0),
            ("/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc", 0),
            (
                "/usr/share/fonts/google-noto-cjk/NotoSansCJK-Regular.ttc",
                0,
            ),
        ],
    }
}

fn read_font(path: &Path) -> Option<Vec<u8>> {
    let size = std::fs::metadata(path).ok()?.len();
    if size > MAX_FONT {
        return None;
    }
    std::fs::read(path).ok()
}

/// the first system font found for `script`, read once per book. runs on
/// the worker drawing the page.
fn fallback(found: &Fallbacks, script: Script) -> Option<(FontData, u32)> {
    let mut found = found.lock().ok()?;
    found
        .entry(script)
        .or_insert_with(|| {
            font_places(script).iter().find_map(|&(path, index)| {
                let bytes = read_font(Path::new(path))?;
                Some((Arc::new(bytes) as FontData, index))
            })
        })
        .clone()
}

fn settings() -> InterpreterSettings {
    let found = Fallbacks::default();
    InterpreterSettings {
        font_resolver: Arc::new(move |query| match query {
            FontQuery::Standard(s) => Some(s.get_font_data()),
            FontQuery::Fallback(f) => {
                let script = f
                    .character_collection
                    .as_ref()
                    .and_then(|c| match c.family {
                        CidFamily::AdobeKorea1 => Some(Script::Korean),
                        CidFamily::AdobeGB1 | CidFamily::AdobeCNS1 => Some(Script::Chinese),
                        CidFamily::AdobeJapan1 => Some(Script::Japanese),
                        _ => None,
                    });
                script
                    .and_then(|s| fallback(&found, s))
                    .or_else(|| Some(f.pick_standard_font().get_font_data()))
            }
        }),
        ..InterpreterSettings::default()
    }
}

/// an open pdf and its page sizes in points, as shown (rotation applied).
pub struct Book {
    pdf: Pdf,
    settings: InterpreterSettings,
    pub pages: Vec<(f32, f32)>,
}

/// a size from the file, kept sane.
fn sane(v: f32) -> f32 {
    if v.is_finite() && v > 1.0 {
        v.min(20_000.0)
    } else {
        612.0
    }
}

/// opens `bytes` as a pdf. worker only: a big file takes a while.
///
/// # Errors
///
/// says why when the bytes are not a pdf, are broken past repair, or are
/// locked with a password.
pub fn open(bytes: Arc<[u8]>) -> Result<Book, String> {
    let pdf = catch_unwind(AssertUnwindSafe(|| Pdf::new(Arc::new(bytes))))
        .map_err(|_| "the pdf broke the reader".to_owned())?
        .map_err(|e| match e {
            LoadPdfError::Decryption(_) => "it has a password".to_owned(),
            LoadPdfError::Invalid => "not a pdf, or a broken one".to_owned(),
        })?;
    let pages = pdf
        .pages()
        .iter()
        .take(MAX_PAGES)
        .map(|p| {
            let (w, h) = p.render_dimensions();
            (sane(w), sane(h))
        })
        .collect();
    Ok(Book {
        pdf,
        settings: settings(),
        pages,
    })
}

impl Book {
    /// page `index` drawn at `scale` pixels per point, on white. worker only.
    ///
    /// # Errors
    ///
    /// fails for a page that is not there, one too big at this scale, or one
    /// that breaks the renderer.
    pub fn render(&self, index: usize, scale: f32) -> Result<Image, String> {
        let &(pw, ph) = self.pages.get(index).ok_or("no such page")?;
        let page = self.pdf.pages().get(index).ok_or("no such page")?;
        let scale = scale.clamp(0.05, 16.0).min(MAX_SIDE / pw.max(ph));
        if pw * scale * ph * scale > MAX_PIXELS {
            return Err("the page is too big to draw at this zoom".to_owned());
        }
        let pixmap = catch_unwind(AssertUnwindSafe(|| {
            hayro::render(
                page,
                &RenderCache::new(),
                &self.settings,
                &RenderSettings::default(),
                &PixmapSettings {
                    x_scale: scale,
                    y_scale: scale,
                    bg_color: WHITE,
                },
            )
        }))
        .map_err(|_| "the page broke the renderer".to_owned())?;
        let (w, h) = (u32::from(pixmap.width()), u32::from(pixmap.height()));
        if w == 0 || h == 0 {
            return Err("the page drew nothing".to_owned());
        }
        // on an opaque white ground premultiplied and straight are the same
        Ok(Image::new(w, h, pixmap.data_as_u8_slice().to_vec()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// one blue box on a 200 by 100 page.
    const SMALL: &[u8] = b"%PDF-1.4\n1 0 obj<</Type/Catalog/Pages 2 0 R>>endobj\n2 0 obj<</Type/Pages/Kids[3 0 R]/Count 1>>endobj\n3 0 obj<</Type/Page/Parent 2 0 R/MediaBox[0 0 200 100]/Contents 4 0 R>>endobj\n4 0 obj<</Length 27>>stream\n0 0 1 rg 20 20 160 60 re f\nendstream endobj\ntrailer<</Root 1 0 R>>\n%%EOF\n";

    #[test]
    fn a_page_draws_where_its_box_is() {
        let book = open(Arc::from(SMALL)).expect("opens");
        assert_eq!(book.pages, vec![(200.0, 100.0)]);
        let image = book.render(0, 1.0).expect("draws");
        assert_eq!((image.width, image.height), (200, 100));
        let at = |x: usize, y: usize| {
            let i = (y * 200 + x) * 4;
            (image.data[i], image.data[i + 1], image.data[i + 2])
        };
        assert_eq!(at(100, 50), (0, 0, 255), "blue in the box");
        assert_eq!(at(5, 5), (255, 255, 255), "white around it");
    }

    #[test]
    fn scale_doubles_the_pixels() {
        let book = open(Arc::from(SMALL)).expect("opens");
        let image = book.render(0, 2.0).expect("draws");
        assert_eq!((image.width, image.height), (400, 200));
    }

    #[test]
    fn junk_and_cut_files_say_why() {
        for bytes in [&b"not a pdf"[..], &SMALL[..40], b""] {
            assert!(open(Arc::from(bytes)).is_err());
        }
    }

    #[test]
    fn a_missing_page_or_huge_zoom_is_an_error_or_capped() {
        let book = open(Arc::from(SMALL)).expect("opens");
        assert!(book.render(3, 1.0).is_err());
        let image = book.render(0, 1000.0).expect("capped");
        assert!(image.width <= 12_001);
    }

    #[test]
    fn random_bytes_never_panic() {
        let mut seed = 0x9e37_79b9_u32;
        for round in 0..200 {
            let mut bytes = SMALL.to_vec();
            for _ in 0..(round % 12 + 1) {
                seed ^= seed << 13;
                seed ^= seed >> 17;
                seed ^= seed << 5;
                let at = seed as usize % bytes.len();
                bytes[at] = (seed >> 8) as u8;
            }
            if let Ok(book) = open(Arc::from(bytes)) {
                let _ = book.render(0, 1.0);
            }
        }
    }
}
