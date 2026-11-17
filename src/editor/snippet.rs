//! the tab stops a snippet leaves. tab walks them in order, shift+tab goes
//! back, and a stop shown in several places gets a caret in each.

use std::ops::Range;

use resolvase::{Bias, ChangeSet};

use super::cursor::Selection;
use crate::document::View;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Session {
    /// stops in visiting order, each with every place it shows, in text order.
    stops: Vec<Vec<Range<usize>>>,
    /// the stop the carets are on.
    at: usize,
}

impl Session {
    /// moves the stops over an edit. text typed at a stop's end grows it.
    pub fn follow(&mut self, set: &ChangeSet) {
        for stop in &mut self.stops {
            for r in stop.iter_mut() {
                let start = set.map(r.start, Bias::Left);
                let end = set.map(r.end, Bias::Right).max(start);
                *r = start..end;
            }
        }
    }

    /// every stop range, and whether it is the one the carets are on.
    pub fn ranges(&self) -> impl Iterator<Item = (&Range<usize>, bool)> {
        self.stops
            .iter()
            .enumerate()
            .flat_map(move |(i, s)| s.iter().map(move |r| (r, i == self.at)))
    }

    /// the caret still sits in the stop it was sent to.
    fn holds(&self, head: usize) -> bool {
        self.stops
            .get(self.at)
            .is_some_and(|s| s.iter().any(|r| r.start <= head && head <= r.end))
    }
}

/// puts `stops`, made for text that went in at `base`, on the view and
/// selects the first. a lone stop needs no session: the carets are set
/// and nothing stays. no stops (a plain completion typed into a stop)
/// leaves the session there is.
pub fn start(view: &mut View, base: usize, stops: &[Vec<Range<usize>>]) {
    if stops.iter().all(Vec::is_empty) {
        return;
    }
    view.snippet = None;
    let stops: Vec<Vec<Range<usize>>> = stops
        .iter()
        .filter(|s| !s.is_empty())
        .map(|s| s.iter().map(|r| r.start + base..r.end + base).collect())
        .collect();
    if stops.is_empty() {
        return;
    }
    let session = Session { stops, at: 0 };
    select(view, &session);
    if session.stops.len() > 1 {
        view.snippet = Some(session);
    }
}

/// tab or shift+tab inside a snippet. false when there is none, or the
/// caret has left it: the key then does what it does anywhere.
pub fn step(view: &mut View, back: bool) -> bool {
    let Some(mut session) = view.snippet.take() else {
        return false;
    };
    if !session.holds(view.sel.head) {
        return false;
    }
    if back {
        session.at = session.at.saturating_sub(1);
    } else {
        session.at += 1;
    }
    select(view, &session);
    // the last stop ends it, the way `$0` does
    if session.at + 1 < session.stops.len() {
        view.snippet = Some(session);
    }
    true
}

fn select(view: &mut View, session: &Session) {
    let Some(stop) = session.stops.get(session.at) else {
        return;
    };
    let mut sels = stop.iter().map(|r| Selection {
        anchor: r.start,
        head: r.end,
    });
    if let Some(first) = sels.next() {
        view.sel = first;
        view.extra = sels.collect();
        view.goal = None;
        view.reveal = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::Documents;

    fn view() -> (Documents, crate::document::ViewId) {
        let mut docs = Documents::default();
        let (doc, _) = docs.open(std::path::Path::new("snippet-test.txt"));
        let v = docs.add_view(doc);
        docs.loaded(doc, Ok(String::new()));
        (docs, v)
    }

    #[test]
    fn tab_walks_the_stops_and_ends_on_the_last() {
        let (mut docs, v) = view();
        // "f(a, b)" at 10: $1 = a, $2 = b, $0 after
        let stops = vec![vec![2..3], vec![5..6], vec![7..7]];
        let view = docs.view_mut(v).unwrap();
        start(view, 10, &stops);
        assert_eq!((view.sel.anchor, view.sel.head), (12, 13));
        assert!(step(view, false));
        assert_eq!((view.sel.anchor, view.sel.head), (15, 16));
        assert!(step(view, true));
        assert_eq!(view.sel.head, 13, "shift+tab goes back");
        assert!(step(view, false));
        assert!(step(view, false));
        assert_eq!(view.sel.head, 17);
        assert!(view.snippet.is_none(), "the last stop ends it");
        assert!(!step(view, false), "tab is tab again");
    }

    #[test]
    fn mirrors_get_carets_and_a_caret_elsewhere_ends_it() {
        let (mut docs, v) = view();
        let stops = vec![vec![0..1, 4..5], vec![6..6]];
        let view = docs.view_mut(v).unwrap();
        start(view, 0, &stops);
        assert_eq!(view.extra.len(), 1);
        assert_eq!((view.extra[0].anchor, view.extra[0].head), (4, 5));
        view.sel = Selection::caret(40);
        assert!(!step(view, false));
        assert!(view.snippet.is_none());
    }
}
