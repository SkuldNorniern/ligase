//! pictures shown as pictures: png through aurea's decoder, svg through its
//! vector reader. fits the tab at first; the wheel zooms at the pointer, a
//! drag moves it, a double click goes between fit and one to one.

use std::path::{Path, PathBuf};

use aurea::AureaResult;
use aurea::CursorIcon;
use aurea::render::{Image, Point, Rect, VectorImage};
use aurea::ui::{Align, Button, Sense, Ui};

use crate::app::Id;
use crate::lang::Texts;

/// biggest picture file read.
const MAX_FILE: u64 = 64 * 1024 * 1024;
/// biggest decoded picture, in bytes of pixels.
const MAX_PIXELS: usize = 256 * 1024 * 1024;
const MIN_ZOOM: f32 = 0.02;
const MAX_ZOOM: f32 = 64.0;
const FOOT: f32 = 32.0;

/// file kinds shown here: the ones drawn, and the ones known to be pictures
/// that are not drawn yet, so they don't open as text.
const DRAWN: [&str; 2] = ["png", "svg"];
const KNOWN: [&str; 8] = ["jpg", "jpeg", "gif", "webp", "bmp", "ico", "tif", "tiff"];

/// whether `path` opens here instead of in a text editor.
pub fn is_picture(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .is_some_and(|e| DRAWN.contains(&e.as_str()) || KNOWN.contains(&e.as_str()))
}

pub enum Look {
    Raster(Image),
    Vector(VectorImage),
}

pub enum State {
    Loading,
    Ready(Look),
    Failed(String),
}

pub struct Picture {
    pub path: PathBuf,
    pub state: State,
    /// none fits the tab; else screen pixels per picture pixel.
    zoom: Option<f32>,
    /// the picture's middle from the tab's middle, in screen pixels.
    pan: (f32, f32),
    /// where the pointer was last frame while dragging.
    grab: Option<Point>,
    /// the press moved the picture, so its release is no click.
    moved: bool,
    /// when the last click was, for a double click.
    last_click: Option<std::time::Instant>,
    /// the file's size in bytes, once read.
    pub bytes: u64,
}

impl Picture {
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            state: State::Loading,
            zoom: None,
            pan: (0.0, 0.0),
            grab: None,
            moved: false,
            last_click: None,
            bytes: 0,
        }
    }

    pub fn name(&self) -> String {
        self.path
            .file_name()
            .map_or_else(String::new, |n| n.to_string_lossy().into_owned())
    }
}

/// reads and decodes `path`. worker only.
pub fn load(path: &Path) -> (u64, Result<Look, String>) {
    use std::io::Read;
    let file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(e) => return (0, Err(e.to_string())),
    };
    let mut bytes = Vec::new();
    if let Err(e) = file.take(MAX_FILE + 1).read_to_end(&mut bytes) {
        return (0, Err(e.to_string()));
    }
    let size = bytes.len() as u64;
    if size > MAX_FILE {
        return (size, Err(format!("over {} MiB", MAX_FILE / 1024 / 1024)));
    }
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    let look = match ext.as_str() {
        "png" => aurea::clipboard::ClipboardImage::from_png(&bytes, MAX_PIXELS)
            .map(|p| Look::Raster(Image::new(p.width, p.height, p.rgba)))
            .map_err(|e| format!("not a png Ligase can read: {e}")),
        "svg" => std::str::from_utf8(&bytes)
            .map_err(|_| "not UTF-8 text".to_owned())
            .and_then(|t| VectorImage::parse(t).map_err(|e| format!("{e:?}")))
            .map(Look::Vector),
        _ => Err("this kind of picture can't be shown yet".to_owned()),
    };
    (size, look)
}

/// the picture's own size in its pixels.
fn natural(look: &Look) -> (f32, f32) {
    match look {
        #[allow(clippy::cast_precision_loss)]
        Look::Raster(i) => (i.width as f32, i.height as f32),
        Look::Vector(v) => (v.view_box.width.max(1.0), v.view_box.height.max(1.0)),
    }
}

/// zoom that fits `size` into `room`; small pictures stay one to one.
fn fit(size: (f32, f32), room: Rect) -> f32 {
    // sizes are at least one pixel, so this is never nan
    (room.width / size.0)
        .min(room.height / size.1)
        .clamp(MIN_ZOOM, 1.0)
}

