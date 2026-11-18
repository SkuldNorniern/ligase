//! source view and editing. draws only the lines in sight; text comes from
//! the buffer snapshot, never a copy of the whole file.

pub mod abbrev;
pub mod cursor;
pub mod find;
pub mod fold;
pub(crate) mod hover;
pub mod input;
pub mod lines;
pub mod multi;
pub mod pairs;
pub mod rows;
pub mod snippet;

use std::ops::Range;
use std::time::Duration;

use aurea::render::{Color, Font, Point, Rect};
use aurea::ui::{Align, Channel, Palette, ScrollArea, Sense, Ui};
use aurea::{AureaResult, CursorIcon};
use resolvase::{Encoding, TextSnapshot};

use crate::app::Id;
use crate::document::{Document, Documents, Load, View, ViewId};
use crate::lang::Texts;
use crate::lsp::{Diag, Severity};
use crate::settings::{CaretStyle, LineNumbers, Whitespace};
use crate::syntax::{self, Spans};
use cursor::Selection;
use input::Action;
use lines::Change;

/// font size before any zoom.
pub const SIZE: f32 = 14.0;
/// longest part of a line drawn; past this it is cut for now.
const MAX_DRAWN: usize = 4096;
/// room the scroll bar takes on the right; clicks there are its.
const BAR: f32 = 14.0;
const TAB: &str = "    ";
/// px a sideways wheel notch moves.
const H_STEP: f32 = 40.0;
/// a caret left alone this long stops blinking.
const BLINK_FOR: Duration = Duration::from_secs(10);
const BLINK_HALF: Duration = Duration::from_millis(530);
/// how fast the caret glides, per second.
const CARET_RATE: f32 = 38.0;
/// parts and channels of the view's eased values.
const PART_H: u16 = 2;
const PART_CARET: u16 = 3;
const PART_HOVER: u16 = 4;
/// the card for what the pointer rests on.
const PART_PEEK: u16 = 5;
/// sticky lines take parts from here, one each.
const PART_STICKY: u16 = 16;
/// at most this many sticky lines.
const MAX_STICKY: usize = 4;
const H_SCROLL: u16 = 0;
const CARET_X: u16 = 0;
const CARET_Y: u16 = 1;

pub(crate) const MONO: &str = if cfg!(windows) {
    "Consolas"
} else if cfg!(target_os = "macos") {
    "Menlo"
} else {
    "DejaVu Sans Mono"
};

/// how typing behaves, from the settings.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EditOpts {
    /// one level of indent.
    pub unit: String,
    /// brackets and quotes typed in pairs.
    pub auto_pairs: bool,
    /// abbreviations by language, from packages.
    pub abbrevs: std::collections::HashMap<&'static str, std::sync::Arc<abbrev::Abbrevs>>,
}

impl Default for EditOpts {
    fn default() -> Self {
        Self {
            unit: TAB.to_owned(),
            auto_pairs: true,
            abbrevs: std::collections::HashMap::new(),
        }
    }
}

/// does `action` on `view` with the default options.
#[cfg(test)]
pub fn act(docs: &mut Documents, view: ViewId, action: Action) -> bool {
    act_with(docs, view, action, &EditOpts::default())
}

/// does `action` on `view`, at every caret. true when something changed.
pub fn act_with(docs: &mut Documents, view: ViewId, action: Action, opts: &EditOpts) -> bool {
    let Some((doc, v)) = docs.editable(view) else {
        return false;
    };
    let text = doc.buffer.snapshot();
    match action {
        Action::Indent | Action::Outdent if snippet::step(v, action == Action::Outdent) => {
            return true;
        }
        Action::Collapse => v.snippet = None,
        _ => {}
    }
    match action {
        Action::Move(motion, extend) => {
            let before = v.sel;
            let row_step = match motion {
                cursor::Motion::Up => Some(true),
                cursor::Motion::Down => Some(false),
                _ => None,
            };
            let stepped = v.wrap_cols.zip(row_step).and_then(|(cols, up)| {
                rows::step(
                    &text,
                    v.sel.head,
                    up,
                    cols,
                    opts.unit.len().max(1),
                    &mut v.goal,
                )
            });
            v.sel = match stepped {
                Some(head) => Selection {
                    anchor: if extend { v.sel.anchor } else { head },
                    head,
                },
                None => cursor::motion(&text, v.sel, motion, extend, &mut v.goal),
            };
            if !v.folded.is_empty() {
                // moves step over folded lines instead of opening them
                let hidden = fold::hidden(&text, &v.folded);
                let to = text.line_of(v.sel.head).unwrap_or(0);
                if let Some((a, b)) = fold::is_hidden(to, &hidden) {
                    let forward = v.sel.head > before.head;
                    let vertical = matches!(
                        motion,
                        cursor::Motion::Up
                            | cursor::Motion::Down
                            | cursor::Motion::PageUp(_)
                            | cursor::Motion::PageDown(_)
                    );
                    let head = if forward && b + 1 >= text.line_count() {
                        // nothing after the fold: stay put
                        None
                    } else if forward && vertical {
                        let line = b + 1;
                        let column = v.goal.unwrap_or(0);
                        let end = cursor::line_end(&text, line);
                        let width = text.point(end, Encoding::Utf32).map_or(0, |p| p.column);
                        Some(
                            text.offset(
                                resolvase::Point::new(line, column.min(width)),
                                Encoding::Utf32,
                            )
                            .unwrap_or(end),
                        )
                    } else if forward {
                        text.line_start(b + 1).ok()
                    } else {
                        // back: the end of the line the fold shows on
                        Some(cursor::line_end(&text, a - 1))
                    };
                    match head {
                        Some(head) => {
                            v.sel.head = head;
                            if !extend {
                                v.sel.anchor = head;
                            }
                        }
                        None => v.sel = before,
                    }
                }
            }
            for s in &mut v.extra {
                // extra carets keep no goal column of their own
                *s = cursor::motion(&text, *s, motion, extend, &mut None);
            }
            multi::merge(v);
            v.reveal = true;
            v.preedit = None;
            // typing after a move is a new undo step
            doc.buffer.break_join();
            return true;
        }
        Action::Collapse => {
            if v.extra.is_empty() {
                v.sel = Selection::caret(v.sel.head);
            } else {
                v.extra.clear();
            }
            v.preedit = None;
            return true;
        }
        Action::Preedit(composing, at) => {
            v.preedit = (!composing.is_empty()).then_some((composing, at));
            v.reveal = true;
            return true;
        }
        Action::Insert(_) => v.preedit = None,
        _ => {}
    }
    let place = Place {
        text: &text,
        lb: doc.line_break(),
        language: doc.language,
        opts,
    };
    each(docs, view, |_, sel| place.change(sel, &action))
}

/// works out an edit for every caret against one snapshot and makes them
/// as one transaction. each caret's new place moves by what the edits
/// before it added or took. true when something changed.
fn each(
    docs: &mut Documents,
    view: ViewId,
    mut plan: impl FnMut(usize, Selection) -> Option<(Change, bool)>,
) -> bool {
    let Some(v) = docs.view(view) else {
        return false;
    };
    let (sels, primary) = multi::all(v);
    let mut edits = Vec::new();
    let mut out = Vec::with_capacity(sels.len());
    let mut delta = 0isize;
    let mut join = true;
    // end of the last edit taken; a caret whose edit reaches back over it
    // (two carets in one indent, say) is left out, not allowed to clash
    let mut reach = 0;
    for (i, &sel) in sels.iter().enumerate() {
        let planned = plan(i, sel).filter(|(change, _)| {
            change.edits.iter().all(|(range, _)| {
                range.start >= reach || (range.is_empty() && range.start == reach)
            })
        });
        let Some((change, joins)) = planned else {
            out.push(multi::shifted(sel, delta));
            continue;
        };
        if let Some(end) = change.edits.iter().map(|(r, _)| r.end).max() {
            reach = reach.max(end);
        }
        join &= joins;
        out.push(multi::shifted(change.sel, delta));
        for (range, new) in change.edits {
            delta += new.len() as isize - range.len() as isize;
            edits.push((range, new));
        }
    }
    if edits.is_empty() && out == sels {
        return false;
    }
    // extra carets are set below; mapping each through the edit first
    // would cost carets x edits for nothing
    let extra = docs.view_mut(view).map(|v| std::mem::take(&mut v.extra));
    let done = edits.is_empty() || docs.edit(view, edits, join);
    if !done {
        if let (Some(v), Some(extra)) = (docs.view_mut(view), extra) {
            v.extra = extra;
        }
        return false;
    }
    if let Some(v) = docs.view_mut(view) {
        multi::set_all(v, out, primary);
        v.goal = None;
        v.reveal = true;
    }
    done
}

/// what one caret's edit needs to know about its document.
struct Place<'t> {
    text: &'t TextSnapshot,
    lb: &'static str,
    language: Option<&'static str>,
    opts: &'t EditOpts,
}

