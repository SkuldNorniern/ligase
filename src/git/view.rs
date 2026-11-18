//! the source control panel: a commit message, the commit button, then the
//! staged files and the changed ones. a click opens a file; the button on a
//! row stages or unstages it.

use std::path::{Path, PathBuf};

use aurea::render::Rect;
use aurea::ui::{Align, Button, ListView, Response, Sense, Splitter, TextField, Ui};
use aurea::{AureaResult, CursorIcon};

use super::graph;
use super::{Change, Status, Tracking};
use crate::app::Id;
use crate::icons;
use crate::lang::Texts;

const FIELD: f32 = 28.0;
const ROW: f32 = 22.0;
const PAD: f32 = 8.0;

/// what the panel asks the app to do.
pub enum Ask {
    Open(PathBuf),
    /// a changed file against HEAD.
    Diff(PathBuf),
    Stage(Vec<String>),
    Unstage(Vec<String>),
    /// throw away the working tree's edits, after asking.
    Discard(Vec<String>),
    Commit,
    /// read this commit in full.
    Show(String),
    /// work in this repository.
    Repo(PathBuf),
    /// the local branches, to switch to.
    Branches,
    Fetch,
    Pull,
    Push,
}

/// the panel's own state.
#[derive(Default)]
pub struct Scm {
    pub message: String,
    /// a fetch, pull, push or switch is out; the app sets this.
    pub busy: bool,
    scroll: f32,
    picked: Option<usize>,
    graph_scroll: f32,
    /// the picked commit, as its index in the history.
    graph_picked: Option<usize>,
    /// the graph's filter text; empty shows all commits.
    pub filter: String,
    /// where the changes end and the graph starts, from the list's top;
    /// none until dragged.
    graph_at: Option<f32>,
    /// the commit picked in the graph, in full once read.
    pub detail: Option<super::graph::Detail>,
}

/// lane colours, by lane, round and round.
fn lane_colour(ui: &Ui<'_, Id>, lane: usize) -> aurea::render::Color {
    let p = ui.theme.palette;
    let all = [p.accent, p.success, p.warning, p.danger, p.text.fade(0.7)];
    all[lane % all.len()]
}

const LANE: f32 = 12.0;

