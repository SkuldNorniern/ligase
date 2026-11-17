//! places the caret jumped from, to go back to and forward again.

use std::path::PathBuf;

use resolvase::Point;

/// most places kept back.
const MAX: usize = 50;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Place {
    pub path: PathBuf,
    /// line and char column; a place past the end lands at the end.
    pub at: Point,
}

#[derive(Default)]
pub struct Trail {
    back: Vec<Place>,
    ahead: Vec<Place>,
}

impl Trail {
    /// a jump is leaving `from`. a new jump drops the way forward.
    pub fn leave(&mut self, from: Place) {
        self.ahead.clear();
        if self.back.last().is_some_and(|p| near(p, &from)) {
            return;
        }
        self.back.push(from);
        if self.back.len() > MAX {
            self.back.remove(0);
        }
    }

    /// the place before `now`, which is kept to come forward to.
    pub fn back(&mut self, now: Place) -> Option<Place> {
        let to = self.back.pop()?;
        self.ahead.push(now);
        Some(to)
    }

    /// the place gone back from, `now` kept to go back to.
    pub fn forward(&mut self, now: Place) -> Option<Place> {
        let to = self.ahead.pop()?;
        self.back.push(now);
        Some(to)
    }
}

/// a few lines apart in the same file count as one place.
fn near(a: &Place, b: &Place) -> bool {
    a.path == b.path && a.at.line.abs_diff(b.at.line) <= 3
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(file: &str, line: usize) -> Place {
        Place {
            path: PathBuf::from(file),
            at: Point::new(line, 0),
        }
    }

    #[test]
    fn back_and_forward_walk_the_jumps() {
        let mut t = Trail::default();
        t.leave(at("a", 1));
        t.leave(at("b", 40));
        assert_eq!(t.back(at("c", 7)), Some(at("b", 40)));
        assert_eq!(t.back(at("b", 40)), Some(at("a", 1)));
        assert_eq!(t.back(at("a", 1)), None);
        assert_eq!(t.forward(at("a", 1)), Some(at("b", 40)));
        assert_eq!(t.forward(at("b", 40)), Some(at("c", 7)));
        assert_eq!(t.forward(at("c", 7)), None);
    }

    #[test]
    fn a_new_jump_drops_forward_and_near_places_join() {
        let mut t = Trail::default();
        t.leave(at("a", 1));
        t.leave(at("a", 2));
        assert_eq!(t.back(at("b", 0)), Some(at("a", 1)));
        t.leave(at("a", 1));
        assert_eq!(t.forward(at("x", 0)), None, "forward went");
        for i in 0..200 {
            t.leave(at("f", i * 10));
        }
        assert_eq!(t.back.len(), MAX);
    }
}