impl Place<'_> {
    /// the edit `action` makes at `sel` alone, and whether it joins the
    /// typing run. none when it does nothing there.
    fn change(&self, sel: Selection, action: &Action) -> Option<(Change, bool)> {
        let text = self.text;
        let lb = self.lb;
        let unit = self.opts.unit.as_str();
        let pairs_on = self.opts.auto_pairs;
        let plain = |range: Range<usize>, new: String| Change {
            sel: Selection::caret(range.start + new.len()),
            edits: vec![(range, new)],
        };
        // a whole abbreviation before the caret: the char typed after it, or
        // tab, turns it over
        let abbrevs = self.language.and_then(|l| self.opts.abbrevs.get(l));
        if sel.is_empty()
            && let Some(abbrevs) = abbrevs
        {
            let turned = match action {
                Action::Insert(typed) => {
                    let mut chars = typed.chars();
                    match (chars.next(), chars.next()) {
                        (Some(c), None) => abbrevs.expand(text, sel.head, c),
                        _ => None,
                    }
                }
                Action::Indent => abbrevs.finish(text, sel.head),
                _ => None,
            };
            if let Some((range, with)) = turned {
                return Some((plain(range, with), true));
            }
        }
        Some(match action {
            Action::Insert(typed) => match pairs::typed(text, sel, typed).filter(|_| pairs_on) {
                Some(change) => (change, true),
                None => (plain(sel.range(), typed.clone()), true),
            },
            Action::Indent if lines::block(text, sel).0 != lines::block(text, sel).1 => {
                (lines::indent_lines(text, sel, unit), false)
            }
            Action::Indent => (plain(sel.range(), unit.to_owned()), true),
            Action::Outdent => (lines::outdent_lines(text, sel, unit), false),
            Action::Newline => {
                let start = sel.range().start;
                let indent = cursor::indent_before(text, start);
                let open = pairs::char_before(text, start);
                let close = pairs::char_after(text, sel.range().end);
                let opens = matches!(open, Some('(' | '[' | '{'))
                    || (open == Some(':') && self.language == Some("python"));
                if opens
                    && open
                        .and_then(pairs::closer)
                        .is_some_and(|c| Some(c) == close)
                {
                    // between a pair: the caret gets its own line, deeper
                    let first = format!("{lb}{indent}{unit}");
                    let change = Change {
                        sel: Selection::caret(start + first.len()),
                        edits: vec![(sel.range(), format!("{first}{lb}{indent}"))],
                    };
                    return Some((change, false));
                }
                let deeper = if opens { unit } else { "" };
                (plain(sel.range(), format!("{lb}{indent}{deeper}")), false)
            }
            Action::OpenLine(above) => {
                let line = text.line_of(sel.head).unwrap_or(0);
                let indent = cursor::indent_before(text, cursor::line_end(text, line));
                let change = if *above {
                    let at = text.line_start(line).unwrap_or(0);
                    Change {
                        sel: Selection::caret(at + indent.len()),
                        edits: vec![(at..at, format!("{indent}{lb}"))],
                    }
                } else {
                    let at = cursor::line_end(text, line);
                    Change {
                        sel: Selection::caret(at + lb.len() + indent.len()),
                        edits: vec![(at..at, format!("{lb}{indent}"))],
                    }
                };
                (change, false)
            }
            Action::DeleteWord(forward) if sel.is_empty() => {
                let motion = if *forward {
                    cursor::Motion::WordRight
                } else {
                    cursor::Motion::WordLeft
                };
                let to = cursor::motion(text, sel, motion, false, &mut None).head;
                if to == sel.head {
                    return None;
                }
                (
                    plain(to.min(sel.head)..to.max(sel.head), String::new()),
                    false,
                )
            }
            Action::Backspace if sel.is_empty() => {
                if let Some(change) = pairs::backspace(text, sel.head).filter(|_| pairs_on) {
                    return Some((change, true));
                }
                // in the indent, back to the last tab stop
                let line = text.line_of(sel.head).unwrap_or(0);
                let start = text.line_start(line).unwrap_or(0);
                let before = text.slice(start..sel.head).unwrap_or_default();
                if !before.is_empty() && before.bytes().all(|b| b == b' ') {
                    let take = match before.len() % unit.len().max(1) {
                        0 => unit.len().max(1),
                        n => n,
                    };
                    return Some((plain(sel.head - take..sel.head, String::new()), true));
                }
                let prev = text.prev_grapheme(sel.head).ok().flatten()?;
                (plain(prev..sel.head, String::new()), true)
            }
            Action::Delete if sel.is_empty() => {
                let next = text.next_grapheme(sel.head).ok().flatten()?;
                (plain(sel.head..next, String::new()), true)
            }
            Action::Backspace | Action::Delete | Action::DeleteWord(_) => {
                (plain(sel.range(), String::new()), false)
            }
            Action::Move(..) | Action::Collapse | Action::Preedit(..) => return None,
        })
    }
}

/// makes `change` and puts its selection on the view.
pub fn apply(docs: &mut Documents, view: ViewId, change: Change, join: bool) -> bool {
    let done = change.edits.is_empty() || docs.edit(view, change.edits, join);
    if done && let Some(v) = docs.view_mut(view) {
        v.sel = change.sel;
        v.extra.clear();
        v.goal = None;
        v.reveal = true;
    }
    done
}

/// a line edit the app asks for by command.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineOp {
    Move(bool),
    Copy(bool),
    Delete,
    Comment,
    Select,
}

/// does `op` on the lines under the selection. true when something changed.
pub fn line_op(docs: &mut Documents, view: ViewId, op: LineOp) -> bool {
    let Some((doc, v)) = docs.editable(view) else {
        return false;
    };
    let text = doc.buffer.snapshot();
    let lb = doc.line_break();
    let sel = v.sel;
    let change = match op {
        LineOp::Move(up) => lines::move_lines(&text, sel, up, lb),
        LineOp::Copy(up) => Some(lines::copy_lines(&text, sel, up, lb)),
        LineOp::Delete => Some(lines::delete_lines(&text, sel)),
        LineOp::Comment => lines::comment_for(doc.language)
            .and_then(|mark| lines::toggle_comment(&text, sel, mark)),
        LineOp::Select => {
            // again and again takes the next line too
            let (a, b) = lines::block(&text, sel);
            let from = text.line_start(a).unwrap_or(0);
            let to = cursor::line_at(&text, text.line_start(b).unwrap_or(0)).head;
            let to = if sel.range() == (from..to) {
                cursor::line_at(&text, to).head
            } else {
                to
            };
            v.sel = Selection {
                anchor: from,
                head: to,
            };
            v.extra.clear();
            v.reveal = true;
            return true;
        }
    };
    // each line edit is its own undo step
    doc.buffer.break_join();
    change.is_some_and(|c| apply(docs, view, c, false))
}

/// the whole line under the caret with its break; what copy and cut take
/// when nothing is selected.
pub fn caret_line(docs: &Documents, view: ViewId) -> Option<(Range<usize>, String)> {
    let v = docs.view(view)?;
    let doc = docs.doc(v.doc)?;
    let text = doc.buffer.text();
    let range = cursor::line_at(text, v.sel.head).range();
    let mut line = text.slice(range.clone()).ok()?;
    if !line.ends_with('\n') {
        line.push_str(doc.line_break());
    }
    Some((range, line))
}

/// pastes a whole line copied with nothing selected: it goes in above the
/// caret's line, the caret stays where it was in the text.
pub fn paste_line(docs: &mut Documents, view: ViewId, line: &str) -> bool {
    let Some(v) = docs.view(view) else {
        return false;
    };
    let Some(doc) = docs.doc(v.doc) else {
        return false;
    };
    let text = doc.buffer.text();
    let at = text
        .line_start(text.line_of(v.sel.head).unwrap_or(0))
        .unwrap_or(0);
    let sel = Selection::caret(v.sel.head + line.len());
    apply(
        docs,
        view,
        Change {
            edits: vec![(at..at, line.to_owned())],
            sel,
        },
        false,
    )
}

/// the selected text of `view`; with more carets, each selection on its
/// own line.
pub fn selected(docs: &Documents, view: ViewId) -> Option<String> {
    let v = docs.view(view)?;
    let doc = docs.doc(v.doc)?;
    let (sels, _) = multi::all(v);
    if sels.iter().all(|s| s.is_empty()) {
        return None;
    }
    let text = doc.buffer.text();
    let parts: Vec<String> = sels
        .iter()
        .map(|s| text.slice(s.range()).unwrap_or_default())
        .collect();
    Some(parts.join(doc.line_break()))
}

/// pastes `text` over the selection, line breaks made to match the file.
pub fn paste(docs: &mut Documents, view: ViewId, text: &str) -> bool {
    let Some((doc, v)) = docs.editable(view) else {
        return false;
    };
    let plain = text.replace("\r\n", "\n");
    let fitted = if doc.crlf {
        plain.replace('\n', "\r\n")
    } else {
        plain
    };
    let carets = v.extra.len() + 1;
    // as many lines as carets: one line each, like vs code
    let lines: Vec<&str> = fitted.split(doc.line_break()).collect();
    let spread = carets > 1 && lines.len() == carets;
    each(docs, view, |i, sel| {
        let new = if spread { lines[i] } else { fitted.as_str() };
        Some((
            Change {
                sel: Selection::caret(sel.range().start + new.len()),
                edits: vec![(sel.range(), new.to_owned())],
            },
            false,
        ))
    })
}

pub fn select_all(docs: &mut Documents, view: ViewId) {
    if let Some((doc, v)) = docs.editable(view) {
        v.sel = Selection {
            anchor: 0,
            head: doc.buffer.len(),
        };
        v.extra.clear();
    }
}

/// tidies a file before it is saved: trailing spaces off, a final line
/// break on, as the settings say. one undo step; true when it changed.
pub fn tidy_for_save(docs: &mut Documents, view: ViewId, trim: bool, final_newline: bool) -> bool {
    let Some((doc, _)) = docs.editable(view) else {
        return false;
    };
    let text = doc.buffer.snapshot();
    let lb = doc.line_break();
    let mut edits = Vec::new();
    if trim {
        for line in 0..text.line_count() {
            let Ok(start) = text.line_start(line) else {
                continue;
            };
            let end = cursor::line_end(&text, line);
            let Ok(body) = text.slice(start..end) else {
                continue;
            };
            let kept = body.trim_end_matches([' ', '\t']).len();
            if kept < body.len() {
                edits.push((start + kept..end, String::new()));
            }
        }
    }
    if final_newline && !text.is_empty() {
        let ends = text
            .slice(text.len().saturating_sub(1)..text.len())
            .is_ok_and(|s| s == "\n");
        if !ends {
            edits.push((text.len()..text.len(), lb.to_owned()));
        }
    }
    if edits.is_empty() {
        return false;
    }
    doc.buffer.break_join();
    // the caret stays where it was, or as near as the text allows
    let keep = docs.view(view).map(|v| (v.sel, v.extra.clone()));
    let done = docs.edit(view, edits, false);
    if let (true, Some((sel, extra))) = (done, keep)
        && let Some((doc, v)) = docs.editable(view)
    {
        let len = doc.buffer.len();
        let clamp = move |s: Selection| Selection {
            anchor: s.anchor.min(len),
            head: s.head.min(len),
        };
        v.extra = extra.into_iter().map(clamp).collect();
        v.sel = clamp(sel);
        multi::merge(v);
    }
    done
}

/// a fold command the app asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FoldOp {
    Fold,
    Unfold,
    FoldAll,
    UnfoldAll,
}