/// the history with its lanes: one row a commit, lines down to its parents,
/// branch names as pills, who and when on the right.
fn graph(
    ui: &mut Ui<'_, Id>,
    scm: &mut Scm,
    status: &Status,
    rect: Rect,
    texts: &Texts,
) -> AureaResult<Option<Ask>> {
    let palette = ui.theme.palette;
    let font = ui.theme.font(12.5);
    let small = ui.theme.font(11.0);
    let (head, list) = rect.split_top(ROW, 2.0);
    // history indices of the commits that pass the filter
    let filtering = !scm.filter.trim().is_empty();
    let shown: Vec<usize> = status
        .history
        .iter()
        .enumerate()
        .filter(|(_, c)| graph::matches(c, &scm.filter))
        .map(|(i, _)| i)
        .collect();
    let label = if filtering {
        format!(
            "{}  {} / {}",
            texts.tr("Graph"),
            shown.len(),
            status.history.len()
        )
    } else {
        format!("{}  {}", texts.tr("Graph"), status.history.len())
    };
    let label_w = ui.text_width(&label, &ui.theme.font(12.0));
    let (label_at, filter_at) = head.inset_xy(4.0, 0.0).split_left(label_w, 8.0);
    ui.text_clipped(
        &label,
        label_at,
        &ui.theme.font(12.0),
        palette.dim,
        Align::Start,
    )?;
    if filter_at.width >= 60.0 {
        let field = Rect::new(
            filter_at.x,
            head.y + 1.0,
            filter_at.width.min(160.0),
            ROW - 2.0,
        );
        TextField::new(&mut scm.filter)
            .placeholder(texts.tr("Filter commits"))
            .max_chars(120)
            .show(ui, Id::ScmFilter, field)?;
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0));
    let widest = status
        .rows
        .iter()
        .map(|r| r.width)
        .max()
        .unwrap_or(1)
        .max(1);
    // a filtered list has no lanes, only a dot column
    let lanes_w = if filtering {
        LANE + 8.0
    } else {
        widest as f32 * LANE + 8.0
    };
    // the picked commit's card takes the bottom of the graph
    let (list, card) = match &scm.detail {
        Some(d) if status.history.iter().any(|c| c.id == d.id) => {
            let h = (list.height * 0.5).clamp(ROW * 4.0, 260.0);
            // split_bottom gives the cut-off bottom part first: the card
            let (card, rest) = list.split_bottom(h, 4.0);
            (rest, Some(card))
        }
        _ => (list, None),
    };
    let mut card_ask = None;
    if let (Some(card), Some(d)) = (card, &scm.detail) {
        card_ask = detail_card(ui, d, card, now, &status.top)?;
    }
    let mut picked = scm
        .graph_picked
        .and_then(|h| shown.iter().position(|&s| s == h));
    let before = picked;
    let response = ListView::new(&mut scm.graph_scroll, shown.len(), ROW)
        .selected(&mut picked)
        .show(ui, Id::ScmGraph, list, |ui, row| {
            let Some(&h) = shown.get(row.index) else {
                return Ok(());
            };
            let (Some(c), Some(r)) = (status.history.get(h), status.rows.get(h)) else {
                return Ok(());
            };
            let lit = row.rect.inset_xy(2.0, 1.0);
            if row.picked {
                ui.round(lit, 4.0, palette.selection)?;
            } else if row.hovered {
                ui.round(lit, 4.0, palette.hover)?;
            }
            let x0 = row.rect.x + 10.0;
            let mid = row.rect.y + ROW / 2.0;
            let x_of = |lane: usize| x0 + lane as f32 * LANE;
            if filtering {
                // no lines between rows when some are hidden
                let colour = lane_colour(ui, 0);
                ui.circle(aurea::render::Point::new(x0, mid), 3.5, colour)?;
            } else {
                draw_lanes(ui, r, x_of, mid, c.parents.len() > 1)?;
            }
            // pills for branches, then the subject; who and when on the right
            let mut x = row.rect.x + lanes_w + 4.0;
            let right = row.rect.x + row.rect.width - 6.0;
            let when = super::graph::ago(now, c.time);
            let meta = format!("{}  {when}", c.author);
            let meta_w = ui.text_width(&meta, &small).min(row.rect.width * 0.35);
            let colour = lane_colour(ui, r.lane);
            for name in &c.refs {
                let w = ui.text_width(name, &small) + 14.0;
                if x + w > right - meta_w - 40.0 {
                    break;
                }
                let pill = Rect::new(x, row.rect.y + 3.0, w, ROW - 6.0);
                let remote = name.contains('/');
                let fill = if remote {
                    palette.raised
                } else {
                    colour.fade(0.28)
                };
                ui.round(pill, (ROW - 6.0) / 2.0, fill)?;
                ui.text(name, pill, &small, palette.text, Align::Center)?;
                x += w + 4.0;
            }
            let subject_colour = if c.outgoing {
                palette.text
            } else {
                palette.text.fade(0.85)
            };
            let body = Rect::new(x, row.rect.y, (right - meta_w - 8.0 - x).max(0.0), ROW);
            ui.text_clipped(&c.subject, body, &font, subject_colour, Align::Start)?;
            let meta_at = Rect::new(right - meta_w, row.rect.y, meta_w, ROW);
            ui.text_clipped(&meta, meta_at, &small, palette.dim, Align::End)?;
            if row.hovered {
                let tip = format!(
                    "{}\n{}  {} UTC\n{}",
                    c.id.get(..12).unwrap_or(&c.id),
                    c.author,
                    graph::utc_stamp(c.time),
                    c.subject
                );
                ui.tooltip(
                    &Response {
                        rect: row.rect,
                        hovered: true,
                        ..Response::default()
                    },
                    &tip,
                );
            }
            Ok(())
        })?;
    if picked != before {
        scm.graph_picked = picked.and_then(|i| shown.get(i).copied());
    }
    let clicked = response
        .row_clicked
        .or((picked != before).then_some(picked).flatten());
    let show = clicked
        .and_then(|i| shown.get(i))
        .and_then(|&h| status.history.get(h))
        .map(|c| Ask::Show(c.id.clone()));
    Ok(card_ask.or(show))
}