/// draws `pic` in `rect`. `n` tells its tab's parts apart.
pub fn show(
    ui: &mut Ui<'_, Id>,
    n: u32,
    pic: &mut Picture,
    rect: Rect,
    texts: &Texts,
) -> AureaResult<()> {
    let palette = ui.theme.palette;
    let small = ui.theme.font(12.0);
    ui.fill(rect, palette.background)?;
    let look = match &pic.state {
        State::Loading => {
            return ui.text(
                texts.tr("Loading…"),
                rect,
                &small,
                palette.dim,
                Align::Center,
            );
        }
        State::Failed(why) => {
            let line = format!("{}: {why}", pic.name());
            ui.text_clipped(&line, rect.inset(16.0), &small, palette.dim, Align::Center)?;
            return Ok(());
        }
        State::Ready(look) => look,
    };
    let (foot, stage) = rect.split_bottom(FOOT, 0.0);
    let size = natural(look);
    let room = stage.inset(16.0);
    let fitted = fit(size, room);
    let mut zoom = pic.zoom.unwrap_or(fitted);

    let input = ui.interact(Id::Picture(n), 0, stage, Sense::Click, true);
    // the wheel zooms about the pointer: the spot under it stays put
    let (_, wy) = ui.wheel_in(stage);
    if wy != 0.0
        && let Some(at) = ui.pointer()
    {
        let next = (zoom * (1.0 + wy.clamp(-3.0, 3.0) * 0.1)).clamp(MIN_ZOOM, MAX_ZOOM);
        let mid = (
            stage.x + stage.width / 2.0 + pic.pan.0,
            stage.y + stage.height / 2.0 + pic.pan.1,
        );
        let k = next / zoom;
        pic.pan.0 += (at.x - mid.0) * (1.0 - k);
        pic.pan.1 += (at.y - mid.1) * (1.0 - k);
        zoom = next;
        pic.zoom = Some(zoom);
        ui.consume_wheel((0.0, wy));
    }
    if input.held {
        if let (Some(at), Some(was)) = (ui.pointer(), pic.grab)
            && (at.x != was.x || at.y != was.y)
        {
            pic.pan.0 += at.x - was.x;
            pic.pan.1 += at.y - was.y;
            pic.moved = true;
        }
        pic.grab = ui.pointer();
    } else {
        pic.grab = None;
    }
    if input.hovered || input.held {
        ui.set_cursor(CursorIcon::Move);
    }
    // two clicks close together, with no drag between
    let mut double = false;
    if input.clicked && !std::mem::take(&mut pic.moved) {
        let now = std::time::Instant::now();
        double = pic
            .last_click
            .is_some_and(|t| now.duration_since(t) < std::time::Duration::from_millis(400));
        pic.last_click = if double { None } else { Some(now) };
    } else if !input.held {
        pic.moved = false;
    }
    if double {
        pic.zoom = if pic.zoom.is_none() && (fitted - 1.0).abs() > f32::EPSILON {
            Some(1.0)
        } else {
            None
        };
        pic.pan = (0.0, 0.0);
        zoom = pic.zoom.unwrap_or(fitted);
    }

    let (w, h) = (size.0 * zoom, size.1 * zoom);
    let at = Rect::new(
        stage.x + (stage.width - w) / 2.0 + pic.pan.0,
        stage.y + (stage.height - h) / 2.0 + pic.pan.1,
        w,
        h,
    );
    ui.clipped(stage, |ui| {
        // a checker under it, so see-through parts read as such
        checker(ui, at.intersection(stage).unwrap_or(at))?;
        match look {
            Look::Raster(image) => ui.ctx.draw_image_rect(image, at),
            Look::Vector(v) => v.draw(ui.ctx, at, palette.text),
        }
    })?;

    // the foot: size, zoom, and the two zooms a click away
    ui.fill(Rect::new(foot.x, foot.y, foot.width, 1.0), palette.border)?;
    let kind = match look {
        Look::Raster(_) => "PNG",
        Look::Vector(_) => "SVG",
    };
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let info = format!(
        "{kind} · {} × {} · {} · {}%",
        size.0.round() as u32,
        size.1.round() as u32,
        human(pic.bytes),
        (zoom * 100.0).round() as u32
    );
    let buttons = Rect::new(foot.right() - 128.0, foot.y + 4.0, 124.0, FOOT - 8.0);
    ui.text_clipped(
        &info,
        Rect::new(
            foot.x + 12.0,
            foot.y,
            (buttons.x - foot.x - 16.0).max(0.0),
            FOOT,
        ),
        &small,
        palette.dim,
        Align::Start,
    )?;
    let (one, fit_at) = buttons.split_right(60.0, 4.0);
    if Button::new(texts.tr("Fit"))
        .quiet()
        .selected(pic.zoom.is_none())
        .show(ui, Id::PictureTool(n * 2), fit_at)?
        .clicked
    {
        pic.zoom = None;
        pic.pan = (0.0, 0.0);
    }
    if Button::new("1:1")
        .quiet()
        .selected(pic.zoom == Some(1.0))
        .show(ui, Id::PictureTool(n * 2 + 1), one)?
        .clicked
    {
        pic.zoom = Some(1.0);
        pic.pan = (0.0, 0.0);
    }
    Ok(())
}