pub fn fold_op(docs: &mut Documents, view: ViewId, op: FoldOp) -> bool {
    let Some((doc, v)) = docs.editable(view) else {
        return false;
    };
    let text = doc.buffer.snapshot();
    let line = text.line_of(v.sel.head).unwrap_or(0);
    match op {
        FoldOp::Fold => {
            // the fold starting on the caret's line, else the smallest one
            // around the caret
            let target = fold::starting_on(&text, &doc.folds, line).or_else(|| {
                doc.folds
                    .iter()
                    .filter(|f| f.start <= v.sel.head && v.sel.head <= f.end)
                    .filter(|f| fold::hides(&text, f).is_some())
                    .min_by_key(|f| f.len())
                    .cloned()
            });
            let Some(target) = target else {
                return false;
            };
            if !v.folded.contains(&target) {
                v.folded.push(target.clone());
            }
            // the caret goes to the line left in sight
            if fold::hides(&text, &target).is_some_and(|(a, b)| (a..=b).contains(&line)) {
                v.sel = Selection::caret(cursor::line_end(
                    &text,
                    text.line_of(target.start).unwrap_or(0),
                ));
                v.extra.clear();
            }
        }
        FoldOp::Unfold => {
            let before = v.folded.len();
            v.folded.retain(|f| {
                let first = text.line_of(f.start).unwrap_or(0);
                let last = text.line_of(f.end).unwrap_or(first);
                !(first..=last).contains(&line)
            });
            if v.folded.len() == before {
                return false;
            }
        }
        FoldOp::FoldAll => {
            v.folded = doc
                .folds
                .iter()
                .filter(|f| fold::hides(&text, f).is_some())
                .cloned()
                .collect();
        }
        FoldOp::UnfoldAll => v.folded.clear(),
    }
    // folding never leaves the caret in a hidden line, or it would open
    // again on the next frame
    if let Some(head) = fold::caret_out(&text, &v.folded, v.sel.head) {
        v.sel = Selection::caret(head);
        v.extra.clear();
    }
    v.reveal = true;
    true
}

/// a multi caret command the app asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CaretOp {
    AddNext,
    SelectMatches,
    AddAbove,
    AddBelow,
}

pub fn caret_op(docs: &mut Documents, view: ViewId, op: CaretOp) -> bool {
    let Some((doc, v)) = docs.editable(view) else {
        return false;
    };
    let text = doc.buffer.snapshot();
    let done = match op {
        CaretOp::AddNext => multi::add_next(&text, v),
        CaretOp::SelectMatches => multi::select_all_matches(&text, v),
        CaretOp::AddAbove => multi::add_vertical(&text, v, true),
        CaretOp::AddBelow => multi::add_vertical(&text, v, false),
    };
    if done {
        v.reveal = true;
        doc.buffer.break_join();
    }
    done
}

/// what a frame of the view tells the app.
#[derive(Default)]
pub struct Shown {
    /// the view has the keyboard.
    pub focused: bool,
    /// caret box in window space, for the ime window.
    pub caret: Option<Rect>,
    /// the mouse placed the caret; typing after it is a new undo step.
    pub pressed: bool,
    /// when the caret blinks next; none once it stays on.
    pub blink: Option<Duration>,
    /// ctrl and a click: the app asks for the definition at the caret.
    pub definition: bool,
    /// a right click: where, for the menu.
    pub menu: Option<Point>,
    /// a part of the breadcrumb was clicked.
    pub crumb: Option<Crumb>,
    /// the text offset under the pointer, and where the pointer is.
    pub pointing: Option<(usize, Point)>,
}

/// what a click on the breadcrumb asks for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Crumb {
    /// the files under this folder, `/` at the end; empty for the root.
    Folder(String),
    /// the symbols of the file.
    Symbols,
    /// read the file as a page.
    Read,
}

/// one line laid out for drawing: tabs opened up, with where each char
/// sits in the buffer and in the drawn text.
struct Laid {
    drawn: String,
    /// (buffer offset, drawn offset) per char, plus one past the end.
    marks: Vec<(usize, usize)>,
}

/// draws one line in runs: plain text between spans, each span in its colour.
fn paint_line(
    ui: &mut Ui<'_, Id>,
    font: &Font,
    laid: &Laid,
    spans: &Spans,
    line: Range<usize>,
    body: Rect,
    (plain, dark): (Color, bool),
) -> AureaResult<()> {
    let run = |ui: &mut Ui<'_, Id>, from: usize, to: usize, color: Color| {
        let (a, b) = (laid.drawn_at(from), laid.drawn_at(to));
        if a >= b {
            return Ok(());
        }
        let x = ui.text_width(&laid.drawn[..a], font);
        let at = Rect::new(body.x + x, body.y, (body.width - x).max(0.0), body.height);
        ui.text(&laid.drawn[a..b], at, font, color, Align::Start)
    };
    // the span just before the line can reach into it, like a block comment
    let first = spans
        .partition_point(|(r, _)| r.start < line.start)
        .saturating_sub(1);
    let mut at = line.start;
    for (range, kind) in &spans[first..] {
        if range.start >= line.end {
            break;
        }
        let from = range.start.max(at);
        let to = range.end.min(line.end);
        if from >= to {
            continue;
        }
        run(ui, at, from, plain)?;
        run(ui, from, to, syntax::colour_on(*kind, dark))?;
        at = to;
    }
    run(ui, at, line.end, plain)
}

/// wavy underlines under the diagnostics that touch this line.
fn squiggles(
    ui: &mut Ui<'_, Id>,
    font: &Font,
    laid: &Laid,
    diags: &[Diag],
    line: Range<usize>,
    body: Rect,
    line_h: f32,
) -> AureaResult<()> {
    let palette = ui.theme.palette;
    for diag in diags {
        if diag.range.start > line.end || diag.range.end < line.start {
            continue;
        }
        let from = diag.range.start.max(line.start);
        let to = diag.range.end.min(line.end);
        let x0 = body.x + laid.x_of(ui, font, from);
        // an empty range still gets a mark the width of one char
        let x1 = (body.x + laid.x_of(ui, font, to)).max(x0 + ui.text_width("m", font));
        let color = severity_colour(&palette, diag.severity);
        let y = body.y + line_h - 3.0;
        let mut points = Vec::new();
        let mut x = x0;
        let mut up = false;
        while x < x1 {
            points.push(Point::new(x, if up { y - 1.5 } else { y + 1.5 }));
            x += 3.0;
            up = !up;
        }
        points.push(Point::new(x1, y));
        ui.polyline(&points, 1.0, color)?;
    }
    Ok(())
}

/// a line's text without its line break, `\r` included, so no click or
/// drag can land between `\r` and `\n`.
fn span(text: &TextSnapshot, line: usize) -> Range<usize> {
    text.line_start(line).unwrap_or(0)..cursor::line_end(text, line)
}

fn lay(text: &TextSnapshot, range: Range<usize>, tab: &str) -> Laid {
    let mut end = range.end.min(range.start + MAX_DRAWN);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    let mut laid = Laid {
        drawn: String::new(),
        marks: Vec::new(),
    };
    let mut at = range.start;
    for c in text
        .chunks(range.start..end)
        .into_iter()
        .flatten()
        .flat_map(str::chars)
    {
        laid.marks.push((at, laid.drawn.len()));
        match c {
            '\t' => laid.drawn.push_str(tab),
            c => laid.drawn.push(c),
        }
        at += c.len_utf8();
    }
    laid.marks.push((at, laid.drawn.len()));
    laid
}

impl Laid {
    fn drawn_at(&self, offset: usize) -> usize {
        self.marks
            .iter()
            .rev()
            .find(|(b, _)| *b <= offset)
            .map_or(0, |(_, d)| *d)
    }

    fn x_of(&self, ui: &mut Ui<'_, Id>, font: &Font, offset: usize) -> f32 {
        ui.text_width(&self.drawn[..self.drawn_at(offset)], font)
    }

    /// buffer offset closest to `x`.
    fn offset_at(&self, ui: &mut Ui<'_, Id>, font: &Font, x: f32) -> usize {
        // marks are in order, so their widths are too; find the first past x
        let (mut lo, mut hi) = (0, self.marks.len() - 1);
        while lo < hi {
            let mid = (lo + hi) / 2;
            if ui.text_width(&self.drawn[..self.marks[mid].1], font) < x {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        if lo > 0 {
            let before = ui.text_width(&self.drawn[..self.marks[lo - 1].1], font);
            let after = ui.text_width(&self.drawn[..self.marks[lo].1], font);
            if x - before < after - x {
                return self.marks[lo - 1].0;
            }
        }
        self.marks[lo].0
    }
}

/// how a view is drawn this frame, from the app.
pub struct Look<'a> {
    /// font size; lines are a bit taller.
    pub size: f32,
    /// time since the caret last moved or text was typed, for the blink.
    pub idle: Duration,
    /// language server hover text, while the caret stays where it was asked.
    pub hover: Option<&'a str>,
    /// the card for what the pointer rests on, and where it rests.
    pub peek: Option<(&'a str, Point)>,
    /// find bar matches, sorted, and the picked one.
    pub found: &'a [Range<usize>],
    pub current: Option<Range<usize>>,
    pub settings: &'a crate::settings::Settings,
    /// the theme is dark; picks the syntax colours.
    pub dark: bool,
    /// the file's path from the workspace folder, `/` between parts; none
    /// for an untitled one.
    pub crumbs: Option<&'a str>,
}

/// height of the strip naming the file above the text.
const CRUMBS: f32 = 24.0;

/// the file's path above the text, folders dim and the file itself bright,
/// then the symbol the caret is in with its kind.
fn crumbs(
    ui: &mut Ui<'_, Id>,
    id: ViewId,
    path: &str,
    symbol: Option<&crate::syntax::Symbol>,
    strip: Rect,
) -> AureaResult<Option<Crumb>> {
    let mut picked = None;
    // a part lights up under the pointer; a click on it is the ask
    let mut hit = |ui: &mut Ui<'_, Id>, n: u16, at: Rect, ask: Crumb| {
        let input = ui.interact(Id::Crumb(id), n, at, Sense::Click, true);
        if input.hovered {
            ui.set_cursor(CursorIcon::Pointer);
            ui.round(at.inset_xy(-3.0, 3.0), 4.0, ui.theme.palette.hover)?;
        }
        if input.clicked {
            picked = Some(ask);
        }
        AureaResult::Ok(())
    };
    let palette = ui.theme.palette;
    let font = ui.theme.font(12.0);
    let parts: Vec<&str> = path.split('/').filter(|p| !p.is_empty()).collect();
    let mut x = strip.x + 12.0;
    let end = strip.x + strip.width - 12.0;
    let sep = |ui: &mut Ui<'_, Id>, x: &mut f32| {
        let at = Rect::new(*x, strip.y, 16.0, strip.height);
        *x += 16.0;
        ui.text("›", at, &font, palette.faint, Align::Center)
    };
    for (i, part) in parts.iter().enumerate() {
        let last = i + 1 == parts.len();
        let w = ui.text_width(part, &font);
        let room = Rect::new(x, strip.y, (end - x).max(0.0), strip.height);
        // a folder lists its files; the file name lists its neighbours
        let folder: String = parts[..if last { i } else { i + 1 }]
            .iter()
            .map(|p| format!("{p}/"))
            .collect();
        let n = u16::try_from(i).unwrap_or(u16::MAX - 1);
        hit(
            ui,
            n,
            Rect::new(x, strip.y, w.min(room.width), strip.height),
            Crumb::Folder(folder),
        )?;
        let colour = if last { palette.text } else { palette.dim };
        ui.text_clipped(part, room, &font, colour, Align::Start)?;
        x += w;
        if x >= end {
            return Ok(picked);
        }
        if !last {
            sep(ui, &mut x)?;
        }
    }
    if let Some(symbol) = symbol
        && x + 40.0 < end
    {
        sep(ui, &mut x)?;
        let tag = ui.text_width(symbol.kind, &font) + 8.0;
        let room = Rect::new(x, strip.y, tag, strip.height);
        let w = tag + ui.text_width(&symbol.name, &font);
        hit(
            ui,
            u16::MAX,
            Rect::new(x, strip.y, w.min(end - x), strip.height),
            Crumb::Symbols,
        )?;
        ui.text(
            symbol.kind,
            room,
            &font,
            palette.accent.fade(0.8),
            Align::Start,
        )?;
        x += tag;
        let room = Rect::new(x, strip.y, (end - x).max(0.0), strip.height);
        ui.text_clipped(&symbol.name, room, &font, palette.text, Align::Start)?;
    }
    Ok(picked)
}