/// the lanes at one row: lines to the next row, then the commit's dot, a ring
/// for a merge.
fn draw_lanes(
    ui: &mut Ui<'_, Id>,
    r: &graph::Row,
    x_of: impl Fn(usize) -> f32,
    mid: f32,
    merge: bool,
) -> AureaResult<()> {
    let palette = ui.theme.palette;
    // lines to the next row: straight down, or a soft bend
    for &(a, b) in &r.down {
        let colour = lane_colour(ui, if a == r.lane { b } else { a });
        let (xa, xb) = (x_of(a), x_of(b));
        if a == b {
            ui.line(
                aurea::render::Point::new(xa, mid),
                aurea::render::Point::new(xb, mid + ROW),
                1.6,
                colour,
            )?;
        } else {
            let pts: Vec<aurea::render::Point> = (0..=8)
                .map(|i| {
                    let t = i as f32 / 8.0;
                    let e = t * t * (3.0 - 2.0 * t);
                    aurea::render::Point::new(xa + (xb - xa) * e, mid + ROW * t)
                })
                .collect();
            ui.polyline(&pts, 1.6, colour)?;
        }
    }
    let dot = aurea::render::Point::new(x_of(r.lane), mid);
    let colour = lane_colour(ui, r.lane);
    if merge {
        // a merge: a ring, not a dot
        ui.circle(dot, 4.5, colour)?;
        ui.circle(dot, 2.5, palette.background)?;
    } else {
        ui.circle(dot, 4.0, colour)?;
    }
    Ok(())
}

/// the picked commit: who, when, the hash, the whole message, its files.
fn detail_card(
    ui: &mut Ui<'_, Id>,
    d: &super::graph::Detail,
    rect: Rect,
    now: i64,
    top: &Path,
) -> AureaResult<Option<Ask>> {
    let palette = ui.theme.palette;
    let font = ui.theme.font(12.5);
    let small = ui.theme.font(11.5);
    let bold = ui.theme.bold(12.5);
    ui.round(rect, 8.0, palette.raised)?;
    let inner = rect.inset(10.0);
    let mut y = inner.y;
    let line = |y: f32| Rect::new(inner.x, y, inner.width, 18.0);
    let when = super::graph::ago(now, d.time);
    let short = &d.id[..d.id.len().min(10)];
    ui.text_clipped(
        &format!("{}  ·  {when}", d.author),
        line(y),
        &bold,
        palette.text,
        Align::Start,
    )?;
    ui.text_clipped(short, line(y), &small, palette.dim, Align::End)?;
    y += 22.0;
    for text in d.message.lines().take(6) {
        if y + 18.0 > inner.y + inner.height {
            return Ok(None);
        }
        ui.text_clipped(text, line(y), &font, palette.text.fade(0.9), Align::Start)?;
        y += 18.0;
    }
    y += 6.0;
    let mut ask = None;
    for (i, (letter, path)) in d.files.iter().enumerate() {
        if y + 18.0 > inner.y + inner.height {
            break;
        }
        let row = line(y).inset_xy(-4.0, 0.0);
        let part = u16::try_from(i).unwrap_or(u16::MAX);
        let hit = ui.interact(Id::ScmCardFile(part), 0, row, Sense::Click, true);
        if hit.hovered {
            ui.round(row, 4.0, palette.hover)?;
            ui.set_cursor(CursorIcon::Pointer);
        }
        if hit.clicked {
            ask = Some(Ask::Open(repo_path(top, path)));
        }
        let colour = letter_colour(ui, *letter);
        let (mark, rest) = line(y).split_left(14.0, 4.0);
        ui.text(&letter.to_string(), mark, &small, colour, Align::Center)?;
        ui.text_clipped(path, rest, &small, palette.dim, Align::Start)?;
        y += 18.0;
    }
    Ok(ask)
}

/// `rel` (git's forward slashes) joined onto the repository root `top`.
fn repo_path(top: &Path, rel: &str) -> PathBuf {
    rel.split('/')
        .filter(|s| !s.is_empty())
        .fold(top.to_path_buf(), |p, s| p.join(s))
}

