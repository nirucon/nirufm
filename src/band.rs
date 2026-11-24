//! Selecting with the mouse: a rubber band dragged over the files, and the
//! scrolling that follows the pointer to the edge while dragging.
//!
//! The geometry is kept here, apart from the window, so it can be tested.
use crate::canvas::Rect;

/// How the band combines with what was selected before it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Mode {
    /// No key held: the band is the selection.
    Replace,
    /// Ctrl: the band adds to it.
    Add,
    /// Shift: the band flips what it covers.
    Toggle,
}

pub struct Band {
    /// The tab it belongs to.
    pub tab: usize,
    /// Where the press was, in folder coordinates (the scroll added).
    pub from: (i32, i32),
    /// The pointer now, in window coordinates.
    pub to: (i32, i32),
    pub before: Vec<bool>,
    pub mode: Mode,
    /// Past the few pixels that separate a click from a drag.
    pub live: bool,
}

impl Band {
    pub fn new(tab: usize, from: (i32, i32), before: Vec<bool>, mode: Mode) -> Self {
        Self {
            tab,
            from,
            to: from,
            before,
            mode,
            live: false,
        }
    }

    /// The band in folder coordinates, given the scroll.
    pub fn rect(&self, scroll: i32) -> Rect {
        span(self.from, (self.to.0, self.to.1 + scroll))
    }
}

/// The rectangle between two corners, whichever way it was dragged.
pub fn span(a: (i32, i32), b: (i32, i32)) -> Rect {
    let (x0, x1) = (a.0.min(b.0), a.0.max(b.0));
    let (y0, y1) = (a.1.min(b.1), a.1.max(b.1));
    Rect::new(x0, y0, (x1 - x0).max(1), (y1 - y0).max(1))
}

pub fn overlaps(a: &Rect, b: &Rect) -> bool {
    a.x < b.x + b.w && b.x < a.x + a.w && a.y < b.y + b.h && b.y < a.y + a.h
}

/// The selection with the band: `hit` holds what the band covers.
pub fn combine(before: &[bool], hit: &[bool], mode: Mode) -> Vec<bool> {
    before
        .iter()
        .zip(hit)
        .map(|(&b, &h)| match mode {
            Mode::Replace => h,
            Mode::Add => b || h,
            Mode::Toggle => b != h,
        })
        .collect()
}

/// The first and last row a span of `y0..y1` touches, for rows of height
/// `h` starting at `top` (both clamped to `0..rows`). None when it misses.
pub fn rows(y0: i32, y1: i32, top: i32, h: i32, rows: usize) -> Option<(usize, usize)> {
    if rows == 0 || h <= 0 || y1 < top {
        return None;
    }
    let a = ((y0 - top).max(0) / h) as usize;
    let b = ((y1 - top) / h) as usize;
    if a >= rows {
        return None;
    }
    Some((a, b.min(rows - 1)))
}

/// Pixels to scroll per step when the pointer is at `y` near the edges of
/// `top..bottom`: negative up, positive down, 0 away from them. The speed
/// grows with how far into the edge zone (or past it) the pointer is.
pub fn edge_speed(y: i32, top: i32, bottom: i32, zone: i32) -> i32 {
    let zone = zone.max(1);
    let f = |d: i32| (d * 24 / zone).clamp(2, 60);
    if y < top + zone {
        -f(top + zone - y)
    } else if y > bottom - zone {
        f(y - (bottom - zone))
    } else {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_band_is_the_same_whichever_way_it_is_drawn() {
        assert_eq!(span((10, 40), (30, 20)), Rect::new(10, 20, 20, 20));
        assert_eq!(span((30, 20), (10, 40)), Rect::new(10, 20, 20, 20));
        let mut b = Band::new(0, (100, 500), vec![], Mode::Replace);
        b.to = (120, 50);
        // The folder scrolled 300 down since the press at 500.
        assert_eq!(b.rect(300), Rect::new(100, 350, 20, 150));
    }

    #[test]
    fn the_band_replaces_adds_or_flips() {
        let before = [true, true, false, false];
        let hit = [false, true, true, false];
        assert_eq!(
            combine(&before, &hit, Mode::Replace),
            [false, true, true, false]
        );
        assert_eq!(combine(&before, &hit, Mode::Add), [true, true, true, false]);
        assert_eq!(
            combine(&before, &hit, Mode::Toggle),
            [true, false, true, false]
        );
    }

    #[test]
    fn rows_under_a_span() {
        // Rows of 20 from y 100: the span 115..161 touches rows 0, 1 and 3.
        assert_eq!(rows(115, 161, 100, 20, 10), Some((0, 3)));
        assert_eq!(rows(50, 90, 100, 20, 10), None);
        assert_eq!(rows(50, 105, 100, 20, 10), Some((0, 0)));
        assert_eq!(rows(500, 900, 100, 20, 10), None);
        assert_eq!(rows(250, 900, 100, 20, 10), Some((7, 9)));
        assert_eq!(rows(0, 10, 0, 20, 0), None);
    }

    #[test]
    fn scrolling_starts_at_the_edges_and_speeds_up() {
        assert_eq!(edge_speed(300, 100, 500, 30), 0);
        assert!(edge_speed(125, 100, 500, 30) < 0);
        assert!(edge_speed(480, 100, 500, 30) > 0);
        // Further past the edge is faster, up to a limit.
        assert!(edge_speed(560, 100, 500, 30) > edge_speed(480, 100, 500, 30));
        assert_eq!(edge_speed(5000, 100, 500, 30), 60);
        assert_eq!(edge_speed(-5000, 100, 500, 30), -60);
    }
}
