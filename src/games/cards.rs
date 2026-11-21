//! Card games: Solitaire (Klondike) and FreeCell. Drag cards, or click one to send it to
//! the best place. Undo as much as you like.

use alloc::format;
use alloc::vec;
use alloc::vec::Vec;

use super::{banner, button, shuffle, GameAction, Sfx, Sound};
use crate::gfx::{rgb, Canvas, Rect, Size};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Klondike,
    FreeCell,
}

/// A card: suit * 13 + rank (rank 0 = ace .. 12 = king); suits: spades, hearts, clubs, diamonds.
type Card = u8;

fn rank(c: Card) -> u8 {
    c % 13
}
fn suit(c: Card) -> u8 {
    c / 13
}
fn red(c: Card) -> bool {
    suit(c) % 2 == 1
}

const RANKS: [&str; 13] = ["A", "2", "3", "4", "5", "6", "7", "8", "9", "10", "J", "Q", "K"];

struct Drag {
    from: usize,
    idx: usize,
    /// Pointer offset from the first dragged card's corner, and the pointer.
    dx: i32,
    dy: i32,
    x: i32,
    y: i32,
    moved: bool,
}

pub struct Cards {
    pub kind: Kind,
    /// (card, face up) per pile.
    piles: Vec<Vec<(Card, bool)>>,
    undo: Vec<Vec<Vec<(Card, bool)>>>,
    drag: Option<Drag>,
    pub draw3: bool,
    won: bool,
    finishing: bool,
    moved_any: bool,
    last_finish: u64,
}

impl Cards {
    pub fn new(kind: Kind) -> Cards {
        let mut c = Cards { kind, piles: Vec::new(), undo: Vec::new(), drag: None, draw3: false, won: false, finishing: false, moved_any: false, last_finish: 0 };
        c.deal();
        c
    }

    // Pile numbering. Klondike: 0 stock, 1 waste, 2-5 foundations, 6-12 tableau.
    // FreeCell: 0-3 cells, 4-7 foundations, 8-15 tableau.
    fn found_range(&self) -> core::ops::Range<usize> {
        match self.kind {
            Kind::Klondike => 2..6,
            Kind::FreeCell => 4..8,
        }
    }
    fn tab_range(&self) -> core::ops::Range<usize> {
        match self.kind {
            Kind::Klondike => 6..13,
            Kind::FreeCell => 8..16,
        }
    }
    fn is_found(&self, p: usize) -> bool {
        self.found_range().contains(&p)
    }
    fn is_tab(&self, p: usize) -> bool {
        self.tab_range().contains(&p)
    }
    fn is_cell(&self, p: usize) -> bool {
        self.kind == Kind::FreeCell && p < 4
    }

    /// A new game. Returns nothing; counts as played once a card has moved.
    pub fn deal(&mut self) {
        let mut deck: Vec<Card> = (0..52).collect();
        shuffle(&mut deck);
        self.won = false;
        self.finishing = false;
        self.moved_any = false;
        self.undo.clear();
        self.drag = None;
        match self.kind {
            Kind::Klondike => {
                self.piles = vec![Vec::new(); 13];
                let mut k = 0;
                for col in 0..7 {
                    for row in 0..=col {
                        self.piles[6 + col].push((deck[k], row == col));
                        k += 1;
                    }
                }
                self.piles[0] = deck[k..].iter().map(|&c| (c, false)).collect();
            }
            Kind::FreeCell => {
                self.piles = vec![Vec::new(); 16];
                for (k, &c) in deck.iter().enumerate() {
                    self.piles[8 + k % 8].push((c, true));
                }
            }
        }
    }

    fn snapshot(&mut self) {
        self.undo.push(self.piles.clone());
        if self.undo.len() > 200 {
            self.undo.remove(0);
        }
    }

    /// Can cards[from][idx..] be picked up?
    fn can_lift(&self, from: usize, idx: usize) -> bool {
        let p = &self.piles[from];
        if idx >= p.len() || !p[idx].1 {
            return false;
        }
        match self.kind {
            Kind::Klondike if from == 0 => false,
            _ if !self.is_tab(from) => idx == p.len() - 1,
            _ => {
                let run = p[idx..].windows(2).all(|w| red(w[0].0) != red(w[1].0) && rank(w[0].0) == rank(w[1].0) + 1);
                run
            }
        }
    }

    fn free_cells(&self) -> usize {
        (0..4).filter(|&i| self.piles[i].is_empty()).count()
    }

