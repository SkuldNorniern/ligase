//! csv and tsv as a table to read: a sticky header, zebra rows, columns
//! sized by their text, numbers on the right. only the rows in sight are drawn.
//! a big file is read through its index instead, a few rows at a time.

use std::collections::HashMap;
use std::ops::Range;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use aurea::render::{Font, FontWeight, Rect};
use aurea::ui::{Align, ScrollArea, Sense, Ui};
use aurea::{AureaResult, CursorIcon};

use crate::app::Id;
use crate::document::ViewId;
use crate::lang::Texts;
use crate::preview::{Ask, ReaderSlot, rows};

/// input past this many bytes is not read in memory; a bigger file is read
/// through its index.
pub const MAX_BYTES: usize = 2 * 1024 * 1024;
/// records kept, the header included.
const MAX_ROWS: usize = 10_000;
const MAX_COLS: usize = 256;
/// a cell keeps this many bytes; the last one is the "…".
const MAX_CELL: usize = 1024;

const CELL: f32 = 14.0;
const HEAD_H: f32 = 28.0;
const ROW_H: f32 = 24.0;
/// the strip above the table, with the way back to the text.
const BAR_H: f32 = 34.0;
/// the strip under a big table, with its row count.
const FOOT_H: f32 = 24.0;
/// rows of a big table kept in memory at most, around the rows in sight.
const MAX_CACHE: usize = 5000;
/// screens read past each side of the rows in sight.
const SCREENS: usize = 2;
/// text inset in a cell, each side.
const PAD: f32 = 10.0;
const MIN_COL: f32 = 60.0;
const MAX_COL: f32 = 320.0;
/// data rows measured for column widths.
const SAMPLE: usize = 200;
/// chars of a cell that get drawn or measured; a column is at most MAX_COL wide.
const SHOWN: usize = 160;

/// a table read from csv or tsv text. the first record is the header.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Table {
    pub head: Vec<String>,
    /// each data row: the 0-based source line it starts on, and its cells.
    pub rows: Vec<(usize, Vec<String>)>,
}

/// reads `text` as csv, or as tsv when its first line has more tabs than
/// commas. never fails: past a cap the rest is dropped.
pub fn parse(text: &str) -> Table {
    let text = cut(text, MAX_BYTES);
    let delim = delimiter(text);
    let mut rec = Records::default();
    let mut quoted = false;
    let mut line = 0;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\r' && chars.peek() == Some(&'\n') {
            continue;
        }
        if c == '\n' || c == '\r' {
            line += 1;
            if quoted {
                rec.cell.push('\n');
            } else {
                rec.end_line(line);
            }
            continue;
        }
        rec.open = true;
        if quoted {
            if c != '"' {
                rec.cell.push(c);
            } else if chars.peek() == Some(&'"') {
                chars.next();
                rec.cell.push('"');
            } else {
                quoted = false;
            }
        } else if c == '"' && rec.cell.is_empty() {
            quoted = true;
        } else if c == delim {
            rec.end_field();
        } else {
            rec.cell.push(c);
        }
    }
    rec.end_row(line);
    let mut out = rec.out;
    let head = if out.is_empty() {
        Vec::new()
    } else {
        out.remove(0).1
    };
    Table { head, rows: out }
}