/// a small rounded button at the right end of `strip`. true when clicked.
fn chip(ui: &mut Ui<'_, Id>, id: ViewId, strip: Rect, label: &str) -> AureaResult<bool> {
    let palette = ui.theme.palette;
    let font = ui.theme.font(11.5);
    let w = ui.text_width(label, &font) + 20.0;
    let at = Rect::new(strip.x + strip.width - w - 14.0, strip.y + 3.0, w, strip.height - 6.0);
    let input = ui.interact(Id::Crumb(id), u16::MAX - 1, at, Sense::Click, true);
    let fill = if input.hovered { palette.hover } else { palette.raised };
    ui.round(at, at.height / 2.0, fill)?;
    ui.text(label, at, &font, palette.dim, Align::Center)?;
    if input.hovered {
        ui.set_cursor(CursorIcon::Pointer);
    }
    Ok(input.clicked)
}

/// where problems and find hits are in the whole file, in the strip the
/// scroll bar runs in: hits on the left half, problems on the right, the
/// caret's line across. by line, so folds and wraps only bend it a little.
fn overview(
    ui: &mut Ui<'_, Id>,
    track: Rect,
    doc: &Document,
    found: &[Range<usize>],
    caret: usize,
) -> AureaResult<()> {
    let text = doc.buffer.text();
    let lines = text.line_count().max(1) as f32;
    let palette = ui.theme.palette;
    // an offset from an older text (a late diagnostic) has no line; no mark
    let y_of = |at: usize| {
        let line = text.line_of(at).ok()? as f32;
        Some((track.y + line / lines * track.height).min(track.y + track.height - 2.0))
    };
    let half = (track.width / 2.0).floor();
    // one mark a pixel row is plenty, however many hits share it
    let mut last = f32::NEG_INFINITY;
    for hit in found {
        let Some(y) = y_of(hit.start).map(f32::floor) else {
            continue;
        };
        if y > last {
            let mark = Rect::new(track.x + 2.0, y, half - 3.0, 2.0);
            ui.fill(mark, palette.warning.fade(0.7))?;
            last = y;
        }
    }
    for diag in &doc.diags {
        if diag.severity == Severity::Hint {
            continue;
        }
        let Some(y) = y_of(diag.range.start).map(f32::floor) else {
            continue;
        };
        let mark = Rect::new(track.x + half + 1.0, y, half - 3.0, 3.0);
        ui.fill(mark, severity_colour(&palette, diag.severity))?;
    }
    let y = y_of(caret).unwrap_or(track.y).floor();
    ui.fill(
        Rect::new(track.x + 2.0, y, track.width - 4.0, 1.0),
        palette.text.fade(0.5),
    )?;
    Ok(())
}

/// line height for a font size and a multiple of it.
pub fn line_height(size: f32, factor: f32) -> f32 {
    (size * factor).round()
}

/// caret on or off after `idle`, and how long until that flips. a caret
/// left alone stops blinking, so an idle window draws nothing.
fn blink(idle: Duration) -> (bool, Option<Duration>) {
    if idle >= BLINK_FOR {
        return (true, None);
    }
    let half = BLINK_HALF.as_millis();
    let ms = idle.as_millis();
    let on = (ms / half).is_multiple_of(2);
    let left = u64::try_from(half - ms % half).unwrap_or(0);
    (on, Some(Duration::from_millis(left)))
}

/// what to light up for the word under the caret, or the selected text:
/// (needle, whole words only).
fn needle(text: &TextSnapshot, sel: Selection) -> Option<(String, bool)> {
    let range = sel.range();
    if !sel.is_empty() {
        if range.len() > 200 {
            return None;
        }
        let picked = text.slice(range).ok()?;
        return (!picked.contains('\n') && !picked.trim().is_empty()).then_some((picked, false));
    }
    let word = cursor::word_at(text, sel.head).range();
    if word.is_empty() || word.len() > 100 {
        return None;
    }
    let found = text.slice(word).ok()?;
    found
        .chars()
        .all(|c| c.is_alphanumeric() || c == '_')
        .then_some((found, true))
}

/// where `needle` sits in `line`, which starts at buffer offset `at`.
fn hits(line: &str, at: usize, needle: &str, whole: bool) -> Vec<Range<usize>> {
    let word = |c: Option<char>| c.is_some_and(|c| c.is_alphanumeric() || c == '_');
    line.match_indices(needle)
        .filter(|(i, _)| {
            !whole
                || (!word(line[..*i].chars().next_back())
                    && !word(line[i + needle.len()..].chars().next()))
        })
        .map(|(i, _)| at + i..at + i + needle.len())
        .collect()
}

/// indent depth of a line in steps of `TAB`, or none for a blank line.
fn depth(line: &str, tab: usize) -> Option<usize> {
    let tab = tab.max(1);
    let mut cols = 0;
    for c in line.chars() {
        match c {
            ' ' => cols += 1,
            '\t' => cols += tab,
            _ => return Some(cols / tab),
        }
    }
    None
}