    /// Can the cards from[idx..] go onto pile `to`?
    fn can_drop(&self, from: usize, idx: usize, to: usize) -> bool {
        if from == to {
            return false;
        }
        let moving = &self.piles[from][idx..];
        let first = moving[0].0;
        let n = moving.len();
        if self.is_found(to) {
            if n != 1 {
                return false;
            }
            return match self.piles[to].last() {
                None => rank(first) == 0,
                Some(&(top, _)) => suit(top) == suit(first) && rank(first) == rank(top) + 1,
            };
        }
        if self.is_cell(to) {
            return n == 1 && self.piles[to].is_empty();
        }
        if !self.is_tab(to) {
            return false;
        }
        if self.kind == Kind::FreeCell {
            let empty_cols = self.tab_range().filter(|&i| self.piles[i].is_empty() && i != to).count();
            let limit = (self.free_cells() + 1) << empty_cols;
            if n > limit {
                return false;
            }
        }
        match self.piles[to].last() {
            None => self.kind == Kind::FreeCell || rank(first) == 12,
            Some(&(top, up)) => up && red(top) != red(first) && rank(top) == rank(first) + 1,
        }
    }

    fn do_move(&mut self, from: usize, idx: usize, to: usize, sfx: &mut Sfx) {
        self.snapshot();
        let moving: Vec<(Card, bool)> = self.piles[from].drain(idx..).collect();
        self.piles[to].extend(moving);
        if self.is_tab(from) {
            if let Some(top) = self.piles[from].last_mut() {
                top.1 = true;
            }
        }
        self.moved_any = true;
        sfx.play(if self.is_found(to) { Sound::Drop } else { Sound::Click });
    }

    /// Best place for from[idx..]: a foundation, then a column with cards, then an empty
    /// column, then a free cell.
    fn best_target(&self, from: usize, idx: usize) -> Option<usize> {
        let single = idx + 1 == self.piles[from].len();
        if single {
            if let Some(t) = self.found_range().find(|&t| self.can_drop(from, idx, t)) {
                return Some(t);
            }
        }
        let tabs: Vec<usize> = self.tab_range().collect();
        if let Some(&t) = tabs.iter().find(|&&t| !self.piles[t].is_empty() && self.can_drop(from, idx, t)) {
            return Some(t);
        }
        // Don't shuffle a whole column into an empty one.
        if !(self.is_tab(from) && idx == 0) {
            if let Some(&t) = tabs.iter().find(|&&t| self.piles[t].is_empty() && self.can_drop(from, idx, t)) {
                return Some(t);
            }
        }
        if single && self.kind == Kind::FreeCell && !self.is_cell(from) {
            return (0..4).find(|&t| self.piles[t].is_empty());
        }
        None
    }

    fn check_won(&mut self, played: &mut (u32, u32), sfx: &mut Sfx) {
        if !self.won && self.found_range().all(|f| self.piles[f].len() == 13) {
            self.won = true;
            self.finishing = false;
            played.0 += 1;
            sfx.play(Sound::Win);
        }
        // Everything face up and the stock empty: the rest can go up by itself.
        if !self.won && !self.finishing {
            let all_up = self.piles.iter().all(|p| p.iter().all(|c| c.1));
            let stock_done = self.kind == Kind::FreeCell || (self.piles[0].is_empty() && self.piles[1].is_empty());
            let sorted = self.tab_range().all(|t| self.piles[t].windows(2).all(|w| rank(w[0].0) > rank(w[1].0)));
            if all_up && stock_done && (self.kind == Kind::Klondike || sorted) {
                self.finishing = true;
            }
        }
    }

    // ---- layout -----------------------------------------------------------------------------

