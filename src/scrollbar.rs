//! A scroll bar the desktop draws over any app's scrolling list, and lets you drag (laptop
//! touchpads often send no scroll wheel). Apps only say where their list is, how far it
//! scrolls and where it is now; see `Desktop::scroll_info`.

use crate::gfx::{Canvas, Rect};

/// Width of the bar.
const W: i32 = 12;

/// The groove the bar runs in, along the right edge of `area`.
pub fn track(area: Rect) -> Rect {
    Rect::new(area.x + area.w - W - 4, area.y + 6, W, (area.h - 12).max(1))
}

/// Where a press grabs the bar (a little wider than it looks, so it's easy to hit).
pub fn hit(area: Rect) -> Rect {
    let t = track(area);
    Rect::new(t.x - 6, area.y, W + 10, area.h)
}

/// The part you drag: its size shows how much of the list is on screen.
fn thumb(area: Rect, max: i32, scroll: i32) -> Rect {
    let t = track(area);
    let view = area.h.max(1) as i64;
    let th = ((t.h as i64 * view / (view + max.max(0) as i64)) as i32).clamp(36.min(t.h), t.h);
    let y = t.y + ((t.h - th) as i64 * scroll.clamp(0, max.max(1)) as i64 / max.max(1) as i64) as i32;
    Rect::new(t.x, y, W, th)
}

pub fn draw(c: &mut Canvas, area: Rect, max: i32, scroll: i32) {
    if max <= 0 {
        return;
    }
    let t = track(area);
    c.round_rect_alpha(t.x, t.y, t.w, t.h, W / 2, 0, 24);
    let th = thumb(area, max, scroll);
    c.round_rect_alpha(th.x, th.y, th.w, th.h, W / 2, 0, 120);
}

/// A press at height `y` on the bar: (new scroll position, where in the bar it was grabbed).
/// Pressing above or below the bar moves it there.
pub fn press(area: Rect, max: i32, scroll: i32, y: i32) -> (i32, i32) {
    let th = thumb(area, max, scroll);
    if y >= th.y && y < th.y + th.h {
        (scroll, y - th.y)
    } else {
        let grab = th.h / 2;
        (drag_to(area, max, y, grab), grab)
    }
}

/// The scroll position for the bar dragged to `y` (grabbed `grab` pixels from its top).
pub fn drag_to(area: Rect, max: i32, y: i32, grab: i32) -> i32 {
    let t = track(area);
    let th = thumb(area, max, 0).h;
    let span = (t.h - th).max(1);
    ((y - grab - t.y).clamp(0, span) as i64 * max.max(0) as i64 / span as i64) as i32
}
