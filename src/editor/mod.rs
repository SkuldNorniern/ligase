//! source view. draws only the lines in sight; text comes from the buffer
//! snapshot, never a copy of the whole file.

use aurea::AureaResult;
use aurea::render::{Font, Rect};
use aurea::ui::{Align, ScrollArea, Ui};

use crate::app::Id;
use crate::document::{Document, Load, View, ViewId};
use crate::lang::Texts;

const SIZE: f32 = 14.0;
const LINE: f32 = 20.0;
/// longest part of a line drawn; past this it is cut for now.
const MAX_DRAWN: usize = 4096;

const MONO: &str = if cfg!(windows) {
    "Consolas"
} else if cfg!(target_os = "macos") {
    "Menlo"
} else {
    "DejaVu Sans Mono"
};

pub fn show(
    ui: &mut Ui<'_, Id>,
    id: ViewId,
    doc: &Document,
    view: &mut View,
    rect: Rect,
    texts: &Texts,
) -> AureaResult<()> {
    let font = Font::new(MONO, SIZE);
    let palette = ui.theme.palette;
    match &doc.load {
        Load::Loading => {
            return ui.text(texts.tr("loading"), rect, &font, palette.dim, Align::Center);
        }
        Load::Failed(why) => {
            let line = format!("{}: {why}", texts.tr("can't open"));
            return ui.text(&line, rect, &font, palette.danger, Align::Center);
        }
        Load::Ready => {}
    }
    let text = doc.buffer.text();
    let lines = text.line_count();
    let digits = lines.to_string().len().max(3);
    let gutter = ui.text_width(&"0".repeat(digits), &font) + 16.0;
    let height = lines as f32 * LINE + LINE;
    let mut line_buf = String::new();
    ScrollArea::new(&mut view.scroll, height).show(
        ui,
        Id::Editor(id),
        rect,
        |ui, content, seen| {
            let first = ((seen.y - content.y) / LINE).floor().max(0.0) as usize;
            let count = (seen.height / LINE).ceil() as usize + 1;
            for line in first..(first + count).min(lines) {
                let y = content.y + line as f32 * LINE;
                let row = Rect::new(content.x, y, content.width, LINE);
                let (number, body) = row.split_left(gutter, 8.0);
                ui.text(
                    &(line + 1).to_string(),
                    number,
                    &font,
                    palette.faint,
                    Align::End,
                )?;
                let Ok(range) = text.line_range(line) else {
                    continue;
                };
                let mut end = range.end.min(range.start + MAX_DRAWN);
                while !text.is_char_boundary(end) {
                    end -= 1;
                }
                line_buf.clear();
                if let Ok(chunks) = text.chunks(range.start..end) {
                    // tabs as four spaces until real tab stops land
                    for c in chunks.flat_map(str::chars) {
                        match c {
                            '\t' => line_buf.push_str("    "),
                            c => line_buf.push(c),
                        }
                    }
                }
                let shown = line_buf.trim_end_matches('\r');
                ui.text(shown, body, &font, palette.text, Align::Start)?;
            }
            Ok(())
        },
    )?;
    Ok(())
}