/// the first `max` bytes of `text`, cut back to a char boundary.
fn cut(text: &str, max: usize) -> &str {
    if text.len() <= max {
        return text;
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

/// tab when the first line has more tabs than commas, else comma.
fn delimiter(text: &str) -> char {
    let first = text.split(['\n', '\r']).next().unwrap_or("");
    if first.matches('\t').count() > first.matches(',').count() {
        '\t'
    } else {
        ','
    }
}

/// a cell cut to `MAX_CELL` bytes, with a "…" on the cut.
fn cap(mut cell: String) -> String {
    if cell.len() <= MAX_CELL {
        return cell;
    }
    let mut end = MAX_CELL - '…'.len_utf8();
    while !cell.is_char_boundary(end) {
        end -= 1;
    }
    cell.truncate(end);
    cell.push('…');
    cell
}

/// the records read so far, and the one being read.
#[derive(Default)]
struct Records {
    out: Vec<(usize, Vec<String>)>,
    row: Vec<String>,
    cell: String,
    /// the line the row being read starts on.
    start: usize,
    /// something was read on this row.
    open: bool,
}

impl Records {
    fn end_field(&mut self) {
        let cell = cap(std::mem::take(&mut self.cell));
        if self.row.len() < MAX_COLS {
            self.row.push(cell);
        }
        self.open = true;
    }

    /// a line break: a blank line is an empty row, as the big file reader
    /// counts it, so row numbers agree on either side of its size.
    fn end_line(&mut self, next: usize) {
        if !self.open && self.out.len() < MAX_ROWS {
            self.out.push((self.start, Vec::new()));
            self.start = next;
            return;
        }
        self.end_row(next);
    }

    /// ends the row; `next` is the line the next row starts on. at the
    /// end of the text nothing read means no row.
    fn end_row(&mut self, next: usize) {
        if self.open {
            self.end_field();
            let cells = std::mem::take(&mut self.row);
            if self.out.len() < MAX_ROWS {
                self.out.push((self.start, cells));
            }
        }
        self.open = false;
        self.start = next;
    }
}

/// the words a table draws, in the language of the app.
#[derive(Clone, Debug, Default)]
pub struct Labels {
    pub edit: String,
    pub indexing: String,
    pub rows: String,
}

impl Labels {
    pub fn new(texts: &Texts) -> Self {
        Self {
            edit: texts.tr("Edit").to_owned(),
            indexing: texts.tr("Indexing…").to_owned(),
            rows: texts.tr("rows").to_owned(),
        }
    }
}

/// one view's table and how it is scrolled.
#[derive(Default)]
pub struct TableState {
    head: Vec<String>,
    rows: Vec<(usize, Vec<String>)>,
    pub scroll: f32,
    hscroll: f32,
    /// column widths and the row number gutter, measured once per table.
    laid: Option<Laid>,
    /// a source line to bring into sight at the next draw.
    pub reveal_line: Option<usize>,
    pub labels: Labels,
}

/// the columns at draw time.
struct Laid {
    widths: Vec<f32>,
    gutter: f32,
}

impl TableState {
    /// a new table; the layout is made again at the next draw.
    pub fn set(&mut self, table: Table) {
        self.head = table.head;
        self.rows = table.rows;
        self.laid = None;
    }

    /// the source line of the row at the top, to go back to the text there.
    pub fn top_line(&self) -> Option<usize> {
        if self.rows.is_empty() {
            return Some(0);
        }
        let i = (self.scroll.max(0.0) / ROW_H) as usize;
        self.rows
            .get(i.min(self.rows.len() - 1))
            .map(|(line, _)| *line)
    }
}

/// a big table: the file read through its index. only the rows near the
/// view are kept, and a row not read yet draws as a bar.
pub struct BigState {
    labels: Labels,
    /// bytes indexed so far, written by the index job.
    progress: Arc<AtomicU64>,
    index: Option<Arc<rows::Index>>,
    /// the reader, opened by the first read on the worker.
    slot: ReaderSlot,
    /// data rows: the index rows less the header.
    count: usize,
    /// index row to its cells. row 0 is the header.
    cache: HashMap<usize, Vec<String>>,
    /// a read is out.
    busy: bool,
    /// why the table can't be read; shown in place of the rows.
    error: Option<String>,
    pub scroll: f32,
    hscroll: f32,
    laid: Option<Laid>,
    /// data rows in sight at the last draw.
    seen_rows: Range<usize>,
}

/// the index rows to keep read for the data rows `seen` out of `count`: the
/// rows in sight plus `screens` screens of each side. index row 0 is the
/// header, so data row k is index row k + 1.
pub fn window(seen: Range<usize>, count: usize, screens: usize) -> Range<usize> {
    if seen.is_empty() {
        return 0..0;
    }
    let span = seen.len().saturating_mul(screens);
    let lo = seen.start.saturating_sub(span).min(count);
    let hi = seen.end.saturating_add(span).min(count);
    (lo + 1)..(hi + 1)
}

impl BigState {
    pub fn new(labels: Labels, progress: Arc<AtomicU64>) -> Self {
        Self {
            labels,
            progress,
            index: None,
            slot: ReaderSlot::default(),
            count: 0,
            cache: HashMap::new(),
            busy: false,
            error: None,
            scroll: 0.0,
            hscroll: 0.0,
            laid: None,
            seen_rows: 0..0,
        }
    }

    /// the index is in; the rows are read from the file from now on.
    pub fn set_index(&mut self, index: Arc<rows::Index>) {
        self.count = index.rows().saturating_sub(1);
        self.index = Some(index);
        self.slot = ReaderSlot::default();
        self.cache.clear();
        self.busy = false;
        self.laid = None;
    }

    /// the table can't be read.
    pub fn fail(&mut self, why: String) {
        self.busy = false;
        self.error = Some(why);
    }

    /// the index and the reader slot, for a read on the worker.
    pub fn source(&self) -> Option<(Arc<rows::Index>, ReaderSlot)> {
        let index = self.index.as_ref()?;
        Some((Arc::clone(index), Arc::clone(&self.slot)))
    }

    /// the index rows to read next: the header first, then the rows near
    /// the view that are not kept yet. none while a read is out.
    pub fn want(&mut self) -> Option<Range<usize>> {
        let index = self.index.as_ref()?;
        if self.busy || self.error.is_some() || index.rows() == 0 {
            return None;
        }
        if !self.cache.contains_key(&0) {
            self.busy = true;
            return Some(0..1);
        }
        let near = window(self.seen_rows.clone(), self.count, SCREENS);
        let lo = near.clone().find(|i| !self.cache.contains_key(i))?;
        let hi = near.rev().find(|i| !self.cache.contains_key(i))?;
        self.busy = true;
        Some(lo..hi + 1)
    }

    /// rows read for `range`, index rows from its start.
    pub fn got(&mut self, range: Range<usize>, rows: Vec<Vec<String>>) {
        self.busy = false;
        if range.start <= SAMPLE {
            // the column widths come from the first rows
            self.laid = None;
        }
        for (i, cells) in range.zip(rows) {
            self.cache.insert(i, cells);
        }
        if self.cache.len() > MAX_CACHE {
            let mid = self.seen_rows.start + self.seen_rows.len() / 2 + 1;
            let half = MAX_CACHE / 2;
            self.cache.retain(|&i, _| i == 0 || i.abs_diff(mid) <= half);
        }
    }
}

struct Fonts {
    prose: Font,
    bold: Font,
    mono: Font,
}

impl Fonts {
    fn new(ui: &Ui<'_, Id>) -> Self {
        let family = ui.theme.font(CELL).family;
        Self {
            prose: Font::new(&family, CELL),
            bold: Font::new(&family, CELL).with_weight(FontWeight::Bold),
            mono: Font::new(crate::editor::MONO, CELL - 1.0),
        }
    }
}

/// a cell as one line: breaks become spaces, and only the start is kept.
fn flat(text: &str) -> String {
    text.chars()
        .take(SHOWN)
        .map(|c| if c == '\n' || c == '\r' { ' ' } else { c })
        .collect()
}

/// a number to set on the right: digits, and a float that parses.
fn numeric(text: &str) -> bool {
    let t = text.trim();
    t.bytes().any(|b| b.is_ascii_digit()) && t.parse::<f64>().is_ok_and(f64::is_finite)
}

/// columns of the header and `rows`, at most MAX_COLS.
fn columns<'a>(head: &[String], rows: impl Iterator<Item = &'a [String]>) -> usize {
    std::iter::once(head.len())
        .chain(rows.map(|r| r.len()))
        .max()
        .unwrap_or(0)
        .min(MAX_COLS)
}

/// column widths from the text of the header and the first `SAMPLE` of `rows`,
/// and the gutter for `count` row numbers.
fn lay<'a>(
    ui: &mut Ui<'_, Id>,
    fonts: &Fonts,
    head: &[String],
    cols: usize,
    rows: impl Iterator<Item = &'a [String]>,
    count: usize,
) -> Laid {
    let mut want = vec![0.0_f32; cols];
    for (c, cell) in head.iter().enumerate().take(cols) {
        want[c] = want[c].max(ui.text_width(&flat(cell), &fonts.bold));
    }
    for row in rows.take(SAMPLE) {
        for (c, cell) in row.iter().enumerate().take(cols) {
            want[c] = want[c].max(ui.text_width(&flat(cell), &fonts.prose));
        }
    }
    let widths = want
        .iter()
        .map(|w| (w + 2.0 * PAD).clamp(MIN_COL, MAX_COL))
        .collect();
    let digits = count.max(1).to_string().len();
    let gutter = ui.text_width(&"9".repeat(digits), &fonts.mono) + 20.0;
    Laid { widths, gutter }
}