pub fn show(
    ui: &mut Ui<'_, Id>,
    id: ViewId,
    doc: &Document,
    view: &mut View,
    rect: Rect,
    texts: &Texts,
    look: &Look<'_>,
) -> AureaResult<Shown> {
    let mut shown = Shown::default();
    let settings = look.settings;
    let font = Font::new(settings.editor_font.as_deref().unwrap_or(MONO), look.size);
    let line_h = line_height(look.size, settings.line_height);
    let tab = " ".repeat(settings.tab_size.max(1));
    let tab = tab.as_str();
    let palette = ui.theme.palette;
    // the path on top also keeps the first line off the tab bar
    let rect = match look.crumbs.filter(|_| settings.breadcrumbs) {
        Some(path) => {
            let (strip, rest) = rect.split_top(CRUMBS, 0.0);
            // the named thing the caret is in, or last passed, ends the path
            let head = view.sel.head;
            let symbol = doc
                .symbols
                .iter()
                .filter(|s| s.at <= head)
                .max_by_key(|s| s.at);
            shown.crumb = crumbs(ui, id, path, symbol, strip)?;
            // markdown offers its page at the far end
            if doc.language == Some("markdown") && chip(ui, id, strip, texts.tr("Read"))? {
                shown.crumb = Some(Crumb::Read);
            }
            rest
        }
        None => rect,
    };
    match &doc.load {
        Load::Loading => {
            ui.text(texts.tr("loading"), rect, &font, palette.dim, Align::Center)?;
            return Ok(shown);
        }
        Load::Failed(why) => {
            let line = format!("{}: {why}", texts.tr("can't open"));
            ui.text(&line, rect, &font, palette.danger, Align::Center)?;
            return Ok(shown);
        }
        Load::Ready => {}
    }
    let text = doc.buffer.text();
    let lines = text.line_count();
    let digits = lines.to_string().len().max(3);
    // numbers, then fold arrows, then the problem mark
    let gutter = ui.text_width(&"0".repeat(digits), &font) + 34.0;
    view.page = ((rect.height / line_h) as usize).saturating_sub(1).max(1);
    if let Some((at, encoding)) = view.goto.take() {
        // past the end or mid char: lands on the nearest place before it
        view.sel = Selection::caret(crate::lsp::offset_near(text, at, encoding));
        view.extra.clear();
        view.reveal = true;
    }
    // a fold the parser no longer has (its closing brace deleted, say) goes;
    // one the caret is inside (find, go to, undo) opens
    let here = text.line_of(view.sel.head).unwrap_or(0);
    view.folded.retain(|f| {
        doc.folds.contains(f) && !fold::hides(text, f).is_some_and(|(a, b)| (a..=b).contains(&here))
    });
    // wrapping: as many columns as fit, never fewer than a few
    let char_w = ui.text_width(" ", &font).max(1.0);
    let wrap_cols = settings.word_wrap.then(|| {
        (((rect.width - gutter - BAR - 12.0) / char_w)
            .floor()
            .max(8.0)) as usize
    });
    let mut cache = std::mem::take(&mut view.wrap);
    let rows = rows::Rows::new(
        text,
        &view.folded,
        wrap_cols,
        settings.tab_size.max(1),
        &mut cache,
    );
    view.wrap_cols = rows.wraps().then_some(rows.cols);
    let row_count = rows.total(lines);
    // the last line can scroll up to near the top
    let height = row_count as f32 * line_h + (rect.height - 3.0 * line_h).max(line_h);
    // lines folds start on and lines folded now, sorted, looked up per row
    let mut fold_lines: Vec<usize> = doc
        .folds
        .iter()
        .filter(|f| fold::hides(text, f).is_some())
        .filter_map(|f| text.line_of(f.start).ok())
        .collect();
    fold_lines.dedup();
    let mut closed_lines: Vec<usize> = view
        .folded
        .iter()
        .filter_map(|f| text.line_of(f.start).ok())
        .collect();
    closed_lines.sort_unstable();
    let reveal = std::mem::take(&mut view.reveal);
    if reveal {
        // a couple of lines kept in sight around the caret when there is room
        let margin = if rect.height > 8.0 * line_h {
            2.0 * line_h
        } else {
            0.0
        };
        let y = rows.row_of(text, view.sel.head).0 as f32 * line_h;
        if y - margin < view.scroll {
            view.scroll = (y - margin).max(0.0);
        } else if y + line_h + margin > view.scroll + rect.height {
            view.scroll = y + line_h + margin - rect.height;
        }
    }
    let mut sel = view.sel;
    let mut extras = view.extra.clone();
    // the snippet's stops not under a caret, outlined
    let stops: Vec<(Range<usize>, bool)> = view.snippet.as_ref().map_or_else(Vec::new, |s| {
        s.ranges().map(|(r, now)| (r.clone(), now)).collect()
    });
    let mut column = view.column;
    let mut folded = view.folded.clone();
    let mut preedit = view.preedit.clone();
    let mut hscroll = view.hscroll;
    let seen_line = view.caret_line;
    let mut stuck_jump: Option<usize> = None;
    if settings.overview {
        let track = Rect::new(rect.x + rect.width - BAR, rect.y, BAR, rect.height);
        overview(ui, track, doc, look.found, view.sel.head)?;
    }
    let (_, pull) = ScrollArea::new(&mut view.scroll, height).show(
        ui,
        Id::Editor(id),
        rect,
        |ui, content, seen| {
            let first_row = ((seen.y - content.y) / line_h).floor().max(0.0) as usize;
            let count = (seen.height / line_h).ceil() as usize + 1;
            // the rows in sight, top down: a line, or the piece of it a
            // wrapped row shows, with whether it is the first and last piece
            let mut visible: Vec<(usize, Range<usize>, bool, bool)> = Vec::with_capacity(count);
            let mut pieces_line = usize::MAX;
            let mut pieces: Vec<Range<usize>> = Vec::new();
            for r in first_row..first_row + count {
                let (line, k) = rows.line_of_row(r);
                if line >= lines {
                    break;
                }
                if line != pieces_line {
                    pieces = rows.pieces_of(text, line);
                    pieces_line = line;
                }
                let Some(piece) = pieces.get(k) else {
                    break;
                };
                visible.push((line, piece.clone(), k == 0, k + 1 == pieces.len()));
            }
            let row_y = |i: usize| content.y + (first_row + i) as f32 * line_h;
            let text_x = content.x + gutter;
            let area = Rect::new(
                text_x,
                seen.y,
                (seen.x + seen.width - BAR - text_x).max(0.0),
                seen.height,
            );
            // wrapped lines never need sideways scrolling
            if rows.wraps() {
                hscroll = 0.0;
            }
            let hshown = ui.animate(
                Id::Editor(id),
                PART_H,
                Channel::Custom(H_SCROLL),
                hscroll,
                ui.theme.motion.slide * 1.4,
            );
            let body_x = text_x + 4.0 - hshown;

            // pointer: press places the caret, drag stretches it
            let hit = Rect::new(seen.x, seen.y, (seen.width - BAR).max(0.0), seen.height);
            let input = ui.interact(Id::Editor(id), 1, hit, Sense::Click, true);
            shown.focused = input.focused;
            if input.hovered {
                ui.set_cursor(CursorIcon::Text);
            }
            // the line and the piece of it on the row at `y`
            let piece_at_y = |y: f32| {
                let row = ((y - content.y) / line_h)
                    .floor()
                    .clamp(0.0, (row_count - 1) as f32) as usize;
                let (line, k) = rows.line_of_row(row);
                let line = line.min(lines - 1);
                let all = rows.pieces_of(text, line);
                let piece = all.get(k).or(all.last()).cloned().unwrap_or(0..0);
                (line, piece)
            };
            let line_at_y = |y: f32| piece_at_y(y).0;
            let hit_test = |ui: &mut Ui<'_, Id>, at: Point| {
                let (line, piece) = piece_at_y(at.y);
                let hit = lay(text, piece.clone(), tab).offset_at(ui, &font, at.x - body_x);
                // past the end of a wrapped row that isn't the line's last:
                // its end is the next row's start, so stop one char before
                if hit >= piece.end && piece.end < cursor::line_end(text, line) {
                    return text
                        .prev_grapheme(piece.end)
                        .ok()
                        .flatten()
                        .filter(|&p| p >= piece.start)
                        .unwrap_or(piece.start);
                }
                hit
            };
            // over the text, not dragging: what is under the pointer
            if input.hovered
                && !input.held
                && let Some(p) = ui.pointer()
                && p.x >= text_x
            {
                shown.pointing = Some((hit_test(ui, p), p));
            }
            let mut pull = 0.0;
            // the fold arrow column: a press there folds or opens
            let arrows = Rect::new(content.x + gutter - 24.0, seen.y, 14.0, seen.height);
            let over_gutter = ui
                .pointer()
                .is_some_and(|p| p.x < text_x && seen.contains(p));
            let mut toggled = false;
            if let Some(press) = ui.press(Id::Editor(id), 1)
                && arrows.contains(press.at)
            {
                let line = line_at_y(press.at.y);
                if let Some(target) = fold::starting_on(text, &doc.folds, line) {
                    if let Some(at) = folded
                        .iter()
                        .position(|f| text.line_of(f.start).ok() == Some(line))
                    {
                        folded.remove(at);
                    } else {
                        folded.push(target);
                        // a caret in what just went out of sight comes up
                        if let Some(head) = fold::caret_out(text, &folded, sel.head) {
                            sel = Selection::caret(head);
                            extras.clear();
                        }
                    }
                    toggled = true;
                }
            }
            if !input.held {
                column = None;
            }
            shown.menu = ui.secondary_at(Id::Editor(id), 1);
            if let Some(press) = ui.press(Id::Editor(id), 1).filter(|_| !toggled) {
                let at = hit_test(ui, press.at);
                let alt = press.modifiers.alt;
                // ctrl, or command on mac, follows the name to its definition
                let follow = press.modifiers.command();
                if alt && press.shift && press.clicks == 1 {
                    // alt, shift and a drag: a box, one selection a line
                    column = Some((line_at_y(press.at.y), press.at.x - body_x));
                    extras.clear();
                    sel = Selection::caret(at);
                } else if follow && press.clicks == 1 && !press.shift {
                    extras.clear();
                    sel = Selection::caret(at);
                    shown.definition = true;
                } else if alt && press.clicks == 1 {
                    // alt and a click: one more caret
                    if extras.len() < multi::MAX_CARETS {
                        extras.push(sel);
                    }
                    sel = Selection::caret(at);
                } else {
                    extras.clear();
                    sel = match press.clicks {
                        2 => cursor::word_at(text, at),
                        n if n >= 3 => cursor::line_at(text, at),
                        _ if press.shift => Selection {
                            anchor: sel.anchor,
                            head: at,
                        },
                        _ => Selection::caret(at),
                    };
                }
                // a click ends composing and the typing run
                preedit = None;
                shown.pressed = true;
            } else if input.held
                && let Some(at) = ui.pointer()
            {
                if let Some((from_line, from_x)) = column {
                    // a box never spans more than this many lines
                    const MAX_BOX: usize = 2000;
                    let to_line = line_at_y(at.y)
                        .clamp(from_line.saturating_sub(MAX_BOX), from_line + MAX_BOX);
                    let to_x = at.x - body_x;
                    let mut boxed = Vec::new();
                    for line in from_line.min(to_line)..=from_line.max(to_line) {
                        let laid = lay(text, span(text, line), tab);
                        boxed.push(Selection {
                            anchor: laid.offset_at(ui, &font, from_x),
                            head: laid.offset_at(ui, &font, to_x),
                        });
                    }
                    // the main one is where the pointer is
                    let main = if to_line >= from_line {
                        boxed.len() - 1
                    } else {
                        0
                    };
                    sel = boxed.remove(main);
                    extras = boxed;
                } else {
                    sel.head = hit_test(ui, at);
                }
                // past an edge the view follows, faster the further out
                let above = seen.y - at.y;
                let below = at.y - (seen.y + seen.height);
                if above > 0.0 {
                    pull = -(above * 0.4).min(line_h * 3.0);
                } else if below > 0.0 {
                    pull = (below * 0.4).min(line_h * 3.0);
                }
                if at.x < area.x {
                    hscroll = (hscroll - (area.x - at.x) * 0.4).max(0.0);
                } else if at.x > area.x + area.width {
                    hscroll += (at.x - area.x - area.width) * 0.4;
                }
                if pull != 0.0 || at.x < area.x || at.x > area.x + area.width {
                    ui.keep_animating();
                }
            }
            // sideways wheel, or shift and the wheel, moves along long lines
            let (wx, _) = ui.wheel();
            if !rows.wraps() && wx != 0.0 && ui.pointer().is_some_and(|p| seen.contains(p)) {
                hscroll = (hscroll - wx * H_STEP).max(0.0);
                ui.consume_wheel((wx, 0.0));
            }

            let range = sel.range();
            let caret_line = text.line_of(sel.head).unwrap_or(0);
            let (caret_row, caret_piece) = rows.row_of(text, sel.head);

            // gutter: numbers, the caret's line brighter, a mark for problems
            let mut marks: Vec<Option<Severity>> = vec![None; visible.len()];
            let first_line = visible.first().map_or(0, |v| v.0);
            let last_line = visible.last().map_or(0, |v| v.0);
            for diag in &doc.diags {
                if let Ok(l) = text.line_of(diag.range.start)
                    && (first_line..=last_line).contains(&l)
                    && let Some(i) = visible.iter().position(|v| v.0 == l && v.2)
                {
                    let mark = &mut marks[i];
                    *mark = Some(mark.map_or(diag.severity, |m| m.min(diag.severity)));
                }
            }
            // lines the server is still checking: a bar at the gutter's edge
            for (busy, fatal) in &doc.busy {
                let (Ok(l0), Ok(l1)) = (text.line_of(busy.start), text.line_of(busy.end)) else {
                    continue;
                };
                if l1 < first_line || l0 > last_line {
                    continue;
                }
                let colour = if *fatal {
                    palette.danger
                } else {
                    palette.warning
                };
                for (i, v) in visible.iter().enumerate() {
                    if (l0..=l1).contains(&v.0) {
                        let bar = Rect::new(content.x + 1.0, row_y(i), 3.0, line_h);
                        ui.fill(bar, colour.fade(0.7))?;
                    }
                }
            }
            let small = ui.theme.font(11.0);
            // lines changed since HEAD: a bar by the text, a wedge where lines went
            // hunks are in line order: only the ones from the top line on are looked at
            let hunks = &doc.git.hunks;
            let from = hunks.partition_point(|h| h.lines.end < first_line);
            let x = content.x + gutter - 3.0;
            for (i, (line, _, first_piece, last_piece)) in visible.iter().enumerate() {
                let y = row_y(i);
                for hunk in hunks[from..]
                    .iter()
                    .take_while(|h| h.lines.start <= *line + 1)
                {
                    let colour = match hunk.kind {
                        crate::git::Kind::Added => palette.success,
                        crate::git::Kind::Modified => palette.accent,
                        crate::git::Kind::Deleted => palette.danger,
                    };
                    if hunk.lines.contains(line) {
                        ui.fill(Rect::new(x, y, 3.0, line_h), colour.fade(0.8))?;
                    } else if hunk.kind == crate::git::Kind::Deleted {
                        // lines went above this one, or past the last line
                        if hunk.lines.start == *line && *first_piece {
                            ui.fill(Rect::new(x - 1.0, y - 1.0, 5.0, 3.0), colour)?;
                        } else if hunk.lines.start == lines && *line + 1 == lines && *last_piece {
                            ui.fill(Rect::new(x - 1.0, y + line_h - 2.0, 5.0, 3.0), colour)?;
                        }
                    }
                }
            }
            for (i, (line, _, first_piece, _)) in visible.iter().enumerate() {
                let (line, first_piece) = (*line, *first_piece);
                // a wrapped line's number goes on its first row only
                if !first_piece {
                    continue;
                }
                let y = row_y(i);
                let number = Rect::new(content.x, y, gutter - 26.0, line_h);
                let color = if line == caret_line {
                    palette.dim
                } else {
                    palette.faint
                };
                let shown_number = match settings.line_numbers {
                    LineNumbers::On => Some(line + 1),
                    LineNumbers::Relative if line == caret_line => Some(line + 1),
                    // rows apart, so folded and wrapped lines count as drawn
                    LineNumbers::Relative => Some((first_row + i).abs_diff(caret_row)),
                    LineNumbers::Off => None,
                };
                if let Some(n) = shown_number {
                    ui.text(&n.to_string(), number, &font, color, Align::End)?;
                }
                // fold arrows show while the pointer is over the gutter, and
                // always on a folded line
                let closed = closed_lines.binary_search(&line).is_ok();
                if closed || (over_gutter && fold_lines.binary_search(&line).is_ok()) {
                    let at = Rect::new(arrows.x, y, arrows.width, line_h);
                    let glyph = if closed { "▸" } else { "▾" };
                    ui.text(glyph, at, &small, palette.dim, Align::Center)?;
                }
                if let Some(severity) = marks[i] {
                    let bar = Rect::new(content.x + gutter - 8.0, y + 3.0, 3.0, line_h - 6.0);
                    ui.round(bar, 1.5, severity_colour(&palette, severity))?;
                }
            }

            let focused = shown.focused;
            let lit = needle(text, sel).filter(|_| settings.word_highlight);
            let extra_ranges: Vec<Range<usize>> = extras.iter().map(|s| s.range()).collect();
            let brackets = (focused && sel.is_empty())
                .then(|| pairs::matching(text, sel.head))
                .flatten();
            // the caret's place in text space, lines from the top
            let caret_laid = lay(text, caret_piece.clone(), tab);
            let mut target_x = caret_laid.x_of(ui, &font, sel.head);
            let mut preedit_w = 0.0;
            if let Some((composing, at)) = &preedit {
                preedit_w = ui.text_width(composing, &font);
                let inside = at.map_or(composing.len(), |c| c.min(composing.len()));
                let inside = (0..=inside)
                    .rev()
                    .find(|i| composing.is_char_boundary(*i))
                    .unwrap_or(0);
                target_x += ui.text_width(&composing[..inside], &font);
            }
            if reveal {
                let room = area.width - 4.0;
                if target_x < hscroll {
                    hscroll = (target_x - room / 3.0).max(0.0);
                } else if target_x + char_w * 2.0 > hscroll + room {
                    hscroll = target_x + char_w * 2.0 - room + room / 3.0;
                }
            }
            let mut widest: f32 = 0.0;
            ui.clipped(area, |ui| {
                let mut carry = 0;
                if let Some(back) = visible.first().and_then(|v| v.0.checked_sub(1)) {
                    let s = span(text, back);
                    carry = depth(&text.slice(s).unwrap_or_default(), tab.len()).unwrap_or(0);
                }
                for (i, (line, piece, first_piece, last_piece)) in visible.iter().enumerate() {
                    let (line, first_piece, last_piece) = (*line, *first_piece, *last_piece);
                    let y = row_y(i);
                    let body =
                        Rect::new(body_x, y, (area.x + area.width - body_x).max(0.0), line_h);
                    let span = piece.clone();
                    let raw = text.slice(span.clone()).unwrap_or_default();
                    let laid = lay(text, span.clone(), tab);
                    if settings.current_line && focused && sel.is_empty() && line == caret_line {
                        let row = Rect::new(area.x, y, area.width, line_h);
                        ui.fill(row, palette.raised.fade(0.45))?;
                    }
                    // indent guides; a blank line keeps the ones above it
                    // guides follow a line's indent, on its first row only
                    let levels = if first_piece {
                        depth(&raw, tab.len()).unwrap_or(carry)
                    } else {
                        0
                    };
                    if first_piece {
                        carry = levels;
                    }
                    let guides = if settings.indent_guides { levels } else { 0 };
                    for level in 1..guides {
                        let x = (body_x + (level * tab.len()) as f32 * char_w).round() + 0.5;
                        ui.fill(Rect::new(x, y, 1.0, line_h), palette.border.fade(0.55))?;
                    }
                    if let Some((needle, whole)) = &lit {
                        for hit in hits(&raw, span.start, needle, *whole) {
                            if hit == range {
                                continue;
                            }
                            let x0 = laid.x_of(ui, &font, hit.start);
                            let x1 = laid.x_of(ui, &font, hit.end);
                            let mark = Rect::new(body.x + x0, y + 1.0, x1 - x0, line_h - 2.0);
                            ui.round(mark, 3.0, palette.selection.fade(0.4))?;
                        }
                    }
                    for (stop, now) in &stops {
                        if stop.end < span.start || stop.start > span.end || *now {
                            continue;
                        }
                        let x0 = laid.x_of(ui, &font, stop.start.max(span.start));
                        let x1 = laid.x_of(ui, &font, stop.end.min(span.end));
                        let mark = Rect::new(
                            body.x + x0 - 1.0,
                            y + 1.0,
                            (x1 - x0).max(2.0) + 2.0,
                            line_h - 2.0,
                        );
                        ui.border(mark, 3.0, 1.0, palette.accent.fade(0.6))?;
                    }
                    let from = look.found.partition_point(|h| h.end <= span.start);
                    for hit in look.found[from..].iter().take_while(|h| h.start < span.end) {
                        let x0 = laid.x_of(ui, &font, hit.start.max(span.start));
                        let x1 = laid.x_of(ui, &font, hit.end.min(span.end));
                        let mark =
                            Rect::new(body.x + x0, y + 1.0, (x1 - x0).max(2.0), line_h - 2.0);
                        if look.current.as_ref() == Some(hit) {
                            ui.round(mark, 3.0, palette.warning.fade(0.45))?;
                            ui.border(mark, 3.0, 1.0, palette.warning)?;
                        } else {
                            ui.round(mark, 3.0, palette.warning.fade(0.22))?;
                        }
                    }
                    // selection behind the text, one extra space when it runs past the line end
                    let picks =
                        std::iter::once(range.clone()).chain(extras.iter().map(|s| s.range()));
                    for range in picks {
                        if range.start > span.end || range.end < span.start {
                            continue;
                        }
                        let from = range.start.max(span.start);
                        let to = range.end.min(span.end);
                        if from < to || (range.start <= span.end && range.end > span.end) {
                            let x0 = laid.x_of(ui, &font, from);
                            let mut x1 = laid.x_of(ui, &font, to);
                            if last_piece && range.end > span.end {
                                x1 += char_w;
                            }
                            let picked = Rect::new(body.x + x0, y, x1 - x0, line_h);
                            ui.round(picked, 3.0, palette.selection)?;
                        }
                    }
                    if let Some((a, b)) = brackets {
                        for at in [a, b] {
                            if span.contains(&at) {
                                let x0 = laid.x_of(ui, &font, at);
                                let x1 = laid.x_of(ui, &font, at + 1);
                                let cell = Rect::new(body.x + x0, y + 1.0, x1 - x0, line_h - 2.0);
                                ui.fill(cell, palette.accent.fade(0.18))?;
                                ui.border(cell, 2.0, 1.0, palette.accent.fade(0.7))?;
                            }
                        }
                    }
                    paint_line(
                        ui,
                        &font,
                        &laid,
                        &doc.spans,
                        span.clone(),
                        body,
                        (palette.text, look.dark),
                    )?;
                    squiggles(ui, &font, &laid, &doc.diags, span.clone(), body, line_h)?;
                    let drawn_w = ui.text_width(&laid.drawn, &font);
                    widest = widest.max(drawn_w);
                    // spaces as dots and tabs as arrows, everywhere or in
                    // the selection only
                    if settings.whitespace != Whitespace::None {
                        let picked = |at: usize| {
                            std::iter::once(&range)
                                .chain(extra_ranges.iter())
                                .any(|r| r.start <= at && at < r.end)
                        };
                        for (i, c) in raw.char_indices() {
                            // past the drawn part there is nothing to mark
                            if i >= MAX_DRAWN {
                                break;
                            }
                            let at = span.start + i;
                            if !matches!(c, ' ' | '\t')
                                || (settings.whitespace == Whitespace::Selection && !picked(at))
                            {
                                continue;
                            }
                            let x0 = laid.x_of(ui, &font, at);
                            let mark = if c == ' ' { "·" } else { "→" };
                            let cell = Rect::new(body.x + x0, y, char_w, line_h);
                            ui.text(mark, cell, &font, palette.faint.fade(0.8), Align::Start)?;
                        }
                    }
                    // a folded line ends in a pill standing for what it hides
                    if last_piece && closed_lines.binary_search(&line).is_ok() {
                        let dots = Rect::new(body.x + drawn_w + 6.0, y + 3.0, 26.0, line_h - 6.0);
                        ui.round(dots, 4.0, palette.raised)?;
                        ui.text("⋯", dots, &font, palette.dim, Align::Center)?;
                    }
                }

                // the caret glides to its place; a long jump just lands
                let target_y = caret_row as f32 * line_h;
                let jump = preedit.is_some()
                    || !settings.smooth_caret
                    || seen_line.is_none_or(|l: usize| l.abs_diff(caret_line) > 6);
                let (cx, cy) = if jump {
                    ui.set_animated(
                        Id::Editor(id),
                        PART_CARET,
                        Channel::Custom(CARET_X),
                        target_x,
                    );
                    ui.set_animated(
                        Id::Editor(id),
                        PART_CARET,
                        Channel::Custom(CARET_Y),
                        target_y,
                    );
                    (target_x, target_y)
                } else {
                    let rate = CARET_RATE;
                    (
                        ui.animate(
                            Id::Editor(id),
                            PART_CARET,
                            Channel::Custom(CARET_X),
                            target_x,
                            rate,
                        ),
                        ui.animate(
                            Id::Editor(id),
                            PART_CARET,
                            Channel::Custom(CARET_Y),
                            target_y,
                            rate,
                        ),
                    )
                };
                let moving = (cx - target_x).abs() > 0.5 || (cy - target_y).abs() > 0.5;
                if !focused {
                    return Ok(());
                }
                let y = content.y + target_y;
                if let Some((composing, _)) = &preedit {
                    let x = body_x + caret_laid.x_of(ui, &font, sel.head);
                    let box_ = Rect::new(x, y, preedit_w, line_h);
                    ui.fill(box_, palette.raised)?;
                    ui.text(composing, box_, &font, palette.text, Align::Start)?;
                    ui.fill(Rect::new(x, y + line_h - 2.0, preedit_w, 1.0), palette.text)?;
                }
                let (on, next) = if settings.caret_blink {
                    blink(look.idle)
                } else {
                    (true, None)
                };
                shown.blink = next;
                let x = body_x + cx;
                let drawn_y = content.y + cy;
                if on || moving || preedit.is_some() {
                    let bar = match settings.caret {
                        CaretStyle::Bar => {
                            Rect::new(x.round() - 0.5, drawn_y + 2.0, 2.0, line_h - 4.0)
                        }
                        CaretStyle::Block => {
                            Rect::new(x.round(), drawn_y + 1.0, char_w, line_h - 2.0)
                        }
                        CaretStyle::Underline => {
                            Rect::new(x.round(), drawn_y + line_h - 3.0, char_w, 2.0)
                        }
                    };
                    let colour = if settings.caret == CaretStyle::Block {
                        palette.accent.fade(0.55)
                    } else {
                        palette.accent
                    };
                    ui.fill(bar, colour)?;
                }
                // the other carets blink with the main one, no glide
                if on || moving {
                    for extra in &extras {
                        let (row, piece) = rows.row_of(text, extra.head);
                        if row < first_row || row >= first_row + visible.len() {
                            continue;
                        }
                        let ex = body_x + lay(text, piece, tab).x_of(ui, &font, extra.head);
                        let ey = content.y + row as f32 * line_h;
                        let bar = Rect::new(ex.round() - 0.5, ey + 2.0, 2.0, line_h - 4.0);
                        ui.fill(bar, palette.accent.fade(0.85))?;
                    }
                }
                shown.caret = Some(Rect::new(body_x + target_x, y, 2.0, line_h));
                Ok(())
            })?;
            // the lines that open the scopes the top line sits in, pinned over
            // it, the innermost last. a click goes to that line
            if settings.sticky_scroll && first_row > 0 {
                let top = visible.first().map_or(0, |v| v.0);
                let mut heads: Vec<usize> = doc
                    .folds
                    .iter()
                    .filter_map(|f| fold::hides(text, f))
                    .map(|(first, last)| (first - 1, last))
                    .filter(|&(head, last)| head < top && top <= last)
                    .map(|(head, _)| head)
                    .collect();
                heads.sort_unstable();
                heads.dedup();
                let keep = heads.len().saturating_sub(MAX_STICKY);
                let heads = &heads[keep..];
                // the lines under them must not show through
                let width = (seen.width - BAR).max(0.0);
                let band = Rect::new(seen.x, seen.y, width, heads.len() as f32 * line_h);
                if !heads.is_empty() {
                    ui.fill(band, palette.surface)?;
                    let edge = Rect::new(seen.x, band.y + band.height, width, 1.0);
                    ui.fill(edge, palette.border)?;
                }
                for (i, &line) in heads.iter().enumerate() {
                    let y = seen.y + i as f32 * line_h;
                    let row = Rect::new(seen.x, y, width, line_h);
                    let input = ui.interact(
                        Id::Editor(id),
                        PART_STICKY + i as u16,
                        row,
                        Sense::Click,
                        true,
                    );
                    if input.hovered {
                        ui.fill(row, palette.hover)?;
                        ui.set_cursor(CursorIcon::Pointer);
                    }
                    if input.clicked {
                        stuck_jump = Some(line);
                    }
                    if settings.line_numbers != LineNumbers::Off {
                        let number = Rect::new(content.x, y, gutter - 26.0, line_h);
                        ui.text(
                            &(line + 1).to_string(),
                            number,
                            &font,
                            palette.faint,
                            Align::End,
                        )?;
                    }
                    let range = span(text, line);
                    let laid = lay(text, range.clone(), tab);
                    let body =
                        Rect::new(body_x, y, (area.x + area.width - body_x).max(0.0), line_h);
                    ui.clipped(Rect::new(area.x, y, area.width, line_h), |ui| {
                        paint_line(
                            ui,
                            &font,
                            &laid,
                            &doc.spans,
                            range,
                            body,
                            (palette.text, look.dark),
                        )
                    })?;
                }
            }
            // no scrolling far past the longest line in sight
            hscroll = hscroll.min((widest - area.width * 0.5).max(0.0));
            if let Some((text, p)) = look.peek {
                // under the pointer's line, so the word stays in sight
                let line = Rect::new(p.x, p.y - line_h / 2.0, 1.0, line_h);
                let alpha = ui.animate(Id::Editor(id), PART_PEEK, Channel::Custom(0), 1.0, 14.0);
                hover::show(ui, text, line, seen, alpha, (look.dark, look.size))?;
            } else {
                ui.set_animated(Id::Editor(id), PART_PEEK, Channel::Custom(0), 0.0);
            }
            if let (Some(text), Some(caret)) = (look.hover, shown.caret) {
                let alpha = ui.animate(Id::Editor(id), PART_HOVER, Channel::Custom(0), 1.0, 14.0);
                hover::show(ui, text, caret, seen, alpha, (look.dark, look.size))?;
            } else {
                ui.set_animated(Id::Editor(id), PART_HOVER, Channel::Custom(0), 0.0);
            }
            Ok(pull)
        },
    )?;
    view.scroll = (view.scroll + pull).max(0.0);
    view.hscroll = hscroll;
    view.caret_line = Some(text.line_of(sel.head).unwrap_or(0));
    view.sel = sel;
    view.extra = extras;
    // a sticky line was clicked: to its start, in sight next frame
    if let Some(line) = stuck_jump {
        view.sel = Selection::caret(span(text, line).start);
        view.extra.clear();
        view.reveal = true;
        ui.mark_changed();
    }
    view.column = column;
    view.folded = folded;
    drop(rows);
    view.wrap = cache;
    multi::merge(view);
    view.preedit = preedit;
    Ok(shown)
}