/// a light checker the size of `rect`, in 8 px squares.
fn checker(ui: &mut Ui<'_, Id>, rect: Rect) -> AureaResult<()> {
    let palette = ui.theme.palette;
    ui.fill(rect, palette.raised)?;
    let cell = 8.0;
    let mut y = rect.y;
    let mut row = 0u32;
    // past a few thousand cells it reads as flat anyway
    let cols = (rect.width / cell).ceil() as u32;
    let rows = (rect.height / cell).ceil() as u32;
    if cols.saturating_mul(rows) > 20_000 {
        return Ok(());
    }
    while y < rect.bottom() {
        let mut x = rect.x + if row.is_multiple_of(2) { 0.0 } else { cell };
        while x < rect.right() {
            let w = cell.min(rect.right() - x);
            let h = cell.min(rect.bottom() - y);
            ui.fill(Rect::new(x, y, w, h), palette.hover)?;
            x += cell * 2.0;
        }
        y += cell;
        row += 1;
    }
    Ok(())
}

/// `n` bytes, said short.
fn human(n: u64) -> String {
    #[allow(clippy::cast_precision_loss)]
    let f = n as f64;
    if n < 1024 {
        format!("{n} B")
    } else if n < 1024 * 1024 {
        format!("{:.1} KiB", f / 1024.0)
    } else {
        format!("{:.1} MiB", f / 1024.0 / 1024.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pictures_are_told_by_their_ending() {
        assert!(is_picture(Path::new("a/logo.PNG")));
        assert!(is_picture(Path::new("icon.svg")));
        assert!(is_picture(Path::new("photo.jpeg")));
        assert!(!is_picture(Path::new("main.rs")));
        assert!(!is_picture(Path::new("png")));
    }

    #[test]
    fn small_pictures_stay_one_to_one_and_big_ones_fit() {
        let room = Rect::new(0.0, 0.0, 400.0, 300.0);
        assert!((fit((100.0, 50.0), room) - 1.0).abs() < f32::EPSILON);
        assert!((fit((800.0, 300.0), room) - 0.5).abs() < f32::EPSILON);
    }

    #[test]
    fn broken_pictures_say_so() {
        let dir = std::env::temp_dir().join(format!("ligase-pic-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for (name, bytes) in [
            ("bad.png", b"\x89PNG\r\n\x1a\nnope".as_slice()),
            ("bad.svg", b"<svg".as_slice()),
            ("odd.jpg", b"\xff\xd8".as_slice()),
        ] {
            let p = dir.join(name);
            std::fs::write(&p, bytes).unwrap();
            assert!(load(&p).1.is_err(), "{name}");
        }
        let svg = dir.join("ok.svg");
        std::fs::write(&svg, r#"<svg viewBox="0 0 10 20" xmlns="http://www.w3.org/2000/svg"><rect width="10" height="20"/></svg>"#).unwrap();
        match load(&svg).1 {
            Ok(Look::Vector(v)) => assert!((v.view_box.height - 20.0).abs() < f32::EPSILON),
            _ => panic!("svg not read"),
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