/// draws the table in `rect`. gives what a click asked for.
pub fn show(
    ui: &mut Ui<'_, Id>,
    view: ViewId,
    state: &mut TableState,
    rect: Rect,
) -> AureaResult<Option<Ask>> {
    let palette = ui.theme.palette;
    ui.fill(rect, palette.background)?;
    let TableState {
        head,
        rows,
        scroll,
        hscroll,
        laid,
        reveal_line,
        labels,
    } = state;
    let head = &head[..];
    let rows = &rows[..];
    let fonts = Fonts::new(ui);
    let laid = laid.get_or_insert_with(|| {
        let cols = columns(head, rows.iter().map(|(_, r)| r.as_slice()));
        lay(
            ui,
            &fonts,
            head,
            cols,
            rows.iter().map(|(_, r)| r.as_slice()),
            rows.len(),
        )
    });
    if let Some(line) = reveal_line.take() {
        let i = rows.partition_point(|(l, _)| *l <= line).saturating_sub(1);
        *scroll = i as f32 * ROW_H;
    }
    let body = Rect::new(
        rect.x,
        rect.y + BAR_H,
        rect.width,
        (rect.height - BAR_H).max(0.0),
    );
    let width = laid.gutter + laid.widths.iter().sum::<f32>() + PAD;
    let height = HEAD_H + rows.len() as f32 * ROW_H + PAD;
    let laid = &*laid;
    ScrollArea::new(scroll, height)
        .sideways(hscroll, width)
        .show(ui, Id::Reader(view), body, |ui, content, seen| {
            draw(
                ui,
                &fonts,
                laid,
                head,
                |i| rows.get(i).map(|(_, r)| r.as_slice()),
                rows.len(),
                content,
                seen,
            )
            .map(|_| ())
        })?;
    bar(ui, view, rect, &labels.edit)
}