    fn card_size(&self, r: Rect) -> (i32, i32) {
        let cols = if self.kind == Kind::Klondike { 7 } else { 8 };
        let cw = ((r.w - 24 - (cols - 1) * 12) / cols).min((r.h - 80) * 5 / 7 / 4).clamp(44, 130);
        (cw, cw * 7 / 5)
    }
    fn pile_pos(&self, r: Rect, p: usize) -> (i32, i32) {
        let (cw, ch) = self.card_size(r);
        let cols = if self.kind == Kind::Klondike { 7 } else { 8 };
        let total = cols * cw + (cols - 1) * 12;
        let x0 = r.x + (r.w - total) / 2;
        let col_x = |k: i32| x0 + k * (cw + 12);
        let top = r.y + 56;
        let tab_y = top + ch + 18;
        match self.kind {
            Kind::Klondike => match p {
                0 => (col_x(0), top),
                1 => (col_x(1), top),
                2..=5 => (col_x(p as i32 + 1), top),
                _ => (col_x(p as i32 - 6), tab_y),
            },
            Kind::FreeCell => match p {
                0..=7 => (col_x(p as i32), top),
                _ => (col_x(p as i32 - 8), tab_y),
            },
        }
    }
    /// Vertical position of each card in a column (squeezed to fit).
    fn offsets(&self, r: Rect, p: usize) -> Vec<i32> {
        let (_, ch) = self.card_size(r);
        let cards = &self.piles[p];
        if !self.is_tab(p) {
            return vec![0; cards.len()];
        }
        let (_, y) = self.pile_pos(r, p);
        let room = (r.y + r.h - 8 - y - ch).max(20);
        let (mut down, mut up) = (ch / 7, ch / 4);
        let need = |d: i32, u: i32| cards.iter().take(cards.len().saturating_sub(1)).map(|c| if c.1 { u } else { d }).sum::<i32>();
        while need(down, up) > room && up > 8 {
            up -= 1;
            down = (down - 1).max(4).min(up);
        }
        let mut v = Vec::with_capacity(cards.len());
        let mut off = 0;
        for c in cards {
            v.push(off);
            off += if c.1 { up } else { down };
        }
        v
    }
    /// Waste (draw 3): horizontal offsets of the cards shown.
    fn waste_shift(&self, r: Rect, k: usize) -> i32 {
        let n = self.piles[1].len();
        let (cw, _) = self.card_size(r);
        let shown = if self.draw3 { n.min(3) } else { 1 };
        let first = n - shown;
        if k < first {
            0
        } else {
            (k - first) as i32 * cw / 4
        }
    }
    fn buttons(&self, r: Rect) -> Vec<(&'static str, Rect)> {
        let mut v = vec![("New game", Rect::new(r.x + 12, r.y + 10, 110, 34)), ("Undo", Rect::new(r.x + 130, r.y + 10, 80, 34))];
        if self.kind == Kind::Klondike {
            v.push((if self.draw3 { "Deal 3" } else { "Deal 1" }, Rect::new(r.x + 218, r.y + 10, 90, 34)));
        }
        v
    }

    // ---- drawing ------------------------------------------------------------------------------

    pub fn draw(&self, c: &mut Canvas, r: Rect, stats: (u32, u32)) {
        c.fill_rect(r.x, r.y, r.w, r.h, rgb(30, 110, 70));
        for (label, b) in self.buttons(r) {
            button(c, b, label, false);
        }
        let info = format!("Games won: {} of {}", stats.0, stats.1);
        let iw = crate::gfx::text_width(&info, Size::Small, true);
        c.text(r.x + r.w - iw - 16, r.y + 18, &info, Size::Small, true, rgb(220, 240, 226));
        let (cw, ch) = self.card_size(r);
        for p in 0..self.piles.len() {
            let (x, y) = self.pile_pos(r, p);
            c.round_rect(x, y, cw, ch, 8, rgb(24, 92, 58));
            if self.is_found(p) && self.piles[p].is_empty() {
                c.text_centered(x + cw / 2, y + ch / 2 - 16, "A", Size::Large, true, rgb(50, 130, 90));
            }
            if self.kind == Kind::Klondike && p == 0 && self.piles[0].is_empty() {
                super::ring(c, x + cw / 2, y + ch / 2, cw / 4, 4, rgb(50, 130, 90));
            }
            let offs = self.offsets(r, p);
            for (k, &(card, up)) in self.piles[p].iter().enumerate() {
                if let Some(d) = &self.drag {
                    if d.moved && d.from == p && k >= d.idx {
                        continue;
                    }
                }
                let dx = if self.kind == Kind::Klondike && p == 1 { self.waste_shift(r, k) } else { 0 };
                draw_card(c, x + dx, y + offs[k], cw, ch, card, up);
            }
        }
        if let Some(d) = &self.drag {
            if d.moved {
                let offs = self.offsets(r, d.from);
                let step = if offs.len() > d.idx + 1 { offs[d.idx + 1] - offs[d.idx] } else { ch / 4 };
                for (k, &(card, up)) in self.piles[d.from][d.idx..].iter().enumerate() {
                    let (x, y) = (d.x - d.dx, d.y - d.dy + k as i32 * step.max(ch / 5));
                    c.round_rect_alpha(x + 4, y + 6, cw, ch, 8, 0, 60);
                    draw_card(c, x, y, cw, ch, card, up);
                }
            }
        }
        if self.won {
            banner(c, r, "You won!", "Press New game to play again");
        }
    }

    // ---- input ----------------------------------------------------------------------------------