enum Line<'a> {
    Head(&'static str, usize),
    File(&'a Change, bool),
}

fn letter_colour(ui: &Ui<'_, Id>, letter: char) -> aurea::render::Color {
    let palette = ui.theme.palette;
    match letter {
        'A' | '?' => palette.success,
        'D' => palette.danger,
        'U' => palette.warning,
        _ => palette.accent,
    }
}

/// the branch as a pill, then pull, push and fetch as small text buttons. a
/// button with nothing to do is dimmed, still clickable. a sync running
/// shows "Syncing…" in their place.
fn sync_row(
    ui: &mut Ui<'_, Id>,
    scm: &Scm,
    t: &Tracking,
    rect: Rect,
    texts: &Texts,
) -> AureaResult<Option<Ask>> {
    let palette = ui.theme.palette;
    let small = ui.theme.font(11.5);
    let mut ask = None;
    let name_w = ui.text_width(&t.branch, &small) + 18.0;
    let pill = Rect::new(
        rect.x,
        rect.y + 2.0,
        name_w.min(rect.width * 0.5),
        rect.height - 4.0,
    );
    let on_pill = ui.interact(Id::ScmBranch, 0, pill, Sense::Click, true);
    let fill = if on_pill.hovered {
        palette.hover
    } else {
        palette.raised
    };
    ui.round(pill, pill.height / 2.0, fill)?;
    ui.text_clipped(&t.branch, pill, &small, palette.text, Align::Center)?;
    if on_pill.hovered {
        ui.set_cursor(CursorIcon::Pointer);
        let tip = Response {
            rect: pill,
            hovered: true,
            ..Response::default()
        };
        ui.tooltip(&tip, texts.tr("Switch branch"));
    }
    if on_pill.clicked {
        ask = Some(Ask::Branches);
    }
    let mut x = pill.right() + 8.0;
    if scm.busy {
        let at = Rect::new(x, rect.y, (rect.right() - x).max(0.0), rect.height);
        ui.text_clipped(texts.tr("Syncing…"), at, &small, palette.dim, Align::Start)?;
        return Ok(ask);
    }
    let lit = palette.text.fade(0.8);
    let quiet = palette.dim.fade(0.5);
    let specs = [
        (
            Id::ScmPull,
            format!("↓ {}", t.behind),
            t.upstream.is_some() && t.behind > 0,
            "Pull (fast-forward only)",
            Ask::Pull,
        ),
        (
            Id::ScmPush,
            format!("↑ {}", t.ahead),
            t.upstream.is_none() || t.ahead > 0,
            "Push",
            Ask::Push,
        ),
        (Id::ScmFetch, "⟳".to_owned(), true, "Fetch", Ask::Fetch),
    ];
    for (id, label, on, tip, want) in specs {
        let w = ui.text_width(&label, &small) + 14.0;
        if x + w > rect.right() {
            break;
        }
        let at = Rect::new(x, rect.y + 2.0, w, rect.height - 4.0);
        let hit = ui.interact(id, 0, at, Sense::Click, true);
        if hit.hovered {
            ui.round(at, 4.0, palette.hover)?;
            ui.set_cursor(CursorIcon::Pointer);
            let tip_at = Response {
                rect: at,
                hovered: true,
                ..Response::default()
            };
            ui.tooltip(&tip_at, texts.tr(tip));
        }
        let ink = if on { lit } else { quiet };
        ui.text_clipped(&label, at, &small, ink, Align::Center)?;
        if hit.clicked {
            ask = Some(want);
        }
        x += w + 4.0;
    }
    Ok(ask)
}

/// the repositories as pills, each with how many files changed; the one
/// worked in lit. gives the one clicked.
fn repo_pills(
    ui: &mut Ui<'_, Id>,
    repos: &[(PathBuf, usize)],
    current: Option<&Path>,
    rect: Rect,
) -> AureaResult<Option<PathBuf>> {
    let palette = ui.theme.palette;
    let small = ui.theme.font(11.5);
    let mut x = rect.x;
    let mut y = rect.y;
    let mut picked = None;
    for (i, (path, count)) in repos.iter().enumerate() {
        let name = path.file_name().map_or_else(
            || path.display().to_string(),
            |n| n.to_string_lossy().into_owned(),
        );
        let label = if *count > 0 {
            format!("{name}  {count}")
        } else {
            name
        };
        let w = ui.text_width(&label, &small) + 18.0;
        if x + w > rect.right() && x > rect.x {
            x = rect.x;
            y += 26.0;
        }
        let pill = Rect::new(x, y, w.min(rect.width), 22.0);
        let part = u16::try_from(i).unwrap_or(u16::MAX);
        let input = ui.interact(Id::ScmRepo(part), 0, pill, Sense::Click, true);
        let lit = current.is_some_and(|c| c == path.as_path());
        let fill = if lit {
            palette.accent.fade(0.22)
        } else if input.hovered {
            palette.hover
        } else {
            palette.raised
        };
        ui.round(pill, 11.0, fill)?;
        let ink = if lit { palette.text } else { palette.dim };
        ui.text(&label, pill, &small, ink, Align::Center)?;
        if input.hovered {
            ui.set_cursor(CursorIcon::Pointer);
        }
        if input.clicked {
            picked = Some(path.clone());
        }
        x += w + 6.0;
    }
    Ok(picked)
}

/// how tall the pills take for `repos` at `width`.
fn pills_height(ui: &mut Ui<'_, Id>, repos: &[(PathBuf, usize)], width: f32) -> f32 {
    let small = ui.theme.font(11.5);
    let mut x = 0.0;
    let mut rows = 1.0;
    for (path, count) in repos {
        let name = path
            .file_name()
            .map_or(0.0, |n| ui.text_width(&n.to_string_lossy(), &small));
        let w = name + if *count > 0 { 30.0 } else { 0.0 } + 18.0;
        if x + w > width && x > 0.0 {
            x = 0.0;
            rows += 1.0;
        }
        x += w + 6.0;
    }
    rows * 26.0
}

pub fn show(
    ui: &mut Ui<'_, Id>,
    scm: &mut Scm,
    status: Option<&Status>,
    repos: &[(PathBuf, usize)],
    rect: Rect,
    texts: &Texts,
) -> AureaResult<Option<Ask>> {
    let palette = ui.theme.palette;
    let font = ui.theme.font(13.0);
    let small = ui.theme.font(12.0);
    let mut inner = rect.inset(PAD);
    // several repositories: pills to switch between them on top
    if repos.len() > 1 {
        let h = pills_height(ui, repos, inner.width);
        let (pills, rest) = inner.split_top(h, 6.0);
        if let Some(path) = repo_pills(ui, repos, status.map(|s| s.top.as_path()), pills)? {
            return Ok(Some(Ask::Repo(path)));
        }
        inner = rest;
    }
    let Some(status) = status else {
        let hint = texts.tr("Not a Git repository");
        ui.text(hint, inner, &small, palette.dim, Align::Center)?;
        return Ok(None);
    };
    let mut ask = None;
    let (field, rest) = inner.split_top(FIELD, 6.0);
    let (button, rest) = rest.split_top(FIELD, 8.0);
    let typed = TextField::new(&mut scm.message)
        .placeholder(texts.tr("commit message"))
        .max_chars(2000)
        .show(ui, Id::ScmField, field)?;
    let pressed = Button::new(texts.tr("Commit")).show(ui, Id::ScmCommit, button)?;
    if typed.submitted || pressed.clicked {
        ask = Some(Ask::Commit);
    }
    // the sync row under the commit button, once git has said what it follows
    let list = match &status.tracking {
        Some(t) => {
            let (row, rest) = rest.split_top(ROW + 4.0, 4.0);
            if let Some(sync) = sync_row(ui, scm, t, row, texts)? {
                ask = Some(sync);
            }
            rest
        }
        None => rest,
    };

    // the graph takes the lower part when there is history to show
    let list = if status.history.is_empty() {
        list
    } else {
        let wanted = (status.changes.len() as f32 + 3.0) * ROW;
        let mut at = scm
            .graph_at
            .unwrap_or_else(|| wanted.clamp(ROW * 3.0, list.height * 0.45))
            .min(list.height - ROW * 3.0)
            .max(ROW * 2.0);
        let (top, bottom, moved) = Splitter::new(&mut at)
            .stacked()
            .min(ROW * 2.0, ROW * 3.0)
            .threshold(2.0)
            .whole_pixels(true)
            .label(texts.tr("Graph"))
            .show(ui, Id::ScmSplit, list)?;
        if moved.changed {
            scm.graph_at = Some(at);
        }
        if let Some(show) = graph(ui, scm, status, bottom, texts)? {
            ask = Some(show);
        }
        top
    };
    let staged: Vec<&Change> = status.changes.iter().filter(|c| c.staged()).collect();
    let changed: Vec<&Change> = status.changes.iter().filter(|c| c.unstaged()).collect();
    if staged.is_empty() && changed.is_empty() {
        let note = Rect::new(list.x, list.y + 8.0, list.width, ROW);
        ui.text(
            texts.tr("no changes"),
            note,
            &small,
            palette.dim,
            Align::Center,
        )?;
        return Ok(ask);
    }
    let mut lines: Vec<Line<'_>> = Vec::new();
    if !staged.is_empty() {
        lines.push(Line::Head("Staged Changes", staged.len()));
        lines.extend(staged.iter().map(|c| Line::File(c, true)));
    }
    lines.push(Line::Head("Changes", changed.len()));
    lines.extend(changed.iter().map(|c| Line::File(c, false)));

    let mut picked = scm.picked;
    let mut button_ask = None;
    let response = ListView::new(&mut scm.scroll, lines.len(), ROW)
        .selected(&mut picked)
        .show(ui, Id::ScmList, list, |ui, row| {
            let Some(line) = lines.get(row.index) else {
                return Ok(());
            };
            match line {
                Line::Head(label, n) => {
                    let text = format!("{}  {n}", texts.tr(label));
                    let at = row.rect.inset_xy(4.0, 0.0);
                    ui.text_clipped(&text, at, &small, palette.dim, Align::Start)?;
                }
                Line::File(change, staged) => {
                    let lit = row.rect.inset_xy(2.0, 1.0);
                    if row.picked {
                        ui.round(lit, 4.0, palette.selection)?;
                    } else if row.hovered {
                        ui.round(lit, 4.0, palette.hover)?;
                    }
                    let letter = if *staged { change.index } else { change.tree };
                    let letter = if letter == '?' { 'U' } else { letter };
                    // git's letter, the file's icon, its name and folder in the
                    // middle, the buttons on the right
                    let untracked = change.tree == '?';
                    let can_discard = !*staged && !untracked;
                    let buttons = if can_discard { 2.0 } else { 1.0 };
                    let inner = row.rect.inset_xy(8.0, 0.0);
                    let (mark, rest) = inner.split_left(14.0, 4.0);
                    let (icon_at, rest) = rest.split_left(16.0, 6.0);
                    // split_right gives the right part first
                    let (button_at, body) = rest.split_right(buttons * 22.0, 4.0);
                    let colour = letter_colour(ui, if untracked { '?' } else { letter });
                    ui.text(&letter.to_string(), mark, &small, colour, Align::Center)?;
                    let icon_box = Rect::new(icon_at.x, icon_at.y + (ROW - 16.0) / 2.0, 16.0, 16.0);
                    let icon = icons::for_path(&change.path, false, false);
                    icons::draw(ui, icon, icon_box, palette.dim)?;
                    let (dir, name) = match change.rel.rsplit_once('/') {
                        Some((dir, name)) => (dir, name),
                        None => ("", change.rel.as_str()),
                    };
                    // a deleted file reads struck through, in git's colour
                    let name_colour = if letter == 'D' { colour } else { palette.text };
                    let name_w = ui.text_width(name, &font).min(body.width);
                    let (name_at, dir_at) = body.split_left(name_w, 8.0);
                    ui.text_clipped(name, name_at, &font, name_colour, Align::Start)?;
                    if letter == 'D' {
                        let mid = name_at.y + ROW / 2.0;
                        ui.fill(Rect::new(name_at.x, mid, name_w, 1.0), colour)?;
                    }
                    ui.text_clipped(dir, dir_at, &small, palette.dim, Align::Start)?;
                    // discard (tracked edits only), then stage or unstage
                    let part = u16::try_from(row.index * 2).unwrap_or(u16::MAX - 1);
                    let mut x = button_at.x;
                    if can_discard {
                        let at = Rect::new(x, row.rect.y + 1.0, 22.0, ROW - 2.0);
                        let hit = Button::icon_only(icons::image("discard"))
                            .quiet()
                            .icon_size(14.0)
                            .show(ui, Id::ScmRow(part + 1), at)?;
                        if hit.clicked {
                            button_ask = Some(Ask::Discard(vec![change.rel.clone()]));
                        }
                        x += 22.0;
                    }
                    let at = Rect::new(x, row.rect.y + 1.0, 22.0, ROW - 2.0);
                    let icon = icons::image(if *staged { "minus" } else { "plus" });
                    let hit = Button::icon_only(icon).quiet().icon_size(14.0).show(
                        ui,
                        Id::ScmRow(part),
                        at,
                    )?;
                    if hit.clicked {
                        let mut rels = vec![change.rel.clone()];
                        rels.extend(change.old.clone());
                        button_ask = Some(if *staged {
                            Ask::Unstage(rels)
                        } else {
                            Ask::Stage(vec![change.rel.clone()])
                        });
                    }
                }
            }
            Ok(())
        })?;
    scm.picked = picked;
    if button_ask.is_some() {
        return Ok(button_ask);
    }
    let opened = response
        .row_clicked
        .or(response.activated.then_some(picked).flatten())
        .and_then(|i| match lines.get(i) {
            // a file git had before opens as a diff, a new one as itself
            Some(Line::File(change, _)) if change.tree != 'D' && change.index != 'D' => {
                if matches!(change.tree, '?') || change.index == 'A' {
                    Some(Ask::Open(change.path.clone()))
                } else {
                    Some(Ask::Diff(change.path.clone()))
                }
            }
            _ => None,
        });
    Ok(opened.or(ask))
}