/// draws a big table in `rect`. gives what a click asked for.
pub fn show_big(
    ui: &mut Ui<'_, Id>,
    view: ViewId,
    state: &mut BigState,
    rect: Rect,
) -> AureaResult<Option<Ask>> {
    let palette = ui.theme.palette;
    ui.fill(rect, palette.background)?;
    let fonts = Fonts::new(ui);
    let BigState {
        labels,
        progress,
        index,
        count,
        cache,
        error,
        scroll,
        hscroll,
        laid,
        seen_rows,
        ..
    } = state;
    let cache: &HashMap<usize, Vec<String>> = cache;
    let count = *count;
    let body = Rect::new(
        rect.x,
        rect.y + BAR_H,
        rect.width,
        (rect.height - BAR_H - FOOT_H).max(0.0),
    );
    let font = ui.theme.font(CELL);
    let notice = match (error.as_ref(), index.is_some()) {
        (Some(why), _) => Some((why.clone(), palette.danger)),
        (None, false) => {
            let mb = progress.load(Ordering::Relaxed) >> 20;
            Some((format!("{} {mb} MB", labels.indexing), palette.dim))
        }
        (None, true) => None,
    };
    if let Some((text, color)) = notice {
        ui.text(&text, body, &font, color, Align::Center)?;
    } else {
        let sample = move || {
            (1..=SAMPLE)
                .filter_map(|i| cache.get(&i))
                .map(Vec::as_slice)
        };
        if laid.is_none()
            && let Some(head) = cache.get(&0)
        {
            let cols = columns(head, sample());
            *laid = Some(lay(ui, &fonts, head, cols, sample(), count));
        }
        if let (Some(laid), Some(head)) = (laid.as_ref(), cache.get(&0)) {
            let width = laid.gutter + laid.widths.iter().sum::<f32>() + PAD;
            let height = HEAD_H + count as f32 * ROW_H + PAD;
            let (_, shown) = ScrollArea::new(scroll, height)
                .sideways(hscroll, width)
                .show(ui, Id::Reader(view), body, |ui, content, seen| {
                    draw(
                        ui,
                        &fonts,
                        laid,
                        head,
                        |k| cache.get(&(k + 1)).map(Vec::as_slice),
                        count,
                        content,
                        seen,
                    )
                })?;
            *seen_rows = shown;
        }
    }
    let foot = Rect::new(rect.x, rect.bottom() - FOOT_H, rect.width, FOOT_H);
    ui.fill(Rect::new(rect.x, foot.y, rect.width, 1.0), palette.border)?;
    if index.is_some() {
        let status = format!("{} {}", grouped(count as u64), labels.rows);
        let at = Rect::new(
            foot.x + PAD,
            foot.y,
            (foot.width - 2.0 * PAD).max(0.0),
            FOOT_H,
        );
        ui.text(&status, at, &font, palette.dim, Align::Start)?;
    }
    bar(ui, view, rect, &labels.edit)
}