    /// The pile and card under (x, y).
    fn hit(&self, r: Rect, x: i32, y: i32) -> Option<(usize, Option<usize>)> {
        let (cw, ch) = self.card_size(r);
        for p in 0..self.piles.len() {
            let (px, py) = self.pile_pos(r, p);
            let offs = self.offsets(r, p);
            for k in (0..self.piles[p].len()).rev() {
                let dx = if self.kind == Kind::Klondike && p == 1 { self.waste_shift(r, k) } else { 0 };
                if Rect::new(px + dx, py + offs[k], cw, ch).contains(x, y) {
                    return Some((p, Some(k)));
                }
            }
            if Rect::new(px, py, cw, ch).contains(x, y) {
                return Some((p, None));
            }
        }
        None
    }

    pub fn on_press(&mut self, x: i32, y: i32, r: Rect, stats: &mut (u32, u32), sfx: &mut Sfx) -> GameAction {
        for (label, b) in self.buttons(r) {
            if b.contains(x, y) {
                match label {
                    "New game" => self.deal(),
                    "Undo" => {
                        if let Some(p) = self.undo.pop() {
                            self.piles = p;
                            self.won = false;
                            self.finishing = false;
                        }
                    }
                    _ => self.draw3 = !self.draw3,
                }
                return GameAction::Redraw;
            }
        }
        if self.won {
            return GameAction::None;
        }
        let Some((p, k)) = self.hit(r, x, y) else { return GameAction::None };
        // Klondike stock: deal to the waste (or turn the waste over).
        if self.kind == Kind::Klondike && p == 0 {
            self.snapshot();
            if self.piles[0].is_empty() {
                let w: Vec<(Card, bool)> = self.piles[1].drain(..).rev().map(|(c, _)| (c, false)).collect();
                self.piles[0] = w;
            } else {
                for _ in 0..if self.draw3 { 3 } else { 1 } {
                    if let Some((c, _)) = self.piles[0].pop() {
                        self.piles[1].push((c, true));
                    }
                }
            }
            if !self.moved_any {
                self.moved_any = true;
                stats.1 += 1;
            }
            sfx.play(Sound::Click);
            return GameAction::Redraw;
        }
        let Some(k) = k else { return GameAction::None };
        // Face-down top card of a column: turn it over.
        if self.is_tab(p) && k + 1 == self.piles[p].len() && !self.piles[p][k].1 {
            self.snapshot();
            self.piles[p][k].1 = true;
            return GameAction::Redraw;
        }
        if !self.can_lift(p, k) {
            return GameAction::None;
        }
        let (px, py) = self.pile_pos(r, p);
        let offs = self.offsets(r, p);
        let dx = if self.kind == Kind::Klondike && p == 1 { self.waste_shift(r, k) } else { 0 };
        self.drag = Some(Drag { from: p, idx: k, dx: x - (px + dx), dy: y - (py + offs[k]), x, y, moved: false });
        GameAction::Redraw
    }

    pub fn on_drag(&mut self, x: i32, y: i32) -> bool {
        let Some(d) = self.drag.as_mut() else { return false };
        if !d.moved && (x - d.x).abs() + (y - d.y).abs() < 6 {
            return false;
        }
        d.moved = true;
        d.x = x;
        d.y = y;
        true
    }

    pub fn on_release(&mut self, r: Rect, stats: &mut (u32, u32), sfx: &mut Sfx) -> bool {
        let Some(d) = self.drag.take() else { return false };
        let target = if d.moved {
            // The pile the dragged card's middle is over.
            let (cw, ch) = self.card_size(r);
            let (mx, my) = (d.x - d.dx + cw / 2, d.y - d.dy + ch / 3);
            let mut best = None;
            for p in 0..self.piles.len() {
                let (px, py) = self.pile_pos(r, p);
                let bottom = py + self.offsets(r, p).last().copied().unwrap_or(0) + ch;
                if mx >= px - 10 && mx < px + cw + 10 && my >= py - 20 && my < bottom + 20 && self.can_drop(d.from, d.idx, p) {
                    best = Some(p);
                }
            }
            best
        } else {
            self.best_target(d.from, d.idx)
        };
        if let Some(t) = target {
            if !self.moved_any {
                stats.1 += 1;
            }
            self.do_move(d.from, d.idx, t, sfx);
            self.check_won(stats, sfx);
        }
        true
    }

