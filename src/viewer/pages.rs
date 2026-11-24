//! a pdf in a tab: its pages one under another, fit to the width at first.
//! pages are drawn by pdfium on a worker at the zoom in use and kept for a
//! while; one still coming shows as a blank sheet.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use aurea::AureaResult;
use aurea::render::{Image, Rect};
use aurea::ui::{Align, Button, Sense, Ui};

use crate::app::Id;
use crate::lang::Texts;
use crate::pdf::Book;

const GAP: f32 = 12.0;
const MARGIN: f32 = 16.0;
const FOOT: f32 = 32.0;
const MIN_ZOOM: f32 = 0.1;
const MAX_ZOOM: f32 = 8.0;
/// pages kept drawn either side of the ones in sight.
const KEEP: usize = 3;
/// a drawn page this far from the zoom in use is drawn again.
const STALE: f32 = 0.15;

pub struct Pages {
    pub book: Arc<Book>,
    /// drawn pages by number, with the zoom they were drawn at.
    drawn: HashMap<usize, (f32, Image)>,
    /// pages out to the worker, by number and zoom.
    asked: HashSet<(usize, u32)>,
    /// pages this frame wants drawn, for the app to send out.
    pub want: Vec<(usize, f32)>,
    scroll: f32,
    /// none fits the width.
    zoom: Option<f32>,
}

impl Pages {
    pub fn new(book: Book) -> Self {
        Self {
            book: Arc::new(book),
            drawn: HashMap::new(),
            asked: HashSet::new(),
            want: Vec::new(),
            scroll: 0.0,
            zoom: None,
        }
    }

    /// a page came back from the worker.
    pub fn drawn(&mut self, page: usize, zoom: f32, image: Image) {
        self.asked.retain(|(p, _)| *p != page);
        self.drawn.insert(page, (zoom, image));
    }

    /// a page that could not be drawn is not asked for again at that zoom.
    pub fn failed(&mut self, page: usize) {
        self.asked.retain(|(p, _)| *p != page);
    }
}

fn key(zoom: f32) -> u32 {
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let k = (zoom * 1000.0).round() as u32;
    k
}