/// the strip above a table: the way back to the text, as an Edit chip.
fn bar(ui: &mut Ui<'_, Id>, view: ViewId, rect: Rect, edit: &str) -> AureaResult<Option<Ask>> {
    let palette = ui.theme.palette;
    ui.fill(
        Rect::new(rect.x, rect.y + BAR_H - 1.0, rect.width, 1.0),
        palette.border,
    )?;
    let font = ui.theme.font(11.5);
    let w = ui.text_width(edit, &font) + 22.0;
    let at = Rect::new(rect.x + rect.width - w - 22.0, rect.y + 5.0, w, 24.0);
    let input = ui.interact(Id::Reader(view), 1, at, Sense::Click, true);
    let fill = if input.hovered {
        palette.hover
    } else {
        palette.raised
    };
    ui.round(at, 12.0, fill)?;
    ui.text(edit, at, &font, palette.dim, Align::Center)?;
    if input.hovered {
        ui.set_cursor(CursorIcon::Pointer);
    }
    Ok(input.clicked.then_some(Ask::Edit))
}

/// the rows in sight, the sticky header, then the row numbers over the left
/// edge. `row` gives data row `i` when it is read; a bar stands in for one
/// that is not. gives the data rows drawn.
#[allow(clippy::too_many_arguments)]
fn draw<'a>(
    ui: &mut Ui<'_, Id>,
    fonts: &Fonts,
    laid: &Laid,
    head: &[String],
    row: impl Fn(usize) -> Option<&'a [String]>,
    count: usize,
    content: Rect,
    seen: Rect,
) -> AureaResult<Range<usize>> {
    let palette = ui.theme.palette;
    let x0 = content.x + laid.gutter;
    let top = content.y + HEAD_H;
    let first = ((seen.y - top) / ROW_H).floor().max(0.0) as usize;
    let last = (((seen.bottom() - top) / ROW_H).ceil().max(0.0) as usize).min(count);
    let first = first.min(last);
    for i in first..last {
        let y = top + i as f32 * ROW_H;
        if i % 2 == 1 {
            ui.fill(
                Rect::new(seen.x, y, seen.width, ROW_H),
                palette.raised.fade(0.4),
            )?;
        }
        match row(i) {
            Some(cells) => cells_row(ui, fonts, laid, cells, (x0, y, ROW_H), seen, false)?,
            None => placeholder(ui, laid, (x0, y), seen)?,
        }
    }
    ui.fill(
        Rect::new(seen.x, seen.y, seen.width, HEAD_H),
        palette.raised,
    )?;
    cells_row(ui, fonts, laid, head, (x0, seen.y, HEAD_H), seen, true)?;
    ui.fill(
        Rect::new(seen.x, seen.y + HEAD_H - 1.0, seen.width, 1.0),
        palette.border,
    )?;
    // row numbers stay on the left while the table moves sideways
    ui.fill(
        Rect::new(seen.x, seen.y, laid.gutter, seen.height),
        palette.raised,
    )?;
    ui.fill(
        Rect::new(seen.x + laid.gutter - 1.0, seen.y, 1.0, seen.height),
        palette.border,
    )?;
    for i in first..last {
        let y = top + i as f32 * ROW_H;
        let num = (i + 1).to_string();
        let at = Rect::new(seen.x, y, laid.gutter - 8.0, ROW_H);
        ui.text(&num, at, &fonts.mono, palette.dim, Align::End)?;
    }
    Ok(first..last)
}