    /// Sends cards up one at a time when the game is as good as won.
    pub fn tick(&mut self, now: u64, stats: &mut (u32, u32), sfx: &mut Sfx) -> bool {
        if !self.finishing || now < self.last_finish + 6 {
            return false;
        }
        self.last_finish = now;
        let sources: Vec<usize> = (0..self.piles.len()).filter(|&p| !self.is_found(p) && !(self.kind == Kind::Klondike && p == 0)).collect();
        // The lowest card that can go up.
        let mut best: Option<(u8, usize, usize)> = None;
        for p in sources {
            if let Some(&(c, _)) = self.piles[p].last() {
                let idx = self.piles[p].len() - 1;
                if let Some(t) = self.found_range().find(|&t| self.can_drop(p, idx, t)) {
                    if best.is_none_or(|b| rank(c) < b.0) {
                        best = Some((rank(c), p, t));
                    }
                }
            }
        }
        match best {
            Some((_, p, t)) => {
                let idx = self.piles[p].len() - 1;
                self.do_move(p, idx, t, sfx);
                self.check_won(stats, sfx);
            }
            None => self.finishing = false,
        }
        true
    }
}

fn suit_shape(c: &mut Canvas, cx: i32, cy: i32, s: i32, which: u8, color: u32) {
    match which {
        1 => {
            // Heart
            c.circle(cx - s / 4, cy - s / 8, s / 4 + 1, color);
            c.circle(cx + s / 4, cy - s / 8, s / 4 + 1, color);
            c.triangle([(cx - s / 2, cy - s / 12), (cx + s / 2, cy - s / 12), (cx, cy + s / 2)], color);
        }
        3 => {
            // Diamond
            c.triangle([(cx, cy - s / 2), (cx - s * 2 / 5, cy), (cx + s * 2 / 5, cy)], color);
            c.triangle([(cx, cy + s / 2), (cx - s * 2 / 5, cy), (cx + s * 2 / 5, cy)], color);
        }
        0 => {
            // Spade
            c.circle(cx - s / 4, cy + s / 10, s / 4 + 1, color);
            c.circle(cx + s / 4, cy + s / 10, s / 4 + 1, color);
            c.triangle([(cx - s / 2, cy + s / 20), (cx + s / 2, cy + s / 20), (cx, cy - s / 2)], color);
            c.triangle([(cx, cy + s / 8), (cx - s / 5, cy + s / 2), (cx + s / 5, cy + s / 2)], color);
        }
        _ => {
            // Club
            let rr = s * 2 / 9 + 1;
            c.circle(cx, cy - s / 4, rr, color);
            c.circle(cx - s / 4, cy + s / 12, rr, color);
            c.circle(cx + s / 4, cy + s / 12, rr, color);
            c.triangle([(cx, cy), (cx - s / 5, cy + s / 2), (cx + s / 5, cy + s / 2)], color);
        }
    }
}

fn draw_card(c: &mut Canvas, x: i32, y: i32, w: i32, h: i32, card: Card, up: bool) {
    c.round_rect(x, y, w, h, 8, rgb(70, 70, 70));
    if !up {
        c.round_rect(x + 1, y + 1, w - 2, h - 2, 7, rgb(255, 255, 255));
        c.round_rect(x + 4, y + 4, w - 8, h - 8, 5, rgb(40, 80, 170));
        // A simple lattice pattern.
        let step = 10;
        let mut yy = y + 8;
        while yy < y + h - 10 {
            let mut xx = x + 8 + ((yy - y) / step % 2) * step / 2;
            while xx < x + w - 10 {
                c.circle(xx + 2, yy + 2, 2, rgb(80, 120, 210));
                xx += step;
            }
            yy += step;
        }
        return;
    }
    c.round_rect(x + 1, y + 1, w - 2, h - 2, 7, rgb(255, 255, 255));
    let col = if red(card) { rgb(204, 34, 34) } else { rgb(24, 24, 28) };
    let label = RANKS[rank(card) as usize];
    let big = w >= 70;
    let (size, th) = if big { (Size::Large, 32) } else { (Size::Medium, 20) };
    let lw = c.text(x + 5, y + 3, label, size, true, col);
    // The suit right beside the rank, so it shows even when cards overlap.
    let ss = if big { 18 } else { 13 };
    suit_shape(c, x + 5 + lw + 3 + ss / 2, y + 3 + th / 2, ss, suit(card), col);
    // Middle: a big suit (or the letter for J, Q, K).
    let (cx, cy) = (x + w / 2 + if big { 8 } else { 6 }, y + h / 2 + 6);
    if rank(card) >= 10 {
        c.round_rect(x + w / 4, y + h / 4, w / 2 + 4, h / 2, 6, if red(card) { rgb(252, 232, 232) } else { rgb(232, 236, 246) });
        c.text_centered(cx, cy - th / 2 - 10, label, size, true, col);
        suit_shape(c, cx, cy + th / 2 + 4, ss, suit(card), col);
    } else {
        suit_shape(c, cx, cy, (w / 2).min(h / 3), suit(card), col);
    }
}