pub fn show(
    ui: &mut Ui<'_, Id>,
    n: u32,
    pages: &mut Pages,
    rect: Rect,
    texts: &Texts,
) -> AureaResult<()> {
    let palette = ui.theme.palette;
    let small = ui.theme.font(12.0);
    ui.fill(rect, palette.background)?;
    let sizes = pages.book.pages.clone();
    if sizes.is_empty() {
        ui.text(
            texts.tr("The PDF has no pages"),
            rect,
            &small,
            palette.dim,
            Align::Center,
        )?;
        return Ok(());
    }
    let (foot, stage) = rect.split_bottom(FOOT, 0.0);
    let widest = sizes.iter().map(|s| s.0).fold(1.0_f32, f32::max);
    let fitted = ((stage.width - MARGIN * 2.0) / widest).clamp(MIN_ZOOM, MAX_ZOOM);
    let mut zoom = pages.zoom.unwrap_or(fitted);

    // the wheel scrolls; with ctrl (cmd on a mac) it zooms about the pointer
    let input = ui.interact(Id::Picture(n), 0, stage, Sense::Click, true);
    let (_, wy) = ui.wheel_in(stage);
    if wy != 0.0 {
        if ui.modifiers().command() {
            let next = (zoom * (1.0 + wy.clamp(-3.0, 3.0) * 0.1)).clamp(MIN_ZOOM, MAX_ZOOM);
            let at = ui.pointer().map_or(stage.height / 2.0, |p| p.y - stage.y);
            pages.scroll = (pages.scroll + at) * (next / zoom) - at;
            zoom = next;
            pages.zoom = Some(zoom);
        } else {
            pages.scroll -= wy * 48.0;
        }
        ui.consume_wheel((0.0, wy));
    }
    let _ = input;

    // where each page sits, from the top of the stack
    let mut tops = Vec::with_capacity(sizes.len());
    let mut y = MARGIN;
    for (_, h) in &sizes {
        tops.push(y);
        y += h * zoom + GAP;
    }
    let total = y - GAP + MARGIN;
    pages.scroll = pages.scroll.clamp(0.0, (total - stage.height).max(0.0));
    let (top, bottom) = (pages.scroll, pages.scroll + stage.height);
    let seen: Vec<usize> = (0..sizes.len())
        .filter(|&i| tops[i] < bottom && tops[i] + sizes[i].1 * zoom > top)
        .collect();
    let current = seen
        .iter()
        .copied()
        .find(|&i| tops[i] + sizes[i].1 * zoom > top + stage.height / 3.0)
        .or(seen.first().copied())
        .unwrap_or(0);

    pages.want.clear();
    ui.clipped(stage, |ui| {
        for &i in &seen {
            let (w, h) = (sizes[i].0 * zoom, sizes[i].1 * zoom);
            let at = Rect::new(
                stage.x + (stage.width - w) / 2.0,
                stage.y + tops[i] - top,
                w,
                h,
            );
            // a soft edge under the sheet
            ui.round(at.inset(-2.0), 3.0, palette.border.fade(0.6))?;
            ui.fill(at, aurea::render::Color::rgb(255, 255, 255))?;
            match pages.drawn.get(&i) {
                Some((z, image)) => {
                    ui.ctx.draw_image_rect(image, at)?;
                    if (z - zoom).abs() / zoom > STALE {
                        pages.want.push((i, zoom));
                    }
                }
                None => pages.want.push((i, zoom)),
            }
        }
        Ok(())
    })?;
    // the page after the ones in sight comes early, so scrolling finds it
    if let Some(&last) = seen.last()
        && last + 1 < sizes.len()
        && !pages.drawn.contains_key(&(last + 1))
    {
        pages.want.push((last + 1, zoom));
    }
    pages
        .want
        .retain(|(p, z)| !pages.asked.contains(&(*p, key(*z))));
    for (p, z) in &pages.want {
        pages.asked.insert((*p, key(*z)));
    }
    // pages far from sight let their pixels go
    let (lo, hi) = (
        seen.first().map_or(0, |f| f.saturating_sub(KEEP)),
        seen.last().map_or(0, |l| l + KEEP),
    );
    pages.drawn.retain(|p, _| (lo..=hi).contains(p));

    // the foot: where, how big, and the zooms a click away
    ui.fill(Rect::new(foot.x, foot.y, foot.width, 1.0), palette.border)?;
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let info = format!(
        "PDF · {} {} / {} · {}%",
        texts.tr("page"),
        current + 1,
        sizes.len(),
        (zoom * 100.0).round() as u32
    );
    let buttons = Rect::new(foot.right() - 196.0, foot.y + 4.0, 192.0, FOOT - 8.0);
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
    let (plus, rest) = buttons.split_right(32.0, 4.0);
    let (minus, rest) = rest.split_right(32.0, 4.0);
    let (_, width) = rest.split_right(112.0, 4.0);
    if Button::new(texts.tr("Fit width"))
        .quiet()
        .selected(pages.zoom.is_none())
        .show(ui, Id::PictureTool(n * 4), width)?
        .clicked
    {
        pages.zoom = None;
    }
    if Button::new("−")
        .quiet()
        .show(ui, Id::PictureTool(n * 4 + 1), minus)?
        .clicked
    {
        pages.zoom = Some((zoom / 1.2).clamp(MIN_ZOOM, MAX_ZOOM));
    }
    if Button::new("+")
        .quiet()
        .show(ui, Id::PictureTool(n * 4 + 2), plus)?
        .clicked
    {
        pages.zoom = Some((zoom * 1.2).clamp(MIN_ZOOM, MAX_ZOOM));
    }
    Ok(())
}