/// a row not read yet: a faint bar in each column.
fn placeholder(
    ui: &mut Ui<'_, Id>,
    laid: &Laid,
    (x0, y): (f32, f32),
    seen: Rect,
) -> AureaResult<()> {
    let palette = ui.theme.palette;
    let mut x = x0;
    for w in &laid.widths {
        let cx = x;
        x += *w;
        if cx + w < seen.x || cx > seen.right() {
            continue;
        }
        let bar_w = ((w - 2.0 * PAD) * 0.5).max(0.0);
        ui.fill(
            Rect::new(cx + PAD, y + (ROW_H - 8.0) / 2.0, bar_w, 8.0),
            palette.dim.fade(0.2),
        )?;
    }
    Ok(())
}

/// one row's cells from `x0`; `(x0, y, h)` is where it starts. a cell that
/// is wider than its column is clipped to it.
fn cells_row(
    ui: &mut Ui<'_, Id>,
    fonts: &Fonts,
    laid: &Laid,
    cells: &[String],
    (x0, y, h): (f32, f32, f32),
    seen: Rect,
    head: bool,
) -> AureaResult<()> {
    let palette = ui.theme.palette;
    let mut x = x0;
    for (c, w) in laid.widths.iter().enumerate() {
        let cx = x;
        x += *w;
        if cx + w < seen.x || cx > seen.right() {
            continue;
        }
        let Some(cell) = cells.get(c) else { continue };
        let text = flat(cell);
        if text.is_empty() {
            continue;
        }
        let font = if head { &fonts.bold } else { &fonts.prose };
        let cell_rect = Rect::new(cx, y, *w, h);
        let inner = Rect::new(cx + PAD, y, (w - 2.0 * PAD).max(0.0), h);
        let align = if !head && numeric(&text) {
            Align::End
        } else {
            Align::Start
        };
        if ui.text_width(&text, font) > inner.width {
            ui.clipped(cell_rect, |ui| {
                ui.text(&text, inner, font, palette.text, align)
            })?;
        } else {
            ui.text(&text, inner, font, palette.text, align)?;
        }
    }
    Ok(())
}