fn severity_colour(palette: &Palette, severity: Severity) -> Color {
    match severity {
        Severity::Error => palette.danger,
        Severity::Warning => palette.warning,
        Severity::Info => palette.accent,
        Severity::Hint => palette.dim,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::cursor::Motion;
    use std::path::Path;

    #[test]
    fn abbreviations_turn_over_as_you_type() {
        let (mut docs, v) = open("");
        let mut opts = EditOpts::default();
        opts.abbrevs.insert(
            "rust",
            std::sync::Arc::new(abbrev::Abbrevs::new(
                '\\',
                [
                    ("to".to_owned(), "→".to_owned()),
                    ("a".to_owned(), "α".to_owned()),
                ],
            )),
        );
        for c in ["\\", "t", "o", " ", "\\", "a"] {
            act_with(&mut docs, v, Action::Insert(c.into()), &opts);
        }
        assert_eq!(text(&docs, v), "→ \\a");
        act_with(&mut docs, v, Action::Indent, &opts);
        assert_eq!(text(&docs, v), "→ α", "tab finishes it, no tab added");
        assert!(docs.undo(v));
        assert_eq!(text(&docs, v), "", "the typing run is one step");
        act(&mut docs, v, Action::Insert("\\to x".into()));
        assert_eq!(text(&docs, v), "\\to x", "a paste is left alone");
    }

    fn open(text: &str) -> (Documents, ViewId) {
        let mut docs = Documents::default();
        let (doc, _) = docs.open(Path::new("t.rs"));
        let view = docs.add_view(doc);
        docs.loaded(doc, Ok(text.to_owned()));
        (docs, view)
    }

    fn carets(docs: &mut Documents, view: ViewId, at: &[usize]) {
        let v = docs.view_mut(view).unwrap();
        v.sel = Selection::caret(at[0]);
        v.extra = at[1..].iter().map(|&a| Selection::caret(a)).collect();
    }

    fn heads(docs: &Documents, view: ViewId) -> Vec<usize> {
        let (sels, _) = multi::all(docs.view(view).unwrap());
        sels.iter().map(|s| s.head).collect()
    }

    #[test]
    fn typing_and_backspace_at_every_caret() {
        let (mut docs, v) = open("ab\nab\nab");
        carets(&mut docs, v, &[2, 5, 8]);
        act(&mut docs, v, Action::Insert("가".into()));
        assert_eq!(text(&docs, v), "ab가\nab가\nab가");
        assert_eq!(heads(&docs, v), vec![5, 11, 17]);
        act(&mut docs, v, Action::Insert("(".into()));
        assert_eq!(text(&docs, v), "ab가()\nab가()\nab가()");
        act(&mut docs, v, Action::Backspace);
        act(&mut docs, v, Action::Backspace);
        assert_eq!(text(&docs, v), "ab\nab\nab");
        assert_eq!(heads(&docs, v), vec![2, 5, 8]);
        assert!(docs.undo(v));
        assert_eq!(text(&docs, v), "ab\nab\nab", "the typing run is one step");
    }

    #[test]
    fn paste_spreads_lines_over_carets() {
        let (mut docs, v) = open("a\nb");
        carets(&mut docs, v, &[1, 3]);
        paste(&mut docs, v, "1\n2");
        assert_eq!(text(&docs, v), "a1\nb2");
        paste(&mut docs, v, "x");
        assert_eq!(text(&docs, v), "a1x\nb2x");
    }

    #[test]
    fn carets_whose_edits_clash_leave_one_out() {
        let (mut docs, v) = open("    x");
        carets(&mut docs, v, &[2, 3]);
        assert!(act(&mut docs, v, Action::Backspace));
        assert_eq!(text(&docs, v), "  x");
        let (mut docs, v) = open("  a");
        carets(&mut docs, v, &[1, 3]);
        assert!(act(&mut docs, v, Action::Outdent));
        assert_eq!(text(&docs, v), "a");
        let (mut docs, v) = open("()");
        carets(&mut docs, v, &[1, 2]);
        assert!(act(&mut docs, v, Action::Backspace));
        assert_eq!(text(&docs, v), "");
    }

    #[test]
    fn carets_meeting_become_one() {
        let (mut docs, v) = open("abc");
        carets(&mut docs, v, &[1, 2]);
        act(&mut docs, v, Action::Move(Motion::DocStart, false));
        assert!(docs.view(v).unwrap().extra.is_empty());
        act(&mut docs, v, Action::Backspace);
        assert_eq!(text(&docs, v), "abc", "nothing to take at the start");
    }

    #[test]
    fn down_steps_over_a_fold_and_fold_commands() {
        let (mut docs, v) = open("fn a() {\n    x;\n    y;\n}\nz");
        let doc = docs.view(v).unwrap().doc;
        // what the parser would give for the body
        docs.doc_mut_for_test(doc).folds = std::iter::once(7..24).collect();
        caret(&mut docs, v, 2);
        assert!(fold_op(&mut docs, v, FoldOp::Fold));
        assert_eq!(docs.view(v).unwrap().folded.first(), Some(&(7..24)));
        act(&mut docs, v, Action::Move(Motion::Down, false));
        let text = docs.doc_of(v).unwrap().buffer.text().clone();
        assert_eq!(
            text.line_of(docs.view(v).unwrap().sel.head).unwrap(),
            3,
            "past the hidden body"
        );
        act(&mut docs, v, Action::Move(Motion::Up, false));
        assert_eq!(text.line_of(docs.view(v).unwrap().sel.head).unwrap(), 0);
        assert!(fold_op(&mut docs, v, FoldOp::Unfold));
        assert!(docs.view(v).unwrap().folded.is_empty());
    }

    #[test]
    fn moves_into_a_fold_step_out_the_way_they_went() {
        // the fold hides through the end: nothing comes after it
        let (mut docs, v) = open("a {\n  b\n  c");
        let doc = docs.view(v).unwrap().doc;
        let fold = 2..docs.doc_of(v).unwrap().buffer.len();
        docs.doc_mut_for_test(doc).folds = std::iter::once(fold).collect();
        caret(&mut docs, v, 7);
        assert!(fold_op(&mut docs, v, FoldOp::FoldAll));
        assert_eq!(
            docs.view(v).unwrap().sel,
            Selection::caret(3),
            "caret came up"
        );
        act(&mut docs, v, Action::Move(Motion::Down, false));
        assert_eq!(
            docs.view(v).unwrap().sel,
            Selection::caret(3),
            "nowhere to go"
        );
        act(&mut docs, v, Action::Move(Motion::Right, false));
        assert_eq!(docs.view(v).unwrap().sel, Selection::caret(3));
        caret(&mut docs, v, 0);
        act(&mut docs, v, Action::Move(Motion::DocEnd, false));
        assert_eq!(
            docs.view(v).unwrap().sel,
            Selection::caret(0),
            "end is inside"
        );
    }

    #[test]
    fn tidy_trims_and_ends_the_file() {
        let (mut docs, v) = open("a  \nb\t\nc");
        assert!(tidy_for_save(&mut docs, v, true, true));
        assert_eq!(text(&docs, v), "a\nb\nc\n");
        assert!(!tidy_for_save(&mut docs, v, true, true), "already tidy");
        assert!(docs.undo(v));
        assert_eq!(text(&docs, v), "a  \nb\t\nc", "one undo step");
    }

    #[test]
    fn down_walks_wrapped_rows_of_one_line() {
        let (mut docs, v) = open("aaaa bbbb cccc\nxy");
        docs.view_mut(v).unwrap().wrap_cols = Some(10);
        caret(&mut docs, v, 2);
        act(&mut docs, v, Action::Move(Motion::Down, false));
        assert_eq!(
            docs.view(v).unwrap().sel,
            Selection::caret(12),
            "next row, same line"
        );
        act(&mut docs, v, Action::Move(Motion::Down, false));
        assert_eq!(docs.view(v).unwrap().sel, Selection::caret(17));
        act(&mut docs, v, Action::Move(Motion::Up, true));
        assert_eq!(
            docs.view(v).unwrap().sel.anchor,
            17,
            "shift keeps the anchor"
        );
    }

    #[test]
    fn blink_flips_then_rests() {
        assert_eq!(blink(Duration::ZERO), (true, Some(BLINK_HALF)));
        let (on, next) = blink(BLINK_HALF + Duration::from_millis(30));
        assert!(!on);
        assert_eq!(next, Some(BLINK_HALF - Duration::from_millis(30)));
        assert_eq!(blink(BLINK_FOR), (true, None));
    }

    #[test]
    fn word_under_caret_lights_whole_words() {
        let t = resolvase::TextBuffer::from_text("foo foobar foo").snapshot();
        assert_eq!(needle(&t, Selection::caret(1)), Some(("foo".into(), true)));
        assert_eq!(hits("foo foobar foo", 0, "foo", true), vec![0..3, 11..14]);
        assert_eq!(hits("foo foobar", 5, "oo", false), vec![6..8, 10..12]);
        assert_eq!(needle(&t, Selection::caret(3)).map(|n| n.1), Some(true));
        let t = resolvase::TextBuffer::from_text("a + b").snapshot();
        assert_eq!(needle(&t, Selection::caret(2)), None, "marks are not words");
    }

    #[test]
    fn indent_depth_counts_tabs_and_blank_lines() {
        assert_eq!(depth("        x", 4), Some(2));
        assert_eq!(depth("\t  x", 4), Some(1));
        assert_eq!(depth("   ", 4), None);
    }

    fn text(docs: &Documents, view: ViewId) -> String {
        docs.doc_of(view).unwrap().buffer.text().to_string()
    }

    fn caret(docs: &mut Documents, view: ViewId, at: usize) {
        docs.view_mut(view).unwrap().sel = Selection::caret(at);
    }

    #[test]
    fn typing_backspace_and_undo() {
        let (mut docs, v) = open("");
        for c in ["가", "e\u{301}", "!"] {
            act(&mut docs, v, Action::Insert(c.into()));
        }
        assert_eq!(text(&docs, v), "가e\u{301}!");
        act(&mut docs, v, Action::Backspace);
        act(&mut docs, v, Action::Backspace);
        assert_eq!(text(&docs, v), "가", "accent goes with its letter");
        // the whole typing run is one step
        assert!(docs.undo(v));
        assert_eq!(text(&docs, v), "");
    }

    #[test]
    fn crlf_lines_lay_out_without_cr() {
        let (docs, v) = open("ab\r\ncd");
        let text = docs.doc_of(v).unwrap().buffer.text();
        assert_eq!(span(text, 0), 0..2);
        let laid = lay(text, span(text, 0), TAB);
        assert_eq!(laid.drawn, "ab");
        assert_eq!(laid.marks.last(), Some(&(2, 2)));
    }

    #[test]
    fn newline_keeps_indent_and_crlf() {
        let (mut docs, v) = open("    let x;\r\n");
        caret(&mut docs, v, 10);
        act(&mut docs, v, Action::Newline);
        assert_eq!(text(&docs, v), "    let x;\r\n    \r\n");
        assert_eq!(docs.view(v).unwrap().sel, Selection::caret(16));
    }

    #[test]
    fn paste_fits_line_breaks() {
        let (mut docs, v) = open("a\r\n");
        caret(&mut docs, v, 1);
        paste(&mut docs, v, "x\ny\r\nz");
        assert_eq!(text(&docs, v), "ax\r\ny\r\nz\r\n");
    }

    #[test]
    fn selection_replaced_and_cut() {
        let (mut docs, v) = open("hello world");
        docs.view_mut(v).unwrap().sel = Selection {
            anchor: 6,
            head: 11,
        };
        assert_eq!(selected(&docs, v).as_deref(), Some("world"));
        act(&mut docs, v, Action::Insert("there".into()));
        assert_eq!(text(&docs, v), "hello there");
        select_all(&mut docs, v);
        act(&mut docs, v, Action::Delete);
        assert_eq!(text(&docs, v), "");
    }

    #[test]
    fn enter_between_braces_opens_a_line() {
        let (mut docs, v) = open("  f {}");
        caret(&mut docs, v, 5);
        act(&mut docs, v, Action::Newline);
        assert_eq!(text(&docs, v), "  f {\n      \n  }");
        assert_eq!(docs.view(v).unwrap().sel, Selection::caret(12));
    }

    #[test]
    fn enter_after_python_colon_at_the_end() {
        let mut docs = Documents::default();
        let (doc, _) = docs.open(Path::new("t.py"));
        let v = docs.add_view(doc);
        docs.loaded(doc, Ok("def f():".to_owned()));
        caret(&mut docs, v, 8);
        act(&mut docs, v, Action::Newline);
        assert_eq!(text(&docs, v), "def f():\n    ");
    }

    #[test]
    fn backspace_in_indent_goes_to_tab_stop() {
        let (mut docs, v) = open("      x");
        caret(&mut docs, v, 6);
        act(&mut docs, v, Action::Backspace);
        assert_eq!(text(&docs, v), "    x");
        act(&mut docs, v, Action::Backspace);
        assert_eq!(text(&docs, v), "x");
    }

    #[test]
    fn typing_pairs_and_word_delete() {
        let (mut docs, v) = open("");
        act(&mut docs, v, Action::Insert("(".into()));
        act(&mut docs, v, Action::Insert("a".into()));
        act(&mut docs, v, Action::Insert(")".into()));
        assert_eq!(text(&docs, v), "(a)");
        assert_eq!(docs.view(v).unwrap().sel, Selection::caret(3));
        act(&mut docs, v, Action::Insert(" word".into()));
        act(&mut docs, v, Action::DeleteWord(false));
        assert_eq!(text(&docs, v), "(a) ");
    }

    #[test]
    fn line_copy_pastes_above() {
        let (mut docs, v) = open("a\nb");
        caret(&mut docs, v, 3);
        let (_, line) = caret_line(&docs, v).unwrap();
        assert_eq!(line, "b\n");
        paste_line(&mut docs, v, &line);
        assert_eq!(text(&docs, v), "a\nb\nb");
        assert_eq!(docs.view(v).unwrap().sel, Selection::caret(5));
    }

    #[test]
    fn move_breaks_typing_run() {
        let (mut docs, v) = open("ab");
        caret(&mut docs, v, 2);
        act(&mut docs, v, Action::Insert("c".into()));
        act(&mut docs, v, Action::Move(Motion::DocStart, false));
        act(&mut docs, v, Action::Insert("z".into()));
        docs.undo(v);
        assert_eq!(text(&docs, v), "abc");
    }
}