/// `n` with a comma every three digits, so big counts read at a glance.
fn grouped(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_read_in_groups() {
        assert_eq!(grouped(0), "0");
        assert_eq!(grouped(999), "999");
        assert_eq!(grouped(1_800_000), "1,800,000");
    }

    fn rows(t: &Table) -> Vec<Vec<&str>> {
        t.rows
            .iter()
            .map(|(_, r)| r.iter().map(String::as_str).collect())
            .collect()
    }

    #[test]
    fn plain_csv_and_header() {
        let t = parse("name,qty\nfoo,1\nbar,2\n");
        assert_eq!(t.head, ["name", "qty"]);
        assert_eq!(rows(&t), [vec!["foo", "1"], vec!["bar", "2"]]);
        assert_eq!(t.rows[0].0, 1);
        assert_eq!(t.rows[1].0, 2);
    }

    #[test]
    fn quoted_fields_keep_commas() {
        let t = parse("a,\"b,c\",d\n");
        assert_eq!(t.head, ["a", "b,c", "d"]);
    }

    #[test]
    fn doubled_quote_is_one_quote() {
        let t = parse("h\n\"say \"\"hi\"\"\",2\n");
        assert_eq!(rows(&t), [vec!["say \"hi\"", "2"]]);
    }

    #[test]
    fn newline_inside_quotes_stays_in_the_cell() {
        let t = parse("h1,h2\n\"x\ny\",z\nlast,row\n");
        assert_eq!(rows(&t), [vec!["x\ny", "z"], vec!["last", "row"]]);
        assert_eq!(t.rows[0].0, 1);
        assert_eq!(t.rows[1].0, 3, "lines inside quotes still count");
    }

    #[test]
    fn crlf_and_blank_lines() {
        let t = parse("a,b\r\n1,2\r\n\r\n3,4");
        assert_eq!(t.head, ["a", "b"]);
        // the blank line is an empty row, as in the big file reader
        assert_eq!(rows(&t), [vec!["1", "2"], vec![], vec!["3", "4"]]);
        let t = parse("a\n1\n");
        assert_eq!(rows(&t), [vec!["1"]], "a last line break adds nothing");
    }

    #[test]
    fn tsv_when_tabs_win_the_first_line() {
        let t = parse("name\tqty\tnote,x\nfoo\t1\tbar,y\n");
        assert_eq!(t.head, ["name", "qty", "note,x"]);
        assert_eq!(rows(&t), [vec!["foo", "1", "bar,y"]]);
    }

    #[test]
    fn ragged_rows_keep_their_own_length() {
        let t = parse("a,b,c\n1\n1,2,3,4\n");
        assert_eq!(rows(&t), [vec!["1"], vec!["1", "2", "3", "4"]]);
    }

    #[test]
    fn empty_input_is_empty() {
        let t = parse("");
        assert_eq!(t, Table::default());
        // two blank lines: a blank head and one blank row, as the big reader has it
        assert_eq!(parse("\n\n").rows.len(), 1);
    }

    #[test]
    fn huge_input_is_cut_on_a_char_boundary() {
        let t = parse(&"가".repeat(1_000_000));
        assert_eq!(t.head.len(), 1);
        assert!(t.head[0].len() <= MAX_CELL);
    }

    #[test]
    fn too_many_columns_and_rows_are_dropped() {
        let t = parse(&"a,".repeat(2_000_000));
        assert_eq!(t.head.len(), MAX_COLS);
        let t = parse(&"1\n".repeat(20_000));
        assert_eq!(t.rows.len(), MAX_ROWS - 1);
    }

    #[test]
    fn long_cells_are_cut_with_an_ellipsis() {
        let t = parse(&format!("{}\n", "x".repeat(5000)));
        assert!(t.head[0].len() <= MAX_CELL);
        assert!(t.head[0].ends_with('…'));
        let t = parse(&format!("{}\n", "가".repeat(2000)));
        assert!(t.head[0].len() <= MAX_CELL);
        assert!(t.head[0].ends_with('…'));
    }

    #[test]
    fn numbers_are_recognised() {
        assert!(numeric("12"));
        assert!(numeric(" -3.5e2 "));
        assert!(!numeric("inf"));
        assert!(!numeric("nan"));
        assert!(!numeric("abc"));
        assert!(!numeric(""));
    }

    #[test]
    fn junk_never_panics_and_cells_stay_capped() {
        const ALPHA: [char; 8] = [',', '"', '\n', '\r', '\t', 'a', '1', '가'];
        let mut seed = 0x9e37_79b9_7f4a_7c15_u64;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        for _ in 0..3000 {
            let len = (next() % 40) as usize;
            let text: String = (0..len)
                .map(|_| ALPHA[(next() % ALPHA.len() as u64) as usize])
                .collect();
            let t = parse(&text);
            assert!(t.head.len() <= MAX_COLS);
            for cell in t.head.iter().chain(t.rows.iter().flat_map(|(_, r)| r)) {
                assert!(cell.len() <= MAX_CELL, "cell over the cap: {text:?}");
            }
        }
    }

    #[test]
    fn top_line_follows_the_scroll() {
        let mut state = TableState::default();
        state.set(parse("h\na\nb\nc\n"));
        assert_eq!(state.top_line(), Some(1));
        state.scroll = 2.0 * ROW_H;
        assert_eq!(state.top_line(), Some(3));
        state.scroll = 99.0 * ROW_H;
        assert_eq!(state.top_line(), Some(3), "clamped to the last row");
    }

    #[test]
    fn window_is_the_sight_plus_margin_in_index_rows() {
        assert_eq!(window(10..20, 100, 2), 1..41);
        assert_eq!(window(0..5, 100, 2), 1..16, "clipped at the top");
        assert_eq!(window(95..100, 100, 2), 86..101, "clipped at the end");
        assert_eq!(window(0..0, 100, 2), 0..0, "nothing in sight");
        assert_eq!(window(3..4, 0, 2), 1..1, "no data rows");
    }

    /// a csv file of `n` data rows under a header, removed on drop.
    struct Csv(std::path::PathBuf);

    impl Drop for Csv {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    fn csv(name: &str, n: usize) -> Csv {
        let path =
            std::env::temp_dir().join(format!("ligase_table_{}_{name}.csv", std::process::id()));
        let mut text = String::from("name,qty\n");
        for i in 0..n {
            text.push_str(&format!("row {i},{i}\n"));
        }
        std::fs::write(&path, text).unwrap();
        Csv(path)
    }

    fn big_of(csv: &Csv) -> BigState {
        let index =
            rows::Index::build(&csv.0, |_| {}, &std::sync::atomic::AtomicBool::new(false)).unwrap();
        let mut big = BigState::new(Labels::default(), Arc::new(AtomicU64::new(0)));
        big.set_index(Arc::new(index));
        big
    }

    #[test]
    fn big_reads_the_header_then_the_rows_in_sight() {
        let file = csv("plan", 100);
        let mut big = big_of(&file);
        assert_eq!(big.want(), Some(0..1), "the header first");
        assert_eq!(big.want(), None, "one read at a time");
        big.got(0..1, vec![vec!["name".into(), "qty".into()]]);

        big.seen_rows = 10..20;
        assert_eq!(big.want(), Some(1..41));
        assert_eq!(big.want(), None, "asked already");
        big.got(1..41, vec![vec!["r".into()]; 40]);
        assert_eq!(big.want(), None, "all kept");

        // the view moved on: only the rows past the kept ones are read
        big.seen_rows = 50..60;
        assert_eq!(big.want(), Some(41..81));
    }

    #[test]
    fn big_stops_asking_after_an_error() {
        let file = csv("err", 10);
        let mut big = big_of(&file);
        assert_eq!(big.want(), Some(0..1));
        big.fail("file changed".into());
        assert_eq!(big.want(), None);
        assert!(big.source().is_some());
    }
}
